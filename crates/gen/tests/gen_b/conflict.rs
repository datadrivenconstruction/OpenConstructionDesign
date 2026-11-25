// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Twenty briefs that cannot be built: the conflict found is minimal (drop any member and the
//! brief builds), keeps out demands that play no part, and comes with relaxations in range.

use ddc_gen::project::conflict::{demands, feasible, find, find_failed, restrict, Demand};
use ddc_gen::project::locks::{Keep, Lock, Target};
use ddc_gen::project::{build_project, parse_spec};
use serde_json::{json, Value as J};

const TYPES: [&str; 4] = ["office", "hotel", "clinic", "residential"];

/// Goals no building can meet, made hard.
const GOALS: [(&str, f64); 4] =
    [("goal.circulation", 0.05), ("goal.efficiency", 0.98), ("goal.daylight", 0.5), ("goal.light_depth", 3.0)];

fn cases() -> Vec<(String, J, Vec<String>)> {
    let mut out = vec![];
    for t in TYPES {
        for (g, x) in GOALS {
            let b = json!({"typology": t, "floors": 2,
                           "rules": {g: {"value": x, "hard": true}, "room.door": 1.0}});
            out.push((format!("{t} {g}"), b, vec![format!("rule {g}")]));
        }
    }
    for t in TYPES {
        let prev = json!({"typology": t, "floors": 2});
        let s = parse_spec(&prev).unwrap();
        let p = build_project(&s).unwrap();
        let lock = Lock::take(Target::Rule("grid.span".into()), Keep::Geometry, &prev, &s, &p).unwrap();
        let other = if s.grid == 6.0 { 8.4 } else { 6.0 };
        let b = json!({"typology": t, "floors": 2, "rules": {"grid.span": other, "room.door": 1.0},
                       "locks": [lock.to_json()]});
        out.push((format!("{t} lock"), b, vec!["rule grid.span".into(), "lock on rule grid.span".into()]));
    }
    out
}

/// The brief with a fix written in, a goal keeping its hardness.
fn with_fix(b: &J, id: &str, x: &J) -> J {
    let mut b = b.clone();
    if b["rules"][id].is_object() {
        b["rules"][id]["value"] = x.clone();
    } else {
        b["rules"][id] = x.clone();
    }
    b
}

#[test]
fn every_conflict_is_minimal_and_relaxable() {
    let all = cases();
    assert_eq!(all.len(), 20);
    let mut relaxed = 0;
    for (name, b, want) in &all {
        assert!(feasible(b).is_err(), "{name}: should not build");
        let c = find(b).unwrap_or_else(|| panic!("{name}: no conflict"));
        let mut got = c.labels.clone();
        got.sort();
        let mut want = want.clone();
        want.sort();
        assert_eq!(got, want, "{name}");
        let every = demands(b);
        assert!(feasible(&restrict(b, &every, &c.set)).is_err(), "{name}: the set alone must fail");
        for d in &c.set {
            let rest: Vec<Demand> = c.set.iter().filter(|x| *x != d).cloned().collect();
            assert!(feasible(&restrict(b, &every, &rest)).is_ok(), "{name}: {} is not needed", d.label(b));
        }
        for f in &c.fixes {
            let d = ddc_gen::project::rules::def(&f.rule).unwrap();
            assert!(d.parse_value(&f.value).is_some(), "{name}: {} out of range", f.rule);
            assert!(feasible(&with_fix(b, &f.rule, &f.value)).is_ok(), "{name}: {} = {} does not resolve", f.rule, f.value);
        }
        if !c.fixes.is_empty() {
            relaxed += 1;
        }
        assert!(c.builds <= 60, "{name}: {} builds", c.builds);
        assert!(c.to_json()["conflict"].is_array());
    }
    assert!(relaxed >= 18, "{relaxed} of 20 relaxed");
}

/// The app hands the search the failure its own build met: the conflict is the one the search
/// finds when it builds the brief itself.
#[test]
fn a_known_failure_finds_the_same_conflict() {
    let (name, b, _) = &cases()[0];
    let failure = feasible(b).unwrap_err();
    let told = find_failed(b, failure);
    let found = find(b).unwrap();
    assert_eq!(told.to_json(), found.to_json(), "{name}");
    assert_eq!(told.builds, found.builds, "{name}");
}

#[test]
fn a_feasible_brief_has_no_conflict() {
    assert!(find(&json!({"typology": "office", "floors": 2, "rules": {"room.door": 1.0}})).is_none());
}

#[test]
fn demands_cover_rules_fields_rows_and_locks() {
    let b = json!({"typology": "house", "floors": 2, "grid": 6.0, "rules": {"room.door": 1.0},
                   "rooms": [{"kind": "bedroom", "area": 12}, {"kind": "kitchen", "area": 10}],
                   "locks": [{"rule": "grid.span", "seal": 6.0}]});
    let d = demands(&b);
    let labels: Vec<String> = d.iter().map(|x| x.label(&b)).collect();
    for want in ["rule room.door", "rule grid.span", "rooms[0] bedroom", "rooms[1] kitchen", "field floors", "lock on rule grid.span"] {
        assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
    }
    let only_rooms: Vec<Demand> = d.iter().filter(|x| matches!(x, Demand::Row { .. })).cloned().collect();
    let r = restrict(&b, &d, &only_rooms[1..]);
    assert_eq!(r["rooms"], json!([{"kind": "kitchen", "area": 10}]));
    assert!(r["grid"].is_null() && r["rules"].is_null() && r["floors"].is_null());
    assert_eq!(r["locks"], json!([]));
}
