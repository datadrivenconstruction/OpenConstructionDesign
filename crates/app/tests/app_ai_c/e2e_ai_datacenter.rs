// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H07 through the tool path: the data centre brief goes to generate_project, Apply writes it in
//! one undo step, and every oracle of the H07 case holds on the applied model.

use crate::{e2e_ai_common, e2e_common};

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{evaluate, EvalCx};
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

#[test]
fn the_h07_data_centre_meets_every_oracle_after_apply() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let case = Case::load(&evals_dir().join("cases").join("H07_datacenter.toml")).unwrap_or_else(|e| panic!("{e}"));
    let hall = "Data hall";
    let spec = json!({
        "typology": "office", "floors": 1, "floor_height": 5.0, "band_depth": 24.0,
        "footprint": {"width": 72.0},
        "rooms": [
            {"name": hall, "kind": "technical", "area": 720.0, "count": 3},
            {"name": "UPS room", "kind": "technical", "area": 150.0, "count": 2},
            {"name": "Battery room", "kind": "technical", "area": 100.0, "count": 2},
            {"name": "Chiller plant", "kind": "technical", "area": 120.0}
        ],
        "rules": {"dc.racks": 600}
    });
    let runs0 = count(&ed.model, "mep_run");
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(count(&ed.model, "mep_run"), runs0, "nothing reaches the model before Apply");
    apply_and_check(&mut s, &mut ed);
    assert!(count(&ed.model, "mep_run") > runs0);

    let facts = Facts::read(&ed.model).unwrap_or_else(|e| panic!("{e}"));
    let ifc = ddc_io::ifc::export_ifc(&ed.model).unwrap();
    // the redundancy the model shows, as the reply would state it: counts read from the applied
    // model, N+1 only where a unit beyond the duty set exists
    let n = |spec: &str| facts.count(spec).unwrap_or_else(|e| panic!("{e}"));
    let (chillers, ups, gens, crah) = (n("comp:chiller"), n("comp:ups"), n("comp:generator|genset"), n("comp:crah|crac|cooling unit|cooler"));
    let scheme = |units: i64, what: &str| {
        if units >= 2 { format!("{what}: {units} units, one spare (N+1)") } else { format!("{what}: {units} unit, no redundancy") }
    };
    let answer = format!(
        "Redundant: {}; {}; {}; {} cooling units with a spare. Not redundant: the cable trays and the hall slab.",
        scheme(chillers, "chillers"),
        scheme(ups, "UPS"),
        scheme(gens, "generators"),
        crah
    );
    assert!(answer.contains("N+1"), "{answer}");
    let answer = answer.as_str();
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer, ifc: Some(&ifc), errors_before: 0, receipts: &[] };
    let results = evaluate(&case, &cx);
    assert!(!results.is_empty());
    let bad: Vec<_> = results.iter().filter(|r| !r.pass).map(|r| format!("{}: want {}, got {}", r.name, r.expected, r.measured)).collect();
    assert!(bad.is_empty(), "failed oracles:\n{}", bad.join("\n"));
}
