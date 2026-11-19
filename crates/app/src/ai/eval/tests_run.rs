// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The executor end to end on a scripted model: record, replay, drift, budget, ledger line.

use super::case::Case;
use super::metrics::{ledger_line, CaseMetrics, RunInfo, Status};
use super::replay;
use super::run::{replay_profile, run_all, run_case, Made, Opts, ProviderFactory, ReplayFactory};
use ddc_ai::fake::{ReplayProvider, RecordingProvider, Script, ScriptedProvider};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

fn opts(price: f64) -> Opts {
    Opts {
        out: None,
        profile: replay_profile(),
        price_in: price,
        price_out: price,
        turn_timeout: Duration::from_secs(120),
    }
}

fn case(id: &str, say: &str) -> Case {
    Case::parse(&format!(
        "id = \"{id}\"\ntier = \"hard\"\n[[turn]]\nsay = \"{say}\"\n[[oracle.count]]\nspec = \"wall\"\nmin = 1.0\n"
    ))
    .unwrap()
}

/// A model that draws one wall and then answers.
fn scripted() -> std::sync::Arc<ScriptedProvider> {
    ScriptedProvider::new(vec![
        vec![Script::Tool {
            name: "create_elements".into(),
            args: json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 0], "b": [5, 0]}]}),
        }],
        vec![Script::Text("done".into())],
    ])
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ddc_eval_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn strip(mut m: CaseMetrics) -> CaseMetrics {
    m.wall_s = 0.0;
    m
}

fn record(c: &Case, root: &std::path::Path) -> CaseMetrics {
    let rec = RecordingProvider::new(scripted());
    let made = Made { provider: rec.clone(), replay: None, rec: Some(rec.clone()) };
    let m = run_case(c, &made, &opts(0.0), None);
    replay::save(root, &c.id, &rec.recording()).unwrap();
    m
}

fn replayed(c: &Case, root: &std::path::Path) -> CaseMetrics {
    let rp = ReplayProvider::new(replay::load(root, &c.id).unwrap());
    let made = Made { provider: rp.clone(), replay: Some(rp), rec: None };
    run_case(c, &made, &opts(0.0), None)
}

#[test]
fn replay_is_deterministic() {
    let root = tmp("det");
    let c = case("det", "build a wall");
    let recorded = record(&c, &root);
    assert_eq!(recorded.status, Status::Pass, "{recorded:?}");
    assert_eq!(recorded.steps, 1);
    assert_eq!(recorded.undo_steps, 1);
    let (a, b) = (replayed(&c, &root), replayed(&c, &root));
    assert_eq!(a.status, Status::Pass, "{a:?}");
    assert_eq!(strip(a.clone()), strip(b.clone()));
    // what the replay measures is what the live run measured
    assert_eq!(strip(a.clone()), strip(recorded));
    let info = RunInfo { commit: "x".into(), provider: "replay".into(), model: "replay".into(), tier: "hard".into(), wall_s: 1.0 };
    let (mut la, mut lb) = (ledger_line(&info, &[a]), ledger_line(&info, &[b]));
    for l in [&mut la, &mut lb] {
        let o = l.as_object_mut().unwrap();
        o.remove("ts");
        o.remove("wall_s");
    }
    assert_eq!(la, lb);
}

#[test]
fn replay_drift_detected() {
    let root = tmp("drift");
    record(&case("drift", "build a wall"), &root);
    // the same recording, asked something else: it must not pass quietly
    let m = replayed(&case("drift", "build two walls"), &root);
    assert_eq!(m.status, Status::Drift, "{m:?}");
    assert!(m.note.starts_with("replay_drift"), "{}", m.note);
    assert!(!m.pass());
}

#[test]
fn ledger_line_schema() {
    let root = tmp("schema");
    let c = case("schema", "build a wall");
    record(&c, &root);
    let ok = replayed(&c, &root);
    let missing = CaseMetrics::not_run("later", "budget spent");
    let info = RunInfo { commit: "abc".into(), provider: "replay".into(), model: "m".into(), tier: "hard".into(), wall_s: 2.5 };
    let l = ledger_line(&info, &[ok, missing]);
    for k in ["ts", "commit", "provider", "model", "tier", "cases", "pass_rate", "oracle_rate", "steps_p50", "tokens", "usd", "refusals", "wall_s", "per_case"] {
        assert!(l.get(k).is_some(), "ledger line has no {k}");
    }
    assert_eq!(l["cases"], json!(1));
    assert_eq!(l["not_run"], json!(1));
    assert_eq!(l["pass_rate"], json!(1.0));
    assert_eq!(l["oracle_rate"], json!(1.0));
    assert_eq!(l["steps_p50"], json!(1.0));
    assert!(l["tokens"]["in"].as_u64().unwrap() > 0 && l["tokens"]["out"].as_u64().unwrap() > 0);
    let per = l["per_case"].as_array().unwrap();
    assert_eq!(per.len(), 2);
    assert_eq!(per[0], json!({"id": "schema", "status": "pass", "pass": true, "failed_oracles": []}));
    assert_eq!(per[1]["status"], json!("not_run"));
    assert!(!l.to_string().contains("DDC_AI_KEY"));
}

struct Fresh;

impl ProviderFactory for Fresh {
    fn make(&self, _: &Case) -> Result<Made, String> {
        Ok(Made { provider: scripted(), replay: None, rec: None })
    }
}

#[test]
fn budget_stops_cleanly() {
    let cases = [case("b1", "a"), case("b2", "b"), case("b3", "c")];
    // one token costs a dollar; the budget is five: the first case crosses it
    let m = run_all(&cases, &Fresh, &opts(1_000_000.0), 1, Some(5.0), None);
    assert_eq!(m.len(), 3);
    assert_eq!(m[0].status, Status::Fail);
    assert_eq!(m[0].note, "budget reached");
    assert!(m[0].usd >= 5.0);
    for later in &m[1..] {
        assert_eq!(later.status, Status::NotRun, "{later:?}");
        assert_eq!(later.note, "budget spent");
        assert!(later.oracles.is_empty());
    }
    let info = RunInfo { commit: "c".into(), provider: "live".into(), model: "m".into(), tier: "hard".into(), wall_s: 0.0 };
    let l = ledger_line(&info, &m);
    assert_eq!(l["cases"], json!(1));
    assert_eq!(l["not_run"], json!(2));
}

#[test]
fn missing_recording_is_not_run() {
    let root = tmp("none");
    let cases = [case("nothing_recorded", "a")];
    let m = run_all(&cases, &ReplayFactory { root }, &opts(0.0), 1, None, None);
    assert_eq!(m[0].status, Status::NotRun);
    assert!(m[0].note.contains("steps.json"), "{}", m[0].note);
}

#[test]
fn parallel_jobs_give_the_same_results() {
    let cases = [case("p1", "a"), case("p2", "b"), case("p3", "c"), case("p4", "d")];
    let seq = run_all(&cases, &Fresh, &opts(0.0), 1, None, None);
    let par = run_all(&cases, &Fresh, &opts(0.0), 3, None, None);
    let ids: Vec<_> = par.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["p1", "p2", "p3", "p4"]);
    assert!(par.iter().all(|m| m.status == Status::Pass), "{par:?}");
    let s: Vec<_> = seq.into_iter().map(strip).collect();
    let p: Vec<_> = par.into_iter().map(strip).collect();
    assert_eq!(s, p);
}

#[test]
fn query_and_qty_see_what_the_plan_built() {
    let c = Case::parse(
        "id = \"q\"\ntier = \"hard\"\n[[turn]]\nsay = \"wall\"\n[[oracle.query]]\nq = \"wall\"\nmin = 1.0\nmax = 1.0\n\
         [[oracle.qty]]\nkey = \"walls\"\nvalue = 1.0\ntol = 0.0\n",
    )
    .unwrap();
    let m = run_case(&c, &Made { provider: scripted(), replay: None, rec: None }, &opts(0.0), None);
    assert_eq!(m.status, Status::Pass, "{m:?}");
    assert!(m.oracles.iter().all(|o| o.pass), "{:?}", m.oracles);
}

#[test]
fn mass_deletion_is_held_back_but_a_large_plan_is_not() {
    use crate::ai::validate::{IssueLevel, PlanIssue};
    let block = |key: &'static str, args: &[&str]| PlanIssue {
        level: IssueLevel::Block,
        item: None,
        el: None,
        text_key: key,
        args: args.iter().map(|a| a.to_string()).collect(),
    };
    let big = block("ai.issue.too_large", &["900", "500"]);
    let del = block("ai.issue.delete_large", &["900", "50"]);
    let mass = block("ai.issue.mass_delete", &["900", "1000"]);
    assert_eq!(super::run::held_back([&big].into_iter()), None);
    assert_eq!(super::run::held_back([&big, &del].into_iter()).as_deref(), Some("blocked: ai.issue.delete_large (900 of 50)"));
    assert_eq!(super::run::held_back([&mass].into_iter()).as_deref(), Some("blocked: ai.issue.mass_delete (900 of 1000)"));
    let mut warn = del.clone();
    warn.level = IssueLevel::Warn;
    assert_eq!(super::run::held_back([&warn].into_iter()), None);
}

/// H12 through the tool path: a German hospital-wing brief, built by `generate_project` with
/// services, gives air, water and power in the model; the run that once built no MEP
/// ("slab hole outside the slab") passes every count.
#[test]
fn hospital_brief_builds_air_water_and_power_through_the_tool() {
    let c = Case::parse(
        "id = \"h12\"\ntier = \"hard\"\nlang = \"de\"\n[[turn]]\nsay = \"Krankenhausfluegel, vier Geschosse, komplette Lueftung, Wasser und Strom.\"\n\
         [[oracle.count]]\nspec = \"sysk:supply\"\nmin = 1.0\n\
         [[oracle.count]]\nspec = \"mep:duct\"\nmin = 10.0\n\
         [[oracle.count]]\nspec = \"mep:pipe\"\nmin = 5.0\n\
         [[oracle.count]]\nspec = \"mep:conduit|cable_tray\"\nmin = 5.0\n",
    )
    .unwrap();
    let provider = ScriptedProvider::new(vec![
        vec![Script::Tool {
            name: "generate_project".into(),
            args: json!({"title": "Krankenhausfluegel", "spec": {"typology": "hospital", "floors": 4, "services": true}}),
        }],
        vec![Script::Text("fertig".into())],
    ]);
    let m = run_case(&c, &Made { provider, replay: None, rec: None }, &opts(0.0), None);
    assert_eq!(m.status, Status::Pass, "{m:?}");
    assert!(m.oracles.iter().all(|o| o.pass), "{:?}", m.oracles);
}
