// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H14, one more viewing platform on the lattice tower, through the chat: the tower is built
//! and applied first (the case's base), then a second session adds the platform at 200 m with
//! steel_frame tower_platform, which brings its own deck slab and railing; Apply once, then every oracle
//! of the case on the applied model, held against the base.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{blocking_findings, evaluate, in_range, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const ANSWER: &str = "Added one viewing platform at 200 m: its perimeter trusses and floor grid sit on the \
                      tower's corner chords, with its deck slab and edge railing. Nothing else on the tower changed.";

#[test]
fn h14_added_platform_through_the_tools_meets_every_oracle() {
    let case = Case::load(&evals_dir().join("cases").join("H14_tower_add_floor.toml")).unwrap();
    let mut ed = Editor::new_in_memory().unwrap();

    // the base: the tower as H01 builds it
    let (mut s, p) = run(
        &mut ed,
        vec![
            tool("load_tools", json!({"groups": ["scripts"]})),
            tool("run_generator", json!({"id": "steel_frame", "variants": 1, "params": {"kind": "lattice_tower_300"}})),
        ],
    );
    let res = tool_results(&p);
    assert_eq!(res.len(), 2, "{res:?}");
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let before = Facts::read(&ed.model).unwrap();
    let count = |ed: &Editor, t: &str| -> i64 { ed.model.sql().query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0)).unwrap() };
    let (slabs0, rails0) = (count(&ed, "slab"), count(&ed, "railing"));
    let errors_before = blocking_findings(&ed.model).unwrap_or(0);

    // the edit: the platform with its deck slab and railing, all from the generator
    let calls = vec![
        tool("load_tools", json!({"groups": ["scripts"]})),
        tool("run_generator", json!({"id": "steel_frame", "variants": 1, "params": {"kind": "tower_platform", "at": 200}})),
    ];
    let n = calls.len();
    let (mut s, p) = run(&mut ed, calls);
    let res = tool_results(&p);
    assert_eq!(res.len(), n, "{res:?}");
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    let plan = plan_of(&s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(&mut s, &mut ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    drop(s);
    assert_eq!(count(&ed, "slab") - slabs0, 1, "the deck slab");
    assert_eq!(count(&ed, "railing") - rails0, 1, "its edge railing");

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
        before: Some(&before),
        answer: ANSWER,
        ifc: ifc.as_deref(),
        errors_before,
        receipts: &receipts,
    };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{failed:#?}");
}
