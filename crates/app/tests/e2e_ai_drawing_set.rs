// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `create_drawing_set`: the set is planned on the scratch copy, reaches the live model only on
//! Apply, counts every element in its schedules, and a second run updates the same sheets.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_app::sheet_view::schedule_table;
use ddc_io::sheets::dset_fixture::build_into;
use e2e_ai_common::*;
use serde_json::json;

fn set_ids(ed: &Editor) -> Vec<i64> {
    let mut v: Vec<i64> = ed
        .model
        .drawing_set_sheets()
        .unwrap()
        .into_iter()
        .map(|s| s.0)
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn drawing_set_is_previewed_applied_and_regenerated_in_place() {
    let mut ed = Editor::new_in_memory().unwrap();
    let made = build_into(&mut ed.model, 2).unwrap();
    ed.reload().unwrap();
    let call = || {
        tool(
            "create_drawing_set",
            json!({"title": "Drawings", "language": "en"}),
        )
    };
    let (mut s, p) = run(&mut ed, vec![call()]);
    let results = tool_results(&p);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert!(
        ed.model.sheets().unwrap().is_empty(),
        "nothing before Apply"
    );
    apply_and_check(&mut s, &mut ed);
    let first = set_ids(&ed);
    // cover, two plans, sections, elevations
    assert!(first.len() >= 5, "{} sheets", first.len());
    let tags = ed.model.tags(made.levels[0]).unwrap().len();
    assert!(tags >= 6 + 7 + 10, "{tags} tags");
    for (k, n) in [
        ("rooms", made.rooms),
        ("doors", made.doors),
        ("windows", made.windows),
    ] {
        assert_eq!(schedule_table(&ed, k).unwrap().rows.len(), n, "{k}");
    }
    let (mut s, _) = run(&mut ed, vec![call()]);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(set_ids(&ed), first, "the same sheets, updated");
    assert_eq!(
        ed.model.tags(made.levels[0]).unwrap().len(),
        tags,
        "no second tags"
    );
}
