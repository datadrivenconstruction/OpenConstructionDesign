// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Non-English fixture strings for integration tests, read from a data file.
use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &str = include_str!("../data/app2_fixtures.json");

/// Fixture text by key; panics on an unknown key.
#[allow(dead_code)]
pub fn fx(key: &str) -> String {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_str(DATA).expect("app2_fixtures.json"))
        .get(key)
        .unwrap_or_else(|| panic!("missing fixture key {key}"))
        .clone()
}
