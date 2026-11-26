// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Frame of the visibility and graphics tables: a header that stays above the scrolling rows,
//! with its columns on the same left edges as the rows, and the note of a template-locked tab.
//!
//! The rows are an `egui::Grid` whose column widths follow the content. The first body row
//! reports where each column starts (`Rec::at`); the header cells take those edges one frame
//! later. A zero-height first grid row carries the header texts, so a column is never narrower
//! than its title. Text comes from an explicit language, never from the global one.

use ddc_ui::sem::{space, stroke};
use ddc_ui::theme::{self, Role};
use ddc_ui::widgets::{text::paint_in, Button};
use egui::{self, Align, Id, Rect, Sense};

/// What the last frame of a table drew: read by the next frame and by the tests.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Geo {
    /// Header texts as drawn.
    pub names: Vec<String>,
    /// Header cell rects.
    pub head: Vec<Rect>,
    /// Left edge of every column in the first body row (empty while there is no row).
    pub body: Vec<f32>,
    /// Top of the first body row; it moves up as the rows scroll.
    pub row_top: f32,
    /// Scroll area of the rows and its offset.
    pub scroll: Option<Id>,
    pub offset: f32,
    /// Visible window of the rows.
    pub view: Rect,
}

impl Default for Geo {
    fn default() -> Self {
        Self {
            names: vec![],
            head: vec![],
            body: vec![],
            row_top: 0.0,
            scroll: None,
            offset: 0.0,
            view: Rect::NOTHING,
        }
    }
}

fn geo_id(id: &str) -> Id {
    Id::new(("vv.tbl.geo", id))
}

/// The last frame of table `id`.
#[cfg(test)]
pub(crate) fn geo(ctx: &egui::Context, id: &str) -> Option<Geo> {
    ctx.data(|d| d.get_temp(geo_id(id)))
}

/// Collects the left edge of each column from the first body row.
pub(super) struct Rec {
    edges: Vec<f32>,
    rows: usize,
    top: f32,
}

impl Rec {
    /// Call before the cell of column `col`, in every row (only the first one is read).
    pub(super) fn at(&mut self, ui: &egui::Ui, col: usize) {
        if self.rows == 0 && col < self.edges.len() {
            let p = ui.next_widget_position();
            // next_widget_position is the centre of the free space; the edge is the cell's left
            self.edges[col] = ui.cursor().min.x;
            if col == 0 {
                self.top = p.y;
            }
        }
    }

    /// Ends the grid row.
    pub(super) fn end_row(&mut self, ui: &mut egui::Ui) {
        ui.end_row();
        self.rows += 1;
    }
}

/// Height of `rows` table rows at the row token, with the gaps between them.
pub(super) fn rows_h(ui: &egui::Ui, rows: f32) -> f32 {
    let th = theme::of(ui.ctx());
    rows * (th.d().row + ui.spacing().item_spacing.y)
}

/// The table: sticky header, then the rows in a scroll area of at most `max_h`.
pub(super) fn show(
    ui: &mut egui::Ui,
    id: &'static str,
    cols: &[&str],
    ru: bool,
    max_h: f32,
    body: impl FnOnce(&mut egui::Ui, &mut Rec),
) {
    let th = theme::of(ui.ctx());
    let s = th.sem();
    let n = cols.len();
    let key = geo_id(id);
    let prev: Option<Geo> = ui.ctx().data(|d| d.get_temp(key));
    let font = th.font(Role::Label);
    let names: Vec<String> = cols.iter().map(|k| crate::tr_lang(ru, k, &[])).collect();
    let widths: Vec<f32> = names
        .iter()
        .map(|t| {
            let g = ui.painter().layout_no_wrap(t.clone(), font.clone(), s.text_secondary);
            g.size().x
        })
        .collect();
    let row = th.d().row;
    let gap = ui.spacing().item_spacing.x;
    let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row), Sense::hover());
    let rel: Option<Vec<f32>> = prev
        .as_ref()
        .filter(|g| g.body.len() == n)
        .map(|g| g.body.iter().map(|x| x - g.body[0]).collect());
    let mut lefts = Vec::with_capacity(n);
    let mut next = hr.left();
    for (i, w) in widths.iter().enumerate() {
        let left = rel.as_ref().map_or(next, |r| hr.left() + r[i]);
        lefts.push(left);
        next = left + w + gap;
    }
    let mut head = Vec::with_capacity(n);
    for (i, name) in names.iter().enumerate() {
        let right = lefts.get(i + 1).map_or(hr.right(), |x| x - gap);
        let cell = Rect::from_min_max(
            egui::pos2(lefts[i], hr.top()),
            egui::pos2(right.max(lefts[i]), hr.bottom()),
        );
        let cut = paint_in(ui, cell, name, font.clone(), s.text_secondary, Align::Min);
        if cut {
            ui.interact(cell, Id::new((id, "head", i)), Sense::hover())
                .on_hover_text(name);
        }
        head.push(cell);
    }
    ui.painter().hline(
        hr.x_range(),
        hr.bottom(),
        egui::Stroke::new(stroke::HAIRLINE, s.border_subtle),
    );
    let mut rec = Rec {
        edges: vec![0.0; n],
        rows: 0,
        top: 0.0,
    };
    let out = egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(max_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().interact_size.y = row;
            // Without a cap a cell may only be as wide as its column so far, which the header
            // text alone sets on the first frame and the cell then never grows: names get cut.
            let cap = ui.available_width();
            egui::Grid::new((id, "grid"))
                .max_col_width(cap)
                .num_columns(n)
                .striped(true)
                .show(ui, |ui| {
                    for w in &widths {
                        ui.allocate_space(egui::vec2(*w, 0.0));
                    }
                    ui.end_row();
                    body(ui, &mut rec);
                });
        });
    crate::ui_kit::mark_scroll_viewport(ui.ctx(), out.inner_rect);
    let known = rec.rows > 0;
    let geo = Geo {
        names,
        head,
        body: if known { rec.edges } else { Vec::new() },
        row_top: rec.top,
        scroll: Some(out.id),
        offset: out.state.offset.y,
        view: out.inner_rect,
    };
    let moved = prev.as_ref().map_or(known, |p| {
        p.body.len() != geo.body.len()
            || p.body.iter().zip(&geo.body).any(|(a, b)| (a - b).abs() > 0.5)
    });
    ui.ctx().data_mut(|d| d.insert_temp(key, geo));
    if moved {
        ui.ctx().request_repaint();
    }
}

/// Text of the note above a locked tab.
pub(super) fn lock_text(name: &str, ru: bool) -> String {
    crate::tr_lang(ru, "view_template.locked", &[("name", name)])
}

/// Tooltip of a locked control.
pub(super) fn lock_tip(name: &str, ru: bool) -> String {
    crate::tr_lang(ru, "view_template.locked_tip", &[("name", name)])
}

/// Muted line above a tab the view template controls; true when Edit template was clicked.
pub(super) fn lock_note(ui: &mut egui::Ui, name: &str, ru: bool) -> bool {
    let th = theme::of(ui.ctx());
    let mut clicked = false;
    ui.horizontal(|ui| {
        let text = egui::RichText::new(lock_text(name, ru))
            .font(th.font(Role::Caption))
            .color(th.sem().text_secondary);
        ui.add(egui::Label::new(text).truncate())
            .on_hover_text(lock_tip(name, ru));
        let edit = crate::tr_lang(ru, "view_template.edit_template", &[]);
        clicked = Button::quiet(&edit).small().show(ui, &th).clicked();
    });
    ui.add_space(space::XS);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view_gfx_ui::vv_rows::{CAT_COLS, FILTER_COLS};
    use ddc_ui::{Catalog, Density, Lang, Mode, Theme};

    const ROWS: usize = 200;

    fn screen() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
            ..Default::default()
        }
    }

    fn run(ctx: &egui::Context, th: &mut Theme, mut f: impl FnMut(&egui::Context)) {
        let _ = ctx.run(screen(), |ctx| {
            let _ = th.apply(ctx);
            f(ctx);
        });
    }

    fn draw(ctx: &egui::Context, id: &'static str, cols: &[&str], ru: bool, rows: usize) {
        egui::CentralPanel::default().show(ctx, |ui| {
            show(ui, id, cols, ru, rows_h(ui, 10.0), |ui, rec| {
                for i in 0..rows {
                    for c in 0..cols.len() {
                        rec.at(ui, c);
                        // one wide cell, so the widths are content driven
                        let long = c == 0 && i == 3;
                        ui.label(if long { "A very long row name".into() } else { format!("{i}") });
                    }
                    rec.end_row(ui);
                }
            });
        });
    }

    fn settled(id: &'static str, cols: &[&str], ru: bool, rows: usize) -> (egui::Context, Theme) {
        let ctx = crate::layout_contract::font_ctx();
        let mut th = Theme::new(Mode::Dark, Density::Compact);
        for _ in 0..4 {
            run(&ctx, &mut th, |c| draw(c, id, cols, ru, rows));
        }
        (ctx, th)
    }

    fn scroll_to(ctx: &egui::Context, id: Id, y: f32) {
        let mut st = egui::scroll_area::State::load(ctx, id).expect("scroll state");
        st.offset.y = y;
        st.store(ctx, id);
    }

    fn header_stays(cols: &[&str], tag: &'static str, lang: Lang) {
        let ru = lang == Lang::Ru;
        let (ctx, mut th) = settled(tag, cols, ru, ROWS);
        let before = geo(&ctx, tag).expect("geo");
        assert_eq!(before.names.len(), cols.len());
        let cat = Catalog::new(lang);
        for (name, key) in before.names.iter().zip(cols) {
            assert_eq!(name, &cat.t(key), "{tag} {lang:?} header text");
        }
        scroll_to(&ctx, before.scroll.unwrap(), 200.0 * 24.0);
        for _ in 0..3 {
            run(&ctx, &mut th, |c| draw(c, tag, cols, ru, ROWS));
        }
        let after = geo(&ctx, tag).expect("geo");
        assert!(after.offset > 100.0, "{tag}: the rows did not scroll ({})", after.offset);
        assert!(after.row_top < before.row_top - 100.0, "{tag}: first row did not move");
        assert_eq!(before.head, after.head, "{tag} {lang:?}: header moved with the rows");
        assert!(after.head[0].top() < after.view.top(), "{tag}: header sits above the rows");
        assert!(after.head[0].bottom() <= after.view.top() + 0.5, "{tag}: header over the rows");
    }

    #[test]
    fn header_stays_visible_after_scrolling_en() {
        header_stays(&CAT_COLS, "t.cats", Lang::En);
        header_stays(&FILTER_COLS, "t.flts", Lang::En);
    }

    #[test]
    fn header_stays_visible_after_scrolling_ru() {
        header_stays(&CAT_COLS, "t.cats", Lang::Ru);
        header_stays(&FILTER_COLS, "t.flts", Lang::Ru);
    }

    fn edges_match(cols: &[&str], tag: &'static str, ru: bool) {
        let (ctx, mut th) = settled(tag, cols, ru, 30);
        let g = geo(&ctx, tag).expect("geo");
        assert_eq!(g.body.len(), cols.len());
        for (i, (h, b)) in g.head.iter().zip(&g.body).enumerate() {
            assert!((h.left() - b).abs() < 0.5, "{tag} ru={ru} column {i}: {h:?} vs {b}");
        }
        // still aligned once the rows are scrolled
        scroll_to(&ctx, g.scroll.unwrap(), 300.0);
        for _ in 0..3 {
            run(&ctx, &mut th, |c| draw(c, tag, cols, ru, 30));
        }
        let g = geo(&ctx, tag).expect("geo");
        for (i, (h, b)) in g.head.iter().zip(&g.body).enumerate() {
            assert!((h.left() - b).abs() < 0.5, "{tag} scrolled column {i}");
        }
    }

    #[test]
    fn column_edges_of_header_and_rows_match_en() {
        edges_match(&CAT_COLS, "t.cats", false);
        edges_match(&FILTER_COLS, "t.flts", false);
    }

    #[test]
    fn column_edges_of_header_and_rows_match_ru() {
        edges_match(&CAT_COLS, "t.cats", true);
        edges_match(&FILTER_COLS, "t.flts", true);
    }

    fn texts(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|x| texts(x, out)),
            _ => {}
        }
    }

    fn painted(
        ctx: &egui::Context,
        th: &mut Theme,
        mut f: impl FnMut(&egui::Context),
    ) -> Vec<String> {
        let out = ctx.run(screen(), |ctx| {
            let _ = th.apply(ctx);
            f(ctx);
        });
        let mut all = vec![];
        out.shapes.iter().for_each(|c| texts(&c.shape, &mut all));
        all
    }

    #[test]
    fn lock_note_names_the_template_and_offers_the_edit() {
        for lang in [Lang::En, Lang::Ru] {
            let ru = lang == Lang::Ru;
            let ctx = crate::layout_contract::font_ctx();
            let mut th = Theme::new(Mode::Dark, Density::Compact);
            let mut all = vec![];
            for _ in 0..2 {
                all = painted(&ctx, &mut th, |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        lock_note(ui, "Plans", ru);
                    });
                });
            }
            let cat = Catalog::new(lang);
            let want = cat.tf("view_template.locked", &[("name", "Plans")]);
            assert!(all.contains(&want), "{lang:?}: note {want:?} not in {all:?}");
            assert!(all.contains(&cat.t("view_template.edit_template")), "{lang:?}: edit action");
            assert!(want.contains("Plans"));
            assert_eq!(lock_text("Plans", ru), want);
            assert_eq!(lock_tip("Plans", ru), cat.tf("view_template.locked_tip", &[("name", "Plans")]));
            assert_eq!(want.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), ru);
        }
    }
}
