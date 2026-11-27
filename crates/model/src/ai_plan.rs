// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Dry run of an AI plan (docs/design/AI_ASSISTANT.md §5): file copies `base.db`/`work.db`,
//! row-by-row comparison via ATTACH + EXCEPT, rollback of the working copy, applying as a single
//! journal entry, stale-plan detection, new integrity violations, materials per level.
//!
//! The live model is open in WAL mode: the file copy is made by online backup without blocking UI commits.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;
use web_time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::{Changes, ElKey, Error, Issue, Model};

/// Cache tables: changing them does not make a plan stale. `clash_item` is not listed: it holds
/// user statuses and comments, and a plan that deletes an element removes them together with the pair.
pub const PLAN_IGNORED: &[&str] = &["mesh_proto", "mesh_geo", "comp_thumb"];

/// Pages per copy step: short steps give frequent progress updates.
const BACKUP_PAGES: i32 = 2048;
/// How long to keep waiting on a busy file before giving up.
const BACKUP_BUSY: Duration = Duration::from_secs(5);
/// Steps without progress after which the copy fails: commits from other connections restart it.
const BACKUP_RESTARTS: u32 = 8;
const COPY_CHUNK: usize = 4 << 20;
/// Lock file of the copies folder: the OS releases it even if the process crashes.
const LOCK_FILE: &str = "lock.db";
/// Abandoned folders younger than this are left alone: the owner may not have taken the lock yet.
const ABANDONED_AGE: Duration = Duration::from_secs(3600);
const ALIAS: &str = "ai_base";

/// How a row changed relative to the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Created,
    Changed,
    Deleted,
}

/// Column value before and after; NULL is `None`, a blob is `"blob(n)"` (with a hash when lengths are equal).
/// In a changed `meta` row the column is the setting name.
#[derive(Clone, Debug, PartialEq)]
pub struct CellDiff {
    pub col: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A table row; `owner` is the element it belongs to (table name or `kind`, id).
/// A changed row holds only the differing columns; the rowid column is not part of `cells`.
#[derive(Clone, Debug, PartialEq)]
pub struct RowDiff {
    pub table: String,
    pub rowid: i64,
    pub kind: RowKind,
    pub owner: Option<ElKey>,
    pub cells: Vec<CellDiff>,
}

impl RowDiff {
    /// A consequence of an edit rather than the edit itself (clash pairs of a deleted element).
    pub fn secondary(&self) -> bool {
        self.table == "clash_item"
    }
}

/// Differences from the base: element tables first, then the rest; within a table, by rowid.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelDiff {
    pub rows: Vec<RowDiff>,
    pub truncated: bool,
}

/// Material on a level. `area` is the layer area (plan footprint for a wall, surface for a rod).
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialQty {
    pub material: String,
    pub level: i64,
    pub volume: f64,
    pub area: f64,
}

static NEXT_COPY: AtomicU64 = AtomicU64::new(0);

/// Dry-run folder `<temp>/ddc-ai-<pid>-<session>-<n>/`; deleted together with the value,
/// and an abandoned one (crash, busy file) is removed by the next `new` of any process.
#[derive(Debug)]
pub struct PlanCopies {
    pub dir: PathBuf,
    pub base: PathBuf,
    pub work: PathBuf,
    /// Mutex only keeps the type `Sync`; the connection is never used after `new`.
    lock: Mutex<Option<Connection>>,
}

impl PlanCopies {
    pub fn new(session: &str) -> Result<Self, Error> {
        let sid: String = session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let root = crate::temp_dir();
        sweep_abandoned(&root, ABANDONED_AGE);
        let n = NEXT_COPY.fetch_add(1, AtomicOrdering::Relaxed);
        let dir = root.join(format!("ddc-ai-{}-{sid}-{n}", crate::process_id()));
        fs::create_dir_all(&dir)?;
        let lock = hold_lock(&dir)?;
        Ok(Self {
            base: dir.join("base.db"),
            work: dir.join("work.db"),
            dir,
            lock: Mutex::new(Some(lock)),
        })
    }

    /// Copies the project file in chunks plus a working copy; free space is checked beforehand. The model on `work`
    /// must be closed. The `progress` scale is shared: half for the base, half for the working copy.
    pub fn prepare(
        &self,
        source: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        let size = |p: &Path| fs::metadata(p).map_or(0, |m| m.len());
        reserve(
            &self.dir,
            2 * (size(source) + size(&sidecar(source, "-wal"))),
        )?;
        let mut pages = 0u64;
        Model::backup_file(source, &self.base, &mut |d: u64, t: u64| {
            pages = t;
            progress(d, t * 2)
        })?;
        copy_file(&self.base, &self.work, &mut |d: u64, t: u64| {
            let part = if t == 0 {
                pages
            } else {
                (u128::from(d) * u128::from(pages) / u128::from(t)) as u64
            };
            progress(pages + part, pages * 2)
        })
    }

    /// A ready in-memory project snapshot (`Model::snapshot_to`) becomes the base.
    pub fn adopt(&self, snapshot: &Path) -> Result<(), Error> {
        remove_sidecars(&self.base)?;
        if fs::rename(snapshot, &self.base).is_err() {
            copy_file(snapshot, &self.base, &mut keep_going)?;
        }
        self.reset_work()
    }

    /// Recreates the working copy from the base. The model on `work` must be closed (`drop`): the old `-wal`
    /// would corrupt the new copy, so failing to delete it is an error and the previous copy stays intact.
    pub fn reset_work(&self) -> Result<(), Error> {
        copy_file(&self.base, &self.work, &mut keep_going)
    }

    /// The working copy becomes the base (the plan is already applied). Connections to the base must be closed
    /// and `work` must have no other connections: a busy journal is an error and the base stays as it was.
    pub fn promote_work(&self, work: &Model) -> Result<(), Error> {
        // a busy log is a result row, not an error: the file would miss the last frames
        let busy: i64 = work
            .db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        if busy != 0 {
            return Err(Error::Invalid(
                "plan copy: the log of the work copy is in use".into(),
            ));
        }
        copy_file(&self.work, &self.base, &mut keep_going)
    }
}

impl Drop for PlanCopies {
    fn drop(&mut self) {
        // an open lock file would keep the folder on Windows
        if let Ok(lock) = self.lock.get_mut() {
            *lock = None;
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Where a session's two plan copies live: files on desktop (`PlanCopies`), memory in the
/// browser build (`MemCopies`). `base`/`work` open with `Model::open_copy` and ATTACH alike.
pub trait CopyStore: Send + Sync + std::fmt::Debug {
    fn base(&self) -> &Path;
    fn work(&self) -> &Path;
    /// The folder for further scratch files (options, agents' dry runs); `None` in memory.
    fn dir(&self) -> Option<&Path>;
    fn prepare(
        &self,
        source: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error>;
    fn adopt(&self, snapshot: &Path) -> Result<(), Error>;
    /// The bytes of a model file (`Model::to_bytes`) become the base, the work copy follows.
    fn adopt_bytes(&self, bytes: &[u8]) -> Result<(), Error>;
    fn reset_work(&self) -> Result<(), Error>;
    fn promote_work(&self, work: &Model) -> Result<(), Error>;
}

impl CopyStore for PlanCopies {
    fn base(&self) -> &Path {
        &self.base
    }
    fn work(&self) -> &Path {
        &self.work
    }
    fn dir(&self) -> Option<&Path> {
        Some(&self.dir)
    }
    fn prepare(
        &self,
        source: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        PlanCopies::prepare(self, source, progress)
    }
    fn adopt(&self, snapshot: &Path) -> Result<(), Error> {
        PlanCopies::adopt(self, snapshot)
    }
    fn adopt_bytes(&self, bytes: &[u8]) -> Result<(), Error> {
        let from = conn_of_bytes(bytes)?;
        remove_sidecars(&self.base)?;
        let tmp = sidecar(&self.base, ".part");
        let _ = fs::remove_file(&tmp);
        match backup_pages(&from, &tmp, &mut keep_going) {
            Ok(_) => install(&tmp, &self.base)?,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
        PlanCopies::reset_work(self)
    }
    fn reset_work(&self) -> Result<(), Error> {
        PlanCopies::reset_work(self)
    }
    fn promote_work(&self, work: &Model) -> Result<(), Error> {
        PlanCopies::promote_work(self, work)
    }
}

/// The copies as shared in-memory databases (SQLite `memdb`): no file is ever written. Their
/// names are URIs that `Model::open_copy`, ATTACH and the backup API open like paths; a copy
/// lives while its keeper connection is open, so both go when the value drops.
#[derive(Debug)]
pub struct MemCopies {
    base: PathBuf,
    work: PathBuf,
    keep: Mutex<Vec<Connection>>,
}

impl MemCopies {
    pub fn new(session: &str) -> Result<Self, Error> {
        let sid: String = session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let n = NEXT_COPY.fetch_add(1, AtomicOrdering::Relaxed);
        let name = |f: &str| PathBuf::from(format!("file:/ddc-ai-{sid}-{n}/{f}.db?vfs=memdb"));
        let (base, work) = (name("base"), name("work"));
        let keep = vec![Connection::open(&base)?, Connection::open(&work)?];
        Ok(Self {
            base,
            work,
            keep: Mutex::new(keep),
        })
    }

    fn fill_base(
        &self,
        from: &Connection,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        if !backup_pages(from, &self.base, progress)? {
            return Err(Error::Invalid("model copy cancelled".into()));
        }
        Ok(())
    }
}

/// A memory copy's name (`MemCopies`): it opens like a file but is none.
pub fn is_memory_copy(p: &Path) -> bool {
    p.to_str()
        .is_some_and(|s| s.starts_with("file:/") && s.ends_with("?vfs=memdb"))
}

impl CopyStore for MemCopies {
    fn base(&self) -> &Path {
        &self.base
    }
    fn work(&self) -> &Path {
        &self.work
    }
    fn dir(&self) -> Option<&Path> {
        None
    }
    fn prepare(
        &self,
        source: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        if !source.is_file() {
            return Err(Error::Invalid(format!("no model file: {}", source.display())));
        }
        let from = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        hold_snapshot(&from)?;
        self.fill_base(&from, progress)?;
        drop(from);
        self.reset_work()
    }
    fn adopt(&self, snapshot: &Path) -> Result<(), Error> {
        let from = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        self.fill_base(&from, &mut keep_going)?;
        drop(from);
        // as the file copies take the snapshot over: it never outlives the adoption
        let _ = fs::remove_file(snapshot);
        self.reset_work()
    }
    fn adopt_bytes(&self, bytes: &[u8]) -> Result<(), Error> {
        self.fill_base(&conn_of_bytes(bytes)?, &mut keep_going)?;
        self.reset_work()
    }
    fn reset_work(&self) -> Result<(), Error> {
        let from = Connection::open(&self.base)?;
        if !backup_pages(&from, &self.work, &mut keep_going)? {
            return Err(Error::Invalid("model copy cancelled".into()));
        }
        Ok(())
    }
    fn promote_work(&self, work: &Model) -> Result<(), Error> {
        self.fill_base(&work.db, &mut keep_going)
    }
}

impl Drop for MemCopies {
    fn drop(&mut self) {
        if let Ok(k) = self.keep.get_mut() {
            k.clear();
        }
    }
}

/// An in-memory connection holding the bytes of a model file (a WAL header is read as rollback).
fn conn_of_bytes(bytes: &[u8]) -> Result<Connection, Error> {
    if !bytes.starts_with(b"SQLite format 3\0") || bytes.len() < 100 {
        return Err(Error::Corrupt("not a model file".into()));
    }
    let mut buf = bytes.to_vec();
    for b in &mut buf[18..20] {
        if *b == 2 {
            *b = 1;
        }
    }
    let mut db = Connection::open_in_memory()?;
    db.deserialize_read_exact(rusqlite::MAIN_DB, &buf[..], buf.len(), false)
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    Ok(db)
}

fn keep_going(_: u64, _: u64) -> bool {
    true
}

/// Lock on the copies folder until the connection closes or the process ends.
fn hold_lock(dir: &Path) -> Result<Connection, Error> {
    let c = Connection::open(dir.join(LOCK_FILE))?;
    c.execute_batch("PRAGMA journal_mode=MEMORY; BEGIN EXCLUSIVE;")?;
    Ok(c)
}

fn owner_gone(lock: &Path) -> bool {
    let Ok(c) = Connection::open(lock) else {
        return false;
    };
    if c.busy_timeout(Duration::ZERO).is_err() {
        return false;
    }
    let free = c.execute_batch("BEGIN EXCLUSIVE").is_ok();
    if free {
        let _ = c.execute_batch("ROLLBACK");
    }
    free
}

/// Copy folders whose owner no longer holds the lock (process crashed). Without a lock file it is not ours.
fn sweep_abandoned(root: &Path, min_age: Duration) {
    let Ok(dirs) = fs::read_dir(root) else {
        return;
    };
    for e in dirs.flatten() {
        if !e.file_name().to_string_lossy().starts_with("ddc-ai-") {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() >= min_age);
        let dir = e.path();
        let lock = dir.join(LOCK_FILE);
        if old && lock.is_file() && owner_gone(&lock) {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// A full disk gives a clear error before copying: NTFS allocates space immediately (where the file is sparse,
/// the check promises nothing and the copy itself will report the error).
fn reserve(dir: &Path, bytes: u64) -> Result<(), Error> {
    let p = dir.join("reserve.tmp");
    let r = fs::File::create(&p).and_then(|f| f.set_len(bytes));
    let _ = fs::remove_file(&p);
    r.map_err(|e| Error::Invalid(format!("no temp space for {bytes} bytes: {e}")))
}

/// Chunked copy via `.part`: an interruption or cancel leaves `dst` unchanged.
fn copy_file(
    src: &Path,
    dst: &Path,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<(), Error> {
    let tmp = sidecar(dst, ".part");
    if let Err(e) = copy_chunks(src, &tmp, progress) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    install(&tmp, dst)
}

fn copy_chunks(
    src: &Path,
    tmp: &Path,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<(), Error> {
    let mut from = fs::File::open(src)?;
    let total = from.metadata()?.len();
    let mut to = fs::File::create(tmp)?;
    let mut buf = vec![0u8; COPY_CHUNK];
    let mut done = 0u64;
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
        done += n as u64;
        if !progress(done, total) {
            return Err(Error::Invalid("model copy cancelled".into()));
        }
    }
}

/// The finished `.part` takes the place of `dst`; the destination's old journal must not apply to the new file.
fn install(tmp: &Path, dst: &Path) -> Result<(), Error> {
    let done = remove_sidecars(dst).and_then(|()| Ok(fs::rename(tmp, dst)?));
    if done.is_err() {
        let _ = fs::remove_file(tmp);
    }
    done
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// A missing file is fine; any other failure (file busy) is an error.
pub(crate) fn remove_sidecars(path: &Path) -> Result<(), Error> {
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::remove_file(sidecar(path, suffix)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// FNV-1a 64 offset basis: the hash of no bytes.
pub const FNV1A_SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64: a short fingerprint (cards, keys, change detection), not for security.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_more(FNV1A_SEED, bytes)
}

/// FNV-1a 64 continued from `h`: feeding in pieces equals hashing them joined.
pub fn fnv1a_more(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

impl Model {
    /// Several operations as one journal entry and one file transaction: a process crash,
    /// an error or a panic in `f` leaves no part of the plan (inner operations are savepoints,
    /// `ops::scope`). No operations is an error: an undo step without an entry would undo someone else's operation.
    pub fn atomic<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.atomic_in(kind, None, f, |_, _| Ok(())).map(|(v, _)| v)
    }

    /// One undo step around `f`: an error or panic in `f` undoes every journal record it wrote;
    /// on success they merge into one (kind of the first). Returns whether a record was written:
    /// none means nothing to undo. Holds no transaction, so `f` may attach files or run `atomic`.
    pub fn step<T>(
        &mut self,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<(T, bool), Error> {
        let before = self.op_top()?;
        let v = match catch_unwind(AssertUnwindSafe(|| f(&mut *self))) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return match self.rollback_to(before) {
                    Ok(_) => Err(e),
                    Err(u) => Err(Error::Invalid(format!("{e}; rollback failed: {u}"))),
                };
            }
            Err(panic) => {
                let _ = self.rollback_to(before);
                resume_unwind(panic)
            }
        };
        let top = self.op_top()?;
        if top > before + 1 {
            let kind: String = self.db.query_row(
                "SELECT kind FROM op WHERE seq>?1 ORDER BY seq LIMIT 1",
                [before],
                |r| r.get(0),
            )?;
            crate::ops::merge_ops(&mut self.db, &kind, before)?;
        }
        Ok((v, top > before))
    }

    /// The one `atomic` body: `prov` is written onto the merged entry, then
    /// `seal(db, (seq, stamp))` writes a side row naming it; both commit or roll back with it.
    /// Returns the result and the entry's seq.
    pub(crate) fn atomic_in<T>(
        &mut self,
        kind: &str,
        prov: Option<&crate::journal::Provenance>,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
        seal: impl FnOnce(&Connection, (i64, i64)) -> Result<(), Error>,
    ) -> Result<(T, i64), Error> {
        if !self.db.is_autocommit() {
            return Err(Error::Invalid(
                "atomic: a transaction is already open".into(),
            ));
        }
        let _a = crate::perf::span("model.atomic");
        let before = self.op_top()?;
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let run = catch_unwind(AssertUnwindSafe(|| -> Result<(T, i64), Error> {
            let v = f(&mut *self)?;
            if self.op_top()? <= before {
                return Err(Error::Invalid("atomic: no operation recorded".into()));
            }
            crate::ops::merge_ops(&mut self.db, kind, before)?;
            let seq = match prov {
                Some(p) => crate::journal::stamp_top(&self.db, p)?,
                None => self.op_top()?,
            };
            seal(&self.db, self.ai_option_head()?)?;
            let current = self.regen.current_in_tx(&self.tracker);
            let _c = crate::perf::span("model.atomic.commit");
            self.db.execute_batch("COMMIT")?;
            if current {
                self.regen.promote_committed(self.change_head());
            }
            // a merge inside the step could not DETACH its source files while the step was open
            crate::merge::detach(&self.db);
            Ok((v, seq))
        }));
        match run {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                self.abandon_atomic();
                Err(e)
            }
            Err(panic) => {
                self.abandon_atomic();
                resume_unwind(panic)
            }
        }
    }

    /// ROLLBACK does not change `total_changes`, which is the cache key: anything built inside the plan must be forgotten.
    fn abandon_atomic(&mut self) {
        if !self.db.is_autocommit() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        crate::merge::detach(&self.db);
        crate::struct_ext::clear_stash();
        self.struct_cache = Default::default();
        self.memo = Default::default();
        // like `trial`: these may describe rows of the rolled-back ops
        self.regen = Default::default();
        self.rebar_cache = Default::default();
        self.paint_cache = Default::default();
    }

    /// Number of the top operation-journal entry (0 means the journal is empty).
    pub fn op_top(&self) -> Result<i64, Error> {
        Ok(self
            .db
            .query_row("SELECT coalesce(max(seq),0) FROM op", [], |r| r.get(0))?)
    }

    /// Undoes while the top entry is newer than `seq`; returns the number of undone entries.
    pub fn rollback_to(&mut self, seq: i64) -> Result<usize, Error> {
        self.undo_to(seq)
    }

    /// The plan is stale: the live model changed after `base_head` (caches do not count), the `meta`
    /// settings changed (the current stage sets the stage of new elements), or the journal window is lost.
    pub fn plan_stale(&self, base_head: u64) -> bool {
        let ch = self.changes_since(base_head);
        ch.all || !ch.only(PLAN_IGNORED) || self.meta_written_since(base_head)
    }

    /// Rows changed after `base_head`, for checking a plan by set intersection (without the
    /// `PLAN_IGNORED` caches). A `meta` edit or a lost window yields `all`: the whole plan is stale.
    pub fn plan_changes(&self, base_head: u64) -> Changes {
        let mut ch = self.changes_since(base_head);
        if self.meta_written_since(base_head) {
            ch.all = true;
        }
        for t in PLAN_IGNORED {
            ch.rows.remove(*t);
        }
        ch
    }

    /// Integrity violations absent from `base`: only they block applying the plan.
    /// Measured values in the violation text do not distinguish it from the earlier one.
    pub fn integrity_regressions(&self, base: &[Issue]) -> Result<Vec<Issue>, Error> {
        let known: HashSet<(String, i64, String)> = base
            .iter()
            .map(|i| (i.table.clone(), i.id, problem_key(&i.problem)))
            .collect();
        let mut now = self.check_integrity()?;
        now.retain(|i| !known.contains(&(i.table.clone(), i.id, problem_key(&i.problem))));
        Ok(now)
    }

    /// A consistent chunked copy of the model file from a second connection. In WAL mode the read snapshot
    /// does not block commits of the live model and they do not restart the copy. Otherwise (a foreign file
    /// with a rollback journal) commits restart it, and after `BACKUP_RESTARTS` steps without progress it fails.
    /// `progress(done, total)` is in pages; `false` means cancel, and `dst` does not appear.
    pub fn backup_file(
        src: &Path,
        dst: &Path,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<(), Error> {
        if !src.is_file() {
            return Err(Error::Invalid(format!("no model file: {}", src.display())));
        }
        if let Some(d) = dst.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(d)?;
        }
        let from = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        hold_snapshot(&from)?;
        let tmp = sidecar(dst, ".part");
        let _ = fs::remove_file(&tmp);
        let done = backup_pages(&from, &tmp, progress);
        drop(from);
        match done {
            Ok(true) => install(&tmp, dst),
            Ok(false) => {
                let _ = fs::remove_file(&tmp);
                Err(Error::Invalid("model copy cancelled".into()))
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Copy of this model (also one held in memory) as a file `dst`, made with the backup API
    /// from the open connection; `dst` appears only when the copy is complete.
    pub fn backup_to(&self, dst: &Path) -> Result<(), Error> {
        let tmp = sidecar(dst, ".part");
        let _ = fs::remove_file(&tmp);
        match backup_pages(self.sql(), &tmp, &mut |_: u64, _: u64| true) {
            Ok(_) => install(&tmp, dst),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Rows differing from the `base` file, over the tables in `touched` (all tables when `all`).
    /// Compares contents rather than the journal: rows removed by REPLACE and the `WHOLE` case are visible.
    /// A row that came back after an undo with a different rowid (composite key) is not counted as a difference.
    /// `meta` is always compared (except the schema version): `Changes` does not show it.
    /// `clash_item` is compared by key, status and comment only. The journal, derived
    /// tables, caches and `ai_*` tables are not compared.
    pub fn diff_from(
        &self,
        base: &Path,
        touched: &Changes,
        limit: usize,
    ) -> Result<ModelDiff, Error> {
        let _s = crate::perf::span("model.diff_from");
        if !base.is_file() && !is_memory_copy(base) {
            return Err(Error::Invalid(format!("no base copy: {}", base.display())));
        }
        let path = base
            .to_str()
            .ok_or_else(|| Error::Invalid("non-UTF-8 path".into()))?;
        let detach = format!("DETACH DATABASE {ALIAS}");
        // a panic in an earlier diff may have left the alias attached
        let _ = self.db.execute(&detach, []);
        self.db
            .execute(&format!("ATTACH DATABASE ?1 AS {ALIAS}"), [path])?;
        let res = diff_attached(&self.db, touched, limit);
        let _ = self.db.execute(&detach, []);
        res
    }

    /// Volume and area by material and level (all levels when `None`), per level and material.
    /// The source is the derived quantities; components and enclosures do not store material.
    /// Elements of non-primary project options are excluded.
    pub fn material_totals(&self, level: Option<i64>) -> Result<Vec<MaterialQty>, Error> {
        self.material_totals_in(level, None)
    }

    /// Same, as seen by option `option` (see `level_totals_in`).
    pub fn material_totals_in(
        &self,
        level: Option<i64>,
        option: Option<i64>,
    ) -> Result<Vec<MaterialQty>, Error> {
        let sql = MATERIAL_SQL.replace("{HIDDEN}", &crate::stats::hidden_sql("ek", "er"));
        let mut st = self.db.prepare(&sql)?;
        let v = st
            .query_map(params![level, option], |r| {
                Ok(MaterialQty {
                    material: r.get(0)?,
                    level: r.get(1)?,
                    volume: r.get(2)?,
                    area: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }
}

const MATERIAL_SQL: &str = "
SELECT material, level, sum(v), sum(a) FROM (
    SELECT q.material AS material, e.level AS level, q.volume AS v, q.plan_area AS a,
        'wall' AS ek, e.id AS er FROM wall_qty q JOIN wall e ON e.id=q.wall
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'slab', e.id
        FROM slab_qty q JOIN slab e ON e.id=q.slab
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'roof', e.id
        FROM roof_qty q JOIN roof e ON e.id=q.roof
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'ceiling', e.id
        FROM ceiling_qty q JOIN ceiling e ON e.id=q.ceiling
    UNION ALL SELECT t.material, e.level, q.volume, q.surface, 'col', e.id FROM member_qty q
        JOIN col e ON q.kind='column' AND e.id=q.ref JOIN member_type t ON t.id=e.mtype
    UNION ALL SELECT t.material, e.level, q.volume, q.surface, 'beam', e.id FROM member_qty q
        JOIN beam e ON q.kind='beam' AND e.id=q.ref JOIN member_type t ON t.id=e.mtype
    UNION ALL SELECT e.material, e.level, q.volume, 0, 'stair', e.id
        FROM stair_qty q JOIN stair e ON e.id=q.stair
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'iso_footing', e.id FROM struct_qty q
        JOIN iso_footing e ON q.kind='footing' AND e.id=q.ref
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'strip_footing', e.id FROM struct_qty q
        JOIN strip_footing e ON q.kind='strip_footing' AND e.id=q.ref
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'raft', e.id FROM struct_qty q
        JOIN raft e ON q.kind='raft' AND e.id=q.ref
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'brace', e.id FROM struct_qty q
        JOIN brace e ON q.kind='brace' AND e.id=q.ref
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'truss', e.id FROM struct_qty q
        JOIN truss e ON q.kind='truss' AND e.id=q.ref
    UNION ALL SELECT q.material, e.level, q.volume, q.area, 'beam_system', e.id FROM struct_qty q
        JOIN beam_system e ON q.kind='beam_system' AND e.id=q.ref
    UNION ALL SELECT e.material, e.level, q.net_volume, q.area, 'solid', e.id FROM solid_qty q
        JOIN solid e ON e.id=q.solid WHERE e.void=0 AND q.error IS NULL
) WHERE (?1 IS NULL OR level=?1) AND NOT {HIDDEN}
GROUP BY material, level ORDER BY level, material";

/// In WAL an open read holds a snapshot: copy steps run against it and commits do not restart the copy.
/// With a rollback journal the same read would block commits, so there is no snapshot there.
fn hold_snapshot(from: &Connection) -> Result<(), Error> {
    let mode: String = from.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode.eq_ignore_ascii_case("wal") {
        from.execute_batch("BEGIN")?;
        from.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    }
    Ok(())
}

/// `Ok(false)` means cancelled; the destination file is closed in every case.
fn backup_pages(
    from: &Connection,
    tmp: &Path,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<bool, Error> {
    let mut to = Connection::open(tmp)?;
    // a memory copy keeps its page size once made: an empty one takes the source's
    let size: i64 = from.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    to.pragma_update(None, "page_size", size)?;
    {
        let b = Backup::new(from, &mut to)?;
        let mut last_move = Instant::now();
        let (mut last_left, mut restarts) = (u64::MAX, 0u32);
        loop {
            match b.step(BACKUP_PAGES)? {
                StepResult::Done => break,
                StepResult::More => {
                    last_move = Instant::now();
                    let left = b.progress().remaining.max(0) as u64;
                    if left >= last_left {
                        restarts += 1;
                        if restarts > BACKUP_RESTARTS {
                            return Err(Error::Invalid(
                                "model copy: the file keeps changing".into(),
                            ));
                        }
                    }
                    last_left = left;
                }
                _ => {
                    if last_move.elapsed() > BACKUP_BUSY {
                        return Err(Error::Invalid("model copy: file is busy".into()));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let p = b.progress();
            let total = p.pagecount.max(0) as u64;
            let left = p.remaining.max(0) as u64;
            if !progress(total.saturating_sub(left), total) {
                return Ok(false);
            }
        }
        let total = b.progress().pagecount.max(0) as u64;
        if !progress(total, total) {
            return Ok(false);
        }
    }
    to.close().map_err(|(_, e)| e)?;
    Ok(true)
}

/// Recomputed from elements: moving a wall would produce neighbour rows without any user edit.
const DERIVED: &[&str] = &[
    "dim_geo",
    "measure_geo",
    "measure_ref",
    "join_cache",
    "mass_cache",
    "struct_issue",
];

/// Journal, derived tables and quantities, caches, service tables.
fn skipped(t: &str) -> bool {
    // value_src: the Apply records it after the writes the card showed
    matches!(t, "op" | "mesh_proto" | "mesh_geo" | "comp_thumb" | "value_src")
        || DERIVED.contains(&t)
        || t.ends_with("_qty")
        || t.starts_with("ai_")
        || t == "issue"
        || t.starts_with("issue_")
        || t.starts_with("sqlite_")
        || crate::calc_store::is_run_table(t)
}

/// Violation text without measured values ("3.200 > 3.000"); integers are row references and stay.
fn problem_key(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len());
    let mut i = 0;
    while i < c.len() {
        let start = i;
        while i < c.len() && c[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            out.push(c[i]);
            i += 1;
        } else if i + 1 < c.len() && c[i] == '.' && c[i + 1].is_ascii_digit() {
            i += 1;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            out.push('#');
        } else {
            out.extend(&c[start..i]);
        }
    }
    out
}

/// `clash_item` columns that matter to the user; the rest is recomputed by a check run.
const CLASH_COLS: [&str; 7] = ["test", "ka", "ia", "kb", "ib", "status", "comment"];

fn compared(table: &str, col: &str) -> bool {
    table != "clash_item" || CLASH_COLS.contains(&col)
}

fn table_names(db: &Connection, schema: &str) -> Result<Vec<String>, Error> {
    let mut st = db.prepare(&format!(
        "SELECT name FROM {schema}.sqlite_master WHERE type='table'"
    ))?;
    let v = st
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(v)
}

fn is_element(t: &str) -> bool {
    crate::phases::PHASED.iter().any(|p| *p == t)
}

fn diff_attached(db: &Connection, touched: &Changes, limit: usize) -> Result<ModelDiff, Error> {
    let names: BTreeSet<String> = table_names(db, "main")?
        .into_iter()
        .chain(table_names(db, ALIAS)?)
        .filter(|t| !skipped(t) && (t.as_str() == "meta" || touched.touched(t)))
        .collect();
    let mut order: Vec<String> = names.into_iter().collect();
    // a truncated diff keeps the elements themselves, not the rows recalculated around them
    order.sort_by_key(|t| !is_element(t));
    let mut out = Out {
        rows: vec![],
        limit,
        truncated: false,
    };
    for t in &order {
        let tab = Table::read(db, t)?;
        let more = if tab.alias.is_some() {
            diff_by_rowid(db, &tab, &mut out)?
        } else {
            diff_by_key(db, &tab, &mut out)?
        };
        if !more {
            break;
        }
    }
    Ok(ModelDiff {
        rows: out.rows,
        truncated: out.truncated,
    })
}

struct Out {
    rows: Vec<RowDiff>,
    limit: usize,
    truncated: bool,
}

impl Out {
    /// `false` means the limit is exhausted and the row was not added.
    fn push(&mut self, r: RowDiff) -> bool {
        if self.rows.len() >= self.limit {
            self.truncated = true;
            return false;
        }
        self.rows.push(r);
        true
    }
}

enum Owner {
    None,
    Own,
    KindRef(usize, usize),
    Parent(usize, &'static str),
}

/// Table columns in both databases and how a row is identified.
struct Table {
    name: String,
    cols: Vec<String>,
    /// The rowid-alias column: the row is identified by it.
    alias: Option<usize>,
    /// Otherwise the primary-key columns; with no key, the whole row.
    key: Vec<usize>,
    owner: Owner,
    after_sql: String,
    before_sql: String,
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Name, type, position in the primary key, default value (SQL text).
type ColInfo = (String, String, i64, Option<String>);

fn columns(db: &Connection, schema: &str, t: &str) -> Result<Vec<ColInfo>, Error> {
    let mut st = db
        .prepare("SELECT name, type, pk, dflt_value FROM pragma_table_info(?1, ?2) ORDER BY cid")?;
    let v = st
        .query_map([t, schema], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<ColInfo>, _>>()?;
    Ok(v)
}

impl Table {
    fn read(db: &Connection, name: &str) -> Result<Self, Error> {
        let main = columns(db, "main", name)?;
        let base = columns(db, ALIAS, name)?;
        let (has_main, has_base) = (!main.is_empty(), !base.is_empty());
        let (primary, other, schema) = if has_main {
            (&main, &base, "main")
        } else {
            (&base, &main, ALIAS)
        };
        let info: Vec<&ColInfo> = primary
            .iter()
            .chain(other.iter().filter(|o| !primary.iter().any(|c| c.0 == o.0)))
            .filter(|c| compared(name, &c.0))
            .collect();
        let no_rowid = db
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema=?1 AND name=?2",
                [schema, name],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            != 0;
        let mut pk: Vec<(i64, usize)> = info
            .iter()
            .enumerate()
            .filter(|(_, c)| c.2 > 0)
            .map(|(i, c)| (c.2, i))
            .collect();
        pk.sort_unstable();
        let key: Vec<usize> = pk.into_iter().map(|p| p.1).collect();
        let alias = match key.as_slice() {
            [i] if !no_rowid && info[*i].1.eq_ignore_ascii_case("INTEGER") => Some(*i),
            _ => None,
        };
        let cols: Vec<String> = info.iter().map(|c| c.0.clone()).collect();
        // a column of one side (late ALTER): the other side reads what ALTER gives old rows
        let list = |side: &[ColInfo]| {
            info.iter()
                .map(|c| match (side.iter().any(|x| x.0 == c.0), &c.3) {
                    (true, _) => quote(&c.0),
                    (false, Some(d)) => format!("({d}) AS {}", quote(&c.0)),
                    (false, None) => format!("NULL AS {}", quote(&c.0)),
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        let rid = if no_rowid { "0" } else { "rowid" };
        let only = if name == "meta" {
            " WHERE key NOT IN ('schema_version','project_uid')"
        } else {
            ""
        };
        let sel = |s: &str, l: String| format!("SELECT {rid},{l} FROM {s}.{}{only}", quote(name));
        let empty = format!("SELECT 0{} WHERE 0", ",NULL".repeat(cols.len()));
        let (m, b) = (sel("main", list(&main)), sel(ALIAS, list(&base)));
        let (after_sql, before_sql) = match (has_main, has_base) {
            (true, true) => (
                format!("{m} EXCEPT {b} ORDER BY 1"),
                format!("{b} EXCEPT {m} ORDER BY 1"),
            ),
            (true, false) => (format!("{m} ORDER BY 1"), empty),
            _ => (empty, format!("{b} ORDER BY 1")),
        };
        let owner = owner_rule(name, &cols, alias.is_some());
        Ok(Self {
            name: name.to_string(),
            cols,
            alias,
            key,
            owner,
            after_sql,
            before_sql,
        })
    }

    fn row(
        &self,
        rowid: i64,
        kind: RowKind,
        before: Option<&[Value]>,
        after: Option<&[Value]>,
    ) -> Option<RowDiff> {
        let mut cells = vec![];
        for (i, col) in self.cols.iter().enumerate() {
            if Some(i) == self.alias {
                continue;
            }
            let (b, a) = (before.map(|v| &v[i]), after.map(|v| &v[i]));
            if let (Some(x), Some(y)) = (b, a) {
                if same(x, y) {
                    continue;
                }
            }
            let (mut bt, mut at) = (b.and_then(cell_text), a.and_then(cell_text));
            if let (Some(Value::Blob(x)), Some(Value::Blob(y))) = (b, a) {
                if x.len() == y.len() {
                    bt = Some(format!("blob({}) {:016x}", x.len(), fnv1a(x)));
                    at = Some(format!("blob({}) {:016x}", y.len(), fnv1a(y)));
                }
            }
            if bt.is_some() || at.is_some() {
                cells.push(CellDiff {
                    col: col.clone(),
                    before: bt,
                    after: at,
                });
            }
        }
        if kind == RowKind::Changed && cells.is_empty() {
            return None;
        }
        let values = after.or(before).unwrap_or(&[]);
        if self.name == "meta" && kind == RowKind::Changed {
            // `value` says nothing: a setting is known by its key
            if let Some(Value::Text(k)) = self.key.first().and_then(|i| values.get(*i)) {
                for c in &mut cells {
                    c.col = k.clone();
                }
            }
        }
        Some(RowDiff {
            table: self.name.clone(),
            rowid,
            kind,
            owner: self.owner_of(rowid, values),
            cells,
        })
    }

    fn owner_of(&self, rowid: i64, v: &[Value]) -> Option<ElKey> {
        match self.owner {
            Owner::None => None,
            Owner::Own => Some((self.name.clone(), rowid)),
            Owner::KindRef(k, r) => match (v.get(k)?, v.get(r)?) {
                (Value::Text(kind), Value::Integer(id)) => Some((kind.clone(), *id)),
                _ => None,
            },
            Owner::Parent(c, parent) => match v.get(c)? {
                Value::Integer(id) => Some((parent.to_string(), *id)),
                _ => None,
            },
        }
    }
}

/// Pair kind/ref, then the element itself, then a part of its owner, then a row with its own id, then the table's first reference.
fn owner_rule(table: &str, cols: &[String], has_alias: bool) -> Owner {
    let at = |name: &str| cols.iter().position(|c| c == name);
    if table == "clash_item" {
        if let (Some(k), Some(r)) = (at("ka"), at("ia")) {
            return Owner::KindRef(k, r);
        }
    }
    if matches!(table, "prop" | "pparam_val" | "elem_phase") {
        if let (Some(k), Some(r)) = (at("kind"), at("ref")) {
            return Owner::KindRef(k, r);
        }
    }
    if is_element(table) {
        return Owner::Own;
    }
    let parent = crate::integrity::parent_ref(table);
    if let Some((col, to, true)) = parent {
        if let Some(c) = at(col) {
            return Owner::Parent(c, to);
        }
    }
    if has_alias {
        return Owner::Own;
    }
    match parent {
        Some((col, to, _)) => at(col).map_or(Owner::None, |c| Owner::Parent(c, to)),
        None => Owner::None,
    }
}

/// Comparison as in EXCEPT: 1 and 1.0 are equal, 2^53+1 and 2^53.0 are not.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Integer(x), Value::Real(y)) | (Value::Real(y), Value::Integer(x)) => {
            const TWO63: f64 = 9_223_372_036_854_775_808.0;
            y.fract() == 0.0 && (-TWO63..TWO63).contains(y) && *y as i64 == *x
        }
        _ => a == b,
    }
}

fn cell_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(i.to_string()),
        Value::Real(f) => Some(f.to_string()),
        Value::Text(s) => Some(s.clone()),
        Value::Blob(b) => Some(format!("blob({})", b.len())),
    }
}

type Row = (i64, Vec<Value>);

fn next_row(rows: &mut rusqlite::Rows<'_>, n: usize) -> Result<Option<Row>, Error> {
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let id: i64 = r.get(0)?;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        v.push(r.get::<_, Value>(i + 1)?);
    }
    Ok(Some((id, v)))
}

/// Both sides of EXCEPT are ordered by rowid: a merge without storing rows, stopping at the limit.
/// `Ok(false)` means the limit is exhausted.
fn diff_by_rowid(db: &Connection, tab: &Table, out: &mut Out) -> Result<bool, Error> {
    let n = tab.cols.len();
    let mut after_st = db.prepare(&tab.after_sql)?;
    let mut before_st = db.prepare(&tab.before_sql)?;
    let mut after = after_st.query([])?;
    let mut before = before_st.query([])?;
    let mut a = next_row(&mut after, n)?;
    let mut b = next_row(&mut before, n)?;
    loop {
        let row = match (a.take(), b.take()) {
            (None, None) => return Ok(true),
            (Some(x), None) => {
                a = next_row(&mut after, n)?;
                tab.row(x.0, RowKind::Created, None, Some(x.1.as_slice()))
            }
            (None, Some(y)) => {
                b = next_row(&mut before, n)?;
                tab.row(y.0, RowKind::Deleted, Some(y.1.as_slice()), None)
            }
            (Some(x), Some(y)) => match x.0.cmp(&y.0) {
                Ordering::Less => {
                    b = Some(y);
                    a = next_row(&mut after, n)?;
                    tab.row(x.0, RowKind::Created, None, Some(x.1.as_slice()))
                }
                Ordering::Greater => {
                    a = Some(x);
                    b = next_row(&mut before, n)?;
                    tab.row(y.0, RowKind::Deleted, Some(y.1.as_slice()), None)
                }
                Ordering::Equal => {
                    a = next_row(&mut after, n)?;
                    b = next_row(&mut before, n)?;
                    tab.row(
                        x.0,
                        RowKind::Changed,
                        Some(y.1.as_slice()),
                        Some(x.1.as_slice()),
                    )
                }
            },
        };
        if let Some(r) = row {
            if !out.push(r) {
                return Ok(false);
            }
        }
    }
}

fn read_all(db: &Connection, sql: &str, n: usize) -> Result<Vec<Row>, Error> {
    let mut st = db.prepare(sql)?;
    let mut rows = st.query([])?;
    let mut v = vec![];
    while let Some(r) = next_row(&mut rows, n)? {
        v.push(r);
    }
    Ok(v)
}

fn key_text(key: &[usize], v: &[Value]) -> String {
    let mut s = String::new();
    if key.is_empty() {
        for c in v {
            s += &format!("{c:?}\u{1f}");
        }
    } else {
        for &i in key {
            s += &format!("{:?}\u{1f}", v[i]);
        }
    }
    s
}

/// Tables without a rowid alias: rowid is unstable (undo re-inserts the row), so the pair
/// is looked up by key, and a row with the same content but a new rowid is not a difference.
fn diff_by_key(db: &Connection, tab: &Table, out: &mut Out) -> Result<bool, Error> {
    let n = tab.cols.len();
    let after = read_all(db, &tab.after_sql, n)?;
    let mut old: HashMap<String, Vec<Row>> = HashMap::new();
    for r in read_all(db, &tab.before_sql, n)? {
        old.entry(key_text(&tab.key, &r.1)).or_default().push(r);
    }
    let mut rows = vec![];
    for (id, v) in &after {
        let pair = old.get_mut(&key_text(&tab.key, v)).and_then(Vec::pop);
        rows.extend(match pair {
            Some((_, was)) => tab.row(
                *id,
                RowKind::Changed,
                Some(was.as_slice()),
                Some(v.as_slice()),
            ),
            None => tab.row(*id, RowKind::Created, None, Some(v.as_slice())),
        });
    }
    for (id, was) in old.values().flatten() {
        rows.extend(tab.row(*id, RowKind::Deleted, Some(was.as_slice()), None));
    }
    rows.sort_by_key(|r| r.rowid);
    for r in rows {
        if !out.push(r) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "ai_plan_tests.rs"]
mod tests;
