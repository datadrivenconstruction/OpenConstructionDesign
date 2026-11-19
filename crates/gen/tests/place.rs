// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Free placement (`layout.strategy` free / auto): the same brief gives the same building,
//! every floor passes the gates, the corridor runs only between the end rooms, pinned rooms
//! stay where they are and the rest is placed round them, a lock-breaking change is worked
//! around instead of refused, and forty rooms on a floor are placed in bounded time.

use ddc_gen::project::locks::{regenerate, Keep, Lock, Target};
use ddc_gen::project::{
    build_project, check_project, parse_spec, GProject, PRole, Pin, Pins, ProjectSpec, RoomKind,
};
use serde_json::{json, Value as J};

type P = (f64, f64);

fn with(v: &J, strategy: &str) -> J {
    let mut b = v.clone();
    b["rules"]["layout.strategy"] = json!(strategy);
    b
}

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{e}: {v}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}: {v}"));
    (s, p)
}

fn area(r: &[P]) -> f64 {
    let n = r.len();
    (0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>().abs() / 2.0
}

fn bbox(r: &[P]) -> (P, P) {
    let lo = r.iter().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
    let hi = r.iter().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
    (lo, hi)
}

fn home() -> J {
    json!([
        {"kind": "living", "area": 25},
        {"kind": "kitchen", "area": 12},
        {"kind": "bedroom", "area": 14, "count": 3},
        {"kind": "bath", "area": 5},
        {"kind": "wc", "area": 2}
    ])
}

fn briefs() -> Vec<(&'static str, J)> {
    vec![
        ("house_1_storey", json!({"typology": "house", "floors": 1, "rooms": home()})),
        ("house_2_storeys", json!({"typology": "house", "floors": 2, "rooms": home()})),
        (
            "office_4_storeys",
            json!({"typology": "office", "floors": 4,
                   "rooms": [{"kind": "office", "area": 30, "count": 4}, {"kind": "meeting", "area": 40},
                             {"kind": "wc", "area": 4, "count": 2}],
                   "ground_rooms": [{"kind": "lobby", "area": 60}, {"kind": "office", "area": 30, "count": 3},
                                    {"kind": "wc", "area": 4, "count": 2}]}),
        ),
        (
            "clinic",
            json!({"typology": "office", "floors": 2,
                   "rooms": [{"kind": "office", "area": 16, "count": 8}, {"kind": "wc", "area": 4, "count": 2}],
                   "ground_rooms": [{"kind": "lobby", "area": 40}, {"kind": "office", "area": 16, "count": 4}]}),
        ),
        (
            "cafe",
            json!({"typology": "office", "floors": 1,
                   "rooms": [{"kind": "lobby", "area": 60}, {"kind": "kitchen", "area": 25},
                             {"kind": "storage", "area": 10}, {"kind": "wc", "area": 4, "count": 2}]}),
        ),
    ]
}

fn corridor_area(p: &GProject) -> f64 {
    p.storeys
        .iter()
        .flat_map(|st| st.layout.rooms.iter().zip(&st.rooms))
        .filter(|(_, r)| r.kind == RoomKind::Corridor)
        .map(|(g, _)| area(&g.polygon))
        .sum()
}

#[test]
fn free_plans_pass_the_gates_and_repeat() {
    for (name, v) in briefs() {
        let v = with(&v, "free");
        let (s, p) = built(&v);
        let g = check_project(&s, &p);
        assert!(g.passed(), "{name}: {:?}", g.failures);
        let (_, again) = built(&v);
        assert_eq!(format!("{p:?}"), format!("{again:?}"), "{name}: not deterministic");
    }
}

/// Every room of a free floor keeps its asked area exactly: a frontage is the area over the
/// side depth, never a module step, so the gates' 2 % holds with nothing to spare.
#[test]
fn asked_rooms_keep_their_area() {
    for (name, v) in briefs() {
        let (_, p) = built(&with(&v, "free"));
        for st in &p.storeys {
            for (g, r) in st.layout.rooms.iter().zip(&st.rooms) {
                if let (Some(want), false) = (r.asked, r.grown) {
                    let a = area(&g.polygon);
                    assert!((a - want).abs() <= 1e-6 * want.max(1.0) + 1e-6, "{name}: room {} is {a}, asked {want}", r.number);
                }
            }
        }
    }
}

/// The spine is one rectangle a corridor wide that runs only between the end rooms, so on
/// every floor it is shorter than the footprint by the end rooms' frontages; the band runs
/// its corridor along the whole length, so the free corridor is never larger.
#[test]
fn the_corridor_runs_between_the_end_rooms() {
    for (name, v) in briefs() {
        let (s, p) = built(&with(&v, "free"));
        let (w, d) = p.footprint;
        for (k, st) in p.storeys.iter().enumerate() {
            let corr: Vec<&Vec<P>> = st
                .layout
                .rooms
                .iter()
                .zip(&st.rooms)
                .filter(|(_, r)| r.kind == RoomKind::Corridor)
                .map(|(g, _)| &g.polygon)
                .collect();
            assert_eq!(corr.len(), 1, "{name} floor {k}: one spine");
            let ((x0, y0), (x1, y1)) = bbox(corr[0]);
            assert!((area(corr[0]) - (x1 - x0) * (y1 - y0)).abs() < 1e-6, "{name} floor {k}: a rectangle");
            assert!((y1 - y0 - s.corridor).abs() < 1e-6, "{name} floor {k}: a corridor wide");
            // what the spine leaves at its ends is taken by rooms of the full depth
            let ends: f64 = st
                .layout
                .rooms
                .iter()
                .map(|g| bbox(&g.polygon))
                .filter(|((_, ya), (_, yb))| ya.abs() < 1e-6 && (yb - d).abs() < 1e-6)
                .map(|((xa, _), (xb, _))| xb - xa)
                .sum();
            assert!((x1 - x0 + ends - w).abs() < 1e-6, "{name} floor {k}: spine {} + ends {ends} != {w}", x1 - x0);
        }
        // a corridor wide along the spine and nothing else
        let spines: f64 = p.storeys.len() as f64 * s.corridor * w;
        assert!(corridor_area(&p) <= spines + 1e-6, "{name}");
    }
}

/// Auto takes the free plan only when it passes the gates and scores better, so it never
/// scores worse than the band.
#[test]
fn auto_never_scores_worse_than_the_band() {
    use ddc_gen::project::score::score;
    for (name, v) in briefs() {
        let (s, auto) = built(&with(&v, "auto"));
        let (_, band) = built(&with(&v, "band"));
        assert!(!score(&s, &auto).compare(&score(&s, &band)).is_gt(), "{name}");
        assert!(check_project(&s, &auto).passed(), "{name}");
    }
}

#[test]
fn pinned_rooms_stay_and_the_rest_moves_round_them() {
    let v = with(&json!({"typology": "house", "floors": 1, "rooms": home()}), "free");
    let (s, p) = built(&v);
    let st = &p.storeys[0];
    // a plain rectangle: a host with small rooms in its front strip is an L
    let (g, r) = st
        .layout
        .rooms
        .iter()
        .zip(&st.rooms)
        .find(|(g, r)| r.kind.habitable() && r.asked.is_some() && g.polygon.len() == 4)
        .unwrap();
    let pins = Pins {
        width: p.footprint.0,
        depth: p.footprint.1,
        south: s.band_depth,
        core_x: None,
        rooms: vec![Pin { storey: 0, kind: r.kind, name: g.name.clone(), owner: PRole::Public, polygon: g.polygon.clone() }],
        flats: vec![],
    };
    // one bedroom fewer: the rest is placed again, the pinned one stays
    let mut next = v.clone();
    next["rooms"][2]["count"] = json!(2);
    let mut s2 = parse_spec(&next).unwrap();
    s2.pins = Some(pins);
    let p2 = build_project(&s2).unwrap();
    assert!(check_project(&s2, &p2).passed(), "{:?}", check_project(&s2, &p2).failures);
    let want = bbox(&g.polygon);
    let kept = p2.storeys[0].layout.rooms.iter().any(|h| {
        let b = bbox(&h.polygon);
        (b.0 .0 - want.0 .0).abs() < 1e-6 && (b.0 .1 - want.0 .1).abs() < 1e-6 && (b.1 .0 - want.1 .0).abs() < 1e-6 && (b.1 .1 - want.1 .1).abs() < 1e-6
    });
    assert!(kept, "the pinned bedroom moved");
    assert_eq!(p2.footprint, p.footprint);
    let beds = p2.storeys[0].rooms.iter().filter(|r| r.kind == RoomKind::Bedroom).count();
    assert_eq!(beds, 2);
}

#[test]
fn a_lock_breaking_change_is_worked_around() {
    let prev = with(&json!({"typology": "house", "floors": 1, "rooms": home()}), "band");
    let (s, p) = built(&prev);
    let number = p.storeys[0].rooms.iter().find(|r| r.kind == RoomKind::Bedroom).unwrap().number.clone();
    let lock = Lock::take(Target::Room { storey: 0, number }, Keep::Geometry, &prev, &s, &p).unwrap();
    let mut next = prev.clone();
    next["rooms"][2]["count"] = json!(2);
    let r = regenerate(&prev, &next, &[lock.clone()]).unwrap();
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert!(lock.holds(&r.brief, &r.spec, &r.project));
    assert_eq!(r.project.storeys[0].rooms.iter().filter(|x| x.kind == RoomKind::Bedroom).count(), 2);
    // the brief carries what was kept: built alone it gives the same plan
    assert!(r.brief["pins"].is_object(), "{}", r.brief);
    let (_, again) = built(&r.brief);
    assert_eq!(format!("{again:?}"), format!("{:?}", r.project));
}

#[test]
fn pins_round_trip_through_the_brief() {
    let pins = Pins {
        width: 12.5,
        depth: 11.0,
        south: 5.0,
        core_x: Some(3.25),
        rooms: vec![Pin { storey: 1, kind: RoomKind::Kitchen, name: "Kitchen".into(), owner: PRole::Apartment(3), polygon: vec![(0.0, 0.0), (3.0, 0.0), (3.0, 4.0), (0.0, 4.0)] }],
        flats: vec![Pin { storey: 1, kind: RoomKind::Hall, name: String::new(), owner: PRole::Apartment(3), polygon: vec![(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)] }],
    };
    assert_eq!(Pins::from_json(&pins.to_json()).unwrap(), pins);
    assert!(Pins::from_json(&json!({"width": 1, "depth": 1, "south": 1, "rooms": [{"kind": "nope", "storey": 0, "polygon": []}]})).is_err());
    let mut v = json!({"typology": "house", "floors": 1, "rooms": home()});
    v["pins"] = json!({"width": -1});
    assert!(parse_spec(&v).is_err());
}

/// Forty rooms on one floor: the search is bounded by its node budget, so the time is too.
/// Five seconds is the target of an optimised build; an unoptimised test build runs the same
/// nodes about ten times slower.
#[test]
fn forty_rooms_are_placed_in_bounded_time() {
    let v = with(
        &json!({"typology": "office", "floors": 1,
                "rooms": [{"kind": "office", "area": 16, "count": 30}, {"kind": "wc", "area": 4, "count": 6},
                          {"kind": "storage", "area": 6, "count": 3}]}),
        "free",
    );
    let t = std::time::Instant::now();
    let (s, p) = built(&v);
    let secs = t.elapsed().as_secs_f64();
    let limit = if cfg!(debug_assertions) { 50.0 } else { 5.0 };
    assert!(secs <= limit, "{secs:.1} s");
    let rooms = p.storeys[0].rooms.iter().filter(|r| r.asked.is_some()).count();
    assert_eq!(rooms, 39);
    assert!(check_project(&s, &p).passed(), "{:?}", check_project(&s, &p).failures);
}

// ---- apartment sections: section -> apartment -> rooms ----

fn sections() -> Vec<(&'static str, J)> {
    vec![
        ("section_3", json!({"typology": "apartment_section", "floors": 3})),
        (
            "hotel_3",
            json!({"typology": "apartment_section", "floors": 3,
                   "apartments": [{"name": "room", "count": 4,
                                   "rooms": [{"kind": "bedroom", "area": 18}, {"kind": "bath", "area": 4}]}]}),
        ),
    ]
}

fn section(v: &J, kind: &str, sun: &str) -> J {
    let mut b = with(v, "free");
    b["rules"]["layout.section"] = json!(kind);
    b["rules"]["light.sun_side"] = json!(sun);
    b
}

fn on_edge(r: &[P], p: P) -> bool {
    let n = r.len();
    (0..n).any(|i| {
        let (a, b) = (r[i], r[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2;
        let q = (a.0 + t * dx, a.1 + t * dy);
        (-1e-6..=1.0 + 1e-6).contains(&t) && ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt() < 1e-4
    })
}

fn centre(r: &[P]) -> P {
    let ((x0, y0), (x1, y1)) = bbox(r);
    ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
}

/// Every apartment of every storey: its rooms are its own, and the counts the brief asks.
fn flats_of(p: &GProject) -> Vec<(usize, u32)> {
    let mut v: Vec<(usize, u32)> = p
        .storeys
        .iter()
        .enumerate()
        .flat_map(|(k, st)| st.rooms.iter().filter_map(move |r| match r.owner {
            PRole::Apartment(a) => Some((k, a)),
            PRole::Public => None,
        }))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn sections_and_hotels_build_free_as_corridor_and_gallery() {
    for (name, v) in sections() {
        let (_, band) = built(&with(&v, "band"));
        for kind in ["corridor", "gallery"] {
            let b = section(&v, kind, "any");
            let (s, p) = built(&b);
            let g = check_project(&s, &p);
            assert!(g.passed(), "{name} {kind}: {:?}", g.failures);
            assert_eq!(p.apartments, band.apartments, "{name} {kind}: the brief's apartments");
            assert_eq!(flats_of(&p).len() as u32, p.apartments, "{name} {kind}");
            let (_, again) = built(&b);
            assert_eq!(format!("{p:?}"), format!("{again:?}"), "{name} {kind}: not deterministic");
            if kind == "gallery" {
                // one side of rooms, the corridor on the north facade
                assert!((p.footprint.1 - s.band_depth - s.corridor).abs() < 1e-6, "{name}");
            }
        }
    }
}

/// The apartment's hall has its door on a wall it shares with the public corridor, and every
/// other room of the apartment opens on that hall.
#[test]
fn every_apartment_is_entered_from_the_corridor() {
    for (name, v) in sections() {
        for kind in ["corridor", "gallery"] {
            let (_, p) = built(&section(&v, kind, "any"));
            for (k, st) in p.storeys.iter().enumerate() {
                let doors: Vec<P> = st
                    .layout
                    .openings
                    .iter()
                    .filter(|o| o.door)
                    .map(|o| {
                        let w = &st.layout.walls[o.wall];
                        let l = ((w.b.0 - w.a.0).powi(2) + (w.b.1 - w.a.1).powi(2)).sqrt();
                        let t = (o.along + o.width / 2.0) / l;
                        (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t)
                    })
                    .collect();
                let poly = |i: usize| &st.layout.rooms[i].polygon;
                let corr: Vec<usize> = (0..st.rooms.len()).filter(|&i| st.rooms[i].kind == RoomKind::Corridor).collect();
                for (_, a) in flats_of(&p).into_iter().filter(|f| f.0 == k) {
                    let mine: Vec<usize> = (0..st.rooms.len()).filter(|&i| st.rooms[i].owner == PRole::Apartment(a)).collect();
                    let halls: Vec<usize> = mine.iter().copied().filter(|&i| st.rooms[i].kind == RoomKind::Hall).collect();
                    assert_eq!(halls.len(), 1, "{name} {kind} floor {k} flat {a}: one hall");
                    let h = halls[0];
                    let entered = doors.iter().any(|&d| on_edge(poly(h), d) && corr.iter().any(|&c| on_edge(poly(c), d)));
                    assert!(entered, "{name} {kind} floor {k} flat {a}: no door from the corridor");
                    for &r in mine.iter().filter(|&&r| r != h) {
                        let opens = doors.iter().any(|&d| on_edge(poly(r), d) && on_edge(poly(h), d));
                        assert!(opens, "{name} {kind} floor {k} flat {a}: room {} has no door to the hall", st.rooms[r].number);
                    }
                }
            }
        }
    }
}

/// Wet rooms stand on the risers: every bath and toilet above the ground floor has one on the
/// storey below within `mep.riser_offset` (1.5 m by default) of its centre.
#[test]
fn wet_rooms_stand_on_the_risers() {
    let reach = 1.5;
    for (name, v) in sections() {
        for kind in ["corridor", "gallery"] {
            let (_, p) = built(&section(&v, kind, "any"));
            let wet = |k: usize| -> Vec<P> {
                let st = &p.storeys[k];
                (0..st.rooms.len())
                    .filter(|&i| matches!(st.rooms[i].kind, RoomKind::Bath | RoomKind::Wc))
                    .map(|i| centre(&st.layout.rooms[i].polygon))
                    .collect()
            };
            assert!(!wet(0).is_empty(), "{name}: the apartments have wet rooms");
            for k in 1..p.storeys.len() {
                let below = wet(k - 1);
                for w in wet(k) {
                    let d = below.iter().map(|b| ((w.0 - b.0).powi(2) + (w.1 - b.1).powi(2)).sqrt()).fold(f64::MAX, f64::min);
                    assert!(d <= reach + 1e-6, "{name} {kind} floor {}: a wet room {d:.2} m off the riser", k + 1);
                }
            }
        }
    }
}

/// `light.sun_side` south: no apartment faces north only; each has a habitable room on the
/// south facade (y = 0) or an end facade. A flat on the north side of the corridor would
/// have none, so the rule keeps flats off it.
#[test]
fn the_sun_rule_puts_every_flat_on_the_south_facade() {
    for (name, v) in sections() {
        for kind in ["corridor", "gallery"] {
            let (s, p) = built(&section(&v, kind, "south"));
            assert!(check_project(&s, &p).passed(), "{name} {kind}");
            for st in &p.storeys {
                let flats: std::collections::BTreeSet<u32> = st.rooms.iter().filter_map(|r| match r.owner {
                    PRole::Apartment(a) => Some(a),
                    PRole::Public => None,
                }).collect();
                let w = p.footprint.0;
                for a in flats {
                    let sunny = st.rooms.iter().zip(&st.layout.rooms).any(|(r, g)| {
                        let ((x0, y0), (x1, _)) = bbox(&g.polygon);
                        r.owner == PRole::Apartment(a) && r.kind.habitable() && (y0.abs() < 1e-6 || x0.abs() < 1e-6 || (x1 - w).abs() < 1e-6)
                    });
                    assert!(sunny, "{name} {kind}: flat {a} faces north only");
                }
            }
        }
    }
}

#[test]
fn auto_takes_free_sections_only_when_they_score_better() {
    use ddc_gen::project::score::score;
    for (name, v) in sections() {
        let (s, auto) = built(&with(&v, "auto"));
        let (_, band) = built(&with(&v, "band"));
        assert!(check_project(&s, &auto).passed(), "{name}");
        assert!(!score(&s, &auto).compare(&score(&s, &band)).is_gt(), "{name}");
    }
}

// ---- pins in apartment sections ----

/// The rooms of apartment `a` on storey `k`: number and outline.
fn rooms_of(p: &GProject, k: usize, a: u32) -> Vec<(String, Vec<P>)> {
    let st = &p.storeys[k];
    st.rooms
        .iter()
        .zip(&st.layout.rooms)
        .filter(|(r, _)| r.owner == PRole::Apartment(a))
        .map(|(r, g)| (r.number.clone(), g.polygon.clone()))
        .collect()
}

fn same_box(a: &[P], b: &[P]) -> bool {
    let (p, q) = (bbox(a), bbox(b));
    [p.0 .0 - q.0 .0, p.0 .1 - q.0 .1, p.1 .0 - q.1 .0, p.1 .1 - q.1 .1].iter().all(|d| d.abs() < 1e-6)
}

fn kept(p: &GProject, k: usize, a: u32, polygon: &[P]) -> bool {
    let st = &p.storeys[k];
    st.rooms.iter().zip(&st.layout.rooms).any(|(r, g)| r.owner == PRole::Apartment(a) && same_box(&g.polygon, polygon))
}

/// A pinned apartment stands first as an obstacle: one apartment fewer is asked, the pinned
/// one keeps every room where it was and the others are laid out again around it.
#[test]
fn a_pinned_apartment_stands_and_the_others_are_laid_round_it() {
    for (name, v) in sections() {
        let prev = section(&v, "corridor", "any");
        let (s, p) = built(&prev);
        let a = flats_of(&p).into_iter().find(|x| x.0 == 0).unwrap().1;
        let rooms = rooms_of(&p, 0, a);
        let locks: Vec<Lock> = rooms
            .iter()
            .map(|(number, _)| Lock::take(Target::Room { storey: 0, number: number.clone() }, Keep::Geometry, &prev, &s, &p).unwrap())
            .collect();
        let mut next = prev.clone();
        next["floors"] = json!(4);
        if name == "hotel_3" {
            next["apartments"][0]["count"] = json!(3);
        }
        let r = regenerate(&prev, &next, &locks).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(r.refused.is_empty(), "{name}: {:?}", r.refused);
        let g = check_project(&r.spec, &r.project);
        assert!(g.passed(), "{name}: {:?}", g.failures);
        for (l, (_, poly)) in locks.iter().zip(&rooms) {
            assert!(l.holds(&r.brief, &r.spec, &r.project), "{name}: a lock broke");
            assert!(kept(&r.project, 0, a, poly), "{name}: a room of the pinned apartment moved");
        }
        let per: Vec<u32> = (0..r.project.storeys.len())
            .map(|k| flats_of(&r.project).iter().filter(|x| x.0 == k).count() as u32)
            .collect();
        assert!(per.windows(2).all(|w| w[0] == w[1]), "{name}: storeys differ in apartments {per:?}");
        if name == "hotel_3" {
            assert_eq!(per[0], 3, "{name}: the others are laid out around the pinned one");
        }
        let (_, again) = built(&r.brief);
        assert_eq!(format!("{again:?}"), format!("{:?}", r.project), "{name}: the brief alone builds another plan");
    }
}

/// A pinned room holds its place inside its apartment; the apartment keeps its zone and the
/// other rooms are laid out again inside it.
#[test]
fn a_pinned_room_holds_and_its_apartment_is_laid_out_round_it() {
    let v = sections().into_iter().find(|x| x.0 == "hotel_3").unwrap().1;
    let prev = section(&v, "corridor", "any");
    let (s, p) = built(&prev);
    let a = flats_of(&p).into_iter().find(|x| x.0 == 0).unwrap().1;
    let st = &p.storeys[0];
    let (r0, g0) = st
        .rooms
        .iter()
        .zip(&st.layout.rooms)
        .find(|(r, _)| r.owner == PRole::Apartment(a) && r.kind == RoomKind::Bedroom)
        .unwrap();
    let lock = Lock::take(Target::Room { storey: 0, number: r0.number.clone() }, Keep::Geometry, &prev, &s, &p).unwrap();
    // a smaller bath: the apartment is laid out again, the bedroom stays
    let mut next = prev.clone();
    next["apartments"][0]["rooms"][1]["area"] = json!(3.5);
    let r = regenerate(&prev, &next, &[lock.clone()]).unwrap();
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    let g = check_project(&r.spec, &r.project);
    assert!(g.passed(), "{:?}", g.failures);
    assert!(lock.holds(&r.brief, &r.spec, &r.project));
    assert!(kept(&r.project, 0, a, &g0.polygon), "the pinned bedroom moved");
    let st = &r.project.storeys[0];
    let bath = st.rooms.iter().zip(&st.layout.rooms).find(|(x, _)| x.owner == PRole::Apartment(a) && x.kind == RoomKind::Bath).unwrap();
    assert!(area(&bath.1.polygon) >= 3.5 - 1e-6, "the bath of the pinned apartment");
    // the zone of the apartment stays: its rooms' outline is where it was
    let zone = |p: &GProject| {
        let pts: Vec<P> = rooms_of(p, 0, a).into_iter().flat_map(|x| x.1).collect();
        bbox(&pts)
    };
    let (z0, z1) = (zone(&p), zone(&r.project));
    assert!(same_box(&[z0.0, z0.1], &[z1.0, z1.1]), "the apartment's zone moved");
}

/// A brief whose pins hold an apartment room without its apartment's zone is refused by the
/// free placement with a reason, not built with a stray room.
#[test]
fn a_loose_apartment_pin_is_refused_with_a_reason() {
    let v = sections().into_iter().find(|x| x.0 == "hotel_3").unwrap().1;
    let mut b = section(&v, "corridor", "any");
    let (_, p) = built(&b);
    let st = &p.storeys[0];
    let (r, g) = st.rooms.iter().zip(&st.layout.rooms).find(|(r, _)| r.kind == RoomKind::Bedroom).unwrap();
    let pins = Pins {
        width: p.footprint.0,
        depth: p.footprint.1,
        south: parse_spec(&b).unwrap().band_depth,
        core_x: None,
        rooms: vec![Pin { storey: 0, kind: r.kind, name: g.name.clone(), owner: r.owner, polygon: g.polygon.clone() }],
        flats: vec![],
    };
    b["pins"] = pins.to_json();
    let e = build_project(&parse_spec(&b).unwrap()).unwrap_err();
    assert!(format!("{e}").contains("apartment zone"), "{e}");
}
