// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor side of the ERP link: `ddc gen` (brief to model) and `ddc publish` (model to revision).
//! Nothing here runs unless a command asks for it; the editor does not depend on it.
//! `erp` + `panel`: the windowed editor's linked session and "Publish to ERP" (E5).

pub mod diff;
pub mod erp;
pub mod foundation;
pub mod gen;
pub mod glb;
pub mod package;
pub mod panel;
pub mod publish;
