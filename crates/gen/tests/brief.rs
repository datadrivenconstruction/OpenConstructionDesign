// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Brief against built: contradictions resolved with a note, typology templates that build and
//! pass the gates, and the generate, check, repair loop settling within its rounds.

use ddc_gen::project::brief::{check, verify_loop, MAX_ROUNDS};
use ddc_gen::project::{
    build_project, check_project, parse_spec, typology_names, GProject, ProjectSpec,
};
use serde_json::{json, Value as J};

fn build(v: &J) -> Result<(ProjectSpec, GProject), String> {
    let s = parse_spec(v).map_err(|e| e.to_string())?;
    let p = build_project(&s).map_err(|e| e.to_string())?;
    let g = check_project(&s, &p);
    if !g.passed() {
        return Err(format!("gates: {}", g.failures.join("; ")));
    }
    Ok((s, p))
}

#[test]
fn footprint_depth_wins_over_band_depth_with_a_note() {
    let v = json!({"typology": "office", "floors": 2,
        "footprint": {"width": 30, "depth": 15}, "band_depth": 6});
    let (s, _) = build(&v).unwrap();
    assert_eq!(s.depth, Some(15.0));
    assert!((s.band_depth - 6.6).abs() < 1e-9, "{}", s.band_depth);
    assert!(s.notes.iter().any(|n| n.contains("footprint wins")), "{:?}", s.notes);
}

#[test]
fn every_template_builds_and_names_its_approximation() {
    let names = typology_names();
    for t in ["hotel", "clinic", "warehouse", "mixed_use", "school_campus", "residential"] {
        assert!(names.contains(&t), "{t} in {names:?}");
        for v in [json!({"typology": t}), json!({"typology": t, "floors": 1})] {
            let (s, p) = build(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
            assert_eq!(s.typology_name(), t);
            assert!(s.notes[0].starts_with("approximated"), "{:?}", s.notes);
            let r = check(&s, &p).report;
            assert_eq!(r["typology"]["asked"], t);
            eprintln!("{v}: storeys {} rooms {}", p.storeys.len(), r["rooms_by_kind"]);
        }
    }
    let (s, p) = build(&json!({"typology": "medical_centre", "floors": 3, "lang": "ru"})).unwrap();
    assert_eq!(s.typology_name(), "clinic");
    assert!(check(&s, &p).report["rooms_by_kind"]["ward"].as_u64() > Some(0));
    let (s, _) = build(&json!({"typology": "hotel", "floors": 3})).unwrap();
    assert!(s.rooms.iter().any(|r| r.name == "Guest room"), "{:?}", s.rooms);
    let e = parse_spec(&json!({"typology": "hotell"})).unwrap_err().to_string();
    assert!(e.contains("did you mean 'hotel'"), "{e}");
}

#[test]
fn the_loop_repairs_what_the_spec_can_say_and_names_the_rest() {
    let brief = json!({"typology": "office", "floors": 4,
        "features": ["l_shape", "Curtain wall", "balcony", "lift", "curved", "atrium"]});
    let rounds = verify_loop(brief, &mut build).unwrap();
    assert_eq!(rounds.len(), 2, "{:#?}", rounds.iter().map(|r| &r.check.report).collect::<Vec<_>>());
    let first = &rounds[0].check;
    assert!(!first.settled);
    assert_eq!(
        first.repair,
        Some(json!({"design": {"massing": "l", "facade": "curtain", "balconies": "balcony"}}))
    );
    let last = &rounds[1].check;
    assert!(last.settled && last.repair.is_none());
    let f = &last.report["features"];
    assert_eq!(f["met"], json!(["l_shape", "curtain_wall", "balcony", "lift"]));
    let not_built: Vec<&str> =
        f["not_built"].as_array().unwrap().iter().filter_map(|x| x["feature"].as_str()).collect();
    assert!(not_built.is_empty(), "{f}");
    // a curve bows a plain bar and the atrium roofs a courtyard: the L asked first rules both
    // out, so the person decides
    let clash: Vec<&str> =
        f["conflict"].as_array().unwrap().iter().filter_map(|x| x["feature"].as_str()).collect();
    assert_eq!(clash, ["curved", "atrium"], "{f}");
    assert_eq!(rounds[1].spec["design"]["massing"], "l");
    assert!(rounds.len() <= MAX_ROUNDS);
}

#[test]
fn clashing_repairs_are_not_guessed() {
    let (s, p) = build(&json!({"typology": "office", "floors": 8,
        "design": {"massing": "tower_podium", "upper_from": 2, "setback": 4},
        "features": ["l_shape", "setback"]}))
    .unwrap();
    let c = check(&s, &p);
    assert!(c.settled && c.repair.is_none(), "{}", c.report);
    assert_eq!(c.report["features"]["conflict"][0]["feature"], "l_shape");
    let (s, p) = build(&json!({"typology": "office", "floors": 5,
        "features": ["setback", "cantilever"]}))
    .unwrap();
    let c = check(&s, &p);
    assert_eq!(c.repair, Some(json!({"design": {"setback": 2.0}})));
    assert_eq!(c.report["features"]["conflict"][0]["feature"], "cantilever");
}

fn curtain_count(p: &GProject) -> usize {
    p.storeys.iter().map(|s| s.curtains.len()).sum()
}

#[test]
fn a_curtain_wall_brief_builds_curtain_walls_and_the_check_counts_them() {
    let v = json!({"typology": "office", "floors": 4, "features": ["curtain_wall"]});
    let (s, p) = build(&v).unwrap();
    assert_eq!(s.design.facade.name(), "curtain");
    assert!(s.notes.iter().any(|n| n.contains("curtain")), "{:?}", s.notes);
    assert!(curtain_count(&p) > 0);
    assert!(p.storeys.iter().all(|t| !t.curtains.is_empty()), "every storey carries curtain walls");
    assert!(p.storeys.iter().flat_map(|t| &t.curtains).all(|c| c.spandrel > 0.0), "spandrel at the slab edge");
    let r = check(&s, &p).report;
    assert_eq!(r["features"]["met"], json!(["curtain_wall"]), "{r}");
    assert_eq!(r["curtain_walls"]["storeys"], json!(4));
    assert_eq!(r["curtain_walls"]["count"], json!(curtain_count(&p)));
}

#[test]
fn a_named_other_facade_or_the_rule_off_keeps_the_brief_unmet() {
    for v in [
        json!({"typology": "office", "floors": 3, "features": ["glass_facade"], "design": {"facade": "ribbon"}}),
        json!({"typology": "office", "floors": 3, "features": ["glass_facade"],
               "rules": {"facade.curtain_from_brief": false}}),
    ] {
        let (s, p) = build(&v).unwrap();
        assert_eq!(curtain_count(&p), 0, "{v}");
        let c = check(&s, &p);
        assert_eq!(c.report["features"]["met"], json!([]), "{v}");
        assert_eq!(c.repair, Some(json!({"design": {"facade": "curtain"}})), "{v}");
    }
}

#[test]
fn the_curtain_facade_name_alone_never_passes() {
    let v = json!({"typology": "office", "floors": 3, "design": {"facade": "curtain"}, "features": ["curtain_wall"]});
    let (s, mut p) = build(&v).unwrap();
    assert_eq!(check(&s, &p).report["features"]["met"], json!(["curtain_wall"]));
    for t in &mut p.storeys {
        t.curtains.clear();
    }
    let r = check(&s, &p).report;
    assert_eq!(r["features"]["met"], json!([]), "{r}");
    assert_eq!(r["features"]["conflict"][0]["feature"], json!("curtain_wall"), "{r}");
    // curtain walls on one storey only are an approximation, not a pass
    let (s, mut p) = build(&v).unwrap();
    p.storeys[1].curtains.clear();
    let r = check(&s, &p).report;
    assert_eq!(r["features"]["approximated"][0]["feature"], json!("curtain_wall"), "{r}");
}

#[test]
fn the_why_names_the_curtain_choice_of_the_brief() {
    let (s, p) = build(&json!({"typology": "office", "floors": 3, "features": ["Curtain wall"]})).unwrap();
    let w = ddc_gen::project::why::explain_project(&s, &p);
    assert!(w.iter().any(|x| x.rule == "facade.curtain_from_brief"), "no why for the curtain choice");
}

/// A Russian sample from the test data file: a string, or the list of strings when `key` holds one.
fn ru_all(key: &str) -> Vec<String> {
    let m: serde_json::Value = serde_json::from_str(include_str!("data/ru_samples.json")).unwrap();
    match m[key].as_array() {
        Some(a) => a.iter().map(|x| x.as_str().unwrap().to_string()).collect(),
        None => vec![m[key].as_str().unwrap_or_else(|| panic!("no ru sample {key}")).to_string()],
    }
}

fn ru(key: &str) -> String {
    ru_all(key).remove(0)
}

#[test]
fn sprinkler_and_curtain_keywords_switch_them_on_in_en_ru_de_es_and_zh() {
    use ddc_gen::project::brief::{asks_curtain_wall, asks_sprinklers};
    for k in ["sprinklers", "sprinkleranlage", "rociadores", "喷淋"].map(String::from).into_iter().chain([ru("sprinklers")]) {
        assert!(asks_sprinklers(&k), "{k}");
    }
    for k in ["curtain_wall", "vorhangfassade", "muro_cortina", "幕墙"].map(String::from).into_iter().chain([ru("curtain")]) {
        assert!(asks_curtain_wall(&k), "{k}");
    }
}

#[test]
fn a_sprinkler_keyword_builds_the_system_at_any_height() {
    let v = json!({"typology": "office", "floors": 2, "features": [ru("sprinklers")]});
    let (s, p) = build(&v).unwrap();
    assert_eq!(s.sprinklers, Some(true));
    assert!(p.fire.heads > 0 && !p.fire.risers.is_empty(), "no heads or riser");
    let met = check(&s, &p).report["features"]["met"].clone();
    assert!(met.as_array().unwrap().iter().any(|f| *f == ru("sprinklers")), "{met}");
}

#[test]
fn sprinklers_and_a_curtain_wall_are_built_with_their_parts() {
    use ddc_gen::project::FireRun;
    let v = json!({"typology": "office", "floors": 3, "sprinklers": true, "design": {"facade": "curtain_wall"}});
    let (s, p) = build(&v).unwrap();
    assert_eq!(s.design.facade.name(), "curtain");
    assert!(p.fire.heads > 0 && !p.fire.risers.is_empty(), "no heads or riser");
    assert!(p.fire.runs.iter().any(|r| r.kind == FireRun::Branch), "no branch lines");
    let curtains: Vec<_> = p.storeys.iter().flat_map(|t| &t.curtains).collect();
    assert!(!curtains.is_empty(), "no curtain wall built");
    assert!(curtains.iter().all(|c| c.mullion.0 > 0.0 && c.spacing.0 > 0.0), "curtain without mullions or transoms");
}

/// The words of a hospital brief in each language find the hospital wing, with its operating
/// theatres, intensive care and isolation rooms, not the clinic or the plain office.
#[test]
fn hospital_words_in_every_language_find_the_hospital_wing() {
    let en = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let words: [(&str, Vec<String>); 5] = [
        ("en", en(&["hospital", "hospital_wing"])),
        ("de", en(&["Krankenhaus", "Krankenhausflügel", "Klinikum", "Bettenstation"])),
        ("es", en(&["hospital_general", "ala_hospitalaria", "pabellon_hospitalario"])),
        ("zh", en(&["医院", "住院楼", "医院病房楼"])),
        ("ru", ru_all("hospital")),
    ];
    for (lang, list) in words {
        for w in &list {
            let (s, p) = build(&json!({"typology": w, "floors": 4})).unwrap_or_else(|e| panic!("{lang} {w}: {e}"));
            assert_eq!(s.typology_name(), "hospital_wing", "{lang} {w}");
            let names: Vec<String> = p.storeys.iter().chain(&p.basements).flat_map(|st| st.layout.rooms.iter().map(|r| r.name.clone())).collect();
            for n in ["Operating theatre", "Intensive care room", "Isolation room"] {
                assert!(names.iter().any(|x| x == n), "{lang} {w}: no {n} in {names:?}");
            }
        }
    }
    // the clinic keeps its own words
    for w in ["medical_centre", "Poliklinik", "诊所"].map(String::from).into_iter().chain([ru("clinic")]) {
        assert_eq!(build(&json!({"typology": w, "floors": 2})).unwrap().0.typology_name(), "clinic", "{w}");
    }
}

/// Service words of a hospital brief in each language reach the same feature keywords: the
/// ventilation is built and named, the sprinklers are met or asked for, the operating-room
/// count hint is judged against the theatres built.
#[test]
fn service_words_in_every_language_reach_the_services() {
    let s3 = |a: &str, b: &str, c: &str| [a.to_string(), b.to_string(), c.to_string()];
    let words: [(&str, [String; 3]); 5] = [
        ("en", s3("ventilation", "Sprinkler", "2 operating rooms")),
        ("de", s3("Lüftung", "Sprinkleranlage", "2 OP-Säle")),
        ("ru", s3(&ru("hvac"), &ru("sprinklers"), &ru("operating_rooms"))),
        ("es", s3("ventilación", "rociadores", "2 quirófanos")),
        ("zh", s3("通风", "喷淋", "2手术室")),
    ];
    for (lang, [vent, spr, ops]) in words {
        let (s, p) = build(&json!({"typology": "hospital_wing", "floors": 4, "features": [vent, spr, ops]})).unwrap_or_else(|e| panic!("{lang}: {e}"));
        assert_eq!(s.features, ["hvac", "sprinklers", "operating_rooms:2"], "{lang}");
        assert!(!p.hvac.ducts.is_empty(), "{lang}: the ventilation is built");
        let f = &check(&s, &p).report["features"];
        let listed = |k: &str, w: &str| f[k].as_array().unwrap().iter().any(|x| x == w || x["feature"] == w);
        assert!(listed("approximated", "hvac"), "{lang}: {f}");
        assert!(listed("met", "sprinklers") || listed("missing", "sprinklers"), "{lang}: {f}");
        assert!(listed("met", "operating_rooms:2"), "{lang}: two theatres are built: {f}");
        assert!(f["not_built"].as_array().unwrap().is_empty(), "{lang}: {f}");
    }
    // services.hvac false leaves the ventilation out, and the feature then asks for it back
    let (s, p) = build(&json!({"typology": "hospital_wing", "floors": 4, "services": {"hvac": false}, "features": ["Lüftung"]})).unwrap();
    assert!(p.hvac.ducts.is_empty());
    assert!(check(&s, &p).report["features"]["missing"][0]["patch"]["services"]["hvac"] == true);
    // three theatres are more than the typology builds
    let (s, p) = build(&json!({"typology": "hospital_wing", "floors": 4, "features": ["3 OP-Säle"]})).unwrap();
    assert_eq!(check(&s, &p).report["features"]["not_built"][0]["feature"], "operating_rooms:3");
}
