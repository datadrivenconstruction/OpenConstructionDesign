// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `deliver_project`: one call drafts the building and its drawing set on the scratch copy and
//! names the files; one Apply writes the model and the sheets in one undo step, and the run that
//! follows writes the PDF and the IFC next to each other.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::deliver::{Kind, Outputs, Run, State, Target};
use ddc_app::editor::Editor;
use ddc_io::ifc::IfcSchema;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;
use std::time::{Duration, Instant};

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

fn spec() -> serde_json::Value {
    json!({"typology": "office", "floors": 3, "lang": "en"})
}

#[test]
fn one_call_drafts_the_building_and_its_sheets_then_writes_the_files() {
    let mut ed = editor();
    let call = tool(
        "deliver_project",
        json!({
            "why": "a small office with drawings and IFC",
            "spec": spec(),
            "drawings": {"language": "en"},
            "outputs": ["pdf", "ifc"],
            "ifc_schema": "IFC4"
        }),
    );
    let (mut s, p) = run(&mut ed, vec![call]);
    let results = tool_results(&p);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let reply = parsed(&results[0].0);
    assert_eq!(reply["outputs"], json!(["pdf", "ifc"]));
    assert_eq!(reply["ifc_schema"], "IFC4");
    assert!(reply["project"].is_object() && reply["drawings"].is_object(), "{reply}");
    let plan = plan_of(&s);
    let want = Outputs {
        pdf: true,
        ifc: true,
        schema: IfcSchema::Ifc4,
    };
    assert_eq!(plan.deliver, Some(want), "the card carries the files");
    assert_eq!(plan.items.len(), 2, "the project and its drawing set");
    assert_eq!(count(&ed.model, "wall"), 0, "nothing reaches the model before Apply");
    assert!(ed.model.sheets().unwrap().is_empty());

    apply_and_check(&mut s, &mut ed);
    assert!(count(&ed.model, "wall") > 0);
    let sheets = ed.model.drawing_set_sheets().unwrap();
    assert!(sheets.len() >= 5, "{} sheets", sheets.len());
    assert_eq!(
        ddc_app::deliver::sheets_to_print(&ed),
        sheets.iter().map(|s| s.0).collect::<Vec<_>>(),
        "the run prints the drawing set"
    );

    let dir = std::env::temp_dir().join(format!("ddc-e2e-deliver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let target = Target {
        dir: dir.clone(),
        stem: "Office".into(),
    };
    let mut run = Run::start(want, target, ddc_app::deliver::sheets_to_print(&ed), false);
    let end = Instant::now() + Duration::from_secs(240);
    while !run.step(&ed) {
        assert!(Instant::now() < end, "the run did not end: {:?}", run.rows());
        std::thread::sleep(Duration::from_millis(2));
    }
    for row in run.rows() {
        assert!(matches!(row.state, State::Done { .. }), "{row:?}");
    }
    let pdf = run.file_of(Kind::Print).unwrap();
    assert_eq!(pdf, dir.join("Office.pdf"));
    assert!(std::fs::read(pdf).unwrap().starts_with(b"%PDF"));
    let ifc = std::fs::read_to_string(run.file_of(Kind::Ifc).unwrap()).unwrap();
    let header = ifc.lines().find(|l| l.starts_with("FILE_SCHEMA")).unwrap_or_default();
    assert!(header.contains("'IFC4'"), "IFC4 header: {header}");
    assert!(ifc.contains("IFCWALL"), "walls in the IFC");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drawings_default_on_and_files_only_without_outputs() {
    let mut ed = editor();
    let call = tool(
        "deliver_project",
        json!({"why": "drawings only", "spec": spec(), "outputs": []}),
    );
    let (s, p) = run(&mut ed, vec![call]);
    let results = tool_results(&p);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let plan = plan_of(&s);
    assert_eq!(plan.items.len(), 2, "the drawing set is on by default");
    assert_eq!(plan.deliver, None, "no files: a plain card");
}

#[test]
fn contradictions_are_refused_and_leave_no_draft() {
    let mut ed = editor();
    for (args, needle) in [
        (
            json!({"why": "x", "spec": spec(), "drawings": false, "outputs": ["pdf"]}),
            "drawing set",
        ),
        (
            json!({"why": "x", "spec": spec(), "outputs": ["dwg"]}),
            // the catalog refuses it before the tool runs
            "pdf, ifc",
        ),
    ] {
        let (s, p) = run(&mut ed, vec![tool("deliver_project", args)]);
        let results = tool_results(&p);
        assert!(results[0].1, "refused: {results:?}");
        assert!(results[0].0.contains(needle), "{}", results[0].0);
        // a refused call leaves no plan at all, or an empty one
        let draft = s.plan().is_some_and(|p| !p.items.is_empty() || p.deliver.is_some());
        assert!(!draft, "no draft");
    }
}
