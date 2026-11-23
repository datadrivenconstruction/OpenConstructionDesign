// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The write, check, fix loop of `family_draft`: the model gets the report of the pipeline (codes,
//! lines, hints) and only an excerpt of the lines around each finding, never the whole text again.
//! The loop is bounded by `RepairPolicy`; what is not fixed is shown to the person with what to
//! clarify. Nothing here runs the text: it is data checked by `component::validate`.

use crate::convo::estimate_text_tokens;
use crate::family_draft::{draft, Drafted, Refusal};
use crate::refusal;
use ddc_model::refusal::RefusalCode;
use std::collections::HashMap;
use web_time::{Duration, Instant};
use serde_json::{json, Value as J};

/// The family DSL reference for the model: the docstrings of the parser, verbatim.
pub const DSL_REFERENCE: &str = include_str!("../data/family_dsl.md");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RepairPolicy {
    pub max_rounds: u32,
    /// Estimated tokens of the texts and replies of the loop.
    pub max_tokens: u64,
    pub stop_on_same_findings: bool,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        RepairPolicy {
            max_rounds: 3,
            max_tokens: 40_000,
            stop_on_same_findings: true,
        }
    }
}

/// Lines of context around a finding.
const AROUND: usize = 3;
const MAX_FINDINGS_SHOWN: usize = crate::limits::REPAIR_SHOWN.us();

#[derive(Clone, Debug, Default)]
pub struct RepairState {
    pub policy: RepairPolicy,
    pub rounds: u32,
    pub tokens: u64,
    last: Option<Vec<String>>,
}

/// What the host tells the model after a refused draft.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Fix and try again: the report with excerpts.
    Fix(J),
    /// Stop and ask the person: why, the report and what to clarify.
    Stop(J),
}

fn clarify(code: &str) -> &'static str {
    match code {
        "sketch_conflict" => "Which of the marked dimensions is the one that must hold?",
        "unknown_ref" => "Which parameter or plane did you mean by the unknown name?",
        "parse_error" => "Which statement was meant on the marked line?",
        "formula_cycle" => "Which of the looping parameters is the independent one?",
        "type_fails" | "flex_fails" => "Which values must the catalog types take?",
        "zero_volume" | "nonmanifold" => "What are the real sizes of the marked body?",
        "too_large" => "What is the real size, and in which units?",
        "budget" => "Which parts can be left out or made a separate family?",
        _ => "What should the family do that the definition does not yet?",
    }
}

/// The error findings of a report as stable signatures (code and lines).
fn signature(report: &J) -> Vec<String> {
    let mut v: Vec<String> = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["severity"] == "error")
        .map(|f| format!("{}@{}", f["code"].as_str().unwrap_or(""), f["lines"]))
        .collect();
    v.sort();
    v
}

fn excerpts(text: &str, report: &J) -> Vec<J> {
    let lines: Vec<&str> = text.lines().collect();
    report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["severity"] == "error")
        .take(MAX_FINDINGS_SHOWN)
        .map(|f| {
            let at: Vec<usize> = f["lines"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_u64().map(|l| l as usize))
                .collect();
            let mut shown: Vec<usize> = vec![];
            for l in at.iter().take(4) {
                for k in l.saturating_sub(AROUND).max(1)..=(l + AROUND).min(lines.len()) {
                    if !shown.contains(&k) {
                        shown.push(k);
                    }
                }
            }
            shown.sort_unstable();
            let rows: Vec<J> = shown
                .iter()
                .map(|&k| json!({ "line": k, "text": lines[k - 1] }))
                .collect();
            json!({ "code": f["code"], "lines": f["lines"], "msg": f["msg"], "hint": f["hint"], "excerpt": rows })
        })
        .collect()
}

impl RepairState {
    pub fn new(policy: RepairPolicy) -> Self {
        RepairState {
            policy,
            ..Self::default()
        }
    }

    /// A new request of the person, or a draft that passed.
    pub fn reset(&mut self) {
        self.rounds = 0;
        self.tokens = 0;
        self.last = None;
    }

    /// A draft was refused with `report`: fix it, or stop.
    pub fn refused(&mut self, text: &str, report: &J) -> Step {
        self.rounds += 1;
        let sig = signature(report);
        let same = self.last.as_ref() == Some(&sig);
        self.last = Some(sig);
        let body = excerpts(text, report);
        self.tokens += estimate_text_tokens(text) + estimate_text_tokens(&J::Array(body.clone()).to_string());
        let progress = json!({ "round": self.rounds, "of": self.policy.max_rounds, "tokens": self.tokens });
        let why = if self.policy.stop_on_same_findings && same {
            Some("same_findings")
        } else if self.rounds >= self.policy.max_rounds {
            Some("rounds")
        } else if self.tokens > self.policy.max_tokens {
            Some("token_budget")
        } else {
            None
        };
        match why {
            None => Step::Fix(json!({
                "repair": progress,
                "findings": body,
                "note": "Fix these findings and call family_draft again with the full corrected text.",
            })),
            Some(why) => {
                let mut asks: Vec<&str> = body
                    .iter()
                    .filter_map(|f| f["code"].as_str())
                    .map(clarify)
                    .collect();
                asks.dedup();
                Step::Stop(json!({
                    "repair_stopped": why,
                    "repair": progress,
                    "findings": body,
                    "ask_the_person": asks,
                    "note": "Stop. Tell the person what could not be fixed and ask these questions; do not retry.",
                }))
            }
        }
    }

    /// `family_draft` with the loop: the failure text is the wire text of the host.
    pub fn draft(
        &mut self,
        args: &J,
        project: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Drafted, String> {
        match draft(args, project) {
            Ok(d) => {
                self.reset();
                Ok(d)
            }
            Err(Refusal::DraftInvalid(report)) => {
                let text = args["text"].as_str().unwrap_or_default();
                Err(match self.refused(text, &report) {
                    Step::Fix(m) => refusal::wire(&refusal::of(RefusalCode::DraftInvalid).detail(m.to_string())),
                    Step::Stop(m) => refusal::wire(&refusal::of(RefusalCode::RepairStopped).detail(m.to_string())),
                })
            }
            Err(other) => Err(other.wire()),
        }
    }
}

/// Repair state per actor: an agent over MCP and the in-app assistant do not share rounds, and a
/// state idle for `IDLE` starts over, so an old refusal streak never blocks a new request.
#[derive(Debug, Default)]
pub struct RepairBook {
    by: HashMap<String, (RepairState, Instant)>,
}

pub const IDLE: Duration = Duration::from_secs(600);

impl RepairBook {
    pub fn state(&mut self, actor: &str, now: Instant) -> &mut RepairState {
        let e = self
            .by
            .entry(actor.to_string())
            .or_insert_with(|| (RepairState::default(), now));
        if now.saturating_duration_since(e.1) > IDLE {
            e.0.reset();
        }
        e.1 = now;
        &mut e.0
    }

    pub fn reset_all(&mut self) {
        self.by.clear();
    }
}

#[cfg(test)]
mod book_tests {
    use super::*;

    #[test]
    fn actors_do_not_share_and_idle_resets() {
        let t0 = Instant::now();
        let mut b = RepairBook::default();
        let rep = json!({ "findings": [{ "code": "unknown_ref", "severity": "error", "lines": [2] }] });
        b.state("agent_a", t0).refused("x
y
", &rep);
        assert_eq!(b.state("agent_a", t0).rounds, 1);
        assert_eq!(b.state("agent_b", t0).rounds, 0, "another actor starts fresh");
        let later = t0 + IDLE + Duration::from_secs(1);
        assert_eq!(b.state("agent_a", later).rounds, 0, "an idle streak does not block a new request");
        b.state("agent_a", later).refused("x
y
", &rep);
        b.reset_all();
        assert_eq!(b.state("agent_a", later).rounds, 0);
    }
}
