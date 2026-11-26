// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Per-job work copies in the temp folder, safe with several editor windows open.
//! Each process holds `ddc-collab-<pid>.lock` open for its whole life; a leftover from
//! another pid is touched only when that lock proves the owner gone, and then it is moved
//! under `_archive/ddc-collab`, never removed.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use web_time::{Duration, SystemTime};

const PREFIX: &str = "ddc-collab-";
/// A dead owner's copies are left alone this long (a crash report may still want them).
const DEAD_AGE: Duration = Duration::from_secs(60 * 60);
/// No lock file: an older build without locks may still be running, so wait much longer.
const UNKNOWN_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Liveness {
    Live,
    Dead,
    Unknown,
}

/// Work folder of one job. Copies live until exit: a merge redo reads them again.
pub(crate) fn work_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static SWEPT: Mutex<bool> = Mutex::new(false);
    let pid = std::process::id();
    let root = std::env::temp_dir();
    {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        if !*swept {
            *swept = true;
            hold_own_lock(&root, pid);
            let mut state = SweepState::default();
            sweep(&root, pid, SystemTime::now(), &mut state, &|p| lock_liveness(&root, p));
        }
    }
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    root.join(format!("{PREFIX}{pid}-{n}-{tag}"))
}

fn lock_path(root: &Path, pid: u32) -> PathBuf {
    root.join(format!("{PREFIX}{pid}.lock"))
}

/// Opens the lock without sharing so other processes see it as held until this one exits.
fn open_exclusive(path: &Path, create: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    if create {
        o.write(true).create(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0);
    }
    o.open(path)
}

fn hold_own_lock(root: &Path, pid: u32) {
    static OWN: OnceLock<Option<File>> = OnceLock::new();
    OWN.get_or_init(|| open_exclusive(&lock_path(root, pid), true).ok());
}

/// Whether the process that wrote `ddc-collab-<pid>.lock` is still running.
pub(crate) fn lock_liveness(root: &Path, pid: u32) -> Liveness {
    let path = lock_path(root, pid);
    if !path.exists() {
        return Liveness::Unknown;
    }
    #[cfg(windows)]
    {
        match open_exclusive(&path, false) {
            Ok(_) => Liveness::Dead,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Liveness::Unknown,
            // Sharing violation means the owner holds it; any other error: assume alive.
            Err(_) => Liveness::Live,
        }
    }
    #[cfg(not(windows))]
    {
        let proc = Path::new("/proc");
        if proc.is_dir() && !proc.join(pid.to_string()).exists() {
            Liveness::Dead
        } else {
            Liveness::Live
        }
    }
}

/// Owner pid of a `ddc-collab-<pid>-<n>-<tag>` folder or `ddc-collab-<pid>.lock` file.
fn owner_pid(name: &str) -> Option<(u32, bool)> {
    let rest = name.strip_prefix(PREFIX)?;
    if let Some(p) = rest.strip_suffix(".lock") {
        return p.parse().ok().map(|p| (p, true));
    }
    let (p, tail) = rest.split_once('-')?;
    if tail.is_empty() {
        return None;
    }
    p.parse().ok().map(|p| (p, false))
}

#[derive(Default)]
pub(crate) struct SweepState {
    pub moved: Vec<PathBuf>,
}

fn age(path: &Path, now: SystemTime) -> Duration {
    std::fs::metadata(path)
        .and_then(|m| ddc_model::wtime::modified(&m))
        .ok()
        .and_then(|t| now.duration_since(t).ok())
        .unwrap_or(Duration::ZERO)
}

/// Moves `path` into `root/_archive/ddc-collab`, picking a free name. Never deletes.
fn archive(root: &Path, path: &Path) -> Option<PathBuf> {
    let dir = root.join("_archive").join("ddc-collab");
    std::fs::create_dir_all(&dir).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let mut dest = dir.join(&name);
    let mut i = 1;
    while dest.exists() {
        dest = dir.join(format!("{name}.{i}"));
        i += 1;
    }
    std::fs::rename(path, &dest).ok()?;
    Some(dest)
}

/// Work folder of a cancelled job: an empty one is removed, one with copies goes to the
/// archive beside it. Returns where the copies went.
pub(crate) fn discard(path: &Path) -> Option<PathBuf> {
    if !path.exists() || std::fs::remove_dir(path).is_ok() {
        return None;
    }
    archive(path.parent()?, path)
}

/// One pass over `root`, run before this process creates its first job folder.
/// Live owners are never touched. Folders named with our own pid can only be left by an
/// earlier process that had the same pid: empty ones are removed, others archived.
pub(crate) fn sweep(
    root: &Path,
    own: u32,
    now: SystemTime,
    st: &mut SweepState,
    live: &dyn Fn(u32) -> Liveness,
) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let mut dead_locks = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((pid, is_lock)) = owner_pid(&name) else {
            continue;
        };
        let path = e.path();
        if pid == own {
            // `remove_dir` fails on a non-empty folder, so nothing with data is lost.
            if !is_lock && std::fs::remove_dir(&path).is_err() {
                if let Some(d) = archive(root, &path) {
                    st.moved.push(d);
                }
            }
            continue;
        }
        let min_age = match live(pid) {
            Liveness::Live => continue,
            Liveness::Dead => DEAD_AGE,
            Liveness::Unknown => UNKNOWN_AGE,
        };
        if is_lock {
            dead_locks.push((pid, path));
            continue;
        }
        if age(&path, now) < min_age {
            continue;
        }
        if let Some(d) = archive(root, &path) {
            st.moved.push(d);
        }
    }
    // A dead owner's lock goes only once none of its folders is left behind.
    for (pid, lock) in dead_locks {
        let pre = format!("{PREFIX}{pid}-");
        let left = std::fs::read_dir(root).map_or(true, |rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(&pre))
        });
        if !left {
            if let Some(d) = archive(root, &lock) {
                st.moved.push(d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("ddc-wd-test-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn job(root: &Path, pid: u32, n: u32, tag: &str) -> PathBuf {
        let d = root.join(format!("{PREFIX}{pid}-{n}-{tag}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("other.ddc"), b"copy").unwrap();
        d
    }

    // Far in the future, so every folder counts as old.
    fn late() -> SystemTime {
        SystemTime::now() + UNKNOWN_AGE + DEAD_AGE
    }

    #[test]
    fn work_dir_keeps_live_other_pid_dirs() {
        let root = scratch("live");
        let other = job(&root, 4242, 0, "merge");
        let mut st = SweepState::default();
        sweep(&root, 1, late(), &mut st, &|_| Liveness::Live);
        assert!(other.join("other.ddc").is_file());
        assert!(st.moved.is_empty());
    }

    #[test]
    fn work_dir_sweeps_dead_pid_dirs() {
        let root = scratch("dead");
        let old = job(&root, 4243, 0, "cmp");
        std::fs::write(lock_path(&root, 4243), b"").unwrap();
        let mut st = SweepState::default();
        sweep(&root, 1, late(), &mut st, &|_| Liveness::Dead);
        assert!(!old.exists());
        let kept = root.join("_archive").join("ddc-collab").join(old.file_name().unwrap());
        assert_eq!(std::fs::read(kept.join("other.ddc")).unwrap(), b"copy");
        assert!(!lock_path(&root, 4243).exists());
        assert_eq!(st.moved.len(), 2);
    }

    #[test]
    fn work_dir_keeps_young_dead_and_unlocked_dirs() {
        let root = scratch("young");
        let dead = job(&root, 4244, 0, "merge");
        let unknown = job(&root, 4245, 0, "merge");
        let now = SystemTime::now();
        let mut st = SweepState::default();
        sweep(&root, 1, now, &mut st, &|p| {
            if p == 4244 { Liveness::Dead } else { Liveness::Unknown }
        });
        assert!(dead.is_dir() && unknown.is_dir());
        // Day-old folder without a lock: an old build may still use it.
        sweep(&root, 1, now + DEAD_AGE * 24, &mut st, &|_| Liveness::Unknown);
        assert!(unknown.is_dir());
        assert!(st.moved.is_empty());
    }

    #[test]
    fn work_dir_own_pid_leftovers_reused_or_cleaned() {
        let root = scratch("own");
        let empty = root.join(format!("{PREFIX}7-0-merge"));
        std::fs::create_dir_all(&empty).unwrap();
        let full = job(&root, 7, 1, "cmp");
        let mut st = SweepState::default();
        sweep(&root, 7, SystemTime::now(), &mut st, &|_| Liveness::Live);
        assert!(!empty.exists(), "empty own folder removed so the name is free");
        assert!(!full.exists(), "non-empty one moved aside, not deleted");
        let kept = root.join("_archive").join("ddc-collab").join(full.file_name().unwrap());
        assert!(kept.join("other.ddc").is_file());
    }

    #[test]
    fn merge_redo_after_second_process_sweep() {
        let root = scratch("redo");
        // Our own pid: alive by lock on Windows and by /proc elsewhere.
        let first = std::process::id();
        let copy = job(&root, first, 0, "merge").join("other.ddc");
        let lock = open_exclusive(&lock_path(&root, first), true).unwrap();
        // Second window starts its first job: the real liveness check must keep our copy.
        let mut st = SweepState::default();
        sweep(&root, first.wrapping_add(1), late(), &mut st, &|p| lock_liveness(&root, p));
        assert_eq!(std::fs::read(&copy).unwrap(), b"copy");
        drop(lock);
        #[cfg(windows)]
        assert_eq!(lock_liveness(&root, first), Liveness::Dead);
        assert_eq!(lock_liveness(&root, 999_999), Liveness::Unknown);
    }

    #[test]
    fn owner_pid_parses_only_own_names() {
        assert_eq!(owner_pid("ddc-collab-12-0-merge"), Some((12, false)));
        assert_eq!(owner_pid("ddc-collab-12.lock"), Some((12, true)));
        assert_eq!(owner_pid("ddc-collab-x-0-merge"), None);
        assert_eq!(owner_pid("ddc-collab-12"), None);
        assert_eq!(owner_pid("other"), None);
    }
}
