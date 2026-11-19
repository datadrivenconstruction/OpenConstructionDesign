// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Grips in 3D. A [`Provider`] gives an element [`Grip3`] grips (point, drag constraint,
//! temporary-dimension origin) and writes the result as one model operation.
//! The built-in provider: plan grips (`crate::grips`) lifted to the element's elevation and
//! written by the same `compute/apply`; vertical grips are wall height/bottom, slab/roof/ceiling
//! offset and thickness, column top/bottom, beam ends in 3D and section rotation, opening position
//! along the wall, sill, top and width, and component elevation.
//! Other modules register a provider via [`register`], which is queried first; network grips
//! (axis vertices, move) are the `mep_grips` provider. The result is written in f64 as the previous
//! model value plus the grip shift: scene coordinates (f32) do not round projects far from the origin.

use std::sync::{Arc, RwLock};

use ddc_geom::{v2, V2};
use ddc_model::{CeilingSpec, Error, Model, OpeningSpec, RoofKind, WallSpec};
use ddc_render::glam::Vec3;

use super::math::{angle_about, f3, rotate3, v3};
use super::pick::{Body, Cat, Target};
use crate::editor::Editor;

/// Grip drag constraint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis3 {
    /// In the plane through the grip with a normal; Ctrl moves along the normal (if the role allows).
    Plane(Vec3),
    /// Along a line through the grip (unit vector).
    Line(Vec3),
    /// Rotation about an axis through the center; `zero` is the zero-angle direction.
    Ring {
        center: Vec3,
        axis: Vec3,
        zero: Vec3,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Role3 {
    /// A plan grip lifted to the element's elevation.
    Plan(crate::grips::Grip),
    /// Top (height) of a wall or column.
    Top,
    /// Bottom of a wall or column (the top stays).
    Base,
    /// Top of a slab/roof (offset from the level), bottom of a ceiling (height above the level).
    Offset,
    /// Thickness: bottom of a slab/roof, top of a ceiling (the type is split if shared).
    Thickness,
    /// Beam end in 3D: in plan, or vertical with Ctrl.
    End3(u8),
    /// Rotation of the beam section about its axis.
    Roll,
    /// Opening: along the wall, sill, top, width.
    Along,
    Sill,
    Head,
    Width,
    /// Component elevation above the level.
    Elevation,
    /// Lean of a wall with a flat top: the top of its axis surface moved across the wall.
    Lean,
    /// A provider grip (the number is the provider's own).
    Ext(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grip3 {
    pub target: Target,
    pub role: Role3,
    /// Position in the scene (m, z measured from the active level's elevation).
    pub pos: Vec3,
    pub axis: Axis3,
    /// Temporary-dimension origin: the value is the distance from it along the axis (or an angle).
    pub anchor: Vec3,
}

impl Grip3 {
    /// A grip with vertical travel on Ctrl.
    pub fn lifts(&self) -> bool {
        matches!(self.role, Role3::End3(_))
            || (self.target.cat == Cat::Mep && matches!(self.role, Role3::Ext(_)))
            // smooth sweep path points move in 3D
            || (self.target.cat == Cat::Ext(crate::solid_edit3d::CAT)
                && matches!(self.axis, Axis3::Plane(_)))
            || self.target.cat == Cat::Ext(crate::scene_ids::EXT_FREEFORM)
    }

    /// Grip shift to `to` in f64 (added to the model values).
    pub fn delta(&self, to: Vec3) -> (f64, f64, f64) {
        let d = to - self.pos;
        (d.x as f64, d.y as f64, d.z as f64)
    }

    /// Temporary-dimension value at position `to`: m or degrees.
    pub fn value(&self, to: Vec3) -> f32 {
        match self.axis {
            Axis3::Line(d) => (to - self.anchor).dot(d),
            Axis3::Plane(_) => (to - self.anchor).length(),
            Axis3::Ring { center, axis, zero } => angle_about(axis, zero, to - center).to_degrees(),
        }
    }

    /// Grip position from a typed value; `cur` is the current one (sets the in-plane direction).
    pub fn typed(&self, v: f32, cur: Vec3) -> Vec3 {
        match self.axis {
            Axis3::Line(d) => self.anchor + d * v,
            Axis3::Plane(n) => {
                let mut d = (cur - self.anchor).normalize_or_zero();
                if d == Vec3::ZERO {
                    d = n.any_orthonormal_vector();
                }
                self.anchor + d * v
            }
            Axis3::Ring { center, axis, zero } => {
                let r = (self.pos - center).length().max(0.1);
                let z = zero.normalize_or(Vec3::X) * r;
                center + v3(rotate3(f3(z), [0.0; 3], f3(axis), (v as f64).to_radians()))
            }
        }
    }
}

/// Provider of grips and pick bodies for categories.
pub trait Provider: Send + Sync {
    /// Provider key: registering again with the same key replaces the previous provider.
    fn key(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
    fn owns(&self, t: Target) -> bool;
    /// Element grips; `z0` is the active level elevation (scene z = absolute − z0).
    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error>;
    /// One model operation: grip `g` moved to `to` (scene).
    fn apply(&self, m: &mut Model, z0: f64, g: &Grip3, to: Vec3) -> Result<(), Error>;
    /// Live preview without writing: polylines in the scene; an error means the state is invalid.
    fn preview(&self, _m: &Model, _z0: f64, g: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
        Ok(vec![vec![g.pos, to]])
    }
    /// Pick bodies for its categories (if they are not in the scene).
    fn bodies(&self, _ed: &Editor) -> Vec<Body> {
        vec![]
    }
    /// Another drag of the same grip chosen by Alt / Shift at the press (None: the grip as is).
    fn variant(&self, _m: &Model, _z0: f64, _g: &Grip3, _alt: bool, _shift: bool) -> Option<Grip3> {
        None
    }
}

/// The grip to drag for the modifiers held at the press.
pub fn variant(m: &Model, z0: f64, g: &Grip3, alt: bool, shift: bool) -> Grip3 {
    if !(alt || shift) {
        return g.clone();
    }
    provider_for(g.target)
        .and_then(|p| p.variant(m, z0, g, alt, shift))
        .unwrap_or_else(|| g.clone())
}

static EXTRA: RwLock<Vec<Arc<dyn Provider>>> = RwLock::new(Vec::new());

/// Register a provider (queried before the built-in ones); calling again with the same key
/// replaces the previous one, so registering on every project open does not pile up duplicates.
pub fn register(p: Arc<dyn Provider>) {
    if let Ok(mut v) = EXTRA.write() {
        v.retain(|q| q.key() != p.key());
        v.push(p);
    }
}

/// Registered providers (for checks).
pub fn registered() -> Vec<&'static str> {
    EXTRA
        .read()
        .map(|v| v.iter().map(|p| p.key()).collect())
        .unwrap_or_default()
}

pub fn providers() -> Vec<Arc<dyn Provider>> {
    let mut v: Vec<Arc<dyn Provider>> = EXTRA.read().map(|v| v.clone()).unwrap_or_default();
    v.push(Arc::new(super::mep_grips::MepGrips));
    v.push(Arc::new(crate::elem::curtain::CurtainGrips));
    v.push(Arc::new(Builtin));
    v
}

pub fn provider_for(t: Target) -> Option<Arc<dyn Provider>> {
    providers().into_iter().find(|p| p.owns(t))
}

/// All grips of an element from its provider.
pub fn grips_of(m: &Model, z0: f64, t: Target) -> Vec<Grip3> {
    provider_for(t)
        .and_then(|p| p.grips(m, z0, t).ok())
        .unwrap_or_default()
}

pub fn apply(m: &mut Model, z0: f64, g: &Grip3, to: Vec3) -> Result<(), Error> {
    match provider_for(g.target) {
        Some(p) => p.apply(m, z0, g, to),
        None => Err(Error::Invalid(format!("no grips for {:?}", g.target.cat))),
    }
}

pub fn preview(m: &Model, z0: f64, g: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
    match provider_for(g.target) {
        Some(p) => p.preview(m, z0, g, to),
        None => Ok(vec![]),
    }
}

// ---- built-in provider ----

pub struct Builtin;

/// Grip error: a label key `e3d.err.*` (the status line translates it, see `err_text`).
fn bad(key: &str) -> Error {
    Error::Invalid(key.into())
}

/// A point inside an outline (for offset and thickness grips): the vertex centroid if it is
/// inside, otherwise the midpoint of the widest horizontal chord through it.
pub fn inside_point(r: &[(f64, f64)]) -> (f64, f64) {
    let c = centroid(r);
    if r.len() < 3
        || ddc_geom::predicates::point_in_ring(
            v2(c.0, c.1),
            &r.iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>(),
        ) > 0
    {
        return c;
    }
    let mut xs = vec![];
    for i in 0..r.len() {
        let (a, b) = (r[i], r[(i + 1) % r.len()]);
        if (a.1 > c.1) != (b.1 > c.1) {
            xs.push(a.0 + (c.1 - a.1) / (b.1 - a.1) * (b.0 - a.0));
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks(2)
        .filter(|w| w.len() == 2)
        .max_by(|a, b| (a[1] - a[0]).total_cmp(&(b[1] - b[0])))
        .map_or(c, |w| ((w[0] + w[1]) / 2.0, c.1))
}

/// Wall top above its bottom: the height or the highest point of the top profile.
pub(crate) fn wall_top(m: &Model, id: i64, height: f64) -> f64 {
    match m.wall_top_profile(id).ok().flatten() {
        Some(p) => p.iter().map(|q| q.1).fold(height, f64::max),
        None => height,
    }
}

/// Point and left normal of a wall axis at the fraction `u` of its length, straight or arc
/// (the lean moves an arc's axis radially, along this normal). None when degenerate.
fn axis_at(w: &WallSpec, bulge: f64, u: f64) -> Option<(V2, V2)> {
    let (a, b) = (v2(w.a.0, w.a.1), v2(w.b.0, w.b.1));
    let len = (b - a).len();
    if !(len > 1e-9) {
        return None;
    }
    if bulge == 0.0 {
        let d = (b - a) * (1.0 / len);
        return Some((a + (b - a) * u, v2(-d.y, d.x)));
    }
    let ax = ddc_geom::arc::ArcAxis::new(a, b, bulge, ddc_geom::Tol::default()).ok()?;
    let s = ax.length() * u;
    let t = ax.tangent_at(s);
    Some((ax.point_at(s), v2(-t.y, t.x)))
}

/// Lean grip of a wall with a flat top, straight or arc: at 3/4 of the axis on top of the axis
/// surface, moving along the wall's left normal there; the anchor is the same point on the
/// vertical wall. A top profile keeps the wall vertical (the model refuses a lean there).
/// Returns (position, direction, anchor) in scene coordinates (`zb`: wall base).
fn lean_grip(m: &Model, id: i64, w: &WallSpec, zb: f64, profiled: bool) -> Result<Option<(Vec3, Vec3, Vec3)>, Error> {
    if profiled || !(w.height > 0.0) {
        return Ok(None);
    }
    let Some((p, n)) = axis_at(w, m.wall_bulge(id)?, 0.75) else {
        return Ok(None);
    };
    let s = m.wall_lean(id)?.shift(w.height);
    let zt = zb + w.height;
    let anchor = p3(p.x, p.y, zt);
    let pos = p3(p.x + n.x * s, p.y + n.y * s, zt);
    Ok(Some((pos, Vec3::new(n.x as f32, n.y as f32, 0.0), anchor)))
}

/// Lean angle that moves the top of a wall of height `h` by `shift` across it.
pub(crate) fn lean_of(shift: f64, h: f64) -> f64 {
    (shift / h).atan()
}

pub(crate) fn elev(m: &Model, level: i64) -> Result<f64, Error> {
    Ok(m.sql()
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })?)
}

pub(crate) fn wall_spec(m: &Model, id: i64) -> Result<WallSpec, Error> {
    Ok(m.sql().query_row(
        "SELECT ax,ay,bx,by,level,base_offset,height,wtype,offset0 FROM wall WHERE id=?1",
        [id],
        |r| {
            Ok(WallSpec {
                a: (r.get(0)?, r.get(1)?),
                b: (r.get(2)?, r.get(3)?),
                level: r.get(4)?,
                base_offset: r.get(5)?,
                height: r.get(6)?,
                wall_type: r.get(7)?,
                offset0: r.get(8)?,
            })
        },
    )?)
}

pub(crate) fn opening_row(m: &Model, id: i64) -> Result<(i64, OpeningSpec), Error> {
    Ok(m.sql().query_row(
        "SELECT wall, along, width, sill, height, kind FROM opening WHERE id=?1",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                OpeningSpec {
                    along: r.get(1)?,
                    width: r.get(2)?,
                    sill: r.get(3)?,
                    height: r.get(4)?,
                    kind: r.get(5)?,
                },
            ))
        },
    )?)
}

pub(crate) fn ceiling_spec(m: &Model, id: i64) -> Result<CeilingSpec, Error> {
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

/// Total layer thickness of a slab type.
pub(crate) fn type_thickness(m: &Model, stype: i64) -> f64 {
    m.sql()
        .query_row(
            "SELECT COALESCE(SUM(thickness),0) FROM slab_type_layer WHERE stype=?1",
            [stype],
            |r| r.get(0),
        )
        .unwrap_or(0.0)
}

fn centroid(r: &[(f64, f64)]) -> (f64, f64) {
    let n = r.len().max(1) as f64;
    let (x, y) = r.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    (x / n, y / n)
}

pub(crate) fn p3(x: f64, y: f64, z: f64) -> Vec3 {
    Vec3::new(x as f32, y as f32, z as f32)
}

/// Absolute elevation of a plan grip by role.
fn plan_z(m: &Model, g: &crate::grips::Grip) -> Result<f64, Error> {
    use crate::grips::{Kind, Role};
    let id = g.id;
    Ok(match g.kind {
        Kind::Wall => {
            let w = wall_spec(m, id)?;
            elev(m, w.level)? + w.base_offset
        }
        Kind::Slab => {
            let (lv, off): (i64, f64) =
                m.sql()
                    .query_row("SELECT level, offset FROM slab WHERE id=?1", [id], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
            elev(m, lv)? + off
        }
        Kind::Ceiling => {
            let c = ceiling_spec(m, id)?;
            elev(m, c.level)? + c.height
        }
        Kind::Roof => {
            let r = m.roof(id)?;
            elev(m, r.level)? + r.offset
        }
        Kind::Column => {
            let c = m.column(id)?;
            match g.role {
                Role::End(1) => elev(m, c.top_level.unwrap_or(c.level))? + c.top_offset,
                _ => elev(m, c.level)? + c.base_offset,
            }
        }
        Kind::Beam => {
            let b = m.beam(id)?;
            elev(m, b.level)? + if g.role == Role::End(1) { b.b.2 } else { b.a.2 }
        }
        Kind::Stair => {
            let s = m.stair(id)?;
            elev(m, s.level)? + s.base_offset
        }
        Kind::Railing => {
            let lv: i64 =
                m.sql()
                    .query_row("SELECT level FROM railing WHERE id=?1", [id], |r| r.get(0))?;
            let p = m.railing_path(id)?;
            let k = match g.role {
                Role::Vertex(i) => i,
                _ => 0,
            };
            elev(m, lv)? + p.get(k).map_or(0.0, |q| q.2)
        }
        Kind::Comp => m
            .sql()
            .query_row("SELECT wz FROM comp_qty WHERE comp=?1", [id], |r| r.get(0))
            .or_else(|_| -> Result<f64, Error> {
                let s = m.component(id)?;
                Ok(elev(m, s.level)? + s.at.2)
            })?,
        Kind::Mesh => m
            .imported_mesh(id)?
            .mesh
            .positions
            .iter()
            .fold(f64::MAX, |a, p| a.min(p.z)),
        Kind::Curtain => {
            let c = m.curtain_wall(id)?;
            elev(m, c.level)? + c.base_offset
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
        | Kind::RoomSep => crate::elem::struct_grips::plan_z(m, g, &|lv| elev(m, lv))?,
        Kind::Mep
        | Kind::Grid
        | Kind::RefPlane
        | Kind::Level
        | Kind::Opening
        | Kind::Mass
        | Kind::Topo => 0.0,
        Kind::Solid => m
            .solid_quantities(id)?
            .map_or(0.0, |q| (q.bbox[2] + q.bbox[5]) / 2.0),
    })
}

/// Zero-angle direction of a plan rotation grip (a stair faces backward).
fn rot_zero(k: crate::grips::Kind) -> Vec3 {
    if k == crate::grips::Kind::Stair {
        Vec3::NEG_X
    } else {
        Vec3::X
    }
}

/// Plan grips lifted to elevations (except flip, levels and axes; beam ends are handled in 3D).
pub(crate) fn lifted(m: &Model, z0: f64, t: Target, out: &mut Vec<Grip3>) -> Result<(), Error> {
    use crate::grips::{Kind, Role};
    let Some(kind) = t.grip_kind() else {
        return Ok(());
    };
    let gs = crate::grips::grips(m, kind, t.id)?;
    let center = gs.iter().find(|g| g.role == Role::Move).map(|g| g.pos);
    for g in gs {
        if matches!(g.role, Role::Flip | Role::Elevation)
            || matches!(kind, Kind::Grid | Kind::RefPlane | Kind::Level)
        {
            continue;
        }
        if kind == Kind::Beam {
            continue;
        }
        let z = plan_z(m, &g)? - z0;
        let pos = p3(g.pos.x, g.pos.y, z);
        let axis = match (&g.role, center) {
            (Role::Rotate, Some(c)) => Axis3::Ring {
                center: p3(c.x, c.y, z),
                axis: Vec3::Z,
                zero: rot_zero(kind),
            },
            _ => Axis3::Plane(Vec3::Z),
        };
        let anchor = match axis {
            Axis3::Ring { center, .. } => center,
            _ => pos,
        };
        out.push(Grip3 {
            target: t,
            role: Role3::Plan(g),
            pos,
            axis,
            anchor,
        });
    }
    Ok(())
}

impl Provider for Builtin {
    fn owns(&self, t: Target) -> bool {
        use crate::scene_ids::{EXT_MASS, EXT_SOLID, EXT_TOPO};
        match t.cat {
            // mass and terrain: a plan move grip; a solid has its own grips in `solid_scene_edit3d`
            Cat::Ext(c) if c == EXT_MASS || c == EXT_TOPO => true,
            Cat::Ext(c) if c == EXT_SOLID => false,
            Cat::Ext(_) => t.grip_kind().is_some(),
            Cat::Curtain | Cat::Panel | Cat::Mep => false,
            _ => true,
        }
    }

    fn grips(&self, m: &Model, z0: f64, t: Target) -> Result<Vec<Grip3>, Error> {
        let mut out = vec![];
        lifted(m, z0, t, &mut out)?;
        let g = |role: Role3, pos: Vec3, axis: Axis3, anchor: Vec3| Grip3 {
            target: t,
            role,
            pos,
            axis,
            anchor,
        };
        let id = t.id;
        match t.cat {
            Cat::Wall => {
                let w = wall_spec(m, id)?;
                let zb = elev(m, w.level)? + w.base_offset - z0;
                let (mx, my) = ((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0);
                let (base, top) = (p3(mx, my, zb), p3(mx, my, zb + w.height));
                let profiled = m.wall_top_profile(id)?.is_some();
                // a profile-defined top (slopes) is set by the profile, not the height
                if !profiled {
                    out.push(g(Role3::Top, top, Axis3::Line(Vec3::Z), base));
                }
                out.push(g(Role3::Base, base, Axis3::Line(Vec3::NEG_Z), top));
                if let Some((pos, n, anchor)) = lean_grip(m, id, &w, zb, profiled)? {
                    out.push(g(Role3::Lean, pos, Axis3::Line(n), anchor));
                }
            }
            Cat::Slab => {
                let (lv, off, st): (i64, f64, i64) = m.sql().query_row(
                    "SELECT level, offset, stype FROM slab WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                let ring = m.slab_rings(id)?.into_iter().next().unwrap_or_default();
                let (cx, cy) = inside_point(&ring);
                let e = elev(m, lv)? - z0;
                let top = p3(cx, cy, e + off);
                let bottom = p3(cx, cy, e + off - type_thickness(m, st));
                out.push(g(Role3::Offset, top, Axis3::Line(Vec3::Z), p3(cx, cy, e)));
                out.push(g(Role3::Thickness, bottom, Axis3::Line(Vec3::NEG_Z), top));
            }
            Cat::Roof => {
                let r = m.roof(id)?;
                let e = elev(m, r.level)? - z0;
                let (cx, cy) = match &r.kind {
                    RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => {
                        inside_point(ring)
                    }
                    RoofKind::Extrusion { a, b, .. } => ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0),
                };
                let sup = p3(cx, cy, e + r.offset);
                out.push(g(Role3::Offset, sup, Axis3::Line(Vec3::Z), p3(cx, cy, e)));
                if matches!(r.kind, RoofKind::Footprint { .. }) {
                    let bottom = sup - Vec3::Z * type_thickness(m, r.slab_type) as f32;
                    out.push(g(Role3::Thickness, bottom, Axis3::Line(Vec3::NEG_Z), sup));
                }
            }
            Cat::Ceiling => {
                let c = ceiling_spec(m, id)?;
                let e = elev(m, c.level)? - z0;
                let (cx, cy) = inside_point(&c.outer);
                let low = p3(cx, cy, e + c.height);
                let up = low + Vec3::Z * type_thickness(m, c.slab_type) as f32;
                out.push(g(Role3::Offset, low, Axis3::Line(Vec3::Z), p3(cx, cy, e)));
                out.push(g(Role3::Thickness, up, Axis3::Line(Vec3::Z), low));
            }
            Cat::Column => {
                let c = m.column(id)?;
                let zb = elev(m, c.level)? + c.base_offset - z0;
                let zt = elev(m, c.top_level.unwrap_or(c.level))? + c.top_offset - z0;
                let top_xy = c.top_at.unwrap_or(c.at);
                let base = p3(c.at.0, c.at.1, zb);
                let top = p3(top_xy.0, top_xy.1, zt);
                // bottom and top are offset from the insertion point and the upper end (plan grips sit there)
                let side = p3(-c.rotation.sin() * 0.3, c.rotation.cos() * 0.3, 0.0);
                out.push(g(
                    Role3::Top,
                    top + side,
                    Axis3::Line(Vec3::Z),
                    p3(top_xy.0, top_xy.1, zb) + side,
                ));
                out.push(g(
                    Role3::Base,
                    base + side,
                    Axis3::Line(Vec3::NEG_Z),
                    base.with_z(zt as f32),
                ));
            }
            Cat::Beam => {
                let b = m.beam(id)?;
                let e = elev(m, b.level)? - z0;
                let a3 = p3(b.a.0, b.a.1, e + b.a.2);
                let b3 = p3(b.b.0, b.b.1, e + b.b.2);
                // the dimension is the length from the other end
                out.push(g(Role3::End3(0), a3, Axis3::Plane(Vec3::Z), b3));
                out.push(g(Role3::End3(1), b3, Axis3::Plane(Vec3::Z), a3));
                let (axis, zero) = roll_frame(a3, b3);
                let c = (a3 + b3) * 0.5;
                let pos = c + v3(rotate3(f3(zero * 0.6), [0.0; 3], f3(axis), b.rotation));
                out.push(g(
                    Role3::Roll,
                    pos,
                    Axis3::Ring {
                        center: c,
                        axis,
                        zero,
                    },
                    c,
                ));
            }
            Cat::Opening => {
                let (wall, o) = opening_row(m, id)?;
                let w = wall_spec(m, wall)?;
                let zb = elev(m, w.level)? + w.base_offset - z0;
                let a = p3(w.a.0, w.a.1, zb);
                let d = (p3(w.b.0, w.b.1, zb) - a).normalize_or(Vec3::X);
                let at = |s: f64, z: f64| a + d * s as f32 + Vec3::Z * z as f32;
                let mid = o.along + o.width / 2.0;
                let zc = o.sill + o.height / 2.0;
                out.push(g(
                    Role3::Along,
                    at(mid, zc),
                    Axis3::Line(d),
                    at(o.width / 2.0, zc),
                ));
                out.push(g(
                    Role3::Sill,
                    at(mid, o.sill),
                    Axis3::Line(Vec3::Z),
                    at(mid, 0.0),
                ));
                out.push(g(
                    Role3::Head,
                    at(mid, o.sill + o.height),
                    Axis3::Line(Vec3::Z),
                    at(mid, o.sill),
                ));
                out.push(g(
                    Role3::Width,
                    at(o.along + o.width, zc),
                    Axis3::Line(d),
                    at(o.along, zc),
                ));
            }
            Cat::Comp => {
                let s = m.component(id)?;
                if let Some(mv) = out.iter().find(
                    |x| matches!(&x.role, Role3::Plan(p) if p.role == crate::grips::Role::Move),
                ) {
                    let o = mv.pos;
                    let e = elev(m, s.level)? - z0;
                    let side = p3(-s.rotation.sin() * 0.3, s.rotation.cos() * 0.3, 0.0);
                    let pos = o - side;
                    out.push(g(
                        Role3::Elevation,
                        pos,
                        Axis3::Line(Vec3::Z),
                        pos.with_z(e as f32),
                    ));
                }
            }
            _ => {}
        }
        Ok(out)
    }

    fn apply(&self, m: &mut Model, _z0: f64, g: &Grip3, to: Vec3) -> Result<(), Error> {
        let id = g.target.id;
        let v = g.value(to) as f64;
        let d = g.delta(to);
        match &g.role {
            Role3::Plan(p) => crate::grips::apply(m, p, v2(p.pos.x + d.0, p.pos.y + d.1)),
            Role3::Top | Role3::Base if g.target.cat == Cat::Wall => {
                let mut w = wall_spec(m, id)?;
                if g.role == Role3::Top {
                    w.height += d.2;
                } else {
                    // the top stays: the bottom moves and the height shrinks by the same shift
                    w.base_offset += d.2;
                    w.height -= d.2;
                }
                if !(w.height > 0.01) {
                    return Err(bad("e3d.err.wall_height"));
                }
                // openings (sill and top are measured from the wall bottom and move with it) must stay within it
                let head = m.openings_head(id)?;
                if m.wall_top_profile(id)?.is_none() && head > w.height + 1e-6 {
                    return Err(bad("e3d.err.wall_below_openings"));
                }
                let bulge = m.wall_bulge(id)?;
                m.apply_edits(&[ddc_model::Edit::Wall { id, spec: w, bulge }])
            }
            Role3::Top | Role3::Base => {
                let mut c = m.column(id)?;
                let base_abs = elev(m, c.level)? + c.base_offset;
                let top_abs = elev(m, c.top_level.unwrap_or(c.level))? + c.top_offset;
                if g.role == Role3::Top {
                    c.top_offset += d.2;
                    if top_abs + d.2 - base_abs < 0.01 {
                        return Err(bad("e3d.err.column_height"));
                    }
                } else {
                    c.base_offset += d.2;
                    if top_abs - (base_abs + d.2) < 0.01 {
                        return Err(bad("e3d.err.column_height"));
                    }
                }
                m.update_column(id, &c)
            }
            Role3::Offset => match g.target.cat {
                Cat::Slab => {
                    let (level, slab_type): (i64, i64) = m.sql().query_row(
                        "SELECT level, stype FROM slab WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    m.apply_edits(&[ddc_model::Edit::Slab {
                        id,
                        level,
                        slab_type,
                        offset: v,
                    }])
                }
                Cat::Roof => {
                    let mut r = m.roof(id)?;
                    r.offset = v;
                    m.update_roof(id, &r)
                }
                Cat::Ceiling => {
                    let mut c = ceiling_spec(m, id)?;
                    c.height = v;
                    m.update_ceiling(id, &c)
                }
                _ => Err(bad("e3d.err.unsupported")),
            },
            Role3::Thickness => {
                let table = match g.target.cat {
                    Cat::Slab => "slab",
                    Cat::Roof => "roof",
                    Cat::Ceiling => "ceiling",
                    _ => return Err(bad("e3d.err.unsupported")),
                };
                set_thickness(m, table, id, v)
            }
            Role3::End3(k) => {
                let mut b = m.beam(id)?;
                let end = if *k == 0 { &mut b.a } else { &mut b.b };
                *end = (end.0 + d.0, end.1 + d.1, end.2 + d.2);
                let l =
                    ((b.b.0 - b.a.0).powi(2) + (b.b.1 - b.a.1).powi(2) + (b.b.2 - b.a.2).powi(2))
                        .sqrt();
                if l < 0.01 {
                    return Err(bad("e3d.err.beam_length"));
                }
                m.update_beam(id, &b)
            }
            Role3::Roll => {
                let mut b = m.beam(id)?;
                b.rotation = (v).to_radians();
                m.update_beam(id, &b)
            }
            Role3::Along | Role3::Sill | Role3::Head | Role3::Width => {
                let (wall, o) = opening_row(m, id)?;
                let o = clamp_opening(m, wall, &o, &g.role, v)?;
                if !(o.width > 0.01 && o.height > 0.01) {
                    return Err(bad("e3d.err.opening"));
                }
                m.update_opening(id, &o)
            }
            Role3::Elevation => {
                let mut s = m.component(id)?;
                s.at.2 = v;
                m.mep_update_device(id, &s)
            }
            Role3::Lean => {
                let w = wall_spec(m, id)?;
                let cur = m.wall_lean(id)?;
                m.set_wall_lean(id, ddc_model::Lean::new(lean_of(v, w.height), cur.top_scale))
            }
            Role3::Ext(_) => Err(bad("e3d.err.unsupported")),
        }
    }

    fn preview(&self, m: &Model, z0: f64, g: &Grip3, to: Vec3) -> Result<Vec<Vec<Vec3>>, Error> {
        let lift = |ls: Vec<Vec<V2>>, z: f32| -> Vec<Vec<Vec3>> {
            ls.into_iter()
                .map(|l| l.into_iter().map(|p| p3(p.x, p.y, 0.0).with_z(z)).collect())
                .collect()
        };
        let id = g.target.id;
        Ok(match &g.role {
            Role3::Plan(p) => {
                let d = g.delta(to);
                let c = crate::grips::compute(m, p, v2(p.pos.x + d.0, p.pos.y + d.1))?;
                let mut out = lift(crate::grips::outline(&c), g.pos.z);
                match &c {
                    crate::grips::Change::Comp(cid, s) => {
                        out.extend(crate::mep_follow::device_ghosts(m, *cid, s, z0)?)
                    }
                    crate::grips::Change::Mep(rid, s) => {
                        out.extend(crate::mep_follow::run_ghosts(m, *rid, s, z0)?)
                    }
                    _ => {}
                }
                out
            }
            Role3::Elevation if g.target.cat == Cat::Comp => {
                let mut s = m.component(id)?;
                s.at.2 = g.value(to) as f64;
                let mut out = vec![vec![g.anchor, to]];
                out.extend(crate::mep_follow::device_ghosts(m, id, &s, z0)?);
                out
            }
            Role3::Along | Role3::Sill | Role3::Head | Role3::Width => {
                // opening within the wall: the outline after clamping
                let (wall, o) = opening_row(m, id)?;
                let c = clamp_opening(m, wall, &o, &g.role, g.value(to) as f64)?;
                let w = wall_spec(m, wall)?;
                let zb = (elev(m, w.level)? + w.base_offset - z0) as f32;
                let a = p3(w.a.0, w.a.1, 0.0).with_z(zb);
                let dir = (p3(w.b.0, w.b.1, 0.0).with_z(zb) - a).normalize_or(Vec3::X);
                let at = |s: f64, z: f64| a + dir * s as f32 + Vec3::Z * z as f32;
                let (s0, s1, z0o, z1) = (c.along, c.along + c.width, c.sill, c.sill + c.height);
                vec![vec![
                    at(s0, z0o),
                    at(s1, z0o),
                    at(s1, z1),
                    at(s0, z1),
                    at(s0, z0o),
                ]]
            }
            Role3::Top | Role3::Base if g.target.cat == Cat::Wall => {
                let w = wall_spec(m, id)?;
                vec![vec![
                    p3(w.a.0, w.a.1, 0.0).with_z(to.z),
                    p3(w.b.0, w.b.1, 0.0).with_z(to.z),
                ]]
            }
            Role3::Lean => {
                // the leaning axis surface: base axis, top axis moved along the local normal
                // (radially on an arc wall)
                let w = wall_spec(m, id)?;
                let bulge = m.wall_bulge(id)?;
                let s = g.value(to) as f64;
                let zb = g.anchor.z - w.height as f32;
                let k = if bulge == 0.0 { 1 } else { 24 };
                let (mut base, mut top) = (vec![], vec![]);
                for j in 0..=k {
                    let Some((p, n)) = axis_at(&w, bulge, j as f64 / k as f64) else {
                        return Ok(vec![]);
                    };
                    base.push(p3(p.x, p.y, 0.0).with_z(zb));
                    top.push(p3(p.x + n.x * s, p.y + n.y * s, 0.0).with_z(g.anchor.z));
                }
                base.extend(top.into_iter().rev());
                base.push(base[0]);
                vec![base]
            }
            Role3::Offset | Role3::Thickness => {
                let ring: Vec<(f64, f64)> = match g.target.cat {
                    Cat::Slab => m.slab_rings(id)?.into_iter().next().unwrap_or_default(),
                    Cat::Ceiling => ceiling_spec(m, id)?.outer,
                    Cat::Roof => match m.roof(id)?.kind {
                        RoofKind::Footprint { ring, .. } | RoofKind::Arrow { ring, .. } => ring,
                        RoofKind::Extrusion { a, b, .. } => vec![a, b],
                    },
                    _ => vec![],
                };
                let mut l: Vec<Vec3> = ring
                    .iter()
                    .map(|q| p3(q.0, q.1, 0.0).with_z(to.z))
                    .collect();
                if let Some(&f) = l.first() {
                    l.push(f);
                }
                vec![l]
            }
            Role3::End3(k) => {
                let b = m.beam(id)?;
                let e = (elev(m, b.level)? - z0) as f32;
                let other = if *k == 0 { b.b } else { b.a };
                vec![vec![
                    p3(other.0, other.1, other.2).with_z(e + other.2 as f32),
                    to,
                ]]
            }
            _ => vec![vec![g.anchor, to]],
        })
    }
}

/// An opening after dragging grip `role` to value `v`: along stays within the wall
/// `[0, L − width]`, width extends to the wall end, sill is not below the bottom, top is not above the wall top.
pub fn clamp_opening(
    m: &Model,
    wall: i64,
    o: &OpeningSpec,
    role: &Role3,
    v: f64,
) -> Result<OpeningSpec, Error> {
    let len = m.wall_length(wall)?;
    let w = wall_spec(m, wall)?;
    let top = wall_top(m, wall, w.height);
    let mut o = o.clone();
    match role {
        Role3::Along => o.along = v.clamp(0.0, (len - o.width).max(0.0)),
        Role3::Width => o.width = v.min(len - o.along),
        Role3::Sill => {
            let head = o.sill + o.height;
            o.sill = v.max(0.0).min(head.max(0.0));
            o.height = head - o.sill;
        }
        Role3::Head => o.height = v.min(top - o.sill),
        _ => {}
    }
    Ok(o)
}

/// Beam axis and the section's "up" direction at rotation 0.
pub fn roll_frame(a: Vec3, b: Vec3) -> (Vec3, Vec3) {
    let axis = (b - a).normalize_or(Vec3::X);
    let side = axis.cross(Vec3::Z).normalize_or(Vec3::Y);
    (axis, side.cross(axis).normalize_or(Vec3::Z))
}

/// New thickness of an element's type in one operation: if the type is used only by it, the layers
/// are scaled; if shared, a copy of the type with scaled layers is assigned to the element.
pub fn set_thickness(m: &mut Model, table: &'static str, id: i64, t: f64) -> Result<(), Error> {
    if !(t > 0.005) || !t.is_finite() {
        return Err(bad("e3d.err.thickness"));
    }
    m.batch_walls_edit("wall_e3d_thickness", |db, snaps| {
        let stype: i64 = db.query_row(
            &format!("SELECT stype FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )?;
        let mut st = db.prepare(
            "SELECT pos, material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos",
        )?;
        let layers: Vec<(i64, String, f64)> = st
            .query_map([stype], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let total: f64 = layers.iter().map(|l| l.2).sum();
        if !(total > 0.0) {
            return Err(bad("e3d.err.no_layers"));
        }
        let k = t / total;
        let users: i64 = db.query_row(
            "SELECT (SELECT COUNT(*) FROM slab WHERE stype=?1) + \
             (SELECT COUNT(*) FROM roof WHERE stype=?1) + \
             (SELECT COUNT(*) FROM ceiling WHERE stype=?1)",
            [stype],
            |r| r.get(0),
        )?;
        if users <= 1 {
            snaps.save(db, "slab_type_layer", "stype", stype)?;
            db.execute(
                "UPDATE slab_type_layer SET thickness = thickness * ?2 WHERE stype=?1",
                (stype, k),
            )?;
            return Ok(());
        }
        let name: String =
            db.query_row("SELECT name FROM slab_type WHERE id=?1", [stype], |r| {
                r.get(0)
            })?;
        let base = format!("{name} {:.0}", t * 1000.0);
        let mut cand = base.clone();
        let mut n = 2;
        while db.query_row(
            "SELECT COUNT(*) FROM slab_type WHERE name=?1",
            [&cand],
            |r| r.get::<_, i64>(0),
        )? > 0
        {
            cand = format!("{base} ({n})");
            n += 1;
        }
        db.execute("INSERT INTO slab_type(name) VALUES (?1)", [&cand])?;
        let nid = db.last_insert_rowid();
        for (pos, mat, th) in &layers {
            db.execute(
                "INSERT INTO slab_type_layer(stype, pos, material, thickness) VALUES (?1,?2,?3,?4)",
                (nid, *pos, mat.as_str(), th * k),
            )?;
        }
        // per-type user parameter values and prop rows go with the copy (prop is watermarked
        // by the batch; pparam_val is not)
        snaps.watermark(db, "pparam_val")?;
        ddc_model::copy_type_data(db, "slab_type", stype, nid)?;
        snaps.save(db, table, "id", id)?;
        db.execute(
            &format!("UPDATE {table} SET stype=?2 WHERE id=?1"),
            (id, nid),
        )?;
        Ok(())
    })
}
