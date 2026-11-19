// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! City layer: a federation of many projects drawn through the chunk renderer. Per update it picks
//! a cut through each project's proxy tree by screen-space error (frustum and hi-Z: hidden nodes
//! are never refined), streams full unit content within one memory budget and composes a `Scene`
//! only when the cut changes. Contract and budgets: `docs/CITY1.md`.

#[cfg(feature = "gpu")]
pub mod bench;
pub mod cook;
pub mod gen;
pub mod hlod;
pub mod pool;
pub mod project;
#[cfg(test)]
mod tests;

use crate::chunks::screen_size;
use crate::perf::{px_per_m_near, HiZ, Motion, Residency};
use crate::scene::{tag_rgb, Aabb, Scene, SceneMesh};
use cook::{Cooked, Kid};
use glam::{Mat4, Vec3};
use pool::{Done, Job, Pool, Spec};
use project::{mix, offset, Placed};
pub use project::{Content, Project, Unit};
use std::collections::HashMap;
use std::sync::Arc;

/// Coarsen below this share of the rest tolerance (refine above the tolerance): no flicker.
pub const HYST: f32 = 0.75;
/// Cooking a project outranks any load: a box is a worse stand-in than a proxy.
const COOK_RANK: f32 = 1.0e7;
/// A unit refused by the budget is asked again only when its screen error grows this much.
const RETRY_GROWTH: f32 = 1.25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CityConfig {
    /// Screen-space error at rest, px.
    pub tol_px: f32,
    /// Screen-space error while the camera moves, px (refinement waits for rest).
    pub tol_px_moving: f32,
    /// Bytes of full unit content held at once.
    pub budget: u64,
    /// Worker threads; 0 — jobs run inside `update` (`jobs_per_update` at most).
    pub workers: usize,
    pub jobs_per_update: usize,
    /// Do not refine what the hi-Z snapshot shows hidden.
    pub occlusion: bool,
}

impl Default for CityConfig {
    fn default() -> Self {
        Self {
            tol_px: 1.5,
            tol_px_moving: 3.0,
            budget: 1 << 30,
            workers: 0,
            jobs_per_update: 4,
            occlusion: true,
        }
    }
}

/// One drawn piece of the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Piece {
    /// Bounds box of a project not cooked yet.
    Box(u32),
    /// Group proxy.
    Node(u32, u32),
    /// Unit proxy level.
    Level(u32, u32, u32),
    /// Full unit content.
    Full(u32, u32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CityStats {
    pub projects: usize,
    pub cooked: usize,
    pub units: usize,
    pub pieces: usize,
    pub full_units: usize,
    pub full_bytes: u64,
    pub budget: u64,
    pub proxy_bytes: u64,
    /// Triangles of the cut (instances expanded).
    pub triangles: usize,
    /// Jobs (cooks, loads the budget allows) wanted and not done yet.
    pub wanted: usize,
    /// Units wanted at full detail that the budget holds back (their proxy stays).
    pub held: usize,
    /// Nodes and units outside the frustum / behind the hi-Z snapshot this update.
    pub culled: usize,
    pub occluded: usize,
    pub moving: bool,
    /// The cut changed in this update (a new scene was returned).
    pub changed: bool,
    /// Jobs waiting or running.
    pub busy: usize,
}

struct Member {
    off: Vec3,
    base: u32,
    end: u32,
    bounds: Aabb,
    stand_in: (Arc<SceneMesh>, Aabb),
    cooked: Option<Cooked>,
    visible: bool,
}

pub struct CityView {
    pub cfg: CityConfig,
    anchor: [f64; 3],
    members: Vec<Member>,
    specs: Arc<Vec<Spec>>,
    pool: Option<Pool>,
    full: HashMap<u64, Placed>,
    res: Residency,
    motion: Motion,
    cut: Vec<Piece>,
    refused: HashMap<u64, f32>,
    stats: CityStats,
}

fn unit_key(p: u32, u: u32) -> u64 {
    (u64::from(p) << 32) | u64::from(u)
}

/// Per-update view of the camera.
struct Eye<'a> {
    vp: Mat4,
    eye: Vec3,
    px: (f32, f32),
    hiz: Option<&'a HiZ>,
    tau: f32,
    tau_rest: f32,
}

/// Jobs to ask for (capped, best first), all that could be asked, and loads the budget holds.
#[derive(Default)]
struct Ask {
    jobs: Vec<(f32, Job)>,
    open: usize,
    held: usize,
}

#[derive(Default)]
struct Out {
    cut: Vec<Piece>,
    wants: Vec<(f32, Job)>,
    touched: Vec<(u64, f32)>,
    culled: usize,
    occluded: usize,
}

impl Eye<'_> {
    fn visible(&self, b: &Aabb, out: &mut Out) -> bool {
        if screen_size(&self.vp, b, self.px).is_none() {
            out.culled += 1;
            return false;
        }
        if self.hiz.is_some_and(|z| z.occluded(b, self.eye)) {
            out.occluded += 1;
            return false;
        }
        true
    }
}

impl CityView {
    /// Projects placed around their common centre (the f64 anchor).
    pub fn new(projects: Vec<Arc<dyn Project>>, cfg: CityConfig) -> Self {
        let n = projects.len().max(1) as f64;
        let mut anchor = [0.0f64; 3];
        for p in &projects {
            let o = p.origin();
            (0..3).for_each(|k| anchor[k] += o[k] / n);
        }
        let mut members = Vec::with_capacity(projects.len());
        let mut specs = Vec::with_capacity(projects.len());
        let mut base = 0u32;
        for (pi, p) in projects.into_iter().enumerate() {
            let off = offset(p.origin(), anchor);
            let units = p.units();
            let mut bounds = Aabb::EMPTY;
            let mut end = 0u32;
            for u in &units {
                bounds.union(&Aabb {
                    min: u.bounds.min + off,
                    max: u.bounds.max + off,
                });
                end = end.max(u.elements.end);
            }
            let stand_in = crate::chunks::box_mesh(
                &if bounds.is_empty() {
                    Aabb {
                        min: off,
                        max: off,
                    }
                } else {
                    bounds
                },
                base,
                tag_rgb([170, 170, 166], false),
            );
            let sb = crate::scene::mesh_box(&stand_in);
            specs.push(Spec {
                project: p,
                off,
                base,
                salt: mix(pi as u64 + 1),
            });
            members.push(Member {
                off,
                base,
                end: base.saturating_add(end),
                bounds,
                stand_in: (Arc::new(stand_in), sb),
                cooked: None,
                visible: true,
            });
            base = base.saturating_add(end).saturating_add(1);
        }
        let specs = Arc::new(specs);
        let pool = (cfg.workers > 0).then(|| Pool::new(cfg.workers, specs.clone()));
        Self {
            cfg,
            anchor,
            res: Residency::new(cfg.budget),
            members,
            specs,
            pool,
            full: HashMap::new(),
            motion: Motion::default(),
            cut: vec![],
            refused: HashMap::new(),
            stats: CityStats::default(),
        }
    }

    /// World position of scene zero (all geometry is relative to it).
    pub fn anchor(&self) -> [f64; 3] {
        self.anchor
    }

    pub fn bounds(&self) -> Option<Aabb> {
        let mut b = Aabb::EMPTY;
        self.members.iter().for_each(|m| b.union(&m.bounds));
        (!b.is_empty()).then_some(b)
    }

    pub fn stats(&self) -> CityStats {
        self.stats
    }

    pub fn cut(&self) -> &[Piece] {
        &self.cut
    }

    /// Bounds of every unit of the cooked projects (anchor coordinates).
    pub fn unit_bounds(&self) -> Vec<Aabb> {
        self.members
            .iter()
            .filter_map(|m| m.cooked.as_ref())
            .flat_map(|c| c.units.iter().map(|u| u.bounds))
            .collect()
    }

    pub fn project_count(&self) -> usize {
        self.members.len()
    }

    /// Offset of project `p` from the anchor.
    pub fn project_offset(&self, p: usize) -> Vec3 {
        self.members[p].off
    }

    /// Project and its local element id of a global element.
    pub fn project_of(&self, element: u32) -> Option<(usize, u32)> {
        let i = self.members.partition_point(|m| m.base <= element);
        let p = i.checked_sub(1)?;
        let m = &self.members[p];
        (element <= m.end).then(|| (p, element - m.base))
    }

    /// Shows or hides a whole project (takes effect on the next update).
    pub fn set_visible(&mut self, p: usize, on: bool) {
        if let Some(m) = self.members.get_mut(p) {
            m.visible = on;
        }
    }

    pub fn set_budget(&mut self, bytes: u64) {
        self.cfg.budget = bytes;
        self.res.set_budget(bytes);
        self.refused.clear();
    }

    /// Camera at rest, everything cooked and nothing wanted that the budget allows: further
    /// updates with the same camera change nothing.
    pub fn settled(&self) -> bool {
        !self.stats.moving
            && self.stats.cooked == self.members.len()
            && self.stats.wanted == 0
            && self.stats.busy == 0
    }

    /// One update for a camera: `px` is the viewport, `hiz` the depth snapshot of a previous frame.
    /// Returns the new scene when the cut changed.
    pub fn update(
        &mut self,
        vp: Mat4,
        eye: Vec3,
        px: (u32, u32),
        hiz: Option<&HiZ>,
    ) -> Option<Scene> {
        self.motion.update(&vp);
        let moving = self.motion.moving();
        let e = Eye {
            vp,
            eye,
            px: (px.0 as f32, px.1 as f32),
            hiz: hiz.filter(|_| self.cfg.occlusion),
            tau: if moving {
                self.cfg.tol_px_moving.max(self.cfg.tol_px)
            } else {
                self.cfg.tol_px
            },
            tau_rest: self.cfg.tol_px,
        };
        let done = self.pool.as_ref().map(|p| p.poll()).unwrap_or_default();
        self.res.begin_frame();
        let mut out = self.walk(&e);
        let mut changed = false;
        for d in done {
            changed |= self.accept(d, &out);
        }
        for k in self.res.evict() {
            self.full.remove(&k);
            changed = true;
        }
        if changed {
            out = self.walk(&e);
        }
        let mut ask = self.requests(&out);
        if self.pool.is_none() && !ask.jobs.is_empty() {
            let mut any = false;
            for (_, j) in &ask.jobs {
                let d = pool::run(*j, &self.specs);
                any |= self.accept(d, &out);
            }
            if any {
                out = self.walk(&e);
            }
            ask = self.requests(&out);
        }
        let (wanted, held) = (ask.open, ask.held);
        if let Some(p) = &self.pool {
            p.set(ask.jobs);
        }
        let new = out.cut != self.cut;
        if new {
            self.cut = std::mem::take(&mut out.cut);
        }
        self.stats = CityStats {
            projects: self.members.len(),
            cooked: self.members.iter().filter(|m| m.cooked.is_some()).count(),
            units: self
                .members
                .iter()
                .filter_map(|m| m.cooked.as_ref())
                .map(|c| c.units.len())
                .sum(),
            pieces: self.cut.len(),
            full_units: self.full.len(),
            full_bytes: self.res.used(),
            budget: self.res.budget(),
            proxy_bytes: self
                .members
                .iter()
                .filter_map(|m| m.cooked.as_ref())
                .map(|c| c.proxy_bytes)
                .sum(),
            triangles: self.cut_triangles(),
            wanted,
            held,
            culled: out.culled,
            occluded: out.occluded,
            moving,
            changed: new,
            busy: self.pool.as_ref().map_or(0, |p| p.busy()),
        };
        new.then(|| self.compose())
    }

    /// Takes a finished job; true when the drawable state changed.
    fn accept(&mut self, d: Done, out: &Out) -> bool {
        match d {
            Done::Cooked(p, c) => {
                let m = &mut self.members[p as usize];
                if m.cooked.is_some() {
                    return false;
                }
                m.cooked = Some(*c);
                true
            }
            Done::Loaded(p, u, placed) => {
                let key = unit_key(p, u);
                if self.full.contains_key(&key) {
                    return false;
                }
                let rank = out
                    .wants
                    .iter()
                    .find(|(_, j)| *j == Job::Load(p, u))
                    .map_or(0.0, |w| w.0);
                match self.res.reserve(placed.bytes, rank) {
                    Some(evicted) => {
                        for k in evicted {
                            self.full.remove(&k);
                        }
                        self.res.insert(key, placed.bytes);
                        self.full.insert(key, placed);
                        self.refused.remove(&key);
                        true
                    }
                    None => {
                        self.refused.insert(key, rank);
                        false
                    }
                }
            }
        }
    }

    /// Jobs worth asking for now, best first: cooks, then loads the budget can take.
    fn requests(&mut self, out: &Out) -> Ask {
        let mut ask = Ask::default();
        for &(rank, j) in &out.wants {
            match j {
                Job::Cook(_) => ask.jobs.push((rank, j)),
                Job::Load(p, u) => {
                    let key = unit_key(p, u);
                    if self.full.contains_key(&key) {
                        continue;
                    }
                    let refused = self
                        .refused
                        .get(&key)
                        .is_some_and(|&r| rank <= r * RETRY_GROWTH);
                    let hint = self.members[p as usize]
                        .cooked
                        .as_ref()
                        .map_or(0, |c| c.units[u as usize].bytes_hint);
                    if refused || !self.res.fits(hint, rank) {
                        ask.held += 1;
                        continue;
                    }
                    ask.jobs.push((rank, j));
                }
            }
        }
        ask.open = ask.jobs.len();
        ask.jobs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let cap = match &self.pool {
            Some(_) => self.cfg.workers.max(1) * 2,
            None => self.cfg.jobs_per_update.max(1),
        };
        ask.jobs.truncate(cap);
        ask
    }

    /// The cut for this camera; touches resident full content in the budget.
    fn walk(&mut self, e: &Eye<'_>) -> Out {
        let mut out = Out::default();
        let full = &self.full;
        for (pi, m) in self.members.iter_mut().enumerate() {
            let p = pi as u32;
            if !m.visible {
                continue;
            }
            match m.cooked.as_mut() {
                None => {
                    out.cut.push(Piece::Box(p));
                    let size = screen_size(&e.vp, &m.bounds, e.px).unwrap_or(0.0).min(1e6);
                    out.wants.push((COOK_RANK + size, Job::Cook(p)));
                }
                Some(c) => {
                    let root = c.root;
                    walk_node(c, p, root, e, full, &mut out);
                }
            }
        }
        for &(k, r) in &out.touched {
            self.res.touch(k);
            self.res.set_rank(k, r);
        }
        out
    }

    fn cut_triangles(&self) -> usize {
        self.cut
            .iter()
            .map(|pc| match *pc {
                Piece::Box(_) => 12,
                Piece::Node(p, n) => self.cooked(p).nodes[n as usize].proxy.triangles,
                Piece::Level(p, u, k) => {
                    self.cooked(p).units[u as usize].levels[k as usize].triangles
                }
                Piece::Full(p, u) => self
                    .full
                    .get(&unit_key(p, u))
                    .map_or(0, |f| f.triangles),
            })
            .sum()
    }

    fn cooked(&self, p: u32) -> &Cooked {
        self.members[p as usize]
            .cooked
            .as_ref()
            .expect("a proxy piece belongs to a cooked project")
    }

    /// The scene of the current cut: shared meshes with cached boxes, instanced groups as loaded.
    pub fn compose(&self) -> Scene {
        let mut meshes = vec![];
        let mut boxes = vec![];
        let mut groups = vec![];
        let mut push = |list: &[(Arc<SceneMesh>, Aabb)]| {
            for (m, b) in list {
                meshes.push(m.clone());
                boxes.push(*b);
            }
        };
        for pc in &self.cut {
            match *pc {
                Piece::Box(p) => push(std::slice::from_ref(&self.members[p as usize].stand_in)),
                Piece::Node(p, n) => push(&self.cooked(p).nodes[n as usize].proxy.meshes),
                Piece::Level(p, u, k) => {
                    push(&self.cooked(p).units[u as usize].levels[k as usize].meshes)
                }
                Piece::Full(p, u) => {
                    if let Some(f) = self.full.get(&unit_key(p, u)) {
                        push(&f.meshes);
                        groups.extend(f.groups.iter().cloned());
                    }
                }
            }
        }
        Scene::from_parts(meshes, boxes).with_instanced(groups)
    }
}

fn walk_node(
    c: &mut Cooked,
    p: u32,
    n: u32,
    e: &Eye<'_>,
    full: &HashMap<u64, Placed>,
    out: &mut Out,
) {
    let node = &mut c.nodes[n as usize];
    if node.bounds.is_empty() {
        return;
    }
    let ppm = px_per_m_near(&e.vp, &node.bounds, e.eye, e.px);
    let err = node.proxy.error * ppm;
    let vis = e.visible(&node.bounds, out);
    let open = if node.open {
        err > HYST * e.tau_rest
    } else {
        vis && err > e.tau
    };
    let open = open && !node.kids.is_empty();
    node.open = open;
    if !open {
        if node.proxy.meshes.is_empty() {
            // nothing to stand in (empty group): its kids are not drawn either
            return;
        }
        out.cut.push(Piece::Node(p, n));
        return;
    }
    let kids = node.kids.clone();
    for k in kids {
        match k {
            Kid::Node(m) => walk_node(c, p, m, e, full, out),
            Kid::Unit(u) => walk_unit(c, p, u, e, full, out),
        }
    }
}

fn walk_unit(
    c: &mut Cooked,
    p: u32,
    u: u32,
    e: &Eye<'_>,
    full: &HashMap<u64, Placed>,
    out: &mut Out,
) {
    let unit = &mut c.units[u as usize];
    if unit.levels.is_empty() {
        return;
    }
    let ppm = px_per_m_near(&e.vp, &unit.bounds, e.eye, e.px);
    let vis = e.visible(&unit.bounds, out);
    let errs: Vec<f32> = unit.levels.iter().map(|l| l.error * ppm).collect();
    let n = errs.len() as i32;
    let err = |k: i32| errs[k as usize];
    let mut k = unit.cur.clamp(-1, n - 1);
    while k + 1 < n && err(k + 1) <= HYST * e.tau_rest {
        k += 1;
    }
    if vis {
        while k >= 0 && err(k) > e.tau {
            k -= 1;
        }
    }
    unit.cur = k;
    if k >= 0 {
        out.cut.push(Piece::Level(p, u, k as u32));
        return;
    }
    let key = unit_key(p, u);
    let rank = err(0).min(1e6);
    if full.contains_key(&key) {
        out.touched.push((key, rank));
        out.cut.push(Piece::Full(p, u));
        return;
    }
    out.cut.push(Piece::Level(p, u, 0));
    if vis {
        out.wants.push((rank, Job::Load(p, u)));
    }
}
