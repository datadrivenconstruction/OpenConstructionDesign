// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! G15, the storage tank with nozzles, through the chat: `help` for the design_plant tool,
//! `design_plant` with the case's brief, Apply once, then every oracle of the case on the
//! applied model (tank, nozzles and manway, ring footings, storey), each count reported.

use crate::e2e_ai_common;

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{evaluate, in_range, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const ANSWER: &str = "Storage tank TK-1 designed: 12 m diameter, 14 m shell under a cone roof, three shell nozzles \
DN 150 at 2.1, 7 and 11.9 m facing different ways and one on the roof, a 0.6 m manway, a caged ladder, and a ring \
foundation 1 m wide under the shell.";

#[test]
fn g15_storage_tank_through_the_tools_meets_every_oracle() {
    let case = Case::load(&evals_dir().join("cases").join("G15_tank_nozzles.toml")).unwrap();
    let brief = case.turns[0].say.clone();
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let calls = vec![
        tool("help", json!({"topic": "design_plant"})),
        tool("load_tools", json!({"groups": ["plant"]})),
        tool("design_plant", json!({"why": "storage tank with nozzles", "brief": brief})),
    ];
    let n = calls.len();
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert_eq!(res.len(), n, "{res:?}");
    assert!(res.iter().all(|r| !r.1), "{res:?}");

    let plan = plan_of(&s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(&mut s, &mut ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    drop(s);

    let facts = Facts::read(&ed.model).unwrap();
    let mut short = vec![];
    for c in &case.oracle.count {
        match facts.count(&c.spec) {
            Ok(v) if in_range(v as f64, c.min, c.max) => {}
            Ok(v) => short.push(format!("{}: {v} (min {:?}, max {:?})", c.spec, c.min, c.max)),
            Err(e) => short.push(format!("{}: {e}", c.spec)),
        }
    }
    assert!(short.is_empty(), "count oracles: {short:#?}");
    assert_eq!(facts.count("strip_footing").unwrap(), 4, "the ring is four arcs");
    assert!(facts.count("comp:nozzle|manway|flange").unwrap() >= 5);
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer: ANSWER, ifc: ifc.as_deref(), errors_before: 0, receipts: &receipts };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{failed:#?}");
}
