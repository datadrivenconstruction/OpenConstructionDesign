// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::math::v3;

/// Hyperbolic test patch z = x^2 - y^2 + x*y on [0, 2]^2.
fn f(x: f64, y: f64) -> f64 {
    x * x - y * y + x * y
}

/// Exact quadratic sections of `f`: along x at height y (x = 2t), along y at x (y = 2s).
fn along_x(y: f64) -> Curve {
    let pts = vec![v3(0.0, y, f(0.0, y)), v3(1.0, y, f(0.0, y) + y), v3(2.0, y, f(2.0, y))];
    Curve::bspline(2, vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0], pts).unwrap()
}

fn along_y(x: f64) -> Curve {
    let pts = vec![v3(x, 0.0, x * x), v3(x, 1.0, x * x + x), v3(x, 2.0, f(x, 2.0))];
    Curve::bspline(2, vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0], pts).unwrap()
}

fn grid() -> (Vec<Curve>, Vec<Curve>) {
    let k = [0.0, 1.0, 2.0];
    (k.iter().map(|&y| along_x(y)).collect(), k.iter().map(|&x| along_y(x)).collect())
}

#[test]
fn gordon_reproduces_a_biquadratic_patch_exactly() {
    let (us, vs) = grid();
    let n = network(&us, &vs, 3, 1e-6).unwrap();
    assert!(n.deviation < 1e-9, "{}", n.deviation);
    for a in 0..=10 {
        for b in 0..=10 {
            let (u, v) = (a as f64 / 10.0, b as f64 / 10.0);
            let p = n.surface.point(u, v);
            let want = v3(2.0 * u, 2.0 * v, f(2.0 * u, 2.0 * v));
            assert!((p - want).len() < 1e-9, "{u} {v}: {p:?} vs {want:?}");
        }
    }
}

#[test]
fn network_refuses_curves_that_miss_or_run_out_of_order() {
    let (us, mut vs) = grid();
    vs[1] = vs[1].translated(v3(0.0, 0.0, 0.3));
    match network(&us, &vs, 3, 1e-3) {
        Err(NetworkError::Apart { v: 1, gap, .. }) => assert!(gap > 1e-3),
        o => panic!("{o:?}"),
    }
    let (us, mut vs) = grid();
    vs.reverse();
    assert_eq!(network(&us, &vs, 3, 1e-3), Err(NetworkError::Order { family: 'u', curve: 0 }));
    assert_eq!(network(&us[..1], &vs, 3, 1e-3), Err(NetworkError::TooFewCurves));
}

/// Dome sections through a 5 x 5 grid, each curve interpolated through its own points
/// (chord-length parameters, so the crossings sit at different parameters per curve).
fn dome() -> (Vec<Curve>, Vec<Curve>, Vec<V3>) {
    let h = |x: f64, y: f64| 5.0 - 0.05 * (x * x + y * y) + 0.02 * x * y * y;
    let k = [-4.0, -2.5, 0.0, 1.5, 4.0];
    let us = k
        .iter()
        .map(|&y| Curve::interpolate(&k.iter().map(|&x| v3(x, y, h(x, y))).collect::<Vec<_>>(), 3).unwrap())
        .collect();
    let vs = k
        .iter()
        .map(|&x| Curve::interpolate(&k.iter().map(|&y| v3(x, y, h(x, y))).collect::<Vec<_>>(), 3).unwrap())
        .collect();
    let pts = k.iter().flat_map(|&x| k.iter().map(move |&y| v3(x, y, h(x, y)))).collect();
    (us, vs, pts)
}

#[test]
fn gordon_on_interpolated_curves_meets_every_crossing() {
    let (us, vs, pts) = dome();
    let n = network(&us, &vs, 3, 1e-3).unwrap();
    assert!(n.deviation < 1e-3, "{}", n.deviation);
    for p in pts {
        let c = n.surface.closest(p);
        assert!(c.dist < 1e-4, "{p:?}: {}", c.dist);
    }
}

#[test]
fn rebuild_keeps_a_patch_its_new_net_can_hold() {
    let mut ctrl = vec![];
    for i in 0..3 {
        for j in 0..3 {
            let z = [[0.0, 1.0, 0.2], [0.5, 2.0, 0.7], [0.1, 0.9, 0.0]][i][j];
            ctrl.push(v3(i as f64 * 3.0, j as f64 * 2.0, z));
        }
    }
    let k = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let s = Surface::new(2, 2, k.clone(), k, 3, 3, ctrl, vec![1.0; 9]).unwrap();
    let r = rebuild(&s, 5, 6, 3, 3).unwrap();
    assert_eq!((r.surface.nu(), r.surface.nv(), r.surface.deg_u(), r.surface.deg_v()), (5, 6, 3, 3));
    assert!(r.deviation < 1e-10, "{}", r.deviation);
    // a coarser net of the dome loses shape and says by how much
    let (us, vs, _) = dome();
    let d = network(&us, &vs, 3, 1e-3).unwrap().surface;
    let c = rebuild(&d, 4, 4, 2, 2).unwrap();
    assert!(c.deviation > 1e-6 && c.deviation < 0.5, "{}", c.deviation);
    assert_eq!(rebuild(&s, 2, 5, 2, 2), Err(NetworkError::Count));
    assert_eq!(rebuild(&s, 65, 5, 2, 2), Err(NetworkError::Count));
}
