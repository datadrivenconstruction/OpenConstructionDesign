// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! File safety: integrity check on open, backup before migration, atomic copy writes
//! (temp + fsync + rename), autosave snapshots and recovery.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use web_time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use crate::{Error, Model};

/// How many autosave snapshots to keep next to the file.
pub const KEEP_SNAPSHOTS: usize = 3;

/// `PRAGMA quick_check` (on open) or `integrity_check`: "ok" or the text of the first problems.
pub(crate) fn integrity(db: &Connection) -> Result<(), Error> {
    check(db, "integrity_check")
}

/// Fast check of pages and records without index cross-checking: opening 1M elements takes < 3 s.
pub(crate) fn quick(db: &Connection) -> Result<(), Error> {
    check(db, "quick_check")
}

fn check(db: &Connection, pragma: &str) -> Result<(), Error> {
    let mut st = db.prepare(&format!("PRAGMA {pragma}(5)"))?;
    let rows: Vec<String> = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(Error::Corrupt(rows.join("; ")))
    }
}

/// Consistent copy of the database into `dst` via a sibling temp file, fsync and rename.
pub(crate) fn write_copy(db: &Connection, dst: &Path) -> Result<(), Error> {
    let tmp = tmp_path(dst);
    let s = tmp
        .to_str()
        .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
    let res = (|| -> Result<(), Error> {
        db.execute("VACUUM INTO ?1", [s])?;
        fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
        fs::rename(&tmp, dst)?;
        if let Some(dir) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
            // directory: cannot be opened as a file on Windows, so skip fsync there
            if let Ok(d) = File::open(dir) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// The name is unique per call (process, counter, time): a foreign temp file is never touched or removed.
pub(crate) fn tmp_path(dst: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut s = dst.as_os_str().to_owned();
    s.push(format!(
        ".tmp-{}-{}-{ns}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(s)
}

/// Backup before migrating an old file in place. Same rules as `project_backup`: a copy of
/// another file at this path is never taken for this file's backup; a failed write is an error,
/// so the caller must not migrate.
pub(crate) fn backup_before_migration(db: &Connection, path: &str, old: i64) -> Result<(), Error> {
    project_backup(db, Path::new(path), old).map(|_| ())
}

/// Copy of project `src` before its first save replaces it with a newer format. An existing
/// `<src>.bak-v<old>*` is reused only when it carries the time of this very file: another file
/// at the same path gets a copy of its own under a unique name.
pub(crate) fn project_backup(db: &Connection, src: &Path, old: i64) -> Result<PathBuf, Error> {
    let stamp = fs::metadata(src)?.modified()?;
    let base = format!("{}.bak-v{old}", src.to_string_lossy());
    let plain = PathBuf::from(&base);
    let same_time = |p: &Path| {
        fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| {
            let gap = t.duration_since(stamp).unwrap_or_else(|e| e.duration());
            // some file systems keep times only to two seconds
            gap <= std::time::Duration::from_secs(2)
        })
    };
    let dir = src.parent().filter(|d| !d.as_os_str().is_empty());
    let name = plain
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let found = fs::read_dir(dir.unwrap_or(Path::new(".")))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.strip_prefix(name.as_str())
                .is_some_and(|r| r.is_empty() || r.starts_with('-'))
        })
        .map(|e| e.path())
        .find(|p| same_time(p));
    if let Some(p) = found {
        return Ok(p);
    }
    let target = if plain.exists() {
        let mut ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        while PathBuf::from(format!("{base}-{ms}")).exists() {
            ms += 1;
        }
        PathBuf::from(format!("{base}-{ms}"))
    } else {
        plain
    };
    write_copy(db, &target)?;
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&target) {
        // failing costs one more copy on the next open, nothing else
        let _ = f.set_modified(stamp);
    }
    Ok(target)
}

fn snap_prefix(path: &Path) -> Option<(PathBuf, String)> {
    let name = path.file_name()?.to_str()?;
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    Some((dir, format!("{name}.snap-")))
}

/// Snapshots of the file, newest first.
pub fn snapshots_of(path: &Path) -> Vec<PathBuf> {
    let Some((dir, pre)) = snap_prefix(path) else {
        return vec![];
    };
    let rd = if dir.as_os_str().is_empty() {
        fs::read_dir(".")
    } else {
        fs::read_dir(&dir)
    };
    let mut v: Vec<(u128, PathBuf)> = rd
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let ts = n.strip_prefix(&pre)?.parse::<u128>().ok()?;
            Some((ts, dir.join(n)))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|x| x.1).collect()
}

/// Snapshot `<file>.snap-<ms>` of database `db`; keeps the last `keep` (at least one).
pub(crate) fn autosave_db(db: &Connection, file: &Path, keep: usize) -> Result<PathBuf, Error> {
    let file = file.to_path_buf();
    if let Some(d) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(d)?;
    }
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let (dir, pre) = snap_prefix(&file).ok_or_else(|| Error::Invalid("bad model path".into()))?;
    let last = snapshots_of(&file).first().and_then(|p| {
        p.file_name()?
            .to_str()?
            .strip_prefix(&pre)?
            .parse::<u128>()
            .ok()
    });
    let ms = last.map_or(ms, |l| ms.max(l + 1));
    let dst = dir.join(format!("{pre}{ms}"));
    write_copy(db, &dst)?;
    for old in snapshots_of(&file).into_iter().skip(keep.max(1)) {
        let _ = fs::remove_file(old);
    }
    Ok(dst)
}

impl Model {
    /// Atomic consistent copy of the model (VACUUM INTO a temp file, fsync, rename).
    pub fn snapshot_to(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        write_copy(&self.db, path.as_ref())
    }

    /// Autosave snapshot `<file>.snap-<ms>` next to the model file; keeps the last
    /// [`KEEP_SNAPSHOTS`]. An error for an in-memory model.
    pub fn autosave(&self) -> Result<PathBuf, Error> {
        let file = self
            .db
            .path()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| Error::Invalid("in-memory model has no file".into()))?;
        self.autosave_as(Path::new(file), KEEP_SNAPSHOTS)
    }

    /// Snapshot `<file>.snap-<ms>` for any model (including in-memory) under the name `file`;
    /// keeps the last `keep` (at least one).
    pub fn autosave_as(&self, file: &Path, keep: usize) -> Result<PathBuf, Error> {
        autosave_db(&self.db, file, keep)
    }

    /// Newest snapshot of `path` that passes integrity_check; `None` if none is usable.
    pub fn recover_latest(path: impl AsRef<Path>) -> Result<Option<PathBuf>, Error> {
        for s in snapshots_of(path.as_ref()) {
            let Ok(db) = Connection::open_with_flags(&s, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
                continue;
            };
            if integrity(&db).is_ok() {
                return Ok(Some(s));
            }
        }
        Ok(None)
    }
}

/// Checks a snapshot before relying on it in place of the only copy.
pub fn check_snapshot(snap: &Path) -> Result<(), Error> {
    let db = Connection::open_with_flags(snap, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    integrity(&db)
}

/// Snapshot for the recovery dialog: time from the name, size, validity.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapInfo {
    pub path: PathBuf,
    /// Snapshot time, ms since the epoch (from the file name).
    pub ms: u128,
    pub bytes: u64,
    /// `None` = integrity_check passed; otherwise the error text (the snapshot is skipped).
    pub damage: Option<String>,
}

/// Snapshots of `file` with integrity checks, newest first.
pub fn snapshot_infos(file: &Path) -> Vec<SnapInfo> {
    let pre = snap_prefix(file).map(|p| p.1).unwrap_or_default();
    snapshots_of(file)
        .into_iter()
        .map(|path| {
            let ms = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix(&pre))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let damage = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            {
                Ok(db) => integrity(&db).err().map(|e| e.to_string()),
                Err(e) => Some(e.to_string()),
            };
            SnapInfo {
                path,
                ms,
                bytes,
                damage,
            }
        })
        .collect()
}

/// Moves a snapshot into a sibling `_archive/` (nothing is deleted); returns the new path.
pub fn archive_snapshot(snap: &Path) -> Result<PathBuf, Error> {
    let dir = snap.parent().unwrap_or(Path::new("")).join("_archive");
    fs::create_dir_all(&dir)?;
    let name = snap
        .file_name()
        .ok_or_else(|| Error::Invalid("bad snapshot path".into()))?;
    let dst = dir.join(name);
    fs::rename(snap, &dst)?;
    Ok(dst)
}

/// Table changes between the model and a snapshot (rows by primary key `id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDiff {
    pub table: String,
    /// Present in the snapshot, absent from the model.
    pub added: u64,
    /// Present in the model, absent from the snapshot.
    pub removed: u64,
    /// Present in both, values differ.
    pub changed: u64,
}

impl Model {
    /// Working copy of a snapshot: integrity check (read-only), copy into `work`, open.
    /// The snapshot itself is not modified.
    pub fn open_snapshot(snap: &Path, work: &Path) -> Result<Model, Error> {
        let db = Connection::open_with_flags(snap, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        integrity(&db)?;
        if let Some(d) = work.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(d)?;
        }
        write_copy(&db, work)?;
        drop(db);
        let w = work
            .to_str()
            .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
        Model::open(w)
    }

    /// Counts of changed rows per table that has an `id` column (non-zero only); the derived
    /// mesh cache is not compared.
    pub fn compare_snapshot(&self, snap: &Path) -> Result<Vec<TableDiff>, Error> {
        let probe = Connection::open_with_flags(snap, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        integrity(&probe)?;
        drop(probe);
        crate::compare::table_counts(&self.db, snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed past schema version, below `schema::VERSION`; independent of later bumps.
    const OLD: i64 = 13;

    #[test]
    fn old_fixture_version_is_below_current() {
        assert!(OLD < crate::schema::VERSION as i64);
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-file-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migration_writes_backup_once() {
        let d = dir("bak");
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        {
            let mut m = Model::open(ps).unwrap();
            m.add_level("L1", 0.0).unwrap();
            m.sql()
                .execute(&format!("UPDATE meta SET value={OLD} WHERE key='schema_version'"), [])
                .unwrap();
        }
        let m = Model::open(ps).unwrap();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let bak = d.join(format!("m.ddc.bak-v{OLD}"));
        drop(m);
        let b = Connection::open(&bak).unwrap();
        let n: i64 = b
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(d);
    }

    /// Old-format file (version OLD) with one level, closed.
    fn make_old(p: &Path, level: &str) {
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        m.add_level(level, 0.0).unwrap();
        m.sql()
            .execute(&format!("UPDATE meta SET value={OLD} WHERE key='schema_version'"), [])
            .unwrap();
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn baks(d: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&format!("m.ddc.bak-v{OLD}")))
            })
            .collect();
        v.sort();
        v
    }

    fn level_names(p: &Path) -> Vec<String> {
        // plain connection: opening a backup as a model would migrate it and write a nested backup
        let c = Connection::open(p).unwrap();
        let mut st = c.prepare("SELECT name FROM level").unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect()
    }

    #[test]
    fn migrate_second_old_file_same_path_gets_own_backup() {
        let d = dir("bak2");
        let p = d.join("m.ddc");
        let ps = p.to_str().unwrap();
        make_old(&p, "First");
        drop(Model::open(ps).unwrap());
        assert_eq!(baks(&d).len(), 1);
        // a different old file restored at the same path, with a different time
        fs::remove_file(&p).unwrap();
        make_old(&p, "Second");
        set_mtime(&p, SystemTime::now() + std::time::Duration::from_secs(3600));
        drop(Model::open(ps).unwrap());
        let b = baks(&d);
        assert_eq!(b.len(), 2);
        let mut seen: Vec<String> = b.iter().flat_map(|x| level_names(x)).collect();
        seen.sort();
        assert_eq!(seen, ["First", "Second"]);
        // reading the backups must not have produced nested backups
        assert_eq!(baks(&d).len(), 2);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn migrate_same_file_reopen_reuses_backup() {
        let d = dir("bak3");
        let p = d.join("m.ddc");
        make_old(&p, "L");
        let db = Connection::open(&p).unwrap();
        backup_before_migration(&db, p.to_str().unwrap(), OLD).unwrap();
        backup_before_migration(&db, p.to_str().unwrap(), OLD).unwrap();
        assert_eq!(baks(&d).len(), 1);
        drop(db);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn migrate_refused_when_backup_fails() {
        let d = dir("bak4");
        let p = d.join("m.ddc");
        make_old(&p, "L");
        let mut db = Connection::open(&p).unwrap();
        // the path given for the backup cannot be stamped or written
        let bogus = d.join("missing").join("m.ddc");
        let r = crate::schema::upgrade(&mut db, Some(bogus.to_str().unwrap()));
        assert!(r.is_err());
        let v: i64 = db
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, OLD);
        assert!(baks(&d).is_empty());
        drop(db);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn garbage_file_is_corrupt_error() {
        let d = dir("bad");
        let p = d.join("x.ddc");
        fs::write(&p, vec![0x5Au8; 8192]).unwrap();
        match Model::open(p.to_str().unwrap()) {
            Err(Error::Corrupt(_)) => {}
            Err(e) => panic!("wrong error {e}"),
            Ok(_) => panic!("garbage opened"),
        }
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn autosave_keeps_last_and_recovers() {
        let d = dir("snap");
        let p = d.join("a.ddc");
        let mut m = Model::open(p.to_str().unwrap()).unwrap();
        let mut last = PathBuf::new();
        for k in 0..5 {
            m.add_level(&format!("L{k}"), k as f64).unwrap();
            last = m.autosave().unwrap();
        }
        assert_eq!(snapshots_of(&p).len(), KEEP_SNAPSHOTS);
        // newest snapshot damaged -> previous one is chosen
        fs::write(&last, b"broken").unwrap();
        let r = Model::recover_latest(&p).unwrap().unwrap();
        assert_ne!(r, last);
        let rm = Model::open(r.to_str().unwrap()).unwrap();
        let n: i64 = rm
            .sql()
            .query_row("SELECT count(*) FROM level", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4);
        let s = d.join("copy.ddc");
        m.snapshot_to(&s).unwrap();
        assert!(Model::open(s.to_str().unwrap()).is_ok());
        assert!(Model::open(":memory:").unwrap().autosave().is_err());
        drop((m, rm));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn in_memory_autosave_restore_compare_archive() {
        let d = dir("mem");
        let virt = d.join("untitled.ddc");
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("A", 0.0).unwrap();
        for _ in 0..4 {
            m.autosave_as(&virt, 2).unwrap();
        }
        let infos = snapshot_infos(&virt);
        assert_eq!(infos.len(), 2);
        assert!(infos[0].ms > infos[1].ms && infos[0].bytes > 0);
        assert!(infos.iter().all(|i| i.damage.is_none()));
        // model moves on: one level more, one renamed
        m.add_level("B", 3.0).unwrap();
        m.sql()
            .execute("UPDATE level SET name='A2' WHERE name='A'", [])
            .unwrap();
        let diff = m.compare_snapshot(&infos[0].path).unwrap();
        let lv = diff
            .iter()
            .find(|t| t.table == "level")
            .expect("level diff");
        assert_eq!((lv.added, lv.removed, lv.changed), (0, 1, 1), "{diff:?}");
        let r = Model::open_snapshot(&infos[0].path, &d.join("work.ddc")).unwrap();
        let n: i64 = r
            .sql()
            .query_row("SELECT count(*) FROM level", [], |x| x.get(0))
            .unwrap();
        assert_eq!(n, 1);
        fs::write(&infos[1].path, b"junk").unwrap();
        let bad = snapshot_infos(&virt);
        assert!(bad[1].damage.is_some());
        assert!(Model::open_snapshot(&bad[1].path, &d.join("w2.ddc")).is_err());
        let moved = archive_snapshot(&bad[1].path).unwrap();
        assert!(moved.exists() && moved.parent().unwrap().ends_with("_archive"));
        assert_eq!(snapshots_of(&virt).len(), 1);
        drop((m, r));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn concurrent_autosave_of_one_file_never_collides() {
        let d = dir("conc");
        let p = d.join("c.ddc");
        let path = p.to_str().unwrap().to_string();
        drop(Model::open(&path).unwrap());
        let hs: Vec<_> = (0..2)
            .map(|_| Model::open(&path).unwrap())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|m| {
                std::thread::spawn(move || {
                    for _ in 0..15 {
                        m.autosave().unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let tmp = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(tmp, 0);
        assert!(Model::recover_latest(&p).unwrap().is_some());
        let _ = fs::remove_dir_all(d);
    }
}
