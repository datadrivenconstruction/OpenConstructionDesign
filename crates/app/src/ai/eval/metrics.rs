// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What one case measured, and the one line a run adds to the experiment ledger.

use super::oracle::OracleResult;
use serde_json::{json, Value as J};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The case ran and every oracle passed.
    Pass,
    /// The case ran and an oracle failed, or the run ended badly.
    Fail,
    /// Not started: the budget was spent, or the case could not be set up.
    NotRun,
    /// The run asked something the recording does not answer.
    Drift,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::NotRun => "not_run",
            Status::Drift => "replay_drift",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaseMetrics {
    pub id: String,
    pub status: Status,
    pub oracles: Vec<OracleResult>,
    /// Tool calls the model made.
    pub steps: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub usd: f64,
    /// Failed tool calls by tool name.
    pub refusals: BTreeMap<String, u32>,
    pub wall_s: f64,
    /// Journal rows the run added: what one undo per row would take back.
    pub undo_steps: i64,
    /// Why the case did not run or ended badly.
    pub note: String,
}

impl CaseMetrics {
    pub fn new(id: &str) -> CaseMetrics {
        CaseMetrics {
            id: id.to_string(),
            status: Status::Fail,
            oracles: vec![],
            steps: 0,
            tokens_in: 0,
            tokens_out: 0,
            usd: 0.0,
            refusals: BTreeMap::new(),
            wall_s: 0.0,
            undo_steps: 0,
            note: String::new(),
        }
    }

    pub fn not_run(id: &str, why: &str) -> CaseMetrics {
        CaseMetrics { status: Status::NotRun, note: why.to_string(), ..CaseMetrics::new(id) }
    }

    pub fn ran(&self) -> bool {
        self.status != Status::NotRun
    }

    pub fn pass(&self) -> bool {
        self.status == Status::Pass
    }

    pub fn failed_oracles(&self) -> Vec<String> {
        self.oracles.iter().filter(|o| !o.pass).map(|o| o.name.clone()).collect()
    }
}

/// What a whole run was made of.
pub struct RunInfo {
    pub commit: String,
    pub provider: String,
    pub model: String,
    pub tier: String,
    pub wall_s: f64,
}

fn median(mut v: Vec<u32>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_unstable();
    v[(v.len() - 1) / 2] as f64
}

/// `{ts, commit, provider, model, tier, cases, pass_rate, oracle_rate, steps_p50, tokens, usd,
/// refusals, wall_s, per_case:[{id, status, pass, failed_oracles}]}`. Rates are over the cases
/// that ran; a case that did not run is listed and counted in `not_run`, never as a failure.
pub fn ledger_line(info: &RunInfo, cases: &[CaseMetrics]) -> J {
    let ran: Vec<&CaseMetrics> = cases.iter().filter(|c| c.ran()).collect();
    let passed = ran.iter().filter(|c| c.pass()).count();
    let (op, ot) = ran.iter().fold((0usize, 0usize), |a, c| {
        (a.0 + c.oracles.iter().filter(|o| o.pass).count(), a.1 + c.oracles.len())
    });
    let rate = |a: usize, b: usize| if b == 0 { 0.0 } else { a as f64 / b as f64 };
    let mut refusals: BTreeMap<&str, u32> = BTreeMap::new();
    for c in &ran {
        for (k, n) in &c.refusals {
            *refusals.entry(k.as_str()).or_default() += n;
        }
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
        "ts": ts,
        "commit": info.commit,
        "provider": info.provider,
        "model": info.model,
        "tier": info.tier,
        "cases": ran.len(),
        "not_run": cases.len() - ran.len(),
        "pass_rate": rate(passed, ran.len()),
        "oracle_rate": rate(op, ot),
        "steps_p50": median(ran.iter().map(|c| c.steps).collect()),
        "tokens": {"in": ran.iter().map(|c| c.tokens_in).sum::<u64>(), "out": ran.iter().map(|c| c.tokens_out).sum::<u64>()},
        "usd": ran.iter().map(|c| c.usd).sum::<f64>(),
        "refusals": refusals,
        "wall_s": info.wall_s,
        "per_case": cases.iter().map(|c| json!({
            "id": c.id, "status": c.status.as_str(), "pass": c.pass(), "failed_oracles": c.failed_oracles(),
        })).collect::<Vec<_>>(),
    })
}

/// Every oracle of every case, for a before/after diff: `{id: {status, passed, total, oracles:
/// [{name, pass, expected, measured}]}}`.
pub fn scores_json(cases: &[CaseMetrics]) -> J {
    let mut m = serde_json::Map::new();
    for c in cases {
        m.insert(
            c.id.clone(),
            json!({
                "status": c.status.as_str(),
                "passed": c.oracles.iter().filter(|o| o.pass).count(),
                "total": c.oracles.len(),
                "note": c.note,
                "oracles": c.oracles.iter().map(|o| json!({"name": o.name, "pass": o.pass, "expected": o.expected, "measured": o.measured})).collect::<Vec<_>>(),
            }),
        );
    }
    J::Object(m)
}

/// The nightly quality gate: how far a live run may fall behind the last one.
pub const GATE_PASS_DROP: f64 = 0.10;
pub const GATE_USD_RISE: f64 = 0.25;

/// The last ledger line of the same provider and tier: what a new run is compared with.
pub fn last_line(path: &Path, provider: &str, tier: &str) -> Option<J> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<J>(l).ok())
        .find(|j| j["provider"] == provider && j["tier"] == tier)
}

fn usd_per_case(j: &J) -> Option<f64> {
    let n = j["cases"].as_f64().filter(|n| *n > 0.0)?;
    Some(j["usd"].as_f64()? / n)
}

/// Regressions of `cur` against `prev`, each as one line a reviewer reads; empty when it holds.
/// A regression is a reason to review the change, never to move the base.
pub fn gate(prev: &J, cur: &J) -> Vec<String> {
    let mut out = vec![];
    if let (Some(a), Some(b)) = (prev["pass_rate"].as_f64(), cur["pass_rate"].as_f64()) {
        if a - b > GATE_PASS_DROP + 1e-9 {
            out.push(format!("pass_rate {:.0} % -> {:.0} % (more than {:.0} points down)", a * 100.0, b * 100.0, GATE_PASS_DROP * 100.0));
        }
    }
    if let (Some(a), Some(b)) = (usd_per_case(prev), usd_per_case(cur)) {
        if a > 0.0 && b > a * (1.0 + GATE_USD_RISE) + 1e-9 {
            out.push(format!("usd per case {a:.3} -> {b:.3} (more than {:.0} % up)", GATE_USD_RISE * 100.0));
        }
    }
    out
}

/// One JSON line appended to the ledger; the folder is created.
pub fn append_ledger(path: &Path, line: &J) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
}

/// The table printed after a run: one row per case, failed oracles under it.
pub fn table(cases: &[CaseMetrics]) -> String {
    let mut s = String::new();
    for c in cases {
        let (p, n) = (c.oracles.iter().filter(|o| o.pass).count(), c.oracles.len());
        s += &format!(
            "{:<34} {:<12} oracles {p}/{n}  steps {}  tokens {}/{}  ${:.3}  {:.1}s\n",
            c.id, c.status.as_str(), c.steps, c.tokens_in, c.tokens_out, c.usd, c.wall_s
        );
        if !c.note.is_empty() {
            s += &format!("    {}\n", c.note);
        }
        for o in c.oracles.iter().filter(|o| !o.pass) {
            s += &format!("    FAIL {}: expected {}, measured {}\n", o.name, o.expected, o.measured);
        }
    }
    s
}
