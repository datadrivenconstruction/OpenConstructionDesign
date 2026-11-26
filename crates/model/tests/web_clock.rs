// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The browser build has no OS clock: `std::time::Instant::now()` / `SystemTime::now()` panic on
//! wasm32. Crates that go into the editor take the clock from `web_time` (std on desktop).
//! Test files are exempt: they never run in the browser.

use std::path::Path;

const WEB_CRATES: &[&str] = &[
    "model", "geom", "solver", "ops", "io", "render", "app", "ui", "ai", "gen", "cli", "link",
    "script", "bridge-proto",
];
const CLOCK: &[&str] = &["Instant", "SystemTime", "UNIX_EPOCH"];

fn is_test_file(name: &str) -> bool {
    let stem = name.trim_end_matches(".rs");
    stem == "tests" || stem == "test" || stem.starts_with("tests_") || stem.ends_with("_tests")
}

fn scan(dir: &Path, hits: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            scan(&p, hits);
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        // wtime.rs is the one bridge between the file system's std times and web_time; it never reads the clock
        if !name.ends_with(".rs") || is_test_file(&name) || p.ends_with("model/src/wtime.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        for (i, line) in text.lines().enumerate() {
            for (at, _) in line.match_indices("std::time::") {
                let rest = &line[at + "std::time::".len()..];
                let end = if rest.starts_with('{') {
                    rest.find('}').unwrap_or(rest.len())
                } else {
                    rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len())
                };
                if CLOCK.iter().any(|c| rest[..end].contains(c)) {
                    hits.push(format!("{}:{}", p.display(), i + 1));
                }
            }
        }
    }
}

#[test]
fn editor_crates_take_the_clock_from_web_time() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for c in WEB_CRATES {
        scan(&crates.join(c).join("src"), &mut hits);
    }
    assert!(hits.is_empty(), "use web_time:: for Instant/SystemTime:\n{}", hits.join("\n"));
}
