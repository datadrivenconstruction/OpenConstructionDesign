// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fixed-size building blocks of the import windows: a window's state changes what is inside
//! these blocks and never their size, so nothing jumps between pick, progress and result.
use super::dialog::elided_weak;
use ddc_ui::i18n::Lang;
use ddc_ui::theme::{self, Role};
use ddc_ui::widgets::{self, field::format_number, NumberFormat};
use egui::{self, Align, Layout, RichText, Sense, Vec2};

/// Caption column of a label and value row.
pub const ROW_LABEL_W: f32 = 120.0;

fn strict_child(ui: &mut egui::Ui, salt: &str, size: Vec2, layout: Layout) -> egui::Ui {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .id_salt(salt)
            .layout(layout),
    );
    child.set_clip_rect(child.clip_rect().intersect(rect.expand(2.0)));
    child
}

/// A block of exactly `h` points across the available width; longer content scrolls inside it.
pub fn zone<R>(ui: &mut egui::Ui, salt: &str, h: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = Vec2::new(ui.available_width(), h);
    let mut child = strict_child(ui, salt, size, Layout::top_down(Align::Min));
    let out = egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false])
        .max_height(h)
        .show(&mut child, add);
    super::mark_scroll_viewport(ui.ctx(), out.inner_rect);
    out.inner
}

/// Height of `rows` rows of `row_h` with the ui's row gap between them.
pub fn rows_height(ui: &egui::Ui, rows: usize, row_h: f32) -> f32 {
    let n = rows as f32;
    n * row_h + (n - 1.0).max(0.0) * ui.spacing().item_spacing.y
}

/// List that shows at most `max_rows` rows of `row_h` and then scrolls inside itself, ending on
/// a row boundary. The bar is solid and always visible, so a list that goes on is never
/// mistaken for a list that ends. `add` draws the rows and gets the width left of the bar.
pub fn scroll_list<R>(
    ui: &mut egui::Ui,
    salt: &str,
    max_rows: usize,
    row_h: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let h = rows_height(ui, max_rows, row_h);
    ui.scope(|ui| {
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        let out = egui::ScrollArea::vertical()
            .id_salt(salt)
            .auto_shrink([false, true])
            .max_height(h)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, add);
        super::mark_scroll_viewport(ui.ctx(), out.inner_rect);
        out.inner
    })
    .inner
}

/// A row one control high across the available width, whatever its widgets ask for.
pub fn fixed_row<R>(ui: &mut egui::Ui, salt: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let h = theme::of(ui.ctx()).control_h(theme::ControlSize::Medium);
    let size = Vec2::new(ui.available_width(), h);
    let mut child = strict_child(ui, salt, size, Layout::left_to_right(Align::Center));
    add(&mut child)
}

/// A cell of width `w` inside a row; text in it elides against that width.
pub fn fixed_cell<R>(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = w.max(0.0);
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        Vec2::new(w, h),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(w);
            ui.set_max_width(w);
            add(ui)
        },
    )
    .inner
}

/// Caption in a fixed column, value beside it: labels and controls align across windows.
pub fn labeled_row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    labeled_row_id(ui, label, |ui, _| add(ui))
}

/// `labeled_row` that also hands the label widget id to `add`, for `labelled_by` on inputs.
pub fn labeled_row_id<R>(
    ui: &mut egui::Ui,
    label: &str,
    add: impl FnOnce(&mut egui::Ui, egui::Id) -> R,
) -> R {
    // the kit form row: label, field and unit on one centre line, one row height everywhere
    let th = theme::of(ui.ctx());
    widgets::form::field_row(ui, &th, label, ROW_LABEL_W, add)
}

/// One elided line whose tooltip always carries `tip`, for a file name and its full path.
pub fn name_with_tip(ui: &mut egui::Ui, text: &str, tip: &str, strong: bool) -> egui::Response {
    let th = theme::of(ui.ctx());
    let font = th.font(if strong { Role::BodyStrong } else { Role::Body });
    let color = th.sem().text_primary;
    let g = widgets::elide(ui, text, font, color, ui.available_width());
    let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, color);
    resp.on_hover_text(tip)
}

/// The last component of a path, for titles; the whole string when it has none.
pub fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// At most `max` characters, the end replaced by an ellipsis: for widgets that take no elision.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Large numbers with captions in equal columns; a `warn` number takes the warning colour.
pub fn count_tiles(ui: &mut egui::Ui, items: &[(usize, String, bool)]) {
    let th = theme::of(ui.ctx());
    let ru = crate::ui_lang_now();
    let nf = NumberFormat::for_lang(if ru { Lang::Ru } else { Lang::En });
    let warn = ui.visuals().warn_fg_color;
    ui.columns(items.len().max(1), |cols| {
        for (col, (n, label, is_warn)) in cols.iter_mut().zip(items) {
            let color = if *is_warn {
                warn
            } else {
                th.sem().text_primary
            };
            col.label(
                RichText::new(format_number(*n as f64, 0, nf))
                    .font(th.font(Role::Heading))
                    .color(color),
            );
            elided_weak(col, label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_falls_back_to_the_whole_text() {
        assert_eq!(file_name_of("a/b/House.rvt"), "House.rvt");
        assert_eq!(file_name_of(""), "");
    }

    #[test]
    fn shorten_keeps_short_text_and_cuts_long() {
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("abcd", 3), "ab…");
    }

    #[test]
    fn a_zone_reserves_its_height_with_any_content() {
        let ctx = egui::Context::default();
        let mut heights = vec![];
        for lines in [0, 200] {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let top = ui.cursor().min.y;
                    zone(ui, "t", 80.0, |ui| {
                        for _ in 0..lines {
                            ui.label("row");
                        }
                    });
                    heights.push(ui.cursor().min.y - top);
                });
            });
        }
        assert_eq!(heights[0], heights[1]);
    }
}
