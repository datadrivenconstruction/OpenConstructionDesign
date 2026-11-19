// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Placement, profiles, forms and patterns (AI_NATIVE.md sections 4.1-4.6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::refs::{Cat, Ref};
use crate::units::{Angle, Dir3, Len, P2, P3};

/// A point given by intent; the solver resolves it and the intent is stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Place {
    At { p: P3 },
    Offset { from: Ref, d: Len, side: Side },
    OnHost { host: Ref, along: Len, from_end: End, side: Side, z: Len },
    Crossing { a: Ref, b: Ref },
    Center { of: Ref },
    AlignTo { refs: Vec<Ref> },
    /// Point at parameter `t` (0..1) from `a` to `b`.
    Between { a: Ref, b: Ref, t: f64 },
    Snap { near: P3, kinds: Vec<SnapKind>, max: Len },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum End {
    A,
    B,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapKind {
    Endpoint,
    Midpoint,
    Intersection,
    Nearest,
    Perpendicular,
    Center,
    Grid,
    Face,
}

/// Named section shape with parameters (lowered to `gsolid::Section` / `profile.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Rect { w: Len, h: Len },
    Circle { r: Len },
    Ring { r_out: Len, r_in: Len },
    I { h: Len, b: Len, tw: Len, tf: Len },
    H { h: Len, b: Len, tw: Len, tf: Len },
    C { h: Len, b: Len, tw: Len, tf: Len },
    T { h: Len, b: Len, tw: Len, tf: Len },
    L { h: Len, b: Len, t: Len },
    Z { h: Len, b: Len, t: Len },
    Tube { w: Len, h: Len, t: Len },
    Polygon { pts: Vec<P2> },
    /// `bulges[i]` belongs to the segment from `pts[i]`; empty means all straight.
    Polyline { pts: Vec<P2>, bulges: Vec<f64>, closed: bool },
    Rings { outer: Vec<P2>, holes: Vec<Vec<P2>> },
    /// A stored profile type.
    Stored { of: Ref },
}

/// Local frame: profile lies in the x/y plane of the frame, `z` is the extrusion direction.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub origin: P3,
    pub x: Dir3,
    pub z: Dir3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Path {
    Polyline { pts: Vec<P3>, bulges: Vec<f64>, closed: bool },
    /// `wall:5/axis`, `slab:9/edge:3`, `grid:A`, a drawing polyline.
    Ref { of: Ref },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub profile: Profile,
    pub frame: Frame,
}

/// Solid-building form (maps to `gsolid::Form` and `mass::MassSpec`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Form {
    Extrude { profile: Profile, frame: Frame, start: Len, end: Len },
    Revolve { profile: Profile, frame: Frame, axis_a: P3, axis_b: P3, angle: Angle },
    /// `twist`: total turn of the profile from start to end; `scale`: profile scale at the
    /// path end (1 = none), linear along the path. Both need an open path and a plain profile.
    Sweep {
        profile: Profile,
        path: Path,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        twist: Option<Angle>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<f64>,
    },
    Loft { sections: Vec<Section>, path: Option<Path> },
    SweptBlend { a: Profile, b: Profile, path: Path },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoolKind {
    Cut,
    Union,
    Intersect,
}

/// IFC entity of a part: `class` like IfcMember, `predefined` one of its enum values (CHORD).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IfcClass {
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predefined: Option<String>,
}

/// A component row written beside a part: it counts as a `name` (blade, gondola), placed at `at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleTag {
    pub name: String,
    pub at: P3,
}

/// Second operand of a boolean.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Tool {
    Ref { of: Ref },
    Form { form: Form },
}

/// Geometry ops (section 4, 9). The result keeps its parameters so a later `Set` re-evaluates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GeoOp {
    Solid {
        slot: u32,
        level: Option<Ref>,
        form: Form,
        material: Option<String>,
        /// What the part is ("diagonal CHS 508x20"); the solid row's name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Semantic IFC class of the part; none exports a generic proxy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ifc: Option<IfcClass>,
        /// Component role of the part: a component row of this family is written at `at`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<RoleTag>,
    },
    Mass { slot: u32, level: Option<Ref>, form: Form, name: Option<String> },
    Boolean { kind: BoolKind, a: Ref, b: Tool, keep_tool: bool },
    /// Cut a wall or slab by a form (`solids_cut.rs`).
    Void { host: Ref, form: Form },
    /// Replace the form parameters of an existing solid or mass.
    Reshape { target: Ref, form: Form },
    /// Change the geometry kind of a mesh or solid; the receipt lists what is lost.
    Convert { target: Ref, to: ConvertTo },
    /// Freeform NURBS surface (a shell when `thickness` is set): roofs, canopies, facades.
    Freeform {
        slot: u32,
        level: Option<Ref>,
        surface: SurfaceBy,
        thickness: Option<Len>,
        side: Option<ShellSide>,
        trim: Option<UvTrim>,
        material: Option<String>,
        name: Option<String>,
    },
    /// Move control point (`i` along u, `j` along v) of a freeform surface to `to`.
    EditPoint { target: Ref, i: u32, j: u32, to: P3 },
    /// Shell thickness of a freeform surface; zero leaves a bare surface.
    Thicken { target: Ref, thickness: Len, side: Option<ShellSide> },
    /// Trim loops of a freeform surface in normalized surface parameters; none clears them.
    Trim { target: Ref, trim: Option<UvTrim> },
    /// New control net of a freeform surface (`count_u` x `count_v` points, degree default 3);
    /// the receipt states the largest deviation from the old shape.
    Rebuild { target: Ref, count_u: u32, count_v: u32, degree_u: Option<u32>, degree_v: Option<u32> },
    /// Curtain system on a freeform surface: a panel grid with mullions along its edges.
    /// `columns` divide along u, `rows` along v (or at absolute heights); `planarize` runs
    /// that many rounds of planar-quad optimisation (default none).
    Panelize {
        slot: u32,
        host: Ref,
        columns: PanelDiv,
        rows: PanelDiv,
        pattern: Option<PanelPattern>,
        planarize: Option<u32>,
        mullion: Option<Mullion>,
        /// Panel (glass) thickness, default 24 mm.
        glass: Option<Len>,
        /// Largest diagonal gap of a panel still taken as flat (default 1 mm) and as cold-bent
        /// (default 20 mm); beyond it a panel is hot-bent.
        flat: Option<Len>,
        cold: Option<Len>,
        material: Option<String>,
        name: Option<String>,
    },
    /// Roof or slab on a freeform surface: `layers` stack along the surface normal from the
    /// `side` (default normal); `openings` are plan polygons cut through the stack. The
    /// element follows later edits of the host.
    SurfaceShell {
        slot: u32,
        host: Ref,
        kind: ShellKind,
        layers: Vec<ShellLayer>,
        side: Option<ShellSide>,
        openings: Option<Vec<Vec<P2>>>,
        name: Option<String>,
    },
    /// Diagrid, gridshell or cable net on an analytic host: nodes on a `count_u` x `count_v`
    /// lattice of the host, members of the catalogue section `member` (boundary members
    /// `edge`, default `member`), ball or clamp nodes of diameter `node` (none sizes them).
    Gridshell {
        slot: u32,
        level: Option<Ref>,
        host: LatticeHost,
        kind: Option<LatticeKind>,
        pattern: Option<LatticePattern>,
        count_u: u32,
        count_v: u32,
        supports: Option<LatticeSupports>,
        member: String,
        edge: Option<String>,
        node: Option<Len>,
    },
    /// One straight structural member from `start` to `end` (absolute coordinates) of the
    /// catalogue section `section` ("CHS 88.9x5"): a beam or brace row; `rotation` turns the
    /// section about its axis.
    Member {
        slot: u32,
        level: Option<Ref>,
        start: P3,
        end: P3,
        section: String,
        role: MemberRole,
        rotation: Option<Angle>,
    },
    /// Flat plate of `thickness` on the polygon `outline` (absolute coordinates) with
    /// `openings` cut through it (see [`plate_kind`]): a horizontal plate is a slab with the
    /// openings as holes, an upright rectangular one a wall with a void opening row per opening
    /// (its bounding rectangle in the plate). Tilted plates are refused.
    Plate {
        slot: u32,
        level: Option<Ref>,
        outline: Vec<P3>,
        thickness: Len,
        openings: Vec<Vec<P3>>,
        material: Option<String>,
    },
}

/// What row a `geometry.member` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    Beam,
    Brace,
    /// A bar of a gridshell; only a gridshell (`geometry.gridshell`) holds these, so the
    /// model refuses a free one.
    GridshellBar,
}

/// Where a plate lands in the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlateKind {
    Slab,
    Wall,
}

/// Largest distance of an outline point from the plate plane, m.
pub const PLATE_FLAT: f64 = 0.001;
/// Largest |cos| between the plate normal and the horizontal (wall) or vertical (slab).
const PLATE_TILT: f64 = 1e-4;

/// Orientation of a plate outline: flat within [`PLATE_FLAT`], horizontal (a slab) or
/// upright (a wall); anything else is refused with the reason.
pub fn plate_kind(outline: &[P3]) -> Result<PlateKind, String> {
    if outline.len() < 3 {
        return Err("a plate outline needs 3 or more points".into());
    }
    let p: Vec<[f64; 3]> = outline.iter().map(|q| q.to_m()).collect();
    if p.iter().flatten().any(|v| !v.is_finite()) {
        return Err("plate outline points must be finite".into());
    }
    let mut n = [0.0f64; 3];
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if !(l > 1e-9) {
        return Err("plate outline encloses no area".into());
    }
    let u = [n[0] / l, n[1] / l, n[2] / l];
    let warp = p
        .iter()
        .map(|q| ((q[0] - p[0][0]) * u[0] + (q[1] - p[0][1]) * u[1] + (q[2] - p[0][2]) * u[2]).abs())
        .fold(0.0, f64::max);
    if warp > PLATE_FLAT {
        return Err(format!("plate outline is not flat ({:.1} mm out of plane)", warp * 1000.0));
    }
    if u[2].abs() > 1.0 - PLATE_TILT {
        Ok(PlateKind::Slab)
    } else if u[2].abs() < PLATE_TILT {
        Ok(PlateKind::Wall)
    } else {
        Err("tilted plates are not supported: lay the plate flat or stand it upright".into())
    }
}

impl GeoOp {
    /// Category of the row the op makes under its slot (None: no slot, or refused anyway).
    pub fn made_cat(&self) -> Option<Cat> {
        match self {
            GeoOp::Solid { .. } => Some(Cat::Solid),
            GeoOp::Mass { .. } => Some(Cat::Mass),
            GeoOp::Freeform { .. } => Some(Cat::Freeform),
            GeoOp::Panelize { .. } => Some(Cat::PanelSystem),
            GeoOp::SurfaceShell { .. } => Some(Cat::SurfaceShell),
            GeoOp::Gridshell { .. } => Some(Cat::Gridshell),
            GeoOp::Member { role: MemberRole::Beam, .. } => Some(Cat::Beam),
            GeoOp::Member { role: MemberRole::Brace, .. } => Some(Cat::Brace),
            GeoOp::Member { role: MemberRole::GridshellBar, .. } => None,
            GeoOp::Plate { outline, .. } => match plate_kind(outline) {
                Ok(PlateKind::Slab) => Some(Cat::Slab),
                Ok(PlateKind::Wall) => Some(Cat::Wall),
                Err(_) => None,
            },
            GeoOp::Boolean { .. }
            | GeoOp::Void { .. }
            | GeoOp::Reshape { .. }
            | GeoOp::Convert { .. }
            | GeoOp::EditPoint { .. }
            | GeoOp::Thicken { .. }
            | GeoOp::Trim { .. }
            | GeoOp::Rebuild { .. } => None,
        }
    }
}

/// Host surface of a grid structure (absolute coordinates).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case", deny_unknown_fields)]
pub enum LatticeHost {
    /// Spherical cap on a base circle; `oculus` is the radius of the top ring (default a
    /// tenth of the radius: a grid cannot meet in the pole).
    Dome { center: P3, radius: Len, rise: Len, oculus: Option<Len> },
    /// Circular barrel vault over the level axis `a`-`b` at the springing line.
    Vault { a: P3, b: P3, span: Len, rise: Len },
    /// Saddle over an `lx` x `ly` rectangle, corners at +-`rise`, turned by `angle`.
    Hypar { center: P3, lx: Len, ly: Len, rise: Len, angle: Option<Angle> },
    /// Flat grid over a `w` x `h` rectangle from `corner`, turned by `angle`.
    Plane { corner: P3, w: Len, h: Len, angle: Option<Angle> },
    /// Dish (paraboloid of revolution) from its `vertex`: rim `radius`, `depth` from the vertex
    /// to the rim plane (at most the radius), axis tilted by `tilt` from the vertical toward
    /// the plan direction `azimuth`; `hub` is the radius of the inner ring the dish stands on
    /// (default a tenth of the radius).
    Paraboloid {
        vertex: P3,
        radius: Len,
        depth: Len,
        hub: Option<Len>,
        tilt: Option<Angle>,
        azimuth: Option<Angle>,
    },
}

/// What a grid structure is built as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatticeKind {
    /// Straight members between ball nodes.
    Diagrid,
    /// Continuous curved laths clamped at the crossings.
    Gridshell,
    /// Straight cables between clamps.
    CableNet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatticePattern {
    Diagrid,
    Triangular,
    QuadBraced,
    Quad,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatticeSupports {
    /// The springing ring or line.
    Base,
    /// Every boundary node.
    Edges,
    /// The four corners of an open grid.
    Corners,
}

/// How a direction of a panel grid is divided.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum PanelDiv {
    /// `n` panels at equal parameter steps.
    Count { n: u32 },
    /// `n` panels of equal length along the surface.
    Equal { n: u32 },
    /// Panels of about this length along the surface (count rounded up).
    Width { width: Len },
    /// Row lines where the surface crosses these absolute heights, rising (rows only).
    Heights { heights: Vec<Len> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PanelPattern {
    Quad,
    Triangle,
    Diamond,
}

/// Rectangular mullion section along every panel edge (default 60 x 150 mm).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mullion {
    pub width: Len,
    pub depth: Len,
}

/// What a surface shell is in the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Roof,
    Slab,
}

/// One layer of a surface shell, listed along the surface normal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellLayer {
    pub thickness: Len,
    pub material: Option<String>,
    pub name: Option<String>,
}

/// A curve given by points (absolute coordinates).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Curve3 {
    pub pts: Vec<P3>,
    /// `through`: the curve passes through the points (default); `control`: the points are
    /// B-spline control points; `polyline`: straight segments.
    pub fit: Option<CurveFit>,
    /// Polynomial degree, 1..5 (default 3, lowered when there are too few points).
    pub degree: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveFit {
    Through,
    Control,
    Polyline,
}

/// How a freeform surface is made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum SurfaceBy {
    /// Skinned through the section curves in order; `degree` across them (default 3).
    /// `params` gives the v parameter of each curve (rising from 0 to 1, one per curve); two
    /// lofts that share a column of points and the same `params` meet on one edge. None takes
    /// chord-length parameters from the curves.
    Loft {
        curves: Vec<Curve3>,
        degree: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Vec<f64>>,
    },
    /// Straight rulings between two curves.
    Ruled { a: Curve3, b: Curve3 },
    /// Profile curve turned about the axis from `axis_a` to `axis_b`.
    Revolve { profile: Curve3, axis_a: P3, axis_b: P3, angle: Angle },
    /// Control net: `rows[i][j]`, rows along u; all rows the same length. `weights` (same
    /// shape, plain numbers > 0) makes it rational.
    Points { rows: Vec<Vec<P3>>, degree_u: Option<u32>, degree_v: Option<u32>, weights: Option<Vec<Vec<f64>>> },
    /// Network of curves: `u_curves` in order across v, `v_curves` in order across u; every
    /// u-curve meets every v-curve. `tolerance` (default 1 mm) bounds both the gaps at the
    /// crossings and the deviation of the surface from the curves.
    Network { u_curves: Vec<Curve3>, v_curves: Vec<Curve3>, degree: Option<u32>, tolerance: Option<Len> },
    /// Spherical cap over the circle of plan `radius` around `center` (the rim height),
    /// rising `rise` (at most the radius; equal makes a hemisphere). Exact.
    Dome { center: P3, radius: Len, rise: Len },
    /// Segmental barrel vault along the level line `a` -> `b` (the springing), its arc
    /// spanning `span` and rising `rise` (at most half the span). Exact.
    Vault { a: P3, b: P3, span: Len, rise: Len },
}

/// Which way the shell grows from the surface (its normal follows u x v).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellSide {
    Normal,
    Reverse,
    Center,
}

/// Trim loops as `[u, v]` pairs of normalized surface parameters (0..1, plain numbers).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UvTrim {
    /// Kept region; none keeps the whole surface.
    pub outer: Option<Vec<[f64; 2]>>,
    /// Cut-outs inside the kept region.
    pub holes: Vec<Vec<[f64; 2]>>,
}

/// Target kind of `geometry.convert`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConvertTo {
    /// Closed mesh to a parametric solid.
    Solid,
    /// Solid to a plain mesh.
    Mesh,
    /// Mesh to the best native element (wall, slab, roof, column, beam).
    Parametric,
    Copies,
    Path,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Spacing {
    Step { d: Len },
    /// Spread `count` copies over `total`.
    Fit { total: Len },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Div {
    Count { n: u32 },
    Step { d: Len },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveAlign {
    Tangent,
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SurfAlign {
    Normal,
    Up,
}

/// Section 4.4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pattern {
    Linear { count: u32, dir: Dir3, spacing: Spacing },
    Radial { center: P3, axis: Dir3, count: u32, angle: Angle },
    AlongCurve {
        path: Path,
        count: Option<u32>,
        spacing: Option<Len>,
        align: CurveAlign,
        start_offset: Len,
    },
    OnSurface { face: Ref, u: Div, v: Div, align: SurfAlign },
    #[serde(rename = "grid_2d")]
    Grid2d { nx: u32, ny: u32, dx: Len, dy: Len, angle: Angle },
    Distribute { a: Ref, b: Ref, count: u32, ends: bool },
}

/// Pattern output: plain copies (default, export-friendly) or one re-evaluating element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatternOut {
    #[default]
    Copies,
    Element,
}

#[cfg(test)]
mod member_plate_tests {
    use super::*;

    fn rect_xz(x0: f64, x1: f64, z0: f64, z1: f64) -> Vec<P3> {
        vec![P3::m(x0, 2.0, z0), P3::m(x1, 2.0, z0), P3::m(x1, 2.0, z1), P3::m(x0, 2.0, z1)]
    }

    #[test]
    fn plate_orientation_picks_slab_or_wall_and_refuses_the_rest() {
        let flat = vec![P3::m(0.0, 0.0, 3.0), P3::m(4.0, 0.0, 3.0), P3::m(4.0, 2.0, 3.0)];
        assert_eq!(plate_kind(&flat), Ok(PlateKind::Slab));
        assert_eq!(plate_kind(&rect_xz(0.0, 12.0, 0.0, 4.0)), Ok(PlateKind::Wall));
        let tilted = vec![P3::m(0.0, 0.0, 0.0), P3::m(4.0, 0.0, 0.0), P3::m(4.0, 2.0, 1.0)];
        assert!(plate_kind(&tilted).unwrap_err().contains("tilted"));
        let warped = vec![P3::m(0.0, 0.0, 0.0), P3::m(4.0, 0.0, 0.0), P3::m(4.0, 2.0, 0.0), P3::m(0.0, 2.0, 0.5)];
        assert!(plate_kind(&warped).unwrap_err().contains("flat"));
        assert!(plate_kind(&flat[..2]).is_err());
        let line = vec![P3::m(0.0, 0.0, 0.0), P3::m(1.0, 0.0, 0.0), P3::m(2.0, 0.0, 0.0)];
        assert!(plate_kind(&line).unwrap_err().contains("area"));
    }

    #[test]
    fn member_and_plate_name_their_rows_and_read_from_json() {
        let j = serde_json::json!({
            "action": "member", "slot": 3, "level": null,
            "start": {"x": 0.0, "y": 0.0, "z": 0.0, "unit": "m"},
            "end": {"x": 5.0, "y": 0.0, "z": 0.0, "unit": "m"},
            "section": "CHS 88.9x5", "role": "gridshell_bar", "rotation": null
        });
        let g: GeoOp = serde_json::from_value(j).unwrap();
        assert!(matches!(g, GeoOp::Member { role: MemberRole::GridshellBar, .. }));
        assert_eq!(g.made_cat(), None);
        let mk = |role| GeoOp::Member {
            slot: 0,
            level: None,
            start: P3::m(0.0, 0.0, 0.0),
            end: P3::m(1.0, 0.0, 0.0),
            section: "CHS 88.9x5".into(),
            role,
            rotation: None,
        };
        assert_eq!(mk(MemberRole::Beam).made_cat(), Some(Cat::Beam));
        assert_eq!(mk(MemberRole::Brace).made_cat(), Some(Cat::Brace));
        let plate = |outline| GeoOp::Plate {
            slot: 1,
            level: None,
            outline,
            thickness: Len::mm(8.0),
            openings: vec![],
            material: None,
        };
        assert_eq!(plate(rect_xz(0.0, 3.0, 0.0, 2.0)).made_cat(), Some(Cat::Wall));
        let back: GeoOp = serde_json::from_value(serde_json::to_value(plate(rect_xz(0.0, 3.0, 0.0, 2.0))).unwrap()).unwrap();
        assert_eq!(back, plate(rect_xz(0.0, 3.0, 0.0, 2.0)));
        // unknown fields stay refused on the new variants
        let bad = serde_json::json!({"action": "plate", "slot": 1, "level": null, "outline": [],
            "thickness": {"v": 8.0, "unit": "mm"}, "openings": [], "material": null, "holes": []});
        assert!(serde_json::from_value::<GeoOp>(bad).is_err());
    }
}
