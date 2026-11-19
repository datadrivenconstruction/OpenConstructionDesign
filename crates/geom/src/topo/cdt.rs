// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Constrained Delaunay triangulation: points are inserted into a super-triangle with Lawson flips;
//! constraint edges are inserted by walking along the segment and flipping crossed edges (Sloan);
//! two crossing constraints are split at a Steiner point on the edge. All decisions use exact predicates.

use std::collections::{HashSet, VecDeque};

use super::exact::incircle;
use crate::math::{v2, V2};
use crate::predicates::{orient2d, point_in_ring};

pub(super) const NONE: u32 = u32::MAX;
/// The first three vertices form the super-triangle.
pub(super) const SUPER: u32 = 3;

#[derive(Clone, Copy, Debug)]
struct Tri {
    v: [u32; 3],
    /// `n[i]` is the neighbor across the edge opposite `v[i]`.
    n: [u32; 3],
    live: bool,
}

enum Loc {
    Inside(u32),
    Edge(u32, usize),
    Vertex(u32),
    Outside,
}

fn key(a: u32, b: u32) -> (u32, u32) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Segments p–q and r–s cross at a point interior to both.
fn crosses(p: V2, q: V2, r: V2, s: V2) -> bool {
    let (o1, o2) = (orient2d(p, q, r), orient2d(p, q, s));
    let (o3, o4) = (orient2d(r, s, p), orient2d(r, s, q));
    ((o1 > 0.0 && o2 < 0.0) || (o1 < 0.0 && o2 > 0.0))
        && ((o3 > 0.0 && o4 < 0.0) || (o3 < 0.0 && o4 > 0.0))
}

pub(crate) struct Cdt {
    pub p: Vec<V2>,
    pub z: Vec<f64>,
    tris: Vec<Tri>,
    /// Triangle containing the vertex (NONE if the vertex is not inserted).
    vt: Vec<u32>,
    fixed: HashSet<(u32, u32)>,
    last: u32,
}

/// Result: vertices (x, y), z, triangles, constraint edges, and the original index of each vertex.
pub(crate) struct Out {
    pub p: Vec<V2>,
    pub z: Vec<f64>,
    pub tris: Vec<[u32; 3]>,
    pub fixed: Vec<[u32; 2]>,
    pub old: Vec<u32>,
}

impl Cdt {
    /// Super-triangle around the rectangle [lo, hi].
    pub fn new(lo: V2, hi: V2) -> Cdt {
        let c = v2((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5);
        let r = (hi.x - lo.x).max(hi.y - lo.y).max(1.0);
        let p = vec![
            v2(c.x - 50.0 * r, c.y - 50.0 * r),
            v2(c.x + 50.0 * r, c.y - 50.0 * r),
            v2(c.x, c.y + 50.0 * r),
        ];
        Cdt {
            p,
            z: vec![0.0; 3],
            tris: vec![Tri {
                v: [0, 1, 2],
                n: [NONE; 3],
                live: true,
            }],
            vt: vec![0, 0, 0],
            fixed: HashSet::new(),
            last: 0,
        }
    }

    pub fn push(&mut self, p: V2, z: f64) -> u32 {
        self.p.push(p);
        self.z.push(z);
        self.vt.push(NONE);
        (self.p.len() - 1) as u32
    }

    pub fn inserted(&self, v: u32) -> bool {
        self.vt.get(v as usize).is_some_and(|&t| t != NONE)
    }

    fn put(&mut self, t: u32, v: [u32; 3], n: [u32; 3]) {
        let tri = Tri { v, n, live: true };
        if t as usize == self.tris.len() {
            self.tris.push(tri);
        } else {
            self.tris[t as usize] = tri;
        }
        for x in v {
            self.vt[x as usize] = t;
        }
    }

    fn relink(&mut self, nb: u32, old: u32, new: u32) {
        if nb == NONE {
            return;
        }
        for x in self.tris[nb as usize].n.iter_mut() {
            if *x == old {
                *x = new;
                return;
            }
        }
    }

    fn rotate(&mut self, t: u32, k: usize) {
        if k % 3 != 0 {
            let tr = &mut self.tris[t as usize];
            tr.v.rotate_left(k % 3);
            tr.n.rotate_left(k % 3);
        }
    }

    fn pt(&self, v: u32) -> V2 {
        self.p[v as usize]
    }

    fn classify(t: u32, tr: &Tri, zero: [bool; 3]) -> Loc {
        match zero.iter().filter(|z| **z).count() {
            0 => Loc::Inside(t),
            1 => Loc::Edge(t, zero.iter().position(|z| *z).unwrap_or(0)),
            _ => Loc::Vertex(tr.v[(0..3).find(|&k| !zero[k]).unwrap_or(0)]),
        }
    }

    fn locate(&mut self, q: V2) -> Loc {
        let mut t = self.last;
        if t as usize >= self.tris.len() || !self.tris[t as usize].live {
            match self.tris.iter().position(|x| x.live) {
                Some(i) => t = i as u32,
                None => return Loc::Outside,
            }
        }
        let limit = 2000 + 20 * (self.tris.len() as f64).sqrt() as usize;
        let mut steps = 0usize;
        'walk: loop {
            steps += 1;
            if steps > limit {
                return self.locate_brute(q);
            }
            let tr = self.tris[t as usize];
            let mut zero = [false; 3];
            for k in 0..3 {
                // alternating the first edge tested from step to step keeps the walk from cycling
                let i = (k + steps) % 3;
                let o = orient2d(self.pt(tr.v[(i + 1) % 3]), self.pt(tr.v[(i + 2) % 3]), q);
                if o < 0.0 {
                    let nb = tr.n[i];
                    if nb == NONE {
                        return Loc::Outside;
                    }
                    t = nb;
                    continue 'walk;
                }
                zero[i] = o == 0.0;
            }
            self.last = t;
            return Self::classify(t, &tr, zero);
        }
    }

    fn locate_brute(&mut self, q: V2) -> Loc {
        for (t, tr) in self.tris.iter().enumerate() {
            if !tr.live {
                continue;
            }
            let mut zero = [false; 3];
            let mut inside = true;
            for i in 0..3 {
                let o = orient2d(self.pt(tr.v[(i + 1) % 3]), self.pt(tr.v[(i + 2) % 3]), q);
                inside &= o >= 0.0;
                zero[i] = o == 0.0;
            }
            if inside {
                self.last = t as u32;
                return Self::classify(t as u32, tr, zero);
            }
        }
        Loc::Outside
    }

    /// Inserts vertex `q`; returns the vertex it became (an existing one if coincident).
    pub fn insert(&mut self, q: u32) -> u32 {
        match self.locate(self.pt(q)) {
            Loc::Vertex(v) => v,
            Loc::Inside(t) => {
                self.split3(t, q);
                q
            }
            Loc::Edge(t, i) => {
                self.split_edge(t, i, q);
                q
            }
            Loc::Outside => q,
        }
    }

    fn split3(&mut self, t: u32, q: u32) {
        let Tri {
            v: [a, b, c],
            n: [n0, n1, n2],
            ..
        } = self.tris[t as usize];
        let tb = self.tris.len() as u32;
        let tc = tb + 1;
        self.put(t, [q, b, c], [n0, tb, tc]);
        self.put(tb, [a, q, c], [t, n1, tc]);
        self.put(tc, [a, b, q], [t, tb, n2]);
        self.relink(n1, t, tb);
        self.relink(n2, t, tc);
        self.legalize(vec![(t, 0), (tb, 1), (tc, 2)]);
    }

    /// `q` lies on the edge opposite `t.v[i]` (geometrically, or by construction as a Steiner point).
    fn split_edge(&mut self, t: u32, i: usize, q: u32) {
        self.rotate(t, i);
        let Tri {
            v: [a, b, c],
            n: [u, t1n, t2n],
            ..
        } = self.tris[t as usize];
        let was_fixed = self.fixed.remove(&key(b, c));
        let t2 = self.tris.len() as u32;
        let mut st = vec![(t, 2), (t2, 1)];
        match self
            .tris
            .get(u as usize)
            .and_then(|x| x.n.iter().position(|&y| y == t))
        {
            None => {
                self.put(t, [a, b, q], [NONE, t2, t2n]);
                self.put(t2, [a, q, c], [NONE, t1n, t]);
                self.relink(t1n, t, t2);
            }
            Some(j) => {
                self.rotate(u, j);
                let Tri {
                    v: [d, _, _],
                    n: [_, u1n, u2n],
                    ..
                } = self.tris[u as usize];
                let u2 = t2 + 1;
                self.put(t, [a, b, q], [u2, t2, t2n]);
                self.put(t2, [a, q, c], [u, t1n, t]);
                self.put(u, [d, c, q], [t2, u2, u2n]);
                self.put(u2, [d, q, b], [t, u1n, u]);
                self.relink(t1n, t, t2);
                self.relink(u1n, u, u2);
                st.extend([(u, 2), (u2, 1)]);
            }
        }
        if was_fixed {
            self.fixed.insert(key(b, q));
            self.fixed.insert(key(q, c));
        }
        self.legalize(st);
    }

    /// The edge opposite `t.v[i]` is replaced by the other diagonal; afterwards `t = [w, p1, z]`,
    /// `u = [w, z, p2]`, where w is the former `t.v[i]` and z is the neighbor's opposite vertex.
    fn flip(&mut self, t: u32, i: usize) -> u32 {
        self.rotate(t, i);
        let Tri {
            v: [p0, p1, p2],
            n: [u, nb, na],
            ..
        } = self.tris[t as usize];
        let j = self.tris[u as usize]
            .n
            .iter()
            .position(|&x| x == t)
            .unwrap_or(0);
        self.rotate(u, j);
        let Tri {
            v: [q, _, _],
            n: [_, nd, nc],
            ..
        } = self.tris[u as usize];
        self.put(t, [p0, p1, q], [nd, u, na]);
        self.put(u, [p0, q, p2], [nc, nb, t]);
        self.relink(nd, u, t);
        self.relink(nb, t, u);
        u
    }

    /// The edge opposite `t.v[i]` violates the Delaunay condition (and is not a constraint).
    fn illegal(&self, t: u32, i: usize) -> Option<u32> {
        let tr = self.tris[t as usize];
        let u = tr.n[i];
        if u == NONE || !self.tris[u as usize].live {
            return None;
        }
        if self
            .fixed
            .contains(&key(tr.v[(i + 1) % 3], tr.v[(i + 2) % 3]))
        {
            return None;
        }
        let ut = self.tris[u as usize];
        let j = ut.n.iter().position(|&x| x == t)?;
        let [a, b, c] = tr.v.map(|x| self.pt(x));
        (incircle(a, b, c, self.pt(ut.v[j])) > 0.0).then_some(u)
    }

    /// Stack of (triangle, new-vertex index): the edges opposite that vertex are checked.
    fn legalize(&mut self, mut st: Vec<(u32, usize)>) {
        while let Some((t, i)) = st.pop() {
            if self.illegal(t, i).is_some() {
                let u = self.flip(t, i);
                st.push((t, 0));
                st.push((u, 0));
            }
        }
    }

    /// All triangles incident to the vertex.
    fn fan(&self, u: u32) -> Vec<u32> {
        let s = self.vt[u as usize];
        if s == NONE {
            return vec![];
        }
        let at = |t: u32| self.tris[t as usize].v.iter().position(|&x| x == u);
        let mut out = vec![];
        let mut t = s;
        loop {
            out.push(t);
            let Some(k) = at(t) else { return out };
            let nx = self.tris[t as usize].n[(k + 2) % 3];
            if nx == s {
                return out;
            }
            if nx == NONE || out.len() > 1 << 16 {
                break;
            }
            t = nx;
        }
        let mut t = s;
        loop {
            let Some(k) = at(t) else { break };
            let nx = self.tris[t as usize].n[(k + 1) % 3];
            if nx == NONE || nx == s || out.len() > 1 << 17 {
                break;
            }
            out.push(nx);
            t = nx;
        }
        out
    }

    /// Triangle with directed edge a→b, and the index of its opposite vertex.
    fn find_edge(&self, a: u32, b: u32) -> Option<(u32, usize)> {
        self.fan(a).into_iter().find_map(|t| {
            let v = self.tris[t as usize].v;
            let k = v.iter().position(|&x| x == a)?;
            (v[(k + 1) % 3] == b).then_some((t, (k + 2) % 3))
        })
    }

    fn has_edge(&self, a: u32, b: u32) -> bool {
        self.find_edge(a, b).is_some() || self.find_edge(b, a).is_some()
    }

    /// Constraint edge a–b (both vertices already inserted).
    pub fn constrain(&mut self, a: u32, b: u32) {
        // the segment is split at vertices lying on it and at Steiner points; a stack replaces recursion,
        // and the number of pieces is bounded by the number of vertices
        let mut st = vec![(a, b)];
        let mut guard = 2 * self.p.len() + 64;
        while let Some((a, b)) = st.pop() {
            if guard == 0 {
                break;
            }
            guard -= 1;
            if let Some(c) = self.constrain_one(a, b) {
                st.push((c, b));
                st.push((a, c));
            }
        }
    }

    /// Constraint a–b; if a vertex or another constraint lies in the way, returns the split point.
    fn constrain_one(&mut self, a: u32, b: u32) -> Option<u32> {
        if a == b || !self.inserted(a) || !self.inserted(b) {
            return None;
        }
        if self.has_edge(a, b) {
            self.fixed.insert(key(a, b));
            return None;
        }
        let (pa, pb) = (self.pt(a), self.pt(b));
        let l2 = (pb - pa).dot(pb - pa);
        let ahead = |c: &Self, v: u32| {
            let t = (c.pt(v) - pa).dot(pb - pa);
            t > 0.0 && t < l2
        };
        let mut start = None;
        for t in self.fan(a) {
            let v = self.tris[t as usize].v;
            let Some(k) = v.iter().position(|&x| x == a) else {
                continue;
            };
            let (p, q) = (v[(k + 1) % 3], v[(k + 2) % 3]);
            let (op, oq) = (orient2d(pa, pb, self.pt(p)), orient2d(pa, pb, self.pt(q)));
            if op == 0.0 && ahead(self, p) {
                return Some(p);
            }
            if oq == 0.0 && ahead(self, q) {
                return Some(q);
            }
            if op < 0.0 && oq > 0.0 {
                start = Some((t, p, q));
                break;
            }
        }
        let Some((mut t, mut p, mut q)) = start else {
            return None;
        };
        let mut cross = vec![];
        loop {
            // edge p–q is crossed: p is right of a→b, q is left, t is on the a side
            let tv = self.tris[t as usize].v;
            let Some(i) = tv.iter().position(|&x| x != p && x != q) else {
                return None;
            };
            if self.fixed.contains(&key(p, q)) {
                let x = self.steiner(t, i, pa, pb);
                return (x != a && x != b).then_some(x);
            }
            cross.push((p, q));
            let u = self.tris[t as usize].n[i];
            if u == NONE {
                return None;
            }
            let Some(&r) = self.tris[u as usize].v.iter().find(|&&x| x != p && x != q) else {
                return None;
            };
            if r == b {
                break;
            }
            let o = orient2d(pa, pb, self.pt(r));
            if o == 0.0 {
                return Some(r);
            }
            if o < 0.0 {
                p = r;
            } else {
                q = r;
            }
            t = u;
        }
        self.resolve(pa, pb, a, b, cross);
        if self.has_edge(a, b) {
            self.fixed.insert(key(a, b));
        }
        None
    }

    /// Intersection of segment pa–pb with the constraint opposite `t.v[i]`: a new vertex on it
    /// (z interpolated along the constraint); near an endpoint, the endpoint itself.
    fn steiner(&mut self, t: u32, i: usize, pa: V2, pb: V2) -> u32 {
        let v = self.tris[t as usize].v;
        let (p, q) = (v[(i + 1) % 3], v[(i + 2) % 3]);
        let (pp, pq) = (self.pt(p), self.pt(q));
        let (d, e) = (pb - pa, pq - pp);
        let den = e.cross(d);
        let s = if den != 0.0 {
            ((pa - pp).cross(d) / den).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let len = e.len();
        if s * len < 1e-9 {
            return p;
        }
        if (1.0 - s) * len < 1e-9 {
            return q;
        }
        let z = self.z[p as usize] + (self.z[q as usize] - self.z[p as usize]) * s;
        let x = self.push(pp + e * s, z);
        self.split_edge(t, i, x);
        x
    }

    /// Flips crossed edges until a–b becomes an edge (Sloan, 1993).
    fn resolve(&mut self, pa: V2, pb: V2, a: u32, b: u32, cross: Vec<(u32, u32)>) {
        let limit = 1000 + 200 * cross.len();
        let mut q: VecDeque<(u32, u32)> = cross.into();
        let mut guard = 0usize;
        while let Some((p, r)) = q.pop_front() {
            guard += 1;
            if guard > limit {
                break;
            }
            let Some((t, i)) = self.find_edge(p, r).or_else(|| self.find_edge(r, p)) else {
                continue;
            };
            let tr = self.tris[t as usize];
            let u = tr.n[i];
            if u == NONE {
                continue;
            }
            let ut = self.tris[u as usize];
            let Some(j) = ut.n.iter().position(|&x| x == t) else {
                continue;
            };
            let (w, z) = (tr.v[i], ut.v[j]);
            let (pw, pz) = (self.pt(w), self.pt(z));
            let s0 = orient2d(pw, pz, self.pt(tr.v[(i + 1) % 3]));
            let s1 = orient2d(pw, pz, self.pt(tr.v[(i + 2) % 3]));
            if (s0 > 0.0 && s1 < 0.0) || (s0 < 0.0 && s1 > 0.0) {
                self.flip(t, i);
                if ![a, b].contains(&w) && ![a, b].contains(&z) && crosses(pa, pb, pw, pz) {
                    q.push_back((w, z));
                }
            } else {
                q.push_back((p, r));
            }
        }
    }

    /// Convex hull of the inserted vertices (collinear points excluded), counter-clockwise.
    pub fn hull(&self) -> Vec<u32> {
        let mut v: Vec<u32> = (SUPER..self.p.len() as u32)
            .filter(|&i| self.inserted(i))
            .collect();
        v.sort_by(|&a, &b| {
            let (p, q) = (self.pt(a), self.pt(b));
            p.x.total_cmp(&q.x).then(p.y.total_cmp(&q.y))
        });
        v.dedup_by(|a, b| self.pt(*a) == self.pt(*b));
        if v.len() < 3 {
            return v;
        }
        let mut h: Vec<u32> = vec![];
        for pass in 0..2 {
            let base = h.len();
            let it: Box<dyn Iterator<Item = &u32>> = if pass == 0 {
                Box::new(v.iter())
            } else {
                Box::new(v.iter().rev())
            };
            for &x in it {
                while h.len() >= base + 2
                    && orient2d(self.pt(h[h.len() - 2]), self.pt(h[h.len() - 1]), self.pt(x)) <= 0.0
                {
                    h.pop();
                }
                h.push(x);
            }
            h.pop();
        }
        h
    }

    /// Lawson flips until the Delaunay condition holds on all non-constraint edges.
    fn lawson(&mut self) {
        let mut st: Vec<(u32, usize)> = vec![];
        for (t, tr) in self.tris.iter().enumerate() {
            if !tr.live {
                continue;
            }
            for i in 0..3 {
                if tr.n[i] != NONE && (t as u32) < tr.n[i] {
                    st.push((t as u32, i));
                }
            }
        }
        let limit = 1000 + 64 * st.len();
        let mut guard = 0usize;
        while let Some((t, i)) = st.pop() {
            guard += 1;
            if guard > limit {
                break;
            }
            if !self.tris[t as usize].live || self.illegal(t, i).is_none() {
                continue;
            }
            let u = self.flip(t, i);
            st.extend([(t, 0), (t, 2), (u, 0), (u, 1)]);
        }
    }

    /// Removes triangles incident to super-vertices and (if given) outside `clip`, restores
    /// the Delaunay condition, and compacts the numbering.
    pub fn finish(mut self, clip: Option<&[V2]>) -> Out {
        for t in 0..self.tris.len() {
            let tr = self.tris[t];
            if !tr.live {
                continue;
            }
            let outside = tr.v.iter().any(|&x| x < SUPER)
                || clip.is_some_and(|r| {
                    let [a, b, c] = tr.v.map(|x| self.pt(x));
                    let m = v2((a.x + b.x + c.x) / 3.0, (a.y + b.y + c.y) / 3.0);
                    point_in_ring(m, r) < 0
                });
            if outside {
                self.tris[t].live = false;
            }
        }
        for t in 0..self.tris.len() {
            if !self.tris[t].live {
                continue;
            }
            for k in 0..3 {
                let nb = self.tris[t].n[k];
                if nb != NONE && !self.tris[nb as usize].live {
                    self.tris[t].n[k] = NONE;
                }
            }
        }
        self.vt.iter_mut().for_each(|x| *x = NONE);
        for t in 0..self.tris.len() {
            if self.tris[t].live {
                for x in self.tris[t].v {
                    self.vt[x as usize] = t as u32;
                }
            }
        }
        self.lawson();
        let mut map = vec![NONE; self.p.len()];
        let mut out = Out {
            p: vec![],
            z: vec![],
            tris: vec![],
            fixed: vec![],
            old: vec![],
        };
        for tr in self.tris.iter().filter(|t| t.live) {
            let mut nt = [0u32; 3];
            for (k, &x) in tr.v.iter().enumerate() {
                if map[x as usize] == NONE {
                    map[x as usize] = out.p.len() as u32;
                    out.p.push(self.p[x as usize]);
                    out.z.push(self.z[x as usize]);
                    out.old.push(x);
                }
                nt[k] = map[x as usize];
            }
            out.tris.push(nt);
        }
        let edges: HashSet<(u32, u32)> = out
            .tris
            .iter()
            .flat_map(|t| (0..3).map(move |k| key(t[k], t[(k + 1) % 3])))
            .collect();
        let mut fixed: Vec<[u32; 2]> = self
            .fixed
            .iter()
            .filter_map(|&(a, b)| {
                let (a, b) = (map[a as usize], map[b as usize]);
                (a != NONE && b != NONE && edges.contains(&key(a, b)))
                    .then_some([a.min(b), a.max(b)])
            })
            .collect();
        fixed.sort_unstable();
        out.fixed = fixed;
        out
    }
}

/// Hilbert curve index of a point on a 2^16 × 2^16 grid (insertion order giving short walks).
pub(super) fn hilbert(mut x: u32, mut y: u32) -> u64 {
    const N: u32 = 1 << 16;
    let mut d = 0u64;
    let mut s = N / 2;
    while s > 0 {
        let rx = u32::from(x & s > 0);
        let ry = u32::from(y & s > 0);
        d += u64::from(s) * u64::from(s) * u64::from((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = N - 1 - x;
                y = N - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}
