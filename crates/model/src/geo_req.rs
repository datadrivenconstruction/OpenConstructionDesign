//! Typed geometry requests: sweeps and lofts from a catalogue profile or a polygon along a path
//! given by points or by a model element, with twist and end scale. One request is one undo
//! step; a request that cannot be built is refused with a reason and writes nothing.
//! `plan_geo` lowers without writing (preview), `apply_geo` writes. The ops layer binds
//! `GeoReq` to its wire format; this module only lowers to `SolidSpec` / `MassSpec`.
//!
//! Lowering: plain sweep -> solid `Sweep`; end scale -> solid `SweptBlend`; twist along one
//! straight vertical segment -> mass `Blend` of rotated sections; a curve path, a styled sweep
//! (frame, roll, eased laws, role) or twist along any other path -> solid `Sweep` on a smooth
//! path (rotation-minimizing frame, exact quantities); two-section loft -> solid
//! `Blend` (exact quantities), more sections -> mass `Blend`; loft along a path -> `SweptBlend`.
//! A path taken from a wall axis or a slab edge is stored as `path_ref` in `solid.def` and
//! re-resolved on every read, so the solid follows later edits of that element.

use std::f64::consts::PI;

use ddc_geom::arc::BulgeRing;
use ddc_geom::frame3::{FrameMode, Law, Role, Seg, Smooth};
use ddc_geom::gsolid::{self, Form, Frame, Path3, Section};
use ddc_geom::profile_cat::{self, ProfileSpec};
use ddc_geom::{v2, v3, V3};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::{Error, MassSpec, Model, SolidSpec};

/// Chord sag for arcs turned into polygons (mass sections), metres.
const SAG: f64 = 1e-3;
/// Largest twist step between two mass sections.
const TWIST_STEP: f64 = PI / 36.0;
const MAX_TWIST: f64 = 4.0 * PI;
const MAX_SCALE: f64 = 100.0;

/// Profile of a sweep or a loft section.
#[derive(Clone, Debug, PartialEq)]
pub enum ProfileReq {
    /// Catalogue kind, polygon or rings, every length with an explicit unit.
    Catalogue(ProfileSpec),
    /// Ready section in metres.
    Section(Section),
}

/// Sweep path. Heights are metres above the request level unless stated otherwise.
#[derive(Clone, Debug, PartialEq)]
pub enum PathReq {
    /// Points `[x, y, z]`; `bulges` one per segment (empty: all straight); `closed` adds the
    /// segment back to the first point (a closed loop makes a ring solid without end caps).
    Points {
        pts: Vec<[f64; 3]>,
        bulges: Vec<f64>,
        closed: bool,
    },
    /// Wall axis a -> b at `z` above the wall base; the solid follows later wall edits.
    WallAxis { wall: i64, z: f64 },
    /// Slab boundary ring (0 outer, 1.. holes) at `z` above the slab top; closed; follows
    /// later slab edits.
    SlabEdge { slab: i64, ring: usize, z: f64 },
    /// Drawing polyline at `z` above the level. Copied once: later drawing edits are not followed.
    Polyline { polyline: i64, z: f64 },
    /// Smooth 3D path from `start` (lines, 3-point arcs in any plane, helices, splines, NURBS).
    Curve { start: [f64; 3], segs: Vec<Seg> },
}

/// How the profile rides a smooth path. Default: rotation-minimizing frame, no roll, linear
/// laws, generic element.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SweepStyle {
    /// Keep the profile `v` axis toward this direction (a handrail stays upright with `[0,0,1]`).
    pub frame: Option<[f64; 3]>,
    /// Constant turn of the profile about the path, radians.
    pub roll: f64,
    /// Twist and scale with zero slope at both ends instead of linear.
    pub ease: bool,
    pub role: Role,
}

/// Common fields of a request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct GeoMeta {
    /// None: the level of the referenced element (required for point paths and plain lofts).
    pub level: Option<i64>,
    pub name: String,
    pub material: String,
    pub void: bool,
    /// Semantic IFC class; only a solid takes one (a mass lowering is then refused).
    pub ifc: Option<crate::SolidIfc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SweepReq {
    pub meta: GeoMeta,
    pub profile: ProfileReq,
    pub path: PathReq,
    /// Total turn of the profile from start to end, radians, counter-clockwise seen from above.
    pub twist: f64,
    /// Profile scale at the path end (1 = none), linear along the path.
    pub scale: f64,
    pub style: SweepStyle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoftSection {
    pub profile: ProfileReq,
    /// Plan offset x, y and height z above the level; ignored for a loft along a path.
    pub at: [f64; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoftReq {
    pub meta: GeoMeta,
    pub sections: Vec<LoftSection>,
    /// None: sections stacked vertically by `at`. Some: exactly two sections, start and end.
    pub path: Option<PathReq>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GeoReq {
    Sweep(SweepReq),
    Loft(LoftReq),
}

/// What a request becomes before it is written.
#[derive(Clone, Debug, PartialEq)]
pub enum GeoPlan {
    /// `follow` is the stored path reference, if the path came from a wall or a slab.
    Solid {
        spec: SolidSpec,
        follow: Option<Value>,
    },
    Mass(MassSpec),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeoBuilt {
    Solid(i64),
    Mass(i64),
}

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn elevation(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("no level {level}")))
}

// ---------- profiles ----------

fn section_of(p: &ProfileReq) -> Result<Section, Error> {
    match p {
        ProfileReq::Catalogue(s) => {
            profile_cat::build(s).map_err(|e| Error::Invalid(format!("profile: {e}")))
        }
        ProfileReq::Section(s) => {
            gsolid::check_section(s).map_err(|e| Error::Degenerate(format!("profile: {e}")))?;
            Ok(s.clone())
        }
    }
}

/// Ring scaled by `k`, turned by `ang` about the profile origin, then shifted by (dx, dy).
fn xform_ring(r: &BulgeRing, k: f64, ang: f64, dx: f64, dy: f64) -> BulgeRing {
    let (c, s) = (ang.cos(), ang.sin());
    BulgeRing {
        pts: r
            .pts
            .iter()
            .map(|p| v2(k * (c * p.x - s * p.y) + dx, k * (s * p.x + c * p.y) + dy))
            .collect(),
        bulges: r.bulges.clone(),
    }
}

fn plan_ring(r: &BulgeRing) -> Vec<(f64, f64)> {
    gsolid::tess(r, SAG).iter().map(|p| (p.x, p.y)).collect()
}

// ---------- paths ----------

struct Resolved {
    level: i64,
    path: Path3,
    follow: Option<Value>,
    closed: bool,
}

fn closed_path(pts: Vec<V3>, bulges: Vec<f64>) -> Path3 {
    let mut p = Path3 { pts, bulges, smooth: None };
    if let Some(&first) = p.pts.first() {
        p.pts.push(first);
    }
    p
}

fn need_level(level: Option<i64>, what: &str) -> Result<i64, Error> {
    level.ok_or_else(|| Error::Invalid(format!("path: level is required for {what}")))
}

fn resolve(db: &Connection, p: &PathReq, level: Option<i64>) -> Result<Resolved, Error> {
    match p {
        PathReq::Points {
            pts,
            bulges,
            closed,
        } => {
            let level = need_level(level, "a point path")?;
            elevation(db, level)?;
            let n = pts.len();
            if n < 2 || (*closed && n < 3) {
                return invalid(format!(
                    "path: needs at least {} points, got {n}",
                    if *closed { 3 } else { 2 }
                ));
            }
            let segs = if *closed { n } else { n - 1 };
            let bulges = if bulges.is_empty() {
                vec![0.0; segs]
            } else if bulges.len() == segs {
                bulges.clone()
            } else {
                return invalid(format!(
                    "path: {} bulges for {segs} segments",
                    bulges.len()
                ));
            };
            let pts: Vec<V3> = pts.iter().map(|q| v3(q[0], q[1], q[2])).collect();
            let path = if *closed {
                closed_path(pts, bulges)
            } else {
                Path3 { pts, bulges, smooth: None }
            };
            Ok(Resolved {
                level,
                path,
                follow: None,
                closed: *closed,
            })
        }
        PathReq::WallAxis { wall, z } => {
            let row: Option<(i64, f64, f64, f64, f64, f64, f64)> = db
                .query_row(
                    "SELECT level, ax, ay, bx, by, base_offset, bulge FROM wall WHERE id=?1",
                    [wall],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )
                .optional()?;
            let Some((wl, ax, ay, bx, by, base, bulge)) = row else {
                return invalid(format!("path: no wall {wall}"));
            };
            let level = level.unwrap_or(wl);
            let h = elevation(db, wl)? + base + z - elevation(db, level)?;
            Ok(Resolved {
                level,
                path: Path3 {
                    pts: vec![v3(ax, ay, h), v3(bx, by, h)],
                    bulges: vec![bulge],
                    smooth: None,
                },
                follow: Some(json!({"kind": "wall_axis", "id": wall, "z": z})),
                closed: false,
            })
        }
        PathReq::SlabEdge { slab, ring, z } => {
            let row: Option<(i64, f64)> = db
                .query_row(
                    "SELECT level, offset FROM slab WHERE id=?1",
                    [slab],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((sl, off)) = row else {
                return invalid(format!("path: no slab {slab}"));
            };
            let rings = crate::slabs::rings(db, *slab)?;
            let Some(r) = rings.get(*ring).filter(|r| r.len() >= 3) else {
                return invalid(format!("path: slab {slab} has no ring {ring}"));
            };
            let level = level.unwrap_or(sl);
            let h = elevation(db, sl)? + off + z - elevation(db, level)?;
            let pts: Vec<V3> = r.iter().map(|p| v3(p.0, p.1, h)).collect();
            let n = pts.len();
            Ok(Resolved {
                level,
                path: closed_path(pts, vec![0.0; n]),
                follow: Some(json!({"kind": "slab_edge", "id": slab, "ring": ring, "z": z})),
                closed: true,
            })
        }
        PathReq::Polyline { polyline, z } => {
            let level = need_level(level, "a drawing polyline")?;
            elevation(db, level)?;
            let row: Option<(i64, Option<i64>)> = db
                .query_row(
                    "SELECT closed, block FROM d_polyline WHERE id=?1",
                    [polyline],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((closed, block)) = row else {
                return invalid(format!("path: no drawing polyline {polyline}"));
            };
            if block.is_some() {
                return invalid(format!(
                    "path: polyline {polyline} belongs to a block definition"
                ));
            }
            let mut st = db.prepare(
                "SELECT x, y, bulge FROM d_vertex WHERE polyline=?1 ORDER BY idx",
            )?;
            let vs: Vec<(f64, f64, f64)> = st
                .query_map([polyline], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            let pts: Vec<[f64; 3]> = vs.iter().map(|v| [v.0, v.1, *z]).collect();
            let mut bulges: Vec<f64> = vs.iter().map(|v| v.2).collect();
            if closed == 0 {
                bulges.pop();
            }
            resolve(
                db,
                &PathReq::Points {
                    pts,
                    bulges,
                    closed: closed != 0,
                },
                Some(level),
            )
        }
        PathReq::Curve { start, segs } => {
            let level = need_level(level, "a curve path")?;
            elevation(db, level)?;
            let sm = Smooth::new(v3(start[0], start[1], start[2]), segs.clone());
            sm.geos().map_err(|e| Error::Invalid(format!("path: {e}")))?;
            let closed = sm.closed();
            Ok(Resolved {
                level,
                path: Path3::smooth(sm),
                follow: None,
                closed,
            })
        }
    }
}

fn ref_of(v: &Value) -> Option<PathReq> {
    let id = v.get("id")?.as_i64()?;
    let z = v.get("z")?.as_f64()?;
    match v.get("kind")?.as_str()? {
        "wall_axis" => Some(PathReq::WallAxis { wall: id, z }),
        "slab_edge" => Some(PathReq::SlabEdge {
            slab: id,
            ring: usize::try_from(v.get("ring")?.as_u64()?).ok()?,
            z,
        }),
        _ => None,
    }
}

/// Called from `solids::read`: a stored path reference replaces the stored path by the current
/// one. A reference that no longer resolves (element deleted) keeps the last stored path.
pub(crate) fn follow(db: &Connection, def: &Value, level: i64, form: &mut Form) {
    let Some(req) = def.get("path_ref").and_then(ref_of) else {
        return;
    };
    let Ok(r) = resolve(db, &req, Some(level)) else {
        return;
    };
    if let Form::Sweep { path, .. } | Form::SweptBlend { path, .. } = form {
        // a styled smooth sweep keeps its laws on the new trace
        *path = match &path.smooth {
            Some(sm) => Path3::smooth(sm.with_polyline(&r.path.pts, &r.path.bulges)),
            None => r.path,
        };
    }
}

// ---------- lowering ----------

fn solid_plan(
    db: &Connection,
    meta: &GeoMeta,
    level: i64,
    frame: Frame,
    form: Form,
    follow: Option<Value>,
    what: &str,
) -> Result<GeoPlan, Error> {
    gsolid::validate(&form).map_err(|e| Error::Degenerate(format!("{what}: {e}")))?;
    let spec = SolidSpec {
        level,
        name: meta.name.clone(),
        void: meta.void,
        material: meta.material.clone(),
        frame,
        form,
        ifc: meta.ifc.clone(),
    };
    crate::solids::check(db, &spec)?;
    Ok(GeoPlan::Solid { spec, follow })
}

fn mass_plan(
    db: &Connection,
    meta: &GeoMeta,
    level: i64,
    sections: Vec<(Vec<(f64, f64)>, f64)>,
    what: &str,
) -> Result<GeoPlan, Error> {
    if let Some(c) = &meta.ifc {
        return invalid(format!(
            "{what}: this form lowers to a mass, which takes no IFC class ({})",
            c.class
        ));
    }
    elevation(db, level)?;
    let spec = MassSpec {
        level,
        name: meta.name.clone(),
        void: meta.void,
        inplace: true,
        form: crate::Form::Blend {
            sections,
            ids: vec![],
        },
    };
    spec.form
        .eval()
        .map_err(|e| Error::Degenerate(format!("{what}: {e}")))?;
    Ok(GeoPlan::Mass(spec))
}

fn plan_sweep(db: &Connection, r: &SweepReq) -> Result<GeoPlan, Error> {
    if !(r.twist.is_finite() && r.twist.abs() <= MAX_TWIST) {
        return invalid("sweep: twist must be a finite angle within 720 degrees");
    }
    if !(r.scale.is_finite() && r.scale > 0.0 && r.scale <= MAX_SCALE) {
        return invalid("sweep: end scale must be in (0, 100]");
    }
    let profile = section_of(&r.profile)?;
    let p = resolve(db, &r.path, r.meta.level)?;
    let (twisted, scaled) = (r.twist.abs() > 1e-12, (r.scale - 1.0).abs() > 1e-12);
    if (twisted || scaled) && !profile.holes.is_empty() {
        return invalid("sweep: twist and end scale need a profile without holes");
    }
    if (twisted || scaled) && p.closed {
        return invalid("sweep: twist and end scale need an open path (a closed loop has no end)");
    }
    let st = &r.style;
    let styled = st.frame.is_some() || st.roll != 0.0 || st.ease || st.role != Role::Generic;
    if twisted && !styled && p.path.smooth.is_none() && vertical(&p.path) {
        return twisted_sweep(db, r, p, &profile.outer);
    }
    if twisted || styled || p.path.smooth.is_some() {
        return smooth_sweep(db, r, p, profile);
    }
    let form = if scaled {
        Form::SweptBlend {
            top: xform_ring(&profile.outer, r.scale, 0.0, 0.0, 0.0),
            bottom: profile.outer,
            path: p.path,
        }
    } else {
        Form::Sweep {
            profile,
            path: p.path,
        }
    };
    solid_plan(
        db,
        &r.meta,
        p.level,
        Frame::default(),
        form,
        p.follow,
        "sweep",
    )
}

fn vertical(path: &Path3) -> bool {
    let pts = &path.pts;
    pts.len() == 2
        && path.bulges.iter().all(|b| *b == 0.0)
        && (pts[1].x - pts[0].x).hypot(pts[1].y - pts[0].y) < 1e-9
        && (pts[1].z - pts[0].z).abs() > 1e-6
}

/// Sweep on a smooth path: polyline arcs become exact arcs/helices, twist and scale become laws.
fn smooth_sweep(
    db: &Connection,
    r: &SweepReq,
    p: Resolved,
    profile: Section,
) -> Result<GeoPlan, Error> {
    let st = &r.style;
    let mut sm = match p.path.smooth {
        Some(sm) => sm,
        None => Smooth::from_polyline(&p.path.pts, &p.path.bulges),
    };
    let law = |a: f64, b: f64| if st.ease { Law::Ease(a, b) } else { Law::Linear(a, b) };
    sm.twist = law(0.0, r.twist);
    sm.scale = law(1.0, r.scale);
    sm.roll = st.roll;
    sm.role = st.role;
    if let Some(up) = st.frame {
        if !up.iter().all(|x| x.is_finite()) || up.iter().map(|x| x * x).sum::<f64>() < 1e-18 {
            return invalid("sweep: frame direction must be a non-zero vector");
        }
        sm.frame = FrameMode::Fixed(v3(up[0], up[1], up[2]));
    }
    if !st.roll.is_finite() {
        return invalid("sweep: roll must be a finite angle");
    }
    let form = Form::Sweep {
        profile,
        path: Path3::smooth(sm),
    };
    solid_plan(
        db,
        &r.meta,
        p.level,
        Frame::default(),
        form,
        p.follow,
        "sweep",
    )
}

/// Twisted sweep along one straight vertical segment: mass sections every 5 degrees.
fn twisted_sweep(
    db: &Connection,
    r: &SweepReq,
    p: Resolved,
    outer: &BulgeRing,
) -> Result<GeoPlan, Error> {
    let pts = &p.path.pts;
    let (a, b) = (pts[0], pts[1]);
    // profile axes on a vertical path (as in gsolid): u = +x, v = +y going up, -y going down
    let down = b.z < a.z;
    let base = BulgeRing {
        pts: outer
            .pts
            .iter()
            .map(|q| v2(q.x, if down { -q.y } else { q.y }))
            .collect(),
        bulges: if down {
            outer.bulges.iter().map(|x| -x).collect()
        } else {
            outer.bulges.clone()
        },
    };
    let n = ((r.twist.abs() / TWIST_STEP).ceil() as usize).max(1);
    let mut sections: Vec<(Vec<(f64, f64)>, f64)> = (0..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let ring = xform_ring(
                &base,
                1.0 + (r.scale - 1.0) * t,
                r.twist * t,
                a.x,
                a.y,
            );
            (plan_ring(&ring), a.z + (b.z - a.z) * t)
        })
        .collect();
    if down {
        sections.reverse();
    }
    mass_plan(db, &r.meta, p.level, sections, "sweep")
}

fn plan_loft(db: &Connection, r: &LoftReq) -> Result<GeoPlan, Error> {
    let n = r.sections.len();
    if n < 2 {
        return invalid(format!("loft: needs at least two sections, got {n}"));
    }
    let mut secs = Vec::with_capacity(n);
    for (i, s) in r.sections.iter().enumerate() {
        let sec = section_of(&s.profile).map_err(|e| match e {
            Error::Invalid(m) => Error::Invalid(format!("loft: section {i}: {m}")),
            Error::Degenerate(m) => Error::Degenerate(format!("loft: section {i}: {m}")),
            e => e,
        })?;
        if !sec.holes.is_empty() {
            return invalid(format!(
                "loft: section {i} has holes; loft sections must be single rings"
            ));
        }
        if !s.at.iter().all(|x| x.is_finite()) {
            return invalid(format!("loft: section {i}: position is not finite"));
        }
        secs.push((sec.outer, s.at));
    }
    if let Some(path) = &r.path {
        if n != 2 {
            return invalid(format!(
                "loft: along a path takes exactly two sections (start and end), got {n}"
            ));
        }
        let p = resolve(db, path, r.meta.level)?;
        if p.closed {
            return invalid("loft: along a path needs an open path (a closed loop has no end)");
        }
        let form = Form::SweptBlend {
            bottom: secs[0].0.clone(),
            top: secs[1].0.clone(),
            path: p.path,
        };
        return solid_plan(
            db,
            &r.meta,
            p.level,
            Frame::default(),
            form,
            p.follow,
            "loft",
        );
    }
    let level = r
        .meta
        .level
        .ok_or_else(|| Error::Invalid("loft: level is required without a path".into()))?;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| secs[i].1[2].total_cmp(&secs[j].1[2]));
    for w in order.windows(2) {
        if secs[w[1]].1[2] - secs[w[0]].1[2] < 1e-6 {
            return invalid(format!(
                "loft: sections {} and {} are at the same height",
                w[0], w[1]
            ));
        }
    }
    let placed = |i: usize| {
        let (ring, at) = &secs[i];
        xform_ring(ring, 1.0, 0.0, at[0], at[1])
    };
    if n == 2 {
        // one blend solid keeps exact quantities
        let (lo, hi) = (order[0], order[1]);
        let z0 = secs[lo].1[2];
        let form = Form::Blend {
            bottom: placed(lo),
            top: placed(hi),
            height: secs[hi].1[2] - z0,
        };
        let frame = Frame {
            o: v3(0.0, 0.0, z0),
            ..Frame::default()
        };
        return solid_plan(db, &r.meta, level, frame, form, None, "loft");
    }
    let sections = order
        .iter()
        .map(|&i| (plan_ring(&placed(i)), secs[i].1[2]))
        .collect();
    mass_plan(db, &r.meta, level, sections, "loft")
}

impl Model {
    /// Lowers a request without writing anything (preview, dry run).
    pub fn plan_geo(&self, r: &GeoReq) -> Result<GeoPlan, Error> {
        match r {
            GeoReq::Sweep(s) => plan_sweep(&self.db, s),
            GeoReq::Loft(l) => plan_loft(&self.db, l),
        }
    }

    /// Builds a request as one undo step.
    pub fn apply_geo(&mut self, r: &GeoReq) -> Result<GeoBuilt, Error> {
        let kind = match r {
            GeoReq::Sweep(_) => "solid_sweep",
            GeoReq::Loft(_) => "solid_loft",
        };
        match self.plan_geo(r)? {
            GeoPlan::Solid { spec, follow } => {
                let id = self.op(kind, |db| {
                    let id = crate::solids::write(db, &spec, None)?;
                    if let Some(f) = &follow {
                        let mut def = crate::solids::form_json(&spec.form);
                        def["path_ref"] = f.clone();
                        db.execute(
                            "UPDATE solid SET def=?2 WHERE id=?1",
                            params![id, def.to_string()],
                        )?;
                    }
                    Ok((id, id, Inv::default().delete("solid", "id", id)))
                })?;
                Ok(GeoBuilt::Solid(id))
            }
            GeoPlan::Mass(m) => Ok(GeoBuilt::Mass(self.add_mass(&m)?)),
        }
    }
}

#[cfg(test)]
#[path = "geo_req_tests.rs"]
mod tests;
