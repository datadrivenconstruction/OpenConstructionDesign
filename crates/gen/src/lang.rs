//! Brief words beyond the English in the code: every `data/lang/<locale>/<list>.json` maps a key
//! to words, and a brief in any language is read with the words of every locale.
use std::collections::HashMap;
use std::sync::OnceLock;

/// The words of every locale under `key` of word list `list` (`brief_words`, `plant_brief`).
pub fn words(list: &str, key: &str) -> &'static [String] {
    static T: OnceLock<HashMap<(String, String), Vec<String>>> = OnceLock::new();
    let t = T.get_or_init(|| {
        let mut t: HashMap<(String, String), Vec<String>> = HashMap::new();
        for (_, stem, text) in crate::project::names::word_files() {
            let m: HashMap<String, Vec<String>> = serde_json::from_str(text).unwrap_or_default();
            for (k, ws) in m {
                let e = t.entry((stem.to_string(), k)).or_default();
                for w in ws {
                    if !e.contains(&w) {
                        e.push(w);
                    }
                }
            }
        }
        t
    });
    t.get(&(list.to_string(), key.to_string())).map_or(&[][..], Vec::as_slice)
}

/// Brief keywords under `key` (`data/lang/<locale>/brief_words.json`).
pub fn brief_words(key: &str) -> &'static [String] {
    words("brief_words", key)
}
