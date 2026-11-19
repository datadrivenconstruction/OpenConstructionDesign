//! One change language for every diff surface (AI plan card, AI viewport preview, XLSX import
//! preview, version compare): a kind owns its colour token, its icon and its label key. The icon
//! and the label always travel with the colour, so meaning never rests on colour alone.
use crate::icons::IconCache;
use crate::sem::icon_size;
use crate::theme::Theme;
use egui::{Color32, Response, Sense, Ui, Vec2};
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffKind {
    Added,
    Changed,
    Deleted,
    /// Not applied: the target is protected or the value is invalid.
    Refused,
    /// Both sides changed the same thing.
    Conflict,
}

impl DiffKind {
    pub const ALL: [DiffKind; 5] = [
        DiffKind::Added,
        DiffKind::Changed,
        DiffKind::Deleted,
        DiffKind::Refused,
        DiffKind::Conflict,
    ];

    /// Colour token in `tokens.json` (defined for both themes).
    pub const fn token(self) -> &'static str {
        match self {
            DiffKind::Added => "ai.created",
            DiffKind::Changed => "ai.changed",
            DiffKind::Deleted => "ai.deleted",
            DiffKind::Refused => "diff.refused",
            DiffKind::Conflict => "diff.conflict",
        }
    }

    /// Icon name: plus, pencil, minus, lock, split-arrows (`docs/design/icons_v4`).
    pub const fn icon(self) -> &'static str {
        match self {
            DiffKind::Added => "diff-added",
            DiffKind::Changed => "diff-changed",
            DiffKind::Deleted => "diff-deleted",
            DiffKind::Refused => "diff-refused",
            DiffKind::Conflict => "merge-conflict",
        }
    }

    /// i18n key of the short lower-case label ("added", "refused", ...).
    pub const fn label_key(self) -> &'static str {
        match self {
            DiffKind::Added => "diff.kind.added",
            DiffKind::Changed => "diff.kind.changed",
            DiffKind::Deleted => "diff.kind.deleted",
            DiffKind::Refused => "diff.kind.refused",
            DiffKind::Conflict => "diff.kind.conflict",
        }
    }

    /// Text sign for places that draw glyphs only (counters, canvas legends).
    pub const fn sign(self) -> &'static str {
        match self {
            DiffKind::Added => "+",
            DiffKind::Changed => "~",
            DiffKind::Deleted => "\u{2212}",
            DiffKind::Refused => "\u{00D7}",
            DiffKind::Conflict => "!",
        }
    }

    pub fn color(self, th: &Theme) -> Color32 {
        th.c(self.token())
    }

    /// Colour for painters that only know the dark flag (canvas overlays).
    pub fn color_for(self, dark: bool) -> Color32 {
        let mode = if dark { crate::theme::Mode::Dark } else { crate::theme::Mode::Light };
        TOKENS.with(|t| t.color(mode, self.token()).unwrap_or(Color32::MAGENTA))
    }
}

thread_local! {
    static ICONS: RefCell<IconCache> = RefCell::new(IconCache::default());
    static TOKENS: crate::theme::Tokens =
        crate::theme::Tokens::embedded().expect("embedded tokens are checked by tests");
}

/// The kind icon in its colour, square `icon_size::SM`; hover text is the caller's.
pub fn badge(ui: &mut Ui, k: DiffKind, color: Color32) -> Response {
    let side = icon_size::SM;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_icon(ui, k.icon(), rect, color);
    }
    resp
}

/// A named icon at `icon_size::SM` into `rect`, tinted; for painters that place it themselves.
pub fn paint_icon(ui: &Ui, name: &str, rect: egui::Rect, color: Color32) {
    let side = icon_size::SM;
    ICONS.with(|c| c.borrow_mut().paint(ui, name, side as u32, rect, color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Catalog, Lang};
    use crate::theme::{contrast, Mode, Tokens};

    #[test]
    fn every_kind_has_icons_in_every_set() {
        for k in DiffKind::ALL {
            let n = k.icon();
            assert!(crate::icons::source(n).is_some(), "{n}: outline");
            assert!(crate::icons::source(&format!("{n}.fill")).is_some(), "{n}: fill");
            for d in crate::icons::Direction::ALL {
                for m in crate::icons::MASTERS {
                    assert!(crate::icons::svg(d, m, n).is_some(), "{}/{m}/{n}", d.dir());
                }
            }
        }
        let mut icons: Vec<_> = DiffKind::ALL.iter().map(|k| k.icon()).collect();
        icons.sort();
        icons.dedup();
        assert_eq!(icons.len(), DiffKind::ALL.len(), "one icon per kind");
    }

    #[test]
    fn every_kind_has_a_token_in_both_themes() {
        let t = Tokens::embedded().unwrap();
        let mut seen = [Vec::new(), Vec::new()];
        for k in DiffKind::ALL {
            for (i, m) in [Mode::Light, Mode::Dark].into_iter().enumerate() {
                let c = t.color(m, k.token()).unwrap_or_else(|e| panic!("{:?}: {e:?}", k));
                seen[i].push(c);
                assert_eq!(c, k.color_for(m == Mode::Dark));
            }
        }
        for s in &mut seen {
            let n = s.len();
            s.sort_by_key(|c| c.to_array());
            s.dedup();
            assert_eq!(s.len(), n, "kind colours differ inside a theme");
        }
    }

    #[test]
    fn every_kind_has_en_and_ru_labels() {
        let cat = Catalog::new(Lang::En);
        let mut ru_seen = Vec::new();
        for k in DiffKind::ALL {
            for lang in [Lang::En, Lang::Ru] {
                let s = cat.table(lang).get(k.label_key()).and_then(|v| v.as_str());
                assert!(s.is_some_and(|s| !s.is_empty()), "{} in {:?}", k.label_key(), lang);
            }
            let (en, ru) = (cat.t_in(Lang::En, k.label_key()), cat.t_in(Lang::Ru, k.label_key()));
            assert_ne!(en, ru, "{}: ru must be translated", k.label_key());
            ru_seen.push(ru);
        }
        ru_seen.sort();
        ru_seen.dedup();
        assert_eq!(ru_seen.len(), DiffKind::ALL.len());
    }

    #[test]
    fn kind_colours_read_on_the_panel() {
        let t = Tokens::embedded().unwrap();
        for m in [Mode::Light, Mode::Dark] {
            let panel = t.color(m, "bg.panel").unwrap();
            for k in DiffKind::ALL {
                let c = t.color(m, k.token()).unwrap();
                let r = contrast(c, panel);
                assert!(r >= 3.0, "{:?} {:?}: {r:.2}:1", k, m);
            }
        }
    }
}
