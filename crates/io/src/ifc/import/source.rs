//! IFC as a source: a re-import is a 3-way merge, not an overwrite.
//!
//! Every linked element is compared field by field (columns, slab outline, properties) with what
//! the previous import said (`src_base`, kept by `ddc_model::source::Merge`). A field edited here
//! and unchanged in the file stays; a field changed only in the file is taken; both changed is a
//! conflict kept for the dialog. Rows deleted here stay deleted unless the file changed them.
//! The file itself (path, content hash, stat) is recorded in `src_file` (schema 39).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use ddc_model::source::file::{
    find_source, hash_bytes, note_shared_base, stamp_db, stat_of, FileStamp,
};
use ddc_model::source::{self as src, Decision, Merge, SourceWrite};
use ddc_model::{Error, Snapshots};
use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as J;

use super::Plan;

/// The file a plan was read from, as it was when read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfcFile {
    pub path: PathBuf,
    pub stamp: FileStamp,
}

impl IfcFile {
    /// Hash and stat of `path` (a streamed pass over the file; for a worker thread).
    pub fn read(path: &Path, stop: &dyn Fn() -> bool) -> Option<Self> {
        let stamp = FileStamp::of_with(path, stop).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            stamp,
        })
    }

    /// For bytes already in memory; `stat` was taken before the read.
    pub fn from_bytes(
        path: &Path,
        bytes: &[u8],
        stat: Option<ddc_model::source::file::Stat>,
    ) -> Option<Self> {
        let stat = stat.or_else(|| stat_of(path).ok())?;
        Some(Self {
            path: path.to_path_buf(),
            stamp: FileStamp {
                hash: hash_bytes(bytes),
                stat,
            },
        })
    }
}

/// A linked row as it is now: its columns (without `id`), the slab outline text, a hash of its
/// child rows (panels, vertices) and its properties (key, value, unit).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct RowState {
    pub cols: Vec<(String, Sql)>,
    pub rings: Option<String>,
    pub kids: Option<String>,
    pub props: Vec<(String, Sql, String)>,
}

impl RowState {
    fn col(&self, name: &str) -> Option<&Sql> {
        self.cols.iter().find(|(c, _)| c == name).map(|(_, v)| v)
    }
}

/// Table names come from the importer's own kinds, never from file text.
pub(super) fn read_row(db: &Connection, table: &str, id: i64) -> Result<Option<RowState>, Error> {
    let has_id: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info(?1) WHERE name='id'",
        [table],
        |r| r.get(0),
    )?;
    if has_id == 0 {
        return Ok(None);
    }
    let mut st = db.prepare_cached(&format!("SELECT * FROM {table} WHERE id=?1"))?;
    let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
    let vals: Option<Vec<Sql>> = st
        .query_row([id], |r| {
            (0..names.len()).map(|i| r.get::<_, Sql>(i)).collect()
        })
        .optional()?;
    let Some(vals) = vals else { return Ok(None) };
    let cols = names
        .into_iter()
        .zip(vals)
        .filter(|(n, _)| n != "id")
        .collect();
    let rings = if table == "slab" {
        Some(slab_rings(db, id)?)
    } else {
        None
    };
    let mut ps = db.prepare_cached(
        "SELECT key, value, unit FROM prop WHERE kind=?1 AND ref=?2 ORDER BY key",
    )?;
    let props = ps
        .query_map(params![table, id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let kids = src::kids_text(db, table, id)?;
    Ok(Some(RowState {
        cols,
        rings,
        kids,
        props,
    }))
}

fn slab_rings(db: &Connection, id: i64) -> Result<String, Error> {
    let mut st =
        db.prepare_cached("SELECT ring, x, y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
    let rows = st.query_map([id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut rings: Vec<Vec<(f64, f64)>> = vec![];
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
    Ok(src::rings_text(&rings))
}

/// Source fields of a row as the file says them (new rows and rows deleted here).
fn theirs_fields(now: &RowState, props: &[(String, Sql, String)]) -> Vec<(String, J)> {
    let mut out: Vec<(String, J)> = now
        .cols
        .iter()
        .map(|(c, v)| (c.clone(), src::enc(v)))
        .collect();
    if let Some(r) = &now.rings {
        out.push(("rings".into(), J::String(r.clone())));
    }
    if let Some(k) = &now.kids {
        out.push(("kids".into(), J::String(k.clone())));
    }
    out.extend(
        props
            .iter()
            .map(|(k, v, _)| (format!("p:{k}"), src::enc(v))),
    );
    out
}

/// What the caller restores after [`merge_row`]: child rows (outline, panels, vertices) whose
/// own field (`rings`, `kids`) the merge kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Applied {
    pub restore_rings: bool,
    pub restore_children: bool,
}

/// The importer has just rewritten row `id` with the file's values; decides every field against
/// the last import's base and puts the local value back where the merge keeps it. Properties
/// were not touched by the importer: only the taken ones are written here.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge_row(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    id: i64,
    mine: &RowState,
    theirs: &[(String, Sql, String)],
) -> Result<Applied, Error> {
    let Some(now) = read_row(db, kind, id)? else {
        return Ok(Applied::default());
    };
    let mut names: Vec<String> = vec![];
    let mut pairs: Vec<(J, J)> = vec![];
    for (c, v) in &now.cols {
        names.push(c.clone());
        pairs.push((mine.col(c).map_or(J::Null, src::enc), src::enc(v)));
    }
    if let (Some(m), Some(n)) = (&mine.rings, &now.rings) {
        names.push("rings".into());
        pairs.push((J::String(m.clone()), J::String(n.clone())));
    }
    if let (Some(m), Some(n)) = (&mine.kids, &now.kids) {
        names.push("kids".into());
        pairs.push((J::String(m.clone()), J::String(n.clone())));
    }
    let keys: BTreeSet<&str> = mine
        .props
        .iter()
        .map(|p| p.0.as_str())
        .chain(theirs.iter().map(|p| p.0.as_str()))
        .collect();
    for k in &keys {
        let m = mine
            .props
            .iter()
            .find(|p| p.0 == *k)
            .map_or(J::Null, |p| src::enc(&p.1));
        let t = theirs
            .iter()
            .find(|p| p.0 == *k)
            .map_or(J::Null, |p| src::enc(&p.1));
        names.push(format!("p:{k}"));
        pairs.push((m, t));
    }
    let fields: Vec<(&str, J, J)> = names
        .iter()
        .zip(pairs)
        .map(|(n, (m, t))| (n.as_str(), m, t))
        .collect();
    let dec = merge.row(db, snaps, guid, kind, id, &fields)?;

    let mut ap = Applied::default();
    for (i, d) in dec.iter().enumerate() {
        let name = names[i].as_str();
        let kept = matches!(d, Decision::Keep | Decision::Conflict);
        if let Some(key) = name.strip_prefix("p:") {
            if *d == Decision::Take {
                match theirs.iter().find(|p| p.0 == key) {
                    Some((k, v, u)) => {
                        db.prepare_cached(
                            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                        )?
                        .execute(params![kind, id, k, v, u])?;
                    }
                    None => {
                        db.prepare_cached("DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3")?
                            .execute(params![kind, id, key])?;
                    }
                }
            }
        } else if name == "rings" {
            ap.restore_rings |= kept;
        } else if name == "kids" {
            // child rows merge as one field: the file's stay unless they were edited here
            ap.restore_children |= kept;
        } else if kept {
            if let Some(v) = mine.col(name) {
                db.prepare_cached(&format!("UPDATE {kind} SET {name}=?2 WHERE id=?1"))?
                    .execute(params![id, v])?;
            }
            if *d == Decision::Conflict {
                if let Some(Sql::Blob(b)) = now.col(name) {
                    merge.conflict_theirs(db, guid, name, &src::blob_json(b))?;
                }
            }
        }
    }
    // a railing re-hosted by the file (stair -> sketch or back) keeps the file's vertices:
    // restoring ours would leave a sketch railing without a path
    if kind == "railing" {
        let i = names.iter().position(|n| n == "stair");
        if i.is_some_and(|i| dec[i] == Decision::Take && mine.col("stair") != now.col("stair")) {
            ap.restore_children = false;
        }
    }
    Ok(ap)
}

/// A railing the file made a sketch railing because it dropped its stair stays on that stair
/// here (the file's values are its base): the stair's removal, by this import or by "take from
/// file" on its conflict, detaches it in that same undo step.
pub(super) fn keep_host_of_dropped_stair(
    db: &Connection,
    source: &str,
    seen: &HashSet<String>,
    id: i64,
    mine: &RowState,
    ap: &mut Applied,
) -> Result<(), Error> {
    let Some(Sql::Integer(stair)) = mine.col("stair") else {
        return Ok(());
    };
    let now: Option<i64> =
        db.query_row("SELECT stair FROM railing WHERE id=?1", [id], |r| r.get(0))?;
    if now.is_some() {
        return Ok(());
    }
    let guid: Option<String> = db
        .prepare_cached("SELECT guid FROM ext_ref WHERE source=?1 AND kind='stair' AND ref=?2")?
        .query_row(params![source, stair], |r| r.get(0))
        .optional()?;
    let alive: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM stair WHERE id=?1)", [stair], |r| r.get(0))?;
    // a stair still in the file, or not from it: the file's re-hosting is taken
    if !alive || guid.is_none_or(|g| seen.contains(&g)) {
        return Ok(());
    }
    db.execute(
        "UPDATE railing SET stair=?2, side=?3 WHERE id=?1",
        params![id, stair, mine.col("side")],
    )?;
    ap.restore_children = true;
    Ok(())
}

/// Properties only of a row made here from the imported element (mesh converted to a wall):
/// geometry stays as edited, each file property is merged with the last import like
/// [`merge_row`] does (local edit kept, file change taken, deletion in the file applied).
/// Markers (`ddc.*`) held only here are left out of the merge.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge_props(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    id: i64,
    theirs: &[(String, Sql, String)],
) -> Result<(), Error> {
    let Some(now) = read_row(db, kind, id)? else {
        return Ok(());
    };
    fn value(ps: &[(String, Sql, String)], k: &str) -> J {
        ps.iter().find(|p| p.0 == k).map_or(J::Null, |p| src::enc(&p.1))
    }
    let keys: Vec<&str> = now
        .props
        .iter()
        .map(|p| p.0.as_str())
        .filter(|k| !k.starts_with("ddc."))
        .chain(theirs.iter().map(|p| p.0.as_str()))
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .collect();
    let names: Vec<String> = keys.iter().map(|k| format!("p:{k}")).collect();
    let fields: Vec<(&str, J, J)> = names
        .iter()
        .zip(&keys)
        .map(|(n, k)| (n.as_str(), value(&now.props, k), value(theirs, k)))
        .collect();
    let dec = merge.props_row(db, snaps, guid, kind, id, &fields)?;
    for (key, d) in keys.iter().zip(dec) {
        if d != Decision::Take {
            continue;
        }
        match theirs.iter().find(|p| p.0 == *key) {
            Some((k, v, u)) => {
                db.prepare_cached(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                )?
                .execute(params![kind, id, k, v, u])?;
            }
            // only a key the last import wrote: without a base, keys added here stay
            None if merge.base_value(db, guid, &format!("p:{key}"))?.is_some() => {
                db.prepare_cached("DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3")?
                    .execute(params![kind, id, key])?;
            }
            None => {}
        }
    }
    Ok(())
}

/// A row this import inserted: its base is what the file said.
pub(super) fn merge_added(
    db: &Connection,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    id: i64,
    theirs: &[(String, Sql, String)],
) -> Result<(), Error> {
    let Some(now) = read_row(db, kind, id)? else {
        return Ok(());
    };
    let f = theirs_fields(&now, theirs);
    let fields: Vec<(&str, J)> = f.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    merge.added(guid, kind, id, &fields);
    Ok(())
}

/// The guid is linked to a row deleted here and the importer has inserted the file's version as
/// `id`. True: the deletion stands (nothing the file changed), the caller drops the new row.
pub(super) fn merge_dead(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    dead_id: i64,
    id: i64,
    theirs: &[(String, Sql, String)],
) -> Result<bool, Error> {
    let Some(now) = read_row(db, kind, id)? else {
        return Ok(false);
    };
    let f = theirs_fields(&now, theirs);
    let fields: Vec<(&str, J)> = f.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    Ok(merge.dead(db, snaps, guid, kind, dead_id, &fields)? == src::Dead::Stay)
}

/// Source namespace: the project GlobalId; without one, an earlier import of the same file by
/// key or content.
pub(super) fn namespace(db: &Connection, pl: &Plan) -> Result<String, Error> {
    if !pl.project_guid.is_empty() {
        return Ok(format!("ifc:{}", pl.project_guid));
    }
    if let Some(f) = &pl.file {
        let key = crate::bridge::file_identity(&f.path);
        if let Some(s) = find_source(db, &key, &f.stamp.hash)? {
            return Ok(s);
        }
    }
    Ok("ifc".to_string())
}

/// End of the import: provenance row, new base, removed elements, conflicts that went stale,
/// then the file's hash and stat. Returns the number of rows the merge deleted.
pub(super) fn finish(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    seen: &HashSet<String>,
    pl: &Plan,
) -> Result<usize, Error> {
    let source = merge.source().to_string();
    let prev: Option<(String, String)> = db
        .query_row(
            "SELECT file_key, path FROM src_file WHERE source=?1",
            [&source],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (file_key, path) = match (&pl.file, prev) {
        (Some(f), _) => (
            crate::bridge::file_identity(&f.path),
            f.path.to_string_lossy().to_string(),
        ),
        // from memory: the earlier file stays the source's file
        (None, Some(p)) => p,
        (None, None) => (source.clone(), String::new()),
    };
    let r = &pl.report;
    let report: BTreeMap<String, i64> = [
        ("levels", r.levels),
        ("walls", r.walls),
        ("openings", r.openings),
        ("slabs", r.slabs),
        ("rooms", r.rooms),
        ("columns", r.columns),
        ("beams", r.beams),
        ("roofs", r.roofs),
        ("meshes", r.meshes),
        ("curtain_walls", r.curtain_walls),
        ("properties", r.properties),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v as i64))
    .collect();
    let mut info = BTreeMap::new();
    info.insert("schema".to_string(), r.schema.clone());
    if !pl.project_guid.is_empty() {
        info.insert("project_guid".to_string(), pl.project_guid.clone());
    }
    let w = SourceWrite {
        file_key,
        path,
        format: "ifc".into(),
        reader: "ddc-ifc".into(),
        reader_version: env!("CARGO_PKG_VERSION").into(),
        info,
        report,
        at: 0,
    };
    let deleted = merge.finish(db, snaps, seen, &w)?;
    if let Some(f) = &pl.file {
        stamp_db(db, &source, &f.stamp, None)?;
    }
    note_shared_base(db, &source)?;
    Ok(deleted.len())
}
