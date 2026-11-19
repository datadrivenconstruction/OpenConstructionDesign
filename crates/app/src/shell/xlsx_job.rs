//! XLSX export and re-export on a worker: the model is read through its own connection and
//! is never changed; the book is written beside the target and moved into place only when
//! the run was not cancelled, so a cancel or a failure leaves no partial file.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use web_time::Instant;

use ddc_io::xlsx;

pub enum Outcome {
    Done,
    /// The book was locked: a refreshed copy was written beside it.
    Copied(PathBuf),
    /// The book was written, but the old one held sheets that cannot be carried over (chart
    /// sheets and the like) or `lost` filled cells of the user's own columns whose rows are
    /// gone; the old book is kept as `old`.
    Dropped { names: Vec<String>, lost: usize, old: PathBuf },
    Cancelled,
    Failed(String),
}

/// What a re-export did not carry over, and where the old book is kept.
#[derive(Debug)]
pub struct Note {
    pub names: Vec<String>,
    pub lost: usize,
    pub old: PathBuf,
}

impl Note {
    /// Status text: dropped sheets and lost cells, each with the old book's path; never empty.
    pub fn text(&self, ru: bool) -> String {
        let mut parts = Vec::new();
        if !self.names.is_empty() {
            parts.push(dropped_note(ru, &self.names, &self.old));
        }
        if self.lost > 0 {
            parts.push(lost_note(ru, self.lost, &self.old));
        }
        if parts.is_empty() {
            // only confirmed losses: the kept old book is named
            parts.push(crate::tr_lang(ru, "xlloss.old_kept", &[("path", &self.old.display().to_string())]));
        }
        parts.join(" ")
    }
}

impl Outcome {
    /// `Dropped` becomes `Done` plus the note to show; any other outcome passes unchanged.
    pub fn split_note(self) -> (Outcome, Option<Note>) {
        match self {
            Outcome::Dropped { names, lost, old } => (Outcome::Done, Some(Note { names, lost, old })),
            o => (o, None),
        }
    }
}

/// Status text naming how many user cells of removed rows a re-export did not carry over.
pub fn lost_note(ru: bool, lost: usize, old: &Path) -> String {
    crate::tr_lang(ru, "xlw.cells_not_carried", &[("count", &lost.to_string()), ("path", &old.display().to_string())])
}

/// Status text naming the sheets a re-export did not carry over and where the old book is.
pub fn dropped_note(ru: bool, names: &[String], old: &Path) -> String {
    let list = names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ");
    crate::tr_lang(ru, "xlw.sheets_not_carried", &[("names", &list), ("path", &old.display().to_string())])
}

/// Free name beside `dest` for the old book: `Book.xlsx.bak`, then `Book.xlsx.2.bak`, ...
fn backup_of(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut n = 1u32;
    loop {
        let tag = if n == 1 { String::new() } else { format!(".{n}") };
        let p = dest.with_file_name(format!("{name}{tag}.bak"));
        if !p.exists() {
            return p;
        }
        n += 1;
    }
}

/// What the re-export after Apply needs beyond a plain export.
#[derive(Clone, Debug, Default)]
pub struct ApplyTail {
    /// Rows created by the apply: they get their id from this export.
    pub created: usize,
    /// Rows matched by key: the export writes their id.
    pub matched: usize,
    /// Conflict cells the user left on the model value; the book keeps their values.
    pub kept_any: bool,
    /// The scope could not be extended: no re-export, the book lacks the new rows.
    pub skip: bool,
    /// With `skip`: the ids went into the book in place (or none were needed), it is in sync.
    pub ids_in_place: bool,
}

/// Extra input of a post-Apply re-export.
#[derive(Default)]
pub struct JobOpts {
    pub kept: Vec<xlsx::KeptCell>,
    pub apply: Option<ApplyTail>,
    /// What the book carries; None = the whole model.
    pub scope: Option<xlsx::Scope>,
    /// Losses of the old book the user confirmed (English items); any other loss refuses.
    pub accept_losses: Vec<String>,
    /// "Export to a copy": the target book is read, never written; the refreshed book goes to
    /// this new file (`Outcome::Copied`).
    pub copy_to: Option<PathBuf>,
}

/// The model state a preview or a worker result was built on (`Editor::content_snap`).
pub type Snap = Option<ddc_model::ContentMark>;

pub struct XlsxJob {
    pub target: PathBuf,
    /// Re-export into the linked book (not a first export).
    pub again: bool,
    /// Where the model stood when the worker started.
    pub rev: Snap,
    pub started: Instant,
    /// Set for the re-export that follows Apply.
    pub apply: Option<ApplyTail>,
    cancel: Arc<AtomicBool>,
    stage: Arc<AtomicU8>,
    /// Fraction of the write stage as f32 bits, fed by the export hook.
    progress: Arc<AtomicU32>,
    rx: Receiver<Outcome>,
    /// Cells the export cut or left blank; filled by the worker before it sends the result.
    notes: Arc<Mutex<Vec<xlsx::ExportNote>>>,
}

/// Status text after an export that cut or blanked `n` cells; None when it did none.
pub fn cells_status(ru: bool, n: usize) -> Option<String> {
    (n > 0).then(|| crate::tr_lang(ru, "xlcell.status", &[("count", &n.to_string())]))
}

/// Stage numbers shown as the bar: 0 reading the model, 1 writing, 2 checking the written
/// book by read-back, 3 moving into place. `STAGES` itself means the result was taken.
pub const STAGES: u8 = 4;

impl XlsxJob {
    /// Starts the worker; `db` is the model's file. None (a model without a file) cannot be
    /// read from another thread: the caller runs `export_sync` instead.
    pub fn start(
        db: String,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
    ) -> Self {
        Self::spawn(db, false, target, again, u, locale, rev, JobOpts::default())
    }

    /// `start` with options (a scoped export).
    pub fn start_db(
        db: String,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
        opts: JobOpts,
    ) -> Self {
        Self::spawn(db, false, target, again, u, locale, rev, opts)
    }

    /// `start` or `start_copy`, whichever the model allows.
    pub fn start_for(
        model: &ddc_model::Model,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
        opts: JobOpts,
    ) -> Result<Self, String> {
        match model.db_path() {
            Some(db) => Ok(Self::spawn(db, false, target, again, u, locale, rev, opts)),
            None => Self::copy_spawn(model, target, again, u, locale, rev, opts),
        }
    }

    /// A model without a file: copied with the backup API to a temp file (removed when the
    /// worker ends), then exported like any other.
    pub fn start_copy(
        model: &ddc_model::Model,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
    ) -> Result<Self, String> {
        Self::copy_spawn(model, target, again, u, locale, rev, JobOpts::default())
    }

    fn copy_spawn(
        model: &ddc_model::Model,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
        opts: JobOpts,
    ) -> Result<Self, String> {
        static N: AtomicU32 = AtomicU32::new(0);
        let tmp = std::env::temp_dir().join(format!(
            "ddc_xl_mem_{}_{}.ddc",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        model.backup_to(&tmp).map_err(|e| e.to_string())?;
        Ok(Self::spawn(tmp.to_string_lossy().into_owned(), true, target, again, u, locale, rev, opts))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn(
        db: String,
        own: bool,
        target: &Path,
        again: bool,
        u: xlsx::Units,
        locale: &str,
        rev: Snap,
        opts: JobOpts,
    ) -> Self {
        let apply = opts.apply.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stage = Arc::new(AtomicU8::new(0));
        let progress = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let notes = Arc::new(Mutex::new(Vec::new()));
        let nn = notes.clone();
        let (c, s, p, t) = (cancel.clone(), stage.clone(), progress.clone(), target.to_path_buf());
        let locale = locale.to_string();
        let tx_panic = tx.clone();
        // the worker reads a copy of the model and writes only the book: the model is untouched
        let spawned = crate::jobs::spawn_guarded("xl-export", move || {
            // removes the temp copy of a file-less model also when the export panics
            struct Cleanup(Option<String>);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    if let Some(db) = self.0.take() {
                        for suffix in ["", "-wal", "-shm", "-journal"] {
                            let _ = std::fs::remove_file(format!("{db}{suffix}"));
                        }
                    }
                }
            }
            let _cleanup = Cleanup(own.then(|| db.clone()));
            let out = match ddc_model::Model::open_copy(&db) {
                Ok(m) => run(&m, &t, u, &locale, &c, &s, &p, &opts),
                Err(e) => Outcome::Failed(e.to_string()),
            };
            // the link write after the export finds the new book's fingerprint cached
            match &out {
                Outcome::Done | Outcome::Dropped { .. } => super::xlsx_relink::warm(&t),
                Outcome::Copied(p) => super::xlsx_relink::warm(p),
                _ => {}
            }
            // the notes live in a thread-local of this worker: taken here, before the result is sent
            if let Ok(mut g) = nn.lock() {
                *g = xlsx::take_export_notes();
            }
            let _ = tx.send(out);
        }, move |msg| {
            let _ = tx_panic.send(Outcome::Failed(msg));
        });
        if let Err(e) = spawned {
            eprintln!("xlsx export thread: {e}");
        }
        Self {
            target: target.to_path_buf(),
            again,
            rev,
            started: Instant::now(),
            apply,
            cancel,
            stage,
            progress,
            rx,
            notes,
        }
    }

    /// Cells the finished export cut or left blank (empty until the result is polled).
    pub fn take_notes(&self) -> Vec<xlsx::ExportNote> {
        self.notes.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 0.0..1.0 for the bar.
    pub fn fraction(&self) -> f32 {
        let stage = self.stage.load(Ordering::Relaxed);
        let p = f32::from_bits(self.progress.load(Ordering::Relaxed));
        match stage {
            1 => (1.0 + p.clamp(0.0, 1.0)) / f32::from(STAGES),
            // the check reports 1 + its fraction in the same value
            2 => (2.0 + (p - 1.0).clamp(0.0, 1.0)) / f32::from(STAGES),
            s if s >= STAGES => 1.0,
            s => (f32::from(s) + 0.5) / f32::from(STAGES),
        }
    }

    pub fn stage(&self) -> u8 {
        self.stage.load(Ordering::Relaxed).min(STAGES - 1)
    }

    /// The result once the worker is done.
    pub fn poll(&self) -> Option<Outcome> {
        match self.rx.try_recv() {
            Ok(o) => {
                self.stage.store(STAGES, Ordering::Relaxed);
                Some(o)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Outcome::Failed(crate::jobs::internal_error())),
        }
    }
}

#[cfg(test)]
impl XlsxJob {
    /// A running export that never reports a result, for the layout contract.
    pub(crate) fn parked(target: &Path, stage: u8) -> Self {
        let (tx, rx) = mpsc::channel();
        // a dropped sender would read as a crashed worker
        std::mem::forget(tx);
        Self {
            target: target.to_path_buf(),
            again: false,
            rev: None,
            started: Instant::now(),
            apply: None,
            cancel: Arc::default(),
            stage: Arc::new(AtomicU8::new(stage)),
            progress: Arc::default(),
            rx,
            notes: Arc::default(),
        }
    }
}

/// Same steps on the calling thread (a model that has no file).
pub fn export_sync(model: &ddc_model::Model, target: &Path, u: xlsx::Units, locale: &str) -> Outcome {
    run(
        model,
        target,
        u,
        locale,
        &Arc::new(AtomicBool::new(false)),
        &Arc::new(AtomicU8::new(0)),
        &Arc::new(AtomicU32::new(0)),
        &JobOpts::default(),
    )
}

/// Refusal when the book was saved or created on disk while the export ran (the UI translates it).
pub const BOOK_CHANGED: &str =
    "the workbook was saved by another program during the export and was not overwritten; export again";

/// A book on disk by size, time and content: equal ids are the bytes the export started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BookId {
    stamp: xlsx::Stamp,
    hash: Option<u64>,
}

impl BookId {
    fn of(path: &Path) -> Option<BookId> {
        let stamp = xlsx::Stamp::of(path)?;
        Some(BookId { stamp, hash: content_hash(path) })
    }
}

/// None when the file cannot be read: it then never matches a readable one.
fn content_hash(path: &Path) -> Option<u64> {
    use std::hash::Hasher;
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            return Some(h.finish());
        }
        h.write(&buf[..n]);
    }
}

#[cfg(test)]
thread_local! {
    /// Runs once between the write of the new book and its move into place.
    static MID_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

#[cfg(test)]
fn mid_write() {
    if let Some(f) = MID_WRITE.with(|m| m.borrow_mut().take()) {
        f();
    }
}

#[cfg(not(test))]
fn mid_write() {}

pub(super) fn part_of(target: &Path) -> PathBuf {
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!("{stem}.export-part.xlsx"))
}

/// Writes the book from `from` (its user columns and sheets are kept) into `to`; the hook
/// reports the fraction and stops the write when `cancel` is set. Nothing is saved on a stop.
pub fn write_book(
    model: &ddc_model::Model,
    from: &Path,
    to: &Path,
    u: xlsx::Units,
    locale: &str,
    cancel: &Arc<AtomicBool>,
    progress: &Arc<AtomicU32>,
    scope: Option<&xlsx::Scope>,
) -> Result<(), String> {
    let fams = crate::elem::sheets::families();
    let (c, p) = (cancel.clone(), progress.clone());
    let hook: xlsx::Hook = Arc::new(move |f| {
        p.store(f.to_bits(), Ordering::Relaxed);
        !c.load(Ordering::Relaxed)
    });
    let mut style = super::xlsx_style::style(locale);
    if let Some(s) = scope {
        style = style.with_scope(s.clone());
    }
    xlsx::export_xlsx_hooked(model, from, to, u, &fams, &style, Some(hook))
}

#[allow(clippy::too_many_arguments)]
fn run(
    model: &ddc_model::Model,
    target: &Path,
    u: xlsx::Units,
    locale: &str,
    cancel: &Arc<AtomicBool>,
    stage: &Arc<AtomicU8>,
    progress: &Arc<AtomicU32>,
    opts: &JobOpts,
) -> Outcome {
    stage.store(1, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Outcome::Cancelled;
    }
    // the original is only read: its columns and sheets go into a new file beside it
    if let Some(copy) = &opts.copy_to {
        if copy.exists() || target == copy {
            return Outcome::Failed(super::xlsx_owner::COPY_EXISTS.into());
        }
        return match place(model, target, copy, BookId::of(copy), u, locale, cancel, stage, progress, opts) {
            Outcome::Done | Outcome::Dropped { .. } => Outcome::Copied(copy.clone()),
            other => other,
        };
    }
    // user columns and sheets of the current book are carried into the new file; the book is
    // identified before it is read, so one saved or created during the export is never replaced
    let attempt = |seen: Option<BookId>| {
        let from = if seen.is_some() { target.to_path_buf() } else { part_of(target) };
        place(model, &from, target, seen, u, locale, cancel, stage, progress, opts)
    };
    let mut out = attempt(BookId::of(target));
    // a plain export starts over once from the book as it is now (its loss check included);
    // after Apply the edits came from the older book: the copy path below takes it
    if opts.apply.is_none() && matches!(&out, Outcome::Failed(e) if e == BOOK_CHANGED) {
        out = attempt(BookId::of(target));
    }
    // a locked book after Apply is left alone; a refreshed copy goes beside it. A book that
    // failed the read-back check is no lock: nothing new is written, the user retries
    let locked = matches!(&out, Outcome::Failed(e) if !super::xlsx_export_verify::is_refusal(e));
    if opts.apply.is_some() && locked && target.exists() {
        if let Some(copy) = super::xlsx_win::free_copy(target) {
            let seen = BookId::of(&copy);
            return match place(model, target, &copy, seen, u, locale, cancel, stage, progress, opts) {
                // the locked book keeps every sheet: a copy beside it is all that was written
                Outcome::Done | Outcome::Dropped { .. } => Outcome::Copied(copy),
                other => other,
            };
        }
    }
    out
}

/// Writes beside `dest` and moves into place only when not cancelled: a failure leaves `dest` as it was.
#[allow(clippy::too_many_arguments)]
fn place(
    model: &ddc_model::Model,
    from: &Path,
    dest: &Path,
    seen: Option<BookId>,
    u: xlsx::Units,
    locale: &str,
    cancel: &Arc<AtomicBool>,
    stage: &Arc<AtomicU8>,
    progress: &Arc<AtomicU32>,
    opts: &JobOpts,
) -> Outcome {
    // replacing the book itself: an unreadable book would lose the user's own columns and sheets
    if from == dest {
        if let Err(e) = std::fs::File::open(dest) {
            return Outcome::Failed(e.to_string());
        }
    }
    // a read-only book is not replaced, moved aside or half written: nothing is created
    if dest.exists() && super::xlsx_owner::read_only(dest) {
        return Outcome::Failed(super::xlsx_owner::READ_ONLY.into());
    }
    // a book open in another program (owner file, share lock) is not replaced: nothing written
    if super::xlsx_owner::check_replace(dest).blocks() {
        return Outcome::Failed(super::xlsx_owner::OPEN_ELSEWHERE.into());
    }
    // sheets the new book cannot hold: read before anything is replaced
    let dropped = if from.exists() { xlsx::dropped_sheets(from) } else { Vec::new() };
    let part = part_of(dest);
    // a panic in the write or the checks must not leave the part beside the book
    // ...unless the old book could not be moved back: the part is then the newest book
    struct PartGuard<'a>(&'a Path, std::cell::Cell<bool>);
    impl Drop for PartGuard<'_> {
        fn drop(&mut self) {
            if !self.1.get() {
                let _ = std::fs::remove_file(self.0);
            }
        }
    }
    let part_guard = PartGuard(&part, std::cell::Cell::new(false));
    // no rights to write in the folder: said as such (a copy beside the book fails the same way).
    // A probe file it created goes at once: an empty part would be read as the old book (`from`)
    let had_part = part.exists();
    match std::fs::OpenOptions::new().write(true).create(true).open(&part) {
        Err(e) if super::xlsx_owner::why(dest, &e) == super::xlsx_owner::Why::Denied => {
            return Outcome::Failed(super::xlsx_owner::DENIED.into());
        }
        Ok(f) if !had_part => {
            drop(f);
            let _ = std::fs::remove_file(&part);
        }
        _ => {}
    }
    stage.store(1, Ordering::Relaxed);
    let wrote = xlsx::with_kept(&opts.kept, || {
        write_book(model, from, &part, u, locale, cancel, progress, opts.scope.as_ref())
    });
    // read on this worker thread right after the export, whatever its result
    let lost = xlsx::take_lost_cells().cells;
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&part);
        return if e == xlsx::CANCELLED { Outcome::Cancelled } else { Outcome::Failed(e) };
    }
    // the write ended at 1.0; the check goes on from there and reports 1 + its fraction
    stage.store(2, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&part);
        return Outcome::Cancelled;
    }
    mid_write();
    // `dest` must still be the book read at the start (or still be missing): checked before the
    // loss check reads it and again right before it is replaced
    let changed = || BookId::of(dest) != seen;
    if changed() {
        let _ = std::fs::remove_file(&part);
        return Outcome::Failed(BOOK_CHANGED.into());
    }
    // one policy for every loss: dropped sheets, user cells of removed rows (reported, never
    // asked: the rows went on purpose) and confirmed losses all move the old book aside once
    let mut aside = !dropped.is_empty() || lost > 0;
    if from == dest && dest.exists() {
        // the new book is compared with the old one: what it would drop is asked about first;
        // cells of rows gone from the model are not counted there, `lost` covers them
        let given = opts.apply.as_ref().map_or(0, |t| t.created + t.matched);
        let losses = xlsx::reexport_losses(dest, &part, &crate::elem::sheets::families(), given);
        let refused = match &losses {
            Ok(l) => xlsx::refusal(l, &opts.accept_losses),
            Err(e) => Some(e.clone()),
        };
        if let Some(e) = refused {
            let _ = std::fs::remove_file(&part);
            return Outcome::Failed(e);
        }
        // confirmed: the old book is kept whole beside the new one
        aside |= losses.is_ok_and(|l| !l.is_empty());
    }
    // the new book must read back as the model before it replaces anything
    let notes = xlsx::export_notes();
    let hook = super::xlsx_export_verify::gate(cancel, progress);
    let verdict = super::xlsx_export_verify::check_hooked(model, &part, dest, &opts.kept, &notes, Some(hook));
    if let Err(f) = verdict {
        let _ = std::fs::remove_file(&part);
        return match f {
            super::xlsx_export_verify::Fault::Cancelled => Outcome::Cancelled,
            f => Outcome::Failed(f.text()),
        };
    }
    progress.fetch_max(2.0f32.to_bits(), Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&part);
        return Outcome::Cancelled;
    }
    stage.store(3, Ordering::Relaxed);
    if changed() {
        let _ = std::fs::remove_file(&part);
        return Outcome::Failed(BOOK_CHANGED.into());
    }
    if !aside {
        return match ddc_io::fsutil::commit_part(&part, dest) {
            Ok(()) => Outcome::Done,
            Err(e) => {
                // the book was opened elsewhere meanwhile: the part goes, the old book stays
                let _ = std::fs::remove_file(&part);
                Outcome::Failed(super::xlsx_owner::failed_at(dest, &e))
            }
        };
    }
    // the old book is moved aside, never overwritten; a book written beside it leaves it as is
    // the part is on the disk before the old book moves
    if let Err(e) = ddc_io::fsutil::sync_file(&part) {
        let _ = std::fs::remove_file(&part);
        return Outcome::Failed(e.to_string());
    }
    let replacing = from == dest;
    let old = if replacing { backup_of(dest) } else { from.to_path_buf() };
    if replacing {
        if let Err(e) = std::fs::rename(dest, &old) {
            let _ = std::fs::remove_file(&part);
            return Outcome::Failed(super::xlsx_owner::failed_at(dest, &e));
        }
    }
    match ddc_io::fsutil::rename_durably(&part, dest) {
        Ok(()) => Outcome::Dropped { names: dropped, lost, old },
        Err(e) => {
            // a failed move back keeps both files; the next Sync offers them (xlsx_recover)
            if replacing && (dest.exists() || std::fs::rename(&old, dest).is_err()) {
                part_guard.1.set(true);
                return Outcome::Failed(super::xlsx_recover::not_restored(&old));
            }
            let _ = std::fs::remove_file(&part);
            Outcome::Failed(super::xlsx_owner::failed_at(dest, &e))
        }
    }
}

/// Opens the book in the program the system associates with it.
pub fn open_book(path: &Path) {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = r {
        eprintln!("open workbook: {e}");
    }
}

/// What «Sync» does: reload the open book, reopen the last one, or offer the export.
#[derive(Debug, PartialEq)]
pub enum SyncAction {
    Reload,
    Open(PathBuf, bool),
    /// The linked book's file is gone (renamed or moved): relink, never a silent fresh export.
    Missing(PathBuf),
    OfferExport,
}

pub fn sync_action(open: bool, last: Option<(PathBuf, bool)>) -> SyncAction {
    if open {
        return SyncAction::Reload;
    }
    match last {
        Some((p, dm)) if p.exists() => SyncAction::Open(p, dm),
        Some((p, _)) => SyncAction::Missing(p),
        None => SyncAction::OfferExport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn file_editor(tag: &str) -> (Editor, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ddc_xl5_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = ddc_model::Model::open(dir.join("m.ddc").to_str().unwrap()).unwrap();
        (Editor::seeded(m).unwrap(), dir)
    }

    fn wait(job: &XlsxJob) -> Outcome {
        for _ in 0..600 {
            if let Some(o) = job.poll() {
                return o;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("export did not finish");
    }

    fn db(ed: &Editor) -> String {
        ed.model.db_path().expect("file model")
    }

    #[test]
    fn export_runs_on_worker_model_rev_unchanged() {
        let (ed, dir) = file_editor("run");
        let rev = ed.revision;
        let book = dir.join("b.xlsx");
        let snap = ed.content_snap();
        let job = XlsxJob::start(db(&ed), &book, false, xlsx::Units::Metric, "en", snap);
        assert!(matches!(wait(&job), Outcome::Done));
        assert!(book.exists());
        assert!(!part_of(&book).exists());
        assert_eq!(ed.revision, rev);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cut_cells_reach_the_job_result() {
        let (mut ed, dir) = file_editor("cutnotes");
        ed.model.add_level(&"x".repeat(40_000), 0.0).unwrap();
        let book = dir.join("b.xlsx");
        let snap = ed.content_snap();
        let job = XlsxJob::start(db(&ed), &book, false, xlsx::Units::Metric, "en", snap);
        assert!(matches!(wait(&job), Outcome::Done));
        let notes = job.take_notes();
        assert!(notes.iter().any(|n| n.kind == xlsx::ExportNoteKind::Clipped && n.id.is_some()), "{notes:?}");
        assert!(job.take_notes().is_empty(), "taken once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_export_has_no_notes_and_no_status() {
        let (ed, dir) = file_editor("nonotes");
        let book = dir.join("b.xlsx");
        let snap = ed.content_snap();
        let job = XlsxJob::start(db(&ed), &book, false, xlsx::Units::Metric, "en", snap);
        assert!(matches!(wait(&job), Outcome::Done));
        assert!(job.take_notes().is_empty());
        assert_eq!(cells_status(true, 0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cells_status_names_the_count_in_both_languages() {
        let en = cells_status(false, 3).unwrap();
        assert!(en.starts_with("Exported.") && en.contains('3'), "{en}");
        let ru = cells_status(true, 3).unwrap();
        assert!(ru.contains('3') && ru != en, "{ru}");
    }

    #[test]
    fn export_cancel_leaves_no_partial_file() {
        let (ed, dir) = file_editor("cancel");
        let book = dir.join("b.xlsx");
        let cancel = Arc::new(AtomicBool::new(true));
        let out = run(
            &ed.model,
            &book,
            xlsx::Units::Metric,
            "en",
            &cancel,
            &Arc::new(AtomicU8::new(0)),
            &Arc::new(AtomicU32::new(0)),
            &JobOpts::default(),
        );
        assert!(matches!(out, Outcome::Cancelled));
        assert!(!book.exists());
        assert!(!part_of(&book).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn is_pro_book(p: &Path) -> bool {
        let n = xlsx::sheet_names(p).unwrap();
        n.iter().any(|s| s == "Keys") && n.iter().any(|s| s == "Lists")
    }

    #[test]
    fn app_export_book_has_keys_and_lists() {
        let (mut ed, dir) = file_editor("pro");
        let book = dir.join("b.xlsx");
        let snap = ed.content_snap();
        let job = XlsxJob::start(db(&ed), &book, false, xlsx::Units::Metric, "ru", snap);
        assert!(matches!(wait(&job), Outcome::Done));
        assert!(is_pro_book(&book));
        let inline = dir.join("c.xlsx");
        assert!(matches!(export_sync(&ed.model, &inline, xlsx::Units::Metric, "en"), Outcome::Done));
        assert!(is_pro_book(&inline));
        let _ = &mut ed;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ru_captions_import_roundtrip_no_changes() {
        let (mut ed, dir) = file_editor("ru");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "ru"), Outcome::Done));
        let pv = super::super::xlsx_win::XlsxPreview::load(&mut ed.model, &book, false).unwrap();
        assert_eq!(pv.report.counts(), xlsx::Counts::default(), "{:?}", pv.report);
        assert!(pv.report.rejected.is_empty() && pv.report.conflicts.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_model_export_on_worker_rev_unchanged() {
        let ed = Editor::new_in_memory().unwrap();
        assert!(ed.model.db_path().is_none());
        let rev = ed.revision;
        let dir = std::env::temp_dir().join(format!("ddc_xl9_mem_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("b.xlsx");
        let snap = ed.content_snap();
        let job = XlsxJob::start_copy(&ed.model, &book, false, xlsx::Units::Metric, "en", snap)
            .unwrap();
        assert!(matches!(wait(&job), Outcome::Done));
        assert!(is_pro_book(&book));
        assert_eq!(ed.revision, rev);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn place_args(ed: &Editor, from: &Path, dest: &Path) -> Outcome {
        place(
            &ed.model,
            from,
            dest,
            BookId::of(dest),
            xlsx::Units::Metric,
            "en",
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicU8::new(0)),
            &Arc::new(AtomicU32::new(0)),
            &JobOpts::default(),
        )
    }

    #[test]
    fn copy_from_unreadable_book_refused_not_stripped() {
        let (ed, dir) = file_editor("copyunread");
        let book = dir.join("b.xlsx");
        std::fs::write(&book, b"locked or damaged").unwrap();
        let copy = dir.join("b (copy).xlsx");
        match place_args(&ed, &book, &copy) {
            Outcome::Failed(e) => assert!(e.contains("close it in a spreadsheet app"), "{e}"),
            _ => panic!("copy from an unreadable book must fail"),
        }
        assert!(!copy.exists());
        assert!(!part_of(&copy).exists(), "no part file left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_place_unreadable_book_untouched() {
        let (ed, dir) = file_editor("inplaceunread");
        let book = dir.join("b.xlsx");
        std::fs::write(&book, b"locked or damaged").unwrap();
        let out = place_args(&ed, &book, &book);
        assert!(matches!(out, Outcome::Failed(_)));
        assert_eq!(std::fs::read(&book).unwrap(), b"locked or damaged");
        assert!(!part_of(&book).exists(), "no part file left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn owner_beside(book: &Path) -> (PathBuf, Vec<u8>) {
        let owner = super::super::xlsx_owner::owner_file(book);
        std::fs::write(&owner, b"owner").unwrap();
        (owner, b"owner".to_vec())
    }

    #[test]
    fn export_over_book_open_elsewhere_fails_cleanly() {
        let (ed, dir) = file_editor("openelse");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        let (owner, owner_bytes) = owner_beside(&book);
        // the owner file is at least as new as the book: open with possible unsaved edits
        let f = std::fs::OpenOptions::new().write(true).open(&owner).unwrap();
        f.set_modified(web_time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
        drop(f);
        let stamp = std::fs::metadata(&owner).unwrap().modified().unwrap();
        match place_args(&ed, &book, &book) {
            Outcome::Failed(e) => assert_eq!(e, super::super::xlsx_owner::OPEN_ELSEWHERE),
            _ => panic!("a book open elsewhere must not be replaced"),
        }
        assert_eq!(std::fs::read(&book).unwrap(), before, "old book untouched");
        assert!(!part_of(&book).exists(), "no part file left");
        assert_eq!(std::fs::read(&owner).unwrap(), owner_bytes);
        assert_eq!(std::fs::metadata(&owner).unwrap().modified().unwrap(), stamp);
        // the owner file is gone: the same export goes through
        std::fs::remove_file(&owner).unwrap();
        assert!(!matches!(place_args(&ed, &book, &book), Outcome::Failed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_over_book_with_stale_owner_file_goes_through() {
        let (ed, dir) = file_editor("staleowner");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let (owner, _) = owner_beside(&book);
        let f = std::fs::OpenOptions::new().write(true).open(&owner).unwrap();
        f.set_modified(web_time::SystemTime::now() - std::time::Duration::from_secs(3600)).unwrap();
        drop(f);
        assert!(!matches!(place_args(&ed, &book, &book), Outcome::Failed(_)));
        assert!(owner.is_file(), "the owner file is not ours to remove");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn export_over_share_locked_book_fails_cleanly() {
        use std::os::windows::fs::OpenOptionsExt;
        let (ed, dir) = file_editor("sharelock");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        {
            // held like an open spreadsheet: readable, not writable or replaceable
            let _h = std::fs::OpenOptions::new().read(true).share_mode(1).open(&book).unwrap();
            match place_args(&ed, &book, &book) {
                Outcome::Failed(e) => assert_eq!(e, super::super::xlsx_owner::OPEN_ELSEWHERE),
                _ => panic!("a share-locked book must not be replaced"),
            }
        }
        assert_eq!(std::fs::read(&book).unwrap(), before);
        assert!(!part_of(&book).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn set_read_only(p: &Path, on: bool) {
        let mut perm = std::fs::metadata(p).unwrap().permissions();
        perm.set_readonly(on);
        std::fs::set_permissions(p, perm).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "m.ddc" && !n.starts_with("m.ddc-"))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn export_over_read_only_book_fails_without_writing() {
        let (ed, dir) = file_editor("readonly");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        set_read_only(&book, true);
        let names = names_in(&dir);
        let plain = export_sync(&ed.model, &book, xlsx::Units::Metric, "en");
        let placed = place_args(&ed, &book, &book);
        let ran = run_opts(&ed, &book, &JobOpts::default());
        for out in [plain, placed, ran] {
            match out {
                Outcome::Failed(e) => assert_eq!(e, super::super::xlsx_owner::READ_ONLY),
                _ => panic!("a read-only book must not be written"),
            }
        }
        assert_eq!(std::fs::read(&book).unwrap(), before, "book bytes unchanged");
        assert!(std::fs::metadata(&book).unwrap().permissions().readonly(), "attribute not cleared");
        assert_eq!(names_in(&dir), names, "no part file, backup or copy left");
        set_read_only(&book, false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_on_read_only_book_writes_a_copy() {
        let (ed, dir) = file_editor("roapply");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        set_read_only(&book, true);
        let opts = JobOpts { apply: Some(ApplyTail::default()), ..Default::default() };
        let Outcome::Copied(copy) = run_opts(&ed, &book, &opts) else {
            panic!("Apply on a read-only book must write a copy");
        };
        assert!(copy.is_file() && copy != book && copy.parent() == book.parent());
        assert!(is_pro_book(&copy));
        assert_eq!(std::fs::read(&book).unwrap(), before);
        assert!(!part_of(&book).exists() && !part_of(&copy).exists());
        set_read_only(&book, false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_to_a_copy_leaves_the_original_untouched() {
        let (ed, dir) = file_editor("tocopy");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        set_read_only(&book, true);
        let copy = dir.join("b-copy.xlsx");
        let opts = JobOpts { copy_to: Some(copy.clone()), ..Default::default() };
        match run_opts(&ed, &book, &opts) {
            Outcome::Copied(c) => assert_eq!(c, copy),
            _ => panic!("the copy must be written"),
        }
        assert!(is_pro_book(&copy));
        assert_eq!(std::fs::read(&book).unwrap(), before, "original untouched");
        assert!(std::fs::metadata(&book).unwrap().permissions().readonly());
        assert!(!part_of(&copy).exists());
        // a taken name is never overwritten
        let taken = std::fs::read(&copy).unwrap();
        match run_opts(&ed, &book, &opts) {
            Outcome::Failed(_) => {}
            _ => panic!("an existing copy must not be replaced"),
        }
        assert_eq!(std::fs::read(&copy).unwrap(), taken);
        set_read_only(&book, false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn export_to_a_copy_of_a_share_locked_book() {
        use std::os::windows::fs::OpenOptionsExt;
        let (ed, dir) = file_editor("lockcopy");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        let copy = dir.join("b-copy.xlsx");
        {
            let _h = std::fs::OpenOptions::new().read(true).share_mode(1).open(&book).unwrap();
            let plain = run_opts(&ed, &book, &JobOpts::default());
            assert!(matches!(&plain, Outcome::Failed(e) if e == super::super::xlsx_owner::OPEN_ELSEWHERE));
            let opts = JobOpts { copy_to: Some(copy.clone()), ..Default::default() };
            assert!(matches!(run_opts(&ed, &book, &opts), Outcome::Copied(c) if c == copy));
        }
        assert!(is_pro_book(&copy));
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reexport_after_apply_on_worker_book_intact_on_io_error() {
        let (ed, dir) = file_editor("ioerr");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        // a directory sits where the part file goes: the write fails, the book stays as it was
        std::fs::create_dir_all(part_of(&book)).unwrap();
        let out = run(
            &ed.model,
            &book,
            xlsx::Units::Metric,
            "en",
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(AtomicU8::new(0)),
            &Arc::new(AtomicU32::new(0)),
            &JobOpts::default(),
        );
        assert!(matches!(out, Outcome::Failed(_)));
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_tail_job_runs_on_worker_and_replaces_book() {
        let (ed, dir) = file_editor("tail");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let opts = JobOpts { kept: vec![], apply: Some(ApplyTail { created: 1, matched: 0, kept_any: false, skip: false, ids_in_place: false }), scope: None, accept_losses: vec![], copy_to: None };
        let snap = ed.content_snap();
        let job = XlsxJob::start_for(&ed.model, &book, true, xlsx::Units::Metric, "en", snap, opts)
            .unwrap();
        assert_eq!(job.apply.as_ref().map(|a| a.created), Some(1));
        assert!(matches!(wait(&job), Outcome::Done));
        assert!(is_pro_book(&book));
        assert!(!part_of(&book).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_without_book_offers_export() {
        assert_eq!(sync_action(false, None), SyncAction::OfferExport);
        assert_eq!(
            sync_action(false, Some((PathBuf::from("Z:/gone/none.xlsx"), false))),
            SyncAction::Missing(PathBuf::from("Z:/gone/none.xlsx"))
        );
        assert_eq!(sync_action(true, None), SyncAction::Reload);
    }

    /// Copy of `src` with a chart sheet `name` added to the package, as a user would add one.
    fn add_chart_sheet(src: &Path, dst: &Path, name: &str) {
        use std::io::{Read, Write};
        let mut a = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
        let mut w = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for i in 0..a.len() {
            let mut f = a.by_index(i).unwrap();
            let n = f.name().unwrap().into_owned();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            let edit = |from: &str, to: &str| {
                let t = String::from_utf8(b.clone()).unwrap();
                t.replacen(from, &format!("{to}{from}"), 1).into_bytes()
            };
            let b = match n.as_str() {
                "xl/workbook.xml" => edit(
                    "</sheets>",
                    &format!("<sheet name=\"{name}\" sheetId=\"900\" r:id=\"rIdChartSheet\"/>"),
                ),
                "xl/_rels/workbook.xml.rels" => edit(
                    "</Relationships>",
                    "<Relationship Id=\"rIdChartSheet\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet\" Target=\"chartsheets/sheet1.xml\"/>",
                ),
                "[Content_Types].xml" => edit(
                    "</Types>",
                    "<Override PartName=\"/xl/chartsheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml\"/>",
                ),
                _ => b,
            };
            w.start_file(n, o).unwrap();
            w.write_all(&b).unwrap();
        }
        w.start_file("xl/chartsheets/sheet1.xml", o).unwrap();
        w.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><chartsheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"/>").unwrap();
        w.finish().unwrap();
    }

    fn book_with_chart(ed: &Editor, dir: &Path) -> PathBuf {
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let with = dir.join("with_chart.xlsx");
        add_chart_sheet(&book, &with, "Chart1");
        std::fs::rename(&with, &book).unwrap();
        book
    }

    #[test]
    fn chart_sheet_does_not_block_reexport() {
        let (ed, dir) = file_editor("chart_ok");
        let book = book_with_chart(&ed, &dir);
        let out = export_sync(&ed.model, &book, xlsx::Units::Metric, "en");
        assert!(matches!(out, Outcome::Dropped { .. }), "re-export is not refused");
        let n = xlsx::sheet_names(&book).unwrap();
        assert!(n.iter().any(|s| s == "Info"), "{n:?}");
        assert!(!part_of(&book).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chart_sheet_named_in_outcome() {
        let (ed, dir) = file_editor("chart_named");
        let book = book_with_chart(&ed, &dir);
        let Outcome::Dropped { names, old, .. } = export_sync(&ed.model, &book, xlsx::Units::Metric, "en") else {
            panic!("a dropped sheet must be reported");
        };
        assert_eq!(names, vec!["Chart1".to_string()]);
        let note = dropped_note(false, &names, &old);
        assert!(note.contains("'Chart1'") && note.contains("b.xlsx.bak"), "{note}");
        let (o, n) = (Outcome::Dropped { names, lost: 0, old }).split_note();
        assert!(matches!(o, Outcome::Done) && n.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_book_kept_when_sheet_dropped() {
        let (ed, dir) = file_editor("chart_bak");
        let book = book_with_chart(&ed, &dir);
        let before = std::fs::read(&book).unwrap();
        let Outcome::Dropped { old, .. } = export_sync(&ed.model, &book, xlsx::Units::Metric, "en") else {
            panic!("a dropped sheet must be reported");
        };
        assert_eq!(old, dir.join("b.xlsx.bak"));
        assert_eq!(std::fs::read(&old).unwrap(), before, "the old book is moved aside byte for byte");
        assert!(xlsx::sheet_names(&old).unwrap().iter().any(|s| s == "Chart1"));
        assert!(!xlsx::sheet_names(&book).unwrap().iter().any(|s| s == "Chart1"));
        // a second book with a chart never overwrites the first backup
        let again = dir.join("with_chart.xlsx");
        add_chart_sheet(&book, &again, "Chart2");
        std::fs::rename(&again, &book).unwrap();
        let Outcome::Dropped { old: second, .. } = export_sync(&ed.model, &book, xlsx::Units::Metric, "en") else {
            panic!("a dropped sheet must be reported");
        };
        assert_eq!(second, dir.join("b.xlsx.2.bak"));
        assert!(old.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_book_reexport_unchanged() {
        let (ed, dir) = file_editor("chart_plain");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        assert!(!dir.join("b.xlsx.bak").exists(), "nothing is moved aside without a dropped sheet");
        let _ = std::fs::remove_dir_all(&dir);
    }

    type Grid = Vec<Vec<calamine::Data>>;

    fn read_grid(p: &Path, sheet: &str) -> Grid {
        use calamine::Reader;
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(p).unwrap();
        wb.worksheet_range(sheet).unwrap().rows().map(|r| r.to_vec()).collect()
    }

    /// Copy of `src` where `f` may change each sheet grid; values only, as a user's edit would leave them.
    fn rewrite(src: &Path, dst: &Path, f: impl Fn(&str, &mut Grid)) {
        use calamine::{Data, Reader};
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(src).unwrap();
        let mut out = rust_xlsxwriter::Workbook::new();
        for name in wb.sheet_names() {
            let mut g: Grid = wb.worksheet_range(&name).unwrap().rows().map(|r| r.to_vec()).collect();
            f(&name, &mut g);
            let ws = out.add_worksheet();
            ws.set_name(&name).unwrap();
            for (i, row) in g.iter().enumerate() {
                for (j, d) in row.iter().enumerate() {
                    let (i, j) = (i as u32, j as u16);
                    match d {
                        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
            }
        }
        out.save(dst).unwrap();
    }

    /// Column of the canonical header `h`; every export writes English captions since 55e95e93.
    fn head(g: &Grid, h: &str) -> usize {
        let key = if h == "type_id" { "wtype" } else { h };
        let cap = super::super::xlsx_style::style("en").captions.get(key).cloned();
        g[0].iter()
            .position(|d| {
                let s = d.to_string();
                s == h || cap.as_deref() == Some(s.as_str())
            })
            .unwrap_or_else(|| panic!("no column {h} in {:?}", g[0]))
    }

    /// Grid row whose `h` cell holds the number `v`; with `pos`, also its layer position.
    fn row_of(g: &Grid, h: &str, v: i64, pos: Option<i64>) -> usize {
        let num = |d: &calamine::Data| d.to_string().parse::<f64>().unwrap_or(f64::NAN);
        (1..g.len())
            .find(|&r| {
                num(&g[r][head(g, h)]) == v as f64
                    && pos.is_none_or(|p| num(&g[r][head(g, "pos")]) == p as f64)
            })
            .unwrap()
    }

    /// Adds the column "my note" with `cells` (grid row, text).
    fn note_col(g: &mut Grid, cells: &[(usize, &str)]) {
        g[0].push(calamine::Data::String("my note".into()));
        let c = g[0].len() - 1;
        for (r, t) in cells {
            while g[*r].len() <= c {
                g[*r].push(calamine::Data::Empty);
            }
            g[*r][c] = calamine::Data::String((*t).into());
        }
    }

    fn notes_of(book: &Path, sheet: &str) -> Vec<String> {
        let g = read_grid(book, sheet);
        let c = head(&g, "my note");
        g[1..].iter().filter_map(|r| r.get(c)).map(|d| d.to_string()).filter(|s| !s.is_empty()).collect()
    }

    fn lay(m: &str, t: f64) -> ddc_model::LayerSpec {
        ddc_model::LayerSpec { material: m.into(), thickness: t }
    }

    /// Book of an editor with level "L2" carrying the user note "roof level" in a column of its own.
    fn book_with_level_note(tag: &str) -> (Editor, PathBuf, PathBuf, i64) {
        let (mut ed, dir) = file_editor(tag);
        let l2 = ed.model.add_level("L2", 3.0).unwrap();
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let tmp = dir.join("edit.xlsx");
        rewrite(&book, &tmp, |sh, g| {
            if sh == "Levels" {
                let r = row_of(g, "id", l2, None);
                note_col(g, &[(r, "roof level")]);
            }
        });
        std::fs::rename(&tmp, &book).unwrap();
        (ed, dir, book, l2)
    }

    fn run_job(ed: &Editor, book: &Path) -> Outcome {
        let snap = ed.content_snap();
        let opts = JobOpts::default();
        let job = XlsxJob::start_for(&ed.model, book, true, xlsx::Units::Metric, "en", snap, opts)
            .unwrap();
        wait(&job)
    }

    #[test]
    fn lost_cells_reported_from_worker_export() {
        let (mut ed, dir) = file_editor("lost_w");
        let wt = ed.model.add_wall_type("ext", &[lay("brick", 0.2), lay("wool", 0.1)]).unwrap();
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let tmp = dir.join("edit.xlsx");
        rewrite(&book, &tmp, |sh, g| {
            if sh == "Layers" {
                let (a, b) = (row_of(g, "type_id", wt, Some(0)), row_of(g, "type_id", wt, Some(1)));
                note_col(g, &[(a, "outer"), (b, "inner")]);
            }
        });
        std::fs::rename(&tmp, &book).unwrap();
        ed.model
            .sql()
            .execute("DELETE FROM wall_type_layer WHERE wtype=?1 AND pos=1", [wt])
            .unwrap();
        // a removed layer of a type that keeps layers has no row of its own to take the note:
        // the worker asks first (a loss item) and the book stays as it was
        let before = std::fs::read(&book).unwrap();
        let Outcome::Failed(e) = run_job(&ed, &book) else {
            panic!("a lost user cell must be reported by the worker");
        };
        let items = xlsx::loss_items(&e).unwrap_or_else(|| panic!("not a loss question: {e}"));
        assert!(items.iter().any(|i| i.contains("'ext'") && i.contains("Layers")), "{items:?}");
        assert_eq!(std::fs::read(&book).unwrap(), before, "a refused export writes nothing");
        // confirmed: the old book goes aside, the new one keeps the note of the layer left
        let snap = ed.content_snap();
        let opts = JobOpts { accept_losses: items, ..JobOpts::default() };
        let job = XlsxJob::start_for(&ed.model, &book, true, xlsx::Units::Metric, "en", snap, opts)
            .unwrap();
        let Outcome::Dropped { old, .. } = wait(&job) else {
            panic!("a confirmed loss keeps the old book aside");
        };
        assert_eq!(old, dir.join("b.xlsx.bak"));
        assert_eq!(notes_of(&book, "Layers"), vec!["outer".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lost_cells_keep_old_book_in_archive() {
        let (mut ed, dir, book, l2) = book_with_level_note("lost_bak");
        let before = std::fs::read(&book).unwrap();
        ed.model.sql().execute("DELETE FROM level WHERE id=?1", [l2]).unwrap();
        let Outcome::Dropped { old, lost, .. } = run_job(&ed, &book) else {
            panic!("a lost user cell must be reported");
        };
        assert_eq!(lost, 1);
        assert_eq!(old, dir.join("b.xlsx.bak"));
        assert_eq!(std::fs::read(&old).unwrap(), before, "the old book is moved aside byte for byte");
        assert_eq!(notes_of(&old, "Levels"), vec!["roof level".to_string()]);
        assert!(notes_of(&book, "Levels").is_empty(), "the new book holds no stray cell");
        // the cell is gone from the new book, so the next export has nothing left to report
        assert!(matches!(run_job(&ed, &book), Outcome::Done));
        assert!(!dir.join("b.xlsx.2.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleted_level_user_cells_reported() {
        let (mut ed, dir, book, l2) = book_with_level_note("lost_lv");
        let l3 = ed.model.add_level("L3", 6.0).unwrap();
        // the book knows L3 only after an export; give it a note of its own
        assert!(matches!(run_job(&ed, &book), Outcome::Done));
        let tmp = dir.join("edit2.xlsx");
        rewrite(&book, &tmp, |sh, g| {
            if sh == "Levels" {
                let c = head(g, "my note");
                let r = row_of(g, "id", l3, None);
                while g[r].len() <= c {
                    g[r].push(calamine::Data::Empty);
                }
                g[r][c] = calamine::Data::String("attic".into());
            }
        });
        std::fs::rename(&tmp, &book).unwrap();
        ed.model.sql().execute("DELETE FROM level WHERE id=?1", [l2]).unwrap();
        let Outcome::Dropped { lost, .. } = run_job(&ed, &book) else {
            panic!("the note of the deleted level must be reported");
        };
        assert_eq!(lost, 1);
        assert_eq!(notes_of(&book, "Levels"), vec!["attic".to_string()], "L3 keeps its own note");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_lost_cells_no_note() {
        let (ed, dir, book, _) = book_with_level_note("lost_none");
        let out = run_job(&ed, &book);
        assert!(matches!(out, Outcome::Done), "every row still exists");
        assert_eq!(notes_of(&book, "Levels"), vec!["roof level".to_string()]);
        assert!(!dir.join("b.xlsx.bak").exists(), "nothing is moved aside without a loss");
        let (o, n) = (Outcome::Dropped { names: vec![], lost: 0, old: dir.join("x.bak") }).split_note();
        // a book moved aside for confirmed losses alone still names where it went
        assert!(matches!(o, Outcome::Done) && n.unwrap().text(false).contains("x.bak"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Copy of `src` with one more package part, as a macro-enabled save would add.
    fn add_part(src: &Path, dst: &Path, part: &str) {
        use std::io::{Read, Write};
        let mut a = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
        let mut w = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for i in 0..a.len() {
            let mut f = a.by_index(i).unwrap();
            let n = f.name().unwrap().into_owned();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            w.start_file(n, o).unwrap();
            w.write_all(&b).unwrap();
        }
        w.start_file(part, o).unwrap();
        w.write_all(b"vba").unwrap();
        w.finish().unwrap();
    }

    fn run_opts(ed: &Editor, book: &Path, opts: &JobOpts) -> Outcome {
        let (c, s, p) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU8::new(0)), Arc::new(AtomicU32::new(0)));
        run(&ed.model, book, xlsx::Units::Metric, "en", &c, &s, &p, opts)
    }

    #[test]
    fn reexport_that_drops_parts_is_refused_until_confirmed() {
        let (ed, dir) = file_editor("loss");
        let book = dir.join("b.xlsx");
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let with = dir.join("with_vba.xlsx");
        add_part(&book, &with, "xl/vbaProject.bin");
        std::fs::rename(&with, &book).unwrap();
        let before = std::fs::read(&book).unwrap();
        let Outcome::Failed(e) = export_sync(&ed.model, &book, xlsx::Units::Metric, "en") else {
            panic!("a re-export that drops macros must be refused");
        };
        let items = xlsx::loss_items(&e).unwrap_or_else(|| panic!("{e}"));
        assert_eq!(items, vec!["macros of the workbook".to_string()]);
        assert_eq!(std::fs::read(&book).unwrap(), before, "a refused re-export leaves the book byte for byte");
        assert!(!part_of(&book).exists());
        // another list than the one found is no confirmation
        let other = JobOpts { accept_losses: vec!["charts on sheet 'Walls'".into()], ..Default::default() };
        assert!(matches!(run_opts(&ed, &book, &other), Outcome::Failed(_)));
        assert_eq!(std::fs::read(&book).unwrap(), before);
        // confirmed: written, the old book kept whole beside it
        let ok = JobOpts { accept_losses: items, ..Default::default() };
        let Outcome::Dropped { names, lost, old } = run_opts(&ed, &book, &ok) else {
            panic!("a confirmed re-export keeps the old book aside");
        };
        assert!(names.is_empty() && lost == 0);
        assert_eq!(std::fs::read(&old).unwrap(), before);
        assert!(xlsx::sheet_names(&book).unwrap().iter().any(|s| s == "Info"));
        assert!(Note { names, lost, old }.text(false).contains("b.xlsx.bak"));
        // the new book has nothing more to drop
        assert!(matches!(export_sync(&ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removed_row_cells_and_confirmed_loss_share_one_bak() {
        let (ed, dir, book, l2) = book_with_level_note("loss_lost");
        let with = dir.join("with_vba.xlsx");
        add_part(&book, &with, "xl/vbaProject.bin");
        std::fs::rename(&with, &book).unwrap();
        let before = std::fs::read(&book).unwrap();
        ed.model.sql().execute("DELETE FROM level WHERE id=?1", [l2]).unwrap();
        // the note of the removed level is reported, not asked about: only the macros are listed
        let Outcome::Failed(e) = run_opts(&ed, &book, &JobOpts::default()) else {
            panic!("the macros must still be refused");
        };
        let items = xlsx::loss_items(&e).unwrap_or_else(|| panic!("{e}"));
        assert_eq!(items, vec!["macros of the workbook".to_string()]);
        assert_eq!(std::fs::read(&book).unwrap(), before);
        assert!(!dir.join("b.xlsx.bak").exists(), "a refused export moves nothing aside");
        let ok = JobOpts { accept_losses: items, ..Default::default() };
        let Outcome::Dropped { names, lost, old } = run_opts(&ed, &book, &ok) else {
            panic!("confirmed: written, the old book aside");
        };
        assert_eq!((names.len(), lost), (0, 1));
        assert_eq!(old, dir.join("b.xlsx.bak"));
        assert_eq!(std::fs::read(&old).unwrap(), before, "the one copy aside is the old book whole");
        assert!(!dir.join("b.xlsx.2.bak").exists(), "one export, one old book aside");
        let text = Note { names, lost, old }.text(false);
        assert!(text.contains("1 user cells") && text.contains("b.xlsx.bak"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn earlier_bak_of_the_user_is_never_overwritten() {
        let (ed, dir, book, l2) = book_with_level_note("loss_bak_free");
        let mine = dir.join("b.xlsx.bak");
        std::fs::write(&mine, b"user copy").unwrap();
        ed.model.sql().execute("DELETE FROM level WHERE id=?1", [l2]).unwrap();
        let Outcome::Dropped { old, lost, .. } = run_job(&ed, &book) else {
            panic!("a lost user cell must be reported");
        };
        assert_eq!(lost, 1);
        assert_eq!(old, dir.join("b.xlsx.2.bak"));
        assert_eq!(std::fs::read(&mine).unwrap(), b"user copy".to_vec());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reexport_after_apply_never_asks_but_writes_a_copy() {
        // after Apply (a confirmed mass deletion included) a loss is not asked about a second
        // time: the old book stays as it is and the ids go to a copy beside it
        let (ed, dir, book, _) = book_with_level_note("loss_apply");
        let with = dir.join("with_vba.xlsx");
        add_part(&book, &with, "xl/vbaProject.bin");
        std::fs::rename(&with, &book).unwrap();
        let before = std::fs::read(&book).unwrap();
        let opts = JobOpts { apply: Some(ApplyTail::default()), ..Default::default() };
        let Outcome::Copied(copy) = run_opts(&ed, &book, &opts) else {
            panic!("a refused re-export after Apply goes to a copy");
        };
        assert_eq!(std::fs::read(&book).unwrap(), before);
        assert!(copy.exists() && copy != book);
        assert_eq!(notes_of(&copy, "Levels"), vec!["roof level".to_string()], "user columns reach the copy");
        assert!(!dir.join("b.xlsx.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs `f` once between the write of the new book and its move into place.
    fn on_mid_write(f: impl FnOnce() + 'static) {
        MID_WRITE.with(|m| *m.borrow_mut() = Some(Box::new(f)));
    }

    /// The user's note of level `l2` becomes `text`, saved over `book` as a spreadsheet program would.
    fn set_note(book: &Path, l2: i64, text: &str) {
        let tmp = book.with_file_name("saved.xlsx");
        let text = text.to_string();
        rewrite(book, &tmp, |sh, g| {
            if sh == "Levels" {
                let (c, r) = (head(g, "my note"), row_of(g, "id", l2, None));
                g[r][c] = calamine::Data::String(text.clone());
            }
        });
        std::fs::rename(&tmp, book).unwrap();
    }

    #[test]
    fn export_refuses_when_book_saved_during_write() {
        let (ed, dir, book, l2) = book_with_level_note("mid_save");
        // a plain export starts over from the book as it is now: the newer note is carried
        let b = book.clone();
        on_mid_write(move || set_note(&b, l2, "saved meanwhile"));
        assert!(matches!(run_opts(&ed, &book, &JobOpts::default()), Outcome::Done));
        assert_eq!(notes_of(&book, "Levels"), vec!["saved meanwhile".to_string()]);
        assert!(!part_of(&book).exists());
        // after Apply the newer book is never overwritten: the ids go to a copy made from it
        let b = book.clone();
        on_mid_write(move || set_note(&b, l2, "saved again"));
        let opts = JobOpts { apply: Some(ApplyTail::default()), ..Default::default() };
        let Outcome::Copied(copy) = run_opts(&ed, &book, &opts) else {
            panic!("a book saved during the re-export after Apply must not be replaced");
        };
        assert_eq!(notes_of(&book, "Levels"), vec!["saved again".to_string()]);
        assert_eq!(notes_of(&copy, "Levels"), vec!["saved again".to_string()]);
        assert!(!part_of(&book).exists() && !dir.join("b.xlsx.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_never_overwrites_book_that_appeared() {
        let (ed, dir, book, _) = book_with_level_note("appeared");
        let saved = dir.join("user.xlsx");
        // missing at the start (the program's save swaps files), back before the move into place
        std::fs::rename(&book, &saved).unwrap();
        let (b, s) = (book.clone(), saved.clone());
        on_mid_write(move || std::fs::rename(&s, &b).unwrap());
        assert!(matches!(run_opts(&ed, &book, &JobOpts::default()), Outcome::Done));
        assert_eq!(notes_of(&book, "Levels"), vec!["roof level".to_string()], "the user's column is carried");
        // after Apply the book that appeared is left byte for byte
        let user = std::fs::read(&book).unwrap();
        std::fs::rename(&book, &saved).unwrap();
        let (b, s) = (book.clone(), saved.clone());
        on_mid_write(move || std::fs::rename(&s, &b).unwrap());
        let opts = JobOpts { apply: Some(ApplyTail::default()), ..Default::default() };
        let Outcome::Copied(copy) = run_opts(&ed, &book, &opts) else {
            panic!("a book that appeared during the re-export after Apply must not be replaced");
        };
        assert_eq!(std::fs::read(&book).unwrap(), user);
        assert_eq!(notes_of(&copy, "Levels"), vec!["roof level".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn book_changed_text_is_translated() {
        let ru = crate::xl_text::tr(BOOK_CHANGED, crate::xl_text::Lang::Ru);
        assert_ne!(ru, BOOK_CHANGED);
        assert_eq!(crate::xl_text::tr(BOOK_CHANGED, crate::xl_text::Lang::En), BOOK_CHANGED);
    }

    /// `book` with two rows above the header of the Levels sheet: a title and a note.
    fn with_titles(book: &Path) {
        use calamine::Data;
        let tmp = book.with_file_name("titled.xlsx");
        rewrite(book, &tmp, |sh, g| {
            if sh == "Levels" {
                g.insert(0, vec![Data::String("Tower A".into())]);
                g.insert(1, vec![Data::Empty, Data::String("issued for review".into())]);
            }
        });
        std::fs::rename(&tmp, book).unwrap();
    }

    #[test]
    fn title_row_above_header_keeps_user_columns() {
        let (mut ed, dir, book, l2) = book_with_level_note("titles");
        with_titles(&book);
        assert!(matches!(run_job(&ed, &book), Outcome::Done), "nothing is lost");
        let g = read_grid(&book, "Levels");
        assert_eq!(g[0][0].to_string(), "Tower A");
        assert_eq!(g[1][1].to_string(), "issued for review");
        // the header follows the rows above it, the user's column keeps its cell
        let h = g.iter().position(|r| r.iter().any(|d| d.to_string() == "my note")).unwrap();
        assert_eq!(h, 2);
        let c = g[h].iter().position(|d| d.to_string() == "my note").unwrap();
        let r = (h + 1..g.len()).find(|&r| g[r][0].to_string().parse::<f64>().ok() == Some(l2 as f64)).unwrap();
        assert_eq!(g[r][c].to_string(), "roof level");
        // the new book reads back with no change and is re-exported the same way
        let pv = super::super::xlsx_win::XlsxPreview::load(&mut ed.model, &book, false).unwrap();
        assert_eq!(pv.report.counts(), xlsx::Counts::default(), "{:?}", pv.report);
        assert!(pv.report.rejected.is_empty(), "{:?}", pv.report.rejected);
        assert!(matches!(run_job(&ed, &book), Outcome::Done));
        assert_eq!(read_grid(&book, "Levels")[0][0].to_string(), "Tower A");
        assert!(!dir.join("b.xlsx.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_rows_lost_are_listed() {
        let (ed, dir, book, _) = book_with_level_note("titles_lost");
        with_titles(&book);
        // a book written without the old one holds neither the rows above the header nor the note
        let fresh = dir.join("fresh.xlsx");
        assert!(matches!(export_sync(&ed.model, &fresh, xlsx::Units::Metric, "en"), Outcome::Done));
        let l = xlsx::reexport_losses(&book, &fresh, &crate::elem::sheets::families(), 0).unwrap();
        assert!(l.contains(&"2 cells above the header on sheet 'Levels'".to_string()), "{l:?}");
        assert!(l.contains(&"1 cells of column 'my note' on sheet 'Levels'".to_string()), "{l:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_with_duplicate_row_writes_copy() {
        // a copied row repeating an id is ignored by the import; the re-export must not drop it
        let (ed, dir, book, l2) = book_with_level_note("loss_dup_row");
        let tmp = dir.join("edit.xlsx");
        rewrite(&book, &tmp, |sh, g| {
            if sh == "Levels" {
                let r = row_of(g, "id", l2, None);
                let copy = g[r].clone();
                g.push(copy);
            }
        });
        std::fs::rename(&tmp, &book).unwrap();
        let before = std::fs::read(&book).unwrap();
        let Outcome::Failed(e) = export_sync(&ed.model, &book, xlsx::Units::Metric, "en") else {
            panic!("a re-export that drops a repeated row must be refused");
        };
        let items = xlsx::loss_items(&e).unwrap_or_else(|| panic!("{e}"));
        assert!(items.iter().any(|i| i == "1 rows with a repeated id on sheet 'Levels'"), "{items:?}");
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let opts = JobOpts { apply: Some(ApplyTail::default()), ..Default::default() };
        let Outcome::Copied(copy) = run_opts(&ed, &book, &opts) else {
            panic!("after Apply the refused re-export goes to a copy");
        };
        assert_eq!(std::fs::read(&book).unwrap(), before, "the repeated row stays in the book");
        assert!(copy.exists() && copy != book);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crashed_export_reports_and_leaves_the_model_alone() {
        let (ed, dir) = file_editor("crash");
        let hash = ed.model.content_hash().unwrap();
        let book = dir.join("b.xlsx");
        crate::jobs::inject_panic("xl-export");
        let snap = ed.content_snap();
        let job = XlsxJob::start(db(&ed), &book, false, xlsx::Units::Metric, "en", snap);
        let Outcome::Failed(e) = wait(&job) else {
            panic!("a crash must be a failure");
        };
        assert!(crate::jobs::is_internal_error(&e), "{e}");
        assert!(!book.exists());
        assert_eq!(ed.model.content_hash().unwrap(), hash);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod crash_tests {
    use super::super::xlsx_load::XlsxLoad;
    use crate::editor::Editor;
    use std::time::Duration;

    #[test]
    fn a_crashed_load_reports_and_leaves_the_model_alone() {
        let ed = Editor::new_in_memory().unwrap();
        let hash = ed.model.content_hash().unwrap();
        let book = std::env::temp_dir().join(format!("rl1_none_{}.xlsx", std::process::id()));
        crate::jobs::inject_panic("xl-load");
        let job = XlsxLoad::start(&ed.model, &book, false, ed.content_snap()).unwrap();
        for _ in 0..400 {
            if let Some(r) = job.poll() {
                let e = r.err().expect("a crash must be a failure");
                assert!(crate::jobs::is_internal_error(&e), "{e}");
                assert_eq!(ed.model.content_hash().unwrap(), hash);
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the load did not report");
    }
}

#[cfg(test)]
#[path = "xlsx_job_verify_tests.rs"]
mod verify_tests;
