//! Queries and edits the freeform element needs on a [`Surface`]: exact iso and boundary
//! curves, knot refinement, transposition, Greville abscissae, closest point (grid start,
//! then Newton on the squared distance), moving one control point, area by Gauss-Legendre.

use super::{basis_funs, find_span, Curve, NurbsError, Surface};
use crate::math::{v3, V3};

/// Foot of a point on the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Closest {
    pub u: f64,
    pub v: f64,
    pub point: V3,
    pub dist: f64,
}

/// Gauss-Legendre nodes and weights on [-1, 1], 5 points (exact for degree 9).
pub(super) const GL: [(f64, f64); 5] = [
    (0.0, 0.568_888_888_888_888_9),
    (-0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (-0.906_179_845_938_664, 0.236_926_885_056_189_1),
    (0.906_179_845_938_664, 0.236_926_885_056_189_1),
];

/// Distinct knot values of the domain.
pub(super) fn breaks(knots: &[f64], p: usize, n: usize) -> Vec<f64> {
    let mut d: Vec<f64> = knots[p..=n].to_vec();
    d.dedup();
    d
}

/// Clamped uniform knots on [0, 1] for `n` control points of degree `p` (needs p < n).
pub fn clamped_knots(n: usize, p: usize) -> Vec<f64> {
    let spans = n.saturating_sub(p).max(1);
    let mut k = vec![0.0; p + 1];
    k.extend((1..spans).map(|i| i as f64 / spans as f64));
    k.extend(vec![1.0; p + 1]);
    k
}

/// Greville abscissae: where each control point "sits" in the parameter domain.
pub fn greville(knots: &[f64], p: usize, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            if p == 0 {
                knots[i]
            } else {
                knots[i + 1..=i + p].iter().sum::<f64>() / p as f64
            }
        })
        .collect()
}

impl Surface {
    /// Same surface with u and v swapped (the normal flips).
    pub fn transposed(&self) -> Surface {
        let (nu, nv) = (self.nu(), self.nv());
        let mut ctrl = Vec::with_capacity(nu * nv);
        let mut w = Vec::with_capacity(nu * nv);
        for j in 0..nv {
            for i in 0..nu {
                ctrl.push(self.ctrl()[i * nv + j]);
                w.push(self.weights()[i * nv + j]);
            }
        }
        Surface::new(
            self.deg_v(),
            self.deg_u(),
            self.knots_v().to_vec(),
            self.knots_u().to_vec(),
            nv,
            nu,
            ctrl,
            w,
        )
        .unwrap_or_else(|_| self.clone())
    }

    /// Exact curve S(u, ·) in v.
    pub fn iso_u(&self, u: f64) -> Result<Curve, NurbsError> {
        let (p, ku, nu, nv) = (self.deg_u(), self.knots_u(), self.nu(), self.nv());
        let span = find_span(ku, p, nu - 1, u);
        let n = basis_funs(ku, span, u, p);
        let mut ctrl = Vec::with_capacity(nv);
        let mut ws = Vec::with_capacity(nv);
        for j in 0..nv {
            let (mut s, mut w) = (v3(0.0, 0.0, 0.0), 0.0);
            for (k, nk) in n.iter().enumerate() {
                let i = span - p + k;
                let wi = self.weights()[i * nv + j];
                s = s + self.ctrl()[i * nv + j] * (nk * wi);
                w += nk * wi;
            }
            if !(w > 0.0) {
                return Err(NurbsError::Degenerate);
            }
            ctrl.push(s * (1.0 / w));
            ws.push(w);
        }
        Curve::new(self.deg_v(), self.knots_v().to_vec(), ctrl, ws)
    }

    /// Exact curve S(·, v) in u.
    pub fn iso_v(&self, v: f64) -> Result<Curve, NurbsError> {
        self.transposed().iso_u(v)
    }

    /// The four boundary curves, each in its own parameter direction:
    /// [S(·, v0), S(u1, ·), S(·, v1), S(u0, ·)].
    pub fn boundary(&self) -> Result<[Curve; 4], NurbsError> {
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();
        Ok([self.iso_v(v0)?, self.iso_u(u1)?, self.iso_v(v1)?, self.iso_u(u0)?])
    }

    /// Inserts every knot of `us` once in u; the shape is unchanged.
    pub fn refine_u(&self, us: &[f64]) -> Result<Surface, NurbsError> {
        let (nu, nv) = (self.nu(), self.nv());
        let mut cols = Vec::with_capacity(nv);
        for j in 0..nv {
            let pts = (0..nu).map(|i| self.ctrl()[i * nv + j]).collect();
            let ws = (0..nu).map(|i| self.weights()[i * nv + j]).collect();
            cols.push(Curve::new(self.deg_u(), self.knots_u().to_vec(), pts, ws)?.refine(us));
        }
        let n2 = cols[0].ctrl().len();
        let mut ctrl = Vec::with_capacity(n2 * nv);
        let mut w = Vec::with_capacity(n2 * nv);
        for i in 0..n2 {
            for c in &cols {
                ctrl.push(c.ctrl()[i]);
                w.push(c.weights()[i]);
            }
        }
        Surface::new(
            self.deg_u(),
            self.deg_v(),
            cols[0].knots().to_vec(),
            self.knots_v().to_vec(),
            n2,
            nv,
            ctrl,
            w,
        )
    }

    /// Inserts every knot of `vs` once in v.
    pub fn refine_v(&self, vs: &[f64]) -> Result<Surface, NurbsError> {
        Ok(self.transposed().refine_u(vs)?.transposed())
    }

    /// Midpoints of every knot span in both directions inserted (one refinement level).
    pub fn refined_once(&self) -> Result<Surface, NurbsError> {
        let mid = |b: Vec<f64>| -> Vec<f64> { b.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect() };
        let bu = breaks(self.knots_u(), self.deg_u(), self.nu());
        let bv = breaks(self.knots_v(), self.deg_v(), self.nv());
        self.refine_u(&mid(bu))?.refine_v(&mid(bv))
    }

    pub fn greville_u(&self) -> Vec<f64> {
        greville(self.knots_u(), self.deg_u(), self.nu())
    }

    pub fn greville_v(&self) -> Vec<f64> {
        greville(self.knots_v(), self.deg_v(), self.nv())
    }

    /// Copy with control point (i, j) moved to `p`.
    pub fn with_ctrl(&self, i: usize, j: usize, p: V3) -> Result<Surface, NurbsError> {
        if i >= self.nu() || j >= self.nv() {
            return Err(NurbsError::TooFewPoints);
        }
        let mut ctrl = self.ctrl().to_vec();
        ctrl[i * self.nv() + j] = p;
        Surface::new(
            self.deg_u(),
            self.deg_v(),
            self.knots_u().to_vec(),
            self.knots_v().to_vec(),
            self.nu(),
            self.nv(),
            ctrl,
            self.weights().to_vec(),
        )
    }

    /// Copy with the weight of control point (i, j) set to `w` (refused unless finite and > 0).
    pub fn with_weight(&self, i: usize, j: usize, w: f64) -> Result<Surface, NurbsError> {
        if i >= self.nu() || j >= self.nv() {
            return Err(NurbsError::TooFewPoints);
        }
        let mut ws = self.weights().to_vec();
        ws[i * self.nv() + j] = w;
        Surface::new(
            self.deg_u(),
            self.deg_v(),
            self.knots_u().to_vec(),
            self.knots_v().to_vec(),
            self.nu(),
            self.nv(),
            self.ctrl().to_vec(),
            ws,
        )
    }

    /// Copy moved by `d`.
    pub fn translated(&self, d: V3) -> Surface {
        Surface::new(
            self.deg_u(),
            self.deg_v(),
            self.knots_u().to_vec(),
            self.knots_v().to_vec(),
            self.nu(),
            self.nv(),
            self.ctrl().iter().map(|p| *p + d).collect(),
            self.weights().to_vec(),
        )
        .unwrap_or_else(|_| self.clone())
    }

    /// Exact area within quadrature error: 5-point Gauss-Legendre on each knot span, every
    /// span split in `sub` parts per direction.
    pub fn area(&self, sub: usize) -> f64 {
        let sub = sub.max(1);
        let bu = breaks(self.knots_u(), self.deg_u(), self.nu());
        let bv = breaks(self.knots_v(), self.deg_v(), self.nv());
        let pieces = |b: &[f64]| -> Vec<(f64, f64)> {
            b.windows(2)
                .flat_map(|w| {
                    (0..sub).map(move |k| {
                        let h = (w[1] - w[0]) / sub as f64;
                        (w[0] + h * k as f64, w[0] + h * (k + 1) as f64)
                    })
                })
                .collect()
        };
        let (pu, pv) = (pieces(&bu), pieces(&bv));
        let mut a = 0.0;
        for &(ua, ub) in &pu {
            let (hu, cu) = (0.5 * (ub - ua), 0.5 * (ub + ua));
            for &(va, vb) in &pv {
                let (hv, cv) = (0.5 * (vb - va), 0.5 * (vb + va));
                for &(xu, wu) in &GL {
                    for &(xv, wv) in &GL {
                        let d = self.derivs(cu + hu * xu, cv + hv * xv, 1);
                        a += wu * wv * hu * hv * d[1][0].cross(d[0][1]).len();
                    }
                }
            }
        }
        a
    }

    /// Nearest point: best of a 24 x 24 sample grid (plus knot lines), then Newton steps on
    /// |S - p|^2 kept inside the domain.
    pub fn closest(&self, p: V3) -> Closest {
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();
        let lines = |a: f64, b: f64, knots: &[f64]| -> Vec<f64> {
            let mut t: Vec<f64> = (0..=24).map(|k| a + (b - a) * k as f64 / 24.0).collect();
            t.extend(knots.iter().copied().filter(|k| *k > a && *k < b));
            t
        };
        let (mut bu, mut bv, mut bd) = (u0, v0, f64::INFINITY);
        for &u in &lines(u0, u1, self.knots_u()) {
            for &v in &lines(v0, v1, self.knots_v()) {
                let d = (self.point(u, v) - p).len();
                if d < bd {
                    (bu, bv, bd) = (u, v, d);
                }
            }
        }
        let (mut u, mut v) = (bu, bv);
        let scale = (u1 - u0).max(v1 - v0);
        for _ in 0..40 {
            let d = self.derivs(u, v, 2);
            let f = d[0][0] - p;
            let (su, sv) = (d[1][0], d[0][1]);
            let (suu, suv, svv) = (d[2][0], d[1][1], d[0][2]);
            let g = [f.dot(su), f.dot(sv)];
            let a = su.dot(su) + f.dot(suu);
            let b = su.dot(sv) + f.dot(suv);
            let c = sv.dot(sv) + f.dot(svv);
            let det = a * c - b * b;
            let (du, dv) = if det > 1e-300 && a > 0.0 {
                ((c * g[0] - b * g[1]) / det, (a * g[1] - b * g[0]) / det)
            } else {
                // gradient step scaled by the first fundamental form
                let k = 1.0 / (su.dot(su) + sv.dot(sv)).max(1e-300);
                (g[0] * k, g[1] * k)
            };
            let nu_ = (u - du).clamp(u0, u1);
            let nv_ = (v - dv).clamp(v0, v1);
            let step = (nu_ - u).abs() + (nv_ - v).abs();
            (u, v) = (nu_, nv_);
            if step <= 1e-14 * scale.max(1.0) {
                break;
            }
        }
        let q = self.point(u, v);
        let dn = (q - p).len();
        if dn <= bd {
            Closest { u, v, point: q, dist: dn }
        } else {
            Closest { u: bu, v: bv, point: self.point(bu, bv), dist: bd }
        }
    }
}
