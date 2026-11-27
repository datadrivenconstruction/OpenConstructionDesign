// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Buttons: one primary per dialog, secondary and quiet for the rest, danger never default.
use super::text::paint_in;
use crate::icons::IconCache;
use crate::theme::{control, icon_size, space, stroke, ControlSize, Role, Sem};
use crate::Theme;
use egui::{Align, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Quiet,
    /// Filled danger: confirmation dialogs only.
    Danger,
    /// Text-only danger: the form allowed in bars.
    DangerText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visual {
    Rest,
    Hover,
    Pressed,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonColors {
    pub fill: Color32,
    pub stroke: Color32,
    pub text: Color32,
}

/// Resolved colours of a button state. A resting secondary button is a field-like surface
/// (`bg.input` + `border.control`); hover and pressed fills stay translucent, so they read on
/// any surface. Disabled is flat `bg.sunken` + `text.disabled`, no border and no extra fade.
pub fn colors(s: &Sem, v: Variant, st: Visual) -> ButtonColors {
    let (none, hover, press) = (Color32::TRANSPARENT, s.bg_hover, s.bg_pressed);
    let solid = |fill, text| ButtonColors {
        fill,
        stroke: none,
        text,
    };
    match (v, st) {
        (Variant::Primary | Variant::Danger | Variant::Secondary, Visual::Disabled) => {
            solid(s.bg_sunken, s.text_disabled)
        }
        (Variant::DangerText, Visual::Rest) => solid(none, s.error),
        // over the hover and pressed fills the filled danger shades keep AA
        (Variant::DangerText, Visual::Hover) => solid(hover, s.danger_hover),
        (Variant::DangerText, Visual::Pressed) => solid(press, s.danger_pressed),
        (Variant::DangerText, Visual::Disabled) => solid(none, s.text_disabled),
        (Variant::Primary, Visual::Rest) => solid(s.accent, s.accent_text_on),
        (Variant::Primary, Visual::Hover) => solid(s.accent_hover, s.accent_text_on),
        (Variant::Primary, Visual::Pressed) => solid(s.accent_pressed, s.accent_text_on),
        (Variant::Danger, Visual::Rest) => solid(s.danger, s.danger_text_on),
        (Variant::Danger, Visual::Hover) => solid(s.danger_hover, s.danger_text_on),
        (Variant::Danger, Visual::Pressed) => solid(s.danger_pressed, s.danger_text_on),
        (Variant::Secondary, st) => ButtonColors {
            fill: match st {
                Visual::Hover => hover,
                Visual::Pressed => press,
                // one raised surface with egui buttons and segmented thumbs (polish_tests)
                Visual::Rest | Visual::Disabled => s.bg_elevated,
            },
            stroke: s.border_control,
            text: s.text_primary,
        },
        (Variant::Quiet, Visual::Rest) => solid(none, s.text_secondary),
        (Variant::Quiet, Visual::Hover) => solid(hover, s.text_primary),
        (Variant::Quiet, Visual::Pressed) => solid(press, s.text_primary),
        (Variant::Quiet, Visual::Disabled) => solid(none, s.text_disabled),
    }
}

/// Side padding of a text button: 8 / 12 / 16 for small / medium / large.
pub fn pad_x(size: ControlSize) -> f32 {
    match size {
        ControlSize::Small => space::SM,
        ControlSize::Medium => space::MD,
        ControlSize::Large => space::LG,
    }
}

/// Corner radius token of a text button: `sm` (4) when small, `md` (8) otherwise.
fn corner_token(size: ControlSize) -> &'static str {
    if size == ControlSize::Small {
        "sm"
    } else {
        "md"
    }
}

/// Icon-button box: 20 / 24 / 32 visual square. The pointer target is never below `HIT_MIN`.
pub fn icon_button_box(size: ControlSize) -> f32 {
    match size {
        ControlSize::Small => 20.0,
        ControlSize::Medium => 24.0,
        ControlSize::Large => 32.0,
    }
}

/// Glyph size inside an icon button: 16, 20 when large.
pub fn icon_button_glyph(size: ControlSize) -> f32 {
    if size == ControlSize::Large {
        icon_size::MD
    } else {
        icon_size::SM
    }
}

/// Square icon-only button: quiet states, `selected` = `bg.selected` + accent glyph. A tooltip
/// (title and optional keys) is mandatory, so no icon button is unnamed.
pub struct IconButton<'a> {
    icon: &'a str,
    tip: &'a str,
    keys: &'a str,
    size: ControlSize,
    selected: bool,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: &'a str, tip: &'a str) -> Self {
        Self {
            icon,
            tip,
            keys: "",
            size: ControlSize::Medium,
            selected: false,
        }
    }
    pub fn keys(mut self, keys: &'a str) -> Self {
        self.keys = keys;
        self
    }
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    pub fn show(self, ui: &mut Ui, th: &Theme, icons: &mut IconCache) -> Response {
        let vis = icon_button_box(self.size);
        let hit = vis.max(control::HIT_MIN);
        let (target, resp) = ui.allocate_exact_size(Vec2::splat(hit), Sense::click());
        let enabled = ui.is_enabled();
        let tip = self.tip;
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
        if ui.is_rect_visible(target) {
            let s = th.sem();
            let rect = Rect::from_center_size(target.center(), Vec2::splat(vis));
            let (fill, ink) = if !enabled {
                (Color32::TRANSPARENT, s.text_disabled)
            } else if resp.is_pointer_button_down_on() {
                (s.bg_pressed, s.text_primary)
            } else {
                let t = ui.ctx().animate_bool_with_time(
                    resp.id.with("ddc.hover"),
                    resp.hovered(),
                    ui.style().animation_time,
                );
                let rest = if self.selected { s.bg_selected } else { Color32::TRANSPARENT };
                let hov = if self.selected { s.bg_selected_hover } else { s.bg_hover };
                let ink = if self.selected { s.accent } else { s.text_secondary };
                (rest.lerp_to_gamma(hov, t), if resp.hovered() && !self.selected { s.text_primary } else { ink })
            };
            ui.painter().rect_filled(rect, th.radius("sm"), fill);
            let g = icon_button_glyph(self.size);
            let ic = Rect::from_center_size(rect.center(), Vec2::splat(g));
            icons.paint(ui, self.icon, g as u32, ic, ink);
        }
        super::rich_tooltip(resp, th, self.tip, "", self.keys)
    }
}

/// Room a leading icon takes in a text button: the glyph and its gap to the label.
const ICON_W: f32 = icon_size::SM + space::SM;

pub struct Button<'a> {
    label: &'a str,
    variant: Variant,
    size: ControlSize,
    min_w: f32,
    full_width: bool,
    desc: &'a str,
    keys: &'a str,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str, variant: Variant) -> Self {
        Self {
            label,
            variant,
            size: ControlSize::Medium,
            min_w: if matches!(variant, Variant::Quiet | Variant::DangerText) {
                0.0
            } else {
                control::BUTTON_MIN_W
            },
            full_width: false,
            desc: "",
            keys: "",
        }
    }
    pub fn primary(label: &'a str) -> Self {
        Self::new(label, Variant::Primary)
    }
    pub fn secondary(label: &'a str) -> Self {
        Self::new(label, Variant::Secondary)
    }
    pub fn quiet(label: &'a str) -> Self {
        Self::new(label, Variant::Quiet)
    }
    pub fn danger(label: &'a str) -> Self {
        Self::new(label, Variant::Danger)
    }
    pub fn danger_text(label: &'a str) -> Self {
        Self::new(label, Variant::DangerText)
    }
    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }
    pub fn small(self) -> Self {
        self.size(ControlSize::Small)
    }
    pub fn large(self) -> Self {
        self.size(ControlSize::Large)
    }
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_w = w;
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    /// Tooltip body and shortcut («Ctrl+K»); the title is the label.
    pub fn tooltip(mut self, desc: &'a str, keys: &'a str) -> Self {
        self.desc = desc;
        self.keys = keys;
        self
    }

    pub fn show(self, ui: &mut Ui, th: &Theme) -> Response {
        self.paint(ui, th, None)
    }

    /// Width of the text button with its whole label: a wrapping row starts a new line when
    /// less is left, instead of cutting the label.
    pub fn width(&self, ui: &Ui, th: &Theme) -> f32 {
        let (text_w, pad) = self.text_w(ui, th);
        (text_w + 2.0 * pad).max(self.min_w)
    }

    /// Width of the button with a leading icon, as `show_icon` paints it.
    pub fn icon_width(&self, ui: &Ui, th: &Theme) -> f32 {
        let (text_w, pad) = self.text_w(ui, th);
        (text_w + ICON_W + 2.0 * pad).max(self.min_w)
    }

    /// The label's width on one line and the side padding.
    fn text_w(&self, ui: &Ui, th: &Theme) -> (f32, f32) {
        let font = th.font(Role::Label);
        let ink = th.sem().text_primary;
        let galley = ui.painter().layout_no_wrap(crate::rtl::display(self.label).into_owned(), font, ink);
        // whole pixels: no fractional edges, no width jump from rounding at paint time
        (galley.size().x.ceil(), pad_x(self.size))
    }

    /// Button with a leading icon.
    pub fn show_icon(self, ui: &mut Ui, th: &Theme, icons: &mut IconCache, icon: &str) -> Response {
        self.paint(ui, th, Some((icons, icon)))
    }

    /// For a caller that draws the button inside a child Ui (a disabled scope): the child starts
    /// a fresh row and cannot move its parent on, so the parent breaks the row first.
    pub fn break_row(&self, ui: &mut Ui, th: &Theme, icon: bool) {
        // measured as paint measures it, or a row keeps a button that then runs past its end
        let want = if icon { self.icon_width(ui, th) } else { self.width(ui, th) };
        self.break_row_for(ui, want);
    }

    /// A button that does not fit the rest of a wrapping row's line goes to the next line
    /// instead of being squeezed. The Ui reports the whole width as available, so the rest of
    /// the line is measured from the cursor.
    fn break_row_for(&self, ui: &mut Ui, want: f32) {
        if !ui.layout().main_wrap || self.full_width || !ui.layout().is_horizontal() {
            return;
        }
        if want.max(self.min_w) <= ui.available_rect_before_wrap().width() {
            return;
        }
        let (cur, max) = (ui.cursor(), ui.max_rect());
        let started = if ui.layout().prefer_right_to_left() {
            cur.right() < max.right() - 0.5
        } else {
            cur.left() > max.left() + 0.5
        };
        if started {
            ui.end_row();
        }
    }

    fn paint(self, ui: &mut Ui, th: &Theme, icon: Option<(&mut IconCache, &str)>) -> Response {
        let font = th.font(Role::Label);
        let h = th.control_h(self.size);
        let (text_w, pad) = self.text_w(ui, th);
        let icon_w = if icon.is_some() { ICON_W } else { 0.0 };
        let want = text_w + icon_w + 2.0 * pad;
        // a wrapping row hands a button that does not fit the rest of the line to the next one
        // instead of squeezing it
        self.break_row_for(ui, want);
        let avail = if ui.layout().main_wrap {
            ui.available_rect_before_wrap().width()
        } else {
            ui.available_width()
        };
        let w = if self.full_width {
            avail
        } else {
            want.max(self.min_w).min(avail.max(self.min_w))
        };
        // a label longer than a whole line of a wrapping row takes more lines, never an ellipsis
        let lines = (ui.layout().main_wrap && !self.full_width && want > avail && avail > 40.0)
            .then(|| {
                let wrap = (avail - icon_w - 2.0 * pad).max(20.0);
                ui.painter().layout(
                    self.label.to_owned(),
                    font.clone(),
                    Color32::PLACEHOLDER,
                    wrap,
                )
            });
        let h = lines.as_ref().map_or(h, |g| h.max(g.size().y + 2.0 * space::SM));
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
        let enabled = ui.is_enabled();
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, self.label)
        });
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let s = th.sem();
        let c = if !enabled {
            colors(s, self.variant, Visual::Disabled)
        } else if resp.is_pointer_button_down_on() {
            colors(s, self.variant, Visual::Pressed)
        } else {
            let t = ui.ctx().animate_bool_with_time(
                resp.id.with("ddc.hover"),
                resp.hovered(),
                ui.style().animation_time,
            );
            let (a, b) = (
                colors(s, self.variant, Visual::Rest),
                colors(s, self.variant, Visual::Hover),
            );
            ButtonColors {
                fill: a.fill.lerp_to_gamma(b.fill, t),
                stroke: a.stroke.lerp_to_gamma(b.stroke, t),
                text: a.text.lerp_to_gamma(b.text, t),
            }
        };
        let corner = th.radius(corner_token(self.size));
        let line = if c.stroke == Color32::TRANSPARENT {
            Stroke::NONE
        } else {
            Stroke::new(stroke::CONTROL, c.stroke)
        };
        ui.painter()
            .rect(rect, corner, c.fill, line, StrokeKind::Inside);
        if let Some(g) = lines {
            let at = egui::pos2(rect.left() + pad + icon_w, rect.center().y - g.size().y / 2.0);
            if let Some((cache, name)) = icon {
                let ic = Rect::from_min_size(
                    egui::pos2(rect.left() + pad, rect.center().y - icon_size::SM / 2.0),
                    Vec2::splat(icon_size::SM),
                );
                cache.paint(ui, name, icon_size::SM as u32, ic, c.text);
            }
            ui.painter().galley(at, g, c.text);
            return resp;
        }
        let room = (w - icon_w - 2.0 * pad).max(1.0);
        let content_w = text_w.min(room) + icon_w;
        let x0 = (rect.center().x - content_w / 2.0).round();
        if let Some((cache, name)) = icon {
            let ic = Rect::from_center_size(
                egui::pos2(x0 + icon_size::SM / 2.0, rect.center().y),
                Vec2::splat(icon_size::SM),
            );
            cache.paint(ui, name, icon_size::SM as u32, ic, c.text);
        }
        let tr = Rect::from_min_size(
            egui::pos2(x0 + icon_w, rect.top()),
            Vec2::new(text_w.min(room), h),
        );
        let cut = paint_in(ui, tr, self.label, font, c.text, Align::Min);
        if cut || !self.desc.is_empty() || !self.keys.is_empty() {
            super::rich_tooltip(resp, th, self.label, self.desc, self.keys)
        } else {
            resp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, over_rgba, sem, Density, Mode};

    const ALL: [Variant; 5] = [
        Variant::Primary,
        Variant::Secondary,
        Variant::Quiet,
        Variant::Danger,
        Variant::DangerText,
    ];

    #[test]
    fn heights_on_grid_and_padding_steps() {
        for mode in [Mode::Light, Mode::Dark] {
            let th = Theme::new(mode, Density::Compact);
            let hs: Vec<f32> = [ControlSize::Small, ControlSize::Medium, ControlSize::Large]
                .iter()
                .map(|s| th.control_h(*s))
                .collect();
            // heights come from the density block of tokens.json, the one source the code reads
            let v: serde_json::Value = serde_json::from_str(crate::theme::TOKENS_JSON).unwrap();
            let d = &v["density"]["compact"];
            let want: Vec<f32> = ["control_sm", "control", "control_lg"]
                .iter()
                .map(|k| d[*k].as_f64().unwrap() as f32)
                .collect();
            assert_eq!(hs, want);
            assert!(hs.windows(2).all(|w| w[0] < w[1]), "{hs:?}");
            assert!(hs.iter().all(|h| h % 4.0 == 0.0), "on the 4 px grid: {hs:?}");
        }
        assert_eq!(pad_x(ControlSize::Small), 8.0);
        assert_eq!(pad_x(ControlSize::Medium), 12.0);
        assert_eq!(pad_x(ControlSize::Large), 16.0);
        assert_eq!(control::BUTTON_MIN_W % 4.0, 0.0);
    }

    #[test]
    fn radii_sm_4_md_8() {
        let th = Theme::new(Mode::Light, Density::Compact);
        assert_eq!(th.radius(corner_token(ControlSize::Small)), 4.0);
        assert_eq!(th.radius(corner_token(ControlSize::Medium)), 8.0);
        assert_eq!(th.radius(corner_token(ControlSize::Large)), 8.0);
    }

    #[test]
    fn filled_text_reads_in_every_state() {
        for dark in [false, true] {
            let s = sem(dark);
            for v in ALL {
                for st in [Visual::Rest, Visual::Hover, Visual::Pressed] {
                    let c = colors(s, v, st);
                    for surface in [s.bg_panel, s.bg_elevated] {
                        let r = contrast(c.text, over_rgba(c.fill, surface));
                        assert!(r >= 4.5, "dark={dark} {v:?} {st:?}: {r:.2}");
                    }
                }
            }
        }
    }

    #[test]
    fn disabled_is_flat_tokens_without_border() {
        for dark in [false, true] {
            let s = sem(dark);
            for v in ALL {
                let c = colors(s, v, Visual::Disabled);
                assert_eq!(c.text, s.text_disabled, "dark={dark} {v:?}");
                assert_eq!(c.stroke, Color32::TRANSPARENT, "dark={dark} {v:?}");
                if !matches!(v, Variant::Quiet | Variant::DangerText) {
                    assert_eq!(c.fill, s.bg_sunken, "dark={dark} {v:?}");
                }
                let r = contrast(c.text, over_rgba(c.fill, s.bg_panel));
                assert!(r >= 3.0, "dark={dark} {v:?} disabled: {r:.2}");
            }
        }
    }

    #[test]
    fn secondary_edge_is_the_control_border() {
        for dark in [false, true] {
            let s = sem(dark);
            for st in [Visual::Rest, Visual::Hover, Visual::Pressed] {
                assert_eq!(colors(s, Variant::Secondary, st).stroke, s.border_control);
            }
            assert!(contrast(s.border_control, s.bg_panel) >= 3.0, "dark={dark}");
        }
    }

    #[test]
    fn states_are_distinct_and_pressed_does_not_move() {
        for dark in [false, true] {
            let s = sem(dark);
            for v in ALL {
                let (r, h, p) = (
                    colors(s, v, Visual::Rest),
                    colors(s, v, Visual::Hover),
                    colors(s, v, Visual::Pressed),
                );
                assert!(r.fill != h.fill && h.fill != p.fill, "dark={dark} {v:?}");
            }
        }
    }

    #[test]
    fn icon_button_hit_target_at_least_24() {
        for size in [ControlSize::Small, ControlSize::Medium, ControlSize::Large] {
            let vis = icon_button_box(size);
            assert!(vis.max(control::HIT_MIN) >= 24.0);
            assert_eq!(vis % 4.0, 0.0);
            assert!(icon_button_glyph(size) < vis);
        }
        assert_eq!(icon_button_glyph(ControlSize::Medium), 16.0);
        assert_eq!(icon_button_glyph(ControlSize::Large), 20.0);
    }

    #[test]
    fn danger_never_default_colours_differ_from_primary() {
        let s = sem(false);
        assert!(colors(s, Variant::Danger, Visual::Rest).fill != colors(s, Variant::Primary, Visual::Rest).fill);
        assert_eq!(colors(s, Variant::DangerText, Visual::Rest).fill, Color32::TRANSPARENT);
    }

    /// An icon button paints exactly `icon_width` wide, and `break_row` (the caller drawing it
    /// in a disabled scope) moves it to the next line when the rest of the row is a little short.
    #[test]
    fn icon_button_width_is_what_the_row_measures() {
        let mut th = Theme::new(Mode::Light, Density::Compact);
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let mut icons = IconCache::default();
        let (mut want, mut got, mut edge) = (0.0, Rect::NOTHING, 0.0);
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let _ = th.apply(ctx);
                egui::CentralPanel::default().show(ctx, |ui| {
                    let b = Button::secondary("Go to view").small();
                    want = b.icon_width(ui, &th);
                    // the row is one pixel too short for the button after a filler
                    let row = 60.0 + ui.spacing().item_spacing.x + want - 1.0;
                    ui.allocate_ui(Vec2::new(row, 200.0), |ui| {
                        ui.set_max_width(row);
                        ui.horizontal_wrapped(|ui| {
                            ui.allocate_exact_size(Vec2::new(60.0, 20.0), Sense::hover());
                            edge = ui.max_rect().right();
                            b.break_row(ui, &th, true);
                            let b = Button::secondary("Go to view").small();
                            got = ui
                                .add_enabled_ui(false, |ui| b.show_icon(ui, &th, &mut icons, "view"))
                                .inner
                                .rect;
                        });
                    });
                });
            });
        }
        assert!((got.width() - want).abs() < 0.01, "painted {} measured {want}", got.width());
        assert!(got.right() <= edge + 0.01, "button {got:?} runs past the row end {edge}");
    }
}
