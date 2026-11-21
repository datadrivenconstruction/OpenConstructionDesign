// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Frame logic for a large model without the GPU (covered by tests): budgets, camera motion and
//! gradual refinement, GPU memory accounting with eviction, the chunk box tree, the depth
//! pyramid for occlusion culling, level-of-detail selection, frame counters. Goals and measurement —
//! `docs/design/PERF_3D.md`.

use crate::scene::Aabb;
use glam::{Mat4, Vec3};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

const MB: u64 = 1 << 20;

/// Adapter class: memory and the idle frame limit depend on it (the API does not report memory size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuClass {
    Cpu,
    Integrated,
    Discrete,
}

#[cfg(feature = "gpu")]
impl GpuClass {
    pub fn of(t: wgpu::DeviceType) -> Self {
        match t {
            wgpu::DeviceType::DiscreteGpu => Self::Discrete,
            wgpu::DeviceType::Cpu => Self::Cpu,
            _ => Self::Integrated,
        }
    }
}

/// Frame budgets. Defaults are for an integrated GPU; the actual class is set by `for_class`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PerfConfig {
    /// GPU memory for meshes and placements (excluding frame targets), bytes.
    pub gpu_budget: u64,
    /// GPU upload per frame, bytes (at least one build per frame).
    pub upload_per_frame: u64,
    /// A build with fewer triangles than this runs in place, as before; larger — in the background.
    pub sync_tris: u64,
    /// A heavier model — budgets, proxies and simplifications apply while moving.
    pub big_tris: u64,
    pub draws_moving: u64,
    /// Idle limit: only a guard against a driver reset on a long frame; what did not fit shows in
    /// the status bar.
    pub draws_still: u64,
    pub tris_moving: u64,
    pub tris_still: u64,
    /// Depth pre-pass draw calls while moving.
    pub prepass_draws_moving: u64,
    /// A chunk smaller than this (pixels) is not drawn.
    pub cull_px: f32,
    /// Multiplier of screen-size thresholds while moving.
    pub moving_scale: f32,
    /// A smaller chunk becomes a proxy box.
    pub impostor_px: f32,
    /// An unbuilt chunk larger than this is not replaced by a box (it would block the view): empty until built.
    pub impostor_max_px: f32,
    /// A smaller placement becomes a box of its mesh; smaller than `inst_skip_px` — not drawn.
    pub inst_box_px: f32,
    pub inst_skip_px: f32,
    /// Level-of-detail tolerance, pixels (also when idle: indistinguishable from the exact mesh to the eye).
    pub tol_px: f32,
    pub max_impostors: usize,
    /// Occlusion culling; enabled by the app (needs an R32Float colour target).
    pub occlusion: bool,
    pub occlusion_min_tris: u64,
    /// A heavier model — no sun shadows.
    pub shadow_tris: u64,
    /// Glass re-sorting per frame, triangles.
    pub glass_sort_tris: usize,
    /// Crease edges in the background build: a heavier mesh goes without them.
    pub crease_limit: usize,
    /// While moving a big model, chunks farther than this (metres) are skipped; 0 is no limit.
    pub draw_distance: f32,
}

impl Default for PerfConfig {
    fn default() -> Self {
        Self {
            gpu_budget: 2000 * MB,
            upload_per_frame: 24 * MB,
            sync_tris: 200_000,
            big_tris: 3_000_000,
            draws_moving: 2_500,
            draws_still: 80_000,
            tris_moving: 5_000_000,
            tris_still: 100_000_000,
            prepass_draws_moving: 800,
            cull_px: 1.5,
            moving_scale: 3.0,
            impostor_px: 6.0,
            impostor_max_px: 64.0,
            inst_box_px: 5.0,
            inst_skip_px: 0.75,
            tol_px: 0.5,
            max_impostors: 65_536,
            occlusion: false,
            occlusion_min_tris: 2_000_000,
            shadow_tris: 20_000_000,
            glass_sort_tris: 300_000,
            crease_limit: 200_000,
            draw_distance: 0.0,
        }
    }
}

impl PerfConfig {
    /// Defaults and `DDC_GPU_BUDGET_MB` (benchmarks and weak machines).
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Some(b) = env_budget() {
            c.gpu_budget = b;
        }
        c
    }

    /// Limits by adapter class; `DDC_GPU_BUDGET_MB` takes precedence. There may be less memory
    /// (a discrete 2 GB card): then the limit is lowered by the allocation-failure fallback (`Chunks::pump`).
    pub fn for_class(mut self, class: GpuClass) -> Self {
        let (mb, draws, tris): (u64, u64, u64) = match class {
            GpuClass::Discrete => (3500, 200_000, 400_000_000),
            GpuClass::Integrated => (2000, 80_000, 100_000_000),
            GpuClass::Cpu => (1000, 20_000, 8_000_000),
        };
        self.gpu_budget = env_budget().unwrap_or(mb * MB);
        self.draws_still = draws;
        self.tris_still = tris;
        self
    }
}

fn env_budget() -> Option<u64> {
    std::env::var("DDC_GPU_BUDGET_MB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|mb| mb * MB)
}

/// After an allocation failure: the limit slightly below what is in use, but at least 256 MB.
pub fn budget_after_oom(used: u64, budget: u64) -> u64 {
    (used / 4 * 3).clamp(256 * MB, budget.max(256 * MB))
}

/// Idle frames over which budgets reach their full values.
pub const LADDER: u32 = 5;

/// Camera motion: consecutive idle frames; budgets double per idle frame.
#[derive(Clone, Debug, Default)]
pub struct Motion {
    last: Option<Mat4>,
    still: u32,
    one_shot: bool,
}

impl Motion {
    pub fn update(&mut self, vp: &Mat4) {
        let moved = self.last.is_none_or(|l| !l.abs_diff_eq(*vp, 1e-5));
        self.still = if moved {
            0
        } else {
            self.still.saturating_add(1)
        };
        self.last = Some(*vp);
        if std::mem::take(&mut self.one_shot) {
            self.still = self.still.max(LADDER);
        }
    }
    /// The next frame is the only one (a snapshot): use idle limits at once, nobody will refine it.
    pub fn assume_rest(&mut self) {
        self.one_shot = true;
    }
    pub fn moving(&self) -> bool {
        self.still == 0
    }
    pub fn still_frames(&self) -> u32 {
        self.still
    }
    pub fn settled(&self) -> bool {
        self.still >= LADDER
    }
    /// Frame budget: `moving` while moving, then doubling up to `still`.
    pub fn budget(&self, moving: u64, still: u64) -> u64 {
        if self.still >= LADDER {
            return still;
        }
        moving.saturating_mul(1u64 << self.still).min(still)
    }
}

/// Frame budget spend counter. The first candidate always passes: the largest chunk on
/// screen never disappears, even if it alone exceeds the budget.
#[derive(Clone, Copy, Debug)]
pub struct Spend {
    pub draws: u64,
    pub tris: u64,
    max_draws: u64,
    max_tris: u64,
}

impl Spend {
    pub fn new(max_draws: u64, max_tris: u64) -> Self {
        Self {
            draws: 0,
            tris: 0,
            max_draws,
            max_tris,
        }
    }
    pub fn take(&mut self, draws: u64, tris: u64) -> bool {
        let ok = self.draws == 0
            || (self.draws + draws <= self.max_draws
                && self.tris.saturating_add(tris) <= self.max_tris);
        if ok {
            self.draws += draws;
            self.tris = self.tris.saturating_add(tris);
        }
        ok
    }
}

/// GPU memory accounting: entries by key and recency of use, the oldest are evicted first.
/// What was used in this frame is evicted only for something larger on screen (`set_rank`):
/// distant small items do not hold memory when a nearby wall needs room.
#[derive(Clone, Debug, Default)]
pub struct Residency {
    budget: u64,
    used: u64,
    /// Occupied by entries touched in this frame.
    fresh: u64,
    frame: u64,
    map: HashMap<u64, (u64, u64)>,
    lru: BTreeSet<(u64, u64)>,
    ranks: Vec<(f32, u64)>,
    /// `ranks` ascending with a byte sum from below: a refusal costs log n, not a list walk.
    sorted: Option<Vec<(f32, u64, u64)>>,
    cursor: usize,
}

impl Residency {
    pub fn new(budget: u64) -> Self {
        Self {
            budget,
            ..Default::default()
        }
    }
    pub fn set_budget(&mut self, b: u64) {
        self.budget = b;
    }
    pub fn budget(&self) -> u64 {
        self.budget
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn contains(&self, key: u64) -> bool {
        self.map.contains_key(&key)
    }
    pub fn frame(&self) -> u64 {
        self.frame
    }
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.fresh = 0;
        self.ranks.clear();
        self.sorted = None;
        self.cursor = 0;
    }
    /// A new entry or a new size for an existing one; counts as used in this frame.
    pub fn insert(&mut self, key: u64, bytes: u64) {
        self.remove(key);
        self.map.insert(key, (bytes, self.frame));
        self.lru.insert((self.frame, key));
        self.used += bytes;
        self.fresh += bytes;
    }
    pub fn touch(&mut self, key: u64) {
        if let Some(e) = self.map.get_mut(&key) {
            if e.1 != self.frame {
                self.lru.remove(&(e.1, key));
                e.1 = self.frame;
                self.lru.insert((self.frame, key));
                self.fresh += e.0;
            }
        }
    }
    pub fn remove(&mut self, key: u64) {
        if let Some((b, last)) = self.map.remove(&key) {
            self.lru.remove(&(last, key));
            self.used -= b;
            if last == self.frame {
                self.fresh -= b;
            }
        }
    }
    /// Screen size of an entry in this frame; without it the frame's entry is not evicted for others.
    pub fn set_rank(&mut self, key: u64, rank: f32) {
        self.ranks.push((rank, key));
        self.sorted = None;
    }

    fn ensure_sorted(&mut self) {
        if self.sorted.is_some() {
            return;
        }
        self.ranks.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        let mut seen = std::collections::HashSet::with_capacity(self.ranks.len());
        let mut v: Vec<(f32, u64, u64)> = self
            .ranks
            .iter()
            .filter(|r| seen.insert(r.1))
            .filter_map(|r| self.map.get(&r.1).map(|e| (r.0, r.1, e.0)))
            .collect();
        v.reverse();
        let mut sum = 0u64;
        for x in &mut v {
            sum += x.2;
            x.2 = sum;
        }
        self.sorted = Some(v);
        self.cursor = 0;
    }

    /// How much can be freed for an entry with screen size `rank`.
    fn reclaimable(&mut self, rank: f32) -> u64 {
        self.ensure_sorted();
        let stale = self.used - self.fresh;
        let Some(v) = self.sorted.as_ref() else {
            return stale;
        };
        let upto = v.partition_point(|x| x.0 < rank);
        let sum = |n: usize| if n == 0 { 0 } else { v[n - 1].2 };
        stale + sum(upto).saturating_sub(sum(self.cursor))
    }

    /// Whether `bytes` with screen size `rank` will fit (ask before an expensive build).
    pub fn fits(&mut self, bytes: u64, rank: f32) -> bool {
        let need = (self.used + bytes).saturating_sub(self.budget);
        need == 0 || self.reclaimable(rank) >= need
    }

    /// Free room for `bytes`: keys of the evicted; None — not enough even after eviction
    /// (then nothing is evicted). First the long-unused, then the smallest from the frame.
    pub fn reserve(&mut self, bytes: u64, rank: f32) -> Option<Vec<u64>> {
        if !self.fits(bytes, rank) {
            return None;
        }
        let mut out = vec![];
        while self.used + bytes > self.budget {
            let Some(&(last, key)) = self.lru.iter().next() else {
                break;
            };
            if last >= self.frame {
                break;
            }
            self.remove(key);
            out.push(key);
        }
        while self.used + bytes > self.budget {
            let Some(&(r, key, _)) = self.sorted.as_ref().and_then(|v| v.get(self.cursor)) else {
                break;
            };
            if r >= rank {
                break;
            }
            self.cursor += 1;
            if self.map.contains_key(&key) {
                self.remove(key);
                out.push(key);
            }
        }
        Some(out)
    }
    /// Return to the budget by evicting entries not used in this frame.
    pub fn evict(&mut self) -> Vec<u64> {
        let mut out = vec![];
        while self.used > self.budget {
            let Some(&(last, key)) = self.lru.iter().next() else {
                break;
            };
            if last >= self.frame {
                break;
            }
            self.remove(key);
            out.push(key);
        }
        out
    }
}

/// Tree-walk event.
pub enum TreeEv<'a> {
    /// A node and the number of leaves under it; `false` — skip the subtree.
    Node(&'a Aabb, u32),
    Leaf(u32),
}

#[derive(Clone, Copy, Debug)]
struct TreeNode {
    b: Aabb,
    /// Leaf: `order[first..first + count]`; node: children `first`, `first + 1`.
    first: u32,
    count: u32,
    leaves: u32,
}

const TREE_LEAF: usize = 8;

/// Tree of chunk boxes: a frame walks only the visible part, not the whole list.
#[derive(Clone, Debug, Default)]
pub struct BoxTree {
    nodes: Vec<TreeNode>,
    order: Vec<u32>,
}

impl BoxTree {
    pub fn build(boxes: &[Aabb]) -> Self {
        let mut order: Vec<u32> = (0..boxes.len() as u32)
            .filter(|&i| !boxes[i as usize].is_empty())
            .collect();
        let mut nodes = Vec::with_capacity(order.len() / 2 + 1);
        if !order.is_empty() {
            let n = order.len();
            nodes.push(TreeNode {
                b: Aabb::EMPTY,
                first: 0,
                count: 0,
                leaves: 0,
            });
            tree_split(boxes, &mut order, &mut nodes, 0, 0, n);
        }
        Self { nodes, order }
    }

    pub fn visit(&self, mut f: impl FnMut(TreeEv) -> bool) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0u32];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i as usize];
            if !f(TreeEv::Node(&n.b, n.leaves)) {
                continue;
            }
            if n.count > 0 {
                for &k in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    f(TreeEv::Leaf(k));
                }
            } else {
                stack.push(n.first);
                stack.push(n.first + 1);
            }
        }
    }
}

fn tree_split(
    boxes: &[Aabb],
    order: &mut [u32],
    nodes: &mut Vec<TreeNode>,
    at: usize,
    lo: usize,
    hi: usize,
) {
    let (mut b, mut c) = (Aabb::EMPTY, Aabb::EMPTY);
    for &k in &order[lo..hi] {
        let x = &boxes[k as usize];
        b.union(x);
        c.grow((x.min + x.max) * 0.5);
    }
    nodes[at].b = b;
    nodes[at].leaves = (hi - lo) as u32;
    if hi - lo <= TREE_LEAF {
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
    order[lo..hi].select_nth_unstable_by(mid - lo, |p, q| key(p).total_cmp(&key(q)));
    let left = nodes.len();
    let empty = TreeNode {
        b: Aabb::EMPTY,
        first: 0,
        count: 0,
        leaves: 0,
    };
    nodes.extend([empty, empty]);
    nodes[at].first = left as u32;
    nodes[at].count = 0;
    tree_split(boxes, order, nodes, left, lo, mid);
    tree_split(boxes, order, nodes, left + 1, mid, hi);
}

/// Depth pyramid of a captured frame (reversed depth: nearer — larger). A tile stores the farthest
/// value under it, so holes in occluders yield "not occluded".
#[derive(Clone, Debug)]
pub struct HiZ {
    mips: Vec<(usize, usize, Vec<f32>)>,
    vp: Mat4,
    inv: Mat4,
    eye: Vec3,
    ortho: bool,
}

/// The eye moved by more than this fraction of the distance to the occluder — the snapshot is unusable here.
const HIZ_PARALLAX: f32 = 0.02;
/// Depth margin: the box must be clearly farther than the occluder.
const HIZ_MARGIN: f32 = 0.995;

impl HiZ {
    /// `tiles` — tiles by rows top to bottom, of size `w`×`h`.
    pub fn new(w: usize, h: usize, tiles: Vec<f32>, vp: Mat4, eye: Vec3) -> Self {
        let mut mips = Vec::new();
        let (mut cw, mut ch) = (w.max(1), h.max(1));
        let mut cur = tiles;
        cur.resize(cw * ch, 0.0);
        while cw > 1 || ch > 1 {
            let (nw, nh) = (cw.div_ceil(2), ch.div_ceil(2));
            let mut next = vec![f32::INFINITY; nw * nh];
            for y in 0..ch {
                for x in 0..cw {
                    let k = (y / 2) * nw + x / 2;
                    next[k] = next[k].min(cur[y * cw + x]);
                }
            }
            mips.push((cw, ch, cur));
            cur = next;
            cw = nw;
            ch = nh;
        }
        mips.push((cw, ch, cur));
        Self {
            mips,
            vp,
            inv: vp.inverse(),
            eye,
            ortho: vp.row(3).truncate().length() < 1e-6,
        }
    }

    /// Whether the snapshot is usable for frame `vp`: in orthographic projection only with the same
    /// camera (eye shift there says nothing about a view change).
    pub fn usable(&self, vp: &Mat4) -> bool {
        !self.ortho || self.same_view(vp)
    }

    /// Captured from exactly this camera: only such a snapshot is acceptable when idle, otherwise
    /// an old view could hide visible content for good.
    pub fn same_view(&self, vp: &Mat4) -> bool {
        self.vp.abs_diff_eq(*vp, 1e-5)
    }

    /// The box is entirely behind the captured depth. Checked in the snapshot camera; with a noticeable
    /// eye shift relative to the distance to the occluder the answer is always "no".
    pub fn occluded(&self, b: &Aabb, eye_now: Vec3) -> bool {
        if b.is_empty() {
            return false;
        }
        let (mut lo_x, mut lo_y) = (f32::INFINITY, f32::INFINITY);
        let (mut hi_x, mut hi_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        let mut near = 0.0f32;
        for k in 0..8 {
            let p = Vec3::new(
                if k & 1 == 0 { b.min.x } else { b.max.x },
                if k & 2 == 0 { b.min.y } else { b.max.y },
                if k & 4 == 0 { b.min.z } else { b.max.z },
            );
            let q = self.vp * p.extend(1.0);
            if q.w <= 1e-6 {
                return false;
            }
            let (x, y, z) = (q.x / q.w, q.y / q.w, q.z / q.w);
            lo_x = lo_x.min(x);
            hi_x = hi_x.max(x);
            lo_y = lo_y.min(y);
            hi_y = hi_y.max(y);
            near = near.max(z);
        }
        // part of the box is outside the captured frame: nothing is known about it
        if lo_x < -1.0 || hi_x > 1.0 || lo_y < -1.0 || hi_y > 1.0 {
            return false;
        }
        let (w, h) = (self.mips[0].0 as f32, self.mips[0].1 as f32);
        let x0 = (lo_x * 0.5 + 0.5) * w;
        let x1 = (hi_x * 0.5 + 0.5) * w;
        let y0 = (0.5 - hi_y * 0.5) * h;
        let y1 = (0.5 - lo_y * 0.5) * h;
        let span = (x1 - x0).max(y1 - y0).max(1.0);
        let level = (span.log2().ceil() as usize).min(self.mips.len() - 1);
        let (mw, mh, tiles) = &self.mips[level];
        let s = (1usize << level) as f32;
        let cell = |v: f32, n: usize| ((v / s).floor().max(0.0) as usize).min(n - 1);
        let (mut far, mut occ) = (f32::INFINITY, 0.0f32);
        for y in cell(y0, *mh)..=cell(y1, *mh) {
            for x in cell(x0, *mw)..=cell(x1, *mw) {
                far = far.min(tiles[y * mw + x]);
                occ = occ.max(tiles[y * mw + x]);
            }
        }
        if !far.is_finite() || near >= far * HIZ_MARGIN {
            return false;
        }
        // the shift is measured against the occluder: a wall a metre from the eye "slides away" after half a metre
        let q = self
            .inv
            .project_point3(Vec3::new((lo_x + hi_x) * 0.5, (lo_y + hi_y) * 0.5, occ));
        eye_now.distance(self.eye) <= HIZ_PARALLAX * q.distance(self.eye)
    }
}

/// Chunk level of detail in this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    Skip,
    Box,
    Level(usize),
}

/// The coarsest level whose pixel error is within tolerance (`errors[0]` is 0).
pub fn pick_level(errors: &[f32], px_per_m: f32, tol_px: f32) -> usize {
    let mut k = 0;
    for (i, e) in errors.iter().enumerate() {
        if e * px_per_m <= tol_px {
            k = i;
        }
    }
    k
}

/// The level a placement of `px` pixels is drawn at: the first whose least size it reaches, the
/// last when it reaches none.
pub fn screen_level(least: &[f32], px: f32) -> usize {
    least.iter().position(|&l| px >= l).unwrap_or(least.len().saturating_sub(1))
}

/// Pixels per metre at the box point nearest the eye: a long chunk next to the camera must not
/// count as distant by its average size. Constant in orthographic projection.
pub fn px_per_m_near(vp: &Mat4, b: &Aabb, eye: Vec3, px: (f32, f32)) -> f32 {
    let row = |k: usize| vp.row(k);
    let scale = (row(0).truncate().length() * px.0).max(row(1).truncate().length() * px.1) * 0.5;
    let w = row(3).truncate().length();
    if w < 1e-6 {
        return scale / row(3).w.abs().max(1e-6);
    }
    let d = eye.clamp(b.min, b.max).distance(eye);
    if d < 1e-3 {
        return f32::INFINITY;
    }
    scale / (d * w)
}

/// A chunk with its own meshes (`size` — screen size of the chunk, pixels). `lean` — a large model
/// and the frame is not idle yet: when idle everything is drawn exactly.
pub fn mesh_detail(size: f32, glass: bool, lean: bool, moving: bool, c: &PerfConfig) -> Detail {
    if !lean {
        return Detail::Level(0);
    }
    let k = if moving { c.moving_scale } else { 1.0 };
    if size < c.impostor_px * k {
        return if glass { Detail::Skip } else { Detail::Box };
    }
    Detail::Level(0)
}

/// A placement chunk of a large model; `per_m` — pixels per metre at the nearest point of the chunk. When idle,
/// no boxes and no culling: only a level indistinguishable from exact; glass without substitutions.
pub fn inst_detail(
    per_m: f32,
    proto_diag: f32,
    errors: &[f32],
    glass: bool,
    lean: bool,
    moving: bool,
    c: &PerfConfig,
) -> Detail {
    if !per_m.is_finite() {
        return Detail::Level(0);
    }
    if !lean {
        return if glass {
            Detail::Level(0)
        } else {
            Detail::Level(pick_level(errors, per_m, c.tol_px))
        };
    }
    let k = if moving { c.moving_scale } else { 1.0 };
    let one = per_m * proto_diag;
    if one < c.inst_skip_px * k {
        return Detail::Skip;
    }
    if one < c.inst_box_px * k {
        return if glass { Detail::Skip } else { Detail::Box };
    }
    if glass {
        return Detail::Level(0);
    }
    Detail::Level(pick_level(errors, per_m, c.tol_px * k))
}

/// The finest resident level from `from` (below `last`, the box) that the budget still holds:
/// placements are visited nearest first, so near ones keep their level and far ones fall to coarser
/// ones once the triangles run out. `None` — not even the coarsest fits.
pub fn fit_level(
    spend: &mut Spend,
    from: usize,
    last: usize,
    tris: impl Fn(usize) -> u64,
    has: impl Fn(usize) -> bool,
) -> Option<usize> {
    (from..last).filter(|&k| has(k)).find(|&k| spend.take(1, tris(k)))
}

/// The exact prototype level — an approximation coarser than the tolerance (`Instanced::approx`).
pub fn is_coarse(approx: f32, per_m: f32, c: &PerfConfig) -> bool {
    approx > 0.0 && approx * per_m > c.tol_px
}

/// Counters of the last frame (overlay, benchmarks).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    /// Frame recording on the CPU, ms.
    pub cpu_ms: f32,
    pub chunks: usize,
    pub drawn: usize,
    pub culled: usize,
    pub lod_skipped: usize,
    pub occluded: usize,
    pub boxes: usize,
    /// Impostor rows dropped because the buffer limit was reached.
    pub impostors_dropped: usize,
    /// Did not fit the frame budget.
    pub dropped: usize,
    pub draws: u64,
    pub triangles: u64,
    /// Total triangles in the model.
    pub total_triangles: u64,
    pub gpu_bytes: u64,
    pub gpu_budget: u64,
    pub uploaded_bytes: u64,
    /// Chunks and meshes queued for build and upload.
    pub pending: usize,
    pub evicted: usize,
    /// Did not fit in GPU memory.
    pub denied: usize,
    /// The memory limit was lowered after an allocation failure.
    pub memory_limited: bool,
    /// Groups shown as an approximation until the exact mesh is read.
    pub coarse: usize,
    /// The model is above the `big_tris` threshold.
    pub big: bool,
    pub moving: bool,
    pub occlusion: bool,
    /// The frame is still being refined: further frames are needed even with a still camera.
    pub refining: bool,
    /// Refinement the next frame continues by itself (uploads held by the frame budget, the
    /// motion ladder): worth a timed frame. Builds in flight wake the window instead.
    pub polling: bool,
}

impl FrameStats {
    /// The frame is idle but not everything visible is shown: how many chunks and why
    /// (frame limit, memory). While refinement is under way this is not final.
    pub fn held_back(&self) -> (usize, usize) {
        if self.moving || self.refining {
            return (0, 0);
        }
        (self.dropped, self.denied)
    }
}

static PUBLISHED: Mutex<Option<FrameStats>> = Mutex::new(None);
/// Keys of groups that need the exact mesh in this frame: renderer frame → read from the model.
static COARSE_WANTED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

pub fn publish_coarse(keys: Vec<u64>) {
    if let Ok(mut p) = COARSE_WANTED.lock() {
        *p = keys;
    }
}

/// Groups that need the exact mesh (last frame).
pub fn coarse_wanted() -> Vec<u64> {
    COARSE_WANTED.lock().map(|p| p.clone()).unwrap_or_default()
}

pub fn publish(s: FrameStats) {
    if let Ok(mut p) = PUBLISHED.lock() {
        *p = Some(s);
    }
}

/// Counters of the last recorded frame: the renderer lives in the paint resources, so the viewport
/// and overlay do not need to lock it.
pub fn published() -> FrameStats {
    PUBLISHED.lock().ok().and_then(|p| *p).unwrap_or_default()
}

type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

/// What a finished background build calls so the window draws the frame that takes it: the
/// view does not poll while builds run.
pub fn set_waker(f: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut w) = WAKER.lock() {
        *w = Some(std::sync::Arc::new(f));
    }
}

#[cfg(feature = "gpu")]
pub(crate) fn wake() {
    let w = WAKER.lock().ok().and_then(|w| w.clone());
    if let Some(w) = w {
        w();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plant_drops_a_level_as_it_shrinks_on_the_screen() {
        let least = [120.0, 50.0, 20.0, 0.0];
        let at = |px| screen_level(&least, px);
        assert_eq!((at(f32::INFINITY), at(300.0), at(120.0)), (0, 0, 0));
        assert_eq!((at(119.0), at(50.0), at(49.0), at(20.0), at(19.0), at(0.5)), (1, 1, 2, 2, 3, 3));
        // a ladder of two levels still ends on its last
        assert_eq!(screen_level(&[30.0, 0.0], 1.0), 1);
        assert_eq!(screen_level(&[], 1.0), 0);
    }

    #[test]
    fn a_thousand_mixed_trees_fit_the_budget_and_the_near_ones_stay_fine() {
        // Fine / Medium / Coarse triangles of a tree; every fourth tree is a small species
        let tris = |kind: usize, l: usize| [[4400u64, 1500, 160], [2600, 900, 120]][kind][l];
        let c = PerfConfig::default();
        let eye = Vec3::new(160.0, -30.0, 12.0);
        // 32 x 32 grid with 10 m spacing, the camera at the edge of the site
        let mut trees: Vec<(f32, usize)> = (0..1000)
            .map(|k| {
                let p = Vec3::new((k % 32) as f32 * 10.0, (k / 32) as f32 * 10.0, 0.0);
                (p.distance(eye), usize::from(k % 4 == 0))
            })
            .collect();
        let all_fine: u64 = trees.iter().map(|t| tris(t.1, 0)).sum();
        assert!(all_fine > c.big_tris, "{all_fine}: all fine must overflow, or the test proves nothing");
        trees.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut spend = Spend::new(u64::MAX, c.big_tris);
        let mut levels = vec![];
        for &(_, kind) in &trees {
            let l = fit_level(&mut spend, 0, 3, |k| tris(kind, k), |_| true);
            // nearest first, so when the frame's triangles run out the farthest trees get no mesh
            // level and are drawn as their boxes (level 3, the last): that is the budget working
            levels.push(l.unwrap_or(3));
        }
        let boxes = levels.iter().filter(|&&l| l == 3).count();
        assert!(boxes * 3 <= levels.len(), "{boxes} of {} trees fell to boxes", levels.len());
        assert!(spend.tris <= c.big_tris, "{} tris over the {} budget", spend.tris, c.big_tris);
        let near = levels.iter().take_while(|&&l| l == 0).count();
        assert!(near >= 300, "only {near} near trees stay fine");
        assert!(levels[..100].iter().all(|&l| l == 0), "the nearest hundred must be fine");
        assert!(levels[999] > 0, "the farthest must have fallen");
    }

    #[test]
    fn budget_ladder_doubles_until_full() {
        let mut m = Motion::default();
        let vp = Mat4::IDENTITY;
        m.update(&vp);
        assert!(m.moving());
        assert_eq!(m.budget(100, 1000), 100);
        m.update(&vp);
        assert!(!m.moving());
        assert_eq!(m.budget(100, 1000), 200);
        m.update(&vp);
        assert_eq!(m.budget(100, 1000), 400);
        for _ in 0..LADDER {
            m.update(&vp);
        }
        assert!(m.settled());
        assert_eq!(m.budget(100, 1000), 1000);
        m.update(&Mat4::from_translation(Vec3::X));
        assert!(m.moving());
        assert_eq!(m.budget(100, 1000), 100);
    }

    #[test]
    fn spend_lets_the_first_through_and_stops_at_limits() {
        let mut s = Spend::new(2, 100);
        assert!(s.take(1, 5000), "the largest chunk is never dropped");
        assert!(!s.take(1, 1), "triangles are already over");
        let mut s = Spend::new(2, 100);
        assert!(s.take(1, 10));
        assert!(s.take(1, 10));
        assert!(!s.take(1, 10), "draw limit");
        assert_eq!((s.draws, s.tris), (2, 20));
    }

    #[test]
    fn evicts_oldest_and_keeps_what_this_frame_used() {
        let mut r = Residency::new(100);
        r.begin_frame();
        r.insert(1, 40);
        r.begin_frame();
        r.insert(2, 40);
        r.begin_frame();
        r.touch(1);
        r.insert(3, 40);
        assert_eq!(r.used(), 120);
        assert_eq!(r.evict(), vec![2], "1 and 3 are used in this frame");
        assert_eq!(r.used(), 80);
        assert!(!r.contains(2) && r.contains(1) && r.contains(3));
    }

    #[test]
    fn stays_over_budget_rather_than_evict_the_visible() {
        let mut r = Residency::new(10);
        r.begin_frame();
        r.insert(1, 20);
        assert!(r.evict().is_empty());
        assert_eq!(r.used(), 20);
        r.begin_frame();
        assert_eq!(r.evict(), vec![1]);
        assert_eq!(r.used(), 0);
    }

    #[test]
    fn reserve_respects_this_frame_unless_the_newcomer_is_larger() {
        let mut r = Residency::new(100);
        r.begin_frame();
        r.insert(1, 60);
        r.begin_frame();
        r.insert(2, 40);
        assert!(r.fits(50, 0.0), "key 1 is stale");
        assert!(!r.fits(150, 0.0), "more than the budget");
        assert_eq!(r.reserve(150, 0.0), None);
        assert_eq!(r.used(), 100, "a refusal evicts nothing");
        assert_eq!(r.reserve(50, 0.0), Some(vec![1]));
        assert!(!r.fits(90, 1e6), "key 2 has no rank, so it cannot go");
        assert!(r.fits(60, 0.0), "still free");
    }

    #[test]
    fn a_large_near_chunk_displaces_small_far_ones() {
        let mut r = Residency::new(100);
        r.begin_frame();
        for k in 1..=4u64 {
            r.insert(k, 25);
            r.set_rank(k, k as f32);
        }
        assert!(!r.fits(30, 1.0), "nothing is smaller than rank 1");
        assert_eq!(
            r.reserve(30, 3.0),
            Some(vec![1, 2]),
            "smallest first, up to the need"
        );
        assert_eq!(r.used(), 50);
        assert!(r.fits(50, 0.0), "the two freed slots");
        assert!(!r.fits(60, 3.0), "rank 3 and 4 stay for equal or smaller");
        assert!(r.fits(70, 3.5), "rank 3 can go for a larger one");
    }

    #[test]
    fn reserve_stops_at_the_protected_when_over_budget() {
        let mut r = Residency::new(10);
        r.begin_frame();
        r.insert(1, 20);
        r.set_rank(1, 5.0);
        assert_eq!(
            r.reserve(1, 1.0),
            None,
            "over budget does not make the visible fair game"
        );
        assert!(r.contains(1));
        assert_eq!(r.used(), 20);
    }

    #[test]
    fn budgets_follow_the_adapter_class() {
        let base = PerfConfig::default();
        let (d, i, c) = (
            base.for_class(GpuClass::Discrete),
            base.for_class(GpuClass::Integrated),
            base.for_class(GpuClass::Cpu),
        );
        assert!(d.draws_still > i.draws_still && i.draws_still > c.draws_still);
        assert!(d.tris_still > i.tris_still && i.tris_still > c.tris_still);
        assert_eq!(
            GpuClass::of(wgpu::DeviceType::DiscreteGpu),
            GpuClass::Discrete
        );
        assert_eq!(GpuClass::of(wgpu::DeviceType::Cpu), GpuClass::Cpu);
        assert_eq!(GpuClass::of(wgpu::DeviceType::Other), GpuClass::Integrated);
        assert_eq!(budget_after_oom(1000 * MB, 3500 * MB), 750 * MB);
        assert_eq!(
            budget_after_oom(10 * MB, 3500 * MB),
            256 * MB,
            "a floor for what must stay"
        );
    }

    #[test]
    fn replace_keeps_the_sum_right() {
        let mut r = Residency::new(1000);
        r.begin_frame();
        r.insert(7, 100);
        r.insert(7, 30);
        assert_eq!(r.used(), 30);
        assert_eq!(r.len(), 1);
        r.remove(7);
        r.remove(7);
        assert_eq!(r.used(), 0);
        assert!(r.is_empty());
    }

    fn cube(at: f32) -> Aabb {
        Aabb {
            min: Vec3::splat(at),
            max: Vec3::splat(at + 1.0),
        }
    }

    #[test]
    fn tree_skips_rejected_subtrees_and_reaches_all_leaves() {
        let boxes: Vec<Aabb> = (0..100).map(|i| cube(i as f32 * 3.0)).collect();
        let t = BoxTree::build(&boxes);
        let mut leaves = vec![];
        t.visit(|e| match e {
            TreeEv::Node(..) => true,
            TreeEv::Leaf(i) => {
                leaves.push(i);
                true
            }
        });
        leaves.sort_unstable();
        assert_eq!(leaves, (0..100).collect::<Vec<u32>>());
        let (mut nodes, mut seen) = (0, 0);
        t.visit(|e| match e {
            TreeEv::Node(b, n) => {
                nodes += 1;
                assert!(n > 0);
                b.min.x < 10.0
            }
            TreeEv::Leaf(_) => {
                seen += 1;
                true
            }
        });
        assert!(seen < 30, "only the near part is walked: {seen}");
        assert!(nodes < 40, "{nodes}");
        assert!(!format!("{:?}", BoxTree::build(&[Aabb::EMPTY])).is_empty());
    }

    fn camera() -> (Mat4, Vec3) {
        let eye = Vec3::new(0.0, -20.0, 0.0);
        let vp = Mat4::perspective_infinite_reverse_rh(1.0, 1.0, 0.1)
            * Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z);
        (vp, eye)
    }

    /// Flat depth wall: the point (0, y, 0) on the view axis in the camera's reversed depth.
    fn wall_depth(vp: &Mat4, y: f32) -> f32 {
        let q = *vp * Vec3::new(0.0, y, 0.0).extend(1.0);
        q.z / q.w
    }

    fn at(c: Vec3, r: f32) -> Aabb {
        Aabb {
            min: c - Vec3::splat(r),
            max: c + Vec3::splat(r),
        }
    }

    #[test]
    fn hiz_culls_only_what_is_behind_the_depth() {
        let (vp, eye) = camera();
        let d = wall_depth(&vp, 0.0);
        let z = HiZ::new(64, 64, vec![d; 64 * 64], vp, eye);
        assert!(z.usable(&vp));
        assert!(
            z.occluded(&at(Vec3::new(0.0, 20.0, 0.0), 1.0), eye),
            "far behind"
        );
        assert!(
            !z.occluded(&at(Vec3::new(0.0, -5.0, 0.0), 1.0), eye),
            "in front"
        );
        assert!(
            !z.occluded(&at(Vec3::new(0.0, 0.0, 0.0), 1.0), eye),
            "crosses the wall"
        );
        assert!(
            !z.occluded(&at(Vec3::new(0.0, -30.0, 0.0), 1.0), eye),
            "behind the eye"
        );
        assert!(
            !z.occluded(&at(Vec3::new(900.0, 20.0, 0.0), 1.0), eye),
            "out of the captured frame"
        );
        assert!(!z.occluded(&Aabb::EMPTY, eye));
    }

    #[test]
    fn hiz_hole_in_the_depth_means_not_occluded() {
        let (vp, eye) = camera();
        let d = wall_depth(&vp, 0.0);
        let mut tiles = vec![d; 64 * 64];
        // a hole in the middle of the frame: one tile is unfilled (background — depth 0)
        tiles[32 * 64 + 32] = 0.0;
        let z = HiZ::new(64, 64, tiles, vp, eye);
        assert!(!z.occluded(&at(Vec3::new(0.0, 20.0, 0.0), 1.0), eye));
        assert!(
            z.occluded(&at(Vec3::new(14.0, 20.0, 14.0), 0.5), eye),
            "the hole is elsewhere"
        );
    }

    #[test]
    fn hiz_rejects_a_moved_eye_for_near_boxes() {
        let (vp, eye) = camera();
        let d = wall_depth(&vp, 0.0);
        let z = HiZ::new(16, 16, vec![d; 256], vp, eye);
        let b = at(Vec3::new(0.0, 20.0, 0.0), 1.0);
        assert!(z.occluded(&b, eye + Vec3::X * 0.1), "small shift at 40 m");
        assert!(
            !z.occluded(&b, eye + Vec3::X * 2.0),
            "parallax is too large"
        );
    }

    #[test]
    fn hiz_shift_is_measured_from_the_occluder_not_the_box() {
        let (vp, eye) = camera();
        let d = wall_depth(&vp, eye.y + 1.0);
        let z = HiZ::new(16, 16, vec![d; 256], vp, eye);
        let far = at(Vec3::new(0.0, eye.y + 50.0, 0.0), 1.0);
        assert!(z.occluded(&far, eye), "same camera, wall at 1 m");
        assert!(
            z.occluded(&far, eye + Vec3::Y * 0.01),
            "a centimetre is nothing"
        );
        assert!(
            !z.occluded(&far, eye + Vec3::Y * 1.0),
            "the eye reached the wall, what is behind it must not be cut off"
        );
    }

    #[test]
    fn hiz_same_view_is_exact() {
        let (vp, eye) = camera();
        let z = HiZ::new(4, 4, vec![0.5; 16], vp, eye);
        assert!(z.same_view(&vp));
        assert!(!z.same_view(&(vp * Mat4::from_rotation_z(0.2))));
    }

    #[test]
    fn hiz_ortho_needs_the_same_camera() {
        let view = Mat4::look_at_rh(Vec3::new(0.0, -20.0, 0.0), Vec3::ZERO, Vec3::Z);
        let vp = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 0.1) * view;
        let z = HiZ::new(4, 4, vec![0.5; 16], vp, Vec3::new(0.0, -20.0, 0.0));
        assert!(z.usable(&vp));
        assert!(!z.usable(&(vp * Mat4::from_rotation_z(0.2))));
        let (persp, eye) = camera();
        let p = HiZ::new(4, 4, vec![0.5; 16], persp, eye);
        assert!(p.usable(&(persp * Mat4::from_rotation_z(0.2))));
    }

    #[test]
    fn pyramid_keeps_the_farthest_value() {
        let (vp, eye) = camera();
        let mut t = vec![0.5f32; 9];
        t[4] = 0.1;
        let z = HiZ::new(3, 3, t, vp, eye);
        assert_eq!(z.mips.last().unwrap().2, vec![0.1]);
        assert_eq!((z.mips[1].0, z.mips[1].1), (2, 2));
        assert_eq!(z.mips[1].2[0], 0.1);
        assert_eq!(z.mips[1].2[3], 0.5, "odd sizes round up");
    }

    #[test]
    fn detail_by_screen_size() {
        let c = PerfConfig::default();
        assert_eq!(
            mesh_detail(0.1, false, false, true, &c),
            Detail::Level(0),
            "not lean: exact"
        );
        assert_eq!(mesh_detail(4.0, false, true, false, &c), Detail::Box);
        assert_eq!(
            mesh_detail(4.0, true, true, false, &c),
            Detail::Skip,
            "no boxes for glass"
        );
        assert_eq!(mesh_detail(10.0, false, true, false, &c), Detail::Level(0));
        assert_eq!(
            mesh_detail(10.0, false, true, true, &c),
            Detail::Box,
            "motion raises the bar"
        );
        let e = [0.0, 0.01, 0.1];
        // 10 px/m, a 1 m mesh — 10 pixels; tolerance 0.5 px
        assert_eq!(
            inst_detail(10.0, 1.0, &e, false, true, false, &c),
            Detail::Level(1)
        );
        assert_eq!(
            inst_detail(4.0, 1.0, &e, false, true, false, &c),
            Detail::Box
        );
        assert_eq!(
            inst_detail(0.5, 1.0, &e, false, true, false, &c),
            Detail::Skip
        );
        assert_eq!(
            inst_detail(1000.0, 1.0, &e, false, true, false, &c),
            Detail::Level(0)
        );
        assert_eq!(
            inst_detail(10.0, 1.0, &e, true, true, false, &c),
            Detail::Level(0),
            "glass stays exact"
        );
        assert_eq!(
            inst_detail(f32::INFINITY, 1.0, &e, false, true, true, &c),
            Detail::Level(0)
        );
    }

    #[test]
    fn at_rest_nothing_is_a_box_or_skipped() {
        let c = PerfConfig::default();
        let e = [0.0, 0.01, 0.1];
        assert_eq!(mesh_detail(0.01, false, false, false, &c), Detail::Level(0));
        assert_eq!(
            mesh_detail(0.01, true, false, false, &c),
            Detail::Level(0),
            "far glass stays"
        );
        for per_m in [0.001, 0.5, 4.0, 10.0, 1000.0] {
            assert!(matches!(
                inst_detail(per_m, 1.0, &e, false, false, false, &c),
                Detail::Level(_)
            ));
            assert_eq!(
                inst_detail(per_m, 1.0, &e, true, false, false, &c),
                Detail::Level(0)
            );
        }
        // a coarse level when idle only within tolerance
        assert_eq!(
            inst_detail(100.0, 1.0, &e, false, false, false, &c),
            Detail::Level(0)
        );
        assert_eq!(
            inst_detail(0.5, 1.0, &e, false, false, false, &c),
            Detail::Level(2)
        );
    }

    #[test]
    fn long_chunk_next_to_the_camera_counts_as_near() {
        let c = PerfConfig::default();
        let (vp, eye) = camera();
        let px = (1500.0, 1500.0);
        // a row of columns along the view: a 240 m chunk, the near edge 2 m from the eye
        let b = Aabb {
            min: Vec3::new(-0.5, -18.0, -2.0),
            max: Vec3::new(0.5, 222.0, 2.0),
        };
        let per_m = px_per_m_near(&vp, &b, eye, px);
        assert!(per_m > 100.0, "{per_m} px/m, not the 6 of the whole chunk");
        let e = [0.0, 0.01, 0.16];
        let d = inst_detail(per_m, 0.5, &e, false, true, false, &c);
        assert_eq!(
            d,
            Detail::Level(0),
            "a column 2 m away is neither a box nor gone"
        );
        assert_eq!(px_per_m_near(&vp, &at(eye, 1.0), eye, px), f32::INFINITY);
    }

    #[test]
    fn ortho_scale_does_not_depend_on_distance() {
        let eye = Vec3::new(0.0, -20.0, 0.0);
        let view = Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Z);
        let vp = Mat4::orthographic_rh(-10.0, 10.0, -10.0, 10.0, 100.0, 0.1) * view;
        let (near, far) = (at(Vec3::ZERO, 1.0), at(Vec3::new(0.0, 60.0, 0.0), 1.0));
        let (a, b) = (
            px_per_m_near(&vp, &near, eye, (1000.0, 1000.0)),
            px_per_m_near(&vp, &far, eye, (1000.0, 1000.0)),
        );
        assert!((a - 50.0).abs() < 1e-3, "{a}");
        assert_eq!(a, b);
    }

    #[test]
    fn held_back_only_at_rest() {
        let mut s = FrameStats {
            dropped: 3,
            denied: 2,
            ..Default::default()
        };
        assert_eq!(s.held_back(), (3, 2));
        s.refining = true;
        assert_eq!(s.held_back(), (0, 0));
        s.refining = false;
        s.moving = true;
        assert_eq!(s.held_back(), (0, 0));
    }

    #[test]
    fn level_is_the_coarsest_within_tolerance() {
        let e = [0.0, 0.01, 0.1];
        assert_eq!(pick_level(&e, 1000.0, 1.0), 0);
        assert_eq!(pick_level(&e, 50.0, 1.0), 1);
        assert_eq!(pick_level(&e, 5.0, 1.0), 2);
        assert_eq!(pick_level(&[0.0], 1e-3, 1.0), 0);
    }

    #[test]
    fn coarse_requests_are_published() {
        let mut m = Motion::default();
        m.assume_rest();
        m.update(&Mat4::IDENTITY);
        assert!(m.settled(), "a one-off frame is a resting frame");
        m.update(&Mat4::ZERO);
        assert!(m.moving(), "only one frame");
        publish_coarse(vec![4, 5]);
        assert_eq!(coarse_wanted(), vec![4, 5]);
        assert!(is_coarse(0.1, 100.0, &PerfConfig::default()));
        assert!(!is_coarse(0.0, 100.0, &PerfConfig::default()));
        assert!(!is_coarse(0.001, 100.0, &PerfConfig::default()));
    }

    #[test]
    fn stats_are_published() {
        publish(FrameStats {
            draws: 7,
            ..Default::default()
        });
        assert_eq!(published().draws, 7);
    }
}
