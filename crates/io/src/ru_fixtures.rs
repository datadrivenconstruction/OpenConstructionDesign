// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Russian-language test fixtures, read from `tests/data/ru_fixtures.tsv` (`key<TAB>text`).
//! In the text, `\n` stands for a newline and `\t` for a tab.

use std::collections::HashMap;
use std::sync::OnceLock;

static DATA: &str = include_str!("../tests/data/ru_fixtures.tsv");

/// The fixture text stored under `key`; panics when the key is missing.
pub fn ru(key: &str) -> &'static str {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for line in DATA.lines() {
            if let Some((k, v)) = line.split_once('\t') {
                let v: &'static str = if v.contains('\\') {
                    Box::leak(v.replace("\\n", "\n").replace("\\t", "\t").into_boxed_str())
                } else {
                    v
                };
                m.insert(k, v);
            }
        }
        m
    });
    map.get(key).copied().unwrap_or_else(|| panic!("missing ru fixture '{key}'"))
}
