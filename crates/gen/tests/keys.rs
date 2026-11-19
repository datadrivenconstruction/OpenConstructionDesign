// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stable element keys (card E1): the same brief gives the same keys, and a change in one
//! place leaves the keys elsewhere alone.

use ddc_gen::project::{build_project, merge_patch, parse_spec, project_keys, ProjectKeys};
use serde_json::{json, Value as J};
use std::collections::BTreeSet;

fn keys(v: &J) -> ProjectKeys {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    project_keys(&p, 1)
}

fn set(k: &ProjectKeys) -> BTreeSet<String> {
    k.all().into_iter().map(String::from).collect()
}

fn retained(old: &BTreeSet<String>, new: &BTreeSet<String>) -> f64 {
    old.intersection(new).count() as f64 / old.len() as f64
}

fn grammar(k: &str) -> bool {
    let mut c = k.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric())
        && c.all(|x| x.is_ascii_alphanumeric() || "._#/:$-".contains(x))
        && k.len() <= 255
}

#[test]
fn keys_are_unique_ascii_and_cover_every_element() {
    for t in ["house", "apartment_section", "office", "school"] {
        for lang in ["en", "ru"] {
            let v = json!({"typology": t, "lang": lang, "floors": 3});
            let s = parse_spec(&v).unwrap();
            let p = build_project(&s).unwrap();
            let k = project_keys(&p, 1);
            let all = k.all();
            let n: usize = p
                .storeys
                .iter()
                .map(|s| {
                    let l = &s.layout;
                    l.walls.len()
                        + l.openings.len()
                        + l.rooms.len()
                        + usize::from(l.slab.len() >= 3)
                        + l.stairs.len()
                        + l.shafts.len()
                        + l.grids.len()
                        + l.columns.len()
                        + l.fixtures.len()
                })
                .sum::<usize>()
                + 1;
            assert_eq!(all.len(), n, "{t} {lang}");
            assert_eq!(set(&k).len(), n, "{t} {lang}: keys are unique");
            assert!(all.iter().all(|x| grammar(x)), "{t} {lang}: {:?}", all.iter().find(|x| !grammar(x)));
            assert!(
                !all.iter().any(|x| x.contains(".dup")),
                "{t} {lang}: no key needed the collision suffix: {:?}",
                all.iter().find(|x| x.contains(".dup"))
            );
        }
    }
}

#[test]
fn same_spec_twice_gives_the_same_keys() {
    for t in ["house", "apartment_section", "office", "school"] {
        let v = json!({"typology": t, "floors": 2});
        let (a, b) = (keys(&v), keys(&v));
        assert_eq!(a, b, "{t}");
        // keys do not depend on the language of the names
        let ru = keys(&json!({"typology": t, "floors": 2, "lang": "ru"}));
        let (x, y) = (set(&a), set(&ru));
        let r = retained(&x, &y);
        assert!(r > 0.99, "{t}: language changed keys, retained {r}");
    }
}

#[test]
fn one_more_floor_keeps_every_key_of_the_floors_below() {
    for (t, from) in [("apartment_section", 5), ("office", 5), ("school", 3), ("house", 2)] {
        let v = json!({"typology": t, "floors": from});
        let old = keys(&v);
        let mut w = v;
        merge_patch(&mut w, &json!({"floors": from + 1}));
        let new = set(&keys(&w));
        // the old top floor may gain its stair; every key it had stays
        let below: BTreeSet<String> = set(&old);
        let r = retained(&below, &new);
        eprintln!("keys M1 {t} floors {from}->{}: retained {:.4} of {}", from + 1, r, below.len());
        assert!((r - 1.0).abs() < 1e-12, "{t}: lost {:?}", below.difference(&new).take(5).collect::<Vec<_>>());
    }
}

/// The gate case of card E1: one unit mix changed on one floor keeps 95 % of all keys.
#[test]
fn one_floor_changed_keeps_95_percent_of_all_keys() {
    let rooms = json!([
        {"kind": "office", "area": 30, "count": 5},
        {"kind": "meeting", "area": 45},
        {"kind": "wc", "area": 4, "count": 3},
        {"kind": "storage", "area": 6}
    ]);
    let ground = |offices: u32, wc: u32| {
        json!([
            {"kind": "lobby", "area": 60},
            {"kind": "office", "area": 30, "count": offices},
            {"kind": "wc", "area": 4, "count": wc}
        ])
    };
    let base = json!({"typology": "office", "floors": 8, "rooms": rooms, "ground_rooms": ground(3, 2)});
    let old = set(&keys(&base));
    let (mut worst, mut gated) = (1.0f64, 0);
    for (o, w) in [(4, 2), (2, 2), (3, 3), (3, 1), (5, 2)] {
        let mut v = base.clone();
        merge_patch(&mut v, &json!({"ground_rooms": ground(o, w)}));
        let new = set(&keys(&v));
        let r = retained(&old, &new);
        eprintln!("keys M1 office 8 floors, ground mix offices {o} wc {w}: retained {r:.4} of {}", old.len());
        // asking for more ground rooms than the typical floor holds widens the whole building and
        // reflows every floor: that is a change of the footprint, reported but not part of the gate
        if footprint(&v) == footprint(&base) {
            worst = worst.min(r);
            gated += 1;
        }
    }
    assert!(gated >= 4, "the gate needs footprint-preserving cases, got {gated}");
    assert!(worst >= 0.95, "worst case retained {worst:.4}");
}

fn footprint(v: &J) -> (f64, f64) {
    build_project(&parse_spec(v).unwrap()).unwrap().footprint
}

/// Not a gate: a mix change that every typical floor shares changes every floor.
#[test]
fn apartment_mix_change_reports_retention() {
    let base = json!({"typology": "apartment_section", "floors": 6});
    let old = set(&keys(&base));
    let s = parse_spec(&base).unwrap();
    eprintln!("keys apartment_section 6 floors: {} keys, {} apartment kinds", old.len(), s.apartments.len());
    let mut v = base;
    merge_patch(&mut v, &json!({"floors": 6, "apartments": [{"name": "A", "count": 3,
        "rooms": [{"kind": "living", "area": 22}, {"kind": "bedroom", "area": 14},
                  {"kind": "kitchen", "area": 9}, {"kind": "bath", "area": 4}]}]}));
    let new = set(&keys(&v));
    eprintln!("keys apartment_section mix replaced: retained {:.4}", retained(&old, &new));
}

#[test]
fn large_projects_keep_keys_unique_and_in_grammar() {
    let rooms = json!([{"kind": "office", "area": 12, "count": 30}, {"kind": "wc", "area": 4, "count": 8}]);
    for v in [
        json!({"typology": "office", "floors": 11, "rooms": rooms, "ground_rooms": rooms}),
        json!({"typology": "apartment_section", "floors": 40}),
        json!({"typology": "school", "floors": 4, "lang": "ru"}),
    ] {
        let k = keys(&v);
        let all = k.all();
        assert_eq!(set(&k).len(), all.len(), "unique");
        assert!(all.iter().all(|x| grammar(x)));
        assert!(!all.iter().any(|x| x.contains(".dup")), "no collision suffix needed");
    }
}

#[test]
fn every_spec_language_gives_one_key_set() {
    assert!(parse_spec(&json!({"typology": "house", "lang": 1})).is_err());
    let en = set(&keys(&json!({"typology": "school", "floors": 2, "lang": "en"})));
    for lang in ["ru", "de", "ja", "ar", "pt_BR", "xx"] {
        let other = set(&keys(&json!({"typology": "school", "floors": 2, "lang": lang})));
        assert_eq!(en, other, "{lang}: names differ by language, keys do not");
    }
}

#[test]
fn two_buildings_never_share_a_key() {
    let s = parse_spec(&json!({"typology": "office", "floors": 3})).unwrap();
    let p = build_project(&s).unwrap();
    let (a, b) = (set(&project_keys(&p, 1)), set(&project_keys(&p, 2)));
    assert!(a.is_disjoint(&b));
    assert_eq!(a.len(), b.len());
}

#[test]
fn moving_the_plot_origin_keeps_every_key() {
    for t in ["office", "apartment_section", "house"] {
        let v = json!({"typology": t, "floors": 2});
        let mut w = v.clone();
        merge_patch(&mut w, &json!({"origin": [137.5, -42.0]}));
        assert_eq!(keys(&v), keys(&w), "{t}: origin must not change keys");
    }
}

#[test]
fn removing_a_floor_and_adding_it_back_restores_the_same_keys() {
    let v = json!({"typology": "office", "floors": 5});
    let mut w = v.clone();
    merge_patch(&mut w, &json!({"floors": 4}));
    let mut back = w.clone();
    merge_patch(&mut back, &json!({"floors": 5}));
    assert_eq!(keys(&v), keys(&back));
    // the four floors that remain are a subset of the five
    assert!(set(&keys(&w)).len() < set(&keys(&v)).len());
}

#[test]
fn reordering_the_asked_rooms_keeps_most_keys() {
    let a = json!([{"kind": "office", "area": 30, "count": 4}, {"kind": "meeting", "area": 45},
                   {"kind": "wc", "area": 4, "count": 2}, {"kind": "storage", "area": 6}]);
    let b = json!([{"kind": "storage", "area": 6}, {"kind": "wc", "area": 4, "count": 2},
                   {"kind": "meeting", "area": 45}, {"kind": "office", "area": 30, "count": 4}]);
    let ka = set(&keys(&json!({"typology": "office", "floors": 3, "rooms": a})));
    let kb = set(&keys(&json!({"typology": "office", "floors": 3, "rooms": b})));
    let r = retained(&ka, &kb);
    eprintln!("keys M1 asked rooms reordered: retained {r:.4} of {}", ka.len());
    assert_eq!(ka.len(), kb.len());
    assert!(r >= 0.95, "reordering the asked rooms moved keys: retained {r}");
}
