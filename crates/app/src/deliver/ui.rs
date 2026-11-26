// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `file.deliver`: one window for the whole way from a brief to files. Review (a brief for the
//! assistant, or the model as it is: the drawing set and the files), then the run with its
//! progress and Cancel, then the result with Open and Show in folder. The window may be hidden
//! while the run goes on; a toast brings it back.

use std::cell::RefCell;
use std::path::Path;

use ddc_ui::notify::Level;
use ddc_ui::theme::{space, Role};
use ddc_ui::widgets::{self, Button, TextField};
use ddc_ui::{Lang, Theme};
use egui::{self, RichText};

use super::{Kind, Outputs, Run, State as RowState, Target, SCHEMAS};
use crate::shell::EditorApp;
use crate::sheet_view::DsetDialog;
use crate::ui_kit as kit;

const CMD: &str = "file.deliver";

/// What the review asks before anything is written.
pub struct Review {
    pub brief: String,
    pub drawings: bool,
    pub dset: DsetDialog,
    pub options: bool,
    pub outputs: Outputs,
    pub target: Target,
    pub err: String,
}

impl Review {
    /// Defaults from the project: its folder and name, the title block of the language, PDF
    /// and IFC4X3.
    pub fn new(app: &EditorApp, ru: bool) -> Self {
        Review {
            brief: String::new(),
            drawings: true,
            dset: DsetDialog::for_lang(ru),
            options: false,
            outputs: Outputs::defaults(),
            target: Target::default_for(&app.ed),
            err: String::new(),
        }
    }

    /// Why the main action is off; `None` when it can run.
    pub fn blocked(&self, app: &EditorApp, ru: bool) -> Option<String> {
        let t = |k: &str| crate::tr_lang(ru, k, &[]);
        if !self.drawings && !self.outputs.any() {
            return Some(t("deliver.why.nothing"));
        }
        if self.drawings {
            if let Some(e) = self.dset.preview_error() {
                return Some(e.to_string());
            }
        } else if self.outputs.prints() && super::sheets_to_print(&app.ed).is_empty() {
            return Some(t("deliver.why.no_sheets"));
        }
        None
    }

    /// The text the assistant gets: the brief and the files to deliver.
    pub fn ask_text(&self, ru: bool) -> String {
        let mut names: Vec<String> = self.outputs.names().iter().map(|s| s.to_string()).collect();
        if self.outputs.ifc {
            names.push(format!("ifc_schema {}", self.outputs.schema_name()));
        }
        let brief = self.brief.trim();
        crate::tr_lang(ru, "deliver.ask", &[("brief", brief), ("outputs", &names.join(", "))])
    }
}

/// The drawing set in one undo step when asked for, then the run of the files.
pub fn start(app: &mut EditorApp, r: &Review, ru: bool) -> Result<Run, String> {
    let sheets = if r.drawings {
        crate::sheet_view::build_drawing_set(&mut app.ed, r.dset.options(), app.sh.cat.lang)?
    } else {
        super::sheets_to_print(&app.ed)
    };
    Ok(Run::start(r.outputs, r.target.clone(), sheets, ru))
}

#[derive(Default)]
pub struct Deliver {
    pub review: Option<Review>,
    pub run: Option<Run>,
    pub shown: bool,
}

thread_local! {
    static STATE: RefCell<Deliver> = RefCell::new(Deliver::default());
}

pub fn with<R>(f: impl FnOnce(&mut Deliver) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

pub fn register(a: &mut EditorApp) {
    a.register_command(CMD, open);
}

/// Opens the window: the run in progress or its result, else a fresh review.
fn open(app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let fresh = with(|s| s.run.is_none() && s.review.is_none());
    let review = fresh.then(|| Review::new(app, ru));
    with(|s| {
        if let Some(r) = review {
            s.review = Some(r);
        }
        s.shown = true;
    });
}

/// After the Apply of a delivery card: its files, from the model as it now is.
pub fn after_apply(app: &mut EditorApp, o: Outputs) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let sheets = super::sheets_to_print(&app.ed);
    let run = Run::start(o, Target::default_for(&app.ed), sheets, ru);
    with(|s| {
        s.review = None;
        s.run = Some(run);
        s.shown = true;
    });
}

/// Every frame: the run moves on whether or not the window is shown.
pub fn frame(ctx: &egui::Context, app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    let mut st = with(std::mem::take);
    if let Some(run) = &mut st.run {
        if !run.finished() {
            if run.step(&app.ed) {
                ended(app, run, ru, st.shown);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }
    if st.shown {
        window(ctx, app, ru, &mut st);
    }
    with(|s| *s = st);
}

fn ended(app: &mut EditorApp, run: &Run, ru: bool, shown: bool) {
    let ok = run.rows().iter().all(|r| matches!(r.state, RowState::Done { .. }));
    let dir = run.target().dir.display().to_string();
    let (level, text) = if ok {
        let t = crate::tr_lang(ru, "deliver.toast.done", &[("dir", &dir)]);
        (Level::Success, t)
    } else {
        (Level::Warning, crate::tr_lang(ru, "deliver.toast.failed", &[]))
    };
    if shown {
        app.sh.toasts.record(level, text);
    } else {
        let show = crate::tr_lang(ru, "deliver.toast.show", &[]);
        app.sh.toasts.push_cmd(level, text, show, CMD);
    }
}

enum Act {
    /// Ask the assistant to draft the deliverable.
    Ask,
    Go,
    Close,
    Hide,
    Cancel,
    Retry,
    Pick,
}

fn window(ctx: &egui::Context, app: &mut EditorApp, ru: bool, st: &mut Deliver) {
    let mut open = true;
    let mut acts: Vec<Act> = vec![];
    if let Some(r) = &mut st.review {
        if r.drawings {
            r.dset.refresh(&app.ed, app.sh.cat.lang);
        }
    }
    let blocked = st.review.as_ref().and_then(|r| r.blocked(app, ru));
    let title = crate::tr_lang(ru, "deliver.title", &[]);
    kit::Dialog::new("deliver", title, kit::DialogSize::Tool)
        .stable()
        .open(&mut open)
        .show(ctx, |ui| {
            let why = blocked.as_deref();
            kit::pinned_footer(ui, |ui, footer| match (&mut st.run, &mut st.review) {
                (Some(run), _) if footer => run_footer(ui, ru, run, &mut acts),
                (Some(run), _) => run_body(ui, ru, run),
                (None, Some(r)) if footer => review_footer(ui, ru, r, why, &mut acts),
                (None, Some(r)) => review_body(ui, ru, r, &mut acts),
                (None, None) => {}
            });
        });
    if !open {
        acts.push(if st.run.as_ref().is_some_and(|r| !r.finished()) {
            Act::Hide
        } else {
            Act::Close
        });
    }
    for a in acts {
        act(app, ru, st, a);
    }
}

fn act(app: &mut EditorApp, ru: bool, st: &mut Deliver, a: Act) {
    match a {
        Act::Ask => {
            if let Some(r) = st.review.take() {
                crate::ai::ui::ask(app, &r.ask_text(ru));
            }
            st.shown = false;
        }
        Act::Go => {
            let Some(r) = &mut st.review else { return };
            match start(app, r, ru) {
                Ok(run) => {
                    st.run = Some(run);
                    st.review = None;
                }
                Err(e) => r.err = e,
            }
        }
        Act::Pick => {
            let Some(r) = &mut st.review else { return };
            let mut d = crate::file_pick::Dlg::new();
            if let Some(p) = r.target.dir.parent().filter(|p| p.is_dir()) {
                d = d.set_directory(p);
            }
            if let Some(dir) = d.pick_folder() {
                r.target.dir = dir;
            }
        }
        Act::Cancel => {
            if let Some(run) = &mut st.run {
                run.cancel();
            }
        }
        Act::Retry => {
            if let Some(again) = st.run.as_ref().and_then(Run::retry) {
                st.run = Some(again);
            }
        }
        Act::Hide => st.shown = false,
        Act::Close => {
            st.shown = false;
            st.review = None;
            st.run = None;
        }
    }
}

fn strong(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

fn muted(ui: &mut egui::Ui, th: &Theme, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(th.sem().text_secondary)).wrap());
}

fn review_body(ui: &mut egui::Ui, ru: bool, r: &mut Review, acts: &mut Vec<Act>) {
    let th = ddc_ui::theme::of(ui.ctx());
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    let brief = t("deliver.brief");
    let lid = ui.label(RichText::new(&brief).strong()).id;
    let hint = t("deliver.brief_hint");
    TextField::new(&mut r.brief).labelled_by(lid, &brief).hint(&hint).show(ui, &th);
    muted(ui, &th, &t("deliver.brief_note"));
    if !r.brief.trim().is_empty() {
        ui.add_space(space::SM);
        files_ui(ui, &th, ru, r);
        return;
    }
    ui.add_space(space::MD);
    ui.separator();
    strong(ui, &t("deliver.or"));
    ui.horizontal(|ui| {
        widgets::checkbox(ui, &th, &mut r.drawings, &t("deliver.drawings"));
        let label = t(if r.options {
            "deliver.options_hide"
        } else {
            "deliver.options"
        });
        if r.drawings && Button::quiet(&label).small().show(ui, &th).clicked() {
            r.options = !r.options;
        }
    });
    if r.drawings {
        if r.options {
            r.dset.controls(ui, ru);
        } else if let Some(s) = r.dset.summary() {
            muted(ui, &th, s);
        } else if let Some(e) = r.dset.preview_error() {
            ui.label(RichText::new(e).color(th.sem().error));
        }
    }
    ui.add_space(space::SM);
    files_ui(ui, &th, ru, r);
    ui.add_space(space::SM);
    let dir = r.target.dir.display().to_string();
    widgets::prop_row(ui, &th, &t("deliver.folder"), |ui| {
        ui.horizontal(|ui| {
            if Button::quiet(&t("deliver.change")).small().show(ui, &th).clicked() {
                acts.push(Act::Pick);
            }
            let c = th.sem().text_primary;
            widgets::elided_label(ui, &dir, th.font(Role::Body), c);
        });
    });
    let old = r.target.replaces(&r.outputs);
    if !old.is_empty() {
        let names: Vec<String> = old
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        let line = crate::tr_lang(ru, "deliver.replaces", &[("names", &names.join(", "))]);
        muted(ui, &th, &line);
    }
    if r.drawings {
        muted(ui, &th, &t("deliver.undo_note"));
    }
    if !r.err.is_empty() {
        kit::error_note(ui, &r.err);
    }
}

fn files_ui(ui: &mut egui::Ui, th: &Theme, ru: bool, r: &mut Review) {
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    strong(ui, &t("deliver.files"));
    let o = &mut r.outputs;
    widgets::checkbox(ui, th, &mut o.pdf, &t("deliver.pdf"));
    ui.horizontal(|ui| {
        widgets::checkbox(ui, th, &mut o.ifc, &t("deliver.ifc"));
        let mut at = SCHEMAS.iter().position(|s| s.0 == o.schema).unwrap_or(0);
        let items: Vec<&str> = SCHEMAS.iter().map(|s| s.1).collect();
        ui.add_enabled_ui(o.ifc, |ui| {
            if widgets::dropdown(ui, th, "deliver.schema", &mut at, &items, None).changed() {
                o.schema = SCHEMAS[at].0;
            }
        });
    });
}

fn review_footer(
    ui: &mut egui::Ui,
    ru: bool,
    r: &Review,
    blocked: Option<&str>,
    acts: &mut Vec<Act>,
) {
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    // one primary: the brief goes to the assistant, else the model as it is is delivered
    if !r.brief.trim().is_empty() {
        let none = (!r.outputs.any()).then(|| t("deliver.why.nothing"));
        if kit::primary_action(ui, t("deliver.draft"), none.as_deref()) {
            acts.push(Act::Ask);
        }
    } else {
        let label = t(if r.drawings {
            "deliver.go"
        } else {
            "deliver.go_files"
        });
        if kit::primary_action(ui, label, blocked) {
            acts.push(Act::Go);
        }
    }
    if kit::cancel_button(ui, t("deliver.close")).clicked() {
        acts.push(Act::Close);
    }
}

fn row_label(ru: bool, kind: Kind, o: &Outputs) -> String {
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    match kind {
        Kind::Print => format!("{} \u{b7} PDF", t("deliver.row.print")),
        Kind::Ifc => format!("{} \u{b7} {}", t("deliver.row.ifc"), o.schema_name()),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn run_body(ui: &mut egui::Ui, ru: bool, run: &Run) {
    let th = ddc_ui::theme::of(ui.ctx());
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    let dir = run.target().dir.display().to_string();
    if run.finished() {
        let ok = run.rows().iter().all(|r| matches!(r.state, RowState::Done { .. }));
        strong(ui, &t(if ok {
            "deliver.done.title"
        } else {
            "deliver.done.partial"
        }));
        muted(ui, &th, &dir);
    } else {
        let head = crate::tr_lang(ru, "deliver.running", &[("dir", &dir)]);
        widgets::progress(ui, &th, Some(run.fraction()), &head);
    }
    ui.add_space(space::SM);
    let o = run.outputs();
    for row in run.rows() {
        ui.add_space(space::XS);
        strong(ui, &row_label(ru, row.kind, &o));
        match &row.state {
            RowState::Waiting => muted(ui, &th, &t("deliver.state.waiting")),
            RowState::Running => muted(ui, &th, &t("deliver.state.running")),
            RowState::Cancelled => muted(ui, &th, &t("deliver.state.cancelled")),
            RowState::Failed(e) => {
                ui.label(RichText::new(t("deliver.state.failed")).color(th.sem().error));
                kit::error_note(ui, e);
            }
            RowState::Done { files, note } => {
                for f in files {
                    ui.horizontal(|ui| {
                        if Button::quiet(&t("deliver.open")).small().show(ui, &th).clicked() {
                            crate::gfx::open_path(f);
                        }
                        let c = th.sem().text_primary;
                        widgets::elided_label(ui, &file_name(f), th.font(Role::Body), c);
                    });
                }
                if let Some(n) = note {
                    muted(ui, &th, n);
                }
            }
        }
    }
}

fn run_footer(ui: &mut egui::Ui, ru: bool, run: &Run, acts: &mut Vec<Act>) {
    let t = |k: &str| crate::tr_lang(ru, k, &[]);
    if !run.finished() {
        if kit::secondary_button(ui, t("deliver.hide")).clicked() {
            acts.push(Act::Hide);
        }
        if kit::cancel_button(ui, t("deliver.cancel")).clicked() {
            acts.push(Act::Cancel);
        }
        return;
    }
    let first = run
        .rows()
        .iter()
        .find_map(|r| match &r.state {
            RowState::Done { files, .. } => files.first().cloned(),
            _ => None,
        })
        .unwrap_or_else(|| run.target().dir.clone());
    if kit::primary_action(ui, t("deliver.show_folder"), None) {
        crate::shell::xlsx_head::show_in_folder(&first);
    }
    if run.retry().is_some() && kit::secondary_button(ui, t("deliver.retry")).clicked() {
        acts.push(Act::Retry);
    }
    if kit::cancel_button(ui, t("deliver.close")).clicked() {
        acts.push(Act::Close);
    }
}
