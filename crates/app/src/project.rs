// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The project as a real file: which file the editor is on, its lock, the private working
//! copy all edits go to, the modified flag, thumbnails and the recent list. Windows and
//! background jobs for these are in `project_ui`.
//!
//! For the start screen: [`recent_projects`], [`Editor::new_project`], [`Editor::open_project`].
//! Those two replace the model at once with no prompt; with an `EditorApp` at hand prefer
//! `project_ui::request_new` / `project_ui::request_open` (unsaved prompt, big files off the UI
//! thread, plain messages).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use web_time::{SystemTime, UNIX_EPOCH};

use ddc_model::{
    open_project_copy, probe_project, remove_db_files, save_project_file, Error, LockError, Model,
    OpenedCopy, ProjectError, ProjectLock, PROJECT_EXT,
};

use crate::editor::Editor;
use crate::prefs::Prefs;
use crate::wall_tool::Mode;

/// Tables holding only what can be rebuilt: writing them does not make the project modified.
const DERIVED: [&str; 21] = [
    "mesh_proto",
    "mesh_geo",
    "comp_thumb",
    "mass_cache",
    "join_cache",
    "dim_geo",
    "measure_geo",
    "ceiling_qty",
    "comp_qty",
    "mass_qty",
    "member_qty",
    "railing_qty",
    "roof_qty",
    "room_qty",
    "slab_qty",
    "solid_qty",
    "stair_qty",
    "struct_qty",
    "wall_qty",
    "struct_issue",
    "src_target",
];

/// Where the model's content stands, rebuildable tables left out as for "modified"; None when
/// unknown, which a caller must take as changed.
pub(crate) fn content_mark(m: &Model) -> Option<ddc_model::ContentMark> {
    m.content_mark(&DERIVED).ok()
}

pub const THUMB_SIZE: (u32, u32) = (320, 200);

/// Application data folder; unit tests stay out of the user's real one.
pub(crate) fn app_dir() -> Option<PathBuf> {
    if cfg!(test) {
        Some(std::env::temp_dir().join(format!("ddc-f1-app-{}", std::process::id())))
    } else {
        crate::gfx::data_dir()
    }
}

fn work_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DDC_WORK_DIR") {
        return PathBuf::from(d);
    }
    app_dir().unwrap_or_else(ddc_model::temp_dir).join("work")
}

fn beside(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Written beside a working file that a background copy kept from being removed: nothing to save.
fn discard_mark(work: &Path) -> PathBuf {
    beside(work, ".discard")
}

/// A working file that holds an unfinished build: a crash must not offer it as lost work.
pub(crate) fn set_nothing_to_save(work: &Path, on: bool) {
    let mark = discard_mark(work);
    if on {
        // without it a crash offers the unfinished build as work: too much, never too little
        if let Err(e) = std::fs::write(&mark, b"") {
            eprintln!("work file: mark {} not written: {e}", mark.display());
        }
        return;
    }
    // a mark left behind would hide real edits from the next start: it must go
    let gone = |r: std::io::Result<()>| match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if let Err(e) = gone(std::fs::remove_file(&mark)).or_else(|_| gone(std::fs::remove_file(&mark)))
    {
        let stale = beside(work, ".discard.stale");
        if let Err(e2) = std::fs::rename(&mark, &stale) {
            eprintln!("work file: mark {} stays: {e}; {e2}", mark.display());
        }
    }
}

/// Which project a working file belongs to, for the start after a crash: one note per working
/// file, so every window has its own. The file names only a project this window may save over.
pub fn write_note(work: &Path, project: Option<&Path>) {
    let (note, tmp) = (beside(work, ".note"), beside(work, ".note.tmp"));
    let text = project.map_or_else(String::new, |p| p.display().to_string());
    // whole or not at all: half a path would offer the snapshot under a wrong name
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &note);
    }
}

/// The project of a working file; no note or an empty one: untitled.
pub fn read_note(work: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(beside(work, ".note")).ok()?;
    let line = text.lines().next()?;
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// Private working copy of the open project, removed with this value unless a panic is
/// unwinding. A lock beside it tells a later start whether its owner is still alive.
#[derive(Debug)]
pub struct WorkFile {
    path: PathBuf,
    _lock: Option<ProjectLock>,
}

impl WorkFile {
    pub fn create() -> std::io::Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = work_dir();
        std::fs::create_dir_all(&dir)?;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let path = dir.join(format!(
            "{}-{}-{ms}.{PROJECT_EXT}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        // without the lock a later start could take this live file for a dead one
        let lock = ProjectLock::acquire(&path).map_err(|e| match e {
            LockError::Io(e) => e,
            LockError::Busy => std::io::Error::other("working file is locked"),
        })?;
        Ok(Self {
            path,
            _lock: Some(lock),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkFile {
    fn drop(&mut self) {
        // a panic: this file is the newest copy of the edits and the next start picks it up
        if std::thread::panicking() {
            return;
        }
        retire(&self.path, &remove_db_files, &|m: &Path| std::fs::write(m, b""));
    }
}

/// A finished working file goes. Kept by a background copy, it gets the mark that tells the
/// next start it holds nothing to save; when the mark cannot be written the file is removed
/// again or moved to `_orphans`, so it is never offered as lost work.
fn retire(path: &Path, remove: &dyn Fn(&Path), mark: &dyn Fn(&Path) -> std::io::Result<()>) {
    remove(path);
    if path.exists() {
        if let Err(e) = mark(&discard_mark(path)) {
            eprintln!("work file: no discard mark for {}: {e}", path.display());
            remove(path);
            if path.exists() {
                let work = path.parent().unwrap_or(Path::new("."));
                match move_aside(path, work) {
                    Ok(to) => eprintln!("work file: moved to {}", to.display()),
                    Err(e) => eprintln!("work file: {} stays: {e}", path.display()),
                }
            }
        }
    }
    let _ = std::fs::remove_file(beside(path, ".note"));
}

/// What the sweep of dead working files could not turn into a checked snapshot.
#[derive(Debug, Default)]
pub struct Swept {
    /// Working copies kept for the user, moved to `work/_orphans` where the move worked.
    pub kept: Vec<PathBuf>,
    /// Files a failed rollback left in `_orphans` while their main file stayed in `work`.
    pub stranded: Vec<PathBuf>,
}

/// Working files whose owner died (their lock is free, whichever window it was): each becomes a
/// checked snapshot in `autosave`, offered at start until the user decides. A file is removed
/// only after that; otherwise it is moved aside and reported, never lost.
pub fn sweep_work(autosave: &Path) -> Swept {
    sweep_dir(&work_dir(), autosave)
}

pub(crate) fn sweep_dir(work: &Path, autosave: &Path) -> Swept {
    let mut out = Swept::default();
    for e in std::fs::read_dir(work).into_iter().flatten().flatten() {
        let p = e.path();
        if !is_project_file(&p) || !p.is_file() {
            continue;
        }
        let Ok(lock) = ProjectLock::acquire(&p) else {
            continue;
        };
        // a sidecar already moved away from this main: its pages are missing here, so a
        // snapshot would pass the check and the deletion below would lose them
        let half = half_moved(&p, work);
        if !half.is_empty() {
            out.kept.push(p.clone());
            out.stranded.extend(half);
            drop(lock);
            continue;
        }
        let nothing_to_save = discard_mark(&p).exists();
        if nothing_to_save || snapshot_orphan(&p, autosave).is_ok() {
            remove_db_files(&p);
            if !p.exists() {
                let _ = std::fs::remove_file(discard_mark(&p));
                let _ = std::fs::remove_file(beside(&p, ".note"));
            }
        } else {
            match move_aside(&p, work) {
                Ok(to) => out.kept.push(to),
                Err(e) => {
                    out.kept.push(p.clone());
                    out.stranded.extend(e.stranded.into_iter().map(|(_, at)| at));
                }
            }
        }
        drop(lock);
    }
    out
}

/// A snapshot that passed the check and a promise to offer it: only then may `work` go.
fn snapshot_orphan(work: &Path, autosave: &Path) -> Result<(), Error> {
    use crate::shell::recover_win::{mark_undecided, recovery_name};
    let project = read_note(work);
    let id = work
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let virt = autosave.join(recovery_name(project.as_deref(), &id));
    let snap = ddc_model::autosave_file(work, &virt, 1)?;
    ddc_model::check_snapshot(&snap)?;
    if matches!(probe_project(&snap), Err(ProjectError::NotProject(_))) {
        // a crash in the middle of an Open: no model ever reached this file
        let _ = std::fs::remove_file(&snap);
        return Ok(());
    }
    mark_undecided(autosave, project.as_deref())?;
    Ok(())
}

/// Files beside a working copy that belong to it; committed pages may still sit in the WAL.
const SIDECARS: [&str; 4] = ["-wal", "-shm", "-journal", ".note"];

/// A move that failed. `stranded` lists (original, current) paths of files whose rollback
/// failed too: they still live at the second path, not beside the main file.
#[derive(Debug)]
pub(crate) struct MoveAsideError {
    pub cause: std::io::Error,
    pub stranded: Vec<(PathBuf, PathBuf)>,
}

impl std::fmt::Display for MoveAsideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cause)?;
        for (from, at) in &self.stranded {
            write!(f, "; {} stays at {}", from.display(), at.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for MoveAsideError {}

impl From<std::io::Error> for MoveAsideError {
    fn from(cause: std::io::Error) -> Self {
        Self { cause, stranded: Vec::new() }
    }
}

/// Sidecars of `p` found in `work/_orphans` under a name planned for it while the main file
/// is not there: the leftovers of a move whose rollback failed. One pass over the folder, not
/// a probe up to the first free number: a removed earlier copy leaves a gap in the numbering.
fn half_moved(p: &Path, work: &Path) -> Vec<PathBuf> {
    let dir = work.join("_orphans");
    let Some(name) = p.file_name().map(Path::new) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let f = e.file_name();
            let Some(f) = f.to_str() else {
                return false;
            };
            SIDECARS.iter().any(|s| {
                f.strip_suffix(s).is_some_and(|base| {
                    is_orphan_name(base, name) && dir.join(base).symlink_metadata().is_err()
                })
            })
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// `base` is `name` itself or `stem (N).ext`, a name `free_orphan_name` may give it.
fn is_orphan_name(base: &str, name: &Path) -> bool {
    let Some(full) = name.to_str() else {
        return false;
    };
    if base == full {
        return true;
    }
    let stem = name.file_stem().and_then(|s| s.to_str()).unwrap_or(full);
    let ext = name.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}"));
    let rest = match ext.as_deref() {
        Some(e) => base.strip_suffix(e),
        None => Some(base),
    };
    rest.and_then(|r| r.strip_prefix(stem))
        .and_then(|r| r.strip_prefix(" ("))
        .and_then(|r| r.strip_suffix(')'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn move_aside(p: &Path, work: &Path) -> Result<PathBuf, MoveAsideError> {
    move_aside_with(p, work, &|a: &Path, b: &Path| std::fs::rename(a, b))
}

/// Sidecars first, the main file last, under a name nothing in `_orphans` uses yet (a rename
/// would replace it). Any failure moves back what was moved and nothing is ever removed.
fn move_aside_with(
    p: &Path,
    work: &Path,
    rename: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<PathBuf, MoveAsideError> {
    let dir = work.join("_orphans");
    std::fs::create_dir_all(&dir)?;
    let name = Path::new(p.file_name().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?);
    let to = free_orphan_name(&dir, name)?;
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let pairs = SIDECARS
        .iter()
        .map(|s| (beside(p, s), beside(&to, s)))
        .filter(|(from, _)| from.symlink_metadata().is_ok())
        .chain(std::iter::once((p.to_path_buf(), to.clone())));
    for (from, dest) in pairs {
        if let Err(e) = rename(from.as_path(), dest.as_path()) {
            let mut stranded = Vec::new();
            for (back_to, back_from) in moved.iter().rev() {
                if rename(back_from.as_path(), back_to.as_path()).is_err() {
                    stranded.push((back_to.clone(), back_from.clone()));
                }
            }
            return Err(MoveAsideError { cause: e, stranded });
        }
        moved.push((from, dest));
    }
    Ok(to)
}

fn orphan_candidate(dir: &Path, name: &Path, n: u32) -> PathBuf {
    if n == 1 {
        return dir.join(name);
    }
    let stem = name.file_stem().unwrap_or(name.as_os_str());
    let mut s = stem.to_os_string();
    s.push(format!(" ({n})"));
    if let Some(e) = name.extension() {
        s.push(".");
        s.push(e);
    }
    dir.join(s)
}

/// `name`, else `stem (2).ext`, `stem (3).ext`, ... : the first with no file or sidecar in `dir`.
fn free_orphan_name(dir: &Path, name: &Path) -> std::io::Result<PathBuf> {
    for n in 1..10_000u32 {
        let cand = orphan_candidate(dir, name, n);
        let taken = cand.symlink_metadata().is_ok()
            || SIDECARS.iter().any(|s| beside(&cand, s).symlink_metadata().is_ok());
        if !taken {
            return Ok(cand);
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

pub fn is_project_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(PROJECT_EXT))
}

/// `path` with the project extension added when it has another or none.
pub fn with_project_ext(path: PathBuf) -> PathBuf {
    if is_project_file(&path) {
        return path;
    }
    let mut s = path.into_os_string();
    s.push(".");
    s.push(PROJECT_EXT);
    PathBuf::from(s)
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Stable name of a path for files of the application (FNV-1a; the std hasher may change).
pub fn path_key(p: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in p.to_string_lossy().to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Fingerprint of the saved selection filters: they live in `meta`, which the change journal
/// leaves out, so a new filter would otherwise never count as an unsaved edit.
/// No filters is 0, the `Default` of a fresh project.
fn filters_mark(m: &Model) -> u64 {
    use std::hash::{Hash, Hasher};
    match m.saved_filters().ok().flatten() {
        Some(v) if !v.is_empty() => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            v.hash(&mut h);
            h.finish().max(1)
        }
        _ => 0,
    }
}

/// Numbers the models of this process: a job started on one must not bind a file to another.
static NEXT_MODEL: AtomicU64 = AtomicU64::new(1);

/// What a save or an open remembers when it begins: which model, and how far along it was.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SaveMark {
    model: u64,
    head: u64,
    phase: Option<i64>,
    state: Option<ddc_model::EditState>,
    filters: u64,
    /// Model revision and autosave drawing count at the start: what the file will hold.
    pub(crate) rev: u64,
    pub(crate) side: u64,
}

/// Which file the editor is on. Default: an untitled project with no working file (in memory).
#[derive(Default)]
pub struct ProjectState {
    model: u64,
    /// Current phase in the file: a setting outside the journal's idea of an edit.
    saved_phase: Option<i64>,
    /// Saved selection filters in the file ([`filters_mark`]): `meta` rows, not journal content.
    saved_filters: u64,
    path: Option<PathBuf>,
    read_only: bool,
    lock: Option<ProjectLock>,
    work: Option<WorkFile>,
    saved_head: u64,
    /// Model state the file holds: undo back to it is not a change.
    saved_state: Option<ddc_model::EditState>,
    /// Placed drawings differ from the file (mirrored every frame by `drawing::store`).
    side_dirty: bool,
    /// Restored from a snapshot: differs from the file whatever the journal says.
    forced_dirty: bool,
    /// Restored for this damaged file while bound to no file: Save asks for a new name and
    /// neither the damaged file nor its kept copies are ever written.
    recovered_from: Option<PathBuf>,
    /// Last answer of the modified check: (journal head, modified).
    seen: Cell<(u64, bool)>,
}

impl ProjectState {
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The damaged file this unbound copy was restored for, until a Save As binds it.
    pub fn recovered_from(&self) -> Option<&Path> {
        self.recovered_from.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn set_read_only_for_test(&mut self) {
        self.read_only = true;
    }

    pub fn work_path(&self) -> Option<&Path> {
        self.work.as_ref().map(WorkFile::path)
    }

    /// Device-loss handoff: the new process takes the file over, this one writes no more.
    pub(crate) fn release_lock(&mut self) {
        self.lock = None;
    }

    /// Model replaced by something that is not this file: Save must never write there.
    pub(crate) fn detach(&mut self, m: &Model) {
        *self = Self {
            model: NEXT_MODEL.fetch_add(1, Ordering::Relaxed),
            ..Self::default()
        };
        self.mark_clean(m);
    }

    pub(crate) fn mark_clean(&mut self, m: &Model) {
        self.saved_head = m.change_head();
        self.saved_state = m.edit_state(&DERIVED).ok();
        self.saved_phase = m.current_phase().ok();
        self.saved_filters = filters_mark(m);
        self.forced_dirty = false;
        self.side_dirty = false;
        self.seen.set((0, false));
    }

    /// Lock for `path`, unless this project already holds it (`None`).
    pub(crate) fn claim(&self, path: &Path) -> Result<Option<ProjectLock>, LockError> {
        if self.lock.is_some() && self.path.as_deref().is_some_and(|p| same_path(p, path)) {
            return Ok(None);
        }
        crate::gfx_guard::acquire_lock(path).map(Some)
    }
}

/// A finished background open, ready for [`Editor::adopt`].
pub struct Opened {
    pub copy: OpenedCopy,
    pub work: WorkFile,
    pub path: PathBuf,
    pub lock: Option<ProjectLock>,
    pub read_only: bool,
    /// A kept copy of this damaged file: adopted unbound, like a restored snapshot.
    pub recovered_from: Option<PathBuf>,
}

/// One row of the recent list.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentProject {
    pub path: PathBuf,
    /// File name without the extension.
    pub name: String,
    /// `None`: the file is gone or its drive is not connected; show it disabled.
    pub modified: Option<SystemTime>,
    pub thumbnail: Option<PathBuf>,
}

/// Recent projects, newest first, read from the preferences file. It touches the disk once per
/// file: call it when the start screen opens, not every frame.
pub fn recent_projects() -> Vec<RecentProject> {
    let text = app_dir()
        .and_then(|d| std::fs::read_to_string(d.join("prefs.txt")).ok())
        .unwrap_or_default();
    recent_from(&Prefs::parse(&text).recent_projects)
}

/// The list without asking the disk about any file: no `modified`, the thumbnail path is not
/// checked. What is on disk comes from `recent_probe` on a worker thread.
pub fn recent_entries(list: &[String]) -> Vec<RecentProject> {
    list.iter()
        .map(|s| {
            let path = PathBuf::from(s);
            RecentProject {
                name: path
                    .file_stem()
                    .map_or_else(|| s.clone(), |n| n.to_string_lossy().into_owned()),
                modified: None,
                thumbnail: thumb_path(&path),
                path,
            }
        })
        .collect()
}

pub fn recent_from(list: &[String]) -> Vec<RecentProject> {
    list.iter()
        .map(|s| {
            let path = PathBuf::from(s);
            RecentProject {
                name: path
                    .file_stem()
                    .map_or_else(|| s.clone(), |n| n.to_string_lossy().into_owned()),
                modified: std::fs::metadata(&path).and_then(|m| ddc_model::wtime::modified(&m)).ok(),
                thumbnail: thumb_path(&path).filter(|t| t.exists()),
                path,
            }
        })
        .collect()
}

pub fn thumb_path(project: &Path) -> Option<PathBuf> {
    Some(
        app_dir()?
            .join("thumbs")
            .join(format!("{}.png", path_key(project))),
    )
}

/// Plan lines for the thumbnail, taken on the UI thread: walls, slab outlines, the cached boxes
/// of imported meshes (so a mesh-only model has a picture) and body outlines.
pub fn thumb_lines(ed: &Editor) -> Vec<[f64; 4]> {
    let mut v = crate::open::preview::db_lines(ed.model.sql(), false);
    for b in &ed.bodies {
        let n = b.hull.len();
        for i in 0..n {
            let (p, q) = (b.hull[i], b.hull[(i + 1) % n]);
            v.push([p.x, p.y, q.x, q.y]);
        }
    }
    v
}

/// RGBA picture of the lines fitted into `w`×`h` on a transparent background, so it reads in
/// both themes; `None` when there is nothing to draw.
pub fn render_thumb(lines: &[[f64; 4]], (w, h): (u32, u32)) -> Option<Vec<u8>> {
    const INK: [u8; 4] = [0x6B, 0x72, 0x80, 0xFF];
    const MARGIN: f64 = 12.0;
    let good: Vec<&[f64; 4]> = lines
        .iter()
        .filter(|l| l.iter().all(|x| x.is_finite()))
        .collect();
    if good.is_empty() || w < 32 || h < 32 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for l in &good {
        x0 = x0.min(l[0]).min(l[2]);
        x1 = x1.max(l[0]).max(l[2]);
        y0 = y0.min(l[1]).min(l[3]);
        y1 = y1.max(l[1]).max(l[3]);
    }
    let (fw, fh) = (f64::from(w), f64::from(h));
    let (mw, mh) = ((x1 - x0).max(1e-6), (y1 - y0).max(1e-6));
    let k = ((fw - 2.0 * MARGIN) / mw).min((fh - 2.0 * MARGIN) / mh);
    let (ox, oy) = ((fw - k * mw) / 2.0, (fh - k * mh) / 2.0);
    let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut plot = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h) {
            let at = (y as usize * w as usize + x as usize) * 4;
            px[at..at + 4].copy_from_slice(&INK);
        }
    };
    for l in good {
        let map = |x: f64, y: f64| {
            (
                (ox + (x - x0) * k).round() as i64,
                // plan Y goes up, image Y goes down
                (fh - oy - (y - y0) * k).round() as i64,
            )
        };
        let ((mut a, mut b), (c, d)) = (map(l[0], l[1]), map(l[2], l[3]));
        let (dx, dy) = ((c - a).abs(), -(d - b).abs());
        let (sx, sy) = ((c - a).signum(), (d - b).signum());
        let mut err = dx + dy;
        loop {
            plot(a, b);
            if a == c && b == d {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                a += sx;
            }
            if e2 <= dx {
                err += dx;
                b += sy;
            }
        }
    }
    Some(px)
}

/// Writes the thumbnail of a saved project; failures only cost the picture.
pub fn save_thumb(project: &Path, lines: &[[f64; 4]], logo: Option<&ddc_io::logo::Logo>) {
    let Some(path) = thumb_path(project) else {
        return;
    };
    if let Some(mut px) = render_thumb(lines, THUMB_SIZE) {
        if let Some(l) = logo {
            crate::logo_ui::stamp_thumb(&mut px, THUMB_SIZE, l);
        }
        let _ = crate::shot::save_png(&path, THUMB_SIZE, &px);
    }
}

fn no_progress(_: u64, _: u64) -> Result<(), Error> {
    Ok(())
}

impl Editor {
    /// Start of the application: a new project on a private working file; in memory when there
    /// is no place to write one.
    pub fn start_project() -> Result<Self, Error> {
        let Some((m, work)) = new_working() else {
            return Self::new_in_memory();
        };
        let mut e = Self::seeded(m)?;
        crate::logo_ui::apply_default(&mut e.model, app_dir().as_deref());
        e.project.work = Some(work);
        e.project.mark_clean(&e.model);
        Ok(e)
    }

    /// Replaces the model with a new untitled project. No prompt: see the module note.
    pub fn new_project(&mut self) -> Result<(), Error> {
        let (m, work) = match new_working() {
            Some((m, w)) => (m, Some(w)),
            None => (Model::open(":memory:")?, None),
        };
        let mut fresh = Self::seeded(m)?;
        crate::logo_ui::apply_default(&mut fresh.model, app_dir().as_deref());
        self.set_mode(Mode::Select);
        self.swap_model(fresh.model, Some(fresh.level))?;
        self.current_type = fresh.current_type;
        self.project.work = work;
        self.project.mark_clean(&self.model);
        Ok(())
    }

    /// Opens a project file: lock, working copy, upgrade if older; undo history starts empty.
    /// A file locked by another instance gives [`ProjectError::Locked`]: offer
    /// [`Self::open_project_read_only`].
    pub fn open_project(&mut self, path: &Path) -> Result<(), ProjectError> {
        let (lock, read_only) = match self.project.claim(path) {
            Ok(l) => (l, false),
            Err(LockError::Busy) => return Err(ProjectError::Locked(path.to_path_buf())),
            // the folder takes no lock file, so nothing can be saved there either
            Err(LockError::Io(_)) => (None, true),
        };
        self.open_with(path, lock, read_only)
    }

    /// Opens a project without a lock; Save then asks for a new name.
    pub fn open_project_read_only(&mut self, path: &Path) -> Result<(), ProjectError> {
        self.open_with(path, None, true)
    }

    fn open_with(
        &mut self,
        path: &Path,
        lock: Option<ProjectLock>,
        read_only: bool,
    ) -> Result<(), ProjectError> {
        let work = WorkFile::create()?;
        let copy = open_project_copy(path, work.path(), !read_only, &mut no_progress)?;
        self.adopt(Opened {
            copy,
            work,
            path: path.to_path_buf(),
            lock,
            read_only,
            recovered_from: None,
        })?;
        Ok(())
    }

    /// Takes a project opened off the UI thread.
    pub(crate) fn adopt(&mut self, o: Opened) -> Result<(), Error> {
        // reopening the same file: the lock this project holds stays
        let kept = if o.lock.is_none() && !o.read_only {
            self.project.lock.take()
        } else {
            None
        };
        let m = &o.copy.model;
        // a project may have levels and nothing on them yet: the level still has to exist
        let level =
            crate::bigfile::first_level(m).or_else(|| m.level_list().ok()?.first().map(|l| l.0));
        self.set_mode(Mode::Select);
        if let Err(e) = self.swap_model(o.copy.model, level) {
            // the lock taken off above still belongs to the file that stays open
            self.project.lock = kept;
            return Err(e);
        }
        self.current_type = self.current_type.min(self.types.len().saturating_sub(1));
        let p = &mut self.project;
        p.work = Some(o.work);
        // a kept copy of a damaged file binds nothing: Save never reaches it or the damaged file
        let unbound = o.recovered_from.is_some();
        p.path = (!unbound).then_some(o.path);
        p.lock = if unbound { None } else { o.lock.or(kept) };
        p.read_only = o.read_only && !unbound;
        p.recovered_from = o.recovered_from;
        self.project.mark_clean(&self.model);
        Ok(())
    }

    /// Takes a project a worker built on its own working file (an import): untitled, clean, no
    /// undo history. The old project and its working file go with the old model.
    pub(crate) fn adopt_built(
        &mut self,
        m: Model,
        work: WorkFile,
        level: Option<i64>,
    ) -> Result<(), Error> {
        self.set_mode(Mode::Select);
        self.swap_model(m, level)?;
        self.project.work = Some(work);
        self.project.mark_clean(&self.model);
        Ok(())
    }

    /// Replaces the model with `m`, untitled, on a private working file when one can be written:
    /// edits made on it are protected like any others.
    pub(crate) fn adopt_untitled(&mut self, m: Model, level: Option<i64>) -> Result<(), Error> {
        let on_disk = WorkFile::create().ok().and_then(|w| {
            m.save_project_to(w.path(), &mut no_progress).ok()?;
            let d = Model::open_working(w.path().to_str()?).ok()?;
            Some((d, w))
        });
        match on_disk {
            Some((d, w)) => {
                self.swap_model(d, level)?;
                self.project.work = Some(w);
            }
            None => self.swap_model(m, level)?,
        }
        self.project.mark_clean(&self.model);
        Ok(())
    }

    /// Model restored from an autosave snapshot of `project` (`None`: untitled). It is the newest
    /// state but not what the file holds, so it counts as modified.
    pub(crate) fn project_recovered(&mut self, work: WorkFile, project: Option<&Path>) {
        let p = &mut self.project;
        p.work = Some(work);
        p.forced_dirty = true;
        if let Some(path) = project {
            p.path = Some(path.to_path_buf());
            match crate::gfx_guard::acquire_lock(path) {
                Ok(l) => p.lock = Some(l),
                Err(_) => p.read_only = true,
            }
        }
    }

    /// Model restored from a snapshot of the damaged file `damaged`: modified and bound to no
    /// file, so Save asks for a new name and autosave writes only snapshots.
    pub(crate) fn project_recovered_unbound(&mut self, work: WorkFile, damaged: &Path) {
        let p = &mut self.project;
        p.work = Some(work);
        p.forced_dirty = true;
        p.path = None;
        p.lock = None;
        p.read_only = false;
        p.recovered_from = Some(damaged.to_path_buf());
    }

    /// Saves to the current file, on this thread. Untitled or read-only: an error, ask for a name.
    pub fn save_project(&mut self) -> Result<(), ProjectError> {
        let path = match self.project.path() {
            Some(p) if !self.project.read_only => p.to_path_buf(),
            _ => {
                return Err(ProjectError::Model(Error::Invalid(
                    "no file to save to".into(),
                )))
            }
        };
        self.save_project_as(&path)
    }

    /// Saves to `target`, on this thread. The undo history stays.
    pub fn save_project_as(&mut self, target: &Path) -> Result<(), ProjectError> {
        let lock = match self.project.claim(target) {
            Ok(l) => l,
            Err(LockError::Busy) => return Err(ProjectError::Locked(target.to_path_buf())),
            Err(LockError::Io(e)) => return Err(e.into()),
        };
        let mark = self.save_mark();
        match self.project.work_path() {
            Some(w) => save_project_file(w, target, &mut no_progress)?,
            None => self.model.save_project_to(target, &mut no_progress)?,
        }
        save_thumb(target, &thumb_lines(self), crate::logo_ui::project_logo(&self.model).as_ref());
        self.project_saved(target, lock, mark);
        Ok(())
    }

    /// To be taken when a save or an open begins.
    pub(crate) fn save_mark(&self) -> SaveMark {
        SaveMark {
            model: self.project.model,
            head: self.model.change_head(),
            phase: self.model.current_phase().ok(),
            state: self.model.edit_state(&DERIVED).ok(),
            filters: filters_mark(&self.model),
            rev: self.revision,
            side: 0,
        }
    }

    /// A save finished; `mark` is from when it began, so later edits keep the flag on. If the
    /// model has been replaced since, the file holds another model: nothing is bound to it and
    /// `false` comes back, or the next Save would write the new model over that file.
    pub(crate) fn project_saved(
        &mut self,
        target: &Path,
        lock: Option<ProjectLock>,
        mark: SaveMark,
    ) -> bool {
        let p = &mut self.project;
        if mark.model != p.model {
            return false;
        }
        if lock.is_some() {
            p.lock = lock;
        }
        p.path = Some(target.to_path_buf());
        p.read_only = false;
        p.recovered_from = None;
        p.forced_dirty = false;
        p.saved_head = mark.head;
        p.saved_state = mark.state;
        p.saved_phase = mark.phase;
        p.saved_filters = mark.filters;
        p.seen.set((0, false));
        true
    }

    /// Placed drawings (outside the journal) differ from the file, or no longer do.
    pub(crate) fn set_side_dirty(&mut self, dirty: bool) {
        self.project.side_dirty = dirty;
    }

    /// Number of this model; a new one comes with every replacement.
    pub(crate) fn project_id(&self) -> u64 {
        self.project.model
    }

    /// Journal head of the last save or open: moves when a save lands.
    pub(crate) fn saved_head(&self) -> u64 {
        self.project.saved_head
    }

    /// `mark` was taken on the model now in the editor.
    pub(crate) fn is_same_model(&self, mark: SaveMark) -> bool {
        mark.model == self.project.model
    }

    /// Something the user would miss changed after `mark`, in this model or by replacing it.
    pub(crate) fn edited_since(&self, mark: SaveMark) -> bool {
        mark.model != self.project.model
            || !self.model.changes_since(mark.head).only(&DERIVED)
            || self.model.current_phase().ok() != mark.phase
    }

    /// The project has changes the file does not: the model or the placed drawings.
    pub fn is_dirty(&self) -> bool {
        self.project.side_dirty || self.is_model_dirty()
    }

    /// The model (elements and data) has changes the file does not.
    pub fn is_model_dirty(&self) -> bool {
        let p = &self.project;
        if p.forced_dirty {
            return true;
        }
        let head = self.model.change_head();
        if head == p.saved_head {
            return false;
        }
        let (seen, was) = p.seen.get();
        if seen == head {
            return was;
        }
        // undo/redo back to the saved step: same content whatever the journal shows
        let back = p.saved_state.is_some() && self.model.edit_state(&DERIVED).ok() == p.saved_state;
        let dirty = (!back && !self.model.changes_since(p.saved_head).only(&DERIVED))
            || self.model.current_phase().ok() != p.saved_phase
            || filters_mark(&self.model) != p.saved_filters;
        p.seen.set((head, dirty));
        dirty
    }

    /// File name of the open project; `None` while untitled.
    pub fn project_name(&self) -> Option<String> {
        let n = self.project.path()?.file_name()?;
        Some(n.to_string_lossy().into_owned())
    }
}

fn new_working() -> Option<(Model, WorkFile)> {
    let work = WorkFile::create().ok()?;
    let m = Model::open_working(work.path().to_str()?).ok()?;
    Some((m, work))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::OpFn;
    use std::sync::Arc;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-app-f1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn add_level(e: &mut Editor, name: &str, z: f64) {
        let name = name.to_string();
        let op = OpFn(Arc::new(move |m: &mut Model| {
            m.add_level(&name, z).map(|_| ())
        }));
        e.run_op(op).unwrap();
    }

    fn levels(e: &Editor) -> usize {
        e.model.level_list().unwrap().len()
    }

    #[test]
    fn undo_back_to_the_saved_state_is_clean() {
        let d = dir("undo-clean");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        add_level(&mut e, "L2", 3.0);
        e.save_project_as(&p).unwrap();
        add_level(&mut e, "L3", 6.0);
        add_level(&mut e, "L4", 9.0);
        assert!(e.is_dirty());
        e.undo();
        assert!(e.is_dirty(), "one step still differs");
        e.undo();
        assert!(!e.is_dirty(), "back at the saved state");
        e.undo();
        assert!(e.is_dirty(), "past the saved state");
        add_level(&mut e, "L5", 3.0);
        assert!(e.is_dirty(), "a new step in place of the saved one");
        // a new project with an edit undone: nothing to lose
        e.new_project().unwrap();
        add_level(&mut e, "L6", 3.0);
        e.undo();
        assert!(!e.is_dirty());
        // placed drawings count on their own, the model stays clean
        e.set_side_dirty(true);
        assert!(e.is_dirty() && !e.is_model_dirty());
        e.set_side_dirty(false);
        assert!(!e.is_dirty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn round_trip_through_the_editor() {
        let d = dir("rt");
        let p = d.join("house.ddc");
        let mut e = Editor::start_project().unwrap();
        let work = e.project.work_path().unwrap().to_path_buf();
        assert!(work.exists(), "on disk from the start");
        add_level(&mut e, "L2", 3.0);
        assert!(e.save_project().is_err(), "untitled has no file yet");
        e.save_project_as(&p).unwrap();
        assert_eq!(e.project_name().as_deref(), Some("house.ddc"));
        let mut b = Editor::start_project().unwrap();
        // the first editor holds the file
        assert!(matches!(b.open_project(&p), Err(ProjectError::Locked(_))));
        b.open_project_read_only(&p).unwrap();
        assert!(b.project.read_only());
        assert_eq!(levels(&b), 2);
        assert!(!b.can_undo(), "undo history starts empty on open");
        assert!(b.save_project().is_err(), "read-only asks for a new name");
        drop(e);
        assert!(!work.exists(), "working file goes with the project");
        b.open_project(&p).unwrap();
        assert!(!b.project.read_only() && !b.is_dirty());
        assert_eq!(levels(&b), 2);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn dirty_flag_follows_edits_and_saves_and_save_keeps_undo() {
        let d = dir("dirty");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        assert!(!e.is_dirty(), "a new project has nothing to lose");
        add_level(&mut e, "L2", 3.0);
        assert!(e.is_dirty());
        e.save_project_as(&p).unwrap();
        assert!(!e.is_dirty());
        assert!(e.can_undo(), "save does not clear undo");
        // moving between levels and reloading are not edits
        let first = e.model.level_list().unwrap()[0].0;
        e.set_level(first);
        e.reload().unwrap();
        assert!(!e.is_dirty());
        e.undo();
        assert_eq!(levels(&e), 1);
        assert!(e.is_dirty(), "undo after a save differs from the file");
        e.redo();
        assert!(e.is_dirty(), "redo logs a new step: still counted as a change");
        e.undo();
        e.save_project().unwrap();
        assert!(!e.is_dirty());
        // edits during a save stay modified
        let mark = e.save_mark();
        add_level(&mut e, "L3", 6.0);
        assert!(e.project_saved(&p, None, mark));
        assert!(e.is_dirty());
        // derived data only: not modified
        e.save_project().unwrap();
        e.model
            .sql()
            .execute("INSERT INTO mass_cache(mass, key) VALUES (1, 'x')", [])
            .unwrap();
        assert!(!e.is_dirty());
        // new project: clean again, no history
        add_level(&mut e, "L9", 9.0);
        e.new_project().unwrap();
        assert!(!e.is_dirty() && !e.can_undo() && e.project.path().is_none());
        assert_eq!(levels(&e), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn failed_save_keeps_the_flag_and_the_file() {
        let d = dir("fail");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        e.save_project_as(&p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        add_level(&mut e, "L2", 3.0);
        let occupied = d.join("folder.ddc");
        std::fs::create_dir_all(&occupied).unwrap();
        assert!(e.save_project_as(&occupied).is_err());
        assert!(e.is_dirty());
        assert_eq!(e.project.path(), Some(p.as_path()), "still on the old file");
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn replacing_the_model_detaches_the_file() {
        let d = dir("detach");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        e.save_project_as(&p).unwrap();
        e.replace_model(Model::open(":memory:").unwrap()).unwrap();
        assert!(
            e.project.path().is_none(),
            "Save must not write a sample over the file"
        );
        assert!(!e.is_dirty());
        // the lock is released with the file
        drop(ProjectLock::acquire(&p).unwrap());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn reopening_the_same_file_keeps_its_lock() {
        let d = dir("same");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        add_level(&mut e, "L2", 3.0);
        e.save_project_as(&p).unwrap();
        add_level(&mut e, "L3", 6.0);
        e.open_project(&p).unwrap();
        assert_eq!(levels(&e), 2, "changes dropped, the file is back");
        assert!(!e.is_dirty());
        assert!(matches!(ProjectLock::acquire(&p), Err(LockError::Busy)));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn recent_list_and_thumbnail() {
        let d = dir("recent");
        let p = d.join("tower.ddc");
        let mut e = Editor::start_project().unwrap();
        let spec = ddc_model::WallSpec {
            a: (0.0, 0.0),
            b: (5.0, 0.0),
            level: e.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: e.types[0].id,
            offset0: None,
        };
        let op = OpFn(Arc::new(move |m: &mut Model| m.add_wall(&spec).map(|_| ())));
        e.run_op(op).unwrap();
        e.save_project_as(&p).unwrap();
        let missing = d.join("gone.ddc").to_string_lossy().into_owned();
        let list = recent_from(&[p.to_string_lossy().into_owned(), missing]);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "tower");
        assert_eq!(list[0].path, p);
        assert!(list[0].modified.is_some());
        assert!(list[0].thumbnail.as_ref().is_some_and(|t| t.exists()));
        assert!(list[1].modified.is_none() && list[1].thumbnail.is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn thumbnail_draws_inside_and_skips_an_empty_model() {
        assert!(render_thumb(&[], THUMB_SIZE).is_none());
        let px = render_thumb(&[[0.0, 0.0, 10.0, 5.0], [3.0, 3.0, 3.0, 3.0]], (64, 40)).unwrap();
        assert_eq!(px.len(), 64 * 40 * 4);
        assert!(px.chunks(4).any(|p| p[3] == 255));
        assert!(
            px.chunks(4).any(|p| p[3] == 0),
            "background stays transparent"
        );
    }

    #[test]
    fn extension_and_keys() {
        assert_eq!(with_project_ext("a/b".into()), PathBuf::from("a/b.ddc"));
        assert_eq!(with_project_ext("a/b.DDC".into()), PathBuf::from("a/b.DDC"));
        assert_eq!(path_key(Path::new("C:/A")), path_key(Path::new("c:/a")));
        assert_ne!(path_key(Path::new("c:/a")), path_key(Path::new("c:/b")));
    }

    /// A working file its owner left behind in a crash: edits in it, its note beside it, no lock.
    fn dead_work(work_dir: &Path, name: &str, project: Option<&Path>) -> PathBuf {
        let w = work_dir.join(format!("{name}.ddc"));
        let mut m = Model::open_working(w.to_str().unwrap()).unwrap();
        m.add_level("only here", 7.0).unwrap();
        drop(m);
        write_note(&w, project);
        w
    }

    fn side_snaps(auto: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(auto)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".snap-"))
            .collect()
    }

    /// A finished working file still held open, whose discard mark cannot be written: it is
    /// moved aside, and the next start does not offer it as lost work.
    #[test]
    fn a_failed_discard_mark_is_not_offered_by_the_sweep() {
        let d = dir("nomark");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let w = dead_work(&work, "1-0-9", None);
        let held = |_: &Path| {};
        let no_mark = |_: &Path| -> std::io::Result<()> { Err(std::io::Error::other("denied")) };
        retire(&w, &held, &no_mark);
        assert!(!discard_mark(&w).exists());
        assert!(!w.exists(), "not left in the work folder unmarked");
        assert!(work.join("_orphans").join("1-0-9.ddc").exists(), "moved, never deleted");
        assert!(sweep_dir(&work, &auto).kept.is_empty());
        assert!(side_snaps(&auto).is_empty(), "no snapshot, no offer");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_stuck_discard_mark_is_moved_off_so_real_edits_are_offered() {
        let d = dir("stuckmark");
        let w = d.join("1-0-8.ddc");
        // a mark that cannot be removed as a file
        std::fs::create_dir_all(discard_mark(&w).join("x")).unwrap();
        set_nothing_to_save(&w, false);
        assert!(!discard_mark(&w).exists(), "the sweep would skip real edits");
        assert!(beside(&w, ".discard.stale").exists(), "moved, never deleted");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn sweep_leaves_live_working_files() {
        let d = dir("live");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let w = dead_work(&work, "1-0-4", None);
        let held = ProjectLock::acquire(&w).unwrap();
        assert!(sweep_dir(&work, &auto).kept.is_empty());
        assert!(w.exists());
        assert!(side_snaps(&auto).is_empty());
        drop(held);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_dead_working_file_goes_only_after_a_checked_snapshot() {
        let d = dir("sweep");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let proj = d.join("house.ddc");
        let w = dead_work(&work, "1-0-5", Some(&proj));
        let swept = sweep_dir(&work, &auto);
        assert!(swept.kept.is_empty());
        assert!(!w.exists(), "replaced by the snapshot");
        assert!(!beside(&w, ".note").exists());
        let snaps = side_snaps(&auto);
        assert_eq!(snaps.len(), 1);
        let db = rusqlite::Connection::open(&snaps[0]).unwrap();
        let n: i64 = db
            .query_row(
                "SELECT count(*) FROM level WHERE name='only here'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "the edit is in the snapshot");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_failed_snapshot_moves_the_working_file_aside_and_says_so() {
        let d = dir("sweepfail");
        let work = d.join("work");
        std::fs::create_dir_all(&work).unwrap();
        // the autosave folder cannot exist: a file sits on its name
        let auto = d.join("full");
        std::fs::write(&auto, b"not a folder").unwrap();
        let w = dead_work(&work, "1-0-6", None);
        let bytes = std::fs::read(&w).unwrap();
        let swept = sweep_dir(&work, &auto);
        assert_eq!(swept.kept.len(), 1);
        let kept = &swept.kept[0];
        assert!(kept.starts_with(work.join("_orphans")), "{kept:?}");
        assert_eq!(std::fs::read(kept).unwrap(), bytes, "not one byte lost");
        assert!(!w.exists());
        // a later start with a good folder does not touch what was moved aside
        let auto2 = d.join("auto2");
        assert!(sweep_dir(&work, &auto2).kept.is_empty());
        assert!(kept.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_damaged_working_file_is_kept_too() {
        let d = dir("sweepbad");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let w = work.join("1-0-7.ddc");
        std::fs::write(&w, vec![0x5Au8; 8192]).unwrap();
        let swept = sweep_dir(&work, &auto);
        assert_eq!(swept.kept.len(), 1);
        assert!(swept.kept[0].exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_working_file_the_owner_discarded_needs_no_snapshot() {
        let d = dir("sweepmark");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let w = dead_work(&work, "1-0-8", None);
        std::fs::write(discard_mark(&w), b"").unwrap();
        assert!(sweep_dir(&work, &auto).kept.is_empty());
        assert!(!w.exists() && !discard_mark(&w).exists());
        assert!(side_snaps(&auto).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_crash_while_opening_leaves_nothing_to_offer() {
        let d = dir("sweepempty");
        let (work, auto) = (d.join("work"), d.join("auto"));
        std::fs::create_dir_all(&work).unwrap();
        let w = work.join("1-0-9.ddc");
        std::fs::write(&w, b"").unwrap();
        assert!(sweep_dir(&work, &auto).kept.is_empty());
        assert!(!w.exists());
        assert!(side_snaps(&auto).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_panic_keeps_the_working_file() {
        let d = dir("panic");
        let owned = |p: &Path| {
            std::fs::write(p, b"edits").unwrap();
            WorkFile {
                path: p.to_path_buf(),
                _lock: Some(ProjectLock::acquire(p).unwrap()),
            }
        };
        let p = d.join("1-0-3.ddc");
        let w = owned(&p);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owner = w;
            panic!("crash in the frame");
        }));
        assert!(r.is_err());
        assert!(p.exists(), "the only copy of the edits");
        // released, so the next start can take it
        drop(ProjectLock::acquire(&p).unwrap());
        // without a panic it goes as before
        let w = owned(&p);
        write_note(&p, None);
        drop(w);
        assert!(!p.exists() && !beside(&p, ".note").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn note_round_trip_is_per_working_file() {
        let d = dir("note");
        let (a, b) = (d.join("a.ddc"), d.join("b.ddc"));
        let (pa, pb) = (d.join("\u{434}\u{43e}\u{43c} \u{43f}\u{440}\u{43e}\u{435}\u{43a}\u{442}.ddc"), d.join("b-proj.ddc"));
        assert_eq!(read_note(&a), None);
        write_note(&a, Some(&pa));
        write_note(&b, Some(&pb));
        assert_eq!(read_note(&a).as_deref(), Some(pa.as_path()));
        assert_eq!(read_note(&b).as_deref(), Some(pb.as_path()));
        write_note(&a, None);
        assert_eq!(read_note(&a), None, "untitled again");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_save_that_ends_after_the_model_changed_binds_nothing() {
        let d = dir("bind");
        let (p, sample) = (d.join("project.ddc"), d.join("from-sample.ddc"));
        let mut e = Editor::start_project().unwrap();
        add_level(&mut e, "L2", 3.0);
        e.save_project_as(&p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        // a background save begins, then the model is replaced by a sample
        let mark = e.save_mark();
        e.replace_model(Model::open(":memory:").unwrap()).unwrap();
        let lock = ProjectLock::acquire(&sample).unwrap();
        assert!(!e.project_saved(&sample, Some(lock), mark));
        assert!(e.project.path().is_none(), "no file behind the sample");
        // the lock of the finished job went with it
        drop(ProjectLock::acquire(&sample).unwrap());
        assert!(e.save_project().is_err(), "Save asks for a name");
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn every_kind_of_edit_counts_as_something_to_lose() {
        let mut e = Editor::start_project().unwrap();
        assert!(!e.is_dirty(), "the seed is nothing to lose");
        // neither walls nor rooms: what the old six-table check missed
        add_level(&mut e, "L2", 3.0);
        assert!(e.is_dirty(), "a level");
        let mut e = Editor::start_project().unwrap();
        let op = OpFn(Arc::new(|m: &mut Model| {
            m.add_grid(&ddc_model::GridSpec::line("A", (0.0, 0.0), (5.0, 0.0)))
                .map(|_| ())
        }));
        e.run_op(op).unwrap();
        assert!(e.is_dirty(), "a grid");
    }

    #[test]
    fn the_current_phase_is_saved_and_counts_as_a_change() {
        let d = dir("phase");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        e.save_project_as(&p).unwrap();
        assert!(!e.is_dirty());
        let phases = e.model.phases().unwrap();
        let (first, now) = (phases[0].id, e.model.current_phase().unwrap());
        assert_ne!(first, now);
        e.model.set_current_phase(first).unwrap();
        assert!(e.is_dirty(), "else closing loses the choice with no prompt");
        e.save_project().unwrap();
        assert!(!e.is_dirty());
        e.open_project(&p).unwrap();
        assert_eq!(e.model.current_phase().unwrap(), first);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn derived_tables_are_named_in_the_list() {
        let m = Model::open(":memory:").unwrap();
        let mut st = m
            .sql()
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap();
        let names: Vec<String> = st
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for t in &names {
            let by_name = ["_qty", "_cache", "_geo", "_thumb", "_issue"]
                .iter()
                .any(|s| t.ends_with(s));
            assert!(
                !by_name || DERIVED.contains(&t.as_str()),
                "{t}: add to DERIVED"
            );
        }
    }

    #[test]
    fn regenerating_does_not_look_like_an_edit() {
        let mut e = Editor::start_project().unwrap();
        let m = crate::help::sample::build(true).unwrap();
        let level = crate::help::sample::first_level(&m);
        e.adopt_untitled(m, level).unwrap();
        let db = e.model.sql();
        let rows: i64 = db
            .query_row("SELECT count(*) FROM wall_qty", [], |r| r.get(0))
            .unwrap();
        assert!(rows > 0, "or this proves nothing");
        db.execute("DELETE FROM wall_qty", []).unwrap();
        assert!(!e.is_dirty());
    }

    #[test]
    fn a_model_that_fails_to_load_leaves_everything_as_it_was() {
        let d = dir("reload");
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        add_level(&mut e, "L2", 3.0);
        e.save_project_as(&p).unwrap();
        add_level(&mut e, "unsaved", 6.0);
        let level = e.level;
        let bad = Model::open(":memory:").unwrap();
        bad.sql()
            .execute_batch("DROP TABLE wall_type_layer;")
            .unwrap();
        assert!(e.swap_model(bad, None).is_err());
        assert_eq!(e.project.path(), Some(p.as_path()), "still on its file");
        assert!(e.is_dirty(), "the unsaved edit is still here");
        assert_eq!(e.level, level);
        assert_eq!(levels(&e), 3);
        assert!(e.can_undo(), "history stays");
        assert!(
            matches!(ProjectLock::acquire(&p), Err(LockError::Busy)),
            "lock stays"
        );
        assert!(
            e.project.work_path().is_some_and(Path::exists),
            "working file stays"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    fn put(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    fn body(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn move_aside_keeps_wal_with_db() {
        let d = dir("aside-wal");
        let p = d.join("x.ddc");
        put(&p, "db");
        put(&beside(&p, "-wal"), "wal");
        put(&beside(&p, "-shm"), "shm");
        let to = move_aside(&p, &d.join("work")).unwrap();
        assert_eq!(body(&to), "db");
        assert_eq!(body(&beside(&to, "-wal")), "wal", "committed pages travel with the db");
        assert_eq!(body(&beside(&to, "-shm")), "shm");
        assert!(!p.exists() && !beside(&p, "-wal").exists(), "nothing left behind");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn move_aside_never_replaces_earlier_orphan() {
        let d = dir("aside-twice");
        let work = d.join("work");
        let orphans = work.join("_orphans");
        std::fs::create_dir_all(&orphans).unwrap();
        put(&orphans.join("x.ddc"), "old");
        // a stray WAL of another earlier orphan also takes its name
        put(&beside(&orphans.join("x (2).ddc"), "-wal"), "old wal");
        let p = d.join("x.ddc");
        put(&p, "new");
        put(&beside(&p, "-wal"), "new wal");
        let to = move_aside(&p, &work).unwrap();
        assert_eq!(to, orphans.join("x (3).ddc"));
        assert_eq!(body(&to), "new");
        assert_eq!(body(&beside(&to, "-wal")), "new wal");
        assert_eq!(body(&orphans.join("x.ddc")), "old");
        assert_eq!(body(&beside(&orphans.join("x (2).ddc"), "-wal")), "old wal");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn move_aside_failed_sidecar_rolls_back() {
        let d = dir("aside-fail");
        let work = d.join("work");
        let p = d.join("x.ddc");
        put(&p, "db");
        put(&beside(&p, "-wal"), "wal");
        put(&beside(&p, "-shm"), "shm");
        let fail_shm = |a: &Path, b: &Path| {
            if a.to_string_lossy().ends_with("-shm") {
                Err(std::io::Error::other("held"))
            } else {
                std::fs::rename(a, b)
            }
        };
        assert!(move_aside_with(&p, &work, &fail_shm).is_err());
        assert_eq!(body(&p), "db");
        assert_eq!(body(&beside(&p, "-wal")), "wal", "the moved WAL came back");
        assert_eq!(body(&beside(&p, "-shm")), "shm");
        let left = std::fs::read_dir(work.join("_orphans")).unwrap().count();
        assert_eq!(left, 0, "nothing stays half-moved");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn failed_rollback_keeps_main_and_reports() {
        let d = dir("aside-rollback");
        let work = d.join("work");
        let p = d.join("x.ddc");
        put(&p, "db");
        put(&beside(&p, "-wal"), "wal");
        put(&beside(&p, "-shm"), "shm");
        // the shm move fails, then the WAL cannot go back
        let fail = |a: &Path, b: &Path| {
            let n = a.to_string_lossy();
            if n.ends_with("-shm") || (n.contains("_orphans") && n.ends_with("-wal")) {
                Err(std::io::Error::other("held"))
            } else {
                std::fs::rename(a, b)
            }
        };
        let e = move_aside_with(&p, &work, &fail).unwrap_err();
        assert_eq!(body(&p), "db", "the main file stays");
        assert_eq!(e.stranded.len(), 1);
        let (from, at) = &e.stranded[0];
        assert_eq!(from, &beside(&p, "-wal"));
        assert_eq!(body(at), "wal", "the report names where the WAL is");
        assert!(e.to_string().contains(&at.display().to_string()));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn sweep_skips_half_moved_orphan() {
        let d = dir("sweep-half");
        let work = d.join("work");
        let orphans = work.join("_orphans");
        std::fs::create_dir_all(&orphans).unwrap();
        let auto = d.join("auto");
        std::fs::create_dir_all(&auto).unwrap();
        let p = work.join("1-1-1.ddc");
        put(&p, "not a database");
        let wal = beside(&orphans.join("1-1-1.ddc"), "-wal");
        put(&wal, "wal");
        let out = sweep_dir(&work, &auto);
        assert_eq!(body(&p), "not a database", "the main file is not deleted");
        assert_eq!(out.kept, vec![p.clone()]);
        assert_eq!(out.stranded, vec![wal.clone()]);
        assert_eq!(body(&wal), "wal");
        let _ = std::fs::remove_dir_all(d);
    }

    /// `_orphans` with only the listed files, a main of `name` in the work folder.
    fn half_setup(tag: &str, files: &[&str]) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let d = dir(tag);
        let work = d.join("work");
        let orphans = work.join("_orphans");
        std::fs::create_dir_all(&orphans).unwrap();
        let auto = d.join("auto");
        std::fs::create_dir_all(&auto).unwrap();
        for f in files {
            put(&orphans.join(f), f);
        }
        let p = work.join("1-1-1.ddc");
        put(&p, "not a database");
        (d, work, auto, p)
    }

    #[test]
    fn sweep_skips_half_moved_orphan_after_gap() {
        // copies 1 and 2 were removed by the user: the stranded WAL sits behind a gap
        let (d, work, auto, p) = half_setup("half-gap", &["1-1-1 (3).ddc-wal"]);
        let wal = work.join("_orphans").join("1-1-1 (3).ddc-wal");
        let out = sweep_dir(&work, &auto);
        assert_eq!(body(&p), "not a database", "the main file is not deleted");
        assert_eq!(out.kept, vec![p.clone()]);
        assert_eq!(out.stranded, vec![wal.clone()]);
        assert_eq!(body(&wal), "1-1-1 (3).ddc-wal");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn half_moved_finds_every_stranded_sidecar() {
        let (d, work, _auto, p) = half_setup(
            "half-all",
            &[
                "1-1-1.ddc-shm",
                "1-1-1 (7).ddc-wal",
                "1-1-1 (7).ddc-shm",
                "1-1-1 (x).ddc-wal",
                "2-2-2 (4).ddc-wal",
            ],
        );
        let o = work.join("_orphans");
        let got = half_moved(&p, &work);
        let want = ["1-1-1 (7).ddc-shm", "1-1-1 (7).ddc-wal", "1-1-1.ddc-shm"];
        assert_eq!(got, want.iter().map(|f| o.join(f)).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn half_moved_ignores_full_triples_and_empty_folder() {
        let (d, work, _auto, p) = half_setup(
            "half-full",
            &[
                "1-1-1 (2).ddc",
                "1-1-1 (2).ddc-wal",
                "1-1-1 (2).ddc-shm",
                "1-1-1 (5).ddc",
                "1-1-1 (5).ddc-wal",
            ],
        );
        assert!(half_moved(&p, &work).is_empty(), "a main beside its sidecars is a whole copy");
        let _ = std::fs::remove_dir_all(work.join("_orphans"));
        assert!(half_moved(&p, &work).is_empty(), "no _orphans folder at all");
        std::fs::create_dir_all(work.join("_orphans")).unwrap();
        assert!(half_moved(&p, &work).is_empty(), "an empty _orphans folder");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn orphan_names_match_only_numbered_copies() {
        let n = Path::new("a.b.ddc");
        assert!(is_orphan_name("a.b.ddc", n));
        assert!(is_orphan_name("a.b (12).ddc", n));
        assert!(!is_orphan_name("a.b ().ddc", n));
        assert!(!is_orphan_name("a.b (1x).ddc", n));
        assert!(!is_orphan_name("a.bc (2).ddc", n));
        assert!(!is_orphan_name("a.b (2).ddcx", n));
    }

    #[test]
    fn the_sample_lands_on_a_working_file_of_its_own() {
        let mut e = Editor::start_project().unwrap();
        let m = crate::help::sample::build(true).unwrap();
        let level = crate::help::sample::first_level(&m);
        e.adopt_untitled(m, level).unwrap();
        assert_eq!(e.walls.len(), 7);
        assert!(!e.is_dirty() && e.project.path().is_none());
        assert!(e.project.work_path().is_some_and(Path::exists));
    }
}
