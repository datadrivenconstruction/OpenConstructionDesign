// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The vertical skeleton: wet rooms stand over wet rooms, a storey under a wet storey carries
//! the stack, and shafts and columns stand on their like below, on bars and stepped blocks.

use ddc_gen::project::nav::check;
use ddc_gen::project::rules::Layer;
use ddc_gen::project::stack::{is_wet, skeleton};
use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, check_project, parse_spec, GProject, ProjectSpec, RoomKind};
use serde_json::{json, Value as J};
use suite::wet_stacked as stacked;

// the benchmark's own metric definitions, measured on the built model
#[path = "../bench/suite.rs"]
mod suite;

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    (s, p)
}

fn clinic() -> J {
    json!({"typology": "office", "floors": 2,
           "rooms": [{"kind": "office", "area": 16, "count": 8}, {"kind": "wc", "area": 4, "count": 2}],
           "ground_rooms": [{"kind": "lobby", "area": 40}, {"kind": "office", "area": 16, "count": 4}]})
}

fn parking() -> J {
    json!({"typology": "office", "floors": 3, "footprint": {"width": 40, "depth": 30},
           "rooms": [{"kind": "technical", "area": 20}]})
}

/// The multi-storey briefs of the evaluation set.
fn bars() -> Vec<J> {
    let home = json!([{"kind": "living", "area": 25}, {"kind": "kitchen", "area": 12},
        {"kind": "bedroom", "area": 14, "count": 3}, {"kind": "bath", "area": 5}, {"kind": "wc", "area": 2}]);
    vec![
        json!({"typology": "house", "floors": 2, "rooms": home}),
        json!({"typology": "house", "floors": 3, "footprint": {"width": 7, "depth": 12}, "rooms": home}),
        json!({"typology": "apartment_section", "floors": 9}),
        json!({"typology": "office", "floors": 4, "lang": "ru",
               "rooms": [{"kind": "office", "area": 30, "count": 4}, {"kind": "meeting", "area": 40},
                         {"kind": "wc", "area": 4, "count": 2}],
               "ground_rooms": [{"kind": "lobby", "area": 60}, {"kind": "office", "area": 30, "count": 3},
                                {"kind": "wc", "area": 4, "count": 2}]}),
        json!({"typology": "school", "floors": 3, "lang": "ru"}),
        clinic(),
        json!({"typology": "apartment_section", "floors": 5,
               "apartments": [{"name": "room", "count": 4,
                               "rooms": [{"kind": "bedroom", "area": 18}, {"kind": "bath", "area": 4}]}]}),
        json!({"typology": "office", "floors": 5,
               "ground_rooms": [{"kind": "lobby", "area": 50}, {"kind": "office", "area": 40, "count": 3}],
               "rooms": [{"kind": "office", "area": 25, "count": 4}, {"kind": "wc", "area": 4}]}),
        json!({"typology": "office", "floors": 9, "lift": true}),
        parking(),
    ]
}

/// Stepped blocks: a podium under a tower, a setback, a cantilever.
fn stepped() -> Vec<J> {
    vec![
        json!({"typology": "office", "floors": 10, "design": {"massing": "tower_podium", "setback": 3}}),
        json!({"typology": "office", "floors": 5, "design": {"setback": 2}}),
        json!({"typology": "office", "floors": 5, "design": {"cantilever": 2}}),
    ]
}

#[test]
fn a_storey_under_a_wet_storey_takes_the_stack() {
    let (s, p) = built(&clinic());
    let sk = skeleton(&s);
    assert_eq!(sk.filled, vec![(0, 1)]);
    let wc: u32 = sk.storeys[0].iter().filter(|r| r.kind == RoomKind::Wc).map(|r| r.count).sum();
    assert_eq!(wc, 2);
    // the brief itself is untouched: the stack is the skeleton's
    assert!(s.ground_rooms.iter().all(|r| !is_wet(r.kind)));
    assert_eq!(stacked(&p), (2, 2));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    assert!(!g.warnings.iter().any(|w| w.contains("mep.riser_offset")), "{:?}", g.warnings);
    let why = explain_project(&s, &p);
    assert!(why.iter().any(|w| w.rule == "mep.wet_stack_fill" && w.target == "storey:0"), "{why:?}");
}

#[test]
fn without_the_fill_the_finding_names_the_rule_and_its_fix() {
    let mut v = clinic();
    v["rules"] = json!({"mep.wet_stack_fill": false});
    let (s, p) = built(&v);
    assert!(skeleton(&s).filled.is_empty());
    assert_eq!(stacked(&p), (0, 2));
    let f = check(&s, &p);
    let r = f.iter().find(|f| f.rule == "mep.riser_offset").expect("a riser finding");
    assert_eq!(r.storey, Some(1));
    assert!(r.line().starts_with("floor 2: mep.riser_offset: 2 of 2"), "{}", r.line());
    assert_eq!(r.fix.as_ref().map(|x| x.rule.as_str()), Some("mep.wet_stack_fill"));
    assert!(!r.hard);
    // a storey with wet rooms of its own keeps its programme
    let (s, _) = built(&bars()[3]);
    assert!(skeleton(&s).filled.is_empty());
}

#[test]
fn wet_rooms_stack_and_cores_hold_on_every_brief() {
    let (mut ok, mut all) = (0, 0);
    for v in bars() {
        let (s, p) = built(&v);
        let (o, a) = stacked(&p);
        assert_eq!(o, a, "{v}: {o} of {a} wet rooms on the stack");
        let f = check(&s, &p);
        for rule in ["mep.riser_offset", "core.stack", "grid.transfer"] {
            assert!(!f.iter().any(|x| x.rule == rule), "{v}: {f:?}");
        }
        assert!(check_project(&s, &p).passed(), "{v}");
        ok += o;
        all += a;
    }
    for v in stepped() {
        let (_, p) = built(&v);
        let (o, a) = stacked(&p);
        eprintln!("{v}: {o} of {a} wet rooms on the stack");
        ok += o;
        all += a;
    }
    let score = 100.0 * ok as f64 / all.max(1) as f64;
    assert!(score >= 80.0, "wet stacking {score:.1} ({ok} of {all})");
}

#[test]
fn the_benchmark_briefs_stack_and_pass_their_checks() {
    let (mut ok, mut all) = (0, 0);
    for b in suite::load_briefs() {
        let Ok(s) = parse_spec(&b.spec) else { continue };
        let Ok(p) = build_project(&s) else { continue };
        let (o, a) = stacked(&p);
        ok += o;
        all += a;
        // the plan checks are advice on the defaults: they never fail a brief
        let g = check_project(&s, &p);
        let f = check(&s, &p);
        let checked: Vec<&String> = g.failures.iter().filter(|x| f.iter().any(|f| x.contains(f.rule))).collect();
        assert!(checked.is_empty(), "{}: {checked:?}", b.name);
    }
    let score = 100.0 * ok as f64 / all.max(1) as f64;
    eprintln!("bench wet stacking {score:.1} ({ok} of {all})");
    assert!(score >= 80.0, "bench wet stacking {score:.1} ({ok} of {all})");
}

#[test]
fn a_moved_shaft_or_column_breaks_the_stack() {
    let (s, mut p) = built(&json!({"typology": "office", "floors": 3, "lift": true}));
    assert!(!check(&s, &p).iter().any(|f| f.rule == "core.stack"));
    let top = &mut p.storeys[2].layout;
    assert!(!top.shafts.is_empty());
    top.shafts[0].ring.iter_mut().for_each(|q| q.0 += 1.0);
    let f = check(&s, &p);
    let shaft = f.iter().find(|f| f.rule == "core.stack").expect("a shaft finding");
    assert_eq!(shaft.storey, Some(2));
    assert!(shaft.line().contains("core.stack"), "{}", shaft.line());
    // a deep plan stands columns in its bay
    let (s, mut p) = built(&parking());
    assert!(!check(&s, &p).iter().any(|f| f.rule == "grid.transfer"));
    let top = &mut p.storeys[2].layout;
    assert!(!top.columns.is_empty());
    top.columns[0].at.0 += 1.0;
    let f = check(&s, &p);
    let col = f.iter().find(|f| f.rule == "grid.transfer").expect("a column finding");
    assert!(col.msg.starts_with("1 of"), "{}", col.msg);
    // a transfer beam the rules allow is no finding
    let mut s2 = s.clone();
    s2.rules.set(Layer::Project, "grid.transfer", 1.0).unwrap();
    assert!(!check(&s2, &p).iter().any(|f| f.rule == "grid.transfer"));
}
