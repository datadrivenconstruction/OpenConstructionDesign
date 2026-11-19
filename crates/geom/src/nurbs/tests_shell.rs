// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::PI;

use super::*;
use crate::math::v3;

fn rel(a: f64, b: f64) -> f64 {
    (a / b - 1.0).abs()
}

#[test]
fn layer_offsets_stack_along_the_normal() {
    assert_eq!(layer_offsets(&[0.1, 0.2, 0.05], -0.1), vec![(-0.1, 0.0), (0.0, 0.2), (0.2, 0.25)]);
    assert!(layer_offsets(&[], 0.0).is_empty());
}

#[test]
fn hemisphere_shell_volume_is_exact() {
    let c = v3(1.0, 2.0, 3.0);
    let s = dome(c, 5.0, 5.0).unwrap();
    assert!((s.point(0.3, 1.0) - v3(1.0, 2.0, 8.0)).len() < 1e-12, "apex");
    let (u0, u1) = s.domain_u();
    let n = s.normal(0.5 * (u0 + u1), 0.5).unwrap();
    let p = s.point(0.5 * (u0 + u1), 0.5);
    assert!(n.dot(p - c) > 0.0, "normal points out of the sphere");
    let shell = |a: f64, b: f64| 2.0 / 3.0 * PI * ((5.0 + b).powi(3) - (5.0 + a).powi(3));
    // rational integrands: 8 sub-spans of 5-point Gauss per knot span
    assert!(rel(s.offset_volume(0.0, 0.2, 8), shell(0.0, 0.2)) < 1e-9);
    assert!(rel(s.offset_volume(-0.1, 0.1, 8), shell(-0.1, 0.1)) < 1e-9);
    // the sheet itself has the hemisphere area
    assert!(rel(s.area(8), 2.0 * PI * 25.0) < 1e-9);
}

#[test]
fn shallow_cap_and_half_cylinder_vault() {
    // cap over r = 5 rising 2: sphere radius 7.25, solid angle 2 pi h / rho
    let s = dome(v3(0.0, 0.0, 0.0), 5.0, 2.0).unwrap();
    let rho: f64 = 7.25;
    let omega = 2.0 * PI * 2.0 / rho;
    let want = omega / 3.0 * ((rho + 0.3).powi(3) - rho.powi(3));
    assert!(rel(s.offset_volume(0.0, 0.3, 8), want) < 1e-9);
    let (u0, _) = s.domain_u();
    let rim = s.point(u0, 0.0);
    assert!(((rim.x * rim.x + rim.y * rim.y).sqrt() - 5.0).abs() < 1e-12 && rim.z.abs() < 1e-12);

    let v = vault(v3(0.0, 0.0, 4.0), v3(6.0, 0.0, 4.0), 4.0, 2.0).unwrap();
    let (u0, u1) = v.domain_u();
    let (w0, w1) = v.domain_v();
    assert!(v.normal(0.5 * (u0 + u1), 0.5 * (w0 + w1)).unwrap().z > 0.9);
    let half_annulus = PI / 2.0 * (2.25f64.powi(2) - 4.0) * 6.0;
    assert!(rel(v.offset_volume(0.0, 0.25, 8), half_annulus) < 1e-9);
    assert!(dome(v3(0.0, 0.0, 0.0), 5.0, 6.0).is_err());
    assert!(vault(v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 3.0), 4.0, 1.0).is_err());
}

#[test]
fn plan_points_project_onto_the_surface() {
    let s = dome(v3(0.0, 0.0, 0.0), 5.0, 5.0).unwrap();
    for (x, y) in [(1.0, 2.0), (-3.0, 0.5), (0.0, -4.9), (0.01, 0.0)] {
        let t = s.at_plan(x, y, 1e-9).unwrap();
        let p = s.point(t[0], t[1]);
        assert!((p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9);
        assert!((p.z - (25.0 - x * x - y * y).sqrt()).abs() < 1e-8, "{x} {y}: {}", p.z);
    }
    assert!(s.at_plan(6.0, 0.0, 1e-6).is_none());
}
