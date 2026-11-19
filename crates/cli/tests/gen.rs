// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc gen`: the brief of the contract examples becomes a model; every brief field is used,
//! reported as unused, or reported as derived, and a brief with unconfirmed values is refused.
#![cfg(feature = "link")]

use std::path::{Path, PathBuf};

use ddc_cli::gen::{run, GenArgs};
use ddc_io::contract::{validate, Doc};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../../io/contract/examples/brief.de-residential.json");

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_gen_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn example() -> Value {
    serde_json::from_str(EXAMPLE).unwrap()
}

fn go(brief: &Value, d: &Path, out: &str, force: bool) -> (Value, i32) {
    let b = d.join("brief.json");
    std::fs::write(&b, brief.to_string()).unwrap();
    let (text, code) = run(&GenArgs { brief: b, out: d.join(out), force }, true, true);
    (serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")), code)
}

fn keys(path: &Path) -> Vec<String> {
    let mut l = ddc_cli::load::load(path).unwrap();
    let c = ddc_io::elements_parquet::collect(&mut l.model, &Default::default()).unwrap();
    let mut k: Vec<String> = c.rows.into_iter().map(|r| r.key).collect();
    k.sort();
    k
}

fn unsupported(log: &Value) -> Vec<String> {
    log["unsupported"].as_array().unwrap().iter().map(|u| u["pointer"].as_str().unwrap().to_string()).collect()
}

#[test]
fn the_example_brief_follows_its_schema() {
    validate(Doc::Brief, &example()).unwrap();
}

#[test]
fn the_germany_residential_example_generates() {
    let d = dir("ex");
    let (log, code) = go(&example(), &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert_eq!(log["schema"], "ddc-cli-log/1");
    assert_eq!(log["command"], "gen");
    assert!(d.join("m.ddc").is_file());
    assert_eq!(log["spec"]["typology"], "apartment_section");
    assert_eq!(log["spec"]["floors"], 5);
    assert_eq!(log["spec"]["footprint"], json!({"width": 34.0, "depth": 12.0}));
    // 10 units of each kind over 5 floors: 2 per floor
    assert_eq!(log["spec"]["apartments"][0]["count"], 2);
    assert!(log["counts"]["wall"].as_i64().unwrap() > 50, "{}", log["counts"]);
    let k = keys(&d.join("m.ddc"));
    assert!(k.len() > 100);
    assert!(k.iter().all(|x| ddc_io::elements_parquet::key_ok(x)), "keys follow the grammar");
    let set: std::collections::BTreeSet<_> = k.iter().collect();
    assert_eq!(set.len(), k.len(), "keys are unique");
}

#[test]
fn fields_the_generator_cannot_use_are_listed_never_dropped() {
    let d = dir("unsup");
    let (log, code) = go(&example(), &d, "m.ddc", false);
    assert_eq!(code, 0);
    let u = unsupported(&log);
    for p in [
        "/site/plot", "/site/setbacks_m/front", "/site/max_floors", "/budget/amount", "/quality_level",
        "/structure/system", "/depth_level", "/country", "/massing/floors_below",
        "/programme/spaces/0",
    ] {
        assert!(u.iter().any(|x| x == p || x.starts_with(&format!("{p}/"))), "{p} not reported: {u:?}");
    }
    // the used ones are not reported as unused
    for p in ["/massing/floors_above", "/massing/lift", "/envelope/roof", "/programme/units/0/name", "/structure/foundation"] {
        assert!(!u.iter().any(|x| x == p), "{p} wrongly reported");
        assert!(log["mapped"].as_array().unwrap().iter().any(|m| m == p), "{p} not mapped");
    }
    // the unit area is spread over the rooms and the log says so
    let der = log["derived"].as_array().unwrap();
    assert!(der.iter().any(|x| x["pointer"] == "/programme/units/0/rooms/0/area_m2"), "{der:?}");
}

#[test]
fn a_value_still_proposed_or_default_refuses_the_run_and_writes_nothing() {
    let d = dir("unconf");
    for status in ["proposed", "default"] {
        let mut b = example();
        b["provenance"]["/massing/floors_above"] = json!({"status": status});
        let (log, code) = go(&b, &d, "m.ddc", false);
        assert_eq!(code, 2);
        assert_eq!(log["error"]["kind"], "unconfirmed");
        assert!(log["error"]["details"][0].as_str().unwrap().contains("/massing/floors_above"));
        assert!(!d.join("m.ddc").exists());
    }
    // a proposed value the generator does not read does not block (the example has one)
    let (_, code) = go(&example(), &d, "ok.ddc", false);
    assert_eq!(code, 0);
    // a proposed parent of a used value does
    let mut b = example();
    b["provenance"]["/massing"] = json!({"status": "proposed"});
    assert_eq!(go(&b, &d, "n.ddc", false).1, 2);
}

#[test]
fn an_invalid_brief_is_refused_with_every_problem_named() {
    let d = dir("bad");
    let mut b = example();
    b.as_object_mut().unwrap().remove("massing");
    b["depth_level"] = json!("D9");
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 2);
    assert_eq!(log["error"]["kind"], "brief_invalid");
    let det = log["error"]["details"].to_string();
    assert!(det.contains("massing") && det.contains("depth_level"), "{det}");
    assert!(!d.join("m.ddc").exists());
}

#[test]
fn usage_and_file_errors_exit_two_without_touching_files() {
    let d = dir("usage");
    // not JSON
    let b = d.join("b.json");
    std::fs::write(&b, "{not json").unwrap();
    let (t, c) = run(&GenArgs { brief: b.clone(), out: d.join("m.ddc"), force: false }, true, true);
    assert_eq!(c, 2);
    assert!(t.contains("not JSON"), "{t}");
    // missing brief
    let (_, c) = run(&GenArgs { brief: d.join("none.json"), out: d.join("m.ddc"), force: false }, true, true);
    assert_eq!(c, 2);
    // output that is the input, or not a .ddc, or already there
    std::fs::write(&b, EXAMPLE).unwrap();
    assert_eq!(run(&GenArgs { brief: b.clone(), out: b.clone(), force: true }, true, true).1, 2);
    assert_eq!(std::fs::read_to_string(&b).unwrap(), EXAMPLE);
    assert_eq!(run(&GenArgs { brief: b.clone(), out: d.join("m.ifc"), force: false }, true, true).1, 2);
    std::fs::write(d.join("m.ddc"), "keep me").unwrap();
    assert_eq!(run(&GenArgs { brief: b.clone(), out: d.join("m.ddc"), force: false }, true, true).1, 2);
    assert_eq!(std::fs::read_to_string(d.join("m.ddc")).unwrap(), "keep me");
    assert_eq!(run(&GenArgs { brief: b, out: d.join("m.ddc"), force: true }, true, true).1, 0);
    assert!(std::fs::metadata(d.join("m.ddc")).unwrap().len() > 1000);
}

#[test]
fn the_same_brief_gives_the_same_spec_and_the_same_keys() {
    let d = dir("det");
    let (a, ca) = go(&example(), &d, "a.ddc", false);
    let (b, cb) = go(&example(), &d, "b.ddc", false);
    assert_eq!((ca, cb), (0, 0));
    for k in ["spec", "mapped", "unsupported", "derived", "counts", "project"] {
        assert_eq!(a[k], b[k], "{k}");
    }
    assert_eq!(keys(&d.join("a.ddc")), keys(&d.join("b.ddc")));
}

#[test]
fn a_use_without_a_generator_is_refused_unless_a_template_is_pinned() {
    let d = dir("use");
    let mut b = example();
    b["use"] = json!({"category": "retail"});
    b.as_object_mut().unwrap().remove("generator");
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 2);
    assert_eq!(log["error"]["kind"], "unsupported");
    b["generator"] = json!({"template": "universal"});
    assert_eq!(go(&b, &d, "m.ddc", false).0["error"]["kind"], "unsupported");
    b["generator"] = json!({"template": "office"});
    b.as_object_mut().unwrap().remove("programme");
    b["programme"] = json!({"spaces": [{"kind": "office", "area_m2": 25, "count": 6}, {"kind": "wc", "area_m2": 4, "count": 2}]});
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert_eq!(log["spec"]["typology"], "office");
    assert_eq!(log["spec"]["rooms"][0]["area"], 25.0);
}

#[test]
fn derived_values_are_said_and_overrides_win() {
    let d = dir("der");
    let mut b = example();
    b["locale"] = json!("de");
    b["programme"]["units"][0]["count"] = json!(6);
    b["generator"] = json!({"template": "apartment_section", "spec_overrides": {"floor_height": 3.2, "lang": "ru"}});
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    let der = log["derived"].to_string();
    assert!(der.contains("/locale") && der.contains("6 units over 5 floors"), "{der}");
    assert_eq!(log["spec"]["floor_height"], 3.2, "overrides are merged last");
    assert_eq!(log["spec"]["lang"], "ru");
}

#[test]
fn an_out_of_range_value_is_refused_with_the_generator_text() {
    let d = dir("range");
    let mut b = example();
    b["massing"]["floor_height_m"] = json!(2.3);
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 2);
    assert_eq!(log["error"]["kind"], "spec");
    assert!(log["error"]["message"].as_str().unwrap().contains("floor_height"), "{log}");
}

fn rows_of(path: &Path) -> Vec<ddc_io::elements_parquet::ElementRow> {
    let mut l = ddc_cli::load::load(path).unwrap();
    ddc_io::elements_parquet::collect(&mut l.model, &Default::default()).unwrap().rows
}

#[test]
fn the_raft_of_the_example_is_built_with_the_footprint_area_and_a_stable_key() {
    let d = dir("raft");
    let (log, code) = go(&example(), &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert_eq!(log["spec"]["foundation"], "raft");
    assert_eq!(log["foundation"]["kind"], "raft");
    assert_eq!(log["foundation"]["keys"], json!(["b1/found/raft"]));
    assert!(log["derived"].to_string().contains("/structure/foundation"));
    let rows = rows_of(&d.join("m.ddc"));
    let rafts: Vec<_> = rows.iter().filter(|r| r.type_key.starts_with("slab/foundation")).collect();
    assert_eq!(rafts.len(), 1, "one foundation slab");
    let r = rafts[0];
    assert_eq!((r.key.as_str(), r.ifc_class.as_str(), r.element_type.as_str()), ("b1/found/raft", "IfcSlab", "Slab"));
    // 34 m x 12 m footprint
    let area = r.area_m2.expect("the raft has an area");
    assert!((area - 408.0).abs() / 408.0 < 0.02, "area {area}");
    assert!(r.volume_m3.unwrap() > 408.0 * 0.39 && r.volume_m3.unwrap() < 408.0 * 0.5, "{:?}", r.volume_m3);
    // the key is stable: a second run gives the same one
    let (_, code) = go(&example(), &d, "m2.ddc", false);
    assert_eq!(code, 0);
    assert!(rows_of(&d.join("m2.ddc")).iter().any(|x| x.key == "b1/found/raft"));
    // no footing class here: a raft is a slab
    assert_eq!(rows.iter().filter(|r| r.ifc_class == "IfcFooting").count(), 0);
}

#[test]
fn a_strip_foundation_is_built_under_the_bearing_walls() {
    let d = dir("strip");
    let mut b = example();
    b["structure"]["foundation"] = json!("strip");
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert_eq!(log["foundation"]["kind"], "strip");
    let n = log["foundation"]["count"].as_u64().unwrap();
    assert!(n >= 4, "{n}");
    let rows = rows_of(&d.join("m.ddc"));
    let strips: Vec<_> = rows.iter().filter(|r| r.ifc_class == "IfcFooting").collect();
    assert_eq!(strips.len() as u64, n);
    for s in &strips {
        assert!(s.key.starts_with("b1/found/strip/") && ddc_io::elements_parquet::key_ok(&s.key), "{}", s.key);
        assert!(s.volume_m3.unwrap() > 0.0, "{}", s.key);
    }
    assert!(rows.iter().all(|r| !r.type_key.starts_with("slab/foundation")), "strips, not a raft");
}

#[test]
fn piles_and_no_foundation_build_none_and_say_so() {
    let d = dir("nofound");
    let mut b = example();
    b["structure"]["foundation"] = json!("piles");
    let (log, code) = go(&b, &d, "m.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert!(unsupported(&log).iter().any(|p| p == "/structure/foundation"), "{log}");
    assert!(log["foundation"].is_null());
    let rows = rows_of(&d.join("m.ddc"));
    assert!(rows.iter().all(|r| !r.type_key.starts_with("slab/foundation") && r.ifc_class != "IfcFooting"));
    b["structure"]["foundation"] = Value::Null;
    let (log, code) = go(&b, &d, "n.ddc", false);
    assert_eq!(code, 0, "{log}");
    assert!(log["foundation"].is_null());
}
