// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Filtered exact arithmetic over implicit points.
//!
//! A point is either an input vertex or the crossing of an input segment `p-q` with the zero
//! set of an affine function `f` (a plane through three input vertices, or a line through two
//! input vertices in an axis projection): `X = (f(p) q - f(q) p) / (f(p) - f(q))`. Predicates
//! use homogeneous coordinates; each is evaluated first in `f64` with a running error bound
//! and, only when the sign is not certain, again with floating-point expansions (exact).

use crate::math::{v2, V3};
use crate::predicates::orient2d;

/// Twice the unit roundoff: every bound below is conservative by this factor.
const U: f64 = f64::EPSILON;

/// Ring used by the generic formulas: `F` (value + error bound) or `X` (exact expansion).
pub(super) trait Ring: Clone {
    fn c(v: f64) -> Self;
    fn add(&self, o: &Self) -> Self;
    fn sub(&self, o: &Self) -> Self;
    fn mul(&self, o: &Self) -> Self;
}

/// `f64` value with a bound on its distance to the exact value.
#[derive(Clone, Copy, Debug)]
pub(super) struct F {
    pub v: f64,
    pub e: f64,
}

impl F {
    /// Sign when certain.
    pub fn sign(self) -> Option<i8> {
        if self.e == 0.0 || self.v.abs() > self.e {
            Some(sgn(self.v))
        } else {
            None
        }
    }
}

impl Ring for F {
    fn c(v: f64) -> F {
        F { v, e: 0.0 }
    }
    fn add(&self, o: &F) -> F {
        let (v, err) = two_sum(self.v, o.v);
        let e = self.e + o.e;
        F {
            v,
            e: if e == 0.0 && err == 0.0 {
                0.0
            } else {
                (e + v.abs() * U) * (1.0 + 4.0 * U)
            },
        }
    }
    fn sub(&self, o: &F) -> F {
        self.add(&F { v: -o.v, e: o.e })
    }
    fn mul(&self, o: &F) -> F {
        let v = self.v * o.v;
        let exact = self.e == 0.0 && o.e == 0.0 && self.v.mul_add(o.v, -v) == 0.0;
        F {
            v,
            e: if exact {
                0.0
            } else {
                (self.v.abs() * o.e + o.v.abs() * self.e + self.e * o.e + v.abs() * U)
                    * (1.0 + 8.0 * U)
            },
        }
    }
}

pub(super) fn sgn(v: f64) -> i8 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

/// Exact value as a sum of `f64` components (after `norm`: increasing magnitude, non-overlapping).
#[derive(Clone, Debug)]
pub(super) struct X(Vec<f64>);

impl X {
    fn norm(mut self) -> X {
        let v = &mut self.0;
        v.retain(|x| *x != 0.0);
        loop {
            let mut changed = false;
            for i in 1..v.len() {
                let (s, e) = two_sum(v[i], v[i - 1]);
                changed |= s != v[i] || e != v[i - 1];
                v[i] = s;
                v[i - 1] = e;
            }
            if !changed {
                break;
            }
        }
        v.retain(|x| *x != 0.0);
        self
    }

    pub fn sign(&self) -> i8 {
        self.0.last().map_or(0, |x| sgn(*x))
    }
}

impl Ring for X {
    fn c(v: f64) -> X {
        X(if v == 0.0 { vec![] } else { vec![v] })
    }
    fn add(&self, o: &X) -> X {
        let mut v = Vec::with_capacity(self.0.len() + o.0.len());
        v.extend_from_slice(&self.0);
        v.extend_from_slice(&o.0);
        X(v).norm()
    }
    fn sub(&self, o: &X) -> X {
        let mut v = Vec::with_capacity(self.0.len() + o.0.len());
        v.extend_from_slice(&self.0);
        v.extend(o.0.iter().map(|x| -x));
        X(v).norm()
    }
    fn mul(&self, o: &X) -> X {
        let mut v = Vec::with_capacity(2 * self.0.len() * o.0.len());
        for a in &self.0 {
            for b in &o.0 {
                let p = a * b;
                v.push(p);
                let l = a.mul_add(*b, -p);
                if l != 0.0 {
                    v.push(l);
                }
            }
        }
        X(v).norm()
    }
}

/// Coordinates `(u, v)` of a point projected along axis `k` (cyclic, so the projected
/// orientation has the sign of the normal's `k` component).
pub(super) fn proj(k: u8, p: V3) -> (f64, f64) {
    match k {
        0 => (p.y, p.z),
        1 => (p.z, p.x),
        _ => (p.x, p.y),
    }
}

/// Exact sign of the projected orientation of three input vertices.
pub(super) fn orient2(k: u8, a: V3, b: V3, c: V3) -> i8 {
    let f = |p| {
        let (u, w) = proj(k, p);
        v2(u, w)
    };
    sgn(orient2d(f(a), f(b), f(c)))
}

fn o3<T: Ring>(a: V3, b: V3, c: V3, d: V3) -> T {
    let s = |x: f64, y: f64| T::c(x).sub(&T::c(y));
    let (ax, ay, az) = (s(a.x, d.x), s(a.y, d.y), s(a.z, d.z));
    let (bx, by, bz) = (s(b.x, d.x), s(b.y, d.y), s(b.z, d.z));
    let (cx, cy, cz) = (s(c.x, d.x), s(c.y, d.y), s(c.z, d.z));
    let t1 = ax.mul(&by.mul(&cz).sub(&bz.mul(&cy)));
    let t2 = ay.mul(&bz.mul(&cx).sub(&bx.mul(&cz)));
    let t3 = az.mul(&bx.mul(&cy).sub(&by.mul(&cx)));
    t1.add(&t2).add(&t3)
}

fn o2<T: Ring>(k: u8, a: V3, b: V3, c: V3) -> T {
    let (a, b, c) = (proj(k, a), proj(k, b), proj(k, c));
    let s = |x: f64, y: f64| T::c(x).sub(&T::c(y));
    s(a.0, c.0)
        .mul(&s(b.1, c.1))
        .sub(&s(a.1, c.1).mul(&s(b.0, c.0)))
}

/// Affine function whose zero set cuts a segment.
#[derive(Clone, Copy, Debug)]
pub(super) enum Lin {
    /// Plane through three input vertices.
    Plane([V3; 3]),
    /// Line through two input vertices in the projection along an axis.
    Line(u8, [V3; 2]),
}

impl Lin {
    fn at<T: Ring>(&self, x: V3) -> T {
        match *self {
            Lin::Plane([a, b, c]) => o3(a, b, c, x),
            Lin::Line(k, [a, b]) => o2(k, a, b, x),
        }
    }
}

/// Explicit or implicit point.
#[derive(Clone, Copy, Debug)]
pub(super) enum P {
    E(V3),
    L { p: V3, q: V3, f: Lin },
}

impl P {
    /// Homogeneous coordinates `[x, y, z, w]`.
    pub fn hom<T: Ring>(&self) -> [T; 4] {
        match *self {
            P::E(v) => [T::c(v.x), T::c(v.y), T::c(v.z), T::c(1.0)],
            P::L { p, q, f } => {
                let (dp, dq): (T, T) = (f.at(p), f.at(q));
                let c = |pc: f64, qc: f64| dp.mul(&T::c(qc)).sub(&dq.mul(&T::c(pc)));
                [c(p.x, q.x), c(p.y, q.y), c(p.z, q.z), dp.sub(&dq)]
            }
        }
    }

    /// Rounded coordinates.
    pub fn approx(&self) -> V3 {
        match *self {
            P::E(v) => v,
            P::L { p, q, f } => {
                let (dp, dq): (F, F) = (f.at(p), f.at(q));
                let t = (dp.v / (dp.v - dq.v)).clamp(0.0, 1.0);
                p + (q - p) * t
            }
        }
    }
}

/// Point table with cached filtered homogeneous coordinates.
#[derive(Default)]
pub(super) struct Store {
    pub pts: Vec<P>,
    hf: Vec<[F; 4]>,
}

fn pick<T: Clone>(h: &[T; 4], k: u8) -> [T; 3] {
    let (u, v) = match k {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    [h[u].clone(), h[v].clone(), h[3].clone()]
}

fn det3<T: Ring>(a: &[T; 3], b: &[T; 3], c: &[T; 3]) -> T {
    let m1 = b[1].mul(&c[2]).sub(&c[1].mul(&b[2]));
    let m2 = b[0].mul(&c[2]).sub(&c[0].mul(&b[2]));
    let m3 = b[0].mul(&c[1]).sub(&c[0].mul(&b[1]));
    a[0].mul(&m1).sub(&a[1].mul(&m2)).add(&a[2].mul(&m3))
}

impl Store {
    pub fn push(&mut self, p: P) -> u32 {
        self.hf.push(p.hom::<F>());
        self.pts.push(p);
        (self.pts.len() - 1) as u32
    }

    /// Exact sign of the orientation of three points projected along axis `k`.
    pub fn orient(&self, k: u8, a: u32, b: u32, c: u32) -> i8 {
        let (pa, pb, pc) = (
            &self.pts[a as usize],
            &self.pts[b as usize],
            &self.pts[c as usize],
        );
        if let (P::E(x), P::E(y), P::E(z)) = (pa, pb, pc) {
            return orient2(k, *x, *y, *z);
        }
        let h = [a, b, c].map(|i| pick(&self.hf[i as usize], k));
        let d = det3(&h[0], &h[1], &h[2]);
        let ws = [h[0][2].sign(), h[1][2].sign(), h[2][2].sign()];
        if let (Some(s), [Some(w0), Some(w1), Some(w2)]) = (d.sign(), ws) {
            return s * w0 * w1 * w2;
        }
        let h = [pa, pb, pc].map(|p| pick(&p.hom::<X>(), k));
        let d = det3(&h[0], &h[1], &h[2]);
        d.sign() * h[0][2].sign() * h[1][2].sign() * h[2][2].sign()
    }

    /// Exact sign of `coord_k(a) - coord_k(b)` (`k` = 0, 1, 2 for x, y, z).
    pub fn cmp_axis(&self, k: usize, a: u32, b: u32) -> i8 {
        let (pa, pb) = (&self.pts[a as usize], &self.pts[b as usize]);
        if let (P::E(x), P::E(y)) = (pa, pb) {
            let (x, y) = ([x.x, x.y, x.z][k], [y.x, y.y, y.z][k]);
            return sgn(x - y);
        }
        let (ha, hb) = (&self.hf[a as usize], &self.hf[b as usize]);
        let d = ha[k].mul(&hb[3]).sub(&hb[k].mul(&ha[3]));
        if let (Some(s), Some(wa), Some(wb)) = (d.sign(), ha[3].sign(), hb[3].sign()) {
            return s * wa * wb;
        }
        let (ha, hb) = (pa.hom::<X>(), pb.hom::<X>());
        ha[k].mul(&hb[3]).sub(&hb[k].mul(&ha[3])).sign() * ha[3].sign() * hb[3].sign()
    }

    /// Lexicographic exact order along the axes in the given priority.
    pub fn lex(&self, axes: [usize; 3], a: u32, b: u32) -> std::cmp::Ordering {
        for k in axes {
            match self.cmp_axis(k, a, b) {
                1 => return std::cmp::Ordering::Greater,
                -1 => return std::cmp::Ordering::Less,
                _ => {}
            }
        }
        std::cmp::Ordering::Equal
    }

    pub fn approx(&self, i: u32) -> V3 {
        self.pts[i as usize].approx()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v3;

    #[test]
    fn implicit_points_on_their_segment_are_exactly_collinear() {
        let mut s = Store::default();
        let (p, q) = (v3(0.1, 0.3, -0.7), v3(0.9, 0.2, 1.3));
        let pl = Lin::Plane([v3(0., 0., 0.11), v3(1., 0., 0.13), v3(0., 1., 0.17)]);
        let a = s.push(P::E(p));
        let b = s.push(P::E(q));
        let x = s.push(P::L { p, q, f: pl });
        for k in 0..3 {
            assert_eq!(s.orient(k, a, b, x), 0);
            assert_eq!(s.orient(k, x, a, b), 0);
        }
        assert_eq!(s.cmp_axis(2, a, x), -1);
        assert_eq!(s.cmp_axis(2, b, x), 1);
    }

    #[test]
    fn two_constructions_of_one_point_compare_equal() {
        // crossing of edge p-q with the plane z = 0.25 and with the line through r-s in x/z
        let (p, q) = (v3(0.0, 0.0, 0.0), v3(1.0, 1.0, 1.0));
        let mut s = Store::default();
        let a = s.push(P::L {
            p,
            q,
            f: Lin::Plane([v3(0., 0., 0.25), v3(1., 0., 0.25), v3(0., 1., 0.25)]),
        });
        let b = s.push(P::L {
            p,
            q,
            f: Lin::Line(1, [v3(-3., 0., 0.25), v3(5., 1., 0.25)]),
        });
        for k in 0..3 {
            assert_eq!(s.cmp_axis(k, a, b), 0);
        }
    }

    #[test]
    fn exact_path_resolves_a_near_tie() {
        let x = X::c(1.0).add(&X::c(1e-30)).sub(&X::c(1.0));
        assert_eq!(x.sign(), 1);
        let y = X::c(3.0).mul(&X::c(1.0 / 3.0)).sub(&X::c(1.0));
        assert_eq!(y.sign(), sgn(3.0f64.mul_add(1.0 / 3.0, -1.0)));
    }
}
