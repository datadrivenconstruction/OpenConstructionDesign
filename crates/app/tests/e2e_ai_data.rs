// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "How much concrete on floor 2": an answer from data, and nothing done to the model.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::ScriptedProvider;
use ddc_ai::Mode;
use ddc_app::ai::UiEvent;
use ddc_app::editor::Editor;
use ddc_model::{SlabSpec, WallSpec};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

/// Floor 2: slab 10 x 8 x 0.25 = 20 m³, wall 5 x 3 x 0.2 = 3 m³; floor 1 only adds noise.
fn model() -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let first = ed.level;
    let floor2 = add_level(&mut ed, "Floor 2", 3.0);
    let st = ed.model.add_slab_type("S250", &concrete(0.25)).unwrap();
    let wt = ed.model.add_wall_type("W200", &concrete(0.2)).unwrap();
    let ring = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 8.0), (0.0, 8.0)];
    ed.model
        .add_slab(&SlabSpec {
            level: floor2,
            slab_type: st,
            offset: 0.0,
            outer: ring.clone(),
            holes: vec![],
        })
        .unwrap();
    ed.model
        .add_slab(&SlabSpec {
            level: first,
            slab_type: st,
            offset: 0.0,
            outer: ring,
            holes: vec![],
        })
        .unwrap();
    let wall = |level, y| WallSpec {
        a: (0.0, y),
        b: (5.0, y),
        level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    };
    ed.model
        .add_walls(&[wall(floor2, 20.0), wall(first, 20.0)])
        .unwrap();
    ed.reload().unwrap();
    (ed, floor2)
}

fn ask(ed: &mut Editor) -> (ddc_app::ai::UiSession, std::sync::Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(script(vec![tool(
        "quantities",
        json!({"target": {"filter": "level=2"}, "group_by": "material"}),
    )]));
    let mut s = start(ed, cfg(provider.clone()));
    turn(&mut s, ed, "how much concrete on floor 2", Mode::ReadOnly);
    (s, provider)
}

#[test]
fn concrete_of_floor_2_matches_the_hand_calculation() {
    let (mut ed, floor2) = model();
    let (_s, provider) = ask(&mut ed);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 1);
    assert!(!results[0].1, "{}", results[0].0);
    let reply = parsed(&results[0].0);
    let groups = reply["groups"].as_array().expect("groups");
    let got = groups
        .iter()
        .find(|g| g["key"] == json!("Concrete"))
        .expect("concrete")["volume"]
        .as_f64()
        .unwrap();
    assert!((got - 23.0).abs() < 0.01, "{got}");

    let totals = ed.model.material_totals(Some(floor2)).unwrap();
    let want: f64 = totals
        .iter()
        .filter(|m| m.material == "Concrete")
        .map(|m| m.volume)
        .sum();
    assert!((got - want).abs() < 0.01, "{got} against {want}");
}

#[test]
fn a_question_changes_nothing_and_makes_no_plan() {
    let (mut ed, _) = model();
    let before = live_hash(&ed.model);
    let ops = count(&ed.model, "op");
    let (mut s, _provider) = ask(&mut ed);
    assert_eq!(live_hash(&ed.model), before);
    assert_eq!(count(&ed.model, "op"), ops);
    assert!(s.plan().is_none());
    assert!(!s.can_apply());
    assert_eq!(s.apply(&mut ed), Err(ddc_app::ai::AiError::EmptyPlan));
}

#[test]
fn a_schedule_export_is_only_an_offer_to_the_person() {
    let (mut ed, _) = model();
    let before = live_hash(&ed.model);
    let provider = ScriptedProvider::new(script(vec![tool(
        "export_schedule",
        json!({"cat": "wall", "filter": "level=2"}),
    )]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "export", Mode::ReadOnly);
    let events = s.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::ExportOffer { cat, .. } if cat == "wall")),
        "{events:?}"
    );
    assert_eq!(
        parsed(&tool_results(&provider)[0].0)["status"],
        json!("offered")
    );
    assert_eq!(live_hash(&ed.model), before);
}
