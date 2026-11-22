// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assistant session tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../e2e_ai_common/mod.rs"]
mod e2e_ai_common;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_ai_deliver;
mod e2e_ai_h08_metro;
mod e2e_ai_h15_hospital;
mod e2e_ai_hosted;
mod e2e_ai_preview;
