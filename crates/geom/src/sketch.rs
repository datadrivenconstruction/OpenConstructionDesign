// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Boundary sketch (slab, roof, ceiling, shaft, opening): curves are a line segment, an arc, an
//! elliptical arc, a spline (cubic B-spline with clamped ends). Curves may be drawn in any
//! order; `Sketch::validate` stitches them into loops by their endpoints, checks closedness and
//! self-intersection, and returns errors with curve indices for highlighting. Area is exact:
//! ½∮(x dy − y dx) is a closed formula for arcs and ellipses, and Gauss quadrature per span for splines
//! (the integrand is a degree-5 polynomial; 3 nodes are exact). Tessellation points lie on the curve.

use crate::math::{v2, V2};
use std::f64::consts::PI;

#[derive(Clone, Debug, PartialEq)]
pub enum SketchCurve {
    Line {
        a: V2,
        b: V2,
    },
    /// Centre, radius, start angle, signed sweep angle (rad).
    Arc {
        c: V2,
        r: f64,
        a0: f64,
        sweep: f64,
    },
    /// Centre, semi-axes, rotation of axis rx, start parameter and signed parameter sweep.
    Ellipse {
        c: V2,
        rx: f64,
        ry: f64,
        rot: f64,
        t0: f64,
        sweep: f64,
    },
    /// Control points; degree min(3, n−1), uniform knots, endpoints interpolated.
    Spline {
        ctrl: Vec<V2>,
    },
}

const GL3: [(f64, f64); 3] = [
    (-0.774_596_669_241_483_4, 5.0 / 9.0),
    (0.0, 8.0 / 9.0),
    (0.774_596_669_241_483_4, 5.0 / 9.0),
];

fn rot(v: V2, a: f64) -> V2 {
    let (s, c) = a.sin_cos();
    v2(v.x * c - v.y * s, v.x * s + v.y * c)
}

impl SketchCurve {
    /// Arc through three points (start, a point on the arc, end).
    pub fn arc3(a: V2, m: V2, b: V2) -> Option<SketchCurve> {
        let d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
        if d.abs() < 1e-12 {
            return None;
        }
        let sq = |p: V2| p.dot(p);
        let c = v2(
            (sq(a) * (m.y - b.y) + sq(m) * (b.y - a.y) + sq(b) * (a.y - m.y)) / d,
            (sq(a) * (b.x - m.x) + sq(m) * (a.x - b.x) + sq(b) * (m.x - a.x)) / d,
        );
        let ang = |p: V2| (p.y - c.y).atan2(p.x - c.x);
        let (a0, am, a1) = (ang(a), ang(m), ang(b));
        let ccw = |x: f64| (x - a0).rem_euclid(2.0 * PI);
        let sweep = if ccw(am) < ccw(a1) {
            ccw(a1)
        } else {
            ccw(a1) - 2.0 * PI
        };
        Some(SketchCurve::Arc {
            c,
            r: (a - c).len(),
            a0,
            sweep,
        })
    }

    fn degree(ctrl: &[V2]) -> usize {
        ctrl.len().saturating_sub(1).min(3)
    }

    /// Number of spline spans (parameter 0..spans).
    fn spans(ctrl: &[V2]) -> usize {
        ctrl.len() - Self::degree(ctrl)
    }

    fn knot(ctrl: &[V2], i: isize) -> f64 {
        let p = Self::degree(ctrl) as isize;
        let n = Self::spans(ctrl) as isize;
        (i - p).clamp(0, n) as f64
    }

    /// De Boor: point and derivative with respect to parameter u ∈ [0, spans].
    fn spline_eval(ctrl: &[V2], u: f64) -> (V2, V2) {
        let p = Self::degree(ctrl);
        let n = Self::spans(ctrl);
        let k = (u.floor() as usize).min(n - 1) + p; // span [knot k, knot k+1)
        let pts = |q: usize| -> Vec<V2> { (0..=q).map(|j| ctrl[k - q + j]).collect() };
        let run = |deg: usize, mut d: Vec<V2>| -> V2 {
            for r in 1..=deg {
                for j in (r..=deg).rev() {
                    let i = (k - deg + j) as isize;
                    let lo = Self::knot(ctrl, i);
                    let hi = Self::knot(ctrl, i + 1 + (deg - r) as isize);
                    let al = if hi > lo { (u - lo) / (hi - lo) } else { 0.0 };
                    d[j] = d[j - 1] * (1.0 - al) + d[j] * al;
                }
            }
            d[deg]
        };
        let pt = run(p, pts(p));
        // derivative: a degree p−1 B-spline built from differences
        let mut q = Vec::with_capacity(p);
        for j in 1..=p {
            let i = (k - p + j) as isize;
            let den = Self::knot(ctrl, i + p as isize) - Self::knot(ctrl, i);
            let dp = ctrl[k - p + j] - ctrl[k - p + j - 1];
            q.push(if den > 0.0 {
                dp * (p as f64 / den)
            } else {
                v2(0.0, 0.0)
            });
        }
        let der = if p == 1 {
            q[0]
        } else {
            let mut d = q;
            for r in 1..p {
                for j in (r..p).rev() {
                    let i = (k - (p - 1) + j) as isize;
                    let lo = Self::knot(ctrl, i);
                    let hi = Self::knot(ctrl, i + p as isize - r as isize);
                    let al = if hi > lo { (u - lo) / (hi - lo) } else { 0.0 };
                    d[j] = d[j - 1] * (1.0 - al) + d[j] * al;
                }
            }
            d[p - 1]
        };
        (pt, der)
    }

    /// Point at t ∈ [0, 1].
    pub fn point(&self, t: f64) -> V2 {
        match self {
            SketchCurve::Line { a, b } => *a + (*b - *a) * t,
            SketchCurve::Arc { c, r, a0, sweep } => {
                let a = a0 + sweep * t;
                *c + v2(a.cos(), a.sin()) * *r
            }
            SketchCurve::Ellipse {
                c,
                rx,
                ry,
                rot: ro,
                t0,
                sweep,
            } => {
                let a = t0 + sweep * t;
                *c + rot(v2(rx * a.cos(), ry * a.sin()), *ro)
            }
            SketchCurve::Spline { ctrl } => Self::spline_eval(ctrl, t * Self::spans(ctrl) as f64).0,
        }
    }

    pub fn start(&self) -> V2 {
        match self {
            SketchCurve::Spline { ctrl } => ctrl[0],
            SketchCurve::Line { a, .. } => *a,
            _ => self.point(0.0),
        }
    }

    pub fn end(&self) -> V2 {
        match self {
            SketchCurve::Spline { ctrl } => ctrl[ctrl.len() - 1],
            SketchCurve::Line { b, .. } => *b,
            _ => self.point(1.0),
        }
    }

    pub fn reversed(&self) -> SketchCurve {
        match self {
            SketchCurve::Line { a, b } => SketchCurve::Line { a: *b, b: *a },
            SketchCurve::Arc { c, r, a0, sweep } => SketchCurve::Arc {
                c: *c,
                r: *r,
                a0: a0 + sweep,
                sweep: -sweep,
            },
            SketchCurve::Ellipse {
                c,
                rx,
                ry,
                rot,
                t0,
                sweep,
            } => SketchCurve::Ellipse {
                c: *c,
                rx: *rx,
                ry: *ry,
                rot: *rot,
                t0: t0 + sweep,
                sweep: -sweep,
            },
            SketchCurve::Spline { ctrl } => SketchCurve::Spline {
                ctrl: ctrl.iter().rev().copied().collect(),
            },
        }
    }

    /// Whether the curve is usable (finite numbers, non-zero length, spline with ≥ 2 points).
    pub fn is_degenerate(&self, eps: f64) -> bool {
        let fin = |v: V2| v.x.is_finite() && v.y.is_finite();
        match self {
            SketchCurve::Line { a, b } => !fin(*a) || !fin(*b) || (*b - *a).len() <= eps,
            SketchCurve::Arc { r, sweep, .. } => {
                !(r.is_finite() && sweep.is_finite()) || r * sweep.abs() <= eps
            }
            SketchCurve::Ellipse { rx, ry, sweep, .. } => {
                !(rx.is_finite() && ry.is_finite() && sweep.is_finite())
                    || rx.min(*ry) <= eps
                    || sweep.abs() <= 1e-12
            }
            SketchCurve::Spline { ctrl } => {
                ctrl.len() < 2 || !ctrl.iter().all(|p| fin(*p)) || self.length() <= eps
            }
        }
    }

    /// ½∫(x dy − y dx) along the curve, exact.
    pub fn area_term(&self) -> f64 {
        match self {
            SketchCurve::Line { a, b } => a.cross(*b) / 2.0,
            SketchCurve::Arc { c, r, a0, sweep } => {
                let a1 = a0 + sweep;
                0.5 * (c.x * r * (a1.sin() - a0.sin()) - c.y * r * (a1.cos() - a0.cos())
                    + r * r * sweep)
            }
            SketchCurve::Ellipse {
                c, rx, ry, sweep, ..
            } => {
                let d = self.end() - self.start();
                0.5 * (c.cross(d) + rx * ry * sweep)
            }
            SketchCurve::Spline { ctrl } => {
                let n = Self::spans(ctrl);
                let mut s = 0.0;
                for k in 0..n {
                    for (x, w) in GL3 {
                        let u = k as f64 + 0.5 + 0.5 * x;
                        let (p, d) = Self::spline_eval(ctrl, u);
                        s += 0.5 * w * p.cross(d) * 0.5;
                    }
                }
                s
            }
        }
    }

    /// Length: exact for a segment and an arc; for an ellipse and a spline, Gauss quadrature over 64 parts per span.
    pub fn length(&self) -> f64 {
        match self {
            SketchCurve::Line { a, b } => (*b - *a).len(),
            SketchCurve::Arc { r, sweep, .. } => r * sweep.abs(),
            _ => {
                let parts = match self {
                    SketchCurve::Spline { ctrl } => 64 * Self::spans(ctrl),
                    _ => 256,
                };
                let h = 1.0 / parts as f64;
                let mut s = 0.0;
                for k in 0..parts {
                    for (x, w) in GL3 {
                        let t = (k as f64 + 0.5 + 0.5 * x) * h;
                        s += 0.5 * w * h * self.velocity(t).len();
                    }
                }
                s
            }
        }
    }

    /// Derivative with respect to t ∈ [0, 1].
    pub fn velocity(&self, t: f64) -> V2 {
        match self {
            SketchCurve::Line { a, b } => *b - *a,
            SketchCurve::Arc { r, a0, sweep, .. } => {
                let a = a0 + sweep * t;
                v2(-a.sin(), a.cos()) * (r * sweep)
            }
            SketchCurve::Ellipse {
                rx,
                ry,
                rot: ro,
                t0,
                sweep,
                ..
            } => {
                let a = t0 + sweep * t;
                rot(v2(-rx * a.sin(), ry * a.cos()), *ro) * *sweep
            }
            SketchCurve::Spline { ctrl } => {
                let n = Self::spans(ctrl) as f64;
                Self::spline_eval(ctrl, t * n).1 * n
            }
        }
    }

    /// Curve points from the start, excluding the end; chord sagitta ≤ `sag`. All points lie on the curve.
    pub fn tessellate(&self, sag: f64) -> Vec<V2> {
        let sag = sag.max(1e-9);
        let k = match self {
            SketchCurve::Line { .. } => 1,
            SketchCurve::Arc { r, sweep, .. } => arc_steps(*r, sweep.abs(), sag),
            SketchCurve::Ellipse { rx, ry, sweep, .. } => {
                // the ellipse's radius of curvature is ≥ min²/max: step sized for the worst case
                let rmin = rx.min(*ry).powi(2) / rx.max(*ry);
                let rmax = rx.max(*ry);
                // the limit only guards against looping; the sagitta is guaranteed up to it
                arc_steps(rmin, sweep.abs() * rmax / rmin.max(1e-12), sag).min(1 << 20)
            }
            SketchCurve::Spline { .. } => 0,
        };
        if k > 0 {
            return (0..k).map(|j| self.point(j as f64 / k as f64)).collect();
        }
        // spline: adaptive subdivision by the midpoint sagitta
        let mut out = vec![self.start()];
        self.split(0.0, 1.0, sag, 0, &mut out);
        out
    }

    fn split(&self, t0: f64, t1: f64, sag: f64, depth: u32, out: &mut Vec<V2>) {
        let (p, q) = (self.point(t0), self.point(t1));
        let tm = 0.5 * (t0 + t1);
        let dev = |t: f64| seg_dist(self.point(t), p, q);
        let worst = dev(tm)
            .max(dev(t0 + 0.25 * (t1 - t0)))
            .max(dev(t0 + 0.75 * (t1 - t0)));
        if depth < 3 || (worst > sag && depth < 20) {
            self.split(t0, tm, sag, depth + 1, out);
            out.push(self.point(tm));
            self.split(tm, t1, sag, depth + 1, out);
        }
    }
}

fn arc_steps(r: f64, sweep: f64, sag: f64) -> usize {
    let step = if r <= sag {
        PI / 8.0
    } else {
        (2.0 * (1.0 - sag / r).acos()).min(PI / 8.0)
    };
    (sweep / step).ceil().max(1.0) as usize
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + d * t)).len()
}

/// Closed loop: curves in order, the end of i coincides with the start of i+1.
#[derive(Clone, Debug, PartialEq)]
pub struct Loop {
    pub curves: Vec<SketchCurve>,
    /// Index of the source sketch curve for each loop curve.
    pub source: Vec<usize>,
}

impl Loop {
    pub fn area(&self) -> f64 {
        self.curves.iter().map(|c| c.area_term()).sum()
    }
    pub fn perimeter(&self) -> f64 {
        self.curves.iter().map(|c| c.length()).sum()
    }
    pub fn ring(&self, sag: f64) -> Vec<V2> {
        self.curves.iter().flat_map(|c| c.tessellate(sag)).collect()
    }
    fn reversed(&self) -> Loop {
        Loop {
            curves: self.curves.iter().rev().map(|c| c.reversed()).collect(),
            source: self.source.iter().rev().copied().collect(),
        }
    }
}

/// Region: an outer loop (counter-clockwise) and holes (clockwise).
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub outer: Loop,
    pub holes: Vec<Loop>,
}

impl Region {
    /// Exact area (net of holes).
    pub fn area(&self) -> f64 {
        self.outer.area() + self.holes.iter().map(|h| h.area()).sum::<f64>()
    }
    /// Rings for the kernel: [outer, holes…].
    pub fn rings(&self, sag: f64) -> Vec<Vec<V2>> {
        std::iter::once(&self.outer)
            .chain(&self.holes)
            .map(|l| l.ring(sag))
            .collect()
    }
}

/// Sketch error; numbers are curve indices in `Sketch::curves` (for highlighting).
#[derive(Clone, Debug, PartialEq)]
pub enum SketchIssue {
    Empty,
    Degenerate {
        curve: usize,
    },
    /// A free curve end (the loop is not closed).
    OpenEnd {
        curve: usize,
        at: V2,
    },
    /// More than two ends meet at one point.
    Branch {
        curves: Vec<usize>,
        at: V2,
    },
    /// Curves intersect (or a curve intersects itself).
    Intersect {
        a: usize,
        b: usize,
        at: V2,
    },
    /// A loop inside a hole (an island) is not supported within a single region.
    Nested {
        curve: usize,
    },
}

impl SketchIssue {
    /// Curves to highlight.
    pub fn curves(&self) -> Vec<usize> {
        match self {
            SketchIssue::Empty => vec![],
            SketchIssue::Degenerate { curve }
            | SketchIssue::OpenEnd { curve, .. }
            | SketchIssue::Nested { curve } => vec![*curve],
            SketchIssue::Branch { curves, .. } => curves.clone(),
            SketchIssue::Intersect { a, b, .. } => vec![*a, *b],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sketch {
    pub curves: Vec<SketchCurve>,
}

/// Endpoint stitching tolerance and the sagitta used for intersection checks.
pub const JOIN_EPS: f64 = 1e-6;
/// Default minimum sketch curve length (1 mm); shorter is an error with highlighting.
pub const MIN_EDGE: f64 = 1e-3;
const CHECK_SAG: f64 = 1e-4;

impl Sketch {
    /// Stitches curves into loops, validates and builds regions. All errors found are reported at once.
    pub fn validate(&self) -> Result<Vec<Region>, Vec<SketchIssue>> {
        self.validate_with(MIN_EDGE)
    }

    /// Like `validate`, with a given minimum curve length.
    pub fn validate_with(&self, min_edge: f64) -> Result<Vec<Region>, Vec<SketchIssue>> {
        if self.curves.is_empty() {
            return Err(vec![SketchIssue::Empty]);
        }
        let mut issues = vec![];
        for (i, c) in self.curves.iter().enumerate() {
            if c.is_degenerate(min_edge.max(JOIN_EPS)) {
                issues.push(SketchIssue::Degenerate { curve: i });
            }
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        // endpoints: (curve, 0 for start / 1 for end)
        let ends: Vec<(usize, u8, V2)> = self
            .curves
            .iter()
            .enumerate()
            .flat_map(|(i, c)| [(i, 0u8, c.start()), (i, 1u8, c.end())])
            .collect();
        let mut mate: Vec<Option<usize>> = vec![None; ends.len()];
        let mut seen = vec![false; ends.len()];
        let grid = Grid::of_points(ends.iter().map(|e| e.2), 4.0 * JOIN_EPS);
        for k in 0..ends.len() {
            if seen[k] {
                continue;
            }
            let mut group: Vec<usize> = grid
                .near(ends[k].2, JOIN_EPS)
                .filter(|&j| (ends[j].2 - ends[k].2).len() <= JOIN_EPS)
                .collect();
            group.sort_unstable();
            for &j in &group {
                seen[j] = true;
            }
            match group.len() {
                1 => issues.push(SketchIssue::OpenEnd {
                    curve: ends[k].0,
                    at: ends[k].2,
                }),
                2 => {
                    mate[group[0]] = Some(group[1]);
                    mate[group[1]] = Some(group[0]);
                }
                _ => {
                    let mut cs: Vec<usize> = group.iter().map(|&j| ends[j].0).collect();
                    cs.dedup();
                    issues.push(SketchIssue::Branch {
                        curves: cs,
                        at: ends[k].2,
                    })
                }
            }
        }
        issues.extend(self.intersections());
        if !issues.is_empty() {
            return Err(issues);
        }
        // loop traversal
        let mut used = vec![false; self.curves.len()];
        let mut loops = vec![];
        for s in 0..self.curves.len() {
            if used[s] {
                continue;
            }
            let mut lp = Loop {
                curves: vec![],
                source: vec![],
            };
            let (mut cur, mut fwd) = (s, true);
            loop {
                used[cur] = true;
                let c = &self.curves[cur];
                lp.curves.push(if fwd { c.clone() } else { c.reversed() });
                lp.source.push(cur);
                let out_end = 2 * cur + if fwd { 1 } else { 0 };
                let nx = mate[out_end].expect("all ends matched");
                let (nc, side) = (ends[nx].0, ends[nx].1);
                if nc == s {
                    break;
                }
                cur = nc;
                fwd = side == 0;
            }
            loops.push(lp);
        }
        // nesting: depth is the number of loops containing a point of the loop
        let rings: Vec<Vec<V2>> = loops.iter().map(|l| l.ring(CHECK_SAG)).collect();
        let depth: Vec<usize> = (0..loops.len())
            .map(|i| {
                let p = rings[i][0];
                (0..loops.len())
                    .filter(|&j| j != i && crate::predicates::point_in_ring(p, &rings[j]) > 0)
                    .count()
            })
            .collect();
        // even-odd: even depth is a region (an island inside a hole too), odd depth is a hole of
        // the nearest enclosing region (depth one less)
        let mut regions: Vec<(usize, Region)> = vec![];
        for (i, l) in loops.iter().enumerate() {
            if depth[i] % 2 == 0 {
                let o = if l.area() < 0.0 {
                    l.reversed()
                } else {
                    l.clone()
                };
                regions.push((
                    i,
                    Region {
                        outer: o,
                        holes: vec![],
                    },
                ));
            }
        }
        for (i, l) in loops.iter().enumerate() {
            if depth[i] % 2 == 1 {
                let p = rings[i][0];
                let host = regions.iter_mut().find(|(j, _)| {
                    depth[*j] + 1 == depth[i] && crate::predicates::point_in_ring(p, &rings[*j]) > 0
                });
                let h = if l.area() > 0.0 {
                    l.reversed()
                } else {
                    l.clone()
                };
                match host {
                    Some((_, r)) => r.holes.push(h),
                    None => issues.push(SketchIssue::Nested { curve: l.source[0] }),
                }
            }
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        Ok(regions.into_iter().map(|(_, r)| r).collect())
    }

    /// Intersections via tessellation: non-adjacent segments; shared endpoints of neighbouring curves do not count.
    fn intersections(&self) -> Vec<SketchIssue> {
        let polys: Vec<Vec<V2>> = self
            .curves
            .iter()
            .map(|c| {
                let mut p = c.tessellate(CHECK_SAG);
                p.push(c.end());
                p
            })
            .collect();
        let shared = |a: V2, b: V2| (a - b).len() <= JOIN_EPS;
        // segments in a uniform grid: only pairs sharing cells are tested
        let segs: Vec<(usize, usize)> = polys
            .iter()
            .enumerate()
            .flat_map(|(i, p)| (0..p.len() - 1).map(move |k| (i, k)))
            .collect();
        let seg = |s: (usize, usize)| (polys[s.0][s.1], polys[s.0][s.1 + 1]);
        let grid = Grid::of_segments(segs.iter().map(|&s| seg(s)));
        let mut pairs = std::collections::BTreeSet::new();
        for (u, v) in grid.pairs() {
            let (p, q) = if segs[u] <= segs[v] {
                (segs[u], segs[v])
            } else {
                (segs[v], segs[u])
            };
            if p.0 == q.0 && q.1 < p.1 + 2 {
                continue; // adjacent segments of the same curve
            }
            pairs.insert((p.0, q.0, p.1, q.1));
        }
        let mut out = vec![];
        let mut done = None;
        for (i, j, si, sj) in pairs {
            if done == Some((i, j)) {
                continue;
            }
            let ((a, b), (c, d)) = (seg((i, si)), seg((j, sj)));
            if let Some(x) = seg_hit(a, b, c, d) {
                let at_end = |p: V2| shared(x, p);
                let ends_i = [polys[i][0], *polys[i].last().unwrap()];
                let ends_j = [polys[j][0], *polys[j].last().unwrap()];
                let joint = ends_i.iter().any(|&e| at_end(e)) && ends_j.iter().any(|&e| at_end(e));
                if !joint {
                    out.push(SketchIssue::Intersect { a: i, b: j, at: x });
                    done = Some((i, j));
                }
            }
        }
        out
    }
}

/// Uniform neighbour grid: curve endpoints and segment pairs without testing all pairs.
pub struct Grid {
    cell: f64,
    map: std::collections::HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn key(&self, p: V2) -> (i64, i64) {
        (
            (p.x / self.cell).floor() as i64,
            (p.y / self.cell).floor() as i64,
        )
    }

    pub fn of_points(pts: impl Iterator<Item = V2>, cell: f64) -> Grid {
        let mut g = Grid {
            cell,
            map: Default::default(),
        };
        for (i, p) in pts.enumerate() {
            let k = g.key(p);
            g.map.entry(k).or_default().push(i);
        }
        g
    }

    /// Candidates within radius `r` (the caller checks the exact distance).
    pub fn near(&self, p: V2, r: f64) -> impl Iterator<Item = usize> + '_ {
        let d = V2 { x: r, y: r };
        let (lo, hi) = (self.key(p - d), self.key(p + d));
        (lo.0..=hi.0)
            .flat_map(move |x| (lo.1..=hi.1).map(move |y| (x, y)))
            .filter_map(|k| self.map.get(&k))
            .flatten()
            .copied()
    }

    /// Segments by the cells they pass through (per column, not the whole bounding box: a long
    /// diagonal among small segments does not fill the square); the cell is the mean segment length
    /// (at least 1 mm).
    pub fn of_segments(segs: impl Iterator<Item = (V2, V2)>) -> Grid {
        let v: Vec<(V2, V2)> = segs.collect();
        let mean = v.iter().map(|s| (s.1 - s.0).len()).sum::<f64>() / v.len().max(1) as f64;
        let mut g = Grid {
            cell: mean.max(1e-3),
            map: Default::default(),
        };
        let pad = V2 { x: 1e-9, y: 1e-9 };
        for (i, (a, b)) in v.iter().enumerate() {
            let lo = g.key(
                V2 {
                    x: a.x.min(b.x),
                    y: a.y.min(b.y),
                } - pad,
            );
            let hi = g.key(
                V2 {
                    x: a.x.max(b.x),
                    y: a.y.max(b.y),
                } + pad,
            );
            let c = g.cell;
            for x in lo.0..=hi.0 {
                // the part of the segment in column x (with margin pad) and its y cells
                let (y0, y1) = if (b.x - a.x).abs() < 1e-15 {
                    (a.y.min(b.y), a.y.max(b.y))
                } else {
                    let t = |x: f64| ((x - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
                    let (ta, tb) = (t(x as f64 * c - pad.x), t((x + 1) as f64 * c + pad.x));
                    let (ya, yb) = (a.y + (b.y - a.y) * ta, a.y + (b.y - a.y) * tb);
                    (ya.min(yb), ya.max(yb))
                };
                let ylo = ((y0 - pad.y) / c).floor() as i64;
                let yhi = ((y1 + pad.y) / c).floor() as i64;
                for y in ylo.max(lo.1)..=yhi.min(hi.1) {
                    g.map.entry((x, y)).or_default().push(i);
                }
            }
        }
        g
    }

    /// Object pairs from shared cells, each once (u < v).
    pub fn pairs(&self) -> std::collections::BTreeSet<(usize, usize)> {
        let mut out = std::collections::BTreeSet::new();
        for ids in self.map.values() {
            for (k, &u) in ids.iter().enumerate() {
                for &v in &ids[k + 1..] {
                    out.insert((u.min(v), u.max(v)));
                }
            }
        }
        out
    }
}

fn seg_hit(a: V2, b: V2, c: V2, d: V2) -> Option<V2> {
    let r = b - a;
    let s = d - c;
    let den = r.cross(s);
    if den.abs() < 1e-15 {
        // collinear overlap
        if (c - a).cross(r).abs() > 1e-12 * r.len().max(1.0) {
            return None;
        }
        let l2 = r.dot(r);
        if l2 == 0.0 {
            return None;
        }
        let (t0, t1) = ((c - a).dot(r) / l2, (d - a).dot(r) / l2);
        let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(1.0));
        return (hi - lo > 1e-9).then(|| a + r * ((lo + hi) / 2.0));
    }
    let t = (c - a).cross(s) / den;
    let u = (c - a).cross(r) / den;
    let e = 1e-12;
    ((-e..=1.0 + e).contains(&t) && (-e..=1.0 + e).contains(&u)).then(|| a + r * t)
}

/// Why the selected walls do not form a loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WallsLoopError {
    /// Fewer than three walls.
    TooFew,
    /// The loop is not closed: a free end at a point.
    Open(V2),
    /// More than two loop branches meet at a point.
    Branch(V2),
    /// The wall with this index is not part of the loop.
    Stray(usize),
    /// The inward offset turned the loop inside out.
    OffsetFlips,
}

/// Axis endpoint coincidence tolerance, in metres.
const AXIS_NEAR: f64 = 1e-4;
/// Axis extension margin until it meets a neighbour: the wall abuts a face, not an axis, in metres.
const AXIS_REACH: f64 = 0.5;

/// Meetings of two axes: parameter (metres from the start) on the first and the second, and the point.
fn axis_meets(p: (V2, V2), q: (V2, V2), reach: f64) -> Vec<(Option<f64>, Option<f64>, V2)> {
    let (lp, lq) = ((p.1 - p.0).len(), (q.1 - q.0).len());
    if lp < AXIS_NEAR || lq < AXIS_NEAR {
        return vec![];
    }
    let (dp, dq) = ((p.1 - p.0) * (1.0 / lp), (q.1 - q.0) * (1.0 / lq));
    let den = dp.cross(dq);
    if den.abs() > 1e-9 {
        let t = (q.0 - p.0).cross(dq) / den;
        let u = (q.0 - p.0).cross(dp) / den;
        let lim = reach + AXIS_NEAR;
        let inside = t >= -lim && t <= lp + lim && u >= -lim && u <= lq + lim;
        return if inside {
            vec![(Some(t), Some(u), p.0 + dp * t)]
        } else {
            vec![]
        };
    }
    // parallel: only endpoints lying on the other axis (continuation or overlap)
    let mut out = vec![];
    for e in [q.0, q.1] {
        let t = (e - p.0).dot(dp);
        if (e - p.0).cross(dp).abs() <= AXIS_NEAR && t >= -AXIS_NEAR && t <= lp + AXIS_NEAR {
            out.push((Some(t), None, e));
        }
    }
    for e in [p.0, p.1] {
        let u = (e - q.0).dot(dq);
        if (e - q.0).cross(dq).abs() <= AXIS_NEAR && u >= -AXIS_NEAR && u <= lq + AXIS_NEAR {
            out.push((None, Some(u), e));
        }
    }
    out
}

/// The single cycle of the selected axes: axes intersect, tails past corners are trimmed.
/// Direction follows the first axis.
fn axes_cycle(axes: &[(V2, V2)], reach: f64) -> Result<Vec<(V2, V2)>, WallsLoopError> {
    let n = axes.len();
    let mut cuts: Vec<Vec<(f64, V2)>> = axes
        .iter()
        .map(|&(a, b)| vec![(0.0, a), ((b - a).len(), b)])
        .collect();
    for i in 0..n {
        for j in i + 1..n {
            for (t, u, p) in axis_meets(axes[i], axes[j], reach) {
                if let Some(t) = t {
                    cuts[i].push((t, p));
                }
                if let Some(u) = u {
                    cuts[j].push((u, p));
                }
            }
        }
    }
    let mut nodes: Vec<V2> = vec![];
    // edges (from, to, axis) without repeats: overlapping axes yield one edge
    let mut edges: Vec<(usize, usize, usize)> = vec![];
    for (i, c) in cuts.iter_mut().enumerate() {
        c.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut ids: Vec<usize> = Vec::with_capacity(c.len());
        for x in c.iter() {
            let k = match nodes.iter().position(|q| (*q - x.1).len() <= AXIS_NEAR) {
                Some(k) => k,
                None => {
                    nodes.push(x.1);
                    nodes.len() - 1
                }
            };
            ids.push(k);
        }
        for w in ids.windows(2) {
            let dup = edges
                .iter()
                .any(|e| (e.0, e.1) == (w[0], w[1]) || (e.0, e.1) == (w[1], w[0]));
            if w[0] != w[1] && !dup {
                edges.push((w[0], w[1], i));
            }
        }
    }
    fn degree(es: &[(usize, usize, usize)], v: usize) -> usize {
        es.iter().filter(|e| e.0 == v || e.1 == v).count()
    }
    let mut free: Option<usize> = None;
    loop {
        let before = edges.len();
        let snapshot = edges.clone();
        edges.retain(|e| {
            let dangling = [e.0, e.1].into_iter().find(|v| degree(&snapshot, *v) == 1);
            if free.is_none() {
                free = dangling;
            }
            dangling.is_none()
        });
        if edges.len() == before {
            break;
        }
    }
    if edges.is_empty() {
        return Err(WallsLoopError::Open(free.map_or(axes[0].0, |v| nodes[v])));
    }
    if let Some(v) = (0..nodes.len()).find(|v| degree(&edges, *v) > 2) {
        return Err(WallsLoopError::Branch(nodes[v]));
    }
    if let Some(i) = (0..n).find(|i| !edges.iter().any(|e| e.2 == *i)) {
        return Err(WallsLoopError::Stray(i));
    }
    // all vertices have degree 2: traverse from the first edge of the first axis
    let first = edges[0];
    let mut cycle = vec![(first.0, first.1)];
    let mut used = vec![0usize];
    while cycle[cycle.len() - 1].1 != first.0 {
        let tail = cycle[cycle.len() - 1].1;
        let Some(k) = (0..edges.len())
            .find(|k| !used.contains(k) && (edges[*k].0 == tail || edges[*k].1 == tail))
        else {
            return Err(WallsLoopError::Open(nodes[tail]));
        };
        used.push(k);
        let e = edges[k];
        cycle.push(if e.0 == tail { (e.0, e.1) } else { (e.1, e.0) });
    }
    if let Some(k) = (0..edges.len()).find(|k| !used.contains(k)) {
        return Err(WallsLoopError::Stray(edges[k].2));
    }
    // parts of one straight side form a single segment
    let m = cycle.len();
    let corners: Vec<V2> = (0..m)
        .filter(|&i| {
            let (a, b) = (cycle[(i + m - 1) % m], cycle[i]);
            let (u, v) = (nodes[a.1] - nodes[a.0], nodes[b.1] - nodes[b.0]);
            u.cross(v).abs() > 1e-9 * u.len() * v.len() || u.dot(v) < 0.0
        })
        .map(|i| nodes[cycle[i].0])
        .collect();
    let k = corners.len();
    if k < 3 {
        return Err(WallsLoopError::Open(nodes[first.0]));
    }
    Ok((0..k).map(|i| (corners[i], corners[(i + 1) % k])).collect())
}

/// Loop from the axes of the selected straight walls with offset `offset` to the left of the loop direction
/// (for a counter-clockwise loop a negative offset points inward). Walls in any order and direction may
/// overshoot a corner or abut as a T: axes intersect, tails are trimmed.
pub fn walls_loop(axes: &[(V2, V2)], offset: f64) -> Result<Vec<SketchCurve>, WallsLoopError> {
    if axes.len() < 3 {
        return Err(WallsLoopError::TooFew);
    }
    let order = match axes_cycle(axes, 0.0) {
        Ok(o) => o,
        Err(e) => axes_cycle(axes, AXIS_REACH).map_err(|_| e)?,
    };
    let n = order.len();
    let ccw = order.iter().map(|(a, b)| a.cross(*b)).sum::<f64>() > 0.0;
    // a positive offset moves "outward" regardless of traversal direction
    let off = if ccw { -offset } else { offset };
    let lines: Vec<(V2, V2)> = order
        .iter()
        .map(|&(a, b)| {
            let d = b - a;
            let l = d.len();
            let nl = v2(-d.y / l, d.x / l);
            (a + nl * off, d * (1.0 / l))
        })
        .collect();
    let meet = |(p1, d1): (V2, V2), (p2, d2): (V2, V2)| -> V2 {
        let den = d1.cross(d2);
        if den.abs() < 1e-12 {
            p2 // collinear: a joint without a kink
        } else {
            p1 + d1 * ((p2 - p1).cross(d2) / den)
        }
    };
    let pts: Vec<V2> = (0..n)
        .map(|i| meet(lines[(i + n - 1) % n], lines[i]))
        .collect();
    if (0..n).any(|i| (pts[(i + 1) % n] - pts[i]).dot(lines[i].1) <= 1e-9) {
        return Err(WallsLoopError::OffsetFlips);
    }
    Ok((0..n)
        .map(|i| SketchCurve::Line {
            a: pts[i],
            b: pts[(i + 1) % n],
        })
        .collect())
}

/// `walls_loop` without the failure reason; `None` means the walls do not form a loop.
pub fn pick_walls(axes: &[(V2, V2)], offset: f64) -> Option<Vec<SketchCurve>> {
    walls_loop(axes, offset).ok()
}

/// Wall profile in elevation: a polygon (u along the axis from the start, z above the wall bottom) with
/// holes. The wall is not rectangular; the volume of a straight wall = profile area × thickness.
#[derive(Clone, Debug, PartialEq)]
pub struct ElevationProfile {
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
}

impl ElevationProfile {
    pub fn rectangle(len: f64, h: f64) -> Self {
        Self {
            outer: vec![(0.0, 0.0), (len, 0.0), (len, h), (0.0, h)],
            holes: vec![],
        }
    }
    fn ring(r: &[(f64, f64)]) -> Vec<V2> {
        r.iter().map(|&(u, z)| v2(u, z)).collect()
    }
    pub fn area(&self) -> f64 {
        crate::poly::area(&Self::ring(&self.outer)).abs()
            - self
                .holes
                .iter()
                .map(|h| crate::poly::area(&Self::ring(h)).abs())
                .sum::<f64>()
    }
    pub fn volume(&self, thickness: f64) -> f64 {
        self.area() * thickness
    }
    /// Check: ≥3 points, no self-intersections, holes inside.
    pub fn validate(&self) -> Result<(), SketchIssue> {
        let mut sk = Sketch::default();
        for r in std::iter::once(&self.outer).chain(&self.holes) {
            let n = r.len();
            if n < 3 {
                return Err(SketchIssue::Empty);
            }
            for i in 0..n {
                sk.curves.push(SketchCurve::Line {
                    a: v2(r[i].0, r[i].1),
                    b: v2(r[(i + 1) % n].0, r[(i + 1) % n].1),
                });
            }
        }
        match sk.validate() {
            Ok(rs) if rs.len() == 1 => Ok(()),
            Ok(_) => Err(SketchIssue::Nested { curve: 0 }),
            Err(mut e) => Err(e.remove(0)),
        }
    }
    /// Solid intervals (z_bottom, z_top) along the vertical at point u.
    pub fn spans_at(&self, u: f64) -> Vec<(f64, f64)> {
        let mut zs = vec![];
        for r in std::iter::once(&self.outer).chain(&self.holes) {
            let n = r.len();
            for i in 0..n {
                let (p, q) = (r[i], r[(i + 1) % n]);
                if (p.0 <= u) != (q.0 <= u) {
                    zs.push(p.1 + (q.1 - p.1) * (u - p.0) / (q.0 - p.0));
                }
            }
        }
        zs.sort_by(f64::total_cmp);
        zs.chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0], c[1]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ax: f64, ay: f64, bx: f64, by: f64) -> SketchCurve {
        SketchCurve::Line {
            a: v2(ax, ay),
            b: v2(bx, by),
        }
    }

    #[test]
    fn rectangle_with_semicircle_and_hole_exact_area() {
        // 6×4 + a semicircle r=2 above the top, the hole is a 1×0.5 ellipse
        let sk = Sketch {
            curves: vec![
                line(6.0, 0.0, 6.0, 4.0),
                line(0.0, 0.0, 6.0, 0.0), // arbitrary order
                line(0.0, 4.0, 0.0, 0.0),
                SketchCurve::arc3(v2(6.0, 4.0), v2(4.0, 6.0), v2(2.0, 4.0)).unwrap(),
                line(2.0, 4.0, 0.0, 4.0),
                SketchCurve::Ellipse {
                    c: v2(3.0, 2.0),
                    rx: 1.0,
                    ry: 0.5,
                    rot: 0.3,
                    t0: 0.0,
                    sweep: 2.0 * PI,
                },
            ],
        };
        let rs = sk.validate().unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].holes.len(), 1);
        let want = 24.0 + PI * 4.0 / 2.0 - PI * 0.5;
        assert!((rs[0].area() - want).abs() < 1e-12, "{}", rs[0].area());
        // tessellation: points lie on the curves, the area converges to the exact value
        let rings = rs[0].rings(1e-5);
        let a: f64 = rings.iter().map(|r| crate::poly::area(r)).sum();
        assert!((a - want).abs() < 1e-3, "{a}");
    }

    #[test]
    fn open_loop_and_crossing_are_highlighted() {
        let sk = Sketch {
            curves: vec![
                line(0.0, 0.0, 4.0, 0.0),
                line(4.0, 0.0, 4.0, 3.0),
                line(4.0, 3.0, 0.0, 3.1), // does not reach the start
            ],
        };
        let e = sk.validate().unwrap_err();
        assert!(e
            .iter()
            .any(|i| matches!(i, SketchIssue::OpenEnd { curve: 2, .. })));
        assert!(e
            .iter()
            .any(|i| matches!(i, SketchIssue::OpenEnd { curve: 0, .. })));
        // bow-tie
        let sk = Sketch {
            curves: vec![
                line(0.0, 0.0, 4.0, 4.0),
                line(4.0, 4.0, 4.0, 0.0),
                line(4.0, 0.0, 0.0, 4.0),
                line(0.0, 4.0, 0.0, 0.0),
            ],
        };
        let e = sk.validate().unwrap_err();
        assert_eq!(
            e,
            vec![SketchIssue::Intersect {
                a: 0,
                b: 2,
                at: v2(2.0, 2.0)
            }]
        );
        assert_eq!(e[0].curves(), vec![0, 2]);
    }

    #[test]
    fn spline_area_exact_and_closed_spline_loop() {
        // spline "lid" over a segment: the area under a cubic B-spline = ∫y dx exactly
        let ctrl = vec![v2(0.0, 0.0), v2(1.0, 2.0), v2(3.0, 2.0), v2(4.0, 0.0)];
        let sk = Sketch {
            curves: vec![
                SketchCurve::Spline { ctrl: ctrl.clone() },
                line(4.0, 0.0, 0.0, 0.0),
            ],
        };
        let rs = sk.validate().unwrap();
        let dense: Vec<V2> = (0..=20000)
            .map(|i| SketchCurve::Spline { ctrl: ctrl.clone() }.point(i as f64 / 20000.0))
            .collect();
        let a_dense = crate::poly::area(&dense).abs();
        assert!(
            (rs[0].area() - a_dense).abs() < 1e-7,
            "{} {}",
            rs[0].area(),
            a_dense
        );
        // a single span is a Bézier: ∫y·x' dt = ∫(6t−6t²)(3+6t−6t²) dt = 4.2
        assert!((rs[0].area() - 4.2).abs() < 1e-12, "{}", rs[0].area());
        // multi-span spline: endpoints are interpolated, tessellation points lie on the curve
        let s = SketchCurve::Spline {
            ctrl: vec![
                v2(0.0, 0.0),
                v2(1.0, 1.0),
                v2(2.0, -1.0),
                v2(3.0, 1.0),
                v2(4.0, 0.0),
                v2(5.0, 0.0),
            ],
        };
        assert_eq!(s.point(1.0), v2(5.0, 0.0));
        assert_eq!(s.point(0.0), v2(0.0, 0.0));
        let t = s.tessellate(1e-4);
        assert!(t.len() > 16);
    }

    #[test]
    fn ellipse_and_arc_lengths() {
        let a = SketchCurve::Arc {
            c: v2(0.0, 0.0),
            r: 2.0,
            a0: 0.0,
            sweep: PI,
        };
        assert_eq!(a.length(), 2.0 * PI);
        assert!((a.end() - v2(-2.0, 0.0)).len() < 1e-15);
        let e = SketchCurve::Ellipse {
            c: v2(0.0, 0.0),
            rx: 3.0,
            ry: 3.0,
            rot: 0.0,
            t0: 0.0,
            sweep: 2.0 * PI,
        };
        assert!((e.length() - 6.0 * PI).abs() < 1e-10);
        assert!((e.area_term() - 9.0 * PI).abs() < 1e-12);
        for p in e.tessellate(1e-3) {
            assert!((p.len() - 3.0).abs() < 1e-12);
        }
    }

    #[test]
    fn pick_walls_offset_outward_and_inward() {
        // walls in random order and varied directions
        let axes = [
            (v2(10.0, 0.0), v2(10.0, 6.0)),
            (v2(10.0, 0.0), v2(0.0, 0.0)),
            (v2(0.0, 6.0), v2(0.0, 0.0)),
            (v2(10.0, 6.0), v2(0.0, 6.0)),
        ];
        let out = pick_walls(&axes, 0.15).unwrap();
        let sk = Sketch { curves: out };
        let a = sk.validate().unwrap()[0].area();
        assert!((a - 10.3 * 6.3).abs() < 1e-12, "{a}");
        let inn = pick_walls(&axes, -0.15).unwrap();
        let a = Sketch { curves: inn }.validate().unwrap()[0].area();
        assert!((a - 9.7 * 5.7).abs() < 1e-12);
        assert!(pick_walls(&axes[..3], 0.0).is_none());
    }

    fn ring_area(cs: Vec<SketchCurve>) -> f64 {
        Sketch { curves: cs }.validate().unwrap()[0].area()
    }

    #[test]
    fn pick_walls_trims_at_t_junctions() {
        // the bottom continues past both walls, the top extends past the right wall
        let axes = [
            (v2(-3.0, 0.0), v2(9.0, 0.0)),
            (v2(6.0, 0.0), v2(6.0, 4.0)),
            (v2(8.0, 4.0), v2(0.0, 4.0)),
            (v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        assert!((ring_area(pick_walls(&axes, 0.0).unwrap()) - 24.0).abs() < 1e-9);
        // one side made of two walls: no extra vertex
        let split = [
            (v2(0.0, 0.0), v2(3.0, 0.0)),
            (v2(3.0, 0.0), v2(6.0, 0.0)),
            (v2(6.0, 0.0), v2(6.0, 4.0)),
            (v2(6.0, 4.0), v2(0.0, 4.0)),
            (v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        let cs = pick_walls(&split, 0.0).unwrap();
        assert_eq!(cs.len(), 4);
        assert!((ring_area(cs) - 24.0).abs() < 1e-9);
        // abutting a face: the end does not reach the neighbouring wall's axis
        let gap = [
            (v2(0.0, 0.0), v2(6.0, 0.0)),
            (v2(6.0, 0.1), v2(6.0, 4.0)),
            (v2(6.0, 4.0), v2(0.0, 4.0)),
            (v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        assert!((ring_area(pick_walls(&gap, 0.0).unwrap()) - 24.0).abs() < 1e-9);
    }

    #[test]
    fn pick_walls_reports_the_real_cause() {
        let rect = [
            (v2(0.0, 0.0), v2(6.0, 0.0)),
            (v2(6.0, 0.0), v2(6.0, 4.0)),
            (v2(6.0, 4.0), v2(0.0, 4.0)),
            (v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        assert_eq!(
            walls_loop(&rect[..2], 0.0).unwrap_err(),
            WallsLoopError::TooFew
        );
        assert!(matches!(
            walls_loop(&rect[..3], 0.0),
            Err(WallsLoopError::Open(_))
        ));
        // a partition across the whole loop: two loops, the outline is ambiguous
        let mut cross = rect.to_vec();
        cross.push((v2(3.0, 0.0), v2(3.0, 4.0)));
        assert!(matches!(
            walls_loop(&cross, 0.0),
            Err(WallsLoopError::Branch(_))
        ));
        // an extra wall off to the side
        let mut far = rect.to_vec();
        far.push((v2(20.0, 0.0), v2(25.0, 0.0)));
        assert_eq!(walls_loop(&far, 0.0).unwrap_err(), WallsLoopError::Stray(4));
    }

    #[test]
    fn inward_offset_never_flips_silently() {
        let room = [
            (v2(0.0, 0.0), v2(3.0, 0.0)),
            (v2(3.0, 0.0), v2(3.0, 3.0)),
            (v2(3.0, 3.0), v2(0.0, 3.0)),
            (v2(0.0, 3.0), v2(0.0, 0.0)),
        ];
        assert!((ring_area(walls_loop(&room, -1.0).unwrap()) - 1.0).abs() < 1e-9);
        assert_eq!(
            walls_loop(&room, -2.0).unwrap_err(),
            WallsLoopError::OffsetFlips
        );
        assert_eq!(
            walls_loop(&room, -1.5).unwrap_err(),
            WallsLoopError::OffsetFlips
        );
    }

    #[test]
    fn gable_wall_profile_area_and_spans() {
        // gable wall 8 m, eave 3, ridge 5, window 1×1.5
        let p = ElevationProfile {
            outer: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 3.0), (4.0, 5.0), (0.0, 3.0)],
            holes: vec![vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.5), (1.0, 2.5)]],
        };
        p.validate().unwrap();
        assert!((p.area() - (24.0 + 8.0 - 1.5)).abs() < 1e-12);
        assert!((p.volume(0.25) - 30.5 * 0.25).abs() < 1e-12);
        assert_eq!(p.spans_at(2.0), vec![(0.0, 4.0)]);
        assert_eq!(p.spans_at(1.5), vec![(0.0, 1.0), (2.5, 3.75)]);
        let bad = ElevationProfile {
            outer: vec![(0.0, 0.0), (4.0, 4.0), (4.0, 0.0), (0.0, 4.0)],
            holes: vec![],
        };
        assert!(matches!(bad.validate(), Err(SketchIssue::Intersect { .. })));
    }

    fn circ(r: f64) -> SketchCurve {
        SketchCurve::Ellipse {
            c: v2(0.0, 0.0),
            rx: r,
            ry: r,
            rot: 0.0,
            t0: 0.0,
            sweep: 2.0 * std::f64::consts::PI,
        }
    }

    #[test]
    fn island_in_hole_is_its_own_region() {
        let sk = Sketch {
            curves: vec![circ(3.0), circ(2.0), circ(1.0)],
        };
        let rs = sk.validate().unwrap();
        assert_eq!(rs.len(), 2);
        let pi = std::f64::consts::PI;
        let a: f64 = rs.iter().map(|r| r.area()).sum();
        assert!((a - (9.0 - 4.0 + 1.0) * pi).abs() < 1e-9, "{a}");
    }

    #[test]
    fn edges_shorter_than_min_edge_are_flagged() {
        let tiny = Sketch {
            curves: vec![
                SketchCurve::Line {
                    a: v2(0.0, 0.0),
                    b: v2(0.0005, 0.0),
                },
                SketchCurve::Line {
                    a: v2(0.0005, 0.0),
                    b: v2(0.0, 1.0),
                },
                SketchCurve::Line {
                    a: v2(0.0, 1.0),
                    b: v2(0.0, 0.0),
                },
            ],
        };
        let e = tiny.validate().unwrap_err();
        assert_eq!(e, vec![SketchIssue::Degenerate { curve: 0 }]);
        assert!(tiny.validate_with(1e-4).is_ok());
    }

    #[test]
    fn flat_ellipse_keeps_sag_beyond_old_cap() {
        let e = SketchCurve::Ellipse {
            c: v2(0.0, 0.0),
            rx: 100.0,
            ry: 0.05,
            rot: 0.0,
            t0: 0.0,
            sweep: 2.0 * std::f64::consts::PI,
        };
        let sag = 1e-5;
        let pts = e.tessellate(sag);
        assert!(pts.len() > 4096);
        // sagitta of each chord: from the chord midpoint to the curve by parameter
        let n = pts.len();
        for k in (0..n).step_by(97) {
            let (a, b) = (pts[k], pts[(k + 1) % n]);
            let m = e.point((k as f64 + 0.5) / n as f64);
            let d = (b - a).cross(m - a).abs() / (b - a).len().max(1e-15);
            assert!(d <= sag * 1.01, "{k}: {d}");
        }
    }

    /// Grid: a 200-tooth sawtooth with no false intersections; an added diagonal gives exactly one.
    #[test]
    fn grid_finds_crossings_in_dense_sketch() {
        let n = 200;
        let mut pts: Vec<V2> = (0..=n)
            .map(|i| v2(i as f64 * 0.05, if i % 2 == 0 { 0.0 } else { 0.03 }))
            .collect();
        pts.push(v2(n as f64 * 0.05, -1.0));
        pts.push(v2(0.0, -1.0));
        let mut sk = Sketch::default();
        for w in 0..pts.len() {
            let (a, b) = (pts[w], pts[(w + 1) % pts.len()]);
            sk.curves.push(SketchCurve::Line { a, b });
        }
        assert_eq!(sk.validate().map(|r| r.len()).ok(), Some(1));
        sk.curves.push(SketchCurve::Line {
            a: v2(1.01, 0.5),
            b: v2(1.03, -0.5),
        });
        let hits = sk.intersections();
        assert_eq!(hits.len(), 1, "{hits:?}");
    }
}
