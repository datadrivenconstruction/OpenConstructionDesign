// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The save pipeline of contract section 6.4, free of UI so each failure can be tested. The target
//! is only ever touched by one atomic rename, after the new file is complete, synced and checked:
//! a crash, a full disk or an error anywhere before that rename leaves the old file as it was.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web_time::{Duration, Instant, SystemTime};

use ddc_io::ifc::{
    check_step_file, export_ifc_file_with, file_sha256_until,
    verify_ifc_over_source, ExportStats, IfcOptions, IfcSchema,
};
use ddc_model::source::SourceRebase;
use ddc_model::{
    remove_db_files, replace_file, save_project_file, Error, LockError, Model, ProjectLock,
};

use super::report::SaveReport;
use crate::jobs::{Progress, CANCELLED};

/// Stage keys in order, for the tasks window.
pub const STAGES: &[&str] = &[
    "save.stage.snapshot",
    "save.stage.write",
    "save.stage.verify",
    "save.stage.backup",
    "save.stage.replace",
];

/// Folder beside the target that holds its backups.
pub const BACKUP_DIR: &str = "_ddc_backup";

/// A temp file of another process is left alone unless it is this old (seconds).
const STALE_AFTER: u64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Snapshot,
    Plan,
    Write,
    Sync,
    Verify,
    Backup,
    Replace,
}

impl Stage {
    pub const ALL: [Stage; 7] = [
        Stage::Snapshot,
        Stage::Plan,
        Stage::Write,
        Stage::Sync,
        Stage::Verify,
        Stage::Backup,
        Stage::Replace,
    ];
}

/// What the back end says it will leave out. Seam of `SourceWriter::plan`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WritePlan {
    /// (kind, catalog key of the reason).
    pub will_not: Vec<(String, String)>,
    /// The target over the source already holds everything: no write, no backup, no replace.
    pub nothing_to_write: bool,
}

/// One way to produce the file. Shaped like `SourceWriter` of contract section 7, so o6 and o7
/// plug in without touching the pipeline. `write` puts a complete file at `out` or returns an
/// error; the pipeline owns `out`, its sync, and everything after it.
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn plan(&self, snap: &Model, c: &Arc<Progress>) -> Result<WritePlan, String>;
    fn write(
        &self,
        plan: &WritePlan,
        snap: &Model,
        out: &Path,
        file_name: &str,
        c: &Arc<Progress>,
    ) -> Result<ExportStats, String>;
    fn verify(
        &self,
        stats: &ExportStats,
        out: &Path,
        over_source: bool,
        c: &Arc<Progress>,
    ) -> Result<(), String>;
    /// The writer that made the file, once `write` is done.
    fn used(&self) -> &'static str {
        self.name()
    }
    /// True when the target over the source must stay as it is; asked last, right before backup.
    fn must_keep_target(&self) -> bool {
        false
    }
    /// Rows of the same shape as `WritePlan::will_not`, known only after `write`.
    fn late_rows(&self) -> Vec<(String, String)> {
        vec![]
    }
    /// How the source's base moves if the file takes the source's place, once `write` is done.
    fn source_rebase(&self) -> Option<SourceRebase> {
        None
    }
}

/// The whole model through the exporter, IFC4X3. Always available.
pub struct Rewrite;

impl Backend for Rewrite {
    fn name(&self) -> &'static str {
        "rewrite"
    }

    fn plan(&self, _snap: &Model, _c: &Arc<Progress>) -> Result<WritePlan, String> {
        // what it leaves out is known exactly after the write: `ExportStats::not_written`
        Ok(WritePlan::default())
    }

    fn write(
        &self,
        _plan: &WritePlan,
        snap: &Model,
        out: &Path,
        file_name: &str,
        c: &Arc<Progress>,
    ) -> Result<ExportStats, String> {
        export_ifc_file_with(snap, out, file_name, Some(c.clone()), &exchange(IfcSchema::Ifc4x3))
    }

    fn verify(
        &self,
        stats: &ExportStats,
        out: &Path,
        over_source: bool,
        c: &Arc<Progress>,
    ) -> Result<(), String> {
        check_step_file(out, Some(c))?;
        if over_source {
            verify_ifc_over_source(out, stats, c)?;
        }
        Ok(())
    }
}

/// Options of a saved file: the chosen schema and a fixed time stamp, so an unchanged model
/// gives the same bytes (the save compares hashes).
fn exchange(schema: IfcSchema) -> IfcOptions {
    IfcOptions {
        schema,
        timestamp: Some("1970-01-01T00:00:00".into()),
        ..Default::default()
    }
}

/// A copy in the schema the user chose (IFC4, IFC4X3 or IFC2X3); always a fresh export.
pub struct RewriteAs(pub IfcSchema);

impl Backend for RewriteAs {
    fn name(&self) -> &'static str {
        "rewrite-as"
    }

    fn plan(&self, snap: &Model, c: &Arc<Progress>) -> Result<WritePlan, String> {
        Rewrite.plan(snap, c)
    }

    fn write(
        &self,
        _plan: &WritePlan,
        snap: &Model,
        out: &Path,
        file_name: &str,
        c: &Arc<Progress>,
    ) -> Result<ExportStats, String> {
        export_ifc_file_with(snap, out, file_name, Some(c.clone()), &exchange(self.0))
    }

    fn verify(
        &self,
        stats: &ExportStats,
        out: &Path,
        over_source: bool,
        c: &Arc<Progress>,
    ) -> Result<(), String> {
        Rewrite.verify(stats, out, over_source, c)
    }
}

pub struct SaveRequest {
    /// A database file the snapshot reads: the working file, or a temp copy.
    pub snapshot: PathBuf,
    pub target: PathBuf,
    pub backend: Arc<dyn Backend>,
    pub over_source: bool,
    /// SHA-256 of our last write here; empty when none.
    pub last_hash: String,
    /// Journal mark of the last save, for the "changed" count.
    pub last_journal: Option<i64>,
    /// How long a held target is retried.
    pub replace_wait: Duration,
}

#[derive(Debug)]
pub struct Outcome {
    pub report: SaveReport,
    /// SHA-256 of the written file.
    pub hash: String,
    /// `op.seq` the file holds.
    pub journal: i64,
    /// The source was left alone, because of a doubt about it, and the file is a copy beside it.
    pub source_kept: bool,
    /// The file took the source's place: what the caller moves in the source's base.
    pub rebase: Option<SourceRebase>,
}

#[derive(Debug)]
pub struct Failure {
    pub stage: Stage,
    pub message: String,
    pub cancelled: bool,
    /// The finished output, kept under this name because it could not take the target's place.
    pub kept: Option<PathBuf>,
}

/// `<target>.tmp-<pid>`, always in the target's folder so the rename stays atomic.
pub fn tmp_path(target: &Path) -> PathBuf {
    with_suffix(target, &format!(".tmp-{}", std::process::id()))
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn stem_ext(p: &Path) -> (String, String) {
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    (stem, ext)
}

/// `<stem><mid>.<ext>`, then `<stem><mid> (2).<ext>`, ...: never an existing file.
fn free_name(target: &Path, mid: &str) -> PathBuf {
    let (stem, ext) = stem_ext(target);
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = |n: u32| {
        let num = if n < 2 {
            String::new()
        } else {
            format!(" ({n})")
        };
        if ext.is_empty() {
            dir.join(format!("{stem}{mid}{num}"))
        } else {
            dir.join(format!("{stem}{mid}{num}.{ext}"))
        }
    };
    let mut n = if mid.is_empty() { 2 } else { 1 };
    while name(n).exists() {
        n += 1;
    }
    name(n)
}

/// Temp files a crash left for this target. Ours go now; another process may still be writing
/// its own, so those wait until they are old.
fn sweep_stale(target: &Path) {
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let pre = format!("{name}.tmp-");
    let ours = format!("{pre}{}", std::process::id());
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !n.starts_with(&pre) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age.as_secs() > STALE_AFTER);
        if n.starts_with(&ours) || old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Takes a free name by creating it empty, so nothing else can get that name between the check
/// and the rename that fills it.
fn claim_free(target: &Path, mid: &str) -> std::io::Result<PathBuf> {
    for _ in 0..64 {
        let p = free_name(target, mid);
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// `tmp` under a free name, or where it is when that fails.
fn move_to_free(tmp: &Path, target: &Path, mid: &str) -> PathBuf {
    let Ok(p) = claim_free(target, mid) else {
        return tmp.to_path_buf();
    };
    if fs::rename(tmp, &p).is_ok() {
        return p;
    }
    let _ = fs::remove_file(&p);
    tmp.to_path_buf()
}

fn sync_file(p: &Path) -> Result<(), String> {
    fs::OpenOptions::new()
        .write(true)
        .open(p)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

fn held(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33 | 1224))
}

/// Puts an independent, synced copy of `src` at `dst` or leaves `dst` as it was. Never a hard
/// link: a tool that rewrites `src` in place would rewrite the backup with it. `fs::copy` clones
/// where the file system can (copy on write), which is still independent. The copy goes through
/// a temp name, so a crash cannot leave half a backup.
fn copy_backup(src: &Path, dst: &Path, wait: Duration) -> Result<(), String> {
    let tmp = with_suffix(dst, &format!(".tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let started = Instant::now();
    loop {
        match copy_synced(src, &tmp) {
            Ok(()) => break,
            Err(e) if held(&e) && started.elapsed() < wait => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e.to_string());
            }
        }
    }
    let res = replace_file(&tmp, dst, wait, &mut |_, _| Ok(())).map_err(|e| e.to_string());
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn copy_synced(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::copy(src, dst)?;
    // a read-only source gives a read-only copy, which can be neither synced nor replaced later
    let mut perm = fs::metadata(dst)?.permissions();
    if perm.readonly() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perm.set_mode(perm.mode() | 0o200);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(dst, perm)?;
    }
    fs::OpenOptions::new().write(true).open(dst)?.sync_all()
}

/// Number of names that share this file's data; 1 when it cannot be told.
#[cfg(unix)]
fn link_count(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(p).map(|m| m.nlink()).unwrap_or(1)
}

#[cfg(windows)]
fn link_count(p: &Path) -> u64 {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct Info {
        attributes: u32,
        times: [u32; 6],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(h: *mut core::ffi::c_void, info: *mut Info) -> i32;
    }
    let Ok(f) = fs::File::open(p) else { return 1 };
    // SAFETY: the handle stays open for the call and `Info` matches the system layout.
    let mut info: Info = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        1
    } else {
        u64::from(info.links)
    }
}

#[cfg(not(any(unix, windows)))]
fn link_count(_: &Path) -> u64 {
    1
}

/// Older builds made backups as hard links; such a backup changes with whatever file it shares
/// data with. Rewrites it as its own copy (same bytes, nothing removed) before it is relied on.
fn make_independent(backup: &Path, wait: Duration) -> Result<(), String> {
    if link_count(backup) <= 1 {
        return Ok(());
    }
    copy_backup(backup, backup, wait).map_err(|e| {
        format!(
            "{} shares its data with another file and could not be separated: {e}",
            backup.display()
        )
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Backups {
    pub prev: Option<PathBuf>,
    pub original: Option<PathBuf>,
}

/// `_ddc_backup/<name>.prev` rolls with each replace; `<name>.original` is written once ever.
fn backup(target: &Path, wait: Duration) -> Result<Backups, String> {
    // retrying would only delay the same answer
    if !target.is_file() {
        return Err(format!("{} is not a file", target.display()));
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("target has no file name")?;
    let dir = target.parent().unwrap_or(Path::new(".")).join(BACKUP_DIR);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (prev, original) = (
        dir.join(format!("{name}.prev")),
        dir.join(format!("{name}.original")),
    );
    if original.exists() {
        make_independent(&original, wait)?;
    } else {
        copy_backup(target, &original, wait)?;
    }
    copy_backup(target, &prev, wait)?;
    Ok(Backups {
        prev: Some(prev),
        original: Some(original),
    })
}

fn gate(
    stage: Stage,
    c: &Arc<Progress>,
    inject: &mut dyn FnMut(Stage) -> Result<(), String>,
) -> Result<(), Failure> {
    let run = inject(stage);
    if c.is_cancelled() {
        return Err(Failure {
            stage,
            message: CANCELLED.into(),
            cancelled: true,
            kept: None,
        });
    }
    run.map_err(|message| Failure {
        stage,
        message,
        cancelled: false,
        kept: None,
    })
}

/// A read path that creates a table on first use cannot run on a read-only snapshot: such a
/// write goes to a private copy of the same file instead. The rest never get here.
fn write_output(
    req: &SaveRequest,
    plan: &WritePlan,
    snap: &Model,
    tmp: &Path,
    file_name: &str,
    c: &Arc<Progress>,
) -> Result<ExportStats, String> {
    match req.backend.write(plan, snap, tmp, file_name, c) {
        Err(e) if e.contains("readonly") => {
            let _ = fs::remove_file(tmp);
            let copy = with_suffix(tmp, ".snap");
            save_project_file(&req.snapshot, &copy, &mut |_, _| Ok(()))
                .map_err(|e| e.to_string())?;
            let res = copy
                .to_str()
                .ok_or_else(|| "non-UTF-8 path".to_string())
                .and_then(|s| Model::open(s).map_err(|e| e.to_string()))
                .and_then(|m| req.backend.write(plan, &m, tmp, file_name, c));
            remove_db_files(&copy);
            res
        }
        r => r,
    }
}

/// The finished output could not take the target's place: it stays under a free name.
fn keep_output(tmp: &Path, target: &Path) -> PathBuf {
    move_to_free(tmp, target, "")
}

/// Runs every stage up to the replace. Recording in the project is for the caller, on the UI
/// thread. `inject` runs on entry to each stage: production passes a no-op, tests fail or panic
/// there. A panic leaves files as a crash would.
pub fn run(
    req: &SaveRequest,
    c: &Arc<Progress>,
    inject: &mut dyn FnMut(Stage) -> Result<(), String>,
) -> Result<Outcome, Failure> {
    let started = Instant::now();
    let target = &req.target;
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fail = |stage: Stage, message: String| Failure {
        stage,
        message,
        cancelled: false,
        kept: None,
    };

    c.phase("save.stage.snapshot", 0.0, 0.02);
    if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| fail(Stage::Snapshot, e.to_string()))?;
    }
    // two windows saving to one file take turns; a folder that refuses the lock fails at the write
    let _lock = match ProjectLock::acquire(target) {
        Ok(l) => Some(l),
        Err(LockError::Busy) => {
            return Err(fail(
                Stage::Snapshot,
                format!("{file_name} is being saved by another window"),
            ));
        }
        Err(LockError::Io(_)) => None,
    };
    gate(Stage::Snapshot, c, inject)?;
    let snap = Model::open_work_snapshot(&req.snapshot)
        .map_err(|e| fail(Stage::Snapshot, e.to_string()))?;
    let journal = snap
        .journal_head()
        .map_err(|e| fail(Stage::Snapshot, e.to_string()))?;
    let changed = match req.last_journal {
        Some(m) => Some(
            snap.edits_since(m)
                .map_err(|e| fail(Stage::Snapshot, e.to_string()))?,
        ),
        None => None,
    };

    gate(Stage::Plan, c, inject)?;
    super::guard::check(&snap, target).map_err(|k| fail(Stage::Plan, k.into()))?;
    let mut plan = req.backend.plan(&snap, c).map_err(|e| fail(Stage::Plan, e))?;
    if plan.nothing_to_write && req.over_source && target.is_file() {
        let hash = file_sha256_until(target, &|| c.is_cancelled())
            .map_err(|e| fail(Stage::Plan, e))?
            .ok_or_else(|| Failure {
                stage: Stage::Plan,
                message: CANCELLED.into(),
                cancelled: true,
                kept: None,
            })?;
        let stats = ExportStats {
            bytes: fs::metadata(target).map(|m| m.len()).unwrap_or(0),
            ..Default::default()
        };
        let secs = started.elapsed().as_secs_f64();
        let report = SaveReport::of(
            target,
            req.backend.used(),
            secs,
            changed,
            &plan,
            &stats,
            false,
            &Backups::default(),
        );
        return Ok(Outcome {
            report,
            hash,
            journal,
            source_kept: false,
            rebase: None,
        });
    }

    let tmp = tmp_path(target);
    sweep_stale(target);
    c.phase("save.stage.write", 0.02, 0.8);
    gate(Stage::Write, c, inject)?;
    let drop_tmp = |f: Failure| {
        let _ = fs::remove_file(&tmp);
        f
    };
    let stats = write_output(req, &plan, &snap, &tmp, &file_name, c).map_err(|e| {
        drop_tmp(Failure {
            stage: Stage::Write,
            cancelled: c.is_cancelled(),
            message: e,
            kept: None,
        })
    })?;
    gate(Stage::Sync, c, inject).map_err(drop_tmp)?;
    sync_file(&tmp).map_err(|e| drop_tmp(fail(Stage::Sync, e)))?;
    drop(snap);

    c.phase("save.stage.verify", 0.8, 0.95);
    let verified = gate(Stage::Verify, c, inject).and_then(|()| {
        req.backend
            .verify(&stats, &tmp, req.over_source, c)
            .map_err(|e| Failure {
                stage: Stage::Verify,
                cancelled: c.is_cancelled(),
                message: e,
                kept: None,
            })
    });
    if let Err(mut f) = verified {
        if f.cancelled {
            return Err(drop_tmp(f));
        }
        f.kept = Some(move_to_free(&tmp, target, ".unverified"));
        return Err(f);
    }
    let hash = match file_sha256_until(&tmp, &|| c.is_cancelled()) {
        Ok(Some(h)) => h,
        Ok(None) => {
            return Err(drop_tmp(Failure {
                stage: Stage::Verify,
                message: CANCELLED.into(),
                cancelled: true,
                kept: None,
            }))
        }
        Err(e) => return Err(drop_tmp(fail(Stage::Verify, e))),
    };

    let copy_to = if req.over_source && target.exists() && req.backend.must_keep_target() {
        // the output is plain IFC, whatever the kept source is
        let plain = target.with_extension(super::FORMAT);
        Some(claim_free(&plain, "-edited").map_err(|e| {
            let mut f = fail(Stage::Backup, e.to_string());
            f.kept = Some(tmp.clone());
            f
        })?)
    } else {
        None
    };
    let source_kept = copy_to.is_some();
    let target = copy_to.as_ref().unwrap_or(target);
    if source_kept {
        plan.will_not
            .push(("patch".to_string(), super::patch::SOURCE_KEPT.to_string()));
    }

    // from here the output is complete and checked: a failure keeps it, a cancel drops it
    let keep = |mut f: Failure| {
        if source_kept {
            // the empty file that held the copy's name
            let _ = fs::remove_file(target);
        }
        if f.cancelled {
            drop_tmp(f)
        } else {
            f.kept = Some(keep_output(&tmp, target));
            f
        }
    };
    c.phase("save.stage.backup", 0.95, 0.97);
    gate(Stage::Backup, c, inject).map_err(keep)?;
    let replacing = !source_kept && target.exists();
    let target_changed = if replacing && !req.last_hash.is_empty() {
        match file_sha256_until(target, &|| c.is_cancelled()) {
            Ok(Some(h)) => h != req.last_hash,
            Ok(None) => {
                return Err(keep(Failure {
                    stage: Stage::Backup,
                    message: CANCELLED.into(),
                    cancelled: true,
                    kept: None,
                }))
            }
            Err(_) => true,
        }
    } else {
        false
    };
    // a file we never wrote (no stored hash) may be someone else's work: say it was replaced
    let replaced_unknown = replacing && req.last_hash.is_empty();
    let backups = if replacing {
        backup(target, req.replace_wait).map_err(|e| keep(fail(Stage::Backup, e)))?
    } else {
        Backups::default()
    };

    c.phase("save.stage.replace", 0.97, 1.0);
    gate(Stage::Replace, c, inject).map_err(keep)?;
    let mut on_wait = |_: u64, _: u64| {
        if c.is_cancelled() {
            Err(Error::Invalid(CANCELLED.into()))
        } else {
            Ok(())
        }
    };
    replace_file(&tmp, target, req.replace_wait, &mut on_wait).map_err(|e| {
        keep(Failure {
            stage: Stage::Replace,
            cancelled: c.is_cancelled(),
            message: e.to_string(),
            kept: None,
        })
    })?;

    plan.will_not.extend(req.backend.late_rows());
    let mut report = SaveReport::of(
        target,
        req.backend.used(),
        started.elapsed().as_secs_f64(),
        changed,
        &plan,
        &stats,
        target_changed,
        &backups,
    );
    report.replaced_unknown = replaced_unknown;
    Ok(Outcome {
        report,
        hash,
        journal,
        source_kept,
        rebase: (!source_kept)
            .then(|| req.backend.source_rebase())
            .flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_io::ifc::file_sha256;
    use ddc_model::{LayerSpec, WallSpec};

    #[test]
    fn a_claimed_name_is_never_one_that_exists() {
        let d = dir("claim");
        let t = d.join("a.ifc");
        fs::write(d.join("a-edited.ifc"), b"theirs").unwrap();
        let p = claim_free(&t, "-edited").unwrap();
        assert_eq!(p, d.join("a-edited (2).ifc"));
        assert_eq!(fs::read(&p).unwrap(), b"");
        assert_eq!(fs::read(d.join("a-edited.ifc")).unwrap(), b"theirs");
    }

    #[test]
    fn moving_to_a_free_name_leaves_other_files_alone() {
        let d = dir("movefree");
        let t = d.join("a.ifc");
        fs::write(&t, b"theirs").unwrap();
        let tmp = d.join("a.ifc.tmp-1");
        fs::write(&tmp, b"ours").unwrap();
        let p = move_to_free(&tmp, &t, "");
        assert_eq!(p, d.join("a (2).ifc"));
        assert_eq!(fs::read(&p).unwrap(), b"ours");
        assert_eq!(fs::read(&t).unwrap(), b"theirs");
    }
    use std::panic::{catch_unwind, AssertUnwindSafe};

    const OLD: &[u8] = b"previous file, must survive";

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-o5-job-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn work(d: &Path) -> (PathBuf, Model) {
        let p = d.join("work.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "ext",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        m.add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: t,
            offset0: None,
        })
        .unwrap();
        (p, m)
    }

    fn req(w: &Path, target: &Path, backend: Arc<dyn Backend>) -> SaveRequest {
        SaveRequest {
            snapshot: w.to_path_buf(),
            target: target.to_path_buf(),
            backend,
            over_source: false,
            last_hash: String::new(),
            last_journal: None,
            replace_wait: Duration::from_millis(200),
        }
    }

    fn no_fault(_: Stage) -> Result<(), String> {
        Ok(())
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("work.ddc"))
            .collect();
        v.sort();
        v
    }

    /// Writes `body` then answers as told, so each kind of bad output can be tried.
    struct Fake {
        body: &'static [u8],
        write_err: Option<&'static str>,
    }

    impl Backend for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn plan(&self, _: &Model, _: &Arc<Progress>) -> Result<WritePlan, String> {
            Ok(WritePlan::default())
        }
        fn write(
            &self,
            _: &WritePlan,
            _: &Model,
            out: &Path,
            _: &str,
            _: &Arc<Progress>,
        ) -> Result<ExportStats, String> {
            fs::write(out, self.body).unwrap();
            match self.write_err {
                Some(e) => Err(e.to_string()),
                None => Ok(ExportStats::default()),
            }
        }
        fn verify(
            &self,
            _: &ExportStats,
            out: &Path,
            _: bool,
            _: &Arc<Progress>,
        ) -> Result<(), String> {
            check_step_file(out, None).map(|_| ())
        }
    }

    fn empty_work(d: &Path) -> PathBuf {
        let p = d.join("work.ddc");
        let _m = Model::open(p.to_str().unwrap()).unwrap();
        p
    }

    #[test]
    fn save_refuses_empty_model_over_nonempty_file() {
        let d = dir("emptyguard");
        let (w, _m) = work(&d);
        let t = d.join("House.ifc");
        run(&req(&w, &t, Arc::new(Rewrite)), &Progress::new(), &mut no_fault).unwrap();
        let before = fs::read(&t).unwrap();
        let e = d.join("empty").join("work.ddc");
        fs::create_dir_all(e.parent().unwrap()).unwrap();
        let ew = empty_work(&d.join("empty"));
        assert_eq!(ew, e);
        let f = run(&req(&ew, &t, Arc::new(Rewrite)), &Progress::new(), &mut no_fault)
            .err()
            .expect("an empty model must not replace a file with content");
        assert_eq!(f.message, super::super::guard::EMPTY_OVER_FULL);
        assert_eq!(fs::read(&t).unwrap(), before, "the file changed");
        let other: Vec<_> = names(&d).into_iter().filter(|n| n != "House.ifc" && n != "empty").collect();
        assert!(other.is_empty(), "leftovers: {other:?}");
    }

    #[test]
    fn empty_model_may_go_to_a_new_or_empty_target() {
        let d = dir("emptyok");
        let ew = empty_work(&d);
        let t = d.join("New.ifc");
        run(&req(&ew, &t, Arc::new(Rewrite)), &Progress::new(), &mut no_fault).unwrap();
        assert!(t.is_file());
        let t2 = d.join("Blank.ifc");
        fs::write(&t2, b"").unwrap();
        run(&req(&ew, &t2, Arc::new(Rewrite)), &Progress::new(), &mut no_fault).unwrap();
        // a file of an empty model holds no content either, so it may be rewritten
        run(&req(&ew, &t, Arc::new(Rewrite)), &Progress::new(), &mut no_fault).unwrap();
    }

    #[test]
    fn a_first_save_writes_a_checked_file_and_leaves_nothing_else() {
        let d = dir("first");
        let (w, _m) = work(&d);
        let t = d.join("House-edited.ifc");
        let o = run(
            &req(&w, &t, Arc::new(Rewrite)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap();
        assert_eq!(
            names(&d),
            vec!["House-edited.ifc".to_string()],
            "no temp, lock or backup"
        );
        assert!(check_step_file(&t, None).unwrap() > 0);
        assert_eq!(o.hash, file_sha256(&t).unwrap());
        assert_eq!(o.report.by_kind.get("wall"), Some(&1));
        assert!(
            fs::read_to_string(&t)
                .unwrap()
                .contains("'House-edited.ifc'"),
            "header has the name"
        );
    }

    #[test]
    fn a_failure_at_every_stage_leaves_the_target_byte_identical() {
        for stage in Stage::ALL {
            let d = dir(&format!("fail-{stage:?}"));
            let (w, _m) = work(&d);
            let t = d.join("a.ifc");
            fs::write(&t, OLD).unwrap();
            let mut inject = |s: Stage| {
                if s == stage {
                    Err("injected".to_string())
                } else {
                    Ok(())
                }
            };
            let e = run(
                &req(&w, &t, Arc::new(Rewrite)),
                &Progress::new(),
                &mut inject,
            )
            .unwrap_err();
            assert_eq!(e.stage, stage);
            assert!(!e.cancelled);
            assert_eq!(fs::read(&t).unwrap(), OLD, "{stage:?}");
            assert!(!tmp_path(&t).exists(), "{stage:?}: the temp name is free");
            assert!(
                !with_suffix(&t, ".lock").exists(),
                "{stage:?}: lock released"
            );
            match stage {
                Stage::Verify => assert_eq!(e.kept, Some(d.join("a.unverified.ifc"))),
                Stage::Backup | Stage::Replace => {
                    let kept = e.kept.unwrap();
                    assert_eq!(kept, d.join("a (2).ifc"));
                    check_step_file(&kept, None).unwrap();
                }
                _ => assert_eq!(e.kept, None),
            }
        }
    }

    #[test]
    fn a_crash_at_every_stage_leaves_the_target_and_the_next_save_works() {
        for stage in Stage::ALL {
            let d = dir(&format!("crash-{stage:?}"));
            let (w, _m) = work(&d);
            let t = d.join("a.ifc");
            fs::write(&t, OLD).unwrap();
            let r = req(&w, &t, Arc::new(Rewrite));
            let crashed = catch_unwind(AssertUnwindSafe(|| {
                let mut inject = |s: Stage| -> Result<(), String> {
                    assert_ne!(s, stage, "simulated crash");
                    Ok(())
                };
                let _ = run(&r, &Progress::new(), &mut inject);
            }));
            assert!(crashed.is_err());
            assert_eq!(fs::read(&t).unwrap(), OLD, "{stage:?}");
            run(&r, &Progress::new(), &mut no_fault).unwrap();
            check_step_file(&t, None).unwrap();
            assert!(
                !tmp_path(&t).exists(),
                "{stage:?}: the crash's temp file is gone"
            );
        }
    }

    #[test]
    fn a_full_disk_while_writing_keeps_the_target_and_drops_the_part() {
        let d = dir("full");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let be = Fake {
            body: b"ISO-10303-21;\nHEADER;\n",
            write_err: Some("no space left on device"),
        };
        let e = run(&req(&w, &t, Arc::new(be)), &Progress::new(), &mut no_fault).unwrap_err();
        assert_eq!(e.stage, Stage::Write);
        assert!(e.message.contains("no space"));
        assert_eq!(fs::read(&t).unwrap(), OLD);
        assert_eq!(names(&d), vec!["a.ifc".to_string()], "no part left behind");
    }

    #[test]
    fn a_full_disk_on_a_new_target_creates_nothing() {
        let d = dir("full-new");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        let be = Fake {
            body: b"part",
            write_err: Some("no space left on device"),
        };
        assert!(run(&req(&w, &t, Arc::new(be)), &Progress::new(), &mut no_fault).is_err());
        assert!(names(&d).is_empty());
    }

    #[test]
    fn output_that_fails_the_check_never_replaces_and_is_kept_aside() {
        let d = dir("bad");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let cut: &[u8] = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=IFCX(#2);\n";
        let be = Arc::new(Fake {
            body: cut,
            write_err: None,
        });
        let e = run(&req(&w, &t, be.clone()), &Progress::new(), &mut no_fault).unwrap_err();
        assert_eq!(e.stage, Stage::Verify);
        assert_eq!(fs::read(&t).unwrap(), OLD);
        assert_eq!(e.kept, Some(d.join("a.unverified.ifc")));
        assert_eq!(fs::read(d.join("a.unverified.ifc")).unwrap(), cut);
        // a second failure does not overwrite the first one's file
        let e = run(&req(&w, &t, be), &Progress::new(), &mut no_fault).unwrap_err();
        assert_eq!(e.kept, Some(d.join("a.unverified (2).ifc")));
    }

    #[test]
    fn cancel_at_every_stage_keeps_the_target_and_leaves_no_file() {
        for stage in Stage::ALL {
            let d = dir(&format!("cancel-{stage:?}"));
            let (w, _m) = work(&d);
            let t = d.join("a.ifc");
            fs::write(&t, OLD).unwrap();
            let c = Progress::new();
            let c2 = c.clone();
            let mut inject = |s: Stage| {
                if s == stage {
                    c2.cancel();
                }
                Ok(())
            };
            let e = run(&req(&w, &t, Arc::new(Rewrite)), &c, &mut inject).unwrap_err();
            assert!(e.cancelled, "{stage:?}");
            assert_eq!(e.kept, None);
            assert_eq!(fs::read(&t).unwrap(), OLD);
            assert!(!tmp_path(&t).exists(), "{stage:?}");
        }
    }

    /// Plans "nothing to write" and gets cancelled while it does.
    struct CancelsInPlan;

    impl Backend for CancelsInPlan {
        fn name(&self) -> &'static str {
            "cancels"
        }
        fn plan(&self, _: &Model, c: &Arc<Progress>) -> Result<WritePlan, String> {
            c.cancel();
            Ok(WritePlan {
                nothing_to_write: true,
                ..Default::default()
            })
        }
        fn write(
            &self,
            _: &WritePlan,
            _: &Model,
            _: &Path,
            _: &str,
            _: &Arc<Progress>,
        ) -> Result<ExportStats, String> {
            unreachable!()
        }
        fn verify(
            &self,
            _: &ExportStats,
            _: &Path,
            _: bool,
            _: &Arc<Progress>,
        ) -> Result<(), String> {
            unreachable!()
        }
    }

    /// Cancels while verifying, and passes.
    struct CancelsInVerify;

    impl Backend for CancelsInVerify {
        fn name(&self) -> &'static str {
            "cancels-verify"
        }
        fn plan(&self, s: &Model, c: &Arc<Progress>) -> Result<WritePlan, String> {
            Rewrite.plan(s, c)
        }
        fn write(
            &self,
            p: &WritePlan,
            s: &Model,
            o: &Path,
            n: &str,
            c: &Arc<Progress>,
        ) -> Result<ExportStats, String> {
            Rewrite.write(p, s, o, n, c)
        }
        fn verify(
            &self,
            _: &ExportStats,
            _: &Path,
            _: bool,
            c: &Arc<Progress>,
        ) -> Result<(), String> {
            c.cancel();
            Ok(())
        }
    }

    #[test]
    fn a_cancel_during_the_checksum_of_the_verify_stage_stops_there() {
        let d = dir("cancel-verify-hash");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let e = run(
            &req(&w, &t, Arc::new(CancelsInVerify)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap_err();
        assert!(e.cancelled);
        assert_eq!(e.stage, Stage::Verify);
        assert_eq!(e.kept, None);
        assert_eq!(fs::read(&t).unwrap(), OLD);
        assert!(!tmp_path(&t).exists());
    }

    #[test]
    fn a_cancel_during_the_hash_of_an_unchanged_source_is_a_cancel_not_a_report() {
        let d = dir("cancel-plan-hash");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let mut r = req(&w, &t, Arc::new(CancelsInPlan));
        r.over_source = true;
        let e = run(&r, &Progress::new(), &mut no_fault).unwrap_err();
        assert!(e.cancelled);
        assert_eq!(e.stage, Stage::Plan);
        assert_eq!(fs::read(&t).unwrap(), OLD);
    }

    #[test]
    fn edits_made_during_the_write_are_not_in_the_file_and_the_next_save_has_them() {
        let d = dir("during");
        let (w, mut m) = work(&d);
        let t = d.join("a.ifc");
        let mut inject = |s: Stage| {
            if s == Stage::Write {
                m.add_level("MadeDuringSave", 9.0).unwrap();
            }
            Ok(())
        };
        let r = req(&w, &t, Arc::new(Rewrite));
        let o = run(&r, &Progress::new(), &mut inject).unwrap();
        assert!(!fs::read_to_string(&t).unwrap().contains("MadeDuringSave"));
        assert_eq!(m.edits_since(o.journal).unwrap(), 1, "the pill can tell");
        let r = SaveRequest {
            last_journal: Some(o.journal),
            ..r
        };
        let o2 = run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert!(fs::read_to_string(&t).unwrap().contains("MadeDuringSave"));
        assert_eq!(o2.report.changed, Some(1));
        assert_eq!(m.edits_since(o2.journal).unwrap(), 0);
    }

    #[test]
    fn backups_keep_the_first_version_once_and_roll_the_last() {
        let d = dir("backup");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, b"version 0").unwrap();
        let r = req(&w, &t, Arc::new(Rewrite));
        let o = run(&r, &Progress::new(), &mut no_fault).unwrap();
        let first = fs::read(&t).unwrap();
        let bak = d.join(BACKUP_DIR);
        assert_eq!(fs::read(bak.join("a.ifc.original")).unwrap(), b"version 0");
        assert_eq!(fs::read(bak.join("a.ifc.prev")).unwrap(), b"version 0");
        assert_eq!(
            o.report.prev.as_deref(),
            Some(bak.join("a.ifc.prev").to_str().unwrap())
        );
        run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert_eq!(
            fs::read(bak.join("a.ifc.original")).unwrap(),
            b"version 0",
            "once ever"
        );
        assert_eq!(fs::read(bak.join("a.ifc.prev")).unwrap(), first);
        let mut left: Vec<String> = names(&bak);
        left.sort();
        assert_eq!(
            left,
            vec!["a.ifc.original".to_string(), "a.ifc.prev".to_string()],
            "two at most"
        );
    }

    #[test]
    fn a_backup_is_whole_when_copied_instead_of_linked() {
        let d = dir("copy");
        let (src, dst) = (d.join("s"), d.join("t"));
        fs::write(&src, b"abc").unwrap();
        copy_synced(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"abc");
        fs::write(&src, b"changed").unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"abc", "a copy shares nothing");
    }

    #[test]
    fn a_backup_over_itself_leaves_no_temp_name() {
        let d = dir("same");
        let (src, dst) = (d.join("s"), d.join("t"));
        fs::write(&src, b"abc").unwrap();
        copy_backup(&src, &dst, Duration::ZERO).unwrap();
        copy_backup(&src, &dst, Duration::ZERO).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"abc");
        assert!(!with_suffix(&dst, &format!(".tmp-{}", std::process::id())).exists());
    }

    /// Rewrites the file's bytes without a new name: truncate and write, as some tools save.
    fn overwrite_in_place(p: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(p)
            .unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
    }

    fn backed_up(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let d = dir(tag);
        let t = d.join("a.ifc");
        fs::write(&t, b"version 0").unwrap();
        let b = backup(&t, Duration::ZERO).unwrap();
        (t, b.original.unwrap(), b.prev.unwrap())
    }

    #[test]
    fn original_survives_in_place_overwrite() {
        let (t, original, _) = backed_up("inplace-orig");
        overwrite_in_place(&t, b"someone else");
        assert_eq!(fs::read(&original).unwrap(), b"version 0");
        assert_eq!(link_count(&original), 1);
    }

    #[test]
    fn prev_survives_in_place_overwrite() {
        let (t, _, prev) = backed_up("inplace-prev");
        overwrite_in_place(&t, b"someone else");
        assert_eq!(fs::read(&prev).unwrap(), b"version 0");
        assert_eq!(link_count(&prev), 1);
    }

    #[test]
    fn backups_survive_a_rename_over_the_source() {
        let (t, original, prev) = backed_up("renameover");
        let other = t.with_file_name("other.tmp");
        fs::write(&other, b"renamed in").unwrap();
        fs::rename(&other, &t).unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"renamed in");
        assert_eq!(fs::read(&original).unwrap(), b"version 0");
        assert_eq!(fs::read(&prev).unwrap(), b"version 0");
    }

    #[test]
    fn a_failed_copy_reports_and_keeps_the_last_backup() {
        let (t, _, prev) = backed_up("copyfail");
        let gone = t.with_file_name("missing.ifc");
        let e = copy_backup(&gone, &prev, Duration::ZERO).unwrap_err();
        assert!(!e.is_empty());
        assert_eq!(fs::read(&prev).unwrap(), b"version 0");
        assert!(!with_suffix(&prev, &format!(".tmp-{}", std::process::id())).exists());
    }

    #[test]
    fn a_hard_linked_original_from_an_older_build_is_made_independent() {
        let d = dir("legacylink");
        let t = d.join("a.ifc");
        fs::write(&t, b"version 0").unwrap();
        let bak = d.join(BACKUP_DIR);
        fs::create_dir_all(&bak).unwrap();
        let original = bak.join("a.ifc.original");
        if fs::hard_link(&t, &original).is_err() {
            return; // this file system has no hard links, so nothing older can share data
        }
        assert_eq!(link_count(&original), 2, "a shared file is seen");
        backup(&t, Duration::ZERO).unwrap();
        assert_eq!(link_count(&original), 1);
        overwrite_in_place(&t, b"someone else");
        assert_eq!(fs::read(&original).unwrap(), b"version 0", "same bytes, own data");
        assert_eq!(fs::read(bak.join("a.ifc.prev")).unwrap(), b"version 0");
        assert_eq!(names(&bak), vec!["a.ifc.original", "a.ifc.prev"], "nothing removed or left");
    }

    #[test]
    fn a_folder_at_the_target_stays_and_the_output_is_kept() {
        let d = dir("folder");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::create_dir(&t).unwrap();
        fs::write(t.join("inside"), b"x").unwrap();
        let e = run(
            &req(&w, &t, Arc::new(Rewrite)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap_err();
        assert_eq!(fs::read(t.join("inside")).unwrap(), b"x");
        check_step_file(&e.kept.unwrap(), None).unwrap();
    }

    #[cfg(windows)]
    fn hold(p: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(p)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_target_held_for_a_moment_is_waited_for() {
        let d = dir("held");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let h = hold(&t);
        let r = SaveRequest {
            replace_wait: Duration::from_secs(10),
            ..req(&w, &t, Arc::new(Rewrite))
        };
        let free = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(h);
        });
        run(&r, &Progress::new(), &mut no_fault).unwrap();
        free.join().unwrap();
        check_step_file(&t, None).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_target_that_stays_held_is_untouched_and_the_output_is_kept() {
        let d = dir("held2");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let h = hold(&t);
        let e = run(
            &req(&w, &t, Arc::new(Rewrite)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap_err();
        drop(h);
        assert_eq!(fs::read(&t).unwrap(), OLD);
        check_step_file(&e.kept.unwrap(), None).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_target_is_untouched_and_the_output_is_kept() {
        let d = dir("ro");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, OLD).unwrap();
        let mut perm = fs::metadata(&t).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&t, perm.clone()).unwrap();
        let e = run(
            &req(&w, &t, Arc::new(Rewrite)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap_err();
        assert_eq!(fs::read(&t).unwrap(), OLD);
        assert!(e.kept.is_some());
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&t, perm).unwrap();
    }

    #[test]
    fn first_ifc_save_over_existing_names_backup() {
        let d = dir("unknown");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        fs::write(&t, b"a file from another session").unwrap();
        let r = req(&w, &t, Arc::new(Rewrite));
        let o = run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert!(o.report.replaced_unknown);
        assert!(!o.report.target_changed);
        let prev = o.report.prev.clone().expect("the old file is kept");
        assert_eq!(fs::read(prev).unwrap(), b"a file from another session");
        // a new name, or a file we wrote ourselves, is not flagged
        let t2 = d.join("b.ifc");
        let o = run(&req(&w, &t2, Arc::new(Rewrite)), &Progress::new(), &mut no_fault).unwrap();
        assert!(!o.report.replaced_unknown);
        let r = SaveRequest {
            last_hash: o.hash.clone(),
            ..req(&w, &t2, Arc::new(Rewrite))
        };
        let o = run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert!(!o.report.replaced_unknown);
    }

    #[test]
    fn a_target_changed_by_someone_else_is_saved_and_said_so() {
        let d = dir("changed");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        let r = req(&w, &t, Arc::new(Rewrite));
        let o = run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert!(!o.report.target_changed);
        let r = SaveRequest {
            last_hash: o.hash.clone(),
            ..r
        };
        assert!(
            !run(&r, &Progress::new(), &mut no_fault)
                .unwrap()
                .report
                .target_changed
        );
        fs::write(&t, b"someone else's edit").unwrap();
        let o = run(&r, &Progress::new(), &mut no_fault).unwrap();
        assert!(o.report.target_changed);
        assert_eq!(
            fs::read(d.join(BACKUP_DIR).join("a.ifc.prev")).unwrap(),
            b"someone else's edit"
        );
    }

    #[test]
    fn a_read_only_snapshot_error_retries_on_a_private_copy() {
        struct NeedsWrite(std::sync::atomic::AtomicU32);
        impl Backend for NeedsWrite {
            fn name(&self) -> &'static str {
                "needs-write"
            }
            fn plan(&self, _: &Model, _: &Arc<Progress>) -> Result<WritePlan, String> {
                Ok(WritePlan::default())
            }
            fn write(
                &self,
                _: &WritePlan,
                snap: &Model,
                out: &Path,
                name: &str,
                c: &Arc<Progress>,
            ) -> Result<ExportStats, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                snap.sql()
                    .execute_batch("CREATE TABLE IF NOT EXISTS made_on_first_use(id INTEGER)")
                    .map_err(|e| e.to_string())?;
                ddc_io::ifc::export_ifc_file(snap, out, name, Some(c.clone()))
            }
            fn verify(
                &self,
                _: &ExportStats,
                _: &Path,
                _: bool,
                _: &Arc<Progress>,
            ) -> Result<(), String> {
                Ok(())
            }
        }
        let d = dir("fallback");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        let be = Arc::new(NeedsWrite(Default::default()));
        run(&req(&w, &t, be.clone()), &Progress::new(), &mut no_fault).unwrap();
        assert_eq!(be.0.load(std::sync::atomic::Ordering::Relaxed), 2);
        check_step_file(&t, None).unwrap();
        assert_eq!(names(&d), vec!["a.ifc".to_string()], "the copy is gone");
    }

    #[test]
    fn replacing_the_source_runs_the_deep_check_and_keeps_the_source_as_original() {
        let d = dir("over");
        let (w, _m) = work(&d);
        let t = d.join("House.ifc");
        fs::write(&t, OLD).unwrap();
        let r = SaveRequest {
            over_source: true,
            ..req(&w, &t, Arc::new(Rewrite))
        };
        run(&r, &Progress::new(), &mut no_fault).unwrap();
        check_step_file(&t, None).unwrap();
        assert_eq!(
            fs::read(d.join(BACKUP_DIR).join("House.ifc.original")).unwrap(),
            OLD
        );
    }

    #[test]
    fn another_process_temp_file_is_left_alone_until_old() {
        let d = dir("stale");
        let (w, _m) = work(&d);
        let t = d.join("a.ifc");
        let other = with_suffix(&t, ".tmp-999999");
        fs::write(&other, b"x").unwrap();
        run(
            &req(&w, &t, Arc::new(Rewrite)),
            &Progress::new(),
            &mut no_fault,
        )
        .unwrap();
        assert!(other.exists());
    }
}
