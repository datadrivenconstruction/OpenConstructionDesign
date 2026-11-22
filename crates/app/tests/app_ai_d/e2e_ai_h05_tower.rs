// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H05, the 40-storey office tower with core and fire strategy, through the chat the way a model
//! is told to build it: the building_brief recipe from `help`, then generate_project with the
//! brief's plate, lifts, sprinklers and curtain wall; Apply once, then every oracle of the case
//! on the applied model, each count oracle named with its number.

use crate::{e2e_ai_common, e2e_common};

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

/// A 40-storey tower with its services takes minutes in a debug build.
const LONG: Duration = Duration::from_secs(1800);
const ANSWER: &str = "Built a 40-storey office tower on a 45 x 45 m plate around a reinforced concrete core \
                      with 8 lifts (one serves the fire brigade) and a glazed curtain wall.\n\
                      Egress: 2 protected stairs on every storey, far apart in the core, each in its own rated \
                      enclosure with rated doors, so every floor has 2 ways out.\n\
                      Fire strategy: each floor is its own fire compartment with rated walls (EI) and doors; \
                      automatic sprinklers on every floor are fed from risers in the core, with wet risers \
                      for the fire brigade.";

#[test]
fn h05_tower_through_the_tools_meets_every_oracle() {
    let case = Case::load(&evals_dir().join("cases").join("H05_tower40_core_fire.toml")).unwrap();
    let brief = &case.turns[0].say;
    let spec = json!({
        "typology": "office",
        "floors": 40,
        "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "lifts": 8,
        "sprinklers": true,
        "design": {"facade": "curtain"},
        "features": ["curtain wall", "sprinklers"]
    });
    let calls = vec![
        tool("help", json!({"topic": "building_brief"})),
        tool("generate_project", json!({"title": "40-storey office tower", "spec": spec})),
    ];
    let mut turns: Vec<Vec<Script>> = calls.into_iter().map(|t| vec![t]).collect();
    turns.push(vec![Script::Text(ANSWER.into())]);
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
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert!(results[0].0.contains("typology"), "{}", results[0].0);

    let plan = plan_of(&s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(&mut s, &mut ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    drop(s);

    // by construction: the builder's own project for the spec, against what the model holds
    let gs = ddc_gen::project::parse_spec(&spec).unwrap();
    let gp = ddc_gen::project::build_project(&gs).unwrap();
    let flights: usize = gp.storeys.iter().map(|st| st.layout.stairs.len()).sum();
    assert!(ddc_gen::project::fire::stairs_to_roof(&gs), "a 40-storey tower carries its stairs to the roof");
    assert_eq!(flights, 80, "two flights on each of the 40 storeys");
    assert_eq!(e2e_common::count(&ed.model, "stair"), flights as i64);
    let top = gp.storeys.last().unwrap();
    assert_eq!(gp.fire.stair_heads.len(), top.layout.stairs.len(), "a head over every top flight: {:?}", gp.fire.head_issues);
    assert!(gp.fire.head_issues.is_empty(), "{:?}", gp.fire.head_issues);
    assert_eq!(e2e_common::count(&ed.model, "roof"), 1 + gp.extra_roofs.len() as i64, "the roof parted round the heads");
    assert!(!gp.extra_roofs.is_empty(), "the roof opens over the heads");
    let walls: i64 = gp.fire.stair_heads.iter().map(|h| h.ring.len() as i64).sum();
    let (held, low): (i64, f64) = ed
        .model
        .sql()
        .query_row("SELECT count(*), coalesce(min(height), 0) FROM wall WHERE top_auto=2 AND top_profile IS NULL AND top_roof IS NULL", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let least = gp.fire.stair_heads.iter().map(|h| h.height).fold(f64::INFINITY, f64::min);
    assert_eq!(held, walls, "every head wall is let go of the roof trim");
    assert!(low >= least - 1e-6, "head walls keep their height: {low} against {least}");
    let g = ddc_gen::project::check_project(&gs, &gp);
    assert!(g.passed(), "{:?}", g.failures);
    assert!(!g.warnings.iter().any(|w| w.starts_with("fire:")), "{:?}", g.warnings);
    assert!(gp.fire.heads >= 400, "sprinkler heads: {}", gp.fire.heads);

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
