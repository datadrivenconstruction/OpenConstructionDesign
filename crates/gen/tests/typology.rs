//! Data-driven typologies: each shipped definition builds a valid project whose loads are read
//! from the rooms that were built, the rules steer the loads, and the four older typologies
//! stay as they were.

use ddc_gen::project::presets::Preset;
use ddc_gen::project::typology::Model;
use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, check_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

const NEW: [&str; 4] = ["hospital_wing", "data_center", "underground_station", "process_plant_site"];

fn build(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap_or_else(|e| panic!("{v}: {e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
    let g = check_project(&s, &p);
    assert!(g.passed(), "{v}: {:?}", g.failures);
    (s, p)
}

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == k).unwrap_or_else(|| panic!("metric {k}")).1
}

fn names(p: &GProject) -> Vec<String> {
    p.storeys.iter().chain(&p.basements).flat_map(|s| s.layout.rooms.iter().map(|r| r.name.clone())).collect()
}

#[test]
fn every_shipped_typology_builds_and_passes_the_gates() {
    for t in NEW {
        let (_, p) = build(&json!({"typology": t}));
        for k in ["typ_rooms", "typ_supply_m3h", "typ_power_kw"] {
            assert!(metric(&p, k) > 0.0, "{t}: {k}");
        }
    }
}

#[test]
fn the_loads_are_those_of_the_built_rooms() {
    let (s, p) = build(&json!({"typology": "hospital_wing"}));
    let m = s.model.as_ref().unwrap();
    let built = ddc_gen::project::typology::schedule_of(&s, &p).unwrap();
    let t = built.totals();
    assert_eq!(f64::from(t.rooms), metric(&p, "typ_rooms"));
    let described = p
        .storeys
        .iter()
        .chain(&p.basements)
        .flat_map(|st| st.rooms.iter().zip(&st.layout.rooms))
        .filter(|(pr, gr)| m.of(pr.kind, &gr.name).is_some())
        .count();
    assert_eq!(described as u32, t.rooms);
}

#[test]
fn rooms_carry_the_names_of_the_typology() {
    let all = |t: &str| names(&build(&json!({"typology": t})).1);
    let hosp = all("hospital_wing");
    for n in ["Operating theatre", "Intensive care room", "Patient room", "Isolation room"] {
        assert!(hosp.iter().any(|x| x == n), "{n} in {hosp:?}");
    }
    let st = all("underground_station");
    for n in ["Platform", "Concourse", "Tunnel ventilation fan room"] {
        assert!(st.iter().any(|x| x == n), "{n} in {st:?}");
    }
    assert!(all("data_center").iter().any(|x| x == "Data hall"));
    assert!(all("process_plant_site").iter().any(|x| x == "Control room"));
}

#[test]
fn the_station_goes_deep_and_holds_back_the_soil() {
    let (s, p) = build(&json!({"typology": "underground_station"}));
    assert_eq!(p.basements.len(), 3);
    let low = p.basements.iter().map(|b| b.elevation).fold(0.0, f64::min);
    assert!(low <= -12.0, "{low}");
    let walls = p.basements.iter().flat_map(|b| &b.layout.walls).filter(|w| w.role == ddc_gen::WallRole::Retaining).count();
    assert!(walls > 0 && metric(&p, "typ_retaining_walls") == walls as f64);
    assert_eq!(metric(&p, "typ_retaining_t"), s.rules.get("underground.retaining_thickness"));
    assert!(p.basements[1].layout.rooms.iter().any(|r| r.name == "Trackway"));
    assert!(p.basements.iter().all(|b| !b.layout.stairs.is_empty()));
}

#[test]
fn a_rule_steers_the_loads_and_the_thickness() {
    let base = build(&json!({"typology": "hospital_wing"})).1;
    let more = build(&json!({"typology": "hospital_wing", "rules": {"typology.icu.ach": 30}})).1;
    assert!(metric(&more, "typ_supply_m3h") > metric(&base, "typ_supply_m3h"));
    let thick = build(&json!({"typology": "underground_station", "rules": {"underground.retaining_thickness": 1.0}})).1;
    assert_eq!(metric(&thick, "typ_retaining_t"), 1.0);
}

#[test]
fn the_why_names_the_rules_behind_the_typology() {
    for t in NEW {
        let (s, p) = build(&json!({"typology": t}));
        let why = explain_project(&s, &p);
        let typ: Vec<_> = why.iter().filter(|w| w.target == "typology").collect();
        assert!(typ.len() >= 3, "{t}: {typ:?}");
        assert!(typ.iter().all(|w| !w.effect.is_empty() && w.rule.starts_with(['t', 'g'])), "{t}");
    }
}

#[test]
fn the_model_and_the_preset_round_trip() {
    for t in NEW {
        let pre = ddc_gen::project::presets::of_brief(&json!({"typology": t})).unwrap().unwrap().into_owned();
        let model = pre.model.clone().expect(t);
        let again = Model::from_json(&model.to_json()).unwrap();
        assert_eq!(again.to_json(), model.to_json(), "{t}");
        let saved = Preset::from_json(&pre.to_json()).unwrap();
        assert_eq!(saved.to_json(), pre.to_json(), "{t}");
        assert_eq!(saved.programme(), pre.programme(), "{t}");
    }
}

#[test]
fn a_program_the_user_edited_survives_a_save() {
    let mut j = ddc_gen::project::presets::of_brief(&json!({"typology": "data_center"})).unwrap().unwrap().to_json();
    j["program"] = json!({"spaces": [{"kind": "office", "area": 30, "count": 3, "floor": "ground"}]});
    let pre = Preset::from_json(&j).unwrap();
    assert_eq!(Preset::from_json(&pre.to_json()).unwrap().programme(), pre.programme());
    assert_ne!(pre.programme(), ddc_gen::project::presets::of_brief(&json!({"typology": "data_center"})).unwrap().unwrap().programme());
}

#[test]
fn bad_models_are_refused_with_the_path() {
    let mut j = ddc_gen::project::presets::of_brief(&json!({"typology": "data_center"})).unwrap().unwrap().to_json();
    j["model"]["spaces"][0]["area"] = json!(-4);
    let e = Preset::from_json(&j).unwrap_err();
    assert!(e.contains("spaces"), "{e}");
    j["model"]["spaces"] = json!([]);
    assert!(Preset::from_json(&j).is_err());
}

#[test]
fn the_older_typologies_carry_no_model() {
    for t in ["house", "school", "apartment_section", "office"] {
        let (s, p) = build(&json!({"typology": t}));
        assert!(s.model.is_none(), "{t}");
        assert!(p.metrics.iter().all(|m| !m.0.starts_with("typ_")), "{t}");
        assert!(explain_project(&s, &p).iter().all(|w| w.target != "typology"), "{t}");
    }
}

#[test]
fn care_air_follows_the_tags_and_the_model_wins_over_the_generic_rate() {
    let (s, p) = build(&json!({"typology": "hospital_wing"}));
    let ops = s.room_tags.of(ddc_gen::project::RoomKind::Technical, "Operating theatre");
    assert!(ops.meets(ddc_gen::project::RoomTags::parse("operating").unwrap()));
    // the operating air comes from the norm rule, so changing it changes the loads
    let more = build(&json!({"typology": "hospital_wing", "rules": {"services.hvac.ach_operating": 30}})).1;
    assert!(metric(&more, "typ_supply_m3h") > metric(&p, "typ_supply_m3h"));
    // pressure holds: the lower side is under the higher one for every pressurised class
    let m = s.model.as_ref().unwrap();
    let clear = s.floor_height - s.rules.get("services.slab");
    for sp in m.spaces.iter().filter(|x| x.demand.pressure.is_some()) {
        if let Some((a, b)) = m.air(sp.kind, &sp.name.0, 20.0, clear, &s.rules) {
            assert!(a != b, "{}: {a} {b}", sp.name.0);
        }
    }
    // the model's own rate takes priority over the generic one for a class that states it
    let icu = m.air(ddc_gen::project::RoomKind::Ward, "Intensive care room", 24.0, clear, &s.rules).unwrap();
    assert!(icu.0 >= s.rules.get("typology.icu.ach") * 24.0 * clear - 1e-6);
}

#[test]
fn the_air_scale_never_goes_below_the_norm() {
    let r = ddc_gen::project::rules::def("typology.air.scale").unwrap();
    assert!(r.min >= 1.0);
}

fn count(p: &GProject, name: &str) -> usize {
    names(p).iter().filter(|n| n.as_str() == name).count()
}

#[test]
fn the_school_builds_its_classrooms_labs_and_halls() {
    let (_, p) = build(&json!({"typology": "secondary_school"}));
    assert!(count(&p, "Classroom") >= 6, "{:?}", names(&p));
    for n in ["Science laboratory", "Sports hall", "School canteen", "Library"] {
        assert!(count(&p, n) >= 1, "{n}");
    }
    assert!(metric(&p, "typ_persons") > 0.0 && metric(&p, "typ_supply_m3h") > 0.0);
}

#[test]
fn light_industry_builds_halls_store_and_loading() {
    let (_, p) = build(&json!({"typology": "light_industrial"}));
    assert!(count(&p, "Production hall") >= 1, "{:?}", names(&p));
    for n in ["Goods store", "Loading bay", "Electrical room"] {
        assert!(count(&p, n) >= 1, "{n}");
    }
    assert!(metric(&p, "typ_power_kw") > 0.0);
}

#[test]
fn the_plant_hall_builds_hall_pumps_boiler_and_switchgear() {
    let (_, p) = build(&json!({"typology": "plant_hall"}));
    for n in ["Plant hall", "Pump house", "Boiler room", "Switchgear room", "Control room"] {
        assert!(count(&p, n) >= 1, "{n} in {:?}", names(&p));
    }
    assert!(metric(&p, "typ_cooling_kw") > 0.0);
}

fn flats(p: &GProject, k: usize) -> usize {
    let mut ids: Vec<u32> = p.storeys[k].layout.rooms.iter().filter_map(|r| r.apartment).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

fn area(poly: &[(f64, f64)]) -> f64 {
    let n = poly.len();
    (0..n).map(|i| poly[i].0 * poly[(i + 1) % n].1 - poly[(i + 1) % n].0 * poly[i].1).sum::<f64>().abs() / 2.0
}

#[test]
fn the_tower_has_refuge_floors_by_rule_and_flats_by_the_unit_mix() {
    let (s, p) = build(&json!({"typology": "residential_tower"}));
    assert_eq!(p.storeys.len(), 40);
    let every = s.rules.get("typology.refuge.every") as usize;
    let refuge: Vec<usize> = (0..40).filter(|k| p.storeys[*k].layout.rooms.iter().any(|r| r.name == "Refuge area")).collect();
    assert_eq!(refuge, (1..40).filter(|k| (k + 1) % every == 0 && k + 1 < 40).collect::<Vec<_>>());
    // the refuge floor holds no apartment
    assert!(refuge.iter().all(|k| flats(&p, *k) == 0 && p.storeys[*k].layout.rooms.iter().all(|r| r.apartment.is_none())));
    let per = (s.rules.get("typology.mix.one") + s.rules.get("typology.mix.two") + s.rules.get("typology.mix.three")) as usize;
    assert_eq!(flats(&p, 1), per);
    assert_eq!(flats(&p, 2), per);
    assert!(p.storeys[0].layout.stairs.len() >= 2 && p.storeys[1].layout.stairs.len() >= 2);
}

#[test]
fn a_two_bedroom_unit_has_two_bedrooms_of_at_least_the_minimum_area() {
    let (s, p) = build(&json!({"typology": "residential_tower"}));
    let st = &p.storeys[1];
    // the unit asks 13 m2 a bedroom; the plan may miss an asked area by `room.area_error` at most
    let min = 13.0 * (1.0 - s.rules.get("room.area_error"));
    let mut two = 0;
    let ids: std::collections::BTreeSet<u32> = st.layout.rooms.iter().filter_map(|r| r.apartment).collect();
    for id in ids {
        let rooms: Vec<_> = st.layout.rooms.iter().filter(|r| r.apartment == Some(id)).collect();
        let beds: Vec<_> = rooms.iter().filter(|r| r.name == "Bedroom").collect();
        for kind in ["Living room", "Kitchen"] {
            assert!(rooms.iter().any(|r| r.name == kind), "flat {id}: {kind} in {:?}", rooms.iter().map(|r| &r.name).collect::<Vec<_>>());
        }
        if beds.len() == 2 {
            two += 1;
            assert!(beds.iter().all(|b| area(&b.polygon) >= min - 1e-6), "flat {id}: {:?}", beds.iter().map(|b| area(&b.polygon)).collect::<Vec<_>>());
        }
    }
    assert_eq!(two as f64, s.rules.get("typology.mix.two"));
}

#[test]
fn the_tower_follows_the_rules_for_refuge_and_mix() {
    let (_, p) = build(&json!({"typology": "residential_tower", "rules": {"typology.refuge.every": 10, "typology.mix.two": 4}}));
    let refuge = (0..40).filter(|k| p.storeys[*k].layout.rooms.iter().any(|r| r.name == "Refuge area")).count();
    assert_eq!(refuge, 3);
    assert_eq!(flats(&p, 1), 4 + 4 + 2);
}

#[test]
fn the_podium_has_shops_on_the_ground_floor_and_parking_below() {
    let (_, p) = build(&json!({"typology": "mixed_use_podium"}));
    let on = |st: &ddc_gen::project::PStorey, n: &str| st.layout.rooms.iter().filter(|r| r.name == n).count();
    assert!(on(&p.storeys[0], "Shop") >= 3 && on(&p.storeys[0], "Supermarket") == 1);
    assert_eq!(p.basements.len(), 2);
    for (i, st) in p.basements.iter().enumerate() {
        assert!(st.layout.rooms.iter().any(|r| r.name.starts_with("Car park")), "level {i}");
    }
    assert!(p.storeys[2].layout.rooms.iter().all(|r| r.name != "Shop"));
}

#[test]
fn the_tower_takes_its_stair_protection_from_the_fire_rule() {
    let (s, p) = build(&json!({"typology": "residential_tower"}));
    assert_eq!(s.rules.get("egress.protected_stairs"), 2.0, "smokeproof from the typology");
    let rated = |k: usize| p.storeys[k].layout.walls.iter().filter(|w| w.fire.is_some_and(|f| f.sep == ddc_gen::FireSep::Stair)).count();
    assert!(rated(1) >= 6, "{}", rated(1));
    let open = build(&json!({"typology": "residential_tower", "rules": {"egress.protected_stairs": "none"}})).1;
    assert_eq!(open.storeys[1].layout.walls.iter().filter(|w| w.fire.is_some()).count(), 0);
}

#[test]
fn an_oversized_unit_mix_is_refused_with_the_rule_to_steer() {
    let e = parse_spec(&json!({"typology": "residential_tower", "rules": {"typology.mix.one": 20, "typology.mix.two": 20, "typology.mix.three": 20}}))
        .err()
        .expect("sixty flats on one floor")
        .to_string();
    assert!(e.contains("apartments") && e.contains("typology.mix"), "{e}");
    let none = parse_spec(&json!({"typology": "residential_tower", "rules": {"typology.mix.one": 0, "typology.mix.two": 0, "typology.mix.three": 0}}));
    assert!(none.is_err());
}

#[test]
fn the_tower_footprint_stays_within_the_two_lit_bands() {
    let (s, _) = build(&json!({"typology": "residential_tower"}));
    // massing.depth reaches 28 m, but flats need two lit bands: 2 x depth.band + the corridor
    let band_max = ddc_gen::project::rules::def("depth.band").unwrap().max;
    let limit = 2.0 * band_max + s.corridor;
    assert!(s.depth.unwrap() <= limit + 1e-9, "{:?} {limit}", s.depth);
}

type P2 = (f64, f64);

fn bbox(poly: &[P2]) -> (f64, f64, f64, f64) {
    let f = |g: fn(&P2) -> f64, lo: bool| poly.iter().map(g).fold(if lo { f64::MAX } else { f64::MIN }, |a, b| if lo { a.min(b) } else { a.max(b) });
    (f(|p| p.0, true), f(|p| p.1, true), f(|p| p.0, false), f(|p| p.1, false))
}

fn on_edge(poly: &[P2], p: P2) -> bool {
    let (x0, y0, x1, y1) = bbox(poly);
    let e = 0.06;
    let inx = p.0 > x0 - e && p.0 < x1 + e;
    let iny = p.1 > y0 - e && p.1 < y1 + e;
    inx && iny && ((p.0 - x0).abs() < e || (p.0 - x1).abs() < e || (p.1 - y0).abs() < e || (p.1 - y1).abs() < e)
}

/// Centre of an opening on its wall.
fn at(l: &ddc_gen::GLayout, o: &ddc_gen::GOpening) -> P2 {
    let w = &l.walls[o.wall];
    let len = ((w.b.0 - w.a.0).powi(2) + (w.b.1 - w.a.1).powi(2)).sqrt();
    let t = (o.along + o.width / 2.0) / len;
    (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t)
}

/// A typical tower floor: storey 1 (storey 0 carries the lobby) and its apartments.
fn tower_floor(extra: J) -> (GProject, usize) {
    let mut v = json!({"typology": "residential_tower"});
    if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(x.clone());
    }
    let (_, p) = build(&v);
    let k = (1..p.storeys.len()).find(|k| flats(&p, *k) > 0).expect("a floor with flats");
    (p, k)
}

fn flat_ids(p: &GProject, k: usize) -> Vec<u32> {
    let mut ids: Vec<u32> = p.storeys[k].layout.rooms.iter().filter_map(|r| r.apartment).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[test]
fn tower_apartment_rooms_do_not_overlap_and_fill_the_unit_cell() {
    let (p, k) = tower_floor(json!({}));
    let l = &p.storeys[k].layout;
    for a in flat_ids(&p, k) {
        let rooms: Vec<_> = l.rooms.iter().filter(|r| r.apartment == Some(a)).collect();
        let boxes: Vec<_> = rooms.iter().map(|r| bbox(&r.polygon)).collect();
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                let (p, q) = (boxes[i], boxes[j]);
                let w = p.2.min(q.2) - p.0.max(q.0);
                let h = p.3.min(q.3) - p.1.max(q.1);
                assert!(w < 0.01 || h < 0.01, "apartment {a}: {} overlaps {}", rooms[i].name, rooms[j].name);
            }
        }
        let cell = (
            boxes.iter().map(|b| b.0).fold(f64::MAX, f64::min),
            boxes.iter().map(|b| b.1).fold(f64::MAX, f64::min),
            boxes.iter().map(|b| b.2).fold(f64::MIN, f64::max),
            boxes.iter().map(|b| b.3).fold(f64::MIN, f64::max),
        );
        let whole = (cell.2 - cell.0) * (cell.3 - cell.1);
        let sum: f64 = rooms.iter().map(|r| area(&r.polygon)).sum();
        assert!((whole - sum).abs() < 0.05, "apartment {a}: rooms cover {sum:.2} of the {whole:.2} m2 cell");
    }
}

#[test]
fn tower_bedrooms_and_living_rooms_have_a_window_on_the_facade() {
    let (p, k) = tower_floor(json!({}));
    let l = &p.storeys[k].layout;
    let mut seen = 0;
    for r in l.rooms.iter().filter(|r| r.apartment.is_some() && (r.name.contains("Bedroom") || r.name.contains("Living"))) {
        seen += 1;
        let lit = l.openings.iter().any(|o| {
            !o.door && l.walls[o.wall].role == ddc_gen::WallRole::Exterior && on_edge(&r.polygon, at(l, o))
        });
        assert!(lit, "{} has no window on the facade", r.name);
    }
    assert!(seen >= 2 * flat_ids(&p, k).len(), "bedrooms and living rooms found: {seen}");
}

#[test]
fn tower_rooms_are_reached_through_doors_from_the_hall() {
    let (p, k) = tower_floor(json!({}));
    let l = &p.storeys[k].layout;
    let doors: Vec<P2> = l.openings.iter().filter(|o| o.door).map(|o| at(l, o)).collect();
    for a in flat_ids(&p, k) {
        let idx: Vec<usize> = (0..l.rooms.len()).filter(|i| l.rooms[*i].apartment == Some(a)).collect();
        let hall = *idx.iter().find(|i| l.rooms[**i].name.contains("Hall")).unwrap_or_else(|| panic!("apartment {a} has no hall"));
        let mut seen = vec![hall];
        let mut i = 0;
        while i < seen.len() {
            let from = seen[i];
            i += 1;
            for &j in &idx {
                let joined = doors.iter().any(|d| on_edge(&l.rooms[from].polygon, *d) && on_edge(&l.rooms[j].polygon, *d));
                if joined && !seen.contains(&j) {
                    seen.push(j);
                }
            }
        }
        assert_eq!(seen.len(), idx.len(), "apartment {a}: only {} of {} rooms are reached from the hall", seen.len(), idx.len());
    }
}

#[test]
fn the_free_planner_keeps_the_refuge_floor_free_of_apartments() {
    let (_, p) = build(&json!({"typology": "residential_tower", "rules": {"layout.strategy": "free"}}));
    let refuge: Vec<usize> = (0..p.storeys.len()).filter(|k| p.storeys[*k].layout.rooms.iter().any(|r| r.name == "Refuge area")).collect();
    assert!(!refuge.is_empty());
    assert!(refuge.iter().all(|k| p.storeys[*k].layout.rooms.iter().all(|r| r.apartment.is_none())));
    assert!((0..p.storeys.len()).filter(|k| !refuge.contains(k)).any(|k| flats(&p, k) > 0));
}

#[test]
fn the_station_platform_lies_18_m_down_with_the_concourse_three_metres_or_more_above() {
    let (s, p) = build(&json!({"typology": "underground_station"}));
    let z = |name: &str| -> Vec<f64> {
        p.basements.iter().filter(|b| b.layout.rooms.iter().any(|r| r.name == name)).map(|b| b.elevation).collect()
    };
    let (platform, concourse) = (z("Platform"), z("Concourse"));
    assert!(!platform.is_empty() && !concourse.is_empty());
    assert!(platform.iter().all(|e| (e + 18.0).abs() <= 0.25 * 18.0), "{platform:?}");
    let gap = concourse.iter().cloned().fold(f64::MAX, f64::min) - platform.iter().cloned().fold(f64::MIN, f64::max);
    assert!(gap >= 3.0, "{gap}");
    // the deep levels hold the soil within the slenderness rule
    assert!(p.basements.iter().all(|b| b.height / s.rules.get("underground.retaining_thickness") <= s.rules.get("underground.retaining_slenderness")));
}

#[test]
fn a_typology_below_height_is_checked_and_survives_a_preset_round_trip() {
    let m = Model::from_json(&serde_json::from_str::<J>(include_str!("../data/typologies/underground_station.json")).unwrap()["model"]).unwrap();
    assert_eq!(m.below.as_ref().and_then(|b| b.height), Some(9.0));
    assert_eq!(Model::from_json(&m.to_json()).unwrap().to_json(), m.to_json());
    let mut j = m.to_json();
    j["below"]["height"] = json!(30.0);
    assert!(Model::from_json(&j).is_err());
}

/// H12 names the typology in German: the word builds the hospital wing, not an error.
#[test]
fn the_german_word_for_hospital_builds_the_hospital_wing() {
    for w in ["Krankenhaus", "krankenhaus"] {
        assert_eq!(ddc_gen::project::presets::find(w).map(|p| p.name.as_str()), Some("hospital_wing"));
        let (s, _) = build(&json!({"typology": w, "floors": 4, "lang": "de"}));
        assert_eq!(s.typology_name(), "hospital_wing");
    }
}

#[test]
fn beds_are_read_from_the_model_and_counted_in_the_schedule() {
    let mut j: J = serde_json::from_str(include_str!("../data/typologies/hospital_wing.json")).unwrap();
    let m = Model::from_json(&j["model"]).unwrap();
    assert_eq!(Model::from_json(&m.to_json()).unwrap().to_json(), m.to_json());
    let (_, p) = build(&json!({"typology": "hospital_wing"}));
    assert!(metric(&p, "typ_beds") > 0.0);
    // a typology without beds reports none
    let (_, q) = build(&json!({"typology": "data_center"}));
    assert!(!q.metrics.iter().any(|m| m.0 == "typ_beds"));
    j["model"]["spaces"][0]["beds"] = json!(3);
    assert_eq!(Model::from_json(&j["model"]).unwrap().to_json()["spaces"][0]["beds"], json!(3));
}

fn touch(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    let (p, q) = (bbox(a), bbox(b));
    let (w, h) = (p.2.min(q.2) - p.0.max(q.0), p.3.min(q.3) - p.1.max(q.1));
    (w > 0.5 && h >= -0.35) || (h > 0.5 && w >= -0.35)
}

#[test]
fn the_hospital_has_theatres_with_anterooms_sterile_supply_wards_and_icu_beds() {
    let (_, p) = build(&json!({"typology": "hospital_wing"}));
    assert_eq!(p.storeys.len(), 4);
    let all = |n: &str| p.storeys.iter().chain(&p.basements).flat_map(|s| s.layout.rooms.iter()).filter(|r| r.name == n).count();
    let theatres = all("Operating theatre");
    assert!(theatres >= 2, "{theatres}");
    assert!(all("Scrub anteroom") >= theatres, "an anteroom for every theatre");
    assert!(all("Patient room") + all("Isolation room") >= 4);
    assert!(all("Intensive care room") >= 1);
    assert!(all("Sterile supply room") >= 1);
    for n in ["Clean transfer lobby", "Dirty transfer lobby", "Clean utility room", "Dirty utility room"] {
        assert!(all(n) >= 1, "{n}");
    }
    // every theatre shares a wall with an anteroom
    for st in &p.storeys {
        let l = &st.layout;
        for t in l.rooms.iter().filter(|r| r.name == "Operating theatre") {
            assert!(l.rooms.iter().any(|a| a.name == "Scrub anteroom" && touch(&a.polygon, &t.polygon)), "theatre {:?} has no anteroom beside it", t.point);
        }
    }
    let beds = metric(&p, "typ_beds");
    assert!(beds >= (all("Patient room") + all("Intensive care room")) as f64, "{beds}");
    // the theatre has one door, and it opens on its anteroom
    for st in &p.storeys {
        let l = &st.layout;
        for t in l.rooms.iter().filter(|r| r.name == "Operating theatre") {
            let doors: Vec<P2> = l.openings.iter().filter(|o| o.door).map(|o| at(l, o)).filter(|d| on_edge(&t.polygon, *d)).collect();
            assert_eq!(doors.len(), 1, "theatre {:?}: {} doors", t.point, doors.len());
            let a = l.rooms.iter().find(|a| a.name == "Scrub anteroom" && on_edge(&a.polygon, doors[0])).expect("the door opens on an anteroom");
            assert!(touch(&a.polygon, &t.polygon));
        }
    }
}

fn rooms_of<'a>(p: &'a GProject, k: usize, n: &str) -> Vec<&'a ddc_gen::GRoom> {
    p.storeys[k].layout.rooms.iter().filter(|r| r.name == n).collect()
}

#[test]
fn the_hotel_has_a_podium_with_lobby_and_ballroom_and_back_of_house_below() {
    let (_, p) = build(&json!({"typology": "conference_hotel"}));
    assert_eq!(p.storeys.len(), 9);
    for n in ["Hotel lobby", "Restaurant", "Hotel kitchen", "Staff room"] {
        assert!(!rooms_of(&p, 0, n).is_empty(), "{n} on the ground floor");
    }
    let ball = rooms_of(&p, 1, "Ballroom");
    assert_eq!(ball.len(), 1);
    assert!(area(&ball[0].polygon) >= 250.0, "{}", area(&ball[0].polygon));
    assert!(!rooms_of(&p, 1, "Pre-function foyer").is_empty() && !rooms_of(&p, 1, "Meeting room").is_empty());
    // the podium floors hold no guest rooms, the floors above them hold no ballroom
    assert!(p.storeys[0].layout.rooms.iter().chain(&p.storeys[1].layout.rooms).all(|r| r.apartment.is_none()));
    assert!((2..9).all(|k| rooms_of(&p, k, "Ballroom").is_empty() && flats(&p, k) > 0));
    // back of house under the ground floor
    let below: Vec<&str> = p.basements.iter().flat_map(|b| b.layout.rooms.iter().map(|r| r.name.as_str())).collect();
    for n in ["Laundry", "Hotel store", "Staff changing room"] {
        assert!(below.contains(&n), "{n} in {below:?}");
    }
}

#[test]
fn every_guest_room_has_an_ensuite_and_the_rooms_stand_on_both_sides_of_the_corridor() {
    let (s, p) = build(&json!({"typology": "conference_hotel"}));
    let want = (s.rules.get("typology.hotel.rooms") + s.rules.get("typology.hotel.suites")) as usize;
    assert_eq!(flats(&p, 2), want);
    let l = &p.storeys[2].layout;
    for a in flat_ids(&p, 2) {
        let own: Vec<&str> = l.rooms.iter().filter(|r| r.apartment == Some(a)).map(|r| r.name.as_str()).collect();
        assert!(own.iter().any(|n| n.contains("ensuite")) || own.iter().any(|n| n.contains("Ensuite")), "{own:?}");
    }
    let corridor = l.rooms.iter().filter(|r| r.apartment.is_none()).find(|r| r.name.to_lowercase().contains("corridor")).expect("a corridor");
    let mid = (bbox(&corridor.polygon).1 + bbox(&corridor.polygon).3) / 2.0;
    let guests: Vec<&ddc_gen::GRoom> = l.rooms.iter().filter(|r| r.name == "Guest room").collect();
    assert!(guests.iter().any(|r| r.point.1 < mid) && guests.iter().any(|r| r.point.1 > mid));
}

#[test]
fn hotel_room_numbers_follow_the_rules_and_the_model_round_trips() {
    let (_, p) = build(&json!({"typology": "conference_hotel", "rules": {"typology.hotel.rooms": 12, "typology.hotel.suites": 2}}));
    assert_eq!(flats(&p, 2), 14);
    let pre = ddc_gen::project::presets::of_brief(&json!({"typology": "conference_hotel"})).unwrap().unwrap().into_owned();
    let model = pre.model.clone().expect("model");
    let again = Model::from_json(&model.to_json()).unwrap();
    assert_eq!(again.to_json(), model.to_json());
    assert_eq!(model.to_json()["periodic"][1]["at"], json!([1]));
}

#[test]
fn service_shafts_stack_through_every_storey() {
    use ddc_gen::ShaftKind;
    for (name, n) in [("hospital_wing", 4), ("conference_hotel", 2)] {
        let (_, p) = build(&json!({"typology": name}));
        let svc = |k: usize| -> Vec<_> {
            p.storeys[k].layout.shafts.iter().filter(|s| s.kind != ShaftKind::Lift && s.kind != ShaftKind::Duct || s.kind == ShaftKind::Duct).collect()
        };
        for st in &p.storeys {
            let bays = st.layout.shafts.iter().filter(|s| s.kind == ShaftKind::Riser || s.kind == ShaftKind::Electrical).count();
            assert!(bays >= n.min(2), "{name}: {bays} riser and cable shafts");
        }
        let first: Vec<_> = svc(0).iter().map(|s| (s.kind, s.ring.clone())).collect();
        for k in 1..p.storeys.len() {
            let here: Vec<_> = svc(k).iter().map(|s| (s.kind, s.ring.clone())).collect();
            assert_eq!(first.len(), here.len(), "{name}: storey {k} shaft count");
            for (a, b) in first.iter().zip(&here) {
                assert_eq!(a.0, b.0);
                assert!(a.1.iter().zip(&b.1).all(|(x, y)| (x.0 - y.0).abs() < 1e-6 && (x.1 - y.1).abs() < 1e-6), "{name}: shaft shifts on storey {k}");
            }
        }
    }
}

fn in_ring(ring: &[(f64, f64)], q: (f64, f64)) -> bool {
    let (x0, x1) = ring.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (y0, y1) = ring.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    q.0 >= x0 - 1e-6 && q.0 <= x1 + 1e-6 && q.1 >= y0 - 1e-6 && q.1 <= y1 + 1e-6
}

/// Vertical stretches of a polyline at least `min` long, as their plan points.
fn rises(points: &[(f64, f64, f64)], min: f64) -> Vec<(f64, f64)> {
    points
        .windows(2)
        .filter(|w| (w[0].0 - w[1].0).abs() < 1e-6 && (w[0].1 - w[1].1).abs() < 1e-6 && (w[0].2 - w[1].2).abs() >= min)
        .map(|w| (w[0].0, w[0].1))
        .collect()
}

#[test]
fn vertical_ducts_and_cables_run_in_their_core_shafts() {
    use ddc_gen::ShaftKind;
    for name in ["hospital_wing", "conference_hotel"] {
        let (_, p) = build(&json!({"typology": name}));
        let rings = |kind: ShaftKind| -> Vec<Vec<(f64, f64)>> {
            p.storeys[0].layout.shafts.iter().filter(|s| s.kind == kind).map(|s| s.ring.clone()).collect()
        };
        let storey = p.storeys[0].height;
        // a core too narrow for the bays keeps the old per-system shafts
        if p.metrics.iter().any(|m| m.0 == "core_shafts_cut" && m.1 == 0.0) {
            continue;
        }
        let (duct, elec, riser) = (rings(ShaftKind::Duct), rings(ShaftKind::Electrical), rings(ShaftKind::Riser));
        assert!(!elec.is_empty(), "{name}: no cable shaft in the core");
        for d in &p.hvac.ducts {
            for q in rises(&d.points, storey * 0.8) {
                assert!(duct.iter().any(|r| in_ring(r, q)), "{name}: vertical duct at {q:?} outside the duct shafts");
            }
        }
        for r in &p.services.water {
            for q in rises(&r.points, storey * 0.8) {
                assert!(riser.iter().any(|e| in_ring(e, q)), "{name}: vertical {:?} pipe at {q:?} outside the riser shaft", r.sys);
            }
        }
        for r in &p.services.elec {
            for q in rises(&r.points, storey * 0.8) {
                assert!(elec.iter().any(|e| in_ring(e, q)), "{name}: vertical cable at {q:?} outside the cable shaft");
            }
        }
    }
}

#[test]
fn narrow_plan_degrades_the_shafts_and_never_refuses() {
    for width in [5.0, 7.0, 9.0, 12.0, 16.0] {
        let old = json!({"typology": "hospital_wing", "width": width, "rules": {"typology.hospital_wing.shafts": 0}});
        let new = json!({"typology": "hospital_wing", "width": width});
        let Ok(so) = parse_spec(&old) else { continue };
        if build_project(&so).is_err() {
            continue;
        }
        let sn = parse_spec(&new).unwrap_or_else(|e| panic!("{new}: {e}"));
        let p = build_project(&sn).unwrap_or_else(|e| panic!("width {width}: the building used to build: {e}"));
        if p.metrics.iter().any(|m| m.0 == "core_shafts_cut") {
            assert!(
                p.services.issues.iter().any(|i| i.starts_with("info:") && i.contains("service shafts")),
                "width {width}: cut shafts not reported"
            );
        }
    }
}

#[test]
fn services_of_the_hospital_wing_and_the_hotel_do_not_clash() {
    use ddc_gen::project::{services_errors, void_check};
    for name in ["hospital_wing", "conference_hotel"] {
        let (_, p) = build(&json!({"typology": name, "services": true}));
        assert!(!p.services.water.is_empty() && !p.hvac.ducts.is_empty(), "{name}: no services to check");
        let void = void_check(&p);
        assert!(void.is_empty(), "{name}: {} clashes in the void, first: {:?}", void.len(), void.first());
        let hard: Vec<String> = services_errors(&p).into_iter().filter(|f| f.contains("clash")).collect();
        assert!(hard.is_empty(), "{name}: {} clashes with the building or each other, first: {:?}", hard.len(), hard.first());
    }
}

#[test]
fn ducts_and_pipes_never_enter_a_lift_shaft() {
    use ddc_gen::ShaftKind;
    // no riser or duct bays: the core keeps only its lift shafts, so every run must go around them
    let (_, p) = build(&json!({"typology": "hospital_wing", "services": true, "rules": {"typology.hospital_wing.shafts": 0}}));
    let lift: Vec<Vec<(f64, f64)>> = p.storeys[0].layout.shafts.iter().filter(|s| s.kind == ShaftKind::Lift).map(|s| s.ring.clone()).collect();
    assert!(!lift.is_empty(), "no lift shaft to keep clear");
    let runs = p.hvac.ducts.iter().map(|d| &d.points).chain(p.services.water.iter().map(|r| &r.points));
    for points in runs {
        for w in points.windows(2) {
            for t in [0.0, 0.5, 1.0] {
                let q = (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
                assert!(!lift.iter().any(|r| in_ring(r, q)), "a duct or pipe enters the lift shaft at {q:?}");
            }
        }
    }
}

/// The hospital wing's power for H02 and H12: a board on every floor over a tray riser, a tray
/// along every corridor reaching the riser, emergency luminaires in every ward and in every
/// light of a theatre on their own circuits, theatre and ICU sockets on the essential supply.
#[test]
fn hospital_wing_has_boards_trays_and_emergency_power() {
    use ddc_geom::mep::Domain;
    use ddc_gen::project::{CircuitUse, RoomKind};
    let (_, p) = build(&json!({"typology": "hospital_wing", "floors": 4, "services": true}));
    let n = p.storeys.len();
    let trays: Vec<_> = p.services.elec.iter().filter(|r| r.section.domain == Domain::CableTray).collect();
    let vertical = |r: &&ddc_gen::project::GRun| r.points.windows(2).all(|w| (w[0].0 - w[1].0).hypot(w[0].1 - w[1].1) < 1e-6);
    let risers: Vec<_> = trays.iter().filter(|r| vertical(r)).collect();
    assert!(risers.len() >= n - 1, "{} riser pieces for {n} storeys", risers.len());
    // the riser: the plan point most vertical tray pieces stand on
    let at = |r: &&&ddc_gen::project::GRun| (r.points[0].0, r.points[0].1);
    let rxy = risers
        .iter()
        .map(at)
        .max_by_key(|q| risers.iter().filter(|r| (at(r).0 - q.0).hypot(at(r).1 - q.1) < 0.05).count())
        .unwrap();
    let seg_d = |a: (f64, f64, f64), b: (f64, f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((rxy.0 - a.0) * dx + (rxy.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
        (a.0 + t * dx - rxy.0).hypot(a.1 + t * dy - rxy.1)
    };
    let mut em_total = 0;
    for (k, st) in p.storeys.iter().enumerate() {
        let fx = &st.layout.fixtures;
        assert!(fx.iter().any(|f| f.family == "Distribution Board"), "storey {k}: no board");
        let flat: Vec<_> = trays.iter().filter(|r| r.storey == k && !vertical(r)).collect();
        assert!(!flat.is_empty(), "storey {k}: no corridor tray");
        let reach = flat.iter().flat_map(|r| r.points.windows(2).map(|w| seg_d(w[0], w[1]))).fold(f64::MAX, f64::min);
        assert!(reach < 1.0, "storey {k}: the corridor tray is {reach:.2} m from the riser");
        for (pr, gr) in st.rooms.iter().zip(&st.layout.rooms) {
            let (x0, y0, x1, y1) = gr.polygon.iter().fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, q| (b.0.min(q.0), b.1.min(q.1), b.2.max(q.0), b.3.max(q.1)));
            let lights: Vec<_> = fx
                .iter()
                .filter(|f| f.family == "Ceiling Light" && f.at.0 > x0 && f.at.0 < x1 && f.at.1 > y0 && f.at.1 < y1)
                .collect();
            let em = lights.iter().filter(|f| f.ty.ends_with(" EM")).count();
            em_total += em;
            if lights.is_empty() {
                continue;
            }
            if gr.name.starts_with("Operating theatre") {
                assert_eq!(em, lights.len(), "storey {k} {}: every theatre light is an emergency one", gr.name);
            } else if pr.kind == RoomKind::Ward {
                assert!(em >= 1 && em < lights.len().max(2), "storey {k} {}: {em} of {} lights emergency", gr.name, lights.len());
            }
        }
    }
    let used = |u: CircuitUse| p.services.circuits.iter().filter(|c| c.use_ == u).count();
    assert!(em_total >= 20, "{em_total} emergency luminaires");
    assert!(used(CircuitUse::Emergency) >= n, "{} emergency circuits for {n} storeys", used(CircuitUse::Emergency));
    assert!(used(CircuitUse::Essential) >= 2, "{} essential circuits", used(CircuitUse::Essential));
    for c in p.services.circuits.iter().filter(|c| c.use_ == CircuitUse::Emergency) {
        assert!(c.name.contains("Emergency lighting"), "{}", c.name);
        for d in &c.devices {
            assert!(p.storeys[d.0].layout.fixtures[d.1].ty.ends_with(" EM"), "{}: a general light", c.name);
        }
    }
    // the same circuits in Russian
    let (_, ru) = build(&json!({"typology": "hospital_wing", "floors": 4, "services": true, "lang": "ru"}));
    let samples: J = serde_json::from_str(include_str!("data/ru_samples.json")).unwrap();
    for k in ["emergency_lighting", "backup_sockets"] {
        let want = samples[k].as_str().unwrap();
        assert!(ru.services.circuits.iter().any(|c| c.name.contains(want)), "{k}");
    }
}

/// The emergency rules steer the count: one in every luminaire of a ward when asked.
#[test]
fn emergency_share_follows_its_rule() {
    let em = |v: &J| {
        let (_, p) = build(v);
        p.storeys.iter().flat_map(|s| &s.layout.fixtures).filter(|f| f.ty.ends_with(" EM")).count()
    };
    let base = em(&json!({"typology": "hospital_wing", "floors": 2, "services": true}));
    let all = em(&json!({"typology": "hospital_wing", "floors": 2, "services": true, "rules": {"services.elec.em_every": 1}}));
    assert!(base > 0 && all > base, "{base} -> {all}");
    let office = em(&json!({"typology": "office", "floors": 2, "services": true}));
    assert_eq!(office, 0, "an office has no clinical room");
}

#[test]
fn a_hospital_wing_designs_its_air_side_with_fire_and_volume_dampers() {
    use ddc_gen::project::mech::{self, equip::Kind};
    let (s, p) = build(&json!({"typology": "hospital_wing", "floors": 4}));
    let m = mech::design(&s, &p).expect("the typology asks for the mechanical design");
    assert!(m.count(Kind::FireDamper) >= 2, "{} fire dampers", m.count(Kind::FireDamper));
    let all = m.count(Kind::FireDamper) + m.count(Kind::VolumeDamper);
    assert!(all >= 8, "{all} dampers");
    assert_eq!(mech::unconnected(&m), 0);
}

#[test]
fn the_dedicated_unit_of_a_hospital_wing_has_its_coils_and_fans() {
    use ddc_gen::project::mech::{self, equip::Kind};
    let (s, p) = build(&json!({"typology": "hospital_wing", "floors": 4}));
    assert_eq!(p.hvac.plant.len(), 2, "a general and a dedicated unit");
    let m = mech::design(&s, &p).expect("the typology asks for the mechanical design");
    let coils = m.cons.iter().filter(|c| m.equip[c.equip].kind == Kind::Ahu).count();
    assert!(coils >= 2, "{coils} coil sets on the air handling units");
    assert_eq!(m.extra_fans.len(), 1);
    assert!(m.extra_fans[0].iter().all(|f| f.flow > 0.0 && f.motor_kw > 0.0), "{:?}", m.extra_fans);
    let own = m.equip.iter().find(|e| e.kind == Kind::Ahu && e.net == 1).expect("the dedicated unit");
    assert!(own.duty("cool_kw") > 0.0 && own.duty("heat_kw") > 0.0);
    assert!(m.coils.0 >= own.duty("cool_kw") && m.coils.1 >= own.duty("heat_kw"), "the totals include both units: {:?}", m.coils);
    assert_eq!(mech::unconnected(&m), 0);
}
