// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::PI;

use super::super::Curve;
use crate::math::{v3, V3};

fn circle(r: f64) -> Curve {
    Curve::circle(v3(0.0, 0.0, 2.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), r).unwrap()
}

fn near(a: V3, b: V3) -> bool {
    (a - b).len() < 1e-12
}

#[test]
fn closest_and_perpendiculars_on_a_circle_are_exact() {
    let c = circle(3.0);
    let p = v3(7.0, 0.0, 2.0);
    let (_, q) = c.closest(p);
    assert!(near(q, v3(3.0, 0.0, 2.0)), "{q:?}");
    let feet: Vec<V3> = c.perpendicular_feet(p).into_iter().map(|t| c.point(t)).collect();
    assert!(feet.iter().any(|q| near(*q, v3(3.0, 0.0, 2.0))));
    assert!(feet.iter().any(|q| near(*q, v3(-3.0, 0.0, 2.0))));
    // off-plane point: the foot is the projection direction scaled to the radius
    let (_, q) = c.closest(v3(1.0, 1.0, 9.0));
    let s = 3.0 / 2f64.sqrt();
    assert!(near(q, v3(s, s, 2.0)), "{q:?}");
}

#[test]
fn tangent_points_from_outside_a_circle() {
    let (r, d) = (3.0, 5.0);
    let c = circle(r);
    let ts = c.tangent_points(v3(d, 0.0, 2.0), 1e-9);
    assert_eq!(ts.len(), 2, "{ts:?}");
    let (x, y) = (r * r / d, r * (1.0 - r * r / (d * d)).sqrt());
    let pts: Vec<V3> = ts.iter().map(|&t| c.point(t)).collect();
    assert!(pts.iter().any(|q| near(*q, v3(x, y, 2.0))), "{pts:?}");
    assert!(pts.iter().any(|q| near(*q, v3(x, -y, 2.0))), "{pts:?}");
    // inside the circle or out of its plane: no tangent
    assert!(c.tangent_points(v3(1.0, 0.0, 2.0), 1e-9).is_empty());
    assert!(c.tangent_points(v3(d, 0.0, 3.0), 1e-9).is_empty());
    // a straight curve has no plane
    let l = Curve::polyline(&[v3(0.0, 0.0, 0.0), v3(4.0, 0.0, 0.0)]).unwrap();
    assert!(l.plane_normal(1e-9).is_none());
}

#[test]
fn arc_length_and_fractions() {
    let c = circle(2.0);
    assert!((c.length() - 4.0 * PI).abs() < 1e-10, "{}", c.length());
    // a third of the circle by arc length is 120 degrees from the start
    let q = c.point(c.at_fraction(1.0 / 3.0));
    let a = 2.0 * PI / 3.0;
    assert!((q - v3(2.0 * a.cos(), 2.0 * a.sin(), 2.0)).len() < 1e-9, "{q:?}");
    let arc = Curve::arc(v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), 1.0, 0.0, PI / 2.0).unwrap();
    let m = arc.point(arc.at_fraction(0.5));
    let h = 0.5f64.sqrt();
    assert!((m - v3(h, h, 0.0)).len() < 1e-9, "{m:?}");
    // a polyline: half of 3 + 4 is 0.5 m into the second leg
    let l = Curve::polyline(&[v3(0.0, 0.0, 0.0), v3(3.0, 0.0, 0.0), v3(3.0, 4.0, 0.0)]).unwrap();
    assert!((l.length() - 7.0).abs() < 1e-12);
    assert!((l.point(l.at_fraction(0.5)) - v3(3.0, 0.5, 0.0)).len() < 1e-9);
}
