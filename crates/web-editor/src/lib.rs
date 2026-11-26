// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! wasm-bindgen bundle of the editor: `WebEditor` from `ddc_app::web` (built for wasm32 only).

#[cfg(target_arch = "wasm32")]
pub use ddc_app::web::*;
