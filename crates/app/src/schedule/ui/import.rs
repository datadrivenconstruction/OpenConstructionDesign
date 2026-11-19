//! Schedule import flow: the book is read on a worker ("Reading workbook..." with Cancel), once,
//! and only its key sheet and this schedule's sheet; the UI thread plans it and shows the
//! preview. A book saved again after the preview is read anew before anything is applied.

use super::*;
use crate::jobs::{Job, Poll, Progress};
use crate::schedule::xlsx_in::{self, Key};
use ddc_io::xlsx::{ReadSheet, Stamp};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Most planned cells the preview lists before "... N more".
pub(super) const LIST_CAP: usize = 200;

/// What the worker read: the file stamp taken before the read, the key sheet, the data sheet.
pub(super) struct Book {
    stamp: Option<Stamp>,
    key: Option<ReadSheet>,
    data: Option<ReadSheet>,
}

/// A read in flight.
pub(super) struct Reading {
    pub(super) path: PathBuf,
    job: Job<Book>,
    /// Started by Apply on a book saved again: the new preview says so.
    resaved: bool,
}

/// A finished read waiting for the action handler, which holds the editor to plan it.
pub(super) struct Done {
    path: PathBuf,
    book: Result<Book, String>,
    resaved: bool,
}

/// Sheet names the schedule's sheet may carry: this language's title first, then both (a book
/// exported in the other UI language). Ids are per category, so no other sheet is ever read.
fn sheet_names(tb: &Table) -> Vec<String> {
    let short = |s: String| -> String { s.chars().take(31).collect() };
    let mut out = vec![short(tb.cat.title())];
    for ru in [false, true] {
        let n = short(tb.cat.title_in(ru));
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// The worker: one open of the book, two sheets at most, Cancel asked between them.
fn read_book(path: &Path, names: Vec<String>, c: &Arc<Progress>) -> Result<Book, String> {
    c.phase("sched_import.reading", 0.0, 1.0);
    c.set_total(2);
    let stamp = Stamp::of(path);
    let groups = vec![vec![ddc_io::xlsx::KEY_SHEET.to_string()], names];
    let asked = std::cell::Cell::new(0u64);
    let stop = || {
        c.set(asked.get());
        asked.set(asked.get() + 1);
        c.is_cancelled()
    };
    let mut got = ddc_io::xlsx::read_first_of(path, &groups, &stop)?;
    c.set(2);
    let data = got.pop().flatten();
    let key = got.pop().flatten();
    Ok(Book { stamp, key, data })
}

impl View {
    /// Starts reading `path` off the UI thread; a read already running is cancelled.
    fn start_read(&mut self, path: &Path, names: Vec<String>, resaved: bool) {
        self.cancel_read();
        self.read_done = None;
        let p = path.to_path_buf();
        let job = Job::spawn("job.title.import", move |c: &Arc<Progress>| read_book(&p, names, c));
        self.reading = Some(Reading { path: path.to_path_buf(), job, resaved });
    }

    /// Cancel: the worker's result is dropped, no preview opens.
    pub fn cancel_read(&mut self) {
        if let Some(r) = self.reading.take() {
            r.job.cancel();
        }
    }

    pub fn is_reading(&self) -> bool {
        self.reading.is_some()
    }

    /// A finished read comes back as `Action::Import` of its path, so the handler, which
    /// holds the editor, plans it ([`View::import_read`] takes the read instead of starting one).
    pub(super) fn poll_read(&mut self, acts: &mut Vec<Action>) {
        let Some(r) = self.reading.as_mut() else {
            return;
        };
        let book = match r.job.poll() {
            Poll::Running => return,
            Poll::Done(b) => Ok(b),
            Poll::Failed(e) => Err(e),
            Poll::Cancelled => {
                self.reading = None;
                return;
            }
        };
        if let Some(r) = self.reading.take() {
            acts.push(Action::Import(r.path.clone()));
            self.read_done = Some(Done { path: r.path, book, resaved: r.resaved });
        }
    }

    /// A picked book starts its read; a finished read of it opens the preview. `Some` is a
    /// status to show.
    pub fn import_read(
        &mut self,
        ed: &Editor,
        path: &Path,
        u: Units,
    ) -> Option<(String, bool)> {
        let Some(names) = self.table.as_ref().map(sheet_names) else {
            return Some((crate::tr("sched_import.no_table", &[]), true));
        };
        let Some(done) = self.read_done.take().filter(|d| d.path == path) else {
            self.import = None;
            self.start_read(path, names, false);
            return None;
        };
        let failed = |e: &str| Some((crate::tr("sched_import.read_failed", &[("e", e)]), true));
        let book = match done.book {
            Ok(b) => b,
            Err(e) => return failed(&e),
        };
        let Some(sheet) = book.data else {
            return failed(&crate::tr("sched_import.wrong_sheet", &[("sheet", &names[0])]));
        };
        let key = Key::of_sheet(book.key.as_ref()).map(|k| {
            let a = self.applied.get(&(book_id(path), k.project));
            k.with_applied(a)
        });
        let tb = self.table.as_ref()?;
        match xlsx_in::plan_sheet(ed, tb, u, &sheet, key.as_ref()) {
            Ok(plan) => {
                self.import = Some(Pending {
                    path: path.to_path_buf(),
                    sheet,
                    plan,
                    key,
                    stamp: book.stamp,
                    note: done.resaved.then(|| crate::tr("sched_import.resaved", &[])),
                });
                None
            }
            Err(e) => failed(&e),
        }
    }

    /// Write the previewed book, planned again on the current table: one op = one undo step.
    /// A book saved again since the preview is never applied: it is read anew and previewed.
    pub fn import_apply(&mut self, ed: &mut Editor, u: Units) -> (String, bool) {
        let Some(p) = self.import.take() else {
            return (crate::tr("sched_edit.nothing", &[]), true);
        };
        let Some(tb) = self.table.as_ref() else {
            return (crate::tr("sched_import.no_table", &[]), true);
        };
        if ddc_io::xlsx::book_changed(p.stamp, Stamp::of(&p.path)) {
            let names = sheet_names(tb);
            self.start_read(&p.path, names, true);
            return (crate::tr("sched_import.resaved_reading", &[]), true);
        }
        match xlsx_in::plan_sheet(ed, tb, u, &p.sheet, p.key.as_ref()) {
            Ok(plan) => {
                let r = xlsx_in::apply_say(ed, tb, &plan, p.plan.changed());
                // the book file itself is left as the user saved it
                if let Some(k) = &p.key {
                    self.applied.entry((book_id(&p.path), k.project)).or_default().record(&plan);
                }
                r
            }
            Err(e) => (crate::tr("sched_import.read_failed", &[("e", &e)]), true),
        }
    }

    /// "Reading workbook..." with a bar and Cancel while the worker reads.
    pub(super) fn reading_window(&mut self, ctx: &egui::Context) {
        let Some(r) = self.reading.as_ref() else {
            return;
        };
        let (mut open, mut cancel) = (true, false);
        let file = file_name(&r.path);
        let frac = r.job.fraction();
        crate::ui_kit::Dialog::new(
            "sched_import.reading",
            crate::tr("sched_import.preview_title", &[]),
            crate::ui_kit::DialogSize::Form,
        )
        .id(egui::Id::new("sch3_reading"))
        .open(&mut open)
        .show(ctx, |ui| {
            crate::ui_kit::elided(ui, &file);
            let text = crate::tr("sched_import.reading", &[]);
            cancel = crate::ui_kit::progress_row(ui, frac, &text, crate::tr("sched_import.cancel", &[]));
        });
        if cancel || !open {
            self.cancel_read();
        }
    }

    /// The preview: counts, then every planned cell (element, column, before -> after) and
    /// every refused cell with its address; rows without Id are named.
    pub(super) fn import_window(&mut self, ctx: &egui::Context, acts: &mut Vec<Action>) {
        let Some(p) = self.import.as_ref() else {
            return;
        };
        let (mut open, mut apply, mut cancel) = (true, false, false);
        let th = ddc_ui::theme::of(ctx);
        crate::ui_kit::Dialog::new(
            "sched_import.import",
            crate::tr("sched_import.preview_title", &[]),
            crate::ui_kit::DialogSize::Wide,
        )
        .id(egui::Id::new("sch3_import"))
        .open(&mut open)
        .show(ctx, |ui| {
            let file = file_name(&p.path);
            ui.label(crate::tr("sched_import.file", &[("file", &file), ("sheet", &p.plan.sheet)]));
            ddc_ui::widgets::menu_divider(ui, &th);
            if let Some(n) = &p.note {
                crate::ui_kit::warn_note(ui, n.clone());
            }
            if let Some(w) = &p.plan.warn {
                ui.add(egui::Label::new(egui::RichText::new(w).weak()).wrap());
            }
            for line in p.plan.count_lines() {
                ui.label(line);
            }
            if let Some(line) = p.plan.no_id_line() {
                crate::ui_kit::warn_note(ui, line);
            }
            if !p.plan.ignored_cols.is_empty() {
                ui.label(crate::tr(
                    "sched_import.ignored_cols",
                    &[("list", &p.plan.ignored_cols.join(", "))],
                ));
            }
            let rows = list_rows(&p.plan);
            if !rows.is_empty() {
                ddc_ui::widgets::menu_divider(ui, &th);
                egui::ScrollArea::vertical()
                    .id_salt("sch3_list")
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (kind, text) in &rows {
                            ui.horizontal(|ui| {
                                if let Some(k) = kind {
                                    let tip = crate::tr(k.label_key(), &[]);
                                    let b = ddc_ui::diff_kind::badge(ui, *k, k.color(&th));
                                    b.on_hover_text(tip);
                                }
                                ui.add(egui::Label::new(text.as_str()).wrap());
                            });
                        }
                    });
            }
            crate::ui_kit::dialog_footer(ui, |ui| {
                let n = p.plan.changed();
                let label = crate::tr("sched_import.apply", &[("n", &n.to_string())]);
                if n > 0 {
                    apply = crate::ui_kit::primary_action(ui, label, None);
                } else {
                    crate::ui_kit::primary_button(ui, label, false);
                }
                if crate::ui_kit::cancel_button(ui, crate::tr("sched_import.cancel", &[])).clicked() {
                    cancel = true;
                }
            });
        });
        if apply {
            acts.push(Action::ImportApply);
        } else if cancel || !open {
            self.import = None;
        }
    }

    /// Test hook: the import windows as `show` draws them.
    #[cfg(test)]
    pub(crate) fn import_frame(&mut self, ctx: &egui::Context) {
        let mut acts = vec![];
        self.reading_window(ctx);
        self.import_window(ctx, &mut acts);
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Lines of the preview list: planned cells (capped, then "... N more"), then refused cells.
pub(super) fn list_rows(p: &xlsx_in::Plan) -> Vec<(Option<ddc_ui::diff_kind::DiffKind>, String)> {
    use ddc_ui::diff_kind::DiffKind;
    let mut out: Vec<(Option<DiffKind>, String)> = p
        .changes
        .iter()
        .take(LIST_CAP)
        .map(|c| (Some(DiffKind::Changed), c.line(&p.sheet)))
        .collect();
    if p.changes.len() > LIST_CAP {
        let more = (p.changes.len() - LIST_CAP).to_string();
        out.push((None, crate::tr("sched_import.more", &[("n", &more)])));
    }
    for r in &p.refusals {
        let k = match r.why {
            xlsx_in::Why::Conflict { .. } => DiffKind::Conflict,
            _ => DiffKind::Refused,
        };
        out.push((Some(k), r.line(&p.sheet)));
    }
    out
}
