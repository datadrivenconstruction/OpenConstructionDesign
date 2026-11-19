// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Direct-edit grips for all element kinds (plan; levels use section coordinates).
//! Flow: [`grips`] -> drag with [`snap`] -> [`compute`] yields a new spec (nothing written,
//! it is also the live preview [`outline`]) -> [`apply`] writes it as one model operation.
//! Temporary dimensions of a grip [`dims`]: click a dimension -> type an exact value -> [`apply_value`].
//! Toggle grips ([`is_click`]: flip, mark, 2D/3D, mirror) fire on a click.
//! Axes and levels: in a view with 2D bounds, ends and marks edit the view record, otherwise the model (3D).

use ddc_geom::arc::{edge_mid, ArcAxis, ArcError};
use ddc_geom::{v2, Tol, V2};
use ddc_model::{
    BeamSpec, CeilingSpec, ColumnSpec, CompHost, CompSpec, CurtainSpec, DatumExtent, DatumKind, Error,
    GridSpec, Model, ParamKind, ParamValue, RoofKind, RoofSpec, RunSpec, StairShape,
    StairSpec, MODEL_VIEW,
};

use crate::temp_dims::{self, TempDim};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Wall,
    Slab,
    Ceiling,
    Roof,
    Column,
    Beam,
    Stair,
    Railing,
    Grid,
    /// Level: a grip in section coordinates (x along, y elevation).
    Level,
    Comp,
    Mep,
    /// Placed door or window: slide along its wall and flips (`grips_opening`).
    Opening,
    /// Imported body: one move grip at the centre of its plan box.
    Mesh,
    Shaft,
    /// Curtain wall axis: ends, move, arc bulge.
    Curtain,
    Footing,
    StripFooting,
    Raft,
    Brace,
    Truss,
    BeamSystem,
    Ramp,
    /// Body, mass, terrain: one move grip.
    Solid,
    Mass,
    Topo,
    /// Zone: one move grip, the zone has no outline of its own.
    Zone,
    /// Reference plane: ends and move.
    RefPlane,
    /// Room: boundary vertices of a sketched room, the room point otherwise.
    Room,
    /// Room separator: move and both ends.
    RoomSep,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    /// End 0/1: wall, beam, axis (bound), top of a tilted column (1), extruded roof.
    End(u8),
    /// Wall flip arrow (layers to the other side); fires on a click.
    Flip,
    /// Outline vertex (slab, ceiling, roof) or path vertex (railing, network).
    Vertex(usize),
    /// Outline edge midpoint: the edge moves along its normal.
    Edge(usize),
    /// Insertion point (column, component, stair).
    Move,
    /// Rotation about the insertion point.
    Rotate,
    /// Stair flight length (tread = length / number of treads).
    Run,
    Elevation,
    /// Component length parameter along local axis 0 (x) or 1 (y).
    Param(String, u8),
    /// Arc wall midpoint: curvature (bulge) through three points.
    Bulge,
    /// Elevation of beam end 0/1 (grip offset along the axis normal equals a z change).
    Z(u8),
    /// Stair flight edge (the opposite edge stays); for a stair railing, its edge.
    Width,
    /// Landing of an L/U stair along the first flight: the first flight's riser count.
    Landing,
    /// Far edge of a U stair landing: the landing depth.
    LandingDepth,
    /// Mark at end 0/1 of an axis or level; a click shows/hides it.
    Bubble(u8),
    /// 2D/3D bounds toggle at end 0/1 in a view; a click.
    Mode(u8),
    /// Vertex / edge midpoint of a slab or ceiling hole (ring h).
    HoleVertex(usize, usize),
    HoleEdge(usize, usize),
    /// Midpoint of network run i->i+1: dragging inserts a vertex.
    Insert(usize),
    /// Mirroring of an instance on a wall: by width (hinges) and by wall side.
    FlipHand,
    FlipFacing,
}

/// The grip fires on a click (no dragging).
pub fn is_click(r: &Role) -> bool {
    matches!(
        r,
        Role::Flip | Role::Bubble(_) | Role::Mode(_) | Role::FlipHand | Role::FlipFacing
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grip {
    pub kind: Kind,
    pub id: i64,
    pub role: Role,
    pub pos: V2,
    /// View key ("plan:<level>", "section:<id>"); empty means the model bounds.
    pub view: String,
}

/// Centre of the plan box of an imported body.
pub fn mesh_center(m: &Model, id: i64) -> Result<V2, Error> {
    let im = m.imported_mesh(id)?;
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for p in &im.mesh.positions {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    if lo.x > hi.x {
        return Err(bad());
    }
    Ok((lo + hi) * 0.5)
}

/// The element's new state after a grip drag.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Wall(i64, V2, V2),
    Slab(i64, Vec<V2>, Vec<Vec<(f64, f64)>>),
    Ceiling(i64, CeilingSpec),
    Roof(i64, RoofSpec),
    Column(i64, ColumnSpec),
    Beam(i64, BeamSpec),
    Stair(i64, StairSpec),
    Railing(i64, Vec<(f64, f64, f64)>),
    Grid(i64, GridSpec),
    /// Reference plane with new ends.
    RefPlane(i64, V2, V2),
    Level(i64, String, f64),
    Comp(i64, CompSpec),
    Mep(i64, RunSpec),
    /// Door or window: new size and position, swing, and its plan extent for the preview.
    Opening(i64, ddc_model::OpeningSpec, ddc_model::Swing, V2, V2),
    /// Imported body: shift in plan and the new centre (for the preview).
    Mesh(i64, V2, V2),
    /// Curtain wall with a new axis (ends or arc); panels follow their world position.
    Curtain(i64, CurtainSpec),
    /// Curtain wall moved as a whole: panels keep their cells. The spec is the preview.
    CurtainShift(i64, V2, CurtainSpec),
    /// Foundations, framing and ramps (`elem::struct_grips`).
    Struct(crate::elem::struct_grips::StructChange),
    /// Move of a body, mass or terrain by an offset (kind, id, d).
    Shift(Kind, i64, V2),
    /// Wall curvature (ends are for the preview).
    WallBulge(i64, V2, V2, f64),
    /// View bounds record (None restores the model ones); for a level the ends' y is the elevation.
    Datum(String, DatumKind, i64, Option<DatumExtent>),
}

/// Rotation grip arm, m.
pub const ARM: f64 = 0.8;
/// Offset of beam elevation grips from the ends along the normal, m.
pub const ZOFF: f64 = 0.45;
/// Offset of the mark beyond the axis/level end, m.
pub const BUB: f64 = 0.9;

fn p2(p: V2) -> (f64, f64) {
    (p.x, p.y)
}

fn pv((x, y): (f64, f64)) -> V2 {
    v2(x, y)
}

fn mid(a: V2, b: V2) -> V2 {
    (a + b) * 0.5
}

fn polar(a: f64, r: f64) -> V2 {
    v2(a.cos() * r, a.sin() * r)
}

fn wall_axis(m: &Model, id: i64) -> Result<(V2, V2), Error> {
    Ok(m.sql()
        .query_row("SELECT ax, ay, bx, by FROM wall WHERE id=?1", [id], |r| {
            Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
        })?)
}

fn ceiling_spec(m: &Model, id: i64) -> Result<CeilingSpec, Error> {
    let (level, slab_type, height) = m.sql().query_row(
        "SELECT level, stype, height FROM ceiling WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut rings = m.ceiling_rings(id)?;
    let outer = if rings.is_empty() {
        vec![]
    } else {
        rings.remove(0)
    };
    Ok(CeilingSpec {
        level,
        slab_type,
        height,
        outer,
        holes: rings,
    })
}

fn level(m: &Model, id: i64) -> Result<(String, f64), Error> {
    m.level_list()?
        .into_iter()
        .find(|l| l.0 == id)
        .map(|l| (l.1, l.2))
        .ok_or_else(|| Error::Invalid(format!("no such level {id}")))
}

fn grid(m: &Model, id: i64) -> Result<GridSpec, Error> {
    let g = m.grid(id)?;
    Ok(GridSpec {
        name: Some(g.name),
        ..g.spec
    })
}

/// Instance length parameters (name, value) from the component family.
fn comp_lengths(m: &Model, s: &CompSpec) -> Result<Vec<(String, f64)>, Error> {
    let fam: i64 =
        m.sql()
            .query_row("SELECT family FROM comp_type WHERE id=?1", [s.ctype], |r| {
                r.get(0)
            })?;
    let f = m.family(fam)?;
    let tv = m.comp_type_values(s.ctype)?;
    let num = |n: &str, d: &ParamValue| {
        let v = s
            .values
            .iter()
            .chain(&tv)
            .find(|x| x.0 == n)
            .map(|x| &x.1)
            .unwrap_or(d);
        match v {
            ParamValue::Num(x) => Some(*x),
            ParamValue::Text(_) => None,
        }
    };
    Ok(f.params
        .iter()
        .filter(|p| p.kind == ParamKind::Length && p.instance && p.formula.is_none())
        .filter_map(|p| num(&p.name, &p.default).map(|v| (p.name.clone(), v)))
        .take(2)
        .collect())
}

fn comp_xy(m: &Model, s: &CompSpec) -> Result<(V2, f64), Error> {
    Ok(match s.host {
        CompHost::Wall(w) => {
            let (a, b) = wall_axis(m, w)?;
            let d = b - a;
            let u = d * (1.0 / d.len().max(1e-12));
            let n = v2(-u.y, u.x);
            (a + u * s.at.0 + n * s.at.1, u.y.atan2(u.x) + s.rotation)
        }
        _ => (v2(s.at.0, s.at.1), s.rotation),
    })
}

pub(crate) fn ring_grips(kind: Kind, id: i64, ring: &[V2], out: &mut Vec<Grip>) {
    ring_grips_as(kind, id, ring, out, Role::Vertex, Role::Edge);
}

fn ring_grips_as(
    kind: Kind,
    id: i64,
    ring: &[V2],
    out: &mut Vec<Grip>,
    v: impl Fn(usize) -> Role,
    e: impl Fn(usize) -> Role,
) {
    let n = ring.len();
    for (i, &p) in ring.iter().enumerate() {
        out.push(Grip {
            kind,
            id,
            role: v(i),
            pos: p,
            view: String::new(),
        });
        out.push(Grip {
            kind,
            id,
            role: e(i),
            pos: mid(p, ring[(i + 1) % n]),
            view: String::new(),
        });
    }
}

/// Insert-vertex grips at a quarter of each outer edge (the midpoint grip shifts the edge).
fn insert_grips(kind: Kind, id: i64, ring: &[V2], out: &mut Vec<Grip>) {
    let n = ring.len();
    for (i, &p) in ring.iter().enumerate() {
        let q = ring[(i + 1) % n];
        out.push(Grip {
            kind,
            id,
            role: Role::Insert(i),
            pos: p + (q - p) * 0.25,
            view: String::new(),
        });
    }
}

fn hole_grips(kind: Kind, id: i64, holes: &[Vec<(f64, f64)>], out: &mut Vec<Grip>) {
    for (h, r) in holes.iter().enumerate() {
        let r: Vec<V2> = r.iter().map(|&p| pv(p)).collect();
        ring_grips_as(
            kind,
            id,
            &r,
            out,
            |i| Role::HoleVertex(h, i),
            |i| Role::HoleEdge(h, i),
        );
    }
}

/// Hole h and the role of a vertex/edge inside it.
fn hole_role(r: &Role) -> Option<(usize, Role)> {
    match r {
        Role::HoleVertex(h, i) => Some((*h, Role::Vertex(*i))),
        Role::HoleEdge(h, i) => Some((*h, Role::Edge(*i))),
        _ => None,
    }
}

fn edit_rings(
    outer: &mut Vec<V2>,
    holes: &mut [Vec<(f64, f64)>],
    role: &Role,
    grab: V2,
    to: V2,
) -> Result<(), Error> {
    match hole_role(role) {
        Some((h, r)) => {
            let hr = holes.get_mut(h).ok_or_else(bad)?;
            let mut v: Vec<V2> = hr.iter().map(|&p| pv(p)).collect();
            ring_edit(&mut v, &r, grab, to)?;
            *hr = v.into_iter().map(p2).collect();
            Ok(())
        }
        None => ring_edit(outer, role, grab, to),
    }
}

fn unit(d: V2) -> V2 {
    d * (1.0 / d.len().max(1e-12))
}

/// Left normal of a direction.
fn left(u: V2) -> V2 {
    v2(-u.y, u.x)
}

/// Right normal (as for a stair flight).
fn right(u: V2) -> V2 {
    v2(u.y, -u.x)
}

/// Host of a stair railing: (stair, side sign relative to the flight's right normal).
fn railing_host(m: &Model, id: i64) -> Result<Option<(i64, usize)>, Error> {
    let (st, side): (Option<i64>, Option<i64>) =
        m.sql()
            .query_row("SELECT stair, side FROM railing WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
    Ok(st.map(|s| (s, side.unwrap_or(0).clamp(0, 1) as usize)))
}

/// Midpoint of a stair railing edge and the side sign (+1 is right of the climb direction).
fn railing_edge(m: &Model, stair: i64, side: usize) -> Result<(V2, f64), Error> {
    let s = m.stair(stair)?;
    let paths = m.stair_layout(stair)?.edge_paths();
    let p = &paths[side];
    let q = p
        .get(p.len() / 2)
        .ok_or_else(|| Error::Invalid("stair railing has no path".into()))?;
    let at = v2(q.x, q.y);
    let o = pv(s.origin);
    let sign = if (at - o).dot(right(polar(s.angle, 1.0))) >= 0.0 {
        1.0
    } else {
        -1.0
    };
    Ok((at, sign))
}

/// The flight edge on side `sign` moves outward by `delta`, the opposite one stays.
fn stair_edge(s: &mut StairSpec, sign: f64, delta: f64) -> Result<(), Error> {
    let w = s.width + delta;
    if w < 0.3 {
        return Err(Error::Invalid("stair width must be >= 0.3 m".into()));
    }
    let rt = right(polar(s.angle, 1.0)) * sign;
    s.origin = p2(pv(s.origin) + rt * (delta / 2.0));
    s.width = w;
    Ok(())
}

/// Landing depth and the first flight's riser count (L/U); a straight stair gives None.
fn landing_of(m: &Model, id: i64, s: &StairSpec) -> Result<Option<(f64, usize, f64)>, Error> {
    let depth = match s.shape {
        StairShape::Straight | StairShape::Spiral { .. } => return Ok(None),
        StairShape::L { .. } => s.width,
        StairShape::U { .. } => s.landing,
    };
    let l = m.stair_layout(id)?;
    let k1 = l.flights.first().map(|f| f.risers).unwrap_or(2);
    Ok(Some((depth, k1, l.tread)))
}

fn datum_kind(k: Kind) -> DatumKind {
    if k == Kind::Level {
        DatumKind::Level
    } else {
        DatumKind::Grid
    }
}

/// All grips of an element (model bounds of axes and levels).
pub fn grips(m: &Model, kind: Kind, id: i64) -> Result<Vec<Grip>, Error> {
    grips_in(m, kind, id, "")
}

/// Element grips in view `view` (axes and levels: the view's 2D bounds and the 2D/3D toggle).
pub fn grips_in(m: &Model, kind: Kind, id: i64, view: &str) -> Result<Vec<Grip>, Error> {
    let mut out = vec![];
    let g = |role, pos| Grip {
        kind,
        id,
        role,
        pos,
        view: view.to_string(),
    };
    match kind {
        Kind::Wall => {
            let (a, b) = wall_axis(m, id)?;
            let d = b - a;
            let n = v2(-d.y, d.x) * (1.0 / d.len().max(1e-12));
            out.push(g(Role::End(0), a));
            out.push(g(Role::End(1), b));
            out.push(g(Role::Flip, mid(a, b) + n * 0.6));
            let bu = m.wall_bulge(id)?;
            if bu != 0.0 {
                out.push(g(Role::Bulge, edge_mid(a, b, bu)));
            }
        }
        Kind::Slab => {
            let r = m.slab_rings(id)?;
            let ring: Vec<V2> = r.first().into_iter().flatten().map(|&p| pv(p)).collect();
            ring_grips(kind, id, &ring, &mut out);
            insert_grips(kind, id, &ring, &mut out);
            hole_grips(kind, id, r.get(1..).unwrap_or(&[]), &mut out);
        }
        Kind::Ceiling => {
            let s = ceiling_spec(m, id)?;
            let ring: Vec<V2> = s.outer.iter().map(|&p| pv(p)).collect();
            ring_grips(kind, id, &ring, &mut out);
            insert_grips(kind, id, &ring, &mut out);
            hole_grips(kind, id, &s.holes, &mut out);
        }
        Kind::Roof => {
            let s = m.roof(id)?;
            out.push(g(Role::Move, pv(crate::elem::roof::center(&s))));
            match s.kind {
                RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => {
                    let ring: Vec<V2> = ring.iter().map(|&p| pv(p)).collect();
                    ring_grips(kind, id, &ring, &mut out);
                    insert_grips(kind, id, &ring, &mut out);
                }
                RoofKind::Extrusion { a, b, .. } => {
                    out.push(g(Role::End(0), pv(a)));
                    out.push(g(Role::End(1), pv(b)));
                }
            }
        }
        Kind::Column => {
            let c = m.column(id)?;
            let at = pv(c.at);
            out.push(g(Role::Move, at));
            out.push(g(Role::Rotate, at + polar(c.rotation, ARM)));
            if let Some(t) = c.top_at {
                out.push(g(Role::End(1), pv(t)));
            }
        }
        Kind::Beam => {
            let bm = m.beam(id)?;
            let (a, b) = (v2(bm.a.0, bm.a.1), v2(bm.b.0, bm.b.1));
            let n = left(unit(b - a));
            out.push(g(Role::End(0), a));
            out.push(g(Role::End(1), b));
            out.push(g(Role::Rotate, mid(a, b) + n * ARM));
            out.push(g(Role::Z(0), a + n * ZOFF));
            out.push(g(Role::Z(1), b + n * ZOFF));
        }
        Kind::Stair => {
            let s = m.stair(id)?;
            let l = m.stair_layout(id)?;
            let o = pv(s.origin);
            out.push(g(Role::Move, o));
            out.push(g(Role::Rotate, o - polar(s.angle, ARM)));
            let run = l.tread * (l.risers.saturating_sub(1)) as f64;
            out.push(g(Role::Run, o + polar(s.angle, run)));
            let u = polar(s.angle, 1.0);
            out.push(g(
                Role::Width,
                o + right(u) * (s.width / 2.0) + u * (l.tread / 2.0),
            ));
            if let Some((depth, k1, t)) = landing_of(m, id, &s)? {
                let l1 = t * (k1 - 1) as f64;
                out.push(g(Role::Landing, o + u * (l1 + depth / 2.0)));
                if matches!(s.shape, StairShape::U { .. }) {
                    out.push(g(Role::LandingDepth, o + u * (l1 + depth)));
                }
            }
        }
        Kind::Railing => match railing_host(m, id)? {
            Some((st, side)) => {
                let (at, _) = railing_edge(m, st, side)?;
                out.push(g(Role::Width, at));
            }
            None => {
                for (i, p) in m.railing_path(id)?.iter().enumerate() {
                    out.push(g(Role::Vertex(i), v2(p.0, p.1)));
                }
            }
        },
        Kind::Grid => {
            let e = m.datum_effective(view, DatumKind::Grid, id)?;
            let (a, b) = (pv(e.a), pv(e.b));
            let u = unit(b - a);
            // on an arc the marks go along the tangent at the ends: the chord rotated by minus/plus half the sweep
            let half = 2.0 * m.grid(id)?.spec.bulge.atan();
            let rot = |t: f64| v2(u.x * t.cos() - u.y * t.sin(), u.x * t.sin() + u.y * t.cos());
            out.push(g(Role::End(0), a));
            out.push(g(Role::End(1), b));
            out.push(g(Role::Bubble(0), a - rot(-half) * BUB));
            out.push(g(Role::Bubble(1), b + rot(half) * BUB));
            if !view.is_empty() {
                out.push(g(Role::Mode(0), a + left(u) * 0.6));
                out.push(g(Role::Mode(1), b + left(u) * 0.6));
            }
        }
        Kind::Level => {
            let (_, z) = level(m, id)?;
            let e = m.datum_effective(view, DatumKind::Level, id)?;
            let (x0, x1) = (e.a.0, e.b.0);
            let sg = if x1 >= x0 { 1.0 } else { -1.0 };
            out.push(g(Role::Elevation, v2((x0 + x1) / 2.0, z)));
            out.push(g(Role::End(0), v2(x0, z)));
            out.push(g(Role::End(1), v2(x1, z)));
            out.push(g(Role::Bubble(0), v2(x0 - sg * BUB, z)));
            out.push(g(Role::Bubble(1), v2(x1 + sg * BUB, z)));
            if !view.is_empty() {
                out.push(g(Role::Mode(0), v2(x0, z + 0.5)));
                out.push(g(Role::Mode(1), v2(x1, z + 0.5)));
            }
        }
        Kind::Opening => return crate::grips_opening::grips(m, id, view),
        Kind::Comp => {
            let s = m.component(id)?;
            let (o, rot) = comp_xy(m, &s)?;
            out.push(g(Role::Move, o));
            out.push(g(Role::Rotate, o + polar(rot, ARM)));
            for (k, (name, v)) in comp_lengths(m, &s)?.into_iter().enumerate() {
                let ang = rot + k as f64 * std::f64::consts::FRAC_PI_2;
                out.push(g(Role::Param(name, k as u8), o + polar(ang, v)));
            }
            if matches!(s.host, CompHost::Wall(_)) {
                out.push(g(Role::FlipHand, o - polar(rot, 0.5)));
                out.push(g(
                    Role::FlipFacing,
                    o + polar(rot - std::f64::consts::FRAC_PI_2, 0.5),
                ));
            }
        }
        Kind::Curtain => {
            let s = m.curtain_wall(id)?;
            let (a, b) = (pv(s.a), pv(s.b));
            out.push(g(Role::End(0), a));
            out.push(g(Role::End(1), b));
            out.push(g(Role::Move, mid(a, b)));
            if s.bulge != 0.0 {
                out.push(g(Role::Bulge, edge_mid(a, b, s.bulge)));
            }
        }
        Kind::Footing
        | Kind::StripFooting
        | Kind::Raft
        | Kind::Brace
        | Kind::Truss
        | Kind::BeamSystem
        | Kind::Ramp
        | Kind::Shaft
        | Kind::Zone
        | Kind::Room
        | Kind::RoomSep => {
            let from = out.len();
            crate::elem::struct_grips::grips_of(m, kind, id, &mut out)?;
            for x in &mut out[from..] {
                x.view = view.to_string();
            }
        }
        Kind::RefPlane => {
            let (a, b) = crate::elem::datum::ref_plane_ends(m, id)?;
            out.push(g(Role::End(0), a));
            out.push(g(Role::End(1), b));
            out.push(g(Role::Move, mid(a, b)));
        }
        Kind::Mep => {
            let pts = m.mep_run(id)?.points;
            for (i, p) in pts.iter().enumerate() {
                out.push(g(Role::Vertex(i), v2(p.0, p.1)));
            }
            for (i, w) in pts.windows(2).enumerate() {
                out.push(g(
                    Role::Insert(i),
                    mid(v2(w[0].0, w[0].1), v2(w[1].0, w[1].1)),
                ));
            }
        }
        Kind::Mesh => out.push(g(Role::Move, mesh_center(m, id)?)),
        Kind::Solid | Kind::Mass | Kind::Topo => out.push(g(Role::Move, shift_center(m, kind, id)?)),
    }
    Ok(out)
}

/// Plan center of a body, mass or terrain: the move grip goes here.
fn shift_center(m: &Model, kind: Kind, id: i64) -> Result<V2, Error> {
    let from_box = |lo: (f64, f64), hi: (f64, f64)| v2((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
    match kind {
        Kind::Solid => {
            let q = m.solid_quantities(id)?.ok_or_else(bad)?;
            Ok(from_box((q.bbox[0], q.bbox[1]), (q.bbox[3], q.bbox[4])))
        }
        Kind::Mass => {
            let (lo, hi) = m.mass_body(id)?.bbox().ok_or_else(bad)?;
            Ok(from_box((lo.x, lo.y), (hi.x, hi.y)))
        }
        _ => {
            let pts = m.topo_points(id)?;
            if pts.is_empty() {
                return Err(bad());
            }
            let n = pts.len() as f64;
            Ok(v2(
                pts.iter().map(|p| p.1 .0).sum::<f64>() / n,
                pts.iter().map(|p| p.1 .1).sum::<f64>() / n,
            ))
        }
    }
}

/// Model edit target for kinds that support moving.
fn shift_target(kind: Kind, id: i64) -> Result<ddc_model::Target, Error> {
    Ok(match kind {
        Kind::Solid => ddc_model::Target::Solid(id),
        Kind::Mass => ddc_model::Target::Mass(id),
        Kind::Topo => ddc_model::Target::Topo(id),
        _ => return Err(bad()),
    })
}

fn edge_shift(ring: &mut [V2], i: usize, grab: V2, to: V2) {
    let n = ring.len();
    let (a, b) = (ring[i], ring[(i + 1) % n]);
    let d = b - a;
    let nrm = v2(-d.y, d.x) * (1.0 / d.len().max(1e-12));
    let off = nrm * (to - grab).dot(nrm);
    ring[i] = a + off;
    ring[(i + 1) % n] = b + off;
}

pub(crate) fn ring_edit(ring: &mut Vec<V2>, role: &Role, grab: V2, to: V2) -> Result<(), Error> {
    match *role {
        Role::Vertex(i) if i < ring.len() => ring[i] = to,
        Role::Edge(i) if i < ring.len() => edge_shift(ring, i, grab, to),
        Role::Insert(i) if i < ring.len() => ring.insert(i + 1, to),
        _ => return Err(Error::Invalid("grip does not fit the element".into())),
    }
    Ok(())
}

fn bad() -> Error {
    Error::Invalid("grip does not fit the element".into())
}

/// Minimum segment length, m (as in the model).
const MIN: f64 = 1e-3;

fn degenerate(what: &str) -> Error {
    Error::Degenerate(what.into())
}

fn seg(what: &str, a: V2, b: V2) -> Result<(), Error> {
    if (b - a).len() < MIN {
        return Err(degenerate(&format!("{what} length would be zero")));
    }
    Ok(())
}

fn simple(what: &str, r: &[V2]) -> Result<(), Error> {
    ddc_geom::poly::simple_ring(r, MIN).map_err(|e| degenerate(&format!("{what}: {e}")))
}

/// Merges coincident neighboring polyline vertices; fewer than two is degenerate.
fn merge3(what: &str, p: &mut Vec<(f64, f64, f64)>) -> Result<(), Error> {
    let d = |a: (f64, f64, f64), b: (f64, f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
    p.dedup_by(|b, a| d(*a, *b) < MIN);
    if p.len() < 2 {
        return Err(degenerate(&format!("{what} would collapse to a point")));
    }
    Ok(())
}

/// Refusal of an edit that would degenerate the element: the grip returns, the model is untouched.
fn check(c: &Change) -> Result<(), Error> {
    match c {
        Change::Wall(_, a, b) => seg("wall", *a, *b),
        Change::Beam(_, s) => {
            let l = ((s.b.0 - s.a.0).powi(2) + (s.b.1 - s.a.1).powi(2) + (s.b.2 - s.a.2).powi(2))
                .sqrt();
            if l < MIN {
                return Err(degenerate("beam length would be zero"));
            }
            Ok(())
        }
        Change::Grid(_, s) => seg("grid", pv(s.a), pv(s.b)),
        Change::Slab(_, r, holes) => {
            simple("slab boundary", r)?;
            for h in holes {
                let h: Vec<V2> = h.iter().map(|&p| pv(p)).collect();
                simple("slab hole", &h)?;
            }
            Ok(())
        }
        Change::Ceiling(_, s) => {
            let r: Vec<V2> = s.outer.iter().map(|&p| pv(p)).collect();
            simple("ceiling boundary", &r)?;
            for h in &s.holes {
                let h: Vec<V2> = h.iter().map(|&p| pv(p)).collect();
                simple("ceiling hole", &h)?;
            }
            Ok(())
        }
        Change::Struct(crate::elem::struct_grips::StructChange::ZoneShaft(
            crate::elem::zone_shaft::ZsChange::Shaft(_, s),
        )) => {
            let r: Vec<V2> = s.ring.iter().map(|&p| pv(p)).collect();
            simple("shaft boundary", &r)
        }
        Change::Datum(_, DatumKind::Grid, _, Some(e)) => seg("grid", pv(e.a), pv(e.b)),
        Change::Datum(_, DatumKind::Level, _, Some(e)) => {
            seg("level", v2(e.a.0, 0.0), v2(e.b.0, 0.0))
        }
        Change::RefPlane(_, a, b) => seg("reference plane", *a, *b),
        Change::Curtain(_, s) | Change::CurtainShift(_, _, s) => {
            seg("curtain wall", pv(s.a), pv(s.b))?;
            if s.bulge.abs() > 1.0 {
                return Err(degenerate("curtain wall arc cannot exceed a half circle"));
            }
            Ok(())
        }
        Change::Roof(_, s) => match &s.kind {
            RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => {
                let r: Vec<V2> = ring.iter().map(|&p| pv(p)).collect();
                simple("roof footprint", &r)
            }
            RoofKind::Extrusion { a, b, .. } => seg("roof extrusion", pv(*a), pv(*b)),
        },
        _ => Ok(()),
    }
}

/// End of an extruded roof's line: the profile stretches over the span, its point count (and the
/// numbers of the dimensionless `roof_ext.bulges` arcs) does not change, so vaults and arcs stay on their spans.
fn stretch(a: &mut (f64, f64), b: &mut (f64, f64), profile: &mut [(f64, f64)], end: u8, to: V2) {
    let old = (pv(*b) - pv(*a)).len();
    if end == 0 {
        *a = p2(to);
    } else {
        *b = p2(to);
    }
    let new = (pv(*b) - pv(*a)).len();
    if old > MIN && new > MIN {
        let k = new / old;
        profile.iter_mut().for_each(|p| p.0 *= k);
    }
}

/// The new state when grip `g` is moved to point `to` (nothing written to the model).
pub fn compute(m: &Model, g: &Grip, to: V2) -> Result<Change, Error> {
    let c = compute_raw(m, g, to)?;
    check(&c)?;
    Ok(c)
}

fn compute_raw(m: &Model, g: &Grip, to: V2) -> Result<Change, Error> {
    let id = g.id;
    Ok(match g.kind {
        Kind::Wall => {
            let (a, b) = wall_axis(m, id)?;
            match g.role {
                Role::End(0) => Change::Wall(id, to, b),
                Role::End(1) => Change::Wall(id, a, to),
                Role::Flip => Change::Wall(id, b, a),
                Role::Bulge => {
                    let bu = match ArcAxis::through(a, to, b, Tol::default()) {
                        Ok(ax) => ax.bulge,
                        // a point on the chord makes the wall straight again
                        Err(ArcError::NoCircle | ArcError::Straight) => 0.0,
                        Err(e) => return Err(degenerate(&format!("arc wall: {e:?}"))),
                    };
                    Change::WallBulge(id, a, b, bu)
                }
                _ => return Err(bad()),
            }
        }
        Kind::Slab => {
            let mut rings = m.slab_rings(id)?;
            let mut ring: Vec<V2> = rings.remove(0).into_iter().map(pv).collect();
            edit_rings(&mut ring, &mut rings, &g.role, g.pos, to)?;
            ring::rings_ok(&ring, &rings)?;
            Change::Slab(id, ring, rings)
        }
        Kind::Ceiling => {
            let mut s = ceiling_spec(m, id)?;
            let mut ring: Vec<V2> = s.outer.iter().map(|&p| pv(p)).collect();
            edit_rings(&mut ring, &mut s.holes, &g.role, g.pos, to)?;
            ring::rings_ok(&ring, &s.holes)?;
            s.outer = ring.into_iter().map(p2).collect();
            Change::Ceiling(id, s)
        }
        Kind::Roof => {
            let mut s = m.roof(id)?;
            if g.role == Role::Move {
                let d = to - g.pos;
                return Ok(Change::Roof(id, crate::elem::roof::translated(&s, (d.x, d.y))));
            }
            match (&mut s.kind, &g.role) {
                (RoofKind::Footprint { ring, slopes, .. }, r) => {
                    let mut v: Vec<V2> = ring.iter().map(|&p| pv(p)).collect();
                    ring_edit(&mut v, r, g.pos, to)?;
                    // the new edge keeps the slope of the edge it was cut from
                    if let Role::Insert(i) = r {
                        if slopes.len() == ring.len() {
                            slopes.insert(i + 1, slopes[*i]);
                        }
                    }
                    *ring = v.into_iter().map(p2).collect();
                }
                (RoofKind::Arrow { ring, .. }, r) => {
                    let mut v: Vec<V2> = ring.iter().map(|&p| pv(p)).collect();
                    ring_edit(&mut v, r, g.pos, to)?;
                    *ring = v.into_iter().map(p2).collect();
                }
                (RoofKind::Extrusion { a, b, profile, .. }, Role::End(k @ (0 | 1))) => {
                    stretch(a, b, profile, *k, to)
                }
                _ => return Err(bad()),
            }
            Change::Roof(id, s)
        }
        Kind::Column => {
            let mut c = m.column(id)?;
            match g.role {
                Role::Move => {
                    let d = to - pv(c.at);
                    c.top_at = c.top_at.map(|t| p2(pv(t) + d));
                    c.at = p2(to);
                }
                Role::Rotate => {
                    let d = to - pv(c.at);
                    c.rotation = d.y.atan2(d.x);
                }
                Role::End(1) => c.top_at = Some(p2(to)),
                _ => return Err(bad()),
            }
            Change::Column(id, c)
        }
        Kind::Beam => {
            let mut b = m.beam(id)?;
            match g.role {
                Role::End(0) => b.a = (to.x, to.y, b.a.2),
                Role::End(1) => b.b = (to.x, to.y, b.b.2),
                Role::Rotate => {
                    let (pa, pb) = (v2(b.a.0, b.a.1), v2(b.b.0, b.b.1));
                    let c = mid(pa, pb);
                    let d = to - c;
                    if d.len() < MIN {
                        return Err(bad());
                    }
                    let cur = (pb - pa).y.atan2((pb - pa).x);
                    let da = d.y.atan2(d.x) - std::f64::consts::FRAC_PI_2 - cur;
                    let (sn, cs) = da.sin_cos();
                    let r = |p: V2| {
                        let q = p - c;
                        c + v2(q.x * cs - q.y * sn, q.x * sn + q.y * cs)
                    };
                    let (na, nb) = (r(pa), r(pb));
                    b.a = (na.x, na.y, b.a.2);
                    b.b = (nb.x, nb.y, b.b.2);
                }
                Role::Z(k) => {
                    let n = left(unit(v2(b.b.0 - b.a.0, b.b.1 - b.a.1)));
                    let dz = (to - g.pos).dot(n);
                    if k == 0 {
                        b.a.2 += dz;
                    } else {
                        b.b.2 += dz;
                    }
                }
                _ => return Err(bad()),
            }
            Change::Beam(id, b)
        }
        Kind::Stair => {
            let mut s = m.stair(id)?;
            let o = pv(s.origin);
            match g.role {
                Role::Move => s.origin = p2(to),
                Role::Rotate => {
                    let d = o - to;
                    s.angle = d.y.atan2(d.x);
                }
                Role::Run => {
                    let l = m.stair_layout(id)?;
                    let n = l.risers.saturating_sub(1).max(1) as f64;
                    let run = (to - o).dot(polar(s.angle, 1.0));
                    if run <= 1e-3 {
                        return Err(Error::Invalid("stair run must be > 0".into()));
                    }
                    s.tread = Some(run / n);
                }
                Role::Width => {
                    let rt = right(polar(s.angle, 1.0));
                    stair_edge(&mut s, 1.0, (to - g.pos).dot(rt))?;
                }
                Role::Landing => {
                    let (depth, _, t) = landing_of(m, id, &s)?.ok_or_else(bad)?;
                    let n = m.stair_layout(id)?.risers;
                    let proj = (to - o).dot(polar(s.angle, 1.0));
                    let k1 = (((proj - depth / 2.0) / t).round() + 1.0).max(2.0) as usize;
                    let k1 = k1.min(n.saturating_sub(2)).max(2);
                    match &mut s.shape {
                        StairShape::L { first, .. } | StairShape::U { first, .. } => {
                            *first = Some(k1)
                        }
                        StairShape::Straight | StairShape::Spiral { .. } => return Err(bad()),
                    }
                }
                Role::LandingDepth => {
                    let (_, k1, t) = landing_of(m, id, &s)?.ok_or_else(bad)?;
                    let l1 = t * (k1 - 1) as f64;
                    let depth = (to - o).dot(polar(s.angle, 1.0)) - l1;
                    if depth < 0.3 {
                        return Err(Error::Invalid("landing depth must be >= 0.3 m".into()));
                    }
                    s.landing = depth;
                }
                _ => return Err(bad()),
            }
            Change::Stair(id, s)
        }
        Kind::Railing if g.role == Role::Width => {
            let (st, side) = railing_host(m, id)?.ok_or_else(bad)?;
            let (_, sign) = railing_edge(m, st, side)?;
            let mut s = m.stair(st)?;
            let rt = right(polar(s.angle, 1.0)) * sign;
            stair_edge(&mut s, sign, (to - g.pos).dot(rt))?;
            Change::Stair(st, s)
        }
        Kind::Railing => {
            let mut p = m.railing_path(id)?;
            match g.role {
                Role::Vertex(i) if i < p.len() => p[i] = (to.x, to.y, p[i].2),
                _ => return Err(bad()),
            }
            merge3("railing", &mut p)?;
            Change::Railing(id, p)
        }
        Kind::Level if g.role == Role::Elevation => {
            let (name, _) = level(m, id)?;
            Change::Level(id, name, to.y)
        }
        Kind::Grid | Kind::Level => datum_change(m, g, to)?,
        Kind::Comp => {
            let mut s = m.component(id)?;
            let (o, rot) = comp_xy(m, &s)?;
            match &g.role {
                Role::Move => match s.host {
                    CompHost::Wall(w) => {
                        let (a, b) = wall_axis(m, w)?;
                        let u = (b - a) * (1.0 / (b - a).len().max(1e-12));
                        s.at.0 = (to - a).dot(u);
                    }
                    _ => {
                        s.at.0 = to.x;
                        s.at.1 = to.y;
                    }
                },
                Role::Rotate => {
                    let d = to - o;
                    s.rotation += d.y.atan2(d.x) - rot;
                }
                Role::Param(name, k) => {
                    let ang = rot + *k as f64 * std::f64::consts::FRAC_PI_2;
                    let v = (to - o).dot(polar(ang, 1.0));
                    if v <= 1e-3 {
                        return Err(Error::Invalid(format!("{name} must be > 0")));
                    }
                    s.values.retain(|x| &x.0 != name);
                    s.values.push((name.clone(), ParamValue::Num(v)));
                }
                Role::FlipHand => s.mirror = !s.mirror,
                Role::FlipFacing => {
                    // mirroring by wall side is a rotation by pi plus a mirror by width
                    s.mirror = !s.mirror;
                    let pi = std::f64::consts::PI;
                    s.rotation = (s.rotation + pi + pi).rem_euclid(2.0 * pi) - pi;
                }
                _ => return Err(bad()),
            }
            Change::Comp(id, s)
        }
        Kind::Opening => crate::grips_opening::compute(m, g, to)?,
        Kind::RefPlane => {
            let (a, b) = crate::elem::datum::ref_plane_ends(m, id)?;
            match g.role {
                Role::End(0) => Change::RefPlane(id, to, b),
                Role::End(1) => Change::RefPlane(id, a, to),
                Role::Move => {
                    let d = to - g.pos;
                    Change::RefPlane(id, a + d, b + d)
                }
                _ => return Err(bad()),
            }
        }
        Kind::Curtain => {
            let mut s = m.curtain_wall(id)?;
            let (a, b) = (pv(s.a), pv(s.b));
            match g.role {
                Role::End(0) => s.a = p2(to),
                Role::End(1) => s.b = p2(to),
                Role::Move => {
                    let d = to - g.pos;
                    s.a = p2(a + d);
                    s.b = p2(b + d);
                    return Ok(Change::CurtainShift(id, d, s));
                }
                Role::Bulge => {
                    s.bulge = match ArcAxis::through(a, to, b, Tol::default()) {
                        Ok(ax) => ax.bulge,
                        // a point on the chord: the wall is straight again
                        Err(ArcError::NoCircle | ArcError::Straight) => 0.0,
                        Err(e) => return Err(degenerate(&format!("curtain wall arc: {e:?}"))),
                    };
                }
                _ => return Err(bad()),
            }
            Change::Curtain(id, s)
        }
        Kind::Footing
        | Kind::StripFooting
        | Kind::Raft
        | Kind::Brace
        | Kind::Truss
        | Kind::BeamSystem
        | Kind::Ramp
        | Kind::Shaft
        | Kind::Zone
        | Kind::Room
        | Kind::RoomSep => Change::Struct(crate::elem::struct_grips::compute(m, g, to)?),
        Kind::Mep => {
            let mut s = m.mep_run(id)?;
            match g.role {
                Role::Vertex(i) if i < s.points.len() => {
                    s.points[i] = (to.x, to.y, s.points[i].2);
                }
                Role::Insert(i) if i + 1 < s.points.len() => {
                    let z = (s.points[i].2 + s.points[i + 1].2) / 2.0;
                    s.points.insert(i + 1, (to.x, to.y, z));
                }
                _ => return Err(bad()),
            }
            merge3("run", &mut s.points)?;
            Change::Mep(id, s)
        }
        Kind::Mesh => match g.role {
            Role::Move => Change::Mesh(id, to - mesh_center(m, id)?, to),
            _ => return Err(bad()),
        },
        Kind::Solid | Kind::Mass | Kind::Topo => match g.role {
            Role::Move => Change::Shift(g.kind, id, to - g.pos),
            _ => return Err(bad()),
        },
    })
}

/// End, mark or 2D/3D of an axis/level: the view record if the view has its own bounds, otherwise the model.
fn datum_change(m: &Model, g: &Grip, to: V2) -> Result<Change, Error> {
    let (dk, id, view) = (datum_kind(g.kind), g.id, g.view.clone());
    let own = if view.is_empty() {
        None
    } else {
        m.datum_extent(&view, dk, id)?
    };
    let mut e = m.datum_effective(&view, dk, id)?;
    let z = if dk == DatumKind::Level {
        level(m, id)?.1
    } else {
        0.0
    };
    match g.role {
        Role::Mode(_) if !view.is_empty() => {
            let next = if own.is_some() { None } else { Some(e) };
            return Ok(Change::Datum(view, dk, id, next));
        }
        Role::End(k) => {
            let p = if dk == DatumKind::Grid {
                // an axis end slides along its line: only the length changes, not the direction
                let (a, b) = (pv(e.a), pv(e.b));
                let u = unit(b - a);
                let fixed = if k == 0 { b } else { a };
                let q = a + u * (to - a).dot(u);
                if (q - fixed).len() <= 1e-3 {
                    return Err(degenerate("grid ends coincide"));
                }
                p2(q)
            } else {
                (to.x, z)
            };
            if k == 0 {
                e.a = p;
            } else {
                e.b = p;
            }
        }
        Role::Bubble(0) => e.bubbles.0 = !e.bubbles.0,
        Role::Bubble(_) => e.bubbles.1 = !e.bubbles.1,
        _ => return Err(bad()),
    }
    if dk == DatumKind::Level {
        e.a.1 = z;
        e.b.1 = z;
    }
    Ok(match (own, dk) {
        (Some(_), _) => Change::Datum(view, dk, id, Some(e)),
        (None, DatumKind::Grid) => {
            let mut s = grid(m, id)?;
            s.a = e.a;
            s.b = e.b;
            s.bubble_start = e.bubbles.0;
            s.bubble_end = e.bubbles.1;
            Change::Grid(id, s)
        }
        (None, DatumKind::Level) => Change::Datum(MODEL_VIEW.into(), dk, id, Some(e)),
    })
}

/// Deletes a vertex of a network path or sketch railing (Del on the grip); at least two remain.
pub fn delete_vertex(m: &mut Model, kind: Kind, id: i64, i: usize) -> Result<(), Error> {
    match kind {
        Kind::Mep => {
            let mut s = m.mep_run(id)?;
            if i >= s.points.len() || s.points.len() <= 2 {
                return Err(degenerate("run needs at least two points"));
            }
            s.points.remove(i);
            m.mep_edit_run(id, &s)
        }
        Kind::Railing => {
            let mut p = m.railing_path(id)?;
            if i >= p.len() || p.len() <= 2 {
                return Err(degenerate("railing needs at least two points"));
            }
            p.remove(i);
            m.set_railing_path(id, &p)
        }
        Kind::Raft | Kind::BeamSystem => crate::elem::struct_grips::remove_vertex(m, kind, id, i),
        _ => ring::remove(m, kind, id, &Role::Vertex(i)),
    }
}

/// Writes the change as one model operation (wall constraints are solved inside it).
pub fn write(m: &mut Model, c: &Change) -> Result<(), Error> {
    match c {
        Change::Wall(id, a, b) => {
            if wall_axis(m, *id)? == (*b, *a) {
                return m.flip_wall(*id);
            }
            m.set_wall_axis(*id, p2(*a), p2(*b))
        }
        Change::Slab(id, ring, holes) => {
            let o: Vec<(f64, f64)> = ring.iter().map(|&p| p2(p)).collect();
            m.set_slab_boundary(*id, &o, holes)
        }
        Change::Ceiling(id, s) => m.update_ceiling(*id, s),
        Change::Roof(id, s) => m.update_roof(*id, s),
        Change::Column(id, s) => m.update_column(*id, s),
        Change::Beam(id, s) => m.update_beam(*id, s),
        Change::Stair(id, s) => m.update_stair(*id, s),
        Change::Railing(id, p) => m.set_railing_path(*id, p),
        Change::Grid(id, s) => m.update_grid(*id, s),
        Change::RefPlane(id, a, b) => m.set_ref_plane(*id, p2(*a), p2(*b)),
        Change::Level(id, name, e) => m.update_level(*id, name, *e),
        Change::Comp(id, s) => m.mep_update_device(*id, s),
        Change::Mep(id, s) => m.mep_edit_run(*id, s),
        Change::Opening(id, s, sw, _, _) => crate::grips_opening::write(m, *id, s, *sw),
        Change::Mesh(id, d, _) => m.move_mesh(*id, (d.x, d.y, 0.0)),
        Change::Curtain(id, s) => {
            let w = m.set_curtain_wall(*id, s)?;
            WARNINGS.with(|c| c.borrow_mut().extend(w));
            Ok(())
        }
        Change::CurtainShift(id, d, _) => {
            m.move_elements(&[ddc_model::Target::Curtain(*id)], (d.x, d.y))
        }
        Change::Struct(c) => c.write(m),
        Change::Shift(k, id, d) => m.move_elements(&[shift_target(*k, *id)?], (d.x, d.y)),
        Change::WallBulge(id, _, _, bu) => m.set_wall_bulge(*id, *bu),
        Change::Datum(view, k, id, e) => m.set_datum_extent(view, *k, *id, *e),
    }
}

pub fn apply(m: &mut Model, g: &Grip, to: V2) -> Result<(), Error> {
    let c = compute(m, g, to)?;
    write(m, &c)
}

/// Live preview: polylines of the new state in plan (closed outlines repeat the first point).
pub fn outline(c: &Change) -> Vec<Vec<V2>> {
    let closed = |r: Vec<V2>| {
        let mut r = r;
        if let Some(&f) = r.first() {
            r.push(f);
        }
        r
    };
    let cross = |p: V2, rot: f64| {
        vec![
            vec![p - polar(rot, 0.25), p + polar(rot, ARM)],
            vec![
                p - polar(rot + std::f64::consts::FRAC_PI_2, 0.25),
                p + polar(rot + std::f64::consts::FRAC_PI_2, 0.25),
            ],
        ]
    };
    match c {
        Change::Wall(_, a, b) => vec![vec![*a, *b]],
        Change::Slab(_, r, _) => vec![closed(r.clone())],
        Change::Ceiling(_, s) => vec![closed(s.outer.iter().map(|&p| pv(p)).collect())],
        Change::Roof(_, s) => match &s.kind {
            RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => {
                vec![closed(ring.iter().map(|&p| pv(p)).collect())]
            }
            RoofKind::Extrusion { a, b, .. } => vec![vec![pv(*a), pv(*b)]],
        },
        Change::Column(_, s) => {
            let mut v = cross(pv(s.at), s.rotation);
            if let Some(t) = s.top_at {
                v.push(vec![pv(s.at), pv(t)]);
            }
            v
        }
        Change::Beam(_, s) => vec![vec![v2(s.a.0, s.a.1), v2(s.b.0, s.b.1)]],
        Change::Stair(_, s) => {
            let o = pv(s.origin);
            let u = polar(s.angle, 1.0);
            let n = v2(-u.y, u.x) * (s.width / 2.0);
            let run = s.tread.unwrap_or(0.28) * 10.0;
            vec![closed(vec![o - n, o + n, o + n + u * run, o - n + u * run])]
        }
        Change::Railing(_, p) => vec![p.iter().map(|q| v2(q.0, q.1)).collect()],
        Change::Grid(_, s) => vec![vec![pv(s.a), pv(s.b)]],
        Change::RefPlane(_, a, b) => vec![vec![*a, *b]],
        Change::Level(_, _, e) => vec![vec![v2(-50.0, *e), v2(50.0, *e)]],
        Change::Comp(_, s) => cross(v2(s.at.0, s.at.1), s.rotation),
        Change::Mep(_, s) => vec![s.points.iter().map(|q| v2(q.0, q.1)).collect()],
        Change::Opening(_, _, _, a, b) => vec![vec![*a, *b]],
        Change::Mesh(_, _, at) => cross(*at, 0.0),
        Change::Curtain(_, s) | Change::CurtainShift(_, _, s) => {
            curtain_line(pv(s.a), pv(s.b), s.bulge)
        }
        Change::Struct(c) => c.outline(),
        Change::WallBulge(_, a, b, bu) => match ArcAxis::new(*a, *b, *bu, Tol::default()) {
            Ok(ax) if *bu != 0.0 => {
                let n = 24;
                let l = ax.length();
                vec![(0..=n)
                    .map(|i| ax.point_at(l * i as f64 / n as f64))
                    .collect()]
            }
            _ => vec![vec![*a, *b]],
        },
        Change::Datum(_, _, _, Some(e)) => vec![vec![pv(e.a), pv(e.b)]],
        Change::Datum(_, _, _, None) => vec![],
        Change::Shift(..) => vec![],
    }
}

/// Plan polyline of a curtain wall axis: the chord, or the arc sampled.
fn curtain_line(a: V2, b: V2, bulge: f64) -> Vec<Vec<V2>> {
    match ArcAxis::new(a, b, bulge, Tol::default()) {
        Ok(ax) if bulge != 0.0 => {
            let n = 24;
            let l = ax.length();
            vec![(0..=n)
                .map(|i| ax.point_at(l * i as f64 / n as f64))
                .collect()]
        }
        _ => vec![vec![a, b]],
    }
}

/// Grip anchor point for the main temporary dimension (length, shift, parameter offset).
fn anchor(m: &Model, g: &Grip) -> Result<Option<V2>, Error> {
    Ok(match (&g.kind, &g.role) {
        (Kind::Wall, Role::End(k))
        | (Kind::Curtain, Role::End(k))
        | (Kind::RefPlane, Role::End(k))
        | (Kind::Grid, Role::End(k))
        | (Kind::Beam, Role::End(k)) => {
            let all = grips_in(m, g.kind, g.id, &g.view)?;
            all.iter()
                .find(|x| x.role == Role::End(1 - k))
                .map(|x| x.pos)
        }
        (Kind::Roof, Role::End(k)) => grips(m, g.kind, g.id)?
            .iter()
            .find(|x| x.role == Role::End(1 - k))
            .map(|x| x.pos),
        (Kind::Stair, Role::Run) => Some(pv(m.stair(g.id)?.origin)),
        (Kind::Stair, Role::Width) => {
            let s = m.stair(g.id)?;
            Some(g.pos - right(polar(s.angle, 1.0)) * s.width)
        }
        (Kind::Railing, Role::Width) => match railing_host(m, g.id)? {
            Some((st, side)) => {
                let s = m.stair(st)?;
                let (_, sign) = railing_edge(m, st, side)?;
                Some(g.pos - right(polar(s.angle, 1.0)) * (sign * s.width))
            }
            None => None,
        },
        (Kind::Stair, Role::LandingDepth) => {
            let s = m.stair(g.id)?;
            let (_, k1, t) = landing_of(m, g.id, &s)?.ok_or_else(bad)?;
            Some(pv(s.origin) + polar(s.angle, t * (k1 - 1) as f64))
        }
        (Kind::Beam, Role::Z(k)) => {
            let b = m.beam(g.id)?;
            let n = left(unit(v2(b.b.0 - b.a.0, b.b.1 - b.a.1)));
            let z = if *k == 0 { b.a.2 } else { b.b.2 };
            Some(g.pos - n * z)
        }
        (Kind::Comp, Role::Param(..)) => grips(m, g.kind, g.id)?
            .iter()
            .find(|x| x.role == Role::Move)
            .map(|x| x.pos),
        (Kind::Level, Role::Elevation) => Some(v2(g.pos.x, 0.0)),
        (_, Role::Edge(_)) | (_, Role::Move) | (_, Role::Vertex(_)) => Some(g.pos),
        _ => None,
    })
}

/// Temporary dimension references: wall axes and grid axes.
pub fn refs(m: &Model) -> Result<Vec<(V2, V2)>, Error> {
    let mut st = m.sql().prepare("SELECT ax, ay, bx, by FROM wall")?;
    let mut out: Vec<(V2, V2)> = st
        .query_map([], |r| {
            Ok((v2(r.get(0)?, r.get(1)?), v2(r.get(2)?, r.get(3)?)))
        })?
        .collect::<Result<_, _>>()?;
    for g in m.grids()? {
        out.push((pv(g.spec.a), pv(g.spec.b)));
    }
    Ok(out)
}

/// Temporary dimensions of a grip at position `to`: the first is the main one (from the anchor point),
/// the rest go to the nearest references (excluding the element itself).
pub fn dims(m: &Model, g: &Grip, to: V2) -> Result<Vec<TempDim>, Error> {
    dims_by(m, g, to, |_, own| {
        let mut r = refs(m)?;
        r.retain(|x| Some(*x) != own);
        Ok(r)
    })
}

/// Same as `dims`, references from the editor's cached snap index (no wall scan).
pub fn dims_ed(ed: &crate::editor::Editor, g: &Grip, to: V2) -> Result<Vec<TempDim>, Error> {
    dims_by(&ed.model, g, to, |at, own| {
        let skip = (g.kind == Kind::Wall).then_some(g.id);
        let mut r = ed.ref_axes_near(at, temp_dims::REACH, skip);
        r.retain(|x| Some(*x) != own);
        Ok(r)
    })
}

fn dims_by(
    m: &Model,
    g: &Grip,
    to: V2,
    refs_for: impl FnOnce(V2, Option<(V2, V2)>) -> Result<Vec<(V2, V2)>, Error>,
) -> Result<Vec<TempDim>, Error> {
    let mut out = vec![];
    if let Some(a) = anchor(m, g)? {
        if (to - a).len() > 1e-9 {
            out.push(TempDim {
                a,
                b: to,
                value: (to - a).len(),
            });
        }
    }
    if g.kind != Kind::Level {
        let own = if g.kind == Kind::Wall {
            Some(wall_axis(m, g.id)?)
        } else {
            None
        };
        let r = refs_for(to, own)?;
        out.extend(temp_dims::for_point(to, &r));
    }
    Ok(out)
}

/// Exact value on a temporary dimension: the grip is placed so the dimension becomes `value`.
pub fn apply_value(m: &mut Model, g: &Grip, d: &TempDim, value: f64) -> Result<(), Error> {
    if value <= 0.0 && g.kind != Kind::Level {
        return Err(Error::Invalid("length must be positive".into()));
    }
    // the dimension runs from reference a to grip b: place b so the length becomes value
    apply(m, g, d.retarget(value))
}

// ---- plan interaction ----

#[derive(Clone, Debug)]
pub struct Active {
    pub grip: Grip,
    pub now: V2,
    pub preview: Vec<Vec<V2>>,
    pub dims: Vec<TempDim>,
    pub error: Option<String>,
}

/// Editor grip state.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// Elements with grips shown besides the selection (the last touched).
    pub focus: Vec<(Kind, i64)>,
    pub active: Option<Active>,
    /// The last released grip: its temporary dimensions are clickable.
    pub last: Option<(Kind, i64, Role)>,
    /// Selection at the moment `last` was picked: with a different selection Del does not touch the vertex.
    pub last_sel: Vec<crate::editor::Sel>,
    /// Value entry: (dimension index, text).
    pub typing: Option<(usize, String)>,
    /// Grips of the selection, rebuilt only when selection, level or model changes.
    pub cache: Cache,
    /// Snap candidates for the dragged grip (shared snap service) and the Tab choice.
    pub snaps: Vec<crate::snap::Snap>,
    pub snap_idx: usize,
    /// Parts of the dragged item that move with the grip: never snap targets.
    pub moving: crate::snap_refine::Moving,
    /// Grip highlighted by Tab from the keyboard; Enter or Space starts dragging it.
    pub hl: Option<(Kind, i64, Role)>,
    /// Selection and level `hl` was set under; any change drops the highlight.
    pub hl_ctx: Option<(Vec<crate::editor::Sel>, i64)>,
    /// Arrow nudges since the last other edit: (selection, total shift, undo pushes after the last one).
    pub burst: Option<(Vec<crate::editor::Sel>, (f64, f64), u64)>,
}

/// Model and view state every grip cache keys on: (view, level, editor revision, journal head).
/// The revision is the same source the snap and pick caches use: every edit, undo, redo, full or
/// scoped reload and model replace bumps it; the journal head catches writes not yet reloaded.
type Stamp = (String, i64, u64, u64);

fn stamp(ed: &crate::editor::Editor) -> Stamp {
    (view_key(ed), ed.level, ed.revision, ed.model.change_head())
}

type CacheKey = (Vec<crate::editor::Sel>, Vec<(Kind, i64)>, Stamp);

/// Visible grips keyed by (selection, focus, [`Stamp`]). Clones empty.
#[derive(Default)]
pub struct Cache {
    slot: std::sync::Mutex<Option<(CacheKey, Vec<Grip>)>>,
    /// How many times the list was rebuilt (each rebuild reads the model).
    pub builds: std::sync::atomic::AtomicUsize,
    /// Temporary dimensions of the last grip, keyed by (grip, [`Stamp`]).
    dims: std::sync::Mutex<Option<(DimKey, Vec<TempDim>)>>,
    /// How many times those dimensions were recomputed.
    pub dim_builds: std::sync::atomic::AtomicUsize,
}

type DimKey = ((Kind, i64, Role), Stamp);

impl Clone for Cache {
    fn clone(&self) -> Self {
        Cache::default()
    }
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cache")
    }
}

fn targets(ed: &crate::editor::Editor) -> Vec<(Kind, i64)> {
    use crate::editor::{bodies::BodyKind, Sel};
    let mut t: Vec<(Kind, i64)> = ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(id) => Some((Kind::Wall, *id)),
            Sel::Room(id) => Some((Kind::Room, *id)),
            Sel::Curtain(id) => Some((Kind::Curtain, *id)),
            Sel::Comp(id) => Some((Kind::Comp, *id)),
            Sel::Body(BodyKind::Column, id) => Some((Kind::Column, *id)),
            Sel::Body(BodyKind::Beam, id) => Some((Kind::Beam, *id)),
            Sel::Body(BodyKind::Ceiling, id) => Some((Kind::Ceiling, *id)),
            Sel::Slab(id) => Some((Kind::Slab, *id)),
            Sel::Mep(id) => Some((Kind::Mep, *id)),
            Sel::Opening(id) => Some((Kind::Opening, *id)),
            Sel::Body(BodyKind::Mesh, id) => Some((Kind::Mesh, *id)),
            Sel::Mass(id) => Some((Kind::Mass, *id)),
            Sel::Ext(c, id) if *c == crate::scene_ids::EXT_SOLID => Some((Kind::Solid, *id)),
            Sel::Ext(c, id) => crate::elem::grip_target(*s).or_else(|| {
                match crate::editor::structx::kind_of(*c) {
                    Some("stair") => Some((Kind::Stair, *id)),
                    Some("railing") => Some((Kind::Railing, *id)),
                    Some("shaft") => Some((Kind::Shaft, *id)),
                    _ => None,
                }
            }),
            _ => None,
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    t.retain(|x| seen.insert(*x));
    for f in &ed.grip.focus {
        if !t.contains(f) {
            t.push(*f);
        }
    }
    t
}

/// Key of the current plan view for axis 2D bounds.
pub fn view_key(ed: &crate::editor::Editor) -> String {
    format!("plan:{}", ed.level)
}

/// Level the element lives on; None: no level filter for this kind.
fn level_of(m: &Model, kind: Kind, id: i64) -> Result<Option<i64>, Error> {
    Ok(match kind {
        Kind::Column => Some(m.column(id)?.level),
        Kind::Beam => Some(m.beam(id)?.level),
        Kind::Comp => Some(m.component(id)?.level),
        Kind::Ceiling => Some(ceiling_spec(m, id)?.level),
        Kind::Curtain => Some(m.curtain_wall(id)?.level),
        _ => row_level(m, kind, id),
    })
}

/// Level column of the table a level-bound kind lives in (tables per `schema.rs`).
fn level_query(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Wall => "SELECT level FROM wall WHERE id=?1",
        Kind::Slab => "SELECT level FROM slab WHERE id=?1",
        Kind::Stair => "SELECT level FROM stair WHERE id=?1",
        Kind::Railing => "SELECT level FROM railing WHERE id=?1",
        Kind::Mep => "SELECT level FROM mep_run WHERE id=?1",
        Kind::Opening => "SELECT w.level FROM opening o JOIN wall w ON w.id = o.wall WHERE o.id=?1",
        _ => return None,
    })
}

/// Level from the kind's own row; a row that is not there (another family table) keeps the
/// grips shown and says so once per kind instead of failing silently.
fn row_level(m: &Model, kind: Kind, id: i64) -> Option<i64> {
    let q = level_query(kind)?;
    match m.sql().query_row(q, [id], |r| r.get(0)) {
        Ok(l) => Some(l),
        Err(e) => {
            warn_level_once(kind, id, &e);
            None
        }
    }
}

fn warn_level_once(kind: Kind, id: i64, e: &rusqlite::Error) {
    static SEEN: std::sync::Mutex<Vec<Kind>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    if !seen.contains(&kind) {
        seen.push(kind);
        eprintln!("grips: no level for {kind:?} {id} ({e}); no level filter");
    }
}

/// Grips of the selection (and the last touched element) on the view's level, uncached.
fn build_visible(ed: &crate::editor::Editor) -> Vec<Grip> {
    let v = view_key(ed);
    targets(ed)
        .into_iter()
        .filter(|&(k, id)| {
            level_of(&ed.model, k, id).is_ok_and(|l| l.map_or(true, |l| l == ed.level))
        })
        .flat_map(|(k, id)| grips_in(&ed.model, k, id, &v).unwrap_or_default())
        .collect()
}

/// [`build_visible`] cached per (selection, focus, [`Stamp`]): hover, press and drag frames
/// read no model rows once the list is built.
pub fn visible(ed: &crate::editor::Editor) -> Vec<Grip> {
    let key: CacheKey = (ed.selection.clone(), ed.grip.focus.clone(), stamp(ed));
    let mut slot = ed.grip.cache.slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, gs)) = slot.as_ref() {
        if *k == key {
            return gs.clone();
        }
    }
    let gs = build_visible(ed);
    ed.grip.cache.builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *slot = Some((key, gs.clone()));
    gs
}

/// Nearest visible grip within the pick radius; unselected elements have none.
fn hit(ed: &crate::editor::Editor, p: V2) -> Option<Grip> {
    let r = ed.tool.radius;
    visible(ed)
        .into_iter()
        .map(|g| ((g.pos - p).len(), g))
        .filter(|x| x.0 <= r)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
}

/// Current temporary dimensions of the last grip (for click and entry).
pub fn last_dims(ed: &crate::editor::Editor) -> Vec<TempDim> {
    let Some((k, id, role)) = &ed.grip.last else {
        return vec![];
    };
    let key: DimKey = ((*k, *id, role.clone()), stamp(ed));
    let mut slot = ed.grip.cache.dims.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((c, d)) = slot.as_ref() {
        if *c == key {
            return d.clone();
        }
    }
    ed.grip.cache.dim_builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = grips_in(&ed.model, *k, *id, &view_key(ed))
        .ok()
        .and_then(|gs| gs.into_iter().find(|g| &g.role == role))
        .and_then(|g| dims_ed(ed, &g, g.pos).ok())
        .unwrap_or_default();
    *slot = Some((key, d.clone()));
    d
}

thread_local! {
    /// Model warnings of the grip edit being committed (shown in the status line).
    static WARNINGS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn commit(ed: &mut crate::editor::Editor, g: Grip, f: crate::editor::OpFn) {
    WARNINGS.with(|c| c.borrow_mut().clear());
    match ed.run_op(f) {
        Ok(()) => {
            let w = WARNINGS.with(|c| std::mem::take(&mut *c.borrow_mut()));
            if !w.is_empty() {
                ed.status = Some(w.join("; "));
            }
            set_last(ed, &g);
        }
        Err(e) => crate::constraints_ui::show(ed, &e),
    }
}

fn set_last(ed: &mut crate::editor::Editor, g: &Grip) {
    ed.grip.last = Some((g.kind, g.id, g.role.clone()));
    ed.grip.last_sel = ed.selection.clone();
    if !matches!(g.kind, Kind::Wall | Kind::Slab | Kind::Opening | Kind::Curtain) {
        ed.grip.focus = vec![(g.kind, g.id)];
    }
}

/// Clears the Tab highlight once the selection or the level is not the one it was set under.
fn drop_stale_hl(ed: &mut crate::editor::Editor) {
    if ed.grip.hl.is_some()
        && ed.grip.hl_ctx.as_ref().is_none_or(|(s, l)| *s != ed.selection || *l != ed.level)
    {
        ed.grip.hl = None;
        ed.grip.hl_ctx = None;
    }
}

/// Select-mode event; true means consumed by grips.
pub fn handle(ed: &mut crate::editor::Editor, e: crate::wall_tool::Event) -> bool {
    use crate::editor::OpFn;
    use crate::wall_tool::{Event, Key};
    use std::sync::Arc;
    drop_stale_hl(ed);
    if ed.grip.active.is_none() && crate::sel_dims::handle(ed, &e) {
        return true;
    }
    match e {
        Event::Press(p) => {
            let Some(g) = hit(ed, p)
                // an opening's slide handle sits on its body, whose own drag keeps the grab offset
                .filter(|g| !is_click(&g.role) && !(g.kind == Kind::Opening && g.role == Role::Move))
            else {
                return false;
            };
            ed.grip.typing = None;
            ed.grip.moving = crate::snap_refine::moving_parts(&ed.model, &g);
            ed.grip.active = Some(Active {
                now: g.pos,
                grip: g,
                preview: vec![],
                dims: vec![],
                error: None,
            });
            true
        }
        Event::Move(p) => {
            let Some(a) = &ed.grip.active else {
                return false;
            };
            let skip = (a.grip.kind == Kind::Wall).then_some(a.grip.id);
            let list = ed.snap_list(p, None, ed.tool.radius * 0.5, skip);
            let list = crate::snap_refine::drop_moving(list, &ed.grip.moving);
            ed.grip.snaps = crate::snap_refine::gate(list, p, ed.tool.no_snap);
            ed.grip.snap_idx = 0;
            drag_to(ed)
        }
        Event::Key(Key::Tab) if ed.grip.active.is_some() => {
            let n = ed.grip.snaps.len();
            if n > 1 {
                ed.grip.snap_idx = (ed.grip.snap_idx + 1) % n;
            }
            drag_to(ed)
        }
        Event::Key(Key::Tab) if ed.grip.typing.is_none() && !ed.drawing() => {
            let vis: Vec<Grip> = visible(ed).into_iter().filter(|g| !is_click(&g.role)).collect();
            if vis.is_empty() {
                return false;
            }
            let cur = ed.grip.hl.as_ref().and_then(|h| {
                vis.iter().position(|g| (g.kind, g.id, &g.role) == (h.0, h.1, &h.2))
            });
            let g = &vis[cur.map_or(0, |i| (i + 1) % vis.len())];
            ed.grip.hl = Some((g.kind, g.id, g.role.clone()));
            ed.grip.hl_ctx = Some((ed.selection.clone(), ed.level));
            true
        }
        Event::Key(Key::Escape)
            if ed.grip.hl.is_some() && ed.grip.active.is_none() && ed.grip.typing.is_none() =>
        {
            ed.grip.hl = None;
            ed.grip.hl_ctx = None;
            true
        }
        Event::Key(Key::Enter) | Event::Char(' ')
            if ed.grip.active.is_none() && ed.grip.typing.is_none() && ed.grip.hl.is_some() =>
        {
            let Some(h) = ed.grip.hl.clone() else {
                return false;
            };
            let Some(g) = visible(ed)
                .into_iter()
                .find(|g| (g.kind, g.id, &g.role) == (h.0, h.1, &h.2))
            else {
                ed.grip.hl = None;
                ed.grip.hl_ctx = None;
                return false;
            };
            ed.grip.snaps.clear();
            ed.grip.active = Some(Active {
                now: g.pos,
                grip: g,
                preview: vec![],
                dims: vec![],
                error: None,
            });
            drag_to(ed)
        }
        // digits during a drag go to the main temporary dimension
        Event::Char(c)
            if ed.grip.active.is_some()
                && (ed.grip.typing.is_some()
                    || c.is_ascii_digit()
                    || c == '.'
                    || c == ','
                    || c == '-') =>
        {
            let s = ed.grip.typing.get_or_insert((0, String::new()));
            if s.1.len() < 32 && (c.is_ascii_alphanumeric() || ".,-+'\"/ ".contains(c)) {
                s.1.push(c);
            }
            true
        }
        Event::Key(Key::Enter) if ed.grip.active.is_some() => {
            let Some(a) = ed.grip.active.clone() else {
                return false;
            };
            let Some((_, txt)) = ed.grip.typing.take() else {
                // no typed value: Enter drops the grip where it is
                return handle(ed, Event::Release(a.now));
            };
            let Ok(v) = crate::units::parse_signed_len(&txt, ed.tool.units) else {
                ed.status = Some(crate::tr("sch.f.not_number", &[("s", txt.as_str())]));
                ed.grip.typing = Some((0, txt));
                return true;
            };
            let dims = if a.dims.is_empty() {
                dims_ed(ed, &a.grip, a.now).unwrap_or_default()
            } else {
                a.dims.clone()
            };
            let Some(d) = dims.first().copied() else {
                ed.status = Some(crate::tr("level_ask.grip.no_dim", &[("s", txt.as_str())]));
                return true;
            };
            ed.grip.active = None;
            ed.grip.snaps.clear();
            let g2 = a.grip.clone();
            let f = OpFn(Arc::new(move |m: &mut Model| apply_value(m, &g2, &d, v)));
            commit(ed, a.grip, f);
            true
        }
        Event::Release(_) => {
            let Some(a) = ed.grip.active.take() else {
                return false;
            };
            if (a.now - a.grip.pos).len() > 1e-9 {
                let (g, to) = (a.grip.clone(), a.now);
                let f = OpFn(Arc::new(move |m: &mut Model| apply(m, &g, to)));
                commit(ed, a.grip, f);
            } else {
                // a press without movement selects the grip: Del deletes the vertex, dimensions are clickable
                set_last(ed, &a.grip);
            }
            ed.grip.snaps.clear();
            true
        }
        Event::Click(p) => {
            if crate::constraints_ui::click(ed, p) {
                return true;
            }
            if let Some(g) = hit(ed, p).filter(|g| is_click(&g.role)) {
                let g2 = g.clone();
                let f = OpFn(Arc::new(move |m: &mut Model| apply(m, &g2, g2.pos)));
                commit(ed, g, f);
                return true;
            }
            // a click on a temporary dimension label starts exact value entry
            let r = ed.tool.radius;
            if let Some(k) = last_dims(ed)
                .iter()
                .position(|d| (mid(d.a, d.b) - p).len() <= r)
            {
                ed.grip.typing = Some((k, String::new()));
                return true;
            }
            ed.grip.typing = None;
            false
        }
        Event::Char(c) if ed.grip.typing.is_some() => {
            if let Some((_, s)) = &mut ed.grip.typing {
                if s.len() < 32 && (c.is_ascii_alphanumeric() || ".,-+'\"/ ".contains(c)) {
                    s.push(c);
                }
            }
            true
        }
        Event::Key(Key::Backspace) if ed.grip.typing.is_some() => {
            if let Some((_, s)) = &mut ed.grip.typing {
                s.pop();
            }
            true
        }
        Event::Key(Key::Escape) if ed.grip.typing.is_some() || ed.grip.active.is_some() => {
            ed.grip.hl = None;
            ed.grip.hl_ctx = None;
            ed.grip.typing = None;
            ed.grip.active = None;
            ed.grip.snaps.clear();
            true
        }
        Event::Key(Key::Enter) if ed.grip.typing.is_some() => {
            let (k, s) = ed.grip.typing.take().expect("typing");
            let Ok(v) = crate::units::parse_signed_len(&s, ed.tool.units) else {
                ed.status = Some(crate::tr("modify_ops.grip.not_number", &[("text", &s.to_string())]));
                return true;
            };
            let Some((kind, id, role)) = ed.grip.last.clone() else {
                return true;
            };
            let Some(g) = grips_in(&ed.model, kind, id, &view_key(ed))
                .ok()
                .and_then(|gs| gs.into_iter().find(|g| g.role == role))
            else {
                return true;
            };
            let Some(d) = dims_ed(ed, &g, g.pos)
                .ok()
                .and_then(|d| d.get(k).copied())
            else {
                return true;
            };
            let g2 = g.clone();
            let f = OpFn(Arc::new(move |m: &mut Model| apply_value(m, &g2, &d, v)));
            commit(ed, g, f);
            true
        }
        _ => false,
    }
}

/// Arrow nudge of the selection by one grid step (`shift`: x10). A burst of nudges with nothing
/// else in between is one undo step: the previous nudge is undone and the total is applied.
pub fn nudge(ed: &mut crate::editor::Editor, dx: i32, dy: i32, shift: bool) -> bool {
    use crate::editor::OpFn;
    use std::sync::Arc;
    if ed.grip.active.is_some()
        || ed.grip.typing.is_some()
        || ed.drag.is_some()
        || !ed.wall_draft().input.is_empty()
        || ed.tool_armed()
        || ed.drawing()
    {
        return false;
    }
    let ts: Vec<ddc_model::Target> = ed
        .selection
        .iter()
        .filter_map(|s| crate::modify_tools::target(*s))
        .collect();
    if ts.is_empty() {
        return false;
    }
    let step = ed.tool.grid.max(1e-6) * if shift { 10.0 } else { 1.0 };
    nudge_by(ed, ts, (f64::from(dx) * step, f64::from(dy) * step), &|ts, t| {
        let ts = ts.to_vec();
        OpFn(Arc::new(move |m: &mut Model| m.move_elements(&ts, t)))
    })
}

/// Applies one nudge `d` on top of the live burst; a refused sum puts the earlier nudges back.
fn nudge_by(
    ed: &mut crate::editor::Editor,
    ts: Vec<ddc_model::Target>,
    d: (f64, f64),
    mk: &dyn Fn(&[ddc_model::Target], (f64, f64)) -> crate::editor::OpFn,
) -> bool {
    let prev = match ed.grip.burst.take() {
        Some((sel, t, n)) if sel == ed.selection && n == ed.pushes => Some(t),
        _ => None,
    };
    let total = prev.map_or(d, |t| (t.0 + d.0, t.1 + d.1));
    if prev.is_some() {
        ed.undo();
        // a failed undo leaves the previous nudge in place: applying the total would double it
        if !ed.can_redo() {
            return true;
        }
    }
    match ed.run_op(mk(&ts, total)) {
        Ok(()) => ed.grip.burst = Some((ed.selection.clone(), total, ed.pushes)),
        Err(e) => {
            let msg = crate::constraints_ui::report(&e);
            if let Some(t) = prev {
                if ed.run_op(crate::editor::OpFn(std::sync::Arc::new({
                    let ts = ts.clone();
                    move |m: &mut Model| m.move_elements(&ts, t)
                })))
                .is_ok()
                {
                    ed.grip.burst = Some((ed.selection.clone(), t, ed.pushes));
                }
            }
            ed.status = Some(msg);
        }
    }
    true
}

/// Moves the active grip to the chosen snap candidate and refreshes preview and dimensions.
fn drag_to(ed: &mut crate::editor::Editor) -> bool {
    let Some(a) = &ed.grip.active else {
        return false;
    };
    let to = ed.grip.snaps.get(ed.grip.snap_idx).map_or(a.now, |s| s.p);
    let g = a.grip.clone();
    let (preview, error) = match compute(&ed.model, &g, to) {
        Ok(c) => (outline(&c), None),
        Err(e) => (vec![], Some(e.to_string())),
    };
    let d = dims_ed(ed, &g, to).unwrap_or_default();
    if let Some(a) = &mut ed.grip.active {
        a.now = to;
        a.preview = preview;
        a.dims = d;
        a.error = error;
    }
    true
}

/// Del on a selected network or railing vertex grip: deletes the vertex (one operation).
/// false means no grip is selected and Del goes to deleting the selection.
pub fn delete_key(ed: &mut crate::editor::Editor) -> bool {
    use std::sync::Arc;
    let Some((kind, id, role)) = ed.grip.last.clone() else {
        return false;
    };
    let (Role::Vertex(_) | Role::HoleVertex(..)) = role else {
        return false;
    };
    // the grip was picked under a different selection or its element is no longer shown: Del deletes the selection
    if !matches!(
        kind,
        Kind::Mep
            | Kind::Railing
            | Kind::Slab
            | Kind::Ceiling
            | Kind::Roof
            | Kind::Shaft
            | Kind::Raft
            | Kind::BeamSystem
    )
        || ed.grip.last_sel != ed.selection
        || !targets(ed).contains(&(kind, id))
    {
        ed.grip.last = None;
        return false;
    }
    let f = crate::editor::OpFn(Arc::new(move |m: &mut Model| match &role {
        Role::Vertex(i) => delete_vertex(m, kind, id, *i),
        r => ring::remove(m, kind, id, r),
    }));
    match ed.run_op(f) {
        Ok(()) => {
            ed.grip.last = None;
            ed.status = Some(crate::tr("modify_ops.grip.vertex_removed", &[]));
        }
        Err(e) => crate::constraints_ui::show(ed, &e),
    }
    true
}

/// Draws grips, the live preview and temporary dimensions over the plan.
pub fn paint(
    painter: &egui::Painter,
    ed: &crate::editor::Editor,
    to_s: &dyn Fn(V2) -> egui::Pos2,
    t: &crate::ui_kit::Tokens,
) {
    use crate::overlay::{self, Grip, Plate};
    use egui::{self, Align2, FontId, Rect, Stroke, Vec2};
    let ov = t.ov();
    let st = Stroke::new(overlay::GRIP_STROKE, ov.selection);
    let cursor = painter.ctx().input(|i| i.pointer.hover_pos());
    if ed.grip.active.is_some() {
        let sn = ed.grip.snaps.get(ed.grip.snap_idx);
        if let Some(sn) = sn.filter(|s| s.kind != crate::snap::SnapKind::Free) {
            let (all, i) = (&ed.grip.snaps, ed.grip.snap_idx);
            crate::plan_view::snap_marker(painter, to_s, *sn, all, i, None, ov);
        }
    }
    for g in visible(ed) {
        let c = to_s(g.pos);
        let glyph = |txt: &str| {
            painter.text(
                c,
                Align2::CENTER_CENTER,
                txt,
                FontId::proportional(15.0), // design-lint: data
                ov.selection,
            );
        };
        let shape = match g.role {
            Role::Edge(_)
            | Role::Run
            | Role::Param(..)
            | Role::HoleEdge(..)
            | Role::Bulge
            | Role::Width
            | Role::Landing
            | Role::LandingDepth => Grip::Diamond,
            Role::Rotate | Role::Bubble(_) => Grip::Ring,
            Role::Insert(_) => Grip::Plus,
            Role::Z(_) => Grip::Triangle,
            Role::Move => Grip::Circle,
            Role::Mode(_) => {
                let own = ed
                    .model
                    .datum_extent(&g.view, datum_kind(g.kind), g.id)
                    .ok()
                    .flatten()
                    .is_some();
                let txt = if own { "2D" } else { "3D" };
                let gal =
                    painter.layout_no_wrap(txt.into(), FontId::proportional(10.0), ov.selection); // design-lint: data
                let r = Rect::from_center_size(c, gal.size() + Vec2::new(6.0, 2.0));
                painter.rect_filled(r, ddc_ui::theme::radius::SM, ov.plate);
                painter.rect_stroke(r, ddc_ui::theme::radius::SM, st, egui::StrokeKind::Inside);
                painter.galley(r.min + Vec2::new(3.0, 1.0), gal, ov.selection);
                continue;
            }
            Role::FlipHand => {
                glyph("⇆");
                continue;
            }
            Role::FlipFacing | Role::Flip => {
                glyph("⇅");
                continue;
            }
            _ => Grip::Square,
        };
        let dragged = ed
            .grip
            .active
            .as_ref()
            .is_some_and(|a| (a.grip.kind, a.grip.id, &a.grip.role) == (g.kind, g.id, &g.role));
        let hl_ok = ed.grip.hl_ctx.as_ref().is_some_and(|(s, l)| *s == ed.selection && *l == ed.level);
        let hl = hl_ok
            && ed.grip.hl.as_ref().is_some_and(|h| (h.0, h.1, &h.2) == (g.kind, g.id, &g.role));
        let hot = dragged || hl || cursor.is_some_and(|h| h.distance(c) < overlay::GRIP_HIT_PX);
        overlay::grip(painter, c, shape, hot, ov);
    }
    let draw_dims = |ds: &[TempDim], typing: Option<&(usize, String)>| {
        for (k, d) in ds.iter().enumerate() {
            let (a, b) = (to_s(d.a), to_s(d.b));
            painter.extend(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(overlay::THIN_PX, t.text_dim),
                4.0,
                3.0,
            ));
            let m = a + (b - a) * 0.5;
            let (txt, kind) = match typing.filter(|x| x.0 == k) {
                Some((_, s)) => (format!("{s}▏"), Plate::Editing),
                None => (
                    crate::units::format_length(d.value, ed.tool.units),
                    Plate::Temp,
                ),
            };
            overlay::dim_label(painter, m, txt, kind, ov);
        }
    };
    if let Some(a) = &ed.grip.active {
        for pl in &a.preview {
            let pts: Vec<_> = pl.iter().map(|&p| to_s(p)).collect();
            painter.extend(egui::Shape::dashed_line(
                &pts,
                ov.ghost(),
                overlay::GHOST_DASH.0,
                overlay::GHOST_DASH.1,
            ));
        }
        draw_dims(&a.dims, ed.grip.typing.as_ref());
        if let Some(e) = &a.error {
            let at = to_s(a.now) + Vec2::new(10.0, 10.0);
            overlay::label_at(painter, at, e.clone(), Plate::Error, ov);
        }
    } else {
        draw_dims(&last_dims(ed), ed.grip.typing.as_ref());
        crate::sel_dims::paint(painter, ed, to_s, t);
    }
    crate::constraints_ui::paint(painter, ed, to_s, t);
    crate::constraints_ui::paint_pin(painter, ed, to_s, t);
}

#[path = "grips_ring.rs"]
mod ring;
pub use ring::is_ring_refusal;

#[cfg(test)]
#[path = "grips_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "grips_shots.rs"]
mod shots;

#[cfg(test)]
#[path = "grips_select_tests.rs"]
mod select_tests;

#[cfg(test)]
#[path = "grips_cache_tests.rs"]
mod cache_tests;
