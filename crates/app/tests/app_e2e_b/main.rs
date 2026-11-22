// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor end-to-end tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../app2_fx/mod.rs"]
mod app2_fx;
#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_clash;
mod e2e_ifc_patch_corpus;
mod e2e_ifc_roundtrip;
mod e2e_ix_parity;
mod e2e_mass_floors;
mod e2e_new_project;
mod e2e_random_ops;
mod e2e_xlsx_undo;
mod family_fuzz;
mod headless_file;
