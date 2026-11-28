// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan and section hatch of a material class. One table keyed by the library class
//! (`material_look::Look::key`), shared by new material rows, plan walls and section cuts, so a
//! class looks the same in every drawing. The patterns follow common drafting conventions:
//! dots for concrete, 45 degree lines for masonry, reverse lines for timber, dense lines for
//! metal, crossing lines for insulation, grid for stone, ticks for soil, horizontal lines for
//! water, sparse lines for glass, stipple for render and plaster.

use crate::material_look::look_of;
use crate::MATERIAL_HATCHES;

/// Built-in hatch keys beyond [`MATERIAL_HATCHES`].
pub const EXTRA_HATCHES: [&str; 6] = ["steel", "earth", "water", "stone", "stipple", "glass"];

/// A key the drawing code can paint without a library entry.
pub fn is_builtin_hatch(key: &str) -> bool {
    MATERIAL_HATCHES.contains(&key) || EXTRA_HATCHES.contains(&key)
}

/// Hatch of a library class; "none" for classes drawn plain.
pub fn class_hatch(class_key: &str) -> &'static str {
    match class_key {
        "concrete" | "gravel" => "dots",
        "brick" | "masonry" | "roof_tile" | "slate" => "diagonal",
        "timber" | "bark" => "back_diagonal",
        "insulation" => "cross",
        "steel" | "iron" | "copper" | "aluminium" | "cladding" => "steel",
        "glass" | "polycarbonate" => "glass",
        "stone" | "paving" | "ceramic" => "stone",
        "soil" => "earth",
        "water" => "water",
        "render" | "plaster" | "asphalt" => "stipple",
        _ => "none",
    }
}

/// Hatch of a material text: its class hatch, "none" when the text names no class.
pub fn default_hatch(name: &str) -> &'static str {
    look_of(name).map_or("none", |l| class_hatch(l.key))
}

/// Drawing class (index of the six poche colours: concrete, masonry, insulation, glass, wood,
/// other) of a library class.
pub fn poche_class(class_key: &str) -> usize {
    match class_key {
        "concrete" => 0,
        "brick" | "masonry" | "stone" | "paving" | "ceramic" | "roof_tile" | "slate" => 1,
        "insulation" => 2,
        "glass" | "polycarbonate" => 3,
        "timber" | "bark" => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material_look::LOOKS;

    #[test]
    fn every_class_hatch_is_a_builtin_key() {
        for l in LOOKS.iter() {
            let h = class_hatch(l.key);
            assert!(h == "none" || is_builtin_hatch(h), "{}: {h}", l.key);
            assert!(poche_class(l.key) < 6);
        }
    }

    #[test]
    fn materials_that_look_different_hatch_differently() {
        let h = |n: &str| default_hatch(n);
        assert_eq!(h("Concrete C30/37"), "dots");
        assert_eq!(h("clay brick"), "diagonal");
        assert_eq!(h("Glulam GL24h"), "back_diagonal");
        assert_eq!(h("steel S355"), "steel");
        assert_eq!(h("float glass"), "glass");
        assert_eq!(h("granite"), "stone");
        assert_eq!(h("mineral wool"), "cross");
        assert_eq!(h("topsoil"), "earth");
        assert_eq!(h("zzz unknown"), "none");
    }

    #[test]
    fn every_listed_local_word_names_a_class() {
        let j: serde_json::Value = serde_json::from_str(include_str!("../data/material_words.json")).unwrap();
        for (class, langs) in j.as_object().unwrap() {
            assert!(crate::material_look::look_by_key(class).is_some(), "{class}");
            for (lang, ws) in langs.as_object().unwrap() {
                for w in ws.as_array().unwrap().iter().filter_map(|w| w.as_str()) {
                    assert!(look_of(w).is_some(), "{class}/{lang}: {w}");
                }
            }
        }
    }

    #[test]
    fn a_row_may_use_any_builtin_hatch() {
        let mut m = crate::Model::open(":memory:").unwrap();
        for (i, k) in EXTRA_HATCHES.iter().enumerate() {
            let mut s = crate::material_defaults(&format!("Zz {i}"));
            s.hatch = (*k).into();
            m.add_material(&s).unwrap_or_else(|e| panic!("{k}: {e}"));
        }
        let mut bad = crate::material_defaults("Zz bad");
        bad.hatch = "nonsense".into();
        assert!(m.add_material(&bad).is_err());
    }
}
