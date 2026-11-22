// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assistant session tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../e2e_ai_common/mod.rs"]
mod e2e_ai_common;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_ai_h04_stadium;
mod e2e_ai_h05_tower;
mod e2e_ai_h10;
mod e2e_ai_norms;
mod e2e_ai_readset;
mod e2e_ai_rules;
mod e2e_param_lock;
mod e2e_roof_walls;
