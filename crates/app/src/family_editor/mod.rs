//! Family editor (schema 29): separate tabbed documents in their own window. The definition text is
//! the source of truth (`FamDoc`, its own undo history), a reference-plane sketch with dragging,
//! parameters, a type catalog, constraints, visibility, nesting, value checks (flex).
//! Into the project only via "Load into project": one `run_op` operation, instances are rebuilt.
//! Drafts are saved bypassing the project journal (a document has its own undo history).

pub mod ai_draft;
mod canvas;
pub mod catalog_import;
mod chrome;
mod load_set;
mod overwrite;
mod profile_sketch;
mod tab_strip;
mod tabs;

pub use overwrite::Resume;

use crate::ui_kit as kit;
use std::cell::RefCell;

use ddc_geom::component::doc::FamDoc;
use ddc_geom::component::rig::{self, RigReport};
use ddc_geom::component::{expr, flex, thumb, Built, Category, Family, Hosting, Lod, Resolved};
use ddc_geom::sketch_solve::Verdict;
use ddc_ui::theme::{ControlSize, Role};
use ddc_ui::widgets::{self, Button};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};

use crate::editor::Editor;
use crate::shell::EditorApp;

pub const HOSTS: [Hosting; 5] = [
    Hosting::None,
    Hosting::Wall,
    Hosting::Floor,
    Hosting::Ceiling,
    Hosting::Face,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Params,
    Types,
    Constraints,
    Solids,
    Visibility,
    Nested,
    Flex,
    Text,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Params,
        Tab::Types,
        Tab::Constraints,
        Tab::Solids,
        Tab::Visibility,
        Tab::Nested,
        Tab::Flex,
        Tab::Text,
    ];
    fn short_key(self) -> &'static str {
        match self {
            Tab::Params => "fe.tab.params.short",
            Tab::Types => "fe.tab.types.short",
            Tab::Constraints => "fe.tab.constraints.short",
            Tab::Solids => "fe.tab.solids.short",
            Tab::Visibility => "fe.tab.visibility.short",
            Tab::Nested => "fe.tab.nested.short",
            Tab::Flex => "fe.tab.flex.short",
            Tab::Text => "fe.tab.text.short",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Tab::Params => "fe.tab.params",
            Tab::Types => "fe.tab.types",
            Tab::Constraints => "fe.tab.constraints",
            Tab::Solids => "fe.tab.solids",
            Tab::Visibility => "fe.tab.visibility",
            Tab::Nested => "fe.tab.nested",
            Tab::Flex => "fe.tab.flex",
            Tab::Text => "fe.tab.text",
        }
    }
}

/// Parse, evaluation by the preview type, the sketch and the body of the current text.
#[derive(Default)]
pub struct Eval {
    pub text: String,
    pub ty: Option<String>,
    pub family: Option<Family>,
    pub error: Option<String>,
    pub resolved: Option<Resolved>,
    pub rig: Option<RigReport>,
    pub built: Option<Built>,
}

impl Eval {
    pub fn compute(text: &str, ty: Option<&str>) -> Eval {
        let mut e = Eval {
            text: text.to_string(),
            ty: ty.map(str::to_string),
            ..Default::default()
        };
        let f = match Family::parse(text) {
            Ok(f) => f,
            Err(x) => {
                e.error = Some(x.to_string());
                return e;
            }
        };
        let tv = ty
            .and_then(|t| f.type_values(t))
            .map(|v| v.to_vec())
            .or_else(|| f.types.first().map(|t| t.values.clone()))
            .unwrap_or_default();
        match f.resolve(&tv, &[]) {
            Err(x) => e.error = Some(x.to_string()),
            Ok(r) => {
                if !f.ext.is_empty_rig() {
                    e.rig = f.rig_report(&r).ok();
                }
                match f.build(&r, Lod::Medium) {
                    Ok(b) => e.built = Some(b),
                    Err(x) => e.error = Some(x.to_string()),
                }
                e.resolved = Some(r);
            }
        }
        e.family = Some(f);
        e
    }
}

/// Check mode: trial values and a parameter sweep.
#[derive(Default)]
pub struct FlexUi {
    pub trial: Vec<(String, String)>,
    pub last: Option<flex::Outcome>,
    pub param: String,
    /// Sweep bounds in model units (metres, radians).
    pub from: f64,
    pub to: f64,
    pub steps: usize,
    /// Selected row of the trial table.
    pub sel: usize,
    pub series: Vec<(f64, flex::Outcome)>,
    /// The sketch shows the last probe, not the type.
    pub show: bool,
}

/// Tab form fields.
#[derive(Default)]
pub struct Forms {
    pub p_name: String,
    pub p_kind: usize,
    pub p_inst: bool,
    pub p_val: String,
    pub r_name: String,
    pub r_axis: usize,
    pub r_free: bool,
    pub r_val: String,
    pub c_kind: usize,
    pub c_val: String,
    pub c_a: String,
    pub c_b: String,
    pub c_list: String,
    pub l_name: String,
    pub l_x: String,
    pub l_y: String,
    pub l_angle: String,
    pub k_solid: String,
    pub k_clause: String,
    pub k_arg: usize,
    pub k_ref: String,
    pub s_kind: usize,
    pub s_name: String,
    pub s_json: String,
    /// Solid form bounds in model units (metres, radians).
    pub s_a: f64,
    pub s_b: f64,
    /// Name of the selected solid definition.
    pub s_sel: String,
    pub vis: Vec<(String, String)>,
    pub vis_for: String,
    pub vis_sel: Option<usize>,
    pub vis_always: bool,
    pub vis_cond: String,
    pub n_name: String,
    pub n_child: String,
    pub n_type: String,
    pub n_param: String,
    pub n_by_param: bool,
    pub n_at: String,
    pub n_rot: String,
    pub n_set: String,
    pub o: [String; 3],
    pub csv: String,
    /// Type catalog import waiting for Apply.
    pub csv_preview: Option<catalog_import::Preview>,
    pub raw: String,
    pub raw_for: String,
}

pub struct Session {
    /// Stable identity of the document for the life of the window.
    pub serial: u64,
    pub doc: FamDoc,
    /// The project family the document is linked to.
    pub family: Option<i64>,
    pub draft: Option<i64>,
    /// The assistant's draft: what it is compared with and its report.
    pub ai: Option<ai_draft::Meta>,
    pub tab: Tab,
    /// (error?, text) of the last edit.
    pub msg: Option<(bool, String)>,
    pub eval: Eval,
    pub ty: Option<String>,
    pub view: canvas::View,
    pub f: Forms,
    pub flex: FlexUi,
    pub overwrite_types: bool,
    /// Profile sketch of the Solids tab (own undo history).
    pub psk: profile_sketch::Panel,
    thumb: Option<(String, egui::TextureHandle)>,
}

impl Session {
    pub fn new(doc: FamDoc, family: Option<i64>) -> Self {
        Self {
            serial: NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            doc,
            family,
            draft: None,
            ai: None,
            tab: Tab::default(),
            msg: None,
            eval: Eval::default(),
            ty: None,
            view: canvas::View::default(),
            f: Forms {
                p_val: "100mm".into(),
                r_val: "0mm".into(),
                k_clause: "rect".into(),
                s_json: "{\"closed\": true, \"pts\": [[0, 0, 0], [0.5, 0, 0], [0.5, 0.5, 0], [0, 0.5, 0]]}".into(),
                s_b: 0.5,
                n_at: "0, 0, 0".into(),
                o: ["0".into(), "0".into(), "0".into()],
                ..Default::default()
            },
            flex: FlexUi {
                steps: 10,
                ..Default::default()
            },
            overwrite_types: false,
            psk: profile_sketch::Panel::default(),
            thumb: None,
        }
    }

    pub fn name(&self) -> String {
        self.eval
            .family
            .as_ref()
            .map(|f| f.name.clone())
            .unwrap_or_else(|| {
                let t = self.doc.text();
                t.lines()
                    .find_map(|l| l.trim().strip_prefix("family "))
                    .unwrap_or("?")
                    .trim()
                    .to_string()
            })
    }

    /// Recompute after a text edit or a preview type change.
    pub fn refresh(&mut self) {
        if self.eval.text != self.doc.text() || self.eval.ty != self.ty {
            self.eval = Eval::compute(self.doc.text(), self.ty.as_deref());
        }
    }

    /// Result of a document edit: an error stays in the message line, the document is unchanged.
    pub fn apply(&mut self, cat: &Catalog, r: Result<(), ddc_geom::component::FamilyError>) {
        self.msg = match r {
            Ok(()) => None,
            Err(e) => Some((true, cat.tf("fe.err.edit", &[("msg", &e.to_string())]))),
        };
        self.refresh();
    }

    /// Ctrl+Z (`redo` false) or Ctrl+Y. One rule: inside the sketch panel the key belongs to
    /// the sketch history, elsewhere to the family document; an empty sketch history never
    /// falls through to the document.
    pub fn undo_key(&mut self, redo: bool, in_sketch: bool) -> bool {
        if in_sketch {
            return if redo { self.psk.redo() } else { self.psk.undo() };
        }
        let r = if redo { self.doc.redo() } else { self.doc.undo() };
        self.refresh();
        r
    }
}

/// Close request waiting for an answer. A tab is named by its document serial, never by
/// its index, so the question stays with the same family when tabs shift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Tab(u64),
    Window,
}

/// Answer to the close question; Keep is the default and the Esc key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Load,
    Discard,
    Keep,
}

static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Documents and the active tab (UI thread).
#[derive(Default)]
pub struct State {
    pub docs: Vec<Session>,
    pub active: usize,
    pub open: bool,
    /// Window rect and layer of the last frame: undo keys over it belong to the editor.
    pub win: Option<(egui::Rect, egui::LayerId)>,
    /// Undo keys taken from the shell this frame (`true` = redo).
    pub keys: Vec<bool>,
    pub ask: Option<Ask>,
    /// Serial of the document whose draft name is held by another draft (save is pending).
    pub clash: Option<u64>,
    /// Load waiting for the "replace project type values" answer.
    pub ow: Option<overwrite::OwAsk>,
    /// How the overwrite question raised by the project's Save ended; read by `project_answer`.
    pub resume: Option<overwrite::Resume>,
    /// Project lists and the model revision they were read at (no SQL on every frame).
    pub project: Project,
    pub project_rev: Option<(u64, u64)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// `Some(redo)` for Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y presses.
fn undo_press(e: &egui::Event) -> Option<bool> {
    let egui::Event::Key {
        key,
        pressed: true,
        modifiers,
        ..
    } = e
    else {
        return None;
    };
    if !modifiers.command || modifiers.alt {
        return None;
    }
    match key {
        egui::Key::Z => Some(modifiers.shift),
        egui::Key::Y if !modifiers.shift => Some(true),
        _ => None,
    }
}

/// Removes undo presses from the input; repeats are removed but not returned.
fn take_undo_keys(i: &mut egui::InputState) -> Vec<bool> {
    let mut out = vec![];
    i.events.retain(|e| match undo_press(e) {
        Some(redo) => {
            if !matches!(e, egui::Event::Key { repeat: true, .. }) {
                out.push(redo);
            }
            false
        }
        None => true,
    });
    out
}

fn focus_inside(ctx: &egui::Context, r: egui::Rect) -> bool {
    ctx.memory(|m| m.focused())
        .and_then(|id| ctx.read_response(id))
        .is_some_and(|w| r.contains_rect(w.rect))
}

fn pointer_in(ctx: &egui::Context, r: egui::Rect) -> bool {
    ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| r.contains(p))
}

/// Called by the shell before its shortcuts: while the pointer or focus is in the editor
/// window, Ctrl+Z/Ctrl+Y belong to it and never reach the project undo. A text field being
/// edited keeps its own undo.
pub fn grab_keys(ctx: &egui::Context) {
    with(|st| {
        let Some((rect, layer)) = st.win.filter(|_| st.open) else {
            return;
        };
        if ctx.wants_keyboard_input() {
            return;
        }
        if st.ask.is_some() || st.ow.is_some() {
            ctx.input_mut(take_undo_keys);
            return;
        }
        let over = ctx
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| rect.contains(p) && ctx.layer_id_at(p) == Some(layer));
        if over || focus_inside(ctx, rect) {
            let k = ctx.input_mut(take_undo_keys);
            st.keys.extend(k);
        }
    });
}

/// A document that would lose work when dropped: unloaded edits or a sketch.
fn needs_ask(d: &Session) -> bool {
    d.doc.dirty || !d.psk.is_empty()
}

fn pos(st: &State, serial: u64) -> Option<usize> {
    st.docs.iter().position(|d| d.serial == serial)
}

/// Tab close: a document with unloaded edits or a sketch asks first, a clean one closes at once.
fn close_tab(st: &mut State, i: usize) {
    if i >= st.docs.len() {
        return;
    }
    if needs_ask(&st.docs[i]) {
        st.ask = Some(Ask::Tab(st.docs[i].serial));
        return;
    }
    st.docs.remove(i);
    st.active = st.active.min(st.docs.len().saturating_sub(1));
    if st.docs.is_empty() {
        st.open = false;
    }
}

/// Window close: asks once while any family has unloaded edits or a sketch.
fn close_window(st: &mut State) {
    if st.docs.iter().any(needs_ask) {
        st.open = true;
        st.ask = Some(Ask::Window);
    } else {
        st.open = false;
    }
}

/// Puts the documents at the given indices into the project as one undo step, all or none;
/// Err is the index of the refused family.
type Load<'a> = dyn FnMut(&mut [Session], &[usize]) -> Result<(), usize> + 'a;

/// Indices of the documents with unloaded edits, in tab order.
fn dirty_ix(st: &State) -> Vec<usize> {
    (0..st.docs.len()).filter(|&i| st.docs[i].doc.dirty).collect()
}

/// Answer to the close question. `load` commits documents to the project; a document is
/// closed only after the model took it.
fn answer(st: &mut State, choice: Choice, load: &mut Load<'_>) {
    let Some(a) = st.ask.take() else {
        return;
    };
    let shown = match a {
        Ask::Tab(sn) => pos(st, sn),
        Ask::Window => st.docs.iter().position(needs_ask),
    };
    match (choice, a) {
        (Choice::Keep, _) => {
            if let Some(i) = shown {
                if !st.docs[i].psk.is_empty() {
                    st.docs[i].tab = Tab::Solids;
                }
                st.active = i;
            }
        }
        (Choice::Discard, Ask::Tab(_)) => {
            if let Some(i) = shown {
                st.docs.remove(i);
            }
        }
        (Choice::Discard, Ask::Window) => {
            st.docs.retain(|d| !d.doc.dirty);
            for d in &mut st.docs {
                d.psk = profile_sketch::Panel::default();
            }
            st.open = false;
        }
        (Choice::Load, Ask::Tab(_)) => {
            if let Some(i) = shown {
                if load(&mut st.docs, &[i]).is_ok() {
                    st.docs.remove(i);
                } else {
                    st.active = i;
                }
            }
        }
        (Choice::Load, Ask::Window) => {
            // all edited families in one step: a refusal loads none and keeps the window open
            let dirty = dirty_ix(st);
            if let Err(i) = load(&mut st.docs, &dirty) {
                st.active = i;
                return;
            }
            for d in &mut st.docs {
                d.psk = profile_sketch::Panel::default();
            }
            st.open = false;
        }
    }
    st.active = st.active.min(st.docs.len().saturating_sub(1));
    if st.docs.is_empty() {
        st.open = false;
    }
}

fn ask_modal(ctx: &egui::Context, th: &Theme, cat: &Catalog, st: &mut State) -> Option<Choice> {
    let a = st.ask?;
    let (body, title, dirty) = match a {
        Ask::Tab(sn) => {
            let Some(d) = st.docs.iter().find(|d| d.serial == sn) else {
                st.ask = None; // the family is gone; nothing to ask about
                return None;
            };
            if d.doc.dirty {
                let name = d.name();
                (cat.tf("profile_sketch.ask.body_dirty", &[("name", &name)]), "profile_sketch.ask.title_dirty", true)
            } else {
                (cat.tf("profile_sketch.ask.body", &[("name", &d.name())]), "profile_sketch.ask.title", false)
            }
        }
        Ask::Window => {
            let names = dirty_names(st);
            if names.is_empty() {
                (cat.t("profile_sketch.ask.body_window").to_string(), "profile_sketch.ask.title", false)
            } else {
                (
                    cat.tf("profile_sketch.ask.body_window_dirty", &[("names", &names.join(", "))]),
                    "profile_sketch.ask.title_dirty",
                    true,
                )
            }
        }
    };
    let mut pick = None;
    let m = kit::modal_dialog("profile_sketch.ask", ctx, |ui| {
        ui.set_width(ddc_ui::theme::layout::DIALOG_TOOL);
        kit::modal_title(ui, cat.t(title));
        ui.label(RichText::new(body).color(th.c("text.secondary")));
        kit::dialog_footer(ui, |ui| {
            if kit::primary_action(ui, cat.t("profile_sketch.ask.keep"), None) {
                pick = Some(Choice::Keep);
            }
            if dirty && kit::primary_click(ui, cat.t("profile_sketch.ask.load"), None) {
                pick = Some(Choice::Load);
            }
            let discard = if dirty {
                "profile_sketch.ask.discard_changes"
            } else {
                "profile_sketch.ask.discard"
            };
            if kit::confirm_danger_button(ui, cat.t(discard), true).clicked() {
                pick = Some(Choice::Discard);
            }
        });
    });
    // Esc or a click on the backdrop keeps editing
    if pick.is_none() && m.should_close() {
        pick = Some(Choice::Keep);
    }
    pick
}

/// Names of the families with unloaded edits, in tab order.
fn dirty_names(st: &State) -> Vec<String> {
    st.docs
        .iter()
        .filter(|d| d.doc.dirty)
        .map(|d| d.name())
        .collect()
}

/// Families a project close or swap would lose (unloaded edits or a sketch), in tab order.
pub fn draft_names() -> Vec<String> {
    STATE.with(|s| s.try_borrow().map_or_else(|_| vec![], |st| draft_list(&st)))
}

fn draft_list(st: &State) -> Vec<String> {
    st.docs.iter().filter(|d| needs_ask(d)).map(|d| d.name()).collect()
}

/// Discard on the project question: the same drop as Discard on the window question.
pub fn discard_drafts() {
    with(drop_drafts);
}

fn drop_drafts(st: &mut State) {
    st.ask = None;
    // the dropped edits were what the overwrite question was about
    st.ow = None;
    st.resume = None;
    st.docs.retain(|d| !d.doc.dirty);
    for d in &mut st.docs {
        d.psk = profile_sketch::Panel::default();
    }
    st.active = st.active.min(st.docs.len().saturating_sub(1));
    if st.docs.is_empty() {
        st.open = false;
    }
}

/// Drops the sketches, as Load on the window question does: called only once the project save
/// that followed `save_drafts` succeeded (or had nothing to write).
pub fn drop_sketches() {
    with(|st| {
        for d in &mut st.docs {
            d.psk = profile_sketch::Panel::default();
        }
    });
}

/// Save on the project question: loads every edited family into the model; sketches stay
/// until `drop_sketches`. One undo step for all; a refusal loads none, shows that family and
/// returns false. Values it would replace are asked in the window first (`asks_overwrite`):
/// false then too, and the answer comes through `project_answer`.
pub fn save_drafts(ed: &mut Editor, cat: &Catalog) -> bool {
    // taken out like `windows` does: the model op never runs under the state borrow
    let mut st = with(std::mem::take);
    st.resume = None;
    // replacing project type values is asked in the window first, never done silently
    if let Some(mut q) = close_overwrites(&ed.model, &st, Ask::Window) {
        q.then = overwrite::Then::Project;
        st.active = pos(&st, q.first).unwrap_or(st.active);
        st.open = true;
        st.ask = None;
        st.ow = Some(q);
        with(|s| *s = st);
        return false;
    }
    let ok = load_all(&mut st, &mut |d, w| load_set::load_set(ed, cat, d, w));
    with(|s| *s = st);
    ok
}

/// The project's Save stopped on the overwrite question, which the family window now shows.
pub fn asks_overwrite() -> bool {
    with(|st| asks_project(st))
}

fn asks_project(st: &State) -> bool {
    st.ow.as_ref().is_some_and(|q| q.then == overwrite::Then::Project)
}

/// How that question ended; None while it is still shown. A question gone without an answer
/// counts as Back, so the project action never runs unasked.
pub fn project_answer() -> Option<Resume> {
    with(|st| {
        if asks_project(st) {
            None
        } else {
            Some(st.resume.take().unwrap_or(Resume::Back))
        }
    })
}

/// The overwrite question for Load on a close question; `None` when no value changes.
fn close_overwrites(m: &ddc_model::Model, st: &State, a: Ask) -> Option<overwrite::OwAsk> {
    let docs = st.docs.iter().filter(|d| match a {
        Ask::Tab(sn) => d.serial == sn,
        Ask::Window => d.doc.dirty,
    });
    let then = overwrite::Then::Close(a);
    overwrite::ask(docs, then, &mut |def| m.type_overwrites(def).unwrap_or_default())
}

/// Answer to the overwrite question: Replace runs the waiting load, Back shows the family and
/// leaves the model as it was. The project's question learns the end through `st.resume`.
fn confirm_overwrite(st: &mut State, a: overwrite::OwAsk, yes: bool, load: &mut Load<'_>) {
    if !yes {
        st.active = pos(st, a.first).unwrap_or(st.active);
        if a.then == overwrite::Then::Project {
            st.resume = Some(Resume::Back);
        }
        return;
    }
    match a.then {
        overwrite::Then::Load(sn) => {
            if let Some(i) = pos(st, sn) {
                st.active = i;
                let _ = load(&mut st.docs, &[i]);
            }
        }
        overwrite::Then::Close(q) => {
            st.ask = Some(q);
            answer(st, Choice::Load, load);
        }
        overwrite::Then::Project => {
            // the window stays: sketches go only once the project file is written
            let ok = load_all(st, load);
            st.resume = Some(if ok { Resume::Go } else { Resume::Refused });
        }
    }
}

fn load_all(st: &mut State, load: &mut Load<'_>) -> bool {
    let dirty = dirty_ix(st);
    match load(&mut st.docs, &dirty) {
        Ok(()) => true,
        Err(i) => {
            st.active = i;
            st.open = true;
            false
        }
    }
}

/// A number or constant expression (`600mm`, `30deg`, `2 * 150mm`) in SI.
pub fn parse_num(s: &str) -> Result<f64, String> {
    expr::parse(s.trim())
        .map_err(|e| e.to_string())?
        .eval(&|_| None)
        .map(|q| q.v)
}

fn open_editor(app: &mut EditorApp) {
    let m = &app.ed.model;
    let fam = m.families().ok().and_then(|v| v.first().map(|f| f.0));
    with(|s| {
        s.open = true;
        if !s.docs.is_empty() {
            return;
        }
        let doc = fam
            .and_then(|id| m.family_def(id).ok().map(|d| (FamDoc::new(d), Some(id))))
            .unwrap_or_else(|| (blank(&app.sh.cat), None));
        s.docs.push(Session::new(doc.0, doc.1));
        s.active = 0;
    });
}

fn blank(cat: &Catalog) -> FamDoc {
    let name = cat.t("fe.new_name").replace(char::is_whitespace, "_");
    FamDoc::blank(&name, Category::Generic, Hosting::None)
}

fn new_family(app: &mut EditorApp) {
    let d = blank(&app.sh.cat);
    with(|s| {
        s.open = true;
        s.docs.push(Session::new(d, None));
        s.active = s.docs.len() - 1;
    });
}

fn open_flex(app: &mut EditorApp) {
    open_editor(app);
    with(|s| {
        if let Some(d) = s.docs.get_mut(s.active) {
            d.tab = Tab::Flex;
        }
    });
}

#[cfg(test)]
pub(crate) fn open_for_layout(app: &mut EditorApp) {
    open_editor(app);
}

#[cfg(test)]
pub(crate) use overwrite::layout_project_modal;

pub fn register(app: &mut EditorApp) {
    app.register_command("mgr.family_editor", open_editor);
    app.register_command("mgr.family_new", new_family);
    app.register_command("mgr.family_flex", open_flex);
}

/// Window actions that need the model (run after drawing).
pub enum Action {
    Load,
    SaveDraft,
    OpenFamily(i64),
    OpenDraft(i64),
    Embed(i64),
    New,
    Close(usize),
}

/// Project lists for the window: families and drafts.
#[derive(Clone, Debug, Default)]
pub struct Project {
    pub families: Vec<(i64, String)>,
    pub drafts: Vec<(i64, String, Option<i64>)>,
    /// Project units: a catalog length with no unit in its header is read in these.
    pub imperial: bool,
}

impl Project {
    pub fn length_unit(&self) -> &'static str {
        if self.imperial {
            "in"
        } else {
            "mm"
        }
    }
}

/// The editor window; called every frame from `panel_docks`.
/// `--family-draft demo`: a draft of the assistant that edits the project's table, with the six
/// changed statements and one finding in its report.
pub fn demo_draft(app: &mut EditorApp) -> Result<(), String> {
    const BASE: &str = "family Table
category furniture
param Width length type 1200mm
param Depth length type 600mm
param Height length type 740mm
ref Left x = -Width / 2
ref Right x = Width / 2
ref Front y = -Depth / 2
ref Back y = Depth / 2
type \"1200x600\" Width=1200mm
extrude Top rect(Left, Front, Right, Back) z(Height - 40mm, Height)
extrude LegA rect(Left, Front, Left + 50mm, Front + 50mm) z(0, Height - 40mm)
";
    let new = BASE
        .replace("Width length type 1200mm", "Width length type 1400mm")
        .replace("Depth length type 600mm", "Depth length type 800mm")
        .replace("Height - 40mm, Height)", "Height - 30mm, Height)")
        .replace("Left + 50mm, Front + 50mm) z(0, Height - 40mm)", "Left + 60mm, Front + 60mm) z(0, Height - 30mm)")
        + "param Overhang length type 30mm
extrude Rail rect(Left, Front, Right, Rear) z(0, 20mm)
";
    app.ed.model.load_family(BASE).map_err(|e| e.to_string())?;
    let drafts = vec![("Table".to_string(), new, Some("Table".to_string()), (2u32, 3100u64))];
    with(|st| ai_draft::open(st, &app.ed, drafts));
    Ok(())
}

pub fn windows(ctx: &egui::Context, app: &mut EditorApp) {
    crate::ai::ui::answer_family_offer(ctx);
    let offered = crate::ai::ui::take_family_drafts();
    if !offered.is_empty() {
        with(|st| ai_draft::open(st, &app.ed, offered));
    }
    if !with(|s| s.open) {
        return;
    }
    let mut st = with(std::mem::take);
    let pkey = app.ed.cache_key();
    if st.project_rev != Some(pkey) {
        st.project = Project {
            families: app.ed.model.families().unwrap_or_default(),
            drafts: app.ed.model.fam_docs().unwrap_or_default(),
            imperial: false,
        };
        st.project_rev = Some(pkey);
    }
    st.project.imperial = app.ed.tool.units == crate::units::Units::Imperial;
    let project = std::mem::take(&mut st.project);
    let keys = std::mem::take(&mut st.keys);
    if let Some(s) = st.docs.get_mut(st.active) {
        let in_sketch = s
            .psk
            .area
            .is_some_and(|r| pointer_in(ctx, r) || focus_inside(ctx, r));
        for redo in keys {
            s.undo_key(redo, in_sketch);
        }
    }
    for d in &mut st.docs {
        d.psk.area = None;
    }
    let mut acts = vec![];
    let mut open = true;
    let cat = &app.sh.cat;
    let th = &app.sh.theme;
    let shown = kit::Dialog::new(
        "family_editor",
        cat.t("fe.title"),
        kit::DialogSize::Workspace,
    )
    .open(&mut open)
    .collapsible(true)
    .show(ctx, |ui| body(ui, th, cat, &mut st, &project, &mut acts));
    st.win = shown.map(|r| (r.response.rect, r.response.layer_id));
    if !open && st.ask.is_none() {
        close_window(&mut st);
    }
    st.project = project;
    // drafts are written bypassing the journal (the revision does not change): lists are re-read after actions
    if !acts.is_empty() {
        st.project_rev = None;
    }
    for a in acts {
        act(&mut app.ed, &app.sh.cat, &mut st, a);
    }
    if let Some(c) = ask_modal(ctx, &app.sh.theme, &app.sh.cat, &mut st) {
        let close = st.ask.filter(|_| c == Choice::Load);
        let q = close.and_then(|a| close_overwrites(&app.ed.model, &st, a));
        if let Some(q) = q {
            st.ask = None;
            st.ow = Some(q);
        } else {
            let (ed, cat) = (&mut app.ed, &app.sh.cat);
            answer(&mut st, c, &mut |d, w| load_set::load_set(ed, cat, d, w));
        }
    }
    let yes = st.ow.as_ref().and_then(|q| overwrite::modal(ctx, &app.sh.theme, &app.sh.cat, q));
    if let Some(yes) = yes {
        if let Some(q) = st.ow.take() {
            let (ed, cat) = (&mut app.ed, &app.sh.cat);
            confirm_overwrite(&mut st, q, yes, &mut |d, w| load_set::load_set(ed, cat, d, w));
        }
    }
    if let Some(replace) = clash_modal(ctx, &app.sh.theme, &app.sh.cat, &mut st) {
        if let Some(sn) = st.clash.take() {
            if let Some(i) = pos(&st, sn) {
                st.active = i;
                if replace {
                    st.project_rev = None;
                    save_draft(&mut app.ed, &app.sh.cat, &mut st, true);
                }
            }
        }
    }
    with(|s| *s = st);
}

fn body(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    st: &mut State,
    project: &Project,
    acts: &mut Vec<Action>,
) {
    // document tabs
    chrome::doc_tabs(ui, th, cat, &st.docs, &mut st.active, project, acts);
    st.active = st.active.min(st.docs.len().saturating_sub(1));
    let Some(s) = st.docs.get_mut(st.active) else {
        kit::empty_note(ui, cat.t("fe.empty"), None);
        return;
    };
    s.refresh();
    ui.separator();
    toolbar(ui, th, cat, s, acts);
    header(ui, th, cat, s);
    if let Some((err, m)) = &s.msg {
        if *err {
            kit::error_note(ui, m);
        } else {
            ui.label(RichText::new(m).color(th.c("text.secondary")));
        }
    }
    if let Some(e) = &s.eval.error {
        kit::warn_note(ui, cat.tf("fe.err.family", &[("msg", e)]));
    }
    ai_draft::panel(ui, th, cat, s);
    ui.separator();
    ui.columns(2, |cols| {
        if let Some((name, v)) = canvas::show(&mut cols[0], th, cat, s) {
            drop_plane(cat, s, &name, v);
        }
        let ui = &mut cols[1];
        let labels: Vec<String> = Tab::ALL.iter().map(|t| cat.t(t.key())).collect();
        let short: Vec<String> = Tab::ALL.iter().map(|t| cat.t(t.short_key())).collect();
        let mut sel = Tab::ALL.iter().position(|t| *t == s.tab).unwrap_or(0);
        if let Some(i) = tab_strip::show(ui, th, &labels, &short, &cat.t("fe.tab.more"), &mut sel) {
            s.tab = Tab::ALL[i];
        }
        let out = egui::ScrollArea::vertical()
            .id_salt("g11.fe.tab")
            .show(ui, |ui| tabs::show(ui, th, cat, s, project, acts));
        widgets::scroll_hint::scroll_fade(ui, th, &out);
    });
}

/// End of a plane drag: if constraints determine its position, the plane does not
/// silently "bounce back"; the message names the minimal set of constraints that holds it.
fn drop_plane(cat: &Catalog, s: &mut Session, name: &str, v: f64) {
    let consistent = s
        .eval
        .rig
        .as_ref()
        .is_some_and(|g| g.verdict != Verdict::Conflict);
    if let (true, Some(f), Some(r)) = (consistent, &s.eval.family, &s.eval.resolved) {
        if let Ok(Some(by)) = rig::drag_check(f, r, name, v) {
            let list = if by.is_empty() {
                cat.t("fe.drag.planes")
            } else {
                by.join("; ")
            };
            s.msg = Some((
                true,
                cat.tf("fe.drag.determined", &[("name", name), ("list", &list)]),
            ));
            return;
        }
    }
    let r = s.doc.move_ref(name, v);
    s.apply(cat, r);
}

fn toolbar(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session, acts: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        let tip = cat.t("profile_sketch.doc_undo_tip");
        let r = ui
            .add_enabled_ui(s.doc.can_undo(), |ui| {
                Button::quiet(&cat.t("fe.undo")).show(ui, th)
            })
            .inner;
        if r.on_hover_text(&tip).on_disabled_hover_text(&tip).clicked() {
            s.doc.undo();
            s.refresh();
        }
        let tip = cat.t("profile_sketch.doc_redo_tip");
        let r = ui
            .add_enabled_ui(s.doc.can_redo(), |ui| {
                Button::quiet(&cat.t("fe.redo")).show(ui, th)
            })
            .inner;
        if r.on_hover_text(&tip).on_disabled_hover_text(&tip).clicked() {
            s.doc.redo();
            s.refresh();
        }
        widgets::vdivider(ui, th, th.control_h(ControlSize::Medium));
        let text = s.doc.text().to_string();
        let ok = s.eval.error.is_none() && s.ai.as_mut().map_or(true, |m| m.blocked(&text).is_none());
        let r = ui
            .add_enabled_ui(ok, |ui| {
                Button::primary(&cat.t("fe.load")).show(ui, th)
            })
            .inner;
        if r.on_hover_text(cat.t("fe.load.tip"))
            .on_disabled_hover_text(cat.t("fe.load.blocked"))
            .clicked()
        {
            acts.push(Action::Load);
        }
        widgets::checkbox(ui, th, &mut s.overwrite_types, &cat.t("fe.overwrite_types"))
            .on_hover_text(cat.t("fe.overwrite_types.tip"));
        if Button::secondary(&cat.t("fe.save_draft"))
            .show(ui, th)
            .clicked()
        {
            acts.push(Action::SaveDraft);
        }
        if let Some(fid) = s.family {
            ui.label(
                RichText::new(cat.tf("fe.linked", &[("id", &fid.to_string())]))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            );
        }
    });
}

fn verdict_badge(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &Session) {
    let Some(rig) = &s.eval.rig else {
        return;
    };
    let (key, col) = match rig.verdict {
        Verdict::Well => ("fe.verdict.well", th.c("success")),
        Verdict::Under => ("fe.verdict.under", th.c("warning")),
        Verdict::Over => ("fe.verdict.over", th.c("warning")),
        Verdict::Conflict => ("fe.verdict.conflict", th.c("error")),
    };
    let text = cat.tf(key, &[("dof", &rig.dof.to_string())]);
    let mut tip = vec![];
    if !rig.free.is_empty() {
        tip.push(cat.tf("fe.verdict.free", &[("list", &rig.free.join(", "))]));
    }
    if !rig.redundant.is_empty() {
        tip.push(cat.tf(
            "fe.verdict.redundant",
            &[("list", &rig.redundant.join("; "))],
        ));
    }
    if !rig.conflicting.is_empty() {
        let l: Vec<String> = rig.conflicting.iter().map(|c| c.0.clone()).collect();
        tip.push(cat.tf("fe.verdict.conflicting", &[("list", &l.join("; "))]));
    }
    let r = ui.label(RichText::new(text).color(col).strong());
    if !tip.is_empty() {
        r.on_hover_text(tip.join("\n"));
    }
}

fn header(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session) {
    let Some(f) = s.eval.family.clone() else {
        return;
    };
    ui.horizontal(|ui| {
        thumbnail(ui, s, &f);
        ui.vertical(|ui| {
            ui.label(RichText::new(&f.name).font(th.font(Role::Title)));
            ui.horizontal(|ui| {
                let cats: Vec<String> = Category::ALL
                    .iter()
                    .map(|k| cat.t(&format!("fe.cat.{}", k.name())))
                    .collect();
                let ci = Category::ALL
                    .iter()
                    .position(|k| *k == f.category)
                    .unwrap_or(0);
                if let Some(i) = chrome::picker(
                    ui,
                    th,
                    "g11.fe.cat",
                    &cat.t("family_editor.cap_category"),
                    ci,
                    &cats,
                ) {
                    let r = s.doc.set_category(Category::ALL[i]);
                    s.apply(cat, r);
                }
                let hosts: Vec<String> = HOSTS
                    .iter()
                    .map(|k| cat.t(&format!("fe.host.{}", k.name())))
                    .collect();
                let hi = HOSTS.iter().position(|k| *k == f.hosting).unwrap_or(0);
                if let Some(i) =
                    chrome::picker(ui, th, "g11.fe.host", &cat.t("family_editor.cap_host"), hi, &hosts)
                {
                    let r = s.doc.set_host(HOSTS[i]);
                    s.apply(cat, r);
                }
                let shown = s
                    .ty
                    .clone()
                    .or_else(|| f.types.first().map(|t| t.name.clone()));
                let (tys, ti): (Vec<String>, usize) = if f.types.is_empty() {
                    (vec![cat.t("fe.defaults")], 0)
                } else {
                    (
                        f.types.iter().map(|t| t.name.clone()).collect(),
                        f.types
                            .iter()
                            .position(|t| Some(&t.name) == shown.as_ref())
                            .unwrap_or(0),
                    )
                };
                if let Some(i) =
                    chrome::picker(ui, th, "g11.fe.ty", &cat.t("family_editor.cap_type"), ti, &tys)
                {
                    if let Some(t) = f.types.get(i) {
                        s.ty = Some(t.name.clone());
                        s.refresh();
                    }
                }
                verdict_badge(ui, th, cat, s);
                profile_sketch::status_label(ui, th, cat, &s.psk);
            });
            if let Some(b) = &s.eval.built {
                ui.label(
                    RichText::new(chrome::stats_text(
                        cat,
                        b.volume,
                        b.parts.len(),
                        crate::units::active_units(),
                        crate::ui_is_ru(),
                    ))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
                );
            }
        });
    });
}

fn thumbnail(ui: &mut egui::Ui, s: &mut Session, f: &Family) {
    let key = s.eval.text.clone();
    if s.thumb.as_ref().is_none_or(|t| t.0 != key) {
        if let Some(b) = &s.eval.built {
            let n = thumb::SIZE;
            let rgba = thumb::render(&b.mesh, n, [176, 160, 140]);
            let img = egui::ColorImage::from_rgba_unmultiplied([n as usize, n as usize], &rgba);
            let tex = ui.ctx().load_texture(
                format!("g11.thumb.{}", f.name),
                img,
                egui::TextureOptions::LINEAR,
            );
            s.thumb = Some((key, tex));
        }
    }
    if let Some((_, t)) = &s.thumb {
        let n = thumb::SIZE as f32 * 0.75;
        ui.image((t.id(), egui::vec2(n, n)));
    }
}

/// Saves the active document as a draft by its own row. A name held by another draft asks
/// first (`st.clash`) and writes nothing until `replace` is chosen.
fn save_draft(ed: &mut Editor, cat: &Catalog, st: &mut State, replace: bool) {
    let Some(s) = st.docs.get_mut(st.active) else {
        return;
    };
    // a draft is not a project edit: it bypasses the journal, and project undo does not touch it
    let (name, def, fam) = (s.name(), s.doc.text().to_string(), s.family);
    match ed.model.save_fam_doc_as(s.draft, &name, &def, fam, replace) {
        Ok(ddc_model::DocSave::Saved(id)) => {
            s.draft = Some(id);
            s.msg = Some((false, cat.tf("fe.saved", &[("name", &name)])));
        }
        Ok(ddc_model::DocSave::NameTaken(_)) => st.clash = Some(s.serial),
        Err(e) => s.msg = Some((true, e.to_string())),
    }
}

/// Name-taken question: true = replace the other draft, false = go back and rename.
fn clash_modal(ctx: &egui::Context, th: &Theme, cat: &Catalog, st: &mut State) -> Option<bool> {
    let sn = st.clash?;
    let Some(d) = st.docs.iter().find(|d| d.serial == sn) else {
        st.clash = None;
        return None;
    };
    let body = cat.tf("fe.clash.body", &[("name", &d.name())]);
    let mut pick = None;
    let m = kit::modal_dialog("fe.clash", ctx, |ui| {
        ui.set_width(ddc_ui::theme::layout::DIALOG_TOOL);
        kit::modal_title(ui, cat.t("fe.clash.title"));
        ui.label(RichText::new(body).color(th.c("text.secondary")));
        kit::dialog_footer(ui, |ui| {
            if kit::primary_action(ui, cat.t("fe.clash.rename"), None) {
                pick = Some(false);
            }
            if kit::confirm_danger_button(ui, cat.t("fe.clash.replace"), true).clicked() {
                pick = Some(true);
            }
        });
    });
    // Esc or a click on the backdrop goes back to editing
    if pick.is_none() && m.should_close() {
        pick = Some(false);
    }
    pick
}

fn act(ed: &mut Editor, cat: &Catalog, st: &mut State, a: Action) {
    match a {
        Action::New => {
            st.docs.push(Session::new(blank(cat), None));
            st.active = st.docs.len() - 1;
        }
        Action::Close(i) => close_tab(st, i),
        Action::OpenFamily(id) => {
            if let Some(k) = st.docs.iter().position(|d| d.family == Some(id)) {
                st.active = k;
                return;
            }
            match ed.model.family_def(id) {
                Ok(def) => {
                    st.docs.push(Session::new(FamDoc::new(def), Some(id)));
                    st.active = st.docs.len() - 1;
                }
                Err(e) => ed.status = Some(e.to_string()),
            }
        }
        Action::OpenDraft(id) => match ed.model.fam_doc(id) {
            Ok(def) => {
                let fam = ed
                    .model
                    .fam_docs()
                    .ok()
                    .and_then(|v| v.into_iter().find(|d| d.0 == id).and_then(|d| d.2));
                let mut s = Session::new(FamDoc::new(def), fam);
                s.draft = Some(id);
                st.docs.push(s);
                st.active = st.docs.len() - 1;
            }
            Err(e) => ed.status = Some(e.to_string()),
        },
        Action::Embed(id) => {
            let Some(s) = st.docs.get_mut(st.active) else {
                return;
            };
            match ed.model.family_def(id) {
                Ok(def) => {
                    let r = s.doc.add_nested(&def);
                    s.apply(cat, r);
                }
                Err(e) => s.msg = Some((true, e.to_string())),
            }
        }
        Action::SaveDraft => save_draft(ed, cat, st, false),
        Action::Load => {
            let Some(sn) = st.docs.get(st.active).map(|s| s.serial) else {
                return;
            };
            let m = &ed.model;
            let one = st.docs.iter().filter(|d| d.serial == sn);
            let then = overwrite::Then::Load(sn);
            let q = overwrite::ask(one, then, &mut |d| m.type_overwrites(d).unwrap_or_default());
            match q {
                Some(q) => st.ow = Some(q),
                None => {
                    load_doc(ed, cat, &mut st.docs[st.active]);
                }
            }
        }
    }
}


/// Loads a document into the project; true only when the model committed it.
fn load_doc(ed: &mut Editor, cat: &Catalog, s: &mut Session) -> bool {
    load_set::load_set(ed, cat, std::slice::from_mut(s), &[0]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_reports_rig_and_errors() {
        let e = Eval::compute(
            "family A\nparam W length type 1m\nref C x = 0\nref L x ~ -1m\nref R x ~ 1m\n\
             eq L C R\ndim W L R\nextrude B rect(L, 0, R, 1m) z(0, 1m)\n",
            None,
        );
        assert!(e.error.is_none(), "{:?}", e.error);
        assert_eq!(e.rig.as_ref().unwrap().verdict, Verdict::Well);
        assert!((e.built.unwrap().volume - 1.0).abs() < 1e-9);
        let e = Eval::compute("family A\nnonsense\n", None);
        assert!(e.error.is_some() && e.family.is_none());
        assert!((parse_num("2 * 150mm").unwrap() - 0.3).abs() < 1e-12);
        assert!(parse_num("Width").is_err());
    }

    fn session() -> Session {
        let doc = FamDoc::blank("A", Category::Generic, Hosting::None);
        let mut s = Session::new(doc, None);
        s.doc.move_ref("CenterLR", 0.25).unwrap();
        s
    }

    fn draw(s: &mut Session) {
        s.psk
            .act(|sk| {
                sk.point(0.0, 0.0);
                sk.point(1.0, 0.0);
                Ok::<_, ()>(sk.analyze())
            })
            .unwrap();
    }

    #[test]
    fn ctrl_z_in_sketch_undoes_sketch_not_doc() {
        let mut s = session();
        draw(&mut s);
        let text = s.doc.text().to_string();
        assert!(s.undo_key(false, true));
        assert!(s.psk.is_empty(), "sketch step undone");
        assert_eq!(s.doc.text(), text, "document untouched");
        assert!(s.doc.can_undo() && !s.doc.can_redo());
        // an empty sketch history does not fall through to the document
        assert!(!s.undo_key(false, true));
        assert_eq!(s.doc.text(), text);
        assert!(s.undo_key(true, true), "Ctrl+Y redoes the sketch");
        assert!(!s.psk.is_empty());
        assert!(!s.doc.can_redo());
    }

    #[test]
    fn ctrl_z_outside_sketch_undoes_doc() {
        let mut s = session();
        draw(&mut s);
        let edited = s.doc.text().to_string();
        let pts = s.psk.sk.pts.clone();
        assert!(s.undo_key(false, false));
        assert_ne!(s.doc.text(), edited, "document step undone");
        assert_eq!(s.psk.sk.pts, pts, "sketch untouched");
        assert!(s.psk.can_undo() && !s.psk.can_redo());
        assert!(s.undo_key(true, false), "Ctrl+Y redoes the document");
        assert_eq!(s.doc.text(), edited);
        assert!(s.psk.can_undo() && !s.psk.can_redo());
    }

    /// Unedited document: nothing to lose.
    fn clean() -> Session {
        Session::new(FamDoc::blank("A", Category::Generic, Hosting::None), None)
    }

    fn dirty() -> Session {
        let mut s = session();
        s.doc.dirty = true;
        s
    }

    fn state(docs: Vec<Session>) -> State {
        State {
            open: true,
            docs,
            ..Default::default()
        }
    }

    fn no_load(_: &mut [Session], _: &[usize]) -> Result<(), usize> {
        panic!("load must not run")
    }

    /// A load that the model takes: the documents become clean.
    fn take(d: &mut [Session], w: &[usize]) {
        for &i in w {
            d[i].doc.dirty = false;
        }
    }

    #[test]
    fn clean_tab_closes_at_once() {
        let mut st = state(vec![clean(), clean()]);
        close_tab(&mut st, 0);
        assert_eq!(st.ask, None);
        assert_eq!(st.docs.len(), 1);
        close_window(&mut st);
        assert_eq!(st.ask, None);
        assert!(!st.open);
    }

    #[test]
    fn close_dirty_tab_asks() {
        let mut st = state(vec![clean(), dirty()]);
        let sn = st.docs[1].serial;
        close_tab(&mut st, 1);
        assert_eq!(st.ask, Some(Ask::Tab(sn)));
        assert_eq!(st.docs.len(), 2, "nothing dropped before the answer");
        // Keep editing (also Esc) keeps the document and shows it
        st.active = 0;
        answer(&mut st, Choice::Keep, &mut no_load);
        assert_eq!(st.ask, None);
        assert_eq!(st.docs.len(), 2);
        assert_eq!(st.active, 1);
        assert!(st.docs[1].doc.dirty);
        // a sketch alone asks too, and keep shows the sketch tab
        let mut st = state(vec![clean(), clean()]);
        draw(&mut st.docs[1]);
        close_tab(&mut st, 1);
        assert!(st.ask.is_some());
        answer(&mut st, Choice::Keep, &mut no_load);
        assert_eq!(st.docs[1].tab, Tab::Solids);
        assert!(!st.docs[1].psk.is_empty());
    }

    #[test]
    fn discard_drops_only_that_doc() {
        let mut st = state(vec![dirty(), dirty(), clean()]);
        let (a, c) = (st.docs[0].serial, st.docs[2].serial);
        close_tab(&mut st, 1);
        answer(&mut st, Choice::Discard, &mut no_load);
        let ids: Vec<u64> = st.docs.iter().map(|d| d.serial).collect();
        assert_eq!(ids, vec![a, c]);
        assert!(st.docs[0].doc.dirty, "the other edits survive");
        assert!(st.open);
    }

    #[test]
    fn load_then_close_commits_once() {
        let mut st = state(vec![dirty(), clean()]);
        let sn = st.docs[0].serial;
        close_tab(&mut st, 0);
        let mut calls = vec![];
        answer(&mut st, Choice::Load, &mut |d, w| {
            calls.extend(w.iter().map(|&i| d[i].serial));
            Ok(())
        });
        assert_eq!(calls, vec![sn], "one load, for that document");
        assert_eq!(st.docs.len(), 1);
        // a refused load keeps the document open and in view
        let mut st = state(vec![clean(), dirty()]);
        close_tab(&mut st, 1);
        answer(&mut st, Choice::Load, &mut |_, w| Err(w[0]));
        assert_eq!(st.docs.len(), 2);
        assert_eq!(st.active, 1);
        assert_eq!(st.ask, None);
    }

    #[test]
    fn ask_follows_doc_when_tabs_shift() {
        let mut st = state(vec![clean(), dirty(), clean()]);
        let sn = st.docs[1].serial;
        close_tab(&mut st, 1);
        // the tab before it goes away while the question is open
        st.docs.remove(0);
        assert_eq!(pos(&st, sn), Some(0));
        let mut loaded = vec![];
        answer(&mut st, Choice::Load, &mut |d, w| {
            loaded.extend(w.iter().map(|&i| d[i].serial));
            Ok(())
        });
        assert_eq!(loaded, vec![sn], "the asked family, not whatever took its index");
        assert_eq!(st.docs.len(), 1);
        assert_ne!(st.docs[0].serial, sn);
        // the asked family vanishes: the answer touches nothing
        let mut st = state(vec![dirty(), dirty()]);
        close_tab(&mut st, 1);
        st.docs.remove(1);
        answer(&mut st, Choice::Discard, &mut no_load);
        assert_eq!(st.docs.len(), 1);
    }

    #[test]
    fn window_close_names_dirty_families() {
        let named = |n: &str| {
            let mut s = Session::new(FamDoc::blank(n, Category::Generic, Hosting::None), None);
            s.doc.dirty = true;
            s
        };
        let (a, b) = (named("Alpha"), named("Beta"));
        let mut st = state(vec![a, clean(), b]);
        close_window(&mut st);
        assert_eq!(st.ask, Some(Ask::Window));
        assert!(st.open);
        let names = dirty_names(&st);
        assert_eq!(names, vec!["Alpha", "Beta"], "both dirty families, not the clean one");
        // Keep leaves the window open on the first one
        st.active = 1;
        answer(&mut st, Choice::Keep, &mut no_load);
        assert!(st.open);
        assert_eq!(st.active, 0);
        // Load commits every dirty family in one call (one undo step), then closes
        close_window(&mut st);
        let mut calls = vec![];
        answer(&mut st, Choice::Load, &mut |d, w| {
            calls.push(w.to_vec());
            take(d, w);
            Ok(())
        });
        assert_eq!(calls, vec![vec![0, 2]]);
        assert!(!st.open);
        assert_eq!(st.docs.len(), 3);
        // a refusal of the second loads none, keeps the window open on it, edits and sketches
        let mut st = state(vec![dirty(), clean(), dirty()]);
        draw(&mut st.docs[1]);
        let texts: Vec<String> = st.docs.iter().map(|d| d.doc.text().to_string()).collect();
        close_window(&mut st);
        answer(&mut st, Choice::Load, &mut |_, w| Err(w[1]));
        assert!(st.open && st.ask.is_none());
        assert_eq!(st.active, 2, "the refused family is shown");
        assert_eq!(st.docs.len(), 3);
        assert!(st.docs[0].doc.dirty && st.docs[2].doc.dirty, "every edit kept");
        assert!(!st.docs[1].psk.is_empty(), "the sketch kept");
        let now: Vec<String> = st.docs.iter().map(|d| d.doc.text().to_string()).collect();
        assert_eq!(now, texts);
        // Discard drops the dirty ones and closes
        let mut st = state(vec![dirty(), clean()]);
        close_window(&mut st);
        answer(&mut st, Choice::Discard, &mut no_load);
        assert!(!st.open);
        assert_eq!(st.docs.len(), 1);
    }

    #[test]
    fn undo_presses_are_taken_from_input() {
        let key = |key, shift, repeat| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers {
                command: true,
                ctrl: true,
                shift,
                ..Default::default()
            },
        };
        let mut i = egui::InputState::default();
        i.events.extend([
            key(egui::Key::Z, false, false),
            key(egui::Key::Z, true, false),
            key(egui::Key::Y, false, false),
            key(egui::Key::Z, false, true),
            key(egui::Key::S, false, false),
        ]);
        assert_eq!(take_undo_keys(&mut i), vec![false, true, true]);
        assert_eq!(i.events.len(), 1, "other keys stay for the shell");
    }

    #[test]
    fn project_drafts_name_edited_and_sketched_families() {
        let mut st = state(vec![clean(), dirty(), clean()]);
        draw(&mut st.docs[2]);
        assert_eq!(draft_list(&st).len(), 2);
        assert!(draft_list(&state(vec![clean()])).is_empty(), "a clean tab is no draft");
        drop_drafts(&mut st);
        assert!(draft_list(&st).is_empty());
        assert_eq!(st.docs.len(), 2, "clean tabs stay open");
        assert!(st.docs.iter().all(|d| !d.doc.dirty && d.psk.is_empty()));
    }

    #[test]
    fn project_save_loads_edited_families_at_once_or_none() {
        let mut st = state(vec![dirty(), clean(), dirty()]);
        draw(&mut st.docs[1]);
        let mut calls = vec![];
        assert!(load_all(&mut st, &mut |d, w| {
            calls.push(w.to_vec());
            take(d, w);
            Ok(())
        }));
        assert_eq!(calls, vec![vec![0, 2]], "one load for both edited families");
        assert_eq!(draft_list(&st).len(), 1, "the sketch waits for the project save");
        assert!(!st.docs[1].psk.is_empty());
        let mut st = state(vec![clean(), dirty()]);
        draw(&mut st.docs[0]);
        st.open = false;
        assert!(!load_all(&mut st, &mut |_, w| Err(w[0])));
        assert_eq!((st.active, st.open), (1, true), "the refused family is shown");
        assert!(st.docs[1].doc.dirty, "a refused draft is kept");
        assert!(!st.docs[0].psk.is_empty(), "a refusal keeps the sketches");
    }
}

/// Puts one edited family tab into this thread's editor (tests of the project question).
#[cfg(test)]
pub(crate) fn seed_draft_for_test() {
    let mut doc = FamDoc::blank("Seeded", Category::Generic, Hosting::None);
    doc.dirty = true;
    with(|s| {
        s.docs.push(Session::new(doc, None));
        s.open = true;
    });
}

#[cfg(test)]
const OW_BOX: &str = "family Crate\ncategory generic\nparam W length type 1m\n\
    ref L x ~ -400mm\nref R x ~ 400mm\nref C x = 0\neq L C R\ndim W L R\n\
    extrude B rect(L, 0, R, 1m) z(0, 1m)\ntype \"Std\" W=1m\n";

/// Loads family "Crate" (type "Std", W = 1 m) into the project, then opens an edited tab of
/// it with W = 2 m and "replace type values" ticked. Returns the family id.
#[cfg(test)]
pub(crate) fn seed_overwrite_for_test(ed: &mut Editor, cat: &Catalog) -> i64 {
    let mut s = Session::new(FamDoc::new(OW_BOX.to_string()), None);
    s.doc.dirty = true;
    assert!(load_doc(ed, cat, &mut s), "the project family loads");
    let mut e = Session::new(FamDoc::new(OW_BOX.replace("W=1m", "W=2m")), None);
    e.doc.dirty = true;
    e.overwrite_types = true;
    with(|st| {
        st.docs.push(e);
        st.open = true;
    });
    s.family.expect("loaded")
}

/// Adds an edited tab the model refuses to load.
#[cfg(test)]
pub(crate) fn seed_broken_for_test() {
    let mut s = Session::new(FamDoc::new("family Broken\nnonsense\n".to_string()), None);
    s.doc.dirty = true;
    with(|st| st.docs.push(s));
}

/// W of the first type of `fam` as stored in the project.
#[cfg(test)]
pub(crate) fn type_w_for_test(m: &ddc_model::Model, fam: i64) -> f64 {
    use ddc_geom::component::Value;
    match m.family_types(fam).unwrap()[0].values.as_slice() {
        [(k, Value::Num(v))] if k == "W" => *v,
        other => panic!("{other:?}"),
    }
}

/// Answers the overwrite question as its buttons do: true = Replace values, false = Back.
#[cfg(test)]
pub(crate) fn answer_overwrite_for_test(ed: &mut Editor, cat: &Catalog, yes: bool) {
    let mut st = with(std::mem::take);
    let q = st.ow.take().expect("the overwrite question is up");
    confirm_overwrite(&mut st, q, yes, &mut |d, w| load_set::load_set(ed, cat, d, w));
    with(|s| *s = st);
}

/// The text the overwrite question shows now, in `cat`'s language.
#[cfg(test)]
pub(crate) fn overwrite_text_for_test(cat: &Catalog) -> Option<String> {
    with(|st| st.ow.as_ref().map(|q| overwrite::body(cat, q)))
}
