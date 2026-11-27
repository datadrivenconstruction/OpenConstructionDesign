// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The hint card of the interaction spine at the bottom-left corner of the view,
//! with the fixes of the last refusal as buttons.
//! Rows come from `Interaction::hint_rows`; the widget is `ddc_ui::hint_card`.
use crate::interact::Scope;
use crate::refusals::{fix_label, run_fix};
use crate::shell::EditorApp;
use egui;

/// Draw the card while a tool runs and the preference is on. The card takes no pointer
/// input; only the fix buttons do, and only while a refusal offers them.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let ed = &app.ed;
    let offer = ed.ix.offer().cloned();
    if !app.prefs.hint_card || (*ed.ix.scope() == Scope::Idle && offer.is_none()) {
        return;
    }
    let rows = ed.ix.hint_rows(ed);
    let vals = ed.ix.value_rows(ed);
    let refusal = ed.ix.refusal_text().or_else(|| {
        offer.as_ref().map(|r| crate::refusals::RefusalText::text(r, &app.sh.cat))
    });
    let labels: Vec<String> =
        offer.as_ref().map(|o| o.fixes.iter().map(|f| fix_label(f, &app.sh.cat)).collect()).unwrap_or_default();
    let mut chosen = None;
    egui::Area::new(egui::Id::new("ix_hint_card"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -40.0))
        .interactable(offer.is_some())
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                if let Some(o) = &offer {
                    ui.horizontal_wrapped(|ui| {
                        for (f, l) in o.fixes.iter().zip(&labels) {
                            if ddc_ui::widgets::Button::secondary(l).show(ui, &app.sh.theme).clicked() {
                                chosen = Some(f.clone());
                            }
                        }
                    });
                }
                ddc_ui::hint_card::draw(ui, &rows, &vals, refusal.as_deref());
            });
        });
    if let Some(f) = chosen {
        run_fix(app, &f);
    }
}
