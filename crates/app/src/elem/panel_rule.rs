// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! One rule for a curtain panel: it never deletes or moves its wall. Delete, Move, Rotate, Mirror
//! and Array refuse on a panel and offer "Select wall" instead.

use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use std::cell::Cell;

/// Command the toast action runs.
pub const CMD: &str = "elem.panel_select_wall";

thread_local! {
    static WALL: Cell<Option<i64>> = const { Cell::new(None) };
    static OFFER: Cell<bool> = const { Cell::new(false) };
}

/// A refusal that left panels in place: the next status toast carries a "Select wall" action.
pub fn offer(left: &[Sel]) {
    if let Some(w) = left.iter().find_map(|s| match s {
        Sel::Panel(w, _) => Some(*w),
        _ => None,
    }) {
        WALL.with(|c| c.set(Some(w)));
        OFFER.with(|c| c.set(true));
    }
}

/// True once per offer; the shell turns the status into a toast with the action.
pub fn take_offer() -> bool {
    OFFER.with(|c| c.replace(false))
}

/// Names of the kinds in `items`, without repeats.
pub fn kinds(ed: &Editor, items: &[Sel]) -> String {
    let mut names: Vec<String> = vec![];
    for &s in items {
        let n = if matches!(s, Sel::Panel(..)) {
            crate::tr("elem.panel", &[])
        } else {
            crate::tr(&format!("selcat.{}", crate::selection::category(ed, s)), &[])
        };
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.join(", ")
}

/// Selects the curtain wall of the panel the last refusal was about.
fn select_wall(a: &mut EditorApp) {
    if let Some(w) = WALL.with(|c| c.get()) {
        a.ed.selection = vec![Sel::Curtain(w)];
    }
}

pub fn register(a: &mut EditorApp) {
    a.register_command(CMD, select_wall);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::delete::tests::dump;
    use crate::modify_tools::{ModTool, Op, Out};
    use ddc_geom::v2;

    const TABLES: [&str; 3] = ["curtain_wall", "curtain_panel", "wall"];

    fn facade() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.add_curtains(&[(v2(0.0, 0.0), v2(6.0, 0.0), 0.0)]);
        let cw = ed.curtains[0].id;
        (ed, cw)
    }

    fn all(ed: &Editor) -> Vec<Vec<String>> {
        TABLES.iter().map(|t| dump(ed, t)).collect()
    }

    #[test]
    fn delete_panel_never_touches_wall() {
        let (mut ed, cw) = facade();
        let before = all(&ed);
        let (head, steps) = (ed.model.change_head(), ed.undo_len());
        ed.selection = vec![Sel::Panel(cw, 2)];
        let _ = take_offer();
        ed.delete_selection();
        assert_eq!(all(&ed), before);
        assert_eq!((ed.model.change_head(), ed.undo_len()), (head, steps));
        assert_eq!(ed.selection, vec![Sel::Panel(cw, 2)]);
        assert!(ed.status.is_some());
    }

    #[test]
    fn delete_refusal_offers_select_wall() {
        let (mut ed, cw) = facade();
        ed.selection = vec![Sel::Panel(cw, 2)];
        let _ = take_offer();
        ed.delete_selection();
        assert!(take_offer());
        assert!(!take_offer(), "offered once");
        let mut app = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        select_wall(&mut app);
        assert_eq!(app.ed.selection, vec![Sel::Curtain(cw)]);
    }

    #[test]
    fn transforms_on_a_panel_refuse_and_the_wall_stays() {
        let (mut ed, cw) = facade();
        let before = all(&ed);
        let (head, steps) = (ed.model.change_head(), ed.undo_len());
        ed.selection = vec![Sel::Panel(cw, 2)];
        for op in [Op::Move, Op::Rotate, Op::Mirror, Op::Array] {
            let _ = take_offer();
            let mut t = ModTool::new(op, &ed.selection);
            assert!(matches!(t.click(&mut ed, v2(0.0, 0.0)), Out::Refused(_)));
            assert!(take_offer(), "{op:?} offers Select wall");
            assert_eq!(all(&ed), before);
            assert_eq!((ed.model.change_head(), ed.undo_len()), (head, steps));
        }
    }

    #[test]
    fn mixed_with_a_wall_deletes_the_wall_and_keeps_the_panel() {
        let (mut ed, cw) = facade();
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 10.0), (4.0, 10.0))]);
        let wall = ed.walls[0].id;
        let curtain = dump(&ed, "curtain_wall");
        ed.selection = vec![Sel::Panel(cw, 2), Sel::Wall(wall)];
        let steps = ed.undo_len();
        ed.delete_selection();
        assert_eq!(dump(&ed, "curtain_wall"), curtain);
        assert_eq!(ed.undo_len(), steps + 1, "one undo step");
        assert_eq!(ed.selection, vec![Sel::Panel(cw, 2)]);
        ed.undo();
        assert_eq!(ed.undo_len(), steps);
    }

    #[test]
    fn keys_are_defined_once_per_language() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n");
        for lang in ["en", "ru"] {
            let mut seen = std::collections::HashMap::new();
            for f in std::fs::read_dir(dir.join(lang)).unwrap() {
                let path = f.unwrap().path();
                let txt = std::fs::read_to_string(&path).unwrap();
                let map: std::collections::BTreeMap<String, serde_json::Value> =
                    serde_json::from_str(&txt).unwrap();
                for k in map.keys() {
                    if let Some(prev) = seen.insert(k.clone(), path.clone()) {
                        panic!("{lang}: {k} is in {prev:?} and {path:?}");
                    }
                }
            }
        }
    }
}
