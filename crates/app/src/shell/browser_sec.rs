// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project browser kit: one row, one section header and one quiet add row for every section.
//!
//! All rows are `ROW_H` high with the icon centre and the label on the tree's slots, so views,
//! levels, sheets and the family trees line up. A section header carries its count at the right,
//! shows an Add button while hovered and stays pinned at the top of the scroll area while its
//! section is scrolled past (`end` paints it last, over the rows).
use super::browser::{paint_chevron, slots, ROW_H};
use ddc_ui::theme::{icon_size, space, stroke, Role};
use ddc_ui::widgets::shell_kit::elided;
use ddc_ui::Theme;
use egui::{self, Sense};

const HEADS: &str = "ddc.browser.heads";

/// A header drawn this frame, kept for `end`.
#[derive(Clone)]
struct Head {
    id: egui::Id,
    title: String,
    count: Option<usize>,
    top: f32,
    open: bool,
    /// Tooltip of the Add button, when the header has one.
    add_tip: Option<String>,
}

/// Add click made on a pinned header, handed to the section's own header next frame.
fn pin_add_id(id: egui::Id) -> egui::Id {
    id.with("pin.add")
}

/// What a section header reports.
pub(crate) struct Section {
    pub open: bool,
    /// The Add button of the header was clicked.
    pub add: bool,
}

/// One browser row: optional icon on the lead slot, elided label on the text slot, a muted
/// right column. `depth` shifts both slots by the tree indent.
#[allow(clippy::too_many_arguments)]
pub(crate) fn row(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    icon: Option<&str>,
    text: &str,
    right: &str,
    on: bool,
    depth: u16,
    sense: Sense,
) -> egui::Response {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), sense);
    if !ui.is_rect_visible(r) {
        return resp;
    }
    let fg = ddc_ui::widgets::paint_state(ui, th, r, &resp, on);
    let tc = if on { th.c("accent") } else { fg };
    let (lead_x, text_x) = slots(r.left(), depth);
    if let Some(name) = icon {
        let px = icon_size::SM;
        let ic = egui::Rect::from_center_size(
            egui::pos2(lead_x, r.center().y),
            egui::Vec2::splat(px),
        );
        icons.paint(ui, &ddc_ui::widgets::icon_name(name, on), px as u32, ic, tc);
    }
    let mut room = r.right() - text_x - space::SM;
    if !right.is_empty() {
        let gr = ui.painter().text(
            r.right_center() - egui::vec2(space::SM, 0.0),
            egui::Align2::RIGHT_CENTER,
            right,
            th.font(Role::Numeric),
            th.c("text.secondary"),
        );
        room -= gr.width() + space::SM;
    }
    let font = th.font(if on { Role::BodyStrong } else { Role::Body });
    let g = elided(ui, text, font, tc, room);
    let cut = g.elided;
    ui.painter().galley(egui::pos2(text_x, r.center().y - g.size().y / 2.0), g, tc);
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// Quiet "+ label" row on the same slots as the rows above it; true when clicked.
pub(crate) fn add_row(ui: &mut egui::Ui, th: &Theme, label: &str, tip: &str) -> bool {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
    if ui.is_rect_visible(r) {
        let fg = ddc_ui::widgets::paint_state(ui, th, r, &resp, false);
        let col = if resp.hovered() { fg } else { th.c("text.secondary") };
        let text_x = slots(r.left(), 0).1;
        let g = elided(ui, label, th.font(Role::Body), col, r.right() - text_x - space::SM);
        ui.painter().galley(egui::pos2(text_x, r.center().y - g.size().y / 2.0), g, col);
    }
    if tip.is_empty() {
        resp.clicked()
    } else {
        ddc_ui::widgets::rich_tooltip(resp, th, tip, "", "").clicked()
    }
}

fn count_galley(ui: &egui::Ui, th: &Theme, n: usize) -> std::sync::Arc<egui::Galley> {
    ui.painter().layout_no_wrap(n.to_string(), th.font(Role::Numeric), th.c("text.secondary"))
}

/// Hit slot of the Add button: left of the count, or at the right edge without one.
fn add_slot(ui: &egui::Ui, th: &Theme, r: egui::Rect, count: Option<usize>) -> egui::Rect {
    let right = match count {
        Some(n) => r.right() - space::SM - count_galley(ui, th, n).size().x - space::XS,
        None => r.right() - space::SM,
    };
    egui::Rect::from_min_max(egui::pos2(right - ROW_H, r.top()), egui::pos2(right, r.bottom()))
}

fn paint_plus(p: &egui::Painter, slot: egui::Rect, col: egui::Color32) {
    let c = slot.center();
    let a = icon_size::XS / 2.0;
    let s = egui::Stroke::new(stroke::HAIRLINE * 1.5, col);
    p.line_segment([c - egui::vec2(a, 0.0), c + egui::vec2(a, 0.0)], s);
    p.line_segment([c - egui::vec2(0.0, a), c + egui::vec2(0.0, a)], s);
}

/// Chevron on the lead slot, upper-case caption on the text slot, count at the right.
#[allow(clippy::too_many_arguments)]
fn paint_head(
    ui: &egui::Ui,
    p: &egui::Painter,
    th: &Theme,
    r: egui::Rect,
    title: &str,
    count: Option<usize>,
    open: bool,
    hot: bool,
    reserve: f32,
) {
    let col = if hot { th.c("text.primary") } else { th.c("text.secondary") };
    let (lead_x, text_x) = slots(r.left(), 0);
    paint_chevron(p, egui::pos2(lead_x, r.center().y), open, col);
    let mut room = r.right() - text_x - space::SM - reserve;
    if let Some(n) = count {
        let g = count_galley(ui, th, n);
        room -= g.size().x + space::SM;
        let pos = egui::pos2(r.right() - space::SM - g.size().x, r.center().y - g.size().y / 2.0);
        p.galley(pos, g, th.c("text.secondary"));
    }
    let g = elided(ui, &title.to_uppercase(), th.font(Role::Section), col, room);
    p.galley(egui::pos2(text_x, r.center().y - g.size().y / 2.0), g, col);
}

/// Collapsible section header. `key` names the fold state (kept for the session and shared with
/// `ui_kit::heading`); `add_tip` turns on the hover Add button.
pub(crate) fn section(
    ui: &mut egui::Ui,
    th: &Theme,
    key: &str,
    title: &str,
    count: Option<usize>,
    add_tip: Option<&str>,
) -> Section {
    ui.add_space(space::SM);
    let id = crate::ui_kit::section_id(key);
    let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(id, true));
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
    let hot = ui.rect_contains_pointer(r);
    let mut add = ui.data_mut(|d| d.remove_temp::<bool>(pin_add_id(id))).unwrap_or(false);
    let mut reserve = 0.0;
    if let Some(tip) = add_tip {
        let slot = add_slot(ui, th, r, count);
        reserve = r.right() - slot.left();
        let hit = ui.interact(slot, id.with("add"), Sense::click());
        if hot || hit.has_focus() {
            let col = if hit.hovered() { th.c("text.primary") } else { th.c("text.secondary") };
            paint_plus(ui.painter(), slot, col);
        }
        add |= ddc_ui::widgets::rich_tooltip(hit, th, tip, "", "").clicked();
    }
    if resp.clicked() && !add {
        open = !open;
        ui.data_mut(|d| d.insert_persisted(id, open));
    }
    paint_head(ui, ui.painter(), th, r, title, count, open, hot, reserve);
    let head = Head {
        id,
        title: title.to_string(),
        count,
        top: r.top(),
        open,
        add_tip: add_tip.map(str::to_string),
    };
    ui.data_mut(|d| d.get_temp_mut_or_default::<Vec<Head>>(egui::Id::new(HEADS)).push(head));
    Section { open, add }
}

/// The header whose section is scrolled past `clip_top`: `(index, y)` with `y` the top of the
/// pinned header, pushed up by the next header. `spans` are `(header top, section end)`.
pub(crate) fn pinned(spans: &[(f32, f32)], clip_top: f32, h: f32) -> Option<(usize, f32)> {
    let i = spans.iter().rposition(|&(top, end)| top < clip_top - 0.5 && end > clip_top)?;
    Some((i, clip_top.min(spans[i].1 - h)))
}

/// Call once at the end of the scroll content: pins the header of the section in view.
pub(crate) fn end(ui: &mut egui::Ui, th: &Theme) {
    let heads: Vec<Head> =
        ui.data_mut(|d| d.remove_temp::<Vec<Head>>(egui::Id::new(HEADS))).unwrap_or_default();
    let clip = ui.clip_rect();
    let bottom = ui.min_rect().bottom();
    let spans: Vec<(f32, f32)> = heads
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let end = heads.get(i + 1).map_or(bottom, |n| n.top - space::SM);
            (h.top, if h.open { end } else { h.top })
        })
        .collect();
    let Some((i, y)) = pinned(&spans, clip.top(), ROW_H) else {
        return;
    };
    let h = &heads[i];
    let m = ui.max_rect();
    let rect = egui::Rect::from_min_size(egui::pos2(m.left(), y), egui::vec2(m.width(), ROW_H));
    let hot = ui.rect_contains_pointer(rect);
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, th.c("bg.panel"));
    let slot = h.add_tip.as_ref().map(|_| add_slot(ui, th, rect, h.count));
    let reserve = slot.map_or(0.0, |s| rect.right() - s.left());
    paint_head(ui, &p, th, rect, &h.title, h.count, h.open, hot, reserve);
    let line = egui::Stroke::new(stroke::HAIRLINE, th.c("border.subtle"));
    p.hline(rect.x_range(), rect.bottom(), line);
    // the whole strip belongs to the header: the row scrolled under it never gets the click
    let hit = ui.interact(rect, h.id.with("pin"), Sense::click());
    let mut add = false;
    if let (Some(slot), Some(tip)) = (slot, h.add_tip.as_deref()) {
        let ahit = ui.interact(slot, h.id.with("pin.addbtn"), Sense::click());
        if hot || ahit.has_focus() {
            let col = if ahit.hovered() { th.c("text.primary") } else { th.c("text.secondary") };
            paint_plus(&p, slot, col);
        }
        add = ddc_ui::widgets::rich_tooltip(ahit, th, tip, "", "").clicked();
    }
    if add {
        ui.data_mut(|d| d.insert_temp(pin_add_id(h.id), true));
        ui.ctx().request_repaint();
    } else if hit.clicked() {
        let open = !h.open;
        ui.data_mut(|d| d.insert_persisted(h.id, open));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_of_the_section_in_view_is_pinned() {
        let spans = [(0.0, 100.0), (110.0, 210.0), (220.0, 320.0)];
        assert_eq!(pinned(&spans, 0.0, 24.0), None);
        assert_eq!(pinned(&spans, 50.0, 24.0), Some((0, 50.0)));
        assert_eq!(pinned(&spans, 105.0, 24.0), None);
        assert_eq!(pinned(&spans, 150.0, 24.0), Some((1, 150.0)));
    }

    #[test]
    fn the_next_header_pushes_the_pinned_one_up() {
        let spans = [(0.0, 100.0), (110.0, 210.0)];
        let (i, y) = pinned(&spans, 90.0, 24.0).unwrap();
        assert_eq!(i, 0);
        assert_eq!(y, 76.0);
    }

    #[test]
    fn a_closed_section_pins_nothing() {
        assert_eq!(pinned(&[(10.0, 10.0)], 40.0, 24.0), None);
    }
}
