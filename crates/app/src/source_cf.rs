// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Conflict list of the linked-sources window: a table on the kit tokens with a two-way
//! Mine | File segmented control per row. Pure pick bookkeeping lives here so it is testable
//! without a window.

use std::collections::BTreeMap;

use ddc_model::source::Pick;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Column};
use ddc_ui::Theme;
use egui::{self, Align, Rect, Sense, Vec2};

/// One conflict as shown in the table (texts are already localised).
pub(crate) struct CfRow {
    pub id: i64,
    pub element: String,
    pub tip: String,
    pub field: String,
    pub base: String,
    pub mine: String,
    pub theirs: String,
    /// Hover text for the file side (child rows are taken as a whole).
    pub theirs_tip: String,
}

const HEAD: [&str; 6] = [
    "src.c.element",
    "src.c.field",
    "src.c.base",
    "src.c.mine",
    "src.c.theirs",
    "src.c.pick",
];

/// Segment index of a pick; no pick is out of range so no side is highlighted.
pub(crate) fn side_index(p: Option<Pick>) -> usize {
    match p {
        Some(Pick::Mine) => 0,
        Some(Pick::Theirs) => 1,
        None => 2,
    }
}

pub(crate) fn pick_from_index(i: usize) -> Pick {
    if i == 0 {
        Pick::Mine
    } else {
        Pick::Theirs
    }
}

/// Sets the same side for every listed conflict ("all mine" / "all from file").
pub(crate) fn pick_all(picks: &mut BTreeMap<i64, Pick>, ids: impl Iterator<Item = i64>, side: Pick) {
    for id in ids {
        picks.insert(id, side);
    }
}

/// Why Apply is off: key of the reason, `None` when at least one pick exists.
pub(crate) fn apply_blocked(picks: &BTreeMap<i64, Pick>) -> Option<&'static str> {
    picks.is_empty().then_some("dlg.src.pick_first")
}

/// Plain number text: gets right-aligned.
pub(crate) fn is_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.parse::<f64>().is_ok()
}

fn align_of(s: &str) -> Align {
    if is_number(s) {
        Align::Max
    } else {
        Align::Min
    }
}

fn pick_cell_width(ui: &egui::Ui, th: &Theme, labels: [&str; 2]) -> f32 {
    let w: f32 = labels
        .iter()
        .map(|t| {
            ui.painter()
                .layout_no_wrap((*t).to_owned(), th.font(Role::Label), th.c("text.primary"))
                .size()
                .x
                + 2.0 * space::MD
        })
        .sum();
    w + 2.0 * space::XXS + th.d().pad_x * 2.0
}

/// Sticky header plus virtualised rows. The segmented control of a row writes into `picks`.
pub(crate) fn conflict_table(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    rows: &[CfRow],
    picks: &mut BTreeMap<i64, Pick>,
    max_h: f32,
) {
    let s = th.sem();
    let d = th.d();
    let titles: Vec<String> = HEAD.iter().map(|k| cat.t(k)).collect();
    let (mine_l, theirs_l) = (cat.t("src.cf.mine"), cat.t("src.cf.theirs"));
    let cols = [
        Column::text(&titles[0]).fill(1.6),
        Column::text(&titles[1]).fill(1.0),
        Column::text(&titles[2]).fill(1.0),
        Column::text(&titles[3]).fill(1.0),
        Column::text(&titles[4]).fill(1.0),
        Column::text(&titles[5]).fixed(pick_cell_width(ui, th, [&mine_l, &theirs_l])),
    ];
    let row_h = d.row.max(d.control + 2.0 * space::XXS);
    let w = ui.available_width();
    let widths = widgets::table::column_widths(&cols, w);
    let cell_rects = |r: Rect| -> Vec<Rect> {
        let mut x = r.left();
        widths
            .iter()
            .map(|cw| {
                let c = Rect::from_min_size(egui::pos2(x, r.top()), Vec2::new(*cw, r.height()));
                x += cw;
                c
            })
            .collect()
    };
    let inset = |r: Rect| r.shrink2(Vec2::new(d.pad_x, 0.0));

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let (head, _) = ui.allocate_exact_size(Vec2::new(w, d.row), Sense::hover());
        for (i, r) in cell_rects(head).into_iter().enumerate() {
            widgets::text::paint_in(
                ui,
                inset(r),
                cols[i].title,
                th.font(Role::TableHeader),
                s.text_secondary,
                Align::Min,
            );
        }
        ui.painter().hline(
            head.x_range(),
            head.bottom(),
            egui::Stroke::new(ddc_ui::theme::stroke::HAIRLINE, s.border_subtle),
        );
        egui::ScrollArea::vertical()
            .id_salt("src_cf_rows")
            .max_height(max_h.max(row_h))
            .auto_shrink([false, true])
            .show_rows(ui, row_h, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                for row in &rows[range] {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::hover());
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 0.0, th.hover_fill());
                    }
                    let cur = picks.get(&row.id).copied();
                    let dim_mine = cur == Some(Pick::Theirs);
                    let dim_theirs = cur == Some(Pick::Mine);
                    let ink = |dim: bool| if dim { s.text_secondary } else { s.text_primary };
                    let cells = cell_rects(rect);
                    let texts: [(&str, egui::Color32, Align, &str); 5] = [
                        (row.element.as_str(), s.text_primary, Align::Min, row.tip.as_str()),
                        (row.field.as_str(), s.text_primary, Align::Min, ""),
                        (row.base.as_str(), s.text_secondary, align_of(&row.base), ""),
                        (row.mine.as_str(), ink(dim_mine), align_of(&row.mine), ""),
                        (
                            row.theirs.as_str(),
                            ink(dim_theirs),
                            align_of(&row.theirs),
                            row.theirs_tip.as_str(),
                        ),
                    ];
                    for (i, (text, col, al, tip)) in texts.iter().enumerate() {
                        let r = inset(cells[i]);
                        let cut = widgets::text::paint_in(
                            ui,
                            r,
                            text,
                            th.font(Role::Body),
                            *col,
                            *al,
                        );
                        let show = if !tip.is_empty() {
                            Some(if cut { format!("{text}\n{tip}") } else { (*tip).to_owned() })
                        } else {
                            cut.then(|| (*text).to_owned())
                        };
                        if let Some(t) = show {
                            ui.interact(r, ui.id().with(("cf_tip", row.id, i)), Sense::hover())
                                .on_hover_text(t);
                        }
                    }
                    let pc = inset(cells[5]);
                    let seg = Rect::from_center_size(
                        egui::pos2(pc.center().x, rect.center().y),
                        Vec2::new(pc.width(), d.control),
                    );
                    let hit = ui
                        .scope_builder(
                            egui::UiBuilder::new()
                                .id_salt(("cf_pick", row.id))
                                .max_rect(seg),
                            |ui| {
                                widgets::segmented(
                                    ui,
                                    th,
                                    &[mine_l.as_str(), theirs_l.as_str()],
                                    side_index(cur),
                                )
                            },
                        )
                        .inner;
                    if let Some(i) = hit {
                        picks.insert(row.id, pick_from_index(i));
                    }
                    ui.painter().hline(
                        rect.x_range(),
                        rect.bottom(),
                        egui::Stroke::new(ddc_ui::theme::stroke::HAIRLINE, s.border_subtle),
                    );
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;

    fn row(id: i64, name: &str) -> CfRow {
        CfRow {
            id,
            element: name.to_owned(),
            tip: "guid".to_owned(),
            field: "height".to_owned(),
            base: "3".to_owned(),
            mine: "3.5".to_owned(),
            theirs: "4".to_owned(),
            theirs_tip: String::new(),
        }
    }

    fn headless(f: impl FnMut(&egui::Context)) {
        let ctx = ddc_ui::theme::headless_ctx();
        let _ = ctx.run(egui::RawInput::default(), f);
    }

    #[test]
    fn segment_pick_sets_the_matching_side() {
        let mut picks = BTreeMap::new();
        picks.insert(7, pick_from_index(1));
        assert_eq!(picks.get(&7), Some(&Pick::Theirs));
        picks.insert(7, pick_from_index(0));
        assert_eq!(picks.get(&7), Some(&Pick::Mine));
        assert_eq!(side_index(Some(Pick::Mine)), 0);
        assert_eq!(side_index(Some(Pick::Theirs)), 1);
        assert!(side_index(None) >= 2, "no pick highlights no side");
    }

    #[test]
    fn all_mine_and_all_theirs_cover_every_conflict() {
        let mut picks = BTreeMap::new();
        pick_all(&mut picks, [1, 2, 3].into_iter(), Pick::Mine);
        assert_eq!(picks.len(), 3);
        assert!(picks.values().all(|p| *p == Pick::Mine));
        pick_all(&mut picks, [1, 2, 3].into_iter(), Pick::Theirs);
        assert!(picks.values().all(|p| *p == Pick::Theirs));
    }

    #[test]
    fn apply_is_blocked_until_a_pick_exists() {
        let mut picks = BTreeMap::new();
        assert_eq!(apply_blocked(&picks), Some("dlg.src.pick_first"));
        picks.insert(1, Pick::Mine);
        assert_eq!(apply_blocked(&picks), None);
    }

    #[test]
    fn numbers_align_right_text_left() {
        assert_eq!(align_of("3.5"), Align::Max);
        assert_eq!(align_of("-12"), Align::Max);
        assert_eq!(align_of("EI 90"), Align::Min);
        assert_eq!(align_of(""), Align::Min);
    }

    #[test]
    fn long_element_names_elide() {
        headless(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let th = ddc_ui::theme::of(ui.ctx());
                let long = "Wall type with a very long descriptive name ".repeat(8);
                let g = widgets::elide(
                    ui,
                    &long,
                    th.font(Role::Body),
                    th.sem().text_primary,
                    th.d().row * 6.0,
                );
                assert!(g.elided);
                let g = widgets::elide(
                    ui,
                    "W1",
                    th.font(Role::Body),
                    th.sem().text_primary,
                    th.d().row * 6.0,
                );
                assert!(!g.elided);
            });
        });
    }

    #[test]
    fn table_renders_with_long_names_and_picks_untouched() {
        // a local catalog: the global UI language is never switched
        let cat = Catalog::new(Lang::En);
        let long = "Curtain panel ".repeat(40);
        let rows = vec![row(1, "W1"), row(2, &long)];
        let mut picks = BTreeMap::new();
        picks.insert(1, Pick::Theirs);
        headless(|ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let th = ddc_ui::theme::of(ui.ctx());
                conflict_table(ui, &th, &cat, &rows, &mut picks, 300.0);
            });
        });
        assert_eq!(picks.len(), 1);
        assert_eq!(picks.get(&1), Some(&Pick::Theirs));
    }

    #[test]
    fn window_code_uses_no_raw_widgets_for_options_actions_and_conflicts() {
        let src = include_str!("source_ui.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap()];
        for pat in [
            "ui.checkbox(",
            "ui.button(",
            "egui::Button",
            "selectable_label",
            "add_enabled(",
            "Grid::new(\"src_cf\")",
        ] {
            assert!(!prod.contains(pat), "raw widget {pat} in source_ui.rs");
        }
        let own = include_str!("source_cf.rs");
        let own = &own[..own.find("#[cfg(test)]").unwrap()];
        for pat in ["Color32::from", "from_rgb", "selectable_label", "ui.button("] {
            assert!(!own.contains(pat), "hard-coded {pat} in source_cf.rs");
        }
    }

    #[test]
    fn new_strings_exist_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let cat = Catalog::new(lang);
            for k in ["src.more", "src.cf.chosen", "src.opt.allow_mass_warn"] {
                assert_ne!(cat.t(k), k, "missing {k}");
            }
        }
    }
}
