// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Smooth 3D sweep paths: lines, arcs in any plane, helices, interpolating splines and NURBS,
//! framed by rotation-minimizing frames (double reflection) or by a fixed reference direction,
//! with roll, twist and scale laws over relative arc length. Sampling is adaptive (chord and
//! tangent angle); a section is mitred only at a true kink (tangent break > [`KINK`]). Lengths
//! are exact: closed forms for lines, arcs and helices, Gauss-Legendre on |C'(t)| for splines.
//! Tight curvature and coils that touch each other are refused with the station named.

use std::f64::consts::{PI, TAU};

use crate::math::{v3, V2, V3};
use crate::nurbs::Curve;

/// Tangent break (rad) above which a joint is a kink and the section is mitred there.
pub const KINK: f64 = 1e-6;

/// One piece of a smooth path; it starts where the previous one ends.
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Line {
        to: V3,
    },
    /// Circular arc through `via` to `to`: the three points fix its plane.
    Arc {
        via: V3,
        to: V3,
    },
    /// Helix about the axis through `center` with direction `axis`: turns by `angle` (rad,
    /// right-hand about `axis`; negative — the other hand) and rises by `rise` along `axis`.
    /// The radius is the distance of the start from the axis; the end follows from the rest.
    Helix {
        center: V3,
        axis: V3,
        angle: f64,
        rise: f64,
    },
    /// Cubic spline through `via` to `to`.
    Spline {
        via: Vec<V3>,
        to: V3,
    },
    /// Explicit NURBS; its first control point is the segment start.
    Nurbs(Curve),
}

/// Value along the path by relative arc length τ ∈ [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Law {
    /// `a` at the start, `b` at the end, linear.
    Linear(f64, f64),
    /// `a` to `b` with zero slope at both ends.
    Ease(f64, f64),
}

impl Law {
    pub const fn constant(v: f64) -> Law {
        Law::Linear(v, v)
    }
    pub fn at(self, t: f64) -> f64 {
        match self {
            Law::Linear(a, b) => a + (b - a) * t,
            Law::Ease(a, b) => a + (b - a) * t * t * (3.0 - 2.0 * t),
        }
    }
    pub fn ends(self) -> (f64, f64) {
        match self {
            Law::Linear(a, b) | Law::Ease(a, b) => (a, b),
        }
    }
    pub fn is_const(self) -> bool {
        let (a, b) = self.ends();
        (a - b).abs() < 1e-12
    }
    fn finite(self) -> bool {
        let (a, b) = self.ends();
        a.is_finite() && b.is_finite()
    }
}

/// How the profile is oriented about the path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameMode {
    /// Rotation-minimizing frame: no spin about the tangent.
    Rmf,
    /// The profile `v` axis stays as close to this direction as the tangent allows (a spiral
    /// handrail stays upright with the helix axis here).
    Fixed(V3),
}

/// What the swept element stands for (its IFC class on export).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Generic,
    Beam,
    Column,
    Member,
    Railing,
    Mullion,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Generic => "generic",
            Role::Beam => "beam",
            Role::Column => "column",
            Role::Member => "member",
            Role::Railing => "railing",
            Role::Mullion => "mullion",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        [
            Role::Generic,
            Role::Beam,
            Role::Column,
            Role::Member,
            Role::Railing,
            Role::Mullion,
        ]
        .into_iter()
        .find(|r| r.name() == s)
    }
}

/// Smooth path with the laws that place the profile on it. Profile axes: `u` to the left of
/// the path, `v` up (as in a polyline sweep); roll and twist turn them counter-clockwise about
/// the tangent.
#[derive(Clone, Debug, PartialEq)]
pub struct Smooth {
    pub start: V3,
    pub segs: Vec<Seg>,
    pub frame: FrameMode,
    /// Constant turn of the profile, rad.
    pub roll: f64,
    /// Extra turn by relative arc length, rad.
    pub twist: Law,
    /// Profile scale by relative arc length.
    pub scale: Law,
    pub role: Role,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SweepError {
    Path(String),
    SelfIntersect(String),
}

impl std::fmt::Display for SweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SweepError::Path(m) => write!(f, "path: {m}"),
            SweepError::SelfIntersect(m) => write!(f, "self-intersection: {m}"),
        }
    }
}

fn path_err<T>(m: impl Into<String>) -> Result<T, SweepError> {
    Err(SweepError::Path(m.into()))
}

fn fin(p: V3) -> bool {
    p.x.is_finite() && p.y.is_finite() && p.z.is_finite()
}

const ZERO: V3 = v3(0.0, 0.0, 0.0);

// ---------- evaluated segments ----------

/// Segment in evaluated form, parameter t ∈ [0, 1].
#[derive(Clone, Debug, PartialEq)]
pub enum Geo {
    Line {
        a: V3,
        d: V3,
    },
    /// Arc (rise 0) or helix: c + ax·rise·t + r·(cos θt·x + sin θt·y).
    Circ {
        c: V3,
        x: V3,
        y: V3,
        ax: V3,
        r: f64,
        theta: f64,
        rise: f64,
    },
    Nurbs {
        cu: Curve,
        u0: f64,
        du: f64,
    },
}

/// 10-point Gauss-Legendre nodes on [-1, 1] and weights.
const GL: [(f64, f64); 5] = [
    (0.148_874_338_981_631_2, 0.295_524_224_714_752_9),
    (0.433_395_394_129_247_2, 0.269_266_719_309_996_3),
    (0.679_409_568_299_024_4, 0.219_086_362_515_982_0),
    (0.865_063_366_688_984_5, 0.149_451_349_150_580_6),
    (0.973_906_528_517_171_7, 0.066_671_344_308_688_1),
];

/// ∫ f over [a, b] by 10-point Gauss-Legendre.
pub fn gauss(a: f64, b: f64, f: impl Fn(f64) -> f64) -> f64 {
    let (m, h) = ((a + b) / 2.0, (b - a) / 2.0);
    GL.iter()
        .map(|&(x, w)| w * (f(m - h * x) + f(m + h * x)))
        .sum::<f64>()
        * h
}

/// Angle step that keeps the chord within `sag` of a circle of radius `r`.
fn step_for(r: f64, sag: f64) -> f64 {
    if r <= sag {
        return PI / 8.0;
    }
    (2.0 * (1.0 - sag / r).acos()).clamp(1e-5, PI / 8.0)
}

fn seg_dist_pt(p: V3, a: V3, b: V3) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + d * t)).len()
}

fn angle_between(a: V3, b: V3) -> f64 {
    a.cross(b).len().atan2(a.dot(b))
}

impl Geo {
    /// Point, first and second derivative at `t`.
    pub fn at(&self, t: f64) -> [V3; 3] {
        match self {
            Geo::Line { a, d } => [*a + *d * t, *d, ZERO],
            Geo::Circ {
                c,
                x,
                y,
                ax,
                r,
                theta,
                rise,
            } => {
                let (s, co) = (theta * t).sin_cos();
                let radial = *x * co + *y * s;
                let tang = *y * co - *x * s;
                [
                    *c + *ax * (rise * t) + radial * *r,
                    *ax * *rise + tang * (r * theta),
                    radial * (-r * theta * theta),
                ]
            }
            Geo::Nurbs { cu, u0, du } => {
                let d = cu.derivs(u0 + du * t, 2);
                let g = |k: usize| d.get(k).copied().unwrap_or(ZERO);
                [g(0), g(1) * *du, g(2) * (du * du)]
            }
        }
    }

    fn speed(&self, t: f64) -> f64 {
        self.at(t)[1].len()
    }

    /// Exact length (closed form; Gauss-Legendre with bisection for NURBS).
    pub fn length(&self) -> f64 {
        self.len_between(0.0, 1.0)
    }

    pub fn len_between(&self, t0: f64, t1: f64) -> f64 {
        match self {
            Geo::Line { d, .. } => d.len() * (t1 - t0),
            Geo::Circ { r, theta, rise, .. } => (r * theta).hypot(*rise) * (t1 - t0),
            Geo::Nurbs { cu, u0, du } => {
                // knot spans are smooth pieces: integrate each on its own
                let mut cuts = vec![t0];
                for k in cu.knots() {
                    let t = (k - u0) / du;
                    if t > t0 + 1e-15 && t < t1 - 1e-15 && cuts.last() != Some(&t) {
                        cuts.push(t);
                    }
                }
                cuts.push(t1);
                cuts.windows(2).map(|w| self.adaptive(w[0], w[1], 0)).sum()
            }
        }
    }

    fn adaptive(&self, a: f64, b: f64, depth: usize) -> f64 {
        let whole = gauss(a, b, |t| self.speed(t));
        if depth >= 12 {
            return whole;
        }
        let m = (a + b) / 2.0;
        let halves = gauss(a, m, |t| self.speed(t)) + gauss(m, b, |t| self.speed(t));
        if (halves - whole).abs() <= 1e-14 * halves.abs().max(1e-300) {
            return halves;
        }
        self.adaptive(a, m, depth + 1) + self.adaptive(m, b, depth + 1)
    }

    /// Sample parameters (with 0 and 1) so that chords stay within `chord` of the curve and
    /// tangents turn by at most `angle` between samples.
    pub fn params(&self, chord: f64, angle: f64) -> Vec<f64> {
        match self {
            Geo::Line { .. } => vec![0.0, 1.0],
            Geo::Circ { r, theta, .. } => {
                let step = step_for(*r, chord).min(angle).max(1e-5);
                let n = ((theta.abs() / step).ceil() as usize).clamp(1, 1 << 20);
                (0..=n).map(|k| k as f64 / n as f64).collect()
            }
            Geo::Nurbs { cu, u0, du } => {
                let mut ks: Vec<f64> = cu
                    .knots()
                    .iter()
                    .map(|k| ((k - u0) / du).clamp(0.0, 1.0))
                    .collect();
                ks.dedup_by(|a, b| (*a - *b).abs() < 1e-15);
                let per = cu.degree().max(1) * 2;
                let mut seeds = vec![0.0];
                for w in ks.windows(2) {
                    for j in 1..=per {
                        seeds.push(w[0] + (w[1] - w[0]) * j as f64 / per as f64);
                    }
                }
                let mut out = vec![0.0];
                for w in seeds.windows(2) {
                    self.refine(w[0], w[1], chord, angle, 0, &mut out);
                }
                out
            }
        }
    }

    fn refine(&self, a: f64, b: f64, chord: f64, angle: f64, depth: usize, out: &mut Vec<f64>) {
        if b - a <= 1e-12 {
            return;
        }
        let ([pa, da, _], [pb, db, _]) = (self.at(a), self.at(b));
        let dev = [0.25, 0.5, 0.75]
            .iter()
            .map(|f| seg_dist_pt(self.at(a + (b - a) * f)[0], pa, pb))
            .fold(0.0, f64::max);
        if depth < 30 && (dev > chord || angle_between(da, db) > angle) {
            let m = (a + b) / 2.0;
            self.refine(a, m, chord, angle, depth + 1, out);
            self.refine(m, b, chord, angle, depth + 1, out);
        } else {
            out.push(b);
        }
    }
}

fn seg_geo(a: V3, s: &Seg) -> Result<Geo, String> {
    match s {
        Seg::Line { to } => {
            if !fin(*to) {
                return Err("non-finite point".into());
            }
            Ok(Geo::Line { a, d: *to - a })
        }
        Seg::Arc { via, to } => {
            if !(fin(*via) && fin(*to)) {
                return Err("non-finite point".into());
            }
            let (u, w) = (*via - a, *to - a);
            let n = u.cross(w);
            let nl = n.len();
            let sc = u.len().max(w.len());
            if !(nl > 1e-9 * sc * sc) || sc < 1e-9 {
                return Err("arc points are collinear".into());
            }
            let o = (n.cross(u) * w.dot(w) + w.cross(n) * u.dot(u)) * (1.0 / (2.0 * nl * nl));
            let c = a + o;
            let r = o.len();
            let x = o * (-1.0 / r);
            let ax = n * (1.0 / nl);
            let y = ax.cross(x);
            let e = *to - c;
            let mut theta = e.dot(y).atan2(e.dot(x));
            if theta <= 1e-12 {
                theta += TAU;
            }
            Ok(Geo::Circ {
                c,
                x,
                y,
                ax,
                r,
                theta,
                rise: 0.0,
            })
        }
        Seg::Helix {
            center,
            axis,
            angle,
            rise,
        } => {
            let al = axis.len();
            if !(fin(*center) && fin(*axis) && al > 1e-12) {
                return Err("helix axis is degenerate".into());
            }
            if !(angle.is_finite() && angle.abs() > 1e-9 && angle.abs() <= 64.0 * PI) {
                return Err("helix angle must be non-zero and within 32 turns".into());
            }
            if !rise.is_finite() {
                return Err("helix rise is not finite".into());
            }
            let ax = *axis * (1.0 / al);
            let w = a - *center;
            let h0 = w.dot(ax);
            let rad = w - ax * h0;
            let r = rad.len();
            if r < 1e-6 {
                return Err("helix start lies on its axis".into());
            }
            let x = rad * (1.0 / r);
            Ok(Geo::Circ {
                c: *center + ax * h0,
                x,
                y: ax.cross(x),
                ax,
                r,
                theta: *angle,
                rise: *rise,
            })
        }
        Seg::Spline { via, to } => {
            let mut pts = vec![a];
            pts.extend(via.iter().copied());
            pts.push(*to);
            if !pts.iter().all(|p| fin(*p)) {
                return Err("non-finite point".into());
            }
            let cu = Curve::interpolate(&pts, 3).map_err(|e| format!("spline: {e:?}"))?;
            let (u0, u1) = cu.domain();
            Ok(Geo::Nurbs {
                cu,
                u0,
                du: u1 - u0,
            })
        }
        Seg::Nurbs(cu) => {
            let (u0, u1) = cu.domain();
            if (cu.point(u0) - a).len() > 1e-6 {
                return Err("NURBS does not start at the previous end".into());
            }
            Ok(Geo::Nurbs {
                cu: cu.clone(),
                u0,
                du: u1 - u0,
            })
        }
    }
}

impl Smooth {
    /// Path with default laws: RMF, no roll or twist, scale 1.
    pub fn new(start: V3, segs: Vec<Seg>) -> Smooth {
        Smooth {
            start,
            segs,
            frame: FrameMode::Rmf,
            roll: 0.0,
            twist: Law::constant(0.0),
            scale: Law::constant(1.0),
            role: Role::Generic,
        }
    }

    /// Polyline with bulges (plan arcs, height linear along each arc) as a smooth path:
    /// straight pieces stay lines, bulged ones become exact arcs or helix pieces.
    pub fn from_polyline(pts: &[V3], bulges: &[f64]) -> Smooth {
        let mut segs = vec![];
        for (i, w) in pts.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let bu = bulges.get(i).copied().unwrap_or(0.0);
            let ch = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
            if bu.abs() < 1e-9 || ch < 1e-9 {
                segs.push(Seg::Line { to: b });
                continue;
            }
            let th = 4.0 * bu.atan();
            // centre: on the chord bisector at distance h, to the left for a CCW arc
            let r = ch / (2.0 * (th / 2.0).sin().abs());
            let h = r * (th / 2.0).cos();
            let (dx, dy) = ((b.x - a.x) / ch, (b.y - a.y) / ch);
            let s = th.signum();
            let cx = (a.x + b.x) / 2.0 - dy * h * s;
            let cy = (a.y + b.y) / 2.0 + dx * h * s;
            segs.push(Seg::Helix {
                center: v3(cx, cy, a.z),
                axis: v3(0.0, 0.0, 1.0),
                angle: th,
                rise: b.z - a.z,
            });
        }
        Smooth::new(pts.first().copied().unwrap_or(ZERO), segs)
    }

    /// Same laws on another polyline (a path that follows a model element).
    pub fn with_polyline(&self, pts: &[V3], bulges: &[f64]) -> Smooth {
        Smooth {
            start: pts.first().copied().unwrap_or(self.start),
            segs: Smooth::from_polyline(pts, bulges).segs,
            ..self.clone()
        }
    }

    pub fn geos(&self) -> Result<Vec<Geo>, SweepError> {
        if self.segs.is_empty() {
            return path_err("needs at least one segment");
        }
        if !fin(self.start) {
            return path_err("non-finite start");
        }
        let mut a = self.start;
        let mut out = Vec::with_capacity(self.segs.len());
        for (i, s) in self.segs.iter().enumerate() {
            let g = seg_geo(a, s).map_err(|m| SweepError::Path(format!("segment {}: {m}", i + 1)))?;
            if !(g.length() > 1e-6) {
                return path_err(format!("segment {}: zero length", i + 1));
            }
            a = g.at(1.0)[0];
            out.push(g);
        }
        Ok(out)
    }

    /// Start and the end of every segment (helix ends computed); empty when invalid.
    pub fn vertices(&self) -> Vec<V3> {
        match self.geos() {
            Ok(g) => std::iter::once(self.start)
                .chain(g.iter().map(|g| g.at(1.0)[0]))
                .collect(),
            Err(_) => vec![],
        }
    }

    pub fn closed(&self) -> bool {
        let v = self.vertices();
        v.len() > 2 && (v[v.len() - 1] - v[0]).len() < 1e-6
    }

    /// Exact length of the path.
    pub fn length(&self) -> Result<f64, SweepError> {
        Ok(self.geos()?.iter().map(Geo::length).sum())
    }

    /// Moves vertex `i` (0 — start, k — end of segment k): a helix end is derived and does not
    /// move. A closed loop keeps its seam. Returns false when the vertex is not free.
    pub fn set_vertex(&mut self, i: usize, p: V3) -> bool {
        let closed = self.closed();
        let n = self.segs.len();
        if i > n || (i > 0 && matches!(self.segs[i - 1], Seg::Helix { .. })) {
            return false;
        }
        if i == 0 {
            if closed && matches!(self.segs[n - 1], Seg::Helix { .. }) {
                return false;
            }
            self.start = p;
            if let Some(Seg::Nurbs(cu)) = self.segs.first_mut() {
                *cu = with_ctrl(cu, 0, p);
            }
            if closed {
                set_end(&mut self.segs[n - 1], p);
            }
            return true;
        }
        set_end(&mut self.segs[i - 1], p);
        if let Some(Seg::Nurbs(cu)) = self.segs.get_mut(i) {
            *cu = with_ctrl(cu, 0, p);
        }
        if closed && i == n {
            self.start = p;
            if let Some(Seg::Nurbs(cu)) = self.segs.first_mut() {
                *cu = with_ctrl(cu, 0, p);
            }
        }
        true
    }
}

fn with_ctrl(cu: &Curve, k: usize, p: V3) -> Curve {
    let mut ctrl = cu.ctrl().to_vec();
    if let Some(q) = ctrl.get_mut(k) {
        *q = p;
    }
    Curve::new(
        cu.degree(),
        cu.knots().to_vec(),
        ctrl,
        cu.weights().to_vec(),
    )
    .unwrap_or_else(|_| cu.clone())
}

fn set_end(s: &mut Seg, p: V3) {
    match s {
        Seg::Line { to } | Seg::Arc { to, .. } | Seg::Spline { to, .. } => *to = p,
        Seg::Nurbs(cu) => {
            let k = cu.ctrl().len() - 1;
            *cu = with_ctrl(cu, k, p);
        }
        Seg::Helix { .. } => {}
    }
}

// ---------- stations and frames ----------

/// Section station on the path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Station {
    pub p: V3,
    /// Tangent the profile axes are normal to (incoming side; outgoing at the first station).
    pub t: V3,
    /// Normal of the section plane: the tangent, or the bisector at a kink.
    pub n: V3,
    /// Profile axes with roll and twist applied.
    pub u: V3,
    pub v: V3,
    pub scale: f64,
    /// Arc length from the path start.
    pub s: f64,
    /// Largest curvature at the station (1/m) and its principal normal.
    pub k: f64,
    pub nrm: V3,
    pub kink: bool,
}

impl Station {
    /// Profile point (u, v) on the section plane of the station.
    pub fn place(&self, q: V2) -> V3 {
        let w = (self.u * q.x + self.v * q.y) * self.scale;
        let d = -w.dot(self.n) / self.t.dot(self.n);
        self.p + w + self.t * d
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stations {
    pub st: Vec<Station>,
    pub closed: bool,
    pub length: f64,
}

#[derive(Clone, Copy)]
struct Raw {
    p: V3,
    tin: V3,
    tout: V3,
    k: f64,
    nrm: V3,
    s: f64,
}

fn curvature(d1: V3, d2: V3) -> (f64, V3) {
    let sp = d1.len();
    let c = d1.cross(d2);
    let k = c.len() / (sp * sp * sp);
    if !(k > 1e-12) {
        return (0.0, ZERO);
    }
    let t = d1 * (1.0 / sp);
    let n = d2 - t * d2.dot(t);
    (k, n.normalized())
}

/// Minimal rotation taking unit `a` to unit `b`, applied to `v`.
pub fn transport(v: V3, a: V3, b: V3) -> V3 {
    let k = a.cross(b);
    let s = k.len();
    let c = a.dot(b);
    if s < 1e-15 {
        return v;
    }
    let k = k * (1.0 / s);
    v * c + k.cross(v) * s + k * (k.dot(v) * (1.0 - c))
}

/// Double reflection (rotation-minimizing) step of the reference vector `r0` from (x0, t0)
/// to (x1, t1).
fn reflect(x0: V3, t0: V3, r0: V3, x1: V3, t1: V3) -> V3 {
    let v1 = x1 - x0;
    let c1 = v1.dot(v1);
    let r1 = if c1 < 1e-24 {
        transport(r0, t0, t1)
    } else {
        let rl = r0 - v1 * (2.0 / c1 * v1.dot(r0));
        let tl = t0 - v1 * (2.0 / c1 * v1.dot(t0));
        let v2 = t1 - tl;
        let c2 = v2.dot(v2);
        if c2 < 1e-24 {
            rl
        } else {
            rl - v2 * (2.0 / c2 * v2.dot(rl))
        }
    };
    (r1 - t1 * r1.dot(t1)).normalized()
}

fn initial_u(t: V3) -> V3 {
    let u = v3(0.0, 0.0, 1.0).cross(t);
    if u.len() < 1e-9 {
        v3(1.0, 0.0, 0.0)
    } else {
        u.normalized()
    }
}

fn fixed_u(d: V3, t: V3) -> Option<V3> {
    let c = d.cross(t);
    (c.len() > 1e-6 * d.len().max(1e-300)).then(|| c.normalized())
}

fn is_kink(r: &Raw) -> bool {
    angle_between(r.tin, r.tout) > KINK
}

/// Signed angle from `a` to `b` about unit `t`.
fn signed_angle(a: V3, b: V3, t: V3) -> f64 {
    a.cross(b).dot(t).atan2(a.dot(b))
}

/// Section stations with frames, sampled within `chord` (m) and `angle` (rad).
pub fn stations(sm: &Smooth, chord: f64, angle: f64) -> Result<Stations, SweepError> {
    if !(sm.roll.is_finite() && sm.twist.finite() && sm.scale.finite()) {
        return path_err("non-finite roll, twist or scale");
    }
    let (s0, s1) = sm.scale.ends();
    if !(s0 > 1e-9 && s1 > 1e-9) {
        return path_err("scale must stay positive");
    }
    if let FrameMode::Fixed(d) = sm.frame {
        if !(fin(d) && d.len() > 1e-12) {
            return path_err("fixed reference direction is degenerate");
        }
    }
    let geos = sm.geos()?;
    let mut raw: Vec<Raw> = vec![];
    let mut base_s = 0.0;
    for (gi, g) in geos.iter().enumerate() {
        let ts = g.params(chord, angle);
        let len = g.length();
        let mut s_prev = base_s;
        for (j, &t) in ts.iter().enumerate() {
            let [p, d1, d2] = g.at(t);
            let sp = d1.len();
            if !(sp > 1e-12) {
                return path_err(format!("segment {}: zero tangent", gi + 1));
            }
            let tan = d1 * (1.0 / sp);
            let (k, nrm) = curvature(d1, d2);
            let s = match g {
                Geo::Nurbs { .. } if j > 0 => s_prev + g.len_between(ts[j - 1], t),
                Geo::Nurbs { .. } => base_s,
                _ => base_s + len * t,
            };
            s_prev = s;
            if j == 0 && gi > 0 {
                let last = raw.last_mut().expect("previous segment");
                last.tout = tan;
                if k > last.k {
                    last.k = k;
                    last.nrm = nrm;
                }
                continue;
            }
            raw.push(Raw {
                p,
                tin: tan,
                tout: tan,
                k,
                nrm,
                s,
            });
        }
        base_s += len;
    }
    let length = base_s;
    let closed = raw.len() > 2 && (raw[raw.len() - 1].p - raw[0].p).len() < 1e-6;
    if closed {
        let last = raw.pop().expect("closed path");
        raw[0].tin = last.tin;
        if last.k > raw[0].k {
            raw[0].k = last.k;
            raw[0].nrm = last.nrm;
        }
        if raw.len() < 3 {
            return path_err("closed path needs at least 3 distinct stations");
        }
        if !(sm.twist.is_const() && sm.scale.is_const()) {
            return path_err("a closed path takes no twist or scale law (it has no end)");
        }
    }
    for r in &raw {
        if r.tin.dot(r.tout) < -1.0 + 1e-9 {
            return path_err(format!("path reverses at s = {:.4} m", r.s));
        }
    }
    let n = raw.len();
    // base profile axis u on the incoming side of every station
    let fixed = |t: V3| match sm.frame {
        FrameMode::Fixed(d) => fixed_u(d, t),
        FrameMode::Rmf => None,
    };
    let t0 = raw[0].tout;
    let u0 = fixed(t0).unwrap_or_else(|| initial_u(t0));
    let mut us = vec![u0];
    let mut uo = u0;
    let step = |i0: usize, i1: usize, uo: V3| -> (V3, V3) {
        let (a, b) = (&raw[i0], &raw[i1]);
        let ui = reflect(a.p, a.tout, uo, b.p, b.tin);
        let ui = fixed(b.tin).unwrap_or(ui);
        let next = if is_kink(b) {
            fixed(b.tout).unwrap_or_else(|| transport(ui, b.tin, b.tout))
        } else {
            ui
        };
        (ui, next)
    };
    for i in 1..n {
        let (ui, next) = step(i - 1, i, uo);
        us.push(ui);
        uo = next;
    }
    // a closed RMF comes back turned (holonomy): spread the mismatch over the length
    let mut drift = 0.0;
    if closed {
        let (_, back) = step(n - 1, 0, uo);
        drift = signed_angle(u0, back, t0);
    }
    let st = raw
        .iter()
        .zip(&us)
        .enumerate()
        .map(|(i, (r, &u))| {
            let tau = if length > 0.0 { r.s / length } else { 0.0 };
            let t = if i == 0 { r.tout } else { r.tin };
            let v = t.cross(u);
            let phi = sm.roll + sm.twist.at(tau) - drift * tau;
            let (sn, cs) = phi.sin_cos();
            let kink = is_kink(r);
            Station {
                p: r.p,
                t,
                n: if kink {
                    (r.tin + r.tout).normalized()
                } else {
                    t
                },
                u: u * cs + v * sn,
                v: v * cs - u * sn,
                scale: sm.scale.at(tau),
                s: r.s,
                k: r.k,
                nrm: r.nrm,
                kink,
            }
        })
        .collect();
    Ok(Stations { st, closed, length })
}

// ---------- checks ----------

fn seg_seg(a: V3, b: V3, c: V3, d: V3) -> f64 {
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

/// Refuses a body that intersects itself for the profile points `probe`: curvature radius
/// below the profile extent, a fold at a kink, or two parts of the path closer than twice the
/// profile radius (coils of a helix, a loop crossing itself). `chord` is the sampling sag.
pub fn check(sts: &Stations, probe: &[V2], chord: f64) -> Result<(), SweepError> {
    let st = &sts.st;
    let n = st.len();
    for (i, s) in st.iter().enumerate() {
        if s.k > 1e-12 {
            let m = probe
                .iter()
                .map(|q| s.nrm.dot(s.u * q.x + s.v * q.y) * s.scale)
                .fold(0.0, f64::max);
            if m * s.k >= 1.0 - 1e-9 {
                return Err(SweepError::SelfIntersect(format!(
                    "curvature radius {:.4} m is smaller than the profile extent {:.4} m at station {i} (s = {:.4} m)",
                    1.0 / s.k,
                    m,
                    s.s
                )));
            }
        }
    }
    let bands = if sts.closed { n } else { n - 1 };
    for k in 0..bands {
        let (a, b) = (&st[k], &st[(k + 1) % n]);
        let d = b.p - a.p;
        for q in probe {
            if (b.place(*q) - a.place(*q)).dot(d) <= 1e-12 {
                return Err(SweepError::SelfIntersect(format!(
                    "profile folds at station {} (s = {:.4} m)",
                    (k + 1) % n,
                    b.s
                )));
            }
        }
    }
    let smax = st.iter().map(|s| s.scale).fold(0.0, f64::max);
    let rho = probe.iter().map(|q| q.len()).fold(0.0, f64::max) * smax;
    if !(rho > 0.0) || bands < 3 {
        return Ok(());
    }
    let lim = 2.0 * rho - 2.0 * chord;
    let skip = PI * rho;
    let total = sts.length;
    let segs: Vec<(V3, V3, f64, f64)> = (0..bands)
        .map(|k| {
            let (a, b) = (&st[k], &st[(k + 1) % n]);
            let s1 = if k + 1 == n { total } else { b.s };
            (a.p, b.p, a.s, s1)
        })
        .collect();
    let maxlen = segs.iter().map(|g| (g.1 - g.0).len()).fold(0.0, f64::max);
    let cell = (lim.max(0.0) + maxlen).max(1e-9);
    let key = |g: &(V3, V3, f64, f64)| {
        let m = (g.0 + g.1) * 0.5;
        (
            (m.x / cell).floor() as i64,
            (m.y / cell).floor() as i64,
            (m.z / cell).floor() as i64,
        )
    };
    let mut grid: std::collections::HashMap<(i64, i64, i64), Vec<usize>> = Default::default();
    for (i, g) in segs.iter().enumerate() {
        grid.entry(key(g)).or_default().push(i);
    }
    for (i, g) in segs.iter().enumerate() {
        let (cx, cy, cz) = key(g);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(list) = grid.get(&(cx + dx, cy + dy, cz + dz)) else {
                        continue;
                    };
                    for &j in list {
                        if j <= i {
                            continue;
                        }
                        let h = &segs[j];
                        let mut gap = h.2 - g.3;
                        if sts.closed {
                            gap = gap.min(total - h.3 + g.2);
                        }
                        if gap < skip {
                            continue;
                        }
                        if seg_seg(g.0, g.1, h.0, h.1) < lim {
                            return Err(SweepError::SelfIntersect(format!(
                                "the body touches itself between s = {:.4} m and s = {:.4} m",
                                g.2, h.2
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// ∫₀¹ law(τ)² dτ (exact for both laws: polynomials of degree ≤ 6).
pub fn mean_square(l: Law) -> f64 {
    gauss(0.0, 1.0, |t| l.at(t).powi(2))
}

// ---------- grips ----------

/// Grip of a smooth path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GripKey {
    /// 0 — start, k — end of segment k.
    Vertex(usize),
    /// Arc via point, spline via point, inner NURBS control point: (segment, index).
    Via(usize, usize),
    /// Helix rise of segment i.
    Rise(usize),
    /// Helix turn of segment i.
    Turn(usize),
}

impl GripKey {
    pub fn code(self) -> u32 {
        let c = |i: usize| (i as u32) & 0x3ff;
        match self {
            GripKey::Vertex(i) => c(i),
            GripKey::Via(i, j) => (1 << 20) | (c(i) << 10) | c(j),
            GripKey::Rise(i) => (2 << 20) | c(i),
            GripKey::Turn(i) => (3 << 20) | c(i),
        }
    }
    pub fn from_code(x: u32) -> Option<GripKey> {
        let (hi, i, j) = (x >> 20, ((x >> 10) & 0x3ff) as usize, (x & 0x3ff) as usize);
        match hi {
            0 if x < 0x400 => Some(GripKey::Vertex(j)),
            1 => Some(GripKey::Via(i, j)),
            2 => Some(GripKey::Rise(j)),
            3 => Some(GripKey::Turn(j)),
            _ => None,
        }
    }
}

/// How a grip moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GripMove {
    /// Free point (dragged in a plane, lifted along its normal).
    Point,
    /// Along a unit direction from the anchor; value — metres.
    Line(V3),
    /// About an axis through `center`; value — angle from `zero`.
    Ring { center: V3, axis: V3, zero: V3 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathGrip {
    pub key: GripKey,
    pub pos: V3,
    pub anchor: V3,
    pub mv: GripMove,
}

/// Grips of the path in its own coordinates.
pub fn grips(sm: &Smooth) -> Vec<PathGrip> {
    let Ok(geos) = sm.geos() else {
        return vec![];
    };
    let closed = sm.closed();
    let mut out = vec![];
    let point = |key, pos| PathGrip {
        key,
        pos,
        anchor: pos,
        mv: GripMove::Point,
    };
    let mut start = sm.start;
    let n = sm.segs.len();
    if !(closed && matches!(sm.segs[n - 1], Seg::Helix { .. })) {
        out.push(point(GripKey::Vertex(0), start));
    }
    for (i, (s, g)) in sm.segs.iter().zip(&geos).enumerate() {
        let end = g.at(1.0)[0];
        match s {
            Seg::Arc { via, .. } => out.push(point(GripKey::Via(i, 0), *via)),
            Seg::Spline { via, .. } => {
                for (j, p) in via.iter().enumerate() {
                    out.push(point(GripKey::Via(i, j), *p));
                }
            }
            Seg::Nurbs(cu) => {
                let c = cu.ctrl();
                for (j, p) in c.iter().enumerate().take(c.len() - 1).skip(1) {
                    out.push(point(GripKey::Via(i, j), *p));
                }
            }
            Seg::Helix { .. } => {
                if let Geo::Circ { c, x, ax, rise, .. } = g {
                    out.push(PathGrip {
                        key: GripKey::Rise(i),
                        pos: end,
                        anchor: start,
                        mv: GripMove::Line(*ax),
                    });
                    let center = *c + *ax * *rise;
                    out.push(PathGrip {
                        key: GripKey::Turn(i),
                        pos: end,
                        anchor: center,
                        mv: GripMove::Ring {
                            center,
                            axis: *ax,
                            zero: *x,
                        },
                    });
                }
            }
            Seg::Line { .. } => {}
        }
        let free = !matches!(s, Seg::Helix { .. }) && !(closed && i + 1 == n);
        if free {
            out.push(point(GripKey::Vertex(i + 1), end));
        }
        start = end;
    }
    out
}

/// Path after dropping grip `key` at `to` (path coordinates); None — not applicable or the
/// result is not a valid path.
pub fn grip_apply(sm: &Smooth, key: GripKey, to: V3) -> Option<Smooth> {
    if !fin(to) {
        return None;
    }
    let mut s = sm.clone();
    let geos = sm.geos().ok()?;
    match key {
        GripKey::Vertex(i) => {
            if !s.set_vertex(i, to) {
                return None;
            }
        }
        GripKey::Via(i, j) => match s.segs.get_mut(i)? {
            Seg::Arc { via, .. } if j == 0 => *via = to,
            Seg::Spline { via, .. } => *via.get_mut(j)? = to,
            Seg::Nurbs(cu) if j > 0 && j + 1 < cu.ctrl().len() => *cu = with_ctrl(cu, j, to),
            _ => return None,
        },
        GripKey::Rise(i) => {
            let start = if i == 0 {
                sm.start
            } else {
                geos.get(i - 1)?.at(1.0)[0]
            };
            let Geo::Circ { ax, .. } = geos.get(i)? else {
                return None;
            };
            let Seg::Helix { rise, .. } = s.segs.get_mut(i)? else {
                return None;
            };
            *rise = (to - start).dot(*ax);
        }
        GripKey::Turn(i) => {
            let Geo::Circ { c, x, y, ax, .. } = geos.get(i)? else {
                return None;
            };
            let Seg::Helix { angle, .. } = s.segs.get_mut(i)? else {
                return None;
            };
            let w = to - *c;
            let w = w - *ax * w.dot(*ax);
            if w.len() < 1e-9 {
                return None;
            }
            let mut phi = w.dot(*y).atan2(w.dot(*x));
            if angle.is_sign_negative() {
                phi = -phi;
            }
            if phi < 1e-3 {
                phi += TAU;
            }
            let whole = ((angle.abs() - 1e-9) / TAU).floor().max(0.0);
            *angle = angle.signum() * (whole * TAU + phi);
        }
    }
    s.geos().ok()?;
    Some(s)
}

#[cfg(test)]
#[path = "frame3_tests.rs"]
mod tests;
