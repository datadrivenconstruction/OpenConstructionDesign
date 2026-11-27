// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Single-line text that never overflows: elided with «…», full text in a tooltip.
use egui::text::{LayoutJob, TextWrapping};
use egui::{Align, Color32, FontId, Galley, Rect, Response, Sense, TextFormat, Ui};
use std::sync::Arc;

/// One line no wider than `max_w`; `Galley::elided` tells whether it was cut.
pub fn elide(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let shown = crate::rtl::display(text).into_owned();
    let mut job = LayoutJob::single_section(shown, TextFormat::simple(font, color));
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

/// Up to `rows` lines no wider than `max_w`, wrapped at words, the last one elided with «…».
pub fn elide_rows(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32, rows: usize) -> Arc<Galley> {
    let max_w = max_w.max(1.0);
    if rows <= 1 {
        return elide(ui, text, font, color, max_w);
    }
    // a word wider than the box breaks inside itself, as file names do, rather than lose its end
    let long_word = text.split_whitespace().any(|w| ui.painter().layout_no_wrap(w.to_owned(), font.clone(), color).size().x > max_w);
    let wrap = TextWrapping { max_width: max_w, max_rows: rows, break_anywhere: long_word, overflow_character: Some('…') };
    let shown = if crate::rtl::contains_rtl(text) {
        // wrap the logical text, then order each line: wrapping reordered text cuts the wrong words
        let measure = |s: &str| ui.painter().layout_no_wrap(s.to_owned(), font.clone(), color).size().x;
        let all = crate::rtl::display_wrapped(text, crate::i18n::rtl(), max_w, measure);
        let lines: Vec<&str> = all.split('\n').collect();
        if lines.len() <= rows {
            all
        } else {
            format!("{}…", lines[..rows].join("\n"))
        }
    } else {
        text.to_owned()
    };
    let mut job = LayoutJob::single_section(shown, TextFormat::simple(font, color));
    job.wrap = wrap;
    ui.painter().layout_job(job)
}

/// As [`elided_label`], over up to `rows` lines: a long name wraps before it is cut.
pub fn wrapped_label(ui: &mut Ui, text: &str, font: FontId, color: Color32, rows: usize) -> Response {
    let g = elide_rows(ui, text, font, color, ui.available_width(), rows);
    let cut = g.elided || g.text().ends_with('…') && !text.ends_with('…');
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
