// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing set command: the dialog's options, Apply as one undo step, words in both languages.
use super::dset::{build_drawing_set, marks_to_assign, plan_drawing_set, DsetDialog};
use super::*;
use crate::schedule::{self, Cat};
use ddc_io::sheets::dset_fixture::{build_into, build_unmarked_into};

fn editor(floors: usize) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    build_into(&mut ed.model, floors).unwrap();
    ed.reload().unwrap();
    ed
}

#[test]
fn default_dialog_asks_for_everything_on_gost() {
    let o = DsetDialog::default().options();
    assert!(o.plans && o.sections && o.elevations && o.schedules);
    assert_eq!(
        (o.scale, o.paper.as_deref(), o.standard.as_str()),
        (None, None, "gost")
    );
}

#[test]
fn command_builds_the_set_in_one_undo_step() {
    let mut ed = editor(2);
    let before = ed.model.sheets().unwrap().len();
    let ids = build_drawing_set(&mut ed, DsetDialog::default().options(), false).unwrap();
    assert!(ids.len() >= 5, "{ids:?}");
    let first = ed.model.sheet(ids[0]).unwrap();
    assert_eq!(first.number, "A-1");
    ed.undo();
    assert_eq!(ed.model.sheets().unwrap().len(), before, "one undo step");
}

#[test]
fn sheet_words_follow_the_language_asked() {
    let ed = editor(1);
    let o = DsetDialog::default().options();
    let en = plan_drawing_set(&ed, &o, false).unwrap();
    let ru = plan_drawing_set(&ed, &o, true).unwrap();
    assert_eq!(en.plan.sheets.len(), ru.plan.sheets.len());
    assert_eq!(en.plan.sheets[0].def.number, "A-1");
    assert_eq!(ru.plan.sheets[0].def.number, "\u{0410}\u{0420}-1");
    let cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
    assert!(en.plan.sheets.iter().all(|s| !cyr(&s.def.name)));
    assert!(ru.plan.sheets.iter().all(|s| cyr(&s.def.name)));
}

/// Doors and windows the set marks by type read the same in the door and window schedules
/// as on their tags, and one undo takes the marks back with the sheets.
#[test]
fn type_marks_reach_the_schedules_and_undo_with_the_set() {
    let mut ed = Editor::new_in_memory().unwrap();
    build_unmarked_into(&mut ed.model, 2).unwrap();
    ed.reload().unwrap();
    let before = marks_to_assign(&ed).unwrap();
    assert!(before > 0);
    build_drawing_set(&mut ed, DsetDialog::default().options(), false).unwrap();
    assert_eq!(marks_to_assign(&ed).unwrap(), 0);
    for (cat, want) in [(Cat::Doors, "D1"), (Cat::Windows, "W1")] {
        // the set's sheets carry the building-wide schedules; the editor stays on its seed level
        let tb = schedule::build_building(&ed, cat);
        let k = tb.cols.iter().position(|c| c.key == "mark").unwrap();
        assert!(!tb.rows.is_empty());
        for r in &tb.rows {
            let shown = schedule::fmt(&r.vals[k], tb.cols[k].kind, ed.tool.units);
            let tag = ed.model.mark(ddc_model::TagHost::Opening(r.id)).unwrap();
            assert_eq!(
                (shown.as_str(), tag.as_str()),
                (want, want),
                "{cat:?} {}",
                r.id
            );
        }
    }
    ed.undo();
    assert_eq!(
        marks_to_assign(&ed).unwrap(),
        before,
        "marks undone with the set"
    );
}
