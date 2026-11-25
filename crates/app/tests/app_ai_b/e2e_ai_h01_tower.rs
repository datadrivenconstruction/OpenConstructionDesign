// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H01 and H11 (the same tower asked in Russian), the 300 m lattice tower, through the chat the way a model is told to build it: the
//! recipe from `help`, the steel_frame generator (legs, bracing, platforms with levels and
//! decks, stairs, lifts), then connect_members at every node; Apply once, then every oracle
//! of the case on the applied model, with the reply built from the model: sway from the generator, counts from the facts.

use crate::{app2_fx, e2e_ai_common};

use app2_fx::fx;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::Mode;
use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{evaluate, in_range, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::ai::session::UiEvent;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;
use std::time::Duration;

/// A 300 m tower with its connections takes minutes in a debug build.
const LONG: Duration = Duration::from_secs(1800);

/// The number a generator metric carries in a tool result (`"key": 123.0`, escaped or not).
fn metric(text: &str, key: &str) -> Option<f64> {
    let rest = &text[text.find(key)? + key.len()..];
    let rest = rest.trim_start_matches(|c: char| !(c.is_ascii_digit() || c == '-'));
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The reply as the model would give it from what it built: sway from the generator's
/// `sway_top_mm`, every count read from the applied model.
struct Said {
    sway_mm: f64,
    levels: i64,
    slabs: i64,
    stairs: i64,
    lifts: i64,
    conns: i64,
    parts: i64,
}

fn said(facts: &Facts, sway_mm: f64) -> Said {
    let n = |spec: &str| facts.count(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
    Said {
        sway_mm,
        levels: n("level"),
        slabs: n("slab"),
        stairs: n("stair"),
        lifts: n("comp:lift|elevator"),
        conns: n("steel_conn"),
        parts: n("steel_conn_part"),
    }
}

#[test]
fn h01_tower_through_the_tools_meets_every_oracle() {
    tower("H01_lattice_tower_300.toml", "Plates and bolts at the nodes", |s| {
        format!(
            "Built the 300 m tower on curved legs: {} levels with {} platform decks, {} stairs and {} lifts in the legs; \
             the wind sway at the top is about {:.0} mm (preliminary). {} connections with {} gusset plates and bolts at the nodes.",
            s.levels, s.slabs, s.stairs, s.lifts, s.sway_mm, s.conns, s.parts
        )
    });
}

#[test]
fn h11_russian_tower_through_the_tools_meets_every_oracle() {
    tower("H11_ru_lattice_tower.toml", &fx("tower_ru_title"), |s| {
        let vals = [
            s.levels.to_string(),
            s.slabs.to_string(),
            s.stairs.to_string(),
            s.lifts.to_string(),
            format!("{:.0}", s.sway_mm),
            s.conns.to_string(),
            s.parts.to_string(),
        ];
        vals.iter()
            .fold(fx("tower_ru_reply"), |t, v| t.replacen("{}", v, 1))
    });
}

fn tower(file: &str, connect_title: &str, reply: fn(&Said) -> String) {
    let case = Case::load(&evals_dir().join("cases").join(file)).unwrap();
    let brief = &case.turns[0].say;
    let calls = vec![
        tool("help", json!({"topic": "lattice_frame"})),
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool(
            "run_generator",
            json!({"id": "steel_frame", "variants": 1, "params": {"kind": "lattice_tower_300", "pattern": "x",
                   "tower_height": 300, "base_width": 125, "platform_1": 57, "platform_2": 115, "platform_3": 276}}),
        ),
        tool("load_tools", json!({"groups": ["steel"]})),
        tool("connect_members", json!({"title": connect_title})),
    ];
    let mut turns: Vec<Vec<Script>> = calls.into_iter().map(|t| vec![t]).collect();
    // the turn ends on a reply; the one judged is built from the model below
    turns.push(vec![Script::Text("Built.".into())]);
    let p = ScriptedProvider::new(turns);

    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let mut s = start(&mut ed, cfg(p.clone()));
    let view = view_of(&ed);
    s.send_turn(&mut ed, brief.clone(), view, Mode::Propose).unwrap();
    assert!(s.wait_idle(&mut ed, LONG), "the turn did not finish");
    let mut answer = String::new();
    let mut refused = vec![];
    for e in s.drain_events() {
        match e {
            UiEvent::Text(x) => answer.push_str(&x),
            UiEvent::Tool { name, ok: Some(false), err, .. } => refused.push((name, err)),
            _ => {}
        }
    }
    assert!(refused.is_empty(), "{refused:?}");
    let results = tool_results(&p);
    assert_eq!(results.len(), 5, "{results:?}");
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert!(results[0].0.contains("steel_frame") && results[0].0.contains("connect_members"), "{}", results[0].0);

    let plan = plan_of(&s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(&mut s, &mut ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    drop(s);

    let facts = Facts::read(&ed.model).unwrap();
    let mut short = vec![];
    for c in &case.oracle.count {
        match facts.count(&c.spec) {
            Ok(n) if in_range(n as f64, c.min, c.max) => {}
            Ok(n) => short.push(format!("{}: {n} (min {:?}, max {:?})", c.spec, c.min, c.max)),
            Err(e) => short.push(format!("{}: {e}", c.spec)),
        }
    }
    assert!(short.is_empty(), "count oracles: {short:#?}");

    let sway = metric(&results[2].0, "sway_top_mm").unwrap_or_else(|| panic!("no sway_top_mm in {}", results[2].0));
    answer = reply(&said(&facts, sway));
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx {
        ed: &ed,
        facts: &facts,
        before: None,
        answer: &answer,
        ifc: ifc.as_deref(),
        errors_before: 0,
        receipts: &receipts,
    };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{failed:#?}");
}
