//! Project browser helpers: filter match, inline rename, the empty search state.

/// What an inline rename targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Level(i64),
    View(i64),
}

#[derive(Clone, Debug)]
pub struct Rename {
    pub target: Target,
    pub old: String,
    pub buf: String,
    focus: bool,
}

#[derive(Default)]
pub struct TreeUx {
    pub rename: Option<Rename>,
    /// Row the `browser.row_*` commands act on: the views or levels row last clicked, focused
    /// or opened for its menu; `None` after a click on a row they do not cover.
    pub row: Option<Target>,
}

impl TreeUx {
    pub fn start(&mut self, target: Target, old: &str) {
        self.rename = Some(Rename {
            target,
            old: old.to_string(),
            buf: old.to_string(),
            focus: true,
        });
    }

    pub fn active(&self, target: Target) -> bool {
        self.rename.as_ref().is_some_and(|r| r.target == target)
    }
}

/// Filter of the tree: empty query keeps everything.
pub fn matches(query: &str, label: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || label.to_lowercase().contains(&q)
}

/// New name to commit: trimmed, not empty, really different.
pub fn rename_result(old: &str, new: &str) -> Option<String> {
    let n = new.trim();
    (!n.is_empty() && n != old.trim()).then(|| n.to_string())
}

/// Result of one frame of the inline editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Keep,
    Done(Option<String>),
}

/// Text field in place of a row: Enter or a click elsewhere commits, Esc cancels.
pub fn rename_row(ui: &mut egui::Ui, st: &mut Rename) -> Edit {
    let id = egui::Id::new("ddc.tree.rename");
    let resp = ui.add(
        egui::TextEdit::singleline(&mut st.buf)
            .id(id)
            .desired_width(ui.available_width()),
    );
    if std::mem::take(&mut st.focus) {
        resp.request_focus();
        if let Some(mut s) = egui::TextEdit::load_state(ui.ctx(), id) {
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(st.buf.chars().count()),
            );
            s.cursor.set_char_range(Some(all));
            s.store(ui.ctx(), id);
        }
        return Edit::Keep;
    }
    if resp.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Edit::Done(None);
        }
        return Edit::Done(rename_result(&st.old, &st.buf));
    }
    Edit::Keep
}

/// Nothing found: says so and offers the way back.
pub fn empty_search(ui: &mut egui::Ui) -> bool {
    let k = crate::empty_kind::Kind::BrowserSearch;
    crate::ui_kit::empty_state_lang(ui, crate::ui_lang_now(), k.what(), k.has_action())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_ignores_case_and_edges() {
        assert!(matches("", "anything"));
        assert!(matches("  ", "anything"));
        let fx = crate::app2_fx::fx;
        let hay = fx("tux_plan_floor1");
        assert!(matches(&fx("tux_floor_upper"), &hay));
        assert!(!matches(&fx("tux_roof"), &hay));
    }

    #[test]
    fn rename_needs_a_real_new_name() {
        assert_eq!(rename_result("A", "  B "), Some("B".to_string()));
        assert_eq!(rename_result("A", "A "), None);
        assert_eq!(rename_result("A", "   "), None);
    }

    #[test]
    fn only_the_target_row_is_edited() {
        let mut t = TreeUx::default();
        assert!(!t.active(Target::Level(1)));
        t.start(Target::Level(1), "L1");
        assert!(t.active(Target::Level(1)));
        assert!(!t.active(Target::View(1)));
        assert_eq!(t.rename.as_ref().map(|r| r.buf.as_str()), Some("L1"));
    }
}
