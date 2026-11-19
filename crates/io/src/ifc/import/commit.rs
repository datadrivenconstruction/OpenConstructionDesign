//! Plan → model rows. Elements with a GlobalId are linked in `ext_ref` (source = the project
//! GlobalId); a re-import of the same project updates linked rows in place (undoable through
//! snapshots) instead of adding duplicates.

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_model::source::{Merge, Policy};
use ddc_model::{Error, Snapshots};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension};

use super::circuits;
use super::mep_links::{self, StoredPorts};
use super::source::{self as isrc, RowState};
use super::{unique, ImportReport, MemberExact, PCompHost, PProp, PVal, Plan};

/// Outcome of linking elements to existing rows.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    pub updated: usize,
    pub unchanged: usize,
    pub missing: usize,
    pub properties: usize,
    pub warnings: Vec<String>,
    /// Re-import merge: fields kept because they were edited here / in conflict with the file /
    /// rows removed because the file no longer has them.
    pub kept_local: usize,
    pub conflicts: usize,
    pub removed: usize,
    /// Locked parameter values the file would change, kept: (table, id, parameter).
    pub locked: Vec<(String, i64, String)>,
}

impl Outcome {
    /// Copies the link and merge outcome into the report.
    pub fn fill(self, rep: &mut ImportReport) {
        rep.updated = self.updated;
        rep.unchanged = self.unchanged;
        rep.missing = self.missing;
        rep.properties = self.properties;
        rep.kept_local = self.kept_local;
        rep.conflicts = self.conflicts;
        rep.removed = self.removed;
        rep.locked = self.locked;
        rep.warnings.extend(self.warnings);
    }
}

/// Child tables (table, parent column) dumped with a row.
fn children(table: &str) -> &'static [(&'static str, &'static str)] {
    match table {
        "slab" => &[("slab_vertex", "slab")],
        "curtain_wall" => &[("curtain_panel", "cw")],
        "roof" => &[("roof_vertex", "roof")],
        "ceiling" => &[("ceiling_vertex", "ceiling")],
        "railing" => &[("railing_vertex", "railing")],
        _ => &[],
    }
}

fn dump(
    db: &Connection,
    table: &str,
    col: &str,
    id: i64,
    kind: &str,
) -> Result<Vec<Vec<SqlValue>>, Error> {
    let sql = if table == "prop" {
        "SELECT key, value, unit FROM prop WHERE kind=?2 AND ref=?1 ORDER BY key".to_string()
    } else {
        format!("SELECT * FROM {table} WHERE {col}=?1 ORDER BY rowid")
    };
    let mut st = db.prepare_cached(&sql)?;
    let n = st.column_count();
    let row = |r: &rusqlite::Row| {
        (0..n)
            .map(|i| r.get(i))
            .collect::<Result<Vec<SqlValue>, _>>()
    };
    let rows = if table == "prop" {
        st.query_map(params![id, kind], row)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        st.query_map([id], row)?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

struct Linker<'a> {
    db: &'a Connection,
    snaps: &'a mut Snapshots,
    source: String,
    /// (kind, plan index) → (guid, props)
    elems: HashMap<(&'static str, usize), (&'a str, &'a [PProp])>,
    /// guid → (kind, row) already linked for this source
    linked: HashMap<String, (String, i64)>,
    seen: HashSet<String>,
    sync: Outcome,
    /// (table, id, state before) of rows being rewritten
    pending: Option<(&'static str, i64, Vec<Vec<Vec<SqlValue>>>)>,
    /// 3-way merge with the last import of this source.
    merge: Merge,
    /// (guid, table, id, row as it was here) of the row being rewritten.
    mine: Option<(String, &'static str, i64, RowState)>,
    /// (guid, table, stale id): the guid is linked to a row deleted here.
    dead: Option<(String, &'static str, i64)>,
    /// Rows inserted for elements that stay deleted here; dropped at the end.
    zombies: Vec<(&'static str, i64)>,
    /// Counts written elements and names their category; a cancel stops the commit.
    ctl: Option<&'a crate::job::Progress>,
}

/// Catalog key naming an element category while it is written.
fn kind_key(kind: &str) -> &'static str {
    match kind {
        "wall" => "job.kind.wall",
        "opening" => "job.kind.opening",
        "slab" => "job.kind.slab",
        "roof" => "job.kind.roof",
        "ceiling" => "job.kind.ceiling",
        "stair" | "ramp" | "railing" => "job.kind.stair",
        "curtain_wall" => "job.kind.curtain_wall",
        "room" | "room_sep" => "job.kind.room",
        "grid" => "job.kind.grid",
        "mep_run" => "job.kind.mep",
        "comp" => "job.kind.comp",
        "mesh" => "job.kind.mesh",
        _ => "job.kind.other",
    }
}

impl<'a> Linker<'a> {
    fn tick(&self, kind: &str) -> Result<(), Error> {
        let Some(c) = self.ctl else { return Ok(()) };
        c.add(1);
        c.set_detail(kind_key(kind));
        if c.is_cancelled() {
            return Err(Error::Invalid(crate::job::CANCELLED.into()));
        }
        Ok(())
    }

    fn state(&self, table: &str, id: i64) -> Result<Vec<Vec<Vec<SqlValue>>>, Error> {
        let mut v = vec![dump(self.db, table, "id", id, "")?];
        for (t, c) in children(table) {
            v.push(dump(self.db, t, c, id, "")?);
        }
        v.push(dump(self.db, "prop", "ref", id, table)?);
        Ok(v)
    }

    /// Existing row for plan element (kind, index); snapshots it before the rewrite.
    fn existing(&mut self, kind: &'static str, index: usize) -> Result<Option<i64>, Error> {
        self.pending = None;
        self.mine = None;
        self.dead = None;
        let Some((guid, _)) = self.elems.get(&(kind, index)).copied() else {
            return Ok(None);
        };
        if guid.is_empty() {
            return Ok(None);
        }
        self.seen.insert(guid.to_string());
        let Some((k, id)) = self.linked.get(guid).cloned() else {
            return Ok(None);
        };
        if k != kind {
            return Ok(None);
        }
        let alive: Option<i64> = self
            .db
            .query_row(&format!("SELECT id FROM {kind} WHERE id=?1"), [id], |r| {
                r.get(0)
            })
            .optional()?;
        if alive.is_none() {
            self.dead = Some((guid.to_string(), kind, id));
            return Ok(None);
        }
        self.mine = isrc::read_row(self.db, kind, id)?.map(|s| (guid.to_string(), kind, id, s));
        let before = self.state(kind, id)?;
        self.snaps.save(self.db, kind, "id", id)?;
        for (t, c) in children(kind) {
            self.snaps.save(self.db, t, c, id)?;
        }
        self.snaps.save(self.db, "prop", "ref", id)?;
        self.pending = Some((kind, id, before));
        Ok(Some(id))
    }

    /// Links plan element (kind, index) to row `id` without data (levels, door/window fillers).
    fn link_only(&mut self, kind: &'static str, index: usize, id: i64) -> Result<(), Error> {
        let Some((guid, _)) = self.elems.get(&(kind, index)).copied() else {
            return Ok(());
        };
        if guid.is_empty() {
            return Ok(());
        }
        self.seen.insert(guid.to_string());
        self.db.execute(
            "INSERT OR REPLACE INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,?4)",
            params![self.source, guid, kind, id],
        )?;
        Ok(())
    }

    /// Row the user made from this element's imported mesh (a native element converted from
    /// it, or a merged mesh): marked by `ddc.converted_from` / `ddc.merged_from`. It stays as
    /// edited; only its properties are refreshed ([`Self::done_kept`]).
    fn kept(&mut self, kind: &'static str, index: usize) -> Result<Option<(String, i64)>, Error> {
        const KINDS: [&str; 7] = ["wall", "slab", "roof", "col", "beam", "solid", "mesh"];
        let Some((guid, _)) = self.elems.get(&(kind, index)).copied() else {
            return Ok(None);
        };
        if guid.is_empty() {
            return Ok(None);
        }
        let Some((k, id)) = self.linked.get(guid).cloned() else {
            return Ok(None);
        };
        if !KINDS.contains(&k.as_str()) {
            return Ok(None);
        }
        let table: i64 = self.db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [&k],
            |r| r.get(0),
        )?;
        if table == 0 {
            return Ok(None);
        }
        let alive: Option<i64> = self
            .db
            .query_row(&format!("SELECT id FROM {k} WHERE id=?1"), [id], |r| {
                r.get(0)
            })
            .optional()?;
        let marked: Option<i64> = self
            .db
            .query_row(
                "SELECT 1 FROM prop WHERE kind=?1 AND ref=?2 AND key IN (?3,?4) LIMIT 1",
                params![k, id, ddc_model::CONVERTED_FROM, ddc_model::MERGED_FROM],
                |r| r.get(0),
            )
            .optional()?;
        if alive.is_none() || marked.is_none() {
            return Ok(None);
        }
        self.seen.insert(guid.to_string());
        Ok(Some((k, id)))
    }

    /// Kept row (see [`Self::kept`]): properties are upserted (user keys and markers stay),
    /// geometry is untouched; counts as updated when a property changed.
    fn done_kept(
        &mut self,
        kind: &'static str,
        index: usize,
        row: (&str, i64),
    ) -> Result<(), Error> {
        let Some((guid, props)) = self.elems.get(&(kind, index)).copied() else {
            return Ok(());
        };
        self.tick(kind)?;
        let (table, id) = row;
        let db = self.db;
        let before = dump(db, "prop", "ref", id, table)?;
        self.snaps.save(db, "prop", "ref", id)?;
        self.merge.ifc_guid(guid, guid);
        // merged like a linked row: local edits of the properties survive a re-import
        let theirs = generic_props(db, table, id, props)?;
        isrc::merge_props(db, &mut *self.snaps, &mut self.merge, guid, table, id, &theirs)?;
        write_params(db, &mut *self.snaps, &mut self.merge, guid, table, id, props)?;
        self.sync.properties += props.len();
        if dump(self.db, "prop", "ref", id, table)? == before {
            self.sync.unchanged += 1;
        } else {
            self.sync.updated += 1;
        }
        Ok(())
    }

    /// After writing row `id` of plan element (kind, index): merge with the last import (or link
    /// a new row), properties, change count.
    fn done(&mut self, kind: &'static str, index: usize, id: i64) -> Result<(), Error> {
        let Some((guid, props)) = self.elems.get(&(kind, index)).copied() else {
            return Ok(());
        };
        self.tick(kind)?;
        let db = self.db;
        // parameter values held in `pparam_val` are merged one by one in `write_params`
        let theirs = generic_props(db, kind, id, props)?;
        let mine = self
            .mine
            .take()
            .filter(|m| m.0 == guid && m.1 == kind && m.2 == id)
            .map(|m| m.3);
        let dead = self.dead.take().filter(|d| d.0 == guid && d.1 == kind);
        if !guid.is_empty() {
            self.merge.ifc_guid(guid, guid);
        }
        match (self.pending.take(), mine) {
            (Some((k, pid, before)), Some(mine)) if k == kind && pid == id => {
                let mut ap = isrc::merge_row(
                    db,
                    &mut *self.snaps,
                    &mut self.merge,
                    guid,
                    kind,
                    id,
                    &mine,
                    &theirs,
                )?;
                if kind == "railing" && self.merge.policy.delete_removed {
                    let (src, seen) = (&self.source, &self.seen);
                    isrc::keep_host_of_dropped_stair(db, src, seen, id, &mine, &mut ap)?;
                }
                self.restore_children(kind, id, &before, ap)?;
                write_params(db, &mut *self.snaps, &mut self.merge, guid, kind, id, props)?;
                self.sync.properties += props.len();
                if self.state(kind, id)? == before {
                    self.sync.unchanged += 1;
                } else {
                    self.sync.updated += 1;
                }
            }
            (pending, _) => {
                if let (Some((_, _, stale)), true) = (&dead, !guid.is_empty()) {
                    // deleted here: stays deleted unless the file changed it
                    if isrc::merge_dead(
                        db,
                        &mut *self.snaps,
                        &mut self.merge,
                        guid,
                        kind,
                        *stale,
                        id,
                        &theirs,
                    )? {
                        self.zombies.push((kind, id));
                        return Ok(());
                    }
                }
                db.execute(
                    "DELETE FROM prop WHERE kind=?1 AND ref=?2",
                    params![kind, id],
                )?;
                let written = write_props(db, &mut *self.snaps, kind, id, props)?;
                if !guid.is_empty() {
                    // base of the parameter values for the next re-import
                    for (f, v) in written {
                        self.merge.base_only(guid, kind, id, &f, v);
                    }
                }
                self.sync.properties += props.len();
                match pending {
                    Some((k, pid, before)) if k == kind && pid == id => {
                        if self.state(kind, id)? == before {
                            self.sync.unchanged += 1;
                        } else {
                            self.sync.updated += 1;
                        }
                    }
                    _ => {
                        if !guid.is_empty() {
                            db.execute(
                                "INSERT OR REPLACE INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,?4)",
                                params![self.source, guid, kind, id],
                            )?;
                            isrc::merge_added(db, &mut self.merge, guid, kind, id, &theirs)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Child rows (slab outline, panels, vertices) go back to what they were here when the merge
    /// kept the fields they belong to.
    fn restore_children(
        &self,
        kind: &'static str,
        id: i64,
        before: &[Vec<Vec<SqlValue>>],
        ap: isrc::Applied,
    ) -> Result<(), Error> {
        let restore = if kind == "slab" {
            ap.restore_rings
        } else {
            ap.restore_children
        };
        if !restore {
            return Ok(());
        }
        for (i, (table, col)) in children(kind).iter().enumerate() {
            self.db
                .execute(&format!("DELETE FROM {table} WHERE {col}=?1"), [id])?;
            for row in before.get(i + 1).into_iter().flatten() {
                let ph: Vec<String> = (1..=row.len()).map(|n| format!("?{n}")).collect();
                self.db.execute(
                    &format!("INSERT INTO {table} VALUES ({})", ph.join(",")),
                    rusqlite::params_from_iter(row.iter()),
                )?;
            }
        }
        Ok(())
    }

    /// Drops the rows inserted for elements that stay deleted here (with their children,
    /// properties and, for a wall, its openings).
    fn bury(&mut self) -> Result<(), Error> {
        let db = self.db;
        for (kind, id) in std::mem::take(&mut self.zombies) {
            if kind == "wall" {
                // what stands on a wall that stays deleted goes with it: no host left dangling,
                // and no link left that a later row with the same id could be mistaken for
                for (table, host) in [("opening", "wall"), ("comp", "host_wall")] {
                    let ids = format!("SELECT id FROM {table} WHERE {host}=?1");
                    db.execute(
                        &format!("DELETE FROM prop WHERE kind='{table}' AND ref IN ({ids})"),
                        [id],
                    )?;
                    db.execute(
                        &format!("DELETE FROM ext_ref WHERE source=?2 AND kind='{table}' AND ref IN ({ids})"),
                        params![id, self.source],
                    )?;
                    db.execute(&format!("DELETE FROM {table} WHERE {host}=?1"), [id])?;
                }
            }
            for (table, col) in children(kind) {
                db.execute(&format!("DELETE FROM {table} WHERE {col}=?1"), [id])?;
            }
            db.execute(
                "DELETE FROM prop WHERE kind=?1 AND ref=?2",
                params![kind, id],
            )?;
            db.execute(&format!("DELETE FROM {kind} WHERE id=?1"), [id])?;
        }
        Ok(())
    }
}

fn sql_value(v: &PVal) -> SqlValue {
    match v {
        PVal::Int(i) => SqlValue::Integer(*i),
        PVal::Real(r) => SqlValue::Real(*r),
        PVal::Text(s) => SqlValue::Text(s.clone()),
        PVal::Bool(b) => SqlValue::Integer(*b as i64),
    }
}

/// A property of the `DDC_Depth` set on a row of a table that carries a depth.
fn is_depth(kind: &str, key: &str) -> bool {
    key.strip_prefix(ddc_model::depth::PSET)
        .is_some_and(|r| r.starts_with('.'))
        && ddc_model::Discipline::of_table(kind).is_some()
}

/// `DDC_Depth` of the file onto row `id` (`elem_depth`), within the import's undo step.
fn write_depth(
    db: &Connection,
    snaps: &mut Snapshots,
    kind: &str,
    id: i64,
    props: &[PProp],
) -> Result<(), Error> {
    use ddc_model::depth::{PROP_DEPTH, PROP_DISCIPLINE, PSET};
    let (mut depth, mut disc) = (None, None);
    for p in props.iter().filter(|p| is_depth(kind, &p.key)) {
        let name = &p.key[PSET.len() + 1..];
        match (name, &p.value) {
            (n, PVal::Int(i)) if n == PROP_DEPTH => depth = Some(*i),
            (n, PVal::Real(x)) if n == PROP_DEPTH && x.fract() == 0.0 => depth = Some(*x as i64),
            (n, PVal::Text(t)) if n == PROP_DISCIPLINE => disc = Some(t.as_str()),
            _ => {}
        }
    }
    if depth.is_some() || disc.is_some() {
        ddc_model::depth::import_prop(db, snaps, kind, id, depth, disc)?;
    }
    Ok(())
}

/// File properties of row `id` merged as generic fields: all but the `DDC_Parameters` values
/// the row holds as project parameter values (those go through `write_params`).
fn generic_props(
    db: &Connection,
    kind: &str,
    id: i64,
    props: &[PProp],
) -> Result<Vec<(String, SqlValue, String)>, Error> {
    let mut out = Vec::with_capacity(props.len());
    for p in props {
        if is_depth(kind, &p.key) {
            continue;
        }
        if super::uparams::is_param(&p.key) && super::uparams::held(db, kind, id, p)? {
            continue;
        }
        out.push((p.key.clone(), sql_value(&p.value), p.unit.clone()));
    }
    Ok(out)
}

/// Properties of the file onto row `id` without a merge; returns the merge fields of the
/// project parameter values written (for the base).
fn write_props(
    db: &Connection,
    snaps: &mut Snapshots,
    kind: &str,
    id: i64,
    props: &[PProp],
) -> Result<Vec<(String, serde_json::Value)>, Error> {
    let mut params_written = vec![];
    write_depth(db, snaps, kind, id, props)?;
    for p in props {
        if is_depth(kind, &p.key) {
            continue;
        }
        if let Some(f) = super::uparams::put(db, snaps, kind, id, p)? {
            params_written.push(f);
            continue;
        }
        db.prepare_cached(
            "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES (?1,?2,?3,?4,?5)",
        )?
        .execute(params![kind, id, p.key, sql_value(&p.value), p.unit])?;
    }
    Ok(params_written)
}

/// Project parameter values of the file (`DDC_Parameters`) onto linked row `id`, each 3-way
/// merged with the last import; generic properties are left to `merge_row` / `merge_props`.
#[allow(clippy::too_many_arguments)]
fn write_params(
    db: &Connection,
    snaps: &mut Snapshots,
    merge: &mut Merge,
    guid: &str,
    kind: &str,
    id: i64,
    props: &[PProp],
) -> Result<(), Error> {
    // the file's depth is taken as it is: a depth is the run's mark, not a hand edit to merge
    write_depth(db, snaps, kind, id, props)?;
    for p in props.iter().filter(|p| super::uparams::is_param(&p.key)) {
        // false: not held as a parameter value; merged as a generic property (`generic_props`)
        super::uparams::merge(db, snaps, merge, guid, kind, id, p)?;
    }
    Ok(())
}

fn layered_type(
    db: &Connection,
    table: &str,
    layer_table: &str,
    fk: &str,
    t: &super::PType,
) -> Result<i64, Error> {
    let same: Option<i64> = db
        .query_row(
            &format!("SELECT id FROM {table} WHERE name=?1"),
            [&t.name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(i) = same {
        let mut st = db.prepare(&format!(
            "SELECT material, thickness FROM {layer_table} WHERE {fk}=?1 ORDER BY pos"
        ))?;
        let ls: Vec<(String, f64)> = st
            .query_map([i], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        if ls == t.layers {
            return Ok(i);
        }
    }
    let n = unique(db, table, &t.name)?;
    db.execute(&format!("INSERT INTO {table}(name) VALUES (?1)"), [n])?;
    let id = db.last_insert_rowid();
    for (pos, (m, th)) in t.layers.iter().enumerate() {
        db.execute(
            &format!("INSERT INTO {layer_table} VALUES (?1,?2,?3,?4)"),
            params![id, pos as i64, m, th],
        )?;
    }
    Ok(id)
}

fn rings(
    db: &Connection,
    table: &str,
    id: i64,
    outer: &[(f64, f64)],
    holes: &[Vec<(f64, f64)>],
) -> Result<(), Error> {
    db.execute(
        &format!("DELETE FROM {table} WHERE {}=?1", &table[..table.len() - 7]),
        [id],
    )?;
    let mut st = db.prepare_cached(&format!("INSERT INTO {table} VALUES (?1,?2,?3,?4,?5)"))?;
    for (ring, pts) in std::iter::once(outer)
        .chain(holes.iter().map(Vec::as_slice))
        .enumerate()
    {
        for (idx, p) in pts.iter().enumerate() {
            st.execute(params![id, ring as i64, idx as i64, p.0, p.1])?;
        }
    }
    Ok(())
}

/// New material rows from the file's colours and values; a name that already has a row is left
/// untouched. Rows added here are removed by undo.
fn add_materials(db: &Connection, snaps: &mut Snapshots, pl: &Plan) -> Result<(), Error> {
    for m in &pl.materials {
        let have: i64 = db.query_row(
            "SELECT count(*) FROM material WHERE name=?1",
            [&m.name],
            |r| r.get(0),
        )?;
        if have > 0 || m.name.trim() != m.name || m.name.chars().count() > 120 {
            continue;
        }
        snaps.watermark(db, "material")?;
        db.execute(
            "INSERT INTO material(name,color,hatch,density,conductivity,specific_heat,note) \
             VALUES (?1,?2,?3,?4,?5,?6,'')",
            params![
                m.name,
                m.color.filter(|c| *c <= 0xFF_FFFF).map(i64::from),
                ddc_model::material_default_hatch(&m.name),
                m.density,
                m.conductivity,
                m.specific_heat
            ],
        )?;
    }
    Ok(())
}

/// Writes the plan; returns the link outcome. Linked rows are merged with the last import of the
/// source (`policy`: removed elements, mass-change guard), not overwritten.
pub(in crate::ifc) fn commit_with(
    db: &Connection,
    snaps: &mut Snapshots,
    pl: &Plan,
    policy: Policy,
) -> Result<Outcome, Error> {
    commit_with_ctl(db, snaps, pl, policy, None)
}

/// [`commit_with`] that counts written elements of `pl.elems` on `ctl`.
pub(in crate::ifc) fn commit_with_ctl(
    db: &Connection,
    snaps: &mut Snapshots,
    pl: &Plan,
    policy: Policy,
    ctl: Option<&crate::job::Progress>,
) -> Result<Outcome, Error> {
    if let Some(c) = ctl {
        c.set_total(pl.elems.len() as u64);
        c.set(0);
    }
    add_materials(db, snaps, pl)?;
    let source = isrc::namespace(db, pl)?;
    let mut linked = HashMap::new();
    {
        let mut st = db.prepare("SELECT guid, kind, ref FROM ext_ref WHERE source=?1")?;
        let rows = st.query_map([&source], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
        })?;
        for r in rows {
            let (g, v) = r?;
            linked.insert(g, v);
        }
    }
    let merge = Merge::new(&source, policy);
    let mut lk = Linker {
        db,
        snaps,
        source,
        elems: pl
            .elems
            .iter()
            .map(|e| ((e.kind, e.index), (e.guid.as_str(), e.props.as_slice())))
            .collect(),
        linked,
        seen: HashSet::new(),
        sync: Outcome::default(),
        pending: None,
        merge,
        mine: None,
        dead: None,
        zombies: vec![],
        ctl,
    };

    let mut lv = vec![];
    for l in &pl.levels {
        let same: Option<i64> = db
            .query_row(
                "SELECT id FROM level WHERE name=?1 AND elevation=?2",
                params![l.name, l.elevation],
                |r| r.get(0),
            )
            .optional()?;
        lv.push(match same {
            Some(i) => i,
            None => {
                let n = unique(db, "level", &l.name)?;
                db.execute(
                    "INSERT INTO level(name, elevation) VALUES (?1,?2)",
                    params![n, l.elevation],
                )?;
                db.last_insert_rowid()
            }
        });
    }
    for (i, id) in lv.iter().enumerate() {
        lk.link_only("level", i, *id)?;
    }
    // project, site and building have no rows: their GlobalIds are linked under ref 0
    for (kind, g) in [
        ("project", &pl.project_guid),
        ("site", &pl.site_guid),
        ("building", &pl.building_guid),
    ] {
        if !g.is_empty() {
            lk.seen.insert(g.clone());
            db.execute(
                "INSERT OR REPLACE INTO ext_ref(source,guid,kind,ref) VALUES (?1,?2,?3,0)",
                params![lk.source, g, kind],
            )?;
        }
    }
    let ty = pl
        .types
        .iter()
        .map(|t| layered_type(db, "wall_type", "wall_type_layer", "wtype", t))
        .collect::<Result<Vec<_>, _>>()?;
    let sty = pl
        .slab_types
        .iter()
        .map(|t| layered_type(db, "slab_type", "slab_type_layer", "stype", t))
        .collect::<Result<Vec<_>, _>>()?;

    let mut wl = vec![];
    for (i, w) in pl.walls.iter().enumerate() {
        let top = w.top.as_deref().map(ddc_model::top_profile_text);
        let id = match lk.existing("wall", i)? {
            Some(id) => {
                db.execute(
                    "UPDATE wall SET level=?2,wtype=?3,ax=?4,ay=?5,bx=?6,by=?7,base_offset=?8,height=?9,\
                     offset0=?10,axis_kind=?11,bulge=?12,top_profile=?13 WHERE id=?1",
                    params![
                        id,
                        lv[w.level],
                        ty[w.wall_type],
                        w.a.0,
                        w.a.1,
                        w.b.0,
                        w.b.1,
                        w.base_offset,
                        w.height,
                        w.offset0,
                        (w.bulge != 0.0) as i64,
                        w.bulge,
                        top
                    ],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO wall(level,wtype,ax,ay,bx,by,base_offset,height,offset0,axis_kind,bulge,top_profile) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        lv[w.level],
                        ty[w.wall_type],
                        w.a.0,
                        w.a.1,
                        w.b.0,
                        w.b.1,
                        w.base_offset,
                        w.height,
                        w.offset0,
                        (w.bulge != 0.0) as i64,
                        w.bulge,
                        top
                    ],
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("wall", i, id)?;
        wl.push(id);
    }
    for (i, o) in pl.openings.iter().enumerate() {
        if o.width <= 0.0 {
            continue;
        }
        let p =
            params![wl[o.wall], o.along, o.width, o.sill, o.height, o.kind, o.swing.0, o.swing.1];
        let id = match lk.existing("opening", i)? {
            Some(id) => {
                db.execute(
                    "UPDATE opening SET wall=?1,along=?2,width=?3,sill=?4,height=?5,kind=?6,swing_side=?7,\
                     swing_hinge=?8 WHERE id=?9",
                    params![wl[o.wall], o.along, o.width, o.sill, o.height, o.kind, o.swing.0, o.swing.1, id],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO opening(wall,along,width,sill,height,kind,swing_side,swing_hinge) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    p,
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("opening", i, id)?;
        lk.link_only("opening_fill", i, id)?;
    }
    for (k, c) in pl.curtains.iter().enumerate() {
        let vals = params![
            lv[c.level],
            c.name,
            c.a.0,
            c.a.1,
            c.b.0,
            c.b.1,
            c.bulge,
            c.base_offset,
            c.height,
            ddc_model::GridLines::At(c.u.clone()).text(),
            ddc_model::GridLines::At(c.v.clone()).text(),
            ddc_model::section_text(&c.interior),
            ddc_model::section_text(&c.border)
        ];
        let id = match lk.existing("curtain_wall", k)? {
            Some(id) => {
                db.execute(
                    "UPDATE curtain_wall SET level=?1,name=?2,ax=?3,ay=?4,bx=?5,by=?6,bulge=?7,base_offset=?8,\
                     height=?9,ugrid=?10,vgrid=?11,mullion_in=?12,mullion_border=?13 WHERE id=?14",
                    rusqlite::params_from_iter(vals.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
                )?;
                db.execute("DELETE FROM curtain_panel WHERE cw=?1", [id])?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO curtain_wall(level,name,ax,ay,bx,by,bulge,base_offset,height,ugrid,vgrid,\
                     mullion_in,mullion_border) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    vals,
                )?;
                db.last_insert_rowid()
            }
        };
        let (nu, nv) = (c.u.len() + 1, c.v.len() + 1);
        let mut filled = HashSet::new();
        for (i, j, kind, name, side, hinge) in &c.panels {
            filled.insert((*i, *j));
            db.execute(
                "INSERT INTO curtain_panel(cw,i,j,kind,name,swing_side,swing_hinge) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![id, *i as i64, *j as i64, kind, name, side, hinge],
            )?;
        }
        for i in 0..nu {
            for j in 0..nv {
                if !filled.contains(&(i, j)) {
                    db.execute(
                        "INSERT INTO curtain_panel(cw,i,j,kind,name,swing_side,swing_hinge) \
                         VALUES (?1,?2,?3,'empty','',0,0)",
                        params![id, i as i64, j as i64],
                    )?;
                }
            }
        }
        lk.done("curtain_wall", k, id)?;
    }
    let mut sl = vec![];
    for (k, s) in pl.slabs.iter().enumerate() {
        let slope = s
            .slope
            .map(|(p, g)| format!("{} {} {} {}", p.0, p.1, g.0, g.1));
        let id = match lk.existing("slab", k)? {
            Some(id) => {
                db.execute(
                    "UPDATE slab SET level=?2, stype=?3, offset=?4, slope=?5 WHERE id=?1",
                    params![id, lv[s.level], sty[s.slab_type], s.offset, slope],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO slab(level, stype, offset, slope) VALUES (?1,?2,?3,?4)",
                    params![lv[s.level], sty[s.slab_type], s.offset, slope],
                )?;
                db.last_insert_rowid()
            }
        };
        rings(db, "slab_vertex", id, &s.outer, &s.holes)?;
        lk.done("slab", k, id)?;
        sl.push(id);
    }
    for (k, r) in pl.rooms.iter().enumerate() {
        let id =
            match lk.existing("room", k)? {
                Some(id) => {
                    db.execute(
                    "UPDATE room SET level=?2,name=?3,number=?4,px=?5,py=?6,height=?7 WHERE id=?1",
                    params![id, lv[r.level], r.name, r.number, r.point.0, r.point.1, r.height],
                )?;
                    id
                }
                None => {
                    db.execute(
                    "INSERT INTO room(level,name,number,px,py,height) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![lv[r.level], r.name, r.number, r.point.0, r.point.1, r.height],
                )?;
                    db.last_insert_rowid()
                }
            };
        lk.done("room", k, id)?;
    }
    for (k, g) in pl.grids.iter().enumerate() {
        let ex = lk.existing("grid", k)?;
        let old: Option<String> = match ex {
            Some(id) => db
                .query_row("SELECT name FROM grid WHERE id=?1", [id], |r| r.get(0))
                .optional()?,
            None => None,
        };
        let n = if old.as_deref() == Some(g.name.as_str()) {
            g.name.clone()
        } else {
            unique(db, "grid", &g.name)?
        };
        let vals = params![
            n,
            g.a.0,
            g.a.1,
            g.b.0,
            g.b.1,
            g.bulge,
            g.base.map(|i| lv[i]),
            g.top.map(|i| lv[i]),
            g.bubbles
        ];
        let id = match ex {
            Some(id) => {
                db.execute(
                    "UPDATE grid SET name=?1,ax=?2,ay=?3,bx=?4,by=?5,bulge=?6,base_level=?7,top_level=?8,\
                     bubbles=?9 WHERE id=?10",
                    rusqlite::params_from_iter(vals.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO grid(name,ax,ay,bx,by,bulge,base_level,top_level,bubbles) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    vals,
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("grid", k, id)?;
    }
    for (k, x) in pl.seps.iter().enumerate() {
        let id = match lk.existing("room_sep", k)? {
            Some(id) => {
                db.execute(
                    "UPDATE room_sep SET level=?2,ax=?3,ay=?4,bx=?5,by=?6 WHERE id=?1",
                    params![id, lv[x.level], x.a.0, x.a.1, x.b.0, x.b.1],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO room_sep(level,ax,ay,bx,by) VALUES (?1,?2,?3,?4,?5)",
                    params![lv[x.level], x.a.0, x.a.1, x.b.0, x.b.1],
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("room_sep", k, id)?;
    }
    let mut comp_ids: Vec<i64> = Vec::with_capacity(pl.comps.len());
    for (k, c) in pl.comps.iter().enumerate() {
        let fam = ddc_model::family_into(db, &c.def)?;
        let cty = ddc_model::type_into(db, fam, &c.type_name, &c.type_vals)?;
        let (wall, slab) = match c.host {
            PCompHost::Wall(w) => (Some(wl[w]), None),
            PCompHost::Slab(s) => (None, Some(sl[s])),
            PCompHost::None => (None, None),
        };
        let ([x, y, z, r], m) = if c.host == PCompHost::None {
            c.world
        } else {
            c.local
        };
        let id = match lk.existing("comp", k)? {
            Some(id) => {
                db.execute(
                    "UPDATE comp SET level=?2,ctype=?3,x=?4,y=?5,z=?6,rotation=?7,mirror=?8,host_wall=?9,\
                     host_slab=?10,vals=?11 WHERE id=?1",
                    params![id, lv[c.level], cty, x, y, z, r, m as i64, wall, slab, c.vals],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO comp(level,ctype,x,y,z,rotation,mirror,host_wall,host_slab,vals) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![lv[c.level], cty, x, y, z, r, m as i64, wall, slab, c.vals],
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("comp", k, id)?;
        comp_ids.push(id);
    }
    let mut systems: HashMap<&str, i64> = HashMap::new();
    for (k, m) in pl.meps.iter().enumerate() {
        // a run linked by a previous import is rewritten in place, in its system by name
        let prior = lk.existing("mep_run", k)?;
        let sys = match systems.get(m.system.as_str()) {
            Some(&s) => s,
            None => {
                let same: Option<i64> = match prior {
                    Some(_) => db
                        .query_row(
                            "SELECT id FROM mep_system WHERE name=?1 AND kind=?2",
                            params![m.system, m.kind.name()],
                            |r| r.get(0),
                        )
                        .optional()?,
                    None => None,
                };
                let s = match same {
                    Some(s) => s,
                    None => {
                        let n = unique(db, "mep_system", &m.system)?;
                        let c = m.kind.color();
                        db.execute(
                            "INSERT INTO mep_system(name,kind,color) VALUES (?1,?2,?3)",
                            params![
                                n,
                                m.kind.name(),
                                (c[0] as i64) << 16 | (c[1] as i64) << 8 | c[2] as i64
                            ],
                        )?;
                        db.last_insert_rowid()
                    }
                };
                systems.insert(&m.system, s);
                s
            }
        };
        let mut spec = m.spec.clone();
        spec.system = sys;
        spec.level = lv[m.level];
        let id = match prior {
            Some(id) => {
                ddc_model::replace_mep_run(db, id, &spec)?;
                id
            }
            None => ddc_model::insert_mep_run(db, &spec)?,
        };
        lk.done("mep_run", k, id)?;
    }
    if !pl.mep_links.is_empty() {
        let mut sink = StoredPorts::new(&mut *lk.snaps);
        mep_links::apply(
            pl,
            &comp_ids,
            &systems,
            db,
            &mut sink,
            &mut lk.sync.warnings,
        );
    }

    circuits::apply(
        pl,
        &comp_ids,
        &systems,
        db,
        &mut *lk.snaps,
        &mut lk.sync.warnings,
    )?;

    // members
    let mut mtypes: HashMap<(String, String, String), i64> = HashMap::new();
    for (k, m) in pl.members.iter().enumerate() {
        let shape = m.shape.text();
        let key = (m.type_name.clone(), shape.clone(), m.material.clone());
        let mt = match mtypes.get(&key) {
            Some(&t) => t,
            None => {
                let same: Option<i64> = db
                    .query_row(
                        "SELECT id FROM member_type WHERE name=?1 AND shape=?2 AND material=?3",
                        params![key.0, key.1, key.2],
                        |r| r.get(0),
                    )
                    .optional()?;
                let t = match same {
                    Some(t) => t,
                    None => {
                        let n = unique(db, "member_type", &m.type_name)?;
                        db.execute(
                            "INSERT INTO member_type(name, shape, material) VALUES (?1,?2,?3)",
                            params![n, shape, m.material],
                        )?;
                        db.last_insert_rowid()
                    }
                };
                mtypes.insert(key, t);
                t
            }
        };
        let elev: f64 = db.query_row(
            "SELECT elevation FROM level WHERE id=?1",
            [lv[m.level]],
            |r| r.get(0),
        )?;
        let kind = if m.column { "col" } else { "beam" };
        let ex = lk.existing(kind, k)?;
        let id = match &m.exact {
            Some(MemberExact::Column(sp, top)) => {
                let mut s = sp.clone();
                s.level = lv[m.level];
                s.member_type = mt;
                s.top_level = top
                    .as_ref()
                    .and_then(|n| pl.levels.iter().position(|l| &l.name == n))
                    .map(|i| lv[i]);
                ddc_model::put_column(db, &s, ex)
            }
            Some(MemberExact::Beam(sp)) => {
                let mut s = sp.clone();
                s.level = lv[m.level];
                s.member_type = mt;
                ddc_model::put_beam(db, &s, ex)
            }
            None if m.column => {
                let vertical = (m.a[0] - m.b[0]).abs() < 1e-9 && (m.a[1] - m.b[1]).abs() < 1e-9;
                let s = ddc_model::ColumnSpec {
                    level: lv[m.level],
                    top_level: None,
                    member_type: mt,
                    structural: true,
                    at: (m.a[0], m.a[1]),
                    base_offset: m.a[2] - elev,
                    top_offset: m.b[2] - elev,
                    rotation: m.rotation,
                    top_at: (!vertical).then_some((m.b[0], m.b[1])),
                };
                ddc_model::put_column(db, &s, ex)
            }
            None => {
                let s = ddc_model::BeamSpec {
                    level: lv[m.level],
                    member_type: mt,
                    a: (m.a[0], m.a[1], m.a[2] - elev),
                    b: (m.b[0], m.b[1], m.b[2] - elev),
                    bulge: 0.0,
                    yjust: ddc_model::Just::Center,
                    zjust: ddc_model::Just::Center,
                    rotation: m.rotation,
                    start_ext: 0.0,
                    end_ext: 0.0,
                    join_start: ddc_model::BeamJoin::None,
                    join_end: ddc_model::BeamJoin::None,
                };
                ddc_model::put_beam(db, &s, ex)
            }
        };
        // a member the model rejects is skipped, not fatal
        match id {
            Ok(id) => lk.done(kind, k, id)?,
            Err(e) => lk.sync.warnings.push(format!("{kind} skipped: {e}")),
        }
    }
    for (k, r) in pl.roofs.iter().enumerate() {
        let s = ddc_model::RoofSpec {
            level: lv[r.level],
            slab_type: sty[r.roof_type],
            offset: r.offset,
            overhang: r.overhang,
            fascia: r.fascia,
            kind: r.kind.clone(),
        };
        let ex = lk.existing("roof", k)?;
        match ddc_model::put_roof(db, &s, ex) {
            Ok(id) => {
                let holes = pl.roof_holes.iter().filter(|h| h.0 == k).map(|h| &h.1);
                super::host_voids::commit_roof_holes(db, &mut *lk.snaps, id, holes)?;
                lk.done("roof", k, id)?
            }
            Err(e) => lk.sync.warnings.push(format!("roof skipped: {e}")),
        }
    }
    for (k, c) in pl.ceilings.iter().enumerate() {
        let s = ddc_model::CeilingSpec {
            level: lv[c.level],
            slab_type: sty[c.ceiling_type],
            height: c.height,
            outer: c.outer.clone(),
            holes: c.holes.clone(),
        };
        let ex = lk.existing("ceiling", k)?;
        match ddc_model::put_ceiling(db, &s, ex) {
            Ok(id) => lk.done("ceiling", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("ceiling skipped: {e}")),
        }
    }
    for (k, p) in pl.footings.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        let ex = lk.existing("iso_footing", k)?;
        match put_footing(db, &s, ex) {
            Ok(id) => lk.done("iso_footing", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("footing skipped: {e}")),
        }
    }
    for (k, p) in pl.strip_footings.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        let ex = lk.existing("strip_footing", k)?;
        match put_strip(db, &s, ex) {
            Ok(id) => lk.done("strip_footing", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("strip footing skipped: {e}")),
        }
    }
    let level_ix = |name: &Option<String>, own: usize| {
        name.as_ref()
            .and_then(|n| pl.levels.iter().position(|l| &l.name == n))
            .map_or(lv[own], |i| lv[i])
    };
    let mut stair_ids: Vec<Option<i64>> = vec![None; pl.stairs.len()];
    for (k, p) in pl.stairs.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        s.top_level = level_ix(&p.top, p.level);
        let ex = lk.existing("stair", k)?;
        match ddc_model::put_stair(db, &s, ex) {
            Ok(id) => {
                stair_ids[k] = Some(id);
                lk.done("stair", k, id)?
            }
            Err(e) => lk.sync.warnings.push(format!("stair skipped: {e}")),
        }
    }
    for (k, p) in pl.railings.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        if let Some((ix, side)) = p.host {
            let Some(stair) = stair_ids.get(ix).copied().flatten() else {
                lk.sync
                    .warnings
                    .push("railing skipped: its stair was not imported".into());
                continue;
            };
            s.host = ddc_model::RailingHost::Stair { stair, side };
        }
        let ex = lk.existing("railing", k)?;
        match ddc_model::put_railing(db, &s, ex) {
            Ok(id) => lk.done("railing", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("railing skipped: {e}")),
        }
    }
    for (k, p) in pl.ramps.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        s.top_level = p
            .top
            .as_ref()
            .and_then(|n| pl.levels.iter().position(|l| &l.name == n))
            .map(|i| lv[i]);
        let ex = lk.existing("ramp", k)?;
        match ddc_model::put_ramp(db, &s, ex) {
            Ok(id) => lk.done("ramp", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("ramp skipped: {e}")),
        }
    }
    for (k, p) in pl.solids.iter().enumerate() {
        let mut s = p.spec.clone();
        s.level = lv[p.level];
        let ex = lk.existing("solid", k)?;
        match ddc_model::put_solid(db, &s, ex) {
            Ok(id) => lk.done("solid", k, id)?,
            Err(e) => lk.sync.warnings.push(format!("solid skipped: {e}")),
        }
    }
    for (k, m) in pl.meshes.iter().enumerate() {
        let mut pb = Vec::with_capacity(m.positions.len() * 24);
        for p in &m.positions {
            for c in p {
                pb.extend_from_slice(&c.to_le_bytes());
            }
        }
        let ib: Vec<u8> = m.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        // converted or merged by the user: refresh data, never re-insert the mesh
        if let Some((t, id)) = lk.kept("mesh", k)? {
            lk.done_kept("mesh", k, (&t, id))?;
            continue;
        }
        let id = match lk.existing("mesh", k)? {
            Some(id) => {
                db.execute(
                    "UPDATE mesh SET level=?2,category=?3,name=?4,positions=?5,indices=?6 WHERE id=?1",
                    params![id, lv[m.level], m.category, m.name, pb, ib],
                )?;
                id
            }
            None => {
                db.execute(
                    "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,?3,?4,?5)",
                    params![lv[m.level], m.category, m.name, pb, ib],
                )?;
                db.last_insert_rowid()
            }
        };
        lk.done("mesh", k, id)?;
    }
    if !pl.project_props.is_empty() {
        lk.snaps.save(db, "prop", "ref", 0)?;
        db.execute("DELETE FROM prop WHERE kind='project' AND ref=0", [])?;
        write_props(db, &mut *lk.snaps, "project", 0, &pl.project_props)?;
        lk.sync.properties += pl.project_props.len();
    }
    close_rooms(db, pl, &lv, &mut lk.sync.warnings)?;
    lk.bury()?;
    lk.sync.missing = lk.linked.keys().filter(|g| !lk.seen.contains(*g)).count();
    lk.sync.removed = isrc::finish(db, &mut *lk.snaps, &mut lk.merge, &lk.seen, pl)?;
    lk.sync.kept_local = lk.merge.stats.kept_local;
    lk.sync.conflicts = lk.merge.stats.conflicts;
    lk.sync.locked = std::mem::take(&mut lk.merge.stats.locked);
    Ok(lk.sync)
}

/// Writes (or rewrites row `id` of) a footing without a host column; same row layout and checks
/// as the model's own insert.
fn put_footing(db: &Connection, s: &ddc_model::FootingSpec, id: Option<i64>) -> Result<i64, Error> {
    ddc_geom::footing::Isolated {
        round: s.round,
        steps: s.steps.clone(),
        center: ddc_geom::v2(s.at.0, s.at.1),
        rotation: s.rotation,
        top: s.top_offset,
    }
    .check()
    .map_err(Error::Invalid)?;
    let steps = s
        .steps
        .iter()
        .map(|t| format!("{} {} {}", t.w, t.l, t.h))
        .collect::<Vec<_>>()
        .join(";");
    db.execute(
        "INSERT OR REPLACE INTO iso_footing(id,level,col,x,y,ex,ey,rotation,round,steps,top_offset,\
         material,frot,ftop) VALUES (?1,?2,NULL,?3,?4,0,0,?5,?6,?7,?8,?9,?5,?8)",
        params![
            id,
            s.level,
            s.at.0,
            s.at.1,
            s.rotation,
            s.round as i64,
            steps,
            s.top_offset,
            s.material
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Writes (or rewrites row `id` of) a straight strip footing without a host wall.
fn put_strip(
    db: &Connection,
    s: &ddc_model::StripFootingSpec,
    id: Option<i64>,
) -> Result<i64, Error> {
    let v = [s.a.0, s.a.1, s.b.0, s.b.1, s.width, s.depth, s.top_offset];
    let len = (s.b.0 - s.a.0).hypot(s.b.1 - s.a.1);
    if !v.iter().all(|x| x.is_finite()) || s.width <= 0.0 || s.depth <= 0.0 || len < 1e-6 {
        return Err(Error::Invalid(
            "strip footing: width, depth and length must be positive".into(),
        ));
    }
    db.execute(
        "INSERT OR REPLACE INTO strip_footing(id,level,wall,ax,ay,bx,by,bulge,width,depth,ecc,\
         top_offset,material,ftop) VALUES (?1,?2,NULL,?3,?4,?5,?6,0,?7,?8,0,?9,?10,?9)",
        params![
            id,
            s.level,
            s.a.0,
            s.a.1,
            s.b.0,
            s.b.1,
            s.width,
            s.depth,
            s.top_offset,
            s.material
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Rooms the walls leave open get separators along the space outline where no wall stands;
/// a room still open is reported (its area stays 0).
fn close_rooms(
    db: &Connection,
    pl: &Plan,
    lv: &[i64],
    warnings: &mut Vec<String>,
) -> Result<(), Error> {
    let mut by_level: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (k, r) in pl.rooms.iter().enumerate() {
        if r.ring.len() >= 3 {
            by_level.entry(r.level).or_default().push(k);
        }
    }
    let query = |ks: &[usize]| -> Vec<((f64, f64), Vec<(f64, f64)>)> {
        ks.iter()
            .map(|&k| (pl.rooms[k].point, pl.rooms[k].ring.clone()))
            .collect()
    };
    let label = |k: usize| {
        let r = &pl.rooms[k];
        format!("room {} '{}'", r.number, r.name)
    };
    let mut closed = 0;
    for (level, ks) in by_level {
        let Ok(closers) = ddc_model::room_closers(db, lv[level], &query(&ks)) else {
            continue;
        };
        let mut seen = HashSet::new();
        let mut added = vec![];
        for (&k, c) in ks.iter().zip(closers) {
            let Some(segs) = c else { continue };
            if segs.is_empty() {
                warnings.push(format!(
                    "{}: outline lies on walls but is not closed; area 0",
                    label(k)
                ));
                continue;
            }
            for (a, b) in segs {
                let key = |p: (f64, f64)| ((p.0 * 1e6).round() as i64, (p.1 * 1e6).round() as i64);
                let (ka, kb) = (key(a), key(b));
                if !seen.insert((ka.min(kb), ka.max(kb))) {
                    continue;
                }
                db.execute(
                    "INSERT INTO room_sep(level,ax,ay,bx,by) VALUES (?1,?2,?3,?4,?5)",
                    params![lv[level], a.0, a.1, b.0, b.1],
                )?;
            }
            added.push(k);
        }
        if added.is_empty() {
            continue;
        }
        let Ok(again) = ddc_model::room_closers(db, lv[level], &query(&added)) else {
            continue;
        };
        for (&k, c) in added.iter().zip(again) {
            match c {
                None => closed += 1,
                Some(_) => warnings.push(format!("{}: outline not closed; area 0", label(k))),
            }
        }
    }
    if closed > 0 {
        warnings.push(format!(
            "{closed} room(s) closed by separators along the space outline"
        ));
    }
    Ok(())
}
