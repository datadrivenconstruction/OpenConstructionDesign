//! Properties panel frame shared by every element kind: one header, a strip of key figures
//! (read-only), then captioned groups of editable rows. Code that holds only a `Ui` uses these;
//! theme and icons come from the context.
use crate::shell::prop_head::{header_card, header_parts};
use ddc_ui::theme::{self, space, Role};
use ddc_ui::widgets::{self, shell_kit, with_icons};

/// Element header: category icon, name, muted id, optional type line (own row).
pub fn element_header(ui: &mut egui::Ui, icon: &str, title: &str, subtitle: &str, id: i64) {
    let th = theme::of(ui.ctx());
    let mut p = header_parts(title, None, "", id);
    p.subtitle = subtitle.to_string();
    // a kind without its own icon gets the generic component glyph, never a placeholder box
    let icon = if ddc_ui::icons::source(icon).is_some() { icon } else { "component" };
    let ctx = ui.ctx().clone();
    with_icons(&ctx, |icons| header_card(ui, &th, icons, icon, &p));
}

/// Caption above a group of rows (Dimensions, Construction, ...).
pub fn group_title(ui: &mut egui::Ui, text: &str) {
    let th = theme::of(ui.ctx());
    ui.add_space(space::SM);
    shell_kit::section_title(ui, &th, text);
    ui.add_space(space::XXS);
}

/// Columns of the figure strip for `n` figures in `avail` points whose widest number takes
/// `widest`: as many equal columns as keep every number on one line (at least one).
pub fn figure_cols(n: usize, avail: f32, widest: f32, gap: f32) -> usize {
    let fits = |c: usize| (avail - gap * (c as f32 - 1.0)) / c as f32 >= widest;
    (1..=n.max(1)).rev().find(|c| fits(*c)).unwrap_or(1)
}

/// Compact read-only strip: caption over value on a grid of equal columns. A number never
/// breaks: columns drop to fewer per row until the widest number fits, and a number wider
/// than the whole strip takes the caption size. Captions wrap between words, never cut.
pub fn figures(ui: &mut egui::Ui, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }
    let th = theme::of(ui.ctx());
    ui.add_space(space::SM);
    egui::Frame::new()
        .fill(th.c("bg.sunken"))
        .corner_radius(th.radius("md"))
        .inner_margin(egui::Margin::same(space::SM as i8))
        .show(ui, |ui| {
            let avail = ui.available_width();
            ui.set_min_width(avail);
            let gap = space::LG;
            let (cap_col, val_col) = (th.c("text.secondary"), th.c("text.primary"));
            let painter = ui.painter().clone();
            let line = |text: &str, role: Role| {
                painter.layout_no_wrap(text.to_owned(), th.font(role), val_col)
            };
            let widest = items
                .iter()
                .map(|(_, v)| line(v, Role::Numeric).size().x)
                .fold(0.0, f32::max);
            let cols = figure_cols(items.len(), avail, widest, gap);
            let col_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(1.0);
            for (row, chunk) in items.chunks(cols).enumerate() {
                if row > 0 {
                    ui.add_space(space::SM);
                }
                let caps: Vec<_> = chunk
                    .iter()
                    .map(|(k, _)| {
                        let font = th.font(Role::Caption);
                        ui.painter().layout(k.clone(), font, cap_col, col_w)
                    })
                    .collect();
                let vals: Vec<_> = chunk
                    .iter()
                    .map(|(_, v)| {
                        let g = line(v, Role::Numeric);
                        if g.size().x <= col_w {
                            return g;
                        }
                        let small = line(v, Role::Caption);
                        if small.size().x <= col_w {
                            return small;
                        }
                        widgets::elide(ui, v, th.font(Role::Caption), val_col, col_w)
                    })
                    .collect();
                let cap_h = caps.iter().map(|g| g.size().y).fold(0.0, f32::max);
                let val_h = vals.iter().map(|g| g.size().y).fold(0.0, f32::max);
                let size = egui::vec2(avail, cap_h + space::XXS + val_h);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                for (i, (cap, val)) in caps.into_iter().zip(vals).enumerate() {
                    let x = rect.left() + i as f32 * (col_w + gap);
                    let p = ui.painter();
                    p.galley(egui::pos2(x, rect.top()), cap, cap_col);
                    let y = rect.top() + cap_h + space::XXS + (val_h - val.size().y) / 2.0;
                    let cut = val.elided;
                    p.galley(egui::pos2(x, y), val, val_col);
                    if cut {
                        // wider than the strip even small: the whole number on hover
                        let size = egui::vec2(col_w, val_h);
                        let r = egui::Rect::from_min_size(egui::pos2(x, y), size);
                        let id = ui.id().with(("ddc.figure", row, i));
                        let full = chunk[i].1.clone();
                        ui.interact(r, id, egui::Sense::hover()).on_hover_text(full);
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::figure_cols;

    #[test]
    fn figure_columns_drop_until_the_widest_number_fits() {
        assert_eq!(figure_cols(3, 260.0, 60.0, 16.0), 3);
        assert_eq!(figure_cols(3, 160.0, 60.0, 16.0), 2);
        assert_eq!(figure_cols(3, 50.0, 60.0, 16.0), 1);
        assert_eq!(figure_cols(1, 300.0, 60.0, 16.0), 1);
        assert_eq!(figure_cols(0, 300.0, 60.0, 16.0), 1);
    }
}
