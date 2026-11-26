// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editor side of the ERP link: `ddc gen` (brief to model) and `ddc publish` (model to revision).
//! Nothing here runs unless a command asks for it; the editor does not depend on it.
//! `erp` + `panel`: the windowed editor's linked session and "Publish to ERP" (E5).
//! `gen` and `foundation` build without the `link` feature (the browser build generates too).

#[cfg(feature = "link")]
pub mod diff;
#[cfg(feature = "link")]
pub mod erp;
pub mod foundation;
pub mod gen;
pub mod pick;
#[cfg(feature = "link")]
pub mod glb;
#[cfg(feature = "link")]
pub mod package;
#[cfg(feature = "link")]
pub mod panel;
#[cfg(feature = "link")]
pub mod publish;
pub mod qa;
