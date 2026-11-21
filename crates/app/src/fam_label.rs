// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Display names of built-in families and their types. The English name stays the id (files,
//! the model and commands use it); only what the person reads is translated.
use ddc_ui::i18n::Lang;

/// Most words a type word spans: «Three gang», «Date Palm».
const MAX_SPAN: usize = 2;

/// Catalog key of a type word, when the catalog has one: «Oak», «Date Palm», «slow».
fn word_key(words: &[&str]) -> Option<String> {
    let key = format!("fam.type.{}", slug(&words.join(" ")));
    (crate::catalog().t_in(Lang::En, &key) != key).then_some(key)
}

/// Type name split into its words, each with the catalog key of the word it starts, longest
/// first; sizes, codes and units have none.
fn type_words(name: &str) -> Vec<(String, Option<String>)> {
    let w: Vec<&str> = name.split(' ').collect();
    let mut out = vec![];
    let mut i = 0;
    while i < w.len() {
        let hit = (1..=MAX_SPAN.min(w.len() - i)).rev().find_map(|n| word_key(&w[i..i + n]).map(|k| (n, k)));
        let n = hit.as_ref().map_or(1, |h| h.0);
        out.push((w[i..i + n].join(" "), hit.map(|h| h.1)));
        i += n;
    }
    out
}

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

/// Type name in `lang`: every known word is translated, sizes and codes stay (for Russian,
/// «Oak A Autumn» becomes the Russian words for oak and autumn around the A).
pub fn type_name(lang: Lang, name: &str) -> String {
    let cat = crate::catalog();
    let words: Vec<String> = type_words(name)
        .into_iter()
        .map(|(w, k)| k.map_or(w, |k| cat.t_in(lang, &k)))
        .collect();
    words.join(" ")
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

    /// A word that reads the same in every language: a size («6», «400x600», «32in»), a unit
    /// («kW», «in»), a standard's acronym («DISS») or a variant letter («A»).
    fn size_or_code(w: &str) -> bool {
        const UNITS: [&str; 10] = ["in", "ft", "m", "mm", "l", "kg", "t", "kW", "kWh", "kVA"];
        let w = w.strip_suffix("in").filter(|s| s.ends_with(|c: char| c.is_ascii_digit())).unwrap_or(w);
        // a one-letter mark before a size is a code too: t200 is a thickness of 200
        let marked = w.len() > 1
            && w.starts_with(|c: char| c.is_ascii_lowercase())
            && w[1..].starts_with(|c: char| c.is_ascii_digit());
        UNITS.contains(&w)
            || marked
            || w.chars().next().is_some_and(|c| c.is_ascii_digit())
            || w.chars().all(|c| c.is_ascii_uppercase())
            || w.starts_with(|c: char| c.is_ascii_uppercase())
                && w.chars().any(|c| c.is_ascii_digit())
                && w.chars().skip(1).all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '.' || c == '-')
    }

    #[test]
    fn size_or_code_tells_sizes_from_words() {
        for w in ["6", "400x600", "32in", "kW", "DISS", "A", "H3.6", "H130in", "FC-32in", "l", "m", "t200", "t8in"] {
            assert!(size_or_code(w), "{w}");
        }
        for w in ["Wide", "Oak", "slow", "Lift130in", "core", "fan", "span"] {
            assert!(!size_or_code(w), "{w}");
        }
    }

    #[test]
    fn every_word_of_a_type_name_is_translated() {
        assert_eq!(type_name(Lang::En, "Oak A Autumn"), "Oak A Autumn");
        let ru = type_name(Lang::Ru, "Oak A Autumn");
        assert!(ru.contains(" A ") && !ru.contains("Oak") && !ru.contains("Autumn"), "{ru}");
        // a two-word word is one word: «Stair fan», not «Stair» plus «fan»
        assert_eq!(type_words("Stair fan 47in").len(), 2);
        assert_eq!(type_words("Date Palm B")[0].0, "Date Palm");
    }

    #[test]
    fn every_built_in_family_and_type_word_is_translated() {
        for f in ddc_model::library().unwrap() {
            let ru = family(Lang::Ru, &f.name);
            assert_ne!(ru, f.name, "family {} has no ru name", f.name);
            for t in &f.types {
                let ru = type_name(Lang::Ru, &t.name);
                let word = type_words(&t.name).iter().any(|w| w.1.is_some());
                assert!(!word || ru != t.name, "type {} not translated", t.name);
            }
        }
        for f in ddc_model::library().unwrap() {
            for t in &f.types {
                for (w, key) in type_words(&t.name) {
                    assert!(key.is_some() || size_or_code(&w), "type {}: {w} is neither a size, a code nor a known word", t.name);
                }
            }
        }
    }
}
