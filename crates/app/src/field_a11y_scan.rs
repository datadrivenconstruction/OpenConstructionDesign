// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every text/number input built in the app names itself through `labelled_by`.

use std::path::Path;

/// (file, id_salt in the chain, reason)
const ALLOW: &[(&str, &str, &str)] = &[
    (
        "import_win.rs",
        "imp.path",
        "no visible label widget to reference; the hint text is the only caption",
    ),
    (
        "xl_diff.rs",
        "xl-diff-search",
        "search box beside the sheet picker; the hint text is the only caption",
    ),
];

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn inputs_have_accessible_names() {
    let mut files = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    let mut bad = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        if name == "field_a11y_scan.rs" {
            continue;
        }
        let s = std::fs::read_to_string(&f).unwrap();
        for key in ["TextField::new(", "NumberField::new("] {
            for (at, _) in s.match_indices(key) {
                // a quoted pattern in a source-contract test is no input
                if s[..at].ends_with('"') {
                    continue;
                }
                let rest = &s[at..];
                let chain = &rest[..rest.find(".show(").unwrap_or(rest.len())];
                if chain.contains(".labelled_by(") {
                    continue;
                }
                let allowed = ALLOW
                    .iter()
                    .any(|(file, salt, _)| name == *file && chain.contains(salt));
                if !allowed {
                    let line = s[..at].matches('\n').count() + 1;
                    bad.push(format!("{name}:{line}"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "inputs without labelled_by: {bad:?}");
}
