// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Type system (docs/design/TYPOGRAPHY.md): every piece of UI text has a role; a role fixes
//! size, line height, weight, tracking, colour token and case. Values mirror `font.scale` in
//! tokens.json (checked by `roles_match_tokens`). All digits are tabular (baked into the font).
use crate::theme::{family, W_MEDIUM, W_REGULAR, W_SEMIBOLD};
use egui::{FontFamily, FontId};

/// Type roles. Pick by what the text is, not by the size you want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Secondary one-liners: hints, sub-titles, units, timestamps.
    Caption,
    /// Chips, segmented controls, field labels.
    Label,
    /// Compact body: dense lists, menus inside panels.
    Small,
    Body,
    BodyStrong,
    /// Small-caps section label (upper case, tracked out).
    Section,
    /// Palette search, larger panel titles.
    Title,
    /// Start page and empty-state headlines.
    Heading,
    /// Values and coordinates in body size.
    Numeric,
    WindowTitle,
    /// Ribbon and document tabs.
    Tab,
    /// Ribbon button captions (large two-line and medium one-line).
    RibbonLabel,
    /// Ribbon group titles under the band.
    GroupTitle,
    PanelHeader,
    TableHeader,
    TableCell,
    /// Ids, formulas, expressions, raw coordinates (monospace slot, tabular digits).
    Code,
    Keycap,
    TooltipTitle,
    TooltipBody,
    DialogTitle,
    /// Status bar text and counters.
    Status,
    /// On-canvas annotation: section names, temporary labels.
    CanvasLabel,
    /// On-canvas tags: panel areas, contour heights, view-cube edges.
    CanvasTag,
    /// Smallest on-canvas glyphs: view-cube faces, badge marks. Never below 9.
    CanvasMicro,
}

/// Weights shipped: 400, 500, 600 (no bold: too heavy at 11-13 px in a dense UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
}

impl Weight {
    pub fn css(self) -> u16 {
        match self {
            Weight::Regular => 400,
            Weight::Medium => 500,
            Weight::SemiBold => 600,
        }
    }
    pub fn face(self) -> &'static str {
        match self {
            Weight::Regular => W_REGULAR,
            Weight::Medium => W_MEDIUM,
            Weight::SemiBold => W_SEMIBOLD,
        }
    }
}

/// One role, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    /// Key in `font.scale`.
    pub key: &'static str,
    pub size: f32,
    pub line: f32,
    pub weight: Weight,
    /// Extra letter spacing; negative from 14 up follows the face's own dynamic tracking.
    pub tracking: f32,
    /// Colour token (`color.<mode>.*`).
    pub color: &'static str,
    pub caps: bool,
    pub mono: bool,
}

const fn s(
    key: &'static str,
    size: f32,
    line: f32,
    weight: Weight,
    tracking: f32,
    color: &'static str,
) -> Spec {
    Spec {
        key,
        size,
        line,
        weight,
        tracking,
        color,
        caps: false,
        mono: false,
    }
}

use Weight::{Medium as M, Regular as R, SemiBold as B};
const PRI: &str = "text.primary";
const SEC: &str = "text.secondary";

impl Role {
    pub const ALL: [Role; 25] = [
        Role::Caption,
        Role::Label,
        Role::Small,
        Role::Body,
        Role::BodyStrong,
        Role::Section,
        Role::Title,
        Role::Heading,
        Role::Numeric,
        Role::WindowTitle,
        Role::Tab,
        Role::RibbonLabel,
        Role::GroupTitle,
        Role::PanelHeader,
        Role::TableHeader,
        Role::TableCell,
        Role::Code,
        Role::Keycap,
        Role::TooltipTitle,
        Role::TooltipBody,
        Role::DialogTitle,
        Role::Status,
        Role::CanvasLabel,
        Role::CanvasTag,
        Role::CanvasMicro,
    ];

    pub const fn spec(self) -> Spec {
        match self {
            Role::Caption => s("caption", 11.0, 14.0, R, 0.0, SEC),
            Role::Label => s("label", 12.0, 16.0, M, 0.0, PRI),
            Role::Small => s("small", 12.0, 16.0, R, 0.0, PRI),
            Role::Body => s("body", 13.0, 18.0, R, 0.0, PRI),
            Role::BodyStrong => s("body.strong", 13.0, 18.0, B, 0.0, PRI),
            Role::Section => Spec {
                caps: true,
                ..s("section", 11.0, 16.0, M, 0.4, SEC)
            },
            Role::Title => s("title", 14.0, 20.0, B, -0.1, PRI),
            Role::Heading => s("heading", 18.0, 24.0, B, -0.25, PRI),
            Role::Numeric => s("numeric", 13.0, 18.0, R, 0.0, PRI),
            Role::WindowTitle => s("window_title", 12.0, 16.0, M, 0.0, PRI),
            Role::Tab => s("tab", 12.0, 16.0, M, 0.0, SEC),
            Role::RibbonLabel => s("ribbon_label", 12.0, 16.0, R, 0.0, PRI),
            Role::GroupTitle => s("group_title", 12.0, 16.0, R, 0.0, SEC),
            Role::PanelHeader => s("panel_header", 13.0, 18.0, B, 0.0, PRI),
            Role::TableHeader => s("table_header", 12.0, 16.0, M, 0.0, SEC),
            Role::TableCell => s("table_cell", 12.0, 16.0, R, 0.0, PRI),
            Role::Code => Spec {
                mono: true,
                ..s("code", 12.0, 16.0, R, 0.0, PRI)
            },
            Role::Keycap => s("keycap", 11.0, 14.0, R, 0.2, SEC),
            Role::TooltipTitle => s("tooltip_title", 12.0, 16.0, B, 0.0, PRI),
            Role::TooltipBody => s("tooltip_body", 12.0, 16.0, R, 0.0, SEC),
            Role::DialogTitle => s("dialog_title", 14.0, 20.0, B, -0.1, PRI),
            Role::Status => s("status", 12.0, 16.0, R, 0.0, SEC),
            Role::CanvasLabel => s("canvas_label", 11.0, 14.0, R, 0.0, SEC),
            Role::CanvasTag => s("canvas_tag", 10.0, 12.0, M, 0.0, SEC),
            Role::CanvasMicro => s("canvas_micro", 9.0, 11.0, M, 0.1, SEC),
        }
    }

    pub fn family(self) -> FontFamily {
        let sp = self.spec();
        if sp.mono {
            FontFamily::Monospace
        } else {
            family(sp.weight.face())
        }
    }
}

/// Font of a role; usable without a `Theme` (canvas painters).
pub fn font(role: Role) -> FontId {
    FontId::new(role.spec().size, role.family())
}

/// Text in the role's case (`Section` is upper case), shaped and reordered for drawing when it
/// holds right-to-left letters (`rtl::display`).
pub fn cased(role: Role, text: &str) -> String {
    let t = if role.spec().caps { text.to_uppercase() } else { text.to_owned() };
    match crate::rtl::display(&t) {
        std::borrow::Cow::Borrowed(_) => t,
        std::borrow::Cow::Owned(s) => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Mode, Tokens, TOKENS_JSON};

    /// The ribbon and the status bar are read all day: nothing there is under 12 px.
    #[test]
    fn ribbon_and_status_text_is_at_least_12px() {
        for r in [Role::RibbonLabel, Role::GroupTitle, Role::Status, Role::Tab, Role::Label] {
            assert!(r.spec().size >= 12.0, "{r:?} is {} px", r.spec().size);
        }
    }

    #[test]
    fn roles_match_tokens() {
        let v: serde_json::Value = serde_json::from_str(TOKENS_JSON).unwrap();
        let scale = v["font"]["scale"].as_object().expect("font.scale");
        let t = Tokens::embedded().unwrap();
        for r in Role::ALL {
            let sp = r.spec();
            let j = scale
                .get(sp.key)
                .unwrap_or_else(|| panic!("font.scale.{} missing", sp.key));
            let n = |k: &str| j[k].as_f64().unwrap_or(0.0) as f32;
            assert_eq!(n("size"), sp.size, "{r:?} size");
            assert_eq!(n("line"), sp.line, "{r:?} line");
            assert_eq!(n("weight") as u16, sp.weight.css(), "{r:?} weight");
            assert!((n("tracking") - sp.tracking).abs() < 1e-6, "{r:?} tracking");
            assert_eq!(j["color"].as_str(), Some(sp.color), "{r:?} color");
            assert_eq!(j["caps"].as_bool().unwrap_or(false), sp.caps, "{r:?} caps");
            assert_eq!(j["family"].as_str() == Some("mono"), sp.mono, "{r:?} family");
            for mode in [Mode::Light, Mode::Dark] {
                assert!(t.color(mode, sp.color).is_ok(), "{r:?}: {}", sp.color);
            }
        }
        let keys: Vec<&str> = Role::ALL.iter().map(|r| r.spec().key).collect();
        for k in scale.keys() {
            assert!(keys.contains(&k.as_str()), "font.scale.{k} has no Role");
        }
        assert_eq!(keys.len(), scale.len(), "duplicate role keys");
    }

    #[test]
    fn scale_is_coherent() {
        for r in Role::ALL {
            let sp = r.spec();
            assert!(sp.size >= 9.0, "{r:?}: below the 9 pt floor");
            assert_eq!(sp.line.fract(), 0.0, "{r:?}: line height on whole points");
            assert!(sp.line >= sp.size + 2.0, "{r:?}: line too tight");
            assert!(sp.line <= (sp.size * 1.5).ceil(), "{r:?}: line too loose");
            assert!(sp.tracking.abs() <= 0.5, "{r:?}: tracking");
            assert!(!sp.caps || sp.tracking > 0.0, "{r:?}: caps need tracking");
            assert!(sp.size < 14.0 || sp.tracking <= 0.0, "{r:?}: large text tracks in");
            assert!(sp.color.starts_with("text."), "{r:?}: text colour token");
            assert_eq!(font(r).size, sp.size);
        }
        assert_eq!(cased(Role::Section, "Grid"), "GRID");
        assert_eq!(cased(Role::Body, "Grid"), "Grid");
    }
    /// Every role resolves to an installed face, and digits share one advance in all of them.
    #[test]
    fn every_role_resolves_with_tabular_digits() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            for r in Role::ALL {
                let f = font(r);
                let w = |s: &str| {
                    ctx.fonts(|fo| {
                        fo.layout_no_wrap(s.to_owned(), f.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    })
                };
                let zero = w("0");
                assert!(zero > 0.0, "{r:?}: no glyphs");
                for d in '1'..='9' {
                    let dw = w(&d.to_string());
                    assert!((dw - zero).abs() < 0.01, "{r:?}: '{d}' {dw} vs '0' {zero}");
                }
                assert!((w("1111.11") - w("8888.88")).abs() < 0.01, "{r:?}: columns drift");
            }
        });
    }

    /// Audit 12: only figures and number signs are tabular. A hyphen or bracket as wide as a
    /// digit reads as a spaced word break ("Built - in", "Russian abbreviation - 200").
    #[test]
    fn hyphen_and_brackets_keep_their_own_width() {
        let ctx = egui::Context::default();
        let _ = crate::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |ctx| {
            for r in Role::ALL {
                let f = font(r);
                if f.family == egui::FontFamily::Monospace {
                    continue; // a code face: every glyph is one cell by design
                }
                let w = |s: &str| {
                    ctx.fonts(|fo| {
                        fo.layout_no_wrap(s.to_owned(), f.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    })
                };
                let digit = w("0");
                for ch in ["-", "\u{2010}", "\u{2011}", "(", ")", "[", "]", "*"] {
                    let cw = w(ch);
                    assert!(cw < 0.85 * digit, "{r:?}: {ch:?} is {cw}, a digit {digit}");
                }
                assert!(w("Built-in") < w("Built0in"), "{r:?}: hyphen as wide as a digit");
            }
        });
    }
}
