// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A transmission tower: cross-arms with insulator strings, counts and lengths from the rules.

use ddc_gen::steel::build_frame;
use serde_json::json;

fn get(m: &[(&str, f64)], k: &str) -> f64 {
    m.iter().find(|x| x.0 == k).unwrap_or_else(|| panic!("no {k}")).1
}

fn pylon(extra: serde_json::Value) -> Result<(ddc_geom::steel::lattice::Frame, Vec<(&'static str, f64)>), ddc_gen::GenError> {
    let mut p = json!({"kind": "lattice_tower_300", "service": "power_line", "tower_height": 45, "base_width": 8});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    build_frame(&p, 0)
}

#[test]
fn a_power_line_tower_carries_three_arms_and_at_least_nine_insulator_strings() {
    let (f, m) = pylon(json!({})).unwrap();
    assert_eq!(get(&m, "cross_arms"), 3.0);
    assert_eq!(f.insulators.len(), 12, "3 arms x 2 tips x 2 strings");
    assert!(f.insulators.len() as f64 == get(&m, "insulator_strings"));
    for i in &f.insulators {
        assert!((i.length - 2.8).abs() < 1e-9 && i.z < 45.0 && i.z - i.length > 8.0, "{i:?}");
    }
    // the arms hang one above another, each pair of tips mirrored about the axis
    let mut zs: Vec<i64> = f.insulators.iter().map(|i| (i.z * 100.0).round() as i64).collect();
    zs.sort();
    zs.dedup();
    assert_eq!(zs.len(), 3);
    assert!(f.insulators.iter().any(|i| i.at.0 > 0.0) && f.insulators.iter().any(|i| i.at.0 < 0.0));
    let plain = build_frame(&json!({"kind": "lattice_tower_300", "tower_height": 45, "base_width": 8}), 0).unwrap().0;
    assert!(plain.insulators.is_empty(), "a plain tower has none");
    assert!(!plain.decks.is_empty(), "the plain tower keeps its platforms");
    assert!(f.decks.is_empty() && f.flights.is_empty() && f.lifts.is_empty(), "a pylon has no platforms, stairs or lifts");
    assert!(f.members.len() > 120, "{} members", f.members.len());
    assert_eq!(f.components(), 1, "every cross-arm hangs on the shaft");
    let tall = build_frame(&json!({"kind": "lattice_tower_300", "service": "power_line"}), 0).unwrap().0;
    assert_eq!(tall.components(), 1, "the arms of a tall pylon hang on the shaft too");
    let tower = build_frame(&json!({"kind": "lattice_tower_300", "tower_height": 300, "base_width": 125}), 0).unwrap().0;
    assert!(tower.insulators.is_empty() && !tower.decks.is_empty(), "a tower with no service named is the plain one");
}

#[test]
fn the_strings_follow_their_rules_and_a_tower_too_low_for_them_is_refused() {
    let (f, _) = pylon(json!({"rules": {"steel.line.arms": 2, "steel.line.strings": 3, "steel.line.string_length": 2.0}})).unwrap();
    assert_eq!(f.insulators.len(), 2 * 2 * 3);
    assert!(f.insulators.iter().all(|i| (i.length - 2.0).abs() < 1e-9));
    let e = pylon(json!({"rules": {"steel.line.arms": 6, "steel.line.arm_pitch": 8.0}})).unwrap_err().to_string();
    assert!(e.contains("steel.line"), "{e}");
}
