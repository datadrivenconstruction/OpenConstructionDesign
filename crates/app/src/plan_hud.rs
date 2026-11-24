// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan canvas overlays: the origin axis cross and the bottom-left HUD (view name, scale,
//! fit-to-view) on quiet chips. Paint and geometry only; no model data is read or changed.
use crate::ui_kit::Tokens;
use egui::{self, Align2, Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// Axis arm past the origin and tail behind it, in points.
pub const AXIS_ARM_PX: f32 = 56.0;
pub const AXIS_TAIL_PX: f32 = 14.0;
/// Axis colour share over the paper: readable, but weaker than the wall cut line.
pub const AXIS_ALPHA: f32 = 0.7;
pub const AXIS_STROKE_PX: f32 = 1.25;
/// Held key that shows the axes as full lines across the sheet. Not F3: that toggles snapping.
pub const FULL_AXES_KEY: egui::Key = egui::Key::F9;
/// Command that owns the key and the Settings toggle (`commands.d/ax97.json`).
pub const FULL_AXES_CMD: &str = "view.axes_full";
/// Room kept between an axis label and the canvas edge.
const EDGE_PX: f32 = 14.0;

pub const HUD_MARGIN: f32 = 12.0;
pub const CHIP_H: f32 = 24.0;
pub const CHIP_PAD: f32 = 8.0;
pub const CHIP_GAP: f32 = 4.0;
/// Free run between the chips and the scale bar.
pub const BAR_GAP: f32 = 12.0;
/// Pixels to millimetres on paper at 96 dpi.
const PX_MM: f64 = 25.4 / 96.0;

/// One axis: the stroke ends and where its letter is anchored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub a: Pos2,
    pub b: Pos2,
    pub label: Pos2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AxisCross {
    pub x: Option<Axis>,
    pub y: Option<Axis>,
}

/// Axes for the origin at screen point `o`. Short mode draws a cross when the origin is on
/// the canvas; `full` runs each line across the canvas (labels near its end).
pub fn axis_cross(rect: Rect, o: Pos2, full: bool) -> AxisCross {
    let inner = rect.shrink(EDGE_PX);
    let (x_in, y_in) = (rect.y_range().contains(o.y), rect.x_range().contains(o.x));
    if full {
        let x = x_in.then(|| Axis {
            a: Pos2::new(rect.left(), o.y),
            b: Pos2::new(rect.right(), o.y),
            label: Pos2::new(rect.right() - 8.0, o.y - 3.0),
        });
        let y = y_in.then(|| Axis {
            a: Pos2::new(o.x, rect.bottom()),
            b: Pos2::new(o.x, rect.top()),
            label: Pos2::new(o.x + 4.0, rect.top() + 6.0),
        });
        return AxisCross { x, y };
    }
    if !rect.contains(o) {
        return AxisCross::default();
    }
    // a canvas narrower than its two edges keeps the cross at its middle, not an inverted range
    let c = rect.center();
    let cx = |v: f32| v.clamp(inner.left().min(c.x), inner.right().max(c.x));
    let cy = |v: f32| v.clamp(inner.top().min(c.y), inner.bottom().max(c.y));
    let x = Axis {
        a: Pos2::new(cx(o.x - AXIS_TAIL_PX), o.y),
        b: Pos2::new(cx(o.x + AXIS_ARM_PX), o.y),
        label: Pos2::new(cx(o.x + AXIS_ARM_PX) + 4.0, o.y),
    };
    // world Y grows up the screen
    let y = Axis {
        a: Pos2::new(o.x, cy(o.y + AXIS_TAIL_PX)),
        b: Pos2::new(o.x, cy(o.y - AXIS_ARM_PX)),
        label: Pos2::new(o.x, cy(o.y - AXIS_ARM_PX) - 3.0),
    };
    AxisCross { x: Some(x), y: Some(y) }
}

/// Full lines when the preference is on, or while the key is held outside a text field.
pub fn full_axes(pref: bool, held: bool, typing: bool) -> bool {
    pref || (held && !typing)
}

/// Axis colour as it lands on the paper (no transparency left), index 0 = X, 1 = Y.
pub fn axis_on_paper(t: &Tokens, i: usize) -> Color32 {
    ddc_ui::theme::over(t.ov().axis[i], AXIS_ALPHA, t.paper)
}

pub fn paint_axes(painter: &Painter, rect: Rect, o: Pos2, t: &Tokens, full: bool) {
    let cross = axis_cross(rect, o, full);
    let small = egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION);
    for (i, ax) in [cross.x, cross.y].into_iter().enumerate() {
        let Some(ax) = ax else { continue };
        let col = axis_on_paper(t, i);
        painter.line_segment([ax.a, ax.b], Stroke::new(AXIS_STROKE_PX, col));
        let (anchor, text) = if i == 0 {
            (if full { Align2::RIGHT_BOTTOM } else { Align2::LEFT_CENTER }, "X")
        } else {
            (if full { Align2::LEFT_TOP } else { Align2::CENTER_BOTTOM }, "Y")
        };
        painter.text(ax.label, anchor, text, small.clone(), col);
    }
    if rect.contains(o) {
        painter.circle_stroke(o, 3.0, Stroke::new(1.0, t.text_dim));
    }
}

/// Text widths (points, without chip padding) and the bar the HUD has to fit.
#[derive(Clone, Copy, Debug)]
pub struct HudIn {
    pub name_w: f32,
    pub scale_w: f32,
    pub fit_w: f32,
    pub bar_px: f32,
    pub bar_label_w: f32,
}

/// Bottom row of the canvas: chips from the left, scale bar at the right, one baseline.
/// A chip that does not fit is zero wide (`shown` is false); the name is cut first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hud {
    pub name: Rect,
    pub scale: Rect,
    pub fit: Rect,
    pub bar: Rect,
}

pub fn shown(r: Rect) -> bool {
    r.width() > 0.5
}

pub fn layout(rect: Rect, w: HudIn) -> Hud {
    let bottom = rect.bottom() - HUD_MARGIN;
    let top = bottom - CHIP_H;
    let cap = rect.width() / 3.0;
    let bar_w = w.bar_px.max(w.bar_label_w).min(cap).max(0.0);
    let bar = Rect::from_min_max(
        Pos2::new(rect.right() - HUD_MARGIN - bar_w, top),
        Pos2::new(rect.right() - HUD_MARGIN, bottom),
    );
    let left = rect.left() + HUD_MARGIN;
    let avail = (bar.left() - BAR_GAP - left).max(0.0);
    let chip = |text_w: f32| text_w + 2.0 * CHIP_PAD;
    let (scale_w, fit_w) = (chip(w.scale_w), chip(w.fit_w));
    let mut name_w = chip(w.name_w).min((avail - scale_w - fit_w - 2.0 * CHIP_GAP).max(0.0));
    if name_w < 2.0 * CHIP_PAD + 16.0 {
        name_w = 0.0;
    }
    let mut x = left;
    let take = |width: f32, x: &mut f32| {
        let r = Rect::from_min_size(Pos2::new(*x, top), Vec2::new(width, CHIP_H));
        *x += width + CHIP_GAP;
        r
    };
    let name = take(name_w, &mut x);
    if name_w == 0.0 {
        x = left;
    }
    let (scale, fit) = if x - left + scale_w + CHIP_GAP + fit_w <= avail {
        let s = take(scale_w, &mut x);
        (s, take(fit_w, &mut x))
    } else if x - left + fit_w <= avail {
        (Rect::from_min_size(Pos2::new(x, top), Vec2::ZERO), take(fit_w, &mut x))
    } else {
        let z = Rect::from_min_size(Pos2::new(x, top), Vec2::ZERO);
        (z, z)
    };
    Hud { name, scale, fit, bar }
}

/// Paper ratio of the plan zoom (1 : n) at 96 dpi.
pub fn paper_ratio(px_per_m: f64) -> u64 {
    (1000.0 / (px_per_m.max(1e-6) * PX_MM)).round().clamp(1.0, 9_999_999.0) as u64
}

pub fn scale_text(px_per_m: f64) -> String {
    format!("1:{}", paper_ratio(px_per_m))
}

/// Plan chip: the view's drawing scale (annotation size, sheets: "1:100") and the zoom against
/// it in percent, so a zoom ratio is never read as the view scale.
pub fn view_scale_text(view_denom: f64, px_per_m: f64) -> String {
    let zoom = view_denom * px_per_m.max(1e-6) * PX_MM / 10.0;
    format!("1:{} \u{b7} {}%", view_denom.round() as u64, zoom.round() as u64)
}

fn font() -> egui::FontId {
    egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION)
}

pub fn text_w(painter: &Painter, s: &str, color: Color32) -> f32 {
    painter.layout_no_wrap(s.to_string(), font(), color).size().x
}

/// `s` cut with an ellipsis so that it is at most `max_w` wide.
pub fn elide(painter: &Painter, s: &str, max_w: f32, color: Color32) -> String {
    let chars: Vec<char> = s.chars().collect();
    for n in (1..=chars.len()).rev() {
        let mut c: String = chars[..n].iter().collect();
        if n < chars.len() {
            c = format!("{}\u{2026}", c.trim_end());
        }
        if text_w(painter, &c, color) <= max_w {
            return c;
        }
    }
    String::new()
}

fn chip(painter: &Painter, r: Rect, text: &str, col: Color32, edge: Color32, t: &Tokens) {
    painter.rect(
        r,
        t.radius as f32,
        t.panel.gamma_multiply(0.9),
        Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );
    painter.text(r.center(), Align2::CENTER_CENTER, text, font(), col);
}

/// Paints the chips; `fit_hot` lights the fit chip under the pointer.
pub fn paint_chips(
    painter: &Painter,
    hud: &Hud,
    name: &str,
    scale: &str,
    fit: &str,
    fit_hot: bool,
    t: &Tokens,
) {
    if shown(hud.name) {
        let s = elide(painter, name, hud.name.width() - 2.0 * CHIP_PAD, t.text_dim);
        chip(painter, hud.name, &s, t.text_dim, t.border, t);
    }
    if shown(hud.scale) {
        chip(painter, hud.scale, scale, t.text_dim, t.border, t);
    }
    if shown(hud.fit) {
        let (col, edge) = if fit_hot { (t.text, t.accent) } else { (t.text_dim, t.border) };
        chip(painter, hud.fit, fit, col, edge, t);
    }
}

/// Scale bar inside `hud.bar`: the line sits on the chip row baseline, the label above it.
pub fn paint_bar(painter: &Painter, bar: Rect, px: f32, label: &str, t: &Tokens) {
    let y = bar.bottom() - 3.0;
    let (l0, r) = (Pos2::new(bar.right() - px, y), Pos2::new(bar.right(), y));
    let st = Stroke::new(1.5, t.text);
    painter.line_segment([l0, r], st);
    for p in [l0, Pos2::new(l0.x + px / 2.0, y), r] {
        painter.line_segment([p, p - Vec2::new(0.0, 5.0)], st);
    }
    painter.text(
        Pos2::new(bar.right() - px / 2.0, y - 7.0),
        Align2::CENTER_BOTTOM,
        label,
        font(),
        t.text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::{Density, Mode, Theme};

    fn tokens(mode: Mode) -> Tokens {
        crate::ui_kit::from_theme(&Theme::new(mode, Density::Compact))
    }

    fn canvas(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(40.0, 100.0), Vec2::new(w, h))
    }

    fn input(bar_px: f32) -> HudIn {
        HudIn { name_w: 150.0, scale_w: 36.0, fit_w: 40.0, bar_px, bar_label_w: 48.0 }
    }

    #[test]
    fn the_full_axes_key_belongs_to_one_held_command_that_a_press_never_runs() {
        let key = FULL_AXES_KEY.name();
        let reg = ddc_ui::Registry::builtin();
        for c in reg.all() {
            let holds = c.shortcuts.iter().any(|s| s == key);
            assert_eq!(holds, c.id == FULL_AXES_CMD, "{} / {key}", c.id);
        }
        assert!(reg.get(FULL_AXES_CMD).is_some_and(|c| c.hold));
        assert!(reg.by_shortcut(FULL_AXES_KEY, egui::Modifiers::NONE).is_none());
    }

    #[test]
    fn full_axes_follow_the_toggle_or_the_held_key() {
        assert!(!full_axes(false, false, false));
        assert!(full_axes(true, false, false), "toggle on");
        assert!(full_axes(false, true, false), "hold F9");
        assert!(!full_axes(false, true, true), "typing in a field ignores the hold");
        assert!(full_axes(true, false, true), "the toggle stays on while typing");
        assert!(!full_axes(false, false, false), "toggle off and released");
    }

    #[test]
    fn short_axes_stay_near_the_origin() {
        let r = canvas(1024.0, 600.0);
        for o in [r.center(), r.left_top() + Vec2::new(3.0, 3.0), r.right_bottom()] {
            let c = axis_cross(r, o, false);
            for ax in [c.x, c.y].into_iter().flatten() {
                let len = (ax.b - ax.a).length();
                assert!(len <= AXIS_ARM_PX + AXIS_TAIL_PX + 0.5, "{len} at {o:?}");
                assert!(r.contains(ax.a) && r.contains(ax.b) && r.contains(ax.label));
            }
        }
        assert_eq!(axis_cross(r, r.right_bottom() + Vec2::splat(5.0), false), AxisCross::default());
    }

    #[test]
    fn a_canvas_smaller_than_its_edges_still_draws_the_cross_inside() {
        for (w, h) in [(20.0, 500.0), (800.0, 20.0), (4.0, 4.0)] {
            let r = canvas(w, h);
            let c = axis_cross(r, r.center(), false);
            for ax in [c.x.unwrap(), c.y.unwrap()] {
                assert!(r.contains(ax.a) && r.contains(ax.b), "{w}x{h}: {ax:?}");
            }
        }
    }

    #[test]
    fn axis_points_to_positive_and_full_lines_span_the_sheet() {
        let r = canvas(800.0, 500.0);
        let o = r.center();
        let c = axis_cross(r, o, false);
        let (x, y) = (c.x.unwrap(), c.y.unwrap());
        assert!(x.b.x > o.x && x.a.x < o.x, "X runs right of the origin");
        assert!(y.b.y < o.y && y.a.y > o.y, "Y runs up the screen");
        let f = axis_cross(r, o, true);
        assert_eq!((f.x.unwrap().a.x, f.x.unwrap().b.x), (r.left(), r.right()));
        assert_eq!((f.y.unwrap().b.y, f.y.unwrap().a.y), (r.top(), r.bottom()));
    }

    #[test]
    fn axes_are_readable_but_quieter_than_the_wall_stroke() {
        for mode in [Mode::Light, Mode::Dark] {
            let t = tokens(mode);
            let wall = ddc_ui::theme::contrast(t.cut, t.paper);
            for i in 0..2 {
                let c = ddc_ui::theme::contrast(axis_on_paper(&t, i), t.paper);
                assert!(c >= 1.5, "{mode:?} axis {i} unreadable: {c}");
                assert!(c < wall, "{mode:?} axis {i} {c} outshines the wall {wall}");
            }
        }
    }

    fn assert_hud(r: Rect, hud: &Hud, tag: &str) {
        let boxes = [hud.name, hud.scale, hud.fit, hud.bar];
        for (i, a) in boxes.iter().enumerate() {
            if !shown(*a) {
                continue;
            }
            assert!(r.contains_rect(*a), "{tag}: box {i} {a:?} leaves {r:?}");
            for b in boxes.iter().skip(i + 1).filter(|b| shown(**b)) {
                assert!(!a.intersects(*b), "{tag}: {a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn hud_fits_wide_and_narrow_canvases_beside_the_bar() {
        for w in [1024.0, 640.0, 420.0, 300.0, 200.0] {
            let r = canvas(w, 500.0);
            let hud = layout(r, input(120.0));
            assert_hud(r, &hud, &format!("w={w}"));
            assert!(shown(hud.fit) || w < 300.0, "w={w}: fit chip dropped too early");
            for c in [hud.name, hud.scale, hud.fit].into_iter().filter(|c| shown(*c)) {
                assert_eq!(c.bottom(), hud.bar.bottom(), "w={w}: one baseline");
            }
        }
        let wide = layout(canvas(1024.0, 500.0), input(120.0));
        assert!(shown(wide.name) && shown(wide.scale) && shown(wide.fit));
    }

    #[test]
    fn plan_chip_shows_the_view_scale_and_zoom_apart() {
        // at the paper size of 1:100 the zoom is 100 %; zooming changes only the percentage
        let paper = 1000.0 / (100.0 * PX_MM);
        assert_eq!(view_scale_text(100.0, paper), "1:100 \u{b7} 100%");
        assert_eq!(view_scale_text(100.0, 2.0 * paper), "1:100 \u{b7} 200%");
        assert_eq!(view_scale_text(50.0, paper), "1:50 \u{b7} 50%");
    }

    #[test]
    fn scale_text_reads_as_a_ratio() {
        assert_eq!(scale_text(1000.0 / PX_MM), "1:1");
        assert!(paper_ratio(80.0) > paper_ratio(160.0));
    }
}
