// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Geometric properties of elements: one registry row per editable value. A row writes one
//! model op through `Editor::run_op`, so plan, 3D and undo follow the normal path. Tests fail
//! when a spec field is neither a row nor listed in `ELSEWHERE`.

use crate::edit3d::grips::{ceiling_spec, opening_row, wall_spec};
use crate::editor::bodies::BodyItem;
use crate::editor::{Editor, OpFn, Sel};
use crate::tr;
use crate::ui_kit;
use crate::units::{display_length, edit_length, unit_label, Units};
use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_ops::Affine;
use ddc_geom::v3;
use ddc_model::{
    BeamSpec, CeilingSpec, CurtainSpec, Edit, Error, GridLines, Just, MepShape, Model,
    MullionSection, OpeningSpec, RoomSpec, WallSpec,
};
use std::sync::{Arc, Mutex, OnceLock};

pub type Get = fn(&Model, i64) -> Result<f64, Error>;
pub type Set = fn(&mut Model, i64, f64) -> Result<(), Error>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    /// Metres; shown in the current units.
    Len,
    /// Radians in the model, degrees on screen.
    Deg,
    /// Percent, shown as is.
    Pct,
    /// Whole number of items, at least one.
    Count,
    /// Row id in this type table.
    Type(&'static str),
    /// Index into these i18n keys.
    Choice(&'static [&'static str]),
}

/// What a change must visibly do; checked for every row by the table test.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Scene,
    ScenePlan,
    /// No body of its own: the room height drives its volume.
    Qty,
}

#[derive(Clone, Copy)]
pub struct Row {
    /// Model table of the element.
    pub kind: &'static str,
    /// i18n key of the label.
    pub key: &'static str,
    pub unit: Unit,
    pub lo: f64,
    pub hi: f64,
    /// A valid test edit from the fixture value.
    pub delta: f64,
    pub effect: Effect,
    /// Spec fields this row writes.
    pub fields: &'static [&'static str],
    /// False: the element's own panel already has this value.
    pub in_panel: bool,
    pub get: Get,
    pub set: Set,
}

/// Spec fields without a row: (kind, field, where they are edited or why they are not geometry).
pub const ELSEWHERE: &[(&str, &str, &str)] = &[
    ("solid", "level", "formula bar"),
    ("solid", "name", "name field"),
    ("solid", "void", "classification, not geometry"),
    ("solid", "ifc", "IFC class: classification, not geometry"),
    ("solid", "material", "material field"),
    ("solid", "frame", "gizmo move and rotate"),
    ("mass", "level", "formula bar"),
    ("mass", "name", "name field"),
    ("mass", "void", "classification, not geometry"),
    ("mass", "inplace", "classification, not geometry"),
    ("toposolid", "name", "name field"),
    ("toposolid", "material", "material field"),
    ("toposolid", "points", "topography point tools"),
    ("toposolid", "breaklines", "topography point tools"),
    ("toposolid", "boundary", "topography boundary tool"),
    ("wall", "a", "plan and 3D grips"),
    ("wall", "b", "panel length, grips"),
    ("wall", "level", "formula bar"),
    ("wall", "offset0", "flip grip"),
    ("opening", "kind", "category, not a size"),
    ("slab", "level", "formula bar"),
    ("slab", "outer", "vertex grips"),
    ("slab", "holes", "sketch tools"),
    ("roof", "level", "roof tool"),
    ("roof", "fascia", "quantity only, no geometry"),
    ("roof", "kind", "vertex grips, roof edge tools"),
    ("col", "level", "formula bar"),
    ("col", "top_level", "formula bar"),
    ("col", "structural", "classification, not geometry"),
    ("col", "top_at", "end grip of a slanted column"),
    ("beam", "level", "formula bar"),
    ("beam", "bulge", "arc grip"),
    ("beam", "join_start", "joins are resolved automatically"),
    ("beam", "join_end", "joins are resolved automatically"),
    ("ceiling", "level", "formula bar"),
    ("ceiling", "outer", "vertex grips"),
    ("ceiling", "holes", "sketch tools"),
    ("curtain_wall", "level", "formula bar"),
    ("curtain_wall", "name", "not geometry"),
    ("curtain_wall", "a", "grips"),
    ("curtain_wall", "b", "grips"),
    ("curtain_wall", "bulge", "arc grip"),
    (
        "curtain_wall",
        "lines",
        "per-line sections in the curtain tools",
    ),
    ("curtain_wall", "top", "curtain tools"),
    ("stair", "level", "stair tool"),
    ("stair", "top_level", "stair tool"),
    ("stair", "origin", "move grip"),
    ("stair", "angle", "rotate grip"),
    ("stair", "material", "not geometry"),
    ("railing", "level", "railing tool"),
    ("railing", "host", "vertex grips"),
    ("railing", "material", "not geometry"),
    ("mep_run", "level", "not geometry: points are absolute"),
    ("mep_run", "dn", "pipe catalog"),
    ("mep_run", "bend", "fitting rule"),
    ("mep_run", "points", "vertex grips, start elevation row"),
    ("mep_run", "caps", "network tools"),
    ("room", "level", "room boundary follows walls"),
    ("room", "name", "not geometry"),
    ("room", "number", "not geometry"),
    ("room", "point", "plan drag"),
    ("shaft", "level", "shaft panel"),
    ("shaft", "top_level", "shaft panel"),
    ("shaft", "base_offset", "shaft panel field"),
    ("shaft", "top_offset", "shaft panel field"),
    ("shaft", "ring", "vertex grips"),
    ("zone", "name", "zone panel"),
    ("zone", "rooms", "zone panel"),
    ("room_sep", "level", "formula bar"),
    ("room_sep", "a", "end grips"),
    ("iso_footing", "level", "formula bar"),
    ("iso_footing", "column", "follows its column"),
    ("iso_footing", "at", "move grip"),
    ("iso_footing", "round", "chosen when placed"),
    ("iso_footing", "material", "not geometry"),
    ("strip_footing", "level", "formula bar"),
    ("strip_footing", "wall", "follows its wall"),
    ("strip_footing", "a", "end grips"),
    ("strip_footing", "b", "end grips"),
    ("strip_footing", "bulge", "chosen when placed"),
    ("strip_footing", "material", "not geometry"),
    ("raft", "level", "formula bar"),
    ("raft", "ring", "vertex grips"),
    ("raft", "edge_width", "raft panel"),
    ("raft", "edge_depth", "raft panel"),
    ("raft", "material", "not geometry"),
    ("brace", "level", "formula bar"),
    ("brace", "start", "follows its member"),
    ("brace", "end", "follows its member"),
    ("truss", "level", "formula bar"),
    ("truss", "web", "truss panel"),
    ("truss", "chord", "truss panel"),
    ("truss", "a", "end grips"),
    ("truss", "b", "end grips"),
    ("truss", "panels", "truss panel"),
    ("truss", "chord_type", "member type list"),
    ("truss", "web_type", "member type list"),
    ("beam_system", "level", "formula bar"),
    ("beam_system", "member_type", "member type list"),
    ("beam_system", "ring", "vertex grips"),
    ("beam_system", "holes", "sketch tools"),
    ("beam_system", "start_edge", "beam system options"),
    ("beam_system", "layout", "beam system panel"),
    ("beam_system", "end_gap", "beam system panel"),
    ("beam_system", "zjust", "beam system options"),
    ("ramp", "level", "formula bar"),
    ("ramp", "top_level", "ramp panel"),
    ("ramp", "base_offset", "ramp panel"),
    ("ramp", "top_offset", "ramp panel"),
    ("ramp", "slope", "ramp panel"),
    ("ramp", "landing", "ramp panel"),
    ("ramp", "max_run", "ramp panel"),
    ("ramp", "path", "vertex grips"),
    ("ramp", "material", "not geometry"),
    ("shaft", "level", "formula bar"),
    ("shaft", "top_level", "formula bar"),
    ("shaft", "ring", "vertex grips"),
    ("zone", "name", "zone panel"),
    ("zone", "rooms", "zone panel"),
    ("anno", "pts", "annotation grips on the canvas"),
    ("anno", "closed", "annotation draw tools"),
];

const LEN: (f64, f64) = (-1e4, 1e4);
const SIZE: (f64, f64) = (0.001, 1e3);
const DEG: (f64, f64) = (-360.0, 360.0);
const Y_JUST: &[&str] = &["geom.just.side_a", "geom.just.center", "geom.just.side_b"];
const Z_JUST: &[&str] = &["geom.just.bottom", "geom.just.center", "geom.just.top"];
/// Same bar as the default of the conversion window: lower is a guess.
const MIN_CONFIDENCE: f64 = 0.6;
const FLAT_TOL: f64 = 1e-6;

fn pair(g: Get, s: Set) -> (Get, Set) {
    (g, s)
}

/// Rows of a spec with its own reader and writer: `field!(read, write, |s| s.path)`.
macro_rules! field {
    ($read:ident, $write:ident, |$s:ident| $place:expr) => {
        pair(
            |m: &Model, id: i64| {
                let $s = m.$read(id)?;
                Ok($place)
            },
            |m: &mut Model, id: i64, v: f64| {
                let mut $s = m.$read(id)?;
                $place = v;
                m.$write(id, &$s)
            },
        )
    };
}

/// Id of a type in a spec, kept as a number in the row.
macro_rules! type_field {
    ($read:ident, $write:ident, |$s:ident| $place:expr) => {
        pair(
            |m: &Model, id: i64| {
                let $s = m.$read(id)?;
                Ok($place as f64)
            },
            |m: &mut Model, id: i64, v: f64| {
                let mut $s = m.$read(id)?;
                $place = v as i64;
                m.$write(id, &$s)
            },
        )
    };
}

fn wall_edit(m: &mut Model, id: i64, f: impl FnOnce(&mut WallSpec)) -> Result<(), Error> {
    let mut spec = wall_spec(m, id)?;
    f(&mut spec);
    let bulge = m.wall_bulge(id)?;
    m.apply_edits(&[Edit::Wall { id, spec, bulge }])
}

fn opening_edit(m: &mut Model, id: i64, f: impl FnOnce(&mut OpeningSpec)) -> Result<(), Error> {
    let (_, mut s) = opening_row(m, id)?;
    f(&mut s);
    m.update_opening(id, &s)
}

fn slab_edit(m: &mut Model, id: i64, f: impl FnOnce(&mut i64, &mut f64)) -> Result<(), Error> {
    let (level, mut slab_type, mut offset) = crate::query::slab_spec(m, id)?;
    f(&mut slab_type, &mut offset);
    m.apply_edits(&[Edit::Slab {
        id,
        level,
        slab_type,
        offset,
    }])
}

fn ceiling_edit(m: &mut Model, id: i64, f: impl FnOnce(&mut CeilingSpec)) -> Result<(), Error> {
    let mut s = ceiling_spec(m, id)?;
    f(&mut s);
    m.update_ceiling(id, &s)
}

fn curtain_edit(
    m: &mut Model,
    id: i64,
    f: impl FnOnce(&mut CurtainSpec) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut s = m.curtain_layout(id)?.spec;
    f(&mut s)?;
    m.set_curtain_wall(id, &s).map(|_| ())
}

fn spacing(g: &GridLines) -> Result<f64, Error> {
    match g {
        GridLines::Spacing(v) => Ok(*v),
        GridLines::At(_) => Err(Error::Invalid("grid is set line by line".into())),
    }
}

fn section_size(s: &MullionSection) -> Result<(f64, f64), Error> {
    match s {
        MullionSection::Rect { w, d } => Ok((*w, *d)),
        _ => Err(Error::Invalid("section is not rectangular".into())),
    }
}

fn rect(w: f64, d: f64) -> MullionSection {
    MullionSection::Rect { w, d }
}

fn room_spec(m: &Model, id: i64) -> Result<RoomSpec, Error> {
    Ok(m.sql().query_row(
        "SELECT level, name, number, px, py, height FROM room WHERE id=?1",
        [id],
        |r| {
            Ok(RoomSpec {
                level: r.get(0)?,
                name: r.get(1)?,
                number: r.get(2)?,
                point: (r.get(3)?, r.get(4)?),
                height: r.get(5)?,
            })
        },
    )?)
}

fn just_index(j: Just) -> f64 {
    match j {
        Just::Neg => 0.0,
        Just::Center => 1.0,
        Just::Pos => 2.0,
    }
}

fn just_of(v: f64) -> Just {
    match v as i64 {
        0 => Just::Neg,
        2 => Just::Pos,
        _ => Just::Center,
    }
}

fn beam_edit(m: &mut Model, id: i64, f: impl FnOnce(&mut BeamSpec)) -> Result<(), Error> {
    let mut s = m.beam(id)?;
    f(&mut s);
    m.update_beam(id, &s)
}

fn z_range(mesh: &Mesh) -> Option<(f64, f64)> {
    let mut it = mesh.positions.iter().map(|p| p.z);
    let first = it.next()?;
    Some(it.fold((first, first), |r, z| (r.0.min(z), r.1.max(z))))
}

/// Every vertex lies on the bottom or the top plane. Stretching along Z then only moves the
/// top plane, so the shape stays what it was. Any other shape would be distorted.
pub fn mesh_height_exact(mesh: &Mesh) -> bool {
    let Some((lo, hi)) = z_range(mesh) else {
        return false;
    };
    hi - lo > FLAT_TOL
        && mesh
            .positions
            .iter()
            .all(|p| (p.z - lo).abs() < FLAT_TOL || (p.z - hi).abs() < FLAT_TOL)
}

fn mesh_z(m: &Model, id: i64) -> Result<(f64, f64), Error> {
    z_range(&m.imported_mesh(id)?.mesh).ok_or_else(|| Error::Invalid("empty mesh".into()))
}

fn set_mesh_height(m: &mut Model, id: i64, h: f64) -> Result<(), Error> {
    let mesh = m.imported_mesh(id)?.mesh;
    if !mesh_height_exact(&mesh) {
        return Err(Error::Invalid(tr("geom.mesh_not_prism", &[])));
    }
    let (lo, hi) = mesh_z(m, id)?;
    let scale = Affine::scale(v3(0.0, 0.0, lo), v3(1.0, 1.0, h / (hi - lo)));
    m.transform_meshes(&[id], &scale)
}

/// Full registry; cheap enough to build per panel frame.
fn no_height() -> Error {
    Error::Invalid("this form has no single height".into())
}

fn solid_height(m: &Model, id: i64) -> Result<f64, Error> {
    match m.solid(id)?.form {
        ddc_model::SolidForm::Extrusion { start, end, .. } => Ok(end - start),
        _ => Err(no_height()),
    }
}

fn set_solid_height(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.solid(id)?;
    match &mut s.form {
        ddc_model::SolidForm::Extrusion { start, end, .. } => *end = *start + v,
        _ => return Err(no_height()),
    }
    m.update_solid(id, &s)
}

fn mass_height(m: &Model, id: i64) -> Result<f64, Error> {
    match m.mass(id)?.form {
        ddc_model::Form::Cuboid { lo, hi } => Ok(hi.2 - lo.2),
        ddc_model::Form::Extrude { h, .. } => Ok(h),
        _ => Err(no_height()),
    }
}

fn set_mass_height(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.mass(id)?;
    match &mut s.form {
        ddc_model::Form::Cuboid { lo, hi } => hi.2 = lo.2 + v,
        ddc_model::Form::Extrude { h, .. } => *h = v,
        _ => return Err(no_height()),
    }
    m.update_mass(id, &s)
}

fn topo_thickness(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(m.toposolid(id)?.thickness)
}

fn set_topo_thickness(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let s = m.toposolid(id)?;
    m.set_topo_props(id, &s.name, v, &s.material)
}

pub fn rows() -> Vec<Row> {
    use Effect::{Qty, Scene, ScenePlan};
    let row = |kind: &'static str,
               key: &'static str,
               unit: Unit,
               range: (f64, f64),
               delta: f64,
               effect: Effect,
               fields: &'static [&'static str],
               in_panel: bool,
               acc: (Get, Set)| Row {
        kind,
        key,
        unit,
        lo: range.0,
        hi: range.1,
        delta,
        effect,
        fields,
        in_panel,
        get: acc.0,
        set: acc.1,
    };
    let (wt, st, mt) = (
        Unit::Type("wall_type"),
        Unit::Type("slab_type"),
        Unit::Type("member_type"),
    );
    let mut all = vec![
        row(
            "wall",
            "geom.height",
            Unit::Len,
            SIZE,
            0.5,
            Scene,
            &["height"],
            false,
            pair(
                |m: &Model, id: i64| Ok(wall_spec(m, id)?.height),
                |m: &mut Model, id: i64, v: f64| wall_edit(m, id, |s| s.height = v),
            ),
        ),
        row(
            "wall",
            "geom.base_offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["base_offset"],
            true,
            pair(
                |m: &Model, id: i64| Ok(wall_spec(m, id)?.base_offset),
                |m: &mut Model, id: i64, v: f64| wall_edit(m, id, |s| s.base_offset = v),
            ),
        ),
        row(
            "wall",
            "geom.wall_lean",
            Unit::Deg,
            (-60.0, 60.0),
            0.2,
            ScenePlan,
            &["lean"],
            true,
            pair(
                |m: &Model, id: i64| Ok(m.wall_lean(id)?.angle),
                |m: &mut Model, id: i64, v: f64| {
                    let l = m.wall_lean(id)?;
                    m.set_wall_lean(id, ddc_model::Lean { angle: v, ..l })
                },
            ),
        ),
        row(
            "wall",
            "geom.wall_top_scale",
            Unit::Pct,
            (10.0, 1000.0),
            -40.0,
            Scene,
            &["top_scale"],
            true,
            pair(
                |m: &Model, id: i64| Ok(m.wall_lean(id)?.top_scale * 100.0),
                |m: &mut Model, id: i64, v: f64| {
                    let l = m.wall_lean(id)?;
                    m.set_wall_lean(id, ddc_model::Lean { top_scale: v / 100.0, ..l })
                },
            ),
        ),
        row(
            "wall",
            "geom.type",
            wt,
            LEN,
            0.0,
            ScenePlan,
            &["wall_type"],
            false,
            pair(
                |m: &Model, id: i64| Ok(wall_spec(m, id)?.wall_type as f64),
                |m: &mut Model, id: i64, v: f64| wall_edit(m, id, |s| s.wall_type = v as i64),
            ),
        ),
        row(
            "opening",
            "geom.width",
            Unit::Len,
            SIZE,
            -0.25,
            ScenePlan,
            &["width"],
            false,
            pair(
                |m: &Model, id: i64| Ok(opening_row(m, id)?.1.width),
                |m: &mut Model, id: i64, v: f64| opening_edit(m, id, |s| s.width = v),
            ),
        ),
        row(
            "opening",
            "geom.height",
            Unit::Len,
            SIZE,
            -0.25,
            Scene,
            &["height"],
            false,
            pair(
                |m: &Model, id: i64| Ok(opening_row(m, id)?.1.height),
                |m: &mut Model, id: i64, v: f64| opening_edit(m, id, |s| s.height = v),
            ),
        ),
        row(
            "opening",
            "geom.sill",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["sill"],
            false,
            pair(
                |m: &Model, id: i64| Ok(opening_row(m, id)?.1.sill),
                |m: &mut Model, id: i64, v: f64| opening_edit(m, id, |s| s.sill = v),
            ),
        ),
        row(
            "opening",
            "geom.along",
            Unit::Len,
            LEN,
            0.25,
            ScenePlan,
            &["along"],
            true,
            pair(
                |m: &Model, id: i64| Ok(opening_row(m, id)?.1.along),
                |m: &mut Model, id: i64, v: f64| opening_edit(m, id, |s| s.along = v),
            ),
        ),
        row(
            "slab",
            "geom.offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["offset"],
            true,
            pair(
                |m: &Model, id: i64| Ok(crate::query::slab_spec(m, id)?.2),
                |m: &mut Model, id: i64, v: f64| slab_edit(m, id, |_, o| *o = v),
            ),
        ),
        row(
            "slab",
            "geom.type",
            st,
            LEN,
            0.0,
            Scene,
            &["slab_type"],
            true,
            pair(
                |m: &Model, id: i64| Ok(crate::query::slab_spec(m, id)?.1 as f64),
                |m: &mut Model, id: i64, v: f64| slab_edit(m, id, |t, _| *t = v as i64),
            ),
        ),
        row(
            "roof",
            "geom.offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["offset"],
            true,
            field!(roof, update_roof, |s| s.offset),
        ),
        row(
            "roof",
            "geom.overhang",
            Unit::Len,
            (0.0, 100.0),
            0.25,
            Scene,
            &["overhang"],
            true,
            field!(roof, update_roof, |s| s.overhang),
        ),
        row(
            "roof",
            "geom.type",
            st,
            LEN,
            0.0,
            Scene,
            &["slab_type"],
            true,
            type_field!(roof, update_roof, |s| s.slab_type),
        ),
        row(
            "solid",
            "geom.height",
            Unit::Len,
            (0.01, 1e4),
            0.5,
            Scene,
            &["form"],
            true,
            pair(solid_height, set_solid_height),
        ),
        row(
            "mass",
            "geom.height",
            Unit::Len,
            (0.01, 1e4),
            0.5,
            Scene,
            &["form"],
            true,
            pair(mass_height, set_mass_height),
        ),
        row(
            "toposolid",
            "geom.thickness",
            Unit::Len,
            (0.01, 1e3),
            0.5,
            Scene,
            &["thickness"],
            true,
            pair(topo_thickness, set_topo_thickness),
        ),
        row(
            "col",
            "geom.base_offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["base_offset"],
            true,
            field!(column, update_column, |s| s.base_offset),
        ),
        row(
            "col",
            "geom.top_offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["top_offset"],
            true,
            field!(column, update_column, |s| s.top_offset),
        ),
        row(
            "col",
            "geom.rotation",
            Unit::Deg,
            DEG,
            0.25,
            Scene,
            &["rotation"],
            true,
            field!(column, update_column, |s| s.rotation),
        ),
        row(
            "col",
            "geom.x",
            Unit::Len,
            LEN,
            0.25,
            ScenePlan,
            &["at"],
            true,
            field!(column, update_column, |s| s.at.0),
        ),
        row(
            "col",
            "geom.y",
            Unit::Len,
            LEN,
            0.25,
            ScenePlan,
            &["at"],
            true,
            field!(column, update_column, |s| s.at.1),
        ),
        row(
            "col",
            "geom.type",
            mt,
            LEN,
            0.0,
            ScenePlan,
            &["member_type"],
            true,
            type_field!(column, update_column, |s| s.member_type),
        ),
        row(
            "beam",
            "geom.z_start",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["a"],
            true,
            field!(beam, update_beam, |s| s.a.2),
        ),
        row(
            "beam",
            "geom.z_end",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["b"],
            true,
            field!(beam, update_beam, |s| s.b.2),
        ),
        row(
            "beam",
            "geom.rotation",
            Unit::Deg,
            DEG,
            0.25,
            Scene,
            &["rotation"],
            true,
            field!(beam, update_beam, |s| s.rotation),
        ),
        row(
            "beam",
            "geom.start_ext",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["start_ext"],
            true,
            field!(beam, update_beam, |s| s.start_ext),
        ),
        row(
            "beam",
            "geom.end_ext",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["end_ext"],
            true,
            field!(beam, update_beam, |s| s.end_ext),
        ),
        row(
            "beam",
            "geom.yjust",
            Unit::Choice(Y_JUST),
            (0.0, 2.0),
            1.0,
            Scene,
            &["yjust"],
            true,
            pair(
                |m: &Model, id: i64| Ok(just_index(m.beam(id)?.yjust)),
                |m: &mut Model, id: i64, v: f64| beam_edit(m, id, |s| s.yjust = just_of(v)),
            ),
        ),
        row(
            "beam",
            "geom.zjust",
            Unit::Choice(Z_JUST),
            (0.0, 2.0),
            1.0,
            Scene,
            &["zjust"],
            true,
            pair(
                |m: &Model, id: i64| Ok(just_index(m.beam(id)?.zjust)),
                |m: &mut Model, id: i64, v: f64| beam_edit(m, id, |s| s.zjust = just_of(v)),
            ),
        ),
        row(
            "beam",
            "geom.type",
            mt,
            LEN,
            0.0,
            Scene,
            &["member_type"],
            true,
            type_field!(beam, update_beam, |s| s.member_type),
        ),
        row(
            "ceiling",
            "geom.height",
            Unit::Len,
            LEN,
            -0.25,
            Scene,
            &["height"],
            true,
            pair(
                |m: &Model, id: i64| Ok(ceiling_spec(m, id)?.height),
                |m: &mut Model, id: i64, v: f64| ceiling_edit(m, id, |s| s.height = v),
            ),
        ),
        row(
            "ceiling",
            "geom.type",
            st,
            LEN,
            0.0,
            Scene,
            &["slab_type"],
            true,
            pair(
                |m: &Model, id: i64| Ok(ceiling_spec(m, id)?.slab_type as f64),
                |m: &mut Model, id: i64, v: f64| ceiling_edit(m, id, |s| s.slab_type = v as i64),
            ),
        ),
        row(
            "curtain_wall",
            "geom.height",
            Unit::Len,
            SIZE,
            0.5,
            Scene,
            &["height"],
            false,
            pair(
                |m: &Model, id: i64| Ok(m.curtain_layout(id)?.spec.height),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.height = v;
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.base_offset",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &["base_offset"],
            true,
            pair(
                |m: &Model, id: i64| Ok(m.curtain_layout(id)?.spec.base_offset),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.base_offset = v;
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.grid_u",
            Unit::Len,
            SIZE,
            -0.25,
            Scene,
            &["u"],
            false,
            pair(
                |m: &Model, id: i64| spacing(&m.curtain_layout(id)?.spec.u),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.u = GridLines::Spacing(v);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.grid_v",
            Unit::Len,
            SIZE,
            -0.25,
            Scene,
            &["v"],
            false,
            pair(
                |m: &Model, id: i64| spacing(&m.curtain_layout(id)?.spec.v),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.v = GridLines::Spacing(v);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.mullion_w",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["interior"],
            false,
            pair(
                |m: &Model, id: i64| Ok(section_size(&m.curtain_layout(id)?.spec.interior)?.0),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.interior = rect(v, section_size(&s.interior)?.1);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.mullion_d",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["interior"],
            false,
            pair(
                |m: &Model, id: i64| Ok(section_size(&m.curtain_layout(id)?.spec.interior)?.1),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.interior = rect(section_size(&s.interior)?.0, v);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.border_w",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["border"],
            true,
            pair(
                |m: &Model, id: i64| Ok(section_size(&m.curtain_layout(id)?.spec.border)?.0),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.border = rect(v, section_size(&s.border)?.1);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "curtain_wall",
            "geom.border_d",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["border"],
            true,
            pair(
                |m: &Model, id: i64| Ok(section_size(&m.curtain_layout(id)?.spec.border)?.1),
                |m: &mut Model, id: i64, v: f64| {
                    curtain_edit(m, id, |s| {
                        s.border = rect(section_size(&s.border)?.0, v);
                        Ok(())
                    })
                },
            ),
        ),
        row(
            "stair",
            "geom.base_offset",
            Unit::Len,
            LEN,
            0.125,
            Scene,
            &["base_offset"],
            true,
            field!(stair, update_stair, |s| s.base_offset),
        ),
        row(
            "stair",
            "geom.top_offset",
            Unit::Len,
            LEN,
            -0.125,
            Scene,
            &["top_offset"],
            true,
            field!(stair, update_stair, |s| s.top_offset),
        ),
        row(
            "stair",
            "geom.width",
            Unit::Len,
            SIZE,
            -0.125,
            ScenePlan,
            &["width"],
            true,
            field!(stair, update_stair, |s| s.width),
        ),
        row(
            "stair",
            "geom.thickness",
            Unit::Len,
            SIZE,
            0.0625,
            Scene,
            &["thickness"],
            true,
            field!(stair, update_stair, |s| s.thickness),
        ),
        row(
            "stair",
            "geom.landing",
            Unit::Len,
            SIZE,
            0.25,
            Scene,
            &["landing"],
            true,
            field!(stair, update_stair, |s| s.landing),
        ),
        row(
            "stair",
            "geom.max_riser",
            Unit::Len,
            SIZE,
            -0.03125,
            Scene,
            &["max_riser"],
            true,
            field!(stair, update_stair, |s| s.max_riser),
        ),
        row(
            "stair",
            "geom.tread",
            Unit::Len,
            SIZE,
            // the comfort rule (2r + t within 0.60..0.65) leaves about 0.02 m either way
            0.015625,
            Scene,
            &["tread"],
            true,
            pair(
                |m: &Model, id: i64| match m.stair(id)?.tread {
                    Some(t) => Ok(t),
                    None => Ok(m.stair_layout(id)?.tread),
                },
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.stair(id)?;
                    s.tread = Some(v);
                    m.update_stair(id, &s)
                },
            ),
        ),
        row(
            "stair",
            "geom.stringer_w",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["stringer"],
            true,
            pair(
                |m: &Model, id: i64| m.stair(id)?.stringer.map(|v| v.0).ok_or_else(no_stringer),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.stair(id)?;
                    let cur = s.stringer.ok_or_else(no_stringer)?;
                    s.stringer = Some((v, cur.1));
                    m.update_stair(id, &s)
                },
            ),
        ),
        row(
            "stair",
            "geom.stringer_h",
            Unit::Len,
            SIZE,
            0.0625,
            Scene,
            &["stringer"],
            true,
            pair(
                |m: &Model, id: i64| m.stair(id)?.stringer.map(|v| v.1).ok_or_else(no_stringer),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.stair(id)?;
                    let cur = s.stringer.ok_or_else(no_stringer)?;
                    s.stringer = Some((cur.0, v));
                    m.update_stair(id, &s)
                },
            ),
        ),
        row(
            "stair",
            "geom.stair_shape",
            Unit::Choice(STAIR_SHAPES),
            (0.0, 2.0),
            -1.0,
            ScenePlan,
            &["shape"],
            true,
            pair(stair_shape_get, stair_shape_set),
        ),
        row(
            "stair",
            "geom.stair_turn",
            Unit::Choice(STAIR_TURNS),
            (0.0, 1.0),
            1.0,
            ScenePlan,
            &["shape"],
            true,
            pair(stair_turn_get, stair_turn_set),
        ),
        row(
            "railing",
            "geom.height",
            Unit::Len,
            SIZE,
            0.125,
            Scene,
            &["height"],
            true,
            field!(railing, update_railing, |s| s.height),
        ),
        row(
            "railing",
            "geom.post_spacing",
            Unit::Len,
            SIZE,
            0.5,
            Scene,
            &["post_spacing"],
            true,
            field!(railing, update_railing, |s| s.post_spacing),
        ),
        row(
            "railing",
            "geom.baluster_spacing",
            Unit::Len,
            SIZE,
            0.0625,
            Scene,
            &["baluster_spacing"],
            true,
            field!(railing, update_railing, |s| s.baluster_spacing),
        ),
        row(
            "railing",
            "geom.post",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["post"],
            true,
            field!(railing, update_railing, |s| s.post),
        ),
        row(
            "railing",
            "geom.baluster",
            Unit::Len,
            SIZE,
            0.0078125,
            Scene,
            &["baluster"],
            true,
            field!(railing, update_railing, |s| s.baluster),
        ),
        row(
            "railing",
            "geom.rail_w",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["rail"],
            true,
            field!(railing, update_railing, |s| s.rail.0),
        ),
        row(
            "railing",
            "geom.rail_h",
            Unit::Len,
            SIZE,
            0.015625,
            Scene,
            &["rail"],
            true,
            field!(railing, update_railing, |s| s.rail.1),
        ),
        row(
            "mep_run",
            "geom.width",
            Unit::Len,
            SIZE,
            0.125,
            Scene,
            &["section"],
            false,
            pair(
                |m: &Model, id: i64| Ok(m.mep_run(id)?.section.shape.wh().0),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.mep_run(id)?;
                    let (_, h) = s.section.shape.wh();
                    s.section.shape = match s.section.shape {
                        MepShape::Round { .. } => {
                            s.dn = None;
                            MepShape::Round { d: v }
                        }
                        MepShape::Oval { .. } => MepShape::Oval { w: v, h },
                        _ => MepShape::Rect { w: v, h },
                    };
                    m.update_mep_run(id, &s)
                },
            ),
        ),
        row(
            "mep_run",
            "geom.height",
            Unit::Len,
            SIZE,
            0.125,
            Scene,
            &["section"],
            false,
            pair(
                |m: &Model, id: i64| Ok(m.mep_run(id)?.section.shape.wh().1),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.mep_run(id)?;
                    let (w, _) = s.section.shape.wh();
                    s.section.shape = match s.section.shape {
                        MepShape::Round { .. } => {
                            s.dn = None;
                            MepShape::Round { d: v }
                        }
                        MepShape::Oval { .. } => MepShape::Oval { w, h: v },
                        _ => MepShape::Rect { w, h: v },
                    };
                    m.update_mep_run(id, &s)
                },
            ),
        ),
        row(
            "mep_run",
            "geom.insulation",
            Unit::Len,
            (0.0, 10.0),
            0.015625,
            Scene,
            &["section"],
            false,
            pair(
                |m: &Model, id: i64| Ok(m.mep_run(id)?.section.insulation),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = m.mep_run(id)?;
                    s.section.insulation = v;
                    m.update_mep_run(id, &s)
                },
            ),
        ),
        row(
            "room",
            "geom.height",
            Unit::Len,
            SIZE,
            0.25,
            Qty,
            &["height"],
            true,
            pair(
                |m: &Model, id: i64| Ok(room_spec(m, id)?.height),
                |m: &mut Model, id: i64, v: f64| {
                    let mut s = room_spec(m, id)?;
                    s.height = v;
                    m.update_room(id, &s)
                },
            ),
        ),
        row(
            "mesh",
            "geom.mesh_bottom",
            Unit::Len,
            LEN,
            0.25,
            Scene,
            &[],
            true,
            pair(
                |m: &Model, id: i64| Ok(mesh_z(m, id)?.0),
                |m: &mut Model, id: i64, v: f64| {
                    let dz = v - mesh_z(m, id)?.0;
                    m.move_mesh(id, (0.0, 0.0, dz))
                },
            ),
        ),
        row(
            "mesh",
            "geom.height",
            Unit::Len,
            SIZE,
            0.25,
            Scene,
            &[],
            true,
            pair(
                |m: &Model, id: i64| {
                    let (lo, hi) = mesh_z(m, id)?;
                    Ok(hi - lo)
                },
                set_mesh_height,
            ),
        ),
    ];
    all.extend(crate::elem::struct_rows::rows());
    all.extend(crate::elem::mep::rows());
    all.extend(crate::elem::rebar::geom_rows());
    all.extend(crate::elem::zone_shaft::rows());
    all.extend(crate::elem::datum::rows());
    all.extend(crate::elem::room_fam::rows());
    all
}

const STAIR_SHAPES: &[&str] = &["geom.shape.straight", "geom.shape.l", "geom.shape.u"];
const STAIR_TURNS: &[&str] = &["geom.turn.left", "geom.turn.right"];
/// Gap between the two flights of a stair that becomes U-shaped.
const U_GAP: f64 = 0.2;

fn stair_shape_get(m: &Model, id: i64) -> Result<f64, Error> {
    Ok(match m.stair(id)?.shape {
        ddc_model::StairShape::Straight => 0.0,
        ddc_model::StairShape::L { .. } => 1.0,
        ddc_model::StairShape::U { .. } => 2.0,
        ddc_model::StairShape::Spiral { .. } => 3.0,
    })
}

/// Straight, L or U; the turn side and the first flight carry over.
fn stair_shape_set(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    use ddc_model::StairShape as S;
    let mut s = m.stair(id)?;
    let (left, first, gap) = match s.shape {
        S::Straight => (true, None, U_GAP),
        S::L { left, first } => (left, first, U_GAP),
        S::U { left, first, gap } => (left, first, gap),
        S::Spiral { .. } => (true, None, U_GAP),
    };
    s.shape = match v.round() as i64 {
        0 => S::Straight,
        1 => S::L { left, first },
        2 => S::U { left, first, gap },
        3 => return Err(Error::Invalid("a spiral stair takes its radius, core and turn from create_elements".into())),
        _ => return Err(Error::Invalid("stair shape out of range".into())),
    };
    m.update_stair(id, &s)
}

fn stair_turn_get(m: &Model, id: i64) -> Result<f64, Error> {
    use ddc_model::StairShape as S;
    match m.stair(id)?.shape {
        S::L { left, .. } | S::U { left, .. } => Ok(if left { 0.0 } else { 1.0 }),
        S::Straight | S::Spiral { .. } => Err(Error::Invalid("a straight stair does not turn".into())),
    }
}

fn stair_turn_set(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    use ddc_model::StairShape as S;
    let mut s = m.stair(id)?;
    let to_left = v < 0.5;
    match &mut s.shape {
        S::L { left, .. } | S::U { left, .. } => *left = to_left,
        S::Straight | S::Spiral { .. } => return Err(Error::Invalid("a straight stair does not turn".into())),
    }
    m.update_stair(id, &s)
}

fn no_stringer() -> Error {
    Error::Invalid("stair has no stringer".into())
}

fn row_of(kind: &str, key: &str) -> Option<Row> {
    rows().into_iter().find(|r| r.kind == kind && r.key == key)
}

/// (id, name) of a type table; the name is a constant of the registry.
pub fn type_options(m: &Model, table: &str) -> Vec<(i64, String)> {
    let Ok(mut st) = m
        .sql()
        .prepare(&format!("SELECT id, name FROM {table} ORDER BY id"))
    else {
        return vec![];
    };
    let Ok(rows) = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) else {
        return vec![];
    };
    rows.filter_map(Result::ok).collect()
}

/// One grid row. `known` replaces the model read where the caller has the value already.
fn edit_row(ui: &mut egui::Ui, ed: &Editor, r: &Row, id: i64, known: Option<f64>) -> Option<f64> {
    // idle frames read nothing: the value is cached on (revision, total_changes), like port-perf
    let rk = ed.cache_key();
    let cur = match known {
        Some(v) => v,
        None => {
            let ck = egui::Id::new(("geom_props.get", r.kind, r.key, id));
            let hit = ui
                .data(|d| d.get_temp::<((u64, u64), Option<f64>)>(ck))
                .filter(|c| c.0 == rk);
            let got = match hit {
                Some(c) => c.1,
                None => {
                    let g = (r.get)(&ed.model, id).ok();
                    ui.data_mut(|d| d.insert_temp(ck, (rk, g)));
                    g
                }
            };
            got?
        }
    };
    let label = tr(r.key, &[]);
    let u = ed.tool.units;
    let key = egui::Id::new(("geom_props", r.kind, r.key, id));
    let rev = ed.revision;
    let mut v = ui
        .data(|d| d.get_temp::<(u64, f64)>(key))
        .filter(|b| b.0 == rev)
        .map_or(cur, |b| b.1);
    let done = match r.unit {
        Unit::Len => ui_kit::kv_field(ui, key, &label, &mut v, ui_kit::NumKind::Len(u), r.lo, r.hi, unit_label(u)),
        Unit::Deg => {
            let shown = v.to_degrees();
            let mut d = shown;
            let c = ui_kit::kv_field(ui, key, &label, &mut d, ui_kit::NumKind::DEG, r.lo, r.hi, "\u{b0}");
            if d != shown {
                v = d.to_radians();
            }
            c
        }
        Unit::Pct => ui_kit::kv_field(ui, key, &label, &mut v, ui_kit::NumKind::PCT, r.lo, r.hi, "%"),
        Unit::Count => ui_kit::kv_field(ui, key, &label, &mut v, ui_kit::NumKind::COUNT, r.lo, r.hi, ""),
        Unit::Type(table) => {
            let tk = egui::Id::new(("geom_props.types", table));
            let opts = match ui
                .data(|d| d.get_temp::<((u64, u64), Vec<(i64, String)>)>(tk))
                .filter(|c| c.0 == rk)
            {
                Some(c) => c.1,
                None => {
                    let o = type_options(&ed.model, table);
                    ui.data_mut(|d| d.insert_temp(tk, (rk, o.clone())));
                    o
                }
            };
            let name = opts
                .iter()
                .find(|o| o.0 == v as i64)
                .map_or("\u{2014}".to_string(), |o| o.1.clone());
            pick(ui, &label, key, name, opts.into_iter(), &mut v)
        }
        Unit::Choice(keys) => {
            let opts = keys.iter().enumerate().map(|(i, k)| (i as i64, tr(k, &[])));
            let name = keys
                .get(v as usize)
                .map_or_else(String::new, |k| tr(k, &[]));
            pick(ui, &label, key, name, opts, &mut v)
        }
    };
    ui.data_mut(|d| d.insert_temp(key, (rev, v)));
    let len_u = matches!(r.unit, Unit::Len).then_some(u);
    (done && value_changed(cur, v, len_u)).then_some(v)
}

/// Lengths compare by their shown text: Enter on an untouched field re-parses the rounded text
/// and must not commit it.
fn value_changed(cur: f64, v: f64, len_units: Option<Units>) -> bool {
    if (v - cur).abs() <= 1e-9 {
        return false;
    }
    len_units.is_none_or(|u| edit_length(cur, u) != edit_length(v, u))
}

fn pick(
    ui: &mut egui::Ui,
    label: &str,
    salt: egui::Id,
    selected: String,
    opts: impl Iterator<Item = (i64, String)>,
    v: &mut f64,
) -> bool {
    let (mut ids, mut names): (Vec<i64>, Vec<String>) = opts.unzip();
    // a value missing from the list shows `selected` as a placeholder row that sets nothing
    if !ids.contains(&(*v as i64)) {
        ids.insert(0, i64::MIN);
        names.insert(0, selected);
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut at = ids.iter().position(|i| *i == *v as i64 || *i == i64::MIN).unwrap_or(0);
    let th = ddc_ui::theme::of(ui.ctx());
    let changed = ui_kit::kv_widget(ui, label, |ui| {
        ddc_ui::widgets::field::dropdown(ui, &th, salt, &mut at, &names, Some(ui_kit::WIDGET_W)).changed()
    });
    if changed {
        if let Some(id) = ids.get(at).filter(|i| **i != i64::MIN) {
            *v = *id as f64;
            return true;
        }
    }
    false
}

/// Applies one row as one undo step; a refusal reaches the status line.
pub fn commit(ed: &mut Editor, r: &Row, id: i64, v: f64) {
    let set = r.set;
    let op = OpFn(Arc::new(move |m: &mut Model| set(m, id, v)));
    ed.status = Some(match ed.run_op(op) {
        Ok(()) => tr("geom.changed", &[]),
        Err(e) => tr("geom.not_changed", &[("e", e.to_string().as_str())]),
    });
}

/// Panel rows of this element kind.
pub fn show(ui: &mut egui::Ui, ed: &mut Editor, kind: &str, id: i64) {
    let rows: Vec<Row> = rows()
        .into_iter()
        .filter(|r| r.kind == kind && r.in_panel)
        .collect();
    if rows.is_empty() {
        return;
    }
    let mut pending = None;
    let shown: &Editor = ed;
    // sizes first, then the rows that pick a type or a variant
    let (build, size): (Vec<Row>, Vec<Row>) = rows
        .into_iter()
        .partition(|r| matches!(r.unit, Unit::Type(_) | Unit::Choice(_)));
    let kw = ui_kit::shared_key_w(ui);
    for (key, group) in [("geom_props.dimensions", size), ("geom_props.construction", build)] {
        if group.is_empty() {
            continue;
        }
        ui_kit::group_title(ui, &tr(key, &[]));
        ui_kit::props_at(ui, &format!("geom.{kind}.{id}.{key}"), kw, |ui| {
            for r in &group {
                if let Some(v) = edit_row(ui, shown, r, id, None) {
                    pending = Some((*r, v));
                }
            }
        });
    }
    if let Some((r, v)) = pending {
        commit(ed, &r, id, v);
    }
}

/// Stairs and railings have no `Sel` yet: a 3D click leaves them in the grip focus only.
pub fn show_focus(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    use crate::grips::Kind;
    if !ed.selection.is_empty() {
        return false;
    }
    let [(k, id)] = ed.grip.focus.as_slice() else {
        return false;
    };
    let id = *id;
    let (kind, title) = match k {
        Kind::Stair => ("stair", "geom.title.stair"),
        Kind::Railing => ("railing", "geom.title.railing"),
        _ => return false,
    };
    let alive = rows()
        .iter()
        .any(|r| r.kind == kind && (r.get)(&ed.model, id).is_ok());
    if !alive {
        return false;
    }
    ui_kit::element_header(ui, kind, &tr(title, &[]), "", id);
    show(ui, ed, kind, id);
    true
}

fn note(ui: &mut egui::Ui, text: String) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

/// Imported mesh: what is measured from its triangles and what may honestly change.
pub fn mesh_block(ui: &mut egui::Ui, ed: &mut Editor, b: &BodyItem) {
    let u = ed.tool.units;
    let exact = b.mesh.as_deref().is_some_and(mesh_height_exact);
    let (bottom, height) = (b.zmin, b.zmax - b.zmin);
    let mut pending = None;
    let shown: &Editor = ed;
    ui_kit::props(ui, &format!("geom.mesh.{}", b.id), |ui| {
        if height >= 0.0 {
            if let Some(r) = row_of("mesh", "geom.mesh_bottom") {
                if let Some(v) = edit_row(ui, shown, &r, b.id, Some(bottom)) {
                    pending = Some((r, v));
                }
            }
        }
        match row_of("mesh", "geom.height").filter(|_| exact) {
            Some(r) => {
                if let Some(v) = edit_row(ui, shown, &r, b.id, Some(height)) {
                    pending = Some((r, v));
                }
            }
            None if height >= 0.0 => {
                prop_row(ui, &tr("geom.height", &[]), &display_length(height, u));
            }
            None => {}
        }
    });
    if !exact {
        note(ui, tr("geom.mesh_not_prism", &[]));
    }
    if let Some((r, v)) = pending {
        commit(ed, &r, b.id, v);
    }
    let button = ui
        .button(tr("geom.make_editable", &[]))
        .on_hover_text(tr("geom.make_editable_tip", &[]));
    if button.clicked() {
        make_editable(ed, b.id);
    }
}

/// Converts on a confident recognition only; otherwise explains and opens the review window.
pub fn make_editable(ed: &mut Editor, id: i64) {
    let found = match ed.model.recognize_mesh(id, None) {
        Ok(c) => c,
        Err(e) => {
            ed.status = Some(tr("geom.not_changed", &[("e", e.to_string().as_str())]));
            return;
        }
    };
    let best = found
        .into_iter()
        .filter(|c| c.consistent && c.confidence >= MIN_CONFIDENCE)
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence));
    let Some(best) = best else {
        ed.status = Some(tr("geom.no_fit", &[]));
        crate::mesh_tools::open_convert();
        return;
    };
    let report: Arc<Mutex<Option<ddc_model::ConvertReport>>> = Arc::new(Mutex::new(None));
    let out = report.clone();
    let items = vec![(id, best.fit)];
    let keep = crate::mesh_tools::keep_source();
    let op = OpFn(Arc::new(move |m: &mut Model| {
        let r = m.convert_meshes(&items, keep)?;
        if let Ok(mut g) = out.lock() {
            *g = Some(r);
        }
        Ok(())
    }));
    if let Err(e) = ed.run_op(op) {
        ed.status = Some(tr("geom.not_changed", &[("e", e.to_string().as_str())]));
        return;
    }
    let rep = report.lock().ok().and_then(|g| g.clone());
    match rep.as_ref().and_then(|r| r.done.first()) {
        Some(c) => {
            select_converted(ed, c.table, c.id);
            ed.status = Some(tr("geom.converted", &[]));
        }
        None => {
            // the op changed nothing: leave no empty undo step behind it
            ed.undo();
            ed.status = Some(tr("geom.no_fit", &[]));
            crate::mesh_tools::open_convert();
        }
    }
}

fn select_converted(ed: &mut Editor, table: &str, id: i64) {
    use crate::editor::bodies::BodyKind as K;
    use crate::grips::Kind;
    let sel = match table {
        "wall" => Some(Sel::Wall(id)),
        "slab" => Some(Sel::Slab(id)),
        "col" => Some(Sel::Body(K::Column, id)),
        "beam" => Some(Sel::Body(K::Beam, id)),
        "ceiling" => Some(Sel::Body(K::Ceiling, id)),
        "mep_run" => Some(Sel::Mep(id)),
        "curtain_wall" => Some(Sel::Curtain(id)),
        "roof" | "stair" | "railing" | "grid" | "ref_plane" => {
            crate::elem::by_table(table).and_then(|f| f.sel(id))
        }
        _ => None,
    };
    ed.selection = sel.into_iter().collect();
    ed.grip.focus = match table {
        "stair" => vec![(Kind::Stair, id)],
        "railing" => vec![(Kind::Railing, id)],
        _ => vec![],
    };
}

fn words() -> &'static [String] {
    static WORDS: OnceLock<Vec<String>> = OnceLock::new();
    WORDS.get_or_init(|| {
        use ddc_ui::i18n::{Catalog, Lang};
        let c = Catalog::new(Lang::En);
        [Lang::En, Lang::Ru]
            .into_iter()
            .flat_map(|l| {
                c.t_in(l, "geom.words")
                    .split(',')
                    .map(|w| w.trim().to_lowercase())
                    .collect::<Vec<_>>()
            })
            .collect()
    })
}

/// Words of a source key: "Dimensions: BaseOffset" gives dimensions, base, offset.
fn tokens(key: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if !c.is_alphanumeric() || (prev_lower && c.is_uppercase()) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        }
        prev_lower = c.is_lowercase();
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A stored source value named like a dimension. It stays data: its meaning in the source
/// (base, reference, unit) is not known here, so it never drives geometry.
pub fn looks_geometric(key: &str) -> bool {
    let w = words();
    tokens(key).iter().any(|t| w.contains(t))
}

/// A stored property row; dimension-like names carry a visible data-only tag.
pub fn prop_row(ui: &mut egui::Ui, k: &str, v: &str) {
    if looks_geometric(k) {
        let tag = format!("{k} \u{b7} {}", tr("geom.data_tag", &[]));
        ui_kit::kv_num(ui, &tag, v.to_string());
    } else {
        ui_kit::kv_num(ui, k, v.to_string());
    }
}

/// One line under the stored rows when some of them read like dimensions.
pub fn data_note<'a>(ui: &mut egui::Ui, mesh: bool, mut keys: impl Iterator<Item = &'a str>) {
    if keys.any(looks_geometric) {
        let key = if mesh {
            "geom.data_only.mesh"
        } else {
            "geom.data_only.native"
        };
        note(ui, tr(key, &[]));
    }
}

fn no_slope() -> Error {
    Error::Invalid("roof: no sloped edge".into())
}

/// Radians; NaN when the sloped edges of this roof differ.
fn roof_slope_get(m: &Model, id: i64) -> Result<f64, Error> {
    let s = m.roof(id)?;
    let ddc_model::RoofKind::Footprint { slopes, .. } = &s.kind else {
        return Err(no_slope());
    };
    let mut it = slopes.iter().flatten();
    let first = *it.next().ok_or_else(no_slope)?;
    Ok(if it.all(|x| (x - first).abs() < 1e-9) {
        first.to_radians()
    } else {
        f64::NAN
    })
}

fn roof_slope_set(m: &mut Model, id: i64, v: f64) -> Result<(), Error> {
    let mut s = m.roof(id)?;
    let ddc_model::RoofKind::Footprint { slopes, .. } = &mut s.kind else {
        return Err(no_slope());
    };
    if slopes.iter().all(Option::is_none) {
        return Err(no_slope());
    }
    let deg = (v.to_degrees() * 1e9).round() / 1e9;
    for x in slopes.iter_mut().flatten() {
        *x = deg;
    }
    m.update_roof(id, &s)
}

/// Rows of a multi-selection of one table: the panel rows plus values only a selection needs.
pub fn multi_rows(kind: &str) -> Vec<Row> {
    let mut v: Vec<Row> = rows()
        .into_iter()
        .filter(|r| r.kind == kind && r.in_panel)
        .collect();
    if kind == "roof" {
        v.push(Row {
            kind: "roof",
            key: "geom_props.roof_slope",
            unit: Unit::Deg,
            lo: 1.0,
            hi: 88.0,
            delta: 5f64.to_radians(),
            effect: Effect::Scene,
            fields: &["kind"],
            in_panel: true,
            get: roof_slope_get,
            set: roof_slope_set,
        });
    }
    v
}

/// Rows every element has, with the shared value or None when the values differ.
pub fn multi_values(m: &Model, kind: &str, ids: &[i64]) -> Vec<(Row, Option<f64>)> {
    if ids.is_empty() {
        return vec![];
    }
    multi_rows(kind)
        .into_iter()
        .filter_map(|r| {
            let vals: Vec<f64> = ids
                .iter()
                .map(|&id| (r.get)(m, id))
                .collect::<Result<_, _>>()
                .ok()?;
            let first = vals[0];
            let same = first.is_finite() && vals.iter().all(|v| (v - first).abs() <= 1e-9);
            Some((r, same.then_some(first)))
        })
        .collect()
}

/// Selected ids per element table, in order of first appearance; sub-elements are skipped.
pub fn tables_of(sels: &[Sel]) -> Vec<(&'static str, Vec<i64>)> {
    let mut out: Vec<(&'static str, Vec<i64>)> = vec![];
    for s in sels {
        let id = match *s {
            Sel::Panel(..) => continue,
            Sel::Wall(i)
            | Sel::Opening(i)
            | Sel::Curtain(i)
            | Sel::Slab(i)
            | Sel::Room(i)
            | Sel::Mass(i)
            | Sel::Mep(i)
            | Sel::Comp(i)
            | Sel::Anno(i)
            | Sel::Ext(_, i)
            | Sel::Body(_, i) => i,
        };
        let Some(f) = crate::elem::of_sel(*s) else {
            continue;
        };
        match out.iter_mut().find(|g| g.0 == f.table) {
            Some(g) => g.1.push(id),
            None => out.push((f.table, vec![id])),
        }
    }
    out
}

/// Why the value may not go to these elements; checked for all of them before any write.
pub fn refusal(m: &Model, r: &Row, ids: &[i64], v: f64, u: Units) -> Option<String> {
    match r.unit {
        Unit::Type(table) => {
            if !type_options(m, table).iter().any(|o| o.0 as f64 == v) {
                return Some(tr("geom_props.no_type", &[]));
            }
        }
        Unit::Choice(keys) => {
            if !(v >= 0.0 && (v as usize) < keys.len()) {
                return Some(tr("geom_props.no_type", &[]));
            }
        }
        _ => {
            let shown = if r.unit == Unit::Deg { v.to_degrees() } else { v };
            if !shown.is_finite() || shown < r.lo - 1e-9 || shown > r.hi + 1e-9 {
                let f = |x: f64| match r.unit {
                    Unit::Len => display_length(x, u),
                    Unit::Deg => format!("{x}\u{b0}"),
                    _ => format!("{x}"),
                };
                return Some(tr(
                    "geom_props.out_of_range",
                    &[("lo", f(r.lo).as_str()), ("hi", f(r.hi).as_str())],
                ));
            }
        }
    }
    for &id in ids {
        match m.is_pinned(r.kind, id) {
            Ok(false) => {}
            Ok(true) => return Some(tr("geom_props.pinned", &[("id", id.to_string().as_str())])),
            Err(e) => return Some(e.to_string()),
        }
    }
    None
}

/// One row to every id as one model record and one undo step; any refusal writes nothing.
/// Returns how many elements changed.
pub fn commit_many(ed: &mut Editor, r: &Row, ids: &[i64], v: f64) -> usize {
    if let Some(why) = refusal(&ed.model, r, ids, v, ed.tool.units) {
        ed.status = Some(tr("geom_props.refused", &[("e", why.as_str())]));
        return 0;
    }
    let todo: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|&id| match (r.get)(&ed.model, id) {
            Ok(c) => c.is_nan() || (c - v).abs() > 1e-9,
            Err(_) => true,
        })
        .collect();
    let n = todo.len();
    if n == 0 {
        ed.status = Some(tr("geom_props.same", &[("n", ids.len().to_string().as_str())]));
        return 0;
    }
    let (set, kind) = (r.set, format!("{}_geom_multi", r.kind));
    // one journal record, so one undo restores every element; an error rolls all of it back
    let op = OpFn(Arc::new(move |m: &mut Model| {
        m.atomic(&kind, |m| {
            for &id in &todo {
                set(m, id, v)?;
            }
            Ok(())
        })
    }));
    let top = ed.model.op_top().ok();
    let ns = n.to_string();
    match ed.run_op(op) {
        Ok(()) => {
            ed.status = Some(tr("geom_props.changed", &[("n", ns.as_str())]));
            n
        }
        // the record is written and only the reload failed
        Err(e) if ed.model.op_top().ok() != top => {
            ed.status = Some(tr("geom_props.not_refreshed", &[("e", e.to_string().as_str())]));
            n
        }
        Err(e) => {
            ed.status = Some(tr("geom_props.refused", &[("e", e.to_string().as_str())]));
            0
        }
    }
}

fn num_text(v: f64) -> String {
    format!("{}", (v * 100.0).round() / 100.0)
}

fn parse_num(s: &str, unit: &str) -> Option<f64> {
    parse_num_mark(s, unit, crate::units::ui_decimal_mark())
}

/// [`parse_num`] in the locale of `mark`: English `1,000` is a thousand, `1,5` is refused.
fn parse_num_mark(s: &str, unit: &str, mark: crate::units::DecimalMark) -> Option<f64> {
    let t = s.trim();
    let t = if unit.is_empty() { t } else { t.trim_end_matches(unit) };
    crate::units::parse_locale_number(t.trim(), mark).ok()
}

/// Number text field with the unit in the unit place; Enter commits.
fn num_field(
    ui: &mut egui::Ui,
    label: &str,
    buf: &mut String,
    unit: &str,
    tip: &str,
    hint: &str,
) -> Option<f64> {
    let r = ui_kit::kv_widget(ui, label, |ui| {
        let c = ui_kit::weak(ui);
        ui.label(egui::RichText::new(unit).color(c));
        ui.add(
            egui::TextEdit::singleline(buf)
                .desired_width(ui_kit::WIDGET_W - 18.0)
                .margin(egui::Margin::symmetric(ddc_ui::theme::space::SM as i8, 4))
                .horizontal_align(egui::Align::RIGHT)
                .hint_text(hint)
                .font(egui::TextStyle::Body),
        )
        .on_hover_text(tip)
    });
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    if r.lost_focus() && enter {
        parse_num(buf, unit)
    } else {
        None
    }
}

/// One row of a multi-selection: the shared value, or an empty field marked as varying.
fn multi_row(
    ui: &mut egui::Ui,
    ed: &Editor,
    r: &Row,
    ids: &[i64],
    common: Option<f64>,
) -> Option<f64> {
    let label = tr(r.key, &[]);
    let varies = tr("geom_props.varies", &[]);
    let tip = tr("geom_props.tip", &[("n", ids.len().to_string().as_str())]);
    let u = ed.tool.units;
    let key = egui::Id::new(("geom_multi", r.kind, r.key, ids));
    match r.unit {
        Unit::Len | Unit::Deg | Unit::Pct | Unit::Count => {
            let rev = ed.revision;
            let fresh = || {
                common.map_or_else(String::new, |v| match r.unit {
                    Unit::Deg => num_text(v.to_degrees()),
                    Unit::Len => edit_length(v, u),
                    _ => num_text(v),
                })
            };
            let mut buf = ui
                .data(|d| d.get_temp::<(u64, String)>(key))
                .filter(|b| b.0 == rev)
                .map_or_else(fresh, |b| b.1);
            let hint = if common.is_none() { varies.as_str() } else { "" };
            let out = match r.unit {
                Unit::Len => ui_kit::kv_length_hint(ui, &label, &mut buf, u, &tip, hint),
                Unit::Deg => {
                    num_field(ui, &label, &mut buf, "\u{b0}", &tip, hint).map(f64::to_radians)
                }
                Unit::Pct => num_field(ui, &label, &mut buf, "%", &tip, hint),
                _ => num_field(ui, &label, &mut buf, "", &tip, hint).map(f64::round),
            };
            ui.data_mut(|d| d.insert_temp(key, (rev, buf)));
            out
        }
        Unit::Type(table) => {
            let opts = type_options(&ed.model, table);
            let name = match common {
                None => varies,
                Some(v) => opts
                    .iter()
                    .find(|o| o.0 as f64 == v)
                    .map_or("\u{2014}".to_string(), |o| o.1.clone()),
            };
            let mut v = common.unwrap_or(-1.0);
            pick(ui, &label, key, name, opts.into_iter(), &mut v).then_some(v)
        }
        Unit::Choice(keys) => {
            let name = match common {
                None => varies,
                Some(v) => keys
                    .get(v as usize)
                    .map_or_else(String::new, |k| tr(k, &[])),
            };
            let opts = keys.iter().enumerate().map(|(i, k)| (i as i64, tr(k, &[])));
            let mut v = common.unwrap_or(-1.0);
            pick(ui, &label, key, name, opts, &mut v).then_some(v)
        }
    }
}

/// Shared values and pinned count of one table group, read once per model revision.
type MultiCache = (u64, Vec<(Row, Option<f64>)>, usize);

/// One table block: table, ids, shared values, pinned count.
type MultiGroup = (&'static str, Vec<i64>, Vec<(Row, Option<f64>)>, usize);

/// Properties of a multi-selection: one block per element table with the rows all of its
/// elements have; a differing value shows as varying, Enter writes it to all of them in one
/// undo step. False: no selected table has such rows (the caller shows its own hint).
pub fn show_multi(ui: &mut egui::Ui, ed: &mut Editor, sels: &[Sel]) -> bool {
    let rev = ed.revision;
    let groups: Vec<MultiGroup> = tables_of(sels)
        .into_iter()
        .map(|(table, ids)| {
            let key = egui::Id::new(("geom_multi.vals", table, &ids));
            let hit = ui
                .data(|d| d.get_temp::<MultiCache>(key))
                .filter(|c| c.0 == rev);
            let (vals, pinned) = match hit {
                Some(c) => (c.1, c.2),
                None => {
                    let vals = multi_values(&ed.model, table, &ids);
                    let pinned = ids
                        .iter()
                        .filter(|&&id| ed.model.is_pinned(table, id).unwrap_or(false))
                        .count();
                    ui.data_mut(|d| d.insert_temp(key, (rev, vals.clone(), pinned)));
                    (vals, pinned)
                }
            };
            (table, ids, vals, pinned)
        })
        .filter(|g| !g.2.is_empty())
        .collect();
    if groups.is_empty() {
        return false;
    }
    let mut pending = None;
    let shown: &Editor = ed;
    for (table, ids, vals, pinned) in &groups {
        let kind = tr(&format!("geom_props.kind.{table}"), &[]);
        let n = ids.len().to_string();
        let head = tr("geom_props.head", &[("kind", kind.as_str()), ("n", n.as_str())]);
        if !ui_kit::heading(ui, &head) {
            continue;
        }
        ui_kit::props(ui, &format!("geom.multi.{table}"), |ui| {
            for (r, common) in vals {
                if let Some(v) = multi_row(ui, shown, r, ids, *common) {
                    pending = Some((*r, ids.clone(), v));
                }
            }
        });
        if *pinned > 0 {
            ui_kit::hint(ui, tr("geom_props.pinned_note", &[("n", pinned.to_string().as_str())]));
        }
    }
    if let Some((r, ids, v)) = pending {
        commit_many(ed, &r, &ids, v);
    }
    true
}

#[cfg(test)]
#[path = "geom_props_multi_tests.rs"]
mod multi_tests;

#[cfg(test)]
#[path = "geom_props_tests.rs"]
pub(crate) mod tests;
