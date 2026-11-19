//! Model steps and Apply: the same steps run on the scratch copy and, in one undo step, on the
//! live model. The `expect` ids prove both ran the same way.

use super::option_exec::committed;
use super::plan::CompiledPlan;
use super::AiError;
use crate::editor::{Editor, OpFn};
use ddc_model::{
    BeamSpec, CeilingSpec, ClashFix, ColumnSpec, CompSpec, CurtainSpec, Edit, ElKey, Error,
    GridSpec, LayerSpec, MemberShape, Model, OpeningSpec, ParamValue, RoofSpec, RoomSpec, RunSpec,
    ShaftSpec, SlabSpec, StairSpec, Swing, WallSpec,
};
use std::sync::Arc;

/// One new element or type with every reference resolved to an id.
#[derive(Clone)]
pub enum NewResolved {
    Opening {
        wall: i64,
        spec: OpeningSpec,
        swing: Swing,
    },
    Room(RoomSpec),
    Ceiling(CeilingSpec),
    Roof(RoofSpec),
    Stair(StairSpec),
    Mep(RunSpec),
    /// The wall and the height of its solid first row, 0 for none.
    Curtain(CurtainSpec, f64),
    Shaft(ShaftSpec),
    Grid(GridSpec),
    Railing(ddc_model::RailingSpec),
    SteelConn(ddc_model::SteelConnSpec),
    StripFooting(ddc_model::StripFootingSpec),
    WallType {
        name: String,
        layers: Vec<LayerSpec>,
    },
    SlabType {
        name: String,
        layers: Vec<LayerSpec>,
    },
    MemberType {
        name: String,
        shape: MemberShape,
        material: String,
    },
    CompType {
        family: i64,
        name: String,
        values: Vec<(String, ParamValue)>,
    },
}

#[derive(Clone)]
pub enum ModelStep {
    Edits(Vec<Edit>),
    Walls {
        specs: Vec<WallSpec>,
        expect: Vec<i64>,
    },
    /// A run of walls of which some are arcs (`bulges[i]`, 0 straight): one model operation.
    ArcWalls {
        specs: Vec<WallSpec>,
        bulges: Vec<f64>,
        expect: Vec<i64>,
    },
    Slabs {
        specs: Vec<SlabSpec>,
        expect: Vec<i64>,
    },
    Columns {
        specs: Vec<ColumnSpec>,
        expect: Vec<i64>,
    },
    Beams {
        specs: Vec<BeamSpec>,
        expect: Vec<i64>,
    },
    Comps {
        specs: Vec<CompSpec>,
        expect: Vec<i64>,
    },
    /// A run of openings, host walls resolved: one model operation.
    Openings {
        specs: Vec<(i64, OpeningSpec, Swing)>,
        expect: Vec<i64>,
    },
    /// A run of rooms: one model operation.
    Rooms {
        specs: Vec<RoomSpec>,
        expect: Vec<i64>,
    },
    /// Chains of straight walls each turned into one spline wall (their rows become pieces).
    Splines {
        specs: Vec<(Vec<i64>, ddc_model::SplineWallSpec)>,
        expect: Vec<i64>,
    },
    Add {
        elem: Box<NewResolved>,
        expect: i64,
    },
    Delete {
        table: &'static str,
        id: i64,
    },
    /// A wall let go of the roof trim for good: its top stays as built.
    Release {
        id: i64,
    },
    Comp {
        id: i64,
        spec: CompSpec,
    },
    Family {
        def: String,
        overwrite: bool,
    },
    ClashFix(ClashFix),
    /// A resolved wall type edit or import.
    TypeLib(ddc_model::TypeLibStep),
    /// A family defined from structured JSON; it lands as a draft.
    DefineFamily { json: String },
    /// Geometry ops of one plan item as an envelope; `made` is what the ops created, in order.
    Geometry {
        env: Box<ddc_ops::Envelope>,
        made: Vec<(String, i64)>,
    },
    /// A script item: its envelope (slots from `slot0`), then the recipe and its links.
    Script {
        env: Box<ddc_ops::Envelope>,
        made: Vec<(String, i64)>,
        slot0: u32,
        item: std::sync::Arc<super::script_tools::ScriptItem>,
    },
    /// A routed network: systems, sleeves, holes, runs, links and circuits as one build.
    Mep {
        build: Box<ddc_model::MepBuild>,
        expect: Vec<(&'static str, i64)>,
    },
    /// Fixtures and devices placed from the library.
    Devices {
        specs: Vec<ddc_model::DeviceSpec>,
        expect: Vec<i64>,
    },
    /// Properties set (or with `None` removed) on elements or records.
    Props(Vec<PropSet>),
    /// A stair, shaft, grid or roof brought to a new spec in place: the id stays.
    Update {
        id: i64,
        elem: Box<NewResolved>,
    },
    /// Room, door and window tags on these levels, for a drawing set; unmarked doors and
    /// windows marked by type with these prefixes first.
    DrawingTags(Vec<i64>, ddc_model::MarkPrefix),
    /// Sheets of a drawing set, updated in place on a second run.
    DrawingSet(Box<ddc_model::DsetPlan>),
    /// A ribbon modify, level or room tool (`modify_plan`); `expect`: the rows it created.
    Tool {
        step: super::modify_plan::ToolStep,
        expect: super::modify_plan::Made,
    },
    /// A generated steel frame on a level; `expect`: member types, columns, beams, braces.
    SteelFrame {
        level: i64,
        frame: Arc<ddc_geom::steel::lattice::Frame>,
        expect: Vec<(&'static str, i64)>,
        /// Also a connection at every free node of the frame (the generator's `joints`).
        joints: bool,
    },
}

/// The frame's rows, then its connections when `joints`. Both run inside the plan's `atomic`
/// (one undo step, all or nothing); nodes a connection cannot be built on are skipped, not fatal.
fn add_frame(m: &mut Model, level: i64, f: &ddc_geom::steel::lattice::Frame, joints: bool) -> Result<Vec<(&'static str, i64)>, Error> {
    let ids = m.add_steel_frame(level, f)?;
    let mut keys = steel_keys(&ids);
    if joints {
        keys.extend(m.connect_steel_frame(&ids)?.into_iter().map(|i| ("steel_conn", i)));
    }
    Ok(keys)
}

fn steel_keys(r: &ddc_model::SteelFrameIds) -> Vec<(&'static str, i64)> {
    let tag = |t: &'static str, v: &[i64]| v.iter().map(move |&i| (t, i)).collect::<Vec<_>>();
    [
        tag("member_type", &r.types),
        tag("col", &r.columns),
        tag("beam", &r.beams),
        tag("brace", &r.braces),
        tag("level", &r.levels),
        tag("slab_type", &r.slab_types),
        tag("slab", &r.slabs),
        tag("stair", &r.stairs),
        tag("iso_footing", &r.footings),
        tag("comp", &r.lifts),
        tag("railing", &r.railings),
        tag("ramp", &r.ramps),
        tag("comp", &r.insulators),
    ]
    .concat()
}

#[derive(Clone, Debug)]
pub struct PropSet {
    pub kind: String,
    pub id: i64,
    pub key: String,
    pub value: Option<(rusqlite::types::Value, String)>,
}

/// Categories of the rows the envelope creates, in op order (the ops that carry a slot).
fn geometry_cats(env: &ddc_ops::Envelope) -> Vec<ddc_ops::Cat> {
    use ddc_ops::Op;
    env.groups
        .iter()
        .flat_map(|g| g.ops.iter())
        .filter_map(|o| match o {
            Op::Geometry(g) => g.made_cat(),
            _ => None,
        })
        .collect()
}

/// Applies the envelope as one step and lists every row it made: first the ones that carry
/// a slot, in op order, then the rows without one (pattern and transform copies, boolean
/// tool bodies and results, void bodies).
fn geometry_rows(m: &mut Model, env: &ddc_ops::Envelope) -> Result<Vec<ElKey>, Error> {
    let r = m
        .apply_envelope_step(env)
        .map_err(|r| Error::Invalid(r.to_string()))?;
    let mut rows = Vec::with_capacity(r.created.len() + r.copied.len());
    for rf in r.created.iter().map(|(_, rf)| rf).chain(&r.copied) {
        let ddc_ops::Ref::Id { cat, id } = rf else {
            continue;
        };
        let table = ddc_model::ref_table(*cat)
            .ok_or_else(|| Error::Invalid(format!("a {} row cannot be listed", cat.key())))?;
        rows.push((table.to_string(), *id));
    }
    Ok(rows)
}

/// Table of a row the envelope created (a surface shell lives in `shell_elem`, not under its
/// category key).
fn made_table(cat: ddc_ops::Cat) -> String {
    ddc_model::ref_table(cat).unwrap_or(cat.key()).to_string()
}

fn add_one(m: &mut Model, e: &NewResolved) -> Result<i64, Error> {
    match e {
        NewResolved::Opening { wall, spec, swing } => m.add_opening_with(*wall, spec, *swing),
        NewResolved::Room(s) => m.add_room(s),
        NewResolved::Ceiling(s) => m.add_ceiling(s),
        NewResolved::Roof(s) => m.add_roof(s),
        NewResolved::Stair(s) => m.add_stair(s),
        NewResolved::Mep(s) => m.add_mep_run(s),
        NewResolved::Curtain(s, spandrel) => {
            let ids = m.add_curtain_walls(std::slice::from_ref(s))?;
            let id = ids
                .first()
                .copied()
                .ok_or_else(|| Error::Invalid("no curtain wall created".into()))?;
            solid_row(m, id, *spandrel, false)?;
            Ok(id)
        }
        NewResolved::Shaft(s) => m.add_shaft(s),
        NewResolved::Grid(s) => m.add_grid(s),
        NewResolved::Railing(s) => m.add_railing(s),
        NewResolved::StripFooting(s) => m
            .add_strip_footings(std::slice::from_ref(s))?
            .first()
            .copied()
            .ok_or_else(|| Error::Invalid("no strip footing created".into())),
        // a node that cannot be built stops the whole step with its own name in the message
        NewResolved::SteelConn(s) => m.add_steel_conn(s).map_err(|e| match e {
            Error::Invalid(t) => Error::Invalid(format!(
                "{} node on {}: {}",
                s.kind.key(),
                s.members
                    .iter()
                    .map(|(k, id)| format!("{k}:{id}"))
                    .collect::<Vec<_>>()
                    .join(" + "),
                t.trim_start_matches("steel_conn: ")
            )),
            other => other,
        }),
        NewResolved::WallType { name, layers } => m.add_wall_type(name, layers),
        NewResolved::SlabType { name, layers } => m.add_slab_type(name, layers),
        NewResolved::MemberType {
            name,
            shape,
            material,
        } => m.add_member_type(name, shape, material),
        NewResolved::CompType {
            family,
            name,
            values,
        } => m.add_comp_type(*family, name, values),
    }
}

fn delete(m: &mut Model, table: &str, id: i64) -> Result<(), Error> {
    match table {
        "ceiling" => m.delete_ceiling(id),
        "comp" => m.delete_component(id),
        "col" => m.delete_column(id),
        "beam" => m.delete_beam(id),
        "wall" => m.delete_wall(id),
        "roof" => m.delete_roof(id),
        "wall_spline" => m.delete_spline_wall(id),
        "stair" => m.delete_stair(id),
        "railing" => m.delete_railing(id),
        "mep_run" => m.delete_mep_run(id),
        "elec_circuit" => m.delete_circuit(id),
        "mesh" => m.delete_imported(&[("mesh", id)]),
        "shaft" => m.delete_shaft(id),
        "grid" => m.delete_grid(id),
        "strip_footing" => m.delete_struct("strip_footing", id),
        "level" => m.delete_level(id),
        other => Err(Error::Invalid(format!("cannot delete from {other}"))),
    }
}

/// Runs a step; the ids it created, in spec order.
fn run(m: &mut Model, s: &ModelStep) -> Result<Vec<i64>, Error> {
    Ok(match s {
        ModelStep::Edits(e) => {
            m.apply_edits(e)?;
            vec![]
        }
        ModelStep::Walls { specs, .. } => m.add_walls(specs)?,
        ModelStep::ArcWalls { specs, bulges, .. } => m.add_arc_walls(specs, bulges)?,
        ModelStep::Slabs { specs, .. } => m.add_slabs(specs)?,
        ModelStep::Columns { specs, .. } => m.add_columns(specs)?,
        ModelStep::Beams { specs, .. } => m.add_beams(specs)?,
        ModelStep::Comps { specs, .. } => m.add_components(specs)?,
        ModelStep::Add { elem, .. } => vec![add_one(m, elem)?],
        ModelStep::Delete { table, id } => {
            delete(m, table, *id)?;
            vec![]
        }
        ModelStep::Release { id } => {
            m.detach_wall_top(*id)?;
            vec![]
        }
        ModelStep::Comp { id, spec } => {
            m.update_component(*id, spec)?;
            vec![]
        }
        ModelStep::Family { def, overwrite } => {
            m.load_family_def(def, *overwrite)?;
            vec![]
        }
        ModelStep::ClashFix(f) => {
            m.apply_clash_fix(f)?;
            vec![]
        }
        ModelStep::TypeLib(s) => {
            m.apply_type_lib(s)?;
            vec![]
        }
        ModelStep::DefineFamily { json } => {
            match m.define_family_json(json)? {
                Ok(_) => {}
                Err(r) => {
                    return Err(Error::Invalid(format!("family {}: {} ({})", r.path, r.msg, r.code)))
                }
            }
            vec![]
        }
        ModelStep::Geometry { env, .. } => {
            geometry_rows(m, env)?.into_iter().map(|k| k.1).collect()
        }
        ModelStep::Script { env, slot0, item, .. } => {
            let r = m
                .apply_envelope_step(env)
                .map_err(|r| Error::Invalid(r.to_string()))?;
            let parts = super::script_tools::parts_of(item, *slot0, &r.created);
            let version = ddc_script::STD_VERSION as i64;
            match item.recipe {
                None => {
                    let rec = ddc_model::Recipe {
                        id: 0,
                        name: item.name.clone(),
                        script: item.script.clone(),
                        params_json: item.params_json.clone(),
                        hash: item.hash.clone(),
                        version,
                    };
                    m.recipe_add(&rec, &parts)?;
                }
                Some(id) => {
                    m.recipe_update(id, &item.params_json, &item.hash, version, &item.gone, &parts)?
                }
            }
            r.created
                .iter()
                .filter_map(|(_, rf)| match rf {
                    ddc_ops::Ref::Id { id, .. } => Some(*id),
                    _ => None,
                })
                .collect()
        }
        ModelStep::Mep { build, .. } => {
            // links read the placement of components created earlier in the held run
            m.settle_derived()?;
            m.hold_derived();
            m.mep_build(build)?.keys().iter().map(|k| k.1).collect()
        }
        ModelStep::Devices { specs, .. } => specs
            .iter()
            .map(|d| m.mep_place(d).map(|p| p.comp))
            .collect::<Result<_, _>>()?,
        ModelStep::Props(p) => {
            let sets: Vec<ddc_model::PropWrite<'_>> = p
                .iter()
                .map(|x| ddc_model::PropWrite {
                    kind: &x.kind,
                    id: x.id,
                    key: &x.key,
                    value: x.value.as_ref().map(|v| (v.0.clone(), v.1.as_str())),
                })
                .collect();
            m.set_props(&sets)?;
            vec![]
        }
        ModelStep::Openings { specs, .. } => m.add_openings_with(specs)?,
        ModelStep::Rooms { specs, .. } => m.add_rooms(specs)?,
        ModelStep::Splines { specs, .. } => specs
            .iter()
            .map(|(walls, s)| m.spline_from_walls(walls, s))
            .collect::<Result<_, _>>()?,
        ModelStep::Update { id, elem } => {
            match &**elem {
                NewResolved::Stair(s) => m.update_stair(*id, s)?,
                NewResolved::Shaft(s) => m.update_shaft(*id, s)?,
                NewResolved::Grid(s) => m.update_grid(*id, s)?,
                NewResolved::Railing(s) => m.update_railing(*id, s)?,
                NewResolved::StripFooting(s) => m.update_strip_footing(*id, s)?,
                NewResolved::Roof(s) => m.update_roof(*id, s)?,
                NewResolved::Curtain(s, spandrel) => {
                    m.set_curtain_wall(*id, s)?;
                    solid_row(m, *id, *spandrel, true)?;
                }
                _ => return Err(Error::Invalid("no in-place update of this kind".into())),
            }
            vec![]
        }
        ModelStep::DrawingTags(levels, prefix) => {
            m.tag_for_drawings(levels, prefix)?;
            vec![]
        }
        ModelStep::DrawingSet(p) => {
            m.apply_drawing_set(p)?;
            vec![]
        }
        ModelStep::Tool { step, .. } => {
            super::modify_plan::run(m, step)?.into_iter().map(|r| r.1).collect()
        }
        ModelStep::SteelFrame { level, frame, joints, .. } => {
            add_frame(m, *level, frame, *joints)?.into_iter().map(|k| k.1).collect()
        }
    })
}

/// The first row of a curtain wall, up to `spandrel` high, takes solid panels. On an update
/// (`reset`) only the panels the generator owns change (glass and solid, as the spandrel now
/// says); doors, windows and openings a person set stay.
fn solid_row(m: &mut Model, id: i64, spandrel: f64, reset: bool) -> Result<(), Error> {
    use ddc_model::PanelKind::{Glazed, Solid};
    if spandrel <= 0.0 && !reset {
        return Ok(());
    }
    let panel = |kind| ddc_model::PanelSpec { kind, name: String::new(), swing_side: false, swing_hinge: false };
    let l = m.curtain_layout(id)?;
    let mut edits = vec![];
    for (c, cur) in l.layout.cells.iter().zip(&l.panels) {
        let want = if c.j == 0 && spandrel > 0.0 { Solid } else { Glazed };
        if cur.kind != want && matches!(cur.kind, Glazed | Solid) {
            edits.push((c.i as u32, c.j as u32, panel(want)));
        }
    }
    m.set_curtain_panels(id, &edits)
}

/// Rows a step created, known from its filled ids.
pub fn created_keys(s: &ModelStep) -> Vec<ElKey> {
    if let ModelStep::Geometry { made, .. } | ModelStep::Script { made, .. } = s {
        return made.clone();
    }
    if let ModelStep::Mep { expect, .. } | ModelStep::SteelFrame { expect, .. } = s {
        return expect.iter().map(|(t, id)| (t.to_string(), *id)).collect();
    }
    if let ModelStep::Tool { expect, .. } = s {
        return expect.iter().map(|r| (r.0.to_string(), r.1)).collect();
    }
    let table = match s {
        ModelStep::Devices { .. } => "comp",
        ModelStep::Walls { .. } | ModelStep::ArcWalls { .. } => "wall",
        ModelStep::Slabs { .. } => "slab",
        ModelStep::Columns { .. } => "col",
        ModelStep::Beams { .. } => "beam",
        ModelStep::Comps { .. } => "comp",
        ModelStep::Openings { .. } => "opening",
        ModelStep::Rooms { .. } => "room",
        ModelStep::Splines { .. } => "wall_spline",
        ModelStep::Add { elem, .. } => match **elem {
            NewResolved::Opening { .. } => "opening",
            NewResolved::Room(_) => "room",
            NewResolved::Ceiling(_) => "ceiling",
            NewResolved::Roof(_) => "roof",
            NewResolved::Stair(_) => "stair",
            NewResolved::Mep(_) => "mep_run",
            NewResolved::Curtain(..) => "curtain_wall",
            NewResolved::Shaft(_) => "shaft",
            NewResolved::Grid(_) => "grid",
            NewResolved::Railing(_) => "railing",
            NewResolved::SteelConn(_) => "steel_conn",
            NewResolved::StripFooting(_) => "strip_footing",
            NewResolved::WallType { .. } => "wall_type",
            NewResolved::SlabType { .. } => "slab_type",
            NewResolved::MemberType { .. } => "member_type",
            NewResolved::CompType { .. } => "comp_type",
        },
        _ => return vec![],
    };
    expected(s)
        .into_iter()
        .map(|id| (table.to_string(), id))
        .collect()
}

fn expected(s: &ModelStep) -> Vec<i64> {
    match s {
        ModelStep::Walls { expect, .. }
        | ModelStep::ArcWalls { expect, .. }
        | ModelStep::Slabs { expect, .. }
        | ModelStep::Columns { expect, .. }
        | ModelStep::Beams { expect, .. }
        | ModelStep::Comps { expect, .. }
        | ModelStep::Devices { expect, .. }
        | ModelStep::Openings { expect, .. }
        | ModelStep::Rooms { expect, .. }
        | ModelStep::Splines { expect, .. } => expect.clone(),
        ModelStep::Add { expect, .. } => vec![*expect],
        ModelStep::Geometry { made, .. } | ModelStep::Script { made, .. } => {
            made.iter().map(|k| k.1).collect()
        }
        ModelStep::Mep { expect, .. } | ModelStep::SteelFrame { expect, .. } => {
            expect.iter().map(|k| k.1).collect()
        }
        _ => vec![],
    }
}

/// Runs a step for the first time and records the ids it got.
pub fn fill(m: &mut Model, s: &mut ModelStep) -> Result<Vec<i64>, Error> {
    if let ModelStep::Mep { build, expect } = s {
        let keys = m.mep_build(build)?.keys();
        *expect = keys.clone();
        return Ok(keys.into_iter().map(|k| k.1).collect());
    }
    if let ModelStep::Tool { step, expect } = s {
        *expect = super::modify_plan::run(m, step)?;
        return Ok(expect.iter().map(|r| r.1).collect());
    }
    if let ModelStep::SteelFrame { level, frame, expect, joints } = s {
        *expect = add_frame(m, *level, frame, *joints)?;
        return Ok(expect.iter().map(|k| k.1).collect());
    }
    if let ModelStep::Geometry { env, made } = s {
        *made = geometry_rows(m, env)?;
        return Ok(made.iter().map(|k| k.1).collect());
    }
    let ids = run(m, s)?;
    match s {
        ModelStep::Walls { expect, .. }
        | ModelStep::ArcWalls { expect, .. }
        | ModelStep::Slabs { expect, .. }
        | ModelStep::Columns { expect, .. }
        | ModelStep::Beams { expect, .. }
        | ModelStep::Comps { expect, .. }
        | ModelStep::Devices { expect, .. }
        | ModelStep::Openings { expect, .. }
        | ModelStep::Rooms { expect, .. }
        | ModelStep::Splines { expect, .. } => expect.clone_from(&ids),
        ModelStep::Add { expect, .. } => *expect = ids.first().copied().unwrap_or(0),
        ModelStep::Script { env, made, .. } => {
            let cats = geometry_cats(env);
            *made = cats.into_iter().map(made_table).zip(ids.iter().copied()).collect();
        }
        _ => {}
    }
    Ok(ids)
}

/// Replays a filled step. Different ids mean the model is not the one compiled against.
pub fn exec(m: &mut Model, s: &ModelStep) -> Result<(), Error> {
    if let ModelStep::Tool { step, expect } = s {
        let made = super::modify_plan::run(m, step)?;
        if made != *expect {
            return Err(Error::Invalid(format!(
                "plan step made {made:?}, expected {expect:?}"
            )));
        }
        return Ok(());
    }
    let ids = run(m, s)?;
    let want = expected(s);
    if ids != want {
        return Err(Error::Invalid(format!(
            "plan step created {ids:?}, expected {want:?}"
        )));
    }
    Ok(())
}

/// Exactly one op row, so exactly one Ctrl+Z; Redo re-runs the same closure and re-checks ids.
pub fn apply(ed: &mut Editor, p: &CompiledPlan) -> Result<(), AiError> {
    apply_checked(ed, p).map(|_| ())
}

/// `Ok(Some(e))`: the plan is committed and has its undo step, only the editor failed to reload.
/// That is not a failed Apply: reporting it as one would invite a second one.
pub fn apply_checked(ed: &mut Editor, p: &CompiledPlan) -> Result<Option<AiError>, AiError> {
    apply_merge_checked(ed, p, None)
}

/// [`apply_checked`] of a design option merge card: `merge = (option, note)` marks the option
/// merged inside the same journal entry, so undo, redo and a failed Apply keep the mark in step.
pub fn apply_merge_checked(
    ed: &mut Editor,
    p: &CompiledPlan,
    merge: Option<(i64, Option<String>)>,
) -> Result<Option<AiError>, AiError> {
    apply_merge_verified(ed, p, merge, &mut |_, _| Ok(()))
}

/// [`apply_merge_checked`] with `check(ed, top)` run on the written step before it is announced
/// (`top`: the op number before Apply). Its error is returned as is; it takes the step back.
pub fn apply_merge_verified(
    ed: &mut Editor,
    p: &CompiledPlan,
    merge: Option<(i64, Option<String>)>,
    check: &mut dyn FnMut(&mut Editor, i64) -> Result<(), AiError>,
) -> Result<Option<AiError>, AiError> {
    apply_merge_verified_src(ed, p, merge, None, check)
}

/// Parameter cells the plan writes: (table, id, parameter), once each, in plan order.
pub fn param_cells(steps: &[ModelStep]) -> Vec<(String, i64, String)> {
    let mut out: Vec<(String, i64, String)> = vec![];
    for s in steps {
        if let ModelStep::Edits(es) = s {
            for e in es {
                if let Edit::ParamValue { param, kind, id, .. } = e {
                    let c = (kind.clone(), *id, param.clone());
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
        }
    }
    out
}

/// Runs the steps, then records `src` on the parameter cells whose value they changed; all in
/// the one journal entry, so undo and redo take values and records together.
fn run_stamped(
    m: &mut Model,
    steps: &[ModelStep],
    src: Option<&ddc_model::ValueSrc>,
) -> Result<(), Error> {
    let _s = ddc_model::perf::span("apply.steps");
    let Some(src) = src else {
        return exec_all(m, steps);
    };
    let before = CellValues::read(m, steps)?;
    exec_all(m, steps)?;
    let changed = before.changed(m)?;
    m.record_value_src(&cell_refs(&changed), src).map(|_| ())
}

/// The steps in order, the derived tables recomputed once at the end rather than after each
/// of their operations, as the draft on the scratch copy does (`plan::compile_one`).
fn exec_all(m: &mut Model, steps: &[ModelStep]) -> Result<(), Error> {
    m.with_derived_held(|m| steps.iter().try_for_each(|s| exec(m, s)))
}

fn cell_refs(cells: &[(String, i64, String)]) -> Vec<(&str, i64, &str)> {
    cells.iter().map(|(t, id, n)| (t.as_str(), *id, n.as_str())).collect()
}

/// The values of the parameter cells a plan writes, read before it runs: after it,
/// [`CellValues::changed`] names the cells the Apply stamps with its source.
pub struct CellValues {
    cells: Vec<(String, i64, String)>,
    before: Vec<Option<String>>,
}

impl CellValues {
    pub fn read(m: &Model, steps: &[ModelStep]) -> Result<CellValues, Error> {
        let cells = param_cells(steps);
        let before = m.cell_values(&cell_refs(&cells))?;
        Ok(CellValues { cells, before })
    }

    /// Cells whose value the steps set or changed, in plan order.
    pub fn changed(&self, m: &Model) -> Result<Vec<(String, i64, String)>, Error> {
        let after = m.cell_values(&cell_refs(&self.cells))?;
        Ok(self
            .cells
            .iter()
            .zip(self.before.iter().zip(&after))
            .filter(|(_, (b, a))| a.is_some() && b != a)
            .map(|(c, _)| c.clone())
            .collect())
    }
}

/// [`apply_merge_verified`] that records who set the values: `src` (an assistant source,
/// unverified) on every parameter value the plan changed.
pub fn apply_merge_verified_src(
    ed: &mut Editor,
    p: &CompiledPlan,
    merge: Option<(i64, Option<String>)>,
    src: Option<ddc_model::ValueSrc>,
    check: &mut dyn FnMut(&mut Editor, i64) -> Result<(), AiError>,
) -> Result<Option<AiError>, AiError> {
    // run_op without an operation would undo someone else's step
    if p.steps.is_empty() {
        return Err(AiError::EmptyPlan);
    }
    if ed.model.plan_stale(p.base_head) {
        return Err(AiError::Stale);
    }
    let top = ed.model.op_top()?;
    let pushes = ed.pushes;
    let steps = p.steps.clone();
    let reload = match ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        // `src` keeps its time: a redo records the same values as the Apply did
        let body = |m: &mut Model| run_stamped(m, &steps, src.as_ref());
        match &merge {
            Some((id, note)) => m.atomic_merge("wall_ai_plan", *id, note.as_deref(), body),
            None => m.atomic("wall_ai_plan", body),
        }
    }))) {
        Ok(()) => None,
        // a failed journal read after a commit falls back to the pushed undo step
        Err(e) if committed(ed.model.op_top(), top, ed.pushes != pushes) => {
            Some(AiError::AppliedNotRefreshed(e.to_string()))
        }
        Err(e) => return Err(e.into()),
    };
    check(ed, top)?;
    if reload.is_none() {
        ed.say(crate::status::Severity::Info, crate::tr("undo.ai_applied", &[]));
        ed.offer_undo();
    }
    Ok(reload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(ed: &Editor, x: f64) -> WallSpec {
        WallSpec {
            a: (x, 0.0),
            b: (x + 4.0, 0.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        }
    }

    fn plan(ed: &Editor, steps: Vec<ModelStep>) -> CompiledPlan {
        CompiledPlan {
            base_head: ed.model.change_head(),
            steps: Arc::new(steps),
        }
    }

    /// A committed Apply pushes one undo step, so a failed journal read after it still
    /// counts as written (applied, not refreshed) and never as a refusal.
    #[test]
    fn committed_apply_survives_a_failed_journal_read() {
        let bad = || Err(ddc_model::Error::Invalid("disk read".into()));
        assert!(!committed(bad(), 4, false), "no step pushed: a refusal");
        let mut ed = Editor::new_in_memory().unwrap();
        let (top, pushes) = (ed.model.op_top().unwrap(), ed.pushes);
        let next = next_wall(&ed);
        let p = plan(
            &ed,
            vec![ModelStep::Walls {
                specs: vec![wall(&ed, 0.0)],
                expect: vec![next],
            }],
        );
        assert_eq!(apply_merge_checked(&mut ed, &p, None), Ok(None));
        assert_eq!(ed.pushes, pushes + 1, "one Apply, one undo step");
        assert!(committed(bad(), top, ed.pushes != pushes), "the pushed step decides");
        assert!(committed(ed.model.op_top(), top, false), "a good read decides");
    }

    fn box_form(w: f64, z0: f64, z1: f64) -> serde_json::Value {
        use serde_json::json;
        let m = |v: f64| json!({"v": v, "unit": "m"});
        json!({"kind": "extrude",
               "profile": {"shape": "rect", "w": m(w), "h": m(w)},
               "frame": {"origin": {"x": 0, "y": 0, "z": 0, "unit": "m"},
                         "x": {"x": 1, "y": 0, "z": 0}, "z": {"x": 0, "y": 0, "z": 1}},
               "start": m(z0), "end": m(z1)})
    }

    /// A boolean makes its tool body and its result without a slot: both are rows of the step,
    /// so the card, the selection and the replay check know them.
    #[test]
    fn rows_without_a_slot_belong_to_the_geometry_step() {
        use serde_json::json;
        let mut ed = Editor::new_in_memory().unwrap();
        let lvl = json!({"by": "id", "cat": "level", "id": ed.level});
        let ops = vec![
            json!({"action": "mass", "level": lvl, "form": box_form(2.0, 0.0, 2.0)}),
            json!({"action": "boolean", "kind": "cut", "a": {"by": "new", "slot": 0},
                   "b": {"kind": "form", "form": box_form(0.5, 0.5, 1.5)}, "keep_tool": true}),
        ];
        let ops = super::super::geo_tools::geo_ops(0, &ops).unwrap();
        let env = super::super::geo_tools::envelope_of(ops, "t").unwrap();
        let mut step = ModelStep::Geometry { env: Box::new(env), made: vec![] };
        let ids = fill(&mut ed.model, &mut step).unwrap();
        let keys = created_keys(&step);
        assert_eq!(keys.len(), 3, "base, tool body and result: {keys:?}");
        assert!(keys.iter().all(|k| k.0 == "mass"));
        assert_eq!(ids, keys.iter().map(|k| k.1).collect::<Vec<_>>());
    }

    /// A big plan (2424 changes) applies as its steps in one undo step.
    #[test]
    fn a_big_plan_applies_in_steps_as_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let top = ed.model.op_top().unwrap();
        let first = next_wall(&ed);
        let (chunks, per) = (4usize, 606usize);
        let steps: Vec<ModelStep> = (0..chunks)
            .map(|c| {
                let at = c * per;
                ModelStep::Walls {
                    specs: (0..per).map(|i| wall(&ed, 10.0 * (at + i) as f64)).collect(),
                    expect: (0..per as i64).map(|i| first + at as i64 + i).collect(),
                }
            })
            .collect();
        let p = plan(&ed, steps);
        apply(&mut ed, &p).unwrap();
        let n: i64 = ed.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2424);
        assert_eq!(ed.model.op_top().unwrap(), top + 1, "one undo step for all of it");
        ed.undo();
        let n: i64 = ed.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "the one undo takes all 2424 walls back");
    }

    #[test]
    fn empty_plan_is_refused_without_a_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        let p = plan(&ed, vec![]);
        assert_eq!(apply(&mut ed, &p), Err(AiError::EmptyPlan));
        assert!(!ed.can_undo());
    }

    #[test]
    fn apply_is_one_undo_and_checks_the_expected_ids() {
        let mut ed = Editor::new_in_memory().unwrap();
        let top = ed.model.op_top().unwrap();
        let next: i64 = ed
            .model
            .sql()
            .query_row("SELECT coalesce(max(id),0)+1 FROM wall", [], |r| r.get(0))
            .unwrap();
        let specs = vec![wall(&ed, 0.0), wall(&ed, 10.0)];
        let p = plan(
            &ed,
            vec![ModelStep::Walls {
                specs: specs.clone(),
                expect: vec![next, next + 1],
            }],
        );
        apply(&mut ed, &p).unwrap();
        assert_eq!(ed.model.op_top().unwrap(), top + 1);
        ed.undo();
        assert_eq!(ed.model.op_top().unwrap(), top);

        let wrong = plan(
            &ed,
            vec![ModelStep::Walls {
                specs,
                expect: vec![next + 5, next + 6],
            }],
        );
        assert!(apply(&mut ed, &wrong).is_err());
        assert_eq!(
            ed.model.op_top().unwrap(),
            top,
            "a failed apply leaves no trace"
        );
    }

    /// A connection that cannot be built refuses the whole plan: naming the node, leaving the
    /// walls of the earlier step unwritten and no undo step behind.
    #[test]
    fn unbuildable_connection_rolls_the_plan_back() {
        let mut ed = Editor::new_in_memory().unwrap();
        let top = ed.model.op_top().unwrap();
        let next = next_wall(&ed);
        let node = ddc_model::SteelConnSpec {
            level: ed.level,
            kind: ddc_geom::steel::ConnKind::BasePlate,
            members: vec![("column".into(), 999)],
            params: Default::default(),
        };
        let p = plan(
            &ed,
            vec![
                ModelStep::Walls {
                    specs: vec![wall(&ed, 0.0)],
                    expect: vec![next],
                },
                ModelStep::Add {
                    elem: Box::new(NewResolved::SteelConn(node)),
                    expect: 1,
                },
            ],
        );
        let e = format!("{:?}", apply(&mut ed, &p).unwrap_err());
        assert!(e.contains("base_plate") && e.contains("column:999"), "{e}");
        assert_eq!(ed.model.op_top().unwrap(), top, "no undo step");
        let walls: i64 = ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(walls, 0, "the first step is rolled back too");
    }

    fn option(ed: &Editor) -> i64 {
        let (base_head, base_stamp) = ed.model.ai_option_head().unwrap();
        ed.model
            .ai_option_add(&ddc_model::OptionRec {
                id: 0,
                set: "Entrance".into(),
                name: "A".into(),
                base_head,
                base_stamp,
                envelope: "{}".into(),
                seed: 0,
                params: "{}".into(),
                generator: None,
                metrics: None,
                note: None,
                why: None,
                status: ddc_model::OptionStatus::Draft,
            })
            .unwrap()
    }

    fn opt_status(ed: &Editor, id: i64) -> (ddc_model::OptionStatus, Option<String>) {
        let r = ed.model.ai_option_get(id).unwrap().unwrap();
        (r.status, r.note)
    }

    fn next_wall(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT coalesce(max(id),0)+1 FROM wall", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn merge_apply_mark_follows_undo_and_redo() {
        use ddc_model::OptionStatus::{Draft, Merged};
        let mut ed = Editor::new_in_memory().unwrap();
        let id = option(&ed);
        let top = ed.model.op_top().unwrap();
        let next = next_wall(&ed);
        let p = plan(
            &ed,
            vec![ModelStep::Walls {
                specs: vec![wall(&ed, 0.0)],
                expect: vec![next],
            }],
        );
        let note = Some("widest entrance".to_string());
        apply_merge_checked(&mut ed, &p, Some((id, note.clone()))).unwrap();
        assert_eq!(ed.model.op_top().unwrap(), top + 1, "one journal entry, one undo step");
        assert_eq!(opt_status(&ed, id), (Merged, note.clone()));
        ed.undo();
        assert_eq!(ed.model.op_top().unwrap(), top);
        assert_eq!(opt_status(&ed, id), (Draft, None), "undo reverts the merged mark");
        ed.redo();
        assert_eq!(ed.model.op_top().unwrap(), top + 1);
        assert_eq!(opt_status(&ed, id), (Merged, note), "redo restores it");
        ed.undo();
        assert_eq!(opt_status(&ed, id).0, Draft);
    }

    #[test]
    fn failed_merge_apply_leaves_option_unmarked() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = option(&ed);
        let top = ed.model.op_top().unwrap();
        let next = next_wall(&ed);
        let wrong = plan(
            &ed,
            vec![ModelStep::Walls {
                specs: vec![wall(&ed, 0.0)],
                expect: vec![next + 5],
            }],
        );
        assert!(apply_merge_checked(&mut ed, &wrong, Some((id, Some("x".into())))).is_err());
        assert_eq!(ed.model.op_top().unwrap(), top, "no journal entry");
        assert!(!ed.can_undo(), "no undo step");
        assert_eq!(opt_status(&ed, id), (ddc_model::OptionStatus::Draft, None));
    }

    #[test]
    fn ai_undo_shows_kept_table_note() {
        let p = std::env::temp_dir().join(format!("ddc-ad6-{}-ai.ddc", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let path = p.to_string_lossy().into_owned();
        let mut ed = Editor::seeded(Model::open(&path).unwrap()).unwrap();
        let depth = ed.undo_len();
        let next: i64 = ed
            .model
            .sql()
            .query_row("SELECT coalesce(max(id),0)+1 FROM wall", [], |r| r.get(0))
            .unwrap();
        let specs = vec![wall(&ed, 0.0)];
        let p_ai = plan(
            &ed,
            vec![ModelStep::Walls {
                specs,
                expect: vec![next],
            }],
        );
        apply(&mut ed, &p_ai).unwrap();
        crate::editor::stage::add_created_table(&mut ed, &path, "ad6_ai");
        ed.model
            .sql()
            .execute("INSERT INTO ad6_ai(v) VALUES ('later')", [])
            .unwrap();
        // the Add's undo, then the AI step's: one note, not lost under the last status
        assert_eq!(ed.jump_history(depth), 2);
        let s = ed.status.clone().unwrap_or_default();
        assert_eq!(s.matches("ad6_ai").count(), 1, "said once: {s}");
        assert_eq!(ed.undo_len(), depth);
        assert!(ed.walls.is_empty(), "the AI step is undone too");
        drop(ed);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_change_after_compiling_makes_the_plan_stale() {
        let mut ed = Editor::new_in_memory().unwrap();
        let p = plan(
            &ed,
            vec![ModelStep::Walls {
                specs: vec![wall(&ed, 0.0)],
                expect: vec![],
            }],
        );
        ed.model.add_walls(&[wall(&ed, 20.0)]).unwrap();
        assert_eq!(apply(&mut ed, &p), Err(AiError::Stale));
    }

    /// An AI Apply records its source on the values it changed, in the same undo step;
    /// a value the plan rewrites unchanged keeps whatever it had, and redo records the same.
    #[test]
    fn an_ai_apply_records_the_source_of_the_values_it_changed() {
        use ddc_model::{PVal, ParamSpec, ParamType, SrcKind, ValueSrc};
        let mut ed = Editor::new_in_memory().unwrap();
        let ids = ed.model.add_walls(&[wall(&ed, 0.0), wall(&ed, 10.0)]).unwrap();
        ed.model
            .add_param(&ParamSpec {
                name: "Fire".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        let set = |id: i64, v: &str| Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text(v.into())),
        };
        ed.model.apply_edits(&[set(ids[1], "EI30")]).unwrap();
        let p = plan(&ed, vec![ModelStep::Edits(vec![set(ids[0], "EI60"), set(ids[1], "EI30")])]);
        let src = ValueSrc::ai(SrcKind::Ai, 1_700_000_000, "s", "s#1", "fill fire ratings");
        let pushes = ed.pushes;
        let r = apply_merge_verified_src(&mut ed, &p, None, Some(src.clone()), &mut |_, _| Ok(()));
        assert_eq!(r, Ok(None));
        assert_eq!(ed.pushes, pushes + 1, "one Apply, one undo step");
        let got = ed.model.value_srcs().unwrap();
        assert_eq!(got.len(), 1, "only the value the plan changed");
        assert_eq!(got[0].0, ("wall".to_string(), ids[0], "Fire".to_string()));
        assert_eq!(got[0].1, src);
        ed.undo();
        assert!(ed.model.value_srcs().unwrap().is_empty());
        ed.redo();
        assert_eq!(ed.model.value_srcs().unwrap(), got, "redo records the same");
        let cells = param_cells(&p.steps);
        assert_eq!(cells.len(), 2);
    }

    /// Every value record row, rowid included, in rowid order.
    fn src_rows(m: &Model) -> Vec<Vec<String>> {
        let mut q = m.sql().prepare("SELECT rowid, * FROM value_src ORDER BY rowid").unwrap();
        let n = q.column_count();
        q.query_map([], |r| {
            (0..n)
                .map(|i| r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}")))
                .collect::<rusqlite::Result<Vec<String>>>()
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// vsrc1: the Apply's records replace older ones in its one undo step: undo brings the exact
    /// prior rows back, redo the same new ones, and the scratch copy stamped from its base copy
    /// holds them row for row, so promoting it keeps the copy equal to the live model.
    #[test]
    fn apply_records_undo_exactly_and_the_scratch_copy_gets_the_same_rows() {
        use ddc_model::{PVal, ParamSpec, ParamType, SrcKind, ValueSrc};
        let mut ed = Editor::new_in_memory().unwrap();
        let ids = ed.model.add_walls(&[wall(&ed, 0.0), wall(&ed, 10.0)]).unwrap();
        ed.model
            .add_param(&ParamSpec {
                name: "Fire".into(),
                ptype: ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        let set = |id: i64, v: &str| Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id,
            value: Some(PVal::Text(v.into())),
        };
        ed.model.apply_edits(&[set(ids[0], "EI30"), set(ids[1], "EI30")]).unwrap();
        let old = ValueSrc::new(SrcKind::Xlsx, 1_600_000_000);
        let both = [("wall", ids[0], "Fire"), ("wall", ids[1], "Fire")];
        assert_eq!(ed.model.record_value_src(&both, &old).unwrap(), 2);
        ed.reload().unwrap();
        let prior = src_rows(&ed.model);
        assert_eq!(prior.len(), 2);

        let dir = std::env::temp_dir().join(format!("ddc-vsrc1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base_p, work_p) = (dir.join("base.db"), dir.join("work.db"));
        ed.model.snapshot_to(&base_p).unwrap();
        ed.model.snapshot_to(&work_p).unwrap();

        let steps = vec![ModelStep::Edits(vec![set(ids[0], "EI60"), set(ids[1], "EI30")])];
        let p = plan(&ed, steps.clone());
        let src = ValueSrc::ai(SrcKind::Ai, 1_700_000_000, "s", "s#1", "fill fire ratings");
        let pushes = ed.pushes;
        let r = apply_merge_verified_src(&mut ed, &p, None, Some(src.clone()), &mut |_, _| Ok(()));
        assert_eq!(r, Ok(None));
        assert_eq!(ed.pushes, pushes + 1, "one Apply, one undo step");
        let applied = src_rows(&ed.model);
        assert_ne!(applied, prior, "the changed value has a new record");
        let srcs = ed.model.value_srcs().unwrap();
        assert_eq!(srcs.len(), 2);
        assert_eq!(srcs[0].1, src, "the changed value is the assistant's");
        assert_eq!(srcs[1].1, old, "the unchanged value keeps its record");

        {
            let base = Model::open_copy(&base_p.to_string_lossy()).unwrap();
            let mut work = Model::open_copy(&work_p.to_string_lossy()).unwrap();
            // the worker's side: the cells the live Apply stamped, recorded on the copy
            let cells = CellValues::read(&base, &steps).unwrap();
            steps.iter().for_each(|s| exec(&mut work, s).unwrap());
            let changed = cells.changed(&work).unwrap();
            work.record_value_src(&cell_refs(&changed), &src).unwrap();
            assert_eq!(src_rows(&work), applied, "the copy holds the Apply's rows");
        }

        ed.undo();
        assert_eq!(src_rows(&ed.model), prior, "undo restores the exact prior rows");
        ed.redo();
        assert_eq!(src_rows(&ed.model), applied, "redo re-adds the same rows");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
