//! Issues: a list with a status filter, a detail pane (status, priority, comments, go to the
//! saved view), the "new issue" dialog with a screenshot, and BCF import and export.
//! Issue tables sit outside the undo journal, so nothing here is an undo step.

use crate::ai::ui::kit::{self, space, Button, Tone};
use crate::shell::report_win::{encode_png, fit};
use crate::shell::EditorApp;
use crate::ui_kit::{cancel_button, DialogSize};
use ddc_geom::v2;
use ddc_io::bcf::{self, BcfVersion};
use ddc_model::{
    clip_box, ClipPlane, Orbit, Topic, TopicEdit, TopicElem, TopicRow, Viewpoint,
    TOPIC_PRIORITIES, TOPIC_STATUSES,
};
use ddc_render::glam::Vec3;
use ddc_render::Aabb;
use ddc_ui::notify::Level;
use ddc_ui::theme::Role;
use ddc_ui::widgets::{self, TextField};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};
use std::cell::RefCell;

pub const LIST_COMMAND: &str = "data.issues";
pub const NEW_COMMAND: &str = "data.issue_new";

const SHOT_MAX_PX: usize = 1280;
const SHOT_WAIT_FRAMES: u32 = 30;
/// Frames between list reloads while the window is open (own edits reload at once).
const RELOAD_EVERY: u32 = 30;
const PLAN_FOV: f64 = std::f64::consts::FRAC_PI_4;
/// Pitch past which an orthographic camera counts as a plan view.
const PLAN_PITCH: f64 = 1.5;
const UNMATCHED_SHOWN: usize = 12;

/// What a new issue starts from; the person completes it in the dialog.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub title: String,
    pub description: String,
    pub elements: Vec<TopicElem>,
    /// `check:<rule>`, `clash` or `selection`.
    pub source: String,
    /// Identity inside the source: one cause makes one issue.
    pub source_key: String,
    pub priority: Option<&'static str>,
}

struct IssueShot;

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// Screenshot requested; frames waited so far.
    Capture(u32),
    Form,
}

struct Edit {
    topic: Topic,
    title: String,
    assignee: String,
    description: String,
    comment: String,
}

impl Edit {
    fn load(app: &EditorApp, id: i64) -> Option<Edit> {
        let t = app.ed.model.topic_get(id).ok()?;
        Some(Edit {
            title: t.title.clone(),
            assignee: t.assignee.clone(),
            description: t.description.clone(),
            comment: String::new(),
            topic: t,
        })
    }

    fn changed(&self) -> bool {
        self.title != self.topic.title
            || self.assignee != self.topic.assignee
            || self.description != self.topic.description
    }
}

#[derive(Default)]
struct State {
    open: bool,
    /// 0 is "all", then the statuses in order.
    filter: usize,
    rows: Vec<TopicRow>,
    dirty: bool,
    age: u32,
    project: Option<String>,
    sel: Option<i64>,
    edit: Option<Edit>,
    screen_h: f32,
    /// (is an error, text) of the last import or export.
    info: Option<(bool, String)>,
    report: Vec<String>,
    stage: Stage,
    draft: Draft,
    form_title: String,
    form_assignee: String,
    form_priority: usize,
    form_snap: bool,
    tried: bool,
    shot: Option<Vec<u8>>,
    view: Option<Viewpoint>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

enum Act {
    Pick(i64),
    New,
    Import,
    Export(BcfVersion),
    Status(&'static str),
    Priority(&'static str),
    Save,
    Comment,
    Goto,
    SetView,
    Delete,
}

pub fn register(app: &mut EditorApp) {
    app.register_command(LIST_COMMAND, |_| {
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.open = true;
            s.dirty = true;
        })
    });
    app.register_command(NEW_COMMAND, |a| {
        let d = selection_draft(a);
        start_draft(a, d);
    });
}

fn mark_dirty() {
    ST.with(|s| s.borrow_mut().dirty = true);
}

fn author() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

/// The selected elements that exist in the model, as issue links.
pub fn selection_draft(app: &EditorApp) -> Draft {
    let mut elements: Vec<TopicElem> = vec![];
    for s in &app.ed.selection {
        let Some((table, id)) = crate::ai::preview::key_of(*s) else {
            continue;
        };
        if app.ed.model.topic_elem_alive(&table, id)
            && !elements.iter().any(|e| e.table == table && e.id == id)
        {
            elements.push(TopicElem::local(&table, id));
        }
    }
    Draft {
        elements,
        source: "selection".to_string(),
        ..Draft::default()
    }
}

/// Issue links for element keys (tables or query categories); gone elements are left out.
pub fn elements_of(app: &EditorApp, keys: &[(String, i64)]) -> Vec<TopicElem> {
    let mut out: Vec<TopicElem> = vec![];
    for (cat, id) in keys {
        let mapped = crate::ai::sets::table_of(cat);
        let table = if mapped.is_empty() { cat.as_str() } else { mapped };
        if app.ed.model.topic_elem_alive(table, *id)
            && !out.iter().any(|e| e.table == table && e.id == *id)
        {
            out.push(TopicElem::local(table, *id));
        }
    }
    out
}

/// Opens the new-issue dialog for `d`, or the existing issue of the same source and key.
pub fn start_draft(app: &mut EditorApp, d: Draft) {
    ST.with(|s| begin(app, &mut s.borrow_mut(), d));
}

fn begin(app: &mut EditorApp, st: &mut State, mut d: Draft) {
    let keys: Vec<(String, i64)> = d.elements.iter().map(|e| (e.table.clone(), e.id)).collect();
    d.elements = elements_of(app, &keys);
    if !d.source_key.is_empty() {
        if let Ok(Some(id)) = app.ed.model.topic_by_source(&d.source, &d.source_key) {
            st.open = true;
            st.dirty = true;
            st.sel = Some(id);
            st.edit = None;
            let msg = app.sh.cat.tf("issues.issue.exists", &[("id", &id.to_string())]);
            app.sh.toasts.push(Level::Info, msg, None);
            return;
        }
    }
    if !matches!(st.stage, Stage::Idle) {
        return;
    }
    st.form_title = d.title.clone();
    st.form_assignee.clear();
    st.form_priority = d
        .priority
        .and_then(|p| TOPIC_PRIORITIES.iter().position(|x| *x == p))
        .unwrap_or(1);
    st.form_snap = true;
    st.tried = false;
    st.draft = d;
    st.stage = Stage::Capture(0);
}

/// Saves a drafted issue (the assistant's card) with the camera as it is now.
pub fn save_draft(app: &mut EditorApp, t: &Topic) -> Result<i64, String> {
    let h = ST.with(|s| s.borrow().screen_h);
    let mut t = t.clone();
    if t.viewpoint.is_none() {
        t.viewpoint = camera_now(app, h);
    }
    if t.author.is_empty() {
        t.author = author();
    }
    let id = app.ed.model.topic_add(&t).map_err(|e| e.to_string())?;
    mark_dirty();
    notify_created(app, &t.title);
    Ok(id)
}

fn notify_created(app: &mut EditorApp, title: &str) {
    let c = &app.sh.cat;
    let (text, act) = (c.tf("issues.issue.created", &[("title", title)]), c.t("issues.issue.open"));
    app.sh.toasts.push_cmd(Level::Info, text, act, LIST_COMMAND);
}

// ---- camera ----

fn camera_now(app: &EditorApp, screen_h: f32) -> Option<Viewpoint> {
    let z = app.ed.elevation;
    let o = if app.view3d {
        let c = &app.view.camera;
        Orbit {
            target: [c.target.x as f64, c.target.y as f64, c.target.z as f64 + z],
            yaw: c.yaw as f64,
            pitch: c.pitch as f64,
            distance: c.distance as f64,
            fov_y: c.fov_y as f64,
            ortho: c.ortho,
        }
    } else {
        // the plan as a top-down orthographic camera whose frame height is the visible height
        let visible = (screen_h as f64 / app.plan.scale.max(1e-6)).max(0.1);
        Orbit {
            target: [app.plan.center.x, app.plan.center.y, z],
            yaw: -std::f64::consts::FRAC_PI_2,
            pitch: std::f64::consts::FRAC_PI_2,
            distance: visible * 0.5 / (PLAN_FOV * 0.5).tan(),
            fov_y: PLAN_FOV,
            ortho: true,
        }
    };
    let mut v = Viewpoint::from_orbit(&o);
    let aspect = app.view.nav.aspect as f64;
    if aspect.is_finite() && aspect > 0.0 {
        v.aspect = aspect;
    }
    if app.view3d {
        if let Some(b) = app.view.section_box {
            let w = |p: Vec3| [p.x as f64, p.y as f64, p.z as f64 + z];
            v.clip = ClipPlane::of_box(w(b.min), w(b.max));
        }
    }
    v.is_valid().then_some(v)
}

/// The section box of a view's clip planes in scene coordinates; open sides end at the model
/// bounds. Planes that are not axis-aligned cannot be shown and are counted.
fn section_of(v: &Viewpoint, elevation: f64, bounds: Option<Aabb>) -> (Option<Aabb>, usize) {
    let Some(b) = clip_box(&v.clip) else {
        return (None, 0);
    };
    let pad = 1.0;
    let side = |x: f64, model: Option<f32>, lo: bool| -> Option<f32> {
        if x.is_finite() {
            Some(x as f32)
        } else {
            model.map(|m| if lo { m - pad } else { m + pad })
        }
    };
    let mut lo = [0f32; 3];
    let mut hi = [0f32; 3];
    for i in 0..3 {
        let dz = if i == 2 { elevation } else { 0.0 };
        let (Some(a), Some(c)) = (
            side(b.min[i] - dz, bounds.map(|m| m.min[i]), true),
            side(b.max[i] - dz, bounds.map(|m| m.max[i]), false),
        ) else {
            return (None, b.oblique);
        };
        if a >= c {
            return (None, b.oblique);
        }
        (lo[i], hi[i]) = (a, c);
    }
    let bx = Aabb {
        min: Vec3::from(lo),
        max: Vec3::from(hi),
    };
    (Some(bx), b.oblique)
}

fn restore_camera(app: &mut EditorApp, screen_h: f32, v: &Viewpoint) -> bool {
    let z = app.ed.elevation;
    let hint = app.view.camera.distance as f64;
    let Some(o) = v.to_orbit(hint) else {
        return false;
    };
    if o.ortho && o.pitch > PLAN_PITCH && v.scale > 0.0 {
        app.plan.center = v2(o.target[0], o.target[1]);
        app.plan.scale = (screen_h as f64 / v.scale).clamp(0.5, 4000.0);
        app.view3d = false;
        return true;
    }
    let c = &mut app.view.camera;
    c.target = Vec3::new(o.target[0] as f32, o.target[1] as f32, (o.target[2] - z) as f32);
    c.yaw = o.yaw as f32;
    c.pitch = (o.pitch as f32).clamp(-ddc_render::nav::PITCH_TOP, ddc_render::nav::PITCH_TOP);
    c.distance = (o.distance as f32).max(1e-3);
    c.fov_y = o.fov_y as f32;
    c.ortho = o.ortho;
    app.view.nav.anim = None;
    app.view.nav.walk = None;
    app.view.nav.elevation = Some(z);
    app.view.keep_camera();
    app.view3d = true;
    let (section, oblique) = section_of(v, z, app.scene.bounds);
    app.view.section_box = section;
    if oblique > 0 {
        let text = app.sh.cat.tf("collab_diff.view.oblique", &[("n", &oblique.to_string())]);
        app.sh.toasts.push(Level::Warning, text, None);
    }
    true
}

/// Selects the linked elements that exist here; false when none does.
fn select_linked(app: &mut EditorApp, t: &Topic, frame: bool) -> bool {
    let keys: Vec<(String, i64)> = t
        .elements
        .iter()
        .filter(|e| !e.table.is_empty() && app.ed.model.topic_elem_alive(&e.table, e.id))
        .map(|e| (e.table.clone(), e.id))
        .collect();
    if frame {
        return crate::ai::ui::receipt::show(app, &keys);
    }
    let sel = crate::ai::ui::receipt::selection_of(&keys);
    if sel.is_empty() {
        return false;
    }
    app.ed.selection = sel;
    true
}

// ---- screenshot ----

fn take_shot(ctx: &egui::Context) -> Option<Vec<u8>> {
    let img = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot {
                user_data, image, ..
            } if user_data
                .data
                .as_ref()
                .is_some_and(|d| d.downcast_ref::<IssueShot>().is_some()) =>
            {
                Some(image.clone())
            }
            _ => None,
        })
    })?;
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    let (rgba, w, h) = fit(rgba, w, h, SHOT_MAX_PX);
    encode_png(&rgba, w, h)
}

// ---- frame ----

pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    st.screen_h = ctx.screen_rect().height();
    capture(app, ctx, &mut st);
    if st.open {
        list_window(app, ctx, &mut st);
    }
    if matches!(st.stage, Stage::Form) {
        form_window(app, ctx, &mut st);
    }
    ST.with(|s| *s.borrow_mut() = st);
}

/// The dialog opens only after the capture, so it is not in the picture.
fn capture(app: &EditorApp, ctx: &egui::Context, st: &mut State) {
    let Stage::Capture(n) = st.stage else {
        return;
    };
    if n == 0 {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(IssueShot)));
    }
    let got = take_shot(ctx);
    if got.is_some() || n >= SHOT_WAIT_FRAMES {
        st.form_snap = got.is_some();
        st.shot = got;
        st.view = camera_now(app, st.screen_h);
        st.stage = Stage::Form;
    } else {
        st.stage = Stage::Capture(n + 1);
    }
    ctx.request_repaint();
}

fn label_of(c: &Catalog, group: &str, value: &str) -> String {
    let key = format!("issues.{group}.{value}");
    let t = c.t(&key);
    if t == key {
        value.to_string()
    } else {
        t
    }
}

fn status_tone(s: &str) -> Tone {
    match s {
        "open" => Tone::Warning,
        "in_progress" => Tone::Accent,
        "resolved" => Tone::Success,
        _ => Tone::Neutral,
    }
}

fn priority_tone(p: &str) -> Tone {
    match p {
        "critical" => Tone::Danger,
        "high" => Tone::Warning,
        _ => Tone::Neutral,
    }
}

fn heading(ui: &mut egui::Ui, th: &Theme, text: &str) -> egui::Response {
    ui.label(
        RichText::new(text)
            .font(th.font(Role::BodyStrong))
            .color(th.c("text.primary")),
    )
}

fn multiline(ui: &mut egui::Ui, text: &mut String, rows: usize, salt: &str) {
    ui.add(
        egui::TextEdit::multiline(text)
            .id_salt(salt)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    );
}

// ---- new issue dialog ----

fn form_window(app: &mut EditorApp, ctx: &egui::Context, st: &mut State) {
    let (mut open, mut create, mut cancelled) = (true, false, false);
    {
        let c = &app.sh.cat;
        let th = &app.sh.theme;
        let title_err = st.tried && st.form_title.trim().is_empty();
        let have_shot = st.shot.is_some();
        let n = st.draft.elements.len().to_string();
        let prios: Vec<String> = TOPIC_PRIORITIES
            .iter()
            .map(|p| label_of(c, "priority", p))
            .collect();
        let State {
            form_title,
            form_assignee,
            form_priority,
            form_snap,
            tried,
            draft,
            ..
        } = st;
        kit::Dialog::new("ag4.issue_new", &c.t("issues.new.title"), DialogSize::Form)
            .open(&mut open)
            .show(ctx, |ui| {
                let lt = c.t("issues.field.title");
                let lr = heading(ui, th, &lt);
                TextField::new(form_title)
                    .id_salt("ag4-new-title")
                    .labelled_by(lr.id, &lt)
                    .width(ui.available_width())
                    .show(ui, th);
                if title_err {
                    kit::error_note(ui, th, &c.t("issues.new.title_err"));
                }
                ui.add_space(space::SM);
                heading(ui, th, &c.t("issues.field.description"));
                multiline(ui, &mut draft.description, 4, "ag4-new-desc");
                ui.add_space(space::SM);
                heading(ui, th, &c.t("issues.field.priority"));
                let refs: Vec<&str> = prios.iter().map(String::as_str).collect();
                if let Some(i) = widgets::segmented(ui, th, &refs, *form_priority) {
                    *form_priority = i;
                }
                ui.add_space(space::SM);
                let la = c.t("issues.field.assignee");
                let lr = heading(ui, th, &la);
                TextField::new(form_assignee)
                    .id_salt("ag4-new-assignee")
                    .labelled_by(lr.id, &la)
                    .width(ui.available_width())
                    .show(ui, th);
                ui.add_space(space::SM);
                kit::caption(ui, th, &c.tf("issues.new.linked", &[("n", n.as_str())]));
                ui.add_enabled_ui(have_shot, |ui| {
                    widgets::checkbox(ui, th, form_snap, &c.t("issues.new.snapshot"));
                });
                kit::caption(ui, th, &c.t("issues.new.view_note"));
                crate::ui_kit::dialog_footer(ui, |ui| {
                    if Button::primary(&c.t("issues.new.create")).show(ui, th).clicked() {
                        if form_title.trim().is_empty() {
                            *tried = true;
                        } else {
                            create = true;
                        }
                    }
                    if cancel_button(ui, c.t("issues.new.cancel")).clicked() {
                        cancelled = true;
                    }
                });
            });
        open &= !cancelled;
    }
    if create {
        create_issue(app, st);
        st.stage = Stage::Idle;
        st.shot = None;
    } else if !open {
        st.stage = Stage::Idle;
        st.shot = None;
    }
}

fn create_issue(app: &mut EditorApp, st: &mut State) {
    let mut t = Topic::new(st.form_title.trim());
    t.description = st.draft.description.trim().to_string();
    t.priority = TOPIC_PRIORITIES[st.form_priority.min(TOPIC_PRIORITIES.len() - 1)].to_string();
    t.assignee = st.form_assignee.trim().to_string();
    t.source = st.draft.source.clone();
    t.source_key = st.draft.source_key.clone();
    t.author = author();
    t.elements = st.draft.elements.clone();
    t.viewpoint = st.view.clone();
    if st.form_snap {
        t.snapshot = st.shot.clone();
    }
    match app.ed.model.topic_add(&t) {
        Ok(id) => {
            st.dirty = true;
            st.sel = Some(id);
            st.edit = None;
            notify_created(app, &t.title);
        }
        Err(e) => app.sh.toasts.push(Level::Warning, e.to_string(), None),
    }
}

// ---- issues window ----

fn load(app: &EditorApp, st: &mut State) {
    st.age = st.age.wrapping_add(1);
    if st.dirty || st.age % RELOAD_EVERY == 0 {
        let project = app.ed.model.db_path();
        if st.project != project {
            st.project = project;
            st.sel = None;
            st.edit = None;
        }
        st.rows = app.ed.model.topic_list(None).unwrap_or_default();
        st.dirty = false;
        if st.sel.is_some_and(|id| !st.rows.iter().any(|r| r.id == id)) {
            st.sel = None;
        }
    }
    let have = st.edit.as_ref().map(|e| e.topic.id);
    if st.sel != have {
        st.edit = st.sel.and_then(|id| Edit::load(app, id));
        if st.edit.is_none() {
            st.sel = None;
        }
    }
}

fn list_window(app: &mut EditorApp, ctx: &egui::Context, st: &mut State) {
    load(app, st);
    let mut acts: Vec<Act> = vec![];
    let mut open = true;
    {
        let c = &app.sh.cat;
        let th = &app.sh.theme;
        kit::Dialog::new("issues.issues", &c.t("issues.issues.title"), DialogSize::Wide)
            .open(&mut open)
            .show(ctx, |ui| body(ui, c, th, st, &mut acts));
    }
    if !open {
        st.open = false;
    }
    for a in acts {
        run(app, st, a);
    }
}

fn body(ui: &mut egui::Ui, c: &Catalog, th: &Theme, st: &mut State, acts: &mut Vec<Act>) {
    let total = st.rows.len();
    ui.horizontal_wrapped(|ui| {
        if Button::primary(&c.t("issues.issues.new")).small().show(ui, th).clicked() {
            acts.push(Act::New);
        }
        if Button::secondary(&c.t("issues.issues.import")).small().show(ui, th).clicked() {
            acts.push(Act::Import);
        }
        for (v, key) in [
            (BcfVersion::V21, "issues.issues.export21"),
            (BcfVersion::V30, "issues.issues.export30"),
        ] {
            if Button::secondary(&c.t(key))
                .small()
                .enabled(total > 0)
                .show(ui, th)
                .clicked()
            {
                acts.push(Act::Export(v));
            }
        }
    });
    if let Some((err, msg)) = &st.info {
        if *err {
            kit::error_note(ui, th, msg);
        } else {
            kit::caption(ui, th, msg);
        }
    }
    for line in &st.report {
        kit::caption(ui, th, line);
    }
    ui.add_space(space::SM);
    if total == 0 {
        let new_label = c.t("issues.issues.new");
        if kit::empty_state(
            ui,
            th,
            &c.t("issues.issues.empty.title"),
            &c.t("issues.issues.empty.hint"),
            Some(new_label.as_str()),
        ) {
            acts.push(Act::New);
        }
        return;
    }
    let mut labels = vec![c.t("issues.filter.all")];
    labels.extend(TOPIC_STATUSES.iter().map(|s| label_of(c, "status", s)));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    if let Some(i) = widgets::segmented(ui, th, &refs, st.filter) {
        st.filter = i;
    }
    let want = st.filter.checked_sub(1).map(|i| TOPIC_STATUSES[i.min(3)]);
    let shown = st
        .rows
        .iter()
        .filter(|r| want.map_or(true, |w| r.status == w))
        .count();
    kit::caption(
        ui,
        th,
        &c.tf(
            "issues.issues.count",
            &[("n", &shown.to_string()), ("m", &total.to_string())],
        ),
    );
    ui.add_space(space::XS);
    ui.columns(2, |cols| {
        list(&mut cols[0], c, th, st, want, acts);
        detail(&mut cols[1], c, th, st, acts);
    });
}

fn list(
    ui: &mut egui::Ui,
    c: &Catalog,
    th: &Theme,
    st: &State,
    want: Option<&str>,
    acts: &mut Vec<Act>,
) {
    egui::ScrollArea::vertical().id_salt("ag4-list").show(ui, |ui| {
        let mut any = false;
        for r in st.rows.iter().filter(|r| want.map_or(true, |w| r.status == w)) {
            any = true;
            let on = st.sel == Some(r.id);
            kit::card(ui, th, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::badge(
                        ui,
                        th,
                        &label_of(c, "status", &r.status),
                        status_tone(&r.status),
                    );
                    if r.priority != "normal" {
                        widgets::badge(
                            ui,
                            th,
                            &label_of(c, "priority", &r.priority),
                            priority_tone(&r.priority),
                        );
                    }
                });
                let role = if on { Role::BodyStrong } else { Role::Body };
                let color = if on { th.c("accent") } else { th.c("text.primary") };
                let title = ui.add(
                    egui::Label::new(RichText::new(&r.title).font(th.font(role)).color(color))
                        .wrap()
                        .sense(egui::Sense::click()),
                );
                let mut meta = c.tf("issues.issues.meta", &[("n", &r.elements.to_string())]);
                if !r.assignee.is_empty() {
                    meta = format!("{} \u{b7} {meta}", r.assignee);
                }
                kit::caption(ui, th, &meta);
                if title.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    acts.push(Act::Pick(r.id));
                }
            });
            ui.add_space(space::XS);
        }
        if !any {
            kit::caption(ui, th, &c.t("issues.issues.none_here"));
        }
    });
}

fn detail(ui: &mut egui::Ui, c: &Catalog, th: &Theme, st: &mut State, acts: &mut Vec<Act>) {
    let Some(e) = st.edit.as_mut() else {
        kit::caption(ui, th, &c.t("issues.detail.pick"));
        return;
    };
    egui::ScrollArea::vertical().id_salt("ag4-detail").show(ui, |ui| {
        let lt = c.t("issues.field.title");
        let lr = heading(ui, th, &lt);
        TextField::new(&mut e.title)
            .id_salt("ag4-d-title")
            .labelled_by(lr.id, &lt)
            .width(ui.available_width())
            .show(ui, th);
        ui.add_space(space::SM);
        heading(ui, th, &c.t("issues.field.status"));
        let labels: Vec<String> = TOPIC_STATUSES
            .iter()
            .map(|s| label_of(c, "status", s))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let cur = TOPIC_STATUSES
            .iter()
            .position(|s| *s == e.topic.status)
            .unwrap_or(usize::MAX);
        if let Some(i) = widgets::segmented(ui, th, &refs, cur) {
            acts.push(Act::Status(TOPIC_STATUSES[i]));
        }
        ui.add_space(space::SM);
        heading(ui, th, &c.t("issues.field.priority"));
        let labels: Vec<String> = TOPIC_PRIORITIES
            .iter()
            .map(|s| label_of(c, "priority", s))
            .collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let cur = TOPIC_PRIORITIES
            .iter()
            .position(|s| *s == e.topic.priority)
            .unwrap_or(usize::MAX);
        if let Some(i) = widgets::segmented(ui, th, &refs, cur) {
            acts.push(Act::Priority(TOPIC_PRIORITIES[i]));
        }
        ui.add_space(space::SM);
        let la = c.t("issues.field.assignee");
        let lr = heading(ui, th, &la);
        TextField::new(&mut e.assignee)
            .id_salt("ag4-d-assignee")
            .labelled_by(lr.id, &la)
            .width(ui.available_width())
            .show(ui, th);
        ui.add_space(space::SM);
        heading(ui, th, &c.t("issues.field.description"));
        multiline(ui, &mut e.description, 4, "ag4-d-desc");
        ui.add_space(space::XS);
        if Button::secondary(&c.t("issues.detail.save"))
            .small()
            .enabled(e.changed() && !e.title.trim().is_empty())
            .show(ui, th)
            .clicked()
        {
            acts.push(Act::Save);
        }
        ui.add_space(space::SM);
        let n = e.topic.elements.len();
        let gone = e.topic.elements.iter().filter(|x| x.table.is_empty()).count();
        let mut facts = c.tf("issues.detail.elements", &[("n", &n.to_string())]);
        if gone > 0 {
            facts.push_str(&c.tf("issues.detail.elements_gone", &[("n", &gone.to_string())]));
        }
        kit::caption(ui, th, &facts);
        let mut line = vec![];
        if !e.topic.source.is_empty() {
            line.push(e.topic.source.clone());
        }
        line.push(c.t(if e.topic.viewpoint.is_some() {
            "issues.detail.has_view"
        } else {
            "issues.detail.no_view"
        }));
        if e.topic.viewpoint.as_ref().is_some_and(|v| !v.clip.is_empty()) {
            line.push(c.t("collab_diff.view.has_section"));
        }
        if e.topic.snapshot.is_some() {
            line.push(c.t("issues.detail.has_snapshot"));
        }
        kit::caption(ui, th, &line.join(" \u{b7} "));
        ui.horizontal_wrapped(|ui| {
            if Button::primary(&c.t("issues.detail.goto"))
                .small()
                .enabled(n > 0 || e.topic.viewpoint.is_some())
                .show(ui, th)
                .clicked()
            {
                acts.push(Act::Goto);
            }
            if Button::secondary(&c.t("issues.detail.set_view"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Act::SetView);
            }
            if Button::danger(&c.t("issues.detail.delete"))
                .small()
                .show(ui, th)
                .clicked()
            {
                acts.push(Act::Delete);
            }
        });
        ui.add_space(space::MD);
        heading(ui, th, &c.t("issues.field.comments"));
        if e.topic.comments.is_empty() {
            kit::caption(ui, th, &c.t("issues.detail.no_comments"));
        }
        for cm in &e.topic.comments {
            let when = bcf::iso(cm.ts).replace('T', " ");
            let when = when.get(..16).unwrap_or(&when);
            kit::caption(ui, th, &format!("{} \u{b7} {when}", cm.author));
            ui.add(
                egui::Label::new(
                    RichText::new(&cm.text)
                        .font(th.font(Role::Label))
                        .color(th.c("text.primary")),
                )
                .wrap(),
            );
            ui.add_space(space::XS);
        }
        let lc = c.t("issues.detail.add_comment");
        let lr = heading(ui, th, &lc);
        TextField::new(&mut e.comment)
            .id_salt("ag4-d-comment")
            .labelled_by(lr.id, &lc)
            .width(ui.available_width())
            .show(ui, th);
        if Button::secondary(&c.t("issues.detail.post"))
            .small()
            .enabled(!e.comment.trim().is_empty())
            .show(ui, th)
            .clicked()
        {
            acts.push(Act::Comment);
        }
    });
}

fn warn(app: &mut EditorApp, text: String) {
    app.sh.toasts.push(Level::Warning, text, None);
}

fn run(app: &mut EditorApp, st: &mut State, a: Act) {
    let id = st.edit.as_ref().map(|e| e.topic.id);
    match a {
        Act::Pick(i) => {
            st.sel = Some(i);
            st.edit = None;
        }
        Act::New => {
            let d = selection_draft(app);
            begin(app, st, d);
        }
        Act::Import => import(app, st),
        Act::Export(v) => export(app, st, v),
        Act::Status(s) => {
            let e = TopicEdit {
                status: Some(s.to_string()),
                ..TopicEdit::default()
            };
            apply_edit(app, st, id, &e);
        }
        Act::Priority(p) => {
            let e = TopicEdit {
                priority: Some(p.to_string()),
                ..TopicEdit::default()
            };
            apply_edit(app, st, id, &e);
        }
        Act::Save => {
            let Some(ed) = st.edit.as_ref() else { return };
            let e = TopicEdit {
                title: Some(ed.title.clone()),
                assignee: Some(ed.assignee.clone()),
                description: Some(ed.description.clone()),
                ..TopicEdit::default()
            };
            apply_edit(app, st, id, &e);
        }
        Act::Comment => {
            let (Some(id), Some(ed)) = (id, st.edit.as_mut()) else {
                return;
            };
            match app.ed.model.topic_comment_add(id, &author(), &ed.comment) {
                Ok(_) => {
                    ed.comment.clear();
                    refresh(app, st, id);
                }
                Err(e) => warn(app, e.to_string()),
            }
        }
        Act::Goto => {
            let Some(t) = st.edit.as_ref().map(|e| e.topic.clone()) else {
                return;
            };
            let had_view = match &t.viewpoint {
                Some(v) => restore_camera(app, st.screen_h, v),
                None => false,
            };
            let found = select_linked(app, &t, !had_view);
            if !found && !t.elements.is_empty() {
                let msg = app.sh.cat.t("issues.issue.elems_gone");
                app.sh.toasts.push(Level::Info, msg, None);
            }
        }
        Act::SetView => {
            let (Some(id), Some(ed)) = (id, st.edit.as_ref()) else {
                return;
            };
            let v = camera_now(app, st.screen_h);
            let r = app
                .ed
                .model
                .topic_set_view(id, v.as_ref(), ed.topic.snapshot.as_deref());
            match r {
                Ok(()) => refresh(app, st, id),
                Err(e) => warn(app, e.to_string()),
            }
        }
        Act::Delete => {
            let (Some(id), Some(ed)) = (id, st.edit.as_ref()) else {
                return;
            };
            let c = &app.sh.cat;
            let sure = crate::file_pick::confirm_yes_no(
                c.t("issues.detail.delete"),
                c.tf("issues.detail.delete_ask", &[("title", &ed.topic.title)]),
            );
            if sure {
                match app.ed.model.topic_delete(id) {
                    Ok(()) => {
                        st.sel = None;
                        st.edit = None;
                        st.dirty = true;
                    }
                    Err(e) => warn(app, e.to_string()),
                }
            }
        }
    }
}

fn apply_edit(app: &mut EditorApp, st: &mut State, id: Option<i64>, e: &TopicEdit) {
    let Some(id) = id else { return };
    match app.ed.model.topic_edit(id, e) {
        Ok(()) => refresh(app, st, id),
        Err(err) => warn(app, err.to_string()),
    }
}

/// Re-reads the stored issue but keeps what the person is typing.
fn refresh(app: &EditorApp, st: &mut State, id: i64) {
    st.dirty = true;
    if let (Some(e), Ok(t)) = (st.edit.as_mut(), app.ed.model.topic_get(id)) {
        if e.title == e.topic.title {
            e.title = t.title.clone();
        }
        if e.assignee == e.topic.assignee {
            e.assignee = t.assignee.clone();
        }
        if e.description == e.topic.description {
            e.description = t.description.clone();
        }
        e.topic = t;
    }
}

// ---- BCF ----

fn stem(app: &EditorApp) -> String {
    let name = app.ed.model.project_info().map(|p| p.name).unwrap_or_default();
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').trim();
    if s.is_empty() {
        "issues".to_string()
    } else {
        s.to_string()
    }
}

fn import(app: &mut EditorApp, st: &mut State) {
    let Some(path) = crate::file_pick::Dlg::new()
        .set_title(app.sh.cat.t("issues.bcf.import_title"))
        .add_filter("BCF", &["bcfzip", "bcf", "zip"])
        .pick_file()
    else {
        return;
    };
    st.report.clear();
    let c = &app.sh.cat;
    match bcf::import_file(&app.ed.model, &path) {
        Ok(r) => {
            let n = r.created + r.updated;
            st.info = Some((
                false,
                c.tf(
                    "issues.bcf.imported",
                    &[
                        ("n", &n.to_string()),
                        ("created", &r.created.to_string()),
                        ("updated", &r.updated.to_string()),
                        ("matched", &r.elements_matched.to_string()),
                        ("unmatched", &r.unmatched.len().to_string()),
                    ],
                ),
            ));
            for u in r.unmatched.iter().take(UNMATCHED_SHOWN) {
                st.report.push(c.tf(
                    "issues.bcf.unmatched_row",
                    &[("issue", &u.issue), ("ref", &u.reference)],
                ));
            }
            if r.unmatched.len() > UNMATCHED_SHOWN {
                let rest = (r.unmatched.len() - UNMATCHED_SHOWN).to_string();
                st.report.push(c.tf("issues.bcf.unmatched_more", &[("n", &rest)]));
            }
            for w in r.warnings.iter().take(UNMATCHED_SHOWN) {
                st.report.push(w.clone());
            }
            st.dirty = true;
        }
        Err(e) => {
            st.info = Some((
                true,
                c.tf("issues.bcf.import_failed", &[("error", &e.to_string())]),
            ));
        }
    }
}

fn export(app: &mut EditorApp, st: &mut State, v: BcfVersion) {
    let Some(path) = crate::file_pick::Dlg::new()
        .set_title(app.sh.cat.t("issues.bcf.export_title"))
        .add_filter("BCF", &["bcfzip"])
        .set_file_name(format!("{}.bcfzip", stem(app)))
        .save_file()
    else {
        return;
    };
    st.report.clear();
    let c = &app.sh.cat;
    st.info = Some(match bcf::export_file(&app.ed.model, None, v, &path) {
        Ok(n) => (
            false,
            c.tf(
                "issues.bcf.exported",
                &[("n", &n.to_string()), ("path", &path.display().to_string())],
            ),
        ),
        Err(e) => (
            true,
            c.tf("issues.bcf.export_failed", &[("error", &e.to_string())]),
        ),
    });
}

/// One frame of the list window (`form` false) or the new-issue dialog for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context, form: bool) {
    use ddc_model::TopicComment;
    let seeded = ST.with(|s| s.borrow().open || !matches!(s.borrow().stage, Stage::Idle));
    if !seeded {
        let mut t = Topic::new(
            "A very long issue title that has to wrap inside the narrow list column unclipped",
        );
        t.description = "Description with several words ".repeat(8);
        t.assignee = "Anna Romanovskaya".to_string();
        t.comments = vec![TopicComment {
            guid: "c1".to_string(),
            ts: 1_700_000_000,
            author: "Anna".to_string(),
            text: "A long comment that wraps over more than one line of the detail pane. "
                .repeat(3),
        }];
        let id = app.ed.model.topic_add(&t).unwrap();
        let mut done = Topic::new("Closed one");
        done.status = "closed".to_string();
        done.priority = "critical".to_string();
        app.ed.model.topic_add(&done).unwrap();
        ST.with(|s| {
            let mut s = s.borrow_mut();
            s.dirty = true;
            if form {
                s.stage = Stage::Form;
                s.form_title = String::new();
                s.tried = true;
                s.shot = Some(vec![0]);
            } else {
                s.open = true;
                s.sel = Some(id);
            }
        });
    }
    frame(app, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(lo: [f32; 3], hi: [f32; 3]) -> Aabb {
        Aabb {
            min: Vec3::from(lo),
            max: Vec3::from(hi),
        }
    }

    #[test]
    fn a_section_box_goes_through_clip_planes_and_back() {
        let b = bx([-1.0, 2.0, 0.5], [4.0, 6.0, 3.25]);
        let z = 12.0;
        let w = |p: Vec3| [p.x as f64, p.y as f64, p.z as f64 + z];
        let v = Viewpoint {
            clip: ClipPlane::of_box(w(b.min), w(b.max)),
            ..Viewpoint::default()
        };
        assert_eq!(v.clip.len(), 6);
        assert_eq!(section_of(&v, z, None), (Some(b), 0));
    }

    #[test]
    fn open_sides_end_at_the_model_and_oblique_planes_are_counted() {
        let mut clip = ClipPlane::of_box([0.0, 0.0, f64::NEG_INFINITY], [5.0, 5.0, 2.0]);
        assert_eq!(clip.len(), 5);
        clip.push(ClipPlane {
            at: [1.0, 1.0, 1.0],
            dir: [0.6, 0.8, 0.0],
        });
        let v = Viewpoint {
            clip,
            ..Viewpoint::default()
        };
        let model = bx([-10.0, -10.0, -3.0], [10.0, 10.0, 9.0]);
        let (got, oblique) = section_of(&v, 0.0, Some(model));
        assert_eq!(oblique, 1);
        assert_eq!(got, Some(bx([0.0, 0.0, -4.0], [5.0, 5.0, 2.0])));
        assert_eq!(section_of(&v, 0.0, None).0, None, "an open side needs the model bounds");
        assert_eq!(section_of(&Viewpoint::default(), 0.0, Some(model)), (None, 0));
    }
}
