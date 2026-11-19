//! Rebar: bar shapes (shape code + dimensions A, B, C), bends and standard hooks, bend radius
//! by diameter, axis length with hooks, mass per meter; stirrup layouts along a path
//! (count, maximum/exact spacing, denser spacing at the ends), mesh bars over a contour with cover,
//! clipping to the host envelope; the bar body is a tube along the axis (face count = level of detail).
//!
//! Shape vertices lie on the bar axis at the intersections of adjacent segment axes; at a hooked
//! end it is the extreme axis point along the segment (outer face of the hook at the cover). Cut
//! length is computed along the axis; dimensions A, B, C... of the bending schedule use outer
//! faces ([`outer_dims`]: axial segment + d/2 at each bend or hook, stirrup +d), as is usual
//! in standard bending schedules.

use std::f64::consts::PI;

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;

/// Steel density, kg/m³.
pub const STEEL_DENSITY: f64 = 7850.0;
/// Shorter than this, no mesh bar is placed.
pub const MIN_BAR: f64 = 0.05;

/// Mass per meter of a round bar, kg/m.
pub fn unit_mass(d: f64) -> f64 {
    STEEL_DENSITY * PI * d * d / 4.0
}

/// Mandrel diameter in bar diameters by rule: up to 16 mm 4d, above that 7d.
pub fn mandrel_rule(d: f64) -> f64 {
    if d <= 0.016 + 1e-9 {
        4.0
    } else {
        7.0
    }
}

/// Bend radius at the axis: half the mandrel plus half the bar; `mandrel` <= 0 uses the rule.
pub fn bend_radius(d: f64, mandrel: f64) -> f64 {
    let k = if mandrel > 0.0 {
        mandrel
    } else {
        mandrel_rule(d)
    };
    (k * d + d) / 2.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Hook {
    #[default]
    None,
    H90,
    H135,
    H180,
}

impl Hook {
    pub fn from_deg(a: i64) -> Option<Hook> {
        Some(match a {
            0 => Hook::None,
            90 => Hook::H90,
            135 => Hook::H135,
            180 => Hook::H180,
            _ => return None,
        })
    }

    pub fn deg(self) -> i64 {
        match self {
            Hook::None => 0,
            Hook::H90 => 90,
            Hook::H135 => 135,
            Hook::H180 => 180,
        }
    }

    pub fn angle(self) -> f64 {
        self.deg() as f64 * PI / 180.0
    }

    /// Straight tail after the bend: 90° is 12d, 135° is 6d (at least 75 mm), 180° is 4d (at least
    /// 65 mm).
    pub fn ext(self, d: f64) -> f64 {
        match self {
            Hook::None => 0.0,
            Hook::H90 => 12.0 * d,
            Hook::H135 => (6.0 * d).max(0.075),
            Hook::H180 => (4.0 * d).max(0.065),
        }
    }
}

/// Shape code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ShapeCode {
    /// 00: straight, A.
    #[default]
    Straight,
    /// 11: L-shaped, A with leg B.
    L,
    /// 21: U-shaped, legs B and C, back A.
    U,
    /// 51: closed stirrup A x B with hooks in one corner.
    Stirrup,
}

impl ShapeCode {
    pub const ALL: [ShapeCode; 4] = [
        ShapeCode::Straight,
        ShapeCode::L,
        ShapeCode::U,
        ShapeCode::Stirrup,
    ];

    pub fn code(self) -> &'static str {
        match self {
            ShapeCode::Straight => "00",
            ShapeCode::L => "11",
            ShapeCode::U => "21",
            ShapeCode::Stirrup => "51",
        }
    }

    pub fn parse(s: &str) -> Option<ShapeCode> {
        ShapeCode::ALL.into_iter().find(|c| c.code() == s)
    }

    /// Shape vertices from dimensions: origin at (0, 0) (stirrup: top-left corner), A along u,
    /// legs toward +v.
    pub fn verts(self, a: f64, b: f64, c: f64) -> Vec<V2> {
        match self {
            ShapeCode::Straight => vec![v2(0.0, 0.0), v2(a, 0.0)],
            ShapeCode::L => vec![v2(0.0, 0.0), v2(a, 0.0), v2(a, b)],
            ShapeCode::U => vec![v2(0.0, b), v2(0.0, 0.0), v2(a, 0.0), v2(a, c)],
            ShapeCode::Stirrup => vec![v2(0.0, b), v2(a, b), v2(a, 0.0), v2(0.0, 0.0), v2(0.0, b)],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RebarError {
    /// Segment shorter than the bends at its ends.
    TooShort,
    /// Zero-length segment or reversal greater than 179°.
    BadShape,
}

impl std::fmt::Display for RebarError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            RebarError::TooShort => "leg shorter than its bends",
            RebarError::BadShape => "degenerate shape",
        })
    }
}

/// Piece of the axis in the shape plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim2 {
    Line(V2, V2),
    /// Arc: center, radius, start angle, signed sweep angle (+ is counter-clockwise).
    Arc {
        c: V2,
        r: f64,
        a0: f64,
        sweep: f64,
    },
}

fn rot(p: V2, a: f64) -> V2 {
    let (s, c) = a.sin_cos();
    v2(p.x * c - p.y * s, p.x * s + p.y * c)
}

fn left(d: V2) -> V2 {
    v2(-d.y, d.x)
}

fn unit2(d: V2) -> Option<V2> {
    let l = d.len();
    (l > 1e-12).then(|| d * (1.0 / l))
}

impl Prim2 {
    pub fn len(&self) -> f64 {
        match *self {
            Prim2::Line(a, b) => (b - a).len(),
            Prim2::Arc { r, sweep, .. } => r * sweep.abs(),
        }
    }

    pub fn start(&self) -> V2 {
        match *self {
            Prim2::Line(a, _) => a,
            Prim2::Arc { c, r, a0, .. } => c + v2(a0.cos(), a0.sin()) * r,
        }
    }

    pub fn end(&self) -> V2 {
        match *self {
            Prim2::Line(_, b) => b,
            Prim2::Arc { c, r, a0, sweep } => c + v2((a0 + sweep).cos(), (a0 + sweep).sin()) * r,
        }
    }

    pub fn reversed(&self) -> Prim2 {
        match *self {
            Prim2::Line(a, b) => Prim2::Line(b, a),
            Prim2::Arc { c, r, a0, sweep } => Prim2::Arc {
                c,
                r,
                a0: a0 + sweep,
                sweep: -sweep,
            },
        }
    }

    /// Points excluding the start point; chord deviation from the arc is at most `sag`.
    fn push_points(&self, sag: f64, out: &mut Vec<V2>) {
        match *self {
            Prim2::Line(_, b) => out.push(b),
            Prim2::Arc { c, r, a0, sweep } => {
                let step = if sag > 0.0 && sag < r {
                    2.0 * (1.0 - sag / r).acos()
                } else {
                    PI / 4.0
                };
                let n = ((sweep.abs() / step.max(1e-3)).ceil() as usize).clamp(1, 64);
                for k in 1..=n {
                    let a = a0 + sweep * k as f64 / n as f64;
                    out.push(c + v2(a.cos(), a.sin()) * r);
                }
            }
        }
    }
}

/// Hooked bend at a free end: from point `v` in direction `d`, turning toward `side`.
/// Returns (arc, straight segment) from the bend start point.
fn hook_prims(v: V2, d: V2, side: V2, hook: Hook, rc: f64, dia: f64) -> [Prim2; 2] {
    let sign = if d.cross(side) < 0.0 { -1.0 } else { 1.0 };
    let theta = hook.angle() * sign;
    let p = v - d * rc;
    let c = p + left(d) * (sign * rc);
    let a0 = (p.y - c.y).atan2(p.x - c.x);
    let e = c + rot(p - c, theta);
    let d2 = rot(d, theta);
    [
        Prim2::Arc {
            c,
            r: rc,
            a0,
            sweep: theta,
        },
        Prim2::Line(e, e + d2 * hook.ext(dia)),
    ]
}

/// Bar axis from shape vertices: all interior vertices are rounded with radius `rc`, hooks at
/// the ends turn toward points `side_start`/`side_end` (directions in the shape plane).
pub fn centerline(
    verts: &[V2],
    rc: f64,
    dia: f64,
    hooks: (Hook, Hook),
    side_start: V2,
    side_end: V2,
) -> Result<Vec<Prim2>, RebarError> {
    let n = verts.len();
    if n < 2 {
        return Err(RebarError::BadShape);
    }
    let mut dirs = Vec::with_capacity(n - 1);
    let mut lens = Vec::with_capacity(n - 1);
    for w in verts.windows(2) {
        let l = (w[1] - w[0]).len();
        dirs.push(unit2(w[1] - w[0]).ok_or(RebarError::BadShape)?);
        lens.push(l);
    }
    // bend tangent lengths at interior vertices
    let mut tang = vec![0.0; n];
    let mut turn = vec![0.0; n];
    for k in 1..n - 1 {
        let (a, b) = (dirs[k - 1], dirs[k]);
        let psi = a.cross(b).atan2(a.dot(b));
        if psi.abs() > 179.0f64.to_radians() {
            return Err(RebarError::BadShape);
        }
        turn[k] = psi;
        tang[k] = rc * (psi.abs() / 2.0).tan();
    }
    if hooks.0 != Hook::None {
        tang[0] = rc;
    }
    if hooks.1 != Hook::None {
        tang[n - 1] = rc;
    }
    let mut out = vec![];
    if hooks.0 != Hook::None {
        let h = hook_prims(verts[0], -dirs[0], side_start, hooks.0, rc, dia);
        out.push(h[1].reversed());
        out.push(h[0].reversed());
    }
    for i in 0..n - 1 {
        let (ta, tb) = (tang[i], tang[i + 1]);
        if ta + tb > lens[i] + 1e-9 {
            return Err(RebarError::TooShort);
        }
        let a = verts[i] + dirs[i] * ta;
        let b = verts[i + 1] - dirs[i] * tb;
        if (b - a).len() > 1e-12 {
            out.push(Prim2::Line(a, b));
        }
        let k = i + 1;
        if k < n - 1 && turn[k].abs() > 1e-12 {
            let p = b;
            let sign = turn[k].signum();
            let c = p + left(dirs[i]) * (sign * rc);
            out.push(Prim2::Arc {
                c,
                r: rc,
                a0: (p.y - c.y).atan2(p.x - c.x),
                sweep: turn[k],
            });
        }
    }
    if hooks.1 != Hook::None {
        let h = hook_prims(verts[n - 1], dirs[n - 2], side_end, hooks.1, rc, dia);
        out.extend(h);
    }
    Ok(out)
}

/// Bar in space: shape plane (origin, axes u, v) and the axis within it.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub d: f64,
    pub o: V3,
    pub u: V3,
    pub v: V3,
    pub path: Vec<Prim2>,
}

impl Bar {
    /// Straight bar a→b.
    pub fn straight(a: V3, b: V3, d: f64) -> Bar {
        let l = (b - a).len();
        let u = if l > 1e-12 {
            (b - a) * (1.0 / l)
        } else {
            v3(1.0, 0.0, 0.0)
        };
        Bar {
            d,
            o: a,
            u,
            v: any_perp(u),
            path: vec![Prim2::Line(v2(0.0, 0.0), v2(l, 0.0))],
        }
    }

    pub fn length(&self) -> f64 {
        self.path.iter().map(|p| p.len()).sum()
    }

    pub fn at(&self, p: V2) -> V3 {
        self.o + self.u * p.x + self.v * p.y
    }

    /// Axis points (arcs as chords with deviation at most `sag`).
    pub fn points(&self, sag: f64) -> Vec<V3> {
        let Some(first) = self.path.first() else {
            return vec![];
        };
        let mut q = vec![first.start()];
        for p in &self.path {
            p.push_points(sag, &mut q);
        }
        q.into_iter().map(|p| self.at(p)).collect()
    }

    /// First and last axis points.
    pub fn ends(&self) -> Option<(V3, V3)> {
        Some((
            self.at(self.path.first()?.start()),
            self.at(self.path.last()?.end()),
        ))
    }

    /// Body: tube along the axis (`sides` faces), bends with deviation d/4.
    pub fn mesh(&self, sides: usize) -> Mesh {
        tube(&self.points(self.d / 4.0), self.d / 2.0, sides)
    }

    /// Maps the shape plane into another system: `f` maps points, `g` maps vectors.
    pub fn mapped(&self, f: impl Fn(V3) -> V3, g: impl Fn(V3) -> V3) -> Bar {
        Bar {
            d: self.d,
            o: f(self.o),
            u: g(self.u),
            v: g(self.v),
            path: self.path.clone(),
        }
    }
}

/// Unit vector perpendicular to `u`.
pub fn any_perp(u: V3) -> V3 {
    let t = if u.z.abs() < 0.9 {
        v3(0.0, 0.0, 1.0)
    } else {
        v3(1.0, 0.0, 0.0)
    };
    let p = t - u * t.dot(u);
    p.normalized()
}

/// Tube face count by the number of bars in the scene: near hundreds, round tubes; at tens of
/// thousands, triangular "lines".
pub fn lod_sides(bars: usize) -> usize {
    if bars <= 2_000 {
        8
    } else if bars <= 20_000 {
        4
    } else {
        3
    }
}

/// Closed tube of radius `r` along a polyline (rings by parallel transport, end caps as fans).
/// Faces: 0 side, 1 start, 2 end.
pub fn tube(pts: &[V3], r: f64, sides: usize) -> Mesh {
    let mut p: Vec<V3> = Vec::with_capacity(pts.len());
    for q in pts {
        if p.last().is_none_or(|l: &V3| (*q - *l).len() > 1e-9) {
            p.push(*q);
        }
    }
    let mut m = Mesh::default();
    let k = sides.max(3);
    if p.len() < 2 || r <= 0.0 {
        return m;
    }
    let n = p.len();
    let seg: Vec<V3> = p.windows(2).map(|w| (w[1] - w[0]).normalized()).collect();
    let tan: Vec<V3> = (0..n)
        .map(|i| {
            if i == 0 {
                seg[0]
            } else if i == n - 1 {
                seg[n - 2]
            } else {
                let s = seg[i - 1] + seg[i];
                if s.len() < 1e-9 {
                    seg[i]
                } else {
                    s.normalized()
                }
            }
        })
        .collect();
    let mut nrm = any_perp(tan[0]);
    for i in 0..n {
        let t = tan[i];
        let q = nrm - t * nrm.dot(t);
        nrm = if q.len() < 1e-9 {
            any_perp(t)
        } else {
            q.normalized()
        };
        let b = t.cross(nrm);
        // preserve diameter at a kink: the ring on the bisector is stretched by 1/cos
        let stretch = if i > 0 && i < n - 1 {
            1.0 / seg[i - 1].dot(t).clamp(0.5, 1.0)
        } else {
            1.0
        };
        for j in 0..k {
            let a = 2.0 * PI * j as f64 / k as f64;
            let off = nrm * (a.cos() * r) + b * (a.sin() * r);
            // stretch only along the kink direction (component in the plane of the turn)
            let along = if stretch > 1.0 {
                let bend = seg[i] - seg[i - 1];
                let bn = bend - t * bend.dot(t);
                if bn.len() > 1e-12 {
                    let bn = bn.normalized();
                    off + bn * (off.dot(bn) * (stretch - 1.0))
                } else {
                    off
                }
            } else {
                off
            };
            m.positions.push(p[i] + along);
        }
    }
    let at = |i: usize, j: usize| (i * k + j % k) as u32;
    for i in 0..n - 1 {
        for j in 0..k {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            m.triangles.push([a, c, b]);
            m.triangles.push([a, d, c]);
            m.face_of.extend([0, 0]);
        }
    }
    let c0 = m.positions.len() as u32;
    m.positions.push(p[0]);
    let c1 = c0 + 1;
    m.positions.push(p[n - 1]);
    for j in 0..k {
        m.triangles.push([c0, at(0, j + 1), at(0, j)]);
        m.face_of.push(1);
        m.triangles.push([c1, at(n - 1, j), at(n - 1, j + 1)]);
        m.face_of.push(2);
    }
    m
}

/// Layout along a path (stirrups along a beam or column).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathLayout {
    /// Given count, evenly spread from edge to edge.
    Fixed(usize),
    /// Spacing at most the given value, evenly distributed.
    MaxSpacing(f64),
    /// Exact spacing; the remainder is split equally at the ends.
    Spacing(f64),
    /// Denser spacing at the ends: zones of length `end` with spacing `end_s`, between them spacing at most `mid_s`.
    Zones { end: f64, end_s: f64, mid_s: f64 },
}

impl PathLayout {
    pub fn rule(self) -> &'static str {
        match self {
            PathLayout::Fixed(_) => "fixed",
            PathLayout::MaxSpacing(_) => "max",
            PathLayout::Spacing(_) => "spacing",
            PathLayout::Zones { .. } => "zones",
        }
    }

    pub fn valid(self) -> bool {
        let pos = |x: f64| x.is_finite() && x >= 0.005;
        match self {
            PathLayout::Fixed(n) => n <= 10_000,
            PathLayout::MaxSpacing(s) | PathLayout::Spacing(s) => pos(s),
            PathLayout::Zones { end, end_s, mid_s } => {
                end.is_finite() && end >= 0.0 && pos(end_s) && pos(mid_s)
            }
        }
    }
}

fn even(s0: f64, s1: f64, n: usize) -> Vec<f64> {
    match n {
        0 => vec![],
        1 => vec![(s0 + s1) / 2.0],
        _ => (0..n)
            .map(|k| s0 + (s1 - s0) * k as f64 / (n - 1) as f64)
            .collect(),
    }
}

/// Positions along the path on the interval [s0, s1] (ascending).
pub fn positions(s0: f64, s1: f64, l: PathLayout) -> Vec<f64> {
    let span = s1 - s0;
    if !span.is_finite() || span < -1e-9 || !l.valid() {
        return vec![];
    }
    if span <= 1e-9 {
        return if matches!(l, PathLayout::Fixed(0)) {
            vec![]
        } else {
            vec![s0]
        };
    }
    match l {
        PathLayout::Fixed(n) => even(s0, s1, n),
        PathLayout::MaxSpacing(s) => even(s0, s1, (span / s - 1e-9).ceil() as usize + 1),
        PathLayout::Spacing(s) => {
            let n = (span / s + 1e-9).floor() as usize + 1;
            let start = s0 + (span - (n - 1) as f64 * s) / 2.0;
            (0..n).map(|k| start + k as f64 * s).collect()
        }
        PathLayout::Zones { end, end_s, mid_s } => {
            if 2.0 * end >= span {
                return positions(s0, s1, PathLayout::MaxSpacing(end_s));
            }
            let ne = (end / end_s + 1e-9).floor() as usize + 1;
            let left: Vec<f64> = (0..ne).map(|k| s0 + k as f64 * end_s).collect();
            let right: Vec<f64> = (0..ne).map(|k| s1 - k as f64 * end_s).collect();
            let (a, b) = (left[ne - 1], right[ne - 1]);
            let gap = b - a;
            let m = ((gap / mid_s - 1e-9).ceil() as usize).saturating_sub(1);
            let mut out = left;
            out.extend((1..=m).map(|j| a + gap * j as f64 / (m + 1) as f64));
            out.extend(right.into_iter().rev());
            out
        }
    }
}

/// Intersections of the line p0 + t·τ with the region rings: inside intervals (even-odd) and the
/// sines of the angles to the edges at their ends.
pub fn region_intervals(p0: V2, t: V2, rings: &[Vec<V2>]) -> Vec<(f64, f64, f64, f64)> {
    let nrm = left(t);
    let w0 = nrm.dot(p0);
    let mut hits: Vec<(f64, f64)> = vec![];
    for r in rings {
        let k = r.len();
        if k < 3 {
            continue;
        }
        for i in 0..k {
            let (a, b) = (r[i], r[(i + 1) % k]);
            let (wa, wb) = (nrm.dot(a) - w0, nrm.dot(b) - w0);
            if (wa > 0.0) == (wb > 0.0) {
                continue;
            }
            let q = a + (b - a) * (wa / (wa - wb));
            let e = unit2(b - a).unwrap_or(t);
            hits.push(((q - p0).dot(t), t.cross(e).abs()));
        }
    }
    hits.sort_by(|x, y| x.0.total_cmp(&y.0));
    hits.chunks_exact(2)
        .map(|c| (c[0].0, c[1].0, c[0].1, c[1].1))
        .collect()
}

/// Interval shrunk by the cover at its ends (slanted edge: c / sin, at most 3c).
fn shrink(iv: (f64, f64, f64, f64), cover: f64) -> (f64, f64) {
    let k = |s: f64| cover / s.max(1.0 / 3.0);
    (iv.0 + k(iv.2), iv.1 - k(iv.3))
}

/// Intersection of ordered non-overlapping interval lists.
fn intersect(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if hi > lo {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Bars of one mesh layer in a region (even-odd rings: piece contours and openings):
/// direction `angle`, spacing at most `spacing`, cover at the ends, to the outermost bars
/// (outermost axis at `cover + d/2`) and along opening edges: a bar running along an edge closer
/// than `cover + d/2` is cut by it (lines on both sides of the axis at that distance must be
/// inside, with cover at the ends).
pub fn area_lines(
    rings: &[Vec<V2>],
    angle: f64,
    spacing: f64,
    cover: f64,
    d: f64,
) -> Vec<(V2, V2)> {
    if !rings.iter().any(|r| r.len() >= 3) {
        return vec![];
    }
    let t = v2(angle.cos(), angle.sin());
    let nrm = left(t);
    let (m0, m1) = rings
        .iter()
        .filter(|r| r.len() >= 3)
        .flatten()
        .map(|p| nrm.dot(*p))
        .fold((f64::MAX, f64::MIN), |(a, b), w| (a.min(w), b.max(w)));
    let offs = positions(
        m0 + cover + d / 2.0,
        m1 - cover - d / 2.0,
        PathLayout::MaxSpacing(spacing),
    );
    // tolerance: the outermost bar touches the contour with its side line
    let side = (cover + d / 2.0 - 1e-6).max(0.0);
    let mut out = vec![];
    for w in offs {
        let p0 = nrm * w;
        let mut keep: Vec<(f64, f64)> = region_intervals(p0, t, rings)
            .into_iter()
            .map(|iv| shrink(iv, cover))
            .filter(|iv| iv.1 > iv.0)
            .collect();
        if side > 0.0 {
            for k in [-side, side] {
                let span: Vec<(f64, f64)> = region_intervals(nrm * (w + k), t, rings)
                    .into_iter()
                    .map(|iv| shrink(iv, cover))
                    .filter(|iv| iv.1 > iv.0)
                    .collect();
                keep = intersect(&keep, &span);
            }
        }
        for (a, b) in keep {
            if b - a >= MIN_BAR.max(2.0 * d) {
                out.push((p0 + t * a, p0 + t * b));
            }
        }
    }
    out
}

/// Point inside the region (even-odd) at least `cover` from the edges along the x and y axes.
pub fn inside_region(p: V2, rings: &[Vec<V2>], cover: f64) -> bool {
    [v2(1.0, 0.0), v2(0.0, 1.0)].into_iter().all(|t| {
        region_intervals(p, t, rings).into_iter().any(|iv| {
            let (a, b) = shrink(iv, cover);
            a <= 1e-12 && b >= -1e-12
        })
    })
}

/// Shape segment dimensions by outer faces: axial segment plus d/2 at each end with a bend
/// (interior vertex) or hook; a free straight end gets no addition.
pub fn outer_dims(verts: &[V2], hooks: (Hook, Hook), d: f64) -> Vec<f64> {
    let n = verts.len();
    if n < 2 {
        return vec![];
    }
    (0..n - 1)
        .map(|i| {
            let a = i > 0 || hooks.0 != Hook::None;
            let b = i + 2 < n || hooks.1 != Hook::None;
            (verts[i + 1] - verts[i]).len() + d / 2.0 * f64::from(u8::from(a) + u8::from(b))
        })
        .collect()
}

/// Segment inside the region (longest piece) with cover at the ends: fractions [ta, tb] from a to b.
pub fn clip_region(a: V2, b: V2, rings: &[Vec<V2>], cover: f64) -> Option<(f64, f64)> {
    let l = (b - a).len();
    let t = unit2(b - a)?;
    let mut best: Option<(f64, f64)> = None;
    for iv in region_intervals(a, t, rings) {
        let (s0, s1) = shrink(iv, cover);
        let (s0, s1) = (s0.max(0.0), s1.min(l));
        if s1 - s0 > 1e-9 && best.is_none_or(|bst| s1 - s0 > bst.1 - bst.0) {
            best = Some((s0, s1));
        }
    }
    best.map(|(s0, s1)| (s0 / l, s1 / l))
}

/// Segment inside the box [lo, hi] (Liang-Barsky); None if outside.
pub fn clip_box(a: V3, b: V3, lo: V3, hi: V3) -> Option<(V3, V3)> {
    let d = b - a;
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q, l, h) in [
        (a.x, d.x, lo.x, hi.x),
        (a.y, d.y, lo.y, hi.y),
        (a.z, d.z, lo.z, hi.z),
    ] {
        if l > h {
            return None;
        }
        if q.abs() < 1e-15 {
            if p < l - 1e-12 || p > h + 1e-12 {
                return None;
            }
            continue;
        }
        let (mut u0, mut u1) = ((l - p) / q, (h - p) / q);
        if u0 > u1 {
            std::mem::swap(&mut u0, &mut u1);
        }
        t0 = t0.max(u0);
        t1 = t1.min(u1);
        if t0 > t1 {
            return None;
        }
    }
    Some((a + d * t0, a + d * t1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    fn path_len(p: &[Prim2]) -> f64 {
        p.iter().map(|q| q.len()).sum()
    }

    fn continuous(p: &[Prim2]) {
        for w in p.windows(2) {
            assert!((w[0].end() - w[1].start()).len() < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn bend_radius_by_diameter_rule() {
        close(bend_radius(0.012, 0.0), (4.0 * 0.012 + 0.012) / 2.0);
        close(bend_radius(0.016, 0.0), (4.0 * 0.016 + 0.016) / 2.0);
        close(bend_radius(0.020, 0.0), (7.0 * 0.020 + 0.020) / 2.0);
        close(bend_radius(0.020, 5.0), (5.0 * 0.020 + 0.020) / 2.0);
        assert!((unit_mass(0.012) - 0.888).abs() < 1e-3);
        assert!((unit_mass(0.025) - 3.853).abs() < 1e-3);
    }

    #[test]
    fn straight_with_two_90_hooks_has_exact_length() {
        let d = 0.012;
        let rc = bend_radius(d, 0.0);
        let v = ShapeCode::Straight.verts(1.0, 0.0, 0.0);
        let up = v2(0.0, 1.0);
        let p = centerline(&v, rc, d, (Hook::H90, Hook::H90), up, up).unwrap();
        continuous(&p);
        let want = (1.0 - 2.0 * rc) + 2.0 * rc * PI / 2.0 + 2.0 * 12.0 * d;
        close(path_len(&p), want);
        // hooks turn to the same side; the outer extreme of the axis stays at the dimension
        let xs: Vec<f64> = p.iter().flat_map(|q| [q.start().x, q.end().x]).collect();
        let (lo, hi) = xs
            .iter()
            .fold((f64::MAX, f64::MIN), |a, x| (a.0.min(*x), a.1.max(*x)));
        close(lo, 0.0);
        close(hi, 1.0);
        assert!(p.first().unwrap().start().y > 0.1 && p.last().unwrap().end().y > 0.1);
    }

    #[test]
    fn hook_extensions_and_180_length() {
        let d = 0.01;
        close(Hook::H90.ext(d), 0.12);
        close(Hook::H135.ext(d), 0.075);
        close(Hook::H135.ext(0.02), 0.12);
        close(Hook::H180.ext(d), 0.065);
        let rc = bend_radius(d, 0.0);
        let v = ShapeCode::Straight.verts(0.8, 0.0, 0.0);
        let s = v2(0.0, -1.0);
        let p = centerline(&v, rc, d, (Hook::None, Hook::H180), s, s).unwrap();
        continuous(&p);
        close(path_len(&p), (0.8 - rc) + rc * PI + 0.065);
        // 180°: the tail runs back parallel, 2rc below
        let e = p.last().unwrap().end();
        close(e.y, -2.0 * rc);
        close(e.x, 0.8 - rc - 0.065);
    }

    #[test]
    fn stirrup_with_135_hooks_closes_in_one_corner() {
        let d = 0.008;
        let rc = bend_radius(d, 0.0);
        let (a, b) = (0.24, 0.44);
        let v = ShapeCode::Stirrup.verts(a, b, 0.0);
        let mid = v2(a / 2.0, b / 2.0) - v2(0.0, b);
        let p = centerline(&v, rc, d, (Hook::H135, Hook::H135), mid, mid).unwrap();
        continuous(&p);
        let ext = Hook::H135.ext(d);
        let want = 2.0 * a + 2.0 * b - 8.0 * rc
            + 3.0 * rc * PI / 2.0
            + 2.0 * rc * 3.0 * PI / 4.0
            + 2.0 * ext;
        close(path_len(&p), want);
        // both hook tails point into the core
        for q in [p.first().unwrap().start(), p.last().unwrap().end()] {
            assert!(q.x > 0.0 && q.x < a && q.y > 0.0 && q.y < b, "{q:?}");
        }
    }

    #[test]
    fn l_and_u_shapes_and_short_legs() {
        let d = 0.016;
        let rc = bend_radius(d, 0.0);
        let p = centerline(
            &ShapeCode::L.verts(2.0, 0.5, 0.0),
            rc,
            d,
            (Hook::None, Hook::None),
            v2(0.0, 1.0),
            v2(0.0, 1.0),
        )
        .unwrap();
        close(path_len(&p), 2.5 - 2.0 * rc + rc * PI / 2.0);
        let u = centerline(
            &ShapeCode::U.verts(1.0, 0.3, 0.4),
            rc,
            d,
            (Hook::None, Hook::None),
            v2(0.0, 1.0),
            v2(0.0, 1.0),
        )
        .unwrap();
        close(path_len(&u), 1.7 - 4.0 * rc + rc * PI);
        let bad = centerline(
            &ShapeCode::L.verts(1.0, 0.02, 0.0),
            rc,
            d,
            (Hook::None, Hook::None),
            v2(0.0, 1.0),
            v2(0.0, 1.0),
        );
        assert_eq!(bad, Err(RebarError::TooShort));
        assert_eq!(ShapeCode::parse("51"), Some(ShapeCode::Stirrup));
        assert_eq!(ShapeCode::parse("99"), None);
    }

    #[test]
    fn stirrup_counts_by_rule() {
        assert_eq!(positions(0.0, 6.0, PathLayout::Fixed(10)).len(), 10);
        assert_eq!(positions(0.0, 6.0, PathLayout::MaxSpacing(0.2)).len(), 31);
        assert_eq!(positions(0.0, 6.05, PathLayout::MaxSpacing(0.2)).len(), 32);
        let s = positions(0.0, 6.05, PathLayout::Spacing(0.2));
        assert_eq!(s.len(), 31);
        close(s[0], 0.025);
        close(s[30], 6.025);
        let z = positions(
            0.0,
            6.0,
            PathLayout::Zones {
                end: 1.0,
                end_s: 0.1,
                mid_s: 0.2,
            },
        );
        assert_eq!(z.len(), 11 + 19 + 11);
        assert!(z.windows(2).all(|w| w[1] > w[0] + 1e-9));
        assert!(z.windows(2).all(|w| w[1] - w[0] <= 0.2 + 1e-9));
        close(z[0], 0.0);
        close(*z.last().unwrap(), 6.0);
        // overlapping zones: dense spacing everywhere
        let o = positions(
            0.0,
            1.5,
            PathLayout::Zones {
                end: 1.0,
                end_s: 0.1,
                mid_s: 0.2,
            },
        );
        assert_eq!(o.len(), 16);
        assert!(positions(1.0, 0.0, PathLayout::Fixed(3)).is_empty());
        assert!(positions(0.0, 1.0, PathLayout::MaxSpacing(0.0)).is_empty());
    }

    fn rect(w: f64, h: f64) -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(w, 0.0), v2(w, h), v2(0.0, h)]
    }

    #[test]
    fn area_layer_respects_cover_and_holes() {
        let d = 0.01;
        let bars = area_lines(&[rect(4.0, 3.0)], 0.0, 0.2, 0.03, d);
        assert_eq!(bars.len(), 16);
        for (a, b) in &bars {
            close(a.x, 0.03);
            close(b.x, 3.97);
        }
        close(bars[0].0.y, 0.035);
        close(bars[15].0.y, 2.965);
        let hole = vec![v2(1.5, 1.0), v2(2.5, 1.0), v2(2.5, 2.0), v2(1.5, 2.0)];
        let with = area_lines(&[rect(4.0, 3.0), hole], 0.0, 0.2, 0.03, d);
        let split = with.iter().filter(|s| s.1.x < 2.0).count();
        assert!(with.len() > bars.len() && split > 0);
        for (a, b) in &with {
            let m = (a.y > 1.0 && a.y < 2.0) as usize;
            if m == 1 {
                assert!(b.x <= 1.47 + 1e-9 || a.x >= 2.53 - 1e-9, "{a:?} {b:?}");
            }
        }
        // rotated layer stays inside
        for (a, b) in area_lines(&[rect(4.0, 3.0)], 0.5, 0.25, 0.03, d) {
            for p in [a, b] {
                assert!(p.x > 0.0 && p.x < 4.0 && p.y > 0.0 && p.y < 3.0, "{p:?}");
            }
        }
    }

    #[test]
    fn bars_along_a_hole_edge_keep_the_cover() {
        let d = 0.01;
        // hole edge y = 1.0; a bar at y = 0.99 runs along it at 0 cover → cut at the hole
        let hole = vec![v2(1.0, 1.0), v2(3.0, 1.0), v2(3.0, 2.0), v2(1.0, 2.0)];
        let rings = [rect(4.0, 3.0), hole];
        for (a, b) in area_lines(&rings, 0.0, 0.2, 0.03, d) {
            let near = a.y > 1.0 - 0.035 && a.y < 2.0 + 0.035;
            if near {
                assert!(
                    b.x <= 1.0 - 0.03 + 1e-9 || a.x >= 3.0 + 0.03 - 1e-9,
                    "{a:?} {b:?}"
                );
            }
        }
        // two pieces of a split plan: bars in both, none in the gap
        let r2 = [
            rect(1.0, 1.0),
            vec![v2(2.0, 0.0), v2(3.0, 0.0), v2(3.0, 1.0), v2(2.0, 1.0)],
        ];
        let bars = area_lines(&r2, 0.0, 0.2, 0.03, d);
        assert!(bars.iter().any(|s| s.0.x > 2.0) && bars.iter().any(|s| s.1.x < 1.0));
        assert!(bars.iter().all(|s| s.1.x < 1.0 || s.0.x > 2.0));
        assert!(inside_region(v2(0.5, 0.5), &r2, 0.03));
        assert!(!inside_region(v2(1.5, 0.5), &r2, 0.03));
        assert!(!inside_region(v2(0.01, 0.5), &r2, 0.03));
    }

    #[test]
    fn outer_dims_add_half_a_bar_per_bent_end() {
        let d = 0.02;
        let none = (Hook::None, Hook::None);
        let l = ShapeCode::L.verts(2.0, 0.5, 0.0);
        let o = outer_dims(&l, none, d);
        close(o[0], 2.01);
        close(o[1], 0.51);
        let s = ShapeCode::Straight.verts(3.0, 0.0, 0.0);
        close(outer_dims(&s, none, d)[0], 3.0);
        close(outer_dims(&s, (Hook::H90, Hook::H90), d)[0], 3.02);
        let u = outer_dims(&ShapeCode::U.verts(1.0, 0.3, 0.3), none, d);
        assert_eq!(u.len(), 3);
        close(u[1], 1.02);
    }

    #[test]
    fn clip_to_box_and_region() {
        let lo = v3(0.03, 0.03, 0.03);
        let hi = v3(3.97, 0.27, 0.47);
        let (a, b) = clip_box(v3(-1.0, 0.1, 0.1), v3(6.0, 0.1, 0.1), lo, hi).unwrap();
        close(a.x, 0.03);
        close(b.x, 3.97);
        assert!(clip_box(v3(0.0, 0.5, 0.1), v3(6.0, 0.5, 0.1), lo, hi).is_none());
        let (t0, t1) = clip_region(v2(-1.0, 1.0), v2(6.0, 1.0), &[rect(4.0, 3.0)], 0.03).unwrap();
        close(-1.0 + 7.0 * t0, 0.03);
        close(-1.0 + 7.0 * t1, 3.97);
    }

    #[test]
    fn tube_is_closed_and_has_the_prism_volume() {
        let pts = [v3(0.0, 0.0, 0.0), v3(2.0, 0.0, 0.0)];
        let m = tube(&pts, 0.01, 16);
        assert!(m.is_watertight());
        let poly = 16.0 / 2.0 * 0.01f64.powi(2) * (2.0 * PI / 16.0).sin();
        assert!((m.volume() - poly * 2.0).abs() < 1e-12, "{}", m.volume());
        // a bent bar stays closed and outward
        let d = 0.012;
        let rc = bend_radius(d, 0.0);
        let p = centerline(
            &ShapeCode::L.verts(1.0, 0.4, 0.0),
            rc,
            d,
            (Hook::None, Hook::H135),
            v2(0.0, 1.0),
            v2(-1.0, 0.0),
        )
        .unwrap();
        let bar = Bar {
            d,
            o: v3(0.0, 0.0, 0.0),
            u: v3(1.0, 0.0, 0.0),
            v: v3(0.0, 0.0, 1.0),
            path: p,
        };
        let m = bar.mesh(8);
        assert!(m.is_watertight());
        assert!(m.volume() > 0.0);
        let want = 4.0 * (PI / 4.0).sin() * (d / 2.0).powi(2) * bar.length();
        assert!(
            (m.volume() - want).abs() / want < 0.06,
            "{} vs {want}",
            m.volume()
        );
        assert_eq!(lod_sides(10), 8);
        assert_eq!(lod_sides(50_000), 3);
    }
}
