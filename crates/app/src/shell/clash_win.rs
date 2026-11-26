// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Clashes" window: sets A and B (categories, level, selection), tolerance, hard or clearance;
//! a threaded check with progress; a table of pairs grouped with statuses and comments (stored
//! in the project); "Show" — plan and 3D with isolation and highlight; quick fixes; report.
use crate::ui_kit as kit;
use crate::units::{self, Units};
use ddc_ui::widgets::{self, Button, Cell, Column, MenuTone, RowState, Table, TextField};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{mpsc, Arc};

use ddc_geom::v2;
use ddc_model::{
    is_mep, ClashFix, ClashHit, ClashItem, ClashParams, ClashSide, ClashStatus, ElKey, CLASH_CATS,
};
use ddc_render::glam::Vec3;
use ddc_render::{Aabb, Scene, SceneMesh};
use ddc_ui::notify::Level;
use egui::{self, Color32, RichText};

use super::EditorApp;
use crate::range_ui::Loc;
use crate::editor::{OpFn, Sel};
use crate::isolate::Temp;

/// Element numbers of markers in the 3D scene: far outside the range of model elements.
pub(crate) const MARK: u32 = 1_000_000;

type Pair = (ElKey, ElKey);

pub fn register(a: &mut EditorApp) {
    a.register_command("data.clash", open);
    a.register_command("data.clash_report", report_cmd);
}

fn open(a: &mut EditorApp) {
    a.clash.open = true;
}

/// Ribbon: "Clash report" — a window with the ready list, with export from it.
fn report_cmd(a: &mut EditorApp) {
    a.clash.open = true;
}

#[derive(Clone)]
pub struct SideUi {
    pub cats: [bool; 8],
    pub level: Option<i64>,
    pub selected: bool,
}

impl SideUi {
    fn new(mep: bool) -> Self {
        SideUi {
            cats: std::array::from_fn(|i| is_mep(CLASH_CATS[i]) == mep),
            level: None,
            selected: false,
        }
    }
    fn side(&self, sel: &[ElKey]) -> ClashSide {
        ClashSide {
            cats: (0..8)
                .filter(|&i| self.cats[i])
                .map(|i| CLASH_CATS[i].to_string())
                .collect(),
            level: self.level,
            only: self.selected.then(|| sel.to_vec()),
        }
    }
}

struct Job {
    rx: mpsc::Receiver<(Vec<ClashHit>, bool)>,
    done: Arc<(AtomicUsize, AtomicUsize)>,
    cancel: Arc<AtomicBool>,
    /// Model revision at start: a result for a different model is not merged.
    rev: u64,
    test: String,
    started: web_time::Instant,
}

pub struct ClashWin {
    pub open: bool,
    pub a: SideUi,
    pub b: SideUi,
    /// Tolerance in mm: clearance (clearance mode) or penetration depth (hard mode).
    pub tol: String,
    pub clearance: bool,
    /// Pairs of the displayed check (`test`).
    pub items: Vec<ClashItem>,
    /// Key of the displayed check; `None` means the last one run among the saved ones.
    pub test: Option<String>,
    levels: Vec<(i64, String)>,
    rev: Option<(u64, u64)>,
    pub filter: Option<ClashStatus>,
    pub sel: Option<Pair>,
    status: ClashStatus,
    comment: String,
    edited: Option<Pair>,
    pub gap: String,
    /// Units the `tol` and `gap` texts are written in; they are re-written on a change.
    buf_units: Option<Units>,
    /// Canonical tolerance and gap in metres; the texts are views of these.
    pub tol_m: f64,
    pub gap_m: f64,
    /// Texts as last absorbed into `tol_m` / `gap_m`; a difference means the user typed.
    tol_seen: String,
    gap_seen: String,
    /// Chosen quick fix: 0 up, 1 down, 2 sleeve.
    fix: usize,
    job: Option<Job>,
    pub msg: String,
    /// 3D markers, scene rebuild and camera framing.
    pub marks: Vec<SceneMesh>,
    pub dirty3d: bool,
    pub fit: Option<Aabb>,
    pub iso: bool,
}

impl Default for ClashWin {
    fn default() -> Self {
        ClashWin {
            open: false,
            a: SideUi::new(true),
            b: SideUi::new(false),
            tol: "0".into(),
            clearance: false,
            items: vec![],
            test: None,
            levels: vec![],
            rev: None,
            filter: None,
            sel: None,
            status: ClashStatus::New,
            comment: String::new(),
            edited: None,
            gap: "50".into(),
            buf_units: None,
            tol_m: 0.0,
            gap_m: 0.05,
            tol_seen: String::new(),
            gap_seen: String::new(),
            fix: 0,
            job: None,
            msg: String::new(),
            marks: vec![],
            dirty3d: false,
            fit: None,
            iso: false,
        }
    }
}

impl ClashWin {
    /// The running check for the status bar; its window draws the details.
    pub fn task_view(&self) -> Option<crate::jobs::JobView> {
        let j = self.job.as_ref()?;
        let (d, n) = (j.done.0.load(Relaxed) as u64, j.done.1.load(Relaxed) as u64);
        Some(crate::jobs::JobView {
            title: "clash.title",
            stage: "clash.running",
            stages: &["clash.running"],
            fraction: if n > 0 { d as f32 / n as f32 } else { 0.0 },
            indeterminate: n == 0,
            elapsed: j.started.elapsed().as_secs_f64(),
            counts: (d, n),
            cancelling: j.cancel.load(Relaxed),
            cancellable: true,
            ..Default::default()
        })
    }

    /// Stops the running check; its result is dropped when it arrives.
    pub fn cancel_job(&mut self) {
        if let Some(j) = &self.job {
            j.cancel.store(true, Relaxed);
        }
    }

    /// Takes a text the user edited (it differs from the last absorbed one) into the metre
    /// truth; a text that does not parse leaves the truth as it was.
    fn absorb(&mut self, u: Units, mark: units::DecimalMark) {
        if self.tol != self.tol_seen {
            if let Some(v) = parse_len0(&self.tol, u, mark) {
                self.tol_m = v;
            }
            self.tol_seen = self.tol.clone();
        }
        if self.gap != self.gap_seen {
            if let Some(v) = parse_len0(&self.gap, u, mark) {
                self.gap_m = v;
            }
            self.gap_seen = self.gap.clone();
        }
    }

    /// Re-renders the texts from `tol_m` / `gap_m` in `u`; the metres are never re-read from
    /// text on a switch, so switching back and forth cannot drift. First call sets the texts.
    pub fn sync_units(&mut self, u: Units) {
        self.sync_units_mark(u, units::ui_decimal_mark());
    }

    fn sync_units_mark(&mut self, u: Units, mark: units::DecimalMark) {
        if let Some(old) = self.buf_units {
            if old == u {
                return;
            }
            self.absorb(old, mark);
        }
        self.tol = units::edit_length_mark(self.tol_m, u, mark);
        self.gap = units::edit_length_mark(self.gap_m, u, mark);
        self.tol_seen = self.tol.clone();
        self.gap_seen = self.gap.clone();
        self.buf_units = Some(u);
    }

    /// Picks up typing before the fields are validated or used.
    pub fn absorb_edits(&mut self, u: Units) {
        self.absorb(u, units::ui_decimal_mark());
    }

    /// Markers are added to the scene after the visibility filter.
    pub fn decorate(&self, sc: Scene) -> Scene {
        if self.marks.is_empty() {
            return sc;
        }
        let mut meshes = sc.meshes;
        meshes.extend(self.marks.iter().cloned().map(std::sync::Arc::new));
        Scene::shared(meshes).with_instanced(sc.instanced)
    }
    pub fn highlight(&self) -> Option<(u32, u32)> {
        (!self.marks.is_empty()).then_some((MARK, MARK + 3))
    }
}

enum Act {
    Run,
    Cancel,
    Select(Pair),
    Save(Pair, ClashStatus, String),
    Show(Pair),
    Issue(Pair),
    Fix(Pair, Fix),
    Export(bool),
    ResetIso,
    /// Drop the status filter.
    ShowAll,
}

/// Why the list shows no rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Empty {
    /// No check has been run for this project.
    Never,
    /// A check ran and found nothing.
    NoClashes,
    /// There are pairs, the status filter hides all of them.
    Hidden,
}

/// `total` pairs of the check, `shown` of them pass the filter, `ran` when a check exists.
pub(crate) fn empty_kind(total: usize, shown: usize, ran: bool) -> Option<Empty> {
    if shown > 0 {
        None
    } else if total > 0 {
        Some(Empty::Hidden)
    } else if ran {
        Some(Empty::NoClashes)
    } else {
        Some(Empty::Never)
    }
}

#[derive(Clone, Copy)]
enum Fix {
    Up,
    Down,
    Sleeve,
}

fn level_label(w: &ClashWin, l: Option<i64>) -> String {
    l.and_then(|l| w.levels.iter().find(|x| x.0 == l))
        .map_or_else(|| "—".into(), |x| x.1.clone())
}

/// Length in metres from field text in project units; zero is allowed (the shared parser
/// refuses it), negative values and junk are not.
fn parse_len0(s: &str, u: Units, mark: units::DecimalMark) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let sc = crate::expr_field::Scope::none();
    if let Ok(v) = units::parse_edit_length_lang(t, u, &sc, mark) {
        return Some(v);
    }
    // "0", "0 m", "0 mm", "0\"" in any unit: drop the trailing unit, read the number by locale
    let n = t.trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace() || matches!(c, '\'' | '"' | '.'));
    if !n.ends_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    units::parse_locale_number(n, mark)
        .ok()
        .filter(|v| v.abs() < 1e-12)
        .map(|_| 0.0)
}

/// Tolerance text in project units -> metres, or the i18n key of the error.
pub fn parse_tol(text: &str, u: Units) -> Result<f64, &'static str> {
    parse_tol_mark(text, u, units::ui_decimal_mark())
}

/// [`parse_tol`] read in the locale of `mark`.
pub fn parse_tol_mark(text: &str, u: Units, mark: units::DecimalMark) -> Result<f64, &'static str> {
    parse_len0(text, u, mark).ok_or("clash_list.err.tol")
}

/// Gap text in project units -> metres, or the i18n key of the error.
pub fn parse_gap(text: &str, u: Units) -> Result<f64, &'static str> {
    parse_gap_mark(text, u, units::ui_decimal_mark())
}

/// [`parse_gap`] read in the locale of `mark`.
pub fn parse_gap_mark(text: &str, u: Units, mark: units::DecimalMark) -> Result<f64, &'static str> {
    parse_len0(text, u, mark).ok_or("clash_list.err.gap")
}

/// A wall or slab in the pair can carry a sleeve.
pub fn pair_has_host(p: &Pair) -> bool {
    [&p.0, &p.1].iter().any(|k| k.0 == "wall" || k.0 == "slab")
}

/// i18n key of the reason a quick fix cannot run (index as in the fix segment), if any.
pub fn fix_block(host: bool, fix: usize) -> Option<&'static str> {
    (fix == 2 && !host).then_some("clash_list.sleeve_no_host")
}

/// Table cells of one pair: status key, element pair, level, volume with its unit.
pub fn clash_cells(it: &ClashItem, level: &str, u: Units, ru: bool) -> [String; 4] {
    [
        it.status.name().to_string(),
        format!("{} #{} × {} #{}", it.a.0, it.a.1, it.b.0, it.b.1),
        level.to_string(),
        crate::unit_text::vol(it.volume, ru, u),
    ]
}

/// Visible list rows (height = row * N from the density token).
const LIST_ROWS: f32 = 9.0;

fn status_color(s: ClashStatus, dark: bool) -> Color32 {
    let c = ddc_ui::theme::sem(dark);
    match s {
        ClashStatus::New => c.error,
        ClashStatus::Active => c.warning,
        ClashStatus::Reviewed => c.accent,
        ClashStatus::Resolved => c.success,
        ClashStatus::Approved => c.text_secondary,
    }
}

fn pair_of(it: &ClashItem) -> Pair {
    (it.a.clone(), it.b.clone())
}

pub(crate) fn box_mesh(element: u32, tag: u32, lo: [f32; 3], hi: [f32; 3]) -> SceneMesh {
    let c =
        |x: usize, y: usize, z: usize| [[lo[0], hi[0]][x], [lo[1], hi[1]][y], [lo[2], hi[2]][z]];
    SceneMesh {
        element,
        tag,
        positions: vec![
            c(0, 0, 0),
            c(1, 0, 0),
            c(1, 1, 0),
            c(0, 1, 0),
            c(0, 0, 1),
            c(1, 0, 1),
            c(1, 1, 1),
            c(0, 1, 1),
        ],
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ],
    }
}

impl EditorApp {
    fn t(&self, k: &str) -> String {
        self.sh.cat.t(k)
    }

    fn el_label(&self, k: &ElKey) -> String {
        format!("{} #{}", self.t(&format!("clash.el.{}", k.0)), k.1)
    }

    fn pair_label(&self, p: &Pair) -> String {
        format!("{} × {}", self.el_label(&p.0), self.el_label(&p.1))
    }

    fn selection_keys(&self) -> Vec<ElKey> {
        self.ed
            .selection
            .iter()
            .filter_map(|s| match s {
                Sel::Wall(id) => Some(("wall".to_string(), *id)),
                Sel::Slab(id) => Some(("slab".to_string(), *id)),
                _ => None,
            })
            .collect()
    }

    fn clash_reload(&mut self, w: &mut ClashWin) {
        let key = self.ed.cache_key();
        if w.rev != Some(key) {
            let all = self.ed.model.clash_items().unwrap_or_default();
            if w.test.is_none() {
                w.test = all
                    .iter()
                    .max_by_key(|i| i.last_run)
                    .map(|i| i.test.clone());
            }
            w.items = all
                .into_iter()
                .filter(|i| w.test.as_deref() == Some(i.test.as_str()))
                .collect();
            w.levels = self
                .ed
                .levels()
                .into_iter()
                .map(|(id, name, _)| (id, name))
                .collect();
            w.rev = Some(key);
        }
    }

    fn clash_start(&mut self, w: &mut ClashWin) {
        let sel = self.selection_keys();
        for s in [&w.a, &w.b] {
            // a selection without elements of the set's categories would give an empty side and "resolve" everything
            let hit = sel.iter().any(|k| {
                CLASH_CATS
                    .iter()
                    .position(|c| *c == k.0)
                    .is_some_and(|i| s.cats[i])
            });
            if s.selected && !hit {
                w.msg = self.t("clash.sel_empty");
                return;
            }
        }
        let tol = match parse_tol(&w.tol, units::active_units()) {
            Ok(v) => v,
            Err(k) => {
                w.msg = self.t(k);
                return;
            }
        };
        let params = ClashParams {
            a: w.a.side(&sel),
            b: w.b.side(&sel),
            tolerance: tol,
            clearance: w.clearance,
        };
        let input = match self.ed.model.clash_input(&params) {
            Ok(i) => i,
            Err(e) => {
                w.msg = e.to_string();
                return;
            }
        };
        let test = input.test().to_string();
        let (tx, rx) = mpsc::channel();
        let done = Arc::new((AtomicUsize::new(0), AtomicUsize::new(input.len().max(1))));
        let cancel = Arc::new(AtomicBool::new(false));
        let (d2, c2) = (done.clone(), cancel.clone());
        // pure computation over a copied input; a crash drops the sender, read below as "stopped"
        let _ = crate::jobs::spawn_guarded(
            "clash-run",
            move || {
                let hits = input.run(&|i, n| {
                    d2.0.store(i, Relaxed);
                    d2.1.store(n.max(1), Relaxed);
                    !c2.load(Relaxed)
                });
                let _ = tx.send((hits, c2.load(Relaxed)));
            },
            |_| {},
        );
        w.job = Some(Job {
            rx,
            done,
            cancel,
            rev: self.ed.revision,
            test,
            started: web_time::Instant::now(),
        });
        w.msg.clear();
    }

    fn clash_poll(&mut self, w: &mut ClashWin, ctx: &egui::Context) {
        let Some(j) = &w.job else { return };
        match j.rx.try_recv() {
            Ok((hits, cancelled)) => {
                let Some(j) = w.job.take() else { return };
                if cancelled {
                    return;
                }
                // the model was edited (or replaced) during the run: ids and bounds are stale
                if j.rev != self.ed.revision {
                    w.msg = self.t("clash.stale");
                    return;
                }
                if w.test.as_deref() != Some(j.test.as_str()) {
                    w.test = Some(j.test.clone());
                    w.sel = None;
                }
                match self.ed.model.clash_merge(&j.test, &hits) {
                    Ok(m) => {
                        w.msg = self
                            .t("clash.summary")
                            .replace("{new}", &m.new.to_string())
                            .replace("{still}", &m.still.to_string())
                            .replace("{resolved}", &m.resolved.to_string());
                        if hits.is_empty() {
                            w.msg = format!("{} {}", self.t("clash.none_found"), w.msg);
                        }
                        w.rev = None;
                    }
                    Err(e) => w.msg = e.to_string(),
                }
            }
            Err(mpsc::TryRecvError::Empty) => ctx.request_repaint(),
            Err(mpsc::TryRecvError::Disconnected) => {
                w.job = None;
                w.msg = crate::jobs::internal_error();
            }
        }
    }

    /// Zoom to and highlight a pair in the plan and in 3D, isolate its elements.
    fn clash_show(&mut self, w: &mut ClashWin, p: &Pair) {
        let Some(it) = w.items.iter().find(|i| pair_of(i) == *p).cloned() else {
            return;
        };
        if let Some(l) = it.level {
            self.ed.set_level(l);
        }
        let z0 = self.ed.elevation;
        let bounds = self
            .ed
            .model
            .clash_element_bounds(&[it.a.clone(), it.b.clone()])
            .unwrap_or_default();
        let mut lo = it.lo;
        let mut hi = it.hi;
        let pad = 0.15;
        let mut marks = vec![];
        let mut plan = vec![];
        let tris = if is_mep(&it.a.0) || is_mep(&it.b.0) {
            self.ed.model.mep_meshes(None).unwrap_or_default()
        } else {
            vec![]
        };
        for (i, (k, b)) in [&it.a, &it.b].into_iter().zip(&bounds).enumerate() {
            if let Some(b) = b {
                // a long run is not framed whole in the plan: only the neighbourhood of the intersection
                let (a0, a1) = ([b.min.x, b.min.y], [b.max.x, b.max.y]);
                let near = |v: f64, c: f64| v.clamp(c - 3.0, c + 3.0);
                let c = it.at();
                plan.push((
                    v2(near(a0[0], c[0]), near(a0[1], c[1])),
                    v2(near(a1[0], c[0]), near(a1[1], c[1])),
                    i as u8,
                ));
                for j in 0..3 {
                    lo[j] = lo[j].min([b.min.x, b.min.y, b.min.z][j].max(it.lo[j] - 3.0));
                    hi[j] = hi[j].max([b.max.x, b.max.y, b.max.z][j].min(it.hi[j] + 3.0));
                }
            }
            let own = MARK + i as u32;
            if is_mep(&k.0) {
                for (_, _, m) in tris.iter().filter(|r| r.0 == k.1) {
                    marks.push(SceneMesh {
                        element: own,
                        tag: 6,
                        positions: m
                            .positions
                            .iter()
                            .map(|q| [q.x as f32, q.y as f32, (q.z - z0) as f32])
                            .collect(),
                        triangles: m.triangles.clone(),
                    });
                }
            } else if (k.0 == "column" || k.0 == "beam") && b.is_some() {
                let b = b.unwrap();
                marks.push(box_mesh(
                    own,
                    4,
                    [b.min.x as f32, b.min.y as f32, (b.min.z - z0) as f32],
                    [b.max.x as f32, b.max.y as f32, (b.max.z - z0) as f32],
                ));
            }
        }
        let e = 0.02;
        marks.push(box_mesh(
            MARK + 2,
            5,
            [
                (it.lo[0] - e) as f32,
                (it.lo[1] - e) as f32,
                (it.lo[2] - z0 - e) as f32,
            ],
            [
                (it.hi[0] + e) as f32,
                (it.hi[1] + e) as f32,
                (it.hi[2] - z0 + e) as f32,
            ],
        ));
        plan.push((
            v2(it.lo[0] - e, it.lo[1] - e),
            v2(it.hi[0] + e, it.hi[1] + e),
            2,
        ));
        w.marks = marks;
        w.dirty3d = true;
        w.fit = Some(Aabb {
            min: Vec3::new(
                lo[0] as f32 - pad,
                lo[1] as f32 - pad,
                (lo[2] - z0) as f32 - pad,
            ),
            max: Vec3::new(
                hi[0] as f32 + pad,
                hi[1] as f32 + pad,
                (hi[2] - z0) as f32 + pad,
            ),
        });
        // plan: centre on the intersection, zoom to its neighbourhood
        let c = it.at();
        let span = ((hi[0] - lo[0]).max(hi[1] - lo[1]) + 2.0 * pad as f64).max(1.5);
        self.plan.center = v2(c[0], c[1]);
        self.plan.scale = (320.0 / span).clamp(20.0, 2000.0);
        self.plan.clash_marks = plan;
        // isolation: the pair's walls and floors, everything else hidden
        let sels: HashSet<Sel> = [&it.a, &it.b]
            .into_iter()
            .filter_map(|k| match k.0.as_str() {
                "wall" => Some(Sel::Wall(k.1)),
                "slab" => Some(Sel::Slab(k.1)),
                _ => None,
            })
            .collect();
        self.plan.b.vis.temp = Temp::Isolate(sels);
        w.iso = true;
    }

    fn clash_reset_view(&mut self, w: &mut ClashWin) {
        if w.iso {
            self.plan.b.vis.reset();
            w.iso = false;
        }
        w.marks.clear();
        w.dirty3d = true;
        self.plan.clash_marks.clear();
    }

    fn clash_fix(&mut self, w: &mut ClashWin, p: &Pair, kind: Fix) {
        let Some(it) = w.items.iter().find(|i| pair_of(i) == *p).cloned() else {
            return;
        };
        if let Some(k) = fix_block(pair_has_host(p), usize::from(matches!(kind, Fix::Sleeve)) * 2) {
            w.msg = self.t(k);
            return;
        }
        let gap = match parse_gap(&w.gap, units::active_units()) {
            Ok(v) => v,
            Err(k) => {
                w.msg = self.t(k);
                return;
            }
        };
        let m = &self.ed.model;
        let fix: Result<ClashFix, _> = match kind {
            Fix::Up => m.clash_fix_reroute(&it, true, gap),
            Fix::Down => m.clash_fix_reroute(&it, false, gap),
            Fix::Sleeve => m.clash_fix_sleeve(&it, gap),
        };
        let res = fix.and_then(|f| {
            self.ed
                .run_op(OpFn(Arc::new(move |m| m.apply_clash_fix(&f))))
        });
        match res {
            Ok(()) => {
                w.msg = self.t("clash.fix_done");
                self.sh.toasts.push(Level::Info, w.msg.clone(), None);
            }
            Err(e) => {
                w.msg = format!("{}: {e}", self.t("clash.fix_fail"));
                self.sh.toasts.push(Level::Warning, w.msg.clone(), None);
            }
        }
    }

    /// Report of the pairs passing the status filter, in project units `u` and language `loc`:
    /// volume and X/Y/Z are numeric cells, the unit is named in the header.
    pub(crate) fn clash_report(&self, w: &ClashWin, u: Units, loc: Loc) -> Report {
        use ddc_io::xlsx::TableCell as C;
        let ru = loc.ru;
        let tr = |k: &str| crate::tr_lang(ru, k, &[]);
        let lu = super::drawing_kit::len_unit(u, ru);
        let (vu, vd) = volume_unit(u, ru);
        let with = |k: &str, unit: &str| {
            crate::tr_lang(ru, "clash.col.unit", &[("name", &tr(k)), ("unit", unit)])
        };
        let head = vec![
            tr("clash.col.group"),
            tr("clash.col.status"),
            tr("clash.col.elements"),
            tr("clash.col.level"),
            with("clash.col.volume", &vu),
            with("clash.col.x", &lu.unit),
            with("clash.col.y", &lu.unit),
            with("clash.col.z", &lu.unit),
            tr("clash.col.comment"),
        ];
        let d = lu.decimals;
        let decs = vec![None, None, None, None, Some(vd), Some(d), Some(d), Some(d), None];
        let el = |k: &ElKey| format!("{} #{}", tr(&format!("clash.el.{}", k.0)), k.1);
        let rows = w
            .items
            .iter()
            .filter(|i| w.filter.is_none_or(|f| f == i.status))
            .map(|i| {
                let at = i.at();
                let mut status = tr(&format!("clash.status.{}", i.status.name()));
                if !i.present {
                    status = format!("{status} ({})", tr("clash.gone"));
                }
                let p = pair_of(i);
                vec![
                    C::Text(format!(
                        "{} × {}",
                        tr(&format!("clash.el.{}", i.a.0)),
                        tr(&format!("clash.el.{}", i.b.0))
                    )),
                    C::Text(status),
                    C::Text(format!("{} × {}", el(&p.0), el(&p.1))),
                    C::Text(level_label(w, i.level)),
                    C::Num(units::volume_in_units(i.volume, u)),
                    C::Num(at[0] * lu.factor),
                    C::Num(at[1] * lu.factor),
                    C::Num(at[2] * lu.factor),
                    C::Text(i.comment.clone()),
                ]
            })
            .collect();
        Report { head, decs, rows, total: w.items.len() }
    }

    fn clash_export(&mut self, w: &mut ClashWin, html: bool) {
        let ext = if html { "html" } else { "xlsx" };
        let name = format!("{}.{ext}", self.t("clash.sheet"));
        let Some(path) = crate::file_pick::Dlg::new()
            .add_filter(ext, &[ext])
            .set_file_name(&name)
            .save_file()
        else {
            return;
        };
        let loc = Loc::of(crate::ui_is_ru());
        let r = self.clash_report(w, units::active_units(), loc);
        w.msg = match clash_write(&r, &path, html, loc) {
            Ok(()) => export_msg(loc, r.rows.len(), r.total, w.filter, &path.display().to_string()),
            Err(e) => e,
        };
    }

    pub(super) fn clash_window(&mut self, ctx: &egui::Context) {
        let mut w = std::mem::take(&mut self.clash);
        self.clash_poll(&mut w, ctx);
        self.clash_reload(&mut w);
        let acts = self.clash_ui(ctx, &mut w);
        for a in acts {
            match a {
                Act::Run => self.clash_start(&mut w),
                Act::Cancel => {
                    if let Some(j) = &w.job {
                        j.cancel.store(true, Relaxed);
                    }
                }
                Act::Select(p) => w.sel = Some(p),
                Act::Save(p, s, c) => {
                    let test = w.test.clone().unwrap_or_default();
                    if let Err(e) = self.ed.model.clash_set_status(&test, &p.0, &p.1, s, &c) {
                        w.msg = e.to_string();
                    }
                    w.rev = None;
                }
                Act::Show(p) => self.clash_show(&mut w, &p),
                Act::Issue(p) => {
                    let d = crate::issues_ui::Draft {
                        title: self.pair_label(&p),
                        elements: vec![
                            ddc_model::TopicElem::local(&p.0 .0, p.0 .1),
                            ddc_model::TopicElem::local(&p.1 .0, p.1 .1),
                        ],
                        source: "clash".to_string(),
                        source_key: format!("{}:{}|{}:{}", p.0 .0, p.0 .1, p.1 .0, p.1 .1),
                        priority: Some("high"),
                        ..crate::issues_ui::Draft::default()
                    };
                    crate::issues_ui::start_draft(self, d);
                }
                Act::Fix(p, k) => self.clash_fix(&mut w, &p, k),
                Act::Export(html) => self.clash_export(&mut w, html),
                Act::ResetIso => self.clash_reset_view(&mut w),
                Act::ShowAll => w.filter = None,
            }
        }
        if !w.open {
            // a closed window is not polled: abort the run instead of merging its result later
            if let Some(j) = w.job.take() {
                j.cancel.store(true, Relaxed);
            }
            self.clash_reset_view(&mut w);
        }
        self.clash = w;
    }

    fn clash_ui(&self, ctx: &egui::Context, w: &mut ClashWin) -> Vec<Act> {
        let mut acts = vec![];
        let mut open = w.open;
        let u = units::active_units();
        w.sync_units(u);
        w.absorb_edits(u);
        let th = &self.sh.theme;
        let levels = self.ed.levels();
        let weak = self.sh.theme.c("text.secondary");
        let t = |k: &str| self.t(k);
        let has_items = !w.items.is_empty();
        kit::Dialog::new("clash", t("clash.title"), kit::DialogSize::Wide)
            .open(&mut open)
            .esc_closes(w.job.is_none())
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    for (i, ui) in cols.iter_mut().enumerate() {
                        let (title, s) = if i == 0 {
                            (t("clash.set_a"), &mut w.a)
                        } else {
                            (t("clash.set_b"), &mut w.b)
                        };
                        ui.label(RichText::new(title).strong());
                        // one grid shared by both sets: columns line up row by row. Up to three
                        // columns, as many as the widest name fits whole (larger type folds)
                        let names: Vec<String> = std::iter::once(t("clash_list.all"))
                            .chain(CLASH_CATS.iter().map(|k| t(&format!("clash.cat.{k}"))))
                            .collect();
                        let need = names
                            .iter()
                            .map(|n| widgets::checkbox_width(ui, th, n))
                            .fold(0.0f32, f32::max);
                        let (room, gap) = (ui.available_width(), ui.spacing().item_spacing.x);
                        let ncol = (((room + gap) / (need + gap)).floor() as usize).clamp(1, 3);
                        let colw = ((room - gap * (ncol - 1) as f32) / ncol as f32).max(0.0);
                        egui::Grid::new(("clash-cats", i))
                            .num_columns(ncol)
                            .min_col_width(colw)
                            .show(ui, |ui| {
                                let all = s.cats.iter().all(|c| *c);
                                let any = s.cats.iter().any(|c| *c);
                                let mut v = all;
                                if widgets::checkbox_mixed(
                                    ui,
                                    th,
                                    &mut v,
                                    any && !all,
                                    &names[0],
                                )
                                .changed()
                                {
                                    s.cats.iter_mut().for_each(|c| *c = v);
                                }
                                for (n, (k, on)) in
                                    CLASH_CATS.iter().zip(s.cats.iter_mut()).enumerate()
                                {
                                    widgets::checkbox(ui, th, on, &names[n + 1]);
                                    // "All" fills the first cell: category n is cell n + 1
                                    if (n + 2) % ncol == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                        ui.horizontal(|ui| {
                            ui.label(t("clash.level"));
                            let mut names = vec![t("clash.any_level")];
                            names.extend(levels.iter().map(|l| l.1.clone()));
                            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                            let mut idx = levels
                                .iter()
                                .position(|l| Some(l.0) == s.level)
                                .map_or(0, |p| p + 1);
                            if widgets::dropdown(ui, th, ("clash-level", i), &mut idx, &refs, None)
                                .changed()
                            {
                                s.level = idx.checked_sub(1).and_then(|p| levels.get(p)).map(|l| l.0);
                            }
                        });
                        // a line of its own: beside the level list larger type cut it
                        widgets::checkbox(ui, th, &mut s.selected, &t("clash_list.only_selected"));
                    }
                });
                ui.add_space(th.d().gap);
                let tol_err = parse_tol(&w.tol, u).err();
                ui.horizontal(|ui| {
                    ui.label(t("clash.mode"));
                    let modes = [t("clash.hard"), t("clash.clearance")];
                    let refs: Vec<&str> = modes.iter().map(String::as_str).collect();
                    if let Some(k) = widgets::segmented(ui, th, &refs, usize::from(w.clearance)) {
                        w.clearance = k == 1;
                    }
                    let tol_lbl = t("clash_list.tolerance");
                    let tol_lr = ui.label(tol_lbl.as_str());
                    // the error is written once, on its own line under the row
                    TextField::new(&mut w.tol)
                        .id_salt("clash-tol")
                        .labelled_by(tol_lr.id, &tol_lbl)
                        .unit(units::unit_label(u))
                        .width(th.d().control * 3.0)
                        .right_aligned()
                        .error(tol_err.is_some())
                        .show(ui, th);
                    if let Some(j) = &w.job {
                        let (d, n) = (j.done.0.load(Relaxed), j.done.1.load(Relaxed));
                        let text = format!("{} {d}/{n}", t("clash.running"));
                        if kit::progress_row(
                            ui,
                            d as f32 / n.max(1) as f32,
                            &text,
                            t("clash.cancel"),
                        ) {
                            acts.push(Act::Cancel);
                        }
                    } else {
                        let label = if w.items.is_empty() {
                            t("clash.run")
                        } else {
                            t("clash.rerun")
                        };
                        // the tolerance field keeps Enter: a long run starts on a click
                        let r = ui
                            .add_enabled_ui(tol_err.is_none(), |ui| {
                                Button::primary(&label).show(ui, th)
                            })
                            .inner;
                        if r.clicked() {
                            acts.push(Act::Run);
                        }
                        if let Some(k) = tol_err {
                            r.on_disabled_hover_text(t(k));
                        }
                    }
                });
                if let Some(k) = tol_err {
                    ui.label(RichText::new(t(k)).color(th.sem().error));
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let mut names = vec![t("clash.filter_all")];
                    names.extend(
                        ClashStatus::ALL
                            .iter()
                            .map(|s| t(&format!("clash.status.{}", s.name()))),
                    );
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    let mut idx = w
                        .filter
                        .and_then(|f| ClashStatus::ALL.iter().position(|s| *s == f))
                        .map_or(0, |p| p + 1);
                    // nothing to filter or export before a check has produced pairs
                    let need = t("clash.need_items");
                    let pick = ui
                        .add_enabled_ui(has_items, |ui| {
                            widgets::dropdown(ui, th, "clash-filter", &mut idx, &refs, None)
                        })
                        .inner;
                    if pick.changed() {
                        w.filter = idx.checked_sub(1).map(|p| ClashStatus::ALL[p]);
                    }
                    if !has_items {
                        pick.on_disabled_hover_text(&need);
                    }
                    let more = t("clash_list.export");
                    let ctx = ui.ctx().clone();
                    let menu_btn = ui
                        .add_enabled_ui(has_items, |ui| {
                            widgets::with_icons(&ctx, |icons| {
                                widgets::icon_button(ui, icons, th, "context-menu", false, &more)
                            })
                        })
                        .inner;
                    if !has_items {
                        menu_btn.clone().on_disabled_hover_text(&need);
                    }
                    egui::Popup::menu(&menu_btn).show(|ui| {
                        if widgets::menu_item(
                            ui,
                            th,
                            &t("clash.export_xlsx"),
                            "",
                            MenuTone::Normal,
                            true,
                        )
                        .clicked()
                        {
                            acts.push(Act::Export(false));
                        }
                        if widgets::menu_item(
                            ui,
                            th,
                            &t("clash.export_html"),
                            "",
                            MenuTone::Normal,
                            true,
                        )
                        .clicked()
                        {
                            acts.push(Act::Export(true));
                        }
                    });
                    if w.iso && Button::quiet(&t("clash.reset_iso")).show(ui, th).clicked() {
                        acts.push(Act::ResetIso);
                    }
                });
                if !w.msg.is_empty() {
                    ui.label(RichText::new(&w.msg).color(weak));
                }
                self.clash_table(ui, w, &mut acts);
                self.clash_detail(ui, w, &mut acts);
            });
        w.open = open;
        acts
    }

    fn clash_table(&self, ui: &mut egui::Ui, w: &mut ClashWin, acts: &mut Vec<Act>) {
        let mut groups: Vec<(String, Vec<&ClashItem>)> = vec![];
        for i in w
            .items
            .iter()
            .filter(|i| w.filter.is_none_or(|f| f == i.status))
        {
            let g = format!(
                "{} × {}",
                self.t(&format!("clash.el.{}", i.a.0)),
                self.t(&format!("clash.el.{}", i.b.0))
            );
            match groups.iter_mut().find(|x| x.0 == g) {
                Some(x) => x.1.push(i),
                None => groups.push((g, vec![i])),
            }
        }
        let th = &self.sh.theme;
        let d = th.d();
        let (u, ru) = (units::active_units(), crate::ui_is_ru());
        let titles = [
            self.t("clash.col.status"),
            self.t("clash.col.elements"),
            self.t("clash.col.level"),
            self.t("clash_list.col.volume"),
            self.t("clash.col.comment"),
        ];
        let cols = [
            Column::text(&titles[0]).fixed(d.row * 3.5).sortable(false),
            Column::text(&titles[1]).fill(2.0).sortable(false),
            Column::text(&titles[2]).fixed(d.row * 3.0).sortable(false),
            Column::number(&titles[3], d.row * 4.0).sortable(false),
            Column::text(&titles[4]).fill(1.5).sortable(false),
        ];
        let dark = ui.visuals().dark_mode;
        egui::ScrollArea::vertical()
            .id_salt("clash-list")
            .max_height(d.row * LIST_ROWS)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                match empty_kind(w.items.len(), groups.len(), w.test.is_some()) {
                    None => {}
                    Some(Empty::Never) => kit::empty_note(ui, self.t("clash.empty"), None),
                    Some(Empty::NoClashes) => kit::empty_note(
                        ui,
                        self.t("clash.none_found"),
                        Some(&self.t("clash.none_hint")),
                    ),
                    Some(Empty::Hidden) => {
                        let hint = self
                            .t("clash.hidden_hint")
                            .replace("{n}", &w.items.len().to_string());
                        let all = self.t("clash.show_all");
                        if widgets::empty_state(
                            ui,
                            th,
                            None,
                            &self.t("clash.hidden"),
                            &hint,
                            Some(&all),
                        ) {
                            acts.push(Act::ShowAll);
                        }
                    }
                }
                for (gi, (g, rows)) in groups.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(g).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(rows.len().to_string()).color(th.sem().text_secondary));
                        });
                    });
                    let out = Table::new(("clash-group", gi), &cols, rows.len()).show(
                        ui,
                        th,
                        &mut None,
                        |r| {
                            if w.sel.as_ref() == Some(&pair_of(rows[r])) {
                                RowState::Selected
                            } else {
                                RowState::Normal
                            }
                        },
                        |r, c| {
                            let it = rows[r];
                            let cells = clash_cells(it, &level_label(w, it.level), u, ru);
                            match c {
                                0 => Cell::text(format!(
                                    "● {}",
                                    self.t(&format!("clash.status.{}", cells[0]))
                                ))
                                .with_tone(status_color(it.status, dark)),
                                1 => Cell::text(self.pair_label(&pair_of(it))),
                                2 => Cell::text(cells[2].clone()),
                                3 => {
                                    let (v, unit) = kit::split_unit(&cells[3]);
                                    Cell::num(v, unit)
                                }
                                _ if !it.present => {
                                    Cell::muted(format!("{} ({})", it.comment, self.t("clash.gone")))
                                }
                                _ => Cell::muted(it.comment.clone()),
                            }
                        },
                    );
                    if let Some(r) = out.clicked {
                        let p = pair_of(rows[r]);
                        acts.push(Act::Select(p.clone()));
                        acts.push(Act::Show(p));
                    }
                }
            });
    }

    fn clash_detail(&self, ui: &mut egui::Ui, w: &mut ClashWin, acts: &mut Vec<Act>) {
        let Some(p) = w.sel.clone() else { return };
        let Some(it) = w.items.iter().find(|i| pair_of(i) == p) else {
            return;
        };
        if w.edited.as_ref() != Some(&p) {
            w.edited = Some(p.clone());
            w.status = it.status;
            w.comment = it.comment.clone();
        }
        let has_mep = is_mep(&p.0 .0) || is_mep(&p.1 .0);
        let host = pair_has_host(&p);
        ui.separator();
        ui.label(RichText::new(self.pair_label(&p)).strong());
        let th = &self.sh.theme;
        let u = units::active_units();
        ui.horizontal(|ui| {
            ui.label(self.t("clash.status"));
            let names: Vec<String> = ClashStatus::ALL
                .iter()
                .map(|s| self.t(&format!("clash.status.{}", s.name())))
                .collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let mut idx = ClashStatus::ALL
                .iter()
                .position(|s| *s == w.status)
                .unwrap_or(0);
            if widgets::dropdown(ui, th, "clash-status", &mut idx, &refs, None).changed() {
                w.status = ClashStatus::ALL[idx];
            }
        });
        let cmt_lbl = self.t("clash.comment");
        let cmt_lr = ui.label(cmt_lbl.as_str());
        TextField::new(&mut w.comment)
            .id_salt("clash-comment")
            .labelled_by(cmt_lr.id, &cmt_lbl)
            .width(ui.available_width())
            .show(ui, th);
        ui.horizontal(|ui| {
            if Button::primary(&self.t("clash.save")).show(ui, th).clicked() {
                acts.push(Act::Save(p.clone(), w.status, w.comment.clone()));
            }
            if Button::secondary(&self.t("clash.show")).show(ui, th).clicked() {
                acts.push(Act::Show(p.clone()));
            }
            if Button::secondary(&self.t("issues.issue.from_finding")).show(ui, th).clicked() {
                acts.push(Act::Issue(p.clone()));
            }
        });
        if has_mep {
            ui.add_space(th.d().gap);
            let gap_err = parse_gap(&w.gap, u).err();
            ui.horizontal(|ui| {
                ui.label(self.t("clash.fixes"));
                let items = [
                    self.t("clash_list.fix.up"),
                    self.t("clash_list.fix.down"),
                    self.t("clash_list.fix.sleeve"),
                ];
                let refs: Vec<&str> = items.iter().map(String::as_str).collect();
                if let Some(k) = widgets::segmented_enabled(ui, th, &refs, w.fix, &[true, true, host]) {
                    w.fix = k;
                }
                let block = fix_block(host, w.fix);
                if !host {
                    ui.label(RichText::new(self.t("clash_list.sleeve_no_host")).weak());
                }
                let gap_lbl = self.t("clash_list.gap");
                let gap_lr = ui.label(gap_lbl.as_str());
                let err_text = gap_err.map(|k| self.t(k)).unwrap_or_default();
                TextField::new(&mut w.gap)
                    .id_salt("clash-gap")
                    .labelled_by(gap_lr.id, &gap_lbl)
                    .unit(units::unit_label(u))
                    .width(th.d().control * 3.0)
                    .right_aligned()
                    .error(gap_err.is_some())
                    .error_text(&err_text)
                    .show(ui, th);
                let r = ui
                    .add_enabled_ui(gap_err.is_none() && block.is_none(), |ui| {
                        Button::secondary(&self.t("clash_list.apply_fix")).show(ui, th)
                    })
                    .inner;
                if r.clicked() {
                    let kind = match w.fix {
                        0 => Fix::Up,
                        1 => Fix::Down,
                        _ => Fix::Sleeve,
                    };
                    acts.push(Act::Fix(p.clone(), kind));
                }
                if let Some(k) = gap_err.or(block) {
                    r.on_disabled_hover_text(self.t(k));
                }
            });
        }
    }
}

/// One frame of the open window for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context) {
    app.clash_window(ctx);
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Clash report: headers with units, decimals of the numeric columns (`None` for text),
/// rows passing the filter and the number of pairs of the whole check.
pub(crate) struct Report {
    pub head: Vec<String>,
    pub decs: Vec<Option<usize>>,
    pub rows: Vec<Vec<ddc_io::xlsx::TableCell>>,
    pub total: usize,
}

/// Clash volumes are small: six places keep 1 cm³ in m³.
const VOLUME_PLACES: usize = 6;

/// Volume unit name and decimals of the report in project units.
fn volume_unit(u: Units, ru: bool) -> (String, usize) {
    let key = match u {
        Units::Metric => "unit_fmt.unit.m3",
        Units::Imperial => "clash.unit.ft3",
    };
    (crate::tr_lang(ru, key, &[]), VOLUME_PLACES)
}

/// Spreadsheet number format with `d` decimals ("0", "0.000").
fn num_format(d: usize) -> String {
    if d == 0 {
        "0".into()
    } else {
        format!("0.{}", "0".repeat(d))
    }
}

/// Write the report as a workbook (numbers stay numbers) or as an HTML page.
pub(crate) fn clash_write(r: &Report, path: &PathBuf, html: bool, loc: Loc) -> Result<(), String> {
    let tr = |k: &str| crate::tr_lang(loc.ru, k, &[]);
    if !html {
        let fmts: Vec<String> =
            r.decs.iter().map(|d| d.map(num_format).unwrap_or_default()).collect();
        let fmts: Vec<&str> = fmts.iter().map(String::as_str).collect();
        return ddc_io::xlsx::export_table(path, &tr("clash.sheet"), &r.head, &fmts, &r.rows, None);
    }
    let page = clash_html(&tr("clash.report.title"), &r.head, &r.decs, &r.rows, loc.ru);
    ddc_io::fsutil::replace_durably_bytes(path, page.as_bytes())
}

/// Status line after an export: pairs written of the check, and the filter that left out the rest.
pub(crate) fn export_msg(
    loc: Loc,
    shown: usize,
    total: usize,
    filter: Option<ClashStatus>,
    path: &str,
) -> String {
    let (n, m) = (shown.to_string(), total.to_string());
    match filter {
        Some(f) => {
            let st = crate::tr_lang(loc.ru, &format!("clash.status.{}", f.name()), &[]);
            crate::tr_lang(
                loc.ru,
                "clash.exported_f",
                &[("n", &n), ("m", &m), ("filter", &st), ("path", path)],
            )
        }
        None => crate::tr_lang(loc.ru, "clash.exported_n", &[("n", &n), ("m", &m), ("path", path)]),
    }
}

/// Self-contained report page (light and dark scheme). Numbers are shown as on screen in
/// language `ru` with `decs[j]` places (raw value when `None`), right-aligned.
pub fn clash_html(
    title: &str,
    head: &[String],
    decs: &[Option<usize>],
    rows: &[Vec<ddc_io::xlsx::TableCell>],
    ru: bool,
) -> String {
    use ddc_io::xlsx::TableCell as C;
    let num = |j: usize, x: f64| match decs.get(j).copied().flatten() {
        Some(d) => units::fmt_num_lang(x, d, crate::units::lang_of(ru)),
        None => format!("{x}"),
    };
    let mut s = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{t}</title><style>\
         :root{{--bg:#fff;--fg:#1d2733;--line:#d5dbe2;--head:#eef2f6}}\
         @media(prefers-color-scheme:dark){{:root{{--bg:#161b22;--fg:#dde3ea;--line:#2f3945;--head:#1f2731}}}}\
         body{{background:var(--bg);color:var(--fg);font:14px system-ui,sans-serif;margin:24px}}\
         table{{border-collapse:collapse;width:100%}}\
         th,td{{border:1px solid var(--line);padding:5px 8px;text-align:left}}\
         th{{background:var(--head)}}td.n{{text-align:right}}</style></head>\
         <body><h1>{t}</h1><table><tr>",
        t = esc(title)
    );
    for h in head {
        s.push_str(&format!("<th>{}</th>", esc(h)));
    }
    s.push_str("</tr>");
    for r in rows {
        s.push_str("<tr>");
        for (j, c) in r.iter().enumerate() {
            match c {
                C::Num(x) | C::Subtotal(_, x) => {
                    s.push_str(&format!("<td class=\"n\">{}</td>", num(j, *x)));
                }
                C::Text(t) => s.push_str(&format!("<td>{}</td>", esc(t))),
                C::CountLabel { pre, post, n } => {
                    s.push_str(&format!("<td>{}</td>", esc(&format!("{pre}{n}{post}"))));
                }
            }
        }
        s.push_str("</tr>");
    }
    s.push_str("</table></body></html>");
    s
}

/// Snapshots: scene with a pipe, a wall and a floor, check run, first pair selected.
pub fn demo(app: &mut EditorApp) -> Result<(), String> {
    use ddc_model::{
        LayerSpec, MepDomain, MepSection, MepShape, PipeMaterial, RunSpec, SlabSpec, SystemKind,
        WallSpec,
    };
    let e = |x: ddc_model::Error| x.to_string();
    let l = app.ed.level;
    let m = &mut app.ed.model;
    let layer = |t: f64| {
        [LayerSpec {
            material: "Concrete".into(),
            thickness: t,
        }]
    };
    let wt = m.add_wall_type("\u{416}/\u{431} 200", &layer(0.2)).map_err(e)?;
    for (a, b) in [
        ((0.0, 0.0), (9.0, 0.0)),
        ((9.0, 0.0), (9.0, 6.0)),
        ((4.0, 0.0), (4.0, 6.0)),
    ] {
        m.add_wall(&WallSpec {
            a,
            b,
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .map_err(e)?;
    }
    let st = m.add_slab_type("\u{41f}\u{43b}\u{438}\u{442}\u{430} 200", &layer(0.2)).map_err(e)?;
    m.add_slab(&SlabSpec {
        level: l,
        slab_type: st,
        offset: 3.0,
        outer: vec![(0.0, 0.0), (9.0, 0.0), (9.0, 6.0), (0.0, 6.0)],
        holes: vec![],
    })
    .map_err(e)?;
    let sys = m
        .add_mep_system("\u{41e}\u{442}\u{43e}\u{43f}\u{43b}\u{435}\u{43d}\u{438}\u{435} \u{422}1", SystemKind::Heating)
        .map_err(e)?;
    let duct = m
        .add_mep_system("\u{41f}\u{440}\u{438}\u{442}\u{43e}\u{43a} \u{41f}1", SystemKind::SupplyAir)
        .map_err(e)?;
    m.add_mep_run(
        &RunSpec::pipe(
            sys,
            l,
            PipeMaterial::Steel,
            80,
            vec![(0.5, 2.0, 2.2), (8.5, 2.0, 2.2)],
        )
        .map_err(e)?,
    )
    .map_err(e)?;
    m.add_mep_run(
        &RunSpec::pipe(
            sys,
            l,
            PipeMaterial::Steel,
            50,
            vec![(6.5, 0.5, 1.0), (6.5, 5.5, 1.0)],
        )
        .map_err(e)?,
    )
    .map_err(e)?;
    m.add_mep_run(&RunSpec::new(
        duct,
        l,
        MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.25 }),
        vec![(1.0, 4.5, 2.5), (8.0, 4.5, 2.5)],
    ))
    .map_err(e)?;
    app.ed.reload().map_err(e)?;
    let mut w = std::mem::take(&mut app.clash);
    w.open = true;
    let (a, b) = (w.a.side(&[]), w.b.side(&[]));
    let p = ClashParams {
        a,
        b,
        ..Default::default()
    };
    let test = p.test_key();
    let hits = app.ed.model.clash_find(&p).map_err(e)?;
    app.ed.model.clash_merge(&test, &hits).map_err(e)?;
    let items = app.ed.model.clash_items().map_err(e)?;
    // different statuses for the snapshot
    if let Some(x) = items.get(1) {
        let _ = app.ed.model.clash_set_status(
            &test,
            &x.a,
            &x.b,
            ClashStatus::Reviewed,
            "\u{441}\u{43e}\u{433}\u{43b}\u{430}\u{441}\u{43e}\u{432}\u{430}\u{43d}\u{43e} \u{441} \u{41a}\u{416}",
        );
    }
    if let Some(x) = items.get(2) {
        let _ = app
            .ed
            .model
            .clash_set_status(&test, &x.a, &x.b, ClashStatus::Approved, "");
    }
    w.test = Some(test);
    w.rev = None;
    app.clash_reload(&mut w);
    if let Some(first) = w.items.first().map(pair_of) {
        w.sel = Some(first.clone());
        app.clash_show(&mut w, &first);
    }
    app.clash = w;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    #[test]
    fn quick_fix_is_one_undo_step_and_status_survives_rerun() {
        let mut a = app();
        demo(&mut a).unwrap();
        let mut w = std::mem::take(&mut a.clash);
        let it = w
            .items
            .iter()
            .find(|i| is_mep(&i.a.0) && i.b.0 == "wall")
            .cloned()
            .expect("pipe x wall pair in the demo scene");
        let run = it.a.1;
        let before = a.ed.model.mep_run(run).unwrap().points;
        a.clash_fix(&mut w, &pair_of(&it), Fix::Up);
        assert_ne!(a.ed.model.mep_run(run).unwrap().points, before, "{}", w.msg);
        a.ed.undo();
        assert_eq!(a.ed.model.mep_run(run).unwrap().points, before);
        a.ed.redo();
        assert_ne!(a.ed.model.mep_run(run).unwrap().points, before);
        a.ed.undo();

        // status and comment survive a re-check
        a.ed.model
            .clash_set_status(&it.test, &it.a, &it.b, ClashStatus::Reviewed, "ok")
            .unwrap();
        let p = ClashParams {
            a: w.a.side(&[]),
            b: w.b.side(&[]),
            ..Default::default()
        };
        assert_eq!(p.test_key(), it.test);
        let hits = a.ed.model.clash_find(&p).unwrap();
        a.ed.model.clash_merge(&it.test, &hits).unwrap();
        let now = a.ed.model.clash_items().unwrap();
        let x = now.iter().find(|i| pair_of(i) == pair_of(&it)).unwrap();
        assert_eq!(
            (x.status, x.comment.as_str()),
            (ClashStatus::Reviewed, "ok")
        );
    }

    #[test]
    fn stale_background_result_is_discarded() {
        let mut a = app();
        demo(&mut a).unwrap();
        let mut w = std::mem::take(&mut a.clash);
        a.ed.model.clash_clear().unwrap();
        a.clash_start(&mut w);
        assert!(w.job.is_some(), "{}", w.msg);
        // an edit during the run
        a.ed.revision += 1;
        let ctx = egui::Context::default();
        while w.job.is_some() {
            a.clash_poll(&mut w, &ctx);
            std::thread::yield_now();
        }
        assert!(a.ed.model.clash_items().unwrap().is_empty());
        assert_eq!(w.msg, a.t("clash.stale"));
    }

    #[test]
    fn reload_follows_a_write_that_kept_the_revision() {
        let mut a = app();
        demo(&mut a).unwrap();
        let mut w = std::mem::take(&mut a.clash);
        a.clash_reload(&mut w);
        assert!(!w.items.is_empty());
        a.ed.model.clash_clear().unwrap();
        a.clash_reload(&mut w);
        assert!(w.items.is_empty(), "the list is read again, not kept for the revision");
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn tolerance_parses_in_project_units() {
        assert!(near(parse_tol("50", Units::Metric).unwrap(), 0.05));
        assert!(near(parse_tol("50 mm", Units::Metric).unwrap(), 0.05));
        assert!(near(parse_tol("0.05 m", Units::Metric).unwrap(), 0.05));
        assert!(near(parse_tol("1 ft", Units::Imperial).unwrap(), units::FOOT));
        assert!(near(parse_gap("2\"", Units::Imperial).unwrap(), 2.0 * 0.0254));
        // zero means "touching counts"; the shared length parser refuses it
        assert_eq!(parse_tol("0", Units::Metric), Ok(0.0));
        assert_eq!(parse_tol("0\"", Units::Imperial), Ok(0.0));
        for bad in ["", "  ", "abc", "-5", "1,2,3 q"] {
            assert_eq!(parse_tol(bad, Units::Metric), Err("clash_list.err.tol"), "{bad}");
            assert_eq!(parse_gap(bad, Units::Metric), Err("clash_list.err.gap"), "{bad}");
        }
    }

    #[test]
    fn texts_survive_a_units_switch() {
        let mut w = ClashWin::default();
        w.sync_units(Units::Metric);
        assert_eq!(parse_tol(&w.tol, Units::Metric), Ok(0.0));
        assert!(near(parse_gap(&w.gap, Units::Metric).unwrap(), 0.05));
        w.gap = "100".into();
        w.tol = "25".into();
        w.sync_units(Units::Imperial);
        // feet-inches text is rounded to the fraction setting (1/8" by default): half a step
        let half = 0.0254 / crate::units::frac_denominator() as f64 / 2.0 + 1e-9;
        let gap = parse_gap(&w.gap, Units::Imperial).unwrap();
        assert!((gap - 0.1).abs() <= half, "{}", w.gap);
        let tol = parse_tol(&w.tol, Units::Imperial).unwrap();
        assert!((tol - 0.025).abs() <= half, "{}", w.tol);
        w.sync_units(Units::Metric);
        assert!(near(parse_gap(&w.gap, Units::Metric).unwrap(), 0.1), "{}", w.gap);
        assert!(near(parse_tol(&w.tol, Units::Metric).unwrap(), 0.025), "{}", w.tol);
    }

    #[test]
    fn unit_round_trip_is_exact() {
        let mut w = ClashWin::default();
        w.sync_units(Units::Metric);
        w.tol = "50".into();
        w.gap = "73".into();
        w.sync_units(Units::Imperial); // absorbs the typed texts first
        let (t0, g0) = (w.tol_m, w.gap_m);
        assert!(near(t0, 0.05) && near(g0, 0.073));
        for _ in 0..10 {
            w.sync_units(Units::Metric);
            w.sync_units(Units::Imperial);
        }
        assert_eq!((w.tol_m, w.gap_m), (t0, g0), "metres must not drift");
        w.sync_units(Units::Metric);
        assert_eq!((w.tol.as_str(), w.gap.as_str()), ("50", "73"));
    }

    #[test]
    fn typed_value_is_kept_exactly() {
        let mut w = ClashWin::default();
        w.sync_units(Units::Metric);
        w.tol = "12,5".into();
        w.absorb_edits(Units::Metric);
        w.sync_units(Units::Metric); // same units: no re-render
        assert_eq!(w.tol, "12,5");
        assert!(near(w.tol_m, 0.0125));
        w.tol = "abc".into();
        w.absorb_edits(Units::Metric);
        assert!(near(w.tol_m, 0.0125), "a bad text keeps the last good value");
        assert_eq!(w.tol, "abc");
    }

    #[test]
    fn zero_in_any_unit() {
        use units::DecimalMark::{Comma, Point};
        for (s, u) in [
            ("0", Units::Metric),
            ("0 m", Units::Metric),
            ("0 mm", Units::Metric),
            ("0.0", Units::Metric),
            ("0 ft", Units::Imperial),
            ("0\"", Units::Imperial),
            ("0 in", Units::Imperial),
            ("0 \u{43c}", Units::Metric),
        ] {
            assert_eq!(parse_tol_mark(s, u, Point), Ok(0.0), "{s}");
        }
        assert_eq!(parse_gap_mark("0,0 m", Units::Metric, Comma), Ok(0.0));
        assert!(parse_tol_mark("-0.5 m", Units::Metric, Point).is_err());
        assert!(parse_tol_mark("m", Units::Metric, Point).is_err());
    }

    #[test]
    fn parses_in_both_locales() {
        use units::DecimalMark::{Comma, Point};
        assert!(near(parse_tol_mark("12,5", Units::Metric, Comma).unwrap(), 0.0125));
        assert!(near(parse_tol_mark("12.5", Units::Metric, Point).unwrap(), 0.0125));
        assert!(near(parse_gap_mark("1 250,5", Units::Metric, Comma).unwrap(), 1.2505));
        assert!(near(parse_gap_mark("1,250.5", Units::Metric, Point).unwrap(), 1.2505));
        // an English comma is not a decimal mark
        assert!(parse_tol_mark("1,5", Units::Metric, Point).is_err());
    }

    #[test]
    fn hostless_sleeve_disabled_with_reason() {
        let wall = ("wall".to_string(), 1);
        let pipe = ("pipe".to_string(), 2);
        let duct = ("duct".to_string(), 3);
        assert!(pair_has_host(&(wall.clone(), pipe.clone())));
        assert!(!pair_has_host(&(pipe, duct)));
        assert_eq!(fix_block(false, 2), Some("clash_list.sleeve_no_host"));
        assert_eq!(fix_block(true, 2), None);
        assert_eq!(fix_block(false, 0), None);
        assert_eq!(fix_block(false, 1), None);
    }

    #[test]
    fn cells_show_volume_with_unit() {
        let mut a = app();
        demo(&mut a).unwrap();
        let w = std::mem::take(&mut a.clash);
        let it = w.items.first().expect("demo pair");
        let en = clash_cells(it, "L1", Units::Metric, false);
        assert_eq!(en[0], it.status.name());
        assert_eq!(en[2], "L1");
        assert_eq!(en[3], crate::unit_text::vol(it.volume, false, Units::Metric));
        assert!(en[3].ends_with("m\u{b3}") && !en[3].contains("cm"), "{}", en[3]);
        let ru = clash_cells(it, "L1", Units::Metric, true);
        assert!(ru[3].contains('\u{43c}'), "{}", ru[3]);
        let im = clash_cells(it, "L1", Units::Imperial, false);
        assert!(im[3].contains("ft") && !im[3].contains("cm"), "{}", im[3]);
    }

    #[test]
    fn window_renders_headless_with_valid_and_bad_tolerance() {
        let mut a = app();
        demo(&mut a).unwrap();
        let mut w = std::mem::take(&mut a.clash);
        let it = w
            .items
            .iter()
            .find(|i| is_mep(&i.a.0) && i.b.0 == "wall")
            .cloned()
            .expect("pipe x wall pair in the demo scene");
        w.sel = Some(pair_of(&it));
        w.open = true;
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                a.sh.theme.apply(c);
            });
        }
        for (tol, gap, run_ok) in [("0", "50", true), ("abc", "-1", false)] {
            w.tol = tol.into();
            w.gap = gap.into();
            w.buf_units = Some(units::active_units());
            for _ in 0..3 {
                let mut acts = vec![];
                let _ = ctx.run(Default::default(), |c| acts = a.clash_ui(c, &mut w));
                assert!(acts.iter().all(|x| !matches!(x, Act::Run)));
            }
            assert_eq!(parse_tol(&w.tol, units::active_units()).is_ok(), run_ok);
        }
        assert!(w.open);
    }

    #[test]
    fn empty_kind_table() {
        assert_eq!(empty_kind(0, 0, false), Some(Empty::Never));
        assert_eq!(empty_kind(0, 0, true), Some(Empty::NoClashes));
        assert_eq!(empty_kind(3, 0, true), Some(Empty::Hidden));
        assert_eq!(empty_kind(3, 0, false), Some(Empty::Hidden));
        assert_eq!(empty_kind(3, 2, true), None);
        assert_eq!(empty_kind(3, 3, false), None);
    }

    fn collect_text(s: &egui::Shape, out: &mut Vec<(String, egui::Pos2, egui::Rect)>) {
        match s {
            egui::Shape::Text(t) => out.push((t.galley.text().to_string(), t.pos, t.galley.rect)),
            egui::Shape::Vec(v) => v.iter().for_each(|x| collect_text(x, out)),
            _ => {}
        }
    }

    /// What the last quiet frame drew, and the actions of the whole run.
    struct Shot {
        texts: Vec<String>,
        disabled: usize,
        acts: Vec<Act>,
    }

    fn pass(
        a: &EditorApp,
        ctx: &egui::Context,
        th: &mut ddc_ui::Theme,
        w: &mut ClashWin,
        events: Vec<egui::Event>,
    ) -> (Vec<Act>, egui::FullOutput) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
        let raw = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
        let mut acts = vec![];
        let out = ctx.run(raw, |c| {
            let _ = th.apply(c);
            acts = a.clash_ui(c, w);
        });
        (acts, out)
    }

    /// Draws the window headless with explicit fonts; `click` presses the text of that label.
    fn drive(a: &EditorApp, w: &mut ClashWin, click: Option<&str>) -> Shot {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx.enable_accesskit();
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Dark, ddc_ui::Density::Compact);
        let mut shot = Shot { texts: vec![], disabled: 0, acts: vec![] };
        let mut found = vec![];
        for _ in 0..4 {
            let (acts, out) = pass(a, &ctx, &mut th, w, vec![]);
            shot.acts.extend(acts);
            found.clear();
            for c in &out.shapes {
                collect_text(&c.shape, &mut found);
            }
            shot.disabled = out.platform_output.accesskit_update.as_ref().map_or(0, |u| {
                u.nodes.iter().filter(|(_, n)| n.is_disabled()).count()
            });
        }
        shot.texts = found.iter().map(|f| f.0.clone()).collect();
        if let Some(label) = click {
            let at = found
                .iter()
                .find(|f| f.0 == label)
                .map(|f| f.1 + f.2.center().to_vec2())
                .expect("label on screen");
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for ev in [egui::Event::PointerMoved(at), press(true), press(false)] {
                let (acts, _) = pass(a, &ctx, &mut th, w, vec![ev]);
                shot.acts.extend(acts);
            }
            let (acts, _) = pass(a, &ctx, &mut th, w, vec![]);
            shot.acts.extend(acts);
        }
        shot
    }

    fn win_for(a: &mut EditorApp, state: Option<Empty>) -> ClashWin {
        let mut w = match state {
            Some(Empty::Never) | Some(Empty::NoClashes) => ClashWin::default(),
            _ => {
                demo(a).unwrap();
                std::mem::take(&mut a.clash)
            }
        };
        w.open = true;
        w.sel = None;
        match state {
            Some(Empty::NoClashes) => w.test = Some("t".into()),
            Some(Empty::Hidden) => {
                let absent = ClashStatus::ALL
                    .iter()
                    .copied()
                    .find(|s| w.items.iter().all(|i| i.status != *s))
                    .expect("a status no pair has");
                w.filter = Some(absent);
            }
            _ => {}
        }
        w
    }

    #[test]
    fn each_empty_state_has_its_own_text_en_ru() {
        use ddc_ui::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app();
            a.sh.cat = Catalog::new(lang);
            crate::lang_on_this_thread(lang == Lang::Ru);
            let mut seen: Vec<(Option<Empty>, Shot)> = vec![];
            let mut n = 0;
            for state in [
                Some(Empty::Never),
                Some(Empty::NoClashes),
                Some(Empty::Hidden),
                None,
            ] {
                // a fresh project per state: the demo scene is built once per app
                let mut a = app();
                a.sh.cat = Catalog::new(lang);
                let mut w = win_for(&mut a, state);
                if state == Some(Empty::Hidden) {
                    n = w.items.len();
                }
                assert_eq!(w.items.is_empty(), matches!(state, Some(Empty::Never | Empty::NoClashes)));
                assert_eq!(
                    empty_kind(
                        w.items.len(),
                        w.items.iter().filter(|i| w.filter.is_none_or(|f| f == i.status)).count(),
                        w.test.is_some()
                    ),
                    state
                );
                seen.push((state, drive(&a, &mut w, None)));
            }
            assert!(n > 0);
            let hint = a.t("clash.hidden_hint").replace("{n}", &n.to_string());
            let own = |s: Option<Empty>| -> Vec<String> {
                match s {
                    Some(Empty::Never) => vec![a.t("clash.empty")],
                    Some(Empty::NoClashes) => vec![a.t("clash.none_found"), a.t("clash.none_hint")],
                    Some(Empty::Hidden) => {
                        vec![a.t("clash.hidden"), hint.clone(), a.t("clash.show_all")]
                    }
                    None => vec![],
                }
            };
            let all = [Some(Empty::Never), Some(Empty::NoClashes), Some(Empty::Hidden)];
            for (state, shot) in &seen {
                for other in all {
                    for text in own(other) {
                        let present = shot.texts.iter().any(|t| *t == text);
                        assert_eq!(present, other == *state, "{lang:?} {state:?} vs {other:?}: {text}");
                    }
                }
                assert!(!shot.texts.iter().any(|t| t.contains("{n}")), "{lang:?}");
            }
        }
    }

    #[test]
    fn show_all_action_clears_the_filter_en_ru() {
        use ddc_ui::{Catalog, Lang};
        for lang in [Lang::En, Lang::Ru] {
            let mut a = app();
            a.sh.cat = Catalog::new(lang);
            crate::lang_on_this_thread(lang == Lang::Ru);
            let mut w = win_for(&mut a, Some(Empty::Hidden));
            let label = a.t("clash.show_all");
            let shot = drive(&a, &mut w, Some(&label));
            let n = shot.acts.iter().filter(|x| matches!(x, Act::ShowAll)).count();
            assert_eq!(n, 1, "{lang:?}");
            assert!(w.filter.is_some(), "the window only reports; the app applies the act");
        }
    }

    #[test]
    fn filter_and_export_are_disabled_until_there_are_pairs() {
        let mut a = app();
        let mut never = win_for(&mut a, Some(Empty::Never));
        let mut none = win_for(&mut a, Some(Empty::NoClashes));
        let mut some = win_for(&mut a, None);
        let (d_never, d_none, d_some) = (
            drive(&a, &mut never, None).disabled,
            drive(&a, &mut none, None).disabled,
            drive(&a, &mut some, None).disabled,
        );
        assert!(d_never > d_some, "{d_never} vs {d_some}");
        assert!(d_none > d_some, "{d_none} vs {d_some}");
    }

    #[test]
    fn bad_tolerance_is_written_once() {
        let mut a = app();
        let mut w = win_for(&mut a, Some(Empty::Never));
        w.tol = "abc".into();
        w.buf_units = Some(units::active_units());
        let shot = drive(&a, &mut w, None);
        let msg = a.t(parse_tol(&w.tol, units::active_units()).unwrap_err());
        assert_eq!(shot.texts.iter().filter(|t| **t == msg).count(), 1, "{msg}");
    }

    #[test]
    fn draw_code_uses_kit_widgets_only() {
        let src = include_str!("clash_win.rs");
        let from = src.find("    fn clash_ui(").unwrap();
        let to = src[from..].find("\nfn esc(").unwrap() + from;
        let body = &src[from..to];
        for bad in [
            "egui::ComboBox",
            "ui.checkbox(",
            "ui.button(",
            "selectable_label(",
            "selectable_value(",
            "add_space(4",
            "TextEdit::",
            ".size([",
        ] {
            assert!(!body.contains(bad), "{bad}");
        }
        for (i, _) in body.match_indices("max_height(") {
            let next = body[i + "max_height(".len()..].chars().next().unwrap();
            assert!(!next.is_ascii_digit(), "literal max_height");
        }
    }

    #[test]
    fn html_report_escapes_cells() {
        use ddc_io::xlsx::TableCell as C;
        let h = clash_html(
            "T",
            &["a<b".into()],
            &[],
            &[vec![C::Text("x&y".into()), C::Num(3.0)]],
            false,
        );
        assert!(h.contains("a&lt;b") && h.contains("x&amp;y"));
        assert!(h.contains("<td class=\"n\">3</td>"));
    }

    #[test]
    fn html_report_renders_total_cells() {
        use ddc_io::xlsx::TableCell as C;
        let h = clash_html(
            "T",
            &["a".into()],
            &[],
            &[vec![
                C::Subtotal(109, 7.0),
                C::CountLabel { pre: "<".into(), post: " rows".into(), n: 4 },
            ]],
            false,
        );
        assert!(h.contains("<td class=\"n\">7</td>") && h.contains("<td>&lt;4 rows</td>"));
    }

    fn num_at(r: &Report, row: usize, col: usize) -> f64 {
        match &r.rows[row][col] {
            ddc_io::xlsx::TableCell::Num(x) => *x,
            c => panic!("column {col} is not a number: {c:?}"),
        }
    }

    fn demo_win() -> (EditorApp, ClashWin) {
        let mut a = app();
        demo(&mut a).unwrap();
        let w = std::mem::take(&mut a.clash);
        assert!(!w.items.is_empty());
        (a, w)
    }

    #[test]
    fn imperial_report_has_feet_columns_and_cubic_feet() {
        let (a, w) = demo_win();
        for ru in [false, true] {
            let r = a.clash_report(&w, Units::Imperial, Loc::of(ru));
            let ft = crate::tr_lang(ru, "drawing_kit.unit.ft", &[]);
            let ft3 = crate::tr_lang(ru, "clash.unit.ft3", &[]);
            let vol = crate::tr_lang(ru, "clash.col.volume", &[]);
            assert_eq!(r.head[4], format!("{vol}, {ft3}"));
            for (j, k) in [(5, "x"), (6, "y"), (7, "z")] {
                let name = crate::tr_lang(ru, &format!("clash.col.{k}"), &[]);
                assert_eq!(r.head[j], format!("{name}, {ft}"));
            }
            assert_eq!(r.rows.len(), w.items.len());
            for (n, it) in w.items.iter().enumerate() {
                let at = it.at();
                for k in 0..3 {
                    assert!((num_at(&r, n, 5 + k) - at[k] / units::FOOT).abs() < 1e-9);
                }
                let v = it.volume / units::FOOT.powi(3);
                assert!((num_at(&r, n, 4) - v).abs() < 1e-12);
            }
            assert_eq!(r.decs[5], Some(3));
        }
    }

    #[test]
    fn metric_report_cells_are_numbers_in_both_languages() {
        let (a, w) = demo_win();
        let en = a.clash_report(&w, Units::Metric, Loc::of(false));
        let ru = a.clash_report(&w, Units::Metric, Loc::of(true));
        let mm = crate::tr_lang(true, "unit_fmt.unit.mm", &[]);
        let m3 = crate::tr_lang(true, "unit_fmt.unit.m3", &[]);
        assert!(ru.head[5].ends_with(&format!(", {mm}")), "{}", ru.head[5]);
        assert!(ru.head[4].ends_with(&format!(", {m3}")), "{}", ru.head[4]);
        assert_ne!(en.head[4], ru.head[4]);
        for n in 0..w.items.len() {
            let at = w.items[n].at();
            for k in 0..3 {
                assert_eq!(num_at(&ru, n, 5 + k), num_at(&en, n, 5 + k));
                assert!((num_at(&ru, n, 5 + k) - at[k] * 1000.0).abs() < 1e-9);
            }
            assert_eq!(num_at(&ru, n, 4), w.items[n].volume);
        }
    }

    #[test]
    fn workbook_reads_back_numbers_and_non_ascii_text() {
        use ddc_io::xlsx::{read_sheet_cells, SheetCell};
        let (a, mut w) = demo_win();
        // fixture: Cyrillic comment with the CSV separator, a quote and a formula-like start
        let note = "=\u{43a}\u{43e}\u{43c}; \"\u{416}/\u{431}\" \u{2116}1";
        w.items[0].comment = note.into();
        let cases = [false, true].into_iter().flat_map(|ru| [(ru, Units::Metric), (ru, Units::Imperial)]);
        for (ru, u) in cases {
            let loc = Loc::of(ru);
            let r = a.clash_report(&w, u, loc);
            let tag = format!("ddc_xs59_{ru}_{u:?}_{}.xlsx", std::process::id());
            let p = std::env::temp_dir().join(tag);
            clash_write(&r, &p, false, loc).unwrap();
            let (sheet, grid) = read_sheet_cells(&p, None).unwrap();
            let _ = std::fs::remove_file(&p);
            assert_eq!(sheet, crate::tr_lang(ru, "clash.sheet", &[]));
            let head: Vec<SheetCell> = r.head.iter().map(|h| SheetCell::Text(h.clone())).collect();
            assert_eq!(grid[0][..head.len()], head[..]);
            for n in 0..r.rows.len() {
                for j in 4..8 {
                    match &grid[n + 1][j] {
                        SheetCell::Num(x) => {
                            assert!((x - num_at(&r, n, j)).abs() < 1e-9, "{ru} {u:?} r{n} c{j}")
                        }
                        c => panic!("{ru} {u:?} r{n} c{j}: {c:?}"),
                    }
                }
            }
            assert_eq!(grid[1][8], SheetCell::Text(note.into()));
        }
    }

    #[test]
    fn html_shows_language_numbers_that_read_back() {
        use units::DecimalMark;
        let (a, w) = demo_win();
        for (ru, mark) in [(false, DecimalMark::Point), (true, DecimalMark::Comma)] {
            let r = a.clash_report(&w, Units::Metric, Loc::of(ru));
            let h = clash_html("T", &r.head, &r.decs, &r.rows, ru);
            for n in 0..r.rows.len() {
                for j in 4..8 {
                    let (x, d) = (num_at(&r, n, j), r.decs[j].unwrap());
                    let shown = units::fmt_num_lang(x, d, crate::units::lang_of(ru));
                    assert!(h.contains(&format!("<td class=\"n\">{shown}</td>")), "{shown}");
                    let back = units::parse_locale_number(&shown, mark).unwrap();
                    let tol = 0.5 * 10f64.powi(-(d as i32)) + 1e-9;
                    assert!((back - x).abs() <= tol, "{shown} {x}");
                }
            }
            // coordinates in mm reach thousands: the language grouping is exercised
            let big = |row: &Vec<ddc_io::xlsx::TableCell>| {
                matches!(row[5], ddc_io::xlsx::TableCell::Num(x) if x.abs() >= 1000.0)
            };
            assert!(r.rows.iter().any(big));
        }
    }

    #[test]
    fn filtered_export_says_how_many_and_which_filter() {
        let (a, mut w) = demo_win();
        w.filter = Some(ClashStatus::Reviewed);
        let r = a.clash_report(&w, Units::Metric, Loc::of(false));
        let want = w.items.iter().filter(|i| i.status == ClashStatus::Reviewed).count();
        assert_eq!((r.rows.len(), r.total), (want, w.items.len()));
        assert!(r.rows.len() < r.total);
        for ru in [false, true] {
            let loc = Loc::of(ru);
            let st = crate::tr_lang(ru, "clash.status.reviewed", &[]);
            let m = export_msg(loc, r.rows.len(), r.total, w.filter, "C:/out/r.xlsx");
            assert!(m.contains(&st) && m.contains("C:/out/r.xlsx"), "{m}");
            let (shown, total) = (r.rows.len().to_string(), r.total.to_string());
            assert!(m.contains(&shown) && m.contains(&total), "{m}");
            let all = export_msg(loc, r.total, r.total, None, "C:/out/r.xlsx");
            assert!(!all.contains(&st) && all.contains(&r.total.to_string()), "{all}");
            assert!(!m.contains('{') && !all.contains('{'), "{m} / {all}");
        }
    }
}
