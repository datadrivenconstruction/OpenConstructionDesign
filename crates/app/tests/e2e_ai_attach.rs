// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Attached files end to end: the scripted model lists the attachments, reads them and traces
//! walls from a DXF plan into the draft; nothing reaches the model before Apply, and a file
//! kept on the machine is neither listed nor readable.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;
#[path = "app2_fx/mod.rs"]
mod app2_fx;

use app2_fx::fx;

use ddc_app::ai::attach;
use ddc_app::editor::Editor;
use ddc_ai::fake::ScriptedProvider;
use ddc_ai::Mode;
use ddc_io::drawing_model::fixture::sample_plan_text;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;
use std::sync::Arc;

fn editor() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let outer = ed.model.add_wall_type("Outer 300", &concrete(0.3)).unwrap();
    let inner = ed.model.add_wall_type("Inner 200", &concrete(0.2)).unwrap();
    ed.reload().unwrap();
    (ed, outer, inner)
}

fn plan(id: u32) -> attach::AttachData {
    attach::from_bytes("plan.dxf", sample_plan_text(0).into_bytes(), id).unwrap()
}

#[test]
fn walls_are_traced_from_a_dxf_underlay_and_applied() {
    let (mut ed, outer, inner) = editor();
    let provider = ScriptedProvider::new(script(vec![
        tool("attachments_list", json!({})),
        tool("attachments_read", json!({"id": 7})),
        tool("underlay_trace", json!({"title": "Walls from the plan", "id": 7})),
    ]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    let brief = attach::from_bytes("brief.txt", fx("attach_floors_1").into_bytes(), 8).unwrap();
    let fact = ddc_io::attach::facts::from_extracted("brief.txt", brief.kind, &brief.ex);
    assert_eq!(fact.len(), 1);
    s.set_attachments(vec![Arc::new(plan(7)), Arc::new(brief)], fact);
    turn(&mut s, &mut ed, "build the walls of the attached plan", Mode::Propose);

    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let list = parsed(&results[0].0);
    assert_eq!(list["count"], json!(2), "{list}");
    assert_eq!(list["files"][0]["name"], json!("plan.dxf"));
    assert_eq!(list["files"][0]["kind"], json!("dxf"));
    assert!(list["files"][0]["lines"].as_u64().unwrap() > 0);
    assert_eq!(list["requirements_confirmed"], json!(true));
    assert!(list["requirements"][0].as_str().unwrap().starts_with("storeys: 1 [brief.txt"), "{list}");
    assert_eq!(parsed(&results[1].0)["kind"], json!("dxf"));

    let traced = parsed(&results[2].0);
    let created = traced["created"].as_array().unwrap_or_else(|| panic!("{traced}"));
    assert_eq!(created.len(), 5, "4 outer walls and a partition: {traced}");
    assert_eq!(count(&ed.model, "wall"), 0, "nothing reaches the live model before Apply");

    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), 5);
    let q = |sql: &str| -> f64 { ed.model.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
    assert!(q("SELECT min(min(ax, bx)) FROM wall").abs() < 0.02);
    assert!((q("SELECT max(max(ax, bx)) FROM wall") - 12.0).abs() < 0.02);
    assert!((q("SELECT max(max(ay, by)) FROM wall") - 8.0).abs() < 0.02);
    let types: Vec<i64> = {
        let mut st = ed.model.sql().prepare("SELECT wtype FROM wall ORDER BY wtype").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(types.iter().filter(|t| **t == outer).count(), 4, "{types:?}");
    assert_eq!(types.iter().filter(|t| **t == inner).count(), 1, "{types:?}");
}

#[test]
fn a_file_kept_on_the_machine_is_neither_listed_nor_read() {
    let (mut ed, _, _) = editor();
    let provider = ScriptedProvider::new(script(vec![
        tool("attachments_list", json!({})),
        tool("attachments_read", json!({"id": 3})),
        tool("underlay_trace", json!({"title": "Walls", "id": 3})),
    ]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    let mut d = plan(3);
    d.shared = false;
    s.set_attachments(vec![Arc::new(d)], vec![]);
    turn(&mut s, &mut ed, "read the plan", Mode::Propose);
    let results = tool_results(&provider);
    let list = parsed(&results[0].0);
    assert_eq!(list["count"], json!(0), "{list}");
    assert_eq!(list["kept_on_machine"], json!(1), "{list}");
    assert!(results[1].1, "reading a local file is refused: {results:?}");
    assert!(results[2].1, "tracing a local file is refused: {results:?}");
    assert!(s.plan().is_none_or(|p| p.items.is_empty()));
}

/// One A4 page whose content is `content`, with a classic xref table.
fn pdf_page(content: &str) -> Vec<u8> {
    use std::io::Write;
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        write!(out, "{} 0 obj\n{o}\nendobj\n", i + 1).unwrap();
    }
    let x = out.len();
    write!(out, "xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).unwrap();
    for o in offs {
        write!(out, "{o:010} 00000 n \n").unwrap();
    }
    write!(out, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).unwrap();
    out
}

#[test]
fn a_pdf_without_a_scale_asks_for_one() {
    let (mut ed, _, _) = editor();
    let d = attach::from_bytes("sheet.pdf", pdf_page("0 0 m 100 0 l S"), 4).unwrap();
    assert!(d.ex.scale.is_none());
    let provider = ScriptedProvider::new(script(vec![tool("underlay_trace", json!({"title": "Walls", "id": 4}))]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    s.set_attachments(vec![Arc::new(d)], vec![]);
    turn(&mut s, &mut ed, "trace", Mode::Propose);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("scale"), "{results:?}");
}
