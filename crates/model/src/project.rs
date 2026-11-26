// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Native project file `.ddc` (SQLite). The user's file is never edited in place:
//! open copies it to a private working file, save writes a temp file beside the target through
//! the online backup API, fsyncs and renames it over, so a failure or a full disk leaves the old
//! file whole. A lock file beside the project keeps two instances from saving over each other.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use web_time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use crate::{file, schema, Error, Model};

/// Extension of a project file.
pub const PROJECT_EXT: &str = "ddc";

const STEP_PAGES: i32 = 2048;
/// Steps a busy source may wait, 20 ms each.
const BUSY_STEPS: u32 = 1500;
/// How long a rename waits for a scanner or a reader to let go of the old file (Windows).
pub const RENAME_WAIT: Duration = Duration::from_secs(30);

/// Why a project file did not open or save; the UI turns each into a plain message.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("file is in use by another instance: {}", .0.display())]
    Locked(PathBuf),
    #[error("file schema version {found} is newer than supported {supported}")]
    Newer { found: i64, supported: i64 },
    #[error("file is damaged: {0}")]
    Damaged(String),
    #[error("not a project file: {}", .0.display())]
    NotProject(PathBuf),
    #[error(transparent)]
    Model(Error),
}

impl From<Error> for ProjectError {
    fn from(e: Error) -> Self {
        match e {
            Error::NewerVersion { found, supported } => Self::Newer { found, supported },
            Error::Corrupt(s) => Self::Damaged(s),
            Error::Db(rusqlite::Error::SqliteFailure(f, m))
                if matches!(
                    f.code,
                    rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
                ) =>
            {
                Self::Damaged(m.unwrap_or_else(|| f.to_string()))
            }
            e => Self::Model(e),
        }
    }
}

impl From<rusqlite::Error> for ProjectError {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e).into()
    }
}

impl From<std::io::Error> for ProjectError {
    fn from(e: std::io::Error) -> Self {
        Self::Model(Error::Io(e))
    }
}

fn side(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Removes a private database file with its log files and migration copies (never a user's file).
pub fn remove_db_files(p: &Path) {
    for s in ["", "-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(side(p, s));
    }
    let (Some(dir), Some(name)) = (p.parent(), p.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let pre = format!("{name}.bak-v");
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(&pre) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Copies `src` page by page into a new file `tmp`; `on_step(done, total)` may stop it with an
/// error. The result is one portable file with no log beside it.
fn backup_into(
    src: &Connection,
    tmp: &Path,
    check: bool,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut out = Connection::open(tmp)?;
    {
        let b = Backup::new(src, &mut out)?;
        let mut waited = 0;
        loop {
            let r = b.step(STEP_PAGES)?;
            let p = b.progress();
            let total = p.pagecount.max(0) as u64;
            on_step(total.saturating_sub(p.remaining.max(0) as u64), total)?;
            match r {
                StepResult::Done => break,
                StepResult::More => waited = 0,
                _ => {
                    waited += 1;
                    if waited > BUSY_STEPS {
                        return Err(Error::Invalid("database stays busy".into()));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
    out.execute_batch("PRAGMA journal_mode=DELETE;")?;
    if check {
        file::quick(&out)?;
    }
    out.close().map_err(|(_, e)| Error::Db(e))?;
    fs::OpenOptions::new().write(true).open(tmp)?.sync_all()?;
    Ok(())
}

/// A log left beside the target would replay over the new file: merge it into the old file
/// first. Failing here stops the save with the old file intact.
fn settle_log(dst: &Path) -> Result<(), Error> {
    if !side(dst, "-wal").exists() {
        return Ok(());
    }
    let db = Connection::open(dst)?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.execute_batch("PRAGMA journal_mode=DELETE;")?;
    db.close().map_err(|(_, e)| Error::Db(e))?;
    if side(dst, "-wal").exists() {
        return Err(Error::Invalid(format!("{} is in use", dst.display())));
    }
    Ok(())
}

/// Rename that is on the disk when it returns: «Saved» survives a power cut right after it.
#[cfg(windows)]
fn rename_over(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const MOVEFILE_REPLACE_EXISTING: u32 = 1;
    const MOVEFILE_WRITE_THROUGH: u32 = 8;
    const PATH_NOT_FOUND: i32 = 3;
    const NAME_TOO_LONG: i32 = 206;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (from, to) = (wide(tmp), wide(dst));
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    // SAFETY: both are NUL-terminated wide strings that outlive the call
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } != 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        // over the plain path limit: std knows the long form
        Some(PATH_NOT_FOUND | NAME_TOO_LONG) => fs::rename(tmp, dst),
        _ => Err(e),
    }
}

#[cfg(not(windows))]
fn rename_over(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    fs::rename(tmp, dst)?;
    if let Some(dir) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// A scanner or a reader may let go of `dst` soon. Not so for a folder or a read-only file.
fn may_clear(e: &std::io::Error, dst: &Path) -> bool {
    cfg!(windows)
        && matches!(e.raw_os_error(), Some(5 | 32 | 33 | 1224))
        && fs::metadata(dst).map_or(true, |m| !m.is_dir() && !m.permissions().readonly())
}

/// What an earlier crash left beside `dst`. The caller holds the lock on `dst`, so no live
/// instance writes these; backups have their own prefix.
fn remove_stale_temps(dst: &Path) {
    let Some(name) = dst.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let dir = dst.parent().filter(|d| !d.as_os_str().is_empty());
    let pre = format!("{name}.tmp-");
    for e in fs::read_dir(dir.unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with(&pre) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Atomic replace of `dst` by `tmp` in the same folder, durable on return, with no database log
/// step: for any file. A file held by a scanner or a reader is retried for `wait`; `on_step` may
/// stop the wait with an error. On failure both files are as they were.
pub fn replace_file(
    tmp: &Path,
    dst: &Path,
    wait: Duration,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    let started = Instant::now();
    let mut pause = Duration::from_millis(50);
    loop {
        match rename_over(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e) if may_clear(&e, dst) && started.elapsed() < wait => {
                // Cancel stops the wait
                on_step(1, 1)?;
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_secs(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn replace(
    tmp: &Path,
    dst: &Path,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    settle_log(dst)?;
    replace_file(tmp, dst, RENAME_WAIT, on_step)?;
    remove_stale_temps(dst);
    Ok(())
}

fn safe_write(
    src: &Connection,
    dst: &Path,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    let tmp = file::tmp_path(dst);
    let mut res = backup_into(src, &tmp, true, &mut *on_step);
    if res.is_ok() {
        res = replace(&tmp, dst, on_step);
    }
    if res.is_err() {
        remove_db_files(&tmp);
    }
    res
}

/// Safe save from a working file on its own connection: runs on any thread while the editor
/// keeps writing. The read transaction pins one consistent state for the whole copy.
pub fn save_project_file(
    work: &Path,
    dst: &Path,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<(), Error> {
    fs::metadata(work)?;
    let src = Connection::open(work)?;
    src.busy_timeout(Duration::from_secs(5))?;
    src.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
    let res = safe_write(&src, dst, on_step);
    let _ = src.execute_batch("COMMIT;");
    res
}

/// Snapshot `<file>.snap-<ms>` of a working file on its own connection, for a worker thread.
pub fn autosave_file(work: &Path, file_name: &Path, keep: usize) -> Result<PathBuf, Error> {
    fs::metadata(work)?;
    let db = Connection::open(work)?;
    db.busy_timeout(Duration::from_secs(5))?;
    file::autosave_db(&db, file_name, keep)
}

/// Format version of a project file; refuses newer files and files that are not projects.
pub fn probe_project(path: &Path) -> Result<i64, ProjectError> {
    fs::metadata(path)?;
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let found = schema::detect(&db)?;
    if found > schema::VERSION {
        return Err(ProjectError::Newer {
            found,
            supported: schema::VERSION,
        });
    }
    if found <= 0 {
        return Err(ProjectError::NotProject(path.to_path_buf()));
    }
    Ok(found)
}

/// Result of [`open_project_copy`].
pub struct OpenedCopy {
    pub model: Model,
    /// The file had this older format; the working copy is upgraded, the file itself is not.
    pub upgraded_from: Option<i64>,
    /// Copy of the older file, written because the first save will replace it.
    pub backup: Option<PathBuf>,
}

/// Opens project `src` on a new working file `work`. `src` is only read; an older format is
/// upgraded on the copy, and with `keep_backup` a `<src>.bak-v<old>` is written first.
pub fn open_project_copy(
    src: &Path,
    work: &Path,
    keep_backup: bool,
    on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
) -> Result<OpenedCopy, ProjectError> {
    let found = probe_project(src)?;
    let old = found < schema::VERSION;
    let db = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut backup = None;
    if old && keep_backup {
        backup = Some(file::project_backup(&db, src, found)?);
    }
    // Model::open below runs the integrity check on the copy
    backup_into(&db, work, false, on_step)?;
    drop(db);
    let w = work
        .to_str()
        .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
    let model = Model::open_working(w)?;
    if old {
        // migration copy of the private file: the user's file is the real one
        let _ = fs::remove_file(format!("{w}.bak-v{found}"));
    }
    Ok(OpenedCopy {
        model,
        upgraded_from: old.then_some(found),
        backup,
    })
}

impl Model {
    /// A private working file: losing it in a power cut costs only what autosave covers, so
    /// commits do not wait for the disk.
    pub fn open_working(path: &str) -> Result<Model, Error> {
        let m = Model::open(path)?;
        m.db.execute_batch("PRAGMA synchronous=NORMAL;")?;
        Ok(m)
    }

    /// Safe save on this connection (a model kept in memory has no second one).
    pub fn save_project_to(
        &self,
        dst: &Path,
        on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        safe_write(&self.db, dst, on_step)
    }

    /// Copy of the project for another person. Cloud consent is per project and per person, so
    /// the grants (`egress.*` except the local-only switch, `mcp.*`) never travel; the AI journal
    /// is left out unless `keep_journal`. The strip happens on the temporary file, so no
    /// unstripped copy ever has the final name. This model is not changed.
    pub fn save_shared_copy(
        &self,
        dst: &Path,
        keep_journal: bool,
        on_step: &mut dyn FnMut(u64, u64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let tmp = file::tmp_path(dst);
        let mut res = backup_into(&self.db, &tmp, true, &mut *on_step)
            .and_then(|_| crate::ai_audit::strip_for_copy(&tmp, keep_journal));
        if res.is_ok() {
            res = replace(&tmp, dst, on_step);
        }
        if res.is_err() {
            remove_db_files(&tmp);
        }
        res
    }
}

/// Lock taken by another live instance, or the folder refuses a lock file.
#[derive(Debug)]
pub enum LockError {
    Busy,
    Io(std::io::Error),
}

/// Lock on `<target>.lock`, held while this value lives. The system releases it when the process
/// dies, so a crash leaves no stale lock.
#[derive(Debug)]
pub struct ProjectLock {
    file: Option<File>,
    path: PathBuf,
}

impl ProjectLock {
    /// Process that holds the lock of `target` right now, from its side file; `None` when it is
    /// free or the file is unreadable.
    pub fn holder_pid(target: &Path) -> Option<u32> {
        fs::read_to_string(side(target, ".lock.pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn acquire(target: &Path) -> Result<Self, LockError> {
        let path = side(target, ".lock");
        let file = take_lock(&path)?;
        // the lock file itself cannot be read by others while it is held: the owner is told apart
        // by a second small file, for "switch to that window"
        let _ = fs::write(side(target, ".lock.pid"), crate::process_id().to_string());
        Ok(Self {
            file: Some(file),
            path,
        })
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
        let mut pid = self.path.as_os_str().to_owned();
        pid.push(".pid");
        let _ = fs::remove_file(PathBuf::from(pid));
    }
}

#[cfg(windows)]
fn take_lock(path: &Path) -> Result<File, LockError> {
    use std::os::windows::fs::OpenOptionsExt;
    const SHARING_VIOLATION: i32 = 32;
    const LOCK_VIOLATION: i32 = 33;
    match fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(path)
    {
        Ok(f) => Ok(f),
        Err(e) if matches!(e.raw_os_error(), Some(SHARING_VIOLATION | LOCK_VIOLATION)) => {
            Err(LockError::Busy)
        }
        Err(e) => Err(LockError::Io(e)),
    }
}

#[cfg(not(windows))]
fn take_lock(path: &Path) -> Result<File, LockError> {
    use std::io::Write;
    for _ in 0..2 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                let _ = write!(f, "{}", crate::process_id());
                return Ok(f);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(path).unwrap_or_default();
                if pid_alive(pid.trim()) {
                    return Err(LockError::Busy);
                }
                let _ = fs::remove_file(path);
            }
            Err(e) => return Err(LockError::Io(e)),
        }
    }
    Err(LockError::Busy)
}

#[cfg(not(windows))]
fn pid_alive(pid: &str) -> bool {
    !pid.is_empty()
        && std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-project-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn go(_: u64, _: u64) -> Result<(), Error> {
        Ok(())
    }

    fn levels(m: &Model) -> i64 {
        m.sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap()
    }

    fn working(p: &Path, n: usize) -> Model {
        let mut m = Model::open_working(p.to_str().unwrap()).unwrap();
        for k in 0..n {
            m.add_level(&format!("L{k}"), k as f64 * 3.0).unwrap();
        }
        m
    }

    fn no_temp(d: &Path) {
        let left: Vec<String> = fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn round_trip_gives_one_portable_file_and_never_edits_it() {
        let d = dir("rt");
        let (work, dst) = (d.join("work.ddc"), d.join("p.ddc"));
        let m = working(&work, 2);
        save_project_file(&work, &dst, &mut go).unwrap();
        assert!(!side(&dst, "-wal").exists() && !side(&dst, "-shm").exists());
        no_temp(&d);
        let bytes = fs::read(&dst).unwrap();
        let w2 = d.join("work2.ddc");
        let mut o = open_project_copy(&dst, &w2, true, &mut go).unwrap();
        assert_eq!(levels(&o.model), 2);
        assert!(o.upgraded_from.is_none() && o.backup.is_none());
        o.model.add_level("edit", 9.0).unwrap();
        assert_eq!(
            fs::read(&dst).unwrap(),
            bytes,
            "edits stay in the working file"
        );
        // a save replaces the file with the working state, journal included
        save_project_file(&w2, &dst, &mut go).unwrap();
        let w3 = d.join("work3.ddc");
        let o3 = open_project_copy(&dst, &w3, true, &mut go).unwrap();
        assert_eq!(levels(&o3.model), 3);
        drop((m, o, o3));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn memory_model_saves_on_its_own_connection() {
        let d = dir("mem");
        let dst = d.join("m.ddc");
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 0.0).unwrap();
        m.save_project_to(&dst, &mut go).unwrap();
        let o = open_project_copy(&dst, &d.join("w.ddc"), true, &mut go).unwrap();
        assert_eq!(levels(&o.model), 1);
        drop(o);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn failed_save_leaves_the_old_file_intact_and_no_temp() {
        let d = dir("fail");
        let (work, dst) = (d.join("work.ddc"), d.join("p.ddc"));
        let mut m = working(&work, 1);
        save_project_file(&work, &dst, &mut go).unwrap();
        let bytes = fs::read(&dst).unwrap();
        m.add_level("later", 3.0).unwrap();
        // the disk fills up in the middle of the copy
        let mut full = |_: u64, _: u64| -> Result<(), Error> {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no space left on device",
            )))
        };
        assert!(save_project_file(&work, &dst, &mut full).is_err());
        assert_eq!(fs::read(&dst).unwrap(), bytes);
        no_temp(&d);
        // the target cannot be replaced at all
        let occupied = d.join("folder.ddc");
        fs::create_dir_all(&occupied).unwrap();
        assert!(save_project_file(&work, &occupied, &mut go).is_err());
        assert!(occupied.is_dir());
        no_temp(&d);
        // and the model still saves afterwards
        save_project_file(&work, &dst, &mut go).unwrap();
        assert_ne!(fs::read(&dst).unwrap(), bytes);
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn save_copies_one_state_while_the_editor_keeps_writing() {
        let d = dir("live");
        let (work, dst) = (d.join("work.ddc"), d.join("p.ddc"));
        let m = working(&work, 3);
        let h = {
            let (work, dst) = (work.clone(), dst.clone());
            std::thread::spawn(move || {
                // slow copy, one step at a time
                let mut slow = |_: u64, _: u64| -> Result<(), Error> {
                    std::thread::sleep(Duration::from_millis(5));
                    Ok(())
                };
                save_project_file(&work, &dst, &mut slow)
            })
        };
        let mut m = m;
        for k in 0..20 {
            m.add_level(&format!("W{k}"), 100.0 + k as f64).unwrap();
        }
        h.join().unwrap().unwrap();
        let o = open_project_copy(&dst, &d.join("w2.ddc"), true, &mut go).unwrap();
        assert!(levels(&o.model) >= 3, "a whole state, never a torn one");
        drop((m, o));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_successful_save_clears_what_a_crash_left_beside_the_file() {
        let d = dir("stale");
        let (work, dst) = (d.join("work.ddc"), d.join("p.ddc"));
        let m = working(&work, 1);
        save_project_file(&work, &dst, &mut go).unwrap();
        let (dead, dead_log) = (
            d.join("p.ddc.tmp-99-0-1"),
            d.join("p.ddc.tmp-99-0-1-journal"),
        );
        fs::write(&dead, b"half a copy").unwrap();
        fs::write(&dead_log, b"log").unwrap();
        // a backup and another project are not this file's temp
        let (bak, other) = (d.join("p.ddc.bak-v3"), d.join("p2.ddc.tmp-99-0-1"));
        fs::write(&bak, b"backup").unwrap();
        fs::write(&other, b"other").unwrap();
        // a failed save takes nothing away
        let occupied = d.join("folder.ddc");
        fs::create_dir_all(&occupied).unwrap();
        assert!(save_project_file(&work, &occupied, &mut go).is_err());
        assert!(dead.exists());
        save_project_file(&work, &dst, &mut go).unwrap();
        assert!(!dead.exists() && !dead_log.exists());
        assert!(bak.exists() && other.exists());
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    #[cfg(windows)]
    #[test]
    fn rename_waits_for_a_scanner_longer_than_two_seconds() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir("scan");
        let (work, dst) = (d.join("work.ddc"), d.join("p.ddc"));
        let m = working(&work, 1);
        save_project_file(&work, &dst, &mut go).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dst)
            .unwrap();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2500));
            drop(held);
        });
        save_project_file(&work, &dst, &mut go).unwrap();
        h.join().unwrap();
        // Cancel gets through while it waits, and the temp goes
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dst)
            .unwrap();
        let mut stop = |d: u64, t: u64| -> Result<(), Error> {
            if d == 1 && t == 1 {
                return Err(Error::Invalid("cancelled".into()));
            }
            Ok(())
        };
        assert!(save_project_file(&work, &dst, &mut stop).is_err());
        drop(held);
        no_temp(&d);
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn second_instance_gets_busy_and_a_released_lock_leaves_nothing() {
        let d = dir("lock");
        let p = d.join("p.ddc");
        let a = ProjectLock::acquire(&p).unwrap();
        assert!(matches!(ProjectLock::acquire(&p), Err(LockError::Busy)));
        drop(a);
        assert!(!side(&p, ".lock").exists());
        drop(ProjectLock::acquire(&p).unwrap());
        // no such folder: not «busy», the caller offers read-only
        let none = d.join("missing").join("p.ddc");
        assert!(matches!(ProjectLock::acquire(&none), Err(LockError::Io(_))));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn newer_file_is_refused_and_untouched() {
        let d = dir("new");
        let p = d.join("n.ddc");
        {
            let m = Model::open(p.to_str().unwrap()).unwrap();
            m.sql()
                .execute(
                    "UPDATE meta SET value=?1 WHERE key='schema_version'",
                    [schema::VERSION + 1],
                )
                .unwrap();
        }
        let bytes = fs::read(&p).unwrap();
        let w = d.join("w.ddc");
        match open_project_copy(&p, &w, true, &mut go) {
            Err(ProjectError::Newer { found, supported }) => {
                assert_eq!((found, supported), (schema::VERSION + 1, schema::VERSION));
            }
            Err(e) => panic!("wrong error {e}"),
            Ok(_) => panic!("newer file opened"),
        }
        assert_eq!(fs::read(&p).unwrap(), bytes);
        assert!(!w.exists());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn older_file_upgrades_on_the_copy_with_a_backup() {
        let d = dir("old");
        let p = d.join("o.ddc");
        let old = schema::VERSION - 1;
        {
            let mut m = Model::open(p.to_str().unwrap()).unwrap();
            m.add_level("L1", 0.0).unwrap();
            m.sql()
                .execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [old])
                .unwrap();
        }
        let w = d.join("w.ddc");
        let o = open_project_copy(&p, &w, true, &mut go).unwrap();
        assert_eq!(o.model.schema_version().unwrap(), schema::VERSION);
        assert_eq!(levels(&o.model), 1);
        assert_eq!(o.upgraded_from, Some(old));
        let bak = o.backup.clone().unwrap();
        assert!(bak.exists());
        assert_eq!(
            probe_project(&p).unwrap(),
            old,
            "the file itself stays as it was"
        );
        assert_eq!(probe_project(&bak).unwrap(), old);
        assert!(
            !d.join(format!("w.ddc.bak-v{old}")).exists(),
            "no second copy beside the working file"
        );
        // the first save brings the file up to date
        save_project_file(&w, &p, &mut go).unwrap();
        assert_eq!(probe_project(&p).unwrap(), schema::VERSION);
        assert_eq!(probe_project(&bak).unwrap(), old);
        // read-only: nothing beside the file, it is never replaced from here
        let ro = d.join("r.ddc");
        fs::copy(&bak, &ro).unwrap();
        let r = open_project_copy(&ro, &d.join("w2.ddc"), false, &mut go).unwrap();
        assert!(r.backup.is_none() && r.upgraded_from == Some(old));
        assert!(!d.join(format!("r.ddc.bak-v{old}")).exists());
        drop((o, r));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn another_file_at_the_same_path_gets_its_own_backup() {
        let d = dir("bak2");
        let p = d.join("o.ddc");
        let old = schema::VERSION - 1;
        let make = |name: &str| {
            let mut m = Model::open(p.to_str().unwrap()).unwrap();
            m.add_level(name, 0.0).unwrap();
            m.sql()
                .execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [old])
                .unwrap();
        };
        make("first");
        let first = open_project_copy(&p, &d.join("w1.ddc"), true, &mut go).unwrap();
        let bak = first.backup.clone().unwrap();
        let bak_bytes = fs::read(&bak).unwrap();
        // nothing changed: the same copy, no new one
        let again = open_project_copy(&p, &d.join("w2.ddc"), true, &mut go).unwrap();
        assert_eq!(again.backup.as_ref(), Some(&bak));
        drop((first, again));
        // another file arrives at this path: its content must not go without a copy
        fs::remove_file(&p).unwrap();
        make("second");
        fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(web_time::SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        let o = open_project_copy(&p, &d.join("w3.ddc"), true, &mut go).unwrap();
        let bak2 = o.backup.clone().unwrap();
        assert_ne!(bak2, bak);
        assert_eq!(fs::read(&bak).unwrap(), bak_bytes, "the older copy stays");
        let copy = Connection::open_with_flags(&bak2, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let n: i64 = copy
            .query_row("SELECT count(*) FROM level WHERE name='second'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1, "the toast names a copy of this file");
        drop((o, copy));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_checked_snapshot_tells_good_from_broken() {
        let d = dir("chk");
        let work = d.join("w.ddc");
        let m = working(&work, 1);
        let snap = autosave_file(&work, &d.join("n.ddc"), 3).unwrap();
        assert!(crate::check_snapshot(&snap).is_ok());
        fs::write(&snap, vec![0x5Au8; 8192]).unwrap();
        assert!(crate::check_snapshot(&snap).is_err());
        drop(m);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn not_a_project_and_damaged_are_told_apart() {
        let d = dir("bad");
        let empty = d.join("e.ddc");
        fs::write(&empty, b"").unwrap();
        assert!(matches!(
            probe_project(&empty),
            Err(ProjectError::NotProject(_))
        ));
        let junk = d.join("j.ddc");
        fs::write(&junk, vec![0x5Au8; 8192]).unwrap();
        assert!(matches!(
            probe_project(&junk),
            Err(ProjectError::Damaged(_))
        ));
        let gone = d.join("gone.ddc");
        assert!(matches!(probe_project(&gone), Err(ProjectError::Model(_))));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn autosave_of_a_working_file_runs_on_a_second_connection() {
        let d = dir("auto");
        let work = d.join("w.ddc");
        let m = working(&work, 2);
        let virt = d.join("name.ddc");
        let snap = autosave_file(&work, &virt, 3).unwrap();
        assert_eq!(crate::snapshots_of(&virt), vec![snap]);
        assert!(autosave_file(&d.join("gone.ddc"), &virt, 3).is_err());
        drop(m);
        let _ = fs::remove_dir_all(d);
    }
}
