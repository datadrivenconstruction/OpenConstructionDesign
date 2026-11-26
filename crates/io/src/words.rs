// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Words io writes into a model or a book (default type, family and room names, sheet notes),
//! from the UI catalogs: `crates/ui/data/i18n/<locale>/io_words.json`, embedded at build time.
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/io_words.rs"));

fn tables() -> &'static BTreeMap<&'static str, HashMap<String, String>> {
    static T: OnceLock<BTreeMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(|| {
        WORD_FILES
            .iter()
            .filter_map(|(l, t)| Some((*l, serde_json::from_str(t).ok()?)))
            .collect()
    })
}

/// The text of `key` in locale `lang` (`de`, `pt-BR`), its base language, then English; the key
/// itself when no bundle has it.
pub fn text(lang: &str, key: &str) -> String {
    let lang = lang.trim();
    let base = lang.split(['-', '_']).next().unwrap_or(lang);
    [lang, base, "en"]
        .iter()
        .find_map(|l| tables().get(l)?.get(key))
        .map_or_else(|| key.to_string(), String::clone)
}

/// [`text`] with `{name}` placeholders filled.
pub fn text_with(lang: &str, key: &str, args: &[(&str, &str)]) -> String {
    args.iter()
        .fold(text(lang, key), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
}

/// The language a model's documents are written in: its stored locale, else `fallback`.
pub fn model_lang(model: &ddc_model::Model, fallback: &str) -> String {
    match model.document_lang() {
        Ok(Some(l)) => l,
        _ if fallback.trim().is_empty() => "en".to_string(),
        _ => fallback.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_has_an_english_source_and_unknown_locales_fall_back() {
        let en = tables().get("en").expect("en io_words.json");
        assert!(!en.is_empty());
        for (l, t) in tables() {
            for k in t.keys() {
                assert!(en.contains_key(k), "{l}: {k} absent from the English source");
            }
        }
        for k in en.keys() {
            assert_eq!(text("xx-YY", k), en[k]);
        }
        assert_eq!(text("en", "no.such.word"), "no.such.word");
    }
}
