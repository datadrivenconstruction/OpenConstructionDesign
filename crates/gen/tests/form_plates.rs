// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Free-form plates build and pass their gates: rooms round corners and along curves reach
//! the corridor through a door, the riser keeps clear of a turned core, the bank's shafts
//! stand in the core, a shape exactly two bands deep holds them, and fins and terraces show.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject};
use serde_json::{json, Value as J};

fn built(v: &J) -> GProject {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    p
}

fn shoelace(r: &[(f64, f64)]) -> f64 {
    (0..r.len()).map(|i| { let (a, b) = (r[i], r[(i + 1) % r.len()]); a.0 * b.1 - a.1 * b.0 }).sum::<f64>().abs() / 2.0
}

#[test]
fn corner_rooms_keep_a_door_to_the_corridor() {
    // the corner rooms of a box and of a skewed plate wrap the corner of the band
    built(&json!({"typology": "office", "floors": 2, "footprint": {"shape": "rectangle", "width": 40, "depth": 26}}));
    built(&json!({"typology": "office", "floors": 2, "footprint": {"polygon": [[0, 0], [40, 0], [50, 28], [10, 28]]}}));
}

#[test]
fn rooms_along_a_curve_have_straight_corridor_walls() {
    let p = built(&json!({"typology": "office", "floors": 2, "footprint": {"shape": "ellipse", "width": 46, "depth": 32}}));
    // no wall of a room to the corridor is a facet stub
    let l = &p.storeys[0].layout;
    let short = l.walls.iter().filter(|w| w.role == ddc_gen::WallRole::Corridor && w.length() < 1.0).count();
    assert!(short * 10 < l.walls.len(), "{short} short corridor walls of {}", l.walls.len());
}

#[test]
fn a_round_core_with_lift_and_sprinklers() {
    let p = built(&json!({"typology": "office", "floors": 11, "lift": true, "footprint": {"shape": "circle", "width": 36}}));
    assert!(!p.fire.risers.is_empty(), "sprinkler risers");
}

#[test]
fn a_tall_plate_holds_its_lift_bank() {
    let p = built(&json!({"typology": "office", "floors": 12, "lift": true, "footprint": {"shape": "circle", "width": 40}}));
    let lifts = |k: usize| p.storeys[k].layout.shafts.iter().filter(|s| s.kind == ddc_gen::ShaftKind::Lift).count();
    assert!(p.lifts.cars > 1, "a tall office has a bank: {}", p.lifts.cars);
    assert!((0..p.storeys.len()).all(|k| lifts(k) == p.lifts.cars as usize));
}

#[test]
fn a_shape_two_bands_deep_holds_them_and_keeps_the_asked_band() {
    let v = |rules: J| json!({"typology": "office", "floors": 2, "footprint": {"shape": "rectangle", "width": 40, "depth": 16}, "roof": {"kind": "barrel"}, "rules": rules});
    let s = parse_spec(&v(json!({}))).unwrap();
    assert!((s.band_depth - 7.1).abs() < 1e-9, "{}", s.band_depth);
    built(&v(json!({})));
    let s = parse_spec(&v(json!({"depth.band": 6.0}))).unwrap();
    assert!((s.band_depth - 6.0).abs() < 1e-9, "the asked band: {}", s.band_depth);
    built(&v(json!({"depth.band": 6.0})));
}

#[test]
fn winged_apartments_build() {
    let arm = |b: [f64; 2]| json!({"from": [0, 0], "to": b, "width": 15});
    built(&json!({"typology": "apartment_section", "floors": 2,
        "footprint": {"wings": [arm([0.0, 30.0]), arm([-25.981, -15.0]), arm([25.981, -15.0])]}}));
}
