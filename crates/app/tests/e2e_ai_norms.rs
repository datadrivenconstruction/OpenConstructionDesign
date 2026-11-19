// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Norm pack tools: lookup, check and programme read the country pack and never edit.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_model::StairSpec;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

fn model() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let top = ed.model.add_level("L2", 3.0).unwrap();
    let mut spec = StairSpec::straight(ed.level, top, (1.0, 2.0), 0.0);
    spec.max_riser = 0.22;
    spec.width = 0.9;
    ed.model.add_stair(&spec).unwrap();
    ed.reload().unwrap();
    ed
}

fn results(ed: &mut Editor, calls: Vec<(&str, J)>) -> Vec<(J, bool)> {
    let (_s, p) = run(ed, calls.into_iter().map(|(n, a)| tool(n, a)).collect());
    tool_results(&p).into_iter().map(|(t, e)| (parsed(&t), e)).collect()
}

#[test]
fn lookup_finds_rules_dims_and_packs_with_their_sources() {
    let mut ed = model();
    let before = live_hash(&ed.model);
    let r = results(
        &mut ed,
        vec![
            ("norms_lookup", json!({"pack": "ru", "query": "apartment"})),
            ("norms_lookup", json!({"topic": "packs"})),
            ("norms_lookup", json!({"lang": "ru", "topic": "dims", "query": "risers"})),
            ("norms_lookup", json!({"lang": "fr", "topic": "drawing"})),
            ("norms_lookup", json!({"pack": "xx"})),
        ],
    );
    assert!(r[..4].iter().all(|x| !x.1), "{r:?}");
    let rules = r[0].0["rules"].as_array().unwrap();
    let area = rules.iter().find(|x| x["id"] == "ru.apartment.area").unwrap();
    assert_eq!(area["verified"], true);
    assert_eq!(area["clause"], "table 5.1");
    assert_eq!(area["source"]["licence"], "public-law");
    assert_eq!(area["source"]["title"], "SP 54.13330.2022");
    assert_eq!(
        r[1].0["packs"],
        json!([
            "ae", "ae-du", "ar", "au", "br", "ca", "cn", "cz", "de", "es", "es-ct", "eu", "fr", "generic", "id", "in", "it",
            "jp", "kr", "kz", "mx", "nl", "pl", "ru", "sa", "se", "tr", "ua", "uk", "us", "vn", "za"
        ])
    );
    assert_eq!(r[2].0["pack"], "ru");
    assert_eq!(r[2].0["dims"][0]["value"], json!([3, 16]));
    assert_eq!(r[2].0["dims"][0]["verified"], true);
    // the French pack has no drawing standard of its own: it inherits the generic one
    assert_eq!(r[3].0["pack"], "fr");
    assert_eq!(r[3].0["drawing"]["titleblock"], "iso7200");
    assert!(r[4].1, "an unknown pack is an error: {:?}", r[4]);
    assert_eq!(live_hash(&ed.model), before, "read only");
}

#[test]
fn programme_gives_the_variant_for_a_room_count() {
    let mut ed = model();
    let r = results(
        &mut ed,
        vec![
            ("norms_programme", json!({"pack": "ru", "typology": "apartment", "rooms": 2})),
            ("norms_programme", json!({"pack": "ru", "typology": "apartment", "rooms": 9})),
            ("norms_programme", json!({"pack": "generic", "typology": "house"})),
        ],
    );
    assert!(!r[0].1, "{r:?}");
    let v = &r[0].0["variant"];
    assert_eq!((v["code"].as_str(), v["area_min"].as_f64()), (Some("2k"), Some(44.0)));
    assert_eq!(v["verified"], true);
    assert_eq!(v["source"]["title"], "SP 54.13330.2022");
    assert!(r[1].1, "no variant for nine rooms");
    assert_eq!(r[2].0["variants"].as_array().unwrap().len(), 5);
}

#[test]
fn check_runs_the_pack_and_keeps_unverified_numbers_as_advice() {
    let mut ed = model();
    let before = live_hash(&ed.model);
    let r = results(
        &mut ed,
        vec![
            ("norms_check", json!({"pack": "ru", "depth": 3})),
            ("norms_check", json!({"pack": "ru", "depth": 1, "discipline": "fire"})),
        ],
    );
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    let all = &r[0].0;
    let ran: Vec<&str> = all["rules_ran"].as_array().unwrap().iter().filter_map(J::as_str).collect();
    assert!(ran.contains(&"generic.stair.step") && ran.contains(&"ex.stair.risers"));
    let skipped = all["rules_skipped"].as_array().unwrap();
    assert!(skipped.iter().any(|s| s["rule"] == "ru.apartment.area" && s["why"] == "typology"));
    let findings = all["findings"].as_array().unwrap();
    for f in findings {
        let id = f["rule"].as_str().unwrap();
        if id.starts_with("generic.") {
            assert_eq!((f["hard"].as_bool(), f["severity"].as_str()), (Some(false), Some("info")), "{f}");
        }
        assert!(!f["clause"].as_str().unwrap_or("").is_empty(), "{f}");
    }
    let hard = all["hard"].as_u64().unwrap();
    assert_eq!(hard + all["advice"].as_u64().unwrap(), findings.len() as u64);
    assert!(r[1].0["rules_ran"].as_array().unwrap().is_empty(), "no fire rule at depth 1");
    assert_eq!(live_hash(&ed.model), before, "read only");
}

#[test]
fn code_check_proposes_fixes_and_quantities_sum_lengths() {
    let mut ed = model();
    let wt = ed.current_type().unwrap().id;
    let wall = ed
        .model
        .add_wall(&ddc_model::WallSpec {
            a: (0.0, -3.0),
            b: (5.0, -3.0),
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let door = ddc_model::OpeningSpec { along: 1.0, width: 0.7, sill: 0.0, height: 2.1, kind: "door".into() };
    let d = ed.model.add_opening(wall, &door).unwrap();
    ed.reload().unwrap();
    let before = live_hash(&ed.model);
    let r = results(
        &mut ed,
        vec![
            ("norms_check", json!({"pack": "generic"})),
            ("quantities", json!({"target": {"filter": "wall"}, "group_by": "cat"})),
        ],
    );
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    let f = r[0].0["findings"].as_array().unwrap();
    let dw = f.iter().find(|x| x["rule"] == "generic.door.width").expect("door width finding");
    assert_eq!(dw["refs"], json!([format!("opening:{d}")]));
    assert_eq!(dw["fix"], json!({"tool": "update_elements", "set": {"width": 0.8}}));
    let g = &r[1].0["groups"][0];
    assert!((g["length"].as_f64().unwrap() - 5.0).abs() < 1e-6, "{g}");
    assert_eq!(live_hash(&ed.model), before, "read only");
}

#[test]
fn programme_answers_for_presets_through_their_base_typology() {
    let mut ed = model();
    let r = results(
        &mut ed,
        vec![
            ("norms_programme", json!({"pack": "us", "typology": "clinic"})),
            ("norms_programme", json!({"pack": "de", "typology": "hospital"})),
            ("norms_programme", json!({"pack": "us", "typology": "warehouse"})),
            ("norms_programme", json!({"pack": "us", "typology": "no_such_thing"})),
        ],
    );
    for i in 0..3 {
        assert!(!r[i].1, "{:?}", r[i]);
        assert!(r[i].0["preset"]["built_as"].is_string(), "{:?}", r[i]);
        assert!(!r[i].0["variants"].as_array().unwrap().is_empty(), "{:?}", r[i]);
    }
    assert_eq!(r[1].0["preset"]["name"], "clinic", "an alias finds its preset");
    assert!(r[3].1, "an unknown name stays an error");
    assert!(r[3].0.to_string().contains("it has:"), "the error names what the pack offers: {:?}", r[3]);
}
