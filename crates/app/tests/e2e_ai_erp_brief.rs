// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A brief the person confirmed in the ERP (bridge `ai_result`) reaches the model the way the
//! desktop assistant's does: one tool call drafts it as a plan item, the preview is reviewed,
//! Apply writes it in one undo step. A second brief revises the applied project in place.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_app::link::gen::brief_tool_call;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

fn brief() -> J {
    serde_json::from_str(include_str!("../../io/contract/examples/brief.de-residential.json")).unwrap()
}

fn checked(call: &J) {
    let name = call["name"].as_str().unwrap();
    let spec = ddc_ai::catalog::find(name).expect("a catalog tool");
    ddc_ai::catalog::validate(spec, &call["args"]).unwrap_or_else(|e| panic!("{name}: {e}"));
}

#[test]
fn an_erp_brief_is_drafted_previewed_and_applied_like_the_assistants() {
    let mut ed = editor();
    let call = brief_tool_call(&ed.model, &brief()).unwrap();
    assert_eq!(call["name"], "generate_project");
    checked(&call);

    let walls0 = count(&ed.model, "wall");
    let (mut s, provider) = run(&mut ed, vec![tool(call["name"].as_str().unwrap(), call["args"].clone())]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let plan = plan_of(&s);
    assert_eq!(plan.items.len(), 1, "one item to review");
    assert_eq!(count(&ed.model, "wall"), walls0, "nothing written before Apply");
    apply_and_check(&mut s, &mut ed);
    assert!(count(&ed.model, "wall") > walls0, "Apply built the brief");
    drop(s);

    // the next confirmed brief revises the stored project instead of adding a second building
    let mut b = brief();
    b["massing"]["floors_above"] = json!(4);
    let call = brief_tool_call(&ed.model, &b).unwrap();
    assert_eq!(call["name"], "revise_project");
    assert_eq!(call["args"]["project"], "P1");
    checked(&call);
}

#[test]
fn a_brief_off_the_contract_gives_no_tool_call() {
    let ed = editor();
    assert!(brief_tool_call(&ed.model, &json!({"schema": "ddc-brief/1", "nonsense": true})).is_err());
}
