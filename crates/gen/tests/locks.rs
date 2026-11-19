// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Locks survive every regeneration: what breaks one is refused and reported, never applied
//! and never a silent unlock.

use ddc_gen::project::locks::{of_brief, regenerate, Keep, Lock, Target};
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap();
    let p = build_project(&s).unwrap();
    (s, p)
}

fn first_room(p: &GProject, k: usize) -> String {
    let st = &p.storeys[k];
    st.rooms.iter().find(|r| r.kind.habitable()).or(st.rooms.first()).unwrap().number.clone()
}

#[test]
fn a_hundred_regenerations_keep_every_lock() {
    let prev = json!({"typology": "office", "floors": 2});
    let (s, p) = built(&prev);
    let room = first_room(&p, 1);
    let locks = vec![
        Lock::take(Target::Storey(0), Keep::Geometry, &prev, &s, &p).unwrap(),
        Lock::take(Target::Room { storey: 1, number: room }, Keep::Geometry, &prev, &s, &p).unwrap(),
    ];
    let grids = [6.0, 7.2, 8.4];
    let doors = [0.8, 0.9, 1.0];
    let facades = ["punched", "ribbon", "curtain", "panels", "fins"];
    for i in 0..100usize {
        let mut next = prev.clone();
        next["rules"] = json!({
            "grid.span": grids[i % 3],
            "room.door": doors[(i / 3) % 3],
            "facade.system": facades[(i / 9) % 5],
        });
        let r = regenerate(&prev, &next, &locks).unwrap();
        for l in &locks {
            assert!(l.holds(&r.brief, &r.spec, &r.project), "run {i}: {} broken", l.label());
        }
        assert!(r.refused.iter().all(|x| !x.because.is_empty()), "run {i}");
        assert!(r.applied.len() + r.refused.len() <= 3, "run {i}");
        // a regeneration that changed something must have said what it kept back
        if r.brief["rules"] != next["rules"] {
            assert!(!r.refused.is_empty(), "run {i}: changes dropped without a report");
        }
    }
}

#[test]
fn a_locked_rule_keeps_its_value_and_says_so() {
    let prev = json!({"typology": "office", "floors": 2});
    let (s, p) = built(&prev);
    let lock = Lock::take(Target::Rule("grid.span".into()), Keep::Geometry, &prev, &s, &p).unwrap();
    let next = json!({"typology": "office", "floors": 2, "rules": {"grid.span": 6.0, "room.door": 1.0}});
    let r = regenerate(&prev, &next, &[lock.clone()]).unwrap();
    assert_eq!(r.spec.grid, s.grid);
    let refused: Vec<String> = r.refused.iter().map(|x| x.path.join("/")).collect();
    assert_eq!(refused, vec!["rules/grid.span"]);
    assert_eq!(r.refused[0].because, vec![lock.label()]);
    assert_eq!(r.brief["rules"]["room.door"], 1.0);
}

#[test]
fn a_program_lock_lets_geometry_free_changes_through() {
    let prev = json!({"typology": "office", "floors": 2});
    let (s, p) = built(&prev);
    let lock = Lock::take(Target::Storey(1), Keep::Program, &prev, &s, &p).unwrap();
    let next = json!({"typology": "office", "floors": 2, "rules": {"room.door": 1.0}});
    let r = regenerate(&prev, &next, &[lock]).unwrap();
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert!(r.applied.iter().any(|x| x.join("/") == "rules/room.door"), "{:?}", r.applied);
}

#[test]
fn locks_ride_in_the_brief_and_a_foreign_seal_is_an_error() {
    let prev = json!({"typology": "office", "floors": 2});
    let (s, p) = built(&prev);
    let zone = vec![(-1000.0, -1000.0), (1000.0, -1000.0), (1000.0, 1000.0), (-1000.0, 1000.0)];
    let locks = vec![
        Lock::take(Target::Storey(0), Keep::Geometry, &prev, &s, &p).unwrap(),
        Lock::take(Target::Zone { storey: 1, polygon: zone }, Keep::Program, &prev, &s, &p).unwrap(),
    ];
    let r = regenerate(&prev, &prev, &locks).unwrap();
    assert_eq!(of_brief(&r.brief).unwrap(), locks);
    assert!(parse_spec(&r.brief).is_ok());
    let mut bad = locks[0].clone();
    bad.seal = json!("0000000000000000");
    let e = regenerate(&prev, &prev, &[bad]).unwrap_err();
    assert!(e.contains("do not hold"), "{e}");
    assert!(Lock::take(Target::Storey(9), Keep::Geometry, &prev, &s, &p).is_err());
}
