//! Value provenance (schema 44): who set a project parameter value and whether a person
//! checked it. One row per value, keyed like a parameter lock: (table of the row, row id,
//! parameter name); a type parameter sits on the type row.
//! - No row: entered in the editor, or set before step 44 (no badge, nothing to verify).
//! - Every write of a value through `params::write_value` drops its row (the inverse puts it
//!   back), so an edit after the AI never keeps the AI's badge; the AI Apply records new rows
//!   after its writes, inside the same undo step.
//! - Times are unix seconds passed in by the caller: tests pass fixed ones, saves stay stable.

use rusqlite::{params, types::Value, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

pub const TABLE: &str = "value_src";
const DDL: &str = "CREATE TABLE IF NOT EXISTS value_src(tbl TEXT NOT NULL, id INTEGER NOT NULL, \
     name TEXT NOT NULL, kind TEXT NOT NULL, at INTEGER NOT NULL, session TEXT, request TEXT, \
     prompt TEXT, verified_at INTEGER, PRIMARY KEY(tbl, id, name))";
const COLS: &str = "rowid, tbl, id, name, kind, at, session, request, prompt, verified_at";
const PUT: &str = "INSERT OR REPLACE INTO value_src(rowid, tbl, id, name, kind, at, session, \
     request, prompt, verified_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)";
const DROP: &str = "DELETE FROM value_src WHERE tbl=?1 AND id=?2 AND name=?3";
/// Rows whose value is still there: a deleted value or parameter leaves no badge.
const LIVE: &str = "EXISTS(SELECT 1 FROM pparam p JOIN pparam_val pv ON pv.param=p.id \
     WHERE p.name=v.name AND pv.kind=v.tbl AND pv.ref=v.id AND pv.value IS NOT NULL \
     AND pv.value<>'')";

/// Longest prompt summary kept, in characters.
pub const PROMPT_MAX: usize = 200;

/// (table of the row, row id, parameter name), as `LockKey`.
pub type CellKey = (String, i64, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrcKind {
    User,
    Ai,
    Xlsx,
    Ifc,
    Generator,
    RuleFix,
}

impl SrcKind {
    pub const ALL: [SrcKind; 6] = [
        SrcKind::User,
        SrcKind::Ai,
        SrcKind::Xlsx,
        SrcKind::Ifc,
        SrcKind::Generator,
        SrcKind::RuleFix,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SrcKind::User => "user",
            SrcKind::Ai => "ai",
            SrcKind::Xlsx => "xlsx",
            SrcKind::Ifc => "ifc",
            SrcKind::Generator => "generator",
            SrcKind::RuleFix => "rule_fix",
        }
    }

    /// A kind this build does not know (a newer file) reads as `None`, never fails.
    pub fn parse(s: &str) -> Option<SrcKind> {
        // "excel": the stored name of files written before the rename
        let s = if s == "excel" { "xlsx" } else { s };
        SrcKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Set by the assistant, directly or through a generator or a check fix it proposed.
    pub fn by_ai(self) -> bool {
        matches!(self, SrcKind::Ai | SrcKind::Generator | SrcKind::RuleFix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueSrc {
    pub kind: SrcKind,
    /// When the value was set, unix seconds.
    pub at: i64,
    pub session: Option<String>,
    pub request: Option<String>,
    /// What the person asked, cut to `PROMPT_MAX` characters on one line.
    pub prompt: Option<String>,
    /// When a person marked it verified, unix seconds; `None` = unverified.
    pub verified_at: Option<i64>,
}

impl ValueSrc {
    pub fn new(kind: SrcKind, at: i64) -> ValueSrc {
        ValueSrc {
            kind,
            at,
            session: None,
            request: None,
            prompt: None,
            verified_at: None,
        }
    }

    /// An assistant value: always starts unverified.
    pub fn ai(kind: SrcKind, at: i64, session: &str, request: &str, prompt: &str) -> ValueSrc {
        let opt = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        ValueSrc {
            session: opt(session),
            request: opt(request),
            prompt: opt(&prompt_summary(prompt)),
            ..ValueSrc::new(kind, at)
        }
    }

    pub fn verified(&self) -> bool {
        self.verified_at.is_some()
    }

    /// Needs a person's look: set by the assistant and not verified yet.
    pub fn needs_review(&self) -> bool {
        self.kind.by_ai() && !self.verified()
    }
}

/// One line, at most `PROMPT_MAX` characters, an ellipsis when cut.
pub fn prompt_summary(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= PROMPT_MAX {
        return flat;
    }
    let mut out: String = flat.chars().take(PROMPT_MAX - 1).collect();
    out.push('\u{2026}');
    out
}

/// Wall clock, unix seconds: what the app passes as `at`. Tests pass fixed numbers instead.
pub fn now_s() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Step 46: additive; opening a file that has the table writes nothing.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

fn has_table(db: &Connection) -> Result<bool, Error> {
    let n: i64 = db
        .prepare_cached("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1")?
        .query_row([TABLE], |r| r.get(0))?;
    Ok(n > 0)
}

fn read_row(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Option<ValueSrc>> {
    let kind: String = r.get(at)?;
    let Some(kind) = SrcKind::parse(&kind) else {
        return Ok(None);
    };
    Ok(Some(ValueSrc {
        kind,
        at: r.get(at + 1)?,
        session: r.get(at + 2)?,
        request: r.get(at + 3)?,
        prompt: r.get(at + 4)?,
        verified_at: r.get(at + 5)?,
    }))
}

/// Rowid of the record of a cell; a kind this build cannot read is still restored as it was.
fn old_row(db: &Connection, tbl: &str, id: i64, name: &str) -> Result<Option<i64>, Error> {
    Ok(db
        .prepare_cached("SELECT rowid FROM value_src WHERE tbl=?1 AND id=?2 AND name=?3")?
        .query_row(params![tbl, id, name], |r| r.get::<_, i64>(0))
        .optional()?)
}

/// The inverse statement that puts row `rowid` back exactly, rowid included.
fn restore(db: &Connection, inv: Inv, rowid: i64) -> Result<Inv, Error> {
    let vals: Vec<Value> = db
        .prepare_cached(&format!("SELECT {COLS} FROM value_src WHERE rowid=?1"))?
        .query_row([rowid], |r| {
            (0..10)
                .map(|i| r.get::<_, Value>(i))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })?;
    Ok(inv.sql(PUT, vals))
}

fn cell_args(tbl: &str, id: i64, name: &str) -> Vec<Value> {
    vec![
        Value::Text(tbl.to_string()),
        Value::Integer(id),
        Value::Text(name.to_string()),
    ]
}

/// Called by the value writer: the value changes, so who set the old one no longer holds.
pub(crate) fn forget(
    db: &Connection,
    tbl: &str,
    id: i64,
    name: &str,
    inv: Inv,
) -> Result<Inv, Error> {
    if !has_table(db)? {
        return Ok(inv);
    }
    let Some(rowid) = old_row(db, tbl, id, name.trim())? else {
        return Ok(inv);
    };
    let inv = restore(db, inv, rowid)?;
    db.prepare_cached(DROP)?.execute(params![tbl, id, name.trim()])?;
    Ok(inv)
}

fn put(db: &Connection, cell: (&str, i64, &str), s: &ValueSrc, mut inv: Inv) -> Result<Inv, Error> {
    let (tbl, id, name) = cell;
    match old_row(db, tbl, id, name)? {
        Some(rowid) => inv = restore(db, inv, rowid)?,
        None => inv = inv.sql(DROP, cell_args(tbl, id, name)),
    }
    db.prepare_cached(
        "INSERT OR REPLACE INTO value_src(tbl, id, name, kind, at, session, request, prompt, \
         verified_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?
    .execute(params![
        tbl,
        id,
        name,
        s.kind.as_str(),
        s.at,
        s.session,
        s.request,
        s.prompt,
        s.verified_at
    ])?;
    Ok(inv)
}

/// Unique cells with a non-empty name, in the order given.
fn clean<'a>(cells: &'a [(&'a str, i64, &'a str)]) -> Vec<(&'a str, i64, &'a str)> {
    let mut seen = std::collections::HashSet::new();
    cells
        .iter()
        .map(|(t, id, n)| (*t, *id, n.trim()))
        .filter(|(_, _, n)| !n.is_empty())
        .filter(|c| seen.insert(*c))
        .collect()
}

/// Rows of `v` limited to values that still exist.
fn select_live(extra: &str) -> String {
    format!(
        "SELECT tbl, id, name, kind, at, session, request, prompt, verified_at FROM value_src v \
         WHERE {LIVE}{extra} ORDER BY tbl, id, name"
    )
}

fn query_srcs(
    db: &Connection,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<(CellKey, ValueSrc)>, Error> {
    if !has_table(db)? {
        return Ok(vec![]);
    }
    let mut st = db.prepare(sql)?;
    let rows = st.query_map(args, |r| {
        let k: CellKey = (r.get(0)?, r.get(1)?, r.get(2)?);
        Ok((k, read_row(r, 3)?))
    })?;
    let mut out = vec![];
    for row in rows {
        let (k, s) = row?;
        if let Some(s) = s {
            out.push((k, s));
        }
    }
    Ok(out)
}

/// `n` of `total` elements of a category lack a value: the ids, in id order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    /// Table of the elements (`opening` for doors and windows).
    pub table: &'static str,
    pub total: usize,
    pub ids: Vec<i64>,
}

impl Model {
    /// Who set the value of parameter `name` on row (`tbl`, `id`); `None`: no record.
    pub fn value_src(&self, tbl: &str, id: i64, name: &str) -> Result<Option<ValueSrc>, Error> {
        let v = query_srcs(
            &self.db,
            &select_live(" AND v.tbl=?1 AND v.id=?2 AND v.name=?3"),
            &[&tbl, &id, &name.trim()],
        )?;
        Ok(v.into_iter().next().map(|x| x.1))
    }

    /// Stored values of the cells as text (`None`: no value), to tell which ones a step changed.
    pub fn cell_values(&self, cells: &[(&str, i64, &str)]) -> Result<Vec<Option<String>>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT CAST(pv.value AS TEXT) FROM pparam p JOIN pparam_val pv ON pv.param=p.id \
             WHERE p.name=?3 AND pv.kind=?1 AND pv.ref=?2",
        )?;
        let mut out = Vec::with_capacity(cells.len());
        for (t, id, n) in cells {
            let v: Option<Option<String>> =
                st.query_row(params![t, id, n.trim()], |r| r.get(0)).optional()?;
            out.push(v.flatten());
        }
        Ok(out)
    }

    /// Every record whose value still exists, ordered by table, id and parameter.
    pub fn value_srcs(&self) -> Result<Vec<(CellKey, ValueSrc)>, Error> {
        query_srcs(&self.db, &select_live(""), &[])
    }

    /// Records of the rows `rows` (table, id), for a selection.
    pub fn value_srcs_of(
        &self,
        rows: &[(String, i64)],
    ) -> Result<Vec<(CellKey, ValueSrc)>, Error> {
        let want: std::collections::HashSet<(&str, i64)> =
            rows.iter().map(|(t, i)| (t.as_str(), *i)).collect();
        Ok(self
            .value_srcs()?
            .into_iter()
            .filter(|(k, _)| want.contains(&(k.0.as_str(), k.1)))
            .collect())
    }

    /// Values the assistant set that nobody verified yet.
    pub fn unverified_ai(&self) -> Result<Vec<(CellKey, ValueSrc)>, Error> {
        Ok(self
            .value_srcs()?
            .into_iter()
            .filter(|(_, s)| s.needs_review())
            .collect())
    }

    /// Records `src` on each cell (table, id, parameter); one undo step, or part of the
    /// enclosing `atomic` step. Returns how many cells were recorded.
    pub fn record_value_src(
        &mut self,
        cells: &[(&str, i64, &str)],
        src: &ValueSrc,
    ) -> Result<usize, Error> {
        let cells = clean(cells);
        if cells.is_empty() {
            return Ok(0);
        }
        let src = ValueSrc {
            prompt: src.prompt.as_deref().map(prompt_summary),
            ..src.clone()
        };
        self.op("value_src", |db| {
            migrate(db)?;
            let mut inv = Inv::default();
            for (t, id, n) in &cells {
                inv = put(db, (t, *id, n), &src, Inv::default())?.then(inv);
            }
            Ok((cells.len(), 0, inv))
        })
    }

    /// Marks the unverified records among `cells` verified at `at`: one undo step. Cells
    /// without a record or already verified are left alone; none to mark writes nothing.
    pub fn verify_values(&mut self, cells: &[(&str, i64, &str)], at: i64) -> Result<usize, Error> {
        let cells = clean(cells);
        let mut todo = vec![];
        for (t, id, n) in &cells {
            if self.value_src(t, *id, n)?.is_some_and(|s| !s.verified()) {
                todo.push((*t, *id, *n));
            }
        }
        if todo.is_empty() {
            return Ok(0);
        }
        self.op("value_verify", |db| {
            let mut inv = Inv::default();
            for (t, id, n) in &todo {
                if let Some(rowid) = old_row(db, t, *id, n)? {
                    inv = restore(db, Inv::default(), rowid)?.then(inv);
                    db.prepare_cached("UPDATE value_src SET verified_at=?2 WHERE rowid=?1")?
                        .execute(params![rowid, at])?;
                }
            }
            Ok((todo.len(), 0, inv))
        })
    }

    /// Elements of category `cat` (a parameter category: `door`, `wall`, `slab`, ...) with no
    /// value of parameter `param`, against all elements of the category.
    pub fn find_missing(&self, cat: &str, param: &str) -> Result<Missing, Error> {
        let d = self
            .param_by_name(param)?
            .ok_or_else(|| Error::Invalid(format!("no such parameter '{}'", param.trim())))?;
        let cat = cat.trim();
        if !d.spec.categories.iter().any(|c| c == cat) {
            return Err(Error::Invalid(format!(
                "parameter '{}' is not bound to {cat}; it is bound to {}",
                d.spec.name,
                d.spec.categories.join(", ")
            )));
        }
        let (table, filter, _) = crate::checks::category_rows(cat)
            .ok_or_else(|| Error::Invalid(format!("unknown category '{cat}'")))?;
        let (kind, reference) = if d.spec.per_type {
            let t = crate::PARAM_TYPE_OF
                .iter()
                .find(|t| t.0 == cat)
                .ok_or_else(|| Error::Invalid(format!("{cat} has no types")))?;
            (t.1, format!("t.{}", t.2))
        } else {
            (table, "t.id".to_string())
        };
        let total: i64 = self.db.query_row(
            &format!("SELECT count(*) FROM {table} t WHERE 1=1{filter}"),
            [],
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT t.id FROM {table} t LEFT JOIN pparam_val v ON v.param=?1 AND v.kind=?2 \
             AND v.ref={reference} WHERE (v.value IS NULL OR v.value=''){filter} ORDER BY t.id"
        );
        let mut st = self.db.prepare(&sql)?;
        let ids = st
            .query_map(params![d.id, kind], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        Ok(Missing {
            table,
            total: total.max(0) as usize,
            ids,
        })
    }
}

#[cfg(test)]
#[path = "value_src_tests.rs"]
mod tests;
