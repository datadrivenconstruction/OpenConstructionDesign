// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan items of the ribbon's modify, level, room and footing tools: move, copy, rotate, mirror,
//! scale, array, offset, split, pin, flip, join, levels, rooms in every closed region and
//! footings under columns. Each resolves on the copy and calls the very function the
//! interactive command calls (`modify_tools.rs`, `clip_tools.rs`, `grips_opening.rs`,
//! `editor/levels.rs`, `tools/room.rs`, `tools/structx.rs`), so a draft and the ribbon cannot
//! drift apart. Lengths are metres, angles degrees counter-clockwise.

use super::exec::ModelStep;
use super::plan::{push, resolve, CompileCx, PlanError, Targets};
use super::sets::Key;
use ddc_ai::registry::{CmdError, ErrorCode};
use super::form_cmds::{FormEdit, MassEditArgs};
use ddc_model::{
    Error, FootingSpec, MeasureAnchor, MeasureKind, MeasureSpec, Model, RoomSpec, Spacing, Target,
    TopoSpec, ViewKey,
};

pub type P2 = (f64, f64);
/// Rows a step created: (table, id), in the model's order.
pub type Made = Vec<(&'static str, i64)>;

/// An act on a target.
#[derive(Clone, Debug, PartialEq)]
pub enum ModifyAct {
    /// Plan offset, then a lift; the move tool moves in plan, the 3D gizmo lifts.
    Move { by: (f64, f64, f64) },
    /// One copy per offset `(dx, dy, dz)`; a `dz` raises the copy.
    Copy { by: Vec<(f64, f64, f64)> },
    Rotate { center: P2, deg: f64, copy: bool },
    Mirror { a: P2, b: P2, copy: bool },
    Scale { center: P2, factor: f64, copy: bool },
    /// `count` instances including the original; `fit`: `step` runs from the first to the last.
    ArrayLinear { count: usize, step: P2, fit: bool },
    /// `count` instances including the original over `deg` (360: a full turn).
    ArrayRadial { count: usize, center: P2, deg: f64 },
    /// A parallel wall `dist` to the left or right of the wall's axis.
    Offset { dist: f64, left: bool },
    /// The wall splits at the point of its axis nearest to `at`.
    Split { at: P2 },
    /// The wall divides into `parts` equal lengths of its axis.
    Divide { parts: usize },
    Pin(bool),
    /// Doors and windows: the hinge side (`hand`) or the facing.
    Flip { hand: bool },
    /// Geometry joins among two or more elements.
    Join(JoinMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinMode {
    /// Joins every overlapping pair not joined yet.
    Join,
    /// Unjoins every joined pair.
    Unjoin,
    /// Swaps which element of each joined pair cuts the other.
    Switch,
}

impl ModifyAct {
    /// The interactive command that runs the same call.
    pub fn ui_command(&self) -> &'static str {
        match self {
            ModifyAct::Move { .. } => "mod.move",
            ModifyAct::Copy { .. } => "mod.copy",
            ModifyAct::Rotate { .. } => "mod.rotate",
            ModifyAct::Mirror { .. } => "mod.mirror",
            ModifyAct::Scale { .. } => "mod.scale",
            ModifyAct::ArrayLinear { .. } | ModifyAct::ArrayRadial { .. } => "mod.array",
            ModifyAct::Offset { .. } => "mod.offset",
            ModifyAct::Split { .. } => "mod.split",
            ModifyAct::Divide { .. } => "mod.divide",
            ModifyAct::Pin(true) => "mod.pin",
            ModifyAct::Pin(false) => "mod.unpin",
            ModifyAct::Flip { hand: true } => "edit.flip_hand",
            ModifyAct::Flip { hand: false } => "edit.flip_facing",
            ModifyAct::Join(JoinMode::Join) => "mod.join",
            ModifyAct::Join(JoinMode::Unjoin) => "mod.unjoin",
            ModifyAct::Join(JoinMode::Switch) => "mod.switch_join",
        }
    }
}

/// An act with no element target: levels and automatic rooms.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolAct {
    AddLevel { name: String, elevation: f64 },
    /// `None` keeps the current value.
    EditLevel { id: i64, name: Option<String>, elevation: Option<f64> },
    DeleteLevel { id: i64 },
    /// A room in every closed region of the level with no room in it.
    PlaceRooms { level: i64, height: f64, name: String },
    /// The default footing under every column of the level that has none.
    FootingsUnderColumns { level: i64 },
    /// A toposolid through survey points, as the terrain tool makes it.
    Terrain(TopoSpec),
    /// A saved measurement; the pins resolve on the copy.
    Measure { kind: MeasureKind, view: ViewKey, anchors: Vec<MeasureAnchor>, label: String },
    /// One history step on a mass; the faces resolve on the copy.
    MassEdit { mass: i64, args: MassEditArgs },
    /// A component family from the mass's feature timeline.
    TimelineFamily { mass: i64, name: Option<String> },
    /// A material's look: library class (`material_look::LOOKS` key), colour 0xRRGGBB, built-in
    /// texture with tile size in metres. `None` keeps what the material has.
    SetMaterial { name: String, look: Option<String>, color: Option<u32>, texture: Option<(String, Option<f64>)> },
}

/// A resolved step: every value fixed on the copy, so Apply replays exactly the preview.
#[derive(Clone, Debug)]
pub enum ToolStep {
    /// `rows`: the targets' (table, id), for pins and flips.
    Act { targets: Vec<Target>, rows: Vec<(&'static str, i64)>, act: ModifyAct },
    AddLevel { name: String, elevation: f64 },
    EditLevel { id: i64, name: String, elevation: f64 },
    DeleteLevel(i64),
    AddRooms(Vec<RoomSpec>),
    Footings { columns: Vec<i64>, spec: FootingSpec },
    AddTerrain(TopoSpec),
    AddMeasure(MeasureSpec),
    MassForm { mass: i64, edit: FormEdit },
    TimelineFamily { mass: i64, name: String },
    /// The material row as it will be written, and its texture slot when one is set.
    SetMaterial { spec: ddc_model::MaterialSpec, texture: Option<ddc_model::MaterialTexture> },
}

/// The model target of a plan key; `None` for a category the modify tools do not change.
pub fn target_of(k: &Key) -> Option<Target> {
    let id = k.id;
    Some(match k.cat {
        "wall" => Target::Wall(id),
        "door" | "window" | "opening" => Target::Opening(id),
        "curtain" => Target::Curtain(id),
        "floor" => Target::Slab(id),
        "room" => Target::Room(id),
        "column" => Target::Column(id),
        "beam" => Target::Beam(id),
        "roof" => Target::Roof(id),
        "ceiling" => Target::Ceiling(id),
        "stair" => Target::Stair(id),
        "railing" => Target::Railing(id),
        "mep" => Target::MepRun(id),
        "comp" => Target::Component(id),
        "shaft" => Target::Shaft(id),
        "grid" => Target::Grid(id),
        "imported" => Target::Mesh(id),
        _ => return None,
    })
}

/// (category word, table, id) of a model target the assistant can name.
pub fn key_of(t: Target) -> Option<(&'static str, &'static str, i64)> {
    Some(match t {
        Target::Wall(i) => ("wall", "wall", i),
        Target::Opening(i) => ("opening", "opening", i),
        Target::Curtain(i) => ("curtain", "curtain_wall", i),
        Target::Slab(i) => ("floor", "slab", i),
        Target::Room(i) => ("room", "room", i),
        Target::Column(i) => ("column", "col", i),
        Target::Beam(i) => ("beam", "beam", i),
        Target::Roof(i) => ("roof", "roof", i),
        Target::Ceiling(i) => ("ceiling", "ceiling", i),
        Target::Stair(i) => ("stair", "stair", i),
        Target::Railing(i) => ("railing", "railing", i),
        Target::MepRun(i) => ("mep", "mep_run", i),
        Target::Component(i) => ("comp", "comp", i),
        Target::Shaft(i) => ("shaft", "shaft", i),
        Target::Grid(i) => ("grid", "grid", i),
        Target::Mesh(i) => ("imported", "mesh", i),
        _ => return None,
    })
}

/// The category word of a created row's table, for the answer's references.
pub fn cat_of_table(table: &str) -> &str {
    match table {
        "slab" => "floor",
        "col" => "column",
        "curtain_wall" => "curtain",
        "mep_run" => "mep",
        "mesh" => "imported",
        other => other,
    }
}

fn tables(made: Vec<Target>) -> Made {
    made.into_iter().filter_map(key_of).map(|k| (k.1, k.2)).collect()
}

fn wall(ts: &[Target]) -> Result<i64, Error> {
    match ts {
        [Target::Wall(id)] => Ok(*id),
        _ => Err(Error::Invalid("needs exactly one wall".into())),
    }
}

fn act(m: &mut Model, ts: &[Target], rows: &[(&str, i64)], a: &ModifyAct) -> Result<Made, Error> {
    Ok(tables(match a {
        ModifyAct::Move { by: (x, y, z) } => {
            if x.abs() > 1e-12 || y.abs() > 1e-12 {
                m.move_elements(ts, (*x, *y))?;
            }
            if z.abs() > 1e-12 {
                m.lift_elements(ts, *z)?;
            }
            vec![]
        }
        ModifyAct::Copy { by } => m.copy_elements_z(ts, by)?,
        ModifyAct::Rotate { center, deg, copy } => {
            m.rotate_elements(ts, *center, deg.to_radians(), *copy)?
        }
        ModifyAct::Mirror { a, b, copy } => m.mirror_elements(ts, *a, *b, *copy)?,
        ModifyAct::Scale { center, factor, copy } => {
            m.scale_elements(ts, *center, *factor, *copy)?
        }
        ModifyAct::ArrayLinear { count, step, fit } => {
            let sp = if *fit { Spacing::Fit(*step) } else { Spacing::Step(*step) };
            m.array_linear(ts, *count, sp)?
        }
        ModifyAct::ArrayRadial { count, center, deg } => {
            m.array_radial(ts, *count, *center, deg.to_radians())?
        }
        ModifyAct::Offset { dist, left } => {
            vec![Target::Wall(m.offset_wall(wall(ts)?, *dist, *left)?)]
        }
        ModifyAct::Split { at } => vec![Target::Wall(m.split_wall_at(wall(ts)?, *at)?)],
        ModifyAct::Divide { parts } => {
            // the first part keeps the wall's id; only the new parts are made
            m.divide_wall(wall(ts)?, *parts)?.into_iter().skip(1).map(Target::Wall).collect()
        }
        ModifyAct::Pin(on) => {
            if *on {
                m.pin(rows)?;
            } else {
                m.unpin(rows)?;
            }
            vec![]
        }
        ModifyAct::Flip { hand } => {
            let ids: Vec<i64> = rows.iter().map(|r| r.1).collect();
            crate::grips_opening::flip_each(m, &ids, *hand)?;
            vec![]
        }
        ModifyAct::Join(mode) => {
            match mode {
                JoinMode::Join => m.join_selected(rows)?,
                JoinMode::Unjoin => m.unjoin_selected(rows)?,
                JoinMode::Switch => m.switch_joins(rows)?,
            };
            vec![]
        }
    }))
}

/// Runs the step's model call; the rows it created.
pub fn run(m: &mut Model, s: &ToolStep) -> Result<Made, Error> {
    match s {
        ToolStep::Act { targets, rows, act: a } => act(m, targets, rows, a),
        ToolStep::AddLevel { name, elevation } => {
            Ok(vec![("level", m.add_level(name, *elevation)?)])
        }
        ToolStep::EditLevel { id, name, elevation } => {
            m.update_level(*id, name, *elevation)?;
            Ok(vec![])
        }
        ToolStep::DeleteLevel(id) => {
            m.delete_level(*id)?;
            Ok(vec![])
        }
        ToolStep::AddRooms(specs) => {
            Ok(m.add_rooms(specs)?.into_iter().map(|id| ("room", id)).collect())
        }
        ToolStep::Footings { columns, spec } => Ok(m
            .footings_under_columns(columns, spec)?
            .into_iter()
            .map(|id| ("iso_footing", id))
            .collect()),
        ToolStep::AddTerrain(spec) => Ok(vec![("toposolid", m.add_toposolid(spec)?)]),
        ToolStep::AddMeasure(spec) => {
            Ok(m.add_measures(std::slice::from_ref(spec))?.into_iter().map(|id| ("measure", id)).collect())
        }
        ToolStep::MassForm { mass, edit } => {
            m.edit_mass_form(*mass, |b| edit.form(b))?;
            Ok(vec![])
        }
        ToolStep::TimelineFamily { mass, name } => {
            Ok(vec![("comp_family", crate::elem::history::to_family(m, *mass, name)?.0)])
        }
        ToolStep::SetMaterial { spec, texture } => {
            m.set_material(&spec.name, spec)?;
            if let Some(t) = texture {
                m.set_material_texture(&spec.name, Some((t.clone(), None)))?;
            }
            Ok(vec![])
        }
    }
}

/// The material row a `SetMaterial` act writes: the current row (or a new one with class
/// values) with the look's colour, then the given colour.
fn material_step(
    m: &ddc_model::Model,
    name: &str,
    look: Option<&str>,
    color: Option<u32>,
    texture: Option<&(String, Option<f64>)>,
) -> Result<ToolStep, PlanError> {
    use ddc_model::material_look::{look_by_key, look_of, LOOKS};
    let cur = m.material(name)?;
    let mut spec = cur.spec.clone();
    spec.name = name.to_string();
    let class = match look {
        Some(k) => Some(look_by_key(k).ok_or_else(|| {
            let keys: Vec<&str> = LOOKS.iter().map(|l| l.key).collect();
            PlanError(format!("look `{k}` is not in the library: {}", keys.join(", ")))
        })?),
        None => None,
    };
    if let Some(l) = class {
        spec.color = Some(l.rgb);
    }
    if let Some(c) = color {
        spec.color = Some(c);
    }
    let texture = match texture {
        Some((key, scale)) => {
            if !ddc_model::BUILTIN_TEXTURES.contains(&key.as_str()) {
                let why = format!("texture `{key}` is not built in: {}", ddc_model::BUILTIN_TEXTURES.join(", "));
                return Err(PlanError(why));
            }
            let tile = scale.unwrap_or_else(|| class.or_else(|| look_of(name)).map_or(1.0, |l| l.tile_m as f64));
            Some(ddc_model::MaterialTexture::builtin(key, tile))
        }
        None => None,
    };
    let unchanged = cur.id != 0 && spec == cur.spec && texture.is_none();
    if unchanged {
        let why = format!("nothing to change: material {name} already looks so");
        return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, why)));
    }
    if class.is_none() && spec.color.is_none() && texture.is_none() && look_of(name).is_none() {
        let why = format!("material {name} names no library look: give look, color or texture");
        return Err(PlanError(why));
    }
    Ok(ToolStep::SetMaterial { spec, texture })
}

fn step(cx: &mut CompileCx<'_>, s: ToolStep, out: &mut Vec<ModelStep>) -> Result<(), PlanError> {
    push(cx, out, ModelStep::Tool { step: s, expect: vec![] })?;
    for k in out.last().map(super::exec::created_keys).unwrap_or_default() {
        cx.declared.insert(k);
    }
    Ok(())
}

/// A door or a window: the ribbon's flip leaves empty openings out, so does the assistant.
fn flippable(m: &Model, k: &Key) -> Result<bool, Error> {
    if k.table() != "opening" {
        return Ok(false);
    }
    let q = "SELECT kind FROM opening WHERE id=?1";
    let kind: String = m.sql().query_row(q, [k.id], |r| r.get(0))?;
    Ok(kind != crate::opening_tool::Kind::Void.model_name())
}

/// Compiles one targeted item on the copy: resolves the target, then runs the call once.
pub fn compile(
    cx: &mut CompileCx<'_>,
    target: &Targets,
    a: &ModifyAct,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let keys = resolve(cx, target)?;
    let mut targets = Vec::with_capacity(keys.len());
    let mut rows = vec![];
    for k in &keys {
        let flip_ok = !matches!(a, ModifyAct::Flip { .. }) || flippable(&cx.ed.model, k)?;
        let t = target_of(k).filter(|_| flip_ok).ok_or_else(|| {
            PlanError(format!("{} cannot be changed by {}", k.text(), a.ui_command()))
        })?;
        targets.push(t);
        rows.push((k.table(), k.id));
        cx.declared.insert((k.table().to_string(), k.id));
    }
    let one_wall = matches!(
        a,
        ModifyAct::Offset { .. } | ModifyAct::Split { .. } | ModifyAct::Divide { .. }
    );
    if one_wall && !matches!(targets.as_slice(), [Target::Wall(_)]) {
        return Err(PlanError(format!(
            "args.target: {} needs exactly one wall, got {} element(s)",
            a.ui_command(),
            keys.len()
        )));
    }
    if matches!(a, ModifyAct::Join(_)) && keys.len() < 2 {
        return Err(PlanError(format!(
            "args.target: {} needs two or more elements, got {}",
            a.ui_command(),
            keys.len()
        )));
    }
    if let ModifyAct::Pin(on) = a {
        let mut keep = vec![];
        for (t, id) in rows {
            let now = cx.ed.model.is_pinned(t, id)?;
            if now != *on && (!*on || cx.ed.model.can_pin(t)) {
                keep.push((t, id));
            }
        }
        if keep.is_empty() {
            let what = if *on { "pinned or cannot be pinned" } else { "unpinned" };
            let why = format!("nothing to change: every element is {what} already");
            return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, why)));
        }
        rows = keep;
    }
    step(cx, ToolStep::Act { targets, rows, act: a.clone() }, out)
}

/// (id, name, elevation) of every level, lowest first.
pub fn levels(m: &Model) -> Result<Vec<(i64, String, f64)>, Error> {
    let mut st = m.sql().prepare("SELECT id, name, elevation FROM level ORDER BY elevation, id")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Compiles one untargeted item on the copy.
pub fn compile_tool(
    cx: &mut CompileCx<'_>,
    a: &ToolAct,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    let all = levels(&cx.ed.model)?;
    let level = |id: i64| {
        all.iter().find(|l| l.0 == id).cloned().ok_or_else(|| {
            PlanError(CmdError::wire(ErrorCode::NotFound, format!("no level {id}")))
        })
    };
    let s = match a {
        ToolAct::AddLevel { name, elevation } => {
            ToolStep::AddLevel { name: name.trim().to_string(), elevation: *elevation }
        }
        ToolAct::EditLevel { id, name, elevation } => {
            let (_, now_name, now_z) = level(*id)?;
            let name = name.as_deref().map_or(now_name.clone(), |n| n.trim().to_string());
            let elevation = elevation.unwrap_or(now_z);
            if name == now_name && (elevation - now_z).abs() < 1e-9 {
                let why =
                    format!("nothing to change: level {now_name} has that name and elevation");
                return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, why)));
            }
            cx.declared.insert(("level".to_string(), *id));
            ToolStep::EditLevel { id: *id, name, elevation }
        }
        ToolAct::DeleteLevel { id } => {
            level(*id)?;
            if all.len() <= 1 {
                return Err(PlanError("the last level cannot be deleted".to_string()));
            }
            cx.declared.insert(("level".to_string(), *id));
            ToolStep::DeleteLevel(*id)
        }
        ToolAct::PlaceRooms { level: lv, height, name } => {
            level(*lv)?;
            let base = crate::tr("tool.room.name", &[]);
            let m = &cx.ed.model;
            let (specs, rep) = crate::tools::room::plan_all(m, *lv, *height, name, &base)?;
            if specs.is_empty() {
                let why = format!(
                    "nothing to place: {} closed region(s) hold a room already, \
                     {} wall group(s) are open",
                    rep.occupied, rep.open
                );
                return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, why)));
            }
            ToolStep::AddRooms(specs)
        }
        ToolAct::FootingsUnderColumns { level: lv } => {
            level(*lv)?;
            let (columns, spec) = crate::tools::structx::footings_plan(cx.ed, *lv);
            if columns.is_empty() {
                let why = "nothing to place: every column of the level has a footing already";
                return Err(PlanError(CmdError::wire(ErrorCode::NoEffect, why)));
            }
            ToolStep::Footings { columns, spec }
        }
        // the model checks the surface on the copy: a refusal leaves the plan item out
        ToolAct::Terrain(spec) => ToolStep::AddTerrain(spec.clone()),
        ToolAct::Measure { kind, view, anchors, label } => {
            let pins = cx.ed.model.measure_pins(anchors)?;
            ToolStep::AddMeasure(MeasureSpec { kind: *kind, view: *view, pins, label: label.clone() })
        }
        ToolAct::MassEdit { mass, args } => {
            let edit = args.resolve(&cx.ed.model, *mass).map_err(PlanError)?;
            cx.declared.insert(("mass".to_string(), *mass));
            ToolStep::MassForm { mass: *mass, edit }
        }
        ToolAct::TimelineFamily { mass, name } => {
            let m = &cx.ed.model;
            let base = match name {
                Some(n) => n.clone(),
                None => m.mass(*mass)?.name,
            };
            let name = crate::elem::history::family_name(m, &base)?;
            ToolStep::TimelineFamily { mass: *mass, name }
        }
        ToolAct::SetMaterial { name, look, color, texture } => {
            material_step(&cx.ed.model, name, look.as_deref(), *color, texture.as_ref())?
        }
    };
    step(cx, s, out)
}
