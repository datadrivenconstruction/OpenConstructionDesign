// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Per-kind import of real DWG files (paths in tests/data/dwg_kinds.tsv, files stay outside the repo).
//! Ignored: set DDC_DWG_CORPUS to the private corpus root (paths in the TSV are relative to it)
//! and run with --ignored. Expected counts marked '?' are printed
//! as `KIND_ROW path kind lowered dropped` for the first build and not asserted.

use ddc_io::{dwg, dxf};
use ddc_model::Model;

const TSV: &str = include_str!("data/dwg_kinds.tsv");
const LOWERED: [&str; 3] = ["spline", "ellipse", "mtext"];

struct Row {
    path: String,
    kind: String,
    lowered: Option<usize>,
    dropped: Option<usize>,
}

fn rows() -> Vec<Row> {
    TSV.lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let c: Vec<&str> = l.trim_end_matches('\r').split('\t').collect();
            Row {
                path: corpus_root().join(c[0]).to_string_lossy().into_owned(),
                kind: c[1].to_string(),
                lowered: c.get(2).and_then(|s| s.parse().ok()),
                dropped: c.get(3).and_then(|s| s.parse().ok()),
            }
        })
        .collect()
}

fn enabled() -> bool {
    std::env::var_os("DDC_DWG_CORPUS").is_some()
}

fn corpus_root() -> std::path::PathBuf {
    std::env::var_os("DDC_DWG_CORPUS").map(Into::into).unwrap_or_default()
}

fn import(path: &str) -> dxf::ImportReport {
    let doc = dwg::read_file(std::path::Path::new(path)).unwrap_or_else(|e| panic!("{path}: {e:?}"));
    let mut m = Model::open(":memory:").unwrap();
    dxf::import_into(&mut m, &doc).unwrap_or_else(|e| panic!("{path}: {e:?}"))
}

fn lowered_of(rep: &dxf::ImportReport, kind: &str) -> usize {
    match kind {
        "spline" | "ellipse" => rep.curves,
        "mtext" => rep.texts,
        _ => 0,
    }
}

#[test]
#[ignore = "needs DDC_DWG_CORPUS"]
fn dwg_kinds_real_counts_match() {
    if !enabled() {
        return;
    }
    for r in rows() {
        let rep = import(&r.path);
        let lowered = lowered_of(&rep, &r.kind);
        let dropped = rep.dropped.get(&r.kind).copied().unwrap_or(0);
        println!("KIND_ROW {}\t{}\t{lowered}\t{dropped}", r.path, r.kind);
        if let Some(e) = r.lowered {
            assert_eq!(lowered, e, "{} lowered {}", r.kind, r.path);
        }
        if let Some(e) = r.dropped {
            assert_eq!(dropped, e, "{} dropped {}", r.kind, r.path);
        }
    }
}

#[test]
#[ignore = "needs DDC_DWG_CORPUS"]
fn no_kind_in_skipped_unknown() {
    if !enabled() {
        return;
    }
    for r in rows().iter().filter(|r| LOWERED.contains(&r.kind.as_str())) {
        let rep = import(&r.path);
        assert!(
            !rep.dropped.contains_key(&r.kind),
            "{} still dropped ({}) in {}",
            r.kind,
            rep.dropped[&r.kind],
            r.path
        );
        assert!(lowered_of(&rep, &r.kind) > 0, "{} lowered nothing in {}", r.kind, r.path);
        let total: usize = rep.dropped.values().sum();
        assert_eq!(total, rep.skipped_unknown, "dropped map vs skipped_unknown in {}", r.path);
    }
}
