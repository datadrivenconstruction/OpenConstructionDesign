// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Test-only lookup of non-English fixture text kept in `tests/data/lang_fixtures.tsv`.
use std::collections::HashMap;
use std::sync::OnceLock;

const RAW: &str = include_str!("../tests/data/lang_fixtures.tsv");

/// Fixture text by key; panics on an unknown key so a typo cannot pass silently.
pub fn fx(key: &str) -> &'static str {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        RAW.lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .collect()
    });
    map.get(key)
        .copied()
        .unwrap_or_else(|| panic!("unknown fixture key {key}"))
}
