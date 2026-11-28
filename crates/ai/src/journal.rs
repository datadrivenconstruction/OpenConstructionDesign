// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The task journal: one record per model step of a turn (role, model, tokens, cost, time,
//! tools called) and the totals, exported as JSON. The chat panel shows it as a step feed.

use crate::profile::StepRole;
use serde::Serialize;
use serde_json::{json, Value as J};

/// Version of the exported JSON; bumped when a field changes meaning.
pub const JOURNAL_VERSION: u32 = 1;
/// Characters of the request kept as the task's title.
pub const TASK_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepRecord {
    /// 1 for the first step of the turn.
    pub n: u32,
    pub role: StepRole,
    pub model: String,
    /// All input of the step, and the part read from the provider's cache.
    pub tokens_in: u64,
    pub tokens_cached: u64,
    pub tokens_out: u64,
    /// At the role's prices; None without prices.
    pub cost: Option<f64>,
    /// What the server says it charged for the step, in its billing unit; None when it reports none.
    pub charged: Option<f64>,
    /// From the request to the end of the reply, retries included.
    pub ms: u64,
    /// Tools the step asked for, in order (catalog names).
    pub tools: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Totals {
    pub steps: usize,
    pub tool_calls: usize,
    pub tokens_in: u64,
    pub tokens_cached: u64,
    pub tokens_out: u64,
    pub cost: Option<f64>,
    pub ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskJournal {
    /// The head of the request that started the turn.
    pub task: String,
    pub steps: Vec<StepRecord>,
    /// How the turn ended: answered, limit:<name>, cancelled, failed; None while it runs.
    pub end: Option<String>,
}

impl TaskJournal {
    pub fn new(task: &str) -> Self {
        TaskJournal {
            task: task.chars().take(TASK_CHARS).collect(),
            ..Self::default()
        }
    }

    pub fn totals(&self) -> Totals {
        let mut t = Totals {
            steps: self.steps.len(),
            ..Totals::default()
        };
        for s in &self.steps {
            t.tool_calls += s.tools.len();
            t.tokens_in += s.tokens_in;
            t.tokens_cached += s.tokens_cached;
            t.tokens_out += s.tokens_out;
            t.ms += s.ms;
            if let Some(c) = s.cost {
                t.cost = Some(t.cost.unwrap_or(0.0) + c);
            }
        }
        t
    }

    /// The export: no request text beyond the task head, no tool arguments or results.
    pub fn to_json(&self) -> J {
        json!({
            "version": JOURNAL_VERSION,
            "task": self.task,
            "end": self.end,
            "totals": self.totals(),
            "steps": self.steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(n: u32, role: StepRole, tools: &[&str], cost: Option<f64>) -> StepRecord {
        StepRecord {
            n,
            role,
            model: "m".to_string(),
            tokens_in: 1000,
            tokens_cached: 600,
            tokens_out: 50,
            cost,
            charged: None,
            ms: 120,
            tools: tools.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn totals_and_export_carry_every_step() {
        let mut j = TaskJournal::new(&"x".repeat(TASK_CHARS + 50));
        assert_eq!(j.task.chars().count(), TASK_CHARS);
        j.steps.push(step(1, StepRole::Planner, &["get_context", "create_geometry"], Some(0.25)));
        j.steps.push(step(2, StepRole::Executor, &[], Some(0.5)));
        j.end = Some("answered".to_string());
        let t = j.totals();
        assert_eq!((t.steps, t.tool_calls, t.tokens_in, t.tokens_cached), (2, 2, 2000, 1200));
        assert_eq!((t.tokens_out, t.ms, t.cost), (100, 240, Some(0.75)));
        let v = j.to_json();
        assert_eq!(v["version"], JOURNAL_VERSION);
        assert_eq!(v["end"], "answered");
        assert_eq!(v["totals"]["tool_calls"], 2);
        assert_eq!(v["steps"][0]["role"], "planner");
        assert_eq!(v["steps"][1]["role"], "executor");
        assert_eq!(v["steps"][0]["tools"][1], "create_geometry");
        // round trip through text: what a file holds
        let back: J = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn without_prices_the_cost_stays_unknown() {
        let mut j = TaskJournal::new("t");
        j.steps.push(step(1, StepRole::Planner, &[], None));
        assert_eq!(j.totals().cost, None);
        assert!(j.to_json()["totals"]["cost"].is_null());
    }
}
