// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D picking for all categories: scene bodies per element (walls, openings, curtain walls, slabs from
//! the editor; other categories are `scene3d` parts), rooms and missing parts come from the
//! `scene3d` providers; plus grip-provider bodies (`grips::Provider::bodies`).
//! Ray to all body hits (nearest per body, rooms last), window/crossing box,
//! feature edges (boundary or a crease > 15°) for highlighting and snaps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use ddc_render::chunks::{Inst, Instanced};
use ddc_render::glam::Vec3;
use ddc_render::pick::{ray_aabb, ray_triangle};
use ddc_render::{Aabb, Ray, Scene, SceneMesh};
use egui::{Pos2, Rect};

use super::math::{ray_segment, Cam};
use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, Sel};
use crate::scene_ids::{EXT_MASS, EXT_SOLID, EXT_TOPO};

/// Codes of the 3D categories the editor itself edits (body, mass, terrain).
pub fn native_ext(c: u16) -> bool {
    matches!(c, EXT_SOLID | EXT_MASS | EXT_TOPO)
}

/// Category of something pickable in 3D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cat {
    Wall,
    Opening,
    Curtain,
    Panel,
    Slab,
    Roof,
    Ceiling,
    Column,
    Beam,
    Stair,
    Railing,
    Comp,
    Room,
    Mep,
    /// Imported mesh (table `mesh`).
    Mesh,
    /// A category from another module's provider (the number is the provider's own).
    Ext(u16),
}

impl Cat {
    /// Label key `e3d.cat.<key>`.
    pub fn key(self) -> &'static str {
        match self {
            Cat::Wall => "wall",
            Cat::Opening => "opening",
            Cat::Curtain | Cat::Panel => "curtain",
            Cat::Slab => "slab",
            Cat::Roof => "roof",
            Cat::Ceiling => "ceiling",
            Cat::Column => "column",
            Cat::Beam => "beam",
            Cat::Stair => "stair",
            Cat::Railing => "railing",
            Cat::Comp => "comp",
            Cat::Room => "room",
            Cat::Mep => "mep",
            Cat::Mesh => "mesh",
            Cat::Ext(_) => "other",
        }
    }
}

fn ext_code(kind: &str) -> Option<u16> {
    crate::editor::structx::code_of(kind)
}

fn body_cat(k: BodyKind) -> Cat {
    match k {
        BodyKind::Mesh => Cat::Mesh,
        BodyKind::Column => Cat::Column,
        BodyKind::Beam => Cat::Beam,
        BodyKind::Ceiling => Cat::Ceiling,
    }
}

/// A model element in 3D: category, id, part number (curtain-wall panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Target {
    pub cat: Cat,
    pub id: i64,
    pub sub: u32,
}

impl Target {
    pub fn new(cat: Cat, id: i64) -> Target {
        Target { cat, id, sub: 0 }
    }

    pub fn from_sel(s: Sel) -> Option<Target> {
        Some(match s {
            Sel::Wall(id) => Target::new(Cat::Wall, id),
            Sel::Opening(id) => Target::new(Cat::Opening, id),
            Sel::Curtain(id) => Target::new(Cat::Curtain, id),
            Sel::Panel(id, k) => Target {
                cat: Cat::Panel,
                id,
                sub: k,
            },
            Sel::Slab(id) => Target::new(Cat::Slab, id),
            Sel::Room(id) => Target::new(Cat::Room, id),
            Sel::Mep(id) => Target::new(Cat::Mep, id),
            Sel::Comp(id) => Target::new(Cat::Comp, id),
            Sel::Ext(c, id) if c == crate::editor::structx::ROOF_CODE => Target::new(Cat::Roof, id),
            Sel::Ext(c, id) => match crate::editor::structx::kind_of(c) {
                Some("stair") => Target::new(Cat::Stair, id),
                Some("railing") => Target::new(Cat::Railing, id),
                _ => Target::new(Cat::Ext(c), id),
            },
            Sel::Body(k, id) => Target::new(body_cat(k), id),
            // masses are edited by `mass_tools` (own scene numbers), not by 3D providers
            Sel::Mass(id) => Target::new(Cat::Ext(crate::scene_ids::EXT_MASS), id),
            Sel::Anno(_) => return None,
        })
    }

    /// The editor selection (shared with the plan), if the category has one.
    pub fn sel(self) -> Option<Sel> {
        Some(match self.cat {
            Cat::Wall => Sel::Wall(self.id),
            Cat::Opening => Sel::Opening(self.id),
            Cat::Curtain => Sel::Curtain(self.id),
            Cat::Panel => Sel::Panel(self.id, self.sub),
            Cat::Slab => Sel::Slab(self.id),
            Cat::Room => Sel::Room(self.id),
            Cat::Mep => Sel::Mep(self.id),
            Cat::Comp => Sel::Comp(self.id),
            Cat::Roof => Sel::Ext(crate::editor::structx::ROOF_CODE, self.id),
            Cat::Stair => return crate::editor::structx::sel("stair", self.id),
            Cat::Railing => return crate::editor::structx::sel("railing", self.id),
            Cat::Ext(c) if c == crate::scene_ids::EXT_MASS => Sel::Mass(self.id),
            Cat::Ext(c) if native_ext(c) => Sel::Ext(c, self.id),
            Cat::Ext(c)
                if crate::editor::structx::kind_of(c).is_some() || crate::elem::is_hosted_ext(c) =>
            {
                Sel::Ext(c, self.id)
            }
            // editor bodies: the pick is shared with the plan, properties and deletion
            Cat::Mesh => Sel::Body(BodyKind::Mesh, self.id),
            Cat::Column => Sel::Body(BodyKind::Column, self.id),
            Cat::Beam => Sel::Body(BodyKind::Beam, self.id),
            Cat::Ceiling => Sel::Body(BodyKind::Ceiling, self.id),
            _ => return None,
        })
    }

    /// The plan grips kind (`crate::grips`), if any.
    pub fn grip_kind(self) -> Option<crate::grips::Kind> {
        use crate::grips::Kind;
        Some(match self.cat {
            Cat::Wall => Kind::Wall,
            Cat::Slab => Kind::Slab,
            Cat::Ceiling => Kind::Ceiling,
            Cat::Roof => Kind::Roof,
            Cat::Column => Kind::Column,
            Cat::Beam => Kind::Beam,
            Cat::Stair => Kind::Stair,
            Cat::Railing => Kind::Railing,
            Cat::Comp => Kind::Comp,
            Cat::Mep => Kind::Mep,
            Cat::Mesh => Kind::Mesh,
            Cat::Curtain => Kind::Curtain,
            Cat::Ext(c) if c == EXT_SOLID => Kind::Solid,
            Cat::Ext(c) if c == EXT_MASS => Kind::Mass,
            Cat::Ext(c) if c == EXT_TOPO => Kind::Topo,
            Cat::Ext(_) => match crate::elem::of_cat(self.cat)?.grips {
                crate::elem::Cap::Yes(k) => k,
                crate::elem::Cap::No(_) => return None,
            },
            Cat::Room => Kind::Room,
            _ => return None,
        })
    }

    pub fn from_grip_kind(k: crate::grips::Kind, id: i64) -> Option<Target> {
        use crate::grips::Kind;
        let cat = match k {
            Kind::Wall => Cat::Wall,
            Kind::Slab => Cat::Slab,
            Kind::Ceiling => Cat::Ceiling,
            Kind::Roof => Cat::Roof,
            Kind::Column => Cat::Column,
            Kind::Beam => Cat::Beam,
            Kind::Stair => Cat::Stair,
            Kind::Railing => Cat::Railing,
            Kind::Comp => Cat::Comp,
            Kind::Mep => Cat::Mep,
            Kind::Mesh => Cat::Mesh,
            Kind::Curtain => Cat::Curtain,
            Kind::Footing => Cat::Ext(ext_code("footing")?),
            Kind::StripFooting => Cat::Ext(ext_code("strip_footing")?),
            Kind::Raft => Cat::Ext(ext_code("raft")?),
            Kind::Brace => Cat::Ext(ext_code("brace")?),
            Kind::Truss => Cat::Ext(ext_code("truss")?),
            Kind::BeamSystem => Cat::Ext(ext_code("beam_system")?),
            Kind::Ramp => Cat::Ext(ext_code("ramp")?),
            Kind::Solid => Cat::Ext(EXT_SOLID),
            Kind::Mass => Cat::Ext(EXT_MASS),
            Kind::Topo => Cat::Ext(EXT_TOPO),
            Kind::Shaft => Cat::Ext(ext_code("shaft")?),
            Kind::Zone => Cat::Ext(ext_code("zone")?),
            Kind::Grid => Cat::Ext(ext_code("grid")?),
            Kind::RefPlane => Cat::Ext(ext_code("ref_plane")?),
            Kind::Room => Cat::Room,
            Kind::RoomSep => Cat::Ext(ext_code("room_sep")?),
            _ => return None,
        };
        Some(Target::new(cat, id))
    }

    /// Target of model edit operations (`ddc_model::modify`), if the category supports them.
    pub fn modify(self) -> Option<ddc_model::Target> {
        use ddc_model::Target as T;
        Some(match self.cat {
            Cat::Wall => T::Wall(self.id),
            Cat::Opening => T::Opening(self.id),
            Cat::Curtain => T::Curtain(self.id),
            Cat::Slab => T::Slab(self.id),
            Cat::Room => T::Room(self.id),
            Cat::Comp => T::Component(self.id),
            Cat::Ext(c) if c == EXT_SOLID => T::Solid(self.id),
            Cat::Ext(c) if c == EXT_MASS => T::Mass(self.id),
            Cat::Ext(c) if c == EXT_TOPO => T::Topo(self.id),
            Cat::Ext(c) => return ext_plan_target(c, self.id),
            _ => return None,
        })
    }
}

/// Plan operations target of a footing, strip, raft, brace, truss, beam system or ramp.
pub(crate) fn ext_plan_target(code: u16, id: i64) -> Option<ddc_model::Target> {
    use ddc_model::Target as T;
    Some(match crate::editor::structx::kind_of(code)? {
        "footing" => T::Footing(id),
        "strip_footing" => T::StripFooting(id),
        "raft" => T::Raft(id),
        "brace" => T::Brace(id),
        "truss" => T::Truss(id),
        "beam_system" => T::BeamSystem(id),
        "ramp" => T::Ramp(id),
        "shaft" => T::Shaft(id),
        "zone" => T::Zone(id),
        "stair" => T::Stair(id),
        "railing" => T::Railing(id),
        "grid" => T::Grid(id),
        "ref_plane" => T::RefPlane(id),
        _ => return None,
    })
}

/// Where the body's triangles come from.
#[derive(Clone, Debug)]
pub enum Src {
    /// Scene meshes (indices into `Scene::meshes`).
    Scene(Vec<usize>),
    /// Placement `k` of instance group `g` (`Scene::instanced`).
    Inst {
        g: usize,
        k: usize,
    },
    Own {
        pos: Vec<Vec3>,
        tris: Vec<[u32; 3]>,
    },
}

/// Feature edges and their endpoints (for highlighting and snaps).
#[derive(Clone, Debug, Default)]
pub struct Feature {
    pub edges: Vec<[Vec3; 2]>,
    pub verts: Vec<Vec3>,
}

#[derive(Clone, Debug)]
pub struct Body {
    /// None means it occludes but is not pickable (other levels, node fillers).
    pub target: Option<Target>,
    pub src: Src,
    pub aabb: Aabb,
    /// 0 is an element, 1 is a room (comes after elements under the cursor).
    pub rank: u8,
    feat: Option<Feature>,
}

impl Body {
    pub fn own(target: Target, pos: Vec<Vec3>, tris: Vec<[u32; 3]>, rank: u8) -> Body {
        let mut aabb = Aabb::EMPTY;
        pos.iter().for_each(|p| aabb.grow(*p));
        Body {
            target: Some(target),
            src: Src::Own { pos, tris },
            aabb,
            rank,
            feat: None,
        }
    }

    /// Every triangle of the body (broken indices are skipped).
    pub fn for_tris(&self, scene: &Scene, mut f: impl FnMut([Vec3; 3])) {
        match &self.src {
            Src::Scene(ms) => {
                for &mi in ms {
                    let Some(m) = scene.meshes.get(mi) else {
                        continue;
                    };
                    for t in &m.triangles {
                        let g = |i: u32| m.positions.get(i as usize).map(|p| Vec3::from(*p));
                        if let (Some(a), Some(b), Some(c)) = (g(t[0]), g(t[1]), g(t[2])) {
                            f([a, b, c]);
                        }
                    }
                }
            }
            Src::Inst { g, k } => {
                let Some(grp) = scene.instanced.get(*g) else {
                    return;
                };
                let Some(i) = grp.inst.get(*k) else {
                    return;
                };
                let m = &grp.proto;
                for t in &m.triangles {
                    let g = |j: u32| {
                        m.positions
                            .get(j as usize)
                            .map(|p| i.m.transform_point3(Vec3::from(*p)))
                    };
                    if let (Some(a), Some(b), Some(c)) = (g(t[0]), g(t[1]), g(t[2])) {
                        f([a, b, c]);
                    }
                }
            }
            Src::Own { pos, tris } => {
                for t in tris {
                    let g = |i: u32| pos.get(i as usize).copied();
                    if let (Some(a), Some(b), Some(c)) = (g(t[0]), g(t[1]), g(t[2])) {
                        f([a, b, c]);
                    }
                }
            }
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.aabb.min + self.aabb.max) * 0.5
    }
}

/// A ray hit on a body: the nearest triangle of the body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit3 {
    pub body: usize,
    pub target: Target,
    pub t: f32,
    pub point: Vec3,
    /// Face normal facing the ray.
    pub normal: Vec3,
}

/// Freshness key of the body set: model revision, level, scene build number (`gen`), and the address and
/// size of the mesh list (without walking triangles: a scene can have millions).
pub type Key = (u64, i64, u64, usize, usize);

pub fn key(ed: &Editor, scene: &Scene, gen: u64) -> Key {
    (
        ed.revision,
        ed.level,
        gen,
        scene.meshes.as_ptr() as usize,
        scene.meshes.len() + scene.instanced.iter().map(|g| g.inst.len()).sum::<usize>(),
    )
}

/// Triangle count in a body above which the feature edges are the edges of its bounding box.
pub const FEATURE_TRIS: usize = 200_000;

/// Ray and box without an index, without a section.
pub const ALL: Query<'static> = Query {
    index: None,
    clip: None,
    full: false,
};

/// Query limits: the scene index (if built for it) and the view's section box.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query<'a> {
    pub index: Option<&'a crate::spatial::Index>,
    pub clip: Option<Aabb>,
    /// Scan all bodies even on a large scene without an index: for one-off clicks and boxes, not per frame.
    pub full: bool,
}

impl Query<'_> {
    pub fn keeps(&self, p: Vec3) -> bool {
        self.clip.is_none_or(|c| inside(&c, p))
    }
}

fn inside(c: &Aabb, p: Vec3) -> bool {
    let e = Vec3::splat(1e-4);
    p.cmpge(c.min - e).all() && p.cmple(c.max + e).all()
}

fn overlaps(a: &Aabb, b: &Aabb) -> bool {
    a.min.cmple(b.max).all() && a.max.cmpge(b.min).all()
}

/// No body at this placement (its box is empty).
const NO_BODY: usize = usize::MAX;

/// A placement group between scenes: addresses of its placement list and prototype (the scheme
/// of `spatial::Index`).
fn group_id(g: &Instanced) -> (usize, usize) {
    (Arc::as_ptr(&g.inst) as usize, Arc::as_ptr(&g.proto) as usize)
}

#[derive(Clone, Debug, Default)]
pub struct Bodies {
    pub key: Option<Key>,
    pub list: Vec<Body>,
    by_target: HashMap<Target, usize>,
    /// Scene mesh to body; placement (group, number) to body; bodies outside the scene.
    mesh_body: HashMap<usize, usize>,
    /// Placement body by group and number; `NO_BODY` where the placement box is empty.
    inst_body: Vec<Vec<usize>>,
    own: Vec<usize>,
    /// Grid axis segments `(body, a, b)` for the pixel-tolerance pick (arcs are sampled).
    grid_segs: Vec<(usize, Vec3, Vec3)>,
    /// Scene sources of the last build, held so their addresses stay unique: `update` tells
    /// unchanged meshes and placement groups by address.
    keep_meshes: Vec<Arc<SceneMesh>>,
    keep_groups: Vec<(Arc<Vec<Inst>>, Arc<SceneMesh>, Aabb)>,
    /// Scene bodies (meshes, placements) of the last build taken from the previous set.
    pub reused: usize,
    /// Scene bodies of the last build built anew.
    pub rebuilt: usize,
}

/// Pixel tolerance of a grid line pick.
pub const GRID_PX: f32 = 6.0;

/// True for the body of a grid line: it can be picked but is never a face-snap target.
pub fn is_grid(t: Target) -> bool {
    crate::editor::structx::code_of("grid").is_some_and(|c| t.cat == Cat::Ext(c))
}

impl Bodies {
    /// Axis polylines `(id, points)` of the grids that already have a body; their segments
    /// (straight or sampled arc) are picked within a pixel tolerance, at the body height.
    pub fn add_grid_lines(&mut self, lines: &[(i64, Vec<(f64, f64)>)]) {
        let Some(code) = crate::editor::structx::code_of("grid") else {
            return;
        };
        for (id, pts) in lines {
            let Some(&bi) = self.by_target.get(&Target::new(Cat::Ext(code), *id)) else {
                continue;
            };
            let z = self.list[bi].center().z;
            for w in pts.windows(2) {
                let p = |q: (f64, f64)| Vec3::new(q.0 as f32, q.1 as f32, z);
                let (a, b) = (p(w[0]), p(w[1]));
                if (b - a).length_squared() > 1e-12 {
                    self.grid_segs.push((bi, a, b));
                }
            }
        }
    }

    /// Registers the axes of all model grids for the pixel pick; call after every (re)build, the
    /// set is not part of the cached scene bodies.
    pub fn add_model_grids(&mut self, ed: &Editor) {
        let lines: Vec<(i64, Vec<(f64, f64)>)> = ed
            .model
            .grids()
            .unwrap_or_default()
            .into_iter()
            .map(|g| (g.id, crate::elem::datum::grid_line(g.spec.a, g.spec.b, g.spec.bulge)))
            .collect();
        self.grid_segs.clear();
        self.add_grid_lines(&lines);
    }

    /// Ray hits plus grid lines within `tol` pixels of `pos`; sorted as `hits_in`.
    pub fn hits_px(&self, scene: &Scene, cam: &Cam, pos: Pos2, q: &Query, tol: f32) -> Vec<Hit3> {
        let ray = cam.ray(pos);
        let mut out = self.hits_in(scene, &ray, q);
        let n = out.len();
        for &(bi, a, b) in &self.grid_segs {
            let Some(target) = self.list[bi].target else {
                continue;
            };
            let p = ray_segment(&ray, a, b);
            let t = (p - ray.origin).dot(ray.dir);
            if t <= 0.0 || cam.px_dist(p, pos) > tol || !q.keeps(p) {
                continue;
            }
            // one hit per grid: the closest segment to the cursor
            if let Some(h) = out.iter_mut().find(|h| h.target == target) {
                if h.t > t + 1e-4 && cam.px_dist(h.point, pos) > cam.px_dist(p, pos) {
                    h.t = t;
                    h.point = p;
                }
                continue;
            }
            out.push(Hit3 {
                body: bi,
                target,
                t,
                point: p,
                normal: Vec3::Z,
            });
        }
        if out.len() > n {
            out.sort_by(|a, b| {
                let (ra, rb) = (self.list[a.body].rank, self.list[b.body].rank);
                ra.cmp(&rb).then(a.t.total_cmp(&b.t))
            });
        }
        out
    }

    pub fn from_list(list: Vec<Body>) -> Bodies {
        let mut out = Bodies::default();
        out.list.reserve(list.len());
        for b in list {
            out.push(b);
        }
        out
    }

    fn push(&mut self, b: Body) {
        let i = self.list.len();
        if let Some(t) = b.target {
            self.by_target.entry(t).or_insert(i);
        }
        match &b.src {
            Src::Scene(ms) => {
                for &m in ms {
                    self.mesh_body.insert(m, i);
                }
            }
            &Src::Inst { g, k } => {
                if self.inst_body.len() <= g {
                    self.inst_body.resize_with(g + 1, Vec::new);
                }
                let row = &mut self.inst_body[g];
                if row.len() <= k {
                    row.resize(k + 1, NO_BODY);
                }
                row[k] = i;
            }
            Src::Own { .. } => self.own.push(i),
        }
        self.list.push(b);
    }

    fn body_of(&self, ix: &crate::spatial::Index, k: u32) -> Option<usize> {
        match ix.item(k).0 {
            crate::spatial::Item::Mesh(m) => self.mesh_body.get(&(m as usize)).copied(),
            crate::spatial::Item::Inst(g, k) => self
                .inst_body
                .get(g as usize)
                .and_then(|r| r.get(k as usize))
                .copied()
                .filter(|&b| b != NO_BODY),
        }
    }

    /// Target of the nearest ray hit.
    pub fn first_hit(&self, scene: &Scene, ray: Option<&Ray>, q: &Query) -> Option<Target> {
        self.hits_in(scene, ray?, q).first().map(|h| h.target)
    }

    /// Candidate bodies: via the scene index (`ks` are its positions) if built for it, otherwise all.
    fn cands(
        &self,
        scene: &Scene,
        q: &Query,
        ks: impl FnOnce(&crate::spatial::Index) -> Vec<u32>,
    ) -> Vec<usize> {
        let Some(ix) = q.index.filter(|ix| ix.matches(scene)) else {
            // the large-scene index is still being built in the background: scanning all bodies would stall the frame
            if crate::spatial::is_big(scene) && !q.full {
                return self.own.clone();
            }
            return (0..self.list.len()).collect();
        };
        let mut v: Vec<usize> = self.own.clone();
        v.extend(ks(ix).into_iter().filter_map(|k| self.body_of(ix, k)));
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Bodies whose boxes are closer than `r` to point `p` (snaps to nearby ends).
    pub fn near(&self, scene: &Scene, p: Vec3, r: f32, q: &Query) -> Vec<usize> {
        let mut v = self.cands(scene, q, |ix| ix.near(p, r));
        v.retain(|&i| {
            self.list[i].aabb.min.cmple(p + r).all() && self.list[i].aabb.max.cmpge(p - r).all()
        });
        v
    }

    /// Scene bodies without the scene providers' parts (their bodies are rebuilt).
    pub fn build(ed: &Editor, scene: &Scene) -> Bodies {
        Bodies::build_with(ed, scene, &[])
    }

    /// Scene bodies per element (`ext` are the `scene3d::compose` part targets) plus the scene providers'
    /// bodies that are not in it (all of them when `ext` is empty), plus grip-provider bodies.
    pub fn build_with(ed: &Editor, scene: &Scene, ext: &[Target]) -> Bodies {
        Bodies::update(None, ed, scene, ext)
    }

    /// The bodies of `build_with` for a new scene, built from the previous set: an element whose
    /// meshes are the same `Arc`s in the same order, and a placement of a group with the same
    /// placement list, prototype and prototype box, keep their box and computed feature edges
    /// instead of walking the geometry again. Targets and provider bodies are always rebuilt, so
    /// the result (bodies, order, hits) equals a full build of the same scene.
    pub fn update(prev: Option<Bodies>, ed: &Editor, scene: &Scene, ext: &[Target]) -> Bodies {
        use super::scene3d;
        let Bodies {
            list: mut old,
            mesh_body: old_mesh_body,
            inst_body: old_inst,
            keep_meshes: old_meshes,
            keep_groups: old_groups,
            ..
        } = prev.unwrap_or_default();
        // previous mesh address -> its body
        let old_by_ptr: HashMap<usize, usize> = old_meshes
            .iter()
            .enumerate()
            .filter_map(|(i, m)| Some((Arc::as_ptr(m) as usize, *old_mesh_body.get(&i)?)))
            .collect();
        let old_group: HashMap<(usize, usize), usize> = old_groups
            .iter()
            .enumerate()
            .map(|(g, (inst, proto, _))| {
                (
                    (Arc::as_ptr(inst) as usize, Arc::as_ptr(proto) as usize),
                    g,
                )
            })
            .collect();
        let mut by_el: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, m) in scene.meshes.iter().enumerate() {
            by_el.entry(m.element).or_default().push(i);
        }
        let placed: usize = scene.instanced.iter().map(|g| g.inst.len()).sum();
        let mut out = Bodies {
            list: Vec::with_capacity(by_el.len() + placed),
            by_target: HashMap::with_capacity(by_el.len() + placed),
            ..Bodies::default()
        };
        for (el, ms) in by_el {
            let target = match scene3d::target_of(el, ext) {
                Some(Some(t)) => Some(t),
                // a provider part with no target map: its body comes from the provider below
                Some(None) => continue,
                None => ed.sel_of_element(el).and_then(Target::from_sel),
            };
            let same = old_by_ptr
                .get(&(Arc::as_ptr(&scene.meshes[ms[0]]) as usize))
                .copied()
                .filter(|&ob| match &old[ob].src {
                    Src::Scene(om) => {
                        om.len() == ms.len()
                            && om.iter().zip(&ms).all(|(&o, &n)| {
                                old_meshes
                                    .get(o)
                                    .is_some_and(|a| Arc::ptr_eq(a, &scene.meshes[n]))
                            })
                    }
                    _ => false,
                });
            let (aabb, feat) = match same {
                Some(ob) => (old[ob].aabb, old[ob].feat.take()),
                None => {
                    let mut aabb = Aabb::EMPTY;
                    for &mi in &ms {
                        for p in &scene.meshes[mi].positions {
                            aabb.grow(Vec3::from(*p));
                        }
                    }
                    (aabb, None)
                }
            };
            if aabb.is_empty() {
                continue;
            }
            if same.is_some() {
                out.reused += 1;
            } else {
                out.rebuilt += 1;
            }
            out.push(Body {
                target,
                src: Src::Scene(ms),
                aabb,
                rank: u8::from(target.is_some_and(|t| t.cat == Cat::Room)),
                feat,
            });
        }
        // components as instances (`mep3d::scene`): one body per placement
        for (g, grp) in scene.instanced.iter().enumerate() {
            let row: Option<&[usize]> = old_group
                .get(&group_id(grp))
                .copied()
                .filter(|&og| old_groups[og].2 == grp.local)
                .map(|og| old_inst.get(og).map_or(&[][..], |r| r.as_slice()));
            for (k, i) in grp.inst.iter().enumerate() {
                let was = row
                    .and_then(|r| r.get(k))
                    .copied()
                    .filter(|&ob| ob != NO_BODY);
                let (aabb, feat) = match was {
                    Some(ob) => (old[ob].aabb, old[ob].feat.take()),
                    None => (grp.world_bounds(i), None),
                };
                if aabb.is_empty() {
                    continue;
                }
                if was.is_some() {
                    out.reused += 1;
                } else {
                    out.rebuilt += 1;
                }
                // cache meshes use part slots; components use their own range
                let target = match scene3d::target_of(i.element, ext) {
                    Some(t) => t,
                    None => ed.sel_of_element(i.element).and_then(Target::from_sel),
                };
                out.push(Body {
                    target,
                    src: Src::Inst { g, k },
                    aabb,
                    rank: 0,
                    feat,
                });
            }
        }
        let mut extra: Vec<Body> = scene3d::parts(ed, ed.level, ext.is_empty(), true)
            .into_iter()
            .map(|(p, rank)| Body::own(p.target, p.pos, p.tris, rank))
            .collect();
        for p in super::grips::providers() {
            extra.extend(p.bodies(ed));
        }
        // a target with a scene body keeps it; among providers the first body of a target wins
        let mut seen = HashSet::new();
        for b in extra {
            if b
                .target
                .is_some_and(|t| !out.by_target.contains_key(&t) && seen.insert(t))
                && !b.aabb.is_empty()
            {
                out.push(b);
            }
        }
        out.keep_meshes = scene.meshes.clone();
        out.keep_groups = scene
            .instanced
            .iter()
            .map(|g| (g.inst.clone(), g.proto.clone(), g.local))
            .collect();
        out.key = Some(key(ed, scene, 0));
        out
    }

    pub fn of(&self, t: Target) -> Option<usize> {
        self.by_target.get(&t).copied()
    }

    /// All pickable ray hits: one per body; hits occluded by bodies with no target are
    /// dropped; order is elements by distance, then rooms.
    pub fn hits(&self, scene: &Scene, r: &Ray) -> Vec<Hit3> {
        self.hits_in(scene, r, &ALL)
    }

    /// The same via the scene index and with a section: points outside the section box are not hits (behind a
    /// clipped part, whatever is inside gets picked).
    pub fn hits_in(&self, scene: &Scene, r: &Ray, q: &Query) -> Vec<Hit3> {
        let mut out: Vec<Hit3> = vec![];
        let mut occ = f32::INFINITY;
        for i in self.cands(scene, q, |ix| ix.along(r)) {
            let b = &self.list[i];
            if q.clip.is_some_and(|c| !overlaps(&c, &b.aabb)) {
                continue;
            }
            let Some(tb) = ray_aabb(r, &b.aabb) else {
                continue;
            };
            if tb > occ {
                continue;
            }
            let mut best: Option<(f32, Vec3)> = None;
            b.for_tris(scene, |[a, bb, c]| {
                if let Some(t) = ray_triangle(r, a, bb, c) {
                    if best.is_none_or(|x| t < x.0) && q.keeps(r.at(t)) {
                        let n = (bb - a).cross(c - a).normalize_or(Vec3::Z);
                        best = Some((t, if n.dot(r.dir) > 0.0 { -n } else { n }));
                    }
                }
            });
            let Some((t, normal)) = best else {
                continue;
            };
            match b.target {
                None => occ = occ.min(t),
                Some(target) => out.push(Hit3 {
                    body: i,
                    target,
                    t,
                    point: r.at(t),
                    normal,
                }),
            }
        }
        out.retain(|h| h.t <= occ + 0.01);
        out.sort_by(|a, b| {
            let (ra, rb) = (self.list[a.body].rank, self.list[b.body].rank);
            ra.cmp(&rb).then(a.t.total_cmp(&b.t))
        });
        // an element made of several bodies (meshes + instances) gives one hit, the nearest
        let mut seen = HashSet::new();
        out.retain(|h| seen.insert(h.target));
        out
    }

    /// Screen box: window needs the body fully inside, crossing needs only a touch.
    pub fn in_box(&self, scene: &Scene, cam: &Cam, r: Rect, crossing: bool) -> Vec<Target> {
        self.in_box_in(scene, cam, r, crossing, &ALL)
    }

    /// The same via the index and with a section: bodies outside the section box are not picked; rooms
    /// (volumes behind elements) only by window.
    pub fn in_box_in(
        &self,
        scene: &Scene,
        cam: &Cam,
        r: Rect,
        crossing: bool,
        q: &Query,
    ) -> Vec<Target> {
        let c = cam.rect;
        let ndc = |p: Pos2| {
            (
                (p.x - c.min.x) / c.width().max(1.0) * 2.0 - 1.0,
                1.0 - (p.y - c.min.y) / c.height().max(1.0) * 2.0,
            )
        };
        let (a, b) = (ndc(r.min), ndc(r.max));
        let (lo, hi) = ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)));
        let mut out = vec![];
        for i in self.cands(scene, q, |ix| ix.in_screen_rect(&cam.vp, lo, hi, false)) {
            let b = &self.list[i];
            let Some(t) = b.target else {
                continue;
            };
            if b.rank > 0 && crossing {
                continue;
            }
            if let Some(c) = q.clip {
                let keep = if crossing {
                    overlaps(&c, &b.aabb)
                } else {
                    inside(&c, b.aabb.min) && inside(&c, b.aabb.max)
                };
                if !keep {
                    continue;
                }
            }
            if box_test(b, scene, cam, r, crossing) && !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }

    /// Feature edges of a body (computed once per body set).
    pub fn feature(&mut self, i: usize, scene: &Scene) -> &Feature {
        if self.list[i].feat.is_none() {
            let mut tris = vec![];
            let mut big = false;
            self.list[i].for_tris(scene, |t| {
                if tris.len() < FEATURE_TRIS {
                    tris.push(t);
                } else {
                    big = true;
                }
            });
            self.list[i].feat = Some(if big {
                box_edges(&self.list[i].aabb)
            } else {
                feature_edges(&tris)
            });
        }
        self.list[i].feat.as_ref().expect("feature")
    }
}

/// Box edges (highlighting huge bodies without examining their triangles).
pub fn box_edges(b: &Aabb) -> Feature {
    let c = |k: u32| {
        Vec3::new(
            if k & 1 == 0 { b.min.x } else { b.max.x },
            if k & 2 == 0 { b.min.y } else { b.max.y },
            if k & 4 == 0 { b.min.z } else { b.max.z },
        )
    };
    let mut f = Feature::default();
    for k in 0..8u32 {
        f.verts.push(c(k));
        for bit in [1, 2, 4] {
            if k & bit == 0 {
                f.edges.push([c(k), c(k | bit)]);
            }
        }
    }
    f
}

fn box_test(b: &Body, scene: &Scene, cam: &Cam, r: Rect, crossing: bool) -> bool {
    use super::math::seg_hits_rect;
    if !crossing {
        let mut all = true;
        let mut any = false;
        b.for_tris(scene, |tri| {
            if !all {
                return;
            }
            for p in tri {
                any = true;
                if !cam.to_screen(p).is_some_and(|s| r.contains(s)) {
                    all = false;
                }
            }
        });
        return all && any;
    }
    let mut hit = false;
    b.for_tris(scene, |tri| {
        if hit {
            return;
        }
        let s = tri.map(|p| cam.to_screen(p));
        let [Some(a), Some(bb), Some(c)] = s else {
            return;
        };
        hit = seg_hits_rect(a, bb, r)
            || seg_hits_rect(bb, c, r)
            || seg_hits_rect(c, a, r)
            || inside_tri(r.center(), a, bb, c);
    });
    hit
}

fn inside_tri(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let s = |u: Pos2, v: Pos2| (v - u).x * (p - u).y - (v - u).y * (p - u).x;
    if ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs() < 1e-6 {
        return false;
    }
    let (d1, d2, d3) = (s(a, b), s(b, c), s(c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// Feature edges: after welding vertices (0.1 mm), boundary edges and those with a normal crease > 15°.
pub fn feature_edges(tris: &[[Vec3; 3]]) -> Feature {
    const MAX: usize = 50_000;
    let cos = 15f32.to_radians().cos();
    let mut ids: HashMap<(i64, i64, i64), u32> = HashMap::new();
    let mut pts: Vec<Vec3> = vec![];
    let mut weld = |p: Vec3| -> u32 {
        let k = (
            (p.x * 1e4).round() as i64,
            (p.y * 1e4).round() as i64,
            (p.z * 1e4).round() as i64,
        );
        *ids.entry(k).or_insert_with(|| {
            pts.push(p);
            (pts.len() - 1) as u32
        })
    };
    let mut edges: HashMap<(u32, u32), (Vec3, u32, bool)> = HashMap::new();
    let mut order: Vec<(u32, u32)> = vec![];
    for t in tris {
        let [a, b, c] = t.map(&mut weld);
        if a == b || b == c || c == a {
            continue;
        }
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        if n == Vec3::ZERO {
            continue;
        }
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let k = (u.min(v), u.max(v));
            match edges.get_mut(&k) {
                Some(e) => {
                    e.1 += 1;
                    if n.dot(e.0) < cos {
                        e.2 = true;
                    }
                }
                None => {
                    edges.insert(k, (n, 1, false));
                    order.push(k);
                }
            }
        }
    }
    let mut out = Feature::default();
    let mut used = HashSet::new();
    for k in order {
        let (_, n, sharp) = edges[&k];
        if n == 1 || sharp {
            out.edges.push([pts[k.0 as usize], pts[k.1 as usize]]);
            for i in [k.0, k.1] {
                if used.insert(i) {
                    out.verts.push(pts[i as usize]);
                }
            }
            if out.edges.len() >= MAX {
                break;
            }
        }
    }
    out
}

pub(crate) fn ids(m: &ddc_model::Model, sql: &str, level: i64) -> Vec<i64> {
    m.sql()
        .prepare(sql)
        .and_then(|mut st| st.query_map([level], |r| r.get(0))?.collect())
        .unwrap_or_default()
}

/// Model mesh (absolute elevations) to scene vertices (from the active level's elevation).
pub fn push_mesh(pos: &mut Vec<Vec3>, tris: &mut Vec<[u32; 3]>, m: &ddc_geom::mesh::Mesh, z0: f64) {
    let base = pos.len() as u32;
    pos.extend(
        m.positions
            .iter()
            .map(|p| Vec3::new(p.x as f32, p.y as f32, (p.z - z0) as f32)),
    );
    tris.extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
}

/// A prism over an outline (holes cut out) from `zb` with height `h` (scene).
pub fn prism(rings: &[Vec<(f64, f64)>], zb: f64, h: f64) -> Option<ddc_geom::mesh::Mesh> {
    use ddc_geom::{v2, Profile, Solid, Tol};
    let tol = Tol::default();
    let ring = |r: &Vec<(f64, f64)>| r.iter().map(|&(x, y)| v2(x, y)).collect::<Vec<_>>();
    let (outer, holes) = rings.split_first()?;
    // drawing only: a spike saved before they were refused must not hide the element
    let p = Profile::new_keeping_spikes(ring(outer), holes.iter().map(ring).collect(), tol).ok()?;
    Solid::extrude(&p, h.max(1e-3), tol)
        .translated_z(zb)
        .triangulate()
        .ok()
}

/// A bar a→b with section w×h; `roll` is the section's "up" direction (for a vertical bar, a rotation).
pub fn bar(a: Vec3, b: Vec3, w: f32, h: f32, up: Vec3) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let d = (b - a).normalize_or(Vec3::Z);
    let side = d.cross(up).normalize_or(Vec3::X);
    let upv = side.cross(d).normalize_or(Vec3::Z);
    let (sw, sh) = (side * (w * 0.5), upv * (h * 0.5));
    let corners = |c: Vec3| [c - sw - sh, c + sw - sh, c + sw + sh, c - sw + sh];
    let mut pos = corners(a).to_vec();
    pos.extend(corners(b));
    let mut tris = vec![[0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7]];
    for i in 0..4u32 {
        let j = (i + 1) % 4;
        tris.push([i, j, j + 4]);
        tris.push([i, j + 4, i + 4]);
    }
    (pos, tris)
}

#[allow(unreachable_patterns)]
pub(crate) fn shape_wh(s: &ddc_model::MemberShape) -> (f64, f64) {
    use ddc_model::MemberShape as S;
    match s {
        S::Rect { b, h } => (*b, *h),
        S::Circle { d } => (*d, *d),
        S::I { h, b, .. } => (*b, *h),
        _ => (0.3, 0.3),
    }
}

pub(crate) fn elev(m: &ddc_model::Model, level: i64) -> f64 {
    m.sql()
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn cube(t: Target, o: Vec3, s: f32) -> Body {
        let (pos, tris) = bar(o, o + Vec3::Z * s, s, s, Vec3::X);
        Body::own(t, pos, tris, u8::from(t.cat == Cat::Room))
    }

    fn grid_body(id: i64, line: &[(f64, f64)]) -> (Target, Body) {
        let code = crate::editor::structx::code_of("grid").unwrap();
        let t = Target::new(Cat::Ext(code), id);
        let mut pos = vec![];
        let mut tris = vec![];
        for w in line.windows(2) {
            let (a, b) = (Vec3::new(w[0].0 as f32, w[0].1 as f32, 0.03), Vec3::new(w[1].0 as f32, w[1].1 as f32, 0.03));
            let (p, t3) = bar(a, b, 0.06, 0.04, Vec3::Z);
            let o = pos.len() as u32;
            pos.extend(p);
            tris.extend(t3.into_iter().map(|t| [t[0] + o, t[1] + o, t[2] + o]));
        }
        (t, Body::own(t, pos, tris, 0))
    }

    #[test]
    fn pick3d_arc_grid() {
        let line = crate::elem::datum::grid_line((0.0, 0.0), (10.0, 0.0), 0.5);
        assert!(line.len() > 2, "an arc is sampled");
        let (t, body) = grid_body(7, &line);
        let mut b = Bodies::from_list(vec![body]);
        b.add_grid_lines(&[(7, line.clone())]);
        let mut oc = ddc_render::OrbitCamera::default();
        oc.target = Vec3::new(5.0, line[line.len() / 2].1 as f32, 0.0);
        let cam = Cam::new(&oc, Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)));
        let s = Scene::default();
        let m = line[line.len() / 2];
        let on = cam.to_screen(Vec3::new(m.0 as f32, m.1 as f32, 0.03)).unwrap();
        let near = b.hits_px(&s, &cam, on + egui::vec2(0.0, 3.0), &ALL, GRID_PX);
        assert_eq!(near.first().map(|h| h.target), Some(t));
        assert_eq!(near.len(), 1, "one hit per grid");
        let far = b.hits_px(&s, &cam, on + egui::vec2(0.0, 60.0), &ALL, GRID_PX);
        assert!(far.is_empty());
    }

    #[test]
    fn pick3d_straight_grid_tolerance() {
        let line = [(0.0, 0.0), (10.0, 0.0)];
        let (t, body) = grid_body(4, &line);
        let mut b = Bodies::from_list(vec![body]);
        b.add_grid_lines(&[(4, line.to_vec())]);
        let mut oc = ddc_render::OrbitCamera::default();
        oc.target = Vec3::new(5.0, 0.0, 0.0);
        let cam = Cam::new(&oc, Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)));
        let s = Scene::default();
        let on = cam.to_screen(Vec3::new(5.0, 0.0, 0.03)).unwrap();
        let inside = b.hits_px(&s, &cam, on + egui::vec2(0.0, GRID_PX - 2.0), &ALL, GRID_PX);
        assert_eq!(inside.first().map(|h| h.target), Some(t));
        let outside = b.hits_px(&s, &cam, on + egui::vec2(0.0, GRID_PX + 14.0), &ALL, GRID_PX);
        assert!(outside.is_empty(), "beyond the tolerance nothing is hit");
        assert!(is_grid(t));
        assert!(!is_grid(Target::new(Cat::Column, 4)));
    }

    fn down(x: f32, y: f32) -> Ray {
        Ray {
            origin: Vec3::new(x, y, 50.0),
            dir: Vec3::NEG_Z,
        }
    }

    #[test]
    fn hits_sorted_nearest_first_rooms_last() {
        let col = Target::new(Cat::Column, 1);
        let room = Target::new(Cat::Room, 2);
        let slab = Target::new(Cat::Slab, 3);
        let b = Bodies::from_list(vec![
            cube(room, Vec3::ZERO, 3.0),
            cube(slab, Vec3::new(0.0, 0.0, -0.2), 0.2),
            cube(col, Vec3::new(0.0, 0.0, 0.0), 0.4),
        ]);
        let s = Scene::default();
        let h = b.hits(&s, &down(0.0, 0.0));
        let order: Vec<Cat> = h.iter().map(|h| h.target.cat).collect();
        assert_eq!(order, vec![Cat::Column, Cat::Slab, Cat::Room]);
        assert!((h[0].point.z - 0.4).abs() < 1e-4 && h[0].normal.z > 0.9);
        assert!(b.hits(&s, &down(10.0, 10.0)).is_empty());
    }

    #[test]
    fn occluder_hides_what_is_behind() {
        let col = Target::new(Cat::Column, 1);
        let mut b = Bodies::from_list(vec![cube(col, Vec3::ZERO, 0.4)]);
        let mut occ = cube(col, Vec3::new(0.0, 0.0, 2.0), 1.0);
        occ.target = None;
        b.list.push(occ);
        assert!(b.hits(&Scene::default(), &down(0.0, 0.0)).is_empty());
    }

    #[test]
    fn feature_edges_of_box_skip_diagonals() {
        let (pos, tris) = bar(Vec3::ZERO, Vec3::Z, 1.0, 1.0, Vec3::X);
        let tris: Vec<[Vec3; 3]> = tris.iter().map(|t| t.map(|i| pos[i as usize])).collect();
        let f = feature_edges(&tris);
        assert_eq!(f.edges.len(), 12, "a box has 12 edges");
        assert_eq!(f.verts.len(), 8);
    }

    #[test]
    fn window_and_crossing_box() {
        let a = Target::new(Cat::Column, 1);
        let b = Bodies::from_list(vec![cube(a, Vec3::ZERO, 1.0)]);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(0.0, 0.0, 0.5);
        let cam = Cam::new(
            &c,
            Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
        );
        let s = Scene::default();
        let all = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        assert_eq!(b.in_box(&s, &cam, all, false), vec![a]);
        let mid = cam.to_screen(Vec3::new(0.0, 0.0, 0.5)).unwrap();
        let small = Rect::from_center_size(mid, egui::vec2(4.0, 4.0));
        assert!(
            b.in_box(&s, &cam, small, false).is_empty(),
            "window needs all inside"
        );
        assert_eq!(b.in_box(&s, &cam, small, true), vec![a], "crossing touches");
    }

    #[test]
    fn section_box_clips_hits_and_box_select() {
        let near = Target::new(Cat::Column, 1);
        let far = Target::new(Cat::Slab, 2);
        let b = Bodies::from_list(vec![
            cube(near, Vec3::new(0.0, 0.0, 5.0), 1.0),
            cube(far, Vec3::ZERO, 1.0),
        ]);
        let s = Scene::default();
        assert_eq!(b.hits(&s, &down(0.0, 0.0))[0].target, near);
        // the section clips the upper column: under the cursor is the slab inside the box
        let clip = Aabb {
            min: Vec3::splat(-2.0),
            max: Vec3::splat(2.0),
        };
        let q = Query {
            index: None,
            clip: Some(clip),
            full: false,
        };
        let h = b.hits_in(&s, &down(0.0, 0.0), &q);
        assert_eq!(h.iter().map(|h| h.target).collect::<Vec<_>>(), vec![far]);
        let cam = Cam::from_matrix(
            ddc_render::glam::Mat4::orthographic_rh(-20.0, 20.0, -20.0, 20.0, 0.1, 100.0)
                * ddc_render::glam::Mat4::look_at_rh(
                    Vec3::new(0.0, 0.0, 50.0),
                    Vec3::ZERO,
                    Vec3::Y,
                ),
            Vec3::new(0.0, 0.0, 50.0),
            Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 800.0)),
        );
        let all = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 800.0));
        assert_eq!(b.in_box_in(&s, &cam, all, true, &ALL).len(), 2);
        assert_eq!(b.in_box_in(&s, &cam, all, true, &q), vec![far]);
    }

    #[test]
    fn crossing_box_skips_rooms_and_feature_falls_back_for_huge_bodies() {
        let room = Target::new(Cat::Room, 1);
        let b = Bodies::from_list(vec![cube(room, Vec3::ZERO, 1.0)]);
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::new(0.0, 0.0, 0.5);
        let cam = Cam::new(
            &c,
            Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
        );
        let all = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let s = Scene::default();
        assert!(
            b.in_box(&s, &cam, all, true).is_empty(),
            "crossing does not grab rooms"
        );
        assert_eq!(b.in_box(&s, &cam, all, false), vec![room]);
        let f = box_edges(&Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        });
        assert_eq!((f.edges.len(), f.verts.len()), (12, 8));
    }

    #[test]
    fn a_full_query_finds_bodies_of_a_big_scene_without_an_index() {
        use ddc_render::SceneMesh;
        let n = crate::spatial::BIG_SCENE;
        let tri = |i: usize| SceneMesh {
            element: crate::scene_ids::EXT_BASE + i as u32,
            tag: 0,
            positions: vec![
                [i as f32, -1.0, 0.0],
                [i as f32 + 1.0, -1.0, 0.0],
                [i as f32, 1.0, 0.0],
            ],
            triangles: vec![[0, 1, 2]],
        };
        let s = Scene::new((0..n).map(tri).collect());
        let ts: Vec<Target> = (0..n as i64).map(|i| Target::new(Cat::Column, i)).collect();
        let ed = Editor::new_in_memory().unwrap();
        let b = Bodies::build_with(&ed, &s, &ts);
        let r = down(5.2, 0.0);
        assert!(
            b.hits_in(&s, &r, &ALL).is_empty(),
            "the frame path does not scan a big scene"
        );
        let full = Query { full: true, ..ALL };
        let hits = b.hits_in(&s, &r, &full);
        assert_eq!(
            hits.first().map(|h| h.target),
            Some(Target::new(Cat::Column, 5))
        );
    }

    #[test]
    fn hits_through_index_match_brute_force() {
        use ddc_render::SceneMesh;
        let mesh = |e: u32, o: Vec3| {
            let (pos, tris) = bar(o, o + Vec3::Z, 1.0, 1.0, Vec3::X);
            SceneMesh {
                element: e,
                tag: 0,
                positions: pos.iter().map(|p| p.to_array()).collect(),
                triangles: tris,
            }
        };
        let base = crate::scene_ids::EXT_BASE;
        let s = Scene::new(
            (0..50)
                .map(|i| mesh(base + i, Vec3::new(i as f32 * 3.0, 0.0, 0.0)))
                .collect(),
        );
        let ts: Vec<Target> = (0..50).map(|i| Target::new(Cat::Column, i)).collect();
        let ed = Editor::new_in_memory().unwrap();
        let b = Bodies::build_with(&ed, &s, &ts);
        let ix = crate::spatial::Index::build(&s);
        let q = Query {
            index: Some(&ix),
            clip: None,
            full: false,
        };
        for x in [0.0, 9.0, 30.0, 200.0] {
            let r = down(x, 0.0);
            let a: Vec<_> = b.hits(&s, &r).iter().map(|h| (h.body, h.t)).collect();
            let c: Vec<_> = b
                .hits_in(&s, &r, &q)
                .iter()
                .map(|h| (h.body, h.t))
                .collect();
            assert_eq!(a, c, "x = {x}");
            assert_eq!(a.len(), usize::from(x < 150.0 && (x % 3.0) < 0.6));
        }
    }

    /// Ray from outside onto the first triangle of the element's body.
    fn ray_at_first_tri(b: &Bodies, scene: &Scene, t: Target) -> Ray {
        let body = &b.list[b.of(t).expect("body of the element")];
        let mut tri = None;
        body.for_tris(scene, |x| {
            tri.get_or_insert(x);
        });
        let [a, bb, c] = tri.expect("body has triangles");
        let n = (bb - a).cross(c - a).normalize();
        let ctr = (a + bb + c) / 3.0;
        Ray {
            origin: ctr + n * 0.5,
            dir: -n,
        }
    }

    #[test]
    fn pick3d_stair_hits_flight() {
        let fx = crate::geom_props::tests::fixture();
        let s = Scene::default();
        let b = Bodies::build(&fx.ed, &s);
        let t = Target::new(Cat::Stair, fx.ids["stair"]);
        let r = ray_at_first_tri(&b, &s, t);
        assert_eq!(b.hits(&s, &r).first().map(|h| h.target), Some(t));
        assert_eq!(t.grip_kind(), Some(crate::grips::Kind::Stair));
        assert_eq!(Target::from_grip_kind(crate::grips::Kind::Stair, t.id), Some(t));
    }

    #[test]
    fn pick3d_railing_hits_rail() {
        let fx = crate::geom_props::tests::fixture();
        let s = Scene::default();
        let b = Bodies::build(&fx.ed, &s);
        let t = Target::new(Cat::Railing, fx.ids["railing"]);
        let r = ray_at_first_tri(&b, &s, t);
        assert_eq!(b.hits(&s, &r).first().map(|h| h.target), Some(t));
        assert_eq!(t.grip_kind(), Some(crate::grips::Kind::Railing));
    }

    #[test]
    fn sel_round_trip() {
        for s in [
            Sel::Wall(3),
            Sel::Opening(4),
            Sel::Panel(5, 2),
            Sel::Slab(6),
            Sel::Room(7),
            Sel::Mep(8),
            Sel::Comp(9),
            Sel::Body(BodyKind::Column, 1),
            Sel::Body(BodyKind::Mesh, 2),
            Sel::Ext(crate::editor::structx::ROOF_CODE, 3),
        ] {
            assert_eq!(Target::from_sel(s).and_then(|t| t.sel()), Some(s));
        }
        let roof = Sel::Ext(crate::editor::structx::ROOF_CODE, 1);
        assert_eq!(Target::new(Cat::Roof, 1).sel(), Some(roof));
        assert_eq!(Target::from_sel(roof), Some(Target::new(Cat::Roof, 1)));
        assert_eq!(
            Target::new(Cat::Beam, 9).grip_kind(),
            Some(crate::grips::Kind::Beam)
        );
    }
}
