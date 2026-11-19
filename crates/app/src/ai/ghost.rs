// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The AI draft in the viewport "as it will land": the style of each change kind, the hold-B
//! "show Before" state, the pulse of a hovered card row, the edge arrow for a hot element that is
//! off screen and the legend chip. Everything here is pure or paints a copy: the model is never
//! touched by a preview.
use ddc_model::RowKind;
use ddc_ui::diff_kind::DiffKind;
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use ddc_ui::Theme;
use egui::{self, Color32, Key, Painter, Pos2, Rect, Shape, Stroke, Vec2};

/// Seconds for one pulse of a hovered element.
pub const PULSE_PERIOD_S: f64 = 1.2;
/// Distance of the edge arrow tip from the viewport border.
pub const ARROW_INSET_PX: f32 = 16.0;
/// Half the base and the length of the arrow head.
const ARROW_HALF_PX: f32 = 6.0;
const ARROW_LEN_PX: f32 = 12.0;
/// Ghost boxes drawn in 3D per frame.
pub const GHOST_BOXES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineForm {
    Solid,
    Dashed,
}

/// How one element of the draft is drawn: the outline form, its width and the fill strength.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GhostStyle {
    pub line: LineForm,
    pub width_px: f32,
    /// 0 = outline only, 1 = opaque.
    pub fill: f32,
}

const BASE_PX: f32 = 2.0;
const HOT_EXTRA_PX: f32 = 1.0;
const HOT_FILL_GAIN: f32 = 1.5;

/// Draft style by change kind. `None` while Before is shown: the model is drawn as it is and the
/// draft adds nothing on top.
pub fn ghost_style(kind: DiffKind, hot: bool, before_mode: bool) -> Option<GhostStyle> {
    if before_mode {
        return None;
    }
    let (line, fill) = match kind {
        // added: solid accent with a body
        DiffKind::Added => (LineForm::Solid, 0.30),
        // changed: the after state is filled, its before outline is `before_outline_style`
        DiffKind::Changed => (LineForm::Solid, 0.22),
        // deleted: a dashed ghost with only a faint tint
        DiffKind::Deleted => (LineForm::Dashed, 0.08),
        DiffKind::Refused | DiffKind::Conflict => (LineForm::Solid, 0.0),
    };
    Some(GhostStyle {
        line,
        width_px: BASE_PX + if hot { HOT_EXTRA_PX } else { 0.0 },
        fill: if hot { (fill * HOT_FILL_GAIN).min(1.0) } else { fill },
    })
}

/// The outline an element had before the change: thin, dashed, no fill.
pub fn before_outline_style(hot: bool, before_mode: bool) -> Option<GhostStyle> {
    if before_mode {
        return None;
    }
    Some(GhostStyle {
        line: LineForm::Dashed,
        width_px: if hot { BASE_PX } else { 1.0 },
        fill: 0.0,
    })
}

pub const fn kind_of(k: RowKind) -> DiffKind {
    crate::ai::preview::diff_kind(k)
}

/// Seconds of the single pulse a draft ghost makes when it appears.
pub const APPEAR_S: f64 = 0.6;
/// Streaming caret blink period in seconds.
pub const CARET_PERIOD_S: f64 = 1.0;

/// Strength 1.0..=1.5 of the one pulse `age` seconds after a ghost appears; flat 1.0 afterwards
/// and always flat with reduced motion.
pub fn appear_pulse(age: f64, reduced: bool) -> f32 {
    if reduced || !(0.0..APPEAR_S).contains(&age) {
        return 1.0;
    }
    1.0 + 0.5 * (std::f64::consts::PI * age / APPEAR_S).sin() as f32
}

/// The streaming caret: on for the first half of each period; steady with reduced motion.
pub fn caret_visible(t: f64, reduced: bool) -> bool {
    reduced || (t / CARET_PERIOD_S).fract() < 0.5
}

/// Pulse factor 0.6..=1.0 of a hovered element at time `t` seconds.
pub fn pulse(t: f64) -> f32 {
    let phase = (t / PULSE_PERIOD_S).fract() * std::f64::consts::TAU;
    0.8 + 0.2 * phase.sin() as f32
}

/// Fill colour of a style: the kind colour at the style's strength, scaled by the pulse.
pub fn fill_color(c: Color32, s: &GhostStyle, pulse: f32) -> Color32 {
    c.gamma_multiply((s.fill * pulse).clamp(0.0, 1.0))
}

/// A ring can take a filled polygon only when it is convex; others keep outline and hatch.
pub fn is_convex(ring: &[Pos2]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut sign = 0.0f32;
    for i in 0..n {
        let (a, b, c) = (ring[i], ring[(i + 1) % n], ring[(i + 2) % n]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    sign != 0.0
}

/// Where an off-screen point is announced: the tip sits inside the viewport, `dir` points at it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeArrow {
    pub tip: Pos2,
    pub dir: Vec2,
}

/// `None` while the point is inside the viewport; otherwise the arrow on the border, `inset`
/// inside it, pointing from the centre to the point.
pub fn edge_arrow(rect: Rect, pt: Pos2, inset: f32) -> Option<EdgeArrow> {
    if rect.contains(pt) || !pt.x.is_finite() || !pt.y.is_finite() {
        return None;
    }
    let inner = rect.shrink(inset.min(rect.width().min(rect.height()) * 0.5));
    let tip = Pos2::new(
        pt.x.clamp(inner.min.x, inner.max.x),
        pt.y.clamp(inner.min.y, inner.max.y),
    );
    let d = pt - rect.center();
    let len = d.length();
    let dir = if len > 1e-6 { d / len } else { Vec2::X };
    Some(EdgeArrow { tip, dir })
}

pub fn paint_arrow(p: &Painter, a: &EdgeArrow, col: Color32) {
    let n = Vec2::new(-a.dir.y, a.dir.x);
    let base = a.tip - a.dir * ARROW_LEN_PX;
    let tri = vec![a.tip, base + n * ARROW_HALF_PX, base - n * ARROW_HALF_PX];
    p.add(Shape::convex_polygon(
        tri,
        col,
        Stroke::new(1.0, col.gamma_multiply(0.6)),
    ));
}

/// What decides whether B shows Before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoldState {
    pub b_down: bool,
    pub no_modifiers: bool,
    /// A text field has focus: the letter belongs to it.
    pub text_focus: bool,
    /// The canvas owns the keyboard (no dialog, palette or menu).
    pub canvas_owns: bool,
    pub has_preview: bool,
}

pub const fn before_wanted(s: HoldState) -> bool {
    s.b_down && s.no_modifiers && !s.text_focus && s.canvas_owns && s.has_preview
}

/// B held on the canvas while a draft is on screen.
/// While a draft is shown, B belongs to the Before hold and never starts a key sequence.
pub fn holds_key(key: Key, has_preview: bool) -> bool {
    key == Key::B && has_preview
}

pub fn read_before(ctx: &egui::Context, has_preview: bool) -> bool {
    let (b_down, no_modifiers) = ctx.input(|i| (i.key_down(Key::B), i.modifiers.is_none()));
    before_wanted(HoldState {
        b_down,
        no_modifiers,
        text_focus: ctx.wants_keyboard_input(),
        canvas_owns: crate::ui_kit::canvas_owns_keys(ctx),
        has_preview,
    })
}

/// Chip at the viewport corner: the draft is a draft, or Before is held.
pub fn chip(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, before: bool) {
    let key = if before { "ai.ghost.before" } else { "ai.ghost.draft" };
    super::ui::kit::card(ui, th, |ui| {
        ui.label(
            egui::RichText::new(cat.t(key))
                .font(th.font(Role::Caption))
                .color(th.c("text.primary")),
        );
        if !before {
            ui.label(
                egui::RichText::new(cat.t("ai.ghost.hint"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOWN: [DiffKind; 3] = [DiffKind::Added, DiffKind::Changed, DiffKind::Deleted];

    fn rect() -> Rect {
        Rect::from_min_max(Pos2::new(100.0, 50.0), Pos2::new(500.0, 350.0))
    }

    #[test]
    fn deleted_is_a_dashed_ghost_and_added_is_solid() {
        let d = ghost_style(DiffKind::Deleted, false, false).unwrap();
        assert_eq!(d.line, LineForm::Dashed);
        assert!(d.fill > 0.0 && d.fill < 0.15, "a faint tint only");
        let a = ghost_style(DiffKind::Added, false, false).unwrap();
        assert_eq!(a.line, LineForm::Solid);
        assert!(a.fill > d.fill, "added is the solid accent");
    }

    #[test]
    fn changed_has_an_after_fill_and_a_dashed_before_outline() {
        let c = ghost_style(DiffKind::Changed, false, false).unwrap();
        assert!(c.fill > 0.0);
        let b = before_outline_style(false, false).unwrap();
        assert_eq!((b.line, b.fill), (LineForm::Dashed, 0.0));
    }

    #[test]
    fn hot_is_stronger_for_every_kind() {
        for k in SHOWN {
            let cold = ghost_style(k, false, false).unwrap();
            let hot = ghost_style(k, true, false).unwrap();
            assert!(hot.width_px > cold.width_px, "{k:?}");
            assert!(hot.fill > cold.fill && hot.fill <= 1.0, "{k:?}");
            assert_eq!(hot.line, cold.line);
        }
    }

    #[test]
    fn before_mode_draws_no_draft() {
        for k in DiffKind::ALL {
            for hot in [false, true] {
                assert_eq!(ghost_style(k, hot, true), None);
            }
        }
        assert_eq!(before_outline_style(true, true), None);
    }

    #[test]
    fn style_colour_comes_from_the_shared_kind() {
        assert_eq!(kind_of(RowKind::Created), DiffKind::Added);
        assert_eq!(kind_of(RowKind::Changed), DiffKind::Changed);
        assert_eq!(kind_of(RowKind::Deleted), DiffKind::Deleted);
    }

    #[test]
    fn appear_pulse_happens_once_and_never_with_reduced_motion() {
        assert!(appear_pulse(APPEAR_S / 2.0, false) > 1.4);
        assert_eq!(appear_pulse(APPEAR_S + 0.01, false), 1.0);
        assert_eq!(appear_pulse(-1.0, false), 1.0);
        for k in 0..20 {
            assert_eq!(appear_pulse(k as f64 * 0.03, true), 1.0);
        }
    }

    #[test]
    fn caret_blinks_each_second_and_stays_on_when_calm() {
        assert!(caret_visible(0.2, false) && !caret_visible(0.7, false) && caret_visible(1.2, false));
        assert!(caret_visible(0.7, true));
    }

    #[test]
    fn pulse_stays_in_range_and_repeats() {
        for i in 0..200 {
            let t = f64::from(i) * 0.037;
            let p = pulse(t);
            assert!((0.6..=1.0).contains(&p), "{p}");
            assert!((p - pulse(t + PULSE_PERIOD_S)).abs() < 1e-3);
        }
    }

    #[test]
    fn fill_scales_with_pulse_and_never_exceeds_the_colour() {
        let s = ghost_style(DiffKind::Added, false, false).unwrap();
        let c = Color32::from_rgb(200, 40, 40);
        let lo = fill_color(c, &s, 0.6).a();
        let hi = fill_color(c, &s, 1.0).a();
        assert!(lo <= hi && hi > 0);
        let none = GhostStyle { fill: 0.0, ..s };
        assert_eq!(fill_color(c, &none, 1.0).a(), 0);
    }

    #[test]
    fn convexity() {
        let sq = [
            Pos2::new(0.0, 0.0),
            Pos2::new(4.0, 0.0),
            Pos2::new(4.0, 4.0),
            Pos2::new(0.0, 4.0),
        ];
        assert!(is_convex(&sq));
        let l = [
            Pos2::new(0.0, 0.0),
            Pos2::new(4.0, 0.0),
            Pos2::new(4.0, 2.0),
            Pos2::new(2.0, 2.0),
            Pos2::new(2.0, 4.0),
            Pos2::new(0.0, 4.0),
        ];
        assert!(!is_convex(&l));
        assert!(!is_convex(&sq[..2]));
    }

    #[test]
    fn no_arrow_on_screen() {
        let r = rect();
        assert_eq!(edge_arrow(r, r.center(), ARROW_INSET_PX), None);
        assert_eq!(edge_arrow(r, r.left_top(), ARROW_INSET_PX), None);
        assert_eq!(edge_arrow(r, Pos2::new(f32::NAN, 0.0), ARROW_INSET_PX), None);
    }

    #[test]
    fn arrow_sits_on_the_side_of_the_point() {
        let r = rect();
        let i = ARROW_INSET_PX;
        let left = edge_arrow(r, Pos2::new(0.0, 200.0), i).unwrap();
        assert_eq!(left.tip, Pos2::new(r.min.x + i, 200.0));
        assert!(left.dir.x < -0.99);
        let right = edge_arrow(r, Pos2::new(900.0, 200.0), i).unwrap();
        assert_eq!(right.tip, Pos2::new(r.max.x - i, 200.0));
        assert!(right.dir.x > 0.99);
        let top = edge_arrow(r, Pos2::new(300.0, -40.0), i).unwrap();
        assert_eq!(top.tip, Pos2::new(300.0, r.min.y + i));
        assert!(top.dir.y < -0.9);
        let bottom = edge_arrow(r, Pos2::new(300.0, 900.0), i).unwrap();
        assert_eq!(bottom.tip, Pos2::new(300.0, r.max.y - i));
        assert!(bottom.dir.y > 0.9);
    }

    #[test]
    fn arrow_in_a_corner_clamps_both_axes() {
        let r = rect();
        let i = ARROW_INSET_PX;
        let corners = [
            (Pos2::new(0.0, 0.0), Pos2::new(r.min.x + i, r.min.y + i), -1.0, -1.0),
            (Pos2::new(900.0, 0.0), Pos2::new(r.max.x - i, r.min.y + i), 1.0, -1.0),
            (Pos2::new(0.0, 900.0), Pos2::new(r.min.x + i, r.max.y - i), -1.0, 1.0),
            (Pos2::new(900.0, 900.0), Pos2::new(r.max.x - i, r.max.y - i), 1.0, 1.0),
        ];
        for (pt, tip, sx, sy) in corners {
            let a = edge_arrow(r, pt, i).unwrap();
            assert_eq!(a.tip, tip);
            assert!(a.dir.x * sx > 0.0 && a.dir.y * sy > 0.0, "{pt:?} {:?}", a.dir);
            assert!((a.dir.length() - 1.0).abs() < 1e-4);
            assert!(r.contains(a.tip));
        }
    }

    #[test]
    fn hold_b_needs_a_free_canvas_and_a_draft() {
        let ok = HoldState {
            b_down: true,
            no_modifiers: true,
            text_focus: false,
            canvas_owns: true,
            has_preview: true,
        };
        assert!(before_wanted(ok));
        assert!(!before_wanted(HoldState { b_down: false, ..ok }));
        assert!(!before_wanted(HoldState { no_modifiers: false, ..ok }));
        assert!(!before_wanted(HoldState { canvas_owns: false, ..ok }));
        assert!(!before_wanted(HoldState { has_preview: false, ..ok }));
    }

    #[test]
    fn b_never_fires_while_a_text_field_has_focus() {
        let typing = HoldState {
            b_down: true,
            no_modifiers: true,
            text_focus: true,
            canvas_owns: true,
            has_preview: true,
        };
        assert!(!before_wanted(typing));
    }

    #[test]
    fn b_is_no_single_key_shortcut_and_is_held_back_from_sequences() {
        let reg = ddc_ui::Registry::builtin();
        assert!(reg.by_shortcut(Key::B, egui::Modifiers::NONE).is_none());
        assert!(reg.all().iter().all(|c| !c.shortcuts.iter().any(|s| s == "B")));
        // B-mnemonics (BM, BR, ...) exist: with a draft shown, B is the Before key only
        assert!(holds_key(Key::B, true));
        assert!(!holds_key(Key::B, false));
        assert!(!holds_key(Key::A, true));
    }

    #[test]
    fn strings_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = Catalog::new(lang);
            for key in ["ai.ghost.draft", "ai.ghost.before", "ai.ghost.hint"] {
                assert_ne!(cat.t(key), key, "{key} in {lang:?}");
            }
        }
    }
}
