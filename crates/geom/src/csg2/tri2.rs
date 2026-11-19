//! Constrained triangulation of one input triangle: points are inserted by splitting the
//! triangle (or edge) that contains them, constraint segments by walking and flipping the
//! crossed edges (any triangulation admits a flip while an edge crosses the segment). No
//! Steiner points are created, so neighbouring triangles split their shared edge alike.
//! All decisions are exact orientations of (implicit) points in one axis projection.

use std::collections::{HashMap, HashSet, VecDeque};

use super::num::Store;

const NONE: u32 = u32::MAX;

/// Why a triangle could not be split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SplitError {
    /// A point lies outside the triangle (input surfaces are not closed manifolds).
    Outside,
    /// Two different points at one place.
    Coincident,
    /// Two constraint segments cross (an operand intersects itself).
    Crossing,
}

pub(super) struct Tri2<'a> {
    k: u8,
    st: &'a Store,
    /// Local vertex -> point id.
    v: Vec<u32>,
    local: HashMap<u32, u32>,
    t: Vec<[u32; 3]>,
    /// `n[t][i]`: neighbour across the edge opposite `t[i]`.
    n: Vec<[u32; 3]>,
    vt: Vec<u32>,
    fixed: HashSet<(u32, u32)>,
    rng: u64,
    last: u32,
}

fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

impl<'a> Tri2<'a> {
    /// Corners must be counter-clockwise in the projection along `k`.
    pub fn new(st: &'a Store, k: u8, c: [u32; 3]) -> Tri2<'a> {
        debug_assert_eq!(st.orient(k, c[0], c[1], c[2]), 1);
        Tri2 {
            k,
            st,
            v: c.to_vec(),
            local: c.iter().enumerate().map(|(i, p)| (*p, i as u32)).collect(),
            t: vec![[0, 1, 2]],
            n: vec![[NONE; 3]],
            vt: vec![0, 0, 0],
            fixed: HashSet::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
            last: 0,
        }
    }

    fn o(&self, a: u32, b: u32, c: u32) -> i8 {
        self.st.orient(
            self.k,
            self.v[a as usize],
            self.v[b as usize],
            self.v[c as usize],
        )
    }

    fn rand3(&mut self) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % 3) as usize
    }

    fn set(&mut self, t: u32, v: [u32; 3], n: [u32; 3]) {
        if t as usize == self.t.len() {
            self.t.push(v);
            self.n.push(n);
        } else {
            self.t[t as usize] = v;
            self.n[t as usize] = n;
        }
        for x in v {
            self.vt[x as usize] = t;
        }
    }

    fn renbr(&mut self, x: u32, old: u32, new: u32) {
        if x == NONE {
            return;
        }
        for s in self.n[x as usize].iter_mut() {
            if *s == old {
                *s = new;
            }
        }
    }

    /// Inserts point `p` (idempotent).
    pub fn insert(&mut self, p: u32) -> Result<(), SplitError> {
        if self.local.contains_key(&p) {
            return Ok(());
        }
        let q = self.v.len() as u32;
        self.v.push(p);
        self.vt.push(NONE);
        self.local.insert(p, q);
        let mut t = self.last;
        let mut guard = 4 * self.t.len() + 64;
        'walk: loop {
            guard -= 1;
            if guard == 0 {
                return Err(SplitError::Outside);
            }
            let tv = self.t[t as usize];
            let r = self.rand3();
            let mut os = [0i8; 3];
            for j in 0..3 {
                let i = (r + j) % 3;
                os[i] = self.o(tv[(i + 1) % 3], tv[(i + 2) % 3], q);
                if os[i] < 0 {
                    let nb = self.n[t as usize][i];
                    if nb == NONE {
                        return Err(SplitError::Outside);
                    }
                    t = nb;
                    continue 'walk;
                }
            }
            let zeros: Vec<usize> = (0..3).filter(|i| os[*i] == 0).collect();
            match zeros.len() {
                0 => self.split3(t, q),
                1 => self.split_edge(t, zeros[0], q),
                _ => return Err(SplitError::Coincident),
            }
            self.last = t;
            return Ok(());
        }
    }

    fn split3(&mut self, t: u32, q: u32) {
        let [a, b, c] = self.t[t as usize];
        let [na, nb, nc] = self.n[t as usize];
        let t1 = self.t.len() as u32;
        let t2 = t1 + 1;
        self.set(t, [a, b, q], [t1, t2, nc]);
        self.set(t1, [b, c, q], [t2, t, na]);
        self.set(t2, [c, a, q], [t, t1, nb]);
        self.renbr(na, t, t1);
        self.renbr(nb, t, t2);
    }

    fn split_edge(&mut self, t: u32, i: usize, q: u32) {
        let tv = self.t[t as usize];
        let (a, b, c) = (tv[i], tv[(i + 1) % 3], tv[(i + 2) % 3]);
        let tn = self.n[t as usize];
        let (u, nb_, nc_) = (tn[i], tn[(i + 1) % 3], tn[(i + 2) % 3]);
        let t1 = self.t.len() as u32;
        let (mut u0, mut u1) = (NONE, NONE);
        if u != NONE {
            u1 = u;
            u0 = t1 + 1;
        }
        self.set(t, [a, b, q], [u0, t1, nc_]);
        self.set(t1, [a, q, c], [u1, nb_, t]);
        self.renbr(nb_, t, t1);
        if u != NONE {
            let uv = self.t[u as usize];
            let j = (0..3)
                .find(|j| uv[*j] != b && uv[*j] != c)
                .expect("opposite");
            let d = uv[j];
            let un = self.n[u as usize];
            let (m_c, m_b) = (un[(j + 1) % 3], un[(j + 2) % 3]);
            // u = (d, c, b): U1 = (d, c, q) reuses u, U0 = (d, q, b) is new
            self.set(u, [d, c, q], [t1, u0, m_b]);
            self.set(u0, [d, q, b], [t, m_c, u]);
            self.renbr(m_c, u, u0);
        }
        if self.fixed.remove(&key(b, c)) {
            self.fixed.insert(key(b, q));
            self.fixed.insert(key(q, c));
        }
    }

    /// Triangles around local vertex `a`.
    fn around(&self, a: u32) -> Vec<u32> {
        let t0 = self.vt[a as usize];
        let mut out = vec![t0];
        let idx = |t: u32| {
            self.t[t as usize]
                .iter()
                .position(|x| *x == a)
                .expect("vertex")
        };
        let mut t = t0;
        loop {
            let i = idx(t);
            let nx = self.n[t as usize][(i + 1) % 3];
            if nx == NONE {
                break;
            }
            if nx == t0 {
                return out;
            }
            out.push(nx);
            t = nx;
        }
        t = t0;
        loop {
            let i = idx(t);
            let nx = self.n[t as usize][(i + 2) % 3];
            if nx == NONE || nx == t0 {
                break;
            }
            out.push(nx);
            t = nx;
        }
        out
    }

    /// Triangle holding edge a-b and the index of its third vertex.
    fn find_edge(&self, a: u32, b: u32) -> Option<(u32, usize)> {
        for t in self.around(a) {
            let tv = self.t[t as usize];
            if tv.contains(&b) {
                let i = (0..3).find(|i| tv[*i] != a && tv[*i] != b)?;
                return Some((t, i));
            }
        }
        None
    }

    fn flip(&mut self, t: u32, i: usize) {
        let tv = self.t[t as usize];
        let (a, b, c) = (tv[i], tv[(i + 1) % 3], tv[(i + 2) % 3]);
        let tn = self.n[t as usize];
        let (u, n_b, n_c) = (tn[i], tn[(i + 1) % 3], tn[(i + 2) % 3]);
        let uv = self.t[u as usize];
        let j = (0..3)
            .find(|j| uv[*j] != b && uv[*j] != c)
            .expect("opposite");
        let d = uv[j];
        let un = self.n[u as usize];
        let (m_c, m_b) = (un[(j + 1) % 3], un[(j + 2) % 3]);
        self.set(t, [a, b, d], [m_c, u, n_c]);
        self.set(u, [a, d, c], [m_b, n_b, t]);
        self.renbr(m_c, u, t);
        self.renbr(n_b, t, u);
    }

    /// Forces the segment between points `pa` and `pb` into the triangulation.
    pub fn constrain(&mut self, pa: u32, pb: u32) -> Result<(), SplitError> {
        let (Some(&a), Some(&b)) = (self.local.get(&pa), self.local.get(&pb)) else {
            return Err(SplitError::Outside);
        };
        let mut st = vec![(a, b)];
        let mut guard = 8 * self.v.len() + 64;
        while let Some((a, b)) = st.pop() {
            guard -= 1;
            if guard == 0 {
                return Err(SplitError::Crossing);
            }
            if a == b {
                continue;
            }
            if self.find_edge(a, b).is_some() {
                self.fixed.insert(key(a, b));
                continue;
            }
            // wedge at `a` that holds the direction to `b`
            let mut start = None;
            for t in self.around(a) {
                let tv = self.t[t as usize];
                let i = tv.iter().position(|x| *x == a).expect("vertex");
                let (u, w) = (tv[(i + 1) % 3], tv[(i + 2) % 3]);
                let (ou, ow) = (self.o(a, u, b), self.o(a, w, b));
                if ou == 0 && ow < 0 {
                    start = Some(Err(u));
                    break;
                }
                if ow == 0 && ou > 0 {
                    start = Some(Err(w));
                    break;
                }
                if ou > 0 && ow < 0 {
                    start = Some(Ok((t, u, w)));
                    break;
                }
            }
            let (t0, mut r, mut l) = match start {
                None => return Err(SplitError::Outside),
                Some(Err(x)) => {
                    self.fixed.insert(key(a, x));
                    st.push((x, b));
                    continue;
                }
                Some(Ok(s)) => s,
            };
            let mut crossed = vec![(r, l)];
            let mut t = t0;
            let mut split = None;
            loop {
                if self.fixed.contains(&key(r, l)) {
                    return Err(SplitError::Crossing);
                }
                let (nt, _) = self.across(t, r, l).ok_or(SplitError::Outside)?;
                let tv = self.t[nt as usize];
                let x = *tv.iter().find(|x| **x != r && **x != l).expect("third");
                if x == b {
                    break;
                }
                match self.o(a, b, x) {
                    0 => {
                        split = Some(x);
                        break;
                    }
                    s if s > 0 => l = x,
                    _ => r = x,
                }
                crossed.push((r, l));
                t = nt;
            }
            if let Some(x) = split {
                st.push((x, b));
                st.push((a, x));
                continue;
            }
            let mut q: VecDeque<(u32, u32)> = crossed.into();
            let mut g2 = 64 * (q.len() + 4) * (q.len() + 4);
            while let Some((p, s)) = q.pop_front() {
                g2 -= 1;
                if g2 == 0 {
                    return Err(SplitError::Crossing);
                }
                let (t, i) = self.find_edge(p, s).ok_or(SplitError::Outside)?;
                let tv = self.t[t as usize];
                let (a2, b2, c2) = (tv[i], tv[(i + 1) % 3], tv[(i + 2) % 3]);
                let u = self.n[t as usize][i];
                let uv = self.t[u as usize];
                let d = *uv
                    .iter()
                    .find(|x| **x != b2 && **x != c2)
                    .expect("opposite");
                if self.o(a2, d, b2) < 0 && self.o(a2, d, c2) > 0 {
                    self.flip(t, i);
                    let crosses = a2 != a
                        && a2 != b
                        && d != a
                        && d != b
                        && self.o(a, b, a2) * self.o(a, b, d) < 0;
                    if crosses {
                        q.push_back((a2, d));
                    }
                } else {
                    q.push_back((p, s));
                }
            }
            if self.find_edge(a, b).is_none() {
                return Err(SplitError::Crossing);
            }
            self.fixed.insert(key(a, b));
        }
        Ok(())
    }

    /// Neighbour of `t` across edge r-l.
    fn across(&self, t: u32, r: u32, l: u32) -> Option<(u32, usize)> {
        let tv = self.t[t as usize];
        let i = (0..3).find(|i| tv[*i] != r && tv[*i] != l)?;
        let nb = self.n[t as usize][i];
        (nb != NONE).then_some((nb, i))
    }

    /// Triangles as point ids (counter-clockwise in the projection) and constrained edges.
    pub fn finish(self) -> (Vec<[u32; 3]>, Vec<(u32, u32)>) {
        let tris = self
            .t
            .iter()
            .map(|t| t.map(|x| self.v[x as usize]))
            .collect();
        let fixed = self
            .fixed
            .iter()
            .map(|&(a, b)| key(self.v[a as usize], self.v[b as usize]))
            .collect();
        (tris, fixed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csg2::num::P;
    use crate::math::v3;

    fn store(pts: &[(f64, f64)]) -> Store {
        let mut s = Store::default();
        for (x, y) in pts {
            s.push(P::E(v3(*x, *y, 0.0)));
        }
        s
    }

    fn area(s: &Store, t: &[[u32; 3]]) -> f64 {
        t.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| s.approx(i));
                (b - a).cross(c - a).z * 0.5
            })
            .sum()
    }

    #[test]
    fn points_on_edges_and_inside_split_without_gaps() {
        let s = store(&[
            (0., 0.),
            (4., 0.),
            (0., 4.),
            (2., 0.),
            (2., 2.),
            (1., 1.),
            (0.5, 0.5),
            (1., 0.),
        ]);
        let mut t = Tri2::new(&s, 2, [0, 1, 2]);
        for p in 3..8 {
            t.insert(p).unwrap();
        }
        assert_eq!(t.insert(4), Ok(()));
        let (tris, _) = t.finish();
        // 6 boundary and 2 interior vertices: 2 * 2 + 6 - 2 triangles
        assert_eq!(tris.len(), 8);
        assert!((area(&s, &tris) - 8.0).abs() < 1e-12);
        for tr in &tris {
            assert_eq!(s.orient(2, tr[0], tr[1], tr[2]), 1);
        }
    }

    #[test]
    fn constraint_through_collinear_vertices_and_flips() {
        // fan of points near the bottom edge forces flips; the constraint passes through (2, 1)
        let mut pts = vec![(0., 0.), (8., 0.), (0., 8.), (1., 1.), (2., 1.), (5., 1.)];
        for i in 0..6 {
            pts.push((0.5 + i as f64, 0.3 + 0.2 * (i % 2) as f64));
            pts.push((0.7 + i as f64 * 0.9, 1.6 + 0.3 * (i % 3) as f64));
        }
        let s = store(&pts);
        let mut t = Tri2::new(&s, 2, [0, 1, 2]);
        for p in 3..pts.len() as u32 {
            t.insert(p).unwrap();
        }
        t.constrain(3, 5).unwrap();
        let (tris, fixed) = t.finish();
        assert!(
            fixed.contains(&(3, 4)) && fixed.contains(&(4, 5)),
            "{fixed:?}"
        );
        assert!((area(&s, &tris) - 32.0).abs() < 1e-12);
        let edges: HashSet<(u32, u32)> = tris
            .iter()
            .flat_map(|t| (0..3).map(move |k| key(t[k], t[(k + 1) % 3])))
            .collect();
        assert!(edges.contains(&(3, 4)) && edges.contains(&(4, 5)));
    }

    #[test]
    fn crossing_constraints_are_refused() {
        let s = store(&[
            (0., 0.),
            (8., 0.),
            (0., 8.),
            (1., 1.),
            (3., 3.),
            (1., 3.),
            (3., 1.),
        ]);
        let mut t = Tri2::new(&s, 2, [0, 1, 2]);
        for p in 3..7 {
            t.insert(p).unwrap();
        }
        t.constrain(3, 4).unwrap();
        assert_eq!(t.constrain(5, 6), Err(SplitError::Crossing));
    }
}
