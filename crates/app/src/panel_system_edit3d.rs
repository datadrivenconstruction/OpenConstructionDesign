// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curtain systems on freeform surfaces in 3D: per system one glass part (panel prisms) and
//! one mullion part, both picking the system. The parts follow the host: they are rebuilt
//! from the host surface whenever the system or its host changes.

use std::sync::Arc;

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::{self, Part, SceneProvider};
use crate::editor::Editor;
use ddc_geom::mesh::Mesh;
use ddc_render::glam::Vec3;

/// Pick category: `Cat::Ext(CAT)`, the target id is the panel system id.
pub const CAT: u16 = crate::scene_ids::EXT_PANEL_SYSTEM;

/// All meshes of `ms` as one part, lowered by the active level elevation `z0`.
fn part(id: i64, tag: u32, ms: &[Mesh], z0: f32) -> Option<Part> {
    if ms.is_empty() {
        return None;
    }
    let mut q = Part::new(Target::new(Cat::Ext(CAT), id), tag);
    for m in ms {
        let base = q.pos.len() as u32;
        q.pos.extend(m.positions.iter().map(|p| Vec3::new(p.x as f32, p.y as f32, p.z as f32 - z0)));
        q.tris.extend(m.triangles.iter().map(|t| [t[0] + base, t[1] + base, t[2] + base]));
    }
    Some(q)
}

struct PanelSystems;

impl SceneProvider for PanelSystems {
    fn key(&self) -> &'static str {
        "panel_system"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let Ok(rows) = ed.model.panel_systems(Some(level)) else {
            return vec![];
        };
        let z0 = ed.elevation as f32;
        let mut out = vec![];
        for (id, _) in rows {
            // a system its host can no longer carry stays out of the scene, the model keeps it
            let Ok(g) = ed.model.panel_system_geom(id) else {
                continue;
            };
            out.extend(part(id, ddc_render::TAG_GLASS, &g.glass, z0));
            out.extend(part(id, ddc_render::TAG_MULLION, &g.frames, z0));
        }
        out
    }

    fn tables(&self) -> &'static [&'static str] {
        &["panel_system", "freeform", "level"]
    }

    fn main_table(&self) -> Option<&'static str> {
        Some("panel_system")
    }
}

/// Wires curtain systems into the 3D scene.
pub fn install() {
    scene3d::register(Arc::new(PanelSystems));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_and_mullion_parts_share_the_target() {
        let a = Mesh { positions: vec![ddc_geom::v3(0.0, 0.0, 3.0); 3], triangles: vec![[0, 1, 2]], face_of: vec![0] };
        let p = part(7, ddc_render::TAG_GLASS, &[a.clone(), a], 3.0).unwrap();
        assert_eq!(p.target, Target::new(Cat::Ext(CAT), 7));
        assert_eq!((p.pos.len(), p.tris), (6, vec![[0, 1, 2], [3, 4, 5]]));
        assert!(p.pos.iter().all(|v| v.z == 0.0));
        assert!(part(7, ddc_render::TAG_MULLION, &[], 0.0).is_none());
    }
}
