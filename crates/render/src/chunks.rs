// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scene in chunks for a large model: meshes are split into 16 m cells (opaque and
//! translucent separately), each chunk has a content fingerprint; after an edit only chunks
//! with a different fingerprint are re-uploaded to the GPU. Every frame: frustum culling and
//! level of detail by screen size (edges only on large chunks, none below a pixel).
//! Repeated bodies are instanced: one mesh, matrices and the element number in the instance buffer.
//! Large model: background builds, frame budgets, proxy boxes, eviction from GPU memory
//! (`perf`, `chunk_build`; goals and measurement — `docs/design/PERF_3D.md`).

#[cfg(feature = "gpu")]
use crate::chunk_build::{
    build_meshes, build_proto, item_element, Built, Done, Job, MeshItem, ProtoJob, Workers,
};
#[cfg(feature = "gpu")]
use crate::perf::{
    self, BoxTree, Detail, FrameStats, GpuClass, HiZ, Motion, PerfConfig, Residency, Spend, TreeEv,
};
use crate::scene::{is_translucent, Aabb, Scene, SceneMesh, Vertex, TAG_EDGE};
#[cfg(feature = "gpu")]
use crate::vpack::{pack_lines, pack_tris, Deq, Stream, StreamGpu, GPU_VERTEX_BYTES};
use glam::{Mat4, Vec3, Vec4};
use std::collections::HashMap;
#[cfg(feature = "gpu")]
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

/// Chunk cell side, m.
pub const CELL: f32 = 16.0;
/// At most this many triangles per chunk (a large cell is split).
pub const CHUNK_TRIS: usize = 400_000;
/// A chunk smaller than this many pixels is not drawn; edges — only on chunks larger than `EDGE_PX`.
pub const LOD_PX: f32 = 1.5;
pub const EDGE_PX: f32 = 24.0;

/// Placement of a repeated body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inst {
    pub m: Mat4,
    pub element: u32,
}

/// One mesh (in its own coordinates) and its placements; `key` — a stable key on the caller's side.
#[derive(Clone, Debug)]
pub struct Instanced {
    pub key: u64,
    pub proto: Arc<SceneMesh>,
    pub inst: Arc<Vec<Inst>>,
    pub local: Aabb,
    /// Coarse mesh levels from exact to coarse: the mesh and its error, m.
    pub lods: Vec<(Arc<SceneMesh>, f32)>,
    /// `proto` — not the exact mesh but an approximation with this error, m (the exact one is not read yet).
    pub approx: f32,
    /// Plants: per level (exact first) the least size in pixels of the mesh's bounding box at
    /// which the level is drawn, falling; the last is 0. Empty: the level follows the error.
    pub screen: Vec<f32>,
    /// Own render source (a linked project): never baked into cell chunks shared with other
    /// groups, so an edit elsewhere never rebuilds its vertices.
    pub own: bool,
    /// Bounds cache; public only so `Instanced { inst, ..g }` works in other crates.
    #[doc(hidden)]
    pub placed: PlacedCache,
}

/// Bounds of all placements: union (`all`) and of those at least 1 m tall (`tall`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedBounds {
    pub all: Aabb,
    pub tall: Aabb,
}

#[derive(Debug)]
struct PlacedEntry {
    // a Weak keeps the allocation alive, so its address cannot be reused by another list
    inst: std::sync::Weak<Vec<Inst>>,
    len: usize,
    local: Aabb,
    bounds: PlacedBounds,
}

/// Cache of placed bounds keyed by the inst list identity, its length and the local box.
#[derive(Debug, Default)]
pub struct PlacedCache {
    entry: std::sync::Mutex<Option<PlacedEntry>>,
    walks: std::sync::atomic::AtomicUsize,
}

impl Clone for PlacedCache {
    fn clone(&self) -> Self {
        let e = self.entry.lock().ok();
        let entry = e.and_then(|e| {
            e.as_ref().map(|e| PlacedEntry {
                inst: e.inst.clone(),
                len: e.len,
                local: e.local,
                bounds: e.bounds,
            })
        });
        Self {
            entry: std::sync::Mutex::new(entry),
            walks: std::sync::atomic::AtomicUsize::new(
                self.walks.load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

impl Instanced {
    pub fn new(key: u64, proto: Arc<SceneMesh>, inst: Vec<Inst>) -> Self {
        let mut local = Aabb::EMPTY;
        proto
            .positions
            .iter()
            .for_each(|p| local.grow(Vec3::from(*p)));
        Self {
            key,
            proto,
            inst: Arc::new(inst),
            local,
            lods: vec![],
            approx: 0.0,
            screen: vec![],
            own: false,
            placed: PlacedCache::default(),
        }
    }
    /// Marks the group as an own render source ([`Instanced::own`]).
    pub fn with_own(mut self) -> Self {
        self.own = true;
        self
    }
    pub fn with_approx(mut self, err: f32) -> Self {
        self.approx = err;
        self
    }
    pub fn with_lods(mut self, lods: Vec<(Arc<SceneMesh>, f32)>) -> Self {
        self.lods = lods;
        self
    }
    /// Levels by the size on screen ([`Instanced::screen`]), also in a small scene.
    pub fn with_screen(mut self, px: Vec<f32>) -> Self {
        self.screen = px;
        self
    }
    /// The same placements, already shared: the group is not recomputed when neighbours are edited.
    pub fn shared(key: u64, proto: Arc<SceneMesh>, inst: Arc<Vec<Inst>>) -> Self {
        let mut g = Self::new(key, proto, vec![]);
        g.inst = inst;
        g
    }
    pub fn world_bounds(&self, i: &Inst) -> Aabb {
        transform_box(&self.local, &i.m)
    }
    /// Bounds of every placement; walks the list once per distinct inst list, then reuses it.
    pub fn placed_bounds(&self) -> PlacedBounds {
        let want = Arc::downgrade(&self.inst);
        let mut slot = self.placed.entry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = slot.as_ref() {
            if e.inst.ptr_eq(&want) && e.len == self.inst.len() && e.local == self.local {
                return e.bounds;
            }
        }
        let mut b = PlacedBounds {
            all: Aabb::EMPTY,
            tall: Aabb::EMPTY,
        };
        for i in self.inst.iter() {
            let bx = self.world_bounds(i);
            b.all.union(&bx);
            if bx.max.z - bx.min.z >= 1.0 {
                b.tall.union(&bx);
            }
        }
        self.placed
            .walks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *slot = Some(PlacedEntry {
            inst: want,
            len: self.inst.len(),
            local: self.local,
            bounds: b,
        });
        b
    }
    /// Full walks done so far by this group (and the clones it came from).
    #[cfg(test)]
    pub(crate) fn placed_walks(&self) -> usize {
        self.placed.walks.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub fn transform_box(b: &Aabb, m: &Mat4) -> Aabb {
    let mut out = Aabb::EMPTY;
    if b.is_empty() {
        return out;
    }
    for k in 0..8 {
        let p = Vec3::new(
            if k & 1 == 0 { b.min.x } else { b.max.x },
            if k & 2 == 0 { b.min.y } else { b.max.y },
            if k & 4 == 0 { b.min.z } else { b.max.z },
        );
        out.grow(m.transform_point3(p));
    }
    out
}

#[cfg(feature = "gpu")]
/// Element number recovered from an instance buffer row.
pub(crate) fn row_element(r: &[f32; 16]) -> u32 {
    ((r[3] as u32).saturating_sub(1) & 0xFFFF) | ((r[7] as u32) << 16)
}

/// Instance buffer row: matrix columns, the element number in the w of the first two columns
/// (low 16 bits + 1, high) — the shader zeroes them before multiplying.
pub fn inst_row(i: &Inst) -> [f32; 16] {
    let mut c = i.m.to_cols_array();
    c[3] = (i.element & 0xFFFF) as f32 + 1.0;
    c[7] = (i.element >> 16) as f32;
    c[11] = 0.0;
    c
}

/// Word fingerprint (4 independent FNV-1a lanes — fast on large meshes).
#[derive(Clone, Copy)]
struct Fp([u64; 4]);

impl Fp {
    fn new() -> Self {
        Fp([0xcbf2_9ce4_8422_2325; 4])
    }
    fn words(&mut self, w: &[u32]) {
        const P: u64 = 0x0100_0000_01b3;
        let mut it = w.chunks_exact(4);
        for c in &mut it {
            for k in 0..4 {
                self.0[k] = (self.0[k] ^ c[k] as u64).wrapping_mul(P);
            }
        }
        for (k, &x) in it.remainder().iter().enumerate() {
            self.0[k] = (self.0[k] ^ x as u64).wrapping_mul(P);
        }
    }
    fn one(&mut self, x: u64) {
        self.words(&[x as u32, (x >> 32) as u32]);
    }
    fn finish(&self) -> u64 {
        self.0.iter().fold(0x9e37_79b9_7f4a_7c15u64, |h, &x| {
            (h ^ x).wrapping_mul(0x0100_0000_01b3).rotate_left(23)
        })
    }
}

pub(crate) fn mesh_fp(m: &SceneMesh) -> u64 {
    let mut f = Fp::new();
    f.words(&[m.element, m.tag, m.positions.len() as u32]);
    f.words(bytemuck::cast_slice(&m.positions));
    f.words(bytemuck::cast_slice(&m.triangles));
    f.finish()
}

fn cell_of(b: &Aabb) -> (i32, i32, i32) {
    cell_at(b, CELL)
}

fn cell_at(b: &Aabb, side: f32) -> (i32, i32, i32) {
    let c = (b.min + b.max) * 0.5;
    let q = |x: f32| (x / side).floor() as i32;
    (q(c.x), q(c.y), q(c.z))
}

/// A scene chunk (no GPU): key, fingerprint, meshes, bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub key: u64,
    pub stamp: u64,
    pub glass: bool,
    pub meshes: Vec<usize>,
    pub bounds: Aabb,
    pub triangles: usize,
}

/// Layout of scene meshes into chunks; chunks are ordered by key.
pub fn plan(scene: &Scene) -> Vec<Planned> {
    plan_with(scene, &mut |m: &SceneMesh| mesh_fp(m))
}

#[cfg(feature = "gpu")]
/// Mesh fingerprints across syncs: a mesh is recognised by its `Arc` (the cache holds it, the address
/// is not reused), the fingerprint is computed only for new ones.
#[derive(Default)]
struct FpCache(HashMap<usize, (Arc<SceneMesh>, u64)>);

#[cfg(feature = "gpu")]
impl FpCache {
    /// Fingerprints of scene meshes and instance prototypes; the cache keeps only these.
    fn refresh(&mut self, scene: &Scene) -> HashMap<usize, u64> {
        let mut next = HashMap::with_capacity(scene.meshes.len() + scene.instanced.len());
        let all = scene
            .meshes
            .iter()
            .chain(scene.instanced.iter().map(|g| &g.proto));
        for m in all {
            let k = Arc::as_ptr(m) as usize;
            let fp = match self.0.get(&k) {
                Some((_, fp)) => *fp,
                None => mesh_fp(m),
            };
            next.insert(k, (m.clone(), fp));
        }
        self.0 = next;
        self.0.iter().map(|(k, v)| (*k, v.1)).collect()
    }
}

/// Like `plan`, the mesh fingerprint comes from `fp`. Within a cell meshes go by element number (not by
/// list position): a new element with a new number does not shift its neighbours among cell parts.
pub fn plan_with(scene: &Scene, fp: &mut dyn FnMut(&SceneMesh) -> u64) -> Vec<Planned> {
    let mut cells: HashMap<((i32, i32, i32), bool), Vec<usize>> = HashMap::new();
    let boxes = scene.boxes();
    for (i, m) in scene.meshes.iter().enumerate() {
        if m.triangles.is_empty() || boxes[i].is_empty() {
            continue;
        }
        cells
            .entry((cell_of(&boxes[i]), is_translucent(m.tag)))
            .or_default()
            .push(i);
    }
    let mut out = vec![];
    for ((c, glass), mut ids) in cells {
        ids.sort_by_key(|&i| scene.meshes[i].element);
        let mut part = 0u32;
        let mut cur: Vec<usize> = vec![];
        let mut tris = 0;
        let mut flush = |cur: &mut Vec<usize>, part: u32, tris: usize, out: &mut Vec<Planned>| {
            if cur.is_empty() {
                return;
            }
            let mut f = Fp::new();
            f.words(&[c.0 as u32, c.1 as u32, c.2 as u32, glass as u32, part]);
            let key = f.finish();
            let mut st = Fp::new();
            let mut bounds = Aabb::EMPTY;
            for &i in cur.iter() {
                st.one(fp(scene.meshes[i].as_ref()));
                st.one(u64::from(scene.cat(i)));
                bounds.union(&boxes[i]);
            }
            out.push(Planned {
                key,
                stamp: st.finish(),
                glass,
                meshes: std::mem::take(cur),
                bounds,
                triangles: tris,
            });
        };
        for i in ids {
            let n = scene.meshes[i].triangles.len();
            if tris + n > CHUNK_TRIS && !cur.is_empty() {
                flush(&mut cur, part, tris, &mut out);
                part += 1;
                tris = 0;
            }
            cur.push(i);
            tris += n;
        }
        flush(&mut cur, part, tris, &mut out);
    }
    out.sort_by_key(|p| p.key);
    out
}

#[cfg(feature = "gpu")]
/// Category number into vertex tags (ground grid lines do not get it).
pub(crate) fn set_cat(v: &mut [Vertex], cat: u8) {
    if cat == 0 {
        return;
    }
    for x in v {
        x.tag = crate::renderer::with_cat(x.tag, cat);
    }
}

/// Share of the outward direction mixed into a foliage normal: the leaf mass reads as one round
/// crown instead of the facets of its lumps.
pub const FOLIAGE_BEND: f32 = 0.8;

/// Vertex normals of a foliage mesh: area-weighted smooth normals bent outward from the crown's
/// core, a point (round crown) or a segment (long hedge) inside its bounds.
pub fn foliage_normals(m: &SceneMesh) -> Vec<Vec3> {
    // corners are matched by place, not index: a triangle soup smooths as well as a shared mesh
    let key = |p: &[f32; 3]| p.map(|c| (c * 1.0e4).round() as i64);
    let mut acc: HashMap<[i64; 3], Vec3> = HashMap::new();
    let mut b = Aabb::EMPTY;
    m.positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
    for t in &m.triangles {
        let p = t.map(|i| Vec3::from(m.positions[i as usize]));
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        for i in t {
            *acc.entry(key(&m.positions[*i as usize])).or_default() += n;
        }
    }
    if b.is_empty() {
        return vec![Vec3::Z; m.positions.len()];
    }
    let half = (b.max - b.min) * 0.5;
    let r = half.min_element();
    let (lo, hi) = (b.min + Vec3::splat(r), b.max - Vec3::splat(r));
    m.positions
        .iter()
        .map(|q| {
            let n = acc.get(&key(q)).copied().unwrap_or(Vec3::ZERO);
            let p = Vec3::from(*q);
            let out = (p - p.clamp(lo, hi.max(lo))).normalize_or_zero();
            let s = n.normalize_or_zero();
            let s = if s == Vec3::ZERO { out } else { s };
            (s + out * FOLIAGE_BEND).normalize_or(s)
        })
        .collect()
}

/// Leaf cards of a near tree (`plant` cards): every two triangles are one quad, [a b c] [a c d]
/// for a leaf picture or [a b d] [b c d] for a needle spray, and no corner belongs to two quads.
/// Returns the corners a b c d of each quad (the picture's corners (0,0) (1,0) (1,1) (0,1)) and
/// whether it is a needle spray; `None` for any other mesh.
pub fn leaf_cards(m: &SceneMesh) -> Option<Vec<([u32; 4], bool)>> {
    let t = &m.triangles;
    if t.is_empty() || t.len() % 2 != 0 {
        return None;
    }
    let mut used = vec![false; m.positions.len()];
    let mut out = Vec::with_capacity(t.len() / 2);
    for p in t.chunks_exact(2) {
        let (x, y) = (p[0], p[1]);
        let q = if x[0] == y[0] && x[2] == y[1] {
            ([x[0], x[1], x[2], y[2]], false)
        } else if x[1] == y[0] && x[2] == y[2] {
            ([x[0], x[1], y[1], x[2]], true)
        } else {
            return None;
        };
        let c = q.0;
        if (0..4).any(|i| (i + 1..4).any(|j| c[i] == c[j])) {
            return None;
        }
        for i in c {
            let u = used.get_mut(i as usize)?;
            if *u {
                return None;
            }
            *u = true;
        }
        out.push(q);
    }
    Some(out)
}

/// Share of the card's own facing in its normal; the rest points away from the crown's core, so
/// the cards shade as one soft crown with a little life of their own.
pub const CARD_FACE: f32 = 0.35;

/// Vertices of leaf cards: per-corner picture coordinates, a tone jitter and a depth tone in
/// the tag bits (`scene::CARD`), normals bent outward from the crown's core.
fn card_vertices(m: &SceneMesh, cards: &[([u32; 4], bool)], out: &mut Vec<Vertex>) {
    use crate::scene::{CARD, CARD_AO_SHIFT, CARD_BITS, CARD_JITTER_SHIFT, CARD_NEEDLE, CARD_U, CARD_V};
    let mut b = Aabb::EMPTY;
    m.positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
    let (mid, half) = ((b.min + b.max) * 0.5, ((b.max - b.min) * 0.5).max(Vec3::splat(1e-3)));
    let r = half.min_element();
    let (lo, hi) = (b.min + Vec3::splat(r), (b.max - Vec3::splat(r)).max(b.min + Vec3::splat(r)));
    let outward = |p: Vec3| (p - p.clamp(lo, hi)).normalize_or(Vec3::Z);
    let base = (m.tag & !CARD_BITS) | CARD;
    for (q, needle) in cards {
        let p = q.map(|i| Vec3::from(m.positions[i as usize]));
        let c = (p[0] + p[1] + p[2] + p[3]) * 0.25;
        let mut face = (p[1] - p[0]).cross(p[3] - p[0]).normalize_or_zero();
        if face.dot(outward(c)) < 0.0 {
            face = -face;
        }
        let key = c.to_array().map(|x| (x * 64.0).round() as i32 as u32);
        let h = key[0].wrapping_mul(0x9E37_79B1) ^ key[1].wrapping_mul(0x85EB_CA6B) ^ key[2].wrapping_mul(0xC2B2_AE35);
        let h = (h ^ (h >> 15)).wrapping_mul(0x2C1B_3C6D);
        let jitter = (h >> 13) & 7;
        let depth = ((c - mid) / half).length();
        let ao = (((depth - 0.35) / 0.6 * 3.0).round().clamp(0.0, 3.0)) as u32;
        let tag = base | jitter << CARD_JITTER_SHIFT | ao << CARD_AO_SHIFT | if *needle { CARD_NEEDLE } else { 0 };
        let corner = [0, CARD_U, CARD_U | CARD_V, CARD_V];
        let n = p.map(|x| (outward(x) + face * CARD_FACE).normalize_or(face));
        let tri: [usize; 6] = if *needle { [0, 1, 3, 1, 2, 3] } else { [0, 1, 2, 0, 2, 3] };
        for k in tri {
            out.push(Vertex {
                pos: p[k].into(),
                normal: n[k].into(),
                element: m.element,
                tag: tag | corner[k],
            });
        }
    }
}

/// Flat triangle vertices (face normal), like `Scene::vertices`; foliage carries its smooth
/// vertex normals instead (`foliage_normals`), leaf cards their picture corners (`card_vertices`).
pub fn tri_vertices(m: &SceneMesh, out: &mut Vec<Vertex>) {
    if crate::scene::is_foliage(m.tag) {
        if let Some(cards) = leaf_cards(m) {
            return card_vertices(m, &cards, out);
        }
        let ns = foliage_normals(m);
        for t in &m.triangles {
            for i in t {
                out.push(Vertex {
                    pos: m.positions[*i as usize],
                    normal: ns[*i as usize].into(),
                    element: m.element,
                    tag: m.tag & !crate::scene::CARD,
                });
            }
        }
        return;
    }
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

/// Crease edges > 25° and the mesh boundary, like `Scene::edge_vertices`.
pub fn crease_vertices(m: &SceneMesh, out: &mut Vec<Vertex>) {
    type K = (i64, i64, i64);
    let key = |p: Vec3| {
        let q = (p * 1.0e4).round();
        (q.x as i64, q.y as i64, q.z as i64)
    };
    if crate::scene::is_foliage(m.tag) {
        return;
    }
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
    // map order is random per process: sorted, the line buffer (and so the blend order of
    // overlapping edge fragments) is the same on every run
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

/// Screen size of a box in pixels; None — outside the view frustum.
pub fn screen_size(vp: &Mat4, b: &Aabb, px: (f32, f32)) -> Option<f32> {
    let mut out = [true; 6];
    let (mut lo, mut hi) = (
        glam::Vec2::splat(f32::INFINITY),
        glam::Vec2::splat(f32::NEG_INFINITY),
    );
    let mut behind = false;
    for k in 0..8 {
        let p = Vec3::new(
            if k & 1 == 0 { b.min.x } else { b.max.x },
            if k & 2 == 0 { b.min.y } else { b.max.y },
            if k & 4 == 0 { b.min.z } else { b.max.z },
        );
        let c: Vec4 = *vp * p.extend(1.0);
        let tests = [
            c.x < -c.w,
            c.x > c.w,
            c.y < -c.w,
            c.y > c.w,
            c.z < 0.0,
            c.z > c.w,
        ];
        for (o, t) in out.iter_mut().zip(tests) {
            *o &= t;
        }
        if c.w <= 1e-6 {
            behind = true;
        } else {
            let n = glam::Vec2::new(c.x, c.y) / c.w;
            lo = lo.min(n);
            hi = hi.max(n);
        }
    }
    if out.iter().any(|&o| o) {
        return None;
    }
    if behind {
        return Some(f32::INFINITY);
    }
    let d = (hi - lo) * 0.5;
    Some((d.x * px.0).max(d.y * px.1))
}

#[cfg(feature = "gpu")]
const VERTEX_BYTES: u64 = GPU_VERTEX_BYTES;
#[cfg(feature = "gpu")]
const ROW_BYTES: u64 = 64;
#[cfg(feature = "gpu")]
/// Worst case of a compressed build: no shared vertices, u16 indices (glass — u32).
const TRI_BYTES: u64 = VERTEX_BYTES * 3 + 6;
#[cfg(feature = "gpu")]
/// Edges: at most a line end with an index per triangle.
const LINE_BYTES: u64 = VERTEX_BYTES + 4;
#[cfg(feature = "gpu")]
/// Target placements per chunk: the group's cell grows until there are fewer chunks.
const INST_PER_CHUNK: usize = 16;
#[cfg(feature = "gpu")]
const INST_CELL_MAX: f32 = CELL * 64.0;
#[cfg(feature = "gpu")]
/// A rare body is written as vertices into the chunk of its cell: a separate chunk and
/// draw call for each unique mesh would cost one call per body (hundreds of thousands in a real model).
const RARE_INST: usize = 8;
#[cfg(feature = "gpu")]
const RARE_TRIS: usize = 20_000;
#[cfg(feature = "gpu")]
const RARE_TOTAL: usize = 64_000;
#[cfg(feature = "gpu")]
/// A placement chunk left unbuilt for lack of memory that is wider than this is not replaced by a single box.
const DENIED_BOX_DIAG: f32 = CELL * 4.0;
#[cfg(feature = "gpu")]
const PROTO_BATCH: usize = 64;
#[cfg(feature = "gpu")]
const PROTO_BATCH_TRIS: usize = 200_000;

#[cfg(feature = "gpu")]
/// Box mesh (faces outward) in the coordinates of `b`.
pub(crate) fn box_mesh(b: &Aabb, element: u32, tag: u32) -> SceneMesh {
    let positions = (0..8)
        .map(|k| {
            [
                if k & 1 == 0 { b.min.x } else { b.max.x },
                if k & 2 == 0 { b.min.y } else { b.max.y },
                if k & 4 == 0 { b.min.z } else { b.max.z },
            ]
        })
        .collect();
    let q = |a: u32, b: u32, c: u32, d: u32| [[a, b, c], [a, c, d]];
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
        element,
        tag,
        positions,
        triangles,
    }
}

#[cfg(feature = "gpu")]
/// Placement row of a unit box onto `b`; w = 0: the element number comes from the vertex
/// (a chunk proxy is neither picked nor highlighted).
pub(crate) fn box_row(b: &Aabb) -> [f32; 16] {
    let e = b.max - b.min;
    [
        e.x, 0.0, 0.0, 0.0, //
        0.0, e.y, 0.0, 0.0, //
        0.0, 0.0, e.z, 0.0, //
        b.min.x, b.min.y, b.min.z, 1.0,
    ]
}

#[cfg(feature = "gpu")]
/// Estimate of build bytes with edges: an upper bound, so a task is not queued in vain. The real size
/// is known after the first build (`Chunk::known`).
fn mesh_est(tris: usize, glass: bool) -> u64 {
    let t = tris as u64;
    t * TRI_BYTES + t * LINE_BYTES + if glass { t * 6 } else { 0 } + ROW_BYTES
}

#[cfg(feature = "gpu")]
fn level_est(tris: usize, lines: bool) -> u64 {
    let base = tris as u64 * TRI_BYTES;
    if lines {
        base + tris as u64 * LINE_BYTES
    } else {
        base
    }
}

#[cfg(feature = "gpu")]
/// Placement rows of a chunk with its own meshes: shared scene matrices × its compression decode.
fn deq_rows(
    device: &wgpu::Device,
    globals: &[[f32; 16]],
    deq: Deq,
) -> Result<wgpu::Buffer, Refused> {
    let rows: Vec<[f32; 16]> = globals.iter().map(|r| deq.row(r)).collect();
    try_buffer(
        device,
        "ddc-chunk-rows",
        bytemuck::cast_slice(&rows),
        wgpu::BufferUsages::VERTEX,
    )
}

#[cfg(feature = "gpu")]
/// Whether a box can be shown instead of the chunk.
fn box_allowed(
    glass: bool,
    size: f32,
    max_px: f32,
    denied: bool,
    instanced: bool,
    diag: f32,
) -> bool {
    !glass && (size <= max_px || (denied && (!instanced || diag <= DENIED_BOX_DIAG)))
}

#[cfg(feature = "gpu")]
fn tagged(tag: u32, cat: u8) -> u32 {
    if cat == 0 {
        tag
    } else {
        crate::renderer::with_cat(tag, cat)
    }
}

#[cfg(feature = "gpu")]
/// Where a chunk takes its content from: if it is not on the GPU, it is rebuilt from this.
enum Src {
    Meshes(crate::chunk_build::MeshList),
    Inst {
        proto: u64,
        rows: Arc<Vec<[f32; 16]>>,
        centers: Arc<Vec<Vec3>>,
        group: u64,
    },
}

#[cfg(feature = "gpu")]
impl Src {
    fn any_element(&self, f: impl Fn(u32) -> bool) -> bool {
        match self {
            Src::Meshes(list) => list.iter().any(|m| f(item_element(m))),
            Src::Inst { rows, .. } => rows.iter().any(|r| f(row_element(r))),
        }
    }
}

#[cfg(feature = "gpu")]
struct MeshGpu {
    tri: StreamGpu,
    lines: StreamGpu,
    deq: Deq,
    /// Own placement rows (`deq_rows`) and the revision of the shared matrices they were built from.
    rows: wgpu::Buffer,
    rows_n: u32,
    rows_rev: u64,
    /// Bytes of the compressed mesh without rows.
    geom: u64,
    /// Glass: triangle centres and their indices (the order is written far to near).
    centers: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
}

#[cfg(feature = "gpu")]
impl MeshGpu {
    fn draw_tri(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_vertex_buffer(1, self.rows.slice(..));
        self.tri.draw(pass, 0..self.rows_n);
    }
}

#[cfg(feature = "gpu")]
enum Gpu {
    Meshes(MeshGpu),
    Rows(wgpu::Buffer, u32),
}

#[cfg(feature = "gpu")]
struct Chunk {
    key: u64,
    stamp: u64,
    glass: bool,
    bounds: Aabb,
    diag: f32,
    triangles: usize,
    imp_tag: u32,
    src: Src,
    /// Content on the GPU; the old fingerprint is drawn until the replacement arrives.
    gpu: Option<Gpu>,
    gpu_stamp: u64,
    /// Fingerprint of the in-flight build.
    job: Option<u64>,
    sorted_eye: Option<Vec3>,
    /// Evicted: returns only if visible again, not together with a neighbour's edit.
    evicted: bool,
    /// Real size of the previous build: a memory refusal does not repeat the build in vain.
    known: Option<u64>,
    /// Element numbers inside: [min, max + 1).
    elems: (u32, u32),
    /// Mixed categories: a box cannot show the view rules.
    mixed: bool,
}

#[cfg(feature = "gpu")]
fn mesh_chunk(
    (key, stamp, glass, bounds, triangles): (u64, u64, bool, Aabb, usize),
    items: Vec<MeshItem>,
    prev: Option<Chunk>,
) -> Chunk {
    let first = items.first().map_or(0, |m| m.1);
    let mixed = items.iter().any(|m| m.1 != first);
    let imp_tag = items
        .first()
        .map_or(0, |m| tagged(m.0.tag, if mixed { 0 } else { first }));
    let mut elems = (u32::MAX, 0u32);
    for m in &items {
        let e = item_element(m);
        elems.0 = elems.0.min(e);
        elems.1 = elems.1.max(e.saturating_add(1));
    }
    let mut c = Chunk {
        key,
        stamp,
        glass,
        bounds,
        diag: (bounds.max - bounds.min).length(),
        triangles,
        imp_tag,
        src: Src::Meshes(Arc::new(items)),
        gpu: None,
        gpu_stamp: 0,
        job: None,
        sorted_eye: None,
        evicted: false,
        known: None,
        elems,
        mixed,
    };
    if let Some(o) = prev {
        c.gpu = o.gpu;
        c.gpu_stamp = o.gpu_stamp;
        c.job = o.job;
        c.evicted = o.evicted;
    }
    c
}

#[cfg(feature = "gpu")]
struct ProtoLevel {
    tri: StreamGpu,
    lines: StreamGpu,
}

#[cfg(feature = "gpu")]
/// Shared mesh of placement groups: levels from exact to coarse, the last is a box.
struct Proto {
    levels: Vec<Arc<SceneMesh>>,
    /// Level errors, m (excluding the box).
    errors: Vec<f32>,
    /// Screen sizes of the levels ([`Instanced::screen`]); empty on error.
    screen: Vec<f32>,
    box_level: usize,
    diag: f32,
    cat: u8,
    /// Compression shared by all levels: the chunk's rows are the same for any level.
    deq: Deq,
    keys: Vec<u64>,
    gpu: Vec<Option<Arc<ProtoLevel>>>,
    job: Vec<bool>,
    evicted: Vec<bool>,
    known: Vec<Option<u64>>,
}

#[cfg(feature = "gpu")]
impl Proto {
    fn new(key: u64, g: &Instanced, cat: u8) -> Self {
        let mut levels = vec![g.proto.clone()];
        let mut errors = vec![g.approx.max(0.0)];
        for (m, e) in &g.lods {
            levels.push(m.clone());
            errors.push(*e);
        }
        let box_level = levels.len();
        levels.push(Arc::new(box_mesh(&g.local, g.proto.element, g.proto.tag)));
        let keys = (0..levels.len())
            .map(|l| {
                let mut f = Fp::new();
                f.one(key);
                f.one(l as u64 + 1);
                f.finish()
            })
            .collect();
        let diag = if g.local.is_empty() {
            0.0
        } else {
            (g.local.max - g.local.min).length()
        };
        let mut all = Aabb::EMPTY;
        levels
            .iter()
            .for_each(|m| all.union(&crate::scene::mesh_box(m)));
        Self {
            deq: Deq::of(&all),
            gpu: vec![None; levels.len()],
            job: vec![false; levels.len()],
            evicted: vec![false; levels.len()],
            known: vec![None; levels.len()],
            levels,
            errors,
            screen: g.screen.clone(),
            box_level,
            diag,
            cat,
            keys,
        }
    }

    /// The level on the GPU nearest to `want`: coarser first, then finer.
    fn nearest(&self, want: usize) -> Option<usize> {
        nearest_level(want, self.gpu.len(), |l| self.gpu[l].is_some())
    }

    /// The most exact level on the GPU, without the box (shadows).
    fn finest(&self) -> Option<&ProtoLevel> {
        self.gpu[..self.box_level]
            .iter()
            .flatten()
            .next()
            .map(|a| &**a)
    }
}

#[cfg(feature = "gpu")]
fn nearest_level(want: usize, n: usize, has: impl Fn(usize) -> bool) -> Option<usize> {
    (want..n).chain((0..want.min(n)).rev()).find(|&l| has(l))
}

#[cfg(feature = "gpu")]
/// A placement-group chunk; computed once per `Arc` pair of the group.
#[derive(Clone)]
struct Part {
    key: u64,
    stamp: u64,
    bounds: Aabb,
    triangles: usize,
    rows: Arc<Vec<[f32; 16]>>,
    centers: Arc<Vec<Vec3>>,
    elems: (u32, u32),
}

#[cfg(feature = "gpu")]
/// Placement of a rare body and its fingerprint.
#[derive(Clone)]
struct Baked {
    cell: (i32, i32, i32),
    glass: bool,
    inst: Inst,
    bounds: Aabb,
    fp: u64,
}

#[cfg(feature = "gpu")]
fn is_rare(g: &Instanced) -> bool {
    let tris = g.proto.triangles.len();
    g.approx <= 0.0
        && tris > 0
        && tris <= RARE_TRIS
        && g.inst.len() <= RARE_INST
        && g.inst.len() * tris <= RARE_TOTAL
}

#[cfg(feature = "gpu")]
/// Rare bodies are written into shared cell chunks, unless the group is an own source.
fn bakes(g: &Instanced) -> bool {
    is_rare(g) && !g.own
}

#[cfg(feature = "gpu")]
fn baked_of(g: &Instanced, proto_key: u64, glass: bool) -> Vec<Baked> {
    g.inst
        .iter()
        .filter_map(|i| {
            let bounds = g.world_bounds(i);
            if bounds.is_empty() {
                return None;
            }
            let mut f = Fp::new();
            f.one(proto_key);
            f.words(bytemuck::cast_slice(inst_row(i).as_slice()));
            Some(Baked {
                cell: cell_at(&bounds, CELL),
                glass,
                inst: *i,
                bounds,
                fp: f.finish(),
            })
        })
        .collect()
}

#[cfg(feature = "gpu")]
/// A chunk of rare bodies from one cell.
struct BakedPart {
    key: u64,
    stamp: u64,
    glass: bool,
    bounds: Aabb,
    triangles: usize,
    items: Vec<MeshItem>,
}

#[cfg(feature = "gpu")]
/// Rare bodies by cell; order within is by element number, as in `plan_with`.
fn bake_parts(
    cells: HashMap<((i32, i32, i32), bool), Vec<(Baked, Arc<SceneMesh>, u8)>>,
) -> Vec<BakedPart> {
    let mut out = vec![];
    for ((c, glass), mut items) in cells {
        items.sort_by_key(|x| (x.0.inst.element, x.0.fp));
        let mut part = 0u32;
        let mut tris = 0usize;
        let mut cur: Vec<(Baked, Arc<SceneMesh>, u8)> = vec![];
        let mut flush = |cur: &mut Vec<(Baked, Arc<SceneMesh>, u8)>, part: u32, tris: usize| {
            if cur.is_empty() {
                return;
            }
            let mut kf = Fp::new();
            kf.words(&[
                c.0 as u32,
                c.1 as u32,
                c.2 as u32,
                glass as u32,
                part,
                0xBA4E,
            ]);
            let mut sf = Fp::new();
            let mut bounds = Aabb::EMPTY;
            let items = cur
                .drain(..)
                .map(|(b, mesh, cat)| {
                    sf.one(b.fp);
                    bounds.union(&b.bounds);
                    (mesh, cat, Some(b.inst))
                })
                .collect();
            out.push(BakedPart {
                key: kf.finish(),
                stamp: sf.finish(),
                glass,
                bounds,
                triangles: tris,
                items,
            });
        };
        for it in items {
            let n = it.1.triangles.len();
            if tris + n > CHUNK_TRIS && !cur.is_empty() {
                flush(&mut cur, part, tris);
                part += 1;
                tris = 0;
            }
            tris += n;
            cur.push(it);
        }
        flush(&mut cur, part, tris);
    }
    out.sort_by_key(|p| p.key);
    out
}

#[cfg(feature = "gpu")]
struct GroupCache {
    inst: Arc<Vec<Inst>>,
    proto: Arc<SceneMesh>,
    cat: u8,
    proto_key: u64,
    parts: Vec<Part>,
    baked: Vec<Baked>,
}

#[cfg(feature = "gpu")]
fn cells_of(boxes: &[Aabb], side: f32) -> HashMap<(i32, i32, i32), Vec<u32>> {
    let mut cells: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
    for (k, b) in boxes.iter().enumerate() {
        if !b.is_empty() {
            cells.entry(cell_at(b, side)).or_default().push(k as u32);
        }
    }
    cells
}

#[cfg(feature = "gpu")]
/// From this many new placements the group layout runs on several threads.
const PAR_GROUP_INST: usize = 20_000;

#[cfg(feature = "gpu")]
/// Build worker threads: all cores except two (frame and I/O), at most 8.
pub(crate) fn worker_count() -> usize {
    workers_for(std::thread::available_parallelism().map_or(2, |n| n.get()))
}

#[cfg(feature = "gpu")]
pub(crate) fn workers_for(cores: usize) -> usize {
    cores.saturating_sub(2).clamp(1, 8)
}

#[cfg(feature = "gpu")]
/// `f` over `items` on `threads` threads; result order is the order of `items`.
fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = threads.min(items.len()).max(1);
    if threads == 1 {
        return items.iter().map(&f).collect();
    }
    let per = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        let hs: Vec<_> = items
            .chunks(per)
            .map(|c| s.spawn(|| c.iter().map(&f).collect::<Vec<R>>()))
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    })
}

#[cfg(feature = "gpu")]
fn group_cache(g: &Instanced, cat: u8, proto_fp: u64) -> GroupCache {
    let glass = is_translucent(g.proto.tag);
    let mut f = Fp::new();
    f.one(proto_fp);
    f.one(u64::from(cat));
    let proto_key = f.finish();
    let rare = bakes(g);
    GroupCache {
        inst: g.inst.clone(),
        proto: g.proto.clone(),
        cat,
        proto_key,
        parts: if rare {
            vec![]
        } else {
            group_parts(g, proto_key, glass)
        },
        baked: if rare {
            baked_of(g, proto_key, glass)
        } else {
            vec![]
        },
    }
}

#[cfg(feature = "gpu")]
/// Group placements by cell; rare bodies get a larger cell: hundreds of thousands of chunks with a couple of
/// placements each would cost more than the culling they enable saves.
fn group_parts(g: &Instanced, proto_key: u64, glass: bool) -> Vec<Part> {
    let boxes: Vec<Aabb> = g.inst.iter().map(|i| g.world_bounds(i)).collect();
    let target = boxes.len() / INST_PER_CHUNK + 1;
    let mut side = CELL;
    let mut cells = cells_of(&boxes, side);
    while cells.len() > target && side < INST_CELL_MAX {
        side *= 2.0;
        cells = cells_of(&boxes, side);
    }
    let mut cells: Vec<_> = cells.into_iter().collect();
    cells.sort_by_key(|c| c.0);
    let tris = g.proto.triangles.len();
    cells
        .into_iter()
        .map(|(c, ks)| {
            let mut kf = Fp::new();
            kf.one(g.key);
            kf.words(&[c.0 as u32, c.1 as u32, c.2 as u32, side as u32]);
            let rows: Vec<[f32; 16]> = ks.iter().map(|&k| inst_row(&g.inst[k as usize])).collect();
            let mut sf = Fp::new();
            sf.one(proto_key);
            sf.words(bytemuck::cast_slice(&rows));
            let mut bounds = Aabb::EMPTY;
            let mut centers = vec![];
            let mut elems = (u32::MAX, 0u32);
            for &k in &ks {
                let b = &boxes[k as usize];
                bounds.union(b);
                let e = g.inst[k as usize].element;
                elems.0 = elems.0.min(e);
                elems.1 = elems.1.max(e.saturating_add(1));
                if glass {
                    centers.push((b.min + b.max) * 0.5);
                }
            }
            Part {
                key: kf.finish(),
                stamp: sf.finish(),
                bounds,
                triangles: tris * ks.len(),
                rows: Arc::new(rows),
                centers: Arc::new(centers),
                elems,
            }
        })
        .collect()
}

#[cfg(feature = "gpu")]
/// What is selected for drawing in this frame.
struct Draw {
    chunk: u32,
    /// Placement mesh level; for a chunk with its own meshes — None.
    level: Option<Arc<ProtoLevel>>,
    edges: bool,
    prepass: bool,
    dist: f32,
}

#[cfg(feature = "gpu")]
enum Want {
    Chunk(u32),
    Level(u64, usize),
}

#[cfg(feature = "gpu")]
enum Ready {
    Meshes { key: u64, stamp: u64, built: Built },
    Level { key: u64, v: Stream, l: Stream },
}

#[cfg(feature = "gpu")]
impl Ready {
    fn bytes(&self) -> u64 {
        match self {
            Ready::Meshes { built, .. } => built.bytes() + ROW_BYTES,
            Ready::Level { v, l, .. } => v.bytes() + l.bytes(),
        }
    }
    fn key(&self) -> u64 {
        match self {
            Ready::Meshes { key, .. } | Ready::Level { key, .. } => *key,
        }
    }
}

#[cfg(feature = "gpu")]
/// Counters of the last sync and the last frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub chunks: usize,
    pub uploaded: usize,
    pub uploaded_bytes: usize,
    pub drawn: usize,
    pub culled: usize,
    pub lod_skipped: usize,
    pub triangles_drawn: usize,
}

#[cfg(feature = "gpu")]
pub struct Chunks {
    list: Vec<Chunk>,
    by_key: HashMap<u64, u32>,
    tree: BoxTree,
    protos: HashMap<u64, Proto>,
    /// Level key in memory accounting → (prototype, level).
    proto_res: HashMap<u64, (u64, usize)>,
    groups: HashMap<u64, GroupCache>,
    visible: Vec<Draw>,
    /// Proxy rows, consecutive by tag; (tag, start, end).
    impostors: Vec<[f32; 16]>,
    imp_runs: Vec<(u32, u32, u32)>,
    imp_buf: Option<(wgpu::Buffer, u64)>,
    imp_dropped: u64,
    imp_boxes: HashMap<u32, StreamGpu>,
    wanted: Vec<Want>,
    ready: VecDeque<Ready>,
    jobs: usize,
    workers: Option<Workers>,
    residency: Residency,
    motion: Motion,
    pub cfg: PerfConfig,
    generation: u64,
    epoch: u64,
    total_tris: u64,
    big: bool,
    /// What occluded has changed: the depth snapshot is stale. New chunks do not change it: they
    /// only add occluders, so the snapshot stays conservative.
    occluder_rev: u64,
    /// Did not fit in memory in the last frame.
    denied: HashSet<u64>,
    memory_limited: bool,
    /// Screen size of what the frame needs (build, upload).
    want_rank: HashMap<u64, f32>,
    /// Chunks with the selected and hovered: always exact, never culled.
    hot: HashSet<u32>,
    hot_key: Option<([u32; 4], u32)>,
    /// Category coverage is set by view rules.
    cat_rules: bool,
    grid: Option<(MeshGpu, Option<Aabb>)>,
    /// Shared scene matrices (`Renderer::set_instances`) and their revision.
    globals: Vec<[f32; 16]>,
    globals_rev: u64,
    /// The provoking vertex of a triangle is the last (GL): the flat normal is taken from it.
    provoke_last: bool,
    fps: FpCache,
    pub stats: Stats,
    frame: FrameStats,
}

#[cfg(feature = "gpu")]
impl Default for Chunks {
    fn default() -> Self {
        let cfg = PerfConfig::from_env();
        Self {
            list: vec![],
            by_key: HashMap::new(),
            tree: BoxTree::default(),
            protos: HashMap::new(),
            proto_res: HashMap::new(),
            groups: HashMap::new(),
            visible: vec![],
            impostors: vec![],
            imp_runs: vec![],
            imp_buf: None,
            imp_dropped: 0,
            imp_boxes: HashMap::new(),
            wanted: vec![],
            ready: VecDeque::new(),
            jobs: 0,
            workers: None,
            residency: Residency::new(cfg.gpu_budget),
            motion: Motion::default(),
            cfg,
            generation: 0,
            epoch: 0,
            total_tris: 0,
            big: false,
            occluder_rev: 0,
            denied: HashSet::new(),
            memory_limited: false,
            want_rank: HashMap::new(),
            hot: HashSet::new(),
            hot_key: None,
            cat_rules: false,
            grid: None,
            globals: vec![Mat4::IDENTITY.to_cols_array()],
            globals_rev: 0,
            provoke_last: false,
            fps: FpCache::default(),
            stats: Stats::default(),
            frame: FrameStats::default(),
        }
    }
}

#[cfg(feature = "gpu")]
/// Buffer refusal: too large for the adapter or a memory allocation error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    TooBig,
    OutOfMemory,
}

#[cfg(feature = "gpu")]
/// Like `create_buffer_init`, but without panicking on out-of-memory (there, mapping an unready buffer
/// is fatal): the error is checked before writing.
pub(crate) fn try_buffer(
    device: &wgpu::Device,
    label: &str,
    data: &[u8],
    usage: wgpu::BufferUsages,
) -> Result<wgpu::Buffer, Refused> {
    let pad = [0u8; 64];
    let contents: &[u8] = if data.is_empty() { &pad } else { data };
    let size = (contents.len() as u64 + 3) & !3;
    if size > device.limits().max_buffer_size {
        return Err(Refused::TooBig);
    }
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let b = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage,
        mapped_at_creation: true,
    });
    if pollster::block_on(device.pop_error_scope()).is_some() {
        return Err(Refused::OutOfMemory);
    }
    b.slice(..).get_mapped_range_mut()[..contents.len()].copy_from_slice(contents);
    b.unmap();
    Ok(b)
}

#[cfg(feature = "gpu")]
/// A compressed mesh on the GPU with its own placement rows.
fn mesh_gpu(
    device: &wgpu::Device,
    b: Built,
    glass: bool,
    globals: &[[f32; 16]],
    rev: u64,
) -> Result<MeshGpu, Refused> {
    let sort = if glass {
        wgpu::BufferUsages::COPY_DST
    } else {
        wgpu::BufferUsages::empty()
    };
    let tri = StreamGpu::new(device, "ddc-chunk", &b.tri, sort)?;
    let lines = StreamGpu::new(
        device,
        "ddc-chunk-lines",
        &b.lines,
        wgpu::BufferUsages::empty(),
    )?;
    let rows = deq_rows(device, globals, b.deq)?;
    let tris = if glass {
        (0..b.tri.count() / 3)
            .map(|t| [0, 1, 2].map(|k| b.tri.vertex_of(t * 3 + k)))
            .collect()
    } else {
        vec![]
    };
    Ok(MeshGpu {
        tri,
        lines,
        deq: b.deq,
        rows,
        rows_n: globals.len() as u32,
        rows_rev: rev,
        geom: b.bytes(),
        centers: b.centers,
        tris,
    })
}

#[cfg(feature = "gpu")]
fn upload_meshes(
    device: &wgpu::Device,
    c: &mut Chunk,
    b: Built,
    res: &mut Residency,
    globals: (&[[f32; 16]], u64),
) -> Result<u64, Refused> {
    let g = mesh_gpu(device, b, c.glass, globals.0, globals.1)?;
    let bytes = g.geom + g.rows_n as u64 * ROW_BYTES;
    c.gpu = Some(Gpu::Meshes(g));
    c.gpu_stamp = c.stamp;
    c.sorted_eye = None;
    c.evicted = false;
    c.known = Some(bytes);
    res.insert(c.key, bytes);
    Ok(bytes)
}

#[cfg(feature = "gpu")]
/// Placement rows of a chunk, already including the compression decode of prototype `deq`.
fn upload_rows(
    device: &wgpu::Device,
    c: &mut Chunk,
    res: &mut Residency,
    deq: Deq,
) -> Result<u64, Refused> {
    let Src::Inst { rows, .. } = &c.src else {
        return Ok(0);
    };
    let rows: Vec<[f32; 16]> = rows.iter().map(|r| deq.row(r)).collect();
    let usage = if c.glass {
        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST
    } else {
        wgpu::BufferUsages::VERTEX
    };
    let bytes = rows.len() as u64 * ROW_BYTES;
    let b = try_buffer(
        device,
        "ddc-inst",
        bytemuck::cast_slice(rows.as_slice()),
        usage,
    )?;
    let n = rows.len() as u32;
    c.gpu = Some(Gpu::Rows(b, n));
    c.gpu_stamp = c.stamp;
    c.sorted_eye = None;
    c.evicted = false;
    res.insert(c.key, bytes);
    Ok(bytes)
}

#[cfg(feature = "gpu")]
fn upload_level(
    device: &wgpu::Device,
    p: &mut Proto,
    level: usize,
    v: &Stream,
    l: &Stream,
    res: &mut Residency,
) -> Result<u64, Refused> {
    let none = wgpu::BufferUsages::empty();
    let bytes = v.bytes() + l.bytes();
    let tri = StreamGpu::new(device, "ddc-proto", v, none)?;
    let lines = StreamGpu::new(device, "ddc-proto-lines", l, none)?;
    p.gpu[level] = Some(Arc::new(ProtoLevel { tri, lines }));
    p.evicted[level] = false;
    p.known[level] = Some(bytes);
    res.insert(p.keys[level], bytes);
    Ok(bytes)
}

#[cfg(feature = "gpu")]
impl Chunks {
    pub fn vertex_count(&self) -> u32 {
        self.list
            .iter()
            .map(|c| c.triangles as u64 * 3)
            .sum::<u64>()
            .min(u64::from(u32::MAX)) as u32
    }

    /// Scene content revision (depth snapshot, shadows).
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Revision of what could occlude (depth snapshot key).
    pub fn occluder_rev(&self) -> u64 {
        self.occluder_rev
    }
    /// Upload counter: if it changed, something new appeared on the GPU.
    pub fn upload_epoch(&self) -> u64 {
        self.epoch
    }
    pub fn total_triangles(&self) -> u64 {
        self.total_tris
    }
    /// The model is above the threshold: budgets and simplifications apply while moving.
    pub fn is_big(&self) -> bool {
        self.big
    }
    pub fn moving(&self) -> bool {
        self.motion.moving()
    }
    /// Camera motion belongs to a view: several 3D views each keep their own.
    pub(crate) fn swap_motion(&mut self, m: &mut Motion) {
        std::mem::swap(&mut self.motion, m);
    }
    pub fn settled(&self) -> bool {
        self.motion.settled()
    }
    pub fn frame_stats(&self) -> FrameStats {
        self.frame
    }
    /// The next frame with the same camera will be idle: the depth snapshot is needed exactly from it.
    pub fn resting_next(&self) -> bool {
        self.motion.still_frames() + 1 >= perf::LADDER
    }
    /// The previous frame was already drawn idle, with full limits.
    pub fn rest_drawn(&self) -> bool {
        self.motion.still_frames() > perf::LADDER
    }
    /// Limits for the adapter.
    pub fn set_gpu_class(&mut self, class: GpuClass) {
        self.cfg = self.cfg.for_class(class);
        self.residency.set_budget(self.cfg.gpu_budget);
    }

    /// Shared scene matrices; chunk rows are rebuilt in `pump`.
    pub fn set_globals(&mut self, rows: Vec<[f32; 16]>) {
        self.globals = rows;
        self.globals_rev += 1;
    }

    /// Provoking vertex of a triangle for the adapter (GL — the last). Content built under another
    /// convention would show wrong face normals: it is dropped and rebuilt.
    pub fn set_provoke_last(&mut self, last: bool) {
        if self.provoke_last == last {
            return;
        }
        self.provoke_last = last;
        for c in &mut self.list {
            if matches!(c.gpu, Some(Gpu::Meshes(_))) {
                c.gpu = None;
                c.gpu_stamp = 0;
                c.evicted = false;
                c.known = None;
                self.residency.remove(c.key);
            }
        }
        for p in self.protos.values_mut() {
            for l in 0..p.gpu.len() {
                if p.gpu[l].take().is_some() {
                    self.residency.remove(p.keys[l]);
                }
                p.evicted[l] = false;
            }
        }
        self.imp_boxes.clear();
        self.generation += 1;
        self.occluder_rev += 1;
    }

    /// Rows of chunks with their own meshes after the shared matrices change.
    fn refresh_rows(&mut self, device: &wgpu::Device) {
        let (globals, rev) = (&self.globals, self.globals_rev);
        for c in &mut self.list {
            let Some(Gpu::Meshes(g)) = &mut c.gpu else {
                continue;
            };
            if g.rows_rev == rev {
                continue;
            }
            if let Ok(b) = deq_rows(device, globals, g.deq) {
                g.rows = b;
                g.rows_rev = rev;
                if g.rows_n != globals.len() as u32 {
                    g.rows_n = globals.len() as u32;
                    self.residency
                        .insert(c.key, g.geom + g.rows_n as u64 * ROW_BYTES);
                }
            }
        }
        if let Some((g, _)) = &mut self.grid {
            if g.rows_rev != rev {
                if let Ok(b) = deq_rows(device, globals, g.deq) {
                    g.rows = b;
                    g.rows_n = globals.len() as u32;
                    g.rows_rev = rev;
                }
            }
        }
    }
    /// Category coverage: under view rules the shared box of a mixed chunk would lie.
    pub fn set_cat_rules(&mut self, on: bool) {
        self.cat_rules = on;
    }
    /// Selection and hover ranges of the frame (`FrameUniforms::sel`). Chunks containing them are computed once
    /// per change: not a walk over all elements every frame.
    /// `set` (two or more merged ranges, `selset`) is the whole selection; then `sel.xy` only
    /// bounds it. `generation` tells a changed set apart from an equal-bounds one.
    pub fn set_hot(&mut self, sel: [u32; 4], set: &[(u32, u32)], generation: u32) {
        if self.hot_key == Some((sel, generation)) {
            return;
        }
        self.hot_key = Some((sel, generation));
        self.hot.clear();
        let multi = set.len() >= 2;
        let in_range = |a: u32, b: u32, e: u32| a < b && (a..b).contains(&e);
        let in_sel = |e: u32| {
            if multi {
                crate::selset::contains(set, e)
            } else {
                in_range(sel[0], sel[1], e)
            }
        };
        for (i, c) in self.list.iter().enumerate() {
            let (lo, hi) = c.elems;
            let near = |a: u32, b: u32| a < b && lo < b && a < hi;
            let near_sel = if multi {
                crate::selset::overlaps(set, lo, hi)
            } else {
                near(sel[0], sel[1])
            };
            if !near_sel && !near(sel[2], sel[3]) {
                continue;
            }
            if c.src
                .any_element(|e| in_sel(e) || in_range(sel[2], sel[3], e))
            {
                self.hot.insert(i as u32);
            }
        }
    }

    /// Bring chunks in line with the scene: those matching by key and fingerprint stay as they are. A little
    /// new content is built in place, a lot — in the background between frames (`pump`).
    pub fn sync(&mut self, device: &wgpu::Device, scene: &Scene) {
        let mut old: HashMap<u64, Chunk> = self.list.drain(..).map(|c| (c.key, c)).collect();
        let mut st = Stats::default();
        let mut changed = false;
        let fps = self.fps.refresh(scene);
        let fp_of = |m: &SceneMesh| {
            fps.get(&(m as *const SceneMesh as usize))
                .copied()
                .unwrap_or_else(|| mesh_fp(m))
        };
        let mut list = Vec::new();
        for p in plan_with(scene, &mut |m: &SceneMesh| fp_of(m)) {
            let prev = match old.remove(&p.key) {
                Some(c) if c.stamp == p.stamp => {
                    list.push(c);
                    continue;
                }
                prev => prev,
            };
            changed = true;
            let meshes: Vec<MeshItem> = p
                .meshes
                .iter()
                .map(|&i| (scene.meshes[i].clone(), scene.cat(i), None))
                .collect();
            let head = (p.key, p.stamp, p.glass, p.bounds, p.triangles);
            list.push(mesh_chunk(head, meshes, prev));
        }
        // instances: one mesh for all groups using it, placements by cell; a translucent
        // prototype goes to the glass pass, placements are sorted far to near
        let mut groups = HashMap::with_capacity(scene.instanced.len());
        let mut live: HashSet<u64> = HashSet::with_capacity(scene.instanced.len());
        let mut bake: HashMap<((i32, i32, i32), bool), Vec<(Baked, Arc<SceneMesh>, u8)>> =
            HashMap::new();
        let n_mesh = scene.meshes.len();
        let valid = |c: &GroupCache, g: &Instanced, cat: u8| {
            Arc::ptr_eq(&c.inst, &g.inst) && Arc::ptr_eq(&c.proto, &g.proto) && c.cat == cat
        };
        // new groups are laid out into cells in parallel: for a city this is hundreds of thousands of placements
        let todo: Vec<usize> = (0..scene.instanced.len())
            .filter(|&gi| {
                let g = &scene.instanced[gi];
                let cat = scene.cat(n_mesh + gi);
                self.groups.get(&g.key).is_none_or(|c| !valid(c, g, cat))
            })
            .collect();
        let todo_inst: usize = todo.iter().map(|&gi| scene.instanced[gi].inst.len()).sum();
        let threads = if todo_inst >= PAR_GROUP_INST {
            worker_count()
        } else {
            1
        };
        let made = par_map(&todo, threads, |&gi| {
            let g = &scene.instanced[gi];
            let cat = scene.cat(n_mesh + gi);
            group_cache(g, cat, fp_of(g.proto.as_ref()))
        });
        let mut fresh: Vec<Option<GroupCache>> = Vec::new();
        fresh.resize_with(scene.instanced.len(), || None);
        for (gi, c) in todo.into_iter().zip(made) {
            fresh[gi] = Some(c);
        }
        for (gi, g) in scene.instanced.iter().enumerate() {
            let glass = is_translucent(g.proto.tag);
            let cat = scene.cat(n_mesh + gi);
            let cached = self.groups.remove(&g.key).filter(|c| valid(c, g, cat));
            let group = cached
                .or_else(|| fresh[gi].take())
                .unwrap_or_else(|| group_cache(g, cat, fp_of(g.proto.as_ref())));
            if !group.baked.is_empty() {
                for b in &group.baked {
                    bake.entry((b.cell, b.glass)).or_default().push((
                        b.clone(),
                        g.proto.clone(),
                        cat,
                    ));
                }
                groups.insert(g.key, group);
                continue;
            }
            let proto_key = group.proto_key;
            live.insert(proto_key);
            if !self.protos.contains_key(&proto_key) {
                let p = Proto::new(proto_key, g, cat);
                for (l, k) in p.keys.iter().enumerate() {
                    self.proto_res.insert(*k, (proto_key, l));
                }
                self.protos.insert(proto_key, p);
            }
            let imp_tag = tagged(g.proto.tag, cat);
            for part in &group.parts {
                let prev = match old.remove(&part.key) {
                    Some(c) if c.stamp == part.stamp => {
                        list.push(c);
                        continue;
                    }
                    prev => prev,
                };
                changed = true;
                let mut c = Chunk {
                    key: part.key,
                    stamp: part.stamp,
                    glass,
                    bounds: part.bounds,
                    diag: (part.bounds.max - part.bounds.min).length(),
                    triangles: part.triangles,
                    imp_tag,
                    src: Src::Inst {
                        proto: proto_key,
                        rows: part.rows.clone(),
                        centers: part.centers.clone(),
                        group: g.key,
                    },
                    gpu: None,
                    gpu_stamp: 0,
                    job: None,
                    sorted_eye: None,
                    evicted: false,
                    known: None,
                    elems: part.elems,
                    mixed: false,
                };
                if let Some(o) = prev {
                    c.gpu = o.gpu;
                    c.gpu_stamp = o.gpu_stamp;
                    c.evicted = o.evicted;
                }
                list.push(c);
            }
            groups.insert(g.key, group);
        }
        self.groups = groups;
        for part in bake_parts(bake) {
            let prev = match old.remove(&part.key) {
                Some(c) if c.stamp == part.stamp => {
                    list.push(c);
                    continue;
                }
                prev => prev,
            };
            changed = true;
            let head = (
                part.key,
                part.stamp,
                part.glass,
                part.bounds,
                part.triangles,
            );
            list.push(mesh_chunk(head, part.items, prev));
        }
        for k in old.into_keys() {
            self.residency.remove(k);
            changed = true;
        }
        let residency = &mut self.residency;
        let proto_res = &mut self.proto_res;
        self.protos.retain(|k, p| {
            let keep = live.contains(k);
            if !keep {
                for key in &p.keys {
                    residency.remove(*key);
                    proto_res.remove(key);
                }
            }
            keep
        });
        self.list = list;
        self.by_key = self
            .list
            .iter()
            .enumerate()
            .map(|(i, c)| (c.key, i as u32))
            .collect();
        let boxes: Vec<Aabb> = self.list.iter().map(|c| c.bounds).collect();
        self.tree = BoxTree::build(&boxes);
        self.total_tris = self.list.iter().map(|c| c.triangles as u64).sum();
        self.big = self.total_tris > self.cfg.big_tris;
        self.visible.clear();
        self.wanted.clear();
        self.impostors.clear();
        self.imp_runs.clear();
        self.hot_key = None;
        if changed {
            self.generation += 1;
            self.occluder_rev += 1;
        }
        if self.missing_tris() <= self.cfg.sync_tris {
            self.build_missing(device, &mut st, false);
        }
        if self.grid.as_ref().is_none_or(|g| g.1 != scene.bounds) {
            let l = scene.grid_vertices();
            let deq = Deq::of(&crate::vpack::bounds(&l));
            let b = Built {
                deq,
                tri: pack_tris(&[], deq, false, false),
                lines: pack_lines(&l, deq),
                centers: vec![],
            };
            self.grid = mesh_gpu(device, b, false, &self.globals, self.globals_rev)
                .ok()
                .map(|g| (g, scene.bounds));
        }
        st.chunks = self.list.len();
        self.stats = st;
    }

    /// Unbuilt content in triangle terms (placement rows are half as expensive); content evicted
    /// beyond the frame does not count: visibility will bring it back, not a neighbour's edit.
    fn missing_tris(&self) -> u64 {
        let chunks: u64 = self
            .list
            .iter()
            .filter(|c| {
                (c.gpu.is_none() && !c.evicted) || (c.gpu.is_some() && c.gpu_stamp != c.stamp)
            })
            .map(|c| match &c.src {
                Src::Meshes(_) => c.triangles as u64,
                Src::Inst { rows, .. } => rows.len() as u64 / 2,
            })
            .sum();
        let protos: u64 = self
            .protos
            .values()
            .filter(|p| p.gpu[0].is_none() && !p.evicted[0])
            .map(|p| p.levels[0].triangles.len() as u64)
            .sum();
        chunks + protos
    }

    /// Build and upload unbuilt content right here (as much as fits in GPU memory); evicted content
    /// — only with `all`. What was visible in the previous frame is not evicted.
    fn build_missing(&mut self, device: &wgpu::Device, st: &mut Stats, all: bool) {
        for i in 0..self.list.len() {
            let c = &self.list[i];
            let fresh = c.gpu.is_some() && c.gpu_stamp == c.stamp;
            if fresh || (c.gpu.is_none() && c.evicted && !all) {
                continue;
            }
            let key = c.key;
            let est = match &c.src {
                Src::Meshes(_) => c.known.unwrap_or_else(|| mesh_est(c.triangles, c.glass)),
                Src::Inst { rows, .. } => rows.len() as u64 * ROW_BYTES,
            };
            match self.residency.reserve(est, 0.0) {
                Some(ev) => self.drop_evicted(&ev),
                None => {
                    self.denied.insert(key);
                    continue;
                }
            }
            let c = &mut self.list[i];
            let replaced = c.gpu.is_some();
            let res = match &c.src {
                Src::Meshes(list) => {
                    let list = list.clone();
                    let b = build_meshes(&list, c.glass, usize::MAX, self.provoke_last);
                    let globals = (self.globals.as_slice(), self.globals_rev);
                    upload_meshes(device, c, b, &mut self.residency, globals)
                }
                Src::Inst { proto, .. } => {
                    let deq = self.protos.get(proto).map_or(Deq::UNIT, |p| p.deq);
                    upload_rows(device, c, &mut self.residency, deq)
                }
            };
            match res {
                Ok(bytes) => {
                    st.uploaded += 1;
                    st.uploaded_bytes += bytes as usize;
                    self.epoch += 1;
                    if replaced {
                        self.occluder_rev += 1;
                    }
                }
                Err(why) => self.refused(key, why),
            }
        }
        let keys: Vec<u64> = self
            .protos
            .iter()
            .filter(|(_, p)| p.gpu[0].is_none() && (all || !p.evicted[0]))
            .map(|(k, _)| *k)
            .collect();
        for k in keys {
            let tris = self.protos[&k].levels[0].triangles.len();
            let est = self.protos[&k].known[0].unwrap_or_else(|| level_est(tris, true));
            let lvl_key = self.protos[&k].keys[0];
            match self.residency.reserve(est, 0.0) {
                Some(ev) => self.drop_evicted(&ev),
                None => {
                    self.denied.insert(lvl_key);
                    continue;
                }
            }
            let Some(p) = self.protos.get_mut(&k) else {
                continue;
            };
            let (v, l) = build_proto(&p.levels[0], p.cat, true, p.deq, self.provoke_last);
            match upload_level(device, p, 0, &v, &l, &mut self.residency) {
                Ok(bytes) => {
                    st.uploaded_bytes += bytes as usize;
                    self.epoch += 1;
                }
                Err(why) => self.refused(lvl_key, why),
            }
        }
    }

    /// Build everything unbuilt now (snapshot and benchmark without warm-up).
    /// A one-off frame (snapshot): everything is built and immediately idle.
    pub fn one_shot(&mut self, device: &wgpu::Device) {
        self.build_all(device);
        self.motion.assume_rest();
    }

    pub fn build_all(&mut self, device: &wgpu::Device) {
        let mut st = self.stats;
        self.build_missing(device, &mut st, true);
        self.stats = st;
    }

    fn refused(&mut self, key: u64, why: Refused) {
        self.denied.insert(key);
        if why == Refused::OutOfMemory {
            let b = perf::budget_after_oom(self.residency.used(), self.residency.budget());
            self.residency.set_budget(b);
            self.memory_limited = true;
        }
    }

    fn remember_size(&mut self, key: u64, bytes: u64) {
        if let Some(&i) = self.by_key.get(&key) {
            self.list[i as usize].known = Some(bytes);
        } else if let Some(&(pk, l)) = self.proto_res.get(&key) {
            if let Some(p) = self.protos.get_mut(&pk) {
                p.known[l] = Some(bytes);
            }
        }
    }

    fn drop_evicted(&mut self, keys: &[u64]) {
        for k in keys {
            if let Some(&i) = self.by_key.get(k) {
                let c = &mut self.list[i as usize];
                c.gpu = None;
                c.gpu_stamp = 0;
                c.evicted = true;
            } else if let Some(&(pk, l)) = self.proto_res.get(k) {
                if let Some(p) = self.protos.get_mut(&pk) {
                    p.gpu[l] = None;
                    p.evicted[l] = true;
                }
            }
        }
        if !keys.is_empty() {
            self.occluder_rev += 1;
        }
        self.frame.evicted += keys.len();
    }

    /// Frame selection: the box tree (frustum, screen size, depth snapshot),
    /// level of detail, then the budget from the largest on screen. When idle nothing is culled by
    /// size; what did not fit the adapter limits is counted and shown in the status bar.
    pub fn cull(&mut self, vp: Mat4, px: (u32, u32), eye: Vec3, hiz: Option<&HiZ>) {
        let pxf = (px.0 as f32, px.1 as f32);
        self.motion.update(&vp);
        self.residency.begin_frame();
        self.want_rank.clear();
        let cfg = self.cfg;
        let big = self.big;
        let moving = big && self.motion.moving();
        let lean = big && !self.motion.settled();
        let k = if moving { cfg.moving_scale } else { 1.0 };
        let min_px = if lean { cfg.cull_px * k } else { 0.0 };
        let (mut culled, mut lod, mut occluded) = (0usize, 0usize, 0usize);
        let mut cands: Vec<(f32, u32)> = vec![];
        let list = &self.list;
        let hot = &self.hot;
        self.tree.visit(|e| match e {
            TreeEv::Node(b, n) => match screen_size(&vp, b, pxf) {
                None => {
                    culled += n as usize;
                    false
                }
                Some(s) if s < min_px => {
                    lod += n as usize;
                    false
                }
                Some(_) => {
                    if hiz.is_some_and(|h| h.occluded(b, eye)) {
                        occluded += n as usize;
                        false
                    } else {
                        true
                    }
                }
            },
            TreeEv::Leaf(i) => {
                let c = &list[i as usize];
                match screen_size(&vp, &c.bounds, pxf) {
                    None => culled += 1,
                    Some(_) if !hot.is_empty() && hot.contains(&i) => {
                        cands.push((f32::INFINITY, i))
                    }
                    Some(_)
                        if lean
                            && cfg.draw_distance > 0.0
                            && eye.clamp(c.bounds.min, c.bounds.max).distance(eye)
                                > cfg.draw_distance =>
                    {
                        lod += 1
                    }
                    Some(s) if s < min_px => lod += 1,
                    Some(_) if hiz.is_some_and(|h| h.occluded(&c.bounds, eye)) => occluded += 1,
                    Some(s) => cands.push((s, i)),
                }
                true
            }
        });
        // the selection is not lost even where the tree culled a whole branch
        if !self.hot.is_empty() {
            let seen: HashSet<u32> = cands.iter().map(|x| x.1).collect();
            for &i in &self.hot {
                if !seen.contains(&i)
                    && screen_size(&vp, &self.list[i as usize].bounds, pxf).is_some()
                {
                    cands.push((f32::INFINITY, i));
                }
            }
        }
        cands.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let mut spend = if big {
            Spend::new(
                self.motion.budget(cfg.draws_moving, cfg.draws_still),
                self.motion.budget(cfg.tris_moving, cfg.tris_still),
            )
        } else {
            Spend::new(u64::MAX, u64::MAX)
        };
        let pre_limit = if moving {
            cfg.prepass_draws_moving
        } else {
            u64::MAX
        };
        let edges_ok = !big || self.motion.still_frames() >= 2;
        let mut visible = std::mem::take(&mut self.visible);
        visible.clear();
        let mut wanted = std::mem::take(&mut self.wanted);
        wanted.clear();
        let mut imps: Vec<(u32, [f32; 16])> = vec![];
        let mut asked: HashSet<(u64, usize)> = HashSet::new();
        let mut coarse: HashSet<u64> = HashSet::new();
        let (mut dropped, mut pre) = (0usize, 0u64);
        for &(size, i) in &cands {
            let c = &self.list[i as usize];
            let is_hot = !self.hot.is_empty() && self.hot.contains(&i);
            let dist = ((c.bounds.min + c.bounds.max) * 0.5).distance_squared(eye);
            let denied = self.denied.contains(&c.key);
            let instanced = matches!(c.src, Src::Inst { .. });
            let box_ok = box_allowed(
                c.glass,
                size,
                cfg.impostor_max_px,
                denied,
                instanced,
                c.diag,
            );
            let edges = edges_ok && size >= EDGE_PX;
            let mut impostor = false;
            match &c.src {
                Src::Meshes(_) => match perf::mesh_detail(size, c.glass, lean, moving, &cfg) {
                    Detail::Skip => lod += 1,
                    Detail::Box => impostor = true,
                    Detail::Level(_) => {
                        if c.gpu.is_none() {
                            wanted.push(Want::Chunk(i));
                            self.want_rank.insert(c.key, size);
                            impostor = box_ok;
                        } else {
                            if c.gpu_stamp != c.stamp {
                                wanted.push(Want::Chunk(i));
                                self.want_rank.insert(c.key, size);
                            }
                            if spend.take(1, c.triangles as u64) || is_hot {
                                self.residency.touch(c.key);
                                self.residency.set_rank(c.key, size);
                                let prepass = !c.glass && pre < pre_limit;
                                pre += u64::from(prepass);
                                visible.push(Draw {
                                    chunk: i,
                                    level: None,
                                    edges,
                                    prepass,
                                    dist,
                                });
                            } else {
                                dropped += 1;
                                impostor = box_ok;
                            }
                        }
                    }
                },
                Src::Inst {
                    proto, rows, group, ..
                } => {
                    let Some(p) = self.protos.get(proto) else {
                        continue;
                    };
                    let per_m = if big || p.errors[0] > 0.0 || !p.screen.is_empty() {
                        perf::px_per_m_near(&vp, &c.bounds, eye, pxf)
                    } else {
                        f32::INFINITY
                    };
                    if !lean && perf::is_coarse(p.errors[0], per_m, &cfg) {
                        coarse.insert(*group);
                    }
                    let detail = if is_hot {
                        Detail::Level(0)
                    } else if !p.screen.is_empty() {
                        Detail::Level(perf::screen_level(&p.screen, per_m * p.diag))
                    } else if !big {
                        Detail::Level(0)
                    } else {
                        perf::inst_detail(per_m, p.diag, &p.errors, c.glass, lean, moving, &cfg)
                    };
                    let want = match detail {
                        Detail::Skip => {
                            lod += 1;
                            continue;
                        }
                        Detail::Box => p.box_level,
                        Detail::Level(l) => l.min(p.box_level.saturating_sub(1)),
                    };
                    if p.gpu[want].is_none() {
                        if asked.insert((*proto, want)) {
                            wanted.push(Want::Level(*proto, want));
                        }
                        let r = self.want_rank.entry(p.keys[want]).or_insert(0.0);
                        *r = r.max(size);
                        // the mesh did not fit in memory: placements keep at least their own box
                        if self.denied.contains(&p.keys[want])
                            && !c.glass
                            && p.gpu[p.box_level].is_none()
                            && asked.insert((*proto, p.box_level))
                        {
                            wanted.push(Want::Level(*proto, p.box_level));
                            self.want_rank.insert(p.keys[p.box_level], f32::INFINITY);
                        }
                    }
                    let has_rows = c.gpu.is_some();
                    if !has_rows || c.gpu_stamp != c.stamp {
                        wanted.push(Want::Chunk(i));
                        self.want_rank.insert(c.key, size);
                    }
                    match p.nearest(want).filter(|_| has_rows) {
                        Some(l) => {
                            // nearest first: what the budget cannot hold at this level is drawn at a coarser one
                            let n = rows.len() as u64;
                            let fit = perf::fit_level(&mut spend, l, p.box_level, |k| p.levels[k].triangles.len() as u64 * n, |k| p.gpu[k].is_some());
                            if fit != Some(l) {
                                for k in l + 1..p.box_level {
                                    if p.gpu[k].is_none() && asked.insert((*proto, k)) {
                                        wanted.push(Want::Level(*proto, k));
                                    }
                                }
                            }
                            if let Some(l) = fit.or(is_hot.then_some(l)) {
                                self.residency.touch(c.key);
                                self.residency.touch(p.keys[l]);
                                self.residency.set_rank(c.key, size);
                                self.residency.set_rank(p.keys[l], size);
                                let prepass = !c.glass && l != p.box_level && pre < pre_limit;
                                pre += u64::from(prepass);
                                visible.push(Draw {
                                    chunk: i,
                                    level: p.gpu[l].clone(),
                                    edges: edges && l == 0,
                                    prepass,
                                    dist,
                                });
                            } else {
                                dropped += 1;
                                impostor = box_ok;
                            }
                        }
                        None => impostor = box_ok,
                    }
                }
            }
            // a mixed chunk under view rules would show hidden content
            if impostor && !(self.cat_rules && c.mixed) {
                if imps.len() < cfg.max_impostors {
                    imps.push((c.imp_tag, box_row(&c.bounds)));
                } else {
                    dropped += 1;
                }
            }
        }
        if !lean {
            perf::publish_coarse(coarse.iter().copied().collect());
        }
        visible.sort_unstable_by(|a, b| a.dist.total_cmp(&b.dist));
        imps.sort_unstable_by_key(|x| x.0);
        self.impostors.clear();
        self.imp_runs.clear();
        for (tag, row) in imps {
            let at = self.impostors.len() as u32;
            match self.imp_runs.last_mut() {
                Some(r) if r.0 == tag => r.2 = at + 1,
                _ => self.imp_runs.push((tag, at, at + 1)),
            }
            self.impostors.push(row);
        }
        self.stats.drawn = visible.len();
        self.stats.culled = culled;
        self.stats.lod_skipped = lod;
        self.stats.triangles_drawn = spend.tris as usize;
        self.frame = FrameStats {
            chunks: self.list.len(),
            drawn: visible.len(),
            culled,
            lod_skipped: lod,
            occluded,
            boxes: self.impostors.len(),
            dropped,
            draws: spend.draws + self.imp_runs.len() as u64,
            triangles: spend.tris + self.impostors.len() as u64 * 12,
            total_triangles: self.total_tris,
            gpu_budget: self.residency.budget(),
            moving,
            occlusion: hiz.is_some(),
            coarse: coarse.len(),
            big,
            memory_limited: self.memory_limited,
            ..Default::default()
        };
        self.visible = visible;
        self.wanted = wanted;
    }

    /// Budgeted part of the frame: accept built content, upload within the byte and GPU memory limits,
    /// queue what the frame needs most, bring accounting back into budget.
    pub fn pump(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let cfg = self.cfg;
        self.denied.clear();
        self.refresh_rows(device);
        let last = self.provoke_last;
        if let Some(w) = &self.workers {
            for d in w.take_done() {
                self.jobs = self.jobs.saturating_sub(1);
                match d {
                    Done::Meshes { key, stamp, built } => {
                        let Some(&i) = self.by_key.get(&key) else {
                            continue;
                        };
                        let c = &mut self.list[i as usize];
                        if c.job == Some(stamp) {
                            c.job = None;
                        }
                        // built before the provoking-vertex change: will be rebuilt
                        if c.stamp == stamp && built.tri.last == last {
                            self.ready.push_back(Ready::Meshes { key, stamp, built });
                        }
                    }
                    Done::Protos(v) => {
                        for (key, a, b) in v {
                            let Some(&(pk, l)) = self.proto_res.get(&key) else {
                                continue;
                            };
                            if let Some(p) = self.protos.get_mut(&pk) {
                                p.job[l] = false;
                                if a.last == last {
                                    self.ready.push_back(Ready::Level { key, v: a, l: b });
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut left = cfg.upload_per_frame;
        let (mut uploaded, mut deferred) = (0u64, 0usize);
        // deferred by the upload budget alone (the rest waits for a build, which wakes the view)
        let mut held = 0usize;
        while let Some(bytes) = self.ready.front().map(Ready::bytes) {
            if uploaded > 0 && bytes > left {
                break;
            }
            let Some(r) = self.ready.pop_front() else {
                break;
            };
            let key = r.key();
            let rank = self.want_rank.get(&key).copied().unwrap_or(0.0);
            match self.residency.reserve(bytes, rank) {
                Some(ev) => self.drop_evicted(&ev),
                None => {
                    // the real size is remembered: the planner will not build this again while there is no room
                    self.remember_size(key, bytes);
                    self.denied.insert(key);
                    continue;
                }
            }
            let res = match r {
                Ready::Meshes { stamp, built, .. } => {
                    let Some(&i) = self.by_key.get(&key) else {
                        continue;
                    };
                    let c = &mut self.list[i as usize];
                    if c.stamp != stamp {
                        continue;
                    }
                    if built.tri.last != self.provoke_last {
                        continue;
                    }
                    let replaced = c.gpu.is_some();
                    let globals = (self.globals.as_slice(), self.globals_rev);
                    let res = upload_meshes(device, c, built, &mut self.residency, globals);
                    if replaced && res.is_ok() {
                        self.occluder_rev += 1;
                    }
                    res
                }
                Ready::Level { v, l, .. } => {
                    let Some(&(pk, lvl)) = self.proto_res.get(&key) else {
                        continue;
                    };
                    if v.last != self.provoke_last {
                        continue;
                    }
                    let Some(p) = self.protos.get_mut(&pk) else {
                        continue;
                    };
                    upload_level(device, p, lvl, &v, &l, &mut self.residency)
                }
            };
            if let Err(why) = res {
                self.remember_size(key, bytes);
                self.refused(key, why);
                continue;
            }
            left = left.saturating_sub(bytes);
            uploaded += bytes.max(1);
            self.epoch += 1;
        }
        let wanted = std::mem::take(&mut self.wanted);
        if !wanted.is_empty() && self.workers.is_none() {
            self.workers = Some(Workers::new());
        }
        let max_jobs = self.workers.as_ref().map_or(0, Workers::max_jobs);
        let mut batch: Vec<ProtoJob> = vec![];
        let mut batch_tris = 0usize;
        for w in wanted {
            match w {
                Want::Chunk(i) => {
                    let c = &self.list[i as usize];
                    let key = c.key;
                    let rank = self.want_rank.get(&key).copied().unwrap_or(0.0);
                    match &c.src {
                        Src::Inst { rows, proto, .. } => {
                            let deq = self.protos.get(proto).map_or(Deq::UNIT, |p| p.deq);
                            let bytes = rows.len() as u64 * ROW_BYTES;
                            if uploaded > 0 && bytes > left {
                                deferred += 1;
                                held += 1;
                                continue;
                            }
                            match self.residency.reserve(bytes, rank) {
                                Some(ev) => self.drop_evicted(&ev),
                                None => {
                                    self.denied.insert(key);
                                    continue;
                                }
                            }
                            let c = &mut self.list[i as usize];
                            let replaced = c.gpu.is_some();
                            match upload_rows(device, c, &mut self.residency, deq) {
                                Ok(_) => {
                                    if replaced {
                                        self.occluder_rev += 1;
                                    }
                                    left = left.saturating_sub(bytes);
                                    uploaded += bytes.max(1);
                                    self.epoch += 1;
                                }
                                Err(why) => self.refused(key, why),
                            }
                        }
                        Src::Meshes(list) => {
                            if c.job == Some(c.stamp) {
                                deferred += 1;
                                continue;
                            }
                            let est = c.known.unwrap_or_else(|| mesh_est(c.triangles, c.glass));
                            if !self.residency.fits(est, rank) {
                                self.denied.insert(key);
                                continue;
                            }
                            if self.jobs >= max_jobs {
                                deferred += 1;
                                continue;
                            }
                            let stamp = c.stamp;
                            let job = Job::Meshes {
                                key,
                                stamp,
                                list: list.clone(),
                                glass: c.glass,
                                crease_limit: cfg.crease_limit,
                                last,
                            };
                            self.list[i as usize].job = Some(stamp);
                            self.jobs += 1;
                            if let Some(w) = &self.workers {
                                w.push(job);
                            }
                        }
                    }
                }
                Want::Level(pk, l) => {
                    let Some(p) = self.protos.get_mut(&pk) else {
                        continue;
                    };
                    if p.job[l] || self.jobs >= max_jobs {
                        deferred += 1;
                        continue;
                    }
                    let key = p.keys[l];
                    let tris = p.levels[l].triangles.len();
                    let lines = l == 0 && tris <= cfg.crease_limit;
                    let rank = self.want_rank.get(&key).copied().unwrap_or(0.0);
                    let est = p.known[l].unwrap_or_else(|| level_est(tris, lines));
                    if !self.residency.fits(est, rank) {
                        self.denied.insert(key);
                        continue;
                    }
                    p.job[l] = true;
                    batch.push(ProtoJob {
                        key,
                        mesh: p.levels[l].clone(),
                        cat: p.cat,
                        lines,
                        deq: p.deq,
                        last,
                    });
                    batch_tris += tris;
                    if batch.len() >= PROTO_BATCH || batch_tris >= PROTO_BATCH_TRIS {
                        self.jobs += 1;
                        batch_tris = 0;
                        if let Some(w) = &self.workers {
                            w.push(Job::Protos(std::mem::take(&mut batch)));
                        }
                    }
                }
            }
        }
        if !batch.is_empty() {
            self.jobs += 1;
            if let Some(w) = &self.workers {
                w.push(Job::Protos(batch));
            }
        }
        let ev = self.residency.evict();
        self.drop_evicted(&ev);
        self.upload_impostors(device, queue);
        let pending = self.jobs + self.ready.len() + deferred;
        self.frame.pending = pending;
        self.frame.denied = self.denied.len();
        self.frame.memory_limited = self.memory_limited;
        self.frame.uploaded_bytes = uploaded;
        self.frame.gpu_bytes = self.residency.used();
        self.frame.gpu_budget = self.residency.budget();
        self.frame.refining = (self.big && !self.motion.settled()) || pending > 0;
        let ladder = self.big && !self.motion.settled();
        self.frame.polling = ladder || !self.ready.is_empty() || held > 0;
    }

    fn upload_impostors(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.impostors.is_empty() {
            return;
        }
        let limit = device.limits().max_buffer_size;
        let plan = crate::buflimit::plan_rows(self.impostors.len() as u64, ROW_BYTES, limit);
        if plan.dropped > 0 {
            // rows are sorted by tag, so the tail is dropped and runs are clamped to match
            let keep = plan.rows as usize;
            self.impostors.truncate(keep);
            self.imp_runs.retain_mut(|r| {
                r.2 = r.2.min(keep as u32);
                r.1 < r.2
            });
            if self.imp_dropped != plan.dropped {
                eprintln!(
                    "impostors: {} rows dropped, buffer limit {} bytes",
                    plan.dropped, limit
                );
            }
        }
        self.imp_dropped = plan.dropped;
        self.frame.impostors_dropped = plan.dropped as usize;
        if plan.rows == 0 {
            return;
        }
        let need = plan.rows * ROW_BYTES;
        if self.imp_buf.as_ref().is_none_or(|b| b.1 < need) {
            let size = plan.bytes;
            self.imp_buf = Some((
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ddc-impostors"),
                    size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                size,
            ));
        }
        if let Some((b, _)) = &self.imp_buf {
            queue.write_buffer(b, 0, bytemuck::cast_slice(&self.impostors));
        }
        for &(tag, _, _) in &self.imp_runs {
            if self.imp_boxes.contains_key(&tag) {
                continue;
            }
            let unit = Aabb {
                min: Vec3::ZERO,
                max: Vec3::ONE,
            };
            let mut v = vec![];
            tri_vertices(
                &box_mesh(&unit, crate::scene::NO_SELECTION - 1, tag),
                &mut v,
            );
            // corners 0/1 are integers: proxy rows decode them without their own compression
            let s = pack_tris(&v, Deq::UNIT, false, self.provoke_last);
            let none = wgpu::BufferUsages::empty();
            if let Ok(b) = StreamGpu::new(device, "ddc-impostor-box", &s, none) {
                self.imp_boxes.insert(tag, b);
            }
        }
    }

    /// Faces selected for the frame (the pipeline is set by the caller). Chunks with their own meshes are drawn
    /// with their own rows: the shared scene matrices already decode their compression.
    fn draw_faces(&self, pass: &mut wgpu::RenderPass<'_>, d: &Draw) {
        let c = &self.list[d.chunk as usize];
        match (&c.gpu, &d.level) {
            (Some(Gpu::Meshes(g)), _) => g.draw_tri(pass),
            (Some(Gpu::Rows(b, k)), Some(l)) => {
                pass.set_vertex_buffer(1, b.slice(..));
                l.tri.draw(pass, 0..*k);
            }
            _ => {}
        }
    }

    /// Opaque chunks; `all` — everything on the GPU, without culling (the shadow map sees
    /// what is off-screen too).
    pub fn draw_opaque(&self, pass: &mut wgpu::RenderPass<'_>, all: bool) {
        if all {
            for c in self.list.iter().filter(|c| !c.glass) {
                match (&c.gpu, &c.src) {
                    (Some(Gpu::Meshes(g)), _) => g.draw_tri(pass),
                    (Some(Gpu::Rows(b, k)), Src::Inst { proto, .. }) => {
                        if let Some(l) = self.protos.get(proto).and_then(Proto::finest) {
                            pass.set_vertex_buffer(1, b.slice(..));
                            l.tri.draw(pass, 0..*k);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        for d in &self.visible {
            if !self.list[d.chunk as usize].glass {
                self.draw_faces(pass, d);
            }
        }
    }

    /// Depth pass: without boxes and (while moving) only the largest. Holes in it merely
    /// cancel occlusion culling and shading for a frame.
    pub fn draw_prepass(&self, pass: &mut wgpu::RenderPass<'_>) {
        for d in self.visible.iter().filter(|d| d.prepass) {
            self.draw_faces(pass, d);
        }
    }

    /// Chunk proxy boxes (opaque-face pipeline).
    pub fn draw_impostors(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some((rows, _)) = &self.imp_buf else {
            return;
        };
        pass.set_vertex_buffer(1, rows.slice(..));
        for &(tag, a, b) in &self.imp_runs {
            if let Some(v) = self.imp_boxes.get(&tag) {
                v.draw(pass, a..b);
            }
        }
    }

    /// Edges of visible large chunks and the ground grid.
    /// With `edges` off only selected and hovered chunks keep theirs: the selection never pops.
    pub fn draw_lines(&self, pass: &mut wgpu::RenderPass<'_>, edges: bool) {
        if let Some((g, _)) = &self.grid {
            pass.set_vertex_buffer(1, g.rows.slice(..));
            g.lines.draw(pass, 0..g.rows_n);
        }
        let shown = self
            .visible
            .iter()
            .filter(|d| d.edges && (edges || self.hot.contains(&d.chunk)));
        for d in shown {
            let c = &self.list[d.chunk as usize];
            match (&c.gpu, &d.level) {
                (Some(Gpu::Meshes(g)), _) if g.lines.count > 0 => {
                    pass.set_vertex_buffer(1, g.rows.slice(..));
                    g.lines.draw(pass, 0..g.rows_n);
                }
                (Some(Gpu::Rows(b, k)), Some(l)) if l.lines.count > 0 => {
                    pass.set_vertex_buffer(1, b.slice(..));
                    l.lines.draw(pass, 0..*k);
                }
                _ => {}
            }
        }
    }

    pub fn has_glass(&self) -> bool {
        self.visible
            .iter()
            .any(|d| self.list[d.chunk as usize].glass)
    }

    /// Re-sorting glass of visible chunks if the eye moved; for a large model — within
    /// the frame budget, starting with the nearest.
    pub fn sort_glass(&mut self, queue: &wgpu::Queue, eye: Vec3) {
        let mut left = if self.big {
            self.cfg.glass_sort_tris
        } else {
            usize::MAX
        };
        for d in &self.visible {
            let c = &mut self.list[d.chunk as usize];
            if !c.glass || c.sorted_eye.is_some_and(|e| e.distance_squared(eye) < 1e-4) {
                continue;
            }
            // the old row buffer has a different size: re-sorting waits for the replacement
            if matches!(c.src, Src::Inst { .. }) && c.gpu_stamp != c.stamp {
                continue;
            }
            let centers: &[Vec3] = match (&c.src, &c.gpu) {
                (Src::Inst { centers, .. }, _) => centers.as_slice(),
                (_, Some(Gpu::Meshes(g))) => g.centers.as_slice(),
                _ => continue,
            };
            if centers.len() > left {
                continue;
            }
            left -= centers.len();
            let mut order: Vec<(f32, u32)> = centers
                .iter()
                .enumerate()
                .map(|(k, p)| (p.distance_squared(eye), k as u32))
                .collect();
            order.sort_by(|a, b| b.0.total_cmp(&a.0));
            match (&c.src, &c.gpu) {
                (Src::Inst { rows, proto, .. }, Some(Gpu::Rows(b, _))) => {
                    let deq = self.protos.get(proto).map_or(Deq::UNIT, |p| p.deq);
                    let sorted: Vec<[f32; 16]> = order
                        .iter()
                        .map(|&(_, k)| deq.row(&rows[k as usize]))
                        .collect();
                    if !sorted.is_empty() {
                        queue.write_buffer(b, 0, bytemuck::cast_slice(&sorted));
                    }
                }
                (_, Some(Gpu::Meshes(g))) => {
                    let idx: Vec<u32> = order
                        .iter()
                        .filter_map(|&(_, t)| g.tris.get(t as usize))
                        .flatten()
                        .copied()
                        .collect();
                    if !idx.is_empty() && idx.len() as u32 == g.tri.count {
                        queue.write_buffer(&g.tri.index, 0, bytemuck::cast_slice(&idx));
                    }
                }
                _ => {}
            }
            c.sorted_eye = Some(eye);
        }
    }

    /// Translucent chunks far to near (the glass pipeline is set by the caller). Chunks
    /// are ordered by bounds centre, triangles within a chunk: intersecting shells of neighbouring
    /// cells may blend with artefacts (expected without OIT).
    pub fn draw_glass(&self, pass: &mut wgpu::RenderPass<'_>) {
        for d in self.visible.iter().rev() {
            let c = &self.list[d.chunk as usize];
            if !c.glass {
                continue;
            }
            match (&c.gpu, &d.level) {
                // u32 indices in a single run, triangle order is written by `sort_glass`
                (Some(Gpu::Meshes(g)), _) => g.draw_tri(pass),
                // glass instances: the order is set by the instance buffer rows
                (Some(Gpu::Rows(b, k)), Some(l)) => {
                    pass.set_vertex_buffer(1, b.slice(..));
                    l.tri.draw(pass, 0..*k);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "tests_bounds.rs"]
mod tests_bounds;

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(element: u32, at: [f32; 3], tag: u32) -> SceneMesh {
        let positions = (0..8)
            .map(|i| {
                [
                    at[0] + (i & 1) as f32,
                    at[1] + ((i >> 1) & 1) as f32,
                    at[2] + ((i >> 2) & 1) as f32,
                ]
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
            element,
            tag,
            positions,
            triangles,
        }
    }

    #[test]
    fn edit_changes_only_its_chunk_stamp() {
        let mk = |dz: f32| {
            Scene::new(vec![
                cube(0, [0., 0., 0.], 0),
                cube(1, [40., 0., dz], 0),
                cube(2, [0., 0., 2.], crate::scene::tag_rgb([0, 90, 200], true)),
            ])
        };
        let (a, b) = (plan(&mk(0.0)), plan(&mk(0.5)));
        assert_eq!(a.len(), 3, "two cells + translucent chunk");
        let changed: Vec<_> = a
            .iter()
            .zip(&b)
            .filter(|(x, y)| x.stamp != y.stamp)
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0.meshes, vec![1]);
        assert!(a.iter().any(|p| p.glass && p.meshes == vec![2]));
        assert_eq!(
            a.iter().map(|p| p.key).collect::<Vec<_>>(),
            b.iter().map(|p| p.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn shared_meshes_keep_fp_and_list_order_does_not_matter() {
        let a = Arc::new(cube(5, [0., 0., 0.], 0));
        let b = Arc::new(cube(3, [1., 0., 0.], 0));
        let s1 = Scene::shared(vec![a.clone(), b.clone()]);
        let s2 = Scene::shared(vec![b.clone(), a.clone()]);
        assert_eq!(
            plan(&s1)[0].stamp,
            plan(&s2)[0].stamp,
            "cell content is ordered by element"
        );
        let mut c = FpCache::default();
        let f1 = c.refresh(&s1);
        assert_eq!(f1[&(Arc::as_ptr(&a) as usize)], mesh_fp(&a));
        assert_eq!(c.refresh(&s2), f1);
        assert_eq!(
            c.refresh(&Scene::shared(vec![a.clone()])).len(),
            1,
            "cache keeps only live meshes"
        );
    }

    #[test]
    fn frustum_and_size() {
        // reversed depth without a far plane, as for the camera
        let vp = Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1)
            * Mat4::look_at_rh(Vec3::new(0., -10., 0.), Vec3::ZERO, Vec3::Z);
        let b = |c: Vec3, r: f32| Aabb {
            min: c - Vec3::splat(r),
            max: c + Vec3::splat(r),
        };
        assert!(screen_size(&vp, &b(Vec3::ZERO, 1.0), (800., 800.)).unwrap() > 50.0);
        assert_eq!(
            screen_size(&vp, &b(Vec3::new(0., -30., 0.), 1.0), (800., 800.)),
            None,
            "behind"
        );
        assert_eq!(
            screen_size(&vp, &b(Vec3::new(500., 0., 0.), 1.0), (800., 800.)),
            None,
            "aside"
        );
        assert!(
            screen_size(&vp, &b(Vec3::new(0., 900., 0.), 0.01), (800., 800.)).unwrap() < LOD_PX
        );
    }

    /// Quads of two triangles with corners of their own are leaf cards: every corner carries
    /// the card bit and its picture corner, a needle spray its bit; a solid crown carries none,
    /// whatever its colour.
    #[test]
    fn leaf_cards_carry_their_picture_corners_and_a_solid_crown_none() {
        use crate::scene::{is_card, tag_foliage, CARD, CARD_NEEDLE, CARD_U, CARD_V};
        let tag = tag_foliage([60, 120, 51]);
        let quad = |o: f32| [[o, 0., 0.], [o + 1., 0., 0.], [o + 1., 0., 1.], [o, 0., 1.]];
        let mut positions = quad(0.0).to_vec();
        positions.extend(quad(2.0));
        let m = SceneMesh { element: 1, tag, positions, triangles: vec![[0, 1, 2], [0, 2, 3], [4, 5, 7], [5, 6, 7]] };
        let cards = leaf_cards(&m).expect("two cards");
        assert_eq!(cards, vec![([0, 1, 2, 3], false), ([4, 5, 6, 7], true)]);
        let mut v = vec![];
        tri_vertices(&m, &mut v);
        assert_eq!(v.len(), 12);
        assert!(v.iter().all(|x| is_card(x.tag)));
        assert!(v[..6].iter().all(|x| x.tag & CARD_NEEDLE == 0) && v[6..].iter().all(|x| x.tag & CARD_NEEDLE != 0));
        let uv = |x: &Vertex| (x.tag & CARD_U != 0, x.tag & CARD_V != 0);
        assert_eq!(uv(&v[0]), (false, false));
        assert_eq!(uv(&v[2]), (true, true));
        // shared corners are a solid: no card bits
        let solid = SceneMesh { triangles: vec![[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]], ..m.clone() };
        assert!(leaf_cards(&solid).is_none());
        let mut v = vec![];
        tri_vertices(&solid, &mut v);
        assert!(v.iter().all(|x| x.tag & CARD == 0 && crate::scene::is_foliage(x.tag)));
    }

    #[test]
    fn category_changes_stamp_and_vertex_tags_not_geometry() {
        let a = Arc::new(cube(0, [0., 0., 0.], crate::scene::TAG_SLAB));
        let s0 = Scene::shared(vec![a.clone()]);
        let s1 = Scene::shared(vec![a.clone()]).with_cats(vec![3]);
        assert_ne!(plan(&s0)[0].stamp, plan(&s1)[0].stamp);
        assert_eq!(
            plan(&s1)[0].stamp,
            plan(&Scene::shared(vec![a.clone()]).with_cats(vec![3]))[0].stamp
        );
        assert_eq!(s1.cat(0), 3);
        assert_eq!(s1.cat(9), 0, "no entry — category 0");
        let mut v = vec![];
        tri_vertices(&a, &mut v);
        set_cat(&mut v, 3);
        assert!(v
            .iter()
            .all(|x| (x.tag >> 24) & 15 == 3
                && crate::style::base_tag(x.tag) == crate::scene::TAG_SLAB));
    }

    #[test]
    fn instance_row_carries_element() {
        let r = inst_row(&Inst {
            m: Mat4::from_translation(Vec3::new(1., 2., 3.)),
            element: 0x5001_0203,
        });
        assert_eq!(r[3], (0x0203 + 1) as f32);
        assert_eq!(r[7], 0x5001 as f32);
        assert_eq!(&r[12..15], &[1., 2., 3.]);
    }

    fn group(key: u64, n: usize, step: f32) -> Instanced {
        let inst = (0..n)
            .map(|i| Inst {
                m: Mat4::from_translation(Vec3::new(i as f32 * step, 0., 0.)),
                element: i as u32,
            })
            .collect();
        Instanced::new(key, Arc::new(cube(0, [0., 0., 0.], 0)), inst)
    }

    #[test]
    fn workers_scale_with_cores_and_stay_bounded() {
        assert_eq!(
            [1, 2, 3, 4, 8, 16, 64].map(workers_for),
            [1, 1, 1, 2, 6, 8, 8]
        );
        assert!((1..=8).contains(&worker_count()));
    }

    #[test]
    fn parallel_group_layout_equals_sequential() {
        let gs: Vec<Instanced> = (0..48).map(|k| group(k, 2_000 + k as usize * 50, 2.0)).collect();
        let idx: Vec<usize> = (0..gs.len()).collect();
        let sig = |c: &GroupCache| {
            let parts: Vec<_> = c.parts.iter().map(|p| (p.key, p.stamp, p.triangles)).collect();
            (c.proto_key, parts, c.baked.len())
        };
        let t0 = web_time::Instant::now();
        let one = par_map(&idx, 1, |&i| group_cache(&gs[i], 3, 11));
        let seq = t0.elapsed();
        let t1 = web_time::Instant::now();
        let many = par_map(&idx, worker_count().max(2), |&i| group_cache(&gs[i], 3, 11));
        let par = t1.elapsed();
        assert_eq!(one.len(), gs.len());
        for (a, b) in one.iter().zip(&many) {
            assert_eq!(sig(a), sig(b));
        }
        eprintln!(
            "group layout {} inst: 1 thread {seq:?}, {} threads {par:?}",
            gs.iter().map(|g| g.inst.len()).sum::<usize>(),
            worker_count().max(2)
        );
    }

    #[test]
    fn group_parts_cover_every_row_once() {
        let g = group(7, 1000, 2.0);
        let parts = group_parts(&g, 1, false);
        assert_eq!(parts.iter().map(|p| p.rows.len()).sum::<usize>(), 1000);
        assert_eq!(parts.iter().map(|p| p.triangles).sum::<usize>(), 12_000);
        assert!(parts.len() > 1 && parts.len() <= 1000 / INST_PER_CHUNK + 1);
        assert!(parts
            .iter()
            .all(|p| !p.bounds.is_empty() && p.centers.is_empty()));
        let again = group_parts(&g, 1, false);
        assert_eq!(
            parts.iter().map(|p| (p.key, p.stamp)).collect::<Vec<_>>(),
            again.iter().map(|p| (p.key, p.stamp)).collect::<Vec<_>>(),
            "keys are stable between syncs"
        );
    }

    #[test]
    fn rare_bodies_do_not_make_a_chunk_each() {
        let parts = group_parts(&group(1, 8, 50.0), 1, true);
        assert!(parts.len() <= 2, "{}", parts.len());
        assert_eq!(
            parts.iter().map(|p| p.centers.len()).sum::<usize>(),
            8,
            "glass keeps centers"
        );
    }

    #[test]
    fn a_row_change_touches_only_its_part() {
        let a = group(9, 400, 4.0);
        let mut moved: Vec<Inst> = a.inst.as_ref().clone();
        moved[0].m = Mat4::from_translation(Vec3::new(0., 0., 0.5));
        let b = Instanced::new(9, a.proto.clone(), moved);
        let (pa, pb) = (group_parts(&a, 1, false), group_parts(&b, 1, false));
        assert_eq!(pa.len(), pb.len());
        let diff = pa
            .iter()
            .zip(&pb)
            .filter(|(x, y)| x.stamp != y.stamp)
            .count();
        assert_eq!(diff, 1);
    }

    #[test]
    fn impostor_box_is_outward_and_row_places_it() {
        let unit = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        let m = box_mesh(&unit, 3, 0);
        let mut v = vec![];
        tri_vertices(&m, &mut v);
        assert_eq!(v.len(), 36);
        let c = Vec3::splat(0.5);
        for t in v.chunks_exact(3) {
            let f = (Vec3::from(t[0].pos) + Vec3::from(t[1].pos) + Vec3::from(t[2].pos)) / 3.0;
            assert!(
                Vec3::from(t[0].normal).dot(f - c) > 0.0,
                "normals look outward"
            );
        }
        let b = Aabb {
            min: Vec3::new(1., 2., 3.),
            max: Vec3::new(2., 4., 6.),
        };
        let r = Mat4::from_cols_array(&box_row(&b));
        assert_eq!(r.transform_point3(Vec3::ZERO), b.min);
        assert_eq!(r.transform_point3(Vec3::ONE), b.max);
        assert_eq!(box_row(&b)[3], 0.0, "element comes from the vertex");
    }

    #[test]
    fn rare_groups_are_written_into_their_cells() {
        let one = group(1, 1, 1.0);
        let many = group(2, 9, 1.0);
        assert!(is_rare(&one) && bakes(&one));
        assert!(!bakes(&group(4, 1, 1.0).with_own()), "an own source keeps its chunks");
        assert!(!is_rare(&many), "enough placements earn their own chunks");
        assert!(
            !is_rare(&group(3, 1, 1.0).with_approx(0.1)),
            "an approximation stays replaceable"
        );
        // a hundred unique bodies in two cells: two chunks, not a hundred draw calls
        let mut cells: HashMap<((i32, i32, i32), bool), Vec<(Baked, Arc<SceneMesh>, u8)>> =
            HashMap::new();
        let proto = Arc::new(cube(0, [0., 0., 0.], 0));
        for k in 0..100u32 {
            let x = if k < 50 { 0.0 } else { 40.0 };
            let g = Instanced::new(
                u64::from(k),
                proto.clone(),
                vec![Inst {
                    m: Mat4::from_translation(Vec3::new(x + (k % 7) as f32, 0., 0.)),
                    element: k,
                }],
            );
            for b in baked_of(&g, 5, false) {
                cells
                    .entry((b.cell, b.glass))
                    .or_default()
                    .push((b, proto.clone(), 3));
            }
        }
        let parts = bake_parts(cells);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts.iter().map(|p| p.items.len()).sum::<usize>(), 100);
        assert_eq!(parts.iter().map(|p| p.triangles).sum::<usize>(), 1200);
        let c = mesh_chunk(
            (
                parts[0].key,
                parts[0].stamp,
                false,
                parts[0].bounds,
                parts[0].triangles,
            ),
            parts[0].items.clone(),
            None,
        );
        assert!(!c.mixed);
        assert!(c
            .src
            .any_element(|e| e == parts[0].items[0].2.unwrap().element));
        assert!(c.elems.0 < c.elems.1);
    }

    #[test]
    fn baked_stamp_follows_the_placement_only() {
        let mk = |x: f32| {
            let g = Instanced::new(
                1,
                Arc::new(cube(0, [0., 0., 0.], 0)),
                vec![Inst {
                    m: Mat4::from_translation(Vec3::new(x, 0., 0.)),
                    element: 4,
                }],
            );
            baked_of(&g, 9, false)[0].fp
        };
        assert_eq!(mk(1.0), mk(1.0));
        assert_ne!(mk(1.0), mk(2.0));
    }

    #[test]
    fn mixed_chunk_box_carries_no_category() {
        let a = Arc::new(cube(1, [0., 0., 0.], 2));
        let b = Arc::new(cube(2, [1., 0., 0.], 3));
        let head = (1u64, 1u64, false, Aabb::EMPTY, 24usize);
        let m = mesh_chunk(head, vec![(a.clone(), 4, None), (b, 5, None)], None);
        assert!(m.mixed);
        assert_eq!(
            m.imp_tag, a.tag,
            "category 0 is never hidden by a view rule"
        );
        assert_eq!(m.elems, (1, 3));
        let s = mesh_chunk(head, vec![(a.clone(), 4, None)], None);
        assert!(!s.mixed);
        assert_eq!((s.imp_tag >> 24) & 15, 4);
    }

    #[test]
    fn row_element_round_trips() {
        for e in [0u32, 1, 0xFFFF, 0x1_0000, 0x5001_0203] {
            let r = inst_row(&Inst {
                m: Mat4::IDENTITY,
                element: e,
            });
            assert_eq!(row_element(&r), e);
        }
    }

    #[test]
    fn a_denied_chunk_may_show_a_box_at_any_size() {
        assert!(box_allowed(false, 30.0, 64.0, false, false, 20.0));
        assert!(
            !box_allowed(false, 300.0, 64.0, false, false, 20.0),
            "still loading: no cover"
        );
        assert!(
            box_allowed(false, 300.0, 64.0, true, false, 20.0),
            "no memory: better than a hole"
        );
        assert!(
            !box_allowed(true, 30.0, 64.0, true, false, 20.0),
            "never for glass"
        );
        assert!(
            !box_allowed(false, 300.0, 64.0, true, true, 240.0),
            "a long row of columns is not one box"
        );
    }

    #[test]
    fn estimates_cover_edges_so_a_ready_build_is_not_thrown_away() {
        assert!(mesh_est(100, false) >= 100 * TRI_BYTES);
        assert!(mesh_est(100, true) > mesh_est(100, false));
        assert!(level_est(100, true) > level_est(100, false));
        // an upper estimate also for a mesh without shared vertices: triangles apart, each with three
        // boundary edges
        let mut loose = cube(0, [0., 0., 0.], 0);
        loose.positions = loose
            .triangles
            .iter()
            .flat_map(|t| t.map(|i| loose.positions[i as usize]))
            .collect();
        loose.triangles = (0..12u32).map(|t| [t * 3, t * 3 + 1, t * 3 + 2]).collect();
        for glass in [false, true] {
            for m in [cube(0, [0., 0., 0.], 0), loose.clone()] {
                let n = m.triangles.len();
                let b = build_meshes(&[(Arc::new(m), 0u8, None)], glass, usize::MAX, false);
                assert!(mesh_est(n, glass) >= b.bytes() + ROW_BYTES, "glass {glass}");
                assert!(level_est(n, true) >= b.bytes() || glass);
            }
        }
    }

    /// Meshes of the synthetic model (all chunks and prototype levels): bytes of flat vertices at 32
    /// (glass — with u32 indices) versus compressed with shared vertices.
    fn synth_bytes(spec: &crate::synth::SynthSpec) -> ((u64, u64), (u64, u64), u64) {
        let scene = crate::synth::generate(spec);
        let (mut flat, mut packed, mut tris) = ((0u64, 0u64), (0u64, 0u64), 0u64);
        let mut add = |t: &[Vertex], l: &[Vertex], glass: bool, b: (&Stream, &Stream)| {
            flat.0 += t.len() as u64 * 32 + if glass { t.len() as u64 * 4 } else { 0 };
            flat.1 += l.len() as u64 * 32;
            packed.0 += b.0.bytes();
            packed.1 += b.1.bytes();
            tris += t.len() as u64 / 3;
        };
        for p in plan(&scene) {
            let list: Vec<MeshItem> = p
                .meshes
                .iter()
                .map(|&i| (scene.meshes[i].clone(), 0u8, None))
                .collect();
            let b = build_meshes(&list, p.glass, usize::MAX, false);
            let (mut t, mut l) = (vec![], vec![]);
            for (m, _, _) in &list {
                tri_vertices(m, &mut t);
                crease_vertices(m, &mut l);
            }
            add(&t, &l, p.glass, (&b.tri, &b.lines));
        }
        for g in &scene.instanced {
            let p = Proto::new(g.key, g, 0);
            for (k, m) in p.levels.iter().enumerate() {
                let lines = k == 0;
                let (v, l) = build_proto(m, 0, lines, p.deq, false);
                let (mut t, mut e) = (vec![], vec![]);
                tri_vertices(m, &mut t);
                if lines {
                    crease_vertices(m, &mut e);
                }
                add(&t, &e, false, (&v, &l));
            }
        }
        (flat, packed, tris)
    }

    #[test]
    fn packed_geometry_on_a_synthetic_city() {
        for spec in [
            // spheres: flat shading of a curved surface, each triangle has its own normal
            crate::synth::SynthSpec {
                elements: 20_000,
                ..Default::default()
            },
            crate::synth::SynthSpec {
                elements: 100_000,
                tris_per_element: 300,
                ..Default::default()
            },
        ] {
            let (flat, packed, tris) = synth_bytes(&spec);
            let (f, p) = (flat.0 + flat.1, packed.0 + packed.1);
            let ratio = f as f64 / p as f64;
            println!(
                "synth {} el × {} tri: {tris} tri; flat {:.1} MB ({:.1} B/tri faces, {:.1} MB edges) \
                 → packed {:.1} MB ({:.1} B/tri faces, {:.1} MB edges); {ratio:.2}×",
                spec.elements,
                spec.tris_per_element,
                f as f64 / 1e6,
                flat.0 as f64 / tris as f64,
                flat.1 as f64 / 1e6,
                p as f64 / 1e6,
                packed.0 as f64 / tris as f64,
                packed.1 as f64 / 1e6,
            );
            // lower bound for flat shading of curved meshes: a vertex (20 B) and three u16 indices per
            // triangle — 26 B versus 96
            assert!(packed.0 as f64 / tris as f64 <= 27.0, "{packed:?} / {tris}");
            assert!(ratio >= 3.5, "{ratio}");
        }
    }

    #[test]
    fn nearest_level_prefers_coarser() {
        assert_eq!(nearest_level(1, 4, |l| l == 1 || l == 2), Some(1));
        assert_eq!(nearest_level(1, 4, |l| l == 0 || l == 3), Some(3));
        assert_eq!(nearest_level(2, 4, |l| l == 0 || l == 1), Some(1));
        assert_eq!(nearest_level(0, 4, |_| false), None);
    }

    #[test]
    fn lods_become_levels_with_a_box_last() {
        let g = group(1, 1, 1.0).with_lods(vec![(Arc::new(cube(0, [0., 0., 0.], 0)), 0.05)]);
        let p = Proto::new(77, &g, 2);
        assert_eq!(p.levels.len(), 3);
        assert_eq!(p.box_level, 2);
        assert_eq!(p.errors, vec![0.0, 0.05]);
        assert_eq!(p.levels[2].triangles.len(), 12);
        assert_eq!(p.keys.iter().collect::<HashSet<_>>().len(), 3);
        assert!(p.diag > 1.7 && p.diag < 1.8);
    }
}
