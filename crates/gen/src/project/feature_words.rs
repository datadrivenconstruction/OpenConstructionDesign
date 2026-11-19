//! Brief words in several languages for the features the brief check judges, read from
//! `data/feature_words.json`: "Lüftung", the Russian word for ventilation and "通风" all say `hvac`.

use serde_json::Value as J;
use std::sync::OnceLock;

const DATA: &str = include_str!("../../data/feature_words.json");

fn norm(w: &str) -> String {
    w.trim().to_lowercase().replace([' ', '-'], "_")
}

/// Feature keyword and every word in any language that names it.
fn table() -> &'static Vec<(String, Vec<String>)> {
    static CELL: OnceLock<Vec<(String, Vec<String>)>> = OnceLock::new();
    CELL.get_or_init(|| {
        let j: J = serde_json::from_str(DATA).expect("feature_words.json");
        j["features"]
            .as_object()
            .expect("feature_words.json features")
            .iter()
            .map(|(id, langs)| {
                let words = langs
                    .as_object()
                    .into_iter()
                    .flat_map(|l| l.values())
                    .flat_map(|w| w.as_array().into_iter().flatten())
                    .filter_map(|w| w.as_str().map(norm))
                    .collect();
                (id.clone(), words)
            })
            .collect()
    })
}

fn find(word: &str) -> Option<&'static str> {
    table().iter().find(|(id, ws)| id == word || ws.iter().any(|x| x == word)).map(|(id, _)| id.as_str())
}

/// The keyword of a brief word: the word itself, lower case with underscores, when no list
/// names it. A count in front stays with it: "2 OP-Saal" is `operating_rooms:2`.
pub fn canon(word: &str) -> String {
    let k = norm(word);
    if let Some(id) = find(&k) {
        return id.to_string();
    }
    let digits = k.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let rest = k[digits..].trim_start_matches(['_', 'x']);
        if let Some(id) = find(rest) {
            return format!("{id}:{}", &k[..digits]);
        }
    }
    k
}
