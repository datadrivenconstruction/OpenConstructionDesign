// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Form rows: the one primitive behind every «label — field — unit» line of the app (Properties,
//! option bars, canvas bars, dialogs, AI tabs). A row has one fixed height; label, field, value
//! and unit sit on the row's centre line, numbers are right-aligned in tabular digits, units are
//! muted, and a read-only value is plain text where an editable one is a framed field.
use super::text::elide;
use crate::theme::{layout, space, Role};
use crate::Theme;
use egui::{pos2, vec2, Align, Color32, Galley, Layout, Rect, Response, Sense, Ui, UiBuilder};
use std::sync::Arc;

/// Height of every form row: the density control height and `space::XXS` of air above and
/// below, so two rows of fields never touch.
pub fn row_h(th: &Theme) -> f32 {
    th.d().control + 2.0 * space::XXS
}

/// Single-line width of `text` in the label font.
pub fn text_w(ui: &Ui, th: &Theme, text: &str) -> f32 {
    let font = th.font(Role::Body);
    ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER).size().x)
}

/// Label column of a panel or dialog: as wide as its widest label, within the Properties
/// limits, so every field of the panel starts at one x.
pub fn label_col_w(ui: &Ui, th: &Theme, labels: &[&str]) -> f32 {
    let widest = labels.iter().map(|l| text_w(ui, th, l)).fold(0.0, f32::max);
    widest.ceil().clamp(layout::PROP_KEY_MIN, layout::PROP_KEY_MAX)
}

/// A fixed-height row laid out left to right; every item is centred on the row's centre line.
/// Items keep their own width, so the row takes only what they need.
pub fn inline<R>(ui: &mut Ui, th: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    let h = row_h(th);
    let w = ui.available_width();
    let w = if w.is_finite() { w } else { 0.0 };
    ui.allocate_ui_with_layout(vec2(w, h), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_height(h);
        ui.set_max_height(h);
        ui.spacing_mut().interact_size.y = th.d().control;
        add(ui)
    })
    .inner
}

/// Paints `g` in a slot `w` wide and one control high, centred on the slot's centre line.
fn slot(ui: &mut Ui, th: &Theme, g: Arc<Galley>, w: f32, align: Align, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, th.d().control), Sense::hover());
    paint_line(ui, rect, g, align, color);
    resp
}

/// Paints one laid-out line inside `rect`, centred vertically and placed by `align`.
fn paint_line(ui: &Ui, rect: Rect, g: Arc<Galley>, align: Align, color: Color32) {
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x - g.size().x / 2.0,
        Align::Max => rect.right() - g.size().x,
    };
    ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, color);
}

/// Field label: secondary ink in the field font, one line, never wrapped. Wider than `max_w`
/// it ends with «…» and shows the whole text on hover.
pub fn label(ui: &mut Ui, th: &Theme, text: &str, max_w: f32) -> Response {
    let col = th.sem().text_secondary;
    let g = elide(ui, text, th.font(Role::Body), col, max_w);
    let cut = g.elided;
    let w = g.size().x;
    let resp = slot(ui, th, g, w, Align::Min, col);
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// Muted unit after a field or a value («mm», «m²»).
pub fn unit(ui: &mut Ui, th: &Theme, unit: &str) -> Response {
    let col = th.sem().text_secondary;
    let g = ui.painter().layout_no_wrap(unit.to_owned(), th.font(Role::Body), col);
    let w = g.size().x;
    slot(ui, th, g, w, Align::Min, col)
}

/// Read-only value: primary ink in tabular digits. Given a width `w` it is right-aligned where
/// an editable field of that width puts its text, but with no frame and no fill, so it never
/// reads as an input. One line always; a value wider than its slot ends with «…» and shows
/// whole on hover.
pub fn value(ui: &mut Ui, th: &Theme, text: &str, w: Option<f32>) -> Response {
    let col = th.sem().text_primary;
    let pad = th.d().pad_x;
    let max = w.map_or(f32::INFINITY, |w| (w - pad).max(1.0));
    let g = elide(ui, text, th.font(Role::Numeric), col, max);
    let cut = g.elided;
    let slot_w = w.unwrap_or(g.size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2(slot_w, th.d().control), Sense::hover());
    let inner = if w.is_some() { rect.shrink2(vec2(pad, 0.0)) } else { rect };
    paint_line(ui, inner, g, Align::Max, col);
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// Paints a label on one line inside `rect` (centred vertically, left), elided to its width;
/// the response carries the full text as tooltip when it was cut.
pub fn label_in(ui: &mut Ui, th: &Theme, rect: Rect, id: egui::Id, text: &str) -> Response {
    let col = th.sem().text_secondary;
    let g = elide(ui, text, th.font(Role::Body), col, rect.width());
    let cut = g.elided;
    paint_line(ui, rect, g, Align::Min, col);
    let resp = ui.interact(rect, id, Sense::hover());
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

/// One form row across the available width: the label in a column of `label_w`, the fields
/// after it left to right, everything on the row's centre line. `add` gets the field area and
/// the label's id (for `labelled_by`). Content taller than a row (a wrapped note) grows the
/// row downwards and keeps the label on the first line.
pub fn field_row<R>(
    ui: &mut Ui,
    th: &Theme,
    text: &str,
    label_w: f32,
    add: impl FnOnce(&mut Ui, egui::Id) -> R,
) -> R {
    let h = row_h(th);
    let w = ui.available_width();
    let w = if w.is_finite() { w } else { label_w + space::SM };
    let rect = Rect::from_min_size(ui.available_rect_before_wrap().min, vec2(w, h));
    let lw = label_w.min(w).max(0.0);
    let lrect = Rect::from_min_size(rect.min, vec2(lw, h));
    let id = ui.id().with(("ddc.form.label", text));
    let lid = label_in(ui, th, lrect, id, text).id;
    let left = (lrect.right() + space::SM).min(rect.right());
    let frect = Rect::from_min_max(pos2(left, rect.top()), rect.max);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(frect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().interact_size.y = th.d().control;
    let out = add(&mut child, lid);
    // the row takes what it shows, so an auto-sized window does not grow to its limit
    ui.advance_cursor_after_rect(lrect.union(child.min_rect()));
    out
}

/// True for text that is a number with at most a short unit («3 600», «18,00 m²», «−0,5 °»):
/// such text must never break over lines.
pub fn is_numeric_text(s: &str) -> bool {
    const MARKS: &str = " .,+-\u{2212}\u{a0}\u{202f}\u{d7}\u{b7}/:%\u{b0}'\"\u{b2}\u{b3}";
    let num = |c: char| c.is_ascii_digit() || MARKS.contains(c);
    let letters = s.chars().filter(|c| !num(*c)).count();
    s.chars().any(|c| c.is_ascii_digit()) && letters <= 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Density, Mode};

    fn run(add: impl FnOnce(&mut Ui)) {
        let ctx = crate::theme::headless_ctx();
        let mut add = Some(add);
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                if let Some(f) = add.take() {
                    f(ui);
                }
            });
        });
    }

    #[test]
    fn row_height_is_the_control_and_air_in_each_density() {
        for d in [Density::Compact, Density::Comfortable] {
            let th = Theme::new(Mode::Light, d);
            assert_eq!(row_h(&th), th.d().control + 4.0);
        }
    }

    #[test]
    fn label_field_value_and_unit_share_the_centre_line() {
        for d in [Density::Compact, Density::Comfortable] {
            let th = Theme::new(Mode::Dark, d);
            let mut rects = vec![];
            run(|ui| {
                inline(ui, &th, |ui| {
                    rects.push(label(ui, &th, "Height", 200.0).rect);
                    let mut s = "3 000".to_string();
                    let f = super::super::TextField::new(&mut s).width(80.0).show(ui, &th);
                    rects.push(f.response.rect);
                    rects.push(unit(ui, &th, "mm").rect);
                    rects.push(value(ui, &th, "6 000", Some(80.0)).rect);
                });
            });
            let c0 = rects[0].center().y;
            for r in &rects {
                assert!((r.center().y - c0).abs() <= 0.5, "{d:?}: {rects:?}");
            }
        }
    }

    #[test]
    fn field_row_puts_the_field_after_the_label_column() {
        let th = Theme::new(Mode::Light, Density::Compact);
        let mut got = None;
        run(|ui| {
            let top = ui.cursor().top();
            let r = field_row(ui, &th, "Offset", 120.0, |ui, _| {
                let mut s = "0".to_string();
                super::super::TextField::new(&mut s).width(80.0).show(ui, &th).response.rect
            });
            got = Some((top, r, ui.cursor().top()));
        });
        let (top, r, next) = got.unwrap();
        assert!(r.left() >= 120.0 + space::SM, "{r:?}");
        assert!((r.center().y - (top + row_h(&th) / 2.0)).abs() <= 0.5, "{r:?}");
        assert!(next >= top + row_h(&th), "{top} {next}");
    }

    #[test]
    fn numbers_with_short_units_are_numeric_and_words_are_not() {
        let nums = ["3 600", "3,600 \u{43c}\u{b3}", "18,00 m\u{b2}", "6 000 mm \u{b7} 0,0\u{b0}"];
        for s in nums.into_iter().chain(["-0.5"]) {
            assert!(is_numeric_text(s), "{s}");
        }
        for s in ["Level 1", "\u{42d}\u{442}\u{430}\u{436} 1", "Walls", ""] {
            assert!(!is_numeric_text(s), "{s}");
        }
    }
}
