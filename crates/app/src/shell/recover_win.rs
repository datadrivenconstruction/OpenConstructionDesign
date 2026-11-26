// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Autosave and recovery: model snapshots by time and edit count,
//! a status bar indicator, a settings window, the "Recovery" dialog at startup and
//! crash capture (the panic hook writes a note, the shell flushes a snapshot).
use super::recover_rows::{self as rows, Danger, Ev, ListState, Nav};
use super::EditorApp;
use crate::jobs::{Job, Poll};
use crate::ui_kit as kit;
use ddc_model::{Model, ProjectLock, SnapInfo, TableDiff};
use ddc_ui::notify::Level;
use ddc_ui::theme::{control, icon_size, layout, ribbon, space, stroke, ControlSize, Role};
use ddc_ui::widgets::{self, Button, Tone};
use egui::{self, RichText};
use std::path::{Path, PathBuf};
use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Toast action of an autosave failure: opens the autosave window (never closes it, unlike
/// the `mgr.autosave` toggle).
pub const AUTOSAVE_SHOW: &str = "mgr.autosave_show";

pub fn register(a: &mut EditorApp) {
    a.register_command(AUTOSAVE_SHOW, |a: &mut EditorApp| a.autosave_open = true);
}

/// Name of a model without a file: snapshots `untitled.ddc.snap-<ms>` in the autosave folder.
pub const UNTITLED: &str = "untitled.ddc";
/// Session marker: present at startup means the previous session did not exit cleanly.
pub const SESSION_LOCK: &str = "session.lock";
/// The lock is held by a running instance: while it is held, another session's marker is not a crash.
const SESSION_INSTANCE: &str = "session.instance";
/// Snapshots deferred with the "Later" button: outside autosave rotation, offered at every startup.
pub const HELD_DIR: &str = "recovery-later";
/// After this many presses of Later the snapshots count as handled and move to `_archive`.
pub const LATER_MAX: u32 = 3;

/// Session autosave state.
pub struct Autosave {
    last_rev: u64,
    /// Edits outside the model revision (placed drawings): counted, and taken by a snapshot.
    side: u64,
    side_last: u64,
    last_at: Instant,
    remind_at: Instant,
    /// Time of the last successful snapshot ("12:04").
    pub last_ok: Option<String>,
    /// Text of the last write error (⚠ in the status bar).
    pub failed: Option<String>,
    /// The toast of the current failure streak was already shown.
    alerted: bool,
    /// A snapshot is scheduled for the next frame ("Autosaving…" indicator).
    pub pending: bool,
    pub last_path: Option<PathBuf>,
    /// Working-file snapshot in the background: a large model does not stall the window.
    job: Option<Job<PathBuf>>,
    /// The running snapshot: counters before it was taken, and a drawing-store failure.
    taking: Option<((u64, u64), Option<String>)>,
    /// `None` means another instance is running: the first one checks and clears the session marker.
    pub instance: Option<ProjectLock>,
    /// Folder holding this session's markers.
    bound: Option<PathBuf>,
    /// Queued "Recovery" dialogs, one per project.
    queue: Vec<Recover>,
    /// Offer for the file given on the command line: shown as a banner, never as a modal.
    pub banner: Option<Recover>,
    /// Start of the current held button or drag; past `GESTURE_CAP` a snapshot is taken anyway.
    held_since: Option<Instant>,
}

impl Autosave {
    /// The model at `rev` is already on disk (just opened): no snapshot until it changes.
    pub fn mark_clean(&mut self, rev: u64) {
        self.last_rev = rev;
        self.side_last = self.side;
    }

    /// There are edits the last snapshot does not hold yet.
    pub fn dirty(&self, rev: u64) -> bool {
        self.changes(rev) > 0
    }

    /// An edit the model revision does not see (a drawing layer, move or geometry edit).
    pub fn note_side_edit(&mut self) {
        self.side += 1;
    }

    /// Edits since the last snapshot, model and drawings together.
    pub fn changes(&self, rev: u64) -> u64 {
        rev.saturating_sub(self.last_rev) + self.side.saturating_sub(self.side_last)
    }

    /// A snapshot of the model at `rev` was taken (or is being written).
    pub(crate) fn snapshot_taken(&mut self, rev: u64) {
        self.last_rev = rev;
        self.side_last = self.side;
    }

    /// Drawing edits counted so far (taken with a save mark when a save begins).
    pub(crate) fn side_mark(&self) -> u64 {
        self.side
    }

    /// A project save that began at model `rev` and drawing count `side` landed: edits made
    /// while it ran are still owed to the next snapshot.
    pub(crate) fn mark_saved(&mut self, rev: u64, side: u64) {
        self.last_rev = self.last_rev.max(rev);
        self.side_last = self.side_last.max(side.min(self.side));
    }

    fn counters(&self) -> (u64, u64) {
        (self.last_rev, self.side_last)
    }

    /// The attempt did not reach disk: everything it took is owed again.
    fn restore(&mut self, before: (u64, u64)) {
        self.last_rev = self.last_rev.min(before.0);
        self.side_last = self.side_last.min(before.1);
    }
}

impl Default for Autosave {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            last_rev: 0,
            side: 0,
            side_last: 0,
            last_at: now,
            remind_at: now,
            last_ok: None,
            failed: None,
            alerted: false,
            pending: false,
            last_path: None,
            job: None,
            taking: None,
            instance: None,
            bound: None,
            queue: vec![],
            banner: None,
            held_since: None,
        }
    }
}

/// What the user has to be told after a snapshot attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    None,
    Failed,
    Recovered,
}

impl Autosave {
    /// Records the result of an attempt; reports only the first failure of a streak and the
    /// first success after it.
    pub fn record(&mut self, err: Option<String>) -> Notice {
        match err {
            Some(e) => {
                self.failed = Some(e);
                if std::mem::replace(&mut self.alerted, true) {
                    Notice::None
                } else {
                    Notice::Failed
                }
            }
            None => {
                self.failed = None;
                if std::mem::replace(&mut self.alerted, false) {
                    Notice::Recovered
                } else {
                    Notice::None
                }
            }
        }
    }
}

/// Whether it is time to snapshot: there are edits and the interval has passed or `edits` edits have accumulated.
pub fn due(changes: u64, elapsed: Duration, minutes: u32, edits: u32) -> bool {
    changes > 0
        && (elapsed >= Duration::from_secs(u64::from(minutes.max(1)) * 60)
            || (edits > 0 && changes >= u64::from(edits)))
}

/// "Recovery" dialog: snapshots newer than the file (or after a crash exit).
#[derive(Default)]
pub struct Recover {
    pub file: PathBuf,
    /// Autosave folder holding these snapshots.
    pub folder: PathBuf,
    /// Project file these snapshots belong to (`None` means a project without a file).
    pub project: Option<PathBuf>,
    pub file_time: Option<SystemTime>,
    pub snaps: Vec<SnapInfo>,
    pub pick: usize,
    pub diff: Option<Result<Vec<TableDiff>, String>>,
    /// Offered for a damaged `project`: a restore binds to no file, so Save never replaces it.
    pub damaged: bool,
}

/// Snapshots worth offering: newer than the file; for a model without a file, only if
/// the previous session did not exit cleanly (`crashed`).
pub fn scan(folder: &Path, crashed: bool) -> Option<Recover> {
    scan_for(folder, crashed, None)
}

/// Modification time of `p` through the clock that also runs in the browser build.
fn file_time_of(p: &Path) -> Option<SystemTime> {
    ddc_model::wtime::modified(&std::fs::metadata(p).ok()?).ok()
}

/// [`scan`] for a project file: snapshots live in the autosave folder under [`autosave_name`], and their time
/// is compared with the file itself. They are not offered after a clean exit: "Don't save" is a decision.
pub fn scan_for(folder: &Path, crashed: bool, project: Option<&Path>) -> Option<Recover> {
    let file = project.map_or_else(|| folder.join(autosave_name(None)), Path::to_path_buf);
    let file_time = file_time_of(&file);
    let newer = |s: &SnapInfo| match file_time {
        Some(t) => {
            (crashed || project.is_none())
                && s.ms > t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis())
        }
        None => crashed,
    };
    collect_snaps(folder, project, &newer)
}

/// Every snapshot of `project` and its held ones, whatever their time: the file itself cannot
/// be read, so even an older snapshot may be the newest good copy.
pub fn all_for(folder: &Path, project: &Path) -> Option<Recover> {
    let mut r = collect_snaps(folder, Some(project), &|_| true).unwrap_or_else(|| Recover {
        file: project.to_path_buf(),
        folder: folder.to_path_buf(),
        project: Some(project.to_path_buf()),
        ..Default::default()
    });
    r.snaps.extend(held_for(folder, Some(project), None));
    if r.snaps.is_empty() {
        return None;
    }
    newest_first(&mut r.snaps);
    r.pick = first_good(&r.snaps);
    Some(r)
}

fn collect_snaps(
    folder: &Path,
    project: Option<&Path>,
    keep: &dyn Fn(&SnapInfo) -> bool,
) -> Option<Recover> {
    let base = autosave_name(project);
    let virt = folder.join(&base);
    let file = project.map_or_else(|| virt.clone(), Path::to_path_buf);
    let file_time = file_time_of(&file);
    let mut snaps: Vec<SnapInfo> =
        ddc_model::snapshot_infos(&virt).into_iter().filter(|s| keep(s)).collect();
    for f in side_families(folder, &base) {
        snaps.extend(ddc_model::snapshot_infos(&f).into_iter().filter(|s| keep(s)));
    }
    snaps.sort_by(|a, b| b.ms.cmp(&a.ms));
    let pick = snaps.iter().position(|s| s.damage.is_none()).unwrap_or(0);
    (!snaps.is_empty()).then_some(Recover {
        file,
        folder: folder.to_path_buf(),
        project: project.map(Path::to_path_buf),
        file_time,
        snaps,
        pick,
        diff: None,
        damaged: false,
    })
}

/// Snapshot name in the autosave folder: each project file has its own, without a file it is [`UNTITLED`].
pub fn autosave_name(project: Option<&Path>) -> String {
    let Some(p) = project else {
        return UNTITLED.to_string();
    };
    let stem = p
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    format!("{stem}-{}.ddc", crate::project::path_key(p))
}

/// Snapshot of a dead working copy: a family of one file, which `keep` rotation will not evict.
pub fn recovery_name(project: Option<&Path>, id: &str) -> String {
    format!("{}.r{id}", autosave_name(project))
}

/// Name of the window's current snapshots. A project the window can write to, and the single unnamed
/// project, share a common name; a read-only window and a second window do not evict the owner's snapshots.
pub fn snapshot_name(project: Option<&Path>, read_only: bool, first: bool) -> String {
    let owner = project.filter(|_| !read_only);
    let base = autosave_name(owner);
    if owner.is_some() || (first && !read_only) {
        base
    } else {
        format!("{base}.i{}", std::process::id())
    }
}

/// Snapshots of an unbound copy restored from the damaged file `bad`: its own family, named after
/// that file, so they never rotate with other untitled work. It is a side family of the untitled
/// name, so a crash still offers it; a second window adds its process id like any other.
pub fn recovered_name(bad: &Path, first: bool) -> String {
    let stem = bad
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let base = format!("{UNTITLED}.recovered-{stem}-{}", crate::project::path_key(bad));
    if first {
        base
    } else {
        format!("{base}.i{}", std::process::id())
    }
}

/// Families `<base>.r*` and `<base>.i*` (without `.snap-<ms>`) that rotation of the common name leaves alone.
fn side_families(folder: &Path, base: &str) -> Vec<PathBuf> {
    let pre = format!("{base}.");
    let mut v: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let rest = name.strip_prefix(&pre)?;
            if !rest.starts_with(['r', 'i']) {
                return None;
            }
            let at = name.rfind(".snap-")?;
            Some(folder.join(&name[..at]))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

fn later_file(folder: &Path, project: Option<&Path>) -> PathBuf {
    let key = project.map_or_else(|| "untitled".to_string(), crate::project::path_key);
    folder.join(format!("later-{key}.txt"))
}

/// Later pressed for a project: returns how many times so far (this press included).
pub fn bump_later(folder: &Path, project: Option<&Path>) -> u32 {
    let f = later_file(folder, project);
    let n = std::fs::read_to_string(&f)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    let _ = std::fs::write(f, n.to_string());
    n
}

/// Offer for the file on the command line, if any: only that project's own snapshots count.
pub fn offer_for_arg(list: Vec<Recover>, arg: &Path) -> Option<Recover> {
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let key = crate::project::path_key(&abs(arg));
    list.into_iter().find(|r| {
        r.project
            .as_deref()
            .is_some_and(|p| crate::project::path_key(&abs(p)) == key)
    })
}

fn undecided_file(folder: &Path, project: Option<&Path>) -> PathBuf {
    let key = project.map_or_else(|| "untitled".to_string(), crate::project::path_key);
    folder.join(format!("undecided-{key}.txt"))
}

/// A promise to offer a project's snapshots at every launch until the user decides: a repeated
/// crash or closing the window without a choice does not cut off access to them.
pub fn mark_undecided(folder: &Path, project: Option<&Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    let text = project.map_or_else(String::new, |p| p.display().to_string());
    std::fs::write(undecided_file(folder, project), text)
}

/// Projects with unresolved recovery (`None` means no file).
pub fn undecided(folder: &Path) -> Vec<Option<PathBuf>> {
    let mut v: Vec<Option<PathBuf>> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("undecided-") && n.ends_with(".txt")
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .map(|t| {
            let line = t.lines().next().unwrap_or_default();
            (!line.is_empty()).then(|| PathBuf::from(line))
        })
        .collect();
    v.sort();
    v
}

/// Toast text after Later: the archived case names how many times it was put off.
fn later_text(c: &ddc_ui::Catalog, key: &str) -> String {
    c.tf(key, &[("n", &LATER_MAX.to_string())])
}

/// The user has decided: the promise is cleared, snapshots of dead windows go to `_archive` and are not deleted.
pub fn settle_recovery(folder: &Path, project: Option<&Path>) {
    let _ = std::fs::remove_file(undecided_file(folder, project));
    let _ = std::fs::remove_file(later_file(folder, project));
    release_held(&held_for(folder, project, None));
    for f in side_families(folder, &autosave_name(project)) {
        for s in ddc_model::snapshots_of(&f) {
            let _ = ddc_model::archive_snapshot(&s);
        }
    }
}

/// Dialogs for all unresolved projects; a promise with nothing left to offer is cleared.
pub fn pending_recoveries(folder: &Path) -> Vec<Recover> {
    undecided(folder)
        .into_iter()
        .filter_map(|p| {
            let r = with_held(scan_for(folder, true, p.as_deref()), folder, p.as_deref());
            if r.is_none() {
                let _ = std::fs::remove_file(undecided_file(folder, p.as_deref()));
            }
            r
        })
        .collect()
}

/// Session started: marker in the autosave folder; returns whether a marker already existed (crash exit).
pub fn begin_session(folder: &Path) -> bool {
    let lock = folder.join(SESSION_LOCK);
    let crashed = lock.exists();
    let _ = std::fs::create_dir_all(folder);
    let _ = std::fs::write(&lock, std::process::id().to_string());
    crashed
}

/// Clean exit: the marker is removed (an app file, not a user file).
pub fn end_session(folder: &Path) {
    let _ = std::fs::remove_file(folder.join(SESSION_LOCK));
}

/// Clean exit of this window: nobody will offer its own snapshots any more.
fn remove_instance_snapshots(folder: &Path) {
    let tag = format!(".i{}.snap-", std::process::id());
    for e in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().contains(&tag) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub(super) fn newest_first(snaps: &mut [SnapInfo]) {
    snaps.sort_by(|a, b| b.ms.cmp(&a.ms));
}

fn first_good(snaps: &[SnapInfo]) -> usize {
    snaps.iter().position(|s| s.damage.is_none()).unwrap_or(0)
}

/// Deferred snapshots, newest first.
pub fn held_snapshots(folder: &Path) -> Vec<SnapInfo> {
    let dir = folder.join(HELD_DIR);
    let mut bases: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.rsplit_once(".snap-").map(|(b, _)| b.to_string()))
        .collect();
    bases.sort();
    bases.dedup();
    let mut out: Vec<SnapInfo> = bases
        .iter()
        .flat_map(|b| ddc_model::snapshot_infos(&dir.join(b)))
        .collect();
    newest_first(&mut out);
    out
}

/// Held snapshots of one project (or of the untitled model), newer than `after`.
pub fn held_for(folder: &Path, project: Option<&Path>, after: Option<SystemTime>) -> Vec<SnapInfo> {
    let base = autosave_name(project);
    let min = after.map_or(0, |t| t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis()));
    held_snapshots(folder)
        .into_iter()
        .filter(|s| {
            let n = s.path.file_name().map(|f| f.to_string_lossy().into_owned());
            n.is_some_and(|n| {
                n.strip_prefix(&base).is_some_and(|r| {
                    r.starts_with(".snap-") || r.starts_with(".r") || r.starts_with(".i")
                })
            })
        })
        .filter(|s| s.ms > min)
        .collect()
}

/// Held snapshots of the same project join its offer; other projects' never do.
pub fn with_held(
    found: Option<Recover>,
    folder: &Path,
    project: Option<&Path>,
) -> Option<Recover> {
    let file_time = match &found {
        Some(r) => r.file_time,
        None => project.and_then(file_time_of),
    };
    let held = held_for(folder, project, file_time);
    if held.is_empty() {
        return found;
    }
    let mut r = found.unwrap_or_else(|| Recover {
        file: project.map_or_else(|| folder.join(UNTITLED), Path::to_path_buf),
        folder: folder.to_path_buf(),
        project: project.map(Path::to_path_buf),
        file_time,
        ..Default::default()
    });
    r.snaps.extend(held);
    newest_first(&mut r.snaps);
    r.pick = first_good(&r.snaps);
    Some(r)
}

fn is_held(p: &Path) -> bool {
    p.parent()
        .and_then(|d| d.file_name())
        .is_some_and(|n| n.to_str() == Some(HELD_DIR))
}

/// "Later": snapshots leave rotation, otherwise new autosaves would evict them.
/// Returns how many could not be moved.
pub fn hold_snapshots(folder: &Path, snaps: &[SnapInfo]) -> usize {
    let dir = folder.join(HELD_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return snaps.len();
    }
    snaps
        .iter()
        .filter(|s| !is_held(&s.path))
        .filter(|s| match s.path.file_name() {
            Some(name) => std::fs::rename(&s.path, dir.join(name)).is_err(),
            None => true,
        })
        .count()
}

/// Decision made: deferred snapshots go to the archive and are no longer offered.
pub fn release_held(snaps: &[SnapInfo]) -> usize {
    snaps
        .iter()
        .filter(|s| is_held(&s.path))
        .filter(|s| ddc_model::archive_snapshot(&s.path).is_err())
        .count()
}

/// Crash note: message, location, stack — `crash-<ms>.txt` in `folder`; the previous hook
/// is still called. The shell flushes the model (`flush_autosave`) after a panic is caught.
pub fn install_crash_handler(folder: Option<PathBuf>) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(d) = &folder {
            let _ = std::fs::create_dir_all(d);
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let note = format!(
                "DDC Editor crash\ntime: {}\npanic: {info}\n\n{}\n",
                fmt_time(SystemTime::now()),
                std::backtrace::Backtrace::force_capture()
            );
            write_crash_note(d, ms, note);
        }
        prev(info);
    }));
}

/// Bytes one crash note may take; a longer stack is cut at a character boundary.
pub const CRASH_NOTE_MAX: usize = 256 * 1024;
/// Crash notes kept beside the snapshots (at most 1 MiB); older ones move to `_archive`.
pub const CRASH_KEEP: usize = 4;
/// How long the crash path waits for a background snapshot before writing its own.
const CRASH_WAIT: Duration = Duration::from_secs(10);
/// A stuck button or an endless drag defers snapshots at most this long.
pub const GESTURE_CAP: Duration = Duration::from_secs(5 * 60);

/// Writes `crash-<ms>.txt` (clipped to `CRASH_NOTE_MAX`) and rotates the older notes.
pub fn write_crash_note(folder: &Path, ms: u128, mut note: String) {
    if note.len() > CRASH_NOTE_MAX {
        let mut cut = CRASH_NOTE_MAX;
        while !note.is_char_boundary(cut) {
            cut -= 1;
        }
        note.truncate(cut);
    }
    let _ = std::fs::write(folder.join(format!("crash-{ms}.txt")), note);
    rotate_crash_notes(folder);
}

fn crash_notes(folder: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        })
        .collect();
    v.sort();
    v
}

/// Only the newest `CRASH_KEEP` notes stay; the rest move to `_archive` (never deleted).
fn rotate_crash_notes(folder: &Path) {
    let notes = crash_notes(folder);
    let old = notes.len().saturating_sub(CRASH_KEEP);
    if old == 0 {
        return;
    }
    let archive = folder.join("_archive");
    let _ = std::fs::create_dir_all(&archive);
    for n in &notes[..old] {
        if let Some(name) = n.file_name() {
            let _ = std::fs::rename(n, archive.join(name));
        }
    }
}

/// A button is held or something is dragged: the model may hold a half-made edit.
fn mid_gesture(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.pointer.any_down()) || ctx.dragged_id().is_some()
}

/// Latest crash note in the folder (for the message in the recovery dialog).
pub fn last_crash_note(folder: &Path) -> Option<PathBuf> {
    crash_notes(folder).pop()
}

/// Local time "DD.MM HH:MM".
pub fn fmt_time(t: SystemTime) -> String {
    let (d, mo, h, mi) = local_parts(t);
    format!("{d:02}.{mo:02} {h:02}:{mi:02}")
}

/// Local time "HH:MM".
pub fn fmt_hhmm(t: SystemTime) -> String {
    let (_, _, h, mi) = local_parts(t);
    format!("{h:02}:{mi:02}")
}

#[cfg(windows)]
fn local_parts(t: SystemTime) -> (u16, u16, u16, u16) {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SysTime {
        year: u16,
        month: u16,
        dow: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn FileTimeToLocalFileTime(ft: *const FileTime, local: *mut FileTime) -> i32;
        fn FileTimeToSystemTime(ft: *const FileTime, st: *mut SysTime) -> i32;
    }
    let ticks = t
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() / 100) as u64
        + 116_444_736_000_000_000;
    let ft = FileTime {
        lo: ticks as u32,
        hi: (ticks >> 32) as u32,
    };
    let mut lt = FileTime::default();
    let mut st = SysTime::default();
    // SAFETY: plain out-parameters of documented kernel32 calls
    let ok = unsafe {
        FileTimeToLocalFileTime(&ft, &mut lt) != 0 && FileTimeToSystemTime(&lt, &mut st) != 0
    };
    if ok {
        (st.day, st.month, st.hour, st.minute)
    } else {
        utc_parts(t)
    }
}

#[cfg(not(windows))]
fn local_parts(t: SystemTime) -> (u16, u16, u16, u16) {
    utc_parts(t)
}

fn utc_parts(t: SystemTime) -> (u16, u16, u16, u16) {
    let s = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (s / 86_400, s % 86_400);
    // civil-from-days (proleptic Gregorian)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u16;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u16;
    (d, m, (rem / 3600) as u16, (rem % 3600 / 60) as u16)
}

fn ms_time(ms: u128) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms as u64)
}

impl EditorApp {
    pub(crate) fn autosave_file(&self) -> Option<PathBuf> {
        let p = &self.ed.project;
        let first = self.autosave.instance.is_some();
        let name = match p.recovered_from().filter(|_| p.path().is_none()) {
            Some(bad) => recovered_name(bad, first),
            None => snapshot_name(p.path(), p.read_only(), first),
        };
        self.prefs.autosave_folder().map(|d| d.join(name))
    }

    /// Another project was opened or it received a file: the note goes next to the working copy. A read-only
    /// project after a crash comes back without a name: "Save" will ask for a new one.
    pub(crate) fn note_session(&self) {
        let p = &self.ed.project;
        if let Some(w) = p.work_path() {
            crate::project::write_note(w, p.path().filter(|_| !p.read_only()));
        }
    }

    /// Read-only window with no edits: a snapshot protects nothing, the file is intact anyway.
    pub(crate) fn nothing_to_snapshot(&self) -> bool {
        self.ed.project.read_only() && !self.ed.is_dirty()
    }

    /// After a caught panic: the last snapshot and a promise to offer it at launch.
    /// A background copy still running finishes first, so two writers never rotate one family.
    pub fn crash_flush(&mut self) {
        // a stuck copy must not cost the edits: past the limit the final copy is written anyway
        self.wait_snapshot(CRASH_WAIT);
        self.flush_autosave();
        if let Some(dir) = self.prefs.autosave_folder() {
            let p = &self.ed.project;
            let _ = mark_undecided(&dir, p.path().filter(|_| !p.read_only()));
        }
    }

    /// Clean exit: session markers are removed wherever they are.
    pub fn end_autosave_session(&mut self) {
        let Some(dir) = self
            .autosave
            .bound
            .clone()
            .or_else(|| self.prefs.autosave_folder())
        else {
            return;
        };
        remove_instance_snapshots(&dir);
        if self.autosave.instance.is_some() {
            end_session(&dir);
        }
    }

    /// The autosave folder changed: session markers move with it, otherwise a crash in the new folder would
    /// go unnoticed and the old one would keep a false marker forever.
    fn rebind_session(&mut self) {
        let Some(old) = self.autosave.bound.clone() else {
            return;
        };
        let now = self.prefs.autosave_folder();
        if now.as_ref() == Some(&old) {
            return;
        }
        if self.autosave.instance.take().is_some() {
            end_session(&old);
        }
        if let Some(dir) = &now {
            begin_session(dir);
            self.autosave.instance = ProjectLock::acquire(&dir.join(SESSION_INSTANCE)).ok();
        }
        self.autosave.bound = now;
    }

    /// Snapshot scheduled: the working file is written on its own connection in the background, the in-memory model here.
    fn write_autosave(&mut self) {
        // the snapshot carries the placed drawings: when they are not written it is no success
        let side_err = crate::drawing::store::persist(self).err();
        if self.nothing_to_snapshot() {
            let before = self.autosave.counters();
            self.autosave.last_at = Instant::now();
            if side_err.is_some() {
                self.settle_autosave(before, side_err, None);
            } else {
                self.autosave.snapshot_taken(self.ed.revision);
            }
            return;
        }
        let work = self.ed.project.work_path().map(Path::to_path_buf);
        let (Some(work), Some(file)) = (work, self.autosave_file()) else {
            self.flush_autosave();
            return;
        };
        let keep = self.prefs.autosave_keep;
        let a = &mut self.autosave;
        a.last_at = Instant::now();
        a.taking = Some((a.counters(), side_err));
        a.snapshot_taken(self.ed.revision);
        a.pending = true;
        a.job = Some(Job::spawn("autosave.saving", move |_| {
            ddc_model::autosave_file(&work, &file, keep).map_err(|e| e.to_string())
        }));
    }

    /// Every frame: schedule a snapshot, write the scheduled one, remind about saving.
    pub(super) fn autosave_tick(&mut self, ctx: &egui::Context) {
        if !self.prefs.autosave {
            return;
        }
        if self.autosave.job.is_some() {
            if self.poll_snapshot() {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
            return;
        }
        // a revision moved by a reload or a level walk is no edit: the file already holds it all
        let edited = self.ed.is_dirty();
        let a = &mut self.autosave;
        if !mid_gesture(ctx) {
            a.held_since = None;
        } else if a.held_since.get_or_insert_with(Instant::now).elapsed() < GESTURE_CAP {
            // a copy taken mid-drag holds a half-made edit: it waits for the release
            if a.pending || a.changes(self.ed.revision) > 0 {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
            return;
        }
        if a.pending {
            a.pending = false;
            self.write_autosave();
            return;
        }
        let changes = if edited { a.changes(self.ed.revision) } else { 0 };
        let p = &self.prefs;
        if kit::autosave::is_due(p, changes, a.last_at.elapsed()) {
            // indicator first, write on the next frame
            a.pending = true;
            ctx.request_repaint();
        } else if changes > 0 {
            ctx.request_repaint_after(Duration::from_secs(5));
        }
        let remind = Duration::from_secs(u64::from(p.remind_min) * 60);
        if p.remind_min > 0 && changes > 0 && a.remind_at.elapsed() >= remind {
            a.remind_at = Instant::now();
            let msg = self.sh.cat.t("autosave.remind_toast");
            self.sh.toasts.push(Level::Info, msg, None);
        }
    }

    /// The background snapshot, if one runs: settled when it ended; `true` while it still runs.
    fn poll_snapshot(&mut self) -> bool {
        let a = &mut self.autosave;
        let Some(j) = &mut a.job else {
            return false;
        };
        let res = match j.poll() {
            Poll::Running => return true,
            Poll::Done(p) => Some(Ok(p)),
            Poll::Failed(e) => Some(Err(e)),
            Poll::Cancelled => None,
        };
        a.job = None;
        a.pending = false;
        let (before, side_err) = a.taking.take().unwrap_or((a.counters(), None));
        match res {
            Some(Ok(p)) => self.settle_autosave(before, side_err, Some(p)),
            Some(Err(e)) => self.settle_autosave(before, Some(e), None),
            None => self.autosave.restore(before),
        }
        false
    }

    /// Waits up to `limit` for the background snapshot; `true` when none runs any more.
    fn wait_snapshot(&mut self, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        while self.poll_snapshot() {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Snapshot now (also from crash capture). An error shows ⚠ in the status bar.
    /// Path of the written snapshot; drawings left out still count as a failure.
    pub fn flush_autosave(&mut self) -> Option<PathBuf> {
        let side_err = crate::drawing::store::persist(self).err();
        if self.nothing_to_snapshot() {
            return None;
        }
        let file = self.autosave_file()?;
        self.autosave.last_at = Instant::now();
        let before = self.autosave.counters();
        match self.ed.model.autosave_as(&file, self.prefs.autosave_keep) {
            Ok(p) => {
                self.autosave.snapshot_taken(self.ed.revision);
                self.settle_autosave(before, side_err, Some(p.clone()));
                Some(p)
            }
            Err(e) => {
                self.settle_autosave(before, Some(e.to_string()), None);
                None
            }
        }
    }

    /// One attempt ended. "Saved at" only when the model and the drawings both reached disk;
    /// otherwise its edits stay owed and the next interval tries again.
    fn settle_autosave(&mut self, before: (u64, u64), err: Option<String>, path: Option<PathBuf>) {
        let a = &mut self.autosave;
        if path.is_some() {
            a.last_path = path;
        }
        if err.is_some() {
            a.restore(before);
        } else {
            a.last_ok = Some(fmt_hhmm(SystemTime::now()));
        }
        let n = a.record(err);
        self.announce_autosave(n);
    }

    /// Humanized reason of the current autosave failure.
    pub fn autosave_reason(&self) -> Option<String> {
        let ru = matches!(self.sh.cat.lang, ddc_ui::i18n::Lang::Ru);
        // the same cause words as the failed-save dialog
        self.autosave.failed.as_deref().map(|e| {
            crate::project_ui::cause_text(&self.sh.cat, e)
                .unwrap_or_else(|| crate::ui_err::humanize(e, ru))
        })
    }

    fn announce_autosave(&mut self, n: Notice) {
        match n {
            Notice::None => {}
            Notice::Failed => {
                let why = self.autosave_reason().unwrap_or_default();
                let c = &self.sh.cat;
                let msg = c.tf("autosave_status.autosave_failed", &[("why", &why)]);
                let act = c.t("autosave_status.autosave_open");
                self.sh
                    .toasts
                    .push_cmd(Level::Warning, msg, act, AUTOSAVE_SHOW);
            }
            Notice::Recovered => {
                let msg = self.sh.cat.t("autosave_status.autosave_recovered");
                self.sh.toasts.push(Level::Info, msg, None);
            }
        }
    }

    /// Autosave indicator text and an error flag.
    pub fn autosave_label(&self) -> (String, bool) {
        let c = &self.sh.cat;
        let a = &self.autosave;
        if !self.prefs.autosave {
            return (c.t("autosave.off"), false);
        }
        if a.failed.is_some() {
            return (c.t("autosave.failed"), true);
        }
        if a.pending {
            return (c.t("autosave.saving"), false);
        }
        match &a.last_ok {
            Some(t) => (c.tf("autosave.saved_at", &[("time", t)]), false),
            None => (c.t("autosave.idle"), false),
        }
    }

    /// Start of any window: working copies of dead windows (their lock is free) become snapshots,
    /// then a dialog per project that has no decision yet. Corrupt snapshots do not
    /// block startup; a copy that failed to produce a snapshot is deferred, and the user is told so.
    pub fn check_recovery(&mut self, crashed: bool) {
        let Some(dir) = self.prefs.autosave_folder() else {
            return;
        };
        let instance = crate::gfx_guard::acquire_lock(&dir.join(SESSION_INSTANCE));
        self.autosave.instance = instance.ok();
        self.autosave.bound = Some(dir.clone());
        self.note_session();
        // the mark of a live first window is no crash; without a working file it is all there is
        if crashed && self.autosave.instance.is_some() {
            let _ = mark_undecided(&dir, None);
        }
        let swept = crate::project::sweep_work(&dir);
        self.autosave.queue = pending_recoveries(&dir);
        self.recover = self.autosave.queue.pop();
        if !swept.kept.is_empty() {
            if swept.stranded.is_empty() {
                crate::project_ui::notify_kept(self, &swept.kept);
            } else {
                crate::project_ui::notify_stranded(self, &swept.kept, &swept.stranded);
            }
        }
    }

    /// A file came on the command line: its own offer (if any) becomes a banner, the modal
    /// never stands in front of the file. Other offers stay on disk for the next plain start.
    pub(crate) fn recovery_to_banner(&mut self, arg: &Path) {
        let mut all: Vec<Recover> = self.recover.take().into_iter().collect();
        all.append(&mut self.autosave.queue);
        self.autosave.banner = offer_for_arg(all, arg);
    }

    /// Non-modal strip over the canvas: Review opens the dialog, Later sets the offer aside.
    fn recover_banner(&mut self, ctx: &egui::Context) {
        let Some(r) = self.autosave.banner.as_ref() else {
            return;
        };
        let name = r
            .project
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let full = r
            .project
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let (c, th) = (&self.sh.cat, &self.sh.theme);
        let s = th.sem();
        let (mut review, mut later) = (false, false);
        egui::Area::new(egui::Id::new("recover_banner"))
            .anchor(
                egui::Align2::CENTER_TOP,
                egui::vec2(0.0, ribbon::TAB_H + space::XL),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .fill(s.bg_elevated)
                    .stroke(egui::Stroke::new(stroke::HAIRLINE, s.border_subtle))
                    .corner_radius(th.radius("lg"))
                    .inner_margin(egui::Margin::symmetric(space::MD as i8, space::SM as i8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (ir, _) = ui.allocate_exact_size(
                                egui::Vec2::splat(icon_size::MD),
                                egui::Sense::hover(),
                            );
                            widgets::with_icons(ui.ctx(), |icons| {
                                icons.paint(ui, "warning", icon_size::MD as u32, ir, s.warning)
                            });
                            let text = c.tf("recover.banner", &[("name", name.as_str())]);
                            let font = th.font(Role::Body);
                            let want = ui
                                .painter()
                                .layout_no_wrap(text.clone(), font.clone(), s.text_primary)
                                .size()
                                .x;
                            let w = want.min(layout::DIALOG_CONFIRM);
                            let (tr, tresp) = ui.allocate_exact_size(
                                egui::vec2(w, th.control_h(ControlSize::Medium)),
                                egui::Sense::hover(),
                            );
                            widgets::text::paint_in(
                                ui,
                                tr,
                                &text,
                                font,
                                s.text_primary,
                                egui::Align::Min,
                            );
                            if !full.is_empty() {
                                tresp.on_hover_text(full.as_str());
                            }
                            ui.add_space(space::SM);
                            if Button::primary(&c.t("recover.banner_review"))
                                .show(ui, th)
                                .clicked()
                            {
                                review = true;
                            }
                            if Button::quiet(&c.t("recover.later"))
                                .tooltip(&c.t("recover.later_tip"), "")
                                .show(ui, th)
                                .clicked()
                            {
                                later = true;
                            }
                        });
                    });
            });
        if review {
            self.recover = self.autosave.banner.take();
        } else if later {
            if let Some(r) = self.autosave.banner.take() {
                self.set_aside(&r);
            }
        }
    }

    /// Later: snapshots leave the rotation; the third time they count as handled and go to
    /// `_archive` (nothing is deleted).
    fn set_aside(&mut self, r: &Recover) {
        let Some(dir) = self.prefs.autosave_folder() else {
            return;
        };
        let failed = hold_snapshots(&dir, &r.snaps);
        let key = if failed > 0 {
            "recover.later_failed"
        } else if bump_later(&dir, r.project.as_deref()) >= LATER_MAX {
            // the held copies of this project go to _archive with the marks
            settle_recovery(&dir, r.project.as_deref());
            "recover.later_archived"
        } else {
            "recover.later_done"
        };
        let level = if failed > 0 { Level::Warning } else { Level::Info };
        let msg = later_text(&self.sh.cat, key);
        self.sh.toasts.push(level, msg, None);
    }

    /// Next dialog from the queue once the current one is closed.
    pub(crate) fn next_recovery(&mut self) {
        self.recover = self.autosave.queue.pop();
    }

    /// The snapshot's working copy becomes the model. The question about unsaved edits is asked in `project_ui`.
    /// `unbound`: a snapshot of the damaged `project`, which stays bound to no file.
    pub(crate) fn restore(
        &mut self,
        snap: &Path,
        project: Option<&Path>,
        unbound: bool,
    ) -> Result<(), String> {
        let work = crate::project::WorkFile::create().map_err(|e| e.to_string())?;
        let m = Model::open_snapshot(snap, work.path()).map_err(|e| e.to_string())?;
        self.ed.replace_model(m).map_err(|e| e.to_string())?;
        match project.filter(|_| unbound) {
            Some(damaged) => self.ed.project_recovered_unbound(work, damaged),
            None => self.ed.project_recovered(work, project),
        }
        Ok(())
    }

    /// "Recovery" window.
    pub(super) fn recover_window(&mut self, ctx: &egui::Context) {
        self.recover_banner(ctx);
        let Some(mut r) = self.recover.take() else {
            return;
        };
        let c = &self.sh.cat;
        let mut close = false;
        // leaving the dialog any way but restore or discard: nothing is lost
        let mut later = false;
        let mut act: Option<u8> = None;
        let th = &self.sh.theme;
        if self.chrome.recover_note.is_none() {
            self.chrome.recover_note = Some(
                self.prefs
                    .autosave_folder()
                    .and_then(|d| last_crash_note(&d)),
            );
        }
        let note = self.chrome.recover_note.clone().flatten();
        let best = r.snaps.iter().position(rows::restorable);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let ask_id = egui::Id::new("ddc.recover.ask");
        let ask_focus_id = egui::Id::new("ddc.recover.ask_focus");
        let mut pending: Option<Danger> = ctx.data(|d| d.get_temp(ask_id));
        let modal = kit::key_owner(ctx) == kit::KeyOwner::Modal;
        // Up/Down/Home/End pick a row, Enter only moves focus to Restore (it replaces the model)
        let nav = if modal && pending.is_none() {
            ctx.input_mut(|i| {
                let n = egui::Modifiers::NONE;
                if i.consume_key(n, egui::Key::ArrowDown) {
                    Some(Nav::Down)
                } else if i.consume_key(n, egui::Key::ArrowUp) {
                    Some(Nav::Up)
                } else if i.consume_key(n, egui::Key::Home) {
                    Some(Nav::First)
                } else if i.consume_key(n, egui::Key::End) {
                    Some(Nav::Last)
                } else {
                    None
                }
            })
        } else {
            None
        };
        let pick0 = r.pick;
        if let Some(n) = nav {
            r.pick = rows::next_pick(&r.snaps, r.pick, n);
            if r.pick != pick0 {
                r.diff = None;
            }
        }
        let focus_restore = modal
            && pending.is_none()
            && ctx.memory(|m| m.focused().is_none())
            && ctx.input(|i| kit::fresh_key(i, egui::Key::Enter))
            && rows::enter_focuses_restore(&r.snaps, r.pick);
        let mut ev: Option<Ev> = None;
        let _ = kit::modal_dialog("recover", ctx, |ui| {
            ui.set_width(ddc_ui::theme::layout::DIALOG_TOOL);
            kit::modal_title(ui, c.t("recover.title"));
            ui.label(RichText::new(c.t("recover.lead")).color(th.c("text.secondary")));
            if let Some(n) = &note {
                let name = n
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.label(
                    RichText::new(c.tf("recover.note", &[("name", name.as_str())]))
                        .font(th.font(Role::Caption))
                        .color(th.c("text.secondary")),
                )
                .on_hover_text(n.display().to_string());
            }
            ui.add_space(space::MD);
            // the queue holds one dialog per project: say which one this is
            if let Some(name) = r.project.as_ref().and_then(|p| p.file_name()) {
                kit::elided(ui, &name.to_string_lossy());
            }
            let note = match (r.project.is_some(), r.file_time) {
                (false, _) => c.t("recover.untitled_note"),
                (true, Some(t)) => c.tf("recover.file_time", &[("time", fmt_time(t).as_str())]),
                (true, None) => c.t("recover.file_missing"),
            };
            ui.label(
                RichText::new(note)
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
            ui.add_space(space::XS);
            let sem = th.sem();
            let mut picked = None;
            match rows::list_state(&r.snaps) {
                ListState::Empty => {
                    let hint = c.t("recover.empty_hint");
                    kit::empty_note(ui, c.t("recover.empty"), Some(hint.as_str()));
                }
                state => {
                    if state == ListState::AllDamaged {
                        kit::warn_note(ui, c.t("recover.all_damaged"));
                    }
                    let row_h = th.control_h(ControlSize::Large);
                    // a long list scrolls: the buttons below must stay inside the window
                    let list_h = (row_h + space::XS) * 4.0;
                    egui::ScrollArea::vertical()
                        .id_salt("recover.rows")
                        .max_height(list_h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = space::XS;
                        for (i, s) in r.snaps.iter().enumerate() {
                            let ok = rows::restorable(s);
                            let (row, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h),
                                if ok {
                                    egui::Sense::click()
                                } else {
                                    egui::Sense::hover()
                                },
                            );
                            let on = r.pick == i && ok;
                            let fg = if ok {
                                widgets::paint_state(ui, th, row, &resp, on)
                            } else {
                                sem.text_secondary
                            };
                            let inner = row.shrink2(egui::vec2(space::SM, 0.0));
                            // right to left: size, then the tag; the time gets what is left
                            let mut cu = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(inner)
                                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                            );
                            cu.add(
                                egui::Label::new(
                                    RichText::new(kit::fmt_size(c, s.bytes))
                                        .font(th.font(Role::Numeric))
                                        .color(sem.text_secondary),
                                )
                                .selectable(false),
                            );
                            cu.add_space(space::SM);
                            let tag = if !ok {
                                Some((c.t("recover.damaged"), Tone::Warning))
                            } else if best == Some(i) {
                                Some((c.t("recover.recommended"), Tone::Success))
                            } else {
                                None
                            };
                            if let Some((text, tone)) = &tag {
                                widgets::badge(&mut cu, th, text, *tone);
                            }
                            let stop = cu.min_rect().left() - space::SM;
                            let dot = egui::pos2(inner.left() + control::CHECK / 2.0, row.center().y);
                            let p = ui.painter();
                            p.circle_stroke(
                                dot,
                                control::CHECK / 2.0,
                                egui::Stroke::new(stroke::CONTROL, fg),
                            );
                            if on {
                                p.circle_filled(dot, control::CHECK / 4.0, sem.accent);
                            }
                            let abs = fmt_time(ms_time(s.ms));
                            let x = inner.left() + control::CHECK + space::SM;
                            let font = th.font(Role::Numeric);
                            let abs_w = ui
                                .painter()
                                .layout_no_wrap(abs.clone(), font.clone(), fg)
                                .size()
                                .x;
                            let band = |x0: f32, x1: f32| {
                                egui::Rect::from_min_max(
                                    egui::pos2(x0, row.top()),
                                    egui::pos2(x1.max(x0), row.bottom()),
                                )
                            };
                            let cut = widgets::text::paint_in(
                                ui,
                                band(x, stop),
                                &abs,
                                font,
                                fg,
                                egui::Align::Min,
                            );
                            let rel_x = x + abs_w + space::SM;
                            if !cut && stop - rel_x > icon_size::LG {
                                widgets::text::paint_in(
                                    ui,
                                    band(rel_x, stop),
                                    &rows::rel_time(c, now_ms, s.ms),
                                    th.font(Role::Caption),
                                    sem.text_secondary,
                                    egui::Align::Min,
                                );
                            }
                            if on && nav.is_some() {
                                resp.scroll_to_me(None);
                            }
                            let resp = match &s.damage {
                                Some(why) => resp.on_hover_text(
                                    c.tf("recover.row_damaged_tip", &[("reason", why.as_str())]),
                                ),
                                None => resp.on_hover_text(
                                    c.tf("recover.row_tip", &[("time", abs.as_str())]),
                                ),
                            };
                            if resp.clicked() {
                                picked = Some(i);
                            }
                        }
                    });
                        });
                    if state == ListState::Ready {
                        ui.label(
                            RichText::new(c.t("recover.keys_hint"))
                                .font(th.font(Role::Caption))
                                .color(sem.text_secondary),
                        );
                    }
                }
            }
            if let Some(i) = picked {
                r.pick = i;
                r.diff = None;
            }
            if let Some(d) = &r.diff {
                ui.add_space(space::SM);
                match d {
                    Ok(v) if v.is_empty() => {
                        ui.label(c.t("recover.same"));
                    }
                    Ok(v) => {
                        crate::collab::diff_view::table_grid(ui, c, v);
                    }
                    Err(e) => kit::error_note(ui, e),
                }
            }
            ui.add_space(space::MD);
            let good = rows::restore_target(&r.snaps, r.pick).is_some();
            if let Some(d) = pending {
                let ask = match d {
                    Danger::ArchiveOne => c.t("recover.archive_ask"),
                    Danger::DiscardAll => c.plural("recover.discard_ask", r.snaps.len() as u64),
                };
                kit::warn_note(ui, ask);
            }
            let any = r.snaps.get(r.pick).is_some();
            let mut left_ev = None;
            // destructive moves sit apart on the left and ask first; the filled button is the
            // answer to that question only
            let left = |ui: &mut egui::Ui| match pending {
                None => {
                    if kit::danger_button(ui, c.t("recover.archive_move"), any)
                        .on_hover_text(c.t("recover.archive_tip"))
                        .clicked()
                    {
                        left_ev = Some(Ev::Ask(Danger::ArchiveOne));
                    }
                    if kit::danger_button(ui, c.t("recover.discard_all"), any)
                        .on_hover_text(c.t("recover.discard_tip"))
                        .clicked()
                    {
                        left_ev = Some(Ev::Ask(Danger::DiscardAll));
                    }
                }
                Some(_) => {
                    // the safe answer takes focus; Enter never confirms the move
                    let keep = Button::secondary(&c.t("recover.confirm_no")).show(ui, th);
                    if ui.ctx().data_mut(|m| m.remove_temp::<bool>(ask_focus_id)).is_some() {
                        keep.request_focus();
                    }
                    if keep.clicked() {
                        left_ev = Some(Ev::Cancel);
                    }
                    let yes = c.t("recover.confirm_yes");
                    if kit::confirm_danger_button(ui, yes, true).clicked() {
                        left_ev = Some(Ev::Confirm);
                    }
                }
            };
            kit::dialog_footer_split(ui, left, |ui| {
                // replaces the open model: a click only, Enter just lands focus here
                let restore = kit::primary_button(ui, c.t("recover.restore"), good)
                    .on_disabled_hover_text(c.t("recover.damaged"));
                if focus_restore {
                    restore.request_focus();
                }
                if restore.clicked() {
                    act = Some(0);
                }
                if Button::secondary(&c.t("recover.later"))
                    .tooltip(&c.t("recover.later_tip"), "Esc")
                    .show(ui, th)
                    .clicked()
                {
                    later = true;
                }
                let compare = ui
                    .add_enabled_ui(good, |ui| {
                        Button::secondary(&c.t("recover.compare")).show(ui, th)
                    })
                    .inner
                    .on_disabled_hover_text(c.t("recover.damaged"));
                if compare.clicked() {
                    act = Some(1);
                }
            });
            if left_ev.is_some() {
                ev = left_ev;
            }
        });
        // Esc closes a pending question first; otherwise it keeps every snapshot for later.
        // A click beside the dialog does nothing.
        if modal && ctx.input(|i| kit::fresh_key(i, egui::Key::Escape)) {
            if pending.is_some() {
                ev = Some(Ev::Cancel);
            } else {
                later = true;
            }
        }
        // choosing another row drops a pending question
        if pending.is_some() && r.pick != pick0 {
            ev = Some(Ev::Cancel);
        }
        if let Some(e) = ev {
            if matches!(e, Ev::Ask(_)) {
                ctx.data_mut(|m| m.insert_temp(ask_focus_id, true));
            }
            let (next, run) = rows::confirm_step(pending, e);
            pending = next;
            match run {
                Some(Danger::ArchiveOne) => act = Some(2),
                Some(Danger::DiscardAll) => act = Some(3),
                None => {}
            }
            ctx.request_repaint();
        }
        if later || matches!(act, Some(0) | Some(1)) {
            pending = None;
        }
        ctx.data_mut(|m| rows::store_pending(m, ask_id, pending));
        let snap = if matches!(act, Some(0) | Some(1)) {
            rows::restore_target(&r.snaps, r.pick).map(Path::to_path_buf)
        } else {
            r.snaps.get(r.pick).map(|s| s.path.clone())
        };
        let mut handled = false;
        if later {
            self.set_aside(&r);
            handled = true;
            close = true;
            act = None;
        }
        match (act, snap) {
            (Some(0), Some(p)) => {
                // the same guard as New and Open; the dialog comes back if the answer is no
                crate::project_ui::request_restore(self, r, p);
                return;
            }
            (Some(1), Some(p)) => {
                r.diff = Some(
                    self.ed
                        .model
                        .compare_snapshot(&p)
                        .map_err(|e| e.to_string()),
                );
            }
            (Some(2), Some(p)) => {
                if let Err(e) = ddc_model::archive_snapshot(&p) {
                    r.diff = Some(Err(e.to_string()));
                } else {
                    r.snaps.remove(r.pick);
                    r.pick = first_good(&r.snaps);
                    r.diff = None;
                    close |= r.snaps.is_empty();
                }
            }
            (Some(3), _) => {
                let failed = r
                    .snaps
                    .iter()
                    .find_map(|s| ddc_model::archive_snapshot(&s.path).err());
                r.snaps.retain(|s| s.path.exists());
                r.pick = first_good(&r.snaps);
                r.diff = failed.map(|e| Err(e.to_string()));
                close |= r.snaps.is_empty();
            }
            _ => {}
        }
        if close {
            self.chrome.recover_note = None;
            if !handled {
                settle_recovery(&r.folder, r.project.as_deref());
            }
            self.next_recovery();
        } else {
            self.recover = Some(r);
        }
    }

    /// "Autosave" window (Manage).
    pub(super) fn autosave_window(&mut self, ctx: &egui::Context) {
        let mut open = self.autosave_open;
        let mut changed = false;
        let mut now = false;
        let mut browse = false;
        let mut left_folder = false;
        let c = &self.sh.cat;
        let p = &mut self.prefs;
        kit::Dialog::new("autosave", c.t("autosave.title"), kit::DialogSize::Form)
            .open(&mut open)
            .show(ctx, |ui| {
                let th = &self.sh.theme;
                changed |= widgets::checkbox(ui, th, &mut p.autosave, &c.t("autosave.enable"))
                    .changed();
                ui.add_space(space::SM);
                // folder edits apply on focus loss, not per keystroke: a half-typed path would become a folder
                let on = p.autosave;
                egui::Grid::new("ddc.autosave.grid")
                    .num_columns(2)
                    .spacing([space::LG, space::MD])
                    .show(ui, |ui| {
                        let mut host = |ui: &mut egui::Ui,
                                        keys: [&'static str; 2],
                                        add: &mut dyn FnMut(&mut egui::Ui)| {
                            ui.label(c.t(keys[0])).on_hover_text(c.t(keys[1]));
                            ui.horizontal(|ui| add(ui));
                            ui.end_row();
                        };
                        let r = kit::autosave_section(ui, th, c, p, on, &mut host);
                        changed |= r.changed;
                        browse = r.browse;
                        left_folder = r.left_folder;
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    now = ui
                        .add_enabled_ui(p.autosave, |ui| {
                            Button::secondary(&c.t("autosave.now")).show(ui, th)
                        })
                        .inner
                        .clicked();
                    let (txt, bad) = (
                        self.autosave
                            .last_ok
                            .as_ref()
                            .map(|t| c.tf("autosave.saved_at", &[("time", t)]))
                            .unwrap_or_else(|| c.t("autosave.idle")),
                        self.autosave.failed.clone(),
                    );
                    match bad {
                        Some(e) => ui.colored_label(th.sem().warning, e),
                        None => {
                            ui.label(RichText::new(txt).color(self.sh.theme.c("text.secondary")))
                        }
                    };
                });
            });
        self.autosave_open = open;
        if browse {
            if let Some(d) = crate::file_pick::Dlg::new().pick_folder() {
                self.prefs.autosave_dir = d.display().to_string();
                changed = true;
            }
        }
        if changed {
            self.prefs.save();
        }
        if browse || left_folder || !open {
            self.rebind_session();
        }
        if now {
            self.flush_autosave();
        }
    }
}

/// One frame of the open autosave window for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context) {
    app.autosave_open = true;
    app.autosave_window(ctx);
}

/// One frame of the recovery dialog for the layout contract (`app.recover` holds its state).
#[cfg(test)]
pub(crate) fn layout_recover(app: &mut EditorApp, ctx: &egui::Context) {
    // the crash note reads the autosave folder: none is on disk here
    app.chrome.recover_note = Some(None);
    app.recover_window(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_by_time_or_edits() {
        let m = Duration::from_secs(60);
        assert!(!due(0, m * 100, 5, 10), "no edits — no snapshot");
        assert!(!due(3, m * 4, 5, 10));
        assert!(due(3, m * 5, 5, 10));
        assert!(due(10, m, 5, 10));
        assert!(!due(10, m, 5, 0), "edit trigger off");
        assert!(due(1, m, 0, 0), "interval clamps to 1 min");
    }

    #[test]
    fn scan_offers_only_after_crash_or_newer_than_file() {
        let d = std::env::temp_dir().join(format!("ddc-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(!begin_session(&d));
        assert!(begin_session(&d), "lock left by the previous run");
        let m = Model::open(":memory:").unwrap();
        m.autosave_as(&d.join(UNTITLED), 3).unwrap();
        assert!(
            scan(&d, false).is_none(),
            "clean exit, no file: nothing to offer"
        );
        let r = scan(&d, true).unwrap();
        assert_eq!(r.snaps.len(), 1);
        // damaged newest snapshot: listed, not picked
        std::thread::sleep(Duration::from_millis(5));
        let bad = m.autosave_as(&d.join(UNTITLED), 3).unwrap();
        std::fs::write(&bad, b"junk").unwrap();
        let r = scan(&d, true).unwrap();
        assert_eq!(r.snaps.len(), 2);
        assert!(r.snaps[0].damage.is_some());
        assert_eq!(r.pick, 1);
        end_session(&d);
        assert!(!d.join(SESSION_LOCK).exists());
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn project_snapshots_have_their_own_name_and_come_back_only_after_a_crash() {
        let d = std::env::temp_dir().join(format!("ddc-rec-p-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let proj = d.join("house.ddc");
        std::fs::write(&proj, b"file").unwrap();
        assert_eq!(autosave_name(None), UNTITLED);
        let name = autosave_name(Some(&proj));
        assert!(name.starts_with("house-") && name.ends_with(".ddc"));
        assert_ne!(name, autosave_name(Some(&d.join("sub").join("house.ddc"))));
        let m = Model::open(":memory:").unwrap();
        // the file was saved before this snapshot
        std::thread::sleep(Duration::from_millis(1100));
        m.autosave_as(&d.join(&name), 3).unwrap();
        assert!(
            scan_for(&d, false, Some(&proj)).is_none(),
            "clean exit: the choice not to save stays decided"
        );
        let r = scan_for(&d, true, Some(&proj)).unwrap();
        assert_eq!(r.file, proj);
        assert_eq!(r.project.as_deref(), Some(proj.as_path()));
        assert_eq!(r.snaps.len(), 1);
        assert!(scan_for(&d, true, None).is_none(), "untitled has its own");
        // a save after the snapshot: nothing newer than the file
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&proj, b"saved again").unwrap();
        assert!(scan_for(&d, true, Some(&proj)).is_none());
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-rec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A window that died: its working file with an edit and its note, nothing else.
    fn dead_window(work: &Path, id: &str, project: Option<&Path>) -> PathBuf {
        let w = work.join(format!("{id}.ddc"));
        let mut m = Model::open_working(w.to_str().unwrap()).unwrap();
        m.add_level("edit", 3.0).unwrap();
        drop(m);
        crate::project::write_note(&w, project);
        w
    }

    /// Window B dies after window A closed normally and took `session.lock` with it: no session
    /// mark anywhere, yet B's edits come back under B's project.
    #[test]
    fn a_second_window_recovers_with_no_session_mark() {
        let d = scratch("second");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let proj = d.join("b.ddc");
        std::fs::write(&proj, b"old").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let w = dead_window(&work, "2-0-1", Some(&proj));
        assert!(
            !begin_session(&auto),
            "no mark: not a crash by the old test"
        );
        end_session(&auto);
        let swept = crate::project::sweep_dir(&work, &auto);
        assert!(swept.kept.is_empty());
        assert!(!w.exists());
        let found = pending_recoveries(&auto);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].project.as_deref(), Some(proj.as_path()));
        assert_eq!(found[0].snaps.len(), 1);
        assert!(found[0].snaps[0].damage.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A window still alive holds its lock: a window starting beside it takes nothing away.
    #[test]
    fn a_live_window_keeps_its_working_file_and_two_dead_ones_both_come_back() {
        let d = scratch("two");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let (pa, pb) = (d.join("a.ddc"), d.join("b.ddc"));
        let live = dead_window(&work, "3-0-1", None);
        let held = ProjectLock::acquire(&live).unwrap();
        dead_window(&work, "4-0-1", Some(&pa));
        dead_window(&work, "5-0-1", Some(&pb));
        assert!(crate::project::sweep_dir(&work, &auto).kept.is_empty());
        assert!(live.exists());
        let mut who: Vec<Option<PathBuf>> = pending_recoveries(&auto)
            .into_iter()
            .map(|r| r.project)
            .collect();
        who.sort();
        let mut want = vec![Some(pa), Some(pb)];
        want.sort();
        assert_eq!(
            who, want,
            "one dialog per project; the live window's is not one"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A crash, then a crash again (or the window closed with no choice): the snapshot is still
    /// offered, with no working file, session mark or note left to point at it.
    #[test]
    fn a_repeated_crash_keeps_the_offer_until_the_user_decides() {
        let d = scratch("repeat");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let proj = d.join("p.ddc");
        dead_window(&work, "6-0-1", Some(&proj));
        crate::project::sweep_dir(&work, &auto);
        assert_eq!(pending_recoveries(&auto).len(), 1);
        // the next session writes its own snapshots of the project until rotation is full
        let m = Model::open(":memory:").unwrap();
        for _ in 0..4 {
            m.autosave_as(&auto.join(autosave_name(Some(&proj))), 1)
                .unwrap();
        }
        crate::project::sweep_dir(&work, &auto);
        let again = pending_recoveries(&auto);
        assert_eq!(again.len(), 1, "second start, no new crash: still offered");
        assert_eq!(
            again[0].snaps.len(),
            2,
            "rotation did not take the recovered one"
        );
        settle_recovery(&auto, Some(&proj));
        assert!(pending_recoveries(&auto).is_empty(), "decided");
        assert!(
            auto.join("_archive").read_dir().unwrap().count() >= 1,
            "moved, not deleted"
        );
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Offer with nothing to offer goes away by itself: the file was saved after the snapshot.
    #[test]
    fn an_offer_older_than_the_file_is_dropped() {
        let d = scratch("older");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let proj = d.join("p.ddc");
        dead_window(&work, "7-0-1", Some(&proj));
        crate::project::sweep_dir(&work, &auto);
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&proj, b"saved later").unwrap();
        assert!(pending_recoveries(&auto).is_empty());
        assert!(undecided(&auto).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names_keep_the_owner_family_apart() {
        let p = Path::new("C:/a/house.ddc");
        let owner = autosave_name(Some(p));
        assert_eq!(snapshot_name(Some(p), false, false), owner);
        assert_eq!(snapshot_name(Some(p), false, true), owner);
        // read-only: never the owner's name, whichever window
        assert_ne!(snapshot_name(Some(p), true, true), owner);
        assert!(!snapshot_name(Some(p), true, true).starts_with(&owner));
        assert_eq!(snapshot_name(None, false, true), UNTITLED);
        assert_ne!(snapshot_name(None, false, false), UNTITLED);
    }

    /// Read-only window: its rotation must not push out what the owner of the file wrote.
    #[test]
    fn a_read_only_window_does_not_rotate_the_owner_out() {
        let d = scratch("ro");
        let p = d.join("house.ddc");
        let m = Model::open(":memory:").unwrap();
        let owner = d.join(snapshot_name(Some(&p), false, true));
        let kept = m.autosave_as(&owner, 2).unwrap();
        let ro = d.join(snapshot_name(Some(&p), true, false));
        for _ in 0..5 {
            m.autosave_as(&ro, 2).unwrap();
        }
        assert!(kept.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn warnings(a: &EditorApp) -> usize {
        a.sh.toasts.live.iter().filter(|t| t.level == Level::Warning).count()
    }

    #[test]
    fn first_autosave_failure_pushes_one_warning_toast() {
        let mut a = EditorApp::new(crate::editor::Editor::start_project().unwrap(), None, "t".into());
        let n = a.autosave.record(Some("disk full".into()));
        assert_eq!(n, Notice::Failed);
        a.announce_autosave(n);
        let t = &a.sh.toasts.live;
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].level, Level::Warning);
        assert_eq!(t[0].command.as_deref(), Some(AUTOSAVE_SHOW));
        assert!(a.autosave_reason().is_some());
    }

    #[test]
    fn toast_action_opens_and_never_closes_the_autosave_window() {
        let mut a = EditorApp::new(crate::editor::Editor::start_project().unwrap(), None, "t".into());
        let n = a.autosave.record(Some("disk full".into()));
        a.announce_autosave(n);
        let cmd = a.sh.toasts.live[0].command.clone().unwrap();
        assert!(a.run(&cmd));
        assert!(a.autosave_open);
        assert!(a.run(&cmd), "second click on an already open window");
        assert!(a.autosave_open, "stays open");
    }

    #[test]
    fn repeated_failures_do_not_repeat_toast() {
        let mut a = EditorApp::new(crate::editor::Editor::start_project().unwrap(), None, "t".into());
        for i in 0..4 {
            let n = a.autosave.record(Some(format!("disk full {i}")));
            a.announce_autosave(n);
        }
        assert_eq!(warnings(&a), 1);
        assert_eq!(a.sh.toasts.history.len(), 1);
    }

    #[test]
    fn recovery_after_failure_clears_flag_and_says_so() {
        let mut a = EditorApp::new(crate::editor::Editor::start_project().unwrap(), None, "t".into());
        let n = a.autosave.record(Some("disk full".into()));
        a.announce_autosave(n);
        let n = a.autosave.record(None);
        assert_eq!(n, Notice::Recovered);
        a.announce_autosave(n);
        assert!(a.autosave.failed.is_none());
        assert_eq!(a.sh.toasts.history.last().map(|h| h.0), Some(Level::Info));
        // a plain success afterwards says nothing
        assert_eq!(a.autosave.record(None), Notice::None);
        // a new streak warns again
        assert_eq!(a.autosave.record(Some("x".into())), Notice::Failed);
    }

    #[test]
    fn a_read_only_window_with_no_edits_writes_no_snapshot() {
        let d = scratch("ro2");
        let proj = d.join("a.ddc");
        let mut owner = crate::editor::Editor::start_project().unwrap();
        owner.save_project_as(&proj).unwrap();
        let mut ed = crate::editor::Editor::start_project().unwrap();
        ed.open_project_read_only(&proj).unwrap();
        let mut a = EditorApp::new(ed, None, "test".into());
        a.prefs.autosave_dir = d.join("auto").display().to_string();
        assert!(a.flush_autosave().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_changed_autosave_folder_takes_the_session_marks_with_it() {
        let d = scratch("rebind");
        let (a1, a2) = (d.join("one"), d.join("two"));
        let mut a = EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        );
        a.prefs.autosave_dir = a1.display().to_string();
        begin_session(&a1);
        a.autosave.instance = ProjectLock::acquire(&a1.join(SESSION_INSTANCE)).ok();
        a.autosave.bound = Some(a1.clone());
        assert!(a.autosave.instance.is_some());
        a.prefs.autosave_dir = a2.display().to_string();
        a.rebind_session();
        assert!(
            !a1.join(SESSION_LOCK).exists(),
            "no false crash in the old folder"
        );
        assert!(a2.join(SESSION_LOCK).exists(), "a crash here will be seen");
        assert!(a.autosave.instance.is_some());
        a.end_autosave_session();
        assert!(!a2.join(SESSION_LOCK).exists());
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn time_formats() {
        let t = UNIX_EPOCH + Duration::from_secs(86_400 * 365);
        assert_eq!(utc_parts(t), (1, 1, 0, 0));
        assert_eq!(fmt_hhmm(t).len(), 5);
    }

    #[test]
    fn later_keeps_snapshots_until_a_decision() {
        let d = std::env::temp_dir().join(format!("ddc-rec-later-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join(UNTITLED);
        let m = Model::open(":memory:").unwrap();
        m.autosave_as(&file, 1).unwrap();
        let r = scan(&d, true).unwrap();
        assert_eq!(hold_snapshots(&d, &r.snaps), 0);
        assert!(
            with_held(scan(&d, false), &d, None).is_some(),
            "a clean exit does not cancel the offer"
        );
        // the next session's snapshots rotate; what was put off stays
        m.autosave_as(&file, 1).unwrap();
        m.autosave_as(&file, 1).unwrap();
        assert_eq!(held_snapshots(&d).len(), 1);
        let again = with_held(scan(&d, true), &d, None).unwrap();
        assert_eq!(again.snaps.len(), 2, "held and the newest of the rotation");
        assert_eq!(release_held(&again.snaps), 0);
        assert!(held_snapshots(&d).is_empty(), "a decision ends the offer");
        drop(m);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_drawing_only_edit_makes_the_snapshot_due() {
        let mut a = Autosave::default();
        a.mark_clean(7);
        assert!(!a.dirty(7));
        a.note_side_edit();
        assert!(a.dirty(7), "no model revision moved");
        assert!(due(a.changes(7), Duration::ZERO, 5, 1));
        a.snapshot_taken(7);
        assert!(!a.dirty(7));
        a.note_side_edit();
        a.mark_clean(8);
        assert_eq!(a.changes(8), 0, "an open or save absorbs it");
    }

    /// A save that began at rev 5 with 2 drawing edits: the third, made meanwhile, stays owed.
    #[test]
    fn a_landed_save_keeps_edits_made_while_it_ran() {
        let mut a = Autosave::default();
        a.note_side_edit();
        a.note_side_edit();
        let side = a.side_mark();
        a.note_side_edit();
        a.mark_saved(5, side);
        assert_eq!(a.changes(5), 1);
        assert_eq!(a.changes(6), 2, "a model edit after the mark too");
    }

    /// A snapshot restored for a damaged file autosaves as an unbound project: the damaged
    /// file, its `.prev` and its own snapshot family are never written.
    #[test]
    fn autosave_after_a_damaged_restore_never_touches_the_damaged_file() {
        let d = scratch("r108a");
        let auto = d.join("auto");
        let bad = d.join("bad.ddc");
        std::fs::write(&bad, b"not a database").unwrap();
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        let prev = bak.join("bad.ddc.prev");
        crate::editor::Editor::new_in_memory().unwrap().save_project_as(&prev).unwrap();
        let mut a = autosave_app(auto.clone());
        let family = auto.join(autosave_name(Some(&bad)));
        let snap = a.ed.model.autosave_as(&family, 3).unwrap();
        let state = |p: &Path| {
            let at = std::fs::metadata(p).and_then(|m| m.modified()).unwrap();
            (std::fs::read(p).unwrap(), at)
        };
        let (bad0, prev0) = (state(&bad), state(&prev));
        let family0 = ddc_model::snapshot_infos(&family).len();
        a.restore(&snap, Some(&bad), true).unwrap();
        assert!(a.ed.project.path().is_none(), "bound to no file");
        assert_eq!(a.ed.project.recovered_from(), Some(bad.as_path()));
        assert!(a.ed.is_dirty());
        let file = a.autosave_file().unwrap();
        assert_eq!(file.parent(), Some(auto.as_path()), "snapshots only");
        assert_ne!(file, family);
        assert_ne!(file, bad);
        a.autosave.note_side_edit();
        snapshot_now(&mut a);
        assert!(a.autosave.last_ok.is_some(), "the snapshot was written");
        assert!(!ddc_model::snapshot_infos(&file).is_empty());
        assert_eq!(ddc_model::snapshot_infos(&family).len(), family0, "not rotated");
        assert!(a.ed.save_project().is_err(), "a plain save has no file to write");
        assert_eq!(state(&bad), bad0, "the damaged file is untouched");
        assert_eq!(state(&prev), prev0, "the .prev is neither written nor rotated");
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A restored copy of a damaged file snapshots into a family of its own, named after that
    /// file: other untitled work never rotates it out, and a crash still offers it.
    #[test]
    fn autosave_family_of_a_recovered_copy() {
        let (x, y) = (Path::new("C:/a/house.ddc"), Path::new("C:/b/house.ddc"));
        for first in [true, false] {
            let n = recovered_name(x, first);
            assert!(n.starts_with(&format!("{UNTITLED}.r")), "{n}: a side family of untitled");
            assert!(n.contains("house"), "{n}: named after the damaged file");
            assert_ne!(n, snapshot_name(None, false, first));
            assert_ne!(n, recovered_name(y, first), "two damaged files, two families");
        }
        let d = scratch("r111b");
        let auto = d.join("auto");
        let bad = d.join("bad.ddc");
        std::fs::write(&bad, b"not a database").unwrap();
        let mut a = autosave_app(auto.clone());
        let snap = a.ed.model.autosave_as(&auto.join(autosave_name(Some(&bad))), 3).unwrap();
        a.restore(&snap, Some(&bad), true).unwrap();
        let file = a.autosave_file().unwrap();
        let first = a.autosave.instance.is_some();
        assert_eq!(file, auto.join(recovered_name(&bad, first)));
        let untitled = auto.join(snapshot_name(None, false, first));
        let other = a.ed.model.autosave_as(&untitled, 1).unwrap();
        a.autosave.note_side_edit();
        snapshot_now(&mut a);
        let mine = ddc_model::snapshot_infos(&file);
        assert!(!mine.is_empty(), "the copy wrote into its own family");
        assert!(other.exists(), "untitled work is not rotated out");
        let offered = scan(&auto, true).expect("a crash offers the copy");
        assert!(offered.snaps.iter().any(|s| s.path == mine[0].path));
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn autosave_app(folder: PathBuf) -> EditorApp {
        let ed = crate::editor::Editor::start_project().unwrap();
        let mut a = EditorApp::new(ed, None, "t".into());
        a.prefs.autosave = true;
        a.prefs.autosave_dir = folder.display().to_string();
        a
    }

    /// Runs one scheduled snapshot to its end, on the worker or inline.
    fn snapshot_now(a: &mut EditorApp) {
        let ctx = egui::Context::default();
        a.write_autosave();
        for _ in 0..2000 {
            if a.autosave.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            a.autosave_tick(&ctx);
        }
        panic!("snapshot did not finish");
    }

    #[test]
    fn drawings_not_written_is_no_saved_at_and_stays_due() {
        let d = scratch("sv16side");
        let mut a = autosave_app(d.join("auto"));
        a.autosave.note_side_edit();
        let rev = a.ed.revision;
        crate::drawing::store::FAIL_WRITE.with(|f| f.set(true));
        snapshot_now(&mut a);
        snapshot_now(&mut a);
        crate::drawing::store::FAIL_WRITE.with(|f| f.set(false));
        assert!(a.autosave.dirty(rev), "the drawing edit is still owed");
        assert!(a.autosave.failed.is_some());
        assert!(a.autosave.last_ok.is_none(), "never 'Saved at'");
        assert!(a.autosave_label().1, "the indicator shows the failure");
        assert_eq!(warnings(&a), 1, "one toast for the streak");
        snapshot_now(&mut a);
        assert!(!a.autosave.dirty(rev));
        assert!(a.autosave.failed.is_none() && a.autosave.last_ok.is_some());
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The first window of a session over `folder`, with snapshots due after one edit.
    fn session_app(folder: &Path) -> EditorApp {
        let mut a = autosave_app(folder.to_path_buf());
        a.prefs.autosave_edits = 1;
        begin_session(folder);
        a.autosave.instance = ProjectLock::acquire(&folder.join(SESSION_INSTANCE)).ok();
        a.autosave.bound = Some(folder.to_path_buf());
        assert!(a.autosave.instance.is_some(), "the first window");
        a
    }

    /// Autosave frames until nothing is scheduled or running any more.
    fn settle_ticks(a: &mut EditorApp) {
        let ctx = egui::Context::default();
        for _ in 0..2000 {
            a.autosave_tick(&ctx);
            if !a.autosave.pending && a.autosave.job.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("autosave did not settle");
    }

    fn snapshot_files(folder: &Path) -> usize {
        std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".snap-"))
            .count()
    }

    /// An untouched untitled window: the load revision, a reload and a level walk are no edits,
    /// so no snapshot is written, the close asks nothing and leaves the folder without one.
    #[test]
    fn an_untouched_untitled_session_closes_without_a_snapshot() {
        let d = scratch("s17clean");
        let auto = d.join("auto");
        let mut a = session_app(&auto);
        a.ed.reload().unwrap();
        let rev = a.ed.revision;
        a.ed.fresh_revision();
        a.ed.restore_revision(rev);
        a.ed.fresh_revision();
        assert!(!a.ed.is_dirty(), "nothing was edited");
        for _ in 0..3 {
            settle_ticks(&mut a);
        }
        assert!(a.autosave.last_ok.is_none(), "no snapshot was taken");
        assert!(!crate::project_ui::close_requested(&mut a), "the close asks nothing");
        a.end_autosave_session();
        assert_eq!(snapshot_files(&auto), 0, "a clean close leaves no snapshot");
        assert!(!auto.join(SESSION_LOCK).exists());
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A real edit in the same window is still snapshotted, kept after the close and asked about.
    #[test]
    fn an_edited_untitled_session_keeps_its_snapshot_and_asks() {
        let d = scratch("s17edit");
        let auto = d.join("auto");
        let mut a = session_app(&auto);
        // entering a drawing tool writes nothing; a new level is a model edit
        let op = crate::editor::OpFn(std::sync::Arc::new(|m: &mut Model| {
            m.add_level("edit", 3.0).map(|_| ())
        }));
        a.ed.run_op(op).unwrap();
        assert!(a.ed.is_dirty());
        settle_ticks(&mut a);
        settle_ticks(&mut a);
        assert!(a.autosave.last_ok.is_some(), "the edit was snapshotted");
        assert!(!ddc_model::snapshot_infos(&auto.join(UNTITLED)).is_empty());
        assert!(crate::project_ui::close_requested(&mut a), "the close dialog decides");
        a.end_autosave_session();
        assert!(snapshot_files(&auto) > 0, "the edit stays protected");
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The archive toast after the last Later reads whole on one line of the toast card.
    #[test]
    fn later_archived_toast_fits_one_line() {
        use ddc_ui::theme::{control, icon_size, layout, space, Role};
        let ctx = crate::layout_contract::font_ctx();
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let room = layout::TOAST_W - icon_size::SM - control::HIT_MIN - space::LG;
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for key in ["recover.later_archived"] {
                let t = later_text(&c, key);
                assert!(t != key && !t.contains('{'), "{lang:?} {key}: {t}");
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    let _ = th.apply(ctx);
                    let w = ctx.fonts(|f| {
                        f.layout_no_wrap(t.clone(), th.font(Role::Body), egui::Color32::WHITE)
                            .size()
                            .x
                    });
                    assert!(w <= room, "{lang:?} {key}: {t} is {w} > {room}");
                });
            }
        }
    }

    #[test]
    fn a_failed_snapshot_keeps_every_edit_owed() {
        let d = scratch("sv16model");
        let blocker = d.join("not-a-folder");
        std::fs::write(&blocker, b"x").unwrap();
        let mut a = autosave_app(blocker.join("auto"));
        for _ in 0..3 {
            a.autosave.note_side_edit();
        }
        let rev = a.ed.revision;
        let before = a.autosave.changes(rev);
        snapshot_now(&mut a);
        assert_eq!(a.autosave.changes(rev), before, "not one edit counted as saved");
        assert!(a.autosave.failed.is_some());
        assert!(a.autosave.last_ok.is_none());
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_snapshot_starts_while_a_button_is_held() {
        let d = scratch("ext2gesture");
        let mut a = autosave_app(d.join("auto"));
        a.prefs.autosave_edits = 1;
        // a drawing edit as `drawing::store::frame` reports it: counted and unsaved
        a.autosave.note_side_edit();
        a.ed.set_side_dirty(true);
        let ctx = egui::Context::default();
        let pos = egui::pos2(10.0, 10.0);
        let btn = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let frame = |events: Vec<egui::Event>, a: &mut EditorApp| {
            let raw = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(raw, |ctx| a.autosave_tick(ctx));
        };
        frame(vec![egui::Event::PointerMoved(pos), btn(true)], &mut a);
        frame(vec![], &mut a);
        assert!(!a.autosave.pending, "nothing scheduled mid-gesture");
        assert!(a.autosave.job.is_none(), "nothing written mid-gesture");
        frame(vec![btn(false)], &mut a);
        assert!(a.autosave.pending, "due again after the release");
        assert!(a.autosave.held_since.is_none(), "the release ends the hold");
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stuck_button_defers_snapshots_only_up_to_the_cap() {
        let d = scratch("r117gesturecap");
        let mut a = autosave_app(d.join("auto"));
        a.prefs.autosave_edits = 1;
        a.autosave.note_side_edit();
        a.ed.set_side_dirty(true);
        let ctx = egui::Context::default();
        let pos = egui::pos2(10.0, 10.0);
        let press = egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let raw = |events| egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw(vec![egui::Event::PointerMoved(pos), press]), |c| {
            a.autosave_tick(c)
        });
        assert!(!a.autosave.pending, "a fresh hold defers the snapshot");
        let long_ago = Instant::now().checked_sub(GESTURE_CAP + Duration::from_secs(1));
        a.autosave.held_since = long_ago;
        let _ = ctx.run(raw(vec![]), |c| a.autosave_tick(c));
        assert!(a.autosave.pending, "past the cap the snapshot is due while still held");
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_crash_copy_waits_for_the_background_one() {
        let d = scratch("ext2crash");
        let mut a = autosave_app(d.join("auto"));
        a.autosave.note_side_edit();
        a.write_autosave();
        assert!(a.autosave.job.is_some(), "a background copy runs");
        a.crash_flush();
        assert!(a.autosave.job.is_none(), "it ended before the final copy");
        assert!(a.autosave.failed.is_none() && a.autosave.last_ok.is_some());
        assert!(!a.autosave.pending);
        drop(a);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn crash_notes_are_clipped_and_rotated_to_the_archive() {
        let d = scratch("ext2notes");
        // two-byte characters after one byte: the limit falls inside a character
        let big = format!("x{}", "\u{e9}".repeat(CRASH_NOTE_MAX));
        let extra = 2;
        for i in 0..(CRASH_KEEP as u128 + extra) {
            write_crash_note(&d, 1_700_000_000_000 + i, big.clone());
        }
        let kept = crash_notes(&d);
        assert_eq!(kept.len(), CRASH_KEEP);
        let total: u64 = kept.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum();
        assert!(total <= 1024 * 1024, "notes beside the snapshots stay within 1 MiB");
        assert!(std::fs::read_to_string(&kept[0]).is_ok(), "cut at a character boundary");
        let archived = crash_notes(&d.join("_archive"));
        assert_eq!(archived.len() as u128, extra, "older notes moved, never deleted");
        assert_eq!(last_crash_note(&d), kept.last().cloned());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn held_snap(d: &Path, project: Option<&Path>) -> Vec<SnapInfo> {
        let file = d.join(autosave_name(project));
        Model::open(":memory:").unwrap().autosave_as(&file, 1).unwrap();
        let r = scan_for(d, true, project).unwrap();
        assert_eq!(hold_snapshots(d, &r.snaps), 0);
        r.snaps
    }

    /// The recovery offer is only for this project: held snapshots of another project or of
    /// the untitled model never join it.
    #[test]
    fn held_snapshots_stay_with_their_project() {
        let d = scratch("held-own");
        let (a, b) = (d.join("a.ddc"), d.join("b.ddc"));
        held_snap(&d, Some(&a));
        assert!(with_held(None, &d, Some(&b)).is_none(), "another project");
        assert!(with_held(None, &d, None).is_none(), "untitled");
        let r = with_held(None, &d, Some(&a)).unwrap();
        assert_eq!(r.project.as_deref(), Some(a.as_path()));
        assert_eq!(r.snaps.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn held_snapshots_older_than_the_save_are_not_offered() {
        let d = scratch("held-stale");
        let a = d.join("a.ddc");
        held_snap(&d, Some(&a));
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&a, b"saved later").unwrap();
        assert!(with_held(None, &d, Some(&a)).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn untitled_crash_is_offered_once() {
        let d = scratch("untitled-once");
        let file = d.join(UNTITLED);
        Model::open(":memory:").unwrap().autosave_as(&file, 1).unwrap();
        mark_undecided(&d, None).unwrap();
        let first = pending_recoveries(&d);
        assert_eq!(first.len(), 1);
        assert!(first[0].project.is_none());
        settle_recovery(&d, None);
        assert!(pending_recoveries(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn command_line_file_gets_only_its_own_offer() {
        let d = scratch("cli");
        let (a, b) = (d.join("a.ddc"), d.join("b.ddc"));
        let rec = |p: Option<&Path>| Recover {
            project: p.map(Path::to_path_buf),
            ..Default::default()
        };
        let list = vec![rec(None), rec(Some(&a))];
        assert!(offer_for_arg(list, &b).is_none());
        let list = vec![rec(None), rec(Some(&a))];
        assert!(offer_for_arg(list, &a).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn discard_and_repeated_later_move_snapshots_to_the_archive() {
        let d = scratch("archive");
        let a = d.join("a.ddc");
        mark_undecided(&d, Some(&a)).unwrap();
        let snaps = held_snap(&d, Some(&a));
        assert_eq!(bump_later(&d, Some(&a)), 1);
        assert_eq!(bump_later(&d, Some(&a)), 2);
        assert_eq!(bump_later(&d, Some(&a)), LATER_MAX);
        settle_recovery(&d, Some(&a));
        assert!(held_snapshots(&d).is_empty());
        assert!(!snaps[0].path.exists());
        let name = snaps[0].path.file_name().unwrap();
        assert!(d.join(HELD_DIR).join("_archive").join(name).exists());
        assert!(undecided(&d).is_empty());
        assert_eq!(bump_later(&d, Some(&a)), 1, "the count restarts");
        let _ = std::fs::remove_dir_all(&d);
    }
}
