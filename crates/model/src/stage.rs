// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Staged Add: a worker builds the import in a copy of the model file; the model thread
//! attaches that copy and applies it inside one journal operation (one undo). The copy starts
//! as this model, so ids are already in this model's id space.
//!
//! What travels: every row the copy added (rowid above the mark taken at the copy) and every
//! row that existed at the copy and the import inserted, changed or removed. The second set is
//! recorded in the copy by temporary triggers ([`Model::stage_watch`]), including rows an
//! `OR REPLACE` removes through a unique key (no delete trigger fires for those). After the
//! Add the carried tables hold exactly what the copy holds, so the Add is complete or refused.
//! Applicable only while this model is unchanged since the job started.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use web_time::{Duration, SystemTime};

use rusqlite::{config::DbConfig, types::Value, Connection};

use crate::stage_err::StageErr;
use crate::{Error, Model};

/// Tables that never travel: the journal (the Add writes its own record) and the analysis
/// runs, stored outside the journal while the import is built.
const SKIP: [&str; 3] = ["op", "calc_run", "calc_blob"];
/// Change record the worker leaves in the copy: (table, rowid) of rows that existed at the copy
/// and the import touched. WITHOUT ROWID, so it is never taken for a model table.
const TOUCHED: &str = "_stage_touched";

/// Highest rowid of every plain table at the moment of the copy, and the user keys of `meta`
/// with their values then (view state left out).
#[derive(Clone, Debug, PartialEq)]
pub struct StageMarks {
    rows: Vec<(String, i64)>,
    meta: Vec<(String, Value)>,
}

impl StageMarks {
    fn get(&self, t: &str) -> Option<i64> {
        self.rows.iter().find(|x| x.0 == t).map(|x| x.1)
    }
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Inverse step of an Add on a table it created; args: table, rowid ranges `lo-hi,..`, a
/// 16-hex-digit fingerprint per row in rowid order, then the change head before the Add and its
/// session (missing in logs written before them) ([`drop_added`]).
pub(crate) const DROP_ADDED: &str = "@drop_added_rows";

/// FNV-1a over a row's values; numbers that are whole compare as integers, so a row put back
/// by a snapshot round trip still matches.
fn row_sum(row: &rusqlite::Row<'_>, n: usize) -> rusqlite::Result<u64> {
    use rusqlite::types::ValueRef;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= u64::from(x);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for i in 0..n {
        match row.get_ref(i)? {
            ValueRef::Null => eat(&[0]),
            ValueRef::Integer(v) => {
                eat(&[1]);
                eat(&v.to_le_bytes());
            }
            ValueRef::Real(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => {
                eat(&[1]);
                eat(&(f as i64).to_le_bytes());
            }
            ValueRef::Real(f) => {
                eat(&[2]);
                eat(&f.to_bits().to_le_bytes());
            }
            ValueRef::Text(t) => {
                eat(&[3]);
                eat(&(t.len() as u64).to_le_bytes());
                eat(t);
            }
            ValueRef::Blob(b) => {
                eat(&[4]);
                eat(&(b.len() as u64).to_le_bytes());
                eat(b);
            }
        }
    }
    Ok(h)
}

/// (rowid, fingerprint) of every row of `main.t` with rowid in `lo..=hi`, in rowid order.
fn row_sums(db: &Connection, t: &str, lo: i64, hi: i64) -> Result<Vec<(i64, u64)>, Error> {
    let mut st = db.prepare(&format!(
        "SELECT rowid, * FROM main.{} WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid",
        q(t)
    ))?;
    let n = st.column_count();
    let rows: Vec<(i64, u64)> = st
        .query_map([lo, hi], |r| Ok((r.get::<_, i64>(0)?, row_sum(r, n)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Args of [`DROP_ADDED`] for the rows `main.t` holds now (all written by the Add); `at`: the
/// change head before the Add and its session.
fn added_rows(db: &Connection, t: &str, at: (u64, i64)) -> Result<Vec<Value>, Error> {
    let rows = row_sums(db, t, i64::MIN, i64::MAX)?;
    let mut ranges: Vec<(i64, i64)> = vec![];
    let mut sums = String::with_capacity(rows.len() * 16);
    for &(r, s) in &rows {
        match ranges.last_mut() {
            Some(x) if x.1.checked_add(1) == Some(r) => x.1 = r,
            _ => ranges.push((r, r)),
        }
        sums.push_str(&format!("{s:016x}"));
    }
    let ranges: Vec<String> = ranges.iter().map(|(a, b)| format!("{a}:{b}")).collect();
    Ok(vec![
        Value::Text(t.to_string()),
        Value::Text(ranges.join(",")),
        Value::Text(sums),
        Value::Integer(i64::try_from(at.0).unwrap_or(-1)),
        Value::Integer(at.1),
    ])
}

/// Undo of an Add on a table it created: deletes each recorded rowid only while the row there
/// is still the one the Add wrote and, when `tracker` holds the Add's session, nothing outside
/// an undo step wrote it after the Add. A row later put at a freed rowid stays, even one equal
/// in every column, and keeps the table. Without that record only the fingerprint decides.
pub(crate) fn drop_added(
    db: &Connection,
    args: &[Value],
    tracker: Option<&std::sync::Mutex<crate::changes::Tracker>>,
) -> Result<(), Error> {
    use crate::changes::Loose;
    let bad = || Error::Invalid("bad op log".into());
    let (t, ranges, sums, at) = match args {
        [Value::Text(t), Value::Text(r), Value::Text(s)] => (t, r, s, None),
        [Value::Text(t), Value::Text(r), Value::Text(s), Value::Integer(h), Value::Integer(id)] => {
            (t, r, s, u64::try_from(*h).ok().map(|h| (h, *id)))
        }
        _ => return Err(bad()),
    };
    let there: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
        [t],
        |r| r.get(0),
    )?;
    if !there {
        return Ok(());
    }
    // read before any delete: the tracker hook takes the same lock
    let later = match (tracker, at) {
        (Some(tr), Some((h, id))) => crate::changes::loose_since(tr, id, h, t),
        _ => Loose::Unknown,
    };
    let written = |r: i64| match &later {
        Loose::Unknown => false,
        Loose::Whole => true,
        Loose::Rows(w) => w.contains(&r),
    };
    let sum_at = |i: usize| -> Result<u64, Error> {
        let s = sums.get(i * 16..i * 16 + 16).ok_or_else(bad)?;
        u64::from_str_radix(s, 16).map_err(|_| bad())
    };
    let mut del = db.prepare(&format!("DELETE FROM main.{} WHERE rowid=?1", q(t)))?;
    let mut base = 0usize;
    for part in ranges.split(',').filter(|p| !p.is_empty()) {
        let (lo, hi) = part.split_once(':').ok_or_else(bad)?;
        let (lo, hi): (i64, i64) = (
            lo.parse().map_err(|_| bad())?,
            hi.parse().map_err(|_| bad())?,
        );
        if hi < lo {
            return Err(bad());
        }
        for (r, s) in row_sums(db, t, lo, hi)? {
            let i = base + usize::try_from(r - lo).map_err(|_| bad())?;
            if sum_at(i)? == s && !written(r) {
                del.execute([r])?;
            }
        }
        base += usize::try_from(hi - lo).map_err(|_| bad())? + 1;
    }
    if base * 16 != sums.len() {
        return Err(bad());
    }
    Ok(())
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Plain rowid tables of `schema` in creation order (parents first).
fn plain_tables(db: &Connection, schema: &str) -> Result<Vec<String>, Error> {
    let mut st = db.prepare(&format!(
        "SELECT name FROM {schema}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' \
         AND sql NOT LIKE '%WITHOUT ROWID%' AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY rowid"
    ))?;
    let v = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn marks_of(db: &Connection, schema: &str) -> Result<StageMarks, Error> {
    let mut v = vec![];
    for t in plain_tables(db, schema)? {
        let m: i64 = db.query_row(
            &format!("SELECT COALESCE(MAX(rowid),0) FROM {schema}.{}", q(&t)),
            [],
            |r| r.get(0),
        )?;
        v.push((t, m));
    }
    Ok(StageMarks {
        rows: v,
        meta: crate::changes::user_meta(db, schema)?,
    })
}

/// Writes a consistent copy of the model file `model_path` to `out` (worker thread, own
/// connection) and returns what the copy held.
pub fn stage_copy(model_path: &str, out: &Path) -> Result<StageMarks, Error> {
    let db = Connection::open(model_path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    crate::file::write_copy(&db, out)?;
    drop(db);
    let c = Connection::open(out)?;
    marks_of(&c, "main")
}

/// Columns of `t` in `schema`: (name, declared type, pk position).
fn columns(db: &Connection, schema: &str, t: &str) -> Result<Vec<(String, String, i64)>, Error> {
    let mut st = db.prepare(&format!("PRAGMA {schema}.table_info({})", q(t)))?;
    let v = st
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

/// Column list that copies a row of `t` whole: its columns, plus `rowid` when the rowid is not
/// the table's own integer key. Refused when the two sides differ in columns: a row copied
/// through the shared ones would lose the rest.
fn copy_list(db: &Connection, t: &str, fresh: bool) -> Result<String, Error> {
    let theirs = columns(db, "stg", t)?;
    let mine = if fresh {
        theirs.clone()
    } else {
        columns(db, "main", t)?
    };
    let names = |v: &[(String, String, i64)]| {
        let mut n: Vec<String> = v.iter().map(|c| c.0.clone()).collect();
        n.sort();
        n
    };
    if mine.is_empty() || names(&mine) != names(&theirs) {
        return Err(StageErr::TableDiffers(t.to_string()).into());
    }
    let alias = mine.iter().filter(|c| c.2 > 0).count() == 1
        && mine
            .iter()
            .any(|c| c.2 > 0 && c.1.eq_ignore_ascii_case("INTEGER"));
    let mut list: Vec<String> = mine.iter().map(|c| q(&c.0)).collect();
    if !alias {
        list.insert(0, "rowid".into());
    }
    Ok(list.join(","))
}

/// `(a=new.a AND b=new.b) OR ...` over the unique keys of `t`: the rows an insert or update
/// with OR REPLACE would remove. None without unique keys (the rowid alone is covered by the
/// insert and update triggers).
fn unique_match(db: &Connection, t: &str) -> Result<Option<String>, Error> {
    let mut st = db.prepare("SELECT name FROM pragma_index_list(?1) WHERE \"unique\"=1")?;
    let idx: Vec<String> = st
        .query_map([t], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut ors = vec![];
    for name in idx {
        let mut st = db.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let cols: Vec<Option<String>> = st
            .query_map([&name], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if cols.is_empty() || cols.iter().any(Option::is_none) {
            return Err(StageErr::UniqueKey(t.to_string()).into());
        }
        let and: Vec<String> = cols
            .iter()
            .flatten()
            .map(|c| format!("{0}=new.{0}", q(c)))
            .collect();
        ors.push(format!("({})", and.join(" AND ")));
    }
    Ok((!ors.is_empty()).then(|| ors.join(" OR ")))
}

/// True while something that stays in the model still points at level `id`.
fn level_used(db: &Connection, id: i64) -> bool {
    crate::integrity::all_refs()
        .into_iter()
        .filter(|r| r.2 == "level")
        .any(|(child, col, _)| {
            db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {child} WHERE {col}=?1)"),
                [id],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        })
}

/// The connection in defensive mode while a staged copy is attached: no writable schema, and
/// functions named in the copy's schema run only when harmless. Restored on drop.
// holds the saved flags, not the connection, so the caller keeps `&mut self` while it is active
#[must_use]
struct Untrusted([(DbConfig, bool); 3]);

impl Untrusted {
    fn new(db: &Connection) -> Result<Self, Error> {
        let want = [
            (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
            (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
            (DbConfig::SQLITE_DBCONFIG_WRITABLE_SCHEMA, false),
        ];
        let mut was = want;
        for (k, (c, v)) in want.into_iter().enumerate() {
            was[k].1 = db.db_config(c)?;
            db.set_db_config(c, v)?;
        }
        Ok(Untrusted(was))
    }

    fn restore(self, db: &Connection) {
        for (c, v) in self.0 {
            let _ = db.set_db_config(c, v);
        }
    }
}

/// Triggers of the connection off until [`Quiet::end`] or drop (also on an error path).
struct Quiet<'c>(&'c Connection, bool);

impl<'c> Quiet<'c> {
    fn new(db: &'c Connection) -> Result<Self, Error> {
        let was = db.db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
        Ok(Quiet(db, was))
    }

    fn end(self) -> Result<(), Error> {
        self.0
            .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, self.1)?;
        Ok(())
    }
}

impl Drop for Quiet<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, self.1);
    }
}

/// One table to carry: its rows above `mark`, the touched rows at or below it.
struct Carry {
    t: String,
    mark: i64,
    news: bool,
    touched: Vec<i64>,
    list: String,
    /// Schema of a table the copy created (made here in the same operation).
    ddl: Vec<String>,
}

impl Model {
    /// What [`StageMarks`] would say about this model now.
    pub fn stage_marks(&self) -> Result<StageMarks, Error> {
        marks_of(&self.db, "main")
    }

    /// On the copy, before the import: records every row at or below `base` that the import
    /// inserts, changes or removes (temporary triggers, this connection only).
    pub fn stage_watch(&self, base: &StageMarks) -> Result<(), Error> {
        self.db.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS stage_touched(t TEXT NOT NULL, r INTEGER NOT NULL, \
             PRIMARY KEY(t, r)) WITHOUT ROWID;",
        )?;
        for (i, (t, m)) in base.rows.iter().enumerate() {
            if *m <= 0 || SKIP.contains(&t.as_str()) {
                continue;
            }
            let (tq, tl) = (q(t), lit(t));
            // trigger bodies take unqualified names only; a TEMP trigger looks in temp first
            let put = "INSERT OR IGNORE INTO stage_touched";
            let mut sql = format!(
                "CREATE TEMP TRIGGER stage_w{i}_ai AFTER INSERT ON main.{tq} WHEN new.rowid<={m} \
                 BEGIN {put} VALUES({tl}, new.rowid); END;\n\
                 CREATE TEMP TRIGGER stage_w{i}_au AFTER UPDATE ON main.{tq} \
                 WHEN old.rowid<={m} OR new.rowid<={m} BEGIN \
                 {put} SELECT {tl}, old.rowid WHERE old.rowid<={m}; \
                 {put} SELECT {tl}, new.rowid WHERE new.rowid<={m}; END;\n\
                 CREATE TEMP TRIGGER stage_w{i}_ad AFTER DELETE ON main.{tq} WHEN old.rowid<={m} \
                 BEGIN {put} VALUES({tl}, old.rowid); END;\n"
            );
            if let Some(keys) = unique_match(&self.db, t)? {
                // OR REPLACE removes the row holding the key without a delete trigger
                for (n, ev) in [("bi", "INSERT"), ("bu", "UPDATE")] {
                    sql.push_str(&format!(
                        "CREATE TEMP TRIGGER stage_w{i}_{n} BEFORE {ev} ON main.{tq} BEGIN \
                         {put} SELECT {tl}, rowid FROM {tq} WHERE rowid<={m} AND ({keys}); \
                         END;\n"
                    ));
                }
            }
            self.db.execute_batch(&sql)?;
        }
        Ok(())
    }

    /// On the copy, after the import: leaves the change record in the file for [`Self::stage_add`].
    pub fn stage_seal(&self) -> Result<(), Error> {
        self.db.execute_batch(&format!(
            "CREATE TABLE main.{TOUCHED}(t TEXT NOT NULL, r INTEGER NOT NULL, \
             PRIMARY KEY(t, r)) WITHOUT ROWID;\n\
             INSERT INTO main.{TOUCHED} SELECT t, r FROM temp.stage_touched;"
        ))?;
        Ok(())
    }

    /// Applies the import done in the copy `staged`: one operation, one undo. `drop_level` (an
    /// untouched default level) is removed in the same operation when the copy brought levels
    /// of its own and nothing stands on it. Refused, model unchanged, when this model changed
    /// after `since` (its [`Model::change_head`] when the job started) or after the copy.
    pub fn stage_add(
        &mut self,
        staged: &Path,
        base: &StageMarks,
        since: u64,
        drop_level: Option<i64>,
    ) -> Result<(), Error> {
        // an earlier Add whose DETACH failed after its commit: released here first
        if stg_attached(&self.db) && detach(&self.db).is_err() {
            return Err(StageErr::PrevCopyAttached.into());
        }
        // meta is left out: view-state writes move its rowids; the user's keys are checked below
        let rows = |m: &StageMarks| -> Vec<(String, i64)> {
            m.rows.iter().filter(|x| x.0 != "meta").cloned().collect()
        };
        if rows(&self.stage_marks()?) != rows(base)
            || !self.changes_since(since).is_empty()
            || self.meta_user_written_since(since, &base.meta)
        {
            return Err(StageErr::ModelChanged.into());
        }
        let p = staged
            .to_str()
            .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
        let guard = Untrusted::new(&self.db)?;
        if let Err(e) = self.db.execute("ATTACH DATABASE ?1 AS stg", [p]) {
            guard.restore(&self.db);
            return Err(damaged_copy(e.into()));
        }
        let r = self.stage_insert(base, drop_level).map_err(damaged_copy);
        // once committed the Add stands with its undo step: a failed DETACH is no failed Add,
        // it is retried by the next Add ([`Self::stage_note`] tells meanwhile)
        let _ = detach(&self.db);
        guard.restore(&self.db);
        r
    }

    /// Note after an Add whose copy could not be released (DETACH failed after the commit):
    /// the Add is done; the next Add releases the copy first. None when nothing is held.
    pub fn stage_note(&self) -> Option<StageErr> {
        stg_attached(&self.db).then_some(StageErr::CopyHeld)
    }

    /// Releases the copy an Add left attached. An Add run inside an open transaction (one
    /// editor undo step) cannot DETACH there, and the next one could not either: the caller
    /// calls this once the step is committed. Ok when nothing is held.
    pub fn stage_release(&self) -> Result<(), Error> {
        if stg_attached(&self.db) {
            detach(&self.db)?;
        }
        Ok(())
    }

    fn stage_plan(&self, base: &StageMarks) -> Result<Vec<Carry>, Error> {
        let db = &self.db;
        let recorded: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM stg.sqlite_master WHERE name=?1)",
            [TOUCHED],
            |r| r.get(0),
        )?;
        if !recorded {
            return Err(StageErr::NoChangeRecord.into());
        }
        let mut touched: HashMap<String, Vec<i64>> = HashMap::new();
        {
            let mut st = db.prepare(&format!("SELECT t, r FROM stg.{TOUCHED} ORDER BY t, r"))?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for x in rows {
                let (t, r) = x?;
                touched.entry(t).or_default().push(r);
            }
        }
        let mine = plain_tables(db, "main")?;
        let mut plan = vec![];
        for t in plain_tables(db, "stg")? {
            if SKIP.contains(&t.as_str()) {
                continue;
            }
            let fresh = !mine.contains(&t);
            let mark = if fresh { 0 } else { base.get(&t).unwrap_or(0) };
            let news: bool = db.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM stg.{} WHERE rowid>?1)", q(&t)),
                [mark],
                |r| r.get(0),
            )?;
            let rows = touched.remove(&t).unwrap_or_default();
            if !news && rows.is_empty() {
                continue;
            }
            let mut ddl: Vec<String> = vec![];
            if fresh {
                // the copy's schema text is the file's choice: only one plain CREATE of this
                // table or its index runs; its triggers stay behind (modules re-create theirs)
                let mut st = db.prepare(
                    "SELECT sql FROM stg.sqlite_master WHERE tbl_name=?1 AND sql IS NOT NULL \
                     AND type IN ('table', 'index') ORDER BY type='index', rowid",
                )?;
                ddl = st
                    .query_map([&t], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                if let Some(bad) = ddl.iter().find(|s| !crate::sql_guard::is_plain_ddl(s, &t)) {
                    let head: String = bad.chars().take(40).collect();
                    return Err(Error::Corrupt(format!(
                        "staged copy holds a schema statement that is not a plain CREATE: {head}"
                    )));
                }
            }
            let list = copy_list(db, &t, fresh)?;
            plan.push(Carry {
                t,
                mark,
                news,
                touched: rows,
                list,
                ddl,
            });
        }
        if let Some(t) = touched.keys().next() {
            return Err(StageErr::TableGone(t.to_string()).into());
        }
        Ok(plan)
    }

    fn stage_insert(&mut self, base: &StageMarks, drop_level: Option<i64>) -> Result<(), Error> {
        let plan = self.stage_plan(base)?;
        let brought_levels = base.get("ext_ref").is_some_and(|m| {
            self.db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM stg.ext_ref WHERE kind='level' AND rowid>?1)",
                    [m],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false)
        });
        // the Add's own commit is an undo step: rows written after this head outside one are
        // someone else's ([`drop_added`])
        let at = (self.change_head(), self.change_session());
        self.batch_in("wall_stage_add", &[], |db, snaps| {
            for c in &plan {
                for s in &c.ddl {
                    db.execute(s, [])?;
                }
            }
            // pre-images of the touched rows first; undo puts them back with OR REPLACE (no
            // delete trigger fires, so rows hanging on them stay) and drops only rows the Add
            // put at a rowid that was free
            for c in &plan {
                let tq = q(&c.t);
                for &r in &c.touched {
                    let had: bool = db.query_row(
                        &format!("SELECT EXISTS(SELECT 1 FROM main.{tq} WHERE rowid=?1)"),
                        [r],
                        |x| x.get(0),
                    )?;
                    if had {
                        snaps.save(db, &tq, "rowid", r)?;
                    } else {
                        snaps.push_front(
                            &format!("DELETE FROM {tq} WHERE rowid=?1"),
                            vec![Value::Integer(r)],
                        );
                    }
                }
            }
            // a created table loses only the Add's rows on undo (bounded after the insert
            // below): rows written into it later by paths with no undo step stay
            for c in plan.iter().filter(|c| c.ddl.is_empty()) {
                snaps.watermark(db, &q(&c.t))?;
            }
            // tables the Add created go on undo after their rows (last inverses, children
            // first), and only when nothing else is left in them
            for c in plan.iter().rev().filter(|c| !c.ddl.is_empty()) {
                snaps.push(crate::ops::Inv::default().sql(
                    crate::ops::DROP_IF_EMPTY,
                    vec![Value::Text(c.t.clone())],
                ));
            }
            // the copy already holds what triggers did during the import; firing them again
            // here would drop rows that hang on a re-inserted row (clash items, phases)
            let quiet = Quiet::new(db)?;
            // old versions of every touched row go before any copy comes in: no transient
            // unique-key clash, and the carried tables end up exactly as in the copy
            let pick = format!("SELECT r FROM stg.{TOUCHED} WHERE t=?1");
            for c in plan.iter().filter(|c| !c.touched.is_empty()) {
                db.execute(
                    &format!("DELETE FROM main.{} WHERE rowid IN ({pick})", q(&c.t)),
                    [&c.t],
                )?;
            }
            for c in plan.iter().filter(|c| !c.touched.is_empty()) {
                let (tq, l) = (q(&c.t), &c.list);
                db.execute(
                    &format!(
                        "INSERT INTO main.{tq}({l}) SELECT {l} FROM stg.{tq} \
                         WHERE rowid IN ({pick}) ORDER BY rowid"
                    ),
                    [&c.t],
                )?;
            }
            // plain INSERT: a clash fails the whole Add instead of dropping rows
            for c in plan.iter().filter(|c| c.news) {
                let tq = q(&c.t);
                // new meta keys take fresh rowids: view-state writes may hold the copy's ones
                let l = match c.t.as_str() {
                    "meta" => c.list.strip_prefix("rowid,").unwrap_or(&c.list),
                    _ => c.list.as_str(),
                };
                db.execute(
                    &format!(
                        "INSERT INTO main.{tq}({l}) SELECT {l} FROM stg.{tq} \
                         WHERE rowid>?1 ORDER BY rowid"
                    ),
                    [c.mark],
                )?;
            }
            quiet.end()?;
            // a created table loses exactly the rows the Add wrote: a row put later at a rowid
            // freed by a path with no undo step differs from the Add's and stays
            for c in plan.iter().filter(|c| !c.ddl.is_empty()) {
                snaps.push_front(DROP_ADDED, added_rows(db, &c.t, at)?);
            }
            if let Some(old) = drop_level.filter(|_| brought_levels) {
                if !level_used(db, old) {
                    // a level the import touched already has its pre-image saved above
                    let saved = plan
                        .iter()
                        .any(|c| c.t == "level" && c.touched.contains(&old));
                    if !saved {
                        snaps.save(db, "level", "id", old)?;
                    }
                    db.execute("DELETE FROM level WHERE id=?1", [old])?;
                }
            }
            Ok(())
        })
    }
}

/// SQLite refuses a copy whose stored schema does not parse ("malformed database schema") as
/// corrupt: that is a damaged copy, the same refusal as a schema statement our guard rejects.
fn damaged_copy(e: Error) -> Error {
    match e {
        Error::Db(rusqlite::Error::SqliteFailure(f, m))
            if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) =>
        {
            Error::Corrupt(format!("staged copy: {}", m.unwrap_or_else(|| f.to_string())))
        }
        e => e,
    }
}

fn stg_attached(db: &Connection) -> bool {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_database_list WHERE name='stg')",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

fn detach(db: &Connection) -> Result<(), Error> {
    if detach_blocked() {
        return Err(Error::Invalid("DETACH failed".into()));
    }
    db.execute("DETACH DATABASE stg", [])?;
    Ok(())
}

#[cfg(not(test))]
fn detach_blocked() -> bool {
    false
}

#[cfg(test)]
thread_local! {
    static FAIL_DETACH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn detach_blocked() -> bool {
    FAIL_DETACH.with(|f| f.get())
}

/// Where the copy of `model_path` goes: next to the model file.
pub fn stage_path(model_path: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("add-{}-{n}.stage", std::process::id());
    match Path::new(model_path).parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => d.join(name),
        None => std::env::temp_dir().join(name),
    }
}

fn session_start() -> SystemTime {
    static T: std::sync::OnceLock<SystemTime> = std::sync::OnceLock::new();
    *T.get_or_init(SystemTime::now)
}

/// `add-<pid>-<n>.stage`, exactly the name [`stage_path`] makes.
fn our_stage_pid(name: &str) -> Option<u32> {
    let mid = name.strip_prefix("add-")?.strip_suffix(".stage")?;
    let (pid, n) = mid.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pid) && digits(n)).then(|| pid.parse().ok())?
}

/// A file an Add leaves: the copy itself (`add-<pid>-<n>.stage`, sidecar false) or what lives
/// beside it (`-wal`, `-shm`, `-journal`, the copy's `.tmp-<pid>-<seq>-<ns>` while it is
/// written). Returns (pid, copy name, sidecar).
fn our_stage_file(name: &str) -> Option<(u32, &str, bool)> {
    let end = name.find(".stage")? + ".stage".len();
    let (base, rest) = name.split_at(end);
    let pid = our_stage_pid(base)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let side = match rest {
        "" => false,
        "-wal" | "-shm" | "-journal" => true,
        r => {
            let parts: Vec<&str> = r.strip_prefix(".tmp-")?.split('-').collect();
            if parts.len() != 3 || !parts.iter().all(|x| digits(*x)) {
                return None;
            }
            true
        }
    };
    Some((pid, base, side))
}

/// Copies left by an Add that never finished (crash, kill) sit next to the model. On open:
/// one from an earlier session, older than an hour, with a database header and not held open
/// is removed with its sidecars; any other copy of our name from an earlier session is moved
/// to `_archive`. A sidecar whose copy is gone (orphan `-wal`/`-shm`/`-journal`, an unfinished
/// `.tmp`) from an earlier session and older than an hour is removed; a younger one stays.
/// Returns how many files were handled. Never fails the open.
pub fn clean_stale_stages(model_path: &str) -> usize {
    match Path::new(model_path).parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(d) => clean_stale_in(d, session_start(), SystemTime::now(), std::process::id()),
        None => 0,
    }
}

fn clean_stale_in(dir: &Path, started: SystemTime, now: SystemTime, own_pid: u32) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut done = 0;
    // copies first: a removed copy takes its sidecars, the rest are judged as orphans after
    for pass_side in [false, true] {
        for name in &names {
            let Some((pid, base, side)) = our_stage_file(name) else {
                continue;
            };
            if side != pass_side || pid == own_pid {
                continue;
            }
            let path = dir.join(name);
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(mtime) = meta.modified() else { continue };
            if mtime >= started {
                continue;
            }
            let old = now.duration_since(mtime).is_ok_and(|a| a > Duration::from_secs(3600));
            if side {
                if old && !dir.join(base).exists() && std::fs::remove_file(&path).is_ok() {
                    done += 1;
                }
            } else {
                done += clean_stale_copy(dir, name, &path, old);
            }
        }
    }
    done
}

/// One stale copy: removed with its sidecars, or moved to `_archive` with them (a copy's
/// `-wal` may hold its pages). 1 when the copy was handled.
fn clean_stale_copy(dir: &Path, name: &str, path: &Path, old: bool) -> usize {
    const SIDES: [&str; 3] = ["-wal", "-shm", "-journal"];
    if old && has_db_header(path) {
        // a copy held open by another process refuses to go
        if std::fs::remove_file(path).is_err() {
            return 0;
        }
        for s in SIDES {
            let _ = std::fs::remove_file(dir.join(format!("{name}{s}")));
        }
        return 1;
    }
    let arch = dir.join("_archive");
    if std::fs::create_dir_all(&arch).is_err() {
        return 0;
    }
    let mut to = arch.join(name);
    let mut k = 1;
    while to.exists() {
        to = arch.join(format!("{name}.{k}"));
        k += 1;
    }
    if std::fs::rename(path, &to).is_err() {
        return 0;
    }
    let to = to.to_string_lossy().into_owned();
    for s in SIDES {
        let side = dir.join(format!("{name}{s}"));
        if side.exists() {
            let _ = std::fs::rename(&side, format!("{to}{s}"));
        }
    }
    1
}

fn has_db_header(p: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open(p)
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok()
        && &b == b"SQLite format 3\0"
}

#[cfg(test)]
mod clean_tests {
    use super::*;

    fn aged(p: &Path, ago: Duration) {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - ago).unwrap();
    }

    #[test]
    fn stale_stage_cleaned_on_open() {
        let d = std::env::temp_dir().join(format!("ddc-ad3-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let day = Duration::from_secs(86400);
        let mut hdr = b"SQLite format 3\0".to_vec();
        hdr.extend_from_slice(&[0; 64]);
        let ours_old = d.join("add-111-0.stage");
        let odd_old = d.join("add-111-1.stage");
        let live = d.join(format!("add-{}-2.stage", std::process::id()));
        let fresh = d.join("add-222-3.stage");
        let user = d.join("add-notes.stage");
        let model = d.join("work.ddc");
        for (p, body) in [
            (&ours_old, &hdr[..]),
            (&odd_old, &b"not a database at all"[..]),
            (&live, &hdr[..]),
            (&fresh, &hdr[..]),
            (&user, &hdr[..]),
            (&model, &hdr[..]),
        ] {
            std::fs::write(p, body).unwrap();
        }
        for p in [&ours_old, &odd_old, &live, &user, &model] {
            aged(p, day);
        }
        let started = SystemTime::now() - Duration::from_secs(60);
        let n = clean_stale_in(&d, started, SystemTime::now(), std::process::id());
        assert_eq!(n, 2);
        assert!(!ours_old.exists(), "certainly ours: removed");
        assert!(!odd_old.exists() && d.join("_archive").join("add-111-1.stage").exists());
        assert!(live.exists() && fresh.exists(), "this session and newer files stay");
        assert!(user.exists() && model.exists(), "other names are never touched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn clean_stale_removes_wal_shm_tmp() {
        let d = std::env::temp_dir().join(format!("ddc-ad4-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let day = Duration::from_secs(86400);
        let mut hdr = b"SQLite format 3\0".to_vec();
        hdr.extend_from_slice(&[0; 64]);
        let own = std::process::id();
        let put = |n: &str, body: &[u8], ago: Option<Duration>| {
            let p = d.join(n);
            std::fs::write(&p, body).unwrap();
            if let Some(a) = ago {
                aged(&p, a);
            }
            p
        };
        // an old copy goes with its sidecars
        let copy = put("add-111-0.stage", &hdr, Some(day));
        let copy_wal = put("add-111-0.stage-wal", b"w", Some(day));
        let copy_shm = put("add-111-0.stage-shm", b"s", Some(day));
        // orphans of an earlier session, older than an hour: removed
        let orphan_wal = put("add-111-5.stage-wal", b"w", Some(day));
        let orphan_shm = put("add-111-5.stage-shm", b"s", Some(day));
        let orphan_tmp = put("add-111-6.stage.tmp-111-0-123456789", &hdr, Some(day));
        // stay: this session's files, a sidecar of a copy still there, a young orphan, odd names
        let fresh_wal = put("add-222-7.stage-wal", b"w", None);
        let live = put("add-111-8.stage", &hdr, None);
        let live_wal = put("add-111-8.stage-wal", b"w", Some(day));
        let own_wal = put(&format!("add-{own}-9.stage-wal"), b"w", Some(day));
        let young = put("add-111-10.stage-shm", b"s", Some(Duration::from_secs(600)));
        let odd_tmp = put("add-111-11.stage.tmp-x", b"t", Some(day));
        let other = put("work.ddc-wal", b"w", Some(day));
        let started = SystemTime::now() - Duration::from_secs(60);
        let n = clean_stale_in(&d, started, SystemTime::now(), own);
        assert_eq!(n, 4, "the copy and three orphans");
        for p in [&copy, &copy_wal, &copy_shm, &orphan_wal, &orphan_shm, &orphan_tmp] {
            assert!(!p.exists(), "{} removed", p.display());
        }
        for p in [&fresh_wal, &live, &live_wal, &own_wal, &young, &odd_tmp, &other] {
            assert!(p.exists(), "{} stays", p.display());
        }
        assert!(!d.join("_archive").exists(), "nothing archived");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod add_tests {
    use super::*;

    fn model(tag: &str) -> (Model, String) {
        let p = std::env::temp_dir().join(format!("ddc-ad4-{}-{tag}.ddc", std::process::id()));
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{s}", p.display()));
        }
        let path = p.to_string_lossy().into_owned();
        (Model::open(&path).unwrap(), path)
    }

    /// A staged import of `work` done in a copy of the model file at `path`.
    fn staged(path: &str, work: impl FnOnce(&mut Model)) -> (PathBuf, StageMarks) {
        let out = stage_path(path);
        let marks = stage_copy(path, &out).unwrap();
        let mut c = Model::open_copy(out.to_str().unwrap()).unwrap();
        c.stage_watch(&marks).unwrap();
        work(&mut c);
        c.stage_seal().unwrap();
        drop(c);
        (out, marks)
    }

    fn ops(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    fn has_level(m: &Model, name: &str) -> bool {
        m.sql()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM level WHERE name=?1)",
                [name],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn detach_fail_after_commit_is_ok_with_note() {
        let (mut m, path) = model("detach");
        let since = m.change_head();
        let (out, marks) = staged(&path, |c| {
            c.add_level("Brought", 3.0).unwrap();
        });
        let n = ops(&m);
        FAIL_DETACH.with(|f| f.set(true));
        let r = m.stage_add(&out, &marks, since, None);
        assert!(r.is_ok(), "an applied Add is never reported failed: {r:?}");
        assert!(has_level(&m, "Brought"));
        assert_eq!(ops(&m), n + 1, "one undo step");
        assert!(m.stage_note().is_some(), "the held copy is noted");
        // the next Add retries the DETACH first; still failing, it is refused unchanged
        let since = m.change_head();
        let (out2, marks2) = staged(&path, |c| {
            c.add_level("Second", 6.0).unwrap();
        });
        assert!(m.stage_add(&out2, &marks2, since, None).is_err());
        assert!(!has_level(&m, "Second"));
        assert_eq!(ops(&m), n + 1);
        // once DETACH works the next Add releases the old copy and goes through
        FAIL_DETACH.with(|f| f.set(false));
        m.stage_add(&out2, &marks2, since, None).unwrap();
        assert!(has_level(&m, "Second"));
        assert_eq!(ops(&m), n + 2);
        assert!(m.stage_note().is_none(), "nothing held after a clean Add");
        // each Add is one undo step
        m.undo().unwrap();
        assert!(!has_level(&m, "Second") && has_level(&m, "Brought"));
        m.undo().unwrap();
        assert!(!has_level(&m, "Brought"));
        drop(m);
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(out2);
    }

    /// Adds inside an outer transaction (the editor's undo step): DETACH fails there, the
    /// release after the commit frees the copy, so the next Add is not refused.
    #[test]
    fn add_inside_transaction_then_release() {
        let (mut m, path) = model("in-tx");
        let mut outs = vec![];
        for (k, name) in ["InTx1", "InTx2"].into_iter().enumerate() {
            let since = m.change_head();
            let (out, marks) = staged(&path, |c| {
                c.add_level(name, 3.0 * (k as f64 + 1.0)).unwrap();
            });
            m.sql().execute_batch("BEGIN IMMEDIATE").unwrap();
            let r = m.stage_add(&out, &marks, since, None);
            m.sql().execute_batch("COMMIT").unwrap();
            r.unwrap();
            m.stage_release().unwrap();
            assert!(m.stage_note().is_none(), "nothing held after the release");
            assert!(has_level(&m, name));
            outs.push(out);
        }
        drop(m);
        for o in outs {
            let _ = std::fs::remove_file(o);
        }
    }

    #[test]
    fn undo_add_drops_created_tables() {
        let (mut m, path) = model("drop");
        let since = m.change_head();
        let (out, marks) = staged(&path, |c| {
            c.sql()
                .execute_batch(
                    "CREATE TABLE ad4_extra(id INTEGER PRIMARY KEY, v TEXT NOT NULL);\n\
                     CREATE INDEX ad4_extra_v ON ad4_extra(v);\n\
                     CREATE TABLE ad4_child(id INTEGER PRIMARY KEY, \
                     p INTEGER NOT NULL REFERENCES ad4_extra(id));\n\
                     INSERT INTO ad4_extra(v) VALUES ('a'), ('b');\n\
                     INSERT INTO ad4_child(p) VALUES (1);",
                )
                .unwrap();
        });
        let objects = |m: &Model| -> i64 {
            m.sql()
                .query_row(
                    "SELECT count(*) FROM sqlite_master \
                     WHERE tbl_name IN ('ad4_extra', 'ad4_child')",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let n = ops(&m);
        m.stage_add(&out, &marks, since, None).unwrap();
        assert_eq!(ops(&m), n + 1);
        assert_eq!(objects(&m), 3, "two tables and the index came in");
        let rows: i64 = m
            .sql()
            .query_row("SELECT count(*) FROM ad4_extra", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        m.undo().unwrap();
        assert_eq!(objects(&m), 0, "undo leaves no table the Add created");
        assert_eq!(ops(&m), n);
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    /// A staged copy whose stored schema text carries a second statement (`; ATTACH ...`) must
    /// not run it: the Add is refused, no file is written, the model is unchanged.
    #[test]
    fn sec_crafted_stage_schema_writes_no_file() {
        let (mut m, path) = model("crafted");
        let since = m.change_head();
        let (out, marks) = staged(&path, |c| {
            c.sql()
                .execute_batch(
                    "CREATE TABLE ad6_extra(id INTEGER PRIMARY KEY, v TEXT);\n\
                     CREATE INDEX ad6_extra_v ON ad6_extra(v);\n\
                     INSERT INTO ad6_extra(v) VALUES ('a');",
                )
                .unwrap();
        });
        let target = std::env::temp_dir().join(format!("ddc-sec-stage-{}.bat", std::process::id()));
        let _ = std::fs::remove_file(&target);
        let lit = target.to_string_lossy().replace('\'', "''");
        let attach = format!("; AtTaCh DATABASE '{lit}' AS z; CREATE TABLE z.t(a)");
        // a tail that would also run inside the Add's transaction, where ATTACH cannot
        let plant = "; CREATE TABLE ad6_planted(a)".to_string();
        for (kind, name, tail) in [
            ("table", "ad6_extra", &attach),
            ("index", "ad6_extra_v", &attach),
            ("table", "ad6_extra", &plant),
            ("index", "ad6_extra_v", &plant),
        ] {
            let raw = Connection::open(&out).unwrap();
            raw.execute_batch("PRAGMA writable_schema=ON").unwrap();
            raw.execute(
                "UPDATE sqlite_master SET sql = sql || ?1 WHERE type=?2 AND name=?3",
                rusqlite::params![tail, kind, name],
            )
            .unwrap();
            drop(raw);
            let n = ops(&m);
            let r = m.stage_add(&out, &marks, since, None);
            assert!(matches!(r, Err(Error::Corrupt(_))), "{kind}: {r:?}");
            assert!(!target.exists(), "{kind}: the Add wrote {}", target.display());
            assert_eq!(ops(&m), n, "{kind}: model unchanged");
            assert!(!table_there(&m, "ad6_extra"));
            assert!(!table_there(&m, "ad6_planted"), "{kind}: the tail ran");
            assert!(m.stage_note().is_none(), "{kind}: the copy is released");
            // the plain schema text back for the next round
            let raw = Connection::open(&out).unwrap();
            raw.execute_batch("PRAGMA writable_schema=ON").unwrap();
            raw.execute(
                "UPDATE sqlite_master SET sql = substr(sql, 1, instr(sql, ';') - 1) \
                 WHERE type=?1 AND name=?2",
                [kind, name],
            )
            .unwrap();
        }
        // the restored copy is a plain one again and goes through
        m.stage_add(&out, &marks, since, None).unwrap();
        assert!(table_there(&m, "ad6_extra"));
        assert!(!target.exists());
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    fn count(m: &Model, sql: &str) -> i64 {
        m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn table_there(m: &Model, t: &str) -> bool {
        count(
            m,
            &format!("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='{t}'"),
        ) == 1
    }

    /// An Add that creates `ad5_extra` (unique `v`) with rows 'a', 'b', and `ad5_gone`.
    fn add_created(m: &mut Model, path: &str) -> PathBuf {
        let since = m.change_head();
        let (out, marks) = staged(path, |c| {
            c.sql()
                .execute_batch(
                    "CREATE TABLE ad5_extra(id INTEGER PRIMARY KEY, v TEXT NOT NULL UNIQUE);
                     CREATE TABLE ad5_gone(id INTEGER PRIMARY KEY, w TEXT);
                     INSERT INTO ad5_extra(v) VALUES ('a'), ('b');
                     INSERT INTO ad5_gone(w) VALUES ('x');",
                )
                .unwrap();
        });
        m.stage_add(&out, &marks, since, None).unwrap();
        out
    }

    /// A write after the Add that records no undo step (import history, family params).
    fn write_later(m: &Model, v: &str) {
        m.sql()
            .execute("INSERT INTO ad5_extra(v) VALUES (?1)", [v])
            .unwrap();
    }

    #[test]
    fn undo_add_keeps_table_with_later_rows() {
        let (mut m, path) = model("ad5keep");
        let n = ops(&m);
        let out = add_created(&mut m, &path);
        write_later(&m, "later");
        m.undo().unwrap();
        assert_eq!(ops(&m), n, "the Add's undo step is gone");
        assert!(table_there(&m, "ad5_extra"), "a table with later rows stays");
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra"),
            1,
            "only the Add's own rows went"
        );
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra WHERE v='later'"),
            1,
            "the later row is kept as written"
        );
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn undo_add_drops_untouched_created_table() {
        let (mut m, path) = model("ad5gone");
        let out = add_created(&mut m, &path);
        write_later(&m, "later");
        m.undo().unwrap();
        assert!(!table_there(&m, "ad5_gone"), "a table nobody wrote to after the Add goes");
        assert!(table_there(&m, "ad5_extra"));
        // nothing written after the Add: every created table goes, as before
        let (mut m2, path2) = model("ad5gone2");
        let out2 = add_created(&mut m2, &path2);
        m2.undo().unwrap();
        assert!(!table_there(&m2, "ad5_gone"));
        assert!(!table_there(&m2, "ad5_extra"));
        assert!(m2.undo_kept().is_empty(), "nothing kept, nothing to say");
        drop((m, m2));
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(out2);
    }

    #[test]
    fn redo_after_kept_table_no_clash() {
        let (mut m, path) = model("ad5redo");
        let out = add_created(&mut m, &path);
        write_later(&m, "later");
        m.undo().unwrap();
        let _ = m.undo_kept();
        // redo of an Add is a new import of the same file into the kept table
        let n = ops(&m);
        let since = m.change_head();
        let (out2, marks) = staged(&path, |c| {
            c.sql()
                .execute_batch("INSERT INTO ad5_extra(v) VALUES ('a'), ('b');")
                .unwrap();
        });
        m.stage_add(&out2, &marks, since, None)
            .expect("the same rows go in again: the undo left no copy of them behind");
        assert_eq!(ops(&m), n + 1);
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 3);
        m.undo().unwrap();
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 1);
        assert!(table_there(&m, "ad5_extra"), "the table was there before this Add");
        assert!(m.undo_kept().is_empty(), "this Add created no table");
        drop(m);
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(out2);
    }

    #[test]
    fn undo_add_note_names_table() {
        let (mut m, path) = model("ad5note");
        let out = add_created(&mut m, &path);
        write_later(&m, "later");
        write_later(&m, "later2");
        m.undo().unwrap();
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 2)]);
        assert!(m.undo_kept().is_empty(), "the note is given once");
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn undo_add_keeps_row_that_reused_freed_rowid() {
        let (mut m, path) = model("ad6reuse");
        let out = add_created(&mut m, &path);
        let top = count(&m, "SELECT max(rowid) FROM ad5_extra");
        // the Add's top row goes by a path with no undo step; the next insert takes its rowid
        m.sql()
            .execute("DELETE FROM ad5_extra WHERE v='b'", [])
            .unwrap();
        write_later(&m, "later");
        assert_eq!(
            count(&m, "SELECT rowid FROM ad5_extra WHERE v='later'"),
            top,
            "the later row reuses the freed rowid"
        );
        m.undo().unwrap();
        assert!(table_there(&m, "ad5_extra"), "the later row keeps the table");
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra WHERE v='later'"),
            1,
            "a row written after the Add survives its undo"
        );
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 1, "'a' went");
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 1)]);
        assert!(!table_there(&m, "ad5_gone"));
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn undo_add_still_deletes_its_own_rows() {
        let (mut m, path) = model("ad6own");
        let since = m.change_head();
        // rowids with gaps (several ranges) and every value kind
        let (out, marks) = staged(&path, |c| {
            c.sql()
                .execute_batch(
                    "CREATE TABLE ad6_rows(id INTEGER PRIMARY KEY, v TEXT, x REAL, b BLOB);
                     INSERT INTO ad6_rows VALUES (1, 'a', 2.0, x'00ff'), (2, NULL, 0.5, NULL),
                       (5, 'c', -1.25, x''), (9, '', 1e300, x'01');",
                )
                .unwrap();
        });
        m.stage_add(&out, &marks, since, None).unwrap();
        assert_eq!(count(&m, "SELECT count(*) FROM ad6_rows"), 4);
        // one Add row gone with no undo step, one later row above the Add's rows
        m.sql()
            .execute_batch(
                "DELETE FROM ad6_rows WHERE id=5;
                 INSERT INTO ad6_rows VALUES (12, 'later', 1.0, NULL);",
            )
            .unwrap();
        m.undo().unwrap();
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad6_rows WHERE id IN (1,2,5,9)"),
            0,
            "every row the Add wrote went"
        );
        assert_eq!(count(&m, "SELECT count(*) FROM ad6_rows WHERE id=12"), 1);
        assert_eq!(m.undo_kept(), vec![("ad6_rows".to_string(), 1)]);
        // nothing written after an Add: its table goes whole, nothing to say
        let (mut m2, path2) = model("ad6own2");
        let out2 = add_created(&mut m2, &path2);
        m2.undo().unwrap();
        assert!(!table_there(&m2, "ad5_extra") && !table_there(&m2, "ad5_gone"));
        assert!(m2.undo_kept().is_empty());
        drop((m, m2));
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(out2);
    }

    #[test]
    fn ai_revert_of_staged_add_refuses_and_keeps_later_rows() {
        use crate::ai_revert::{Refusal, RevertError};
        let (mut m, path) = model("ad8ai");
        let out = add_created(&mut m, &path);
        let seq = m.op_top().unwrap();
        m.audit_append(&crate::ai_audit::AuditEntry {
            session: "s".into(),
            actor: "chat".into(),
            event: "apply".into(),
            model: Some("m".into()),
            text: Some("Add".into()),
            op_seq: Some(seq),
            status: "ok".into(),
            ..Default::default()
        })
        .unwrap();
        write_later(&m, "later");
        let r = m.revert_ai(&[seq]);
        assert!(matches!(r, Err(RevertError::Refused(Refusal::Opaque { .. }))));
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra WHERE v='later'"), 1);
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 3);
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    fn id_of(m: &Model, v: &str) -> i64 {
        count(m, &format!("SELECT id FROM ad5_extra WHERE v='{v}'"))
    }

    #[test]
    fn undo_add_keeps_identical_row_written_later() {
        let (mut m, path) = model("ad7same");
        let out = add_created(&mut m, &path);
        let top = id_of(&m, "b");
        // the Add's 'b' goes by a path with no undo step; an equal row comes back at its rowid
        m.sql()
            .execute("DELETE FROM ad5_extra WHERE v='b'", [])
            .unwrap();
        write_later(&m, "b");
        assert_eq!(id_of(&m, "b"), top, "same rowid, same values: the Add's fingerprint");
        m.undo().unwrap();
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra WHERE v='b'"),
            1,
            "a row written after the Add survives its undo, even one equal to the Add's"
        );
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 1, "'a' went");
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 1)]);
        assert!(!table_there(&m, "ad5_gone"));
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn undo_add_drops_own_rows_unchanged() {
        let (mut m, path) = model("ad7own");
        let out = add_created(&mut m, &path);
        let a = id_of(&m, "a");
        // a step with an undo writes the Add's row and is undone first: the row is the Add's
        // again and goes with it
        m.op("ad7_edit", |db| {
            let inv = crate::ops::Inv::default().restore(db, "ad5_extra", "id", a)?;
            db.execute("UPDATE ad5_extra SET v='a2' WHERE id=?1", [a])?;
            Ok(((), 0, inv))
        })
        .unwrap();
        m.undo().unwrap();
        assert_eq!(id_of(&m, "a"), a, "the edit is undone");
        write_later(&m, "later");
        m.undo().unwrap();
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra WHERE v IN ('a', 'a2', 'b')"),
            0,
            "every row the Add wrote went"
        );
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra WHERE v='later'"), 1);
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 1)]);
        assert!(!table_there(&m, "ad5_gone"));
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn old_op_args_without_head_still_undo() {
        let (mut m, path) = model("ad7old");
        let out = add_created(&mut m, &path);
        // the Add's step as a log written before the head was kept: three args per table
        let (seq, inv): (i64, String) = m
            .sql()
            .query_row("SELECT seq, inv FROM op ORDER BY seq DESC LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let mut j: serde_json::Value = serde_json::from_str(&inv).unwrap();
        let mut n = 0;
        for st in j.as_array_mut().unwrap() {
            if st[0] == DROP_ADDED {
                let a = st[1].as_array_mut().unwrap();
                assert_eq!(a.len(), 5, "table, ranges, sums, head, session");
                a.truncate(3);
                n += 1;
            }
        }
        assert_eq!(n, 2, "one step per created table");
        m.sql()
            .execute(
                "UPDATE op SET inv=?1 WHERE seq=?2",
                rusqlite::params![j.to_string(), seq],
            )
            .unwrap();
        write_later(&m, "later");
        m.undo().unwrap();
        assert_eq!(
            count(&m, "SELECT count(*) FROM ad5_extra WHERE v IN ('a', 'b')"),
            0,
            "the fingerprint alone still takes the Add's rows"
        );
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra WHERE v='later'"), 1);
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 1)]);
        assert!(!table_there(&m, "ad5_gone"));
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn kept_row_keeps_created_table() {
        let (mut m, path) = model("ad7table");
        let out = add_created(&mut m, &path);
        // every Add row rewritten with its own values by a path with no undo step
        m.sql()
            .execute("UPDATE ad5_extra SET v=v", [])
            .unwrap();
        m.undo().unwrap();
        assert!(table_there(&m, "ad5_extra"), "rows written after the Add keep the table");
        assert_eq!(count(&m, "SELECT count(*) FROM ad5_extra"), 2);
        assert_eq!(m.undo_kept(), vec![("ad5_extra".to_string(), 2)]);
        assert!(m.undo_kept().is_empty(), "the note is given once");
        assert!(!table_there(&m, "ad5_gone"), "a table nobody wrote to still goes");
        drop(m);
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn meta_rewritten_same_value_not_user_edit() {
        let (m, _path) = model("meta");
        let put = |k: &str, v: i64| {
            m.sql()
                .execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)",
                    rusqlite::params![k, v],
                )
                .unwrap();
        };
        put("ad4_note", 1);
        let at_copy = m.stage_marks().unwrap().meta;
        let since = m.change_head();
        // rewritten twice: new rowids, same value; a view key twice with new values
        put("ad4_note", 1);
        put("ad4_note", 1);
        put("view:zoom", 2);
        put("view:zoom", 3);
        assert!(!m.meta_user_written_since(since, &at_copy));
        // a real change of a user key
        put("ad4_note", 2);
        assert!(m.meta_user_written_since(since, &at_copy));
        // written back to the value at the copy: nothing to lose
        put("ad4_note", 1);
        assert!(!m.meta_user_written_since(since, &at_copy));
        // a new user key and a removed one are changes
        put("ad4_more", 1);
        assert!(m.meta_user_written_since(since, &at_copy));
        m.sql()
            .execute("DELETE FROM meta WHERE key IN ('ad4_more', 'ad4_note')", [])
            .unwrap();
        assert!(m.meta_user_written_since(since, &at_copy));
    }
}
