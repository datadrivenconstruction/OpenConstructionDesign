// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sections and elevations: exact section of vertical prisms by the view plane and the
//! visible edges of the projection (exact hidden-line removal for planar faces).
//!
//! View frame: origin `o`, unit axis `u` along the view line, look direction `n` is the left normal of `u`.
//! Drawing coordinates (s, z): s = (p − o)·u; depth d = (p − o)·n ≥ 0 is in front of the viewer.

use std::collections::HashMap;

use crate::layered::par_map;
use crate::math::{v2, V2};

/// Depth tolerance: an edge is hidden only if it lies farther than the face by more than this, in metres.
const DEPTH_EPS: f64 = 1e-6;
/// A face counts as front-facing when |cos| exceeds this (otherwise it is seen edge-on): at a smaller
/// tilt the face depth along s ~ 1/cos is numerical noise.
const FACING_EPS: f64 = 1e-6;

/// Elevation as a function of plan position: z = z + grad·(p − o) + polyline(u), u = (p − o)·dir.
/// The polyline is points (u, h) with increasing u; equal u make a step; beyond the ends it is horizontal.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Zfn {
    pub o: V2,
    pub z: f64,
    pub grad: V2,
    pub dir: V2,
    pub prof: Vec<(f64, f64)>,
}

impl Zfn {
    pub fn flat(z: f64) -> Self {
        Zfn {
            z,
            ..Default::default()
        }
    }
    fn u(&self, p: V2) -> f64 {
        (p - self.o).dot(self.dir)
    }
    /// Value at `p` on the polyline segment containing point `mid` (at a kink, the column's segment).
    fn at(&self, p: V2, mid: V2) -> f64 {
        let (h, k) = seg(&self.prof, self.u(mid));
        self.z + self.grad.dot(p - self.o) + h + k * (self.u(p) - self.u(mid))
    }
    fn gradient(&self, mid: V2) -> V2 {
        self.grad + self.dir * seg(&self.prof, self.u(mid)).1
    }
}

/// Polyline value and slope at `u` (inside a segment).
fn seg(prof: &[(f64, f64)], u: f64) -> (f64, f64) {
    let (Some(f), Some(l)) = (prof.first(), prof.last()) else {
        return (0.0, 0.0);
    };
    if u <= f.0 {
        return (f.1, 0.0);
    }
    if u >= l.0 {
        return (l.1, 0.0);
    }
    for w in prof.windows(2) {
        let (p, q) = (w[0], w[1]);
        if u >= p.0 && u <= q.0 && q.0 > p.0 {
            let k = (q.1 - p.1) / (q.0 - p.0);
            return (p.1 + k * (u - p.0), k);
        }
    }
    (l.1, 0.0)
}

/// Through void (opening): u = (p − o)·dir ∈ [u0, u1], z ∈ [z0, z1], through the full thickness.
#[derive(Clone, Debug, PartialEq)]
pub struct Void {
    pub o: V2,
    pub dir: V2,
    pub u0: f64,
    pub u1: f64,
    pub z0: f64,
    pub z1: f64,
}

/// Void in an arc wall: a wedge from centre `c`, counter-clockwise from ray `r0` to ray `r1`
/// (unit vectors, angle less than π), z ∈ [z0, z1], through the full thickness.
#[derive(Clone, Debug, PartialEq)]
pub struct ArcVoid {
    pub c: V2,
    pub r0: V2,
    pub r1: V2,
    pub z0: f64,
    pub z1: f64,
}

/// Void as the intersection of two vertical half-spaces (p − o)·m > 0, with m a unit vector.
#[derive(Clone, Copy, Debug)]
struct Cutout {
    pl: [(V2, V2); 2],
    z0: f64,
    z1: f64,
}

impl Cutout {
    fn g(&self, k: usize, p: V2) -> f64 {
        (p - self.pl[k].0).dot(self.pl[k].1)
    }
    fn inside(&self, p: V2) -> bool {
        self.g(0, p) > 0.0 && self.g(1, p) > 0.0
    }
}

fn cutouts(pr: &Prism, arcs: &[ArcVoid]) -> Vec<Cutout> {
    let unit = |v: V2| v * (1.0 / v.len());
    let lin = pr.voids.iter().map(|v| {
        let d = unit(v.dir);
        Cutout {
            pl: [(v.o + d * v.u0, d), (v.o + d * v.u1, -d)],
            z0: v.z0,
            z1: v.z1,
        }
    });
    let arc = arcs.iter().map(|a| {
        let (r0, r1) = (unit(a.r0), unit(a.r1));
        Cutout {
            pl: [(a.c, v2(-r0.y, r0.x)), (a.c, v2(r1.y, -r1.x))],
            z0: a.z0,
            z1: a.z1,
        }
    });
    lin.chain(arc).collect()
}

/// Vertical prism: plan loops (outer counter-clockwise, holes clockwise),
/// bottom and top, voids.
#[derive(Clone, Debug, PartialEq)]
pub struct Prism {
    pub rings: Vec<Vec<V2>>,
    pub bottom: Zfn,
    pub top: Zfn,
    pub voids: Vec<Void>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub o: V2,
    /// Unit vector of the view line.
    pub u: V2,
    /// Far limit (depth); None means no clipping.
    pub far: Option<f64>,
    /// Section: cut loops are hatched (not projection edges); an elevation yields edges.
    pub cut: bool,
}

impl Frame {
    pub fn n(&self) -> V2 {
        v2(-self.u.y, self.u.x)
    }
    fn s(&self, p: V2) -> f64 {
        (p - self.o).dot(self.u)
    }
    fn d(&self, p: V2) -> f64 {
        (p - self.o).dot(self.n())
    }
}

/// Section of prism `item`: loops in (s, z), outer counter-clockwise, holes clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct CutRegion {
    pub item: usize,
    pub rings: Vec<Vec<V2>>,
}

/// Visible segment of an edge of prism `item` in (s, z).
#[derive(Clone, Debug, PartialEq)]
pub struct Line2 {
    pub item: usize,
    pub a: V2,
    pub b: V2,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SectionOut {
    pub cuts: Vec<CutRegion>,
    pub lines: Vec<Line2>,
}

/// Intervals of parameter t of the line o + u·t inside a polygon (even-odd rule, half-open).
pub fn line_intervals(rings: &[Vec<V2>], o: V2, u: V2) -> Vec<(f64, f64)> {
    let n = v2(-u.y, u.x);
    let mut xs = vec![];
    for r in rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            let (dp, dq) = ((p - o).dot(n), (q - o).dot(n));
            if (dp > 0.0) != (dq > 0.0) {
                let (sp, sq) = ((p - o).dot(u), (q - o).dot(u));
                xs.push(sp + (sq - sp) * dp / (dp - dq));
            }
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.chunks_exact(2)
        .filter(|c| c[1] - c[0] > 1e-12)
        .map(|c| (c[0], c[1]))
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum B {
    Bot,
    Top,
    C(f64),
}

/// Region {(t, z): t ∈ [t0, t1], bottom ≤ z ≤ top, outside voids} above points p(t) = p0 + e·t.
/// Columns between kinks and void boundaries, trapezoids within a column; loops are built by chaining edges.
fn region(pr: &Prism, voids: &[Cutout], p0: V2, e: V2, t0: f64, t1: f64) -> Vec<Vec<V2>> {
    let at = |t: f64| p0 + e * t;
    let mut ts = vec![t0, t1];
    let add_u = |o: V2, dir: V2, u: f64, ts: &mut Vec<f64>| {
        let k = dir.dot(e);
        if k.abs() > 1e-12 {
            let t = (u - (p0 - o).dot(dir)) / k;
            if t > t0 && t < t1 {
                ts.push(t);
            }
        }
    };
    for z in [&pr.bottom, &pr.top] {
        for q in &z.prof {
            add_u(z.o, z.dir, q.0, &mut ts);
        }
    }
    for v in voids {
        for (o, m) in v.pl {
            add_u(o, m, 0.0, &mut ts);
        }
    }
    let dedup = |ts: &mut Vec<f64>| {
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    };
    dedup(&mut ts);
    let active = |pm: V2| -> Vec<(f64, f64)> {
        voids
            .iter()
            .filter(|v| v.inside(pm))
            .map(|v| (v.z0, v.z1))
            .collect()
    };
    // Intersections of bottom/top with void elevations split columns: the boundary order within a column is constant.
    let mut extra = vec![];
    for w in ts.windows(2) {
        let (ta, tb) = (w[0], w[1]);
        let pm = at((ta + tb) / 2.0);
        for (z0, z1) in active(pm) {
            for f in [&pr.bottom, &pr.top] {
                let (fa, fb) = (f.at(at(ta), pm), f.at(at(tb), pm));
                for c in [z0, z1] {
                    if (fa - c) * (fb - c) < 0.0 {
                        extra.push(ta + (tb - ta) * (c - fa) / (fb - fa));
                    }
                }
            }
        }
    }
    ts.extend(extra);
    dedup(&mut ts);
    // Column bands: (lo_a, hi_a, lo_b, hi_b).
    let mut cols: Vec<Vec<[f64; 4]>> = vec![];
    for w in ts.windows(2) {
        let (ta, tb) = (w[0], w[1]);
        let pm = at((ta + tb) / 2.0);
        let ev = |b: B, p: V2| match b {
            B::Bot => pr.bottom.at(p, pm),
            B::Top => pr.top.at(p, pm),
            B::C(v) => v,
        };
        let mut bands = vec![(B::Bot, B::Top)];
        for (z0, z1) in active(pm) {
            let mut next = vec![];
            for (lo, hi) in bands {
                let (lm, hm) = (ev(lo, pm), ev(hi, pm));
                if z1 <= lm || z0 >= hm {
                    next.push((lo, hi));
                    continue;
                }
                if z0 > lm {
                    next.push((lo, B::C(z0)));
                }
                if z1 < hm {
                    next.push((B::C(z1), hi));
                }
            }
            bands = next;
        }
        let mut out = vec![];
        for (lo, hi) in bands {
            if ev(hi, pm) - ev(lo, pm) <= 1e-12 {
                continue;
            }
            let (pa, pb) = (at(ta), at(tb));
            let (la, lb) = (ev(lo, pa), ev(lo, pb));
            out.push([la, ev(hi, pa).max(la), lb, ev(hi, pb).max(lb)]);
        }
        out.sort_by(|a, b| a[0].total_cmp(&b[0]));
        cols.push(out);
    }
    let mut edges: Vec<(V2, V2)> = vec![];
    for (k, w) in ts.windows(2).enumerate() {
        let (ta, tb) = (w[0], w[1]);
        for b in &cols[k] {
            edges.push((v2(ta, b[0]), v2(tb, b[2])));
            edges.push((v2(tb, b[3]), v2(ta, b[1])));
        }
    }
    for (k, &t) in ts.iter().enumerate() {
        let left: Vec<(f64, f64)> = if k > 0 {
            cols[k - 1].iter().map(|b| (b[2], b[3])).collect()
        } else {
            vec![]
        };
        let right: Vec<(f64, f64)> = cols
            .get(k)
            .map(|c| c.iter().map(|b| (b[0], b[1])).collect())
            .unwrap_or_default();
        for (lo, hi) in diff(&left, &right) {
            edges.push((v2(t, lo), v2(t, hi)));
        }
        for (lo, hi) in diff(&right, &left) {
            edges.push((v2(t, hi), v2(t, lo)));
        }
    }
    chain(edges)
}

fn merge(mut v: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = vec![];
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 + 1e-12 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// a \ b for unions of intervals; pieces shorter than 1e-9 are dropped.
fn diff(a: &[(f64, f64)], b: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let b = merge(b.to_vec());
    let mut out = vec![];
    for &(lo, hi) in &merge(a.to_vec()) {
        let mut cur = lo;
        for &(c, d) in &b {
            if d <= cur || c >= hi {
                continue;
            }
            if c > cur {
                out.push((cur, c));
            }
            cur = cur.max(d);
        }
        if hi > cur {
            out.push((cur, hi));
        }
    }
    out.into_iter().filter(|(a, b)| b - a > 1e-9).collect()
}

fn key(p: V2) -> (i64, i64) {
    ((p.x * 1e9).round() as i64, (p.y * 1e9).round() as i64)
}

/// Chains directed edges into closed loops; collinear vertices are removed.
fn chain(edges: Vec<(V2, V2)>) -> Vec<Vec<V2>> {
    let edges: Vec<(V2, V2)> = edges
        .into_iter()
        .filter(|(a, b)| key(*a) != key(*b))
        .collect();
    let mut from: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        from.entry(key(e.0)).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut rings = vec![];
    for s in 0..edges.len() {
        if used[s] {
            continue;
        }
        let start = key(edges[s].0);
        let mut ring = vec![];
        let mut cur = s;
        let mut closed = false;
        loop {
            used[cur] = true;
            ring.push(edges[cur].0);
            let end = key(edges[cur].1);
            if end == start {
                closed = true;
                break;
            }
            let Some(&next) = from.get(&end).and_then(|v| v.iter().find(|&&j| !used[j])) else {
                break;
            };
            cur = next;
        }
        if closed {
            let r = simplify(ring);
            if r.len() >= 3 {
                rings.push(r);
            }
        }
    }
    rings
}

fn simplify(mut r: Vec<V2>) -> Vec<V2> {
    let mut changed = true;
    while changed && r.len() >= 3 {
        changed = false;
        let n = r.len();
        for i in 0..n {
            let (p, c, q) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
            let (a, b) = (c - p, q - c);
            if a.cross(b).abs() <= 1e-12 * (a.len() * b.len()).max(1e-300) && a.dot(b) > 0.0 {
                r.remove(i);
                changed = true;
                break;
            }
        }
    }
    r
}

pub fn signed_area(r: &[V2]) -> f64 {
    let mut s = 0.0;
    for i in 0..r.len() {
        s += r[i].cross(r[(i + 1) % r.len()]);
    }
    s / 2.0
}

/// Front-facing planar face in (s, z): loops, a "draw edge" flag per vertex (the edge to the
/// next one), depth d = c0 + c1·s + c2·z.
struct Face {
    rings: Vec<Vec<(V2, bool)>>,
    dep: [f64; 3],
    item: usize,
    bb: [f64; 4],
    dmin: f64,
}

impl Face {
    fn depth(&self, p: V2) -> f64 {
        self.dep[0] + self.dep[1] * p.x + self.dep[2] * p.y
    }
    fn finish(mut self) -> Option<Face> {
        self.rings.retain(|r| r.len() >= 3);
        if self.rings.is_empty() {
            return None;
        }
        let mut bb = [f64::INFINITY, f64::INFINITY, -f64::INFINITY, -f64::INFINITY];
        let mut dmin = f64::INFINITY;
        for r in &self.rings {
            for (p, _) in r {
                bb = [
                    bb[0].min(p.x),
                    bb[1].min(p.y),
                    bb[2].max(p.x),
                    bb[3].max(p.y),
                ];
                dmin = dmin.min(self.depth(*p));
            }
        }
        self.bb = bb;
        self.dmin = dmin;
        Some(self)
    }
    /// Keeps the part where f ≥ 0; edges along the clip line are not drawn.
    fn clip(&mut self, f: impl Fn(&Face, V2) -> f64) {
        let mut rings = vec![];
        for r in &self.rings {
            let mut out = vec![];
            for i in 0..r.len() {
                let ((p, fl), (q, _)) = (r[i], r[(i + 1) % r.len()]);
                let (fp, fq) = (f(self, p), f(self, q));
                let x = |fp: f64, fq: f64| p + (q - p) * (fp / (fp - fq));
                if fp >= 0.0 {
                    out.push((p, fl));
                    if fq < 0.0 {
                        out.push((x(fp, fq), false));
                    }
                } else if fq >= 0.0 {
                    out.push((x(fp, fq), fl));
                }
            }
            rings.push(out);
        }
        self.rings = rings;
    }
    fn on_boundary(&self, p: V2) -> bool {
        self.rings.iter().any(|r| {
            (0..r.len()).any(|i| {
                let (a, b) = (r[i].0, r[(i + 1) % r.len()].0);
                let e = b - a;
                let l = e.len();
                l > 0.0 && ((p - a).cross(e) / l).abs() <= 1e-9 && {
                    let t = (p - a).dot(e) / (l * l);
                    (0.0..=1.0).contains(&t)
                }
            })
        })
    }
    fn contains(&self, p: V2) -> bool {
        let mut inside = false;
        for r in &self.rings {
            for i in 0..r.len() {
                let (a, b) = (r[i].0, r[(i + 1) % r.len()].0);
                if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (b.x - a.x) * (p.y - a.y) / (b.y - a.y)
                {
                    inside = !inside;
                }
            }
        }
        inside
    }
}

fn plain(rings: Vec<Vec<V2>>, draw: bool) -> Vec<Vec<(V2, bool)>> {
    rings
        .into_iter()
        .map(|r| r.into_iter().map(|p| (p, draw)).collect())
        .collect()
}

/// Sutherland–Hodgman clipping of plan loops by the half-plane (p − o)·d ≥ c.
fn clip_plan(rings: &[Vec<V2>], o: V2, d: V2, c: f64) -> Vec<Vec<V2>> {
    rings
        .iter()
        .map(|r| {
            let f = |p: V2| (p - o).dot(d) - c;
            let mut out = vec![];
            for i in 0..r.len() {
                let (p, q) = (r[i], r[(i + 1) % r.len()]);
                let (fp, fq) = (f(p), f(q));
                if fp >= 0.0 {
                    out.push(p);
                }
                if (fp >= 0.0) != (fq >= 0.0) {
                    out.push(p + (q - p) * (fp / (fp - fq)));
                }
            }
            out
        })
        .filter(|r| r.len() >= 3)
        .collect()
}

fn faces_of(pr: &Prism, voids: &[Cutout], item: usize, f: &Frame) -> Vec<Face> {
    let (u, n) = (f.u, f.n());
    let mut out = vec![];
    let face = |rings, dep, item| Face {
        rings,
        dep,
        item,
        bb: [0.0; 4],
        dmin: 0.0,
    };
    // Side faces: along loop edges, with void cut-outs.
    for r in &pr.rings {
        for i in 0..r.len() {
            let (p, q) = (r[i], r[(i + 1) % r.len()]);
            let l = (q - p).len();
            if l <= 1e-12 {
                continue;
            }
            let e = (q - p) * (1.0 / l);
            let eu = e.dot(u);
            if eu <= FACING_EPS {
                continue;
            }
            let (sp, dp) = (f.s(p), f.d(p));
            let rs: Vec<Vec<V2>> = region(pr, voids, p, e, 0.0, l)
                .into_iter()
                .map(|r| r.into_iter().map(|t| v2(sp + t.x * eu, t.y)).collect())
                .collect();
            let c1 = e.dot(n) / eu;
            out.push(face(plain(rs, true), [dp - sp * c1, c1, 0.0], item));
        }
    }
    // Void reveals: boundary planes (m points into the void), limited by the second plane.
    for v in voids {
        for k in 0..2 {
            let (b, m) = v.pl[k];
            if m.dot(n) >= -FACING_EPS {
                continue;
            }
            let w = if k == 0 { v2(-m.y, m.x) } else { v2(m.y, -m.x) };
            let other = |t: f64| v.g(1 - k, b + w * t);
            for (mut w0, mut w1) in line_intervals(&pr.rings, b, w) {
                let (g0, g1) = (other(w0), other(w1));
                if g0 <= 0.0 && g1 <= 0.0 {
                    continue;
                }
                if g0 < 0.0 {
                    w0 += (w1 - w0) * g0 / (g0 - g1);
                } else if g1 < 0.0 {
                    w1 = w0 + (w1 - w0) * g0 / (g0 - g1);
                }
                let pm = b + w * ((w0 + w1) / 2.0);
                let lo = v.z0.max(pr.bottom.at(pm, pm));
                let hi = v.z1.min(pr.top.at(pm, pm));
                if hi - lo <= 1e-12 {
                    continue;
                }
                let (pa, pb) = (b + w * w0, b + w * w1);
                let ring = vec![
                    v2(f.s(pa), lo),
                    v2(f.s(pb), lo),
                    v2(f.s(pb), hi),
                    v2(f.s(pa), hi),
                ];
                let c1 = w.dot(n) / w.dot(u);
                out.push(face(
                    plain(vec![ring], true),
                    [f.d(pa) - f.s(pa) * c1, c1, 0.0],
                    item,
                ));
            }
        }
    }
    // Top and bottom: per polyline segment; only those inclined towards the viewer are visible.
    for (zf, up) in [(&pr.top, 1.0), (&pr.bottom, -1.0)] {
        let mut us: Vec<f64> = if zf.dir.len() > 0.0 {
            zf.prof.iter().map(|q| q.0).collect()
        } else {
            vec![]
        };
        us.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
        let mut strips: Vec<(Option<f64>, Option<f64>)> = vec![];
        let mut lo = None;
        for &x in &us {
            strips.push((lo, Some(x)));
            lo = Some(x);
        }
        strips.push((lo, None));
        for (a, b) in strips {
            let um = match (a, b) {
                (Some(a), Some(b)) => (a + b) / 2.0,
                (Some(a), None) => a + 1.0,
                (None, Some(b)) => b - 1.0,
                (None, None) => 0.0,
            };
            let pm = zf.o + zf.dir * um;
            let g = zf.gradient(pm);
            let gn = g.dot(n) * up;
            if gn <= FACING_EPS * (1.0 + g.len()) {
                continue;
            }
            let mut rings = pr.rings.clone();
            if let Some(a) = a {
                rings = clip_plan(&rings, zf.o, zf.dir, a);
            }
            if let Some(b) = b {
                rings = clip_plan(&rings, zf.o, -zf.dir, -b);
            }
            if rings.is_empty() {
                continue;
            }
            let rs: Vec<Vec<V2>> = rings
                .iter()
                .map(|r| r.iter().map(|&p| v2(f.s(p), zf.at(p, pm))).collect())
                .collect();
            // z = z0 + s·(g·u) + d·(g·n)  ⇒  d = (z − z0 − s·g·u) / (g·n)
            let z0 = zf.at(f.o, pm);
            let (gu, gn) = (g.dot(u), g.dot(n));
            out.push(face(plain(rs, true), [-z0 / gn, -gu / gn, 1.0 / gn], item));
        }
    }
    out
}

/// Intervals λ ∈ [0, 1] of segment a→b hidden by face `f` (inside it and beyond its plane).
fn hidden_by(f: &Face, a: V2, b: V2, da: f64, db: f64, out: &mut Vec<(f64, f64)>) {
    let g0 = da - f.depth(a);
    let g1 = db - f.depth(b);
    let (l0, l1) = if g0 > DEPTH_EPS && g1 > DEPTH_EPS {
        (0.0, 1.0)
    } else if g0 <= DEPTH_EPS && g1 <= DEPTH_EPS {
        return;
    } else {
        let lc = (DEPTH_EPS - g0) / (g1 - g0);
        if g0 > DEPTH_EPS {
            (0.0, lc)
        } else {
            (lc, 1.0)
        }
    };
    let d = b - a;
    let mut ls = vec![0.0, 1.0];
    for r in &f.rings {
        for i in 0..r.len() {
            let (p, q) = (r[i].0, r[(i + 1) % r.len()].0);
            let e = q - p;
            let den = d.cross(e);
            if den.abs() <= 1e-18 {
                continue;
            }
            let lam = (p - a).cross(e) / den;
            let mu = (p - a).cross(d) / den;
            if (0.0..=1.0).contains(&mu) && lam > 0.0 && lam < 1.0 {
                ls.push(lam);
            }
        }
    }
    ls.sort_by(f64::total_cmp);
    for w in ls.windows(2) {
        let (x, y) = (w[0].max(l0), w[1].min(l1));
        if y - x <= 1e-12 {
            continue;
        }
        let m = a + d * ((w[0] + w[1]) / 2.0);
        // On the occluder's boundary the edge coincides with its line: it is drawn once, the nearer one.
        if f.contains(m) || f.on_boundary(m) {
            out.push((x, y));
        }
    }
}

struct Grid {
    x0: f64,
    y0: f64,
    cw: f64,
    ch: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u32>>,
}

impl Grid {
    fn new(faces: &[Face]) -> Grid {
        let mut bb = [f64::INFINITY, f64::INFINITY, -f64::INFINITY, -f64::INFINITY];
        for f in faces {
            bb = [
                bb[0].min(f.bb[0]),
                bb[1].min(f.bb[1]),
                bb[2].max(f.bb[2]),
                bb[3].max(f.bb[3]),
            ];
        }
        let (w, h) = ((bb[2] - bb[0]).max(1e-9), (bb[3] - bb[1]).max(1e-9));
        // Cell size ~ average face: views narrow along one axis do not bloat the lists or the traversal of long edges.
        let n = faces.len().max(1) as f64;
        let mw = faces.iter().map(|f| f.bb[2] - f.bb[0]).sum::<f64>() / n;
        let mh = faces.iter().map(|f| f.bb[3] - f.bb[1]).sum::<f64>() / n;
        let cells =
            |ext: f64, mean: f64| ((ext / mean.max(ext / 512.0)).ceil() as usize).clamp(1, 512);
        let (nx, ny) = (cells(w, mw), cells(h, mh));
        let mut g = Grid {
            x0: bb[0],
            y0: bb[1],
            cw: w / nx as f64,
            ch: h / ny as f64,
            nx,
            ny,
            cells: vec![vec![]; nx * ny],
        };
        for (i, f) in faces.iter().enumerate() {
            let (a, b, c, d) = g.span(f.bb);
            for y in b..=d {
                for x in a..=c {
                    g.cells[y * g.nx + x].push(i as u32);
                }
            }
        }
        g
    }
    fn span(&self, bb: [f64; 4]) -> (usize, usize, usize, usize) {
        let cx = |v: f64| (((v - self.x0) / self.cw).floor().max(0.0) as usize).min(self.nx - 1);
        let cy = |v: f64| (((v - self.y0) / self.ch).floor().max(0.0) as usize).min(self.ny - 1);
        (cx(bb[0]), cy(bb[1]), cx(bb[2]), cy(bb[3]))
    }
}

struct Edge {
    a: V2,
    b: V2,
    da: f64,
    db: f64,
    face: usize,
    item: usize,
}

/// View: sections of prisms by the plane d = 0 and visible edges in the band d ∈ [0, far].
pub fn section(prisms: &[Prism], f: &Frame) -> SectionOut {
    section_with_arcs(prisms, &[], f)
}

/// Like `section`, plus arc-wall voids: `arcs[k]` belongs to prism k (if shorter, the rest have none).
pub fn section_with_arcs(prisms: &[Prism], arcs: &[Vec<ArcVoid>], f: &Frame) -> SectionOut {
    let far = f.far.unwrap_or(f64::INFINITY);
    let per: Vec<(Vec<CutRegion>, Vec<Face>)> = par_map(
        &prisms.iter().enumerate().collect::<Vec<_>>(),
        |&(k, pr)| {
            let voids = cutouts(pr, arcs.get(k).map_or(&[][..], |a| &a[..]));
            let (mut lo, mut hi) = (f64::INFINITY, -f64::INFINITY);
            for r in &pr.rings {
                for &p in r {
                    let d = f.d(p);
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
            }
            if hi < 0.0 || lo > far {
                return (vec![], vec![]);
            }
            let mut cuts = vec![];
            for (s0, s1) in line_intervals(&pr.rings, f.o, f.u) {
                let rings = region(pr, &voids, f.o, f.u, s0, s1);
                if !rings.is_empty() {
                    cuts.push(CutRegion { item: k, rings });
                }
            }
            let mut faces = vec![];
            for mut fc in faces_of(pr, &voids, k, f) {
                if lo < 0.0 {
                    fc.clip(|fc, p| fc.depth(p));
                }
                if hi > far {
                    fc.clip(|fc, p| far - fc.depth(p));
                }
                faces.extend(fc.finish());
            }
            for c in &cuts {
                faces.extend(
                    Face {
                        rings: plain(c.rings.clone(), !f.cut),
                        dep: [0.0; 3],
                        item: k,
                        bb: [0.0; 4],
                        dmin: 0.0,
                    }
                    .finish(),
                );
            }
            (cuts, faces)
        },
    );
    let mut cuts = vec![];
    let mut faces = vec![];
    for (c, fs) in per {
        cuts.extend(c);
        faces.extend(fs);
    }
    let mut edges = vec![];
    for (i, fc) in faces.iter().enumerate() {
        for r in &fc.rings {
            for j in 0..r.len() {
                let ((a, draw), (b, _)) = (r[j], r[(j + 1) % r.len()]);
                if draw && (b - a).len() > 1e-12 {
                    edges.push(Edge {
                        a,
                        b,
                        da: fc.depth(a),
                        db: fc.depth(b),
                        face: i,
                        item: fc.item,
                    });
                }
            }
        }
    }
    drop_seams(&mut edges, &faces);
    if faces.is_empty() {
        return SectionOut {
            cuts,
            lines: vec![],
        };
    }
    let grid = Grid::new(&faces);
    let vis: Vec<Vec<Line2>> = par_map(&edges, |e| {
        let bb = [
            e.a.x.min(e.b.x),
            e.a.y.min(e.b.y),
            e.a.x.max(e.b.x),
            e.a.y.max(e.b.y),
        ];
        let dmax = e.da.max(e.db);
        let (x0, y0, x1, y1) = grid.span(bb);
        let mut cand: Vec<u32> = vec![];
        for y in y0..=y1 {
            for x in x0..=x1 {
                for &k in &grid.cells[y * grid.nx + x] {
                    let fc = &faces[k as usize];
                    if k as usize != e.face
                        && fc.dmin < dmax - DEPTH_EPS
                        && fc.bb[0] <= bb[2]
                        && fc.bb[2] >= bb[0]
                        && fc.bb[1] <= bb[3]
                        && fc.bb[3] >= bb[1]
                    {
                        // A face spanning several cells is taken once, in the first cell shared with the edge.
                        let (fx, fy, _, _) = grid.span(fc.bb);
                        if x == x0.max(fx) && y == y0.max(fy) {
                            cand.push(k);
                        }
                    }
                }
            }
        }
        cand.sort_by(|&p, &q| faces[p as usize].dmin.total_cmp(&faces[q as usize].dmin));
        let mut hid: Vec<(f64, f64)> = vec![];
        for k in cand {
            let before = hid.len();
            hidden_by(&faces[k as usize], e.a, e.b, e.da, e.db, &mut hid);
            if hid.len() != before {
                hid = merge(hid);
                if hid.len() == 1 && hid[0].0 <= 1e-12 && hid[0].1 >= 1.0 - 1e-12 {
                    return vec![];
                }
            }
        }
        let d = e.b - e.a;
        diff(&[(0.0, 1.0)], &hid)
            .into_iter()
            .filter(|(x, y)| (y - x) * d.len() > 1e-9)
            .map(|(x, y)| Line2 {
                item: e.item,
                a: e.a + d * x,
                b: e.a + d * y,
            })
            .collect()
    });
    SectionOut {
        cuts,
        lines: collinear_merge(vis.into_iter().flatten().collect()),
    }
}

/// Seam of coplanar faces: the part of an edge covered by edges of same-plane faces (equal depths)
/// on the other side of the same line is an interior line of a continuous surface; it is not drawn.
fn drop_seams(edges: &mut Vec<Edge>, faces: &[Face]) {
    type Key = (i64, i64, i64, [i64; 3]);
    let q = |v: f64| (v * 1e7).round() as i64;
    let mut groups: HashMap<Key, Vec<usize>> = HashMap::new();
    let mut dirs = Vec::with_capacity(edges.len());
    for (i, e) in edges.iter().enumerate() {
        let mut d = e.b - e.a;
        d = d * (1.0 / d.len());
        // Only vertical joints between neighbouring prisms; horizontal ones (wall on a slab,
        // tiers) remain drawing lines.
        if d.x.abs() > 1e-9 {
            dirs.push(d);
            continue;
        }
        if d.x < -1e-12 || (d.x.abs() <= 1e-12 && d.y < 0.0) {
            d = -d;
        }
        let off = e.a.dot(v2(-d.y, d.x));
        let dep = faces[e.face].dep;
        let k = (q(d.x), q(d.y), q(off), [q(dep[0]), q(dep[1]), q(dep[2])]);
        groups.entry(k).or_default().push(i);
        dirs.push(d);
    }
    let mut keep = vec![true; edges.len()];
    let mut out: Vec<Edge> = vec![];
    let mut keys: Vec<(Key, Vec<usize>)> = groups.into_iter().filter(|g| g.1.len() > 1).collect();
    keys.sort_by_key(|g| g.1[0]);
    for (_, g) in keys {
        let d = dirs[g[0]];
        // Side of the face relative to the line: a seam exists only between faces on opposite sides.
        let side = |i: usize| {
            let e = &edges[i];
            let m = (e.a + e.b) * 0.5 + v2(-d.y, d.x) * 1e-6;
            faces[e.face].contains(m)
        };
        let iv = |i: usize| {
            let (ta, tb) = (edges[i].a.dot(d), edges[i].b.dot(d));
            (ta.min(tb), ta.max(tb))
        };
        for &i in &g {
            let other = merge(
                g.iter()
                    .filter(|&&j| edges[j].face != edges[i].face && side(j) != side(i))
                    .map(|&j| iv(j))
                    .collect(),
            );
            let (t0, t1) = iv(i);
            if !other.iter().any(|&(a, b)| b > t0 + 1e-9 && a < t1 - 1e-9) {
                continue;
            }
            keep[i] = false;
            let e = &edges[i];
            let fwd = e.b.dot(d) >= e.a.dot(d);
            for (x, y) in diff(&[(t0, t1)], &other) {
                let (mut lx, mut ly) = ((x - t0) / (t1 - t0), (y - t0) / (t1 - t0));
                if !fwd {
                    (lx, ly) = (1.0 - ly, 1.0 - lx);
                }
                let (ab, dd) = (e.b - e.a, e.db - e.da);
                out.push(Edge {
                    a: e.a + ab * lx,
                    b: e.a + ab * ly,
                    da: e.da + dd * lx,
                    db: e.da + dd * ly,
                    face: e.face,
                    item: e.item,
                });
            }
        }
    }
    let mut k = 0;
    edges.retain(|_| {
        k += 1;
        keep[k - 1]
    });
    edges.extend(out);
}

/// Merges overlapping collinear segments of the same `item` (face seams, duplicate edges).
fn collinear_merge(lines: Vec<Line2>) -> Vec<Line2> {
    type Key = (usize, i64, i64, i64);
    type Group = (Key, V2, V2, Vec<(f64, f64)>);
    let mut groups: Vec<Group> = vec![];
    let mut index: HashMap<Key, usize> = HashMap::new();
    for l in lines {
        let mut d = l.b - l.a;
        d = d * (1.0 / d.len());
        if d.x < -1e-12 || (d.x.abs() <= 1e-12 && d.y < 0.0) {
            d = -d;
        }
        let nrm = v2(-d.y, d.x);
        let off = l.a.dot(nrm);
        let q = |v: f64| (v * 1e8).round() as i64;
        let k = (l.item, q(d.x), q(d.y), q(off));
        let g = *index.entry(k).or_insert_with(|| {
            groups.push((k, d, nrm * off, vec![]));
            groups.len() - 1
        });
        let (_, d, _, iv) = &mut groups[g];
        let (ta, tb) = (l.a.dot(*d), l.b.dot(*d));
        iv.push((ta.min(tb), ta.max(tb)));
    }
    let mut out = vec![];
    for (k, d, base, iv) in groups {
        let mut iv = iv;
        iv.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut m: Vec<(f64, f64)> = vec![];
        for (a, b) in iv {
            match m.last_mut() {
                Some(l) if a <= l.1 + 1e-9 => l.1 = l.1.max(b),
                _ => m.push((a, b)),
            }
        }
        for (a, b) in m {
            out.push(Line2 {
                item: k.0,
                a: base + d * a,
                b: base + d * b,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
