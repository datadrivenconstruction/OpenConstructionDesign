//! Partial regeneration of derived tables after an operation and undo. Rows changed since the
//! last regeneration (the `changes` journal + the transaction's uncommitted rows) form the dirty
//! set: walls and rooms, slabs, bars, component instances. Dependents: a wall -> its neighbours at
//! the joint (`walls::regenerate_near`) and the components on it; a column -> the deduction from
//! the affected walls (without a new joint) and, as a bearing element, the slabs it crosses (old
//! and new position); a slab -> walls with their bottom attached to it, bars under the
//! deduction, and components on it.
//! Only these are recomputed; the light stages (roofs, structure, shafts, stairs, solids,
//! dimensions) are recomputed in full if their inputs changed. Types, levels, axes, curtain
//! walls, shapes and shape joins, shafts, roofs, sketches and any unknown table trigger a full
//! regeneration. `DDC_REGEN_VERIFY=1` compares every partial regeneration with a full one (debug
//! panics, release prints a message; the full result stays in the database), `DDC_REGEN_FULL=1`
//! forces full regeneration only.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use ddc_geom::member::Member;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension};

use crate::changes::Tracker;
use crate::ops::Inv;
use crate::structure::{
    bbox, beam_box, boxes_meet, column_plan, slab_box, Built, ColPlan, FlatSlab, SlabBox,
};
use crate::walls::{Pieces, RegenLevel, WallLayer};
use crate::Error;

/// Rows at once beyond which a full regeneration is cheaper than analysis (bulk import).
const MAX_ROWS: usize = 5000;

/// Tables that no regeneration stage reads.
const INERT: &[&str] = &[
    // joints only constrain; a drive moves `comp` rows, which regenerate as usual
    "asm_joint",
    "elem_phase",
    "phase",
    "view_phase",
    "prop",
    "param",
    "pparam",
    "pparam_cat",
    "pparam_val",
    "ext_ref",
    "ext_ref_v7",
    "clash_item",
    "mesh",
    "mesh_geo",
    "mesh_proto",
    "mep_run",
    "mep_system",
    "site_info",
    "datum_extent",
    "view_def",
    "viewport",
    "ref_plane",
    "topo_pad",
    "topo_point",
    "topo_region",
    "toposolid",
    "rebar_type",
    "rebar_set",
    "rebar_cover",
    "rebar_mark",
    "analytical_adj",
    "ramp",
    "zone",
    "zone_room",
    "mep_port",
    "boundary_link",
    // written by the measurement refresh only
    "measure_geo",
    "measure_ref",
    // freeform patches: mesh and quantities are built from the row on demand
    "freeform",
    // curtain systems on freeform hosts: panels and mullions derived on demand
    "panel_system",
    // roofs and slabs on surfaces: the host sync writes the row itself
    "shell_elem",
    // who set a value and when: provenance, no geometry
    "value_src",
    // design depth of elements and the project setting: no geometry
    "elem_depth",
    "depth_cfg",
];

/// Derived tables (written only by regeneration), for comparison with a full regeneration.
const DERIVED: &[&str] = &[
    "wall_qty",
    "room_qty",
    "slab_qty",
    "member_qty",
    "roof_qty",
    "struct_qty",
    "struct_issue",
    "ceiling_qty",
    "shaft_cut",
    "stair_qty",
    "railing_qty",
    "comp_qty",
    "solid_qty",
    "solid_cut",
    "solid_base",
    "mass_qty",
    "mass_cache",
    "join_cache",
    "dim_geo",
    "measure_geo",
    "measure_ref",
    // spline walls regenerate through their piece rows in `wall`
    "wall_spline",
    "wall_spline_piece",
];

fn inert(t: &str) -> bool {
    INERT.contains(&t)
        || t.starts_with("d_")
        || t.starts_with("sheet")
        // the assistant's records (options, audit, chat, policy)
        || t.starts_with("ai_")
        || crate::calc_store::CALC_TABLES.contains(&t)
}

/// Rows no recompute looks at: the ones `changes::dirty` leaves out and inert tables.
fn unread(t: &str) -> bool {
    matches!(t, "op" | "meta" | crate::changes::UNDERLAY) || inert(t)
}

thread_local! {
    static STATS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    static VERIFY: Cell<bool> = const { Cell::new(false) };
    static NEAR: Cell<u64> = const { Cell::new(0) };
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

fn verify_on() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    *ENV.get_or_init(|| env_on("DDC_REGEN_VERIFY")) || VERIFY.with(Cell::get)
}

fn force_full() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    *ENV.get_or_init(|| env_on("DDC_REGEN_FULL"))
}

fn tally(incremental: bool) {
    STATS.with(|s| {
        let (i, f) = s.get();
        s.set(if incremental { (i + 1, f) } else { (i, f + 1) });
    });
}

/// Thread counters: (partial, full) regenerations.
#[cfg(test)]
pub(crate) fn stats() -> (u64, u64) {
    STATS.with(Cell::get)
}

/// Thread counter: levels recomputed only around the walls' neighbourhood.
#[cfg(test)]
pub(crate) fn near_passes() -> u64 {
    NEAR.with(Cell::get)
}

/// Compare every partial regeneration with a full one in this thread (tests).
#[cfg(test)]
pub(crate) fn set_verify(on: bool) {
    VERIFY.with(|v| v.set(on));
}

/// A bar after joints, as the last regeneration saw it.
struct MemberRec {
    kind: &'static str,
    id: i64,
    level: i64,
    m: Member,
    /// Joint deduction (flange trimming), before slab deductions.
    join_cut: f64,
    structural: bool,
    plan: Option<ColPlan>,
    seg: Option<[f64; 4]>,
}

impl MemberRec {
    fn new(b: &Built, join_cut: f64) -> Self {
        let col = b.kind == "column";
        Self {
            kind: b.kind,
            id: b.id,
            level: b.level,
            m: b.m.clone(),
            join_cut,
            structural: b.structural,
            plan: if col { column_plan(&b.m) } else { None },
            seg: if col { None } else { beam_box(&b.m) },
        }
    }
    fn built(&self) -> Built {
        Built {
            kind: self.kind,
            id: self.id,
            level: self.level,
            m: self.m.clone(),
            cut: self.join_cut,
            structural: self.structural,
        }
    }
    fn key(&self) -> (&'static str, i64) {
        (self.kind, self.id)
    }
    fn same(&self, o: &MemberRec) -> bool {
        self.m == o.m
            && self.join_cut == o.join_cut
            && self.structural == o.structural
            && self.level == o.level
    }
    /// Extent by which a bar takes its deduction from slabs (a bearing column gives the deduction away).
    fn cut_box(&self) -> Option<[f64; 4]> {
        if self.kind == "column" {
            (!self.structural)
                .then(|| self.plan.as_ref().map(|p| p.2))
                .flatten()
        } else {
            self.seg
        }
    }
}

/// A level: wall contributions and their extent in plan and height.
struct LevelCache {
    pieces: Pieces,
    overlaps: bool,
    env: Option<([f64; 4], f64, f64)>,
}

impl LevelCache {
    fn new(r: RegenLevel) -> Self {
        Self {
            env: envelope(&r.pieces),
            pieces: r.pieces,
            overlaps: r.overlaps,
        }
    }
}

fn envelope(pieces: &Pieces) -> Option<([f64; 4], f64, f64)> {
    let mut out: Option<([f64; 4], f64, f64)> = None;
    for (_, _, rings, z0, z1) in pieces.values().flat_map(|p| &p.layers) {
        for r in rings.iter().filter(|r| !r.is_empty()) {
            let b = bbox(r);
            out = Some(match out {
                None => (b, *z0, *z1),
                Some((a, lo, hi)) => (
                    [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ],
                    lo.min(*z0),
                    hi.max(*z1),
                ),
            });
        }
    }
    out
}

/// Model state at the moment of the last regeneration: old row owners (a deleted row can no longer
/// name its level), level footprints, bars, slab extents.
#[derive(Default)]
pub(crate) struct Index {
    /// Journal number up to which the index is valid; None means the next regeneration is full.
    head: Option<u64>,
    /// This transaction's regeneration succeeded: `promote` after commit makes the index valid.
    fresh: bool,
    /// Operations leave the recompute to `Model::settle_derived` (`Model::hold_derived`).
    pub(crate) held: bool,
    /// An operation skipped its recompute while held: the next recompute covers its rows.
    pub(crate) owed: bool,
    /// The open transaction's rows the last recompute took in (`changes::tx_mark`).
    mark: Option<(u64, usize)>,
    wall_level: HashMap<i64, i64>,
    opening_wall: HashMap<i64, i64>,
    room_level: HashMap<i64, i64>,
    sep_level: HashMap<i64, i64>,
    vertex_slab: HashMap<i64, i64>,
    /// Flat slabs: the extent of the outer outline and thickness; None if not read.
    slabs: Option<HashMap<i64, SlabBox>>,
    levels: HashMap<i64, LevelCache>,
    members: Vec<MemberRec>,
}

#[derive(Clone, Copy)]
enum Map {
    Wall,
    Opening,
    Room,
    Sep,
    Vertex,
}

impl Index {
    /// After committing a transaction with regeneration: the index is valid up to number `head`.
    pub(crate) fn promote(&mut self, head: u64) {
        if std::mem::take(&mut self.fresh) {
            self.head = Some(head);
        }
    }

    /// Before the commit of a longer transaction (`Model::atomic`): its last recompute took in
    /// every row it wrote but those no recompute reads. Then, committed, the index is right at
    /// the new head ([`Self::promote_committed`]) and the next operation does not recompute
    /// the whole transaction again.
    pub(crate) fn current_in_tx(&self, tracker: &Mutex<Tracker>) -> bool {
        self.head.is_some()
            && !self.held
            && !self.owed
            && self.mark.is_some_and(|m| crate::changes::quiet_since(tracker, m, unread))
    }

    pub(crate) fn promote_committed(&mut self, head: u64) {
        self.head = Some(head);
        self.mark = None;
    }

    fn map(&mut self, m: Map) -> &mut HashMap<i64, i64> {
        match m {
            Map::Wall => &mut self.wall_level,
            Map::Opening => &mut self.opening_wall,
            Map::Room => &mut self.room_level,
            Map::Sep => &mut self.sep_level,
            Map::Vertex => &mut self.vertex_slab,
        }
    }

    fn rebuild(
        &mut self,
        db: &Connection,
        levels: Vec<RegenLevel>,
        members: Vec<MemberRec>,
        slabs: Option<Vec<(i64, SlabBox)>>,
    ) -> Result<(), Error> {
        self.wall_level = pairs(db, "SELECT id, level FROM wall")?;
        self.opening_wall = pairs(db, "SELECT id, wall FROM opening")?;
        self.room_level = pairs(db, "SELECT id, level FROM room")?;
        self.sep_level = pairs(db, "SELECT id, level FROM room_sep")?;
        self.vertex_slab = pairs(db, "SELECT rowid, slab FROM slab_vertex")?;
        self.slabs = slabs.map(|v| v.into_iter().collect());
        self.levels = levels
            .into_iter()
            .map(|r| (r.polys.0, LevelCache::new(r)))
            .collect();
        self.members = members;
        Ok(())
    }
}

/// Extents of flat slabs if not yet read (current state).
fn load_slabs(slot: &mut Option<HashMap<i64, SlabBox>>, db: &Connection) -> Result<(), Error> {
    if slot.is_none() {
        let all = crate::structure::flat_slabs(db, None)?;
        *slot = Some(
            all.iter()
                .filter_map(|s| slab_box(s).map(|b| (s.0, b)))
                .collect(),
        );
    }
    Ok(())
}

fn pairs(db: &Connection, sql: &str) -> Result<HashMap<i64, i64>, Error> {
    let mut st = db.prepare(sql)?;
    let v = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<HashMap<i64, i64>, _>>()?;
    Ok(v)
}

fn one(db: &Connection, sql: &str, id: i64) -> Result<Option<i64>, Error> {
    Ok(db
        .prepare_cached(sql)?
        .query_row([id], |r| r.get::<_, i64>(0))
        .optional()?)
}

fn many(db: &Connection, sql: &str, id: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare_cached(sql)?;
    let v = st
        .query_map([id], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn exists(db: &Connection, table: &str) -> Result<bool, Error> {
    Ok(
        db.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |r| {
            r.get::<_, bool>(0)
        })?,
    )
}

/// Regeneration in an operation or undo transaction: partial by the index, otherwise full.
pub(crate) fn logged(
    db: &Connection,
    tracker: &Mutex<Tracker>,
    idx: &mut Index,
) -> Result<Inv, Error> {
    idx.fresh = false;
    // the rows since `head` include those of operations that held their recompute back
    idx.owed = false;
    let covered = idx.mark.take();
    let since = idx.head.take().filter(|_| !force_full());
    let inv = match since {
        None => {
            tally(false);
            let _s = crate::perf::span("regen.full");
            full_in(db, Some(&mut *idx), true)?
        }
        Some(since) => {
            let plan_s = crate::perf::span("regen.plan");
            crate::cuts::sync_walls(db)?;
            let plan = match crate::changes::dirty_after(tracker, since, covered) {
                Some(rows) => Plan::new(db, idx, &rows)?,
                None => None,
            };
            drop(plan_s);
            match plan {
                None => {
                    tally(false);
                    let _s = crate::perf::span("regen.full");
                    full_in(db, Some(&mut *idx), false)?
                }
                Some(p) => {
                    tally(true);
                    let _s = crate::perf::span("regen.partial");
                    let inv = run(db, p, idx)?;
                    if verify_on() {
                        verify(db)?;
                    }
                    inv
                }
            }
        }
    };
    idx.fresh = true;
    // inside a longer transaction (`Model::atomic`) the next recompute skips the rows so far
    idx.mark = crate::changes::tx_mark(tracker);
    Ok(inv)
}

/// Full regeneration of all derived tables.
#[cfg(test)]
pub(crate) fn full(db: &Connection) -> Result<Inv, Error> {
    full_in(db, None, true)
}

fn full_in(db: &Connection, idx: Option<&mut Index>, sync: bool) -> Result<Inv, Error> {
    if sync {
        crate::cuts::sync_walls(db)?;
    }
    let levels = crate::walls::regenerate(db, None)?;
    let fps: Vec<_> = levels.iter().map(|l| l.polys.clone()).collect();
    crate::rooms::regenerate(db, &fps, false)?;
    let fresh = crate::slabs::regenerate(db, None)?;
    let layers: Vec<&[WallLayer]> = levels.iter().map(|l| &l.layers[..]).collect();
    let st = crate::perf::span("regen.full.structure");
    let (members, joins, slabs) = crate::structure::regenerate(db, &layers, &fresh)?;
    drop(st);
    crate::roofs::regenerate(db)?;
    let recs: Vec<MemberRec> = if idx.is_some() {
        members
            .iter()
            .zip(&joins)
            .map(|(b, &c)| MemberRec::new(b, c))
            .collect()
    } else {
        vec![]
    };
    let st = crate::perf::span("regen.full.struct_ext");
    let inv = crate::struct_ext::regenerate(db, members)?;
    drop(st);
    crate::shafts::regenerate(db)?;
    crate::stairs::regenerate(db)?;
    crate::components::regenerate(db)?;
    let st = crate::perf::span("regen.full.solids");
    crate::solids::regenerate(db)?;
    drop(st);
    crate::mass::regenerate(db)?;
    crate::annot::regenerate(db)?;
    crate::measure::regenerate(db)?;
    if let Some(idx) = idx {
        idx.rebuild(db, levels, recs, slabs)?;
    }
    Ok(inv)
}

/// What to recompute: the dirty rows and their closure over dependencies.
#[derive(Default)]
struct Plan {
    /// Walls with changed rows (their own, their openings', or attached to a changed slab).
    walls: BTreeSet<i64>,
    /// The same walls by level (old and new).
    rejoin: BTreeMap<i64, BTreeSet<i64>>,
    /// Levels a wall left or arrived on: their join cache cannot be patched near the wall.
    moved: BTreeSet<i64>,
    /// Changed columns, old and new position: the affected walls take their deduction anew.
    col_hits: Vec<([f64; 4], (f64, f64))>,
    room_levels: BTreeSet<i64>,
    rooms: BTreeSet<i64>,
    /// Slabs with changed rows.
    slab_rows: BTreeSet<i64>,
    /// Slabs to recompute (+ those crossed by changed bearing columns).
    slabs: BTreeSet<i64>,
    /// Flat ones from `slabs` after the edit.
    geo: Vec<FlatSlab>,
    comps: BTreeSet<i64>,
    /// Bars anew (col/beam changed).
    members: Option<Vec<MemberRec>>,
    redo: HashSet<(&'static str, i64)>,
    gone: Vec<(&'static str, i64)>,
    dims: bool,
    /// Saved measurements with changed rows.
    measures: BTreeSet<i64>,
    stairs: bool,
    /// Steel connections with changed rows: only they are rebuilt.
    conns: BTreeSet<i64>,
    /// Current row owners for the index.
    now: Vec<(Map, i64, Option<i64>)>,
}

impl Plan {
    /// None means a full regeneration is needed.
    fn new(
        db: &Connection,
        idx: &mut Index,
        rows: &HashMap<String, HashSet<i64>>,
    ) -> Result<Option<Plan>, Error> {
        // connection rows rebuild one by one whatever their number
        let n: usize = rows.iter().filter(|(t, _)| t.as_str() != "steel_conn").map(|(_, v)| v.len()).sum();
        if n > MAX_ROWS || exists(db, "geom_join")? {
            return Ok(None);
        }
        let mut p = Plan::default();
        let mut openings: Vec<i64> = vec![];
        let mut seps: Vec<i64> = vec![];
        let mut vertices: Vec<i64> = vec![];
        let mut members = false;
        for (t, ids) in rows {
            let t = t.as_str();
            if inert(t) {
                continue;
            }
            if ids.contains(&crate::changes::WHOLE) {
                return Ok(None);
            }
            match t {
                "wall" => p.walls.extend(ids),
                "opening" => openings.extend(ids),
                "room" => p.rooms.extend(ids),
                "room_sep" => seps.extend(ids),
                "slab" => p.slab_rows.extend(ids),
                "slab_vertex" => vertices.extend(ids),
                "col" | "beam" => members = true,
                "comp" => p.comps.extend(ids),
                "dim" => p.dims = true,
                "measure" => p.measures.extend(ids),
                "stair" | "railing" | "railing_vertex" => p.stairs = true,
                "steel_conn" => p.conns.extend(ids),
                _ => return Ok(None),
            }
        }
        for o in openings {
            let now = one(db, "SELECT wall FROM opening WHERE id=?1", o)?;
            p.walls
                .extend(now.into_iter().chain(idx.opening_wall.get(&o).copied()));
            p.now.push((Map::Opening, o, now));
        }
        for &r in &p.rooms {
            let now = one(db, "SELECT level FROM room WHERE id=?1", r)?;
            p.room_levels
                .extend(now.into_iter().chain(idx.room_level.get(&r).copied()));
            p.now.push((Map::Room, r, now));
        }
        for s in seps {
            let now = one(db, "SELECT level FROM room_sep WHERE id=?1", s)?;
            p.room_levels
                .extend(now.into_iter().chain(idx.sep_level.get(&s).copied()));
            p.now.push((Map::Sep, s, now));
        }
        for v in vertices {
            let now = one(db, "SELECT slab FROM slab_vertex WHERE rowid=?1", v)?;
            p.slab_rows
                .extend(now.into_iter().chain(idx.vertex_slab.get(&v).copied()));
            p.now.push((Map::Vertex, v, now));
        }
        // previous outlines of changed slabs: bars could have stood under them
        let had_slabs = idx.slabs.is_some();
        let mut boxes: Vec<[f64; 4]> = p
            .slab_rows
            .iter()
            .filter_map(|s| idx.slabs.as_ref().and_then(|m| m.get(s)).map(|b| b.0))
            .collect();
        for &s in &p.slab_rows {
            // a wall bottom attached to a slab is read from the slab's outline
            p.walls
                .extend(many(db, "SELECT wall FROM wall_attach WHERE slab=?1", s)?);
            p.comps
                .extend(many(db, "SELECT id FROM comp WHERE host_slab=?1", s)?);
        }
        for &w in &p.walls {
            let now = one(db, "SELECT level FROM wall WHERE id=?1", w)?;
            if let (Some(n), Some(o)) = (now, idx.wall_level.get(&w).copied()) {
                if n != o {
                    p.moved.extend([n, o]);
                }
            }
            for l in now.into_iter().chain(idx.wall_level.get(&w).copied()) {
                p.rejoin.entry(l).or_default().insert(w);
            }
            p.now.push((Map::Wall, w, now));
            p.comps
                .extend(many(db, "SELECT id FROM comp WHERE host_wall=?1", w)?);
        }
        p.slabs = p.slab_rows.clone();
        if members {
            let recs: Vec<MemberRec> = crate::structure::build_all(db)?
                .iter()
                .map(|b| MemberRec::new(b, b.cut))
                .collect();
            let old: HashMap<(&'static str, i64), &MemberRec> =
                idx.members.iter().map(|r| (r.key(), r)).collect();
            let keys: HashSet<(&'static str, i64)> = recs.iter().map(MemberRec::key).collect();
            let mut changed: Vec<&MemberRec> = vec![];
            for r in &recs {
                match old.get(&r.key()) {
                    Some(o) if o.same(r) => {}
                    o => {
                        p.redo.insert(r.key());
                        changed.push(r);
                        changed.extend(o.copied());
                    }
                }
            }
            for o in &idx.members {
                if !keys.contains(&o.key()) {
                    p.gone.push(o.key());
                    changed.push(o);
                }
            }
            if changed.iter().any(|r| r.structural && r.plan.is_some()) {
                load_slabs(&mut idx.slabs, db)?;
            }
            for r in &changed {
                let Some((_, _, bb, zr)) = &r.plan else {
                    continue;
                };
                p.col_hits.push((*bb, *zr));
                if r.structural {
                    for (s, sb) in idx.slabs.iter().flatten() {
                        if boxes_meet(*bb, sb.0) && zr.0 <= sb.2 && sb.1 <= zr.1 {
                            p.slabs.insert(*s);
                        }
                    }
                }
            }
            p.members = Some(recs);
        }
        // slab -> bars that take a deduction from it (old and new outline)
        let any_members = p
            .members
            .as_ref()
            .map_or(!idx.members.is_empty(), |m| !m.is_empty());
        if !p.slabs.is_empty() {
            p.geo = crate::structure::flat_slabs(db, Some(&p.slabs))?;
            if any_members && !p.slab_rows.is_empty() {
                if !had_slabs {
                    // there are no old outlines, so bars under the slab's previous position cannot be found
                    return Ok(None);
                }
                boxes.extend(
                    p.geo
                        .iter()
                        .filter(|s| p.slab_rows.contains(&s.0))
                        .filter_map(|s| s.1.first().map(|r| bbox(r))),
                );
                let mem = p.members.as_deref().unwrap_or(&idx.members);
                for r in mem {
                    if let Some(cb) = r.cut_box() {
                        if boxes.iter().any(|b| boxes_meet(cb, *b)) {
                            p.redo.insert(r.key());
                        }
                    }
                }
            }
            if let Some(m) = idx.slabs.as_mut() {
                for s in &p.slabs {
                    m.remove(s);
                }
                for s in &p.geo {
                    if let Some(b) = slab_box(s) {
                        m.insert(s.0, b);
                    }
                }
            }
        }
        if !p.redo.is_empty() {
            load_slabs(&mut idx.slabs, db)?;
        }
        p.room_levels.extend(p.rejoin.keys().copied());
        Ok(Some(p))
    }
}

fn run(db: &Connection, mut p: Plan, idx: &mut Index) -> Result<Inv, Error> {
    // walls
    for &w in &p.walls {
        db.prepare_cached("DELETE FROM wall_qty WHERE wall=?1")?
            .execute([w])?;
    }
    let layers = rewrite_walls(db, &p, idx)?;
    // rooms
    for &r in &p.rooms {
        db.prepare_cached("DELETE FROM room_qty WHERE room=?1")?
            .execute([r])?;
    }
    if !p.room_levels.is_empty() {
        let mut rp = vec![];
        for (l, e) in crate::walls::levels(db)? {
            if !p.room_levels.contains(&l) {
                continue;
            }
            if let Some(c) = idx.levels.get(&l) {
                rp.push(crate::walls::pieces_polys(l, e, &c.pieces));
            } else {
                let lw = crate::walls::level_walls(db, l, e)?;
                rp.push((l, e, lw.polys, lw.faces));
            }
        }
        crate::rooms::regenerate(db, &rp, true)?;
    }
    // slabs
    let fresh = if p.slabs.is_empty() {
        HashSet::new()
    } else {
        crate::slabs::regenerate(db, Some(&p.slabs))?
    };
    // bars: deductions into fresh wall and slab rows, rows of changed bars
    let geo_changed = !p.walls.is_empty() || !p.slabs.is_empty() || p.members.is_some();
    let ext = geo_changed && crate::struct_ext::any_rows(db)?;
    let shafts = geo_changed && (exists(db, "shaft")? || exists(db, "ceiling")?);
    let mem: &[MemberRec] = p.members.as_deref().unwrap_or(&idx.members);
    if shafts {
        // shafts subtract from beam rows: the rows of all beams are fresh
        p.redo
            .extend(mem.iter().filter(|r| r.kind == "beam").map(MemberRec::key));
        if !p.redo.is_empty() {
            load_slabs(&mut idx.slabs, db)?;
        }
    }
    for (kind, id) in &p.gone {
        db.prepare_cached("DELETE FROM member_qty WHERE kind=?1 AND ref=?2")?
            .execute(rusqlite::params![kind, id])?;
    }
    let mut built: Vec<Built> = vec![];
    if !mem.is_empty() && (!layers.is_empty() || !fresh.is_empty() || !p.redo.is_empty()) {
        built = mem.iter().map(MemberRec::built).collect();
        let plans: Vec<(usize, &ColPlan)> = mem
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.plan.as_ref().map(|pl| (i, pl)))
            .collect();
        let sub: Vec<&FlatSlab> = p.geo.iter().filter(|s| fresh.contains(&s.0)).collect();
        let mut want = BTreeSet::new();
        for r in mem.iter().filter(|r| p.redo.contains(&r.key())) {
            if let Some(cb) = r.cut_box() {
                for (s, sb) in idx.slabs.iter().flatten() {
                    if boxes_meet(cb, sb.0) {
                        want.insert(*s);
                    }
                }
            }
        }
        let cutgeo = if want.is_empty() {
            vec![]
        } else {
            crate::structure::flat_slabs(db, Some(&want))?
        };
        let cut: Vec<&FlatSlab> = cutgeo.iter().collect();
        let walls: Vec<&[WallLayer]> = layers.iter().map(|l| &l[..]).collect();
        crate::structure::apply(db, &mut built, &plans, &walls, &sub, &cut, Some(&p.redo))?;
    }
    if geo_changed && exists(db, "roof")? {
        crate::roofs::regenerate(db)?;
    }
    let inv = if ext {
        if built.is_empty() {
            built = mem.iter().map(MemberRec::built).collect();
        }
        crate::struct_ext::regenerate(db, built)?
    } else if !p.conns.is_empty() {
        // members as they were: only the connections whose rows changed are rebuilt
        let _s = crate::perf::span("regen.partial.conns");
        if built.is_empty() {
            built = mem.iter().map(MemberRec::built).collect();
        }
        let mut solids = vec![];
        let inv = crate::steel_conn::sync_ids(db, &built, &p.conns, &mut solids)?;
        if !(geo_changed && exists(db, "solid")?) {
            crate::solids::refresh(db, &solids)?;
        }
        inv
    } else {
        Inv::default()
    };
    if shafts {
        crate::shafts::regenerate(db)?;
    }
    if p.stairs {
        crate::stairs::regenerate(db)?;
    }
    if !p.comps.is_empty() {
        crate::components::regenerate_ids(db, &p.comps)?;
    }
    if geo_changed && exists(db, "solid")? {
        crate::solids::regenerate(db)?;
    }
    if p.dims || !p.walls.is_empty() {
        crate::annot::regenerate(db)?;
    }
    crate::measure::refresh(
        db,
        &crate::measure::Touch {
            walls: &p.walls,
            slabs: &p.slab_rows,
            rooms: &p.rooms,
            rooms_any: !p.room_levels.is_empty() || !p.walls.is_empty() || !p.slab_rows.is_empty(),
            measures: &p.measures,
        },
    )?;
    // index
    for &(m, id, now) in &p.now {
        let map = idx.map(m);
        match now {
            Some(v) => map.insert(id, v),
            None => map.remove(&id),
        };
    }
    if let Some(m) = p.members {
        idx.members = m;
    }
    Ok(inv)
}

/// Wall rows: the neighbourhood of changed ones anew, those touched by columns reset to the
/// pre-deduction values. Returns the wall layers with fresh rows (column deductions go into them).
fn rewrite_walls(db: &Connection, p: &Plan, idx: &mut Index) -> Result<Vec<Vec<WallLayer>>, Error> {
    let mut out: Vec<Vec<WallLayer>> = vec![];
    if !p.rejoin.is_empty() {
        let mut whole = BTreeSet::new();
        for (l, e) in crate::walls::levels(db)? {
            let Some(dirty) = p.rejoin.get(&l) else {
                continue;
            };
            let near = match idx.levels.get_mut(&l) {
                Some(c) if !c.overlaps && !p.moved.contains(&l) => {
                    let near = crate::walls::regenerate_near(db, l, e, dirty, &mut c.pieces)?;
                    if near.is_some() {
                        c.env = envelope(&c.pieces);
                        NEAR.with(|n| n.set(n.get() + 1));
                    }
                    near
                }
                _ => None,
            };
            match near {
                Some(layers) => out.push(layers),
                None => {
                    whole.insert(l);
                }
            }
        }
        if !whole.is_empty() {
            for r in crate::walls::regenerate(db, Some(&whole))? {
                out.push(r.layers.clone());
                idx.levels.insert(r.polys.0, LevelCache::new(r));
            }
        }
    }
    let done: HashSet<i64> = out.iter().flatten().map(|l| l.0).collect();
    let mut hit = BTreeSet::new();
    for (bb, zr) in &p.col_hits {
        for c in idx.levels.values() {
            let Some((eb, z0, z1)) = c.env else { continue };
            if !boxes_meet(*bb, eb) || zr.1 < z0 || z1 < zr.0 {
                continue;
            }
            for (id, w) in &c.pieces {
                // a ring with a corner end extends beyond the wall's box: compare by rings
                let met = w.layers.iter().any(|l| {
                    zr.0 <= l.4
                        && l.3 <= zr.1
                        && l.2
                            .iter()
                            .any(|r| !r.is_empty() && boxes_meet(*bb, bbox(r)))
                });
                if met && !done.contains(id) {
                    hit.insert(*id);
                }
            }
        }
    }
    if !hit.is_empty() {
        let mut layers = vec![];
        for c in idx.levels.values() {
            for (id, w) in c.pieces.iter().filter(|(id, _)| hit.contains(id)) {
                crate::walls::restore_gross(db, *id, w)?;
                layers.extend(w.layers.iter().cloned());
            }
        }
        out.push(layers);
    }
    Ok(out)
}

/// Rows of derived tables: ordered, numbers are compared with a tolerance.
type Snapshot = Vec<(&'static str, Vec<Vec<Value>>)>;

fn snapshot(db: &Connection) -> Result<Snapshot, Error> {
    let mut out = vec![];
    for &t in DERIVED {
        let Ok(mut st) = db.prepare(&format!("SELECT * FROM {t}")) else {
            continue;
        };
        let n = st.column_count();
        let mut rows: Vec<Vec<Value>> = st
            .query_map([], |r| {
                (0..n)
                    .map(|i| r.get::<_, Value>(i))
                    .collect::<rusqlite::Result<Vec<Value>>>()
            })?
            .collect::<Result<_, _>>()?;
        rows.sort_by_cached_key(|r| r.iter().map(sort_key).collect::<Vec<String>>());
        out.push((t, rows));
    }
    Ok(out)
}

fn sort_key(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => format!("{i:020}"),
        Value::Real(f) => format!("{:.3}", f + 0.0),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => format!("{b:?}"),
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Real(x), Value::Real(y)) => {
            (x - y).abs() <= 1e-7 * x.abs().max(y.abs()).max(1.0) || x.is_nan() && y.is_nan()
        }
        (Value::Real(x), Value::Integer(i)) | (Value::Integer(i), Value::Real(x)) => {
            (x - *i as f64).abs() <= 1e-7
        }
        _ => a == b,
    }
}

/// The first difference between two snapshots.
fn diff(a: &Snapshot, b: &Snapshot) -> Option<String> {
    for ((t, ra), (_, rb)) in a.iter().zip(b) {
        if ra.len() != rb.len() {
            return Some(format!(
                "{t}: {} rows incremental vs {} full",
                ra.len(),
                rb.len()
            ));
        }
        for (x, y) in ra.iter().zip(rb) {
            if x.len() != y.len() || x.iter().zip(y).any(|(u, v)| !same(u, v)) {
                return Some(format!("{t}: {x:?} incremental vs {y:?} full"));
            }
        }
    }
    None
}

/// Compares a partial regeneration with a full one: the full result stays in the database.
fn verify(db: &Connection) -> Result<(), Error> {
    let a = snapshot(db)?;
    full_in(db, None, true)?;
    let b = snapshot(db)?;
    if let Some(d) = diff(&a, &b) {
        if cfg!(debug_assertions) {
            panic!("incremental regenerate differs from full: {d}");
        }
        eprintln!("ddc-model: incremental regenerate differs from full: {d}");
    }
    Ok(())
}

#[cfg(test)]
#[path = "regen_tests.rs"]
mod tests;
