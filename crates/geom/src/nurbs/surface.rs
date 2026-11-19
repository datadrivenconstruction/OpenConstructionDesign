//! Tensor-product NURBS surface.

use super::{basis_funs, binom, check_knots, check_points, ders_basis, find_span, NurbsError, H};
use crate::math::V3;

/// NURBS surface; control net is row-major in u: index `i * nv + j`.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    deg_u: usize,
    deg_v: usize,
    knots_u: Vec<f64>,
    knots_v: Vec<f64>,
    nu: usize,
    nv: usize,
    ctrl: Vec<V3>,
    weights: Vec<f64>,
}

impl Surface {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        deg_u: usize,
        deg_v: usize,
        knots_u: Vec<f64>,
        knots_v: Vec<f64>,
        nu: usize,
        nv: usize,
        ctrl: Vec<V3>,
        weights: Vec<f64>,
    ) -> Result<Surface, NurbsError> {
        check_knots(&knots_u, deg_u, nu)?;
        check_knots(&knots_v, deg_v, nv)?;
        if ctrl.len() != nu * nv {
            return Err(NurbsError::TooFewPoints);
        }
        if weights.len() != ctrl.len() {
            return Err(NurbsError::BadWeight(weights.len().min(ctrl.len())));
        }
        check_points(&ctrl, &weights)?;
        Ok(Surface {
            deg_u,
            deg_v,
            knots_u,
            knots_v,
            nu,
            nv,
            ctrl,
            weights,
        })
    }

    pub fn deg_u(&self) -> usize {
        self.deg_u
    }
    pub fn deg_v(&self) -> usize {
        self.deg_v
    }
    pub fn knots_u(&self) -> &[f64] {
        &self.knots_u
    }
    pub fn knots_v(&self) -> &[f64] {
        &self.knots_v
    }
    pub fn nu(&self) -> usize {
        self.nu
    }
    pub fn nv(&self) -> usize {
        self.nv
    }
    pub fn ctrl(&self) -> &[V3] {
        &self.ctrl
    }
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    pub fn domain_u(&self) -> (f64, f64) {
        (self.knots_u[self.deg_u], self.knots_u[self.nu])
    }
    pub fn domain_v(&self) -> (f64, f64) {
        (self.knots_v[self.deg_v], self.knots_v[self.nv])
    }

    fn h(&self, i: usize, j: usize) -> H {
        let k = i * self.nv + j;
        H::of(self.ctrl[k], self.weights[k])
    }

    fn clamp(&self, u: f64, v: f64) -> (f64, f64) {
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();
        (u.clamp(u0, u1), v.clamp(v0, v1))
    }

    pub fn point(&self, u: f64, v: f64) -> V3 {
        let (u, v) = self.clamp(u, v);
        let (pu, pv) = (self.deg_u, self.deg_v);
        let su = find_span(&self.knots_u, pu, self.nu - 1, u);
        let sv = find_span(&self.knots_v, pv, self.nv - 1, v);
        let bu = basis_funs(&self.knots_u, su, u, pu);
        let bv = basis_funs(&self.knots_v, sv, v, pv);
        let mut acc = H::default();
        for (a, &nu) in bu.iter().enumerate() {
            let mut row = H::default();
            for (b, &nv) in bv.iter().enumerate() {
                row = row.add(self.h(su - pu + a, sv - pv + b).scale(nv));
            }
            acc = acc.add(row.scale(nu));
        }
        acc.point()
    }

    /// Partial derivatives up to total order `d`: result[k][l] = d^(k+l) S / du^k dv^l
    /// for k + l <= d (other entries are zero).
    pub fn derivs(&self, u: f64, v: f64, d: usize) -> Vec<Vec<V3>> {
        let (u, v) = self.clamp(u, v);
        let (pu, pv) = (self.deg_u, self.deg_v);
        let su = find_span(&self.knots_u, pu, self.nu - 1, u);
        let sv = find_span(&self.knots_v, pv, self.nv - 1, v);
        let du = d.min(pu);
        let dv = d.min(pv);
        let nu_d = ders_basis(&self.knots_u, su, u, pu, du);
        let nv_d = ders_basis(&self.knots_v, sv, v, pv, dv);
        let mut aw = vec![vec![H::default(); d + 1]; d + 1];
        for k in 0..=du {
            for l in 0..=(d - k).min(dv) {
                let mut acc = H::default();
                for a in 0..=pu {
                    let mut row = H::default();
                    for b in 0..=pv {
                        row = row.add(self.h(su - pu + a, sv - pv + b).scale(nv_d[l][b]));
                    }
                    acc = acc.add(row.scale(nu_d[k][a]));
                }
                aw[k][l] = acc;
            }
        }
        let mut skl = vec![vec![V3::default(); d + 1]; d + 1];
        for k in 0..=d {
            for l in 0..=d - k {
                let mut v = aw[k][l].xyz();
                for j in 1..=l {
                    v = v - skl[k][l - j] * (binom(l, j) * aw[0][j].w);
                }
                for i in 1..=k {
                    v = v - skl[k - i][l] * (binom(k, i) * aw[i][0].w);
                    let mut v2 = V3::default();
                    for j in 1..=l {
                        v2 = v2 + skl[k - i][l - j] * (binom(l, j) * aw[i][j].w);
                    }
                    v = v - v2 * binom(k, i);
                }
                skl[k][l] = v * (1.0 / aw[0][0].w);
            }
        }
        skl
    }

    /// Unit normal along Su x Sv. At a degenerate point (pole, collapsed edge) the normal is
    /// taken slightly inside the domain; None when it stays undefined.
    pub fn normal(&self, u: f64, v: f64) -> Option<V3> {
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();
        let (uc, vc) = (0.5 * (u0 + u1), 0.5 * (v0 + v1));
        for f in [0.0, 1e-7, 1e-5, 1e-3] {
            let d = self.derivs(u + (uc - u) * f, v + (vc - v) * f, 1);
            let (a, b) = (d[1][0], d[0][1]);
            let n = a.cross(b);
            let l = n.len();
            if l.is_finite() && l > 0.0 && l > 1e-12 * a.len() * b.len() {
                return Some(n * (1.0 / l));
            }
        }
        None
    }

    fn scale(&self) -> f64 {
        1.0 + self
            .ctrl
            .iter()
            .map(|p| p.x.abs().max(p.y.abs()).max(p.z.abs()))
            .fold(0.0, f64::max)
    }

    fn same(&self, a: usize, b: usize, eps: f64) -> bool {
        let (wa, wb) = (self.weights[a], self.weights[b]);
        (self.ctrl[a] - self.ctrl[b]).len() <= eps && (wa - wb).abs() <= 1e-12 * wa.max(wb)
    }

    /// Boundary u = u0 coincides with u = u1 (seam of a closed surface).
    pub fn closed_u(&self) -> bool {
        let eps = 1e-10 * self.scale();
        (0..self.nv).all(|j| self.same(j, (self.nu - 1) * self.nv + j, eps))
    }

    /// Boundary v = v0 coincides with v = v1.
    pub fn closed_v(&self) -> bool {
        let eps = 1e-10 * self.scale();
        (0..self.nu).all(|i| self.same(i * self.nv, i * self.nv + self.nv - 1, eps))
    }

    /// Boundary u = u0 (`end` false) or u = u1 (`end` true) collapses to one point.
    pub fn pole_u(&self, end: bool) -> bool {
        let eps = 1e-10 * self.scale();
        let i = if end { self.nu - 1 } else { 0 };
        let p0 = self.ctrl[i * self.nv];
        (0..self.nv).all(|j| (self.ctrl[i * self.nv + j] - p0).len() <= eps)
    }

    /// Boundary v = v0 (`end` false) or v = v1 (`end` true) collapses to one point.
    pub fn pole_v(&self, end: bool) -> bool {
        let eps = 1e-10 * self.scale();
        let j = if end { self.nv - 1 } else { 0 };
        let p0 = self.ctrl[j];
        (0..self.nu).all(|i| (self.ctrl[i * self.nv + j] - p0).len() <= eps)
    }
}
