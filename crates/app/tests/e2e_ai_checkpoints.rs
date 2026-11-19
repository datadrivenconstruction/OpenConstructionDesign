// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `mark_checkpoint` and `run_checks { since }`: after a change the agent reads only what the
//! change brought and what it cleared.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::Script;
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use ddc_model::checks::{CheckRule, CheckScope, Finding, Fix, Severity};
use ddc_model::{Error, Model};
use e2e_ai_common::*;
use serde_json::{json, Value as J};

/// Doors narrower than 0.8 m.
struct NarrowDoors;

impl CheckRule for NarrowDoors {
    fn id(&self) -> &'static str {
        "test.narrow_door"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let mut st = m
            .sql()
            .prepare("SELECT id FROM opening WHERE kind='door' AND width<0.8 ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        for id in rows {
            let key = ("opening".to_string(), id?);
            if let CheckScope::Elements(list) = scope {
                if !list.contains(&key) {
                    continue;
                }
            }
            out.push(Finding {
                rule: "test.narrow_door",
                severity: Severity::Error,
                elements: vec![key.clone()],
                at: None,
                measure: None,
                msg_key: "test.narrow",
                args: vec![],
                fix: Fix::Delete(vec![key]),
            });
        }
        Ok(())
    }
}

static NARROW: NarrowDoors = NarrowDoors;

/// One wall with one narrow door; returns the editor, the wall and the narrow door.
fn model() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wall = add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    let narrow = add_door(&mut ed, wall, 2.0);
    let spec = ddc_model::OpeningSpec {
        along: 2.0,
        width: 0.6,
        sill: 0.0,
        height: 2.1,
        kind: "door".to_string(),
    };
    ed.model.update_opening(narrow, &spec).unwrap();
    ed.reload().unwrap();
    (ed, wall, narrow)
}

fn results(ed: &mut Editor, tools: Vec<Script>) -> Vec<(J, bool)> {
    let provider = ddc_ai::fake::ScriptedProvider::new(script(tools));
    let mut s = start(ed, with_rules(cfg(provider.clone()), vec![&NARROW]));
    turn(&mut s, ed, "go", Mode::Propose);
    tool_results(&provider)
        .into_iter()
        .map(|(t, e)| (serde_json::from_str(&t).unwrap_or(J::String(t)), e))
        .collect()
}

fn narrow_door_at(wall: i64, along: f64) -> Script {
    tool(
        "create_elements",
        json!({"title": "Door", "cat": "door",
            "items": [{"wall": format!("wall:{wall}"), "along": along, "width": 0.6}]}),
    )
}

#[test]
fn since_only_new() {
    let (mut ed, wall, _) = model();
    let r = results(
        &mut ed,
        vec![
            tool("mark_checkpoint", json!({"name": "before"})),
            narrow_door_at(wall, 6.0),
            tool("run_checks", json!({"rules": ["test.narrow_door"], "since": "before"})),
            tool("run_checks", json!({"rules": ["test.narrow_door"]})),
        ],
    );
    assert_eq!(r.len(), 4, "{r:?}");
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    assert_eq!(r[0].0["findings"], 1, "the old narrow door stood at the mark: {}", r[0].0);
    assert!(r[0].0["token"].as_str().is_some_and(|t| t.starts_with("c:")));
    assert_eq!(r[2].0["total"], 1, "only the new door: {}", r[2].0);
    assert_eq!(r[2].0["baseline"], "ledger");
    assert_eq!(r[2].0["resolved"], json!([]));
    assert_eq!(r[3].0["total"], 2, "the full run still lists both");
}

#[test]
fn since_resolved_listed() {
    let (mut ed, _, narrow) = model();
    let r = results(
        &mut ed,
        vec![
            tool("mark_checkpoint", json!({"name": "m"})),
            tool("delete_elements", json!({"title": "Drop", "target": {"refs": [format!("door:{narrow}")]}})),
            tool("run_checks", json!({"rules": ["test.narrow_door"], "since": "m"})),
        ],
    );
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    assert_eq!(r[2].0["total"], 0, "{}", r[2].0);
    let gone = r[2].0["resolved"].as_array().unwrap();
    assert_eq!(gone.len(), 1, "{}", r[2].0);
    assert!(gone[0].as_str().unwrap().starts_with("test.narrow_door|"), "{gone:?}");
}

#[test]
fn since_expired_refusal_and_unknown_name() {
    let (mut ed, _, _) = model();
    let r = results(
        &mut ed,
        vec![
            tool("run_checks", json!({"since": "c:999999"})),
            tool("run_checks", json!({"since": "never_marked"})),
        ],
    );
    assert!(r[0].1 && r[1].1, "{r:?}");
    let text = |v: &J| v.to_string();
    assert!(text(&r[0].0).contains("since_expired"), "{}", r[0].0);
    assert!(text(&r[0].0).contains("run_checks"), "the fix names the call: {}", r[0].0);
    assert!(text(&r[1].0).contains("invalid_args"), "{}", r[1].0);
}

#[test]
fn since_after_undo() {
    let (mut ed, wall, _) = model();
    let r = results(
        &mut ed,
        vec![
            tool("mark_checkpoint", json!({"name": "m"})),
            narrow_door_at(wall, 6.0),
            tool("run_checks", json!({"rules": ["test.narrow_door"], "since": "m"})),
            tool("drop_item", json!({"item": 1})),
            tool("run_checks", json!({"rules": ["test.narrow_door"], "since": "m"})),
        ],
    );
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    assert_eq!(r[2].0["total"], 1, "{}", r[2].0);
    assert_eq!(r[4].0["total"], 0, "the plan went back to the mark: nothing is new: {}", r[4].0);
    assert_eq!(r[4].0["resolved"], json!([]));
}

/// One changed wall in a model of 10 000: the run over its scope, and the whole run, in ms.
#[test]
fn since_budget_10k_walls() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let mut wall = 0;
    for i in 0..10_000 {
        wall = add_wall(&mut ed, level, (0.0, i as f64 * 1.5), (4.0, i as f64 * 1.5));
    }
    let full = std::time::Instant::now();
    let r = results(
        &mut ed,
        vec![
            tool("mark_checkpoint", json!({"name": "m"})),
            tool("move_elements", json!({"title": "Move", "target": {"refs": [format!("wall:{wall}")]}, "by": [0.0, 0.4]})),
            tool("run_checks", json!({"rules": ["test.narrow_door"], "since": "m"})),
            tool("run_checks", json!({"rules": ["test.narrow_door"]})),
        ],
    );
    eprintln!("whole script {:?}", full.elapsed());
    assert!(r.iter().all(|x| !x.1), "{r:?}");
    let since = r[2].0["elapsed_ms"].as_f64().expect("since answers its time");
    assert!(r[2].0["scope_size"].as_u64().unwrap() < 10, "{}", r[2].0);
    eprintln!("run_checks since: {since:.1} ms, scope {}", r[2].0["scope_size"]);
    let budget = if cfg!(debug_assertions) { 1000.0 } else { 50.0 };
    assert!(since <= budget, "{since} ms over the {budget} ms budget");
}
