// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assistant session tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../app2_fx/mod.rs"]
mod app2_fx;
#[path = "../e2e_ai_common/mod.rs"]
mod e2e_ai_common;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_ai_anylang;
mod e2e_ai_create;
mod e2e_ai_create_more;
mod e2e_ai_h12_hospital;
mod e2e_ai_layers;
mod e2e_ai_levels;
mod e2e_ai_station;
mod e2e_ai_trees;
