// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D providers of structural kinds of schema 26 (footings, slabs, bracing, trusses, beam systems)
//! for the `edit3d::scene3d` registry: one provider per kind, the target is `Cat::Ext(code)` with the same
//! codes as `Sel::Ext` (`editor::structx::code_of`), the tag is `editor::structx::tag_of`.
//!
//! This file is not yet in the module tree. To wire it in:
//! 1. wire with `mod struct3d;` in edit3d/mod.rs and `struct3d::register()` at start-up once g2
//!    is merged (for example from `edit3d::register(a)`);
//! 2. in `pick.rs`, add a branch to `Target::sel`
//!    `Cat::Ext(c) if crate::editor::structx::is_struct(c) => Sel::Ext(c, self.id),`
//!    and to `Target::from_sel` `Sel::Ext(c, id) => Target::new(Cat::Ext(c), id),`
//!    (or call [`sel_of`] / [`target_of`] below);
//! 3. remove `self.struct_meshes(&mut meshes)` from `Editor::scene_report` if `compose`
//!    takes the whole editor scene, otherwise the level's bodies are drawn twice.

use std::sync::Arc;

use super::pick::{Cat, Target};
use super::scene3d::{self, Part, SceneProvider};
use crate::editor::structx::{code_of, kind_of, level_parts, tag_of};
use crate::editor::{Editor, Sel};
use ddc_model::STRUCT_KINDS;

/// Body provider for one kind (`STRUCT_KINDS`).
pub struct StructProvider {
    pub kind: &'static str,
}

impl SceneProvider for StructProvider {
    fn key(&self) -> &'static str {
        self.kind
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let Some(code) = code_of(self.kind) else {
            return vec![];
        };
        let tag = tag_of(self.kind);
        let all = level_parts(ed, level);
        let mut out = vec![];
        for (kind, id, mesh) in all.iter() {
            if *kind != self.kind || mesh.triangles.is_empty() {
                continue;
            }
            let mut p = Part::new(Target::new(Cat::Ext(code), *id), tag);
            p.push(mesh, ed.elevation);
            out.push(p);
        }
        out
    }
}

/// Parts the structure build makes for kinds outside `STRUCT_KINDS` (gridshells, steel
/// connections). With only one provider per `STRUCT_KINDS` entry they were built, counted and
/// never drawn: a gridshell dome showed an empty 3D view.
pub struct BuiltRest;

impl SceneProvider for BuiltRest {
    fn key(&self) -> &'static str {
        "struct_rest"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let all = level_parts(ed, level);
        let mut out = vec![];
        for (kind, id, mesh) in all.iter() {
            if STRUCT_KINDS.contains(kind) || mesh.triangles.is_empty() {
                continue;
            }
            let mut p = Part::new(Target::new(Cat::Ext(crate::scene_ids::EXT_STRUCT_REST), *id), tag_of(kind));
            p.push(mesh, ed.elevation);
            out.push(p);
        }
        out
    }
}

/// 3D target to editor selection (structural codes only).
pub fn sel_of(t: Target) -> Option<Sel> {
    match t.cat {
        Cat::Ext(c) if kind_of(c).is_some() => Some(Sel::Ext(c, t.id)),
        _ => None,
    }
}

/// Editor selection to 3D target (structural codes only).
pub fn target_of(s: Sel) -> Option<Target> {
    match s {
        Sel::Ext(c, id) if kind_of(c).is_some() => Some(Target::new(Cat::Ext(c), id)),
        _ => None,
    }
}

/// Register providers for all kinds (calling again replaces the same ones by key).
pub fn register() {
    for kind in STRUCT_KINDS {
        scene3d::register(Arc::new(StructProvider { kind }));
    }
    scene3d::register(Arc::new(BuiltRest));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{FootingSpec, FootingStep};

    #[test]
    fn footing_part_targets_round_trip() {
        let mut e = Editor::new_in_memory().unwrap();
        let id = e
            .model
            .add_footing(&FootingSpec {
                level: e.level,
                column: None,
                at: (1.0, 1.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep {
                    w: 1.2,
                    l: 1.2,
                    h: 0.5,
                }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        e.reload().unwrap();
        let parts = StructProvider { kind: "footing" }.parts(&e, e.level);
        assert_eq!(parts.len(), 1);
        let t = parts[0].target;
        assert_eq!(t, Target::new(Cat::Ext(code_of("footing").unwrap()), id));
        let s = sel_of(t).unwrap();
        assert_eq!(target_of(s), Some(t));
        assert!(StructProvider { kind: "truss" }
            .parts(&e, e.level)
            .is_empty());
        assert!(BuiltRest.parts(&e, e.level).is_empty(), "a footing is drawn by its own provider");
    }

    // Seen live: a ribbed dome as one gridshell had 176 bars and an empty 3D view
    #[test]
    fn a_gridshell_is_drawn_in_3d() {
        use ddc_geom::host::{Axes, Host};
        use ddc_model::{GridKind, GridPattern, GridShellSpec, GridSupports};
        let mut e = Editor::new_in_memory().unwrap();
        let t = e
            .model
            .add_member_type("CHS 88.9x5", &ddc_geom::member::Shape::Pipe { d: 0.0889, t: 0.005 }, "S355")
            .unwrap();
        let host = Host::Sphere { ax: Axes::world(), r: 15.0, lon0: 0.0, lon1: std::f64::consts::TAU, lat0: 0.1, lat1: 1.1 };
        let id = e
            .model
            .add_gridshell(&GridShellSpec {
                level: e.level,
                kind: GridKind::Gridshell,
                host,
                pattern: GridPattern::QuadBraced,
                nu: 16,
                nv: 4,
                supports: GridSupports::Base,
                member_type: t,
                edge_type: None,
                node_d: 0.0,
            })
            .unwrap();
        e.reload().unwrap();
        let parts = BuiltRest.parts(&e, e.level);
        assert!(!parts.is_empty(), "the gridshell has no 3D part");
        assert!(parts.iter().all(|p| p.target == Target::new(Cat::Ext(crate::scene_ids::EXT_STRUCT_REST), id)));
        assert!(STRUCT_KINDS.iter().all(|k| StructProvider { kind: *k }.parts(&e, e.level).is_empty()));
        register();
        let drawn = scene3d::parts(&e, e.level, true, false);
        assert!(drawn.iter().any(|(p, _)| p.target.cat == Cat::Ext(crate::scene_ids::EXT_STRUCT_REST)));
    }
}
