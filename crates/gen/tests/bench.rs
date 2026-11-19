// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Generator regression benchmark: the briefs of `data/bench`, measured on the built model by
//! `bench/suite.rs`. Fails when a metric of any brief worsens beyond its tolerance against
//! `bench/baseline.json`. Re-pin with `DDC_BENCH_UPDATE=1` only when a change of result is
//! intended; the report goes to `target/ddc-bench-report.json` (or `DDC_BENCH_OUT`).

#[path = "../bench/suite.rs"]
mod suite;

use serde_json::{json, Value as J};
use std::collections::BTreeSet;

const GROUPS: [&str; 13] = [
    "house",
    "villa",
    "apartment_block",
    "office",
    "office_tower",
    "hotel",
    "school",
    "clinic",
    "mixed_use",
    "irregular_site",
    "steep_site",
    "basement",
    "free_form",
];

#[test]
fn the_bench_covers_thirty_briefs_every_group_and_fourteen_metrics() {
    let briefs = suite::load_briefs();
    assert!(briefs.len() >= 30, "{} briefs", briefs.len());
    let groups: BTreeSet<&str> = briefs.iter().map(|b| b.group.as_str()).collect();
    for g in GROUPS {
        assert!(groups.contains(g), "no brief of group {g}");
    }
    assert!(groups.iter().all(|g| GROUPS.contains(g)), "unknown group in {groups:?}");
    assert!(suite::METRICS.len() >= 14);
    let ids: BTreeSet<&str> = suite::METRICS.iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), suite::METRICS.len(), "metric ids are unique");
    assert!(suite::METRICS.iter().all(|m| m.short.len() <= 7 && m.tol >= 0.0));
    assert!(briefs.iter().any(|b| b.plot.is_some() && b.slope.is_some()), "a steep site");
}

fn report_path() -> std::path::PathBuf {
    std::env::var_os("DDC_BENCH_OUT")
        .map_or_else(|| suite::crate_dir().join("../../target/ddc-bench-report.json"), Into::into)
}

#[test]
fn the_bench_holds_against_the_baseline() {
    let rows = suite::run_all(&suite::load_briefs());
    let report = suite::report(&rows);
    let out = report_path();
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap());
    eprintln!("{}", suite::table(&rows));

    // Hard gates, whatever the baseline says: every brief builds, identically twice.
    let refused: Vec<String> = rows
        .iter()
        .filter_map(|r| r.refused.as_ref().map(|w| format!("{}: {w}", r.name)))
        .collect();
    assert!(refused.is_empty(), "bench briefs refused: {refused:#?}");
    let unstable: Vec<&str> = rows
        .iter()
        .filter(|r| r.get("deterministic") != Some(100.0))
        .map(|r| r.name.as_str())
        .collect();
    assert!(unstable.is_empty(), "rebuild differs: {unstable:?}");

    let path = suite::baseline_path();
    if std::env::var_os("DDC_BENCH_UPDATE").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
        return;
    }
    let base: J = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("bench/baseline.json is committed"),
    )
    .unwrap();
    let worse = suite::regressions(&base, &rows);
    assert!(
        worse.is_empty(),
        "bench regressed (re-pin with DDC_BENCH_UPDATE=1 only for an intended change):\n{}",
        worse.join("\n")
    );
}

/// Moving the whole project changes none of its measures.
#[test]
fn the_measures_do_not_depend_on_where_the_project_stands() {
    let briefs = suite::load_briefs();
    for name in ["house_2_storeys", "section_5", "office_4_storeys", "clinic_preset", "basement_office"] {
        let b = briefs.iter().find(|b| b.name == name).unwrap_or_else(|| panic!("{name}"));
        let mut moved = b.clone();
        moved.spec["origin"] = json!([100.0, 50.0]);
        let (here, there) = (suite::measure(b), suite::measure(&moved));
        assert!(here.refused.is_none() && there.refused.is_none(), "{name}");
        for m in suite::METRICS.iter().filter(|m| m.id != suite::RUNTIME) {
            let (x, y) = (here.get(m.id), there.get(m.id));
            let same = match (x, y) {
                (Some(x), Some(y)) => (x - y).abs() <= 0.05,
                (None, None) => true,
                _ => false,
            };
            assert!(same, "{name}: {} {x:?} here, {y:?} moved", m.id);
        }
    }
}
