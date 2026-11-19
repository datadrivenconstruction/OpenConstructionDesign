// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D ramp provider: targets use the same codes as `Sel::Ext`; selection is shared with the plan.

use std::sync::Arc;

use super::pick::{Cat, Target};
use super::scene3d::{self, Part, SceneProvider};
use crate::editor::structx::code_of;
use crate::editor::Editor;

struct RampProvider;

impl SceneProvider for RampProvider {
    fn key(&self) -> &'static str {
        "ramp"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let Some(code) = code_of("ramp") else {
            return vec![];
        };
        let mut out = vec![];
        for id in ed.model.ramps(level).unwrap_or_default() {
            // a ramp that cannot be built is shown and selected as a flat placeholder
            let meshes = ed
                .model
                .ramp_meshes(id)
                .or_else(|_| ed.model.ramp_stub_meshes(id));
            let Ok(meshes) = meshes else { continue };
            let mut p = Part::new(Target::new(Cat::Ext(code), id), ddc_render::TAG_SLAB);
            for m in &meshes {
                p.push(m, ed.elevation);
            }
            if !p.is_empty() {
                out.push(p);
            }
        }
        out
    }

    fn tables(&self) -> &'static [&'static str] {
        &["ramp", "level"]
    }
}

pub fn register() {
    scene3d::register(Arc::new(RampProvider));
}
