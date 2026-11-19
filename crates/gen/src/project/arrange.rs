// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A planar arrangement of segments: every crossing and touch becomes a vertex, faces are
//! traced with the region on their left. Faces take labels from a probe point; regions are
//! label unions traced on shared vertices, so neighbouring rooms agree exactly, and walls are
//! the edges between different labels, joined along straight runs.

use std::collections::HashMap;

pub(crate) type P = (f64, f64);

/// Vertices closer than this are one.
const MERGE: f64 = 1e-7;

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn len(a: P) -> f64 {
    a.0.hypot(a.1)
}

fn inside(r: &[P], p: P) -> bool {
    let n = r.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            c = !c;
        }
    }
    c
}

pub(crate) fn area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| cross(r[i], r[(i + 1) % n])).sum::<f64>() / 2.0
}

#[derive(Debug)]
pub struct Arrangement {
    pub v: Vec<P>,
    /// Half-edges: `he[2k]` and `he[2k + 1]` are twins.
    pub he: Vec<(usize, usize)>,
    /// Face of each half-edge (on its left).
    pub face_of: Vec<usize>,
    /// Half-edge loops; bounded faces have positive area.
    pub faces: Vec<Vec<usize>>,
    next: Vec<usize>,
}

struct Grid {
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn id(&mut self, p: P, vs: &mut Vec<P>) -> usize {
        let key = ((p.0 / 1e-6).floor() as i64, (p.1 / 1e-6).floor() as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(c) = self.cells.get(&(key.0 + dx, key.1 + dy)) {
                    if let Some(&k) = c.iter().find(|&&k| len(sub(vs[k], p)) <= MERGE) {
                        return k;
                    }
                }
            }
        }
        vs.push(p);
        self.cells.entry(key).or_default().push(vs.len() - 1);
        vs.len() - 1
    }
}

impl Arrangement {
    pub fn new(segs: &[(P, P)]) -> Arrangement {
        let segs: Vec<(P, P)> = segs.iter().copied().filter(|(a, b)| len(sub(*b, *a)) > 1e-9).collect();
        let n = segs.len();
        let bb: Vec<(P, P)> = segs
            .iter()
            .map(|(a, b)| ((a.0.min(b.0) - 1e-6, a.1.min(b.1) - 1e-6), (a.0.max(b.0) + 1e-6, a.1.max(b.1) + 1e-6)))
            .collect();
        let mut ts: Vec<Vec<f64>> = vec![vec![0.0, 1.0]; n];
        let on = |p: P, a: P, b: P| -> Option<f64> {
            let d = sub(b, a);
            let l2 = dot(d, d);
            let t = dot(sub(p, a), d) / l2;
            let q = (a.0 + d.0 * t, a.1 + d.1 * t);
            (t > 0.0 && t < 1.0 && len(sub(p, q)) <= MERGE).then_some(t)
        };
        for i in 0..n {
            for j in i + 1..n {
                let ((lo1, hi1), (lo2, hi2)) = (bb[i], bb[j]);
                if lo1.0 > hi2.0 || lo2.0 > hi1.0 || lo1.1 > hi2.1 || lo2.1 > hi1.1 {
                    continue;
                }
                let ((a, b), (c, d)) = (segs[i], segs[j]);
                let (r, q) = (sub(b, a), sub(d, c));
                let den = cross(r, q);
                if den.abs() > 1e-12 * len(r) * len(q) {
                    let t = cross(sub(c, a), q) / den;
                    let s = cross(sub(c, a), r) / den;
                    let inr = |x: f64| (-1e-9..=1.0 + 1e-9).contains(&x);
                    if inr(t) && inr(s) {
                        ts[i].push(t.clamp(0.0, 1.0));
                        ts[j].push(s.clamp(0.0, 1.0));
                    }
                }
                // touches and overlaps: an end on the other segment splits it
                for p in [c, d] {
                    if let Some(t) = on(p, a, b) {
                        ts[i].push(t);
                    }
                }
                for p in [a, b] {
                    if let Some(s) = on(p, c, d) {
                        ts[j].push(s);
                    }
                }
            }
        }
        let mut vs: Vec<P> = vec![];
        let mut grid = Grid { cells: HashMap::new() };
        let mut edges: std::collections::BTreeSet<(usize, usize)> = Default::default();
        for ((a, b), t) in segs.iter().zip(ts.iter_mut()) {
            t.sort_by(f64::total_cmp);
            let ids: Vec<usize> = t
                .iter()
                .map(|&t| {
                    let p = if t <= 0.0 {
                        *a
                    } else if t >= 1.0 {
                        *b
                    } else {
                        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
                    };
                    grid.id(p, &mut vs)
                })
                .collect();
            for w in ids.windows(2) {
                if w[0] != w[1] {
                    edges.insert((w[0].min(w[1]), w[0].max(w[1])));
                }
            }
        }
        let mut he = Vec::with_capacity(edges.len() * 2);
        for (u, v) in edges {
            he.push((u, v));
            he.push((v, u));
        }
        let mut out: Vec<Vec<usize>> = vec![vec![]; vs.len()];
        for (k, (u, _)) in he.iter().enumerate() {
            out[*u].push(k);
        }
        let ang = |k: usize| {
            let (u, v) = he[k];
            let d = sub(vs[v], vs[u]);
            d.1.atan2(d.0)
        };
        for o in &mut out {
            o.sort_by(|&x, &y| ang(x).total_cmp(&ang(y)));
        }
        // next of u->v: at v, the outgoing edge just clockwise of v->u
        let mut next = vec![0; he.len()];
        for k in 0..he.len() {
            let tw = k ^ 1;
            let v = he[k].1;
            let o = &out[v];
            let pos = o.iter().position(|&x| x == tw).expect("twin leaves v");
            next[k] = o[(pos + o.len() - 1) % o.len()];
        }
        let mut face_of = vec![usize::MAX; he.len()];
        let mut faces = vec![];
        for s in 0..he.len() {
            if face_of[s] != usize::MAX {
                continue;
            }
            let f = faces.len();
            let mut lp = vec![];
            let mut k = s;
            while face_of[k] == usize::MAX {
                face_of[k] = f;
                lp.push(k);
                k = next[k];
            }
            faces.push(lp);
        }
        Arrangement { v: vs, he, face_of, faces, next }
    }

    pub fn ring(&self, f: usize) -> Vec<P> {
        self.faces[f].iter().map(|&k| self.v[self.he[k].0]).collect()
    }

    pub fn bounded(&self, f: usize) -> bool {
        area(&self.ring(f)) > 1e-12
    }

    /// A point strictly inside bounded face `f`: off the middle of its longest edge, halfway
    /// to the nearest other boundary along the inward normal.
    pub fn probe(&self, f: usize) -> P {
        let lp = &self.faces[f];
        let seg = |k: usize| (self.v[self.he[k].0], self.v[self.he[k].1]);
        let &k = lp
            .iter()
            .max_by(|&&x, &&y| {
                let (a, b) = seg(x);
                let (c, d) = seg(y);
                len(sub(b, a)).total_cmp(&len(sub(d, c)))
            })
            .expect("a face has edges");
        let (a, b) = seg(k);
        let m = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let d = sub(b, a);
        let l = len(d);
        let n = (-d.1 / l, d.0 / l);
        let mut best = 1.0f64;
        for &j in lp {
            if j == k {
                continue;
            }
            let (c, e) = seg(j);
            let q = sub(e, c);
            let den = cross(n, q);
            if den.abs() < 1e-15 {
                continue;
            }
            let t = cross(sub(c, m), q) / den;
            let s = cross(sub(c, m), n) / den;
            if t > 1e-12 && (-1e-12..=1.0 + 1e-12).contains(&s) {
                best = best.min(t);
            }
        }
        (m.0 + n.0 * best / 2.0, m.1 + n.1 * best / 2.0)
    }

    /// Labels every bounded face by its probe point.
    pub fn label<L: Copy + PartialEq>(&self, f: impl Fn(P) -> Option<L>) -> Vec<Option<L>> {
        let n = self.faces.len();
        let mut out: Vec<Option<L>> =
            (0..n).map(|i| if self.bounded(i) { f(self.probe(i)) } else { None }).collect();
        // a loop with no area (the outside of a component) takes the label of the smallest
        // bounded face around it, so an island inside a face is a hole of that face
        let rings: Vec<Option<(Vec<P>, f64)>> = (0..n)
            .map(|i| self.bounded(i).then(|| (self.ring(i), area(&self.ring(i)))))
            .collect();
        for i in 0..n {
            if rings[i].is_some() {
                continue;
            }
            let k = self.faces[i][0];
            let vi = self.he[k].0;
            let p = self.v[vi];
            let mut best: Option<(usize, f64)> = None;
            for (j, r) in rings.iter().enumerate() {
                let Some((ring, a)) = r else { continue };
                if self.faces[j].iter().any(|&h| self.he[h].0 == vi) || !inside(ring, p) {
                    continue;
                }
                if best.map_or(true, |(_, ba)| *a < ba) {
                    best = Some((j, *a));
                }
            }
            if let Some((j, _)) = best {
                out[i] = out[j];
            }
        }
        out
    }

    /// Outlines of the union of the faces labelled `l`: outer rings counter-clockwise first,
    /// then holes clockwise. Collinear points are dropped.
    pub fn region<L: Copy + PartialEq>(&self, labels: &[Option<L>], l: L) -> (Vec<Vec<P>>, Vec<Vec<P>>) {
        let is = |k: usize| labels[self.face_of[k]] == Some(l);
        let border: Vec<bool> = (0..self.he.len()).map(|k| is(k) && !is(k ^ 1)).collect();
        let mut used = vec![false; self.he.len()];
        let (mut outs, mut holes) = (vec![], vec![]);
        for s in 0..self.he.len() {
            if !border[s] || used[s] {
                continue;
            }
            let mut lp = vec![];
            let mut k = s;
            loop {
                used[k] = true;
                lp.push(self.v[self.he[k].0]);
                // walk round the end vertex clockwise until the next border edge of the label
                let mut j = self.next[k];
                let mut guard = 0;
                while !border[j] {
                    j = self.next[j ^ 1];
                    guard += 1;
                    if guard > self.he.len() {
                        break;
                    }
                }
                k = j;
                if k == s || used[k] {
                    break;
                }
            }
            let r = simplify(&lp);
            if r.len() >= 3 {
                if area(&r) > 0.0 {
                    outs.push(r);
                } else {
                    holes.push(r);
                }
            }
        }
        (outs, holes)
    }

    /// The walls: maximal straight runs of edges between two different labels (a `None`
    /// side is outside), each with the role `role` gives for the pair; `None` from `role`
    /// means no wall.
    pub fn walls<L: Copy + PartialEq, R: Copy + PartialEq>(
        &self,
        labels: &[Option<L>],
        role: impl Fn(Option<L>, Option<L>) -> Option<R>,
    ) -> Vec<(P, P, R)> {
        let lab = |k: usize| labels[self.face_of[k]];
        let mut wall_of: Vec<Option<R>> = vec![None; self.he.len() / 2];
        for e in 0..wall_of.len() {
            let (a, b) = (lab(2 * e), lab(2 * e + 1));
            if a != b {
                wall_of[e] = role(a, b);
            }
        }
        // edges at each vertex, to join straight runs
        let mut at: Vec<Vec<usize>> = vec![vec![]; self.v.len()];
        for (e, w) in wall_of.iter().enumerate() {
            if w.is_some() {
                let (u, v) = self.he[2 * e];
                at[u].push(e);
                at[v].push(e);
            }
        }
        let dir = |e: usize, from: usize| {
            let (u, v) = self.he[2 * e];
            let (p, q) = if u == from { (u, v) } else { (v, u) };
            let d = sub(self.v[q], self.v[p]);
            let l = len(d);
            (d.0 / l, d.1 / l)
        };
        // at a vertex, at most one straight pair of the same role joins
        let mut join: HashMap<(usize, usize), usize> = HashMap::new();
        for (vx, es) in at.iter().enumerate() {
            'pair: for (i, &e) in es.iter().enumerate() {
                for &f in &es[i + 1..] {
                    if wall_of[e] != wall_of[f] {
                        continue;
                    }
                    let (d0, d1) = (dir(e, vx), dir(f, vx));
                    if cross(d0, d1).abs() < 1e-9 && dot(d0, d1) < 0.0 {
                        join.insert((vx, e), f);
                        join.insert((vx, f), e);
                        break 'pair;
                    }
                }
            }
        }
        let mut done = vec![false; wall_of.len()];
        let mut out = vec![];
        for e in 0..wall_of.len() {
            let Some(r) = wall_of[e] else { continue };
            if done[e] {
                continue;
            }
            done[e] = true;
            let (u, v) = self.he[2 * e];
            let mut ends = [u, v];
            for (side, start) in [(0usize, u), (1, v)] {
                let (mut cur, mut vx) = (e, start);
                while let Some(&f) = join.get(&(vx, cur)) {
                    if done[f] {
                        break;
                    }
                    done[f] = true;
                    let (a, b) = self.he[2 * f];
                    vx = if a == vx { b } else { a };
                    cur = f;
                }
                ends[side] = vx;
            }
            out.push((self.v[ends[0]], self.v[ends[1]], r));
        }
        out
    }
}

/// Drops points on a straight line between their neighbours.
pub(crate) fn simplify(r: &[P]) -> Vec<P> {
    let mut v = r.to_vec();
    loop {
        let n = v.len();
        if n < 3 {
            return v;
        }
        let k = (0..n).find(|&i| {
            let (a, b, c) = (v[(i + n - 1) % n], v[i], v[(i + 1) % n]);
            let (e0, e1) = (sub(b, a), sub(c, b));
            cross(e0, e1).abs() <= 1e-9 * len(e0) * len(e1) && dot(e0, e1) > 0.0
        });
        match k {
            Some(i) => {
                v.remove(i);
            }
            None => return v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(P, P)> {
        let r = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        (0..4).map(|i| (r[i], r[(i + 1) % 4])).collect()
    }

    #[test]
    fn a_cross_makes_four_faces_and_regions_merge() {
        let mut s = square(0.0, 0.0, 10.0, 10.0);
        s.push(((5.0, 0.0), (5.0, 10.0)));
        s.push(((0.0, 5.0), (10.0, 5.0)));
        let a = Arrangement::new(&s);
        let bounded: Vec<usize> = (0..a.faces.len()).filter(|&f| a.bounded(f)).collect();
        assert_eq!(bounded.len(), 4);
        // left half one label, right half two
        let lab = a.label(|p| Some(if p.0 < 5.0 { 0 } else if p.1 < 5.0 { 1 } else { 2 }));
        let (o, h) = a.region(&lab, 0);
        assert_eq!((o.len(), h.len()), (1, 0));
        assert!((area(&o[0]) - 50.0).abs() < 1e-9 && o[0].len() == 4, "{o:?}");
        let w = a.walls(&lab, |x, y| Some(if x.is_none() || y.is_none() { 'e' } else { 'p' }));
        // outside: 4 walls (sides joined straight); inside: the half-line x = 5 and y = 5 east
        assert_eq!(w.iter().filter(|w| w.2 == 'e').count(), 4, "{w:?}");
        assert_eq!(w.iter().filter(|w| w.2 == 'p').count(), 2, "{w:?}");
    }

    #[test]
    fn dangling_and_t_junctions_and_holes() {
        let mut s = square(0.0, 0.0, 10.0, 10.0);
        s.extend(square(4.0, 4.0, 6.0, 6.0));
        // a stub into the face and a segment ending on another (T)
        s.push(((0.0, 2.0), (3.0, 2.0)));
        s.push(((8.0, 0.0), (8.0, 10.0)));
        let a = Arrangement::new(&s);
        let lab = a.label(|p| {
            let inner = p.0 > 4.0 && p.0 < 6.0 && p.1 > 4.0 && p.1 < 6.0;
            Some(if inner { 1 } else if p.0 > 8.0 { 2 } else { 0 })
        });
        let (o, h) = a.region(&lab, 0);
        assert_eq!((o.len(), h.len()), (1, 1), "{o:?} {h:?}");
        assert!((area(&o[0]) + area(&h[0]) - 76.0).abs() < 1e-9);
        let total: f64 = (0..a.faces.len()).filter(|&f| a.bounded(f)).map(|f| area(&a.ring(f))).sum();
        // a face ring spans its island, so the island is counted once more: 80 + 4 + 20 - 4 (hole)
        assert!((total + area(&h[0]) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn near_coincident_lines_merge() {
        let mut s = square(0.0, 0.0, 10.0, 10.0);
        s.push(((0.0, 5.0), (10.0, 5.0 + 1e-12)));
        s.push(((2.0, 5.0 + 1e-13), (7.0, 5.0)));
        let a = Arrangement::new(&s);
        let n = (0..a.faces.len()).filter(|&f| a.bounded(f)).count();
        assert_eq!(n, 2);
    }
}
