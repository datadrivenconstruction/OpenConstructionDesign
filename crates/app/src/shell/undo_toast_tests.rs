// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Toasts offer Undo or Redo only for the exact history entry they report (audit item 9).
use super::EditorApp;
use crate::editor::{Editor, Sel};
use ddc_ui::notify::STEP_CMDS;

fn app() -> EditorApp {
    EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
}

fn rect(a: &mut EditorApp, x: f64) {
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;
    a.ed.set_mode(Mode::Rect);
    a.ed.handle(Event::Press(v2(x, 0.0)));
    a.ed.handle(Event::Release(v2(x + 2.0, 2.0)));
    a.ed.set_mode(Mode::Select);
}

/// What a frame does with a finished command and a new status line.
fn frame(a: &mut EditorApp) {
    crate::quick::track_runs(a);
    a.status_to_toast();
}

/// (command, entry) of every toast that steps history.
fn steps(a: &EditorApp) -> Vec<(String, Option<u64>)> {
    a.sh.toasts
        .live
        .iter()
        .filter(|t| t.command.as_deref().is_some_and(|c| STEP_CMDS.contains(&c)))
        .map(|t| (t.command.clone().unwrap_or_default(), t.entry))
        .collect()
}

/// The invariant: every step toast carries an entry, and that entry is the one its button
/// would step right now (Undo: top of the undo stack; Redo: top of the redo stack).
fn assert_exact(a: &EditorApp, when: &str) {
    for (cmd, entry) in steps(a) {
        let e = entry.unwrap_or_else(|| panic!("{when}: {cmd} toast without an entry"));
        let top = if cmd == "edit.undo" {
            a.ed.top_entry()
        } else {
            a.ed.redo_entry_top()
        };
        assert_eq!(Some(e), top, "{when}: {cmd} toast steps entry {e}, top is {top:?}");
    }
    for t in &a.sh.toasts.live {
        let a = t.action.as_deref().unwrap_or_default();
        assert!(!STEP_CMDS.contains(&a), "{when}: a label never names a step command");
    }
}

#[test]
fn opening_a_menu_window_or_panel_offers_no_undo() {
    for cmd in ["app.menu", "ai.settings", "mgr.settings", "app.history", "view.3d"] {
        let mut a = app();
        // an edit made in the same frame, outside any command, as the screenshot run does
        rect(&mut a, 0.0);
        assert!(a.ed.top_entry().is_some());
        a.run(cmd);
        frame(&mut a);
        assert!(steps(&a).is_empty(), "{cmd}: {:?}", a.sh.toasts.live);
        assert_exact(&a, cmd);
    }
}

#[test]
fn a_command_toast_undoes_its_own_entry_and_nothing_else() {
    let mut a = app();
    rect(&mut a, 0.0);
    let ids: Vec<i64> = a.ed.walls.iter().map(|w| w.id).collect();
    a.ed.selection = vec![Sel::Wall(ids[0])];
    assert!(crate::shell::panels::edit_walls(&mut a.ed, &ids[..1], None, Some(2.4)));
    a.run("edit.copy_params");
    a.ed.selection = ids.iter().map(|i| Sel::Wall(*i)).collect();
    frame(&mut a);
    a.run("edit.paste_params");
    let pasted = a.ed.top_entry().expect("paste wrote an entry");
    frame(&mut a);
    assert_eq!(steps(&a), vec![("edit.undo".to_string(), Some(pasted))]);
    assert_exact(&a, "paste");

    // a later edit: the paste toast's button must not take it back
    rect(&mut a, 6.0);
    let walls = a.ed.walls.len();
    let later = a.ed.top_entry();
    a.step_toast_entry(true, pasted);
    assert_eq!(a.ed.walls.len(), walls, "the later rectangle stays");
    assert_eq!(a.ed.top_entry(), later);

    // once the later edit is undone the paste is on top again and only it goes
    a.run("edit.undo");
    assert_eq!(a.ed.top_entry(), Some(pasted));
    a.step_toast_entry(true, pasted);
    assert_eq!(a.ed.redo_entry_top(), Some(pasted));
    let tall = a.ed.walls.iter().filter(|w| (w.height - 2.4).abs() < 1e-9).count();
    assert_eq!(tall, 1, "only the copied-from wall keeps 2.4 m");
}

#[test]
fn every_undo_and_redo_toast_maps_to_the_exact_entry() {
    let mut a = app();
    rect(&mut a, 0.0);
    frame(&mut a);
    assert_exact(&a, "rect");
    let w = a.ed.walls[0].id;
    a.ed.selection = vec![Sel::Wall(w)];
    a.ed.delete_selection();
    frame(&mut a);
    assert_eq!(steps(&a).len(), 1, "delete offers Undo");
    assert_exact(&a, "delete");
    a.run("edit.undo");
    frame(&mut a);
    assert_eq!(steps(&a), vec![("edit.redo".to_string(), a.ed.redo_entry_top())]);
    assert_exact(&a, "undo");
    a.run("edit.redo");
    frame(&mut a);
    assert_eq!(steps(&a), vec![("edit.undo".to_string(), a.ed.top_entry())]);
    assert_exact(&a, "redo");
    // a stale offer is dropped: the status toast of an unrelated line carries no step
    a.ed.offer_undo();
    rect(&mut a, 9.0);
    a.ed.say(crate::status::Severity::Info, "x");
    frame(&mut a);
    assert_exact(&a, "stale offer");
    for cmd in ["app.menu", "ai.settings", "mgr.settings"] {
        a.run(cmd);
        frame(&mut a);
        assert_exact(&a, cmd);
    }
}

#[test]
fn entry_ids_follow_their_steps_through_undo_redo_and_take_back() {
    let mut ed = Editor::new_in_memory().unwrap();
    assert_eq!(ed.top_entry(), None);
    let mut a = app();
    rect(&mut a, 0.0);
    let first = a.ed.top_entry().unwrap();
    rect(&mut a, 4.0);
    let second = a.ed.top_entry().unwrap();
    assert!(second > first);
    a.ed.undo();
    assert_eq!((a.ed.top_entry(), a.ed.redo_entry_top()), (Some(first), Some(second)));
    a.ed.redo();
    assert_eq!((a.ed.top_entry(), a.ed.redo_entry_top()), (Some(second), None));
    a.ed.undo();
    rect(&mut a, 8.0);
    let third = a.ed.top_entry().unwrap();
    assert!(third > second, "ids never repeat");
    assert_eq!(a.ed.redo_entry_top(), None, "a new step clears redo and its ids");
    assert_eq!(a.ed.entry_since(a.ed.pushes), None);
    assert_eq!(a.ed.entry_since(a.ed.pushes - 1), Some(third));
    assert!(!ed.undo_entry(42), "nothing to step");
}
