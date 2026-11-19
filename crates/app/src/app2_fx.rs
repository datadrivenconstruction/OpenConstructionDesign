//! Test-only access to non-English fixture strings kept in a data file.
use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &str = include_str!("../data/lang/ru/app2_fixtures.json");

/// Fixture text by key; panics on an unknown key.
pub(crate) fn fx(key: &str) -> String {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_str(DATA).expect("app2_fixtures.json"))
        .get(key)
        .unwrap_or_else(|| panic!("missing fixture key {key}"))
        .clone()
}
