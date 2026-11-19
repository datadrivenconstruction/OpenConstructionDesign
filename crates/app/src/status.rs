//! Severity of a status line, set by the code that raises it instead of guessed from its words.
use crate::editor::Editor;
use ddc_ui::notify::Level;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// Plain news.
    Info,
    /// Done in part, or done with a caveat.
    Warning,
    /// The action was declined before it changed anything.
    Refusal,
    /// The action failed.
    Error,
}

impl Severity {
    fn rank(self) -> u8 {
        match self {
            Severity::Info => 0,
            Severity::Warning => 1,
            Severity::Refusal => 2,
            Severity::Error => 3,
        }
    }

    pub fn level(self) -> Level {
        match self {
            Severity::Info => Level::Info,
            Severity::Warning | Severity::Refusal => Level::Warning,
            Severity::Error => Level::Error,
        }
    }
}

impl Editor {
    /// Sets the status line with an explicit severity. Saying the same text again still toasts.
    pub fn say(&mut self, sev: Severity, msg: impl Into<String>) {
        let mut m = msg.into();
        let mut sev = sev;
        // a room warning of this step that is not yet in the line joins the new text
        if let Some(rooms) = self.room_watch.take_pending() {
            (m, sev) = fold_rooms(m, sev, &rooms);
            self.room_watch.remember_written(rooms);
        }
        self.status_sev = Some((sev, m.clone()));
        self.status = Some(m);
        self.status_seq += 1;
    }

    /// End of a step: a room warning that a plain `status` assignment replaced or cleared is put
    /// back, never lost and never below Warning. Also puts back, once, the note the previous
    /// settle wrote when the caller replaced the status after it.
    pub fn settle_status(&mut self) {
        // a note put back from `written` is not remembered again: the chrome settles every frame
        let (rooms, fresh) = match self.room_watch.take_pending() {
            Some(r) => (r, true),
            None => match self.room_watch.take_written() {
                Some(r) => (r, false),
                None => return,
            },
        };
        let sev = self.status_severity().unwrap_or(Severity::Info);
        let (m, sev) = match self.status.take() {
            Some(cur) => fold_rooms(cur, sev, &rooms),
            None => (crate::tr("room_status.open", &[("room", &rooms)]), Severity::Warning),
        };
        self.status_sev = Some((sev, m.clone()));
        self.status = Some(m);
        if fresh {
            self.room_watch.remember_written(rooms);
        }
    }

    /// Marks the status line as the result of the op just committed, so its toast offers Undo
    /// for that history entry. Call right after the op; nothing on top, nothing offered.
    pub fn offer_undo(&mut self) {
        self.undo_offer = self.top_entry();
    }

    /// Severity set for the current status text; a later plain assignment drops it.
    pub fn status_severity(&self) -> Option<Severity> {
        match (&self.status_sev, &self.status) {
            (Some((s, t)), Some(cur)) if t == cur => Some(*s),
            _ => None,
        }
    }
}

/// `text` with the room note appended unless it already says so; severity at least Warning.
fn fold_rooms(text: String, sev: Severity, rooms: &str) -> (String, Severity) {
    let open = crate::tr("room_status.open", &[("room", rooms)]);
    let note = crate::tr("room_status.fold", &[("room", rooms)]);
    let sev = if sev.rank() > Severity::Warning.rank() {
        sev
    } else {
        Severity::Warning
    };
    if text.contains(&open) || text.contains(&note) {
        return (text, sev);
    }
    if text.is_empty() {
        return (open, sev);
    }
    (format!("{text} \u{b7} {note}"), sev)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deleted_room_editor() -> Editor {
        let (mut e, walls, _) = crate::room_watch::tests::square_room();
        e.selection = vec![crate::editor::Sel::Wall(walls[0])];
        e.delete_selection();
        e
    }

    fn fold_note() -> String {
        crate::tr("room_status.fold", &[("room", "101")])
    }

    #[test]
    fn room_warning_shown_when_status_empty() {
        let mut e = deleted_room_editor();
        e.status = None;
        e.settle_status();
        let s = e.status.clone().unwrap();
        assert!(s.contains("101"), "{s}");
        assert_eq!(e.status_severity(), Some(Severity::Warning));
    }

    #[test]
    fn replaced_info_status_gets_the_note() {
        let mut e = deleted_room_editor();
        e.status = Some("moved".into());
        e.settle_status();
        let s = e.status.clone().unwrap();
        assert!(s.starts_with("moved") && s.contains(&fold_note()), "{s}");
        assert_eq!(e.status_severity(), Some(Severity::Warning));
    }

    #[test]
    fn error_status_keeps_error_and_room_note() {
        let mut e = deleted_room_editor();
        e.status = Some("boom".into());
        e.status_sev = Some((Severity::Error, "boom".into()));
        e.settle_status();
        let s = e.status.clone().unwrap();
        assert!(s.starts_with("boom") && s.contains(&fold_note()), "{s}");
        assert_eq!(e.status_severity(), Some(Severity::Error));
    }

    #[test]
    fn folded_plain_error_not_downgraded() {
        let mut e = deleted_room_editor();
        e.say(Severity::Error, "boom");
        e.room_watch.set_pending_for_test("101");
        e.settle_status();
        assert_eq!(e.status_severity(), Some(Severity::Error));
        assert!(e.status.clone().unwrap().contains(&fold_note()));
    }

    #[test]
    fn next_say_takes_the_pending_warning() {
        let mut e = deleted_room_editor();
        // the delete's own line took the note; a later reload of the step leaves it pending
        e.room_watch.set_pending_for_test("101");
        // no settle: a headless run
        e.say(Severity::Info, "next");
        let s = e.status.clone().unwrap();
        assert!(s.contains(&fold_note()), "{s}");
        assert_eq!(e.status_severity(), Some(Severity::Warning));
        assert!(e.room_watch.take_pending().is_none());
    }

    #[test]
    fn note_written_by_settle_returns_once_after_a_replaced_status() {
        let mut e = deleted_room_editor();
        e.room_watch.set_pending_for_test("101");
        e.status = Some("moved".into());
        e.settle_status();
        e.status = Some("later".into());
        e.settle_status();
        let s = e.status.clone().unwrap();
        assert!(s.starts_with("later") && s.contains(&fold_note()), "{s}");
        e.status = Some("again".into());
        e.settle_status();
        assert_eq!(e.status.as_deref(), Some("again"));
    }

    #[test]
    fn a_new_edit_forgets_the_old_note() {
        let (mut e, walls, _) = crate::room_watch::tests::square_room();
        e.selection = vec![crate::editor::Sel::Wall(walls[0])];
        e.delete_selection();
        e.status = Some("moved".into());
        e.settle_status();
        e.undo();
        e.status = Some("undone".into());
        e.settle_status();
        assert_eq!(e.status.as_deref(), Some("undone"));
    }

    #[test]
    fn severity_maps_to_toast_level() {
        assert_eq!(Severity::Info.level(), Level::Info);
        assert_eq!(Severity::Warning.level(), Level::Warning);
        assert_eq!(Severity::Refusal.level(), Level::Warning);
        assert_eq!(Severity::Error.level(), Level::Error);
    }

    #[test]
    fn say_sets_text_severity_and_bumps_the_counter() {
        let mut ed = Editor::new_in_memory().unwrap();
        let n = ed.status_seq;
        ed.say(Severity::Refusal, "x");
        assert_eq!(ed.status.as_deref(), Some("x"));
        assert_eq!(ed.status_severity(), Some(Severity::Refusal));
        ed.say(Severity::Refusal, "x");
        assert_eq!(ed.status_seq, n + 2);
    }

    #[test]
    fn a_plain_assignment_drops_the_stale_severity() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.say(Severity::Error, "boom");
        ed.status = Some("other".into());
        assert_eq!(ed.status_severity(), None);
    }
}
