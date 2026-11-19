// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `--provider judge`: scores a result a live run already left (`<root>/<case id>/model.ddc` and `result.json`)
//! with the case's oracles, without any provider: the recorded failures stay measurable.
//! The run's per-step answers were not kept, only the tool names, so this is not a replay of
//! the conversation; it judges the model the conversation ended with.

use super::case::Case;
use super::facts::Facts;
use super::metrics::{CaseMetrics, Status};
use super::oracle::{evaluate, EvalCx};
use crate::editor::Editor;
use serde_json::Value as J;
use std::path::Path;
use web_time::Instant;

/// What `result.json` says about the run: answer text, tool names, failed tools, tokens, cost.
#[derive(Debug, Default, PartialEq)]
pub struct Run {
    pub answer: String,
    pub tools: usize,
    pub refused: Vec<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub usd: f64,
}

pub fn parse_result(text: &str) -> Result<Run, String> {
    let j: J = serde_json::from_str(text).map_err(|e| format!("result.json: {e}"))?;
    let r = j.get("run").ok_or("result.json has no run")?;
    let name = |x: &J| x.as_str().map(str::to_string).or_else(|| x.get("name").and_then(J::as_str).map(str::to_string));
    Ok(Run {
        answer: r.get("model_text").and_then(J::as_str).unwrap_or("").to_string(),
        tools: r.get("tool_calls").and_then(J::as_array).map_or(0, Vec::len),
        refused: r.get("tool_errors").and_then(J::as_array).map(|a| a.iter().filter_map(name).collect()).unwrap_or_default(),
        tokens_in: r.get("input_tokens").and_then(J::as_u64).unwrap_or(0),
        tokens_out: r.get("output_tokens").and_then(J::as_u64).unwrap_or(0),
        usd: r.get("cost_usd").and_then(J::as_f64).unwrap_or(0.0),
    })
}

pub fn run_recorded(case: &Case, root: &Path) -> CaseMetrics {
    let t0 = Instant::now();
    let dir = root.join(&case.id);
    let (model, result) = (dir.join("model.ddc"), dir.join("result.json"));
    if !model.is_file() || !result.is_file() {
        return CaseMetrics::not_run(&case.id, &format!("no recorded result in {}", dir.display()));
    }
    let mut m = CaseMetrics::new(&case.id);
    let run = match std::fs::read_to_string(&result).map_err(|e| e.to_string()).and_then(|t| parse_result(&t)) {
        Ok(r) => r,
        Err(e) => return CaseMetrics::not_run(&case.id, &e),
    };
    let copy = std::env::temp_dir().join(format!("ddc_eval_rec_{}_{}.ddc", case.id, std::process::id()));
    let mut ed = match Editor::new_in_memory() {
        Ok(e) => e,
        Err(e) => return CaseMetrics::not_run(&case.id, &format!("editor: {e:?}")),
    };
    if let Err(e) = std::fs::copy(&model, &copy).map_err(|e| e.to_string()).and_then(|_| ed.open_project(&copy).map_err(|e| format!("{e:?}"))) {
        return CaseMetrics::not_run(&case.id, &format!("{}: {e}", model.display()));
    }
    let _ = std::fs::remove_file(&copy);
    let facts = match Facts::read(&ed.model) {
        Ok(f) => f,
        Err(e) => {
            m.note = format!("facts: {e}");
            return m;
        }
    };
    let ifc = if case.oracle.ifc.is_some() { ddc_io::ifc::export_ifc(&ed.model).ok() } else { None };
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer: &run.answer, ifc: ifc.as_deref(), errors_before: 0, receipts: &[] };
    m.oracles = evaluate(case, &cx);
    m.steps = run.tools as u32;
    (m.tokens_in, m.tokens_out, m.usd) = (run.tokens_in, run.tokens_out, run.usd);
    for t in run.refused {
        *m.refusals.entry(t).or_default() += 1;
    }
    m.wall_s = t0.elapsed().as_secs_f64();
    m.status = if m.oracles.iter().all(|o| o.pass) { Status::Pass } else { Status::Fail };
    m.note = "recorded result, not a replay".to_string();
    m
}
