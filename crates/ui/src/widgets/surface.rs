// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Card, badge, progress and the designed non-content states: empty, loading, error.
use super::button::Button;
use super::text::paint_in;
use crate::icons::IconCache;
use crate::notify::Level;
use crate::theme::{control, icon_size, layout, over_rgba, space, stroke, Role, Sem};
use crate::Theme;
use egui::{
    Align, Color32, InnerResponse, Margin, Rect, Response, RichText, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};

/// Meaning of a status mark: same mapping for badges, toasts and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub fn of_level(l: Level) -> Self {
        match l {
            Level::Info => Tone::Accent,
            Level::Success => Tone::Success,
            Level::Warning => Tone::Warning,
            Level::Error => Tone::Danger,
        }
    }
    pub fn foreground(self, s: &Sem) -> Color32 {
        match self {
            Tone::Neutral => s.text_secondary,
            Tone::Accent => s.accent,
            Tone::Success => s.success,
            Tone::Warning => s.warning,
            Tone::Danger => s.error,
        }
    }
    pub fn background(self, s: &Sem) -> Color32 {
        match self {
            Tone::Neutral => s.neutral_subtle,
            Tone::Accent => s.accent_subtle,
            Tone::Success => s.success_subtle,
            Tone::Warning => s.warning_subtle,
            Tone::Danger => s.danger_subtle,
        }
    }
}

/// Small status label.
pub fn badge(ui: &mut Ui, th: &Theme, text: &str, tone: Tone) -> Response {
    let s = th.sem();
    let font = th.font(Role::Caption);
    let fg = tone.foreground(s);
    let tw = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), fg)
        .size()
        .x;
    let w = (tw + 2.0 * space::SM)
        .min(ui.available_width().max(control::BADGE_H))
        .max(control::BADGE_H);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, control::BADGE_H), Sense::hover());
    ui.painter()
        .rect_filled(r, th.radius("sm"), tone.background(s));
    let cut = paint_in(
        ui,
        r.shrink2(Vec2::new(space::XS, 0.0)),
        text,
        font,
        fg,
        Align::Center,
    );
    if cut {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

fn card_frame(th: &Theme) -> egui::Frame {
    egui::Frame::new()
        .corner_radius(th.radius("lg"))
        .inner_margin(Margin::same(space::MD as i8))
}

/// Static surface.
pub fn card<R>(ui: &mut Ui, th: &Theme, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let s = th.sem();
    card_frame(th)
        .fill(s.bg_panel)
        .stroke(Stroke::new(stroke::HAIRLINE, s.border_subtle))
        .show(ui, add)
}

/// Clickable card with hover and selected states. Put no other clickable widgets inside.
pub fn card_button(ui: &mut Ui, th: &Theme, selected: bool, add: impl FnOnce(&mut Ui)) -> Response {
    let s = th.sem();
    let bg = ui.painter().add(Shape::Noop);
    let inner = card_frame(th).show(ui, add);
    let rect = inner.response.rect;
    let resp = ui.interact(rect, inner.response.id.with("ddc.card"), Sense::click());
    let enabled = ui.is_enabled();
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, "")
    });
    let (fill, line) = if selected {
        (s.selection_bg, s.accent)
    } else if resp.hovered() && enabled {
        (over_rgba(s.bg_hover, s.bg_panel), s.border_strong)
    } else {
        (s.bg_panel, s.border_subtle)
    };
    ui.painter().set(
        bg,
        egui::epaint::RectShape::new(
            rect,
            th.radius("lg"),
            fill,
            Stroke::new(stroke::HAIRLINE, line),
            StrokeKind::Inside,
        ),
    );
    resp
}

fn centered_state(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    centered_pad(ui, space::XL, add);
}

fn centered_pad(ui: &mut Ui, pad: f32, add: impl FnOnce(&mut Ui)) {
    ui.vertical_centered(|ui| {
        ui.set_max_width(layout::EMPTY_W.min(ui.available_width()));
        ui.add_space(pad);
        add(ui);
        ui.add_space(pad);
    });
}

/// Empty state for a panel whose form follows right below: title and hint, no icon or action,
/// small padding (about 60 px tall).
pub fn empty_state_compact(ui: &mut Ui, th: &Theme, title: &str, hint: &str) {
    let s = th.sem();
    centered_pad(ui, space::SM, |ui| {
        ui.label(
            RichText::new(title)
                .font(th.font(Role::BodyStrong))
                .color(s.text_primary),
        );
        if !hint.is_empty() {
            ui.add_space(space::XS);
            ui.label(
                RichText::new(hint)
                    .font(th.font(Role::Body))
                    .color(s.text_secondary),
            );
        }
    });
}

/// Empty state: what is missing and what to do; returns true when the action was clicked.
pub fn empty_state(
    ui: &mut Ui,
    th: &Theme,
    icon: Option<(&mut IconCache, &str)>,
    title: &str,
    hint: &str,
    action: Option<&str>,
) -> bool {
    let s = th.sem();
    let mut clicked = false;
    let top = empty_top_pad(
        ui.available_height(),
        empty_content_h(icon.is_some(), !hint.is_empty(), action.is_some()),
    );
    centered_pad_top(ui, top, space::XL, |ui| {
        if let Some((cache, name)) = icon {
            // an illustration: the large direction (duotone), ink plus the accent surface
            let (r, _) = ui.allocate_exact_size(Vec2::splat(icon_size::XL), Sense::hover());
            cache.paint(ui, name, icon_size::XL as u32, r, s.text_secondary);
            ui.add_space(space::SM);
        }
        ui.label(
            RichText::new(title)
                .font(th.font(Role::BodyStrong))
                .color(s.text_primary),
        );
        if !hint.is_empty() {
            ui.add_space(space::XS);
            ui.label(
                RichText::new(hint)
                    .font(th.font(Role::Body))
                    .color(s.text_secondary),
            );
        }
        if let Some(a) = action {
            ui.add_space(space::MD);
            clicked = Button::primary(a).show(ui, th).clicked();
        }
    });
    clicked
}

/// Optical centre of an empty state: 40 % of the height, not 50 %.
pub const EMPTY_CENTER: f32 = 0.4;

/// Estimated height of the empty-state block (icon 32 + gap, title, two hint lines, button).
pub fn empty_content_h(icon: bool, hint: bool, action: bool) -> f32 {
    let mut h = 20.0;
    if icon {
        h += icon_size::XL + space::SM;
    }
    if hint {
        h += space::XS + 36.0;
    }
    if action {
        h += space::MD + 28.0;
    }
    h
}

/// Space above the block so that its centre sits at `EMPTY_CENTER` of `avail_h`.
pub fn empty_top_pad(avail_h: f32, content_h: f32) -> f32 {
    if !avail_h.is_finite() {
        return space::XL;
    }
    (avail_h * EMPTY_CENTER - content_h / 2.0).round().max(space::XL)
}

fn centered_pad_top(ui: &mut Ui, top: f32, bottom: f32, add: impl FnOnce(&mut Ui)) {
    ui.vertical_centered(|ui| {
        ui.set_max_width(layout::EMPTY_W.min(ui.available_width()));
        ui.add_space(top);
        add(ui);
        ui.add_space(bottom);
    });
}

/// Loading state. With reduced motion the spinner is a still ring.
pub fn loading_state(ui: &mut Ui, th: &Theme, title: &str) {
    let s = th.sem();
    centered_state(ui, |ui| {
        spinner(ui, th);
        ui.add_space(space::SM);
        ui.label(
            RichText::new(title)
                .font(th.font(Role::Body))
                .color(s.text_secondary),
        );
    });
}

/// Spinner appears after this much continuous work, and stays at least `SPINNER_MIN_SHOW_S`.
pub const SPINNER_DELAY_S: f64 = 0.4;
pub const SPINNER_MIN_SHOW_S: f64 = 0.6;

#[derive(Clone, Copy, Default)]
struct SpinGate {
    since: Option<f64>,
    shown_at: Option<f64>,
    last_call: f64,
}

/// Anti-flicker gate: `true` when the spinner should be drawn at `now`. A gap between calls
/// longer than `SPINNER_MIN_SHOW_S` means a new job and restarts the delay.
fn spin_gate(g: &mut SpinGate, now: f64, busy: bool) -> bool {
    if now - g.last_call > SPINNER_MIN_SHOW_S {
        *g = SpinGate::default();
    }
    g.last_call = now;
    if busy {
        let since = *g.since.get_or_insert(now);
        if now - since >= SPINNER_DELAY_S {
            g.shown_at.get_or_insert(now);
        }
    }
    match g.shown_at {
        Some(t) if busy || now - t < SPINNER_MIN_SHOW_S => true,
        Some(_) => {
            *g = SpinGate::default();
            false
        }
        None => {
            if !busy {
                *g = SpinGate::default();
            }
            false
        }
    }
}

/// Activity indicator of icon size, drawn after 400 ms of work and kept for at least 600 ms.
pub fn spinner(ui: &mut Ui, th: &Theme) {
    spinner_busy(ui, th, true)
}

/// Like `spinner`, but `busy = false` lets the minimum display time run out instead of
/// vanishing at once; keep calling it for a few frames after the work ends.
pub fn spinner_busy(ui: &mut Ui, th: &Theme, busy: bool) {
    let s = th.sem();
    let key = ui.next_auto_id();
    let now = ui.input(|i| i.time);
    let (r, _) = ui.allocate_exact_size(Vec2::splat(icon_size::MD), Sense::hover());
    let mut g: SpinGate = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
    let show = spin_gate(&mut g, now, busy);
    ui.ctx().data_mut(|d| d.insert_temp(key, g));
    ui.ctx().request_repaint();
    if !show || !ui.is_rect_visible(r) {
        return;
    }
    let radius = icon_size::MD / 2.0 - stroke::FOCUS;
    let time = now;
    if th.reduced_motion {
        // no rotation: the full ring pulses instead
        let k = pulse(time, SPINNER_PULSE_S);
        ui.painter().circle_stroke(
            r.center(),
            radius,
            Stroke::new(stroke::FOCUS, s.accent.gamma_multiply(0.4 + 0.6 * k)),
        );
    } else {
        let start = (time / SPINNER_TURN_S).fract() as f32 * std::f32::consts::TAU;
        let pts: Vec<egui::Pos2> = (0..=SPINNER_STEPS)
            .map(|i| {
                let a = start + SPINNER_ARC * i as f32 / SPINNER_STEPS as f32;
                r.center() + radius * Vec2::angled(a)
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(pts, Stroke::new(stroke::FOCUS, s.accent)));
    }
}

/// Spinner: 270 degree arc, one turn in 0.8 s, linear.
pub const SPINNER_TURN_S: f64 = 0.8;
/// Reduced-motion spinner: one pulse period.
pub const SPINNER_PULSE_S: f64 = 1.2;
const SPINNER_ARC: f32 = std::f32::consts::TAU * 0.75;
const SPINNER_STEPS: usize = 32;
/// Linear indeterminate progress: one pass of the band.
pub const PROGRESS_PASS_S: f64 = 1.2;

/// 0..1 triangle-free smooth pulse of period `period` seconds.
fn pulse(time: f64, period: f64) -> f32 {
    (0.5 - 0.5 * (time / period * std::f64::consts::TAU).cos()) as f32
}

/// Error state: what happened and what to do; returns true when the action was clicked.
pub fn error_state(
    ui: &mut Ui,
    th: &Theme,
    title: &str,
    detail: &str,
    action: Option<&str>,
) -> bool {
    let s = th.sem();
    let mut clicked = false;
    centered_state(ui, |ui| {
        ui.label(
            RichText::new(title)
                .font(th.font(Role::BodyStrong))
                .color(s.error),
        );
        if !detail.is_empty() {
            ui.add_space(space::XS);
            ui.label(
                RichText::new(detail)
                    .font(th.font(Role::Body))
                    .color(s.text_secondary),
            );
        }
        if let Some(a) = action {
            ui.add_space(space::MD);
            clicked = Button::secondary(a).show(ui, th).clicked();
        }
    });
    clicked
}

/// Progress bar. `None` is indeterminate; with reduced motion it stays still.
pub fn progress(ui: &mut Ui, th: &Theme, fraction: Option<f32>, label: &str) -> Response {
    let s = th.sem();
    let w = ui.available_width();
    let text_h = if label.is_empty() {
        0.0
    } else {
        control::BADGE_H
    };
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(w, text_h + control::PROGRESS_H), Sense::hover());
    let track = Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - control::PROGRESS_H),
        Vec2::new(w, control::PROGRESS_H),
    );
    let corner = control::PROGRESS_H / 2.0;
    let frac = fraction.map(|f| f.clamp(0.0, 1.0));
    if !label.is_empty() {
        let line = Rect::from_min_size(rect.min, Vec2::new(w, text_h));
        let font = th.font(Role::Caption);
        let pct_w = if let Some(f) = frac {
            let pct = format!("{:.0} %", f * 100.0);
            paint_in(ui, line, &pct, font.clone(), s.text_secondary, Align::Max);
            space::XXXL
        } else {
            0.0
        };
        let mut left = line;
        left.max.x -= pct_w;
        paint_in(ui, left, label, font, s.text_secondary, Align::Min);
    }
    let p = ui.painter();
    p.rect_filled(track, corner, s.bg_sunken);
    let bar = match frac {
        Some(f) => Rect::from_min_size(track.min, Vec2::new(w * f, track.height())),
        None => {
            let band = w * 0.3;
            let x = if th.reduced_motion {
                // no travel: the whole bar breathes
                ui.ctx().request_repaint();
                let k = pulse(ui.input(|i| i.time), SPINNER_PULSE_S);
                p.rect_filled(track, corner, s.accent.gamma_multiply(0.3 + 0.7 * k));
                return resp;
            } else {
                ui.ctx().request_repaint();
                let t = (ui.input(|i| i.time) % PROGRESS_PASS_S / PROGRESS_PASS_S) as f32;
                t * (w + band) - band
            };
            Rect::from_min_max(
                egui::pos2((track.left() + x).max(track.left()), track.top()),
                egui::pos2((track.left() + x + band).min(track.right()), track.bottom()),
            )
        }
    };
    p.rect_filled(bar, corner, s.accent);
    resp
}

#[cfg(test)]
mod state_tests {
    use super::*;

    #[test]
    fn empty_state_block_centres_at_40_percent() {
        let content = empty_content_h(true, true, true);
        let top = empty_top_pad(800.0, content);
        let centre = top + content / 2.0;
        assert!((centre - 800.0 * EMPTY_CENTER).abs() <= 1.0, "{centre}");
        // short panels never push the block under the edge padding
        assert_eq!(empty_top_pad(40.0, content), space::XL);
        assert_eq!(empty_top_pad(f32::INFINITY, content), space::XL);
        assert_eq!(icon_size::XL, 32.0);
        assert!(layout::EMPTY_W <= 320.0);
    }

    #[test]
    fn spinner_waits_400ms_and_stays_600ms() {
        let mut g = SpinGate::default();
        assert!(!spin_gate(&mut g, 0.0, true));
        assert!(!spin_gate(&mut g, 0.3, true));
        assert!(spin_gate(&mut g, 0.4, true));
        // work ended right away: still shown until 0.6 s after it appeared
        assert!(spin_gate(&mut g, 0.5, false));
        assert!(!spin_gate(&mut g, 1.05, false));
        // a quick job never flashes
        let mut q = SpinGate::default();
        assert!(!spin_gate(&mut q, 0.0, true));
        assert!(!spin_gate(&mut q, 0.2, false));
    }
}
