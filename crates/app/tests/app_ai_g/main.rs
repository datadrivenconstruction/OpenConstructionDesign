// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assistant session tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../e2e_ai_common/mod.rs"]
mod e2e_ai_common;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_ai_bulk;
mod e2e_ai_family;
mod e2e_ai_free;
mod e2e_ai_h03_plant;
mod e2e_ai_materials;
mod e2e_ai_modify;
mod e2e_ai_plant;
mod e2e_ai_steel;
mod e2e_ai_tool_fuzz;
mod plant_run_site;
