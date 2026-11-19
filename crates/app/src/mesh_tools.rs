// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Imported meshes to editable elements. The "Meshes" window: a mesh list (filter by
//! category and level), the recognized kind with confidence and a plan preview, batch
//! conversion; the edit tab has move, rotate, scale, mirror, crop and split
//! by height above the level, face removal by direction, welding, merging, info. Each
//! action is one `Editor::run_op`. Recognition runs in a background thread and is stored by
//! the mesh revision signature: the UI frame does not wait, and editing one mesh does not recompute others.
//! The window state lives here (thread_local); the shell only registers and calls the window.

use crate::ui_kit as kit;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use ddc_geom::mesh::Mesh;
use ddc_geom::mesh_fit::{Candidate, Fit};
use ddc_geom::mesh_ops::{self, Affine, Keep, Plane, Stats};
use ddc_geom::{v2, v3, V2, V3};
use ddc_model::{ConvertReport, MeshPlanCache, Model};
use ddc_ui::notify::Level;
use ddc_ui::theme::space;
use ddc_ui::widgets::field::format_number;
use ddc_ui::widgets::{
    self, Button, Cell, Column, NumberField, NumberFormat, RowState, Sort, Table,
};
use ddc_ui::Theme;
use egui::{self, Color32, RichText, Stroke};

use crate::editor::{OpFn, Sel};
use crate::shell::EditorApp;

/// No more than this many meshes in the list.
const LIST_LIMIT: usize = 300;
/// Plan cut height above the level, m (as for other plan layer elements).
const PLAN_CUT: f64 = 1.2;
const PREVIEW_TRIS: usize = 50_000;
/// Triangles sent to recognition per frame (the BLOB read happens on the UI thread).
const FEED_TRIS: usize = 200_000;
/// Direction tolerance for face removal, degrees.
const FACING_DEG: f64 = 10.0;
/// Face removal directions: (label key, vector).
const DIRS: [(&str, [f64; 3]); 6] = [
    ("mesh_tools.dir.up", [0.0, 0.0, 1.0]),
    ("mesh_tools.dir.down", [0.0, 0.0, -1.0]),
    ("mesh_tools.dir.px", [1.0, 0.0, 0.0]),
    ("mesh_tools.dir.nx", [-1.0, 0.0, 0.0]),
    ("mesh_tools.dir.py", [0.0, 1.0, 0.0]),
    ("mesh_tools.dir.ny", [0.0, -1.0, 0.0]),
];

type Sig = (u64, i64);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Convert,
    Edit,
}

struct Row {
    id: i64,
    sig: Sig,
    triangles: usize,
    category: String,
    name: String,
    /// None means still being recognized.
    cands: Option<Vec<Candidate>>,
    queued: bool,
    pick: usize,
    on: bool,
}

impl Row {
    fn cand(&self) -> Option<&Candidate> {
        self.cands.as_ref().and_then(|c| c.get(self.pick))
    }
}

struct Job {
    id: i64,
    sig: Sig,
    category: String,
    mesh: Mesh,
    solids: bool,
}

struct Done {
    id: i64,
    sig: Sig,
    cands: Vec<Candidate>,
}

/// The background recognition thread; it closes together with the channel.
struct Worker {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Done>,
}

impl Worker {
    fn spawn() -> Option<Worker> {
        let (tx, jobs) = mpsc::channel::<Job>();
        let (done, rx) = mpsc::channel::<Done>();
        // recognizes candidates from copied mesh data; a crash closes the channel, see line 392
        crate::jobs::spawn_guarded(
            "g4-recognize",
            move || {
                for j in jobs {
                    let cands = ddc_model::recognize_mesh_data(&j.mesh, &j.category, j.solids);
                    let d = Done {
                        id: j.id,
                        sig: j.sig,
                        cands,
                    };
                    if done.send(d).is_err() {
                        break;
                    }
                }
            },
            |_| {},
        )
        .ok()?;
        Some(Worker { tx, rx })
    }
}

struct State {
    open: bool,
    tab: Tab,
    category: Option<String>,
    this_level: bool,
    min_conf: f64,
    keep_source: bool,
    rows: Vec<Row>,
    total: usize,
    categories: Vec<(String, i64)>,
    /// (revision, level) the list was loaded with.
    loaded: Option<(u64, i64)>,
    focus: Option<i64>,
    preview: Option<(i64, Vec<(V2, V2)>)>,
    /// Candidates by mesh and its revision signature.
    known: HashMap<i64, (Sig, Vec<Candidate>)>,
    worker: Option<Worker>,
    dx: f64,
    dy: f64,
    dz: f64,
    angle: f64,
    scale: f64,
    /// Plane height above the current level.
    cut_z: f64,
    tol: f64,
    face_dir: usize,
    stats: Vec<(i64, Stats)>,
}

impl Default for State {
    fn default() -> Self {
        State {
            open: false,
            tab: Tab::Convert,
            category: None,
            this_level: true,
            min_conf: 0.6,
            // sources stay until the user picks "replace"
            keep_source: true,
            rows: vec![],
            total: 0,
            categories: vec![],
            loaded: None,
            focus: None,
            preview: None,
            known: HashMap::new(),
            worker: None,
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
            angle: 90.0,
            scale: 1.0,
            cut_z: PLAN_CUT,
            tol: 0.001,
            face_dir: 0,
            stats: vec![],
        }
    }
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
    static PLAN: RefCell<MeshPlanCache> = RefCell::new(MeshPlanCache::default());
}

fn with(f: impl FnOnce(&mut State)) {
    ST.with(|s| f(&mut s.borrow_mut()));
}

pub fn register(a: &mut EditorApp) {
    a.register_command("data.mesh_convert", h_convert);
    a.register_command("data.mesh_edit", h_edit);
}

fn h_convert(_: &mut EditorApp) {
    open_convert();
}

/// Opens the conversion window, where recognition can be reviewed by hand.
/// Source handling picked last in the convert window; quick converts follow it (default keep).
pub fn keep_source() -> bool {
    ST.with(|s| s.try_borrow().map_or(true, |s| s.keep_source))
}

#[cfg(test)]
pub(crate) fn set_keep_source(keep: bool) {
    with(|s| s.keep_source = keep);
}

pub fn open_convert() {
    with(|s| {
        s.open = true;
        s.tab = Tab::Convert;
        s.loaded = None;
    });
}

fn h_edit(_: &mut EditorApp) {
    with(|s| {
        s.open = true;
        s.tab = Tab::Edit;
        s.loaded = None;
    });
}

/// A selected mesh: an imported mesh body (`Sel::Body(BodyKind::Mesh, id)`).
fn mesh_of(s: &Sel) -> Option<i64> {
    match s {
        Sel::Body(crate::editor::bodies::BodyKind::Mesh, id) => Some(*id),
        _ => None,
    }
}

/// A mesh outline class in plan (the plan layer maps it to its style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanCls {
    /// Section fill: consecutive triangles (three points each), holes are real holes.
    Fill,
    Cut,
    Hole,
    Edge,
    /// Projected outline of a mesh below the cut plane.
    Beyond,
    /// Projected outline of a mesh above the cut plane (dashed).
    Overhead,
}

/// Level mesh outlines for the plan layer: section fill, its outlines and holes (lines),
/// edges below. Sections come from the cache by each mesh revision.
pub fn plan_polys(m: &Model, level: i64) -> Vec<(PlanCls, Vec<V2>, bool)> {
    let plans = PLAN.with(|c| m.mesh_plan_cached(level, PLAN_CUT, &mut c.borrow_mut()));
    let mut out = vec![];
    let ring = |mut r: Vec<V2>| {
        if let Some(&f) = r.first() {
            r.push(f);
        }
        r
    };
    for e in plans.unwrap_or_default() {
        if !e.fill.is_empty() {
            out.push((
                PlanCls::Fill,
                e.fill.iter().flatten().copied().collect(),
                false,
            ));
        }
        for r in e.cut {
            out.push((PlanCls::Cut, ring(r.outer), false));
            for h in r.holes {
                out.push((PlanCls::Hole, ring(h), false));
            }
        }
        for l in e.open {
            out.push((PlanCls::Edge, l, false));
        }
        for (a, b) in e.below {
            out.push((PlanCls::Edge, vec![a, b], false));
        }
        if e.beyond.len() >= 2 {
            let k = if e.overhead {
                PlanCls::Overhead
            } else {
                PlanCls::Beyond
            };
            let closed = e.beyond.len() >= 3;
            out.push((k, e.beyond, closed));
        }
    }
    out
}

/// The meshes window; called every frame from the shell.
pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if st.open {
        show(app, ctx, &mut st);
    }
    ST.with(|s| *s.borrow_mut() = st);
}

fn reload(st: &mut State, m: &Model, level: i64) {
    st.categories = m.mesh_categories().unwrap_or_default();
    let lv = st.this_level.then_some(level);
    let rows = m.mesh_rows(lv, st.category.as_deref()).unwrap_or_default();
    st.total = rows.len();
    let prev: Vec<(i64, bool, usize)> = st.rows.iter().map(|r| (r.id, r.on, r.pick)).collect();
    let known = &st.known;
    st.rows = rows
        .into_iter()
        .take(LIST_LIMIT)
        .map(|r| {
            let cands = known
                .get(&r.id)
                .filter(|k| k.0 == r.sig)
                .map(|k| k.1.clone());
            let (on, pick) = prev
                .iter()
                .find(|p| p.0 == r.id)
                .map_or((false, 0), |&(_, on, pick)| (on, pick));
            let mut row = Row {
                id: r.id,
                sig: r.sig,
                triangles: r.triangles,
                category: r.category,
                name: r.name,
                cands,
                queued: false,
                pick,
                on,
            };
            settle(&mut row);
            row
        })
        .collect();
    let ids: std::collections::HashSet<i64> = st.rows.iter().map(|r| r.id).collect();
    st.known.retain(|id, _| ids.contains(id));
    st.stats.clear();
}

/// Kind choice among the candidates.
fn settle(r: &mut Row) {
    if let Some(c) = &r.cands {
        if r.pick >= c.len() {
            r.pick = 0;
        }
        if c.is_empty() {
            r.on = false;
        }
    }
}

/// Send meshes without candidates to the thread and receive finished ones; true means some are unfinished.
fn pump(st: &mut State, m: &Model) -> bool {
    if st.worker.is_none() && st.rows.iter().any(|r| r.cands.is_none()) {
        st.worker = Worker::spawn();
    }
    let Some(w) = &st.worker else {
        // without a thread, in place (rare: the thread is not created)
        for r in st.rows.iter_mut().filter(|r| r.cands.is_none()) {
            if let Ok(c) = m.recognize_mesh(r.id, None) {
                st.known.insert(r.id, (r.sig, c.clone()));
                r.cands = Some(c);
            } else {
                r.cands = Some(vec![]);
            }
            settle(r);
        }
        return false;
    };
    while let Ok(d) = w.rx.try_recv() {
        if let Some(r) = st.rows.iter_mut().find(|r| r.id == d.id && r.sig == d.sig) {
            r.cands = Some(d.cands.clone());
            settle(r);
        }
        st.known.insert(d.id, (d.sig, d.cands));
    }
    let solids = m.solids_available().unwrap_or(false);
    let mut budget = FEED_TRIS;
    let mut dead = false;
    for r in st
        .rows
        .iter_mut()
        .filter(|r| r.cands.is_none() && !r.queued)
    {
        if r.triangles > budget && budget < FEED_TRIS {
            break;
        }
        budget = budget.saturating_sub(r.triangles);
        let Ok(i) = m.mesh_info(r.id) else {
            r.cands = Some(vec![]);
            continue;
        };
        let job = Job {
            id: r.id,
            sig: r.sig,
            category: i.category,
            mesh: i.mesh,
            solids,
        };
        if w.tx.send(job).is_err() {
            dead = true;
            break;
        }
        r.queued = true;
    }
    if dead {
        st.worker = None;
        st.rows.iter_mut().for_each(|r| r.queued = false);
    }
    st.rows.iter().any(|r| r.cands.is_none())
}

fn preview_edges(m: &Model, id: i64) -> Vec<(V2, V2)> {
    let Ok(i) = m.mesh_info(id) else {
        return vec![];
    };
    if i.mesh.triangles.len() > PREVIEW_TRIS {
        let s = mesh_ops::stats(&i.mesh, 1e-6);
        let (a, b) = (v2(s.min.x, s.min.y), v2(s.max.x, s.max.y));
        let (c, d) = (v2(b.x, a.y), v2(a.x, b.y));
        return vec![(a, c), (c, b), (b, d), (d, a)];
    }
    mesh_ops::edges(&i.mesh, 0.94, 1e-6)
        .into_iter()
        .map(|(p, q)| (v2(p.x, p.y), v2(q.x, q.y)))
        .filter(|(p, q)| (*q - *p).len() > 1e-9)
        .collect()
}

/// Rows the conversion can act on: a recognized kind exists.
fn eligible(r: &Row) -> bool {
    r.cand().is_some()
}

/// Ticked rows that Convert will turn into elements.
fn convertible(r: &Row) -> bool {
    r.on && eligible(r)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllState {
    Empty,
    Partial,
    Full,
}

/// State of the header checkbox over the rows that can be ticked.
fn all_state(rows: &[Row]) -> AllState {
    let total = rows.iter().filter(|r| eligible(r)).count();
    let on = rows.iter().filter(|r| convertible(r)).count();
    if on == 0 {
        AllState::Empty
    } else if on == total {
        AllState::Full
    } else {
        AllState::Partial
    }
}

/// Ticks or unticks every row that can be ticked; rows without a kind are left alone.
fn toggle_all(rows: &mut [Row], on: bool) {
    for r in rows.iter_mut().filter(|r| eligible(r)) {
        r.on = on;
    }
}

/// Unticks the rows of meshes that were just converted.
fn untick(rows: &mut [Row], ids: &[i64]) {
    for r in rows.iter_mut().filter(|r| ids.contains(&r.id)) {
        r.on = false;
    }
}

/// Ticks only rows whose kind matches the mesh category with confidence of at least `min`.
fn check_confident(rows: &mut [Row], min: f64) {
    for r in rows.iter_mut() {
        r.on = r
            .cand()
            .is_some_and(|c| c.consistent && c.confidence >= min);
    }
}

/// What Convert will do with the current ticks: shown before the click, so it matches the result.
#[derive(Debug, PartialEq, Eq)]
struct Plan {
    create: usize,
    remove: usize,
    /// (kind key, count), sorted by key.
    kinds: Vec<(String, usize)>,
}

fn plan_of(rows: &[Row], keep: bool) -> Plan {
    let mut kinds: Vec<(String, usize)> = vec![];
    let mut create = 0;
    for c in rows.iter().filter(|r| convertible(r)).filter_map(Row::cand) {
        create += 1;
        let k = c.kind().key();
        match kinds.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 += 1,
            None => kinds.push((k.to_string(), 1)),
        }
    }
    kinds.sort();
    Plan {
        create,
        remove: if keep { 0 } else { create },
        kinds,
    }
}

fn convert_key(keep: bool) -> &'static str {
    if keep {
        "mesh_recognize.convert_keep"
    } else {
        "mesh_recognize.convert_remove"
    }
}

fn convert_label(tr: &dyn Fn(&str) -> String, n: usize, keep: bool) -> String {
    tr(convert_key(keep)).replace("{n}", &n.to_string())
}

/// Reason key for the disabled primary action; None when something is ticked.
fn blocked_key(n: usize) -> Option<&'static str> {
    (n == 0).then_some("dlg.mesh.tick")
}

/// (what is created, what happens to the sources); the second is empty with nothing ticked.
fn plan_lines(tr: &dyn Fn(&str) -> String, p: &Plan, keep: bool) -> (String, String) {
    if p.create == 0 {
        return (tr("mesh_recognize.creates_none"), String::new());
    }
    let kinds: Vec<String> = p
        .kinds
        .iter()
        .map(|(k, n)| format!("{n} {}", tr(&format!("mesh_tools.kind.{k}")).to_lowercase()))
        .collect();
    let head = tr("mesh_recognize.creates")
        .replace("{n}", &p.create.to_string())
        .replace("{kinds}", &kinds.join(", "));
    let src = if keep {
        tr("mesh_recognize.stay").replace("{n}", &p.create.to_string())
    } else {
        tr("mesh_recognize.removed").replace("{n}", &p.remove.to_string())
    };
    (head, src)
}

enum Act {
    Convert,
    Move,
    Rotate,
    Scale,
    MirrorX,
    MirrorY,
    Cut(Keep),
    Split,
    DeleteFacing(V3),
    Weld,
    Merge,
    Stats,
}

fn show(app: &mut EditorApp, ctx: &egui::Context, st: &mut State) {
    let key = (app.ed.revision, app.ed.level);
    if st.loaded != Some(key) {
        reload(st, &app.ed.model, app.ed.level);
        st.loaded = Some(key);
        st.preview = None;
    }
    if pump(st, &app.ed.model) {
        ctx.request_repaint();
    }
    if let Some(f) = st.focus {
        if st.preview.as_ref().map(|p| p.0) != Some(f) {
            st.preview = Some((f, preview_edges(&app.ed.model, f)));
        }
    }
    let cat = &app.sh.cat;
    let tr = |k: &str| cat.t(k);
    let th = &app.sh.theme;
    let (sec, acc) = (th.c("text.secondary"), th.c("accent"));
    let nf = NumberFormat::for_lang(cat.lang);
    let mut open = true;
    let mut act: Option<Act> = None;
    let mut refilter = false;
    let pending = st.rows.iter().filter(|r| r.cands.is_none()).count();
    kit::Dialog::new("mesh_tools", tr("mesh_tools.title"), kit::DialogSize::Wide)
        .open(&mut open)
        .scroll_body()
        .show(ctx, |ui| {
            let tabs = [tr("mesh_tools.tab.convert"), tr("mesh_tools.tab.edit")];
            let tab_names: Vec<&str> = tabs.iter().map(String::as_str).collect();
            if let Some(i) = widgets::segmented(ui, th, &tab_names, usize::from(st.tab == Tab::Edit))
            {
                st.tab = if i == 1 { Tab::Edit } else { Tab::Convert };
            }
            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                let mut items = vec![tr("mesh_tools.all")];
                items.extend(st.categories.iter().map(|(c, n)| format!("{c} ({n})")));
                let names: Vec<&str> = items.iter().map(String::as_str).collect();
                let mut sel = st
                    .category
                    .as_ref()
                    .and_then(|c| st.categories.iter().position(|(k, _)| k == c))
                    .map_or(0, |i| i + 1);
                if widgets::dropdown(ui, th, "g4-cat", &mut sel, &names, None).changed() {
                    let next = sel
                        .checked_sub(1)
                        .and_then(|i| st.categories.get(i))
                        .map(|(c, _)| c.clone());
                    st.category = next;
                    refilter = true;
                }
                refilter |= widgets::checkbox(ui, th, &mut st.this_level, &tr("mesh_tools.this_level"))
                    .changed();
                if st.total > st.rows.len() {
                    ui.label(
                        RichText::new(
                            tr("mesh_tools.shown")
                                .replace("{n}", &st.rows.len().to_string())
                                .replace("{total}", &st.total.to_string()),
                        )
                        .color(sec),
                    );
                }
                if pending > 0 {
                    widgets::surface::spinner(ui, th);
                    ui.label(
                        RichText::new(
                            tr("mesh_recognize.recognizing")
                                .replace("{done}", &(st.rows.len() - pending).to_string())
                                .replace("{total}", &st.rows.len().to_string()),
                        )
                        .color(sec),
                    );
                }
            });
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                let eligible_n = st.rows.iter().filter(|r| eligible(r)).count();
                let state = all_state(&st.rows);
                let mut all_on = state == AllState::Full;
                let all_label = tr("mesh_recognize.all").replace("{n}", &eligible_n.to_string());
                let mixed = state == AllState::Partial;
                if widgets::checkbox_mixed(ui, th, &mut all_on, mixed, &all_label).changed() {
                    toggle_all(&mut st.rows, all_on);
                }
                // only a kind matching the mesh category: a cupboard never becomes a column
                let only = tr("mesh_recognize.only_confident");
                let tip = tr("mesh_recognize.confident_tip")
                    .replace("{pct}", &format!("{:.0}", st.min_conf * 100.0));
                if Button::quiet(&only)
                    .show(ui, th)
                    .on_hover_text(tip)
                    .clicked()
                {
                    check_confident(&mut st.rows, st.min_conf);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let before = (st.min_conf * 100.0).round();
                    let mut pct = before;
                    let cap = tr("mesh_tools.min_conf");
                    NumberField::new("g4-min-conf", &mut pct, nf)
                        .labelled_by(egui::Id::new("g4-min-conf.lbl"), &cap)
                        .decimals(0)
                        .step(5.0)
                        .range(30.0, 100.0)
                        .unit("%")
                        .width(NUM_W)
                        .show(ui, th);
                    ui.label(RichText::new(cap).color(sec));
                    if pct != before {
                        st.min_conf = (pct / 100.0).clamp(0.3, 1.0);
                    }
                });
            });
            ui.add_space(space::XS);
            if st.rows.is_empty() {
                kit::empty_note(ui, tr("dlg.mesh.none"), Some(&tr("dlg.mesh.none_hint")));
            } else {
                rows_table(ui, th, &tr, st);
            }
            ui.add_space(space::SM);
            match st.tab {
                Tab::Convert => {
                    preview(ui, st, sec, acc);
                    ui.add_space(space::SM);
                    source_choice(ui, th, &tr, st, sec);
                    kit::dialog_footer(ui, |ui| {
                        let n = st.rows.iter().filter(|r| convertible(r)).count();
                        let why = blocked_key(n).map(&tr);
                        let label = convert_label(&tr, n, st.keep_source);
                        // converting that removes the sources is click only, never Enter
                        let go = if st.keep_source {
                            kit::primary_action(ui, label, why.as_deref())
                        } else {
                            kit::primary_click(ui, label, why.as_deref())
                        };
                        if go {
                            act = Some(Act::Convert);
                        }
                    });
                }
                Tab::Edit => edit_panel(ui, th, nf, st, &tr, &mut act),
            }
        });
    if !open {
        st.open = false;
    }
    if refilter {
        st.loaded = None;
    }
    if let Some(a) = act {
        run(app, st, a);
    }
}

/// Width of every number field in this window.
const NUM_W: f32 = ddc_ui::theme::control::FIELD_MIN_W * 2.0;

/// Rows visible in the list before it scrolls.
const VISIBLE_ROWS: f32 = 8.0;

fn kind_label(tr: &dyn Fn(&str) -> String, c: &Candidate) -> String {
    tr(&format!("mesh_tools.kind.{}", c.kind().key()))
}

fn row_cell(r: &Row, col: usize, tr: &dyn Fn(&str) -> String) -> Cell {
    match col {
        0 if convertible(r) => Cell::text("\u{2713}"),
        0 => Cell::text(""),
        1 => Cell::text(r.category.clone()),
        2 => Cell::text(r.name.clone()),
        3 => match &r.cands {
            None => Cell::muted(tr("mesh_tools.pending")),
            Some(c) if c.is_empty() => Cell::muted(tr("mesh_tools.kind.none")),
            Some(c) => Cell::text(kind_label(tr, &c[r.pick.min(c.len() - 1)])),
        },
        4 => r.cand().map_or_else(
            || Cell::text(""),
            |c| Cell::num(format!("{:.0}", c.confidence * 100.0), "%"),
        ),
        _ => {
            let notes: Vec<String> = r.cand().map_or_else(Vec::new, |c| {
                c.notes.iter().map(|n| tr(&format!("mesh_tools.note.{n}"))).collect()
            });
            Cell::muted(notes.join(", "))
        }
    }
}

/// The mesh list: a click ticks a row and focuses it, arrows move the focus, Enter ticks.
fn rows_table(ui: &mut egui::Ui, th: &Theme, tr: &dyn Fn(&str) -> String, st: &mut State) {
    let heads: Vec<String> = [
        "",
        "mesh_tools.col.category",
        "mesh_tools.col.name",
        "mesh_tools.col.kind",
        "mesh_tools.col.conf",
        "mesh_tools.col.notes",
    ]
    .iter()
    .map(|k| if k.is_empty() { String::new() } else { tr(*k) })
    .collect();
    let cols = [
        Column::text(&heads[0]).fixed(space::XL).sortable(false),
        Column::text(&heads[1]).fill(0.8).sortable(false),
        Column::text(&heads[2]).fill(1.4).sortable(false),
        Column::text(&heads[3]).fill(1.0).sortable(false),
        Column::number(&heads[4], space::XXXL + space::LG).sortable(false),
        Column::text(&heads[5]).fill(1.4).sortable(false),
    ];
    let mut sort: Option<Sort> = None;
    let focus = st.focus;
    let rows = &st.rows;
    let out = Table::new("g4-rows", &cols, rows.len())
        .max_height(th.d().row * (VISIBLE_ROWS + 1.0))
        .show(
            ui,
            th,
            &mut sort,
            |i| {
                if focus == Some(rows[i].id) {
                    RowState::Selected
                } else if rows[i].cands.as_ref().is_some_and(|c| c.is_empty()) {
                    RowState::Disabled
                } else {
                    RowState::Normal
                }
            },
            |i, c| row_cell(&rows[i], c, tr),
        );
    let cur = focus.and_then(|f| st.rows.iter().position(|r| r.id == f));
    let mut toggle = None;
    if let Some(i) = out.clicked {
        st.focus = st.rows.get(i).map(|r| r.id);
        toggle = Some(i);
    }
    if let Some(d) = out.moved {
        let n = st.rows.len() as isize;
        let base = cur.map_or(if d > 0 { -1 } else { n }, |c| c as isize);
        let next = (base + d).clamp(0, n - 1) as usize;
        st.focus = st.rows.get(next).map(|r| r.id);
    }
    if out.activated {
        toggle = cur;
    }
    if let Some(r) = toggle.and_then(|i| st.rows.get_mut(i)) {
        if eligible(r) {
            r.on = !r.on;
        }
    }
    let sec = th.c("text.secondary");
    ui.label(RichText::new(tr("mesh_recognize.row_hint")).color(sec));
    // the kind of the focused row: several fits can describe one mesh
    let Some(r) = st.focus.and_then(|f| st.rows.iter_mut().find(|r| r.id == f)) else {
        return;
    };
    let Some(labels) = r.cands.as_ref().filter(|c| c.len() > 1).map(|c| {
        c.iter()
            .map(|c| format!("{} · {:.0}%", kind_label(tr, c), c.confidence * 100.0))
            .collect::<Vec<String>>()
    }) else {
        return;
    };
    let names: Vec<&str> = labels.iter().map(String::as_str).collect();
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("mesh_recognize.kind_for").replace("{name}", &r.name)).color(sec));
        widgets::dropdown(ui, th, ("g4-kind", r.id), &mut r.pick, &names, None);
    });
}

/// Source handling as an explicit choice with a count of what Convert will create and remove.
fn source_choice(
    ui: &mut egui::Ui,
    th: &Theme,
    tr: &dyn Fn(&str) -> String,
    st: &mut State,
    sec: Color32,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr("mesh_recognize.source")).color(sec));
        let items = [tr("mesh_recognize.keep"), tr("mesh_recognize.replace")];
        let names: Vec<&str> = items.iter().map(String::as_str).collect();
        if let Some(i) = widgets::segmented(ui, th, &names, usize::from(!st.keep_source)) {
            st.keep_source = i == 0;
        }
    });
    let plan = plan_of(&st.rows, st.keep_source);
    let (head, src) = plan_lines(tr, &plan, st.keep_source);
    ui.label(RichText::new(head).color(sec));
    if !src.is_empty() {
        let col = if plan.remove > 0 { th.sem().danger } else { sec };
        ui.label(RichText::new(src).color(col));
    }
}

fn preview(ui: &mut egui::Ui, st: &State, sec: Color32, acc: Color32) {
    let w = ui.available_width().max(200.0);
    let (resp, p) = ui.allocate_painter(egui::vec2(w, 200.0), egui::Sense::hover());
    let r = resp.rect;
    p.rect_filled(r, ddc_ui::theme::radius::MD, ui.visuals().extreme_bg_color);
    let Some(row) = st.focus.and_then(|f| st.rows.iter().find(|x| x.id == f)) else {
        return;
    };
    let edges: &[(V2, V2)] = match &st.preview {
        Some(p) => &p.1[..],
        None => &[],
    };
    let outline = row.cand().map(|c| c.fit.plan_outline()).unwrap_or_default();
    let pts = edges
        .iter()
        .flat_map(|e| [e.0, e.1])
        .chain(outline.iter().flatten().copied());
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for q in pts {
        lo = v2(lo.x.min(q.x), lo.y.min(q.y));
        hi = v2(hi.x.max(q.x), hi.y.max(q.y));
    }
    if lo.x > hi.x {
        return;
    }
    let span = (hi.x - lo.x).max(hi.y - lo.y).max(1e-6);
    let k = ((r.width().min(r.height()) - 20.0).max(10.0) as f64) / span;
    let c = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    let mid = r.center();
    let to_s = |q: V2| {
        egui::pos2(
            mid.x + ((q.x - c.x) * k) as f32,
            mid.y - ((q.y - c.y) * k) as f32,
        )
    };
    for (a, b) in edges {
        p.line_segment([to_s(*a), to_s(*b)], Stroke::new(1.0, sec));
    }
    for ring in &outline {
        let s: Vec<egui::Pos2> = ring.iter().map(|q| to_s(*q)).collect();
        if s.len() >= 2 {
            p.add(egui::Shape::closed_line(s, Stroke::new(1.6, acc)));
        }
    }
}

/// One secondary button; true when clicked.
fn op_button(ui: &mut egui::Ui, th: &Theme, label: &str) -> bool {
    Button::secondary(label).show(ui, th).clicked()
}

/// A length field in metres, named by the row label for screen readers.
#[allow(clippy::too_many_arguments)]
fn num_field(
    ui: &mut egui::Ui,
    th: &Theme,
    nf: NumberFormat,
    salt: &str,
    v: &mut f64,
    unit: &str,
    decimals: usize,
    label: (egui::Id, &str),
) {
    NumberField::new(salt, v, nf)
        .decimals(decimals)
        .step(10f64.powi(-(decimals.min(3) as i32)))
        .unit(unit)
        .width(NUM_W)
        .labelled_by(label.0, label.1)
        .show(ui, th);
}

fn edit_panel(
    ui: &mut egui::Ui,
    th: &Theme,
    nf: NumberFormat,
    st: &mut State,
    tr: &dyn Fn(&str) -> String,
    act: &mut Option<Act>,
) {
    let m = tr("mesh_tools.unit.m");
    let apply = tr("mesh_tools.apply");
    let (l_move, l_rot, l_scale) = (tr("mesh_tools.move"), tr("mesh_tools.rotate"), tr("mesh_tools.scale"));
    let (l_cut, l_faces, l_weld) = (tr("mesh_tools.cut_at"), tr("mesh_tools.delete_faces"), tr("mesh_tools.weld"));
    kit::labeled_row_id(ui, &l_move, |ui, id| {
        num_field(ui, th, nf, "g4-dx", &mut st.dx, &m, 3, (id, &l_move));
        num_field(ui, th, nf, "g4-dy", &mut st.dy, &m, 3, (id, &l_move));
        num_field(ui, th, nf, "g4-dz", &mut st.dz, &m, 3, (id, &l_move));
        if op_button(ui, th, &apply) {
            *act = Some(Act::Move);
        }
    });
    kit::labeled_row_id(ui, &l_rot, |ui, id| {
        num_field(ui, th, nf, "g4-angle", &mut st.angle, "\u{b0}", 1, (id, &l_rot));
        if op_button(ui, th, &apply) {
            *act = Some(Act::Rotate);
        }
    });
    kit::labeled_row_id(ui, &l_scale, |ui, id| {
        NumberField::new("g4-scale", &mut st.scale, nf)
            .decimals(3)
            .step(0.01)
            .range(0.001, 1000.0)
            .width(NUM_W)
            .labelled_by(id, &l_scale)
            .show(ui, th);
        if op_button(ui, th, &apply) {
            *act = Some(Act::Scale);
        }
    });
    kit::labeled_row(ui, &tr("mesh_tools.mirror"), |ui| {
        if op_button(ui, th, &tr("mesh_tools.mirror_x")) {
            *act = Some(Act::MirrorX);
        }
        if op_button(ui, th, &tr("mesh_tools.mirror_y")) {
            *act = Some(Act::MirrorY);
        }
    });
    kit::labeled_row_id(ui, &l_cut, |ui, id| {
        num_field(ui, th, nf, "g4-cut-z", &mut st.cut_z, &m, 3, (id, &l_cut));
        if op_button(ui, th, &tr("mesh_tools.keep_below")) {
            *act = Some(Act::Cut(Keep::Below));
        }
        if op_button(ui, th, &tr("mesh_tools.keep_above")) {
            *act = Some(Act::Cut(Keep::Above));
        }
        if op_button(ui, th, &tr("mesh_tools.split")) {
            *act = Some(Act::Split);
        }
    });
    kit::labeled_row(ui, &l_faces, |ui| {
        let dirs: Vec<String> = DIRS.iter().map(|d| tr(d.0)).collect();
        let names: Vec<&str> = dirs.iter().map(String::as_str).collect();
        st.face_dir = st.face_dir.min(DIRS.len() - 1);
        widgets::dropdown(ui, th, "g4-face-dir", &mut st.face_dir, &names, None);
        if op_button(ui, th, &apply) {
            let d = DIRS[st.face_dir].1;
            *act = Some(Act::DeleteFacing(v3(d[0], d[1], d[2])));
        }
    });
    kit::labeled_row_id(ui, &l_weld, |ui, id| {
        NumberField::new("g4-tol", &mut st.tol, nf)
            .decimals(6)
            .step(0.0001)
            .range(0.000001, 0.1)
            .unit(&m)
            .width(NUM_W)
            .labelled_by(id, &l_weld)
            .show(ui, th);
        if op_button(ui, th, &apply) {
            *act = Some(Act::Weld);
        }
    });
    kit::labeled_row(ui, "", |ui| {
        if op_button(ui, th, &tr("mesh_tools.merge")) {
            *act = Some(Act::Merge);
        }
        if op_button(ui, th, &tr("mesh_tools.stats")) {
            *act = Some(Act::Stats);
        }
    });
    if st.stats.is_empty() {
        return;
    }
    ui.add_space(space::SM);
    let yes_no = |b: bool| tr(if b { "mesh_tools.yes" } else { "mesh_tools.no" });
    let heads: Vec<String> = [
        "mesh_tools.col.name",
        "mesh_tools.col.tris",
        "mesh_tools.col.area",
        "mesh_tools.col.volume",
        "mesh_tools.col.closed",
        "mesh_tools.col.convex",
    ]
    .iter()
    .map(|k| tr(*k))
    .collect();
    let cols = [
        Column::text(&heads[0]).fill(1.4).sortable(false),
        Column::number(&heads[1], space::XXXL + space::LG).sortable(false),
        Column::number(&heads[2], space::XXXL + space::XL).sortable(false),
        Column::number(&heads[3], space::XXXL + space::XL).sortable(false),
        Column::text(&heads[4]).fixed(space::XXXL + space::LG).sortable(false),
        Column::text(&heads[5]).fixed(space::XXXL + space::LG).sortable(false),
    ];
    let mut sort: Option<Sort> = None;
    let stats = &st.stats;
    let rows = &st.rows;
    Table::new("g4-stats", &cols, stats.len())
        .max_height(th.d().row * (VISIBLE_ROWS + 1.0))
        .show(
            ui,
            th,
            &mut sort,
            |_| RowState::Normal,
            |i, c| {
                let (id, s) = &stats[i];
                let solid = s.closed && s.oriented;
                match c {
                    0 => Cell::text(
                        rows.iter()
                            .find(|r| r.id == *id)
                            .map_or_else(|| id.to_string(), |r| r.name.clone()),
                    ),
                    1 => Cell::num(s.triangles.to_string(), ""),
                    2 => Cell::num(format_number(s.area, 3, nf), ""),
                    // the volume of an open or inconsistent mesh is undefined
                    3 if solid => Cell::num(format_number(s.volume.abs(), 4, nf), ""),
                    3 => Cell::muted("\u{2014}"),
                    4 => Cell::text(yes_no(solid)),
                    _ => s
                        .convex
                        .map_or_else(|| Cell::muted("\u{2014}"), |b| Cell::text(yes_no(b))),
                }
            },
        );
}

/// Edit targets: the selected meshes, otherwise the checked rows.
fn targets(app: &EditorApp, st: &State) -> Vec<i64> {
    let sel: Vec<i64> = app.ed.selection.iter().filter_map(mesh_of).collect();
    if !sel.is_empty() {
        return sel;
    }
    st.rows.iter().filter(|r| r.on).map(|r| r.id).collect()
}

/// The center of the targets' bounds in plan and the bottom.
fn center(m: &Model, ids: &[i64]) -> (f64, f64, f64) {
    let (mut lo, mut hi) = (
        v3(f64::MAX, f64::MAX, f64::MAX),
        v3(f64::MIN, f64::MIN, f64::MIN),
    );
    for &id in ids {
        if let Ok(s) = m.mesh_stats(id) {
            lo = v3(lo.x.min(s.min.x), lo.y.min(s.min.y), lo.z.min(s.min.z));
            hi = v3(hi.x.max(s.max.x), hi.y.max(s.max.y), hi.z.max(s.max.z));
        }
    }
    if lo.x > hi.x {
        return (0.0, 0.0, 0.0);
    }
    ((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0, lo.z)
}

fn op(f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static) -> OpFn {
    OpFn(Arc::new(f))
}

fn run(app: &mut EditorApp, st: &mut State, a: Act) {
    let tr = |app: &EditorApp, k: &str| app.sh.cat.t(k);
    let report: Arc<Mutex<Option<ConvertReport>>> = Arc::new(Mutex::new(None));
    let f = match a {
        Act::Convert => {
            let items: Vec<(i64, Fit)> = st
                .rows
                .iter()
                .filter(|r| convertible(r))
                .filter_map(|r| r.cand().map(|c| (r.id, c.fit.clone())))
                .collect();
            let keep = st.keep_source;
            let out = report.clone();
            op(move |m| {
                let r = m.convert_meshes(&items, keep)?;
                if let Ok(mut g) = out.lock() {
                    *g = Some(r);
                }
                Ok(())
            })
        }
        Act::Stats => {
            st.stats = targets(app, st)
                .into_iter()
                .filter_map(|id| Some((id, app.ed.model.mesh_stats(id).ok()?)))
                .collect();
            return;
        }
        _ => {
            let ids = targets(app, st);
            if ids.is_empty() {
                let msg = tr(app, "mesh_tools.no_targets");
                app.sh.toasts.push(Level::Warning, msg, None);
                return;
            }
            let (cx, cy, z0) = center(&app.ed.model, &ids);
            let c = v3(cx, cy, z0);
            let tf = |f: Affine| {
                let ids = ids.clone();
                op(move |m| m.transform_meshes(&ids, &f))
            };
            // the plane height is from the current level, like the plan cut plane
            let zc = app.ed.elevation + st.cut_z;
            match a {
                Act::Move => tf(Affine::translate(v3(st.dx, st.dy, st.dz))),
                Act::Rotate => tf(Affine::rotate_z(c, st.angle.to_radians())),
                Act::Scale => tf(Affine::scale(c, v3(st.scale, st.scale, st.scale))),
                Act::MirrorX | Act::MirrorY => {
                    let (p, q) = if matches!(a, Act::MirrorX) {
                        (v2(cx, cy - 1.0), v2(cx, cy + 1.0))
                    } else {
                        (v2(cx - 1.0, cy), v2(cx + 1.0, cy))
                    };
                    let Some(pl) = Plane::vertical(p, q) else {
                        return;
                    };
                    tf(Affine::mirror(pl))
                }
                Act::Cut(keep) => {
                    let pl = Plane::horizontal(zc);
                    op(move |m| m.cut_meshes(&ids, &pl, keep))
                }
                Act::Split => {
                    let pl = Plane::horizontal(zc);
                    op(move |m| m.split_meshes(&ids, &pl).map(|_| ()))
                }
                Act::DeleteFacing(d) => {
                    op(move |m| m.delete_mesh_faces_facing(&ids, d, FACING_DEG).map(|_| ()))
                }
                Act::Weld => {
                    let tol = st.tol;
                    op(move |m| m.clean_meshes(&ids, tol).map(|_| ()))
                }
                Act::Merge => op(move |m| m.merge_meshes(&ids).map(|_| ())),
                Act::Convert | Act::Stats => return,
            }
        }
    };
    match app.ed.run_op(f) {
        Ok(()) => {
            let rep = report.lock().ok().and_then(|g| g.clone());
            let msg = match &rep {
                Some(r) if r.failed.is_empty() => {
                    tr(app, "mesh_tools.converted").replace("{n}", &r.done.len().to_string())
                }
                Some(r) => tr(app, "mesh_tools.converted_some")
                    .replace("{n}", &r.done.len().to_string())
                    .replace("{failed}", &r.failed.len().to_string()),
                None => tr(app, "mesh_tools.done"),
            };
            app.sh.toasts.push(Level::Info, msg, None);
            if let Some(w) = rep
                .as_ref()
                .and_then(|r| r.warnings.first().map(|w| (r, w)))
            {
                let msg = tr(app, "mesh_tools.warnings")
                    .replace("{n}", &w.0.warnings.len().to_string())
                    .replace("{first}", &w.1 .1);
                app.sh.toasts.push(Level::Warning, msg, None);
            }
            // kept sources stay in the list: untick them so a second click cannot duplicate
            if let Some(r) = &rep {
                let ids: Vec<i64> = r.done.iter().map(|c| c.mesh).collect();
                untick(&mut st.rows, &ids);
            }
            st.stats.clear();
        }
        Err(e) => {
            let msg = format!("{}: {e}", tr(app, "mesh_tools.failed"));
            app.sh.toasts.push(Level::Warning, msg, None);
        }
    }
    st.loaded = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::{Catalog, Lang};

    fn wall() -> Fit {
        Fit::Wall {
            a: v2(0.0, 0.0),
            b: v2(6.0, 0.0),
            z0: 0.0,
            height: 3.0,
            thickness: 0.2,
            openings: vec![],
            top: None,
        }
    }

    fn cand(confidence: f64, consistent: bool) -> Candidate {
        Candidate {
            fit: wall(),
            confidence,
            mesh_volume: 1.0,
            fit_volume: 1.0,
            notes: vec![],
            consistent,
        }
    }

    fn row(id: i64, on: bool, cands: Option<Vec<Candidate>>) -> Row {
        Row {
            id,
            sig: (0, 0),
            triangles: 12,
            category: "Walls".to_string(),
            name: format!("m{id}"),
            cands,
            queued: false,
            pick: 0,
            on,
        }
    }

    fn sample() -> Vec<Row> {
        vec![
            row(1, true, Some(vec![cand(0.9, true)])),
            row(2, true, Some(vec![cand(0.5, false)])),
            row(3, false, Some(vec![cand(0.8, true)])),
            row(4, true, None),
            row(5, true, Some(vec![])),
        ]
    }

    const KEYS: [&str; 13] = [
        "mesh_recognize.recognizing",
        "mesh_recognize.all",
        "mesh_recognize.only_confident",
        "mesh_recognize.confident_tip",
        "mesh_recognize.row_hint",
        "mesh_recognize.kind_for",
        "mesh_recognize.source",
        "mesh_recognize.keep",
        "mesh_recognize.replace",
        "mesh_recognize.convert_keep",
        "mesh_recognize.convert_remove",
        "mesh_recognize.creates",
        "mesh_recognize.creates_none",
    ];

    #[test]
    fn default_keeps_the_source_meshes() {
        assert!(State::default().keep_source);
        assert_eq!(plan_of(&sample(), true).remove, 0);
    }

    #[test]
    fn remove_option_counts_exactly_the_converted_rows() {
        let rows = sample();
        let keep = plan_of(&rows, true);
        let gone = plan_of(&rows, false);
        // rows 1 and 2 are ticked with a kind; 4 is still recognizing, 5 has no kind
        assert_eq!((keep.create, gone.create), (2, 2));
        assert_eq!(gone.remove, 2);
        assert_eq!(gone.kinds, vec![("wall".to_string(), 2)]);
        let items = rows.iter().filter(|r| convertible(r)).count();
        assert_eq!(items, gone.create, "preview matches what Convert sends");
    }

    #[test]
    fn empty_selection_is_blocked_with_a_reason() {
        let mut rows = sample();
        toggle_all(&mut rows, false);
        let p = plan_of(&rows, false);
        assert_eq!((p.create, p.remove), (0, 0));
        assert_eq!(blocked_key(p.create), Some("dlg.mesh.tick"));
        assert_eq!(blocked_key(1), None);
        let c = Catalog::new(Lang::En);
        for lang in [Lang::En, Lang::Ru] {
            assert_ne!(c.t_in(lang, "dlg.mesh.tick"), "dlg.mesh.tick");
        }
    }

    #[test]
    fn header_checkbox_state_and_toggle() {
        let mut rows = sample();
        assert_eq!(all_state(&rows), AllState::Partial);
        toggle_all(&mut rows, true);
        assert_eq!(all_state(&rows), AllState::Full);
        assert!(rows[3].on, "a row still recognizing is untouched");
        assert!(rows[4].on, "a row without a kind keeps its value");
        toggle_all(&mut rows, false);
        assert_eq!(all_state(&rows), AllState::Empty);
    }

    #[test]
    fn only_confident_uses_category_match_and_threshold() {
        let mut rows = sample();
        check_confident(&mut rows, 0.6);
        let on: Vec<i64> = rows.iter().filter(|r| r.on).map(|r| r.id).collect();
        assert_eq!(on, vec![1, 3]);
    }

    #[test]
    fn converted_rows_are_unticked() {
        let mut rows = sample();
        untick(&mut rows, &[1, 2]);
        assert!(!rows[0].on && !rows[1].on);
        assert!(rows[3].on, "others keep their ticks");
    }

    #[test]
    fn labels_and_summary_exist_in_both_languages() {
        let c = Catalog::new(Lang::En);
        for lang in [Lang::En, Lang::Ru] {
            let tr = |k: &str| c.t_in(lang, k);
            for k in KEYS.iter().chain(["mesh_recognize.stay", "mesh_recognize.removed"].iter()) {
                assert_ne!(tr(k), *k, "{k} missing in {lang:?}");
            }
            assert!(convert_label(&tr, 3, true).contains('3'));
            assert!(convert_label(&tr, 3, false).contains('3'));
            assert_ne!(convert_label(&tr, 3, true), convert_label(&tr, 3, false));
            let plan = plan_of(&sample(), false);
            let (head, src) = plan_lines(&tr, &plan, false);
            assert!(head.contains('2') && src.contains('2'));
            let (_, kept) = plan_lines(&tr, &plan_of(&sample(), true), true);
            assert!(kept.contains('2'));
            assert_ne!(src, kept);
        }
        let en = |k: &str| c.t_in(Lang::En, k);
        let ru = |k: &str| c.t_in(Lang::Ru, k);
        assert_ne!(convert_label(&en, 1, false), convert_label(&ru, 1, false));
        for k in KEYS.iter().chain(["mesh_recognize.stay", "mesh_recognize.removed"].iter()) {
            assert_ne!(en(k), ru(k), "{k} not translated");
        }
    }

    #[test]
    fn dialog_uses_kit_widgets_only() {
        let src = include_str!("mesh_tools.rs");
        let prod = src.split("#[cfg(test)]").next().unwrap();
        for raw in [
            "egui::ComboBox",
            "ui.checkbox(",
            "egui::DragValue",
            "selectable_label",
            "selectable_value",
            "egui::Slider",
            "egui::Grid",
            "ui.button(",
            ".spacing([",
            "ui.separator()",
        ] {
            assert!(!prod.contains(raw), "raw widget in mesh_tools.rs: {raw}");
        }
    }
}
