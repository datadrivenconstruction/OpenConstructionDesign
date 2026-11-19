//! Deleting a level that still holds elements: say how many, then offer to select them or move
//! them to another level. Nothing is deleted until the user picks a target; move + delete is one undo.
use std::cell::RefCell;
use std::sync::Arc;

use ddc_model::Model;

use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::ui_kit as kit;

/// A level delete waiting for the user's choice.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: i64,
    pub name: String,
    pub by: Vec<(&'static str, i64)>,
    /// Level the move goes to; None until picked.
    pub target: Option<i64>,
    /// The room-height warning was shown; the next Move click goes through.
    pub confirm: bool,
}

impl Ask {
    /// The window's first line: how many elements the level holds, by kind.
    pub fn headline(&self, ru: bool) -> String {
        summary(ru, &self.by)
    }
}

thread_local! {
    static ASK: RefCell<Option<Ask>> = const { RefCell::new(None) };
}

pub fn pending() -> Option<Ask> {
    ASK.with(|a| a.borrow().clone())
}

pub fn clear() {
    ASK.with(|a| *a.borrow_mut() = None);
}

pub fn open(ed: &Editor, id: i64, by: Vec<(&'static str, i64)>) {
    let name = ed
        .levels()
        .into_iter()
        .find(|l| l.0 == id)
        .map(|l| l.1)
        .unwrap_or_default();
    ASK.with(|a| {
        *a.borrow_mut() = Some(Ask {
            id,
            name,
            by,
            target: None,
            confirm: false,
        })
    });
}

fn kind_name(ru: bool, table: &str) -> String {
    let key = format!("level_ask.kind.{table}");
    let s = crate::tr_lang(ru, &key, &[]);
    if s == key {
        table.to_string()
    } else {
        s
    }
}

/// "12 elements are on this level: Wall 8, Slab 4".
pub fn summary(ru: bool, by: &[(&'static str, i64)]) -> String {
    let mut counts: Vec<(&str, usize)> = vec![];
    for (t, _) in by {
        match counts.iter_mut().find(|c| c.0 == *t) {
            Some(c) => c.1 += 1,
            None => counts.push((t, 1)),
        }
    }
    let kinds = counts
        .iter()
        .map(|(t, n)| format!("{} {n}", kind_name(ru, t)))
        .collect::<Vec<_>>()
        .join(", ");
    crate::tr_lang(
        ru,
        "level_ask.level.has",
        &[("n", &by.len().to_string()), ("kinds", &kinds)],
    )
}

/// Rooms on the deleted level that take the target level's height: they move by the
/// elevation difference (rooms have no offset). None when nothing changes.
pub fn room_warning(ru: bool, ed: &Editor, ask: &Ask, to: i64) -> Option<String> {
    let ls = ed.levels();
    let z = |l: i64| ls.iter().find(|x| x.0 == l).map(|x| x.2);
    let dz = z(ask.id)? - z(to)?;
    if dz.abs() < 1e-9 {
        return None;
    }
    let names: Vec<String> = ask
        .by
        .iter()
        .filter(|(t, _)| *t == "room")
        .filter_map(|(_, id)| {
            ed.model
                .sql()
                .query_row("SELECT number, name FROM room WHERE id=?1", [id], |r| {
                    Ok(format!("{} {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .ok()
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(crate::tr_lang(
        ru,
        "lv.level.rooms",
        &[
            ("n", &names.len().to_string()),
            ("dz", &format!("{:.2}", -dz)),
            ("names", &names.join(", ")),
        ],
    ))
}

/// Pinned elements on level `id`, named for the refusal ("Wall #3, Slab #7"); None when free.
pub fn pinned_names(ru: bool, ed: &Editor, id: i64) -> Option<String> {
    let pinned = ed.model.level_pinned(id).unwrap_or_default();
    if pinned.is_empty() {
        return None;
    }
    let names: Vec<String> = pinned
        .iter()
        .map(|(t, i)| format!("{} #{i}", kind_name(ru, t)))
        .collect();
    Some(crate::tr_lang(ru, "lv.level.pinned", &[("names", &names.join(", "))]))
}

fn sel_of(table: &str, id: i64) -> Option<Sel> {
    Some(match table {
        "wall" => Sel::Wall(id),
        "slab" => Sel::Slab(id),
        "room" => Sel::Room(id),
        "mass" => Sel::Mass(id),
        "comp" => Sel::Comp(id),
        "mep_run" => Sel::Mep(id),
        "curtain_wall" => Sel::Curtain(id),
        t => Sel::Body(BodyKind::from_table(t)?, id),
    })
}

/// Makes the level active and selects what it holds; the level stays.
pub fn select_contents(ru: bool, ed: &mut Editor, ask: &Ask) {
    ed.set_level(ask.id);
    let sel: Vec<Sel> = ask
        .by
        .iter()
        .filter_map(|(t, id)| sel_of(t, *id))
        .collect();
    let n = sel.len();
    ed.selection = sel;
    ed.say(
        Severity::Info,
        crate::tr_lang(ru, "level_ask.level.selected", &[("n", &n.to_string())]),
    );
}

/// Moves the contents to `to` and deletes the level as one undo step.
/// Refused, with nothing written, while any of the contents is pinned.
pub fn move_and_delete(ru: bool, ed: &mut Editor, id: i64, to: i64) -> bool {
    if let Some(why) = pinned_names(ru, ed, id) {
        ed.say(Severity::Refusal, why);
        return false;
    }
    let was_active = ed.level == id;
    let op = OpFn(Arc::new(move |m: &mut Model| m.delete_level_into(id, to)));
    // the reload after the op must not look for the deleted level
    if was_active {
        ed.level = to;
        ed.selection.clear();
    }
    let head = ed.model.change_head();
    match ed.run_op(op) {
        Ok(()) => {
            ed.say(Severity::Info, crate::tr_lang(ru, "level_ask.level.moved", &[]));
            true
        }
        // the move is done and on the undo stack; only the view reload failed
        Err(e) if ed.model.change_head() != head => {
            ed.say(
                Severity::Warning,
                crate::tr_lang(ru, "elem.reload_failed", &[("e", &e.to_string())]),
            );
            true
        }
        Err(e) => {
            if was_active {
                ed.level = id;
            }
            ed.say(
                Severity::Error,
                format!("{}: {e}", crate::tr_lang(ru, "level_ask.level.move_failed", &[])),
            );
            false
        }
    }
}

pub fn ui(app: &mut EditorApp, ctx: &egui::Context) {
    let Some(mut ask) = pending() else {
        return;
    };
    let others: Vec<(i64, String, f64)> = app
        .ed
        .levels()
        .into_iter()
        .filter(|l| l.0 != ask.id)
        .collect();
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let pinned = pinned_names(ru, &app.ed, ask.id);
    let (mut open, mut select, mut mv, mut cancel) = (true, false, false, false);
    kit::Dialog::new(
        "level-delete",
        crate::tr_lang(ru, "level_ask.level.title", &[("name", &ask.name)]),
        kit::DialogSize::Confirm,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        ui.label(ask.headline(ru));
        ui.add_space(ddc_ui::theme::space::SM);
        ui.label(crate::tr_lang(ru, "level_ask.level.move_to", &[]));
        for (id, name, _) in &others {
            if ui
                .selectable_label(ask.target == Some(*id), name.as_str())
                .clicked()
            {
                ask.target = Some(*id);
                ask.confirm = false;
            }
        }
        if let Some(p) = &pinned {
            ui.add_space(ddc_ui::theme::space::SM);
            ui.label(p.as_str());
        } else if ask.confirm {
            if let Some(w) = ask.target.and_then(|t| room_warning(ru, &app.ed, &ask, t)) {
                ui.add_space(ddc_ui::theme::space::SM);
                ui.label(w);
            }
        }
        kit::dialog_footer(ui, |ui| {
            let why = pinned.clone().or_else(|| {
                ask.target
                    .is_none()
                    .then(|| crate::tr_lang(ru, "level_ask.level.pick_target", &[]))
            });
            if kit::primary_action(ui, crate::tr_lang(ru, "level_ask.level.move_btn", &[]), why.as_deref()) {
                mv = true;
            }
            if ui.button(crate::tr_lang(ru, "level_ask.level.select_btn", &[])).clicked() {
                select = true;
            }
            if kit::cancel_button(ui, crate::tr_lang(ru, "dlg.cancel", &[])).clicked() {
                cancel = true;
            }
        });
    });
    if mv {
        let target = ask.target;
        match target {
            Some(to) if !ask.confirm && room_warning(ru, &app.ed, &ask, to).is_some() => {
                ask.confirm = true;
                ASK.with(|a| *a.borrow_mut() = Some(ask));
            }
            Some(to) => {
                move_and_delete(ru, &mut app.ed, ask.id, to);
                clear();
            }
            None => clear(),
        }
    } else if select {
        select_contents(ru, &mut app.ed, &ask);
        clear();
    } else if cancel || !open {
        clear();
    } else {
        ASK.with(|a| *a.borrow_mut() = Some(ask));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::WallSpec;

    fn with_walls() -> (Editor, i64, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        let a = e.level;
        let b = e.add_level("Upper", 3.0).unwrap();
        e.set_level(a);
        let wall_type = e.current_type().unwrap().id;
        for y in [0.0, 2.0] {
            let spec = WallSpec {
                a: (0.0, y),
                b: (4.0, y),
                level: a,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            };
            e.model.add_wall(&spec).unwrap();
        }
        e.reload().unwrap();
        (e, a, b)
    }

    fn walls(e: &Editor) -> i64 {
        e.model
            .sql()
            .query_row("SELECT COUNT(*) FROM wall", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn level_delete_names_count_and_actions() {
        let (mut e, a, _) = with_walls();
        let n = e.model.level_contents(a).unwrap().len();
        assert!(n >= 2);
        e.status = None;
        assert!(!e.delete_level(a), "occupied level is not deleted");
        let ask = pending().expect("choice is offered");
        assert_eq!(ask.by.len(), n);
        assert!(e.levels().iter().any(|l| l.0 == a));
        // c2f4aa13e: the open window carries the count, the status line stays silent
        let shown = pending().unwrap().headline(false);
        assert!(shown.contains(&n.to_string()), "{shown}");
        assert_eq!(e.status, None, "no toast answers before the window does");
        select_contents(false, &mut e, &ask);
        assert_eq!(e.selection.len(), n);
        clear();
    }

    /// Every wall row with its absolute base elevation instead of the level link.
    fn wall_rows(e: &Editor) -> Vec<String> {
        let mut st = e
            .model
            .sql()
            .prepare(
                "SELECT w.id, w.wtype, w.ax, w.ay, w.bx, w.by, \
                 round(l.elevation + w.base_offset, 9), round(w.height, 9) \
                 FROM wall w JOIN level l ON l.id=w.level ORDER BY w.id",
            )
            .unwrap();
        st.query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    fn level_rows(e: &Editor) -> Vec<(i64, String, f64)> {
        e.model.level_list().unwrap()
    }

    #[test]
    fn delete_into_confirm_names_rooms() {
        let (mut e, a, b) = with_walls();
        e.model
            .add_room(&ddc_model::RoomSpec {
                level: a,
                name: "Hall".into(),
                number: "101".into(),
                point: (2.0, 1.0),
                height: 2.7,
            })
            .unwrap();
        assert!(!e.delete_level(a));
        let ask = pending().expect("choice is offered");
        let w = room_warning(false, &e, &ask, b).expect("rooms change height");
        assert!(w.contains("101 Hall"), "{w}");
        assert!(w.contains("3.00"), "{w}");
        let z = e.levels().iter().find(|l| l.0 == a).unwrap().2;
        let same = e.add_level("Same", z).unwrap();
        assert!(room_warning(false, &e, &ask, same).is_none(), "equal elevation: no change");
        clear();
    }

    #[test]
    fn level_delete_with_elements_moves_them_nothing_lost() {
        let (mut e, a, b) = with_walls();
        let rows = wall_rows(&e);
        assert_eq!(rows.len(), 2);
        assert!(!e.delete_level(a), "occupied level is never deleted directly");
        assert_eq!(wall_rows(&e), rows, "the refusal writes nothing");
        let ask = pending().expect("choice is offered");
        assert!(move_and_delete(false, &mut e, ask.id, b));
        clear();
        assert!(!e.levels().iter().any(|l| l.0 == a));
        assert_eq!(wall_rows(&e), rows, "same walls, same place, same heights");
        let on_b: i64 = e
            .model
            .sql()
            .query_row("SELECT count(*) FROM wall WHERE level=?1", [b], |r| r.get(0))
            .unwrap();
        assert_eq!(on_b, 2, "all walls now stand on the target level");
        e.undo();
        assert!(e.levels().iter().any(|l| l.0 == a), "one undo restores the level");
        assert_eq!(wall_rows(&e), rows);
    }

    #[test]
    fn level_delete_cancel_leaves_model_unchanged() {
        let (mut e, a, _) = with_walls();
        let (walls0, levels0) = (wall_rows(&e), level_rows(&e));
        let head = e.model.change_head();
        assert!(!e.delete_level(a));
        assert!(pending().is_some(), "choice is offered");
        // Cancel / Esc on the card only drops the pending choice
        clear();
        assert!(pending().is_none());
        assert_eq!(wall_rows(&e), walls0);
        assert_eq!(level_rows(&e), levels0);
        assert_eq!(e.model.change_head(), head, "no model step was written");
    }

    #[test]
    fn level_delete_pinned_element_refused_with_reason() {
        let (mut e, a, b) = with_walls();
        let w: i64 = e
            .model
            .sql()
            .query_row("SELECT min(id) FROM wall WHERE level=?1", [a], |r| r.get(0))
            .unwrap();
        e.model.pin(&[("wall", w)]).unwrap();
        let (walls0, levels0) = (wall_rows(&e), level_rows(&e));
        let head = e.model.change_head();
        assert!(!e.delete_level(a));
        let ask = pending().expect("choice is offered");
        assert!(!move_and_delete(false, &mut e, ask.id, b), "pinned contents block the move");
        clear();
        let st = e.status.clone().unwrap_or_default();
        assert!(st.contains(&format!("#{w}")), "reason names the pinned wall: {st}");
        assert_eq!(e.status_severity(), Some(Severity::Refusal));
        assert_eq!(wall_rows(&e), walls0);
        assert_eq!(level_rows(&e), levels0);
        assert_eq!(e.model.change_head(), head, "nothing written, nothing to undo");
    }

    #[test]
    fn level_move_then_delete_keeps_elements() {
        let (mut e, a, b) = with_walls();
        let n = e.model.level_contents(a).unwrap().len();
        let w = walls(&e);
        assert!(move_and_delete(false, &mut e, a, b));
        assert!(!e.levels().iter().any(|l| l.0 == a));
        assert_eq!(walls(&e), w);
        assert_eq!(e.model.level_contents(b).unwrap().len(), n);
        e.undo();
        assert!(
            e.levels().iter().any(|l| l.0 == a),
            "one undo restores the level"
        );
        assert_eq!(e.model.level_contents(a).unwrap().len(), n);
        clear();
    }

    #[test]
    fn en_status_has_no_cyrillic() {
        let files = [
            include_str!("roof_edge_tools.rs"),
            include_str!("solid_tools.rs"),
            include_str!("rebar_ui.rs"),
            include_str!("sheet_view.rs"),
            include_str!("editor/sec.rs"),
            include_str!("editor/levels.rs"),
            include_str!("shell/drawing_win.rs"),
            include_str!("level_ask.rs"),
        ];
        for src in files {
            for l in src
                .lines()
                .filter(|l| l.contains(".say(") || l.contains("status = Some("))
            {
                assert!(
                    !l.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)),
                    "{l}"
                );
            }
        }
    }
}
