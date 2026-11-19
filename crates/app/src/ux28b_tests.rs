// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Strings moved to `i18n/{en,ru}/unit_fmt.json` exist in both languages with the same placeholders; EN has no Cyrillic.

use std::collections::BTreeMap;

const EN: &str = include_str!("../../ui/data/i18n/en/unit_fmt.json");
const RU: &str = include_str!("../../ui/data/i18n/ru/unit_fmt.json");

fn cyr(s: &str) -> bool {
    s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c))
}

fn table(s: &str) -> BTreeMap<String, String> {
    serde_json::from_str(s).expect("catalog json")
}

fn holes(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s
        .split('{')
        .skip(1)
        .filter_map(|r| r.split('}').next())
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

#[test]
fn keys_in_both_languages_same_placeholders_en_without_cyrillic() {
    let (en, ru) = (table(EN), table(RU));
    assert!(en.len() >= 34);
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    for (k, v) in &en {
        assert!(!cyr(v), "{k}: Cyrillic in the English text");
        assert_eq!(holes(v), holes(&ru[k]), "{k}: placeholders differ");
        // explicit language: the global UI language is never touched
        assert_eq!(&crate::tr_lang(false, k, &[]), v, "{k}");
        assert_eq!(&crate::tr_lang(true, k, &[]), &ru[k], "{k}");
    }
}

#[test]
fn en_texts_with_filled_placeholders_have_no_cyrillic() {
    let a = &[
        ("p", "3"), ("u", "x"), ("max", "1000"), ("k", "a"), ("s", "b"), ("key", "w"),
        ("name", "n"), ("why", "w"), ("n", "2"), ("e", "e"), ("arg", "f"), ("kind", "wall"),
        ("id", "1"), ("g", "G"), ("owner_kind", "door"), ("owner_id", "2"),
    ];
    for k in table(EN).keys() {
        assert!(!cyr(&crate::tr_lang(false, k, a)), "{k}");
    }
}
