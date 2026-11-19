//! Reusing sent prompts: Up/Down in the composer walks the earlier prompts of this project,
//! and a person's own message offers Edit (text back into the composer) and Copy.
//! Nothing here sends: Edit only fills the input.

use super::kit::Button;
use super::{Cx, Entry};
use egui::{self, Id, Key, Rect, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Older,
    Newer,
}

/// Where the caret sits in the input; Up is the caret's own on any line but the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    pub first_line: bool,
    pub last_line: bool,
}

/// Position of the caret (a char index) among the input's lines.
pub fn caret_of(input: &str, char_idx: usize) -> Caret {
    let mut before_nl = false;
    let mut after_nl = false;
    for (n, c) in input.chars().enumerate() {
        if c == '\n' {
            if n < char_idx {
                before_nl = true;
            } else {
                after_nl = true;
            }
        }
    }
    Caret {
        first_line: !before_nl,
        last_line: !after_nl,
    }
}

/// Earlier prompts, oldest first; blanks dropped and consecutive repeats collapsed.
pub fn collapse<'a>(texts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in texts {
        if t.trim().is_empty() || out.last().is_some_and(|l| l.trim() == t.trim()) {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

pub fn prompts(log: &[Entry]) -> Vec<String> {
    collapse(log.iter().filter_map(|e| match e {
        Entry::User(t) => Some(t.as_str()),
        _ => None,
    }))
}

/// What Edit puts into the composer: the message exactly as it was sent.
pub fn edit_text(msg: &str) -> String {
    msg.to_string()
}

/// What Copy puts on the clipboard: the message exactly as it was sent.
pub fn copy_text(msg: &str) -> String {
    msg.to_string()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recall {
    /// Index in the oldest-first prompt list of the prompt now in the input.
    at: Option<usize>,
    draft: String,
    shown: String,
}

impl Recall {
    pub fn reset(&mut self) {
        *self = Recall::default();
    }

    pub fn active(&self) -> bool {
        self.at.is_some()
    }

    /// The new input text when the key moves through the history, else None (the key belongs
    /// to the text field). Recall starts only from an empty input; an edited recalled prompt
    /// is the person's own text again.
    pub fn step(&mut self, prompts: &[String], input: &str, dir: Dir, caret: Caret) -> Option<String> {
        if self.at.is_some() && input != self.shown {
            self.reset();
        }
        if self.at.is_none() && !input.is_empty() {
            return None;
        }
        let n = prompts.len();
        match dir {
            Dir::Older => {
                if !caret.first_line || n == 0 {
                    return None;
                }
                let next = match self.at {
                    None => {
                        self.draft = input.to_string();
                        n - 1
                    }
                    Some(i) if i > 0 => (i - 1).min(n - 1),
                    Some(_) => return None,
                };
                self.at = Some(next);
                self.shown = prompts[next].clone();
                Some(self.shown.clone())
            }
            Dir::Newer => {
                let i = self.at?;
                if !caret.last_line {
                    return None;
                }
                if i + 1 < n {
                    self.at = Some(i + 1);
                    self.shown = prompts[i + 1].clone();
                    Some(self.shown.clone())
                } else {
                    Some(self.leave())
                }
            }
        }
    }

    /// Esc: back to the draft while the recalled prompt is untouched.
    pub fn restore(&mut self, input: &str) -> Option<String> {
        if self.at.is_some() && input == self.shown {
            Some(self.leave())
        } else {
            None
        }
    }

    fn leave(&mut self) -> String {
        let d = std::mem::take(&mut self.draft);
        self.at = None;
        self.shown.clear();
        d
    }
}

fn edit_req() -> Id {
    Id::new("ddc.ai.recall.edit")
}

fn set_input(ui: &Ui, id: Id, input: &mut String, text: String) {
    let n = text.chars().count();
    *input = text;
    let ctx = ui.ctx();
    let mut s = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    s.cursor
        .set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(n))));
    s.store(ctx, id);
}

/// Composer hook, before the text field: applies a pending Edit and the history keys.
pub fn keys(
    ui: &mut Ui,
    id: Id,
    input: &mut String,
    rc: &mut Recall,
    log: &[Entry],
    enabled: bool,
) {
    let ctx = ui.ctx().clone();
    if let Some(text) = ctx.data_mut(|d| d.remove_temp::<String>(edit_req())) {
        rc.reset();
        set_input(ui, id, input, edit_text(&text));
        ui.memory_mut(|m| m.request_focus(id));
        return;
    }
    if !enabled {
        return;
    }
    let (up, down, esc) = ui.input(|i| {
        let plain = i.modifiers.is_none();
        (
            plain && i.key_pressed(Key::ArrowUp),
            plain && i.key_pressed(Key::ArrowDown),
            plain && i.key_pressed(Key::Escape),
        )
    });
    if !(up || down || esc) {
        return;
    }
    let idx = egui::text_edit::TextEditState::load(&ctx, id)
        .and_then(|s| s.cursor.char_range())
        .map_or_else(|| input.chars().count(), |r| r.primary.index);
    let caret = caret_of(input, idx);
    let next = if esc {
        rc.restore(input)
    } else {
        let dir = if up { Dir::Older } else { Dir::Newer };
        rc.step(&prompts(log), input, dir, caret)
    };
    if let Some(text) = next {
        let key = if esc {
            Key::Escape
        } else if up {
            Key::ArrowUp
        } else {
            Key::ArrowDown
        };
        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        set_input(ui, id, input, text);
        // the recalled text is what the next step compares against
    }
}

const COPIED_SECS: f64 = 1.5;

/// Quiet Edit and Copy under a person's own bubble; the row keeps its place so nothing jumps.
pub fn actions(ui: &mut Ui, cx: &Cx<'_>, text: &str, at: usize, busy: bool, bubble: Rect) {
    let (th, cat) = (cx.th, cx.cat);
    let ctx = ui.ctx().clone();
    let id = Id::new(("ddc.ai.recall.row", at));
    let now = ctx.input(|i| i.time);
    let copied = ctx
        .data(|d| d.get_temp::<f64>(id.with("copied")))
        .is_some_and(|t| now - t < COPIED_SECS);
    let prev = ctx.data(|d| d.get_temp::<Rect>(id));
    let hot = ui.rect_contains_pointer(bubble) || prev.is_some_and(|r| ui.rect_contains_pointer(r));
    let row = ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !(hot || copied) {
                ui.set_invisible();
            }
            let label = cat.t(if copied { "ai.chat.copied" } else { "ai.chat.copy" });
            if Button::quiet(&label).small().show(ui, th).clicked() {
                ctx.copy_text(copy_text(text));
                ctx.data_mut(|d| d.insert_temp(id.with("copied"), now));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS + 0.1));
            }
            if Button::quiet(&cat.t("ai.chat.edit"))
                .small()
                .enabled(!busy)
                .tooltip(&cat.t("ai.chat.edit_tip"), "")
                .show(ui, th)
                .clicked()
            {
                ctx.data_mut(|d| d.insert_temp(edit_req(), text.to_string()));
                ctx.request_repaint();
            }
        });
    });
    ctx.data_mut(|d| d.insert_temp(id, row.response.rect));
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANY: Caret = Caret {
        first_line: true,
        last_line: true,
    };

    fn list(v: &[&str]) -> Vec<String> {
        collapse(v.iter().copied())
    }

    #[test]
    fn empty_history_changes_nothing() {
        let mut r = Recall::default();
        assert_eq!(r.step(&[], "", Dir::Older, ANY), None);
        assert_eq!(r.step(&[], "", Dir::Newer, ANY), None);
        assert!(!r.active());
    }

    #[test]
    fn up_walks_back_and_stops_at_the_oldest() {
        let p = list(&["a", "b", "c"]);
        let mut r = Recall::default();
        assert_eq!(r.step(&p, "", Dir::Older, ANY).as_deref(), Some("c"));
        assert_eq!(r.step(&p, "c", Dir::Older, ANY).as_deref(), Some("b"));
        assert_eq!(r.step(&p, "b", Dir::Older, ANY).as_deref(), Some("a"));
        assert_eq!(r.step(&p, "a", Dir::Older, ANY), None);
        assert!(r.active(), "still on the oldest");
    }

    #[test]
    fn down_walks_forward_and_returns_to_the_draft() {
        let p = list(&["a", "b"]);
        let mut r = Recall::default();
        assert_eq!(r.step(&p, "", Dir::Newer, ANY), None, "nothing to go forward to");
        r.step(&p, "", Dir::Older, ANY);
        r.step(&p, "b", Dir::Older, ANY);
        assert_eq!(r.step(&p, "a", Dir::Newer, ANY).as_deref(), Some("b"));
        assert_eq!(r.step(&p, "b", Dir::Newer, ANY).as_deref(), Some(""));
        assert!(!r.active());
        assert_eq!(r.step(&p, "", Dir::Newer, ANY), None, "stops at the newest end");
    }

    #[test]
    fn draft_is_kept_and_restored() {
        let mut r = Recall::default();
        r.at = Some(0);
        r.shown = "a".into();
        r.draft = "half typed".into();
        let p = list(&["a"]);
        assert_eq!(r.step(&p, "a", Dir::Newer, ANY).as_deref(), Some("half typed"));
        let mut r2 = Recall::default();
        r2.step(&p, "", Dir::Older, ANY);
        assert_eq!(r2.restore("a").as_deref(), Some(""));
        assert!(!r2.active());
    }

    #[test]
    fn edited_recalled_prompt_stops_recall() {
        let p = list(&["a", "b"]);
        let mut r = Recall::default();
        r.step(&p, "", Dir::Older, ANY);
        assert_eq!(r.step(&p, "b and more", Dir::Older, ANY), None);
        assert!(!r.active());
        assert_eq!(r.step(&p, "b and more", Dir::Newer, ANY), None);
        assert_eq!(r.restore("b and more"), None);
    }

    #[test]
    fn typed_text_keeps_up_for_the_caret() {
        let p = list(&["a"]);
        let mut r = Recall::default();
        assert_eq!(r.step(&p, "typing", Dir::Older, ANY), None);
        assert!(!r.active());
    }

    #[test]
    fn multi_line_caret_rule() {
        let text = "one\ntwo\nthree";
        assert_eq!(caret_of(text, 0), Caret { first_line: true, last_line: false });
        assert_eq!(caret_of(text, 3), Caret { first_line: true, last_line: false });
        assert_eq!(caret_of(text, 5), Caret { first_line: false, last_line: false });
        assert_eq!(caret_of(text, 13), Caret { first_line: false, last_line: true });
        assert_eq!(caret_of("", 0), ANY);
        // a recalled two-line prompt: Up inside it moves the caret, not the history
        let p = list(&["old", "l1\nl2"]);
        let mut r = Recall::default();
        assert_eq!(r.step(&p, "", Dir::Older, ANY).as_deref(), Some("l1\nl2"));
        let end = caret_of("l1\nl2", 5);
        assert_eq!(r.step(&p, "l1\nl2", Dir::Older, end), None);
        assert_eq!(r.step(&p, "l1\nl2", Dir::Newer, caret_of("l1\nl2", 1)), None);
        let top = caret_of("l1\nl2", 1);
        assert_eq!(r.step(&p, "l1\nl2", Dir::Older, top).as_deref(), Some("old"));
    }

    #[test]
    fn consecutive_duplicates_collapse() {
        let p = list(&["a", "a", " ", "b", "a", "a"]);
        assert_eq!(p, vec!["a", "b", "a"]);
        let mut r = Recall::default();
        assert_eq!(r.step(&p, "", Dir::Older, ANY).as_deref(), Some("a"));
        assert_eq!(r.step(&p, "a", Dir::Older, ANY).as_deref(), Some("b"));
    }

    #[test]
    fn only_own_messages_are_prompts() {
        let log = vec![
            Entry::User("first".into()),
            Entry::Assistant("answer".into()),
            Entry::User("second".into()),
        ];
        assert_eq!(prompts(&log), vec!["first", "second"]);
    }

    #[test]
    fn edit_and_copy_give_the_exact_text() {
        let m = "  make the walls 200 mm\non level 2  \n";
        assert_eq!(edit_text(m), m);
        assert_eq!(copy_text(m), m);
    }
}
