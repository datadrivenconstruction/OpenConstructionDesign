// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Prints the generator benchmark table and what got worse than the pinned scoreboard:
//! `cargo run -p ddc-gen --example gen_bench [-- brief_name ...]`.

#[path = "../bench/suite.rs"]
mod suite;

fn main() {
    let only: Vec<String> = std::env::args().skip(1).collect();
    let briefs: Vec<suite::Brief> = suite::load_briefs()
        .into_iter()
        .filter(|b| only.is_empty() || only.contains(&b.name))
        .collect();
    let rows = suite::run_all(&briefs);
    print!("{}", suite::table(&rows));
    let Ok(text) = std::fs::read_to_string(suite::baseline_path()) else {
        println!("\nno bench/baseline.json");
        return;
    };
    let base: serde_json::Value = serde_json::from_str(&text).expect("bench/baseline.json");
    if !only.is_empty() {
        return;
    }
    let worse = suite::regressions(&base, &rows);
    if worse.is_empty() {
        println!("\nno regression against bench/baseline.json");
    } else {
        println!("\nworse than bench/baseline.json:");
        for w in worse {
            println!("  {w}");
        }
    }
}
