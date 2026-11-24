// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The user guide names no release other than the workspace version.

use std::path::PathBuf;

#[test]
fn user_guide_names_no_stale_release() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/USER_GUIDE.md");
    let text = std::fs::read_to_string(&p).unwrap();
    let ours = env!("CARGO_PKG_VERSION");
    for w in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let w = w.trim_matches('.');
        let parts: Vec<&str> = w.split('.').collect();
        let semver = parts.len() == 3 && parts.iter().all(|s| !s.is_empty());
        assert!(!semver || w == ours, "docs/USER_GUIDE.md names release {w}, workspace is {ours}");
    }
}
