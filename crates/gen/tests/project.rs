// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project from a brief: every typology passes the gates, errors name the field and the fix,
//! the same spec gives the same project, and a 9-storey section builds within its budget.

use ddc_gen::project::{
    axis_letter, build_project, check_project, merge_patch, parse_spec, GProject, ProjectSpec,
    RoomKind, Typology,
};
use serde_json::{json, Value as J};
use std::time::Instant;

fn spec(v: J) -> ProjectSpec {
    parse_spec(&v).unwrap_or_else(|e| panic!("{e}"))
}

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = spec(v.clone());
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    (s, p)
}

fn count(p: &GProject, what: &str) -> usize {
    let s = spec(json!({"typology": "house"}));
    check_project(&s, p)
        .counts
        .iter()
        .find(|c| c.0 == what)
        .map_or(0, |c| c.1)
}

#[test]
fn every_typology_builds_with_defaults_in_both_languages() {
    for t in Typology::NAMES {
        for lang in ["en", "ru"] {
            let (s, p) = built(json!({"typology": t, "lang": lang}));
            assert_eq!(p.storeys.len(), s.floors as usize, "{t}");
            let g = check_project(&s, &p);
            eprintln!(
                "gen1 typology {t} {lang}: {:?} warnings {}",
                g.counts,
                g.warnings.len()
            );
            for st in &p.storeys {
                assert!(st.layout.rooms.iter().all(|r| !r.name.is_empty()));
                assert!(
                    st.stair_room.is_some(),
                    "{t}: every floor has its stair room"
                );
            }
        }
    }
}

#[test]
fn nine_storey_section_builds_within_two_seconds() {
    let t = Instant::now();
    let (s, p) = built(json!({"typology": "apartment_section", "floors": 9}));
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let g = check_project(&s, &p);
    eprintln!(
        "gen1 timing: apartment_section 9 floors built and gated in {ms:.1} ms; {:?}",
        g.counts
    );
    assert!(ms < 2000.0, "{ms} ms");
    assert_eq!(p.apartments, 36);
    assert_eq!(p.storeys[8].elevation, 24.0);
    assert!(
        p.storeys.iter().all(|s| s.layout.shafts.len() == 2),
        "a lift and the ventilation shaft on 9 floors"
    );
    // every floor carries a stair up, the top one on to the roof (`fire.stair_to_roof` from
    // `fire.tall_storeys`); with that rule off the top has none. Every floor but the ground a hole
    assert_eq!(count(&p, "stairs"), 9);
    let (_, off) = built(json!({"typology": "apartment_section", "floors": 9, "rules": {"fire.stair_to_roof": false}}));
    assert_eq!(count(&off, "stairs"), 8);
    assert!(p.storeys[0].layout.slab_holes.is_empty());
    assert!(p.storeys[1..]
        .iter()
        .all(|s| s.layout.slab_holes.len() == 1));
}

#[test]
fn rooms_take_the_asked_area_and_every_room_has_a_door() {
    let (s, p) = built(json!({
        "typology": "office",
        "floors": 3,
        "rooms": [
            {"kind": "office", "area": 30, "count": 5},
            {"kind": "meeting", "area": 45},
            {"kind": "wc", "area": 4, "count": 3},
            {"kind": "storage", "area": 6}
        ],
        "ground_rooms": [
            {"kind": "lobby", "area": 60},
            {"kind": "office", "area": 30, "count": 3},
            {"kind": "wc", "area": 4, "count": 2}
        ]
    }));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    let st = &p.storeys[1];
    let offices = st.rooms.iter().filter(|r| r.asked == Some(30.0)).count();
    assert_eq!(offices, 5);
    // the footprint is the same on every floor
    let w0 = p.storeys[0].layout.slab.clone();
    assert!(p.storeys.iter().all(|s| s.layout.slab == w0));
}

#[test]
fn same_spec_same_project_and_more_floors_keep_the_lower_ones() {
    let v = json!({"typology": "apartment_section", "floors": 9, "lang": "ru"});
    let (_, a) = built(v.clone());
    let (_, b) = built(v.clone());
    assert_eq!(a, b, "the builder is deterministic");
    let mut w = v;
    // 10 floors keep the top under `egress.two_stairs_above_height` (28 m); past it every
    // storey takes a second stair, so the lower floors rightly change
    merge_patch(&mut w, &json!({"floors": 10}));
    let (_, c) = built(w);
    assert_eq!(c.storeys.len(), 10);
    // floors 1-8 are unchanged, so their slots and therefore their references stay
    for k in 0..8 {
        assert_eq!(a.storeys[k], c.storeys[k], "floor {}", k + 1);
    }
}

#[test]
fn merge_patch_removes_with_null_and_merges_objects() {
    let mut v = json!({"typology": "house", "stair": {"width": 1.1}, "floors": 2});
    merge_patch(
        &mut v,
        &json!({"floors": null, "stair": {"max_riser": 0.17}}),
    );
    assert_eq!(
        v,
        json!({"typology": "house", "stair": {"width": 1.1, "max_riser": 0.17}})
    );
}

#[test]
fn errors_name_the_field_the_range_and_a_fix() {
    let e = parse_spec(&json!({"typology": "ofice"}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("did you mean 'office'"), "{e}");
    let e = parse_spec(&json!({
        "typology": "office",
        "flors": 3,
        "floor_height": 9,
        "rooms": [{"kind": "bathroom", "area": 5}],
        "apartments": []
    }))
    .unwrap_err();
    let text = e.to_string();
    assert_eq!(e.0.len(), 4, "{text}");
    assert!(
        text.contains("flors: unknown field (did you mean 'floors'?)"),
        "{text}"
    );
    assert!(text.contains("floor_height: 9 is outside 2.7..6"), "{text}");
    assert!(
        text.contains("rooms[0].kind: unknown kind 'bathroom'"),
        "{text}"
    );
    assert!(
        text.contains("apartments: only for typology apartment_section"),
        "{text}"
    );
    let e = parse_spec(&json!({"typology": "house", "floors": 5}))
        .unwrap_err()
        .to_string();
    assert!(e.contains("floors: must be a whole number in 1..3"), "{e}");
    let e = parse_spec(&json!({
        "typology": "apartment_section",
        "apartments": [{"rooms": [{"kind": "bath", "area": 4}]}]
    }))
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("needs at least one living room or bedroom"),
        "{e}"
    );
}

#[test]
fn a_footprint_too_narrow_says_how_wide_it_must_be() {
    let s = spec(json!({"typology": "school", "footprint": {"width": 20}}));
    let e = build_project(&s).unwrap_err().to_string();
    assert!(e.contains("footprint.width 20 m is too small"), "{e}");
    assert!(e.contains("the program needs"), "{e}");
    // a wide one fits, the spare going to rooms of the programme
    let (_, p) = built(json!({"typology": "school", "footprint": {"width": 90}}));
    assert!((p.footprint.0 - 90.0).abs() < 1e-9);
}

#[test]
fn origin_moves_everything_and_house_gets_a_pitched_roof() {
    let (_, a) = built(json!({"typology": "house"}));
    let (_, b) = built(json!({"typology": "house", "origin": [100, 50]}));
    assert_eq!(a.storeys.len(), 2);
    assert_eq!(a.roof.slope, 30.0);
    let (_, office) = built(json!({"typology": "office"}));
    // a flat roof still falls 1.5 % to its edges
    let fall = office.roof.slope.to_radians().tan();
    assert!((fall - 0.015).abs() < 1e-4, "{fall}");
    assert_eq!(office.roof.overhang, 0.0);
    let (wa, wb) = (&a.storeys[0].layout.walls[0], &b.storeys[0].layout.walls[0]);
    assert!((wb.a.0 - wa.a.0 - 100.0).abs() < 1e-9 && (wb.a.1 - wa.a.1 - 50.0).abs() < 1e-9);
    assert_eq!(b.roof.ring[0], (100.0, 50.0));
}

#[test]
fn cross_axes_take_the_letters_of_the_drawing_language() {
    // Cyrillic capitals without the excluded ones, doubled past the alphabet
    let ru: Vec<String> = (0..23).map(|i| axis_letter(i, true)).collect();
    assert_eq!(ru[..4].join(""), "\u{410}\u{411}\u{412}\u{413}");
    let excluded = "\u{401}\u{417}\u{419}\u{41e}\u{425}\u{426}\u{427}\u{429}\u{42a}\u{42b}\u{42c}";
    assert!(
        ru.iter().all(|l| !l.chars().any(|c| excluded.contains(c))),
        "{ru:?}"
    );
    assert_eq!(ru[21], "\u{42f}");
    assert_eq!(ru[22], "\u{410}\u{410}");
    let en: Vec<String> = (0..10).map(|i| axis_letter(i, false)).collect();
    assert!(!en.contains(&"I".to_string()) && !en.contains(&"O".to_string()));
    assert_eq!(axis_letter(24, false), "AA");
    for (lang, first) in [("ru", "\u{410}"), ("en", "A")] {
        let (_, p) = built(json!({"typology": "office", "floors": 1, "lang": lang}));
        let names: Vec<&str> = p.storeys[0]
            .layout
            .grids
            .iter()
            .map(|g| g.name.as_str())
            .collect();
        assert!(names.contains(&first), "{lang}: {names:?}");
        assert!(names.contains(&"1"), "{lang}: {names:?}");
    }
}

#[test]
fn room_numbers_stay_unique_past_ninety_nine_rooms_on_a_floor() {
    let rooms = json!([
        {"kind": "office", "area": 12, "count": 50},
        {"kind": "storage", "area": 6, "count": 50},
        {"kind": "meeting", "area": 18, "count": 4}
    ]);
    let (_, p) =
        built(json!({"typology": "office", "floors": 11, "rooms": rooms, "ground_rooms": rooms}));
    let all: Vec<&str> = p
        .storeys
        .iter()
        .flat_map(|s| s.rooms.iter().map(|r| r.number.as_str()))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let dup: Vec<&&str> = all.iter().filter(|n| !seen.insert(**n)).collect();
    assert!(dup.is_empty(), "duplicate numbers {dup:?}");
    // three digits on every floor once one floor needs them: 1 + 001, 10 + 001
    assert!(
        all.contains(&"1001") && all.contains(&"10001"),
        "{:?}",
        &all[..10]
    );
}

#[test]
fn one_floor_has_no_stair_and_forty_floors_reach_the_roof() {
    for t in ["house", "office", "school"] {
        let (s, p) = built(json!({"typology": t, "floors": 1}));
        assert_eq!(p.storeys.len(), 1);
        assert!(p.storeys[0].layout.stairs.is_empty(), "{t}");
        assert!((p.roof.elevation - s.floor_height).abs() < 1e-9, "{t}");
    }
    let t = Instant::now();
    let (s, p) = built(json!({"typology": "apartment_section", "floors": 40}));
    eprintln!(
        "gen1 timing: 40-storey section {:.0} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(p.storeys.len(), 40);
    // a high-rise has two stairs on every storey, and they run on to the roof
    assert_eq!(count(&p, "stairs"), 80);
    assert!((p.roof.elevation - 40.0 * s.floor_height).abs() < 1e-6);
    let (_, off) = built(json!({"typology": "apartment_section", "floors": 40, "rules": {"fire.stair_to_roof": false}}));
    assert_eq!(count(&off, "stairs"), 78);
    let e = parse_spec(&json!({"typology": "apartment_section", "floors": 201})).unwrap_err();
    assert!(e.to_string().contains("floors"), "{e}");
}

#[test]
fn the_smallest_and_largest_footprints_build_or_say_why() {
    // depth: two room bands of 3-12 m and the corridor, stated with the range
    let depth_err = |v: J| parse_spec(&v).unwrap_err().to_string();
    let e =
        depth_err(json!({"typology": "house", "floors": 1, "footprint": {"width": 6, "depth": 6}}));
    assert!(
        e.contains("footprint.depth: 6 m is out of range: between 7.4 m and 126.2 m with a 1.4 m corridor"),
        "{e}"
    );
    let e = depth_err(json!({"typology": "office", "footprint": {"width": 200, "depth": 130}}));
    assert!(
        e.contains("between 7.8 m and 127.8 m with a 1.8 m corridor")
            && e.contains("open bay of up to 120 m"),
        "{e}"
    );
    // past two bands the north band is one open bay between columns, gated when it parks cars
    let (_, p) = built(json!({"typology": "office", "floors": 2,
        "footprint": {"width": 40, "depth": 30},
        "rooms": [{"kind": "parking", "area": 800}, {"kind": "technical", "area": 20}]}));
    assert!((p.footprint.1 - 30.0).abs() < 1e-9);
    for st in &p.storeys {
        // the whole bay is one parking hall; spare south length adds spaces on the aisle
        let bay = st.layout.rooms.iter().zip(&st.rooms).filter(|(g, r)| {
            let xs = g.polygon.iter().map(|p| p.0);
            r.kind == RoomKind::Parking
                && xs.clone().fold(f64::MAX, f64::min) < 1e-6
                && xs.fold(f64::MIN, f64::max) > 40.0 - 1e-6
        });
        assert_eq!(bay.count(), 1, "{}", st.name);
        assert!(st.layout.columns.len() >= 10, "{}: {} columns", st.name, st.layout.columns.len());
    }
    let gates = p.storeys[0].layout.openings.iter().filter(|o| o.door && o.width > 3.0).count();
    assert_eq!(gates, 1);
    // both ends of the range parse
    for d in [25.8, 7.8] {
        parse_spec(&json!({"typology": "office", "footprint": {"depth": d}})).unwrap();
    }
    // the deepest office floor builds and passes the gates
    let (_, p) = built(json!({"typology": "office", "floors": 2, "footprint": {"depth": 25.8}}));
    assert!((p.footprint.1 - 25.8).abs() < 1e-9);
    // the shallowest one is too shallow for its stair: the reply names the depth that fits
    let s = spec(json!({"typology": "office", "floors": 2, "footprint": {"depth": 7.8}}));
    let e = build_project(&s).unwrap_err().to_string();
    let head = "footprint.depth 7.8 m is too shallow for the stair: at least ";
    assert!(e.contains(head), "{e}");
    let fits = number_after(&e, head);
    let (_, p) = built(json!({"typology": "office", "floors": 2, "footprint": {"depth": fits}}));
    assert!((p.footprint.1 - fits).abs() < 1e-9);
    // width: the program sets the minimum and the reply names it; that width builds
    let s = spec(json!({"typology": "office", "floors": 1, "footprint": {"width": 6}}));
    let e = build_project(&s).unwrap_err().to_string();
    let head = "footprint.width 6 m is too small: the program needs ";
    assert!(e.contains(head), "{e}");
    let fits = number_after(&e, head);
    let (_, p) = built(json!({"typology": "office", "floors": 1, "footprint": {"width": fits}}));
    assert!((p.footprint.0 - fits).abs() < 1e-9);
    // the widest floor: the spare length goes to rooms of the program, the gates pass
    let (_, p) = built(json!({"typology": "office", "floors": 2, "footprint": {"width": 200}}));
    assert!((p.footprint.0 - 200.0).abs() < 1e-9);
    for (w, d) in [(5.9, 12.0), (201.0, 12.0), (30.0, 5.9), (30.0, 128.0)] {
        let v = json!({"typology": "office", "footprint": {"width": w, "depth": d}});
        assert!(parse_spec(&v).is_err(), "{w} x {d}");
    }
}

#[test]
fn every_room_gets_the_kit_of_its_kind_inside_it_and_hosted_items_sit_on_walls() {
    let briefs = [
        json!({"typology": "house", "floors": 2}),
        json!({"typology": "apartment_section", "floors": 2}),
        json!({"typology": "office", "floors": 1}),
        json!({"typology": "school", "floors": 1}),
        json!({"typology": "office", "floors": 1, "footprint": {"width": 40, "depth": 30},
            "rooms": [{"kind": "parking", "area": 800}, {"kind": "technical", "area": 20}]}),
        json!({"typology": "office", "floors": 1, "rooms": [
            {"kind": "dining", "area": 60}, {"kind": "kitchen", "area": 20},
            {"kind": "ward", "area": 20, "count": 2}, {"kind": "bath", "area": 5},
            {"kind": "wc", "area": 3, "count": 2}, {"kind": "staff", "area": 15},
            {"kind": "lobby", "area": 30}]}),
    ];
    let wants: [(RoomKind, &[&str]); 7] = [
        (RoomKind::Bath, &["Bathtub|Shower Tray", "Washbasin"]),
        (RoomKind::Wc, &["WC"]),
        (RoomKind::Kitchen, &["Kitchen Counter", "Sink"]),
        (RoomKind::Bedroom, &["Bed"]),
        (RoomKind::Classroom, &["Whiteboard", "Table", "Chair"]),
        (RoomKind::Dining, &["Table", "Chair"]),
        (RoomKind::Ward, &["Hospital Bed"]),
    ];
    let mut seen = 0;
    for b in briefs {
        let (_, p) = built(b.clone());
        let (w, d) = p.footprint;
        for st in &p.storeys {
            let l = &st.layout;
            for f in &l.fixtures {
                assert!(
                    f.at.0 > 0.0 && f.at.0 < w && f.at.1 > 0.0 && f.at.1 < d,
                    "{b}: {} at {:?} outside",
                    f.family,
                    f.at
                );
                if let Some(k) = f.wall {
                    // on the room face of its wall: half the wall off the axis
                    let wl = &l.walls[k];
                    let (dx, dy) = (wl.b.0 - wl.a.0, wl.b.1 - wl.a.1);
                    let len = dx.hypot(dy);
                    let off = ((f.at.0 - wl.a.0) * dy - (f.at.1 - wl.a.1) * dx).abs() / len;
                    let half = wl.role.thickness() / 2.0;
                    assert!((off - half).abs() < 1e-6, "{}: {off} off its wall", f.family);
                }
            }
            for (g, r) in l.rooms.iter().zip(&st.rooms) {
                let xs = g.polygon.iter().map(|p| p.0);
                let ys = g.polygon.iter().map(|p| p.1);
                let bx = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
                let by = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
                let inside: Vec<&str> = l
                    .fixtures
                    .iter()
                    .filter(|f| {
                        f.at.0 > bx.0 && f.at.0 < bx.1 && f.at.1 > by.0 && f.at.1 < by.1
                    })
                    .map(|f| f.family)
                    .collect();
                for (kind, fams) in &wants {
                    if r.kind != *kind {
                        continue;
                    }
                    seen += 1;
                    for want in *fams {
                        assert!(
                            want.split('|').any(|f| inside.contains(&f)),
                            "{b}: {} {:?} {bx:?} x {by:?} has no {want}: {inside:?}",
                            st.name,
                            r.kind
                        );
                    }
                }
                if r.kind == RoomKind::Parking && (bx.1 - bx.0) * (by.1 - by.0) > 300.0 {
                    let cars = inside.iter().filter(|f| **f == "Car").count();
                    assert!(cars >= 30, "{}: {cars} cars", st.name);
                }
            }
        }
    }
    assert!(seen >= 20, "{seen} rooms checked");
}

/// The number that follows `head` in an error text.
fn number_after(e: &str, head: &str) -> f64 {
    let rest = &e[e.find(head).unwrap() + head.len()..];
    let n: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    n.parse().unwrap_or_else(|_| panic!("no number in {e}"))
}

/// Briefs of the gallery the ventilation is checked on.
fn hvac_briefs() -> Vec<J> {
    vec![
        json!({"typology": "house", "floors": 2, "lang": "ru",
            "roof": {"kind": "pitched", "slope": 30},
            "rooms": [{"kind": "living", "area": 28}, {"kind": "kitchen", "area": 14},
            {"kind": "bedroom", "area": 14, "count": 3}, {"kind": "bath", "area": 6},
            {"kind": "wc", "area": 2.5}, {"kind": "storage", "area": 4}]}),
        json!({"typology": "apartment_section", "floors": 5, "lang": "ru",
            "apartments": [{"name": "Room", "count": 8,
            "rooms": [{"kind": "bedroom", "area": 18}, {"kind": "bath", "area": 4}]}]}),
        json!({"typology": "apartment_section", "floors": 9}),
        json!({"typology": "office", "floors": 4, "lang": "ru",
            "rooms": [{"kind": "office", "area": 30, "count": 4},
            {"kind": "meeting", "area": 40}, {"kind": "wc", "area": 4, "count": 2}],
            "ground_rooms": [{"kind": "lobby", "area": 60},
            {"kind": "office", "area": 30, "count": 3}, {"kind": "wc", "area": 4, "count": 2}]}),
        json!({"typology": "school", "floors": 3}),
        json!({"typology": "office", "floors": 1, "rooms": [{"kind": "dining", "area": 60},
            {"kind": "kitchen", "area": 25}, {"kind": "storage", "area": 10},
            {"kind": "staff", "area": 10}, {"kind": "wc", "area": 4, "count": 2}]}),
    ]
}

fn key(p: (f64, f64, f64)) -> (i64, i64, i64) {
    let r = |v: f64| (v * 1000.0).round() as i64;
    (r(p.0), r(p.1), r(p.2))
}

fn root(parent: &mut [usize], i: usize) -> usize {
    let mut i = i;
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// Every room gets its air, each system is one tree from the unit to its terminals with every
/// run end on a joint or a connector, ducts stay over the footprint, and each is sized within
/// the equal-friction rate and the velocity cap.
#[test]
fn ventilation_serves_every_room_from_one_connected_tree_per_system() {
    use ddc_gen::project::{air_ports, AirSys};
    use ddc_geom::mep::duct_sizing::{friction_rect, friction_round, velocity};
    use std::collections::{HashMap, HashSet};
    for b in hvac_briefs() {
        let (s, p) = built(b.clone());
        let h = &p.hvac;
        let res = matches!(s.typology, Typology::House | Typology::ApartmentSection);
        let metric = |k: &str| p.metrics.iter().find(|m| m.0 == k).map_or(-1.0, |m| m.1);
        assert_eq!(metric("hvac_unserved"), 0.0, "{b}: every terminal placed");
        assert_eq!(h.plant.len(), 1, "{b}: one unit");
        for st in &p.storeys {
            for (pr, gr) in st.rooms.iter().zip(&st.layout.rooms) {
                let xs = gr.polygon.iter().map(|q| q.0);
                let ys = gr.polygon.iter().map(|q| q.1);
                let (x0, x1) = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
                let (y0, y1) = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
                let has = |fam: &str| {
                    st.layout.fixtures.iter().any(|f| {
                        f.family == fam && f.at.0 > x0 && f.at.0 < x1 && f.at.1 > y0 && f.at.1 < y1
                    })
                };
                let k = pr.kind;
                if k.habitable() && !(res && k == RoomKind::Kitchen) {
                    assert!(has("Supply Diffuser"), "{b}: {} {k:?} has supply", st.name);
                }
                if matches!(k, RoomKind::Bath | RoomKind::Wc | RoomKind::Kitchen) {
                    assert!(has("Exhaust Grille"), "{b}: {} {k:?} has extract", st.name);
                }
            }
        }
        let (ox, oy) = p.roof.ring[0];
        let (fw, fd) = p.footprint;
        for sys in [AirSys::Supply, AirSys::Extract] {
            let ducts: Vec<_> = h.ducts.iter().filter(|d| d.sys == sys).collect();
            if ducts.is_empty() {
                continue;
            }
            let mut ids: HashMap<(i64, i64, i64), usize> = HashMap::new();
            let mut parent: Vec<usize> = vec![];
            let mut ends: HashMap<(i64, i64, i64), usize> = HashMap::new();
            for d in &ducts {
                for q in &d.points {
                    let over = q.0 >= ox - 1e-6
                        && q.0 <= ox + fw + 1e-6
                        && q.1 >= oy - 1e-6
                        && q.1 <= oy + fd + 1e-6;
                    assert!(over, "{b}: duct point {q:?} outside the footprint");
                }
                let mut node = |q: (f64, f64, f64)| {
                    let k = key(q);
                    *ends.entry(k).or_insert(0) += 1;
                    *ids.entry(k).or_insert_with(|| {
                        parent.push(parent.len());
                        parent.len() - 1
                    })
                };
                let (ia, iz) = (node(d.points[0]), node(*d.points.last().unwrap()));
                let (ra, rz) = (root(&mut parent, ia), root(&mut parent, iz));
                parent[ra] = rz;
                let (f, v) = if d.size.1 > 0.0 {
                    let (w, hh) = d.size;
                    (friction_rect(d.flow, w, hh), velocity(d.flow, w * hh))
                } else {
                    let a = std::f64::consts::PI * d.size.0 * d.size.0 / 4.0;
                    (friction_round(d.flow, d.size.0), velocity(d.flow, a))
                };
                assert!(
                    f <= 1.0 + 1e-9 && v <= 8.0 + 1e-9,
                    "{b}: {sys:?} {:?} {f} Pa/m {v} m/s",
                    d.size
                );
            }
            let n = p.storeys.len();
            let mut ports = vec![];
            for l in h.links.iter().filter(|l| l.sys == sys) {
                let (f, base) = if l.storey == n {
                    (&h.plant[l.fixture], p.roof.elevation)
                } else {
                    let st = &p.storeys[l.storey];
                    (&st.layout.fixtures[l.fixture], st.elevation)
                };
                let (_, q) = air_ports(f)
                    .into_iter()
                    .find(|c| c.0 == l.connector)
                    .expect("a connector");
                let k = key((q.0, q.1, base + q.2));
                assert!(
                    ends.contains_key(&k),
                    "{b}: {sys:?} {} {} is not on a duct end",
                    f.family,
                    l.connector
                );
                ports.push(k);
            }
            for (k, c) in &ends {
                assert!(*c >= 2 || ports.contains(k), "{b}: {sys:?} open duct end {k:?}");
            }
            let roots: HashSet<usize> = (0..parent.len()).map(|i| root(&mut parent, i)).collect();
            assert_eq!(roots.len(), 1, "{b}: {sys:?} ducts form one tree");
            assert!(ports.len() >= 2, "{b}: {sys:?} unit and terminals linked");
        }
        eprintln!(
            "hvac {}: supply {} extract {} m3/h, {} terminals, {} m of duct, unit {}",
            s.name,
            metric("hvac_supply_m3h"),
            metric("hvac_extract_m3h"),
            metric("hvac_terminals"),
            metric("hvac_duct_m"),
            h.plant[0].ty
        );
    }
}

/// Every duct routes with its elbows: the segments are long enough for the bend radius the
/// model takes (`bend`, else the section default), as the model checks when the run is built.
#[test]
fn ducts_are_long_enough_for_their_bends() {
    use ddc_geom::math::v3;
    use ddc_geom::mep::{default_bend_radius, route, Domain, Section, Shape};
    let mut briefs = hvac_briefs();
    briefs.push(json!({"typology": "office", "floors": 3, "lang": "ru",
        "footprint": {"width": 40, "depth": 30},
        "rooms": [{"kind": "parking", "area": 800}, {"kind": "technical", "area": 20},
        {"kind": "wc", "area": 4}]}));
    briefs.push(json!({"typology": "school", "floors": 2, "lang": "ru",
        "rooms": [{"kind": "classroom", "area": 50, "count": 4},
        {"kind": "bedroom", "area": 40, "count": 2}, {"kind": "wc", "area": 12, "count": 2},
        {"kind": "staff", "area": 15}]}));
    for m in ["l", "u", "courtyard", "tower_podium"] {
        briefs.push(json!({"typology": "school", "floors": 3,
            "design": {"massing": m, "facade": "panels", "rhythm": 2.4}}));
    }
    // a two-storey house with every service off: its hood still ducts to the kitchen extract
    briefs.push(json!({"typology": "house", "name": "Garden House", "floors": 2, "floor_height": 3.0,
        "rooms": [{"kind": "living", "area": 32}, {"kind": "dining", "area": 14},
        {"kind": "kitchen", "area": 14}, {"kind": "bedroom", "area": 15, "count": 3},
        {"kind": "bath", "area": 6, "count": 2}, {"kind": "wc", "area": 2.5}, {"kind": "storage", "area": 4}],
        "roof": {"kind": "hip", "slope": 30, "overhang": 0.6}, "origin": [14, 10],
        "site": {"plot": [[0, 0], [40, 0], [40, 30], [0, 30]], "terrain": {"slope": 0.06, "downhill": [0, 1]}},
        "design": {"terrace": true, "trees": 8},
        "services": {"electrical": false, "water": false, "appliances": false}}));
    for b in briefs {
        let (_, p) = built(b.clone());
        assert!(!p.storeys.is_empty(), "{b}: no plan");
        if b["name"] == "Garden House" {
            assert!(!p.hvac.ducts.is_empty(), "{b}: no ducts");
        }
        for (i, d) in p.hvac.ducts.iter().enumerate() {
            let shape = if d.size.1 > 0.0 {
                Shape::Rect { w: d.size.0, h: d.size.1 }
            } else {
                Shape::Round { d: d.size.0 }
            };
            let mut sec = Section::new(Domain::Duct, shape);
            sec.wall = 0.0008;
            let pts: Vec<_> = d.points.iter().map(|q| v3(q.0, q.1, q.2)).collect();
            let r = route(
                &pts,
                &|u| d.bend.unwrap_or_else(|| default_bend_radius(&sec, u)),
                (0.0, 0.0),
            );
            assert!(r.is_ok(), "{b}: duct {i} {:?} {:?}: {:?}", d.sys, d.points, r.err());
        }
    }
}

/// The README briefs with every service off (the car park's typology is not on this branch) build a plan, and every run they generate (ducts,
/// pipes, conduits) bends in the model once the model fits its legs (`RunSpec::fit_bends`).
#[test]
fn readme_briefs_with_services_off_give_runs_the_model_can_bend() {
    use ddc_geom::math::v3;
    use ddc_geom::mep::{route, Domain, Section, Shape};
    use ddc_model::RunSpec;
    let off = json!({"electrical": false, "water": false, "appliances": false});
    let briefs = [
        json!({"typology": "house", "name": "Garden House", "floors": 2, "floor_height": 3.0,
            "rooms": [{"kind": "living", "area": 32}, {"kind": "dining", "area": 14},
            {"kind": "kitchen", "area": 14}, {"kind": "bedroom", "area": 15, "count": 3},
            {"kind": "bath", "area": 6, "count": 2}, {"kind": "wc", "area": 2.5}, {"kind": "storage", "area": 4}],
            "roof": {"kind": "hip", "slope": 30, "overhang": 0.6}, "origin": [14, 10],
            "site": {"plot": [[0, 0], [40, 0], [40, 30], [0, 30]], "terrain": {"slope": 0.06, "downhill": [0, 1]}},
            "design": {"terrace": true, "trees": 8}, "services": off}),
        json!({"typology": "apartment_section", "floors": 5, "services": off}),
        // the twisted ellipse office is left out: its layout fails on this branch before any duct
        // the hospital wing is left out: its layout fails on this branch (no room for the gas store)
    ];
    let mut bad = vec![];
    for b in briefs {
        let s = spec(b.clone());
        let p = build_project(&s).unwrap_or_else(|e| panic!("{b}: {e}"));
        assert!(!p.storeys.is_empty() && p.storeys.iter().any(|st| !st.layout.walls.is_empty()), "{b}: empty plan");
        // and the networks build with the fittings at every junction
        bad.extend(ddc_gen::project::duct_faults(&p).into_iter().map(|f| format!("{}: {f}", s.typology_name())));
        let mut runs: Vec<RunSpec> = vec![];
        for d in &p.hvac.ducts {
            let shape = if d.size.1 > 0.0 { Shape::Rect { w: d.size.0, h: d.size.1 } } else { Shape::Round { d: d.size.0 } };
            let mut sec = Section::new(Domain::Duct, shape);
            sec.wall = 0.0008;
            let mut r = RunSpec::new(0, 0, sec, d.points.clone());
            r.bend = d.bend;
            runs.push(r);
        }
        for g in p.services.water.iter().chain(&p.services.elec) {
            let mut r = RunSpec::new(0, 0, g.section.clone(), g.points.clone());
            r.bend = g.bend;
            runs.push(r);
        }
        for (i, mut r) in runs.into_iter().enumerate() {
            r.fit_bends();
            let pts: Vec<_> = r.points.iter().map(|q| v3(q.0, q.1, q.2)).collect();
            if let Err(e) = route(&pts, &|u| r.radius(u), (0.0, 0.0)) {
                bad.push(format!("{}: run {i} {:?}: {e}", s.typology_name(), r.points));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("
"));
}

/// Every duct of every building type routes in the model as one network per air system: each
/// segment holds the tees, crosses, elbows and reducers at both its ends, whatever their size.
#[test]
fn every_duct_segment_holds_the_fittings_at_its_ends() {
    let mut briefs = hvac_briefs();
    for t in Typology::NAMES {
        for floors in [3, 12] {
            briefs.push(json!({"typology": t, "floors": floors, "lift": true}));
        }
    }
    // the office the live AI scenarios start from
    briefs.push(json!({"typology": "office", "floors": 12, "lift": true, "lang": "en", "name": "Live base office"}));
    let mut bad = vec![];
    for b in briefs {
        // a brief over the typology's storey limit is refused before anything is built
        let Ok(s) = parse_spec(&b) else { continue };
        let p = build_project(&s).unwrap_or_else(|e| panic!("{b}: {e}"));
        for f in ddc_gen::project::duct_faults(&p).into_iter().take(6) {
            bad.push(format!("{b}: {f}"));
        }
    }
    assert!(bad.is_empty(), "{} faults:\n{}", bad.len(), bad.join("\n"));
}

/// The family house of a free-text brief: its garage on the ground floor is named so, a gable
/// roof slopes only its long sides, and the same house takes a parking basement.
#[test]
fn a_family_house_has_its_garage_a_gable_roof_and_takes_a_basement() {
    let brief = json!({"typology": "house", "floors": 2, "roof": {"kind": "gable"},
        "rooms": [{"kind": "living", "area": 26}, {"kind": "kitchen", "area": 12},
                  {"kind": "bedroom", "area": 15}, {"kind": "bedroom", "area": 14.5, "count": 2},
                  {"kind": "bath", "area": 6}, {"kind": "bath", "area": 5},
                  {"kind": "office", "area": 10}, {"kind": "parking", "area": 18}],
        "features": ["garage", "gable_roof"]});
    let (s, p) = built(brief.clone());
    let report = ddc_gen::project::brief::check(&s, &p).report;
    assert_eq!(report["features"]["met"], json!(["garage", "gable_roof"]), "{report}");
    let e = &p.roof.edges;
    assert_eq!((e.len(), e.iter().filter(|x| x.is_some()).count()), (4, 2), "gable: {e:?}");
    let (w, d) = p.footprint;
    let long = |i: usize| if w >= d { i % 2 == 0 } else { i % 2 == 1 };
    assert!(e.iter().enumerate().all(|(i, x)| x.is_some() == long(i)), "the long sides slope: {e:?} on {w}x{d}");
    let names: Vec<&str> = p.storeys[0].layout.rooms.iter().map(|r| r.name.as_str()).collect();
    assert!(names.contains(&"Garage"), "{names:?}");
    let hip = built(json!({"typology": "house", "floors": 2, "roof": {"kind": "hip", "slope": 35, "overhang": 0.6}})).1;
    assert!(hip.roof.edges.iter().all(Option::is_some), "{:?}", hip.roof.edges);
    assert_eq!((hip.roof.slope, hip.roof.overhang), (35.0, 0.6));
    assert!(parse_spec(&json!({"typology": "house", "roof": {"kind": "hip", "overhang": 9}})).is_err());

    let mut deeper = brief;
    deeper["basements"] = json!(1);
    let (_, p) = built(deeper);
    assert_eq!(p.basements.len(), 1);
    let kinds: Vec<RoomKind> = p.basements[0].rooms.iter().map(|r| r.kind).collect();
    assert!(kinds.contains(&RoomKind::Parking), "{kinds:?}");
}

/// Rooms asked for one storey go to that storey only:
/// (a) every room the brief orders for it appears on it;
/// (b) every other room it gains that is not a filler is a declared derivative of an ordered
///     room: an operating theatre comes with its scrub anteroom (`room.anteroom_depth`);
/// (c) every other storey keeps its rooms exactly: names, kinds and areas;
/// (d) fillers (the programme's spare room and storage) may change in number on that storey,
///     but its rooms cover the same floor area as before.
#[test]
fn storey_rooms_add_rooms_to_that_storey_only() {
    let theatre = json!({"name": "Operating theatre", "kind": "technical", "area": 48, "tags": ["operating"]});
    let base = json!({"typology": "hospital_wing", "floors": 4});
    let (s0, p0) = built(base.clone());
    let mut more = base.clone();
    more["storey_rooms"] = json!({"2": [theatre]});
    let (s, p1) = built(more);
    assert_eq!(s.floor_rooms(2).len(), spec(base.clone()).floor_rooms(2).len() + 1);
    assert_eq!(s.floor_rooms(1).len(), spec(base).floor_rooms(1).len());
    let area = |r: &[(f64, f64)]| {
        let n = r.len();
        (0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>().abs() / 2.0
    };
    let rooms = |p: &GProject, k: usize| -> Vec<(String, RoomKind, f64)> {
        let st = &p.storeys[k];
        let mut v: Vec<_> = st.layout.rooms.iter().zip(&st.rooms).map(|(g, r)| (g.name.clone(), r.kind, area(&g.polygon))).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)));
        v
    };
    // (c) the other storeys exactly as they were
    for k in [0, 1, 3] {
        let (a, b) = (rooms(&p0, k), rooms(&p1, k));
        assert_eq!(a.len(), b.len(), "storey {k}: {a:?} / {b:?}");
        for (x, y) in a.iter().zip(&b) {
            assert!(x.0 == y.0 && x.1 == y.1 && (x.2 - y.2).abs() < 1e-6, "storey {k}: {x:?} became {y:?}");
        }
    }
    let spare = &s0.spare;
    let filler = |r: &(String, RoomKind, f64)| r.1 == RoomKind::Storage || (r.1 == spare.kind && r.0 == spare.name.0);
    let (before, after) = (rooms(&p0, 2), rooms(&p1, 2));
    // (a) the ordered room is there
    assert!(after.iter().any(|r| r.0 == "Operating theatre"), "{after:?}");
    // (b) beyond the ordered room and fillers, only its declared derivative is new
    let mut left: Vec<&(String, RoomKind, f64)> = before.iter().filter(|r| !filler(r)).collect();
    let mut gained = vec![];
    for r in after.iter().filter(|r| !filler(r)) {
        match left.iter().position(|q| q.0 == r.0 && q.1 == r.1) {
            Some(i) => {
                left.remove(i);
            }
            None => gained.push(r.0.as_str()),
        }
    }
    gained.sort_unstable();
    assert_eq!(gained, ["Operating theatre", "Scrub anteroom"], "storey 2 gained {gained:?}");
    assert!(left.is_empty(), "storey 2 lost {left:?}");
    // (d) the storey's rooms cover the same floor area
    let total = |v: &[(String, RoomKind, f64)]| v.iter().map(|r| r.2).sum::<f64>();
    assert!((total(&before) - total(&after)).abs() < 0.1, "storey 2 covers {:.2} m2, was {:.2}", total(&after), total(&before));
    // a storey out of range is refused with its field
    let err = parse_spec(&json!({"typology": "hospital_wing", "floors": 4, "storey_rooms": {"9": []}})).err().map(|e| e.to_string());
    assert!(err.is_some_and(|e| e.contains("storey_rooms.9")));
}
