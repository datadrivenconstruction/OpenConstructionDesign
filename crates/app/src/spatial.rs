// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spatial index over element bounds (BVH, median split along the longest axis):
//! ray picking (exact triangles only in leaves along the ray path), box select, nearby elements
//! for snapping, visible plan area. A 50k-element scene builds in milliseconds;
//! after an edit the index is updated from the previous one (`Index::update`) rather than rebuilt.

use crate::editor::Sel;
use ddc_geom::V2;
use ddc_render::glam::{Mat4, Vec3};
use ddc_render::pick::{ray_aabb, ray_triangle};
use ddc_render::{Aabb, Hit, Instanced, Ray, Scene, SceneMesh};
use std::collections::HashMap;
use std::sync::Arc;

const LEAF: usize = 4;

#[derive(Clone, Copy, Debug)]
struct Node {
    b: Aabb,
    /// Leaf: elements `order[first..first + count]`; node: children `first`, `first + 1`.
    first: u32,
    count: u32,
}

/// Tree of boxes; elements are indices into the input list.
#[derive(Clone, Debug, Default)]
pub struct Bvh {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

/// Ray vs box; an empty box (removed elements, empty leaf) is never hit.
fn ray_box(r: &Ray, b: &Aabb) -> Option<f32> {
    if b.is_empty() {
        None
    } else {
        ray_aabb(r, b)
    }
}

impl Bvh {
    pub fn build(boxes: &[Aabb]) -> Self {
        Self::build_from(boxes, (0..boxes.len() as u32).collect())
    }

    /// Tree over a subset of elements `order` (indices into `boxes`).
    pub fn build_from(boxes: &[Aabb], mut order: Vec<u32>) -> Self {
        order.retain(|&i| !boxes[i as usize].is_empty());
        let mut nodes = Vec::with_capacity(2 * order.len() / LEAF + 1);
        if !order.is_empty() {
            nodes.push(Node {
                b: Aabb::EMPTY,
                first: 0,
                count: 0,
            });
            let n = order.len();
            split(boxes, &mut order, &mut nodes, 0, 0, n);
        }
        Self { nodes, order }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Recompute node boxes bottom-up with the same topology (children always follow their parent).
    fn refit(&mut self, boxes: &[Aabb]) {
        for i in (0..self.nodes.len()).rev() {
            let n = self.nodes[i];
            let mut b = Aabb::EMPTY;
            if n.count > 0 {
                for &k in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    b.union(&boxes[k as usize]);
                }
            } else {
                b.union(&self.nodes[n.first as usize].b);
                b.union(&self.nodes[n.first as usize + 1].b);
            }
            self.nodes[i].b = b;
        }
    }

    /// Elements whose boxes pass the `hit` test (applied to both nodes and leaves).
    pub fn query(&self, mut hit: impl FnMut(&Aabb) -> bool, boxes: &[Aabb], out: &mut Vec<u32>) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0u32];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i as usize];
            if n.b.is_empty() || !hit(&n.b) {
                continue;
            }
            if n.count > 0 {
                for &k in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    let b = &boxes[k as usize];
                    if !b.is_empty() && hit(b) {
                        out.push(k);
                    }
                }
            } else {
                stack.push(n.first);
                stack.push(n.first + 1);
            }
        }
    }

    /// Ray traversal starting from the nearest nodes; `leaf` receives the element and the entry into its box and
    /// returns the current best t (nodes beyond it are skipped).
    pub fn ray(&self, r: &Ray, boxes: &[Aabb], mut leaf: impl FnMut(u32, f32) -> f32) {
        if self.nodes.is_empty() {
            return;
        }
        let mut best = f32::INFINITY;
        let mut stack: Vec<(f32, u32)> = vec![];
        if let Some(t) = ray_box(r, &self.nodes[0].b) {
            stack.push((t, 0));
        }
        while let Some((t, i)) = stack.pop() {
            if t > best {
                continue;
            }
            let n = self.nodes[i as usize];
            if n.count > 0 {
                for &k in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    if let Some(tb) = ray_box(r, &boxes[k as usize]) {
                        if tb <= best {
                            best = best.min(leaf(k, tb));
                        }
                    }
                }
                continue;
            }
            let (a, b) = (n.first, n.first + 1);
            let ta = ray_box(r, &self.nodes[a as usize].b);
            let tb = ray_box(r, &self.nodes[b as usize].b);
            // nearer child is pushed last (popped first)
            match (ta, tb) {
                (Some(x), Some(y)) if x <= y => stack.extend([(y, b), (x, a)]),
                (Some(x), Some(y)) => stack.extend([(x, a), (y, b)]),
                (Some(x), None) => stack.push((x, a)),
                (None, Some(y)) => stack.push((y, b)),
                (None, None) => {}
            }
        }
    }
}

fn split(
    boxes: &[Aabb],
    order: &mut [u32],
    nodes: &mut Vec<Node>,
    at: usize,
    lo: usize,
    hi: usize,
) {
    let mut b = Aabb::EMPTY;
    let mut c = Aabb::EMPTY;
    for &k in &order[lo..hi] {
        let x = &boxes[k as usize];
        b.union(x);
        c.grow((x.min + x.max) * 0.5);
    }
    nodes[at].b = b;
    if hi - lo <= LEAF {
        nodes[at].first = lo as u32;
        nodes[at].count = (hi - lo) as u32;
        return;
    }
    let e = c.max - c.min;
    let axis = if e.x >= e.y && e.x >= e.z {
        0
    } else if e.y >= e.z {
        1
    } else {
        2
    };
    let mid = (lo + hi) / 2;
    let key = |k: &u32| {
        let x = &boxes[*k as usize];
        (x.min[axis] + x.max[axis]) * 0.5
    };
    order[lo..hi].select_nth_unstable_by(mid - lo, |a, b| key(a).total_cmp(&key(b)));
    let left = nodes.len();
    let empty = Node {
        b: Aabb::EMPTY,
        first: 0,
        count: 0,
    };
    nodes.extend([empty, empty]);
    nodes[at].first = left as u32;
    nodes[at].count = 0;
    split(boxes, order, nodes, left, lo, mid);
    split(boxes, order, nodes, left + 1, mid, hi);
}

/// Scene index element: a mesh or a placement of an instance group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Mesh(u32),
    Inst(u32, u32),
}

/// Identity of an index element across scenes: a mesh — the address of its `Arc`; a placement — the addresses
/// of the placement list and the group prototype plus the position in the list.
type Ident = (usize, usize, u32);

/// 3D scene index: meshes and instances.
#[derive(Clone, Debug, Default)]
pub struct Index {
    bvh: Bvh,
    /// Elements added after the full build go into a small tree (rebuilt as a whole).
    extra: Bvh,
    items: Vec<Item>,
    /// One per element plus an extra empty one at the end: elements removed from `bvh` leaves point to it.
    boxes: Vec<Aabb>,
    elements: Vec<u32>,
    ids: Vec<Ident>,
    /// Scene meshes and groups stay alive as long as the index does: their addresses cannot be reused by new ones.
    #[allow(dead_code)]
    keep: (Vec<Arc<SceneMesh>>, Vec<Instanced>),
    shape: (usize, usize),
}

fn shape(s: &Scene) -> (usize, usize) {
    (
        s.meshes.len(),
        s.instanced.iter().map(|g| g.inst.len()).sum(),
    )
}

type Parts = (Vec<Item>, Vec<Aabb>, Vec<u32>, Vec<Ident>);

fn parts(s: &Scene) -> Parts {
    let (nm, ni) = shape(s);
    let (mut items, mut boxes) = (Vec::with_capacity(nm + ni), Vec::with_capacity(nm + ni + 1));
    let (mut elements, mut ids) = (Vec::with_capacity(nm + ni), Vec::with_capacity(nm + ni));
    for (i, b) in s.boxes().iter().enumerate() {
        items.push(Item::Mesh(i as u32));
        boxes.push(*b);
        elements.push(s.meshes[i].element);
        ids.push((Arc::as_ptr(&s.meshes[i]) as usize, 0, u32::MAX));
    }
    for (g, grp) in s.instanced.iter().enumerate() {
        let (gi, gp) = (
            Arc::as_ptr(&grp.inst) as usize,
            Arc::as_ptr(&grp.proto) as usize,
        );
        for (k, i) in grp.inst.iter().enumerate() {
            items.push(Item::Inst(g as u32, k as u32));
            boxes.push(grp.world_bounds(i));
            elements.push(i.element);
            ids.push((gi, gp, k as u32));
        }
    }
    (items, boxes, elements, ids)
}

impl Index {
    pub fn build(s: &Scene) -> Self {
        let (items, mut boxes, elements, ids) = parts(s);
        let bvh = Bvh::build(&boxes);
        boxes.push(Aabb::EMPTY);
        Self {
            bvh,
            extra: Bvh::default(),
            items,
            boxes,
            elements,
            ids,
            keep: (s.meshes.clone(), s.instanced.clone()),
            shape: shape(s),
        }
    }

    /// Index of a new scene derived from the previous one: surviving meshes and placements (the same `Arc`s) keep
    /// their place in the tree, node boxes are refit bottom-up; removed ones are taken out of the leaves,
    /// new ones go into the small `extra` tree. If more than an eighth changed, do a full rebuild.
    pub fn update(prev: Option<&Index>, s: &Scene) -> Self {
        let Some(p) = prev else {
            return Self::build(s);
        };
        let (items, mut boxes, elements, ids) = parts(s);
        let n = items.len();
        let dead = n as u32;
        let old: HashMap<Ident, u32> = p
            .ids
            .iter()
            .enumerate()
            .map(|(k, id)| (*id, k as u32))
            .collect();
        // old index → new; the last one is the empty box of the previous index
        let mut remap = vec![dead; p.items.len() + 1];
        let mut added = vec![];
        for (k, id) in ids.iter().enumerate() {
            match old.get(id) {
                Some(&o) if remap[o as usize] == dead => remap[o as usize] = k as u32,
                _ => added.push(k as u32),
            }
        }
        let mut bvh = p.bvh.clone();
        let mut gone = 0;
        for x in bvh.order.iter_mut() {
            *x = remap[*x as usize];
            gone += (*x == dead) as usize;
        }
        let mut extra: Vec<u32> = p
            .extra
            .order
            .iter()
            .map(|&o| remap[o as usize])
            .filter(|&k| k != dead)
            .collect();
        extra.extend(added);
        if gone + extra.len() > (n / 8).max(256) {
            return Self::build(s);
        }
        boxes.push(Aabb::EMPTY);
        bvh.refit(&boxes);
        Self {
            bvh,
            extra: Bvh::build_from(&boxes, extra),
            items,
            boxes,
            elements,
            ids,
            keep: (s.meshes.clone(), s.instanced.clone()),
            shape: shape(s),
        }
    }

    /// The index was built for this scene: the same meshes and placements (`Arc`s) in the same order —
    /// hit mesh numbers match (matching shape alone is not enough: N hidden and N fills added).
    pub fn matches(&self, s: &Scene) -> bool {
        self.shape == shape(s)
            && self
                .keep
                .0
                .iter()
                .zip(&s.meshes)
                .all(|(a, b)| Arc::ptr_eq(a, b))
            && self
                .keep
                .1
                .iter()
                .zip(&s.instanced)
                .all(|(a, b)| Arc::ptr_eq(&a.inst, &b.inst) && Arc::ptr_eq(&a.proto, &b.proto))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item(&self, k: u32) -> (Item, u32, Aabb) {
        let k = k as usize;
        (self.items[k], self.elements[k], self.boxes[k])
    }

    /// Nearest ray hit — same as `Scene::pick`, but without scanning all boxes.
    pub fn pick(&self, s: &Scene, r: &Ray) -> Option<Hit> {
        self.pick_where(s, r, &|_| true)
    }

    /// Nearest hit accepted by `accept` (hidden categories, clipped by the box):
    /// a rejected hit does not occlude what lies behind it.
    pub fn pick_where(&self, s: &Scene, r: &Ray, accept: &dyn Fn(&Hit) -> bool) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let n = s.meshes.len();
        let mut leaf = |k: u32, _: f32| {
            match self.items[k as usize] {
                Item::Mesh(mi) => {
                    let m = &s.meshes[mi as usize];
                    for (ti, tri) in m.triangles.iter().enumerate() {
                        let [a, b, c] = tri.map(|i| Vec3::from(m.positions[i as usize]));
                        if let Some(t) = ray_triangle(r, a, b, c) {
                            if best.is_none_or(|h| t < h.t) {
                                let h = Hit {
                                    mesh: mi as usize,
                                    triangle: ti,
                                    element: m.element,
                                    t,
                                    point: r.at(t),
                                };
                                if accept(&h) {
                                    best = Some(h);
                                }
                            }
                        }
                    }
                }
                Item::Inst(g, i) => {
                    let grp = &s.instanced[g as usize];
                    let inst = &grp.inst[i as usize];
                    let inv = inst.m.inverse();
                    let lr = Ray {
                        origin: inv.transform_point3(r.origin),
                        dir: inv.transform_vector3(r.dir),
                    };
                    let m = &grp.proto;
                    for (ti, tri) in m.triangles.iter().enumerate() {
                        let [a, b, c] = tri.map(|i| Vec3::from(m.positions[i as usize]));
                        if let Some(t) = ray_triangle(&lr, a, b, c) {
                            if best.is_none_or(|h| t < h.t) {
                                let h = Hit {
                                    mesh: n + g as usize,
                                    triangle: ti,
                                    element: inst.element,
                                    t,
                                    point: r.at(t),
                                };
                                if accept(&h) {
                                    best = Some(h);
                                }
                            }
                        }
                    }
                }
            }
            best.map_or(f32::INFINITY, |h| h.t)
        };
        self.bvh.ray(r, &self.boxes, &mut leaf);
        self.extra.ray(r, &self.boxes, &mut leaf);
        best
    }

    /// Query over both trees.
    fn query(&self, hit: impl Fn(&Aabb) -> bool) -> Vec<u32> {
        let mut out = vec![];
        self.bvh.query(&hit, &self.boxes, &mut out);
        self.extra.query(&hit, &self.boxes, &mut out);
        out
    }

    /// Elements whose boxes intersect `b`.
    pub fn in_box(&self, b: &Aabb) -> Vec<u32> {
        self.query(|x: &Aabb| x.min.cmple(b.max).all() && x.max.cmpge(b.min).all())
    }

    /// Elements whose boxes the ray crosses (no triangle test): candidates for
    /// picking all hits along the ray.
    pub fn along(&self, r: &Ray) -> Vec<u32> {
        self.query(|x: &Aabb| !x.is_empty() && ray_aabb(r, x).is_some())
    }

    /// Elements within a sphere (for snapping to nearby endpoints).
    pub fn near(&self, p: Vec3, r: f32) -> Vec<u32> {
        self.query(|x: &Aabb| p.clamp(x.min, x.max).distance_squared(p) <= r * r)
    }

    /// Screen box (NDC `lo`–`hi`): window mode — box fully inside, otherwise — crossing.
    pub fn in_screen_rect(
        &self,
        vp: &Mat4,
        lo: (f32, f32),
        hi: (f32, f32),
        window: bool,
    ) -> Vec<u32> {
        let rect = |b: &Aabb| -> Option<(f32, f32, f32, f32)> {
            let mut r = (
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            );
            for k in 0..8 {
                let p = Vec3::new(
                    if k & 1 == 0 { b.min.x } else { b.max.x },
                    if k & 2 == 0 { b.min.y } else { b.max.y },
                    if k & 4 == 0 { b.min.z } else { b.max.z },
                );
                let c = *vp * p.extend(1.0);
                if c.w <= 1e-6 {
                    return None;
                }
                let (x, y) = (c.x / c.w, c.y / c.w);
                r = (r.0.min(x), r.1.min(y), r.2.max(x), r.3.max(y));
            }
            Some(r)
        };
        let touch = |b: &Aabb| {
            rect(b).is_none_or(|r| r.0 <= hi.0 && r.2 >= lo.0 && r.1 <= hi.1 && r.3 >= lo.1)
        };
        let mut out = self.query(touch);
        if window {
            out.retain(|&k| {
                rect(&self.boxes[k as usize])
                    .is_some_and(|r| r.0 >= lo.0 && r.2 <= hi.0 && r.1 >= lo.1 && r.3 <= hi.1)
            });
        }
        out
    }

    /// Elements visible in the frustum (for counters and prefetch).
    pub fn visible(&self, vp: &Mat4, px: (f32, f32)) -> Vec<u32> {
        self.query(|b: &Aabb| ddc_render::chunks::screen_size(vp, b, px).is_some())
    }

    /// Scene element numbers for the found positions.
    pub fn elements(&self, ks: &[u32]) -> Vec<u32> {
        let mut v: Vec<u32> = ks.iter().map(|&k| self.elements[k as usize]).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// From this many bodies up, the scene is not scanned on the frame thread: picking waits for the background index.
pub const BIG_SCENE: usize = 100_000;

pub fn is_big(s: &Scene) -> bool {
    s.meshes.len() + s.instanced.iter().map(|g| g.inst.len()).sum::<usize>() >= BIG_SCENE
}

/// Ray pick: via the index if it was built for this scene, otherwise by brute force (except for a large scene).
pub fn pick(index: Option<&Index>, s: &Scene, r: &Ray) -> Option<Hit> {
    match index {
        Some(ix) if ix.matches(s) => ix.pick(s, r),
        _ if is_big(s) => None,
        _ => s.pick(r),
    }
}

/// Large scene without a matching index: it is being built, hover waits, a click computes [`pick_exact`].
pub fn index_pending(index: Option<&Index>, s: &Scene) -> bool {
    is_big(s) && !index.is_some_and(|ix| ix.matches(s))
}

/// Like `pick`, with a hit filter; without a matching index — brute force with the same filter.
pub fn pick_where(
    index: Option<&Index>,
    s: &Scene,
    r: &Ray,
    accept: &dyn Fn(&Hit) -> bool,
) -> Option<Hit> {
    if let Some(ix) = index.filter(|ix| ix.matches(s)) {
        return ix.pick_where(s, r, accept);
    }
    if is_big(s) {
        return None;
    }
    pick_exact(s, r, accept)
}

/// Click: via the index, and without one by brute force even on a large scene. A one-off delay instead of a miss.
pub fn pick_click(
    index: Option<&Index>,
    s: &Scene,
    r: &Ray,
    accept: &dyn Fn(&Hit) -> bool,
) -> Option<Hit> {
    match index.filter(|ix| ix.matches(s)) {
        Some(ix) => ix.pick_where(s, r, accept),
        None => pick_exact(s, r, accept),
    }
}

/// Brute force over all bodies with a filter.
pub fn pick_exact(s: &Scene, r: &Ray, accept: &dyn Fn(&Hit) -> bool) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    let take = |h: Hit, best: &mut Option<Hit>| {
        if best.is_none_or(|b| h.t < b.t) && accept(&h) {
            *best = Some(h);
        }
    };
    for (mi, (m, bx)) in s.meshes.iter().zip(s.boxes()).enumerate() {
        if ray_aabb(r, bx).is_none_or(|t| best.is_some_and(|h| h.t < t)) {
            continue;
        }
        for (ti, tri) in m.triangles.iter().enumerate() {
            let [a, b, c] = tri.map(|i| Vec3::from(m.positions[i as usize]));
            if let Some(t) = ray_triangle(r, a, b, c) {
                let h = Hit {
                    mesh: mi,
                    triangle: ti,
                    element: m.element,
                    t,
                    point: r.at(t),
                };
                take(h, &mut best);
            }
        }
    }
    let n = s.meshes.len();
    for (g, grp) in s.instanced.iter().enumerate() {
        for inst in grp.inst.iter() {
            if ray_aabb(r, &grp.world_bounds(inst)).is_none_or(|t| best.is_some_and(|h| h.t < t)) {
                continue;
            }
            let inv = inst.m.inverse();
            let lr = Ray {
                origin: inv.transform_point3(r.origin),
                dir: inv.transform_vector3(r.dir),
            };
            for (ti, tri) in grp.proto.triangles.iter().enumerate() {
                let [a, b, c] = tri.map(|i| Vec3::from(grp.proto.positions[i as usize]));
                if let Some(t) = ray_triangle(&lr, a, b, c) {
                    let h = Hit {
                        mesh: n + g,
                        triangle: ti,
                        element: inst.element,
                        t,
                        point: r.at(t),
                    };
                    take(h, &mut best);
                }
            }
        }
    }
    best
}

/// Plan index: selections and their plan extents.
#[derive(Clone, Debug, Default)]
pub struct Plan2 {
    bvh: Bvh,
    sels: Vec<Sel>,
    boxes: Vec<Aabb>,
}

impl Plan2 {
    pub fn build(items: impl IntoIterator<Item = (Sel, Vec<V2>)>) -> Self {
        let mut sels = vec![];
        let mut boxes = vec![];
        for (s, pts) in items {
            let mut b = Aabb::EMPTY;
            for p in &pts {
                b.grow(Vec3::new(p.x as f32, p.y as f32, 0.0));
            }
            sels.push(s);
            boxes.push(b);
        }
        Self {
            bvh: Bvh::build(&boxes),
            sels,
            boxes,
        }
    }

    pub fn len(&self) -> usize {
        self.sels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sels.is_empty()
    }

    /// Selections whose extents touch rectangle `a`–`b` (with tolerance `pad`).
    pub fn rect(&self, a: V2, b: V2, pad: f64) -> Vec<Sel> {
        let lo = Vec3::new(
            (a.x.min(b.x) - pad) as f32,
            (a.y.min(b.y) - pad) as f32,
            -1.0,
        );
        let hi = Vec3::new(
            (a.x.max(b.x) + pad) as f32,
            (a.y.max(b.y) + pad) as f32,
            1.0,
        );
        let hit = |x: &Aabb| x.min.cmple(hi).all() && x.max.cmpge(lo).all();
        let mut out = vec![];
        self.bvh.query(hit, &self.boxes, &mut out);
        out.into_iter().map(|k| self.sels[k as usize]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_render::SceneMesh;

    fn cube(e: u32, at: Vec3) -> SceneMesh {
        let positions = (0..8)
            .map(|i| {
                (at + Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32))
                    .to_array()
            })
            .collect();
        let q = |a, b, c, d| [[a, b, c], [a, c, d]];
        let triangles = [
            q(0, 2, 3, 1),
            q(4, 5, 7, 6),
            q(0, 1, 5, 4),
            q(2, 6, 7, 3),
            q(0, 4, 6, 2),
            q(1, 3, 7, 5),
        ]
        .concat();
        SceneMesh {
            element: e,
            tag: 0,
            positions,
            triangles,
        }
    }

    #[test]
    fn pick_and_queries_agree_with_brute_force() {
        let mut ms = vec![];
        for i in 0..40 {
            for j in 0..40 {
                ms.push(cube(
                    i * 40 + j,
                    Vec3::new(i as f32 * 2.0, j as f32 * 2.0, (i % 3) as f32),
                ));
            }
        }
        let s = Scene::new(ms);
        let ix = Index::build(&s);
        assert!(ix.matches(&s));
        for k in 0..200 {
            let (x, y) = (
                (k * 7 % 80) as f32 * 0.97 + 0.3,
                (k * 13 % 80) as f32 * 0.93 + 0.2,
            );
            let r = Ray {
                origin: Vec3::new(x, y - 30.0, 20.0),
                dir: Vec3::new(0.05, 1.0, -0.6).normalize(),
            };
            let (a, b) = (s.pick(&r), ix.pick(&s, &r));
            assert_eq!(a.map(|h| h.element), b.map(|h| h.element), "ray {k}");
        }
        let bx = Aabb {
            min: Vec3::new(3.5, 3.5, -1.0),
            max: Vec3::new(6.5, 4.5, 9.0),
        };
        let got = ix.elements(&ix.in_box(&bx));
        assert_eq!(got, vec![82, 122]);
        assert_eq!(
            ix.elements(&ix.near(Vec3::new(-0.5, 0.5, 0.5), 0.6)),
            vec![0]
        );
        let p = Plan2::build([
            (
                Sel::Wall(1),
                vec![ddc_geom::v2(0.0, 0.0), ddc_geom::v2(5.0, 0.0)],
            ),
            (
                Sel::Wall(2),
                vec![ddc_geom::v2(20.0, 0.0), ddc_geom::v2(25.0, 1.0)],
            ),
        ]);
        assert_eq!(
            p.rect(ddc_geom::v2(4.0, -1.0), ddc_geom::v2(6.0, 1.0), 0.0),
            vec![Sel::Wall(1)]
        );
    }

    #[test]
    fn matches_by_identity_and_filtered_pick_sees_through() {
        let ms: Vec<Arc<SceneMesh>> = (0..4)
            .map(|i| Arc::new(cube(i, Vec3::new(0.0, i as f32 * 3.0, 0.0))))
            .collect();
        let s = Scene::shared(ms.clone());
        let ix = Index::build(&s);
        // same shape, different meshes: the index does not match
        let mut other = ms.clone();
        other[1] = Arc::new(cube(1, Vec3::new(0.0, 3.0, 0.0)));
        let s2 = Scene::shared(other);
        assert!(ix.matches(&s) && !ix.matches(&s2));
        let r = Ray {
            origin: Vec3::new(0.5, -10.0, 0.5),
            dir: Vec3::Y,
        };
        assert_eq!(ix.pick(&s, &r).map(|h| h.element), Some(0));
        // the nearer one is rejected — the hit lands on the next one, same with index and brute force
        let skip0 = |h: &Hit| h.element != 0;
        assert_eq!(ix.pick_where(&s, &r, &skip0).map(|h| h.element), Some(1));
        assert_eq!(pick_where(None, &s, &r, &skip0).map(|h| h.element), Some(1));
        assert_eq!(
            pick_where(Some(&ix), &s2, &r, &skip0).map(|h| h.element),
            Some(1)
        );
    }

    #[test]
    fn update_keeps_shared_meshes_and_agrees_with_rebuild() {
        let ms: Vec<Arc<SceneMesh>> = (0..30)
            .map(|i| Arc::new(cube(i, Vec3::new(i as f32 * 2.0, 0.0, 0.0))))
            .collect();
        let ix1 = Index::build(&Scene::shared(ms.clone()));
        let mut ms2 = ms.clone();
        ms2[3] = Arc::new(cube(3, Vec3::new(6.0, 5.0, 0.0)));
        ms2.remove(10);
        ms2.push(Arc::new(cube(99, Vec3::new(70.0, 0.0, 0.0))));
        let s2 = Scene::shared(ms2);
        let ix2 = Index::update(Some(&ix1), &s2);
        assert!(ix2.matches(&s2));
        assert_eq!(ix2.extra.len(), 2, "moved and added go to the small tree");
        let full = Index::build(&s2);
        let all = Aabb {
            min: Vec3::splat(-100.0),
            max: Vec3::splat(100.0),
        };
        assert_eq!(
            ix2.elements(&ix2.in_box(&all)),
            full.elements(&full.in_box(&all))
        );
        for x in [0.5f32, 6.5, 20.5, 70.5] {
            let r = Ray {
                origin: Vec3::new(x, -10.0, 0.5),
                dir: Vec3::Y,
            };
            let (a, b) = (ix2.pick(&s2, &r), full.pick(&s2, &r));
            assert_eq!(a.map(|h| h.element), b.map(|h| h.element), "x {x}");
        }
        let ix3 = Index::update(Some(&ix2), &s2);
        assert_eq!(ix3.extra.len(), 2, "same scene: nothing new");
    }
}

#[cfg(test)]
mod big_scene_tests {
    use super::*;
    use ddc_render::glam::{Mat4, Vec3};
    use ddc_render::{Inst, Instanced};

    fn scene(n: usize) -> Scene {
        let proto = Arc::new(SceneMesh {
            element: 0,
            tag: 0,
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            triangles: vec![[0, 1, 2]],
        });
        let inst = (0..n)
            .map(|k| Inst {
                m: Mat4::from_translation(Vec3::new(
                    (k % 1000) as f32 * 2.0,
                    (k / 1000) as f32 * 2.0,
                    0.0,
                )),
                element: k as u32 + 1,
            })
            .collect();
        Scene::shared(vec![]).with_instanced(vec![Instanced::new(1, proto, inst)])
    }

    fn down() -> Ray {
        Ray {
            origin: Vec3::new(0.2, 0.2, 5.0),
            dir: Vec3::NEG_Z,
        }
    }

    #[test]
    fn without_a_matching_index_a_big_scene_is_not_scanned() {
        let big = scene(BIG_SCENE);
        assert!(is_big(&big));
        assert!(pick(None, &big, &down()).is_none());
        assert!(pick_where(None, &big, &down(), &|_| true).is_none());
        let small = scene(10);
        assert!(!is_big(&small));
        assert_eq!(pick(None, &small, &down()).map(|h| h.element), Some(1));
    }

    #[test]
    fn a_click_on_a_big_scene_finds_the_element_while_the_index_is_building() {
        let big = scene(BIG_SCENE);
        assert!(index_pending(None, &big));
        let hit = pick_click(None, &big, &down(), &|_| true);
        assert_eq!(hit.map(|h| h.element), Some(1));
        assert_eq!(
            pick_exact(&big, &down(), &|_| false).map(|h| h.element),
            None
        );
        let ix = Index::build(&big);
        assert!(!index_pending(Some(&ix), &big));
        assert!(
            index_pending(Some(&ix), &scene(BIG_SCENE)),
            "an index of another scene"
        );
        assert_eq!(
            pick_click(Some(&ix), &big, &down(), &|_| true).map(|h| h.element),
            Some(1)
        );
        assert!(
            !index_pending(None, &scene(10)),
            "a small scene is scanned anyway"
        );
    }

    #[test]
    fn the_index_picks_in_a_big_scene() {
        let big = scene(BIG_SCENE);
        let ix = Index::build(&big);
        assert_eq!(pick(Some(&ix), &big, &down()).map(|h| h.element), Some(1));
    }
}
