// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The foundation a spec asks for: a raft with the footprint's area, strips under the bearing
//! walls, keys in the contract grammar, and errors that name the field.

use ddc_gen::project::{build_project, check_project, parse_spec, plan_foundation, FoundationSpec, GProject, ProjectSpec, Typology};
use serde_json::{json, Value as J};
use std::collections::BTreeSet;

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    assert!(check_project(&s, &p).passed());
    (s, p)
}

fn key_ok(k: &str) -> bool {
    let mut c = k.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric() || f == '_' || f == '$') && c.all(|x| x.is_ascii_alphanumeric() || "._#/:-$".contains(x)) && k.len() <= 255
}

#[test]
fn no_foundation_is_asked_so_none_is_planned() {
    for t in Typology::NAMES {
        let (s, p) = built(json!({"typology": t}));
        assert!(s.foundation.is_none());
        assert!(plan_foundation(&s, &p, 1).is_none(), "{t}");
    }
}

#[test]
fn a_raft_has_the_footprint_area_and_one_key() {
    let (s, p) = built(json!({"typology": "apartment_section", "floors": 5, "footprint": {"width": 34.0, "depth": 12.0}, "foundation": "raft"}));
    assert_eq!(s.foundation, Some(FoundationSpec::Raft { thickness: 0.4 }));
    let f = plan_foundation(&s, &p, 1).unwrap();
    assert!((f.raft_area() - 34.0 * 12.0).abs() < 1e-6, "{}", f.raft_area());
    assert_eq!(f.key, "b1/found/raft");
    assert!(key_ok(&f.key));
    assert!(f.strips.is_empty());
    // the raft follows the origin the building is moved to
    let (s2, p2) = built(json!({"typology": "apartment_section", "floors": 5, "footprint": {"width": 34.0, "depth": 12.0}, "foundation": "raft", "origin": [10.0, 20.0]}));
    let f2 = plan_foundation(&s2, &p2, 1).unwrap();
    assert!((f2.raft_area() - f.raft_area()).abs() < 1e-6);
    assert_ne!(f2.ring, f.ring);
}

#[test]
fn raft_thickness_comes_from_the_spec_or_the_default() {
    let (s, _) = built(json!({"typology": "house", "foundation": {"kind": "raft", "thickness": 0.6}}));
    assert_eq!(s.foundation, Some(FoundationSpec::Raft { thickness: 0.6 }));
    let (s, _) = built(json!({"typology": "house", "foundation": {"kind": "raft"}}));
    assert_eq!(s.foundation, Some(FoundationSpec::Raft { thickness: 0.4 }));
}

#[test]
fn strips_run_under_the_bearing_walls_with_unique_keys() {
    let (s, p) = built(json!({"typology": "apartment_section", "floors": 3, "foundation": "strip"}));
    let f = plan_foundation(&s, &p, 1).unwrap();
    assert!(f.strips.len() >= 4, "{}", f.strips.len());
    let mut seen = BTreeSet::new();
    for st in &f.strips {
        assert!(key_ok(&st.key), "{}", st.key);
        assert!(st.key.starts_with("b1/found/strip/"));
        assert!(seen.insert(st.key.clone()), "{} twice", st.key);
        assert!(st.wall_key.starts_with("b1/s01/"));
        assert_ne!(st.a, st.b);
    }
    // a wall that is only a partition gets none
    let walls = p.storeys[0].layout.walls.len();
    assert!(f.strips.len() < walls);
}

#[test]
fn the_same_spec_gives_the_same_foundation() {
    let v = json!({"typology": "office", "floors": 4, "foundation": "strip"});
    let (s1, p1) = built(v.clone());
    let (s2, p2) = built(v);
    assert_eq!(plan_foundation(&s1, &p1, 1), plan_foundation(&s2, &p2, 1));
}

#[test]
fn a_bad_foundation_names_the_field_and_the_choices() {
    for (v, what) in [
        (json!("piles"), "unknown 'piles', one of raft, strip"),
        (json!(5), "must be raft, strip"),
        (json!({"kind": "raft", "thickness": 9.0}), "foundation.thickness"),
        (json!({"kind": "raft", "colour": "red"}), "colour"),
        (json!({"kind": "strip", "width": 0.01}), "foundation.width"),
    ] {
        let e = parse_spec(&json!({"typology": "house", "foundation": v})).unwrap_err().to_string();
        assert!(e.contains(what), "{v}: {e}");
    }
}
