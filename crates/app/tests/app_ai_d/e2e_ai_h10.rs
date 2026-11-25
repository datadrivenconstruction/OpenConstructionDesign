// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H10, the 90 m arch footbridge, through the assistant's tool path: help finds the bridge
//! recipe, the generator is run from it, the plan is applied (one undo step) and every oracle
//! of the H10 case is read off the saved model.

use crate::{e2e_ai_common, e2e_common};

use ddc_ai::Mode;
use ddc_app::ai::eval::case::{evals_dir, load_dir};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{blocking_findings, evaluate, EvalCx};
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

#[test]
fn h10_footbridge_meets_every_oracle_of_its_case() {
    let case = load_dir(&evals_dir().join("cases")).unwrap().into_iter().find(|c| c.id == "H10_footbridge_arch").expect("the H10 case");
    let mut ed = Editor::new_in_memory().unwrap();
    if count(&ed.model, "level") == 0 {
        ed.try_add_level("Ground", 0.0).unwrap();
    }
    let errors_before = blocking_findings(&ed.model).unwrap();
    let (mut s, provider) = {
        let provider = ddc_ai::fake::ScriptedProvider::new(script(vec![
            tool("help", json!({"topic": "arch_span"})),
            tool("load_tools", json!({"groups": ["scripts"]})),
            tool("run_generator", json!({"id": "steel_frame", "params": {"kind": "arch_bridge", "span": 90.0, "rise": 15.0, "deck_z": 6.0, "width": 4.0}})),
        ]));
        let mut s = start(&mut ed, cfg(provider.clone()));
        turn(&mut s, &mut ed, &case.turns[0].say, Mode::Propose);
        (s, provider)
    };
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let (help, generated) = (&results[0].0, &results[2].0);
    assert!(help.contains("arch_bridge") && help.contains("approach") && help.contains("guard"), "the recipe names the approach and the guards: {help}");
    assert!(generated.contains("deck_freq_hz"), "the vibration check is reported: {generated}");
    apply_and_check(&mut s, &mut ed);

    // the answer is what the generator reported
    let answer = format!("Deck vibration and deflection checked.\n{generated}");
    let facts = Facts::read(&ed.model).unwrap();
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer: &answer, ifc: ifc.as_deref(), errors_before, receipts: &[] };
    let results = evaluate(&case, &cx);
    assert!(!results.is_empty());
    let failed: Vec<_> = results.iter().filter(|r| !r.pass).map(|r| format!("{}: expected {}, measured {}", r.name, r.expected, r.measured)).collect();
    assert!(failed.is_empty(), "H10 oracles failed:\n{}", failed.join("\n"));

    // the approach and guards are in the model: two ramps, two deck guards and four ramp guards
    assert_eq!(count(&ed.model, "ramp"), 2);
    assert_eq!(count(&ed.model, "railing"), 6);
}
