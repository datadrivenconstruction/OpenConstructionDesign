//! Comparison of two file versions: per-table counters (restore dialog), a list of changed
//! elements with their plan location (compare dialog), a content hash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

pub use crate::compare_keys::{FieldChange, FieldGroup};
use crate::file::TableDiff;
use crate::merge::tables::{self, q, RefCol, Schema};
use crate::{Error, Model};

/// The list shows no more rows than this (`VersionDiff::total` counts all).
const LIST_CAP: usize = 20_000;

fn path_text(p: &Path) -> Result<&str, Error> {
    p.to_str()
        .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))
}

pub(crate) fn table_counts(db: &Connection, snap: &Path) -> Result<Vec<TableDiff>, Error> {
    db.execute("ATTACH DATABASE ?1 AS snap", [path_text(snap)?])?;
    let res = (|| -> Result<Vec<TableDiff>, Error> {
        let mut st = db.prepare(
            "SELECT m.name FROM main.sqlite_master m JOIN snap.sqlite_master s \
             ON s.name=m.name AND s.type='table' WHERE m.type='table' \
             AND m.name NOT LIKE 'sqlite_%' AND m.name NOT IN ('mesh_proto', 'mesh_geo', \
             'param_lock', 'calc_run', 'calc_blob', 'value_src') ORDER BY m.name",
        )?;
        let names: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut out = vec![];
        for t in names {
            let cols = |schema: &str| -> Result<Vec<String>, Error> {
                let mut c = db.prepare(&format!(
                    "SELECT name FROM pragma_table_info('{t}', '{schema}')"
                ))?;
                let v = c.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                Ok(v)
            };
            // columns of both files: a snapshot from before a column was added still compares
            let b = cols("snap")?;
            let a: Vec<String> = cols("main")?.into_iter().filter(|c| b.contains(c)).collect();
            if !a.iter().any(|c| c == "id") {
                continue;
            }
            let list = a
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            let n = |sql: String| -> Result<u64, Error> {
                Ok(db.query_row(&sql, [], |r| r.get::<_, i64>(0))? as u64)
            };
            let added = n(format!(
                "SELECT count(*) FROM snap.\"{t}\" WHERE id NOT IN (SELECT id FROM main.\"{t}\")"
            ))?;
            let removed = n(format!(
                "SELECT count(*) FROM main.\"{t}\" WHERE id NOT IN (SELECT id FROM snap.\"{t}\")"
            ))?;
            let differ = n(format!(
                "SELECT count(*) FROM (SELECT {list} FROM snap.\"{t}\" EXCEPT SELECT {list} FROM main.\"{t}\")"
            ))?;
            let changed = differ.saturating_sub(added);
            if added + removed + changed > 0 {
                out.push(TableDiff {
                    table: t,
                    added,
                    removed,
                    changed,
                });
            }
        }
        Ok(out)
    })();
    let _ = db.execute("DETACH DATABASE snap", []);
    res
}

/// How an element differs from the other version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    /// Present only in the open model.
    HereOnly,
    /// Present only in the other version.
    ThereOnly,
    /// Present in both, fields differ.
    Differs,
}

/// Element location: plan bounding box and level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub level: Option<i64>,
    pub z: f64,
}

/// A single difference.
#[derive(Clone, Debug, PartialEq)]
pub struct ElementDiff {
    pub table: String,
    /// Id in the version it exists in (the other version for `ThereOnly`).
    pub id: i64,
    pub kind: DiffKind,
    /// Element key (generator, script, imported or minted), when it has one.
    pub key: Option<String>,
    /// Id of the paired element in the other version (`Differs` only).
    pub other: Option<i64>,
    /// Differing fields (a `#`-prefixed entry denotes the row group of a part).
    pub fields: Vec<String>,
    /// Old (other version) and new (this one) value of every changed field.
    pub changes: Vec<FieldChange>,
    pub spot: Option<Spot>,
}

/// Result of comparing with another file version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VersionDiff {
    /// First differences (up to the list limit), by table.
    pub elements: Vec<ElementDiff>,
    /// Total number of differences.
    pub total: usize,
    pub tables: Vec<TableDiff>,
}

fn has(cols: &[String], names: &[&str]) -> bool {
    names.iter().all(|n| cols.iter().any(|c| c == n))
}

fn box_of(pts: impl Iterator<Item = (f64, f64)>) -> Option<([f64; 2], [f64; 2])> {
    let mut b: Option<([f64; 2], [f64; 2])> = None;
    for (x, y) in pts {
        let (lo, hi) = b.get_or_insert(([x, y], [x, y]));
        lo[0] = lo[0].min(x);
        lo[1] = lo[1].min(y);
        hi[0] = hi[0].max(x);
        hi[1] = hi[1].max(y);
    }
    b
}

fn level_of(
    db: &Connection,
    schema: &str,
    t: &tables::Table,
    id: i64,
) -> Result<(Option<i64>, f64), Error> {
    if !has(&t.cols, &["level"]) {
        return Ok((None, 0.0));
    }
    let level: Option<i64> = db
        .query_row(
            &format!("SELECT level FROM {schema}.{} WHERE id=?1", q(&t.name)),
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(l) = level else {
        return Ok((None, 0.0));
    };
    let z: f64 = db
        .query_row(
            &format!("SELECT elevation FROM {schema}.level WHERE id=?1"),
            [l],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0.0);
    Ok((Some(l), z))
}

fn extent(
    db: &Connection,
    s: &Schema,
    schema: &str,
    t: &tables::Table,
    id: i64,
) -> Result<Option<([f64; 2], [f64; 2])>, Error> {
    let tq = q(&t.name);
    if has(&t.cols, &["ax", "ay", "bx", "by"]) {
        let row: Option<(f64, f64, f64, f64)> = db
            .query_row(
                &format!("SELECT ax, ay, bx, \"by\" FROM {schema}.{tq} WHERE id=?1"),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        return Ok(row.and_then(|(a, b, c, d)| box_of([(a, b), (c, d)].into_iter())));
    }
    if has(&t.cols, &["x", "y"]) {
        let row: Option<(f64, f64)> = db
            .query_row(
                &format!("SELECT x, y FROM {schema}.{tq} WHERE id=?1"),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        return Ok(row.and_then(|p| box_of(std::iter::once(p))));
    }
    if t.name == "mep_run" {
        let text: Option<String> = db
            .query_row(
                &format!("SELECT points FROM {schema}.mep_run WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let pts = text.as_deref().and_then(crate::mep::parse_points);
        return Ok(pts.and_then(|v| box_of(v.into_iter().map(|p| (p.0, p.1)))));
    }
    for (_, c) in s.kids_of(&t.name) {
        if matches!(c.spec.owner, tables::Owner::Col(..)) && has(&c.cols, &["x", "y"]) {
            let pts: Vec<(f64, f64)> = db
                .prepare(&format!(
                    "SELECT x, y FROM {schema}.{} WHERE {}=?1",
                    q(c.spec.table),
                    q(c.owner_col())
                ))?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            if !pts.is_empty() {
                return Ok(box_of(pts.into_iter()));
            }
        }
    }
    Ok(None)
}

/// Element location; for openings and similar, the host's location.
fn spot(
    db: &Connection,
    s: &Schema,
    schema: &str,
    table: &str,
    id: i64,
    depth: u8,
) -> Result<Option<Spot>, Error> {
    let Some(t) = s.table(table) else {
        return Ok(None);
    };
    if let Some((min, max)) = extent(db, s, schema, t, id)? {
        let (level, z) = level_of(db, schema, t, id)?;
        return Ok(Some(Spot { min, max, level, z }));
    }
    if depth == 0 {
        return Ok(None);
    }
    for r in &t.refs {
        if let RefCol::To { col, parent } = r {
            if t.notnull.contains(col) {
                let host: Option<i64> = db
                    .query_row(
                        &format!("SELECT {} FROM {schema}.{} WHERE id=?1", q(col), q(table)),
                        [id],
                        |x| x.get(0),
                    )
                    .optional()?;
                if let Some(h) = host {
                    if let Some(sp) = spot(db, s, schema, parent, h, depth - 1)? {
                        return Ok(Some(sp));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// Places of elements of this model, skipping those without one.
pub(crate) fn spots_here(db: &Connection, keys: &[(&str, i64)]) -> Result<Vec<Spot>, Error> {
    let s = tables::load(db, &[])?;
    let mut out = vec![];
    for (t, id) in keys {
        if let Some(sp) = spot(db, &s, "main", t, *id, 2)? {
            out.push(sp);
        }
    }
    Ok(out)
}

/// Copy of the other version migrated to this build's schema (callable from a background thread).
pub fn prepare_compare(source: &Path, work: &Path) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(work)?;
    let copy = work.join("other.ddc");
    drop(Model::open_snapshot(source, &copy)?);
    Ok(copy)
}

fn diff_on(db: &Connection, copy: &Path) -> Result<VersionDiff, Error> {
    let tables_diff = table_counts(db, copy)?;
    db.execute("ATTACH DATABASE ?1 AS their", [path_text(copy)?])?;
    let res = (|| -> Result<VersionDiff, Error> {
        let s = tables::load(db, &["their"])?;
        let p = crate::compare_keys::pair(db, &s)?;
        let mut parts = p.part_changes(db, &s)?;
        // (table, id, other version) -> (kind, id in the other version, changes)
        type Found = BTreeMap<(String, i64, bool), (DiffKind, Option<i64>, Vec<FieldChange>)>;
        let mut found: Found = BTreeMap::new();
        for t in &s.tables {
            let n = &t.name;
            for id in p.here.ids(n) {
                if p.there_of(n, id).is_none() {
                    found.insert((n.clone(), id, false), (DiffKind::HereOnly, None, vec![]));
                }
            }
            for id in p.there.ids(n) {
                if p.here_of(n, id).is_none() {
                    found.insert((n.clone(), id, true), (DiffKind::ThereOnly, None, vec![]));
                }
            }
            for (here, there) in p.pairs_of(n) {
                let mut ch = p.column_changes(&s, t, here, there);
                if let Some(x) = parts.remove(&(n.clone(), here)) {
                    ch.extend(x);
                }
                if !ch.is_empty() {
                    found.insert((n.clone(), here, false), (DiffKind::Differs, Some(there), ch));
                }
            }
        }
        let total = found.len();
        let mut elements = Vec::with_capacity(total.min(LIST_CAP));
        for ((table, id, there), (kind, other, changes)) in found.into_iter().take(LIST_CAP) {
            let side = if there { &p.there } else { &p.here };
            let sp = spot(db, &s, side.schema, &table, id, 2)?;
            let key = if there {
                p.key_of(&table, None, Some(id))
            } else {
                p.key_of(&table, Some(id), other)
            };
            elements.push(ElementDiff {
                fields: changes.iter().map(|c| c.field.clone()).collect(),
                table,
                id,
                kind,
                key,
                other,
                changes,
                spot: sp,
            });
        }
        Ok(VersionDiff {
            elements,
            total,
            tables: vec![],
        })
    })();
    let _ = db.execute("DETACH DATABASE their", []);
    let mut d = res?;
    d.tables = tables_diff;
    Ok(d)
}

/// Comparison on a separate connection to the model file (for a background thread).
pub fn compare_file(model_path: &str, copy: &Path) -> Result<VersionDiff, Error> {
    let db = Connection::open(model_path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    diff_on(&db, copy)
}

impl Model {
    /// Differences from the other version; `copy` is the result of [`prepare_compare`].
    pub fn compare_version(&self, copy: &Path) -> Result<VersionDiff, Error> {
        diff_on(&self.db, copy)
    }

    /// Hash of the model data (excluding derived tables and the journal): rows in rowid order, rowid included.
    /// `meta` counts by key (saved filters and other user keys); view state, the project
    /// identity and import bookkeeping there are left out ([`content_meta_key`]).
    /// Equal hashes mean byte-for-byte equal content.
    pub fn content_hash(&self) -> Result<u64, Error> {
        let s = tables::load(&self.db, &[])?;
        let names = s
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .chain(s.children.iter().map(|c| c.spec.table))
            .chain(["merge_peer", "merge_map", "view_phase", "view_option", "calc_settings"]);
        let mut h = crate::source::fnv(b"");
        for n in names {
            let mut st = self.db.prepare(&format!("SELECT rowid, * FROM {} ORDER BY rowid", q(n)))?;
            let width = st.column_count();
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                let mut text = format!("{h}|{n}");
                for i in 0..width {
                    let v: rusqlite::types::Value = r.get(i)?;
                    text += &format!("|{v:?}");
                }
                h = crate::source::fnv(text.as_bytes());
            }
        }
        // by key, not rowid: an INSERT OR REPLACE of a view key moves the rowids of the rest
        for (k, v) in crate::changes::user_meta(&self.db, "main")? {
            if content_meta_key(&k) {
                h = crate::source::fnv(format!("{h}|meta|{k}|{v:?}").as_bytes());
            }
        }
        Ok(h)
    }
}

/// A `meta` key whose value is project content. Not content: view state (`view:`, `pref:`,
/// the current phase, the 3D view style), the random project identity, the schema version and
/// the import bookkeeping of sources (`src_shared_*`, like the `src_*` tables).
pub(crate) fn content_meta_key(k: &str) -> bool {
    let state = matches!(k, "phase_current" | "project_uid" | "schema_version" | "view3d.style");
    !state && !k.starts_with("view:") && !k.starts_with("pref:") && !k.starts_with("src_shared_")
}

#[cfg(test)]
mod tests;
