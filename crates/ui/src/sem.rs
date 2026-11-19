// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed access to the design tokens: semantic colours (`Sem`) and size scales. Feature code
//! uses these names, never literals; tests keep them equal to docs/design/tokens.json and check
//! every text/background pair against WCAG AA in both themes.
use crate::theme::{Mode, TokenError, Tokens};
use egui::Color32;
use std::sync::OnceLock;

macro_rules! semantic {
    ($($field:ident => $name:literal),* $(,)?) => {
        /// Semantic palette of one theme. Alpha tokens stay translucent: flatten with `over_rgba`.
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Sem {
            $(pub $field: Color32,)*
        }
        /// Token names behind the `Sem` fields.
        pub const SEM_NAMES: &[&str] = &[$($name),*];
        #[cfg(test)]
        const SEM_FIELDS: &[&str] = &[$(stringify!($field)),*];
        impl Sem {
            pub fn resolve(t: &Tokens, mode: Mode) -> Result<Self, TokenError> {
                Ok(Self {
                    $($field: t.color(mode, $name)?,)*
                })
            }
        }
    };
}

semantic! {
    bg_app => "bg.app",
    bg_panel => "bg.panel",
    bg_viewport => "bg.viewport",
    bg_sunken => "bg.sunken",
    bg_input => "bg.input",
    bg_elevated => "bg.elevated",
    bg_overlay => "bg.overlay",
    bg_backdrop => "bg.backdrop",
    bg_hover => "bg.hover",
    bg_pressed => "bg.pressed",
    border_subtle => "border.subtle",
    border_strong => "border.strong",
    border_control => "border.control",
    border_overlay => "border.overlay",
    text_primary => "text.primary",
    text_secondary => "text.secondary",
    text_placeholder => "text.placeholder",
    text_disabled => "text.disabled",
    accent => "accent",
    accent_hover => "accent.hover",
    accent_pressed => "accent.pressed",
    accent_subtle => "accent.subtle",
    accent_text_on => "accent.text_on",
    focus => "focus",
    focus_ring => "focus.ring",
    selection => "selection",
    selection_fill => "selection.fill",
    selection_bg => "selection.bg",
    selection_text => "selection.text",
    hover => "hover",
    preview => "preview",
    ai_accent => "ai.accent",
    ai_subtle => "ai.subtle",
    text_on_subtle => "text.on_subtle",
    border_focus => "border.focus",
    bg_selected => "bg.selected",
    bg_selected_hover => "bg.selected.hover",
    bg_canvas_chip => "bg.canvas_chip",
    snap => "snap",
    snap_strong => "snap.strong",
    snap_text_on => "snap.text_on",
    error => "error",
    warning => "warning",
    warning_subtle => "warning.subtle",
    success => "success",
    success_subtle => "success.subtle",
    neutral_subtle => "neutral.subtle",
    danger => "danger",
    danger_hover => "danger.hover",
    danger_pressed => "danger.pressed",
    danger_subtle => "danger.subtle",
    danger_text_on => "danger.text_on",
    control_track => "control.track",
    control_thumb => "control.thumb",
    axis_x => "axis.x",
    axis_y => "axis.y",
    axis_z => "axis.z",
    cube_ink => "cube.ink",
    cube_edge => "cube.edge",
    cube_face_lo => "cube.face.lo",
    cube_face_hi => "cube.face.hi",
    canvas_bg_top => "canvas.bg.top",
    canvas_bg_bottom => "canvas.bg.bottom",
    canvas_warn_fill => "canvas.warn.fill",
    canvas_warn_ink => "canvas.warn.ink",
    canvas_warn_edge => "canvas.warn.edge",
    canvas_tag_door => "canvas.tag.door",
    canvas_tag_window => "canvas.tag.window",
    canvas_tag_room => "canvas.tag.room",
    canvas_tag_grid => "canvas.tag.grid",
    canvas_tag_column => "canvas.tag.column",
    canvas_clash_a => "canvas.clash.a",
    canvas_clash_b => "canvas.clash.b",
    canvas_clash_box => "canvas.clash.box",
    canvas_contour_minor => "canvas.contour.minor",
    canvas_contour_major => "canvas.contour.major",
    canvas_dz_below => "canvas.dz.below",
    canvas_dz_above => "canvas.dz.above",
    canvas_dim => "canvas.dim",
    canvas_ref => "canvas.ref",
    canvas_mat_grass => "canvas.mat.grass",
    canvas_mat_asphalt => "canvas.mat.asphalt",
    canvas_mat_gravel => "canvas.mat.gravel",
    canvas_mat_paving => "canvas.mat.paving",
    canvas_mat_water => "canvas.mat.water",
    canvas_mat_soil => "canvas.mat.soil",
}

/// Semantic palette by theme; parsed once, cheap on hot paths.
pub fn sem(dark: bool) -> &'static Sem {
    static CACHE: OnceLock<[Sem; 2]> = OnceLock::new();
    let c = CACHE.get_or_init(|| {
        let t = Tokens::embedded().expect("embedded tokens.json is validated by tests");
        let of = |m| Sem::resolve(&t, m).expect("semantic tokens are validated by tests");
        [of(Mode::Light), of(Mode::Dark)]
    });
    &c[dark as usize]
}

/// Palette of the theme applied to this context.
pub fn sem_of(ctx: &egui::Context) -> Sem {
    *crate::theme::of(ctx).sem()
}

/// 4 px grid (`space.named`).
pub mod space {
    pub const XXS: f32 = 2.0;
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
    pub const XXXL: f32 = 48.0;

    /// Space above `content` that centres it in a `band` (a text line in a control-high row).
    pub fn centre(band: f32, content: f32) -> f32 {
        ((band - content) / 2.0).max(0.0)
    }
}

/// Corner radii (`radius`). Dialogs and windows use `Theme::radius`, which also honours the
/// layout's radius scale; these are for code without a `Theme`.
pub mod radius {
    pub const XS: f32 = 2.0;
    pub const SM: f32 = 4.0;
    pub const MD: f32 = 8.0;
    pub const LG: f32 = 12.0;
}

/// Type scale in points (`font.scale.*.size`).
pub mod type_size {
    pub const CAPTION: f32 = 11.0;
    pub const LABEL: f32 = 12.0;
    pub const BODY: f32 = 13.0;
    pub const SECTION: f32 = 11.0;
    pub const TITLE: f32 = 14.0;
    pub const HEADING: f32 = 18.0;
}

/// Icon sizes (`icon.named`); each size draws its optical master (16 / 20 / 24, `icons`).
pub mod icon_size {
    pub const XS: f32 = 12.0;
    pub const SM: f32 = 16.0;
    pub const MD: f32 = 20.0;
    pub const LG: f32 = 24.0;
    /// Illustration size (empty states, onboarding): the duotone direction.
    pub const XL: f32 = 32.0;
}

/// Line weights (`stroke`).
pub mod stroke {
    pub const HAIRLINE: f32 = 1.0;
    pub const CONTROL: f32 = 1.0;
    pub const FOCUS: f32 = 2.0;
    pub const INDICATOR: f32 = 2.0;
}

/// Fixed control metrics (`control`); density-dependent heights come from `Theme::control_h`.
pub mod control {
    pub const CHECK: f32 = 16.0;
    pub const TOGGLE_W: f32 = 28.0;
    pub const TOGGLE_H: f32 = 16.0;
    pub const KEYCAP_H: f32 = 16.0;
    pub const BADGE_H: f32 = 16.0;
    /// Smallest pointer target of any clickable control.
    pub const HIT_MIN: f32 = 24.0;
    pub const PROGRESS_H: f32 = 4.0;
    pub const TAB_H: f32 = 32.0;
    pub const SECTION_H: f32 = 28.0;
    pub const FIELD_MIN_W: f32 = 48.0;
    pub const BUTTON_MIN_W: f32 = 64.0;
    pub const DROPDOWN_W: f32 = 160.0;
    /// Menu row height, and the width range of a menu: rows grow to their label and shortcut
    /// within it, a longer label ends with «…».
    pub const MENU_ROW_H: f32 = 28.0;
    pub const MENU_W: f32 = 220.0;
    pub const MENU_MAX_W: f32 = 320.0;
}

/// Panel and dialog widths (`layout`).
pub mod layout {
    pub const PROP_VALUE: f32 = 104.0;
    pub const PROP_UNIT: f32 = 24.0;
    pub const PROP_GAP: f32 = 8.0;
    pub const PROP_KEY_MIN: f32 = 88.0;
    pub const PROP_KEY_MAX: f32 = 220.0;
    pub const DIALOG_HUD: f32 = 320.0;
    pub const DIALOG_CONFIRM: f32 = 400.0;
    pub const DIALOG_FORM: f32 = 460.0;
    pub const DIALOG_TOOL: f32 = 560.0;
    pub const DIALOG_TOOL_H: f32 = 480.0;
    pub const DIALOG_WIDE: f32 = 760.0;
    pub const DIALOG_WIDE_H: f32 = 560.0;
    pub const DIALOG_WORKSPACE: f32 = 980.0;
    pub const DIALOG_WORKSPACE_H: f32 = 640.0;
    pub const TOOLTIP_W: f32 = 320.0;
    pub const TOAST_W: f32 = 380.0;
    pub const EMPTY_W: f32 = 320.0;
}

/// Density-dependent control height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlSize {
    Small,
    #[default]
    Medium,
    Large,
}

/// Minimum contrast for text (WCAG AA) and for graphics that carry meaning (WCAG 1.4.11).
pub const AA_TEXT: f64 = 4.5;
pub const AA_GRAPHIC: f64 = 3.0;

/// A foreground on a surface; `base` is the opaque colour under a translucent surface.
pub type Pair = (&'static str, &'static str, Option<&'static str>);

/// Every text/background combination the widgets and panels may produce.
pub const TEXT_PAIRS: &[Pair] = &[
    ("text.primary", "bg.app", None),
    ("text.primary", "bg.panel", None),
    ("text.primary", "bg.viewport", None),
    ("text.primary", "bg.sunken", None),
    ("text.primary", "bg.input", None),
    ("text.primary", "bg.elevated", None),
    ("text.primary", "bg.overlay", Some("bg.viewport")),
    ("text.primary", "bg.hover", Some("bg.panel")),
    ("text.primary", "bg.pressed", Some("bg.panel")),
    ("text.primary", "bg.hover", Some("bg.elevated")),
    ("text.primary", "bg.pressed", Some("bg.elevated")),
    ("text.primary", "accent.subtle", None),
    ("text.primary", "selection.bg", None),
    ("text.primary", "warning.subtle", None),
    ("text.primary", "success.subtle", None),
    ("text.primary", "danger.subtle", None),
    ("text.primary", "neutral.subtle", None),
    ("text.secondary", "bg.app", None),
    ("text.secondary", "bg.panel", None),
    ("text.secondary", "bg.viewport", None),
    ("text.secondary", "bg.sunken", None),
    ("text.secondary", "bg.input", None),
    ("text.secondary", "bg.elevated", None),
    ("text.secondary", "bg.overlay", Some("bg.viewport")),
    ("text.secondary", "bg.hover", Some("bg.panel")),
    ("text.secondary", "bg.pressed", Some("bg.panel")),
    ("accent", "bg.hover", Some("bg.elevated")),
    ("accent", "bg.pressed", Some("bg.panel")),
    ("error", "bg.hover", Some("bg.panel")),
    ("error", "bg.hover", Some("bg.elevated")),
    ("text.secondary", "bg.hover", Some("bg.elevated")),
    ("text.secondary", "bg.pressed", Some("bg.elevated")),
    ("text.secondary", "neutral.subtle", None),
    ("text.secondary", "warning.subtle", None),
    ("text.secondary", "danger.subtle", None),
    ("text.placeholder", "bg.input", None),
    ("selection.text", "selection.bg", None),
    ("accent", "bg.panel", None),
    ("accent", "bg.elevated", None),
    ("accent", "bg.app", None),
    ("accent", "accent.subtle", None),
    ("accent.text_on", "accent", None),
    ("accent.text_on", "accent.hover", None),
    ("accent.text_on", "accent.pressed", None),
    ("danger.text_on", "danger", None),
    ("danger.text_on", "danger.hover", None),
    ("danger.text_on", "danger.pressed", None),
    ("danger", "bg.panel", None),
    ("danger", "bg.elevated", None),
    ("danger", "danger.subtle", None),
    ("error", "bg.panel", None),
    ("error", "bg.elevated", None),
    ("error", "bg.app", None),
    ("error", "danger.subtle", None),
    ("warning", "bg.panel", None),
    ("warning", "bg.elevated", None),
    ("warning", "bg.app", None),
    ("warning", "warning.subtle", None),
    ("success", "bg.panel", None),
    ("success", "bg.elevated", None),
    ("success", "bg.app", None),
    ("success", "success.subtle", None),
    ("snap.text_on", "snap.strong", None),
    ("canvas.warn.ink", "canvas.warn.fill", None),
    ("text.on_subtle", "bg.selected", None),
    ("text.on_subtle", "bg.selected.hover", None),
    ("text.on_subtle", "ai.subtle", None),
    ("selection.text", "bg.selected", None),
    ("selection.text", "bg.selected.hover", None),
    ("ai.accent", "bg.panel", None),
    ("ai.accent", "bg.elevated", None),
    ("ai.accent", "bg.app", None),
    ("ai.accent", "ai.subtle", None),
    ("text.primary", "bg.canvas_chip", Some("bg.viewport")),
    ("text.secondary", "bg.canvas_chip", Some("bg.viewport")),
];

/// Disabled text is exempt from AA but keeps 3:1 on every control surface, drawn flat.
pub const DISABLED_FLOOR: f64 = 3.0;
pub const DISABLED_PAIRS: &[Pair] = &[
    ("text.disabled", "bg.panel", None),
    ("text.disabled", "bg.elevated", None),
    ("text.disabled", "bg.input", None),
    ("text.disabled", "bg.sunken", None),
];

/// Graphics that must stay visible: focus, control outlines, canvas marks.
pub const GRAPHIC_PAIRS: &[Pair] = &[
    ("focus", "bg.panel", None),
    ("focus", "bg.elevated", None),
    ("focus", "bg.app", None),
    ("focus.ring", "bg.panel", None),
    ("focus.ring", "bg.elevated", None),
    ("focus.ring", "canvas.paper", None),
    ("focus.ring", "accent.subtle", None),
    ("border.control", "bg.panel", None),
    ("border.control", "bg.elevated", None),
    ("border.control", "bg.input", None),
    ("focus", "bg.input", None),
    ("border.focus", "bg.panel", None),
    ("border.focus", "bg.elevated", None),
    ("border.focus", "bg.input", None),
    ("ai.accent", "bg.viewport", None),
    ("preview", "canvas.paper", None),
    ("control.track", "bg.panel", None),
    ("control.track", "bg.elevated", None),
    ("control.thumb", "control.track", None),
    ("accent", "bg.sunken", None),
    ("selection", "bg.viewport", None),
    ("hover", "bg.viewport", None),
    ("preview", "bg.viewport", None),
    ("snap", "bg.viewport", None),
    ("error", "bg.viewport", None),
    ("warning", "bg.viewport", None),
    ("success", "bg.viewport", None),
    ("axis.x", "bg.viewport", None),
    ("axis.y", "bg.viewport", None),
    ("axis.z", "bg.viewport", None),
    ("cube.ink", "cube.face.hi", None),
    ("canvas.tag.door", "canvas.paper", None),
    ("canvas.tag.window", "canvas.paper", None),
    ("canvas.tag.room", "canvas.paper", None),
    ("canvas.tag.grid", "canvas.paper", None),
    ("canvas.tag.column", "canvas.paper", None),
    ("canvas.clash.a", "canvas.paper", None),
    ("canvas.clash.b", "canvas.paper", None),
    ("canvas.clash.box", "canvas.paper", None),
    ("canvas.contour.minor", "canvas.paper", None),
    ("canvas.contour.major", "canvas.paper", None),
    ("canvas.dz.below", "canvas.paper", None),
    ("canvas.dz.above", "canvas.paper", None),
    ("canvas.dim", "canvas.paper", None),
    ("canvas.ref", "canvas.paper", None),
    ("disc.arch", "bg.viewport", None),
    ("disc.struct", "bg.viewport", None),
    ("disc.mep.mech", "bg.viewport", None),
    ("disc.mep.elec", "bg.viewport", None),
    ("disc.mep.plumb", "bg.viewport", None),
    // 3D draws on the canvas gradient, not on bg.viewport
    ("selection", "canvas.bg.bottom", None),
    ("hover", "canvas.bg.bottom", None),
    ("preview", "canvas.bg.bottom", None),
    ("snap", "canvas.bg.bottom", None),
    ("error", "canvas.bg.bottom", None),
    ("warning", "canvas.bg.bottom", None),
    ("success", "canvas.bg.bottom", None),
    ("selection", "canvas.bg.top", None),
    ("hover", "canvas.bg.top", None),
    ("preview", "canvas.bg.top", None),
    ("snap", "canvas.bg.top", None),
    ("axis.x", "canvas.bg.bottom", None),
    ("axis.y", "canvas.bg.bottom", None),
    ("axis.z", "canvas.bg.bottom", None),
    ("ai.created", "bg.viewport", None),
    ("ai.changed", "bg.viewport", None),
    ("ai.deleted", "bg.viewport", None),
    ("ai.created", "canvas.paper", None),
    ("ai.changed", "canvas.paper", None),
    ("ai.deleted", "canvas.paper", None),
    ("ai.created", "canvas.bg.bottom", None),
    ("ai.changed", "canvas.bg.bottom", None),
    ("ai.deleted", "canvas.bg.bottom", None),
];

/// Named alphas for faded canvas graphics; call sites use these, `FADED_PAIRS` checks them.
/// ZONE_EDGE and MESH_EDGE are 0.7 (0.6 gave 2.55 and 2.59 on the light paper, below 3:1).
pub mod fade {
    pub const ZONE_EDGE: f32 = 0.7;
    pub const INVALID: f32 = 0.2;
    pub const MESH_EDGE: f32 = 0.7;
}

/// Graphics drawn through `gamma_multiply`: foreground, the alpha used at the call site, surface.
/// Plan origin axes use 0.75, 3D gizmo rings 0.85.
pub const FADED_PAIRS: &[(&str, f32, &str)] = &[
    ("axis.x", 0.75, "canvas.paper"),
    ("axis.y", 0.75, "canvas.paper"),
    ("axis.z", 0.75, "canvas.paper"),
    ("axis.x", 0.85, "canvas.bg.bottom"),
    ("axis.y", 0.85, "canvas.bg.bottom"),
    ("axis.z", 0.85, "canvas.bg.bottom"),
    ("axis.x", 0.85, "canvas.bg.top"),
    ("axis.y", 0.85, "canvas.bg.top"),
    ("axis.z", 0.85, "canvas.bg.top"),
    ("preview", fade::ZONE_EDGE, "canvas.paper"),
    ("text.secondary", fade::MESH_EDGE, "canvas.paper"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{contrast, over, over_rgba};
    use serde_json::Value;
    use std::collections::BTreeSet;

    const MODES: [Mode; 2] = [Mode::Light, Mode::Dark];

    fn ratio(t: &Tokens, mode: Mode, pair: &Pair) -> f64 {
        let (fg, bg, base) = *pair;
        let col = |n: &str| t.color(mode, n).unwrap_or_else(|e| panic!("{e}"));
        let bg_c = match base {
            Some(b) => over_rgba(col(bg), col(b)),
            None => col(bg),
        };
        assert_eq!(
            bg_c.a(),
            255,
            "{mode:?} {bg} is translucent: name the surface under it"
        );
        contrast(over_rgba(col(fg), bg_c), bg_c)
    }

    /// Every semantic text pair passes AA in both themes.
    #[test]
    fn text_pairs_pass_aa() {
        let t = Tokens::embedded().unwrap();
        for mode in MODES {
            for p in TEXT_PAIRS {
                let r = ratio(&t, mode, p);
                assert!(r >= AA_TEXT, "{mode:?} {} on {}: {r:.2}", p.0, p.1);
            }
        }
    }

    #[test]
    fn graphic_pairs_pass_aa() {
        let t = Tokens::embedded().unwrap();
        for mode in MODES {
            for p in GRAPHIC_PAIRS {
                let r = ratio(&t, mode, p);
                assert!(r >= AA_GRAPHIC, "{mode:?} {} on {}: {r:.2}", p.0, p.1);
            }
        }
    }

    #[test]
    fn faded_pairs_contrast_both_themes() {
        for (fg, a) in [("preview", fade::ZONE_EDGE), ("text.secondary", fade::MESH_EDGE)] {
            assert!(FADED_PAIRS.contains(&(fg, a, "canvas.paper")), "{fg} missing");
        }
        faded_pairs_pass_aa();
    }

    #[test]
    fn faded_pairs_pass_aa() {
        let t = Tokens::embedded().unwrap();
        for mode in MODES {
            for (fg, alpha, bg) in FADED_PAIRS {
                let bg_c = t.color(mode, bg).unwrap();
                let r = contrast(over(t.color(mode, fg).unwrap(), *alpha, bg_c), bg_c);
                assert!(r >= AA_GRAPHIC, "{mode:?} {fg}@{alpha} on {bg}: {r:.2}");
            }
        }
    }

    /// Every semantic name is a key of both theme blocks of the embedded tokens.
    #[test]
    fn sem_names_match_tokens_json() {
        let v: Value = serde_json::from_str(include_str!("../../../docs/design/tokens.json")).unwrap();
        let txt = v.to_string();
        for n in SEM_NAMES {
            assert!(txt.matches(&format!("\"{n}\"")).count() >= 2, "{n} missing in a theme");
        }
        for mode in MODES {
            Sem::resolve(&Tokens::embedded().unwrap(), mode).unwrap();
        }
    }

    /// `field => "name"` is typed by hand: a swap would pass every other test.
    #[test]
    fn sem_fields_match_token_names() {
        assert_eq!(SEM_FIELDS.len(), SEM_NAMES.len());
        for (field, name) in SEM_FIELDS.iter().zip(SEM_NAMES) {
            assert_eq!(*field, name.replace('.', "_"), "{field} => {name}");
        }
    }

    /// Keys repeated in one JSON object. Deserialization silently keeps the last one.
    fn duplicate_keys(json: &str) -> Vec<String> {
        let mut seen: Vec<BTreeSet<String>> = Vec::new();
        let mut dups = Vec::new();
        let mut chars = json.chars();
        while let Some(c) = chars.next() {
            match c {
                '{' => seen.push(BTreeSet::new()),
                '}' => {
                    seen.pop();
                }
                '"' => {
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => {
                                chars.next();
                            }
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    let is_key = chars.clone().find(|c| !c.is_whitespace()) == Some(':');
                    if let (true, Some(top)) = (is_key, seen.last_mut()) {
                        if !top.insert(s.clone()) {
                            dups.push(s);
                        }
                    }
                }
                _ => {}
            }
        }
        dups
    }

    #[test]
    fn tokens_have_no_duplicate_keys() {
        assert_eq!(
            duplicate_keys(r#"{"a": {"b": "a", "c": 1}, "b": 2}"#),
            Vec::<String>::new()
        );
        assert_eq!(
            duplicate_keys(r#"{"a": 1, "b": {"a": 2}, "a": 3}"#),
            vec!["a".to_string()]
        );
        assert_eq!(
            duplicate_keys(crate::theme::TOKENS_JSON),
            Vec::<String>::new()
        );
    }

    /// A new text-like token cannot ship without a contrast pair.
    #[test]
    fn every_foreground_token_has_a_pair() {
        let t = Tokens::embedded().unwrap();
        let listed = |n: &str| TEXT_PAIRS.iter().any(|p| p.0 == n);
        for name in t.color.light.keys() {
            let text_like = (name.starts_with("text.") && name != "text.disabled")
                || name.ends_with(".text_on")
                || name.ends_with(".text");
            if text_like {
                assert!(listed(name), "{name} has no entry in TEXT_PAIRS");
            }
        }
    }

    #[test]
    fn semantic_palette_resolves_in_both_themes() {
        let t = Tokens::embedded().unwrap();
        for mode in MODES {
            Sem::resolve(&t, mode).unwrap();
        }
        assert_eq!(
            sem(false).bg_panel,
            t.color(Mode::Light, "bg.panel").unwrap()
        );
        assert_eq!(sem(true).bg_panel, t.color(Mode::Dark, "bg.panel").unwrap());
        assert!(SEM_NAMES.iter().all(|n| t.color.light.contains_key(*n)));
    }

    fn num(v: &Value, path: &str) -> f32 {
        let mut cur = v;
        for k in path.split('.') {
            cur = &cur[k];
        }
        cur.as_f64()
            .unwrap_or_else(|| panic!("no number at {path}")) as f32
    }

    /// Rust constants and tokens.json are one source of truth.
    #[test]
    fn constants_match_json() {
        let v: Value = serde_json::from_str(crate::theme::TOKENS_JSON).unwrap();
        let checks: &[(&str, f32)] = &[
            ("space.named.xxs", space::XXS),
            ("space.named.xs", space::XS),
            ("space.named.sm", space::SM),
            ("space.named.md", space::MD),
            ("space.named.lg", space::LG),
            ("space.named.xl", space::XL),
            ("space.named.xxl", space::XXL),
            ("space.named.xxxl", space::XXXL),
            ("radius.xs", radius::XS),
            ("radius.sm", radius::SM),
            ("radius.md", radius::MD),
            ("radius.lg", radius::LG),
            ("font.scale.caption.size", type_size::CAPTION),
            ("font.scale.label.size", type_size::LABEL),
            ("font.scale.body.size", type_size::BODY),
            ("font.scale.section.size", type_size::SECTION),
            ("font.scale.title.size", type_size::TITLE),
            ("font.scale.heading.size", type_size::HEADING),
            ("icon.named.xs", icon_size::XS),
            ("icon.named.sm", icon_size::SM),
            ("icon.named.md", icon_size::MD),
            ("icon.named.lg", icon_size::LG),
            ("icon.named.xl", icon_size::XL),
            ("stroke.hairline", stroke::HAIRLINE),
            ("stroke.control", stroke::CONTROL),
            ("stroke.focus", stroke::FOCUS),
            ("stroke.indicator", stroke::INDICATOR),
            ("control.check", control::CHECK),
            ("control.toggle_w", control::TOGGLE_W),
            ("control.toggle_h", control::TOGGLE_H),
            ("control.keycap_h", control::KEYCAP_H),
            ("control.badge_h", control::BADGE_H),
            ("control.progress_h", control::PROGRESS_H),
            ("control.tab_h", control::TAB_H),
            ("control.section_h", control::SECTION_H),
            ("control.field_min_w", control::FIELD_MIN_W),
            ("control.button_min_w", control::BUTTON_MIN_W),
            ("control.dropdown_w", control::DROPDOWN_W),
            ("control.menu_row_h", control::MENU_ROW_H),
            ("control.menu_w", control::MENU_W),
            ("control.menu_max_w", control::MENU_MAX_W),
            ("layout.prop_value", layout::PROP_VALUE),
            ("layout.prop_unit", layout::PROP_UNIT),
            ("layout.prop_gap", layout::PROP_GAP),
            ("layout.prop_key_min", layout::PROP_KEY_MIN),
            ("layout.prop_key_max", layout::PROP_KEY_MAX),
            ("layout.dialog_hud", layout::DIALOG_HUD),
            ("layout.dialog_confirm", layout::DIALOG_CONFIRM),
            ("layout.dialog_form", layout::DIALOG_FORM),
            ("layout.dialog_tool", layout::DIALOG_TOOL),
            ("layout.dialog_tool_h", layout::DIALOG_TOOL_H),
            ("layout.dialog_wide", layout::DIALOG_WIDE),
            ("layout.dialog_wide_h", layout::DIALOG_WIDE_H),
            ("layout.dialog_workspace", layout::DIALOG_WORKSPACE),
            ("layout.dialog_workspace_h", layout::DIALOG_WORKSPACE_H),
            ("layout.tooltip_w", layout::TOOLTIP_W),
            ("layout.toast_w", layout::TOAST_W),
            ("layout.empty_w", layout::EMPTY_W),
        ];
        for (path, c) in checks {
            assert_eq!(num(&v, path), *c, "{path}");
        }
    }

    /// Spacing keeps the 4 px grid; 2 is the single documented half-step.
    #[test]
    fn spacing_stays_on_grid() {
        let t = Tokens::embedded().unwrap();
        for s in &t.space.scale {
            assert!(*s == 2.0 || s % t.space.unit == 0.0, "{s}");
        }
    }
}
