//! The Import window as one designed sequence in one frame: pick, progress with stages, calm
//! result. File row, body zone and footer keep their size and place in every state.
use super::{drawing_win, tasks_ui, EditorApp};
use crate::import::{entry_for, Entry};
use crate::tr;
use crate::ui_kit as kit;
use ddc_ui::theme::{self, space, Role};
use ddc_ui::widgets::{self, Button, TextField};
use egui::{self, RichText};
use std::path::Path;

/// Body zone of the window: holds four stages with bar and caption, and the format list.
const ZONE_H: f32 = 184.0;
/// The key-trust question, the longest text of its window.
const DEV_ZONE_H: f32 = 80.0;
const PICK_W: f32 = 132.0;
const CANCEL_W: f32 = 96.0;
const EXT_W: f32 = 128.0;
const STATE_W: f32 = 148.0;
/// Skipped kinds listed by name under the counts; the rest is a count.
const DETAILS: usize = 12;

/// What one finished import reports to the window; `open::summary` may fill it for other doors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts {
    pub path: String,
    pub native: usize,
    pub mesh: usize,
    pub skipped: usize,
    pub notes: usize,
    pub details: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Done(Counts),
    Failed { path: String, message: String },
}

/// State of the window beside the running job, which lives in `EditorApp::import_job`.
#[derive(Default)]
pub struct ImportFlow {
    pub outcome: Option<Outcome>,
    pub dev_open: bool,
    dev_note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Pick,
    Run,
    Done,
    Failed,
}

fn phase(flow: &ImportFlow, running: bool) -> Phase {
    if running {
        return Phase::Run;
    }
    match flow.outcome {
        Some(Outcome::Done(_)) => Phase::Done,
        Some(Outcome::Failed { .. }) => Phase::Failed,
        None => Phase::Pick,
    }
}

/// Why this path cannot be imported, in words for the disabled primary action.
pub fn blocked(path: &str, entries: &[Entry]) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return Some(tr("dlg.imp.no_path", &[]));
    }
    let p = Path::new(path);
    if let Some(e) = entry_for(entries, p) {
        return e.dir.is_none().then(|| module_problem(e));
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if drawing_win::import_exts().contains(&ext.as_str()) {
        None
    } else {
        Some(tr("dlg.imp.unsupported", &[("ext", &ext)]))
    }
}

fn module_problem(e: &Entry) -> String {
    let key = if e.untrusted {
        "dlg.imp.module_untrusted"
    } else {
        "dlg.imp.module_missing"
    };
    tr(key, &[("label", &e.label)])
}

/// Sorted by count; a long tail folds into one line.
fn skipped_rows(skipped: &std::collections::BTreeMap<String, usize>) -> Vec<String> {
    let mut v: Vec<(&String, &usize)> = skipped.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let mut rows: Vec<String> = v
        .iter()
        .take(DETAILS)
        .map(|(k, n)| {
            tr(
                "dlg.imp.skipped_row",
                &[("what", k.as_str()), ("n", &n.to_string())],
            )
        })
        .collect();
    if v.len() > DETAILS {
        let more = (v.len() - DETAILS).to_string();
        rows.push(tr("dlg.imp.skipped_more", &[("n", &more)]));
    }
    rows
}

pub fn counts_from_ifc(path: &Path, r: &ddc_io::ifc::ImportReport) -> Counts {
    let mut details = vec![];
    if r.levels == 0 {
        details.push(tr("dlg.imp.no_levels", &[]));
    }
    if !r.locked.is_empty() {
        details.push(tr("dlg.imp.locked_row", &[("n", &r.locked.len().to_string())]));
    }
    details.extend(skipped_rows(&r.skipped));
    Counts {
        path: path.to_string_lossy().into_owned(),
        native: r.walls
            + r.openings
            + r.slabs
            + r.curtain_walls
            + r.rooms
            + r.separators
            + r.grids
            + r.components
            + r.mep_runs
            + r.columns
            + r.beams
            + r.roofs
            + r.ceilings
            + r.stairs
            + r.ramps
            + r.railings,
        mesh: r.meshes,
        skipped: r.skipped.values().sum(),
        notes: r.warnings.len(),
        details,
    }
}

pub fn counts_from_bridge(path: &Path, r: &ddc_io::bridge::Report) -> Counts {
    let mut details = vec![];
    if r.updated > 0 || r.unchanged > 0 {
        let (u, k) = (r.updated.to_string(), r.unchanged.to_string());
        details.push(tr("dlg.imp.reimport_row", &[("updated", &u), ("same", &k)]));
    }
    details.extend(skipped_rows(&r.skipped));
    Counts {
        path: path.to_string_lossy().into_owned(),
        native: r.wall_segments
            + r.doors
            + r.windows
            + r.voids
            + r.slabs
            + r.curtain_walls
            + r.rooms
            + r.grids
            + r.columns
            + r.beams
            + r.mep_runs
            + r.ceilings,
        mesh: r.meshes,
        skipped: r.skipped.values().sum(),
        notes: r.warning_count,
        details,
    }
}

enum Act {
    Pick,
    Go(String),
    Stop,
    Close,
    Another,
    DevOpen,
}

/// One row of the format list.
fn format_row(
    ui: &mut egui::Ui,
    exts: &str,
    what: &str,
    state: &str,
    warn: bool,
    tip: Option<&str>,
) {
    kit::fixed_row(ui, exts, |ui| {
        let gap = 2.0 * ui.spacing().item_spacing.x;
        let middle = ui.available_width() - EXT_W - STATE_W - gap;
        kit::fixed_cell(ui, EXT_W, |ui| {
            kit::elided(ui, exts);
        });
        kit::fixed_cell(ui, middle, |ui| {
            kit::elided_weak(ui, what);
        });
        kit::fixed_cell(ui, STATE_W, |ui| {
            let th = theme::of(ui.ctx());
            let color = if warn {
                ui.visuals().warn_fg_color
            } else {
                th.sem().text_secondary
            };
            let r = widgets::elided_label(ui, state, th.font(Role::Body), color);
            if let Some(t) = tip {
                let _ = r.on_hover_text(t);
            }
        });
    });
}

fn formats_body(ui: &mut egui::Ui, entries: &[Entry]) {
    let ready = tr("dlg.imp.ready", &[]);
    let drawings = drawing_win::drawing_exts()
        .iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(" ");
    format_row(
        ui,
        ".ifc .ifczip",
        &tr("dlg.imp.fam.model", &[]),
        &ready,
        false,
        None,
    );
    format_row(
        ui,
        &drawings,
        &tr("dlg.imp.fam.drawing", &[]),
        &ready,
        false,
        None,
    );
    format_row(
        ui,
        ".xlsx",
        &tr("dlg.imp.fam.table", &[]),
        &ready,
        false,
        None,
    );
    let what = tr("dlg.imp.fam.module", &[]);
    for e in entries {
        if e.dir.is_some() {
            format_row(ui, &e.label, &what, &ready, false, None);
        } else {
            let state = tr(
                if e.untrusted {
                    "dlg.imp.st_untrusted"
                } else {
                    "dlg.imp.st_missing"
                },
                &[],
            );
            let tip = module_problem(e);
            format_row(ui, &e.label, &what, &state, true, Some(&tip));
        }
    }
}

fn run_body(ui: &mut egui::Ui, cat: &ddc_ui::Catalog, v: &crate::jobs::JobView) {
    let th = theme::of(ui.ctx());
    tasks_ui::stage_rows(ui, &th, cat, v);
    ui.add_space(space::SM);
    tasks_ui::run_bar(ui, &th, v);
    ui.add_space(space::XS);
    let info = tasks_ui::run_info(cat, v);
    widgets::elided_label(ui, &info, th.font(Role::Numeric), th.sem().text_secondary);
}

fn done_body(ui: &mut egui::Ui, c: &Counts) {
    let name = kit::file_name_of(&c.path);
    kit::name_with_tip(ui, &name, &c.path, true);
    ui.add_space(space::SM);
    kit::count_tiles(
        ui,
        &[
            (c.native, tr("dlg.imp.native", &[]), false),
            (c.mesh, tr("dlg.imp.mesh", &[]), false),
            (c.skipped, tr("dlg.imp.skipped", &[]), c.skipped > 0),
        ],
    );
    ui.add_space(space::SM);
    if c.notes > 0 {
        kit::elided_weak(ui, &tr("dlg.imp.notes", &[("n", &c.notes.to_string())]));
    }
    for d in &c.details {
        kit::elided_weak(ui, d);
    }
}

fn failed_body(ui: &mut egui::Ui, path: &str, message: &str) {
    let name = kit::file_name_of(path);
    kit::name_with_tip(ui, &name, path, true);
    ui.add_space(space::SM);
    kit::error_note(ui, message);
}

impl EditorApp {
    /// The window; `import_any` and `poll_import` feed it through `ImportFlow`.
    pub(super) fn import_window(&mut self, ctx: &egui::Context) {
        let entries = crate::import::entries(&self.bridges.list);
        let any_untrusted = self.bridges.list.iter().any(|b| b.trust.is_err());
        let view = tasks_ui::import_view(self);
        let state = phase(&self.import_flow, self.import_job.is_some());
        let mut open = self.import_open;
        let mut act = None;
        let shown_path = self.import_path.clone();
        kit::Dialog::new("import", tr("dlg.imp.title", &[]), kit::DialogSize::Form)
            .open(&mut open)
            .esc_closes(state != Phase::Run)
            .stable()
            .show(ctx, |ui| {
                let th = theme::of(ui.ctx());
                kit::fixed_row(ui, "imp.file", |ui| {
                    if state == Phase::Pick {
                        let w = ui.available_width() - PICK_W - ui.spacing().item_spacing.x;
                        TextField::new(&mut self.import_path)
                            .id_salt("imp.path")
                            .hint(&tr("dlg.imp.path_hint", &[]))
                            .width(w)
                            .show(ui, &th);
                        if Button::secondary(&tr("dlg.imp.pick", &[]))
                            .min_width(PICK_W)
                            .show(ui, &th)
                            .clicked()
                        {
                            act = Some(Act::Pick);
                        }
                    } else {
                        kit::name_with_tip(ui, &shown_path, &shown_path, false);
                    }
                });
                ui.add_space(space::SM);
                kit::zone(ui, "imp.body", ZONE_H, |ui| match state {
                    Phase::Pick => formats_body(ui, &entries),
                    Phase::Run => {
                        if let Some(v) = &view {
                            run_body(ui, &self.sh.cat, v);
                        }
                    }
                    Phase::Done | Phase::Failed => match &self.import_flow.outcome {
                        Some(Outcome::Done(c)) => done_body(ui, c),
                        Some(Outcome::Failed { path, message }) => failed_body(ui, path, message),
                        None => {}
                    },
                });
                kit::dialog_footer(ui, |ui| match state {
                    Phase::Pick => {
                        let why = blocked(&self.import_path, &entries);
                        let go = kit::primary_button(ui, tr("dlg.imp.go", &[]), why.is_none());
                        // loads a model: a click, never Enter from the path field
                        let go = match &why {
                            Some(w) => go.on_disabled_hover_text(w.as_str()),
                            None => go,
                        };
                        if go.clicked() {
                            act = Some(Act::Go(self.import_path.trim().to_string()));
                        }
                        if kit::cancel_button(ui, tr("dlg.close", &[])).clicked() {
                            act = Some(Act::Close);
                        }
                        if any_untrusted
                            && Button::quiet(&tr("dlg.imp.dev_btn", &[]))
                                .show(ui, &th)
                                .clicked()
                        {
                            act = Some(Act::DevOpen);
                        }
                    }
                    Phase::Run => {
                        let (cancelling, can) = view
                            .as_ref()
                            .map_or((false, false), |v| (v.cancelling, v.cancellable));
                        let label = if cancelling {
                            "job.cancelling"
                        } else {
                            "job.cancel"
                        };
                        let r = ui.add_enabled_ui(can && !cancelling, |ui| {
                            Button::secondary(&tr(label, &[]))
                                .min_width(CANCEL_W)
                                .show(ui, &th)
                        });
                        if r.inner.clicked() {
                            act = Some(Act::Stop);
                        }
                    }
                    Phase::Done => {
                        if kit::primary_action(ui, tr("dlg.imp.done", &[]), None) {
                            act = Some(Act::Close);
                        }
                        if Button::quiet(&tr("dlg.imp.another", &[]))
                            .show(ui, &th)
                            .clicked()
                        {
                            act = Some(Act::Another);
                        }
                    }
                    Phase::Failed => {
                        let retry = kit::primary_button(ui, tr("dlg.imp.retry", &[]), true);
                        if retry.clicked() {
                            act = Some(Act::Go(shown_path.trim().to_string()));
                        }
                        if kit::cancel_button(ui, tr("dlg.close", &[])).clicked() {
                            act = Some(Act::Close);
                        }
                    }
                });
            });
        match act {
            Some(Act::Pick) => {
                let filter = tr("dlg.imp.filter", &[]);
                if let Some(p) = crate::shot_press::guard(|| {
                    crate::file_pick::Dlg::new()
                        .add_filter(filter, drawing_win::import_exts().as_slice())
                        .pick_file()
                }) {
                    self.import_path = p.to_string_lossy().into_owned();
                }
            }
            Some(Act::Go(p)) => {
                self.import_flow.outcome = None;
                self.import_any(Path::new(&p));
            }
            Some(Act::Stop) => {
                if let Some(j) = &self.import_job {
                    j.cancel();
                }
            }
            Some(Act::Close) => open = false,
            Some(Act::Another) => {
                self.import_flow.outcome = None;
                self.import_path.clear();
            }
            Some(Act::DevOpen) => self.import_flow.dev_open = true,
            None => {}
        }
        // import_any may have moved on to another window or reported a failure
        self.import_open = open && self.import_open;
        if !self.import_open {
            self.import_flow.outcome = None;
        }
        if self.import_flow.dev_open {
            self.import_dev_window(ctx);
        }
    }

    /// Trusting a module author's key is its own small window, reached only when a module
    /// failed its signature check.
    fn import_dev_window(&mut self, ctx: &egui::Context) {
        let mut open = self.import_flow.dev_open;
        let (mut ask, mut confirm, mut back) = (false, false, false);
        kit::Dialog::new(
            "import_dev_key",
            tr("dlg.imp.dev", &[]),
            kit::DialogSize::Confirm,
        )
        .open(&mut open)
        .stable()
        .show(ctx, |ui| {
            let th = theme::of(ui.ctx());
            let key_lbl = tr("dlg.imp.key", &[]);
            let key_id = ui.add(egui::Label::new(key_lbl.as_str()).wrap()).id;
            kit::fixed_row(ui, "imp.dev.key", |ui| {
                let w = ui.available_width();
                TextField::new(&mut self.dev_key)
                    .labelled_by(key_id, &key_lbl)
                    .id_salt("imp.dev.field")
                    .width(w)
                    .show(ui, &th);
            });
            ui.add_space(space::SM);
            kit::zone(ui, "imp.dev.note", DEV_ZONE_H, |ui| {
                if let Some(id) = &self.dev_pending {
                    kit::warn_note(ui, tr("dlg.imp.trust_q", &[("id", id)]));
                } else if let Some(n) = &self.import_flow.dev_note {
                    kit::error_note(ui, n);
                } else {
                    ui.add(
                        egui::Label::new(
                            RichText::new(tr("dlg.imp.key_hint", &[]))
                                .color(th.sem().text_secondary),
                        )
                        .wrap(),
                    );
                }
            });
            kit::dialog_footer(ui, |ui| {
                // grants a module the user's rights: a click, never Enter
                if self.dev_pending.is_some() {
                    confirm = kit::primary_button(ui, tr("dlg.imp.confirm", &[]), true).clicked();
                    back = kit::cancel_button(ui, tr("dlg.cancel", &[])).clicked();
                } else {
                    let has = !self.dev_key.trim().is_empty();
                    ask = kit::primary_button(ui, tr("dlg.imp.trust", &[]), has)
                        .on_disabled_hover_text(tr("dlg.imp.key_empty", &[]))
                        .clicked();
                    back = kit::cancel_button(ui, tr("dlg.close", &[])).clicked();
                }
            });
        });
        if ask {
            match crate::import::parse_key(&self.dev_key) {
                Ok(id) => {
                    self.dev_pending = Some(id);
                    self.import_flow.dev_note = None;
                }
                Err(e) => self.import_flow.dev_note = Some(tr("dlg.imp.key_bad", &[("e", &e)])),
            }
        }
        if confirm {
            let key = self.dev_key.clone();
            // the key confirmed is exactly the key shown
            let same = crate::import::parse_key(&key).ok() == self.dev_pending;
            self.dev_pending = None;
            if !same {
                self.import_flow.dev_note = Some(tr("dlg.imp.key_changed", &[]));
            } else {
                match self.bridges.confirm_developer_key(&key) {
                    Ok(id) => {
                        self.ed.status = Some(tr("dlg.imp.key_added", &[("id", &id)]));
                        self.dev_key.clear();
                        self.import_flow.dev_note = None;
                        open = false;
                    }
                    Err(e) => {
                        self.import_flow.dev_note = Some(tr("dlg.imp.key_failed", &[("e", &e)]));
                    }
                }
            }
        }
        if back && self.dev_pending.take().is_none() {
            open = false;
        }
        if !open {
            self.dev_pending = None;
        }
        self.import_flow.dev_open = open;
    }

    /// A module import ended: the open window shows the result, the status line always does.
    pub(super) fn import_finished(
        &mut self,
        input: &Path,
        r: Result<ddc_io::bridge::Report, String>,
        cancelled: bool,
    ) {
        match r {
            Ok(rep) => {
                // the result is the strip under the ribbon, not a window
                self.import_open = false;
                self.import_flow.outcome = None;
                crate::open::summary::show_bridge(self, input, &rep);
            }
            Err(_) if cancelled => self.ed.status = Some(tr("job.cancelled", &[])),
            Err(e) => {
                self.ed.status = Some(tr("dlg.imp.failed_status", &[("e", &e)]));
                self.import_failed(input, e);
            }
        }
    }

    /// A failed import: the strip says so; a window that is already open shows it too.
    pub(super) fn import_failed(&mut self, input: &Path, message: String) {
        self.import_path = input.to_string_lossy().into_owned();
        crate::open::summary::show_failed(self, input, message.clone());
        // a window already open keeps its own failed state; none is opened for the failure
        if self.import_open {
            self.import_flow.outcome = Some(Outcome::Failed {
                path: self.import_path.clone(),
                message,
            });
        }
    }
}

const LONG_PATH: &str = "C:\\Projects\\Residential complex North Park\\Phase 2\\Architecture\\Building A\\Issued for coordination\\North Park - Building A - Architecture - Levels 1-24 - rev 12.rvt";

fn demo_counts(skipped: bool) -> Counts {
    let mut map = std::collections::BTreeMap::new();
    if skipped {
        map.insert("IfcAnnotation".to_string(), 118);
        map.insert("IfcVirtualElement: no axis curve".to_string(), 12);
        map.insert("IfcDistributionPort".to_string(), 3);
    }
    Counts {
        path: LONG_PATH.to_string(),
        native: 2006,
        mesh: 3212,
        skipped: map.values().sum(),
        notes: if skipped { 95 } else { 0 },
        details: skipped_rows(&map),
    }
}

/// `--import-demo STATE`: one state of the import flow for a screenshot. Every state has a long
/// path or name, so a builder sees the elision and the frame at once.
pub fn demo(app: &mut EditorApp, state: &str) -> Result<(), String> {
    use ddc_io::bridge::Bridge;
    // "pick:ru" also picks the language, since both have to fit
    let (state, lang) = state.split_once(':').unwrap_or((state, ""));
    match lang {
        "ru" => app.sh.cat.lang = ddc_ui::Lang::Ru,
        "en" => app.sh.cat.lang = ddc_ui::Lang::En,
        _ => {}
    }
    crate::set_ui_lang(app.sh.cat.lang);
    let module = |trusted: bool| Bridge {
        name: "module".into(),
        version: "1".into(),
        dir: "module".into(),
        formats: vec!["rvt".into(), "rfa".into()],
        trust: if trusted {
            Ok(String::new())
        } else {
            Err("signature".into())
        },
    };
    app.bridges.list.clear();
    app.import_open = true;
    // the start card of an empty model would cover the window under test
    app.chrome.start.open = false;
    app.welcome_closed = true;
    match state {
        "pick" | "no-module" => {}
        "pick-file" => {
            app.bridges.list = vec![module(true)];
            app.import_path = LONG_PATH.to_string();
        }
        "pick-unsupported" => app.import_path = "C:\\Projects\\notes.txt".to_string(),
        "untrusted" => {
            app.bridges.list = vec![module(false)];
            app.import_path = LONG_PATH.to_string();
        }
        "run" | "run-wait" | "cancelling" | "task" => {
            app.import_path = LONG_PATH.to_string();
            let done = if state == "run-wait" {
                (0, 0)
            } else {
                (42, 100)
            };
            let job = crate::import::Job::demo(LONG_PATH.into(), done);
            if state == "cancelling" {
                job.cancel();
            }
            app.import_job = Some(job);
            if state == "task" {
                app.import_open = false;
                app.chrome.tasks.expanded = true;
            }
        }
        "done" | "done-skipped" => {
            app.import_path = LONG_PATH.to_string();
            app.import_flow.outcome = Some(Outcome::Done(demo_counts(state == "done-skipped")));
        }
        "failed" => {
            app.import_path = LONG_PATH.to_string();
            app.import_flow.outcome = Some(Outcome::Failed {
                path: LONG_PATH.to_string(),
                message: tr("dlg.imp.demo_failure", &[]),
            });
        }
        "dev-key" | "dev-key-confirm" | "dev-key-bad" => {
            app.bridges.list = vec![module(false)];
            app.import_flow.dev_open = true;
            app.dev_key = "demo-key".to_string();
            match state {
                "dev-key-confirm" => app.dev_pending = Some("0123456789ABCDEF".to_string()),
                "dev-key-bad" => {
                    app.import_flow.dev_note = Some(tr("dlg.imp.key_bad", &[("e", "base64")]));
                }
                _ => {}
            }
        }
        "drawing" | "drawing-files" | "drawing-error" => {
            app.import_open = false;
            drawing_win::demo_import_form(app, state);
        }
        "links" => {
            app.import_open = false;
            app.links.open = true;
        }
        "sources" => {
            app.import_open = false;
            app.src.panel = true;
        }
        "conflicts" => {
            app.import_open = false;
            app.src.conflicts = true;
        }
        "topo" => {
            app.import_open = false;
            crate::topo::demo_import(&mut app.ed, LONG_PATH);
        }
        other => return Err(format!("--import-demo: unknown state {other}")),
    }
    Ok(())
}

/// Every state `demo` knows, for the frame test and for builders.
pub const DEMO_STATES: &[&str] = &[
    "pick",
    "no-module",
    "pick-file",
    "pick-unsupported",
    "untrusted",
    "run",
    "run-wait",
    "cancelling",
    "done",
    "done-skipped",
    "failed",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::ui_kit::DialogSize;

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    fn frame_size(
        a: &mut EditorApp,
        id: egui::Id,
        draw: fn(&mut EditorApp, &egui::Context),
    ) -> egui::Vec2 {
        let ctx = egui::Context::default();
        // titles use the semibold family: without the fonts text layout panics
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        for _ in 0..4 {
            let _ = ctx.run(Default::default(), |c| draw(a, c));
        }
        egui::AreaState::load(&ctx, id)
            .and_then(|s| s.size)
            .expect("window was drawn")
    }

    #[test]
    fn import_summary_counts_stairs() {
        let r = ddc_io::ifc::ImportReport {
            stairs: 2,
            ramps: 1,
            railings: 3,
            ..Default::default()
        };
        let c = counts_from_ifc(Path::new("a.ifc"), &r);
        assert_eq!(c.native, 6);
    }

    /// The owner's complaint in one test: no state of the Import window changes its frame.
    #[test]
    fn every_state_of_the_import_window_has_one_frame() {
        let id = egui::Id::new(("ddc.dialog", "import"));
        let mut sizes = vec![];
        for st in DEMO_STATES {
            let mut a = app();
            demo(&mut a, st).unwrap();
            sizes.push((*st, frame_size(&mut a, id, |a, c| a.import_window(c))));
        }
        let first = sizes[0].1;
        assert!(first.x >= DialogSize::Form.size().x);
        for (st, s) in &sizes {
            assert_eq!(*s, first, "state {st}");
        }
    }

    #[test]
    fn the_drawing_form_keeps_its_frame_with_files_and_errors() {
        let id = egui::Id::new(("ddc.dialog", "drawing_import"));
        let mut sizes = vec![];
        for st in ["drawing", "drawing-files", "drawing-error"] {
            let mut a = app();
            demo(&mut a, st).unwrap();
            sizes.push(frame_size(&mut a, id, |a, c| a.drawing_import_window(c)));
        }
        assert_eq!(sizes[0], sizes[1]);
        assert_eq!(sizes[0], sizes[2]);
    }

    #[test]
    fn unknown_demo_state_is_an_error() {
        let mut a = app();
        assert!(demo(&mut a, "nope").is_err());
    }

    fn entry(dir: Option<&str>, untrusted: bool) -> Entry {
        Entry {
            label: ".rvt/.rfa".into(),
            formats: vec!["rvt".into(), "rfa".into()],
            dir: dir.map(Into::into),
            note: None,
            untrusted,
        }
    }

    #[test]
    fn blocked_names_the_reason() {
        let ok = [entry(Some("m"), false)];
        assert!(blocked("", &ok).is_some());
        assert!(blocked("a.rvt", &ok).is_none());
        assert!(blocked("a.ifc", &ok).is_none());
        assert!(blocked("a.txt", &ok).is_some());
        let missing = blocked("a.rvt", &[entry(None, false)]).unwrap();
        let untrusted = blocked("a.rvt", &[entry(None, true)]).unwrap();
        assert_ne!(missing, untrusted);
    }

    #[test]
    fn counts_split_native_mesh_skipped() {
        let mut r = ddc_io::ifc::ImportReport {
            walls: 3,
            openings: 2,
            levels: 1,
            meshes: 5,
            ..Default::default()
        };
        r.skipped.insert("IfcAnnotation".into(), 4);
        let c = counts_from_ifc(Path::new("x/House.ifc"), &r);
        assert_eq!((c.native, c.mesh, c.skipped), (5, 5, 4));
        assert_eq!(c.details.len(), 1);
    }

    #[test]
    fn long_skip_lists_fold_into_one_row() {
        let map: std::collections::BTreeMap<String, usize> =
            (0..30).map(|k| (format!("T{k}"), k)).collect();
        let rows = skipped_rows(&map);
        assert_eq!(rows.len(), DETAILS + 1);
        assert!(rows[0].contains("T29"));
    }

    /// Fails when an import window leaves the scaffold or its frame follows its content again.
    const IMPORT_WINDOWS: &[(&str, &str)] = &[
        ("shell/import_win.rs", "import"),
        ("shell/import_win.rs", "import_dev_key"),
        ("shell/drawing_win.rs", "drawing_import"),
        ("shell/drawing_win.rs", "drawing_layers"),
        ("shell/drawing_win.rs", "drawing_recognize"),
        ("shell/xlsx_win.rs", "xlsx_preview"),
        ("shell/xlsx_win.rs", "xlsx_link"),
        ("shell/tasks_ui.rs", "ddc_task"),
        ("project_ui.rs", "project_job"),
        ("links_ui.rs", "links"),
        ("source_ui.rs", "sources"),
        ("source_ui.rs", "source_conflicts"),
        ("topo/window.rs", "topo_import"),
    ];

    /// The builder chain of a window: from its key to `.show(`.
    fn chain<'s>(src: &'s str, key: &str) -> Option<&'s str> {
        let quoted = format!("\"{key}\"");
        let at = src.match_indices(&quoted).map(|(i, _)| i).find(|i| {
            let from = i.saturating_sub(48);
            src.get(from..*i).is_some_and(|b| b.contains("Dialog::"))
        })?;
        let rest = &src[at..];
        Some(&rest[..rest.find(".show(")?])
    }

    #[test]
    fn import_windows_use_the_stable_scaffold() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut bad = vec![];
        for (file, key) in IMPORT_WINDOWS {
            let src = std::fs::read_to_string(root.join(file)).unwrap();
            if src.contains(concat!("egui::", "Window", "::new")) {
                bad.push(format!("{file}: raw window"));
            }
            match chain(&src, key) {
                None => bad.push(format!("{file}: no window {key}")),
                Some(c) => {
                    if !c.contains(".stable()") {
                        bad.push(format!("{key}: not stable"));
                    }
                    // a number here is a width class of its own
                    if let Some(i) = c.find(".size(") {
                        if !c[i..].contains("layout::") {
                            bad.push(format!("{key}: literal size"));
                        }
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// A new window with "import" in its key has to join the list above.
    #[test]
    fn every_import_keyed_window_is_listed() {
        fn walk(d: &Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let s = std::fs::read_to_string(&p).unwrap();
                    for part in s.split(concat!("Dialog", "::new(\"")).skip(1) {
                        if let Some(end) = part.find('"') {
                            out.push((p.display().to_string(), part[..end].to_string()));
                        }
                    }
                }
            }
        }
        let mut found = vec![];
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        for (file, key) in found {
            if key.contains("import") {
                assert!(
                    IMPORT_WINDOWS.iter().any(|(_, k)| *k == key),
                    "{key} in {file} is not in IMPORT_WINDOWS"
                );
            }
        }
    }

    /// Window text comes from the catalog, in both languages.
    #[test]
    fn this_file_has_no_cyrillic_and_all_keys_exist() {
        let src = include_str!("import_win.rs");
        assert!(src.chars().all(|c| !('\u{400}'..='\u{4ff}').contains(&c)));
        let cats = [
            ddc_ui::Catalog::new(ddc_ui::Lang::Ru),
            ddc_ui::Catalog::new(ddc_ui::Lang::En),
        ];
        let is_key = |k: &&str| k.len() > 8 && k.starts_with("dlg.imp.") && !k.contains(' ');
        for key in src.split('"').filter(is_key) {
            for c in &cats {
                assert_ne!(c.t(key), key, "{key}");
            }
        }
    }
}
