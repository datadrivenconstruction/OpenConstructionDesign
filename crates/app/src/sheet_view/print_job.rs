//! Batch printing as a job: assembly runs one sheet per frame (it needs `&Editor`), files and
//! compression run on a worker thread. The queue between them is short, so at most a couple of
//! sheets are held in memory. A failed sheet does not stop the batch; a file appears whole or not at all.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    channel, sync_channel, Receiver, Sender, SyncSender, TryRecvError, TrySendError,
};
use web_time::{Duration, Instant};

use ddc_io::pdf::PdfBook;

use super::*;

/// Sheets queued for the worker thread.
const QUEUE: usize = 2;
/// How many failure and warning lines the report shows.
const REPORT_LINES: usize = 12;

/// Sheet to print and its files; `pos` is its number in the issue for the "Sheet" field.
pub(super) struct OutItem {
    id: i64,
    pos: usize,
    label: String,
    pdf: Option<PathBuf>,
    svg: Option<PathBuf>,
}

/// Where the issue is written. Built before writing so that file replacement can be confirmed.
pub(super) struct OutPlan {
    items: Vec<OutItem>,
    /// Shared multi-page PDF.
    single: Option<PathBuf>,
    dir: PathBuf,
    total: usize,
    failures: Vec<String>,
}

pub(super) fn plan_outputs(ed: &Editor, p: &Print, ids: &[i64], target: &Path) -> OutPlan {
    let one_file = p.pdf && !p.separate;
    let dir = if !one_file || target.is_dir() {
        target.to_path_buf()
    } else {
        target.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    // names that become identical after sanitising ("A/1" and "A_1", a template without {number}) do not overwrite each other
    let mut used = std::collections::HashSet::new();
    let mut items = Vec::with_capacity(ids.len());
    let mut failures = vec![];
    for (k, id) in ids.iter().enumerate() {
        let def = match ed.model.sheet(*id) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("#{id}: {e}"));
                continue;
            }
        };
        let mut path = |ext: &str| {
            let name = io::file_name(&p.pattern, &def.number, &def.name, ext);
            dir.join(io::unique_name(name, &mut used))
        };
        items.push(OutItem {
            id: *id,
            pos: k + 1,
            label: format!("{} {}", def.number, def.name),
            pdf: (p.pdf && p.separate).then(|| path("pdf")),
            svg: p.svg.then(|| path("svg")),
        });
    }
    OutPlan {
        items,
        single: one_file.then(|| target.to_path_buf()),
        dir,
        total: ids.len(),
        failures,
    }
}

impl OutPlan {
    /// Files the issue will replace. The shared PDF is not included: the save dialog already asked about it.
    pub(super) fn existing(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .flat_map(|i| i.pdf.iter().chain(&i.svg))
            .filter(|p| p.exists())
            .cloned()
            .collect()
    }
}

fn temp_beside(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "print".into(), |n| n.to_string_lossy());
    target.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

/// Write via a temporary file next to the target: the previous file stays intact on any failure.
#[cfg(test)]
pub(super) fn write_atomic(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage(target, bytes)?;
    let renamed = std::fs::rename(&tmp, target);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    renamed
}

/// The whole byte buffer goes to a temporary file next to the target; on failure no temporary file is left.
fn stage(target: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let tmp = temp_beside(target);
    let written = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()
    })();
    match written {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Like `write_atomic`, but a file that cannot be replaced (open in a viewer) does not lose
/// the work: it is written next to it under a free name, and that path is returned.
fn write_or_beside(target: &Path, bytes: &[u8]) -> std::io::Result<Option<PathBuf>> {
    let tmp = stage(target, bytes)?;
    let Err(first) = std::fs::rename(&tmp, target) else {
        return Ok(None);
    };
    // a directory target is not a busy file: a name next to it does not work around it
    if !target.is_dir() {
        let other = free_name(target);
        if std::fs::rename(&tmp, &other).is_ok() {
            return Ok(Some(other));
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(first)
}

use crate::io_kind::{io_kind, IoKind};

/// Write-failure text in the UI language; an unknown cause is kept as the system text.
fn io_text(ru: bool, kind: IoKind, raw: &str) -> String {
    let key = match kind {
        IoKind::Locked => "sheets.print.err_locked",
        IoKind::Denied => "sheets.print.err_denied",
        IoKind::Full => "sheets.print.err_full",
        IoKind::Missing | IoKind::Offline => "sheets.print.err_missing",
        IoKind::TooLong | IoKind::Other => return raw.to_string(),
    };
    msg(ru, key, &[])
}

/// Failure of a single file: the language is chosen when the report is shown, not on the worker thread.
fn io_failure(label: &str, e: &std::io::Error) -> Event {
    Event::IoFailed {
        label: label.to_string(),
        kind: io_kind(e),
        raw: e.to_string(),
    }
}

enum Failure {
    Text(String),
    Io {
        label: String,
        kind: IoKind,
        raw: String,
    },
}

impl Failure {
    fn text(&self, ru: bool) -> String {
        match self {
            Failure::Text(t) => t.clone(),
            Failure::Io { label, kind, raw } => format!("{label}: {}", io_text(ru, *kind, raw)),
        }
    }
}

/// Free name next to a file that cannot be replaced (e.g. open in a viewer).
fn free_name(target: &Path) -> PathBuf {
    let stem = target
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = target
        .extension()
        .map_or_else(String::new, |e| format!(".{}", e.to_string_lossy()));
    (1..)
        .map(|k| target.with_file_name(format!("{stem}-{k}{ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| target.to_path_buf())
}

struct Work {
    label: String,
    drawing: io::SheetDrawing,
    pdf: Option<PathBuf>,
    svg: Option<PathBuf>,
}

enum ToWorker {
    Sheet(Box<Work>),
    Finish,
}

enum Event {
    Sheet { ok: bool },
    File,
    Failed(String),
    IoFailed {
        label: String,
        kind: IoKind,
        raw: String,
    },
    SavedAs(PathBuf),
    Finished { cancelled: bool },
}

/// Shared PDF: written to a temporary file page by page, renamed at the end.
struct Book {
    tmp: PathBuf,
    target: PathBuf,
    pdf: PdfBook<BufWriter<File>>,
}

impl Book {
    fn open(target: PathBuf) -> Result<Book, String> {
        let tmp = temp_beside(&target);
        let f = File::create(&tmp).map_err(|e| e.to_string())?;
        let pdf = PdfBook::new(BufWriter::new(f)).map_err(|e| e.to_string());
        match pdf {
            Ok(pdf) => Ok(Book { tmp, target, pdf }),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    fn discard(self) {
        let Book { tmp, pdf, .. } = self;
        drop(pdf);
        let _ = std::fs::remove_file(tmp);
    }

    fn finish(self, ev: &Sender<Event>) {
        if self.pdf.page_count() == 0 {
            return self.discard();
        }
        let Book { tmp, target, pdf } = self;
        let closed = pdf.finish().map_err(|e| e.to_string()).and_then(|w| {
            let f = w.into_inner().map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())
        });
        if let Err(e) = closed {
            let _ = std::fs::remove_file(&tmp);
            let _ = ev.send(Event::Failed(format!("{}: {e}", target.display())));
            return;
        }
        let Err(first) = std::fs::rename(&tmp, &target) else {
            let _ = ev.send(Event::File);
            return;
        };
        // the work of the whole issue is not lost because of a file open in a viewer
        let other = free_name(&target);
        if std::fs::rename(&tmp, &other).is_err() {
            let _ = std::fs::remove_file(&tmp);
            let _ = ev.send(io_failure(&target.display().to_string(), &first));
            return;
        }
        let _ = ev.send(Event::File);
        let _ = ev.send(Event::SavedAs(other));
    }
}

fn write_sheet(w: &Work, book: &mut Option<Book>, ev: &Sender<Event>) {
    let ok = std::cell::Cell::new(true);
    let fail = |e: String| {
        ok.set(false);
        let _ = ev.send(Event::Failed(format!("{}: {e}", w.label)));
    };
    // sheet file: one held by a viewer goes under a free name next to it, a failure goes to the report
    let put = |path: &Path, bytes: &[u8]| match write_or_beside(path, bytes) {
        Ok(moved) => {
            let _ = ev.send(Event::File);
            if let Some(p) = moved {
                let _ = ev.send(Event::SavedAs(p));
            }
        }
        Err(e) => {
            ok.set(false);
            let _ = ev.send(io_failure(&w.label, &e));
        }
    };
    if w.pdf.is_some() || w.svg.is_some() || book.is_some() {
        let page = w.drawing.to_pdf_sheet();
        if let Some(path) = &w.svg {
            let o = ddc_io::svg::Options {
                title: w.label.clone(),
                ..Default::default()
            };
            match ddc_io::svg::write(&page, &o) {
                Ok(s) => put(path, s.as_bytes()),
                Err(e) => fail(e),
            }
        }
        if let Some(path) = &w.pdf {
            match io::to_pdf(std::slice::from_ref(&page)) {
                Ok(b) => put(path, &b),
                Err(e) => fail(e.0),
            }
        }
        let mut broken = false;
        if let Some(b) = book.as_mut() {
            if let Err(e) = b.pdf.add_page(&page) {
                fail(e.to_string());
                broken = b.pdf.is_broken();
            }
        }
        if broken {
            if let Some(b) = book.take() {
                b.discard();
            }
        }
    }
    let _ = ev.send(Event::Sheet { ok: ok.get() });
}

fn worker(
    rx: Receiver<ToWorker>,
    ev: Sender<Event>,
    cancel: Arc<AtomicBool>,
    single: Option<PathBuf>,
) {
    let mut book = None;
    if let Some(target) = single {
        let shown = target.display().to_string();
        match Book::open(target) {
            Ok(b) => book = Some(b),
            Err(e) => drop(ev.send(Event::Failed(format!("{shown}: {e}")))),
        }
    }
    let cancelled = loop {
        match rx.recv() {
            Ok(_) if cancel.load(Ordering::Relaxed) => break true,
            Ok(ToWorker::Sheet(w)) => write_sheet(&w, &mut book, &ev),
            Ok(ToWorker::Finish) => break false,
            Err(_) => break true,
        }
    };
    if let Some(b) = book {
        if cancelled {
            b.discard();
        } else {
            b.finish(&ev);
        }
    }
    let _ = ev.send(Event::Finished { cancelled });
}

/// Issue result; the text is built in the UI language.
pub(super) struct Outcome {
    pub(super) files: usize,
    pub(super) cancelled: bool,
    sheets: usize,
    replaced: usize,
    failures: Vec<Failure>,
    warnings: Vec<String>,
    /// Files held by a viewer: written under a different name.
    saved_as: Vec<PathBuf>,
    dir: PathBuf,
}

fn lines(out: &mut String, title: String, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(&title);
    for l in items.iter().take(REPORT_LINES) {
        out.push('\n');
        out.push_str(l);
    }
    if items.len() > REPORT_LINES {
        out.push_str("\n…");
    }
}

impl Outcome {
    /// Files written under another name because the target was held open.
    pub(super) fn saved_as(&self) -> &[PathBuf] {
        &self.saved_as
    }

    pub(super) fn clean(&self) -> bool {
        self.failures.is_empty() && self.warnings.is_empty() && self.saved_as.is_empty()
    }

    /// First line of the report.
    pub(super) fn headline(&self, ru: bool) -> String {
        let (n, files, dir) = (
            self.sheets.to_string(),
            self.files.to_string(),
            self.dir.display().to_string(),
        );
        let mut s = msg(
            ru,
            "sheets.print.done",
            &[("n", &n), ("files", &files), ("dir", &dir)],
        );
        if self.replaced > 0 && !self.cancelled {
            let k = self.replaced.to_string();
            s += &msg(ru, "sheets.print.replaced", &[("n", &k)]);
        }
        if self.cancelled {
            s += &msg(ru, "sheets.print.cancelled", &[]);
        }
        s
    }

    pub(super) fn report(&self, ru: bool) -> String {
        let mut s = self.headline(ru);
        let count = |key: &str, n: usize| msg(ru, key, &[("n", &n.to_string())]);
        for p in self.saved_as.iter().take(REPORT_LINES) {
            let shown = p.display().to_string();
            s.push('\n');
            s += &msg(ru, "sheets.print.saved_as", &[("path", &shown)]);
        }
        let failures: Vec<String> = self.failures.iter().map(|f| f.text(ru)).collect();
        lines(
            &mut s,
            count("sheets.print.failed", failures.len()),
            &failures,
        );
        lines(
            &mut s,
            count("sheets.print.warnings", self.warnings.len()),
            &self.warnings,
        );
        s
    }
}

pub(super) struct PrintJob {
    items: Vec<OutItem>,
    next: usize,
    total: usize,
    paper: Option<String>,
    landscape: bool,
    pending: Option<ToWorker>,
    tx: Option<SyncSender<ToWorker>>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    /// The query covers all sheets: one per issue while the model does not change.
    refs: Option<(u64, io::SheetRefs)>,
    finish_sent: bool,
    finished: bool,
    /// Sheets processed.
    done: usize,
    current: String,
    out: Outcome,
}

impl PrintJob {
    pub(super) fn start(plan: OutPlan, paper: Option<String>, landscape: bool) -> PrintJob {
        let replaced = plan.existing().len();
        let OutPlan {
            items,
            single,
            dir,
            total,
            failures,
        } = plan;
        let (tx, work) = sync_channel(QUEUE);
        let (ev, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let ev_panic = ev.clone();
        // writes the PDF files only; the model is not touched
        let spawned = crate::jobs::spawn_guarded(
            "sheet-print",
            move || worker(work, ev, flag, single),
            move |msg| {
                let _ = ev_panic.send(Event::Failed(msg));
                let _ = ev_panic.send(Event::Finished { cancelled: true });
            },
        );
        let mut out = Outcome {
            files: 0,
            cancelled: false,
            sheets: 0,
            replaced,
            failures: failures.into_iter().map(Failure::Text).collect(),
            warnings: vec![],
            saved_as: vec![],
            dir,
        };
        let finished = match spawned {
            Ok(_) => false,
            Err(e) => {
                out.failures.push(Failure::Text(e.to_string()));
                true
            }
        };
        PrintJob {
            items,
            next: 0,
            total,
            paper,
            landscape,
            pending: None,
            tx: Some(tx),
            rx,
            cancel,
            refs: None,
            finish_sent: false,
            finished,
            done: 0,
            current: String::new(),
            out,
        }
    }

    /// (processed, total, sheet in progress).
    pub(super) fn progress(&self) -> (usize, usize, &str) {
        (self.done, self.items.len(), &self.current)
    }

    pub(super) fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The issue is cancelled; the worker thread removes the temporary file and reports completion.
    pub(super) fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.pending = None;
        self.tx = None;
    }

    pub(super) fn into_outcome(self) -> Outcome {
        self.out
    }

    fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Event::Sheet { ok }) => {
                    self.done += 1;
                    self.out.sheets += usize::from(ok);
                }
                Ok(Event::File) => self.out.files += 1,
                Ok(Event::Failed(e)) => self.out.failures.push(Failure::Text(e)),
                Ok(Event::IoFailed { label, kind, raw }) => {
                    self.out.failures.push(Failure::Io { label, kind, raw });
                }
                Ok(Event::SavedAs(p)) => self.out.saved_as.push(p),
                Ok(Event::Finished { cancelled }) => {
                    self.out.cancelled = cancelled;
                    self.finished = true;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if !self.finished {
                        self.out.failures.push(Failure::Text(crate::jobs::internal_error()));
                        self.finished = true;
                    }
                    return;
                }
            }
        }
    }

    /// The queue is free: the deferred message was sent or there is none.
    fn flush_pending(&mut self) -> bool {
        let (Some(m), Some(tx)) = (self.pending.take(), &self.tx) else {
            return true;
        };
        match tx.try_send(m) {
            Ok(()) => true,
            Err(TrySendError::Full(m)) => {
                self.pending = Some(m);
                false
            }
            // the thread has finished: `drain` will report the cause
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn compose_next(&mut self, ed: &Editor, ru: bool) {
        let item = &self.items[self.next];
        self.next += 1;
        self.current.clone_from(&item.label);
        if self.refs.as_ref().is_none_or(|r| r.0 != ed.revision) {
            self.refs = io::SheetRefs::load(&ed.model)
                .ok()
                .map(|r| (ed.revision, r));
        }
        let refs = self.refs.as_ref().map(|r| &r.1);
        let paper = self.paper.as_deref().map(|s| (s, self.landscape));
        let index = Some((item.pos, self.total));
        let composed = with_ctx_refs(ed, ru, index, paper, refs, |ctx| {
            io::print_composed(&ed.model, item.id, ctx)
        });
        match composed {
            Ok(c) => {
                let warned = c.warnings.iter().map(|w| format!("{}: {w}", item.label));
                self.out.warnings.extend(warned);
                self.pending = Some(ToWorker::Sheet(Box::new(Work {
                    label: item.label.clone(),
                    drawing: c.drawing,
                    pdf: item.pdf.clone(),
                    svg: item.svg.clone(),
                })));
            }
            Err(e) => {
                self.out.failures.push(Failure::Text(format!("{}: {}", item.label, e.0)));
                self.done += 1;
            }
        }
    }

    /// One step per frame: at least one sheet if the queue is free, then more until `budget` runs out.
    /// Returns true when the issue is finished.
    pub(super) fn step(&mut self, ed: &Editor, ru: bool, budget: Duration) -> bool {
        self.drain();
        if self.finished || self.is_cancelling() {
            return self.finished;
        }
        let t0 = Instant::now();
        let mut composed = false;
        while self.flush_pending() {
            if self.next >= self.items.len() {
                if self.finish_sent {
                    break;
                }
                self.finish_sent = true;
                self.pending = Some(ToWorker::Finish);
                continue;
            }
            if composed && t0.elapsed() >= budget {
                break;
            }
            self.compose_next(ed, ru);
            composed = true;
        }
        self.drain();
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ddc_print_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn only_temp_free(dir: &Path) {
        for e in std::fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.ends_with(".tmp"), "{name}");
        }
    }

    fn two_sheets() -> (Editor, i64, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let a = ed.model.add_sheet(&SheetDef::new("A-101", "a")).unwrap();
        let b = ed.model.add_sheet(&SheetDef::new("A-102", "b")).unwrap();
        (ed, a, b)
    }

    #[test]
    fn atomic_write_keeps_the_old_file_on_failure() {
        let dir = temp("atomic");
        let f = dir.join("x.pdf");
        write_atomic(&f, b"one").unwrap();
        write_atomic(&f, b"two").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two");
        // the target is occupied by a directory: renaming fails, no temporary file is left
        let d = dir.join("busy.pdf");
        std::fs::create_dir_all(&d).unwrap();
        assert!(write_atomic(&d, b"x").is_err());
        only_temp_free(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_lists_the_files_it_would_replace() {
        let (ed, a, b) = two_sheets();
        let dir = temp("plan");
        let mut p = Print::new(None);
        p.separate = true;
        let plan = plan_outputs(&ed, &p, &[a, b], &dir);
        assert!(plan.existing().is_empty());
        std::fs::write(dir.join("A-102 - b.pdf"), b"old").unwrap();
        let plan = plan_outputs(&ed, &p, &[a, b], &dir);
        assert_eq!(plan.existing(), vec![dir.join("A-102 - b.pdf")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn svg_per_sheet_with_or_without_the_pdf() {
        let (ed, a, b) = two_sheets();
        let dir = temp("svg");
        let mut p = Print::new(None);
        p.pdf = false;
        p.svg = true;
        let report = print_to(&ed, true, &p, &[a, b], &dir).unwrap();
        assert!(report.contains(&crate::app2_fx::fx("print_files_2")), "{report}");
        for n in ["A-101 - a.svg", "A-102 - b.svg"] {
            let t = std::fs::read_to_string(dir.join(n)).unwrap();
            assert!(t.starts_with("<?xml") && t.contains("<svg") && t.trim_end().ends_with("</svg>"), "{n}");
        }
        assert!(!dir.join("A-101 - a.pdf").exists());
        // with the set's PDF: the SVGs land beside it and count as replaced files
        p.pdf = true;
        let one = dir.join("set.pdf");
        let plan = plan_outputs(&ed, &p, &[a, b], &one);
        assert_eq!(plan.existing().len(), 2);
        let report = print_to(&ed, true, &p, &[a, b], &one).unwrap();
        assert!(one.is_file(), "{report}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_sheet_does_not_stop_the_set_and_is_reported() {
        let (ed, a, b) = two_sheets();
        let dir = temp("partial");
        let mut p = Print::new(None);
        p.separate = true;
        let report = print_to(&ed, true, &p, &[a, 999_999, b], &dir).unwrap();
        assert!(report.contains(&crate::app2_fx::fx("print_files_2")), "{report}");
        assert!(report.contains("999999"), "{report}");
        assert!(dir.join("A-101 - a.pdf").is_file());
        assert!(dir.join("A-102 - b.pdf").is_file());

        // write failure of one sheet: the target is a directory, the other sheets are still issued
        std::fs::remove_file(dir.join("A-101 - a.pdf")).unwrap();
        std::fs::create_dir_all(dir.join("A-101 - a.pdf")).unwrap();
        let report = print_to(&ed, true, &p, &[a, b], &dir).unwrap();
        assert!(report.contains("A-101 a"), "{report}");
        assert!(report.contains(&crate::app2_fx::fx("print_files_1")), "{report}");
        only_temp_free(&dir);

        // shared PDF: without the failed sheet, but with the rest
        p.separate = false;
        let one = dir.join("set.pdf");
        print_to(&ed, true, &p, &[a, 999_999, b], &one).unwrap();
        let bytes = std::fs::read(&one).unwrap();
        let pdf = ddc_io::pdf::import::PdfFile::parse(&bytes).unwrap();
        assert_eq!(pdf.page_count(), 2);
        only_temp_free(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_written_is_an_error_with_the_reason() {
        let (ed, _, _) = two_sheets();
        let dir = temp("none");
        let p = Print::new(None);
        let e = print_to(&ed, true, &p, &[999_999], &dir.join("x.pdf")).unwrap_err();
        assert!(e.contains("999999"), "{e}");
        assert!(!dir.join("x.pdf").exists());
        only_temp_free(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_leaves_no_partial_pdf() {
        let (ed, a, b) = two_sheets();
        let dir = temp("cancel");
        let target = dir.join("set.pdf");
        let p = Print::new(None);
        let plan = plan_outputs(&ed, &p, &[a, b], &target);
        let mut job = PrintJob::start(plan, None, true);
        job.cancel();
        assert!(job.is_cancelling());
        while !job.step(&ed, true, Duration::ZERO) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let o = job.into_outcome();
        assert!(o.cancelled);
        assert_eq!(o.files, 0);
        assert!(!target.exists());
        only_temp_free(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn locked_sheet_pdf_saved_as_free_name() {
        use std::os::windows::fs::OpenOptionsExt;
        let (ed, a, b) = two_sheets();
        let dir = temp("locked");
        let mut p = Print::new(None);
        p.separate = true;
        let locked = dir.join("A-101 - a.pdf");
        std::fs::write(&locked, b"old").unwrap();
        // opened without delete/replace sharing, as in a viewer
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&locked)
            .unwrap();
        let plan = plan_outputs(&ed, &p, &[a, b], &dir);
        let mut job = PrintJob::start(plan, None, true);
        while !job.step(&ed, true, Duration::ZERO) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let o = job.into_outcome();
        assert_eq!(o.files, 2);
        assert!(o.failures.is_empty());
        assert_eq!(o.saved_as, vec![dir.join("A-101 - a-1.pdf")]);
        assert!(dir.join("A-101 - a-1.pdf").is_file());
        assert_eq!(std::fs::read(&locked).unwrap(), b"old");
        let report = o.report(false);
        assert!(report.contains("A-101 - a-1.pdf"), "{report}");
        only_temp_free(&dir);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sheet_failure_text_is_localized_ru_en() {
        let mut cases = vec![
            (
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                IoKind::Denied,
            ),
            (
                std::io::Error::from(std::io::ErrorKind::NotFound),
                IoKind::Missing,
            ),
        ];
        #[cfg(windows)]
        cases.extend([
            (std::io::Error::from_raw_os_error(32), IoKind::Locked),
            (std::io::Error::from_raw_os_error(33), IoKind::Locked),
            (std::io::Error::from_raw_os_error(112), IoKind::Full),
        ]);
        #[cfg(unix)]
        cases.push((std::io::Error::from_raw_os_error(28), IoKind::Full));
        for (e, kind) in cases {
            assert_eq!(io_kind(&e), kind);
            let raw = e.to_string();
            let (ru, en) = (io_text(true, kind, &raw), io_text(false, kind, &raw));
            assert_ne!(ru, en);
            for t in [&ru, &en] {
                assert!(!t.contains("os error") && !t.starts_with("sheets."), "{t}");
            }
        }
        // an unknown cause is kept as the system text
        assert_eq!(io_text(false, IoKind::Other, "raw"), "raw");
        let f = Failure::Io {
            label: "A-1 x".into(),
            kind: IoKind::Locked,
            raw: "os error 32".into(),
        };
        assert!(f.text(false).starts_with("A-1 x: "));
    }

    #[test]
    fn cancel_leaves_no_tmp_files() {
        let (ed, a, b) = two_sheets();
        let dir = temp("cancel_tmp");
        let mut p = Print::new(None);
        p.separate = true;
        let plan = plan_outputs(&ed, &p, &[a, b], &dir);
        let mut job = PrintJob::start(plan, None, true);
        job.step(&ed, true, Duration::ZERO);
        job.cancel();
        while !job.step(&ed, true, Duration::ZERO) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(job.into_outcome().cancelled);
        only_temp_free(&dir);
        // a rename failure on the fallback path also leaves no temporary files
        let busy = dir.join("busy.pdf");
        std::fs::create_dir_all(&busy).unwrap();
        assert!(write_or_beside(&busy, b"x").is_err());
        only_temp_free(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_counts_every_sheet() {
        let (ed, a, b) = two_sheets();
        let dir = temp("progress");
        let mut p = Print::new(None);
        p.separate = true;
        let plan = plan_outputs(&ed, &p, &[a, b], &dir);
        let mut job = PrintJob::start(plan, None, true);
        // one step with a zero budget assembles one sheet, not the whole batch
        job.step(&ed, true, Duration::ZERO);
        assert_eq!(job.next, 1);
        while !job.step(&ed, true, Duration::ZERO) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(job.progress().0, job.progress().1);
        let o = job.into_outcome();
        assert!(o.clean() && !o.cancelled);
        assert_eq!(o.files, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_has_the_strings_sheets_print_on_paper() {
        for k in [
            "as_indicated",
            "revisions",
            "rev_mark",
            "rev_date",
            "rev_desc",
            "rev_by",
            "no_data",
            "continued",
            "rows_hidden",
            "warn_hidden",
            "warn_clipped",
            "warn_revisions",
            "sheet_list",
            "col_sheet",
            "col_name",
            "col_rev",
            "col_date",
            "lines",
            "line",
            "weight",
            "l_cut",
            "l_outline",
            "l_thin",
            "l_hatch",
            "l_dim",
            "materials",
            "material",
            "wall_types",
            "type",
            "thickness",
            "layers",
        ] {
            let key = format!("sheets.paper.{k}");
            assert_ne!(msg(true, &key, &[]), key);
            assert_ne!(msg(false, &key, &[]), key);
        }
    }
}
