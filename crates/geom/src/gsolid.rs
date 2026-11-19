//! Parametric solids of the generic model: extrusion, blend (loft), revolution, sweep along a path,
//! and swept blend. Profiles are rings with arcs (bulge); the solid is built in the local system
//! of the work plane [`Frame`]. Quantities use exact formulas (area and first moment of rings
//! with arcs, Pappus-Guldinus, prismatoid); the mesh serves views, sections and host cuts.

use crate::arc::BulgeRing;
use crate::frame3::{self, Smooth};
use crate::math::{v2, v3, Tol, V2, V3};
use crate::mesh::Mesh;
use crate::poly;
use crate::profile::{signed_area, Profile};
use crate::Solid;

pub use crate::gsolid_vol::{
    boolean_volume, clip_volume, intersect_volume, prism_halves, prism_overlap, through_area, Body,
    Half,
};

use std::f64::consts::PI;

/// Profile: outer ring and holes (any orientation; normalized on build).
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub outer: BulgeRing,
    pub holes: Vec<BulgeRing>,
}

impl Section {
    pub fn polygon(pts: Vec<V2>) -> Self {
        Section {
            outer: BulgeRing::from_polygon(pts),
            holes: vec![],
        }
    }
}

/// Work plane: origin and axes. `z` is given explicitly because det < 0 under mirroring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub o: V3,
    pub x: V3,
    pub y: V3,
    pub z: V3,
}

impl Default for Frame {
    fn default() -> Self {
        Frame {
            o: v3(0.0, 0.0, 0.0),
            x: v3(1.0, 0.0, 0.0),
            y: v3(0.0, 1.0, 0.0),
            z: v3(0.0, 0.0, 1.0),
        }
    }
}

impl Frame {
    /// Horizontal plane at height `z`.
    pub fn level(z: f64) -> Self {
        Frame {
            o: v3(0.0, 0.0, z),
            ..Default::default()
        }
    }
    /// Vertical plane through `o` along plan direction `dir`: x along `dir`, y up.
    pub fn vertical(o: V3, dir: V2) -> Self {
        let l = dir.len().max(1e-300);
        let x = v3(dir.x / l, dir.y / l, 0.0);
        let y = v3(0.0, 0.0, 1.0);
        Frame {
            o,
            x,
            y,
            z: x.cross(y),
        }
    }
    pub fn at(&self, p: V3) -> V3 {
        self.o + self.x * p.x + self.y * p.y + self.z * p.z
    }
    pub fn dir(&self, d: V3) -> V3 {
        self.x * d.x + self.y * d.y + self.z * d.z
    }
    /// Local coordinates of a world point (axes are assumed orthonormal).
    pub fn local(&self, p: V3) -> V3 {
        let d = p - self.o;
        v3(d.dot(self.x), d.dot(self.y), d.dot(self.z))
    }
    pub fn det(&self) -> f64 {
        self.x.cross(self.y).dot(self.z)
    }
    /// Axes are finite, unit length and mutually perpendicular (tolerance 1e-6).
    pub fn is_valid(&self) -> bool {
        let ok = |v: V3| v.x.is_finite() && v.y.is_finite() && v.z.is_finite();
        let unit = |v: V3| (v.len() - 1.0).abs() < 1e-6;
        ok(self.o)
            && [self.x, self.y, self.z].iter().all(|&v| ok(v) && unit(v))
            && self.x.dot(self.y).abs() < 1e-6
            && self.x.dot(self.z).abs() < 1e-6
            && self.y.dot(self.z).abs() < 1e-6
    }
}

/// Path in the local system: points and the bulge of each segment (arc in xy projection, z linear).
/// `smooth` is a smooth 3D path (arcs in any plane, helices, splines) with twist and scale
/// laws: the solid is then built from it, and `pts` are its vertices for display.
#[derive(Clone, Debug, PartialEq)]
pub struct Path3 {
    pub pts: Vec<V3>,
    pub bulges: Vec<f64>,
    pub smooth: Option<Smooth>,
}

impl Path3 {
    pub fn polyline(pts: Vec<V3>) -> Self {
        let n = pts.len().saturating_sub(1);
        Path3 {
            pts,
            bulges: vec![0.0; n],
            smooth: None,
        }
    }

    /// Smooth path; `pts` are its vertices, `bulges` zero.
    pub fn smooth(sm: Smooth) -> Self {
        let pts = sm.vertices();
        let n = pts.len().saturating_sub(1);
        Path3 {
            pts,
            bulges: vec![0.0; n],
            smooth: Some(sm),
        }
    }

    /// Moves vertex `i` (a smooth path moves its own vertex and refreshes `pts`).
    pub fn move_vertex(&mut self, i: usize, p: V3) {
        if let Some(sm) = &mut self.smooth {
            if sm.set_vertex(i, p) {
                self.pts = sm.vertices();
            }
        } else if let Some(q) = self.pts.get_mut(i) {
            *q = p;
        }
    }
}

/// Solid kind. Angles are radians; profiles lie in the local xy plane of the work plane.
#[derive(Clone, Debug, PartialEq)]
pub enum Form {
    /// Profile x [start, end] along local z.
    Extrusion {
        profile: Section,
        start: f64,
        end: f64,
    },
    /// Bottom contour at z = 0, top at z = height; linear blend between corresponding points.
    Blend {
        bottom: BulgeRing,
        top: BulgeRing,
        height: f64,
    },
    /// Revolution of the profile about an axis (point, direction) in the xy plane from `start` to `end`.
    Revolve {
        profile: Section,
        axis_o: V2,
        axis_d: V2,
        start: f64,
        end: f64,
    },
    /// Profile (u to the left of the path, v "up") swept along the path with mitered joints on the bisector.
    Sweep { profile: Section, path: Path3 },
    /// Like sweep, but the contour changes from `bottom` at the path start to `top` at the end.
    SweptBlend {
        bottom: BulgeRing,
        top: BulgeRing,
        path: Path3,
    },
}

impl Form {
    pub fn kind(&self) -> &'static str {
        match self {
            Form::Extrusion { .. } => "extrusion",
            Form::Blend { .. } => "blend",
            Form::Revolve { .. } => "revolve",
            Form::Sweep { .. } => "sweep",
            Form::SweptBlend { .. } => "swept_blend",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormError {
    /// Profile ring: (ring, reason).
    Profile(usize, String),
    /// Depth/height not above tolerance, or not a number.
    Depth,
    /// Revolution angle outside (0, 2π], or not a number.
    Angle,
    /// Revolution axis is degenerate or crosses the profile.
    Axis,
    /// Path: fewer than two points, zero-length segment, or reversal.
    Path(String),
    /// Solid self-intersects (path joint sharper than the profile radius, etc.).
    SelfIntersect(String),
}

impl std::fmt::Display for FormError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FormError::Profile(r, m) => write!(f, "profile ring {r}: {m}"),
            FormError::Depth => f.write_str("depth must be positive"),
            FormError::Angle => f.write_str("angle must be in (0, 360°]"),
            FormError::Axis => f.write_str("axis is degenerate or crosses the profile"),
            FormError::Path(m) => write!(f, "path: {m}"),
            FormError::SelfIntersect(m) => write!(f, "self-intersection: {m}"),
        }
    }
}

/// Volume and total surface area.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Qty {
    pub volume: f64,
    pub area: f64,
}

/// Chord sagitta for views.
pub const VIEW_SAG: f64 = 2e-3;
/// Chord sagitta for quantities where no exact formula exists (path arcs).
pub const QTY_SAG: f64 = 1e-6;

// ---------- arcs ----------

fn edge_ok(p: V2, q: V2, b: f64) -> bool {
    b.is_finite() && b.abs() >= 1e-9 && (q - p).len() > 0.0
}

/// Arc of edge p→q: (center, radius, angle of p, signed central angle).
fn arc_of(p: V2, q: V2, b: f64) -> (V2, f64, f64, f64) {
    let ch = q - p;
    let c = ch.len();
    let r = c * (1.0 + b * b) / (4.0 * b.abs());
    let th = 4.0 * b.atan();
    let m = crate::arc::edge_mid(p, q, b);
    let h = (p + q) * 0.5;
    let w = m - h;
    let wl = w.len();
    let center = if wl > 0.0 { m - w * (r / wl) } else { h };
    let v = p - center;
    (center, r, v.y.atan2(v.x), th)
}

fn polar(c: V2, r: f64, a: f64) -> V2 {
    c + v2(a.cos(), a.sin()) * r
}

fn step_for(r: f64, sag: f64) -> f64 {
    if r <= sag {
        return PI / 8.0;
    }
    (2.0 * (1.0 - sag / r).acos()).clamp(1e-4, PI / 8.0)
}

/// Interior points of arc p→q (endpoints excluded).
fn arc_pts(p: V2, q: V2, b: f64, sag: f64) -> Vec<V2> {
    if !edge_ok(p, q, b) {
        return vec![];
    }
    let (c, r, a0, th) = arc_of(p, q, b);
    let k = (th.abs() / step_for(r, sag)).ceil().max(1.0) as usize;
    (1..k)
        .map(|j| polar(c, r, a0 + th * j as f64 / k as f64))
        .collect()
}

/// Signed area and first moment (Sx = ∫x dA, Sy = ∫y dA) of a ring with arcs, exact.
pub fn ring_moments(r: &BulgeRing) -> (f64, V2) {
    let n = r.pts.len();
    let (mut a, mut s) = (0.0, v2(0.0, 0.0));
    for i in 0..n {
        let (p, q) = (r.pts[i], r.pts[(i + 1) % n]);
        let c = p.cross(q);
        a += c / 2.0;
        s = s + (p + q) * (c / 6.0);
        let b = r.bulges.get(i).copied().unwrap_or(0.0);
        if edge_ok(p, q, b) {
            let (center, rad, _, th) = arc_of(p, q, b);
            let t = th.abs();
            let seg = rad * rad / 2.0 * (t - t.sin());
            let m = crate::arc::edge_mid(p, q, b);
            let w = m - center;
            let wl = w.len().max(1e-300);
            let d = 4.0 * rad * (t / 2.0).sin().powi(3) / (3.0 * (t - t.sin()).max(1e-300));
            let g = center + w * (d / wl);
            let sg = b.signum() * seg;
            a += sg;
            s = s + g * sg;
        }
    }
    (a, s)
}

/// Length and "center of mass" of each edge (for an arc, the centroid of the arc line).
fn edges_of(r: &BulgeRing) -> Vec<(f64, V2)> {
    let n = r.pts.len();
    (0..n)
        .map(|i| {
            let (p, q) = (r.pts[i], r.pts[(i + 1) % n]);
            let b = r.bulges.get(i).copied().unwrap_or(0.0);
            if edge_ok(p, q, b) {
                let (c, rad, _, th) = arc_of(p, q, b);
                let t = th.abs();
                let m = crate::arc::edge_mid(p, q, b);
                let w = m - c;
                let wl = w.len().max(1e-300);
                let d = rad * (t / 2.0).sin() / (t / 2.0);
                (rad * t, c + w * (d / wl))
            } else {
                ((q - p).len(), (p + q) * 0.5)
            }
        })
        .collect()
}

/// Ring polygon with sagitta `sag`.
pub fn tess(r: &BulgeRing, sag: f64) -> Vec<V2> {
    let n = r.pts.len();
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (p, q) = (r.pts[i], r.pts[(i + 1) % n]);
        out.push(p);
        out.extend(arc_pts(p, q, r.bulges.get(i).copied().unwrap_or(0.0), sag));
    }
    out
}

fn check_ring(i: usize, r: &BulgeRing) -> Result<(), FormError> {
    let e = |m: &str| FormError::Profile(i, m.to_string());
    if r.pts.len() < 2 || r.bulges.len() != r.pts.len() {
        return Err(e("needs ≥ 2 points and one bulge per edge"));
    }
    let fin = r.pts.iter().all(|p| p.x.is_finite() && p.y.is_finite())
        && r.bulges.iter().all(|b| b.is_finite() && b.abs() < 1e3);
    if !fin {
        return Err(e("non-finite coordinate or bulge"));
    }
    let t = tess(r, VIEW_SAG);
    poly::simple_ring(&t, 1e-6).map_err(e)
}

/// Profile check: simple rings, holes inside the outer ring and not touching each other.
pub fn check_section(s: &Section) -> Result<(), FormError> {
    check_ring(0, &s.outer)?;
    for (k, h) in s.holes.iter().enumerate() {
        check_ring(k + 1, h)?;
    }
    if !s.holes.is_empty() {
        let (o, hs) = oriented(s, VIEW_SAG);
        Profile::new(o, hs, Tol::default()).map_err(|e| FormError::Profile(0, format!("{e:?}")))?;
    }
    Ok(())
}

/// Profile polygons: outer counter-clockwise, holes clockwise.
fn oriented(s: &Section, sag: f64) -> (Vec<V2>, Vec<Vec<V2>>) {
    let mut o = tess(&s.outer, sag);
    if signed_area(&o) < 0.0 {
        o.reverse();
    }
    let hs = s
        .holes
        .iter()
        .map(|h| {
            let mut v = tess(h, sag);
            if signed_area(&v) > 0.0 {
                v.reverse();
            }
            v
        })
        .collect();
    (o, hs)
}

/// Area (> 0) and centroid of the profile, exact.
pub fn section_moments(s: &Section) -> (f64, V2) {
    let (a0, s0) = ring_moments(&s.outer);
    let k = a0.signum();
    let (mut a, mut m) = (a0 * k, s0 * k);
    for h in &s.holes {
        let (ah, sh) = ring_moments(h);
        let kh = ah.signum();
        a -= ah * kh;
        m = m - sh * kh;
    }
    let c = if a.abs() > 1e-300 {
        m * (1.0 / a)
    } else {
        v2(0.0, 0.0)
    };
    (a, c)
}

fn perimeter(s: &Section) -> f64 {
    std::iter::once(&s.outer)
        .chain(&s.holes)
        .map(|r| r.perimeter())
        .sum()
}

// ---------- validation ----------

fn axis_unit(d: V2) -> Result<V2, FormError> {
    let l = d.len();
    if !(l.is_finite() && l > 1e-9) {
        return Err(FormError::Axis);
    }
    Ok(d * (1.0 / l))
}

fn sweep_angle(start: f64, end: f64) -> Result<f64, FormError> {
    let d = end - start;
    if !(d.is_finite() && d > 1e-9 && d <= 2.0 * PI + 1e-9) {
        return Err(FormError::Angle);
    }
    Ok(d.min(2.0 * PI))
}

fn full_turn(d: f64) -> bool {
    (d - 2.0 * PI).abs() < 1e-9
}

/// Signed distances of profile points to the axis: all on one side (touching allowed).
fn axis_side(pts: &[V2], o: V2, d: V2) -> Result<f64, FormError> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in pts {
        let s = d.cross(*p - o);
        lo = lo.min(s);
        hi = hi.max(s);
    }
    if lo < -1e-9 && hi > 1e-9 || hi - lo < 1e-9 {
        return Err(FormError::Axis);
    }
    Ok(if hi > 1e-9 { 1.0 } else { -1.0 })
}

pub fn validate(f: &Form) -> Result<(), FormError> {
    match f {
        Form::Extrusion {
            profile,
            start,
            end,
        } => {
            check_section(profile)?;
            if !(start.is_finite() && end.is_finite() && end - start > 1e-6) {
                return Err(FormError::Depth);
            }
        }
        Form::Blend {
            bottom,
            top,
            height,
        } => {
            check_ring(0, bottom)?;
            check_ring(1, top)?;
            if !(height.is_finite() && *height > 1e-6) {
                return Err(FormError::Depth);
            }
        }
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => {
            check_section(profile)?;
            let d = axis_unit(*axis_d)?;
            sweep_angle(*start, *end)?;
            axis_side(&tess(&profile.outer, VIEW_SAG), *axis_o, d)?;
        }
        Form::Sweep { profile, path } => {
            check_section(profile)?;
            let pts = tess(&profile.outer, VIEW_SAG);
            match &path.smooth {
                Some(sm) => {
                    let st = frame3::stations(sm, VIEW_SAG, VIEW_ANGLE).map_err(sweep_err)?;
                    frame3::check(&st, &pts, VIEW_SAG).map_err(sweep_err)?;
                }
                None => {
                    path_stations(path, VIEW_SAG, &pts)?;
                }
            }
        }
        Form::SweptBlend { bottom, top, path } => {
            if path.smooth.is_some() {
                return Err(FormError::Path(
                    "a swept blend takes a polyline path; a smooth sweep has its own scale law"
                        .into(),
                ));
            }
            check_ring(0, bottom)?;
            check_ring(1, top)?;
            let mut pts = tess(bottom, VIEW_SAG);
            pts.extend(tess(top, VIEW_SAG));
            if path_stations(path, VIEW_SAG, &pts)?.1 {
                return Err(FormError::Path("a swept blend needs an open path".into()));
            }
        }
    }
    Ok(())
}

// ---------- path ----------

/// Path station: point, tangent of the incoming segment, normal of the joint plane, profile axes.
#[derive(Clone, Copy, Debug)]
struct Station {
    p: V3,
    t: V3,
    n: V3,
    u: V3,
    v: V3,
    /// Path length from the start to the station.
    s: f64,
}

/// Path polyline (arcs within `sag`) and whether it is closed: a last point equal to the first
/// closes the loop and is dropped.
fn path_poly(path: &Path3, sag: f64) -> Result<(Vec<V3>, bool), FormError> {
    let n = path.pts.len();
    if n < 2 || path.bulges.len() + 1 != n {
        return Err(FormError::Path(
            "needs ≥ 2 points and one bulge per segment".into(),
        ));
    }
    if !path
        .pts
        .iter()
        .all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
    {
        return Err(FormError::Path("non-finite point".into()));
    }
    let mut out = vec![path.pts[0]];
    for i in 0..n - 1 {
        let (a, b) = (path.pts[i], path.pts[i + 1]);
        let bu = path.bulges[i];
        let (pa, pb) = (v2(a.x, a.y), v2(b.x, b.y));
        let mids = arc_pts(pa, pb, bu, sag);
        let k = mids.len() + 1;
        for (j, m) in mids.into_iter().enumerate() {
            let t = (j + 1) as f64 / k as f64;
            out.push(v3(m.x, m.y, a.z + (b.z - a.z) * t));
        }
        out.push(b);
    }
    for w in out.windows(2) {
        if (w[1] - w[0]).len() < 1e-6 {
            return Err(FormError::Path("zero-length segment".into()));
        }
    }
    let closed = (out[out.len() - 1] - out[0]).len() < 1e-6;
    if closed {
        out.pop();
        if out.len() < 3 {
            return Err(FormError::Path(
                "closed path needs at least 3 distinct points".into(),
            ));
        }
    }
    if let Some(i) = self_crossing(&out, closed) {
        return Err(FormError::SelfIntersect(format!(
            "path crosses itself at segment {}",
            i + 1
        )));
    }
    Ok((out, closed))
}

/// Distance between segments [a, b] and [c, d] in 3D.
fn seg_dist(a: V3, b: V3, c: V3, d: V3) -> f64 {
    let (u, v, w) = (b - a, d - c, a - c);
    let (aa, bb, cc, dd, ee) = (u.dot(u), u.dot(v), v.dot(v), u.dot(w), v.dot(w));
    let den = aa * cc - bb * bb;
    let clamp = |x: f64| x.clamp(0.0, 1.0);
    let mut s = if den > 1e-18 {
        clamp((bb * ee - cc * dd) / den)
    } else {
        0.0
    };
    let t = if cc > 1e-18 {
        clamp((bb * s + ee) / cc)
    } else {
        0.0
    };
    if aa > 1e-18 {
        s = clamp((bb * t - dd) / aa);
    }
    ((a + u * s) - (c + v * t)).len()
}

/// First polyline segment that crosses (touches) a non-adjacent segment.
fn self_crossing(p: &[V3], closed: bool) -> Option<usize> {
    let n = p.len();
    let segs = if closed { n } else { n.saturating_sub(1) };
    let s = |k: usize| (p[k], p[(k + 1) % n]);
    for i in 0..segs {
        for j in i + 2..segs {
            // the last and the first segment meet at the seam of a closed loop
            if closed && i == 0 && j == segs - 1 {
                continue;
            }
            let ((a, b), (c, d)) = (s(i), s(j));
            if seg_dist(a, b, c, d) < 1e-6 {
                return Some(i);
            }
        }
    }
    None
}

/// Rotates `v` by the minimal rotation taking unit `a` to unit `b`.
fn transport(v: V3, a: V3, b: V3) -> V3 {
    let k = a.cross(b);
    let s = k.len();
    let c = a.dot(b);
    if s < 1e-15 {
        return v;
    }
    let k = k * (1.0 / s);
    // Rodrigues
    v * c + k.cross(v) * s + k * (k.dot(v) * (1.0 - c))
}

/// Path stations with profile axes and a fold check at the joints for the points `probe`; the
/// flag tells a closed loop (one station per vertex, the last band returns to station 0).
fn path_stations(
    path: &Path3,
    sag: f64,
    probe: &[V2],
) -> Result<(Vec<Station>, bool), FormError> {
    let (pts, closed) = path_poly(path, sag)?;
    let m = pts.len();
    let ns = if closed { m } else { m - 1 };
    let seg: Vec<V3> = (0..ns)
        .map(|k| (pts[(k + 1) % m] - pts[k]).normalized())
        .collect();
    let turns = if closed { ns } else { ns - 1 };
    for k in 0..turns {
        if seg[k].dot(seg[(k + 1) % ns]) < -1.0 + 1e-9 {
            return Err(FormError::Path("path reverses".into()));
        }
    }
    let t0 = seg[0];
    let up = v3(0.0, 0.0, 1.0);
    let mut u = up.cross(t0);
    if u.len() < 1e-9 {
        u = v3(1.0, 0.0, 0.0);
    }
    let mut u = u.normalized();
    let mut v = t0.cross(u);
    // profile axes on every segment: minimal-rotation transport
    let mut axes = Vec::with_capacity(ns);
    for k in 0..ns {
        if k > 0 {
            u = transport(u, seg[k - 1], seg[k]);
            v = transport(v, seg[k - 1], seg[k]);
        }
        axes.push((u, v));
    }
    if closed {
        // a loop out of one plane comes back twisted: the seam would not match
        let back = transport(axes[ns - 1].0, seg[ns - 1], seg[0]);
        if (back - axes[0].0).len() > 1e-6 {
            return Err(FormError::Path("closed path must lie in one plane".into()));
        }
    }
    let mut out = Vec::with_capacity(m);
    let mut s = 0.0;
    for i in 0..m {
        let kin = if closed {
            (i + ns - 1) % ns
        } else {
            i.saturating_sub(1)
        };
        if i > 0 {
            s += (pts[i] - pts[i - 1]).len();
        }
        let n = if !closed && (i == 0 || i == m - 1) {
            seg[kin]
        } else {
            (seg[kin] + seg[i % ns]).normalized()
        };
        out.push(Station {
            p: pts[i],
            t: seg[kin],
            n,
            u: axes[kin].0,
            v: axes[kin].1,
            s,
        });
    }
    // every profile point needs a positive generator length on every segment
    for k in 0..ns {
        let (a, b) = (out[k], out[(k + 1) % m]);
        let (uk, vk) = axes[k];
        let l = (b.p - a.p).len();
        let tk = seg[k];
        for q in probe {
            let w = uk * q.x + vk * q.y;
            let g = l + w.dot(a.n) / tk.dot(a.n) - w.dot(b.n) / tk.dot(b.n);
            if g <= 1e-9 {
                return Err(FormError::SelfIntersect(format!(
                    "profile folds at path vertex {}",
                    (k + 1) % m
                )));
            }
        }
    }
    Ok((out, closed))
}

/// Profile point (u, v) in the joint plane of station `i` (along the incoming segment axes).
fn at_station(st: &[Station], i: usize, q: V2) -> V3 {
    let s = st[i];
    let w = s.u * q.x + s.v * q.y;
    let t = -w.dot(s.n) / s.t.dot(s.n);
    s.p + w + s.t * t
}

// ---------- mesh ----------

/// Cap triangles for polygons (outer counter-clockwise, holes clockwise):
/// indices into the shared ring vertex list, counter-clockwise.
pub fn cap(outer: &[V2], holes: &[Vec<V2>]) -> Vec<[usize; 3]> {
    if holes.is_empty() {
        return poly::triangulate(outer);
    }
    let Ok(p) = Profile::new(outer.to_vec(), holes.to_vec(), Tol::default()) else {
        return poly::triangulate(outer);
    };
    let Ok(m) = Solid::extrude(&p, 1.0, Tol::default()).triangulate() else {
        return poly::triangulate(outer);
    };
    // brep: per ring, n bottom vertices then n top vertices; the cap is face 1
    let mut map = std::collections::HashMap::new();
    let (mut base, mut off) = (0usize, 0usize);
    for r in p.rings() {
        let n = r.len();
        for i in 0..n {
            map.insert(base + n + i, off + i);
        }
        base += 2 * n;
        off += n;
    }
    m.triangles
        .iter()
        .zip(&m.face_of)
        .filter(|(_, f)| **f == 1)
        .filter_map(|(t, _)| {
            Some([
                *map.get(&(t[0] as usize))?,
                *map.get(&(t[1] as usize))?,
                *map.get(&(t[2] as usize))?,
            ])
        })
        .collect()
}

/// Loft over stations: `rings[k]` are the rings of station k in world space (sizes equal at all
/// stations); rings counter-clockwise (holes clockwise) when looking against the travel direction.
fn loft(
    rings: &[Vec<Vec<V3>>],
    caps: Option<(Vec<[usize; 3]>, Vec<[usize; 3]>)>,
    closed: bool,
) -> Mesh {
    let mut m = Mesh::default();
    let k = rings.len();
    let sizes: Vec<usize> = rings[0].iter().map(|r| r.len()).collect();
    let per: usize = sizes.iter().sum();
    for st in rings {
        for r in st {
            m.positions.extend(r.iter().copied());
        }
    }
    let idx = |s: usize, flat: usize| (s * per + flat) as u32;
    let bands = if closed { k } else { k - 1 };
    for s in 0..bands {
        let s1 = (s + 1) % k;
        let mut off = 0;
        for &n in &sizes {
            for i in 0..n {
                let j = (i + 1) % n;
                let (bi, bj) = (idx(s, off + i), idx(s, off + j));
                let (ti, tj) = (idx(s1, off + i), idx(s1, off + j));
                m.triangles.push([bi, bj, tj]);
                m.triangles.push([bi, tj, ti]);
            }
            off += n;
        }
    }
    if let (false, Some((c0, c1))) = (closed, caps) {
        for t in c0 {
            m.triangles.push([idx(0, t[0]), idx(0, t[2]), idx(0, t[1])]);
        }
        for t in c1 {
            m.triangles
                .push([idx(k - 1, t[0]), idx(k - 1, t[1]), idx(k - 1, t[2])]);
        }
    }
    // degenerate triangles (points on the revolution axis) are not needed
    let pos = &m.positions;
    let keep: Vec<[u32; 3]> = m
        .triangles
        .iter()
        .copied()
        .filter(|t| {
            let [a, b, c] = t.map(|i| pos[i as usize]);
            (b - a).cross(c - a).len() > 1e-18
        })
        .collect();
    m.face_of = vec![0; keep.len()];
    m.triangles = keep;
    if m.volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
    }
    m
}

/// Corresponding points of two rings (counter-clockwise) by a common length parameter from vertex 0.
pub fn correspond(a: &[V2], b: &[V2]) -> (Vec<V2>, Vec<V2>) {
    let params = |r: &[V2]| {
        let n = r.len();
        let mut acc = vec![0.0];
        for i in 0..n {
            let l = acc[i] + (r[(i + 1) % n] - r[i]).len();
            acc.push(l);
        }
        let tot = acc[n].max(1e-300);
        acc.iter().take(n).map(|x| x / tot).collect::<Vec<f64>>()
    };
    let (pa, pb) = (params(a), params(b));
    let mut ts: Vec<f64> = pa.iter().chain(&pb).copied().collect();
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let at = |r: &[V2], p: &[f64], t: f64| {
        let n = r.len();
        let mut i = p.partition_point(|&x| x <= t + 1e-12).saturating_sub(1);
        i = i.min(n - 1);
        let t0 = p[i];
        let t1 = if i + 1 < n { p[i + 1] } else { 1.0 };
        let f = if t1 - t0 > 1e-15 {
            (t - t0) / (t1 - t0)
        } else {
            0.0
        };
        r[i] + (r[(i + 1) % n] - r[i]) * f
    };
    (
        ts.iter().map(|&t| at(a, &pa, t)).collect(),
        ts.iter().map(|&t| at(b, &pb, t)).collect(),
    )
}

fn ccw_poly(r: &BulgeRing, sag: f64) -> Vec<V2> {
    let mut v = tess(r, sag);
    if signed_area(&v) < 0.0 {
        // vertex 0 stays the first given point for either winding
        v.reverse();
        v.rotate_right(1);
    }
    v
}

/// Vertex 0 of each station is the first ring point; blend profile: (bottom, top).
fn blend_pair(bottom: &BulgeRing, top: &BulgeRing, sag: f64) -> (Vec<V2>, Vec<V2>) {
    let a = ccw_poly(bottom, sag);
    let b = align_start(&a, ccw_poly(top, sag));
    correspond(&a, &b)
}

/// Uniform scale `k` and shift `t` with `top = k * bottom + t`, vertex for vertex (from some
/// vertex of `top`, same turning sense, equal bulges), when the blend surface pairs exactly
/// those points: the blend is then a tapered extrusion of `bottom`. A non-uniform scale is
/// not one, since the blend pairs points by relative arc length.
pub fn blend_taper(bottom: &BulgeRing, top: &BulgeRing) -> Option<(f64, V2)> {
    let n = bottom.pts.len();
    if n < 2 || top.pts.len() != n || bottom.bulges.len() != n || top.bulges.len() != n {
        return None;
    }
    let diag = |r: &[V2]| {
        let (mut lo, mut hi) = (r[0], r[0]);
        for p in r {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        }
        (hi - lo).len()
    };
    let (da, db) = (diag(&bottom.pts), diag(&top.pts));
    if da < 1e-9 || db < 1e-9 {
        return None;
    }
    let k = db / da;
    let eps = 1e-9 * db.max(1.0);
    let shift = (0..n).find(|&j| {
        let t = top.pts[j] - bottom.pts[0] * k;
        (0..n).all(|i| {
            let q = top.pts[(i + j) % n];
            (q - (bottom.pts[i] * k + t)).len() <= eps
                && (top.bulges[(i + j) % n] - bottom.bulges[i]).abs() <= 1e-9
        })
    })?;
    let t = top.pts[shift] - bottom.pts[0] * k;
    // the pairing the blend body uses must be the same map
    let sag = 1e-4;
    let (a, b) = blend_pair(bottom, top, sag);
    let tol = 4.0 * sag * (1.0 + k) + eps;
    a.iter()
        .zip(&b)
        .all(|(p, q)| (*q - (*p * k + t)).len() <= tol)
        .then_some((k, t))
}

/// Starts `b` at the vertex that points (from its centre) where `a[0]` points: a square to
/// circle blend whose first points sit on different sides would otherwise twist and pinch.
fn align_start(a: &[V2], mut b: Vec<V2>) -> Vec<V2> {
    let mid = |r: &[V2]| r.iter().fold(v2(0.0, 0.0), |s, p| s + *p) * (1.0 / r.len().max(1) as f64);
    let Some(&a0) = a.first() else {
        return b;
    };
    let (da, cb) = (a0 - mid(a), mid(&b));
    let la = da.len();
    if la < 1e-12 {
        return b;
    }
    let mut best = (0usize, f64::NEG_INFINITY);
    for (i, p) in b.iter().enumerate() {
        let d = *p - cb;
        let l = d.len();
        if l < 1e-12 {
            continue;
        }
        let cos = (da.x * d.x + da.y * d.y) / (la * l);
        if cos > best.1 + 1e-12 {
            best = (i, cos);
        }
    }
    b.rotate_left(best.0);
    b
}

fn lerp(a: &[V2], b: &[V2], t: f64) -> Vec<V2> {
    a.iter().zip(b).map(|(p, q)| *p + (*q - *p) * t).collect()
}

/// Blend steps along the height (and on a straight path segment): faces between steps are nearly planar.
const BLEND_STEPS: usize = 8;

/// Straight path segments are split into `k` parts (arcs are already subdivided by the sagitta).
fn refine_straight(path: &Path3, k: usize) -> Path3 {
    let mut out = Path3 {
        pts: vec![],
        bulges: vec![],
        smooth: None,
    };
    for (i, w) in path.pts.windows(2).enumerate() {
        let b = path.bulges.get(i).copied().unwrap_or(0.0);
        if out.pts.is_empty() {
            out.pts.push(w[0]);
        }
        if b != 0.0 {
            out.bulges.push(b);
            out.pts.push(w[1]);
            continue;
        }
        for j in 1..=k {
            out.bulges.push(0.0);
            out.pts.push(w[0] + (w[1] - w[0]) * (j as f64 / k as f64));
        }
    }
    if out.pts.is_empty() {
        return path.clone();
    }
    out
}

/// Solid mesh in world coordinates; outward normals, closed.
pub fn mesh(f: &Form, fr: &Frame, sag: f64) -> Result<Mesh, FormError> {
    validate(f)?;
    let w = |p: V3| fr.at(p);
    let flat = |r: &[V2], z: f64| r.iter().map(|q| w(v3(q.x, q.y, z))).collect::<Vec<V3>>();
    let m = match f {
        Form::Extrusion {
            profile,
            start,
            end,
        } => {
            let (o, hs) = oriented(profile, sag);
            let c = cap(&o, &hs);
            let st = |z: f64| {
                std::iter::once(flat(&o, z))
                    .chain(hs.iter().map(|h| flat(h, z)))
                    .collect::<Vec<_>>()
            };
            loft(&[st(*start), st(*end)], Some((c.clone(), c)), false)
        }
        Form::Blend {
            bottom,
            top,
            height,
        } => {
            let (a, b) = blend_pair(bottom, top, sag);
            const K: usize = BLEND_STEPS;
            let rings: Vec<Vec<Vec<V3>>> = (0..=K)
                .map(|k| {
                    let t = k as f64 / K as f64;
                    vec![flat(&lerp(&a, &b, t), height * t)]
                })
                .collect();
            loft(
                &rings,
                Some((poly::triangulate(&a), poly::triangulate(&b))),
                false,
            )
        }
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => {
            let d = axis_unit(*axis_d)?;
            let da = sweep_angle(*start, *end)?;
            let (o, hs) = oriented(profile, sag);
            let rmax = o
                .iter()
                .map(|p| d.cross(*p - *axis_o).abs())
                .fold(0.0, f64::max);
            let full = full_turn(da);
            let k = (da / step_for(rmax, sag)).ceil().max(2.0) as usize;
            let ao = v3(axis_o.x, axis_o.y, 0.0);
            let ad = v3(d.x, d.y, 0.0);
            let rot = |q: V2, phi: f64| {
                let p = v3(q.x, q.y, 0.0) - ao;
                let (c, s) = (phi.cos(), phi.sin());
                let r = p * c + ad.cross(p) * s + ad * (ad.dot(p) * (1.0 - c));
                w(ao + r)
            };
            let n = if full { k } else { k + 1 };
            let rings: Vec<Vec<Vec<V3>>> = (0..n)
                .map(|j| {
                    let phi = start + da * j as f64 / k as f64;
                    std::iter::once(&o)
                        .chain(hs.iter())
                        .map(|r| r.iter().map(|q| rot(*q, phi)).collect())
                        .collect()
                })
                .collect();
            let c = cap(&o, &hs);
            loft(&rings, Some((c.clone(), c)), full)
        }
        Form::Sweep {
            profile,
            path: Path3 {
                smooth: Some(sm), ..
            },
        } => {
            let (o, hs) = oriented(profile, sag);
            let sts = frame3::stations(sm, sag.max(1e-7), VIEW_ANGLE).map_err(sweep_err)?;
            let rings: Vec<Vec<Vec<V3>>> = sts
                .st
                .iter()
                .map(|s| {
                    std::iter::once(&o)
                        .chain(hs.iter())
                        .map(|r| r.iter().map(|q| w(s.place(*q))).collect())
                        .collect()
                })
                .collect();
            let c = cap(&o, &hs);
            loft(&rings, Some((c.clone(), c)), sts.closed)
        }
        Form::Sweep { profile, path } => {
            let (o, hs) = oriented(profile, sag);
            let (st, closed) = path_stations(path, sag, &o)?;
            let rings: Vec<Vec<Vec<V3>>> = (0..st.len())
                .map(|i| {
                    std::iter::once(&o)
                        .chain(hs.iter())
                        .map(|r| r.iter().map(|q| w(at_station(&st, i, *q))).collect())
                        .collect()
                })
                .collect();
            let c = cap(&o, &hs);
            loft(&rings, Some((c.clone(), c)), closed)
        }
        Form::SweptBlend { bottom, top, path } => {
            let (a, b) = blend_pair(bottom, top, sag);
            let mut probe = a.clone();
            probe.extend(b.iter().copied());
            let (st, _) = path_stations(&refine_straight(path, BLEND_STEPS), sag, &probe)?;
            let total = st.last().map_or(1.0, |s| s.s).max(1e-300);
            let rings: Vec<Vec<Vec<V3>>> = (0..st.len())
                .map(|i| {
                    let r = lerp(&a, &b, st[i].s / total);
                    vec![r.iter().map(|q| w(at_station(&st, i, *q))).collect()]
                })
                .collect();
            loft(
                &rings,
                Some((poly::triangulate(&a), poly::triangulate(&b))),
                false,
            )
        }
    };
    Ok(m)
}

// ---------- quantities ----------

/// Volume and area: extrusion, revolution, sweep along a polyline are exact (path arcs use
/// sagitta [`QTY_SAG`]); blend volume is exact (prismatoid), area comes from a dense mesh.
pub fn quantities(f: &Form) -> Result<Qty, FormError> {
    validate(f)?;
    Ok(match f {
        Form::Extrusion {
            profile,
            start,
            end,
        } => {
            let (a, _) = section_moments(profile);
            let h = end - start;
            Qty {
                volume: a * h,
                area: 2.0 * a + perimeter(profile) * h,
            }
        }
        Form::Blend {
            bottom,
            top,
            height,
        } => {
            let (a, b) = blend_pair(bottom, top, QTY_SAG.max(1e-5));
            let am = signed_area(&lerp(&a, &b, 0.5)).abs();
            let (a0, a1) = (ring_moments(bottom).0.abs(), ring_moments(top).0.abs());
            let fine = mesh(f, &Frame::default(), 1e-4)?;
            // mesh caps are the same polygons as in the mesh (sagitta 1e-4)
            let (fa, fb) = blend_pair(bottom, top, 1e-4);
            Qty {
                volume: height / 6.0 * (a0 + 4.0 * am + a1),
                area: fine.area() - signed_area(&fa).abs() - signed_area(&fb).abs() + a0 + a1,
            }
        }
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => {
            let d = axis_unit(*axis_d)?;
            let da = sweep_angle(*start, *end)?;
            let (a, c) = section_moments(profile);
            let dist = |p: V2| d.cross(p - *axis_o).abs();
            let lateral: f64 = std::iter::once(&profile.outer)
                .chain(&profile.holes)
                .flat_map(edges_of)
                .map(|(l, g)| l * dist(g))
                .sum::<f64>()
                * da;
            let caps = if full_turn(da) { 0.0 } else { 2.0 * a };
            Qty {
                volume: a * dist(c) * da,
                area: lateral + caps,
            }
        }
        Form::Sweep {
            profile,
            path: Path3 {
                smooth: Some(sm), ..
            },
        } => smooth_qty(profile, sm)?,
        Form::Sweep { profile, path } => {
            let (a, c) = section_moments(profile);
            // folds are checked in validate at VIEW_SAG; here only the station geometry
            let (st, closed) = path_stations(path, QTY_SAG, &[])?;
            let n = st.len();
            let bands = if closed { n } else { n - 1 };
            let gen = |q: V2| -> f64 {
                (0..bands)
                    .map(|k| (at_station(&st, (k + 1) % n, q) - at_station(&st, k, q)).len())
                    .sum()
            };
            let lateral: f64 = std::iter::once(&profile.outer)
                .chain(&profile.holes)
                .flat_map(edges_of)
                .map(|(l, g)| l * gen(g))
                .sum();
            Qty {
                volume: a * gen(c),
                area: lateral + if closed { 0.0 } else { 2.0 * a },
            }
        }
        Form::SweptBlend { .. } => {
            let m = mesh(f, &Frame::default(), 1e-4)?;
            Qty {
                volume: m.volume().abs(),
                area: m.area(),
            }
        }
    })
}

/// Tangent angle step of smooth paths for the view.
pub const VIEW_ANGLE: f64 = 3.0 * PI / 180.0;
const QTY_ANGLE: f64 = 0.5 * PI / 180.0;

fn sweep_err(e: frame3::SweepError) -> FormError {
    match e {
        frame3::SweepError::Path(m) => FormError::Path(m),
        frame3::SweepError::SelfIntersect(m) => FormError::SelfIntersect(m),
    }
}

/// Smooth sweep: volume = A·L·∫σ² exactly when the profile centroid is on the path (any roll,
/// twist and kinks), else by section integration over stations with sag [`QTY_SAG`]. Lateral
/// area: Σ edge·σ·(generator length) without a twist or scale law, else a dense mesh (sag 1e-4).
fn smooth_qty(profile: &Section, sm: &Smooth) -> Result<Qty, FormError> {
    let (a, c) = section_moments(profile);
    let len = sm.length().map_err(sweep_err)?;
    let (o, _) = oriented(profile, VIEW_SAG);
    let extent = o.iter().map(|q| q.len()).fold(0.0, f64::max).max(1e-300);
    let on_path = c.len() <= 1e-12 * extent;
    let plain = sm.twist.is_const() && sm.scale.is_const();
    // advance of profile point q along the chords, weighted by σ (edge) or σ² (section)
    let gen = |sts: &frame3::Stations, q: V2, pow: i32| -> f64 {
        let st = &sts.st;
        let n = st.len();
        let bands = if sts.closed { n } else { n - 1 };
        (0..bands)
            .map(|k| {
                let (p, r) = (&st[k], &st[(k + 1) % n]);
                let d = (r.p - p.p).normalized();
                let w = ((p.scale + r.scale) / 2.0).powi(pow);
                (r.place(q) - p.place(q)).dot(d) * w
            })
            .sum()
    };
    let dense = if !on_path || plain {
        Some(frame3::stations(sm, QTY_SAG, QTY_ANGLE).map_err(sweep_err)?)
    } else {
        None
    };
    let volume = match &dense {
        Some(sts) if !on_path => a * gen(sts, c, 2),
        _ => a * len * frame3::mean_square(sm.scale),
    };
    let (s0, s1) = sm.scale.ends();
    let caps = if sm.closed() {
        0.0
    } else {
        s0 * s0 + s1 * s1
    };
    let area = match &dense {
        Some(sts) if plain => {
            let lateral: f64 = std::iter::once(&profile.outer)
                .chain(&profile.holes)
                .flat_map(edges_of)
                .map(|(l, g)| l * gen(sts, g, 1))
                .sum();
            lateral + a * caps
        }
        _ => {
            let f = Form::Sweep {
                profile: profile.clone(),
                path: Path3::smooth(sm.clone()),
            };
            let m = mesh(&f, &Frame::default(), 1e-4)?;
            let (fo, fh) = oriented(profile, 1e-4);
            let poly_a =
                signed_area(&fo).abs() - fh.iter().map(|h| signed_area(h).abs()).sum::<f64>();
            // mesh caps are polygons: the exact section area goes in their place
            m.area() - poly_a * caps + a * caps
        }
    };
    Ok(Qty { volume, area })
}

// ---------- views ----------

/// Section of the mesh by plane `n·p = c`: oriented segments (counter-clockwise winding when
/// viewed from the tip of `n` onto material lying in the plane).
pub fn oriented_slice(m: &Mesh, n: V3, c: f64) -> Vec<(V3, V3)> {
    let mut out = vec![];
    for t in &m.triangles {
        let p = t.map(|i| m.positions[i as usize]);
        let s = p.map(|q| n.dot(q) - c);
        let mut pts: Vec<V3> = vec![];
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            if (s[a] < 0.0) != (s[b] < 0.0) {
                // edge point in canonical vertex order so neighbors coincide
                let (i, j) = if t[a] < t[b] { (a, b) } else { (b, a) };
                pts.push(p[i] + (p[j] - p[i]) * (s[i] / (s[i] - s[j])));
            }
        }
        if pts.len() != 2 || (pts[0] - pts[1]).len() < 1e-12 {
            continue;
        }
        let nt = (p[1] - p[0]).cross(p[2] - p[0]);
        let dir = n.cross(nt);
        if (pts[1] - pts[0]).dot(dir) >= 0.0 {
            out.push((pts[0], pts[1]));
        } else {
            out.push((pts[1], pts[0]));
        }
    }
    out
}

/// Assembles directed segments into closed contours (endpoints match bitwise or within `eps`).
pub fn chain_directed(segs: &[(V2, V2)], eps: f64) -> Vec<Vec<V2>> {
    use std::collections::HashMap;
    let q = |p: V2| ((p.x / eps).round() as i64, (p.y / eps).round() as i64);
    let mut starts: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        starts.entry(q(s.0)).or_default().push(i);
    }
    let mut used = vec![false; segs.len()];
    let mut out = vec![];
    for s0 in 0..segs.len() {
        if used[s0] {
            continue;
        }
        used[s0] = true;
        let start = segs[s0].0;
        let mut ring = vec![start];
        let mut cur = segs[s0].1;
        let mut closed = false;
        for _ in 0..segs.len() {
            if (cur - start).len() <= eps {
                closed = true;
                break;
            }
            ring.push(cur);
            let (kx, ky) = q(cur);
            let mut next = None;
            'find: for dx in -1..=1 {
                for dy in -1..=1 {
                    if let Some(v) = starts.get(&(kx + dx, ky + dy)) {
                        if let Some(&j) = v
                            .iter()
                            .find(|&&j| !used[j] && (segs[j].0 - cur).len() <= eps)
                        {
                            next = Some(j);
                            break 'find;
                        }
                    }
                }
            }
            let Some(j) = next else { break };
            used[j] = true;
            cur = segs[j].1;
        }
        if closed && ring.len() >= 3 && signed_area(&ring).abs() > 1e-12 {
            out.push(ring);
        }
    }
    out
}

/// Contours (outer counter-clockwise) with their holes.
pub fn nest(rings: Vec<Vec<V2>>) -> Vec<(Vec<V2>, Vec<Vec<V2>>)> {
    let (mut outs, holes): (Vec<Vec<V2>>, Vec<Vec<V2>>) =
        rings.into_iter().partition(|r| signed_area(r) > 0.0);
    outs.sort_by(|a, b| signed_area(a).total_cmp(&signed_area(b)));
    let mut res: Vec<(Vec<V2>, Vec<Vec<V2>>)> = outs.into_iter().map(|o| (o, vec![])).collect();
    for h in holes {
        // smallest outer contour containing the hole
        if let Some(k) = res
            .iter()
            .position(|(o, _)| crate::predicates::point_in_ring(h[0], o) >= 0)
        {
            res[k].1.push(h);
        }
    }
    res
}

/// Plan section of the solid at elevation `z`: contours (outer, holes) in plan.
pub fn plan_cut(m: &Mesh, z: f64) -> Vec<(Vec<V2>, Vec<Vec<V2>>)> {
    let segs: Vec<(V2, V2)> = oriented_slice(m, v3(0.0, 0.0, 1.0), z)
        .into_iter()
        .map(|(a, b)| (v2(a.x, a.y), v2(b.x, b.y)))
        .collect();
    nest(chain_directed(&segs, 1e-9))
}

/// Crease threshold (cos 25°): faces of a smooth surface (cylinder, revolution, path arc) are finer.
pub const CREASE_COS: f64 = 0.906;

/// View lines for view direction `d`: boundaries, creases sharper than [`CREASE_COS`], and the
/// silhouette (faces on opposite sides of the view). Order is deterministic (by vertex index).
pub fn view_edges(m: &Mesh, d: V3) -> Vec<(V3, V3)> {
    use std::collections::{BTreeMap, HashMap};
    let normal = |t: &[u32; 3]| {
        let [a, b, c] = t.map(|i| m.positions[i as usize]);
        let n = (b - a).cross(c - a);
        let l = n.len();
        if l > 0.0 {
            n * (1.0 / l)
        } else {
            n
        }
    };
    let key = |p: V3| {
        let q = |v: f64| (v * 1e7).round() as i64;
        (q(p.x), q(p.y), q(p.z))
    };
    let mut ids: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let canon: Vec<u32> = m
        .positions
        .iter()
        .enumerate()
        .map(|(i, &p)| *ids.entry(key(p)).or_insert(i as u32))
        .collect();
    let mut map: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for (ti, t) in m.triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (canon[t[k] as usize], canon[t[(k + 1) % 3] as usize]);
            if a != b {
                map.entry((a.min(b), a.max(b))).or_default().push(ti);
            }
        }
    }
    let dl = d.len().max(1e-300);
    let d = d * (1.0 / dl);
    let mut out = vec![];
    for ((a, b), ts) in map {
        let keep = ts.len() != 2 || {
            let (n0, n1) = (normal(&m.triangles[ts[0]]), normal(&m.triangles[ts[1]]));
            let (s0, s1) = (n0.dot(d), n1.dot(d));
            n0.dot(n1) < CREASE_COS || (s0 > 1e-9 && s1 < -1e-9) || (s0 < -1e-9 && s1 > 1e-9)
        };
        if keep {
            out.push((m.positions[a as usize], m.positions[b as usize]));
        }
    }
    out
}

/// Projection lines in plan: characteristic edges below `z` (clipped at `z`).
pub fn plan_lines(m: &Mesh, z: f64) -> Vec<(V2, V2)> {
    let mut out = vec![];
    for (a, b) in view_edges(m, v3(0.0, 0.0, -1.0)) {
        let (lo, hi) = if a.z <= b.z { (a, b) } else { (b, a) };
        if lo.z > z {
            continue;
        }
        let hi = if hi.z > z {
            lo + (hi - lo) * ((z - lo.z) / (hi.z - lo.z))
        } else {
            hi
        };
        let (p, q) = (v2(lo.x, lo.y), v2(hi.x, hi.y));
        if (q - p).len() > 1e-9 {
            out.push((p, q));
        }
    }
    out
}

/// Section by a vertical view plane (origin `o`, axis `u`, view along the left normal of `u`) in
/// (s, z) coordinates: outer contours counter-clockwise.
pub fn section_cut(m: &Mesh, o: V2, u: V2) -> Vec<(Vec<V2>, Vec<Vec<V2>>)> {
    let n = v3(-u.y, u.x, 0.0);
    let c = n.dot(v3(o.x, o.y, 0.0));
    // the viewer looks along +n: counter-clockwise in (s, z) when viewed from -n
    let segs: Vec<(V2, V2)> = oriented_slice(m, n * -1.0, -c)
        .into_iter()
        .map(|(a, b)| (sz(a, o, u), sz(b, o, u)))
        .collect();
    nest(chain_directed(&segs, 1e-9))
}

fn sz(p: V3, o: V2, u: V2) -> V2 {
    v2((v2(p.x, p.y) - o).dot(u), p.z)
}

/// Projected edges in a section: characteristic edges in front of the viewer (depth >= 0, up to
/// `far`), clipped by the view plane; hidden lines are not removed.
pub fn section_lines(m: &Mesh, o: V2, u: V2, far: Option<f64>) -> Vec<(V2, V2)> {
    let n = v2(-u.y, u.x);
    let depth = |p: V3| (v2(p.x, p.y) - o).dot(n);
    let mut out = vec![];
    for (a, b) in view_edges(m, v3(n.x, n.y, 0.0)) {
        let (mut a, mut b) = (a, b);
        let (da, db) = (depth(a), depth(b));
        if da < 0.0 && db < 0.0 {
            continue;
        }
        if da < 0.0 {
            a = a + (b - a) * (da / (da - db));
        } else if db < 0.0 {
            b = b + (a - b) * (db / (db - da));
        }
        if let Some(f) = far {
            if depth(a) > f && depth(b) > f {
                continue;
            }
        }
        let (p, q) = (sz(a, o, u), sz(b, o, u));
        if (q - p).len() > 1e-9 {
            out.push((p, q));
        }
    }
    out
}

/// Mesh bounding box: (min, max).
pub fn bbox(m: &Mesh) -> (V3, V3) {
    let mut lo = v3(f64::INFINITY, f64::INFINITY, f64::INFINITY);
    let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in &m.positions {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    (lo, hi)
}

#[cfg(test)]
#[path = "gsolid_tests.rs"]
mod tests;
