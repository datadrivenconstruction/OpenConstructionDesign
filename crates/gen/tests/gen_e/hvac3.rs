// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Hospital air by care class, and the service fixtures of storeys and basements in the slot
//! blocks `storey_block` gives them.

use ddc_gen::project::care::{self, Care};
use ddc_gen::project::rules::rule;
use ddc_gen::project::{build_project, parse_spec, slots, RoomKind, RoomTags};
use serde_json::json;
use std::collections::BTreeSet;

fn tags(ids: &[&str]) -> RoomTags {
    ids.iter().fold(RoomTags::default(), |a, i| a.with(RoomTags::parse(i).unwrap()))
}

#[test]
fn care_classes_come_from_tags_and_beat_each_other_in_order() {
    use RoomKind::*;
    assert_eq!(care::class_of(Technical, tags(&["operating"])), Some(Care::Operating));
    assert_eq!(care::class_of(Ward, tags(&["isolation"])), Some(Care::Isolation));
    assert_eq!(care::class_of(Ward, tags(&["isolation", "operating"])), Some(Care::Operating));
    assert_eq!(care::class_of(Ward, tags(&["clinical"])), Some(Care::Ward));
    assert_eq!(care::class_of(Office, tags(&["clinical"])), None, "only a ward takes the ward floor");
    assert_eq!(care::class_of(Ward, RoomTags::default()), None);
    assert_eq!(care::air(Ward, RoomTags::default(), 20.0, 2.7), None);
}

#[test]
fn pressure_and_air_changes_follow_the_rules() {
    let (area, h) = (36.0, 2.8);
    let vol = area * h;
    let (so, eo) = care::air(RoomKind::Technical, tags(&["operating"]), area, h).unwrap();
    assert!((so - rule("services.hvac.ach_operating") * vol).abs() < 1e-9);
    assert!(eo < so, "a theatre stays at positive pressure: {so} in, {eo} out");
    let (si, ei) = care::air(RoomKind::Ward, tags(&["isolation"]), area, h).unwrap();
    assert!((si - rule("services.hvac.ach_isolation") * vol).abs() < 1e-9);
    assert!(ei > si, "an isolation room stays at negative pressure: {si} in, {ei} out");
    let (sw, ew) = care::air(RoomKind::Ward, tags(&["clinical"]), area, h).unwrap();
    assert!(sw >= rule("services.hvac.ach_ward") * vol - 1e-9 && sw >= rule("services.hvac.rate_ward") * area - 1e-9);
    assert_eq!(sw, ew);
    // a theatre moves several times the air of a ward, an isolation room more than a ward
    assert!(so > si && si > sw, "{so} {si} {sw}");
}

#[test]
fn the_tags_are_asked_in_a_brief_and_unknown_ones_refused() {
    let ok = json!({"typology": "office", "floors": 1, "rooms": [
        {"kind": "technical", "area": 30, "name": "Theatre 1", "tags": ["operating"]},
        {"kind": "ward", "area": 20, "name": "Bay", "tags": ["isolation", "clinical"]}]});
    let s = parse_spec(&ok).unwrap();
    assert_eq!(s.room_tags.of(RoomKind::Technical, "Theatre 1"), tags(&["operating"]));
    assert_eq!(s.room_tags.of(RoomKind::Ward, "Bay"), tags(&["isolation", "clinical"]));
    let bad = json!({"typology": "office", "floors": 1, "rooms": [{"kind": "ward", "area": 20, "tags": ["sterile"]}]});
    assert!(parse_spec(&bad).is_err());
}

#[test]
fn every_care_rule_has_a_default_inside_its_range() {
    for id in [
        "services.hvac.ach_ward",
        "services.hvac.ach_operating",
        "services.hvac.operating_extract_ratio",
        "services.hvac.ach_isolation",
        "services.hvac.isolation_extract_ratio",
    ] {
        let d = ddc_gen::project::rules::def(id).unwrap_or_else(|| panic!("{id}"));
        let v = d.default.unwrap_or_else(|| panic!("{id} has no default"));
        assert!(d.min <= v && v <= d.max && !d.why.is_empty(), "{id}");
    }
}

#[test]
fn services_of_storeys_and_basements_never_meet_in_a_fixture_block() {
    // 39 floors over 2 basements: the top storeys' blocks move into the basements' low blocks
    let v = json!({"typology": "office", "floors": 39, "basements": 2, "sprinklers": true});
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    let nb = p.basements.len();
    assert_eq!(nb, 2);
    let blocks: Vec<(u32, usize)> = p.blocks().map(|(k, s)| (k, s.layout.fixtures.len())).collect();
    assert_eq!(blocks.len(), p.storeys.len() + nb);
    assert_eq!(blocks.iter().map(|b| b.0).collect::<BTreeSet<_>>().len(), blocks.len(), "one block each");
    let mut slot = BTreeSet::new();
    for (k, n) in &blocks {
        assert!(*n <= slots::FIXTURES_MAX as usize, "block {k}: {n} fixtures");
        for i in 0..*n as u32 {
            assert!(slot.insert(slots::fixture_base(*k) + i), "block {k}: fixture {i} shares a slot");
        }
    }
    // the storeys the mapping moved still carry their own air terminals and heads
    let moved = (0..p.storeys.len()).filter(|&k| slots::storey_block(k, nb) as usize != k).count();
    assert!(moved > 0, "the case moves storeys");
    for (k, s) in p.storeys.iter().enumerate() {
        let at = slots::storey_block(k, nb);
        let (_, n) = blocks.iter().find(|b| b.0 == at).unwrap();
        assert_eq!(*n, s.layout.fixtures.len(), "storey {k} in block {at}");
    }
}

#[test]
fn the_building_air_system_leaves_the_data_centre_rooms_to_their_cooling_units() {
    use ddc_gen::project::mech::loads::room_loads;
    let brief = |tag: bool| {
        let mut hall = json!({"kind": "office", "area": 80, "name": "Hall 1"});
        if tag {
            hall["tags"] = json!(["data_hall"]);
        }
        json!({"typology": "office", "floors": 1, "rooms": [hall, {"kind": "office", "area": 40, "name": "Desk"}]})
    };
    let served = |tag: bool| -> Vec<String> {
        let s = parse_spec(&brief(tag)).unwrap();
        let p = build_project(&s).unwrap();
        room_loads(&s, false, &p)
            .iter()
            .map(|l| p.storeys[l.storey].layout.rooms[l.room].name.clone())
            .collect()
    };
    assert!(served(false).iter().any(|n| n == "Hall 1"), "an untagged hall is an office to the air system");
    let with = served(true);
    assert!(!with.iter().any(|n| n == "Hall 1") && with.iter().any(|n| n == "Desk"), "{with:?}");
}

#[test]
fn no_two_systems_run_through_each_other_in_a_ten_storey_hospital() {
    use ddc_gen::project::void_check;
    let ward = |n: &str| json!({"kind": "ward", "area": 22, "count": 4, "name": n, "tags": ["clinical"]});
    let v = json!({"typology": "office", "floors": 10, "services": true, "sprinklers": true,
        "rooms": [ward("Ward"), {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                  {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                  {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]});
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    assert_eq!(p.storeys.len(), 10);
    assert!(!p.hvac.ducts.is_empty() && !p.fire.runs.is_empty() && !p.services.water.is_empty() && !p.services.elec.is_empty());
    let clashes = void_check(&p);
    assert!(clashes.is_empty(), "{} clashes, first: {:?}", clashes.len(), clashes.first());
}

#[test]
fn the_void_layers_keep_a_ten_storey_hospital_clash_free() {
    use ddc_gen::project::void_check;
    let ward = |n: &str| json!({"kind": "ward", "area": 22, "count": 4, "name": n, "tags": ["clinical"]});
    let v = json!({"typology": "office", "floors": 10, "services": true, "sprinklers": true,
        "rules": {"mep.layers": true, "mep.hvac.system": "auto"},
        "rooms": [ward("Ward"), {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                  {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                  {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]});
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    assert!(!p.hvac.ducts.is_empty() && !p.fire.runs.is_empty() && !p.services.water.is_empty() && !p.services.elec.is_empty());
    let clashes = void_check(&p);
    assert!(clashes.is_empty(), "{} clashes, first: {:?}", clashes.len(), clashes.first());
}

/// The evals2 oracles for a hospital: every air terminal sits within 1.5 m of a duct whose
/// connected network (ends within 0.35 m of a run of the same system) reaches an air handling unit
/// (a run end within 3 m), and every room has a terminal on its level within 6 m.
#[test]
fn every_air_terminal_of_a_hospital_is_fed_and_every_room_served() {
    type P = (f64, f64, f64);
    let d = |a: P, b: P| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt();
    let seg = |a: P, b: P, q: P| {
        let (v, w) = ((b.0 - a.0, b.1 - a.1, b.2 - a.2), (q.0 - a.0, q.1 - a.1, q.2 - a.2));
        let l2 = v.0 * v.0 + v.1 * v.1 + v.2 * v.2;
        let t = if l2 < 1e-12 { 0.0 } else { ((w.0 * v.0 + w.1 * v.1 + w.2 * v.2) / l2).clamp(0.0, 1.0) };
        d((a.0 + v.0 * t, a.1 + v.1 * t, a.2 + v.2 * t), q)
    };
    let near = |r: &[P], q: P, tol: f64| r.windows(2).any(|w| seg(w[0], w[1], q) <= tol) || r.iter().any(|x| d(*x, q) <= tol);
    let ward = |n: &str| json!({"kind": "ward", "area": 22, "count": 4, "name": n, "tags": ["clinical"]});
    let v = json!({"typology": "office", "floors": 10, "services": true, "sprinklers": true,
        "rooms": [ward("Ward"), {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                  {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                  {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]});
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    let ducts = &p.hvac.ducts;
    let n = ducts.len();
    let mut root: Vec<usize> = (0..n).collect();
    fn find(r: &mut Vec<usize>, a: usize) -> usize {
        let mut x = a;
        while r[x] != x {
            x = r[x];
        }
        let mut y = a;
        while r[y] != x {
            let nx = r[y];
            r[y] = x;
            y = nx;
        }
        x
    }
    for i in 0..n {
        for e in [ducts[i].points.first(), ducts[i].points.last()].into_iter().flatten() {
            for j in 0..n {
                if ducts[j].sys == ducts[i].sys && near(&ducts[j].points, *e, 0.35) {
                    let (a, b) = (find(&mut root, i), find(&mut root, j));
                    root[a.max(b)] = a.min(b);
                }
            }
        }
    }
    let roof = p.roof.elevation;
    let units: Vec<P> = p.hvac.plant.iter().map(|u| (u.at.0, u.at.1, roof + u.z)).collect();
    assert!(!units.is_empty(), "no air handling unit");
    let fed: std::collections::HashSet<usize> = (0..n)
        .filter(|&i| [ducts[i].points.first(), ducts[i].points.last()].into_iter().flatten().any(|e| units.iter().any(|u| d(*u, *e) <= 3.0)))
        .map(|i| find(&mut root, i))
        .collect();
    let (mut all, mut orphans) = (0, vec![]);
    for (k, st) in p.storeys.iter().enumerate() {
        for f in st.layout.fixtures.iter().filter(|f| matches!(f.family, "Supply Diffuser" | "Exhaust Grille")) {
            all += 1;
            let q = (f.at.0, f.at.1, st.elevation + f.z);
            if !(0..n).any(|j| near(&ducts[j].points, q, 1.5) && fed.contains(&find(&mut root, j))) {
                orphans.push(format!("{} on storey {k} at {:.1},{:.1}", f.family, f.at.0, f.at.1));
            }
        }
    }
    assert!(all > 0 && orphans.is_empty(), "{} of {all} terminals unfed, first: {:?}", orphans.len(), orphans.first());
    let mut bare = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        for r in &st.layout.rooms {
            let served = st.layout.fixtures.iter().any(|f| {
                matches!(f.family, "Supply Diffuser" | "Exhaust Grille") && (f.at.0 - r.point.0).hypot(f.at.1 - r.point.1) <= 6.0
            });
            if !served && !r.name.to_lowercase().contains("shaft") {
                bare.push(format!("{} on storey {k}", r.name));
            }
        }
    }
    assert!(p.storeys.iter().any(|st| st.layout.rooms.iter().any(|r| r.name.contains("Theatre"))), "no theatre in the layout");
    let theatre: Vec<_> = bare.iter().filter(|b| b.contains("Theatre")).collect();
    assert!(theatre.is_empty(), "operating rooms without a terminal: {theatre:?}");
}

#[test]
fn a_room_tagged_operating_gets_more_air_in_the_project() {
    let brief = |tag: bool| {
        let mut r = json!({"kind": "office", "area": 40, "name": "Room A"});
        if tag {
            r["tags"] = json!(["operating"]);
        }
        json!({"typology": "office", "floors": 2, "rooms": [r, {"kind": "office", "area": 30, "name": "Desk"}]})
    };
    let flow = |tag: bool| {
        let p = build_project(&parse_spec(&brief(tag)).unwrap()).unwrap();
        assert!(!p.hvac.ducts.is_empty(), "the project is ventilated");
        p.hvac.flow
    };
    let (plain, theatre) = (flow(false), flow(true));
    assert!(theatre.0 > plain.0 + 1.0, "supply {} against {}", theatre.0, plain.0);
}

#[test]
fn void_layers_are_off_by_default_and_ordered() {
    use ddc_gen::project::rules::{def, rule};
    assert_eq!(rule("mep.layers"), 0.0);
    assert!(def("mep.layers").unwrap().flag);
    let d = |k: &str| rule(&format!("mep.layer.{k}"));
    assert!(d("sprinkler") < d("tray") && d("tray") < d("water") && d("water") < d("heating") && d("heating") < d("chilled"));
}

/// The air handling unit on the roof keeps its whole outline out of the stair heads and their
/// clearance (`fire.stair_head.plant_clear`). Against the run with no clearance it must move or
/// turn where that run stood in the way; where no place is left, the project says why.
#[test]
fn the_roof_unit_keeps_clear_of_the_stair_heads() {
    type Box2 = ((f64, f64), (f64, f64));
    let built = |clear: f64| {
        let v = json!({"typology": "office", "floors": 10, "services": true,
            "rules": {"fire.stair_head.plant_clear": clear}});
        let s = parse_spec(&v).unwrap();
        let p = build_project(&s).unwrap();
        (s, p)
    };
    // the unit's outline as its plan box: the type's length along its own axis, turned
    let outline = |u: &ddc_gen::GFixture| -> Box2 {
        let k = match u.ty {
            "900x600" => "s",
            "2000x1000" => "m",
            _ => "l",
        };
        let (l, w) = (rule(&format!("services.hvac.unit_{k}_l")), rule(&format!("services.hvac.unit_{k}_w")));
        let (c, s) = (u.rotation.cos().abs(), u.rotation.sin().abs());
        let (hx, hy) = ((l * c + w * s) / 2.0, (l * s + w * c) / 2.0);
        ((u.at.0 - hx, u.at.1 - hy), (u.at.0 + hx, u.at.1 + hy))
    };
    let hits = |a: Box2, b: Box2| a.0 .0 < b.1 .0 && a.1 .0 > b.0 .0 && a.0 .1 < b.1 .1 && a.1 .1 > b.0 .1;
    let boxes = |p: &ddc_gen::project::GProject, clear: f64| -> Vec<Box2> {
        p.fire
            .stair_heads
            .iter()
            .map(|h| {
                let lo = h.ring.iter().fold((f64::MAX, f64::MAX), |a, q| (a.0.min(q.0), a.1.min(q.1)));
                let hi = h.ring.iter().fold((f64::MIN, f64::MIN), |a, q| (a.0.max(q.0), a.1.max(q.1)));
                ((lo.0 - clear, lo.1 - clear), (hi.0 + clear, hi.1 + clear))
            })
            .collect()
    };
    let (_, free) = built(0.0);
    assert!(!free.fire.stair_heads.is_empty(), "a 10-storey office carries its stairs to the roof");
    assert!(!free.hvac.plant.is_empty(), "without clearance the unit is placed");
    let mut forced = 0;
    for clear in [1.0, 3.0, 5.0] {
        let (s, p) = built(clear);
        let grown = boxes(&p, clear);
        let was_in_the_way = free.hvac.plant.iter().any(|u| grown.iter().any(|b| hits(outline(u), *b)));
        for u in &p.hvac.plant {
            assert!(!grown.iter().any(|b| hits(outline(u), *b)), "clear {clear}: unit at {:?} touches a stair head or its clearance", u.at);
        }
        if was_in_the_way {
            forced += 1;
            let moved = p.hvac.plant.iter().any(|u| free.hvac.plant.iter().all(|f| f.at != u.at || f.rotation != u.rotation));
            if p.hvac.plant.is_empty() {
                let g = ddc_gen::project::check_project(&s, &p);
                let refused = p.metrics.iter().any(|m| m.0 == "hvac_refused_stair_head" && m.1 > 0.0);
                assert!(refused && g.warnings.iter().any(|w| w.contains("fire.stair_head.plant_clear")), "clear {clear}: no unit and no reason: {:?}", g.warnings);
            } else {
                assert!(moved, "clear {clear}: the unit stayed where the stair head stands");
            }
        }
    }
    assert!(forced >= 1, "no clearance of 1, 3 or 5 m put the unit in the way: the case proves nothing");
    assert!(!built(1.0).1.hvac.plant.is_empty(), "the default clearance leaves room for the unit");
}

fn hospital(rules: serde_json::Value, height: f64) -> serde_json::Value {
    let ward = |n: &str| json!({"kind": "ward", "area": 22, "count": 4, "name": n, "tags": ["clinical"]});
    json!({"typology": "office", "floors": 4, "floor_height": height, "services": true, "rules": rules,
        "rooms": [ward("Ward"), {"kind": "technical", "area": 36, "name": "Theatre", "tags": ["operating"]},
                  {"kind": "ward", "area": 20, "name": "Isolation", "tags": ["isolation"]},
                  {"kind": "office", "area": 18, "count": 2}, {"kind": "wc", "area": 4, "count": 2}]})
}

/// The air terminals of the linked fixtures: (storey, network, whether the room is tagged).
fn terminal_rooms(s: &ddc_gen::project::ProjectSpec, p: &ddc_gen::project::GProject) -> Vec<(usize, u8, bool)> {
    let dedicated = tags(&["operating", "isolation"]);
    let n = p.storeys.len();
    p.hvac
        .links
        .iter()
        .filter(|l| l.storey < n && l.connector == "Air")
        .map(|l| {
            let st = &p.storeys[l.storey];
            let at = st.layout.fixtures[l.fixture].at;
            let room = st.rooms.iter().zip(&st.layout.rooms).find(|(_, gr)| {
                let (x0, x1) = gr.polygon.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
                let (y0, y1) = gr.polygon.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
                at.0 > x0 && at.0 < x1 && at.1 > y0 && at.1 < y1
            });
            let tagged = room.is_some_and(|(pr, gr)| s.room_tags.of(pr.kind, &gr.name).meets(dedicated));
            (l.storey, l.net, tagged)
        })
        .collect()
}

#[test]
fn operating_and_isolation_rooms_have_their_own_outdoor_air_unit() {
    use ddc_gen::project::{check_project, void_check};
    let v = hospital(json!({"mep.hvac.dedicated_oa": "operating_isolation"}), 4.2);
    let s = parse_spec(&v).unwrap();
    let p = build_project(&s).unwrap();
    assert!(check_project(&s, &p).passed());
    let n = p.storeys.len();
    assert_eq!(p.hvac.plant.len(), 2, "a general unit and a dedicated one");
    assert_eq!(p.hvac.nets.len(), 1);
    // each unit is joined to its own network, by plant index
    for (net, unit) in [(0u8, 0usize), (1, 1)] {
        let own: Vec<_> = p.hvac.links.iter().filter(|l| l.storey == n && l.net == net).collect();
        assert_eq!(own.len(), 2, "supply and return of network {net}");
        assert!(own.iter().all(|l| l.fixture == unit), "{own:?}");
        assert!(p.hvac.ducts.iter().any(|d| d.net == net && d.storey == n), "plant ducts of network {net}");
    }
    let rooms = terminal_rooms(&s, &p);
    assert!(rooms.iter().any(|r| r.2) && rooms.iter().any(|r| !r.2));
    assert!(rooms.iter().all(|r| (r.1 == 1) == r.2), "tagged rooms are served by the dedicated unit alone: {rooms:?}");
    // a shaft per system, the duct runs of the two never meet
    let shafts = p.storeys[0].layout.shafts.iter().filter(|x| x.kind == ddc_gen::ShaftKind::Duct).count();
    assert_eq!(shafts, 2);
    let ends = |net: u8| -> Vec<(i64, i64, i64)> {
        p.hvac
            .ducts
            .iter()
            .filter(|d| d.net == net)
            .flat_map(|d| [d.points[0], *d.points.last().unwrap()])
            .map(|q| ((q.0 * 100.0).round() as i64, (q.1 * 100.0).round() as i64, (q.2 * 100.0).round() as i64))
            .collect()
    };
    let (a, b) = (ends(0), ends(1));
    assert!(a.iter().all(|e| !b.contains(e)), "the two networks touch");
    let clashes = void_check(&p);
    assert!(clashes.is_empty(), "{} clashes, first: {:?}", clashes.len(), clashes.first());
}

#[test]
fn without_the_rule_or_without_room_one_unit_serves_all() {
    let off = build_project(&parse_spec(&hospital(json!({}), 4.2)).unwrap()).unwrap();
    assert_eq!(off.hvac.plant.len(), 1);
    assert!(off.hvac.nets.is_empty() && off.hvac.ducts.iter().all(|d| d.net == 0));
    // a low storey has no void for a second set of mains: the tagged rooms stay on the one unit
    let v = hospital(json!({"mep.hvac.dedicated_oa": "operating_isolation"}), 3.0);
    let s = parse_spec(&v).unwrap();
    let p = build_project(&s).unwrap();
    assert_eq!(p.hvac.plant.len(), 1);
    assert!(terminal_rooms(&s, &p).iter().any(|r| r.2 && r.1 == 0), "the theatre is still served");
}

#[test]
fn the_mechanical_design_follows_both_units_and_both_shafts() {
    use ddc_gen::project::mech::{self, equip::Kind};
    let v = hospital(json!({"mep.hvac.dedicated_oa": "operating_isolation", "mep.hvac.system": "auto"}), 4.2);
    let s = parse_spec(&v).unwrap();
    let p = build_project(&s).unwrap();
    assert_eq!(p.hvac.plant.len(), 2);
    let m = mech::design(&s, &p).expect("the rules ask for the design");
    assert_eq!(m.count(Kind::Ahu), 2);
    assert!(m.equip[..2].iter().all(|e| e.kind == Kind::Ahu), "the units lead the equipment");
    assert_eq!(m.equip[1].net, 1);
    assert_eq!(mech::unconnected(&m), 0);
    let lost: Vec<_> = m.air.faults.iter().filter(|f| f.contains("not reached") || f.contains("not connected")).collect();
    assert!(lost.is_empty(), "{lost:?}");
    // every run of the dedicated network is reached from its own unit, and has its own fire damper
    assert!(m.air.runs.iter().any(|r| r.net == 1));
    let fire = |net: u8| m.air.dampers.iter().filter(|d| d.kind == Kind::FireDamper && d.net == net).count();
    assert!(fire(0) >= 1 && fire(1) >= 1, "fire dampers {} and {}", fire(0), fire(1));
    let rooms = terminal_rooms(&s, &p);
    assert!(m.air.terms.iter().all(|t| t.run.is_some()), "{} terminals, {} rooms", m.air.terms.len(), rooms.len());
}
