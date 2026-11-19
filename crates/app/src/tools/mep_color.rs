//! Plan colour of a building-services system, readable on the paper of either theme.
//! The stored colour is the system's identity; what is drawn comes from the theme tokens
//! `mep.<kind>` for the default systems and from a contrast fix for any custom colour.

use crate::ui_kit::Tokens;
use ddc_model::SystemKind;
use ddc_ui::theme::{contrast, Mode, Tokens as DesignTokens};
use egui::Color32;
use std::sync::OnceLock;

/// Smallest contrast of a plan line against the paper (WCAG graphics).
const MIN_CONTRAST: f64 = 3.0;

/// Token name of a system kind.
pub(crate) fn token_name(kind: SystemKind) -> String {
    format!("mep.{}", kind.name())
}

/// Colour of a default system kind in a theme.
pub(crate) fn kind_color(kind: SystemKind, dark: bool) -> Color32 {
    static TABLE: OnceLock<[Vec<(SystemKind, Color32)>; 2]> = OnceLock::new();
    let tab = TABLE.get_or_init(|| {
        let t = DesignTokens::embedded().expect("embedded tokens.json is validated by tests");
        let build = |mode: Mode| {
            SystemKind::ALL
                .iter()
                .map(|&k| (k, token_or_identity(k, t.color(mode, &token_name(k)))))
                .collect::<Vec<_>>()
        };
        [build(Mode::Light), build(Mode::Dark)]
    });
    tab[usize::from(dark)]
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(Color32::GRAY, |(_, c)| *c) // design-lint: data
}

/// The theme colour of `kind`, or its stored identity colour with a logged warning when the
/// token is missing: a gap in the design tokens must never take the plan down.
fn token_or_identity<E: std::fmt::Display>(kind: SystemKind, token: Result<Color32, E>) -> Color32 {
    token.unwrap_or_else(|e| {
        let name = token_name(kind);
        eprintln!("warning: system token {name} is missing ({e}); using the system colour");
        let [r, g, b] = kind.color();
        Color32::from_rgb(r, g, b) // design-lint: data
    })
}

/// A custom colour pulled toward black (light paper) or white (dark paper) in 5 % steps until it
/// reaches the minimum contrast; a colour that already does is returned unchanged.
pub(crate) fn readable_on(rgb: [u8; 3], paper: Color32) -> Color32 {
    let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let dark = paper.r() < 128;
    let target = if dark { Color32::WHITE } else { Color32::BLACK }; // design-lint: data
    let mix = |a: u8, b: u8, t: f32| (f32::from(a) * (1.0 - t) + f32::from(b) * t).round() as u8;
    for step in 0..=20u8 {
        let t = f32::from(step) * 0.05;
        let k = Color32::from_rgb(
            mix(c.r(), target.r(), t),
            mix(c.g(), target.g(), t),
            mix(c.b(), target.b(), t),
        );
        if contrast(k, paper) >= MIN_CONTRAST {
            return k;
        }
    }
    target
}

/// Colour to draw a system with stored colour `rgb` on the plan of the theme of `t`.
pub(crate) fn plan(rgb: [u8; 3], t: &Tokens) -> Color32 {
    let dark = t.paper.r() < 128;
    match SystemKind::ALL.iter().find(|k| k.color() == rgb) {
        Some(&k) => kind_color(k, dark),
        None => readable_on(rgb, t.paper),
    }
}

/// Side of the system swatch in a picker.
pub(crate) const SWATCH_PX: f32 = 10.0;

/// The active app theme (mode, density, accent and scale the user picked), never a guess
/// from the paper colour.
pub(crate) fn active_theme(ui: &egui::Ui) -> std::sync::Arc<ddc_ui::Theme> {
    ddc_ui::theme::of(ui.ctx())
}

/// What a picker row shows in front of its label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// The stored colour of a system.
    Color([u8; 3]),
    /// Draw into a new system.
    Plus,
    /// No system.
    Dash,
    /// The stored id names a system that is gone: drawn in the warning tone.
    Missing,
}

/// Catalog key of the row of a system id that is no longer in the model.
pub(crate) const MISSING_KEY: &str = "mep_color.system.missing";

/// Text of the missing-system row, "Missing system (#404)", in an explicit language.
pub(crate) fn missing_label(ru: bool, id: i64) -> String {
    crate::tr_lang(ru, MISSING_KEY, &[("id", &id.to_string())])
}

/// `rows` with a missing-system row on top when `sel` names an id no row has, so the closed
/// picker shows the stale id instead of the first row, and picking that first row is a change.
pub(crate) fn with_missing(rows: &[SysRow], sel: Option<i64>, label: &str) -> Vec<SysRow> {
    let mut out = rows.to_vec();
    if let Some(id) = sel.filter(|id| !rows.iter().any(|r| r.id == Some(*id))) {
        out.insert(0, SysRow { id: Some(id), label: label.to_owned(), mark: Mark::Missing });
    }
    out
}

/// Writes the row at `ix` into `sel` when the picker reported a change; true when it did.
fn apply_pick(sel: &mut Option<i64>, rows: &[SysRow], ix: usize, changed: bool) -> bool {
    match rows.get(ix) {
        Some(r) if changed && r.id != *sel => {
            *sel = r.id;
            true
        }
        _ => false,
    }
}

/// One row of a system picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SysRow {
    pub id: Option<i64>,
    pub label: String,
    pub mark: Mark,
}

/// Picker rows: the first row (new system or none) and then the systems in id order.
pub(crate) fn system_items(systems: &[ddc_model::MepSystem], first: &str, mark: Mark) -> Vec<SysRow> {
    let mut sorted: Vec<&ddc_model::MepSystem> = systems.iter().collect();
    sorted.sort_by_key(|s| s.id);
    let mut rows = vec![SysRow { id: None, label: first.to_owned(), mark }];
    rows.extend(sorted.into_iter().map(|s| SysRow {
        id: Some(s.id),
        label: s.name.clone(),
        mark: Mark::Color(s.color),
    }));
    rows
}

/// Swatch colour of a row on `paper`; the none and new rows have no swatch colour.
pub(crate) fn swatch_color(mark: Mark, paper: Color32) -> Option<Color32> {
    match mark {
        Mark::Color(rgb) => Some(readable_on(rgb, paper)),
        Mark::Plus | Mark::Dash | Mark::Missing => None,
    }
}

fn paint_mark(ui: &mut egui::Ui, th: &ddc_ui::Theme, mark: Mark, paper: Color32) {
    use egui::{vec2, Sense, Stroke, StrokeKind};
    let (r, _) = ui.allocate_exact_size(vec2(SWATCH_PX, SWATCH_PX), Sense::hover());
    if mark == Mark::Missing {
        let st = Stroke::new(1.5, th.sem().warning);
        ui.painter().rect_stroke(r.shrink(0.75), th.radius("pill").min(SWATCH_PX / 2.0), st, StrokeKind::Inside);
        let (a, b) = (r.center() - vec2(0.0, SWATCH_PX / 4.0), r.center() + vec2(0.0, SWATCH_PX / 8.0));
        ui.painter().line_segment([a, b], st);
        return;
    }
    match swatch_color(mark, paper) {
        Some(c) => {
            ui.painter().rect_filled(r, th.radius("pill").min(SWATCH_PX / 2.0), c);
        }
        None => {
            let st = Stroke::new(1.5, th.sem().text_secondary);
            let (a, b) = (r.center() - vec2(SWATCH_PX / 2.0, 0.0), r.center() + vec2(SWATCH_PX / 2.0, 0.0));
            ui.painter().line_segment([a, b], st);
            if mark == Mark::Plus {
                let (c, d) = (r.center() - vec2(0.0, SWATCH_PX / 2.0), r.center() + vec2(0.0, SWATCH_PX / 2.0));
                ui.painter().line_segment([c, d], st);
            }
        }
    }
}

/// System dropdown of the kit look (row height, `DROPDOWN_W`) with a swatch on the closed
/// control and on every row. Returns true when the choice changed.
pub(crate) fn system_dropdown(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    id_salt: &str,
    sel: &mut Option<i64>,
    rows: &[SysRow],
    paper: Color32,
) -> bool {
    system_dropdown_w(ui, th, id_salt, sel, rows, paper, ddc_ui::theme::control::DROPDOWN_W + SWATCH_PX + ddc_ui::theme::space::XS)
}

/// `system_dropdown` whose swatch and control together take `total` points (a property column).
pub(crate) fn system_dropdown_w(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    id_salt: &str,
    sel: &mut Option<i64>,
    rows: &[SysRow],
    paper: Color32,
    total: f32,
) -> bool {
    let missing = sel.map(|id| missing_label(crate::ui_lang_now(), id)).unwrap_or_default();
    system_dropdown_in(ui, th, id_salt, sel, rows, paper, total, &missing)
}

/// `system_dropdown_w` with the missing-system text given, so tests pick the language.
#[allow(clippy::too_many_arguments)]
fn system_dropdown_in(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    id_salt: &str,
    sel: &mut Option<i64>,
    rows: &[SysRow],
    paper: Color32,
    total: f32,
    missing: &str,
) -> bool {
    use ddc_ui::theme::space;
    let combo_w = (total - SWATCH_PX - space::XS).max(SWATCH_PX * 4.0);
    let rows = with_missing(rows, *sel, missing);
    let rows = rows.as_slice();
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    let mut ix = rows.iter().position(|r| r.id == *sel).unwrap_or(0);
    let mut changed = false;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        if let Some(r) = rows.get(ix) {
            paint_mark(ui, th, r.mark, paper);
        }
        let mark = |ui: &mut egui::Ui, i: usize| {
            if let Some(r) = rows.get(i) {
                paint_mark(ui, th, r.mark, paper);
            }
        };
        let r = ddc_ui::widgets::dropdown_marked(
            ui,
            th,
            id_salt,
            &mut ix,
            &labels,
            Some(combo_w),
            None,
            Some(&mark),
        );
        changed = r.changed();
    });
    apply_pick(sel, rows, ix, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_rows_keep_id_order_with_the_first_row_on_top() {
        let sys = |id: i64, name: &str, kind: SystemKind| ddc_model::MepSystem {
            id,
            name: name.to_owned(),
            kind,
            color: kind.color(),
        };
        let list = [sys(9, "C", SystemKind::ALL[0]), sys(2, "A", SystemKind::ALL[1]), sys(5, "B", SystemKind::ALL[0])];
        let rows = system_items(&list, "first", Mark::Plus);
        let ids: Vec<Option<i64>> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![None, Some(2), Some(5), Some(9)]);
        assert_eq!((rows[0].label.as_str(), rows[0].mark), ("first", Mark::Plus));
        assert_eq!(rows[1].mark, Mark::Color(SystemKind::ALL[1].color()));
        assert_eq!(system_items(&list, "first", Mark::Plus), rows, "stable order");
    }

    #[test]
    fn swatch_follows_the_system_colour_and_the_theme() {
        let light = Color32::from_rgb(0xFB, 0xFB, 0xFA);
        let dark = Color32::from_rgb(0x14, 0x16, 0x19);
        let navy = [20, 20, 120];
        assert_eq!(swatch_color(Mark::Color(navy), light), Some(Color32::from_rgb(20, 20, 120)));
        let on_dark = swatch_color(Mark::Color(navy), dark).unwrap();
        assert!(contrast(on_dark, dark) >= MIN_CONTRAST, "a dark system stays visible");
        assert_ne!(on_dark, Color32::from_rgb(20, 20, 120));
        assert_ne!(
            swatch_color(Mark::Color([200, 30, 30]), light),
            swatch_color(Mark::Color([30, 30, 200]), light)
        );
        assert_eq!(swatch_color(Mark::Plus, light), None);
        assert_eq!(swatch_color(Mark::Dash, dark), None);
    }

    #[test]
    fn system_dropdown_renders_and_keeps_the_choice() {
        let rows = system_items(&[], "none", Mark::Dash);
        let ctx = egui::Context::default();
        let mut sel = Some(404);
        let mut changed = true;
        let _ = ctx.run(Default::default(), |c| {
            egui::CentralPanel::default().show(c, |ui| {
                let paper = Color32::WHITE; // design-lint: data
                let th = active_theme(ui);
                changed = system_dropdown(ui, &th, "t", &mut sel, &rows, paper);
            });
        });
        assert!(!changed);
        assert_eq!(sel, Some(404), "only a click changes the choice");
    }

    fn two_systems() -> Vec<SysRow> {
        let sys = |id: i64| ddc_model::MepSystem {
            id,
            name: format!("S{id}"),
            kind: SystemKind::ALL[0],
            color: SystemKind::ALL[0].color(),
        };
        system_items(&[sys(2), sys(5)], "none", Mark::Dash)
    }

    #[test]
    fn a_stale_id_gets_a_missing_row_and_picking_none_clears_it() {
        let rows = with_missing(&two_systems(), Some(404), "gone");
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[0].id, rows[0].label.as_str(), rows[0].mark), (Some(404), "gone", Mark::Missing));
        assert_eq!(rows.iter().position(|r| r.id == Some(404)), Some(0), "the stale row is selected");
        let mut sel = Some(404);
        assert!(apply_pick(&mut sel, &rows, 1, true), "none from a stale id is a change");
        assert_eq!(sel, None);
        let mut sel = Some(404);
        assert!(!apply_pick(&mut sel, &rows, 0, true), "the missing row itself is no change");
        assert_eq!(sel, Some(404));
        let mut sel = Some(404);
        assert!(apply_pick(&mut sel, &rows, 3, true));
        assert_eq!(sel, Some(5), "another system can be picked from the stale row");
    }

    #[test]
    fn valid_ids_and_none_stay_untouched() {
        let base = two_systems();
        assert_eq!(with_missing(&base, Some(5), "gone"), base, "a valid id adds no row");
        assert_eq!(with_missing(&base, None, "gone"), base, "none adds no row");
        let mut sel = Some(5);
        assert!(!apply_pick(&mut sel, &base, 2, false), "re-render without a click");
        assert!(!apply_pick(&mut sel, &base, 2, true), "same id is no change");
        assert_eq!(sel, Some(5));
        let mut sel = None;
        assert!(!apply_pick(&mut sel, &base, 0, true), "none row stays a no-op for none");
        assert_eq!(sel, None);
        assert!(!apply_pick(&mut sel, &base, 9, true), "an index past the rows writes nothing");
    }

    #[test]
    fn missing_row_renders_headless_in_both_languages() {
        for ru in [false, true] {
            let label = missing_label(ru, 404);
            assert!(label.contains("#404") && !label.contains(MISSING_KEY), "ru={ru}: {label}");
            for (mode, _) in MODES {
                let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
                let ctx = egui::Context::default();
                let (mut sel, mut valid) = (Some(404), Some(2));
                let mut changed = [true; 2];
                let _ = ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        let paper = th.c("canvas.paper");
                        let rows = two_systems();
                        changed[0] = system_dropdown_in(ui, &th, "s", &mut sel, &rows, paper, 160.0, &label);
                        changed[1] = system_dropdown_in(ui, &th, "v", &mut valid, &rows, paper, 160.0, &label);
                    });
                });
                assert_eq!(changed, [false; 2], "ru={ru} {mode:?}");
                assert_eq!((sel, valid), (Some(404), Some(2)), "ru={ru} {mode:?}: no write without a click");
            }
        }
        assert_ne!(missing_label(false, 1), missing_label(true, 1), "both catalogs carry the key");
    }

    const MODES:[(Mode, bool); 2] = [(Mode::Light, false), (Mode::Dark, true)];
    /// Smallest CIE76 distance between two system colours, and to a selection or diff colour.
    const MIN_DE: f64 = 15.0;

    fn tokens() -> DesignTokens {
        DesignTokens::embedded().unwrap()
    }

    fn lab(c: Color32) -> [f64; 3] {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        let f = |t: f64| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
    }

    fn de(a: Color32, b: Color32) -> f64 {
        let (a, b) = (lab(a), lab(b));
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn hue(c: Color32) -> f64 {
        let (r, g, b) = (f64::from(c.r()), f64::from(c.g()), f64::from(c.b()));
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if mx == mn {
            return 0.0;
        }
        let d = mx - mn;
        let h = if mx == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if mx == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h * 60.0
    }

    #[test]
    fn every_system_has_a_token_in_both_themes() {
        let t = tokens();
        for (mode, _) in MODES {
            for k in SystemKind::ALL {
                let name = token_name(k);
                let c = t.color(mode, &name).unwrap_or_else(|e| panic!("{mode:?} {e}"));
                assert_eq!(c.a(), 255, "{mode:?} {name} is translucent");
            }
        }
        assert_eq!(token_name(SystemKind::ColdWater), "mep.cold_water");
    }

    #[test]
    fn a_missing_token_falls_back_to_the_system_colour_without_panic() {
        for k in SystemKind::ALL {
            let [r, g, b] = k.color();
            let c = token_or_identity(k, Err::<Color32, _>("no such token"));
            assert_eq!(c, Color32::from_rgb(r, g, b), "{}", k.name());
            let ok = Color32::from_rgb(1, 2, 3);
            assert_eq!(token_or_identity(k, Ok::<_, String>(ok)), ok);
        }
    }

    #[test]
    fn every_system_class_has_a_colour_in_both_themes() {
        for k in SystemKind::ALL {
            for dark in [false, true] {
                assert_eq!(kind_color(k, dark).a(), 255, "{} dark={dark}", k.name());
            }
        }
    }

    #[test]
    fn contrast_against_plan_and_3d_backgrounds() {
        let t = tokens();
        for (mode, dark) in MODES {
            for bg in ["canvas.paper", "canvas.bg.top", "canvas.bg.bottom"] {
                let b = t.color(mode, bg).unwrap();
                for k in SystemKind::ALL {
                    let r = contrast(kind_color(k, dark), b);
                    assert!(r >= MIN_CONTRAST, "{mode:?} {} on {bg}: {r:.2}", k.name());
                }
            }
        }
    }

    #[test]
    fn systems_differ_from_each_other() {
        for (mode, dark) in MODES {
            for (i, a) in SystemKind::ALL.iter().enumerate() {
                for b in &SystemKind::ALL[i + 1..] {
                    let d = de(kind_color(*a, dark), kind_color(*b, dark));
                    assert!(d >= MIN_DE, "{mode:?} {} vs {}: dE {d:.1}", a.name(), b.name());
                }
            }
        }
    }

    #[test]
    fn systems_differ_from_selection_and_diff_colours() {
        let t = tokens();
        let others = [
            "selection",
            "hover",
            "preview",
            "snap",
            "ai.created",
            "ai.changed",
            "ai.deleted",
        ];
        for (mode, dark) in MODES {
            for o in others {
                let oc = t.color(mode, o).unwrap();
                for k in SystemKind::ALL {
                    let d = de(kind_color(k, dark), oc);
                    assert!(d >= MIN_DE, "{mode:?} {} vs {o}: dE {d:.1}", k.name());
                }
            }
        }
    }

    #[test]
    fn hue_family_of_each_system_is_kept() {
        for (mode, dark) in MODES {
            for k in SystemKind::ALL {
                let [r, g, b] = k.color();
                let (h0, h1) = (hue(Color32::from_rgb(r, g, b)), hue(kind_color(k, dark)));
                let d = (h0 - h1).abs();
                let d = d.min(360.0 - d);
                assert!(d <= 40.0, "{mode:?} {}: hue {h0:.0} -> {h1:.0}", k.name());
            }
        }
    }

    #[test]
    fn default_systems_use_the_theme_token() {
        for (mode, dark) in MODES {
            let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
            let t = crate::ui_kit::from_theme(&th);
            for k in SystemKind::ALL {
                assert_eq!(plan(k.color(), &t), kind_color(k, dark), "{mode:?} {}", k.name());
                assert_eq!(kind_color(k, dark), th.c(&token_name(k)), "{mode:?} {}", k.name());
            }
        }
    }

    #[test]
    fn custom_colours_become_readable_and_readable_ones_stay() {
        for (mode, _) in MODES {
            let th = ddc_ui::Theme::new(mode, ddc_ui::Density::Compact);
            let t = crate::ui_kit::from_theme(&th);
            for rgb in [[0, 0, 0], [255, 255, 255], [255, 255, 0], [0, 0, 128]] {
                let c = plan(rgb, &t);
                assert!(contrast(c, t.paper) >= MIN_CONTRAST, "{mode:?} {rgb:?}");
            }
            let ok = [0x80, 0x30, 0x90];
            let same = Color32::from_rgb(ok[0], ok[1], ok[2]);
            if contrast(same, t.paper) >= MIN_CONTRAST {
                assert_eq!(plan(ok, &t), same);
            }
        }
        let light = Color32::from_rgb(0xFB, 0xFB, 0xFA);
        let dark = Color32::from_rgb(0x14, 0x16, 0x19);
        assert_eq!(readable_on([20, 20, 120], light), Color32::from_rgb(20, 20, 120));
        assert_eq!(readable_on([240, 240, 120], dark), Color32::from_rgb(240, 240, 120));
    }

    #[test]
    fn plan_sites_hold_no_literal_system_colour() {
        let src = include_str!("ui.rs");
        assert!(!src.contains("Color32::from_rgb(rgb["), "literal rgb at a plan site");
        assert!(src.contains("mep_color::plan("));
    }
}
