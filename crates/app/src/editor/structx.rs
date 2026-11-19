// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural kinds of schema 26 in the editor: selection `Sel::Ext(code, id)` (code = `STRUCT_BASE` + the
//! kind index in `STRUCT_KINDS`), plan outlines (click, box and Tab picking), deletion as one undo
//! step, level 3D (one mesh per tag). Helpers live here too, the 3D adapter (`edit3d/struct3d.rs`).

use super::levels::OTHER_LEVEL;
use crate::scene_ids::EXT_TOPO;
use super::{Editor, Sel};
use crate::scene_ids::{EXT_ANALYTICAL, EXT_REBAR};
use crate::selection::inside_poly;
use crate::snap::closest_on_segment;
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, V2};
use ddc_model::{Error, Model, STRUCT_KINDS};
use ddc_render::{SceneMesh, TAG_MULLION, TAG_SLAB};
use std::collections::HashMap;
use std::sync::Arc;

/// First `Sel::Ext` / `Cat::Ext` category code of the structural module.
pub const STRUCT_BASE: u16 = crate::scene_ids::EXT_STRUCT_BASE;
/// Selection filter category key.
pub const CATEGORY: &str = "structure";
/// Elements picked as structural but with their own filter category.
pub const CORE_BASE: u16 = crate::scene_ids::EXT_CORE_BASE;
pub const CORE_KINDS: [&str; 9] = ["ramp", "room_sep", "zone", "roof", "stair", "railing", "shaft", "grid", "ref_plane"];
/// A roof outline is picked by its edges only: its fill would swallow every click of the building.
pub const ROOF_CODE: u16 = CORE_BASE + 3;
/// Stair, railing, grid, reference plane (`elem::datum`): their positions in `CORE_KINDS`.
pub const DATUM_KINDS: [&str; 4] = ["stair", "railing", "grid", "ref_plane"];
pub const STAIR_CODE: u16 = CORE_BASE + 4;
pub const RAILING_CODE: u16 = CORE_BASE + 5;
pub const GRID_CODE: u16 = CORE_BASE + 7;
pub const REF_PLANE_CODE: u16 = CORE_BASE + 8;

pub fn code_of(kind: &str) -> Option<u16> {
    STRUCT_KINDS
        .iter()
        .position(|k| *k == kind)
        .map(|i| STRUCT_BASE + i as u16)
        .or_else(|| {
            CORE_KINDS
                .iter()
                .position(|k| *k == kind)
                .map(|i| CORE_BASE + i as u16)
        })
}

pub fn kind_of(code: u16) -> Option<&'static str> {
    if let Some(i) = code.checked_sub(CORE_BASE) {
        return CORE_KINDS.get(i as usize).copied();
    }
    let i = code.checked_sub(STRUCT_BASE)? as usize;
    STRUCT_KINDS.get(i).copied()
}

/// Selection filter category key for a `Sel::Ext` code.
pub fn category_of(code: u16) -> &'static str {
    if code == crate::scene_ids::EXT_FITTING {
        return "mep";
    }
    match kind_of(code) {
        Some(k) if CORE_KINDS.contains(&k) || DATUM_KINDS.contains(&k) => k,
        _ => CATEGORY,
    }
}

/// Picked by its edges and lines only: its fill (a roof) or its host (rebar) would swallow clicks.
pub fn edge_only(code: u16) -> bool {
    code == ROOF_CODE || crate::elem::is_hosted_ext(code) || crate::elem::datum::edge_only(code)
}

pub fn is_struct(code: u16) -> bool {
    kind_of(code).is_some()
}

pub fn sel(kind: &str, id: i64) -> Option<Sel> {
    code_of(kind).map(|c| Sel::Ext(c, id))
}

/// (kind, id) of the selected structural element.
pub fn of_sel(s: Sel) -> Option<(&'static str, i64)> {
    match s {
        Sel::Ext(c, id) => kind_of(c).map(|k| (k, id)),
        _ => None,
    }
}

/// Concrete is tagged like slabs, steel like mullions.
pub fn tag_of(kind: &str) -> u32 {
    if matches!(kind, "footing" | "strip_footing" | "raft") {
        TAG_SLAB
    } else {
        TAG_MULLION
    }
}

/// Level element outlines in plan (world, m); re-read in `reload`.
#[derive(Clone, Debug, Default)]
pub struct StructFp {
    /// (code, id, rings).
    pub items: Vec<(u16, i64, Vec<Vec<V2>>)>,
    index: HashMap<(u16, i64), usize>,
}

impl StructFp {
    pub fn load(m: &Model, level: i64) -> StructFp {
        let mut fp = StructFp::default();
        let core = crate::tools::core_footprints(m, level);
        let own = m.struct_footprints(level).unwrap_or_default();
        for (kind, id, rings) in own.into_iter().chain(core) {
            let Some(c) = code_of(kind) else { continue };
            let rings: Vec<Vec<V2>> = rings
                .into_iter()
                .map(|r| r.into_iter().map(|(x, y)| v2(x, y)).collect())
                .collect();
            fp.index.insert((c, id), fp.items.len());
            fp.items.push((c, id, rings));
        }
        for (key, rings) in crate::elem::fitting::footprints(m, level) {
            fp.index
                .insert((crate::scene_ids::EXT_FITTING, key), fp.items.len());
            fp.items.push((crate::scene_ids::EXT_FITTING, key, rings));
        }
        let hosted = [
            (EXT_REBAR, crate::rebar_scene::pick_sets(m, level)),
            (EXT_ANALYTICAL, crate::rebar_scene::pick_members(m, level)),
        ];
        for (c, items) in hosted {
            for (id, rings) in items {
                fp.index.insert((c, id), fp.items.len());
                fp.items.push((c, id, rings));
            }
        }
        // terrains belong to the site: one outline on every level
        for (id, ring) in crate::elem::topo_fam::outlines(m) {
            fp.index.insert((EXT_TOPO, id), fp.items.len());
            fp.items.push((EXT_TOPO, id, vec![ring]));
        }
        fp
    }

    pub fn rings(&self, code: u16, id: i64) -> &[Vec<V2>] {
        self.index
            .get(&(code, id))
            .map_or(&[], |&i| self.items[i].2.as_slice())
    }

    pub fn contains(&self, code: u16, id: i64) -> bool {
        self.index.contains_key(&(code, id))
    }

    pub fn sels(&self) -> impl Iterator<Item = Sel> + '_ {
        self.items.iter().map(|(c, id, _)| Sel::Ext(*c, *id))
    }
}

/// Distance to a ring; an open polyline (bars, axes) has no closing segment.
fn ring_dist(p: V2, o: &[V2], open: bool) -> f64 {
    let n = o.len();
    (0..if open { n - 1 } else { n })
        .map(|i| (closest_on_segment(p, o[i], o[(i + 1) % n]).0 - p).len())
        .fold(f64::MAX, f64::min)
}

fn ring_area(o: &[V2]) -> f64 {
    let n = o.len();
    (0..n)
        .map(|i| o[i].cross(o[(i + 1) % n]))
        .sum::<f64>()
        .abs()
        / 2.0
}

/// Delete elements as one record: n model operations; on error the ones done are rolled back.
pub fn delete_all(m: &mut Model, items: &[(&'static str, i64)]) -> Result<usize, Error> {
    for (k, &(kind, id)) in items.iter().enumerate() {
        let r = match kind {
            "ramp" => m.delete_ramp(id),
            "room_sep" => m.delete_room_sep(id),
            "zone" => m.delete_zone(id),
            "roof" => m.delete_roof(id),
            "stair" => m.delete_stair(id),
            "railing" => m.delete_railing(id),
            "shaft" => m.delete_shaft(id),
            "grid" => m.delete_grid(id),
            "ref_plane" => m.delete_ref_plane(id),
            k => m.delete_struct(k, id),
        };
        if let Err(e) = r {
            for _ in 0..k {
                m.undo()?;
            }
            return Err(e);
        }
    }
    Ok(items.len())
}

type Parts = Arc<Vec<(&'static str, i64, Mesh)>>;
type PartsKey = (usize, u64, i64, u64);

thread_local! {
    static PARTS: std::cell::RefCell<Option<(PartsKey, Parts)>> = const { std::cell::RefCell::new(None) };
}

/// Level bodies for 3D providers (one per kind): one build per (model, revision, level).
pub fn level_parts(ed: &Editor, level: i64) -> Parts {
    // the journal head tells two models at one address apart
    let key = (
        &ed.model as *const Model as usize,
        ed.revision,
        level,
        ed.model.change_head(),
    );
    if let Some(p) = PARTS.with_borrow(|g| {
        g.as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, p)| p.clone())
    }) {
        return p;
    }
    let p: Parts = Arc::new(ed.model.struct_meshes(Some(level)).unwrap_or_default());
    PARTS.with_borrow_mut(|g| *g = Some((key, p.clone())));
    p
}

impl Editor {
    /// Level outlines and a selection without vanished elements (called from `reload`).
    pub(super) fn struct_reload(&mut self) {
        self.struct_fp = StructFp::load(&self.model, self.level);
        let fp = &self.struct_fp;
        self.selection.retain(|s| {
            !matches!(s, Sel::Ext(c, id)
                if (is_struct(*c)
                    || *c == crate::scene_ids::EXT_FITTING
                    || *c == EXT_TOPO
                    || crate::elem::is_hosted_ext(*c))
                    && !fp.contains(*c, *id))
        });
    }

    /// The element under a point: inside the outline (0) or nearer an edge than the radius; of nested ones, the smaller.
    /// Rebar and analytical lines are left to [`Self::hosted_pick`]: they lie inside their hosts.
    pub(super) fn struct_pick(&self, p: V2) -> Option<(Sel, f64)> {
        self.fp_pick(p, false)
    }

    /// A rebar or analytical line under the point; asked after hosts, so a host stays clickable
    /// and its bars are reached where nothing else is hit, or by Tab.
    pub(super) fn hosted_pick(&self, p: V2) -> Option<(Sel, f64)> {
        self.fp_pick(p, true)
    }

    fn fp_pick(&self, p: V2, hosted: bool) -> Option<(Sel, f64)> {
        let r = self.tool.radius;
        let mut best: Option<(Sel, f64, f64)> = None;
        for (c, id, rings) in &self.struct_fp.items {
            // a terrain covers the whole site: `topo_pick` takes it where nothing else is
            if crate::elem::is_hosted_ext(*c) != hosted || *c == EXT_TOPO {
                continue;
            }
            for o in rings.iter().filter(|o| o.len() >= 2) {
                let d = if o.len() >= 3 && !edge_only(*c) && inside_poly(p, o) {
                    0.0
                } else {
                    ring_dist(p, o, hosted)
                };
                if d > r {
                    continue;
                }
                let a = if hosted { 0.0 } else { ring_area(o) };
                if best.is_none_or(|b| (d, a) < (b.1, b.2)) {
                    best = Some((Sel::Ext(*c, *id), d, a));
                }
            }
        }
        best.map(|b| (b.0, b.1))
    }

    /// Terrain under the point, the smaller one when several overlap; picked last of all.
    pub(super) fn topo_pick(&self, p: V2) -> Option<Sel> {
        self.struct_fp
            .items
            .iter()
            .filter(|(c, _, rings)| *c == EXT_TOPO && rings.iter().any(|o| inside_poly(p, o)))
            .map(|(c, id, rings)| (Sel::Ext(*c, *id), rings.iter().map(|o| ring_area(o)).sum::<f64>()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|x| x.0)
    }

    /// Delete the selected structural elements (one undo step); true if there were any.
    /// Level bodies in scene coordinates (z from the level elevation): one mesh per tag.
    /// Level structural bodies as one piece per tag (3D is drawn by `edit3d::struct3d`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn struct_meshes(&self, out: &mut Vec<SceneMesh>) {
        let Ok(parts) = self.model.struct_meshes(Some(self.level)) else {
            return;
        };
        out.extend(merge_by_tag(&parts, self.elevation));
    }
}

/// Element meshes merged by tag (vertices and triangles back to back), element `OTHER_LEVEL`.
pub fn merge_by_tag(parts: &[(&'static str, i64, Mesh)], dz: f64) -> Vec<SceneMesh> {
    let mut by: Vec<SceneMesh> = vec![];
    for (kind, _, m) in parts {
        if m.triangles.is_empty() {
            continue;
        }
        let tag = tag_of(kind);
        let i = match by.iter().position(|s| s.tag == tag) {
            Some(i) => i,
            None => {
                by.push(SceneMesh {
                    element: OTHER_LEVEL,
                    tag,
                    positions: vec![],
                    triangles: vec![],
                });
                by.len() - 1
            }
        };
        let s = &mut by[i];
        let base = s.positions.len() as u32;
        s.positions.extend(
            m.positions
                .iter()
                .map(|p| [p.x as f32, p.y as f32, (p.z - dz) as f32]),
        );
        s.triangles.extend(
            m.triangles
                .iter()
                .map(|t| [t[0] + base, t[1] + base, t[2] + base]),
        );
    }
    by
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::wall_tool::{Event, Mode};
    use ddc_model::{FootingSpec, FootingStep};
    use ddc_render::TAG_SLAB;

    fn spec(e: &Editor, at: (f64, f64), steps: Vec<FootingStep>) -> FootingSpec {
        FootingSpec {
            level: e.level,
            column: None,
            at,
            rotation: 0.0,
            round: false,
            steps,
            top_offset: 0.0,
            material: "Concrete".into(),
        }
    }

    fn one(w: f64, h: f64) -> Vec<FootingStep> {
        vec![FootingStep { w, l: w, h }]
    }

    fn count(e: &Editor) -> i64 {
        e.model
            .sql()
            .query_row("SELECT count(*) FROM iso_footing", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn codes_round_trip() {
        for k in STRUCT_KINDS {
            let c = code_of(k).unwrap();
            assert!(c >= STRUCT_BASE && is_struct(c));
            assert_eq!(kind_of(c), Some(k));
            assert_eq!(of_sel(sel(k, 7).unwrap()), Some((k, 7)));
        }
        assert_eq!(kind_of(STRUCT_BASE - 1), None);
        assert_eq!(kind_of(STRUCT_BASE + STRUCT_KINDS.len() as u16), None);
        assert_eq!(of_sel(Sel::Wall(1)), None);
    }

    #[test]
    fn shaft_is_picked_by_click_edited_and_deleted_in_one_step() {
        use ddc_model::ShaftSpec;
        let mut e = Editor::new_in_memory().unwrap();
        let top = e.model.add_level("Top", 3.0).unwrap();
        let s = ShaftSpec {
            level: e.level,
            top_level: top,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
        };
        let id = e.model.add_shaft(&s).unwrap();
        e.reload().unwrap();
        let c = code_of("shaft").unwrap();
        assert_eq!(kind_of(c), Some("shaft"));
        assert_eq!(category_of(c), "shaft");
        assert!(e.struct_fp.contains(c, id));
        assert_eq!(e.struct_pick(v2(1.0, 1.0)).map(|x| x.0), Some(Sel::Ext(c, id)));
        // the panel fields write through one model op
        assert_eq!(crate::struct_props::values(&e.model, "shaft", id).unwrap(), vec![0.0, 0.0]);
        crate::struct_props::apply(&mut e.model, "shaft", id, &[0.5, 1.0]).unwrap();
        let t = e.model.shaft(id).unwrap();
        assert_eq!((t.base_offset, t.top_offset), (0.5, 1.0));
        e.model.undo().unwrap();
        assert_eq!(e.model.shaft(id).unwrap(), s);
        // Delete: one step, and undo brings the same rows back
        e.selection = vec![Sel::Ext(c, id)];
        e.delete_selection();
        assert!(e.model.shaft_ids().unwrap().is_empty());
        assert!(e.selection.is_empty());
        e.undo();
        assert_eq!(e.model.shaft(id).unwrap(), s);
    }

    #[test]
    fn footing_in_level_scene() {
        let mut e = Editor::new_in_memory().unwrap();
        let s = spec(&e, (1.0, 1.0), one(1.2, 0.5));
        e.model.add_footing(&s).unwrap();
        e.reload().unwrap();
        // in 3D the registry providers supply bodies; `Editor::scene` does not include them
        use crate::edit3d::scene3d::SceneProvider;
        let parts = crate::edit3d::struct3d::StructProvider { kind: "footing" }.parts(&e, e.level);
        let f = parts
            .iter()
            .find(|p| p.tag == TAG_SLAB)
            .expect("footing body");
        let zmin = f.pos.iter().map(|p| p[2]).fold(f32::MAX, f32::min);
        assert!((zmin + 0.5).abs() < 1e-5, "{zmin}");
    }

    #[test]
    fn struct_meshes_merged_per_tag() {
        let mut e = Editor::new_in_memory().unwrap();
        let stepped = vec![
            FootingStep {
                w: 1.8,
                l: 1.8,
                h: 0.3,
            },
            FootingStep {
                w: 1.0,
                l: 1.0,
                h: 0.3,
            },
        ];
        let ss = [
            spec(&e, (0.0, 0.0), one(1.2, 0.5)),
            spec(&e, (5.0, 0.0), stepped),
            spec(&e, (10.0, 0.0), one(1.5, 0.6)),
        ];
        e.model.add_footings(&ss).unwrap();
        e.reload().unwrap();
        let parts = e.model.struct_meshes(Some(e.level)).unwrap();
        let tris: usize = parts.iter().map(|p| p.2.triangles.len()).sum();
        let verts: usize = parts.iter().map(|p| p.2.positions.len()).sum();
        let mut out = vec![];
        e.struct_meshes(&mut out);
        assert_eq!(out.len(), 1, "one scene mesh per tag");
        assert_eq!(out[0].tag, TAG_SLAB);
        assert_eq!(out[0].element, OTHER_LEVEL);
        assert_eq!(out[0].triangles.len(), tris);
        assert_eq!(out[0].positions.len(), verts);
        let n = out[0].positions.len() as u32;
        assert!(out[0].triangles.iter().flatten().all(|&i| i < n));
    }

    #[test]
    fn click_selects_footing_delete_and_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let id = e
            .model
            .add_footing(&spec(&e, (3.0, 4.0), one(1.2, 0.5)))
            .unwrap();
        e.reload().unwrap();
        let s = sel("footing", id).unwrap();
        assert!(e.struct_fp.contains(code_of("footing").unwrap(), id));
        e.set_mode(Mode::Select);
        e.handle(Event::Click(v2(3.2, 4.1)));
        assert_eq!(e.selection, vec![s]);
        assert!(!crate::selection::outlines(&e, s).is_empty());
        e.delete_selection();
        assert_eq!(count(&e), 0);
        assert!(
            e.selection.is_empty(),
            "deleted element leaves the selection"
        );
        e.undo();
        assert_eq!(count(&e), 1, "one undo step restores the footing");
        e.redo();
        assert_eq!(count(&e), 0);
        e.undo();
        e.handle(Event::Click(v2(9.0, 9.0)));
        assert!(e.selection.is_empty());
    }

    fn has_wall(e: &Editor, id: i64) -> bool {
        e.model
            .sql()
            .query_row("SELECT count(*) FROM wall WHERE id=?1", [id], |r| r.get::<_, i64>(0))
            .unwrap()
            > 0
    }

    #[test]
    fn footing_and_wall_deleted_together_are_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        let f = e
            .model
            .add_footing(&spec(&e, (3.0, 4.0), one(1.2, 0.5)))
            .unwrap();
        let wt = e.types.first().map(|t| t.id).unwrap();
        let w = e
            .model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        e.reload().unwrap();
        e.selection = vec![Sel::Wall(w), sel("footing", f).unwrap()];
        e.delete_selection();
        assert!(e.model.footing(f).is_err() && !has_wall(&e, w));
        e.undo();
        assert!(e.model.footing(f).is_ok() && has_wall(&e, w));
    }

    #[test]
    fn box_and_filter_see_structure() {
        let mut e = Editor::new_in_memory().unwrap();
        let ss = [
            spec(&e, (0.0, 0.0), one(1.0, 0.5)),
            spec(&e, (4.0, 0.0), one(1.0, 0.5)),
        ];
        e.model.add_footings(&ss).unwrap();
        e.reload().unwrap();
        let mode = crate::selection::BoxMode::Window;
        let got = crate::selection::in_box(&e, v2(-1.0, -1.0), v2(5.0, 1.0), mode, |_| true);
        assert_eq!(got.len(), 2);
        assert_eq!(crate::selection::counts(&e, &got), vec![(CATEGORY, 2)]);
        let sim = crate::selection::similar(&e, got[0], false, |_| true);
        assert_eq!(sim.len(), 2);
    }
}
