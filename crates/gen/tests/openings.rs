// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed openings: every door and window of every bench project has a catalogue type, a mark
//! and a swing; exit doors open outward, stair doors with the escape direction, fire-rated doors
//! sit in rated walls, and the schedules list the types with their counts.

#[path = "../bench/suite.rs"]
mod suite;

use ddc_gen::project::openings::{catalogue, schedule, FUNCTIONS};
use ddc_gen::project::rules::{norms_from, rule, scoped};
use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, parse_spec, GProject, PRole, PStorey, ProjectSpec, RoomKind};
use ddc_gen::{GOpening, GOpeningType};
use ddc_model::checks::norm_pack::PackSet;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;

type P = (f64, f64);

/// How far from the wall axis the rooms are probed, as the builder does.
const PROBE: f64 = 0.15;
const TOL: f64 = 1e-6;

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap();
    let p = build_project(&s).unwrap();
    (s, p)
}

/// Every bench project that builds.
fn bench_projects() -> Vec<(String, ProjectSpec, GProject)> {
    suite::load_briefs()
        .iter()
        .filter_map(|b| {
            let s = parse_spec(&suite::spec_of(b)).ok()?;
            let p = build_project(&s).ok()?;
            Some((b.name.clone(), s, p))
        })
        .collect()
}

fn storeys(p: &GProject) -> impl Iterator<Item = (usize, bool, &PStorey)> {
    let up = p.storeys.iter().enumerate().map(|(k, s)| (k, k == 0, s));
    let down = p.basements.iter().enumerate().map(|(k, s)| (k, false, s));
    up.chain(down)
}

fn typed(p: &GProject) -> Vec<(&PStorey, &GOpening, &GOpeningType)> {
    storeys(p)
        .flat_map(|(_, _, st)| st.layout.openings.iter().filter_map(move |o| o.ty.as_ref().map(|t| (st, o, t))))
        .collect()
}

fn centre(st: &PStorey, o: &GOpening) -> P {
    st.layout.walls[o.wall].at(o.along + o.width / 2.0)
}

/// The room index the leaf swings into, read from the geometry alone.
fn leaf_room(st: &PStorey, o: &GOpening, t: &GOpeningType) -> Option<usize> {
    let w = &st.layout.walls[o.wall];
    let m = o.along + o.width / 2.0;
    let (c, u) = (w.at(m), w.tangent(m));
    let n = (-u.1, u.0);
    let k = if t.swing.side { -PROBE } else { PROBE };
    let q = (c.0 + n.0 * k, c.1 + n.1 * k);
    st.layout.rooms.iter().position(|r| suite::inside(q, &r.polygon))
}

/// Rooms whose outline passes through the opening's centre.
fn touching(st: &PStorey, o: &GOpening) -> Vec<usize> {
    let c = centre(st, o);
    (0..st.layout.rooms.len()).filter(|&r| suite::dist_poly(c, &st.layout.rooms[r].polygon) < TOL).collect()
}

#[test]
fn every_bench_opening_has_type_mark_and_swing() {
    let all = bench_projects();
    assert!(all.len() >= 25, "{} bench projects built", all.len());
    let mut total = 0;
    for (name, _, p) in &all {
        let mut marks: BTreeMap<String, (String, i64, i64, i64)> = BTreeMap::new();
        let mut by_type: BTreeMap<(String, i64, i64, i64), String> = BTreeMap::new();
        for (_, st) in p.blocks() {
            for (i, o) in st.layout.openings.iter().enumerate() {
                total += 1;
                let t = o.ty.as_ref().unwrap_or_else(|| panic!("{name}: opening {i} has no type"));
                let cat = catalogue().iter().find(|c| c.id == t.catalogue).unwrap_or_else(|| panic!("{name}: {}", t.catalogue));
                assert_eq!(cat.door, o.door, "{name}: {} on a {}", t.catalogue, if o.door { "door" } else { "window" });
                assert!(FUNCTIONS.contains(&t.function) && cat.uses.iter().any(|u| u == t.function), "{name}: {}", t.function);
                assert!(t.mark.starts_with(if o.door { 'D' } else { 'W' }) && t.mark.len() >= 3, "{name}: mark {:?}", t.mark);
                assert!(t.leaves == cat.leaves && t.operation == cat.operation, "{name}");
                assert!(t.clear > 0.0 && t.clear < o.width, "{name}: clear {} of {}", t.clear, o.width);
                if t.operation == "swing" {
                    // a swung leaf lands in a room or, for an exit, outside it
                    let room = leaf_room(st, o, t);
                    assert!(room.is_some() || t.exit, "{name}: leaf of {} swings into nothing", t.mark);
                }
                // one mark is one type at one size, and the other way round
                let key = (t.catalogue.clone(), (o.width * 1000.0).round() as i64, (o.height * 1000.0).round() as i64, (o.sill * 1000.0).round() as i64);
                assert_eq!(marks.entry(t.mark.clone()).or_insert_with(|| key.clone()), &key, "{name}: {} is two types", t.mark);
                assert_eq!(by_type.entry(key).or_insert_with(|| t.mark.clone()), &t.mark, "{name}: a type has two marks");
            }
        }
    }
    assert!(total > 2000, "{total} openings");
}

#[test]
fn marks_are_the_same_whatever_storey_is_read_first() {
    let (_, p) = built(&json!({"typology": "apartment_section", "floors": 5}));
    let rows = schedule(&p);
    // doors come first, marks run D01.. and W01.. without gaps
    let door_marks: Vec<&str> = rows.iter().filter(|r| r.door).map(|r| r.mark.as_str()).collect();
    let want: Vec<String> = (1..=door_marks.len()).map(|n| format!("D{n:02}")).collect();
    assert_eq!(door_marks, want.iter().map(String::as_str).collect::<Vec<_>>());
    let window_marks: Vec<&str> = rows.iter().filter(|r| !r.door).map(|r| r.mark.as_str()).collect();
    assert!(window_marks.first() == Some(&"W01"), "{window_marks:?}");
    // the same brief twice gives the same schedule
    let (_, q) = built(&json!({"typology": "apartment_section", "floors": 5}));
    assert_eq!(schedule(&q), rows);
}

#[test]
fn exit_doors_open_outward_and_stair_doors_with_the_escape_direction() {
    let (mut exits, mut stairs_in, mut stairs_out) = (0, 0, 0);
    for (name, _, p) in bench_projects() {
        for (k, ground, st) in storeys(&p) {
            for o in st.layout.openings.iter().filter(|o| o.door) {
                let t = o.ty.as_ref().unwrap();
                match t.function {
                    "exit" if t.exit => {
                        exits += 1;
                        assert!(leaf_room(st, o, t).is_none(), "{name}: exit {} swings into the building", t.mark);
                    }
                    "stair" => {
                        assert!(t.exit, "{name}: stair door not on the escape direction");
                        let into = leaf_room(st, o, t).map(|r| st.rooms[r].kind);
                        if ground {
                            // out of the stair at the ground floor
                            stairs_out += 1;
                            assert_ne!(into, Some(RoomKind::Stair), "{name}: storey {k}: ground stair door swings into the stair");
                        } else {
                            stairs_in += 1;
                            assert_eq!(into, Some(RoomKind::Stair), "{name}: storey {k}: stair door swings out of the stair");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(exits >= 30 && stairs_in >= 30 && stairs_out >= 10, "exits {exits}, stair in {stairs_in}, out {stairs_out}");
}

#[test]
fn doors_do_not_swing_into_a_corridor_from_a_room_by_default() {
    for (name, _, p) in bench_projects() {
        for (_, _, st) in storeys(&p) {
            for o in st.layout.openings.iter().filter(|o| o.door) {
                let t = o.ty.as_ref().unwrap();
                if t.operation != "swing" || t.exit || t.function == "lift" {
                    continue;
                }
                let at = touching(st, o);
                let corridor = at.iter().find(|&&r| st.rooms[r].kind == RoomKind::Corridor);
                let other = at.iter().find(|&&r| !matches!(st.rooms[r].kind, RoomKind::Corridor | RoomKind::Hall | RoomKind::Lobby));
                if let (Some(&c), Some(_)) = (corridor, other) {
                    assert_ne!(leaf_room(st, o, t), Some(c), "{name}: {} swings into the corridor", t.mark);
                }
            }
        }
    }
}

#[test]
fn fire_rated_doors_sit_in_rated_walls_and_rated_walls_hold_rated_doors() {
    let (mut stair, mut unit, mut shaft) = (0, 0, 0);
    for (name, s, p) in bench_projects() {
        let fire = |id: &str| s.rules.get(id) as u32;
        for (st, o, t) in typed(&p) {
            if !o.door {
                continue;
            }
            // a rated leaf only where the wall asks for it, a rated wall only with a rated leaf
            assert_eq!(t.fire > 0, t.wall_fire > 0, "{name}: {} EI {} in a wall of EI {}", t.mark, t.fire, t.wall_fire);
            assert!(t.fire >= t.wall_fire, "{name}: {} EI {} under its wall EI {}", t.mark, t.fire, t.wall_fire);
            if st.layout.walls[o.wall].role == ddc_gen::WallRole::Exterior {
                continue;
            }
            // read the wall's rating from the rooms either side, not from the builder's own call
            let at = touching(st, o);
            let kinds: Vec<RoomKind> = at.iter().map(|&r| st.rooms[r].kind).collect();
            let owners: Vec<PRole> = at.iter().map(|&r| st.rooms[r].owner).collect();
            if kinds.contains(&RoomKind::Stair) {
                stair += 1;
                assert_eq!(t.wall_fire, fire("opening.fire_stair"), "{name}: stair core door {}", t.mark);
            } else if kinds.contains(&RoomKind::Technical) || kinds.contains(&RoomKind::Lift) {
                shaft += 1;
                assert_eq!(t.wall_fire, fire("opening.fire_shaft"), "{name}: shaft door {}", t.mark);
            } else if owners.iter().any(|o| matches!(o, PRole::Apartment(_))) && owners.contains(&PRole::Public) {
                unit += 1;
                assert_eq!(t.wall_fire, fire("opening.fire_unit"), "{name}: flat entrance {}", t.mark);
            }
        }
    }
    assert!(stair >= 30 && unit >= 30 && shaft >= 1, "stair {stair}, unit {unit}, shaft {shaft}");
}

#[test]
fn a_rule_value_changes_the_fire_class_of_the_doors_it_governs() {
    let base = json!({"typology": "apartment_section", "floors": 5});
    let mut hot = base.clone();
    hot["rules"] = json!({"opening.fire_stair": 60, "opening.fire_unit": 0});
    let (_, p30) = built(&base);
    let (_, p60) = built(&hot);
    let classes = |p: &GProject, f: &str| -> Vec<u32> { typed(p).iter().filter(|x| x.2.function == f).map(|x| x.2.fire).collect() };
    assert!(classes(&p30, "stair").iter().all(|&m| m == 30) && !classes(&p30, "stair").is_empty());
    assert!(classes(&p60, "stair").iter().all(|&m| m == 60));
    assert!(classes(&p30, "unit").iter().all(|&m| m == 30) && !classes(&p30, "unit").is_empty());
    assert!(classes(&p60, "unit").iter().all(|&m| m == 0));
    // geometry stays: only the type changed
    let sizes = |p: &GProject| -> Vec<(f64, f64)> { typed(p).iter().map(|x| (x.1.along, x.1.width)).collect() };
    assert_eq!(sizes(&p30), sizes(&p60));
}

#[test]
fn escape_swing_follows_its_rule() {
    let base = json!({"typology": "office", "floors": 3});
    let mut none = base.clone();
    none["rules"] = json!({"opening.egress_swing": "none"});
    let (_, p) = built(&none);
    let exits: Vec<_> = typed(&p).into_iter().filter(|x| x.2.function == "exit").collect();
    assert!(!exits.is_empty());
    for (st, o, t) in exits {
        assert!(!t.exit && leaf_room(st, o, t).is_some(), "an exit swings into its room when the rule is none");
    }
    // route: large public rooms open towards the corridor while it keeps its width
    let mut route = base.clone();
    route["rules"] = json!({"opening.egress_swing": "route", "opening.out_swing_area": 10, "opening.corridor_clear": 0});
    let (_, p) = built(&route);
    let routed: Vec<_> = typed(&p).into_iter().filter(|x| x.2.rules.contains(&"opening.out_swing_area")).collect();
    assert!(!routed.is_empty(), "no door opens towards the route");
    for (st, o, t) in routed {
        let kind = leaf_room(st, o, t).map(|r| st.rooms[r].kind);
        assert!(matches!(kind, Some(RoomKind::Corridor | RoomKind::Hall | RoomKind::Lobby)), "{kind:?}");
    }
    // the same brief with a corridor that must stay wide: the leaves stay in their rooms
    let mut tight = route.clone();
    tight["rules"]["opening.corridor_clear"] = json!(3.0);
    let (_, p) = built(&tight);
    let kept = typed(&p).iter().filter(|x| x.2.rules.contains(&"opening.corridor_clear")).count();
    assert!(kept > 0, "a 3 m clear corridor leaves no door in it");
    assert!(typed(&p).iter().all(|x| !x.2.rules.contains(&"opening.out_swing_area") || {
        let kind = leaf_room(x.0, x.1, x.2).map(|r| x.0.rooms[r].kind);
        !matches!(kind, Some(RoomKind::Corridor))
    }));
}

#[test]
fn a_country_clear_width_widens_the_doors_on_the_accessible_route() {
    let set = PackSet::embedded();
    let pack = set.resolve("de").unwrap();
    let norms = norms_from(|k| pack.dim_cite(k));
    let need = norms["opening.clear_access"]["value"].as_f64().expect("the pack sets opening.clear_access");
    assert!(need >= 0.9 - TOL, "{need}");
    let dir = suite::crate_dir().join("data/bench");
    let (mut accessible, mut widened, mut short) = (0, 0, 0);
    for name in ["office_4_storeys", "section_5", "school_u", "hotel_preset"] {
        let mut b = suite::read_brief(&dir.join(format!("{name}.json")));
        b.spec["norms"] = norms.clone();
        let (s, p) = built(&b.spec);
        assert_eq!(s.rules.get("opening.clear_access"), need);
        for (_, o, t) in typed(&p).into_iter().filter(|x| x.2.accessible) {
            accessible += 1;
            widened += usize::from(t.widened);
            if t.clear < need - TOL {
                short += 1;
            } else if t.widened {
                assert!(o.width - s.rules.get("opening.leaf_loss") >= need - TOL);
            }
        }
    }
    assert!(accessible > 50 && widened > 0, "accessible {accessible}, widened {widened}");
    assert!(short * 10 <= accessible, "{short} of {accessible} accessible doors are still narrower than {need} m");
}

#[test]
fn exterior_openings_meet_the_thermal_and_sound_rules() {
    let (s, p) = built(&json!({"typology": "office", "floors": 4}));
    let u = s.rules.get("opening.u_max");
    let rw = s.rules.get("opening.rw_exterior") as u32;
    let mut ext = 0;
    for (st, o, t) in typed(&p) {
        if st.layout.walls[o.wall].role == ddc_gen::WallRole::Exterior {
            ext += 1;
            assert!(t.u > 0.0 && t.u <= u + TOL && t.rw >= rw, "{} u {} rw {}", t.mark, t.u, t.rw);
        }
    }
    assert!(ext > 20);
    let mut strict = json!({"typology": "office", "floors": 4});
    strict["rules"] = json!({"opening.u_max": 1.0});
    let (_, q) = built(&strict);
    let outside = |x: &&(&PStorey, &GOpening, &GOpeningType)| x.0.layout.walls[x.1.wall].role == ddc_gen::WallRole::Exterior;
    assert!(typed(&q).iter().filter(outside).all(|x| x.2.u <= 1.0), "triple glazing under U 1.0");
    assert!(typed(&q).iter().any(|x| x.2.catalogue.ends_with(".hp")));
}

#[test]
fn schedules_list_types_with_counts() {
    for (name, _, p) in bench_projects() {
        let rows = schedule(&p);
        let n = typed(&p).len();
        assert_eq!(rows.iter().map(|r| r.count).sum::<usize>(), n, "{name}");
        assert_eq!(n, p.blocks().map(|(_, s)| s.layout.openings.len()).sum::<usize>(), "{name}: untyped openings");
        let mut seen = std::collections::BTreeSet::new();
        assert!(rows.iter().all(|r| seen.insert(r.mark.clone()) && r.count > 0 && r.storeys > 0), "{name}");
        assert!(rows.iter().any(|r| r.door) && rows.iter().any(|r| !r.door), "{name}");
        // doors list before windows
        let first_window = rows.iter().position(|r| !r.door).unwrap();
        assert!(rows[..first_window].iter().all(|r| r.door) && rows[first_window..].iter().all(|r| !r.door), "{name}");
    }
    // a five-storey block shows real types: plain, wet, flat entrance, stair, exit and windows
    let (_, p) = built(&json!({"typology": "apartment_section", "floors": 5}));
    let rows = schedule(&p);
    let doors: Vec<_> = rows.iter().filter(|r| r.door).collect();
    assert!(doors.len() >= 4, "{} door types", doors.len());
    assert!(doors.iter().any(|r| r.fire >= 30) && doors.iter().any(|r| r.fire == 0));
    assert!(doors.iter().any(|r| r.catalogue.starts_with("door.ext")));
    assert!(rows.iter().filter(|r| !r.door).all(|r| r.u > 0.0));
    assert!(rows.iter().any(|r| r.count >= 10), "types repeat over the storeys");
}

#[test]
fn every_choice_is_explained_with_its_rule() {
    let (s, p) = built(&json!({"typology": "apartment_section", "floors": 5}));
    let all = explain_project(&s, &p);
    let mine: Vec<_> = all.iter().filter(|w| w.target == "openings").collect();
    for rule_id in ["opening.leaf_loss", "opening.egress_swing", "opening.fire_stair", "opening.fire_unit", "opening.u_max"] {
        let w = mine.iter().find(|w| w.rule == rule_id).unwrap_or_else(|| panic!("no reason for {rule_id}"));
        assert!(w.effect.chars().any(|c| c.is_ascii_digit()) && !w.effect.contains('{'), "{}", w.effect);
    }
    // each reason's rule value is the rule's value in the spec
    for w in &mine {
        if let Some(v) = w.value.as_f64() {
            assert_eq!(scoped(&s.rules, || rule(&w.rule)), v, "{}", w.rule);
        }
    }
}
