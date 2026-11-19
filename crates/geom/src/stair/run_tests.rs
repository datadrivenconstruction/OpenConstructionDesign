// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;

fn def(pts: Vec<V2>, bulges: Vec<f64>) -> RunDef {
    RunDef {
        pts,
        bulges,
        width: 1.0,
        walk: 0.0,
        base_z: 0.0,
        height: 3.0,
        max_riser: 0.18,
        thickness: 0.15,
        min_inner: 0.10,
        headroom: 2.0,
    }
}

/// Circle of radius `r` about the origin from angle 0 to `th` in `k` equal arcs.
fn circle(r: f64, th: f64, k: usize) -> (Vec<V2>, Vec<f64>) {
    let pts = (0..=k).map(|i| dir(th * i as f64 / k as f64) * r).collect();
    (pts, vec![(th / k as f64 / 4.0).tan(); k])
}

fn check_bodies(s: &RunStair, q: &RunQty, rel: f64) {
    let ms = s.meshes(1e-4);
    assert_eq!(ms.len(), q.treads);
    let mut v = 0.0;
    for m in &ms {
        assert!(m.is_watertight());
        v += m.volume();
    }
    assert!((v - q.volume).abs() <= rel * q.volume, "mesh {v} vs exact {}", q.volume);
}

#[test]
fn straight_run_matches_the_comfort_rule_and_exact_areas() {
    let d = def(vec![v2(0.0, 0.0), v2(4.48, 0.0)], vec![0.0]);
    let s = d.layout().unwrap();
    let q = s.quantities();
    assert_eq!((q.risers, q.treads), (17, 16));
    assert!((q.going - 0.28).abs() < 1e-12);
    assert!((COMFORT.0..=COMFORT.1).contains(&q.comfort));
    for a in &q.tread_areas {
        assert!((a - 0.28).abs() < 1e-12);
    }
    assert!((q.walk_area - 4.48).abs() < 1e-12);
    // first step stands on the floor (riser deep), the others are riser + thickness deep
    let r = 3.0 / 17.0;
    let want = 0.28 * r + 15.0 * 0.28 * (r + 0.15);
    assert!((q.volume - want).abs() < 1e-12);
    assert!((q.narrow - 0.28).abs() < 1e-12);
    assert_eq!(q.headroom, None);
    check_bodies(&s, &q, 1e-9);
}

#[test]
fn curved_run_has_radial_nosings_and_annulus_treads() {
    let th = 4.48 / 2.2;
    let (pts, bulges) = circle(2.0, th, 1);
    let mut d = def(pts, bulges);
    d.walk = -0.2; // outward: walk radius 2.2
    let s = d.layout().unwrap();
    assert!((s.going - 0.28).abs() < 1e-12);
    for n in &s.nosings {
        assert!(n.left.cross(n.right).abs() < 1e-9, "nosing not radial");
        assert!(n.left.cross(n.walk).abs() < 1e-9);
        assert!((n.walk.len() - 2.2).abs() < 1e-9);
        assert!((n.left.len() - 1.5).abs() < 1e-9 && (n.right.len() - 2.5).abs() < 1e-9);
    }
    let q = s.quantities();
    let dth = th / 16.0;
    for a in &q.tread_areas {
        assert!((a - dth / 2.0 * (2.5 * 2.5 - 1.5 * 1.5)).abs() < 1e-12);
    }
    // inner edge going is the narrowest one
    assert!((q.narrow - 1.5 * dth).abs() < 1e-12);
    check_bodies(&s, &q, 1e-3);
}

#[test]
fn bad_going_is_refused_by_the_comfort_rule() {
    let d = def(vec![v2(0.0, 0.0), v2(3.0, 0.0)], vec![0.0]);
    assert!(matches!(d.layout(), Err(RunError::Comfort { .. })));
}

fn l_run() -> RunDef {
    def(
        vec![v2(0.0, 0.0), v2(2.5, 0.0), v2(2.5, 2.0)],
        vec![0.0, 0.0],
    )
}

#[test]
fn corner_is_wound_with_balanced_steps() {
    let s = l_run().layout().unwrap();
    let q = s.quantities();
    assert!((q.going - 4.5 / 16.0).abs() < 1e-12);
    // every nosing passes through its walk point, which lies between the edges
    for n in &s.nosings {
        let (a, b) = (n.left - n.walk, n.right - n.walk);
        assert!(a.cross(b).abs() < 1e-9 && a.dot(b) < 0.0);
    }
    // treads tile the L footprint without gaps or overlaps
    let footprint = 3.0 * 1.0 + 1.0 * 1.5;
    assert!((q.walk_area - footprint).abs() < 1e-9, "{}", q.walk_area);
    assert!(q.tread_areas.iter().all(|a| *a > 0.0));
    // inner (left) goings fall towards the corner and rise after it, never below min_inner
    let g: Vec<f64> = s.nosings.windows(2).map(|w| w[1].sl - w[0].sl).collect();
    assert!(g.iter().all(|x| *x >= 0.10 - 1e-12));
    let k = g
        .iter()
        .enumerate()
        .fold((0, f64::INFINITY), |b, (i, x)| if *x < b.1 { (i, *x) } else { b })
        .0;
    assert!(g[..=k].windows(2).all(|w| w[1] <= w[0] + 1e-12));
    assert!(g[k..].windows(2).all(|w| w[1] >= w[0] - 1e-12));
    // the narrowest inner going sits at the corner tread
    let sc = 2.5 / q.going;
    assert!((k as f64 + 0.5 - sc).abs() <= 1.0);
    // turn zone: straight treads outside it keep the full going on both edges
    assert!((g[0] - q.going).abs() < 1e-12 && (g[15] - q.going).abs() < 1e-12);
    check_bodies(&s, &q, 1e-9);
}

#[test]
fn winder_refused_when_inner_going_cannot_be_reached() {
    let mut d = l_run();
    d.min_inner = 0.25;
    match d.layout() {
        Err(RunError::Winder { corner, best }) => {
            assert_eq!(corner, 1);
            assert!(best > 0.10 && best < 0.25, "{best}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn spiral_headroom_is_measured_one_turn_up() {
    let (pts, bulges) = circle(1.0, 8.0, 4);
    let mut d = def(pts, bulges);
    d.height = 6.0;
    d.walk = -0.15;
    let s = d.layout().unwrap();
    assert_eq!(s.risers, 34);
    let q = s.quantities();
    let step = 8.0 / 33.0;
    let dmin = (2..33).find(|&d| (d + 1) as f64 * step > 2.0 * PI + 1e-9).unwrap();
    let want = s.riser * (dmin - 1) as f64 - 0.15;
    let h = q.headroom.unwrap();
    assert!((h - want).abs() < 1e-12, "{h} vs {want}");
    check_bodies(&s, &q, 1e-3);
    d.headroom = want + 0.1;
    match d.layout() {
        Err(RunError::Headroom {
            below,
            above,
            clearance,
        }) => {
            assert_eq!(above - below, dmin);
            assert!((clearance - want).abs() < 1e-12);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn refusals_name_the_input() {
    let mut d = def(
        vec![v2(0.0, 0.0), v2(2.0, 0.0), v2(2.0, 2.0)],
        vec![0.3, 0.0],
    );
    assert!(matches!(d.layout(), Err(RunError::Input(m)) if m.contains("corner")));
    d = def(vec![v2(0.0, 0.0), v2(4.48, 0.0)], vec![0.0]);
    d.walk = 0.5;
    assert!(matches!(d.layout(), Err(RunError::Input(m)) if m.contains("walk line")));
    let (pts, bulges) = circle(0.4, 2.0, 1);
    d = def(pts, bulges);
    assert!(matches!(d.layout(), Err(RunError::Input(m)) if m.contains("tighter")));
}
