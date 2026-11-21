// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scene: element meshes (element, layer) → flat GPU vertices and bounds for picking.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::sync::Arc;

pub const NO_SELECTION: u32 = u32::MAX;
/// Line tags (in the vertex `tag` field): face edges and the ground grid.
pub const TAG_EDGE: u32 = 0x8000_0000;
pub const TAG_GRID_MINOR: u32 = 0x8000_0001;
pub const TAG_GRID_MAJOR: u32 = 0x8000_0002;
pub const TAG_AXIS_X: u32 = 0x8000_0003;
pub const TAG_AXIS_Y: u32 = 0x8000_0004;
/// Face tags (shader palette): door, window, glass (transparent), transom, solid panel, slab.
pub const TAG_DOOR: u32 = 5;
pub const TAG_WINDOW: u32 = 6;
pub const TAG_GLASS: u32 = 8;
pub const TAG_MULLION: u32 = 9;
pub const TAG_PANEL: u32 = 10;
pub const TAG_SLAB: u32 = 11;
/// Face colour directly in the tag: `TAG_RGB | 0xRRGGBB` (network systems); with `TAG_TRANSLUCENT` —
/// a semi-transparent shell (insulation, cladding), drawn in the glass pass.
pub const TAG_RGB: u32 = 0x4000_0000;
pub const TAG_TRANSLUCENT: u32 = 0x2000_0000;

pub fn tag_rgb(rgb: [u8; 3], translucent: bool) -> u32 {
    let c = (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
    TAG_RGB | c | if translucent { TAG_TRANSLUCENT } else { 0 }
}

/// Foliage: a colour tag (`TAG_RGB`) with this bit is leaves, lit as a soft translucent mass in
/// the realistic style. The bit alone is the section fill (`style::TAG_CAP`), so it never comes
/// without `TAG_RGB` here.
pub const TAG_FOLIAGE: u32 = 0x1000_0000;

pub fn tag_foliage(rgb: [u8; 3]) -> u32 {
    tag_rgb(rgb, false) | TAG_FOLIAGE
}

pub fn is_foliage(tag: u32) -> bool {
    tag & 0xF000_0000 == TAG_RGB | TAG_FOLIAGE
}

/// The face is drawn with blending (glass, semi-transparent shells).
pub fn is_translucent(tag: u32) -> bool {
    tag == TAG_GLASS || tag & 0xE000_0000 == TAG_RGB | TAG_TRANSLUCENT
}

#[derive(Clone, Debug, Default)]
pub struct SceneMesh {
    pub element: u32,
    /// Layer/category — palette colour index.
    pub tag: u32,
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for Aabb {
    /// The empty box, so the first `grow`/`union` sets it.
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::splat(f32::INFINITY),
        max: Vec3::splat(f32::NEG_INFINITY),
    };
    pub fn grow(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }
    pub fn union(&mut self, o: &Aabb) {
        self.min = self.min.min(o.min);
        self.max = self.max.max(o.max);
    }
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub element: u32,
    pub tag: u32,
}

/// Mesh bounds (empty for a mesh without vertices).
pub fn mesh_box(m: &SceneMesh) -> Aabb {
    let mut b = Aabb::EMPTY;
    m.positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
    b
}

/// Meshes are shared (`Arc`): the caller's cache hands out unchanged ones without copying, and
/// chunks and the index recognise them by pointer.
#[derive(Debug, Default)]
pub struct Scene {
    /// Build number: empty `meshes` always share one address, so the scene cannot be told apart by it.
    serial: u64,
    pub meshes: Vec<Arc<SceneMesh>>,
    pub(crate) boxes: Vec<Aabb>,
    /// Tree over `boxes` for `pick`: small scenes build it immediately, large ones on first pick.
    pub(crate) pick_tree: std::sync::OnceLock<crate::perf::BoxTree>,
    pub bounds: Option<Aabb>,
    /// Repeated bodies: one mesh, many placements (`chunks::Instanced`).
    pub instanced: Vec<crate::chunks::Instanced>,
    /// Category number (0…15, bits 24–27 of the vertex tag) of mesh `i`, then of instance groups
    /// `meshes.len() + g`; no entry — 0. Category coverage is a frame uniform.
    pub cats: Vec<u8>,
    /// Zoom-to-fit box: the bounds without site context (`exclude_from_focus`).
    pub focus: Option<Aabb>,
}

/// Up to this many meshes the pick tree is built together with the scene: a fraction of a millisecond.
const PICK_TREE_EAGER: usize = 20_000;

fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Scene {
    /// Number of this build; `Scene::default()` — 0.
    pub fn serial(&self) -> u64 {
        self.serial
    }
    pub fn new(meshes: Vec<SceneMesh>) -> Self {
        Self::shared(meshes.into_iter().map(Arc::new).collect())
    }
    pub fn shared(meshes: Vec<Arc<SceneMesh>>) -> Self {
        let boxes: Vec<Aabb> = meshes.iter().map(|m| mesh_box(m)).collect();
        Self::from_parts(meshes, boxes)
    }
    /// Meshes with already computed bounds (`boxes[i]` — `mesh_box(&meshes[i])`).
    pub fn from_parts(meshes: Vec<Arc<SceneMesh>>, boxes: Vec<Aabb>) -> Self {
        assert_eq!(meshes.len(), boxes.len());
        let mut all = Aabb::EMPTY;
        boxes
            .iter()
            .filter(|b| !b.is_empty())
            .for_each(|b| all.union(b));
        let bounds = (!all.is_empty()).then_some(all);
        let pick_tree = std::sync::OnceLock::new();
        if boxes.len() <= PICK_TREE_EAGER {
            let _ = pick_tree.set(crate::perf::BoxTree::build(&boxes));
        }
        Self {
            serial: next_serial(),
            meshes,
            boxes,
            pick_tree,
            bounds,
            instanced: vec![],
            cats: vec![],
            focus: bounds,
        }
    }
    /// Category number of a mesh (`i < meshes.len()`) or of an instance group.
    pub fn cat(&self, i: usize) -> u8 {
        self.cats.get(i).copied().unwrap_or(0) & 15
    }
    pub fn with_cats(mut self, cats: Vec<u8>) -> Self {
        self.serial = next_serial();
        self.cats = cats;
        self
    }
    /// Scene with repeated bodies; the bounds include all their placements.
    pub fn with_instanced(mut self, v: Vec<crate::chunks::Instanced>) -> Self {
        self.serial = next_serial();
        let mut placed = Aabb::EMPTY;
        for g in &v {
            placed.union(&g.placed_bounds().all);
        }
        let mut all = self.bounds.unwrap_or(Aabb::EMPTY);
        all.union(&placed);
        self.bounds = (!all.is_empty()).then_some(all);
        let mut f = self.focus.unwrap_or(Aabb::EMPTY);
        f.union(&placed);
        self.focus = (!f.is_empty()).then_some(f);
        self.instanced = v;
        self
    }
    /// Zoom-to-fit leaves out meshes of `element` (terrain and other site context); falls back to
    /// the full bounds when nothing else is left.
    /// Bodies at least 1 m tall (walls, columns, furniture) set the box when there are any: flat
    /// slabs of roads and landscaping around a building would otherwise shrink it to a dot.
    pub fn exclude_from_focus(&mut self, element: u32) {
        let (mut tall, mut all) = (Aabb::EMPTY, Aabb::EMPTY);
        for (_, bx) in self
            .meshes
            .iter()
            .zip(&self.boxes)
            .filter(|(m, bx)| m.element != element && !bx.is_empty())
        {
            all.union(bx);
            if bx.max.z - bx.min.z >= 1.0 {
                tall.union(bx);
            }
        }
        // repeated bodies (one mesh, many placements) count by each placement
        for g in &self.instanced {
            let pb = g.placed_bounds();
            all.union(&pb.all);
            tall.union(&pb.tall);
        }
        self.focus = [tall, all]
            .into_iter()
            .find(|b| !b.is_empty())
            .or(self.bounds);
    }
    /// Bounds of every mesh (in `meshes` order).
    pub fn boxes(&self) -> &[Aabb] {
        &self.boxes
    }
    /// Bounds of elements in the range [start, end).
    pub fn bounds_of(&self, (a, b): (u32, u32)) -> Option<Aabb> {
        let mut all = Aabb::EMPTY;
        self.meshes
            .iter()
            .zip(&self.boxes)
            .filter(|(m, bx)| m.element >= a && m.element < b && !bx.is_empty())
            .for_each(|(_, bx)| all.union(bx));
        for g in &self.instanced {
            for i in g.inst.iter().filter(|i| i.element >= a && i.element < b) {
                all.union(&g.world_bounds(i));
            }
        }
        (!all.is_empty()).then_some(all)
    }
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.triangles.len()).sum()
    }
    /// Vertices of opaque faces: they come first in `vertices`, glass after.
    pub fn opaque_vertex_count(&self) -> usize {
        let glass: usize = self
            .meshes
            .iter()
            .filter(|m| is_translucent(m.tag))
            .map(|m| m.triangles.len())
            .sum();
        (self.triangle_count() - glass) * 3
    }

    /// Unshared vertices with a flat normal: wall faces stay crisp.
    /// Opaque meshes first, then glass (drawn in a second pass with blending).
    pub fn vertices(&self) -> Vec<Vertex> {
        let mut out = Vec::with_capacity(self.triangle_count() * 3);
        let opaque = self.meshes.iter().filter(|m| !is_translucent(m.tag));
        for m in opaque.chain(self.meshes.iter().filter(|m| is_translucent(m.tag))) {
            for t in &m.triangles {
                let p = t.map(|i| Vec3::from(m.positions[i as usize]));
                let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                for q in p {
                    out.push(Vertex {
                        pos: q.into(),
                        normal: n.into(),
                        element: m.element,
                        tag: m.tag,
                    });
                }
            }
        }
        out
    }

    /// Silhouette edges for the engineering look: mesh boundary and creases > 25° (diagonals
    /// of flat faces are not drawn). Vertices are compared by coordinates, not by indices.
    pub fn edge_vertices(&self) -> Vec<Vertex> {
        use std::collections::HashMap;
        let key = |p: Vec3| {
            let q = (p * 1.0e4).round();
            (q.x as i64, q.y as i64, q.z as i64)
        };
        let mut out = Vec::new();
        for m in &self.meshes {
            type K = (i64, i64, i64);
            let mut edges: HashMap<(K, K), (Vec3, Vec3, Vec<Vec3>)> = HashMap::new();
            for t in &m.triangles {
                let p = t.map(|i| Vec3::from(m.positions[i as usize]));
                let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                if n == Vec3::ZERO {
                    continue;
                }
                for k in 0..3 {
                    let (a, b) = (p[k], p[(k + 1) % 3]);
                    let (ka, kb) = (key(a), key(b));
                    let id = if ka < kb { (ka, kb) } else { (kb, ka) };
                    edges.entry(id).or_insert((a, b, Vec::new())).2.push(n);
                }
            }
            // sorted: map order is random per process, the line order must not be
            let mut edges: Vec<_> = edges.into_iter().collect();
            edges.sort_unstable_by_key(|(id, _)| *id);
            for (_, (a, b, ns)) in edges {
                let crease = match ns.as_slice() {
                    [_] => true,
                    [n0, rest @ ..] => rest.iter().any(|n| n0.dot(*n).abs() < 0.906),
                    [] => false,
                };
                if crease {
                    for q in [a, b] {
                        out.push(Vertex {
                            pos: q.into(),
                            normal: [0.0, 0.0, 1.0],
                            element: m.element,
                            tag: TAG_EDGE,
                        });
                    }
                }
            }
        }
        out
    }

    /// Ground grid under the model: 1 m step, every fifth line heavier in colour, X/Y axes.
    pub fn grid_vertices(&self) -> Vec<Vertex> {
        let (min, max) = match self.bounds {
            Some(b) => (b.min, b.max),
            None => (Vec3::splat(-10.0), Vec3::splat(10.0)),
        };
        let pad = ((max - min).truncate().length() * 0.5).max(5.0);
        let (x0, x1) = ((min.x - pad).floor() as i32, (max.x + pad).ceil() as i32);
        let (y0, y1) = ((min.y - pad).floor() as i32, (max.y + pad).ceil() as i32);
        let z = min.z.min(0.0) - 0.01;
        let mut out = Vec::new();
        let mut line = |a: Vec3, b: Vec3, tag: u32| {
            for q in [a, b] {
                out.push(Vertex {
                    pos: q.into(),
                    normal: [0.0, 0.0, 1.0],
                    element: NO_SELECTION - 1,
                    tag,
                });
            }
        };
        for x in x0..=x1 {
            let tag = match x {
                0 => TAG_AXIS_Y,
                _ if x % 5 == 0 => TAG_GRID_MAJOR,
                _ => TAG_GRID_MINOR,
            };
            line(
                Vec3::new(x as f32, y0 as f32, z),
                Vec3::new(x as f32, y1 as f32, z),
                tag,
            );
        }
        for y in y0..=y1 {
            let tag = match y {
                0 => TAG_AXIS_X,
                _ if y % 5 == 0 => TAG_GRID_MAJOR,
                _ => TAG_GRID_MINOR,
            };
            line(
                Vec3::new(x0 as f32, y as f32, z),
                Vec3::new(x1 as f32, y as f32, z),
                tag,
            );
        }
        out
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    #[test]
    fn box_edges_skip_face_diagonals() {
        let p: Vec<[f32; 3]> = (0..8)
            .map(|i| [(i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32])
            .collect();
        let q = |a, b, c, d| [[a, b, c], [a, c, d]];
        let tris: Vec<[u32; 3]> = [
            q(0, 2, 3, 1),
            q(4, 5, 7, 6),
            q(0, 1, 5, 4),
            q(2, 6, 7, 3),
            q(0, 4, 6, 2),
            q(1, 3, 7, 5),
        ]
        .concat();
        let s = Scene::new(vec![SceneMesh {
            element: 1,
            tag: 0,
            positions: p,
            triangles: tris,
        }]);
        assert_eq!(s.edge_vertices().len(), 24);
        assert!(s.grid_vertices().len() > 40);
    }
}

#[cfg(test)]
mod foliage_tests {
    use super::*;

    /// A foliage tag keeps its colour, is opaque, takes no texture layer and is not a section fill.
    #[test]
    fn foliage_tag_is_an_opaque_colour_tag_of_its_own() {
        let t = tag_foliage([0x54, 0x80, 0x3A]);
        assert!(is_foliage(t));
        assert_eq!(t & 0x00FF_FFFF, 0x0054_803A);
        assert!(!is_translucent(t));
        assert!(!crate::style::is_cap(t));
        assert!(!crate::texture::takes_layer(t));
        assert!(!is_foliage(tag_rgb([0x54, 0x80, 0x3A], false)));
        assert!(!is_foliage(crate::style::cap_tag([0x54, 0x80, 0x3A])));
        // the category bits the renderer adds do not change what it is
        assert!(is_foliage(crate::renderer::with_cat(t, 5)));
    }

    /// The realistic style lights foliage on its own path: wrapped, translucent, no sun highlight.
    #[test]
    fn realistic_foliage_has_no_specular_highlight() {
        let src = include_str!("realistic.wgsl");
        let f = &src[src.find("fn foliage(").unwrap()..src.find("@fragment").unwrap()];
        assert!(f.contains("wrap") && f.contains("against"), "wrapped diffuse and backlight");
        assert!(!f.contains("dot(n, h)"), "no sun highlight");
        assert!(src.contains("0x50000000u") && src.contains("i.seed"));
        assert!(include_str!("mesh.wgsl").contains("o.seed = mix32("));
    }
}
