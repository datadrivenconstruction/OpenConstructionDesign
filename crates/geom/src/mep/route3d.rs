// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Auto-routing of networks on a 3D lattice. A* over axis-aligned moves: a turn costs extra and
//! needs a straight long enough for two elbows (one next to a port), walls are crossed at a cost
//! (the caller cuts a sleeve), slabs only vertically, hard obstacles never; horizontal metres
//! outside the preferred band cost more, metres along attractive lines (grid axes) less.
//! A tree for many targets grows nearest-first: each new path ends perpendicular on a straight
//! part of the tree, far enough from bends and other joins for a tee. Pure geometry: the model
//! supplies obstacles and terminals and turns the branches into runs.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::math::{v2, v3, V2, V3};

const EPS: f64 = 1e-6;
/// Spatial hash cell, m.
const CELL: f64 = 2.0;
const NONE: u8 = 6;
const DIRS: [V3; 6] = [
    v3(1.0, 0.0, 0.0),
    v3(-1.0, 0.0, 0.0),
    v3(0.0, 1.0, 0.0),
    v3(0.0, -1.0, 0.0),
    v3(0.0, 0.0, 1.0),
    v3(0.0, 0.0, -1.0),
];

/// How an obstacle may be crossed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// Never: columns, beams, doors, windows, other networks.
    Block,
    /// In any direction at a cost (a sleeve or a chase): walls.
    Wall,
    /// Only vertically, at a cost (a sleeve): slabs.
    Slab,
}

/// A convex prism: footprint ring (either orientation), bottom and top.
#[derive(Clone, Debug)]
pub struct Obstacle {
    pub ring: Vec<V2>,
    pub z0: f64,
    pub z1: f64,
    pub pass: Pass,
    /// The caller's tag (wall or slab id), reported with crossings.
    pub tag: i64,
}

impl Obstacle {
    pub fn aabb(lo: V3, hi: V3, pass: Pass, tag: i64) -> Self {
        Obstacle {
            ring: vec![v2(lo.x, lo.y), v2(hi.x, lo.y), v2(hi.x, hi.y), v2(lo.x, hi.y)],
            z0: lo.z,
            z1: hi.z,
            pass,
            tag,
        }
    }
    fn bbox(&self) -> (V2, V2) {
        let mut lo = v2(f64::INFINITY, f64::INFINITY);
        let mut hi = v2(f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in &self.ring {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        }
        (lo, hi)
    }
}

/// Obstacles with a spatial hash in plan.
#[derive(Clone, Debug, Default)]
pub struct Index {
    pub obstacles: Vec<Obstacle>,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

impl Index {
    pub fn new(obstacles: Vec<Obstacle>) -> Self {
        let mut ix = Index {
            obstacles: vec![],
            cells: HashMap::new(),
        };
        for o in obstacles {
            ix.push(o);
        }
        ix
    }
    pub fn push(&mut self, o: Obstacle) {
        let id = self.obstacles.len() as u32;
        let (lo, hi) = o.bbox();
        for c in cells_of(lo, hi, 0.5) {
            self.cells.entry(c).or_default().push(id);
        }
        self.obstacles.push(o);
    }
    /// Obstacles whose plan box comes within `c` of the box lo..hi.
    fn near(&self, lo: V2, hi: V2, c: f64, out: &mut Vec<u32>) {
        out.clear();
        for k in cells_of(lo, hi, c) {
            if let Some(v) = self.cells.get(&k) {
                out.extend_from_slice(v);
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

fn cells_of(lo: V2, hi: V2, pad: f64) -> impl Iterator<Item = (i64, i64)> {
    let f = |v: f64| (v / CELL).floor() as i64;
    let (x0, x1, y0, y1) = (f(lo.x - pad), f(hi.x + pad), f(lo.y - pad), f(hi.y + pad));
    (x0..=x1).flat_map(move |x| (y0..=y1).map(move |y| (x, y)))
}

fn seg_point(a: V2, b: V2, p: V2) -> f64 {
    let d = b - a;
    let dd = d.dot(d);
    let t = if dd > 1e-18 {
        ((p - a).dot(d) / dd).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + d * t - p).len()
}

fn segs_cross(a: V2, b: V2, c: V2, d: V2) -> bool {
    let o = |p: V2, q: V2, r: V2| (q - p).cross(r - p);
    let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
}

/// Distance in plan between the segment a..b and a convex ring (0 when they overlap).
pub fn seg_ring_dist(ring: &[V2], a: V2, b: V2) -> f64 {
    let inside = |p: V2| crate::predicates::point_in_ring(p, ring) >= 0;
    if inside(a) || inside(b) {
        return 0.0;
    }
    let n = ring.len();
    let mut d = f64::INFINITY;
    for i in 0..n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        if segs_cross(a, b, p, q) {
            return 0.0;
        }
        d = d.min(seg_point(a, b, p)).min(seg_point(p, q, a)).min(seg_point(p, q, b));
    }
    d
}

/// Distance between two 3D segments (closest points, clamped).
pub fn seg_seg_dist(p1: V3, q1: V3, p2: V3, q2: V3) -> f64 {
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.dot(d1), d2.dot(d2), d2.dot(r));
    let (s, t);
    if a <= 1e-18 && e <= 1e-18 {
        return r.len();
    }
    if a <= 1e-18 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-18 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut s0 = if den > 1e-18 {
                ((b * f - c * e) / den).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    ((p1 + d1 * s) - (p2 + d2 * t)).len()
}

/// A route end: a point and the outward direction a run must leave it along.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Terminal {
    pub at: V3,
    pub dir: Option<V3>,
}

/// Where a tree starts.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A port: the first path enters it against its direction.
    Terminal(Terminal),
    /// Straight lines already there (a riser, a main): paths join them perpendicular.
    Lines(Vec<(V3, V3)>),
}

/// Weights and limits of one search.
#[derive(Clone, Debug)]
pub struct Rules {
    /// Half the run envelope plus the gap kept to everything, m.
    pub clearance: f64,
    /// Lattice step, m.
    pub step: f64,
    /// Shortest straight between two elbows, m.
    pub min_leg: f64,
    /// Shortest straight next to a port before an elbow, m.
    pub tangent: f64,
    /// Shortest straight before a tee on the tree, m.
    pub tee_leg: f64,
    /// Equivalent metres of one elbow.
    pub bend_cost: f64,
    /// Equivalent metres of one wall crossing.
    pub wall_cost: f64,
    /// Factor on metres run inside a wall (a chase).
    pub in_wall: f64,
    /// Equivalent metres of one slab crossing.
    pub slab_cost: f64,
    /// Preferred z band of horizontal runs.
    pub zone: (f64, f64),
    /// Factor on horizontal metres outside the band.
    pub off_zone: f64,
    /// Attractive x lines and y lines (grid axes).
    pub axes: (Vec<f64>, Vec<f64>),
    /// Factor (<= 1) on metres along an attractive line.
    pub axis_gain: f64,
    /// z of horizontal travel.
    pub layers: Vec<f64>,
    /// Plan bounds of the lattice.
    pub bounds: (V2, V2),
    /// Expanded states per path before giving up.
    pub max_states: usize,
    /// Per target (by index): the run of its own size on the straight leaving its port, where
    /// it is not the trunk's. Empty or None: the trunk's clearance and tangent.
    pub ends: Vec<Option<EndRun>>,
}

/// The run at a target's port: the target's own size, not the trunk's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndRun {
    /// Half its envelope plus the gap, m.
    pub clearance: f64,
    /// Shortest straight next to the port before an elbow, m.
    pub tangent: f64,
}

impl Rules {
    pub fn new(clearance: f64, min_leg: f64, layers: Vec<f64>, bounds: (V2, V2)) -> Self {
        let zone = layers.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &z| {
            (a.0.min(z), a.1.max(z))
        });
        Rules {
            clearance,
            step: min_leg.max(0.4),
            min_leg,
            tangent: min_leg * 0.5,
            tee_leg: min_leg * 0.5,
            bend_cost: 1.0,
            wall_cost: 2.0,
            in_wall: 3.0,
            slab_cost: 1.0,
            zone,
            off_zone: 4.0,
            axes: (vec![], vec![]),
            axis_gain: 1.0,
            layers,
            bounds,
            max_states: 400_000,
            ends: vec![],
        }
    }
}

/// A crossing of a wall or slab by a path: the caller's tag and the crossing segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crossing {
    pub tag: i64,
    pub pass: Pass,
    pub a: V3,
    pub b: V3,
}

/// One routed branch: points from its target to the join on the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub target: usize,
    pub points: Vec<V3>,
    /// The branch it joins (None: the source) and where.
    pub parent: Option<usize>,
    pub join: V3,
    /// Ends on the tree (a tee), not on the source port.
    pub tee: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub branches: Vec<Branch>,
    /// Targets no path reached.
    pub failed: Vec<usize>,
}

impl Tree {
    pub fn length(&self) -> f64 {
        self.branches.iter().map(|b| poly_len(&b.points)).sum()
    }
    /// Elbows (turns inside branches) and tees (joins on a branch or a line).
    pub fn fittings(&self) -> (usize, usize) {
        let bends = self.branches.iter().map(|b| turns(&b.points)).sum();
        (bends, self.branches.iter().filter(|b| b.tee).count())
    }
}

pub fn poly_len(p: &[V3]) -> f64 {
    p.windows(2).map(|w| (w[1] - w[0]).len()).sum()
}

fn turns(p: &[V3]) -> usize {
    p.windows(3)
        .filter(|w| {
            let (a, b) = ((w[1] - w[0]).normalized(), (w[2] - w[1]).normalized());
            a.cross(b).len() > 1e-6
        })
        .count()
}

/// Drops collinear and repeated points.
pub fn clean(p: &[V3]) -> Vec<V3> {
    let mut out: Vec<V3> = vec![];
    for &q in p {
        if out.last().is_some_and(|l| (q - *l).len() < 1e-9) {
            continue;
        }
        while out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (d1, d2) = ((b - a).normalized(), (q - b).normalized());
            if d1.cross(d2).len() < 1e-9 && d1.dot(d2) > 0.0 {
                out.pop();
            } else {
                break;
            }
        }
        out.push(q);
    }
    out
}

fn axis_of(d: V3) -> Option<u8> {
    let d = d.normalized();
    (0..6u8).find(|&k| (DIRS[k as usize] - d).len() < 1e-6)
}

/// Sorted unique coordinates.
fn uniq(mut v: Vec<f64>) -> Vec<f64> {
    v.retain(|x| x.is_finite());
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mut out: Vec<f64> = vec![];
    for x in v {
        if out.last().is_none_or(|l| x - *l > 1e-4) {
            out.push(x);
        }
    }
    out
}

struct Lattice {
    xs: Vec<f64>,
    ys: Vec<f64>,
    zs: Vec<f64>,
}

impl Lattice {
    /// Coordinates of the lines along axis `k` (0 x, 1 y, 2 z).
    fn line(&self, k: usize) -> &[f64] {
        match k {
            0 => &self.xs,
            1 => &self.ys,
            _ => &self.zs,
        }
    }
    fn find(v: &[f64], x: f64) -> Option<usize> {
        let i = v.partition_point(|a| *a < x - 1e-4);
        (i < v.len() && (v[i] - x).abs() <= 1e-4).then_some(i)
    }
    fn node(&self, p: V3) -> Option<u32> {
        let (i, j, k) = (
            Self::find(&self.xs, p.x)?,
            Self::find(&self.ys, p.y)?,
            Self::find(&self.zs, p.z)?,
        );
        Some(((k * self.ys.len() + j) * self.xs.len() + i) as u32)
    }
    fn ijk(&self, n: u32) -> (usize, usize, usize) {
        let n = n as usize;
        let (nx, ny) = (self.xs.len(), self.ys.len());
        (n % nx, (n / nx) % ny, n / (nx * ny))
    }
    fn at(&self, n: u32) -> V3 {
        let (i, j, k) = self.ijk(n);
        v3(self.xs[i], self.ys[j], self.zs[k])
    }
    fn step(&self, n: u32, d: u8) -> Option<u32> {
        let (i, j, k) = self.ijk(n);
        let (nx, ny, nz) = (self.xs.len(), self.ys.len(), self.zs.len());
        let (i, j, k) = match d {
            0 if i + 1 < nx => (i + 1, j, k),
            1 if i > 0 => (i - 1, j, k),
            2 if j + 1 < ny => (i, j + 1, k),
            3 if j > 0 => (i, j - 1, k),
            4 if k + 1 < nz => (i, j, k + 1),
            5 if k > 0 => (i, j, k - 1),
            _ => return None,
        };
        Some(((k * ny + j) * nx + i) as u32)
    }
}

/// What an edge costs and crosses; None when blocked.
#[derive(Clone, Debug)]
struct EdgeInfo {
    walls: Vec<i64>,
    slabs: Vec<i64>,
}

/// A routed segment others keep clear of: ends and its clearance.
#[derive(Clone, Copy, Debug)]
pub struct Routed {
    pub a: V3,
    pub b: V3,
    pub clearance: f64,
}

/// Routed segments with a spatial hash in plan.
#[derive(Clone, Debug, Default)]
pub struct RoutedIndex {
    pub segs: Vec<Routed>,
    cells: HashMap<(i64, i64), Vec<u32>>,
    /// Largest clearance stored: the query pad.
    pad: f64,
}

impl RoutedIndex {
    pub fn new(segs: Vec<Routed>) -> Self {
        let mut ix = RoutedIndex::default();
        for s in segs {
            ix.push(s);
        }
        ix
    }
    pub fn push(&mut self, s: Routed) {
        let id = self.segs.len() as u32;
        let lo = v2(s.a.x.min(s.b.x), s.a.y.min(s.b.y));
        let hi = v2(s.a.x.max(s.b.x), s.a.y.max(s.b.y));
        for c in cells_of(lo, hi, s.clearance) {
            self.cells.entry(c).or_default().push(id);
        }
        self.pad = self.pad.max(s.clearance);
        self.segs.push(s);
    }
    /// The segment a..b keeps `c` plus each stored clearance; segments through `touch` are
    /// ignored (a join arrives on them).
    pub fn clear(&self, a: V3, b: V3, c: f64, touch: Option<V3>) -> bool {
        let lo = v2(a.x.min(b.x), a.y.min(b.y));
        let hi = v2(a.x.max(b.x), a.y.max(b.y));
        let mut seen: Vec<u32> = vec![];
        for k in cells_of(lo, hi, c + self.pad) {
            for &id in self.cells.get(&k).into_iter().flatten() {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
                let r = &self.segs[id as usize];
                if touch.is_some_and(|m| seg_seg_dist(m, m, r.a, r.b) < 1e-6) {
                    continue;
                }
                if seg_seg_dist(a, b, r.a, r.b) < c + r.clearance - EPS {
                    return false;
                }
            }
        }
        true
    }
}

/// The search state of one tree.
struct Search<'a> {
    /// Queue entries skipped as out of date.
    stale: usize,
    ix: &'a Index,
    rules: &'a Rules,
    lat: Lattice,
    /// By (node, axis, clearance bits).
    edges: HashMap<(u32, u8, u64), Option<EdgeInfo>>,
    inside: HashMap<u32, Vec<i64>>,
    near: Vec<u32>,
    others: RoutedIndex,
    /// Port straights of the targets not routed yet (target, port, escape, clearance): an
    /// earlier branch keeps clear of them; a target's own leaves the list as it is routed.
    pending: Vec<(usize, V3, V3, f64)>,
    last_exp: usize,
    last_box: (V3, V3),
    last_zs: Vec<i64>,
}

#[derive(PartialEq)]
struct Open(f64, u64);
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.partial_cmp(&self.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| o.1.cmp(&self.1))
    }
}

/// State key: node, incoming direction, accumulated straight in quarters of `min_leg`
/// (a lower bound, capped at 4), and whether no turn was made yet.
fn key(n: u32, d: u8, q: u8, first: bool) -> u64 {
    ((n as u64) << 16) | ((d as u64) << 8) | ((q as u64) << 1) | first as u64
}
fn unkey(k: u64) -> (u32, u8, u8, bool) {
    ((k >> 16) as u32, (k >> 8) as u8, ((k >> 1) & 0x7f) as u8, k & 1 == 1)
}

/// Joinable lattice nodes of the tree: node -> axis of the straight part it lies on.
type Joins = HashMap<u32, u8>;

impl<'a> Search<'a> {
    fn new(ix: &'a Index, rules: &'a Rules, extra: &[V3], others: RoutedIndex) -> Self {
        let (lo, hi) = rules.bounds;
        let s = rules.step.max(0.05);
        let regular = |a: f64, b: f64| -> Vec<f64> {
            let n = ((b - a) / s).floor().max(0.0) as usize;
            (0..=n).map(|k| a + k as f64 * s).collect()
        };
        let mut xs = regular(lo.x, hi.x);
        let mut ys = regular(lo.y, hi.y);
        let mut zs = rules.layers.clone();
        xs.extend(rules.axes.0.iter().filter(|x| (lo.x..=hi.x).contains(*x)));
        ys.extend(rules.axes.1.iter().filter(|y| (lo.y..=hi.y).contains(*y)));
        for p in extra {
            xs.push(p.x);
            ys.push(p.y);
            zs.push(p.z);
        }
        // lines closer than a short leg to a regular line would only make jogs
        let thin = |v: Vec<f64>, keep: &[f64]| -> Vec<f64> {
            let v = uniq(v);
            let mut out: Vec<f64> = vec![];
            for x in v {
                let must = keep.iter().any(|k| (k - x).abs() <= 1e-4);
                if let Some(l) = out.last_mut() {
                    if x - *l < rules.min_leg * 0.5 && !must {
                        continue;
                    }
                    if x - *l < rules.min_leg * 0.5 && must {
                        let lk = keep.iter().any(|k| (k - *l).abs() <= 1e-4);
                        if !lk {
                            *l = x;
                            continue;
                        }
                    }
                }
                out.push(x);
            }
            out
        };
        let kx: Vec<f64> = extra.iter().map(|p| p.x).collect();
        let ky: Vec<f64> = extra.iter().map(|p| p.y).collect();
        let kz: Vec<f64> = extra.iter().map(|p| p.z).chain(rules.layers.clone()).collect();
        Search {
            ix,
            rules,
            lat: Lattice {
                xs: thin(xs, &kx),
                ys: thin(ys, &ky),
                zs: uniq(zs.into_iter().chain(kz).collect()),
            },
            edges: HashMap::new(),
            inside: HashMap::new(),
            near: vec![],
            others,
            pending: vec![],
            last_exp: 0,
            last_box: (v3(1e9, 1e9, 1e9), v3(-1e9, -1e9, -1e9)),
            last_zs: vec![],
            stale: 0,
        }
    }

    /// Adds a line along axis `k` at `c`; node numbers change, so the caches by node go.
    fn add_line(&mut self, k: usize, c: f64) {
        let v = match k {
            0 => &mut self.lat.xs,
            1 => &mut self.lat.ys,
            _ => &mut self.lat.zs,
        };
        if Lattice::find(v, c).is_some() {
            return;
        }
        let i = v.partition_point(|a| *a < c);
        v.insert(i, c);
        self.edges.clear();
        self.inside.clear();
    }

    fn hits(&mut self, a: V3, b: V3, c: f64) -> Vec<(u32, f64)> {
        let (lo, hi) = (v2(a.x.min(b.x), a.y.min(b.y)), v2(a.x.max(b.x), a.y.max(b.y)));
        let mut near = std::mem::take(&mut self.near);
        self.ix.near(lo, hi, c, &mut near);
        let mut out = vec![];
        for &id in &near {
            let o = &self.ix.obstacles[id as usize];
            if a.z.max(b.z) <= o.z0 - c + EPS || a.z.min(b.z) >= o.z1 + c - EPS {
                continue;
            }
            let d = seg_ring_dist(&o.ring, v2(a.x, a.y), v2(b.x, b.y));
            if d < c - EPS {
                out.push((id, d));
            }
        }
        self.near = near;
        out
    }

    fn inside_walls(&mut self, n: u32) -> Vec<i64> {
        if let Some(v) = self.inside.get(&n) {
            return v.clone();
        }
        let p = self.lat.at(n);
        let v: Vec<i64> = self
            .hits(p, p, self.rules.clearance)
            .into_iter()
            .map(|(id, _)| &self.ix.obstacles[id as usize])
            .filter(|o| o.pass == Pass::Wall)
            .map(|o| o.tag)
            .collect();
        self.inside.insert(n, v.clone());
        v
    }

    fn edge(&mut self, n: u32, d: u8, c: f64) -> Option<EdgeInfo> {
        let k = (n.min(self.lat.step(n, d)?), d / 2, c.to_bits());
        if let Some(e) = self.edges.get(&k) {
            return e.clone();
        }
        let (a, b) = (self.lat.at(n), self.lat.at(self.lat.step(n, d)?));
        let vertical = d >= 4;
        let mut info = Some(EdgeInfo {
            walls: vec![],
            slabs: vec![],
        });
        for (id, _) in self.hits(a, b, c) {
            let o = &self.ix.obstacles[id as usize];
            match (o.pass, &mut info) {
                (_, None) => break,
                (Pass::Block, _) => info = None,
                (Pass::Slab, Some(_)) if !vertical => info = None,
                (Pass::Slab, Some(i)) => i.slabs.push(o.tag),
                (Pass::Wall, Some(i)) => i.walls.push(o.tag),
            }
        }
        self.edges.insert(k, info.clone());
        info
    }

    /// `touch`: a join point the edge may end on (it arrives perpendicular there).
    fn free_of_others(&self, a: V3, b: V3, touch: Option<V3>, c: f64) -> bool {
        self.others.clear(a, b, c, touch)
            && self
                .pending
                .iter()
                .all(|&(_, p, q, pc)| seg_seg_dist(a, b, p, q) >= c + pc - EPS)
    }

    fn cost(&mut self, n: u32, d: u8, info: &EdgeInfo) -> f64 {
        let (a, b) = (self.lat.at(n), self.lat.at(self.lat.step(n, d).unwrap_or(n)));
        let r = self.rules;
        let l = (b - a).len();
        let mut f = 1.0;
        if d < 4 {
            if a.z < r.zone.0 - 1e-4 || a.z > r.zone.1 + 1e-4 {
                f *= r.off_zone;
            }
            let on_axis = if d < 2 {
                r.axes.1.iter().any(|y| (y - a.y).abs() < 1e-4)
            } else {
                r.axes.0.iter().any(|x| (x - a.x).abs() < 1e-4)
            };
            if on_axis {
                f *= r.axis_gain;
            }
        }
        let mut c = l * f;
        if !info.walls.is_empty() {
            let from = self.inside_walls(n);
            let to = self.inside_walls(self.lat.step(n, d).unwrap_or(n));
            for w in &info.walls {
                if from.contains(w) && to.contains(w) {
                    c += l * r.in_wall;
                } else if !from.contains(w) {
                    c += r.wall_cost;
                }
            }
        }
        c + info.slabs.len() as f64 * r.slab_cost
    }

    /// A* from `start` (leaving along `start_dir` when it is an axis) to the goal; returns the
    /// lattice points of the path. `end`: the straight leaving the port is the target's own run.
    fn path(
        &mut self,
        start: V3,
        start_dir: Option<u8>,
        start_acc: f64,
        goal: &Goal<'_>,
        end: Option<EndRun>,
    ) -> Option<Vec<V3>> {
        let r = self.rules;
        self.last_exp = 0;
        self.last_box = (v3(1e9, 1e9, 1e9), v3(-1e9, -1e9, -1e9));
        self.last_zs.clear();
        let n0 = self.lat.node(start)?;
        let quarter = (r.min_leg / 4.0).max(1e-6);
        let qof = |acc: f64| ((acc / quarter + 1e-6).floor() as u8).min(4);
        // an own-size port straight already made by the escape needs nothing more; quarters of
        // the trunk's leg would round it away
        let tq = match end {
            Some(e) if start_acc + 1e-9 >= e.tangent => 0,
            Some(e) => ((e.tangent / quarter) - 1e-6).ceil().max(0.0) as u8,
            None if start_acc + 1e-9 >= r.tangent => 0,
            None => ((r.tangent / quarter) - 1e-6).ceil().max(0.0) as u8,
        };
        let teeq = ((r.tee_leg / quarter) - 1e-6).ceil().max(0.0) as u8;
        let h = |p: V3| -> f64 {
            let (lo, hi) = goal.bbox;
            let dx = (lo.x - p.x).max(p.x - hi.x).max(0.0);
            let dy = (lo.y - p.y).max(p.y - hi.y).max(0.0);
            let dz = (lo.z - p.z).max(p.z - hi.z).max(0.0);
            (dx + dy + dz) * r.axis_gain.min(1.0)
        };
        let mut g: HashMap<u64, f64> = HashMap::new();
        let mut parent: HashMap<u64, u64> = HashMap::new();
        let mut open = BinaryHeap::new();
        let k0 = key(n0, start_dir.unwrap_or(NONE), qof(start_acc), true);
        g.insert(k0, 0.0);
        open.push(Open(h(start), k0));
        let mut expanded = 0usize;
        while let Some(Open(f, k)) = open.pop() {
            let (n, d, q, first) = unkey(k);
            let gk = g[&k];
            // an entry pushed before a cheaper way to this state was found
            if stale(f, gk, h(self.lat.at(n))) {
                self.stale += 1;
                continue;
            }
            if goal.reached(n, d, q, teeq) {
                let mut pts = vec![self.lat.at(n)];
                let mut c = k;
                while let Some(&p) = parent.get(&c) {
                    pts.push(self.lat.at(unkey(p).0));
                    c = p;
                }
                pts.reverse();
                return Some(pts);
            }
            expanded += 1;
            self.last_exp = expanded;
            { let p = self.lat.at(n); let (lo, hi) = &mut self.last_box; *lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)); *hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)); if std::env::var("R3ZS").is_ok() { self.last_zs.push((p.z * 100.0).round() as i64); } }
            if expanded > r.max_states {
                return None;
            }
            for nd in 0..6u8 {
                if d != NONE && nd == (d ^ 1) {
                    continue;
                }
                let turn = d != NONE && nd != d;
                if turn && q < if first { tq } else { 4 } {
                    continue;
                }
                let Some(m) = self.lat.step(n, nd) else {
                    continue;
                };
                let nfirst = if turn || d == NONE { first && d == NONE } else { first };
                let cl = match end {
                    Some(e) if nfirst => e.clearance,
                    _ => r.clearance,
                };
                let Some(info) = self.edge(n, nd, cl) else {
                    continue;
                };
                let (a, b) = (self.lat.at(n), self.lat.at(m));
                let touch = goal.joins.get(&m).filter(|ax| **ax != nd / 2).map(|_| b);
                if !self.free_of_others(a, b, touch, cl) {
                    continue;
                }
                let l = (b - a).len();
                let nq = if turn || d == NONE { qof(l) } else { qof(q as f64 * quarter + l) };
                let mut c = self.cost(n, nd, &info);
                if turn {
                    c += r.bend_cost;
                }
                let nk = key(m, nd, nq, nfirst);
                let ng = gk + c;
                if g.get(&nk).is_none_or(|&o| ng < o - 1e-12) {
                    g.insert(nk, ng);
                    parent.insert(nk, k);
                    open.push(Open(ng + h(b), nk));
                }
            }
        }
        None
    }
}

/// A queue entry of priority `f` for a state now known at cost `g` with estimate `h`.
fn stale(f: f64, g: f64, h: f64) -> bool {
    f > g + h + 1e-9
}

/// Points in a uniform grid: is any point within a distance without scanning them all.
struct PointGrid {
    cell: f64,
    cells: HashMap<(i64, i64, i64), Vec<V3>>,
}

impl PointGrid {
    fn new(cell: f64) -> Self {
        PointGrid {
            cell: cell.max(1e-6),
            cells: HashMap::new(),
        }
    }
    fn key(&self, p: V3) -> (i64, i64, i64) {
        let k = |v: f64| (v / self.cell).floor() as i64;
        (k(p.x), k(p.y), k(p.z))
    }
    fn add(&mut self, p: V3) {
        let k = self.key(p);
        self.cells.entry(k).or_default().push(p);
    }
    /// Any point closer than `r` (at most one cell) to `p`.
    fn any_within(&self, p: V3, r: f64) -> bool {
        debug_assert!(r <= self.cell + 1e-9);
        let (x, y, z) = self.key(p);
        (-1..=1).any(|dx| {
            (-1..=1).any(|dy| {
                (-1..=1).any(|dz| {
                    self.cells
                        .get(&(x + dx, y + dy, z + dz))
                        .is_some_and(|v| v.iter().any(|q| (*q - p).len() < r))
                })
            })
        })
    }
}

/// What a path must reach.
struct Goal<'a> {
    /// A terminal node and the direction to arrive moving in (None: any).
    node: Option<(u32, Option<u8>)>,
    joins: &'a Joins,
    bbox: (V3, V3),
}

impl Goal<'_> {
    fn reached(&self, n: u32, d: u8, q: u8, teeq: u8) -> bool {
        if let Some((t, arrive)) = self.node {
            // on the port's axis, or turning there off a full leg: the escape is the port's straight
            return n == t
                && match arrive {
                    Some(a) => d == a || (d != NONE && d != (a ^ 1) && q >= 4),
                    None => q >= 2,
                };
        }
        match self.joins.get(&n) {
            Some(&axis) => d != NONE && d / 2 != axis && q >= teeq,
            None => false,
        }
    }
}

/// Coordinate `k` (0 x, 1 y, 2 z) of a point.
fn comp(p: V3, k: usize) -> f64 {
    match k {
        0 => p.x,
        1 => p.y,
        _ => p.z,
    }
}

/// The point with coordinate `k` set to `c`.
fn with(p: V3, k: usize, c: f64) -> V3 {
    match k {
        0 => v3(c, p.y, p.z),
        1 => v3(p.x, c, p.z),
        _ => v3(p.x, p.y, c),
    }
}

/// Escape point of a terminal: along its direction by the tangent (or itself).
fn escape(t: &Terminal, r: &Rules, end: Option<EndRun>) -> (V3, Option<u8>, f64) {
    match t.dir {
        Some(d) if d.len() > 1e-9 => {
            let d = d.normalized();
            let l = end.map_or(r.tangent, |e| e.tangent) * 1.05;
            (t.at + d * l, axis_of(d), l)
        }
        _ => (t.at, None, r.min_leg),
    }
}

/// Grows a tree from the source to every target; `others` are runs already placed (other
/// systems), and the tree's own segments are added to it as they are routed.
pub fn route_tree(
    ix: &Index,
    rules: &Rules,
    source: &Source,
    targets: &[Terminal],
    others: &mut RoutedIndex,
) -> Tree {
    let end_of = |i: usize| rules.ends.get(i).copied().flatten();
    let mut extra: Vec<V3> = vec![];
    for (i, t) in targets.iter().enumerate() {
        extra.push(t.at);
        extra.push(escape(t, rules, end_of(i)).0);
    }
    match source {
        Source::Terminal(t) => {
            extra.push(t.at);
            extra.push(escape(t, rules, None).0);
        }
        Source::Lines(l) => {
            for (a, b) in l {
                extra.push(*a);
                extra.push(*b);
                for z in &rules.layers {
                    if (a.z.min(b.z)..=a.z.max(b.z)).contains(z) {
                        extra.push(v3(a.x, a.y, *z));
                    }
                }
            }
        }
    }
    let mut s = Search::new(ix, rules, &extra, std::mem::take(others));
    s.pending = targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.dir.is_some())
        .map(|(i, t)| {
            let c = end_of(i).map_or(rules.clearance, |e| e.clearance);
            (i, t.at, escape(t, rules, end_of(i)).0, c)
        })
        .collect();
    let mut tree = Tree::default();
    // tree straight parts: (a, b, branch or None for the source)
    let mut segs: Vec<(V3, V3, Option<usize>)> = vec![];
    if let Source::Lines(l) = source {
        segs.extend(l.iter().map(|&(a, b)| (a, b, None)));
    }
    // a tee keeps a leg from either end of the straight it is on and from the next tee: the
    // branch's own straight (`tee_leg`) is off the main, so two tees need not hold two of it;
    // a grid of that cell answers "any close?" in O(1)
    let gap = rules.min_leg.max(rules.tee_leg);
    let edge = gap;
    // a port's straight (its escape) carries what sits on the port: no tee lands on it
    let mut port_runs: Vec<(V3, f64)> = targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.dir.is_some())
        .map(|(i, t)| (t.at, escape(t, rules, end_of(i)).2))
        .collect();
    if let Source::Terminal(st) = source {
        if st.dir.is_some() {
            port_runs.push((st.at, escape(st, rules, None).2));
        }
    }
    let margin = |p: V3| {
        port_runs
            .iter()
            .filter(|(q, _)| (p - *q).len() < 1e-9)
            .fold(edge, |m, &(_, l)| m.max(l))
    };
    let mut joined = PointGrid::new(gap);
    let mut left: Vec<usize> = (0..targets.len()).collect();
    // distance of each target to the tree, kept up to date as segments are added: the tree only
    // grows, so a target never needs the whole tree measured again
    let mut near: Vec<f64> = targets
        .iter()
        .map(|t| match source {
            Source::Terminal(st) => (t.at - st.at).len(),
            Source::Lines(l) => l
                .iter()
                .map(|(a, b)| seg_seg_dist(t.at, t.at, *a, *b))
                .fold(f64::INFINITY, f64::min),
        })
        .collect();
    while !left.is_empty() {
        // nearest target to the tree first (to the source while the tree is empty)
        let (pos, &ti) = left
            .iter()
            .enumerate()
            .min_by(|a, b| near[*a.1].total_cmp(&near[*b.1]).then(a.1.cmp(b.1)))
            .expect("left is not empty");
        left.remove(pos);
        s.pending.retain(|p| p.0 != ti);
        let t = targets[ti];
        let (e, edir, eacc) = escape(&t, rules, end_of(ti));
        // a straight long enough for a tee whose middle holds no lattice line gets one, at the
        // point of the middle nearest the target: the regular lines fall where the bounds put them
        let mut lines: Vec<(usize, f64)> = vec![];
        for &(a, b, _) in &segs {
            let Some(axis) = axis_of(b - a) else {
                continue;
            };
            let k = (axis / 2) as usize;
            let len = (b - a).len();
            let (m0, m1) = (margin(a), margin(b));
            if len < m0 + m1 {
                continue;
            }
            let dir = (b - a).normalized();
            let (u0, u1) = (comp(a + dir * m0, k), comp(b - dir * m1, k));
            let (lo, hi) = (u0.min(u1), u0.max(u1));
            let open = |c: f64| !joined.any_within(with(a, k, c), gap - 1e-6);
            let has = s.lat.line(k).iter().any(|&c| c >= lo - 1e-6 && c <= hi + 1e-6 && open(c));
            let c = comp(e, k).clamp(lo, hi);
            if !has && open(c) {
                lines.push((k, c));
            }
        }
        for (k, c) in lines {
            s.add_line(k, c);
        }
        let mut joins: Joins = HashMap::new();
        let mut seg_of: HashMap<u32, (V3, V3)> = HashMap::new();
        let mut lo = v3(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(a, b, _) in &segs {
            let Some(axis) = axis_of(b - a) else {
                continue;
            };
            let len = (b - a).len();
            let (m0, m1) = (margin(a), margin(b));
            let dir = (b - a).normalized();
            let mut nodes = vec![];
            let along = |p: V3| (p - a).dot(dir);
            // lattice nodes on the segment
            let coords: Vec<f64> = s.lat.line((axis / 2) as usize).to_vec();
            for c in coords {
                let p = match axis / 2 {
                    0 => v3(c, a.y, a.z),
                    1 => v3(a.x, c, a.z),
                    _ => v3(a.x, a.y, c),
                };
                let u = along(p);
                if u < m0 - 1e-6 || u > len - m1 + 1e-6 {
                    continue;
                }
                if joined.any_within(p, gap - 1e-6) {
                    continue;
                }
                if let Some(n) = s.lat.node(p) {
                    nodes.push((n, p));
                }
            }
            for (n, p) in nodes {
                joins.insert(n, axis / 2);
                seg_of.insert(n, (a, b));
                lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
                hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
            }
        }
        let empty = HashMap::new();
        let goal = match (source, segs.is_empty()) {
            (Source::Terminal(st), true) => {
                let (se, sdir, _) = escape(st, rules, None);
                let Some(n) = s.lat.node(se) else {
                    tree.failed.push(ti);
                    continue;
                };
                Goal {
                    node: Some((n, sdir.map(|d| d ^ 1))),
                    joins: &empty,
                    bbox: (se, se),
                }
            }
            _ => Goal {
                node: None,
                joins: &joins,
                bbox: (lo, hi),
            },
        };
        if goal.node.is_none() && joins.is_empty() {
            tree.failed.push(ti);
            continue;
        }
        let Some(mut pts) = s.path(e, edir, eacc, &goal, end_of(ti)) else {
            if std::env::var("R3DBG").is_ok() {
                let hs: Vec<_> = s.hits(t.at, e, rules.clearance).iter().map(|h| s.ix.obstacles[h.0 as usize].tag).collect();
                let src_e = match source { Source::Terminal(st) => { let (se, _, _) = escape(st, rules, None); let h2: Vec<_> = s.hits(st.at, se, rules.clearance).iter().map(|h| s.ix.obstacles[h.0 as usize].tag).collect(); format!("src {:?} esc {:?} hits {:?} oth {}", st.at, se, h2, s.others.clear(st.at, se, rules.clearance, None)) }, _ => "lines".into() };
                eprintln!("R3DBG fail t {:?} e {:?} node {:?} hits {:?} oth {} segs {} joins {} exp {} box {:?} zs {:?} lat_z {:?} | {}", t.at, e, s.lat.node(e).is_some(), hs, s.others.clear(t.at, e, rules.clearance, None), segs.len(), joins.len(), s.last_exp, s.last_box, { let mut z = s.last_zs.clone(); z.sort(); z.dedup(); z }, s.lat.line(2), src_e);
            }
            tree.failed.push(ti);
            continue;
        };
        let end = *pts.last().expect("a path has points");
        let mut full = vec![t.at];
        full.append(&mut pts);
        let tee = goal.node.is_none();
        let (parent, join) = match (goal.node, source) {
            (Some(_), Source::Terminal(st)) => {
                full.push(st.at);
                (None, st.at)
            }
            _ => {
                let seg = seg_of.get(&s.lat.node(end).unwrap_or(u32::MAX)).copied();
                let p = segs
                    .iter()
                    .find(|x| {
                        seg.is_some_and(|q| (q.0 - x.0).len() < 1e-9 && (q.1 - x.1).len() < 1e-9)
                    })
                    .and_then(|x| x.2);
                (p, end)
            }
        };
        let full = clean(&full);
        joined.add(join);
        let bi = tree.branches.len();
        for w in full.windows(2) {
            segs.push((w[0], w[1], Some(bi)));
            for &o in &left {
                near[o] = near[o].min(seg_seg_dist(targets[o].at, targets[o].at, w[0], w[1]));
            }
            s.others.push(Routed {
                a: w[0],
                b: w[1],
                clearance: rules.clearance,
            });
        }
        // the joined part stays a straight part of the tree, split at the join
        if let Some(i) = segs.iter().position(|x| {
            seg_seg_dist(join, join, x.0, x.1) < 1e-6
                && (join - x.0).len() > 1e-6
                && (join - x.1).len() > 1e-6
                && x.2 != Some(bi)
        }) {
            let (a, b, owner) = segs.remove(i);
            segs.push((a, join, owner));
            segs.push((join, b, owner));
        }
        tree.branches.push(Branch {
            target: ti,
            points: full,
            parent,
            join,
            tee,
        });
    }
    *others = s.others;
    tree
}

/// Plan length of a polyline from point `k` to its end.
fn plan_len_from(p: &[V3], k: usize) -> f64 {
    p[k..]
        .windows(2)
        .map(|w| v2(w[1].x - w[0].x, w[1].y - w[0].y).len())
        .sum()
}

/// Gravity fall: every vertex on a layer rises by `pct` % of its plan distance along the tree
/// to the source, so flow runs down to it; vertices off the layers (ports, drops) keep their z.
pub fn apply_fall(tree: &mut Tree, layers: &[f64], pct: f64) {
    let on_layer = |z: f64| layers.iter().any(|l| (l - z).abs() < 1e-4);
    // plan distance of each branch's join to the source
    let n = tree.branches.len();
    let mut dj = vec![f64::NAN; n];
    for _ in 0..n {
        for i in 0..n {
            if !dj[i].is_nan() {
                continue;
            }
            let b = &tree.branches[i];
            match b.parent {
                None => dj[i] = 0.0,
                Some(p) if !dj[p].is_nan() => {
                    let pb = &tree.branches[p];
                    let k = pb
                        .points
                        .windows(2)
                        .position(|w| seg_seg_dist(b.join, b.join, w[0], w[1]) < 1e-6)
                        .unwrap_or(0);
                    let tail = v2(pb.points[k + 1].x - b.join.x, pb.points[k + 1].y - b.join.y)
                        .len()
                        + plan_len_from(&pb.points, k + 1);
                    dj[i] = dj[p] + tail;
                }
                _ => {}
            }
        }
    }
    let rise = pct / 100.0;
    let lifted: Vec<Vec<V3>> = tree
        .branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let base = if dj[i].is_nan() { 0.0 } else { dj[i] };
            (0..b.points.len())
                .map(|k| {
                    let p = b.points[k];
                    if on_layer(p.z) {
                        v3(p.x, p.y, p.z + rise * (base + plan_len_from(&b.points, k)))
                    } else {
                        p
                    }
                })
                .collect()
        })
        .collect();
    for (b, pts) in tree.branches.iter_mut().zip(lifted) {
        if on_layer(b.join.z) {
            b.join = *pts.last().unwrap_or(&b.join);
        }
        b.points = pts;
    }
}

#[cfg(test)]
#[path = "route3d_tests.rs"]
mod tests;
