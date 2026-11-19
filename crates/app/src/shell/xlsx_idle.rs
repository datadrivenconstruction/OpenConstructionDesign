// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Empty state of the XLSX preview: the book matches the model, so there is no table. One
//! compact card says what Export and Sync do and when the book was last synced; the window
//! is then narrower and as tall as its content. Pure decisions plus one painter, no model access.
use super::xlsx_head::{age_of, age_text};
use crate::tr_lang;
use ddc_ui::theme::{layout, space};
use egui::{self, RichText};
use web_time::SystemTime;

/// Nothing to show in the diff table: no changes, errors, notes or matches to decide.
pub fn is_idle(total: usize, nerr: usize, nign: usize, notes: usize, matches: usize) -> bool {
    total == 0 && nerr == 0 && nign == 0 && notes == 0 && matches == 0
}

/// Window width: the empty state is compact, a table keeps the wide class.
pub fn width(idle: bool) -> f32 {
    if idle {
        layout::DIALOG_TOOL
    } else {
        layout::DIALOG_WIDE
    }
}

/// "Last synced: 5 min ago"; None when this book was never synced.
pub fn last_sync_line(at: Option<SystemTime>, ru: bool) -> Option<String> {
    let secs = at?.elapsed().map_or(0, |d| d.as_secs());
    let when = age_text(age_of(Some(secs)), ru);
    Some(tr_lang(ru, "xlsx_idle.last_sync", &[("when", &when)]))
}

/// Paints the card; returns its rectangle.
pub fn card(ui: &mut egui::Ui, ru: bool, synced_at: Option<SystemTime>) -> egui::Rect {
    let th = ddc_ui::theme::of(ui.ctx());
    let (sec, prim) = (th.c("text.secondary"), th.c("text.primary"));
    let t = |k: &str| tr_lang(ru, k, &[]);
    let out = ddc_ui::widgets::card(ui, &th, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t("xlsx_idle.idle_title")).strong().color(prim));
        ui.label(RichText::new(t("xlw.no_cell_changes_the_workbook_match")).color(sec));
        ui.add_space(space::SM);
        for (name, body) in [
            ("xlsx_idle.export_name", "xlsx_idle.export_body"),
            ("xlsx_idle.sync_name", "xlsx_idle.sync_body"),
        ] {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t(name)).strong().color(prim));
                ui.label(RichText::new(t(body)).color(sec));
            });
        }
        if let Some(line) = last_sync_line(synced_at, ru) {
            ui.add_space(space::XS);
            ui.label(RichText::new(line).weak());
        }
    });
    out.response.rect
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_kit::{Dialog, DialogSize};
    use std::time::Duration;

    fn themed() -> egui::Context {
        let mut th = ddc_ui::Theme::new(ddc_ui::theme::Mode::Light, ddc_ui::theme::Density::Comfortable);
        let cx = egui::Context::default();
        let _ = cx.run(Default::default(), |c| {
            th.apply(c);
        });
        cx
    }

    /// Settled window rectangle of the idle dialog with the card, as the XLSX window builds it.
    fn idle_window(cx: &egui::Context, ru: bool, at: Option<SystemTime>) -> (egui::Rect, egui::Rect) {
        let (mut win, mut card_rect) = (egui::Rect::NOTHING, egui::Rect::NOTHING);
        for _ in 0..4 {
            let _ = cx.run(Default::default(), |c| {
                let r = Dialog::new("t.xl53.idle", "T", DialogSize::Form)
                    .size([width(true), 0.0])
                    .stable()
                    .show(c, |ui| card(ui, ru, at));
                if let Some(r) = r {
                    win = r.response.rect;
                    card_rect = r.inner.unwrap_or(egui::Rect::NOTHING);
                }
            });
        }
        (win, card_rect)
    }

    #[test]
    fn card_only_without_anything_to_show() {
        assert!(is_idle(0, 0, 0, 0, 0));
        for v in [[1, 0, 0, 0, 0], [0, 1, 0, 0, 0], [0, 0, 1, 0, 0], [0, 0, 0, 1, 0], [0, 0, 0, 0, 1]] {
            assert!(!is_idle(v[0], v[1], v[2], v[3], v[4]), "{v:?}");
        }
        assert!(width(true) < width(false), "the empty state is compact");
    }

    #[test]
    fn idle_window_is_as_tall_as_its_content() {
        let cx = themed();
        for ru in [false, true] {
            let (win, c) = idle_window(&cx, ru, None);
            assert!(c.height() > 0.0, "card painted ru={ru}");
            assert!(win.height() < 320.0, "no tall empty area: {} ru={ru}", win.height());
            assert!(win.height() < layout::DIALOG_WIDE_H * 0.6, "ru={ru}");
            assert!(win.height() >= c.height(), "ru={ru}");
        }
    }

    #[test]
    fn last_sync_line_only_when_synced() {
        assert!(last_sync_line(None, false).is_none());
        let at = SystemTime::now() - Duration::from_secs(300);
        for ru in [false, true] {
            let s = last_sync_line(Some(at), ru).unwrap();
            assert!(s.contains('5') && !s.contains('{') && !s.starts_with("xlsx_idle."), "{s}");
        }
        let cx = themed();
        let (_, with) = idle_window(&cx, false, Some(at));
        let (_, without) = idle_window(&cx, false, None);
        assert!(with.height() > without.height(), "the line adds a row");
    }

    #[test]
    fn keys_exist_in_en_and_ru() {
        for k in [
            "xlsx_idle.idle_title",
            "xlsx_idle.export_name",
            "xlsx_idle.export_body",
            "xlsx_idle.sync_name",
            "xlsx_idle.sync_body",
            "xlsx_idle.last_sync",
        ] {
            let (en, ru) = (tr_lang(false, k, &[]), tr_lang(true, k, &[]));
            assert!(!en.is_empty() && en != k, "{k} en");
            assert!(!ru.is_empty() && ru != k && ru != en, "{k} ru");
        }
    }
}
