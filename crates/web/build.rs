// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! English texts of the model check findings, taken from the editor's catalog at build time.

use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/data/i18n/en");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut out = BTreeMap::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).map(|e| e.path()).collect();
    files.sort();
    for p in files.iter().filter(|p| p.extension().is_some_and(|e| e == "json")) {
        let text = std::fs::read_to_string(p).unwrap();
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&text) else {
            continue;
        };
        for (k, v) in map {
            if (k.starts_with("ai.check.") || k.starts_with("ai.realism.")) && v.is_string() {
                out.insert(k, v);
            }
        }
    }
    let dst = Path::new(&std::env::var("OUT_DIR").unwrap()).join("checks_en.json");
    std::fs::write(dst, serde_json::to_string(&out).unwrap()).unwrap();
}
