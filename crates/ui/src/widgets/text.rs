// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Single-line text that never overflows: elided with «…», full text in a tooltip.
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Color32, FontId, Galley, Rect, Response, Sense, TextFormat, Ui};
use std::sync::Arc;

/// One line no wider than `max_w`; `Galley::elided` tells whether it was cut.
pub fn elide(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    ui.painter().layout_job(job)
}

/// Label that takes at most the available width; shows the full text on hover when elided.
pub fn elided_label(ui: &mut Ui, text: &str, font: FontId, color: Color32) -> Response {
    let g = elide(ui, text, font, color, ui.available_width());
    let cut = g.elided;
    let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, color);
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// Paints one line inside `rect`, vertically centred. Returns true if the text was elided.
pub fn paint_in(
    ui: &Ui,
    rect: Rect,
    text: &str,
    font: FontId,
    color: Color32,
    align: Align,
) -> bool {
    let g = elide(ui, text, font, color, rect.width());
    let cut = g.elided;
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x - g.size().x / 2.0,
        Align::Max => rect.right() - g.size().x,
    };
    let pos = egui::pos2(x, rect.center().y - g.size().y / 2.0);
    ui.painter().galley(pos, g, color);
    cut
}
