//! Tab bar with an underline indicator, like the ribbon tabs. Left/Right move between tabs.
use super::text::paint_in;
use crate::theme::{control, space, stroke, Role};
use crate::Theme;
use egui::{Align, CornerRadius, EventFilter, Key, Rect, Sense, Stroke, Ui, Vec2};

/// Widths of the tabs: natural, or shrunk evenly when the bar is too narrow (labels elide).
pub fn tab_widths(natural: &[f32], avail: f32) -> Vec<f32> {
    let total: f32 = natural.iter().sum();
    let k = if total > avail && total > 0.0 {
        avail / total
    } else {
        1.0
    };
    natural.iter().map(|w| w * k).collect()
}

/// Underline of the selected tab: as wide as its text (capped by the tab), `INDICATOR` high,
/// flush with the bar's bottom edge.
pub fn indicator_rect(tab: Rect, text_w: f32) -> Rect {
    let w = text_w.clamp(0.0, tab.width());
    let x = tab.center().x - w / 2.0;
    Rect::from_min_size(
        egui::pos2(x, tab.bottom() - stroke::INDICATOR),
        Vec2::new(w, stroke::INDICATOR),
    )
}

/// Room a closable tab keeps right of its text: gap + 16 px close glyph.
pub const CLOSE_SLOT: f32 = space::XS + 16.0;
/// Width of the overflow «»» button.
pub const MORE_W: f32 = 24.0;

#[derive(Debug, Clone, Copy)]
pub struct TabItem<'a> {
    pub label: &'a str,
    pub closable: bool,
    /// Unsaved changes: a dot where the close glyph is, until the tab is hovered.
    pub dirty: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TabsOut {
    pub selected: Option<usize>,
    pub closed: Option<usize>,
}

/// Tabs shown in the strip, in order: all of them when they fit; otherwise as many as fit beside
/// the overflow button, with the selected tab always among them (it replaces the last shown).
pub fn visible_tabs(natural: &[f32], avail: f32, sel: usize) -> Vec<usize> {
    let total: f32 = natural.iter().sum();
    if total <= avail {
        return (0..natural.len()).collect();
    }
    let room = (avail - MORE_W).max(0.0);
    let mut used = 0.0;
    let mut out = Vec::new();
    for (i, w) in natural.iter().enumerate() {
        if used + w > room {
            break;
        }
        used += w;
        out.push(i);
    }
    if sel < natural.len() && !out.contains(&sel) {
        while let Some(l) = out.pop() {
            used -= natural[l];
            if used + natural[sel] <= room {
                break;
            }
        }
        out.push(sel);
    }
    out
}

/// Document-style tab strip: close glyph on hover/selected, dirty dot, overflow menu.
pub fn tab_strip(ui: &mut Ui, th: &Theme, tabs: &[TabItem<'_>], sel: usize) -> TabsOut {
    let s = th.sem();
    let font = th.font(Role::Tab);
    let w = ui.available_width();
    let (bar, bar_resp) = ui.allocate_exact_size(Vec2::new(w, control::TAB_H), Sense::hover());
    let base = bar_resp.id.with("ddc.tab_strip");
    let pad = 2.0 * space::MD;
    let text_w: Vec<f32> = tabs
        .iter()
        .map(|t| ui.painter().layout_no_wrap(t.label.to_owned(), font.clone(), s.text_primary).size().x)
        .collect();
    let natural: Vec<f32> = tabs
        .iter()
        .zip(&text_w)
        .map(|(t, tw)| tw + pad + if t.closable { CLOSE_SLOT } else { 0.0 })
        .collect();
    let shown = visible_tabs(&natural, w, sel);
    ui.painter().hline(
        bar.x_range(),
        bar.bottom() - stroke::HAIRLINE / 2.0,
        Stroke::new(stroke::HAIRLINE, s.border_subtle),
    );
    let mut out = TabsOut::default();
    let mut x = bar.left();
    for &i in &shown {
        let t = tabs[i];
        let r = Rect::from_min_size(egui::pos2(x, bar.top()), Vec2::new(natural[i], bar.height()));
        x += natural[i];
        let on = i == sel;
        let resp = ui.interact(r, base.with(i), Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, t.label)
        });
        let hov = resp.hovered();
        let col = if on || hov { s.text_primary } else { s.text_secondary };
        let slot = if t.closable { CLOSE_SLOT } else { 0.0 };
        let inner = Rect::from_min_max(
            egui::pos2(r.left() + space::MD, r.top()),
            egui::pos2(r.right() - space::MD - slot, r.bottom()),
        );
        paint_in(ui, inner, t.label, font.clone(), col, Align::Center);
        let mut closed_here = false;
        if t.closable {
            let g = Rect::from_center_size(
                egui::pos2(r.right() - space::MD - 8.0, r.center().y),
                Vec2::splat(control::HIT_MIN),
            );
            let cr = ui.interact(g, base.with((i, "x")), Sense::click());
            if t.dirty && !hov && !cr.hovered() {
                ui.painter().circle_filled(g.center(), 4.0, s.text_secondary);
            } else if on || hov || cr.hovered() {
                if cr.hovered() {
                    ui.painter().rect_filled(g.shrink(space::XXS), th.radius("sm"), s.bg_hover);
                }
                let ir = Rect::from_center_size(g.center(), Vec2::splat(16.0));
                crate::diff_kind::paint_icon(ui, "close-tab", ir, col);
            }
            if cr.clicked() {
                out.closed = Some(i);
                closed_here = true;
            }
        }
        if resp.clicked() && !closed_here && !on {
            out.selected = Some(i);
        }
        if on {
            ui.painter().rect_filled(
                indicator_rect(r, text_w[i].min(r.width())),
                CornerRadius::same(1),
                s.accent,
            );
        }
    }
    if shown.len() < tabs.len() {
        let r = Rect::from_min_size(
            egui::pos2(x, bar.top() + space::XS),
            Vec2::new(MORE_W, bar.height() - 2.0 * space::XS),
        );
        let resp = ui.interact(r, base.with("more"), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, th.radius("sm"), s.bg_hover);
        }
        crate::diff_kind::paint_icon(
            ui,
            crate::icons::ctl::CHEVRON_DOWN,
            Rect::from_center_size(r.center(), Vec2::splat(16.0)),
            s.text_secondary,
        );
        egui::Popup::menu(&resp).show(|ui| {
            for (i, t) in tabs.iter().enumerate().filter(|(i, _)| !shown.contains(i)) {
                let label = if t.dirty { format!("{} \u{2022}", t.label) } else { t.label.to_owned() };
                if ui.button(label).clicked() {
                    out.selected = Some(i);
                    ui.close();
                }
            }
        });
    }
    out
}

/// Draws the tabs; `sel` follows clicks and arrows. Returns the new index if it changed.
pub fn tab_bar(ui: &mut Ui, th: &Theme, tabs: &[&str], sel: &mut usize) -> Option<usize> {
    tab_bar_gated(ui, th, tabs, &[], sel)
}

/// Next tab from `i` by `step` (+1/-1) that `blocked` leaves open.
pub fn next_open(blocked: &[Option<&str>], len: usize, i: usize, step: isize) -> Option<usize> {
    let mut j = i as isize + step;
    while j >= 0 && (j as usize) < len {
        if blocked.get(j as usize).is_none_or(|b| b.is_none()) {
            return Some(j as usize);
        }
        j += step;
    }
    None
}

/// As `tab_bar`; a tab with `Some(reason)` in `blocked` is muted, not clickable or reachable
/// by arrows, and names the reason on hover. A missing entry means open.
pub fn tab_bar_gated(
    ui: &mut Ui,
    th: &Theme,
    tabs: &[&str],
    blocked: &[Option<&str>],
    sel: &mut usize,
) -> Option<usize> {
    let s = th.sem();
    let font = th.font(Role::Tab);
    let w = ui.available_width();
    let (bar, bar_resp) = ui.allocate_exact_size(Vec2::new(w, control::TAB_H), Sense::hover());
    let base = bar_resp.id.with("ddc.tabs");
    let pad = 2.0 * space::MD;
    let natural: Vec<f32> = tabs
        .iter()
        .map(|t| {
            ui.painter()
                .layout_no_wrap((*t).to_owned(), font.clone(), s.text_primary)
                .size()
                .x
                + pad
        })
        .collect();
    let widths = tab_widths(&natural, w);
    ui.painter().hline(
        bar.x_range(),
        bar.bottom() - stroke::HAIRLINE / 2.0,
        Stroke::new(stroke::HAIRLINE, s.border_subtle),
    );
    let mut out = None;
    let mut x = bar.left();
    for (i, t) in tabs.iter().enumerate() {
        let r = Rect::from_min_size(egui::pos2(x, bar.top()), Vec2::new(widths[i], bar.height()));
        x += widths[i];
        let id = base.with(i);
        let why = blocked.get(i).copied().flatten();
        let sense = if why.is_some() { Sense::hover() } else { Sense::click() };
        let resp = ui.interact(r, id, sense);
        let on = i == *sel;
        let enabled = ui.is_enabled() && why.is_none();
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, on, *t)
        });
        if let Some(why) = why {
            let inner = r.shrink2(Vec2::new(space::MD, 0.0));
            paint_in(ui, inner, t, font.clone(), s.text_disabled, Align::Center);
            resp.on_hover_text(format!("{t}\n{why}"));
            continue;
        }
        if resp.clicked() && !on {
            out = Some(i);
        }
        if resp.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    EventFilter {
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let (left, right) = ui.input(|k| {
                (
                    k.key_pressed(Key::ArrowLeft),
                    k.key_pressed(Key::ArrowRight),
                )
            });
            let to = if left {
                next_open(blocked, tabs.len(), i, -1)
            } else if right {
                next_open(blocked, tabs.len(), i, 1)
            } else {
                None
            };
            if let Some(to) = to {
                ui.memory_mut(|m| m.request_focus(base.with(to)));
                out = Some(to);
            }
        }
        let col = if !enabled {
            s.text_secondary
        } else if on || resp.hovered() {
            s.text_primary
        } else {
            s.text_secondary
        };
        let inner = r.shrink2(Vec2::new(space::MD, 0.0));
        let cut = paint_in(ui, inner, t, font.clone(), col, Align::Center);
        if on {
            let text_w = natural[i] - pad;
            ui.painter().rect_filled(
                indicator_rect(r, text_w),
                CornerRadius::same(1),
                s.accent,
            );
        }
        if cut {
            resp.on_hover_text(*t);
        }
    }
    if let Some(i) = out {
        *sel = i;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicator_is_text_wide_centred_and_flush_with_the_bar() {
        let tab = Rect::from_min_size(egui::pos2(100.0, 0.0), Vec2::new(80.0, 32.0));
        let r = indicator_rect(tab, 56.0);
        assert_eq!(r.width(), 56.0);
        assert_eq!(r.center().x, tab.center().x);
        assert_eq!(r.bottom(), tab.bottom());
        assert_eq!(r.height(), stroke::INDICATOR);
        // a shrunk tab never gets an indicator wider than itself
        assert_eq!(indicator_rect(tab, 200.0).width(), tab.width());
    }

    #[test]
    fn tab_padding_is_the_spec_twelve_each_side() {
        assert_eq!(space::MD, 12.0);
        assert_eq!(tab_widths(&[80.0, 80.0], 100.0), vec![50.0, 50.0]);
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;

    #[test]
    fn all_tabs_show_when_they_fit() {
        assert_eq!(visible_tabs(&[80.0, 80.0], 200.0, 1), vec![0, 1]);
    }

    #[test]
    fn overflow_keeps_the_selected_tab_visible_and_reserves_the_button() {
        let nat = [100.0; 6];
        let v = visible_tabs(&nat, 350.0, 0);
        assert_eq!(v, vec![0, 1, 2]);
        let v = visible_tabs(&nat, 350.0, 5);
        assert!(v.contains(&5));
        let w: f32 = v.iter().map(|&i| nat[i]).sum();
        assert!(w + MORE_W <= 350.0);
    }

    #[test]
    fn close_slot_and_more_button_are_on_the_grid() {
        assert_eq!(CLOSE_SLOT % 4.0, 0.0);
        assert_eq!(MORE_W % 4.0, 0.0);
        assert!(CLOSE_SLOT - space::XS >= 16.0);
    }
}
