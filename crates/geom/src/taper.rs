// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tapered insulation for a flat roof sloping toward drains. Thickness
//! t(p) = t_min + i·d(p), where d is the water path length to a drain inside the outline (going around
//! openings and reentrant corners): d(p) = min over nodes v visible from p (drains and non-convex
//! vertices) of D(v) + ‖p − v‖, with ‖r‖ = max(|r·u|, |r·v|) along the axes `axis`, and D(v) the shortest
//! node-to-drain path (Dijkstra on the visibility graph). Each term is four planes (norm sectors),
//! so the envelope is piecewise planar: the pieces are tapered slabs; the volume is exact (Σ area × t(centroid)).
//! Without drains the thickness is a constant t_min. There is no insulation over openings (shafts, voids).

use std::collections::HashMap;
use std::f64::consts::PI;

use crate::math::{v2, v3, V2};
use crate::mesh::Mesh;
use crate::poly;
use crate::predicates::{on_segment, orient2d, point_in_ring};

#[derive(Clone, Debug, PartialEq)]
pub enum TaperError {
    BadRing,
    /// Slope, thickness or axis is not a number / not positive.
    BadParams,
    /// Drain outside the outline.
    DrainOutside(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Taper {
    pub ring: Vec<V2>,
    /// Slab openings (shafts, voids), any winding: no insulation inside, water goes around them.
    pub holes: Vec<Vec<V2>>,
    pub drains: Vec<V2>,
    /// Slope (rise per meter, 0.02 = 2 %).
    pub slope: f64,
    pub t_min: f64,
    /// Underside of the insulation.
    pub base_z: f64,
    /// Plan direction of the drain faces (length is irrelevant).
    pub axis: V2,
}

/// Planar piece of the envelope: thickness t(p) = c0 + c·p.
#[derive(Clone, Debug, PartialEq)]
pub struct Facet {
    pub pts: Vec<V2>,
    pub drain: usize,
    pub c0: f64,
    pub c: V2,
}

impl Facet {
    pub fn t(&self, p: V2) -> f64 {
        self.c0 + self.c.dot(p)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaperQty {
    pub area: f64,
    pub volume: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub t_avg: f64,
}

/// Half-plane n·p ≤ c; for n ≈ 0 it is the whole plane when c ≥ 0, otherwise empty.
#[derive(Clone, Copy, Debug)]
struct Half {
    n: V2,
    c: f64,
}

fn clip(poly_: &[V2], h: Half) -> Vec<V2> {
    if poly_.is_empty() {
        return vec![];
    }
    if h.n.len() < 1e-12 {
        return if h.c >= 0.0 { poly_.to_vec() } else { vec![] };
    }
    poly::clip_half(poly_, h.n, h.c)
}

fn neg(h: Half) -> Half {
    Half { n: -h.n, c: -h.c }
}

/// Half-planes of a convex polygon, counter-clockwise (interior is to the left of the edges).
fn halves(r: &[V2]) -> Vec<Half> {
    let n = r.len();
    (0..n)
        .filter_map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            let d = b - a;
            (d.len() > 0.0).then(|| {
                let nn = v2(d.y, -d.x);
                Half {
                    n: nn,
                    c: nn.dot(a),
                }
            })
        })
        .collect()
}

/// The piece is non-degenerate: area exceeds `amin`, width (2·area / longest edge) exceeds `wmin`.
fn big(p: &[V2], amin: f64, wmin: f64) -> bool {
    if p.len() < 3 {
        return false;
    }
    let a = poly::area(p).abs();
    let n = p.len();
    let e = (0..n)
        .map(|i| (p[(i + 1) % n] - p[i]).len())
        .fold(0.0, f64::max);
    a > amin && e > 0.0 && 2.0 * a / e > wmin
}

/// For convex `f` and convex ⋂hs: (intersection, remainder as convex pieces); None if the intersection
/// is empty or degenerate.
fn split(f: &[V2], hs: &[Half], amin: f64, wmin: f64, tol: f64) -> Option<(Vec<V2>, Vec<Vec<V2>>)> {
    let cut: Vec<Half> = hs
        .iter()
        .copied()
        .filter(|h| f.iter().any(|p| h.n.dot(*p) > h.c + tol * h.n.len()))
        .collect();
    let mut x = f.to_vec();
    for h in &cut {
        x = clip(&x, *h);
        if x.len() < 3 {
            break;
        }
    }
    if !big(&x, amin, wmin) {
        return None;
    }
    // f \ X = ⋃_l (f ∩ ¬h_l ∩ h_0..h_{l−1})
    let mut rest = vec![];
    for l in 0..cut.len() {
        let mut q = clip(f, neg(cut[l]));
        for h in &cut[..l] {
            if q.len() < 3 {
                break;
            }
            q = clip(&q, *h);
        }
        if big(&q, amin, wmin) {
            rest.push(q);
        }
    }
    Some((x, rest))
}

fn bbox(p: &[V2]) -> [f64; 4] {
    p.iter()
        .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| {
            [b[0].min(q.x), b[1].min(q.y), b[2].max(q.x), b[3].max(q.y)]
        })
}

fn overlap(a: &[f64; 4], b: &[f64; 4], tol: f64) -> bool {
    a[0] <= b[2] + tol && b[0] <= a[2] + tol && a[1] <= b[3] + tol && b[1] <= a[3] + tol
}

/// No duplicate or collinear vertices.
fn clean(p: &[V2], tol: f64) -> Vec<V2> {
    let mut v: Vec<V2> = vec![];
    for &q in p {
        if v.last().is_none_or(|l| (*l - q).len() > tol) {
            v.push(q);
        }
    }
    while v.len() > 1 && (v[0] - v[v.len() - 1]).len() <= tol {
        v.pop();
    }
    let mut i = 0;
    while v.len() > 3 && i < v.len() {
        let n = v.len();
        let (a, b, c) = (v[(i + n - 1) % n], v[i], v[(i + 1) % n]);
        let (e, f) = (b - a, c - b);
        if e.cross(f).abs() <= 1e-10 * e.len() * f.len() && e.dot(f) > 0.0 {
            v.remove(i);
        } else {
            i += 1;
        }
    }
    v
}

/// The counter-clockwise polygon is convex (collinear vertices allowed).
fn convex(p: &[V2], eps: f64) -> bool {
    let n = p.len();
    n >= 3
        && (0..n).all(|i| {
            let (a, b, c) = (p[i], p[(i + 1) % n], p[(i + 2) % n]);
            let (e, f) = (b - a, c - b);
            e.cross(f) >= -eps * e.len() * f.len()
        })
}

/// Union of two convex pieces sharing an edge, if the union is convex.
fn union(a: &[V2], b: &[V2], tol: f64) -> Option<Vec<V2>> {
    let (na, nb) = (a.len(), b.len());
    let near = |p: V2, q: V2| (p - q).len() <= tol;
    for i in 0..na {
        let (p, q) = (a[i], a[(i + 1) % na]);
        for j in 0..nb {
            if near(b[j], q) && near(b[(j + 1) % nb], p) {
                // a from q to p, then b from the point after p to the point before q
                let mut u: Vec<V2> = (0..na).map(|k| a[(i + 1 + k) % na]).collect();
                u.extend((1..nb - 1).map(|k| b[(j + 1 + k) % nb]));
                let u = clean(&u, tol);
                return convex(&u, 1e-9).then_some(u);
            }
        }
    }
    None
}

/// Merges adjacent pieces of the same plane while the union stays convex.
fn merge(mut fs: Vec<Facet>, tol: f64) -> Vec<Facet> {
    let same = |a: &Facet, b: &Facet| (a.c0 - b.c0).abs() < 1e-12 && (a.c - b.c).len() < 1e-12;
    let mut i = 0;
    while i < fs.len() {
        let mut merged = false;
        let mut j = i + 1;
        while j < fs.len() {
            if same(&fs[i], &fs[j]) {
                if let Some(u) = union(&fs[i].pts, &fs[j].pts, tol) {
                    fs[i].pts = u;
                    fs.swap_remove(j);
                    merged = true;
                    continue;
                }
            }
            j += 1;
        }
        if !merged {
            i += 1;
        }
    }
    fs
}

fn opp(x: f64, y: f64) -> bool {
    (x > 0.0 && y < 0.0) || (x < 0.0 && y > 0.0)
}

/// Segments a–b and c–d cross at a point interior to both.
fn proper(a: V2, b: V2, c: V2, d: V2) -> bool {
    opp(orient2d(a, b, c), orient2d(a, b, d)) && opp(orient2d(c, d, a), orient2d(c, d, b))
}

fn ring_ok(r: &[V2]) -> bool {
    r.len() >= 3
        && r.iter().all(|p| p.x.is_finite() && p.y.is_finite())
        && poly::simple_ring(r, 1e-9).is_ok()
        && poly::area(r).abs() >= 1e-9
}

fn crosses(a: &[V2], b: &[V2]) -> bool {
    let (na, nb) = (a.len(), b.len());
    (0..na).any(|i| (0..nb).any(|j| proper(a[i], a[(i + 1) % na], b[j], b[(j + 1) % nb])))
}

fn cheb(w: &[V2; 4], r: V2) -> f64 {
    r.dot(w[0]).abs().max(r.dot(w[1]).abs())
}

/// Norm sector: the direction w_k with the largest projection.
fn sector(w: &[V2; 4], r: V2) -> usize {
    (1..4).fold(0, |b, k| if r.dot(w[k]) > r.dot(w[b]) { k } else { b })
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

/// Region: outer outline counter-clockwise, holes clockwise (the region is to the left of the edges).
struct Domain {
    loops: Vec<Vec<V2>>,
    edges: Vec<(V2, V2)>,
    scale: f64,
}

impl Domain {
    fn new(ring: &[V2], holes: &[Vec<V2>]) -> Self {
        let mut outer = ring.to_vec();
        if poly::area(&outer) < 0.0 {
            outer.reverse();
        }
        let mut loops = vec![outer];
        for h in holes {
            let mut h = h.clone();
            if poly::area(&h) > 0.0 {
                h.reverse();
            }
            loops.push(h);
        }
        let edges = loops
            .iter()
            .flat_map(|l| (0..l.len()).map(move |i| (l[i], l[(i + 1) % l.len()])))
            .collect();
        let b = bbox(&loops[0]);
        let scale = if loops[0].is_empty() {
            1.0
        } else {
            (b[2] - b[0]).hypot(b[3] - b[1]).max(1e-9)
        };
        Self {
            loops,
            edges,
            scale,
        }
    }

    fn contains(&self, p: V2) -> bool {
        poly::contains(&self.loops, p)
    }

    fn area(&self) -> f64 {
        self.loops.iter().map(|l| poly::area(l)).sum()
    }

    /// Segment a–b lies inside the region: it crosses no edges and touches no vertices in its interior.
    /// Touching a reentrant vertex is harmless: that vertex is itself a node with no longer path.
    fn visible(&self, a: V2, b: V2) -> bool {
        if (b - a).len() <= 1e-12 * self.scale {
            return true;
        }
        for &(c, d) in &self.edges {
            if proper(a, b, c, d) {
                return false;
            }
            for q in [c, d] {
                if q != a && q != b && on_segment(a, b, q) {
                    return false;
                }
            }
        }
        self.contains((a + b) * 0.5)
    }

    /// Nearest edge along the ray o + t·d, t > tiny.
    fn first_hit(&self, o: V2, d: V2, tiny: f64) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, &(c, f)) in self.edges.iter().enumerate() {
            let e = f - c;
            let den = d.cross(e);
            if den.abs() < 1e-300 {
                continue;
            }
            let t = (c - o).cross(e) / den;
            let s = (c - o).cross(d) / den;
            if t > tiny && (-1e-12..=1.0 + 1e-12).contains(&s) && best.is_none_or(|b| t < b.1) {
                best = Some((i, t));
            }
        }
        best.map(|b| b.0)
    }

    /// Point of the ray o + t·d on the line of edge `e`.
    fn ray_line(&self, o: V2, d: V2, e: usize) -> Option<V2> {
        let (c, f) = self.edges[e];
        let ed = f - c;
        let den = d.cross(ed);
        if den.abs() < 1e-300 {
            return None;
        }
        let t = (c - o).cross(ed) / den;
        (t.is_finite() && t >= 0.0).then(|| o + d * t)
    }

    /// The part of the region visible from `o`, as convex fans with apex o, each within one norm
    /// sector: (sector, counter-clockwise polygon). Angular sweep over the outline vertices.
    fn fans(&self, o: V2, w: &[V2; 4]) -> Vec<(usize, Vec<V2>)> {
        let tiny = 1e-9 * self.scale;
        let probe = 1e-7 * self.scale;
        let mut ang: Vec<f64> = self
            .loops
            .iter()
            .flatten()
            .filter(|q| (**q - o).len() > tiny)
            .map(|q| (q.y - o.y).atan2(q.x - o.x))
            .collect();
        for k in 0..4 {
            let d = w[k] + w[(k + 1) % 4];
            ang.push(d.y.atan2(d.x));
        }
        ang.sort_by(f64::total_cmp);
        ang.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
        let dir = |a: f64| v2(a.cos(), a.sin());
        let n = ang.len();
        let mut out: Vec<(usize, Vec<V2>)> = vec![];
        let mut cur: Option<(usize, Vec<V2>)> = None;
        for j in 0..n {
            let a0 = ang[j];
            let a1 = if j + 1 < n {
                ang[j + 1]
            } else {
                ang[0] + 2.0 * PI
            };
            let dm = dir(0.5 * (a0 + a1));
            // between adjacent angles the nearest edge is the same
            let wedge = if a1 - a0 > 1e-12 && self.contains(o + dm * probe) {
                self.first_hit(o, dm, tiny).and_then(|e| {
                    Some((self.ray_line(o, dir(a0), e)?, self.ray_line(o, dir(a1), e)?))
                })
            } else {
                None
            };
            let Some((h0, h1)) = wedge else {
                if let Some(f) = cur.take() {
                    out.push(f);
                }
                continue;
            };
            if (h0 - o).cross(h1 - o) <= tiny * tiny {
                continue;
            }
            let k = sector(w, dm);
            let grown = match &cur {
                Some((ck, poly_)) if *ck == k => {
                    let mut c = poly_.clone();
                    for h in [h0, h1] {
                        if c.last().is_none_or(|l| (*l - h).len() > tiny) {
                            c.push(h);
                        }
                    }
                    convex(&c, 1e-9).then_some(c)
                }
                _ => None,
            };
            match grown {
                Some(c) => cur = Some((k, c)),
                None => {
                    if let Some(f) = cur.take() {
                        out.push(f);
                    }
                    cur = Some((k, vec![o, h0, h1]));
                }
            }
        }
        if let Some(f) = cur {
            out.push(f);
        }
        out
    }
}

/// Water-path node: a drain (D = 0) or a non-convex outline vertex; `root` is the path's drain.
#[derive(Clone, Copy, Debug)]
struct Node {
    p: V2,
    d: f64,
    root: usize,
}

/// Envelope piece under construction: plane (c0, c, drain); None means not yet covered.
struct Piece {
    pts: Vec<V2>,
    bb: [f64; 4],
    plane: Option<(f64, V2, usize)>,
}

impl Piece {
    fn new(pts: Vec<V2>, plane: Option<(f64, V2, usize)>) -> Self {
        Self {
            bb: bbox(&pts),
            pts,
            plane,
        }
    }
}

/// Candidate: a node's visible fan within one sector, with its plane.
struct Cand {
    hs: Vec<Half>,
    bb: [f64; 4],
    plane: (f64, V2, usize),
}

/// Vertex hash grid: points closer than `eps` are merged.
struct Pool {
    eps: f64,
    pts: Vec<V2>,
    grid: HashMap<(i64, i64), Vec<u32>>,
}

impl Pool {
    fn cell(&self, p: V2) -> (i64, i64) {
        (
            (p.x / self.eps).floor() as i64,
            (p.y / self.eps).floor() as i64,
        )
    }

    fn id(&mut self, p: V2) -> u32 {
        let (cx, cy) = self.cell(p);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(v) = self.grid.get(&(cx + dx, cy + dy)) {
                    for &i in v {
                        if (self.pts[i as usize] - p).len() <= self.eps {
                            return i;
                        }
                    }
                }
            }
        }
        let i = self.pts.len() as u32;
        self.pts.push(p);
        self.grid.entry((cx, cy)).or_default().push(i);
        i
    }
}

impl Taper {
    fn dirs(&self) -> [V2; 4] {
        let l = self.axis.len();
        let u = self.axis * (1.0 / l);
        let v = v2(-u.y, u.x);
        [u, v, -u, -v]
    }

    fn check(&self) -> Result<(), TaperError> {
        if !ring_ok(&self.ring) || !self.holes.iter().all(|h| ring_ok(h)) {
            return Err(TaperError::BadRing);
        }
        // openings lie inside the outline, with no intersections or nesting
        for (i, h) in self.holes.iter().enumerate() {
            if h.iter().any(|p| point_in_ring(*p, &self.ring) < 0) {
                return Err(TaperError::BadRing);
            }
            for (j, g) in self.holes.iter().enumerate() {
                if i != j && point_in_ring(h[0], g) > 0 {
                    return Err(TaperError::BadRing);
                }
            }
        }
        let loops: Vec<&[V2]> = std::iter::once(self.ring.as_slice())
            .chain(self.holes.iter().map(|h| h.as_slice()))
            .collect();
        for i in 0..loops.len() {
            for j in i + 1..loops.len() {
                if crosses(loops[i], loops[j]) {
                    return Err(TaperError::BadRing);
                }
            }
        }
        if !(self.slope >= 0.0 && self.slope.is_finite())
            || !(self.t_min > 0.0 && self.t_min.is_finite())
            || !self.base_z.is_finite()
            || !(self.axis.len() > 1e-9)
        {
            return Err(TaperError::BadParams);
        }
        for (i, d) in self.drains.iter().enumerate() {
            if point_in_ring(*d, &self.ring) < 0
                || self.holes.iter().any(|h| point_in_ring(*d, h) > 0)
            {
                return Err(TaperError::DrainOutside(i));
            }
        }
        Ok(())
    }

    /// Nodes with their path to a drain: O(N²) Dijkstra on the visibility graph, edges built on the fly.
    fn nodes(&self, dom: &Domain) -> Vec<Node> {
        let w = self.dirs();
        let mut ns: Vec<Node> = self
            .drains
            .iter()
            .enumerate()
            .map(|(i, &p)| Node { p, d: 0.0, root: i })
            .collect();
        let nd = ns.len();
        for l in &dom.loops {
            let n = l.len();
            for i in 0..n {
                let (a, v, b) = (l[(i + n - 1) % n], l[i], l[(i + 1) % n]);
                // reentrant and straight vertices (region on the left): a path may bend around them
                if orient2d(a, v, b) <= 0.0 {
                    ns.push(Node {
                        p: v,
                        d: f64::INFINITY,
                        root: usize::MAX,
                    });
                }
            }
        }
        let mut done = vec![false; ns.len()];
        loop {
            let Some(u) = (0..ns.len())
                .filter(|&i| !done[i] && ns[i].d.is_finite())
                .min_by(|&i, &j| ns[i].d.total_cmp(&ns[j].d))
            else {
                break;
            };
            done[u] = true;
            let nu = ns[u];
            for j in nd..ns.len() {
                if done[j] {
                    continue;
                }
                let c = nu.d + cheb(&w, ns[j].p - nu.p);
                if c < ns[j].d && dom.visible(nu.p, ns[j].p) {
                    ns[j].d = c;
                    ns[j].root = nu.root;
                }
            }
        }
        ns
    }

    /// Best visible node for a point: (node, sector, path).
    fn best(&self, dom: &Domain, ns: &[Node], p: V2) -> Option<(usize, usize, f64)> {
        let w = self.dirs();
        let mut out: Option<(usize, usize, f64)> = None;
        for (i, nd) in ns.iter().enumerate() {
            if !nd.d.is_finite() {
                continue;
            }
            let r = p - nd.p;
            let val = nd.d + cheb(&w, r);
            if out.is_some_and(|o| val >= o.2) || !dom.visible(nd.p, p) {
                continue;
            }
            out = Some((i, sector(&w, r), val));
        }
        out
    }

    /// Plane of a term (node, sector k): t = t_min + i·(D + (p − v)·w_k).
    fn plane(&self, nd: &Node, k: usize, w: &[V2; 4]) -> (f64, V2, usize) {
        (
            self.t_min + self.slope * (nd.d - nd.p.dot(w[k])),
            w[k] * self.slope,
            nd.root,
        )
    }

    /// Thickness at a point (water path through the region; outside the region, a straight line to the drain).
    pub fn t_at(&self, p: V2) -> f64 {
        if self.drains.is_empty() {
            return self.t_min;
        }
        let dom = Domain::new(&self.ring, &self.holes);
        let ns = self.nodes(&dom);
        let d = match self.best(&dom, &ns, p) {
            Some(b) => b.2,
            None => {
                let w = self.dirs();
                self.drains
                    .iter()
                    .map(|q| cheb(&w, p - *q))
                    .fold(f64::INFINITY, f64::min)
            }
        };
        self.t_min + self.slope * d
    }

    /// Planar envelope pieces covering the region (outline minus openings) without overlap.
    /// The envelope is built by inserting candidates one at a time: each cuts only the pieces where it
    /// is below the current plane (a check on the piece's vertices rejects the rest immediately).
    pub fn facets(&self) -> Result<Vec<Facet>, TaperError> {
        self.check()?;
        let dom = Domain::new(&self.ring, &self.holes);
        let total = dom.area().abs().max(1e-300);
        let (amin, wmin, tol) = (1e-13 * total, 1e-10 * dom.scale, 1e-12 * dom.scale);
        let mut base = poly::convex_pieces(&dom.loops[0]);
        for h in &dom.loops[1..] {
            for tri in poly::convex_pieces(h) {
                let hs = halves(&tri);
                base = base
                    .into_iter()
                    .flat_map(|f| match split(&f, &hs, amin, wmin, tol) {
                        Some((_, rest)) => rest,
                        None => vec![f],
                    })
                    .collect();
            }
        }
        if self.drains.is_empty() {
            return Ok(base
                .into_iter()
                .map(|pts| Facet {
                    pts,
                    drain: usize::MAX,
                    c0: self.t_min,
                    c: v2(0.0, 0.0),
                })
                .collect());
        }
        let w = self.dirs();
        let ns = self.nodes(&dom);
        let mut order: Vec<usize> = (0..ns.len()).filter(|&i| ns[i].d.is_finite()).collect();
        order.sort_by(|&a, &b| ns[a].d.total_cmp(&ns[b].d));
        let mut cands = vec![];
        for &i in &order {
            for (k, pts) in dom.fans(ns[i].p, &w) {
                cands.push(Cand {
                    hs: halves(&pts),
                    bb: bbox(&pts),
                    plane: self.plane(&ns[i], k, &w),
                });
            }
        }
        let mut env: Vec<Piece> = base.into_iter().map(|p| Piece::new(p, None)).collect();
        for cd in &cands {
            let (p0, pc, _) = cd.plane;
            let mut next = Vec::with_capacity(env.len() + 8);
            for f in std::mem::take(&mut env) {
                if !overlap(&cd.bb, &f.bb, tol) {
                    next.push(f);
                    continue;
                }
                let mut hs = cd.hs.clone();
                if let Some((q0, qc, _)) = f.plane {
                    // candidate is lower: (c − c_q)·p ≤ c0_q − c0 − margin (on a tie the old plane stays)
                    let n = pc - qc;
                    let c = q0 - p0 - 1e-12;
                    if n.len() < 1e-15 {
                        if c <= 0.0 {
                            next.push(f);
                            continue;
                        }
                    } else {
                        if f.pts.iter().all(|p| n.dot(*p) >= c) {
                            next.push(f);
                            continue;
                        }
                        hs.push(Half { n, c });
                    }
                }
                match split(&f.pts, &hs, amin, wmin, tol) {
                    None => next.push(f),
                    Some((x, rest)) => {
                        next.extend(rest.into_iter().map(|r| Piece::new(r, f.plane)));
                        next.push(Piece::new(x, Some(cd.plane)));
                    }
                }
            }
            env = next;
        }
        let mut out = Vec::with_capacity(env.len());
        for f in env {
            let plane = match f.plane {
                Some(pl) => pl,
                // numerical leftovers on shadow boundaries: take the term at the piece centroid
                None => {
                    let c = poly::centroid(&f.pts);
                    let (i, k) = match self.best(&dom, &ns, c) {
                        Some((i, k, _)) => (i, k),
                        None => {
                            let i = (0..self.drains.len())
                                .min_by(|&a, &b| {
                                    cheb(&w, c - ns[a].p).total_cmp(&cheb(&w, c - ns[b].p))
                                })
                                .unwrap_or(0);
                            (i, sector(&w, c - ns[i].p))
                        }
                    };
                    self.plane(&ns[i], k, &w)
                }
            };
            let pts = clean(&f.pts, tol);
            if pts.len() >= 3 {
                out.push(Facet {
                    pts,
                    drain: plane.2,
                    c0: plane.0,
                    c: plane.1,
                });
            }
        }
        Ok(merge(out, 1e-9 * dom.scale))
    }

    pub fn quantities(&self) -> Result<TaperQty, TaperError> {
        let fs = self.facets()?;
        let mut q = TaperQty {
            t_min: f64::INFINITY,
            ..Default::default()
        };
        for f in &fs {
            let a = poly::area(&f.pts).abs();
            q.area += a;
            q.volume += a * f.t(poly::centroid(&f.pts));
            for p in &f.pts {
                let t = f.t(*p);
                q.t_max = q.t_max.max(t);
                q.t_min = q.t_min.min(t);
            }
        }
        if fs.is_empty() {
            q.t_min = 0.0;
        }
        q.t_avg = if q.area > 0.0 { q.volume / q.area } else { 0.0 };
        Ok(q)
    }

    /// Closed solid with shared vertices: top from the envelope pieces, bottom at base_z, sides along the
    /// outline and openings. Neighbors' vertices on a piece's edges are inserted (no T-junctions).
    /// Faces: top is the piece index, bottom is the piece count, sides follow in outline-edge order.
    pub fn mesh(&self) -> Result<Mesh, TaperError> {
        let fs = self.facets()?;
        let dom = Domain::new(&self.ring, &self.holes);
        let eps = 1e-9 * dom.scale.max(1.0);
        let mut pool = Pool {
            eps,
            pts: vec![],
            grid: HashMap::new(),
        };
        let mut rings: Vec<(usize, Vec<u32>)> = vec![];
        for (k, f) in fs.iter().enumerate() {
            let mut pts = f.pts.clone();
            if poly::area(&pts) < 0.0 {
                pts.reverse();
            }
            let mut ids: Vec<u32> = pts.iter().map(|p| pool.id(*p)).collect();
            ids.dedup();
            while ids.len() > 1 && ids[0] == ids[ids.len() - 1] {
                ids.pop();
            }
            if ids.len() >= 3 {
                rings.push((k, ids));
            }
        }
        let pts = pool.pts.clone();
        for (_, r) in &mut rings {
            let n = r.len();
            let mut out = Vec::with_capacity(n * 2);
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                out.push(a);
                let (pa, pb) = (pts[a as usize], pts[b as usize]);
                let d = pb - pa;
                let l2 = d.dot(d);
                if l2 <= 0.0 {
                    continue;
                }
                let bb = [
                    pa.x.min(pb.x) - eps,
                    pa.y.min(pb.y) - eps,
                    pa.x.max(pb.x) + eps,
                    pa.y.max(pb.y) + eps,
                ];
                let mut on: Vec<(f64, u32)> = pts
                    .iter()
                    .enumerate()
                    .filter_map(|(j, &p)| {
                        let j = j as u32;
                        if j == a
                            || j == b
                            || p.x < bb[0]
                            || p.x > bb[2]
                            || p.y < bb[1]
                            || p.y > bb[3]
                        {
                            return None;
                        }
                        let t = (p - pa).dot(d) / l2;
                        let off = (p - (pa + d * t)).len();
                        (t > 0.0 && t < 1.0 && off <= eps).then_some((t, j))
                    })
                    .collect();
                on.sort_by(|x, y| x.0.total_cmp(&y.0));
                out.extend(on.into_iter().map(|x| x.1));
            }
            out.dedup();
            while out.len() > 1 && out[0] == out[out.len() - 1] {
                out.pop();
            }
            *r = out;
        }
        // pieces narrower than the weld tolerance drop out: their neighbors are stitched by the inserted vertices
        rings.retain(|(_, r)| {
            let p: Vec<V2> = r.iter().map(|&i| pts[i as usize]).collect();
            r.len() >= 3 && big(&p, 0.0, eps)
        });
        let np = pts.len();
        let mut tv = vec![f64::NAN; np];
        for (k, r) in &rings {
            for &i in r {
                if tv[i as usize].is_nan() {
                    tv[i as usize] = fs[*k].t(pts[i as usize]);
                }
            }
        }
        let z = self.base_z;
        let mut m = Mesh::default();
        for (i, p) in pts.iter().enumerate() {
            let t = if tv[i].is_nan() { self.t_min } else { tv[i] };
            m.positions.push(v3(p.x, p.y, z + t));
        }
        for p in &pts {
            m.positions.push(v3(p.x, p.y, z));
        }
        let nf = fs.len() as u32;
        let bot = np as u32;
        for (k, r) in &rings {
            let f = &fs[*k];
            let poly_: Vec<V2> = r.iter().map(|&i| pts[i as usize]).collect();
            let c = poly::centroid(&poly_);
            let ct = m.positions.len() as u32;
            m.positions.push(v3(c.x, c.y, z + f.t(c)));
            m.positions.push(v3(c.x, c.y, z));
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                m.triangles.push([ct, a, b]);
                m.face_of.push(*k as u32);
                m.triangles.push([ct + 1, bot + b, bot + a]);
                m.face_of.push(nf);
            }
        }
        let mut cnt: HashMap<(u32, u32), u32> = HashMap::new();
        for (_, r) in &rings {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                *cnt.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        let tol_e = 1e-7 * dom.scale;
        for (_, r) in &rings {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                if cnt.get(&(a.min(b), a.max(b))) != Some(&1) {
                    continue;
                }
                let mid = (pts[a as usize] + pts[b as usize]) * 0.5;
                let e = dom
                    .edges
                    .iter()
                    .position(|&(c, d)| seg_dist(mid, c, d) <= tol_e)
                    .unwrap_or(dom.edges.len());
                let face = nf + 1 + e as u32;
                m.triangles.push([bot + a, bot + b, b]);
                m.triangles.push([bot + a, b, a]);
                m.face_of.push(face);
                m.face_of.push(face);
            }
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol * b.abs().max(1.0), "{a} vs {b}");
    }

    fn square(a: f64) -> Vec<V2> {
        vec![v2(-a, -a), v2(a, -a), v2(a, a), v2(-a, a)]
    }

    /// Every undirected edge belongs to exactly two triangles.
    fn edges_twice(m: &Mesh) -> bool {
        let mut c: HashMap<(u32, u32), u32> = HashMap::new();
        for t in &m.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *c.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        c.values().all(|&v| v == 2)
    }

    #[test]
    fn single_drain_pyramid_volume() {
        // ∫∫ max(|x|,|y|) over [−a,a]² = 8a³/3
        let (a, s, t0) = (5.0, 0.02, 0.05);
        let tp = Taper {
            ring: square(a),
            holes: vec![],
            drains: vec![v2(0.0, 0.0)],
            slope: s,
            t_min: t0,
            base_z: 10.0,
            axis: v2(1.0, 0.0),
        };
        let q = tp.quantities().unwrap();
        near(q.area, 4.0 * a * a, 1e-12);
        near(q.volume, t0 * 4.0 * a * a + s * 8.0 * a * a * a / 3.0, 1e-9);
        near(q.t_max, t0 + s * a, 1e-12);
        near(q.t_min, t0, 1e-12);
        let m = tp.mesh().unwrap();
        near(m.volume(), q.volume, 1e-9);
        assert!(edges_twice(&m));
        assert!(m.is_watertight());
    }

    #[test]
    fn two_drains_split_by_valley_ridge() {
        let (a, s, t0) = (3.0, 0.025, 0.04);
        let ring = vec![
            v2(-2.0 * a, -a),
            v2(2.0 * a, -a),
            v2(2.0 * a, a),
            v2(-2.0 * a, a),
        ];
        let tp = Taper {
            ring,
            holes: vec![],
            drains: vec![v2(-a, 0.0), v2(a, 0.0)],
            slope: s,
            t_min: t0,
            base_z: 0.0,
            axis: v2(1.0, 0.0),
        };
        let q = tp.quantities().unwrap();
        let one = t0 * 4.0 * a * a + s * 8.0 * a * a * a / 3.0;
        near(q.area, 8.0 * a * a, 1e-9);
        near(q.volume, 2.0 * one, 1e-9);
        // ridge between drains at x = 0: thickness t0 + s·a
        near(tp.t_at(v2(0.0, 0.0)), t0 + s * a, 1e-12);
        let m = tp.mesh().unwrap();
        near(m.volume(), q.volume, 1e-9);
        assert!(edges_twice(&m));
    }

    #[test]
    fn aligned_drains_tie_without_double_count() {
        // drains on one line along v: the ±u sector planes coincide, so nothing is counted twice
        let tp = Taper {
            ring: vec![v2(-4.0, -2.0), v2(4.0, -2.0), v2(4.0, 6.0), v2(-4.0, 6.0)],
            holes: vec![],
            drains: vec![v2(0.0, 0.0), v2(0.0, 4.0)],
            slope: 0.02,
            t_min: 0.05,
            base_z: 0.0,
            axis: v2(1.0, 0.0),
        };
        let q = tp.quantities().unwrap();
        near(q.area, 64.0, 1e-9);
        // numerical volume check on a midpoint grid
        let n = 400;
        let (mut v, cell) = (0.0, 8.0 / n as f64);
        for i in 0..n {
            for j in 0..n {
                let p = v2(
                    -4.0 + (i as f64 + 0.5) * cell,
                    -2.0 + (j as f64 + 0.5) * cell,
                );
                v += tp.t_at(p) * cell * cell;
            }
        }
        near(q.volume, v, 1e-5);
    }

    #[test]
    fn rotated_axis_and_l_shape_cover_ring() {
        let ring = vec![
            v2(0.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 4.0),
            v2(4.0, 4.0),
            v2(4.0, 10.0),
            v2(0.0, 10.0),
        ];
        let tp = Taper {
            ring: ring.clone(),
            holes: vec![],
            drains: vec![v2(2.0, 2.0), v2(8.0, 2.0), v2(2.0, 8.0)],
            slope: 0.02,
            t_min: 0.06,
            base_z: 0.0,
            axis: v2(1.0, 1.0),
        };
        let q = tp.quantities().unwrap();
        near(q.area, poly::area(&ring), 1e-9);
        for f in tp.facets().unwrap() {
            let c = poly::centroid(&f.pts);
            near(f.t(c), tp.t_at(c), 1e-9);
        }
        assert!(edges_twice(&tp.mesh().unwrap()));
    }

    #[test]
    fn l_shape_path_goes_around_reflex_corner() {
        // drain at the end of the lower wing; the corner of the upper wing cannot see it: the path goes via (4, 4)
        let ring = vec![
            v2(0.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 4.0),
            v2(4.0, 4.0),
            v2(4.0, 10.0),
            v2(0.0, 10.0),
        ];
        let (s, t0) = (0.02, 0.05);
        let tp = Taper {
            ring: ring.clone(),
            holes: vec![],
            drains: vec![v2(9.0, 2.0)],
            slope: s,
            t_min: t0,
            base_z: 0.0,
            axis: v2(1.0, 0.0),
        };
        let p = v2(0.5, 9.5);
        // a straight line would give max(8.5, 7.5) = 8.5; going around: max(5, 2) + max(3.5, 5.5) = 10.5
        near(tp.t_at(p), t0 + s * 10.5, 1e-12);
        let fs = tp.facets().unwrap();
        let f = fs
            .iter()
            .find(|f| point_in_ring(p, &f.pts) >= 0)
            .expect("point covered");
        near(f.t(p), t0 + s * 10.5, 1e-9);
        // a point visible from the drain: straight line
        near(tp.t_at(v2(3.0, 1.0)), t0 + s * 6.0, 1e-12);
        let q = tp.quantities().unwrap();
        near(q.area, poly::area(&ring), 1e-9);
        near(q.t_max, t0 + s * (5.0 + 6.0), 1e-9);
        for f in &fs {
            let c = poly::centroid(&f.pts);
            near(f.t(c), tp.t_at(c), 1e-9);
        }
        let m = tp.mesh().unwrap();
        assert!(edges_twice(&m));
        near(m.volume(), q.volume, 1e-9);
    }

    #[test]
    fn holes_carry_no_insulation_and_mesh_is_closed() {
        // 2 × 2 shaft to the right of the drain; clockwise or counter-clockwise winding makes no difference
        let hole = vec![v2(2.0, -1.0), v2(2.0, 1.0), v2(4.0, 1.0), v2(4.0, -1.0)];
        let tp = Taper {
            ring: square(5.0),
            holes: vec![hole.clone()],
            drains: vec![v2(0.0, 0.0)],
            slope: 0.02,
            t_min: 0.05,
            base_z: 3.0,
            axis: v2(1.0, 0.0),
        };
        let q = tp.quantities().unwrap();
        near(q.area, 100.0 - 4.0, 1e-9);
        let fs = tp.facets().unwrap();
        for f in &fs {
            let c = poly::centroid(&f.pts);
            assert!(point_in_ring(c, &hole) < 0, "facet inside hole");
            near(f.t(c), tp.t_at(c), 1e-9);
        }
        // behind the shaft water goes around its corner: (4.5, 0) via (4, 1) gives 1 + 4 = 5, not 4.5
        near(tp.t_at(v2(4.5, 0.0)), 0.05 + 0.02 * 5.0, 1e-12);
        let n = 200;
        let (mut v, cell) = (0.0, 10.0 / n as f64);
        for i in 0..n {
            for j in 0..n {
                let p = v2(
                    -5.0 + (i as f64 + 0.5) * cell,
                    -5.0 + (j as f64 + 0.5) * cell,
                );
                if point_in_ring(p, &hole) < 0 {
                    v += tp.t_at(p) * cell * cell;
                }
            }
        }
        near(q.volume, v, 1e-3);
        let m = tp.mesh().unwrap();
        assert!(edges_twice(&m));
        assert!(m.is_watertight());
        near(m.volume(), q.volume, 1e-9);
        let rev = Taper {
            holes: vec![hole.iter().rev().copied().collect()],
            ..tp.clone()
        };
        near(rev.quantities().unwrap().volume, q.volume, 1e-9);
        let inside = Taper {
            drains: vec![v2(3.0, 0.0)],
            ..tp.clone()
        };
        assert_eq!(inside.facets().unwrap_err(), TaperError::DrainOutside(0));
        let crossing = Taper {
            holes: vec![vec![
                v2(4.0, -1.0),
                v2(6.0, -1.0),
                v2(6.0, 1.0),
                v2(4.0, 1.0),
            ]],
            ..tp.clone()
        };
        assert_eq!(crossing.facets().unwrap_err(), TaperError::BadRing);
    }

    #[test]
    fn twenty_drains_stay_bounded() {
        let ring = vec![v2(0.0, 0.0), v2(40.0, 0.0), v2(40.0, 30.0), v2(0.0, 30.0)];
        let mut drains = vec![];
        for i in 0..5 {
            for j in 0..4 {
                // slightly shifted grid: no coincident planes
                let dx = 0.3 * ((i * 7 + j * 3) % 5) as f64;
                drains.push(v2(
                    4.0 + 8.0 * i as f64 + dx,
                    3.75 + 7.5 * j as f64 - 0.2 * dx,
                ));
            }
        }
        let tp = Taper {
            ring: ring.clone(),
            holes: vec![],
            drains,
            slope: 0.02,
            t_min: 0.05,
            base_z: 0.0,
            axis: v2(1.0, 0.0),
        };
        let fs = tp.facets().unwrap();
        assert!(fs.len() <= 20 * 4 * 8, "{} facets", fs.len());
        let area: f64 = fs.iter().map(|f| poly::area(&f.pts).abs()).sum();
        near(area, 1200.0, 1e-6);
        for f in fs.iter().step_by(7) {
            let c = poly::centroid(&f.pts);
            near(f.t(c), tp.t_at(c), 1e-9);
        }
        assert!(edges_twice(&tp.mesh().unwrap()));
    }

    #[test]
    fn no_drains_is_flat_and_errors_are_typed() {
        let tp = Taper {
            ring: square(2.0),
            holes: vec![],
            drains: vec![],
            slope: 0.02,
            t_min: 0.1,
            base_z: 0.0,
            axis: v2(1.0, 0.0),
        };
        let q = tp.quantities().unwrap();
        near(q.volume, 16.0 * 0.1, 1e-12);
        assert!(edges_twice(&tp.mesh().unwrap()));
        let out = Taper {
            drains: vec![v2(9.0, 0.0)],
            ..tp.clone()
        };
        assert_eq!(out.facets().unwrap_err(), TaperError::DrainOutside(0));
        let bad = Taper {
            t_min: 0.0,
            ..tp.clone()
        };
        assert_eq!(bad.facets().unwrap_err(), TaperError::BadParams);
    }
}
