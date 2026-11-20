// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View panes (`view.panes`): the view area as one to four panes (side by side, stacked, one
//! big and two, four), each showing a level plan, a 3D view or a section/elevation of the model.
//! Plans of several levels and up to four 3D views (each with its own camera, style, section box
//! and renderer slot) can be open at once. The pane pressed last is active: its level becomes
//! the editor's level (the live, editable plan) and its 3D view the main one (tools, keys); the
//! others draw read-only. Selection is the editor's one set, so every pane highlights it. One
//! pane maximizes and restores (`view.pane_max`, header double click). Display style is per
//! pane: the 3D visual style, the detail level of a section/elevation; plans keep their template.

use crate::editor::sec::{display_name, look};
use crate::editor::Editor;
use crate::section_view::SectionView;
use crate::shell::EditorApp;
use crate::ui_kit::Tokens;
use crate::view_gfx_ui::GfxLayer;
use crate::viewport::Viewport;
use ddc_geom::{v2, V2};
use ddc_model::{DetailLevel, ViewKind};
use ddc_render::VisualStyle;
use ddc_ui::theme::space;
use ddc_ui::widgets::{self, MenuTone};
use ddc_ui::Catalog;
use eframe::egui::{self, pos2, Rect};
use std::sync::Arc;

/// Width of the line between panes, points.
pub const DIV: f32 = 2.0;

/// How the view area is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    One,
    /// Two side by side.
    Cols2,
    /// Two stacked.
    Rows2,
    /// One big on the left, two stacked on the right.
    Left3,
    /// One big on top, two side by side below.
    Top3,
    Quad,
}

impl Layout {
    pub const ALL: [Layout; 6] = [
        Layout::One,
        Layout::Cols2,
        Layout::Rows2,
        Layout::Left3,
        Layout::Top3,
        Layout::Quad,
    ];

    pub fn count(self) -> usize {
        match self {
            Layout::One => 1,
            Layout::Cols2 | Layout::Rows2 => 2,
            Layout::Left3 | Layout::Top3 => 3,
            Layout::Quad => 4,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Layout::One => "view_cube.layout.one",
            Layout::Cols2 => "view_cube.layout.cols2",
            Layout::Rows2 => "view_cube.layout.rows2",
            Layout::Left3 => "view_cube.layout.left3",
            Layout::Top3 => "view_cube.layout.top3",
            Layout::Quad => "view_cube.layout.quad",
        }
    }
}

/// Pane rectangles of `l` in `area`, in pane order: they cover the area except the `DIV`
/// lines between them and never overlap. Split lines sit on whole points.
pub fn rects(l: Layout, area: Rect) -> Vec<Rect> {
    let h = DIV * 0.5;
    let (x, y) = (area.center().x.round(), area.center().y.round());
    let left = |r: Rect| Rect::from_min_max(r.min, pos2(x - h, r.max.y));
    let right = |r: Rect| Rect::from_min_max(pos2(x + h, r.min.y), r.max);
    let top = |r: Rect| Rect::from_min_max(r.min, pos2(r.max.x, y - h));
    let bottom = |r: Rect| Rect::from_min_max(pos2(r.min.x, y + h), r.max);
    match l {
        Layout::One => vec![area],
        Layout::Cols2 => vec![left(area), right(area)],
        Layout::Rows2 => vec![top(area), bottom(area)],
        Layout::Left3 => {
            let r = right(area);
            vec![left(area), top(r), bottom(r)]
        }
        Layout::Top3 => {
            let b = bottom(area);
            vec![top(area), left(b), right(b)]
        }
        Layout::Quad => {
            let (t, b) = (top(area), bottom(area));
            vec![left(t), right(t), left(b), right(b)]
        }
    }
}

/// What a pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    /// The plan of a level (`level` id).
    Plan(i64),
    /// 3D view number 0..`CAMS`.
    D3(usize),
    /// A section or elevation of the model (`view_def` id).
    View(i64),
    /// Nothing chosen yet (no elevation in the model).
    Empty,
}

/// One pane slot: its source and its own display style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pane {
    pub src: Src,
    /// Used while the pane shows 3D.
    pub style: VisualStyle,
    /// Detail level of a section/elevation; `None` follows the view template.
    pub detail: Option<DetailLevel>,
}

impl Pane {
    fn of(src: Src, style: VisualStyle) -> Self {
        Pane {
            src,
            style,
            detail: None,
        }
    }
}

/// Look of a front view (seen from the south, looking north) and of a side view (seen from
/// the east, looking west).
pub const FRONT_LOOK: (f64, f64) = (0.0, 1.0);
pub const SIDE_LOOK: (f64, f64) = (-1.0, 0.0);

/// The model view that looks most nearly along `want` (elevations before sections), when one
/// looks within about 45 degrees of it.
pub fn pick_by_look(views: &[(i64, ddc_model::SectionSpec)], want: V2) -> Option<i64> {
    views
        .iter()
        .filter_map(|(id, s)| {
            let d = look(s);
            let dot = d.x * want.x + d.y * want.y;
            let bonus = if s.kind == ViewKind::Elevation { 1.0 } else { 0.0 };
            (dot > 0.7).then_some((*id, dot + bonus))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|p| p.0)
}

/// A level plan in the terms the section painter draws: wall cuts as cut rings, door leaves,
/// swings, window and curtain lines as projection edges; grids, room tags and dimensions as they
/// are. Picking keeps working since every piece names its element.
pub fn plan_as_drawing(mut d: ddc_model::PlanView) -> ddc_model::PlanView {
    use ddc_model::{LineClass, ViewPrim as V};
    let edge = |table, element, a, b| V::Edge {
        table,
        element,
        class: LineClass::Projection,
        a,
        b,
    };
    let mut out = Vec::with_capacity(d.prims.len());
    for p in d.prims.drain(..) {
        match p {
            V::Cut {
                wall,
                material,
                ring,
                ..
            } => out.push(V::SectionCut {
                table: "wall",
                element: wall,
                material,
                rings: ring,
                holes: vec![],
            }),
            V::DoorLeaf { opening, a, b } | V::WindowLine { opening, a, b } => {
                out.push(edge("opening", opening, a, b))
            }
            V::CurtainLine { curtain, a, b } => out.push(edge("curtain_wall", curtain, a, b)),
            V::DoorSwing {
                opening,
                center,
                radius,
                a0,
                a1,
            } => {
                let n = 12;
                let at = |k: usize| {
                    let t = (a0 + (a1 - a0) * k as f64 / n as f64).to_radians();
                    (center.0 + radius * t.cos(), center.1 + radius * t.sin())
                };
                out.extend((0..n).map(|k| edge("opening", opening, at(k), at(k + 1))));
            }
            V::Annot { .. } | V::Tag { .. } => {}
            other => out.push(other),
        }
    }
    d.prims = out;
    d
}

/// Overridden graphics of one pane: source layer (address, stamp), level, result.
type GfxCache = Option<(usize, u64, DetailLevel, Arc<GfxLayer>)>;

/// 3D views at most: one per pane.
pub const CAMS: usize = 4;

/// The pane state of the session.
pub struct Panes {
    pub on: bool,
    pub layout: Layout,
    pub panes: [Pane; 4],
    pub active: usize,
    pub max: Option<usize>,
    sviews: [SectionView; 4],
    gfx: [GfxCache; 4],
    seeded: bool,
    /// Panes drawn last frame: (slot, whole pane, header).
    pub drawn: Vec<(usize, Rect, Rect)>,
    /// The 3D view living in the shell's viewport; the others wait in `cams`.
    pub main: usize,
    cams: [Option<Box<Viewport>>; CAMS],
    /// The editor level seen last: a plan pane follows a level changed elsewhere.
    level: Option<i64>,
}

impl Default for Panes {
    fn default() -> Self {
        let s = VisualStyle::default();
        Panes {
            on: false,
            layout: Layout::Quad,
            panes: [
                Pane::of(Src::Plan(0), s),
                Pane::of(Src::Empty, s),
                Pane::of(Src::Empty, s),
                Pane::of(Src::D3(0), s),
            ],
            active: 0,
            max: None,
            sviews: Default::default(),
            gfx: Default::default(),
            seeded: false,
            drawn: vec![],
            main: 0,
            cams: Default::default(),
            level: None,
        }
    }
}

impl Panes {
    /// Slots on screen, in drawing order.
    pub fn shown(&self) -> Vec<usize> {
        match self.max {
            Some(m) => vec![m],
            None => (0..self.layout.count()).collect(),
        }
    }

    /// Slot and rectangle of every pane on screen.
    pub fn layout_rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        match self.max {
            Some(m) => vec![(m, area)],
            None => rects(self.layout, area).into_iter().enumerate().collect(),
        }
    }

    /// A shown pane has this source.
    pub fn shows(&self, src: Src) -> bool {
        self.on && self.shown().iter().any(|&i| self.panes[i].src == src)
    }

    /// A shown pane is a 3D view.
    pub fn shows_3d(&self) -> bool {
        self.on && self.shown().iter().any(|&i| matches!(self.panes[i].src, Src::D3(_)))
    }

    pub fn active_src(&self) -> Src {
        self.panes[self.active].src
    }

    /// The shown pane with `src`, the active one first.
    fn pane_of(&self, src: Src) -> Option<usize> {
        let shown = self.shown();
        let mut it = std::iter::once(self.active).chain(shown.iter().copied());
        it.find(|&i| shown.contains(&i) && self.panes[i].src == src)
    }

    /// The pane of the main 3D view (the shell's viewport).
    pub fn main_pane(&self) -> Option<usize> {
        self.on.then(|| self.pane_of(Src::D3(self.main))).flatten()
    }

    /// The pane that draws the live plan of `level`; other plan panes draw read-only.
    pub fn plan_pane(&self, level: i64) -> Option<usize> {
        self.pane_of(Src::Plan(level))
    }

    /// Pane `i` draws the live, editable plan: it shows the editor's level and is that level's
    /// plan pane. Every other plan pane draws read-only.
    pub fn draws_live_plan(&self, i: usize, ed_level: i64) -> bool {
        self.panes.get(i).is_some_and(|p| p.src == Src::Plan(ed_level)) && self.plan_pane(ed_level) == Some(i)
    }

    /// 3D view `k` was opened.
    pub fn cam_exists(&self, k: usize) -> bool {
        k == self.main || self.cams.get(k).is_some_and(|c| c.is_some()) || self.panes.iter().any(|p| p.src == Src::D3(k))
    }

    /// The number a new 3D view gets, while there is room for one.
    pub fn new_cam(&self) -> Option<usize> {
        (0..CAMS).find(|&k| !self.cam_exists(k))
    }

    /// Makes 3D view `k` the main one: it moves into `view`, the old main one is parked. The
    /// tools and the work plane stay in `view`, as they are the editor's.
    pub fn make_main(&mut self, view: &mut Viewport, k: usize) {
        if k == self.main || k >= CAMS {
            return;
        }
        let mut v = self.cams[k].take().unwrap_or_else(|| Box::new(view.twin(k as u32)));
        std::mem::swap(&mut *v, view);
        std::mem::swap(&mut v.e3, &mut view.e3);
        self.cams[self.main] = Some(v);
        self.main = k;
    }

    /// Another model (open, import): every parked 3D view shows all of it on its next frame,
    /// as the main one does.
    pub fn refit_cams(&mut self) {
        for c in self.cams.iter_mut().flatten() {
            c.refit();
        }
    }

    /// Parked 3D view `k` (made from `main` the first time), to draw read-only.
    fn cam(&mut self, k: usize, main: &Viewport) -> Box<Viewport> {
        let mut v = self.cams[k].take().unwrap_or_else(|| Box::new(main.twin(k as u32)));
        v.share_from(main);
        v
    }

    /// The camera of 3D view `k`.
    pub fn camera_of<'a>(&'a self, main: &'a Viewport, k: usize) -> Option<&'a ddc_render::OrbitCamera> {
        if k == self.main {
            return Some(&main.camera);
        }
        self.cams.get(k)?.as_ref().map(|v| &v.camera)
    }

    /// Makes the active pane's level the editor's (the live plan moves to that pane, the plan
    /// it leaves keeps its place read-only) and its 3D view the main one.
    pub fn activate(&mut self, ed: &mut Editor, plan: &mut crate::plan_view::PlanView, view: &mut Viewport) {
        match self.active_src() {
            Src::Plan(l) if l != ed.level && ed.levels().iter().any(|x| x.0 == l) => {
                let old = ed.level;
                if let Some(j) = self.plan_pane(old) {
                    self.sviews[j].place_plan(old, plan.center, plan.scale);
                }
                let sv = &self.sviews[self.active];
                let (c, s, seen) = (sv.center, sv.scale, sv.plan_level == Some(l));
                ed.set_level(l);
                self.level = Some(ed.level);
                if s > 0.0 && seen {
                    plan.center = c;
                    plan.scale = s;
                }
            }
            Src::D3(k) => self.make_main(view, k),
            _ => {}
        }
    }

    /// Turns the panes on; the first time the slots get plan, front, side and 3D.
    pub fn enable(&mut self, ed: &Editor, style: VisualStyle) {
        if !self.seeded {
            self.seed(ed, style);
        }
        self.on = true;
        self.check(ed);
    }

    /// Plan, front elevation, side elevation, 3D; every pane starts with `style`.
    pub fn seed(&mut self, ed: &Editor, style: VisualStyle) {
        let pick = |w: (f64, f64)| pick_by_look(&ed.views, v2(w.0, w.1)).map_or(Src::Empty, Src::View);
        let srcs = [Src::Plan(ed.level), pick(FRONT_LOOK), pick(SIDE_LOOK), Src::D3(self.main)];
        for (p, s) in self.panes.iter_mut().zip(srcs) {
            *p = Pane::of(s, style);
        }
        self.layout = Layout::Quad;
        self.active = 0;
        self.max = None;
        self.seeded = true;
        self.level = Some(ed.level);
    }

    /// Views that left the model empty their panes, plans of deleted levels show the editor's
    /// level, a level picked elsewhere moves its plan pane along; empty panes take the front
    /// and side elevations that are not shown yet.
    pub fn check(&mut self, ed: &Editor) {
        let levels: Vec<i64> = ed.levels().iter().map(|l| l.0).collect();
        let moved = self.level.filter(|l| *l != ed.level);
        let follow = moved.and_then(|old| self.plan_pane(old).or_else(|| self.panes.iter().position(|p| p.src == Src::Plan(old))));
        if let Some(i) = follow {
            self.panes[i].src = Src::Plan(ed.level);
        }
        self.level = Some(ed.level);
        for p in &mut self.panes {
            match p.src {
                Src::View(id) if ed.view_spec(id).is_none() => p.src = Src::Empty,
                Src::Plan(l) if !levels.contains(&l) => p.src = Src::Plan(ed.level),
                _ => {}
            }
        }
        for w in [FRONT_LOOK, SIDE_LOOK] {
            let Some(id) = pick_by_look(&ed.views, v2(w.0, w.1)) else {
                continue;
            };
            if self.panes.iter().any(|p| p.src == Src::View(id)) {
                continue;
            }
            if let Some(p) = self.panes.iter_mut().find(|p| p.src == Src::Empty) {
                p.src = Src::View(id);
            }
        }
    }

    /// Shows `src` in slot `i`. Plans of one level may repeat (one draws live); a 3D view has
    /// one frame on the GPU, so the slot that had it takes the slot's old source.
    pub fn set_src(&mut self, i: usize, src: Src) {
        if matches!(src, Src::D3(_)) {
            if let Some(j) = (0..4).find(|&j| j != i && self.panes[j].src == src) {
                self.panes[j].src = self.panes[i].src;
            }
        }
        self.panes[i].src = src;
    }

    /// Makes the pane that shows `src` active (any 3D pane for the main 3D view), or puts `src`
    /// into the active pane.
    pub fn focus(&mut self, src: Src) {
        let shown = self.shown();
        let hit = shown.iter().copied().find(|&i| self.panes[i].src == src).or_else(|| {
            let any_3d = src == Src::D3(self.main);
            shown.iter().copied().find(|&i| any_3d && matches!(self.panes[i].src, Src::D3(_)))
        });
        match hit {
            Some(i) => self.active = i,
            None => self.set_src(self.active, src),
        }
    }

    /// New split: the active pane stays on screen, and the 3D view too when there is room.
    pub fn set_layout(&mut self, l: Layout) {
        self.layout = l;
        self.max = None;
        let n = l.count();
        if self.active >= n {
            self.panes.swap(self.active, n - 1);
            self.active = n - 1;
        }
        if n >= 2 {
            let on_screen = (0..n).any(|j| matches!(self.panes[j].src, Src::D3(_)));
            let off = (n..4).find(|&j| matches!(self.panes[j].src, Src::D3(_)));
            if let Some(j) = off.filter(|_| !on_screen) {
                let k = if self.active == n - 1 { n - 2 } else { n - 1 };
                self.panes.swap(j, k);
            }
        }
    }

    /// Maximizes the active pane, or restores the split.
    pub fn toggle_max(&mut self) {
        self.max = match self.max {
            Some(_) => None,
            None => Some(self.active),
        };
    }

    /// Graphics of `base` with detail level `d`, kept while `base` stays the same layer.
    fn gfx_for(&mut self, i: usize, base: &Arc<GfxLayer>, d: DetailLevel) -> Arc<GfxLayer> {
        let key = (Arc::as_ptr(base) as usize, base.stamp, d);
        if let Some((p, s, k, g)) = &self.gfx[i] {
            if (*p, *s, *k) == key {
                return g.clone();
            }
        }
        let mut g = (**base).clone();
        Arc::make_mut(&mut g.paint).detail = d;
        let g = Arc::new(g);
        self.gfx[i] = Some((key.0, key.1, d, g.clone()));
        g
    }
}

/// What the pane header's menu picked.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pick {
    Style(VisualStyle),
    Detail(Option<DetailLevel>),
    Layout(Layout),
    Off,
}

/// What the panes did this frame, for the shell.
pub struct Out {
    /// Where the plan and the 3D view were drawn (`NOTHING` when not shown).
    pub plan: Rect,
    pub d3: Rect,
    /// "Create compass elevations" was pressed in an empty pane.
    pub compass: bool,
}

impl Default for Out {
    fn default() -> Self {
        Out {
            plan: Rect::NOTHING,
            d3: Rect::NOTHING,
            compass: false,
        }
    }
}

/// Everything a pane draws with.
pub struct Cx<'a> {
    pub panes: &'a mut Panes,
    pub plan: &'a mut crate::plan_view::PlanView,
    pub view: &'a mut crate::viewport::Viewport,
    pub scene: &'a ddc_render::Scene,
    pub ed: &'a mut Editor,
    pub t: &'a Tokens,
    pub cat: &'a Catalog,
}

/// Header height of a pane: one control with a small margin.
pub fn head_h(th: &ddc_ui::Theme) -> f32 {
    th.d().control + 2.0 * space::XS
}

/// Source list of the header dropdown: the plan of every level, the open 3D views and a new
/// one while there is room, then the sections and elevations.
fn sources(ed: &Editor, panes: &Panes) -> Vec<Src> {
    let mut v: Vec<Src> = ed.levels().iter().map(|l| Src::Plan(l.0)).collect();
    v.extend((0..CAMS).filter(|&k| panes.cam_exists(k)).map(Src::D3));
    v.extend(panes.new_cam().map(Src::D3));
    v.extend(ed.views.iter().map(|(id, _)| Src::View(*id)));
    v
}

/// Header label of a source: "Plan · level", "3D" (numbered once there are several),
/// "New 3D view", the view name.
pub fn src_label(ed: &Editor, panes: &Panes, cat: &Catalog, s: Src) -> String {
    match s {
        Src::Plan(l) => {
            let name = ed.levels().into_iter().find(|x| x.0 == l).map(|x| x.1).unwrap_or_default();
            cat.tf("view_cube.src.plan", &[("level", &name)])
        }
        Src::D3(k) if !panes.cam_exists(k) => cat.t("view_cube.src.d3new"),
        Src::D3(k) => {
            let many = (0..CAMS).filter(|&j| panes.cam_exists(j)).count() > 1;
            if many {
                cat.tf("view_cube.src.d3n", &[("n", &(k + 1).to_string())])
            } else {
                cat.t("view_cube.src.d3")
            }
        }
        Src::View(id) => ed.view_spec(id).map(display_name).unwrap_or_default(),
        Src::Empty => cat.t("view_cube.src.pick"),
    }
}

/// Draws the panes into `area`.
pub fn show(ui: &mut egui::Ui, area: Rect, cx: &mut Cx<'_>) -> Out {
    let mut out = Out {
        plan: Rect::NOTHING,
        d3: Rect::NOTHING,
        compass: false,
    };
    let th = ddc_ui::theme::of(ui.ctx());
    // the lines between panes are the area's own fill showing through
    ui.painter().rect_filled(area, 0.0, cx.t.border);
    let lay = cx.panes.layout_rects(area);
    let many = lay.len() > 1;
    let hh = head_h(&th);
    cx.panes.drawn.clear();
    let pressed = ui.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    for (i, r) in &lay {
        if pressed.is_some_and(|p| r.contains(p)) {
            cx.panes.active = *i;
        }
    }
    cx.panes.activate(cx.ed, cx.plan, cx.view);
    for (i, r) in lay {
        let head = Rect::from_min_max(r.min, pos2(r.max.x, (r.min.y + hh).min(r.max.y)));
        let body = Rect::from_min_max(pos2(r.min.x, head.max.y), r.max);
        cx.panes.drawn.push((i, r, head));
        header(ui, &th, i, head, many, cx);
        if body.height() < 1.0 {
            continue;
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            ui.set_clip_rect(body);
            pane_body(ui, &th, i, body, cx, &mut out);
        });
        if many && i == cx.panes.active {
            ui.painter().rect_stroke(
                r,
                0.0,
                egui::Stroke::new(1.5, cx.t.accent),
                egui::StrokeKind::Inside,
            );
        }
    }
    out
}

fn header(ui: &mut egui::Ui, th: &ddc_ui::Theme, i: usize, head: Rect, many: bool, cx: &mut Cx<'_>) {
    let p = ui.painter_at(head);
    p.rect_filled(head, 0.0, cx.t.panel);
    p.hline(head.x_range(), head.bottom() - 0.5, egui::Stroke::new(1.0, cx.t.border));
    let bg = ui.interact(head, ui.id().with(("view4.head", i)), egui::Sense::click());
    if bg.double_clicked() {
        cx.panes.active = i;
        cx.panes.toggle_max();
    }
    let srcs = sources(cx.ed, cx.panes);
    let label = |s: Src| src_label(cx.ed, cx.panes, cx.cat, s);
    let labels: Vec<String> = srcs.iter().map(|s| label(*s)).collect();
    let cur = cx.panes.panes[i].src;
    let closed = label(cur);
    let max_tip = cx.cat.t(if cx.panes.max.is_some() {
        "view_cube.pane.restore"
    } else {
        "view_cube.pane.max"
    });
    let menu_tip = cx.cat.t("view_cube.pane.menu");
    let inner = head.shrink2(egui::vec2(space::XS, space::XS));
    let mut pick = None;
    let mut max = false;
    let mut chosen = None;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.set_clip_rect(head);
            ui.spacing_mut().item_spacing.x = space::XS;
            // two icon buttons (control + 4 square each) and the gaps before and between them
            let glyphs = 2.0 * (th.d().control + 4.0) + 2.0 * space::XS;
            let w = (inner.width() - glyphs).clamp(th.d().control, 240.0);
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let mut sel = srcs.iter().position(|s| *s == cur).unwrap_or(srcs.len());
            let r = widgets::field::dropdown_shown(
                ui,
                th,
                ("view_cube.src", i),
                &mut sel,
                &refs,
                Some(w),
                None,
                None,
                Some(closed.as_str()),
            );
            if r.changed() {
                chosen = srcs.get(sel).copied();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let icon = if many { "maximize" } else { "tile" };
                if widgets::glyph_button(ui, icon, true, &max_tip).clicked() {
                    max = true;
                }
                let m = widgets::glyph_button(ui, "layout-settings", true, &menu_tip);
                egui::Popup::menu(&m)
                    .gap(space::XS)
                    .show(|ui| pick = menu(ui, th, cx, i));
            });
        },
    );
    if let Some(s) = chosen {
        cx.panes.set_src(i, s);
        cx.panes.active = i;
    }
    if max {
        cx.panes.active = i;
        cx.panes.toggle_max();
    }
    match pick {
        Some(Pick::Style(s)) => {
            // the 3D view draws with the camera's style; the shell keeps the two equal
            cx.panes.panes[i].style = s;
            if cx.panes.panes[i].src == Src::D3(cx.panes.main) {
                cx.view.nav.style = s;
            }
        }
        Some(Pick::Detail(d)) => cx.panes.panes[i].detail = d,
        Some(Pick::Layout(l)) => {
            cx.panes.active = i;
            cx.panes.set_layout(l);
        }
        Some(Pick::Off) => cx.panes.on = false,
        None => {}
    }
}

/// The header menu: display style of this pane, the split, back to a single view.
fn menu(ui: &mut egui::Ui, th: &ddc_ui::Theme, cx: &Cx<'_>, i: usize) -> Option<Pick> {
    let mut pick = None;
    let pane = cx.panes.panes[i];
    let tone = MenuTone::Normal;
    let mut row = |ui: &mut egui::Ui, label: &str, on: bool, enabled: bool, p: Pick| {
        let icon = on.then_some("check");
        if widgets::menu_item_icon(ui, th, icon, label, "", tone, enabled).clicked() {
            pick = Some(p);
            ui.close();
        }
    };
    widgets::menu::menu_section(ui, th, &cx.cat.t("view_cube.menu.style"));
    match pane.src {
        Src::D3(_) => {
            for s in VisualStyle::ALL {
                let label = cx.cat.t(&format!("v3d.style.{}", s.key()));
                row(ui, &label, pane.style == s, true, Pick::Style(s));
            }
        }
        Src::View(_) => {
            let label = cx.cat.t("view_cube.detail.template");
            row(ui, &label, pane.detail.is_none(), true, Pick::Detail(None));
            for d in DetailLevel::ALL {
                let label = cx.cat.t(&format!("view_gfx.detail.{}", d.key()));
                row(ui, &label, pane.detail == Some(d), true, Pick::Detail(Some(d)));
            }
        }
        Src::Plan(_) | Src::Empty => {
            let label = cx.cat.t("view_cube.detail.plan");
            row(ui, &label, false, false, Pick::Detail(None));
        }
    }
    widgets::menu_divider(ui, th);
    widgets::menu::menu_section(ui, th, &cx.cat.t("view_cube.menu.layout"));
    for l in Layout::ALL {
        let label = cx.cat.t(l.key());
        let on = cx.panes.max.is_none() && cx.panes.layout == l;
        row(ui, &label, on, true, Pick::Layout(l));
    }
    widgets::menu_divider(ui, th);
    let label = cx.cat.t("view_cube.menu.off");
    row(ui, &label, false, true, Pick::Off);
    pick
}

fn pane_body(ui: &mut egui::Ui, th: &ddc_ui::Theme, i: usize, body: Rect, cx: &mut Cx<'_>, out: &mut Out) {
    let active = cx.panes.active == i;
    match cx.panes.panes[i].src {
        Src::Plan(_) if cx.panes.draws_live_plan(i, cx.ed.level) => {
            out.plan = body;
            cx.plan.show(ui, cx.ed, cx.t);
            let level = cx.ed.level;
            let plan = &*cx.plan;
            crate::ai::ui::point_marks::overlay(ui, false, level, body, &|a| {
                Some(plan.screen_at(body, ddc_geom::math::v2(a[0], a[1])))
            });
        }
        Src::Plan(l) => {
            let sv = &mut cx.panes.sviews[i];
            sv.set_plan(Some(l));
            sv.show(ui, cx.ed, crate::section_view::PLAN_ID, cx.t, &cx.plan.b.vis, &mut cx.plan.b.ms);
            if let Some(crate::section_hud::SvAct::Close) = sv.act.take() {
                cx.panes.panes[i].src = Src::Empty;
            }
        }
        Src::D3(k) if k == cx.panes.main => {
            out.d3 = body;
            crate::edit3d::show(ui, cx.view, cx.scene, cx.ed);
            if active {
                crate::mass_tools::keys_3d(ui, cx.ed);
            }
            crate::topo::overlay3d(cx.ed, cx.view, ui);
            let level = cx.ed.level;
            let vp = &*cx.view;
            crate::ai::ui::point_marks::overlay(ui, true, level, body, &|a| {
                vp.screen_of(ddc_render::glam::Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32))
            });
        }
        Src::D3(k) => {
            // a view that is not the main one: its own camera, style, section box and frame,
            // the editor's tools and work plane lent for the frame
            let mut v = cx.panes.cam(k, cx.view);
            v.nav.style = cx.panes.panes[i].style;
            std::mem::swap(&mut v.e3, &mut cx.view.e3);
            crate::edit3d::show(ui, &mut v, cx.scene, cx.ed);
            std::mem::swap(&mut v.e3, &mut cx.view.e3);
            cx.panes.cams[k] = Some(v);
        }
        Src::View(id) if cx.ed.view_spec(id).is_some() => {
            let base = cx.plan.b.vis.gfx.clone();
            let own = cx.panes.panes[i]
                .detail
                .filter(|d| *d != base.paint.detail)
                .map(|d| cx.panes.gfx_for(i, &base, d));
            if let Some(g) = own {
                cx.plan.b.vis.gfx = g;
            }
            let sv = &mut cx.panes.sviews[i];
            sv.set_plan(None);
            sv.show(ui, cx.ed, id, cx.t, &cx.plan.b.vis, &mut cx.plan.b.ms);
            cx.plan.b.vis.gfx = base;
            match sv.act.take() {
                Some(crate::section_hud::SvAct::ShowOnPlan) => cx.panes.focus(Src::Plan(cx.ed.level)),
                Some(crate::section_hud::SvAct::Close) => cx.panes.panes[i].src = Src::Empty,
                None => {}
            }
        }
        Src::View(_) | Src::Empty => {
            let card = body.shrink(space::LG);
            if !card.is_positive() {
                return;
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
                let (title, hint, action) = (
                    cx.cat.t("view_cube.empty.title"),
                    cx.cat.t("view_cube.empty.hint"),
                    cx.cat.t("view_cube.empty.action"),
                );
                if widgets::empty_state(ui, th, None, &title, &hint, Some(&action)) {
                    out.compass = true;
                    cx.panes.active = i;
                }
            });
        }
    }
}

fn say(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(a.sh.cat.t(key));
}

fn h_panes(a: &mut EditorApp) {
    if a.panes.on {
        a.panes.on = false;
        say(a, "view_cube.panes.off");
    } else {
        a.panes.enable(&a.ed, a.view.nav.style);
        say(a, "view_cube.panes.on");
    }
}

fn h_pane_max(a: &mut EditorApp) {
    if !a.panes.on {
        return say(a, "view_cube.max.no_panes");
    }
    a.panes.toggle_max();
}

pub fn register(app: &mut EditorApp) {
    app.register_command("view.panes", h_panes);
    app.register_command("view.pane_max", h_pane_max);
}

#[cfg(test)]
#[path = "view_panes_tests.rs"]
mod tests;
