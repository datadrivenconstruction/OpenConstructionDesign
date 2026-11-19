// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Re-export refused because the new book would drop parts of the old one. The card lists
//! them; "Export anyway" runs the same export with exactly that list accepted (the old book
//! is kept whole beside the new one), "Keep the workbook" leaves everything as it is.

use std::path::PathBuf;

use ddc_io::xlsx;

use super::EditorApp;
use crate::ui_kit as kit;
use crate::xl_text::{self, Lang};

/// Items listed in the card; the rest are counted.
const SHOWN: usize = 12;

pub struct LossCard {
    pub path: PathBuf,
    pub again: bool,
    pub scope: Option<xlsx::Scope>,
    /// English items as the export reported them; accepted exactly as listed.
    pub items: Vec<String>,
}

#[derive(PartialEq)]
enum Pick {
    None,
    Export,
    Keep,
}

/// What the card decided. Dropping parts of the old book needs its own click; Enter, Esc and the
/// close button all keep the workbook.
fn outcome(keep: bool, export: bool, closed: bool) -> Pick {
    if export && !keep && !closed {
        Pick::Export
    } else if keep || closed {
        Pick::Keep
    } else {
        Pick::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_and_enter_keep_the_workbook() {
        assert!(outcome(false, false, true) == Pick::Keep);
        assert!(outcome(true, false, false) == Pick::Keep);
    }

    #[test]
    fn dropping_needs_an_explicit_click() {
        assert!(outcome(false, false, false) == Pick::None);
        assert!(outcome(false, true, false) == Pick::Export);
        assert!(outcome(false, true, true) == Pick::Keep);
    }
}

impl EditorApp {
    pub(super) fn xlsx_loss_card(&mut self, ctx: &egui::Context, lang: Lang) {
        let Some(c) = self.xlsx_loss.take() else {
            return;
        };
        let ru = lang == Lang::Ru;
        let t = |k: &'static str| crate::tr_str(ru, k);
        let file = c.path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (mut keep, mut export, mut open) = (false, false, true);
        kit::Dialog::new("xlloss", t("xlloss.title"), kit::DialogSize::Confirm)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(file).strong()).truncate());
                ui.add_space(ddc_ui::theme::space::SM);
                // the list scrolls in what the hint and the footer leave (their height last
                // frame): on a short screen the buttons stay in the window
                let tail_id = egui::Id::new("xlloss.tail");
                let tail = ui.data(|d| d.get_temp::<f32>(tail_id)).unwrap_or(0.0);
                let least = ui.text_style_height(&egui::TextStyle::Body) * 3.0;
                egui::ScrollArea::vertical()
                    .id_salt("xlloss.items")
                    .max_height((kit::room_left(ui) - tail).max(least))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for i in c.items.iter().take(SHOWN) {
                            let line = format!("\u{2022} {}", xl_text::tr(i, lang));
                            ui.add(egui::Label::new(line).wrap());
                        }
                        if c.items.len() > SHOWN {
                            let more = (c.items.len() - SHOWN).to_string();
                            ui.weak(crate::tr_lang(ru, "xlloss.more", &[("n", &more)]));
                        }
                    });
                // the gap egui leaves after the list belongs to the tail, or the window is a
                // spacing taller than the room
                let tail_top = ui.cursor().top() - ui.spacing().item_spacing.y;
                ui.add_space(ddc_ui::theme::space::SM);
                ui.add(egui::Label::new(egui::RichText::new(t("xlloss.hint")).weak()).wrap());
                ui.add_space(ddc_ui::theme::space::MD);
                // keeping the book is the safe default; dropping is never the primary action
                kit::dialog_footer(ui, |ui| {
                    keep = kit::primary_action(ui, t("xlloss.keep"), None);
                    export = kit::danger_button(ui, t("xlloss.export_anyway"), true).clicked();
                });
                let tail_now = ui.min_rect().bottom() - tail_top;
                ui.data_mut(|d| d.insert_temp(tail_id, tail_now));
            });
        let pick = outcome(keep, export, !open);
        match pick {
            Pick::None => self.xlsx_loss = Some(c),
            Pick::Keep => self.ed.status = Some(t("xlloss.kept").to_string()),
            Pick::Export => self.xlsx_run_accepting(&c.path, c.again, c.scope, c.items),
        }
    }
}
