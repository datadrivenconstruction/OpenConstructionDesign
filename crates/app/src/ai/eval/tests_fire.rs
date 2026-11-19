// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `room_sep`: the fire separation measured on a hand-made core, in both outcomes.

use super::fire_oracles::{is_rated, measure, Door, Fire, Shaft, Wall};

fn square(x: f64) -> Vec<[f64; 2]> {
    vec![[x, 0.0], [x + 4.0, 0.0], [x + 4.0, 4.0], [x, 4.0]]
}

fn walls(base: i64, x: f64) -> Vec<Wall> {
    let p = square(x);
    (0..4).map(|i| Wall { id: base + i as i64, level: 2, a: p[i], b: p[(i + 1) % 4], rated: true }).collect()
}

/// Four levels of 900 m2, two cores of rated walls and doors, a stair in each, rated slabs.
fn tower() -> Fire {
    let mut w = walls(10, 0.0);
    w.extend(walls(20, 50.0));
    // a partition far from both cores: not a core wall, rating or not
    w.push(Wall { id: 99, level: 2, a: [20.0, 0.0], b: [20.0, 9.0], rated: false });
    Fire {
        levels: (1..=4).map(|l| (l, l as usize - 1, 900.0)).collect(),
        slabs: (1..=4).map(|l| (l, true)).collect(),
        walls: w,
        doors: vec![Door { id: 1, wall: 10, rated: true }, Door { id: 2, wall: 20, rated: true }, Door { id: 3, wall: 99, rated: false }],
        shafts: vec![Shaft { level: 1, top: 4, poly: square(0.0) }, Shaft { level: 1, top: 4, poly: square(50.0) }],
        stairs: vec![(1, 2.0, 2.0), (1, 52.0, 2.0)],
    }
}

#[test]
fn a_rated_core_with_two_stairs_passes() {
    let (shown, ok) = measure(&tower(), 2500.0, 2);
    assert!(ok, "{shown}");
    assert_eq!(shown, "largest compartment 900 m2, slabs rated 3/3, core walls rated 8/8, core doors rated 2/2, protected stairs 2");
}

#[test]
fn a_compartment_over_the_rule_fails_and_the_rule_is_the_bound() {
    let mut f = tower();
    f.levels[2].2 = 2600.0;
    let (shown, ok) = measure(&f, 2500.0, 2);
    assert!(!ok && shown.starts_with("largest compartment 2600 m2"), "{shown}");
    assert!(measure(&f, 2600.0, 2).1);
}

#[test]
fn an_unrated_core_wall_door_or_slab_is_named_and_fails() {
    let mut f = tower();
    f.walls[5].rated = false;
    let (shown, ok) = measure(&f, 2500.0, 2);
    assert!(!ok, "{shown}");
    assert!(shown.contains("core walls rated 7/8") && shown.contains("wall 21 on a shaft edge has no rating"), "{shown}");
    // the second core is no longer protected: one stair left
    assert!(shown.ends_with("protected stairs 1; wall 21 on a shaft edge has no rating"), "{shown}");

    let mut f = tower();
    f.doors[1].rated = false;
    let (shown, ok) = measure(&f, 2500.0, 2);
    assert!(!ok && shown.contains("core doors rated 1/2") && shown.contains("door 2 in a core wall"), "{shown}");

    let mut f = tower();
    f.slabs[3].1 = false;
    assert!(!measure(&f, 2500.0, 2).1);
    // the slab of the first level is the ground, not a compartment boundary
    let mut f = tower();
    f.slabs[0].1 = false;
    assert!(measure(&f, 2500.0, 2).1);
}

#[test]
fn stairs_outside_a_shaft_are_not_protected() {
    let mut f = tower();
    f.stairs[1] = (1, 30.0, 2.0);
    let (shown, ok) = measure(&f, 2500.0, 2);
    assert!(!ok && shown.contains("protected stairs 1"), "{shown}");
    assert!(measure(&f, 2500.0, 1).1);
    // a stair on a level above the shaft's top is not in it
    let mut f = tower();
    f.shafts[1].top = 1;
    f.stairs[1] = (3, 52.0, 2.0);
    assert!(!measure(&f, 2500.0, 2).1);
}

#[test]
fn no_core_and_no_levels_fail_instead_of_passing_quietly() {
    let mut f = tower();
    f.shafts.clear();
    let (shown, ok) = measure(&f, 2500.0, 2);
    assert!(!ok && shown.contains("core walls rated 0/0"), "{shown}");
    assert!(!measure(&Fire::default(), 2500.0, 0).1);
}

#[test]
fn ratings_are_told_from_empty_values() {
    for v in ["EI30", "REI 60", "90", "30 min"] {
        assert!(is_rated(v), "{v}");
    }
    for v in ["", " ", "0", "None", "no", "-"] {
        assert!(!is_rated(v), "{v:?}");
    }
}

#[test]
fn the_model_reader_sees_ratings_and_an_empty_model_fails() {
    use super::oracle::{evaluate, EvalCx};
    use super::tests::editor;
    let ed = editor();
    let f = super::fire_oracles::read(&ed.model).unwrap();
    assert!(f.walls.is_empty() && f.shafts.is_empty());
    let c = super::case::Case::parse("id=\"t\"\n[[turn]]\nsay=\"x\"\n[[oracle.geom]]\nkind=\"room_sep\"\narea_max=2500.0\n").unwrap();
    let facts = super::facts::Facts::read(&ed.model).unwrap();
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer: "", ifc: None, errors_before: 0, receipts: &[] };
    let r = evaluate(&c, &cx);
    assert!(!r[0].pass && r[0].measured.contains("core walls rated 0/0"), "{:?}", r[0]);
}

/// H05's claim counts rated walls as `wt:fire|EI|REI`: by type name or by the fire rating property.
#[test]
fn wt_counts_walls_by_type_name_or_fire_rating() {
    let f = super::facts::Facts {
        tables: ["wall".to_string(), "room_sep".to_string()].into_iter().collect(),
        walls: vec![
            ("Fire wall REI 120".into(), String::new()),
            ("Partition".into(), "EI 60".into()),
            ("Partition".into(), String::new()),
        ],
        ..Default::default()
    };
    assert_eq!(f.count("wt:fire|EI|REI"), Ok(2));
    assert_eq!(f.count("room_sep+wt:fire|EI|REI"), Ok(2));
    assert!(super::facts::Facts::default().count("wt:fire").is_err(), "no walls table");
}