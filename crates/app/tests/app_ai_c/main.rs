// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assistant session tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../e2e_ai_common/mod.rs"]
mod e2e_ai_common;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;
#[path = "../ai_contract/executors.rs"]
mod executors;
#[path = "../ai_contract/fixtures.rs"]
mod fixtures;

mod ai_contract;
mod e2e_ai_anygeo;
mod e2e_ai_block;
mod e2e_ai_datacenter;
mod e2e_ai_geo_g4;
mod e2e_ai_scenarios;
mod e2e_mep_us_gate;
