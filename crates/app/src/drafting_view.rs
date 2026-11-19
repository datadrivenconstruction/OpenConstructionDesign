// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drafting views in the app: a tab with a named 2D canvas (pan, zoom, adaptive grid) that shows
//! and edits annotation only, through `annot_text` with `AnnoView::Drafting`; the ann.* tools work
//! there unchanged and model elements are never drawn. A manager window opens, renames, rescales,
//! duplicates and deletes views; every change is one `run_op`, so one undo step.

use std::cell::RefCell;
use std::sync::Arc;

use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::ui_kit::{Dialog, DialogSize, Tokens};
use ddc_geom::{v2, V2};
use ddc_model::{AnnoView, DraftingDef, SCALES};
use ddc_ui::theme::{self, space, type_size, Role};
use ddc_ui::widgets::{self, Button};
use egui::{self, Align2, FontId, Pos2, Rect, Sense, Stroke};

/// Tab ids of drafting views sit far below sheet tabs (those are `-sheet id`).
const TAB_BASE: i64 = -(1 << 40);
/// Scale of a new drafting view (1:N): details are drawn large.
pub const DEFAULT_SCALE: f64 = 10.0;
/// Zoom limits, pixels per metre.
const PX_MIN: f64 = 4.0;
const PX_MAX: f64 = 50_000.0;
/// Smallest grid spacing on screen, px.
const GRID_PX: f64 = 16.0;
/// Browser and command glyph: a drafting view holds detail lines and nothing from the model.
const ICON: &str = "detail-line";

#[derive(Default)]
struct State {
    active: Option<i64>,
    center: V2,
    /// Pixels per metre.
    px: f64,
    fitted: Option<i64>,
    /// (view, editor revision, has annotations) for the empty-state hint.
    filled: Option<(i64, u64, bool)>,
    list_open: bool,
    /// Name typed in the manager: (view, text).
    rename: Option<(i64, String)>,
    /// Delete of a view with annotations waits for a second click.
    confirm: Option<i64>,
    /// Shell view state of the last frame: (3D, section, level).
    seen: Option<(bool, Option<i64>, i64)>,
    /// Editor revision the open manager last saw.
    rev_seen: Option<u64>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Drafting view open in the view area, if any (not checked against the model).
pub fn active() -> Option<i64> {
    with(|s| s.active)
}

/// Plan, 3D, a section or another project takes the view area.
pub fn deactivate() {
    with(|s| {
        s.active = None;
        s.fitted = None;
    });
}

/// Shell frame: any switch to 3D, a section or another level (browser, palette, shortcuts)
/// takes the view area from the drafting view.
pub fn follow(view3d: bool, section: Option<i64>, level: i64) {
    let now = (view3d, section, level);
    with(|s| {
        if s.seen.is_some_and(|was| was != now) {
            s.active = None;
            s.fitted = None;
        }
        s.seen = Some(now);
    });
}

/// Show the drafting view in the view area; a sheet canvas gives way to it.
pub fn open(id: i64) {
    with(|s| {
        if s.active != Some(id) {
            s.fitted = None;
        }
        s.active = Some(id);
    });
    crate::sheet_view::deactivate();
}

/// Open drafting view that still exists; a view gone by undo or delete closes its tab.
pub fn current(ed: &Editor) -> Option<i64> {
    let id = active()?;
    if ed.model.drafting(id).is_ok() {
        Some(id)
    } else {
        deactivate();
        None
    }
}

pub fn tab_id(id: i64) -> i64 {
    TAB_BASE - id
}

pub fn of_tab(tab: i64) -> Option<i64> {
    (tab < TAB_BASE).then(|| TAB_BASE - tab)
}

fn scale_text(s: f64) -> String {
    format!("1:{s}")
}

pub fn title(ed: &Editor, id: i64) -> Option<String> {
    let d = ed.model.drafting(id).ok()?;
    Some(crate::tr("drafting_view.tab", &[("name", &d.name)]))
}

// ---------- actions (each one model operation) ----------

/// Result of one operation. `Stale`: committed with its undo step, only the editor reload failed.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ran {
    Done,
    Stale,
    Failed,
}

impl Ran {
    fn ok(self) -> bool {
        self != Ran::Failed
    }
}

fn run(
    ed: &mut Editor,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Ran {
    let top = ed.model.op_top().ok();
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => Ran::Done,
        Err(e) => {
            // the op row is there: the edit happened, so it is no failure and must not be retried
            let committed = top.is_some() && ed.model.op_top().ok() != top;
            let (key, ran) = if committed {
                ("drafting_view.stale", Ran::Stale)
            } else {
                ("drafting_view.err", Ran::Failed)
            };
            ed.status = Some(crate::tr(key, &[("e", &e.to_string())]));
            ran
        }
    }
}

fn by_name(ed: &Editor, name: &str) -> Option<i64> {
    ed.model
        .draftings()
        .ok()?
        .into_iter()
        .find(|d| d.name == name)
        .map(|d| d.id)
}

/// New empty view under a free default name; it opens at once.
pub fn create(ed: &mut Editor) -> Option<i64> {
    let base = crate::tr("drafting_view.default_name", &[]);
    let name = ed.model.drafting_free_name(&base).ok()?;
    let n = name.clone();
    let ran = run(ed, move |m| m.add_drafting(&n, DEFAULT_SCALE).map(|_| ()));
    if !ran.ok() {
        return None;
    }
    let id = by_name(ed, &name)?;
    if ran == Ran::Done {
        ed.status = Some(crate::tr("drafting_view.created", &[("name", &name)]));
    }
    open(id);
    Some(id)
}

/// Copy with all annotations under the next free name (`Name 2`); the copy opens.
pub fn duplicate(ed: &mut Editor, id: i64) -> Option<i64> {
    let src = ed.model.drafting(id).ok()?;
    let name = ed.model.drafting_free_name(&src.name).ok()?;
    let n = name.clone();
    if !run(ed, move |m| m.duplicate_drafting(id, &n).map(|_| ())).ok() {
        return None;
    }
    let copy = by_name(ed, &name)?;
    open(copy);
    Some(copy)
}

/// False when refused (empty or taken name); the reason goes to the status line.
pub fn rename(ed: &mut Editor, id: i64, name: &str) -> bool {
    let name = name.trim().to_string();
    let Ok(d) = ed.model.drafting(id) else {
        return false;
    };
    if name == d.name {
        return true;
    }
    if name.is_empty() {
        ed.status = Some(crate::tr("drafting_view.name_empty", &[]));
        return false;
    }
    if by_name(ed, &name).is_some() {
        ed.status = Some(crate::tr("drafting_view.name_taken", &[("name", &name)]));
        return false;
    }
    run(ed, move |m| m.rename_drafting(id, &name)).ok()
}

pub fn set_scale(ed: &mut Editor, id: i64, scale: f64) -> bool {
    run(ed, move |m| m.set_drafting_scale(id, scale)).ok()
}

/// A view with annotations is only marked for the manager to ask; an empty one is deleted at
/// once. True when it was deleted.
pub fn request_delete(ed: &mut Editor, id: i64) -> bool {
    let n = ed
        .model
        .annos(AnnoView::Drafting(id))
        .map_or(0, |a| a.len());
    if n > 0 {
        with(|s| s.confirm = Some(id));
        return false;
    }
    delete(ed, id)
}

/// A pending delete question or half-typed name must not outlive the manager or the model it was
/// asked about: both are dropped when the manager is closed or the revision changed.
fn sync_state(rev: u64) {
    with(|s| {
        if !s.list_open {
            s.confirm = None;
            s.rename = None;
            s.rev_seen = None;
        } else {
            if s.rev_seen.is_some_and(|r| r != rev) {
                s.confirm = None;
                s.rename = None;
            }
            s.rev_seen = Some(rev);
        }
    });
}

/// Only a pointer click confirms a delete; Enter never does, even on a focused button.
fn delete_confirmed(clicked: bool, enter: bool) -> bool {
    clicked && !enter
}

/// How a rename text field ended this frame.
#[derive(PartialEq, Debug)]
enum RenameEnd {
    Keep,
    Commit,
    Cancel,
}

/// Enter and a click elsewhere commit, Esc alone cancels.
fn rename_end(lost_focus: bool, esc: bool) -> RenameEnd {
    match (lost_focus, esc) {
        (false, _) => RenameEnd::Keep,
        (true, true) => RenameEnd::Cancel,
        (true, false) => RenameEnd::Commit,
    }
}

/// The view with its annotations; an open tab of it closes.
pub fn delete(ed: &mut Editor, id: i64) -> bool {
    let Ok(d) = ed.model.drafting(id) else {
        return false;
    };
    let ran = run(ed, move |m| m.delete_drafting(id));
    if !ran.ok() {
        return false;
    }
    with(|s| {
        if s.confirm == Some(id) {
            s.confirm = None;
        }
    });
    if active() == Some(id) {
        deactivate();
    }
    if ran == Ran::Done {
        ed.status = Some(crate::tr("drafting_view.deleted", &[("name", &d.name)]));
    }
    true
}

// ---------- commands ----------

fn h_new(app: &mut EditorApp) {
    create(&mut app.ed);
}

fn h_open(_app: &mut EditorApp) {
    show_list();
}

/// Opens the manager window.
pub(crate) fn show_list() {
    with(|s| s.list_open = true);
}

/// Shell registration (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("view.drafting_new", h_new);
    app.register_command("view.drafting_open", h_open);
}

// ---------- manager window ----------

enum Act {
    New,
    Open(i64),
    Rename(i64, String),
    Scale(i64, f64),
    Duplicate(i64),
    /// Asks first when the view has annotations.
    Delete(i64),
    ConfirmDelete(i64),
    CancelDelete,
}

/// Manager window (`view.drafting_open`): one row per view with name, scale and actions.
pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    window_in(app, ctx, crate::ui_lang_now());
}

/// As `window`, in an explicit language (headless layout probes).
pub(crate) fn window_in(app: &mut EditorApp, ctx: &egui::Context, ru: bool) {
    sync_state(app.ed.revision);
    let Some((mut open_flag, mut rename_buf, confirm)) =
        with(|s| s.list_open.then(|| (true, s.rename.take(), s.confirm)))
    else {
        return;
    };
    let rows: Vec<(DraftingDef, f64, usize)> = app
        .ed
        .model
        .draftings()
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            let v = AnnoView::Drafting(d.id);
            let sc = app.ed.model.anno_scale(v).unwrap_or(d.scale);
            let n = app.ed.model.annos(v).map_or(0, |a| a.len());
            (d, sc, n)
        })
        .collect();
    let shown = current(&app.ed);
    let th = &app.sh.theme;
    let mut acts: Vec<Act> = vec![];
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let title = crate::tr_lang(ru, "drafting_view.title", &[]);
    Dialog::new("dv1.list", title, DialogSize::Tool)
        .open(&mut open_flag)
        .scroll_body()
        .show(ctx, |ui| {
            // the empty state carries the one primary action itself
            if rows.is_empty() {
                if crate::ui_kit::empty_state(ui, "drafting") {
                    acts.push(Act::New);
                }
                return;
            }
            let new = crate::tr_lang(ru, "drafting_view.new", &[]);
            let new_tip = crate::tr_lang(ru, "cmd.view.drafting_new.desc", &[]);
            if Button::primary(&new)
                .tooltip(&new_tip, "")
                .show(ui, th)
                .clicked()
            {
                acts.push(Act::New);
            }
            ui.add_space(space::SM);
            let mut ctl = Ctl {
                th,
                ru,
                shown,
                confirm,
                enter,
                rename: &mut rename_buf,
                acts: &mut acts,
            };
            for (d, sc, n) in &rows {
                ctl.row(ui, d, *sc, *n);
            }
        });
    with(|s| {
        s.list_open = open_flag;
        s.rename = rename_buf;
        if !open_flag {
            s.confirm = None;
            s.rename = None;
            s.rev_seen = None;
        }
    });
    for a in acts {
        let ed = &mut app.ed;
        match a {
            Act::New => {
                create(ed);
            }
            Act::Open(id) => open(id),
            Act::Rename(id, name) => {
                // a refused name stays in the field to be fixed
                if !rename(ed, id, &name) && ed.model.drafting(id).is_ok() {
                    with(|s| s.rename = Some((id, name)));
                }
            }
            Act::Scale(id, sc) => {
                set_scale(ed, id, sc);
            }
            Act::Duplicate(id) => {
                duplicate(ed, id);
            }
            Act::Delete(id) => {
                request_delete(ed, id);
            }
            Act::ConfirmDelete(id) => {
                with(|s| s.confirm = None);
                delete(ed, id);
            }
            Act::CancelDelete => with(|s| s.confirm = None),
        }
    }
}

/// Width of the scale column, equal on every row.
const SCALE_W: f32 = 84.0;

/// Manager rows of one frame: the language, what is being edited or asked, and the sink of
/// the actions. A row is the name (takes the free width, elided), the scale column, a quiet
/// Open and an overflow menu; the destructive action lives only in that menu and in the
/// question that follows it.
struct Ctl<'a> {
    th: &'a ddc_ui::Theme,
    ru: bool,
    shown: Option<i64>,
    confirm: Option<i64>,
    enter: bool,
    rename: &'a mut Option<(i64, String)>,
    acts: &'a mut Vec<Act>,
}

impl Ctl<'_> {
    fn t(&self, key: &str) -> String {
        crate::tr_lang(self.ru, key, &[])
    }

    fn row(&mut self, ui: &mut egui::Ui, d: &DraftingDef, sc: f64, n: usize) {
        let h = self.th.d().control;
        let w = ui.available_width();
        let right = egui::Layout::right_to_left(egui::Align::Center);
        ui.allocate_ui_with_layout(egui::vec2(w, h), right, |ui| {
            if self.confirm == Some(d.id) {
                self.ask(ui, d, n);
                return;
            }
            self.more(ui, d);
            let open = self.t("drafting_view.open");
            if Button::quiet(&open).small().show(ui, self.th).clicked() {
                self.acts.push(Act::Open(d.id));
            }
            self.scale(ui, d.id, sc);
            let left = egui::Layout::left_to_right(egui::Align::Center);
            ui.with_layout(left, |ui| self.name(ui, d, n));
        });
    }

    /// The delete question: replaces the row's actions until answered.
    fn ask(&mut self, ui: &mut egui::Ui, d: &DraftingDef, n: usize) {
        let th = self.th;
        let (yes_l, no_l) = (self.t("drafting_view.delete_yes"), self.t("drafting_view.cancel"));
        let yes = Button::danger(&yes_l).small().show(ui, th).clicked();
        if delete_confirmed(yes, self.enter) {
            self.acts.push(Act::ConfirmDelete(d.id));
        }
        if Button::quiet(&no_l).small().show(ui, th).clicked() {
            self.acts.push(Act::CancelDelete);
        }
        let ask = crate::tr_lang(
            self.ru,
            "drafting_view.delete_ask",
            &[("name", &d.name), ("n", &n.to_string())],
        );
        let left = egui::Layout::left_to_right(egui::Align::Center);
        ui.with_layout(left, |ui| {
            widgets::elided_label(ui, &ask, th.font(Role::Body), th.sem().text_primary)
        });
    }

    /// Overflow menu: Rename, Duplicate, then Delete as the only danger row.
    fn more(&mut self, ui: &mut egui::Ui, d: &DraftingDef) {
        let th = self.th;
        let more = self.t("drafting_view.more");
        let btn = Button::quiet(&more).small().show(ui, th);
        let (ren, dup, del, tip) = (
            self.t("drafting_view.rename"),
            self.t("drafting_view.duplicate"),
            self.t("drafting_view.delete"),
            self.t("drafting_view.delete_tip"),
        );
        let mut pick = None;
        egui::Popup::menu(&btn)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                if crate::ctx_menu::verb_row(ui, th, "rename", &ren, "").clicked() {
                    pick = Some(0);
                }
                if crate::ctx_menu::verb_row(ui, th, "duplicate", &dup, "").clicked() {
                    pick = Some(1);
                }
                widgets::menu_divider(ui, th);
                let r = crate::ctx_menu::verb_row(ui, th, "delete", &del, "");
                if r.on_hover_text(tip).clicked() {
                    pick = Some(2);
                }
            });
        match pick {
            Some(0) => *self.rename = Some((d.id, d.name.clone())),
            Some(1) => self.acts.push(Act::Duplicate(d.id)),
            Some(_) => self.acts.push(Act::Delete(d.id)),
            None => {}
        }
    }

    /// Scale dropdown in the fixed column; an off-list scale is shown, not rewritten.
    fn scale(&mut self, ui: &mut egui::Ui, id: i64, sc: f64) {
        let labels: Vec<String> = SCALES.iter().map(|s| scale_text(*s)).collect();
        let cur = scale_text(sc);
        let mut items: Vec<&str> = labels.iter().map(String::as_str).collect();
        let mut idx = SCALES
            .iter()
            .position(|s| (*s - sc).abs() < 1e-9)
            .unwrap_or(SCALES.len());
        if idx == SCALES.len() {
            items.push(&cur);
        }
        let key = ("drafting_view.scale", id);
        let r = widgets::dropdown(ui, self.th, key, &mut idx, &items, Some(SCALE_W));
        if r.changed() && idx < SCALES.len() {
            self.acts.push(Act::Scale(id, SCALES[idx]));
        }
        r.on_hover_text(self.t("drafting_view.scale"));
    }

    /// Name over the free width: click opens, double click renames, a long name is elided
    /// with the full text and the annotation count on hover.
    fn name(&mut self, ui: &mut egui::Ui, d: &DraftingDef, n: usize) {
        let th = self.th;
        let w = ui.available_width().max(1.0);
        let editing = self.rename.as_ref().is_some_and(|(i, _)| *i == d.id);
        if editing {
            let Some((_, buf)) = self.rename.as_mut() else {
                return;
            };
            let cap = crate::tr_lang(self.ru, "drafting_view.rename", &[]);
            let out = widgets::TextField::new(buf)
                .labelled_by(egui::Id::new(("dv1.rename.lbl", d.id)), &cap)
                .id_salt(("drafting_view.rename", d.id))
                .width(w)
                .show(ui, th);
            let lost = out.response.lost_focus() || out.committed || out.cancelled;
            let end = rename_end(lost, out.cancelled);
            if !lost && !out.response.has_focus() {
                out.response.request_focus();
            }
            let text = buf.clone();
            match end {
                RenameEnd::Keep => {}
                RenameEnd::Commit => {
                    self.acts.push(Act::Rename(d.id, text));
                    *self.rename = None;
                }
                RenameEnd::Cancel => *self.rename = None,
            }
            return;
        }
        let s = th.sem();
        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, th.d().control), Sense::click());
        let count = crate::tr_lang(self.ru, "drafting_view.count", &[("n", &n.to_string())]);
        r.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, d.name.as_str())
        });
        if r.hovered() {
            ui.painter().rect_filled(rect, th.radius("md"), s.bg_hover);
        }
        let role = if self.shown == Some(d.id) {
            Role::BodyStrong
        } else {
            Role::Body
        };
        let max_w = rect.width() - 2.0 * space::SM;
        let g = widgets::elide(ui, &d.name, th.font(role), s.text_primary, max_w);
        let pos = egui::pos2(rect.left() + space::SM, rect.center().y - g.size().y / 2.0);
        ui.painter().galley(pos, g, s.text_primary);
        let r = r.on_hover_text(format!("{}\n{}", d.name, count));
        if r.double_clicked() {
            *self.rename = Some((d.id, d.name.clone()));
        } else if r.clicked() {
            self.acts.push(Act::Open(d.id));
        }
    }
}

// ---------- project browser ----------

/// What a browser row asked for; applied after the rows are drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Open(i64),
    Rename(i64),
    Duplicate(i64),
    Delete(i64),
}

/// Project browser section "Drafting views": a click opens the view, the context menu opens,
/// renames, duplicates or deletes it. Rows filtered by `keep`; returns how many were shown.
pub fn browser_rows(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    keep: &dyn Fn(&str) -> bool,
    th: &ddc_ui::Theme,
    row: &mut dyn FnMut(&mut egui::Ui, &str, &str, &str, bool) -> egui::Response,
) -> usize {
    let list: Vec<DraftingDef> = ed
        .model
        .draftings()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| keep(&d.name))
        .collect();
    if list.is_empty() {
        return 0;
    }
    let head = crate::tr("drafting_view.title", &[]);
    let n = list.len();
    if !crate::shell::panels::browser_sec::section(ui, th, &head, &head, Some(n), None).open {
        return n;
    }
    let shown = current(ed);
    let mut act = None;
    for d in &list {
        let sc = ed.model.anno_scale(AnnoView::Drafting(d.id)).unwrap_or(d.scale);
        let r = row(ui, ICON, &d.name, &scale_text(sc), shown == Some(d.id));
        if r.clicked() || r.double_clicked() {
            act = Some(Row::Open(d.id));
        }
        r.context_menu(|ui| {
            for (verb, key, cmd, a) in [
                ("open", "drafting_view.open", crate::ctx_menu::OPEN, Row::Open(d.id)),
                ("rename", "drafting_view.rename", crate::ctx_menu::RENAME, Row::Rename(d.id)),
                ("duplicate", "drafting_view.duplicate", "", Row::Duplicate(d.id)),
            ] {
                if crate::ctx_menu::verb_row(ui, th, verb, &crate::tr(key, &[]), cmd).clicked() {
                    act = Some(a);
                    ui.close();
                }
            }
            ddc_ui::widgets::menu_divider(ui, th);
            let del = crate::tr("drafting_view.delete", &[]);
            if crate::ctx_menu::verb_row(ui, th, "delete", &del, crate::ctx_menu::DELETE).clicked() {
                act = Some(Row::Delete(d.id));
                ui.close();
            }
        });
    }
    if let Some(a) = act {
        browser_act(ed, a);
    }
    list.len()
}

/// Rename and a delete that must ask go through the manager window, so both have one home.
fn browser_act(ed: &mut Editor, a: Row) {
    match a {
        Row::Open(id) => open(id),
        Row::Rename(id) => {
            if let Ok(d) = ed.model.drafting(id) {
                with(|s| {
                    s.list_open = true;
                    s.rename = Some((id, d.name));
                });
            }
        }
        Row::Duplicate(id) => {
            duplicate(ed, id);
        }
        Row::Delete(id) => {
            if !request_delete(ed, id) && with(|s| s.confirm == Some(id)) {
                with(|s| s.list_open = true);
            }
        }
    }
}

// ---------- canvas ----------

/// Frame of everything drawn in the view, or a 2 x 1.5 m sheet around the origin when empty.
fn extent(ed: &Editor, id: i64) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for a in ed.model.annos(AnnoView::Drafting(id)).unwrap_or_default() {
        for p in a.shape.grips() {
            b = [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)];
        }
    }
    if b[0] > b[2] {
        return [-1.0, -0.75, 1.0, 0.75];
    }
    b
}

fn fit(ed: &Editor, id: i64, size: egui::Vec2) -> (V2, f64) {
    let b = extent(ed, id);
    let (w, h) = ((b[2] - b[0]).max(0.05), (b[3] - b[1]).max(0.05));
    let px = (f64::from(size.x) * 0.8 / w)
        .min(f64::from(size.y) * 0.8 / h)
        .clamp(PX_MIN, PX_MAX);
    (v2((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0), px)
}

/// Grid step in metres (1, 2 or 5 times a power of ten) at least `GRID_PX` apart on screen.
pub fn grid_step(px: f64) -> f64 {
    let raw = GRID_PX / px.max(1e-9);
    let p = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|k| k * p)
        .find(|s| *s >= raw - 1e-12)
        .unwrap_or(10.0 * p)
}

fn paint_grid(
    painter: &egui::Painter,
    rect: Rect,
    t: &Tokens,
    px: f64,
    to_w: &dyn Fn(Pos2) -> V2,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let step = grid_step(px);
    let major = step * 5.0;
    let (a, b) = (to_w(rect.left_bottom()), to_w(rect.right_top()));
    let lines = |lo: f64, hi: f64| {
        let i0 = (lo / step).floor() as i64;
        let i1 = (hi / step).ceil() as i64;
        (i0..=i1).take(400).map(move |i| i as f64 * step)
    };
    let col = |v: f64| {
        let k = (v / major).round() * major;
        if (v - k).abs() < step * 1e-3 {
            t.grid_major
        } else {
            t.grid_minor
        }
    };
    for x in lines(a.x, b.x) {
        let sx = to_s(v2(x, 0.0)).x;
        painter.vline(sx, rect.y_range(), Stroke::new(1.0, col(x)));
    }
    for y in lines(a.y, b.y) {
        let sy = to_s(v2(0.0, y)).y;
        painter.hline(rect.x_range(), sy, Stroke::new(1.0, col(y)));
    }
}

fn has_annos(ed: &Editor, id: i64) -> bool {
    let rev = ed.revision;
    if let Some(v) = with(|s| s.filled.filter(|f| f.0 == id && f.1 == rev).map(|f| f.2)) {
        return v;
    }
    let v = ed
        .model
        .annos(AnnoView::Drafting(id))
        .is_ok_and(|a| !a.is_empty());
    with(|s| s.filled = Some((id, rev, v)));
    v
}

/// The drafting tab: paper, grid, annotation of the view, its tools and grips.
pub fn show(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    id: i64,
    t: &Tokens,
    lane: &mut crate::modify_tools::Lane,
) {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let (mut c, mut px, fitted) = with(|s| (s.center, s.px, s.fitted));
    if fitted != Some(id) || px <= 0.0 {
        (c, px) = fit(ed, id, rect.size());
    }
    // navigation: middle or left drag on empty paper pans, the wheel zooms at the cursor
    if resp.dragged_by(egui::PointerButton::Middle)
        || (resp.dragged_by(egui::PointerButton::Primary)
            && !resp.drag_started_by(egui::PointerButton::Primary)
            && !crate::annot_text::dragging()
            && !crate::annot_text::armed())
    {
        let d = resp.drag_delta();
        c = c - v2(f64::from(d.x) / px, -f64::from(d.y) / px);
    }
    let c0 = rect.center();
    if let Some(h) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let before = v2(
                c.x + f64::from(h.x - c0.x) / px,
                c.y - f64::from(h.y - c0.y) / px,
            );
            let next = (px * (f64::from(scroll) * 0.0025).exp()).clamp(PX_MIN, PX_MAX);
            c = c + (before - c) * (1.0 - px / next);
            px = next;
        }
    }
    with(|s| {
        s.center = c;
        s.px = px;
        s.fitted = Some(id);
    });
    let (cx, cy) = (c.x, c.y);
    let to_s = move |p: V2| {
        Pos2::new(
            c0.x + ((p.x - cx) * px) as f32,
            c0.y - ((p.y - cy) * px) as f32,
        )
    };
    let to_w = move |p: Pos2| {
        v2(
            cx + f64::from(p.x - c0.x) / px,
            cy - f64::from(p.y - c0.y) / px,
        )
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.paper);
    paint_grid(&painter, rect, t, px, &to_w, &to_s);

    let view = AnnoView::Drafting(id);
    // model elements are never shown here: a selection carried over from the plan must not be
    // deleted or edited unseen
    ed.selection.retain(|s| matches!(s, Sel::Anno(_)));
    let busy = crate::annot_text::busy();
    // an edit tool (move, rotate, copy ...) works on this view's items, never on the plan
    let tool = lane.drafting_input(ui, &resp, ed, &to_w);
    let used = tool || crate::annot_text::view_input(ui, &resp, ed, view, &to_w);
    if resp.clicked() && !used && !ed.tool_armed() {
        ed.selection.clear();
    }
    if !busy
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
        && crate::ui_kit::canvas_owns_keys(ui.ctx())
        && !ui.ctx().wants_keyboard_input()
    {
        ed.selection.clear();
    }
    crate::annot_text::draw(ui, &painter, rect, ed, t, &to_s, view);
    lane.draw_drafting(ui, &painter, ed, &to_s, t);

    // quiet caption: name and scale, top left
    let label = FontId::new(type_size::LABEL, theme::family(theme::W_REGULAR));
    if let Ok(d) = ed.model.drafting(id) {
        let sc = ed.model.anno_scale(view).unwrap_or(d.scale);
        let cap = crate::tr(
            "drafting_view.caption",
            &[("name", &d.name), ("scale", &scale_text(sc))],
        );
        painter.text(
            rect.left_top() + egui::vec2(space::MD, space::SM),
            Align2::LEFT_TOP,
            cap,
            label.clone(),
            t.text_dim,
        );
    }
    if !has_annos(ed, id) && !crate::annot_text::armed() {
        let title = FontId::new(type_size::TITLE, theme::family(theme::W_SEMIBOLD));
        painter.text(
            c0 - egui::vec2(0.0, space::SM),
            Align2::CENTER_BOTTOM,
            crate::tr("drafting_view.canvas_empty", &[]),
            title,
            t.text,
        );
        painter.text(
            c0 + egui::vec2(0.0, space::XS),
            Align2::CENTER_TOP,
            crate::tr("drafting_view.canvas_hint", &[]),
            label,
            t.text_dim,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        with(|s| *s = State::default());
        Editor::new_in_memory().unwrap()
    }

    #[test]
    fn new_view_opens_and_one_undo_removes_it() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        assert_eq!(current(&e), Some(id));
        let d = e.model.drafting(id).unwrap();
        assert_eq!(d.scale, DEFAULT_SCALE);
        // a second one gets the next free name
        let id2 = create(&mut e).unwrap();
        assert_eq!(
            e.model.drafting(id2).unwrap().name,
            format!("{} 2", d.name)
        );
        assert_eq!(current(&e), Some(id2));
        e.undo();
        assert!(e.model.drafting(id2).is_err());
        // the tab of a view gone by undo closes itself
        assert_eq!(current(&e), None);
        assert_eq!(active(), None);
        e.undo();
        assert!(e.model.draftings().unwrap().is_empty());
    }

    #[test]
    fn rename_duplicate_delete_are_one_step_each() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        assert!(rename(&mut e, id, "  Eave  "));
        assert_eq!(e.model.drafting(id).unwrap().name, "Eave");
        assert!(!rename(&mut e, id, "   "));
        let other = create(&mut e).unwrap();
        assert!(!rename(&mut e, other, "Eave"));
        assert!(e.status.is_some());
        let line = ddc_model::AnnoShape::Line {
            pts: vec![(0.0, 0.0), (1.0, 0.0)],
            closed: false,
        };
        assert!(run(&mut e, move |m| m
            .add_anno(AnnoView::Drafting(id), &line, None, None)
            .map(|_| ()))
        .ok());
        let copy = duplicate(&mut e, id).unwrap();
        assert_eq!(e.model.drafting(copy).unwrap().name, "Eave 2");
        assert_eq!(e.model.annos(AnnoView::Drafting(copy)).unwrap().len(), 1);
        assert_eq!(current(&e), Some(copy));
        assert!(set_scale(&mut e, copy, 5.0));
        assert_eq!(e.model.anno_scale(AnnoView::Drafting(copy)).unwrap(), 5.0);
        e.undo();
        assert_eq!(e.model.anno_scale(AnnoView::Drafting(copy)).unwrap(), 10.0);
        assert!(delete(&mut e, copy));
        assert_eq!(current(&e), None);
        assert_eq!(e.model.annos(AnnoView::Drafting(id)).unwrap().len(), 1);
        e.undo();
        assert_eq!(e.model.annos(AnnoView::Drafting(copy)).unwrap().len(), 1);
        e.undo();
        assert!(e.model.drafting(copy).is_err());
        e.undo();
        e.undo();
        assert_eq!(e.model.drafting(id).unwrap().name, "Eave");
        e.undo();
        assert_ne!(e.model.drafting(id).unwrap().name, "Eave");
    }

    fn break_reload(e: &mut Editor) {
        e.model
            .sql()
            .execute_batch("ALTER TABLE wall_type_layer RENAME TO zz_layer")
            .unwrap();
    }

    fn stale_prefix() -> String {
        let s = crate::tr("drafting_view.stale", &[("e", "\u{1}")]);
        s.split('\u{1}').next().unwrap().to_string()
    }

    fn is_stale(e: &Editor) -> bool {
        e.status.as_ref().is_some_and(|s| s.starts_with(&stale_prefix()))
    }

    #[test]
    fn committed_with_reload_error_is_warning_and_view_opens() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        break_reload(&mut e);
        // committed, only the reload fails: the new view still opens, the status is a warning
        let id2 = create(&mut e).expect("committed create returns the view");
        assert_eq!(current(&e), Some(id2));
        assert!(is_stale(&e));
        let copy = duplicate(&mut e, id).expect("committed duplicate returns the copy");
        assert_eq!(current(&e), Some(copy));
        assert!(rename(&mut e, id, "Eave"));
        assert!(is_stale(&e));
        assert_eq!(e.model.drafting(id).unwrap().name, "Eave");
        assert!(set_scale(&mut e, id, 5.0));
        assert!(delete(&mut e, copy));
        assert_eq!(active(), None);
        assert!(e.model.drafting(copy).is_err());
        assert!(is_stale(&e));
    }

    #[test]
    fn an_op_that_fails_before_commit_stays_a_failure() {
        let mut e = editor();
        let ran = run(&mut e, |m| m.rename_drafting(9999, "x"));
        assert_eq!(ran, Ran::Failed);
        assert!(!is_stale(&e));
    }

    #[test]
    fn rename_commits_on_focus_loss_esc_cancels() {
        assert_eq!(rename_end(false, false), RenameEnd::Keep);
        assert_eq!(rename_end(false, true), RenameEnd::Keep);
        assert_eq!(rename_end(true, false), RenameEnd::Commit);
        assert_eq!(rename_end(true, true), RenameEnd::Cancel);
        // typed names are validated on commit: empty and taken names are refused
        let mut e = editor();
        let a = create(&mut e).unwrap();
        let b = create(&mut e).unwrap();
        let taken = e.model.drafting(a).unwrap().name;
        let own = e.model.drafting(b).unwrap().name;
        assert!(!rename(&mut e, b, "  "));
        assert!(!rename(&mut e, b, &taken));
        assert_eq!(e.model.drafting(b).unwrap().name, own);
        assert!(rename(&mut e, b, " Detail "));
        assert_eq!(e.model.drafting(b).unwrap().name, "Detail");
    }

    #[test]
    fn delete_with_annos_asks_empty_deletes() {
        let mut e = editor();
        let full = create(&mut e).unwrap();
        let empty = create(&mut e).unwrap();
        let line = ddc_model::AnnoShape::Line {
            pts: vec![(0.0, 0.0), (1.0, 0.0)],
            closed: false,
        };
        assert!(run(&mut e, move |m| m
            .add_anno(AnnoView::Drafting(full), &line, None, None)
            .map(|_| ()))
        .ok());
        assert!(request_delete(&mut e, empty), "an empty view goes at once");
        assert!(e.model.drafting(empty).is_err());
        assert!(!request_delete(&mut e, full), "annotations: ask first");
        assert!(e.model.drafting(full).is_ok());
        assert_eq!(with(|s| s.confirm), Some(full));
        assert!(delete(&mut e, full));
        assert_eq!(with(|s| s.confirm), None);
        assert!(e.model.drafting(full).is_err());
    }

    #[test]
    fn confirm_cleared_on_close() {
        let _ = editor();
        with(|s| {
            s.list_open = true;
            s.confirm = Some(3);
            s.rename = Some((3, "x".into()));
        });
        sync_state(5);
        assert_eq!(with(|s| s.confirm), Some(3), "open and unchanged: kept");
        with(|s| s.list_open = false);
        sync_state(5);
        assert_eq!(with(|s| s.confirm), None);
        assert_eq!(with(|s| s.rename.clone()), None);
        with(|s| s.list_open = true);
        sync_state(5);
        assert_eq!(with(|s| s.confirm), None, "reopened without the question");
    }

    #[test]
    fn confirm_cleared_on_model_replace() {
        let _ = editor();
        with(|s| s.list_open = true);
        sync_state(1);
        with(|s| {
            s.confirm = Some(3);
            s.rename = Some((3, "x".into()));
        });
        sync_state(1);
        assert_eq!(with(|s| s.confirm), Some(3));
        sync_state(2);
        assert_eq!(with(|s| s.confirm), None);
        assert_eq!(with(|s| s.rename.clone()), None);
    }

    #[test]
    fn enter_does_not_confirm_delete() {
        assert!(delete_confirmed(true, false));
        assert!(!delete_confirmed(true, true), "Enter on a focused button");
        assert!(!delete_confirmed(false, true));
        assert!(!delete_confirmed(false, false));
    }

    #[test]
    fn tab_ids_do_not_meet_sheet_tabs() {
        for id in [1, 7, 1_000_000] {
            assert_eq!(of_tab(tab_id(id)), Some(id));
        }
        // sheet tabs are `-sheet id`, section tabs are positive
        for t in [-1, -42, -1_000_000, 0, 5] {
            assert_eq!(of_tab(t), None);
        }
    }

    #[test]
    fn opening_a_drafting_view_closes_the_sheet_canvas() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        crate::sheet_view::with(|s| s.active = Some(3));
        open(id);
        assert_eq!(crate::sheet_view::active(), None);
        deactivate();
        assert_eq!(active(), None);
    }

    #[test]
    fn switching_to_3d_section_or_level_closes_the_drafting_tab() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        follow(false, None, 1);
        follow(false, None, 1);
        assert_eq!(current(&e), Some(id));
        follow(true, None, 1);
        assert_eq!(active(), None);
        open(id);
        follow(true, None, 1);
        assert_eq!(active(), Some(id));
        follow(false, Some(4), 1);
        assert_eq!(active(), None);
        open(id);
        follow(false, Some(4), 2);
        assert_eq!(active(), None);
    }

    fn line_in(e: &mut Editor, id: i64) {
        let line = ddc_model::AnnoShape::Line {
            pts: vec![(0.0, 0.0), (1.5, 0.0)],
            closed: false,
        };
        assert!(run(e, move |m| m
            .add_anno(AnnoView::Drafting(id), &line, None, None)
            .map(|_| ()))
        .ok());
    }

    #[test]
    fn create_rename_delete_are_one_undo_each() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        let first = e.model.drafting(id).unwrap().name;
        line_in(&mut e, id);
        assert!(rename(&mut e, id, "Sill"));
        // one undo takes back the rename only
        e.undo();
        assert_eq!(e.model.drafting(id).unwrap().name, first);
        assert_eq!(e.model.annos(AnnoView::Drafting(id)).unwrap().len(), 1);
        assert!(delete(&mut e, id));
        assert!(e.model.drafting(id).is_err());
        // one undo brings back the view with its annotation
        e.undo();
        assert_eq!(e.model.drafting(id).unwrap().name, first);
        assert_eq!(e.model.annos(AnnoView::Drafting(id)).unwrap().len(), 1);
        e.undo();
        assert!(e.model.annos(AnnoView::Drafting(id)).unwrap().is_empty());
        // and one more removes the create itself
        e.undo();
        assert!(e.model.drafting(id).is_err());
        assert!(e.model.draftings().unwrap().is_empty());
    }

    #[test]
    fn drafting_content_survives_save_and_reopen() {
        with(|s| *s = State::default());
        let ru = crate::ui_lang_now();
        let d = std::env::temp_dir().join(format!("ddc-dv-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("a.ddc");
        let mut e = Editor::start_project().unwrap();
        e.save_project_as(&p).unwrap();
        assert!(!e.is_dirty());
        let id = create(&mut e).unwrap();
        assert!(e.is_dirty(), "a new drafting view is something to lose");
        line_in(&mut e, id);
        assert!(rename(&mut e, id, "Eave"));
        assert!(set_scale(&mut e, id, 5.0));
        let before = e.model.annos(AnnoView::Drafting(id)).unwrap();
        e.save_project().unwrap();
        assert!(!e.is_dirty());
        e.open_project(&p).unwrap();
        let back = e.model.drafting(id).unwrap();
        assert_eq!((back.name.as_str(), back.scale), ("Eave", 5.0));
        let after = e.model.annos(AnnoView::Drafting(id)).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].shape, before[0].shape);
        assert!(!e.is_dirty());
        // opening a project leaves the UI language alone
        assert_eq!(crate::ui_lang_now(), ru);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn browser_rename_and_delete_go_through_the_manager() {
        let mut e = editor();
        let id = create(&mut e).unwrap();
        line_in(&mut e, id);
        browser_act(&mut e, Row::Rename(id));
        let name = e.model.drafting(id).unwrap().name;
        assert_eq!(with(|s| (s.list_open, s.rename.clone())), (true, Some((id, name))));
        with(|s| *s = State::default());
        // a view with annotations is not deleted from the browser without the question
        browser_act(&mut e, Row::Delete(id));
        assert!(e.model.drafting(id).is_ok());
        assert_eq!(with(|s| (s.list_open, s.confirm)), (true, Some(id)));
        browser_act(&mut e, Row::Open(id));
        assert_eq!(active(), Some(id));
    }

    #[test]
    fn grid_step_keeps_lines_apart() {
        for px in [4.0, 37.0, 100.0, 999.0, 20_000.0] {
            let s = grid_step(px);
            assert!(s * px >= GRID_PX - 1e-9, "{px} {s}");
            assert!(s * px < GRID_PX * 5.0 + 1e-9, "{px} {s}");
        }
    }

    // ---- manager window, headless, explicit language ----

    use crate::layout_contract::{app_in, driven_ctx, pass, probe, violations, Probe};
    use ddc_ui::Lang;
    use egui::accesskit::Role as Ak;

    fn lang(ru: bool) -> Lang {
        if ru {
            Lang::Ru
        } else {
            Lang::En
        }
    }

    fn tl(ru: bool, key: &str) -> String {
        crate::tr_lang(ru, key, &[])
    }

    fn list_app(ru: bool, names: &[&str]) -> EditorApp {
        with(|s| *s = State::default());
        let mut app = app_in(lang(ru));
        for n in names {
            let id = create(&mut app.ed).unwrap();
            assert!(rename(&mut app.ed, id, n));
        }
        show_list();
        app
    }

    fn list_probe(app: &mut EditorApp, ru: bool) -> Probe {
        probe("dv1.list", |ctx| window_in(app, ctx, ru))
    }

    fn count(p: &Probe, label: &str) -> usize {
        p.controls.iter().filter(|c| c.label == label).count()
    }

    fn add_line(ed: &mut Editor, id: i64) {
        let line = ddc_model::AnnoShape::Line {
            pts: vec![(0.0, 0.0), (1.0, 0.0)],
            closed: false,
        };
        let ran = run(ed, move |m| {
            m.add_anno(AnnoView::Drafting(id), &line, None, None).map(|_| ())
        });
        assert!(ran.ok());
    }

    fn long_name_ends_before_the_scale(ru: bool) {
        let name = "W".repeat(100);
        let mut app = list_app(ru, &[&name, "Detail"]);
        let p = list_probe(&mut app, ru);
        let nm = p.controls.iter().find(|c| c.label == name).expect("name control");
        let scales: Vec<_> = p.controls.iter().filter(|c| c.role == Ak::ComboBox).collect();
        assert_eq!(scales.len(), 2, "one scale per row");
        let own = scales
            .iter()
            .find(|c| (c.rect.center().y - nm.rect.center().y).abs() < 2.0)
            .expect("scale on the name's row");
        assert!(nm.rect.right() < own.rect.left(), "{:?} vs {:?}", nm.rect, own.rect);
        assert!(p.elided.contains(&name), "a long name is cut by width");
        assert!(!p.elided.iter().any(|e| e == "Detail"), "a short name is not");
        let bad: Vec<String> = violations(&p, false)
            .into_iter()
            .filter(|v| !v.contains("is elided"))
            .collect();
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn long_name_ends_before_the_scale_en() {
        long_name_ends_before_the_scale(false);
    }

    #[test]
    fn long_name_ends_before_the_scale_ru() {
        long_name_ends_before_the_scale(true);
    }

    fn no_danger_button_per_row(ru: bool) {
        let mut app = list_app(ru, &["Eave", "Stair", "Door"]);
        let p = list_probe(&mut app, ru);
        for key in ["drafting_view.delete", "drafting_view.rename", "drafting_view.duplicate"] {
            assert_eq!(count(&p, &tl(ru, key)), 0, "{key} while the menu is closed");
        }
        assert_eq!(count(&p, &tl(ru, "drafting_view.more")), 3);
        assert_eq!(count(&p, &tl(ru, "drafting_view.open")), 3);
        let has_cyr = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let any = p.controls.iter().any(|c| has_cyr(&c.label));
        assert_eq!(any, ru, "the window speaks the language it was given");
    }

    #[test]
    fn no_danger_button_per_row_en() {
        no_danger_button_per_row(false);
    }

    #[test]
    fn no_danger_button_per_row_ru() {
        no_danger_button_per_row(true);
    }

    #[test]
    fn manager_uses_kit_widgets_and_one_danger_button() {
        let src = include_str!("drafting_view.rs");
        let code = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for raw in ["ComboBox", "TextEdit", "desired_width("] {
            assert!(!code.contains(raw), "raw widget {raw} in the drafting views");
        }
        assert_eq!(code.matches("Button::danger(").count(), 1, "only the delete question");
    }

    /// Menu item at `label`, pressed and released with the pointer over it.
    fn click(
        step: &mut dyn FnMut(Vec<egui::Event>) -> Probe,
        label: &str,
    ) -> Probe {
        let p = step(vec![]);
        let c = p.controls.iter().find(|c| c.label == label).expect(label);
        let at = c.rect.center();
        let btn = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![egui::Event::PointerMoved(at)]);
        step(vec![btn(true)]);
        step(vec![btn(false)]);
        step(vec![]);
        step(vec![])
    }

    fn menu_asks_then_deletes_in_one_undo_step(ru: bool) {
        let mut app = list_app(ru, &["Eave"]);
        let id = by_name(&app.ed, "Eave").unwrap();
        add_line(&mut app.ed, id);
        let (ctx, mut th) = driven_ctx();
        let wid = egui::Id::new(("ddc.dialog", "dv1.list"));
        let mut step = |ev: Vec<egui::Event>| {
            pass(&ctx, &mut th, wid, ev, &mut |c| window_in(&mut app, c, ru))
        };
        for _ in 0..4 {
            step(vec![]);
        }
        let p = click(&mut step, &tl(ru, "drafting_view.more"));
        for key in ["drafting_view.rename", "drafting_view.duplicate", "drafting_view.delete"] {
            assert_eq!(count(&p, &tl(ru, key)), 1, "{key} in the open menu");
        }
        let p = click(&mut step, &tl(ru, "drafting_view.delete"));
        assert_eq!(with(|s| s.confirm), Some(id), "a view with annotations asks first");
        assert_eq!(count(&p, &tl(ru, "drafting_view.more")), 0, "the question replaces the actions");
        assert_eq!(count(&p, &tl(ru, "drafting_view.delete_yes")), 1);
        assert_eq!(count(&p, &tl(ru, "drafting_view.cancel")), 1);
        let p = click(&mut step, &tl(ru, "drafting_view.cancel"));
        assert_eq!(with(|s| s.confirm), None);
        assert_eq!(count(&p, &tl(ru, "drafting_view.more")), 1);
        click(&mut step, &tl(ru, "drafting_view.more"));
        click(&mut step, &tl(ru, "drafting_view.delete"));
        click(&mut step, &tl(ru, "drafting_view.delete_yes"));
        assert!(app.ed.model.drafting(id).is_err(), "deleted after the question");
        app.ed.undo();
        assert!(app.ed.model.drafting(id).is_ok(), "one undo brings it back");
        assert_eq!(app.ed.model.annos(AnnoView::Drafting(id)).unwrap().len(), 1);
    }

    #[test]
    fn menu_asks_then_deletes_in_one_undo_step_en() {
        menu_asks_then_deletes_in_one_undo_step(false);
    }

    #[test]
    fn menu_asks_then_deletes_in_one_undo_step_ru() {
        menu_asks_then_deletes_in_one_undo_step(true);
    }

    fn rename_field_replaces_the_name(ru: bool) {
        let mut app = list_app(ru, &["Eave", "Stair"]);
        let id = by_name(&app.ed, "Eave").unwrap();
        with(|s| s.rename = Some((id, "Eave".into())));
        let p = list_probe(&mut app, ru);
        let fields = p.controls.iter().filter(|c| c.role == Ak::TextInput).count();
        assert_eq!(fields, 1);
        let names = |n: &str| {
            let hit = |c: &&crate::layout_contract::Control| c.role == Ak::Button && c.label == n;
            p.controls.iter().filter(hit).count()
        };
        assert_eq!(names("Eave"), 0, "the name is the field now");
        assert_eq!(names("Stair"), 1);
        let bad = violations(&p, false);
        assert!(bad.is_empty(), "{bad:?}");
    }

    #[test]
    fn rename_field_replaces_the_name_en() {
        rename_field_replaces_the_name(false);
    }

    #[test]
    fn rename_field_replaces_the_name_ru() {
        rename_field_replaces_the_name(true);
    }
}
