// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_fem::Axes;
use serde_json::json;

use crate::blob::{read_blob, write_blob};
use crate::loads::direction;
use crate::tributary::{self as tr, Piece};

fn rect(lx: f64, ly: f64, sup: [bool; 4]) -> Vec<Piece> {
    let c = [[0.0, 0.0], [lx, 0.0], [lx, ly], [0.0, ly]];
    (0..4).map(|k| Piece { a: c[k], b: c[(k + 1) % 4], supported: sup[k] }).collect()
}

/// Depth of a piece load at station s.
fn depth(l: &tr::PieceLoad, s: f64) -> f64 {
    for t in &l.trap {
        if s >= t.0 - 1e-12 && s <= t.2 + 1e-12 {
            return t.1 + (t.3 - t.1) * (s - t.0) / (t.2 - t.0);
        }
    }
    f64::NAN
}

#[test]
fn two_way_rectangle_is_the_45_degree_rule() {
    // Lx = 6, Ly = 4: long edges carry trapezoids of height Ly/2 with 45 degree ends,
    // short edges triangles of height Ly/2; totals Lx Ly / 2 - Ly^2 / 4 and Ly^2 / 4.
    let (lx, ly) = (6.0, 4.0);
    let l = tr::two_way(&rect(lx, ly, [true; 4]));
    let long = lx * ly / 2.0 - ly * ly / 4.0;
    let short = ly * ly / 4.0;
    for (k, want) in [(0, long), (1, short), (2, long), (3, short)] {
        assert!((l[k].area() - want).abs() < 1e-12, "{k}: {} vs {want}", l[k].area());
    }
    assert!((depth(&l[0], 1.0) - 1.0).abs() < 1e-12);
    assert!((depth(&l[0], 3.0) - ly / 2.0).abs() < 1e-12);
    assert!((depth(&l[1], ly / 2.0) - ly / 2.0).abs() < 1e-12);
    let total: f64 = l.iter().map(|x| x.area()).sum();
    assert!((total - lx * ly).abs() < 1e-12);
}

#[test]
fn one_way_and_cantilever_strips() {
    // span along X: the two short edges (x = 0, x = Lx) carry Lx / 2 each per unit length
    let (lx, ly) = (6.0, 4.0);
    let l = tr::one_way(&rect(lx, ly, [true; 4]), 0.0);
    assert_eq!(l[0].area(), 0.0);
    assert!((l[1].area() - lx * ly / 2.0).abs() < 1e-12);
    assert!((depth(&l[1], 1.0) - lx / 2.0).abs() < 1e-12);
    // only x = Lx supported: a cantilever strip, all load to it
    let l = tr::one_way(&rect(lx, ly, [false, true, false, false]), 0.0);
    assert!((l[1].area() - lx * ly).abs() < 1e-12);
    // two-way with one free edge: the free edge carries nothing, the total stays p A
    let l = tr::two_way(&rect(lx, ly, [true, true, false, true]));
    assert_eq!(l[2].area(), 0.0);
    let total: f64 = l.iter().map(|x| x.area()).sum();
    assert!((total - lx * ly).abs() < 1e-9, "{total}");
}

#[test]
fn openings_remove_exactly_their_area() {
    let (lx, ly) = (6.0, 4.0);
    let ps = rect(lx, ly, [true; 4]);
    let hole = vec![[2.0, 1.5], [3.0, 1.5], [3.0, 2.5], [2.0, 2.5]];
    for one in [None, Some(0.0)] {
        let mut l = match one {
            None => tr::two_way(&ps),
            Some(d) => tr::one_way(&ps, d),
        };
        let lost = tr::holes(&ps, &mut l, std::slice::from_ref(&hole), one);
        let total: f64 = l.iter().map(|x| x.area()).sum();
        assert!(lost.abs() < 1e-12);
        assert!((total - (lx * ly - 1.0)).abs() < 1e-12, "{one:?}: {total}");
    }
}

#[test]
fn polygon_helpers() {
    let r = [[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [0.0, 2.0]];
    assert_eq!(tr::signed_area(&r), 8.0);
    assert_eq!(tr::centroid(&r), [2.0, 1.0]);
    assert!(tr::is_convex(&r));
    let l = [[0.0, 0.0], [4.0, 0.0], [4.0, 1.0], [1.0, 1.0], [1.0, 3.0], [0.0, 3.0]];
    assert!(!tr::is_convex(&l));
}

#[test]
fn directions_and_frames() {
    assert_eq!(direction("-z", "global"), Some(([0.0, 0.0, -1.0], Axes::Global)));
    assert_eq!(direction("-lz", "global"), Some(([0.0, 0.0, -1.0], Axes::Local)));
    assert_eq!(direction("y", "local"), Some(([0.0, 1.0, 0.0], Axes::Local)));
    let (v, a) = direction("[3, 0, 4]", "global").unwrap();
    assert_eq!(a, Axes::Global);
    assert!((v[0] - 0.6).abs() < 1e-15 && (v[2] - 0.8).abs() < 1e-15);
    assert_eq!(direction("[0, 0, 0]", "global"), None);
    assert_eq!(direction("w", "global"), None);
}

#[test]
fn blob_round_trip_is_bit_exact() {
    let h = json!({"v": 1, "rows": ["n:m:beam:1:start"], "cols": ["ux"]});
    let data = [0.1, -0.0, f64::MIN_POSITIVE, 1e300, -2.5];
    let b = write_blob(&h, &data);
    let r = read_blob(&b).unwrap();
    assert_eq!(r.header, h);
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&r.data), bits(&data));
    assert!(read_blob(&b[..b.len() - 3]).is_none());
    assert!(read_blob(b"XXXX").is_none());
}
