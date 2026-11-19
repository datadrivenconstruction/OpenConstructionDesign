//! Grips exist only for the selection, on the view's level, and a press reads no model rows.

use super::*;
use crate::editor::{bodies::BodyKind, Editor, Sel};
use crate::wall_tool::Event;
use ddc_model::ColumnSpec;
use std::sync::atomic::Ordering;

fn col(ed: &mut Editor, level: i64, at: (f64, f64)) -> i64 {
    let have: Option<i64> = ed
        .model
        .sql()
        .query_row("SELECT id FROM member_type WHERE name='R 400x400'", [], |r| r.get(0))
        .ok();
    let mt = have.unwrap_or_else(|| ed.model.add_catalog_member_type("R 400x400").unwrap());
    ed.model
        .add_column(&ColumnSpec {
            level,
            top_level: None,
            member_type: mt,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap()
}

fn lv(ed: &Editor) -> i64 {
    ed.level
}

fn col_here(ed: &mut Editor, at: (f64, f64)) -> i64 {
    let l = lv(ed);
    col(ed, l, at)
}

fn builds(ed: &Editor) -> usize {
    ed.grip.cache.builds.load(Ordering::Relaxed)
}

#[test]
fn press_on_an_unselected_column_starts_no_drag() {
    let mut ed = Editor::new_in_memory().unwrap();
    let c = col_here(&mut ed, (0.0, 0.0));
    ed.reload().unwrap();
    assert!(!handle(&mut ed, Event::Press(v2(0.0, 0.0))));
    assert!(ed.grip.active.is_none());
    assert_eq!(ed.model.column(c).unwrap().at, (0.0, 0.0));
}

#[test]
fn selected_column_shows_grips_and_press_grabs_them() {
    let mut ed = Editor::new_in_memory().unwrap();
    let c = col_here(&mut ed, (0.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Body(BodyKind::Column, c)];
    let vis = visible(&ed);
    assert!(vis.iter().any(|g| g.kind == Kind::Column && g.role == Role::Move));
    assert!(handle(&mut ed, Event::Press(v2(0.0, 0.0))));
    assert!(ed.grip.active.is_some());
}

#[test]
fn column_of_another_level_is_never_grabbed() {
    let mut ed = Editor::new_in_memory().unwrap();
    let up = ed.model.add_level("L2", 3.0).unwrap();
    let c = col(&mut ed, up, (0.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Body(BodyKind::Column, c)];
    assert!(ed.level != up);
    assert!(visible(&ed).is_empty());
    assert!(!handle(&mut ed, Event::Press(v2(0.0, 0.0))));
    assert_eq!(ed.model.column(c).unwrap().at, (0.0, 0.0));
}

#[test]
fn press_cost_does_not_depend_on_model_size() {
    let rebuilds = |n: usize| {
        let mut ed = Editor::new_in_memory().unwrap();
        for i in 0..n {
            col_here(&mut ed, (10.0 + i as f64, 10.0));
        }
        ed.reload().unwrap();
        let sel = col_here(&mut ed, (0.0, 0.0));
        ed.reload().unwrap();
        ed.selection = vec![Sel::Body(BodyKind::Column, sel)];
        let _ = visible(&ed);
        let before = builds(&ed);
        for _ in 0..20 {
            handle(&mut ed, Event::Press(v2(-5.0, -5.0)));
            handle(&mut ed, Event::Click(v2(-5.0, -5.0)));
        }
        builds(&ed) - before
    };
    assert_eq!(rebuilds(3), 0);
    assert_eq!(rebuilds(300), 0);
}

#[test]
fn cache_follows_selection_and_model_changes() {
    let mut ed = Editor::new_in_memory().unwrap();
    let c = col_here(&mut ed, (0.0, 0.0));
    ed.reload().unwrap();
    ed.selection = vec![Sel::Body(BodyKind::Column, c)];
    let before = visible(&ed).iter().find(|g| g.role == Role::Move).unwrap().pos;
    let mut spec = ed.model.column(c).unwrap();
    spec.at = (2.0, 1.0);
    ed.model.update_column(c, &spec).unwrap();
    let after = visible(&ed).iter().find(|g| g.role == Role::Move).unwrap().pos;
    assert_ne!(before, after);
    ed.selection.clear();
    assert!(visible(&ed).is_empty());
}

fn wall_ed(a: (f64, f64), b: (f64, f64)) -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let layer = [ddc_model::LayerSpec { material: "c".into(), thickness: 0.2 }];
    let wt = ed.model.add_wall_type("W", &layer).unwrap();
    let id = ed
        .model
        .add_wall(&ddc_model::WallSpec {
            a,
            b,
            level: ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    ed.reload().unwrap();
    ed.selection = vec![Sel::Wall(id)];
    ed.set_mode(crate::wall_tool::Mode::Select);
    (ed, id)
}

fn len(ed: &Editor, id: i64) -> f64 {
    let (a, b) = wall_axis(&ed.model, id).unwrap();
    (b - a).len()
}

#[test]
fn drag_type_2000_enter_sets_length_2m() {
    use crate::wall_tool::Key;
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    assert!(handle(&mut ed, Event::Press(v2(5.0, 0.0))));
    for c in "2000".chars() {
        assert!(handle(&mut ed, Event::Char(c)));
    }
    assert!(handle(&mut ed, Event::Key(Key::Enter)));
    assert!(ed.grip.active.is_none());
    assert!((len(&ed, id) - 2.0).abs() < 1e-6);
}

#[test]
fn esc_cancels_drag_no_change() {
    use crate::wall_tool::Key;
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let before = ed.pushes;
    assert!(handle(&mut ed, Event::Press(v2(5.0, 0.0))));
    handle(&mut ed, Event::Char('2'));
    assert!(handle(&mut ed, Event::Key(Key::Escape)));
    assert!(ed.grip.active.is_none() && ed.grip.typing.is_none());
    assert_eq!(ed.pushes, before);
    assert!((len(&ed, id) - 5.0).abs() < 1e-9);
}

#[test]
fn tab_cycles_grips() {
    use crate::wall_tool::Key;
    let (mut ed, _) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let n = visible(&ed).iter().filter(|g| !is_click(&g.role)).count();
    assert!(n >= 2);
    assert!(handle(&mut ed, Event::Key(Key::Tab)));
    let first = ed.grip.hl.clone().unwrap();
    assert!(handle(&mut ed, Event::Key(Key::Tab)));
    assert_ne!(ed.grip.hl.clone().unwrap(), first);
    for _ in 0..n - 1 {
        handle(&mut ed, Event::Key(Key::Tab));
    }
    assert_eq!(ed.grip.hl.clone().unwrap(), first);
    assert!(handle(&mut ed, Event::Char(' ')));
    assert!(ed.grip.active.is_some());
}

#[test]
fn arrow_nudge_one_undo() {
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let step = ed.tool.grid;
    assert!(nudge(&mut ed, 1, 0, false));
    assert!(nudge(&mut ed, 1, 0, false));
    assert!(nudge(&mut ed, 0, 1, true));
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!((a.x - 2.0 * step).abs() < 1e-9 && (a.y - 10.0 * step).abs() < 1e-9);
    ed.undo();
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!(a.x.abs() < 1e-9 && a.y.abs() < 1e-9);
}

#[test]
fn arrow_nudge_ignored_while_typing() {
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let before = ed.pushes;
    ed.wall_draft_mut().input = "12".into();
    assert!(!nudge(&mut ed, 1, 0, false));
    ed.wall_draft_mut().input.clear();
    ed.grip.typing = Some((0, "1".into()));
    assert!(!nudge(&mut ed, 1, 0, false));
    assert_eq!(ed.pushes, before);
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!(a.x.abs() < 1e-9 && a.y.abs() < 1e-9);
}

#[test]
fn refused_fourth_nudge_keeps_three() {
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let step = ed.tool.grid;
    for _ in 0..3 {
        assert!(nudge(&mut ed, 1, 0, false));
    }
    let ts = vec![ddc_model::Target::Wall(id)];
    let fail = |_: &[ddc_model::Target], _: (f64, f64)| {
        crate::editor::OpFn(std::sync::Arc::new(|_: &mut Model| Err(bad())))
    };
    nudge_by(&mut ed, ts.clone(), (step, 0.0), &fail);
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!((a.x - 3.0 * step).abs() < 1e-9);
    assert!(ed.status.is_some());
    ed.undo();
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!(a.x.abs() < 1e-9);
}

#[test]
fn nudge_after_ctrl_z_keeps_earlier_edit() {
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let step = ed.tool.grid;
    assert!(nudge(&mut ed, 1, 0, false));
    assert!(nudge(&mut ed, 1, 0, false));
    ed.undo();
    // a fresh burst: the undo above must not be followed by undoing an earlier step
    assert!(nudge(&mut ed, 0, 1, false));
    let (a, _) = wall_axis(&ed.model, id).unwrap();
    assert!(a.x.abs() < 1e-9 && (a.y - step).abs() < 1e-9);
    ed.undo();
    assert!(len(&ed, id) > 4.9);
}

#[test]
fn esc_clears_grip_highlight() {
    use crate::wall_tool::Key;
    let (mut ed, _) = wall_ed((0.0, 0.0), (5.0, 0.0));
    assert!(handle(&mut ed, Event::Key(Key::Tab)));
    assert!(ed.grip.hl.is_some());
    assert!(handle(&mut ed, Event::Key(Key::Escape)));
    assert!(ed.grip.hl.is_none());
    assert!(!handle(&mut ed, Event::Key(Key::Enter)));
    assert!(ed.grip.active.is_none());
}

#[test]
fn selection_change_clears_highlight_enter_free() {
    use crate::wall_tool::Key;
    let (mut ed, _) = wall_ed((0.0, 0.0), (5.0, 0.0));
    assert!(handle(&mut ed, Event::Key(Key::Tab)));
    ed.selection.clear();
    assert!(!handle(&mut ed, Event::Key(Key::Enter)));
    assert!(ed.grip.hl.is_none() && ed.grip.active.is_none());
    let (mut ed, _) = wall_ed((0.0, 0.0), (5.0, 0.0));
    assert!(handle(&mut ed, Event::Key(Key::Tab)));
    ed.level += 1;
    assert!(!handle(&mut ed, Event::Char(' ')));
    assert!(ed.grip.hl.is_none() && ed.grip.active.is_none());
}

#[test]
fn typed_minus_first_sets_negative_offset() {
    let (mut ed, _) = wall_ed((0.0, 0.0), (5.0, 0.0));
    assert!(handle(&mut ed, Event::Press(v2(5.0, 0.0))));
    assert!(handle(&mut ed, Event::Char('-')));
    assert!(handle(&mut ed, Event::Char('2')));
    assert_eq!(ed.grip.typing.as_ref().map(|t| t.1.as_str()), Some("-2"));
}

#[test]
fn negative_length_refused() {
    use crate::wall_tool::Key;
    let (mut ed, id) = wall_ed((0.0, 0.0), (5.0, 0.0));
    let before = ed.pushes;
    assert!(handle(&mut ed, Event::Press(v2(5.0, 0.0))));
    handle(&mut ed, Event::Char('-'));
    handle(&mut ed, Event::Char('2'));
    assert!(handle(&mut ed, Event::Key(Key::Enter)));
    assert_eq!(ed.pushes, before);
    assert!((len(&ed, id) - 5.0).abs() < 1e-9);
    assert!(ed.status.as_deref().is_some_and(|s| !s.is_empty()));
}
