// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Freeform support: evaluation against analytic shapes (cylinder, hyperbolic paraboloid),
//! iso curves, refinement, closest point, offset, trimmed tessellation.

use super::*;
use crate::math::{v3, V3};
use std::f64::consts::TAU;

const O: V3 = V3 { x: 0.0, y: 0.0, z: 0.0 };
const Z: V3 = V3 { x: 0.0, y: 0.0, z: 1.0 };

fn cylinder(r: f64, h: f64) -> Surface {
    let line = Curve::polyline(&[v3(r, 0.0, 0.0), v3(r, 0.0, h)]).unwrap();
    revolve(&line, O, Z, TAU).unwrap()
}

/// z = x y over [-1, 1]^2 as a bilinear patch (exact: a hyperbolic paraboloid is ruled).
fn hypar() -> Surface {
    let k = vec![0.0, 0.0, 1.0, 1.0];
    let ctrl = vec![v3(-1.0, -1.0, 1.0), v3(-1.0, 1.0, -1.0), v3(1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0)];
    Surface::new(1, 1, k.clone(), k, 2, 2, ctrl, vec![1.0; 4]).unwrap()
}

fn grid(n: usize) -> impl Iterator<Item = (f64, f64)> {
    (0..=n).flat_map(move |i| (0..=n).map(move |j| (i as f64 / n as f64, j as f64 / n as f64)))
}

#[test]
fn cylinder_points_and_area_are_exact() {
    let (r, h) = (2.5, 4.0);
    let s = cylinder(r, h);
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    for (a, b) in grid(13) {
        let p = s.point(u0 + (u1 - u0) * a, v0 + (v1 - v0) * b);
        assert!(((p.x * p.x + p.y * p.y).sqrt() - r).abs() < 1e-12, "{p:?}");
        assert!(p.z > -1e-12 && p.z < h + 1e-12);
    }
    let a = s.area(4);
    assert!((a - TAU * r * h).abs() < 1e-6 * a, "area {a}");
}

#[test]
fn hypar_points_normals_and_area() {
    let s = hypar();
    for (a, b) in grid(10) {
        let p = s.point(a, b);
        assert!((p.z - p.x * p.y).abs() < 1e-12, "{p:?}");
        let n = s.normal(a, b).unwrap();
        // gradient of z - x y is (-y, -x, 1)
        let g = v3(-p.y, -p.x, 1.0);
        let g = g * (1.0 / g.len());
        assert!(n.cross(g).len() < 1e-12, "{n:?} {g:?}");
    }
    // reference: midpoint sum of sqrt(1 + x^2 + y^2) on a 2000^2 grid
    let m = 2000;
    let hh = 2.0 / m as f64;
    let mut want = 0.0;
    for i in 0..m {
        for j in 0..m {
            let (x, y) = (-1.0 + hh * (i as f64 + 0.5), -1.0 + hh * (j as f64 + 0.5));
            want += (1.0 + x * x + y * y).sqrt() * hh * hh;
        }
    }
    let got = s.area(8);
    assert!((got - want).abs() < 1e-6, "{got} vs {want}");
}

#[test]
fn iso_curves_and_refinement_keep_the_shape() {
    let s = cylinder(1.0, 2.0);
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let u = u0 + 0.37 * (u1 - u0);
    let v = v0 + 0.61 * (v1 - v0);
    let cu = s.iso_u(u).unwrap();
    let cv = s.iso_v(v).unwrap();
    for k in 0..=8 {
        let t = k as f64 / 8.0;
        let vv = v0 + (v1 - v0) * t;
        let uu = u0 + (u1 - u0) * t;
        assert!((cu.point(vv) - s.point(u, vv)).len() < 1e-12);
        assert!((cv.point(uu) - s.point(uu, v)).len() < 1e-12);
    }
    let r = s.refined_once().unwrap();
    assert!(r.nu() > s.nu() && r.nv() > s.nv());
    for (a, b) in grid(9) {
        let (uu, vv) = (u0 + (u1 - u0) * a, v0 + (v1 - v0) * b);
        assert!((r.point(uu, vv) - s.point(uu, vv)).len() < 1e-12);
    }
    let t = s.transposed();
    assert!((t.point(v, u) - s.point(u, v)).len() < 1e-12);
    let [b0, _, b2, _] = s.boundary().unwrap();
    assert!((b0.point(u) - s.point(u, v0)).len() < 1e-12);
    assert!((b2.point(u) - s.point(u, v1)).len() < 1e-12);
}

#[test]
fn closest_point_round_trip() {
    for s in [cylinder(1.5, 3.0), hypar()] {
        let (u0, u1) = s.domain_u();
        let (v0, v1) = s.domain_v();
        for (a, b) in [(0.2, 0.3), (0.55, 0.8), (0.9, 0.1)] {
            let (u, v) = (u0 + (u1 - u0) * a, v0 + (v1 - v0) * b);
            let n = s.normal(u, v).unwrap();
            let p = s.point(u, v) + n * 0.05;
            let c = s.closest(p);
            assert!((c.dist - 0.05).abs() < 1e-9, "{c:?}");
            assert!((c.point - s.point(u, v)).len() < 1e-8, "{c:?}");
        }
    }
    // a point on the axis of the cylinder is r away from every foot
    let c = cylinder(1.5, 3.0).closest(v3(0.0, 0.0, 1.0));
    assert!((c.dist - 1.5).abs() < 1e-9);
}

#[test]
fn offset_deviation_within_tol_and_fold_refused() {
    let s = cylinder(2.0, 3.0);
    let tol = 1e-4;
    let n = s.normal(0.3, 0.5).unwrap();
    let p = s.point(0.3, 0.5);
    // the normal points out of or into the cylinder; test both ways
    let out = if n.dot(v3(p.x, p.y, 0.0)) > 0.0 { 1.0 } else { -1.0 };
    let o = offset(&s, 0.25 * out, tol).unwrap();
    assert!(o.converged && o.max_dev <= tol, "{}", o.max_dev);
    let (u0, u1) = o.surface.domain_u();
    let (v0, v1) = o.surface.domain_v();
    for (a, b) in grid(11) {
        let q = o.surface.point(u0 + (u1 - u0) * a, v0 + (v1 - v0) * b);
        assert!(((q.x * q.x + q.y * q.y).sqrt() - 2.25).abs() < tol, "{q:?}");
    }
    assert!(matches!(offset(&s, -2.5 * out, tol), Err(NurbsError::BadThickness)));
    // a plane offsets exactly with no refinement
    let h = hypar();
    let o = offset(&h, 0.01, 1e-5).unwrap();
    assert!(o.max_dev <= 1e-5, "{}", o.max_dev);
}

#[test]
fn trimmed_tessellation_closes_when_thickened() {
    let k = vec![0.0, 0.0, 1.0, 1.0];
    let ctrl = vec![v3(0.0, 0.0, 0.0), v3(0.0, 4.0, 0.0), v3(4.0, 0.0, 0.0), v3(4.0, 4.0, 0.0)];
    let plane = Surface::new(1, 1, k.clone(), k, 2, 2, ctrl, vec![1.0; 4]).unwrap();
    let hole = vec![[0.25, 0.25], [0.75, 0.25], [0.75, 0.75], [0.25, 0.75]];
    let trim = Trim { outer: None, holes: vec![hole] };
    assert!((trim.kept_fraction() - 0.75).abs() < 1e-12);
    let sm = tessellate_trimmed(&plane, &trim, &TessTol::default()).unwrap();
    assert!((sm.mesh.area() - 12.0).abs() < 1e-9, "{}", sm.mesh.area());
    let th = thicken_between(&sm, 0.0, 0.2).unwrap();
    assert!(th.mesh.is_watertight());
    assert!((th.mesh.volume() - 2.4).abs() < 1e-9, "{}", th.mesh.volume());
    // a curved patch with an outer loop: closed and within tolerance
    let hy = hypar();
    let tri = Trim { outer: Some(vec![[0.1, 0.1], [0.9, 0.2], [0.5, 0.9]]), holes: vec![] };
    let tol = TessTol { chord: 1e-3, ..TessTol::default() };
    let sm = tessellate_trimmed(&hy, &tri, &tol).unwrap();
    assert!(thicken_between(&sm, -0.05, 0.05).unwrap().mesh.is_watertight());
    for (k, p) in sm.mesh.positions.iter().enumerate() {
        let uv = sm.uv[k];
        assert!((hy.point(uv[0], uv[1]) - *p).len() < 1e-12);
    }
    assert!(Trim { outer: Some(vec![[0.0, 0.0], [1.0, 1.0]]), holes: vec![] }.validate().is_err());
    assert!(Trim { outer: Some(vec![[0.0, 0.0], [2.0, 0.0], [0.0, 1.0]]), holes: vec![] }
        .validate()
        .is_err());
}
