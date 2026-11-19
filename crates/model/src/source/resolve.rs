// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Resolving re-import conflicts (one model operation, one undo) and deleting
//! an element with its cascade (wall -> openings, components, constraints; slab -> outline).
//! All row snapshots go through [`Snapshots`]: a row is snapshotted once before its first change,
//! so undoing several decisions on one row restores its original state.

use rusqlite::types::Value as Sql;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as J;

use super::{
    conflict_row, current_fields, dec, fields_text, ident, load_base, parse_rings, ConflictCase,
    Shapes, CONFLICT_COLS,
};
use crate::ops::Inv;
use crate::{Error, Model, Snapshots};

/// The user's choice for a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Keep the local one.
    Mine,
    /// Take from the source.
    Theirs,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolveStats {
    pub kept_mine: usize,
    pub took_theirs: usize,
    /// Elements deleted by the "take theirs" choice (deleted in the source).
    pub deleted: usize,
    /// Restored from the base (deleted here, changed there).
    pub restored: usize,
    /// Will be applied at the next import (a mesh body with no copy in the conflict, an element without geometry).
    pub deferred: usize,
    /// Detached from the source (edited here, deleted there, "keep mine").
    pub detached: usize,
    /// Fields the choice could not write because the parameter value is locked:
    /// (kind, element id, field). Not counted in `took_theirs`.
    pub refused: Vec<(String, i64, String)>,
}

/// Railings of a stair removed by "take from file" stay as sketch railings at their absolute
/// position unless the file already dropped them: a railing still in the file, one edited here
/// with its own open conflict, or one kept or added here must not vanish with the stair.
fn detach_in_file_railings(
    db: &Connection,
    snaps: &mut Snapshots,
    source: &str,
    stair: i64,
) -> Result<(), Error> {
    let rails: Vec<i64> = db
        .prepare_cached("SELECT id FROM railing WHERE stair=?1")?
        .query_map([stair], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for r in rails {
        let gone_from_file: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM ext_ref e WHERE e.source=?1 AND e.kind='railing' \
             AND e.ref=?2 AND NOT EXISTS(SELECT 1 FROM src_base b \
             WHERE b.source=e.source AND b.guid=e.guid))",
            params![source, r],
            |x| x.get(0),
        )?;
        if !gone_from_file {
            snaps.save(db, "railing", "id", r)?;
            snaps.save(db, "railing_vertex", "railing", r)?;
            crate::stairs::detach_railing(db, r)?;
        }
    }
    Ok(())
}

/// Deletes element `kind`/`id` with its dependent rows (snapshots go to `snaps`).
pub(crate) fn drop_element(
    db: &Connection,
    snaps: &mut Snapshots,
    kind: &str,
    id: i64,
) -> Result<(), Error> {
    let table = kind_table(kind)?;
    snaps.save(db, table, "id", id)?;
    snaps.save(db, "prop", "ref", id)?;
    let hosted = |db: &Connection, snaps: &mut Snapshots, col: &str| -> Result<(), Error> {
        let comps: Vec<i64> = db
            .prepare_cached(&format!("SELECT id FROM comp WHERE {col}=?1"))?
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for c in comps {
            snaps.save(db, "comp", "id", c)?;
        }
        db.execute(&format!("DELETE FROM comp WHERE {col}=?1"), [id])?;
        Ok(())
    };
    match kind {
        "wall" => {
            let ops: Vec<i64> = db
                .prepare_cached("SELECT id FROM opening WHERE wall=?1")?
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for o in &ops {
                snaps.save(db, "opening", "id", *o)?;
                snaps.save(db, "prop", "ref", *o)?;
                db.execute("DELETE FROM prop WHERE kind='opening' AND ref=?1", [o])?;
            }
            db.execute("DELETE FROM opening WHERE wall=?1", [id])?;
            hosted(db, snaps, "host_wall")?;
            for (cid, s) in crate::constraints::load(db)? {
                if s.walls.contains(&id) {
                    snaps.save(db, "constraint_rule", "id", cid)?;
                    db.execute("DELETE FROM constraint_rule WHERE id=?1", [cid])?;
                }
            }
        }
        "slab" => {
            snaps.save(db, "slab_vertex", "slab", id)?;
            db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
            hosted(db, snaps, "host_slab")?;
        }
        "ceiling" => {
            snaps.save(db, "ceiling_vertex", "ceiling", id)?;
            db.execute("DELETE FROM ceiling_vertex WHERE ceiling=?1", [id])?;
            hosted(db, snaps, "host_ceiling")?;
        }
        "solid" => {
            snaps.save(db, "solid_host", "solid", id)?;
            db.execute("DELETE FROM solid_host WHERE solid=?1", [id])?;
        }
        "curtain_wall" => {
            snaps.save(db, "curtain_panel", "cw", id)?;
            db.execute("DELETE FROM curtain_panel WHERE cw=?1", [id])?;
        }
        "stair" => {
            snaps.save(db, "stair_qty", "stair", id)?;
            db.execute("DELETE FROM stair_qty WHERE stair=?1", [id])?;
            let rails: Vec<i64> = db
                .prepare_cached("SELECT id FROM railing WHERE stair=?1")?
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for r in rails {
                drop_element(db, snaps, "railing", r)?;
            }
        }
        "railing" => {
            snaps.save(db, "railing_vertex", "railing", id)?;
            db.execute("DELETE FROM railing_vertex WHERE railing=?1", [id])?;
            snaps.save(db, "railing_qty", "railing", id)?;
            db.execute("DELETE FROM railing_qty WHERE railing=?1", [id])?;
        }
        _ => {}
    }
    db.execute(&format!("DELETE FROM {table} WHERE id=?1"), [id])?;
    db.execute(
        "DELETE FROM prop WHERE kind=?1 AND ref=?2",
        params![kind, id],
    )?;
    Ok(())
}

fn kind_table(kind: &str) -> Result<&str, Error> {
    if super::DELETABLE.contains(&kind) {
        Ok(kind)
    } else {
        Err(Error::Invalid(format!("cannot delete imported {kind}")))
    }
}

/// Deletes the reference, base and other guid conflicts (the element becomes local or is forgotten).
fn unlink(db: &Connection, snaps: &mut Snapshots, source: &str, guid: &str) -> Result<(), Error> {
    let base: Option<i64> = db
        .query_row(
            "SELECT id FROM src_base WHERE source=?1 AND guid=?2",
            params![source, guid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(b) = base {
        snaps.save(db, "src_base", "id", b)?;
        db.execute("DELETE FROM src_base WHERE id=?1", [b])?;
    }
    let link: Option<i64> = db
        .query_row(
            "SELECT rowid FROM ext_ref WHERE source=?1 AND guid=?2",
            params![source, guid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(r) = link {
        snaps.save(db, "ext_ref", "rowid", r)?;
        db.execute("DELETE FROM ext_ref WHERE rowid=?1", [r])?;
    }
    let cs: Vec<i64> = db
        .prepare_cached("SELECT id FROM src_conflict WHERE source=?1 AND guid=?2")?
        .query_map(params![source, guid], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for c in cs {
        snaps.save(db, "src_conflict", "id", c)?;
        db.execute("DELETE FROM src_conflict WHERE id=?1", [c])?;
    }
    Ok(())
}

/// A free unique name for a lookup row (`name`, `name (2)`, ...), excluding the row itself.
fn free_name(db: &Connection, table: &str, id: i64, name: &str) -> Result<String, Error> {
    let sql = format!("SELECT count(*) FROM {table} WHERE name=?1 AND id<>?2");
    crate::free_name(name, |k| format!("{name} ({k})"), |cand| {
        Ok::<_, Error>(db.query_row(&sql, params![cand, id], |r| r.get::<_, i64>(0))? > 0)
    })
}

/// True: the field is a parameter value that is locked (the choice must not write it).
fn field_locked(db: &Connection, kind: &str, id: i64, field: &str) -> Result<bool, Error> {
    let Some(name) = field.strip_prefix(super::UPARAM_FIELD) else {
        return Ok(false);
    };
    match super::uparam_slot(db, ident(kind)?, id, name)? {
        Some(slot) => super::uparam_locked(db, &slot),
        None => Ok(false),
    }
}

/// Writes a field value into a row (a column, a parameter or slab outlines). false means the value
/// is not applicable now (a mesh body known only by hash).
fn write_field(
    db: &Connection,
    snaps: &mut Snapshots,
    kind: &str,
    id: i64,
    field: &str,
    v: &J,
) -> Result<bool, Error> {
    let kind = ident(kind)?;
    if let Some(name) = field.strip_prefix(super::UPARAM_FIELD) {
        if let Some(slot) = super::uparam_slot(db, kind, id, name)? {
            return super::write_uparam(db, snaps, &slot, &dec(v).unwrap_or(Sql::Null));
        }
    }
    if let Some(key) = field.strip_prefix("p:") {
        snaps.save(db, "prop", "ref", id)?;
        match dec(v) {
            Some(Sql::Null) | None => {
                db.execute(
                    "DELETE FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                    params![kind, id, key],
                )?;
            }
            Some(val) => {
                let unit: String = db
                    .query_row(
                        "SELECT unit FROM prop WHERE kind=?1 AND ref=?2 AND key=?3",
                        params![kind, id, key],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or_default();
                db.execute(
                    "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
                    params![kind, id, key, val, unit],
                )?;
            }
        }
        return Ok(true);
    }
    if field == "kids" {
        // child rows are known only by hash
        return Ok(false);
    }
    if field == "rings" {
        let rings = v
            .as_str()
            .and_then(parse_rings)
            .ok_or_else(|| Error::Invalid("bad slab outline in base".into()))?;
        snaps.save(db, "slab_vertex", "slab", id)?;
        db.execute("DELETE FROM slab_vertex WHERE slab=?1", [id])?;
        insert_rings(db, id, &rings)?;
        return Ok(true);
    }
    let col = ident(field)?;
    let Some(mut val) = dec(v) else {
        return Ok(false);
    };
    // the name chosen at import may have been taken by another row since then (UNIQUE)
    if col == "name" {
        if let Sql::Text(n) = &val {
            let unique = matches!(
                kind,
                "level" | "wall_type" | "slab_type" | "member_type" | "comp_type"
            );
            if unique {
                val = Sql::Text(free_name(db, kind, id, n)?);
            }
        }
    }
    snaps.save(db, kind, "id", id)?;
    db.execute(
        &format!("UPDATE {kind} SET {col}=?2 WHERE id=?1"),
        params![id, val],
    )?;
    Ok(true)
}

/// "Take from file" is deferred: the base field := the current value, so the next import takes
/// the file's value (here "unchanged").
fn rebase_field(
    db: &Connection,
    snaps: &mut Snapshots,
    source: &str,
    guid: &str,
    kind: &str,
    id: i64,
    field: &str,
) -> Result<(), Error> {
    let Some(mut base) = load_base(db, &mut Shapes::new(), source, guid)? else {
        return Ok(());
    };
    let key = vec![field.to_string()];
    let now = current_fields(db, kind, id, &key)?
        .and_then(|m| m.get(field).cloned())
        .unwrap_or(J::Null);
    base.vals.insert(field.to_string(), now);
    snaps.save(db, "src_base", "id", base.row)?;
    db.execute(
        "UPDATE src_base SET vals=?2 WHERE id=?1",
        params![base.row, fields_text(&base.vals)],
    )?;
    Ok(())
}

fn insert_rings(db: &Connection, slab: i64, rings: &[Vec<(f64, f64)>]) -> Result<(), Error> {
    let mut st =
        db.prepare_cached("INSERT INTO slab_vertex(slab,ring,idx,x,y) VALUES (?1,?2,?3,?4,?5)")?;
    for (ri, r) in rings.iter().enumerate() {
        for (i, p) in r.iter().enumerate() {
            st.execute(params![slab, ri as i64, i as i64, p.0, p.1])?;
        }
    }
    Ok(())
}

/// Recreates a row from the base (columns, outlines); the base stores parameters as hashes, so the
/// next import brings them back (base parameters are reset). None means the base has no geometry.
fn recreate(
    db: &Connection,
    snaps: &mut Snapshots,
    pre: &mut Inv,
    source: &str,
    guid: &str,
    kind: &str,
) -> Result<Option<i64>, Error> {
    if !matches!(kind, "wall" | "opening" | "slab" | "room" | "grid") {
        return Ok(None);
    }
    let Some(base) = load_base(db, &mut Shapes::new(), source, guid)? else {
        return Ok(None);
    };
    let mut cols = vec![];
    let mut vals = vec![];
    for (k, v) in &base.vals {
        if k.starts_with("p:") || k.starts_with('~') || k == "rings" {
            continue;
        }
        let Some(val) = dec(v) else { return Ok(None) };
        cols.push(ident(k)?.to_string());
        vals.push(val);
    }
    if cols.is_empty() {
        return Ok(None);
    }
    if kind == "opening" {
        let host = base.vals.get("wall").and_then(J::as_i64).unwrap_or(0);
        let n: i64 = db.query_row("SELECT count(*) FROM wall WHERE id=?1", [host], |r| {
            r.get(0)
        })?;
        if n == 0 {
            return Ok(None);
        }
    }
    let ph: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    db.execute(
        &format!(
            "INSERT INTO {kind}({}) VALUES ({})",
            cols.join(","),
            ph.join(",")
        ),
        rusqlite::params_from_iter(vals),
    )?;
    let id = db.last_insert_rowid();
    let mut inv = std::mem::take(pre).delete(kind, "id", id);
    if let Some(r) = base
        .vals
        .get("rings")
        .and_then(J::as_str)
        .and_then(parse_rings)
    {
        insert_rings(db, id, &r)?;
        inv = inv.delete("slab_vertex", "slab", id);
    }
    *pre = inv;
    // the new row has no parameters: a base without them means the next import takes them
    let kept: std::collections::BTreeMap<String, J> = base
        .vals
        .iter()
        .filter(|(k, _)| !k.starts_with("p:"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    snaps.save(db, "src_base", "id", base.row)?;
    db.execute(
        "UPDATE src_base SET shape=0, hashes=x'', vals=?2 WHERE id=?1",
        params![base.row, fields_text(&kept)],
    )?;
    Ok(Some(id))
}

impl Model {
    /// Applies the choices on conflicts in one operation (undo restores everything, including the conflicts).
    pub fn resolve_conflicts(&mut self, picks: &[(i64, Pick)]) -> Result<ResolveStats, Error> {
        if picks.is_empty() {
            return Ok(ResolveStats::default());
        }
        // "wall...": regenerate derived tables (walls/slabs may have changed)
        self.op("wall_src_resolve", |db| {
            let mut st = ResolveStats::default();
            let mut snaps = Snapshots::new();
            // deleting rows created by "restore from file": before the snapshots
            let mut pre = Inv::default();
            for &(cid, pick) in picks {
                let c = db
                    .query_row(
                        &format!("SELECT {CONFLICT_COLS} FROM src_conflict WHERE id=?1"),
                        [cid],
                        conflict_row,
                    )
                    .optional()?;
                let Some(c) = c else { continue };
                // a locked value is not written; the conflict stays open for after the unlock
                let writes = pick == Pick::Theirs && !(c.case == ConflictCase::EditDelete && c.field == "*");
                if writes
                    && matches!(c.case, ConflictCase::EditEdit | ConflictCase::EditDelete)
                    && field_locked(db, &c.kind, c.element, &c.field)?
                {
                    st.refused.push((c.kind.clone(), c.element, c.field.clone()));
                    continue;
                }
                snaps.save(db, "src_conflict", "id", cid)?;
                db.execute("DELETE FROM src_conflict WHERE id=?1", [cid])?;
                let alive =
                    current_fields(db, &c.kind, c.element, &Vec::<String>::new())?.is_some();
                match (c.case, pick) {
                    (_, Pick::Mine) => {
                        st.kept_mine += 1;
                        if c.case == ConflictCase::EditDelete && c.field == "*" {
                            unlink(db, &mut snaps, &c.source, &c.guid)?;
                            st.detached += 1;
                        }
                    }
                    (ConflictCase::EditEdit, Pick::Theirs) => {
                        if alive
                            && !write_field(
                                db, &mut snaps, &c.kind, c.element, &c.field, &c.theirs,
                            )?
                        {
                            rebase_field(
                                db, &mut snaps, &c.source, &c.guid, &c.kind, c.element, &c.field,
                            )?;
                            st.deferred += 1;
                        }
                        st.took_theirs += 1;
                    }
                    (ConflictCase::EditDelete, Pick::Theirs) => {
                        if c.field == "*" {
                            if alive {
                                if c.kind == "stair" {
                                    detach_in_file_railings(db, &mut snaps, &c.source, c.element)?;
                                }
                                drop_element(db, &mut snaps, &c.kind, c.element)?;
                            }
                            unlink(db, &mut snaps, &c.source, &c.guid)?;
                            st.deleted += 1;
                        } else if alive {
                            write_field(db, &mut snaps, &c.kind, c.element, &c.field, &J::Null)?;
                        }
                        st.took_theirs += 1;
                    }
                    (ConflictCase::DeleteEdit, Pick::Theirs) => {
                        st.took_theirs += 1;
                        if alive {
                            continue;
                        }
                        match recreate(db, &mut snaps, &mut pre, &c.source, &c.guid, &c.kind)? {
                            Some(id) => {
                                let row: i64 = db.query_row(
                                    "SELECT rowid FROM ext_ref WHERE source=?1 AND guid=?2",
                                    params![c.source, c.guid],
                                    |r| r.get(0),
                                )?;
                                snaps.save(db, "ext_ref", "rowid", row)?;
                                db.execute(
                                    "UPDATE ext_ref SET ref=?2 WHERE rowid=?1",
                                    params![row, id],
                                )?;
                                st.restored += 1;
                            }
                            None => {
                                // the next import will create the element again
                                unlink(db, &mut snaps, &c.source, &c.guid)?;
                                st.deferred += 1;
                            }
                        }
                    }
                }
            }
            Ok((st, 0, pre.then(snaps.into_inv())))
        })
    }

    /// Detaches an element from the source: edits are no longer tracked, and re-import leaves it
    /// alone (the guid will arrive as a new element).
    pub fn detach_from_source(&mut self, kind: &str, id: i64) -> Result<(), Error> {
        let link: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT source, guid FROM ext_ref WHERE kind=?1 AND ref=?2",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((source, guid)) = link else {
            return Err(Error::Invalid(format!("{kind} {id} is not imported")));
        };
        self.op("src_detach", |db| {
            let mut snaps = Snapshots::new();
            unlink(db, &mut snaps, &source, &guid)?;
            Ok(((), id, snaps.into_inv()))
        })
    }
}
