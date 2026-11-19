// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::PI;

use super::*;
use crate::poly::area;

fn arc(r: f64, sweep: f64) -> SplineAxis {
    let c = Curve::arc(v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0), r, 0.0, sweep).unwrap();
    SplineAxis::new(c).unwrap()
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let (e, w) = (b - a, p - a);
    let t = (w.dot(e) / e.dot(e)).clamp(0.0, 1.0);
    (w - e * t).len()
}

/// An S-shaped free axis through five plan points.
fn wave() -> SplineAxis {
    let p = [v2(0.0, 0.0), v2(3.0, 1.5), v2(6.0, 0.0), v2(9.0, -1.5), v2(12.0, 0.0)];
    SplineAxis::through(&p).unwrap()
}

#[test]
fn arc_axis_layer_area_is_the_ring_sector() {
    let (r, th) = (5.0, 1.2);
    let s = arc(r, th);
    assert!((s.length() - r * th).abs() < 1e-12);
    assert!((s.turning() - th).abs() < 1e-12);
    // left of a counter-clockwise arc is toward the centre: radius r - d
    let (a, b) = (-0.15, 0.1);
    let want = 0.5 * th * ((r - a).powi(2) - (r - b).powi(2));
    assert!((s.layer_area(a, b) - want).abs() < 1e-12, "{} vs {want}", s.layer_area(a, b));
    // the footprint ring converges to it
    let ring = s.layer_ring(a, b, 1e-6);
    assert!(area(&ring) > 0.0);
    assert!((area(&ring) - want).abs() < 1e-5 * want);
}

#[test]
fn free_axis_area_matches_a_fine_ring() {
    let s = wave();
    assert!(s.turning().abs() < 1e-9, "symmetric S turns back: {}", s.turning());
    for (a, b) in [(-0.2, 0.2), (0.0, 0.3), (-0.35, -0.05)] {
        let exact = s.layer_area(a, b);
        let ring = area(&s.layer_ring(a, b, 1e-7)).abs();
        assert!((exact - ring).abs() < 1e-6 * exact, "{a} {b}: {exact} vs {ring}");
    }
}

#[test]
fn offset_refit_stays_within_tolerance() {
    let s = wave();
    for (d, tol) in [(0.25, 1e-4), (-0.25, 1e-5), (0.1, 1e-6)] {
        let (fit, dev) = s.offset(d, tol).unwrap();
        assert!(dev <= tol);
        // independent check: dense true offset points against a dense polyline of the fit
        let (fa, fb) = fit.domain();
        let poly: Vec<V2> = (0..=20000).map(|k| xy(fit.point(fa + (fb - fa) * k as f64 / 20000.0))).collect();
        let (ta, tb) = s.curve().domain();
        let mut worst: f64 = 0.0;
        for k in 0..=400 {
            let q = s.offset_point(ta + (tb - ta) * k as f64 / 400.0, d);
            let m = poly
                .windows(2)
                .map(|w| seg_dist(q, w[0], w[1]))
                .fold(f64::INFINITY, f64::min);
            worst = worst.max(m);
        }
        assert!(worst < 1.5 * tol + 1e-7, "d {d}: {worst} > {tol}");
        // ends meet the true offset exactly
        assert!((xy(fit.point(fa)) - s.offset_point(ta, d)).len() < 1e-12);
        assert!((xy(fit.point(fb)) - s.offset_point(tb, d)).len() < 1e-12);
    }
}

#[test]
fn folding_offset_and_bad_axes_are_refused() {
    let s = arc(0.5, PI / 2.0);
    // toward the centre the offset folds at the radius; outward it never does
    assert!(matches!(s.check_offset(0.5), Err(SplineError::Folds { .. })));
    assert!(s.check_offset(0.49).is_ok() && s.check_offset(-3.0).is_ok());
    assert!(matches!(s.offset(0.6, 1e-4), Err(SplineError::Folds { .. })));
    let lifted = Curve::polyline(&[v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.5)]).unwrap();
    assert_eq!(SplineAxis::new(lifted), Err(SplineError::NotPlanar));
    assert!(SplineAxis::through(&[v2(1.0, 1.0), v2(1.0, 1.0)]).is_err());
}

fn rot(v: V2, a: f64) -> V2 {
    let (s, c) = a.sin_cos();
    v2(v.x * c - v.y * s, v.x * s + v.y * c)
}

#[test]
fn a_gentle_axis_through_many_points_keeps_pieces_longer_than_its_point_spacing() {
    // 25 points 0.5 m apart: a fit cut at the knots could not exceed about 0.28 m a piece
    let p: Vec<V2> = (0..=24).map(|i| v2(i as f64 * 0.5, (i as f64 * 0.25).sin())).collect();
    let s = SplineAxis::through(&p).unwrap();
    let tol = 2e-3;
    let ps = s.biarcs(tol).unwrap();
    let longest = ps.iter().map(|q| crate::arc::axis_length(q.0, q.1, q.2)).fold(0.0, f64::max);
    assert!(longest > 0.6, "{longest} in {} pieces", ps.len());
    let (ta, tb) = s.curve().domain();
    for k in 0..=4000 {
        let q = s.point(ta + (tb - ta) * k as f64 / 4000.0);
        let d = ps.iter().map(|x| piece_dist(q, *x)).fold(f64::INFINITY, f64::min);
        assert!(d <= 1.01 * tol, "{k}: {d}");
    }
}

#[test]
fn biarcs_follow_the_axis_tangent_continuously() {
    let s = wave();
    let tol = 1e-4;
    let ps = s.biarcs(tol).unwrap();
    assert!(ps.len() >= 4 && ps.len() < 400, "{}", ps.len());
    let (ta, tb) = s.curve().domain();
    assert!((ps[0].0 - s.point(ta)).len() < 1e-12 && (ps[ps.len() - 1].1 - s.point(tb)).len() < 1e-12);
    for w in ps.windows(2) {
        assert!((w[0].1 - w[1].0).len() < 1e-12);
        // end tangent of one piece equals the start tangent of the next
        let c0 = w[0].1 - w[0].0;
        let c1 = w[1].1 - w[1].0;
        let e = rot(c0 * (1.0 / c0.len()), 2.0 * w[0].2.atan());
        let st = rot(c1 * (1.0 / c1.len()), -2.0 * w[1].2.atan());
        assert!((e - st).len() < 1e-9, "{e:?} {st:?}");
    }
    // every axis point lies within the tolerance of the chain
    for k in 0..=2000 {
        let p = s.point(ta + (tb - ta) * k as f64 / 2000.0);
        let d = ps.iter().map(|q| piece_dist(p, *q)).fold(f64::INFINITY, f64::min);
        assert!(d <= 1.01 * tol, "{k}: {d}");
    }
    // an arc axis is fitted by arcs of its own radius
    let a = arc(5.0, 1.2);
    for (p, q, b) in a.biarcs(1e-6).unwrap() {
        let ax = crate::arc::ArcAxis::new(p, q, b, crate::Tol::default()).unwrap();
        assert!((ax.radius - 5.0).abs() < 1e-6 && ax.center.len() < 1e-6, "{ax:?}");
    }
}
