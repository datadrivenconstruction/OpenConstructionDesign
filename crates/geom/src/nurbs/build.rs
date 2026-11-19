// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Surface construction: ruled, skinned (loft through curves) and revolved.

use super::{
    averaging_knots, chord_params, interp_hom, make_compatible, unit_arc, Curve, NurbsError,
    Surface, H,
};
use crate::math::V3;
use std::f64::consts::TAU;

/// Lofted surface and the v parameter of each input curve (S(u, params[k]) = curve k).
#[derive(Clone, Debug)]
pub struct Loft {
    pub surface: Surface,
    pub params: Vec<f64>,
}

/// Surface with straight rulings between two curves: S(u, 0) = a, S(u, 1) = b.
pub fn ruled(a: &Curve, b: &Curve) -> Result<Surface, NurbsError> {
    let cs = make_compatible(&[a.clone(), b.clone()])?;
    let nu = cs[0].ctrl.len();
    let mut ctrl = Vec::with_capacity(nu * 2);
    let mut weights = Vec::with_capacity(nu * 2);
    for i in 0..nu {
        for c in &cs {
            ctrl.push(c.ctrl[i]);
            weights.push(c.weights[i]);
        }
    }
    Surface::new(
        cs[0].degree,
        1,
        cs[0].knots.clone(),
        vec![0.0, 0.0, 1.0, 1.0],
        nu,
        2,
        ctrl,
        weights,
    )
}

/// Skinned surface through the curves in order (u along the curves, v across them).
/// Interpolation runs on homogeneous control points, so each section is reproduced
/// exactly, rational ones included. `degree_v` is lowered to `curves.len() - 1` if needed.
pub fn loft(curves: &[Curve], degree_v: usize) -> Result<Loft, NurbsError> {
    if curves.len() < 2 {
        return Err(NurbsError::TooFewPoints);
    }
    if degree_v == 0 {
        return Err(NurbsError::BadDegree);
    }
    if curves.len() == 2 {
        return Ok(Loft {
            surface: ruled(&curves[0], &curves[1])?,
            params: vec![0.0, 1.0],
        });
    }
    let cs = make_compatible(curves)?;
    let k = cs.len();
    let q = degree_v.min(k - 1);
    let nu = cs[0].ctrl.len();
    let cols: Vec<Vec<V3>> = (0..nu)
        .map(|i| cs.iter().map(|c| c.ctrl[i]).collect())
        .collect();
    let params = chord_params(&cols).ok_or(NurbsError::Degenerate)?;
    let knots_v = averaging_knots(&params, q);
    let data: Vec<Vec<H>> = cs.iter().map(|c| c.hom()).collect();
    let sol = interp_hom(&params, &knots_v, q, data)?;
    let mut ctrl = Vec::with_capacity(nu * k);
    let mut weights = Vec::with_capacity(nu * k);
    for i in 0..nu {
        for (j, row) in sol.iter().enumerate() {
            let h = row[i];
            if !(h.w > 0.0) {
                return Err(NurbsError::BadWeight(i * k + j));
            }
            ctrl.push(h.point());
            weights.push(h.w);
        }
    }
    let surface = Surface::new(
        cs[0].degree,
        q,
        cs[0].knots.clone(),
        knots_v,
        nu,
        k,
        ctrl,
        weights,
    )?;
    Ok(Loft { surface, params })
}

/// [`loft`] with the v parameter of each curve given: `params` rise strictly from 0 to 1, one
/// per curve. Two lofts that share a column of points and take the same `params` meet on
/// the same edge curve, which a loft with its own chord-length parameters does not give.
pub fn loft_through(curves: &[Curve], degree_v: usize, params: &[f64]) -> Result<Loft, NurbsError> {
    let k = curves.len();
    if k < 2 {
        return Err(NurbsError::TooFewPoints);
    }
    if degree_v == 0 {
        return Err(NurbsError::BadDegree);
    }
    let rising = params.windows(2).all(|w| w[1] > w[0]);
    if params.len() != k || !rising || params[0] != 0.0 || params[k - 1] != 1.0 {
        return Err(NurbsError::BadKnots);
    }
    let cs = make_compatible(curves)?;
    let q = degree_v.min(k - 1);
    let nu = cs[0].ctrl.len();
    let knots_v = averaging_knots(params, q);
    let data: Vec<Vec<H>> = cs.iter().map(|c| c.hom()).collect();
    let sol = interp_hom(params, &knots_v, q, data)?;
    let mut ctrl = Vec::with_capacity(nu * k);
    let mut weights = Vec::with_capacity(nu * k);
    for i in 0..nu {
        for (j, row) in sol.iter().enumerate() {
            let h = row[i];
            if !(h.w > 0.0) {
                return Err(NurbsError::BadWeight(i * k + j));
            }
            ctrl.push(h.point());
            weights.push(h.w);
        }
    }
    let surface = Surface::new(cs[0].degree, q, cs[0].knots.clone(), knots_v, nu, k, ctrl, weights)?;
    Ok(Loft { surface, params: params.to_vec() })
}

/// Surface of revolution of `profile` about the axis through `origin` along `axis`, by
/// `angle` in (0, 2*pi] (right-hand rule). u runs around the axis, v along the profile.
/// Profile control points on the axis become poles.
pub fn revolve(profile: &Curve, origin: V3, axis: V3, angle: f64) -> Result<Surface, NurbsError> {
    if !(angle > 0.0 && angle <= TAU + 1e-12) {
        return Err(NurbsError::BadAngle);
    }
    let la = axis.len();
    if !(la > 0.0) || !la.is_finite() {
        return Err(NurbsError::Degenerate);
    }
    let ax = axis * (1.0 / la);
    let (knots_u, pts, ws) = unit_arc(angle.min(TAU));
    let nu = pts.len();
    let nv = profile.ctrl.len();
    let mut ctrl = vec![V3::default(); nu * nv];
    let mut weights = vec![0.0; nu * nv];
    for j in 0..nv {
        let p = profile.ctrl[j];
        let d = p - origin;
        let h = d.dot(ax);
        let o = origin + ax * h;
        let x = d - ax * h;
        let r = x.len();
        let on_axis = r <= 1e-12 * (1.0 + d.len());
        let (xh, yh) = if on_axis {
            (V3::default(), V3::default())
        } else {
            let xh = x * (1.0 / r);
            (xh, ax.cross(xh))
        };
        for i in 0..nu {
            ctrl[i * nv + j] = if on_axis {
                p
            } else {
                o + (xh * pts[i][0] + yh * pts[i][1]) * r
            };
            weights[i * nv + j] = ws[i] * profile.weights[j];
        }
    }
    Surface::new(
        2,
        profile.degree,
        knots_u,
        profile.knots.clone(),
        nu,
        nv,
        ctrl,
        weights,
    )
}
