// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof edges in the 3D scene registry (`edit3d::scene3d`): profiles, skylights (frame and glass),
//! soffits and wedges, as bodies with pick targets `Cat::Ext(CAT)` (`sub` is the element kind), deletion
//! of the selection as one undo step, and the properties window.
//! Integration: declare `pub mod roof_edge_scene;` in app/lib.rs and call `roof_edge_scene::install(app)` from `roof_edge_tools::register`.

use std::sync::Arc;

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d::{self, Part, SceneProvider};
use crate::editor::Editor;
use crate::roof_edge_tools::{self as tools, Elem};
use crate::shell::EditorApp;

/// Category number of the edge strip in `Cat::Ext`.
pub const CAT: u16 = crate::scene_ids::EXT_ROOF_EDGE;

pub fn target(e: Elem) -> Target {
    Target {
        cat: Cat::Ext(CAT),
        id: e.id(),
        sub: e.index(),
    }
}

pub fn elem(t: Target) -> Option<Elem> {
    match t.cat {
        Cat::Ext(CAT) => Elem::from_index(t.sub, t.id),
        _ => None,
    }
}

struct Provider;

impl SceneProvider for Provider {
    fn key(&self) -> &'static str {
        "roof_edge"
    }

    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        let bodies = ed.model.roof_edge_bodies(Some(level)).unwrap_or_default();
        bodies
            .iter()
            .filter_map(|(kind, id, mesh, _)| {
                let e = Elem::from_kind(kind, *id)?;
                let mut p = Part::new(target(e), tools::tag_of(kind));
                p.push(mesh, ed.elevation);
                Some(p)
            })
            .collect()
    }
}

/// Attach the provider (the old `scene_meshes` path is switched off) and the properties command.
pub fn install(app: &mut EditorApp) {
    scene3d::register(Arc::new(Provider));
    app.register_command("arch.roof_edges", props_or_list);
}

/// Edge elements selected in 3D.
pub fn selected(a: &EditorApp) -> Vec<Elem> {
    a.view.e3.sel.iter().filter_map(|t| elem(*t)).collect()
}

/// One selected element shows its properties; otherwise the level's element list.
fn props_or_list(a: &mut EditorApp) {
    match selected(a).as_slice() {
        [e] => crate::roof_edge_ui::open_props(&mut a.ed, *e),
        _ => crate::roof_edge_ui::open_list(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roof_edge_tools::{exec, tests::with_roof, Cmd, Params};

    #[test]
    fn provider_parts_pick_back_to_elements() {
        let (mut ed, _, roof) = with_roof();
        assert!(exec(&mut ed, Cmd::Soffit, &Params::default()));
        let so = ed.model.soffits(roof).unwrap()[0];
        let parts = Provider.parts(&ed, ed.level);
        assert!(!parts.is_empty());
        assert!(parts.iter().all(|p| !p.is_empty()));
        assert!(parts
            .iter()
            .any(|p| elem(p.target) == Some(Elem::Soffit(so))));
        for e in [
            Elem::Sweep(1),
            Elem::Skylight(2),
            Elem::Soffit(3),
            Elem::Taper(4),
        ] {
            assert_eq!(elem(target(e)), Some(e));
        }
        assert_eq!(elem(Target::new(Cat::Ext(CAT + 1), 3)), None);
    }
}
