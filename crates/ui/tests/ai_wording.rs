//! AI-access wording: one noun ("AI agent") and matching en/ru key sets.
//! Reads the JSON files directly; never touches the global UI language.
use std::collections::BTreeMap;

fn parse(s: &str) -> BTreeMap<String, String> {
    let m: BTreeMap<String, serde_json::Value> = serde_json::from_str(s).expect("valid json");
    m.into_iter()
        .map(|(k, v)| (k, v.as_str().unwrap_or_default().to_string()))
        .collect()
}

const EN: [(&str, &str); 2] = [
    ("ai.json", include_str!("../data/i18n/en/ai.json")),
    ("mcp_bridge.json", include_str!("../data/i18n/en/mcp_bridge.json")),
];
const RU: [(&str, &str); 2] = [
    ("ai.json", include_str!("../data/i18n/ru/ai.json")),
    ("mcp_bridge.json", include_str!("../data/i18n/ru/mcp_bridge.json")),
];

#[test]
fn en_ai_files_have_no_stray_client_term() {
    for (name, src) in EN {
        for (k, v) in parse(src) {
            let low = v.to_lowercase();
            assert!(!low.contains("client"), "{name}: {k} says client: {v}");
            for at in low.match_indices("agent") {
                let before = &low[..at.0];
                assert!(
                    before.ends_with("ai "),
                    "{name}: {k} uses bare 'agent': {v}"
                );
            }
        }
    }
}

#[test]
fn en_and_ru_ai_key_sets_match() {
    for ((name, en), (_, ru)) in EN.iter().zip(RU.iter()) {
        let (e, r) = (parse(en), parse(ru));
        let ek: Vec<_> = e.keys().collect();
        let rk: Vec<_> = r.keys().collect();
        assert_eq!(ek, rk, "{name}: en/ru keys differ");
    }
}
