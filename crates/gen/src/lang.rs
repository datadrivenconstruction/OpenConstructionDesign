//! Russian display names of generated objects, keyed by their English name (`data/lang/ru/names.json`).
use std::collections::HashMap;
use std::sync::OnceLock;

const NAMES: &str = include_str!("../data/lang/ru/names.json");

const BRIEF_WORDS: &str = include_str!("../data/lang/ru/brief_words.json");

/// Russian keywords of a brief under `key` (`data/lang/ru/brief_words.json`).
pub fn brief_words(key: &str) -> &'static [String] {
    static T: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(BRIEF_WORDS).unwrap_or_default()).get(key).map_or(&[][..], Vec::as_slice)
}

/// Russian name for an English one; the English name itself when the table has none.
pub fn ru(en: &str) -> &str {
    static T: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    let t = T.get_or_init(|| {
        let m: HashMap<String, String> = serde_json::from_str(NAMES).unwrap_or_default();
        m.into_iter().map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect()
    });
    t.get(en).copied().unwrap_or(en)
}
