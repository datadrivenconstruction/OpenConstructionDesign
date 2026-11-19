//! Edit-path status texts: both locales carry the same keys, English renders without Cyrillic
//! and the touched sources keep no Cyrillic outside comments and test modules.
use std::collections::BTreeSet;

const EN: &str = include_str!("../../ui/data/i18n/en/modify_ops.json");
const RU: &str = include_str!("../../ui/data/i18n/ru/modify_ops.json");

fn table(s: &str) -> std::collections::BTreeMap<String, String> {
    serde_json::from_str(s).expect("valid locale json")
}

fn cyr(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn holes(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        if let Some(j) = rest[i..].find('}') {
            out.insert(rest[i + 1..i + j].to_string());
            rest = &rest[i + j + 1..];
        } else {
            break;
        }
    }
    out
}

#[test]
fn qa16d_keys_en_ru_same_set() {
    let (en, ru) = (table(EN), table(RU));
    assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
    for (k, v) in &en {
        assert_eq!(holes(v), holes(&ru[k]), "placeholders differ in {k}");
        assert!(!v.is_empty() && !ru[k].is_empty(), "{k}");
    }
}

#[test]
fn qa16d_statuses_en_no_cyrillic() {
    for (k, v) in table(EN) {
        assert!(!cyr(&v), "{k}: {v}");
        let got = crate::tr_lang(false, &k, &[]);
        assert_eq!(got, v, "{k} not served by the catalog");
    }
    for (k, v) in table(RU) {
        assert!(cyr(&v), "{k}");
        assert_eq!(crate::tr_lang(true, &k, &[]), v, "{k}");
    }
}

#[test]
fn qa16d_sources_no_cyrillic_in_code() {
    let files = [
        ("modify_tools.rs", include_str!("modify_tools.rs")),
        ("grips.rs", include_str!("grips.rs")),
        ("edit_bind.rs", include_str!("edit_bind.rs")),
        ("constraints_ui.rs", include_str!("constraints_ui.rs")),
    ];
    for (name, src) in files {
        let code = src.split("\n#[cfg(test)]").next().unwrap_or(src);
        for (n, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            let body = line.split("//").next().unwrap_or(line);
            assert!(!cyr(body), "{name}:{}: {line}", n + 1);
        }
    }
}
