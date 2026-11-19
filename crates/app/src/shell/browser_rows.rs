//! Row commands of the project browser (`browser.row_open`, `browser.row_rename`,
//! `browser.row_delete`). They act on the current row of the views and levels sections: the one
//! last clicked, focused or opened for its menu (`TreeUx::row`). The row keys Enter and Del of a
//! focused row run the same code; F2 and the context menus start the same inline rename.
//! Deleting goes through the editor, so it is one undo step.

use super::tree_ux::Target;
use super::EditorApp;

impl EditorApp {
    /// The current browser row, if it still exists in the model.
    pub(crate) fn browser_row(&self) -> Option<Target> {
        match self.chrome.tree.row? {
            Target::Level(id) => self
                .ed
                .levels()
                .iter()
                .any(|l| l.0 == id)
                .then_some(Target::Level(id)),
            Target::View(id) => self.ed.view_spec(id).map(|_| Target::View(id)),
        }
    }

    /// Opens the row: the plan of a level, a section or elevation in its tab.
    pub(crate) fn row_open(&mut self, t: Target) {
        match t {
            Target::Level(id) => {
                crate::sheet_view::deactivate();
                crate::drafting_view::deactivate();
                self.ed.set_level(id);
                self.view3d = false;
                self.section = None;
            }
            Target::View(id) => self.open_view(id),
        }
        self.chrome.tree.row = Some(t);
    }

    /// Starts the inline rename of the row; the browser is shown and its filter cleared so the
    /// row is on screen.
    pub(crate) fn row_rename(&mut self, t: Target) {
        let old = match t {
            Target::Level(id) => self.ed.levels().into_iter().find(|l| l.0 == id).map(|l| l.1),
            Target::View(id) => self.ed.view_spec(id).map(crate::editor::sec::display_name),
        };
        let Some(old) = old else { return };
        if !self.prefs.browser_open {
            self.prefs.browser_open = true;
            self.prefs.save();
        }
        super::panels::browser::clear_search();
        self.chrome.tree.start(t, &old);
        self.chrome.tree.row = Some(t);
    }

    /// Deletes the level or the view of the row (one undo step); a view's tab closes first.
    pub(crate) fn row_delete(&mut self, t: Target) {
        match t {
            Target::Level(id) => {
                self.ed.delete_level(id);
            }
            Target::View(id) => {
                self.close_view(id);
                self.ed.delete_view(id);
            }
        }
        if self.browser_row().is_none() {
            self.chrome.tree.row = None;
        }
    }
}

fn with_row(a: &mut EditorApp, f: fn(&mut EditorApp, Target)) {
    match a.browser_row() {
        Some(t) => f(a, t),
        None => a.ed.status = Some(crate::tr("view_cmds.row.none", &[])),
    }
}

fn h_open(a: &mut EditorApp) {
    with_row(a, EditorApp::row_open);
}

fn h_rename(a: &mut EditorApp) {
    with_row(a, EditorApp::row_rename);
}

fn h_delete(a: &mut EditorApp) {
    with_row(a, EditorApp::row_delete);
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command(crate::ctx_menu::OPEN, h_open);
    app.register_command(crate::ctx_menu::RENAME, h_rename);
    app.register_command(crate::ctx_menu::DELETE, h_delete);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn app() -> EditorApp {
        crate::shot_press::set_screenshot_mode(true);
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        a.prefs.path = None;
        a
    }

    fn add_section(a: &mut EditorApp) -> i64 {
        let s = crate::editor::sec::spec_from(
            "S",
            ddc_geom::v2(0.0, 0.0),
            ddc_geom::v2(6.0, 0.0),
            Some(ddc_geom::v2(3.0, 2.0)),
        )
        .unwrap();
        a.ed.add_view(&s).unwrap()
    }

    #[test]
    fn without_a_row_the_commands_say_so_and_change_nothing() {
        let mut a = app();
        let rev = a.ed.revision;
        for id in [crate::ctx_menu::OPEN, crate::ctx_menu::RENAME, crate::ctx_menu::DELETE] {
            a.ed.status = None;
            assert!(a.run(id), "{id}");
            assert_eq!(a.ed.status.as_deref(), Some(crate::tr("view_cmds.row.none", &[]).as_str()));
        }
        assert_eq!(a.ed.revision, rev);
    }

    #[test]
    fn open_rename_delete_act_on_the_view_row_and_delete_undoes() {
        let mut a = app();
        let id = add_section(&mut a);
        a.chrome.tree.row = Some(Target::View(id));
        assert!(a.run(crate::ctx_menu::OPEN));
        assert_eq!(a.section, Some(id));
        assert!(a.tabs.contains(&id));
        assert!(a.run(crate::ctx_menu::RENAME));
        assert!(a.chrome.tree.active(Target::View(id)));
        a.chrome.tree.rename = None;
        assert!(a.run(crate::ctx_menu::DELETE));
        assert!(a.ed.view_spec(id).is_none());
        assert!(!a.tabs.contains(&id) && a.section.is_none());
        assert_eq!(a.chrome.tree.row, None);
        a.run("edit.undo");
        assert!(a.ed.view_spec(id).is_some(), "delete is one undo step");
    }

    #[test]
    fn level_row_opens_its_plan_and_deletes_with_undo() {
        let mut a = app();
        let l2 = a.ed.add_level("L2", 3.0).unwrap();
        let first = a.ed.levels()[0].0;
        a.ed.set_level(first);
        a.view3d = true;
        a.chrome.tree.row = Some(Target::Level(l2));
        assert!(a.run(crate::ctx_menu::OPEN));
        assert_eq!(a.ed.level, l2);
        assert!(!a.view3d);
        assert!(a.run(crate::ctx_menu::RENAME));
        assert!(a.chrome.tree.active(Target::Level(l2)));
        a.chrome.tree.rename = None;
        assert!(a.run(crate::ctx_menu::DELETE));
        assert!(!a.ed.levels().iter().any(|l| l.0 == l2));
        a.run("edit.undo");
        assert!(a.ed.levels().iter().any(|l| l.0 == l2));
    }

    #[test]
    fn a_stale_row_is_not_acted_on() {
        let mut a = app();
        let id = add_section(&mut a);
        a.chrome.tree.row = Some(Target::View(id));
        a.ed.delete_view(id);
        assert_eq!(a.browser_row(), None);
        assert!(a.run(crate::ctx_menu::OPEN));
        assert_eq!(a.section, None);
    }
}
