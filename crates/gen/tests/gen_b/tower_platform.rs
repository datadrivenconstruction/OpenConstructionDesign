// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One more platform on a built lattice tower: only the platform comes out, within a metre or
//! two of its height, its corners on the tower's corner chords, with one deck to land as a slab.

use ddc_gen::steel::build_frame;
use ddc_geom::steel::lattice::Frame;
use ddc_geom::V3;
use serde_json::json;

/// Distance from `p` to the nearest member of `f`.
fn gap(f: &Frame, p: V3) -> f64 {
    f.members
        .iter()
        .map(|m| {
            let (a, b) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
            (a + ab * t - p).len()
        })
        .fold(f64::MAX, f64::min)
}

#[test]
fn a_platform_added_at_200_m_sits_on_the_tower_alone() {
    let (tower, _) = build_frame(&json!({"kind": "lattice_tower_300"}), 0).unwrap();
    let (pf, _) = build_frame(&json!({"kind": "tower_platform", "at": 200}), 0).unwrap();
    assert_eq!(pf.decks.len(), 1);
    assert!((pf.decks[0].z - 200.0).abs() < 1e-9);
    assert!(pf.nodes.iter().all(|p| (195.0..=205.0).contains(&p.z)), "all within the band");
    // the deck's corners, top and bottom of its truss, sit on the tower's corner chords
    let w = pf.decks[0].half;
    for (x, y) in [(w, w), (-w, w), (-w, -w), (w, -w)] {
        for z in [200.0, 198.5] {
            let d = gap(&tower, ddc_geom::v3(x, y, z));
            assert!(d < 0.3, "corner ({x:.1}, {y:.1}, {z}) is {d:.3} m off the tower");
        }
    }
    // a platform where one already is, or above the top, is refused
    for at in [115.7, 299.5] {
        assert!(build_frame(&json!({"kind": "tower_platform", "at": at}), 0).is_err(), "{at}");
    }
}
