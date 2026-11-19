//! Roofs. From an outline, a weighted straight skeleton (each edge is a plane with its own
//! slope; an edge without slope is a vertical gable): the wavefront moves edges inward with edge-collapse
//! and split events (a reflex vertex hits an edge). Simultaneous events
//! are processed as a batch with a tolerance; collinear edges of equal slope merge, and for different slopes
//! a vertical step (a zero-speed edge) that grows with the front is inserted between them; a "spike" (opposing edges on one line) becomes a ridge.
//! Mansard: slopes change at the break height (or a flat deck). Extrusion roof: a profile
//! (u, z) in a vertical plane along a→b, swept to the left by the depth.
//! Quantities are exact: face area from the polygon, volume under the roof Σ plan·z(centre),
//! layer volume = true area × thickness (a sheet, with vertical ends).

use std::collections::HashMap;

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::poly;

#[derive(Debug, Clone, PartialEq)]
pub enum RoofError {
    TooFewPoints,
    BadSlope {
        edge: usize,
    },
    /// Adjacent collinear edges with different slopes.
    CollinearSlopes {
        edge: usize,
    },
    NoSlopedEdge,
    SelfIntersecting,
    /// The skeleton did not converge (degenerate input).
    Stuck,
    BadProfile,
}

/// Roof from an outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Footprint {
    /// Outline along the bearing lines (walls), either winding direction.
    pub ring: Vec<V2>,
    /// Slope of each edge i (ring[i]→ring[i+1]) in degrees; None is a gable.
    pub slopes: Vec<Option<f64>>,
    /// Elevation of the roof planes above the bearing line.
    pub base_z: f64,
    /// Horizontal overhang (for the edge with the largest run; steeper edges get proportionally less).
    pub overhang: f64,
    /// Mansard: (break height above base_z, upper slope in degrees; None is a flat deck).
    pub mansard: Option<(f64, Option<f64>)>,
}

/// Extruded roof: profile (u, z) over segment a→b, swept to the left by `depth`.
#[derive(Clone, Debug, PartialEq)]
pub struct Extrusion {
    pub a: V2,
    pub b: V2,
    pub profile: Vec<(f64, f64)>,
    pub depth: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RoofFace {
    /// Face vertices (roof top), counter-clockwise in plan for sloped faces.
    pub pts: Vec<V3>,
    /// Sloped (roof) or vertical (gable).
    pub sloped: bool,
    pub edge: usize,
    pub phase: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineKind {
    Eave,
    Ridge,
    Hip,
    Valley,
    /// Edge above a gable.
    Rake,
    /// Mansard break.
    Break,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoofLine {
    pub a: V3,
    pub b: V3,
    pub kind: LineKind,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct RoofQty {
    pub plan_area: f64,
    /// True roof area (sloped faces).
    pub area: f64,
    /// Volume between the roof and the eaves plane.
    pub volume_under: f64,
    /// Gable area (vertical faces).
    pub gable_area: f64,
    pub ridge: f64,
    pub hip: f64,
    pub valley: f64,
    pub eave: f64,
    pub rake: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Roof {
    pub faces: Vec<RoofFace>,
    pub lines: Vec<RoofLine>,
    /// Eaves elevation (roof bottom).
    pub eave_z: f64,
}

// ---------- skeleton ----------

#[derive(Clone, Copy, Debug)]
struct Edge {
    n: V2,
    c: f64,
    s: f64,
    key: usize,
}

impl Edge {
    fn dir(&self) -> V2 {
        v2(self.n.y, -self.n.x)
    }
    fn dist(&self, p: V2, t: f64) -> f64 {
        self.n.dot(p) - self.c - self.s * t
    }
}

#[derive(Clone, Copy, Debug)]
struct Vx {
    p: V2,
    vel: Option<V2>,
    ein: usize,
    eout: usize,
    start: V3,
}

struct Front {
    edges: Vec<Edge>,
    vs: Vec<Vx>,
    lavs: Vec<Vec<usize>>,
    t: f64,
    z0: f64,
    segs: Vec<(V3, V3, usize)>,
    eps: f64,
}

fn velocity(a: &Edge, b: &Edge) -> Option<V2> {
    let det = a.n.cross(b.n);
    if det.abs() < 1e-12 {
        return (a.n.dot(b.n) > 0.0).then_some(a.n * a.s);
    }
    Some(v2(
        (a.s * b.n.y - b.s * a.n.y) / det,
        (a.n.x * b.s - b.n.x * a.s) / det,
    ))
}

fn meet(a: &Edge, b: &Edge, t: f64) -> Option<V2> {
    let det = a.n.cross(b.n);
    if det.abs() < 1e-12 {
        return None;
    }
    let (ca, cb) = (a.c + a.s * t, b.c + b.s * t);
    Some(v2(
        (ca * b.n.y - cb * a.n.y) / det,
        (a.n.x * cb - b.n.x * ca) / det,
    ))
}

impl Front {
    fn at(&self, p: V2) -> V3 {
        v3(p.x, p.y, self.z0 + self.t)
    }

    fn emit(&mut self, a: V3, b: V3, key: usize) {
        if (a - b).len() > self.eps {
            self.segs.push((a, b, key));
        }
    }

    fn new_vx(&mut self, p: V2, ein: usize, eout: usize) -> usize {
        let (a, b) = (self.edges[ein], self.edges[eout]);
        let p = match meet(&a, &b, self.t) {
            Some(q) if (q - p).len() < 1e3 * self.eps.max(1e-9) => q,
            _ => p,
        };
        self.vs.push(Vx {
            p,
            vel: velocity(&a, &b),
            ein,
            eout,
            start: self.at(p),
        });
        self.vs.len() - 1
    }

    fn end_vx(&mut self, v: usize) {
        let x = self.vs[v];
        let e = self.at(x.p);
        let (ki, ko) = (self.edges[x.ein].key, self.edges[x.eout].key);
        self.emit(x.start, e, ki);
        self.emit(x.start, e, ko);
    }

    fn finalize(&mut self, l: usize) {
        let lav = std::mem::take(&mut self.lavs[l]);
        for &v in &lav {
            self.end_vx(v);
        }
        if lav.len() >= 2 {
            for i in 0..lav.len() {
                let (u, w) = (lav[i], lav[(i + 1) % lav.len()]);
                let (a, b) = (self.at(self.vs[u].p), self.at(self.vs[w].p));
                let k = self.edges[self.vs[u].eout].key;
                self.emit(a, b, k);
            }
        }
    }

    fn lav_area(&self, l: usize) -> f64 {
        let r: Vec<V2> = self.lavs[l].iter().map(|&v| self.vs[v].p).collect();
        poly::area(&r)
    }

    /// Next event after t: (time, whether it is a split).
    fn next_event(&self) -> Option<f64> {
        let mut best: Option<f64> = None;
        let mut take = |t: f64| {
            if t.is_finite() && t >= self.t - self.eps {
                best = Some(best.map_or(t, |b: f64| b.min(t)));
            }
        };
        for lav in &self.lavs {
            let n = lav.len();
            for i in 0..n {
                let (a, b) = (&self.vs[lav[i]], &self.vs[lav[(i + 1) % n]]);
                let (Some(va), Some(vb)) = (a.vel, b.vel) else {
                    take(self.t);
                    continue;
                };
                let d = self.edges[a.eout].dir();
                let f0 = d.dot(b.p - a.p);
                let r = d.dot(vb - va);
                if r < -1e-12 {
                    take(self.t + f0.max(0.0) / -r);
                }
            }
            for i in 0..n {
                let v = &self.vs[lav[i]];
                let Some(vel) = v.vel else { continue };
                let (din, dout) = (self.edges[v.ein].dir(), self.edges[v.eout].dir());
                if din.cross(dout) >= -1e-12 {
                    continue;
                }
                for j in 0..n {
                    let (a, b) = (lav[j], lav[(j + 1) % n]);
                    let e = self.edges[self.vs[a].eout];
                    if a == lav[i] || b == lav[i] {
                        continue;
                    }
                    let g0 = e.dist(v.p, self.t);
                    let g1 = e.n.dot(vel) - e.s;
                    if g1 >= -1e-12 || g0 < -self.eps {
                        continue;
                    }
                    let tau = g0.max(0.0) / -g1;
                    let x = v.p + vel * tau;
                    let (pa, pb) = (self.vs[a], self.vs[b]);
                    let (Some(va), Some(vb)) = (pa.vel, pb.vel) else {
                        continue;
                    };
                    let (qa, qb) = (pa.p + va * tau, pb.p + vb * tau);
                    let d = e.dir();
                    let s = d.dot(x - qa);
                    if s >= -self.eps && s <= d.dot(qb - qa) + self.eps {
                        take(self.t + tau);
                    }
                }
            }
        }
        best
    }

    fn advance(&mut self, t: f64) {
        let dt = t - self.t;
        for lav in &self.lavs {
            for &v in lav {
                if let Some(vel) = self.vs[v].vel {
                    self.vs[v].p = self.vs[v].p + vel * dt;
                }
            }
        }
        self.t = t;
    }

    /// A single split, if the reflex vertex lies on another edge.
    fn split_once(&mut self) -> bool {
        for l in 0..self.lavs.len() {
            let lav = self.lavs[l].clone();
            let n = lav.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let v = self.vs[lav[i]];
                let (din, dout) = (self.edges[v.ein].dir(), self.edges[v.eout].dir());
                if din.cross(dout) >= -1e-12 {
                    continue;
                }
                for j in 0..n {
                    let (a, b) = (lav[j], lav[(j + 1) % n]);
                    if a == lav[i] || b == lav[i] {
                        continue;
                    }
                    let ek = self.vs[a].eout;
                    let e = self.edges[ek];
                    if e.dist(v.p, self.t).abs() > self.eps * 10.0 {
                        continue;
                    }
                    // a diverging vertex (at the start of a growing step) does not split the edge
                    if v.vel.is_some_and(|w| e.n.dot(w) - e.s >= -1e-12) {
                        continue;
                    }
                    let d = e.dir();
                    let s = d.dot(v.p - self.vs[a].p);
                    if s < -self.eps * 10.0
                        || s > d.dot(self.vs[b].p - self.vs[a].p) + self.eps * 10.0
                    {
                        continue;
                    }
                    self.end_vx(lav[i]);
                    let v1 = self.new_vx(v.p, ek, v.eout);
                    let v2_ = self.new_vx(v.p, v.ein, ek);
                    // lav: … a → b … prev → v → next …
                    let rot = |k: usize| (0..n).map(|q| lav[(k + q) % n]).collect::<Vec<_>>();
                    let from_v = rot(i); // v, next, …, a, b, …, prev
                    let ja = from_v.iter().position(|&x| x == a).unwrap();
                    let mut l1 = vec![v1];
                    l1.extend_from_slice(&from_v[1..=ja]); // v1 → next … a
                    let mut l2 = vec![v2_];
                    l2.extend_from_slice(&from_v[ja + 1..]); // v2 → b … prev
                                                             // winding order: l2 starts at v2, its incoming edge is v.ein from prev
                    let l2: Vec<usize> = {
                        let mut r = l2[1..].to_vec();
                        r.push(v2_);
                        r
                    };
                    self.lavs[l] = l1;
                    self.lavs.push(l2);
                    return true;
                }
            }
        }
        false
    }

    /// Collapsed edges, spikes and degenerate outlines.
    fn cleanup(&mut self) -> bool {
        let mut changed = false;
        for l in 0..self.lavs.len() {
            let n = self.lavs[l].len();
            if n == 0 {
                continue;
            }
            if n < 3 || self.lav_area(l).abs() < self.eps * self.eps.sqrt() {
                self.finalize(l);
                changed = true;
                continue;
            }
            for i in 0..n {
                let (a, b) = (self.lavs[l][i], self.lavs[l][(i + 1) % n]);
                if (self.vs[a].p - self.vs[b].p).len() <= self.eps * 10.0 {
                    // a growing zero-length step is not a collapsed edge
                    if let (Some(va), Some(vb)) = (self.vs[a].vel, self.vs[b].vel) {
                        if self.edges[self.vs[a].eout].dir().dot(vb - va) > 1e-12 {
                            continue;
                        }
                    }
                    self.end_vx(a);
                    self.end_vx(b);
                    let m = (self.vs[a].p + self.vs[b].p) * 0.5;
                    let w = self.new_vx(m, self.vs[a].ein, self.vs[b].eout);
                    let lav = &mut self.lavs[l];
                    lav[i] = w;
                    lav.remove((i + 1) % n);
                    return true;
                }
            }
            for i in 0..n {
                let v = self.lavs[l][i];
                if self.vs[v].vel.is_some() {
                    continue;
                }
                // spike: opposing edges on one line; a ridge runs to the nearest neighbour
                let prev = self.lavs[l][(i + n - 1) % n];
                let next = self.lavs[l][(i + 1) % n];
                let (pv, pp, pn) = (self.vs[v].p, self.vs[prev].p, self.vs[next].p);
                let near_prev = (pp - pv).len() <= (pn - pv).len();
                let x = self.vs[v];
                self.end_vx(v);
                let tip = self.at(pv);
                let (ki, ko) = (self.edges[x.ein].key, self.edges[x.eout].key);
                if near_prev {
                    self.emit(tip, self.at(pp), ki);
                    self.emit(tip, self.at(pp), ko);
                    self.end_vx(prev);
                    let w = self.new_vx(pp, self.vs[prev].ein, x.eout);
                    let lav = &mut self.lavs[l];
                    lav[(i + n - 1) % n] = w;
                    lav.remove(i);
                } else {
                    self.emit(tip, self.at(pn), ki);
                    self.emit(tip, self.at(pn), ko);
                    self.end_vx(next);
                    let w = self.new_vx(pn, x.ein, self.vs[next].eout);
                    let lav = &mut self.lavs[l];
                    lav[(i + 1) % n] = w;
                    lav.remove(i);
                }
                return true;
            }
        }
        self.lavs.retain(|l| !l.is_empty());
        changed
    }

    fn settle(&mut self) {
        for _ in 0..100_000 {
            if !(self.split_once() || self.cleanup()) {
                break;
            }
        }
    }

    fn run_until(&mut self, stop: Option<f64>, guard: usize) -> Result<(), RoofError> {
        self.settle();
        for _ in 0..guard {
            if self.lavs.is_empty() {
                return Ok(());
            }
            let Some(t) = self.next_event() else {
                return Err(RoofError::Stuck);
            };
            if let Some(s) = stop {
                if t >= s - self.eps {
                    self.advance(s);
                    self.settle();
                    return Ok(());
                }
            }
            self.advance(t.max(self.t));
            self.settle();
        }
        Err(RoofError::Stuck)
    }
}

/// Canonical outline form: counter-clockwise, no duplicates; collinear edges of equal slope merged.
fn normalize(
    ring: &[V2],
    slopes: &[Option<f64>],
) -> Result<(Vec<V2>, Vec<Option<f64>>), RoofError> {
    if ring.len() < 3 || slopes.len() != ring.len() {
        return Err(RoofError::TooFewPoints);
    }
    for (i, s) in slopes.iter().enumerate() {
        if let Some(s) = s {
            if !(s.is_finite() && *s > 0.0 && *s < 89.0) {
                return Err(RoofError::BadSlope { edge: i });
            }
        }
    }
    let n = ring.len();
    let (mut r, mut sl): (Vec<V2>, Vec<Option<f64>>) = if poly::area(ring) < 0.0 {
        // reversal: new edge j is old edge n−2−j
        (
            (0..n).map(|j| ring[n - 1 - j]).collect(),
            (0..n).map(|j| slopes[(2 * n - 2 - j) % n]).collect(),
        )
    } else {
        (ring.to_vec(), slopes.to_vec())
    };
    let scale = r.iter().map(|p| p.len()).fold(1.0, f64::max);
    loop {
        let m = r.len();
        if m < 3 {
            return Err(RoofError::TooFewPoints);
        }
        let mut hit = None;
        for i in 0..m {
            let (p, q, w) = (r[(i + m - 1) % m], r[i], r[(i + 1) % m]);
            if (q - p).len() < 1e-9 * scale {
                hit = Some((i, true));
                break;
            }
            let (d0, d1) = (q - p, w - q);
            if d0.cross(d1).abs() < 1e-12 * d0.len() * d1.len() && d0.dot(d1) > 0.0 {
                let (a, b) = (sl[(i + m - 1) % m], sl[i]);
                let same = match (a, b) {
                    (None, None) => true,
                    (Some(x), Some(y)) => (x - y).abs() < 1e-9,
                    _ => false,
                };
                if !same {
                    continue;
                }
                hit = Some((i, false));
                break;
            }
        }
        match hit {
            Some((i, _)) => {
                r.remove(i);
                sl.remove(i);
            }
            None => break,
        }
    }
    // self-intersection
    let m = r.len();
    for i in 0..m {
        for j in i + 1..m {
            if j == i + 1 || (i == 0 && j == m - 1) {
                continue;
            }
            if crate::predicates::segments_meet(r[i], r[(i + 1) % m], r[j], r[(j + 1) % m]) {
                return Err(RoofError::SelfIntersecting);
            }
        }
    }
    Ok((r, sl))
}

fn chain3(segs: &[(V3, V3)], eps: f64) -> Vec<Vec<V3>> {
    let mut used = vec![false; segs.len()];
    let mut out = vec![];
    for s in 0..segs.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let start = segs[s].0;
        let mut ring = vec![start];
        let mut cur = segs[s].1;
        for _ in 0..segs.len() {
            if (cur - start).len() <= eps {
                break;
            }
            ring.push(cur);
            let Some(j) = (0..segs.len()).find(|&j| {
                !used[j] && ((segs[j].0 - cur).len() <= eps || (segs[j].1 - cur).len() <= eps)
            }) else {
                break;
            };
            used[j] = true;
            cur = if (segs[j].0 - cur).len() <= eps {
                segs[j].1
            } else {
                segs[j].0
            };
        }
        if ring.len() >= 3 {
            out.push(ring);
        }
    }
    out
}

pub(crate) fn newell(p: &[V3]) -> V3 {
    let mut n = v3(0.0, 0.0, 0.0);
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n = n + v3(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        );
    }
    n
}

/// Removes vertices lying on the line through their neighbours (after face assembly).
fn simplify(p: Vec<V3>, eps: f64) -> Vec<V3> {
    let mut p = p;
    let mut i = 0;
    while p.len() > 3 && i < p.len() {
        let m = p.len();
        let (a, b, c) = (p[(i + m - 1) % m], p[i], p[(i + 1) % m]);
        if (b - a).cross(c - b).len() <= eps * ((b - a).len() + (c - b).len()) {
            p.remove(i);
        } else {
            i += 1;
        }
    }
    p
}

impl Footprint {
    pub fn build(&self) -> Result<Roof, RoofError> {
        let (ring, slopes) = normalize(&self.ring, &self.slopes)?;
        let n = ring.len();
        let speed = |deg: Option<f64>| deg.map_or(0.0, |d| 1.0 / d.to_radians().tan());
        let s0: Vec<f64> = slopes.iter().map(|s| speed(*s)).collect();
        let smax = s0.iter().cloned().fold(0.0, f64::max);
        if smax <= 0.0 {
            return Err(RoofError::NoSlopedEdge);
        }
        let scale = ring.iter().map(|p| p.len()).fold(1.0, f64::max);
        let eps = 1e-9 * scale;
        // overhang: edge lines are offset outward by time T (gables by `overhang`)
        let tt = self.overhang.max(0.0) / smax;
        let mut edges = vec![];
        let mut s0x = s0.clone();
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let d = (q - p) * (1.0 / (q - p).len());
            let nrm = v2(-d.y, d.x);
            // collinear edges with different slopes: a vertical step between them, running from the
            // faster front to the slower one (otherwise the vertex has no consistent velocity)
            let pv = (i + n - 1) % n;
            let dp = (p - ring[pv]) * (1.0 / (p - ring[pv]).len());
            if dp.cross(d).abs() < 1e-12 && dp.dot(d) > 0.0 && (s0[pv] - s0[i]).abs() > 1e-9 {
                let dw = if s0[pv] > s0[i] { -nrm } else { nrm };
                let nw = v2(-dw.y, dw.x);
                edges.push(Edge {
                    n: nw,
                    c: nw.dot(p),
                    s: 0.0,
                    key: s0x.len() * 2,
                });
                s0x.push(0.0);
            }
            let shift = if s0[i] > 0.0 {
                s0[i] * tt
            } else {
                self.overhang.max(0.0)
            };
            edges.push(Edge {
                n: nrm,
                c: nrm.dot(p) - shift,
                s: s0[i],
                key: i * 2,
            });
        }
        let m = edges.len();
        let mut base = vec![];
        for i in 0..m {
            let (a, b) = (edges[(i + m - 1) % m], edges[i]);
            base.push(meet(&a, &b, 0.0).ok_or(RoofError::CollinearSlopes { edge: b.key / 2 })?);
        }
        let z0 = self.base_z - tt;
        let mut f = Front {
            edges: edges.clone(),
            vs: vec![],
            lavs: vec![],
            t: 0.0,
            z0,
            segs: vec![],
            eps,
        };
        let lav: Vec<usize> = (0..m)
            .map(|i| f.new_vx(base[i], (i + m - 1) % m, i))
            .collect();
        f.lavs.push(lav);
        let guard = 20 * m * m + 100;
        let mut extra: Vec<RoofFace> = vec![];
        match self.mansard {
            Some((hb, upper)) if hb > 0.0 => {
                let tb = hb + tt;
                f.run_until(Some(tb), guard)?;
                // break: horizontal segments are the top of the lower faces and the bottom of the upper faces
                let lavs = std::mem::take(&mut f.lavs);
                for lav in &lavs {
                    for &v in lav {
                        f.end_vx(v);
                    }
                }
                let mut new_lavs = vec![];
                for lav in &lavs {
                    let pts: Vec<V3> = lav.iter().map(|&v| f.at(f.vs[v].p)).collect();
                    for i in 0..lav.len() {
                        let e = f.vs[lav[i]].eout;
                        let (a, b) = (pts[i], pts[(i + 1) % lav.len()]);
                        f.emit(a, b, f.edges[e].key);
                        if upper.is_some() {
                            f.emit(a, b, f.edges[e].key + 1);
                        }
                    }
                    match upper {
                        None => {
                            if pts.len() >= 3 {
                                extra.push(RoofFace {
                                    pts,
                                    sloped: true,
                                    edge: usize::MAX,
                                    phase: 1,
                                });
                            }
                        }
                        Some(_) => new_lavs.push(lav.clone()),
                    }
                }
                if let Some(deg) = upper {
                    if !(deg.is_finite() && deg > 0.0 && deg < 89.0) {
                        return Err(RoofError::BadSlope { edge: 0 });
                    }
                    let s1 = speed(Some(deg));
                    for e in f.edges.iter_mut() {
                        if e.s > 0.0 {
                            e.c += (e.s - s1) * tb;
                            e.s = s1;
                        }
                        e.key += 1;
                    }
                    for lav in new_lavs {
                        let l: Vec<usize> = lav
                            .iter()
                            .map(|&v| {
                                let x = f.vs[v];
                                f.new_vx(x.p, x.ein, x.eout)
                            })
                            .collect();
                        f.lavs.push(l);
                    }
                    f.run_until(None, guard)?;
                }
            }
            _ => f.run_until(None, guard)?,
        }
        // faces: bases + traces
        let mut by: HashMap<usize, Vec<(V3, V3)>> = HashMap::new();
        for i in 0..m {
            let (a, b) = (base[i], base[(i + 1) % m]);
            by.entry(edges[i].key)
                .or_default()
                .push((v3(a.x, a.y, z0), v3(b.x, b.y, z0)));
        }
        for &(a, b, k) in &f.segs {
            by.entry(k).or_default().push((a, b));
        }
        let mut faces = vec![];
        let mut keys: Vec<usize> = by.keys().copied().collect();
        keys.sort();
        for k in keys {
            let (edge, phase) = (k / 2, (k % 2) as u8);
            for loop_ in chain3(&by[&k], eps * 100.0) {
                let mut pts = simplify(loop_, eps * 10.0);
                let nn = newell(&pts);
                let sloped = s0x[edge] > 0.0;
                if sloped && nn.z < 0.0 {
                    pts.reverse();
                }
                if nn.len() > eps * eps {
                    faces.push(RoofFace {
                        pts,
                        sloped,
                        edge,
                        phase,
                    });
                }
            }
        }
        faces.extend(extra);
        let lines = classify(&faces, z0, eps * 100.0);
        Ok(Roof {
            faces,
            lines,
            eave_z: z0,
        })
    }
}

impl Extrusion {
    pub fn build(&self, base_z: f64) -> Result<Roof, RoofError> {
        let l = (self.b - self.a).len();
        if l < 1e-9
            || self.profile.len() < 2
            || !(self.depth > 0.0)
            || self.profile.windows(2).any(|w| w[1].0 <= w[0].0)
        {
            return Err(RoofError::BadProfile);
        }
        let d = (self.b - self.a) * (1.0 / l);
        let nrm = v2(-d.y, d.x);
        let at = |u: f64, w: f64, z: f64| {
            let p = self.a + d * u + nrm * w;
            v3(p.x, p.y, base_z + z)
        };
        let mut faces = vec![];
        for (k, s) in self.profile.windows(2).enumerate() {
            let (p, q) = (s[0], s[1]);
            faces.push(RoofFace {
                pts: vec![
                    at(p.0, 0.0, p.1),
                    at(q.0, 0.0, q.1),
                    at(q.0, self.depth, q.1),
                    at(p.0, self.depth, p.1),
                ],
                sloped: true,
                edge: k,
                phase: 0,
            });
        }
        let zmin = self.profile.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        let lines = classify(&faces, base_z + zmin, 1e-7);
        Ok(Roof {
            faces,
            lines,
            eave_z: base_z + zmin,
        })
    }
}

pub(crate) fn plane_z(f: &RoofFace, p: V2) -> f64 {
    let n = newell(&f.pts);
    let o = f.pts[0];
    o.z - (n.x * (p.x - o.x) + n.y * (p.y - o.y)) / n.z
}

pub(crate) fn plan(f: &RoofFace) -> Vec<V2> {
    f.pts.iter().map(|p| v2(p.x, p.y)).collect()
}

pub(crate) fn classify(faces: &[RoofFace], eave_z: f64, eps: f64) -> Vec<RoofLine> {
    let q = |p: V3| {
        let k = |v: f64| (v / eps).round() as i64;
        (k(p.x), k(p.y), k(p.z))
    };
    let mut map: HashMap<((i64, i64, i64), (i64, i64, i64)), (V3, V3, Vec<usize>)> = HashMap::new();
    for (fi, f) in faces.iter().enumerate() {
        for i in 0..f.pts.len() {
            let (a, b) = (f.pts[i], f.pts[(i + 1) % f.pts.len()]);
            let (ka, kb) = (q(a), q(b));
            let key = if ka <= kb { (ka, kb) } else { (kb, ka) };
            map.entry(key).or_insert((a, b, vec![])).2.push(fi);
        }
    }
    let mut out = vec![];
    let mut vals: Vec<_> = map.into_values().collect();
    vals.sort_by(|x, y| {
        (x.0.x, x.0.y, x.0.z, x.1.x, x.1.y)
            .partial_cmp(&(y.0.x, y.0.y, y.0.z, y.1.x, y.1.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (a, b, fs) in vals {
        let horizontal = (a.z - b.z).abs() <= eps;
        let sl: Vec<usize> = fs.iter().copied().filter(|&i| faces[i].sloped).collect();
        let kind = match sl.len() {
            0 => continue,
            1 => {
                if fs.len() > 1 {
                    LineKind::Rake
                } else if horizontal && (a.z - eave_z).abs() <= eps {
                    LineKind::Eave
                } else if horizontal {
                    // edge of the mansard deck / top of the extrusion
                    LineKind::Eave
                } else {
                    LineKind::Rake
                }
            }
            _ => {
                let (fa, fb) = (&faces[sl[0]], &faces[sl[1]]);
                if fa.edge == fb.edge && fa.phase != fb.phase
                    || fa.edge == usize::MAX
                    || fb.edge == usize::MAX
                {
                    LineKind::Break
                } else {
                    let cb = poly::centroid(&plan(fb));
                    let za = plane_z(fa, cb);
                    let zb = plane_z(fb, cb);
                    if (za - zb).abs() <= eps {
                        LineKind::Break
                    } else if horizontal && za > zb {
                        LineKind::Ridge
                    } else if horizontal {
                        LineKind::Valley
                    } else if za > zb {
                        LineKind::Hip
                    } else {
                        LineKind::Valley
                    }
                }
            }
        };
        out.push(RoofLine { a, b, kind });
    }
    out
}

impl Roof {
    pub fn quantities(&self) -> RoofQty {
        let mut q = RoofQty::default();
        for f in &self.faces {
            let n = newell(&f.pts);
            if f.sloped {
                let pa = poly::area(&plan(f)).abs();
                q.plan_area += pa;
                q.area += n.len() / 2.0;
                let c = poly::centroid(&plan(f));
                q.volume_under += pa * (plane_z(f, c) - self.eave_z);
            } else {
                q.gable_area += n.len() / 2.0;
            }
        }
        for l in &self.lines {
            let len = (l.b - l.a).len();
            match l.kind {
                LineKind::Ridge => q.ridge += len,
                LineKind::Hip => q.hip += len,
                LineKind::Valley => q.valley += len,
                LineKind::Eave => q.eave += len,
                LineKind::Rake => q.rake += len,
                LineKind::Break => {}
            }
        }
        q
    }

    /// Sloped face above a plan point.
    fn face_at(&self, p: V2) -> Option<&RoofFace> {
        let sloped = || self.faces.iter().filter(|f| f.sloped);
        sloped()
            .find(|f| crate::predicates::point_in_ring(p, &plan(f)) >= 0)
            // seam of two faces after rounding: the point falls in the gap between rings
            .or_else(|| {
                sloped().find(|f| {
                    let r = plan(f);
                    (0..r.len()).any(|i| {
                        let (a, b) = (r[i], r[(i + 1) % r.len()]);
                        let d = b - a;
                        let t = ((p - a).dot(d) / d.dot(d).max(1e-30)).clamp(0.0, 1.0);
                        (a + d * t - p).len() < 1e-9
                    })
                })
            })
    }

    pub fn top_at(&self, p: V2) -> Option<f64> {
        self.face_at(p).map(|f| plane_z(f, p))
    }

    /// Roof underside (thickness `t` along the normal): top − t/cos θ of the face.
    pub fn underside_at(&self, p: V2, t: f64) -> Option<f64> {
        self.face_at(p).map(|f| {
            let n = newell(&f.pts);
            plane_z(f, p) - t * n.len() / n.z.abs()
        })
    }

    /// Roof underside polyline along a→b: (u from a, z); no points outside the roof (intervals without a roof
    /// are skipped, steps are a pair of points with the same u). Gaps are not marked: for continuous
    /// stretches see `underside_runs`.
    pub fn underside_along(&self, a: V2, b: V2, t: f64) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = vec![];
        for pt in self.underside_runs(a, b, t).into_iter().flatten() {
            match out.last() {
                Some(last) if (last.0 - pt.0).abs() < 1e-9 && (last.1 - pt.1).abs() < 1e-9 => {}
                _ => out.push(pt),
            }
        }
        // collinear points
        let mut i = 1;
        while i + 1 < out.len() {
            let (p, c, q) = (out[i - 1], out[i], out[i + 1]);
            let col = ((c.0 - p.0) * (q.1 - p.1) - (c.1 - p.1) * (q.0 - p.0)).abs() < 1e-12;
            if col && c.0 > p.0 && q.0 > c.0 {
                out.remove(i);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Roof underside along a→b as continuous runs: there is no roof between runs (cut-out, L-shaped
    /// outline), so they must not be joined. At a ridge or valley with different slopes the path along the seam takes the
    /// intersection of the offset planes of both faces.
    pub fn underside_runs(&self, a: V2, b: V2, t: f64) -> Vec<Vec<(f64, f64)>> {
        let rings: Vec<Vec<V2>> = self.faces.iter().filter(|f| f.sloped).map(plan).collect();
        let mut ts = vec![0.0, 1.0];
        for r in &rings {
            ts.extend(poly::crossings(a, b, std::slice::from_ref(r)));
        }
        // a path along a ring edge has no crossings: split it at the ring vertices it passes
        let d = b - a;
        let d2 = d.dot(d);
        if d2 > 1e-18 {
            for r in &rings {
                for &v in r {
                    let s = (v - a).dot(d) / d2;
                    if s > 1e-9 && s < 1.0 - 1e-9 && (a + d * s - v).len() < 1e-9 {
                        ts.push(s);
                    }
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.dedup_by(|x, y| (*x - *y).abs() < 1e-12);
        let l = (b - a).len();
        let dir = if l > 1e-12 { d * (1.0 / l) } else { v2(1.0, 0.0) };
        let side = v2(-dir.y, dir.x) * 1e-6;
        let off = |f: &RoofFace| {
            let n = newell(&f.pts);
            t * n.len() / n.z.abs()
        };
        let mut runs: Vec<Vec<(f64, f64)>> = vec![];
        let mut open = false;
        // face of the previous interval of the open run (for the joint between two faces)
        let mut prev: Option<&RoofFace> = None;
        let mut prev_s0 = 0.0;
        let mut mitre: Option<(f64, f64)> = None;
        for w in ts.windows(2) {
            let m = a + d * ((w[0] + w[1]) / 2.0);
            let Some(f) = self.face_at(m) else {
                open = false;
                prev = None;
                continue;
            };
            if !open {
                runs.push(vec![]);
                open = true;
            }
            let run = runs.last_mut().unwrap();
            // the path lies on a seam of two different faces
            let seam = match (self.face_at(m + side), self.face_at(m - side)) {
                (Some(fl), Some(fr)) if !std::ptr::eq(fl, fr) => Some((fl, fr)),
                _ => None,
            };
            for s in [w[0], w[1]] {
                let p = a + d * s;
                // two faces meet across the path: the offset surface is continuous there, the
                // lower plane on a ridge and the higher one in a valley, not a step
                let joint = match (prev, seam, s == w[0]) {
                    (Some(fp), None, true) if !std::ptr::eq(fp, f) => {
                        let q = p + dir * 1e-4;
                        let (zp, zn) = (plane_z(fp, p) - off(fp), plane_z(f, p) - off(f));
                        let zj = if plane_z(fp, q) >= plane_z(f, q) { zp.min(zn) } else { zp.max(zn) };
                        // the offset planes meet at `delta` from the seam, not on it: the kink of
                        // the underside is there (the lower plane runs on past the seam)
                        let gd = ((plane_z(fp, q) - plane_z(f, q)) - (plane_z(fp, p) - plane_z(f, p))) / 1e-4;
                        let delta = if gd.abs() > 1e-9 { -(zp - zn) / gd } else { 0.0 };
                        let next_len = (w[1] - w[0]) * l;
                        let prev_len = (w[0] - prev_s0) * l;
                        if delta > 1e-9 && delta < next_len - 1e-9 {
                            let x = p + dir * delta;
                            mitre = Some((s * l + delta, plane_z(fp, x) - off(fp)));
                            Some(zp)
                        } else if delta < -1e-9 && -delta < prev_len - 1e-9 && run.len() >= 2 {
                            let x = p + dir * delta;
                            let at = run.len() - 1;
                            run.insert(at, (s * l + delta, plane_z(f, x) - off(f)));
                            if let Some(last) = run.last_mut() {
                                last.1 = zn;
                            }
                            Some(zn)
                        } else {
                            if let Some(last) = run.last_mut() {
                                if (last.0 - s * l).abs() < 1e-9 {
                                    last.1 = zj;
                                }
                            }
                            Some(zj)
                        }
                    }
                    _ => None,
                };
                let z = match seam {
                    Some((fl, fr)) => {
                        let (zl, zr) = (plane_z(fl, p) - off(fl), plane_z(fr, p) - off(fr));
                        let (tl, tr) = (plane_z(fl, p), plane_z(fr, p));
                        let valley = plane_z(fl, m + side) > tl && plane_z(fr, m - side) > tr;
                        if valley {
                            zl.max(zr)
                        } else {
                            zl.min(zr)
                        }
                    }
                    None => joint.unwrap_or_else(|| plane_z(f, p) - off(f)),
                };
                let pt = (s * l, z);
                match run.last() {
                    Some(last) if (last.0 - pt.0).abs() < 1e-9 && (last.1 - pt.1).abs() < 1e-9 => {}
                    _ => run.push(pt),
                }
                if let Some(m) = mitre.take() {
                    run.push(m);
                }
            }
            prev_s0 = w[0];
            prev = if seam.is_none() { Some(f) } else { None };
        }
        runs
    }

    /// Roof body of thickness `t` along the normal: top, bottom (offset by t/cos θ), ends along the outline.
    /// Closed when adjacent faces have equal slopes.
    pub fn mesh(&self, t: f64) -> Mesh {
        let mut m = Mesh::default();
        // shared vertices by coordinates: the mesh is welded
        let mut ids: HashMap<(i64, i64, i64), u32> = HashMap::new();
        let mut push = |m: &mut Mesh, tri: [V3; 3], face: u32| {
            let t = tri.map(|p| {
                let k = |v: f64| (v * 1e7).round() as i64;
                *ids.entry((k(p.x), k(p.y), k(p.z))).or_insert_with(|| {
                    m.positions.push(p);
                    (m.positions.len() - 1) as u32
                })
            });
            m.triangles.push(t);
            m.face_of.push(face);
        };
        let sloped: Vec<(usize, &RoofFace)> = self
            .faces
            .iter()
            .enumerate()
            .filter(|f| f.1.sloped)
            .collect();
        let dz = |f: &RoofFace| {
            let n = newell(&f.pts);
            t * n.len() / n.z.abs()
        };
        for &(fi, f) in &sloped {
            let pl = plan(f);
            let d = v3(0.0, 0.0, -dz(f));
            for tr in poly::triangulate(&pl) {
                let p = tr.map(|i| f.pts[i]);
                push(&mut m, p, fi as u32);
                push(&mut m, [p[0] + d, p[2] + d, p[1] + d], fi as u32);
            }
        }
        // ends: edges belonging to a single sloped face
        let key = |p: V3| {
            let k = |v: f64| (v * 1e7).round() as i64;
            (k(p.x), k(p.y), k(p.z))
        };
        let mut count: HashMap<_, usize> = HashMap::new();
        for &(_, f) in &sloped {
            for i in 0..f.pts.len() {
                let (a, b) = (key(f.pts[i]), key(f.pts[(i + 1) % f.pts.len()]));
                *count
                    .entry(if a <= b { (a, b) } else { (b, a) })
                    .or_default() += 1;
            }
        }
        for &(fi, f) in &sloped {
            let d = v3(0.0, 0.0, -dz(f));
            for i in 0..f.pts.len() {
                let (a, b) = (f.pts[i], f.pts[(i + 1) % f.pts.len()]);
                let (ka, kb) = (key(a), key(b));
                if count[&if ka <= kb { (ka, kb) } else { (kb, ka) }] != 1 {
                    continue;
                }
                // face traversed counter-clockwise: outward is to the right of a→b
                push(&mut m, [a, a + d, b + d], fi as u32);
                push(&mut m, [a, b + d, b], fi as u32);
            }
        }
        m
    }

    /// Edges for views (roof lines).
    pub fn edges(&self) -> Vec<(V3, V3, LineKind)> {
        self.lines.iter().map(|l| (l.a, l.b, l.kind)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: f64, h: f64) -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(w, 0.0), v2(w, h), v2(0.0, h)]
    }

    fn near(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");
    }

    #[test]
    fn hip_10x6_30deg() {
        let r = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(30.0); 4],
            base_z: 3.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = r.quantities();
        let t30 = 30f64.to_radians().tan();
        assert_eq!(r.faces.len(), 4);
        near(q.plan_area, 60.0, "plan");
        near(q.area, 60.0 / 30f64.to_radians().cos(), "area");
        near(q.ridge, 4.0, "ridge");
        near(q.hip, 4.0 * (18.0 + 9.0 * t30 * t30).sqrt(), "hip");
        near(q.eave, 32.0, "eave");
        // tan θ · W²(3L − W)/12
        near(q.volume_under, t30 * 36.0 * 24.0 / 12.0, "volume");
        near(r.top_at(v2(5.0, 3.0)).unwrap(), 3.0 + 3.0 * t30, "ridge z");
        let m = r.mesh(0.2);
        assert!(m.is_watertight());
        near(m.volume(), q.area * 0.2, "sheet volume");
    }

    #[test]
    fn gable_10x6_30deg() {
        let r = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(30.0), None, Some(30.0), None],
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = r.quantities();
        let t30 = 30f64.to_radians().tan();
        near(q.ridge, 10.0, "ridge");
        near(q.area, 60.0 / 30f64.to_radians().cos(), "area");
        near(q.gable_area, 6.0 * 3.0 * t30, "gables");
        near(q.volume_under, 0.5 * 6.0 * 3.0 * t30 * 10.0, "volume");
        near(q.hip, 0.0, "hip");
        near(q.rake, 4.0 * 3.0 / 30f64.to_radians().cos(), "rake");
    }

    #[test]
    fn shed_and_overhang() {
        let r = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(45.0), None, None, None],
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = r.quantities();
        near(q.area, 60.0 * 2f64.sqrt(), "shed area");
        near(q.volume_under, 0.5 * 6.0 * 6.0 * 10.0, "shed volume");
        // overhang 0.5 on a 45° hip roof: plan 11×7, eaves 0.5 lower
        let h = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(45.0); 4],
            base_z: 3.0,
            overhang: 0.5,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = h.quantities();
        near(q.plan_area, 77.0, "plan with overhang");
        near(h.eave_z, 2.5, "eave z");
        near(q.ridge, 4.0, "ridge");
        near(h.top_at(v2(5.0, 3.0)).unwrap(), 6.0, "ridge z");
    }

    #[test]
    fn l_shaped_hip() {
        let ring = vec![
            v2(0.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 6.0),
            v2(6.0, 6.0),
            v2(6.0, 12.0),
            v2(0.0, 12.0),
        ];
        let r = Footprint {
            ring,
            slopes: vec![Some(30.0); 6],
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = r.quantities();
        let (t30, c30) = (30f64.to_radians().tan(), 30f64.to_radians().cos());
        assert_eq!(r.faces.len(), 6);
        near(q.plan_area, 96.0, "plan");
        near(q.area, 96.0 / c30, "area");
        near(q.ridge, 10.0, "ridge");
        near(q.valley, (18.0 + 9.0 * t30 * t30).sqrt(), "valley");
        near(q.hip, 5.0 * (18.0 + 9.0 * t30 * t30).sqrt(), "hips");
        assert!(r.mesh(0.2).is_watertight());
    }

    #[test]
    fn mansard_two_slopes_and_flat_deck() {
        let r = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(60.0); 4],
            base_z: 0.0,
            overhang: 0.0,
            mansard: Some((1.0, Some(20.0))),
        }
        .build()
        .unwrap();
        let q = r.quantities();
        let s = 1.0 / 60f64.to_radians().tan();
        // lower band: outline 10×6 → (10−2s)×(6−2s) at height 1
        let (l1, w1) = (10.0 - 2.0 * s, 6.0 - 2.0 * s);
        near(q.ridge, l1 - w1, "upper ridge");
        let lower_plan = 60.0 - l1 * w1;
        let c = |d: f64| d.to_radians().cos();
        near(q.area, lower_plan / c(60.0) + l1 * w1 / c(20.0), "area");
        let deck = Footprint {
            mansard: Some((1.0, None)),
            ..Footprint {
                ring: rect(10.0, 6.0),
                slopes: vec![Some(60.0); 4],
                base_z: 0.0,
                overhang: 0.0,
                mansard: None,
            }
        }
        .build()
        .unwrap();
        let q = deck.quantities();
        near(q.plan_area, 60.0, "deck plan");
        near(q.area, lower_plan / c(60.0) + l1 * w1, "deck area");
    }

    #[test]
    fn collinear_edges_with_different_slopes_make_a_closed_stepped_roof() {
        for overhang in [0.0, 0.5] {
            let ring = vec![
                v2(0.0, 0.0),
                v2(5.0, 0.0),
                v2(10.0, 0.0),
                v2(10.0, 6.0),
                v2(0.0, 6.0),
            ];
            let slopes = vec![Some(30.0), Some(45.0), Some(40.0), Some(40.0), Some(40.0)];
            let r = Footprint {
                ring,
                slopes,
                base_z: 0.0,
                overhang,
                mansard: None,
            }
            .build()
            .unwrap();
            // side face: slope from the normal, each face is planar
            for (edge, deg) in [(0usize, 30.0f64), (1, 45.0)] {
                let f = r.faces.iter().find(|f| f.edge == edge && f.sloped).unwrap();
                let nn = newell(&f.pts);
                let tilt = (nn.x * nn.x + nn.y * nn.y).sqrt().atan2(nn.z).to_degrees();
                near(tilt, deg, "slope");
                let n = nn * (1.0 / nn.len());
                for p in &f.pts {
                    near(n.dot(*p - f.pts[0]), 0.0, "planar");
                }
            }
            assert!(r.faces.iter().any(|f| !f.sloped && f.edge >= 5));
            // closedness: every face edge is shared by two faces, except the eaves edge
            let q = |p: V3| {
                let k = |v: f64| (v * 1e6).round() as i64;
                (k(p.x), k(p.y), k(p.z))
            };
            let mut cnt: HashMap<_, (usize, V3, V3)> = HashMap::new();
            for f in &r.faces {
                for i in 0..f.pts.len() {
                    let (a, b) = (f.pts[i], f.pts[(i + 1) % f.pts.len()]);
                    let (ka, kb) = (q(a), q(b));
                    let k = if ka <= kb { (ka, kb) } else { (kb, ka) };
                    cnt.entry(k).or_insert((0, a, b)).0 += 1;
                }
            }
            for (c, a, b) in cnt.values() {
                let eave = (a.z - r.eave_z).abs() < 1e-9 && (b.z - r.eave_z).abs() < 1e-9;
                assert!(*c == 2 || (*c == 1 && eave), "open edge {a:?} {b:?} x{c}");
            }
            // the projection of the sloped faces covers the outline with overhang exactly once
            let plan: f64 = r
                .faces
                .iter()
                .filter(|f| f.sloped)
                .map(|f| poly::area(&f.pts.iter().map(|p| v2(p.x, p.y)).collect::<Vec<_>>()).abs())
                .sum();
            if overhang == 0.0 {
                near(plan, 60.0, "plan");
            } else {
                assert!(plan > 60.0);
            }
        }
    }

    #[test]
    fn collinear_and_reflex_inputs() {
        // extra vertex on an edge, clockwise winding
        let ring = vec![
            v2(0.0, 0.0),
            v2(0.0, 6.0),
            v2(10.0, 6.0),
            v2(10.0, 0.0),
            v2(5.0, 0.0),
        ];
        let r = Footprint {
            ring,
            slopes: vec![Some(30.0); 5],
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        near(r.quantities().ridge, 4.0, "ridge");
        // cross shape (4 reflex vertices)
        let c = vec![
            v2(4.0, 0.0),
            v2(8.0, 0.0),
            v2(8.0, 4.0),
            v2(12.0, 4.0),
            v2(12.0, 8.0),
            v2(8.0, 8.0),
            v2(8.0, 12.0),
            v2(4.0, 12.0),
            v2(4.0, 8.0),
            v2(0.0, 8.0),
            v2(0.0, 4.0),
            v2(4.0, 4.0),
        ];
        let r = Footprint {
            ring: c,
            slopes: vec![Some(45.0); 12],
            base_z: 0.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let q = r.quantities();
        near(q.plan_area, 80.0, "cross plan");
        near(q.area, 80.0 * 2f64.sqrt(), "cross area");
        near(q.ridge, 16.0, "cross ridges");
        assert!(r.mesh(0.1).is_watertight());
        assert!(matches!(
            Footprint {
                ring: rect(10.0, 6.0),
                slopes: vec![None; 4],
                base_z: 0.0,
                overhang: 0.0,
                mansard: None
            }
            .build(),
            Err(RoofError::NoSlopedEdge)
        ));
    }

    #[test]
    fn extrusion_and_underside_profile() {
        let e = Extrusion {
            a: v2(0.0, 0.0),
            b: v2(8.0, 0.0),
            profile: vec![(0.0, 0.0), (4.0, 2.0), (8.0, 0.0)],
            depth: 10.0,
        };
        let r = e.build(3.0).unwrap();
        let q = r.quantities();
        near(q.area, 2.0 * 20f64.sqrt() * 10.0, "area");
        near(q.volume_under, 8.0 * 2.0 / 2.0 * 10.0, "volume");
        near(q.ridge, 10.0, "ridge");
        // wall along y=5 under the ridge: roof underside = top − t/cos
        let h = Footprint {
            ring: rect(10.0, 6.0),
            slopes: vec![Some(45.0), None, Some(45.0), None],
            base_z: 3.0,
            overhang: 0.0,
            mansard: None,
        }
        .build()
        .unwrap();
        let p = h.underside_along(v2(0.0, 0.0), v2(0.0, 6.0), 0.0);
        assert_eq!(p.len(), 3, "{p:?}");
        near(p[1].0, 3.0, "u ridge");
        near(p[1].1, 6.0, "z ridge");
        let p = h.underside_along(v2(0.0, 0.0), v2(0.0, 6.0), 0.1);
        near(p[0].1, 3.0 - 0.1 * 2f64.sqrt(), "underside at eave");
    }
}
