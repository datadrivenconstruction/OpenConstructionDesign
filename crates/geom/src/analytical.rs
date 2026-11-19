// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Analytical model: member axes and panel mid-surfaces → node graph.
//! Member ends snap to members and panel planes: first to more important ones
//! (lower `prio`: columns and walls before beams and bracing), then free ends snap to any target closer than
//! `snap`; panel vertices snap to the planes of other panels. Points closer than `merge` are merged into a node
//! (position taken from the most important one), members are split at nodes lying on them, panels collect the
//! boundary nodes and the support nodes inside. [`check`]: free ends, nearly coincident nodes, short members,
//! unsupported parts.

use std::collections::HashMap;

use crate::math::{v2, V2, V3};

/// Restraint of a member end or support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Release {
    Fixed,
    Pinned,
}

impl Release {
    pub fn name(self) -> &'static str {
        match self {
            Release::Fixed => "fixed",
            Release::Pinned => "pinned",
        }
    }

    pub fn parse(s: &str) -> Option<Release> {
        match s {
            "fixed" => Some(Release::Fixed),
            "pinned" => Some(Release::Pinned),
            _ => None,
        }
    }
}

/// Member axis (world), importance (0 is the highest).
#[derive(Clone, Debug, PartialEq)]
pub struct MemberIn {
    pub a: V3,
    pub b: V3,
    pub prio: u8,
}

/// Planar boundary of a mid-surface (world), importance.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelIn {
    pub ring: Vec<V3>,
    pub prio: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerances {
    /// Points closer than this form one node.
    pub merge: f64,
    /// An end closer than this to a member/panel snaps to it.
    pub snap: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            merge: 0.05,
            snap: 0.6,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<V3>,
    /// Member nodes along a→b: the first and last are its ends.
    pub members: Vec<Vec<usize>>,
    /// Panel boundary nodes in traversal order (vertices and nodes on edges).
    pub panels: Vec<Vec<usize>>,
    /// Nodes inside the panel (members bearing on its plane).
    pub inner: Vec<Vec<usize>>,
    /// A member end was moved by snapping farther than the merge tolerance.
    pub snapped: Vec<[bool; 2]>,
}

fn closest_on_seg(p: V3, a: V3, b: V3) -> (V3, f64) {
    let d = b - a;
    let l2 = d.dot(d);
    if l2 < 1e-24 {
        return (a, 0.0);
    }
    let t = ((p - a).dot(d) / l2).clamp(0.0, 1.0);
    (a + d * t, t)
}

fn crossing_inside(p: V2, ring: &[V2]) -> bool {
    let n = ring.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

fn seg2_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 < 1e-24 {
        0.0
    } else {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    };
    (a + d * t - p).len()
}

/// Panel plane: origin, normal, axes, boundary in plane axes.
#[derive(Clone, Debug)]
struct Plane {
    o: V3,
    n: V3,
    u: V3,
    w: V3,
    poly: Vec<V2>,
    lo: V3,
    hi: V3,
}

/// Point farther than `d` from box [lo, hi] (fast target rejection).
fn far(p: V3, lo: V3, hi: V3, d: f64) -> bool {
    p.x < lo.x - d
        || p.y < lo.y - d
        || p.z < lo.z - d
        || p.x > hi.x + d
        || p.y > hi.y + d
        || p.z > hi.z + d
}

impl Plane {
    fn of(ring: &[V3]) -> Option<Plane> {
        if ring.len() < 3 {
            return None;
        }
        let mut n = V3::default();
        for i in 0..ring.len() {
            let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
            n.x += (p.y - q.y) * (p.z + q.z);
            n.y += (p.z - q.z) * (p.x + q.x);
            n.z += (p.x - q.x) * (p.y + q.y);
        }
        if n.len() < 1e-12 {
            return None;
        }
        let n = n.normalized();
        let o = ring[0];
        let e = ring
            .iter()
            .map(|p| *p - o)
            .find(|d| (*d - n * d.dot(n)).len() > 1e-9)?;
        let u = (e - n * e.dot(n)).normalized();
        let w = n.cross(u);
        let poly = ring
            .iter()
            .map(|p| v2((*p - o).dot(u), (*p - o).dot(w)))
            .collect();
        let (lo, hi) = bbox(ring);
        Some(Plane {
            o,
            n,
            u,
            w,
            poly,
            lo,
            hi,
        })
    }

    fn local(&self, p: V3) -> V2 {
        v2((p - self.o).dot(self.u), (p - self.o).dot(self.w))
    }

    fn edge_dist(&self, q: V2) -> f64 {
        let k = self.poly.len();
        (0..k)
            .map(|i| seg2_dist(q, self.poly[i], self.poly[(i + 1) % k]))
            .fold(f64::MAX, f64::min)
    }

    /// Projection onto the plane if the point is above the polygon (edge inclusive within tolerance `tol`).
    fn project(&self, p: V3, tol: f64) -> Option<(V3, f64)> {
        let q = self.local(p);
        if !crossing_inside(q, &self.poly) && self.edge_dist(q) > tol {
            return None;
        }
        let h = (p - self.o).dot(self.n);
        Some((p - self.n * h, h.abs()))
    }
}

/// What attracts a point: a member or a panel (their indices prevent snapping to itself).
#[derive(Clone, Copy, PartialEq)]
enum Own {
    Member(usize),
    Panel(usize),
}

struct Snapper<'a> {
    ends: &'a [[V3; 2]],
    prio_m: &'a [u8],
    planes: &'a [Option<Plane>],
    prio_p: &'a [u8],
    tol: Tolerances,
    /// Boxes of members and panels, expanded by at least `snap`.
    mg: &'a BoxGrid,
    pg: &'a BoxGrid,
}

impl Snapper<'_> {
    /// Nearest target point within `snap`: (point, distance). `strict` considers only targets
    /// more important than `prio`.
    fn best(&self, p: V3, own: Own, prio: u8, strict: bool) -> Option<(V3, f64)> {
        let ok = |q: u8| !strict || q < prio;
        let mut best: Option<(V3, f64)> = None;
        let mut take = |q: V3, d: f64| {
            if d <= self.tol.snap && best.is_none_or(|b| d < b.1 - 1e-12) {
                best = Some((q, d));
            }
        };
        for j in self.mg.at(p) {
            let e = &self.ends[j];
            if own == Own::Member(j) || !ok(self.prio_m[j]) {
                continue;
            }
            let (lo, hi) = bbox(e);
            if far(p, lo, hi, self.tol.snap) {
                continue;
            }
            let (q, _) = closest_on_seg(p, e[0], e[1]);
            take(q, (q - p).len());
        }
        for j in self.pg.at(p) {
            let Some(pl) = &self.planes[j] else { continue };
            if own == Own::Panel(j) || !ok(self.prio_p[j]) || far(p, pl.lo, pl.hi, self.tol.snap) {
                continue;
            }
            if let Some((q, d)) = pl.project(p, self.tol.merge) {
                take(q, d);
            }
        }
        best
    }

    /// The point already touches another target (within `merge`).
    fn touches(&self, p: V3, own: Own) -> bool {
        self.best(p, own, u8::MAX, false)
            .is_some_and(|(_, d)| d <= self.tol.merge)
    }
}

/// Union-find.
fn root(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

fn unite(p: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (root(p, a), root(p, b));
    if ra != rb {
        p[ra.max(rb)] = ra.min(rb);
    }
}

fn cell(p: V3, s: f64) -> (i64, i64, i64) {
    (
        (p.x / s).floor() as i64,
        (p.y / s).floor() as i64,
        (p.z / s).floor() as i64,
    )
}

/// Groups of points closer than `tol` (transitively, by chains): the group index of each point.
fn clusters(pts: &[V3], tol: f64) -> Vec<usize> {
    let s = tol.max(1e-9);
    let mut grid: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
    for (i, p) in pts.iter().enumerate() {
        grid.entry(cell(*p, s)).or_default().push(i);
    }
    let mut par: Vec<usize> = (0..pts.len()).collect();
    for (i, p) in pts.iter().enumerate() {
        let c = cell(*p, s);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(v) = grid.get(&(c.0 + dx, c.1 + dy, c.2 + dz)) else {
                        continue;
                    };
                    for &j in v {
                        if j > i && (pts[j] - *p).len() <= tol {
                            unite(&mut par, i, j);
                        }
                    }
                }
            }
        }
    }
    (0..pts.len()).map(|i| root(&mut par, i)).collect()
}

/// Node grid for finding nodes on segments and in panels.
struct NodeGrid {
    s: f64,
    map: HashMap<(i64, i64, i64), Vec<usize>>,
}

impl NodeGrid {
    fn new(nodes: &[V3], s: f64) -> NodeGrid {
        let mut map: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
        for (i, p) in nodes.iter().enumerate() {
            map.entry(cell(*p, s)).or_default().push(i);
        }
        NodeGrid { s, map }
    }

    /// Nodes in a box (with margin `pad`).
    fn near(&self, lo: V3, hi: V3, pad: f64) -> Vec<usize> {
        let a = cell(
            lo - V3 {
                x: pad,
                y: pad,
                z: pad,
            },
            self.s,
        );
        let b = cell(
            hi + V3 {
                x: pad,
                y: pad,
                z: pad,
            },
            self.s,
        );
        let cells = ((b.0 - a.0 + 1) * (b.1 - a.1 + 1) * (b.2 - a.2 + 1)) as usize;
        let mut out = vec![];
        if cells > self.map.len() {
            for v in self.map.values() {
                out.extend(v.iter().copied());
            }
        } else {
            for x in a.0..=b.0 {
                for y in a.1..=b.1 {
                    for z in a.2..=b.2 {
                        if let Some(v) = self.map.get(&(x, y, z)) {
                            out.extend(v.iter().copied());
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// More cells than this: the box goes into the shared list (large panel) instead of every cell.
const GRID_CELLS: i64 = 1 << 14;

/// Grid of target boxes: a point scans the targets of its cell rather than all of them (boxes are expanded by
/// `pad`, so a target closer than `pad` to the point is always in its cell).
struct BoxGrid {
    s: f64,
    map: HashMap<(i64, i64, i64), Vec<usize>>,
    big: Vec<usize>,
}

impl BoxGrid {
    fn new(boxes: impl Iterator<Item = Option<(V3, V3)>>, s: f64, pad: f64) -> BoxGrid {
        let mut g = BoxGrid {
            s,
            map: HashMap::new(),
            big: vec![],
        };
        let p = V3 {
            x: pad,
            y: pad,
            z: pad,
        };
        for (i, b) in boxes.enumerate() {
            let Some((lo, hi)) = b else { continue };
            let (a, c) = (cell(lo - p, s), cell(hi + p, s));
            let span = |x0: i64, x1: i64| x1.saturating_sub(x0).saturating_add(1).max(1);
            let n = span(a.0, c.0)
                .saturating_mul(span(a.1, c.1))
                .saturating_mul(span(a.2, c.2));
            if n > GRID_CELLS {
                g.big.push(i);
                continue;
            }
            for x in a.0..=c.0 {
                for y in a.1..=c.1 {
                    for z in a.2..=c.2 {
                        g.map.entry((x, y, z)).or_default().push(i);
                    }
                }
            }
        }
        g
    }

    /// Targets whose expanded boxes cover the point's cell, in ascending index order.
    fn at(&self, p: V3) -> Vec<usize> {
        let mut v = self.big.clone();
        if let Some(c) = self.map.get(&cell(p, self.s)) {
            v.extend_from_slice(c);
        }
        v.sort_unstable();
        v.dedup();
        v
    }
}

fn bbox(pts: &[V3]) -> (V3, V3) {
    let mut lo = V3 {
        x: f64::MAX,
        y: f64::MAX,
        z: f64::MAX,
    };
    let mut hi = V3 {
        x: f64::MIN,
        y: f64::MIN,
        z: f64::MIN,
    };
    for p in pts {
        lo = V3 {
            x: lo.x.min(p.x),
            y: lo.y.min(p.y),
            z: lo.z.min(p.z),
        };
        hi = V3 {
            x: hi.x.max(p.x),
            y: hi.y.max(p.y),
            z: hi.z.max(p.z),
        };
    }
    (lo, hi)
}

/// Analytical model graph.
pub fn build(members: &[MemberIn], panels: &[PanelIn], tol: Tolerances) -> Graph {
    let mut ends: Vec<[V3; 2]> = members.iter().map(|m| [m.a, m.b]).collect();
    let prio_m: Vec<u8> = members.iter().map(|m| m.prio).collect();
    let prio_p: Vec<u8> = panels.iter().map(|p| p.prio).collect();
    let mut rings: Vec<Vec<V3>> = panels.iter().map(|p| p.ring.clone()).collect();
    let planes: Vec<Option<Plane>> = rings.iter().map(|r| Plane::of(r)).collect();
    let gs = (tol.snap * 2.0).max(0.5);
    let boxes = planes.iter().map(|p| p.as_ref().map(|p| (p.lo, p.hi)));
    let pg = BoxGrid::new(boxes, gs, tol.snap);
    // 1: to more important targets (original target positions)
    let orig = ends.clone();
    let mg = BoxGrid::new(orig.iter().map(|e| Some(bbox(e))), gs, tol.snap);
    for i in 0..ends.len() {
        for k in 0..2 {
            let sn = Snapper {
                ends: &orig,
                prio_m: &prio_m,
                planes: &planes,
                prio_p: &prio_p,
                tol,
                mg: &mg,
                pg: &pg,
            };
            let p = ends[i][k];
            if sn.touches(p, Own::Member(i)) {
                continue;
            }
            if let Some((q, _)) = sn.best(p, Own::Member(i), prio_m[i], true) {
                ends[i][k] = q;
            }
        }
    }
    // 2: free ends to any target (positions after step 1); ends move in this
    // step by at most `snap`, so the boxes are expanded by 2·snap + merge
    let mg2 = BoxGrid::new(
        ends.iter().map(|e| Some(bbox(e))),
        gs,
        2.0 * tol.snap + tol.merge,
    );
    for i in 0..ends.len() {
        for k in 0..2 {
            let to = {
                let sn = Snapper {
                    ends: &ends,
                    prio_m: &prio_m,
                    planes: &planes,
                    prio_p: &prio_p,
                    tol,
                    mg: &mg2,
                    pg: &pg,
                };
                let p = ends[i][k];
                let near_end = mg2
                    .at(p)
                    .into_iter()
                    .any(|j| j != i && ends[j].iter().any(|q| (*q - p).len() <= tol.merge));
                if near_end || sn.touches(p, Own::Member(i)) {
                    None
                } else {
                    sn.best(p, Own::Member(i), u8::MAX, false).map(|b| b.0)
                }
            };
            if let Some(q) = to {
                ends[i][k] = q;
            }
        }
    }
    // panel vertices to the planes of other panels
    for (j, r) in rings.iter_mut().enumerate() {
        for p in r.iter_mut() {
            let mut best: Option<(V3, f64)> = None;
            for k in pg.at(*p) {
                let Some(pl) = &planes[k] else { continue };
                if k == j || far(*p, pl.lo, pl.hi, tol.snap) {
                    continue;
                }
                if let Some((q, d)) = pl.project(*p, tol.merge) {
                    if d <= tol.snap && best.is_none_or(|b| d < b.1) {
                        best = Some((q, d));
                    }
                }
            }
            if let Some((q, d)) = best {
                if d > tol.merge {
                    *p = q;
                }
            }
        }
    }
    let snapped: Vec<[bool; 2]> = members
        .iter()
        .zip(&ends)
        .map(|(m, e)| {
            [
                (e[0] - m.a).len() > tol.merge,
                (e[1] - m.b).len() > tol.merge,
            ]
        })
        .collect();
    // merge: member ends, then panel vertices
    let mut pts: Vec<V3> = vec![];
    let mut prio: Vec<u8> = vec![];
    for (i, e) in ends.iter().enumerate() {
        pts.extend_from_slice(e);
        prio.extend([prio_m[i], prio_m[i]]);
    }
    for (j, r) in rings.iter().enumerate() {
        pts.extend_from_slice(r);
        prio.extend(std::iter::repeat_n(prio_p[j], r.len()));
    }
    let cl = clusters(&pts, tol.merge);
    let mut node_of_root: HashMap<usize, usize> = HashMap::new();
    let mut rep: Vec<usize> = vec![];
    let mut node_of = vec![0usize; pts.len()];
    for (i, &c) in cl.iter().enumerate() {
        let n = *node_of_root.entry(c).or_insert_with(|| {
            rep.push(i);
            rep.len() - 1
        });
        // the representative is the most important point of the group, the first one on ties
        if (prio[i], i) < (prio[rep[n]], rep[n]) {
            rep[n] = i;
        }
        node_of[i] = n;
    }
    let nodes: Vec<V3> = rep.iter().map(|&i| pts[i]).collect();
    let grid = NodeGrid::new(&nodes, (tol.snap * 4.0).max(1.0));
    // members are split at nodes lying on them
    let mut out_m = Vec::with_capacity(ends.len());
    for i in 0..ends.len() {
        let (na, nb) = (node_of[2 * i], node_of[2 * i + 1]);
        let (a, b) = (nodes[na], nodes[nb]);
        let mut mid: Vec<(f64, usize)> = vec![];
        let (lo, hi) = bbox(&[a, b]);
        let len = (b - a).len();
        for k in grid.near(lo, hi, tol.merge) {
            if k == na || k == nb {
                continue;
            }
            let (q, t) = closest_on_seg(nodes[k], a, b);
            if (q - nodes[k]).len() <= tol.merge
                && t * len > tol.merge
                && (1.0 - t) * len > tol.merge
            {
                mid.push((t, k));
            }
        }
        mid.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut v = vec![na];
        v.extend(mid.into_iter().map(|m| m.1));
        v.push(nb);
        out_m.push(v);
    }
    // panels: boundary nodes (vertices and on edges) and interior nodes
    let mut out_p = vec![];
    let mut inner = vec![];
    let mut base = 2 * ends.len();
    for (j, r) in rings.iter().enumerate() {
        let ids: Vec<usize> = (0..r.len()).map(|k| node_of[base + k]).collect();
        base += r.len();
        let mut ring_nodes: Vec<usize> = vec![];
        let (lo, hi) = bbox(r);
        let cand = grid.near(lo, hi, tol.merge);
        for k in 0..ids.len() {
            let (na, nb) = (ids[k], ids[(k + 1) % ids.len()]);
            if ring_nodes.last() != Some(&na) {
                ring_nodes.push(na);
            }
            let (a, b) = (nodes[na], nodes[nb]);
            let len = (b - a).len();
            let mut on: Vec<(f64, usize)> = cand
                .iter()
                .copied()
                .filter(|&c| c != na && c != nb)
                .filter_map(|c| {
                    let (q, t) = closest_on_seg(nodes[c], a, b);
                    ((q - nodes[c]).len() <= tol.merge
                        && t * len > tol.merge
                        && (1.0 - t) * len > tol.merge)
                        .then_some((t, c))
                })
                .collect();
            on.sort_by(|x, y| x.0.total_cmp(&y.0));
            for (_, c) in on {
                if !ring_nodes.contains(&c) {
                    ring_nodes.push(c);
                }
            }
        }
        if ring_nodes.len() > 1 && ring_nodes.first() == ring_nodes.last() {
            ring_nodes.pop();
        }
        let mut ins = vec![];
        if let Some(pl) = &planes[j] {
            for &c in &cand {
                if ring_nodes.contains(&c) {
                    continue;
                }
                let p = nodes[c];
                let h = (p - pl.o).dot(pl.n).abs();
                let q = pl.local(p);
                if h <= tol.merge && crossing_inside(q, &pl.poly) && pl.edge_dist(q) > tol.merge {
                    ins.push(c);
                }
            }
        }
        out_p.push(ring_nodes);
        inner.push(ins);
    }
    Graph {
        nodes,
        members: out_m,
        panels: out_p,
        inner,
        snapped,
    }
}

/// Result of the consistency check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// (member, 0 = start | 1 = end) with no connection and no support.
    pub free_ends: Vec<(usize, usize)>,
    /// Pairs of distinct nodes closer than two merge tolerances.
    pub close_nodes: Vec<(usize, usize)>,
    /// Members shorter than the merge tolerance (their ends merged).
    pub short: Vec<usize>,
    /// Connected parts without supports: (members, panels).
    pub unsupported: Vec<(Vec<usize>, Vec<usize>)>,
}

/// Graph check; `supported[node]` means the node has a support.
pub fn check(g: &Graph, supported: &[bool], tol: Tolerances) -> Report {
    let n = g.nodes.len();
    let sup = |k: usize| supported.get(k).copied().unwrap_or(false);
    let mut inc = vec![0usize; n];
    for m in &g.members {
        for (k, &id) in m.iter().enumerate() {
            inc[id] += if k == 0 || k + 1 == m.len() { 1 } else { 2 };
        }
    }
    for (p, ins) in g.panels.iter().zip(&g.inner) {
        for &id in p.iter().chain(ins) {
            inc[id] += 1;
        }
    }
    let mut rep = Report::default();
    for (i, m) in g.members.iter().enumerate() {
        let (a, b) = (m[0], m[m.len() - 1]);
        if a == b || (g.nodes[a] - g.nodes[b]).len() < tol.merge {
            rep.short.push(i);
        }
        for (k, id) in [(0, a), (1, b)] {
            if inc[id] <= 1 && !sup(id) {
                rep.free_ends.push((i, k));
            }
        }
    }
    let close = clusters(&g.nodes, 2.0 * tol.merge);
    let mut by: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, c) in close.iter().enumerate() {
        by.entry(*c).or_default().push(i);
    }
    let mut groups: Vec<Vec<usize>> = by.into_values().filter(|v| v.len() > 1).collect();
    groups.sort();
    for v in groups {
        for w in v.windows(2) {
            rep.close_nodes.push((w[0], w[1]));
        }
    }
    let mut par: Vec<usize> = (0..n).collect();
    for m in &g.members {
        for w in m.windows(2) {
            unite(&mut par, w[0], w[1]);
        }
    }
    for (p, ins) in g.panels.iter().zip(&g.inner) {
        if let Some(&f) = p.first() {
            for &id in p.iter().chain(ins) {
                unite(&mut par, f, id);
            }
        }
    }
    let mut has_sup: HashMap<usize, bool> = HashMap::new();
    for k in 0..n {
        let r = root(&mut par, k);
        *has_sup.entry(r).or_default() |= sup(k);
    }
    let mut comp: Vec<(usize, Vec<usize>, Vec<usize>)> = vec![];
    let mut slot: HashMap<usize, usize> = HashMap::new();
    for (i, m) in g.members.iter().enumerate() {
        let r = root(&mut par, m[0]);
        if has_sup.get(&r).copied().unwrap_or(false) {
            continue;
        }
        let s = *slot.entry(r).or_insert_with(|| {
            comp.push((r, vec![], vec![]));
            comp.len() - 1
        });
        comp[s].1.push(i);
    }
    for (i, p) in g.panels.iter().enumerate() {
        let Some(&f) = p.first() else { continue };
        let r = root(&mut par, f);
        if has_sup.get(&r).copied().unwrap_or(false) {
            continue;
        }
        let s = *slot.entry(r).or_insert_with(|| {
            comp.push((r, vec![], vec![]));
            comp.len() - 1
        });
        comp[s].2.push(i);
    }
    rep.unsupported = comp.into_iter().map(|c| (c.1, c.2)).collect();
    rep
}

/// Section: area and moments of inertia (about the centroid, section y/z axes) from the
/// outer boundary and the void.
pub fn section_props(outer: &[V2], hole: Option<&[V2]>) -> (f64, f64, f64) {
    fn ring(r: &[V2]) -> (f64, f64, f64, f64, f64) {
        // (A, Sx, Sy, Ixx, Iyy) from polygon formulas, sign follows the winding
        let (mut a, mut sx, mut sy, mut ixx, mut iyy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let n = r.len();
        for i in 0..n {
            let (p, q) = (r[i], r[(i + 1) % n]);
            let c = p.x * q.y - q.x * p.y;
            a += c;
            sx += (p.x + q.x) * c;
            sy += (p.y + q.y) * c;
            ixx += (p.y * p.y + p.y * q.y + q.y * q.y) * c;
            iyy += (p.x * p.x + p.x * q.x + q.x * q.x) * c;
        }
        (a / 2.0, sx / 6.0, sy / 6.0, ixx / 12.0, iyy / 12.0)
    }
    let norm = |t: (f64, f64, f64, f64, f64)| {
        let s = t.0.signum();
        (t.0 * s, t.1 * s, t.2 * s, t.3 * s, t.4 * s)
    };
    let o = norm(ring(outer));
    let h = hole.map_or((0.0, 0.0, 0.0, 0.0, 0.0), |h| norm(ring(h)));
    let a = o.0 - h.0;
    if a <= 1e-15 {
        return (0.0, 0.0, 0.0);
    }
    let (cx, cy) = ((o.1 - h.1) / a, (o.2 - h.2) / a);
    // Ixx is about the horizontal axis (y² contribution), Iyy about the vertical one
    let ixx = (o.3 - h.3) - a * cy * cy;
    let iyy = (o.4 - h.4) - a * cx * cx;
    (a, ixx, iyy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v3;

    fn m(a: V3, b: V3, prio: u8) -> MemberIn {
        MemberIn { a, b, prio }
    }

    #[test]
    fn large_frame_grid_shares_nodes_through_the_spatial_index() {
        let n = 30;
        let mut ms = vec![];
        for i in 0..n {
            for j in 0..n {
                let (x, y) = (i as f64 * 6.0, j as f64 * 6.0);
                ms.push(m(v3(x, y, 0.0), v3(x, y, 3.0), 0));
                if i + 1 < n {
                    ms.push(m(v3(x, y, 3.0), v3(x + 6.0, y, 3.0), 1));
                }
                if j + 1 < n {
                    // short by 0.3 m: the free end snaps to the next column axis
                    ms.push(m(v3(x, y, 3.0), v3(x, y + 5.7, 3.0), 1));
                }
            }
        }
        let g = build(&ms, &[], Tolerances::default());
        assert_eq!(g.nodes.len(), 2 * n * n);
        assert!(g.members.iter().all(|m| m.len() == 2));
        let snapped = g.snapped.iter().filter(|s| s[1]).count();
        assert_eq!(snapped, n * (n - 1));
        let one = [Some((v3(0.0, 0.0, 0.0), v3(1.0, 1.0, 1.0)))];
        let grid = BoxGrid::new(one.into_iter(), 0.5, 0.6);
        assert_eq!(grid.at(v3(1.5, 1.5, 1.5)), vec![0]);
        assert!(grid.at(v3(3.0, 0.0, 0.0)).is_empty());
    }

    #[test]
    fn portal_frame_beam_snaps_to_column_axes_and_columns_to_beam() {
        let ms = [
            m(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0), 0),
            m(v3(6.0, 0.0, 0.0), v3(6.0, 0.0, 3.0), 0),
            m(v3(0.2, 0.0, 2.75), v3(5.8, 0.0, 2.75), 1),
        ];
        let tol = Tolerances::default();
        let g = build(&ms, &[], tol);
        assert_eq!(g.nodes.len(), 4, "{:?}", g.nodes);
        let beam = &g.members[2];
        assert_eq!(beam.len(), 2);
        assert!((g.nodes[beam[0]] - v3(0.0, 0.0, 2.75)).len() < 1e-9);
        assert!((g.nodes[beam[1]] - v3(6.0, 0.0, 2.75)).len() < 1e-9);
        // column tops go down to the beam axis and share its nodes
        assert_eq!(g.members[0][1], beam[0]);
        assert_eq!(g.members[1][1], beam[1]);
        assert_eq!(g.snapped[2], [true, true]);
        let mut sup = vec![false; g.nodes.len()];
        sup[g.members[0][0]] = true;
        sup[g.members[1][0]] = true;
        let r = check(&g, &sup, tol);
        assert!(r.free_ends.is_empty(), "{r:?}");
        assert!(r.unsupported.is_empty());
        assert!(r.short.is_empty());
    }

    #[test]
    fn close_ends_merge_into_one_node() {
        let ms = [
            m(v3(0.0, 0.0, 0.0), v3(3.0, 0.0, 0.0), 1),
            m(v3(3.01, 0.0, 0.0), v3(3.0, 4.0, 0.0), 1),
        ];
        let g = build(&ms, &[], Tolerances::default());
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.members[0][1], g.members[1][0]);
    }

    #[test]
    fn t_joint_splits_the_through_member() {
        let ms = [
            m(v3(0.0, 0.0, 0.0), v3(10.0, 0.0, 0.0), 1),
            m(v3(5.0, 0.0, 0.0), v3(5.0, 5.0, 0.0), 1),
        ];
        let g = build(&ms, &[], Tolerances::default());
        assert_eq!(g.members[0].len(), 3, "{:?}", g.members);
        assert_eq!(g.members[0][1], g.members[1][0]);
    }

    #[test]
    fn cantilever_without_support_is_reported() {
        let ms = [m(v3(0.0, 0.0, 3.0), v3(4.0, 0.0, 3.0), 1)];
        let tol = Tolerances::default();
        let g = build(&ms, &[], tol);
        let r = check(&g, &[false, false], tol);
        assert_eq!(r.free_ends, vec![(0, 0), (0, 1)]);
        assert_eq!(r.unsupported, vec![(vec![0], vec![])]);
    }

    #[test]
    fn column_top_lands_inside_slab_panel() {
        let ring = vec![
            v3(0.0, 0.0, 2.9),
            v3(6.0, 0.0, 2.9),
            v3(6.0, 6.0, 2.9),
            v3(0.0, 6.0, 2.9),
        ];
        let ps = [PanelIn { ring, prio: 2 }];
        let ms = [m(v3(3.0, 3.0, 0.0), v3(3.0, 3.0, 3.0), 0)];
        let tol = Tolerances::default();
        let g = build(&ms, &ps, tol);
        let top = g.members[0][1];
        assert!((g.nodes[top] - v3(3.0, 3.0, 2.9)).len() < 1e-9);
        assert_eq!(g.inner[0], vec![top]);
        assert_eq!(g.panels[0].len(), 4);
        let mut sup = vec![false; g.nodes.len()];
        sup[g.members[0][0]] = true;
        let r = check(&g, &sup, tol);
        assert!(r.free_ends.is_empty() && r.unsupported.is_empty(), "{r:?}");
    }

    #[test]
    fn wall_top_snaps_to_slab_mid_plane_and_shares_edge_nodes() {
        let slab = PanelIn {
            ring: vec![
                v3(0.0, 0.0, 2.9),
                v3(5.0, 0.0, 2.9),
                v3(5.0, 5.0, 2.9),
                v3(0.0, 5.0, 2.9),
            ],
            prio: 2,
        };
        let wall = PanelIn {
            ring: vec![
                v3(0.0, 0.0, 0.0),
                v3(5.0, 0.0, 0.0),
                v3(5.0, 0.0, 3.0),
                v3(0.0, 0.0, 3.0),
            ],
            prio: 0,
        };
        let g = build(&[], &[slab, wall], Tolerances::default());
        // wall top corners became slab corners
        assert_eq!(g.nodes.len(), 6, "{:?}", g.nodes);
        let shared = g.panels[1]
            .iter()
            .filter(|n| g.panels[0].contains(n))
            .count();
        assert_eq!(shared, 2);
    }

    #[test]
    fn close_but_unmerged_nodes_are_flagged() {
        let ms = [
            m(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0), 0),
            m(v3(0.07, 0.0, 0.0), v3(0.07, 0.0, 3.0), 0),
        ];
        let tol = Tolerances {
            merge: 0.05,
            snap: 0.01,
        };
        let g = build(&ms, &[], tol);
        assert_eq!(g.nodes.len(), 4);
        let r = check(&g, &[true; 4], tol);
        assert_eq!(r.close_nodes.len(), 2);
    }

    #[test]
    fn rectangle_section_properties() {
        let r = [
            v2(-0.15, -0.25),
            v2(0.15, -0.25),
            v2(0.15, 0.25),
            v2(-0.15, 0.25),
        ];
        let (a, ixx, iyy) = section_props(&r, None);
        assert!((a - 0.15).abs() < 1e-12);
        assert!((ixx - 0.3 * 0.5f64.powi(3) / 12.0).abs() < 1e-12);
        assert!((iyy - 0.5 * 0.3f64.powi(3) / 12.0).abs() < 1e-12);
        // reversed winding and an offset origin give the same numbers
        let mut s: Vec<V2> = r.iter().map(|p| v2(p.x + 3.0, p.y - 1.0)).collect();
        s.reverse();
        let (a2, i2, j2) = section_props(&s, None);
        assert!((a2 - a).abs() < 1e-12 && (i2 - ixx).abs() < 1e-9 && (j2 - iyy).abs() < 1e-9);
    }
}
