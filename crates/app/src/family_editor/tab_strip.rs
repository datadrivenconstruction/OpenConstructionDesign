// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Section tabs of the family editor: full labels in up to two rows, short labels when that is not
//! enough, and the rest behind a "More" menu. A label is never elided; the selected tab is
//! always visible.

use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Button, MenuTone};
use ddc_ui::Theme;
use egui::{self, Rect, Vec2};

pub const MAX_ROWS: usize = 2;

/// Height of one tab row: the kit's tab height.
pub fn row_h() -> f32 {
    ddc_ui::theme::control::TAB_H
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub rows: Vec<Vec<usize>>,
    pub hidden: Vec<usize>,
}

fn greedy(w: &[f32], sel: usize, caps: &[f32]) -> Layout {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut hidden = Vec::new();
    let mut used = 0.0_f32;
    let mut closed = false;
    for i in 0..w.len() {
        if closed {
            if i == sel {
                rows.last_mut().map(|r| r.push(i));
            } else {
                hidden.push(i);
            }
            continue;
        }
        loop {
            let last = rows.len() == caps.len();
            let reserve = if last && i < sel { w[sel] } else { 0.0 };
            let cap = caps[rows.len() - 1];
            let empty = rows.last().is_some_and(|r| r.is_empty());
            if used + w[i] + reserve <= cap || empty {
                rows.last_mut().map(|r| r.push(i));
                used += w[i];
                break;
            }
            if !last {
                rows.push(Vec::new());
                used = 0.0;
            } else {
                hidden.push(i);
                closed = true;
                break;
            }
        }
    }
    Layout { rows, hidden }
}

/// Places tabs of the given widths into at most `max_rows` rows of `avail` px. When something
/// does not fit, the last row leaves room for the "More" button (`more_w`).
pub fn layout(w: &[f32], sel: usize, avail: f32, more_w: f32, max_rows: usize) -> Layout {
    let n = max_rows.max(1);
    let caps = vec![avail; n];
    let first = greedy(w, sel, &caps);
    if first.hidden.is_empty() {
        return first;
    }
    let mut caps = caps;
    caps[n - 1] = (avail - more_w).max(0.0);
    greedy(w, sel, &caps)
}

fn text_w(ui: &egui::Ui, th: &Theme, role: Role, t: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(t.to_owned(), th.font(role), th.sem().text_primary)
        .size()
        .x
}

/// Width one tab takes in `ddc_ui::widgets::tab_bar`.
pub fn tab_w(ui: &egui::Ui, th: &Theme, t: &str) -> f32 {
    text_w(ui, th, Role::Tab, t) + 2.0 * space::MD
}

/// Layout for the current width: full labels when they fit, else short ones.
pub fn plan(
    ui: &egui::Ui,
    th: &Theme,
    full: &[String],
    short: &[String],
    more: &str,
    sel: usize,
    avail: f32,
) -> (Layout, bool) {
    let more_w = tab_w(ui, th, more) + space::MD;
    let wf: Vec<f32> = full.iter().map(|t| tab_w(ui, th, t)).collect();
    let lf = layout(&wf, sel, avail, more_w, MAX_ROWS);
    if lf.hidden.is_empty() {
        return (lf, false);
    }
    let ws: Vec<f32> = short.iter().map(|t| tab_w(ui, th, t)).collect();
    (layout(&ws, sel, avail, more_w, MAX_ROWS), true)
}

/// Draws the strip; returns the newly chosen tab index (same order as `full`).
pub fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    full: &[String],
    short: &[String],
    more: &str,
    sel: &mut usize,
) -> Option<usize> {
    let avail = (ui.available_width() - 1.0).max(0.0);
    let (lay, use_short) = plan(ui, th, full, short, more, *sel, avail);
    let labels = if use_short { short } else { full };
    let mut out = None;
    let rows = lay.rows.len();
    for (ri, row) in lay.rows.iter().enumerate() {
        let names: Vec<&str> = row.iter().map(|i| labels[*i].as_str()).collect();
        let mut local = row.iter().position(|i| i == sel).unwrap_or(usize::MAX);
        let row_w: f32 = row.iter().map(|i| tab_w(ui, th, &labels[*i])).sum::<f32>() + 1.0;
        let last = ri + 1 == rows;
        ui.horizontal(|ui| {
            crate::ui_kit::no_gap(ui, crate::ui_kit::Gap::Cells);
            // one tab row high and top-down: a centred layout floats the bar mid-area
            let area = Rect::from_min_size(ui.cursor().min, Vec2::new(row_w, row_h()));
            let top_down = egui::Layout::top_down(egui::Align::Min);
            let hit = ui
                .scope_builder(egui::UiBuilder::new().max_rect(area).layout(top_down), |ui| {
                    widgets::tab_bar(ui, th, &names, &mut local)
                })
                .inner;
            if let Some(k) = hit {
                out = Some(row[k]);
            }
            if last && !lay.hidden.is_empty() {
                let resp = Button::quiet(more).show(ui, th);
                egui::Popup::menu(&resp)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                    .show(|ui| {
                        for i in &lay.hidden {
                            if widgets::menu_item(ui, th, &full[*i], "", MenuTone::Normal, true)
                                .clicked()
                            {
                                out = Some(*i);
                            }
                        }
                    });
            }
        });
    }
    if let Some(i) = out {
        *sel = i;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::{Catalog, Lang};

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn labels(cat: &Catalog, short: bool) -> Vec<String> {
        super::super::Tab::ALL
            .iter()
            .map(|t| cat.t(if short { t.short_key() } else { t.key() }))
            .collect()
    }

    fn catalog(lang: Lang) -> Catalog {
        Catalog::new(lang)
    }

    #[test]
    fn layout_keeps_order_and_selected_visible() {
        let w = [100.0; 8];
        let all = layout(&w, 0, 1000.0, 50.0, 2);
        assert_eq!(all.rows, vec![(0..8).collect::<Vec<_>>()]);
        assert!(all.hidden.is_empty());
        let two = layout(&w, 0, 450.0, 50.0, 2);
        assert!(two.hidden.is_empty() && two.rows.len() == 2, "{two:?}");
        let narrow = layout(&w, 0, 250.0, 50.0, 2);
        assert!(!narrow.hidden.is_empty());
        let last_row: f32 = narrow.rows.last().unwrap().len() as f32 * 100.0;
        assert!(last_row + 50.0 <= 250.0, "room for More: {narrow:?}");
        for sel in 0..8 {
            let l = layout(&w, sel, 250.0, 50.0, 2);
            let vis: Vec<usize> = l.rows.iter().flatten().copied().collect();
            assert!(vis.contains(&sel), "sel {sel}: {l:?}");
            assert!(vis.windows(2).all(|p| p[0] < p[1]), "order {l:?}");
            let mut every: Vec<usize> = vis.iter().chain(l.hidden.iter()).copied().collect();
            every.sort_unstable();
            assert_eq!(every, (0..8).collect::<Vec<_>>());
        }
    }

    fn column_width() -> f32 {
        // default family window is about 1000 px wide, the tab column is half of it
        480.0
    }

    #[test]
    fn every_label_fits_at_default_width_in_en_and_ru() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        for lang in [Lang::En, Lang::Ru] {
            let cat = catalog(lang);
            let (full, short) = (labels(&cat, false), labels(&cat, true));
            let ctx = font_ctx();
            let mut seen = None;
            for _ in 0..3 {
                let _ = ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        let more = cat.t("fe.tab.more");
                        seen = Some(plan(ui, &th, &full, &short, &more, 0, column_width()));
                    });
                });
            }
            let (lay, _) = seen.unwrap();
            assert!(lay.hidden.is_empty(), "{lang:?}: {lay:?}");
            assert_eq!(lay.rows.iter().map(Vec::len).sum::<usize>(), 8);
        }
    }

    #[test]
    fn narrow_width_reaches_the_more_menu_and_switch_keeps_state() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        for lang in [Lang::En, Lang::Ru] {
            let cat = catalog(lang);
            let (full, short) = (labels(&cat, false), labels(&cat, true));
            let ctx = font_ctx();
            let mut sel = 5usize;
            let mut hidden = 0;
            for _ in 0..3 {
                let _ = ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        ui.scope(|ui| {
                            ui.set_max_width(220.0);
                            let more = cat.t("fe.tab.more");
                            let (lay, _) = plan(ui, &th, &full, &short, &more, sel, 219.0);
                            hidden = lay.hidden.len();
                            assert!(lay.rows.iter().flatten().any(|i| *i == sel));
                            let r = show(ui, &th, &full, &short, &more, &mut sel);
                            assert!(r.is_none());
                        });
                    });
                });
            }
            assert!(hidden > 0, "{lang:?}");
            assert_eq!(sel, 5, "drawing never changes the tab");
        }
    }

    /// Height the strip takes and the top of its first tab, drawn at `width` from the panel top.
    fn strip_metrics(
        full: &[String],
        short: &[String],
        more: &str,
        width: f32,
    ) -> (f32, f32, f32, usize) {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        let ctx = font_ctx();
        let mut out = (0.0, 0.0, 0.0, 0usize);
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    ui.set_max_width(width);
                    let top = ui.cursor().min.y;
                    let mut sel = 0usize;
                    let (lay, _) = plan(ui, &th, full, short, more, 0, (width - 1.0).max(0.0));
                    show(ui, &th, full, short, more, &mut sel);
                    let gap = ui.spacing().item_spacing.y;
                    // the caller's item spacing after the last row is not part of the strip
                    out = (ui.cursor().min.y - top - gap, top, gap, lay.rows.len());
                });
            });
        }
        out
    }

    #[test]
    fn strip_height_is_the_row_token_times_rows_in_en_and_ru() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = catalog(lang);
            let (full, short) = (labels(&cat, false), labels(&cat, true));
            let more = cat.t("fe.tab.more");
            for width in [480.0, 300.0, 220.0] {
                let (h, _, gap, rows) = strip_metrics(&full, &short, &more, width);
                let rows = rows as f32;
                let most = rows * row_h() + (rows - 1.0) * gap + 1.0;
                assert!(h >= row_h() - 0.5, "{lang:?} {width}: {h}");
                assert!(h <= most, "{lang:?} {width}: {h} > {most}, not 200 px rows");
            }
        }
    }

    #[test]
    fn one_row_strip_is_exactly_one_tab_high_and_starts_at_the_top() {
        let full: Vec<String> = ["One", "Two", "Three"].map(String::from).to_vec();
        let (h, top, _, rows) = strip_metrics(&full, &full, "More", 480.0);
        assert_eq!(rows, 1);
        assert!((h - row_h()).abs() < 1.0, "{h} vs {}", row_h());
        // the first tab sits at the panel top, not centred in a taller area
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        let ctx = font_ctx();
        let mut tops = vec![];
        for _ in 0..3 {
            tops.clear();
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let mut sel = 0usize;
                    show(ui, &th, &full, &full, "More", &mut sel);
                    tops.push(ui.min_rect().top());
                });
            });
        }
        assert!((tops[0] - top).abs() < 1.0, "{tops:?} {top}");
    }

    #[test]
    fn many_long_names_stay_within_two_rows_and_reach_the_more_menu() {
        let full: Vec<String> = (0..14)
            .map(|i| format!("A rather long section name number {i}"))
            .collect();
        let (h, _, gap, rows) = strip_metrics(&full, &full, "More", 480.0);
        assert!(rows <= MAX_ROWS, "{rows}");
        let most = MAX_ROWS as f32 * row_h() + (MAX_ROWS as f32 - 1.0) * gap + 1.0;
        assert!(h <= most, "{h} > {most}");
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Comfortable);
        let ctx = font_ctx();
        let mut hidden = 0;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    hidden = plan(ui, &th, &full, &full, "More", 0, 479.0).0.hidden.len();
                });
            });
        }
        assert!(hidden > 0, "long names must overflow into More");
    }

    #[test]
    fn strings_exist_in_en_and_ru() {
        let cat = catalog(Lang::En);
        for lang in [Lang::En, Lang::Ru] {
            let t = cat.table(lang);
            let mut keys: Vec<String> = vec!["fe.tab.more".into()];
            for tab in super::super::Tab::ALL {
                keys.push(tab.key().to_string());
                keys.push(tab.short_key().to_string());
            }
            for k in keys {
                assert!(
                    t.get(&k).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
                    "{k} {lang:?}"
                );
            }
        }
    }

    #[test]
    fn body_uses_the_strip_not_the_squeezing_tab_bar() {
        let src = include_str!("mod.rs");
        // test-only helpers sit among the code: the scan ends at the test module
        let src = src.split("#[cfg(test)]\nmod tests").next().unwrap_or(src);
        assert!(src.contains("tab_strip::show"));
        assert!(!src.contains("widgets::tab_bar"));
    }
}
