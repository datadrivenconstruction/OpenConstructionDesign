// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Generator layouts as plan steps. The elements are pure in the layout and what the model holds,
//! so a variant replays to the same ids; types and a top level the model lacks are added first.

use super::exec::{ModelStep, NewResolved};
use super::plan::{compile_create, push, CompileCx, ElRef, IdOrNew, NewElem, PlanError};
use ddc_gen::hangar::{HallCrane, HallOutfit, CRANE_FAMILY};
use ddc_gen::project::structure::System;
use ddc_gen::{GLayout, WallRole};
use ddc_model::{Edit, LayerSpec, MemberShape, ParamValue};
use std::collections::{BTreeSet, HashMap};

const STOREY: f64 = 3.0;
const NEW_WALL_T: f64 = 0.2;
const NEW_SLAB_T: f64 = 0.2;
const MATERIAL: &str = "Concrete";

/// What the model gave the layout: `None` and empty fall back to the first type and no renames.
#[derive(Default)]
pub struct Bound {
    /// Wall types, thinnest first.
    pub wall_types: Vec<i64>,
    pub slab_type: Option<i64>,
    /// Member type of each column of the layout.
    pub member_types: Vec<i64>,
    /// Component type of each fixture of the layout; fixtures past its end are left out.
    pub fixture_types: Vec<i64>,
    /// Top level and the offset from it that makes the storey exact.
    pub top: Option<(i64, f64)>,
    pub taken_grids: BTreeSet<String>,
}

pub(super) fn type_ref(table: &str, id: i64) -> ElRef {
    ElRef {
        cat: table.to_string(),
        id: IdOrNew::Id(id),
    }
}

fn wall_ref(b: &Bound, role: WallRole) -> Option<ElRef> {
    let n = b.wall_types.len();
    if n == 0 {
        return None;
    }
    let i = match role {
        WallRole::Exterior | WallRole::Retaining => n - 1,
        WallRole::Partition => 0,
        WallRole::Party | WallRole::Corridor => (n - 1).div_ceil(2),
    };
    Some(type_ref("wall_type", b.wall_types[i]))
}

/// A name no grid has yet, so a second run on the same model still adds its axes.
pub(super) fn free_grid(taken: &mut BTreeSet<String>, name: &str) -> String {
    let mut out = name.to_string();
    let mut k = 2;
    while !taken.insert(out.clone()) {
        out = format!("{name}-{k}");
        k += 1;
    }
    out
}

/// Numbers within an apartment read `apartment.room`; other rooms take a running number.
fn room_numbers(l: &GLayout) -> Vec<String> {
    let mut per: HashMap<u32, u32> = HashMap::new();
    let mut other = 0;
    l.rooms
        .iter()
        .map(|r| match r.apartment {
            Some(a) => {
                let k = per.entry(a).or_insert(0);
                *k += 1;
                format!("{a}.{k}")
            }
            None => {
                other += 1;
                other.to_string()
            }
        })
        .collect()
}

/// Element `i` of the result takes slot `slot0 + i`: walls first, so openings can name them.
pub fn layout_items(l: &GLayout, level: i64, slot0: u32, b: &Bound) -> Vec<NewElem> {
    let height = if l.height > 0.0 { l.height } else { STOREY };
    let mut out = Vec::with_capacity(slot_count(l) as usize);
    for w in &l.walls {
        out.push(NewElem::Wall {
            level,
            a: w.a,
            b: w.b,
            bulge: w.bulge,
            base_offset: 0.0,
            height,
            wtype: wall_ref(b, w.role),
        });
    }
    for o in &l.openings {
        out.push(NewElem::Opening {
            wall: ElRef::new("wall", slot0 + o.wall as u32),
            along: o.along,
            width: o.width,
            height: o.height,
            sill: o.sill,
            kind: if o.door { "door" } else { "window" }.to_string(),
        });
    }
    if l.slab.len() >= 3 {
        let stype = b.slab_type.map(|id| type_ref("slab_type", id));
        out.push(NewElem::Slab {
            level,
            stype,
            offset: 0.0,
            outer: l.slab.clone(),
            holes: l.slab_holes.clone(),
        });
    }
    for (r, number) in l.rooms.iter().zip(room_numbers(l)) {
        out.push(NewElem::Room {
            level,
            name: r.name.clone(),
            number,
            point: r.point,
            height,
        });
    }
    let mut taken = b.taken_grids.clone();
    for g in &l.grids {
        out.push(NewElem::Grid {
            name: free_grid(&mut taken, &g.name),
            a: g.a,
            b: g.b,
        });
    }
    let (top_level, top_offset) = match b.top {
        Some((t, off)) => (Some(t), off),
        None => (None, 0.0),
    };
    for (i, c) in l.columns.iter().enumerate() {
        out.push(NewElem::Column {
            level,
            top_level,
            mtype: b.member_types.get(i).map(|id| type_ref("member_type", *id)),
            at: c.at,
            base_offset: 0.0,
            top_offset,
            rotation: c.rotation,
            structural: true,
            top_at: None,
        });
    }
    if let Some(top) = top_level {
        for s in &l.stairs {
            out.push(NewElem::GenStair {
                level,
                top_level: top,
                top_offset,
                origin: s.origin,
                angle: s.angle,
                shape: s.shape,
                width: s.width,
                max_riser: s.max_riser,
                landing: s.landing,
            });
        }
        for s in &l.shafts {
            out.push(NewElem::Shaft {
                level,
                top_level: top,
                top_offset,
                ring: s.ring.clone(),
            });
        }
    }
    for (f, id) in l.fixtures.iter().zip(&b.fixture_types) {
        let (at, rotation, host) = match f.wall.and_then(|k| l.walls.get(k).map(|w| (k, w))) {
            // a hosted family takes its point along the wall axis and its turn off the wall
            // (on an arc wall: the distance along the arc, the offset off it at that point)
            Some((k, w)) => {
                let s = w.along(f.at);
                let (o, (tx, ty)) = (w.at(s), w.tangent(s));
                let (px, py) = (f.at.0 - o.0, f.at.1 - o.1);
                let at = (s, py * tx - px * ty, 0.0);
                let host = ElRef::new("wall", slot0 + k as u32);
                (at, f.rotation - ty.atan2(tx), Some(host))
            }
            None => ((f.at.0, f.at.1, f.z), f.rotation, None),
        };
        out.push(NewElem::Comp {
            level,
            ctype: type_ref("comp_type", *id),
            at,
            rotation,
            host,
            scale: f.scale,
            min_clear: f.min_clear,
        });
    }
    out
}

/// Slots a layout needs, for reserving room across variants.
pub fn slot_count(l: &GLayout) -> u32 {
    (l.walls.len()
        + l.openings.len()
        + 1
        + l.rooms.len()
        + l.grids.len()
        + l.columns.len()
        + l.stairs.len()
        + l.shafts.len()) as u32
}

pub(super) fn new_type(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    table: &str,
    elem: NewResolved,
) -> Result<i64, PlanError> {
    let ids = push(
        cx,
        out,
        ModelStep::Add {
            elem: Box::new(elem),
            expect: 0,
        },
    )?;
    let id = *ids.first().ok_or("a type was not created")?;
    cx.declared.insert((table.to_string(), id));
    Ok(id)
}

fn wall_types(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>) -> Result<Vec<i64>, PlanError> {
    let ids = {
        let mut st = cx.ed.model.sql().prepare(
            "SELECT t.id FROM wall_type t LEFT JOIN wall_type_layer l ON l.wtype=t.id GROUP BY t.id ORDER BY COALESCE(SUM(l.thickness),0), t.id",
        )?;
        let v = st
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<i64>, _>>()?;
        v
    };
    if !ids.is_empty() {
        return Ok(ids);
    }
    let layers = vec![LayerSpec {
        material: MATERIAL.to_string(),
        thickness: NEW_WALL_T,
    }];
    Ok(vec![new_type(
        cx,
        out,
        "wall_type",
        NewResolved::WallType {
            name: "Gen Wall 200".to_string(),
            layers,
        },
    )?])
}

fn slab_type(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>) -> Result<i64, PlanError> {
    let first: Option<i64> =
        cx.ed
            .model
            .sql()
            .query_row("SELECT min(id) FROM slab_type", [], |r| r.get(0))?;
    if let Some(id) = first {
        return Ok(id);
    }
    let layers = vec![LayerSpec {
        material: MATERIAL.to_string(),
        thickness: NEW_SLAB_T,
    }];
    new_type(
        cx,
        out,
        "slab_type",
        NewResolved::SlabType {
            name: "Gen Slab 200".to_string(),
            layers,
        },
    )
}

/// One type per section in the structural system's material, found by name so a later run
/// reuses it.
pub(super) fn member_types(
    cx: &mut CompileCx<'_>,
    l: &GLayout,
    out: &mut Vec<ModelStep>,
    sys: &System,
) -> Result<Vec<i64>, PlanError> {
    let mut known: HashMap<String, i64> = HashMap::new();
    let mut ids = Vec::with_capacity(l.columns.len());
    let mut material_row = false;
    for c in &l.columns {
        let (b, h) = c.section;
        let name = format!("{} {:.0}x{:.0}", sys.column_prefix, b * 1000.0, h * 1000.0);
        if let Some(id) = known.get(&name) {
            ids.push(*id);
            continue;
        }
        let found =
            cx.ed
                .model
                .sql()
                .query_row("SELECT id FROM member_type WHERE name=?1", [&name], |r| {
                    r.get::<_, i64>(0)
                });
        let id = match found {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // the column material gets its row (colour, density) when the model has none
                if !material_row && cx.ed.model.material(&sys.column_material)?.id == 0 {
                    material_row = true;
                    let spec = ddc_model::material_defaults(&sys.column_material);
                    let edit = Edit::Material { name: sys.column_material.clone(), spec };
                    push(cx, out, ModelStep::Edits(vec![edit]))?;
                }
                let elem = NewResolved::MemberType {
                    name: name.clone(),
                    shape: MemberShape::Rect { b, h },
                    material: sys.column_material.clone(),
                };
                new_type(cx, out, "member_type", elem)?
            }
            Err(e) => return Err(e.into()),
        };
        known.insert(name, id);
        ids.push(id);
    }
    Ok(ids)
}

/// The next level up with the offset that makes the storey exact, or a new level when none is above.
fn top_level(
    cx: &mut CompileCx<'_>,
    level: i64,
    height: f64,
    out: &mut Vec<ModelStep>,
) -> Result<(i64, f64), PlanError> {
    let base: f64 =
        cx.ed
            .model
            .sql()
            .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                r.get(0)
            })?;
    let above = cx.ed.model.sql().query_row(
        "SELECT id, elevation FROM level WHERE elevation>?1 ORDER BY elevation, id LIMIT 1",
        [base],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    );
    match above {
        Ok((id, elevation)) => return Ok((id, base + height - elevation)),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => return Err(e.into()),
    }
    let count: i64 = cx
        .ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM level", [], |r| r.get(0))?;
    let mut n = count + 1;
    let name = loop {
        let name = format!("Gen Level {n}");
        if !level_taken(cx, &name)? {
            break name;
        }
        n += 1;
    };
    Ok((new_level(cx, out, &name, base + height)?, 0.0))
}

pub(super) fn level_taken(cx: &CompileCx<'_>, name: &str) -> Result<bool, PlanError> {
    let sql = "SELECT EXISTS(SELECT 1 FROM level WHERE name=?1)";
    Ok(cx.ed.model.sql().query_row(sql, [name], |r| r.get(0))?)
}

/// Adds a level under a name no level has; returns its id.
pub(super) fn new_level(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    name: &str,
    elevation: f64,
) -> Result<i64, PlanError> {
    let edit = Edit::NewLevel {
        name: name.to_string(),
        elevation,
    };
    push(cx, out, ModelStep::Edits(vec![edit]))?;
    let sql = "SELECT id FROM level WHERE name=?1";
    let id: i64 = cx.ed.model.sql().query_row(sql, [name], |r| r.get(0))?;
    cx.declared.insert(("level".to_string(), id));
    Ok(id)
}

/// Compile a layout: what it needs from the model first, then its elements.
pub fn compile_layout(
    cx: &mut CompileCx<'_>,
    l: &GLayout,
    level: i64,
    slot0: u32,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let mut b = Bound::default();
    if !l.walls.is_empty() {
        b.wall_types = wall_types(cx, out)?;
    }
    if l.slab.len() >= 3 {
        b.slab_type = Some(slab_type(cx, out)?);
    }
    let sys = ddc_gen::project::structure::system(ddc_gen::project::Structure::Concrete);
    b.member_types = member_types(cx, l, out, sys)?;
    if !l.columns.is_empty() || !l.stairs.is_empty() || !l.shafts.is_empty() {
        let height = if l.height > 0.0 { l.height } else { STOREY };
        b.top = Some(top_level(cx, level, height, out)?);
    }
    if !l.grids.is_empty() {
        let mut st = cx.ed.model.sql().prepare("SELECT name FROM grid")?;
        let names = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        b.taken_grids = names;
    }
    let items = layout_items(l, level, slot0, &b);
    compile_create(cx, slot0, &items, out)
}

/// Slots the outfit of a hangar takes after its frame.
pub fn hall_slots(h: &HallOutfit) -> u32 {
    1 + slot_count(&h.gable) + h.annex.iter().map(slot_count).sum::<u32>() + h.cranes.len() as u32
}

/// The library family `name` in the model, loaded first when the model lacks it.
fn library_family(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, name: &str) -> Result<i64, PlanError> {
    if let Some(id) = cx.ed.model.family_id(name)? {
        return Ok(id);
    }
    let head = format!("family {name}");
    let def = ddc_model::library_sources()
        .into_iter()
        .find(|s| s.lines().any(|x| x.trim() == head))
        .ok_or_else(|| PlanError(format!("no library family {name}")))?;
    push(cx, out, ModelStep::Family { def: def.to_string(), overwrite: false })?;
    cx.ed.model.family_id(name)?.ok_or_else(|| PlanError(format!("family {name} was not loaded")))
}

/// The crane type of a span and capacity, found by name so a later run reuses it.
fn crane_type(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, c: &HallCrane) -> Result<i64, PlanError> {
    let fam = library_family(cx, out, CRANE_FAMILY)?;
    let name = format!("{} t {:.2} m", c.capacity, c.span);
    let sql = "SELECT id FROM comp_type WHERE family=?1 AND name=?2";
    match cx.ed.model.sql().query_row(sql, rusqlite::params![fam, name], |r| r.get::<_, i64>(0)) {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => return Err(e.into()),
    }
    let values = vec![("Span".to_string(), ParamValue::Num(c.span)), ("Capacity".to_string(), ParamValue::Num(c.capacity))];
    new_type(cx, out, "comp_type", NewResolved::CompType { family: fam, name, values })
}

/// The level at `elevation`, added under a free name when the model has none there.
fn level_at(cx: &mut CompileCx<'_>, out: &mut Vec<ModelStep>, elevation: f64) -> Result<i64, PlanError> {
    let sql = "SELECT id FROM level WHERE abs(elevation - ?1) < 1e-6 ORDER BY id LIMIT 1";
    match cx.ed.model.sql().query_row(sql, [elevation], |r| r.get::<_, i64>(0)) {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(e) => return Err(e.into()),
    }
    let mut n = 1;
    let name = loop {
        let name = format!("Annex {n}");
        if !level_taken(cx, &name)? {
            break name;
        }
        n += 1;
    };
    new_level(cx, out, &name, elevation)
}

/// The outfit of a hangar on its frame's level: the roof cladding over the top chords, the gable
/// wall with the door opening, then the annex storeys, each on the level its height reaches
/// (made first, so the stair below lands on it), then the overhead cranes on the runway.
pub fn compile_hall(
    cx: &mut CompileCx<'_>,
    h: &HallOutfit,
    level: i64,
    slot0: u32,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let stype = slab_type(cx, out)?;
    let roof = NewElem::Roof {
        level,
        stype: Some(type_ref("slab_type", stype)),
        offset: h.roof_z,
        overhang: h.overhang,
        ring: h.roof.clone(),
        slope: None,
        edges: vec![],
        vault: None,
        eave: None,
    };
    compile_create(cx, slot0, &[roof], out)?;
    let mut slot = slot0 + 1;
    compile_layout(cx, &h.gable, level, slot, out)?;
    slot += slot_count(&h.gable);
    let sql = "SELECT elevation FROM level WHERE id=?1";
    let mut z: f64 = cx.ed.model.sql().query_row(sql, [level], |r| r.get(0))?;
    let mut levels = vec![level];
    for st in h.annex.iter().take(h.annex.len().saturating_sub(1)) {
        z += st.height;
        levels.push(level_at(cx, out, z)?);
    }
    for (st, lv) in h.annex.iter().zip(levels) {
        compile_layout(cx, st, lv, slot, out)?;
        slot += slot_count(st);
    }
    let mut cranes = Vec::with_capacity(h.cranes.len());
    for c in &h.cranes {
        let t = crane_type(cx, out, c)?;
        cranes.push(NewElem::Comp { level, ctype: type_ref("comp_type", t), at: c.at, rotation: 0.0, host: None, scale: 1.0, min_clear: 0.0 });
    }
    if cranes.is_empty() {
        return Ok(());
    }
    compile_create(cx, slot, &cranes, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_gen::{GGrid, GOpening, GRoom, GWall, WallRole};

    #[test]
    fn openings_name_walls_by_slot() {
        let l = GLayout {
            walls: vec![
                GWall::line((0.0, 0.0), (4.0, 0.0), WallRole::Exterior),
                GWall::line((4.0, 0.0), (4.0, 4.0), WallRole::Exterior),
            ],
            openings: vec![GOpening {
                wall: 1,
                along: 1.0,
                door: true,
                width: 0.9,
                height: 2.1,
                sill: 0.0,
                ty: None,
            }],
            ..GLayout::default()
        };
        let v = layout_items(&l, 3, 100, &Bound::default());
        assert_eq!(v.len(), 3);
        match &v[2] {
            NewElem::Opening { wall, kind, .. } => {
                assert_eq!(wall.slot(), Some(101));
                assert_eq!(kind, "door");
            }
            other => panic!("{other:?}"),
        }
        assert!(slot_count(&l) as usize >= v.len());
    }

    #[test]
    fn roles_take_types_by_thickness() {
        let b = Bound {
            wall_types: vec![1, 2, 3],
            ..Bound::default()
        };
        let id = |r| match wall_ref(&b, r).unwrap().id {
            IdOrNew::Id(i) => i,
            IdOrNew::New(_) => panic!(),
        };
        assert_eq!(id(WallRole::Partition), 1);
        assert_eq!(id(WallRole::Corridor), 2);
        assert_eq!(id(WallRole::Party), 2);
        assert_eq!(id(WallRole::Exterior), 3);
    }

    #[test]
    fn room_numbers_follow_apartments_and_grid_names_stay_free() {
        let room = |a| GRoom {
            name: "R".into(),
            apartment: a,
            point: (0.0, 0.0),
            polygon: vec![],
        };
        let l = GLayout {
            rooms: vec![room(None), room(Some(4)), room(Some(4)), room(Some(5))],
            ..GLayout::default()
        };
        assert_eq!(room_numbers(&l), ["1", "4.1", "4.2", "5.1"]);
        let grids = GLayout {
            grids: vec![GGrid {
                name: "A".into(),
                a: (0.0, 0.0),
                b: (1.0, 0.0),
            }],
            ..GLayout::default()
        };
        let b = Bound {
            taken_grids: ["A".to_string()].into_iter().collect(),
            ..Bound::default()
        };
        match &layout_items(&grids, 1, 0, &b)[0] {
            NewElem::Grid { name, .. } => assert_eq!(name, "A-2"),
            other => panic!("{other:?}"),
        }
    }
}
