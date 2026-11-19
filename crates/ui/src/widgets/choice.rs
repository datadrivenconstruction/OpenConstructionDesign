// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Toggle and checkbox. Both take keyboard focus; Space and Enter flip them.
use super::text::paint_in;
use crate::theme::{control, space, stroke, Role};
use crate::Theme;
use egui::{Align, Key, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

/// Switch track, thumb and thumb inset (spec 2.4): 32x16 track, 12 thumb, 2 inset.
pub const TOGGLE_W: f32 = 32.0;
pub const TOGGLE_H: f32 = 16.0;
pub const TOGGLE_THUMB: f32 = 12.0;
/// Tick, dash and slider-thumb metrics (spec 2.5, 2.9).
pub const TICK_STROKE: f32 = 1.5;
pub const DASH_W: f32 = 8.0;
pub const DASH_H: f32 = 2.0;
pub const RADIO_DOT: f32 = 6.0;
/// Row of a checkbox, radio or switch: hit target and text line.
pub const ROW_H: f32 = 24.0;
pub const SLIDER_TRACK_H: f32 = 4.0;
pub const SLIDER_THUMB: f32 = 16.0;
pub const SLIDER_THUMB_HOT: f32 = 18.0;

fn allocate(ui: &mut Ui, th: &Theme, mark_w: f32, label: &str) -> (Rect, Rect, Response, bool) {
    let font = th.font(Role::Body);
    let text_w = if label.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(label.to_owned(), font, th.sem().text_primary)
            .size()
            .x
    };
    let gap = if label.is_empty() { 0.0 } else { space::SM };
    let w = (mark_w + gap + text_w).min(ui.available_width().max(mark_w));
    let _ = th;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
    let text = Rect::from_min_max(
        egui::pos2((rect.left() + mark_w + gap).min(rect.right()), rect.top()),
        rect.max,
    );
    let enabled = ui.is_enabled();
    (rect, text, resp, enabled)
}

fn paint_label(ui: &Ui, th: &Theme, rect: Rect, label: &str, enabled: bool) -> bool {
    if label.is_empty() {
        return false;
    }
    let col = if enabled {
        th.sem().text_primary
    } else {
        th.sem().text_disabled
    };
    paint_in(ui, rect, label, th.font(Role::Body), col, Align::Min)
}

/// Checkbox. Response is `changed()` when flipped.
pub fn checkbox(ui: &mut Ui, th: &Theme, value: &mut bool, label: &str) -> Response {
    check(ui, th, value, false, false, label)
}

/// Checkbox in the error state: the box border turns `error`.
pub fn checkbox_error(ui: &mut Ui, th: &Theme, value: &mut bool, label: &str) -> Response {
    check(ui, th, value, false, true, label)
}

/// Checkbox for a multi-selection whose elements disagree: shows a dash, a click turns it on.
pub fn checkbox_mixed(
    ui: &mut Ui,
    th: &Theme,
    value: &mut bool,
    mixed: bool,
    label: &str,
) -> Response {
    check(ui, th, value, mixed, false, label)
}

fn check(
    ui: &mut Ui,
    th: &Theme,
    value: &mut bool,
    mixed: bool,
    err: bool,
    label: &str,
) -> Response {
    let (rect, text, mut resp, enabled) = allocate(ui, th, control::CHECK, label);
    if resp.clicked() {
        *value = if mixed { true } else { !*value };
        resp.mark_changed();
    }
    let on = *value;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, label));
    let s = th.sem();
    let b = Rect::from_center_size(
        egui::pos2(rect.left() + control::CHECK / 2.0, rect.center().y),
        Vec2::splat(control::CHECK),
    );
    let filled = (on || mixed) && enabled;
    let fill = if filled {
        if resp.hovered() {
            s.accent_hover
        } else {
            s.accent
        }
    } else if !enabled {
        s.bg_sunken
    } else {
        s.bg_input
    };
    let line = if filled {
        fill
    } else if !enabled {
        s.border_subtle
    } else if err {
        s.error
    } else if resp.hovered() {
        s.border_strong
    } else {
        s.border_control
    };
    let p = ui.painter();
    p.rect(
        b,
        th.radius("xs"),
        fill,
        Stroke::new(stroke::CONTROL, line),
        StrokeKind::Inside,
    );
    let ink = Stroke::new(
        TICK_STROKE,
        if enabled {
            s.accent_text_on
        } else {
            s.text_disabled
        },
    );
    let c = b.center();
    if mixed {
        p.rect_filled(
            Rect::from_center_size(c, Vec2::new(DASH_W, DASH_H)),
            0.0,
            ink.color,
        );
    } else if on {
        let pts = [
            c + Vec2::new(-3.5, 0.0),
            c + Vec2::new(-1.0, 2.5),
            c + Vec2::new(3.5, -2.5),
        ];
        p.add(Shape::line(pts.to_vec(), ink));
        // round caps and join: a disc of half the stroke width on every vertex
        for q in pts {
            p.circle_filled(q, TICK_STROKE / 2.0, ink.color);
        }
    }
    if paint_label(ui, th, text, label, enabled) {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

/// On/off switch for settings that apply at once. Response is `changed()` when flipped.
pub fn toggle(ui: &mut Ui, th: &Theme, on: &mut bool, label: &str) -> Response {
    toggle_shown(ui, th, on, label, label)
}

/// A switch with no text beside it, for a form row whose label already says what it turns on;
/// `name` is what a screen reader announces.
pub fn toggle_bare(ui: &mut Ui, th: &Theme, on: &mut bool, name: &str) -> Response {
    toggle_shown(ui, th, on, "", name)
}

fn toggle_shown(ui: &mut Ui, th: &Theme, on: &mut bool, label: &str, name: &str) -> Response {
    let (rect, text, mut resp, enabled) = allocate(ui, th, TOGGLE_W, label);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let v = *on;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, v, name));
    let s = th.sem();
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, v, ui.style().animation_time);
    let track = Rect::from_center_size(
        egui::pos2(rect.left() + TOGGLE_W / 2.0, rect.center().y),
        Vec2::new(TOGGLE_W, TOGGLE_H),
    );
    let (off_c, on_c) = if enabled {
        (s.control_track, s.accent)
    } else {
        (s.bg_sunken, s.border_strong)
    };
    let r = track.height() / 2.0;
    let knob_r = TOGGLE_THUMB / 2.0;
    let x = egui::lerp(
        (track.left() + space::XXS + knob_r)..=(track.right() - space::XXS - knob_r),
        t,
    );
    // the thumb stays readable on the accent in both themes
    let knob = if !enabled {
        s.text_disabled
    } else {
        s.control_thumb.lerp_to_gamma(s.accent_text_on, t)
    };
    let p = ui.painter();
    p.rect_filled(track, r, off_c.lerp_to_gamma(on_c, t));
    let kc = egui::pos2(x, track.center().y);
    if enabled {
        let sh = th.shadow(1);
        p.add(sh.as_shape(Rect::from_center_size(kc, Vec2::splat(TOGGLE_THUMB)), knob_r));
    }
    p.circle_filled(kc, knob_r, knob);
    if paint_label(ui, th, text, label, enabled) {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

/// Radio button: 16 circle, 6 dot. Response is `clicked()` when chosen; the caller owns the
/// selection. Use `radio_group` for a column of options.
pub fn radio(ui: &mut Ui, th: &Theme, selected: bool, label: &str) -> Response {
    let (rect, text, resp, enabled) = allocate(ui, th, control::CHECK, label);
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, selected, label)
    });
    let s = th.sem();
    let c = egui::pos2(rect.left() + control::CHECK / 2.0, rect.center().y);
    let (fill, line) = if !enabled {
        (s.bg_sunken, s.border_subtle)
    } else if selected {
        (s.accent, if resp.hovered() { s.accent_hover } else { s.accent })
    } else if resp.hovered() {
        (s.bg_input, s.border_strong)
    } else {
        (s.bg_input, s.border_control)
    };
    let fill = if selected && enabled && resp.hovered() { s.accent_hover } else { fill };
    let p = ui.painter();
    p.circle(c, control::CHECK / 2.0, fill, Stroke::new(stroke::CONTROL, line));
    if selected {
        let dot = if enabled { s.accent_text_on } else { s.text_disabled };
        p.circle_filled(c, RADIO_DOT / 2.0, dot);
    }
    if paint_label(ui, th, text, label, enabled) {
        resp.on_hover_text(label)
    } else {
        resp
    }
}

/// Column of radios; returns the newly chosen index.
pub fn radio_group(ui: &mut Ui, th: &Theme, items: &[&str], sel: usize) -> Option<usize> {
    let mut out = None;
    for (i, t) in items.iter().enumerate() {
        if radio(ui, th, i == sel, t).clicked() && i != sel {
            out = Some(i);
        }
    }
    out
}

/// Horizontal slider: 4 px pill track, 16 px thumb (18 on hover or drag), accent fill left of the
/// thumb. Arrows move by `step`, PageUp/PageDown by 10 steps, Home/End to the ends.
pub fn slider(
    ui: &mut Ui,
    th: &Theme,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f32,
) -> Response {
    let (lo, hi) = (*range.start(), *range.end());
    let w = ui.available_width().clamp(96.0, 240.0);
    let (rect, mut resp) =
        ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click_and_drag());
    let enabled = ui.is_enabled();
    let half = SLIDER_THUMB_HOT / 2.0;
    let span = (rect.width() - 2.0 * half).max(1.0);
    let before = *value;
    if enabled {
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on() || resp.clicked()) {
            let f = ((p.x - rect.left() - half) / span).clamp(0.0, 1.0);
            *value = lo + f * (hi - lo);
        }
        if resp.has_focus() {
            let k = ui.input(|i| {
                let d = |a: Key, b: Key| {
                    (i.key_pressed(a) as i32 - i.key_pressed(b) as i32) as f32
                };
                (
                    d(Key::ArrowRight, Key::ArrowLeft) + d(Key::ArrowUp, Key::ArrowDown),
                    d(Key::PageUp, Key::PageDown),
                    i.key_pressed(Key::Home),
                    i.key_pressed(Key::End),
                )
            });
            *value += k.0 * step + k.1 * 10.0 * step;
            if k.2 {
                *value = lo;
            }
            if k.3 {
                *value = hi;
            }
        }
        *value = value.clamp(lo, hi);
        if step > 0.0 && !resp.dragged() {
            *value = (lo + ((*value - lo) / step).round() * step).clamp(lo, hi);
        }
    }
    if *value != before {
        resp.mark_changed();
    }
    let v = *value;
    resp.widget_info(|| {
        egui::WidgetInfo::slider(enabled, v as f64, "")
    });
    let s = th.sem();
    let f = if hi > lo { (v - lo) / (hi - lo) } else { 0.0 };
    let cx = rect.left() + half + f * span;
    let y = rect.center().y;
    let hot = enabled && (resp.hovered() || resp.dragged());
    let r = ui.ctx().animate_value_with_time(
        resp.id,
        if hot { SLIDER_THUMB_HOT } else { SLIDER_THUMB } / 2.0,
        ui.style().animation_time,
    );
    let track = Rect::from_min_max(
        egui::pos2(rect.left() + half, y - SLIDER_TRACK_H / 2.0),
        egui::pos2(rect.right() - half, y + SLIDER_TRACK_H / 2.0),
    );
    let (base, fillc) = if enabled {
        (s.bg_sunken, s.accent)
    } else {
        (s.bg_sunken, s.border_strong)
    };
    let p = ui.painter();
    let pill = SLIDER_TRACK_H / 2.0;
    p.rect_filled(track, pill, base);
    p.rect_filled(track.with_max_x(cx.max(track.left())), pill, fillc);
    let kc = egui::pos2(cx, y);
    if enabled {
        p.add(th.shadow(1).as_shape(Rect::from_center_size(kc, Vec2::splat(2.0 * r)), r));
    }
    p.circle(
        kc,
        r,
        if enabled { s.bg_elevated } else { s.bg_sunken },
        Stroke::new(
            stroke::CONTROL,
            if enabled { s.border_control } else { s.border_subtle },
        ),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, Density, Mode};
    use egui::{Event, RawInput};

    fn ready(mode: Mode) -> (egui::Context, Theme) {
        let ctx = egui::Context::default();
        let mut th = Theme::new(mode, Density::Compact);
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                th.apply(c);
            });
        }
        (ctx, th)
    }

    fn keys(ks: &[Key]) -> RawInput {
        RawInput {
            events: ks
                .iter()
                .map(|&key| Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn shapes(ctx: &egui::Context, th: &Theme, on: bool, f: impl Fn(&mut Ui, &Theme, &mut bool)) -> Vec<Shape> {
        let mut v = on;
        let out = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| f(ui, th, &mut v));
        });
        out.shapes.into_iter().map(|s| s.shape).collect()
    }

    #[test]
    fn metrics_sit_on_the_grid() {
        for v in [TOGGLE_W, TOGGLE_H, TOGGLE_THUMB, control::CHECK, ROW_H, DASH_W, RADIO_DOT, SLIDER_THUMB, SLIDER_THUMB_HOT, SLIDER_TRACK_H] {
            assert_eq!(v % 2.0, 0.0, "{v}");
        }
        for v in [TOGGLE_W, control::CHECK, ROW_H, SLIDER_THUMB] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        assert!(ROW_H >= control::HIT_MIN);
    }

    #[test]
    fn toggle_hit_rect_includes_label_and_track_is_32x16() {
        let (ctx, th) = ready(Mode::Light);
        let mut rect = Rect::NOTHING;
        let mut bare = Rect::NOTHING;
        let mut on = false;
        let mut sh = Vec::new();
        let out = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                rect = toggle(ui, &th, &mut on, "Snap to grid").rect;
                bare = toggle_bare(ui, &th, &mut on, "x").rect;
            });
        });
        sh.extend(out.shapes.into_iter().map(|s| s.shape));
        assert!(rect.width() > TOGGLE_W + space::SM + 20.0, "label is part of the hit rect");
        assert_eq!(bare.size(), Vec2::new(TOGGLE_W, ROW_H));
        let track = sh.iter().find_map(|s| match s {
            Shape::Rect(r) if (r.rect.height() - TOGGLE_H).abs() < 0.01 => Some(r.rect),
            _ => None,
        });
        assert_eq!(track.expect("track").width(), TOGGLE_W);
    }

    #[test]
    fn thumb_travels_between_two_inset_stops() {
        for (on, left) in [(false, true), (true, false)] {
            // a fresh context per state: a reused one would show the thumb mid-animation
            let (ctx, th) = ready(Mode::Light);
            let sh = shapes(&ctx, &th, on, |ui, th, v| {
                toggle_bare(ui, th, v, "x");
            });
            let track = sh.iter().find_map(|s| match s {
                Shape::Rect(r) if (r.rect.height() - TOGGLE_H).abs() < 0.01 => Some(r.rect),
                _ => None,
            });
            let track = track.expect("track");
            let thumb = sh.iter().rev().find_map(|s| match s {
                Shape::Circle(c) if (c.radius - TOGGLE_THUMB / 2.0).abs() < 0.01 => Some(c.center),
                _ => None,
            });
            let c = thumb.expect("thumb");
            let want = if left {
                track.left() + space::XXS + TOGGLE_THUMB / 2.0
            } else {
                track.right() - space::XXS - TOGGLE_THUMB / 2.0
            };
            assert!((c.x - want).abs() < 0.5, "on={on}: {} vs {want}", c.x);
            assert!((c.y - track.center().y).abs() < 0.01);
        }
    }

    #[test]
    fn checkbox_box_is_16_radius_2_and_tick_is_centred() {
        let (ctx, th) = ready(Mode::Light);
        let sh = shapes(&ctx, &th, true, |ui, th, v| {
            checkbox(ui, th, v, "");
        });
        let b = sh.iter().find_map(|s| match s {
            Shape::Rect(r) if (r.rect.width() - control::CHECK).abs() < 0.01 => Some(r.clone()),
            _ => None,
        });
        let b = b.expect("box");
        assert_eq!(b.rect.height(), control::CHECK);
        assert_eq!(b.corner_radius.nw, th.radius("xs") as u8);
        let tick = sh.iter().find_map(|s| match s {
            Shape::Path(l) if !l.closed => Some(l.clone()),
            _ => None,
        });
        let l = tick.expect("tick");
        assert_eq!(l.stroke.width, TICK_STROKE);
        assert_eq!(l.stroke.color, egui::epaint::ColorMode::Solid(th.sem().accent_text_on));
        let (mut lo, mut hi) = (egui::Pos2::new(f32::MAX, f32::MAX), egui::Pos2::new(f32::MIN, f32::MIN));
        for p in &l.points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let mid = Rect::from_min_max(lo, hi).center();
        assert!((mid - b.rect.center()).abs().max_elem() <= 0.5, "{mid:?} vs {:?}", b.rect.center());
    }

    #[test]
    fn mixed_checkbox_draws_an_8x2_bar() {
        let (ctx, th) = ready(Mode::Dark);
        let mut v = false;
        let out = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                checkbox_mixed(ui, &th, &mut v, true, "");
            });
        });
        assert!(out.shapes.iter().any(|s| matches!(&s.shape, Shape::Rect(r) if r.rect.size() == Vec2::new(DASH_W, DASH_H))));
    }

    #[test]
    fn error_checkbox_border_uses_error_role() {
        let (ctx, th) = ready(Mode::Light);
        let sh = shapes(&ctx, &th, false, |ui, th, v| {
            checkbox_error(ui, th, v, "");
        });
        assert!(sh.iter().any(|s| matches!(s, Shape::Rect(r) if r.stroke.color == th.sem().error && r.rect.width() == control::CHECK)));
    }

    #[test]
    fn radio_is_16_circle_with_6_dot_only_when_selected() {
        let (ctx, th) = ready(Mode::Light);
        for sel in [false, true] {
            let sh = shapes(&ctx, &th, sel, |ui, th, v| {
                radio(ui, th, *v, "");
            });
            let circles: Vec<_> = sh
                .iter()
                .filter_map(|s| if let Shape::Circle(c) = s { Some(c.clone()) } else { None })
                .collect();
            assert!(circles.iter().any(|c| c.radius == control::CHECK / 2.0));
            let dot = circles.iter().find(|c| c.radius == RADIO_DOT / 2.0);
            assert_eq!(dot.is_some(), sel);
            if let Some(d) = dot {
                let ring = circles.iter().find(|c| c.radius == control::CHECK / 2.0).unwrap();
                assert!((d.center - ring.center).abs().max_elem() <= 0.5);
            }
        }
    }

    #[test]
    fn radio_group_reports_a_new_choice_only() {
        let (ctx, th) = ready(Mode::Light);
        let mut got = None;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                got = radio_group(ui, &th, &["a", "b"], 0);
            });
        });
        assert_eq!(got, None);
    }

    #[test]
    fn slider_keyboard_steps_and_page_keys() {
        let (ctx, th) = ready(Mode::Light);
        let mut v = 5.0_f32;
        let id = std::cell::Cell::new(None);
        let frame = |input: RawInput, v: &mut f32| {
            let mut changed = false;
            let _ = ctx.run(input, |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let r = slider(ui, &th, v, 0.0..=100.0, 1.0);
                    id.set(Some(r.id));
                    changed = r.changed();
                });
            });
            changed
        };
        frame(Default::default(), &mut v);
        ctx.memory_mut(|m| m.request_focus(id.get().unwrap()));
        frame(Default::default(), &mut v);
        assert!(frame(keys(&[Key::ArrowRight]), &mut v));
        assert_eq!(v, 6.0);
        frame(keys(&[Key::PageUp]), &mut v);
        assert_eq!(v, 16.0);
        frame(keys(&[Key::ArrowLeft]), &mut v);
        assert_eq!(v, 15.0);
        frame(keys(&[Key::End]), &mut v);
        assert_eq!(v, 100.0);
        frame(keys(&[Key::PageDown]), &mut v);
        assert_eq!(v, 90.0);
        frame(keys(&[Key::Home]), &mut v);
        assert_eq!(v, 0.0);
    }

    #[test]
    fn slider_thumb_is_16_at_rest_and_stays_inside_its_row() {
        let (ctx, th) = ready(Mode::Dark);
        let mut v = 100.0_f32;
        let mut rect = Rect::NOTHING;
        let out = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                rect = slider(ui, &th, &mut v, 0.0..=100.0, 1.0).rect;
            });
        });
        assert_eq!(rect.height(), ROW_H);
        let thumb = out.shapes.iter().rev().find_map(|s| match &s.shape {
            Shape::Circle(c) => Some(c.clone()),
            _ => None,
        });
        let c = thumb.expect("thumb");
        assert_eq!(c.radius, SLIDER_THUMB / 2.0);
        assert!(c.center.x + SLIDER_THUMB_HOT / 2.0 <= rect.right() + 0.01);
    }

    #[test]
    fn segmented_has_exactly_one_selected_runner_in_a_radius_8_track() {
        for mode in [Mode::Light, Mode::Dark] {
            let (ctx, th) = ready(mode);
            let mut h = 0.0;
            let out = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    let before = ui.cursor().top();
                    super::super::segmented(ui, &th, &["Plan", "3D", "Both"], 1);
                    h = ui.min_rect().bottom() - before;
                });
            });
            let sh: Vec<_> = out.shapes.iter().map(|s| &s.shape).collect();
            let elev = th.c("bg.elevated");
            let runners = sh
                .iter()
                .flat_map(|s| match s {
                    Shape::Vec(v) => v.iter().collect::<Vec<_>>(),
                    s => vec![*s],
                })
                .filter(|s| matches!(s, Shape::Rect(r) if r.fill == elev && r.stroke.color == Default::default()))
                .count();
            assert_eq!(runners, 1, "{mode:?}");
            let track = sh.iter().find_map(|s| match s {
                Shape::Rect(r) if r.fill == th.c("bg.sunken") => Some(r.clone()),
                _ => None,
            });
            assert_eq!(track.expect("track").corner_radius.nw, th.radius("md") as u8);
            assert!(h >= th.d().control - 0.01);
        }
    }

    /// Software-rasterises the frame's meshes (solid vertex colours, source-over) and returns the
    /// pixel under the point `(x, y)` given in points.
    fn probe(ctx: &egui::Context, out: &egui::FullOutput, bg: egui::Color32, at: egui::Pos2) -> egui::Color32 {
        let ppp = out.pixels_per_point;
        let (px, py) = (at.x * ppp, at.y * ppp);
        let mut acc = [bg.r() as f32, bg.g() as f32, bg.b() as f32];
        for cp in ctx.tessellate(out.shapes.clone(), ppp) {
            let egui::epaint::Primitive::Mesh(m) = cp.primitive else { continue };
            if !cp.clip_rect.contains(at) {
                continue;
            }
            for t in m.indices.chunks_exact(3) {
                let v = [&m.vertices[t[0] as usize], &m.vertices[t[1] as usize], &m.vertices[t[2] as usize]];
                let p = |i: usize| (v[i].pos.x * ppp, v[i].pos.y * ppp);
                let (a, b, c) = (p(0), p(1), p(2));
                let e = |p0: (f32, f32), p1: (f32, f32)| (p1.0 - p0.0) * (py - p0.1) - (p1.1 - p0.1) * (px - p0.0);
                let (w0, w1, w2) = (e(b, c), e(c, a), e(a, b));
                let inside = (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0);
                if !inside {
                    continue;
                }
                // barycentric blend of the (premultiplied) vertex colours
                let s = w0 + w1 + w2;
                if s == 0.0 {
                    continue;
                }
                let mut col = [0.0_f32; 4];
                for (w, vv) in [(w0, v[0]), (w1, v[1]), (w2, v[2])] {
                    for (k, ch) in vv.color.to_array().iter().enumerate() {
                        col[k] += w / s * *ch as f32;
                    }
                }
                let ia = 1.0 - col[3] / 255.0;
                for k in 0..3 {
                    acc[k] = col[k] + acc[k] * ia;
                }
            }
        }
        egui::Color32::from_rgb(acc[0].round() as u8, acc[1].round() as u8, acc[2].round() as u8)
    }

    fn near(a: egui::Color32, b: egui::Color32) -> bool {
        a.to_array().iter().zip(b.to_array()).all(|(x, y)| (*x as i32 - y as i32).abs() <= 2)
    }

    /// Renders one unlabelled control at `ppp` and returns the frame, its rect and the panel fill.
    fn render(
        mode: Mode,
        ppp: f32,
        f: impl Fn(&mut Ui, &Theme) -> Rect,
    ) -> (egui::Context, egui::FullOutput, Rect, egui::Color32) {
        let (ctx, th) = ready(mode);
        let mut rect = Rect::NOTHING;
        let input = RawInput {
            viewport_id: egui::ViewportId::ROOT,
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(120.0))),
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo { native_pixels_per_point: Some(ppp), ..Default::default() },
            ))
            .collect(),
            ..Default::default()
        };
        ctx.set_pixels_per_point(ppp);
        let out = ctx.run(input, |c| {
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(c, |ui| {
                rect = f(ui, &th);
            });
        });
        (ctx, out, rect, egui::Color32::TRANSPARENT)
    }

    #[test]
    fn pixel_probe_checkbox_toggle_radio_1x_and_2x() {
        for mode in [Mode::Light, Mode::Dark] {
            let th = ready(mode).1;
            let s = th.sem();
            for ppp in [1.0_f32, 2.0] {
                // checkbox on: corner is accent, a point on the tick is text_on, off is bg_input
                let (ctx, out, r, bg) = render(mode, ppp, |ui, th| checkbox(ui, th, &mut true.clone(), "").rect);
                let b = Rect::from_min_size(r.min + Vec2::new(0.0, (r.height() - 16.0) / 2.0), Vec2::splat(16.0));
                assert!(near(probe(&ctx, &out, bg, b.center() + Vec2::new(-6.0, -6.0)), s.accent), "{mode:?} {ppp} fill");
                assert!(near(probe(&ctx, &out, bg, b.center() + Vec2::new(1.25, 0.0)), s.accent_text_on), "{mode:?} {ppp} tick");
                let (ctx, out, r, bg) = render(mode, ppp, |ui, th| checkbox(ui, th, &mut false.clone(), "").rect);
                let c = r.center();
                assert!(near(probe(&ctx, &out, bg, c), s.bg_input), "{mode:?} {ppp} off");
                // toggle: thumb centre sits on the inset stop
                for on in [false, true] {
                    let (ctx, out, r, bg) = render(mode, ppp, move |ui, th| toggle_bare(ui, th, &mut on.clone(), "x").rect);
                    let kx = if on { r.right() - 2.0 - 6.0 } else { r.left() + 2.0 + 6.0 };
                    let want = if on { s.accent_text_on } else { s.control_thumb };
                    assert!(near(probe(&ctx, &out, bg, egui::pos2(kx, r.center().y)), want), "{mode:?} {ppp} thumb on={on}");
                    let tx = if on { r.left() + 2.0 } else { r.right() - 2.0 };
                    let want = if on { s.accent } else { s.control_track };
                    assert!(near(probe(&ctx, &out, bg, egui::pos2(tx, r.center().y)), want), "{mode:?} {ppp} track on={on}");
                }
                // radio selected: dot is text_on, ring body accent; unselected: centre is bg_input
                let (ctx, out, r, bg) = render(mode, ppp, |ui, th| radio(ui, th, true, "").rect);
                let c = egui::pos2(r.left() + 8.0, r.center().y);
                assert!(near(probe(&ctx, &out, bg, c), s.accent_text_on), "{mode:?} {ppp} dot");
                assert!(near(probe(&ctx, &out, bg, c + Vec2::new(5.5, 0.0)), s.accent), "{mode:?} {ppp} ring");
                let (ctx, out, r, bg) = render(mode, ppp, |ui, th| radio(ui, th, false, "").rect);
                let c = egui::pos2(r.left() + 8.0, r.center().y);
                assert!(near(probe(&ctx, &out, bg, c), s.bg_input), "{mode:?} {ppp} empty");
            }
        }
    }

    #[test]
    fn selected_marks_meet_contrast_floors_in_both_themes() {
        for mode in [Mode::Light, Mode::Dark] {
            let (_, th) = ready(mode);
            let s = th.sem();
            assert!(contrast(s.accent_text_on, s.accent) >= 4.5, "{mode:?} tick on accent");
            assert!(contrast(s.control_track, s.bg_panel) >= 3.0, "{mode:?} off track");
            assert!(contrast(s.border_control, s.bg_panel) >= 3.0, "{mode:?} box border");
            assert!(contrast(s.accent, s.bg_panel) >= 3.0, "{mode:?} on state");
        }
    }
}
