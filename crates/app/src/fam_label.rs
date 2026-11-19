// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Display names of built-in families and their types. The English name stays the id (files,
//! the model and commands use it); only what the person reads is translated.
use ddc_ui::i18n::Lang;

/// Leading words of built-in type names, longest first: «Three gang 9 in», «Floor 650».
const TYPE_WORDS: &[&str] = &[
    "Wall-hung",
    "Three gang",
    "Two gang",
    "One gang",
    "Compact",
    "Accessible",
    "Standard",
    "Double",
    "Single",
    "Round",
    "Sedan",
    "Queen",
    "Floor",
    "Large",
    "Small",
    "Box",
];

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

/// Family name in `lang`; an own (user) family keeps the name as typed.
pub fn family(lang: Lang, name: &str) -> String {
    let key = format!("fam.name.{}", slug(name));
    let s = crate::catalog().t_in(lang, &key);
    if s == key {
        name.to_owned()
    } else {
        s
    }
}

/// Type name in `lang`: the leading word is translated, sizes stay (for Russian, "Floor 650" becomes the Russian "Floor" word plus 650).
pub fn type_name(lang: Lang, name: &str) -> String {
    for w in TYPE_WORDS {
        let Some(rest) = name.strip_prefix(w) else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with(' ')) {
            continue;
        }
        let key = format!("fam.type.{}", slug(w));
        let s = crate::catalog().t_in(lang, &key);
        return if s == key { name.to_owned() } else { format!("{s}{rest}") };
    }
    name.to_owned()
}

/// Built-in material name in `lang` (`mat.<id>`); an own material keeps its name as typed.
pub fn material(lang: Lang, name: &str) -> String {
    let key = format!("mat.{name}");
    let s = crate::catalog().t_in(lang, &key);
    if s == key {
        name.to_owned()
    } else {
        s
    }
}

/// Name of a purge row in `lang`: families, their types and built-in materials are translated.
pub fn purge_item(lang: Lang, table: &str, name: &str) -> String {
    match table {
        "comp_family" => family(lang, name),
        "comp_type" => type_name(lang, name),
        "material" => material(lang, name),
        _ => name.to_owned(),
    }
}

/// «Family : Type» label in `lang`.
pub fn label(lang: Lang, label: &str) -> String {
    match label.split_once(" : ") {
        Some((f, t)) => format!("{} : {}", family(lang, f), type_name(lang, t)),
        None => family(lang, label),
    }
}

/// The name or its Russian form contains the lowercase query: search works in either language.
pub fn matches(name: &str, q: &str) -> bool {
    name.to_lowercase().contains(q) || family(Lang::Ru, name).to_lowercase().contains(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_names_follow_the_language_and_keep_sizes() {
        assert_eq!(family(Lang::En, "Ceiling Light"), "Ceiling Light");
        assert_eq!(family(Lang::Ru, "Ceiling Light"), crate::app2_fx::fx("fam_ceiling_light"));
        assert_eq!(label(Lang::Ru, "WC : Floor 650"), crate::app2_fx::fx("fam_wc_floor"));
        assert_eq!(label(Lang::En, "WC : Floor 650"), "WC : Floor 650");
        assert_eq!(type_name(Lang::Ru, "Three gang 9 in"), crate::app2_fx::fx("fam_three_gang"));
        // numbers-only type names and user families are left as typed
        assert_eq!(type_name(Lang::Ru, "1600x800"), "1600x800");
        assert_eq!(family(Lang::Ru, "My Crate"), "My Crate");
        assert_eq!(type_name(Lang::Ru, "Floors"), "Floors");
    }

    #[test]
    fn every_built_in_family_and_type_word_is_translated() {
        for f in ddc_model::library().unwrap() {
            let ru = family(Lang::Ru, &f.name);
            assert_ne!(ru, f.name, "family {} has no ru name", f.name);
            for t in &f.types {
                let ru = type_name(Lang::Ru, &t.name);
                let word = TYPE_WORDS.iter().any(|w| t.name.starts_with(w));
                assert!(!word || ru != t.name, "type {} not translated", t.name);
            }
        }
        for f in ddc_model::library().unwrap() {
            for t in &f.types {
                let first = t.name.split(' ').next().unwrap_or("");
                let plain = first.chars().next().is_some_and(|c| c.is_ascii_digit())
                    || first.starts_with(|c: char| c.is_ascii_uppercase())
                        && first.chars().any(|c| c.is_ascii_digit())
                        && first.chars().skip(1).all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.' || c == '-');
                assert!(
                    plain || TYPE_WORDS.iter().any(|w| t.name.starts_with(w)),
                    "type {} is neither a size nor a known word",
                    t.name
                );
            }
        }
    }
}
