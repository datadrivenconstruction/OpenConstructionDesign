//! Feedback channels (ONBOARDING_FEEDBACK §3.1) and the toast queue: ≤3 visible, rest «+N»,
//! identical toasts merge with a counter, 4 s (6 s with action), paused while hovered;
//! every toast is also kept in the notification center.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Toast,
    Inline,
    StatusBar,
    Issues,
    Banner,
    Modal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    /// Optional action label.
    pub action: Option<String>,
    /// Command the action runs; `None` means the host decides by the label.
    pub command: Option<String>,
    pub count: u32,
    /// Remaining life in seconds.
    pub left: f64,
    /// Longer explanation shown under the text and included when the toast is copied.
    pub details: Option<String>,
    /// History entry an Undo or Redo action steps, exactly that one (see `STEP_CMDS`).
    pub entry: Option<u64>,
}

/// Commands that step the history. A toast offers them only tied to the entry it reports,
/// so the button never takes back an edit made later or before.
pub const STEP_CMDS: [&str; 2] = ["edit.undo", "edit.redo"];

/// A toast reads as a sentence: the first letter of a plain leading word is a capital, whatever
/// case the status text was written in. Ids, paths and units («mm», «ann.text») are left alone.
pub fn sentence_case(text: String) -> String {
    let word_end = text.find(|c: char| !(c.is_alphabetic() || c == '-')).unwrap_or(text.len());
    let (word, rest) = text.split_at(word_end);
    let plain = word.chars().count() > 2
        && !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == '_' || c == '/' || c == '\\');
    let mut cs = word.chars();
    match cs.next() {
        Some(f) if plain && f.is_lowercase() => {
            let mut s: String = f.to_uppercase().collect();
            s.push_str(cs.as_str());
            s.push_str(rest);
            s
        }
        _ => text,
    }
}

impl Toast {
    /// What "Copy" puts on the clipboard: the text, then the details on their own lines.
    pub fn copy_text(&self) -> String {
        match &self.details {
            Some(d) if !d.is_empty() => format!("{}\n{}", self.text, d),
            _ => self.text.clone(),
        }
    }
}

pub const MAX_VISIBLE: usize = 3;
/// What the toast stack returns when "+N" is clicked: the host opens the notification center.
pub const HISTORY_CMD: &str = "notify.history";
pub const LIFE: f64 = 4.0;
pub const HISTORY_CAP: usize = 200;
pub const LIFE_ACTION: f64 = 6.0;
/// Errors stay until dismissed (the person must be able to read and copy them).
pub const LIFE_ERROR: f64 = f64::INFINITY;

#[derive(Default)]
pub struct Toasts {
    pub live: Vec<Toast>,
    /// Notification center: everything ever shown, newest last.
    pub history: Vec<(Level, String)>,
    /// Entry of the toast whose action was clicked last frame; the host takes it with the
    /// command `show` returned.
    pub fired_entry: Option<u64>,
    /// When each history entry was added (same order); shorter than `history` if a caller
    /// pushed there directly, then the missing ones have no time.
    pub times: Vec<web_time::SystemTime>,
    /// History entries the person has already looked at (the panel was open).
    pub seen: usize,
}

/// Unseen entries of the notification center, by severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Unseen {
    pub errors: usize,
    pub warnings: usize,
}

impl Toasts {
    pub fn push(&mut self, level: Level, text: impl Into<String>, action: Option<String>) {
        self.push_inner(level, text.into(), action, None, None);
    }
    /// Result toast whose button undoes (`edit.undo`) or redoes (`edit.redo`) history entry
    /// `entry` and nothing else; without an entry the toast has no button.
    pub fn push_step(
        &mut self,
        level: Level,
        text: impl Into<String>,
        action: impl Into<String>,
        command: &str,
        entry: Option<u64>,
    ) {
        let text = text.into();
        match entry.filter(|_| STEP_CMDS.contains(&command)) {
            Some(e) => {
                self.push_inner(level, text, Some(action.into()), Some(command.into()), Some(e))
            }
            None => self.push_inner(level, text, None, None, None),
        }
    }
    /// Toast whose action button runs command `command`. A history step command needs its
    /// entry (`push_step`): passed here, the toast keeps its text and drops the button.
    pub fn push_cmd(
        &mut self,
        level: Level,
        text: impl Into<String>,
        action: impl Into<String>,
        command: impl Into<String>,
    ) {
        let command = command.into();
        if STEP_CMDS.contains(&command.as_str()) {
            self.push_inner(level, text.into(), None, None, None);
            return;
        }
        self.push_inner(level, text.into(), Some(action.into()), Some(command), None);
    }
    /// Toast with a longer explanation under the text.
    pub fn push_details(
        &mut self,
        level: Level,
        text: impl Into<String>,
        details: impl Into<String>,
    ) {
        let text = text.into();
        self.push_inner(level, text.clone(), None, None, None);
        if let Some(t) = self
            .live
            .iter_mut()
            .rev()
            .find(|t| t.text == text && t.level == level && t.action.is_none())
        {
            t.details = Some(details.into());
        }
    }
    /// Removes a live toast (close button); it stays in the notification center.
    pub fn dismiss(&mut self, i: usize) {
        if i < self.live.len() {
            self.live.remove(i);
        }
    }
    /// Adds a line to the notification center only (no toast).
    pub fn record(&mut self, level: Level, text: impl Into<String>) {
        self.history.push((level, text.into()));
        self.times.push(web_time::SystemTime::now());
        self.trim_history();
    }
    fn trim_history(&mut self) {
        while self.history.len() > HISTORY_CAP {
            self.history.remove(0);
            if !self.times.is_empty() {
                self.times.remove(0);
            }
            self.seen = self.seen.saturating_sub(1);
        }
    }
    /// Warnings and errors not yet seen; info and success never raise the count.
    pub fn unseen(&self) -> Unseen {
        let mut u = Unseen::default();
        for (l, _) in self.history.iter().skip(self.seen) {
            match l {
                Level::Error => u.errors += 1,
                Level::Warning => u.warnings += 1,
                _ => {}
            }
        }
        u
    }
    pub fn mark_seen(&mut self) {
        self.seen = self.history.len();
    }
    /// Empties the notification center (live toasts stay).
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.times.clear();
        self.seen = 0;
    }
    /// Newest first, at most `n`: level, time (if known), text.
    pub fn recent(&self, n: usize) -> Vec<(Level, Option<web_time::SystemTime>, &str)> {
        // times line up with the tail only while every push went through this type
        let aligned = self.times.len() == self.history.len();
        self.history
            .iter()
            .enumerate()
            .rev()
            .take(n)
            .map(|(i, (l, t))| (*l, aligned.then(|| self.times[i]), t.as_str()))
            .collect()
    }
    fn push_inner(
        &mut self,
        level: Level,
        text: String,
        action: Option<String>,
        command: Option<String>,
        entry: Option<u64>,
    ) {
        let text = sentence_case(text);
        // a label alone is run by the host as a command name: Undo there would step any entry
        let action = match (&command, action) {
            (None, Some(a)) if STEP_CMDS.contains(&a.as_str()) => None,
            (_, a) => a,
        };
        self.history.push((level, text.clone()));
        self.times.push(web_time::SystemTime::now());
        self.trim_history();
        // an error must be readable to the end, an action needs time to reach
        let life = match (level, &action) {
            (Level::Error, _) => LIFE_ERROR,
            (_, Some(_)) => LIFE_ACTION,
            _ => LIFE,
        };
        if let Some(t) = self.live.iter_mut().find(|t| {
            t.text == text
                && t.level == level
                && t.action == action
                && t.command == command
                && t.entry == entry
        }) {
            t.count += 1;
            t.left = life;
            return;
        }
        // an older toast stepping history is outdated by a newer one: one Undo on screen
        if entry.is_some() {
            self.live.retain(|t| t.entry.is_none());
        }
        self.live.push(Toast {
            text,
            level,
            action,
            command,
            count: 1,
            left: life,
            details: None,
            entry,
        });
    }
    /// Age visible toasts by `dt` unless `hovered` (index into visible) is paused.
    pub fn tick(&mut self, dt: f64, hovered: Option<usize>) {
        for (k, i) in self.visible_idx().into_iter().enumerate() {
            if hovered != Some(k) {
                self.live[i].left -= dt;
            }
        }
        self.live.retain(|t| t.left > 0.0);
    }
    /// Seconds until the next visible toast that ages (`hovered` paused) runs out; `None`
    /// when nothing on screen expires (only errors, or none): no repaint is needed then.
    pub fn next_expiry(&self, hovered: Option<usize>) -> Option<f64> {
        self.visible_idx()
            .into_iter()
            .enumerate()
            .filter(|(k, i)| hovered != Some(*k) && self.live[*i].left.is_finite())
            .map(|(_, i)| self.live[i].left.max(0.0))
            .min_by(f64::total_cmp)
    }
    /// Indices into `live` of the cards on screen, in stack order. Errors never expire, so while
    /// any other toast waits they hold at most `MAX_VISIBLE - 1` slots (newest errors win);
    /// the remaining slots go to the oldest other toasts.
    pub fn visible_idx(&self) -> Vec<usize> {
        let (errs, others): (Vec<usize>, Vec<usize>) =
            (0..self.live.len()).partition(|&i| self.live[i].level == Level::Error);
        let cap = if others.is_empty() {
            MAX_VISIBLE
        } else {
            MAX_VISIBLE - 1
        };
        let ne = errs.len().min(cap);
        let mut v: Vec<usize> = errs[errs.len() - ne..].to_vec();
        v.extend(others.iter().copied().take(MAX_VISIBLE - ne));
        v.sort_unstable();
        v
    }
    pub fn visible(&self) -> Vec<&Toast> {
        self.visible_idx().into_iter().map(|i| &self.live[i]).collect()
    }
    pub fn overflow(&self) -> usize {
        self.live.len() - self.visible_idx().len()
    }
    /// "+N" was opened: the hidden errors are read in the notification center, so they leave
    /// the stack (they stay in the history); hidden toasts of other levels keep waiting.
    pub fn release_hidden_errors(&mut self) {
        let shown = self.visible_idx();
        let mut i = 0;
        self.live.retain(|t| {
            let keep = t.level != Level::Error || shown.contains(&i);
            i += 1;
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fx::fx;

    #[test]
    fn toasts_start_with_a_capital() {
        for (raw, want) in [
            (fx("nt_1"), fx("nt_1c")),
            (fx("nt_2"), fx("nt_2c")),
            ("section: line start", "Section: line start"),
            ("elevations added: 4", "Elevations added: 4"),
            ("Already fine", "Already fine"),
            ("ann.text: armed", "ann.text: armed"),
            ("mm wall", "mm wall"),
            ("t0", "t0"),
            ("", ""),
        ] {
            assert_eq!(sentence_case(raw.to_string()), want, "{raw}");
        }
        let mut q = Toasts::default();
        q.push(Level::Info, fx("nt_3"), None);
        assert_eq!(q.live[0].text, fx("nt_3c"));
        assert_eq!(q.recent(1)[0].2, fx("nt_3c"));
    }

    #[test]
    fn toast_rules() {
        let mut q = Toasts::default();
        for i in 0..5 {
            q.push(Level::Info, format!("t{i}"), None);
        }
        q.push(Level::Info, "t0", None);
        assert_eq!(q.visible().len(), 3);
        assert_eq!(q.overflow(), 2);
        assert_eq!(q.live[0].count, 2);
        assert_eq!(q.history.len(), 6);
        q.tick(3.0, Some(1));
        q.tick(1.5, Some(1));
        // t0 refreshed to 4 s then aged 4.5 s → gone; hovered t1 paused; t2 aged → gone.
        assert_eq!(q.live[0].text, "t1");
        q.push(Level::Success, "done", Some("open".into()));
        assert_eq!(q.live.last().unwrap().left, LIFE_ACTION);
    }

    #[test]
    fn unseen_counts_warn_error_only() {
        let mut q = Toasts::default();
        q.push(Level::Info, "a", None);
        q.push(Level::Success, "b", None);
        assert_eq!(q.unseen(), Unseen::default());
        q.push(Level::Warning, "w", None);
        q.push(Level::Error, "e1", None);
        q.record(Level::Error, "e2");
        assert_eq!(q.unseen(), Unseen { errors: 2, warnings: 1 });
        q.mark_seen();
        assert_eq!(q.unseen(), Unseen::default());
        q.push(Level::Warning, "w2", None);
        assert_eq!(q.unseen().warnings, 1);
        let r = q.recent(2);
        assert_eq!(r[0].2, "w2");
        assert!(r[0].1.is_some());
        q.clear_history();
        assert!(q.history.is_empty() && q.recent(5).is_empty());
        assert_eq!(q.seen, 0);
    }

    #[test]
    fn unseen_survives_the_cap() {
        let mut q = Toasts::default();
        for i in 0..HISTORY_CAP + 5 {
            q.record(Level::Info, format!("{i}"));
        }
        q.mark_seen();
        q.record(Level::Error, "late");
        assert_eq!(q.history.len(), HISTORY_CAP);
        assert_eq!(q.unseen().errors, 1);
    }

    #[test]
    fn error_stays_until_dismissed_but_others_expire() {
        let mut q = Toasts::default();
        q.push(Level::Error, "bad", None);
        q.push(Level::Info, "fyi", None);
        q.push(Level::Warning, "hmm", None);
        q.push(Level::Success, "ok", Some("Undo".into()));
        // fake clock: one minute in 0.25 s frames
        for _ in 0..240 {
            q.tick(0.25, None);
        }
        assert_eq!(q.live.len(), 1);
        assert_eq!(q.live[0].level, Level::Error);
        q.push(Level::Error, "bad", None);
        assert_eq!(q.live[0].count, 2, "same error merges");
        q.tick(1.0e6, None);
        assert_eq!(q.live.len(), 1);
        q.dismiss(0);
        assert!(q.live.is_empty());
        assert_eq!(q.history.len(), 5, "dismissed errors stay in the history");
    }

    #[test]
    fn persistent_errors_never_block_other_toasts() {
        let mut q = Toasts::default();
        for i in 0..5 {
            q.push(Level::Error, format!("e{i}"), None);
        }
        q.push(Level::Error, "e4", None);
        assert_eq!(q.live.len(), 5, "an identical error merges");
        assert_eq!(q.live[4].count, 2);
        let texts = |q: &Toasts| q.visible().iter().map(|t| t.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&q), ["e2", "e3", "e4"], "newest errors, at most three");
        assert_eq!(q.overflow(), 2);
        // an info toast still gets a slot and expires on its own
        q.push(Level::Info, "saved", None);
        assert_eq!(texts(&q), ["e3", "e4", "saved"]);
        assert_eq!(q.overflow(), 3);
        q.tick(LIFE + 0.1, None);
        assert_eq!(texts(&q), ["e2", "e3", "e4"]);
        // "+N" opened: hidden errors leave the stack, the history keeps all of them
        q.release_hidden_errors();
        assert_eq!(texts(&q), ["e2", "e3", "e4"]);
        assert_eq!(q.overflow(), 0);
        assert_eq!(q.history.len(), 7);
    }

    #[test]
    fn info_hides_after_its_life_on_a_fake_clock() {
        let mut q = Toasts::default();
        q.push(Level::Info, "fyi", None);
        q.tick(LIFE - 0.1, None);
        assert_eq!(q.live.len(), 1);
        q.tick(0.2, None);
        assert!(q.live.is_empty());
    }

    #[test]
    fn copy_text_is_text_then_details() {
        let mut q = Toasts::default();
        q.push_details(Level::Error, "Import failed", "row 3: bad unit");
        assert_eq!(q.live[0].copy_text(), "Import failed\nrow 3: bad unit");
        q.push(Level::Error, "Plain", None);
        assert_eq!(q.live[1].copy_text(), "Plain");
        q.push_details(Level::Error, "Plain", "");
        assert_eq!(q.live[1].copy_text(), "Plain");
        q.push_cmd(Level::Error, "Act", "Retry", "file.save");
        assert_eq!(q.live[2].copy_text(), "Act");
        assert_eq!(q.live[2].command.as_deref(), Some("file.save"));
    }

    #[test]
    fn toast_dismiss_and_command() {
        let mut q = Toasts::default();
        q.push(Level::Error, "bad", None);
        assert_eq!(q.live[0].left, LIFE_ERROR);
        q.push_cmd(Level::Success, "done", "Open", "view.warnings");
        assert_eq!(q.live[1].command.as_deref(), Some("view.warnings"));
        q.dismiss(0);
        q.dismiss(7);
        assert_eq!(q.live.len(), 1);
        assert_eq!(q.history.len(), 2);
    }

    #[test]
    fn a_history_step_button_needs_its_entry() {
        let mut q = Toasts::default();
        q.push_cmd(Level::Info, "Context menu", "Undo", "edit.undo");
        q.push(Level::Info, "Settings", Some("edit.undo".into()));
        q.push_step(Level::Info, "Saved", "Undo", "edit.undo", None);
        q.push_step(Level::Info, "Opened", "Undo", "view.open", Some(3));
        assert!(q.live.iter().all(|t| t.action.is_none() && t.command.is_none()));
        assert_eq!(q.history.len(), 4, "the texts are still reported");
        q.push_step(Level::Success, "Deleted", "Undo", "edit.undo", Some(5));
        q.push_step(Level::Success, "Moved", "Undo", "edit.undo", Some(6));
        let steps: Vec<_> = q.live.iter().filter(|t| t.action.is_some()).collect();
        assert_eq!(steps.len(), 1, "a newer step toast replaces the older one");
        assert_eq!(steps[0].entry, Some(6));
        assert_eq!(steps[0].command.as_deref(), Some("edit.undo"));
    }
}
