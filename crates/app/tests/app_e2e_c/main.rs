// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor end-to-end tests in one binary, so they link once; `tools/ci_shard.py` weighs it as
//! the sum of its member files. Helpers are declared here once, members reach them by `crate::`.

#[path = "../e2e_common/mod.rs"]
mod e2e_common;

mod e2e_annot_survive;
mod e2e_phases;
mod e2e_property;
mod e2e_save_reopen;
mod e2e_showcase;
mod e2e_underlay;
mod e2e_xlsx;
mod e2e_xlsx_fuzz;
mod e2e_xlsx_ifc;
mod ifc_random_roundtrip;
mod plant_lod;
