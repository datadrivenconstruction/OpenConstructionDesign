// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The built-in family library passes the validation pipeline without errors.

use ddc_geom::component::validate::{validate, Limits, Severity};

#[test]
fn library_families_validate_ok() {
    let lim = Limits {
        eval_ms: 60_000,
        ..Limits::default()
    };
    let mut bad = vec![];
    for src in ddc_model::library_sources() {
        let r = validate(src, &lim);
        if !r.ok {
            let name = src.lines().find(|l| l.starts_with("family ")).unwrap_or("?");
            let errs: Vec<_> = r
                .findings
                .iter()
                .filter(|f| f.severity == Severity::Error)
                .map(|f| format!("{} {:?} {}", f.code, f.lines, f.msg))
                .collect();
            bad.push(format!("{name}: {errs:?}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
