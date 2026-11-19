// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Row model of the visibility and graphics tables: column sets, the name and tooltip of every
//! toggle, and the draft edits a row makes. No drawing here, so the rules are testable.

use ddc_model::{CatGfx, ViewGfx, GFX_CATEGORIES};

/// Override columns shared by both tables (line colour, weight, fill, transparency).
const OVER_COLS: [&str; 4] = [
    "view_gfx.over.line",
    "view_gfx.over.weight",
    "view_gfx.over.fill",
    "view_gfx.over.transparency",
];

/// Header keys of the category table: name with its visibility toggle, overrides, actions.
pub(super) const CAT_COLS: [&str; 6] = [
    "view_gfx.vv.category",
    OVER_COLS[0],
    OVER_COLS[1],
    OVER_COLS[2],
    OVER_COLS[3],
    "view_gfx.vv.col.actions",
];

/// Header keys of the filter table.
pub(super) const FILTER_COLS: [&str; 8] = [
    "view_gfx.vv.filter",
    "view_gfx.vv.enabled",
    "view_gfx.vv.visible",
    OVER_COLS[0],
    OVER_COLS[1],
    OVER_COLS[2],
    OVER_COLS[3],
    "view_gfx.vv.col.actions",
];

/// Every toggle the tables draw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Toggle {
    /// Category shown in the view; its label is the category name.
    CategoryVisible,
    FilterOn,
    FilterVisible,
}

impl Toggle {
    pub(super) const ALL: [Toggle; 3] = [
        Toggle::CategoryVisible,
        Toggle::FilterOn,
        Toggle::FilterVisible,
    ];

    /// Label key; `None` when the label is the row's own name (the category).
    pub(super) fn label_key(self) -> Option<&'static str> {
        match self {
            Toggle::CategoryVisible => None,
            Toggle::FilterOn => Some("view_gfx.vv.enabled"),
            Toggle::FilterVisible => Some("view_gfx.vv.visible"),
        }
    }

    pub(super) fn tip_key(self) -> &'static str {
        match self {
            Toggle::CategoryVisible | Toggle::FilterVisible => "view_gfx.vv.visible_tip",
            Toggle::FilterOn => "view_gfx.vv.enabled_tip",
        }
    }
}

/// One category line: its key and the setting as drafted.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct CatRow {
    pub cat: &'static str,
    pub gfx: CatGfx,
}

impl CatRow {
    /// The Reset action exists only while the row differs from the default.
    pub(super) fn show_reset(&self) -> bool {
        self.gfx != CatGfx::default()
    }
}

pub(super) fn cat_rows(draft: &ViewGfx) -> Vec<CatRow> {
    GFX_CATEGORIES
        .iter()
        .map(|&(cat, _)| CatRow {
            cat,
            gfx: draft.categories.get(cat).copied().unwrap_or_default(),
        })
        .collect()
}

/// Writes a row back into the draft; a default row is removed. True when the draft changed.
pub(super) fn set_cat(draft: &mut ViewGfx, cat: &str, c: CatGfx) -> bool {
    let before = draft.categories.get(cat).copied().unwrap_or_default();
    if before == c {
        return false;
    }
    if c == CatGfx::default() {
        draft.categories.remove(cat);
    } else {
        draft.categories.insert(cat.to_string(), c);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_model::ViewKey;

    fn catalog(lang: &str) -> serde_json::Map<String, serde_json::Value> {
        let (f5, own) = match lang {
            "en" => (
                include_str!("../../../ui/data/i18n/en/view_gfx.json"),
                include_str!("../../../ui/data/i18n/en/view_gfx_rows.json"),
            ),
            _ => (
                include_str!("../../../ui/data/i18n/ru/view_gfx.json"),
                include_str!("../../../ui/data/i18n/ru/view_gfx_rows.json"),
            ),
        };
        let mut all = serde_json::Map::new();
        for src in [f5, own] {
            let v: serde_json::Value = serde_json::from_str(src).unwrap();
            all.extend(v.as_object().unwrap().clone());
        }
        all
    }

    fn text<'a>(c: &'a serde_json::Map<String, serde_json::Value>, k: &str, lang: &str) -> &'a str {
        c.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{lang}: missing {k}"))
    }

    #[test]
    fn every_toggle_has_a_label_and_tip_in_both_languages() {
        for lang in ["en", "ru"] {
            let c = catalog(lang);
            for tg in Toggle::ALL {
                if let Some(k) = tg.label_key() {
                    assert!(!text(&c, k, lang).trim().is_empty(), "{lang}: {k}");
                }
                assert!(!text(&c, tg.tip_key(), lang).trim().is_empty());
            }
            for (cat, _) in GFX_CATEGORIES {
                let k = format!("view_gfx.cat.{cat}");
                assert!(!text(&c, &k, lang).trim().is_empty(), "{lang}: {k}");
            }
            for k in CAT_COLS.iter().chain(FILTER_COLS.iter()) {
                assert!(!text(&c, k, lang).trim().is_empty(), "{lang}: {k}");
            }
            for k in [
                "view_gfx.vv.reset_tip",
                "view_gfx.vv.nothing_to_apply",
                "view_gfx.flt.save_blocked",
                "view_gfx.flt.confirm_delete",
                "view_gfx.flt.param_pick",
                "view_gfx.vv.edit_filter",
            ] {
                assert!(!text(&c, k, lang).trim().is_empty(), "{lang}: {k}");
            }
        }
        // Russian values are Russian, not copied English
        let (en, ru) = (catalog("en"), catalog("ru"));
        for k in ["view_gfx.vv.visible_tip", "view_gfx.vv.enabled_tip", "view_gfx.vv.col.actions"] {
            assert_ne!(text(&en, k, "en"), text(&ru, k, "ru"), "{k}");
            assert!(text(&ru, k, "ru").chars().any(|c| c as u32 >= 0x400), "{k}");
        }
    }

    #[test]
    fn column_sets_are_stable() {
        assert_eq!(CAT_COLS.len(), 6);
        assert_eq!(FILTER_COLS.len(), 8);
        assert_eq!(CAT_COLS[0], "view_gfx.vv.category");
        assert_eq!(FILTER_COLS[1], "view_gfx.vv.enabled");
        assert_eq!(CAT_COLS[1..5], FILTER_COLS[3..7]);
        assert_eq!(cat_rows(&ViewGfx::default()).len(), GFX_CATEGORIES.len());
    }

    #[test]
    fn reset_is_offered_only_for_a_changed_row() {
        let mut g = ViewGfx::default();
        assert!(cat_rows(&g).iter().all(|r| !r.show_reset()));
        let hidden = CatGfx {
            visible: false,
            ..Default::default()
        };
        let cat = GFX_CATEGORIES[0].0;
        assert!(set_cat(&mut g, cat, hidden));
        let rows = cat_rows(&g);
        assert!(rows[0].show_reset() && !rows[0].gfx.visible);
        assert!(rows[1..].iter().all(|r| !r.show_reset()));
        assert!(!set_cat(&mut g, cat, hidden), "same value is no change");
        assert!(set_cat(&mut g, cat, CatGfx::default()));
        assert!(g.categories.is_empty(), "a default row is not stored");
    }

    #[test]
    fn changing_a_toggle_is_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        let v = ViewKey::ThreeD;
        let cat = GFX_CATEGORIES[0].0;
        let mut g = e.model.view_gfx(v).unwrap_or_default().effective;
        let steps = e.undo_len();
        assert!(set_cat(
            &mut g,
            cat,
            CatGfx {
                visible: false,
                ..Default::default()
            }
        ));
        let gc = g.clone();
        assert!(super::super::run(&mut e, move |m| m.set_view_gfx(v, &gc)));
        assert_eq!(e.undo_len(), steps + 1);
        let stored = e.model.view_gfx(v).unwrap_or_default().effective;
        assert!(!stored.categories.get(cat).unwrap().visible);
        e.undo();
        let back = e.model.view_gfx(v).unwrap_or_default().effective;
        assert!(back.categories.get(cat).map_or(true, |c| c.visible));
    }

    #[test]
    fn the_window_file_draws_no_raw_widgets() {
        let src = include_str!("vv_win.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        let banned = [
            ["egui::Combo", "Box"].concat(),
            [".check", "box("].concat(),
            ["radio", "_value"].concat(),
            ["selectable", "_value"].concat(),
            ["selectable", "_label"].concat(),
            ["ui.", "button("].concat(),
            ["small", "_button"].concat(),
            ["menu", "_button"].concat(),
            ["egui::", "Button"].concat(),
            ["Drag", "Value"].concat(),
            ["Text", "Edit"].concat(),
            ["ui.", "link("].concat(),
            ["ui.label", "(\"\")"].concat(),
        ];
        for b in &banned {
            assert!(!body.contains(b.as_str()), "raw widget in vv_win.rs: {b}");
        }
    }
}
