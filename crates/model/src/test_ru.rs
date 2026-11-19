// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Test-only access to Russian fixture strings kept in `tests/data/ru.json`.
use std::collections::HashMap;
use std::sync::OnceLock;

/// The Russian fixture string stored under `key`.
pub(crate) fn ru(key: &str) -> &'static str {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        serde_json::from_str(include_str!("../tests/data/ru.json")).expect("valid ru.json")
    });
    map.get(key).unwrap_or_else(|| panic!("missing fixture key {key}"))
}
