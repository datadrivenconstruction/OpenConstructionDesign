//! Shells on a surface (geometry plan W9): the offsets of a layer stack, the exact volume of
//! a layer by the Steiner formula, plan projection of openings onto the surface, dome and
//! vault presets (exact rational surfaces).

use std::f64::consts::{PI, TAU};

use super::free::{breaks, GL};
use super::{revolve, ruled, Curve, NurbsError, Surface};
use crate::math::{v3, V3};

/// Offsets (a, b) along the normal of consecutive layers of thickness `thick`, the first
/// starting at `start`.
pub fn layer_offsets(thick: &[f64], start: f64) -> Vec<(f64, f64)> {
    let mut a = start;
    thick
        .iter()
        .map(|&t| {
            let r = (a, a + t);
            a += t;
            r
        })
        .collect()
}

impl Surface {
    /// Volume between the offsets `a` and `b` (a < b) along the unit normal Su x Sv: the
    /// integral over the surface of (b - a) - H (b^2 - a^2) + K (b^3 - a^3) / 3, H the mean
    /// and K the Gaussian curvature signed along that normal. Gauss-Legendre on every knot
    /// span split `sub` times; exact for an offset that does not fold, up to quadrature error.
    pub fn offset_volume(&self, a: f64, b: f64, sub: usize) -> f64 {
        let sub = sub.max(1);
        let pieces = |br: Vec<f64>| -> Vec<(f64, f64)> {
            br.windows(2)
                .flat_map(|w| {
                    (0..sub).map(move |k| {
                        let h = (w[1] - w[0]) / sub as f64;
                        (w[0] + h * k as f64, w[0] + h * (k + 1) as f64)
                    })
                })
                .collect()
        };
        let pu = pieces(breaks(self.knots_u(), self.deg_u(), self.nu()));
        let pv = pieces(breaks(self.knots_v(), self.deg_v(), self.nv()));
        let (t1, t2, t3) = (b - a, b * b - a * a, (b * b * b - a * a * a) / 3.0);
        let mut vol = 0.0;
        for &(ua, ub) in &pu {
            let (hu, cu) = (0.5 * (ub - ua), 0.5 * (ub + ua));
            for &(va, vb) in &pv {
                let (hv, cv) = (0.5 * (vb - va), 0.5 * (vb + va));
                for &(xu, wu) in &GL {
                    for &(xv, wv) in &GL {
                        let d = self.derivs(cu + hu * xu, cv + hv * xv, 2);
                        let (su, sv) = (d[1][0], d[0][1]);
                        let nr = su.cross(sv);
                        let j = nr.len();
                        if !(j > 1e-300) {
                            continue; // a pole: measure zero
                        }
                        let n = nr * (1.0 / j);
                        let (e, f, g) = (su.dot(su), su.dot(sv), sv.dot(sv));
                        let (l, m, nn) = (d[2][0].dot(n), d[1][1].dot(n), d[0][2].dot(n));
                        let den = e * g - f * f;
                        let h = (e * nn - 2.0 * f * m + g * l) / (2.0 * den);
                        let k = (l * nn - m * m) / den;
                        vol += wu * wv * hu * hv * j * (t1 - h * t2 + k * t3);
                    }
                }
            }
        }
        vol
    }

    /// Parameters of the surface point above or below the plan point (x, y): Newton on the
    /// plan position of S from the best of a 32 x 32 sample grid. None when no surface point
    /// projects there within `tol`.
    pub fn at_plan(&self, x: f64, y: f64, tol: f64) -> Option<[f64; 2]> {
        let (u0, u1) = self.domain_u();
        let (v0, v1) = self.domain_v();
        let k = 32;
        let gap = |p: V3| ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt();
        let (mut u, mut v, mut best) = (u0, v0, f64::INFINITY);
        for i in 0..=k {
            for j in 0..=k {
                let (a, b) = (u0 + (u1 - u0) * i as f64 / k as f64, v0 + (v1 - v0) * j as f64 / k as f64);
                let d = gap(self.point(a, b));
                if d < best {
                    (u, v, best) = (a, b, d);
                }
            }
        }
        for _ in 0..50 {
            let d = self.derivs(u, v, 1);
            let (fx, fy) = (d[0][0].x - x, d[0][0].y - y);
            if (fx * fx + fy * fy).sqrt() <= 1e-13 {
                break;
            }
            let (a, b, c, e) = (d[1][0].x, d[0][1].x, d[1][0].y, d[0][1].y);
            let det = a * e - b * c;
            if !(det.abs() > 1e-300) {
                break; // vertical tangent plane: keep the grid point
            }
            let du = (e * fx - b * fy) / det;
            let dv = (a * fy - c * fx) / det;
            let (nu, nv) = ((u - du).clamp(u0, u1), (v - dv).clamp(v0, v1));
            if (nu - u).abs() + (nv - v).abs() < 1e-15 {
                break;
            }
            (u, v) = (nu, nv);
        }
        (gap(self.point(u, v)) <= tol).then_some([u, v])
    }
}

/// Spherical cap over the circle of plan radius `r` around `c` (rim at `c.z`) rising `rise`
/// at the apex (`rise <= r`; equal is a hemisphere). u runs around, v from the rim to the
/// apex pole; the normal points out of the sphere.
pub fn dome(c: V3, r: f64, rise: f64) -> Result<Surface, NurbsError> {
    if !(r.is_finite() && rise.is_finite() && r > 0.0 && rise > 0.0 && rise <= r) {
        return Err(NurbsError::Degenerate);
    }
    let rho = (r * r + rise * rise) / (2.0 * rise);
    let centre = c + v3(0.0, 0.0, rise - rho);
    let a0 = ((rho - rise) / rho).clamp(-1.0, 1.0).asin();
    let prof = Curve::arc(centre, v3(1.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), rho, a0, PI / 2.0)?;
    revolve(&prof, centre, v3(0.0, 0.0, 1.0), TAU)
}

/// Segmental barrel vault along the horizontal axis `a` -> `b` (springing at `a.z`), its
/// arc spanning `span` across the axis and rising `rise` (`rise <= span / 2`; equal is a
/// half cylinder). The normal points out of the cylinder.
pub fn vault(a: V3, b: V3, span: f64, rise: f64) -> Result<Surface, NurbsError> {
    let ax = b - a;
    let flat = v3(ax.x, ax.y, 0.0);
    let half = 0.5 * span;
    if !(flat.len() > 1e-9 && ax.z.abs() <= 1e-9 && rise > 0.0 && rise <= half && half.is_finite()) {
        return Err(NurbsError::Degenerate);
    }
    let along = flat * (1.0 / flat.len());
    let up = v3(0.0, 0.0, 1.0);
    let side = up.cross(along);
    let rho = (half * half + rise * rise) / (2.0 * rise);
    let a0 = ((rho - rise) / rho).clamp(-1.0, 1.0).asin();
    let arc = Curve::arc(a + up * (rise - rho), side, up, rho, a0, PI - a0)?;
    let s = ruled(&arc, &arc.translated(ax))?;
    // the ruled patch runs u along the arc and v along the axis or the other way round:
    // keep the normal out of the cylinder
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let (um, vm) = (0.5 * (u0 + u1), 0.5 * (v0 + v1));
    match s.normal(um, vm) {
        Some(n) if n.z < 0.0 => Ok(s.transposed()),
        Some(_) => Ok(s),
        None => Err(NurbsError::Degenerate),
    }
}

#[cfg(test)]
#[path = "tests_shell.rs"]
mod tests;
