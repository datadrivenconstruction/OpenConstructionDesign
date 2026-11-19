// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Looks (Settings → "Appearance"): named presets from `data/looks.json` that bundle the theme,
//! accent, neutral palette patches, radii, density, face, text size, UI scale, icon set and
//! ribbon mode. Every accent the person picks is fitted per theme so text on it and accent text
//! keep WCAG AA (`sem::TEXT_PAIRS`, `sem::GRAPHIC_PAIRS`).
use crate::sem::{Pair, AA_GRAPHIC, AA_TEXT, GRAPHIC_PAIRS, TEXT_PAIRS};
use crate::theme::{contrast, over_rgba, parse_hex, Density, Mode, Tokens, UiFont};
use egui::Color32;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const LOOKS_JSON: &str = include_str!("../data/looks.json");

/// Ribbon of a look: full, tab strip only, or one row of small buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RibbonLook {
    #[default]
    Full,
    Tabs,
    Toolbar,
}

impl RibbonLook {
    pub const ALL: [RibbonLook; 3] = [Self::Full, Self::Tabs, Self::Toolbar];
    pub fn key(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Tabs => "tabs",
            Self::Toolbar => "toolbar",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.key() == s)
    }
}

/// Density keys of the settings: «compact», «normal», «comfortable».
pub const DENSITIES: [&str; 3] = ["compact", "normal", "comfortable"];

/// Theme density of a settings key; None for «auto» (the layout decides).
pub fn density_of(key: &str) -> Option<Density> {
    match key {
        "compact" => Some(Density::Dense),
        "normal" => Some(Density::Compact),
        "comfortable" => Some(Density::Comfortable),
        _ => None,
    }
}

/// UI scale range of the settings, in steps of 5 %.
pub const SCALE_MIN: f32 = 0.9;
pub const SCALE_MAX: f32 = 1.5;
/// Text size range over the type roles.
pub const TEXT_MIN: f32 = 0.9;
pub const TEXT_MAX: f32 = 1.2;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    pub id: String,
    /// «system», «light» or «dark».
    pub theme: String,
    pub accent: String,
    /// The accent the dark theme starts from; the light one when absent.
    #[serde(default)]
    pub accent_dark: Option<String>,
    pub radius_scale: f32,
    pub density: String,
    pub font: String,
    pub text_size: f32,
    pub ui_scale: f32,
    pub icons: String,
    pub ribbon: String,
    /// Neutral colours over the embedded light / dark palettes.
    pub light: BTreeMap<String, String>,
    pub dark: BTreeMap<String, String>,
}

impl Look {
    /// Catalog key of the name: `theme.look.<id>`.
    pub fn name_key(&self) -> String {
        format!("theme.look.{}", self.id)
    }
    pub fn desc_key(&self) -> String {
        format!("theme.look.{}.desc", self.id)
    }
    pub fn accent_color(&self) -> Color32 {
        parse_hex(&self.accent).unwrap_or(Color32::from_rgb(0x22, 0x53, 0xC2))
    }
    pub fn face(&self) -> UiFont {
        UiFont::parse(&self.font).unwrap_or_default()
    }
    pub fn ribbon_look(&self) -> RibbonLook {
        RibbonLook::parse(&self.ribbon).unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LooksFile {
    version: u32,
    accents: Vec<String>,
    presets: Vec<Look>,
}

fn file() -> &'static LooksFile {
    static F: OnceLock<LooksFile> = OnceLock::new();
    F.get_or_init(|| {
        let f: LooksFile = serde_json::from_str(LOOKS_JSON).expect("data/looks.json");
        assert_eq!(f.version, 1, "data/looks.json version");
        f
    })
}

/// The embedded tokens, parsed once.
pub fn base_tokens() -> &'static Tokens {
    static T: OnceLock<Tokens> = OnceLock::new();
    T.get_or_init(|| Tokens::embedded().expect("embedded tokens.json is validated by tests"))
}

/// The seven presets in their settings order.
pub fn presets() -> &'static [Look] {
    &file().presets
}

pub fn preset(id: &str) -> Option<&'static Look> {
    presets().iter().find(|l| l.id == id)
}

/// The default look.
pub fn default_look() -> &'static Look {
    &presets()[0]
}

/// Accent swatches offered beside the custom colour.
pub fn accents() -> Vec<Color32> {
    file().accents.iter().filter_map(|s| parse_hex(s)).collect()
}

pub fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

fn hex_a(c: Color32, a: u8) -> String {
    format!("{}{a:02X}", hex(c))
}

/// `a` moved `t` (0–1) of the way to `b`.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// The accent names `derive_accent` writes.
pub const ACCENT_KEYS: [&str; 14] = [
    "bg.selected",
    "bg.selected.hover",
    "border.focus",
    "accent",
    "accent.hover",
    "accent.pressed",
    "accent.subtle",
    "accent.text_on",
    "selection",
    "selection.bg",
    "selection.text",
    "selection.fill",
    "focus",
    "focus.ring",
];

/// Contrast of one pair in `pal` (hex values); a translucent surface is flattened over `base`.
pub fn pair_ratio(pal: &BTreeMap<String, String>, p: &Pair) -> Option<f64> {
    let col = |n: &str| pal.get(n).and_then(|v| parse_hex(v));
    let bg = match p.2 {
        Some(base) => over_rgba(col(p.1)?, col(base)?),
        None => col(p.1)?,
    };
    Some(contrast(over_rgba(col(p.0)?, bg), bg))
}

/// Pairs of `list` below `min` in `pal`, restricted to those `filter` keeps.
fn failing<'a>(
    pal: &BTreeMap<String, String>,
    list: &'a [Pair],
    min: f64,
    filter: impl Fn(&Pair) -> bool,
) -> Vec<&'a Pair> {
    list.iter()
        .filter(|p| filter(p))
        .filter(|p| pair_ratio(pal, p).is_none_or(|r| r < min))
        .collect()
}

/// Every text pair under AA and every graphic pair under 3:1 in a full palette.
pub fn contrast_failures(pal: &BTreeMap<String, String>) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (list, min) in [(TEXT_PAIRS, AA_TEXT), (GRAPHIC_PAIRS, AA_GRAPHIC)] {
        for p in failing(pal, list, min, |_| true) {
            let r = pair_ratio(pal, p).unwrap_or(0.0);
            v.push(format!("{} on {} ({:?}): {r:.2}", p.0, p.1, p.2));
        }
    }
    v
}

/// Accent colours from `base` for a palette (`pal` is the full palette of one theme). The base
/// is darkened (light theme) or lightened (dark theme) in small steps until every pair that
/// involves an accent colour passes; the hue stays the person's.
pub fn derive_accent(pal: &BTreeMap<String, String>, base: Color32, dark: bool) -> BTreeMap<String, String> {
    let (white, ink) = (Color32::WHITE, Color32::from_rgb(0x0B, 0x0E, 0x13));
    let toward = if dark { white } else { Color32::BLACK };
    let panel = pal.get("bg.panel").and_then(|v| parse_hex(v)).unwrap_or(white);
    let touches = |p: &Pair| ACCENT_KEYS.contains(&p.0) || ACCENT_KEYS.contains(&p.1);
    let mut last = BTreeMap::new();
    for i in 0..=80 {
        let c = mix(base, toward, i as f32 * 0.0125);
        let (hov, pre) = if dark {
            (mix(c, white, 0.15), mix(c, white, 0.30))
        } else {
            (mix(c, Color32::BLACK, 0.12), mix(c, Color32::BLACK, 0.25))
        };
        let sub = mix(c, panel, if dark { 0.78 } else { 0.90 });
        let sub_hov = mix(sub, c, if dark { 0.04 } else { 0.08 });
        let worst = |t: Color32| contrast(t, c).min(contrast(t, hov)).min(contrast(t, pre));
        let on = if worst(white) >= worst(ink) { white } else { ink };
        let mut out = BTreeMap::new();
        for (k, v) in [
            ("accent", hex(c)),
            ("accent.hover", hex(hov)),
            ("accent.pressed", hex(pre)),
            ("accent.subtle", hex(sub)),
            ("accent.text_on", hex(on)),
            ("selection", hex(c)),
            ("selection.bg", hex(sub)),
            ("selection.text", hex(c)),
            ("selection.fill", hex_a(c, if dark { 0x33 } else { 0x26 })),
            ("focus", hex(c)),
            ("focus.ring", hex_a(c, 0xCC)),
            ("bg.selected", hex(sub)),
            ("bg.selected.hover", hex(sub_hov)),
            ("border.focus", hex(c)),
        ] {
            out.insert(k.to_string(), v);
        }
        let mut full = pal.clone();
        full.extend(out.clone());
        if failing(&full, TEXT_PAIRS, AA_TEXT, touches).is_empty()
            && failing(&full, GRAPHIC_PAIRS, AA_GRAPHIC, touches).is_empty()
        {
            return out;
        }
        last = out;
    }
    last
}

/// Light and dark patches of a look with an accent (None: the look's own).
pub fn patches(
    look: &Look,
    accent: Option<Color32>,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let base = base_tokens();
    let dark_own = look.accent_dark.as_deref().and_then(parse_hex);
    let one = |mode: Mode, patch: &BTreeMap<String, String>| {
        let acc = match (accent, mode) {
            (Some(a), _) => a,
            (None, Mode::Dark) => dark_own.unwrap_or_else(|| look.accent_color()),
            (None, Mode::Light) => look.accent_color(),
        };
        let mut pal = base.palette(mode).clone();
        pal.extend(patch.clone());
        let mut out = patch.clone();
        // the designed accent keeps its designed hover, pressed and subtle tones
        let designed = base.color(mode, "accent").ok() == Some(acc)
            && ACCENT_KEYS.iter().all(|k| base.palette(mode).contains_key(*k));
        if !designed || !contrast_failures(&pal).is_empty() {
            out.extend(derive_accent(&pal, acc, mode == Mode::Dark));
        }
        out
    };
    (one(Mode::Light, &look.light), one(Mode::Dark, &look.dark))
}

/// Cached `patches`: the settings page and every frame ask for the same pair.
pub fn patches_cached(
    look: &Look,
    accent: Option<Color32>,
) -> std::sync::Arc<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    type Key = (String, Option<[u8; 4]>);
    type Val = std::sync::Arc<(BTreeMap<String, String>, BTreeMap<String, String>)>;
    static C: OnceLock<std::sync::Mutex<Vec<(Key, Val)>>> = OnceLock::new();
    let key = (look.id.clone(), accent.map(|c| c.to_array()));
    let m = C.get_or_init(Default::default);
    let mut g = m.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, v)) = g.iter().find(|(k, _)| *k == key) {
        return v.clone();
    }
    let v = std::sync::Arc::new(patches(look, accent));
    if g.len() > 64 {
        g.clear();
    }
    g.push((key, v.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn full(mode: Mode, patch: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut p = Tokens::embedded().unwrap().palette(mode).clone();
        p.extend(patch.clone());
        p
    }

    #[test]
    fn seven_presets_with_known_parts() {
        let ids: Vec<&str> = presets().iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            ["studio", "engineer", "night", "focus", "contrast", "colour", "classic"]
        );
        let t = Tokens::embedded().unwrap();
        for l in presets() {
            assert!(["system", "light", "dark"].contains(&l.theme.as_str()), "{}", l.id);
            assert!(UiFont::parse(&l.font).is_some(), "{}: font", l.id);
            assert!(RibbonLook::parse(&l.ribbon).is_some(), "{}: ribbon", l.id);
            assert!(density_of(&l.density).is_some(), "{}: density", l.id);
            assert!(crate::icons::set(&l.icons).is_some(), "{}: icons", l.id);
            assert!(parse_hex(&l.accent).is_some(), "{}: accent", l.id);
            assert!((SCALE_MIN..=SCALE_MAX).contains(&l.ui_scale), "{}", l.id);
            assert!((TEXT_MIN..=TEXT_MAX).contains(&l.text_size), "{}", l.id);
            assert!((0.0..=2.0).contains(&l.radius_scale), "{}", l.id);
            for (mode, patch) in [(Mode::Light, &l.light), (Mode::Dark, &l.dark)] {
                for (k, v) in patch {
                    assert!(t.palette(mode).contains_key(k), "{}: unknown {k}", l.id);
                    assert!(parse_hex(v).is_some(), "{}: {k}={v}", l.id);
                    assert!(!ACCENT_KEYS.contains(&k.as_str()), "{}: {k} is derived", l.id);
                }
            }
        }
    }

    /// A preset lands in the theme: its neutrals and its fitted accent are what widgets read.
    #[test]
    fn each_preset_applies_its_tokens() {
        for l in presets() {
            let p = patches(l, None);
            let mut th = Theme::new(Mode::Light, Density::Compact);
            th.set_palette(&p.0, &p.1);
            for (mode, patch) in [(Mode::Light, &p.0), (Mode::Dark, &p.1)] {
                th.mode = mode;
                for (k, v) in patch {
                    assert_eq!(th.c(k), parse_hex(v).unwrap(), "{} {mode:?} {k}", l.id);
                }
                assert_eq!(th.sem().bg_panel, th.c("bg.panel"), "{} {mode:?}", l.id);
                assert_eq!(th.sem().accent, th.c("accent"), "{} {mode:?}", l.id);
                assert!(patch.contains_key("accent") || l.id == "studio", "{} {mode:?}", l.id);
            }
        }
    }

    /// Text ≥ 4.5:1 and meaningful graphics ≥ 3:1 for every preset, with its own accent and
    /// every swatch, in both themes.
    #[test]
    fn every_preset_and_accent_pass_aa() {
        let mut bad = Vec::new();
        for l in presets() {
            let mut acc: Vec<Option<Color32>> = vec![None];
            acc.extend(accents().into_iter().map(Some));
            for a in acc {
                let p = patches(l, a);
                for (mode, patch) in [(Mode::Light, &p.0), (Mode::Dark, &p.1)] {
                    for f in contrast_failures(&full(mode, patch)) {
                        bad.push(format!("{} {mode:?} {a:?}: {f}", l.id));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// Any custom colour is fitted, even ones that start far off (yellow on light, navy on dark).
    #[test]
    fn custom_accents_are_fitted() {
        for hexs in ["#FFE000", "#00FFFF", "#101060", "#FF00FF", "#808080", "#000000", "#FFFFFF"] {
            let c = parse_hex(hexs).unwrap();
            for l in presets() {
                let p = patches(l, Some(c));
                for (mode, patch) in [(Mode::Light, &p.0), (Mode::Dark, &p.1)] {
                    let f = contrast_failures(&full(mode, patch));
                    assert!(f.is_empty(), "{} {mode:?} {hexs}: {f:?}", l.id);
                }
            }
        }
    }

    #[test]
    fn the_default_look_changes_nothing_but_keeps_the_designed_accent() {
        let p = patches(default_look(), None);
        assert!(p.0.is_empty() && p.1.is_empty(), "{p:?}");
        // a swatch on the default look is fitted, not the designed one
        let q = patches(default_look(), Some(Color32::from_rgb(0x0F, 0x76, 0x6E)));
        assert!(q.0.contains_key("accent") && q.1.contains_key("accent"));
    }

    #[test]
    fn density_keys_map_to_three_densities() {
        let d: Vec<_> = DENSITIES.iter().map(|k| density_of(k).unwrap()).collect();
        assert_eq!(d, [Density::Dense, Density::Compact, Density::Comfortable]);
        assert_eq!(density_of("auto"), None);
        let t = Tokens::embedded().unwrap();
        assert!(t.density.dense.row < t.density.compact.row);
        assert!(t.density.compact.row < t.density.comfortable.row);
    }

    #[test]
    fn seven_icon_sets_and_three_faces() {
        let ids: Vec<&str> = crate::icons::sets().iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), 7, "{ids:?}");
        let mut u = ids.clone();
        u.sort_unstable();
        u.dedup();
        assert_eq!(u.len(), 7);
        assert_eq!(crate::icons::style(), crate::icons::set("studio").unwrap().style());
        for f in UiFont::ALL {
            let fd = crate::theme::font_definitions_for(f);
            assert_eq!(fd.font_data.len() >= 3, true, "{f:?}");
            assert_eq!(UiFont::parse(f.key()), Some(f));
        }
        // the two bundled faces differ: the choice is real
        let a = crate::theme::font_definitions_for(UiFont::Sans);
        let b = crate::theme::font_definitions_for(UiFont::Plex);
        assert_ne!(
            a.font_data[crate::theme::W_REGULAR].font.len(),
            b.font_data[crate::theme::W_REGULAR].font.len()
        );
    }
}
