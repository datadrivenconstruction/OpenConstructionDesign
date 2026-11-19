// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Neighbourhood rebuild: take a few items out of their runs and put them back by the exact
//! search; keep the result only when it costs less. The choices come from a fixed seed, so
//! the rebuild is as deterministic as the search.

use super::search::pack;
use super::{mix, Plan, Problem, EPS};

/// Nodes the exact search may spend on one rebuild.
const REPAIR_NODES: usize = 4000;

pub(crate) fn improve(p: &Problem, mut plan: Plan) -> Plan {
    let placed: usize = plan.runs.iter().map(|r| r.items.len()).sum();
    if placed < 2 {
        return plan;
    }
    let corridor = p.spine_cost(plan.spine.1 - plan.spine.0);
    let mut seed = 0x51ED_2701_u64 ^ p.items.len() as u64;
    for _ in 0..p.rounds {
        if plan.cost - corridor <= EPS || super::late() {
            break;
        }
        let k = (2 + (mix(&mut seed) % 3) as usize).min(placed);
        let mut runs = plan.runs.clone();
        let mut out = vec![];
        for _ in 0..k {
            let full: Vec<usize> = (0..runs.len()).filter(|&r| !runs[r].items.is_empty()).collect();
            if full.is_empty() {
                break;
            }
            let r = full[(mix(&mut seed) % full.len() as u64) as usize];
            let j = (mix(&mut seed) % runs[r].items.len() as u64) as usize;
            out.push(runs[r].items.remove(j));
        }
        let mut budget = REPAIR_NODES;
        if let Some((c, r)) = pack(p, runs, &out, plan.cost - corridor, &mut budget) {
            if corridor + c < plan.cost - EPS {
                plan.runs = r;
                plan.cost = corridor + c;
            }
        }
    }
    plan
}
