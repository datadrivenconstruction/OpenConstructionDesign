// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Process plants from a brief: topology with N+1 pumps, every nozzle piped, no clashes,
//! clearances and reach met, line list adding up to the totals, deterministic, explained.

use std::collections::BTreeMap;

use ddc_gen::plant::{self, Plant};
use ddc_gen::project::rules;
use ddc_geom::component::{Family, Lod};
use serde_json::{json, Value as J};

fn design(brief: J) -> Plant {
    plant::design(&brief).unwrap_or_else(|e| panic!("{brief}: {e}"))
}

fn assert_sound(p: &Plant) {
    let r = &p.report;
    let issues: Vec<String> = r.issues.iter().take(20).map(|i| format!("{}: {}", i.check, i.what)).collect();
    assert!(r.ok, "{} issues: {issues:#?}", r.issues.len());
    assert_eq!(r.connected, r.nozzles);
    assert!(r.nozzles > 0 && r.pipe_pairs > 0);
    assert!(r.min_margin >= 0.0, "{}", r.min_margin);
    assert_eq!(r.valves_in_reach, r.valves);
    assert!(p.piping.unrouted.is_empty());
}

/// Duty and standby count of each pump set.
fn pump_sets(p: &Plant) -> BTreeMap<&'static str, (usize, usize)> {
    let mut m: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for e in p.equipment.iter().filter(|e| e.kind == plant::EqKind::Pump) {
        let c = m.entry(e.role).or_default();
        if e.standby {
            c.1 += 1;
        } else {
            c.0 += 1;
        }
    }
    m
}

fn assert_totals(p: &Plant) {
    let s = &p.schedules;
    let t = &s.totals;
    let sum = |f: &dyn Fn(&plant::LineRow) -> usize| s.lines.iter().map(f).sum::<usize>();
    let len: f64 = s.lines.iter().map(|l| l.length).sum();
    assert!((len - t.pipe_m).abs() < 0.01, "{len} vs {}", t.pipe_m);
    assert_eq!(sum(&|l| l.elbows), t.elbows);
    assert_eq!(sum(&|l| l.tees), t.tees);
    assert_eq!(sum(&|l| l.reducers), t.reducers);
    assert_eq!(sum(&|l| l.flanges), t.flanges);
    assert_eq!(sum(&|l| l.valves), t.valves);
    assert_eq!(sum(&|l| l.instruments), t.instruments);
    assert_eq!(sum(&|l| l.supports), t.supports);
    let by_dn: f64 = t.by_dn.values().sum();
    assert!((by_dn - len).abs() < 0.01);
    assert!(t.pipe_m > 0.0 && t.elbows > 0 && t.flanges > 0 && t.supports > 0 && t.valves > 0);
    for l in &s.lines {
        assert!(!l.legs.is_empty() && l.length > 0.0, "{}", l.number);
        assert!(l.p_test >= l.p_design && l.t_design >= l.t_op, "{}", l.number);
    }
}

fn assert_families(p: &Plant) {
    for (e, (tag, text)) in p.equipment.iter().zip(p.families()) {
        let f = Family::parse(&text).unwrap_or_else(|err| panic!("{tag}: {err:?}\n{text}"));
        let r = f.resolve(&[], &[]).unwrap_or_else(|err| panic!("{tag}: {err:?}"));
        let b = f.build(&r, Lod::Medium).unwrap_or_else(|err| panic!("{tag}: {err:?}"));
        assert!(b.bbox[1].z > b.bbox[0].z, "{tag}");
        let ports = f.ports(&r).unwrap();
        for n in &e.nozzles {
            let q = ports.iter().find(|q| q.name == n.name).unwrap_or_else(|| panic!("{tag}.{}", n.name));
            assert!((q.at - n.at).len() < 1e-3 && (q.dir - n.dir).len() < 1e-6, "{tag}.{}", n.name);
        }
    }
}

#[test]
fn boiler_house_brief_gives_a_sound_piped_plant() {
    let p = design(json!({"text": "Boiler house 2 MW with 3 boilers"}));
    let boilers: Vec<_> = p.equipment.iter().filter(|e| e.kind == plant::EqKind::Boiler).collect();
    assert_eq!(boilers.len(), 3);
    // 2000 kW + 10 % over 3 boilers: 733 kW rounded up to 750 kW
    assert_eq!(boilers[0].duty[0], ("kw", 750.0));
    let sets = pump_sets(&p);
    assert_eq!(sets["network_pump"], (1, 1), "{sets:?}");
    assert_eq!(sets["makeup_pump"], (1, 1));
    assert_eq!(sets["boiler_pump"].0 + sets["boiler_pump"].1, 3);
    for kind in [plant::EqKind::Separator, plant::EqKind::Expansion, plant::EqKind::Softener, plant::EqKind::Sump] {
        assert!(p.equipment.iter().any(|e| e.kind == kind), "{kind:?}");
    }
    assert_eq!(p.flues.len(), 3);
    assert!(p.flues.iter().all(|f| f.top > p.hall.z));
    assert_sound(&p);
    assert_totals(&p);
    assert_families(&p);
    // the boilers' valves are above reach: platforms with ladders serve them
    assert!(!p.platforms.is_empty());
    assert!(p.steel.iter().any(|s| s.kind == plant::SteelKind::Rung));
    assert!(p.steel.iter().any(|s| s.kind == plant::SteelKind::Rail));
    // fittings of every kind a tree of pipes needs
    for k in [plant::FitKind::Elbow, plant::FitKind::Tee, plant::FitKind::Flange] {
        assert!(p.piping.fittings.iter().any(|f| f.kind == k), "{k:?}");
    }
    // hot lines insulated, gas in its own spec
    assert!(p.lines.iter().filter(|l| l.svc == plant::Svc::Hws).all(|l| l.insul > 0.0));
    assert!(p.lines.iter().filter(|l| l.svc == plant::Svc::Ng).all(|l| l.spec == plant::Spec::G1));
    // drains fall to the sump
    assert!(p.piping.drain_fall > 0.0);
    let keys: Vec<&str> = p.why.iter().map(|y| y.key).collect();
    for k in ["plant_boilers", "plant_pumps", "plant_expansion", "plant_flue", "plant_line_dn", "plant_platform", "plant_rack", "plant_hall"] {
        assert!(keys.contains(&k), "{k}");
    }
}

/// Every element of the kit builds, every connector meets a pipe end or the next item's
/// connector, and every pipe end meets a connector or another pipe.
fn assert_kit(p: &Plant) {
    use ddc_geom::math::{v3, V3};
    let k = &p.kit;
    let near = |a: V3, b: V3| (a - b).len() < 2e-3;
    let fams: BTreeMap<&str, &plant::KitFamily> = k.families.iter().map(|f| (f.name.as_str(), f)).collect();
    let mut ports: Vec<(String, V3, usize)> = vec![];
    for c in &k.comps {
        let fam = fams.get(c.family.as_str()).unwrap_or_else(|| panic!("{}: no family {}", c.key, c.family));
        let f = Family::parse(&fam.text).unwrap_or_else(|e| panic!("{}: {e:?}\n{}", fam.name, fam.text));
        let r = f.resolve(&[], &[]).unwrap();
        f.build(&r, Lod::Medium).unwrap_or_else(|e| panic!("{}: {e:?}", fam.name));
        let (s, co) = c.rotation.sin_cos();
        for q in f.ports(&r).unwrap() {
            let line = c.ports.iter().find(|x| x.0 == q.name).unwrap_or_else(|| panic!("{} {}", c.key, q.name)).1;
            let w = v3(q.at.x * co - q.at.y * s, q.at.x * s + q.at.y * co, q.at.z) + c.at;
            ports.push((format!("{} {}", c.key, q.name), w, line));
        }
        assert_eq!(f.ports(&r).unwrap().len(), c.ports.len(), "{}", c.key);
    }
    let ends: Vec<(V3, usize)> = k.runs.iter().flat_map(|r| [(r.points[0], r.line), (*r.points.last().unwrap(), r.line)]).collect();
    for (name, w, line) in &ports {
        let to_pipe = ends.iter().any(|e| e.1 == *line && near(e.0, *w));
        let to_item = ports.iter().filter(|o| o.2 == *line && near(o.1, *w)).count() >= 2;
        assert!(to_pipe || to_item, "{name} at {w:?} meets nothing");
    }
    for r in &k.runs {
        assert!(r.points.len() >= 2 && r.dn > 0);
        for e in [r.points[0], *r.points.last().unwrap()] {
            let to_port = ports.iter().any(|q| q.2 == r.line && near(q.1, e));
            let to_pipe = k.runs.iter().filter(|o| o.line == r.line).flat_map(|o| o.points.iter()).filter(|q| near(**q, e)).count() >= 2;
            assert!(to_port || to_pipe, "line {} end {e:?} is open", r.line);
        }
    }
    let units = p.equipment.len();
    assert!(k.comps.iter().filter(|c| c.key.starts_with("unit:")).count() == units);
    assert!(k.comps.iter().any(|c| c.key.starts_with("support:")) && k.comps.iter().any(|c| c.key.starts_with("stack:")));
    let keys: std::collections::BTreeSet<&str> = k.comps.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys.len(), k.comps.len(), "keys are unique");
}

/// A Russian sample from the test data file.
fn ru_sample(key: &str) -> String {
    let m: J = serde_json::from_str(include_str!("../data/ru_samples.json")).unwrap();
    m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()
}

#[test]
fn every_plant_is_a_set_of_connected_model_elements() {
    for b in [
        json!({"text": "Boiler house 2 MW with 3 boilers"}),
        json!({"text": ru_sample("pump")}),
        json!({"text": "water treatment 20 l/s"}),
        json!({"kind": "process_line", "flow_m3h": 8}),
    ] {
        let p = design(b);
        assert_kit(&p);
    }
}

#[test]
fn the_same_brief_gives_the_same_plant() {
    let b = json!({"text": "Boiler house 2 MW with 3 boilers"});
    let (a, c) = (design(b.clone()), design(b));
    assert_eq!(a.to_json(), c.to_json());
    assert_eq!(a.families(), c.families());
    assert_eq!(a.kit, c.kit);
}

#[test]
fn every_kind_of_plant_is_sound() {
    for b in [
        json!({"text": ru_sample("pump")}),
        json!({"text": "water treatment 20 l/s"}),
        json!({"kind": "process_line", "flow_m3h": 8}),
        json!({"text": ru_sample("boiler_small")}),
    ] {
        let p = design(b.clone());
        assert_sound(&p);
        assert_totals(&p);
        assert_families(&p);
        // N+1 in every set but the boiler pumps, which go one per boiler
        for (role, (duty, standby)) in pump_sets(&p) {
            assert!(duty >= 1 && (standby == 1 || role == "boiler_pump"), "{b}: {role} {duty}+{standby}");
        }
    }
}

#[test]
fn rules_change_the_plant_and_say_so() {
    let p = design(json!({"text": "pumping station 200 m3/h", "rules": {"plant.pump.standby": 2, "plant.pump.max_flow": 50}}));
    assert_eq!(pump_sets(&p)["station_pump"], (4, 2));
    let y = p.why.iter().find(|y| y.key == "plant_pumps").unwrap();
    assert_eq!((y.rule.as_str(), y.source.as_str()), ("plant.pump.standby", "brief"));
    assert_sound(&p);
    assert!(plant::design(&json!({"text": "pumping station 50 m3/h", "rules": {"plant.pump.standby": 9}})).is_err());
    assert!(plant::design(&json!({"text": "boiler house"})).is_err());
}

#[test]
fn every_plant_rule_exists_and_is_read() {
    let srcs = [
        include_str!("../../src/plant/mod.rs"),
        include_str!("../../src/plant/brief.rs"),
        include_str!("../../src/plant/topo.rs"),
        include_str!("../../src/plant/size.rs"),
        include_str!("../../src/plant/equip.rs"),
        include_str!("../../src/plant/layout.rs"),
        include_str!("../../src/plant/route.rs"),
        include_str!("../../src/plant/sched.rs"),
        include_str!("../../src/plant/check.rs"),
        include_str!("../../src/plant/why.rs"),
        include_str!("../../src/plant/model.rs"),
        include_str!("../../src/plant/tank.rs"),
    ];
    let mut read = std::collections::BTreeSet::new();
    for src in srcs {
        for part in src.split("\"plant.").skip(1) {
            // the id's own characters only: error texts quote ids inside sentences
            let tail = part.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')).next();
            let id = format!("plant.{}", tail.unwrap_or("").trim_end_matches('.'));
            let d = rules::def(&id).unwrap_or_else(|| panic!("rule {id} is not in rules.json"));
            assert!(d.default.is_some(), "{id}");
            read.insert(id);
        }
    }
    let all: Vec<&str> = rules::defs().iter().filter(|d| d.group == "plant").map(|d| d.id.as_str()).collect();
    assert!(all.len() >= 100, "{}", all.len());
    for id in all {
        assert!(read.contains(id), "rule {id} is never read");
    }
}

#[test]
fn every_reason_has_en_and_ru_text_with_its_slots() {
    let en: BTreeMap<String, String> = serde_json::from_str(include_str!("../../../ui/data/i18n/en/gen_plant.json")).unwrap();
    let ru: BTreeMap<String, String> = serde_json::from_str(include_str!("../../../ui/data/i18n/ru/gen_plant.json")).unwrap();
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    let slots = |t: &str| {
        let mut v: Vec<String> = t.split('{').skip(1).map(|s| s.split('}').next().unwrap_or("").to_string()).collect();
        v.sort();
        v
    };
    for (k, t) in plant::why::EN {
        let key = format!("gen.why.e.{k}");
        assert_eq!(en[&key], *t, "{k}");
        assert_eq!(slots(&ru[&key]), slots(t), "{k}");
    }
    for d in rules::defs().iter().filter(|d| d.group == "plant") {
        for f in ["title", "desc"] {
            let k = format!("gen.rule.{}.{f}", d.id);
            assert!(en.contains_key(&k) && ru[&k].chars().any(|c| ('\u{0430}'..='\u{044f}').contains(&c.to_lowercase().next().unwrap())), "{k}");
        }
    }
    // every reason the plants give fills its template from its slots
    for b in [json!({"text": "water treatment 30 m3/h"}), json!({"text": "distillation process line 10 m3/h"})] {
        let p = design(b);
        for y in &p.why {
            let t = plant::why::template(y.key).unwrap_or_else(|| panic!("{}", y.key));
            let named: Vec<String> = { let mut v: Vec<String> = y.args.iter().map(|a| a.0.to_string()).collect(); v.sort(); v };
            assert_eq!(named, slots(t), "{}", y.key);
            assert!(!y.effect.contains('{'), "{}", y.effect);
        }
    }
}
