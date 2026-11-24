// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A model file opens as a new project built on a worker (section 3.2 of the open/save
//! contract): the worker makes its own working file, reads and commits under bulk pragmas, builds
//! the mesh cache, stores the import cache entry, and hands the finished model over. The current
//! project is replaced only at the end, after the unsaved-work question; cancel in any stage
//! drops the worker's files and leaves the current project as it was.
//!
//! Also here: drawings read on a worker (cancellable) before they are placed.
use crate::editor::{default_level, imported_level, Editor};
use crate::jobs::{Job, JobView, Poll, Progress, CANCELLED};
use crate::project::{set_nothing_to_save, WorkFile};
use crate::shell::drawing_win::LevelPick;
use crate::shell::EditorApp;
use ddc_io::bridge::{RunOptions, TrustStore};
use ddc_model::source::file::{stat_of, Stat};
use ddc_model::Model;
use ddc_ui::notify::Level;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::cache;

pub const STAGES_IFC: &[&str] = &[
    "job.stage.read",
    "job.stage.plan",
    "job.stage.commit",
    "job.stage.meshes",
    "job.stage.prepare",
];
pub const STAGES_RVT: &[&str] = &[
    "job.stage.bridge",
    "job.stage.commit",
    "job.stage.meshes",
    "job.stage.prepare",
];
const STAGES_READ: &[&str] = &["job.stage.read"];
pub const STAGES_MESH: &[&str] = &[
    "job.stage.read",
    "job.stage.parse",
    "job.stage.plan",
    "job.stage.commit",
    "job.stage.meshes",
    "job.stage.prepare",
];

/// A drawing read by the worker: file, level choice, underlay with its unit.
pub type UnderlayRead = (
    PathBuf,
    LevelPick,
    Result<(ddc_io::drawing_model::Underlay, f64), String>,
);

/// How a finished build reports itself.
pub enum Done {
    Ifc,
    /// The module's own summary line.
    Rvt(String),
}

/// A finished project, not yet the editor's. Field order matters: the model closes before its
/// working file is removed.
pub struct Built {
    pub model: Model,
    pub work: WorkFile,
    pub source: PathBuf,
    pub level: Option<i64>,
    pub done: Done,
    /// Came from the import cache: nothing was read.
    pub cached: bool,
}

enum Kind {
    Ifc,
    Rvt { dir: PathBuf, trust: TrustStore },
    Mesh,
}

pub struct BuildTask {
    job: Job<Built>,
    stages: &'static [&'static str],
}

pub struct UnderlayTask {
    job: Job<Vec<UnderlayRead>>,
    /// Back into the form when the read is cancelled or fails.
    files: Vec<(PathBuf, LevelPick)>,
}

/// The running job of the open flow, kept by `bigfile` next to the other jobs.
pub enum Task {
    Build(Box<BuildTask>),
    Underlay(Box<UnderlayTask>),
}

impl Task {
    pub fn view(&self, file: Option<String>) -> JobView {
        match self {
            Task::Build(b) => b.job.view(b.stages, file),
            Task::Underlay(u) => u.job.view(STAGES_READ, file),
        }
    }

    pub fn cancel(&self) {
        match self {
            Task::Build(b) => b.job.cancel(),
            Task::Underlay(u) => u.job.cancel(),
        }
    }

    /// Stage of the running build, if it is one.
    pub fn stage(&self) -> &'static str {
        match self {
            Task::Build(b) => b.job.stage(),
            Task::Underlay(u) => u.job.stage(),
        }
    }
}

fn check(c: &Progress) -> Result<(), String> {
    if c.is_cancelled() {
        Err(CANCELLED.to_string())
    } else {
        Ok(())
    }
}

fn step(c: &Progress, done: u64, total: u64) -> Result<(), ddc_model::Error> {
    c.set_total(total);
    c.set(done);
    if c.is_cancelled() {
        Err(ddc_model::Error::Invalid(CANCELLED.into()))
    } else {
        Ok(())
    }
}

/// A new working file with an empty seeded model. The model is declared after the file so that
/// on any early return it closes first.
pub(crate) fn fresh(shared: &Mutex<Option<PathBuf>>) -> Result<(WorkFile, Model, i64), String> {
    let work = WorkFile::create().map_err(|e| e.to_string())?;
    // a crash in the middle must not offer this half-built file as lost work
    set_nothing_to_save(work.path(), true);
    let path = work.path().to_str().ok_or("non-UTF-8 path")?.to_string();
    let mut model = Model::open_working(&path).map_err(|e| e.to_string())?;
    let level = Editor::seed_model(&mut model).map_err(|e| e.to_string())?;
    if let Ok(mut s) = shared.lock() {
        *s = Some(work.path().to_path_buf());
    }
    Ok((work, model, level))
}

/// The default level goes when the file brought its own, and a model without views gets the
/// default sections and elevations; returns the level to show.
pub(crate) fn tidy(model: &mut Model, seeded: i64) -> Option<i64> {
    let shown = imported_level(model);
    if let Some(l) = shown {
        if l != seeded && default_level(model, seeded) {
            // refused while anything stands on it: then it simply stays
            let _ = model.delete_level(seeded);
        }
    }
    // an imported model gets the sections and elevations a drawn one would have
    if let Err(e) = ddc_io::sheets::add_default_views(model) {
        eprintln!("import: default views not added: {e}");
    }
    shown
        .or_else(|| crate::bigfile::first_level(model))
        .or_else(|| model.level_list().ok().and_then(|l| l.first().map(|x| x.0)))
}

fn has_meshes(model: &Model) -> bool {
    model
        .sql()
        .query_row("SELECT EXISTS(SELECT 1 FROM mesh)", [], |r| {
            r.get::<_, bool>(0)
        })
        .unwrap_or(false)
}

/// Instances and levels of detail, so the first frames draw without a build in the UI. Skipped
/// under memory pressure (the user can run it later from the command).
fn mesh_stage(c: &Progress, model: &mut Model) -> Result<(), String> {
    if !has_meshes(model) {
        return Ok(());
    }
    if ddc_io::bridge::limit::memory().is_some_and(|m| m.available < super::memory::LOW) {
        return Ok(());
    }
    let mut go = |d: u64, t: u64| {
        c.set_total(t);
        c.set(d);
        !c.is_cancelled()
    };
    let r = model.mesh_cache_build(&mut go);
    check(c)?;
    // a failed cache costs speed, not the project
    drop(r);
    Ok(())
}

/// Hash of the source for the cache key: from the index when size and mtime match, else read
/// now (only when there is something in the cache to hit).
fn source_hash(c: &Progress, dir: &Path, path: &Path, stat: Stat) -> Option<String> {
    if let Some(h) = cache::hash_hint(dir, path, stat) {
        return Some(h);
    }
    if cache::entries(dir).is_empty() {
        return None;
    }
    c.phase("job.stage.read", 0.0, 0.1);
    let h = ddc_model::source::file::hash_file(path, &|| c.is_cancelled()).ok()?;
    cache::remember(dir, path, stat, &h);
    Some(h)
}

/// A cached build, copied to a new working file; `None` when the entry is unusable (it is
/// removed so the next open rebuilds).
fn from_cache(
    c: &Arc<Progress>,
    hit: &Path,
    source: &Path,
    stat: Stat,
    shared: &Mutex<Option<PathBuf>>,
) -> Result<Option<Built>, String> {
    c.phase("job.stage.prepare", 0.0, 1.0);
    let work = WorkFile::create().map_err(|e| e.to_string())?;
    set_nothing_to_save(work.path(), true);
    let mut on = |d: u64, t: u64| step(c, d, t);
    let copy = match ddc_model::open_project_copy(hit, work.path(), false, &mut on) {
        Ok(copy) => copy,
        Err(_) if c.is_cancelled() => return Err(CANCELLED.to_string()),
        Err(_) => {
            let _ = std::fs::remove_file(hit);
            return Ok(None);
        }
    };
    if let Ok(mut s) = shared.lock() {
        *s = Some(work.path().to_path_buf());
    }
    let mut model = copy.model;
    // the same bytes may be at another path now: the project follows the file
    let key = ddc_io::bridge::file_identity(source);
    let here = source.to_string_lossy().into_owned();
    for f in model.source_files().unwrap_or_default() {
        if f.path != here {
            let _ = model.rebind_source(&f.source, &here, &key, None);
        }
        let _ = model.refresh_source_stat(&f.source, &stat);
    }
    let level = imported_level(&model)
        .or_else(|| crate::bigfile::first_level(&model))
        .or_else(|| model.level_list().ok().and_then(|l| l.first().map(|x| x.0)));
    let done = if model
        .source_files()
        .unwrap_or_default()
        .iter()
        .any(|f| f.format != "ifc")
    {
        Done::Rvt(String::new())
    } else {
        Done::Ifc
    };
    Ok(Some(Built {
        model,
        work,
        source: source.to_path_buf(),
        level,
        done,
        cached: true,
    }))
}

/// Stores the finished build for the next open of the same bytes.
fn remember(
    c: &Arc<Progress>,
    dir: &Path,
    hash: &str,
    reader: &str,
    path: &Path,
    stat: Stat,
    m: &Model,
) {
    c.phase("job.stage.prepare", 0.9, 1.0);
    let key = cache::key(hash, reader, ddc_model::SCHEMA_VERSION);
    let mut on = |d: u64, t: u64| step(c, d, t);
    if cache::store(dir, &key, m, &mut on).is_ok() {
        cache::remember(dir, path, stat, hash);
        cache::trim(dir, cache::CAP);
    }
}

pub(crate) fn build_ifc(
    path: PathBuf,
    shared: Arc<Mutex<Option<PathBuf>>>,
    c: &Arc<Progress>,
) -> Result<Built, String> {
    let stat = stat_of(&path).map_err(|e| e.to_string())?;
    let cdir = cache::dir();
    if let Some(dir) = &cdir {
        if let Some(h) = source_hash(c, dir, &path, stat) {
            let key = cache::key(&h, cache::READER_IFC, ddc_model::SCHEMA_VERSION);
            if let Some(hit) = cache::lookup(dir, &key) {
                if let Some(b) = from_cache(c, &hit, &path, stat, &shared)? {
                    return Ok(b);
                }
            }
        }
    }
    check(c)?;
    let pl = ddc_io::ifc::plan_file(&path, c).map_err(|e| e.to_string())?;
    check(c)?;
    let hash = pl.file.as_ref().map(|f| f.stamp.hash.clone());
    c.phase("job.stage.commit", 0.9, 0.95);
    let (work, mut model, seeded) = fresh(&shared)?;
    let rep = ddc_io::ifc::commit_plan_ctl(&mut model, &pl, c).map_err(|e| e.to_string())?;
    drop(pl);
    check(c)?;
    let level = tidy(&mut model, seeded);
    c.phase("job.stage.meshes", 0.95, 0.97);
    mesh_stage(c, &mut model)?;
    c.phase("job.stage.prepare", 0.97, 1.0);
    super::summary::record(
        &mut model,
        &path,
        rep.skipped.clone(),
        &rep.warnings,
        rep.warnings.len(),
    );
    if let (Some(dir), Some(h)) = (&cdir, &hash) {
        remember(c, dir, h, cache::READER_IFC, &path, stat, &model);
    }
    check(c)?;
    c.finish();
    Ok(Built {
        model,
        work,
        source: path,
        level,
        done: Done::Ifc,
        cached: false,
    })
}

/// A glTF/STEP/STL/OBJ file as a new project: generic meshes with their materials.
pub(crate) fn build_mesh(
    path: PathBuf,
    shared: Arc<Mutex<Option<PathBuf>>>,
    c: &Arc<Progress>,
) -> Result<Built, String> {
    check(c)?;
    let (set, notes) = ddc_io::meshx::read_file(&path, Some(c))?;
    check(c)?;
    let (work, mut model, seeded) = fresh(&shared)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    ddc_io::meshx::import_into(&mut model, &set, &name, Some(c))?;
    drop(set);
    check(c)?;
    let level = tidy(&mut model, seeded);
    c.phase("job.stage.meshes", 0.95, 0.97);
    mesh_stage(c, &mut model)?;
    c.phase("job.stage.prepare", 0.97, 1.0);
    super::summary::record(&mut model, &path, Vec::new(), &notes, notes.len());
    check(c)?;
    c.finish();
    Ok(Built {
        model,
        work,
        source: path,
        level,
        done: Done::Ifc,
        cached: false,
    })
}

pub(crate) fn build_rvt(
    path: PathBuf,
    module: PathBuf,
    trust: TrustStore,
    shared: Arc<Mutex<Option<PathBuf>>>,
    c: &Arc<Progress>,
) -> Result<Built, String> {
    let stat = stat_of(&path).map_err(|e| e.to_string())?;
    let cdir = cache::dir();
    let reader = cache::rvt_reader(&module);
    if let Some(dir) = &cdir {
        if let Some(h) = source_hash(c, dir, &path, stat) {
            let key = cache::key(&h, &reader, ddc_model::SCHEMA_VERSION);
            if let Some(hit) = cache::lookup(dir, &key) {
                if let Some(b) = from_cache(c, &hit, &path, stat, &shared)? {
                    return Ok(b);
                }
            }
        }
    }
    check(c)?;
    c.phase("job.stage.bridge", 0.0, 0.8);
    let o = RunOptions {
        cancel: Some(c.cancel_flag()),
        memory_limit: super::memory::child_limit(),
        ..Default::default()
    };
    let frames = ddc_io::bridge::collect_trusted(&module, &trust, &path, &o, &mut |pr| {
        c.set_total(pr.total);
        c.set(pr.done);
    })
    .map_err(|e| e.to_string())?;
    check(c)?;
    c.phase("job.stage.commit", 0.8, 0.9);
    let (work, mut model, seeded) = fresh(&shared)?;
    // the frames move into the build file; nothing keeps a copy
    let rep = ddc_io::bridge::import_frames_ctl(&mut model, &path, frames, c)
        .map_err(|e| e.to_string())?;
    check(c)?;
    let level = tidy(&mut model, seeded);
    c.phase("job.stage.meshes", 0.9, 0.94);
    mesh_stage(c, &mut model)?;
    c.phase("job.stage.prepare", 0.94, 1.0);
    super::summary::record(
        &mut model,
        &path,
        rep.skipped.clone(),
        &rep.warnings,
        rep.warning_count,
    );
    let hash = model
        .source_files()
        .ok()
        .and_then(|f| f.into_iter().next())
        .map(|f| f.content_hash)
        .filter(|h| !h.is_empty());
    if let (Some(dir), Some(h)) = (&cdir, &hash) {
        remember(c, dir, h, &reader, &path, stat, &model);
    }
    check(c)?;
    c.finish();
    // before `model` moves into the result
    let done = Done::Rvt(crate::import::summary(&model, &rep));
    Ok(Built {
        model,
        work,
        source: path,
        level,
        done,
        cached: false,
    })
}

fn status(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

/// Opens `path` (`.ifc`, `.ifczip`, `.rvt`, a mesh file) as a new project built on a worker. The current
/// project stays as it is until the build is ready. False when nothing started.
pub fn start(a: &mut EditorApp, path: PathBuf) -> bool {
    if super::jobs(a).running() {
        status(a, "job.busy");
        return false;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = match ext.as_str() {
        "ifc" | "ifczip" => Kind::Ifc,
        e if ddc_io::meshx::READ_EXTS.contains(&e) => Kind::Mesh,
        "rvt" => {
            let br = crate::import::Bridges::load();
            let entries = crate::import::entries(&br.list);
            let Some(dir) = crate::import::entry_for(&entries, &path).and_then(|e| e.dir.clone())
            else {
                super::no_module(a);
                return false;
            };
            Kind::Rvt {
                dir,
                trust: br.trust,
            }
        }
        _ => {
            status(a, "job.unsupported");
            return false;
        }
    };
    a.open.early.reset();
    let shared = a.open.early.path.clone();
    let p = path.clone();
    let (job, stages) = match kind {
        Kind::Ifc => (
            Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                build_ifc(p, shared, c)
            }),
            STAGES_IFC,
        ),
        Kind::Mesh => (
            Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                build_mesh(p, shared, c)
            }),
            STAGES_MESH,
        ),
        Kind::Rvt { dir, trust } => (
            Job::spawn("job.title.import", move |c: &Arc<Progress>| {
                build_rvt(p, dir, trust, shared, c)
            }),
            STAGES_RVT,
        ),
    };
    a.prefs.touch_recent(&path.to_string_lossy());
    a.prefs.save();
    a.big.set_open(
        Task::Build(Box::new(BuildTask { job, stages })),
        path.file_name().map(|n| n.to_string_lossy().into_owned()),
    );
    a.big.set_size(crate::bigfile::file_size(&path));
    true
}

/// Reads drawings on a worker, then places them (`EditorApp::place_read`).
pub fn start_underlay(
    a: &mut EditorApp,
    files: Vec<(PathBuf, LevelPick)>,
    unit: Option<f64>,
    ratio: f64,
) {
    if files.is_empty() {
        return;
    }
    if a.io_busy() {
        status(a, "job.busy");
        a.drawing_form.files = files;
        return;
    }
    let work = files.clone();
    let name = files[0]
        .0
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let job = Job::spawn("job.title.import", move |c: &Arc<Progress>| {
        c.phase("job.stage.read", 0.0, 1.0);
        c.set_total(work.len() as u64);
        let mut out: Vec<UnderlayRead> = Vec::with_capacity(work.len());
        for (i, (p, pick)) in work.into_iter().enumerate() {
            check(c)?;
            c.set_detail(&p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default());
            let r = crate::drawing::read_underlay_with(&p, unit, ratio, 0, &|| c.is_cancelled());
            check(c)?;
            c.set(i as u64 + 1);
            out.push((p, pick, r));
        }
        c.finish();
        Ok(out)
    });
    let size = files.iter().filter_map(|f| crate::bigfile::file_size(&f.0)).sum();
    a.big
        .set_open(Task::Underlay(Box::new(UnderlayTask { job, files })), name);
    a.big.set_size(Some(size));
}

/// A build that waits for `release`, for tests of the busy rules.
#[cfg(test)]
pub(crate) fn parked_task(release: Arc<std::sync::atomic::AtomicBool>) -> Task {
    let job = Job::spawn("job.title.import", move |_c: &Arc<Progress>| {
        while !release.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(Vec::new())
    });
    Task::Underlay(Box::new(UnderlayTask {
        job,
        files: Vec::new(),
    }))
}

fn failed(a: &mut EditorApp, detail: &str) {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let text = format!(
        "{}: {}",
        a.sh.cat.t("job.failed"),
        crate::ui_err::humanize(detail, ru)
    );
    a.ed.status = Some(text.clone());
    a.sh.toasts.push(Level::Warning, text, None);
}

/// Polls the job: gives it back while it runs, otherwise applies the outcome.
pub fn finish(a: &mut EditorApp, t: Task) -> Option<Task> {
    match t {
        Task::Build(mut b) => {
            // a cancel is honoured at once: the worker unwinds on its own and removes its files
            let r = if b.job.ctl.is_cancelled() {
                Poll::Cancelled
            } else {
                b.job.poll()
            };
            match r {
                Poll::Running => return Some(Task::Build(b)),
                Poll::Cancelled => status(a, "job.cancelled"),
                Poll::Failed(e) => failed(a, &e),
                Poll::Done(built) => crate::project_ui::hand_built(a, built),
            }
            a.open.early.reset();
            None
        }
        Task::Underlay(mut u) => {
            let r = if u.job.ctl.is_cancelled() {
                Poll::Cancelled
            } else {
                u.job.poll()
            };
            match r {
                Poll::Running => return Some(Task::Underlay(u)),
                Poll::Cancelled => {
                    a.drawing_form.files = u.files;
                    status(a, "job.cancelled");
                }
                Poll::Failed(e) => {
                    a.drawing_form.files = u.files;
                    failed(a, &e);
                }
                Poll::Done(read) => a.place_read(read),
            }
            None
        }
    }
}

/// The finished build becomes the project (after the unsaved-work question).
pub fn adopt(a: &mut EditorApp, b: Built) {
    let Built {
        model,
        work,
        source,
        level,
        done,
        cached,
    } = b;
    // from here a crash leaves a recoverable untitled project
    set_nothing_to_save(work.path(), false);
    if let Err(e) = a.ed.adopt_built(model, work, level) {
        failed(a, &e.to_string());
        return;
    }
    crate::project_ui::after_swap(a);
    super::summary::show_stored(a);
    a.welcome_closed = true;
    if a.chrome.start.open {
        a.close_start();
    }
    a.ed.status = Some(match done {
        Done::Rvt(s) if !s.is_empty() => s,
        _ => a.sh.cat.t("job.done"),
    });
    if cached {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let msg = a.sh.cat.tf("open_guard.from_cache", &[("name", name.as_str())]);
        a.sh.toasts.push(Level::Info, msg, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_lists_end_with_the_preparation() {
        assert_eq!(STAGES_IFC.last(), Some(&"job.stage.prepare"));
        assert_eq!(STAGES_RVT.last(), Some(&"job.stage.prepare"));
        assert!(STAGES_IFC.contains(&"job.stage.read") && STAGES_IFC.contains(&"job.stage.plan"));
    }

    /// A small IFC of its own (unique level name: no cached build answers for it).
    fn ifc_file(tag: &str) -> PathBuf {
        use ddc_model::{LayerSpec, WallSpec};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level(&format!("imp1 {tag} {}", std::process::id()), 0.0).unwrap();
        let layer = LayerSpec {
            material: "masonry".into(),
            thickness: 0.2,
        };
        let t = m.add_wall_type("W", &[layer]).unwrap();
        for k in 0..5 {
            let y = k as f64 * 3.0;
            let w = WallSpec {
                a: (0.0, y),
                b: (6.0, y),
                level: l,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            };
            m.add_wall(&w).unwrap();
        }
        let text = ddc_io::ifc::export_ifc(&m).unwrap();
        let p = std::env::temp_dir().join(format!("ddc-imp1-{tag}-{}.ifc", std::process::id()));
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn an_ifc_build_walks_every_stage_in_order_up_to_one() {
        let p = ifc_file("build");
        let c = Progress::new();
        let shared = Arc::new(Mutex::new(None));
        let b = build_ifc(p.clone(), shared, &c).unwrap();
        assert_eq!(c.trail(), STAGES_IFC.to_vec());
        assert!((c.fraction() - 1.0).abs() < 1e-6);
        let walls: i64 = b
            .model
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap();
        assert_eq!(walls, 5);
        drop(b);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_imported_model_gets_default_sections_and_elevations() {
        let p = ifc_file("views");
        let b = build_ifc(p.clone(), Arc::new(Mutex::new(None)), &Progress::new()).unwrap();
        let views = b.model.view_defs().unwrap();
        let count = |k| views.iter().filter(|v| v.1.kind == k).count();
        assert_eq!(
            (count(ddc_model::ViewKind::Section), count(ddc_model::ViewKind::Elevation)),
            (2, 4),
            "{views:?}"
        );
        // a second pass adds nothing
        let mut m = b.model;
        assert_eq!(ddc_io::sheets::add_default_views(&mut m).unwrap(), 0);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_cancelled_build_hands_nothing_over() {
        let p = ifc_file("cancel");
        let c = Progress::new();
        c.cancel();
        let r = build_ifc(p.clone(), Arc::new(Mutex::new(None)), &c);
        assert!(r.is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tidy_drops_the_default_level_when_the_file_brought_levels() {
        let mut m = Model::open(":memory:").unwrap();
        let seeded = Editor::seed_model(&mut m).unwrap();
        assert_eq!(
            tidy(&mut m, seeded),
            Some(seeded),
            "nothing imported: the default stays"
        );
        assert_eq!(m.level_list().unwrap().len(), 1);
    }

    #[test]
    fn cancelled_step_reports_the_marker() {
        let c = Progress::new();
        assert!(step(&c, 1, 2).is_ok());
        c.cancel();
        assert!(matches!(step(&c, 2, 2), Err(ddc_model::Error::Invalid(m)) if m == CANCELLED));
        assert_eq!(check(&c), Err(CANCELLED.to_string()));
    }

    #[test]
    fn worker_build_of_a_cancelled_job_leaves_no_file() {
        // the working file goes with the value, whatever stage the worker stops in
        let shared = Mutex::new(None);
        let path = {
            let (work, model, _) = fresh(&shared).unwrap();
            let p = work.path().to_path_buf();
            assert!(p.exists());
            drop(model);
            drop(work);
            p
        };
        assert!(!path.exists());
        assert_eq!(shared.lock().unwrap().as_deref(), Some(path.as_path()));
    }
}
