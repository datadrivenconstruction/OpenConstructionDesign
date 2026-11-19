// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Mesh simplification for levels of detail: quadric edge collapse (sum of squared distances
//! to the planes of the original faces, unweighted, so `sqrt(cost)` bounds the distance of
//! a new vertex to every original plane it absorbed). Coincident vertices are welded first;
//! open borders are kept by perpendicular border planes; a collapse is refused when it would
//! make the surface non-manifold (link condition), pinch two borders or flip a face.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::math::{v3, V3};
use crate::mesh::Mesh;

/// Result of [`simplify`]: the mesh and the largest `sqrt(cost)` of an applied collapse.
#[derive(Clone, Debug)]
pub struct Simplified {
    pub mesh: Mesh,
    pub error: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    /// Plane `n·p + d = 0` with unit `n`.
    fn plane(n: V3, d: f64) -> Self {
        Quadric([
            n.x * n.x,
            n.x * n.y,
            n.x * n.z,
            n.x * d,
            n.y * n.y,
            n.y * n.z,
            n.y * d,
            n.z * n.z,
            n.z * d,
            d * d,
        ])
    }

    fn add(&mut self, o: &Quadric) {
        for k in 0..10 {
            self.0[k] += o.0[k];
        }
    }

    fn eval(&self, p: V3) -> f64 {
        let q = &self.0;
        let (x, y, z) = (p.x, p.y, p.z);
        let v = q[0] * x * x
            + 2.0 * q[1] * x * y
            + 2.0 * q[2] * x * z
            + 2.0 * q[3] * x
            + q[4] * y * y
            + 2.0 * q[5] * y * z
            + 2.0 * q[6] * y
            + q[7] * z * z
            + 2.0 * q[8] * z
            + q[9];
        v.max(0.0)
    }

    /// Minimiser of the quadric, None when the 3×3 system is near singular.
    fn optimum(&self, scale: f64) -> Option<V3> {
        let q = &self.0;
        let (a, b, c, e, f, i) = (q[0], q[1], q[2], q[4], q[5], q[7]);
        let det = a * (e * i - f * f) - b * (b * i - f * c) + c * (b * f - e * c);
        if det.abs() <= 1e-12 * scale.max(1e-30) {
            return None;
        }
        let (r0, r1, r2) = (-q[3], -q[6], -q[8]);
        let x = (r0 * (e * i - f * f) - b * (r1 * i - f * r2) + c * (r1 * f - e * r2)) / det;
        let y = (a * (r1 * i - f * r2) - r0 * (b * i - f * c) + c * (b * r2 - r1 * c)) / det;
        let z = (a * (e * r2 - r1 * f) - b * (b * r2 - r1 * c) + r0 * (b * f - e * c)) / det;
        let p = v3(x, y, z);
        (p.x.is_finite() && p.y.is_finite() && p.z.is_finite()).then_some(p)
    }
}

#[derive(Clone, Copy, Debug)]
struct Cand {
    cost: f64,
    u: u32,
    v: u32,
    vu: u32,
    vv: u32,
    p: V3,
}

impl PartialEq for Cand {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Cand {
    /// Reversed: the heap pops the cheapest collapse; ties by vertex ids (deterministic).
    fn cmp(&self, o: &Self) -> Ordering {
        o.cost
            .total_cmp(&self.cost)
            .then_with(|| o.u.cmp(&self.u))
            .then_with(|| o.v.cmp(&self.v))
    }
}

/// Merges vertices closer than `eps` (grid cells); drops triangles that become degenerate.
/// Returns positions, triangles and the source triangle of each kept triangle.
pub fn weld(m: &Mesh, eps: f64) -> (Vec<V3>, Vec<[u32; 3]>, Vec<u32>) {
    let eps = eps.max(1e-12);
    let mut ix: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(m.positions.len());
    let mut pos = Vec::with_capacity(m.positions.len());
    let remap: Vec<u32> = m
        .positions
        .iter()
        .map(|p| {
            let k = (
                (p.x / eps).round() as i64,
                (p.y / eps).round() as i64,
                (p.z / eps).round() as i64,
            );
            *ix.entry(k).or_insert_with(|| {
                pos.push(*p);
                (pos.len() - 1) as u32
            })
        })
        .collect();
    let mut tris = Vec::with_capacity(m.triangles.len());
    let mut src = Vec::with_capacity(m.triangles.len());
    for (k, t) in m.triangles.iter().enumerate() {
        if t.iter().any(|i| *i as usize >= remap.len()) {
            continue;
        }
        let t = t.map(|i| remap[i as usize]);
        if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
            tris.push(t);
            src.push(k as u32);
        }
    }
    (pos, tris, src)
}

struct State {
    pos: Vec<V3>,
    q: Vec<Quadric>,
    ver: Vec<u32>,
    dead: Vec<bool>,
    locked: Vec<bool>,
    tris: Vec<[u32; 3]>,
    alive: Vec<bool>,
    vt: Vec<Vec<u32>>,
    scale: f64,
}

fn tri_normal(a: V3, b: V3, c: V3) -> V3 {
    (b - a).cross(c - a)
}

fn edge_key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

impl State {
    /// Distinct neighbours of `u` and, per neighbour, triangles of `u` that contain it.
    fn ring(&self, u: u32) -> Vec<(u32, u32)> {
        let mut n: Vec<(u32, u32)> = vec![];
        for &t in &self.vt[u as usize] {
            for w in self.tris[t as usize] {
                if w == u {
                    continue;
                }
                match n.iter_mut().find(|e| e.0 == w) {
                    Some(e) => e.1 += 1,
                    None => n.push((w, 1)),
                }
            }
        }
        n
    }

    fn boundary(&self, u: u32) -> bool {
        self.ring(u).iter().any(|e| e.1 == 1)
    }

    fn cand(&self, u: u32, v: u32) -> Option<Cand> {
        if self.locked[u as usize] || self.locked[v as usize] {
            return None;
        }
        let mut q = self.q[u as usize];
        q.add(&self.q[v as usize]);
        let (pu, pv) = (self.pos[u as usize], self.pos[v as usize]);
        let mid = (pu + pv) * 0.5;
        let mut best = (q.eval(pu), pu);
        for p in [pv, mid] {
            let c = q.eval(p);
            if c < best.0 {
                best = (c, p);
            }
        }
        if let Some(p) = q.optimum(self.scale) {
            // only near the edge: a far optimum means an ill-conditioned quadric
            let r = (pv - pu).len();
            if (p - mid).len() <= r {
                let c = q.eval(p);
                if c < best.0 {
                    best = (c, p);
                }
            }
        }
        if !best.0.is_finite() {
            return None;
        }
        Some(Cand {
            cost: best.0,
            u,
            v,
            vu: self.ver[u as usize],
            vv: self.ver[v as usize],
            p: best.1,
        })
    }

    /// Link condition, border pinch and face flips for collapsing `v` into `u` at `p`.
    fn valid(&self, u: u32, v: u32, p: V3) -> bool {
        let (ru, rv) = (self.ring(u), self.ring(v));
        let shared: Vec<u32> = self.vt[u as usize]
            .iter()
            .copied()
            .filter(|t| self.tris[*t as usize].contains(&v))
            .collect();
        if shared.is_empty() || shared.len() > 2 {
            return false;
        }
        let opposite: Vec<u32> = shared
            .iter()
            .map(|t| {
                *self.tris[*t as usize]
                    .iter()
                    .find(|w| **w != u && **w != v)
                    .unwrap_or(&u)
            })
            .collect();
        let common = ru
            .iter()
            .filter(|a| a.0 != v && rv.iter().any(|b| b.0 == a.0))
            .count();
        if common != opposite.len() || !opposite.iter().all(|w| ru.iter().any(|a| a.0 == *w)) {
            return false;
        }
        let border_edge = shared.len() == 1;
        if !border_edge && ru.iter().any(|e| e.1 == 1) && rv.iter().any(|e| e.1 == 1) {
            return false;
        }
        // a lone triangle or a closed tetrahedron would vanish entirely
        if self.vt[u as usize].len() + self.vt[v as usize].len() <= shared.len() + 1 {
            return false;
        }
        for (x, other) in [(u, v), (v, u)] {
            for &t in &self.vt[x as usize] {
                let tri = self.tris[t as usize];
                if tri.contains(&other) {
                    continue;
                }
                let old = tri.map(|i| self.pos[i as usize]);
                let new = tri.map(|i| if i == x { p } else { self.pos[i as usize] });
                let n0 = tri_normal(old[0], old[1], old[2]);
                let n1 = tri_normal(new[0], new[1], new[2]);
                let (l0, l1) = (n0.len(), n1.len());
                if l1 <= 1e-12 * self.scale * self.scale {
                    return false;
                }
                if l0 > 0.0 && n0.dot(n1) < 0.2 * l0 * l1 {
                    return false;
                }
            }
        }
        true
    }

    fn collapse(&mut self, u: u32, v: u32, p: V3) -> usize {
        let mut removed = 0;
        let vts = std::mem::take(&mut self.vt[v as usize]);
        for &t in &vts {
            let tri = self.tris[t as usize];
            if tri.contains(&u) {
                self.alive[t as usize] = false;
                removed += 1;
                for w in tri {
                    if w != v {
                        self.vt[w as usize].retain(|x| *x != t);
                    }
                }
            } else {
                let tr = &mut self.tris[t as usize];
                for w in tr.iter_mut() {
                    if *w == v {
                        *w = u;
                    }
                }
                self.vt[u as usize].push(t);
            }
        }
        let qv = self.q[v as usize];
        self.q[u as usize].add(&qv);
        self.pos[u as usize] = p;
        self.dead[v as usize] = true;
        self.ver[u as usize] += 1;
        self.ver[v as usize] += 1;
        removed
    }
}

/// Collapses edges, cheapest first, until at most `target` triangles remain or the next
/// collapse would exceed `max_error` (a distance, same units as the mesh).
pub fn simplify(m: &Mesh, target: usize, max_error: f64) -> Simplified {
    let (pos, tris, src) = weld(m, 1e-9);
    let n = pos.len();
    let mut lo = v3(f64::MAX, f64::MAX, f64::MAX);
    let mut hi = v3(f64::MIN, f64::MIN, f64::MIN);
    for p in &pos {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let scale = if n > 0 {
        (hi - lo).len().max(1e-9)
    } else {
        1.0
    };
    let mut s = State {
        pos,
        q: vec![Quadric::default(); n],
        ver: vec![0; n],
        dead: vec![false; n],
        locked: vec![false; n],
        alive: vec![true; tris.len()],
        vt: vec![vec![]; n],
        tris,
        scale,
    };
    let mut edges: HashMap<(u32, u32), (u32, u32)> = HashMap::with_capacity(s.tris.len() * 2);
    for (t, tri) in s.tris.iter().enumerate() {
        for k in 0..3 {
            let e = edges
                .entry(edge_key(tri[k], tri[(k + 1) % 3]))
                .or_insert((0, t as u32));
            e.0 += 1;
        }
        let [a, b, c] = tri.map(|i| s.pos[i as usize]);
        let nn = tri_normal(a, b, c);
        let l = nn.len();
        if l > 0.0 {
            let nu = nn * (1.0 / l);
            let qp = Quadric::plane(nu, -nu.dot(a));
            for i in tri {
                s.q[*i as usize].add(&qp);
            }
        }
        for i in tri {
            s.vt[*i as usize].push(t as u32);
        }
    }
    for (&(a, b), &(count, t)) in &edges {
        if count > 2 {
            s.locked[a as usize] = true;
            s.locked[b as usize] = true;
        } else if count == 1 {
            let tri = s.tris[t as usize];
            let [p0, p1, p2] = tri.map(|i| s.pos[i as usize]);
            let nn = tri_normal(p0, p1, p2);
            let (pa, pb) = (s.pos[a as usize], s.pos[b as usize]);
            let m = (pb - pa).cross(nn);
            let l = m.len();
            if l > 0.0 {
                let mu = m * (1.0 / l);
                let qp = Quadric::plane(mu, -mu.dot(pa));
                s.q[a as usize].add(&qp);
                s.q[b as usize].add(&qp);
            }
        }
    }
    let mut keys: Vec<(u32, u32)> = edges.keys().copied().collect();
    keys.sort_unstable();
    let mut heap: BinaryHeap<Cand> = keys.iter().filter_map(|&(a, b)| s.cand(a, b)).collect();
    let mut live = s.tris.len();
    let limit = max_error.max(0.0).powi(2);
    let mut error = 0.0f64;
    while live > target {
        let Some(c) = heap.pop() else { break };
        if c.cost > limit {
            break;
        }
        let (u, v) = (c.u, c.v);
        if s.dead[u as usize]
            || s.dead[v as usize]
            || s.ver[u as usize] != c.vu
            || s.ver[v as usize] != c.vv
        {
            continue;
        }
        if !s.valid(u, v, c.p) {
            continue;
        }
        live -= s.collapse(u, v, c.p);
        error = error.max(c.cost.sqrt());
        for (w, _) in s.ring(u) {
            if let Some(c) = s.cand(u, w) {
                heap.push(c);
            }
        }
    }
    let mut map = vec![u32::MAX; n];
    let mut out = Mesh::default();
    for (t, tri) in s.tris.iter().enumerate() {
        if !s.alive[t] {
            continue;
        }
        let t2 = tri.map(|i| {
            if map[i as usize] == u32::MAX {
                map[i as usize] = out.positions.len() as u32;
                out.positions.push(s.pos[i as usize]);
            }
            map[i as usize]
        });
        out.triangles.push(t2);
        out.face_of
            .push(m.face_of.get(src[t] as usize).copied().unwrap_or(0));
    }
    Simplified { mesh: out, error }
}

/// Levels of detail: level 0 is the input; coarser levels are kept only when they drop at
/// least a third of the triangles. `errors[k]` is the bound of level k (0 for level 0).
#[derive(Clone, Debug)]
pub struct Lods {
    pub levels: Vec<Mesh>,
    pub errors: Vec<f64>,
}

/// Share of triangles and error budget (fraction of the bounding diagonal) per coarse level.
/// The error is the conservative plane bound, so budgets are wider than the visible error.
pub const LOD_STEPS: [(f64, f64); 2] = [(0.25, 0.02), (0.06, 0.08)];

pub fn lods(m: &Mesh) -> Lods {
    let mut out = Lods {
        levels: vec![m.clone()],
        errors: vec![0.0],
    };
    if m.triangles.len() < 64 {
        return out;
    }
    let diag = bounds_diag(&m.positions);
    let mut prev = m.triangles.len();
    for (share, err) in LOD_STEPS {
        let target = ((m.triangles.len() as f64 * share) as usize).max(4);
        let base = out.levels.last().unwrap_or(m);
        let s = simplify(base, target, err * diag);
        if (s.mesh.triangles.len() as f64) > prev as f64 * (2.0 / 3.0)
            || s.mesh.triangles.is_empty()
        {
            break;
        }
        prev = s.mesh.triangles.len();
        let e = out.errors.last().copied().unwrap_or(0.0) + s.error;
        out.levels.push(s.mesh);
        out.errors.push(e);
    }
    out
}

pub fn bounds_diag(ps: &[V3]) -> f64 {
    if ps.is_empty() {
        return 0.0;
    }
    let mut lo = ps[0];
    let mut hi = ps[0];
    for p in ps {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    (hi - lo).len()
}

/// Coarsest level whose error, projected onto an object `px` pixels across (its bounding
/// diagonal `diag`), stays within `tol_px` pixels.
pub fn pick_lod(errors: &[f64], diag: f64, px: f32, tol_px: f32) -> usize {
    if diag <= 0.0 || !px.is_finite() {
        return 0;
    }
    let per_unit = px as f64 / diag;
    let mut k = 0;
    for (i, e) in errors.iter().enumerate() {
        if e * per_unit <= tol_px as f64 {
            k = i;
        }
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize) -> Mesh {
        let mut m = Mesh::default();
        for j in 0..=n {
            for i in 0..=n {
                m.positions.push(v3(i as f64, j as f64, 0.0));
            }
        }
        let id = |i: usize, j: usize| (j * (n + 1) + i) as u32;
        for j in 0..n {
            for i in 0..n {
                m.triangles.push([id(i, j), id(i + 1, j), id(i + 1, j + 1)]);
                m.triangles.push([id(i, j), id(i + 1, j + 1), id(i, j + 1)]);
            }
        }
        m.face_of = vec![0; m.triangles.len()];
        m
    }

    fn sphere(nu: usize, nv: usize) -> Mesh {
        let mut m = Mesh::default();
        m.positions.push(v3(0.0, 0.0, 1.0));
        for j in 1..nv {
            let th = std::f64::consts::PI * j as f64 / nv as f64;
            for i in 0..nu {
                let ph = 2.0 * std::f64::consts::PI * i as f64 / nu as f64;
                m.positions
                    .push(v3(th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos()));
            }
        }
        m.positions.push(v3(0.0, 0.0, -1.0));
        let ring = |j: usize, i: usize| (1 + (j - 1) * nu + i % nu) as u32;
        let south = (m.positions.len() - 1) as u32;
        for i in 0..nu {
            m.triangles.push([0, ring(1, i), ring(1, i + 1)]);
            m.triangles
                .push([south, ring(nv - 1, i + 1), ring(nv - 1, i)]);
        }
        for j in 1..nv - 1 {
            for i in 0..nu {
                m.triangles
                    .push([ring(j, i), ring(j + 1, i), ring(j + 1, i + 1)]);
                m.triangles
                    .push([ring(j, i), ring(j + 1, i + 1), ring(j, i + 1)]);
            }
        }
        m.face_of = vec![0; m.triangles.len()];
        m
    }

    /// Box with each face split k×k, faces with own (unwelded) vertices.
    fn fine_box(k: usize) -> Mesh {
        let mut m = Mesh::default();
        let faces: [(V3, V3, V3); 6] = [
            (v3(0., 0., 0.), v3(0., 1., 0.), v3(1., 0., 0.)),
            (v3(0., 0., 1.), v3(1., 0., 0.), v3(0., 1., 0.)),
            (v3(0., 0., 0.), v3(1., 0., 0.), v3(0., 0., 1.)),
            (v3(0., 1., 0.), v3(0., 0., 1.), v3(1., 0., 0.)),
            (v3(0., 0., 0.), v3(0., 0., 1.), v3(0., 1., 0.)),
            (v3(1., 0., 0.), v3(0., 1., 0.), v3(0., 0., 1.)),
        ];
        for (o, a, b) in faces {
            let base = m.positions.len() as u32;
            for j in 0..=k {
                for i in 0..=k {
                    let (s, t) = (i as f64 / k as f64, j as f64 / k as f64);
                    m.positions.push(o + a * s + b * t);
                }
            }
            let id = |i: usize, j: usize| base + (j * (k + 1) + i) as u32;
            for j in 0..k {
                for i in 0..k {
                    m.triangles.push([id(i, j), id(i + 1, j), id(i + 1, j + 1)]);
                    m.triangles.push([id(i, j), id(i + 1, j + 1), id(i, j + 1)]);
                }
            }
        }
        m.face_of = vec![0; m.triangles.len()];
        m
    }

    fn point_tri(p: V3, a: V3, b: V3, c: V3) -> f64 {
        // closest point on triangle (Ericson, Real-Time Collision Detection 5.1.5)
        let (ab, ac, ap) = (b - a, c - a, p - a);
        let (d1, d2) = (ab.dot(ap), ac.dot(ap));
        if d1 <= 0.0 && d2 <= 0.0 {
            return (p - a).len();
        }
        let bp = p - b;
        let (d3, d4) = (ab.dot(bp), ac.dot(bp));
        if d3 >= 0.0 && d4 <= d3 {
            return (p - b).len();
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            let v = d1 / (d1 - d3);
            return (p - (a + ab * v)).len();
        }
        let cp = p - c;
        let (d5, d6) = (ab.dot(cp), ac.dot(cp));
        if d6 >= 0.0 && d5 <= d6 {
            return (p - c).len();
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            let w = d2 / (d2 - d6);
            return (p - (a + ac * w)).len();
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
            return (p - (b + (c - b) * w)).len();
        }
        let den = 1.0 / (va + vb + vc);
        let (v, w) = (vb * den, vc * den);
        (p - (a + ab * v + ac * w)).len()
    }

    fn dist_to(m: &Mesh, p: V3) -> f64 {
        m.triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| m.positions[i as usize]);
                point_tri(p, a, b, c)
            })
            .fold(f64::MAX, f64::min)
    }

    #[test]
    fn flat_grid_collapses_without_error_and_keeps_its_border() {
        let g = grid(20);
        let s = simplify(&g, 2, 1e-9);
        assert!(
            s.mesh.triangles.len() <= g.triangles.len() / 10,
            "{}",
            s.mesh.triangles.len()
        );
        assert!(s.error < 1e-9);
        assert!((s.mesh.area() - 400.0).abs() < 1e-9, "{}", s.mesh.area());
        assert!(s.mesh.positions.iter().all(|p| p.z.abs() < 1e-12));
        for c in [
            v3(0., 0., 0.),
            v3(20., 0., 0.),
            v3(20., 20., 0.),
            v3(0., 20., 0.),
        ] {
            assert!(
                s.mesh.positions.iter().any(|p| (*p - c).len() < 1e-9),
                "corner {c:?}"
            );
        }
    }

    #[test]
    fn sphere_error_stays_within_the_reported_bound() {
        let m = sphere(32, 16);
        let n0 = m.triangles.len();
        let budget = 0.25;
        let s = simplify(&m, n0 / 4, budget);
        assert!(
            s.mesh.triangles.len() <= n0 / 4 + 2,
            "{}",
            s.mesh.triangles.len()
        );
        assert!(s.error > 0.0 && s.error <= budget + 1e-12, "{}", s.error);
        for p in &s.mesh.positions {
            let r = p.len();
            assert!((0.9..=1.05).contains(&r), "{r}");
        }
        // every original vertex is near the simplified surface
        let worst = m
            .positions
            .iter()
            .map(|p| dist_to(&s.mesh, *p))
            .fold(0.0, f64::max);
        assert!(worst <= 0.08, "{worst}");
        assert!(s.mesh.is_watertight());
    }

    /// Two planes meeting at a fold: within a zero budget the fold line must survive and
    /// every vertex stays on its plane, so the area is exact.
    #[test]
    fn fold_is_kept_exactly_within_zero_budget() {
        let mut g = grid(10);
        for p in &mut g.positions {
            p.z = (p.x - 5.0).max(0.0);
        }
        let s = simplify(&g, 2, 1e-9);
        assert!(
            s.mesh.triangles.len() < g.triangles.len() / 4,
            "{}",
            s.mesh.triangles.len()
        );
        let want = 50.0 + 50.0 * 2f64.sqrt();
        assert!((s.mesh.area() - want).abs() < 1e-6, "{}", s.mesh.area());
        for p in &s.mesh.positions {
            let flat = p.z.abs() < 1e-9 && p.x <= 5.0 + 1e-9;
            let slope = (p.z - (p.x - 5.0)).abs() < 1e-9 && p.x >= 5.0 - 1e-9;
            assert!(flat || slope, "{p:?}");
        }
        for c in [v3(5., 0., 0.), v3(5., 10., 0.)] {
            assert!(
                s.mesh.positions.iter().any(|p| (*p - c).len() < 1e-9),
                "fold end {c:?}"
            );
        }
    }

    #[test]
    fn unwelded_box_welds_and_stays_closed() {
        let b = fine_box(4);
        let s = simplify(&b, 12, 1e-9);
        assert!(s.mesh.triangles.len() <= 48, "{}", s.mesh.triangles.len());
        assert!(s.mesh.is_watertight());
        assert!((s.mesh.volume() - 1.0).abs() < 1e-9, "{}", s.mesh.volume());
        // a plain box cannot lose a triangle within a tiny error
        let plain = simplify(&fine_box(1), 1, 1e-6);
        assert_eq!(plain.mesh.triangles.len(), 12);
    }

    #[test]
    fn lods_shrink_and_pick_by_screen_size() {
        let m = sphere(64, 32);
        let l = lods(&m);
        assert!(l.levels.len() >= 2, "{}", l.levels.len());
        for w in l.levels.windows(2) {
            assert!(w[1].triangles.len() * 3 <= w[0].triangles.len() * 2);
        }
        for w in l.errors.windows(2) {
            assert!(w[1] >= w[0]);
        }
        let diag = bounds_diag(&m.positions);
        assert_eq!(
            pick_lod(&l.errors, diag, 100_000.0, 1.0),
            0,
            "big on screen: full"
        );
        assert_eq!(
            pick_lod(&l.errors, diag, 5.0, 1.0),
            l.levels.len() - 1,
            "tiny: coarsest"
        );
        assert_eq!(pick_lod(&l.errors, 0.0, 5.0, 1.0), 0);
        let small = grid(2);
        assert_eq!(lods(&small).levels.len(), 1);
    }

    #[test]
    fn degenerate_input_does_not_panic() {
        let mut m = grid(3);
        m.triangles.push([0, 0, 1]);
        m.triangles.push([0, 1, 999]);
        m.positions.push(v3(f64::NAN, 0.0, 0.0));
        let s = simplify(&m, 1, 1.0);
        assert!(s.mesh.triangles.len() <= m.triangles.len());
        let e = simplify(&Mesh::default(), 0, 1.0);
        assert!(e.mesh.triangles.is_empty());
    }
}
