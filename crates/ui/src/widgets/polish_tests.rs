// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules of the visual system, computed from the tokens: surface steps, text contrast, one
//! accent, distinct states, equal control heights, one radius per control class.
use super::button::{colors, Variant, Visual};
use super::*;
use crate::sem::{radius, AA_TEXT};
use crate::theme::{
    contrast, luminance, over_rgba, sem, ControlSize, Density, Mode, Role, Sem, Tokens, Weight,
};
use crate::Theme;
use egui::Color32;

const MODES: [Mode; 2] = [Mode::Light, Mode::Dark];

fn ready(mode: Mode, density: Density) -> (egui::Context, Theme) {
    let ctx = egui::Context::default();
    let mut th = Theme::new(mode, density);
    for _ in 0..2 {
        let _ = ctx.run(Default::default(), |c| {
            th.apply(c);
        });
    }
    (ctx, th)
}

/// Largest channel difference: states differ by hue too, which a luminance ratio does not see.
fn step(a: Color32, b: Color32) -> u8 {
    a.r().abs_diff(b.r()).max(a.g().abs_diff(b.g())).max(a.b().abs_diff(b.b()))
}

/// Chrome recedes, panels sit on it, dialogs and the drawing are lifted off the panels.
#[test]
fn surfaces_form_a_visible_ladder() {
    let (l, d) = (sem(false), sem(true));
    let paper = |m| Tokens::embedded().unwrap().color(m, "canvas.paper").unwrap();
    let lum = luminance;
    assert!(lum(l.bg_app) < lum(l.bg_panel));
    assert!(lum(l.bg_panel) < lum(l.bg_elevated));
    assert!(lum(l.bg_panel) < lum(paper(Mode::Light)), "light: the drawing is the brightest");
    assert!(lum(paper(Mode::Dark)) < lum(d.bg_app), "dark: the drawing is the deepest");
    assert!(lum(d.bg_app) < lum(d.bg_panel));
    assert!(lum(d.bg_panel) < lum(d.bg_elevated));
    for (a, b) in [
        (l.bg_app, l.bg_panel),
        (l.bg_panel, l.bg_elevated),
        (d.bg_app, d.bg_panel),
        (d.bg_panel, d.bg_elevated),
    ] {
        assert!(step(a, b) >= 6, "{a:?} and {b:?} read as one surface");
    }
}

/// Every text colour a type role can pick stays at AA on every opaque surface.
#[test]
fn role_text_passes_aa_on_every_surface() {
    let t = Tokens::embedded().unwrap();
    for mode in MODES {
        for name in ["bg.app", "bg.panel", "bg.elevated", "bg.input", "bg.sunken", "canvas.paper"] {
            let bg = t.color(mode, name).unwrap();
            for role in Role::ALL {
                let fg = t.color(mode, role.spec().color).unwrap();
                let r = contrast(fg, bg);
                assert!(r >= AA_TEXT, "{mode:?} {role:?} on {name}: {r:.2}");
            }
        }
    }
}

/// Selected, focused and active all use the one accent.
#[test]
fn one_accent_for_every_state() {
    for s in [sem(false), sem(true)] {
        assert_eq!(s.focus, s.accent);
        assert_eq!(s.selection, s.accent);
        assert_eq!(s.selection_text, s.accent);
        assert_eq!(s.selection_bg, s.accent_subtle);
    }
}

/// Rest, hover, pressed and selected are told apart on panels and on dialogs alike.
#[test]
fn state_fills_differ_on_every_surface() {
    for s in [sem(false), sem(true)] {
        for surface in [s.bg_panel, s.bg_elevated] {
            let hover = over_rgba(s.bg_hover, surface);
            let press = over_rgba(s.bg_pressed, surface);
            assert!(step(surface, hover) >= 8, "{surface:?} hover");
            assert!(step(hover, press) >= 8, "{surface:?} pressed");
            assert!(step(surface, s.accent_subtle) >= 8, "{surface:?} selected");
            assert!(step(hover, s.accent_subtle) >= 8, "{surface:?} selected and hover");
            for fill in [hover, press] {
                assert!(contrast(s.text_primary, fill) >= AA_TEXT);
                assert!(contrast(s.text_secondary, fill) >= AA_TEXT);
            }
            assert!(contrast(s.accent, s.accent_subtle) >= AA_TEXT);
        }
    }
}

/// The theme hands out the translucent fills, so no surface loses its hover state.
#[test]
fn theme_state_fills_are_translucent() {
    for mode in MODES {
        let th = Theme::new(mode, Density::Compact);
        assert_eq!(th.hover_fill(), th.sem().bg_hover);
        assert_eq!(th.pressed_fill(), th.sem().bg_pressed);
        assert!(th.hover_fill().a() < 255 && th.pressed_fill().a() < 255);
    }
}

/// A resting egui button is raised: its hover and press still step off that fill, dark included.
#[test]
fn egui_states_step_off_the_raised_rest() {
    for mode in MODES {
        let st = Theme::new(mode, Density::Compact).style();
        let w = &st.visuals.widgets;
        assert!(step(w.inactive.weak_bg_fill, w.hovered.weak_bg_fill) >= 8, "{mode:?} hover");
        assert!(step(w.hovered.weak_bg_fill, w.active.weak_bg_fill) >= 8, "{mode:?} pressed");
    }
}

fn secondary_rest(s: &Sem) -> Color32 {
    colors(s, Variant::Secondary, Visual::Rest).fill
}

/// A resting secondary button, the egui button and a segmented thumb are one raised surface.
#[test]
fn raised_controls_share_the_elevated_surface() {
    for mode in MODES {
        let th = Theme::new(mode, Density::Compact);
        let s = th.sem();
        assert_eq!(secondary_rest(s), s.bg_elevated);
        let st = th.style();
        assert_eq!(st.visuals.widgets.inactive.weak_bg_fill, s.bg_elevated);
        assert_eq!(st.visuals.window_fill, s.bg_elevated);
        assert_eq!(st.visuals.panel_fill, s.bg_panel);
        assert_eq!(st.visuals.window_stroke.color, s.border_overlay);
        let h = colors(s, Variant::Secondary, Visual::Hover);
        assert_eq!(h.fill, s.bg_hover);
        // v3 (2b70f8c8a): the secondary edge is border.control in every state
        assert_eq!(h.stroke, s.border_control);
    }
}

/// Buttons of every variant, text fields and segmented controls are one height per density.
#[test]
fn control_heights_are_equal() {
    for density in [Density::Compact, Density::Comfortable] {
        let (ctx, th) = ready(Mode::Light, density);
        let want = th.control_h(ControlSize::Medium);
        assert_eq!(want, th.d().control);
        assert_eq!(th.style().spacing.interact_size.y, want);
        let mut heights = Vec::new();
        let mut buf = String::new();
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                heights.push(("primary", Button::primary("a").show(ui, &th).rect.height()));
                heights.push(("second", Button::secondary("a").show(ui, &th).rect.height()));
                heights.push(("quiet", Button::quiet("a").show(ui, &th).rect.height()));
                heights.push(("danger", Button::danger("a").show(ui, &th).rect.height()));
                let field = ui.scope(|ui| {
                    TextField::new(&mut buf).id_salt("h").show(ui, &th);
                });
                heights.push(("field", field.response.rect.height()));
                let seg = ui.scope(|ui| {
                    segmented(ui, &th, &["a", "b"], 0);
                });
                heights.push(("segmented", seg.response.rect.height()));
            });
        });
        assert_eq!(heights.len(), 6);
        for (name, h) in heights {
            assert!((h - want).abs() < 0.5, "{density:?} {name}: {h} != {want}");
        }
    }
}

/// All interactive widgets share `md`; windows take `lg`; every radius is whole pixels.
#[test]
fn corner_radii_are_consistent() {
    assert!(radius::SM < radius::MD && radius::MD < radius::LG);
    for r in [radius::SM, radius::MD, radius::LG] {
        assert_eq!(r.fract(), 0.0, "{r}");
    }
    for mode in MODES {
        let th = Theme::new(mode, Density::Compact);
        let v = th.style().visuals;
        let md = egui::CornerRadius::same(radius::MD as u8);
        for w in [
            &v.widgets.noninteractive,
            &v.widgets.inactive,
            &v.widgets.hovered,
            &v.widgets.active,
            &v.widgets.open,
        ] {
            assert_eq!(w.corner_radius, md);
        }
        assert_eq!(v.menu_corner_radius, md);
        assert_eq!(v.window_corner_radius, egui::CornerRadius::same(radius::LG as u8));
    }
}

/// Elevation grows with its level and stays restrained on light and dark.
#[test]
fn shadows_are_restrained_and_ordered() {
    for mode in MODES {
        let th = Theme::new(mode, Density::Compact);
        let (a, b, c) = (th.shadow(1), th.shadow(2), th.shadow(3));
        assert!(a.blur < b.blur && b.blur < c.blur, "{mode:?}");
        assert!(a.offset[1] < b.offset[1] && b.offset[1] < c.offset[1], "{mode:?}");
        assert!(a.color.a() < b.color.a() && b.color.a() < c.color.a(), "{mode:?}");
        // the deepest shadow stays within its elevation token: no code path makes it louder
        let v: serde_json::Value = serde_json::from_str(crate::theme::TOKENS_JSON).unwrap();
        let key = if mode == Mode::Dark { "dark" } else { "light" };
        let alpha = v["elevation"][key]["3"]["alpha"].as_f64().unwrap();
        let cap = (alpha * 255.0).round() as u8;
        assert!(c.color.a() <= cap, "{mode:?} deepest shadow is loud: {}", c.color.a());
    }
}

/// Labels that organise the screen stay quieter than the content they head.
#[test]
fn structure_labels_are_quiet() {
    assert_eq!(Role::Section.spec().weight, Weight::Medium);
    assert_eq!(Role::GroupTitle.spec().weight, Weight::Regular);
    assert_eq!(Role::Keycap.spec().weight, Weight::Regular);
    for r in [Role::Section, Role::GroupTitle, Role::Keycap, Role::Status] {
        assert_eq!(r.spec().color, "text.secondary", "{r:?}");
    }
    assert_eq!(Role::DialogTitle.spec().size, Role::Title.spec().size);
}
