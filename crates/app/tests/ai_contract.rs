// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One table of cases, three executors: the pure core, the MCP wire and the dialog must leave
//! the same project, the same journal rows and the same refusal codes.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;
#[path = "ai_contract/executors.rs"]
mod executors;
#[path = "ai_contract/fixtures.rs"]
mod fixtures;

use ddc_ai::catalog::{self, Access};
use e2e_common::diff;
use executors::{Call, Chat, Executor, Outcome, Pure, Wire};
use serde_json::{json, Value as J};
use std::collections::BTreeSet;

const CASES: &str = include_str!("ai_contract/cases.json");

/// Write commands the table leaves to their own suites: they need a generator, a rule set or a
/// script host that the two fixture projects do not carry.
const ELSEWHERE: &[(&str, &str)] = &[
    ("generate_project", "e2e_ai_project"),
    ("revise_project", "e2e_ai_project"),
    ("generate_block", "e2e_ai_block"),
    ("run_generator", "e2e_ai_create"),
    ("set_rule", "e2e_ai_rules"),
    ("lock", "e2e_ai_rules"),
    ("unlock", "e2e_ai_rules"),
    ("regenerate", "e2e_ai_rules"),
    ("define_rule", "e2e_ai_checks"),
    ("apply_fix", "e2e_ai_flow"),
    ("define_family", "e2e_ai_geometry"),
    ("create_geometry", "e2e_ai_geometry"),
    ("run_script", "e2e_ai_script"),
    ("rerun_recipe", "e2e_ai_script"),
    ("generate_options", "e2e_ai_options"),
    ("discard_option", "e2e_ai_options"),
    ("create_issue", "e2e_ai_checks"),
    ("load_requirements", "e2e_ai_requirements"),
    ("drop_item", "the plan card: needs an item of the chat"),
    ("connect_members", "e2e_ai_steel"),
    ("create_drawing_set", "e2e_ai_drawing_set"),
    ("deliver_project", "e2e_ai_deliver"),
    ("design_plant", "e2e_ai_plant"),
    ("plan_station", "e2e_ai_station"),
    ("replan_items", "e2e_ai_readset: needs items of an earlier chat turn"),
];

struct Case {
    id: String,
    base: String,
    calls: Vec<Call>,
    expect_refused: Option<Vec<String>>,
    receipt_counts: Option<(usize, usize, usize)>,
    writes: bool,
}

fn fill(v: &J, ph: &std::collections::BTreeMap<&'static str, String>) -> J {
    match v {
        J::String(s) => {
            let mut s = s.clone();
            for (k, val) in ph {
                s = s.replace(&format!("{{{k}}}"), val);
            }
            J::String(s)
        }
        J::Array(a) => J::Array(a.iter().map(|x| fill(x, ph)).collect()),
        J::Object(o) => J::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, ph))).collect()),
        other => other.clone(),
    }
}

fn cases() -> Vec<Case> {
    let list: Vec<J> = serde_json::from_str(CASES).expect("cases.json");
    list.iter()
        .map(|c| {
            let base = c["base"].as_str().unwrap().to_string();
            let (_, ph) = fixtures::open(&base);
            Case {
                id: c["id"].as_str().unwrap().to_string(),
                calls: c["calls"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| Call {
                        tool: k["tool"].as_str().unwrap().to_string(),
                        args: fill(&k["args"], &ph),
                    })
                    .collect(),
                expect_refused: c["refused"].as_array().map(|a| {
                    a.iter().map(|x| x.as_str().unwrap().to_string()).collect()
                }),
                receipt_counts: c["receipt_counts"].as_array().map(|a| {
                    let n = |i: usize| a[i].as_u64().unwrap() as usize;
                    (n(0), n(1), n(2))
                }),
                writes: c["writes"].as_bool().unwrap_or(true),
                base,
            }
        })
        .collect()
}

fn same(case: &str, a: (&str, &Outcome), b: (&str, &Outcome)) {
    assert_eq!(a.1.refusals, b.1.refusals, "{case}: refusals {} vs {}", a.0, b.0);
    assert_eq!(a.1.journal, b.1.journal, "{case}: journal {} vs {}", a.0, b.0);
    assert_eq!(a.1.receipt, b.1.receipt, "{case}: receipt counts {} vs {}", a.0, b.0);
    assert!(
        a.1.data == b.1.data,
        "{case}: project {} vs {}{}",
        a.0,
        b.0,
        diff(&a.1.data, &b.1.data)
    );
}

fn executors() -> Vec<Box<dyn Executor>> {
    vec![Box::new(Pure), Box::new(Wire), Box::new(Chat)]
}

#[test]
fn ai_contract_all_executors_agree() {
    let all = cases();
    assert!(all.len() >= 30, "{} cases", all.len());
    for case in &all {
        let outs: Vec<(&'static str, Outcome)> = executors()
            .iter_mut()
            .map(|e| (e.name(), e.run(&case.base, &case.calls)))
            .collect();
        let first = &outs[0];
        for o in &outs[1..] {
            same(&case.id, (first.0, &first.1), (o.0, &o.1));
        }
        if let Some(want) = &case.expect_refused {
            assert_eq!(&first.1.refusals, want, "{}", case.id);
        } else {
            assert!(first.1.refusals.iter().all(|r| r.is_empty()), "{}: {:?}", case.id, first.1.refusals);
        }
        if let Some(want) = case.receipt_counts {
            assert_eq!(first.1.receipt, want, "{}: created, changed, deleted", case.id);
        }
        // a case that writes leaves exactly one journal row: one Apply, one undo step
        if case.writes {
            assert_eq!(first.1.journal.len(), 1, "{}: {:?}", case.id, first.1.journal);
        } else {
            assert!(first.1.journal.is_empty(), "{}: {:?}", case.id, first.1.journal);
        }
    }
}

#[test]
fn every_propose_tool_has_cases() {
    let used: BTreeSet<String> = cases()
        .iter()
        .flat_map(|c| c.calls.iter().map(|k| k.tool.clone()))
        .collect();
    let elsewhere: BTreeSet<&str> = ELSEWHERE.iter().map(|e| e.0).collect();
    for spec in catalog::catalog().iter().filter(|s| s.access == Access::Propose) {
        let here = used.contains(spec.name);
        let away = elsewhere.contains(spec.name);
        assert!(here || away, "{}: a new write tool needs a case in cases.json", spec.name);
        assert!(!(here && away), "{}: it has cases now, drop it from ELSEWHERE", spec.name);
    }
    for (name, _) in ELSEWHERE {
        assert!(catalog::find(name).is_some(), "{name} is no longer a tool");
    }
}

/// Every write tool refuses the same malformed call with one code on all three paths.
#[test]
fn every_propose_tool_refuses_bad_args_alike() {
    let calls: Vec<Call> = catalog::catalog()
        .iter()
        .filter(|s| s.access == Access::Propose)
        .map(|s| Call {
            tool: s.name.to_string(),
            args: json!({"title": 5}),
        })
        .collect();
    let outs: Vec<(&'static str, Outcome)> = executors()
        .iter_mut()
        .map(|e| (e.name(), e.run("walls", &calls)))
        .collect();
    for o in &outs[1..] {
        same("bad args", (outs[0].0, &outs[0].1), (o.0, &o.1));
    }
    for (c, r) in calls.iter().zip(&outs[0].1.refusals) {
        assert_eq!(r, "invalid_args", "{}", c.tool);
    }
    assert!(outs[0].1.journal.is_empty());
}

/// Three writes are one Apply, one journal row, one undo in every executor.
#[test]
fn changeset_one_undo_everywhere() {
    let all = cases();
    let case = all.iter().find(|c| c.id == "changeset_of_three").unwrap();
    assert_eq!(case.calls.len(), 3);
    for mut e in executors() {
        let out = e.run(&case.base, &case.calls);
        assert_eq!(out.journal.len(), 1, "{}: {:?}", e.name(), out.journal);
    }
}
