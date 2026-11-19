// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Grid bubbles that stay readable at any zoom: the label is fitted into the circle (font shrinks
//! to a floor, then the name is elided with a tooltip) and an end that left the canvas keeps its
//! bubble pinned to the canvas edge along the grid line.

use crate::overlay::{self, Palette};
use ddc_ui::sem::{space, type_size};
use ddc_ui::theme::W_SEMIBOLD;
use egui::{self, Align2, FontId, Pos2, Rect, Stroke, Vec2};

const R: f32 = overlay::BUBBLE_R;
/// Smallest legible label size: the canvas micro role (the type-scale floor).
pub(super) const MIN_FONT: f32 = ddc_ui::theme::Role::CanvasMicro.spec().size;
const FONT_STEP: f32 = 0.5;

/// Label that fits into the bubble.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Fit {
    pub font: f32,
    pub text: String,
    pub elided: bool,
}

/// Text width available inside the circle.
pub(super) fn inner_width() -> f32 {
    2.0 * R - space::XS
}

/// Largest font in `[min_font, max_font]` at which `name` fits `max_w`; at the floor the name is
/// cut and ends with an ellipsis. `measure(text, font)` is the text width in points.
pub(super) fn fit_label(
    name: &str,
    max_w: f32,
    min_font: f32,
    max_font: f32,
    measure: &dyn Fn(&str, f32) -> f32,
) -> Fit {
    let mut font = max_font;
    while font >= min_font {
        if measure(name, font) <= max_w {
            return Fit { font, text: name.to_owned(), elided: false };
        }
        font -= FONT_STEP;
    }
    let chars: Vec<char> = name.chars().collect();
    for n in (0..chars.len()).rev() {
        let mut s: String = chars[..n].iter().collect();
        s.push('\u{2026}');
        if measure(&s, min_font) <= max_w {
            return Fit { font: min_font, text: s, elided: true };
        }
    }
    Fit { font: min_font, text: "\u{2026}".to_owned(), elided: true }
}

/// Point of the segment `a -> b` nearest to `b` that lies in `rect` shrunk by `inset`; `None`
/// when the segment does not touch that rectangle (grid fully off-screen).
pub(super) fn pin_to_rect(a: Pos2, b: Pos2, rect: Rect, inset: f32) -> Option<Pos2> {
    let r = rect.shrink(inset);
    if r.width() < 0.0 || r.height() < 0.0 {
        return None;
    }
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q, lo, hi) in [(a.x, d.x, r.min.x, r.max.x), (a.y, d.y, r.min.y, r.max.y)] {
        if q.abs() < 1e-9 {
            if p < lo || p > hi {
                return None;
            }
        } else {
            let (u, v) = ((lo - p) / q, (hi - p) / q);
            let (u, v) = if u < v { (u, v) } else { (v, u) };
            t0 = t0.max(u);
            t1 = t1.min(v);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(a + d * t1)
}

/// Centre of the bubble for the end `b` of the line `a -> b` and whether it is pinned.
/// Visible end: one radius beyond it along the line. Otherwise the bubble sits on the line at
/// the canvas edge, inset by one radius.
pub(super) fn end_bubble(a: Pos2, b: Pos2, rect: Rect) -> Option<(Pos2, bool)> {
    let d = b - a;
    if d.length() < 1e-3 {
        return None;
    }
    let c = b + d.normalized() * R;
    if rect.shrink(R).contains(c) {
        return Some((c, false));
    }
    if let Some(p) = pin_to_rect(a, b, rect, R) {
        return Some((p, true));
    }
    // The line runs within one radius of the edge: pin on the visible part, then pull the centre
    // into the inset rectangle so the bubble stays whole and the line still crosses it.
    let p = pin_to_rect(a, b, rect, 0.0)?;
    Some((clamp_into(p, rect.shrink(R), rect.center()), true))
}

/// `p` clamped into `r`; a degenerate `r` (tiny canvas) gives `fallback`.
fn clamp_into(p: Pos2, r: Rect, fallback: Pos2) -> Pos2 {
    if r.width() < 0.0 || r.height() < 0.0 {
        return fallback;
    }
    Pos2::new(p.x.clamp(r.min.x, r.max.x), p.y.clamp(r.min.y, r.max.y))
}

/// One bubble to lay out: `a` is the opposite end of the line, `b` the end the bubble belongs to.
#[derive(Debug, Clone, Copy)]
pub(super) struct End {
    pub a: Pos2,
    pub b: Pos2,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Spot {
    /// Position before overlap separation.
    pub at: Pos2,
    /// One-radius step used to separate it (outward; inward along the line for pinned).
    pub out: Vec2,
    pub centre: Pos2,
    pub pinned: bool,
}

/// Spots for `ends` in order (`None`: the line is entirely off the canvas). Bubbles that would
/// touch move along their own line: outward when free, inward when pinned.
pub(super) fn layout(ends: &[End], rect: Rect) -> Vec<Option<Spot>> {
    let mut idx = Vec::with_capacity(ends.len());
    let mut raw = Vec::with_capacity(ends.len());
    let mut pin = Vec::with_capacity(ends.len());
    for (k, e) in ends.iter().enumerate() {
        if let Some((c, pinned)) = end_bubble(e.a, e.b, rect) {
            let dir = (e.b - e.a).normalized() * R;
            idx.push(k);
            raw.push((c, if pinned { -dir } else { dir }));
            pin.push(pinned);
        }
    }
    let centres = super::ui::place_bubbles(&raw);
    let mut out = vec![None; ends.len()];
    for (j, &k) in idx.iter().enumerate() {
        out[k] = Some(Spot { at: raw[j].0, out: raw[j].1, centre: centres[j], pinned: pin[j] });
    }
    out
}

/// Bubble with a fitted label, pixel-snapped. An elided name shows in full in a tooltip.
pub(super) fn paint(
    p: &egui::Painter,
    c: Pos2,
    name: &str,
    st: Stroke,
    ink: egui::Color32,
    ov: &Palette,
) {
    let ppp = p.ctx().pixels_per_point();
    let c = Pos2::new(ddc_ui::px::snap(c.x, ppp), ddc_ui::px::snap(c.y, ppp));
    p.circle(c, R, ov.paper, st);
    let fam = ddc_ui::theme::family(W_SEMIBOLD);
    let measure = |s: &str, f: f32| {
        p.layout_no_wrap(s.to_owned(), FontId::new(f, fam.clone()), ink).size().x
    };
    let fit = fit_label(name, inner_width(), MIN_FONT, type_size::CAPTION, &measure);
    p.text(c, Align2::CENTER_CENTER, &fit.text, FontId::new(fit.font, fam), ink);
    if fit.elided {
        let ctx = p.ctx();
        let hot = ctx.input(|i| i.pointer.hover_pos()).filter(|h| h.distance(c) <= R);
        if let Some(h) = hot {
            if ctx.layer_id_at(h) == Some(p.layer_id()) {
                let id = egui::Id::new(("grid_bubble", name));
                egui::Tooltip::always_open(ctx.clone(), p.layer_id(), id, egui::PopupAnchor::Pointer)
                    .show(|ui| {
                        ui.label(name);
                    });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    fn rect() -> Rect {
        Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0))
    }

    /// Fake metrics: every char is `0.6 * font` wide.
    fn m(s: &str, f: f32) -> f32 {
        s.chars().count() as f32 * f * 0.6
    }

    #[test]
    fn short_name_keeps_full_size() {
        let f = fit_label("A", inner_width(), MIN_FONT, type_size::CAPTION, &m);
        assert_eq!(f.font, type_size::CAPTION);
        assert_eq!(f.text, "A");
        assert!(!f.elided);
    }

    #[test]
    fn medium_name_shrinks_without_eliding() {
        let f = fit_label("A.1", inner_width(), MIN_FONT, type_size::CAPTION, &m);
        assert!(f.font < type_size::CAPTION && f.font >= MIN_FONT, "{f:?}");
        assert!(!f.elided);
        assert!(m(&f.text, f.font) <= inner_width());
    }

    #[test]
    fn long_name_is_elided_at_the_floor() {
        let f = fit_label("Axis-12345", inner_width(), MIN_FONT, type_size::CAPTION, &m);
        assert_eq!(f.font, MIN_FONT);
        assert!(f.elided);
        assert!(f.text.ends_with('\u{2026}'));
        assert!(m(&f.text, f.font) <= inner_width());
    }

    #[test]
    fn nothing_fits_gives_bare_ellipsis() {
        let f = fit_label("abc", 1.0, MIN_FONT, type_size::CAPTION, &m);
        assert_eq!(f.text, "\u{2026}");
        assert!(f.elided);
    }

    #[test]
    fn pin_each_side() {
        let (r, i) = (rect(), 11.0);
        let a = pos2(100.0, 50.0);
        let cases = [
            (pos2(400.0, 50.0), pos2(189.0, 50.0)),
            (pos2(-300.0, 50.0), pos2(11.0, 50.0)),
            (pos2(100.0, -200.0), pos2(100.0, 11.0)),
            (pos2(100.0, 500.0), pos2(100.0, 89.0)),
        ];
        for (b, want) in cases {
            let got = pin_to_rect(a, b, r, i).unwrap();
            assert!(got.distance(want) < 1e-3, "{b:?}: {got:?} != {want:?}");
            // the other end of the same line pinned from the opposite side
            let rev = pin_to_rect(b, a, r, i).unwrap();
            assert!(rev.distance(a) < 1e-3);
        }
    }

    #[test]
    fn pin_corner_and_rotated() {
        let r = rect();
        let a = pos2(100.0, 50.0);
        let corner = pin_to_rect(a, a + vec2(89.0, 39.0) * 3.0, r, 11.0).unwrap();
        assert!(corner.distance(pos2(189.0, 89.0)) < 1e-2, "{corner:?}");
        // 30 degrees up-right: leaves through the top edge, stays on the line
        let b = a + vec2(30f32.to_radians().cos(), -30f32.to_radians().sin()) * 300.0;
        let got = pin_to_rect(a, b, r, 11.0).unwrap();
        assert!((got.y - 11.0).abs() < 1e-3, "{got:?}");
        let cross = (got - a).x * (b - a).y - (got - a).y * (b - a).x;
        assert!(cross.abs() < 1e-2, "off the line: {cross}");
    }

    #[test]
    fn end_inside_is_unchanged_and_off_screen_is_none() {
        let r = rect();
        assert_eq!(pin_to_rect(pos2(20.0, 20.0), pos2(60.0, 40.0), r, 11.0), Some(pos2(60.0, 40.0)));
        assert_eq!(pin_to_rect(pos2(-50.0, 10.0), pos2(-10.0, 90.0), r, 11.0), None);
        // passes beside the corner
        assert_eq!(pin_to_rect(pos2(250.0, 0.0), pos2(300.0, 200.0), r, 11.0), None);
        assert!(end_bubble(pos2(-50.0, 10.0), pos2(-10.0, 90.0), r).is_none());
    }

    #[test]
    fn both_ends_visible_gives_normal_bubbles() {
        let (a, b) = (pos2(50.0, 50.0), pos2(150.0, 50.0));
        let (c, pinned) = end_bubble(a, b, rect()).unwrap();
        assert!(!pinned);
        assert!(c.distance(pos2(161.0, 50.0)) < 1e-3);
        let (c, pinned) = end_bubble(b, a, rect()).unwrap();
        assert!(!pinned && c.distance(pos2(39.0, 50.0)) < 1e-3);
    }

    #[test]
    fn off_screen_end_is_pinned_both_axes() {
        let r = rect();
        let vert = end_bubble(pos2(100.0, 50.0), pos2(100.0, -400.0), r).unwrap();
        assert!(vert.1 && vert.0.distance(pos2(100.0, 11.0)) < 1e-3);
        let hor = end_bubble(pos2(100.0, 50.0), pos2(900.0, 50.0), r).unwrap();
        assert!(hor.1 && hor.0.distance(pos2(189.0, 50.0)) < 1e-3);
    }

    /// Distance from `p` to the infinite line through `a` and `b`.
    fn dist_to_line(p: Pos2, a: Pos2, b: Pos2) -> f32 {
        let d = b - a;
        ((p - a).x * d.y - (p - a).y * d.x).abs() / d.length()
    }

    fn assert_whole_bubble(c: Pos2, r: Rect) {
        assert!(r.shrink(R).contains(c), "bubble {c:?} not whole in {r:?}");
    }

    #[test]
    fn line_fully_inside_has_unpinned_bubbles() {
        let (a, b) = (pos2(60.0, 30.0), pos2(140.0, 70.0));
        assert!(!end_bubble(a, b, rect()).unwrap().1);
        assert!(!end_bubble(b, a, rect()).unwrap().1);
    }

    #[test]
    fn vertical_line_near_left_edge_keeps_its_bubble() {
        let r = Rect::from_min_max(pos2(238.0, 0.0), pos2(638.0, 300.0));
        let (a, b) = (pos2(240.0, 250.0), pos2(240.0, -400.0));
        let (c, pinned) = end_bubble(a, b, r).unwrap();
        assert!(pinned);
        assert_whole_bubble(c, r);
        assert!(dist_to_line(c, a, b) <= R, "line misses its bubble: {c:?}");
    }

    #[test]
    fn line_near_each_edge_keeps_its_bubble() {
        let r = rect();
        let cases = [
            (pos2(100.0, 50.0), pos2(198.0, 50.0)),
            (pos2(100.0, 50.0), pos2(2.0, 50.0)),
            (pos2(100.0, 98.0), pos2(1000.0, 98.0)),
            (pos2(100.0, 2.0), pos2(-900.0, 2.0)),
            (pos2(2.0, 90.0), pos2(2.0, -900.0)),
            (pos2(198.0, 10.0), pos2(198.0, 900.0)),
        ];
        for (a, b) in cases {
            let (c, pinned) = end_bubble(a, b, r).unwrap();
            assert!(pinned, "{a:?} -> {b:?}");
            assert_whole_bubble(c, r);
            assert!(dist_to_line(c, a, b) <= R, "{a:?} -> {b:?}: {c:?}");
        }
    }

    #[test]
    fn nearly_parallel_to_edge_keeps_its_bubble() {
        let r = rect();
        let (a, b) = (pos2(2.0, 90.0), pos2(3.0, -900.0));
        let (c, pinned) = end_bubble(a, b, r).unwrap();
        assert!(pinned);
        assert_whole_bubble(c, r);
        assert!(dist_to_line(c, a, b) <= R, "{c:?}");
    }

    #[test]
    fn line_outside_the_rect_has_no_bubble() {
        let r = Rect::from_min_max(pos2(238.0, 0.0), pos2(638.0, 300.0));
        assert!(end_bubble(pos2(230.0, 250.0), pos2(230.0, -400.0), r).is_none());
    }

    #[test]
    fn tiny_rect_still_gives_a_bubble_on_the_line() {
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0));
        let (a, b) = (pos2(5.0, 5.0), pos2(5.0, -400.0));
        let (c, pinned) = end_bubble(a, b, r).unwrap();
        assert!(pinned && r.contains(c), "{c:?}");
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 40.0));
        let (c, _) = end_bubble(pos2(1.0, 20.0), pos2(1.0, -400.0), r).unwrap();
        assert!(r.contains(c) && dist_to_line(c, pos2(1.0, 20.0), pos2(1.0, -400.0)) <= R);
    }

    #[test]
    fn pinned_bubbles_do_not_overlap() {
        // six vertical axes 6 px apart, all running off the top edge
        let ends: Vec<End> = (0..6)
            .map(|i| End {
                a: pos2(60.0 + 6.0 * i as f32, 80.0),
                b: pos2(60.0 + 6.0 * i as f32, -500.0),
            })
            .collect();
        let spots = layout(&ends, rect());
        let c: Vec<Pos2> = spots.iter().map(|s| s.unwrap().centre).collect();
        assert!(spots.iter().all(|s| s.unwrap().pinned));
        for i in 0..c.len() {
            for j in 0..i {
                assert!(c[i].distance(c[j]) >= 2.0 * R, "{i} {j}: {c:?}");
            }
            assert!((c[i].x - ends[i].a.x).abs() < 1e-3, "moved off its line");
            assert!(c[i].y >= 11.0 - 1e-3, "left the canvas inset");
        }
    }
}
