//! Fire safety and egress: a long office floor takes more stairs, a crowded hall takes exit
//! width, a plate over the compartment limit takes fire walls, corridor ends stay within the
//! dead-end limit, and stair enclosures are rated.

use ddc_gen::project::{build_project, check_project, fire, nav, parse_spec, GProject, ProjectSpec, RoomKind};
use ddc_gen::{FireSep, WallRole};
use serde_json::{json, Value as J};

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{:?}", g.failures);
    (s, p)
}

fn long_office(extra: J) -> J {
    let mut v = json!({
        "typology": "office",
        "floors": 3,
        "rooms": [{"kind": "office", "area": 30, "count": 26}, {"kind": "wc", "area": 4, "count": 2}],
        "ground_rooms": [{"kind": "lobby", "area": 40}, {"kind": "office", "area": 30, "count": 24}]
    });
    if let J::Object(m) = extra {
        for (k, x) in m {
            v[k] = x;
        }
    }
    v
}

fn rules_broken(s: &ProjectSpec, p: &GProject) -> Vec<&'static str> {
    nav::check(s, p).iter().map(|f| f.rule).collect()
}

#[test]
fn a_long_office_floor_gets_a_second_stair() {
    let (s, p) = built(long_office(json!({})));
    let m = fire::measure(&s, &p);
    assert!(m[1].need >= 2, "{} occupants need {} exit(s)", m[1].people, m[1].need);
    let stairs = p.storeys[0].layout.stairs.len();
    assert!(stairs >= 2, "{stairs} stair(s)");
    for (k, f) in m.iter().enumerate() {
        assert!(f.exits >= f.need, "floor {k}: {} exits for {} occupants", f.exits, f.people);
        let w = f64::from(f.people) * s.rules.get("egress.door_per_person");
        assert!(f.exit_width >= w - 1e-9, "floor {k}: exits {:.2} m for {w:.2} m", f.exit_width);
    }
    let broken = rules_broken(&s, &p);
    for r in ["egress.single_exit_people", "egress.stair_per_person", "egress.max_travel", "egress.exit_separation"] {
        assert!(!broken.contains(&r), "{r}: {:?}", nav::check(&s, &p));
    }
    // one stair is enough for a short floor
    let (_, small) = built(json!({"typology": "office", "floors": 3}));
    assert_eq!(small.storeys[0].layout.stairs.len(), 1);
}

#[test]
fn dead_ends_stay_within_the_limit() {
    let (s, p) = built(long_office(json!({})));
    let limit = s.rules.get("egress.dead_end");
    for (k, f) in fire::measure(&s, &p).iter().enumerate() {
        assert!(f.dead_end <= limit + 1e-6, "floor {k}: dead end {:.1} m > {limit} m", f.dead_end);
    }
    assert!(!rules_broken(&s, &p).contains(&"egress.dead_end"));
    // a stricter country limit is met too: the generator adds the stairs and doors it needs
    let cite = "xx egress.dead_end_max: Test code, art. 1";
    let (s, p) = built(long_office(json!({"norms": {"egress.dead_end": {"value": 6.0, "cite": cite}}})));
    for (k, f) in fire::measure(&s, &p).iter().enumerate() {
        assert!(f.dead_end <= 6.0 + 1e-6, "floor {k}: dead end {:.1} m", f.dead_end);
    }
}

#[test]
fn a_400_person_hall_gets_enough_exit_width() {
    let (s, p) = built(json!({
        "typology": "office",
        "floors": 1,
        "footprint": {"width": 30, "depth": 30},
        "rooms": [{"kind": "dining", "area": 560}, {"kind": "office", "area": 20, "count": 2}, {"kind": "lobby", "area": 30}]
    }));
    let st = &p.storeys[0];
    let people = fire::people_of(&s, st);
    let hall = (0..st.rooms.len()).find(|&r| st.rooms[r].kind == RoomKind::Dining).expect("a dining hall");
    assert!(people[hall] >= 400, "{} occupants", people[hall]);
    let f = &fire::measure(&s, &p)[0];
    assert!(f.crowded.is_empty(), "{:?}", f.crowded);
    let pp = s.rules.get("egress.door_per_person");
    assert!(f.exits >= 2 && f.exit_width >= f64::from(f.people) * pp - 1e-9, "{f:?}");
    assert!(!rules_broken(&s, &p).contains(&"egress.door_per_person"), "{:?}", nav::check(&s, &p));
    // the hall has two doors or more, the added ones at least the least exit width
    let l = &st.layout;
    let on_hall = |o: &&ddc_gen::GOpening| {
        let w = &l.walls[o.wall];
        let len = ((w.b.0 - w.a.0).powi(2) + (w.b.1 - w.a.1).powi(2)).sqrt();
        let t = (o.along + o.width / 2.0) / len;
        let c = (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t);
        let poly = &l.rooms[hall].polygon;
        (0..poly.len()).any(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let u = (((c.0 - a.0) * dx + (c.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (c.0 - a.0 - u * dx).hypot(c.1 - a.1 - u * dy) < 1e-6
        })
    };
    let doors: Vec<_> = l.openings.iter().filter(|o| o.door).filter(on_hall).collect();
    assert!(doors.len() >= 2, "{} door(s) of the hall", doors.len());
    let min = s.rules.get("egress.exit_door_min");
    assert!(doors.iter().all(|o| o.width >= min - 1e-9), "{doors:?}");
}

#[test]
fn a_compartment_over_the_limit_gets_a_fire_wall() {
    let cite = "xx fire.compartment_area_max: Test code, art. 2";
    let (s, p) = built(long_office(json!({"norms": {"fire.compartment_area": {"value": 500, "cite": cite}}})));
    let rating = s.rules.get("fire.wall_rating").round() as u32;
    for st in &p.storeys {
        let l = &st.layout;
        let walls: Vec<_> = l.walls.iter().filter(|w| w.fire.is_some_and(|f| f.sep == FireSep::Compartment)).collect();
        assert!(!walls.is_empty(), "{}: no fire wall", st.name);
        assert!(walls.iter().all(|w| w.role != WallRole::Exterior && w.fire.unwrap().minutes >= rating));
        let comp = fire::compartments_of(l);
        let mut ids = comp.clone();
        ids.sort_unstable();
        ids.dedup();
        assert!(ids.len() >= 2, "{}: one compartment", st.name);
    }
    let found = nav::check(&s, &p);
    assert!(found.iter().all(|f| f.rule != "fire.compartment_area"), "{found:?}");
    // without the limit the bar keeps one compartment
    let (_, plain) = built(long_office(json!({})));
    assert!(plain.storeys.iter().flat_map(|s| &s.layout.walls).all(|w| w.fire.is_none_or(|f| f.sep != FireSep::Compartment)));
}

#[test]
fn stair_enclosures_are_rated_and_findings_cite_the_norm() {
    let cite = "xx fire.stair_enclosure_rating: Test code, art. 4";
    let (s, p) = built(json!({"typology": "office", "floors": 3,
        "norms": {"fire.stair_rating": {"value": 90, "cite": cite}}}));
    for st in &p.storeys {
        let n = st.layout.walls.iter().filter(|w| w.fire.is_some_and(|f| f.sep == FireSep::Stair && f.minutes == 90)).count();
        assert!(n >= 2, "{}: {n} rated stair wall(s)", st.name);
    }
    assert_eq!(s.rules.cite("fire.stair_rating"), Some(cite));
    // a storey short of exits names the rule and its clause
    let cite = "xx egress.single_exit_occupants_max: Test code, art. 5";
    let (s, p) = built(long_office(json!({"norms": {"egress.single_exit_people": {"value": 1, "cite": cite}},
        "rules": {"egress.stairs_max": 1}})));
    let f = nav::check(&s, &p).into_iter().find(|f| f.rule == "egress.single_exit_people").expect("a finding");
    assert_eq!(f.cite.as_deref(), Some(cite));
    assert!(f.line().contains(cite));
}

#[test]
fn smokeproof_stairs_get_a_two_door_lobby_above_and_below_grade() {
    let (s, p) = built(json!({"typology": "office", "floors": 4, "basements": 1,
        "rules": {"egress.protected_stairs": "smokeproof"}}));
    let lobbies = fire::smoke_lobbies(&p);
    let levels: Vec<&ddc_gen::project::PStorey> = p.storeys.iter().chain(&p.basements).collect();
    assert!(!p.basements.is_empty());
    for (k, st) in levels.iter().enumerate() {
        let stairs = st.rooms.iter().filter(|r| r.kind == RoomKind::Stair).count();
        let mine: Vec<usize> = lobbies.iter().filter(|l| l.0 == k).map(|l| l.1).collect();
        assert_eq!(mine.len(), stairs, "{}: a lobby per stair room", st.name);
        for &r in &mine {
            let poly = &st.layout.rooms[r].polygon;
            let on = |q: (f64, f64)| {
                (0..poly.len()).any(|i| {
                    let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                    let l2 = (b.0 - a.0).powi(2) + (b.1 - a.1).powi(2);
                    let t = (((q.0 - a.0) * (b.0 - a.0) + (q.1 - a.1) * (b.1 - a.1)) / l2).clamp(0.0, 1.0);
                    (q.0 - a.0 - t * (b.0 - a.0)).hypot(q.1 - a.1 - t * (b.1 - a.1)) < 1e-4
                })
            };
            let doors = st
                .layout
                .openings
                .iter()
                .filter(|o| o.door)
                .filter(|o| {
                    let w = &st.layout.walls[o.wall];
                    let t = (o.along + o.width / 2.0) / w.length();
                    on((w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t))
                })
                .count();
            assert!(doors >= 2, "{}: lobby {r} has {doors} door(s)", st.name);
            let a: f64 = poly.windows(2).map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1).sum::<f64>()
                + poly[poly.len() - 1].0 * poly[0].1
                - poly[0].0 * poly[poly.len() - 1].1;
            assert!(a.abs() / 2.0 >= s.rules.get("egress.smoke_lobby_area") - 1e-6, "{}: lobby area", st.name);
        }
        let rated = st.layout.walls.iter().filter(|w| w.fire.is_some_and(|f| f.sep == FireSep::Stair)).count();
        assert!(rated >= 3, "{}: {rated} rated stair wall(s)", st.name);
    }
    assert!(rules_broken(&s, &p).iter().all(|r| *r != "egress.protected_stairs"), "{:?}", nav::check(&s, &p));
    // the enclosed level cuts no lobby, none leaves the stairs open
    let (_, q) = built(json!({"typology": "office", "floors": 4, "basements": 1}));
    assert!(fire::smoke_lobbies(&q).is_empty());
    let (_, n) = built(json!({"typology": "office", "floors": 4, "rules": {"egress.protected_stairs": "none"}}));
    assert!(n.storeys.iter().flat_map(|s| &s.layout.walls).all(|w| w.fire.is_none_or(|f| f.sep != FireSep::Stair)));
}

#[test]
fn basements_are_measured_and_checked_for_compartments_like_the_storeys() {
    let (s, p) = built(json!({"typology": "office", "floors": 3, "basements": 2}));
    let m = fire::measure_basements(&s, &p);
    assert_eq!(m.len(), 2);
    for (b, f) in m.iter().enumerate() {
        // the way out of a basement is a stair door
        assert!(f.exits >= 1 && f.exits >= f.need, "basement {b}: {} exit(s) for {} occupants", f.exits, f.people);
        let l = &p.basements[b].layout;
        assert_eq!(fire::compartments_of(l).len(), l.rooms.len());
    }
    // the storeys are cut into compartments within the limit, a basement left in one piece is
    // found, and its level index follows the storeys
    let cite = "xx fire.compartment_area_max: Test code, art. 2";
    let (s, mut p) = built(long_office(json!({"basements": 1, "norms": {"fire.compartment_area": {"value": 500, "cite": cite}}})));
    for w in &mut p.basements[0].layout.walls {
        if w.fire.is_some_and(|f| f.sep == FireSep::Compartment) {
            w.fire = None;
        }
    }
    let found = nav::check(&s, &p);
    let f = found.iter().find(|f| f.rule == "fire.compartment_area").unwrap_or_else(|| panic!("{found:?}"));
    assert_eq!(f.storey, Some(p.storeys.len()), "{found:?}");
}
