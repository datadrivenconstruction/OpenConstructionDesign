// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A block of buildings from the chat: a scripted model asks for twelve plots between streets,
//! the tool builds every building in parallel under the shared site rules, drafts one project
//! per plot, and Apply writes the whole block in one undo step.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::project_plan;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};
use std::time::Instant;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

/// 6 x 2 plots of 26 x 30 m between four streets 12 m wide.
fn block_args() -> J {
    let (w, d, cols) = (26.0, 30.0, 6);
    let mut plots = vec![];
    for row in 0..2 {
        for c in 0..cols {
            let (x, y) = (c as f64 * w, row as f64 * d);
            plots.push(json!({"id": format!("{}{}", ["S", "N"][row], c + 1),
                "ring": [[x, y], [x + w, y], [x + w, y + d], [x, y + d]]}));
        }
    }
    let (x1, y1, s) = (cols as f64 * w, 2.0 * d, 12.0);
    let rect = |a: f64, b: f64, c: f64, e: f64| json!([[a, b], [c, b], [c, e], [a, e]]);
    json!({"title": "Block", "why": "a block of small buildings",
        "plots": plots,
        "streets": [rect(-s, -s, x1 + s, 0.0), rect(-s, y1, x1 + s, y1 + s),
                    rect(-s, 0.0, 0.0, y1), rect(x1, 0.0, x1 + s, y1)],
        "rules": {"block.storeys_min": 1, "block.storeys_max": 2, "block.far_target": 0.8}})
}

#[test]
fn a_twelve_plot_block_is_drafted_and_applied_in_one_step() {
    let mut ed = editor();
    let t = Instant::now();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_block", block_args())]);
    let drafted = t.elapsed().as_secs_f64();
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let r = parsed(&results[0].0);
    assert_eq!(r["buildings"], 12, "{r}");
    assert_eq!(r["block"]["passed"], true, "{r}");
    assert!(r["block"]["min_gap_m"].as_f64().unwrap() >= r["block"]["rules"]["block.spacing"].as_f64().unwrap());
    assert_eq!(plan_of(&s).items.len(), 12, "one item per building");
    assert_eq!(count(&ed.model, "wall"), 0, "a draft never touches the model");
    let t = Instant::now();
    apply_and_check(&mut s, &mut ed);
    eprintln!(
        "e2e block 12 plots: draft {drafted:.2} s (tool ms {}), apply+undo+redo {:.2} s",
        r["ms"],
        t.elapsed().as_secs_f64()
    );
    let stored = project_plan::stored(&ed.model).unwrap();
    assert_eq!(stored.len(), 12, "{:?}", stored.iter().map(|p| &p.0).collect::<Vec<_>>());
    assert!(count(&ed.model, "wall") > 12 * 4);
}

#[test]
fn a_block_that_breaks_its_rules_drafts_nothing() {
    let mut ed = editor();
    let mut args = block_args();
    args["rules"]["block.spacing"] = json!(59);
    let (s, provider) = run(&mut ed, vec![tool("generate_block", args)]);
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("block."), "{}", results[0].0);
    assert!(s.plan().map_or(true, |p| p.items.is_empty()));
}

/// The live city-block scenario as one call: four buildings with the storeys the brief asks,
/// 12 m apart, inside 120 x 90 m between streets.
fn city_block_args() -> J {
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| json!([[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
    let s = 12.0;
    json!({"title": "City block", "why": "four buildings of the brief, 12 m apart",
        "plots": [
            {"id": "N", "ring": rect(0.0, 62.0, 96.0, 90.0),
             "overrides": {"typology": "apartment_section", "floors": 9, "name": "Apartments"}},
            {"id": "E", "ring": rect(84.0, 0.0, 120.0, 48.0),
             "overrides": {"typology": "office", "floors": 6, "name": "Office"}},
            {"id": "SW", "ring": rect(0.0, 0.0, 44.0, 40.0),
             "overrides": {"typology": "school", "floors": 3, "name": "School"}},
            {"id": "M", "ring": rect(56.0, 12.0, 76.0, 46.0),
             "overrides": {"typology": "office", "floors": 2, "name": "Shop pavilion",
                           "ground_rooms": [{"kind": "retail", "area": 80, "count": 2}]}},
        ],
        "streets": [rect(-s, -s, 120.0 + s, 0.0), rect(-s, 90.0, 120.0 + s, 90.0 + s),
                    rect(-s, 0.0, 0.0, 90.0), rect(120.0, 0.0, 120.0 + s, 90.0)],
        "rules": {"block.spacing": 12}})
}

#[test]
fn the_city_block_brief_is_one_call_with_the_asked_storeys_and_gaps() {
    let mut ed = editor();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_block", city_block_args())]);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 1, "one tool call for the block");
    assert!(!results[0].1, "{}", results[0].0);
    let r = parsed(&results[0].0);
    assert_eq!(r["buildings"], 4, "{r}");
    assert!(r["block"]["min_gap_m"].as_f64().unwrap() + 1e-3 >= 12.0, "{r}");
    let floors: Vec<(String, u64)> = r["per_plot"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["plot"].as_str().unwrap().to_string(), p["facts"]["floors"].as_u64().unwrap()))
        .collect();
    let want: Vec<(String, u64)> = [("N", 9), ("E", 6), ("SW", 3), ("M", 2)].iter().map(|x| (x.0.to_string(), x.1)).collect();
    assert_eq!(floors, want, "{r}");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(project_plan::stored(&ed.model).unwrap().len(), 4);
    // nine storeys of the tallest building plus its roof
    assert!(count(&ed.model, "level") >= 10, "levels {}", count(&ed.model, "level"));
}

/// `origin [x, y]` from a refusal.
fn origin_in(text: &str) -> J {
    let at = text.find("origin [").unwrap_or_else(|| panic!("no origin in {text}")) + "origin ".len();
    let end = at + text[at..].find(']').unwrap() + 1;
    serde_json::from_str(&text[at..end]).unwrap()
}

#[test]
fn a_project_on_top_of_another_is_refused_with_a_free_origin() {
    let mut ed = editor();
    let office = json!({"spec": {"typology": "office", "floors": 2}});
    let school = json!({"spec": {"typology": "school", "floors": 2}});
    // both at the default origin: the second would stand inside the first
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", office), tool("generate_project", school.clone())]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    assert!(results[1].1, "{}", results[1].0);
    assert!(results[1].0.contains("overlaps P1") && results[1].0.contains("block.spacing"), "{}", results[1].0);
    assert_eq!(plan_of(&s).items.len(), 1);
    apply_and_check(&mut s, &mut ed);
    // the applied project is a neighbour too
    let (_, provider) = run(&mut ed, vec![tool("generate_project", school.clone())]);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("overlaps P1"), "{}", results[0].0);
    let mut moved = school;
    moved["spec"]["origin"] = origin_in(&results[0].0);
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", moved)]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(project_plan::stored(&ed.model).unwrap().len(), 2);
}
