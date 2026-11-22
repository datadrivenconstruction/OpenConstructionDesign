// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Typed solids in the app: creation (profile sketch on the level plane, second profile,
//! revolve axis, path, profile holes; plane is the level's or a face's), selection and plan handles
//! (vertices, move, depth/height/angle; Tab picks the next solid under the cursor), properties,
//! 3D meshes (voids as ghosts) and wall cuts. Each action is one `run_op`.

use std::cell::RefCell;
use std::sync::Arc;

use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;
use crate::status::Severity;
use crate::ui_kit::{self, Tokens};
use crate::unit_text::{area_ui, vol_ui};
use crate::units::{edit_length, parse_edit_length, unit_label, Units};
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::arc::{edge_mid, BulgeRing};
use ddc_geom::gsolid;
use ddc_geom::{v2, v3, V2, V3};
use ddc_model::{Model, SolidForm, SolidFrame, SolidPath, SolidSection, SolidSpec};
use ddc_ui::i18n::{Catalog, Lang};
use egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

/// 3D scene elements of solids: `SOLID_BASE + id`.
pub const SOLID_BASE: u32 = 0x2000_0000;
pub const SOLID_SPAN: u32 = 0x0800_0000;
/// tan(π/8): bulge of a 90° arc.
const QUARTER: f64 = 0.414_213_562_373_095_1;
/// Plan cut plane above the level, m.
const PLAN_CUT: f64 = 1.2;
const BAR_H: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Extrusion,
    Blend,
    Revolve,
    Sweep,
    SweptBlend,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Extrusion, Kind::Blend, Kind::Revolve, Kind::Sweep, Kind::SweptBlend];

    /// Ribbon command of the solid (`void` false) or void form of this kind.
    pub fn command(self, void: bool) -> &'static str {
        match (self, void) {
            (Kind::Extrusion, false) => "arch.solid_extrusion",
            (Kind::Blend, false) => "arch.solid_blend",
            (Kind::Revolve, false) => "arch.solid_revolve",
            (Kind::Sweep, false) => "arch.solid_sweep",
            (Kind::SweptBlend, false) => "arch.solid_swept_blend",
            (Kind::Extrusion, true) => "arch.void_extrusion",
            (Kind::Blend, true) => "arch.void_blend",
            (Kind::Revolve, true) => "arch.void_revolve",
            (Kind::Sweep, true) => "arch.void_sweep",
            (Kind::SweptBlend, true) => "arch.void_swept_blend",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Extrusion => "extrusion",
            Kind::Blend => "blend",
            Kind::Revolve => "revolve",
            Kind::Sweep => "sweep",
            Kind::SweptBlend => "swept_blend",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Profile,
    Top,
    /// Profile holes (Enter with no points moves on).
    Hole,
    Axis,
    Path,
}

impl Stage {
    fn key(self) -> &'static str {
        match self {
            Stage::Profile => "profile",
            Stage::Top => "top",
            Stage::Hole => "hole",
            Stage::Axis => "axis",
            Stage::Path => "path",
        }
    }
}

/// Tool options bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Opts {
    /// Work plane above the level, m.
    pub base: f64,
    pub start: f64,
    pub end: f64,
    pub height: f64,
    pub angle_deg: f64,
    /// Revolve: the sketch "stands up" — the axis becomes vertical at its first point.
    pub upright: bool,
    /// Rise of the path end above its start, m.
    pub rise: f64,
    pub material: String,
    /// After the outline, sketch the holes (extrude, revolve, sweep).
    pub holes: bool,
    /// Face work plane (plan points are carried onto it vertically); None — the level's plane.
    pub plane: Option<SolidFrame>,
}

impl Opts {
    fn new(wall_h: f64) -> Opts {
        Opts {
            base: 0.0,
            start: 0.0,
            end: if wall_h > 0.0 { wall_h } else { 3.0 },
            height: 1.0,
            angle_deg: 360.0,
            upright: true,
            rise: 0.0,
            material: "concrete".into(),
            holes: false,
            plane: None,
        }
    }
}

pub struct Draft {
    pub kind: Kind,
    pub void: bool,
    stage: Stage,
    pub pts: Vec<V2>,
    /// Bulge of edge i → i+1 (for the last point, the closing edge).
    pub bulges: Vec<f64>,
    profile: Option<BulgeRing>,
    holes: Vec<BulgeRing>,
    top: Option<BulgeRing>,
    pub cur: V2,
    pub opts: Opts,
    cat: Catalog,
    level: i64,
}

impl Draft {
    fn hint(&self) -> String {
        self.cat.t(&format!("hint.solid.{}", self.stage.key()))
    }
    fn reset(&mut self) {
        self.stage = Stage::Profile;
        self.pts.clear();
        self.bulges.clear();
        self.profile = None;
        self.holes.clear();
        self.top = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grip {
    Outer(usize),
    Top(usize),
    Path(usize),
    /// Extrusion depth, blend height, revolve angle: travel along plan x.
    Value,
    Move,
}

struct Drag {
    id: i64,
    grip: Grip,
    spec: SolidSpec,
    from: V2,
    cur: V2,
}

struct Item {
    id: i64,
    void: bool,
    cuts: Vec<(Vec<V2>, Vec<Vec<V2>>)>,
    /// Section at the solid's mid-height — for click selection inside (also for voids).
    outline: Vec<(Vec<V2>, Vec<Vec<V2>>)>,
    edges: Vec<(V2, V2)>,
}

#[derive(Default)]
struct Bufs {
    key: (i64, u64),
    name: String,
    material: String,
    base: String,
    a: String,
    b: String,
}

#[derive(Default)]
struct State {
    draft: Option<Draft>,
    sel: Option<i64>,
    drag: Option<Drag>,
    plan: Option<((u64, i64), Vec<Item>)>,
    buf: Bufs,
    /// Last selection click: point, solids under it, current one (Tab — next).
    cycle: Option<(V2, Vec<i64>, usize)>,
    /// Face plane for the next sketch.
    plane: Option<SolidFrame>,
    lang: Option<Lang>,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State::default());
}

/// The points of the solid being drawn (its own chain).
pub fn chain() -> Option<Vec<V2>> {
    with(|s| s.draft.as_ref().map(|d| d.pts.clone()))
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// Selected solid (plan, 3D, properties).
pub fn selected() -> Option<i64> {
    with(|s| s.sel)
}

pub fn select(id: Option<i64>) {
    with(|s| s.sel = id);
}

/// Reset the sketch and drag state (tool change).
pub fn cancel() {
    with(|s| {
        s.draft = None;
        s.drag = None;
        s.cycle = None;
    });
}

/// Command of the sketch in progress (`interact::Scope`); the stale-draft guard of `handle` applies.
pub fn armed_command(ed: &Editor) -> Option<(&'static str, bool)> {
    // its own draft is the arm (any `set_mode` cancels it), not the session it begins
    if ed.model_tool().is_some() || ed.opening_mode {
        return None;
    }
    with(|s| s.draft.as_ref().map(|d| (d.kind, d.void, d.pts.is_empty())))
        .map(|(k, v, first)| (k.command(v), first))
}

/// A body is being dragged (`interact::Scope::Moving`).
pub fn dragging() -> bool {
    with(|s| s.drag.is_some())
}

/// A sketch or solid drag is in progress (Enter/Space belong to the tool).
pub fn busy() -> bool {
    with(|s| s.draft.is_some() || s.drag.is_some())
}

/// Undo during a sketch cancels the sketch (as with walls); true — the undo was absorbed.
pub fn undo_draft(ed: &mut Editor) -> bool {
    let drawing = with(|s| {
        s.drag = None;
        match &mut s.draft {
            Some(d) if !d.pts.is_empty() || d.profile.is_some() => {
                d.reset();
                Some(d.hint())
            }
            _ => None,
        }
    });
    match drawing {
        Some(h) => {
            ed.say(Severity::Info, h);
            true
        }
        None => false,
    }
}

/// The model changed (undo, snapshot replaced): sketch, handles and plan cache are reset, the
/// selection only if the solid still exists.
pub fn on_model_reset(ed: &Editor) {
    let sel = selected().filter(|id| ed.model.solid(*id).is_ok());
    with(|s| {
        if let Some(d) = &mut s.draft {
            if d.level != ed.level {
                s.draft = None;
            } else {
                d.reset();
            }
        }
        s.drag = None;
        s.cycle = None;
        s.plan = None;
        s.sel = sel;
    });
}

/// Work plane for the next sketches (from a 3D face); None — the level plane.
pub fn set_work_plane(f: Option<SolidFrame>) {
    with(|s| {
        s.plane = f;
        if let Some(d) = &mut s.draft {
            d.opts.plane = f;
        }
    });
}

fn lang() -> Lang {
    with(|s| s.lang).unwrap_or(Lang::Ru)
}

// ---------- commands ----------

pub fn arm(ed: &mut Editor, kind: Kind, void: bool, lang: Lang) {
    // arm through the session: the wall stays unarmed, the selection goes
    ed.set_mode(Mode::Select);
    ed.selection.clear();
    crate::interact::sessions::model_tool::stop(ed);
    let d = Draft {
        kind,
        void,
        stage: Stage::Profile,
        pts: vec![],
        bulges: vec![],
        profile: None,
        holes: vec![],
        top: None,
        cur: v2(0.0, 0.0),
        opts: Opts {
            plane: with(|s| s.plane),
            ..Opts::new(ed.height)
        },
        cat: Catalog::new(lang),
        level: ed.level,
    };
    ed.say(Severity::Info, d.hint());
    with(|s| {
        s.draft = Some(d);
        s.sel = None;
        s.drag = None;
        s.cycle = None;
        s.lang = Some(lang);
    });
    crate::interact::Interaction::refresh(ed);
}

macro_rules! commands {
    ($($f:ident => $k:expr, $v:expr, $id:literal;)*) => {
        $(fn $f(app: &mut EditorApp) {
            let lang = app.sh.cat.lang;
            arm(&mut app.ed, $k, $v, lang);
        })*
        const COMMANDS: &[(&str, fn(&mut EditorApp))] = &[$(($id, $f)),*];
    };
}

commands! {
    h_extrusion => Kind::Extrusion, false, "arch.solid_extrusion";
    h_blend => Kind::Blend, false, "arch.solid_blend";
    h_revolve => Kind::Revolve, false, "arch.solid_revolve";
    h_sweep => Kind::Sweep, false, "arch.solid_sweep";
    h_swept_blend => Kind::SweptBlend, false, "arch.solid_swept_blend";
    h_v_extrusion => Kind::Extrusion, true, "arch.void_extrusion";
    h_v_blend => Kind::Blend, true, "arch.void_blend";
    h_v_revolve => Kind::Revolve, true, "arch.void_revolve";
    h_v_sweep => Kind::Sweep, true, "arch.void_sweep";
    h_v_swept_blend => Kind::SweptBlend, true, "arch.void_swept_blend";
}

/// Register commands in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    for (id, h) in COMMANDS {
        app.register_command(id, *h);
    }
}

// ---------- plan events ----------

/// Plan event: the active solid tool or solid handles/selection; false — not ours.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    if let Some(mut d) = with(|s| s.draft.take()) {
        // another tool took over the plan — the sketch is reset
        if ed.model_tool().is_some() || ed.opening_mode {
            return false;
        }
        if step(ed, &mut d, e) {
            with(|s| s.draft = Some(d));
        } else {
            ed.set_mode(Mode::Select);
            ed.status = None;
        }
        return true;
    }
    if ed.tool_armed() || ed.opening_mode {
        return false;
    }
    select_event(ed, e)
}

fn step(ed: &mut Editor, d: &mut Draft, e: Event) -> bool {
    match e {
        Event::Move(p) => track(ed, d, p),
        Event::Click(p) => {
            track(ed, d, p);
            let q = d.cur;
            click(ed, d, q);
        }
        Event::Press(_) | Event::Release(_) | Event::Char(_) => {}
        Event::Key(Key::Tab) => {
            if !ed.tool.snaps.is_empty() {
                ed.tool.snap_idx = (ed.tool.snap_idx + 1) % ed.tool.snaps.len();
            }
            d.cur = ed.wall_snapped();
        }
        Event::Key(Key::Backspace) => {
            d.pts.pop();
            d.bulges.truncate(d.pts.len());
        }
        Event::Key(Key::Escape) => {
            if d.pts.is_empty() && d.stage == Stage::Profile {
                return false;
            }
            d.reset();
            ed.say(Severity::Info, d.hint());
        }
        Event::Key(Key::Enter) | Event::Key(Key::Close) => enter(ed, d),
        Event::Key(Key::Arc) => {
            // arc on the last edge: straight → 90° left → 90° right
            let n = d.pts.len();
            if n >= 2 {
                let b = &mut d.bulges[n - 2];
                *b = if *b == 0.0 {
                    QUARTER
                } else if *b > 0.0 {
                    -QUARTER
                } else {
                    0.0
                };
            }
        }
    }
    true
}

fn track(ed: &mut Editor, d: &mut Draft, p: V2) {
    let z = ed.tool.snap_state.z;
    ed.tool.track_chain(p, crate::wall_tool::Chain::of(&d.pts, z, true));
    d.cur = ed.wall_snapped();
}

fn click(ed: &mut Editor, d: &mut Draft, q: V2) {
    let ring = matches!(d.stage, Stage::Profile | Stage::Top | Stage::Hole);
    if ring && d.pts.len() >= 3 && (q - d.pts[0]).len() <= ed.tool.radius.max(1e-3) {
        return enter(ed, d);
    }
    if d.pts
        .last()
        .is_some_and(|l| (*l - q).len() < crate::wall_tool::MIN_LEN)
    {
        return;
    }
    d.pts.push(q);
    d.bulges.push(0.0);
    if d.stage == Stage::Axis && d.pts.len() == 2 {
        finish(ed, d);
    }
}

/// With profile holes: shapes with a section (blends use rings without holes).
fn takes_holes(k: Kind) -> bool {
    matches!(k, Kind::Extrusion | Kind::Revolve | Kind::Sweep)
}

/// Stage after the outline (and its holes).
fn after_profile(ed: &mut Editor, d: &mut Draft) {
    d.stage = match d.kind {
        Kind::Extrusion => return finish(ed, d),
        Kind::Blend | Kind::SweptBlend => Stage::Top,
        Kind::Revolve => Stage::Axis,
        Kind::Sweep => Stage::Path,
    };
    ed.say(Severity::Info, d.hint());
}

fn enter(ed: &mut Editor, d: &mut Draft) {
    match d.stage {
        Stage::Hole if d.pts.is_empty() => after_profile(ed, d),
        Stage::Profile | Stage::Top | Stage::Hole => {
            if d.pts.len() < 3 {
                ed.say(Severity::Refusal, d.cat.t("tool.err.open_loop"));
                return;
            }
            let ring = BulgeRing {
                pts: std::mem::take(&mut d.pts),
                bulges: std::mem::take(&mut d.bulges),
            }
            .ccw();
            match d.stage {
                Stage::Profile => {
                    d.profile = Some(ring);
                    if d.opts.holes && takes_holes(d.kind) {
                        d.stage = Stage::Hole;
                        ed.say(Severity::Info, d.hint());
                    } else {
                        after_profile(ed, d);
                    }
                }
                Stage::Hole => {
                    d.holes.push(ring);
                    ed.say(Severity::Info, d.hint());
                }
                _ => {
                    d.top = Some(ring);
                    if d.kind == Kind::Blend {
                        return finish(ed, d);
                    }
                    d.stage = Stage::Path;
                    ed.say(Severity::Info, d.hint());
                }
            }
        }
        Stage::Path => {
            if d.pts.len() < 2 {
                ed.say(Severity::Refusal, d.cat.t("tool.err.short"));
                return;
            }
            finish(ed, d);
        }
        Stage::Axis => {}
    }
}

fn section(r: BulgeRing, holes: &[BulgeRing]) -> SolidSection {
    SolidSection {
        outer: r,
        holes: holes.to_vec(),
    }
}

fn shifted(r: &BulgeRing, c: V2) -> BulgeRing {
    BulgeRing {
        pts: r.pts.iter().map(|p| *p - c).collect(),
        bulges: r.bulges.clone(),
    }
}

fn center(r: &BulgeRing) -> V2 {
    let (lo, hi) = r.pts.iter().fold(
        (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN)),
        |(a, b), p| {
            (
                v2(a.x.min(p.x), a.y.min(p.y)),
                v2(b.x.max(p.x), b.y.max(p.y)),
            )
        },
    );
    (lo + hi) * 0.5
}

/// Ring relative to the bounding-box center (section along the path).
fn centered(r: &BulgeRing) -> BulgeRing {
    shifted(r, center(r))
}

/// Plan ring on the face work plane (winding is counter-clockwise again).
fn on_plane(f: &SolidFrame, r: &BulgeRing) -> Option<BulgeRing> {
    let pts = r
        .pts
        .iter()
        .map(|p| crate::solid_scene::onto(f, *p))
        .collect::<Option<Vec<_>>>()?;
    Some(
        BulgeRing {
            pts,
            bulges: r.bulges.clone(),
        }
        .ccw(),
    )
}

fn path_of(d: &Draft) -> SolidPath {
    let n = d.pts.len();
    let total: f64 = (1..n).map(|i| (d.pts[i] - d.pts[i - 1]).len()).sum();
    let mut run = 0.0;
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        if i > 0 {
            run += (d.pts[i] - d.pts[i - 1]).len();
        }
        let z = if total > 0.0 {
            d.opts.rise * run / total
        } else {
            0.0
        };
        let q = match &d.opts.plane {
            Some(f) => crate::solid_scene::onto(f, d.pts[i]).unwrap_or(d.pts[i]),
            None => d.pts[i],
        };
        pts.push(v3(q.x, q.y, z));
    }
    SolidPath {
        pts,
        bulges: d.bulges[..n.saturating_sub(1)].to_vec(),
        smooth: None,
    }
}

/// Solid description from the sketch.
fn build(d: &Draft) -> Result<SolidSpec, String> {
    let o = &d.opts;
    let none = || d.cat.t("tool.err.open_loop");
    let mut prof = d.profile.clone().ok_or_else(none)?;
    let mut holes = d.holes.clone();
    let mut top = d.top.clone();
    // face: the plan sketch is carried onto it (an upright revolve builds its own plane)
    let face = o.plane.filter(|_| !(d.kind == Kind::Revolve && o.upright));
    let lv = match &face {
        Some(f) => {
            let bad = || d.cat.t("solid.err.face");
            prof = on_plane(f, &prof).ok_or_else(bad)?;
            holes = holes
                .iter()
                .map(|h| on_plane(f, h))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(bad)?;
            top = match top {
                Some(t) => Some(on_plane(f, &t).ok_or_else(bad)?),
                None => None,
            };
            *f
        }
        None => SolidFrame::level(o.base),
    };
    let (frame, form) = match d.kind {
        Kind::Extrusion => (
            lv,
            SolidForm::Extrusion {
                profile: section(prof, &holes),
                start: o.start,
                end: o.end,
            },
        ),
        Kind::Blend => (
            lv,
            SolidForm::Blend {
                bottom: prof,
                top: top.ok_or_else(none)?,
                height: o.height,
            },
        ),
        Kind::Revolve => {
            let (a, b) = match d.pts.as_slice() {
                [a, b, ..] => (*a, *b),
                _ => return Err(d.cat.t("tool.err.short")),
            };
            let end = o.angle_deg.to_radians();
            if o.upright {
                let dd = (b - a) * (1.0 / (b - a).len());
                let n = v2(dd.y, -dd.x);
                // (s, t) = (w·n, w·d): det = 1, winding is preserved
                let stand = |r: &BulgeRing| BulgeRing {
                    pts: r
                        .pts
                        .iter()
                        .map(|p| {
                            let w = *p - a;
                            v2(w.dot(n), w.dot(dd))
                        })
                        .collect(),
                    bulges: r.bulges.clone(),
                };
                let ring = stand(&prof);
                let hs: Vec<BulgeRing> = holes.iter().map(stand).collect();
                (
                    SolidFrame::vertical(v3(a.x, a.y, o.base), n),
                    SolidForm::Revolve {
                        profile: section(ring, &hs),
                        axis_o: v2(0.0, 0.0),
                        axis_d: v2(0.0, 1.0),
                        start: 0.0,
                        end,
                    },
                )
            } else {
                let (a, b) = match &face {
                    Some(f) => {
                        let p = |q: V2| crate::solid_scene::onto(f, q).unwrap_or(q);
                        (p(a), p(b))
                    }
                    None => (a, b),
                };
                (
                    lv,
                    SolidForm::Revolve {
                        profile: section(prof, &holes),
                        axis_o: a,
                        axis_d: b - a,
                        start: 0.0,
                        end,
                    },
                )
            }
        }
        Kind::Sweep => {
            let c = center(&prof);
            let hs: Vec<BulgeRing> = holes.iter().map(|h| shifted(h, c)).collect();
            (
                lv,
                SolidForm::Sweep {
                    profile: section(shifted(&prof, c), &hs),
                    path: path_of(d),
                },
            )
        }
        Kind::SweptBlend => (
            lv,
            SolidForm::SweptBlend {
                bottom: centered(&prof),
                top: centered(&top.ok_or_else(none)?),
                path: path_of(d),
            },
        ),
    };
    Ok(SolidSpec {
        level: d.level,
        name: String::new(),
        void: d.void,
        material: o.material.clone(),
        ifc: None,
        frame,
        form,
    })
}

fn finish(ed: &mut Editor, d: &mut Draft) {
    let r = build(d);
    d.reset();
    let spec = match r {
        Ok(s) => s,
        Err(m) => {
            ed.say(Severity::Refusal, m);
            return;
        }
    };
    let op = OpFn(Arc::new(move |m: &mut Model| {
        m.add_solid(&spec).map(|_| ())
    }));
    match ed.run_op(op) {
        Ok(()) => ed.say(
            Severity::Info,
            d.cat.tf(
                "tool.done",
                &[("what", &d.cat.t(&format!("solid.kind.{}", d.kind.key())))],
            ),
        ),
        Err(e) => ed.say(
            Severity::Error,
            format!("{}: {e}", d.cat.t("tool.err.model")),
        ),
    }
}

// ---------- selection and handles ----------

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a + d * t - p).len()
}

fn inside(p: V2, r: &[V2]) -> bool {
    let mut c = false;
    let n = r.len();
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            c = !c;
        }
    }
    c
}

fn build_items(ed: &Editor) -> Vec<Item> {
    let z = ed.elevation + PLAN_CUT;
    let Ok(all) = ed.model.solids(Some(ed.level)) else {
        return vec![];
    };
    all.into_iter()
        .filter_map(|(id, s)| {
            let m = ed.model.solid_mesh(id).ok()?;
            let (z0, z1) = m
                .positions
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| {
                    (a.0.min(p.z), a.1.max(p.z))
                });
            Some(Item {
                id,
                void: s.void,
                cuts: if s.void {
                    vec![]
                } else {
                    gsolid::plan_cut(&m, z)
                },
                outline: gsolid::plan_cut(&m, (z0 + z1) * 0.5),
                edges: gsolid::plan_lines(&m, if s.void { f64::MAX } else { z }),
            })
        })
        .collect()
}

fn with_items<R>(ed: &Editor, f: impl FnOnce(&[Item]) -> R) -> R {
    let key = (ed.revision, ed.level);
    if with(|s| s.plan.as_ref().map_or(true, |c| c.0 != key)) {
        let items = build_items(ed);
        with(|s| s.plan = Some((key, items)));
    }
    ST.with(|s| f(s.borrow().plan.as_ref().map_or(&[][..], |c| &c.1[..])))
}

/// Solids under a plan point, nearest first; the second is a point inside the solid's section (not a void).
fn ranked(ed: &Editor, p: V2) -> (Vec<i64>, bool) {
    let r = ed.tool.radius.max(1e-3);
    with_items(ed, |items| {
        let within = |rs: &[(Vec<V2>, Vec<Vec<V2>>)]| {
            rs.iter()
                .any(|(o, hs)| inside(p, o) && !hs.iter().any(|h| inside(p, h)))
        };
        let mut v: Vec<(i64, f64, bool)> = items
            .iter()
            .filter_map(|it| {
                let d = it
                    .edges
                    .iter()
                    .map(|e| seg_dist(p, e.0, e.1))
                    .fold(f64::MAX, f64::min);
                let solid_in = !it.void && within(&it.cuts);
                let ins = solid_in || within(&it.outline);
                let d = if ins { 0.0 } else { d };
                (d <= r).then_some((it.id, d, solid_in))
            })
            .collect();
        // solids before voids at equal proximity
        v.sort_by(|a, b| a.1.total_cmp(&b.1).then(b.2.cmp(&a.2)));
        let solid_in = v.iter().any(|x| x.2);
        (v.into_iter().map(|x| x.0).collect(), solid_in)
    })
}

/// Solid under a plan point: inside the solid's section it is the solid; near a wall it is the wall (Tab cycles solids).
fn hit(ed: &Editor, p: V2) -> Option<i64> {
    let r = ed.tool.radius.max(1e-3);
    let (c, solid_in) = ranked(ed, p);
    if !solid_in && ed.walls.iter().any(|w| seg_dist(p, w.a, w.b) <= r + 0.15) {
        return None;
    }
    c.first().copied()
}

fn flat(f: &SolidFrame) -> bool {
    f.z.z.abs() > 1.0 - 1e-9
}

/// Solid handles in the plan.
fn grips(s: &SolidSpec) -> Vec<(Grip, V2)> {
    let f = s.frame;
    let plan = |p: V3| {
        let w = f.at(p);
        v2(w.x, w.y)
    };
    let mut out = vec![];
    match &s.form {
        SolidForm::Extrusion { profile, .. } | SolidForm::Revolve { profile, .. } if flat(&f) => {
            for (i, q) in profile.outer.pts.iter().enumerate() {
                out.push((Grip::Outer(i), plan(v3(q.x, q.y, 0.0))));
            }
        }
        SolidForm::Blend {
            bottom,
            top,
            height,
        } if flat(&f) => {
            for (i, q) in bottom.pts.iter().enumerate() {
                out.push((Grip::Outer(i), plan(v3(q.x, q.y, 0.0))));
            }
            for (i, q) in top.pts.iter().enumerate() {
                out.push((Grip::Top(i), plan(v3(q.x, q.y, *height))));
            }
        }
        SolidForm::Sweep { path, .. } | SolidForm::SweptBlend { path, .. } => {
            for (i, q) in path.pts.iter().enumerate() {
                out.push((Grip::Path(i), plan(*q)));
            }
        }
        _ => {}
    }
    let c = if out.is_empty() {
        plan(v3(0.0, 0.0, 0.0))
    } else {
        let s = out.iter().fold(v2(0.0, 0.0), |a, g| a + g.1);
        s * (1.0 / out.len() as f64)
    };
    if flat(&f)
        && matches!(
            s.form,
            SolidForm::Extrusion { .. } | SolidForm::Blend { .. } | SolidForm::Revolve { .. }
        )
    {
        let right = out.iter().map(|g| g.1.x).fold(c.x, f64::max);
        out.push((Grip::Value, v2(right + VALUE_GAP, c.y)));
    }
    out.push((Grip::Move, c));
    out
}

/// Value handle sits to the right of the solid; 1 m travel in x = 1 m of depth/height or 90° of angle.
const VALUE_GAP: f64 = 0.4;

fn grip_at(ed: &Editor, s: &SolidSpec, p: V2) -> Option<Grip> {
    let r = ed.tool.radius.max(1e-3);
    grips(s)
        .into_iter()
        .map(|(g, q)| (g, (q - p).len()))
        .filter(|g| g.1 <= r)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|g| g.0)
}

/// Description after dragging a handle from `from` to `to`.
fn moved(s: &SolidSpec, g: Grip, from: V2, to: V2) -> SolidSpec {
    let mut s = s.clone();
    let f = s.frame;
    let reloc = |p: V3| {
        let w = f.at(p);
        f.local(v3(to.x, to.y, w.z))
    };
    let flat2 = |q: &mut V2, z: f64| {
        let l = reloc(v3(q.x, q.y, z));
        *q = v2(l.x, l.y);
    };
    let dx = to.x - from.x;
    match (g, &mut s.form) {
        (Grip::Move, _) => s.frame.o = s.frame.o + v3(to.x - from.x, to.y - from.y, 0.0),
        (Grip::Value, SolidForm::Extrusion { start, end, .. }) => {
            // depth grows toward the end
            let dir = if *end >= *start { 1.0 } else { -1.0 };
            let ne = *end + dir * dx;
            if (ne - *start) * dir > 1e-3 {
                *end = ne;
            }
        }
        (Grip::Value, SolidForm::Blend { height, .. }) => *height = (*height + dx).max(1e-3),
        (Grip::Value, SolidForm::Revolve { start, end, .. }) => {
            let sweep = (*end - *start + dx * std::f64::consts::FRAC_PI_2)
                .clamp(1e-3, std::f64::consts::TAU);
            *end = *start + sweep;
        }
        (Grip::Outer(i), SolidForm::Extrusion { profile, .. })
        | (Grip::Outer(i), SolidForm::Revolve { profile, .. }) => {
            if let Some(q) = profile.outer.pts.get_mut(i) {
                flat2(q, 0.0);
            }
        }
        (Grip::Outer(i), SolidForm::Blend { bottom, .. }) => {
            if let Some(q) = bottom.pts.get_mut(i) {
                flat2(q, 0.0);
            }
        }
        (Grip::Top(i), SolidForm::Blend { top, height, .. }) => {
            let h = *height;
            if let Some(q) = top.pts.get_mut(i) {
                flat2(q, h);
            }
        }
        (Grip::Path(i), SolidForm::Sweep { path, .. })
        | (Grip::Path(i), SolidForm::SweptBlend { path, .. }) => {
            let n = path.pts.len();
            let closed = n > 3 && (path.pts[n - 1] - path.pts[0]).len() < 1e-6;
            if path.smooth.is_some() {
                // the smooth path keeps its seam and derived helix ends itself
                if let Some(q) = path.pts.get(i).copied() {
                    path.move_vertex(i, reloc(q));
                }
            } else {
                if let Some(q) = path.pts.get_mut(i) {
                    *q = reloc(*q);
                }
                // a closed loop stores its seam point twice: move both so the loop stays closed
                if closed && (i == 0 || i == n - 1) {
                    let j = n - 1 - i;
                    path.pts[j] = path.pts[i];
                }
            }
        }
        _ => {}
    }
    s
}

fn edit(ed: &mut Editor, id: i64, s: SolidSpec) {
    let op = OpFn(Arc::new(move |m: &mut Model| m.update_solid(id, &s)));
    if let Err(e) = ed.run_op(op) {
        ed.say(Severity::Error, crate::constraints_ui::report(&e));
    }
}

fn select_event(ed: &mut Editor, e: Event) -> bool {
    match e {
        Event::Press(p) => {
            let Some(id) = selected().filter(|_| ed.selection.is_empty()) else {
                return false;
            };
            let Ok(spec) = ed.model.solid(id) else {
                select(None);
                return false;
            };
            let Some(grip) = grip_at(ed, &spec, p) else {
                return false;
            };
            with(|s| {
                s.drag = Some(Drag {
                    id,
                    grip,
                    spec,
                    from: p,
                    cur: p,
                })
            });
            true
        }
        Event::Move(p) => with(|s| match &mut s.drag {
            Some(d) => {
                d.cur = p;
                true
            }
            None => false,
        }),
        Event::Release(p) => {
            let Some(d) = with(|s| s.drag.take()) else {
                return false;
            };
            if (p - d.from).len() > 1e-9 {
                let next = moved(&d.spec, d.grip, d.from, p);
                edit(ed, d.id, next);
            }
            true
        }
        Event::Click(p) => {
            let h = hit(ed, p);
            let (c, _) = ranked(ed, p);
            let at = h.and_then(|id| c.iter().position(|x| *x == id));
            with(|s| s.cycle = (!c.is_empty()).then(|| (p, c, at.unwrap_or(usize::MAX))));
            select(h);
            if h.is_some() {
                ed.selection.clear();
            }
            h.is_some()
        }
        Event::Key(Key::Tab) => {
            let next = with(|s| {
                let (_, c, k) = s.cycle.as_mut()?;
                *k = if *k == usize::MAX {
                    0
                } else {
                    (*k + 1) % c.len()
                };
                Some(c[*k])
            });
            match next {
                Some(id) => {
                    select(Some(id));
                    ed.selection.clear();
                    true
                }
                None => false,
            }
        }
        Event::Key(Key::Escape) => {
            if with(|s| s.drag.take()).is_some() {
                return true;
            }
            if selected().is_some() && ed.selection.is_empty() {
                select(None);
                with(|s| s.cycle = None);
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Delete the selected solid (Delete); false — no solid selected.
pub fn delete_selected(ed: &mut Editor) -> bool {
    if !ed.selection.is_empty() {
        return false;
    }
    let Some(id) = selected() else {
        return false;
    };
    match ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.delete_solid(id)))) {
        Ok(()) => select(None),
        Err(e) => ed.say(Severity::Error, crate::constraints_ui::report(&e)),
    }
    true
}

// ---------- 3D ----------

/// Selection from 3D: true — the scene element is a solid.
pub fn select_scene(ed: &mut Editor, e: Option<u32>) -> bool {
    match e.filter(|e| (SOLID_BASE..SOLID_BASE + SOLID_SPAN).contains(e)) {
        Some(e) => {
            select(Some((e - SOLID_BASE) as i64));
            ed.selection.clear();
            true
        }
        None => {
            select(None);
            false
        }
    }
}

pub fn highlight(ed: &Editor) -> Option<(u32, u32)> {
    if !ed.selection.is_empty() {
        return None;
    }
    let e = SOLID_BASE + selected()? as u32;
    Some((e, e + 1))
}

/// Meshes of solids on all levels (heights relative to the current level's elevation); voids as ghosts
/// (selectable in 3D). Voids that 3D does not cut are reported in the problem list.
pub fn scene_meshes(ed: &Editor, out: &mut Vec<ddc_render::SceneMesh>, issues: &mut Vec<String>) {
    let (parts, bad) = crate::solid_scene::parts(&ed.model, None, ed.elevation);
    for p in parts {
        if p.id < 0 || p.id as u64 >= SOLID_SPAN as u64 {
            continue;
        }
        out.push(ddc_render::SceneMesh {
            element: SOLID_BASE + p.id as u32,
            tag: p.tag,
            positions: p.pos,
            triangles: p.tris,
        });
    }
    for (id, e) in bad {
        issues.push(format!("solid {id}: {e}"));
    }
    let cat = Catalog::new(lang());
    let what = cat.t("solid.issue.host.wall");
    for id in crate::solid_scene::uncut_wall_voids(&ed.model) {
        issues.push(cat.tf(
            "solid.issue.no3d",
            &[("id", &id.to_string()), ("what", &what)],
        ));
    }
}

/// Floor solid for 3D with through voids; uncut voids go to the problem list.
pub fn slab_mesh(
    ed: &Editor,
    id: i64,
    issues: &mut Vec<String>,
) -> Result<ddc_geom::mesh::Mesh, ddc_model::Error> {
    let (m, skipped) = ed.model.slab_mesh_cut(id)?;
    if !skipped.is_empty() {
        let cat = Catalog::new(lang());
        let what = cat.t("solid.issue.host.slab");
        for v in skipped {
            issues.push(cat.tf(
                "solid.issue.no3d",
                &[("id", &v.to_string()), ("what", &what)],
            ));
        }
    }
    Ok(m)
}

/// Extrusion voids with a vertical axis as wall cutters of the 3D scene.
pub fn wall_cutters(ed: &Editor) -> Vec<ddc_geom::cut::Cutter> {
    let levels = ed.model.level_list().unwrap_or_default();
    let Ok(all) = ed.model.void_solids() else {
        return vec![];
    };
    let mut out = vec![];
    for (id, s) in all {
        let SolidForm::Extrusion {
            profile,
            start,
            end,
        } = &s.form
        else {
            continue;
        };
        if !s.void || !flat(&s.frame) {
            continue;
        }
        let hosts = ed.model.solid_hosts(id).unwrap_or_default();
        if !hosts.is_empty() && !hosts.iter().any(|h| h.0 == "wall") {
            continue;
        }
        // in quantities a void cuts only floors/roofs — it does not touch 3D walls
        let cuts = ed.model.solid_cuts(id).unwrap_or_default();
        if !cuts.is_empty() && !cuts.iter().any(|k| k.kind == "wall") {
            continue;
        }
        let Some(lv) = levels.iter().find(|l| l.0 == s.level) else {
            continue;
        };
        let f = s.frame;
        let dz = lv.2 - ed.elevation;
        let map = |r: &BulgeRing| -> Vec<V2> {
            gsolid::tess(r, gsolid::VIEW_SAG)
                .into_iter()
                .map(|p| {
                    let w = f.at(v3(p.x, p.y, 0.0));
                    v2(w.x, w.y)
                })
                .collect()
        };
        let (za, zb) = (
            f.at(v3(0.0, 0.0, *start)).z + dz,
            f.at(v3(0.0, 0.0, *end)).z + dz,
        );
        out.push(ddc_geom::cut::Cutter {
            ring: map(&profile.outer),
            holes: profile.holes.iter().map(|h| map(h)).collect(),
            z0: za.min(zb),
            z1: za.max(zb),
        });
    }
    out
}

// ---------- properties ----------

fn signed(s: &str, u: Units) -> Option<f64> {
    let t = s.trim();
    let (neg, r) = match t.strip_prefix('-').or_else(|| t.strip_prefix('−')) {
        Some(r) => (true, r),
        None => (false, t),
    };
    let zero = r
        .trim()
        .trim_end_matches("\u{43c}\u{43c}")
        .trim();
    let zero = crate::units::parse_locale_number(zero, crate::units::ui_decimal_mark())
        .is_ok_and(|v| v == 0.0);
    let v = if zero { 0.0 } else { parse_edit_length(r, u)? };
    Some(if neg { -v } else { v })
}

fn edit_signed(m: f64, u: Units) -> String {
    if m < 0.0 {
        format!("-{}", edit_length(-m, u))
    } else {
        edit_length(m, u)
    }
}

fn degrees(s: &str) -> Option<f64> {
    let t = s.trim().trim_end_matches('°').trim();
    crate::units::parse_locale_number(t, crate::units::ui_decimal_mark()).ok()
}

fn bufs(key: (i64, u64), s: &SolidSpec, u: Units) -> Bufs {
    let (a, b) = match &s.form {
        SolidForm::Extrusion { start, end, .. } => (edit_signed(*start, u), edit_signed(*end, u)),
        SolidForm::Blend { height, .. } => (edit_length(*height, u), String::new()),
        SolidForm::Revolve { start, end, .. } => (
            format!("{:.1}", start.to_degrees()),
            format!("{:.1}", end.to_degrees()),
        ),
        _ => (String::new(), String::new()),
    };
    Bufs {
        key,
        name: s.name.clone(),
        material: s.material.clone(),
        base: edit_signed(s.frame.o.z, u),
        a,
        b,
    }
}

/// Properties panel of the selected solid; false — no solid selected.
pub fn props(ui: &mut egui::Ui, t: &Tokens, ed: &mut Editor, cat: &Catalog) -> bool {
    with(|s| s.lang = Some(cat.lang));
    // a solid picked in 3D sits in the shared selection; anything else there is not ours
    match ed.selection.as_slice() {
        [] => {}
        [crate::editor::Sel::Ext(c, id)] if *c == crate::scene_ids::EXT_SOLID => select(Some(*id)),
        _ => {
            select(None);
            return false;
        }
    }
    let Some(id) = selected() else {
        return false;
    };
    let Ok(spec) = ed.model.solid(id) else {
        select(None);
        return false;
    };
    let q = ed.model.solid_quantities(id).ok().flatten();
    let cuts = ed.model.solid_cuts(id).unwrap_or_default();
    let u = ed.tool.units;
    let ul = unit_label(u);
    let key = (id, ed.revision);
    let mut b = with(|s| std::mem::take(&mut s.buf));
    if b.key != key {
        b = bufs(key, &spec, u);
    }
    let tip = cat.t("solid.tip.enter");
    let mut next: Option<SolidSpec> = None;
    let mut delete = false;
    let title = if spec.void {
        "solid.void"
    } else {
        "solid.title"
    };
    ui_kit::element_header(ui, "component", &cat.t(title), "", id);
    {
        ui_kit::props(ui, "solid_props", |ui| {
            let kind = cat.t(&format!("solid.kind.{}", spec.form.kind()));
            ui_kit::kv(ui, &cat.t("solid.kind"), kind);
            if let Some(v) = ui_kit::kv_edit(ui, &cat.t("solid.name"), &mut b.name, "", &tip) {
                let mut s = spec.clone();
                s.name = v.trim().to_string();
                next = Some(s);
            }
            if let Some(v) =
                ui_kit::kv_edit(ui, &cat.t("solid.material"), &mut b.material, "", &tip)
            {
                let mut s = spec.clone();
                s.material = v.trim().to_string();
                next = Some(s);
            }
            if let Some(v) = ui_kit::kv_edit(ui, &cat.t("solid.base"), &mut b.base, ul, &tip)
                .and_then(|v| signed(&v, u))
            {
                let mut s = spec.clone();
                s.frame.o.z = v;
                next = Some(s);
            }
            match &spec.form {
                SolidForm::Extrusion { .. } => {
                    let a = ui_kit::kv_edit(ui, &cat.t("solid.start"), &mut b.a, ul, &tip)
                        .and_then(|v| signed(&v, u));
                    let e = ui_kit::kv_edit(ui, &cat.t("solid.end"), &mut b.b, ul, &tip)
                        .and_then(|v| signed(&v, u));
                    if a.is_some() || e.is_some() {
                        let mut s = spec.clone();
                        if let SolidForm::Extrusion { start, end, .. } = &mut s.form {
                            *start = a.unwrap_or(*start);
                            *end = e.unwrap_or(*end);
                        }
                        next = Some(s);
                    }
                }
                SolidForm::Blend { .. } => {
                    if let Some(h) = ui_kit::kv_edit(ui, &cat.t("solid.height"), &mut b.a, ul, &tip)
                        .and_then(|v| parse_edit_length(&v, u))
                    {
                        let mut s = spec.clone();
                        if let SolidForm::Blend { height, .. } = &mut s.form {
                            *height = h;
                        }
                        next = Some(s);
                    }
                }
                SolidForm::Revolve { .. } => {
                    let a = ui_kit::kv_edit(ui, &cat.t("solid.angle_start"), &mut b.a, "°", &tip)
                        .and_then(|v| degrees(&v));
                    let e = ui_kit::kv_edit(ui, &cat.t("solid.angle_end"), &mut b.b, "°", &tip)
                        .and_then(|v| degrees(&v));
                    if a.is_some() || e.is_some() {
                        let mut s = spec.clone();
                        if let SolidForm::Revolve { start, end, .. } = &mut s.form {
                            *start = a.map_or(*start, f64::to_radians);
                            *end = e.map_or(*end, f64::to_radians);
                        }
                        next = Some(s);
                    }
                }
                SolidForm::Sweep { path, .. } | SolidForm::SweptBlend { path, .. } => {
                    ui_kit::kv(ui, &cat.t("solid.path_pts"), path.pts.len().to_string());
                }
            }
            match &q {
                Some(q) => {
                    ui_kit::kv_num(ui, &cat.t("solid.volume"), vol_ui(q.volume));
                    ui_kit::kv_num(ui, &cat.t("solid.area"), area_ui(q.area));
                    if !spec.void {
                        ui_kit::kv_num(ui, &cat.t("solid.net"), vol_ui(q.net_volume));
                    }
                }
                None => ui_kit::kv(ui, &cat.t("solid.volume"), cat.t("solid.err.geometry")),
            }
            let list: Vec<String> = cuts
                .iter()
                .map(|c| {
                    format!(
                        "{} #{} ({})",
                        cat.t(&format!("solid.host.{}", c.kind)),
                        c.id,
                        vol_ui(c.volume)
                    )
                })
                .collect();
            if spec.void || !list.is_empty() {
                let v = if list.is_empty() {
                    cat.t("solid.cuts.none")
                } else {
                    list.join(", ")
                };
                ui_kit::kv(ui, &cat.t("solid.cuts"), v);
            }
        });
        // the shared height row of the element table (extrusions only; others show nothing)
        crate::geom_props::show(ui, ed, "solid", id);
        let mut void = spec.void;
        if ui.checkbox(&mut void, cat.t("solid.is_void")).changed() {
            let mut s = spec.clone();
            s.void = void;
            next = Some(s);
        }
        ui_kit::dim(ui, t, cat.t("solid.grips"));
        if ui.button(cat.t("solid.delete")).clicked() {
            delete = true;
        }
    }
    with(|s| s.buf = b);
    if let Some(s) = next {
        edit(ed, id, s);
    }
    if delete {
        if ed.selection.is_empty() {
            delete_selected(ed);
        } else {
            ed.delete_selection();
        }
    }
    true
}

// ---------- drawing ----------

fn arc_pts(p: V2, q: V2, b: f64, out: &mut Vec<V2>, depth: u32) {
    if b == 0.0 || depth == 0 {
        out.push(q);
        return;
    }
    let m = edge_mid(p, q, b);
    let h = (b.atan() / 2.0).tan();
    arc_pts(p, m, h, out, depth - 1);
    arc_pts(m, q, h, out, depth - 1);
}

/// Polyline through points with arcs (`closed` — with a closing edge).
fn bulged(pts: &[V2], bulges: &[f64], closed: bool) -> Vec<V2> {
    let n = pts.len();
    let mut out = pts.first().copied().into_iter().collect::<Vec<_>>();
    let edges = if closed { n } else { n.saturating_sub(1) };
    for i in 0..edges {
        let b = bulges.get(i).copied().unwrap_or(0.0);
        arc_pts(pts[i], pts[(i + 1) % n], b, &mut out, 4);
    }
    out
}

fn fill(
    p: &egui::Painter,
    outer: &[V2],
    holes: &[Vec<V2>],
    col: Color32,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let tris = gsolid::cap(outer, holes);
    let mut m = egui::Mesh::default();
    for q in outer.iter().chain(holes.iter().flatten()) {
        m.colored_vertex(to_s(*q), col);
    }
    for t in tris {
        m.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
    }
    p.add(m);
}

fn line(p: &egui::Painter, pts: &[V2], st: Stroke, to_s: &dyn Fn(V2) -> Pos2) {
    let s: Vec<Pos2> = pts.iter().map(|q| to_s(*q)).collect();
    p.add(egui::Shape::line(s, st));
}

fn ring_line(
    p: &egui::Painter,
    r: &BulgeRing,
    st: Stroke,
    map: &dyn Fn(V2) -> V2,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let pts: Vec<V2> = bulged(&r.pts, &r.bulges, true)
        .into_iter()
        .map(map)
        .collect();
    line(p, &pts, st, to_s);
}

/// Plan: sections and projections of the level's solids, handles of the selected one, drag and sketch previews.
pub fn paint(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    rect: Rect,
    ed: &mut Editor,
    t: &Tokens,
    to_s: &dyn Fn(V2) -> Pos2,
) {
    let sel = selected().filter(|_| ed.selection.is_empty());
    with_items(ed, |items| {
        for it in items {
            let hot = sel == Some(it.id);
            let col = if hot { t.accent } else { t.cut };
            for (o, hs) in &it.cuts {
                fill(p, o, hs, t.poche, to_s);
                for r in std::iter::once(o).chain(hs) {
                    let mut r = r.clone();
                    r.extend(r.first().copied());
                    line(p, &r, Stroke::new(if hot { 2.0 } else { 1.4 }, col), to_s);
                }
            }
            let st = Stroke::new(1.0, if hot { t.accent } else { t.projected });
            for (a, b) in &it.edges {
                if it.void {
                    p.add(egui::Shape::dashed_line(
                        &[to_s(*a), to_s(*b)],
                        st,
                        6.0,
                        3.0,
                    ));
                } else {
                    p.line_segment([to_s(*a), to_s(*b)], st);
                }
            }
        }
    });
    if let Some(spec) = sel.and_then(|id| ed.model.solid(id).ok()) {
        let st = Stroke::new(1.4, t.accent);
        for (g, q) in grips(&spec) {
            let c = to_s(q);
            if g == Grip::Move {
                p.circle_stroke(c, 5.0, st);
                p.circle_filled(c, 2.0, t.accent);
            } else {
                let r = Rect::from_center_size(c, Vec2::splat(8.0));
                p.rect_filled(r, 0.0, t.paper);
                p.rect_stroke(r, 0.0, st, egui::StrokeKind::Middle);
            }
        }
    }
    let drag = with(|s| {
        s.drag
            .as_ref()
            .map(|d| moved(&d.spec, d.grip, d.from, d.cur))
    });
    if let Some(s) = drag {
        preview_spec(p, &s, Stroke::new(1.5, t.ov().preview), to_s);
    }
    let Some(mut d) = with(|s| s.draft.take()) else {
        return;
    };
    let st = Stroke::new(1.5, t.ov().preview);
    let id = |q: V2| q;
    for r in d.profile.iter().chain(&d.holes).chain(d.top.iter()) {
        ring_line(p, r, st, &id, to_s);
    }
    let mut pts = d.pts.clone();
    let mut bs = d.bulges.clone();
    pts.push(d.cur);
    bs.push(0.0);
    let open = bulged(&pts, &bs, false);
    if d.stage == Stage::Axis {
        if let [a, ..] = d.pts.as_slice() {
            p.add(egui::Shape::dashed_line(
                &[to_s(*a), to_s(d.cur)],
                st,
                8.0,
                4.0,
            ));
        }
    } else {
        line(p, &open, st, to_s);
    }
    for q in &d.pts {
        p.circle_filled(to_s(*q), 3.0, t.ov().preview);
    }
    options_bar(ui, rect, &mut d, t);
    with(|s| {
        if s.draft.is_none() {
            s.draft = Some(d);
        }
    });
}

/// Solid outlines in the plan (level work plane — the profile; otherwise the path/axis projection).
fn preview_spec(p: &egui::Painter, s: &SolidSpec, st: Stroke, to_s: &dyn Fn(V2) -> Pos2) {
    let f = s.frame;
    let at = |z: f64| {
        move |q: V2| {
            let w = f.at(v3(q.x, q.y, z));
            v2(w.x, w.y)
        }
    };
    match &s.form {
        SolidForm::Extrusion { profile, .. } | SolidForm::Revolve { profile, .. } => {
            ring_line(p, &profile.outer, st, &at(0.0), to_s)
        }
        SolidForm::Blend {
            bottom,
            top,
            height,
        } => {
            ring_line(p, bottom, st, &at(0.0), to_s);
            ring_line(p, top, st, &at(*height), to_s);
        }
        SolidForm::Sweep { path, .. } | SolidForm::SweptBlend { path, .. } => {
            let pts: Vec<V2> = path
                .polyline_plan()
                .into_iter()
                .map(|q| {
                    let w = f.at(q);
                    v2(w.x, w.y)
                })
                .collect();
            line(p, &pts, st, to_s);
        }
    }
}

trait PlanPath {
    fn polyline_plan(&self) -> Vec<V3>;
}

impl PlanPath for SolidPath {
    fn polyline_plan(&self) -> Vec<V3> {
        if let Some(sm) = &self.smooth {
            if let Ok(st) = ddc_geom::frame3::stations(sm, 1e-3, 0.05) {
                return st.st.iter().map(|s| v3(s.p.x, s.p.y, 0.0)).collect();
            }
        }
        let plan: Vec<V2> = self.pts.iter().map(|q| v2(q.x, q.y)).collect();
        let flat = bulged(&plan, &self.bulges, false);
        // per-point polyline heights are not needed for the plan
        flat.into_iter().map(|q| v3(q.x, q.y, 0.0)).collect()
    }
}

fn num(ui: &mut egui::Ui, label: &str, v: &mut f64, unit: &str, lo: f64) {
    ui.label(label);
    ui.add(
        egui::DragValue::new(v)
            .speed(0.01)
            .range(lo..=1.0e4)
            .suffix(format!(" {unit}")),
    );
}

fn options_bar(ui: &mut egui::Ui, rect: Rect, d: &mut Draft, t: &Tokens) {
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), BAR_H));
    let p = ui.painter_at(bar);
    p.rect_filled(bar, 0.0, t.panel);
    p.line_segment(
        [bar.left_bottom(), bar.right_bottom()],
        Stroke::new(1.0, t.border),
    );
    let mut cu = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar.shrink2(Vec2::new(10.0, 4.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    crate::ui_kit::bar_row(&mut cu, "solid_opts_scroll", |ui| options_row(ui, d, t));
}

fn options_row(ui: &mut egui::Ui, d: &mut Draft, t: &Tokens) {
    ui.spacing_mut().item_spacing.x = 6.0;
    let m = d.cat.t("unit.m");
    let what = d.cat.t(&format!(
        "{}.{}",
        if d.void {
            "solid.void_kind"
        } else {
            "solid.kind"
        },
        d.kind.key()
    ));
    ui.label(egui::RichText::new(what).strong());
    ui.separator();
    let c = &d.cat;
    let o = &mut d.opts;
    num(ui, &c.t("solid.base"), &mut o.base, &m, -1.0e4);
    match d.kind {
        Kind::Extrusion => {
            num(ui, &c.t("solid.start"), &mut o.start, &m, -1.0e4);
            num(ui, &c.t("solid.end"), &mut o.end, &m, -1.0e4);
        }
        Kind::Blend => num(ui, &c.t("solid.height"), &mut o.height, &m, 0.001),
        Kind::Revolve => {
            num(ui, &c.t("solid.opt.angle"), &mut o.angle_deg, "°", 0.1);
            o.angle_deg = o.angle_deg.min(360.0);
            ui.checkbox(&mut o.upright, c.t("solid.opt.upright"));
        }
        Kind::Sweep | Kind::SweptBlend => num(ui, &c.t("solid.opt.rise"), &mut o.rise, &m, -1.0e4),
    }
    if takes_holes(d.kind) {
        ui.checkbox(&mut o.holes, c.t("solid.opt.holes"));
    }
    if o.plane.is_some() {
        let mut on = true;
        ui.checkbox(&mut on, c.t("solid.opt.face"));
        if !on {
            o.plane = None;
            with(|s| s.plane = None);
        }
    }
    ui.label(c.t("solid.material"));
    ui.add(egui::TextEdit::singleline(&mut o.material).desired_width(110.0));
    ui.separator();
    ui.label(egui::RichText::new(d.hint()).color(t.text_dim));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn sketch(e: &mut Editor, pts: &[V2]) {
        for p in pts {
            e.handle(Event::Click(*p));
        }
        e.handle(Event::Key(Key::Enter));
    }

    fn volume(e: &Editor, id: i64) -> f64 {
        e.model.solid_quantities(id).unwrap().unwrap().volume
    }

    #[test]
    fn sketch_extrusion_grip_edit_delete_and_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Extrusion, false, Lang::En);
        with(|s| s.draft.as_mut().unwrap().opts.end = 2.0);
        sketch(
            &mut e,
            &[v2(0.0, 0.0), v2(2.0, 0.0), v2(2.0, 1.0), v2(0.0, 1.0)],
        );
        let all = e.model.solids(None).unwrap();
        assert_eq!(all.len(), 1, "{:?}", e.status);
        let id = all[0].0;
        assert!(close(volume(&e, id), 4.0));
        e.handle(Event::Key(Key::Escape));
        assert!(with(|s| s.draft.is_none()));
        assert_eq!(e.wall_mode(), Mode::Select);
        // selection by clicking inside the section, vertex handle (2, 1) → (3, 1): trapezoid 2.5 × 2
        e.handle(Event::Click(v2(1.0, 0.5)));
        assert_eq!(selected(), Some(id));
        assert_eq!(
            highlight(&e),
            Some((SOLID_BASE + id as u32, SOLID_BASE + id as u32 + 1))
        );
        e.handle(Event::Press(v2(2.0, 1.0)));
        e.handle(Event::Move(v2(3.0, 1.0)));
        e.handle(Event::Release(v2(3.0, 1.0)));
        assert!(close(volume(&e, id), 5.0), "{}", volume(&e, id));
        e.undo();
        assert!(close(volume(&e, id), 4.0));
        e.delete_selection();
        assert!(e.model.solids(None).unwrap().is_empty());
        e.undo();
        assert_eq!(e.model.solids(None).unwrap().len(), 1);
        select(None);
    }

    #[test]
    fn upright_revolve_and_void_cutter() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Revolve, false, Lang::Ru);
        // 1×2 profile to the right of the axis, axis along +y → cylindrical tube r 1..2, h 2
        sketch(
            &mut e,
            &[v2(11.0, 0.0), v2(12.0, 0.0), v2(12.0, 2.0), v2(11.0, 2.0)],
        );
        e.handle(Event::Click(v2(10.0, 0.0)));
        e.handle(Event::Click(v2(10.0, 3.0)));
        let all = e.model.solids(None).unwrap();
        assert_eq!(all.len(), 1, "{:?}", e.status);
        let v = volume(&e, all[0].0);
        let exact = std::f64::consts::PI * (4.0 - 1.0) * 2.0;
        assert!((v - exact).abs() < 1e-6, "{v} vs {exact}");
        e.handle(Event::Key(Key::Escape));
        arm(&mut e, Kind::Extrusion, true, Lang::En);
        sketch(
            &mut e,
            &[v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0), v2(0.0, 1.0)],
        );
        let cut = wall_cutters(&e);
        assert_eq!(cut.len(), 1);
        assert!(cut[0].z1 > cut[0].z0);
        e.handle(Event::Key(Key::Escape));
    }

    #[test]
    fn profile_holes_and_value_grip() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Extrusion, false, Lang::En);
        with(|s| {
            let d = s.draft.as_mut().unwrap();
            d.opts.end = 2.0;
            d.opts.holes = true;
        });
        sketch(
            &mut e,
            &[v2(0.0, 0.0), v2(4.0, 0.0), v2(4.0, 4.0), v2(0.0, 4.0)],
        );
        assert!(e.model.solids(None).unwrap().is_empty(), "waits for holes");
        sketch(
            &mut e,
            &[v2(1.0, 1.0), v2(2.0, 1.0), v2(2.0, 2.0), v2(1.0, 2.0)],
        );
        e.handle(Event::Key(Key::Enter));
        let all = e.model.solids(None).unwrap();
        assert_eq!(all.len(), 1, "{:?}", e.status);
        let id = all[0].0;
        assert!(close(volume(&e, id), 30.0), "{}", volume(&e, id));
        e.handle(Event::Key(Key::Escape));
        // value handle right of the solid: +1 m in x — depth 3
        e.handle(Event::Click(v2(3.0, 3.0)));
        assert_eq!(selected(), Some(id));
        let at = v2(4.0 + VALUE_GAP, 2.0);
        e.handle(Event::Press(at));
        e.handle(Event::Release(at + v2(1.0, 0.0)));
        assert!(close(volume(&e, id), 45.0), "{}", volume(&e, id));
        // Esc clears the selection; undoing the creation removes the selection
        e.handle(Event::Key(Key::Escape));
        assert_eq!(selected(), None);
        select(Some(id));
        e.undo();
        e.undo();
        assert!(e.model.solids(None).unwrap().is_empty());
        assert_eq!(selected(), None);
    }

    #[test]
    fn undo_and_tool_change_reset_the_sketch() {
        let mut e = Editor::new_in_memory().unwrap();
        arm(&mut e, Kind::Blend, false, Lang::Ru);
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Click(v2(1.0, 0.0)));
        e.undo();
        assert!(with(|s| s.draft.as_ref().is_some_and(|d| d.pts.is_empty())));
        assert!(e.wall_draft().points.is_empty());
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.set_mode(Mode::Rect);
        assert!(with(|s| s.draft.is_none()));
    }

    #[test]
    fn void_in_wall_is_reached_by_tab() {
        let mut e = Editor::new_in_memory().unwrap();
        e.handle(Event::Click(v2(0.0, 0.0)));
        e.handle(Event::Click(v2(6.0, 0.0)));
        e.handle(Event::Key(Key::Escape));
        assert_eq!(e.walls.len(), 1);
        arm(&mut e, Kind::Extrusion, true, Lang::En);
        with(|s| {
            let d = s.draft.as_mut().unwrap();
            d.opts.start = 0.5;
            d.opts.end = 1.5;
        });
        sketch(
            &mut e,
            &[v2(2.0, -1.0), v2(3.0, -1.0), v2(3.0, 1.0), v2(2.0, 1.0)],
        );
        e.handle(Event::Key(Key::Escape));
        let v = e.model.solids(None).unwrap()[0].0;
        assert_eq!(e.model.solid_cuts(v).unwrap().len(), 1);
        e.handle(Event::Click(v2(2.5, 0.0)));
        assert_eq!(selected(), None, "the wall wins the click");
        e.handle(Event::Key(Key::Tab));
        assert_eq!(selected(), Some(v));
        // a void is a translucent ghost in 3D, selectable there
        let (parts, bad) = crate::solid_scene::parts(&e.model, None, e.elevation);
        assert!(bad.is_empty());
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].tag, crate::solid_scene::TAG_VOID);
        assert!(select_scene(&mut e, Some(SOLID_BASE + v as u32)));
        select(None);
    }
}
