//! Import sources (schema 28): origin, last-import base, local edits,
//! 3-way re-import, conflicts.
//!
//! The base (`src_base`) is what the source said about an element at the last import, compactly: the values
//! of columns, outlines and the name in the source (`vals`, JSON) and parameter hashes (`hashes`, 8 bytes each in
//! the key order of the `src_shape` shape). A parameter value is stored only when it differs from the
//! current one (an edit was kept or a conflict); the base holds no copy of `prop`. Fields: columns (`ax`, ...),
//! `p:<key>` is a parameter, `rings` is the slab outlines, `~name` is the name in the source.
//! A local edit = the current value differs from the base (hash); there is no flag.
//! After every import base = new file, so "keep mine" remains a visible edit.
//! Import undo restores the previous base rows from `src_base_prev` (the journal entry is 4 commands).

pub mod file;
mod merge;
pub mod rebase;
pub mod record;
mod resolve;
pub mod target;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as J;

use crate::ops::Inv;
use crate::{Error, Model};

pub use merge::{
    decide, Dead, Decision, Merge, MergeStats, Policy, SourceWrite, GUARD_MIN, MASS_CHANGE,
};
pub use rebase::{PendingLink, SourceRebase, Unwritten, WrittenLink};
pub use resolve::{Pick, ResolveStats};
/// Value types of the source API (for crates with no dependencies of their own on them).
pub use rusqlite::types::Value as SqlValue;
pub use serde_json::Value as JsonValue;

pub(crate) const DDL: &str = "
-- \u{418}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}: \u{43f}\u{440}\u{43e}\u{441}\u{442}\u{440}\u{430}\u{43d}\u{441}\u{442}\u{432}\u{43e} \u{438}\u{43c}\u{451}\u{43d} guid (\u{43a}\u{430}\u{43a} ext_ref.source), \u{43a}\u{43b}\u{44e}\u{447} \u{444}\u{430}\u{439}\u{43b}\u{430}, \u{43f}\u{443}\u{442}\u{44c}, \u{444}\u{43e}\u{440}\u{43c}\u{430}\u{442}, \u{43c}\u{43e}\u{434}\u{443}\u{43b}\u{44c} \u{447}\u{442}\u{435}\u{43d}\u{438}\u{44f},
-- \u{441}\u{432}\u{435}\u{434}\u{435}\u{43d}\u{438}\u{44f} \u{43e} \u{434}\u{43e}\u{43a}\u{443}\u{43c}\u{435}\u{43d}\u{442}\u{435} (JSON), \u{434}\u{430}\u{442}\u{44b} \u{43f}\u{435}\u{440}\u{432}\u{43e}\u{433}\u{43e} \u{438} \u{43f}\u{43e}\u{441}\u{43b}\u{435}\u{434}\u{43d}\u{435}\u{433}\u{43e} \u{438}\u{43c}\u{43f}\u{43e}\u{440}\u{442}\u{430} (unix, \u{441}), \u{438}\u{442}\u{43e}\u{433}\u{438} \u{43f}\u{43e}\u{441}\u{43b}\u{435}\u{434}\u{43d}\u{435}\u{433}\u{43e} (JSON).
CREATE TABLE IF NOT EXISTS src_file(id INTEGER PRIMARY KEY, source TEXT NOT NULL UNIQUE,
    file_key TEXT NOT NULL, path TEXT NOT NULL, format TEXT NOT NULL, reader TEXT NOT NULL,
    reader_version TEXT NOT NULL, info TEXT NOT NULL DEFAULT '{}', first_import INTEGER NOT NULL,
    imported_at INTEGER NOT NULL, imports INTEGER NOT NULL DEFAULT 1, report TEXT NOT NULL DEFAULT '{}');
-- \u{424}\u{43e}\u{440}\u{43c}\u{430} \u{431}\u{430}\u{437}\u{44b}: \u{43e}\u{442}\u{441}\u{43e}\u{440}\u{442}\u{438}\u{440}\u{43e}\u{432}\u{430}\u{43d}\u{43d}\u{44b}\u{435} \u{43a}\u{43b}\u{44e}\u{447}\u{438} \u{445}\u{435}\u{448}\u{438}\u{440}\u{443}\u{435}\u{43c}\u{44b}\u{445} \u{43f}\u{43e}\u{43b}\u{435}\u{439} (JSON-\u{43c}\u{430}\u{441}\u{441}\u{438}\u{432}), \u{43e}\u{431}\u{449}\u{430}\u{44f} \u{443} \u{43f}\u{43e}\u{445}\u{43e}\u{436}\u{438}\u{445} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{43e}\u{432}.
CREATE TABLE IF NOT EXISTS src_shape(id INTEGER PRIMARY KEY, hash INTEGER NOT NULL, keys TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS src_shape_hash ON src_shape(hash);
-- \u{411}\u{430}\u{437}\u{430} \u{43f}\u{43e}\u{441}\u{43b}\u{435}\u{434}\u{43d}\u{435}\u{433}\u{43e} \u{438}\u{43c}\u{43f}\u{43e}\u{440}\u{442}\u{430} \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442}\u{430}: \u{444}\u{43e}\u{440}\u{43c}\u{430} + \u{445}\u{435}\u{448}\u{438}, \u{437}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{44f} (vals); ifc_guid — GlobalId \u{432} \u{438}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}\u{435}.
CREATE TABLE IF NOT EXISTS src_base(id INTEGER PRIMARY KEY, source TEXT NOT NULL, guid TEXT NOT NULL,
    kind TEXT NOT NULL, shape INTEGER NOT NULL DEFAULT 0, hashes BLOB NOT NULL DEFAULT x'',
    vals TEXT NOT NULL DEFAULT '{}', ifc_guid TEXT, UNIQUE(source, guid));
-- \u{41f}\u{430}\u{43a}\u{435}\u{442}\u{44b} \u{438}\u{43c}\u{43f}\u{43e}\u{440}\u{442}\u{430} (\u{43f}\u{43e}\u{441}\u{43b}\u{435}\u{434}\u{43d}\u{438}\u{435} 4 \u{43d}\u{430} \u{438}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}) \u{438} \u{43f}\u{440}\u{435}\u{436}\u{43d}\u{438}\u{435} \u{441}\u{442}\u{440}\u{43e}\u{43a}\u{438} \u{431}\u{430}\u{437}\u{44b}, \u{438}\u{437}\u{43c}\u{435}\u{43d}\u{451}\u{43d}\u{43d}\u{44b}\u{435}/\u{443}\u{434}\u{430}\u{43b}\u{451}\u{43d}\u{43d}\u{44b}\u{435} \u{43f}\u{430}\u{43a}\u{435}\u{442}\u{43e}\u{43c}:
-- undo \u{43f}\u{430}\u{43a}\u{435}\u{442}\u{430} \u{432}\u{43e}\u{437}\u{432}\u{440}\u{430}\u{449}\u{430}\u{435}\u{442} \u{438}\u{445}; \u{43d}\u{435}\u{442} \u{43f}\u{430}\u{43a}\u{435}\u{442}\u{430} (\u{432}\u{44b}\u{442}\u{435}\u{441}\u{43d}\u{435}\u{43d}) — \u{431}\u{430}\u{437}\u{430} \u{438}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}\u{430} \u{441}\u{431}\u{440}\u{430}\u{441}\u{44b}\u{432}\u{430}\u{435}\u{442}\u{441}\u{44f}.
CREATE TABLE IF NOT EXISTS src_batch(id INTEGER PRIMARY KEY, source TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS src_base_prev(batch INTEGER NOT NULL, id INTEGER NOT NULL, source TEXT NOT NULL,
    guid TEXT NOT NULL, kind TEXT NOT NULL, shape INTEGER NOT NULL, hashes BLOB NOT NULL, vals TEXT NOT NULL,
    ifc_guid TEXT);
CREATE INDEX IF NOT EXISTS src_base_prev_batch ON src_base_prev(batch);
-- \u{41e}\u{43f}\u{440}\u{435}\u{434}\u{435}\u{43b}\u{435}\u{43d}\u{438}\u{435} \u{43f}\u{430}\u{440}\u{430}\u{43c}\u{435}\u{442}\u{440}\u{430} \u{432} \u{438}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}\u{435}: \u{433}\u{440}\u{443}\u{43f}\u{43f}\u{430}, \u{442}\u{438}\u{43f}/\u{44d}\u{43a}\u{437}\u{435}\u{43c}\u{43f}\u{43b}\u{44f}\u{440}, \u{43e}\u{431}\u{449}\u{438}\u{439} (NULL — \u{43d}\u{435}\u{438}\u{437}\u{432}\u{435}\u{441}\u{442}\u{43d}\u{43e}), \u{442}\u{438}\u{43f} \u{434}\u{430}\u{43d}\u{43d}\u{44b}\u{445}.
CREATE TABLE IF NOT EXISTS src_param(id INTEGER PRIMARY KEY, source TEXT NOT NULL, key TEXT NOT NULL,
    scope TEXT NOT NULL, grp TEXT NOT NULL DEFAULT '', shared INTEGER, shared_guid TEXT NOT NULL DEFAULT '',
    spec TEXT NOT NULL DEFAULT '', UNIQUE(source, key, scope));
-- \u{41d}\u{435}\u{440}\u{435}\u{448}\u{451}\u{43d}\u{43d}\u{44b}\u{439} \u{43a}\u{43e}\u{43d}\u{444}\u{43b}\u{438}\u{43a}\u{442} \u{43f}\u{43e}\u{432}\u{442}\u{43e}\u{440}\u{43d}\u{43e}\u{433}\u{43e} \u{438}\u{43c}\u{43f}\u{43e}\u{440}\u{442}\u{430}; base/mine/theirs — JSON \u{437}\u{43d}\u{430}\u{447}\u{435}\u{43d}\u{438}\u{44f}, \u{43f}\u{43e}\u{43b}\u{435} '*' — \u{44d}\u{43b}\u{435}\u{43c}\u{435}\u{43d}\u{442} \u{446}\u{435}\u{43b}\u{438}\u{43a}\u{43e}\u{43c}.
CREATE TABLE IF NOT EXISTS src_conflict(id INTEGER PRIMARY KEY, source TEXT NOT NULL, guid TEXT NOT NULL,
    kind TEXT NOT NULL, ref INTEGER NOT NULL, field TEXT NOT NULL, case_kind TEXT NOT NULL,
    base TEXT NOT NULL, mine TEXT NOT NULL, theirs TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS src_conflict_source ON src_conflict(source, guid);
-- \u{412}\u{438}\u{434}\u{44b} \u{438}\u{441}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{43a}\u{430} (\u{438}\u{43c}\u{435}\u{43d}\u{430} \u{441}\u{43e}\u{445}\u{440}\u{430}\u{43d}\u{44f}\u{44e}\u{442}\u{441}\u{44f}): kind plan|ceiling|section|elevation|3d|sheet|schedule|other.
CREATE TABLE IF NOT EXISTS src_view(id INTEGER PRIMARY KEY, source TEXT NOT NULL, guid TEXT NOT NULL,
    name TEXT NOT NULL, kind TEXT NOT NULL, level TEXT, UNIQUE(source, guid));
";

/// Schema 28: source tables (additive, idempotent). The early base form (a full JSON copy of
/// `fields`) is renamed to `src_base_v0`, not deleted; its elements count as "without a base".
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    let early: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('src_base') WHERE name='fields'",
        [],
        |r| r.get(0),
    )?;
    let kept: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='src_base_v0'",
        [],
        |r| r.get(0),
    )?;
    if early > 0 && kept == 0 {
        db.execute_batch("ALTER TABLE src_base RENAME TO src_base_v0")?;
    }
    db.execute_batch(DDL)?;
    file::migrate(db)?;
    Ok(())
}

/// Tables the import writes to (for the undo watermark in an import batch).
pub(crate) const TABLES: [&str; 6] = [
    "src_file",
    "src_base",
    "src_param",
    "src_conflict",
    "src_view",
    "src_shape",
];

/// Element kinds deleted by a re-import when the source no longer contains them.
pub const DELETABLE: [&str; 14] = [
    "solid",
    "wall",
    "opening",
    "slab",
    "room",
    "mesh",
    "curtain_wall",
    "grid",
    "col",
    "beam",
    "ceiling",
    "stair",
    "ramp",
    "railing",
];

/// Base field of an element our own write took out of the file.
pub(crate) const GONE: &str = "~gone";

// ---------- values ----------

/// Row value -> base JSON. A body (BLOB) is length and hash, not bytes: the base does not duplicate meshes.
pub fn enc(v: &Sql) -> J {
    match v {
        Sql::Null => J::Null,
        Sql::Integer(i) => J::from(*i),
        Sql::Real(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
        Sql::Text(s) => J::String(s.clone()),
        Sql::Blob(b) => J::String(format!("\u{0}blob:{}:{:016x}", b.len(), fnv(b))),
    }
}

/// Base JSON -> row value (the inverse of [`enc`]; a body hash is not restorable, giving `None`).
pub fn dec(j: &J) -> Option<Sql> {
    Some(match j {
        J::Null => Sql::Null,
        J::Bool(b) => Sql::Integer(*b as i64),
        J::Number(n) => match n.as_i64() {
            Some(i) => Sql::Integer(i),
            None => Sql::Real(n.as_f64()?),
        },
        J::String(s) if s.starts_with('\u{0}') => return None,
        J::String(s) => Sql::Text(s.clone()),
        J::Object(o) => {
            let h = o.get("blob").and_then(J::as_str)?;
            let d = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            Sql::Blob(
                h.as_bytes()
                    .chunks_exact(2)
                    .map(|p| (d(p[0]) << 4) | d(p[1]))
                    .collect(),
            )
        }
        _ => return None,
    })
}

/// The whole body (`{"blob": hex}`), only for the "theirs" value of a geometry conflict.
pub fn blob_json(b: &[u8]) -> J {
    let s: String = b.iter().map(|x| format!("{x:02x}")).collect();
    serde_json::json!({ "blob": s })
}

pub(crate) use crate::ai_plan::{fnv1a as fnv, fnv1a_more as fnv_more};

/// Base value hash consistent with [`same_value`]: numbers (integer = real) by 10 significant
/// digits, |x| < 1e-9 is zero.
pub fn vhash(j: &J) -> u64 {
    let num = |f: f64| {
        let f = if f.abs() < 1e-9 { 0.0 } else { f };
        fnv(format!("f{f:.9e}").as_bytes())
    };
    match j {
        J::Null => fnv(b"n"),
        J::Bool(b) => num(*b as i64 as f64),
        J::Number(n) => num(n.as_f64().unwrap_or(f64::NAN)),
        J::String(s) => fnv_more(fnv(b"s"), s.as_bytes()),
        other => fnv_more(fnv(b"j"), other.to_string().as_bytes()),
    }
}

/// A base field: a value or only a hash (a parameter that matched the current one when the base was written).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Bv<'a> {
    Val(&'a J),
    Hash(u64),
}

impl Bv<'_> {
    pub(crate) fn same(&self, v: &J) -> bool {
        match self {
            Bv::Val(b) => same_value(b, v),
            Bv::Hash(h) => *h == vhash(v),
        }
    }
    /// For humans: the value; for a hash, `Null` (the value is not stored).
    pub(crate) fn shown(&self) -> J {
        match self {
            Bv::Val(b) => (*b).clone(),
            Bv::Hash(_) => J::Null,
        }
    }
}

/// An element base read from `src_base`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Base {
    pub row: i64,
    pub kind: String,
    pub shape: i64,
    /// Sorted keys of the hashed fields (from `src_shape`).
    pub keys: Arc<Vec<String>>,
    pub hashes: Vec<u64>,
    pub vals: BTreeMap<String, J>,
    pub ifc: Option<String>,
}

impl Base {
    pub(crate) fn get(&self, f: &str) -> Option<Bv<'_>> {
        if let Some(v) = self.vals.get(f) {
            return Some(Bv::Val(v));
        }
        let i = self.keys.binary_search_by(|k| k.as_str().cmp(f)).ok()?;
        self.hashes.get(i).map(|h| Bv::Hash(*h))
    }
    /// All base fields (values and hashes), in order.
    pub(crate) fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.vals.keys().chain(self.keys.iter()).cloned().collect();
        v.sort();
        v.dedup();
        v
    }
}

pub(crate) fn hashes_blob(h: &[u64]) -> Vec<u8> {
    h.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(crate) fn blob_hashes(b: &[u8]) -> Vec<u64> {
    b.chunks_exact(8)
        .map(|c| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
        .collect()
}

/// Base shape cache: id -> keys.
pub(crate) type Shapes = HashMap<i64, Arc<Vec<String>>>;

pub(crate) fn shape_keys(
    db: &Connection,
    shapes: &mut Shapes,
    id: i64,
) -> Result<Arc<Vec<String>>, Error> {
    if let Some(k) = shapes.get(&id) {
        return Ok(k.clone());
    }
    let t: Option<String> = db
        .prepare_cached("SELECT keys FROM src_shape WHERE id=?1")?
        .query_row([id], |r| r.get(0))
        .optional()?;
    let keys: Vec<String> = t
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let k = Arc::new(keys);
    shapes.insert(id, k.clone());
    Ok(k)
}

/// Columns of a `src_base` row for [`base_row`].
pub(crate) const BASE_COLS: &str = "id, kind, shape, hashes, vals, ifc_guid";

type RawBase = (i64, String, i64, Vec<u8>, String, Option<String>);

pub(crate) fn raw_base(r: &rusqlite::Row, at: usize) -> rusqlite::Result<RawBase> {
    Ok((
        r.get(at)?,
        r.get(at + 1)?,
        r.get(at + 2)?,
        r.get(at + 3)?,
        r.get(at + 4)?,
        r.get(at + 5)?,
    ))
}

pub(crate) fn base_row(db: &Connection, shapes: &mut Shapes, raw: RawBase) -> Result<Base, Error> {
    let (row, kind, shape, hashes, vals, ifc) = raw;
    Ok(Base {
        row,
        kind,
        shape,
        keys: shape_keys(db, shapes, shape)?,
        hashes: blob_hashes(&hashes),
        vals: parse_fields(&vals),
        ifc,
    })
}

pub(crate) fn load_base(
    db: &Connection,
    shapes: &mut Shapes,
    source: &str,
    guid: &str,
) -> Result<Option<Base>, Error> {
    let raw = db
        .prepare_cached(&format!(
            "SELECT {BASE_COLS} FROM src_base WHERE source=?1 AND guid=?2"
        ))?
        .query_row(params![source, guid], |r| raw_base(r, 0))
        .optional()?;
    raw.map(|r| base_row(db, shapes, r)).transpose()
}

/// Base value equality: numbers with a relative tolerance of 1e-9 (integer = real).
pub fn same_value(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Number(x), J::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(k)) => i == k,
            _ => {
                let (x, y) = (
                    x.as_f64().unwrap_or(f64::NAN),
                    y.as_f64().unwrap_or(f64::NAN),
                );
                (x - y).abs() <= 1e-9 * x.abs().max(1.0)
            }
        },
        _ => a == b,
    }
}

/// Value text for humans (the conflict dialog, XLSX): numbers as is, a body as "(geometry)".
pub fn show(j: &J) -> String {
    match j {
        J::Null => String::new(),
        J::String(s) if s.starts_with('\u{0}') => "(geometry)".into(),
        J::Object(o) if o.contains_key("blob") => "(geometry)".into(),
        J::String(s) => s.clone(),
        J::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Slab outlines as a base field: `x,y x,y ...|...` (outer first, then holes).
pub fn rings_text(rings: &[Vec<(f64, f64)>]) -> String {
    rings
        .iter()
        .map(|r| {
            r.iter()
                .map(|p| format!("{},{}", p.0, p.1))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Parsing of [`rings_text`].
pub fn parse_rings(t: &str) -> Option<Vec<Vec<(f64, f64)>>> {
    t.split('|')
        .map(|r| {
            r.split(' ')
                .map(|p| {
                    let (x, y) = p.split_once(',')?;
                    Some((x.parse().ok()?, y.parse().ok()?))
                })
                .collect()
        })
        .collect()
}

pub(crate) fn ident(s: &str) -> Result<&str, Error> {
    if !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        Ok(s)
    } else {
        Err(Error::Invalid(format!("bad field name '{s}'")))
    }
}

fn slab_rings(db: &Connection, id: i64) -> Result<String, Error> {
    let mut st =
        db.prepare_cached("SELECT ring, x, y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
    let mut rings: Vec<Vec<(f64, f64)>> = vec![];
    let rows = st.query_map([id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut cur = -1;
    for r in rows {
        let (ring, x, y) = r?;
        if ring != cur {
            rings.push(vec![]);
            cur = ring;
        }
        if let Some(last) = rings.last_mut() {
            last.push((x, y));
        }
    }
    Ok(rings_text(&rings))
}

fn kids_table(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "curtain_wall" => ("curtain_panel", "cw"),
        "roof" => ("roof_vertex", "roof"),
        "ceiling" => ("ceiling_vertex", "ceiling"),
        "railing" => ("railing_vertex", "railing"),
        _ => return None,
    })
}

/// Stable hash of an element's child rows (panels, roof/ceiling/railing vertices) as the
/// merge field `kids`; `None` for kinds without such rows (slab outlines are the `rings` field).
pub fn kids_text(db: &Connection, kind: &str, id: i64) -> Result<Option<String>, Error> {
    let Some((table, col)) = kids_table(kind) else {
        return Ok(None);
    };
    let order = match kind {
        "curtain_wall" => "i, j",
        "ceiling" => "ring, idx",
        _ => "idx",
    };
    let mut st = db.prepare_cached(&format!(
        "SELECT * FROM {table} WHERE {col}=?1 ORDER BY {order}"
    ))?;
    let n = st.column_count();
    // The parent id changes when an element is recreated; it must not count as an edit.
    let skip = st.column_names().position(|c| c == col);
    let mut text = String::new();
    let mut rows = st.query([id])?;
    while let Some(r) = rows.next()? {
        for i in (0..n).filter(|i| Some(*i) != skip) {
            match r.get::<_, Sql>(i)? {
                Sql::Null => text.push('n'),
                Sql::Integer(v) => text.push_str(&v.to_string()),
                Sql::Real(v) => text.push_str(&format!("{v:.9e}")),
                Sql::Text(v) => text.push_str(&v),
                Sql::Blob(b) => text.push_str(&format!("{:x}", fnv(&b))),
            }
            text.push('\x1f');
        }
        text.push('\n');
    }
    Ok(Some(format!("{:016x}", fnv(text.as_bytes()))))
}

/// Current values of an element's fields `keys`; `None` means there is no row. A parameter without a row is `Null`.
pub(crate) fn current_fields<'k>(
    db: &Connection,
    kind: &str,
    id: i64,
    keys: impl IntoIterator<Item = &'k String>,
) -> Result<Option<BTreeMap<String, J>>, Error> {
    let kind = ident(kind)?;
    let keys: Vec<&String> = keys.into_iter().filter(|k| !k.starts_with('~')).collect();
    let cols: Vec<&str> = keys
        .iter()
        .filter(|k| !k.starts_with("p:") && k.as_str() != "rings" && k.as_str() != "kids")
        .map(|k| ident(k))
        .collect::<Result<_, _>>()?;
    let sel = if cols.is_empty() {
        "1".to_string()
    } else {
        cols.join(",")
    };
    let row: Option<Vec<Sql>> = db
        .prepare_cached(&format!("SELECT {sel} FROM {kind} WHERE id=?1"))?
        .query_row([id], |r| {
            (0..cols.len().max(1))
                .map(|i| r.get::<_, Sql>(i))
                .collect::<Result<Vec<_>, _>>()
        })
        .optional()?;
    let Some(row) = row else { return Ok(None) };
    let mut out = BTreeMap::new();
    for (c, v) in cols.iter().zip(&row) {
        out.insert(c.to_string(), enc(v));
    }
    if keys.iter().any(|k| k.starts_with("p:")) {
        let props = props_of(db, kind, id)?;
        for k in keys.iter().filter(|k| k.starts_with("p:")) {
            if let Some(name) = k.strip_prefix(UPARAM_FIELD) {
                if let Some(slot) = uparam_slot(db, kind, id, name)? {
                    out.insert(k.to_string(), uparam_value(db, &slot)?);
                    continue;
                }
            }
            let v = props.get(&k[2..]).map_or(J::Null, |(v, _)| v.clone());
            out.insert(k.to_string(), v);
        }
    }
    if keys.iter().any(|k| k.as_str() == "rings") {
        out.insert("rings".into(), J::String(slab_rings(db, id)?));
    }
    if keys.iter().any(|k| k.as_str() == "kids") {
        out.insert(
            "kids".into(),
            kids_text(db, kind, id)?.map_or(J::Null, J::String),
        );
    }
    Ok(Some(out))
}

/// Project parameter values travel through the merge as `p:DDC_Parameters.<name>` fields, but
/// live in `pparam_val`, not in `prop`; without such a parameter the field is a plain property.
pub const UPARAM_FIELD: &str = "p:DDC_Parameters.";

/// Value slot of project parameter `name` on row (kind, id): (parameter, slot table, slot id);
/// a type parameter's slot is the row's type. None: no such parameter, or no type to hold it.
pub fn uparam_slot(
    db: &Connection,
    kind: &str,
    id: i64,
    name: &str,
) -> Result<Option<(i64, String, i64)>, Error> {
    let p: Option<(i64, i64)> = db
        .prepare_cached("SELECT id, per_type FROM pparam WHERE name=?1 COLLATE NOCASE")?
        .query_row([name], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((pid, per_type)) = p else {
        return Ok(None);
    };
    if per_type == 0 {
        return Ok(Some((pid, kind.to_string(), id)));
    }
    Ok(crate::params::type_of(db, kind, id)?.map(|(t, tid)| (pid, t.to_string(), tid)))
}

/// Stored value of a slot ([`uparam_slot`]); Null when the slot has none.
pub fn uparam_value(db: &Connection, slot: &(i64, String, i64)) -> Result<J, Error> {
    let v: Option<Sql> = db
        .prepare_cached("SELECT value FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3")?
        .query_row(params![slot.0, slot.1, slot.2], |r| r.get(0))
        .optional()?;
    Ok(v.map_or(J::Null, |v| enc(&v)))
}

/// True: the value in a slot is locked (`Model::set_param_lock`); import writers skip it.
pub fn uparam_locked(db: &Connection, slot: &(i64, String, i64)) -> Result<bool, Error> {
    crate::groups::is_locked_param(db, &slot.1, slot.2, slot.0)
}

/// Writes `v` into a slot through `snaps` (Null removes the value): undo puts the old row back
/// with its rowid. False: the slot is locked and kept as it is.
pub fn write_uparam(
    db: &Connection,
    snaps: &mut crate::Snapshots,
    slot: &(i64, String, i64),
    v: &Sql,
) -> Result<bool, Error> {
    if uparam_locked(db, slot)? {
        return Ok(false);
    }
    let old: Option<(i64, Sql)> = db
        .prepare_cached(
            "SELECT rowid, value FROM pparam_val WHERE param=?1 AND kind=?2 AND ref=?3",
        )?
        .query_row(params![slot.0, slot.1, slot.2], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    match (old, v) {
        (None, Sql::Null) => {}
        (Some((_, ov)), _) if ov == *v => {}
        (Some((rowid, _)), Sql::Null) => {
            snaps.save(db, "pparam_val", "rowid", rowid)?;
            db.execute("DELETE FROM pparam_val WHERE rowid=?1", [rowid])?;
        }
        (Some((rowid, _)), _) => {
            snaps.save(db, "pparam_val", "rowid", rowid)?;
            db.execute(
                "UPDATE pparam_val SET value=?2 WHERE rowid=?1",
                params![rowid, v],
            )?;
        }
        (None, _) => {
            snaps.watermark(db, "pparam_val")?;
            db.execute(
                "INSERT INTO pparam_val(param, kind, ref, value) VALUES (?1,?2,?3,?4)",
                params![slot.0, slot.1, slot.2, v],
            )?;
        }
    }
    Ok(true)
}

/// Row parameters: key -> (value, unit).
pub(crate) fn props_of(
    db: &Connection,
    kind: &str,
    id: i64,
) -> Result<HashMap<String, (J, String)>, Error> {
    let mut st = db.prepare_cached("SELECT key, value, unit FROM prop WHERE kind=?1 AND ref=?2")?;
    let rows = st.query_map(params![kind, id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Sql>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let (k, v, u) = r?;
        out.insert(k, (enc(&v), u));
    }
    Ok(out)
}

pub(crate) fn parse_fields(t: &str) -> BTreeMap<String, J> {
    match serde_json::from_str::<J>(t) {
        Ok(J::Object(o)) => o.into_iter().collect(),
        _ => BTreeMap::new(),
    }
}

pub(crate) fn fields_text(f: &BTreeMap<String, J>) -> String {
    J::Object(f.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).to_string()
}

/// Differences of the current state from the base (`~` fields are not compared). A parameter base stored
/// only as a hash is shown as `Null`.
pub(crate) fn diff(base: &Base, now: &BTreeMap<String, J>) -> Vec<FieldDiff> {
    base.names()
        .into_iter()
        .filter(|k| !k.starts_with('~'))
        .filter_map(|k| {
            let b = base.get(&k)?;
            let m = now.get(&k).cloned().unwrap_or(J::Null);
            (!b.same(&m)).then(|| FieldDiff {
                base: b.shown(),
                field: k,
                mine: m,
            })
        })
        .collect()
}

// ---------- types ----------

/// An imported source (the "Origin" panel).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceInfo {
    pub source: String,
    pub file_key: String,
    pub path: String,
    pub format: String,
    pub reader: String,
    pub reader_version: String,
    /// Document information from the reader module (file format version, project, ...).
    pub info: BTreeMap<String, String>,
    pub first_import: i64,
    pub imported_at: i64,
    pub imports: i64,
    /// Results of the last import: key -> number (added, merged, conflicts, ...).
    pub report: BTreeMap<String, i64>,
    /// Elements with a base, unresolved conflicts.
    pub elements: i64,
    pub conflicts: i64,
}

/// A parameter definition in the source.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ParamDef {
    pub key: String,
    pub group: String,
    /// A type parameter (otherwise an instance one).
    pub type_param: bool,
    /// A shared parameter (otherwise a project/built-in one); None if the source did not say.
    pub shared: Option<bool>,
    pub shared_guid: String,
    /// Data type in the source (length, area, text, ...), as reported by the reader module.
    pub spec: String,
}

/// An element parameter with source metadata and a local-edit flag.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceParam {
    pub key: String,
    pub value: J,
    pub unit: String,
    pub group: String,
    /// "instance" | "type".
    pub scope: &'static str,
    pub shared: Option<bool>,
    /// Value at the last import; None if the parameter was added locally or the element is not from a source.
    pub base: Option<J>,
    pub edited: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDiff {
    pub field: String,
    pub base: J,
    pub mine: J,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Edited,
    DeletedLocally,
    AddedLocally,
}

impl ChangeKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Edited => "edited",
            Self::DeletedLocally => "deleted",
            Self::AddedLocally => "added",
        }
    }
}

/// A local change relative to the source (for exporting a delta).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub source: String,
    /// Empty for locally added ones.
    pub guid: String,
    pub ifc_guid: Option<String>,
    pub kind: String,
    pub id: i64,
    pub change: ChangeKind,
    /// For an edit the changed fields; empty for a deletion; empty for an addition.
    pub fields: Vec<FieldDiff>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictCase {
    /// The field changed both here and in the source, differently.
    EditEdit,
    /// Changed here, deleted in the source (an element or a parameter).
    EditDelete,
    /// Deleted here, changed in the source.
    DeleteEdit,
}

impl ConflictCase {
    pub fn key(self) -> &'static str {
        match self {
            Self::EditEdit => "edit_edit",
            Self::EditDelete => "edit_delete",
            Self::DeleteEdit => "delete_edit",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "edit_edit" => Self::EditEdit,
            "edit_delete" => Self::EditDelete,
            "delete_edit" => Self::DeleteEdit,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub id: i64,
    pub source: String,
    pub guid: String,
    pub kind: String,
    pub element: i64,
    /// A field name, or `*` for the whole element.
    pub field: String,
    pub case: ConflictCase,
    pub base: J,
    pub mine: J,
    pub theirs: J,
}

/// An element's origin.
#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub source: String,
    pub guid: String,
    pub ifc_guid: Option<String>,
    pub path: String,
    pub imported_at: i64,
    /// Has a base (the element was imported in schema 28+); without it edits are not tracked.
    pub tracked: bool,
    pub edited: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceView {
    pub guid: String,
    pub name: String,
    pub kind: String,
    pub level: Option<String>,
}

pub(crate) fn conflict_row(r: &rusqlite::Row) -> rusqlite::Result<Conflict> {
    let j = |i: usize| -> rusqlite::Result<J> {
        let t: String = r.get(i)?;
        Ok(serde_json::from_str(&t).unwrap_or(J::Null))
    };
    Ok(Conflict {
        id: r.get(0)?,
        source: r.get(1)?,
        guid: r.get(2)?,
        kind: r.get(3)?,
        element: r.get(4)?,
        field: r.get(5)?,
        case: ConflictCase::parse(&r.get::<_, String>(6)?).unwrap_or(ConflictCase::EditEdit),
        base: j(7)?,
        mine: j(8)?,
        theirs: j(9)?,
    })
}

pub(crate) const CONFLICT_COLS: &str = "id,source,guid,kind,ref,field,case_kind,base,mine,theirs";

/// An element's reference to a source: (source, guid) by (kind, id). The first by rowid if there are several.
fn link_of(db: &Connection, kind: &str, id: i64) -> Result<Option<(String, String)>, Error> {
    Ok(db
        .prepare_cached(
            "SELECT source, guid FROM ext_ref WHERE kind=?1 AND ref=?2 ORDER BY rowid LIMIT 1",
        )?
        .query_row(params![kind, id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

fn base_of(db: &Connection, source: &str, guid: &str) -> Result<Option<Base>, Error> {
    load_base(db, &mut Shapes::new(), source, guid)
}

impl Model {
    /// Imported sources, by last-import date (newest first).
    pub fn sources(&self) -> Result<Vec<SourceInfo>, Error> {
        let mut st = self.db.prepare(
            "SELECT source,file_key,path,format,reader,reader_version,info,first_import,imported_at,\
             imports,report,(SELECT count(*) FROM src_base b WHERE b.source=f.source),\
             (SELECT count(*) FROM src_conflict c WHERE c.source=f.source) \
             FROM src_file f ORDER BY imported_at DESC, id DESC",
        )?;
        let rows = st.query_map([], |r| {
            let info: String = r.get(6)?;
            let rep: String = r.get(10)?;
            Ok(SourceInfo {
                source: r.get(0)?,
                file_key: r.get(1)?,
                path: r.get(2)?,
                format: r.get(3)?,
                reader: r.get(4)?,
                reader_version: r.get(5)?,
                info: parse_fields(&info)
                    .into_iter()
                    .map(|(k, v)| (k, show(&v)))
                    .collect(),
                first_import: r.get(7)?,
                imported_at: r.get(8)?,
                imports: r.get(9)?,
                report: parse_fields(&rep)
                    .into_iter()
                    .filter_map(|(k, v)| Some((k, v.as_i64()?)))
                    .collect(),
                elements: r.get(11)?,
                conflicts: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A source by namespace.
    pub fn source_info(&self, source: &str) -> Result<Option<SourceInfo>, Error> {
        Ok(self.sources()?.into_iter().find(|s| s.source == source))
    }

    /// A model row by source guid, if it is alive.
    pub fn element_by_guid(
        &self,
        source: &str,
        guid: &str,
    ) -> Result<Option<(String, i64)>, Error> {
        let r: Option<(String, i64)> = self
            .db
            .query_row(
                "SELECT kind, ref FROM ext_ref WHERE source=?1 AND guid=?2",
                params![source, guid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        // a door or window filler is linked as `opening_fill` but lives on its opening row
        let r = r.map(|(k, id)| if k == "opening_fill" { ("opening".to_string(), id) } else { (k, id) });
        match r {
            Some((k, id)) if current_fields(&self.db, &k, id, &Vec::<String>::new())?.is_some() => {
                Ok(Some((k, id)))
            }
            _ => Ok(None),
        }
    }

    /// Where an element comes from: source, guid, GlobalId, file, import date, whether edited locally.
    pub fn provenance(&self, kind: &str, id: i64) -> Result<Option<Provenance>, Error> {
        let Some((source, guid)) = link_of(&self.db, kind, id)? else {
            return Ok(None);
        };
        let file: Option<(String, i64)> = self
            .db
            .query_row(
                "SELECT path, imported_at FROM src_file WHERE source=?1",
                [&source],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (path, imported_at) = file.unwrap_or_default();
        let base = base_of(&self.db, &source, &guid)?.filter(|b| b.kind == kind);
        let (tracked, edited, ifc_guid) = match &base {
            Some(b) => {
                let now = current_fields(&self.db, kind, id, &b.names())?.unwrap_or_default();
                let edited = !diff(b, &now).is_empty()
                    || added_props(&self.db, b, kind, id)?.next().is_some();
                (true, edited, b.ifc.clone())
            }
            None => (false, false, None),
        };
        Ok(Some(Provenance {
            source,
            guid,
            ifc_guid,
            path,
            imported_at,
            tracked,
            edited,
        }))
    }

    /// Fields changed locally since the last import (empty if not edited / not from a source).
    pub fn element_diff(&self, kind: &str, id: i64) -> Result<Vec<FieldDiff>, Error> {
        let Some((source, guid)) = link_of(&self.db, kind, id)? else {
            return Ok(vec![]);
        };
        let Some(base) = base_of(&self.db, &source, &guid)?.filter(|b| b.kind == kind) else {
            return Ok(vec![]);
        };
        let now = current_fields(&self.db, kind, id, &base.names())?.unwrap_or_default();
        let mut d = diff(&base, &now);
        // parameters added locally to an imported element
        d.extend(added_props(&self.db, &base, kind, id)?);
        d.sort_by(|a, b| a.field.cmp(&b.field));
        Ok(d)
    }

    pub fn is_edited(&self, kind: &str, id: i64) -> Result<bool, Error> {
        Ok(!self.element_diff(kind, id)?.is_empty())
    }

    /// All local changes relative to the source: edits, deletions; with `added`, also elements
    /// of kinds [`DELETABLE`] with no reference to any source.
    pub fn local_changes(&self, source: &str, added: bool) -> Result<Vec<Change>, Error> {
        let mut out = vec![];
        let mut shapes = Shapes::new();
        let mut st = self.db.prepare(
            "SELECT b.guid, b.id, b.kind, b.shape, b.hashes, b.vals, b.ifc_guid, e.kind, e.ref \
             FROM src_base b LEFT JOIN ext_ref e ON e.source=b.source AND e.guid=b.guid \
             WHERE b.source=?1 ORDER BY b.kind, b.guid",
        )?;
        let rows = st.query_map([source], |r| {
            Ok((
                r.get::<_, String>(0)?,
                raw_base(r, 1)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<i64>>(8)?,
            ))
        })?;
        for r in rows {
            let (guid, raw, link_kind, id) = r?;
            let base = base_row(&self.db, &mut shapes, raw)?;
            // the element's table comes from the reference (a base of another kind is not compared with this row)
            let kind = link_kind.unwrap_or_else(|| base.kind.clone());
            let now = match id {
                Some(id) if kind == base.kind => {
                    current_fields(&self.db, &kind, id, &base.names())?
                }
                Some(id) => current_fields(&self.db, &kind, id, &Vec::<String>::new())?,
                None => None,
            };
            let gone = base.vals.contains_key(GONE);
            let (change, fields) = match now {
                None if gone => continue,
                None => (ChangeKind::DeletedLocally, vec![]),
                // an undo brought back what the file no longer has
                Some(_) if gone && !added => continue,
                Some(_) if gone => (ChangeKind::AddedLocally, vec![]),
                Some(_) if kind != base.kind => continue,
                Some(now) => {
                    let mut d = diff(&base, &now);
                    d.extend(added_props(&self.db, &base, &kind, id.unwrap_or(0))?);
                    if d.is_empty() {
                        continue;
                    }
                    d.sort_by(|a, b| a.field.cmp(&b.field));
                    (ChangeKind::Edited, d)
                }
            };
            out.push(Change {
                source: source.into(),
                guid,
                ifc_guid: base.ifc.clone(),
                kind,
                id: id.unwrap_or(0),
                change,
                fields,
            });
        }
        if added {
            for kind in DELETABLE {
                let mut st = self.db.prepare(&format!(
                    "SELECT id FROM {kind} WHERE id NOT IN (SELECT ref FROM ext_ref WHERE kind='{kind}') ORDER BY id"
                ))?;
                let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                out.extend(ids.into_iter().map(|id| Change {
                    source: source.into(),
                    guid: String::new(),
                    ifc_guid: None,
                    kind: kind.into(),
                    id,
                    change: ChangeKind::AddedLocally,
                    fields: vec![],
                }));
            }
        }
        Ok(out)
    }

    /// Element parameters with group, scope (type/instance), shared and edited flags.
    /// `type.*` keys are type parameters copied to the instance.
    pub fn source_params(&self, kind: &str, id: i64) -> Result<Vec<SourceParam>, Error> {
        let link = link_of(&self.db, kind, id)?;
        let base = match &link {
            Some((s, g)) => base_of(&self.db, s, g)?.filter(|b| b.kind == kind),
            None => None,
        };
        let source = link.as_ref().map(|l| l.0.clone()).unwrap_or_default();
        let defs = self.param_defs(&source)?;
        let type_row = matches!(
            kind,
            "wall_type" | "slab_type" | "member_type" | "comp_type"
        );
        let mut out: Vec<SourceParam> = props_of(&self.db, kind, id)?
            .into_iter()
            .map(|(key, (value, unit))| {
                let is_type = type_row || key.starts_with("type.");
                let bare = key.strip_prefix("type.").unwrap_or(&key);
                let def = defs
                    .get(&(bare.to_string(), is_type))
                    .or_else(|| defs.get(&(bare.to_string(), !is_type)));
                let f = format!("p:{key}");
                let bv = base.as_ref().and_then(|b| b.get(&f));
                let edited = match (&base, &bv) {
                    (Some(_), Some(b)) => !b.same(&value),
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                let b = bv.map(|b| b.shown());
                SourceParam {
                    group: def.map(|d| d.group.clone()).unwrap_or_default(),
                    scope: if def.map_or(is_type, |d| d.type_param) {
                        "type"
                    } else {
                        "instance"
                    },
                    shared: def.and_then(|d| d.shared),
                    key,
                    value,
                    unit,
                    base: b,
                    edited,
                }
            })
            .collect();
        out.sort_by(|a, b| (a.scope, &a.group, &a.key).cmp(&(b.scope, &b.group, &b.key)));
        Ok(out)
    }

    /// Source parameter definitions: (key, type?) -> definition.
    pub fn param_defs(&self, source: &str) -> Result<HashMap<(String, bool), ParamDef>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT key, scope, grp, shared, shared_guid, spec FROM src_param WHERE source=?1",
        )?;
        let rows = st.query_map([source], |r| {
            Ok(ParamDef {
                key: r.get(0)?,
                type_param: r.get::<_, String>(1)? == "type",
                group: r.get(2)?,
                shared: r.get::<_, Option<i64>>(3)?.map(|v| v != 0),
                shared_guid: r.get(4)?,
                spec: r.get(5)?,
            })
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let d = r?;
            out.insert((d.key.clone(), d.type_param), d);
        }
        Ok(out)
    }

    /// Number of child rows (panels, roof/ceiling/railing vertices) of an element; `None` for other kinds.
    pub fn kids_count(&self, kind: &str, id: i64) -> Option<i64> {
        let (table, col) = kids_table(kind)?;
        self.db
            .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE {col}=?1"), [id], |r| r.get(0))
            .ok()
    }

    /// Unresolved conflicts (all sources or one).
    pub fn conflicts(&self, source: Option<&str>) -> Result<Vec<Conflict>, Error> {
        let mut st = self.db.prepare(&format!(
            "SELECT {CONFLICT_COLS} FROM src_conflict WHERE ?1 IS NULL OR source=?1 ORDER BY id"
        ))?;
        let v = st
            .query_map([source], conflict_row)?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Source views with their names.
    pub fn source_views(&self, source: &str) -> Result<Vec<SourceView>, Error> {
        let mut st = self.db.prepare(
            "SELECT guid, name, kind, level FROM src_view WHERE source=?1 ORDER BY kind, name",
        )?;
        let v = st
            .query_map([source], |r| {
                Ok(SourceView {
                    guid: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    level: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(v)
    }

    /// Source guid and GlobalId of rows of kind `kind`: id -> (guid, ifc_guid). For IFC export with
    /// the original GlobalIds; of several sources the first by rowid is taken.
    pub fn source_ids(&self, kind: &str) -> Result<HashMap<i64, (String, Option<String>)>, Error> {
        let mut st = self.db.prepare_cached(
            "SELECT e.ref, e.guid, b.ifc_guid FROM ext_ref e LEFT JOIN src_base b \
             ON b.source=e.source AND b.guid=e.guid WHERE e.kind=?1 ORDER BY e.rowid DESC",
        )?;
        let rows = st.query_map([kind], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (id, g, ifc) = r?;
            out.insert(id, (g, ifc));
        }
        Ok(out)
    }

    /// An element parameter value (an edit in the source parameters panel): one operation, undoable.
    /// `Null` deletes the parameter. The unit is kept; for a new parameter it is `unit`.
    pub fn set_source_prop(
        &mut self,
        kind: &str,
        id: i64,
        key: &str,
        value: Sql,
        unit: &str,
    ) -> Result<(), Error> {
        let kind = ident(kind)?.to_string();
        if key.is_empty() {
            return Err(Error::Invalid("empty parameter name".into()));
        }
        if current_fields(&self.db, &kind, id, &Vec::<String>::new())?.is_none() {
            return Err(Error::Invalid(format!("no such {kind} {id}")));
        }
        if let Sql::Real(f) = value {
            if !f.is_finite() {
                return Err(Error::Invalid("value is not finite".into()));
            }
        }
        self.op("prop_set", |db| {
            let old: Option<String> = db
                .query_row(
                    "SELECT unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    params![kind, id, key],
                    |r| r.get(0),
                )
                .optional()?;
            let inv = match &old {
                Some(_) => restore_prop(db, &kind, id, key)?,
                None => Inv::default().sql(
                    "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    vec![
                        Sql::Text(kind.clone()),
                        Sql::Integer(id),
                        Sql::Text(key.into()),
                    ],
                ),
            };
            if value == Sql::Null {
                db.execute(
                    "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    params![kind, id, key],
                )?;
            } else {
                let u = old.unwrap_or_else(|| unit.to_string());
                db.execute(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                    params![kind, id, key, value, u],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// One parameter value on many elements (a multi-selection edit): one operation, one undo
    /// step. Checked before anything is written; a failure on the way rolls all of it back.
    pub fn set_source_props(
        &mut self,
        items: &[(&str, i64)],
        key: &str,
        value: Sql,
        unit: &str,
    ) -> Result<(), Error> {
        if key.is_empty() {
            return Err(Error::Invalid("empty parameter name".into()));
        }
        if let Sql::Real(f) = value {
            if !f.is_finite() {
                return Err(Error::Invalid("value is not finite".into()));
            }
        }
        for (kind, id) in items {
            if current_fields(&self.db, kind, *id, &Vec::<String>::new())?.is_none() {
                return Err(Error::Invalid(format!("no such {kind} {id}")));
            }
        }
        self.compound("prop_set", |m| {
            for (kind, id) in items {
                m.set_source_prop(kind, *id, key, value.clone(), unit)?;
            }
            Ok(())
        })
    }

    /// Removes the parameter `key` from every element that has it: one operation, one undo step.
    /// Returns how many elements lost it; none having it is an error and writes nothing. For an
    /// imported element the source diff then carries `p:key` as removed and the patch drops it.
    pub fn remove_source_props(
        &mut self,
        items: &[(&str, i64)],
        key: &str,
    ) -> Result<usize, Error> {
        if key.is_empty() {
            return Err(Error::Invalid("empty parameter name".into()));
        }
        let mut have: Vec<(&str, i64)> = vec![];
        for (kind, id) in items {
            if current_fields(&self.db, kind, *id, &Vec::<String>::new())?.is_none() {
                return Err(Error::Invalid(format!("no such {kind} {id}")));
            }
            if props_of(&self.db, kind, *id)?.contains_key(key) {
                have.push((*kind, *id));
            }
        }
        if have.is_empty() {
            return Err(Error::Invalid(format!("no parameter {key}")));
        }
        self.compound("prop_remove", |m| {
            for (kind, id) in &have {
                m.set_source_prop(kind, *id, key, Sql::Null, "")?;
            }
            Ok(())
        })?;
        Ok(have.len())
    }
}

/// An inverse command for one `prop` row (a composite key; `Inv::restore` takes all by ref).
pub(crate) fn restore_prop(db: &Connection, kind: &str, id: i64, key: &str) -> Result<Inv, Error> {
    let row: Option<(Sql, String)> = db
        .query_row(
            "SELECT value, unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            params![kind, id, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((v, u)) => Inv::default().sql(
            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
            vec![
                Sql::Text(kind.into()),
                Sql::Integer(id),
                Sql::Text(key.into()),
                v,
                Sql::Text(u),
            ],
        ),
        None => Inv::default().sql(
            "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
            vec![
                Sql::Text(kind.into()),
                Sql::Integer(id),
                Sql::Text(key.into()),
            ],
        ),
    })
}

/// Row parameters absent from the base (added here), as edits.
fn added_props<'b>(
    db: &Connection,
    base: &'b Base,
    kind: &str,
    id: i64,
) -> Result<impl Iterator<Item = FieldDiff> + 'b, Error> {
    let props = props_of(db, kind, id)?;
    Ok(props.into_iter().filter_map(move |(k, (v, _))| {
        let f = format!("p:{k}");
        base.get(&f).is_none().then_some(FieldDiff {
            field: f,
            base: J::Null,
            mine: v,
        })
    }))
}
