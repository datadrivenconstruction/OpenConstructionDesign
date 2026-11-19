// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The "unsaved changes" question shared by the library and style windows: Apply / Discard /
//! Cancel, Cancel is the default and Esc. The rule is the same as in the type dialog; the draft
//! is kept until the question is answered.

use crate::ui_kit as kit;
use ddc_ui::theme::layout;
use egui::{self, RichText};

/// Answer of the question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Apply,
    Discard,
    Cancel,
}

/// What a switch or close request does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Switch {
    /// Nothing to lose: go ahead.
    Now,
    /// Unsaved edits and no answer yet: ask, keep the draft.
    Ask,
    ApplyThenGo,
    DiscardThenGo,
    /// The user cancelled: keep the draft and the selection as they are.
    Stay,
}

/// Pure rule for leaving a draft (switching the entry or closing). A clean draft never asks.
pub fn decide_switch(dirty: bool, choice: Option<Choice>) -> Switch {
    if !dirty {
        return Switch::Now;
    }
    match choice {
        None => Switch::Ask,
        Some(Choice::Apply) => Switch::ApplyThenGo,
        Some(Choice::Discard) => Switch::DiscardThenGo,
        Some(Choice::Cancel) => Switch::Stay,
    }
}

/// Esc answers Cancel, but not in the frame that opened the question.
pub fn esc_answer(answered: bool, asked_before: bool, esc: bool) -> Option<Choice> {
    (!answered && asked_before && esc).then_some(Choice::Cancel)
}

fn held_id() -> egui::Id {
    egui::Id::new("ddc.unsaved.held_esc")
}

/// Call before drawing the window under an open question: takes this frame's Esc away from the
/// window (so it does not close it and ask again) and keeps it for `ask`.
pub fn hold_esc(ctx: &egui::Context) {
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        ctx.data_mut(|d| d.insert_temp(held_id(), true));
    }
}

/// The held Esc, once; the window calls it at the end of its frame so none outlives the frame.
pub fn take_held_esc(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(held_id())).unwrap_or(false)
}

/// The shared footer: primary (click or Enter), `middle` extras, quiet Cancel (Esc), and the
/// danger Discard last, click only. Apply stands for the primary action.
pub fn footer(
    ui: &mut egui::Ui,
    primary: &str,
    cancel: &str,
    discard: &str,
    middle: impl FnOnce(&mut egui::Ui),
) -> Option<Choice> {
    let mut pick = None;
    kit::dialog_footer(ui, |ui| {
        if kit::primary_action(ui, primary, None) {
            pick = Some(Choice::Apply);
        }
        middle(ui);
        if kit::cancel_button(ui, cancel).clicked() {
            pick = Some(Choice::Cancel);
        }
        if kit::danger_button(ui, discard, true).clicked() {
            pick = Some(Choice::Discard);
        }
    });
    pick
}

/// Draws the question; None while unanswered. `key` names the modal, `name` the edited entry.
pub fn ask(
    ctx: &egui::Context,
    ru: bool,
    key: &str,
    name: &str,
    body: &str,
    apply_label: &str,
    asked_before: bool,
) -> Option<Choice> {
    let th = ddc_ui::theme::of(ctx);
    let mut pick = None;
    let m = kit::modal_dialog(key, ctx, |ui| {
        ui.set_width(layout::DIALOG_CONFIRM);
        kit::modal_title(ui, crate::tr_lang(ru, "type_layers.unsaved.title", &[("name", name)]));
        ui.label(RichText::new(body).color(th.c("text.secondary")));
        pick = footer(
            ui,
            apply_label,
            crate::tr_str(ru, "type_dialog.typedial.cancel"),
            crate::tr_str(ru, "type_layers.unsaved.discard"),
            |_| {},
        );
    });
    let held = take_held_esc(ctx);
    let esc = held || m.should_close() || ctx.input(|i| i.key_pressed(egui::Key::Escape));
    pick.or_else(|| esc_answer(false, asked_before, esc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_matches_the_type_dialog() {
        assert_eq!(decide_switch(false, None), Switch::Now);
        assert_eq!(decide_switch(false, Some(Choice::Cancel)), Switch::Now);
        assert_eq!(decide_switch(true, None), Switch::Ask);
        assert_eq!(decide_switch(true, Some(Choice::Apply)), Switch::ApplyThenGo);
        assert_eq!(decide_switch(true, Some(Choice::Discard)), Switch::DiscardThenGo);
        assert_eq!(decide_switch(true, Some(Choice::Cancel)), Switch::Stay);
    }

    #[test]
    fn held_esc_is_hidden_from_the_window_and_answers_once() {
        let ctx = ddc_ui::theme::headless_ctx();
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let _ = ctx.run(raw, |c| {
            hold_esc(c);
            assert!(!c.input(|i| i.key_pressed(egui::Key::Escape)), "the window sees no Esc");
            assert!(take_held_esc(c), "the question gets it");
            assert!(!take_held_esc(c), "only once");
        });
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Two idle frames, then one with `events`; the answer of the last frame.
    fn answer_after(events: Vec<egui::Event>) -> (Option<Choice>, Option<Choice>) {
        let ctx = ddc_ui::theme::headless_ctx();
        let mut last = None;
        let mut idle = None;
        for (n, ev) in [vec![], vec![], events].into_iter().enumerate() {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
                events: ev,
                ..Default::default()
            };
            let _ = ctx.run(raw, |c| {
                let r = ask(c, false, "ux54c.test", "Plan", "Body", "Save", n > 0);
                if n == 1 {
                    idle = r;
                }
                last = r;
            });
        }
        (idle, last)
    }

    #[test]
    fn headless_render_asks_nothing_by_itself() {
        let (idle, last) = answer_after(vec![]);
        assert_eq!(idle, None);
        assert_eq!(last, None);
    }

    #[test]
    fn esc_cancels_close() {
        let (_, last) = answer_after(vec![key(egui::Key::Escape)]);
        assert_eq!(last, Some(Choice::Cancel));
    }

    #[test]
    fn discard_is_never_default() {
        let (_, last) = answer_after(vec![key(egui::Key::Enter)]);
        assert_ne!(last, Some(Choice::Discard), "Enter never discards");
        assert_ne!(last, Some(Choice::Cancel));
    }

    #[test]
    fn save_and_discard_paths_stay_apart() {
        for c in [Choice::Apply, Choice::Cancel] {
            assert_ne!(decide_switch(true, Some(c)), Switch::DiscardThenGo);
        }
        assert_eq!(decide_switch(true, Some(Choice::Apply)), Switch::ApplyThenGo);
        assert_eq!(decide_switch(true, Some(Choice::Discard)), Switch::DiscardThenGo);
    }

    #[test]
    fn dialogs_use_kit_buttons_only() {
        let root = env!("CARGO_MANIFEST_DIR");
        let files = ["src/unsaved.rs", "src/save/report.rs", "src/project_ui.rs", "src/save/mod.rs"];
        let raw = ["ui.button(", "ui.small_button(", "egui::Button::new("];
        for f in files {
            let text = std::fs::read_to_string(format!("{root}/{f}")).unwrap();
            let body = text.split("#[cfg(test)]").next().unwrap();
            let hits: Vec<&str> = body
                .lines()
                // a menu row with a shortcut is not a dialog button
                .filter(|l| !l.contains("shortcut_text"))
                .filter(|l| !l.trim_start().starts_with("//") && raw.iter().any(|r| l.contains(r)))
                .collect();
            assert!(hits.is_empty(), "{f}: raw widgets {hits:?}");
            assert!(!body.contains("\\u{d7}"), "{f}: close glyph button");
        }
    }

    #[test]
    fn esc_cancels_only_an_open_question() {
        assert_eq!(esc_answer(false, true, true), Some(Choice::Cancel));
        assert_eq!(esc_answer(false, false, true), None, "the Esc that opened it");
        assert_eq!(esc_answer(true, true, true), None, "already answered");
        assert_eq!(esc_answer(false, true, false), None);
    }
}
