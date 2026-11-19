// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saving to IFC: `file.save_ifc`, `file.save_ifc_as`, `data.export_ifc`. A named save on its own
//! job slot, separate from Ctrl+S, which only ever writes `.ddc`. Default target is a copy,
//! `<stem>-edited.ifc`; the imported file is replaced only after an explicit per-project choice.
//! Contract: docs/design/OPEN_SAVE.md, section 6. Stages: `job`. Result: `report`. Status: `pill`.
//!
//! Seams with the IFC commit and the exporter:
//! - `source_of` reads `Model::sources()`: IFC appears there once the IFC commit writes `src_file`.
//! - `model/source/target.rs` creates `src_target` itself; the IFC commit must use the same columns.
//! - `after_over_source` sets `written_hash` when the column exists and moves `src_base`.
//! - GlobalIds: `export/guids.rs` gained two accessors; the exporter rewrites that file, keep both.
//! - Back ends: `backend_for` picks Patch for an IFC source, else Rewrite; `Backend` may be replaced.

pub mod guard;
pub mod job;
pub mod patch;
pub mod pill;
pub mod report;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use web_time::{SystemTime, UNIX_EPOCH};

use ddc_model::source::target::SaveTarget;
use ddc_model::{remove_db_files, Model, RENAME_WAIT};
use ddc_ui::notify::Level;
use egui::{self, RichText};

use crate::jobs::{Job, Poll, Progress};
use crate::project::{same_path, SaveMark};
use crate::shell::EditorApp;
use ddc_io::ifc::IfcSchema;
use job::{Backend, Failure, Outcome, Rewrite, SaveRequest, Stage};
use report::{SaveReport, Strip};

const FORMAT: &str = "ifc";
const EDITED: &str = crate::open::names::COPY_SUFFIX;

/// The project's imported file, as far as saving cares.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceRef {
    pub key: String,
    pub path: PathBuf,
    pub is_ifc: bool,
}

/// Where one save goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub target: PathBuf,
    pub over_source: bool,
    pub source: Option<SourceRef>,
    /// Remember it in `src_target`; a one-off export does not.
    pub record: bool,
}

pub(crate) struct Running {
    pub(crate) job: Job<Result<Outcome, Failure>>,
    pub(crate) target: PathBuf,
    plan: Resolved,
    mark: SaveMark,
    /// A database copy made for this save, removed with it.
    temp: Option<PathBuf>,
}

/// A refused save and what its target holds.
#[derive(Clone, Debug)]
pub(crate) struct Refusal {
    pub(crate) plan: Resolved,
    pub(crate) holds: guard::Holds,
    /// "Replace anyway" was pressed once; the naming question is showing.
    pub(crate) asked: bool,
}

#[derive(Default)]
pub struct State {
    pub(crate) running: Option<Running>,
    /// Pressed again while running: one follow-up, with the edits made since.
    queued: bool,
    /// The window waits for this save, then closes.
    closing: bool,
    pub(crate) strip: Option<Strip>,
    pub(crate) report: Option<SaveReport>,
    pub(crate) report_open: bool,
    /// Inline confirmation of "Use the original from now on": the source it would replace.
    confirm: Option<SourceRef>,
    /// The card came from a save onto the original: Yes also writes that save.
    confirm_writes: bool,
    /// An empty model was refused over a file with content: the offer to replace or save as.
    pub(crate) refusal: Option<Refusal>,
    /// Ctrl+S chose this `.ddc` by itself; its toast offers another place.
    first_ddc: Option<PathBuf>,
    /// `src_target` changed since the last `.ddc` save: written silently on close.
    target_unsaved: bool,
    pub(crate) saved_mark: Option<SaveMark>,
    /// Schema of the one export that is starting (`data.export_ifc4` and its siblings).
    pub(crate) export_schema: Option<ddc_io::ifc::IfcSchema>,
    pub(crate) pill: RefCell<Option<((u64, u64), Option<String>)>>,
}

pub fn register(a: &mut EditorApp) {
    a.register_command("file.save_ifc", save_ifc);
    a.register_command("file.save_ifc_as", save_ifc_as);
    a.register_command("data.export_ifc", export_ifc_copy);
    a.register_command("file.ifc_replace_original", toggle_replace_original);
    a.register_command("data.export_ifc4", |a| export_ifc_as(a, IfcSchema::Ifc4));
    a.register_command("data.export_ifc4x3", |a| export_ifc_as(a, IfcSchema::Ifc4x3));
    a.register_command("data.export_ifc2x3", |a| export_ifc_as(a, IfcSchema::Ifc2x3));
}

/// `data.export_ifc4` / `4x3` / `2x3`: a copy in the chosen schema, always a fresh export.
fn export_ifc_as(a: &mut EditorApp, schema: IfcSchema) {
    if a.save.running.is_some() {
        status(a, "job.busy");
        return;
    }
    let mut d = crate::file_pick::Dlg::new().add_filter("IFC", &[FORMAT]);
    if let Some(p) = resolve(a) {
        if let Some(n) = p.target.file_name().and_then(|n| n.to_str()) {
            d = d.set_file_name(n);
        }
        if let Some(dir) = p.target.parent() {
            d = d.set_directory(dir);
        }
    }
    let Some(path) = d.save_file() else { return };
    a.save.export_schema = Some(schema);
    let source = source_of(&a.ed.model);
    start(
        a,
        Resolved {
            target: with_ifc_ext(path),
            over_source: false,
            source,
            record: false,
        },
    );
    a.save.export_schema = None;
}

/// `<project or source dir>/<stem>-edited.ifc`: the stem of the source when there is one. `None`
/// with neither: the name dialog decides. Never the source itself, whatever its name.
pub fn default_target(project: Option<&Path>, source: Option<&Path>) -> Option<PathBuf> {
    let named = source.or(project)?;
    let stem = named.file_stem()?.to_string_lossy().into_owned();
    let dir = project.or(source)?.parent()?;
    Some(dir.join(format!("{stem}{EDITED}.{FORMAT}")))
}

/// `path` lies under `dir`; case does not matter on Windows.
fn under_dir(path: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        let low = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
        low(path).starts_with(low(dir))
    } else {
        path.starts_with(dir)
    }
}

/// The remembered path; for a target stored next to the project, the relative one from the
/// folder the project is in now (a moved or copied project never writes to its old folder).
fn remembered(row: &SaveTarget, project: Option<&Path>) -> PathBuf {
    let path = PathBuf::from(&row.path);
    if row.rel_path.is_empty() {
        return path;
    }
    match project.and_then(Path::parent) {
        Some(dir) if !under_dir(&path, dir) => dir.join(&row.rel_path),
        _ => path,
    }
}

/// Where a save goes, given what the project remembers. This is the rule that keeps the source
/// safe: only a stored over-source flag on an IFC source may name it.
pub fn resolve_target(
    row: Option<&SaveTarget>,
    project: Option<&Path>,
    source: Option<&SourceRef>,
) -> Option<Resolved> {
    let src_path = source.map(|s| s.path.as_path());
    let default = || default_target(project, src_path);
    let (target, over_source) = match row {
        // the setting applies only while the save target is the original; after a Save As
        // elsewhere the target is that file and Ctrl+S keeps writing there
        Some(r)
            if r.over_source
                && source.is_some_and(|s| s.is_ifc && same_path(&s.path, Path::new(&r.path))) =>
        {
            (src_path?.to_path_buf(), true)
        }
        Some(r) => {
            let p = remembered(r, project);
            if src_path.is_some_and(|s| same_path(s, &p)) {
                (default()?, false)
            } else {
                (p, false)
            }
        }
        None => (default()?, false),
    };
    Some(Resolved {
        target,
        over_source,
        source: source.cloned(),
        record: true,
    })
}

/// Seam: an IFC source first, else any with a path.
fn source_of(m: &Model) -> Option<SourceRef> {
    let list = m.sources().ok()?;
    let is_ifc = |f: &str| f.eq_ignore_ascii_case(FORMAT);
    let pick = list
        .iter()
        .filter(|s| !s.path.is_empty())
        .find(|s| is_ifc(&s.format))
        .or_else(|| list.iter().find(|s| !s.path.is_empty()))?;
    Some(SourceRef {
        key: pick.source.clone(),
        path: PathBuf::from(&pick.path),
        is_ifc: is_ifc(&pick.format),
    })
}

/// Patch for an IFC source, which itself falls back to a rewrite; rewrite for everything else.
fn backend_for(plan: &Resolved) -> Arc<dyn Backend> {
    match &plan.source {
        Some(s) if s.is_ifc => Arc::new(patch::Patch::new(s.clone(), plan.over_source)),
        _ => Arc::new(Rewrite),
    }
}

fn resolve(a: &EditorApp) -> Option<Resolved> {
    let row = a.ed.model.save_target(FORMAT).ok().flatten();
    let source = source_of(&a.ed.model);
    resolve_target(row.as_ref(), a.ed.project.path(), source.as_ref())
}

pub(crate) fn save_ifc(a: &mut EditorApp) {
    if a.save.running.is_some() {
        a.save.queued = true;
        return;
    }
    match resolve(a) {
        Some(p) => {
            start(a, p);
        }
        None => save_ifc_as(a),
    }
}

fn with_ifc_ext(p: PathBuf) -> PathBuf {
    let has = p
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(FORMAT));
    if has {
        p
    } else {
        let mut s = p.into_os_string();
        s.push(".");
        s.push(FORMAT);
        PathBuf::from(s)
    }
}

fn save_ifc_as(a: &mut EditorApp) {
    if let Some(path) = ask_path(a) {
        save_ifc_as_to(a, path);
    }
}

fn ask_path(a: &mut EditorApp) -> Option<PathBuf> {
    if a.save.running.is_some() {
        status(a, "job.busy");
        return None;
    }
    let plan = resolve(a);
    let mut d = crate::file_pick::Dlg::new().add_filter("IFC", &[FORMAT]);
    if let Some(t) = plan.as_ref().map(|p| &p.target) {
        if let Some(n) = t.file_name().and_then(|n| n.to_str()) {
            d = d.set_file_name(n);
        }
        if let Some(dir) = t.parent() {
            d = d.set_directory(dir);
        }
    }
    d.save_file().map(with_ifc_ext)
}

pub(crate) fn save_ifc_as_to(a: &mut EditorApp, path: PathBuf) {
    start(
        a,
        Resolved {
            target: path,
            over_source: false,
            source: source_of(&a.ed.model),
            record: true,
        },
    );
}

/// `data.export_ifc`: asks for a file and exports a copy; the Save target stays as it was.
fn export_ifc_copy(a: &mut EditorApp) {
    if a.save.running.is_some() {
        status(a, "job.busy");
        return;
    }
    let plan = resolve(a);
    let mut d = crate::file_pick::Dlg::new().add_filter("IFC", &[FORMAT]);
    if let Some(t) = plan.as_ref().map(|p| &p.target) {
        if let Some(n) = t.file_name().and_then(|n| n.to_str()) {
            d = d.set_file_name(n);
        }
        if let Some(dir) = t.parent() {
            d = d.set_directory(dir);
        }
    }
    if let Some(path) = d.save_file() {
        export_copy(a, path);
    }
}

/// One-off export to a file the user named, through the same safe pipeline; nothing is remembered.
pub fn export_copy(a: &mut EditorApp, path: PathBuf) {
    if a.save.running.is_some() {
        status(a, "job.busy");
        return;
    }
    let source = source_of(&a.ed.model);
    start(
        a,
        Resolved {
            target: with_ifc_ext(path),
            over_source: false,
            source,
            record: false,
        },
    );
}

fn status(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

/// Reader-facing text of a model error: the typed field refusal in words, any other error as a
/// neutral phrase. Raw database text is never shown.
pub(crate) fn model_detail(ru: bool, e: &ddc_model::Error) -> String {
    match e {
        ddc_model::Error::Field(f) => {
            let args = f.args();
            let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
            crate::tr_lang(ru, f.key(), &args)
        }
        _ => crate::tr_lang(ru, "save.detail.internal", &[]),
    }
}

/// A warning that leads with what is true of the file, then the localized reason.
pub(crate) fn warn_text(ru: bool, lead_key: &str, e: &ddc_model::Error) -> String {
    crate::tr_lang(ru, lead_key, &[("error", model_detail(ru, e).as_str())])
}

pub(crate) fn temp_copy() -> PathBuf {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("ddc-save-{}-{ns}.ddc", std::process::id()))
}

/// Starts the job. A target that is the source without the explicit flag goes to the confirmation
/// instead, so no path through here can replace the imported file by accident.
pub fn start(a: &mut EditorApp, plan: Resolved) -> bool {
    if a.save.running.is_some() {
        return false;
    }
    // a finished import waits in the big slot; writing rows now would cost it
    if a.big.busy() {
        status(a, "job.busy");
        return false;
    }
    // an old refusal card must not act on a plan that is no longer the current one
    a.save.refusal = None;
    if !plan.over_source {
        if let Some(s) = plan
            .source
            .as_ref()
            .filter(|s| same_path(&s.path, &plan.target))
        {
            let s = s.clone();
            if s.is_ifc {
                a.save.confirm = Some(s);
                a.save.confirm_writes = true;
            } else {
                status(a, "save.source_refused");
            }
            return false;
        }
    }
    let row =
        a.ed.model
            .save_target(FORMAT)
            .ok()
            .flatten()
            // the stored hash and journal describe the recorded file only, not another Save As target
            .filter(|r| plan.record && same_path(Path::new(&r.path), &plan.target));
    let mark = a.ed.save_mark();
    let (snapshot, temp) = match a.ed.project.work_path() {
        Some(w) => (w.to_path_buf(), None),
        None => {
            // in memory: no second connection can reach it, so a copy is taken on this thread
            let t = temp_copy();
            if let Err(e) = a.ed.model.save_project_to(&t, &mut |_, _| Ok(())) {
                remove_db_files(&t);
                let d = model_detail(crate::ui_lang_now(), &e);
                fail_strip(a, &d, None);
                return false;
            }
            (t.clone(), Some(t))
        }
    };
    let req = SaveRequest {
        snapshot,
        target: plan.target.clone(),
        backend: match a.save.export_schema.take() {
            Some(s) => Arc::new(job::RewriteAs(s)),
            None => backend_for(&plan),
        },
        over_source: plan.over_source,
        last_hash: row
            .as_ref()
            .map(|r| r.last_hash.clone())
            .unwrap_or_default(),
        last_journal: row.as_ref().map(|r| r.last_journal),
        replace_wait: RENAME_WAIT,
    };
    let job = Job::spawn("save.job.save", move |c: &Arc<Progress>| {
        Ok(job::run(&req, c, &mut |_| Ok(())))
    });
    a.save.running = Some(Running {
        job,
        target: plan.target.clone(),
        plan,
        mark,
        temp,
    });
    true
}

pub fn cancel(a: &mut EditorApp) {
    if let Some(r) = &a.save.running {
        a.save.queued = false;
        r.job.cancel();
    }
}

pub fn busy(a: &EditorApp) -> bool {
    a.save.running.is_some()
}

fn fail_strip(a: &mut EditorApp, detail: &str, kept: Option<&Path>) {
    fail_strip_for(a, detail, kept, true, false);
}

/// `had_file`: something stood at the target before, so "unchanged" is true.
/// `relocate`: offers another place, for failures the folder caused.
fn fail_strip_for(
    a: &mut EditorApp,
    detail: &str,
    kept: Option<&Path>,
    had_file: bool,
    relocate: bool,
) {
    let name = kept
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    let c = &a.sh.cat;
    let text = match (name, had_file) {
        (Some(n), true) => c.tf("save.failed_kept", &[("error", detail), ("name", n.as_str())]),
        (Some(n), false) => c.tf("save.failed_new_kept", &[("error", detail), ("name", n.as_str())]),
        (None, true) => c.tf("save.failed", &[("error", detail)]),
        (None, false) => c.tf("save.failed_new", &[("error", detail)]),
    };
    let mut strip = Strip::new(text, true, kept.map(Path::to_path_buf));
    strip.can_relocate = relocate;
    report::show(a, strip);
}

/// Stores the result in the project. Skipped when another model took this one's place: the row
/// belongs to the project the file was written from.
fn record(a: &mut EditorApp, r: &Running, o: &Outcome) {
    // a copy made because the source changed is not this project's save target
    if !r.plan.record || o.source_kept || !a.ed.is_same_model(r.mark) {
        return;
    }
    let project_dir =
        a.ed.project
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
    let rel_path = project_dir
        .and_then(|d| {
            r.target
                .strip_prefix(d)
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_default();
    let old =
        a.ed.model
            .save_target(FORMAT)
            .ok()
            .flatten()
            .unwrap_or_default();
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let row = SaveTarget {
        format: FORMAT.into(),
        source: r.plan.source.as_ref().map(|s| s.key.clone()),
        path: r.target.display().to_string(),
        rel_path,
        // a save elsewhere does not switch the per-project choice off
        over_source: r.plan.over_source || old.over_source,
        settings: old.settings,
        last_hash: o.hash.clone(),
        last_at: at,
        last_journal: o.journal,
        last_report: o.report.to_json(),
    };
    if let Err(e) = a.ed.model.put_save_target(&row) {
        // the file is written and whole; only the memory of it is missing
        let text = warn_text(crate::ui_lang_now(), "save.warn.target_not_remembered", &e);
        a.sh.toasts.push(Level::Warning, text, None);
        return;
    }
    a.save.saved_mark = Some(r.mark);
    a.save.target_unsaved = true;
    if r.plan.over_source {
        if let Some(s) = &r.plan.source {
            if let Err(e) = after_over_source(&a.ed.model, &s.key, o) {
                // the file is whole; the edits only stay pending and the next save writes them
                let text = warn_text(crate::ui_lang_now(), "save.warn.edits_pending", &e);
                a.sh.toasts.push(Level::Warning, text, None);
            }
        }
    }
}

/// After a replace of the source, so the next save and update see only what changed since.
fn after_over_source(m: &Model, source: &str, o: &Outcome) -> Result<(), ddc_model::Error> {
    m.note_source_written(source, &o.hash)?;
    match &o.rebase {
        Some(r) => m.apply_source_rebase(source, r),
        None => Ok(()),
    }
}

/// The file a finished save wrote: the copy, when the source was kept.
pub(crate) fn written_file(target: &Path, o: &Outcome) -> PathBuf {
    if o.source_kept {
        PathBuf::from(&o.report.target)
    } else {
        target.to_path_buf()
    }
}

fn finished(a: &mut EditorApp, r: Running, o: Outcome) {
    record(a, &r, &o);
    let mut text = o.report.line(&a.sh.cat);
    if o.report.target_changed {
        text = a.sh.cat.tf("save.target_changed", &[("line", text.as_str())]);
    }
    if o.report.replaced_unknown && !o.report.target_changed {
        let kept = o.report.prev.clone().unwrap_or_default();
        text = a
            .sh
            .cat
            .tf("save.replaced_unknown", &[("line", text.as_str()), ("backup", kept.as_str())]);
    }
    let warn = o.report.not_written_total > 0
        || o.report.target_changed
        || o.report.replaced_unknown;
    let source_is_ifc = r.plan.source.as_ref().is_some_and(|s| s.is_ifc);
    let mut strip = Strip::new(text, warn, Some(written_file(&r.target, &o)));
    strip.has_report = true;
    strip.can_use_original = r.plan.record && source_is_ifc && !r.plan.over_source;
    report::show(a, strip);
    a.save.report = Some(o.report);
}

fn failed(a: &mut EditorApp, f: Failure, plan: &Resolved) {
    if f.cancelled {
        status(a, "save.cancelled");
        return;
    }
    if f.stage == Stage::Plan && f.message == guard::EMPTY_OVER_FULL {
        a.save.refusal = Some(Refusal {
            plan: plan.clone(),
            holds: guard::holds(&plan.target),
            asked: false,
        });
    }
    let detail = match f.stage {
        Stage::Verify => {
            a.sh.cat
                .tf("save.failed_check", &[("error", f.message.as_str())])
        }
        Stage::Plan if f.message == guard::EMPTY_OVER_FULL => {
            a.sh.cat.tf(guard::EMPTY_OVER_FULL, &[])
        }
        _ => f.message.clone(),
    };
    // a failed write never touches the target, so a missing file means there was none before;
    // the refusal card already offers its own way out
    let had_file = plan.target.exists();
    let relocate = a.save.refusal.is_none();
    fail_strip_for(a, &detail, f.kept.as_deref(), had_file, relocate);
}

/// "Replace anyway", first press: the question that names what the target holds.
pub(crate) fn ask_replace(a: &mut EditorApp) {
    if let Some(r) = a.save.refusal.as_mut().filter(|r| r.holds.can_replace()) {
        r.asked = true;
    }
}

/// Confirmed replace: the target moves to `_archive` beside it, then the save runs again. A
/// target that is not ours, or a move that fails, writes nothing. `true` when the save started.
pub(crate) fn replace_anyway(a: &mut EditorApp) -> bool {
    let Some(r) = a.save.refusal.clone() else {
        return false;
    };
    if !r.asked || !r.holds.can_replace() || a.save.running.is_some() {
        return false;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match guard::archive_target(&r.plan.target, &guard::stamp_of(secs)) {
        Ok(moved) => {
            a.save.refusal = None;
            let p = moved.display().to_string();
            a.sh.toasts
                .push(Level::Info, a.sh.cat.tf("save_guard.replace.moved", &[("path", p.as_str())]), None);
            start(a, r.plan)
        }
        Err(e) => {
            let name = r.plan.target.display().to_string();
            let msg = a
                .sh
                .cat
                .tf("save_guard.replace.move_failed", &[("name", name.as_str()), ("error", &e.to_string())]);
            a.sh.toasts.push(Level::Warning, msg, None);
            false
        }
    }
}

/// Cancel (or Esc): the target stays as it is.
pub(crate) fn replace_cancel(a: &mut EditorApp) {
    a.save.refusal = None;
}

fn refusal_card(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(r) = a.save.refusal.clone() else {
        return;
    };
    let (mut replace, mut yes, mut save_as) = (false, false, false);
    let mut no = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let th = &a.sh.theme;
    let c = &a.sh.cat;
    let name = r
        .plan
        .target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    egui::Area::new(egui::Id::new("ri2.refusal"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 132.0])
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(th.c("bg.panel"))
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(c.tf("save_guard.refuse.body", &[("name", name.as_str())]));
                    if !r.holds.can_replace() {
                        ui.label(RichText::new(c.t("save_guard.refuse.foreign")).color(th.c("warning")));
                    } else if r.asked {
                        let what = match r.holds {
                            guard::Holds::Ours(n) => {
                                c.tf("save_guard.refuse.holds_db", &[("n", n.to_string().as_str())])
                            }
                            _ => c.t("save_guard.refuse.holds_ifc"),
                        };
                        ui.label(c.tf(
                            "save_guard.refuse.ask",
                            &[("name", name.as_str()), ("what", what.as_str())],
                        ));
                    }
                    ui.horizontal(|ui| {
                        no |= crate::ui_kit::cancel_button(ui, c.t("save_guard.refuse.cancel")).clicked();
                        // the safe way out is the accent button, click only; Enter stays unbound
                        save_as = crate::ui_kit::primary_click(ui, c.t("save_guard.refuse.save_as"), None);
                        if r.holds.can_replace() {
                            // destructive: danger style, never the default
                            let key = if r.asked {
                                "save_guard.refuse.replace_yes"
                            } else {
                                "save_guard.refuse.replace"
                            };
                            // the second, asked stage is the confirmation: filled
                            let b = if r.asked {
                                crate::ui_kit::confirm_danger_button(ui, c.t(key), true)
                            } else {
                                crate::ui_kit::danger_button(ui, c.t(key), true)
                            };
                            if b.clicked() {
                                if r.asked {
                                    yes = true;
                                } else {
                                    replace = true;
                                }
                            }
                        }
                    });
                });
        });
    if no {
        replace_cancel(a);
    } else if save_as {
        replace_cancel(a);
        save_ifc_as(a);
    } else if replace {
        ask_replace(a);
    } else if yes {
        replace_anyway(a);
    }
}

fn poll(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(mut r) = a.save.running.take() else {
        return;
    };
    let polled = r.job.poll_finished();
    if matches!(polled, Poll::Running) {
        a.save.running = Some(r);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    if let Some(t) = r.temp.take() {
        remove_db_files(&t);
    }
    let ok = matches!(polled, Poll::Done(Ok(_)));
    match polled {
        Poll::Done(Ok(o)) => finished(a, r, o),
        Poll::Done(Err(f)) => {
            let k = if f.cancelled {
                crate::jobs::Ended::Cancelled
            } else {
                crate::jobs::Ended::Failed
            };
            crate::jobs::record_end("save.job.save", k, Some(f.message.clone()));
            failed(a, f, &r.plan)
        }
        Poll::Failed(e) => fail_strip(a, &e, None),
        Poll::Cancelled => status(a, "save.cancelled"),
        Poll::Running => {}
    }
    if a.save.closing && !ok {
        // the failure strip stays visible; the held close is dropped
        a.save.closing = false;
        status(a, "save.close_held");
    }
    if !a.save.closing && std::mem::take(&mut a.save.queued) {
        save_ifc(a);
    }
}

/// Anything that writes a project file and must finish before the window goes.
fn writing(a: &EditorApp) -> bool {
    a.save.running.is_some()
}

/// The held close goes through once the last writer is done.
fn resume_close(a: &mut EditorApp, ctx: &egui::Context) {
    if a.save.closing && !writing(a) {
        a.save.closing = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The window was asked to close. A running save holds it, with a small note; `true` means the
/// close is cancelled for now. With nothing running, a clean project writes its newer
/// `src_target` to its file: nothing to ask about, and the next open then knows the target.
pub fn hold_close(a: &mut EditorApp, ctx: &egui::Context) -> bool {
    if a.save.running.is_some() {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        a.save.closing = true;
        a.save.queued = false;
        return true;
    }
    if a.save.target_unsaved
        && !a.ed.is_dirty()
        && !a.ed.project.read_only()
        && a.ed.project.path().is_some()
        && a.ed.save_project().is_ok()
    {
        a.save.target_unsaved = false;
    }
    false
}

fn closing_note(a: &mut EditorApp, ctx: &egui::Context) {
    if !a.save.closing {
        return;
    }
    let mut stop = false;
    let c = &a.sh.cat;
    // no open flag: Esc and Enter leave the running save alone, only the button stops it
    crate::ui_kit::Dialog::new("save.closing", c.t("save.closing.title"), crate::ui_kit::DialogSize::Confirm)
        .id(egui::Id::new("save.closing"))
        .show(ctx, |ui| {
            ui.label(c.t("save.closing.body"));
            stop = crate::ui_kit::secondary_button(ui, c.t("save.closing.cancel")).clicked();
        });
    if stop {
        cancel_closing(a);
    }
}

/// Layout probe seam: the closing note; `arm` starts it. True while it is still shown.
#[cfg(test)]
pub(crate) fn layout_closing(a: &mut EditorApp, ctx: &egui::Context, arm: bool) -> bool {
    a.save.closing |= arm;
    closing_note(a, ctx);
    a.save.closing
}

/// "Cancel the save" in the closing note: the job stops and the window stays open.
fn cancel_closing(a: &mut EditorApp) {
    a.save.closing = false;
    cancel(a);
    status(a, "save.cancelled");
}

/// "Use the original from now on" pressed on the strip.
pub(crate) fn ask_use_original(a: &mut EditorApp) {
    if let Some(s) = source_of(&a.ed.model).filter(|s| s.is_ifc) {
        a.save.confirm = Some(s);
    }
}

/// Turns the over-source flag on. Only the confirmation below calls this.
fn use_original(m: &Model, s: &SourceRef) -> Result<(), ddc_model::Error> {
    let mut row = m.save_target(FORMAT)?.unwrap_or_default();
    row.format = FORMAT.into();
    row.source = Some(s.key.clone());
    row.path = s.path.display().to_string();
    row.over_source = true;
    m.put_save_target(&row)
}

/// The project setting: replace the imported file (with backups). On goes through the
/// confirmation, off at once. Save As and Export never change it.
fn toggle_replace_original(a: &mut EditorApp) {
    let row = a.ed.model.save_target(FORMAT).ok().flatten();
    if row.as_ref().is_some_and(|r| r.over_source) {
        match clear_original(&a.ed.model) {
            Ok(()) => {
                a.save.target_unsaved = true;
                *a.save.pill.borrow_mut() = None;
                status(a, "save.original_off");
            }
            Err(e) => a.sh.toasts.push(Level::Warning, e.to_string(), None),
        }
    } else {
        ask_use_original(a);
    }
}

fn clear_original(m: &Model) -> Result<(), ddc_model::Error> {
    let Some(mut row) = m.save_target(FORMAT)? else {
        return Ok(());
    };
    row.over_source = false;
    m.put_save_target(&row)
}

/// One inline question, stating the backups and the writer. A rewrite loses what the exporter
/// cannot carry, so the losses are listed and "No" is the default.
fn confirm_card(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(s) = a.save.confirm.clone() else {
        return;
    };
    let (mut yes, mut no) = (false, ctx.input(|i| i.key_pressed(egui::Key::Escape)));
    let th = &a.sh.theme;
    let c = &a.sh.cat;
    let name = s
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (not, proxies) = a
        .save
        .report
        .as_ref()
        .map_or((0, 0), |r| (r.not_written_total, r.proxies));
    egui::Area::new(egui::Id::new("save.confirm"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 132.0])
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(th.c("bg.panel"))
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(c.tf(
                        "save.confirm.body",
                        &[("name", name.as_str()), ("dir", job::BACKUP_DIR)],
                    ));
                    ui.label(RichText::new(c.t("save.confirm.rewrite")).color(th.c("warning")));
                    if not > 0 || proxies > 0 {
                        let (n, p) = (not.to_string(), proxies.to_string());
                        ui.label(c.tf(
                            "save.confirm.losses",
                            &[("not", n.as_str()), ("proxies", p.as_str())],
                        ));
                    }
                    ui.horizontal(|ui| {
                        // "No" is the safe answer; replacing is danger style and click only
                        no |= crate::ui_kit::primary_click(ui, c.t("save.confirm.no"), None);
                        let y = c.t("save.confirm.yes");
                        yes = crate::ui_kit::confirm_danger_button(ui, y, true).clicked();
                    });
                });
        });
    if no {
        a.save.confirm = None;
        a.save.confirm_writes = false;
    } else if yes {
        a.save.confirm = None;
        confirm_yes(a, s);
    }
}

fn confirm_yes(a: &mut EditorApp, s: SourceRef) {
    let writes = std::mem::take(&mut a.save.confirm_writes);
    match use_original(&a.ed.model, &s) {
        Ok(()) => {
            a.save.target_unsaved = true;
            *a.save.pill.borrow_mut() = None;
            status(a, "save.original_on");
            if writes {
                confirm_yes_save(a, s);
            }
        }
        Err(e) => {
            let text = warn_text(crate::ui_lang_now(), "save.warn.use_original", &e);
            a.sh.toasts.push(Level::Warning, text, None);
        }
    }
}

/// Yes on the card opened by a save onto the original: that save runs now, over the source.
fn confirm_yes_save(a: &mut EditorApp, s: SourceRef) {
    let target = s.path.clone();
    start(
        a,
        Resolved {
            target,
            over_source: true,
            source: Some(s),
            record: true,
        },
    );
}

/// `<source dir>/<stem>.ddc` for the first Ctrl+S of an imported project; `<stem> (2).ddc` when
/// the name is taken. `None` with no source or an unwritable folder: the normal dialog asks.
pub fn first_ddc_target(a: &mut EditorApp) -> Option<PathBuf> {
    let s = source_of(&a.ed.model)?;
    // the status line says why Ctrl+S falls back to the dialog
    if a.ed.project.read_only() {
        status(a, "save.ctrl_s_read_only");
        return None;
    }
    let p = ddc_beside(&s.path)?;
    if !folder_writable(p.parent()?) {
        status(a, "save.ctrl_s_folder");
        return None;
    }
    a.save.first_ddc = Some(p.clone());
    Some(p)
}

/// Always `.ddc`: Ctrl+S cannot name a source format through here.
pub fn ddc_beside(source: &Path) -> Option<PathBuf> {
    let stem = source.file_stem()?.to_string_lossy().into_owned();
    let dir = source.parent()?;
    let ext = ddc_model::PROJECT_EXT;
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    Some(p)
}

fn folder_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ddc-write-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The save of a `.ddc` that Ctrl+S named by itself: its toast says where, and offers another
/// place. `false` for any other save. A `.ddc` save also stores `src_target`.
pub fn announce_ddc_saved(a: &mut EditorApp, target: &Path, name: &str) -> bool {
    a.save.target_unsaved = false;
    let Some(p) = a.save.first_ddc.take() else {
        return false;
    };
    if !same_path(&p, target) {
        return false;
    }
    let msg = a.sh.cat.tf("save.saved_as", &[("name", name)]);
    let action = a.sh.cat.t("save.btn.change_location");
    a.sh.toasts
        .push_cmd(Level::Success, msg, action, "file.save_as");
    true
}

pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    poll(a, ctx);
    resume_close(a, ctx);
    report::strip_ui(a, ctx);
    report::window(a, ctx);
    confirm_card(a, ctx);
    refusal_card(a, ctx);
    closing_note(a, ctx);
}

#[cfg(test)]
mod tests {
    fn raw_err() -> ddc_model::Error {
        ddc_model::Error::Conflict("no such table: save_target (SQLITE_ERROR)".into())
    }

    #[test]
    fn save_target_put_failure_toast_localized_ru() {
        let ru = warn_text(true, "save.warn.target_not_remembered", &raw_err());
        let en = warn_text(false, "save.warn.target_not_remembered", &raw_err());
        assert_ne!(ru, en);
        assert!(!ru.contains("save_target") && !ru.contains("SQLITE"));
        assert!(!ru.contains("{error}"));
    }

    #[test]
    fn after_over_source_failure_toast_says_file_saved() {
        let en = warn_text(false, "save.warn.edits_pending", &raw_err());
        assert!(en.starts_with("The file was saved completely"), "{en}");
        let ru = warn_text(true, "save.warn.edits_pending", &raw_err());
        assert_ne!(ru, en);
        assert!(!en.contains("SQLITE"));
    }

    #[test]
    fn use_original_failure_toast_has_no_raw_sql_text() {
        for ru in [false, true] {
            let t = warn_text(ru, "save.warn.use_original", &raw_err());
            assert!(!t.contains("no such table") && !t.contains("SQLITE"), "{t}");
            assert!(!t.contains("{error}"));
        }
    }

    use super::*;
    use crate::editor::{Editor, OpFn};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-o5-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn app(tag: &str) -> (EditorApp, PathBuf) {
        let d = dir(tag);
        let mut a = EditorApp::new(Editor::start_project().unwrap(), None, "test".into());
        a.prefs.autosave_dir = d.join("auto").display().to_string();
        (a, d)
    }

    fn edit(a: &mut EditorApp, name: &str, z: f64) {
        let name = name.to_string();
        let op = OpFn(Arc::new(move |m: &mut Model| {
            m.add_level(&name, z).map(|_| ())
        }));
        a.ed.run_op(op).unwrap();
    }

    /// One element, so the empty-over-full guard (d0b31e36) does not apply: a level alone is no
    /// content.
    fn add_content(a: &mut EditorApp) {
        let op = OpFn(Arc::new(|m: &mut Model| m.create("proxy").map(|_| ())));
        a.ed.run_op(op).unwrap();
    }

    fn wait(a: &mut EditorApp) {
        let ctx = egui::Context::default();
        for _ in 0..12000 {
            if !busy(a) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            poll(a, &ctx);
        }
        panic!("save did not finish");
    }

    fn src(path: &Path, is_ifc: bool) -> SourceRef {
        SourceRef {
            key: "ifc:x".into(),
            path: path.to_path_buf(),
            is_ifc,
        }
    }

    fn to(target: &Path) -> Resolved {
        Resolved {
            target: target.to_path_buf(),
            over_source: false,
            source: None,
            record: true,
        }
    }

    #[test]
    fn moved_project_folder_keeps_target() {
        let old = dir("moved-old");
        let new = dir("moved-new");
        let row = SaveTarget {
            path: old.join("House-edited.ifc").display().to_string(),
            rel_path: "House-edited.ifc".into(),
            ..Default::default()
        };
        // the old folder still exists (a copied project): the save follows the project
        let project = new.join("House.ddc");
        let r = resolve_target(Some(&row), Some(&project), None).unwrap();
        assert_eq!(r.target, new.join("House-edited.ifc"));
        // a target stored outside the project folder has no relative path and stays as named
        let far = SaveTarget {
            rel_path: String::new(),
            ..row
        };
        assert_eq!(remembered(&far, Some(&project)), old.join("House-edited.ifc"));
    }

    #[cfg(windows)]
    #[test]
    fn dir_match_ignores_case_on_windows() {
        let d = dir("case");
        let shown = d.display().to_string();
        let row = SaveTarget {
            path: format!("{}/House-edited.ifc", shown.to_uppercase()),
            rel_path: "House-edited.ifc".into(),
            ..Default::default()
        };
        let project = PathBuf::from(shown.to_lowercase()).join("House.ddc");
        // the same folder spelled in another case: the stored path stays, no re-rooting
        assert_eq!(remembered(&row, Some(&project)), PathBuf::from(&row.path));
        // a really different folder still re-roots
        let elsewhere = d.join("moved").join("House.ddc");
        assert_eq!(
            remembered(&row, Some(&elsewhere)),
            d.join("moved").join("House-edited.ifc")
        );
    }

    #[test]
    fn cancel_in_closing_note_keeps_window() {
        let (mut a, d) = app("closing-cancel");
        let job = Job::spawn("save.job.save", |c: &Arc<Progress>| {
            while !c.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err("stopped".to_string())
        });
        let mark = a.ed.save_mark();
        a.save.running = Some(Running {
            job,
            target: d.join("x.ddc"),
            plan: to(&d.join("x.ddc")),
            mark,
            temp: None,
        });
        let ctx = egui::Context::default();
        assert!(hold_close(&mut a, &ctx));
        assert!(a.save.closing);
        cancel_closing(&mut a);
        assert!(!a.save.closing, "the cancelled save must not close the window");
        assert_eq!(a.ed.status, Some(a.sh.cat.t("save.cancelled")));
        wait(&mut a);
        assert!(!a.save.closing);
    }

    #[test]
    fn close_after_failed_ifc_save_keeps_window() {
        let (mut a, d) = app("closing-failed");
        let job = Job::spawn("save.job.save", |_c: &Arc<Progress>| Err("boom".to_string()));
        let mark = a.ed.save_mark();
        a.save.running = Some(Running {
            job,
            target: d.join("x.ifc"),
            plan: to(&d.join("x.ifc")),
            mark,
            temp: None,
        });
        a.save.closing = true;
        wait(&mut a);
        assert!(!a.save.closing, "the held close is dropped after a failed save");
        assert_eq!(a.ed.status, Some(a.sh.cat.t("save.close_held")));
    }

    #[test]
    fn the_default_is_a_copy_named_after_the_source() {
        let s = Path::new("d/House.ifc");
        let p = Path::new("e/Proj.ddc");
        assert_eq!(
            default_target(None, Some(s)),
            Some(PathBuf::from("d/House-edited.ifc"))
        );
        assert_eq!(
            default_target(Some(p), Some(s)),
            Some(PathBuf::from("e/House-edited.ifc"))
        );
        assert_eq!(
            default_target(Some(p), None),
            Some(PathBuf::from("e/Proj-edited.ifc"))
        );
        assert_eq!(default_target(None, None), None);
        let e = Path::new("d/House-edited.ifc");
        assert_ne!(default_target(None, Some(e)).as_deref(), Some(e));
    }

    #[test]
    fn only_the_stored_flag_on_an_ifc_source_names_the_original() {
        let path = PathBuf::from("d/House.ifc");
        let s = src(&path, true);
        let none = resolve_target(None, None, Some(&s)).unwrap();
        assert_eq!(
            (none.target, none.over_source),
            (PathBuf::from("d/House-edited.ifc"), false)
        );
        // a remembered path that is the source, without the flag, falls back to the copy
        let row = SaveTarget {
            path: "d/House.ifc".into(),
            ..Default::default()
        };
        let r = resolve_target(Some(&row), None, Some(&s)).unwrap();
        assert_eq!(
            (r.target, r.over_source),
            (PathBuf::from("d/House-edited.ifc"), false)
        );
        let on = SaveTarget {
            over_source: true,
            ..row.clone()
        };
        let r = resolve_target(Some(&on), None, Some(&s)).unwrap();
        assert_eq!((r.target, r.over_source), (path.clone(), true));
        // a .rvt source is never replaced, whatever the row says
        let rvt = src(Path::new("d/House.rvt"), false);
        let on_rvt = SaveTarget {
            path: "d/House.rvt".into(),
            over_source: true,
            ..Default::default()
        };
        let r = resolve_target(Some(&on_rvt), None, Some(&rvt)).unwrap();
        assert_eq!(
            (r.target, r.over_source),
            (PathBuf::from("d/House-edited.ifc"), false)
        );
    }

    #[test]
    fn a_save_never_replaces_the_source_without_the_flag() {
        let (mut a, d) = app("guard");
        let f = d.join("House.ifc");
        std::fs::write(&f, b"source").unwrap();
        let plan = Resolved {
            source: Some(src(&f, true)),
            ..to(&f)
        };
        assert!(!start(&mut a, plan), "goes to the confirmation");
        assert!(!busy(&a));
        assert!(a.save.confirm.is_some());
        assert_eq!(std::fs::read(&f).unwrap(), b"source");
        let plan = Resolved {
            source: Some(src(&f, false)),
            ..to(&f)
        };
        a.save.confirm = None;
        assert!(!start(&mut a, plan));
        assert!(a.save.confirm.is_none(), "a .rvt offers nothing");
    }

    #[test]
    fn ctrl_s_from_an_import_names_only_a_ddc() {
        let d = dir("ddc");
        let f = d.join("House.ifc");
        assert_eq!(ddc_beside(&f), Some(d.join("House.ddc")));
        std::fs::write(d.join("House.ddc"), b"another project").unwrap();
        assert_eq!(ddc_beside(&f), Some(d.join("House (2).ddc")));
        for s in ["a.ifc", "a.rvt", "a.dwg"] {
            let p = ddc_beside(&d.join(s)).unwrap();
            assert_eq!(p.extension().unwrap(), ddc_model::PROJECT_EXT);
        }
    }

    #[test]
    fn a_save_writes_the_file_remembers_the_target_and_does_not_dirty() {
        let (mut a, d) = app("dirty");
        edit(&mut a, "L1", 0.0);
        let p = d.join("p.ddc");
        a.ed.save_project_as(&p).unwrap();
        assert!(!a.ed.is_dirty());
        let t = d.join("p-edited.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        ddc_io::ifc::check_step_file(&t, None).unwrap();
        assert!(!a.ed.is_dirty(), "src_target is not an edit");
        let row = a.ed.model.save_target(FORMAT).unwrap().unwrap();
        assert_eq!(PathBuf::from(&row.path), t);
        assert!(!row.over_source);
        assert_eq!(row.rel_path, "p-edited.ifc");
        assert_eq!(row.last_hash, ddc_io::ifc::file_sha256(&t).unwrap());
        assert_eq!(
            SaveReport::from_json(&row.last_report).unwrap().target,
            t.display().to_string()
        );
    }

    #[test]
    fn save_as_and_export_keep_the_underlay_tessellation() {
        let (mut a, d) = app("tess");
        edit(&mut a, "L1", 0.0);
        let lv = a.ed.level;
        let doc = ddc_io::drawing_model::fixture::sample_plan(0);
        let origin = crate::drawing::Origin::AsIs;
        a.plan
            .drawings
            .add(&doc, "p", None, lv, Some(0.001), origin, ddc_geom::v2(0.0, 0.0));
        let st = crate::drawing::tess::Style::plain(egui::Color32::WHITE, egui::Color32::BLACK);
        assert!(!a.plan.drawings.tess_warm(lv, 40.0, &st));
        let (v, n) = (a.plan.drawings.version, a.plan.drawings.tess_builds());
        a.ed.save_project_as(&d.join("p.ddc")).unwrap();
        assert!(start(&mut a, to(&d.join("p.ifc"))));
        wait(&mut a);
        export_copy(&mut a, d.join("copy.ifc"));
        wait(&mut a);
        assert_eq!(a.plan.drawings.version, v, "saving is not a drawing edit");
        assert!(a.plan.drawings.tess_warm(lv, 40.0, &st), "no rebuild after save");
        assert_eq!(a.plan.drawings.tess_builds(), n);
    }

    #[test]
    fn a_repeat_save_needs_no_dialog_and_goes_to_the_same_place() {
        let (mut a, d) = app("repeat");
        edit(&mut a, "L1", 0.0);
        let t = d.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        let again = resolve(&a).expect("resolved without asking");
        assert_eq!(again.target, t);
        assert!(start(&mut a, again));
        wait(&mut a);
        assert!(d.join(job::BACKUP_DIR).join("x.ifc.prev").exists());
    }

    #[test]
    fn pressed_again_while_running_queues_one_follow_up() {
        let (mut a, d) = app("queue");
        edit(&mut a, "L1", 0.0);
        let t = d.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        save_ifc(&mut a);
        save_ifc(&mut a);
        assert!(a.save.queued);
        wait(&mut a);
        // the follow-up ran after the first or is running now; only one was queued
        wait(&mut a);
        assert!(!a.save.queued);
        ddc_io::ifc::check_step_file(&t, None).unwrap();
    }

    #[test]
    fn ctrl_s_works_during_an_ifc_save_and_the_slots_are_separate() {
        let (mut a, d) = app("slots");
        edit(&mut a, "L1", 0.0);
        assert!(start(&mut a, to(&d.join("x.ifc"))));
        assert!(!a.project_ui.busy(), "the .ddc slot stays free");
        a.ed.save_project_as(&d.join("x.ddc")).unwrap();
        wait(&mut a);
        assert!(d.join("x.ddc").exists());
    }

    #[test]
    fn edits_during_a_save_show_on_the_pill() {
        let (mut a, d) = app("pill");
        edit(&mut a, "L1", 0.0);
        assert_eq!(pill::edits(&a), None, "no target yet");
        let t = d.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        assert_eq!(pill::edits(&a), Some(0));
        edit(&mut a, "L2", 3.0);
        assert_eq!(pill::edits(&a), Some(1));
    }

    #[test]
    fn a_result_for_a_replaced_model_is_not_recorded_there() {
        let (mut a, d) = app("swap");
        edit(&mut a, "L1", 0.0);
        let t = d.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        a.ed.new_project().unwrap();
        wait(&mut a);
        assert_eq!(a.ed.model.save_target(FORMAT).unwrap(), None);
        assert!(t.exists(), "the file itself is fine");
    }

    #[test]
    fn a_failed_save_keeps_the_target_and_says_so_on_the_strip() {
        let (mut a, d) = app("fail");
        edit(&mut a, "L1", 0.0);
        let t = d.join("x.ifc");
        std::fs::create_dir(&t).unwrap();
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        assert!(t.is_dir());
        let st = a.save.strip.as_ref().unwrap();
        assert!(st.warn);
        assert_eq!(
            a.ed.model.save_target(FORMAT).unwrap(),
            None,
            "not remembered"
        );
    }

    #[test]
    fn save_ifc_as_elsewhere_keeps_over_source_flag() {
        let (mut a, d) = app("keepflag");
        edit(&mut a, "L1", 0.0);
        let f = d.join("House.ifc");
        let s = src(&f, true);
        use_original(&a.ed.model, &s).unwrap();
        save_ifc_as_to(&mut a, d.join("elsewhere.ifc"));
        wait(&mut a);
        let row = a.ed.model.save_target(FORMAT).unwrap().unwrap();
        assert!(row.over_source);
        // Ctrl+S after Save As goes to the chosen file, never to the original
        let r = resolve_target(Some(&row), None, Some(&s)).unwrap();
        assert_eq!((r.target, r.over_source), (d.join("elsewhere.ifc"), false));
        clear_original(&a.ed.model).unwrap();
        assert!(!a.ed.model.save_target(FORMAT).unwrap().unwrap().over_source);
    }

    #[test]
    fn confirm_yes_from_save_as_writes_the_file() {
        let (mut a, d) = app("yes");
        edit(&mut a, "L1", 0.0);
        let p = d.join("House.ifc");
        std::fs::write(&p, b"x").unwrap();
        let s = src(&p, true);
        let plan = Resolved {
            target: p.clone(),
            over_source: false,
            source: Some(s.clone()),
            record: true,
        };
        assert!(!start(&mut a, plan));
        assert!(a.save.confirm_writes && a.save.confirm.is_some());
        a.save.confirm = None;
        confirm_yes(&mut a, s.clone());
        assert!(busy(&a), "Yes starts the save");
        assert!(a.ed.model.save_target(FORMAT).unwrap().unwrap().over_source);
        wait(&mut a);
        // the setting card (not a save) only sets the flag
        let (mut b, _) = app("yes_flag");
        b.save.confirm_writes = false;
        confirm_yes(&mut b, s);
        assert!(!busy(&b));
    }

    #[test]
    fn failed_first_write_no_previous_claim() {
        let (mut a, d) = app("first_fail");
        edit(&mut a, "L1", 0.0);
        // a missing folder is made by the save: only a file in its place makes the write fail
        let blocked = d.join("blocked");
        std::fs::write(&blocked, b"").unwrap();
        let t = blocked.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        let st = a.save.strip.as_ref().unwrap();
        assert!(st.warn);
        let lead = a.sh.cat.tf("save.failed_new", &[("error", "")]);
        assert!(st.text.starts_with(lead.trim_end()), "{}", st.text);
        let old = a.sh.cat.tf("save.failed", &[("error", "")]);
        assert!(!st.text.starts_with(old.trim_end()), "{}", st.text);
    }

    #[test]
    fn ifc_save_to_readonly_dir_offers_dialog() {
        let (mut a, d) = app("ro");
        edit(&mut a, "L1", 0.0);
        let blocked = d.join("blocked");
        std::fs::write(&blocked, b"").unwrap();
        let t = blocked.join("x.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        assert!(a.save.strip.as_ref().unwrap().can_relocate);
        assert_ne!(a.sh.cat.t("save.btn.choose_location"), "save.btn.choose_location");
    }

    #[test]
    fn ctrl_s_unwritable_folder_says_why() {
        let (mut a, d) = app("ctrl_s_why");
        let s = d.join("gone").join("House.ifc");
        a.ed.model
            .sql()
            .execute(
                "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,first_import,imported_at) \
                 VALUES ('ifc:x','k',?1,'ifc','r','1',100,100)",
                [s.display().to_string()],
            )
            .unwrap();
        assert_eq!(first_ddc_target(&mut a), None);
        assert_eq!(
            a.ed.status.as_deref(),
            Some(a.sh.cat.t("save.ctrl_s_folder").as_str())
        );
        // read-only project: its own reason
        a.ed.project.set_read_only_for_test();
        assert_eq!(first_ddc_target(&mut a), None);
        assert_eq!(
            a.ed.status.as_deref(),
            Some(a.sh.cat.t("save.ctrl_s_read_only").as_str())
        );
    }

    #[test]
    fn the_flag_goes_on_only_through_its_function() {
        let m = Model::open(":memory:").unwrap();
        let s = src(Path::new("d/House.ifc"), true);
        use_original(&m, &s).unwrap();
        let row = m.save_target(FORMAT).unwrap().unwrap();
        assert!(row.over_source);
        let r = resolve_target(Some(&row), None, Some(&s)).unwrap();
        assert_eq!(r.target, s.path);
    }

    const STEP: &[u8] = b"ISO-10303-21;
DATA;
#1=IFCWALL('a');
ENDSEC;
END-ISO-10303-21;
";

    fn refused(tag: &str, bytes: &[u8]) -> (EditorApp, PathBuf, PathBuf) {
        let (mut a, d) = app(tag);
        let t = d.join("House.ifc");
        std::fs::write(&t, bytes).unwrap();
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        (a, d, t)
    }

    #[test]
    fn refusal_without_replace_writes_nothing() {
        let (a, d, t) = refused("sv14none", STEP);
        let r = a.save.refusal.as_ref().expect("the refusal is offered");
        assert_eq!(r.holds, guard::Holds::Ifc);
        assert!(!r.asked, "never replaced by the first press");
        assert_eq!(std::fs::read(&t).unwrap(), STEP);
        assert!(!d.join("_archive").exists());
    }

    #[test]
    fn later_save_clears_refusal_card() {
        let (mut a, d, t) = refused("sv15clear", STEP);
        assert!(a.save.refusal.is_some());
        let other = d.join("Other.ifc");
        assert!(start(&mut a, to(&other)));
        assert!(a.save.refusal.is_none());
        wait(&mut a);
        assert_eq!(std::fs::read(&t).unwrap(), STEP);
    }

    #[test]
    fn replace_anyway_archives_target_then_writes() {
        let (mut a, d, t) = refused("sv14rep", STEP);
        assert!(!replace_anyway(&mut a), "needs the question first");
        assert_eq!(std::fs::read(&t).unwrap(), STEP);
        ask_replace(&mut a);
        assert!(replace_anyway(&mut a));
        wait(&mut a);
        let arch: Vec<_> = std::fs::read_dir(d.join("_archive"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(arch.len(), 1);
        assert!(arch[0].file_name().unwrap().to_string_lossy().starts_with("House.ifc."));
        assert_eq!(std::fs::read(&arch[0]).unwrap(), STEP);
        assert!(t.is_file());
        assert_ne!(std::fs::read(&t).unwrap(), STEP);
        assert!(a.save.refusal.is_none());
    }

    fn last_report(a: &EditorApp) -> SaveReport {
        let row = a.ed.model.save_target(FORMAT).unwrap().unwrap();
        SaveReport::from_json(&row.last_report).unwrap()
    }

    #[test]
    fn save_as_over_other_existing_file_says_replaced_unknown() {
        let (mut a, d) = app("r38a-foreign");
        edit(&mut a, "L1", 0.0);
        add_content(&mut a);
        assert!(start(&mut a, to(&d.join("main.ifc"))));
        wait(&mut a);
        let other = d.join("other.ifc");
        std::fs::write(&other, STEP).unwrap();
        assert!(start(&mut a, to(&other)));
        wait(&mut a);
        let rep = last_report(&a);
        assert!(rep.replaced_unknown);
        assert!(!rep.target_changed);
    }

    #[test]
    fn save_to_own_file_changed_outside_says_target_changed() {
        let (mut a, d) = app("r38a-own-changed");
        edit(&mut a, "L1", 0.0);
        add_content(&mut a);
        let t = d.join("main.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        std::fs::write(&t, STEP).unwrap();
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        let rep = last_report(&a);
        assert!(rep.target_changed);
        assert!(!rep.replaced_unknown);
    }

    #[test]
    fn save_to_own_unchanged_file_flags_nothing() {
        let (mut a, d) = app("r38a-own-same");
        edit(&mut a, "L1", 0.0);
        let t = d.join("main.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        let rep = last_report(&a);
        assert!(!rep.target_changed);
        assert!(!rep.replaced_unknown);
    }

    #[test]
    fn export_ifc_does_not_change_save_target() {
        let (mut a, d) = app("exportcopy");
        edit(&mut a, "L1", 0.0);
        let t = d.join("main.ifc");
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        let before = a.ed.model.save_target(FORMAT).unwrap().unwrap();
        assert_eq!(before.path, t.display().to_string());
        export_copy(&mut a, d.join("copy.ifc"));
        wait(&mut a);
        assert!(d.join("copy.ifc").is_file());
        assert_eq!(a.ed.model.save_target(FORMAT).unwrap().unwrap(), before);
    }

    #[test]
    fn foreign_db_has_no_replace_action() {
        let (mut a, d) = app("sv14foreign");
        let t = d.join("notes.ifc");
        {
            let db = rusqlite::Connection::open(&t).unwrap();
            db.execute_batch("CREATE TABLE notes(t TEXT); INSERT INTO notes VALUES ('x');")
                .unwrap();
        }
        let before = std::fs::read(&t).unwrap();
        assert!(start(&mut a, to(&t)));
        wait(&mut a);
        assert_eq!(a.save.refusal.as_ref().unwrap().holds, guard::Holds::Foreign);
        ask_replace(&mut a);
        assert!(!a.save.refusal.as_ref().unwrap().asked);
        assert!(!replace_anyway(&mut a));
        assert_eq!(std::fs::read(&t).unwrap(), before);
        assert!(!d.join("_archive").exists());
    }

    #[test]
    fn replace_cancel_keeps_target() {
        let (mut a, d, t) = refused("sv14cancel", STEP);
        ask_replace(&mut a);
        assert!(a.save.refusal.as_ref().unwrap().asked);
        replace_cancel(&mut a);
        assert!(a.save.refusal.is_none());
        assert!(!replace_anyway(&mut a));
        assert_eq!(std::fs::read(&t).unwrap(), STEP);
        assert!(!d.join("_archive").exists());
    }

    #[test]
    fn commands_and_strings_exist_in_both_languages() {
        let reg = ddc_ui::Registry::builtin();
        for id in ["file.save_ifc", "file.save_ifc_as", "data.export_ifc"] {
            assert!(reg.get(id).is_some(), "{id}");
        }
        assert_eq!(
            reg.get("file.save").unwrap().shortcuts,
            vec!["Ctrl+S".to_string()]
        );
        for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
            let c = ddc_ui::Catalog::new(lang);
            for k in [
                "cmd.file.save_ifc",
                "cmd.file.save_ifc_as",
                "save.job.save",
                "save.stage.snapshot",
                "save.stage.write",
                "save.stage.verify",
                "save.stage.backup",
                "save.stage.replace",
                "save.failed",
                "save.failed_kept",
                "save.failed_new",
                "save.failed_new_kept",
                "save.btn.choose_location",
                "save.ctrl_s_read_only",
                "save.ctrl_s_folder",
                "save.failed_check",
                "save.cancelled",
                "save.source_refused",
                "save.target_changed",
                "save.replaced_unknown",
                "save.report.replaced_unknown",
                "save.saved_as",
                "save.original_on",
                "save.original_off",
                "save.pill.replaces",
                "cmd.file.ifc_replace_original",
                "save.btn.open_folder",
                "save.btn.use_original",
                "save.btn.report",
                "save.btn.change_location",
                "save.confirm.body",
                "save.confirm.rewrite",
                "save.confirm.losses",
                "save.confirm.yes",
                "save.confirm.no",
                "save.closing.title",
                "save.closing.body",
                "save.closing.cancel",
                "save.report.title",
                "save.report.written",
                "save.report.proxies",
                "save.report.ids",
                "save.report.ids_kept",
                "save.report.ids_new",
                "save.report.ids_conflict",
                "save.report.target_changed",
                "save.report.prev",
                "save.report.not",
                "save_guard.refuse.body",
                "save_guard.refuse.foreign",
                "save_guard.refuse.ask",
                "save_guard.refuse.holds_db",
                "save_guard.refuse.holds_ifc",
                "save_guard.refuse.replace",
                "save_guard.refuse.replace_yes",
                "save_guard.refuse.save_as",
                "save_guard.refuse.cancel",
                "save_guard.replace.moved",
                "save_guard.replace.move_failed",
            ] {
                assert_ne!(c.t(k), k, "{k}");
            }
        }
    }
}
