//! Data utilities: glTF export, project information with coordinates, purge unused;
//! DXF export of the edited drawing.
//! Model edits go through `Editor::run_op`: one window action = one undo step.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_model::{Model, ProjectInfo, PurgeItem, SiteProps};
use ddc_ui::theme::Role;
use ddc_ui::{widgets, Lang};

use crate::editor::{Editor, OpFn};
use crate::jobs::{Job, Poll, Progress, CANCELLED};
use crate::shell::EditorApp;
use crate::ui_kit;
use crate::units::Units;
use crate::unsaved::{self, Choice, Switch};

/// Whole dialog sizes, in control heights (the theme's density decides the pixels).
const FIELD_WIDTH: f32 = 10.0;
const LABEL_WIDTH: f32 = 6.0;
const LIST_HEIGHT: f32 = 11.0;

#[derive(Default)]
pub struct State {
    pub(crate) purge: Option<Vec<(PurgeItem, bool)>>,
    plan: Option<PlanMemo>,
    /// STL/OBJ export options, while the dialog is up.
    pub mesh: Option<crate::mesh_io::Dlg>,
}

/// The purge plan size for one model state and one set of ticks.
struct PlanMemo {
    key: ((u64, u64), Vec<(&'static str, i64)>),
    n: usize,
}

struct InfoBuf {
    info: ProjectInfo,
    site: SiteProps,
    /// The close question is open; the typed values stay until it is answered.
    asking: bool,
    /// The question was drawn before this frame (its opening Esc is not an answer).
    asked_before: bool,
    /// Stored values at a model revision: the per-frame draft check reads the model once.
    seen: Option<((u64, u64), ProjectInfo, SiteProps)>,
    /// Size of the company logo at a model revision, for the line under the fields.
    logo: Option<((u64, u64), Option<(u32, u32)>)>,
}

thread_local! {
    /// The open Project information window: its typed values are a draft until applied, so
    /// Close, New and Open ask about them through `project_ui::drafts`.
    static INFO: RefCell<Option<InfoBuf>> = const { RefCell::new(None) };
}

fn with_info<R>(f: impl FnOnce(&mut Option<InfoBuf>) -> R) -> R {
    INFO.with(|s| f(&mut s.borrow_mut()))
}

pub(crate) fn info_open() -> bool {
    with_info(|s| s.is_some())
}

/// The window holds typed values the model lacks (reads the model; for the question's list).
pub(crate) fn info_draft(m: &Model) -> bool {
    INFO.with(|s| {
        let Ok(s) = s.try_borrow() else { return false };
        s.as_ref().is_some_and(|draft| info_changed(m, draft))
    })
}

/// `info_draft` asked every frame: the stored values are read once per `Editor::cache_key`.
pub(crate) fn info_draft_at(m: &Model, key: (u64, u64)) -> bool {
    INFO.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return false };
        let Some(draft) = s.as_mut() else { return false };
        if draft.seen.as_ref().map(|x| x.0) != Some(key) {
            let info = m.project_info().unwrap_or_default().trimmed();
            draft.seen = Some((key, info, m.site().unwrap_or_default()));
        }
        let Some((_, info, site)) = draft.seen.as_ref() else { return false };
        draft.info.trimmed() != *info || draft.site != *site
    })
}

/// Discard on the project question: the window and its typed values go.
pub(crate) fn discard_drafts() {
    with_info(|s| *s = None);
}

/// Save on the project question: applies the typed values in one step as Apply does. A refusal
/// keeps the window with everything typed and its question closed.
pub(crate) fn save_drafts(ed: &mut Editor) -> bool {
    let Some(mut draft) = with_info(Option::take) else {
        return true;
    };
    if !info_changed(&ed.model, &draft) || apply_info(ed, &draft).is_ok() {
        return true;
    }
    (draft.asking, draft.asked_before) = (false, false);
    with_info(|s| *s = Some(draft));
    false
}

/// A new model (open, new, snapshot restore): typed values of the old one must not carry over.
pub(crate) fn on_model_reset() {
    with_info(|s| *s = None);
}

// below the draft functions: the drafts contract scans production code up to the first
// test-only item
#[cfg(test)]
thread_local! {
    static PLAN_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Opens the window with the stored values and the project name changed.
#[cfg(test)]
pub(crate) fn seed_draft_for_test(ed: &Editor) {
    let m = &ed.model;
    let mut info = m.project_info().unwrap_or_default();
    info.name = "Zz Info".into();
    let site = m.site().unwrap_or_default();
    let draft = InfoBuf {
        info,
        site,
        asking: false,
        asked_before: false,
        seen: None,
        logo: None,
    };
    with_info(|s| *s = Some(draft));
}

/// Site limit as `Model::set_site` checks it; lengths are metres in the model.
const SITE_LIMIT: f64 = 1e9;

/// Site rows: key, value, plain (step, decimals) or `None` for a length in project units, limit.
type SiteRow<'a> = (&'static str, &'a mut f64, Option<(f64, usize)>, f64);

fn site_rows(s: &mut SiteProps) -> [SiteRow<'_>; 6] {
    [
        ("north", &mut s.north_deg, Some((0.1, 2)), SITE_LIMIT),
        ("east", &mut s.base.0, None, SITE_LIMIT),
        ("northing", &mut s.base.1, None, SITE_LIMIT),
        ("elevation", &mut s.base.2, None, SITE_LIMIT),
        ("lat", &mut s.latitude, Some((0.0001, 6)), 90.0),
        ("lon", &mut s.longitude, Some((0.0001, 6)), 180.0),
    ]
}

/// Unit column of a site length: millimetres in metric, none in imperial (the text has ' and ").
fn site_len_unit(u: Units, ru: bool) -> &'static str {
    match u {
        Units::Metric => crate::tr_str(ru, "unit_fmt.unit.mm"),
        Units::Imperial => "",
    }
}

/// The typed values differ from the model (what Apply would write).
fn info_changed(m: &Model, b: &InfoBuf) -> bool {
    b.info.trimmed() != m.project_info().unwrap_or_default().trimmed()
        || b.site != m.site().unwrap_or_default()
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.export_gltf", export_gltf);
    crate::mesh_io::register(a);
    a.register_command("mgr.project_info", open_info);
    a.register_command("mgr.coords", open_info);
    a.register_command("mgr.purge", open_purge);
}

fn status(a: &mut EditorApp, key: &str, args: &[(&str, &str)]) {
    a.ed.status = Some(a.sh.cat.tf(key, args));
}

/// File name from a project name: no path separators or characters the file system refuses.
pub(crate) fn safe_stem(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_end_matches('.').trim_end();
    if s.is_empty() {
        "model".to_string()
    } else {
        s.to_string()
    }
}

/// Appends `.glb` unless it is there: a typed `Haus 7.2` must not lose its `.2`.
fn glb_path(p: PathBuf) -> PathBuf {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("glb")) {
        return p;
    }
    let mut s = p.into_os_string();
    s.push(".glb");
    s.into()
}

fn export_gltf(a: &mut EditorApp) {
    let name =
        a.ed.model
            .project_info()
            .map(|p| p.name)
            .unwrap_or_default();
    let stem = safe_stem(&name);
    let Some(p) = crate::file_pick::Dlg::new()
        .set_title(a.sh.cat.t("data_export.gltf.title"))
        .add_filter("glTF", &["glb"])
        .set_file_name(format!("{stem}.glb"))
        .save_file()
    else {
        return;
    };
    let path = glb_path(p);
    let title = if name.is_empty() {
        stem.as_str()
    } else {
        name.as_str()
    };
    let e = match crate::gltf_export::build(&mut a.ed, title) {
        Ok(e) => e,
        Err(e) => return status(a, "data_export.gltf.failed", &[("error", &e)]),
    };
    if e.gltf.nodes.is_empty() {
        return status(a, "data_export.gltf.empty", &[]);
    }
    for i in &e.issues {
        eprintln!("glTF: {i}");
    }
    let shown = path.display().to_string();
    match ddc_io::gltf::write_glb_file(&e.gltf, &path) {
        Ok(()) if e.issues.is_empty() => status(
            a,
            "data_export.gltf.saved",
            &[
                ("path", &shown),
                ("nodes", &e.gltf.nodes.len().to_string()),
                ("meshes", &e.gltf.meshes.len().to_string()),
            ],
        ),
        Ok(()) => status(
            a,
            "data_export.gltf.partial",
            &[("path", &shown), ("n", &e.issues.len().to_string())],
        ),
        Err(err) => status(a, "data_export.gltf.failed", &[("error", &err)]),
    }
}

fn open_info(a: &mut EditorApp) {
    let m = &a.ed.model;
    let buf = InfoBuf {
        info: m.project_info().unwrap_or_default(),
        site: m.site().unwrap_or_default(),
        asking: false,
        asked_before: false,
        seen: None,
        logo: None,
    };
    with_info(|s| *s = Some(buf));
}

fn open_purge(a: &mut EditorApp) {
    match a.ed.model.purge_candidates() {
        // nothing is ticked: the red button never offers a purge nobody asked for
        Ok(v) => a.data.purge = Some(v.into_iter().map(|i| (i, false)).collect()),
        Err(e) => status(a, "data_export.purge.failed", &[("error", &e.to_string())]),
    }
}

/// Layout probes: opens "info", "purge" (two spare types to remove) or "none" (empty purge list).
#[cfg(test)]
pub(crate) fn open_for_layout(a: &mut EditorApp, which: &str) {
    let layer = [ddc_model::LayerSpec {
        material: "m".into(),
        thickness: 0.1,
    }];
    match which {
        "info" => open_info(a),
        "purge" => {
            a.ed.model.add_wall_type("Spare A", &layer).unwrap();
            a.ed.model.add_wall_type("Spare B", &layer).unwrap();
            open_purge(a);
        }
        _ => a.data.purge = Some(Vec::new()),
    }
}

/// Frame hook: draws whichever windows are open.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    if info_open() {
        info_window(a, ctx);
    }
    if a.data.purge.is_some() {
        purge_window(a, ctx);
    }
    if a.data.mesh.is_some() {
        crate::mesh_io::window(a, ctx);
    }
}

fn info_window(a: &mut EditorApp, ctx: &egui::Context) {
    let (cat, th) = (&a.sh.cat, &a.sh.theme);
    // out of the cell while drawn: nothing in the frame may borrow it twice
    let Some(mut buf) = with_info(Option::take) else {
        return;
    };
    let ru = cat.lang == Lang::Ru;
    let u = a.ed.tool.units;
    let (asking, asked_before) = (buf.asking, buf.asked_before);
    if asking {
        // this frame's Esc answers the question, it must not close the window under it
        unsaved::hold_esc(ctx);
    }
    let (mut open, mut apply, mut cancel) = (true, false, false);
    let mut open_logo = false;
    let key = a.ed.cache_key();
    if buf.logo.map(|x| x.0) != Some(key) {
        let size = a.ed.model.project_logo().ok().flatten().and_then(|b| ddc_model::png_size(&b));
        buf.logo = Some((key, size));
    }
    let logo_size = buf.logo.and_then(|x| x.1);
    ui_kit::Dialog::new("data_export.info", cat.t("data_export.info.title"), ui_kit::DialogSize::Form)
        .id(egui::Id::new("ddc.f7.info"))
        .open(&mut open)
        .show(ctx, |ui| {
            ui_kit::section_title(ui, cat.t("data_export.info.project"));
            let control = th.d().control;
            for k in buf.info.fields().map(|f| f.0) {
                let Some(s) = buf.info.slot(k) else { continue };
                let name = cat.t(&format!("data_export.info.{k}"));
                ui.horizontal(|ui| {
                    let label = ui
                        .allocate_ui(egui::vec2(control * LABEL_WIDTH, control), |ui| {
                            widgets::elided_label(
                                ui,
                                &name,
                                th.font(Role::Label),
                                th.sem().text_secondary,
                            )
                        })
                        .inner;
                    widgets::TextField::new(s)
                        .id_salt(("data_export.info", k))
                        .width(control * FIELD_WIDTH)
                        .labelled_by(label.id, &name)
                        .show(ui, th);
                });
            }
            ui.horizontal(|ui| {
                let name = cat.t("branding.title");
                ui.allocate_ui(egui::vec2(control * LABEL_WIDTH, control), |ui| {
                    widgets::elided_label(ui, &name, th.font(Role::Label), th.sem().text_secondary)
                });
                let status = match logo_size {
                    Some((w, h)) => {
                        cat.tf("branding.info.have", &[("w", &w.to_string()), ("h", &h.to_string())])
                    }
                    None => cat.t("branding.info.none"),
                };
                ui.allocate_ui(egui::vec2(control * FIELD_WIDTH, control), |ui| {
                    widgets::elided_label(ui, &status, th.font(Role::Label), th.sem().text_primary)
                });
            });
            if widgets::Button::secondary(&cat.t("branding.info.open")).small().show(ui, th).clicked() {
                open_logo = true;
            }
            ui.separator();
            ui_kit::section_title(ui, cat.t("data_export.info.site"));
            let deg = cat.t("data_export.unit.deg");
            ui_kit::props(ui, "f7.site.grid", |ui| {
                for (k, v, plain, limit) in site_rows(&mut buf.site) {
                    // typed values stay in the buffer (metres): Apply writes them in one step
                    let (kind, unit) = match plain {
                        Some((step, decimals)) => {
                            (ui_kit::NumKind::Plain { decimals, step }, deg.as_str())
                        }
                        None => (ui_kit::NumKind::Len(u), site_len_unit(u, ru)),
                    };
                    let salt = egui::Id::new(("f7.site", k));
                    let name = cat.t(&format!("data_export.info.{k}"));
                    ui_kit::kv_field_lang(ui, salt, &name, v, kind, -limit, limit, unit, ru);
                }
            });
            ui_kit::dialog_footer(ui, |ui| {
                apply = ui_kit::primary_action(ui, cat.t("data_export.info.apply"), None);
                cancel = ui_kit::cancel_button(ui, cat.t("dlg.cancel")).clicked();
            });
        });
    with_info(|s| *s = Some(buf));
    let answer = if asking {
        let title = cat.t("data_export.info.title");
        let body = cat.t("project_info.info.unsaved_body");
        let apply_label = cat.t("data_export.info.apply");
        unsaved::ask(ctx, ru, "f7.info.unsaved", &title, &body, &apply_label, asked_before)
    } else {
        None
    };
    settle_info(a, apply, !open || cancel, answer);
    if open_logo {
        a.open_settings(crate::shell::settings_win::BRANDING_PAGE);
    }
}

/// After a frame of the window: `apply` its Apply, `close` its X, Esc or Cancel, `answer` the
/// close question. A close with typed edits asks first (Cancel keeps the window and the text).
fn settle_info(a: &mut EditorApp, apply: bool, close: bool, answer: Option<Choice>) {
    let Some(mut b) = with_info(Option::take) else {
        return;
    };
    let write = if b.asking {
        match answer {
            Some(Choice::Apply) => true,
            Some(Choice::Discard) => return,
            Some(Choice::Cancel) => {
                (b.asking, b.asked_before) = (false, false);
                false
            }
            None => {
                b.asked_before = true;
                false
            }
        }
    } else if apply {
        true
    } else if close {
        match unsaved::decide_switch(info_changed(&a.ed.model, &b), None) {
            Switch::Ask => (b.asking, b.asked_before) = (true, false),
            _ => return,
        }
        false
    } else {
        false
    };
    // a refused value keeps the window with everything typed
    if write {
        if save_info(a, &b) {
            return;
        }
        (b.asking, b.asked_before) = (false, false);
    }
    with_info(|s| *s = Some(b));
}

/// Fields and site in one step, none when nothing differs. `false` — the model refused.
fn save_info(a: &mut EditorApp, b: &InfoBuf) -> bool {
    match apply_info(&mut a.ed, b) {
        Ok(false) => {
            status(a, "data_export.info.unchanged", &[]);
            true
        }
        Ok(true) => {
            status(a, "data_export.info.saved", &[]);
            true
        }
        Err(e) => {
            status(a, "data_export.info.failed", &[("error", &e.to_string())]);
            false
        }
    }
}

/// Writes the typed values in one undo step; `Ok(false)` when nothing differs.
fn apply_info(ed: &mut Editor, b: &InfoBuf) -> Result<bool, ddc_model::Error> {
    let info = b.info.trimmed();
    let site = b.site.clone();
    let m = &ed.model;
    if info == m.project_info().unwrap_or_default() && site == m.site().unwrap_or_default() {
        return Ok(false);
    }
    let f = OpFn(Arc::new(move |m: &mut Model| {
        m.set_project_data(&info, &site).map(|_| ())
    }));
    ed.run_op(f).map(|()| true)
}

/// Keeps the ticks consistent after one was changed: a family goes only with all its types,
/// so unticking a type unticks its family and ticking a family ticks its types.
fn link_ticks(items: &mut [(PurgeItem, bool)], changed: &PurgeItem, on: bool) {
    for (item, tick) in items.iter_mut() {
        let family_of_type = changed.table == "comp_type"
            && !on
            && item.table == "comp_family"
            && Some(item.id) == changed.family;
        let type_of_family = changed.table == "comp_family"
            && on
            && item.table == "comp_type"
            && item.family == Some(changed.id);
        if family_of_type {
            *tick = false;
        } else if type_of_family {
            *tick = true;
        }
    }
}

/// What a purge of the ticked rows removes: `purge_plan`, so the count never promises more than
/// the model does. Falls back to the tick count when the model cannot say.
fn planned(model: &Model, items: &[(PurgeItem, bool)]) -> usize {
    #[cfg(test)]
    PLAN_QUERIES.with(|c| c.set(c.get() + 1));
    let ticked: Vec<PurgeItem> = items.iter().filter(|i| i.1).map(|i| i.0.clone()).collect();
    model.purge_plan(&ticked).map_or(ticked.len(), |p| p.len())
}

/// `planned`, asked of the model only when the ticks or the model state changed.
fn planned_cached(
    memo: &mut Option<PlanMemo>,
    model_key: (u64, u64),
    model: &Model,
    items: &[(PurgeItem, bool)],
) -> usize {
    let ticks: Vec<(&'static str, i64)> =
        items.iter().filter(|i| i.1).map(|i| (i.0.table, i.0.id)).collect();
    let key = (model_key, ticks);
    if let Some(m) = memo.as_ref().filter(|m| m.key == key) {
        return m.n;
    }
    let n = planned(model, items);
    *memo = Some(PlanMemo { key, n });
    n
}

fn purge_window(a: &mut EditorApp, ctx: &egui::Context) {
    let model_key = a.ed.cache_key();
    let (cat, th, model) = (&a.sh.cat, &a.sh.theme, &a.ed.model);
    let Some(items) = a.data.purge.as_mut() else {
        return;
    };
    let memo = &mut a.data.plan;
    let control = th.d().control;
    let (mut open, mut run, mut cancel) = (true, false, false);
    ui_kit::Dialog::new("data_export.purge", cat.t("data_export.purge.title"), ui_kit::DialogSize::Tool)
        .id(egui::Id::new("ddc.f7.purge"))
        .open(&mut open)
        .show(ctx, |ui| {
            if items.is_empty() {
                ui_kit::empty_note(ui, cat.t("data_export.purge.none"), None);
                ui_kit::dialog_footer(ui, |ui| {
                    cancel = ui_kit::cancel_button(ui, cat.t("dlg.close")).clicked();
                });
                return;
            }
            let quiet = |t: String| {
                egui::RichText::new(t)
                    .font(th.font(Role::Caption))
                    .color(th.sem().text_secondary)
            };
            ui.label(quiet(cat.t("data_export.purge.hint")));
            ui.horizontal(|ui| {
                if widgets::Button::quiet(&cat.t("data_export.purge.all")).show(ui, th).clicked() {
                    items.iter_mut().for_each(|i| i.1 = true);
                }
                if widgets::Button::quiet(&cat.t("data_export.purge.clear")).show(ui, th).clicked() {
                    items.iter_mut().for_each(|i| i.1 = false);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(control * LIST_HEIGHT)
                .show(ui, |ui| {
                    let mut last = "";
                    let mut changed = None;
                    for (item, on) in items.iter_mut() {
                        if item.table != last {
                            last = item.table;
                            ui_kit::section_title(ui, cat.t(&format!("data_export.purge.kind.{}", item.table)));
                        }
                        let was = *on;
                        // named by the item, so a screen reader reads the name and the state
                        let shown = crate::fam_label::purge_item(cat.lang, item.table, &item.name);
                        widgets::checkbox(ui, th, on, &shown).on_hover_text(&shown);
                        if *on != was {
                            changed = Some((item.clone(), *on));
                        }
                    }
                    if let Some((item, on)) = changed {
                        link_ticks(items, &item, on);
                    }
                });
            let ticked = items.iter().filter(|i| i.1).count();
            let n = planned_cached(memo, model_key, model, items);
            if n < ticked {
                let kept = (ticked - n).to_string();
                ui.label(quiet(cat.tf("data_export.purge.kept", &[("n", &kept)])));
            }
            ui.label(quiet(cat.t("cmd_buttons.purge.undo")));
            // destructive: distinct danger button, never on Enter
            ui_kit::dialog_footer(ui, |ui| {
                let label = cat.tf("data_export.purge.run", &[("n", &n.to_string())]);
                run = ui_kit::confirm_danger_button(ui, label, n > 0).clicked();
                cancel = ui_kit::cancel_button(ui, cat.t("dlg.cancel")).clicked();
            });
        });
    if run {
        let chosen: Vec<PurgeItem> = a
            .data
            .purge
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.1)
            .map(|i| i.0)
            .collect();
        purge(a, chosen);
    } else if !open || cancel {
        a.data.purge = None;
    }
}

/// All chosen items in one model operation: one undo step. `run_op` keeps a step per call, so
/// a choice that removes nothing must not reach it.
pub fn purge(a: &mut EditorApp, items: Vec<PurgeItem>) {
    let items = match a.ed.model.purge_plan(&items) {
        Ok(p) if p.is_empty() => return status(a, "data_export.purge.none", &[]),
        Ok(p) => p,
        Err(e) => return status(a, "data_export.purge.failed", &[("error", &e.to_string())]),
    };
    let n = items.len();
    let f = OpFn(Arc::new(move |m: &mut Model| {
        m.purge_unused(&items).map(|_| ())
    }));
    match a.ed.run_op(f) {
        Ok(()) => status(a, "data_export.purge.done", &[("n", &n.to_string())]),
        Err(e) => status(a, "data_export.purge.failed", &[("error", &e.to_string())]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> EditorApp {
        EditorApp::new(
            crate::editor::Editor::new_in_memory().unwrap(),
            None,
            "test".into(),
        )
    }

    fn layer() -> [ddc_model::LayerSpec; 1] {
        [ddc_model::LayerSpec {
            material: "m".into(),
            thickness: 0.1,
        }]
    }

    #[test]
    fn purge_is_one_undo_step() {
        let mut a = app();
        a.ed.model.add_wall_type("spare", &layer()).unwrap();
        a.ed.model.add_wall_type("spare 2", &layer()).unwrap();
        let items = a.ed.model.purge_candidates().unwrap();
        let n = items.len();
        assert!(n >= 1);
        let steps = a.ed.undo_len();
        purge(&mut a, items);
        assert_eq!(a.ed.undo_len(), steps + 1);
        // what only the purged types used (their materials) is unused now: a second round
        let left = a.ed.model.purge_candidates().unwrap();
        assert!(left.iter().all(|i| i.table == "material"), "{left:?}");
        a.ed.undo();
        assert_eq!(a.ed.model.purge_candidates().unwrap().len(), n);
    }

    #[test]
    fn a_purge_that_removes_nothing_adds_no_step() {
        let mut a = app();
        let fam =
            a.ed.model
                .load_family(ddc_model::library_sources()[0])
                .unwrap();
        let family: Vec<PurgeItem> =
            a.ed.model
                .purge_candidates()
                .unwrap()
                .into_iter()
                .filter(|i| i.table == "comp_family" && i.id == fam)
                .collect();
        assert_eq!(family.len(), 1);
        let steps = a.ed.undo_len();
        purge(&mut a, family);
        assert_eq!(a.ed.undo_len(), steps);
        assert!(!a.ed.model.comp_types(fam).unwrap().is_empty());
    }

    #[test]
    fn ticks_keep_a_family_with_its_unticked_type() {
        let item = |table, id, family| PurgeItem {
            table,
            id,
            name: String::new(),
            family,
        };
        let (ty, other, fam, wall) = (
            item("comp_type", 1, Some(7)),
            item("comp_type", 2, Some(8)),
            item("comp_family", 7, None),
            item("wall_type", 7, None),
        );
        let mut v = vec![
            (ty.clone(), true),
            (other, true),
            (fam.clone(), true),
            (wall, true),
        ];
        link_ticks(&mut v, &ty, false);
        v[0].1 = false;
        assert_eq!(
            v.iter().map(|i| i.1).collect::<Vec<_>>(),
            [false, true, false, true]
        );
        link_ticks(&mut v, &fam, true);
        v[2].1 = true;
        assert_eq!(
            v.iter().map(|i| i.1).collect::<Vec<_>>(),
            [true, true, true, true]
        );
    }

    #[test]
    fn apply_is_one_undo_step_and_nothing_changed_is_none() {
        let mut a = app();
        open_info(&mut a);
        let steps = a.ed.undo_len();
        let b = take_info().unwrap();
        assert!(save_info(&mut a, &b));
        assert_eq!(a.ed.undo_len(), steps);
        open_info(&mut a);
        let mut b = take_info().unwrap();
        b.info.name = "Haus".into();
        b.site.north_deg = 20.0;
        assert!(save_info(&mut a, &b));
        assert_eq!(a.ed.undo_len(), steps + 1);
        assert_eq!(a.ed.model.project_info().unwrap().name, "Haus");
        assert_eq!(a.ed.model.site().unwrap().north_deg, 20.0);
        a.ed.undo();
        assert_eq!(a.ed.model.project_info().unwrap().name, "");
        assert_eq!(a.ed.model.site().unwrap().north_deg, 0.0);
    }

    #[test]
    fn a_refused_value_adds_no_step_and_keeps_the_text() {
        let mut a = app();
        open_info(&mut a);
        let mut b = take_info().unwrap();
        b.info.name = "x".repeat(501);
        let steps = a.ed.undo_len();
        assert!(!save_info(&mut a, &b));
        assert_eq!(a.ed.undo_len(), steps);
        assert_eq!(b.info.name.len(), 501);
    }

    use crate::units::DecimalMark;

    fn take_info() -> Option<InfoBuf> {
        with_info(Option::take)
    }

    /// Reads the open window; panics with `why` when it is closed.
    fn peek<R>(why: &str, f: impl FnOnce(&InfoBuf) -> R) -> R {
        with_info(|s| f(s.as_ref().expect(why)))
    }

    /// What a site length field stores from `text` (the window's own limits and parser).
    fn typed(text: &str, u: Units, ru: bool) -> Result<f64, crate::units::InputError> {
        ui_kit::len_commit(text, u, -SITE_LIMIT, SITE_LIMIT, DecimalMark::for_ru(ru))
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn site_base_point_is_a_length_and_angles_stay_degrees() {
        let mut s = SiteProps::default();
        let lens: Vec<&str> = site_rows(&mut s)
            .into_iter()
            .filter(|r| r.2.is_none())
            .map(|r| r.0)
            .collect();
        assert_eq!(lens, ["east", "northing", "elevation"]);
        assert_eq!(site_len_unit(Units::Imperial, false), "");
        assert_eq!(site_len_unit(Units::Imperial, true), "");
        for ru in [false, true] {
            assert_eq!(site_len_unit(Units::Metric, ru), crate::tr_str(ru, "unit_fmt.unit.mm"));
        }
    }

    #[test]
    fn site_lengths_are_typed_in_project_units_and_stored_in_metres() {
        for ru in [false, true] {
            near(typed("10'", Units::Imperial, ru).unwrap(), 3.048);
            near(typed("-2'", Units::Imperial, ru).unwrap(), -0.6096);
            near(typed("-1500", Units::Metric, ru).unwrap(), -1.5);
            let mark = DecimalMark::for_ru(ru);
            let shown = ui_kit::len_text(3.048, Units::Imperial, mark);
            assert!(shown.starts_with("10'"), "{shown}");
            near(typed(&shown, Units::Imperial, ru).unwrap(), 3.048);
        }
        near(typed("1,500.5", Units::Metric, false).unwrap(), 1.5005);
        near(typed("1 500,5", Units::Metric, true).unwrap(), 1.5005);
        // imperial project: 10' elevation goes into the model as metres, one undo step
        let mut a = app();
        a.ed.tool.units = Units::Imperial;
        open_info(&mut a);
        let steps = a.ed.undo_len();
        let mut b = take_info().unwrap();
        b.site.base.2 = typed("10'", a.ed.tool.units, false).unwrap();
        assert!(save_info(&mut a, &b));
        near(a.ed.model.site().unwrap().base.2, 3.048);
        assert_eq!(a.ed.undo_len(), steps + 1);
        a.ed.undo();
        near(a.ed.model.site().unwrap().base.2, 0.0);
    }

    #[test]
    fn bad_site_text_is_refused_and_changes_nothing() {
        for (u, ru, bad) in [
            (Units::Metric, false, "abc"),
            (Units::Metric, false, "1,5"),
            (Units::Metric, true, "1.2.3"),
            (Units::Imperial, false, "ten feet"),
            (Units::Imperial, true, "abc"),
        ] {
            let mark = DecimalMark::for_ru(ru);
            let shown = ui_kit::len_text(2.0, u, mark);
            let step = ui_kit::len_step(bad, &shown, 2.0, u, -SITE_LIMIT, SITE_LIMIT, mark);
            assert!(matches!(step, ui_kit::LenStep::Refuse(_)), "{bad} {u:?} {ru}");
        }
        // the buffer keeps the old metres, so Apply writes nothing
        let mut a = app();
        open_info(&mut a);
        let steps = a.ed.undo_len();
        let b = take_info().unwrap();
        assert!(save_info(&mut a, &b));
        assert_eq!(a.ed.undo_len(), steps);
        assert_eq!(a.ed.model.site().unwrap_or_default(), SiteProps::default());
    }

    fn typed_name(_: &mut EditorApp, name: &str) {
        with_info(|s| s.as_mut().unwrap().info.name = name.into());
    }

    #[test]
    fn unchanged_close_asks_nothing() {
        let mut a = app();
        open_info(&mut a);
        settle_info(&mut a, false, true, None);
        assert!(!info_open());
    }

    #[test]
    fn close_with_edits_asks_and_discard_drops_them() {
        let mut a = app();
        open_info(&mut a);
        let steps = a.ed.undo_len();
        typed_name(&mut a, "Haus");
        settle_info(&mut a, false, true, None);
        let flags = peek("the window stays while asking", |b| (b.asking, b.asked_before));
        assert_eq!(flags, (true, false));
        settle_info(&mut a, false, false, None);
        assert!(peek("still asking", |b| b.asked_before));
        settle_info(&mut a, false, false, Some(Choice::Discard));
        assert!(!info_open());
        assert_eq!(a.ed.undo_len(), steps);
        assert_eq!(a.ed.model.project_info().unwrap_or_default().name, "");
    }

    #[test]
    fn close_question_cancel_keeps_and_apply_is_one_step() {
        let mut a = app();
        open_info(&mut a);
        let steps = a.ed.undo_len();
        typed_name(&mut a, "Haus");
        with_info(|s| s.as_mut().unwrap().site.base.2 = 3.048);
        settle_info(&mut a, false, true, None);
        settle_info(&mut a, false, false, Some(Choice::Cancel));
        let (asking, name) = peek("Cancel keeps the window", |b| (b.asking, b.info.name.clone()));
        assert!(!asking);
        assert_eq!(name, "Haus");
        assert_eq!(a.ed.undo_len(), steps);
        settle_info(&mut a, false, true, None);
        settle_info(&mut a, false, false, Some(Choice::Apply));
        assert!(!info_open());
        assert_eq!(a.ed.undo_len(), steps + 1);
        assert_eq!(a.ed.model.project_info().unwrap().name, "Haus");
        near(a.ed.model.site().unwrap().base.2, 3.048);
        a.ed.undo();
        assert_eq!(a.ed.model.project_info().unwrap_or_default().name, "");
    }

    #[test]
    fn refused_apply_from_the_question_keeps_the_text() {
        let mut a = app();
        open_info(&mut a);
        let steps = a.ed.undo_len();
        typed_name(&mut a, &"x".repeat(501));
        settle_info(&mut a, false, true, None);
        settle_info(&mut a, false, false, Some(Choice::Apply));
        let why = "a refused value keeps the window";
        let (asking, len) = peek(why, |b| (b.asking, b.info.name.len()));
        assert!(!asking);
        assert_eq!(len, 501);
        assert_eq!(a.ed.undo_len(), steps);
    }

    #[test]
    fn esc_with_edits_asks_then_esc_cancels_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let mut a = app_in(lang);
            open_info(&mut a);
            let steps = a.ed.undo_len();
            typed_name(&mut a, "Haus");
            let ctx = font_ctx();
            for k in [None, None, Some(egui::Key::Escape)] {
                draw(&mut a, &ctx, k);
            }
            assert!(with_info(|s| s.as_ref().is_some_and(|b| b.asking)), "{lang:?} asks");
            for k in [None, Some(egui::Key::Escape)] {
                draw(&mut a, &ctx, k);
            }
            let why = "Esc on the question is Cancel";
            let (asking, name) = peek(why, |b| (b.asking, b.info.name.clone()));
            assert!(!asking, "{lang:?}");
            assert_eq!(name, "Haus");
            assert_eq!(a.ed.undo_len(), steps);
        }
    }

    fn drafts_held(a: &EditorApp) -> usize {
        crate::project_ui::drafts::count(&a.ed)
    }

    #[test]
    fn swap_model_closes_the_info_window_and_its_draft() {
        let mut a = app();
        seed_draft_for_test(&a.ed);
        assert_eq!(drafts_held(&a), 1);
        // a swap that fails keeps the old project, so the window stays too
        let bad = Model::open(":memory:").unwrap();
        bad.sql().execute_batch("DROP TABLE wall_type_layer;").unwrap();
        assert!(a.ed.replace_model(bad).is_err());
        assert!(info_open());
        let fresh = crate::editor::Editor::new_in_memory().unwrap().model;
        a.ed.replace_model(fresh).unwrap();
        assert!(!info_open(), "typed values of the old project do not carry over");
        assert_eq!(drafts_held(&a), 0);
    }

    #[test]
    fn draft_check_follows_the_model_revision() {
        let mut a = app();
        seed_draft_for_test(&a.ed);
        assert_eq!(drafts_held(&a), 1);
        // the typed values stored by another route: a new revision, nothing left to ask about
        let b = peek("open", |b| InfoBuf {
            info: b.info.clone(),
            site: b.site.clone(),
            asking: false,
            asked_before: false,
            seen: None,
            logo: None,
        });
        assert!(apply_info(&mut a.ed, &b).unwrap());
        assert_eq!(drafts_held(&a), 0);
        a.ed.undo();
        assert_eq!(drafts_held(&a), 1, "undo brings the stored name back");
        discard_drafts();
        assert!(!info_open());
        assert_eq!(drafts_held(&a), 0);
    }

    #[test]
    fn close_question_strings_exist_in_both_languages() {
        let key = "project_info.info.unsaved_body";
        let (en, ru) = (crate::tr_lang(false, key, &[]), crate::tr_lang(true, key, &[]));
        assert!(en != key && ru != key && en != ru);
    }

    #[test]
    fn current_wall_type_follows_its_id_through_purge_and_undo() {
        let mut a = app();
        let n = a.ed.types.len();
        let spare = a.ed.model.add_wall_type("spare", &layer()).unwrap();
        let last = a.ed.model.add_wall_type("last", &layer()).unwrap();
        a.ed.reload().unwrap();
        a.ed.current_type = n + 1;
        assert_eq!(a.ed.current_type().unwrap().id, last);
        let items: Vec<PurgeItem> =
            a.ed.model
                .purge_candidates()
                .unwrap()
                .into_iter()
                .filter(|i| i.table == "wall_type" && i.id == spare)
                .collect();
        purge(&mut a, items);
        assert_eq!(a.ed.current_type().unwrap().id, last);
        a.ed.undo();
        assert_eq!(a.ed.current_type().unwrap().id, last);
    }

    #[test]
    fn file_names_are_safe_and_keep_dots() {
        assert_eq!(safe_stem("A/B: c?"), "A_B_ c_");
        assert_eq!(safe_stem(" . "), "model");
        assert_eq!(safe_stem(""), "model");
        assert_eq!(
            glb_path(PathBuf::from("Haus 7.2")),
            PathBuf::from("Haus 7.2.glb")
        );
        assert_eq!(glb_path(PathBuf::from("a.GLB")), PathBuf::from("a.GLB"));
    }

    /// The two windows are built from kit widgets: no raw grid, text edit, heading, button or
    /// unnamed checkbox between `info_window` and the purge operation.
    #[test]
    fn info_and_purge_windows_use_the_kit() {
        let src = include_str!("data_ui.rs");
        let from = src.find("fn info_window(").unwrap();
        let to = src.find("/// All chosen items in one model operation").unwrap();
        let win = &src[from..to];
        let unnamed = format!("{0}{0})", '"');
        for raw in ["egui::Grid", "TextEdit", "ui.heading", "ui.button", "ui.strong", "ui.checkbox"] {
            assert!(!win.contains(raw), "raw {raw} in the info or purge window");
        }
        assert!(!win.contains(&unnamed), "a checkbox without a name");
        for kit in ["section_title", "TextField::new", "labelled_by", "cancel_button", "empty_note"] {
            assert!(win.contains(kit), "{kit} missing in the info or purge window");
        }
    }

    #[test]
    fn purge_count_is_the_plan_not_the_ticks() {
        let mut a = app();
        let fam = a.ed.model.load_family(ddc_model::library_sources()[0]).unwrap();
        let mut items: Vec<(PurgeItem, bool)> = a
            .ed
            .model
            .purge_candidates()
            .unwrap()
            .into_iter()
            .filter(|i| i.table == "comp_family" && i.id == fam)
            .map(|i| (i, true))
            .collect();
        assert_eq!(items.len(), 1);
        // the family alone keeps all its types: ticked, but nothing goes
        assert_eq!(planned(&a.ed.model, &items), 0);
        // every candidate ticked: the count is the plan of those candidates
        items = a.ed.model.purge_candidates().unwrap().into_iter().map(|i| (i, true)).collect();
        let all: Vec<PurgeItem> = items.iter().map(|i| i.0.clone()).collect();
        assert_eq!(planned(&a.ed.model, &items), a.ed.model.purge_plan(&all).unwrap().len());
        // one type of the family unticked while the family stays ticked
        if let Some(t) = items
            .iter_mut()
            .find(|i| i.0.table == "comp_type" && i.0.family == Some(fam))
        {
            t.1 = false;
            let ticked = items.iter().filter(|i| i.1).count();
            assert!(planned(&a.ed.model, &items) < ticked);
        }
    }

    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    fn draw(a: &mut EditorApp, ctx: &egui::Context, key: Option<egui::Key>) {
        let events = key
            .map(|key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .into_iter()
            .collect();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run(raw, |c| {
            let _ = a.sh.theme.apply(c);
            frame(a, c);
        });
    }

    fn app_in(lang: ddc_ui::Lang) -> EditorApp {
        let mut a = app();
        a.sh.cat = ddc_ui::Catalog::new(lang);
        a
    }

    #[test]
    fn esc_closes_both_windows_and_changes_nothing() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            for which in ["info", "purge", "none"] {
                let mut a = app_in(lang);
                crate::data_ui::open_for_layout(&mut a, which);
                let steps = a.ed.undo_len();
                let ctx = font_ctx();
                for k in [None, None, Some(egui::Key::Escape)] {
                    draw(&mut a, &ctx, k);
                }
                assert!(!info_open() && a.data.purge.is_none(), "{which} {lang:?}");
                assert_eq!(a.ed.undo_len(), steps);
            }
        }
    }

    #[test]
    fn purge_plan_is_queried_once_per_tick_change() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let mut a = app_in(lang);
            crate::data_ui::open_for_layout(&mut a, "purge");
            let ctx = font_ctx();
            PLAN_QUERIES.with(|c| c.set(0));
            for _ in 0..5 {
                draw(&mut a, &ctx, None);
            }
            assert_eq!(PLAN_QUERIES.with(|c| c.get()), 1, "{lang:?} still frames");
            // one tick flipped: one more query, then quiet again
            let first = a.data.purge.as_mut().unwrap().first_mut().unwrap();
            first.1 = !first.1;
            for _ in 0..3 {
                draw(&mut a, &ctx, None);
            }
            assert_eq!(PLAN_QUERIES.with(|c| c.get()), 2, "{lang:?} after a tick change");
            // a model change is a new key too
            a.ed.add_level("Roof", 9.0).unwrap();
            draw(&mut a, &ctx, None);
            draw(&mut a, &ctx, None);
            assert_eq!(PLAN_QUERIES.with(|c| c.get()), 3, "{lang:?} after a model change");
        }
    }

    #[test]
    fn purge_notes_use_the_caption_role_not_weak_small() {
        let src = include_str!("data_ui.rs");
        let marker = ["#[cfg(", "test)]\nmod tests"].concat();
        let code = src.split(marker.as_str()).next().unwrap();
        assert!(!code.contains(".weak().small()"), "theme role instead of weak().small()");
    }

    #[test]
    fn enter_never_runs_a_purge() {
        let mut a = app_in(ddc_ui::Lang::En);
        crate::data_ui::open_for_layout(&mut a, "purge");
        let n = a.data.purge.as_ref().unwrap().len();
        assert!(n >= 2);
        let steps = a.ed.undo_len();
        let ctx = font_ctx();
        for k in [None, None, Some(egui::Key::Enter), None] {
            draw(&mut a, &ctx, k);
        }
        assert_eq!(a.data.purge.as_ref().map(|p| p.len()), Some(n));
        assert_eq!(a.ed.undo_len(), steps);
    }
}
