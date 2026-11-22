// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Browser viewer: the editor's model, geometry and io crates compiled to WebAssembly, with an
//! egui shell (WebGL2). Opens a `.ddc` project or an IFC file, shows it in 3D and as a level plan,
//! picks elements, lists their properties and runs the model checks.

pub mod cam;
pub mod engine;
pub mod plan;

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod gl;
#[cfg(target_arch = "wasm32")]
pub use app::web::*;
