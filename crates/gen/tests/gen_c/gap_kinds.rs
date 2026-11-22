// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Room kinds the brief names beyond the built-in ones (foyer, gallery, atrium, lobby) and
//! the apartment limit of a floor, both driven by rules; plus the windowless kitchen check.

use ddc_gen::project::kinds;
use ddc_gen::project::{build_project, check_project, parse_spec, RoomKind};
use serde_json::{json, Value};

fn office(ground: Value, rooms: Value) -> Value {
    json!({"typology": "office", "floors": 3, "footprint": {"width": 40.0, "depth": 14.0},
           "ground_rooms": ground, "rooms": rooms})
}

/// A Russian sample from the test data file.
fn ru_sample(key: &str) -> String {
    let m: Value = serde_json::from_str(include_str!("../data/ru_samples.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
}

fn typical() -> Value {
    json!([{"kind": "office", "area": 20.0, "count": 6}, {"kind": "wc", "area": 6.0}])
}

fn err(v: &Value) -> String {
    parse_spec(v).err().map(|e| e.to_string()).unwrap_or_default()
}

#[test]
fn room_words_resolve_through_data_in_any_language() {
    for (w, id) in [
        ("foyer".to_string(), "foyer"),
        (ru_sample("foyer_cap"), "foyer"),
        ("gallery".to_string(), "gallery"),
        (ru_sample("gallery"), "gallery"),
        ("atrium".to_string(), "atrium"),
        (ru_sample("atrium"), "atrium"),
        ("vestibule".to_string(), "lobby"),
        (ru_sample("lobby"), "lobby"),
    ] {
        let w = w.as_str();
        let (base, def) = kinds::parse(w).unwrap_or_else(|| panic!("{w}"));
        assert_eq!(def.map(|d| d.id.as_str()), Some(id), "{w}");
        assert_eq!(base, RoomKind::Lobby, "{w}");
    }
    // a built-in kind keeps working with no data entry
    assert_eq!(kinds::parse("office"), Some((RoomKind::Office, None)));
    assert!(kinds::parse("nonsense").is_none());
    for d in kinds::defs() {
        if let Some(r) = &d.min_area_rule {
            assert!(ddc_gen::project::rules::def(r).is_some(), "{} names rule {r}", d.id);
        }
    }
}

#[test]
fn a_foyer_gallery_and_atrium_are_read_named_and_built_as_circulation() {
    let ground = json!([{"kind": "foyer", "area": 36.0}, {"kind": "gallery", "area": 30.0},
        {"kind": "atrium", "area": 90.0}, {"kind": "office", "area": 20.0, "count": 4}]);
    let s = parse_spec(&office(ground, typical())).unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<(&str, RoomKind)> = s.ground_rooms.iter().map(|r| (r.name.as_str(), r.kind)).collect();
    assert_eq!(
        &names[..3],
        &[("Foyer", RoomKind::Lobby), ("Gallery", RoomKind::Lobby), ("Atrium", RoomKind::Lobby)]
    );
    let ru = json!({"typology": "office", "floors": 1, "lang": "ru", "footprint": {"width": 30.0, "depth": 14.0},
        "rooms": [{"kind": ru_sample("foyer"), "area": 24.0}, {"kind": "office", "area": 20.0, "count": 4}]});
    let s = parse_spec(&ru).unwrap_or_else(|e| panic!("{e}"));
    assert!(s.rooms.iter().any(|r| r.name == ru_sample("foyer_cap")));
}

#[test]
fn a_room_below_its_kind_rule_is_refused_and_the_rule_moves_the_limit() {
    let ground = |a: f64| json!([{"kind": "foyer", "area": a}, {"kind": "office", "area": 20.0, "count": 4}]);
    let e = err(&office(ground(8.0), typical()));
    assert!(e.contains("room.min_area_foyer") && e.contains("ground_rooms[0].area"), "{e}");
    let mut v = office(ground(8.0), typical());
    v["rules"] = json!({"room.min_area_foyer": 6.0});
    assert!(parse_spec(&v).is_ok(), "{}", err(&v));
    for (kind, rule, area) in [("gallery", "room.min_area_gallery", 10.0), ("atrium", "room.min_area_atrium", 30.0)] {
        let v = office(json!([{"kind": kind, "area": area}, {"kind": "office", "area": 20.0, "count": 4}]), typical());
        assert!(err(&v).contains(rule), "{kind}: {}", err(&v));
    }
}

#[test]
fn a_ground_only_room_on_the_typical_floors_and_inside_a_flat_is_refused() {
    let rooms = json!([{"kind": "foyer", "area": 30.0}, {"kind": "office", "area": 20.0, "count": 4}]);
    let e = err(&office(json!([{"kind": "office", "area": 20.0, "count": 4}]), rooms));
    assert!(e.contains("ground_rooms") && e.contains("foyer"), "{e}");
    let flat = json!({"name": "1", "count": 1, "rooms": [{"kind": "living", "area": 20.0},
        {"kind": "foyer", "area": 12.0}, {"kind": "kitchen", "area": 10.0}]});
    let v = json!({"typology": "apartment_section", "floors": 3, "apartments": [flat]});
    assert!(err(&v).contains("not of an apartment"), "{}", err(&v));
}

#[test]
fn a_misspelt_room_word_suggests_the_data_kind() {
    let v = office(json!([{"kind": "foier", "area": 30.0}]), typical());
    assert!(err(&v).contains("foyer"), "{}", err(&v));
}

fn flats(count: u32, width: Option<f64>, rules: Value) -> Value {
    let flat = json!({"name": "1", "count": count, "rooms": [{"kind": "living", "area": 18.0},
        {"kind": "bedroom", "area": 13.0}, {"kind": "kitchen", "area": 10.0}, {"kind": "bath", "area": 4.4}]});
    let mut v = json!({"typology": "apartment_section", "floors": 3, "apartments": [flat], "rules": rules});
    if let Some(w) = width {
        v["footprint"] = json!({"width": w, "depth": 12.0});
    }
    v
}

#[test]
fn apartments_per_floor_follow_the_footprint_width_and_the_frontage_rule() {
    // both sides of the corridor, 5 m of facade each: width 40 holds 16, width 20 holds 8
    let sixteen = flats(16, Some(40.0), json!({}));
    assert!(parse_spec(&sixteen).is_ok(), "{}", err(&sixteen));
    let e = err(&flats(17, Some(40.0), json!({})));
    assert!(e.contains("at most 16") && e.contains("room.apartment_frontage"), "{e}");
    assert!(parse_spec(&flats(8, Some(20.0), json!({}))).is_ok());
    assert!(err(&flats(9, Some(20.0), json!({}))).contains("at most 8"));
    // a wider facade per apartment lowers the limit: the rule is the lever, not a constant
    let e = err(&flats(9, Some(40.0), json!({"room.apartment_frontage": 10.0})));
    assert!(e.contains("at most 8"), "{e}");
    assert!(parse_spec(&flats(8, Some(40.0), json!({"room.apartment_frontage": 10.0}))).is_ok());
}

#[test]
fn without_a_width_the_rule_is_the_limit_and_a_set_value_also_caps_a_derived_one() {
    assert!(parse_spec(&flats(12, None, json!({}))).is_ok());
    assert!(err(&flats(13, None, json!({}))).contains("room.apartments_max"));
    assert!(parse_spec(&flats(20, None, json!({"room.apartments_max": 20}))).is_ok());
    // width 40 would hold 16; the brief's own limit of 10 wins
    assert!(err(&flats(11, Some(40.0), json!({"room.apartments_max": 10}))).contains("at most 10"));
}

/// An apartment section with an 8 m2 kitchen, whose windows the test strips.
fn kitchen_spec(rules: Value) -> Value {
    let two = json!({"name": "2", "count": 2, "rooms": [{"kind": "living", "area": 20.0}, {"kind": "bedroom", "area": 14.5},
        {"kind": "kitchen", "area": 8.0}, {"kind": "bath", "area": 4.9}, {"kind": "hall", "area": 3.0}]});
    json!({"typology": "apartment_section", "floors": 3, "floor_height": 3.0, "lift": true,
           "footprint": {"width": 34.0, "depth": 12.0}, "apartments": [two], "rules": rules})
}

fn on_edge(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = poly.len();
    (0..n).any(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
        (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy) < 1e-6
    })
}

#[test]
fn a_kitchen_without_a_facade_window_fails_unless_it_is_a_lit_niche() {
    for (rules, niche_allowed) in [(json!({}), true), (json!({"light.kitchen_niche_max": 0.0}), false)] {
        let spec = parse_spec(&kitchen_spec(rules)).unwrap_or_else(|e| panic!("{e}"));
        let mut p = build_project(&spec).unwrap_or_else(|e| panic!("{e:?}"));
        let before = check_project(&spec, &p);
        assert!(before.passed(), "{:?}", before.failures);
        let (mut adjacent, mut stripped) = (false, 0);
        for st in &mut p.storeys {
            let l = &mut st.layout;
            let kitchens: Vec<usize> = (0..st.rooms.len()).filter(|&i| st.rooms[i].kind == RoomKind::Kitchen).collect();
            let centers: Vec<(f64, f64)> = l.openings.iter().map(|o| l.walls[o.wall].at(o.along + o.width / 2.0)).collect();
            for &k in &kitchens {
                for (i, o) in l.openings.iter().enumerate() {
                    if o.door && on_edge(&l.rooms[k].polygon, centers[i]) {
                        adjacent |= (0..st.rooms.len()).any(|j| {
                            st.rooms[j].kind == RoomKind::Living
                                && st.rooms[j].owner == st.rooms[k].owner
                                && on_edge(&l.rooms[j].polygon, centers[i])
                        });
                    }
                }
            }
            let keep: Vec<bool> = l
                .openings
                .iter()
                .enumerate()
                .map(|(i, o)| o.door || !kitchens.iter().any(|&k| on_edge(&l.rooms[k].polygon, centers[i])))
                .collect();
            stripped += keep.iter().filter(|k| !**k).count();
            let mut it = keep.iter();
            l.openings.retain(|_| *it.next().unwrap());
        }
        assert!(stripped > 0, "no kitchen window to strip");
        let after = check_project(&spec, &p);
        let borrowed = after.warnings.iter().any(|w| w.contains("light.kitchen_niche_max"));
        let refused = after.failures.iter().any(|f| f.contains("kitchen without a facade window"));
        if niche_allowed && adjacent {
            assert!(borrowed && !refused, "{:?} {:?}", after.failures, after.warnings);
        } else {
            assert!(refused && !borrowed, "niche_allowed {niche_allowed} adjacent {adjacent}: {:?}", after.failures);
        }
    }
}
