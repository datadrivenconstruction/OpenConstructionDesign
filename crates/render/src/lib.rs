// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! ddc-render: wgpu viewport renderer (scene, chunks with culling and instancing, camera, picking,
//! offscreen). No UI dependencies.

pub mod anchor;
pub mod buflimit;
pub mod camera;
pub mod city;
#[cfg(feature = "gpu")]
pub mod gfx_probe;
pub mod graphics;
#[cfg(feature = "gpu")]
mod chunk_build;
pub mod chunks;
#[cfg(feature = "gpu")]
pub mod headless;
#[cfg(feature = "gpu")]
pub mod hiz;
pub mod perf;
pub mod pick;
pub mod renderer;
pub mod reveal;
pub mod reveal_export;
pub mod scene;
pub mod selset;
#[cfg(feature = "gpu")]
pub mod synth;
pub mod texture;
#[cfg(feature = "gpu")]
mod texture_gpu;
pub mod vpack;

pub use camera::{OrbitCamera, Ray};
pub use chunks::{Inst, Instanced};
pub use glam;
pub use graphics::{
    AdapterFacts, Adaptive, AoMode, Budgets, FrameFx, Health, Limits, PerfLevel, Rung, Tier,
};
pub use perf::{FrameStats, PerfConfig};
pub use pick::Hit;
pub use renderer::{FrameUniforms, Quality};
#[cfg(feature = "gpu")]
pub use renderer::{init_resolve_target, Renderer, Targets};
pub use scene::{
    is_foliage, is_translucent, tag_foliage, tag_rgb, Aabb, Scene, SceneMesh, Vertex, NO_SELECTION, TAG_DOOR,
    TAG_FOLIAGE, TAG_GLASS, TAG_MULLION, TAG_PANEL, TAG_RGB, TAG_SLAB, TAG_TRANSLUCENT, TAG_WINDOW,
};
#[cfg(feature = "gpu")]
pub use wgpu;

// 3D views: navigation, styles, section fill
pub mod caps;
pub mod nav;
pub mod style;
pub use nav::{Move, Pose, Transition};
pub use style::VisualStyle;
