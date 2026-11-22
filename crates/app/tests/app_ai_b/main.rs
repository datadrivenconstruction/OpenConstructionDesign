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

mod e2e_ai_data;
mod e2e_ai_drawing_set;
mod e2e_ai_geometry;
mod e2e_ai_h01_tower;
mod e2e_ai_h02_hospital;
mod e2e_ai_h14_tower_platform;
mod e2e_ai_mcp;
