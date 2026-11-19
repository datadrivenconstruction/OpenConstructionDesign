// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-ai-eval run [--tier hard|edit|smoke] [--case ID,ID] [--provider replay|live|judge]
//! [--judge-root DIR] [--scores FILE] [--budget-usd N] [--record DIR] [--profile FILE] [--jobs N] [--out DIR] [--ledger FILE]
//! [--replay-root DIR] [--recorded-only] [--gate]`
//!
//! Runs eval cases without a window and appends one line to the ledger. Exit code 1 when a
//! case ran and failed; cases not run (budget) do not fail the run. `--recorded-only` (replay)
//! keeps the cases that have a recording. `--gate` (the nightly live run) compares the line with
//! the last one of the same provider and tier: exit 4 on a regression,
//! single failed cases do not fail it. Exit 3: nothing ran. The provider key is read
//! from DDC_AI_KEY by the provider and is never printed.

use ddc_app::ai::eval::case::{evals_dir, load_dir};
use ddc_app::ai::eval::recorded::run_recorded;
use ddc_app::ai::eval::metrics::{scores_json, append_ledger, gate, last_line, ledger_line, table, RunInfo};
use ddc_app::ai::eval::replay::{self, default_root};
use ddc_app::ai::eval::run::{live_provider, replay_profile, run_all, LiveFactory, Opts, ReplayFactory};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        eprintln!("usage: ddc-ai-eval run [--tier T] [--case ID,ID] [--provider replay|live|judge] [--judge-root DIR] [--budget-usd N] [--record DIR] [--profile FILE] [--jobs N] [--out DIR] [--ledger FILE]");
        std::process::exit(2);
    }
    let mut cases = match load_dir(&evals_dir().join("cases")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let tier = arg(&args, "--tier");
    let only: Vec<String> = arg(&args, "--case").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    cases.retain(|c| tier.as_ref().is_none_or(|t| &c.tier == t) && (only.is_empty() || only.iter().any(|o| c.id == *o || c.id.starts_with(&format!("{o}_")))));
    let live = arg(&args, "--provider").as_deref() == Some("live");
    let judge = arg(&args, "--provider").as_deref() == Some("judge");
    let gate_on = args.iter().any(|a| a == "--gate");
    let replay_root = arg(&args, "--replay-root").map(PathBuf::from).unwrap_or_else(default_root);
    if args.iter().any(|a| a == "--recorded-only") && !live {
        let all = cases.len();
        cases.retain(|c| replay::exists(&replay_root, &c.id));
        println!("{} of {all} cases have a recording", cases.len());
        if cases.is_empty() {
            std::process::exit(3);
        }
    }
    if cases.is_empty() {
        eprintln!("no case matches");
        std::process::exit(2);
    }
    let jobs = arg(&args, "--jobs").and_then(|j| j.parse().ok()).unwrap_or(1usize);
    let budget = arg(&args, "--budget-usd").and_then(|j| j.parse::<f64>().ok());
    let record = arg(&args, "--record").map(PathBuf::from);
    let out = arg(&args, "--out").map(PathBuf::from).or_else(|| Some(std::env::temp_dir().join("ddc_ai_eval_out")));
    let ledger = arg(&args, "--ledger").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("quality/ai_eval.jsonl"));
    let t0 = Instant::now();
    let (opts, factory, model): (Opts, Box<dyn ddc_app::ai::eval::run::ProviderFactory>, String) = if live {
        let Some(file) = arg(&args, "--profile") else {
            eprintln!("--provider live needs --profile FILE");
            std::process::exit(2);
        };
        let (profile, provider) = match live_provider(&PathBuf::from(file)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        };
        let o = Opts {
            out,
            price_in: profile.price_in_per_mtok.unwrap_or(0.0),
            price_out: profile.price_out_per_mtok.unwrap_or(0.0),
            turn_timeout: Duration::from_secs(1600),
            profile: profile.clone(),
        };
        (o, Box::new(LiveFactory { provider, record: record.is_some() }), profile.model)
    } else {
        let o = Opts { out, profile: replay_profile(), price_in: 0.0, price_out: 0.0, turn_timeout: Duration::from_secs(300) };
        (o, Box::new(ReplayFactory { root: replay_root }), "replay".to_string())
    };
    let results: Vec<ddc_app::ai::eval::metrics::CaseMetrics> = if judge {
        let root = arg(&args, "--judge-root").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        cases.iter().map(|c| run_recorded(c, &root)).collect()
    } else {
        run_all(&cases, factory.as_ref(), &opts, jobs, budget, record.as_deref())
    };
    print!("{}", table(&results));
    if let Some(p) = arg(&args, "--scores") {
        if let Err(e) = std::fs::write(&p, serde_json::to_string_pretty(&scores_json(&results)).unwrap_or_default()) {
            eprintln!("scores: {p}: {e}");
            std::process::exit(2);
        }
    }
    let info = RunInfo {
        commit: commit(),
        provider: if live { "live" } else if judge { "judge" } else { "replay" }.to_string(),
        model,
        tier: tier.unwrap_or_else(|| "all".to_string()),
        wall_s: t0.elapsed().as_secs_f64(),
    };
    let line = ledger_line(&info, &results);
    // compared before appending, so the line is never compared with itself
    let regressions = match gate_on.then(|| last_line(&ledger, &info.provider, &info.tier)) {
        Some(Some(prev)) => gate(&prev, &line),
        Some(None) => {
            println!("gate: no earlier {} line for tier {}: this run is the base", info.provider, info.tier);
            vec![]
        }
        None => vec![],
    };
    if let Err(e) = append_ledger(&ledger, &line) {
        eprintln!("ledger: {e}");
        std::process::exit(2);
    }
    let failed = results.iter().filter(|c| c.ran() && !c.pass()).count();
    println!("{} run, {} passed, {} failed, {} not run", results.iter().filter(|c| c.ran()).count(), results.iter().filter(|c| c.pass()).count(), failed, results.iter().filter(|c| !c.ran()).count());
    // a run that started nothing must not look like a pass
    let ran = results.iter().filter(|c| c.ran()).count();
    for r in &regressions {
        println!("GATE: {r}");
    }
    std::process::exit(if failed > 0 && !gate_on {
        1
    } else if ran == 0 {
        3
    } else if !regressions.is_empty() {
        4
    } else {
        0
    });
}
