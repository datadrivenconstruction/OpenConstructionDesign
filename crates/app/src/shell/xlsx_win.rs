// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Data → Export to XLSX / Import from XLSX": the model workbook and a preview of edits before
//! applying (added / changed / deleted, cell list, errors with addresses like `Sheet!A1`).
//! Live link: while the window is open the workbook is polled once a second; once it is saved
//! the preview is rebuilt, no wizard. Unknown columns become parameter suggestions.
use crate::ui_kit as kit;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use web_time::Instant;

use crate::xl_text::{self, Lang};
use ddc_io::xlsx::{self, ImportOptions, KeyDecisions, MatchState, Report, Stamp, Watch};
use egui::{self, RichText};

use super::xlsx_counts as cnt;
use super::xlsx_issues;
use super::xlsx_scope;
use super::xlsx_job::{self, XlsxJob, Outcome, SyncAction};
use super::xlsx_load::{self, XlsxLoad};
use super::EditorApp;

/// Selection for a row id of a book sheet; only sheets that hold model elements.
pub(crate) fn sheet_sel(sheet: &str, id: i64) -> Option<crate::editor::Sel> {
    use crate::editor::Sel;
    Some(match sheet {
        "Walls" => Sel::Wall(id),
        "Openings" => Sel::Opening(id),
        "Rooms" => Sel::Room(id),
        "Slabs" => Sel::Slab(id),
        "Curtain walls" => Sel::Curtain(id),
        _ => {
            return crate::elem::FAMILIES
                .iter()
                .find(|f| matches!(f.sheet, crate::elem::Cap::Yes(s) if s == sheet))
                .and_then(|f| f.sel(id))
        }
    })
}

/// Selects the element a diff row names; the model and its revision stay untouched.
pub(crate) fn pick_row(ed: &mut crate::editor::Editor, sheet: &str, id: i64) -> bool {
    match sheet_sel(sheet, id) {
        Some(s) => {
            ed.selection = vec![s];
            true
        }
        None => false,
    }
}

thread_local! {
    // the window was closed: only the path is remembered until "Synchronize"
    static LAST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn remember(path: &Path) {
    LAST.with(|l| *l.borrow_mut() = Some(path.to_path_buf()));
}

/// `remember`, and the project keeps the link in `meta` so a reopen still finds it.
fn remember_in(model: &ddc_model::Model, path: &Path) {
    remember(path);
    super::xlsx_relink::store(model, path);
}

/// Book and delete flag for "Synchronize" after the window was closed. The flag is never
/// remembered: a destructive option starts off on every reopen.
fn sync_target() -> Option<(PathBuf, bool)> {
    LAST.with(|l| l.borrow().clone()).map(|p| (p, false))
}

/// The link belongs to the open project: another project opened or created drops it, so
/// "Synchronize" never reads the previous project's book.
pub fn forget_link() {
    LAST.with(|l| *l.borrow_mut() = None);
    super::xlsx_relink::forget();
}

/// Last linked workbook (exported or read).
pub fn last_book() -> Option<PathBuf> {
    LAST.with(|l| l.borrow().clone())
}

pub struct XlsxPreview {
    pub path: PathBuf,
    pub report: Report,
    pub delete_missing: bool,
    pub only_errors: bool,
    /// Workbook link: the file stamp last taken into account.
    pub watch: Watch,
    /// Workbook matches the model (just exported or applied): waiting for its edits.
    pub synced: bool,
    /// The last read failed (often the file is still being written); report text, English key.
    pub error: Option<String>,
    /// Rows created by the last apply: they need ids from a re-export.
    pub created: usize,
    /// Model state the book was compared with; another one at Apply reads the book again.
    pub rev: xlsx_job::Snap,
    /// Decisions on rows matched by key (sheet, row); dropped when the file changes.
    pub decisions: KeyDecisions,
    /// Conflict cells where the user chose the book value over the model's.
    pub take: xlsx::Take,
    /// The book was locked at Apply: a refreshed copy beside it is linked now.
    pub copy: Option<PathBuf>,
    /// The linked book is a copy written by this session: "Show in folder" is offered.
    pub copy_made: bool,
    /// Scope the linked book was exported with; a re-export keeps it.
    pub scope: Option<xlsx::Scope>,
    /// Bumped whenever `report` is replaced; part of the label cache key.
    pub report_gen: u64,
    /// Change table: filters, sort and selected row; rebuilt whenever `report_gen` moves.
    pub diff: super::xl_diff::DiffState,
    /// The one tick that confirms deletions (`xlsx_gate`); `apply_begin` requires it for a mass
    /// deletion (`Report::mass_deletes`). Reset whenever the report is rebuilt or applied.
    pub confirm_mass: bool,
    /// The tick of a type or layer change: (report_gen, elements reached) it was given for
    /// (`xlsx_gate::types_ok`). Any other report or count needs it again.
    pub confirm_types: Option<(u64, usize)>,
    /// Explicit confirmation of a book without baseline, valid for one report only.
    pub no_base_ok: super::xlsx_no_base::Confirm,
    /// `error` comes from a failed export (Retry writes the book again), not from a read.
    pub export_failed: bool,
    /// When this window last wrote the book (export or apply); the file time is the fallback.
    pub synced_at: Option<web_time::SystemTime>,
    /// Rows of repeated ids the user turned into new elements (`decisions.reject`): one click
    /// takes the choice back.
    pub dup_new: Vec<(String, u32)>,
}

/// What the buttons under a failed read or export do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ErrAct {
    Retry,
    OpenBook,
    /// Ignore the owner file of this book for the session.
    ReadAnyway,
    /// Write the export to a new file beside a read-only or held book and link that one.
    ExportCopy,
    /// No rights in the book's folder: write the export to a file the user picks elsewhere.
    ExportElsewhere,
}

/// The failed export has a way out: a copy beside the book (read-only or held by another program).
pub fn copy_offered(error: Option<&str>, export_failed: bool) -> bool {
    export_failed
        && matches!(error, Some(e) if e == super::xlsx_owner::READ_ONLY || e == super::xlsx_owner::OPEN_ELSEWHERE)
}

/// The way-out button of a failed export: a copy beside the book, or another folder when the
/// book's folder has no write rights (a copy there would fail the same way).
pub fn way_out(error: Option<&str>, export_failed: bool) -> Option<(&'static str, ErrAct)> {
    if copy_offered(error, export_failed) {
        Some(("xlsx_readonly.export_copy", ErrAct::ExportCopy))
    } else if export_failed && error == Some(super::xlsx_owner::DENIED) {
        Some(("xlsx_write.save_to", ErrAct::ExportElsewhere))
    } else {
        None
    }
}

/// Start folder of "Save to...": the user's Documents, else home, else the temp folder.
pub(crate) fn writable_default() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    home.as_ref()
        .map(|h| h.join("Documents"))
        .filter(|d| d.is_dir())
        .or(home.filter(|h| h.is_dir()))
        .unwrap_or_else(std::env::temp_dir)
}

/// The file "Save to..." writes: none on cancel or when the pick is the book itself; `.xlsx`
/// added when the name has no extension. A taken name is refused by the job, never replaced.
pub(super) fn elsewhere_target(book: &Path, picked: Option<PathBuf>) -> Option<PathBuf> {
    let p = picked?;
    let p = if p.extension().is_none() { p.with_extension("xlsx") } else { p };
    (p != book).then_some(p)
}

/// Label keys and actions of the error line: a failed read also offers the workbook itself.
pub fn error_buttons(export_failed: bool) -> Vec<(&'static str, ErrAct)> {
    let mut v = vec![("xlw.retry_now", ErrAct::Retry)];
    if !export_failed {
        v.push(("xlw.open_workbook", ErrAct::OpenBook));
    }
    v
}

/// Success toast of Apply: a wrong Apply is one click back (`undo`: the Apply wrote a step).
/// The button takes back that Apply only (`xlsx_undo`); an older Apply's button is dropped.
pub fn done_toast(toasts: &mut ddc_ui::notify::Toasts, lang: Lang, status: &Option<String>, undo: bool) {
    use super::xlsx_undo::CMD;
    let text = status.clone().unwrap_or_else(|| l(lang, "xlw.applied").to_string());
    toasts.live.retain(|t| t.command.as_deref() != Some(CMD));
    if !undo {
        toasts.push(ddc_ui::notify::Level::Success, text, None);
        return;
    }
    let text = format!("{text} (Ctrl+Z)");
    toasts.push_cmd(ddc_ui::notify::Level::Success, text, l(lang, "xlw.undo"), CMD);
}

/// Result of "Apply".
#[derive(Debug, PartialEq)]
pub enum Applied {
    /// The model changed after the preview: the workbook was re-compared, confirm again.
    Stale,
    /// Applied; `reexported` — the workbook was exported again and new rows got ids.
    Done { reexported: bool },
}

/// "Apply": only with a report built against the current model (otherwise re-read).
/// Created rows get ids in the workbook immediately: otherwise the next workbook save would
/// create them again.
pub fn apply_preview(
    ed: &mut crate::editor::Editor,
    pv: &mut XlsxPreview,
    units: xlsx::Units,
    lang: Lang,
) -> Result<Applied, String> {
    apply_preview_in(ed, pv, units, lang, if lang == Lang::Ru { "ru" } else { "en" })
}

/// `apply_preview` with the book captions taken by locale code.
pub fn apply_preview_in(
    ed: &mut crate::editor::Editor,
    pv: &mut XlsxPreview,
    units: xlsx::Units,
    lang: Lang,
    locale: &str,
) -> Result<Applied, String> {
    let Some(opts) = apply_begin(ed, pv, lang)? else {
        return Ok(Applied::Stale);
    };
    let tail = opts.apply.clone().unwrap_or_default();
    // no UI loop here: run the same worker job and wait for it
    let outcome = if tail.skip {
        Outcome::Cancelled
    } else {
        let rev = ed.content_snap();
        match XlsxJob::start_for(&ed.model, &pv.path, true, units, locale, rev, opts) {
            Ok(job) => loop {
                if let Some(o) = job.poll() {
                    break o;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
            Err(e) => Outcome::Failed(e),
        }
    };
    let (outcome, note) = outcome.split_note();
    if let Some(n) = note {
        ed.status = Some(n.text(lang == Lang::Ru));
    }
    let reexported = apply_finish(ed.content_snap(), &mut ed.model, pv, &tail, &outcome);
    Ok(Applied::Done { reexported })
}

/// English report text of the refusal to apply while key matches wait for a decision.
const UNDECIDED: &str = "rows matched by name wait for a decision";

/// English report text of the refusal to apply a mass deletion that is not confirmed.
const MASS_UNCONFIRMED: &str = "confirm the mass deletion first";

/// True when the report deletes more than 20 % of a sheet (or a whole one) unconfirmed.
fn mass_unconfirmed(pv: &XlsxPreview) -> bool {
    !pv.report.mass_deletes().is_empty() && !pv.confirm_mass
}

/// Applies the previewed changes to the model (one undo step) and returns what the book
/// re-export needs; None when the model changed since the preview (the book is compared again).
pub(super) fn apply_begin(
    ed: &mut crate::editor::Editor,
    pv: &mut XlsxPreview,
    lang: Lang,
) -> Result<Option<xlsx_job::JobOpts>, String> {
    apply_begin_with(ed, pv, lang, xlsx::Selected::of_model)
}

/// `read_after` reads the model once the Apply is committed; its failure never fails the Apply.
fn apply_begin_with(
    ed: &mut crate::editor::Editor,
    pv: &mut XlsxPreview,
    lang: Lang,
    read_after: impl FnOnce(&ddc_model::Model) -> Result<xlsx::Selected, String>,
) -> Result<Option<xlsx_job::JobOpts>, String> {
    // another project's book: its ids name other elements, nothing of it is applied
    if super::xlsx_project::is_foreign(pv) {
        return Err(super::xlsx_project::FOREIGN.into());
    }
    // the book is open elsewhere: the report may show a stale file
    if pv.hold().blocks() {
        return Err(super::xlsx_owner::OPEN_ELSEWHERE.into());
    }
    // unknown state counts as changed: two unknowns must not read as equal
    let now = ed.content_snap();
    if now.is_none() || pv.rev != now {
        pv.reload_keyed(&mut ed.model);
        return Ok(None);
    }
    // saved after the last read (the reload is still settling): the old report would be applied,
    // the new version marked as read and then overwritten by the re-export
    let read = pv.watch.seen;
    if xlsx::book_changed(read, Stamp::of(&pv.path)) {
        pv.reload_keyed(&mut ed.model);
        return Ok(None);
    }
    if pv.report.pending_matches() > 0 {
        return Err(UNDECIDED.into());
    }
    if mass_unconfirmed(pv) {
        return Err(MASS_UNCONFIRMED.into());
    }
    // counted afresh: a type row reaches every element of its type, inside a scope or not
    let users = crate::type_use::report_users(&ed.model, &pv.report);
    if !super::xlsx_gate::types_ok(pv.confirm_types, pv.report_gen, users) {
        return Err(super::xlsx_gate::TYPES_UNCONFIRMED.into());
    }
    if super::xlsx_no_base::unconfirmed(pv) {
        return Err(super::xlsx_no_base::UNCONFIRMED.into());
    }
    if super::xlsx_no_base::deletes_without_base(&pv.report) {
        return Err(super::xlsx_no_base::NO_BASE_DELETE.into());
    }
    // read through an alias the book is not re-exported: new rows must get their ids in place
    if !super::xlsx_alias_ids::refused_sheets(&pv.report, &pv.decisions.alias).is_empty() {
        return Err(super::xlsx_alias_ids::REFUSED.into());
    }
    let matched = pv.report.accepted_matches();
    // a selection scope keeps the rows this Apply creates, or the re-export would drop them;
    // read before the report is taken apart, so a failed read leaves the preview whole
    let before = match &pv.scope {
        Some(sc) if sc.selected.is_some() => Some(xlsx::Selected::of_model(&ed.model)?),
        _ => None,
    };
    let summary = pv.summary_in(lang);
    let created = pv.report.counts().added;
    let made_rows: Vec<(String, u32, String)> = pv
        .report
        .changes
        .iter()
        .filter(|c| c.kind() == "added")
        .map(|c| {
            let fp = pv.report.fingerprints.get(&(c.sheet.clone(), c.row)).cloned().unwrap_or_default();
            (c.sheet.clone(), c.row, fp)
        })
        .collect();
    let aliased = !pv.decisions.alias.is_empty();
    let made_kept = if aliased { made_rows.clone() } else { Vec::new() };
    let matched_rows = if aliased { super::xlsx_alias_ids::matched(&pv.report) } else { Vec::new() };
    let link = (!made_rows.is_empty())
        .then(|| std::sync::Arc::new(crate::editor::XlsxLink { book: pv.path.clone(), rows: made_rows }));
    // copied, not taken: a refused Apply leaves the preview whole for the next try
    let props = pv.report.props.clone();
    let top = ed.model.op_top().ok();
    // the written step is compared with the preview; a difference takes it back
    let planned = props.clone();
    let write = move |ed: &mut crate::editor::Editor, b| ed.apply_xlsx_linked(b, props, &summary, link);
    if let Err(r) = super::xlsx_verify::verified(ed, lang == Lang::Ru, pv.report.batch(), &planned, write) {
        let e = match r {
            super::xlsx_verify::Refused::Check(m) => return Err(m),
            super::xlsx_verify::Refused::Write(e) => e,
        };
        if top.is_none() || ed.model.op_top().ok() == top {
            return Err(e);
        }
        // the step is written (and undoable), only the view reload failed
        ed.status = Some(format!("{} {e}", l(lang, "xlw.applied_not_refreshed")));
    }
    // conflicts the user left on the model value stay in the book through the re-export
    let kept = std::mem::take(&mut pv.report.kept);
    // the base of the next read until a re-export writes a fresh baseline (`apply_finish`)
    super::xlsx_applied::record(&ed.model, &pv.path, &pv.report.applied_cells);
    // committed: nothing below may return Err, or the preview would keep its edits and a
    // second Apply would create the added rows again
    let mut skip = false;
    let mut scope_read_failed = false;
    if let (Some(before), Some(sc)) = (before, pv.scope.as_mut()) {
        match read_after(&ed.model) {
            Ok(after) => {
                if let Some(sel) = sc.selected.as_mut() {
                    sel.join_new(&before, &after);
                }
            }
            // the scope lacks the new rows: a re-export would leave them out of the book
            Err(_) => {
                skip = created > 0;
                scope_read_failed = true;
            }
        }
    }
    if scope_read_failed && !skip {
        let t = l(lang, "xlw.scope_not_extended").to_string();
        ed.status = Some(match ed.status.take() {
            Some(s) if !s.is_empty() => format!("{s} · {t}"),
            _ => t,
        });
    }
    if skip {
        let t = crate::tr_lang(lang == Lang::Ru, "xlw.new_rows_not_in_book", &[("n", &created.to_string())]);
        ed.status = Some(match ed.status.take() {
            Some(s) if !s.is_empty() => format!("{s} · {t}"),
            _ => t,
        });
    }
    // a rewrite would keep the renamed sheet as a stale user sheet beside the fresh known one,
    // and later edits typed into it would go unread: the ids go into the user's sheet in place
    let mut ids_in_place = false;
    let mut mark = read;
    if !skip && aliased {
        skip = true;
        let ru = lang == Lang::Ru;
        let r = super::xlsx_alias_ids::after_apply(&ed.model, &pv.path, read, &pv.decisions.alias, &made_kept, &matched_rows, ru);
        ids_in_place = r.in_sync;
        if r.stamp.is_some() {
            mark = r.stamp;
        }
        if let Some(t) = r.text {
            ed.status = Some(match ed.status.take() {
                Some(s) if !s.is_empty() => format!("{s} · {t}"),
                _ => t,
            });
        }
    }
    pv.report = Report::default();
    pv.report_gen += 1;
    pv.take = xlsx::Take::default();
    pv.confirm_mass = false;
    pv.synced = !skip;
    if !skip {
        pv.synced_at = Some(web_time::SystemTime::now());
    }
    pv.created = created;
    // only the version this report came from (or the ids were written into); a save during the
    // Apply stays pending
    pv.watch.mark(mark);
    crate::help::first_card::note_xlsx_applied();
    let tail = xlsx_job::ApplyTail { created, matched, kept_any: !kept.is_empty(), skip, ids_in_place };
    Ok(Some(xlsx_job::JobOpts { kept, apply: Some(tail), scope: pv.scope.clone(), accept_losses: vec![], copy_to: None }))
}

/// Where the "to check" disclosure keeps its state, and its state when nothing is kept. Quiet
/// items share one choice; blocking ones open for each new report whatever was chosen before.
fn check_slot(path: &Path, report_gen: u64, blocking: bool) -> (egui::Id, bool) {
    if blocking {
        (egui::Id::new(("xlsx.check_block", path, report_gen)), true)
    } else {
        (egui::Id::new("xlsx.check_open"), false)
    }
}

/// Saved-geometry key of the workspace preview; the idle form keeps "xlsx_preview".
pub(crate) const PREVIEW_WORK_GEOM: &str = "xlsx_preview.work";

/// Where a chip click scrolls the check area to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CheckGoto {
    Top,
    Proposals,
    Matches,
}

/// What a chip click leaves behind: the scroll target, kept only while a check area draws to
/// take it, and whether "errors only" turns off to show it (Top shows under either filter).
fn chip_goto(hit: Option<CheckGoto>, has_check: bool) -> (Option<CheckGoto>, bool) {
    match hit {
        Some(g) if has_check => (Some(g), g != CheckGoto::Top),
        _ => (None, false),
    }
}

/// Height of the check area out of `free`: the table keeps the rows it has (5 at least, 15 at
/// most) and the check area takes the rest; a long table gets the room past 8 XXL.
pub(crate) fn check_cap(free: f32, row: f32, table_rows: usize) -> f32 {
    let xxl = ddc_ui::theme::space::XXL;
    let rows = (table_rows as f32).clamp(5.0, 15.0);
    let keep = row * rows + xxl * 3.0 + SP_SM * 4.0;
    let most = if table_rows > 15 { xxl * 8.0 } else { f32::INFINITY };
    (free - keep).min(most).max(xxl * 2.0)
}

/// Width of the value count and the cell drawn after a proposal's tick.
fn proposal_tail_w(ui: &egui::Ui, vals: &str, addr: &str) -> f32 {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let col = ui.visuals().text_color();
    let p = ui.painter();
    let w = p.layout_no_wrap(vals.to_owned(), body, col).size().x
        + p.layout_no_wrap(addr.to_owned(), mono, col).size().x;
    w + ui.spacing().item_spacing.x * 2.0
}

/// Why a new column leaves rows out: their rows were refused (`None`: nothing left out).
pub(crate) fn proposal_note(p: &ddc_io::xlsx::Proposal, ru: bool) -> Option<String> {
    if p.refused.is_empty() {
        return None;
    }
    let rows: Vec<String> = p.refused.iter().map(u32::to_string).collect();
    let key = if p.blocked() { "xlw.prop_refused_all" } else { "xlw.prop_refused_some" };
    Some(crate::tr_lang(ru, key, &[("rows", &rows.join(", "))]))
}

/// Window id of the preview. The idle form and the workspace are different windows: they would
/// share one stored resize state, and a preview that gets changes would keep the small size. Each
/// class flip takes a fresh id (the first keeps the plain one) and the old position.
fn preview_window_id(ctx: &egui::Context, idle: bool) -> (egui::Id, Option<egui::Pos2>) {
    let slot = egui::Id::new("xlsx.preview_class");
    let (last, n) = ctx.data(|d| d.get_temp::<(bool, u32)>(slot)).unwrap_or((idle, 0));
    if last == idle {
        ctx.data_mut(|d| d.insert_temp(slot, (idle, n)));
        return (preview_id_n(n), None);
    }
    let pos = egui::AreaState::load(ctx, preview_id_n(n)).map(|a| a.left_top_pos());
    ctx.data_mut(|d| d.insert_temp(slot, (idle, n + 1)));
    (preview_id_n(n + 1), pos)
}

fn preview_id_n(n: u32) -> egui::Id {
    match n {
        0 => egui::Id::new(("ddc.dialog", "xlsx_preview")),
        n => egui::Id::new(("ddc.dialog", "xlsx_preview", n)),
    }
}

/// The preview window currently shown (as `preview_window_id` last chose it).
#[cfg(test)]
pub(crate) fn current_preview_id(ctx: &egui::Context) -> egui::Id {
    let c = ctx.data(|d| d.get_temp::<(bool, u32)>(egui::Id::new("xlsx.preview_class")));
    preview_id_n(c.map_or(0, |c| c.1))
}

/// The line under a disclosure closed over blocking items: its tick or decision is inside.
fn collapsed_reason(
    blocking: bool,
    open: bool,
    why: Option<&'static str>,
) -> Option<&'static str> {
    why.filter(|_| blocking && !open)
}

/// Runs the app's own bookkeeping after a worker: a snapshot still current stays current through
/// it (no false stale), one the model moved past during the job stays stale.
fn keep_snap(
    model: &ddc_model::Model,
    rev: xlsx_job::Snap,
    write: impl FnOnce(&ddc_model::Model),
) -> xlsx_job::Snap {
    let current = rev.is_some() && crate::project::content_mark(model) == rev;
    write(model);
    if current {
        crate::project::content_mark(model)
    } else {
        rev
    }
}

/// Takes the result of the re-export into the preview: a fresh book is watched at its new
/// stamp, a locked one left alone with a copy linked. True when new rows got their id.
pub(super) fn apply_finish(
    rev: xlsx_job::Snap,
    model: &mut ddc_model::Model,
    pv: &mut XlsxPreview,
    tail: &xlsx_job::ApplyTail,
    o: &Outcome,
) -> bool {
    if tail.skip {
        // no re-export ran: the book is in sync only when the ids went into it in place
        pv.rev = rev;
        pv.synced = tail.ids_in_place;
        return tail.ids_in_place;
    }
    let done = matches!(o, Outcome::Done);
    let rev = keep_snap(model, rev, |m| {
        if done {
            crate::shell::xlsx_links::clear(m, &pv.path);
        }
    });
    let fresh = match o {
        Outcome::Done => {
            pv.watch.mark(Stamp::of(&pv.path));
            super::xlsx_applied::clear(model, &pv.path);
            true
        }
        Outcome::Copied(copy) => {
            // the original was not rewritten: its created rows still have no id, so its records
            // stay and a later import of it matches them instead of creating the rows again
            pv.watch = Watch::new(copy);
            pv.path = copy.clone();
            pv.copy = Some(copy.clone());
            pv.copy_made = true;
            true
        }
        _ => false,
    };
    // new rows and rows matched by key get their id from the re-export
    let reexported = (tail.created > 0 || tail.matched > 0) && fresh;
    if reexported {
        pv.created = 0;
    }
    pv.rev = rev;
    if tail.kept_any {
        // unresolved conflicts: show them again, the book is not in sync
        if fresh {
            pv.reload(model);
        }
        pv.synced = false;
    } else if !fresh {
        pv.synced = false;
    }
    reexported
}

/// `book-synced.xlsx` beside the book (`-2`, `-3`... when taken); a copy of a copy reuses the stem.
pub(super) fn free_copy(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let mut root = stem.as_str();
    if let Some(i) = root.rfind("-synced") {
        if root[i + 7..].trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
            root = &root[..i];
        }
    }
    let dir = path.parent()?;
    (1..100).find_map(|n| {
        let tail = if n == 1 { String::new() } else { format!("-{n}") };
        let p = dir.join(format!("{root}-synced{tail}.xlsx"));
        (!p.exists()).then_some(p)
    })
}

pub(super) fn read(
    model: &mut ddc_model::Model,
    path: &Path,
    delete_missing: bool,
    take: &xlsx::Take,
    dec: &KeyDecisions,
) -> Result<Report, String> {
    read_hooked(model, path, delete_missing, take, dec, None)
}

/// `read` with a progress/cancel hook (stop = `Err(job::CANCELLED)`, model untouched).
pub(super) fn read_hooked(
    model: &mut ddc_model::Model,
    path: &Path,
    delete_missing: bool,
    take: &xlsx::Take,
    dec: &KeyDecisions,
    hook: Option<xlsx::Hook>,
) -> Result<Report, String> {
    // before the read: no report is ever computed from another project's book
    super::xlsx_project::check(model, path)?;
    let o = ImportOptions {
        apply: false,
        delete_missing,
        new_params: false,
    };
    // rows an earlier Apply created are not created again, even after a restart
    let mut take = take.clone();
    take.made = crate::shell::xlsx_links::load(model, path);
    // values applied since the book's baseline was written are its base while the model holds them
    take.applied = super::xlsx_applied::get(model, path);
    let fams = crate::elem::sheets::families();
    let mut r = xlsx::import_xlsx_families_hooked(model, path, o, &fams, &take, dec, hook)?;
    // openings and wall sizes get the checks of the panel: a refused row is listed, not applied
    crate::fit_check::screen_report(model, &mut r)?;
    Ok(r)
}

fn quiet(r: &Report) -> bool {
    r.changes.is_empty()
        && r.rejected.is_empty()
        && r.warnings.is_empty()
        && r.proposals.is_empty()
        && r.ignored.is_empty()
        && r.matches.is_empty()
        && r.conflicts.is_empty()
        && r.removed.is_empty()
        && r.sheet_issues.is_empty()
        && r.look_alikes.is_empty()
        && r.model_newer == 0
}

/// The number in `(os error N)`, whole: 5 and 50 are different errors.
fn os_error_code(low: &str) -> Option<u32> {
    let tail = &low[low.find("os error ")? + 9..];
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The read failed because another program holds the workbook (a save in progress).
pub fn is_locked(e: &str) -> bool {
    let low = e.to_lowercase();
    let by_code = os_error_code(&low).is_some_and(|c| matches!(c, 5 | 32 | 33));
    let by_text = ["used by another process", "access is denied", "permission denied"]
        .iter()
        .any(|k| low.contains(k));
    by_code || by_text
}

/// A workbook another program holds open: the OS text is no help to the user.
pub fn book_error(e: &str, lang: Lang) -> String {
    if e == super::xlsx_owner::OPEN_ELSEWHERE {
        l(lang, "xlw.open_elsewhere").to_string()
    } else if e == super::xlsx_owner::READ_ONLY {
        l(lang, "xlsx_readonly.read_only").to_string()
    } else if e == super::xlsx_owner::DENIED {
        l(lang, "xlsx_write.denied").to_string()
    } else if e == super::xlsx_owner::COPY_EXISTS {
        l(lang, "xlsx_readonly.copy_exists").to_string()
    } else if e == super::xlsx_project::FOREIGN {
        l(lang, "xlsx_project.title").to_string()
    } else if let Some(t) = super::xlsx_recover::not_restored_text(e, lang == Lang::Ru) {
        t
    } else if is_locked(e) {
        l(lang, "xlw.book_locked").to_string()
    } else {
        xl_text::tr(e, lang)
    }
}

pub(super) fn is_length(sheet: &str, column: &str) -> bool {
    xlsx::change_is_metres(sheet, column)
}

/// A length of the format's own columns (metres) in the project's units; other text as is.
pub(super) fn shown_value(sheet: &str, column: &str, v: &str) -> String {
    if !xlsx::change_is_metres(sheet, column) {
        return v.to_string();
    }
    match v.parse::<f64>() {
        Ok(m) => crate::units::display_length(m, crate::units::active_units()),
        Err(_) => v.to_string(),
    }
}

/// How a worker re-read of the open preview ended.
#[derive(Debug, PartialEq)]
pub enum Reloaded {
    Applied,
    Cancelled,
    Locked,
    Failed(String),
}

impl XlsxPreview {
    /// Parse the workbook without changing the model.
    pub fn load(
        model: &mut ddc_model::Model,
        path: &Path,
        delete_missing: bool,
    ) -> Result<Self, String> {
        let watch = Watch::new(path);
        let decisions = KeyDecisions::default();
        let take = xlsx::Take::default();
        let report = read(model, path, delete_missing, &take, &decisions)?;
        Ok(Self::from_report(path, delete_missing, report, watch, take))
    }

    /// The preview of a book already read (by the worker); `watch` was taken before the read.
    pub fn from_report(
        path: &Path,
        delete_missing: bool,
        report: Report,
        watch: Watch,
        take: xlsx::Take,
    ) -> Self {
        // a fresh read: no by-key decision is taken yet
        let decisions = KeyDecisions::default();
        let scope = xlsx::Scope::from_spec(&report.scope_spec, &report.scope, &report.base_ids);
        Self {
            path: path.to_path_buf(),
            synced: quiet(&report),
            report,
            delete_missing,
            only_errors: false,
            watch,
            error: None,
            created: 0,
            rev: None,
            decisions,
            take,
            copy: None,
            copy_made: false,
            scope,
            report_gen: 0,
            diff: Default::default(),
            confirm_mass: false,
            confirm_types: None,
            no_base_ok: Default::default(),
            export_failed: false,
            synced_at: None,
            dup_new: vec![],
        }
    }

    /// Freshly exported workbook: no edits, waiting for a save.
    pub fn linked(path: &Path, delete_missing: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            report: Report::default(),
            delete_missing,
            only_errors: false,
            watch: Watch::new(path),
            synced: true,
            error: None,
            created: 0,
            rev: None,
            decisions: KeyDecisions::default(),
            take: xlsx::Take::default(),
            copy: None,
            copy_made: false,
            scope: None,
            report_gen: 0,
            diff: Default::default(),
            confirm_mass: false,
            confirm_types: None,
            no_base_ok: Default::default(),
            export_failed: false,
            synced_at: Some(web_time::SystemTime::now()),
            dup_new: vec![],
        }
    }

    /// Rows of a repeated id are read as new elements; the caller reads the book again.
    pub fn dup_decide(&mut self, rows: Vec<(String, u32)>) {
        for k in rows {
            self.decisions.accept.remove(&k);
            self.decisions.reject.insert(k.clone());
            if !self.dup_new.contains(&k) {
                self.dup_new.push(k);
            }
        }
    }

    /// Takes back every "create as new elements" choice; false when there was none. The caller
    /// reads the book again.
    pub fn dup_undo(&mut self) -> bool {
        if self.dup_new.is_empty() {
            return false;
        }
        for k in std::mem::take(&mut self.dup_new) {
            self.decisions.reject.remove(&k);
        }
        true
    }

    /// The book is open in another program with unsaved edits (owner file) or share-locked:
    /// the preview shows the "open elsewhere" line instead of reading a stale file. The report
    /// is marked stale (Apply reads again first); the save in the other program changes the
    /// book's stamp and the poll reads it.
    pub fn hold(&mut self) -> super::xlsx_owner::Gate {
        let g = super::xlsx_owner::check(&self.path);
        if g.blocks() {
            self.error = Some(super::xlsx_owner::OPEN_ELSEWHERE.to_string());
            self.export_failed = false;
            self.rev = None;
            self.watch.mark(Stamp::of(&self.path));
        }
        g
    }

    /// The error line is the "open elsewhere" one.
    pub fn is_held(&self) -> bool {
        self.error.as_deref() == Some(super::xlsx_owner::OPEN_ELSEWHERE)
    }

    /// Accept (`true`) or reject (`false`) the key match at (sheet, row), then re-read.
    pub fn decide(&mut self, model: &mut ddc_model::Model, sheet: &str, row: u32, accept: bool) {
        let k = (sheet.to_string(), row);
        let (add, other) = if accept {
            (&mut self.decisions.accept, &mut self.decisions.reject)
        } else {
            (&mut self.decisions.reject, &mut self.decisions.accept)
        };
        other.remove(&k);
        add.insert(k);
        self.reload(model);
    }

    /// Read the sheet `found` as the known sheet `expected` in this import (`None`: back to the
    /// book's own names), then re-read. The workbook is not touched.
    pub fn read_as(&mut self, model: &mut ddc_model::Model, expected: &str, found: Option<&str>) {
        match found {
            Some(f) => self.decisions.alias.insert(expected.to_string(), f.to_string()),
            None => self.decisions.alias.remove(expected),
        };
        self.reload(model);
    }

    /// Re-read the workbook (it changed or "Synchronize"); suggestion checkboxes are preserved.
    pub fn reload(&mut self, model: &mut ddc_model::Model) {
        self.confirm_mass = false;
        if self.hold().blocks() {
            return;
        }
        let stamp = Stamp::of(&self.path);
        match read(model, &self.path, self.delete_missing, &self.take, &self.decisions) {
            Ok(mut r) => {
                for p in &mut r.proposals {
                    if let Some(old) = self
                        .report
                        .proposals
                        .iter()
                        .find(|o| o.spec.name == p.spec.name)
                    {
                        p.accept = old.accept && !p.blocked();
                    }
                }
                self.synced = quiet(&r);
                if !r.changes.is_empty() {
                    crate::help::tutorial::note_xlsx_edited();
                }
                self.report = r;
                self.report_gen += 1;
                self.error = None;
                self.export_failed = false;
            }
            Err(e) => {
                super::xlsx_project::drop_report_if_foreign(self, &e);
                self.error = Some(e);
                self.export_failed = false;
                // the old report stays on screen and the stamp is marked read: only this stops it
                self.rev = None;
            }
        }
        self.watch.mark(stamp);
    }

    /// `reload`, then the report is keyed on the model as it stands now. A read that failed left
    /// the old report in place: it stays stale (`rev` None) and Apply reads again.
    pub fn reload_keyed(&mut self, model: &mut ddc_model::Model) {
        self.reload(model);
        self.key_on(model);
    }

    fn key_on(&mut self, model: &ddc_model::Model) {
        self.rev = match self.error {
            Some(_) => None,
            None => crate::project::content_mark(model),
        };
    }

    /// The book changed since the last read and has stopped changing: time to re-read it
    /// (on the worker, see `finish_reload`).
    pub fn reload_due(&mut self, now: Instant) -> bool {
        self.watch.settled(now, Stamp::of).is_some()
    }

    /// Takes the result of a worker re-read. On success the new report replaces the old one
    /// (proposal ticks kept) as `reload` does. On cancel or a locked book the old report stays
    /// but is marked stale (`rev` None): Apply then re-reads the book instead of applying it.
    /// A locked book is not marked as seen, so the poll reads it again.
    pub fn finish_reload(
        &mut self,
        out: Result<xlsx_load::Loaded, String>,
        rev: xlsx_job::Snap,
    ) -> Reloaded {
        self.confirm_mass = false;
        match out {
            Ok((mut r, watch)) => {
                for p in &mut r.proposals {
                    if let Some(old) = self.report.proposals.iter().find(|o| o.spec.name == p.spec.name) {
                        p.accept = old.accept && !p.blocked();
                    }
                }
                self.synced = quiet(&r);
                if !r.changes.is_empty() {
                    crate::help::tutorial::note_xlsx_edited();
                }
                self.report = r;
                self.report_gen += 1;
                self.error = None;
                self.export_failed = false;
                self.rev = rev;
                self.watch.mark(watch.seen);
                Reloaded::Applied
            }
            Err(e) if xlsx_load::is_cancelled(&e) => {
                self.rev = None;
                self.watch.mark(Stamp::of(&self.path));
                Reloaded::Cancelled
            }
            Err(e) if is_locked(&e) => {
                self.rev = None;
                Reloaded::Locked
            }
            Err(e) => {
                super::xlsx_project::drop_report_if_foreign(self, &e);
                self.error = Some(e.clone());
                self.export_failed = false;
                self.watch.mark(Stamp::of(&self.path));
                Reloaded::Failed(e)
            }
        }
    }

    pub fn summary(&self) -> String {
        self.summary_in(Lang::Ru)
    }

    pub fn summary_in(&self, lang: Lang) -> String {
        let c = self.report.counts();
        let p = self.report.accepted();
        let ru = lang == Lang::Ru;
        let (a, ch, d) = (c.added.to_string(), c.changed.to_string(), c.deleted.to_string());
        let mut s = crate::tr_lang(
            ru,
            "xlw.summary",
            &[("added", a.as_str()), ("changed", ch.as_str()), ("deleted", d.as_str())],
        );
        if p > 0 {
            s += &crate::tr_lang(ru, "xlw.summary_params", &[("n", &p.to_string())]);
        }
        s
    }
}

/// Count per sheet and, for a mass deletion, a warning with the explicit confirmation.
fn mass_delete_block(ui: &mut egui::Ui, pv: &mut XlsxPreview, lang: Lang, warn: egui::Color32, sec: egui::Color32) {
    let ru = lang == Lang::Ru;
    for d in &pv.report.deletions {
        let t = crate::tr_lang(
            ru,
            "xlw.sheet_deletion",
            &[
                ("sheet", &xl_text::sheet_name(&d.sheet, lang)),
                ("n", &d.deleted.to_string()),
                ("total", &d.total.to_string()),
                ("pct", &d.percent().to_string()),
            ],
        );
        ui.label(RichText::new(t).color(if d.is_mass() { warn } else { sec }));
    }
    let list = pv
        .report
        .mass_deletes()
        .iter()
        .map(|d| format!("{} {}", xl_text::sheet_name(&d.sheet, lang), d.deleted))
        .collect::<Vec<_>>()
        .join(", ");
    if list.is_empty() {
        return;
    }
    let th = ddc_ui::theme::of(ui.ctx());
    let cards = xlsx_issues::issue_cards(&pv.report, true);
    let none = xlsx_issues::conflict_choice(&pv.report);
    xlsx_issues::show(ui, &th, ru, &cards, xlsx_issues::Slot::Mass, none, &mut pv.confirm_mass);
}

/// A click by pointer only: a focused button also reacts to Enter and Space.
fn pointer_click(clicked: bool, enter: bool, space: bool) -> bool {
    clicked && !enter && !space
}

fn apply_label(lang: Lang, changes: usize, deleted: usize) -> String {
    if deleted == 0 {
        if changes == 0 {
            return l(lang, "xlw.apply").to_string();
        }
        return super::xlsx_head::action_text(super::xlsx_head::Kind::Apply(changes), lang == Lang::Ru);
    }
    crate::tr_lang(lang == Lang::Ru, "xlw.apply_delete", &[("n", &deleted.to_string())])
}

/// Up to `max` deleted elements as "sheet #id - type - level", and how many are left out.
fn deleted_lines(model: &ddc_model::Model, report: &Report, max: usize) -> (Vec<String>, usize) {
    let ids: Vec<(&str, i64)> = report
        .changes
        .iter()
        .filter(|c| c.kind() == "deleted")
        .filter_map(|c| c.id.map(|id| (c.sheet.as_str(), id)))
        .collect();
    let lines = ids
        .iter()
        .take(max)
        .map(|&(sheet, id)| {
            let more: Option<(String, String)> = if sheet == "Walls" {
                model
                    .sql()
                    .query_row(
                        "SELECT t.name, l.name FROM wall w JOIN wall_type t ON t.id=w.wtype \
                         JOIN level l ON l.id=w.level WHERE w.id=?1",
                        [id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .ok()
            } else {
                None
            };
            match more {
                Some((t, lv)) => format!("{sheet} #{id} - {t} - {lv}"),
                None => format!("{sheet} #{id}"),
            }
        })
        .collect();
    (lines, ids.len().saturating_sub(max))
}

/// Folder and file name offered by the export dialog: next to the saved project, named after it
/// (`<stem>.xlsx`, `<stem> (imperial).xlsx`); an unsaved project gets the locale name and no folder.
fn default_book_path(
    project: Option<&Path>,
    units: crate::units::Units,
    lang: Lang,
) -> (Option<PathBuf>, String) {
    let imperial = matches!(units, crate::units::Units::Imperial);
    let stem = project
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    let Some(stem) = stem else {
        let key = if imperial { "xlw.file_imperial" } else { "xlw.file_metric" };
        return (None, l(lang, key).to_string());
    };
    let name = if imperial {
        format!("{stem} (imperial).xlsx")
    } else {
        format!("{stem}.xlsx")
    };
    let dir = project
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf);
    (dir, name)
}

fn pick(
    save: bool,
    units: crate::units::Units,
    lang: Lang,
    project: Option<&Path>,
) -> Option<PathBuf> {
    let d = crate::file_pick::Dlg::new().add_filter("XLSX", &["xlsx"]);
    if save {
        let (dir, name) = default_book_path(project, units, lang);
        let mut d = d
            .set_title(l(lang, "xlw.export_model_to_a_workbook"))
            .set_file_name(name);
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        d.save_file().map(|p| {
            if p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
            {
                p
            } else {
                p.with_extension("xlsx")
            }
        })
    } else {
        d.set_title(l(lang, "xlw.import_from_a_workbook"))
            .pick_file()
    }
}

/// Ribbon, shortcut and palette entry points of the XLSX round trip.
pub fn register(a: &mut EditorApp) {
    a.register_command("data.export_xlsx", |a| a.xlsx_export());
    a.register_command("data.import_xlsx", |a| a.xlsx_import());
    a.register_command("data.sync_xlsx", |a| a.xlsx_sync());
    a.register_command(super::xlsx_undo::CMD, super::xlsx_undo::run);
}

const SP_XXS: f32 = ddc_ui::theme::space::XXS;
const SP_XS: f32 = ddc_ui::theme::space::XS;
const SP_SM: f32 = ddc_ui::theme::space::SM;
const SP_MD: f32 = ddc_ui::theme::space::MD;
/// Toolbar and footer under the preview table before their first measure: the old estimate.
const TAIL_FIRST: f32 = 44.0 + ddc_ui::theme::space::XXL;

/// Anchor offset of the linked-workbook HUD (bottom-right): above the status bar and
/// above the toast stack that shares this corner.
fn hud_offset(bottom: f32, stack: f32) -> [f32; 2] {
    let gap = if stack > 0.0 { SP_SM } else { 0.0 };
    [-SP_MD, -(bottom + SP_SM + stack + gap)]
}

fn l(lang: Lang, key: &'static str) -> &'static str {
    crate::tr_str(lang == Lang::Ru, key)
}

/// Footer of the linked-workbook panel: reread is the one primary, export again is outlined,
/// close is quiet. Both actions are off while a job runs, with the reason on hover.
/// Returns [reread, export again, close] clicks.
fn linked_footer(ui: &mut egui::Ui, lang: Lang, busy: bool) -> [bool; 3] {
    let mut out = [false; 3];
    kit::dialog_footer(ui, |ui| {
        // Enter stays with the model: this panel sits beside the viewport
        let r = kit::primary_button(ui, l(lang, "xlw.synchronize"), !busy);
        out[0] = r.on_disabled_hover_text(l(lang, "xlw.export_running")).clicked();
        let r = ui.add_enabled_ui(!busy, |ui| kit::secondary_button(ui, l(lang, "xlw.export_again"))).inner;
        let r = r.on_hover_text(l(lang, "xlw.rewrite_the_workbook_from_the_mode"));
        out[1] = r.on_disabled_hover_text(l(lang, "xlw.export_running")).clicked();
        out[2] = kit::cancel_button(ui, l(lang, "xlw.close")).clicked();
    });
    out
}

/// "Errors only" checkbox; the missing-rows note is its own muted line below, not a trailing
/// sentence. Returns the checkbox and note rects.
fn filter_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::theme::Theme,
    lang: Lang,
    only_errors: &mut bool,
    note: Option<String>,
    no_base: bool,
) -> (egui::Rect, Option<egui::Rect>) {
    let boxed = ddc_ui::widgets::checkbox(ui, th, only_errors, l(lang, "xlw.errors_only")).rect;
    let note = note.map(|t| {
        let tip = if no_base { "xlsx_delete.no_base_reason" } else { "xlw.not_in_book_tip" };
        ui.add_space(SP_XS);
        ui.add(egui::Label::new(RichText::new(t).small().color(ui.visuals().weak_text_color())))
            .on_hover_text(l(lang, tip))
            .rect
    });
    (boxed, note)
}

/// State of the footer "delete rows missing from the workbook" checkbox.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MissingToggle {
    Hidden,
    Free { on: bool, busy: bool },
    /// No baseline: shown disabled with its reason, cannot arm.
    NoBase,
}

/// Footer tail, drawn after the action buttons in a right-to-left row, into the space left of
/// them: the checkbox at the left edge, then the muted note (elided, full text in the tooltip).
/// Some(new state) when the checkbox is flipped.
fn footer_tail(
    ui: &mut egui::Ui,
    ru: bool,
    toggle: MissingToggle,
    note: Option<(String, egui::Color32)>,
) -> Option<bool> {
    let mut flipped = None;
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        let th = ddc_ui::theme::of(ui.ctx());
        match toggle {
            MissingToggle::Hidden => {}
            MissingToggle::Free { on, busy } => {
                let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
                let key = if on { "xlw.ft_keep_missing_tip" } else { "xlw.not_in_book_tip" };
                let tip = crate::tr_str(ru, key);
                let mut v = on;
                let r = ui
                    .add_enabled_ui(!busy, |ui| ddc_ui::widgets::checkbox(ui, &th, &mut v, label))
                    .inner;
                if r.on_hover_text(tip).on_disabled_hover_text(tip).changed() {
                    flipped = Some(v);
                }
            }
            MissingToggle::NoBase => {
                super::xlsx_no_base::missing_off(ui, ru);
            }
        }
        if let Some((text, col)) = note {
            if toggle != MissingToggle::Hidden {
                ui.add_space(SP_MD);
            }
            let label = egui::Label::new(RichText::new(text.clone()).small().color(col)).truncate();
            ui.add(label).on_hover_text(text);
        }
    });
    flipped
}

/// Which footer toggle the report calls for.
fn missing_toggle(pv: &XlsxPreview, dm: bool, busy: bool) -> MissingToggle {
    if super::xlsx_no_base::off_shown(&pv.report) {
        MissingToggle::NoBase
    } else if dm || (!pv.report.no_base && pv.report.removed_count() > 0) {
        MissingToggle::Free { on: dm, busy }
    } else {
        MissingToggle::Hidden
    }
}

fn book_units(u: crate::units::Units) -> xlsx::Units {
    match u {
        crate::units::Units::Imperial => xlsx::Units::Imperial,
        crate::units::Units::Metric => xlsx::Units::Metric,
    }
}

impl EditorApp {
    fn xlang(&self) -> Lang {
        match self.sh.cat.lang {
            ddc_ui::Lang::Ru => Lang::Ru,
            _ => Lang::En,
        }
    }

    /// True (with the reason in the status line) while an export runs on the worker.
    fn xlsx_busy(&mut self) -> bool {
        let busy = self.xlsx_job.is_some() || self.xlsx_load.is_some();
        if busy {
            let lang = self.xlang();
            self.ed.status = Some(l(lang, "xlw.export_running").into());
        }
        busy
    }

    pub fn xlsx_export(&mut self) {
        if self.xlsx_busy() {
            return;
        }
        self.xlsx_offer = false;
        self.xlsx_card = Some(xlsx_scope::ScopeCard::default());
    }

    /// The card's Export button: scope, then the file dialog, then the worker.
    fn xlsx_export_scoped(&mut self, card: &mut xlsx_scope::ScopeCard) -> bool {
        let (u, lang) = (self.ed.tool.units, self.xlang());
        let scope = match xlsx_scope::build(card, &self.ed, lang == Lang::Ru) {
            Ok(s) => s,
            Err(e) => {
                card.error = Some(e);
                return false;
            }
        };
        let project = self.ed.project.path().map(Path::to_path_buf);
        let Some(path) = pick(true, u, lang, project.as_deref()) else {
            return false;
        };
        self.xlsx_export_to(&path, scope);
        true
    }

    /// Writes the book at `path`, unless it already holds edits or rows a rewrite would drop:
    /// then the card asks first.
    pub(super) fn xlsx_export_to(&mut self, path: &Path, scope: Option<xlsx::Scope>) {
        if self.xlsx_busy() {
            return;
        }
        let g = xlsx_scope::guard(&mut self.ed.model, path, scope.as_ref(), |m, p| {
            read(m, p, false, &xlsx::Take::default(), &KeyDecisions::default())
        });
        match g {
            Ok(xlsx_scope::Guard::Write) => self.xlsx_run(path, false, scope),
            Ok(xlsx_scope::Guard::Ask(c)) => self.xlsx_over = Some(c),
            // the check failed: nothing is overwritten, the book gets a new name
            Err(_) => {
                self.xlsx_over = Some(xlsx_scope::OverCard {
                    path: path.to_path_buf(),
                    scope,
                    pending: 0,
                    outside: 0,
                    ours: false,
                    name: xlsx_scope::free_name(path),
                })
            }
        }
    }

    /// Replace: the old book moves to `_archive/` beside it, then the export writes the name.
    pub(super) fn xlsx_replace(&mut self, c: xlsx_scope::OverCard) {
        let lang = self.xlang();
        match xlsx_scope::archive_book(&c.path) {
            Ok(old) => {
                self.xlsx_run(&c.path, false, c.scope);
                let t = crate::tr_lang(lang == Lang::Ru, "xlsx_exists.archived", &[("path", &old.display().to_string())]);
                self.add_status(&t);
            }
            Err(e) => {
                let t = crate::tr_lang(lang == Lang::Ru, "xlsx_exists.archive_failed", &[("error", &e)]);
                self.ed.status = Some(t);
            }
        }
    }

    fn xlsx_over_card(&mut self, ctx: &egui::Context, lang: Lang) {
        let Some(c) = self.xlsx_over.take() else {
            return;
        };
        let ru = lang == Lang::Ru;
        let file = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        #[derive(PartialEq)]
        enum Pick {
            None,
            Apply,
            NewName,
            Replace,
            Close,
        }
        let mut pick = Pick::None;
        kit::Dialog::new("xlsx_overwrite", l(lang, "xlsx_exists.title"), kit::DialogSize::Form)
            .size([400.0, 0.0])
            .show(ctx, |ui| {
                ui.add(egui::Label::new(file(&c.path)).truncate());
                ui.add_space(SP_SM);
                if !c.ours {
                    ui.add(egui::Label::new(l(lang, "xlsx_exists.foreign")).wrap());
                }
                if c.pending > 0 {
                    let t = crate::tr_lang(ru, "xlsx_exists.pending", &[("n", &c.pending.to_string())]);
                    ui.add(egui::Label::new(t).wrap());
                }
                if c.outside > 0 {
                    let t = crate::tr_lang(ru, "xlsx_exists.outside", &[("n", &c.outside.to_string())]);
                    ui.add(egui::Label::new(t).wrap());
                }
                ui.add_space(SP_MD);
                ui.horizontal_wrapped(|ui| {
                    let name = crate::tr_lang(ru, "xlsx_exists.new_name", &[("name", &file(&c.name))]);
                    if kit::primary_button(ui, name, true).clicked() {
                        pick = Pick::NewName;
                    }
                    if c.ours && c.pending > 0 && kit::secondary_button(ui, l(lang, "xlsx_exists.apply_first")).clicked() {
                        pick = Pick::Apply;
                    }
                    if c.ours && kit::danger_button(ui, l(lang, "xlsx_exists.replace"), true).clicked() {
                        pick = Pick::Replace;
                    }
                    if kit::cancel_button(ui, l(lang, "xlw.cancel")).clicked() {
                        pick = Pick::Close;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            pick = Pick::Close;
        }
        match pick {
            Pick::None => self.xlsx_over = Some(c),
            Pick::Close => {}
            Pick::Apply => self.xlsx_open(&c.path, false),
            Pick::NewName => self.xlsx_run(&c.name, false, c.scope),
            Pick::Replace => self.xlsx_replace(c),
        }
    }

    /// Starts the export on a worker (or inline for a model without a file).
    fn xlsx_run(&mut self, path: &Path, again: bool, scope: Option<xlsx::Scope>) {
        self.xlsx_run_accepting(path, again, scope, vec![]);
    }

    /// `xlsx_run` that may drop exactly `accept` from the old book (the list the user confirmed).
    pub(super) fn xlsx_run_accepting(
        &mut self,
        path: &Path,
        again: bool,
        scope: Option<xlsx::Scope>,
        accept: Vec<String>,
    ) {
        if self.xlsx_busy() {
            return;
        }
        self.xlsx_scope = scope.clone();
        let opts = xlsx_job::JobOpts { scope, accept_losses: accept, ..Default::default() };
        let u = book_units(self.ed.tool.units);
        let locale = self.sh.cat.lang.code();
        let rev = self.ed.content_snap();
        match self.ed.model.db_path() {
            Some(db) => {
                self.xlsx_job = Some(XlsxJob::start_db(db, path, again, u, locale, rev, opts));
            }
            None => match XlsxJob::start_for(&self.ed.model, path, again, u, locale, rev, opts) {
                Ok(job) => self.xlsx_job = Some(job),
                Err(e) => {
                    self.xlsx_finished(path, again, rev, Outcome::Failed(e));
                }
            },
        }
    }

    /// "Export to a copy": the book that cannot be written is only read; the refreshed book
    /// goes to a new file beside it, which is linked when the worker is done.
    fn xlsx_export_copy(&mut self, path: &Path, scope: Option<xlsx::Scope>) {
        if self.xlsx_busy() {
            return;
        }
        let lang = self.xlang();
        let Some(copy) = free_copy(path) else {
            self.ed.status = Some(format!("{}: {}", l(lang, "xlw.not_saved"), l(lang, "xlsx_readonly.no_copy_name")));
            return;
        };
        self.xlsx_export_into(path, scope, copy);
    }

    /// "Save to...": the book's folder has no write rights, so the user picks a file in another
    /// folder (Documents first, book name filled in); the book itself is only read.
    fn xlsx_export_elsewhere(&mut self, path: &Path, scope: Option<xlsx::Scope>) {
        if self.xlsx_busy() {
            return;
        }
        let name = path.file_name().map_or_else(|| "book.xlsx".into(), |n| n.to_string_lossy().into_owned());
        let picked = crate::file_pick::Dlg::new()
            .set_title(l(self.xlang(), "xlsx_write.save_to"))
            .add_filter("xlsx", &["xlsx"])
            .set_directory(writable_default())
            .set_file_name(name)
            .save_file();
        self.xlsx_export_picked(path, scope, picked);
    }

    /// The picked file of "Save to...": nothing is written on cancel; the link moves to the new
    /// book only once the worker has written it (`Outcome::Copied`).
    fn xlsx_export_picked(&mut self, path: &Path, scope: Option<xlsx::Scope>, picked: Option<PathBuf>) {
        match elsewhere_target(path, picked) {
            Some(to) => self.xlsx_export_into(path, scope, to),
            None => self.ed.status = Some(l(self.xlang(), "xlw.export_cancelled").into()),
        }
    }

    /// The refreshed book goes to `copy` (never replaced when taken); `path` is only read.
    fn xlsx_export_into(&mut self, path: &Path, scope: Option<xlsx::Scope>, copy: PathBuf) {
        if self.xlsx_busy() {
            return;
        }
        self.xlsx_scope = scope.clone();
        let opts = xlsx_job::JobOpts { scope, copy_to: Some(copy), ..Default::default() };
        let u = book_units(self.ed.tool.units);
        let locale = self.sh.cat.lang.code();
        let rev = self.ed.content_snap();
        match XlsxJob::start_for(&self.ed.model, path, true, u, locale, rev, opts) {
            Ok(job) => self.xlsx_job = Some(job),
            Err(e) => self.xlsx_finished(path, true, rev, Outcome::Failed(e)),
        }
    }

    /// Result of an export: status line, link to the book, opening it.
    fn xlsx_finished(&mut self, path: &Path, again: bool, rev: xlsx_job::Snap, o: Outcome) {
        let lang = self.xlang();
        let (o, note) = o.split_note();
        if let Some(n) = note {
            self.xlsx_finished(path, again, rev, o);
            self.add_status(&n.text(lang == Lang::Ru));
            return;
        }
        match o {
            Outcome::Done => {
                let t = l(lang, if again { "xlw.workbook_updated" } else { "xlw.saved" });
                self.ed.status = Some(format!("{t}: {}", path.display()));
                remember_in(&self.ed.model, path);
                crate::help::tutorial::note_xlsx_exported();
                // the export gave every row its id
                let clear = |m: &ddc_model::Model| crate::shell::xlsx_links::clear(m, path);
                let rev = keep_snap(&self.ed.model, rev, clear);
                let mut pv = match self.xlsx.take() {
                    Some(p) if again && p.path == path => p,
                    _ => XlsxPreview::linked(path, false),
                };
                pv.report = Report::default();
                pv.scope = self.xlsx_scope.clone();
                pv.report_gen += 1;
                pv.take = xlsx::Take::default();
                pv.synced = true;
                pv.error = None;
                pv.export_failed = false;
                pv.created = 0;
                pv.watch.mark(Stamp::of(path));
                pv.rev = rev;
                self.xlsx = Some(pv);
                if !again && self.prefs.xlsx_open_after {
                    xlsx_job::open_book(path);
                }
            }
            Outcome::Cancelled => {
                self.ed.status = Some(l(lang, "xlw.export_cancelled").into());
            }
            Outcome::Failed(e) => {
                let t = l(lang, "xlw.not_saved");
                self.ed.status = Some(format!("{t}: {}", book_error(&e, lang)));
                // the new book would drop parts of the old one: the user decides
                if let Some(items) = xlsx::loss_items(&e) {
                    let scope = self.xlsx_scope.clone();
                    self.xlsx_loss = Some(super::xlsx_loss::LossCard { path: path.to_path_buf(), again, scope, items });
                }
                // the linked panel shows the reason and offers Retry
                if let Some(pv) = self.xlsx.as_mut().filter(|p| p.path == path) {
                    pv.error = Some(e);
                    pv.export_failed = true;
                } else if way_out(Some(&e), true).is_some() {
                    // no linked panel yet: open one on the book so the way-out action is there
                    let mut pv = XlsxPreview::linked(path, false);
                    pv.synced = false;
                    pv.error = Some(e);
                    pv.export_failed = true;
                    self.xlsx = Some(pv);
                }
            }
            // the re-export after Apply and "Export to a copy" write a copy beside the book:
            // the copy is the linked book from now on
            Outcome::Copied(copy) => {
                self.copy_note(&copy);
                let clear = |m: &ddc_model::Model| crate::shell::xlsx_links::clear(m, &copy);
                let rev = keep_snap(&self.ed.model, rev, clear);
                let mut pv = XlsxPreview::linked(&copy, false);
                pv.scope = self.xlsx_scope.clone();
                pv.copy_made = true;
                pv.rev = rev;
                pv.watch.mark(Stamp::of(&copy));
                self.xlsx = Some(pv);
            }
            // turned into Done plus a note by `split_note` above
            Outcome::Dropped { .. } => {}
        }
    }

    /// The book was locked: the -synced copy is named and remembered as the linked book.
    fn copy_note(&mut self, copy: &Path) {
        let lang = self.xlang();
        let t = crate::tr_lang(lang == Lang::Ru, "xlsx_readonly.copied", &[("path", &copy.display().to_string())]);
        self.add_status(&t);
        remember_in(&self.ed.model, copy);
    }

    /// Starts the re-export that follows Apply; the book is replaced only when the worker is done.
    fn xlsx_apply_job(&mut self, path: &Path, opts: xlsx_job::JobOpts) -> Result<(), String> {
        let u = book_units(self.ed.tool.units);
        let locale = self.sh.cat.lang.code();
        let rev = self.ed.content_snap();
        let job = XlsxJob::start_for(&self.ed.model, path, true, u, locale, rev, opts)?;
        self.xlsx_job = Some(job);
        Ok(())
    }

    /// Result of the re-export after Apply, for the linked preview if it still shows this book.
    fn xlsx_apply_done(
        &mut self,
        target: &Path,
        rev: xlsx_job::Snap,
        tail: &xlsx_job::ApplyTail,
        o: Outcome,
    ) {
        let (o, note) = o.split_note();
        if let Some(n) = note {
            let ru = self.xlang() == Lang::Ru;
            self.xlsx_apply_done(target, rev, tail, o);
            self.add_status(&n.text(ru));
            return;
        }
        match self.xlsx.take() {
            Some(mut pv) if pv.path == target => {
                self.finish_apply_pv(&mut pv, rev, tail, o);
                self.xlsx = Some(pv);
            }
            other => {
                // the window was closed while the worker ran
                self.xlsx = other;
                match &o {
                    Outcome::Copied(copy) => self.copy_note(copy),
                    _ => self.apply_note(&o, false),
                }
            }
        }
    }

    fn finish_apply_pv(
        &mut self,
        pv: &mut XlsxPreview,
        rev: xlsx_job::Snap,
        tail: &xlsx_job::ApplyTail,
        o: Outcome,
    ) {
        let reexported = apply_finish(rev, &mut self.ed.model, pv, tail, &o);
        if let Outcome::Copied(copy) = &o {
            pv.copy = None;
            self.copy_note(copy);
        } else {
            self.apply_note(&o, reexported);
        }
        // a failed re-export shows its reason in the panel with Retry, as a plain export does
        if let Outcome::Failed(e) = o {
            pv.error = Some(e);
            pv.export_failed = true;
        }
    }

    fn add_status(&mut self, t: &str) {
        let s = self.ed.status.take().unwrap_or_default();
        self.ed.status = Some(if s.is_empty() { t.to_string() } else { format!("{s} · {t}") });
    }

    /// List of the cells the last export cut or left blank, until the user closes it.
    fn xlsx_cell_notes_ui(&mut self, ctx: &egui::Context) {
        if self.xlsx_cell_notes.is_empty() {
            return;
        }
        const SHOWN: usize = 500;
        let ru = self.xlang() == Lang::Ru;
        let t = |k: &str| crate::tr_lang(ru, k, &[]);
        let mut close = false;
        // a long sheet name is cut (full name on hover): the window never outgrows the screen
        let (sheet_w, max_w) = (200.0, (ctx.screen_rect().width() - 48.0).max(320.0));
        let mut open = true;
        let width = ddc_ui::theme::layout::DIALOG_TOOL.min(max_w);
        kit::Dialog::new("xlsx_cell_notes", t("xlcell.title"), kit::DialogSize::Form)
            .size([width, 0.0])
            .stable()
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(egui::Label::new(t("xlcell.intro")).wrap());
                ui.add_space(SP_SM);
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    egui::Grid::new("xlcell_notes").striped(true).show(ui, |ui| {
                        for h in ["xlcell.col_sheet", "xlcell.col_row", "xlcell.col_col", "xlcell.col_id", "xlcell.col_what"] {
                            ui.label(RichText::new(t(h)).strong());
                        }
                        ui.end_row();
                        for n in self.xlsx_cell_notes.iter().take(SHOWN) {
                            let cell = egui::vec2(sheet_w, ui.spacing().interact_size.y);
                            ui.allocate_ui(cell, |ui| {
                                ui.add(egui::Label::new(&n.sheet).truncate());
                            });
                            ui.label(n.row.to_string());
                            ui.label(n.col.to_string());
                            ui.label(n.id.map(|i| i.to_string()).unwrap_or_default());
                            let what = match n.kind {
                                xlsx::ExportNoteKind::Clipped => "xlcell.what_clipped",
                                xlsx::ExportNoteKind::NonFinite => "xlcell.what_blank",
                            };
                            ui.label(t(what));
                            ui.end_row();
                        }
                    });
                });
                let more = self.xlsx_cell_notes.len().saturating_sub(SHOWN);
                if more > 0 {
                    ui.label(crate::tr_lang(ru, "xlcell.more", &[("count", &more.to_string())]));
                }
                ui.add_space(SP_MD);
                if kit::cancel_button(ui, t("xlcell.close")).clicked() {
                    close = true;
                }
            });
        if close || !open {
            self.xlsx_cell_notes.clear();
        }
    }

    fn apply_note(&mut self, o: &Outcome, reexported: bool) {
        let lang = self.xlang();
        let t = match o {
            Outcome::Done if reexported => l(lang, "xlw.new_rows_got_their_id").to_string(),
            Outcome::Cancelled => l(lang, "xlw.export_cancelled").to_string(),
            Outcome::Failed(e) => format!("{}: {}", l(lang, "xlw.not_saved"), book_error(e, lang)),
            _ => return,
        };
        self.add_status(&t);
    }

    /// Progress window of a running export and the «no linked book» offer.
    pub(super) fn xlsx_job_ui(&mut self, ctx: &egui::Context) {
        let lang = self.xlang();
        if let Some(job) = &self.xlsx_job {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if let Some(o) = job.poll() {
                let (path, again, rev) = (job.target.clone(), job.again, job.rev);
                let tail = job.apply.clone();
                // taken with the result: the worker read them on its own thread
                let notes = job.take_notes();
                let written = matches!(o, Outcome::Done | Outcome::Dropped { .. } | Outcome::Copied(_));
                self.xlsx_job = None;
                match tail {
                    Some(t) => self.xlsx_apply_done(&path, rev, &t, o),
                    None => self.xlsx_finished(&path, again, rev, o),
                }
                if written {
                    if let Some(t) = xlsx_job::cells_status(lang == Lang::Ru, notes.len()) {
                        self.add_status(&t);
                        self.xlsx_cell_notes = notes;
                    }
                }
            }
        }
        self.xlsx_cell_notes_ui(ctx);
        let mut stop = false;
        if let Some(job) = &self.xlsx_job {
            // `poll` sets stage STAGES (past the last name) once the result is taken
            let names = ["xlw.stage_read", "xlw.stage_write", "xlw.stage_check", "xlw.stage_finish"];
            let stage = names.get(usize::from(job.stage())).copied().unwrap_or("xlw.stage_finish");
            let text = if job.cancelling() { l(lang, "xlw.stopping") } else { l(lang, stage) };
            kit::Dialog::new("xlsx_exporting", l(lang, "xlw.exporting"), kit::DialogSize::Form)
                .size([380.0, 0.0])
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(job.target.display().to_string()).truncate());
                    ui.add_space(SP_SM);
                    stop = kit::progress_row(ui, job.fraction(), text, l(lang, "xlw.cancel"));
                });
        }
        if stop {
            if let Some(job) = &self.xlsx_job {
                job.cancel();
            }
        }
        if let Some(mut card) = self.xlsx_card.take() {
            let ru = lang == Lang::Ru;
            let (mut go, mut close) = (false, false);
            kit::Dialog::new("xlsx_scope", l(lang, "xlsx_scope.title"), kit::DialogSize::Form)
                .size([340.0, 0.0])
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(l(lang, "xlsx_scope.intro")).wrap());
                    ui.add_space(SP_SM);
                    xlsx_scope::show(ui, &mut card, ru);
                    if let Some(e) = &card.error {
                        ui.add_space(SP_SM);
                        kit::error_note(ui, e);
                    }
                    ui.add_space(SP_MD);
                    ui.horizontal(|ui| {
                        if kit::primary_button(ui, l(lang, "xlsx_scope.export"), true).clicked() {
                            go = true;
                        }
                        if kit::cancel_button(ui, l(lang, "xlw.cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if go {
                card.error = None;
            }
            let started = go && self.xlsx_export_scoped(&mut card);
            if !close && !started {
                self.xlsx_card = Some(card);
            }
        }
        self.xlsx_over_card(ctx, lang);
        self.xlsx_loss_card(ctx, lang);
        self.xlsx_relink_card(ctx);
        if self.xlsx_offer {
            let (mut export, mut close) = (false, false);
            kit::Dialog::new("xlsx_sync_offer", l(lang, "xlw.sync_none_title"), kit::DialogSize::Form)
                .size([380.0, 0.0])
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(l(lang, "xlw.sync_none")).wrap());
                    ui.add_space(SP_MD);
                    ui.horizontal(|ui| {
                        if kit::primary_button(ui, l(lang, "xlw.offer_export"), true).clicked() {
                            export = true;
                        }
                        if kit::cancel_button(ui, l(lang, "xlw.close")).clicked() {
                            close = true;
                        }
                    });
                });
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if export {
                self.xlsx_export();
            } else if close {
                self.xlsx_offer = false;
            }
        }
    }

    pub fn xlsx_import(&mut self) {
        if let Some(path) = pick(false, self.ed.tool.units, self.xlang(), None) {
            self.xlsx_open(&path, false);
        }
    }

    /// "Synchronize": the last linked book, no wizard; with no link, offer an export.
    pub fn xlsx_sync(&mut self) {
        if self.xlsx_busy() {
            return;
        }
        // after a reopen only the project's stored link is known
        let target = sync_target().or_else(|| super::xlsx_relink::stored_target(&self.ed.model));
        match xlsx_job::sync_action(self.xlsx.is_some(), target) {
            SyncAction::Reload => {
                if let Some(pv) = &mut self.xlsx {
                    // "delete missing" is never carried into a sync: it is chosen anew each time
                    set_delete_missing(pv, false);
                    pv.reload_keyed(&mut self.ed.model);
                }
            }
            SyncAction::Open(p, dm) => self.xlsx_open(&p, dm),
            SyncAction::Missing(p) => self.xlsx_missing(p),
            SyncAction::OfferExport => self.xlsx_offer = true,
        }
    }

    /// Opens the import preview of a book without changing the model: the read runs on a worker
    /// with a progress bar and Cancel; `xlsx_load_ui` takes the result.
    pub fn xlsx_open(&mut self, path: &Path, delete_missing: bool) {
        if self.xlsx_load.is_some() {
            let lang = self.xlang();
            self.ed.status = Some(l(lang, "xlw.load_running").into());
            return;
        }
        match super::xlsx_owner::check(path) {
            g if g.blocks() => {
                // a preview without a report carries the line and its Retry
                let mut pv = XlsxPreview::linked(path, delete_missing);
                pv.synced = false;
                pv.hold();
                // a locked book is not read yet: one that names another project never takes the link
                if super::xlsx_project::origin(&self.ed.model, path) == super::xlsx_project::Origin::Other {
                    remember(path);
                } else {
                    remember_in(&self.ed.model, path);
                }
                self.xlsx = Some(pv);
                self.ed.status = Some(l(self.xlang(), "xlw.open_elsewhere").into());
                return;
            }
            super::xlsx_owner::Gate::Soft if super::xlsx_owner::soft_note_due(path) => {
                self.ed.status = Some(l(self.xlang(), "xlw.owner_stale").into());
            }
            _ => {}
        }
        match XlsxLoad::start(&self.ed.model, path, delete_missing, self.ed.content_snap()) {
            Ok(job) => self.xlsx_load = Some(job),
            // a model that cannot be copied is read on the calling thread
            Err(_) => self.xlsx_open_now(path, delete_missing),
        }
    }

    /// Re-reads the open preview on the worker (progress and Cancel as for the first read).
    fn xlsx_reload_async(&mut self, pv: &mut XlsxPreview) {
        let lang = self.xlang();
        if self.xlsx_load.is_some() {
            self.ed.status = Some(l(lang, "xlw.load_running").into());
            return;
        }
        match pv.hold() {
            g if g.blocks() => {
                self.ed.status = Some(l(lang, "xlw.open_elsewhere").into());
                return;
            }
            super::xlsx_owner::Gate::Soft if super::xlsx_owner::soft_note_due(&pv.path) => {
                self.ed.status = Some(l(lang, "xlw.owner_stale").into());
            }
            _ => {}
        }
        let started = XlsxLoad::start_with(
            &self.ed.model,
            &pv.path,
            pv.delete_missing,
            self.ed.content_snap(),
            pv.take.clone(),
            pv.decisions.clone(),
            true,
        );
        match started {
            Ok(job) => self.xlsx_load = Some(job),
            // a model that cannot be copied is read on the calling thread
            Err(_) => pv.reload_keyed(&mut self.ed.model),
        }
    }

    /// The result of a worker re-read goes to the open preview of the same book. A result
    /// read with other choices than the preview holds now (a click during the read) is dropped
    /// and the book is read again, so the shown report always matches the choices.
    fn xlsx_reloaded(&mut self, job: &XlsxLoad, out: Result<xlsx_load::Loaded, String>) {
        let lang = self.xlang();
        let Some(mut pv) = self.xlsx.take() else {
            return;
        };
        if pv.path != job.path {
            self.xlsx = Some(pv);
            return;
        }
        let stale = pv.take != job.take
            || pv.decisions != job.decisions
            || pv.delete_missing != job.delete_missing;
        if stale && out.is_ok() {
            pv.rev = None;
            self.xlsx_reload_async(&mut pv);
            self.xlsx = Some(pv);
            return;
        }
        let got = pv.finish_reload(out, job.rev);
        if stale {
            pv.rev = None;
        }
        self.xlsx = Some(pv);
        let line = match got {
            Reloaded::Applied => return,
            Reloaded::Cancelled => l(lang, "xlw.reload_cancelled").to_string(),
            Reloaded::Locked => l(lang, "xlw.reload_locked").to_string(),
            Reloaded::Failed(e) => format!("{}: {}", l(lang, "xlw.reload_failed"), book_error(&e, lang)),
        };
        self.ed.status = Some(line);
    }

    /// `xlsx_open` on the calling thread (screenshots and tests).
    pub fn xlsx_open_now(&mut self, path: &Path, delete_missing: bool) {
        let got = XlsxPreview::load(&mut self.ed.model, path, delete_missing);
        self.xlsx_loaded(path, delete_missing, self.ed.content_snap(), got);
    }

    fn xlsx_loaded(
        &mut self,
        path: &Path,
        delete_missing: bool,
        rev: xlsx_job::Snap,
        got: Result<XlsxPreview, String>,
    ) {
        match got {
            Ok(mut p) => {
                remember_in(&self.ed.model, path);
                p.rev = rev;
                self.xlsx = Some(p);
            }
            // the card replaces the preview; the link is not moved to this book
            Err(e) if e == super::xlsx_project::FOREIGN => {
                self.xlsx = Some(super::xlsx_project::refused(path, delete_missing));
            }
            Err(e) => {
                let lang = self.xlang();
                let t = l(lang, "xlw.workbook_not_read");
                self.ed.status = Some(format!("{t}: {}", book_error(&e, lang)));
            }
        }
    }

    /// Progress window of the running book read; takes its result when the worker is done.
    pub(super) fn xlsx_load_ui(&mut self, ctx: &egui::Context) {
        let lang = self.xlang();
        let Some(job) = &self.xlsx_load else {
            return;
        };
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if let Some(out) = job.poll() {
            let Some(job) = self.xlsx_load.take() else {
                return;
            };
            let (path, dm, rev) = (job.path.clone(), job.delete_missing, job.rev);
            if job.reload {
                self.xlsx_reloaded(&job, out);
                return;
            }
            match out {
                Err(e) if xlsx_load::is_cancelled(&e) => {
                    self.ed.status = Some(l(lang, "xlw.load_cancelled").into());
                }
                Ok((report, watch)) => {
                    let pv = XlsxPreview::from_report(&path, dm, report, watch, xlsx::Take::default());
                    self.xlsx_loaded(&path, dm, rev, Ok(pv));
                }
                Err(e) => self.xlsx_loaded(&path, dm, rev, Err(e)),
            }
            return;
        }
        let mut stop = false;
        kit::Dialog::new("xlsx_loading", l(lang, "xlw.loading"), kit::DialogSize::Form)
            .size([380.0, 0.0])
            .show(ctx, |ui| {
                let text = if job.cancelling() { l(lang, "xlw.stopping") } else { l(lang, "xlw.stage_load") };
                ui.add(egui::Label::new(job.path.display().to_string()).truncate());
                ui.add_space(SP_SM);
                stop = kit::progress_row(ui, job.fraction(), text, l(lang, "xlw.cancel"));
            });
        if stop {
            job.cancel();
        }
    }

    /// Export again into the linked workbook (new rows get ids, computed columns get
    /// fresh values); user columns and sheets are preserved.
    fn xlsx_reexport(&mut self, pv: &mut XlsxPreview) {
        // the window keeps `pv`; the result refreshes it when the worker is done
        let path = pv.path.clone();
        let scope = pv.scope.clone();
        self.xlsx_run(&path, true, scope);
        // a model without a file ran inline: take the refreshed state back
        if let Some(fresh) = self.xlsx.take() {
            *pv = fresh;
        }
    }

    pub(super) fn xlsx_window(&mut self, ctx: &egui::Context) {
        let Some(mut pv) = self.xlsx.take() else {
            return;
        };
        let lang = self.xlang();
        if super::xlsx_project::is_foreign(&pv) {
            use super::xlsx_project::Click;
            match super::xlsx_project::card(ctx, &pv.path, lang == Lang::Ru) {
                None => self.xlsx = Some(pv),
                Some(c) => {
                    if last_book().as_deref() == Some(pv.path.as_path()) {
                        forget_link();
                    }
                    if c == Click::ExportNew {
                        self.xlsx_export();
                    }
                }
            }
            return;
        }
        let busy = self.xlsx_job.is_some() || self.xlsx_load.is_some();
        // the book was renamed or moved away: the relink card, never a stale report
        if super::xlsx_relink::vanished(&pv.path, busy, Instant::now()) {
            self.xlsx_missing(pv.path.clone());
            return;
        }
        ctx.request_repaint_after(xlsx::SETTLE_STEP);
        let changed_on_disk = self.xlsx_load.is_none() && pv.reload_due(Instant::now());
        if changed_on_disk {
            // rows moved or changed: old decisions would point at the wrong rows
            pv.decisions = KeyDecisions { alias: std::mem::take(&mut pv.decisions.alias), ..KeyDecisions::default() };
            pv.dup_new.clear();
        }
        let th = &self.sh.theme;
        let (err, ok, warn, acc) = (
            th.c("error"),
            th.c("success"),
            th.c("warning"),
            th.c("accent"),
        );
        let (sec, prim) = (th.c("text.secondary"), th.c("text.primary"));
        let (mut open, mut apply, mut cancel, mut reload, mut reexport) =
            (true, false, false, false, false);
        let mut decide: Option<(String, u32, bool)> = None;
        let mut read_as: Option<super::xlsx_read_as::ReadAsClick> = None;
        let mut show: Option<(String, i64)> = None;
        let mut zoom = false;
        // the user's choice for conflicts: one cell, or all of them
        let mut flips: Vec<((String, u32, u32), bool)> = vec![];
        let mut dup_new: Vec<(String, u32)> = vec![];
        let mut dup_undo = false;
        let mut pick_all: Option<bool> = None;
        let head = super::xlsx_head::from_book(
            &pv.path,
            pv.synced,
            pv.synced_at.or(pv.watch.seen.and_then(|s| s.mtime)),
            pv.rev.is_some() && pv.rev != self.ed.content_snap(),
        );
        let ru = lang == Lang::Ru;
        let mut list_note: Option<String> = None;
        let mut err_act: Option<ErrAct> = None;
        let error_line = |ui: &mut egui::Ui, pv: &XlsxPreview| -> Option<ErrAct> {
            let e = pv.error.as_ref()?;
            let t = l(lang, if pv.export_failed { "xlw.not_saved" } else { "xlw.workbook_not_read" });
            kit::error_note(ui, &format!("{t}: {}", book_error(e, lang)));
            let mut act = None;
            let eth = ddc_ui::theme::of(ui.ctx());
            let quiet = |ui: &mut egui::Ui, text: &str| ddc_ui::widgets::Button::quiet(text).small().show(ui, &eth).clicked();
            ui.horizontal(|ui| {
                for (k, a) in error_buttons(pv.export_failed) {
                    if quiet(ui, l(lang, k)) {
                        act = Some(a);
                    }
                }
                if pv.is_held() && quiet(ui, l(lang, "xlw.read_anyway")) {
                    act = Some(ErrAct::ReadAnyway);
                }
                if let Some((k, a)) = way_out(pv.error.as_deref(), pv.export_failed) {
                    if quiet(ui, l(lang, k)) {
                        act = Some(a);
                    }
                }
            });
            if !pv.export_failed && !pv.is_held() {
                ui.label(RichText::new(l(lang, "xlw.error_resave_note")).weak());
            }
            act
        };
        if pv.synced {
            let bottom = self.sh.theme.d().row + self.chrome.tasks.window_h;
            let toast_rows = self.sh.toasts.visible().len();
            let toast_more = self.sh.toasts.overflow() > 0;
            let stack = ddc_ui::widgets::toasts::stack_height_in(ctx, toast_rows, toast_more);
            kit::Dialog::hud(
                "xlsx_link",
                l(lang, "xlw.linked_workbook"),
                egui::Align2::RIGHT_BOTTOM,
                hud_offset(bottom, stack),
            )
                .size([ddc_ui::theme::layout::DIALOG_FORM, 0.0])
                .stable()
                .open(&mut open)
                .show(ctx, |ui| {
                    super::xlsx_head::strip(ui, &head, ru);
                    ui.label(
                        RichText::new(l(lang, "xlw.the_workbook_matches_the_model_sav"))
                        .color(sec),
                    );
                    if pv.created > 0 {
                        ui.label(
                            RichText::new(l(lang, "xlw.new_rows_get_their_id_after_re_exp"))
                            .color(warn),
                        );
                    }
                    err_act = error_line(ui, &pv).or(err_act);
                    let fth = ddc_ui::theme::of(ui.ctx());
                    if pv.copy_made
                        && ddc_ui::widgets::Button::quiet(l(lang, "xlsx_readonly.show_in_folder")).small().show(ui, &fth).clicked()
                    {
                        super::xlsx_head::show_in_folder(&pv.path);
                    }
                    let [again, export, close] = linked_footer(ui, lang, busy);
                    reload |= again;
                    reexport |= export;
                    cancel |= close;
                });
        } else {
            let c = pv.report.counts();
            let nerr = pv.report.blocking();
            let nskip = pv.report.skipped();
            let total = c.added + c.changed + c.deleted;
            let pending = pv.report.pending_matches();
            let accepted = pv.report.accepted() + pv.report.accepted_matches();
            // without a baseline the flag is inert: every site below reads this, not the flag
            let dm = super::xlsx_no_base::effective_delete(&pv);
            let del_lines = if super::xlsx_gate::plain_tick(&pv.report, dm) {
                // up to five lookups: done once per (revision, total_changes, report), not per frame
                let dk = egui::Id::new("xlsx.del_lines");
                let (rev, tc) = self.ed.cache_key();
                let rk = (rev, tc, pv.report_gen);
                match ctx
                    .data(|d| d.get_temp::<((u64, u64, u64), (Vec<String>, usize))>(dk))
                    .filter(|c| c.0 == rk)
                {
                    Some(c) => c.1,
                    None => {
                        let v = deleted_lines(&self.ed.model, &pv.report, 5);
                        ctx.data_mut(|d| d.insert_temp(dk, (rk, v.clone())));
                        v
                    }
                }
            } else {
                (Vec::new(), 0)
            };
            // elements the type and layer rows reach: one count per (revision, report, changes)
            let type_users = {
                let tk = egui::Id::new("xlsx.type_users");
                let (rev, tc) = self.ed.cache_key();
                let rk = (rev, tc, pv.report_gen, pv.report.changes.len() as u64);
                match ctx
                    .data(|d| d.get_temp::<((u64, u64, u64, u64), usize)>(tk))
                    .filter(|c| c.0 == rk)
                {
                    Some(c) => c.1,
                    None => {
                        let n = crate::type_use::report_users(&self.ed.model, &pv.report);
                        ctx.data_mut(|d| d.insert_temp(tk, (rk, n)));
                        n
                    }
                }
            };
            let types_ok = super::xlsx_gate::types_ok(pv.confirm_types, pv.report_gen, type_users);
            let nign = pv.report.ignored.len() + pv.report.conflicts.len() + nskip;
            let notes = pv.report.removed_count()
                + pv.report.sheet_issues.len()
                + pv.report.look_alikes.len()
                + super::xlsx_read_as::count(&pv.report, &pv.decisions.alias)
                + pv.report.model_newer
                + pv.report.warned();
            let idle = super::xlsx_idle::is_idle(total, nerr, nign, notes, pv.report.matches.len());
            // nothing to show: fixed width, height follows the content. Otherwise the table is the
            // content: a workspace window that opens as tall as the screen allows
            let class = if idle { kit::DialogSize::Form } else { kit::DialogSize::Workspace };
            let tall = ddc_ui::theme::layout::DIALOG_WORKSPACE_H + ddc_ui::theme::space::XXL * 4.0;
            let (win_id, win_pos) = preview_window_id(ctx, idle);
            let title = l(lang, "xlw.import_from_a_workbook");
            // each class keeps its own saved geometry: a dragged idle form must not size the
            // workspace or beat the position it takes over
            let mut dialog = kit::Dialog::new(
                if idle { "xlsx_preview" } else { PREVIEW_WORK_GEOM },
                title,
                class,
            )
                .id(win_id)
                .size(if idle {
                    [super::xlsx_idle::width(idle), 0.0]
                } else {
                    [ddc_ui::theme::layout::DIALOG_WORKSPACE, tall]
                })
                .stable()
                .open(&mut open);
            if let Some(p) = win_pos {
                dialog = dialog.first_pos(p);
            }
            dialog.show(ctx, |ui| {
                // the room's bottom before any row lands: egui grows max_rect with what overflows
                let room_bottom = ui.max_rect().bottom();
                super::xlsx_head::strip(ui, &head, ru);
                kit::elided_weak(
                    ui,
                    l(lang, "xlw.preview_model_unchanged_workbook_i"),
                );
                err_act = error_line(ui, &pv).or(err_act);
                ui.add_space(SP_SM);
                {
                    use super::xl_diff::Ctx as DiffCtx;
                    let dcx = DiffCtx {
                        rep: &pv.report,
                        model: &self.ed.model,
                        gen: pv.report_gen,
                        rev: self.ed.cache_key(),
                        lang,
                    };
                    pv.diff.set_errors_only(pv.only_errors);
                    pv.diff.ensure(&dcx);
                }
                // everything that needs a look folds into one disclosure under the chips; it opens
                // by itself only for items that block Apply, and its body never takes the rows
                // the table needs
                let scope_line = super::xlsx_scope::preview_line(lang == Lang::Ru, &pv.report);
                let unconf = super::xlsx_no_base::unconfirmed(&pv);
                let n_check = nerr
                    + nign
                    + notes
                    + pending
                    + pv.report.proposals.len()
                    + pv.report.matches.len()
                    + usize::from(scope_line.is_some())
                    + usize::from(pv.report.no_project)
                    + usize::from(pv.report.no_base)
                    + usize::from(c.deleted > 0)
                    + usize::from(type_users > 0);
                // a chip whose items live in the check area opens it and scrolls to them
                let mut goto: Option<CheckGoto> = None;
                ui.horizontal_wrapped(|ui| {
                    let ru = lang == Lang::Ru;
                    // one counter row: the kind chips also filter the list below; a zero count is
                    // muted and inert, the rest of the chips only count; with no check area to
                    // open a chip is a plain counter (no hand, no click)
                    let chip = |ui: &mut egui::Ui, n: usize, label: &str, col: egui::Color32| {
                        let col = if n == 0 { sec } else { col };
                        let r = egui::Frame::new()
                            .fill(col.gamma_multiply(0.12))
                            .corner_radius(ddc_ui::theme::radius::SM)
                            .inner_margin(egui::Margin::symmetric(SP_MD as i8, SP_XS as i8))
                            .show(ui, |ui| {
                                ui.label(RichText::new(n.to_string()).strong().color(col));
                                ui.label(RichText::new(label).color(prim));
                            })
                            .response;
                        if n_check == 0 {
                            return false;
                        }
                        let r = r.interact(egui::Sense::click());
                        r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                    };
                    let no_base = pv.report.no_base;
                    pv.diff.kind_row_in(ui, th, ru, no_base);
                    if !pv.report.proposals.is_empty() {
                        let n = pv.report.proposals.len();
                        if chip(ui, n, &cnt::chip_label(ru, cnt::Chip::NewColumns, n), acc) {
                            goto = Some(CheckGoto::Proposals);
                        }
                    }
                    if pending > 0 && chip(ui, pending, l(lang, "xlsx_confirm.chip"), warn) {
                        goto = Some(CheckGoto::Matches);
                    }
                    let nrm = pv.report.removed_count();
                    if nrm > 0 {
                        let hit = if pv.report.no_base {
                            chip(ui, nrm, l(lang, "xlsx_delete.not_removed"), sec)
                        } else {
                            chip(ui, nrm, &cnt::chip_label(ru, cnt::Chip::Removed, nrm), warn)
                        };
                        if hit {
                            goto = Some(CheckGoto::Top);
                        }
                    }
                });
                let goto_id = egui::Id::new("xlsx.check_goto");
                let (goto, show_all) = chip_goto(goto, n_check > 0);
                if let Some(g) = goto {
                    if show_all {
                        pv.only_errors = false;
                    }
                    ctx.data_mut(|d| d.insert_temp(goto_id, g));
                }
                // an open type tick opens the disclosure like a deletion does
                let blocking = nerr > 0 || pending > 0 || unconf || c.deleted > 0 || !types_ok;
                let gate_why = |pv: &XlsxPreview| {
                    super::xlsx_gate::blocked(&super::xlsx_gate::GateIn {
                        busy,
                        errors: nerr,
                        pending,
                        deleted: c.deleted,
                        mass: !pv.report.mass_deletes().is_empty(),
                        ticked: pv.confirm_mass,
                        type_users,
                        types_ticked: super::xlsx_gate::types_ok(
                            pv.confirm_types,
                            pv.report_gen,
                            type_users,
                        ),
                        no_base: super::xlsx_no_base::unconfirmed(pv),
                        // rows created last time still lack ids (the book was not rewritten):
                        // applying again would create them twice
                        no_ids: pv.created > 0 && c.added > 0,
                        work: total > 0 || accepted > 0,
                    })
                    .map(|k| l(lang, k))
                };
                let mut check_open = false;
                if n_check > 0 {
                    let (key, by_default) = check_slot(&pv.path, pv.report_gen, blocking);
                    check_open = ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(by_default);
                    check_open |= pv.only_errors || goto.is_some();
                    let n = n_check.to_string();
                    let title = crate::tr_lang(lang == Lang::Ru, "xlsx_confirm.to_check", &[("n", &n)]);
                    ddc_ui::widgets::section_header(ui, th, &title, &mut check_open);
                    ctx.data_mut(|d| d.insert_temp(key, check_open));
                }
                if let Some(w) = collapsed_reason(blocking, check_open, gate_why(&pv)) {
                    kit::error_note(ui, w);
                }
                // a warning, not an option: it stays in view while the disclosure is closed
                if !check_open {
                    super::xlsx_project::note(ui, &pv.report, ru);
                }
                let cap = check_cap(ui.available_height(), th.d().row, pv.diff.shown());
                if check_open {
                    let ru = lang == Lang::Ru;
                    let pending_goto = ctx.data(|d| d.get_temp::<CheckGoto>(goto_id));
                    // a cut line always comes with a bar that says there is more
                    let over_id = egui::Id::new("xlsx.check_over");
                    let over = ctx.data(|d| d.get_temp::<bool>(over_id)).unwrap_or(false);
                    let bar = if over {
                        egui::scroll_area::ScrollBarVisibility::AlwaysVisible
                    } else {
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
                    };
                    let goto_to = |ui: &mut egui::Ui, at: CheckGoto| {
                        if pending_goto == Some(at) {
                            ui.scroll_to_cursor(Some(egui::Align::TOP));
                        }
                    };
                    let shown = egui::ScrollArea::vertical()
                        .id_salt("xlsx.check")
                        .max_height(cap)
                        .auto_shrink([false, true])
                        .scroll_bar_visibility(bar)
                        .show(ui, |ui| {
                            goto_to(ui, CheckGoto::Top);
                            if let Some(line) = &scope_line {
                                kit::elided_weak(ui, line);
                            }
                            super::xlsx_project::note(ui, &pv.report, ru);
                            super::xlsx_no_base::card(ui, &mut pv, ru);
                            if !pv.only_errors {
                                if pv.report.model_newer > 0 {
                                    let n = pv.report.model_newer.to_string();
                                    let args = [("n", n.as_str())];
                                    let t = crate::tr_lang(ru, "xlw.model_newer_note", &args);
                                    ui.label(RichText::new(t).color(sec));
                                }
                                let th = ddc_ui::theme::of(ui.ctx());
                                let cards = xlsx_issues::issue_cards(&pv.report, dm);
                                let choice = xlsx_issues::conflict_choice(&pv.report);
                                let mut unused = false;
                                let top = xlsx_issues::Slot::Top;
                                match xlsx_issues::show(
                                    ui, &th, ru, &cards, top, choice, &mut unused,
                                ) {
                                    Some(xlsx_issues::CardClick::PickAll(t)) => pick_all = Some(t),
                                    Some(xlsx_issues::CardClick::OpenWorkbook) => {
                                        err_act = err_act.or(Some(ErrAct::OpenBook));
                                    }
                                    None => {}
                                }
                                use super::xlsx_read_as as read_card;
                                let alias = &pv.decisions.alias;
                                read_as = read_card::show(ui, &th, ru, &pv.report, alias);
                            }
                            ui.add_space(SP_XS);
                            if dm {
                                mass_delete_block(ui, &mut pv, lang, warn, sec);
                            }
                            // deletions are confirmed by one tick: the mass card's, else this one
                            if super::xlsx_gate::plain_tick(&pv.report, dm) {
                                let tick = &mut pv.confirm_mass;
                                super::xlsx_gate::tick(ui, &pv.report, lang, &del_lines, tick);
                            }
                            if type_users > 0 {
                                let at = (pv.report_gen, type_users);
                                super::xlsx_gate::types_tick(ui, ru, at, &mut pv.confirm_types);
                            }
                            if !pv.report.proposals.is_empty() && !pv.only_errors {
                                goto_to(ui, CheckGoto::Proposals);
                                ui.separator();
                                let t = l(lang, "xlw.new_columns_project_parameters_tic");
                                ui.label(RichText::new(t).strong().color(sec));
                                for p in &mut pv.report.proposals {
                                    let what = xl_text::describe(p, lang);
                                    let vals = cnt::values(ru, p.values.len());
                                    let addr = p.address();
                                    ui.horizontal(|ui| {
                                        let cth = ddc_ui::theme::of(ui.ctx());
                                        // the count and the cell stay in view: a long name is
                                        // cut, the tick never leaves the window
                                        let tail = proposal_tail_w(ui, &vals, &addr);
                                        let room = (ui.available_width() - tail)
                                            .max(ddc_ui::theme::control::CHECK);
                                        // all its rows refused: shown, never tickable
                                        ui.add_enabled_ui(!p.blocked(), |ui| {
                                            ui.set_max_width(room);
                                            let on = &mut p.accept;
                                            ddc_ui::widgets::checkbox(ui, &cth, on, &what);
                                            // the workbook's column name: cut with its tooltip
                                            crate::ui_kit::mark_user_text(ui.ctx(), &what);
                                        });
                                        ui.label(RichText::new(vals).color(sec));
                                        ui.label(RichText::new(addr).monospace().color(sec));
                                    });
                                    if let Some(note) = proposal_note(p, ru) {
                                        ui.label(RichText::new(note).small().color(warn));
                                    }
                                }
                            }
                            if !pv.report.matches.is_empty() && !pv.only_errors {
                                goto_to(ui, CheckGoto::Matches);
                                ui.separator();
                                ui.label(RichText::new(l(lang, "xlsx_confirm.section")).strong().color(sec));
                                for m in &pv.report.matches {
                                    if let Some(d) = match_row(ui, m, lang, [ok, warn, sec, prim]) {
                                        decide = Some((m.sheet.clone(), m.row, d));
                                    }
                                }
                            }
                        });
                    // a line cut at the bottom edge reads as more to scroll, not as a glitch
                    ddc_ui::widgets::scroll_hint::scroll_fade(ui, th, &shown);
                    let over = shown.content_size.y > shown.inner_rect.height() + 1.0;
                    ctx.data_mut(|d| {
                        d.insert_temp(over_id, over);
                        d.remove::<CheckGoto>(goto_id);
                    });
                } else {
                    // no area took the jump: it must not fire when the area opens later
                    ctx.data_mut(|d| d.remove::<CheckGoto>(goto_id));
                }
                {
                    let th = ddc_ui::theme::of(ui.ctx());
                    // delete-missing lives in the footer (xlsx_head); without a baseline it is off
                    let note = missing_note(&pv.report, dm, lang);
                    filter_row(ui, &th, lang, &mut pv.only_errors, note, pv.report.no_base);
                }
                ui.separator();
                let tail_id = egui::Id::new("xlsx.ws.tail");
                let mut tail_top: Option<f32> = None;
                if idle {
                    ui.add_space(SP_SM);
                    super::xlsx_idle::card(ui, lang == Lang::Ru, pv.synced_at);
                    ui.add_space(SP_SM);
                } else {
                    // the table takes what the toolbar and the footer leave, as tall as they
                    // were last frame: an estimate pushes the buttons out on a short screen
                    let tail = ctx.data(|d| d.get_temp::<f32>(tail_id)).unwrap_or(TAIL_FIRST);
                    let avail = (ui.available_height() - tail).max(120.0);
                    pv.diff.set_errors_only(pv.only_errors);
                    let cx = super::xl_diff::Ctx {
                        rep: &pv.report,
                        model: &self.ed.model,
                        gen: pv.report_gen,
                        rev: self.ed.cache_key(),
                        lang,
                    };
                    let out = pv.diff.show(ui, th, &cx, avail);
                    tail_top = Some(ui.cursor().top() - ui.spacing().item_spacing.y);
                    show = out.show;
                    zoom = out.zoom;
                    flips = out.flips;
                    dup_new = out.dup_new;
                    list_note = super::xl_issue_list::toolbar(ui, th, &pv.diff, &pv.report, &pv.path, lang);
                    if !pv.dup_new.is_empty() {
                        let ru = lang == Lang::Ru;
                        let n = pv.dup_new.len().to_string();
                        let tip = crate::tr_lang(ru, "xlsx_dup.dup_undo_tip", &[("n", &n)]);
                        if kit::secondary_button(ui, crate::tr_str(ru, "xlsx_dup.dup_undo")).on_hover_text(tip).clicked() {
                            dup_undo = true;
                        }
                    }
                }
                let why = gate_why(&pv);
                let mut apply_shown = false;
                kit::dialog_footer(ui, |ui| {
                    use super::xlsx_head::{self as eh, Kind as FK, Role};
                    let plan = eh::footer_actions(&eh::FootIn {
                        exported: true,
                        synced: false,
                        model_changed: false,
                        changes: total.max(accepted),
                        to_review: nerr + pending,
                        // the delete mode is the footer checkbox, not an action
                        missing: 0,
                    });
                    for a in plan {
                        let text = eh::action_text(a.kind, ru);
                        match (a.kind, a.role) {
                            (FK::Apply(n), _) => {
                                apply_shown = true;
                                let label = apply_label(lang, n, c.deleted);
                                // the preview refreshes by itself: Enter and Space must not confirm deletions
                                let go = if c.deleted > 0 {
                                    let r = kit::danger_button(ui, label, why.is_none());
                                    let (en, sp) = ui.input(|i| {
                                        (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Space))
                                    });
                                    let r = match why {
                                        Some(w) => r.on_disabled_hover_text(w),
                                        None => r,
                                    };
                                    let click = pointer_click(r.clicked(), en, sp);
                                    super::xlsx_gate::go(pv.confirm_mass, c.deleted, click)
                                } else {
                                    kit::primary_action(ui, label, why)
                                };
                                if go {
                                    apply = true;
                                }
                            }
                            (FK::Review, _) => {
                                // blocking errors are listed on click; matches are decided in the table
                                let r = kit::primary_button(ui, text, !busy && nerr > 0);
                                let r = match why {
                                    Some(w) => r.on_disabled_hover_text(w),
                                    None => r,
                                };
                                if r.clicked() {
                                    pv.only_errors = true;
                                }
                            }
                            (FK::Sync, Role::Primary) => {
                                if kit::primary_button(ui, text, !busy).clicked() {
                                    reload = true;
                                }
                            }
                            (FK::Sync, _) => {
                                let tip = l(lang, "xlw.synchronize_tip");
                                let r = ui
                                    .add_enabled_ui(!busy, |ui| kit::secondary_button(ui, l(lang, "xlw.synchronize")))
                                    .inner;
                                if r.on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                                    reload = true;
                                }
                            }
                            (FK::DeleteMissing, _) => {
                                // quiet, click only: turns the flag on, the list is rebuilt
                                let tip = l(lang, "xlw.not_in_book_tip");
                                let r = kit::danger_button(ui, text, !busy);
                                if r.on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                                    reload |= set_delete_missing(&mut pv, true);
                                }
                            }
                            (FK::Close, _) => {
                                if kit::cancel_button(ui, l(lang, "xlw.cancel")).clicked() {
                                    cancel = true;
                                }
                            }
                            (FK::Export, _) => {}
                        }
                    }
                    // the delete mode is a checkbox (Apply label counts deletions); no baseline:
                    // shown disabled with its reason and cannot arm
                    let toggle = missing_toggle(&pv, dm, busy);
                    // a disabled Apply says why in the same row (errors have their own text)
                    let note = if nerr > 0 {
                        Some((l(lang, "xlw.nothing_is_applied_while_there_are").to_string(), err))
                    } else if let Some(w) = why.filter(|_| apply_shown) {
                        Some((w.to_string(), sec))
                    } else if total > 0 || accepted > 0 {
                        Some((l(lang, "xlw.one_undo_step_ctrl_z").to_string(), sec))
                    } else {
                        None
                    };
                    if let Some(on) = footer_tail(ui, ru, toggle, note) {
                        reload |= set_delete_missing(&mut pv, on);
                    }
                });
                // the table takes the room under the footer from the last frame: the buttons sit
                // on the bottom edge, a short list leaves no empty band under them
                if !idle {
                    let gap = room_bottom - ui.cursor().top();
                    pv.diff.fill_h = (pv.diff.body_h + gap).max(0.0);
                }
                if let Some(top) = tail_top {
                    let h = ui.min_rect().bottom() - top;
                    ctx.data_mut(|d| d.insert_temp(tail_id, h));
                }
            });
        }
        if let Some((sheet, id)) = show {
            // a click on a row only selects and outlines; the Show button moves the view
            if pick_row(&mut self.ed, &sheet, id) && zoom {
                crate::open::summary::frame_selection(self);
            }
        }
        if let Some(c) = read_as {
            use super::xlsx_read_as::ReadAsClick as C;
            match c {
                C::Read { expected, found } => pv.read_as(&mut self.ed.model, &expected, Some(&found)),
                C::Undo { expected } => pv.read_as(&mut self.ed.model, &expected, None),
            }
        }
        if let Some((sheet, row, accept)) = decide {
            pv.decide(&mut self.ed.model, &sheet, row, accept);
            pv.key_on(&self.ed.model);
        }
        if !dup_new.is_empty() {
            // copies of a repeated id are read as new rows; the preview is read again
            pv.dup_decide(dup_new);
            reload = true;
        }
        if dup_undo && pv.dup_undo() {
            reload = true;
        }
        if pick_all.is_some() || !flips.is_empty() {
            let cells: Vec<(String, u32, u32)> = pv
                .report
                .conflicts
                .iter()
                .map(|c| (c.sheet.clone(), c.row, c.col))
                .collect();
            if let Some(t) = pick_all {
                pv.take.set_all(t);
            }
            for (k, t) in flips {
                pv.take.set(k, t, &cells);
            }
            reload = true;
        }
        if list_note.is_some() {
            self.ed.status = list_note;
        }
        match err_act {
            Some(ErrAct::Retry) if pv.export_failed => reexport = true,
            Some(ErrAct::Retry) => reload = true,
            Some(ErrAct::OpenBook) => xlsx_job::open_book(&pv.path),
            Some(ErrAct::ExportCopy) => {
                let (path, scope) = (pv.path.clone(), pv.scope.clone());
                self.xlsx_export_copy(&path, scope);
            }
            Some(ErrAct::ExportElsewhere) => {
                let (path, scope) = (pv.path.clone(), pv.scope.clone());
                self.xlsx_export_elsewhere(&path, scope);
            }
            Some(ErrAct::ReadAnyway) => {
                super::xlsx_owner::read_anyway(&pv.path);
                reload = true;
            }
            None => {}
        }
        if reload || changed_on_disk {
            self.xlsx_reload_async(&mut pv);
        }
        if reexport {
            self.xlsx_reexport(&mut pv);
        }
        // the button is disabled while a job runs; a second worker would share the part file
        if apply && !self.xlsx_busy() {
            let book_changed = xlsx::book_changed(pv.watch.seen, Stamp::of(&pv.path));
            let mark = super::xlsx_undo::Mark::of(&self.ed);
            let started = apply_begin(&mut self.ed, &mut pv, lang).and_then(|o| match o {
                None => Ok(None),
                Some(opts) => {
                    let tail = opts.apply.clone().unwrap_or_default();
                    if tail.skip {
                        return Ok(Some(Some((tail, String::new()))));
                    }
                    let path = pv.path.clone();
                    match self.xlsx_apply_job(&path, opts) {
                        Ok(()) => Ok(Some(None)),
                        Err(e) => Ok(Some(Some((tail, e)))),
                    }
                }
            });
            if matches!(started, Ok(Some(_))) {
                self.xlsx_undo = super::xlsx_undo::Mark::written(&self.ed, mark);
                done_toast(&mut self.sh.toasts, lang, &self.ed.status, self.xlsx_undo.is_some());
            }
            match started {
                Ok(None) => {
                    let key = if book_changed {
                        "xlw.book_changed_reread"
                    } else {
                        "xlw.the_model_changed_after_the_previe"
                    };
                    let t = l(lang, key);
                    self.ed.status = Some(t.into());
                }
                Ok(Some(None)) => {}
                Ok(Some(Some((tail, e)))) => {
                    let rev = self.ed.content_snap();
                    if tail.skip {
                        apply_finish(rev, &mut self.ed.model, &mut pv, &tail, &Outcome::Cancelled);
                    } else {
                        self.finish_apply_pv(&mut pv, rev, &tail, Outcome::Failed(e));
                    }
                }
                // the error line now carries Retry and "Read anyway"; nothing was written
                Err(e) if e == super::xlsx_owner::OPEN_ELSEWHERE => {
                    self.ed.status = Some(l(lang, "xlw.open_elsewhere").into());
                }
                Err(e) if e == MASS_UNCONFIRMED => {
                    self.ed.status = Some(l(lang, "xlw.confirm_the_mass_deletion_first").into());
                }
                Err(e) if e == super::xlsx_no_base::UNCONFIRMED => {
                    self.ed.status = Some(l(lang, "xlsx_no_base.confirm_first").into());
                }
                Err(e) if e == super::xlsx_no_base::NO_BASE_DELETE => {
                    self.ed.status = Some(l(lang, "xlsx_delete.no_base_reason").into());
                }
                Err(e) if e == super::xlsx_alias_ids::REFUSED => {
                    let sheets = super::xlsx_alias_ids::refused_sheets(&pv.report, &pv.decisions.alias);
                    self.ed.status = Some(super::xlsx_alias_ids::refused_text(&sheets, lang == Lang::Ru));
                }
                Err(e) => {
                    let t = l(lang, "xlw.workbook_import_failed");
                    self.ed.status = Some(format!("{t}: {}", xl_text::tr(&e, lang)));
                }
            }
        }
        if open && !cancel {
            self.xlsx = Some(pv);
        } else if !super::xlsx_project::is_foreign(&pv) {
            remember_in(&self.ed.model, &pv.path);
        }
    }
}

/// One by-key match: yellow tag, what it matched, and the two decisions.
/// Returns Some(true) for accept, Some(false) for "create new".
fn match_row(
    ui: &mut egui::Ui,
    m: &xlsx::KeyMatch,
    lang: Lang,
    c: [egui::Color32; 4],
) -> Option<bool> {
    let [ok, warn, sec, prim] = c;
    let mut out = None;
    let accepted = m.state == MatchState::Accepted;
    let rejected = m.state == MatchState::Rejected;
    ui.horizontal(|ui| {
        let tag = if accepted { ok } else if rejected { sec } else { warn };
        egui::Frame::new()
            .fill(tag.gamma_multiply(0.16))
            .corner_radius(ddc_ui::theme::radius::SM)
            .inner_margin(egui::Margin::symmetric(SP_SM as i8, SP_XXS as i8))
            .show(ui, |ui| {
                let key = match m.state {
                    MatchState::Accepted => "xlsx_confirm.tag_accepted",
                    MatchState::Rejected => "xlsx_confirm.tag_rejected",
                    MatchState::Pending => "xlsx_confirm.tag",
                };
                ui.label(RichText::new(l(lang, key)).color(tag));
            });
        ui.label(RichText::new(xlsx::cell_ref(&xl_text::sheet_name(&m.sheet, lang), m.row, 0)).monospace().color(sec));
        let kind = if m.kind == "room" { "xlsx_confirm.kind_room" } else { "xlsx_confirm.kind_curtain_wall" };
        let t = crate::tr_lang(
            lang == Lang::Ru,
            "xlsx_confirm.row_line",
            &[("kind", l(lang, kind)), ("id", &m.id.to_string()), ("key", &m.key)],
        );
        ui.label(RichText::new(t).color(prim));
        let th = ddc_ui::theme::of(ui.ctx());
        let sel = if accepted { 0 } else if rejected { 1 } else { usize::MAX };
        let items = [l(lang, "xlsx_confirm.accept"), l(lang, "xlsx_confirm.reject")];
        let tip = format!(
            "{}: {}\n{}: {}",
            items[0],
            l(lang, "xlsx_confirm.accept_tip"),
            items[1],
            l(lang, "xlsx_confirm.reject_tip")
        );
        let group = ui.horizontal(|ui| ddc_ui::widgets::segmented(ui, &th, &items, sel));
        group.response.on_hover_text(tip);
        let picked = group.inner;
        match picked {
            Some(0) => out = Some(true),
            Some(1) => out = Some(false),
            _ => {}
        }
    });
    out
}

/// Switches "delete missing"; true when the value changed and the book must be read again.
fn set_delete_missing(pv: &mut XlsxPreview, on: bool) -> bool {
    let changed = pv.delete_missing != on;
    pv.delete_missing = on;
    changed
}

/// "N elements not in the workbook" while "delete missing" is off; None when nothing is missing.
fn missing_note(r: &Report, delete_missing: bool, lang: Lang) -> Option<String> {
    let n = r.removed_count();
    if delete_missing || n == 0 {
        return None;
    }
    Some(cnt::not_in_book(lang == Lang::Ru, n))
}

/// Two text columns (type or name, level) of a one-row query.
fn row_parts(model: &ddc_model::Model, sql: &str, id: i64) -> Vec<String> {
    model
        .sql()
        .query_row(sql, [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map(|(a, b)| vec![a, b])
        .unwrap_or_default()
}

fn name_of(model: &ddc_model::Model, table: &str, id: i64) -> String {
    model
        .sql()
        .query_row(&format!("SELECT name FROM {table} WHERE id = ?1"), [id], |r| r.get(0))
        .unwrap_or_default()
}

/// Type or name and level of an element the model already has.
fn existing_parts(model: &ddc_model::Model, sheet: &str, id: i64) -> Vec<String> {
    let sql = match sheet {
        "Walls" => {
            "SELECT t.name, l.name FROM wall w JOIN wall_type t ON t.id = w.wtype \
             JOIN level l ON l.id = w.level WHERE w.id = ?1"
        }
        "Openings" => {
            "SELECT t.name, l.name FROM opening o JOIN wall w ON w.id = o.wall \
             JOIN wall_type t ON t.id = w.wtype JOIN level l ON l.id = w.level WHERE o.id = ?1"
        }
        "Rooms" => "SELECT r.name, l.name FROM room r JOIN level l ON l.id = r.level WHERE r.id = ?1",
        "Slabs" => {
            "SELECT t.name, l.name FROM slab s JOIN slab_type t ON t.id = s.stype \
             JOIN level l ON l.id = s.level WHERE s.id = ?1"
        }
        "Curtain walls" => {
            "SELECT c.name, l.name FROM curtain_wall c JOIN level l ON l.id = c.level WHERE c.id = ?1"
        }
        "Levels" => return vec![name_of(model, "level", id)],
        _ => return vec![],
    };
    row_parts(model, sql, id)
}

/// Whether the edit creates a row of this sheet (the n-th created row has the n-th such edit).
fn creates(sheet: &str, e: &ddc_model::Edit) -> bool {
    use ddc_model::Edit as E;
    matches!(
        (sheet, e),
        ("Walls", E::NewWall { .. })
            | ("Levels", E::NewLevel { .. })
            | ("Rooms", E::NewRoom(_))
            | ("Slabs", E::NewSlab(_))
            | ("Curtain walls", E::NewCurtain(_))
    )
}

/// Type or name and level typed in the book row that creates an element.
fn new_parts(model: &ddc_model::Model, e: &ddc_model::Edit) -> Vec<String> {
    use ddc_model::Edit as E;
    match e {
        E::NewWall { spec, .. } => vec![
            name_of(model, "wall_type", spec.wall_type),
            name_of(model, "level", spec.level),
        ],
        E::NewLevel { name, .. } => vec![name.clone()],
        E::NewRoom(s) => vec![s.name.clone(), name_of(model, "level", s.level)],
        E::NewSlab(s) => vec![
            name_of(model, "slab_type", s.slab_type),
            name_of(model, "level", s.level),
        ],
        E::NewCurtain(s) => vec![s.name.clone(), name_of(model, "level", s.level)],
        _ => vec![],
    }
}

/// What an added or deleted row is: "Walls · type · level"; the sheet alone when unknown.
pub(super) fn change_label(model: &ddc_model::Model, report: &Report, idx: usize) -> String {
    let ch = &report.changes[idx];
    let parts = match ch.kind() {
        "deleted" => ch.id.map(|id| existing_parts(model, &ch.sheet, id)).unwrap_or_default(),
        "added" => {
            let n = report.changes[..idx]
                .iter()
                .filter(|c| c.sheet == ch.sheet && c.kind() == "added")
                .count();
            report
                .edits
                .iter()
                .filter(|e| creates(&ch.sheet, e))
                .nth(n)
                .map(|e| new_parts(model, e))
                .unwrap_or_default()
        }
        _ => vec![],
    };
    let mut out = vec![ch.sheet.clone()];
    out.extend(parts.into_iter().filter(|p| !p.is_empty()));
    out.join(" \u{b7} ")
}

/// Workbook for the window snapshot: the model's wall sheet with height edits, a new wall, a cell error,
/// without the last row (deletion under "Delete elements…") and with a new work-code column.
pub fn demo_book(model: &ddc_model::Model, path: &Path) -> Result<(), String> {
    use xlsx::TableCell as C;
    let mut st = model
        .sql()
        .prepare(
            "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
             JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
        )
        .map_err(|e| e.to_string())?;
    let mut rows: Vec<Vec<C>> = st
        .query_map([], |r| {
            let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64)];
            v.push(C::Text(r.get(1)?));
            v.push(C::Text(r.get(2)?));
            for i in 3..9 {
                v.push(C::Num(r.get(i)?));
            }
            Ok(v)
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    if rows.len() < 3 {
        return Err("demo needs at least 3 walls".into());
    }
    rows.pop();
    for (i, r) in rows.iter_mut().enumerate() {
        r.push(C::Text(format!("\u{41a}\u{420}-{:02}", 10 + i)));
    }
    rows[0][8] = C::Num(3.2);
    rows[1][8] = C::Text("3200 mm".into());
    rows[2][7] = C::Text("1'-6\"".into());
    if let Some(r) = rows.get_mut(3) {
        r[8] = C::Text("\u{432}\u{44b}\u{441}\u{43e}\u{43a}\u{430}\u{44f}".into());
    }
    let mut new = rows[0].clone();
    new[0] = C::Text(String::new());
    new[3] = C::Num(12.0);
    new[5] = C::Num(16.0);
    rows.push(new);
    let heads = [
        "id",
        "level",
        "type",
        "ax [m]",
        "ay [m]",
        "bx [m]",
        "by [m]",
        "base_offset [m]",
        "height [m]",
        "\u{41a}\u{43e}\u{434} \u{440}\u{430}\u{431}\u{43e}\u{442}",
    ]
    .map(String::from);
    xlsx::export_table(path, "Walls", &heads, &[], &rows, None)
}

/// One frame of the open preview for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context) {
    app.xlsx_window(ctx);
}

/// One frame of the cards around a running export or read (progress, scope, overwrite, loss,
/// sync offer, cell notes) for the layout contract.
#[cfg(test)]
pub(crate) fn layout_cards(app: &mut EditorApp, ctx: &egui::Context) {
    app.xlsx_job_ui(ctx);
    app.xlsx_load_ui(ctx);
}

#[cfg(test)]
#[path = "xlsx_snap_tests.rs"]
mod snap_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    #[test]
    fn default_book_path_uses_project_stem() {
        let p = Path::new("D:/work/Tower A.ddc");
        let (dir, name) = default_book_path(Some(p), crate::units::Units::Metric, Lang::En);
        assert_eq!(dir, Some(PathBuf::from("D:/work")));
        assert_eq!(name, "Tower A.xlsx");
    }

    #[test]
    fn default_book_path_unsaved_falls_back() {
        let (dir, name) = default_book_path(None, crate::units::Units::Metric, Lang::Ru);
        assert_eq!(dir, None);
        assert_eq!(name, l(Lang::Ru, "xlw.file_metric"));
    }

    #[test]
    fn default_book_path_imperial_suffix() {
        let p = Path::new("D:/work/Tower A.ddc");
        let (_, name) = default_book_path(Some(p), crate::units::Units::Imperial, Lang::En);
        assert_eq!(name, "Tower A (imperial).xlsx");
        let (_, bare) = default_book_path(None, crate::units::Units::Imperial, Lang::En);
        assert_eq!(bare, l(Lang::En, "xlw.file_imperial"));
    }

    fn editor_with_walls() -> Editor {
        editor_n_walls(4)
    }

    fn editor_n_walls(n: usize) -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let t = ed.model.add_wall_type(
            "ext",
            &[ddc_model::LayerSpec {
                material: "brick".into(),
                thickness: 0.25,
            }],
        );
        let t = t.unwrap();
        let p = [(0., 0.), (6., 0.), (6., 4.), (0., 4.)];
        for i in 0..n {
            // past 4 walls the same box repeats 10 m further up
            let (k, dy) = (i % 4, (i / 4) as f64 * 10.0);
            ed.model
                .add_wall(&ddc_model::WallSpec {
                    a: (p[k].0, p[k].1 + dy),
                    b: (p[(k + 1) % 4].0, p[(k + 1) % 4].1 + dy),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: t,
                    offset0: None,
                })
                .unwrap();
        }
        ed.reload().unwrap();
        ed
    }

    fn row(sheet: &str, id: Option<i64>, new: &str, row: u32) -> xlsx::Change {
        xlsx::Change {
            sheet: sheet.into(),
            row,
            col: 0,
            column: "id".into(),
            id,
            old: String::new(),
            new: new.into(),
        }
    }

    #[test]
    fn deleted_row_says_which_wall_goes() {
        let ed = editor_with_walls();
        let id: i64 = ed.model.sql().query_row("SELECT MIN(id) FROM wall", [], |r| r.get(0)).unwrap();
        let level = ed.model.level_list().unwrap()[0].1.clone();
        let mut r = Report::default();
        r.changes.push(row("Walls", Some(id), "deleted (row removed)", 0));
        assert_eq!(change_label(&ed.model, &r, 0), format!("Walls \u{b7} ext \u{b7} {level}"));
        // an id the model does not have: the sheet alone
        r.changes.push(row("Walls", Some(9999), "deleted", 0));
        assert_eq!(change_label(&ed.model, &r, 1), "Walls");
    }

    #[test]
    fn added_row_says_the_typed_type_and_level() {
        let ed = editor_with_walls();
        let (lv, wt): (i64, i64) = ed
            .model
            .sql()
            .query_row("SELECT level, wtype FROM wall LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let level = ed.model.level_list().unwrap()[0].1.clone();
        let spec = ddc_model::WallSpec {
            a: (0., 0.),
            b: (1., 0.),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        };
        let mut r = Report::default();
        r.changes.push(row("Walls", None, "created", 5));
        r.changes.push(row("Walls", None, "created", 6));
        // a row without a matching edit falls back to the sheet name
        assert_eq!(change_label(&ed.model, &r, 0), "Walls");
        r.edits.push(ddc_model::Edit::NewWall { spec: spec.clone(), bulge: 0.0 });
        r.edits.push(ddc_model::Edit::NewWall { spec, bulge: 0.0 });
        assert_eq!(change_label(&ed.model, &r, 1), format!("Walls \u{b7} ext \u{b7} {level}"));
    }

    #[test]
    fn delete_missing_is_off_by_default_and_toggles_from_the_footer() {
        let p = PathBuf::from("none.xlsx");
        let mut pv = XlsxPreview::linked(&p, false);
        assert!(!pv.delete_missing);
        assert!(set_delete_missing(&mut pv, true));
        assert!(pv.delete_missing);
        assert!(!set_delete_missing(&mut pv, true), "no re-read when nothing changed");
        assert!(set_delete_missing(&mut pv, false));
        assert!(!pv.delete_missing);
    }

    #[test]
    fn delete_missing_resets_on_sync_and_on_a_new_book() {
        let mut pv = XlsxPreview::linked(&PathBuf::from("a.xlsx"), false);
        set_delete_missing(&mut pv, true);
        assert!(set_delete_missing(&mut pv, false), "sync clears it and re-reads");
        // a link remembered for sync never carries the flag
        LAST.with(|l| *l.borrow_mut() = Some(PathBuf::from("b.xlsx")));
        assert_eq!(sync_target().map(|t| t.1), Some(false));
        // a book opened afresh starts with the flag off
        assert!(!XlsxPreview::linked(&PathBuf::from("b.xlsx"), false).delete_missing);
    }

    #[test]
    fn apply_label_counts_deletions_only_when_there_are_some() {
        let plain = apply_label(Lang::En, 3, 0);
        assert!(!plain.contains("delete"), "{plain}");
        let del = apply_label(Lang::En, 3, 7);
        assert!(del.contains('7') && del.contains("delete"), "{del}");
        let ru = apply_label(Lang::Ru, 3, 7);
        assert!(ru.contains('7') && ru != del, "{ru}");
    }

    #[test]
    fn footer_texts_for_delete_missing_exist_in_both_languages() {
        for ru in [false, true] {
            for k in ["xlsx_delete.delete_missing", "xlw.ft_keep_missing_tip", "xlw.not_in_book_tip"] {
                assert_ne!(crate::tr_lang(ru, k, &[]), k);
            }
        }
    }

    #[test]
    fn missing_note_counts_rows_only_while_delete_is_off() {
        let mut r = Report::default();
        r.removed = vec![
            xlsx::Removed { sheet: "Walls".into(), id: 1 },
            xlsx::Removed { sheet: "Walls".into(), id: 2 },
        ];
        for lang in [Lang::En, Lang::Ru] {
            let t = missing_note(&r, false, lang).unwrap();
            assert!(t.contains('2'), "{t}");
        }
        assert!(missing_note(&r, true, Lang::En).is_none());
        assert!(missing_note(&Report::default(), false, Lang::En).is_none());
    }

    #[test]
    fn diff_row_id_selects_the_element_without_an_edit() {
        let mut ed = editor_with_walls();
        let rev = ed.revision;
        assert!(pick_row(&mut ed, "Walls", 2));
        assert_eq!(ed.selection, vec![crate::editor::Sel::Wall(2)]);
        assert_eq!(ed.revision, rev);
        ed.selection.clear();
        assert!(!pick_row(&mut ed, "Levels", 2));
        assert!(ed.selection.is_empty());
    }

    #[test]
    fn change_table_lists_every_row_of_a_large_report_and_sheet_filters_sum() {
        use super::super::xl_diff::{Ctx, DiffState};
        let ed = editor_with_walls();
        let mut r = Report::default();
        for i in 0..50_000u32 {
            r.changes.push(xlsx::Change {
                sheet: if i < 35_000 { "Walls" } else { "Rooms" }.into(),
                row: i + 2,
                col: 3,
                column: "h".into(),
                id: Some(i as i64),
                old: "1".into(),
                new: "2".into(),
            });
        }
        let cx = Ctx { rep: &r, model: &ed.model, gen: 0, rev: ed.cache_key(), lang: Lang::En };
        let mut d = DiffState::default();
        d.ensure(&cx);
        assert_eq!((d.total(), d.shown()), (50_000, 50_000));
        d.set_sheet(Some("Walls"));
        d.ensure(&cx);
        let walls = d.shown();
        d.set_sheet(Some("Rooms"));
        d.ensure(&cx);
        assert_eq!((walls, d.shown()), (35_000, 15_000));
        // a row names its element only on sheets that hold model elements
        assert_eq!(d.element_of_row(&r, 0), Some(("Rooms".to_string(), 35_000)));
    }

    fn book_preview(ed: &mut Editor, tag: &str) -> XlsxPreview {
        let p = std::env::temp_dir().join(format!("ddc_app_xlp_{}_{}.xlsx", tag, std::process::id()));
        xlsx::export_xlsx_units(&ed.model, &p, xlsx::Units::Metric).unwrap();
        XlsxPreview::load(&mut ed.model, &p, true).unwrap()
    }

    #[test]
    fn change_table_restarts_when_the_report_is_replaced() {
        use super::super::xl_diff::{Ctx, Kind};
        let mut ed = editor_with_walls();
        let mut pv = book_preview(&mut ed, "a");
        pv.report.changes.push(row("Walls", None, "created", 99));
        let gen = pv.report_gen;
        let rev = ed.cache_key();
        let cx = Ctx { rep: &pv.report, model: &ed.model, gen, rev, lang: Lang::En };
        pv.diff.ensure(&cx);
        assert_eq!(pv.diff.count(Kind::Added), 1);
        pv.diff.set_kind(Kind::Added, false);
        // no file change: no re-read is due, the report and its filters stay
        assert!(!pv.reload_due(Instant::now()));
        pv.reload(&mut ed.model);
        assert!(pv.report_gen > gen);
        let rev = ed.cache_key();
        let cx = Ctx { rep: &pv.report, model: &ed.model, gen: pv.report_gen, rev, lang: Lang::En };
        pv.diff.ensure(&cx);
        assert_eq!(pv.diff.count(Kind::Added), 0, "a re-read rebuilds the list");
        assert_eq!(pv.diff.shown(), pv.diff.total(), "an empty kind hides nothing");
    }

    #[test]
    fn unedited_export_previews_zero_changes() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_{}.xlsx", std::process::id()));
        for u in [xlsx::Units::Metric, xlsx::Units::Imperial] {
            xlsx::export_xlsx_units(&ed.model, &p, u).unwrap();
            let pv = XlsxPreview::load(&mut ed.model, &p, true).unwrap();
            assert_eq!(
                pv.report.counts(),
                Default::default(),
                "{:?}",
                pv.report.changes
            );
            assert!(pv.report.rejected.is_empty() && pv.report.edits.is_empty());
            assert!(
                pv.synced,
                "{:?} {:?}",
                pv.report.ignored, pv.report.proposals
            );
        }
    }

    #[test]
    fn preview_then_apply_is_one_undo_step() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl2_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let op = |ed: &Editor| {
            ed.model
                .sql()
                .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let (ops, walls) = (op(&ed), ed.walls.len());
        let pv = XlsxPreview::load(&mut ed.model, &p, true).unwrap();
        assert!(pv.report.rejected.is_empty(), "{:?}", pv.report.rejected);
        let c = pv.report.counts();
        // the demo book has no baseline: its missing row is never a deletion
        assert!(pv.report.no_base);
        assert_eq!((c.added, c.changed, c.deleted), (1, 3, 0));
        assert_eq!(
            (op(&ed), ed.walls.len()),
            (ops, walls),
            "preview changes nothing"
        );
        let s = pv.summary();
        ed.apply_xlsx(pv.report.edits, &s).unwrap();
        assert_eq!((op(&ed), ed.walls.len()), (ops + 1, walls + 1));
        ed.undo();
        assert_eq!(ed.walls.len(), walls);
        ed.redo();
        assert_eq!(ed.walls.len(), walls + 1);
        let again = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        let c = again.report.counts();
        // rows without id always create; edited cells now match the model
        assert_eq!((c.added, c.changed, c.deleted), (1, 0, 0));
    }

    #[test]
    fn accepted_new_column_becomes_parameter_in_the_same_undo_step() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl3_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(pv.report.proposals.len(), 1, "{:?}", pv.report.proposals);
        let pr = &mut pv.report.proposals[0];
        assert_eq!(pr.spec.name, crate::app2_fx::fx("xw_work_code"));
        assert_eq!(pr.spec.ptype, ddc_model::ParamType::Text);
        assert_eq!(pr.spec.categories, vec!["wall".to_string()]);
        assert!(xl_text::describe(pr, Lang::Ru).contains(&crate::app2_fx::fx("xw_for_walls")));
        pr.accept = true;
        let s = pv.summary();
        assert!(s.contains(&crate::app2_fx::fx("xw_new_params_1")), "{s}");
        ed.apply_xlsx(pv.report.batch(), &s).unwrap();
        let d = ed.model.param_by_name(&crate::app2_fx::fx("xw_work_code")).unwrap().unwrap();
        // three kept rows plus the new row, whose value goes in with its creation (22b83543)
        assert_eq!(ed.model.param_values(d.id).unwrap().len(), 4);
        ed.undo();
        assert!(ed.model.param_by_name(&crate::app2_fx::fx("xw_work_code")).unwrap().is_none());
    }

    #[test]
    fn apply_rechecks_the_model_and_writes_new_ids_back() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl5_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let op = |ed: &Editor| {
            ed.model
                .sql()
                .query_row("SELECT count(*) FROM op", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!(pv.report.counts().added, 1);
        // the model changed after the preview: the stale report is not applied
        ed.model
            .sql()
            .execute("INSERT INTO prop(kind,ref,key,value,unit) VALUES ('t',0,'k','v','')", [])
            .unwrap();
        let ops = op(&ed);
        assert_eq!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::Ru),
            Ok(Applied::Stale)
        );
        assert_eq!(op(&ed), ops);
        assert_eq!(pv.rev, ed.content_snap());
        let walls = ed.walls.len();
        let before = std::fs::read(&p).unwrap();
        super::super::xlsx_no_base::confirm(&mut pv);
        assert_eq!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::Ru),
            Ok(Applied::Done { reexported: true })
        );
        assert_eq!(ed.walls.len(), walls + 1);
        assert_eq!(pv.created, 0);
        // the demo book is not the app's own: it stays as is, the ids go to a copy beside it
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert_ne!(pv.path, p);
        // workbook with ids: the next read creates nothing
        let again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert_eq!(again.report.counts().added, 0, "{:?}", again.report.changes);
    }

    fn first_wall(ed: &Editor) -> (i64, i64) {
        ed.model
            .sql()
            .query_row("SELECT id,wtype FROM wall ORDER BY id LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    fn height(ed: &Editor, id: i64) -> f64 {
        ed.model
            .sql()
            .query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn xlsx_apply_skips_pinned_wall() {
        let mut ed = editor_with_walls();
        let (id, wtype) = first_wall(&ed);
        ed.model.pin(&[("wall", id)]).unwrap();
        let spec = ddc_model::WallSpec {
            a: (0., 0.),
            b: (6., 0.),
            level: ed.level,
            base_offset: 0.0,
            height: 3.5,
            wall_type: wtype,
            offset0: None,
        };
        let edit = ddc_model::Edit::Wall { id, spec, bulge: 0.0 };
        ed.apply_xlsx(vec![edit], "x").unwrap();
        assert_eq!(height(&ed, id), 3.0, "the pinned wall stays");
        let note = crate::tr("edst.spreadsheet_pinned", &[("n", "1")]);
        assert!(ed.status.as_deref().is_some_and(|s| s.contains(&note)), "{:?}", ed.status);
    }

    #[test]
    fn editor_change_after_export_is_not_reverted_through_the_app() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl6_{}.xlsx", std::process::id()));
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let (id, _) = first_wall(&ed);
        ed.model
            .sql()
            .execute("UPDATE wall SET height=3.5 WHERE id=?1", [id])
            .unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!(pv.report.counts(), xlsx::Counts::default(), "{:?}", pv.report.changes);
        assert_eq!(pv.report.model_newer, 1);
        assert!(!pv.synced, "the book is not the model: say so");
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert_eq!(height(&ed, id), 3.5);
    }

    #[test]
    fn copy_exists_is_localized() {
        let e = super::super::xlsx_owner::COPY_EXISTS;
        for lang in [Lang::En, Lang::Ru] {
            assert_eq!(book_error(e, lang), l(lang, "xlsx_readonly.copy_exists"));
        }
        assert_ne!(book_error(e, Lang::En), e);
        assert_ne!(book_error(e, Lang::En), book_error(e, Lang::Ru));
    }

    #[test]
    fn read_only_message_never_says_open_elsewhere_and_is_translated() {
        let ro = super::super::xlsx_owner::READ_ONLY;
        assert!(!is_locked(ro), "a read-only failure is no lock");
        for lang in [Lang::En, Lang::Ru] {
            let t = book_error(ro, lang);
            assert_eq!(t, l(lang, "xlsx_readonly.read_only"));
            assert_ne!(t, l(lang, "xlw.book_locked"));
            assert_ne!(t, l(lang, "xlw.open_elsewhere"));
            assert!(!t.to_lowercase().contains("another program"), "{t}");
            assert!(!l(lang, "xlsx_readonly.export_copy").is_empty() && l(lang, "xlsx_readonly.export_copy") != "xlsx_readonly.export_copy");
        }
        assert_ne!(l(Lang::En, "xlsx_readonly.read_only"), l(Lang::Ru, "xlsx_readonly.read_only"));
        assert_ne!(l(Lang::En, "xlsx_readonly.export_copy"), l(Lang::Ru, "xlsx_readonly.export_copy"));
        let (en, ru) = (crate::tr_lang(false, "xlsx_readonly.copied", &[("path", "P")]), crate::tr_lang(true, "xlsx_readonly.copied", &[("path", "P")]));
        assert!(en.contains('P') && ru.contains('P') && en != ru);
    }

    #[test]
    fn copy_action_is_offered_only_for_a_failed_export_of_a_held_book() {
        let ro = Some(super::super::xlsx_owner::READ_ONLY);
        let held = Some(super::super::xlsx_owner::OPEN_ELSEWHERE);
        assert!(copy_offered(ro, true) && copy_offered(held, true));
        assert!(!copy_offered(ro, false), "a failed read has no copy");
        assert!(!copy_offered(Some("disk full"), true) && !copy_offered(None, true));
    }

    #[test]
    fn copy_name_is_unique_and_beside_the_book() {
        let dir = std::env::temp_dir().join(format!("ddc_xs20_name_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("plan.xlsx");
        std::fs::write(&book, b"x").unwrap();
        let mut seen = vec![];
        for _ in 0..4 {
            let c = free_copy(&book).unwrap();
            assert_eq!(c.parent(), book.parent());
            assert!(!c.exists() && c != book && !seen.contains(&c), "{c:?}");
            std::fs::write(&c, b"x").unwrap();
            seen.push(c);
        }
        // a copy of a copy takes the next free name, never a taken one
        let again = free_copy(seen.last().unwrap()).unwrap();
        assert!(!again.exists() && !seen.contains(&again));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_export_offers_a_copy_and_links_it() {
        let mut app = EditorApp::new(editor_with_walls(), None, "test".into());
        let dir = std::env::temp_dir().join(format!("ddc_xs20_app_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("b.xlsx");
        let rev = app.ed.content_snap();
        assert!(matches!(xlsx_job::export_sync(&app.ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        let mut perm = std::fs::metadata(&book).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&book, perm).unwrap();
        // the failed export opens the panel with the copy action
        let out = xlsx_job::export_sync(&app.ed.model, &book, xlsx::Units::Metric, "en");
        assert!(matches!(&out, Outcome::Failed(e) if e == super::super::xlsx_owner::READ_ONLY));
        app.xlsx_finished(&book, false, rev, out);
        let pv = app.xlsx.as_ref().expect("panel with the error");
        assert!(copy_offered(pv.error.as_deref(), pv.export_failed));
        // the copy is written and linked
        let copy = free_copy(&book).unwrap();
        let o = Outcome::Copied(copy.clone());
        std::fs::write(&copy, b"x").unwrap();
        app.xlsx_finished(&book, true, rev, o);
        let pv = app.xlsx.as_ref().expect("copy linked");
        assert_eq!(pv.path, copy);
        assert!(pv.error.is_none() && !pv.export_failed && pv.synced);
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let mut perm = std::fs::metadata(&book).unwrap().permissions();
        perm.set_readonly(false);
        std::fs::set_permissions(&book, perm).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn denied_offers_other_folder() {
        let denied = Some(super::super::xlsx_owner::DENIED);
        let ro = Some(super::super::xlsx_owner::READ_ONLY);
        assert!(!copy_offered(denied, true), "a copy beside the book fails the same way");
        assert_eq!(way_out(denied, true), Some(("xlsx_write.save_to", ErrAct::ExportElsewhere)));
        assert_eq!(way_out(denied, false), None, "a failed read has no way out");
        assert_eq!(way_out(ro, true), Some(("xlsx_readonly.export_copy", ErrAct::ExportCopy)), "read-only file as before");
        assert_eq!(way_out(Some("disk full"), true), None);
        let e = super::super::xlsx_owner::DENIED;
        assert!(!is_locked(e));
        for lang in [Lang::En, Lang::Ru] {
            assert_eq!(book_error(e, lang), l(lang, "xlsx_write.denied"));
            assert_ne!(book_error(e, lang), l(lang, "xlsx_readonly.read_only"));
            assert_ne!(l(lang, "xlsx_write.save_to"), "xlsx_write.save_to");
        }
        assert_ne!(book_error(e, Lang::En), book_error(e, Lang::Ru));
    }

    #[test]
    fn save_to_target_skips_cancel_and_the_book_itself() {
        let book = std::env::temp_dir().join("plan.xlsx");
        assert_eq!(elsewhere_target(&book, None), None, "cancel writes nothing");
        assert_eq!(elsewhere_target(&book, Some(book.clone())), None);
        let other = std::env::temp_dir().join("other").join("plan");
        assert_eq!(elsewhere_target(&book, Some(other.clone())), Some(other.with_extension("xlsx")));
        assert!(writable_default().is_dir());
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn finish(app: &mut EditorApp) -> Outcome {
        let job = app.xlsx_job.take().expect("export started");
        for _ in 0..600 {
            if let Some(o) = job.poll() {
                return o;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("worker did not finish");
    }

    #[test]
    fn denied_export_saves_once_to_the_picked_file_and_keeps_the_book() {
        let mut app = EditorApp::new(editor_with_walls(), None, "test".into());
        let base = std::env::temp_dir().join(format!("ddc_xs37_app_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (here, there) = (base.join("here"), base.join("there"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        let book = here.join("b.xlsx");
        let rev = app.ed.content_snap();
        assert!(matches!(xlsx_job::export_sync(&app.ed.model, &book, xlsx::Units::Metric, "en"), Outcome::Done));
        let before = std::fs::read(&book).unwrap();
        let here_names = names(&here);
        let denied = super::super::xlsx_owner::DENIED;
        app.xlsx_finished(&book, false, rev, Outcome::Failed(denied.into()));
        let pv = app.xlsx.as_ref().expect("panel with the error");
        assert_eq!(way_out(pv.error.as_deref(), pv.export_failed).map(|w| w.1), Some(ErrAct::ExportElsewhere));
        // cancel: no job, nothing written anywhere
        app.xlsx_export_picked(&book, None, None);
        assert!(app.xlsx_job.is_none());
        assert!(names(&there).is_empty());
        // the picked file is written once and linked; the book stays as it was
        let picked = there.join("b.xlsx");
        app.xlsx_export_picked(&book, None, Some(picked.clone()));
        let out = finish(&mut app);
        assert!(matches!(&out, Outcome::Copied(c) if *c == picked), "the picked file is written");
        app.xlsx_finished(&book, true, rev, out);
        assert_eq!(app.xlsx.as_ref().map(|p| p.path.clone()), Some(picked.clone()));
        assert_eq!(names(&there), vec!["b.xlsx".to_string()]);
        assert_eq!(std::fs::read(&book).unwrap(), before, "original untouched");
        assert_eq!(names(&here), here_names);
        // a taken file is never overwritten
        let taken = std::fs::read(&picked).unwrap();
        app.xlsx_export_picked(&book, None, Some(picked.clone()));
        let again = finish(&mut app);
        assert!(matches!(&again, Outcome::Failed(e) if e == super::super::xlsx_owner::COPY_EXISTS));
        assert_eq!(std::fs::read(&picked).unwrap(), taken);
        assert_eq!(std::fs::read(&book).unwrap(), before);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn locked_workbook_gets_a_plain_message() {
        let os = "cannot open workbook: The process cannot access the file because it is being used by another process. (os error 32)";
        for lang in [Lang::En, Lang::Ru] {
            assert_eq!(book_error(os, lang), l(lang, "xlw.book_locked"));
        }
        assert_ne!(l(Lang::En, "xlw.book_locked"), l(Lang::Ru, "xlw.book_locked"));
        assert_eq!(book_error("value required", Lang::En), "value required");
    }

    #[test]
    fn diff_lengths_are_shown_in_project_units() {
        let u = crate::units::active_units();
        let want = crate::units::display_length(6.0, u);
        assert_ne!(want, "6");
        assert_eq!(shown_value("Walls", "ax [m]", "6"), want);
        assert_eq!(shown_value("Walls", "ax [mm]", "6"), want, "any book unit");
        assert_eq!(shown_value("Walls", "Code", "6"), "6", "parameters keep their text");
        assert_eq!(shown_value("Walls", "name", "6"), "6");
    }

    #[test]
    fn diff_lengths_group_thousands_in_the_ui_language() {
        if crate::units::active_units() != crate::units::Units::Metric {
            return;
        }
        crate::with_test_lang(false, || {
            assert_eq!(shown_value("Walls", "ax [m]", "3"), "3,000\u{a0}mm");
            assert_eq!(shown_value("Walls", "ax [m]", "-1.25"), "-1,250\u{a0}mm");
            assert_eq!(shown_value("Walls", "ax [m]", "1234.5"), "1,234,500\u{a0}mm");
            assert_eq!(shown_value("Walls", "ax [m]", "0.2"), "200\u{a0}mm");
        });
        crate::with_test_lang(true, || {
            let mm = "\u{43c}\u{43c}";
            assert_eq!(shown_value("Walls", "ax [m]", "3"), format!("3\u{a0}000\u{a0}{mm}"));
            assert_eq!(shown_value("Walls", "ax [m]", "-1.25"), format!("-1\u{a0}250\u{a0}{mm}"));
        });
        // stored cell text is untouched: only lengths of the format's own columns are formatted
        crate::with_test_lang(false, || assert_eq!(shown_value("Walls", "Code", "3000"), "3000"));
    }

    #[test]
    fn saved_workbook_is_noticed_by_the_poll() {
        let ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl4_{}.xlsx", std::process::id()));
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::linked(&p, false);
        let t0 = Instant::now();
        assert!(!pv.reload_due(t0), "unchanged file does nothing");
        pv.watch.seen = None;
        assert!(!pv.reload_due(t0 + xlsx::POLL), "first sight of a change only waits");
        assert!(pv.reload_due(t0 + xlsx::POLL + xlsx::SETTLE_STEP), "stable on the second check");
        assert!(pv.synced && pv.error.is_none());
    }

    /// One room "Hall" (101) in the model; the book holds it renamed "Lobby" with no id.
    fn room_book(ed: &mut Editor, tag: &str) -> (PathBuf, i64) {
        room_book_at(ed, tag, (1.0, 1.0))
    }

    /// `room_book` with the book row's point at `pt`. A row created in spite of the match
    /// needs a point outside Hall's walls: a second room in a held region is refused (e0acc221).
    fn room_book_at(ed: &mut Editor, tag: &str, pt: (f64, f64)) -> (PathBuf, i64) {
        use xlsx::TableCell as C;
        let id = ed
            .model
            .add_room(&ddc_model::RoomSpec {
                level: ed.level,
                name: "Hall".into(),
                number: "101".into(),
                point: (1.0, 1.0),
                height: 3.0,
            })
            .unwrap();
        ed.reload().unwrap();
        let lv: String = ed
            .model
            .sql()
            .query_row("SELECT name FROM level WHERE id=?1", [ed.level], |r| r.get(0))
            .unwrap();
        let p = std::env::temp_dir().join(format!("ddc_app_ri1_{tag}_{}.xlsx", std::process::id()));
        let heads = ["id", "level", "name", "number", "point x [m]", "point y [m]", "height [m]"]
            .map(String::from);
        let row = vec![
            C::Text(String::new()),
            C::Text(lv),
            C::Text("Lobby".into()),
            C::Text("101".into()),
            C::Num(pt.0),
            C::Num(pt.1),
            C::Num(3.0),
        ];
        xlsx::export_table(&p, "Rooms", &heads, &[], &[row], None).unwrap();
        (p, id)
    }

    fn room_name(ed: &Editor, id: i64) -> String {
        ed.model
            .sql()
            .query_row("SELECT name FROM room WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    }

    fn room_count(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM room", [], |r| r.get(0))
            .unwrap()
    }

    fn wait(job: &XlsxLoad) -> Result<xlsx_load::Loaded, String> {
        for _ in 0..600 {
            if let Some(o) = job.poll() {
                return o;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("worker did not finish");
    }

    #[test]
    fn reload_runs_on_the_worker_and_applies_once() {
        let ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_rl1_{}.xlsx", std::process::id()));
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::linked(&p, false);
        pv.synced = false;
        let (take, dec) = (pv.take.clone(), pv.decisions.clone());
        let rev = ed.content_snap();
        let job = XlsxLoad::start_with(&ed.model, &p, false, rev, take, dec, true).unwrap();
        assert!(job.reload);
        let out = wait(&job);
        let gen = pv.report_gen;
        assert_eq!(pv.finish_reload(out, job.rev), Reloaded::Applied);
        assert!(pv.synced && pv.error.is_none());
        assert!(pv.report_gen > gen, "a background re-read starts a fresh change table");
        assert_eq!(pv.rev, rev);
        // the channel is spent: a second poll cannot hand the same result again
        assert!(matches!(job.poll(), Some(Err(_))));
    }

    #[test]
    fn cancelled_reload_keeps_the_previous_preview() {
        let ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_rl2_{}.xlsx", std::process::id()));
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::linked(&p, false);
        pv.synced = false;
        pv.take.all = true;
        pv.rev = ed.content_snap();
        let got = pv.finish_reload(Err(ddc_io::job::CANCELLED.to_string()), ed.content_snap());
        assert_eq!(got, Reloaded::Cancelled);
        assert!(!pv.synced && pv.take.all && pv.error.is_none());
        // the old report may not match the book any more: Apply re-reads before applying
        assert_eq!(pv.rev, None);
    }

    fn xs12_book(tag: &str, owner: Option<(u64, u64)>) -> (Editor, PathBuf) {
        let ed = editor_with_walls();
        let d = std::env::temp_dir().join(format!("ddc_xs12_win_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("sync.xlsx");
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let age = |f: &Path, secs: u64| {
            let h = std::fs::OpenOptions::new().write(true).open(f).unwrap();
            h.set_modified(web_time::SystemTime::now() - std::time::Duration::from_secs(secs)).unwrap();
        };
        if let Some((owner_age, book_age)) = owner {
            let o = super::super::xlsx_owner::owner_file(&p);
            std::fs::write(&o, b"owner").unwrap();
            age(&o, owner_age);
            age(&p, book_age);
        }
        (ed, p)
    }

    #[test]
    fn sync_with_owner_file_says_open_elsewhere_not_no_changes() {
        let (mut ed, p) = xs12_book("held", Some((0, 60)));
        let owner = super::super::xlsx_owner::owner_file(&p);
        let before = (std::fs::read(&owner).unwrap(), std::fs::metadata(&owner).unwrap().modified().unwrap());
        let mut pv = XlsxPreview::linked(&p, false);
        pv.synced = true;
        pv.reload(&mut ed.model);
        assert!(pv.is_held());
        assert_eq!(pv.error.as_deref(), Some(super::super::xlsx_owner::OPEN_ELSEWHERE));
        assert_eq!(pv.rev, None, "the report is stale: Apply reads again first");
        assert!(!pv.export_failed);
        // the line shown to the user, without touching the global language
        let shown = book_error(pv.error.as_deref().unwrap(), Lang::En);
        assert!(shown.contains("open elsewhere"), "{shown}");
        assert!(error_buttons(false).iter().any(|(_, a)| *a == ErrAct::Retry));
        assert_eq!(apply_begin(&mut ed, &mut pv, Lang::En).err().unwrap(), super::super::xlsx_owner::OPEN_ELSEWHERE);
        // the lock file was only read
        let after = (std::fs::read(&owner).unwrap(), std::fs::metadata(&owner).unwrap().modified().unwrap());
        assert_eq!(before, after);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn sync_without_owner_file_reads_normally() {
        let (mut ed, p) = xs12_book("free", None);
        let mut pv = XlsxPreview::linked(&p, false);
        pv.reload(&mut ed.model);
        assert!(!pv.is_held() && pv.error.is_none());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn sync_with_stale_owner_file_reads_with_a_soft_gate() {
        let (mut ed, p) = xs12_book("stale", Some((600, 10)));
        let mut pv = XlsxPreview::linked(&p, false);
        assert_eq!(pv.hold(), super::super::xlsx_owner::Gate::Soft);
        pv.reload(&mut ed.model);
        assert!(!pv.is_held() && pv.error.is_none(), "read went through");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn read_anyway_lets_the_held_book_through_and_a_save_clears_the_line() {
        let (mut ed, p) = xs12_book("anyway", Some((0, 60)));
        let mut pv = XlsxPreview::linked(&p, false);
        pv.reload(&mut ed.model);
        assert!(pv.is_held());
        super::super::xlsx_owner::read_anyway(&p);
        pv.reload(&mut ed.model);
        assert!(!pv.is_held() && pv.error.is_none());
        // the same choice lets Apply through: the user is never stuck behind the lock file
        let r = apply_begin(&mut ed, &mut pv, Lang::En);
        assert_ne!(r.err().as_deref(), Some(super::super::xlsx_owner::OPEN_ELSEWHERE));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn saved_book_newer_than_owner_file_clears_the_held_line() {
        let (mut ed, p) = xs12_book("saved", Some((0, 60)));
        let mut pv = XlsxPreview::linked(&p, false);
        pv.reload(&mut ed.model);
        assert!(pv.is_held());
        // saved in the other program: the book is now newer than its owner file
        let o = super::super::xlsx_owner::owner_file(&p);
        let h = std::fs::OpenOptions::new().write(true).open(&o).unwrap();
        h.set_modified(web_time::SystemTime::now() - std::time::Duration::from_secs(30)).unwrap();
        drop(h);
        let h = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        h.set_modified(web_time::SystemTime::now()).unwrap();
        drop(h);
        pv.reload(&mut ed.model);
        assert!(!pv.is_held() && pv.error.is_none());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn share_locked_book_gives_the_open_elsewhere_line() {
        use std::os::windows::fs::OpenOptionsExt;
        let (mut ed, p) = xs12_book("share", None);
        let mut pv = XlsxPreview::linked(&p, false);
        {
            let _h = std::fs::OpenOptions::new().read(true).share_mode(0).open(&p).unwrap();
            pv.reload(&mut ed.model);
            assert!(pv.is_held(), "clean error, not a raw os text");
        }
        pv.reload(&mut ed.model);
        assert!(!pv.is_held());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn locked_book_is_retried_not_reported_as_broken() {
        let ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_rl3_{}.xlsx", std::process::id()));
        xlsx::export_xlsx(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::linked(&p, false);
        pv.watch.seen = None;
        let e = "The process cannot access the file (os error 32)".to_string();
        assert_eq!(pv.finish_reload(Err(e), ed.content_snap()), Reloaded::Locked);
        assert!(pv.error.is_none(), "no error shown on the preview");
        assert!(pv.watch.seen.is_none(), "not marked as read: the poll comes back to it");
        let t0 = Instant::now();
        assert!(!pv.reload_due(t0));
        assert!(pv.reload_due(t0 + xlsx::SETTLE_STEP), "read again once it is stable");
    }

    fn op_count(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM op", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn xlsx_pending_match_blocks_apply_until_decided() {
        let mut ed = editor_with_walls();
        let (p, id) = room_book(&mut ed, "pend");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!(pv.report.pending_matches(), 1);
        assert!(!pv.synced, "a pending match keeps the preview open");
        let ops = op_count(&ed);
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Err(UNDECIDED.to_string()));
        assert_eq!((op_count(&ed), room_name(&ed, id).as_str()), (ops, "Hall"));
    }

    #[test]
    fn xlsx_accept_match_is_one_undo_step() {
        let mut ed = editor_with_walls();
        let (p, id) = room_book(&mut ed, "acc");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.decide(&mut ed.model, "Rooms", 2, true);
        pv.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut pv);
        assert_eq!(pv.report.pending_matches(), 0);
        let ops = op_count(&ed);
        let hand_made = std::fs::read(&p).unwrap();
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Ok(Applied::Done { reexported: true }));
        assert_eq!((op_count(&ed), room_count(&ed)), (ops + 1, 1));
        assert_eq!(room_name(&ed, id), "Lobby");
        // a book this app did not make is never replaced: the refreshed book is a copy beside
        // it, linked from now on, and the original keeps its bytes
        assert_ne!(pv.path, p);
        assert_eq!(std::fs::read(&p).unwrap(), hand_made);
        ed.undo();
        assert_eq!((room_name(&ed, id).as_str(), room_count(&ed)), ("Hall", 1));
        // the linked book got the id: reading it again proposes nothing
        let again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert!(again.report.matches.is_empty(), "{:?}", again.report.matches);
    }

    #[test]
    fn xlsx_reject_match_creates_new() {
        let mut ed = editor_with_walls();
        let (p, id) = room_book_at(&mut ed, "rej", (10.0, 1.0));
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.decide(&mut ed.model, "Rooms", 2, false);
        pv.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut pv);
        assert_eq!(pv.report.matches.len(), 1);
        assert_eq!(pv.report.matches[0].state, MatchState::Rejected);
        assert_eq!(pv.report.counts().added, 1);
        let ops = op_count(&ed);
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Ok(Applied::Done { reexported: true }));
        assert_eq!((op_count(&ed), room_count(&ed)), (ops + 1, 2));
        assert_eq!(room_name(&ed, id), "Hall");
        ed.undo();
        assert_eq!(room_count(&ed), 1);
    }

    #[test]
    fn xlsx_reject_keeps_row_and_decision_can_flip_back() {
        let mut ed = editor_with_walls();
        let (p, _) = room_book_at(&mut ed, "flip", (10.0, 1.0));
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.decide(&mut ed.model, "Rooms", 2, false);
        assert_eq!(pv.report.matches.len(), 1);
        assert_eq!(pv.report.matches[0].state, MatchState::Rejected);
        assert_eq!((pv.report.accepted_matches(), pv.report.pending_matches()), (0, 0));
        assert_eq!(pv.report.counts().added, 1);
        pv.decide(&mut ed.model, "Rooms", 2, true);
        assert!(pv.decisions.reject.is_empty());
        assert_eq!(pv.report.matches[0].state, MatchState::Accepted);
        assert_eq!((pv.report.accepted_matches(), pv.report.counts().added), (1, 0));
        pv.decide(&mut ed.model, "Rooms", 2, false);
        assert_eq!(pv.report.matches[0].state, MatchState::Rejected);
        assert!(pv.decisions.accept.is_empty());
    }

    #[test]
    fn dup_copies_as_new_decision_is_undone_in_one_step() {
        let mut ed = editor_with_walls();
        let mut pv = book_preview(&mut ed, "dupundo");
        // a key-match rejection taken before stays when the copies choice is taken back
        pv.decisions.reject.insert(("Rooms".to_string(), 7));
        let rows = vec![("Walls".to_string(), 3), ("Walls".to_string(), 9)];
        pv.dup_decide(rows.clone());
        pv.dup_decide(rows.clone());
        assert_eq!(pv.dup_new, rows, "a second click adds nothing");
        assert!(rows.iter().all(|k| pv.decisions.reject.contains(k)));
        assert!(pv.dup_undo());
        assert!(pv.dup_new.is_empty());
        assert_eq!(pv.decisions.reject.len(), 1);
        assert!(pv.decisions.reject.contains(&("Rooms".to_string(), 7)));
        assert!(!pv.dup_undo(), "nothing left to undo");
    }

    fn hud_and_toast_rects(w: f32, h: f32, status: f32, rows: usize) -> (egui::Rect, egui::Rect) {
        let hud_h = 120.0;
        let hud_w = ddc_ui::theme::layout::DIALOG_FORM;
        let stack = ddc_ui::widgets::toasts::stack_height(rows, false);
        let off = hud_offset(status, stack);
        let hud = egui::Rect::from_min_size(
            egui::pos2(w + off[0] - hud_w, h + off[1] - hud_h),
            egui::vec2(hud_w, hud_h),
        );
        let tw = ddc_ui::theme::layout::TOAST_W;
        let tb = h - status - ddc_ui::theme::space::SM;
        let toasts = egui::Rect::from_min_size(
            egui::pos2(w - ddc_ui::theme::space::MD - tw, tb - stack),
            egui::vec2(tw, stack),
        );
        (hud, toasts)
    }

    #[test]
    fn hud_stays_clear_of_toasts_and_status_bar() {
        for (w, h, scale) in [(1280.0, 720.0, 1.0), (1920.0, 1080.0, 1.0), (1280.0, 720.0, 2.0)] {
            let (w, h) = (w / scale, h / scale);
            let (hud, toasts) = hud_and_toast_rects(w, h, 24.0, 3);
            assert!(!hud.intersects(toasts), "{w}x{h}");
            assert!(hud.max.y <= h - 24.0, "{w}x{h}");
        }
    }

    #[test]
    fn hud_rect_clear_of_measured_stack() {
        // a measured stack taller than the estimate (two-line + action toasts)
        let measured = ddc_ui::widgets::toasts::stack_height(2, false) + 40.0;
        let (w, h, status) = (1280.0_f32, 720.0_f32, 24.0_f32);
        let off = hud_offset(status, measured);
        let hud_bottom = h + off[1];
        let toasts_top = h - status - ddc_ui::theme::space::SM - measured;
        assert!(hud_bottom <= toasts_top, "{w}");
    }

    #[test]
    fn xlsx_win_has_no_off_grid_literals() {
        let src = include_str!("xlsx_win.rs");
        let prod = &src[..src.find("#[cfg(test)]").unwrap()];
        let bad = ["add_space(", "Margin::symmetric(", ".spacing(["];
        for (n, line) in prod.lines().enumerate() {
            for p in bad {
                if let Some(i) = line.find(p) {
                    let arg = &line[i + p.len()..];
                    assert!(
                        !arg.starts_with(|c: char| c.is_ascii_digit() || c == '-'),
                        "line {}: literal spacing: {line}",
                        n + 1
                    );
                }
            }
        }
        assert!(!prod.contains("RIGHT_BOTTOM,
                [-"));
    }

    /// A book exported from the model, then its first wall edited in the book (to 4.0) and in
    /// the editor (to 3.5): the same cell changed on both sides.
    fn conflict_book(ed: &mut Editor, tag: &str) -> (PathBuf, i64) {
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, dst) = (dir.join("a.xlsx"), dir.join("b.xlsx"));
        xlsx::export_xlsx_families(&ed.model, &src, xlsx::Units::Metric, &crate::elem::sheets::families())
            .unwrap();
        let (id, _) = first_wall(ed);
        xlsx::patch_cell(&src, &dst, "Walls", id, "height [m]", 4.0).unwrap();
        ed.model
            .sql()
            .execute("UPDATE wall SET height=3.5 WHERE id=?1", [id])
            .unwrap();
        (dst, id)
    }

    #[test]
    fn reexport_after_apply_uses_locale_captions() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl9_{}.xlsx", std::process::id()));
        let fams = crate::elem::sheets::families();
        let en = crate::shell::xlsx_style::style("en");
        xlsx::export_xlsx_styled(&ed.model, &p, &p, xlsx::Units::Metric, &fams, &en).unwrap();
        let (id, _) = first_wall(&ed);
        let q = p.with_file_name(format!("ddc_app_xl9_{}_e.xlsx", std::process::id()));
        xlsx::patch_cell(&p, &q, "Walls", id, "height [m]", 4.0).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &q, false).unwrap();
        pv.rev = ed.content_snap();
        // the interface language is English but the project locale code is "ru"
        apply_preview_in(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En, "ru").unwrap();
        let ru = crate::shell::xlsx_style::style("ru");
        let (cap_en, cap_ru) = (en.captions["height"].clone(), ru.captions["height"].clone());
        assert_ne!(cap_en, cap_ru);
        let heads = xlsx::sheet_headers(&q, "Walls").unwrap();
        assert!(heads.iter().any(|h| h.starts_with(cap_ru.as_str())), "{heads:?}");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&q);
    }

    #[test]
    fn reexport_after_apply_keeps_style_and_baseline() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl7_{}.xlsx", std::process::id()));
        xlsx::export_xlsx_styled(
            &ed.model,
            &p,
            &p,
            xlsx::Units::Metric,
            &crate::elem::sheets::families(),
            &crate::shell::xlsx_style::style("ru"),
        )
        .unwrap();
        let (id, _) = first_wall(&ed);
        let q = p.with_file_name(format!("ddc_app_xl7_{}_e.xlsx", std::process::id()));
        // the styled book carries the caption, not the raw key
        let cap = format!("{} [m]", crate::tr_lang(true, "xlc.height", &[]));
        xlsx::patch_cell(&p, &q, "Walls", id, &cap, 4.0).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &q, false).unwrap();
        pv.rev = ed.content_snap();
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::Ru).unwrap();
        assert_eq!(height(&ed, id), 4.0);
        let n = xlsx::sheet_names(&q).unwrap();
        assert!(n.iter().any(|s| s == "Keys") && n.iter().any(|s| s == "Lists"), "{n:?}");
        let again = XlsxPreview::load(&mut ed.model, &q, false).unwrap();
        assert_eq!(again.report.counts(), xlsx::Counts::default());
        assert!(again.report.conflicts.is_empty());
    }

    #[test]
    fn reexport_keeps_conflict_cells() {
        let mut ed = editor_with_walls();
        let (p, id) = conflict_book(&mut ed, "keep");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!(pv.report.conflicts.len(), 1, "{:?}", pv.report);
        assert!(!pv.report.conflicts[0].take_book);
        assert!(pv.report.batch().is_empty(), "nothing to apply for a kept conflict");
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert_eq!(height(&ed, id), 3.5, "the editor value stays");
        assert!(!pv.synced, "an unresolved conflict means not in sync");
        assert_eq!(pv.report.conflicts.len(), 1, "the conflict is shown again");
        // the book keeps the user's value and the old export value: the next read still conflicts
        let again = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(again.report.conflicts.len(), 1, "{:?}", again.report.conflicts);
        assert_eq!(again.report.conflicts[0].book, "4");
        assert_eq!(again.report.counts(), xlsx::Counts::default());
    }

    #[test]
    fn os_error_50_not_locked() {
        let lock = |c: u32| book_error(&format!("cannot open workbook: x (os error {c})"), Lang::En);
        let held = l(Lang::En, "xlw.book_locked").to_string();
        assert_eq!(lock(5), held);
        assert_eq!(lock(32), held);
        assert_ne!(lock(50), held, "50 is not 5");
        assert_ne!(lock(320), held);
        assert_eq!(os_error_code("failed (os error 50)"), Some(50));
    }

    #[test]
    fn taking_the_book_value_is_one_undo_step_with_the_other_edits() {
        let mut ed = editor_with_walls();
        let (p, id) = conflict_book(&mut ed, "take");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!(pv.report.conflicts.len(), 1);
        pv.take.set_all(true);
        pv.reload(&mut ed.model);
        pv.rev = ed.content_snap();
        assert!(pv.report.conflicts[0].take_book);
        assert_eq!(pv.report.counts().changed, 1, "{:?}", pv.report.changes);
        let ops = op_count(&ed);
        assert_eq!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En),
            Ok(Applied::Done { reexported: false })
        );
        assert_eq!(height(&ed, id), 4.0, "the book value was taken");
        assert_eq!(op_count(&ed), ops + 1, "one operation");
        ed.undo();
        assert_eq!(height(&ed, id), 3.5, "one undo restores the editor value");
        // per cell: choosing "keep" for the only cell out of "all" leaves the model alone
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.take.set_all(true);
        let cell = {
            let c = &pv.report.conflicts;
            c.first().map(|c| (c.sheet.clone(), c.row, c.col))
        };
        if let Some(cell) = cell {
            pv.take.set(cell.clone(), false, &[cell]);
            pv.reload(&mut ed.model);
            assert!(pv.report.batch().is_empty());
        }
    }

    #[test]
    fn bad_cell_is_listed_as_skipped_and_the_rest_can_apply() {
        let mut ed = editor_with_walls();
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xlsx");
        let heads = ["id", "level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]", "height [m]"]
            .map(String::from);
        let mut rows = vec![];
        {
            use xlsx::TableCell as C;
            let mut st = ed
                .model
                .sql()
                .prepare(
                    "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w \
                     JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
                )
                .unwrap();
            let all: Vec<Vec<C>> = st
                .query_map([], |r| {
                    let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64), C::Text(r.get(1)?), C::Text(r.get(2)?)];
                    for i in 3..9 {
                        v.push(C::Num(r.get(i)?));
                    }
                    Ok(v)
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for (i, mut r) in all.into_iter().enumerate() {
                match i {
                    0 => r[8] = C::Text("not a length".into()),
                    1 => r[8] = C::Num(3.3),
                    _ => {}
                }
                rows.push(r);
            }
        }
        xlsx::export_table(&p, "Walls", &heads, &[], &rows, None).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        assert_eq!((pv.report.blocking(), pv.report.skipped()), (0, 1), "{:?}", pv.report.rejected);
        assert_eq!(pv.report.counts().changed, 1);
        let (id0, _) = first_wall(&ed);
        super::super::xlsx_no_base::confirm(&mut pv);
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert_eq!(height(&ed, id0), 3.0, "the bad cell leaves the model value");
        let n: i64 = ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM wall WHERE abs(height-3.3)<1e-9", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the good cell of the next row applied");
    }

    /// Book of `ed` whose Walls sheet keeps only its first `keep` rows, as a user's row delete
    /// leaves it. The baseline still lists every wall: a wall the book never had is not a
    /// deletion (22b83543), so a book of a smaller model would delete nothing.
    fn book_without_rows(ed: &Editor, p: &Path, keep: usize) {
        use calamine::{Data, Reader};
        let full = p.with_extension("full.xlsx");
        xlsx::export_xlsx_families(&ed.model, &full, xlsx::Units::Metric, &crate::elem::sheets::families())
            .unwrap();
        let mut wb: calamine::Xlsx<_> = calamine::open_workbook(&full).unwrap();
        let mut out = rust_xlsxwriter::Workbook::new();
        for name in wb.sheet_names() {
            let range = wb.worksheet_range(&name).unwrap();
            let (r0, c0) = range.start().unwrap_or((0, 0));
            let ws = out.add_worksheet();
            ws.set_name(&name).unwrap();
            let rows = range.rows().take(if name == "Walls" { keep + 1 } else { usize::MAX });
            for (i, row) in rows.enumerate() {
                for (j, d) in row.iter().enumerate() {
                    let (i, j) = (r0 + i as u32, (c0 as usize + j) as u16);
                    match d {
                        Data::Float(x) => ws.write_number(i, j, *x).map(|_| ()),
                        Data::Int(x) => ws.write_number(i, j, *x as f64).map(|_| ()),
                        Data::Bool(b) => ws.write_boolean(i, j, *b).map(|_| ()),
                        Data::String(s) => ws.write_string(i, j, s).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
            }
        }
        out.save(p).unwrap();
    }

    /// Book of 4 walls without its last 2 rows, read with `delete_missing`: 2 deletions.
    fn deleting_preview(tag: &str) -> (Editor, XlsxPreview) {
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xlsx");
        let mut ed = editor_with_walls();
        book_without_rows(&ed, &p, 2);
        let mut pv = XlsxPreview::load(&mut ed.model, &p, true).unwrap();
        pv.rev = ed.content_snap();
        (ed, pv)
    }

    #[test]
    fn export_over_an_edited_linked_book_needs_confirm_and_leaves_it_untouched() {
        // the over-card guard covers every export target, previewed or not
        let mut ed = editor_with_walls();
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_gate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, dst) = (dir.join("a.xlsx"), dir.join("b.xlsx"));
        xlsx::export_xlsx_families(&ed.model, &src, xlsx::Units::Metric, &crate::elem::sheets::families())
            .unwrap();
        let (id, _) = first_wall(&ed);
        xlsx::patch_cell(&src, &dst, "Walls", id, "height [m]", 4.0).unwrap();
        let (bytes, mtime) = (
            std::fs::read(&dst).unwrap(),
            std::fs::metadata(&dst).unwrap().modified().unwrap(),
        );
        let g = xlsx_scope::guard(&mut ed.model, &dst, None, |m, p| {
            read(m, p, false, &xlsx::Take::default(), &KeyDecisions::default())
        })
        .unwrap();
        assert!(matches!(g, xlsx_scope::Guard::Ask(_)));
        assert_eq!(std::fs::read(&dst).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&dst).unwrap().modified().unwrap(), mtime);
        // a new file never asks
        let g = xlsx_scope::guard(&mut ed.model, &dir.join("c.xlsx"), None, |m, p| {
            read(m, p, false, &xlsx::Take::default(), &KeyDecisions::default())
        })
        .unwrap();
        assert!(matches!(g, xlsx_scope::Guard::Write));
    }

    #[test]
    fn apply_with_deletions_needs_one_tick() {
        let (mut ed, mut pv) = deleting_preview("arm");
        assert_eq!(pv.report.counts().deleted, 2, "{:?}", pv.report.changes);
        let (ops, rev, walls) = (op_count(&ed), ed.revision, wall_count(&ed));
        let n = pv.report.counts().deleted;
        // one tick on screen; unticked a click does nothing
        assert_eq!(super::super::xlsx_gate::ticks(&pv.report, pv.delete_missing), 1);
        assert!(!super::super::xlsx_gate::go(pv.confirm_mass, n, true));
        assert_eq!((op_count(&ed), ed.revision), (ops, rev));
        // ticked, the first click applies; the apply itself is unchanged: same rows deleted
        pv.confirm_mass = true;
        assert!(super::super::xlsx_gate::go(pv.confirm_mass, n, true));
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert!(op_count(&ed) > ops);
        assert_eq!(wall_count(&ed), walls - n as i64);
        assert!(!pv.confirm_mass, "the tick is used once");
    }

    /// Model of `n` walls; the book lists only the first `keep`, read with "delete missing".
    fn partial_book_preview(n: usize, keep: usize, tag: &str) -> (Editor, XlsxPreview) {
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_mass_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.xlsx");
        let mut ed = editor_n_walls(n);
        book_without_rows(&ed, &p, keep);
        let mut pv = XlsxPreview::load(&mut ed.model, &p, true).unwrap();
        pv.rev = ed.content_snap();
        (ed, pv)
    }

    #[test]
    fn mass_delete_counted_per_sheet_over_the_threshold() {
        for (keep, mass) in [(9usize, false), (8, false), (7, true), (1, true)] {
            let (_, pv) = partial_book_preview(10, keep, &format!("thr{keep}"));
            let d = pv.report.deletions.iter().find(|d| d.sheet == "Walls");
            let d = d.unwrap_or_else(|| panic!("keep {keep}: {:?}", pv.report.deletions));
            assert_eq!((d.deleted, d.total), (10 - keep, 10), "keep {keep}");
            assert_eq!(!pv.report.mass_deletes().is_empty(), mass, "keep {keep}");
        }
        // without "delete missing" nothing is counted
        let (mut ed, pv) = partial_book_preview(10, 5, "off");
        let off = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert!(off.report.deletions.is_empty());
    }

    #[test]
    fn mass_delete_blocks_apply_until_confirmed() {
        let (mut ed, mut pv) = partial_book_preview(10, 5, "block");
        let (ops, walls) = (op_count(&ed), wall_count(&ed));
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Err(MASS_UNCONFIRMED.to_string()));
        assert_eq!((op_count(&ed), wall_count(&ed)), (ops, walls), "nothing deleted unconfirmed");
        assert_eq!(pv.report.counts().deleted, 5, "the preview stays whole");
        pv.confirm_mass = true;
        assert!(matches!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En),
            Ok(Applied::Done { .. })
        ));
        assert_eq!(wall_count(&ed), 5);
        assert!(!pv.confirm_mass, "a confirmation is used once");
        // a rebuilt report asks again
        pv.confirm_mass = true;
        pv.reload(&mut ed.model);
        assert!(!pv.confirm_mass);
    }

    #[test]
    fn small_deletion_needs_no_mass_confirmation() {
        let (mut ed, mut pv) = partial_book_preview(10, 9, "small");
        assert!(pv.report.mass_deletes().is_empty());
        assert!(matches!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En),
            Ok(Applied::Done { .. })
        ));
        assert_eq!(wall_count(&ed), 9);
    }

    #[test]
    fn delete_missing_with_baseline_is_in_effect_as_before() {
        use super::super::xlsx_no_base as nb;
        let (mut ed, mut pv) = partial_book_preview(10, 9, "xs44_on");
        assert!(!pv.report.no_base && nb::effective_delete(&pv));
        assert_eq!(pv.report.counts().deleted, 1);
        assert!(super::super::xlsx_gate::plain_tick(&pv.report, nb::effective_delete(&pv)));
        assert!(!nb::deletes_without_base(&pv.report));
        assert!(matches!(
            apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En),
            Ok(Applied::Done { .. })
        ));
        assert_eq!(wall_count(&ed), 9);
    }

    #[test]
    fn apply_refuses_deletions_of_a_report_without_baseline() {
        use super::super::xlsx_no_base as nb;
        // guard behind the reader: a no-base report carrying deletions never reaches the model
        let (mut ed, mut pv) = partial_book_preview(10, 9, "xs44_refuse");
        assert!(!pv.report.deletions.is_empty() || pv.report.counts().deleted > 0);
        pv.report.no_base = true;
        assert!(!nb::effective_delete(&pv) && nb::deletes_without_base(&pv.report));
        nb::confirm(&mut pv);
        pv.confirm_mass = true;
        let (ops, walls) = (op_count(&ed), wall_count(&ed));
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En);
        assert_eq!(r, Err(nb::NO_BASE_DELETE.to_string()));
        assert_eq!((op_count(&ed), wall_count(&ed)), (ops, walls), "nothing deleted");
    }

    #[test]
    fn sync_never_reuses_the_delete_flag() {
        let (_, pv) = partial_book_preview(4, 2, "sync");
        assert!(pv.delete_missing);
        remember(&pv.path);
        assert_eq!(sync_target(), Some((pv.path.clone(), false)));
        assert_eq!(last_book(), Some(pv.path.clone()));
    }

    #[test]
    fn keys_never_confirm_a_deletion() {
        assert!(!pointer_click(true, true, false));
        assert!(!pointer_click(true, false, true));
        assert!(pointer_click(true, false, false));
    }

    #[test]
    fn apply_label_carries_the_count_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            assert!(apply_label(lang, 9, 7).contains('7'));
            assert!(apply_label(lang, 9, 0).contains('9'));
            assert_eq!(apply_label(lang, 0, 0), l(lang, "xlw.apply"));
            assert_ne!(apply_label(lang, 2, 2), apply_label(lang, 2, 0));
            assert_ne!(apply_label(lang, 2, 0), apply_label(lang, 0, 0));
        }
    }

    #[test]
    fn deleted_lines_list_five_and_count_the_rest() {
        let (ed, pv) = deleting_preview("lines");
        let (lines, more) = deleted_lines(&ed.model, &pv.report, 1);
        assert_eq!((lines.len(), more), (1, 1), "{lines:?}");
        assert!(lines[0].starts_with("Walls #"), "{lines:?}");
        assert!(lines[0].contains("ext"), "type is listed: {lines:?}");
        let (lines, more) = deleted_lines(&ed.model, &pv.report, 5);
        assert_eq!((lines.len(), more), (2, 0));
    }

    #[test]
    fn free_copy_name_does_not_stack_suffixes() {
        let dir = std::env::temp_dir().join(format!("ddc_app_xl_copy_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("plan.xlsx");
        assert_eq!(free_copy(&a), Some(dir.join("plan-synced.xlsx")));
        assert_eq!(free_copy(&dir.join("plan-synced.xlsx")), Some(dir.join("plan-synced.xlsx")));
        assert_eq!(free_copy(&dir.join("plan-synced-3.xlsx")), Some(dir.join("plan-synced.xlsx")));
    }

    #[cfg(windows)]
    #[test]
    fn locked_book_gets_a_refreshed_copy_beside_it() {
        use std::os::windows::fs::OpenOptionsExt;
        let mut ed = editor_with_walls();
        let (p, id) = conflict_book(&mut ed, "lock");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        pv.take.set_all(true);
        pv.reload(&mut ed.model);
        pv.rev = ed.content_snap();
        // held as a spreadsheet program holds it: readable, not replaceable. A book nobody can
        // read gives no copy at all, its user columns would be lost (14039ab8)
        let lock = std::fs::OpenOptions::new().read(true).share_mode(1).open(&p).unwrap();
        apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        let copy = pv.copy.clone().expect("a copy was written");
        assert!(copy.exists() && copy != p && pv.path == copy, "{copy:?}");
        drop(lock);
        assert_eq!(height(&ed, id), 4.0);
        let again = XlsxPreview::load(&mut ed.model, &copy, false).unwrap();
        assert!(again.report.conflicts.is_empty() && again.report.counts() == xlsx::Counts::default());
    }

    fn busy_app(tag: &str) -> (EditorApp, PathBuf) {
        let mut app = EditorApp::new(editor_with_walls(), None, "test".into());
        let dir = std::env::temp_dir().join(format!("ddc_xl11_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("b.xlsx");
        let rev = app.ed.content_snap();
        let job = XlsxJob::start_copy(&app.ed.model, &book, false, xlsx::Units::Metric, "en", rev)
            .unwrap();
        app.xlsx_job = Some(job);
        (app, book)
    }

    #[test]
    fn second_export_says_busy() {
        let (mut app, book) = busy_app("busy");
        app.ed.status = None;
        let other = book.with_file_name("c.xlsx");
        app.xlsx_run(&other, false, None);
        let want = l(app.xlang(), "xlw.export_running");
        assert_eq!(app.ed.status.as_deref(), Some(want));
        assert!(!other.exists());
        assert_eq!(app.xlsx_job.as_ref().map(|j| j.target.clone()), Some(book));
    }

    #[test]
    fn apply_disabled_while_export_runs() {
        let (mut app, book) = busy_app("applyoff");
        let rev = app.ed.content_snap();
        for run in [
            (|a: &mut EditorApp| a.xlsx_sync()) as fn(&mut EditorApp),
            |a: &mut EditorApp| a.xlsx_export(),
            |a: &mut EditorApp| {
                let mut pv = XlsxPreview::linked(&PathBuf::from("none.xlsx"), false);
                a.xlsx_reexport(&mut pv);
            },
        ] {
            app.ed.status = None;
            run(&mut app);
            assert_eq!(app.ed.status.as_deref(), Some(l(app.xlang(), "xlw.export_running")));
        }
        assert_eq!(app.ed.content_snap(), rev, "the model is untouched");
        assert_eq!(app.xlsx_job.as_ref().map(|j| j.target.clone()), Some(book));
    }

    #[test]
    fn apply_reexport_runs_on_worker_and_keeps_conflict() {
        let mut ed = editor_with_walls();
        let (p, id) = conflict_book(&mut ed, "worker");
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        let opts = apply_begin(&mut ed, &mut pv, Lang::En).unwrap().expect("not stale");
        assert!(opts.apply.as_ref().is_some_and(|t| t.kept_any));
        assert_eq!(height(&ed, id), 3.5);
        let rev = ed.content_snap();
        let job = XlsxJob::start_for(&ed.model, &p, true, xlsx::Units::Metric, "en", rev, opts)
            .unwrap();
        let out = loop {
            if let Some(o) = job.poll() {
                break o;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(matches!(out, Outcome::Done), "worker export finished");
        assert!(!p.with_file_name("b.export-part.xlsx").exists());
        // the conflict survives the re-export: the book still disagrees with the model
        let again = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(again.report.conflicts.len(), 1);
    }

    #[test]
    fn reopened_scoped_book_reexport_stays_scoped() {
        let mut ed = editor_with_walls();
        let l1 = ed.level;
        let l2 = ed.model.add_level("L2", 3.0).unwrap();
        let t: i64 = ed.model.sql().query_row("SELECT id FROM wall_type LIMIT 1", [], |r| r.get(0)).unwrap();
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a: (0., 0.),
                b: (3., 0.),
                level: l2,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_scope_{}.xlsx", std::process::id()));
        let sc = xlsx::Scope { label: "L1".into(), level: Some(l1), ..Default::default() };
        let st = xlsx::Style::default().with_scope(sc.clone());
        xlsx::export_xlsx_styled(&ed.model, &p, &p, xlsx::Units::Metric, &[], &st).unwrap();
        // reopened from disk: the scope comes back from the book, not from the session
        let pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(pv.scope, Some(sc));
        let opts = xlsx_job::JobOpts { scope: pv.scope.clone(), ..Default::default() };
        let rev = ed.content_snap();
        let job = XlsxJob::start_for(&ed.model, &p, true, xlsx::Units::Metric, "en", rev, opts)
            .unwrap();
        let out = loop {
            if let Some(o) = job.poll() {
                break o;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(matches!(out, Outcome::Done));
        let again = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(again.report.base_ids.walls, vec![1, 2, 3, 4], "level 2 wall 5 stays out");
        assert!(again.scope.is_some());
    }

    fn over_app(tag: &str) -> (EditorApp, PathBuf) {
        let app = EditorApp::new(editor_with_walls(), None, "test".into());
        let dir = std::env::temp_dir().join(format!("ddc_xl18_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (app, dir)
    }

    fn export_book(ed: &Editor, path: &Path) {
        xlsx::export_xlsx_families(&ed.model, path, xlsx::Units::Metric, &crate::elem::sheets::families()).unwrap();
    }

    #[test]
    fn loss_refusal_opens_the_card_with_the_list() {
        let (mut app, dir) = over_app("loss");
        let book = dir.join("b.xlsx");
        let items = vec!["macros of the workbook".to_string()];
        let e = xlsx::refusal(&items, &[]).unwrap();
        let rev = app.ed.content_snap();
        app.xlsx_finished(&book, true, rev, Outcome::Failed(e));
        let c = app.xlsx_loss.as_ref().expect("the user decides");
        assert_eq!((c.path.as_path(), c.again, &c.items), (book.as_path(), true, &items));
        // any other failure opens no card
        app.xlsx_loss = None;
        app.xlsx_finished(&book, true, rev, Outcome::Failed("disk full".into()));
        assert!(app.xlsx_loss.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_over_book_with_pending_edits_asks() {
        let (mut app, dir) = over_app("pending");
        let (src, book) = (dir.join("src.xlsx"), dir.join("b.xlsx"));
        export_book(&app.ed, &src);
        let (id, _) = first_wall(&app.ed);
        xlsx::patch_cell(&src, &book, "Walls", id, "height [m]", 4.0).unwrap();
        let before = std::fs::read(&book).unwrap();
        app.xlsx_export_to(&book, None);
        let c = app.xlsx_over.clone().expect("the card asks");
        assert!(c.ours && c.pending >= 1, "{c:?}");
        assert!(app.xlsx_job.is_none(), "nothing is written before the answer");
        assert_eq!(std::fs::read(&book).unwrap(), before);
        assert_ne!(c.name, book);
        assert!(!c.name.exists());
    }

    #[test]
    fn scoped_export_over_whole_book_counts_out_of_scope_rows() {
        let (mut app, dir) = over_app("outside");
        let book = dir.join("b.xlsx");
        export_book(&app.ed, &book);
        let sc = xlsx::Scope { cats: Some(vec![xlsx::Cat::Rooms]), label: "rooms".into(), ..Default::default() };
        app.xlsx_export_to(&book, Some(sc));
        let c = app.xlsx_over.clone().expect("the card asks");
        assert!(c.ours);
        assert_eq!((c.pending, c.outside), (0, 4), "the four walls are not in a rooms-only book");
        assert!(app.xlsx_job.is_none());
    }

    #[test]
    fn replace_moves_old_book_to_archive() {
        let (mut app, dir) = over_app("replace");
        let (src, book) = (dir.join("src.xlsx"), dir.join("b.xlsx"));
        export_book(&app.ed, &src);
        let (id, _) = first_wall(&app.ed);
        xlsx::patch_cell(&src, &book, "Walls", id, "height [m]", 4.0).unwrap();
        let before = std::fs::read(&book).unwrap();
        app.xlsx_export_to(&book, None);
        let c = app.xlsx_over.take().expect("the card asks");
        app.xlsx_replace(c);
        let kept = dir.join("_archive").join("b.xlsx");
        assert_eq!(std::fs::read(&kept).unwrap(), before, "the old book is intact in _archive");
        assert_eq!(app.xlsx_job.as_ref().map(|j| j.target.clone()), Some(book));
        assert!(app.ed.status.as_deref().is_some_and(|s| s.contains("_archive")), "{:?}", app.ed.status);
    }

    #[test]
    fn clean_book_overwritten_without_card() {
        let (mut app, dir) = over_app("clean");
        let book = dir.join("b.xlsx");
        export_book(&app.ed, &book);
        app.xlsx_export_to(&book, None);
        assert!(app.xlsx_over.is_none());
        assert_eq!(app.xlsx_job.as_ref().map(|j| j.target.clone()), Some(book));
    }

    #[test]
    fn foreign_book_never_replaced() {
        let (mut app, dir) = over_app("foreign");
        // a book of another project, and a workbook this program never wrote
        let other = EditorApp::new(editor_with_walls(), None, "test".into());
        let (theirs, plain) = (dir.join("theirs.xlsx"), dir.join("plain.xlsx"));
        export_book(&other.ed, &theirs);
        xlsx::export_table(&plain, "Data", &["a".to_string()], &[""], &[], None).unwrap();
        for book in [theirs, plain] {
            let before = std::fs::read(&book).unwrap();
            app.xlsx_export_to(&book, None);
            let c = app.xlsx_over.take().expect("the card asks");
            assert!(!c.ours, "{book:?}");
            assert_ne!(c.name, book);
            assert!(app.xlsx_job.is_none());
            assert_eq!(std::fs::read(&book).unwrap(), before);
        }
    }

    fn scoped_pv_with_added_row(ed: &mut Editor, tag: &str) -> XlsxPreview {
        let p = std::env::temp_dir().join(format!("ddc_app_xl19_{tag}_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(pv.report.counts().added, 1);
        pv.scope = Some(xlsx::Scope {
            label: "all".into(),
            selected: Some(xlsx::Selected::of_model(&ed.model).unwrap()),
            ..Default::default()
        });
        pv.rev = ed.content_snap();
        // the demo book has no baseline: these tests are about created rows, not that guard
        super::super::xlsx_no_base::confirm(&mut pv);
        pv
    }

    fn wall_count(ed: &Editor) -> i64 {
        ed.model.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn apply_after_read_error_is_not_failure() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "a");
        let n = wall_count(&ed);
        let r = apply_begin_with(&mut ed, &mut pv, Lang::En, |_| Err("boom".into()));
        let opts = r.expect("committed Apply is not an error").expect("not stale");
        assert_eq!(opts.apply.as_ref().map(|t| t.created), Some(1));
        assert_eq!(wall_count(&ed), n + 1);
        assert!(!pv.synced, "the book lacks the new rows");
        assert!(pv.report.edits.is_empty() && pv.report.counts().added == 0);
        assert_eq!(pv.created, 1);
    }

    #[test]
    fn second_apply_after_error_creates_nothing() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "b");
        apply_begin_with(&mut ed, &mut pv, Lang::En, |_| Err("boom".into())).unwrap();
        let n = wall_count(&ed);
        pv.rev = ed.content_snap();
        let _ = apply_begin(&mut ed, &mut pv, Lang::En);
        assert_eq!(wall_count(&ed), n, "no row is created twice");
    }

    #[test]
    fn scope_kept_with_warning_on_read_error() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "c");
        let kept = pv.scope.clone();
        apply_begin_with(&mut ed, &mut pv, Lang::En, |_| Err("boom".into())).unwrap();
        assert_eq!(pv.scope, kept, "scope stays as it was");
        let want = crate::tr_lang(false, "xlw.new_rows_not_in_book", &[("n", "1")]);
        assert!(!want.starts_with("xlw.") && want.contains('1'), "key exists");
        // the warning is the tail of the Apply status line the editor writes (bf5389a8)
        let st = ed.status.clone().unwrap_or_default();
        assert!(st.ends_with(&format!(" · {want}")), "{st}");
        assert_eq!(st.matches(want.as_str()).count(), 1, "{st}");
    }

    #[test]
    fn apply_during_reload_window_rereads_first() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "rw");
        let n = wall_count(&ed);
        // the user saves the book again before the debounced reload ran: the added row is gone
        xlsx::export_xlsx(&ed.model, &pv.path).unwrap();
        let saved = Stamp::of(&pv.path);
        assert_ne!(pv.watch.seen, saved, "the save is not read yet");
        let r = apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap();
        assert!(r.is_none(), "nothing applied from the old report");
        assert_eq!(wall_count(&ed), n, "the stale added row is not created");
        assert_eq!(pv.report.counts().added, 0, "the report is the saved book");
        assert_eq!(pv.watch.seen, saved);
        assert_eq!(pv.rev, ed.content_snap());
    }

    #[test]
    fn apply_never_marks_unread_version_read() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "mk");
        let read = pv.watch.seen;
        let p = pv.path.clone();
        // the book is saved while the Apply runs, after its stamp check
        let r = apply_begin_with(&mut ed, &mut pv, Lang::En, |m| {
            let _ = xlsx::export_xlsx(m, &p);
            xlsx::Selected::of_model(m)
        });
        assert!(r.unwrap().is_some(), "the Apply itself went through");
        assert_ne!(Stamp::of(&p), read, "the book was rewritten");
        assert_eq!(pv.watch.seen, read, "only the version the report came from counts as read");
        let t0 = Instant::now();
        let due = (0..=xlsx::SETTLE_CHECKS).any(|i| pv.reload_due(t0 + (xlsx::POLL + xlsx::SETTLE_STEP) * (i + 1)));
        assert!(due, "the unread save is picked up by the poll");
    }

    #[test]
    fn read_after_error_keeps_created_no_reexport() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "e");
        let opts = apply_begin_with(&mut ed, &mut pv, Lang::En, |_| Err("boom".into())).unwrap().unwrap();
        let tail = opts.apply.clone().unwrap();
        assert!(tail.skip && tail.created == 1);
        assert!(!pv.synced && pv.created == 1);
        // the finish for a skipped re-export neither resets the counter nor reports ids
        let rev = ed.content_snap();
        let reexported = apply_finish(rev, &mut ed.model, &mut pv, &tail, &Outcome::Cancelled);
        assert!(!reexported && !pv.synced && pv.created == 1);
        // the sync path runs no job and says so
        let mut pv2 = scoped_pv_with_added_row(&mut ed, "e2");
        let n = wall_count(&ed);
        let r = apply_begin_with(&mut ed, &mut pv2, Lang::En, |_| Err("boom".into()));
        assert!(r.unwrap().unwrap().apply.unwrap().skip);
        assert_eq!(wall_count(&ed), n + 1);
        assert_eq!(pv2.created, 1);
    }

    #[test]
    fn read_after_error_note_survives_dropped_note() {
        let mut app = EditorApp::new(editor_with_walls(), None, "test".into());
        let mut pv = scoped_pv_with_added_row(&mut app.ed, "f");
        apply_begin_with(&mut app.ed, &mut pv, Lang::En, |_| Err("boom".into())).unwrap();
        let want = crate::tr_lang(false, "xlw.new_rows_not_in_book", &[("n", "1")]);
        let old = std::path::PathBuf::from("old.xlsx");
        app.add_status(&xlsx_job::dropped_note(false, &["Extra".to_string()], &old));
        let st = app.ed.status.clone().unwrap_or_default();
        assert!(st.contains(&want), "{st}");
        assert!(st.contains("Extra"), "{st}");
    }

    #[test]
    fn read_after_ok_reexports_new_rows() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "g");
        let opts = apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        let tail = opts.apply.clone().unwrap();
        assert!(!tail.skip && tail.created == 1);
        assert!(pv.synced && pv.created == 1);
        let rev = ed.content_snap();
        let reexported = apply_finish(rev, &mut ed.model, &mut pv, &tail, &Outcome::Done);
        assert!(reexported && pv.created == 0 && pv.synced);
    }

    fn made_count(m: &xlsx::Made) -> usize {
        m.values().map(Vec::len).sum()
    }

    fn made_of(ed: &Editor, pv: &XlsxPreview) -> xlsx::Made {
        crate::shell::xlsx_links::load(&ed.model, &pv.path)
    }

    #[test]
    fn second_apply_after_reopen_creates_nothing() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "h");
        let n = wall_count(&ed);
        apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        assert_eq!(wall_count(&ed), n + 1);
        let newest: i64 = ed.model.sql().query_row("SELECT max(id) FROM wall", [], |r| r.get(0)).unwrap();
        let made = made_of(&ed, &pv);
        assert_eq!(made_count(&made), 1);
        assert_eq!(made.values().flatten().map(|h| h.1).next(), Some(newest));
        // the app restarts / the book is opened again: a fresh preview of the same, unchanged book
        let mut again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert_eq!(again.report.counts().added, 0, "the row is not offered again");
        assert!(
            again.report.ignored.iter().any(|i| i.reason == format!("row already created as {newest}, export the workbook again")),
            "{:?}",
            again.report.ignored
        );
        again.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut again);
        let _ = apply_begin(&mut ed, &mut again, Lang::En);
        assert_eq!(wall_count(&ed), n + 1, "nothing is created twice");
    }

    #[test]
    fn created_rows_cleared_by_reexport() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "i");
        let opts = apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        let tail = opts.apply.clone().unwrap();
        assert_eq!(made_count(&made_of(&ed, &pv)), 1);
        // a re-export that did not happen keeps the record
        apply_finish(ed.content_snap(), &mut ed.model, &mut pv, &tail, &Outcome::Cancelled);
        assert_eq!(made_count(&made_of(&ed, &pv)), 1);
        assert!(apply_finish(ed.content_snap(), &mut ed.model, &mut pv, &tail, &Outcome::Done));
        assert!(made_of(&ed, &pv).is_empty());
    }

    #[test]
    fn copy_after_locked_apply_keeps_original_records() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "k");
        let n = wall_count(&ed);
        let opts = apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        let tail = opts.apply.clone().unwrap();
        assert_eq!(wall_count(&ed), n + 1);
        let original = pv.path.clone();
        // the book was held open: the re-export went to a -synced copy, the original is unchanged
        let copy = free_copy(&original).unwrap();
        let o = Outcome::Copied(copy.clone());
        assert!(apply_finish(ed.content_snap(), &mut ed.model, &mut pv, &tail, &o));
        assert_eq!(pv.path, copy);
        assert_eq!(made_count(&crate::shell::xlsx_links::load(&ed.model, &original)), 1);
        // the user re-imports the original book: its created row is known, nothing is added
        let mut again = XlsxPreview::load(&mut ed.model, &original, false).unwrap();
        assert_eq!(again.report.counts().added, 0, "{:?}", again.report.changes);
        again.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut again);
        let _ = apply_begin(&mut ed, &mut again, Lang::En);
        assert_eq!(wall_count(&ed), n + 1, "nothing is created twice");
    }

    #[test]
    fn undone_apply_forgets_created_rows() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "j");
        let n = wall_count(&ed);
        apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        ed.undo();
        assert_eq!(wall_count(&ed), n);
        assert!(made_of(&ed, &pv).is_empty(), "the element is gone, the row may be created again");
        let again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert_eq!(again.report.counts().added, 1);
    }

    /// Walls sheet of the model with the id-less rows `extra` (ax, bx) appended in order.
    fn walls_book(ed: &Editor, path: &Path, extra: &[(f64, f64)]) {
        use xlsx::TableCell as C;
        let mut st = ed
            .model
            .sql()
            .prepare(
                "SELECT w.id,lv.name,t.name,w.ax,w.ay,w.bx,w.by,w.base_offset,w.height FROM wall w                  JOIN level lv ON lv.id=w.level JOIN wall_type t ON t.id=w.wtype ORDER BY w.id",
            )
            .unwrap();
        let mut rows: Vec<Vec<C>> = st
            .query_map([], |r| {
                let mut v = vec![C::Num(r.get::<_, i64>(0)? as f64), C::Text(r.get(1)?), C::Text(r.get(2)?)];
                for i in 3..9 {
                    v.push(C::Num(r.get(i)?));
                }
                Ok(v)
            })
            .unwrap()
            .flatten()
            .collect();
        for (ax, bx) in extra {
            let mut v = rows[0].clone();
            v[0] = C::Text(String::new());
            v[3] = C::Num(*ax);
            v[5] = C::Num(*bx);
            rows.push(v);
        }
        let heads = ["id", "level", "type", "ax [m]", "ay [m]", "bx [m]", "by [m]", "base_offset [m]", "height [m]"].map(String::from);
        xlsx::export_table(path, "Walls", &heads, &[], &rows, None).unwrap();
    }

    fn applied_ignored(pv: &XlsxPreview) -> usize {
        pv.report.ignored.iter().filter(|i| i.reason.starts_with("row already created as")).count()
    }

    #[test]
    fn row_inserted_above_matches_by_fingerprint() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl30_a_{}.xlsx", std::process::id()));
        walls_book(&ed, &p, &[(12.0, 16.0)]);
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        assert_eq!(pv.report.counts().added, 1);
        pv.rev = ed.content_snap();
        let n = wall_count(&ed);
        super::super::xlsx_no_base::confirm(&mut pv);
        apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        assert_eq!(wall_count(&ed), n + 1);
        // a row is inserted above the created one; the book is otherwise as it was (the four
        // walls it had, not the model's created wall, which only a re-export would add)
        walls_book(&editor_with_walls(), &p, &[(30.0, 34.0), (12.0, 16.0)]);
        let mut again = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        let added: Vec<_> = again.report.changes.iter().filter(|c| c.kind() == "added").collect();
        assert_eq!(added.len(), 1, "{:?}", again.report.changes);
        assert_eq!(added[0].row, 6, "the inserted row is the new one");
        assert_eq!(applied_ignored(&again), 1, "the moved row is the one refused");
        again.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut again);
        apply_begin_with(&mut ed, &mut again, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        assert_eq!(wall_count(&ed), n + 2);
        let moved: i64 = ed.model.sql().query_row("SELECT count(*) FROM wall WHERE ax=12", [], |r| r.get(0)).unwrap();
        assert_eq!(moved, 1, "the moved row is not created twice");
    }

    #[test]
    fn undo_preview_redo_no_duplicate() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "u");
        let n = wall_count(&ed);
        apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        ed.undo();
        assert_eq!(wall_count(&ed), n);
        // a preview between the undo and the redo drops the stale record
        let between = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert_eq!(between.report.counts().added, 1);
        ed.redo();
        assert_eq!(wall_count(&ed), n + 1);
        assert_eq!(made_count(&made_of(&ed, &pv)), 1, "the redo remembers the row again");
        let again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        assert_eq!(again.report.counts().added, 0, "{:?}", again.report.changes);
        assert_eq!(applied_ignored(&again), 1);
    }

    #[test]
    fn idless_row_refused_rest_applied() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "k");
        let n = wall_count(&ed);
        apply_begin_with(&mut ed, &mut pv, Lang::En, xlsx::Selected::of_model).unwrap().unwrap();
        // the model drifts away from the book's second row; the book is applied again
        let ids: Vec<i64> = {
            let mut st = ed.model.sql().prepare("SELECT id FROM wall ORDER BY id").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().flatten().collect()
        };
        ed.model.sql().execute("UPDATE wall SET height=2.0 WHERE id=?1", [ids[1]]).unwrap();
        let mut again = XlsxPreview::load(&mut ed.model, &pv.path, false).unwrap();
        let c = again.report.counts();
        assert_eq!((c.added, c.changed), (0, 1), "{:?}", again.report.changes);
        assert_eq!(again.report.ignored.iter().filter(|i| i.reason.starts_with("row already created as")).count(), 1);
        assert_eq!(again.report.blocking() + again.report.skipped(), 0);
        again.rev = ed.content_snap();
        // no baseline: the drifted model value is overwritten only after the explicit confirmation
        assert!(again.report.no_base);
        super::super::xlsx_no_base::confirm(&mut again);
        apply_begin(&mut ed, &mut again, Lang::En).unwrap().expect("not stale");
        assert_eq!(wall_count(&ed), n + 1, "the refused row is not created");
        let h: f64 = ed.model.sql().query_row("SELECT height FROM wall WHERE id=?1", [ids[1]], |r| r.get(0)).unwrap();
        assert!((h - 3.2).abs() < 1e-9, "the other row applied: {h}");
    }

    #[test]
    fn read_after_error_without_created_says_so() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "l");
        // nothing new in this Apply
        pv.report.changes.retain(|c| c.kind() != "added");
        pv.report.edits.retain(|e| !matches!(e, ddc_model::Edit::NewWall { .. } | ddc_model::Edit::Mark { .. }));
        let n = wall_count(&ed);
        let opts = apply_begin_with(&mut ed, &mut pv, Lang::En, |_| Err("boom".into())).unwrap().unwrap();
        assert!(!opts.apply.unwrap().skip, "no new rows: the re-export still runs");
        assert_eq!(wall_count(&ed), n);
        let want = l(Lang::En, "xlw.scope_not_extended");
        assert!(!want.starts_with("xlw."), "key exists");
        assert!(ed.status.as_deref().is_some_and(|s| s.contains(want)), "{:?}", ed.status);
    }

    #[test]
    fn apply_reload_error_after_commit_is_not_failure() {
        let mut ed = editor_with_walls();
        let mut pv = scoped_pv_with_added_row(&mut ed, "d");
        let n = wall_count(&ed);
        // the reload after the commit reads saved views and fails
        let sql = |ed: &Editor, q: &str| ed.model.sql().execute_batch(q).unwrap();
        sql(&ed, "ALTER TABLE view_def RENAME TO zz_view_def");
        let r = apply_begin(&mut ed, &mut pv, Lang::En);
        sql(&ed, "ALTER TABLE zz_view_def RENAME TO view_def");
        r.expect("committed Apply is not an error").expect("not stale");
        assert_eq!(wall_count(&ed), n + 1);
        assert!(pv.report.edits.is_empty() && pv.report.counts().added == 0);
        let st = ed.status.clone().unwrap_or_default();
        assert!(st.starts_with(l(Lang::En, "xlw.applied_not_refreshed")), "{st}");
        assert!(!l(Lang::En, "xlw.applied_not_refreshed").starts_with("xlw."), "key exists");
        // one undo step takes the whole Apply back
        ed.undo();
        assert_eq!(wall_count(&ed), n);
    }

    #[test]
    fn failed_read_offers_retry_and_the_workbook_and_retry_clears_it() {
        assert_eq!(
            error_buttons(false).iter().map(|b| b.1).collect::<Vec<_>>(),
            [ErrAct::Retry, ErrAct::OpenBook]
        );
        assert_eq!(error_buttons(true).len(), 1);
        for k in ["xlw.retry_now", "xlw.open_workbook", "xlw.error_resave_note"] {
            assert_ne!(l(Lang::En, k), l(Lang::Ru, k), "{k}");
        }
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_ux12_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.error = Some("cannot open workbook".into());
        pv.synced = false;
        pv.report = Report::default();
        pv.reload(&mut ed.model);
        assert!(pv.error.is_none());
        assert_eq!(pv.report.counts().added, 1);
    }

    #[test]
    fn apply_success_toast_carries_undo_that_restores_the_values() {
        let mut ed = editor_with_walls();
        let p = std::env::temp_dir().join(format!("ddc_app_xl_ux13_{}.xlsx", std::process::id()));
        demo_book(&ed.model, &p).unwrap();
        let walls = ed.walls.len();
        let mut pv = XlsxPreview::load(&mut ed.model, &p, false).unwrap();
        pv.rev = ed.content_snap();
        super::super::xlsx_no_base::confirm(&mut pv);
        let r = apply_preview(&mut ed, &mut pv, xlsx::Units::Metric, Lang::En).unwrap();
        assert!(matches!(r, Applied::Done { .. }));
        assert_eq!(ed.walls.len(), walls + 1);
        let mut toasts = ddc_ui::notify::Toasts::default();
        done_toast(&mut toasts, Lang::En, &ed.status, true);
        let t = toasts.live.last().unwrap();
        assert_eq!(t.command.as_deref(), Some(super::super::xlsx_undo::CMD));
        assert!(t.text.contains("Ctrl+Z") && t.action.is_some());
        ed.undo();
        assert_eq!(ed.walls.len(), walls);
    }

    /// Context with the named font weights bound (segment labels): a bare one panics, and
    /// fonts installed in the same pass bind only on the next one.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn key_match(state: MatchState) -> xlsx::KeyMatch {
        xlsx::KeyMatch {
            sheet: "rooms".into(),
            row: 3,
            id: 7,
            kind: "room",
            key: "A-101".into(),
            state,
            changes: vec![],
        }
    }

    #[test]
    fn match_row_idle_frames_decide_nothing_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            for state in [MatchState::Pending, MatchState::Accepted, MatchState::Rejected] {
                let ctx = font_ctx();
                let m = key_match(state);
                let c = [egui::Color32::GREEN, egui::Color32::YELLOW, egui::Color32::GRAY, egui::Color32::WHITE];
                for _ in 0..3 {
                    let mut got = Some(true);
                    let _ = ctx.run(Default::default(), |cx| {
                        egui::CentralPanel::default().show(cx, |ui| got = match_row(ui, &m, lang, c));
                    });
                    assert_eq!(got, None, "{lang:?} {state:?}");
                }
                assert_eq!(m.state, state);
            }
        }
    }

    fn shape_texts(s: &egui::Shape, out: &mut Vec<(String, egui::Pos2, egui::Vec2)>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|x| shape_texts(x, out)),
            egui::Shape::Text(t) => out.push((t.galley.text().to_string(), t.pos, t.galley.size())),
            _ => {}
        }
    }

    /// Runs one frame of a match row; returns the decision and the text shapes it painted.
    fn row_frame(
        ctx: &egui::Context,
        m: &xlsx::KeyMatch,
        lang: Lang,
        events: Vec<egui::Event>,
    ) -> (Option<bool>, Vec<(String, egui::Pos2, egui::Vec2)>) {
        let c = [egui::Color32::GREEN, egui::Color32::YELLOW, egui::Color32::GRAY, egui::Color32::WHITE];
        let mut got = None;
        let raw = egui::RawInput { events, ..Default::default() };
        let out = ctx.run(raw, |cx| {
            egui::CentralPanel::default().show(cx, |ui| got = match_row(ui, m, lang, c));
        });
        let mut texts = vec![];
        out.shapes.iter().for_each(|cs| shape_texts(&cs.shape, &mut texts));
        (got, texts)
    }

    #[test]
    fn clicking_each_segment_returns_its_exact_decision() {
        for lang in [Lang::En, Lang::Ru] {
            for (label_key, want) in [("xlsx_confirm.accept", true), ("xlsx_confirm.reject", false)] {
                let ctx = font_ctx();
                let m = key_match(MatchState::Pending);
                let mut texts = vec![];
                for _ in 0..3 {
                    let (got, t) = row_frame(&ctx, &m, lang, vec![]);
                    assert_eq!(got, None, "idle frame decides nothing");
                    texts = t;
                }
                let label = l(lang, label_key);
                let (_, pos, size) = texts.iter().find(|(t, _, _)| t == label).expect("segment label painted").clone();
                let at = pos + size / 2.0;
                let btn = |pressed| egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                };
                let mut all = vec![];
                all.push(row_frame(&ctx, &m, lang, vec![egui::Event::PointerMoved(at)]).0);
                all.push(row_frame(&ctx, &m, lang, vec![btn(true)]).0);
                all.push(row_frame(&ctx, &m, lang, vec![btn(false)]).0);
                let got: Vec<bool> = all.into_iter().flatten().collect();
                assert_eq!(got, vec![want], "{lang:?} {label_key}");
            }
        }
    }

    #[test]
    fn rendering_match_rows_leaves_import_settings_unchanged() {
        let mut ed = editor_with_walls();
        let mut pv = book_preview(&mut ed, "ux58a");
        pv.report.matches.push(key_match(MatchState::Pending));
        pv.report.matches.push(key_match(MatchState::Accepted));
        let before = (
            pv.delete_missing,
            format!("{:?}", pv.decisions),
            pv.report.matches.clone(),
            pv.report.proposals.iter().map(|p| p.accept).collect::<Vec<_>>(),
            ImportOptions { apply: false, delete_missing: pv.delete_missing, new_params: false },
        );
        for lang in [Lang::En, Lang::Ru] {
            let ctx = font_ctx();
            for _ in 0..3 {
                for m in &pv.report.matches {
                    assert_eq!(row_frame(&ctx, m, lang, vec![]).0, None);
                }
            }
        }
        let after = (
            pv.delete_missing,
            format!("{:?}", pv.decisions),
            pv.report.matches.clone(),
            pv.report.proposals.iter().map(|p| p.accept).collect::<Vec<_>>(),
            ImportOptions { apply: false, delete_missing: pv.delete_missing, new_params: false },
        );
        assert_eq!(before, after);
    }

    #[test]
    fn error_row_buttons_idle_frames_click_nothing_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            let ctx = font_ctx();
            let mut keys: Vec<&str> = error_buttons(true).into_iter().map(|(k, _)| k).collect();
            keys.extend(error_buttons(false).into_iter().map(|(k, _)| k));
            keys.extend(["xlw.read_anyway", "xlsx_readonly.show_in_folder"]);
            for _ in 0..3 {
                let mut clicked = false;
                let _ = ctx.run(Default::default(), |cx| {
                    let th = ddc_ui::theme::of(cx);
                    egui::CentralPanel::default().show(cx, |ui| {
                        for k in &keys {
                            clicked |= ddc_ui::widgets::Button::quiet(l(lang, k)).small().show(ui, &th).clicked();
                        }
                    });
                });
                assert!(!clicked, "{lang:?}");
            }
            for k in &keys {
                assert!(!l(lang, k).is_empty(), "{lang:?} {k}");
            }
        }
    }

    use crate::layout_contract as lc;
    use egui::accesskit::Role as AkRole;

    /// One footer frame: Cancel and Apply on the right, the tail to their left.
    fn tail_frame(
        ctx: &egui::Context,
        th: &mut ddc_ui::Theme,
        ru: bool,
        toggle: MissingToggle,
        events: Vec<egui::Event>,
        flipped: &mut Option<bool>,
    ) -> lc::Probe {
        let id = egui::Id::new("tail_probe");
        let mut f = |cx: &egui::Context| {
            egui::CentralPanel::default().show(cx, |ui| {
                let key = "xlw.one_undo_step_ctrl_z";
                let note = Some((crate::tr_str(ru, key).to_string(), egui::Color32::GRAY));
                kit::dialog_footer(ui, |ui| {
                    kit::primary_button(ui, "Apply", true);
                    kit::cancel_button(ui, crate::tr_str(ru, "xlw.cancel"));
                    if let Some(on) = footer_tail(ui, ru, toggle, note) {
                        *flipped = Some(on);
                    }
                });
            });
        };
        lc::pass(ctx, th, id, events, &mut f)
    }

    fn click_at(at: egui::Pos2) -> Vec<Vec<egui::Event>> {
        let b = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![vec![egui::Event::PointerMoved(at), b(true)], vec![b(false)]]
    }

    fn settled(ru: bool, toggle: MissingToggle) -> (egui::Context, ddc_ui::Theme, lc::Probe) {
        let (ctx, mut th) = lc::driven_ctx();
        let mut none = None;
        let mut p = lc::Probe::default();
        for _ in 0..4 {
            p = tail_frame(&ctx, &mut th, ru, toggle, vec![], &mut none);
            assert!(none.is_none(), "idle frame must not toggle, ru={ru}");
        }
        (ctx, th, p)
    }

    #[test]
    fn footer_checkbox_has_role_label_and_shares_the_buttons_row_en_ru() {
        for ru in [false, true] {
            for on in [false, true] {
                let (_, _, p) = settled(ru, MissingToggle::Free { on, busy: false });
                let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
                let boxed = p.controls.iter().find(|c| c.label == label).expect("checkbox");
                assert_eq!(boxed.role, AkRole::CheckBox, "ru={ru}");
                let cancel = p.controls.iter().find(|c| c.label == crate::tr_str(ru, "xlw.cancel"));
                let cancel = cancel.expect("cancel");
                let apply = p.controls.iter().find(|c| c.label == "Apply").expect("apply");
                for c in [cancel, apply] {
                    let dy = (c.rect.center().y - boxed.rect.center().y).abs();
                    assert!(dy <= 1.0, "ru={ru} on={on}: one row, dy={dy}");
                }
                assert!(boxed.rect.right() <= cancel.rect.left(), "checkbox left of buttons");
                let note = crate::tr_str(ru, "xlw.one_undo_step_ctrl_z");
                let n = p.spans.iter().find(|(t, _)| t == note).expect("note painted").1;
                assert!((n.center().y - cancel.rect.center().y).abs() <= 1.5, "note centre y");
                assert!(n.left() >= boxed.rect.right() - 0.5, "note right of the checkbox");
                assert!(n.right() <= cancel.rect.left() + 0.5, "note left of the buttons");
            }
        }
    }

    #[test]
    fn footer_checkbox_flips_on_click_and_reports_the_new_state() {
        for ru in [false, true] {
            for on in [false, true] {
                let toggle = MissingToggle::Free { on, busy: false };
                let (ctx, mut th, p) = settled(ru, toggle);
                let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
                let at = p.controls.iter().find(|c| c.label == label).unwrap().rect.center();
                let mut flipped = None;
                for ev in click_at(at) {
                    let _ = tail_frame(&ctx, &mut th, ru, toggle, ev, &mut flipped);
                }
                assert_eq!(flipped, Some(!on), "ru={ru} on={on}");
            }
        }
    }

    #[test]
    fn footer_checkbox_is_inert_while_busy_or_without_baseline() {
        for ru in [false, true] {
            for toggle in [MissingToggle::Free { on: false, busy: true }, MissingToggle::NoBase] {
                let (ctx, mut th, p) = settled(ru, toggle);
                let label = crate::tr_str(ru, "xlsx_delete.delete_missing");
                let c = p.controls.iter().find(|c| c.label == label).expect("shown");
                assert_eq!(c.role, AkRole::CheckBox);
                let mut flipped = None;
                for ev in click_at(c.rect.center()) {
                    let _ = tail_frame(&ctx, &mut th, ru, toggle, ev, &mut flipped);
                }
                assert_eq!(flipped, None, "ru={ru} {toggle:?}");
            }
        }
    }

    #[test]
    fn footer_has_no_checkbox_when_nothing_is_missing() {
        for ru in [false, true] {
            let (_, _, p) = settled(ru, MissingToggle::Hidden);
            assert!(p.controls.iter().all(|c| c.role != AkRole::CheckBox), "ru={ru}");
            let note = crate::tr_str(ru, "xlw.one_undo_step_ctrl_z");
            assert!(p.text.iter().any(|t| t == note), "note still painted, ru={ru}");
        }
    }

    #[test]
    fn long_note_is_elided_not_wrapped_and_keeps_the_row() {
        let (ctx, mut th) = lc::driven_ctx();
        let long = "x ".repeat(200);
        let mut p = lc::Probe::default();
        for _ in 0..4 {
            let mut f = |cx: &egui::Context| {
                egui::CentralPanel::default().show(cx, |ui| {
                    kit::dialog_footer(ui, |ui| {
                        kit::cancel_button(ui, "Cancel");
                        let note = Some((long.clone(), egui::Color32::GRAY));
                        let free = MissingToggle::Free { on: false, busy: false };
                        footer_tail(ui, false, free, note);
                    });
                });
            };
            p = lc::pass(&ctx, &mut th, egui::Id::new("tail_long"), vec![], &mut f);
        }
        let cancel = p.controls.iter().find(|c| c.label == "Cancel").expect("cancel");
        let n = p.spans.iter().find(|(t, _)| t.starts_with("x ")).expect("note").1;
        assert!(n.height() < 24.0, "one line, h={}", n.height());
        assert!(n.right() <= cancel.rect.left() + 0.5, "stays left of the buttons");
        assert!(p.elided.iter().any(|t| t.starts_with("x ")), "cut with an ellipsis");
    }

    #[test]
    fn footer_has_exactly_one_primary_in_every_state() {
        use super::super::xlsx_head::{footer_actions, FootIn, Role};
        for exported in [false, true] {
            for synced in [false, true] {
                for model_changed in [false, true] {
                    for changes in [0, 3] {
                        for to_review in [0, 2] {
                            for missing in [0, 4] {
                                let i = FootIn { exported, synced, model_changed, changes, to_review, missing };
                                let a = footer_actions(&i);
                                assert_eq!(a.iter().filter(|x| x.role == Role::Primary).count(), 1, "{i:?}");
                                assert_eq!(a[0].role, Role::Primary, "primary first (far right): {i:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn removed_chip_without_check_area_leaves_no_goto() {
        use super::super::xlsx_counts::{chip_label, Chip};
        // no area: the click leaves no jump and keeps the filter, for any chip
        for g in [CheckGoto::Top, CheckGoto::Proposals, CheckGoto::Matches] {
            assert_eq!(chip_goto(Some(g), false), (None, false));
        }
        // with an area the Removed chip scrolls to the top and keeps "errors only"
        assert_eq!(chip_goto(Some(CheckGoto::Top), true), (Some(CheckGoto::Top), false));
        // sections hidden by the filter turn it off to be seen
        assert_eq!(chip_goto(Some(CheckGoto::Matches), true), (Some(CheckGoto::Matches), true));
        assert_eq!(chip_goto(None, true), (None, false));
        // the counter label still reads in both languages
        for ru in [false, true] {
            assert!(!chip_label(ru, Chip::Removed, 1).is_empty());
        }
    }

    #[test]
    fn count_chips_read_with_plural_labels_in_both_languages() {
        use super::super::xlsx_counts::{chip_label, Chip};
        assert_eq!(format!("1 {}", chip_label(false, Chip::Removed, 1)), "1 row removed");
        assert_eq!(format!("2 {}", chip_label(false, Chip::Removed, 2)), "2 rows removed");
        let r = crate::shell::xlsx_counts::not_in_book(true, 22);
        assert!(r.starts_with("22 "), "{r}");
        let mut rep = Report::default();
        rep.removed = vec![xlsx::Removed { sheet: "Walls".into(), id: 1 }];
        assert_eq!(missing_note(&rep, false, Lang::En).unwrap(), "1 element not in the workbook");
    }

    #[test]
    fn decision_labels_exist_in_both_languages() {
        for key in ["xlsx_confirm.accept", "xlsx_confirm.reject", "xlsx_exists.apply_first"] {
            assert!(!l(Lang::En, key).is_empty() && !l(Lang::Ru, key).is_empty(), "{key}");
            assert_ne!(l(Lang::En, key), l(Lang::Ru, key), "{key}");
        }
    }

    fn footer_frame(
        ctx: &egui::Context,
        lang: Lang,
        busy: bool,
        events: Vec<egui::Event>,
    ) -> ([bool; 3], Vec<(String, egui::Pos2, egui::Vec2)>) {
        let mut got = [false; 3];
        let raw = egui::RawInput { events, ..Default::default() };
        let out = ctx.run(raw, |cx| {
            egui::CentralPanel::default().show(cx, |ui| got = linked_footer(ui, lang, busy));
        });
        let mut texts = vec![];
        out.shapes.iter().for_each(|cs| shape_texts(&cs.shape, &mut texts));
        (got, texts)
    }

    fn click_once(p: egui::Pos2) -> Vec<egui::Event> {
        let b = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![egui::Event::PointerMoved(p), b(true), b(false)]
    }

    #[test]
    fn linked_footer_idle_frames_click_nothing_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            for busy in [false, true] {
                let ctx = font_ctx();
                for _ in 0..3 {
                    let (got, _) = footer_frame(&ctx, lang, busy, vec![]);
                    assert_eq!(got, [false; 3], "{lang:?} busy={busy}");
                }
            }
        }
    }

    #[test]
    fn export_again_click_fires_only_when_idle_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            for busy in [false, true] {
                let ctx = font_ctx();
                let label = l(lang, "xlw.export_again");
                let mut at = None;
                for _ in 0..2 {
                    let (_, t) = footer_frame(&ctx, lang, busy, vec![]);
                    at = t.iter().find(|x| x.0 == label).map(|x| x.1 + x.2 / 2.0);
                }
                let p = at.unwrap_or_else(|| panic!("{lang:?}: no export button text"));
                let (got, _) = footer_frame(&ctx, lang, busy, click_once(p));
                assert_eq!(got[1], !busy, "{lang:?} busy={busy}");
                assert!(!got[0] && !got[2], "{lang:?} busy={busy}: only export again was clicked");
            }
        }
    }

    #[test]
    fn disabled_footer_reason_is_a_catalog_text_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let why = l(lang, "xlw.export_running");
            assert!(!why.is_empty() && !why.starts_with("xlw."), "{lang:?}: {why}");
            let tip = l(lang, "xlw.rewrite_the_workbook_from_the_mode");
            assert!(!tip.is_empty() && !tip.starts_with("xlw."), "{lang:?}: {tip}");
        }
        assert_ne!(l(Lang::En, "xlw.export_running"), l(Lang::Ru, "xlw.export_running"));
    }

    #[test]
    fn linked_footer_has_one_primary_and_only_kit_buttons() {
        let src = include_str!("xlsx_win.rs");
        let from = src.find("fn linked_footer(").unwrap();
        let body = &src[from..from + src[from..].find("fn filter_row(").unwrap()];
        assert_eq!(body.matches("primary_button(").count(), 1, "one primary in the footer");
        assert_eq!(body.matches("secondary_button(").count(), 1);
        assert_eq!(body.matches("cancel_button(").count(), 1);
        assert!(!body.contains("egui::Button"), "raw button in the footer");
    }

    #[test]
    fn errors_only_note_sits_on_its_own_line_below_the_checkbox_en_ru() {
        for lang in [Lang::En, Lang::Ru] {
            for no_base in [false, true] {
                let ctx = font_ctx();
                let mut rects = None;
                let mut flag = false;
                for _ in 0..2 {
                    let _ = ctx.run(Default::default(), |cx| {
                        egui::CentralPanel::default().show(cx, |ui| {
                            let th = ddc_ui::theme::of(ui.ctx());
                            let note = Some("1 element not in the workbook".to_string());
                            rects = Some(filter_row(ui, &th, lang, &mut flag, note, no_base));
                        });
                    });
                }
                let (boxed, note) = rects.unwrap();
                let note = note.expect("note rect");
                assert!(note.min.y >= boxed.max.y, "{lang:?}: note {note:?} not below checkbox {boxed:?}");
                assert!(!flag, "idle frames must not toggle the filter");
                let mut none = None;
                let _ = ctx.run(Default::default(), |cx| {
                    egui::CentralPanel::default().show(cx, |ui| {
                        let th = ddc_ui::theme::of(ui.ctx());
                        none = Some(filter_row(ui, &th, lang, &mut flag, None, no_base).1);
                    });
                });
                assert_eq!(none, Some(None));
            }
        }
    }

    #[test]
    fn xlsx_window_uses_kit_widgets_only() {
        let src = include_str!("xlsx_win.rs");
        // the product code ends at the test module; small cfg(test) helpers sit above it
        let body = &src[..src.find("\nmod tests {").unwrap()];
        assert!(!body.contains("egui::Button::new"), "raw egui button in the XLSX window");
        for raw in [
            "ComboBox",
            "DragValue",
            "ui.button(",
            "ui.checkbox(",
            "selectable_label",
            "selectable_value",
            "radio_value",
            ".small_button(",
        ] {
            assert!(!body.contains(raw), "raw widget left: {raw}");
        }
        // footer keep-missing is a kit checkbox now
        let quiet = body.matches("Button::quiet(").count();
        assert_eq!(quiet, 2, "error row helper, show-in-folder");
        assert!(body.contains("widgets::segmented(") && body.contains("widgets::checkbox("));
    }
}
