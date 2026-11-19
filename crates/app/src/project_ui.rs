// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! New / Open / Save / Save As: commands, background jobs, the unsaved-changes prompt, plain
//! messages, the window title and the File menu rows. Files themselves: `crate::project`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ddc_model::{
    open_project_copy, save_project_file, Error, LockError, ProjectError, ProjectLock,
};
use ddc_ui::notify::Level;
use ddc_ui::Theme;
use egui::{self, RichText};

use crate::jobs::{Job, Poll, Progress, CANCELLED};
use crate::project::{save_thumb, thumb_lines, with_project_ext, Opened, SaveMark, WorkFile};
use crate::shell::recover_win::{release_held, settle_recovery, Recover};
use crate::shell::EditorApp;

pub mod drafts;
#[cfg(test)]
mod drafts_contract;
#[cfg(test)]
mod drafts_overwrite_tests;
mod damaged;
mod save_fail;
pub use save_fail::{cause_text, save_fail_view, FailView};
use save_fail::{Again, SaveFail};

const APP_TITLE: &str = "OpenConstructionDesign";

/// Work a close or a model swap would lose: model edits, drawings the project file lacks,
/// draft editors (families, line library entry, line styles) not yet in the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unsaved {
    pub model: bool,
    pub drawings: usize,
    pub drafts: usize,
    /// Operations waiting in external agents' drafts: they live in the session, a close drops
    /// them, and no Save keeps them.
    pub agent: usize,
}

impl Unsaved {
    pub fn any(self) -> bool {
        self.project() || self.drafts > 0 || self.agent > 0
    }

    /// What the project file lacks: the title mark and Save look at this, drafts are not in it.
    pub fn project(self) -> bool {
        self.model || self.drawings > 0
    }
}

/// The one "has unsaved work" check every discard path asks.
pub fn unsaved(a: &EditorApp) -> Unsaved {
    Unsaved {
        model: a.ed.is_model_dirty(),
        drawings: a.plan.drawings.unsaved_count(),
        drafts: drafts::count(&a.ed),
        agent: crate::ai::session::with_session(|s| {
            s.as_ref().map_or(0, |u| u.agent_writes_waiting())
        }),
    }
}

/// What to do once the changes are saved or discarded.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    New,
    Open(PathBuf),
    Close,
    Sample,
    Restore {
        snap: PathBuf,
        project: Option<PathBuf>,
        /// From the damaged-file dialog: the restored model binds to no file.
        unbound: bool,
    },
    /// An Open finished after edits were made: the project waits in `State::opened`.
    Adopt,
    /// A project built from a model file is ready; it waits in `State::built`.
    AdoptBuilt,
    /// A model file becomes a new project: asked now, before the long build.
    BuildFirst(PathBuf),
}

struct SaveJob {
    job: Job<()>,
    target: PathBuf,
    lock: Option<ProjectLock>,
    mark: SaveMark,
}

struct OpenJob {
    job: Job<Result<Opened, ProjectError>>,
    mark: SaveMark,
    /// Drawing list signature when the Open began (the prompt was answered for that).
    drawings: u64,
    path: PathBuf,
    /// Size of the project file, taken when the open starts.
    size: Option<u64>,
    /// A kept copy of this damaged file is opening: it is adopted bound to no file.
    recovered_from: Option<PathBuf>,
}

enum Running {
    Save(SaveJob),
    Open(OpenJob),
}

enum Notice {
    /// Another instance holds the file: offers opening it read-only.
    Locked(PathBuf),
    Plain {
        title: String,
        body: String,
    },
    /// A save did not happen: cause, Save as, Retry.
    SaveFailed(SaveFail),
    /// The file cannot be read: its kept copies and snapshots are offered.
    Damaged(damaged::Damaged),
}

#[derive(Default)]
pub struct State {
    job: Option<Running>,
    prompt: Option<Pending>,
    notice: Option<Notice>,
    after_save: Option<Pending>,
    /// Save all stopped on the family overwrite question: the action waits for its answer.
    after_drafts: Option<Pending>,
    /// Save all is running: family sketches go only once that save succeeds.
    sketches_after_save: bool,
    /// Finished Open waiting for the answer about edits made meanwhile.
    opened: Option<Opened>,
    /// Finished import build waiting for the unsaved-work answer.
    built: Option<crate::open::build::Built>,
    /// A finished build whose unsaved-work question was answered Cancel: waits for "Open it".
    kept_build: Option<crate::open::build::Built>,
    /// Model revision at the moment the question before a build was answered.
    cleared: Option<u64>,
    /// Recovery dialog waiting for the answer: comes back if the restore does not happen.
    parked: Option<Recover>,
    allow_close: bool,
    close_now: bool,
    sent: String,
    applied: String,
    /// A project to open from a surface that has no `EditorApp` at hand (next frame).
    pub open_request: Option<PathBuf>,
}

impl State {
    /// A project save is running (the status bar shows it).
    pub fn saving(&self) -> bool {
        matches!(self.job, Some(Running::Save(_)))
    }

    /// A prompt or message is up: canvas shortcuts wait.
    pub fn modal(&self) -> bool {
        self.prompt.is_some() || self.notice.is_some()
    }

    /// A save or an open is running: the model must not be replaced or fed from outside.
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.built.is_some()
    }

    /// The title the window has right now (the native title bar is looked up by it).
    pub fn applied_title(&self) -> &str {
        if self.applied.is_empty() {
            APP_TITLE
        } else {
            &self.applied
        }
    }
}

pub fn register(a: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("file.new", request_new),
        ("file.open", h_open),
        ("file.save", h_save),
        ("file.save_as", h_save_as),
        ("file.save_copy", h_save_copy),
        ("file.open_kept", h_open_kept),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
}

fn h_open(a: &mut EditorApp) {
    // the unsaved-work prompt comes from the step that replaces the project, once
    crate::open::pick(a, crate::open::decide::Door::OpenDialog);
}

fn h_save(a: &mut EditorApp) {
    // an untitled project that came from a file saves beside it, no dialog
    if let Some(t) = crate::open::first_save_target(a) {
        if start_save(a, Some(t.clone())) {
            crate::open::saved_as_notice(a, &t);
        }
        return;
    }
    start_save(a, None);
}

fn h_save_as(a: &mut EditorApp) {
    if let Some(p) = pick_save_path(a) {
        start_save(a, Some(p));
    }
}

/// A copy to hand to someone else: the project stays as it is, the copy carries no AI consent
/// and, unless asked, no AI journal. The default answer is to leave the journal out.
fn h_save_copy(a: &mut EditorApp) {
    if a.project_ui.job.is_some() {
        status(a, "job.busy");
        return;
    }
    let Some(target) = pick_save_path(a) else {
        return;
    };
    if a.ed.project.path().is_some_and(|p| p == target.as_path()) {
        status(a, "ai.copy.same");
        return;
    }
    // the copy carries the placed drawings as they are now
    if let Err(e) = crate::drawing::store::persist(a) {
        save_failed(a, &e, Again::Copy);
        return;
    }
    let keep = crate::file_pick::confirm_yes_no(a.sh.cat.t("ai.copy.title"), a.sh.cat.t("ai.copy.journal"));
    match a.ed.model.save_shared_copy(&target, keep, &mut |_, _| Ok(())) {
        Ok(()) => {
            let name = file_name(&target);
            a.ed.status = Some(a.sh.cat.tf("ai.copy.done", &[("name", name.as_str())]));
        }
        Err(e) => {
            let kind = crate::io_kind::model_kind(&e);
            save_fail::report_as(a, kind, &e.to_string(), Again::Copy);
        }
    }
}

/// New project, after the unsaved-changes prompt.
pub fn request_new(a: &mut EditorApp) {
    guard(a, Pending::New);
}

/// Opens a project file, after the unsaved-changes prompt.
pub fn request_open(a: &mut EditorApp, path: PathBuf) {
    guard(a, Pending::Open(path));
}

/// A project built on a worker from a model file replaces the current one, after the
/// unsaved-changes prompt (asked now, after the wait, not before it).
pub fn hand_built(a: &mut EditorApp, b: crate::open::build::Built) {
    a.project_ui.built = Some(b);
    guard(a, Pending::AdoptBuilt);
}

/// A model file becomes a new project. The unsaved-work question comes now, before the build,
/// so that Cancel never throws away a finished import.
/// False when nothing started and nothing was asked (busy, no module, unsupported).
pub fn request_build(a: &mut EditorApp, path: PathBuf) -> bool {
    guard(a, Pending::BuildFirst(path)) && (a.project_ui.prompt.is_some() || a.big.busy())
}

/// Command `file.open_kept`: the build that was parked by Cancel asks again and opens.
pub fn h_open_kept(a: &mut EditorApp) {
    if a.project_ui.built.is_none() {
        a.project_ui.built = a.project_ui.kept_build.take();
    }
    // the answer given before that build may belong to another one: ask again if dirty
    a.project_ui.cleared = None;
    if a.project_ui.built.is_some() {
        guard(a, Pending::AdoptBuilt);
    }
}

/// Moves a held value into the parking slot (an earlier parked one is replaced).
fn park<T>(slot: &mut Option<T>, kept: &mut Option<T>) {
    if let Some(v) = slot.take() {
        *kept = Some(v);
    }
}

/// Does replacing the model need the question? A build whose question was answered at the start
/// asks again only when the model was edited since.
fn needs_ask(dirty: bool, adopts_built: bool, cleared: Option<u64>, revision: u64) -> bool {
    dirty && !(adopts_built && cleared == Some(revision))
}

/// The sample replaces the model, after the unsaved-changes prompt.
pub fn request_sample(a: &mut EditorApp) {
    guard(a, Pending::Sample);
}

/// Restores snapshot `snap` of the dialog `r`, after the unsaved-changes prompt.
pub fn request_restore(a: &mut EditorApp, r: Recover, snap: PathBuf) {
    let (project, unbound) = (r.project.clone(), r.damaged);
    a.project_ui.parked = Some(r);
    guard(a, Pending::Restore { snap, project, unbound });
}

/// Working copies whose snapshot failed are kept: the user has to know where.
pub fn notify_kept(a: &mut EditorApp, kept: &[PathBuf]) {
    let folder = kept
        .first()
        .and_then(|p| p.parent())
        .map_or_else(String::new, |d| d.display().to_string());
    let c = &a.sh.cat;
    let title = c.t("project.orphans.title");
    let body = c.tf("project.orphans.body", &[("folder", folder.as_str())]);
    a.project_ui.notice = Some(Notice::Plain { title, body });
}

/// A rollback failed: the listed files are not beside their main file any more. The folders
/// of the kept copies are named too, since this notice replaces the one that names them.
pub fn notify_stranded(a: &mut EditorApp, kept: &[PathBuf], stranded: &[PathBuf]) {
    let mut folders: Vec<String> = Vec::new();
    for d in kept.iter().filter_map(|p| p.parent()) {
        let d = d.display().to_string();
        if !folders.contains(&d) {
            folders.push(d);
        }
    }
    let folder = folders.join("\n");
    let list = stranded
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let c = &a.sh.cat;
    let title = c.t("project.orphans.stranded_title");
    let args = [("paths", list.as_str()), ("folder", folder.as_str())];
    let body = c.tf("project.orphans.stranded_body", &args);
    a.project_ui.notice = Some(Notice::Plain { title, body });
}

/// Per frame, once.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    if std::mem::take(&mut a.project_ui.close_now) {
        a.project_ui.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    handle_close(a, ctx);
    if let Some(p) = a.project_ui.open_request.take() {
        request_open(a, p);
    }
    poll_job(a);
    resume_drafts(a);
    tell_settings_failure(a);
    job_window(a, ctx);
    prompt_window(a, ctx);
    notice_window(a, ctx);
    update_title(a, ctx);
    if a.project_ui.close_now {
        ctx.request_repaint();
    }
}

/// Settings, recent projects or last views are not reaching disk: one toast a session.
pub(crate) fn tell_settings_failure(a: &mut EditorApp) {
    let Some(e) = a.prefs.write_fail.untold() else {
        return;
    };
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let why = cause_text(&a.sh.cat, &e).unwrap_or_else(|| crate::ui_err::humanize(&e, ru));
    let msg = a.sh.cat.tf("project.prefs_not_saved", &[("why", why.as_str())]);
    a.sh.toasts.push(Level::Warning, msg, None);
}

fn handle_close(a: &mut EditorApp, ctx: &egui::Context) {
    if a.project_ui.allow_close || !ctx.input(|i| i.viewport().close_requested()) {
        return;
    }
    if crate::save::hold_close(a, ctx) {
        return;
    }
    if close_requested(a) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    }
}

/// The window is asked to close: `true` when the close waits for a running save or a prompt.
pub(crate) fn close_requested(a: &mut EditorApp) -> bool {
    if matches!(a.project_ui.job, Some(Running::Save(_))) {
        a.project_ui.after_save = Some(Pending::Close);
        true
    } else if unsaved(a).any() || workbook_pending(a) > 0 {
        a.project_ui.notice = None;
        forget_pending(a);
        a.project_ui.prompt = Some(Pending::Close);
        true
    } else {
        false
    }
}

/// Workbook edits and undecided column matches the linked preview holds and the model does not
/// yet: closing or switching project would drop them. A synced preview holds nothing.
fn workbook_pending(a: &EditorApp) -> usize {
    match &a.xlsx {
        Some(pv) if !pv.synced => {
            let c = pv.report.counts();
            c.added + c.changed + c.deleted + pv.report.proposals.len()
        }
        _ => 0,
    }
}

/// What was held for a prompted action goes back: that action will not run.
fn forget_pending(a: &mut EditorApp) {
    a.project_ui.after_save = None;
    a.project_ui.after_drafts = None;
    a.project_ui.sketches_after_save = false;
    a.project_ui.opened = None;
    let st = &mut a.project_ui;
    park(&mut st.built, &mut st.kept_build);
    if let Some(r) = a.project_ui.parked.take() {
        a.recover = Some(r);
    }
}

/// A refused holding action `p` gives back only its own held value; the waiting action's stays.
fn release_own(a: &mut EditorApp, p: &Pending) {
    let waits = a.project_ui.after_drafts.as_ref();
    if waits.is_some_and(|w| std::mem::discriminant(w) == std::mem::discriminant(p)) {
        return;
    }
    let st = &mut a.project_ui;
    match p {
        Pending::Restore { .. } => {
            if let Some(r) = st.parked.take() {
                a.recover = Some(r);
            }
        }
        Pending::AdoptBuilt => park(&mut st.built, &mut st.kept_build),
        _ => st.opened = None,
    }
}

fn status(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

/// The one way to replace the model: runs `p` if nothing would be lost, otherwise asks. Refuses
/// while a save or an open runs, so no job can end up bound to another model.
/// False when the request was refused (a dialog is open or a job is running), true otherwise.
fn guard(a: &mut EditorApp, p: Pending) -> bool {
    let holds = matches!(
        p,
        Pending::Restore { .. } | Pending::Adopt | Pending::AdoptBuilt
    );
    // an action waiting on the family overwrite answer keeps waiting: this one is refused
    if a.project_ui.after_drafts.is_some() {
        status(a, "drafts_overwrite.drafts.ow_wait");
        if holds {
            release_own(a, &p);
        }
        return false;
    }
    if a.project_ui.modal() {
        if holds {
            forget_pending(a);
        }
        return false;
    }
    // a module import still running would land in the replaced model
    if a.project_ui.job.is_some() || a.big.busy() || a.rvt_import_running() {
        status(a, "job.busy");
        if holds {
            forget_pending(a);
        }
        return false;
    }
    // the answer given before a build covers model edits only; drawings, drafts and workbook
    // always ask
    let u = unsaved(a);
    let dirty = needs_ask(
        u.model,
        p == Pending::AdoptBuilt,
        a.project_ui.cleared,
        a.ed.revision,
    ) || u.drawings > 0
        || u.drafts > 0
        || u.agent > 0
        || workbook_pending(a) > 0;
    if dirty {
        a.project_ui.prompt = Some(p);
    } else {
        proceed(a, p);
    }
    true
}

fn proceed(a: &mut EditorApp, p: Pending) {
    match p {
        Pending::New => do_new(a),
        Pending::Open(path) => start_open(a, path, false),
        Pending::Close => a.project_ui.close_now = true,
        Pending::Sample => crate::help::load_sample(a),
        Pending::Restore { snap, project, unbound } => {
            do_restore(a, &snap, project.as_deref(), unbound)
        }
        Pending::Adopt => {
            if let Some(o) = a.project_ui.opened.take() {
                finish_open(a, o);
            }
        }
        Pending::AdoptBuilt => {
            a.project_ui.cleared = None;
            if let Some(b) = a.project_ui.built.take() {
                crate::open::build::adopt(a, b);
            }
        }
        Pending::BuildFirst(path) => {
            a.project_ui.cleared = Some(a.ed.revision);
            if !crate::open::build::start(a, path) {
                a.project_ui.cleared = None;
            }
        }
    }
}

fn do_restore(a: &mut EditorApp, snap: &Path, project: Option<&Path>, unbound: bool) {
    match a.restore(snap, project, unbound) {
        Ok(()) => {
            after_swap(a);
            a.welcome_closed = true;
            let key = if unbound { "project.recovered_toast" } else { "recover.restored" };
            let msg = a.sh.cat.t(key);
            a.sh.toasts.push(Level::Success, msg, None);
            if let Some(r) = a.project_ui.parked.take() {
                release_held(&r.snaps);
                settle_recovery(&r.folder, project);
            }
            // the restored work wins over a file named on the command line
            a.chrome.start.arg_file = None;
            a.chrome.recover_note = None;
            a.next_recovery();
        }
        Err(e) => {
            if let Some(mut r) = a.project_ui.parked.take() {
                r.diff = Some(Err(e));
                a.recover = Some(r);
            } else {
                plain(a, "project.open_failed.title", "project.open_failed.body", &e);
            }
        }
    }
}

fn do_new(a: &mut EditorApp) {
    match a.ed.new_project() {
        Ok(()) => {
            after_swap(a);
            a.welcome_closed = false;
        }
        Err(e) => {
            let detail = e.to_string();
            plain(a, "project.open_failed.title", "project.open_failed.body", &detail);
        }
    }
}

/// The model is another one now: everything held for the old one goes.
pub(crate) fn after_swap(a: &mut EditorApp) {
    crate::sheet_view::deactivate();
    crate::drafting_view::deactivate();
    a.section = None;
    a.tabs.clear();
    a.view3d = false;
    a.wall_edit = None;
    a.curtain_edit = None;
    a.xlsx = None;
    crate::shell::xlsx_win::forget_link();
    // a book read still running was compared with the old model: its preview never lands here
    if let Some(j) = a.xlsx_load.take() {
        j.cancel();
    }
    a.level_edit = None;
    a.clash = Default::default();
    a.src = Default::default();
    a.links = Default::default();
    // the unsaved prompt was answered before any swap: the list is what the new model stores
    crate::drawing::store::reload(a);
    a.autosave.mark_clean(a.ed.revision);
    crate::bigfile::reset_feed(a);
    a.plan.fit(&a.ed);
    a.view.refit();
    a.panes.refit_cams();
    a.sview.refit();
    crate::open::first_view::on_swap(a);
    a.note_session();
}

fn start_open(a: &mut EditorApp, path: PathBuf, read_only: bool) {
    let (lock, read_only) = if read_only {
        (None, true)
    } else {
        match a.ed.project.claim(&path) {
            Ok(l) => (l, false),
            Err(LockError::Busy) => {
                a.project_ui.notice = Some(Notice::Locked(path));
                return;
            }
            // the folder takes no lock file: nothing could be saved there either
            Err(LockError::Io(_)) => (None, true),
        }
    };
    let work = match WorkFile::create() {
        Ok(w) => w,
        Err(e) => {
            let detail = e.to_string();
            plain(a, "project.open_failed.title", "project.open_failed.body", &detail);
            return;
        }
    };
    let mark = a.ed.save_mark();
    let drawings = a.plan.drawings.stored_sig();
    let opened = path.clone();
    let job = Job::spawn("job.title.open", move |c: &Arc<Progress>| {
        c.phase("job.stage.open", 0.0, 0.9);
        let mut step = |done: u64, total: u64| progress(c, done, total);
        let r = open_project_copy(&path, work.path(), !read_only, &mut step);
        if c.is_cancelled() {
            return Err(CANCELLED.to_string());
        }
        // the check runs inside the open: reaching here means it passed
        c.phase("job.stage.check", 0.9, 1.0);
        c.finish();
        Ok(r.map(|copy| Opened {
            copy,
            work,
            path,
            lock,
            read_only,
            recovered_from: None,
        }))
    });
    a.project_ui.job = Some(Running::Open(OpenJob {
        job,
        mark,
        drawings,
        size: crate::bigfile::file_size(&opened),
        path: opened,
        recovered_from: None,
    }));
}

fn progress(c: &Progress, done: u64, total: u64) -> Result<(), Error> {
    if total > 0 && done >= total && c.stage() == "job.stage.open" {
        // every page is copied: the integrity check of the copy runs next, without a count
        c.phase("job.stage.check", 0.9, 1.0);
        return check_cancel(c);
    }
    c.set_total(total);
    c.set(done);
    check_cancel(c)
}

fn check_cancel(c: &Progress) -> Result<(), Error> {
    if c.is_cancelled() {
        Err(Error::Invalid(CANCELLED.into()))
    } else {
        Ok(())
    }
}

/// The name for a Save As.
pub(crate) fn pick_save_path(a: &EditorApp) -> Option<PathBuf> {
    let filter = a.sh.cat.t("project.filter");
    let (name, dir) = save_as_seed(a);
    let mut d = crate::file_pick::Dlg::new()
        .add_filter(filter, &[ddc_model::PROJECT_EXT])
        .set_file_name(name);
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.save_file().map(with_project_ext)
}

/// File name and folder a Save As suggests. A copy restored for a damaged file gets
/// `<name> (recovered).<ext>` beside it, never the damaged file's own name.
fn save_as_seed(a: &EditorApp) -> (String, Option<PathBuf>) {
    if let Some(bad) = a.ed.project.recovered_from() {
        let dir = bad.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf);
        return (recovered_name(&a.sh.cat, bad), dir);
    }
    let name =
        a.ed.project_name()
            .unwrap_or_else(|| format!("{}.{}", a.sh.cat.t("project.untitled"), ddc_model::PROJECT_EXT));
    (name, a.ed.project.path().and_then(Path::parent).map(Path::to_path_buf))
}

/// `<stem> (recovered).<ext>`, numbered when that name is taken next to `bad`.
fn recovered_name(c: &ddc_ui::Catalog, bad: &Path) -> String {
    let stem = bad.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = bad
        .extension()
        .map_or_else(|| ddc_model::PROJECT_EXT.to_string(), |e| e.to_string_lossy().into_owned());
    let base = c.tf("project.recovered_name", &[("name", stem.as_str())]);
    let dir = bad.parent().unwrap_or(Path::new(""));
    let mut name = format!("{base}.{ext}");
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("{base} ({n}).{ext}");
        n += 1;
    }
    name
}

/// The file a plain Save replaces; `None`: Save asks for a name.
fn bound_target(a: &EditorApp) -> Option<PathBuf> {
    let p = &a.ed.project;
    p.path().filter(|_| !p.read_only()).map(Path::to_path_buf)
}

/// `true` when the save began or finished; `false` when there was nothing to do, the name dialog
/// was cancelled or a message says why not.
fn start_save(a: &mut EditorApp, target: Option<PathBuf>) -> bool {
    // a finished import waits for its answer in the big slot; a save would write rows under it
    if a.project_ui.job.is_some() || a.big.busy() {
        status(a, "job.busy");
        return false;
    }
    // first, so the dirty check and the save mark both see the drawings
    if let Err(e) = crate::drawing::store::begin_save(a) {
        save_failed(a, &e, Again::Save(target.clone()));
        return false;
    }
    let target = match (target, bound_target(a)) {
        (Some(t), _) => t,
        (None, Some(c)) => {
            if !unsaved(a).project() && c.exists() {
                return false;
            }
            c
        }
        (None, None) => {
            // a restored copy of a damaged file always asks: no name is picked for it
            let first = if a.ed.project.recovered_from().is_some() {
                None
            } else {
                crate::save::first_ddc_target(a)
            };
            match first.or_else(|| pick_save_path(a)) {
                Some(p) => p,
                None => return false,
            }
        }
    };
    let lock = match a.ed.project.claim(&target) {
        Ok(l) => l,
        Err(LockError::Busy) => {
            let name = file_name(&target);
            let title = a.sh.cat.t("project.locked.title");
            let body =
                a.sh.cat
                    .tf("project.locked_save.body", &[("name", name.as_str())]);
            a.project_ui.notice = Some(Notice::Plain { title, body });
            return false;
        }
        Err(LockError::Io(e)) => {
            let kind = crate::io_kind::io_kind(&e);
            save_fail::report_as(a, kind, &e.to_string(), Again::Save(Some(target)));
            return false;
        }
    };
    let mut mark = a.ed.save_mark();
    mark.side = a.autosave.side_mark();
    let lines = thumb_lines(&a.ed);
    let logo = crate::logo_ui::project_logo(&a.ed.model);
    let (want, missing) = crate::drawing::store::expected(a);
    let Some(work) = a.ed.project.work_path().map(Path::to_path_buf) else {
        // in memory: no second connection can reach it, so it saves on this thread
        let r = a.ed.model.save_project_to(&target, &mut |_, _| Ok(()));
        if let Err(e) = r {
            let kind = crate::io_kind::model_kind(&e);
            save_fail::report_as(a, kind, &e.to_string(), Again::Save(Some(target)));
            return false;
        }
        let r = crate::drawing::store::check_file(&target, want, &missing);
        return match r {
            Ok(()) => {
                save_thumb(&target, &lines, logo.as_ref());
                finish_save(a, &target, lock, mark);
                true
            }
            Err(e) => {
                save_failed(a, &e, Again::Save(Some(target)));
                false
            }
        };
    };
    let to = target.clone();
    let job = Job::spawn("project.job.save", move |c: &Arc<Progress>| {
        c.phase("project.stage.save", 0.0, 1.0);
        let mut step = |done: u64, total: u64| progress(c, done, total);
        save_project_file(&work, &to, &mut step).map_err(|e| {
            if c.is_cancelled() {
                CANCELLED.to_string()
            } else {
                e.to_string()
            }
        })?;
        crate::drawing::store::check_file(&to, want, &missing)?;
        save_thumb(&to, &lines, logo.as_ref());
        Ok(())
    });
    a.project_ui.job = Some(Running::Save(SaveJob {
        job,
        target,
        lock,
        mark,
    }));
    true
}

fn finish_save(a: &mut EditorApp, target: &Path, lock: Option<ProjectLock>, mark: SaveMark) {
    let sketches = std::mem::take(&mut a.project_ui.sketches_after_save);
    if a.ed.project_saved(target, lock, mark) {
        if sketches {
            drafts::saved();
        }
        a.prefs.touch_project(&target.display().to_string());
        a.prefs.save();
        // edits made while the save ran are not in the file: the snapshot still owes them
        a.autosave.mark_saved(mark.rev, mark.side);
        crate::drawing::store::saved(a);
        a.note_session();
    }
    let name = file_name(target);
    if !crate::save::announce_ddc_saved(a, target, &name) {
        let msg = a.sh.cat.tf("project.saved", &[("name", name.as_str())]);
        a.sh.toasts.push(Level::Success, msg, None);
    }
    if let Some(p) = a.project_ui.after_save.take() {
        // edits made during the save still ask
        guard(a, p);
    }
}

fn save_failed(a: &mut EditorApp, detail: &str, again: Again) {
    save_fail::report(a, detail, again);
}

/// Model or drawing edits made after the prompt was answered, while the Open ran.
fn edited_during(a: &EditorApp, mark: SaveMark, drawings: u64) -> bool {
    // drafts asked about before the open were saved or dropped: any left now are new
    a.ed.edited_since(mark)
        || (a.plan.drawings.unsaved_count() > 0 && a.plan.drawings.stored_sig() != drawings)
        || drafts::count(&a.ed) > 0
}

fn finish_open(a: &mut EditorApp, o: Opened) {
    let (upgraded, backup) = (o.copy.upgraded_from, o.copy.backup.clone());
    let (path, read_only) = (o.path.clone(), o.read_only);
    let recovered = o.recovered_from.is_some();
    if let Err(e) = a.ed.adopt(o) {
        let detail = e.to_string();
        plain(a, "project.open_failed.title", "project.open_failed.body", &detail);
        return;
    }
    after_swap(a);
    a.welcome_closed = true;
    // the project opens in the view of the chosen focus (3D, plan, data, AI)
    a.default_view();
    if recovered {
        // a kept copy is no project of its own: not in the recent list, Save asks for a name
        let msg = a.sh.cat.t("project.recovered_toast");
        a.sh.toasts.push(Level::Info, msg, None);
        return;
    }
    a.prefs.touch_project(&path.display().to_string());
    a.prefs.save();
    if upgraded.is_some() {
        let msg = match &backup {
            Some(b) => {
                let name = file_name(b);
                a.sh.cat.tf("project.upgraded", &[("backup", name.as_str())])
            }
            None => a.sh.cat.t("project.upgraded_ro"),
        };
        a.sh.toasts.push(Level::Info, msg, None);
    }
    if read_only {
        let msg = a.sh.cat.t("project.read_only_toast");
        a.sh.toasts.push(Level::Info, msg, None);
    }
}

fn open_failed(a: &mut EditorApp, e: ProjectError, j: &OpenJob) {
    let path = j.path.as_path();
    match e {
        ProjectError::Locked(p) => a.project_ui.notice = Some(Notice::Locked(p)),
        ProjectError::Newer { found, supported } => {
            let (found, supported) = (found.to_string(), supported.to_string());
            let c = &a.sh.cat;
            let title = c.t("project.newer.title");
            let body = c.tf(
                "project.newer.body",
                &[("found", found.as_str()), ("supported", supported.as_str())],
            );
            a.project_ui.notice = Some(Notice::Plain { title, body });
        }
        ProjectError::Damaged(d) => {
            let dir = a.prefs.autosave_folder();
            let mut dm = damaged::find(path, d, dir.as_deref());
            dm.since = Some((j.mark, j.drawings));
            a.project_ui.notice = Some(Notice::Damaged(dm));
        }
        ProjectError::NotProject(_) => {
            let c = &a.sh.cat;
            let (title, body) = (c.t("project.not_project.title"), c.t("project.not_project.body"));
            a.project_ui.notice = Some(Notice::Plain { title, body });
        }
        other => {
            let detail = other.to_string();
            plain(a, "project.open_failed.title", "project.open_failed.body", &detail);
        }
    }
}

/// A message with `{detail}` in the words of the application.
fn plain(a: &mut EditorApp, title: &str, body: &str, detail: &str) {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let detail = crate::ui_err::humanize(detail, ru);
    let c = &a.sh.cat;
    let (title, body) = (c.t(title), c.tf(body, &[("detail", detail.as_str())]));
    a.project_ui.notice = Some(Notice::Plain { title, body });
}

fn poll_job(a: &mut EditorApp) {
    let Some(run) = a.project_ui.job.take() else {
        return;
    };
    match run {
        // a file already replaced stays replaced, however late Cancel was pressed
        Running::Save(mut s) => match s.job.poll_finished() {
            Poll::Running => a.project_ui.job = Some(Running::Save(s)),
            Poll::Done(()) => finish_save(a, &s.target, s.lock, s.mark),
            Poll::Cancelled => {
                forget_pending(a);
                status(a, "job.cancelled");
            }
            Poll::Failed(e) => save_failed(a, &e, Again::Save(Some(s.target))),
        },
        Running::Open(mut j) => match j.job.poll() {
            Poll::Running => a.project_ui.job = Some(Running::Open(j)),
            // the prompt came before the Open began: what was done since has not been asked about
            Poll::Done(Ok(mut o)) if edited_during(a, j.mark, j.drawings) => {
                o.recovered_from = j.recovered_from.take();
                a.project_ui.opened = Some(o);
                a.project_ui.notice = None;
                a.project_ui.prompt = Some(Pending::Adopt);
            }
            Poll::Done(Ok(mut o)) => {
                o.recovered_from = j.recovered_from.take();
                finish_open(a, o)
            }
            Poll::Done(Err(e)) => {
                crate::jobs::record_end(
                    "job.title.open",
                    crate::jobs::Ended::Failed,
                    Some(e.to_string()),
                );
                open_failed(a, e, &j)
            }
            Poll::Cancelled => status(a, "job.cancelled"),
            Poll::Failed(e) => open_failed(a, ProjectError::Model(Error::Invalid(e)), &j),
        },
    }
}

/// Stages of a project open: the page copy, then the check of the copy.
pub const STAGES_OPEN: &[&str] = &["job.stage.open", "job.stage.check"];

/// The running project open as the loading window shows it.
pub fn open_view(a: &EditorApp) -> Option<crate::jobs::JobView> {
    let Some(Running::Open(j)) = &a.project_ui.job else {
        return None;
    };
    let name = j.path.file_name().map(|n| n.to_string_lossy().into_owned());
    let mut v = j.job.view(STAGES_OPEN, name);
    v.size = j.size;
    Some(v)
}

/// Cancel of the running project open; the current project stays as it is.
pub fn cancel_open(a: &EditorApp) {
    if let Some(Running::Open(j)) = &a.project_ui.job {
        j.job.cancel();
    }
}

fn job_window(a: &EditorApp, ctx: &egui::Context) {
    let Some(r) = &a.project_ui.job else {
        return;
    };
    let view = match r {
        Running::Save(s) => s.job.view(&[], None),
        // the loading window shows an open from its first frame
        Running::Open(_) => return,
    };
    let (th, c) = (&a.sh.theme, &a.sh.cat);
    let mut stop = false;
    // above the task window, whose height depends on its stages
    let lift = 44.0 + a.chrome.tasks.window_h;
    crate::ui_kit::Dialog::hud(
        "project_job",
        c.t("project.job.window"),
        egui::Align2::RIGHT_BOTTOM,
        [-12.0, -lift],
    )
    .size([ddc_ui::theme::layout::TOAST_W, 0.0])
    .stable()
    .no_title_bar()
    .show(ctx, |ui| {
        crate::shell::tasks_ui::body(ui, th, c, &view, None, &mut stop);
    });
    if stop {
        match r {
            Running::Save(s) => s.job.cancel(),
            Running::Open(j) => j.job.cancel(),
        }
    }
    ctx.request_repaint_after(Duration::from_millis(100));
}

fn primary(ui: &mut egui::Ui, _th: &Theme, text: String) -> egui::Response {
    crate::ui_kit::primary_button(ui, text, true)
}

fn message(ui: &mut egui::Ui, th: &Theme, title: &str, body: &str) {
    ui.set_width(440.0);
    ui.add(egui::Label::new(RichText::new(title).heading()).truncate());
    ui.add_space(6.0);
    ui.label(RichText::new(body).color(th.c("text.secondary")));
    ui.add_space(12.0);
}

fn right_to_left(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    // one row high: a plain layout would stretch the auto-sized modal to the whole screen
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let layout = egui::Layout::right_to_left(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, add);
}

/// What the unsaved-changes question can end in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Save,
    Review,
    Discard,
    Cancel,
}

/// The kit footer's answer as an action: the primary button (and Enter) is `main`, Discard is
/// only ever the danger button.
fn act_of(main: Act, c: crate::unsaved::Choice) -> Act {
    use crate::unsaved::Choice;
    match c {
        Choice::Apply => main,
        Choice::Cancel => Act::Cancel,
        Choice::Discard => Act::Discard,
    }
}

fn prompt_window(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(p) = a.project_ui.prompt.clone() else {
        return;
    };
    let held = drafts::labels(&a.sh.cat, &a.ed.model);
    prompt_with(a, ctx, p, held);
}

/// Layout probe seam: the close question with `held` as the draft lines; `false` once answered.
#[cfg(test)]
pub(crate) fn layout_prompt(a: &mut EditorApp, ctx: &egui::Context, held: &[String]) -> bool {
    if a.project_ui.prompt.is_none() {
        a.project_ui.prompt = Some(Pending::Close);
    }
    let p = Pending::Close;
    prompt_with(a, ctx, p, held.to_vec());
    a.project_ui.prompt.is_some()
}

/// Layout probe seam: whether the answer to the close question would close the window now.
#[cfg(test)]
pub(crate) fn layout_closes(a: &EditorApp) -> bool {
    a.project_ui.close_now
}

/// Layout probe seam: shows the failed-save dialog for `kind` with the system text `raw`.
#[cfg(test)]
pub(crate) fn layout_save_fail(
    a: &mut EditorApp,
    ctx: &egui::Context,
    kind: Option<crate::io_kind::IoKind>,
    raw: &str,
) -> bool {
    if let Some(kind) = kind {
        let again = save_fail::Again::Save(None);
        let f = SaveFail { kind, raw: raw.to_string(), again };
        a.project_ui.notice = Some(Notice::SaveFailed(f));
    }
    if let Some(Notice::SaveFailed(f)) = a.project_ui.notice.take() {
        save_fail::window(a, ctx, f);
    }
    a.project_ui.notice.is_some()
}

/// Layout probe seam: the damaged-file dialog, on `first` with a previous copy and/or
/// snapshots of the same date (so the previous copy leads).
#[cfg(test)]
pub(crate) fn layout_damaged(
    a: &mut EditorApp,
    ctx: &egui::Context,
    first: bool,
    prev: bool,
    snaps: bool,
) -> bool {
    if first {
        let at = web_time::SystemTime::now();
        let path = PathBuf::from("C:/projects/storage_building_block_b_revision_7.ddc");
        let kept = path.with_file_name("storage_building_block_b_revision_7.ddc.prev");
        let dm = damaged::Damaged {
            prev: prev.then(|| damaged::Kept { path: kept, at }),
            snaps: snaps.then_some((at, 12)),
            detail: "file is not a database".into(),
            path,
            since: None,
        };
        a.project_ui.notice = Some(Notice::Damaged(dm));
    }
    if let Some(Notice::Damaged(d)) = a.project_ui.notice.take() {
        damaged::window(a, ctx, d);
    }
    a.project_ui.notice.is_some()
}

fn prompt_with(a: &mut EditorApp, ctx: &egui::Context, p: Pending, held: Vec<String>) {
    let u = unsaved(a);
    // model and drawing edits both go to the project file
    let name = project_label(a);
    let dirty = a.ed.is_dirty();
    let n = workbook_pending(a);
    let main = if u.any() {
        Act::Save
    } else if n > 0 {
        Act::Review
    } else {
        Act::Save
    };
    let c = &a.sh.cat;
    let th = &a.sh.theme;
    let (title, body) = if dirty || u.drawings > 0 || u.agent > 0 || !held.is_empty() {
        (
            c.tf("project.unsaved.title", &[("name", name.as_str())]),
            prompt_body(c, u, &held),
        )
    } else {
        (c.t("workbook_changes.wb.title"), c.t("workbook_changes.wb.body"))
    };
    let mut act = None;
    // Enter runs the primary through the kit footer; Discard is a click-only danger button
    let resp = crate::ui_kit::modal_dialog("project.unsaved", ctx, |ui| {
        message(ui, th, &title, &body);
        if n > 0 {
            ui.horizontal(|ui| {
                ui.label(c.tf("workbook_changes.wb.line", &[("n", n.to_string().as_str())]));
                if crate::ui_kit::secondary_button(ui, c.t("workbook_changes.wb.review")).clicked() {
                    act = Some(Act::Review);
                }
            });
            ui.add_space(12.0);
        }
        let main_label = match main {
            Act::Save if !held.is_empty() => c.t("drafts.btn.save_all"),
            Act::Save => c.t("project.btn.save"),
            _ => c.t("workbook_changes.wb.review"),
        };
        let discard = c.t("project.btn.discard");
        let cancel = c.t("project.btn.cancel");
        let pick = crate::unsaved::footer(ui, &main_label, &cancel, &discard, |_| {});
        if let Some(ch) = pick {
            act = Some(act_of(main, ch));
        }
    });
    if act.is_none() && resp.should_close() {
        act = Some(Act::Cancel);
    }
    let Some(act) = act else {
        return;
    };
    a.project_ui.prompt = None;
    answer(a, p, act);
}

/// Carries out the answer to the unsaved-changes question about `p`.
fn answer(a: &mut EditorApp, p: Pending, act: Act) {
    match act {
        Act::Review => forget_pending(a),
        Act::Save => {
            // drafts first, so the project save writes them; a refusal keeps all and stays
            if !drafts::save_all(a) {
                if crate::family_editor::asks_overwrite() {
                    // not refused: the family window asks first, `resume_drafts` reads the answer
                    a.project_ui.after_drafts = Some(p);
                    status(a, "drafts_overwrite.drafts.ow_wait");
                } else {
                    forget_pending(a);
                    status(a, "drafts.drafts.refused");
                }
            } else if !unsaved(a).project() {
                // the drafts changed nothing stored: no file to write, the action runs
                drafts::saved();
                proceed(a, p);
            } else {
                a.project_ui.after_save = Some(p);
                a.project_ui.sketches_after_save = true;
                if !start_save(a, None) {
                    forget_pending(a);
                }
            }
        }
        Act::Discard => {
            drafts::discard_all();
            proceed(a, p);
        }
        Act::Cancel => {
            forget_pending(a);
            if a.project_ui.kept_build.is_some() {
                let text = a.sh.cat.t("import_queue.kept");
                let action = a.sh.cat.t("import_queue.open_it");
                a.sh.toasts
                    .push_cmd(Level::Info, text, action, "file.open_kept");
            }
        }
    }
}

/// Save all waits on the family overwrite question: Replace runs Save again (the families are
/// in the model now), Back keeps every edit and drops the action, a refusal keeps its reason.
fn resume_drafts(a: &mut EditorApp) {
    let Some(p) = a.project_ui.after_drafts.take() else {
        return;
    };
    match crate::family_editor::project_answer() {
        None => a.project_ui.after_drafts = Some(p),
        Some(crate::family_editor::Resume::Go) => answer(a, p, Act::Save),
        Some(crate::family_editor::Resume::Back) => {
            forget_pending(a);
            status(a, "drafts_overwrite.drafts.ow_back");
        }
        // the load wrote nothing and its status names the family and the reason
        Some(crate::family_editor::Resume::Refused) => forget_pending(a),
    }
}

/// Prompt text naming what is unsaved: the model, edited drawings, draft editors (`held`, one
/// label each).
fn prompt_body(c: &ddc_ui::Catalog, u: Unsaved, held: &[String]) -> String {
    if u.drawings == 0 && u.agent == 0 && held.is_empty() {
        return c.t("project.unsaved.body");
    }
    let mut lines = vec![];
    if u.model {
        lines.push(c.t("project_unsaved.unsaved.model"));
    }
    if u.drawings > 0 {
        lines.push(c.plural("project_unsaved.unsaved.drawings", u.drawings as u64));
    }
    if !held.is_empty() {
        lines.push(c.t("drafts.drafts.head"));
        lines.extend(held.iter().map(|h| format!("\u{2022} {h}")));
    }
    if u.agent > 0 {
        lines.push(c.tf("agent_draft.unsaved.agent", &[("n", u.agent.to_string().as_str())]));
    }
    lines.join("\n")
}

fn note_no_window(a: &mut EditorApp) {
    let msg = a.sh.cat.t("open_guard.no_window");
    a.sh.toasts.push(Level::Info, msg, None);
}

fn notice_window(a: &mut EditorApp, ctx: &egui::Context) {
    let n = match a.project_ui.notice.take() {
        None => return,
        Some(Notice::SaveFailed(f)) => return save_fail::window(a, ctx, f),
        Some(Notice::Damaged(d)) => return damaged::window(a, ctx, d),
        Some(n) => n,
    };
    let c = &a.sh.cat;
    let th = &a.sh.theme;
    let mut keep = true;
    let mut read_only = None;
    let mut switch = None;
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let resp = egui::Modal::new(egui::Id::new("f1.notice")).show(ctx, |ui| match &n {
        Notice::Locked(path) => {
            let name = file_name(path);
            let body = c.tf("project.locked.body", &[("name", name.as_str())]);
            message(ui, th, &c.t("project.locked.title"), &body);
            right_to_left(ui, |ui| {
                if primary(ui, th, c.t("project.btn.read_only")).clicked() || enter {
                    read_only = Some(path.clone());
                }
                if crate::ui_kit::cancel_button(ui, c.t("project.btn.cancel")).clicked() {
                    keep = false;
                }
                if let Some(pid) = crate::open::window::holder(path) {
                    if crate::ui_kit::secondary_button(ui, c.t("open_guard.switch_window")).clicked() {
                        switch = Some(pid);
                    }
                }
            });
        }
        Notice::Plain { title, body } => {
            message(ui, th, title, body);
            right_to_left(ui, |ui| {
                if primary(ui, th, c.t("project.btn.ok")).clicked() || enter {
                    keep = false;
                }
            });
        }
        // drawn by `save_fail::window` and `damaged::window` above
        Notice::SaveFailed(_) | Notice::Damaged(_) => {}
    });
    if let Some(pid) = switch {
        // the other window is the answer: nothing else to do here
        if !crate::open::window::raise(pid) {
            note_no_window(a);
        }
    } else if let Some(p) = read_only {
        start_open(a, p, true);
    } else if keep && !resp.should_close() {
        a.project_ui.notice = Some(n);
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn project_label(a: &EditorApp) -> String {
    a.ed.project_name()
        .unwrap_or_else(|| a.sh.cat.t("project.untitled"))
}

/// File name and dirty flag for the in-app header; an unsaved project shows the untitled name.
pub fn header_parts(name: Option<String>, untitled: &str, dirty: bool) -> (String, bool) {
    (name.unwrap_or_else(|| untitled.to_string()), dirty)
}

pub fn header_label(a: &EditorApp) -> (String, bool) {
    header_parts(a.ed.project_name(), &a.sh.cat.t("project.untitled"), a.ed.is_dirty())
}

/// `name* [tag] — OpenConstructionDesign`.
pub fn format_title(name: &str, dirty: bool, tag: Option<&str>) -> String {
    let star = if dirty { "*" } else { "" };
    match tag {
        Some(t) => format!("{name}{star} [{t}] — {APP_TITLE}"),
        None => format!("{name}{star} — {APP_TITLE}"),
    }
}

fn current_title(a: &EditorApp) -> String {
    let p = &a.ed.project;
    let tag = if p.read_only() {
        Some(a.sh.cat.t("project.read_only_tag"))
    } else {
        p.recovered_from().map(|_| a.sh.cat.t("project.recovered_tag"))
    };
    format_title(&project_label(a), unsaved(a).project(), tag.as_deref())
}

fn update_title(a: &mut EditorApp, ctx: &egui::Context) {
    // what was sent last frame has reached the window by now
    if a.project_ui.applied != a.project_ui.sent {
        a.project_ui.applied = a.project_ui.sent.clone();
    }
    let title = current_title(a);
    if title != a.project_ui.sent {
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        a.project_ui.sent = title;
    }
}

/// Rows of the File menu, gathered before the menu borrows the app.
pub struct MenuItems {
    rows: Vec<(&'static str, String, String)>,
    export_label: String,
    /// Save as / Export submenu: (id, short format label, full command label as the tip).
    export: Vec<(&'static str, String, String)>,
    recent_label: String,
    recent_empty: String,
    recent: Vec<(String, PathBuf)>,
}

/// Title-bar menu row of command `id`: (label from `label_key`, key text from the keymap, the
/// same source as tooltips and the palette, so a rebound key shows here too).
pub fn menu_row(a: &EditorApp, id: &str, label_key: &str) -> (String, String) {
    (a.sh.cat.t(label_key), crate::quick::keys_text(a, id))
}

pub fn menu_items(a: &EditorApp) -> MenuItems {
    let c = &a.sh.cat;
    let shortcut = |id: &str| crate::quick::keys_text(a, id);
    // one IFC row: it asks for a path the first time; the asking variant stays in the palette
    let rows = [
        "file.new",
        "file.open",
        "file.save",
        "file.save_as",
        "file.save_ifc",
    ]
    .into_iter()
    .map(|id| (id, c.t(&format!("cmd.{id}")), shortcut(id)))
    .collect();
    MenuItems {
        rows,
        export_label: c.t("fmenu.export"),
        export: export_rows(c),
        recent_label: c.t("project.menu.recent"),
        recent_empty: c.t("project.recent.empty"),
        recent: a
            .prefs
            .recent_projects
            .iter()
            .map(|s| {
                let p = PathBuf::from(s);
                (file_name(&p), p)
            })
            .collect(),
    }
}

/// Save as / Export: every format the project leaves in, by its format name. Models leave as
/// IFC, glTF binary, STL, OBJ, exact STEP or 3MF; sheets as PDF or SVG (the print dialog).
pub const EXPORT_ROWS: [(&str, &str, &str); 8] = [
    ("data.export_ifc4x3", "fmenu.export.ifc4x3", "ifc-out-4x3"),
    ("data.export_ifc2x3", "fmenu.export.ifc2x3", "ifc-out-2x3"),
    ("data.export_gltf", "fmenu.export.glb", "gltf-out"),
    ("data.export_stl", "fmenu.export.stl", "gltf-out"),
    ("data.export_obj", "fmenu.export.obj", "gltf-out"),
    ("data.export_step", "fmenu.export.step", "gltf-out"),
    ("data.export_3mf", "fmenu.export.3mf", "gltf-out"),
    ("data.export_pdf", "fmenu.export.pdf", "pdf-out"),
];

fn export_rows(c: &ddc_ui::Catalog) -> Vec<(&'static str, String, String)> {
    EXPORT_ROWS
        .iter()
        .map(|(id, key, _)| (*id, c.t(key), c.t(&format!("cmd.{id}"))))
        .collect()
}

fn export_icon(id: &str) -> &'static str {
    EXPORT_ROWS
        .iter()
        .find(|r| r.0 == id)
        .map_or("save-as", |r| r.2)
}

pub fn file_menu(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    m: &MenuItems,
    st: &mut State,
    fire: &mut Vec<String>,
) {
    for (id, label, keys) in &m.rows {
        let icon = match *id {
            "file.new" => "file-new",
            "file.open" => "open",
            "file.save" | "file.save_as" => {
                if *id == "file.save" {
                    "save"
                } else {
                    "save-as"
                }
            }
            _ => "ifc-out",
        };
        if ddc_ui::widgets::menu_item_icon(
            ui,
            th,
            Some(icon),
            label,
            keys,
            ddc_ui::widgets::MenuTone::Normal,
            true,
        )
        .clicked()
        {
            fire.push((*id).into());
            ui.close();
        }
    }
    ddc_ui::widgets::menu_sub(ui, th, Some("save-as"), &m.export_label, true, |ui| {
        for (id, label, tip) in &m.export {
            let row = ddc_ui::widgets::menu_item_icon(
                ui,
                th,
                Some(export_icon(id)),
                label,
                "",
                ddc_ui::widgets::MenuTone::Normal,
                true,
            )
            .on_hover_text(tip.as_str());
            if row.clicked() {
                fire.push((*id).into());
                ui.close();
            }
        }
    });
    ddc_ui::widgets::menu_sub(ui, th, Some("history"), &m.recent_label, true, |ui| {
        if m.recent.is_empty() {
            ui.weak(m.recent_empty.as_str());
        }
        for (name, path) in &m.recent {
            let row = ddc_ui::widgets::menu_item_icon(
                ui,
                th,
                None,
                name,
                "",
                ddc_ui::widgets::MenuTone::Normal,
                true,
            )
            .on_hover_text(path.display().to_string());
            if row.clicked() {
                st.open_request = Some(path.clone());
                ui.close();
            }
        }
    });
}

/// Kit row for a (label, key text) pair of the title-bar menus.
pub fn kit_row(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    icon: Option<&str>,
    (label, keys): &(String, String),
) -> egui::Response {
    ddc_ui::widgets::menu_item_icon(
        ui,
        th,
        icon,
        label,
        keys,
        ddc_ui::widgets::MenuTone::Normal,
        true,
    )
}

/// Body of the closure `name` in `src` (text between its braces), for source checks.
#[cfg(test)]
fn closure_body<'a>(src: &'a str, start_marker: &str) -> &'a str {
    let start = src.find(start_marker).expect("marker");
    let open = start + src[start..].find('{').unwrap();
    let mut depth = 0usize;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced")
}

#[cfg(test)]
mod ux57a_tests {
    use super::*;
    use ddc_ui::{Catalog, Density, Lang, Mode};

    const PANELS: &str = include_str!("shell/panels.rs");
    const SELF_SRC: &str = include_str!("project_ui.rs");
    const FILE_POPUP: &str = "egui::Popup::menu(&file_resp).show(|ui|";

    const FILE_IDS: [&str; 5] = [
        "file.new",
        "file.open",
        "file.save",
        "file.save_as",
        "file.save_ifc",
    ];

    fn items(lang: Lang) -> MenuItems {
        let c = Catalog::new(lang);
        MenuItems {
            rows: FILE_IDS
                .into_iter()
                .map(|id| (id, c.t_in(lang, &format!("cmd.{id}")), "Ctrl+S".to_string()))
                .collect(),
            export_label: c.t_in(lang, "fmenu.export"),
            export: export_rows(&c),
            recent_label: c.t_in(lang, "project.menu.recent"),
            recent_empty: c.t_in(lang, "project.recent.empty"),
            recent: vec![("a.ddc".into(), PathBuf::from("a.ddc"))],
        }
    }

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    #[test]
    fn menus_have_no_raw_rows() {
        for body in [
            closure_body(PANELS, FILE_POPUP),
            closure_body(SELF_SRC, "pub fn file_menu("),
        ] {
            for raw in [".button(", "egui::Button", "menu_button(", "ui.separator("] {
                assert!(!body.contains(raw), "raw row {raw}");
            }
        }
    }

    #[test]
    fn commands_keep_their_ids_and_order() {
        let file = closure_body(PANELS, FILE_POPUP);
        let at = |s: &str| file.find(s).unwrap_or_else(|| panic!("{s}"));
        assert!(at("\"file.import\".into()") < at("\"mgr.settings\".into()"));
        assert!(at("\"mgr.settings\".into()") < at("\"ui.start\".into()"));
        let body = closure_body(SELF_SRC, "pub fn file_menu(");
        assert!(body.contains("fire.push((*id).into())"));
        let m = items(Lang::En);
        let ids: Vec<_> = m.rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, FILE_IDS);
    }

    /// The real File menu: these rows in this order, and no label twice (an ellipsis is no
    /// difference a reader sees: "Save copy as IFC" was listed twice once).
    #[test]
    fn file_menu_lists_each_command_once() {
        let a = EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "t".into());
        let m = menu_items(&a);
        let ids: Vec<_> = m.rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, FILE_IDS);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut seen: Vec<String> = Vec::new();
            for id in FILE_IDS {
                let l = c.t_in(lang, &format!("cmd.{id}"));
                let l = l.trim_end_matches('\u{2026}').trim_end_matches("...").to_string();
                assert!(!seen.contains(&l), "{lang:?}: {l} twice");
                seen.push(l);
            }
        }
    }

    /// File > Save as / Export lists IFC 4.3, IFC 2x3, glTF binary, STL, OBJ and PDF, each a registered
    /// command with a handler, labelled in both languages.
    #[test]
    fn file_menu_has_save_as_export() {
        let a = EditorApp::new(crate::editor::Editor::new_in_memory().unwrap(), None, "t".into());
        let m = menu_items(&a);
        let ids: Vec<_> = m.export.iter().map(|r| r.0).collect();
        assert_eq!(
            ids,
            [
                "data.export_ifc4x3",
                "data.export_ifc2x3",
                "data.export_gltf",
                "data.export_stl",
                "data.export_obj",
                "data.export_step",
                "data.export_3mf",
                "data.export_pdf",
            ]
        );
        for id in &ids {
            assert!(a.handlers.contains_key(*id), "{id} has no handler");
        }
        let body = closure_body(SELF_SRC, "pub fn file_menu(");
        assert!(body.contains("&m.export_label") && body.contains("in &m.export"));
        let reg = ddc_ui::Registry::builtin();
        for id in ["file.save_native_new", "file.save_source", "data.export_dxf"] {
            assert!(reg.get(id).is_none(), "{id} is not in the public build");
        }
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut keys = vec!["fmenu.export".to_string()];
            keys.extend(EXPORT_ROWS.iter().map(|r| r.1.to_string()));
            for k in keys {
                let v = c.t_in(lang, &k);
                assert!(v != k && !v.is_empty(), "{lang:?} {k}");
            }
            assert!(c.t_in(lang, "fmenu.export.glb").contains(".glb"));
        }
    }

    /// The title bar has no Data menu: its items are commands with handlers, on the Data and
    /// Manage ribbon tabs, in the File menu (XLSX rows) and in the palette.
    #[test]
    fn data_items_are_commands_not_a_second_menu() {
        let title = closure_body(PANELS, "pub(super) fn title_bar(");
        assert!(!title.contains("menu_button("));
        assert!(!title.contains("xlsx_export()"));
        let reg = ddc_ui::Registry::builtin();
        for id in [
            "data.export_xlsx",
            "data.import_xlsx",
            "data.sync_xlsx",
            "mgr.project_params",
            "mgr.phases",
        ] {
            assert!(reg.get(id).is_some(), "{id}");
        }
        assert!(include_str!("shell/xlsx_win.rs").contains("\"data.sync_xlsx\""));
    }

    /// XLSX is the main workflow: export, import and sync stay labelled rows of the File menu
    /// (sync is also a ribbon button), in this order, between the file rows and Import.
    #[test]
    fn file_menu_keeps_the_xlsx_rows() {
        let ids = ["data.export_xlsx", "data.import_xlsx", "data.sync_xlsx"];
        let start = PANELS.find("const XLSX_ROWS").expect("XLSX_ROWS");
        let rows = &PANELS[start..start + PANELS[start..].find("];").unwrap()];
        let at = |s: &str| rows.find(s).unwrap_or_else(|| panic!("{s} not in XLSX_ROWS"));
        assert!(at(ids[0]) < at(ids[1]) && at(ids[1]) < at(ids[2]));
        let file = closure_body(PANELS, FILE_POPUP);
        let loop_at = file.find("in &xlsx").expect("the File menu draws the XLSX rows");
        assert!(file.find("file_menu(").unwrap() < loop_at);
        assert!(loop_at < file.find("\"file.import\".into()").unwrap());
        assert!(file.contains("fire.push((*id).into())"));
        let reg = ddc_ui::Registry::builtin();
        assert!(reg.get(ids[2]).expect("sync").on("R"), "sync has no ribbon button");
        // each row carries its description as a tooltip (the old Sync tip was lost once)
        assert!(file.contains("rich_tooltip(resp, &th, &r.0, desc"));
        for id in ids {
            for lang in [Lang::En, Lang::Ru] {
                let k = format!("cmd.{id}.desc");
                let c = Catalog::new(lang);
                assert!(c.t(&k) != k && !c.t(&k).is_empty(), "{lang:?} {k}");
            }
        }
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        for id in ids {
            let k = format!("cmd.{id}");
            let (e, r) = (en.t(&k), ru.t(&k));
            assert!(e != k && r != k && !e.is_empty() && !r.is_empty(), "{k}");
            assert_ne!(e, r, "{k} not translated");
            assert!(e.contains("XLSX") && r.contains("XLSX"), "{k}: {e} / {r}");
        }
    }

    #[test]
    fn rows_align_in_both_languages() {
        let th = Theme::new(Mode::Light, Density::Comfortable);
        for lang in [Lang::En, Lang::Ru] {
            let m = items(lang);
            let ctx = ctx();
            // rows + Save as/Export + Recent submenu rows, all of one height
            let mut got = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for _ in 0..3 {
                let mut st = State::default();
                let mut fire = Vec::new();
                let _ = ctx.run(Default::default(), |c| {
                    egui::CentralPanel::default().show(c, |ui| {
                        let top = ui.cursor().top();
                        file_menu(ui, &th, &m, &mut st, &mut fire);
                        let mid = ui.cursor().top();
                        let probe = (String::from("Import"), String::from("Ctrl+I"));
                        let r = kit_row(ui, &th, Some("open"), &probe).rect;
                        got = (mid - top, r.height(), ui.spacing().item_spacing.y, r.left());
                    });
                });
                assert!(fire.is_empty());
            }
            let (total, h, sp, _) = got;
            let n = (m.rows.len() + 2) as f32;
            assert!((total - (n * h + n * sp)).abs() < 0.6, "{lang:?} {total} {h} {sp}");
        }
    }

    #[test]
    fn file_menu_rows_share_height_and_left_edge() {
        let th = Theme::new(Mode::Dark, Density::Comfortable);
        let ctx = ctx();
        let mut got = Vec::new();
        for _ in 0..3 {
            got.clear();
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    for (i, icon) in [Some("open"), None, Some("no-such-icon")].into_iter().enumerate() {
                        let pair = (format!("Row {i}"), "Ctrl+K".to_string());
                        got.push(kit_row(ui, &th, icon, &pair).rect);
                    }
                });
            });
        }
        assert!(got.iter().all(|r| r.height() == got[0].height() && r.left() == got[0].left()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, OpFn};
    use ddc_model::Model;
    use ddc_ui::{Catalog, Lang, Registry};

    #[test]
    fn prompt_discard_only_from_the_danger_button() {
        use crate::unsaved::Choice;
        for main in [Act::Save, Act::Review] {
            assert_eq!(act_of(main, Choice::Apply), main, "the primary button and Enter");
            assert_ne!(act_of(main, Choice::Apply), Act::Discard);
            assert_eq!(act_of(main, Choice::Cancel), Act::Cancel);
            assert_eq!(act_of(main, Choice::Discard), Act::Discard);
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-ui-f1-{tag}-{}", std::process::id()));
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

    fn add_level(a: &mut EditorApp, name: &str, z: f64) {
        let name = name.to_string();
        let op = OpFn(Arc::new(move |m: &mut Model| {
            m.add_level(&name, z).map(|_| ())
        }));
        a.ed.run_op(op).unwrap();
    }

    /// Title-bar File/Data rows carry their keys from the keymap.
    #[test]
    fn menu_rows_show_keys() {
        let (mut a, _d) = app("menukeys");
        for built in [false, true] {
            if built {
                crate::quick::rebuild_keys(&mut a);
            }
            let (label, keys) = menu_row(&a, "file.import", "menu.import");
            assert!(!label.is_empty() && label != "menu.import", "{label}");
            assert!(keys.contains("Ctrl") && keys.ends_with('I'), "{built}: {keys}");
            let (_, keys) = menu_row(&a, "mgr.settings", "menu.settings");
            assert!(keys.starts_with("Ctrl+"), "{built}: {keys}");
            let rows = menu_items(&a).rows;
            assert!(
                rows.iter().any(|(id, _, k)| *id == "file.save" && !k.is_empty()),
                "{built}: {rows:?}"
            );
        }
    }

    #[test]
    fn save_refused_while_big_job_holds_the_model() {
        let (mut a, d) = app("bigbusy");
        crate::bigfile::start_demo(&mut a);
        let target = d.join("x.ddc");
        assert!(!start_save(&mut a, Some(target.clone())));
        assert!(!target.exists());
        assert!(a.big.busy());
    }

    fn has_level(a: &EditorApp, name: &str) -> bool {
        a.ed.model.level_list().unwrap().iter().any(|l| l.1 == name)
    }

    fn wait_for_job(a: &mut EditorApp) {
        for _ in 0..2000 {
            if !a.project_ui.busy() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            poll_job(a);
        }
        panic!("job did not finish");
    }

    fn saved_project(a: &mut EditorApp, d: &Path, name: &str) -> PathBuf {
        let p = d.join(name);
        assert!(start_save(a, Some(p.clone())));
        wait_for_job(a);
        p
    }

    #[test]
    fn request_build_busy_returns_false() {
        let (mut a, _d) = app("buildbusy");
        crate::bigfile::start_demo(&mut a);
        assert!(a.big.busy());
        assert!(!request_build(&mut a, PathBuf::from("x.ddc")));
        assert!(a.project_ui.prompt.is_none());
        a.big.cancel_job();
    }

    #[test]
    fn the_sample_asks_first_when_there_is_something_to_lose() {
        let (mut a, d) = app("sample");
        // a level is no wall, slab, roof, comp, curtain or room: the old check called this empty
        add_level(&mut a, "mine", 3.0);
        crate::help::open_sample(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::Sample));
        assert!(has_level(&a, "mine"), "nothing replaced before the answer");
        assert!(
            a.ed.project.work_path().is_some_and(Path::exists),
            "working copy alive"
        );
        // Cancel keeps it all
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        assert!(has_level(&a, "mine"));
        // Discard: an explicit answer
        proceed(&mut a, Pending::Sample);
        assert!(!has_level(&a, "mine"));
        assert!(a.ed.walls.len() > 0);
        assert!(a.ed.project.path().is_none());
        assert!(!a.ed.is_dirty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_sample_needs_no_question_over_nothing() {
        let (mut a, d) = app("sample0");
        crate::help::open_sample(&mut a);
        assert!(a.project_ui.prompt.is_none());
        assert!(a.ed.walls.len() > 0);
        let _ = std::fs::remove_dir_all(d);
    }

    /// The sample during a background save: refused, so the save cannot bind it to the file.
    #[test]
    fn the_sample_cannot_get_between_a_save_and_its_file() {
        let (mut a, d) = app("race");
        let p = saved_project(&mut a, &d, "project.ddc");
        add_level(&mut a, "mine", 3.0);
        assert!(start_save(&mut a, None));
        crate::help::open_sample(&mut a);
        assert!(has_level(&a, "mine"), "refused while the save runs");
        assert!(a.project_ui.prompt.is_none());
        wait_for_job(&mut a);
        assert_eq!(a.ed.project.path(), Some(p.as_path()));
        assert!(!a.ed.is_dirty());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Even if something replaces the model behind the guard, the finished save binds nothing:
    /// Ctrl+S cannot write the new model over the project.
    #[test]
    fn a_save_that_outlives_its_model_binds_nothing() {
        let (mut a, d) = app("outlive");
        let p = saved_project(&mut a, &d, "project.ddc");
        add_level(&mut a, "mine", 3.0);
        assert!(start_save(&mut a, None));
        a.ed.replace_model(crate::help::sample::build(true).unwrap())
            .unwrap();
        wait_for_job(&mut a);
        assert!(a.ed.project.path().is_none(), "the sample has no file");
        assert!(ddc_model::probe_project(&p).is_ok(), "the project is whole");
        let _ = std::fs::remove_dir_all(d);
    }

    /// Settings that cannot be written are told once a session, in the UI language.
    #[test]
    fn unwritable_settings_give_one_toast_once() {
        let (mut a, d) = app("sv18prefs");
        let blocker = d.join("not-a-folder");
        std::fs::write(&blocker, b"x").unwrap();
        a.prefs.path = Some(blocker.join("prefs.txt"));
        let warnings = |a: &EditorApp| {
            a.sh.toasts.live.iter().filter(|t| t.level == Level::Warning).count()
        };
        let before = warnings(&a);
        for _ in 0..3 {
            a.prefs.touch_project("C:/x.ddc");
            a.prefs.save();
            tell_settings_failure(&mut a);
        }
        assert!(a.prefs.write_fail.current().is_some());
        assert_eq!(warnings(&a), before + 1, "one toast, not one per write");
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let t = c.tf("project.prefs_not_saved", &[("why", "x")]);
            assert!(t.contains('x') && !t.contains("{why}"), "{lang:?}: {t}");
        }
        a.prefs.path = None;
        let _ = std::fs::remove_dir_all(d);
    }

    /// An edit made while the save ran is in neither the file nor a snapshot: still owed.
    #[test]
    fn an_edit_during_the_save_stays_due_for_autosave() {
        let (mut a, d) = app("sv16during");
        saved_project(&mut a, &d, "project.ddc");
        add_level(&mut a, "one", 3.0);
        assert!(start_save(&mut a, None));
        add_level(&mut a, "two", 6.0);
        a.autosave.note_side_edit();
        wait_for_job(&mut a);
        assert!(a.ed.is_dirty(), "the file lacks the second level");
        assert!(a.autosave.dirty(a.ed.revision), "the snapshot still owes it");
        assert!(a.autosave.changes(a.ed.revision) >= 2, "model and drawing edit");
        let _ = std::fs::remove_dir_all(d);
    }

    /// Edits made while a damaged file failed to open are asked about before its kept copy
    /// replaces the model.
    #[test]
    fn open_prev_after_a_damaged_open_asks_about_edits_made_meanwhile() {
        let (mut a, d) = app("r108dmg");
        let bad = d.join("bad.ddc");
        std::fs::write(&bad, b"not a database").unwrap();
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        Editor::new_in_memory().unwrap().save_project_as(&bak.join("bad.ddc.prev")).unwrap();
        start_open(&mut a, bad.clone(), false);
        add_level(&mut a, "typed meanwhile", 9.0);
        wait_for_job(&mut a);
        let Some(Notice::Damaged(dm)) = a.project_ui.notice.take() else {
            panic!("the damaged-file dialog");
        };
        assert!(dm.since.is_some());
        damaged::run(&mut a, damaged::Act::OpenPrev, dm);
        wait_for_job(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::Adopt), "asked, not replaced");
        assert!(has_level(&a, "typed meanwhile"));
        assert_eq!(std::fs::read(&bad).unwrap(), b"not a database");
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        let _ = std::fs::remove_dir_all(d);
    }

    /// A damaged file with a `.prev` and one snapshot of its own; returns (file, prev, snap).
    fn damaged_with_copies(a: &mut EditorApp, d: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let bad = d.join("bad.ddc");
        std::fs::write(&bad, b"not a database").unwrap();
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        let prev = bak.join("bad.ddc.prev");
        Editor::new_in_memory().unwrap().save_project_as(&prev).unwrap();
        let auto = a.prefs.autosave_folder().unwrap();
        let family = auto.join(crate::shell::recover_win::autosave_name(Some(&bad)));
        let snap = a.ed.model.autosave_as(&family, 3).unwrap();
        (bad, prev, snap)
    }

    fn file_state(p: &Path) -> (Vec<u8>, web_time::SystemTime) {
        let at = std::fs::metadata(p).and_then(|m| m.modified()).unwrap();
        (std::fs::read(p).unwrap(), at)
    }

    /// Restored from the damaged-file dialog: bound to no file, Save asks for a name beside it,
    /// the title says so (en/ru); Save As binds the new file and the damaged one stays as is.
    #[test]
    fn restore_from_damaged_asks_for_a_name() {
        let (mut a, d) = app("r108a");
        let (bad, prev, _) = damaged_with_copies(&mut a, &d);
        let (bad0, prev0) = (file_state(&bad), file_state(&prev));
        let dm = damaged::find(&bad, "x".into(), a.prefs.autosave_folder().as_deref());
        damaged::run(&mut a, damaged::Act::Snapshots, dm);
        let r = a.recover.take().expect("the snapshots of the damaged file");
        assert!(r.damaged);
        let snap = crate::shell::recover_rows::restore_target(&r.snaps, r.pick)
            .unwrap()
            .to_path_buf();
        request_restore(&mut a, r, snap);
        if let Some(p) = a.project_ui.prompt.take() {
            proceed(&mut a, p);
        }
        assert!(a.ed.project.path().is_none(), "not bound to the damaged file");
        assert_eq!(a.ed.project.recovered_from(), Some(bad.as_path()));
        assert!(a.ed.is_dirty());
        assert_eq!(bound_target(&a), None, "a plain Save asks for a name");
        // an imported model: Ctrl+S would save beside its source; the restored copy still asks
        let src = d.join("House.ifc").display().to_string();
        a.ed.model
            .sql()
            .execute(
                "INSERT INTO src_file(source,file_key,path,format,reader,reader_version,\
                 first_import,imported_at) VALUES ('ifc:x','k',?1,'ifc','r','1',100,100)",
                [src],
            )
            .unwrap();
        assert_eq!(crate::open::first_save_target(&a), None, "no silent name for the copy");
        for lang in [Lang::En, Lang::Ru] {
            a.sh.cat = Catalog::new(lang);
            let (name, dir) = save_as_seed(&a);
            let base = a.sh.cat.tf("project.recovered_name", &[("name", "bad")]);
            assert_eq!(name, format!("{base}.ddc"), "{lang:?}");
            assert!(name != "bad.ddc" && !name.contains("{name}"), "{lang:?}: {name}");
            assert_eq!(dir.as_deref(), Some(d.as_path()), "the same folder");
            let tag = a.sh.cat.t("project.recovered_tag");
            assert_ne!(tag, "project.recovered_tag");
            assert!(current_title(&a).contains(&format!("[{tag}]")), "{lang:?}");
            assert_ne!(a.sh.cat.t("project.recovered_toast"), "project.recovered_toast");
        }
        let en = Catalog::new(Lang::En).t("project.recovered_tag");
        assert_ne!(en, Catalog::new(Lang::Ru).t("project.recovered_tag"));
        a.sh.cat = Catalog::new(Lang::En);
        let (name, dir) = save_as_seed(&a);
        let target = dir.unwrap().join(name);
        assert!(start_save(&mut a, Some(target.clone())));
        wait_for_job(&mut a);
        assert_eq!(a.ed.project.path(), Some(target.as_path()), "bound to the new file");
        assert!(a.ed.project.recovered_from().is_none());
        assert!(!current_title(&a).contains(&en));
        add_level(&mut a, "after", 3.0);
        assert!(start_save(&mut a, None), "the next Save goes to the new file");
        wait_for_job(&mut a);
        assert!(!a.ed.is_dirty());
        assert_eq!(file_state(&bad), bad0, "the damaged file is untouched");
        assert_eq!(file_state(&prev), prev0, "the .prev is neither written nor rotated");
        let _ = std::fs::remove_dir_all(d);
    }

    /// The kept copy opened from the damaged-file dialog is unbound too: Save never writes
    /// into `_ddc_backup` or over the damaged file.
    #[test]
    fn open_prev_from_damaged_binds_no_file() {
        let (mut a, d) = app("r108aprev");
        let (bad, prev, _) = damaged_with_copies(&mut a, &d);
        let prev0 = file_state(&prev);
        let dm = damaged::find(&bad, "x".into(), None);
        damaged::run(&mut a, damaged::Act::OpenPrev, dm);
        wait_for_job(&mut a);
        assert!(a.ed.project.path().is_none() && !a.ed.project.read_only());
        assert_eq!(a.ed.project.recovered_from(), Some(bad.as_path()));
        assert_eq!(bound_target(&a), None);
        assert_eq!(save_as_seed(&a).1.as_deref(), Some(d.as_path()), "not `_ddc_backup`");
        assert!(a.ed.save_project().is_err());
        assert_eq!(file_state(&prev), prev0);
        assert_eq!(std::fs::read(&bad).unwrap(), b"not a database");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn restore_asks_about_unsaved_edits_and_the_dialog_comes_back_on_cancel() {
        let (mut a, d) = app("restore");
        let auto = d.join("auto");
        let snap =
            a.ed.model
                .autosave_as(&auto.join(crate::shell::recover_win::UNTITLED), 3)
                .unwrap();
        crate::shell::recover_win::mark_undecided(&auto, None).unwrap();
        add_level(&mut a, "mine", 3.0);
        let r = crate::shell::recover_win::scan_for(&auto, true, None).unwrap();
        request_restore(&mut a, r, snap.clone());
        assert!(matches!(a.project_ui.prompt, Some(Pending::Restore { .. })));
        assert!(has_level(&a, "mine"));
        assert!(a.recover.is_none());
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        assert!(a.recover.is_some(), "the choice is still there");
        assert!(has_level(&a, "mine"));
        // the answer «don't save»: restored, modified, the offer settled
        let r = a.recover.take().unwrap();
        request_restore(&mut a, r, snap.clone());
        let p = a.project_ui.prompt.take().unwrap();
        proceed(&mut a, p);
        assert!(!has_level(&a, "mine"));
        assert!(
            a.ed.is_dirty(),
            "a restored state is not what any file holds"
        );
        assert!(crate::shell::recover_win::undecided(&auto).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Edits made under a running Open: the finished Open waits for an answer, it does not
    /// replace them silently.
    #[test]
    fn edits_made_during_an_open_are_asked_about() {
        let (mut a, d) = app("during");
        let other = saved_project(&mut a, &d, "other.ddc");
        let mut second = Editor::start_project().unwrap();
        let p = d.join("second.ddc");
        second.save_project_as(&p).unwrap();
        drop(second);
        start_open(&mut a, p.clone(), false);
        assert!(a.project_ui.busy());
        add_level(&mut a, "typed meanwhile", 9.0);
        wait_for_job(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::Adopt));
        assert!(
            has_level(&a, "typed meanwhile"),
            "still here until the answer"
        );
        assert_eq!(a.ed.project.path(), Some(other.as_path()));
        // Cancel: the edits stay and the opened copy goes with its lock
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        assert!(has_level(&a, "typed meanwhile"));
        assert!(a.project_ui.opened.is_none());
        drop(ProjectLock::acquire(&p).unwrap());
        // once more, and the answer «don't save» takes the project
        start_open(&mut a, p.clone(), false);
        add_level(&mut a, "typed again", 12.0);
        wait_for_job(&mut a);
        let q = a.project_ui.prompt.take().unwrap();
        proceed(&mut a, q);
        assert_eq!(a.ed.project.path(), Some(p.as_path()));
        assert!(!has_level(&a, "typed again"));
        let _ = std::fs::remove_dir_all(d);
    }

    /// Drawings edited while a project opens are asked about, not dropped.
    #[test]
    fn drawing_edits_during_an_open_are_asked_about() {
        let (mut a, d) = app("drawopen");
        let other = saved_project(&mut a, &d, "other.ddc");
        let mut second = Editor::start_project().unwrap();
        let p = d.join("second.ddc");
        second.save_project_as(&p).unwrap();
        drop(second);
        with_drawing(&mut a, false);
        start_open(&mut a, p.clone(), false);
        a.plan.drawings.list[0].mark_edited();
        wait_for_job(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::Adopt));
        assert_eq!(a.plan.drawings.list.len(), 1, "kept until the answer");
        assert_eq!(a.ed.project.path(), Some(other.as_path()));
        // Cancel: old project and its edited drawing stay
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        assert!(a.project_ui.opened.is_none());
        assert_eq!(a.plan.drawings.unsaved_count(), 1);
        assert_eq!(a.ed.project.path(), Some(other.as_path()));
        let _ = std::fs::remove_dir_all(d);
    }

    /// No edit after the answer: the adopt resets the drawings without a second question.
    #[test]
    fn no_drawing_edit_during_an_open_adopts_and_resets() {
        let (mut a, d) = app("drawopen2");
        let _ = saved_project(&mut a, &d, "other.ddc");
        let mut second = Editor::start_project().unwrap();
        let p = d.join("second.ddc");
        second.save_project_as(&p).unwrap();
        drop(second);
        with_drawing(&mut a, true);
        start_open(&mut a, p.clone(), false);
        wait_for_job(&mut a);
        assert!(a.project_ui.prompt.is_none());
        assert_eq!(a.ed.project.path(), Some(p.as_path()));
        assert!(a.plan.drawings.list.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    /// A finished build asks at adopt time, so edited drawings are not dropped.
    #[test]
    fn a_finished_build_asks_about_edited_drawings() {
        let (mut a, d) = app("drawbuild");
        with_drawing(&mut a, true);
        guard(&mut a, Pending::AdoptBuilt);
        assert_eq!(a.project_ui.prompt, Some(Pending::AdoptBuilt));
        assert_eq!(a.plan.drawings.list.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    /// «Don't save» came before the Open: with no edits after it, no second question.
    #[test]
    fn an_open_after_discard_does_not_ask_twice() {
        let (mut a, d) = app("once");
        let p = saved_project(&mut a, &d, "a.ddc");
        add_level(&mut a, "dropped", 3.0);
        assert!(a.ed.is_dirty());
        start_open(&mut a, p.clone(), false);
        wait_for_job(&mut a);
        assert!(a.project_ui.prompt.is_none());
        assert!(!has_level(&a, "dropped"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_project_open_reports_copy_then_check_up_to_one() {
        let (mut a, d) = app("openstages");
        let p = saved_project(&mut a, &d, "a.ddc");
        start_open(&mut a, p.clone(), false);
        let v = open_view(&a).expect("the loading window has the open at once");
        assert_eq!(v.file.as_deref(), Some("a.ddc"));
        assert_eq!(v.size, std::fs::metadata(&p).ok().map(|m| m.len()));
        let ctl = match &a.project_ui.job {
            Some(Running::Open(j)) => j.job.ctl.clone(),
            _ => panic!("open job"),
        };
        wait_for_job(&mut a);
        assert_eq!(ctl.trail(), STAGES_OPEN.to_vec());
        assert!((ctl.fraction() - 1.0).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn an_import_cannot_start_during_an_open() {
        let (mut a, d) = app("import");
        let p = saved_project(&mut a, &d, "a.ddc");
        start_open(&mut a, p, false);
        crate::bigfile::start(&mut a, d.join("model.ifc"));
        assert!(!a.big.busy());
        wait_for_job(&mut a);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_ready_result_beats_a_late_cancel_only_where_asked() {
        let wait = |c: &Arc<Progress>| -> Result<(), String> {
            while !c.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        };
        let mut j = Job::spawn("project.job.save", wait);
        j.cancel();
        let done = loop {
            match j.poll_finished() {
                Poll::Running => std::thread::sleep(Duration::from_millis(1)),
                p => break p,
            }
        };
        assert!(
            matches!(done, Poll::Done(())),
            "the file is replaced: say so"
        );
        let mut j = Job::spawn("project.job.save", wait);
        j.cancel();
        let done = loop {
            match j.poll() {
                Poll::Running => std::thread::sleep(Duration::from_millis(1)),
                p => break p,
            }
        };
        assert!(matches!(done, Poll::Cancelled));
    }

    #[test]
    fn kept_working_copies_are_told_about() {
        let (mut a, d) = app("kept");
        notify_kept(&mut a, &[d.join("_orphans").join("1.ddc")]);
        let Some(Notice::Plain { body, .. }) = &a.project_ui.notice else {
            panic!("no message");
        };
        assert!(body.contains("_orphans"), "{body}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn stranded_note_names_kept_folder() {
        let (mut a, d) = app("stranded");
        let kept = d.join("_orphans").join("1.ddc");
        let side = d.join("work").join("3.ddc-wal");
        notify_stranded(&mut a, &[kept], &[side]);
        let Some(Notice::Plain { body, .. }) = &a.project_ui.notice else {
            panic!("no message");
        };
        assert!(body.contains("3.ddc-wal"), "{body}");
        assert!(body.contains("_orphans"), "{body}");
        assert!(!body.contains("{folder}"), "{body}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn header_shows_file_name_or_untitled_and_dirty_flag() {
        assert_eq!(header_parts(Some("a.ddc".into()), "New", true), ("a.ddc".into(), true));
        assert_eq!(header_parts(None, "New", false), ("New".into(), false));
    }

    #[test]
    fn title_shows_name_flag_and_tag() {
        assert_eq!(format_title("a.ddc", false, None), "a.ddc — OpenConstructionDesign");
        assert_eq!(format_title("a.ddc", true, None), "a.ddc* — OpenConstructionDesign");
        assert_eq!(
            format_title("a.ddc", true, Some("read-only")),
            "a.ddc* [read-only] — OpenConstructionDesign"
        );
    }

    #[test]
    fn cancel_on_unsaved_prompt_keeps_built() {
        let mut built = Some("import");
        let mut kept = None;
        park(&mut built, &mut kept);
        assert_eq!((built, kept), (None, Some("import")));
        // nothing held: an earlier parked result stays
        let mut none: Option<&str> = None;
        park(&mut none, &mut kept);
        assert_eq!(kept, Some("import"));
    }

    #[test]
    fn question_before_the_build_is_not_asked_twice() {
        // dirty and never asked: ask
        assert!(needs_ask(true, true, None, 7));
        // asked at the start, nothing edited since: adopt without a second question
        assert!(!needs_ask(true, true, Some(7), 7));
        // edited during the build: ask again
        assert!(needs_ask(true, true, Some(7), 8));
        // the answer belongs to builds only
        assert!(needs_ask(true, false, Some(7), 7));
        assert!(!needs_ask(false, true, None, 7));
    }

    #[test]
    fn modal_only_while_a_window_is_up() {
        let mut s = State::default();
        assert!(!s.modal());
        assert_eq!(s.applied_title(), "OpenConstructionDesign");
        s.prompt = Some(Pending::Close);
        assert!(s.modal());
    }

    fn pending_book(a: &mut EditorApp, d: &Path, synced: bool) {
        let mut pv = crate::shell::xlsx_win::XlsxPreview::linked(&d.join("book.xlsx"), false);
        pv.synced = synced;
        if !synced {
            pv.report.changes.push(ddc_io::xlsx::Change {
                sheet: "s".into(),
                row: 2,
                col: 3,
                column: "c".into(),
                id: Some(1),
                old: "a".into(),
                new: "b".into(),
            });
        }
        a.xlsx = Some(pv);
    }

    /// Runs one frame with a window-close request; true when the close was cancelled.
    fn close_is_cancelled(a: &mut EditorApp) -> bool {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let out = ctx.run(raw, |ctx| handle_close(a, ctx));
        out.viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::CancelClose))
            })
    }

    #[test]
    fn no_pending_workbook_adds_no_line() {
        let (mut a, d) = app("wb-none");
        assert_eq!(workbook_pending(&a), 0);
        pending_book(&mut a, &d, true);
        assert_eq!(workbook_pending(&a), 0, "a synced preview holds nothing");
        assert!(!close_is_cancelled(&mut a), "a synced preview never blocks close");
        assert!(a.project_ui.prompt.is_none());
        guard(&mut a, Pending::New);
        assert!(a.project_ui.prompt.is_none(), "nothing to lose: no prompt");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn pending_workbook_edits_join_the_close_prompt() {
        let (mut a, d) = app("wb-close");
        pending_book(&mut a, &d, false);
        assert_eq!(workbook_pending(&a), 1);
        assert!(!a.ed.is_dirty(), "the model itself is clean");
        assert!(close_is_cancelled(&mut a));
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        assert!(a.xlsx.is_some(), "nothing dropped before the answer");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn pending_workbook_edits_join_the_open_other_prompt() {
        let (mut a, d) = app("wb-open");
        pending_book(&mut a, &d, false);
        guard(&mut a, Pending::New);
        assert_eq!(a.project_ui.prompt, Some(Pending::New));
        assert!(a.xlsx.is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn workbook_prompt_keep_and_discard() {
        let (mut a, d) = app("wb-answer");
        pending_book(&mut a, &d, false);
        // keep (Review or Cancel): the action is dropped, the preview stays
        guard(&mut a, Pending::New);
        a.project_ui.prompt = None;
        forget_pending(&mut a);
        assert!(a.xlsx.is_some());
        assert_eq!(workbook_pending(&a), 1);
        // discard: the action runs and the preview goes with the old project
        guard(&mut a, Pending::New);
        let p = a.project_ui.prompt.take().unwrap();
        proceed(&mut a, p);
        assert!(a.xlsx.is_none());
        assert_eq!(workbook_pending(&a), 0);
        let _ = std::fs::remove_dir_all(d);
}

    /// One placed drawing, stored as the project file holds it (as after a save), then edited
    /// when `edit`; the model's elements stay untouched.
    fn with_drawing(a: &mut EditorApp, edit: bool) {
        let doc = ddc_io::drawing_model::fixture::sample_plan(0);
        let level = a.ed.level;
        a.plan.drawings.add(
            &doc,
            "plan",
            None,
            level,
            None,
            crate::drawing::Origin::MinToZero,
            ddc_geom::v2(0.0, 0.0),
        );
        drawings_saved(a);
        if edit {
            a.plan.drawings.list[0].mark_edited();
        }
    }

    /// The drawings as they are now are what the project file holds.
    fn drawings_saved(a: &mut EditorApp) {
        crate::drawing::store::begin_save(a).unwrap();
        crate::drawing::store::saved(a);
    }

    fn mark_drawings_exported(a: &mut EditorApp) {
        let snap = a.plan.drawings.generations();
        a.plan.drawings.mark_saved(&snap);
    }

    fn answer_cancel(a: &mut EditorApp) {
        a.project_ui.prompt = None;
        forget_pending(a);
    }

    #[test]
    fn close_prompts_on_dirty_drawing() {
        let (mut a, d) = app("dw24close");
        with_drawing(&mut a, true);
        assert!(!a.ed.is_dirty(), "only the drawing holds edits");
        assert_eq!(
            unsaved(&a),
            Unsaved {
                model: false,
                drawings: 1,
                ..Default::default()
            }
        );
        assert!(close_requested(&mut a), "the close waits for the answer");
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        assert!(!a.project_ui.close_now);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn guard_prompts_on_dirty_drawing() {
        let (mut a, d) = app("dw24guard");
        with_drawing(&mut a, true);
        request_new(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::New));
        assert_eq!(a.plan.drawings.edited(), 1, "nothing dropped before the answer");
        answer_cancel(&mut a);
        let other = d.join("other.ddc");
        request_open(&mut a, other.clone());
        assert_eq!(a.project_ui.prompt, Some(Pending::Open(other)));
        assert!(a.project_ui.job.is_none(), "no open started");
        answer_cancel(&mut a);
        // a recent-file row or a dropped project file arrives as an open request
        a.project_ui.open_request = Some(d.join("recent.ddc"));
        if let Some(p) = a.project_ui.open_request.take() {
            request_open(&mut a, p);
        }
        assert_eq!(a.project_ui.prompt, Some(Pending::Open(d.join("recent.ddc"))));
        answer_cancel(&mut a);
        request_sample(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::Sample));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn clean_drawing_needs_no_prompt() {
        let (mut a, d) = app("dw24clean");
        with_drawing(&mut a, false);
        assert!(!unsaved(&a).any());
        assert!(!close_requested(&mut a));
        assert!(a.project_ui.prompt.is_none());
        request_new(&mut a);
        assert!(a.project_ui.prompt.is_none(), "nothing to lose: runs at once");
        let _ = std::fs::remove_dir_all(d);
    }

    /// A DXF export is not the project file: the drawing still asks before a close.
    #[test]
    fn dxf_export_does_not_stand_for_the_project_save() {
        let (mut a, d) = app("dw24saved");
        with_drawing(&mut a, true);
        mark_drawings_exported(&mut a);
        assert_eq!(unsaved(&a).drawings, 1);
        assert!(close_requested(&mut a));
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        let _ = std::fs::remove_dir_all(d);
    }

    /// The project save writes the drawings: no second question, and they come back on open.
    #[test]
    fn project_save_writes_drawings_and_ends_the_prompt() {
        let (mut a, d) = app("dw24both");
        add_level(&mut a, "mine", 3.0);
        with_drawing(&mut a, true);
        assert_eq!(
            unsaved(&a),
            Unsaved {
                model: true,
                drawings: 1,
                ..Default::default()
            }
        );
        request_new(&mut a);
        assert_eq!(a.project_ui.prompt, Some(Pending::New));
        // the Save answer
        a.project_ui.prompt = None;
        a.project_ui.after_save = Some(Pending::New);
        let p = d.join("both.ddc");
        assert!(start_save(&mut a, Some(p.clone())));
        wait_for_job(&mut a);
        assert!(a.project_ui.prompt.is_none(), "nothing left to ask");
        assert!(!has_level(&a, "mine"), "the new project came");
        assert!(a.plan.drawings.list.is_empty());
        assert_eq!(ddc_model::saved_underlays(&p).unwrap().0, 1);
        request_open(&mut a, p.clone());
        wait_for_job(&mut a);
        assert_eq!(a.ed.project.path(), Some(p.as_path()));
        assert_eq!(a.plan.drawings.list.len(), 1, "the drawing is back");
        assert!(!unsaved(&a).any());
        let _ = std::fs::remove_dir_all(d);
    }

    /// Undo of a drawing edit back to the saved state needs no save.
    #[test]
    fn drawing_undo_to_saved_is_clean() {
        let (mut a, d) = app("dw24undo");
        with_drawing(&mut a, false);
        let g = a.plan.drawings.list[0].gen;
        a.plan.drawings.list[0].mark_edited();
        assert_eq!(unsaved(&a).drawings, 1);
        a.plan.drawings.list[0].gen = g;
        assert!(!unsaved(&a).any());
        let _ = std::fs::remove_dir_all(d);
    }

    /// A drawing-only edit makes the autosave due, and the snapshot carries it.
    #[test]
    fn autosave_snapshot_carries_a_drawing_edit() {
        let (mut a, d) = app("dw24auto");
        with_drawing(&mut a, false);
        crate::drawing::store::frame(&mut a);
        a.autosave.mark_clean(a.ed.revision);
        let n = a.plan.drawings.list[0].u.lines.len();
        a.plan.drawings.list[0].u.lines.truncate(n - 1);
        a.plan.drawings.list[0].mark_edited();
        a.plan.drawings.touch();
        crate::drawing::store::frame(&mut a);
        assert!(a.autosave.dirty(a.ed.revision), "no model revision moved");
        assert!(a.ed.is_dirty() && !a.ed.is_model_dirty());
        let snap = a.flush_autosave().expect("snapshot written");
        let want = ddc_io::drawing_model::blob::entity_count(&a.plan.drawings.list[0].u) as i64;
        assert_eq!(ddc_model::saved_underlays(&snap).unwrap(), (1, want));
        assert!(!a.autosave.dirty(a.ed.revision));
        let _ = std::fs::remove_dir_all(d);
    }

    /// A first save into a folder that cannot take the file says so; nothing is lost.
    #[test]
    fn first_save_to_unwritable_folder_keeps_the_work() {
        let (mut a, d) = app("dw24ro");
        add_level(&mut a, "mine", 3.0);
        with_drawing(&mut a, true);
        let bad = d.join("no-such-dir").join("x.ddc");
        if start_save(&mut a, Some(bad.clone())) {
            wait_for_job(&mut a);
        }
        let Some(Notice::SaveFailed(f)) = &a.project_ui.notice else {
            panic!("the failure is shown");
        };
        assert_eq!(f.again, Again::Save(Some(bad.clone())));
        assert!(!bad.exists());
        assert_eq!(a.ed.project.path(), None, "not bound to the failed file");
        assert!(has_level(&a, "mine"));
        let want = Unsaved { model: true, drawings: 1, ..Default::default() };
        assert_eq!(unsaved(&a), want);
        assert_eq!(a.plan.drawings.list.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn new_project_drops_stale_drawings() {
        let (mut a, d) = app("dw24bnew");
        with_drawing(&mut a, false);
        request_new(&mut a);
        assert!(a.project_ui.prompt.is_none());
        assert!(a.plan.drawings.list.is_empty(), "no drawing bound to the old levels");
        assert_eq!(a.plan.drawings.edited(), 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn swap_to_other_project_clears_drawings_and_keeps_rules() {
        let (mut a, d) = app("dw24bopen");
        with_drawing(&mut a, true);
        let rules = a.plan.drawings.rules.clone();
        a.plan.drawings.sel = 3;
        // the swap runs only after the prompt was answered
        a.ed.new_project().unwrap();
        after_swap(&mut a);
        assert!(a.plan.drawings.list.is_empty());
        assert_eq!(a.plan.drawings.sel, 0);
        assert!(!unsaved(&a).any());
        assert_eq!(a.plan.drawings.rules.clone(), rules);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn title_star_for_edited_drawing_only() {
        let (mut a, d) = app("dw24btitle");
        assert!(!current_title(&a).contains('*'));
        with_drawing(&mut a, true);
        assert!(!a.ed.is_model_dirty());
        assert!(current_title(&a).contains('*'), "{}", current_title(&a));
        mark_drawings_exported(&mut a);
        assert!(current_title(&a).contains('*'), "an export is not the project save");
        drawings_saved(&mut a);
        assert!(!current_title(&a).contains('*'));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn prompt_names_what_is_unsaved() {
        let c = Catalog::new(Lang::En);
        let model = Unsaved {
            model: true,
            ..Default::default()
        };
        assert_eq!(prompt_body(&c, model, &[]), c.t("project.unsaved.body"));
        let two = Unsaved {
            drawings: 2,
            ..Default::default()
        };
        let b = prompt_body(&c, two, &[]);
        assert!(b.contains('2') && !b.contains('#'), "{b}");
        assert!(!b.contains(&c.t("project_unsaved.unsaved.model")));
        let both = prompt_body(
            &c,
            Unsaved {
                model: true,
                drawings: 1,
                ..Default::default()
            },
            &[],
        );
        assert!(both.starts_with(&c.t("project_unsaved.unsaved.model")), "{both}");
        let agent = Unsaved {
            agent: 3,
            ..Default::default()
        };
        assert!(agent.any(), "an external agent's draft is work a close loses");
        assert!(!agent.project(), "it is not in the project file");
        let line = c.tf("agent_draft.unsaved.agent", &[("n", "3")]);
        assert!(prompt_body(&c, agent, &[]).contains(&line), "named in the question");
        assert!(both.contains('1'), "{both}");
        for lang in [Lang::Ru, Lang::En] {
            let c = Catalog::new(lang);
            let k = "project_unsaved.unsaved.model";
            assert_ne!(c.t(k), k, "{k}");
            for n in [1u64, 3, 5] {
                let s = c.plural("project_unsaved.unsaved.drawings", n);
                assert!(s.contains(&n.to_string()), "{lang:?} {n}: {s}");
            }
        }
    }

    #[test]
    fn strings_and_commands_exist_in_both_languages() {
        let reg = Registry::builtin();
        for id in ["file.new", "file.open", "file.save", "file.save_as"] {
            assert!(reg.get(id).is_some(), "{id}");
        }
        for lang in [Lang::Ru, Lang::En] {
            let c = Catalog::new(lang);
            for k in [
                "project.untitled",
                "project.read_only_tag",
                "project.job.save",
                "project.job.window",
                "project.stage.save",
                "project.unsaved.title",
                "project.unsaved.body",
                "project.btn.save",
                "project.btn.discard",
                "project.btn.cancel",
                "project.btn.ok",
                "project.btn.read_only",
                "project.locked.title",
                "project.locked.body",
                "project.locked_save.body",
                "project.newer.title",
                "project.newer.body",
                "project.damaged.title",
                "project.damaged.body",
                "project.not_project.title",
                "project.not_project.body",
                "project.open_failed.title",
                "project.open_failed.body",
                "project.save_failed.title",
                "project.save_failed.body",
                "project.saved",
                "project.upgraded",
                "project.upgraded_ro",
                "project.read_only_toast",
                "project.filter",
                "project.menu.recent",
                "project.recent.empty",
                "project.orphans.title",
                "project.orphans.body",
                "workbook_changes.wb.title",
                "workbook_changes.wb.body",
                "workbook_changes.wb.line",
                "workbook_changes.wb.review",
                "workbook_changes.wb.discard",
            ] {
                assert_ne!(c.t(k), k, "{k}");
            }
        }
    }

    /// The question is answered with `act`, as a click in `prompt_window` does.
    fn reply(a: &mut EditorApp, act: Act) {
        let p = a.project_ui.prompt.take().expect("a question is up");
        answer(a, p, act);
    }

    /// One seed per draft kind: family tab, line library entry, line styles.
    fn draft_seeds() -> [fn(&EditorApp); 3] {
        [
            |_| crate::family_editor::seed_draft_for_test(),
            |_| crate::range_libs::seed_hatch_draft_for_test("h7"),
            |a| crate::range_ui::seed_styles_draft_for_test(&a.ed, a.ed.level),
        ]
    }

    /// One seed per window draft: view graphics, view filter, object styles, material card,
    /// type dialog, parameter editor.
    fn window_seeds() -> [fn(&EditorApp); 6] {
        [
            |a| crate::view_gfx_ui::vv_win::seed_draft_for_test(&a.ed),
            |_| crate::view_gfx_ui::vv_win::seed_filter_draft_for_test("Zz F"),
            |a| crate::view_gfx_ui::vv_win::seed_styles_draft_for_test(&a.ed),
            |a| crate::view_gfx_ui::materials_win::seed_draft_for_test(&a.ed),
            |a| crate::shell::panels::browser::seed_type_draft_for_test(&a.ed, "Zz Type"),
            |a| {
                crate::shell::panels::browser::seed_params_draft_for_test(&a.ed);
            },
        ]
    }

    #[test]
    fn each_draft_kind_blocks_close_new_and_open_until_answered() {
        let seeds: Vec<fn(&EditorApp)> = draft_seeds().into_iter().chain(window_seeds()).collect();
        for (i, seed) in seeds.iter().enumerate() {
            let (mut a, d) = app(&format!("xs54kind{i}"));
            drafts::discard_all();
            seed(&a);
            let u = unsaved(&a);
            assert_eq!((u.model, u.drawings, u.drafts), (false, 0, 1), "kind {i}");
            assert!(!u.project(), "kind {i}: a draft is not in the project file");
            assert!(!current_title(&a).contains('*'), "kind {i}");
            // Cancel keeps the draft on every path
            assert!(close_requested(&mut a), "kind {i}");
            assert_eq!(a.project_ui.prompt, Some(Pending::Close));
            reply(&mut a, Act::Cancel);
            assert!(!a.project_ui.close_now, "kind {i}");
            request_new(&mut a);
            assert_eq!(a.project_ui.prompt, Some(Pending::New), "kind {i}");
            reply(&mut a, Act::Cancel);
            let other = d.join("other.ddc");
            request_open(&mut a, other.clone());
            assert_eq!(a.project_ui.prompt, Some(Pending::Open(other)), "kind {i}");
            assert!(a.project_ui.job.is_none(), "kind {i}: no open started");
            reply(&mut a, Act::Cancel);
            assert_eq!(drafts::count(&a.ed), 1, "kind {i}: Cancel keeps everything");
            // Discard drops the draft and runs the action
            request_new(&mut a);
            reply(&mut a, Act::Discard);
            assert_eq!(drafts::count(&a.ed), 0, "kind {i}");
            assert!(a.project_ui.prompt.is_none());
            seed(&a);
            assert!(close_requested(&mut a), "kind {i}");
            reply(&mut a, Act::Discard);
            assert!(a.project_ui.close_now, "kind {i}");
            assert_eq!(drafts::count(&a.ed), 0, "kind {i}");
            drafts::discard_all();
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn no_drafts_no_extra_question() {
        let (mut a, d) = app("xs54none");
        drafts::discard_all();
        crate::family_editor::with(|s| {
            s.docs.push(crate::family_editor::Session::new(
                ddc_geom::component::doc::FamDoc::blank(
                    "Clean",
                    ddc_geom::component::Category::Generic,
                    ddc_geom::component::Hosting::None,
                ),
                None,
            ));
            s.open = true;
        });
        assert_eq!(unsaved(&a), Unsaved::default(), "a clean family tab is no draft");
        assert!(!close_requested(&mut a));
        request_new(&mut a);
        assert!(a.project_ui.prompt.is_none(), "nothing to lose: runs at once");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn question_lists_every_draft_in_the_given_language() {
        let (a, d) = app("xs54labels");
        drafts::discard_all();
        for seed in draft_seeds() {
            seed(&a);
        }
        let fam = crate::family_editor::draft_names();
        assert_eq!(fam.len(), 1);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let held = drafts::labels(&c, &a.ed.model);
            assert_eq!(held.len(), 3, "{lang:?}");
            assert!(held[0].contains(fam[0].as_str()), "{held:?}");
            assert!(held[1].contains("h7"), "{held:?}");
            assert_eq!(held[2], c.t("drafts.draft.styles"));
            let body = prompt_body(&c, unsaved(&a), &held);
            assert!(body.starts_with(&c.t("drafts.drafts.head")), "{body}");
            assert!(held.iter().all(|h| body.contains(h.as_str())), "{body}");
            let model = Unsaved {
                model: true,
                drafts: 3,
                ..Default::default()
            };
            let both = prompt_body(&c, model, &held);
            assert!(both.starts_with(&c.t("project_unsaved.unsaved.model")), "{both}");
            for k in [
                "drafts.drafts.head",
                "drafts.draft.styles",
                "drafts.btn.save_all",
                "drafts.drafts.refused",
            ] {
                assert_ne!(c.t(k), k, "{lang:?} {k}");
            }
            for k in ["drafts.draft.family", "drafts.draft.line"] {
                let s = c.tf(k, &[("name", "N1")]);
                assert!(s.contains("N1") && !s.contains("{name}"), "{lang:?} {k}: {s}");
            }
        }
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn save_puts_drafts_into_the_model_before_the_project_save() {
        let (mut a, d) = app("xs54save");
        drafts::discard_all();
        crate::range_libs::seed_hatch_draft_for_test("h7");
        crate::range_ui::seed_styles_draft_for_test(&a.ed, a.ed.level);
        assert!(drafts::save_all(&mut a));
        let u = unsaved(&a);
        assert_eq!(u.drafts, 0, "nothing left outside the model");
        assert!(u.model, "the project save now writes them");
        assert!(a.ed.model.drawq_hatches().unwrap().get("h7").is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn question_names_every_window_draft_in_the_given_language() {
        let (a, d) = app("xs54blabels");
        drafts::discard_all();
        for seed in window_seeds() {
            seed(&a);
        }
        let v = ddc_model::ViewKey::Plan(a.ed.level);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let ru = lang == Lang::Ru;
            let list = drafts::list(&a.ed.model, ru);
            use drafts::Draft as D;
            let [D::View(view), D::Filter(filter), D::ObjectStyles, D::Material(_), D::Type(ty), D::Params(fam)] =
                list.as_slice()
            else {
                panic!("{lang:?}: {list:?}");
            };
            assert_eq!(*view, crate::view_gfx_ui::view_label_in(ru, &a.ed, v));
            assert_eq!(filter, "Zz F");
            let held = drafts::labels(&c, &a.ed.model);
            assert_eq!(held.len(), 6, "{lang:?}");
            for (h, name) in [(&held[0], view), (&held[1], filter), (&held[4], ty), (&held[5], fam)] {
                assert!(h.contains(name.as_str()), "{lang:?}: {h} lacks {name}");
            }
            assert_eq!(held[2], c.t("drafts.draft.object_styles"));
            assert!(held.iter().all(|h| !h.is_empty() && !h.contains("{name}")), "{held:?}");
            let body = prompt_body(&c, unsaved(&a), &held);
            assert!(held.iter().all(|h| body.contains(h.as_str())), "{body}");
            assert_eq!(drafts::label(&c, &drafts::Draft::Filter(String::new())), c.t("drafts.draft.filter_new"));
            for k in ["drafts.draft.filter_new", "drafts.draft.object_styles"] {
                assert_ne!(c.t(k), k, "{lang:?} {k}");
            }
            for k in [
                "drafts.draft.view",
                "drafts.draft.filter",
                "drafts.draft.material",
                "drafts.draft.type",
                "drafts.draft.params",
            ] {
                let s = c.tf(k, &[("name", "N1")]);
                assert!(s.contains("N1") && !s.contains("{name}") && s != k, "{lang:?} {k}: {s}");
            }
        }
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        assert_ne!(en.t("drafts.draft.object_styles"), ru.t("drafts.draft.object_styles"));
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn save_all_puts_every_window_draft_into_the_model() {
        let (mut a, d) = app("xs54bsave");
        drafts::discard_all();
        // the parameter editor saves only into a family of the project
        if a.ed.model.families().unwrap().is_empty() {
            let f = ddc_model::library().unwrap().into_iter().next().unwrap();
            let loaded = crate::shell::panels::browser::load_family(&mut a.ed, &f.name, &a.sh.cat);
            assert!(loaded);
        }
        for seed in window_seeds() {
            seed(&a);
        }
        let fam = crate::shell::panels::browser::seed_params_draft_for_test(&a.ed);
        assert_eq!(unsaved(&a).drafts, 6);
        let v = ddc_model::ViewKey::Plan(a.ed.level);
        let detail = a.ed.model.view_gfx(v).unwrap().effective.detail;
        assert!(drafts::save_all(&mut a));
        let u = unsaved(&a);
        assert_eq!(u.drafts, 0, "{:?}", drafts::list(&a.ed.model, false));
        assert!(u.model, "the project save now writes them");
        let m = &a.ed.model;
        assert_ne!(m.view_gfx(v).unwrap().effective.detail, detail);
        assert!(m.view_filters().unwrap().iter().any(|f| f.spec.name == "Zz F"));
        assert!(m.object_styles().unwrap().values().any(|o| o.line == Some(0x12_34_56)));
        assert!(m.materials().unwrap().iter().any(|x| x.spec.note == "Zz note"));
        let types = crate::shell::panels::browser::wall_type_names_for_test(m);
        assert!(types.iter().any(|t| t == "Zz Type"), "{types:?}");
        let fid = m.family_id(&fam).unwrap().unwrap();
        assert!(m.comp_types(fid).unwrap().iter().any(|t| t.1 == "Zz T"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_refused_window_draft_stops_save_all_and_nothing_closes() {
        let (mut a, d) = app("xs54brefused");
        drafts::discard_all();
        crate::view_gfx_ui::vv_win::seed_draft_for_test(&a.ed);
        crate::shell::panels::browser::seed_type_draft_for_test(&a.ed, "  ");
        assert!(close_requested(&mut a));
        reply(&mut a, Act::Save);
        assert!(!a.project_ui.close_now, "nothing closed");
        assert!(a.project_ui.prompt.is_none());
        let left = drafts::list(&a.ed.model, false);
        assert!(matches!(left.as_slice(), [drafts::Draft::Type(_)]), "{left:?}");
        assert!(crate::shell::panels::browser::type_error_for_test().is_some(), "the dialog says why");
        assert!(unsaved(&a).model, "the view draft went into the model, not lost");
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn untouched_project_info_window_is_no_draft() {
        let (mut a, d) = app("xs61bclean");
        drafts::discard_all();
        crate::data_ui::open_for_layout(&mut a, "info");
        assert!(crate::data_ui::info_open());
        assert_eq!(unsaved(&a), Unsaved::default(), "no typed values, nothing to ask");
        assert!(drafts::list(&a.ed.model, false).is_empty());
        assert!(!close_requested(&mut a));
        assert!(a.project_ui.prompt.is_none());
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn project_info_edits_ask_on_close_and_cancel_keeps_discard_drops() {
        let (mut a, d) = app("xs61binfo");
        drafts::discard_all();
        crate::data_ui::seed_draft_for_test(&a.ed);
        let u = unsaved(&a);
        assert_eq!((u.model, u.drawings, u.drafts), (false, 0, 1));
        assert!(!current_title(&a).contains('*'), "a draft is not in the project file");
        assert_eq!(drafts::list(&a.ed.model, false), [drafts::Draft::ProjectInfo]);
        let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
        for c in [&en, &ru] {
            let held = drafts::labels(c, &a.ed.model);
            assert_eq!(held, [c.t("drafts.draft.info")]);
            assert_ne!(held[0], "drafts.draft.info");
            let body = prompt_body(c, unsaved(&a), &held);
            assert!(body.contains(held[0].as_str()), "{body}");
        }
        assert_ne!(en.t("drafts.draft.info"), ru.t("drafts.draft.info"));
        // Cancel keeps the window and every typed value
        assert!(close_requested(&mut a));
        assert_eq!(a.project_ui.prompt, Some(Pending::Close));
        reply(&mut a, Act::Cancel);
        assert!(!a.project_ui.close_now);
        assert!(crate::data_ui::info_open());
        assert_eq!(drafts::count(&a.ed), 1, "Cancel keeps the draft");
        // Discard drops the window and the project closes without writing it
        let steps = a.ed.undo_len();
        assert!(close_requested(&mut a));
        reply(&mut a, Act::Discard);
        assert!(a.project_ui.close_now);
        assert!(!crate::data_ui::info_open());
        assert_eq!(drafts::count(&a.ed), 0);
        assert_eq!(a.ed.undo_len(), steps);
        assert_ne!(a.ed.model.project_info().unwrap_or_default().name, "Zz Info");
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn save_all_writes_project_info_in_one_step() {
        let (mut a, d) = app("xs61bsave");
        drafts::discard_all();
        crate::data_ui::seed_draft_for_test(&a.ed);
        let steps = a.ed.undo_len();
        assert!(drafts::save_all(&mut a));
        let u = unsaved(&a);
        assert_eq!(u.drafts, 0, "{:?}", drafts::list(&a.ed.model, false));
        assert!(u.model, "the project save now writes it");
        assert_eq!(a.ed.model.project_info().unwrap().name, "Zz Info");
        assert_eq!(a.ed.undo_len(), steps + 1);
        drafts::discard_all();
        let _ = std::fs::remove_dir_all(d);
    }
}
