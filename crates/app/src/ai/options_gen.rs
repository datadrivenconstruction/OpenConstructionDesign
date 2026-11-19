// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `generate_options`: up to eight design options from one request, made in drafts without
//! confirmation. Pure generation runs on a thread pool; each option is lowered on its own scratch
//! editor (a copy of the base snapshot, one worker thread each), deduplicated by diff hash and
//! stored as an `ai_option` recipe. Element tables of the model holding the recipes are never
//! written here: only the user's Apply (`ai_option_apply`) does that.
//!
//! A resource guard runs before any file is written: free disk and free memory decide how many
//! copies and workers fit, or the call is refused with the reason. Disk is checked again before
//! each copy, and a refused or failed option leaves no file behind.

use super::fmt::{self, Out};
use super::plan::{compile_one, CompileCx, PlanOp};
use super::session::{Core, Done};
use super::sets::Sets;
use crate::editor::Editor;
use crate::units::Units;
use ddc_gen::sample::{self, Candidate, Sampler, Vary, MAX_OPTIONS};
use ddc_gen::{GenOut, Generator};
use super::options_keep;
use ddc_model::{Model, ModelDiff, OptionRec, OptionStatus, OptionStore, RecipeStore, RowKind};
use serde_json::{json, Value as J};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const DEFAULT_N: usize = 4;
const DIFF_LIMIT: usize = 200_000;
const MIB: u64 = 1 << 20;
/// Left free on the disk after all copies: the system and the journals need room too.
pub const DISK_RESERVE: u64 = 256 * MIB;
/// Left free in memory after all workers.
pub const MEM_RESERVE: u64 = 512 * MIB;
const SET_NAME: &str = "generated";

/// Free space and memory as the system reports them; injectable so the guard is tested.
pub trait Probe: Sync {
    fn free_disk(&self, dir: &Path) -> Option<u64>;
    fn free_mem(&self) -> Option<u64>;
    fn cores(&self) -> usize {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    }
}

pub struct SystemProbe;

impl Probe for SystemProbe {
    fn free_disk(&self, dir: &Path) -> Option<u64> {
        // the folder may not exist yet: ask the nearest existing one
        let mut at = Some(dir);
        while let Some(d) = at {
            if d.is_dir() {
                return disk::free(d);
            }
            at = d.parent();
        }
        None
    }

    fn free_mem(&self) -> Option<u64> {
        ddc_io::bridge::limit::memory().map(|m| m.available)
    }
}

#[cfg(windows)]
mod disk {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            dir: *const u16,
            free_to_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    pub fn free(dir: &Path) -> Option<u64> {
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
        let mut free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }
}

#[cfg(not(windows))]
mod disk {
    /// Unknown here: the guard refuses rather than guess.
    pub fn free(_: &std::path::Path) -> Option<u64> {
        None
    }
}

/// Disk one option copy may take: the file, its journal and headroom.
pub fn copy_bytes(base_bytes: u64) -> u64 {
    base_bytes + base_bytes / 4 + 8 * MIB
}

/// Memory one lowering worker may take: page cache, editor and diff of one copy.
pub fn worker_bytes(base_bytes: u64) -> u64 {
    base_bytes + 128 * MIB
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    /// Option copies that fit on the disk, at most the request.
    pub copies: usize,
    pub workers: usize,
    pub capped: Option<String>,
}

/// The guard. `Err` is the refusal with its reason; nothing is written either way.
pub fn budget(probe: &dyn Probe, dir: &Path, base_bytes: u64, n: usize) -> Result<Budget, String> {
    let per = copy_bytes(base_bytes);
    let disk = probe
        .free_disk(dir)
        .ok_or("cannot read free disk space, so no option copies are made")?;
    let fit = (disk.saturating_sub(DISK_RESERVE) / per) as usize;
    if fit == 0 {
        return Err(format!(
            "not enough free disk: {} MiB free, one option copy needs {} MiB and {} MiB stay reserved",
            disk / MIB,
            per.div_ceil(MIB),
            DISK_RESERVE / MIB
        ));
    }
    let per_w = worker_bytes(base_bytes);
    let mem = probe
        .free_mem()
        .ok_or("cannot read free memory, so no options are generated")?;
    let fit_w = (mem.saturating_sub(MEM_RESERVE) / per_w) as usize;
    if fit_w == 0 {
        return Err(format!(
            "not enough free memory: {} MiB free, one option needs {} MiB and {} MiB stay reserved",
            mem / MIB,
            per_w.div_ceil(MIB),
            MEM_RESERVE / MIB
        ));
    }
    let copies = n.min(fit);
    let capped = (copies < n).then(|| {
        format!(
            "{n} options asked, {copies} fit the disk check ({} MiB free, {} MiB per copy)",
            disk / MIB,
            per.div_ceil(MIB)
        )
    });
    Ok(Budget {
        copies,
        workers: copies.min(fit_w).min(probe.cores()).max(1),
        capped,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub generator: String,
    pub params: J,
    pub level: i64,
    pub vary: Vary,
    pub sampler: Sampler,
    pub n: usize,
    pub seed: u64,
    pub set: String,
}

impl Request {
    /// `{request: {generator, params?, level?}, n?, vary?, sampler?, seed?, set?}`. An `n` above
    /// [`MAX_OPTIONS`] is capped, and the reason comes back with the request.
    pub fn parse(a: &J, level: i64) -> Result<(Request, Option<String>), String> {
        let r = &a["request"];
        let generator = r["generator"]
            .as_str()
            .ok_or("request.generator must be text")?
            .to_string();
        let asked = match &a["n"] {
            J::Null => DEFAULT_N as u64,
            v => v.as_u64().ok_or("n must be a whole number")?,
        };
        if asked == 0 {
            return Err("n must be at least 1".into());
        }
        let n = (asked as usize).min(MAX_OPTIONS);
        let capped = (asked as usize > MAX_OPTIONS)
            .then(|| format!("n {asked} is above {MAX_OPTIONS}: capped at {MAX_OPTIONS}"));
        let req = Request {
            generator,
            params: r["params"].clone(),
            level: r["level"].as_i64().unwrap_or(level),
            vary: Vary::parse(&a["vary"]).map_err(|e| e.to_string())?,
            sampler: Sampler::parse(a["sampler"].as_str()).map_err(|e| e.to_string())?,
            n,
            seed: a["seed"].as_u64().unwrap_or(1),
            set: a["set"].as_str().unwrap_or(SET_NAME).to_string(),
        };
        Ok((req, capped))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenOption {
    /// `ai_option` id of the recipe.
    pub id: i64,
    pub index: usize,
    pub seed: u64,
    pub criterion: String,
    /// The materialised branch copy `opt-<id>.db`: a cache, re-creatable from the recipe.
    pub path: PathBuf,
    pub diff_hash: u64,
    pub created: usize,
    pub changed: usize,
    pub deleted: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GenReport {
    pub options: Vec<GenOption>,
    /// Option index and why it was not kept.
    pub dropped: Vec<(usize, String)>,
    pub capped: Vec<String>,
}

fn sidecar(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn remove_copy(p: &Path) {
    for s in ["", "-wal", "-shm", "-journal", ".part"] {
        let _ = fs::remove_file(sidecar(p, s));
    }
}

fn file_bytes(p: &Path) -> u64 {
    let len = |p: &Path| fs::metadata(p).map_or(0, |m| m.len());
    len(p) + len(&sidecar(p, "-wal"))
}

/// FNV-1a over the diff rows: same rows, same hash, in every run of the same build.
pub fn diff_hash(d: &ModelDiff) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{:?}", d.rows).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn counts(d: &ModelDiff) -> (usize, usize, usize) {
    let own = |r: &&ddc_model::RowDiff| r.owner.as_ref() == Some(&(r.table.clone(), r.rowid));
    let n = |k: RowKind| d.rows.iter().filter(own).filter(|r| r.kind == k).count();
    (n(RowKind::Created), n(RowKind::Changed), n(RowKind::Deleted))
}

fn metrics_of(out: &GenOut) -> Vec<(String, f64)> {
    match out {
        GenOut::Layout(l) => l.metrics.iter().map(|m| (m.0.to_string(), m.1)).collect(),
        GenOut::Family { def } => ddc_gen::family_metrics(def)
            .into_iter()
            .map(|m| (m.0.to_string(), m.1))
            .collect(),
        GenOut::Steel { metrics, .. } => metrics.iter().map(|m| (m.0.to_string(), m.1)).collect(),
    }
}

/// Lowers one candidate on its own scratch editor over a copy of `base` at `path`.
fn lower_one(
    base: &Path,
    path: &Path,
    req: &Request,
    c: &Candidate,
    gens: &[&'static dyn Generator],
    units: Units,
) -> Result<ModelDiff, String> {
    let e = |e: ddc_model::Error| e.to_string();
    Model::backup_file(base, path, &mut |_, _| true).map_err(e)?;
    let mut ed = Editor::new_in_memory().map_err(e)?;
    ed.replace_model(Model::open_copy(&path.to_string_lossy()).map_err(e)?)
        .map_err(e)?;
    ed.set_units(units);
    let head = ed.model.change_head();
    let mut slots = HashMap::new();
    let sets = Sets::default();
    let op = PlanOp::Generate {
        id: req.generator.clone(),
        params: c.params.clone(),
        seed: c.seed,
        level: req.level,
        slot0: 0,
    };
    let mut cx = CompileCx {
        ed: &mut ed,
        slots: &mut slots,
        sets: &sets,
        gens,
        rules: &[],
        declared: HashSet::new(),
        skips: vec![],
    };
    compile_one(&mut cx, &op).map_err(|e| e.0)?;
    let ch = ed.model.changes_since(head);
    ed.model.diff_from(base, &ch, DIFF_LIMIT).map_err(e)
}

/// Generates, lowers and stores the options of `req`. `store` gets one draft recipe per kept
/// option with `head` as its base; `base` is a snapshot of the model at `head`; copies go to
/// `dir`. The guard refuses before anything is written.
#[allow(clippy::too_many_arguments)]
pub fn generate_options(
    store: &dyn RecipeStore,
    head: (i64, i64),
    base: &Path,
    dir: &Path,
    req: &Request,
    gens: &[&'static dyn Generator],
    units: Units,
    probe: &dyn Probe,
) -> Result<GenReport, String> {
    let g = *gens
        .iter()
        .find(|g| g.id() == req.generator)
        .ok_or_else(|| format!("no generator {}", req.generator))?;
    if !base.is_file() {
        return Err(format!("no base snapshot: {}", base.display()));
    }
    let base_bytes = file_bytes(base);
    let bud = budget(probe, dir, base_bytes, req.n)?;
    // pure stage first: duplicates and failures cost no disk
    let batch = sample::generate(
        g,
        &req.params,
        &req.vary,
        req.sampler,
        req.n,
        req.seed,
        bud.workers,
    )
    .map_err(|e| e.to_string())?;
    let mut report = GenReport::default();
    report.capped.extend(bud.capped.clone());
    for i in &batch.duplicates {
        report
            .dropped
            .push((*i, "same result as an earlier option".to_string()));
    }
    report.dropped.extend(batch.failed.iter().cloned());
    let take = batch.options.len().min(bud.copies);
    for c in &batch.options[take..] {
        report
            .dropped
            .push((c.index, "over the disk check".to_string()));
    }
    let todo = &batch.options[..take];
    if todo.is_empty() {
        report.dropped.sort_by_key(|d| d.0);
        return Ok(report);
    }
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let per = copy_bytes(base_bytes);
    let tmp = |c: &Candidate| dir.join(format!("gen-{}.db", c.index));
    let results: Mutex<Vec<Option<Result<ModelDiff, String>>>> = Mutex::new(vec![None; todo.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..bud.workers.min(todo.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(c) = todo.get(i) else { break };
                let path = tmp(c);
                remove_copy(&path);
                let free = probe.free_disk(dir).unwrap_or(0);
                let r = if free < DISK_RESERVE + per {
                    Err(format!(
                        "disk check before the copy: {} MiB free, {} MiB needed",
                        free / MIB,
                        (DISK_RESERVE + per).div_ceil(MIB)
                    ))
                } else {
                    lower_one(base, &path, req, c, gens, units)
                };
                if r.is_err() {
                    remove_copy(&path);
                }
                results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut seen: Vec<u64> = vec![];
    let mut failure: Option<String> = None;
    for (c, r) in todo.iter().zip(results) {
        let path = tmp(c);
        let diff = match r {
            Some(Ok(d)) => d,
            Some(Err(why)) => {
                report.dropped.push((c.index, why));
                continue;
            }
            None => {
                remove_copy(&path);
                report.dropped.push((c.index, "not lowered".to_string()));
                continue;
            }
        };
        let h = diff_hash(&diff);
        let why_not = if failure.is_some() {
            Some("not stored: an earlier recipe failed".to_string())
        } else if diff.rows.is_empty() {
            Some("changes nothing in the model".to_string())
        } else if seen.contains(&h) {
            Some("same changes as an earlier option".to_string())
        } else {
            None
        };
        if let Some(why) = why_not {
            remove_copy(&path);
            report.dropped.push((c.index, why));
            continue;
        }
        seen.push(h);
        let (created, changed, deleted) = counts(&diff);
        let metrics: Vec<J> = metrics_of(&c.out)
            .into_iter()
            .map(|(k, v)| json!([k, v]))
            .collect();
        let rec = OptionRec {
            id: 0,
            set: req.set.clone(),
            name: format!("{} {}", req.generator, c.index + 1),
            base_head: head.0,
            base_stamp: head.1,
            envelope: json!({
                "op": "generate",
                "generator": req.generator,
                "params": c.params,
                "seed": c.seed,
                "level": req.level,
            })
            .to_string(),
            seed: c.seed,
            params: c.params.to_string(),
            generator: Some(req.generator.clone()),
            metrics: Some(
                json!({
                    "metrics": metrics,
                    "created": created,
                    "changed": changed,
                    "deleted": deleted,
                    "diff_hash": format!("{h:016x}"),
                    "truncated": diff.truncated,
                })
                .to_string(),
            ),
            note: Some(ddc_gen::SCHEMATIC_NOTE.to_string()),
            why: Some(c.criterion.clone()),
            status: OptionStatus::Draft,
        };
        let id = match super::option_freeze::add_option(None, None, store, &rec) {
            Ok(id) => id,
            Err(e) => {
                remove_copy(&path);
                let e = match e {
                    ddc_model::OptionApplyError::Model(s) => s,
                    e => format!("{e:?}"),
                };
                failure = Some(e.clone());
                report.dropped.push((c.index, format!("recipe not stored: {e}")));
                continue;
            }
        };
        let fin = dir.join(format!("opt-{id}.db"));
        // a reused id must not overwrite an older copy: it goes to _archive
        if let Err(e) = options_keep::archive_copy(&fin) {
            remove_copy(&path);
            failure = Some(e.clone());
            report.dropped.push((c.index, format!("old copy not archived: {e}")));
            continue;
        }
        let path = match fs::rename(&path, &fin) {
            Ok(()) => fin,
            // the copy is a cache: keep it under its first name
            Err(_) => path,
        };
        report.options.push(GenOption {
            id,
            index: c.index,
            seed: c.seed,
            criterion: c.criterion.clone(),
            path,
            diff_hash: h,
            created,
            changed,
            deleted,
        });
    }
    report.dropped.sort_by_key(|d| d.0);
    match failure {
        Some(e) if report.options.is_empty() => Err(e),
        _ => Ok(report),
    }
}

/// Tool handler: recipes go to a store inside the session folder, so plan resets keep them and
/// no project file is opened for writing from the worker.
pub fn handle(c: &mut Core, a: &J) -> Result<Done, String> {
    let (req, capped) = Request::parse(a, c.view.level)?;
    let (base, session_dir) = c.option_paths();
    let head = Model::open_copy(&base.to_string_lossy())
        .and_then(|m| m.ai_option_head())
        .map_err(|e| e.to_string())?;
    let (dir, db) = options_keep::locate(&session_dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if let Some(parent) = db.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let store = OptionStore::open(&db).map_err(|e| e.to_string())?;
    let gens = c.gens.clone();
    let mut rep = generate_options(
        &store,
        head,
        &base,
        &dir,
        &req,
        &gens,
        c.view.units,
        &SystemProbe,
    )?;
    if let Some(why) = capped {
        rep.capped.insert(0, why);
    }
    let mut o = Out::new(c.max_chars);
    let opts: Vec<J> = rep
        .options
        .iter()
        .map(|g| {
            json!({"option": g.id, "index": g.index, "seed": g.seed, "criterion": fmt::text(&g.criterion),
                   "created": g.created, "changed": g.changed, "deleted": g.deleted})
        })
        .collect();
    let dropped: Vec<J> = rep
        .dropped
        .iter()
        .map(|d| json!({"index": d.0, "why": fmt::text(&d.1)}))
        .collect();
    o.put("generated", &json!(rep.options.len()));
    o.put("status", &json!("draft: nothing reaches the model until the person applies an option"));
    if !rep.capped.is_empty() {
        o.put("capped", &json!(rep.capped));
    }
    o.put("note", &json!(ddc_gen::SCHEMATIC_NOTE));
    o.list("options", &opts);
    o.list("dropped", &dropped);
    Ok(o.finish().into())
}

#[cfg(test)]
#[path = "options_gen_tests.rs"]
mod tests;
