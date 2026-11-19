// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roofs and slabs on freeform surfaces in 3D: one part per element (all its layers),
//! picking the element. Rebuilt whenever the element or its host changes.

use std::sync::Arc;

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::{self, Part, SceneProvider, TAG_ROOF};
use crate::editor::Editor;
use ddc_geom::mesh::Mesh;
use ddc_model::ShellKind;
use ddc_render::glam::Vec3;

/// Pick category: `Cat::Ext(CAT)`, the target id is the shell element id.
pub const CAT: u16 = crate::scene_ids::EXT_SURFACE_SHELL;

fn tag(kind: ShellKind) -> u32 {
    match kind {
        ShellKind::Roof => TAG_ROOF,
        ShellKind::Slab => ddc_render::TAG_SLAB,
    }
}

/// All layer meshes as one part, lowered by the active level elevation `z0`.
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

struct SurfaceShells;

impl SceneProvider for SurfaceShells {
    fn key(&self) -> &'static str {
        "shell_elem"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let Ok(rows) = ed.model.shell_elems(Some(level)) else {
            return vec![];
        };
        let z0 = ed.elevation as f32;
        let mut out = vec![];
        for (id, s) in rows {
            // an element whose surface does not tessellate stays out of the scene
            let Ok(m) = ed.model.shell_elem_mesh(id) else {
                continue;
            };
            out.extend(part(id, tag(s.kind), &m.layers, z0));
        }
        out
    }

    fn tables(&self) -> &'static [&'static str] {
        &["shell_elem", "freeform", "level"]
    }

    fn main_table(&self) -> Option<&'static str> {
        Some("shell_elem")
    }
}

/// Wires roofs and slabs on surfaces into the 3D scene.
pub fn install() {
    scene3d::register(Arc::new(SurfaceShells));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_make_one_part_per_element() {
        let a = Mesh { positions: vec![ddc_geom::v3(0.0, 0.0, 3.0); 3], triangles: vec![[0, 1, 2]], face_of: vec![0] };
        let p = part(4, tag(ShellKind::Slab), &[a.clone(), a], 3.0).unwrap();
        assert_eq!(p.target, Target::new(Cat::Ext(CAT), 4));
        assert_eq!((p.pos.len(), p.tris), (6, vec![[0, 1, 2], [3, 4, 5]]));
        assert!(p.pos.iter().all(|v| v.z == 0.0));
        assert_eq!(p.tag, ddc_render::TAG_SLAB);
        assert_eq!(tag(ShellKind::Roof), TAG_ROOF);
        assert!(part(4, TAG_ROOF, &[], 0.0).is_none());
    }
}
