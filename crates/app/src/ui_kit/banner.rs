//! Banner surface, middle-elided path label and selectable file row: the pieces of the
//! "source changed" strip and the relink card.
use ddc_ui::theme::{self, space, stroke, Role};
use ddc_ui::widgets::{self, Tone};
use ddc_ui::Theme;
use egui::{self, Color32, Margin, Response, Sense, Stroke, Ui};

/// `text` cut in the middle ("C:\Projects\…\Book.xlsx") until `fits` accepts it; the tail
/// (the file name) keeps the larger share. `fits` must be monotone in the length.
pub fn mid_ellipsis(text: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |keep: usize| -> String {
        // the tail is the file name: two thirds of the kept text
        let head = keep / 3;
        let tail = keep - head;
        let mut s: String = chars[..head].iter().collect();
        s.push('…');
        s.extend(chars[chars.len() - tail..].iter());
        s
    };
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&build(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}

/// One line of a path in the available width, cut in the middle; the whole path on hover.
pub fn path_label(ui: &mut Ui, full: &str, color: Color32) -> Response {
    let th = theme::of(ui.ctx());
    let font = th.font(Role::Body);
    let max_w = ui.available_width();
    let width = |s: &str| {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), color)
            .size()
            .x
    };
    let shown = mid_ellipsis(full, |s| width(s) <= max_w);
    let g = ui.painter().layout_no_wrap(shown, font, color);
    let (rect, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(rect.min, g, color);
    resp.on_hover_text(full)
}

/// Icon of a banner tone: the shape tells the level, the colour only backs it up.
pub fn banner_icon(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral | Tone::Accent => "level-info",
        Tone::Success => "level-success",
        Tone::Warning => "warning",
        Tone::Danger => "level-error",
    }
}

/// Tinted strip for a state the user may act on; the buttons go inside. A 20 px level icon
/// on the left, at least one control tall.
pub fn banner<R>(
    ui: &mut Ui,
    th: &Theme,
    tone: Tone,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    banner_closable(ui, th, tone, false, add).0
}

/// `banner` with an optional close cross on the right; true in the second field once clicked.
pub fn banner_closable<R>(
    ui: &mut Ui,
    th: &Theme,
    tone: Tone,
    closable: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> (egui::InnerResponse<R>, bool) {
    let s = th.sem();
    let mut closed = false;
    let out = egui::Frame::new()
        .fill(tone.background(s))
        .stroke(Stroke::new(stroke::HAIRLINE, tone.foreground(s)))
        .corner_radius(th.radius("lg"))
        .inner_margin(Margin::symmetric(space::MD as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_height(th.d().control);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(
                    egui::Vec2::splat(theme::icon_size::SM),
                    Sense::hover(),
                );
                let col = tone.foreground(s);
                let ctx = ui.ctx().clone();
                widgets::with_icons(&ctx, |icons| {
                    icons.paint(ui, banner_icon(tone), theme::icon_size::SM as u32, r, col)
                });
                let inner = add(ui);
                if closable {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let tip = crate::tr("dlg.close", &[]);
                        let b = widgets::with_icons(&ctx, |icons| {
                            widgets::icon_button(ui, icons, th, "close", false, &tip)
                        });
                        closed = b.clicked();
                    });
                }
                inner
            })
            .inner
        });
    (out, closed)
}

/// Selectable row: file name and, muted below it, its folder cut in the middle. The tooltip
/// holds the full path. Clicking it is the choice.
pub fn file_row(ui: &mut Ui, th: &Theme, selected: bool, name: &str, folder: &str, full: &str) -> Response {
    let s = th.sem();
    widgets::card_button(ui, th, selected, |ui| {
        ui.set_width(ui.available_width());
        ui.label(th.rich(Role::BodyStrong, name).color(s.text_primary));
        path_label(ui, folder, s.text_secondary);
    })
    .on_hover_text(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_has_icon_per_tone_and_is_a_control_tall() {
        assert_eq!(banner_icon(Tone::Warning), "warning");
        assert_eq!(banner_icon(Tone::Danger), "level-error");
        assert_ne!(banner_icon(Tone::Warning), banner_icon(Tone::Danger));
        let ctx = egui::Context::default();
        let mut th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let mut h = 0.0;
        let mut closed = true;
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                th.apply(c);
                egui::CentralPanel::default().show(c, |ui| {
                    let (r, x) = banner_closable(ui, &th, Tone::Danger, true, |ui| {
                        ui.label("x");
                    });
                    h = r.response.rect.height();
                    closed = x;
                });
            });
        }
        assert!(h >= th.d().control, "{h}");
        assert!(!closed);
    }

    #[test]
    fn short_text_is_kept() {
        assert_eq!(mid_ellipsis("a.xlsx", |s| s.chars().count() <= 20), "a.xlsx");
    }

    #[test]
    fn long_path_keeps_both_ends_within_the_limit() {
        let p = "C:/Projects/Very/Long/Folder/Structure/Tower-B/Book-final.xlsx";
        let s = mid_ellipsis(p, |s| s.chars().count() <= 24);
        assert_eq!(s.chars().count(), 24);
        assert!(s.starts_with("C:/"));
        assert!(s.ends_with("Book-final.xlsx"));
        assert!(s.contains('…'));
    }

    #[test]
    fn long_path_label_fits_the_panel_width() {
        let ctx = egui::Context::default();
        let mut th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                th.apply(c);
            });
        }
        let long = "D:/Projects/Very Long Customer Name/Tower B/Structure/Revision 14/Exports/Spreadsheets/Archive/Old";
        for w in [160.0_f32, 260.0, 420.0] {
            let mut got = 0.0;
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    // a panel ui cannot shrink below its full width: give the label a w-wide box
                    let area = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(w, 100.0));
                    let col = th.sem().text_primary;
                    got = ui
                        .scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                            path_label(ui, long, col).rect.width()
                        })
                        .inner;
                });
            });
            assert!(got > 0.0 && got <= w + 0.5, "{got} in {w}");
        }
    }

    #[test]
    fn nothing_fits_gives_only_the_mark() {
        assert_eq!(mid_ellipsis("abcdef", |s| s.chars().count() <= 1), "…");
    }
}
