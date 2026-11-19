// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Big files: import as a background [`Job`] (IFC, DXF, .rvt through its module; a project
//! goes to `project_ui`) with progress, ETA and cancel; the model changes only on the UI thread, as one
//! undo step, after the job has finished — a cancelled or failed job changes nothing.
//! After an open the level that will be shown is picked first, and cached meshes are fed in
//! from the database lazily, that level first, through an LRU with a byte budget
//! ([`MeshCache`], [`MeshFeed`]); the shapes map onto the renderer's instanced chunks.

use crate::ui_kit as kit;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web_time::{Duration, Instant};

use ddc_geom::simplify::pick_lod;
use ddc_model::{CacheStats, Model, Placed};
use ddc_render::SceneMesh;

use crate::editor::stage::{stage, Staged};
use crate::editor::OpFn;
use crate::jobs::{Job, JobView, Poll, Progress};
use crate::shell::EditorApp;

/// Default memory budget of loaded prototype meshes.
pub const MESH_BUDGET: usize = 512 << 20;

/// Loaded prototype shapes keyed by (prototype, level of detail), least recently used out first.
pub struct MeshCache {
    budget: usize,
    bytes: usize,
    tick: u64,
    map: HashMap<(i64, usize), (Arc<SceneMesh>, u64, usize)>,
    order: BTreeMap<u64, (i64, usize)>,
    pub hits: u64,
    pub misses: u64,
}

fn mesh_bytes(m: &SceneMesh) -> usize {
    m.positions.len() * 12 + m.triangles.len() * 12 + 64
}

impl MeshCache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            tick: 0,
            map: HashMap::new(),
            order: BTreeMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// New byte budget (low-memory mode halves it); shapes over it go out on the next load.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        while self.bytes > self.budget && self.order.len() > 1 {
            let Some((&t, &k)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&t);
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.2;
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    /// Shape of prototype `id` at `lod` (a missing coarse level gives the next finer one).
    pub fn get(
        &mut self,
        m: &Model,
        id: i64,
        lod: usize,
    ) -> Result<Arc<SceneMesh>, ddc_model::Error> {
        self.fetch(m, id, lod).map(|x| x.0)
    }

    fn touch(&mut self, k: (i64, usize)) -> Option<Arc<SceneMesh>> {
        let e = self.map.get_mut(&k)?;
        self.order.remove(&e.1);
        e.1 = self.tick;
        self.order.insert(self.tick, k);
        Some(e.0.clone())
    }

    /// [`Self::get`] plus the level actually served; entries are keyed by that level, so a
    /// fallback to a finer level is cached once, not once per requested level.
    pub fn fetch(
        &mut self,
        m: &Model,
        id: i64,
        lod: usize,
    ) -> Result<(Arc<SceneMesh>, usize), ddc_model::Error> {
        self.tick += 1;
        if let Some(s) = self.touch((id, lod)) {
            self.hits += 1;
            return Ok((s, lod));
        }
        let p = m.mesh_proto(id, lod)?;
        let k = (id, p.lod);
        if let Some(s) = self.touch(k) {
            self.hits += 1;
            return Ok((s, p.lod));
        }
        self.misses += 1;
        let mesh = Arc::new(SceneMesh {
            element: 0,
            tag: 0,
            positions: p.positions,
            triangles: p.triangles,
        });
        let b = mesh_bytes(&mesh);
        self.map.insert(k, (mesh.clone(), self.tick, b));
        self.order.insert(self.tick, k);
        self.bytes += b;
        // the entry just loaded stays even when it alone is over budget
        while self.bytes > self.budget && self.order.len() > 1 {
            let Some((&t, &k)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&t);
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.2;
            }
        }
        Ok((mesh, k.1))
    }
}

/// One cached mesh row ready to draw: prototype key plus column-major placement. The shape
/// itself stays in [`MeshCache`] only, so its byte budget is the real memory bound.
#[derive(Clone)]
pub struct Loaded {
    pub mesh: i64,
    pub proto: i64,
    pub lod: usize,
    pub cols: [f32; 16],
    pub category: String,
}

/// Placements read once (cheap rows); shapes come in per frame within a time budget.
pub struct MeshFeed {
    todo: Vec<Placed>,
    next: usize,
    pub loaded: Vec<Loaded>,
    /// Object size on screen per metre of its diagonal at the current view (px/m); 0 — full detail.
    pub px_per_m: f32,
    pub tol_px: f32,
    info: HashMap<i64, (f64, Vec<f64>)>,
}

impl MeshFeed {
    pub fn new(todo: Vec<Placed>) -> Self {
        Self {
            todo,
            next: 0,
            loaded: vec![],
            px_per_m: 0.0,
            tol_px: 1.0,
            info: HashMap::new(),
        }
    }

    /// Placements of `m`, level `first` first.
    pub fn of(m: &Model, first: Option<i64>) -> Result<Self, ddc_model::Error> {
        Ok(Self::new(m.mesh_placements(first)?))
    }

    pub fn total(&self) -> usize {
        self.todo.len()
    }

    pub fn pending(&self) -> usize {
        self.todo.len() - self.next
    }

    fn lod(&mut self, m: &Model, proto: i64) -> Result<usize, ddc_model::Error> {
        if self.px_per_m <= 0.0 {
            return Ok(0);
        }
        if !self.info.contains_key(&proto) {
            let i = m.mesh_proto_info(proto)?;
            self.info.insert(proto, (2.0 * i.radius, i.errors));
        }
        let (diag, errors) = &self.info[&proto];
        Ok(pick_lod(
            errors,
            *diag,
            self.px_per_m * *diag as f32,
            self.tol_px,
        ))
    }

    /// Loads shapes until `budget` elapses or `max` rows are done; returns rows loaded now.
    pub fn step(
        &mut self,
        m: &Model,
        cache: &mut MeshCache,
        budget: Duration,
        max: usize,
    ) -> Result<usize, ddc_model::Error> {
        let t = Instant::now();
        let mut n = 0;
        while self.next < self.todo.len() && n < max {
            let p = self.todo[self.next].clone();
            let want = self.lod(m, p.proto)?;
            // warms the cache within its budget; the row keeps only the key
            let (_, lod) = cache.fetch(m, p.proto, want)?;
            self.loaded.push(Loaded {
                mesh: p.mesh,
                proto: p.proto,
                lod,
                cols: p.place.cols(),
                category: p.category,
            });
            self.next += 1;
            n += 1;
            if t.elapsed() >= budget {
                break;
            }
        }
        Ok(n)
    }

    /// Loaded rows grouped by shape: (shape, [(placement, mesh id)]) — one instanced draw each;
    /// shapes come from the cache (reloaded if evicted), held only while the caller draws.
    #[allow(clippy::type_complexity)]
    pub fn groups(
        &self,
        m: &Model,
        cache: &mut MeshCache,
    ) -> Result<Vec<(Arc<SceneMesh>, Vec<([f32; 16], i64)>)>, ddc_model::Error> {
        let mut ix: HashMap<(i64, usize), usize> = HashMap::new();
        let mut keys: Vec<(i64, usize)> = vec![];
        let mut rows: Vec<Vec<([f32; 16], i64)>> = vec![];
        for l in &self.loaded {
            let k = *ix.entry((l.proto, l.lod)).or_insert_with(|| {
                keys.push((l.proto, l.lod));
                rows.push(vec![]);
                rows.len() - 1
            });
            rows[k].push((l.cols, l.mesh));
        }
        keys.into_iter()
            .zip(rows)
            .map(|((p, lod), r)| Ok((cache.get(m, p, lod)?, r)))
            .collect()
    }
}

/// What an IFC worker hands over: the plan (model kept in memory) or the import already done in
/// a copy of the model file.
enum IfcDone {
    Plan(Arc<ddc_io::ifc::Plan>),
    Staged(Staged<ddc_io::ifc::ImportReport>),
}

/// Same for a .rvt through its module.
enum RvtDone {
    Frames(Arc<Vec<ddc_io::bridge::proto::Frame>>),
    Staged(Staged<ddc_io::bridge::Report>),
}

enum Running {
    Ifc(PathBuf, Job<IfcDone>),
    Dxf(Job<Arc<ddc_io::dxf::DxfDocument>>),
    Rvt(PathBuf, Job<RvtDone>),
    Meshes(Job<CacheStats>),
    /// A glTF/STEP/STL/OBJ file read on a worker, written into the project on the UI thread.
    MeshFile(PathBuf, Job<Arc<crate::mesh_io::Read>>),
    /// A project built on a worker from a model file, or drawings read on one.
    Open(crate::open::build::Task),
}

pub(crate) const STAGES_IFC: &[&str] = &["job.stage.read", "job.stage.plan", "job.stage.commit"];
pub(crate) const STAGES_DXF: &[&str] = &["job.stage.read", "job.stage.parse"];
const STAGES_RVT: &[&str] = &["job.stage.bridge", "job.stage.commit"];
const STAGES_MESHES: &[&str] = &["job.stage.meshes"];
pub(crate) const STAGES_MESH_FILE: &[&str] = &["job.stage.read", "job.stage.parse", "job.stage.plan"];

impl Running {
    fn view(&self, file: Option<String>) -> JobView {
        match self {
            Running::Ifc(_, j) => j.view(STAGES_IFC, file),
            Running::Dxf(j) => j.view(STAGES_DXF, file),
            Running::Open(t) => t.view(file),
            Running::Rvt(_, j) => j.view(STAGES_RVT, file),
            Running::Meshes(j) => j.view(STAGES_MESHES, file),
            Running::MeshFile(_, j) => j.view(STAGES_MESH_FILE, file),
        }
    }

    fn cancel(&self) {
        match self {
            Running::Ifc(_, j) => j.cancel(),
            Running::Dxf(j) => j.cancel(),
            Running::Rvt(_, j) => j.cancel(),
            Running::Meshes(j) => j.cancel(),
            Running::MeshFile(_, j) => j.cancel(),
            Running::Open(t) => t.cancel(),
        }
    }
}

/// Big-file state of the app (one job at a time).
pub struct BigFile {
    job: Option<Running>,
    job_file: Option<String>,
    /// Size of the file the job reads, taken (one stat, no read) when it starts.
    job_size: Option<u64>,
    pub cache: MeshCache,
    pub feed: Option<MeshFeed>,
}

impl Default for BigFile {
    fn default() -> Self {
        Self {
            job: None,
            job_file: None,
            job_size: None,
            cache: MeshCache::new(MESH_BUDGET),
            feed: None,
        }
    }
}

impl BigFile {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Starts a job of the open flow (worker-built project, drawings).
    pub(crate) fn set_open(&mut self, t: crate::open::build::Task, file: Option<String>) {
        self.job_file = file;
        self.job_size = None;
        self.job = Some(Running::Open(t));
    }

    /// Size of the file being read, for the loading window.
    pub(crate) fn set_size(&mut self, size: Option<u64>) {
        self.job_size = size;
    }

    /// The running job reads a file into the model (all but the mesh cache build).
    pub fn loading(&self) -> bool {
        self.job.as_ref().is_some_and(|j| !matches!(j, Running::Meshes(_)))
    }

    /// The running load job as the loading window shows it.
    pub fn load_view(&self) -> Option<JobView> {
        if !self.loading() {
            return None;
        }
        let mut v = self.view()?;
        v.size = self.job_size;
        Some(v)
    }

    /// The running open-flow job is a build that writes its file (early view source).
    pub fn building(&self) -> bool {
        matches!(&self.job, Some(Running::Open(t)) if matches!(t.stage(), "job.stage.commit" | "job.stage.meshes" | "job.stage.prepare"))
    }

    pub fn view(&self) -> Option<JobView> {
        self.job.as_ref().map(|j| j.view(self.job_file.clone()))
    }

    /// The job stops at its next check; the model stays as it was.
    pub fn cancel_job(&self) {
        if let Some(j) = &self.job {
            j.cancel();
        }
    }
}

/// Lowest level holding walls or meshes: the one to show (and to load) first.
pub fn first_level(m: &Model) -> Option<i64> {
    m.sql()
        .query_row(
            "SELECT l.id FROM level l WHERE EXISTS (SELECT 1 FROM wall w WHERE w.level=l.id) \
             OR EXISTS (SELECT 1 FROM mesh s WHERE s.level=l.id) ORDER BY l.elevation, l.id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

/// The folder of the model file takes the stage copy (a read-only folder does not).
fn can_stage(model_path: &str) -> bool {
    let probe = ddc_model::stage_path(model_path);
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if made {
        let _ = std::fs::remove_file(&probe);
    }
    made
}

/// The model's own file and its change head now: the worker copies the file to stage an Add,
/// and any edit after this head refuses that Add. None for a model in memory or when the copy
/// cannot be written: the import then applies in place, as one undo step.
fn model_file(a: &EditorApp) -> Option<(String, u64)> {
    a.ed.model
        .sql()
        .path()
        .filter(|p| !p.is_empty() && can_stage(p))
        .map(|p| (p.to_string(), a.ed.model.change_head()))
}

fn ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn status(a: &mut EditorApp, key: &str, detail: Option<String>) {
    let mut s = a.sh.cat.t(key);
    if let Some(d) = detail {
        s = format!("{s}: {d}");
    }
    a.ed.status = Some(s);
}

/// Starts opening or importing `path` by its extension.
pub fn start(a: &mut EditorApp, path: PathBuf) {
    // an import finished during an Open would land in a model about to be replaced
    if crate::open::jobs(a).running() {
        status(a, "job.busy", None);
        return;
    }
    let job = match ext(&path).as_str() {
        ddc_model::PROJECT_EXT => {
            // the native file goes through the working copy, its lock and the unsaved prompt
            crate::project_ui::request_open(a, path);
            return;
        }
        "ifc" | "ifczip" => {
            let p = path.clone();
            let mp = model_file(a);
            Running::Ifc(
                path.clone(),
                Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                    let pl = ddc_io::ifc::plan_file(&p, c).map_err(|e| e.to_string())?;
                    let Some((mp, since)) = mp else {
                        return Ok(IfcDone::Plan(Arc::new(pl)));
                    };
                    c.phase("job.stage.commit", 0.9, 1.0);
                    let st = stage(&mp, since, |m| {
                        ddc_io::ifc::commit_plan_ctl(m, &pl, c).map_err(|e| e.to_string())
                    })?;
                    if c.is_cancelled() {
                        return Err(crate::jobs::CANCELLED.to_string());
                    }
                    c.finish();
                    Ok(IfcDone::Staged(st))
                }),
            )
        }
        "dxf" => {
            let p = path.clone();
            Running::Dxf(Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                #[cfg(test)]
                hold(&p, c);
                let d = ddc_io::dxf::read_file_ctl(&p, c).map_err(|e| e.to_string())?;
                c.finish();
                Ok(Arc::new(d))
            }))
        }
        e if ddc_io::meshx::READ_EXTS.contains(&e) => {
            let p = path.clone();
            Running::MeshFile(
                path.clone(),
                Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                    #[cfg(test)]
                    hold(&p, c);
                    crate::mesh_io::read(&p, c)
                }),
            )
        }
        "rvt" | "rfa" => {
            let br = crate::import::Bridges::load();
            let entries = crate::import::entries(&br.list);
            let Some(dir) = crate::import::entry_for(&entries, &path).and_then(|e| e.dir.clone())
            else {
                crate::open::no_module(a);
                return;
            };
            let (p, trust) = (path.clone(), br.trust.clone());
            let mp = model_file(a);
            Running::Rvt(
                path.clone(),
                Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                    c.phase("job.stage.bridge", 0.0, 1.0);
                    let o = ddc_io::bridge::RunOptions {
                        cancel: Some(c.cancel_flag()),
                        memory_limit: crate::open::memory::child_limit(),
                        ..Default::default()
                    };
                    let frames = ddc_io::bridge::collect_trusted(&dir, &trust, &p, &o, &mut |pr| {
                        c.set_total(pr.total);
                        c.set(pr.done);
                    })
                    .map_err(|e| e.to_string())?;
                    if c.is_cancelled() {
                        return Err(crate::jobs::CANCELLED.to_string());
                    }
                    let Some((mp, since)) = mp else {
                        return Ok(RvtDone::Frames(Arc::new(frames)));
                    };
                    // the copy and the import run here; the UI thread only attaches the result
                    c.phase("job.stage.commit", 0.8, 1.0);
                    let st = stage(&mp, since, |m| {
                        ddc_io::bridge::import_frames_ctl(m, &p, frames, c)
                            .map_err(|e| e.to_string())
                    })?;
                    if c.is_cancelled() {
                        return Err(crate::jobs::CANCELLED.to_string());
                    }
                    c.finish();
                    Ok(RvtDone::Staged(st))
                }),
            )
        }
        _ => {
            status(a, "job.unsupported", None);
            return;
        }
    };
    a.prefs.touch_recent(&path.to_string_lossy());
    a.prefs.save();
    a.big.job_file = path.file_name().map(|n| n.to_string_lossy().into_owned());
    a.big.job_size = file_size(&path);
    a.big.job = Some(job);
}

/// Tests: a slow reader. The worker waits while `<file>.hold` exists, before reading a byte.
#[cfg(test)]
fn hold(p: &Path, c: &Progress) {
    let h = p.with_extension("hold");
    while h.exists() && !c.is_cancelled() {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// One stat, no read: the loading window shows the size before the reader starts.
pub(crate) fn file_size(p: &Path) -> Option<u64> {
    std::fs::metadata(p).ok().map(|m| m.len())
}

/// Screenshots of the stage window: a job that never ends until it is cancelled.
pub fn start_demo(a: &mut EditorApp) {
    a.big.job_file = Some("Tower_A_structure_coordination_model_v12.ifc".into());
    a.big.job_size = Some(195_035_136);
    a.big.job = Some(Running::Ifc(
        PathBuf::new(),
        Job::spawn("job.title.import", move |c: &Arc<Progress>| {
            c.phase("job.stage.read", 0.0, 0.3);
            c.phase("job.stage.plan", 0.3, 0.6);
            c.phase("job.stage.commit", 0.6, 1.0);
            c.set_total(12_480);
            c.set(5_230);
            c.set_detail("job.kind.wall");
            while !c.is_cancelled() {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err("cancelled".to_string())
        }),
    ));
}

/// Builds the instance/LOD cache of the model file on a worker connection.
pub fn start_mesh_cache(a: &mut EditorApp) {
    if crate::open::jobs(a).running() {
        status(a, "job.busy", None);
        return;
    }
    if a.open.mem.low {
        // the LOD build pauses while memory is short; it can be started again later
        status(a, "open_guard.mem_low_short", None);
        return;
    }
    let path = a.ed.model.sql().path().unwrap_or_default().to_string();
    if path.is_empty() {
        // a model kept in memory has no second connection: build in place
        let mut go = |_: u64, _: u64| true;
        match a.ed.model.mesh_cache_build(&mut go) {
            Ok(st) => done_meshes(a, st),
            Err(e) => status(a, "job.failed", Some(e.to_string())),
        }
        return;
    }
    a.big.job = Some(Running::Meshes(Job::spawn(
        "job.title.meshes",
        move |c: &Arc<Progress>| {
            c.phase("job.stage.meshes", 0.0, 1.0);
            let mut go = |d: u64, t: u64| {
                c.set_total(t);
                c.set(d);
                !c.is_cancelled()
            };
            ddc_model::build_mesh_cache_file(&path, &mut go).map_err(|e| e.to_string())
        },
    )));
}

fn done_meshes(a: &mut EditorApp, st: CacheStats) {
    let s =
        a.sh.cat
            .t("job.meshes.done")
            .replace("{n}", &st.meshes.to_string())
            .replace("{p}", &st.protos.to_string())
            .replace("{r}", &st.reused.to_string());
    a.ed.status = Some(s);
    a.big.cache.clear();
    restart_feed(a);
    a.view.e3.cache.inst.invalidate();
    a.clash.dirty3d = true;
}

/// The model was replaced: cached shapes and the feed belong to the old one.
pub(crate) fn reset_feed(a: &mut EditorApp) {
    a.big.cache.clear();
    restart_feed(a);
}

fn restart_feed(a: &mut EditorApp) {
    let lv = Some(a.ed.level);
    a.big.feed = MeshFeed::of(&a.ed.model, lv).ok().filter(|f| f.total() > 0);
}

/// A big import's data, used by the first apply only. The undo step must not keep the whole
/// plan alive for the session: undo works from the model's own snapshot, redo asks for a new
/// import instead of replaying it on the UI thread.
fn once<T>(v: Arc<T>) -> std::sync::Mutex<Option<Arc<T>>> {
    std::sync::Mutex::new(Some(v))
}

fn take<T>(
    slot: &std::sync::Mutex<Option<Arc<T>>>,
    redo: &str,
) -> Result<Arc<T>, ddc_model::Error> {
    slot.lock()
        .ok()
        .and_then(|mut s| s.take())
        .ok_or_else(|| ddc_model::Error::Invalid(redo.to_string()))
}

/// The status line of a finished DXF/DWG import: `done`, then what was skipped, renamed or
/// not imported. The flag is true when anything was skipped or dropped.
fn dxf_summary(
    r: &ddc_io::dxf::ImportReport,
    done: &str,
    t: &dyn Fn(&str, &[(&str, &str)]) -> String,
) -> (String, bool) {
    let mut parts: Vec<String> = vec![];
    let mut count = |key: &str, n: usize| {
        if n > 0 {
            parts.push(t(key, &[("n", &n.to_string())]));
        }
    };
    count("job.dxf.skipped", r.skipped_unknown);
    count("job.dxf.skipped_inserts", r.skipped_inserts);
    count("job.dxf.cyclic", r.cyclic_inserts);
    count("job.dxf.renamed", r.renamed_blocks);
    count("job.dxf.reused", r.reused_blocks);
    let kinds: Vec<String> = r
        .dropped
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(k, n)| format!("{} {}", t(&format!("job.dxf.kind.{k}"), &[]), n))
        .collect();
    if !kinds.is_empty() {
        parts.push(t("job.dxf.dropped", &[("list", &kinds.join(", "))]));
    }
    let warn = r.skipped_unknown + r.skipped_inserts + r.cyclic_inserts > 0 || !kinds.is_empty();
    if parts.is_empty() {
        (done.to_string(), false)
    } else {
        (format!("{done}: {}", parts.join("; ")), warn)
    }
}

/// The stale-view warning of an Add that stands outranks the done text.
fn keep_warning(warn: Option<String>, done: String) -> String {
    warn.unwrap_or(done)
}

/// Applies a finished job on the UI thread; gives the job back while it is still running.
fn finish(a: &mut EditorApp, mut r: Running) -> Option<Running> {
    if let Running::Open(t) = r {
        return crate::open::build::finish(a, t).map(Running::Open);
    }
    macro_rules! poll {
        ($j:expr) => {
            match $j.poll() {
                Poll::Running => return Some(r),
                Poll::Cancelled => {
                    status(a, "job.cancelled", None);
                    return None;
                }
                Poll::Failed(e) => {
                    status(a, "job.failed", Some(e));
                    return None;
                }
                Poll::Done(v) => v,
            }
        };
    }
    match &mut r {
        Running::Ifc(file, j) => {
            let file = file.clone();
            let redo = a.sh.cat.t("job.redo_reimport");
            let pl = match poll!(j) {
                IfcDone::Plan(pl) => once(pl),
                IfcDone::Staged(st) => {
                    match a.ed.add_staged_warn(st, redo) {
                        Ok((r, warn)) => {
                            a.plan.fit(&a.ed);
                            status(a, "job.done", None);
                            if warn.is_some() {
                                a.ed.status = warn;
                            }
                            if !file.as_os_str().is_empty() {
                                crate::open::summary::show_ifc(a, &file, &r);
                            }
                        }
                        Err(e) => status(a, "job.failed", Some(e)),
                    }
                    return None;
                }
            };
            let report: Arc<std::sync::Mutex<Option<ddc_io::ifc::ImportReport>>> =
                Arc::new(std::sync::Mutex::new(None));
            let out = report.clone();
            let op = OpFn(Arc::new(move |m: &mut Model| {
                let pl = take(&pl, &redo)?;
                let r = ddc_io::ifc::commit_plan(m, &pl)
                    .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
                if let Ok(mut g) = out.lock() {
                    *g = Some(r);
                }
                Ok(())
            }));
            match a.ed.run_op(op) {
                Ok(()) => {
                    if let Some(l) = first_level(&a.ed.model) {
                        a.ed.set_level(l);
                    }
                    a.plan.fit(&a.ed);
                    status(a, "job.done", None);
                    let r = report.lock().ok().and_then(|mut g| g.take());
                    if let Some(r) = r.filter(|_| !file.as_os_str().is_empty()) {
                        crate::open::summary::show_ifc(a, &file, &r);
                    }
                }
                Err(e) => status(a, "job.failed", Some(e.to_string())),
            }
        }
        Running::Dxf(j) => {
            let doc = once(poll!(j));
            let redo = a.sh.cat.t("job.redo_reimport");
            let report: Arc<std::sync::Mutex<Option<ddc_io::dxf::ImportReport>>> =
                Arc::new(std::sync::Mutex::new(None));
            let out = report.clone();
            let op = OpFn(Arc::new(move |m: &mut Model| {
                let doc = take(&doc, &redo)?;
                let r = ddc_io::dxf::import_into(m, &doc)
                    .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
                if let Ok(mut g) = out.lock() {
                    *g = Some(r);
                }
                Ok(())
            }));
            match a.ed.run_op(op) {
                Ok(()) => {
                    let r = report.lock().ok().and_then(|mut g| g.take());
                    let done = a.sh.cat.t("job.done");
                    let (text, warn) = match r {
                        Some(r) => dxf_summary(&r, &done, &|k, args| crate::tr(k, args)),
                        None => (done, false),
                    };
                    let sev = if warn {
                        crate::status::Severity::Warning
                    } else {
                        crate::status::Severity::Info
                    };
                    a.ed.say(sev, text);
                }
                Err(e) => status(a, "job.failed", Some(e.to_string())),
            }
        }
        Running::Rvt(p, j) => {
            let done = poll!(j);
            let p = p.clone();
            // Add is one operation, one undo; the worker already did the import in a copy (a model
            // in memory keeps the frames path: untouched default level tidied there too)
            let r = match done {
                RvtDone::Frames(frames) => a.ed.apply_import_arc(&p, frames, true),
                RvtDone::Staged(st) => {
                    let redo = a.sh.cat.t("job.redo_reimport");
                    let r = a.ed.add_staged_warn(st, redo);
                    a.ed.status = Some(match &r {
                        Ok((rep, warn)) => keep_warning(warn.clone(), crate::import::summary(&a.ed.model, rep)),
                        Err(e) => crate::tr("import_queue.import_failed", &[("e", e.as_str())]),
                    });
                    r.map(|(rep, _)| rep)
                }
            };
            match r {
                Ok(rep) => crate::open::summary::show_bridge(a, &p, &rep),
                Err(e) => crate::open::summary::show_failed(a, &p, e),
            }
        }
        Running::Meshes(j) => {
            let st = poll!(j);
            done_meshes(a, st);
        }
        Running::MeshFile(file, j) => {
            let file = file.clone();
            let read = once(poll!(j));
            let redo = a.sh.cat.t("job.redo_reimport");
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let stats: Arc<std::sync::Mutex<Option<(ddc_io::meshx::ImportStats, Vec<String>)>>> =
                Arc::new(std::sync::Mutex::new(None));
            let out = stats.clone();
            let src = name.clone();
            let op = OpFn(Arc::new(move |m: &mut Model| {
                let r = take(&read, &redo)?;
                let st = ddc_io::meshx::import_into(m, &r.0, &src, None).map_err(ddc_model::Error::Invalid)?;
                if let Ok(mut g) = out.lock() {
                    *g = Some((st, r.1.clone()));
                }
                Ok(())
            }));
            match a.ed.run_op(op) {
                Ok(()) => {
                    a.plan.fit(&a.ed);
                    match stats.lock().ok().and_then(|mut g| g.take()) {
                        Some((st, notes)) => {
                            for n in &notes {
                                eprintln!("{name}: {n}");
                            }
                            let (text, warn) = crate::mesh_io::summary(&name, &st, &notes);
                            let sev = if warn {
                                crate::status::Severity::Warning
                            } else {
                                crate::status::Severity::Info
                            };
                            a.ed.say(sev, text);
                        }
                        None => status(a, "job.done", None),
                    }
                }
                Err(e) => crate::open::summary::show_failed(a, &file, e.to_string()),
            }
        }
        Running::Open(_) => {}
    }
    None
}

/// Per frame: poll the job, draw its window, feed cached meshes in.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    crate::edit3d::mesh_inst::frame(a, ctx);
    if let Some(r) = a.big.job.take() {
        a.big.job = finish(a, r);
    }
    crate::shell::tasks_ui::window(a, ctx);
    crate::shell::load_win::window(a, ctx);
    if let Some(f) = &mut a.big.feed {
        if f.pending() > 0 {
            if f.step(
                &a.ed.model,
                &mut a.big.cache,
                Duration::from_millis(4),
                4096,
            )
            .is_err()
            {
                a.big.feed = None;
            } else {
                ctx.request_repaint();
            }
        }
    }
}

fn h_open(a: &mut EditorApp) {
    crate::open::pick(a, crate::open::decide::Door::OpenDialog);
}

fn h_mesh_cache(a: &mut EditorApp) {
    start_mesh_cache(a);
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mgr.open_big", h_open);
    a.register_command("mgr.mesh_cache", h_mesh_cache);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_stage_needs_a_writable_folder() {
        let d = std::env::temp_dir().join(format!("ddc-cs-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(can_stage(&d.join("m.ddc").to_string_lossy()));
        assert!(!can_stage(&d.join("no_such_dir").join("m.ddc").to_string_lossy()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_view_warning_survives_done() {
        assert_eq!(keep_warning(Some("warn".into()), "done".into()), "warn");
        assert_eq!(keep_warning(None, "done".into()), "done");
    }

    fn cube(m: &Model, level: Option<i64>, d: f64, scale: f64) {
        let mut p = vec![];
        for k in 0..8 {
            let b = |i: usize| ((k >> i) & 1) as f64 * scale;
            p.push([b(0) + d, b(1), b(2) + (k as f64) * 1e-3]);
        }
        let t: [[u32; 3]; 12] = [
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ];
        let pb: Vec<u8> = p
            .iter()
            .flatten()
            .flat_map(|x: &f64| x.to_le_bytes())
            .collect();
        let ib: Vec<u8> = t.iter().flatten().flat_map(|x| x.to_le_bytes()).collect();
        m.sql()
            .execute(
                "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,'x','c',?2,?3)",
                (level, pb, ib),
            )
            .unwrap();
    }

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let a = m.add_level("A", 0.0).unwrap();
        let b = m.add_level("B", 3.0).unwrap();
        for k in 0..6 {
            cube(&m, Some(if k < 4 { a } else { b }), k as f64 * 3.0, 1.0);
        }
        cube(&m, Some(b), 30.0, 2.0);
        m.mesh_cache_build(&mut |_, _| true).unwrap();
        (m, a, b)
    }

    #[test]
    fn feed_loads_the_visible_level_first_and_shares_shapes() {
        let (m, _, b) = model();
        let mut f = MeshFeed::of(&m, Some(b)).unwrap();
        assert_eq!(f.total(), 7);
        let mut c = MeshCache::new(MESH_BUDGET);
        assert_eq!(f.step(&m, &mut c, Duration::from_secs(5), 3).unwrap(), 3);
        let first: Vec<i64> = f.todo[..3].iter().map(|p| p.level.unwrap()).collect();
        assert_eq!(first, vec![b, b, b]);
        while f.pending() > 0 {
            f.step(&m, &mut c, Duration::from_secs(5), 100).unwrap();
        }
        assert_eq!(c.misses, 2);
        assert_eq!(c.hits, 5);
        let g = f.groups(&m, &mut c).unwrap();
        assert_eq!(
            g.len(),
            2,
            "unit cubes share one shape, the big cube has its own"
        );
        assert_eq!(g.iter().map(|x| x.1.len()).sum::<usize>(), 7);
    }

    #[test]
    fn loaded_rows_do_not_pin_shapes_beyond_the_budget() {
        let (m, _, _) = model();
        let mut f = MeshFeed::of(&m, None).unwrap();
        let mut c = MeshCache::new(1);
        while f.pending() > 0 {
            f.step(&m, &mut c, Duration::from_secs(5), 100).unwrap();
        }
        assert_eq!(f.loaded.len(), 7);
        assert_eq!(c.len(), 1, "only the last shape stays");
        let g = f.groups(&m, &mut c).unwrap();
        assert_eq!(g.len(), 2, "evicted shapes reload for drawing");
        drop(g);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn missing_coarse_level_is_cached_once() {
        let (m, _, _) = model();
        let id = m.mesh_placements(None).unwrap()[0].proto;
        let mut c = MeshCache::new(MESH_BUDGET);
        let (_, l2) = c.fetch(&m, id, 2).unwrap();
        // asking again for 2, or for what was served, reuses the one entry
        assert_eq!(c.fetch(&m, id, 2).unwrap().1, l2);
        assert_eq!(c.fetch(&m, id, l2).unwrap().1, l2);
        assert_eq!(c.len(), 1);
        assert_eq!(c.misses, 1);
    }

    #[test]
    fn big_import_op_drops_its_plan_after_apply() {
        let slot = once(Arc::new(vec![1u8; 16]));
        let held = take(&slot, "again").unwrap();
        assert_eq!(Arc::strong_count(&held), 1, "the step keeps no copy");
        match take(&slot, "again") {
            Err(ddc_model::Error::Invalid(s)) => assert_eq!(s, "again"),
            other => panic!("{:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn cache_evicts_least_recently_used_within_budget() {
        let (m, _, _) = model();
        let ids: Vec<i64> = {
            let mut v: Vec<i64> = m
                .mesh_placements(None)
                .unwrap()
                .iter()
                .map(|p| p.proto)
                .collect();
            v.sort();
            v.dedup();
            v
        };
        assert_eq!(ids.len(), 2);
        let one = mesh_bytes(&MeshCache::new(0).get(&m, ids[0], 0).unwrap());
        let mut c = MeshCache::new(one + one / 2);
        c.get(&m, ids[0], 0).unwrap();
        c.get(&m, ids[1], 0).unwrap();
        assert_eq!(c.len(), 1, "over budget: the older shape left");
        assert!(c.bytes() <= one + one / 2);
        c.get(&m, ids[1], 0).unwrap();
        assert_eq!(c.hits, 1);
        c.get(&m, ids[0], 0).unwrap();
        assert_eq!(c.misses, 3);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn every_reader_can_be_cancelled() {
        let dxf = Job::spawn("t", |_: &Arc<Progress>| {
            Err::<Arc<ddc_io::dxf::DxfDocument>, _>("x".to_string())
        });
        assert!(Running::Dxf(dxf).view(None).cancellable);
        let meshes = Job::spawn("t", |_: &Arc<Progress>| {
            Err::<CacheStats, _>("x".to_string())
        });
        assert!(Running::Meshes(meshes).view(None).cancellable);
    }

    #[test]
    fn first_level_is_the_lowest_with_content() {
        let (m, a, _) = model();
        assert_eq!(first_level(&m), Some(a));
        let e = Model::open(":memory:").unwrap();
        assert_eq!(first_level(&e), None);
    }

    fn echo(k: &str, a: &[(&str, &str)]) -> String {
        let v: Vec<String> = a.iter().map(|(_, v)| v.to_string()).collect();
        format!("{k}[{}]", v.join(","))
    }

    #[test]
    fn dxf_job_summary_lists_dropped_kinds() {
        let mut r = ddc_io::dxf::ImportReport::default();
        r.skipped_unknown = 2;
        r.dropped.insert("spline".into(), 3);
        r.dropped.insert("mtext".into(), 1);
        let (text, warn) = dxf_summary(&r, "Done", &echo);
        assert!(warn);
        assert!(text.starts_with("Done: job.dxf.skipped[2]"), "{text}");
        assert!(text.contains("job.dxf.kind.mtext[] 1"), "{text}");
        assert!(text.contains("job.dxf.kind.spline[] 3"), "{text}");
    }

    #[test]
    fn dxf_job_summary_cyclic_and_renamed_blocks() {
        let mut r = ddc_io::dxf::ImportReport::default();
        r.cyclic_inserts = 1;
        r.renamed_blocks = 4;
        let (text, warn) = dxf_summary(&r, "Done", &echo);
        assert!(warn);
        assert!(text.contains("job.dxf.cyclic[1]") && text.contains("job.dxf.renamed[4]"), "{text}");
        let mut only = ddc_io::dxf::ImportReport::default();
        only.renamed_blocks = 1;
        assert!(!dxf_summary(&only, "Done", &echo).1, "renaming alone is not a warning");
    }

    #[test]
    fn dxf_job_clean_import_plain_done() {
        let r = ddc_io::dxf::ImportReport::default();
        assert_eq!(dxf_summary(&r, "Done", &echo), ("Done".to_string(), false));
    }
}
