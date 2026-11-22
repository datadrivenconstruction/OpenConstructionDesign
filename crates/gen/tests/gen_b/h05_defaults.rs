// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H05 by construction from the shortest brief: a 40-storey office on a 45 x 45 m plate that
//! names only a curtain wall and the lifts. Two protected stairs on every storey, sprinklers by
//! the height rule, rated stair enclosures and a curtain wall on the facade follow from the
//! typology and rules, not from the spec spelling them out.

use ddc_gen::project::{build_project, check_project, parse_spec};
use serde_json::json;

#[test]
fn a_40_storey_office_gets_its_fire_strategy_from_the_rules() {
    let spec = json!({
        "typology": "office",
        "floors": 40,
        "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "lifts": 8,
        "features": ["curtain wall"]
    });
    let s = parse_spec(&spec).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    // two stairs on every storey
    for (k, st) in p.storeys.iter().enumerate() {
        assert_eq!(st.layout.stairs.len(), 2, "storey {k}");
    }
    // sprinklers without being asked: the building is above fire.sprinkler_height
    assert!(p.fire.heads >= 400, "sprinkler heads: {}", p.fire.heads);
    // stair enclosures carry a fire rating
    assert!(p.storeys.iter().all(|st| st.layout.walls.iter().any(|w| w.fire.is_some())), "a storey without a rated wall");
    // the facade is a curtain wall on every storey
    assert!(p.storeys.iter().all(|st| !st.curtains.is_empty()), "storeys without a curtain wall");
}

#[test]
fn a_narrow_40_storey_tower_still_gets_two_stairs_and_rated_floors() {
    use ddc_gen::project::fire::{high_rise, slab_rating};
    // 12 x 30 m: about 36 people a floor, which one stair would serve below the 28 m rule
    let spec = json!({
        "typology": "office",
        "floors": 40,
        "footprint": {"polygon": [[0, 0], [30, 0], [30, 12], [0, 12]]},
        "lifts": 4
    });
    let s = parse_spec(&spec).unwrap_or_else(|e| panic!("{e}"));
    assert!(high_rise(&s));
    assert_eq!(slab_rating(&s), Some(120));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    for (k, st) in p.storeys.iter().enumerate() {
        assert_eq!(st.layout.stairs.len(), 2, "storey {k}");
    }
    // lift shaft walls carry a rating beside the stair enclosures
    let rated = |kind: ddc_gen::FireSep| p.storeys[1].layout.walls.iter().filter(|w| w.fire.is_some_and(|f| f.sep == kind && f.minutes >= 120)).count();
    assert!(rated(ddc_gen::FireSep::Stair) > 0 && rated(ddc_gen::FireSep::Compartment) > 0);
    // a low building is not a high-rise
    let low = parse_spec(&json!({"typology": "office", "floors": 3})).unwrap();
    assert_eq!(slab_rating(&low), None);
}
