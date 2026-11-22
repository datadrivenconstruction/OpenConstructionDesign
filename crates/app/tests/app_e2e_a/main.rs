// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tests that drive the whole `EditorApp` in one binary, kept apart from the rest: a frame sets
//! the process-wide UI language. Source lints share it. `tools/ci_shard.py` weighs it as the sum
//! of its member files.

#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod dialog_probe_coverage;
mod e2e_contracts;
mod e2e_mep_manual;
mod e2e_merge_compare;
mod e2e_registry;
mod e2e_users;
mod num_lint;
