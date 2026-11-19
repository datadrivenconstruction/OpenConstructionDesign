// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The preview of a plan: what the panel draws is built from the scratch copy and the base, and
//! the live model stays out of it.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::preview::{self, hide_of};
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

fn two_walls() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = add_level(&mut ed, "L1", 0.0);
    ed.level = level;
    let a = add_wall(&mut ed, level, (0.0, 0.0), (5.0, 0.0));
    let b = add_wall(&mut ed, level, (0.0, 4.0), (5.0, 4.0));
    (ed, a, b)
}

#[test]
fn a_deleted_wall_is_marked_hidden_and_the_live_model_untouched() {
    let (mut ed, a, b) = two_walls();
    let before = live_hash(&ed.model);
    let target = format!("wall id={a}");
    let (s, _p) = run(
        &mut ed,
        vec![tool(
            "delete_elements",
            json!({"title": "Remove", "target": {"filter": target}}),
        )],
    );
    let plan = plan_of(&s);
    let set = &plan.preview;
    let key = ("wall".to_string(), a);
    assert!(
        set.marks
            .iter()
            .any(|m| m.key == key && !m.outline.is_empty()),
        "no mark for the deleted wall"
    );
    assert!(set.boxes.contains_key(&key), "no box to zoom to");
    assert!(set.bounds.is_some());
    assert!(
        !set.boxes.contains_key(&("wall".to_string(), b)),
        "an untouched wall is in the preview"
    );
    let (sel, part) = hide_of(&key);
    assert!(
        sel.is_some_and(|x| set.hide.contains(&x))
            || part.is_some_and(|x| set.hide_parts.contains(&x)),
        "the deleted wall stays visible"
    );
    assert_eq!(
        live_hash(&ed.model),
        before,
        "a preview changed the live model"
    );
}

#[test]
fn publish_and_clear_change_the_revision() {
    let (mut ed, a, _b) = two_walls();
    let target = format!("wall id={a}");
    let (s, _p) = run(
        &mut ed,
        vec![tool(
            "delete_elements",
            json!({"title": "Remove", "target": {"filter": target}}),
        )],
    );
    let plan = plan_of(&s);
    preview::publish(Some(plan.preview.clone()), false);
    let light = preview::rev();
    assert_ne!(light, 0);
    preview::publish(Some(plan.preview.clone()), true);
    assert_ne!(
        preview::rev(),
        light,
        "the theme is part of the revision: meshes carry its colours"
    );
    preview::publish(None, false);
    assert_eq!(preview::rev(), 0);
}

#[test]
fn showing_the_draft_and_before_never_changes_the_model() {
    let (mut ed, a, _b) = two_walls();
    let target = format!("wall id={a}");
    let (s, _p) = run(
        &mut ed,
        vec![tool(
            "delete_elements",
            json!({"title": "Remove", "target": {"filter": target}}),
        )],
    );
    let plan = plan_of(&s);
    let hash = live_hash(&ed.model);
    let revision = ed.revision;
    preview::publish(Some(plan.preview.clone()), false);
    for before in [false, true, false] {
        preview::set_before(before);
        let mut vis = ddc_app::isolate::Vis::default();
        preview::hide_into(&mut vis);
        let mut meshes = Vec::new();
        preview::apply(&ed, &mut meshes);
        assert_eq!(vis.hidden.is_empty(), before);
        assert!(!before || meshes.is_empty());
    }
    preview::publish(None, false);
    assert_eq!(live_hash(&ed.model), hash, "preview and Before are draft only");
    assert_eq!(ed.revision, revision);
}
