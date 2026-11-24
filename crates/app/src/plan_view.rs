// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan (orthogonal top view): grid, walls, openings, draft with joins, snaps, labels.
//! Thin egui layer: translates input into `wall_tool::Event` and draws the `Editor` state.

use crate::cursor_labels::{Prio, Style};
use crate::editor::{Editor, Sel};
use crate::interact::{Btn, Input, PlanAdapter, ViewAdapter};
use crate::opening_tool::Symbol;
use crate::overlay::{self, Grip, Plate, SnapGlyph};
use crate::plan_hud;
use crate::snap::SnapKind;
use crate::ui_kit::Tokens;
use crate::units::{display_length, format_length, Units};
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::{v2, V2};
use ddc_ui::px;
use egui::{self, Color32, Pos2, Sense, Stroke, Vec2};
use std::sync::Arc;

pub struct PlanView {
    /// Welcome card is shown over the plan (no in-canvas start hint).
    pub welcome: bool,
    pub center: V2,
    /// Pixels per meter.
    pub scale: f64,
    /// Cursor in world coordinates (for the status bar).
    pub cursor: Option<V2>,
    last: Option<(Pos2, f64, V2)>,
    /// Canvas rect and window rect of the last frame.
    frame: Option<(egui::Rect, egui::Rect)>,
    /// The chrome is folding: a moved canvas keeps the model under the same pixel.
    pub follow_chrome: bool,
    /// A level switch asked for it: the next frame fits the level when it would be cut off.
    pub fit_if_cut: bool,
    /// Tab cycle over the hits under the cursor (select mode).
    pcycle: crate::plan_cycle::PlanCycle,
    /// Still-hover tip timer.
    htip: crate::plan_hovertip::HoverTip,
    /// Model units at frame time (scale ruler).
    units: Units,
    /// Walls with warnings (plan marker); set by the shell.
    pub warn_walls: Vec<i64>,
    /// Collision boxes in plan: (corner, corner, role 0 = A, 1 = B, 2 = intersection spot).
    pub clash_marks: Vec<(V2, V2, u8)>,
    /// Editing, box selection and Tab, isolation, dimensions (`modify_tools`).
    pub b: crate::modify_tools::Lane,
    /// Selected placed dimension and the drag of its line.
    pub dims: crate::modify_tools::dims::DimSel,
    /// Underlay drawings (.dxf) per level, tracing and recognition.
    pub drawings: crate::drawing::Drawings,
    /// Wall outline edge counter: avoids rebuilding every frame (key is `EdgesStamp`).
    edges: (Option<EdgesStamp>, std::collections::HashMap<EdgeKey, u32>),
    /// Material class per wall type; key is the editor revision and the change counter.
    classes: ((u64, u64), Vec<(i64, usize)>),
    /// Hidden-element masks for `Vis::stash`; key is `edges_stamp`.
    stash_masks: crate::isolate::MaskCache<EdgesStamp>,
    cull: crate::plan_cull::CullCache,
    /// Wall fill and lines tessellated once; key hashes model, view, selection and theme.
    wall_mesh: Option<(u64, Arc<egui::Mesh>)>,
    /// Slabs with masses, and bodies with the underlay: the same kind of cache as `wall_mesh`.
    slab_mesh: Option<(u64, Arc<egui::Mesh>)>,
    body_mesh: Option<(u64, Arc<egui::Mesh>)>,
    /// Off: every layer is painted shape by shape (reference for the cache tests).
    bake: bool,
    /// Circuit wiring of the level (`wire_plan`).
    wires: crate::wire_plan::WireCache,
}

/// Paints the cached mesh on a key hit and says whether the layer must be painted (`run`);
/// `from` is where its shapes start when they are to be baked after painting.
fn bake_begin(
    ui: &egui::Ui,
    painter: &egui::Painter,
    cache: &Option<(u64, Arc<egui::Mesh>)>,
    key: Option<u64>,
) -> (bool, Option<usize>) {
    let hit = key.and_then(|k| cache.as_ref().filter(|m| m.0 == k).map(|m| m.1.clone()));
    if let Some(mesh) = hit {
        painter.add(egui::Shape::Mesh(mesh));
        return (false, None);
    }
    let layer = painter.layer_id();
    let from = key.map(|_| ui.ctx().graphics(|g| g.get(layer).map_or(0, |l| l.next_idx().0)));
    (true, from)
}

fn bake_end(
    ui: &egui::Ui,
    painter: &egui::Painter,
    cache: &mut Option<(u64, Arc<egui::Mesh>)>,
    key: Option<u64>,
    from: Option<usize>,
) {
    if let (Some(k), Some(start)) = (key, from) {
        let mesh = bake_shapes(ui.ctx(), painter.layer_id(), start, painter.clip_rect());
        painter.add(egui::Shape::Mesh(mesh.clone()));
        *cache = Some((k, mesh));
    }
}

impl Default for PlanView {
    fn default() -> Self {
        Self {
            welcome: false,
            center: v2(3.0, 2.0),
            // 80 px/m: fine grid 0.1 m = 8 px, coarse 1 m = 80 px
            scale: 80.0,
            cursor: None,
            last: None,
            frame: None,
            follow_chrome: false,
            fit_if_cut: false,
            pcycle: Default::default(),
            htip: Default::default(),
            units: Units::Metric,
            warn_walls: vec![],
            clash_marks: vec![],
            b: Default::default(),
            dims: Default::default(),
            drawings: crate::drawing::Drawings::new(),
            edges: (None, Default::default()),
            classes: ((u64::MAX, 0), vec![]),
            stash_masks: Default::default(),
            cull: Default::default(),
            wall_mesh: None,
            slab_mesh: None,
            wires: Default::default(),
            body_mesh: None,
            bake: true,
        }
    }
}

/// Moves the shapes painted on `layer` since index `start` into one tessellated mesh.
fn bake_shapes(
    ctx: &egui::Context,
    layer: egui::LayerId,
    start: usize,
    clip: egui::Rect,
) -> Arc<egui::Mesh> {
    let opts = ctx.tessellation_options(|o| *o);
    let tex = ctx.fonts(|f| f.font_image_size());
    let mut tess = egui::epaint::Tessellator::new(ctx.pixels_per_point(), opts, tex, vec![]);
    tess.set_clip_rect(clip);
    let mut mesh = egui::Mesh::default();
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        for i in start..list.next_idx().0 {
            let mut shape = egui::Shape::Noop;
            list.mutate_shape(egui::layers::ShapeIdx(i), |c| {
                shape = std::mem::replace(&mut c.shape, egui::Shape::Noop)
            });
            tess.tessellate_shape(shape, &mut mesh);
        }
    });
    Arc::new(mesh)
}

/// Grid step: no finer than 16 px on screen.
pub fn grid_step(scale: f64) -> f64 {
    [0.1, 0.5, 1.0, 5.0, 10.0, 50.0, 100.0]
        .into_iter()
        .find(|s| s * scale >= 16.0)
        .unwrap_or(100.0)
}

/// Drawn grid steps (fine, coarse = 10 fine): fine is no narrower than 6 px.
pub fn grid_levels(scale: f64) -> (f64, f64) {
    let minor = [0.1, 1.0, 10.0, 100.0]
        .into_iter()
        .find(|s| s * scale >= 6.0)
        .unwrap_or(100.0);
    (minor, minor * 10.0)
}

/// Scale ruler length in the current units: 1·2·5×10ⁿ m or round inches/feet,
/// no longer than 200 px on screen.
pub fn scale_bar_len(scale: f64, u: Units) -> f64 {
    use crate::units::{FOOT, INCH};
    let steps: Vec<f64> = match u {
        Units::Metric => (-3..=4)
            .flat_map(|e| [1.0, 2.0, 5.0].map(|m| m * 10f64.powi(e)))
            .collect(),
        Units::Imperial => [1.0, 2.0, 6.0]
            .map(|i| i * INCH)
            .into_iter()
            .chain([1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0].map(|f| f * FOOT))
            .collect(),
    };
    steps
        .into_iter()
        .filter(|l| l * scale <= 200.0)
        .last()
        .unwrap_or(0.001)
}

/// Ruler label uses the same formatter as properties and status.
fn bar_label(l: f64, u: Units) -> String {
    display_length(l, u)
}

/// Smallest zoom (px per m): a drawing tens of kilometres wide still fits.
const MIN_SCALE: f64 = 0.002;

/// Zoom by `factor` keeping the world point `anchor` under the cursor; returns (center, scale).
impl PlanView {
    /// Plan of the active level as a view adapter; `c` is the canvas center on screen.
    pub fn adapter(&self, ed: &Editor, c: Pos2) -> PlanAdapter {
        PlanAdapter {
            center: self.center,
            scale: self.scale,
            canvas: [c.x as f64, c.y as f64],
            level: ed.level,
            z: ed.elevation,
        }
    }

    /// Where the plan was drawn in the last frame.
    pub fn drawn_rect(&self) -> Option<egui::Rect> {
        self.frame.map(|(r, _)| r)
    }

    /// Pixel of a model point when the plan fills `rect` at the current centre and scale.
    /// Where a model point sits on screen in the last frame (scripted shots click through it).
    pub fn screen_of(&self, p: V2) -> Option<Pos2> {
        self.frame.map(|(r, _)| self.screen_at(r, p))
    }

    pub fn screen_at(&self, rect: egui::Rect, p: V2) -> Pos2 {
        let c = rect.center();
        Pos2::new(
            c.x + ((p.x - self.center.x) * self.scale) as f32,
            c.y - ((p.y - self.center.y) * self.scale) as f32,
        )
    }
}

/// The view center that keeps every model point at the same screen position when the canvas
/// center moves from `c0` to `c1` at `scale` px per metre.
pub fn keep_on_screen(center: V2, scale: f64, c0: Pos2, c1: Pos2) -> V2 {
    v2(
        center.x + (c1.x - c0.x) as f64 / scale,
        center.y - (c1.y - c0.y) as f64 / scale,
    )
}

pub fn zoom_about(center: V2, scale: f64, factor: f64, anchor: V2) -> (V2, f64) {
    let new = (scale * factor).clamp(MIN_SCALE, 2000.0);
    let k = 1.0 - scale / new;
    (center + (anchor - center) * k, new)
}

impl PlanView {
    /// Fit all walls with margins; an empty model gets the default view.
    pub fn fit(&mut self, ed: &Editor) {
        let Some((lo, hi)) = self.content_bounds(ed) else {
            (self.center, self.scale) = (v2(3.0, 2.0), 80.0);
            return;
        };
        self.center = (lo + hi) * 0.5;
        let span = (hi.x - lo.x).max(hi.y - lo.y).max(1.0);
        self.scale = (600.0 / span).clamp(MIN_SCALE, 2000.0);
    }

    /// Fit the content of the level into a canvas of the given size, with a margin.
    pub fn fit_in(&mut self, ed: &Editor, size: Vec2) {
        let Some((lo, hi)) = self.content_bounds(ed) else {
            return;
        };
        self.center = (lo + hi) * 0.5;
        let margin = 48.0;
        let sx = (size.x - 2.0 * margin).max(40.0) as f64 / (hi.x - lo.x).max(0.5);
        let sy = (size.y - 2.0 * margin).max(40.0) as f64 / (hi.y - lo.y).max(0.5);
        self.scale = sx.min(sy).clamp(MIN_SCALE, 2000.0);
    }

    /// Part of the level content lies outside `rect` at the current centre and scale.
    pub fn is_cut(&self, ed: &Editor, rect: egui::Rect) -> bool {
        let Some((lo, hi)) = self.content_bounds(ed) else {
            return false;
        };
        let c = rect.center();
        let at = |p: V2| {
            Pos2::new(
                c.x + ((p.x - self.center.x) * self.scale) as f32,
                c.y - ((p.y - self.center.y) * self.scale) as f32,
            )
        };
        let (a, b) = (at(lo), at(hi));
        let inner = rect.shrink(8.0);
        !(inner.contains(a) && inner.contains(b))
    }

    /// Corners of the box around the walls, curtain lines, slabs, bodies and the underlay.
    fn content_bounds(&self, ed: &Editor) -> Option<(V2, V2)> {
        let it = ed
            .walls
            .iter()
            .flat_map(|w| [w.a, w.b])
            .chain(ed.curtain_lines.iter().flat_map(|l| [l.1, l.2]))
            .chain(
                ed.slabs
                    .iter()
                    .flat_map(|s| s.rings.first().cloned().unwrap_or_default()),
            )
            .chain(
                ed.bodies
                    .iter()
                    .filter(|b| !crate::editor::bodies::is_site(&b.category))
                    .flat_map(|b| b.hull.iter().copied()),
            );
        let under = self.drawings.fit_bbox(ed.level).map(|(a, b)| [a, b]);
        let mut it = it.chain(under.into_iter().flatten());
        let first = it.next()?;
        let (mut lo, mut hi) = (first, first);
        for p in it {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        }
        Some((lo, hi))
    }

    /// Rubber band and release of the drawing-primitive box pick (left to right: window).
    fn pick_box_input(
        &mut self,
        ui: &egui::Ui,
        ed: &mut Editor,
        resp: &egui::Response,
        rect: egui::Rect,
        to_w: &dyn Fn(Pos2) -> V2,
    ) {
        use egui::PointerButton::Primary;
        if resp.drag_started_by(Primary) {
            self.drawings.box_from = resp.interact_pointer_pos();
        }
        let Some(s) = self.drawings.box_from else { return };
        let Some(e) = resp.interact_pointer_pos().or(resp.hover_pos()) else {
            return;
        };
        let mode = crate::selection::BoxMode::from_drag(s.x, e.x);
        if resp.dragged_by(Primary) {
            let sel = ui.visuals().selection;
            let r = egui::Rect::from_two_pos(s, e);
            let p = ui.painter_at(rect);
            let fill = sel.bg_fill.gamma_multiply(if mode == crate::selection::BoxMode::Window { 0.25 } else { 0.12 });
            p.rect_filled(r, 0.0, fill);
            p.rect_stroke(r, 0.0, sel.stroke, egui::StrokeKind::Inside);
        }
        if resp.drag_stopped_by(Primary) {
            self.drawings.box_from = None;
            if (e - s).length() >= 4.0 {
                let m = ui.input(|i| i.modifiers);
                ed.status = Some(self.drawings.pick_box(ed.level, to_w(s), to_w(e), mode, m.shift, m.ctrl));
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ed: &mut Editor, t: &Tokens) {
        self.units = ed.tool.units;
        // typed canvas text is read in the language this view is shown in
        ed.tool.mark = crate::units::DecimalMark::for_ru(crate::ui_is_ru());
        let sense = if canvas_locked(ui.ctx()) { Sense::hover() } else { Sense::click_and_drag() };
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), sense);
        crate::shell::pane_focus::land_canvas(ui.ctx(), &resp);
        // bottom-left HUD: view name, scale, fit chip; the scale bar shares its baseline
        let hud_p = ui.painter().clone();
        let bar_len = scale_bar_len(self.scale, self.units);
        let bar_px = (bar_len * self.scale) as f32;
        let bar_text = bar_label(bar_len, self.units);
        let lvl = ed.level_name();
        let view_name = crate::tr("view_gfx.view.plan", &[("name", lvl.as_str())]);
        let denom = ed.model.anno_scale(ddc_model::AnnoView::Plan(ed.level)).unwrap_or(100.0);
        let scale_txt = plan_hud::view_scale_text(denom, self.scale);
        let fit_txt = crate::tr("plan.hud.fit", &[]);
        let hud = plan_hud::layout(
            rect,
            plan_hud::HudIn {
                name_w: plan_hud::text_w(&hud_p, &view_name, t.text_dim),
                scale_w: plan_hud::text_w(&hud_p, &scale_txt, t.text_dim),
                fit_w: plan_hud::text_w(&hud_p, &fit_txt, t.text_dim),
                bar_px,
                bar_label_w: plan_hud::text_w(&hud_p, &bar_text, t.text),
            },
        );
        if plan_hud::shown(hud.scale) {
            let r = ui.interact(hud.scale, resp.id.with("hud.scale"), Sense::hover());
            r.on_hover_text(crate::tr(
                "plan.hud.axes_tip",
                &[("key", plan_hud::FULL_AXES_KEY.name())],
            ));
        }
        let mut fit_hot = false;
        if plan_hud::shown(hud.fit) {
            let r = ui.interact(hud.fit, resp.id.with("hud.fit"), Sense::click());
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, fit_txt.clone())
            });
            fit_hot = r.hovered();
            if r.clicked() {
                self.fit(ed);
            }
            r.on_hover_text(crate::tr("cmd.view.zoom_fit.desc", &[]));
        }
        let c = rect.center();
        let screen = ui.ctx().screen_rect();
        let resized = match self.frame.replace((rect, screen)) {
            Some((r0, s0)) => {
                // only a plan that was fully in view is refitted: a zoomed-in one stays
                let was_whole = !ed.walls.is_empty() && !self.is_cut(ed, r0);
                // chrome folded or opened in the same window: the model stays where it was on screen
                if self.follow_chrome && s0 == screen && r0 != rect {
                    self.center = keep_on_screen(self.center, self.scale, r0.center(), c);
                }
                was_whole && s0 == screen && r0.size() != rect.size()
            }
            None => false,
        };
        // a level switch or a pane that shrank (tile, split layout) must not cut the plan off
        let asked = std::mem::take(&mut self.fit_if_cut);
        if (asked || resized) && !ed.walls.is_empty() && self.is_cut(ed, rect) {
            self.fit_in(ed, rect.size());
        }
        let c = rect.center();
        let (cx, cy, s) = (self.center.x, self.center.y, self.scale);
        let to_s = |p: V2| Pos2::new(c.x + ((p.x - cx) * s) as f32, c.y - ((p.y - cy) * s) as f32);
        let to_w = |p: Pos2| v2(cx + (p.x - c.x) as f64 / s, cy - (p.y - c.y) as f64 / s);

        // component library card dropped on the plan: place an instance at this point
        use crate::tools::component::{place, HostHint, Pick};
        if resp.dnd_hover_payload::<Pick>().is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
        }
        if let (Some(p), Some(at)) = (resp.dnd_release_payload::<Pick>(), resp.hover_pos()) {
            if let Err(e) = place(ed, &p, to_w(at), HostHint::Auto) {
                ed.status = Some(e);
            }
        }

        // navigation: middle mouse button pans, wheel zooms toward the cursor
        if resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.center = self.center - v2(d.x as f64 / s, -d.y as f64 / s);
        }
        let hover = resp.hover_pos();
        self.cursor = hover.map(to_w);
        if let Some(h) = hover {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll as f64 * 0.0025).exp() * pinch as f64;
            if factor != 1.0 {
                let (c, sc) = zoom_about(self.center, self.scale, factor, to_w(h));
                self.center = c;
                self.scale = sc;
            }
        }
        let snap_r = crate::snap_refine::radius_m(crate::snap_refine::snap_px(), self.scale);
        ed.tool.radius = snap_r;
        ed.otool.radius = snap_r;
        let no_snap = ui.input(|i| i.modifiers.alt);
        if no_snap != ed.tool.no_snap {
            ed.tool.no_snap = no_snap;
            self.last = None;
        }
        ed.tool.grid = grid_step(self.scale);

        // tool input: cursor → `Pointer` via the plan adapter (after the frame's pan and zoom)
        // Move only on real motion/zoom, otherwise the Tab cycle would reset every frame
        let ad = self.adapter(ed, c);
        if let Some(h) = hover {
            let now = (h, self.scale, self.center);
            if self.last != Some(now) {
                ed.input(Input::Move(ad.pointer([h.x, h.y])));
                self.last = Some(now);
            }
        }
        // hidden elements are removed from the per-frame lists: not drawn and not selectable
        self.b
            .vis
            .phase
            .sync(ed, ddc_model::ViewKey::Plan(ed.level));
        self.b.vis.gfx = crate::view_gfx_ui::layer(ed, ddc_model::ViewKey::Plan(ed.level));
        self.b.vis.range =
            crate::range_layer::layer(ed, ed.level, self.scale, &self.b.vis.gfx);
        if let Some(g) = self.b.vis.range.as_ref().and_then(|r| r.gfx.clone()) {
            self.b.vis.gfx = g;
        }
        let key = edges_stamp(ed, &self.b.vis);
        ed.tool.layer.sync(&ed.model, ed.level, ed.revision);
        let total_before = crate::plan_empty::drawn(ed);
        let stash = self.b.vis.stash_cached(ed, key, &mut self.stash_masks);
        if let Some(h) = hover {
            self.pcycle.moved(h.x, h.y);
            let keys_free = crate::ui_kit::canvas_owns_keys(ui.ctx())
                && !ui.ctx().wants_keyboard_input()
                && !ed.tool_armed()
                && !ed.opening_mode
                && !self.b.busy(ed);
            if keys_free {
                let back = ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
                let fwd = !back
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
                if back || fwd {
                    let (vis, r, w) = (&self.b.vis, 8.0 / self.scale, to_w(h));
                    self.pcycle.step(h.x, h.y, back, || {
                        crate::pick_rules::items_here(ed, vis, w, r)
                            .into_iter()
                            .map(|x| x.sel)
                            .collect()
                    });
                }
            }
        }
        let dim_took = self.dim_input(&resp, ed, &to_w);
        let consumed = self.b.input(ui, &resp, ed, &to_w, 8.0 / self.scale);
        if dim_took {
            // the press was on a dimension: no rubber band, no editor press/release
            self.b.drag = None;
        }
        let consumed = consumed || dim_took;
        // press/release: rectangle and opening drag in select mode
        // grips of a picked drawing primitive take the press, drag and click first
        let (grip, grip_st) = if consumed {
            (false, None)
        } else {
            self.drawings
                .grip_pointer(ui, &resp, ed.level, &to_w, 8.0 / self.scale)
        };
        if let Some(m) = grip_st {
            ed.status = Some(m);
        }
        let consumed = consumed || grip;
        let rect_mode =
            matches!(ed.wall_mode(), Mode::Rect | Mode::Select)
                && !ed.opening_mode
                && !consumed
                && !self.drawings.pick_mode;
        if rect_mode && resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                ed.input(Input::Press(ad.pointer([p.x, p.y]), Btn::Left));
            }
        }
        if rect_mode && resp.drag_stopped_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos().or(hover) {
                ed.input(Input::Release(ad.pointer([p.x, p.y]), Btn::Left));
            }
        }
        // snaps to the underlay; a click traces a line or picks in the recognition preview
        let segs = self.drawings.snap_segs(ed.level);
        ed.tool.lp.layers = self.drawings.snap_layers(ed.level);
        crate::wall_pick::sync(ed, segs);
        if let Some(m) = self.drawings.sync_tool(crate::drawing::tool_sig(ed)) {
            ed.status = Some(m);
        }
        // suggestions from another level do not intercept clicks
        let recog_hover =
            hover.and_then(|h| self.drawings.recog_hit(ed.level, to_w(h), 6.0 / self.scale));
        // drawing primitives: a drag picks by box, a double click picks the layer
        if self.drawings.pick_mode && !consumed {
            self.pick_box_input(ui, ed, &resp, rect, &to_w);
        } else {
            self.drawings.box_from = None;
        }
        // RMB while a chain is drawn finishes it (the quick menu opens only with no chain)
        if resp.secondary_clicked() && !consumed && !ed.chain().is_empty() {
            if let Some(p) = resp.interact_pointer_pos() {
                ed.input(Input::Click(ad.pointer([p.x, p.y]), Btn::Right));
            }
        }
        if resp.double_clicked() && self.drawings.pick_mode && !consumed && recog_hover.is_none() {
            if let Some(p) = resp.interact_pointer_pos() {
                let m = ui.input(|i| i.modifiers);
                let tol = 8.0 / self.scale;
                ed.status = Some(self.drawings.pick_layer(ed.level, to_w(p), tol, m.shift, m.ctrl));
            }
        } else if resp.clicked() && !consumed {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = to_w(p);
                if let Some((id, _)) = &recog_hover {
                    if let Some(r) = self.drawings.recog.as_mut() {
                        r.toggle(*id);
                    }
                } else if self.drawings.draw_click(ed.level, w, 8.0 / self.scale) {
                } else if self.drawings.pick_click(ed.level, w, 8.0 / self.scale) {
                } else if !(crate::drawing::wall_tool_active(ed)
                    && self.drawings.trace_click(ed, w, 8.0 / self.scale))
                {
                    // a running session (modify) takes the click, not the Tab cycle's choice
                    let chosen = self.pcycle.chosen().filter(|_| !crate::interact::sessions::owns_selection(ed));
                    if let Some(s) = chosen {
                        let m = ui.input(|i| i.modifiers);
                        ed.selection =
                            crate::selection::combine(&ed.selection, &[s], m.command, m.shift);
                    } else {
                        crate::pick_rules::click(ed, &self.b.vis, w);
                    }
                    self.pcycle.reset();
                }
            }
        }
        let free = crate::ui_kit::canvas_owns_keys(ui.ctx());
        let dim_keys = self.dims.id.is_some() || self.dims.drag.is_some();
        if free && !ui.ctx().wants_keyboard_input() && !self.b.busy(ed) && dim_keys {
            let (del, esc) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            if del {
                if let Err(m) = self.dims.delete(ed) {
                    ed.say(crate::status::Severity::Refusal, m);
                }
            }
            if esc {
                self.dims.escape();
            }
        }
        if free && !ui.ctx().wants_keyboard_input() && !self.b.active() {
            if let Some(m) = self.drawings.grip_keys(ui) {
                ed.status = Some(m);
            }
            self.keys(ui, ed);
        }

        let painter = ui.painter_at(rect);
        let ov = t.ov();
        painter.rect_filled(rect, 0.0, t.paper);
        // grid: fine and coarse (×10), X/Y axes through the origin
        let (minor, major) = grid_levels(self.scale);
        let (w0, w1) = (to_w(rect.left_bottom()), to_w(rect.right_top()));
        // the fine grid fades in between 6 and 24 px step: levels change without a jump
        let step_px = (minor * self.scale) as f32;
        let fade = ((step_px - 6.0) / 18.0).clamp(0.0, 1.0);
        let ppp = painter.ctx().pixels_per_point();
        let fine = px::stroke(ppp, t.grid_minor.gamma_multiply(fade));
        let bold = px::stroke(ppp, t.grid_major);
        let is_major = |v: f64| ((v / major).round() * major - v).abs() < minor * 1e-3;
        let (mut gx, mut n) = ((w0.x / minor).floor() * minor, 0);
        let grid_on = ed.tool.grid_on;
        while grid_on && gx <= w1.x && n < 600 {
            let st = if is_major(gx) { bold } else { fine };
            if st.color.a() > 0 {
                painter.vline(px::snap(to_s(v2(gx, 0.0)).x, ppp), rect.y_range(), st);
            }
            gx += minor;
            n += 1;
        }
        let (mut gy, mut n) = ((w0.y / minor).floor() * minor, 0);
        while grid_on && gy <= w1.y && n < 600 {
            let st = if is_major(gy) { bold } else { fine };
            if st.color.a() > 0 {
                painter.hline(rect.x_range(), px::snap(to_s(v2(0.0, gy)).y, ppp), st);
            }
            gy += minor;
            n += 1;
        }
        let o = to_s(v2(0.0, 0.0));
        // short origin cross; full lines only while the key is held
        let full_axes = plan_hud::full_axes(
            ed.axes_pref,
            ui.input(|i| i.key_down(plan_hud::FULL_AXES_KEY)),
            ui.ctx().wants_keyboard_input(),
        );
        plan_hud::paint_axes(&painter, rect, o, t, full_axes);
        crate::topo::paint(&painter, rect, ed, t, &to_s);
        crate::clip_tools::paint(&painter, t, &to_s);
        let mut arrows: Vec<(Pos2, Pos2)> = vec![];
        // slabs under walls: dashed outline; sloped ones get a slope arrow with a label
        // hover asks the click's own query: what lights up is what a click takes
        let hot = hover
            .filter(|_| !ed.tool_armed() && !ed.opening_mode)
            .and_then(|h| crate::pick_rules::hover_cached(ed, &self.b.vis, to_w(h)));
        let hot = self.pcycle.chosen().or(hot);
        let hot_cw = hot.filter(|s| matches!(s, Sel::Slab(_) | Sel::Curtain(_) | Sel::Panel(..)));
        let cls = crate::plan_range::classes(ed);
        // slabs of the level below that the view range reaches: halftone dashed outline
        let below_st = Stroke::new(t.lw[2], t.beyond.gamma_multiply(0.8));
        for (rings, class) in &cls.below {
            for r in rings {
                let mut pts: Vec<Pos2> = r.iter().map(|&p| to_s(p)).collect();
                if let Some(&p0) = pts.first() {
                    pts.push(p0);
                }
                if *class == crate::plan_range::RangeClass::Projection {
                    painter.add(egui::Shape::line(pts, Stroke::new(t.lw[1], t.projected)));
                } else {
                    painter.extend(egui::Shape::dashed_line(
                        &pts,
                        below_st,
                        overlay::GHOST_DASH.0,
                        overlay::GHOST_DASH.1,
                    ));
                }
            }
        }
        let selset = crate::plan_cull::SelIndex::new(&ed.selection);
        let cull_key = (
            ed.revision,
            ed.model.sql().total_changes(),
            ed.level as usize,
            ed.walls.len(),
            ed.slabs.len(),
            if self.b.vis.any() { self.b.vis.wall_stamp() } else { 0 },
        );
        self.cull.refresh(cull_key, ed);
        // world view box, grown by the widest stroke/halo so edge items keep their lines
        let view_w = crate::plan_cull::view_bb(to_w(rect.left_top()), to_w(rect.right_bottom()), 12.0 / s);
        // static layers are tessellated once per exact view: same inputs, same shapes, same pixels
        let layer_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (cull_key, self.b.vis.gfx.stamp, &ed.selection, ed.level).hash(&mut h);
            let range = self.b.vis.range.as_ref();
            range.map_or(0, |r| Arc::as_ptr(r) as *const () as usize).hash(&mut h);
            [cx, cy, s, ed.elevation].map(f64::to_bits).hash(&mut h);
            let clip = painter.clip_rect();
            [rect.min, rect.max, clip.min, clip.max]
                .map(|p| [p.x.to_bits(), p.y.to_bits()])
                .hash(&mut h);
            (ppp.to_bits(), t.lw.map(f32::to_bits)).hash(&mut h);
            // `ov` is picked by the paper colour
            (t.paper, t.cut, t.poche, t.projected, t.beyond).hash(&mut h);
            h
        };
        // a slope arrow carries text (font atlas): such plans paint their slabs live
        let sloped = ed.slabs.iter().any(|sl| sl.slope.is_some_and(|p| !p.is_flat()));
        let slab_key = (self.bake && !sloped).then(|| {
            use std::hash::{Hash, Hasher};
            let mut h = layer_key.clone();
            hot_cw.hash(&mut h);
            for (si, sl) in ed.slabs.iter().enumerate() {
                (sl.id, std::mem::discriminant(&cls.slab(si))).hash(&mut h);
            }
            ed.mass_plan.iter().for_each(|(id, rs)| (id, rs.len()).hash(&mut h));
            h.finish()
        });
        let (slab_run, slab_from) = bake_begin(ui, &painter, &self.slab_mesh, slab_key);
        for (si, sl) in ed.slabs.iter().enumerate().filter(|_| slab_run) {
            if !crate::plan_cull::hits(self.cull.slabs.get(si).copied().flatten(), view_w) {
                continue;
            }
            let class = cls.slab(si);
            if class == crate::plan_range::RangeClass::Hidden {
                continue;
            }
            let beyond = class == crate::plan_range::RangeClass::Beyond;
            let sel = selset.contains(&Sel::Slab(sl.id));
            let hot = hot_cw == Some(Sel::Slab(sl.id));
            // below the cut plane: projection line
            // above the cut (beyond): dashed
            let gfx = &self.b.vis.gfx;
            let st = ov.state(
                sel,
                hot,
                Stroke::new(
                    gfx.width(Sel::Slab(sl.id), t.lw[if beyond { 2 } else { 1 }]),
                    gfx.line(Sel::Slab(sl.id), if beyond { t.beyond } else { t.projected }),
                ),
            );
            let col = st.color;
            if !beyond && !sel {
                // light tone so a floor inside walls reads as an area, not lines under the hatch
                if let Some(m) = region_fill(&sl.rings, &sl.cuts, &to_s, slab_tone(t)) {
                    painter.add(m);
                }
            }
            for r in &sl.rings {
                let mut pts: Vec<Pos2> = r.iter().map(|&p| to_s(p)).collect();
                if let Some(&p0) = pts.first() {
                    pts.push(p0);
                }
                if beyond {
                    painter.extend(egui::Shape::dashed_line(
                        &pts,
                        st,
                        overlay::GHOST_DASH.0,
                        overlay::GHOST_DASH.1,
                    ));
                } else {
                    painter.add(egui::Shape::line(pts, st));
                }
            }
            // cutout (shaft, opening): solid outline with a cross
            for c in &sl.cuts {
                let mut pts: Vec<Pos2> = c.iter().map(|&p| to_s(p)).collect();
                if pts.len() < 3 {
                    continue;
                }
                let (lo, hi) = c.iter().fold((c[0], c[0]), |(l, h), p| {
                    (
                        v2(l.x.min(p.x), l.y.min(p.y)),
                        v2(h.x.max(p.x), h.y.max(p.y)),
                    )
                });
                let x = Stroke::new(t.lw[2], col);
                painter.line_segment([to_s(lo), to_s(hi)], x);
                painter.line_segment([to_s(v2(lo.x, hi.y)), to_s(v2(hi.x, lo.y))], x);
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, st));
            }
            if sel {
                if let Some(o) = sl.rings.first() {
                    let pts: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
                    let mesh = egui::Shape::convex_polygon(pts, ov.selection_fill, Stroke::NONE);
                    painter.add(mesh);
                }
            }
            if let (Some(pl), Some(o)) = (sl.slope.filter(|p| !p.is_flat()), sl.rings.first()) {
                arrows.push(slope_arrow(
                    &painter,
                    o,
                    v2(pl.gx, pl.gy),
                    sl.slope_percent(),
                    &to_s,
                    col,
                ));
            }
        }
        // shapes: section at plan height as a thin dash-dot line, the selected one in the accent color
        for (id, rs) in ed.mass_plan.iter().filter(|_| slab_run) {
            let sel = selset.contains(&Sel::Mass(*id));
            let st = ov.state(sel, false, Stroke::new(t.lw[2], t.projected));
            for r in rs {
                let mut pts: Vec<Pos2> = r.iter().map(|&p| to_s(p)).collect();
                if let Some(&p0) = pts.first() {
                    pts.push(p0);
                }
                painter.extend(egui::Shape::dashed_line(&pts, st, 12.0, 3.0));
            }
        }
        bake_end(ui, &painter, &mut self.slab_mesh, slab_key, slab_from);
        // level underlay drawings and recognition preview
        self.drawings
            .paint(&painter, &to_s, rect, self.scale, ed.level, t.text, t.paper);
        self.drawings.paint_recog(
            &painter,
            &to_s,
            ed.level,
            ov.selection,
            t.text_dim.gamma_multiply(0.6),
            recog_hover.as_ref().map(|h| h.0),
        );
        self.drawings
            .paint_pick(&painter, &to_s, ed.level, ov.selection);
        for ring in crate::elem::fitting::selected_rings(ed) {
            painter.add(egui::Shape::closed_line(
                ring.iter().map(|q| to_s(*q)).collect(),
                Stroke::new(2.0, ov.selection),
            ));
        }
        self.drawings
            .paint_grips(&painter, &to_s, ed.level, ov, hover);
        let hover_w = hover.map(to_w);
        let tol = 8.0 / self.scale;
        self.drawings
            .paint_draw(&painter, &to_s, ed.level, ov.selection, hover_w, tol, ov);
        let mut cursor_labels = crate::cursor_labels::CursorLabels::default();
        if let (Some((i, n)), Some(s), Some(h)) = (self.pcycle.label(), self.pcycle.chosen(), hover)
        {
            let cat = crate::selection::category(ed, s);
            let key = format!("flt.cat.{cat}");
            let name = crate::tr(&key, &[]);
            let name = if name == key { cat.to_string() } else { name };
            let (i, n) = (i.to_string(), n.to_string());
            painter.text(
                h + egui::vec2(14.0, 18.0),
                egui::Align2::LEFT_TOP,
                crate::tr("tabcycle.label", &[("name", &name), ("i", &i), ("n", &n)]),
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                t.text,
            );
        }
        {
            let now = ui.input(|i| i.time);
            self.htip.update(hover.map(|h| (h.x, h.y)), now);
            let busy = crate::plan_hovertip::Busy {
                dragging: ed.drag.is_some() || resp.dragged(),
                tool_active: ed.tool_armed() || ed.opening_mode,
                cycle_label: self.pcycle.label().is_some(),
            };
            if let (Some(left), Some(_)) = (self.htip.remaining(now), hover) {
                if left > 0.0 {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_secs_f64(left));
                } else if let (true, Some(s)) = (self.htip.visible(now, busy), hot) {
                    let cat = crate::selection::category(ed, s);
                    let key = format!("flt.cat.{cat}");
                    let name = crate::tr(&key, &[]);
                    let name = if name == key { cat.to_string() } else { name };
                    let text = crate::plan_hovertip::tip_text(
                        &name,
                        &crate::plan_hovertip::type_name(ed, s),
                        crate::plan_hovertip::measure(ed, s),
                        &ed.level_name(),
                        ed.tool.units,
                        crate::ui_lang_now(),
                    );
                    cursor_labels.push(Prio::Tip, Style::Dim(Plate::Normal), text);
                }
            }
        }
        if let (Some((_, label)), Some(h)) = (&recog_hover, hover) {
            painter.text(
                h + egui::vec2(14.0, -14.0),
                egui::Align2::LEFT_BOTTOM,
                crate::tr("edit_panels.plan.recog", &[("label", &label.to_string())]),
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                t.text,
            );
        }
        // level bodies: at the plan's cut plane (view range) fill and outline, below it a thin
        // projection, above it dashed; terrain and masses outline only
        let hot_body = hot.and_then(|s| match s {
            Sel::Body(k, id) => Some((k, id)),
            _ => None,
        });
        let range = crate::plan_range::drawn(ed);
        let body_key = self.bake.then(|| {
            use std::hash::{Hash, Hasher};
            let mut h = layer_key.clone();
            hot_body.hash(&mut h);
            for bd in ed.bodies.iter().chain(&ed.bodies_through) {
                let class = crate::plan_range::classify(&range, ed.elevation, bd.zmin, bd.zmax);
                (bd.kind, bd.id, std::mem::discriminant(&class), bd.hull.len()).hash(&mut h);
                bd.category.hash(&mut h);
            }
            for o in ed.underlay.iter().flatten() {
                o.len().hash(&mut h);
            }
            h.finish()
        });
        let (body_run, body_from) = bake_begin(ui, &painter, &self.body_mesh, body_key);
        for bd in ed.bodies.iter().chain(&ed.bodies_through).filter(|_| body_run) {
            use crate::plan_range::{classify, RangeClass as RC};
            let class = classify(&range, ed.elevation, bd.zmin, bd.zmax);
            if class == RC::Hidden {
                continue;
            }
            let pts: Vec<Pos2> = bd.hull.iter().map(|&p| to_s(p)).collect();
            let bb = egui::Rect::from_points(&pts);
            if !bb.intersects(rect) {
                continue;
            }
            let sel = selset.contains(&Sel::Body(bd.kind, bd.id));
            let hot = hot_body == Some((bd.kind, bd.id));
            // ceilings belong to the reflected plan; imported meshes have their exact cut on the
            // plan layer (mesh_tools): both are outlined only while picked
            use crate::editor::bodies::BodyKind as BK;
            if matches!(bd.kind, BK::Ceiling | BK::Mesh) && !sel && !hot {
                continue;
            }
            let site = crate::editor::bodies::is_site(&bd.category);
            let gl = |base: Color32| self.b.vis.gfx.line(Sel::Body(bd.kind, bd.id), base);
            let mut line = pts.clone();
            line.push(pts[0]);
            if site {
                let st = ov.state(sel, hot, Stroke::new(t.lw[2], gl(t.beyond)));
                painter.add(egui::Shape::line(line, st));
            } else if class == RC::Cut {
                let fill = if sel {
                    ov.selection_fill
                } else {
                    t.poche.gamma_multiply(0.55)
                };
                painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::NONE));
                let st = ov.state(sel, hot, Stroke::new(t.lw[0], gl(t.cut)));
                painter.add(egui::Shape::line(line, st));
            } else if class == RC::Projection {
                if sel {
                    painter.add(egui::Shape::convex_polygon(
                        pts,
                        ov.selection_fill,
                        Stroke::NONE,
                    ));
                }
                let st = ov.state(sel, hot, Stroke::new(t.lw[1], gl(t.projected)));
                painter.add(egui::Shape::line(line, st));
            } else {
                let st = ov.state(sel, hot, Stroke::new(t.lw[2], gl(t.beyond)));
                painter.extend(egui::Shape::dashed_line(
                    &line,
                    st,
                    overlay::GHOST_DASH.0,
                    overlay::GHOST_DASH.1,
                ));
            }
        }
        // underlay: walls of the level below as a pale outline without fill
        let under = Stroke::new(t.lw[2], t.beyond.gamma_multiply(0.6));
        for o in ed.underlay.iter().flatten().filter(|_| body_run) {
            let mut pts: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
            if let Some(&p0) = pts.first() {
                pts.push(p0);
            }
            painter.add(egui::Shape::line(pts, under));
        }
        bake_end(ui, &painter, &mut self.body_mesh, body_key, body_from);
        // walls; under the cursor (select mode) a hover outline, selected ones in the selection color, as in 3D
        let hovered = hot.and_then(|s| match s {
            Sel::Wall(id) => Some(id),
            _ => None,
        });
        // cut: fill (poché) without inner seams, outline only the outer edges with a thick
        // line; selected ones get a soft accent halo under the outline
        let key = |p: V2| ((p.x * 1e5).round() as i64, (p.y * 1e5).round() as i64);
        // `ed.plan` changes when the model is re-read and for a frame without hidden walls (`Vis::stash`)
        let stamp = edges_stamp(ed, &self.b.vis);
        if self.edges.0 != Some(stamp) {
            self.edges.1.clear();
            for o in ed.plan.iter().flatten() {
                for i in 0..o.len() {
                    let (a, b) = (key(o[i]), key(o[(i + 1) % o.len()]));
                    *self
                        .edges
                        .1
                        .entry(if a < b { (a, b) } else { (b, a) })
                        .or_default() += 1;
                }
            }
            self.edges.0 = Some(stamp);
        }
        let shared = &self.edges.1;
        let outer = |a: V2, b: V2| {
            let (a, b) = (key(a), key(b));
            shared.get(&if a < b { (a, b) } else { (b, a) }).copied() == Some(1)
        };
        let halo = Stroke::new(6.0, ov.selection.gamma_multiply(0.22));
        // large model: only walls in the visible area (plan index)
        let on = ed
            .big
            .plan_visible(ed, to_w(rect.left_top()), to_w(rect.right_bottom()));
        let shown = |id: i64| on.as_ref().is_none_or(|s| s.contains(&id));
        // poché and hatching by material class (constant step on screen)
        let class_key = ed.cache_key();
        if self.classes.0 != class_key {
            self.classes = (class_key, wall_classes(ed));
        }
        let classes = &self.classes.1;
        let other = crate::ui_kit::MAT_CLASSES.len() - 1;
        let hatch_st = Stroke::new(0.75, t.cut.gamma_multiply(0.45));
        let in_view = on.as_ref().map_or(ed.walls.len(), |s| s.len());
        let mut hatch_left = if in_view > overlay::HATCH_WALLS_MAX {
            0
        } else {
            overlay::HATCH_FRAME_MAX
        };
        let visible = |i: usize| cls.wall(i) != crate::plan_range::RangeClass::Hidden;
        // walls are the bulk of a big plan: fill and lines stay one mesh until the model,
        // view, selection or theme changes; hover only draws its outline over it
        let wkey = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (cull_key, stamp, class_key, self.b.vis.gfx.stamp, &ed.selection).hash(&mut h);
            let range = self.b.vis.range.as_ref();
            range.map_or(0, |r| Arc::as_ptr(r) as *const () as usize).hash(&mut h);
            [cx, cy, s].map(f64::to_bits).hash(&mut h);
            let clip = painter.clip_rect();
            let ppp = ui.ctx().pixels_per_point();
            [rect.min, rect.max, clip.min, clip.max]
                .map(|p| [p.x.to_bits(), p.y.to_bits()])
                .hash(&mut h);
            (ppp.to_bits(), t.lw.map(f32::to_bits)).hash(&mut h);
            (t.cut, t.poche, t.projected, t.beyond, t.mat, ov.selection).hash(&mut h);
            (0..ed.walls.len()).for_each(|i| std::mem::discriminant(&cls.wall(i)).hash(&mut h));
            h.finish()
        };
        let layer = painter.layer_id();
        let baked = self.wall_mesh.as_ref().filter(|m| m.0 == wkey).map(|m| m.1.clone());
        let bake_from = baked
            .is_none()
            .then(|| ui.ctx().graphics(|g| g.get(layer).map_or(0, |l| l.next_idx().0)));
        if let Some(mesh) = baked {
            painter.add(egui::Shape::Mesh(mesh));
        }
        for (i, (w, outlines)) in ed
            .walls
            .iter()
            .zip(&ed.plan)
            .enumerate()
            .filter(|(i, x)| {
                bake_from.is_some()
                    && shown(x.0.id)
                    && visible(*i)
                    && crate::plan_cull::hits(self.cull.wall(*i), view_w)
            })
        {
            // low or raised walls have no section fill: the line pass draws them
            if cls.wall(i) != crate::plan_range::RangeClass::Cut {
                continue;
            }
            let sel = selset.contains(&Sel::Wall(w.id));
            let class = classes
                .iter()
                .find(|c| c.0 == w.wall_type)
                .map_or(other, |c| c.1);
            // class "other": neutral section poché
            let base = if class == other {
                t.poche
            } else {
                t.mat[class]
            };
            let gfx = &self.b.vis.gfx;
            let fill = ov.state_fill(sel, gfx.fill(Sel::Wall(w.id), base));
            // layer material hatch; without a project entry, by material class
            let pattern = match gfx.wall_hatch(w.wall_type) {
                Some(k) => crate::view_gfx_ui::hatch_of_key(k),
                None if gfx.coarse() => overlay::Hatch::None,
                None => overlay::hatch_of_class(class),
            };
            for o in outlines {
                let pts: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
                if sel {
                    for i in 0..o.len() {
                        if outer(o[i], o[(i + 1) % o.len()]) {
                            painter.line_segment([pts[i], pts[(i + 1) % pts.len()]], halo);
                        }
                    }
                }
                painter.add(egui::Shape::convex_polygon(
                    pts.clone(),
                    fill,
                    Stroke::new(0.75, fill),
                ));
                overlay::hatch(&painter, &pts, pattern, hatch_st, rect, &mut hatch_left);
            }
        }
        let wall_lines = |i: usize, hot: bool| {
            let (w, outlines) = (&ed.walls[i], &ed.plan[i]);
            let sel = selset.contains(&Sel::Wall(w.id));
            let gfx = &self.b.vis.gfx;
            let w_id = Sel::Wall(w.id);
            let wc = cls.wall(i);
            let (lw, base) = match wc {
                crate::plan_range::RangeClass::Cut => (t.lw[0], t.cut),
                crate::plan_range::RangeClass::Projection => (t.lw[1], t.projected),
                _ => (t.lw[2], t.beyond),
            };
            let st = ov.state(
                sel,
                hot,
                Stroke::new(gfx.width(w_id, lw), gfx.line(w_id, base)),
            );
            let dashed = wc == crate::plan_range::RangeClass::Beyond;
            // high detail level: seams between layers
            let seam = gfx
                .fine()
                .then(|| Stroke::new(gfx.width(w_id, 0.5), st.color));
            for o in outlines {
                for i in 0..o.len() {
                    let (a, b) = (o[i], o[(i + 1) % o.len()]);
                    if outer(a, b) {
                        if dashed {
                            painter.extend(egui::Shape::dashed_line(
                                &[to_s(a), to_s(b)],
                                st,
                                overlay::GHOST_DASH.0,
                                overlay::GHOST_DASH.1,
                            ));
                        } else {
                            if sel {
                                painter.line_segment([to_s(a), to_s(b)], ov.halo(st));
                            }
                            painter.line_segment([to_s(a), to_s(b)], st);
                        }
                    } else if let Some(s) = seam {
                        painter.line_segment([to_s(a), to_s(b)], s);
                    }
                }
            }
        };
        let drawn = |i: usize| {
            shown(ed.walls[i].id)
                && visible(i)
                && crate::plan_cull::hits(self.cull.wall(i), view_w)
        };
        let n_walls = ed.walls.len().min(ed.plan.len());
        if let Some(start) = bake_from {
            (0..n_walls).filter(|&i| drawn(i)).for_each(|i| wall_lines(i, false));
            let mesh = bake_shapes(ui.ctx(), layer, start, painter.clip_rect());
            painter.add(egui::Shape::Mesh(mesh.clone()));
            self.wall_mesh = Some((wkey, mesh));
        }
        if let Some(i) = hovered.and_then(|id| (0..n_walls).find(|&i| ed.walls[i].id == id)) {
            if drawn(i) {
                wall_lines(i, true);
            }
        }
        // rooms: the selected one and the one under the cursor get an accent fill and outline
        let hot_room = hot.and_then(|s| match s {
            Sel::Room(id) => Some(id),
            _ => None,
        });
        for r in &ed.rooms {
            let sel = selset.contains(&Sel::Room(r.id));
            if !sel && hot_room != Some(r.id) {
                continue;
            }
            let Some(o) = r.rings.first() else {
                continue;
            };
            let pts: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
            if sel && convex(o) {
                painter.add(egui::Shape::convex_polygon(
                    pts.clone(),
                    ov.selection_fill,
                    Stroke::NONE,
                ));
            }
            let mut line = pts;
            line.push(line[0]);
            let st = ov.state(sel, !sel, Stroke::new(overlay::OUTLINE_PX, ov.hover));
            painter.add(egui::Shape::line(line, st));
        }
        // curtain walls: conventional symbol from the model's views (faces and mullion ticks)
        // a panel is selected: the wall is muted, the panel cell gets the accent
        let sel_panel = ed.selection.iter().find_map(|s| match *s {
            Sel::Panel(c, k) => Some((c, k)),
            _ => None,
        });
        for &(id, a, b) in &ed.curtain_lines {
            let sel = ed
                .selection
                .iter()
                .any(|s| matches!(s, Sel::Curtain(c) if *c == id));
            let dimmed = sel_panel.is_some_and(|(c, _)| c == id);
            let hot = hot_cw == Some(Sel::Curtain(id));
            let st = if dimmed {
                Stroke::new(t.lw[1], t.beyond)
            } else {
                ov.state(sel, hot, Stroke::new(t.lw[1], t.cut))
            };
            if sel && !dimmed {
                painter.line_segment([to_s(a), to_s(b)], ov.halo(st));
            }
            painter.line_segment([to_s(a), to_s(b)], st);
        }
        if let Some((cell, _)) = sel_panel.and_then(|(c, k)| {
            ed.curtain(c)
                .and_then(|cw| crate::editor::cw::panel_of(cw, k))
        }) {
            let (a, b) = (cell.corners[0], cell.corners[1]);
            let (a, b) = (to_s(v2(a.x, a.y)), to_s(v2(b.x, b.y)));
            painter.line_segment([a, b], ov.halo(Stroke::new(overlay::OUTLINE_PX, ov.selection)));
            painter.line_segment([a, b], Stroke::new(overlay::OUTLINE_PX, ov.selection));
            let n = (b - a).normalized().rot90() * 6.0;
            for p in [a, b] {
                painter.line_segment([p - n, p + n], Stroke::new(overlay::GHOST_PX, ov.selection));
            }
        }
        // section and elevation marks, draft of the "Section" tool
        crate::section_view::plan_marks(
            &painter,
            ed,
            &to_s,
            t,
            1.0 / self.scale,
            hover.map(to_w),
            ov.preview,
        );
        // selected wall: temporary dimension (length and angle) and end handles
        // model center on screen: the dimension is offset outward, away from it
        if !ed.tool_armed() && !ed.selection.is_empty() {
            let centroid = {
                let n = (ed.walls.len() * 2).max(1) as f64;
                let c = ed.walls.iter().fold(v2(0.0, 0.0), |acc, w| acc + w.a + w.b) * (1.0 / n);
                to_s(c)
            };
            for w in ed
                .walls
                .iter()
                .filter(|w| selset.contains(&Sel::Wall(w.id)))
            {
                let (a, b) = (to_s(w.a), to_s(w.b));
                let d = b - a;
                if d.length() < 8.0 {
                    continue;
                }
                let mut n = Vec2::new(d.y, -d.x).normalized();
                // dimension line outside: normal pointing away from the model center (not into the room)
                if (a + d * 0.5 - centroid).dot(n) < 0.0 {
                    n = -n;
                }
                let half = ed
                    .types
                    .iter()
                    .find(|ty| ty.id == w.wall_type)
                    .map_or(0.1, |ty| ty.thickness() / 2.0);
                let off = n * ((half * s) as f32 + 22.0);
                let (da, db) = (a + off, b + off);
                let ext = Stroke::new(overlay::THIN_PX, t.text_dim);
                for (p, q) in [(a, da), (b, db)] {
                    painter.line_segment([p + n * ((half * s) as f32 + 4.0), q + n * 4.0], ext);
                }
                overlay::dim_line(&painter, da, db, t.text_dim);
                let wd = w.b - w.a;
                let ang = wd.y.atan2(wd.x).to_degrees().rem_euclid(360.0);
                let label = format!(
                    "{} · {}°",
                    display_length(wd.len(), ed.tool.units),
                    crate::units::fmt_num(ang, 1)
                );
                overlay::dim_label(&painter, da + (db - da) * 0.5, label, Plate::Normal, ov);
                for p in [a, b] {
                    overlay::grip(&painter, p, Grip::Square, false, ov);
                }
            }
        }
        // selected bodies (a few): live clearances to the faces of the walls on screen
        let picked: Vec<_> = if !ed.tool_armed() && !ed.selection.is_empty() {
            let max = crate::clearance::MAX_BODIES;
            ed.bodies
                .iter()
                .filter(|b| selset.contains(&Sel::Body(b.kind, b.id)))
                .take(max + 1)
                .collect()
        } else {
            vec![]
        };
        if (1..=crate::clearance::MAX_BODIES).contains(&picked.len()) {
            let faces = crate::clearance::faces(
                ed.walls
                    .iter()
                    .zip(&ed.plan)
                    .enumerate()
                    .filter(|(i, (w, _))| {
                        shown(w.id)
                            && visible(*i)
                            && crate::plan_cull::hits(self.cull.wall(*i), view_w)
                    })
                    .map(|(_, (_, o))| o),
            );
            for bd in picked {
                let Some((lo, hi)) = bd.hull.iter().fold(None, |m: Option<(V2, V2)>, q| {
                    Some(m.map_or((*q, *q), |(l, h)| {
                        (v2(l.x.min(q.x), l.y.min(q.y)), v2(h.x.max(q.x), h.y.max(q.y)))
                    }))
                }) else {
                    continue;
                };
                crate::clearance::paint(
                    &painter, &to_s, lo, hi, &faces, ed.tool.units, t.text_dim, ov,
                );
            }
        }
        // openings: clear gap in the background color, then the symbol
        let draw_sym = |sym: &Symbol, color: Color32| {
            let gap: Vec<Pos2> = sym.gap.iter().map(|&p| to_s(p)).collect();
            painter.add(egui::Shape::convex_polygon(gap, t.paper, Stroke::NONE));
            // jambs and leaf use the main line; the swing arc and glass are thin
            let door = sym.lines.iter().any(|l| l.len() > 2);
            for (i, l) in sym.lines.iter().enumerate() {
                let pts: Vec<Pos2> = l.iter().map(|&p| to_s(p)).collect();
                let st = match (i, l.len()) {
                    (0 | 1, _) => Stroke::new(t.lw[1], color),
                    (_, n) if n > 2 => Stroke::new(t.lw[2], color.gamma_multiply(0.7)),
                    _ if door => Stroke::new(t.lw[1], color),
                    _ => Stroke::new(t.lw[2], color),
                };
                painter.add(egui::Shape::line(pts, st));
            }
        };
        for (id, sym) in ed.opening_symbols() {
            let sel = selset.contains(&Sel::Opening(id));
            let bad = ed
                .drag
                .as_ref()
                .is_some_and(|d| d.id == id && d.error.is_some());
            let color = match (bad, sel) {
                (true, _) => ov.error,
                (_, true) => ov.selection,
                _ => self.b.vis.gfx.line(Sel::Opening(id), t.cut),
            };
            draw_sym(&sym, color);
        }
        if let Some((sym, err)) = ed.opening_preview() {
            let bad = err.is_some();
            draw_sym(&sym, if bad { ov.error } else { ov.preview });
            if let (Some(_), Some(pv)) = (hover, ed.otool.preview.as_ref()) {
                let label = err.unwrap_or_else(|| {
                    format!(
                        "{} · {}",
                        format_length(pv.along, ed.tool.units),
                        pv.snap.label()
                    )
                });
                let kind = if bad { Plate::Error } else { Plate::Temp };
                cursor_labels.push(Prio::Tool, Style::Dim(kind), label);
            }
        }
        // phase halftone: over elements, under handles, the draft and tools
        crate::phase_ui::paint_plan(&painter, ed, &self.b.vis.phase, &to_s, t);
        crate::range_layer::paint_plan(&painter, self.b.vis.range.as_deref(), &to_s, t);
        let wires = self.wires.get(ed);
        crate::wire_plan::paint_plan(&painter, &wires, &self.b.vis.gfx.paint.hidden_cats, &to_s, t);
        crate::wp_views::paint_plan(&painter, ed, rect, &to_s, &to_w, t);
        // order: layer elements, then labels, handles and the draft, so a label is never crossed by anything
        crate::tools::paint_layer(&painter, ed, t, &to_s);
        room_labels(&painter, ed, t, &to_s, s, &arrows);
        if !ed.tool_armed() {
            crate::grips::paint(&painter, ed, &to_s, t);
            crate::constraints_ui::toolbar(ui, ed, rect);
        }
        // draft
        for o in ed.preview_plan() {
            let pts: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
            overlay::ghost_poly(&painter, pts, ov);
        }
        let ghost = ov.ghost();
        let draft = ed.draft_preview();
        for &(a, b) in &draft {
            overlay::ghost_line(&painter, to_s(a), to_s(b), ghost);
        }
        let arc = ed.draft_arc();
        for w in arc.windows(2) {
            overlay::ghost_line(&painter, to_s(w[0]), to_s(w[1]), ghost);
        }
        // a running tool session: the primitives the 3D and section views draw too
        let prims = crate::interact::Interaction::preview(ed);
        if !prims.0.is_empty() {
            let units = ed.tool.units;
            let draws = crate::interact::preview::draws(&prims.0, &ad);
            overlay::paint_preview(&painter, &draws, ov, &|v| display_length(v, units));
        }
        if ed.wall_mode() == Mode::Polyline && !ed.tool.arc {
            if let (Some(&(a, b)), Some(_)) = (draft.last(), hover) {
                let chain = ed.chain();
                let prev = chain.len().checked_sub(2).map(|i| chain[i]);
                if let Some(r) = crate::draw_chip::reading(prev, a, b) {
                    let text = crate::draw_chip::chip_text(&r, ed.tool.units, crate::ui_is_ru());
                    cursor_labels.push(Prio::Chip, Style::Dim(Plate::Temp), text);
                }
            }
        }
        if ed.wall_mode() == Mode::Rect && draft.len() == 4 {
            let (a, b) = (draft[0].0, draft[1].1);
            let label = format!(
                "{} × {}",
                format_length((b.x - a.x).abs(), ed.tool.units),
                format_length((b.y - a.y).abs(), ed.tool.units)
            );
            cursor_labels.push(Prio::Tool, Style::Dim(Plate::Temp), label);
        }
        // snap: glyph at the point and a label on a chip, over the draft
        if hover.is_some() {
            for a in &ed.tool.snap_state.acquired {
                overlay::guide_rays(&painter, to_s(a.p), ov);
            }
        }
        if let (Some(sn), Some(_)) = (ed.tool.snap(), hover) {
            if sn.kind.is_align() {
                let src = ed.tool.align.sources(&ed.tool.last.pts, &sn, ed.tool.radius);
                for q in src {
                    overlay::snap_trace(&painter, to_s(q), to_s(sn.p), ov);
                    overlay::snap_glyph(&painter, to_s(q), SnapGlyph::Plus, ov);
                }
            }
            let from = ed.tool.query_from_in(ed.wall_draft()).or(ed.wall_draft().rect_start);
            let (all, i) = (&ed.tool.snaps, ed.tool.snap_idx);
            snap_marker_glyph(&painter, &to_s, sn, from, ov);
            let mut text = snap_text(sn, all, i);
            if let (SnapKind::EqualStep, Some(f)) = (sn.kind, from) {
                text = format!("{text}  = {}", ((sn.p - f).len() * 1000.0).round());
            }
            cursor_labels.push(Prio::Snap, Style::Snap, text);
        }
        if hover.is_some() && ed.tool_armed() {
            crate::snap_refine::paint_tool(&painter, &to_s, &ed.tool, ed.wall_draft(), ov);
        }
        if let Some(h) = hover {
            if ed.tool_armed() {
                painter.circle_filled(to_s(ed.wall_snapped()), 2.5, t.text);
            }
            let input = if ed.opening_mode {
                &ed.otool.input
            } else {
                &ed.wall_draft().input
            };
            if !input.is_empty() {
                cursor_labels.push(Prio::Input, Style::Dim(Plate::Editing), format!("{input}▏"));
            }
            let anchor = if ed.tool_armed() {
                to_s(ed.wall_snapped())
            } else {
                h
            };
            if ed.tool_armed() || ed.opening_mode {
                cursor_labels.drop_tips();
            }
            std::mem::take(&mut cursor_labels).paint(&painter, anchor, rect, ov);
        }
        if resp.hovered() {
            use egui::CursorIcon as C;
            let icon = if resp.dragged_by(egui::PointerButton::Middle) {
                C::Grabbing
            } else if ed.opening_mode || ed.tool_armed() {
                C::Crosshair
            } else if ed.drag.is_some() {
                C::Grabbing
            } else if hot.is_some() {
                C::PointingHand
            } else {
                C::Default
            };
            ui.ctx().set_cursor_icon(icon);
        }
        // the hovered item of a kind without its own hover look: its outline
        if let Some(s) = hot.filter(|s| {
            !matches!(
                s,
                Sel::Wall(_) | Sel::Slab(_) | Sel::Curtain(_) | Sel::Panel(..) | Sel::Room(_)
            ) && !matches!(s, Sel::Body(..))
        }) {
            if !selset.contains(&s) {
                for o in crate::selection::outlines(ed, s) {
                    let mut line: Vec<Pos2> = o.iter().map(|&p| to_s(p)).collect();
                    if let Some(&p0) = line.first() {
                        line.push(p0);
                    }
                    painter.add(egui::Shape::line(
                        line,
                        Stroke::new(overlay::OUTLINE_PX, ov.hover),
                    ));
                }
            }
        }
        crate::tools::paint(ui, &painter, rect, ed, t, &to_s);
        crate::shape_edit::paint(&painter, rect, ed, t, &to_s);
        crate::sketch_mode::paint(&painter, rect, ed, t, &to_s);
        crate::sketch_mode::bar_buttons(ui, rect, ed);
        crate::boundary_link_ui::paint_warnings(&painter, ed, t, &to_s);
        crate::ai::preview::paint_plan(&painter, rect, ed, t, &to_s);
        crate::solid_tools::paint(ui, &painter, rect, ed, t, &to_s);
        let anno_view = ddc_model::AnnoView::Plan(ed.level);
        crate::annot_text::draw(ui, &painter, rect, ed, t, &to_s, anno_view);
        warn_markers(&painter, ed, &self.warn_walls, to_s, ov);
        for (lo, hi, role) in &self.clash_marks {
            let col = [ov.warning, ov.selection, ov.error][(*role).min(2) as usize];
            let r = egui::Rect::from_two_pos(to_s(*lo), to_s(*hi)).expand(if *role == 2 {
                6.0
            } else {
                2.0
            });
            painter.rect_stroke(
                r,
                2.0,
                Stroke::new(if *role == 2 { 2.5 } else { 1.5 }, col),
                egui::StrokeKind::Outside,
            );
        }
        plan_hud::paint_chips(&painter, &hud, &view_name, &scale_txt, &fit_txt, fit_hot, t);
        plan_hud::paint_bar(&painter, hud.bar, bar_px.min(hud.bar.width()), &bar_text, t);
        // in select mode the shell shows the welcome card instead
        let empty = crate::plan_empty::drawn(ed) == 0
            && ed.chain().is_empty()
            && !self.b.vis.any()
            && ed.model_tool().is_none()
            && ed.sketch.is_none()
            && ed.shape.is_none();
        let has_under = ed.underlay.iter().flatten().any(|l| !l.is_empty())
            || !self.drawings.list.is_empty();
        if start_hint_shown(empty, has_under)
            && !ed.opening_mode
            && !self.welcome
            // a dialog (import…) over the canvas: the coach card waits instead of lying under it
            && !crate::ui_kit::window_over(ui.ctx(), rect.shrink2(Vec2::new(rect.width() * 0.25, rect.height() * 0.25)))
        {
            empty_hint(&painter, rect, t, !ed.tool_armed());
        } else if let Some(kind) = self
            .empty_card(ed, total_before)
            .filter(|_| !has_under)
        {
            if crate::plan_empty::show(ui, rect, kind) {
                self.empty_act(ed, kind);
            }
        }
        self.b.draw(ui, &painter, ed, &to_s, rect, t);
        crate::modify_tools::dims::draw_selected(&painter, ed, &self.dims, &to_s, t);
        crate::isolate::Vis::restore(ed, stash);
    }

    /// The card that explains an empty-looking level; only while nothing is being drawn.
    fn empty_card(&self, ed: &Editor, total: usize) -> Option<crate::plan_empty::Kind> {
        let idle = !ed.tool_armed()
            && !ed.opening_mode
            && !self.welcome
            && ed.chain().is_empty()
            && ed.model_tool().is_none()
            && ed.sketch.is_none()
            && ed.shape.is_none();
        let visible = crate::plan_empty::drawn(ed);
        if !idle || visible > 0 {
            return None;
        }
        let v = &self.b.vis;
        let hides = crate::plan_empty::Hides {
            temporary: v.temp_on(),
            phase: !v.phase.hide.is_empty(),
            graphics: !v.gfx.hide.is_empty() || !v.hidden.is_empty(),
        };
        let other = if total == 0 {
            crate::bigfile::first_level(&ed.model).filter(|l| *l != ed.level)
        } else {
            None
        };
        crate::plan_empty::plan_empty_kind(total, visible, hides, other)
    }

    /// The one action of the empty-plan card.
    fn empty_act(&mut self, ed: &mut Editor, kind: crate::plan_empty::Kind) {
        use crate::plan_empty::Kind;
        match kind {
            Kind::Hidden => self.b.vis.temp = crate::isolate::Temp::Off,
            Kind::Graphics => crate::view_gfx_ui::open_vv(),
            Kind::Phase => crate::phase_ui::open_window(),
            Kind::Level => {
                if let Some(l) = crate::bigfile::first_level(&ed.model) {
                    ed.set_level(l);
                }
            }
        }
    }

    /// Placed dimensions in Select mode: a click selects one where no element is, a drag of
    /// its line moves it (one undo step). True: the pointer event belongs to a dimension.
    fn dim_input(
        &mut self,
        resp: &egui::Response,
        ed: &mut Editor,
        to_w: &dyn Fn(Pos2) -> V2,
    ) -> bool {
        use crate::modify_tools::dims;
        self.dims.sync(ed);
        if self.b.busy(ed) || ed.tool_armed() || ed.opening_mode {
            self.dims.drag = None;
            return false;
        }
        let r = 8.0 / self.scale;
        let primary = egui::PointerButton::Primary;
        if resp.drag_started_by(primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = to_w(p);
                let elem = dims::element_at(ed, w, r, |s| self.b.vis.visible(ed, s));
                self.dims.press(ed, w, r, elem);
            }
        }
        if self.dims.drag.is_some() {
            if let Some(h) = resp.hover_pos().or(resp.interact_pointer_pos()) {
                self.dims.moved(to_w(h));
            }
            if resp.drag_stopped_by(primary) {
                if let Err(m) = self.dims.release(ed) {
                    ed.say(crate::status::Severity::Refusal, m);
                }
            }
            return true;
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = to_w(p);
                let elem = dims::element_at(ed, w, r, |s| self.b.vis.visible(ed, s));
                return self.dims.click(ed, w, r, elem);
            }
        }
        false
    }

    fn keys(&self, ui: &mut egui::Ui, ed: &mut Editor) {
        let evs = ui.input_mut(|i| {
            // Tab cycles snaps, not focus
            let tab = i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
            (tab, i.events.clone())
        });
        if evs.0 {
            ed.handle(Event::Key(Key::Tab));
        }
        // the frame clock: snap dwell and session timers see time only as `Tick`
        if ed.tool_armed() {
            let ms = ui.input(|i| (i.time * 1000.0) as u64);
            ed.input(crate::interact::Input::Tick(ms));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        for e in evs.1 {
            match e {
                egui::Event::Text(s) => {
                    for ch in s.chars() {
                        let toggle = matches!(ch, 'a' | 'A' | '\u{444}' | '\u{424}')
                            && ed.wall_draft().input.is_empty()
                            && !ed.opening_mode
                            && ed.wall_mode() == Mode::Polyline;
                        if toggle {
                            ed.handle(Event::Key(Key::Arc));
                            continue;
                        }
                        ed.handle(Event::Char(ch));
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let cmd = modifiers.command;
                    match key {
                        // Ctrl+Z/Y and other shortcuts go through the shell's command registry
                        _ if cmd => {}
                        egui::Key::Enter => ed.handle(Event::Key(Key::Enter)),
                        egui::Key::Escape => ed.handle(Event::Key(Key::Escape)),
                        egui::Key::Backspace => ed.handle(Event::Key(Key::Backspace)),
                        // Delete and nudges act on the selection, never under a running session
                        _ if crate::interact::sessions::owns_selection(ed) => {}
                        egui::Key::Delete => crate::view_gfx_ui::delete_key(ed),
                        egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp
                        | egui::Key::ArrowDown => {
                            let (dx, dy) = match key {
                                egui::Key::ArrowLeft => (-1, 0),
                                egui::Key::ArrowRight => (1, 0),
                                egui::Key::ArrowUp => (0, 1),
                                _ => (0, -1),
                            };
                            crate::grips::nudge(ed, dx, dy, modifiers.shift);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

/// "number name" of the model room picked at a label point; none without a placed room.
#[cfg(test)]
fn placed_label(picked: Option<i64>, rooms: &[crate::editor::levels::RoomItem]) -> Option<String> {
    picked
        .and_then(|id| rooms.iter().find(|r| r.id == id))
        .map(|r| format!("{} {}", r.number, r.name).trim().to_string())
}

/// Room name and area at the outline center: drawn after grids, sections and layer elements
/// so their lines do not cross the label.
fn room_labels(
    painter: &egui::Painter,
    ed: &Editor,
    t: &Tokens,
    to_s: &impl Fn(V2) -> Pos2,
    s: f64,
    arrows: &[(Pos2, Pos2)],
) {
    let tag_font = egui::FontId::new(
        ddc_ui::theme::type_size::LABEL,
        ddc_ui::theme::family(ddc_ui::theme::W_SEMIBOLD),
    );
    let area_font = egui::FontId::new(
        ddc_ui::theme::type_size::CAPTION,
        ddc_ui::theme::family(ddc_ui::theme::W_REGULAR),
    );
    // rooms with a tag element are labelled by the tag
    let tagged: std::collections::HashSet<i64> = ed
        .tags
        .iter()
        .filter_map(|t| match t.spec.host {
            ddc_model::TagHost::Room(h) => Some(h),
            _ => None,
        })
        .collect();
    let clip = painter.clip_rect().expand(100.0);
    use crate::room_label_fit as fit;
    let ppp = painter.ctx().pixels_per_point();
    let line_h = |f: &egui::FontId| painter.layout_no_wrap("Ag".into(), f.clone(), t.text).size().y;
    let metrics = fit::Metrics {
        title_h: line_h(&tag_font),
        area_h: line_h(&area_font),
        gap: 6.0,
        pad: 3.0,
        dot_min: 8.0,
    };
    let model = {
        let c = ed.model.sql();
        (c as *const rusqlite::Connection as usize, c.total_changes(), ed.revision, ed.level)
    };
    let key = fit::LayoutKey {
        model,
        bucket: fit::zoom_bucket(s),
        // area text depends on language and units
        fmt: format!("{}|{}", crate::unit_text::area_ui(1234.5), ppp),
    };
    thread_local! {
        static LAYOUT: std::cell::RefCell<fit::LayoutCache> = std::cell::RefCell::new(fit::LayoutCache::default());
    }
    LAYOUT.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entries = cache.get(key.clone(), || {
            let mut measure = |text: &str, face: fit::Face| {
                let (font, ink) = match face {
                    fit::Face::Title => (&tag_font, t.text),
                    fit::Face::Area => (&area_font, t.text_dim),
                };
                painter.layout_no_wrap(text.to_string(), font.clone(), ink).size().x
            };
            let centres: Vec<(V2, f64)> = rooms(ed).iter().map(|&(c, a, _)| (c, a)).collect();
            fit::build_layout(
                &centres,
                &ed.rooms,
                &crate::unit_text::area_ui,
                key.bucket,
                &metrics,
                &mut measure,
            )
        });
        for e in entries {
            if tagged.contains(&e.id) || !clip.contains(to_s(e.c0)) {
                continue;
            }
            let p = to_s(e.c0);
            let (Some(title), area) = (&e.fit.title, &e.fit.area) else {
                // too small for text: a quiet dot marks the room
                painter.circle_filled(p, 2.0, t.text_dim.gamma_multiply(0.6));
                continue;
            };
            let name = painter.layout_no_wrap(title.clone(), tag_font.clone(), t.text);
            let ar = area
                .as_ref()
                .map(|a| painter.layout_no_wrap(a.clone(), area_font.clone(), t.text_dim));
            let w = name.size().x.max(ar.as_ref().map_or(0.0, |a| a.size().x));
            let h = name.size().y + ar.as_ref().map_or(0.0, |a| a.size().y + 6.0);
            let c = tag_clear_of(p, Vec2::new(w, h), arrows);
            let line_y = c.y + 1.0;
            overlay::halo_text(
                painter,
                egui::pos2(c.x - name.size().x / 2.0, line_y - 2.0 - name.size().y),
                name,
                t.text,
                t.paper,
                0.0,
            );
            if let Some(ar) = ar {
                painter.hline(
                    egui::Rangef::new(c.x - w / 2.0, c.x + w / 2.0),
                    px::snap(line_y, ppp),
                    px::stroke(ppp, t.text_dim.gamma_multiply(0.5)),
                );
                overlay::halo_text(
                    painter,
                    egui::pos2(c.x - ar.size().x / 2.0, line_y + 3.0),
                    ar,
                    t.text_dim,
                    t.paper,
                    0.0,
                );
            }
        }
    });
}

/// The snap marker with its kind label and the Tab position; shared by draw, modify and grip tools.
pub(crate) fn snap_marker(
    painter: &egui::Painter,
    to_s: &dyn Fn(V2) -> Pos2,
    sn: crate::snap::Snap,
    all: &[crate::snap::Snap],
    idx: usize,
    from: Option<V2>,
    ov: &crate::overlay::Palette,
) {
    snap_marker_glyph(painter, to_s, sn, from, ov);
    overlay::snap_label(painter, to_s(sn.p), snap_text(sn, all, idx), ov);
}

/// Glyph and angle trace of the active snap, without its name.
pub(crate) fn snap_marker_glyph(
    painter: &egui::Painter,
    to_s: &dyn Fn(V2) -> Pos2,
    sn: crate::snap::Snap,
    from: Option<V2>,
    ov: &crate::overlay::Palette,
) {
    let p = to_s(sn.p);
    if let (SnapKind::Angle(_), Some(f)) = (sn.kind, from) {
        overlay::snap_trace(painter, to_s(f), p, ov);
    }
    overlay::snap_glyph(painter, p, snap_glyph(sn.kind), ov);
}

/// Snap name, with the Tab cycle position when several snaps compete.
pub(crate) fn snap_text(sn: crate::snap::Snap, all: &[crate::snap::Snap], idx: usize) -> String {
    let n = all.iter().filter(|s| s.kind != SnapKind::Free).count();
    if n > 1 {
        let (i, n) = ((idx + 1).to_string(), n.to_string());
        let more = crate::tr("plan.snap.cycle", &[("i", &i), ("n", &n)]);
        format!("{}  {more}", sn.kind.label())
    } else {
        sn.kind.label().to_string()
    }
}

/// Plan snap glyph (shape distinguishable without color, INTERACTIONS §1.4).
pub(crate) fn snap_glyph(k: SnapKind) -> SnapGlyph {
    match k {
        SnapKind::Endpoint => SnapGlyph::Square,
        SnapKind::Midpoint | SnapKind::Param => SnapGlyph::Triangle,
        SnapKind::Perpendicular => SnapGlyph::Perp,
        SnapKind::Tangent => SnapGlyph::Circle,
        SnapKind::Intersection => SnapGlyph::Cross,
        SnapKind::WallAxis => SnapGlyph::Diamond,
        SnapKind::WallFace => SnapGlyph::Slab,
        SnapKind::GridAxis => SnapGlyph::DiamondDot,
        SnapKind::RefPlane => SnapGlyph::Dashes,
        SnapKind::WorkPlane => SnapGlyph::Slant,
        SnapKind::AlignX | SnapKind::AlignY | SnapKind::AlignXY => SnapGlyph::Track,
        SnapKind::Angle(_) | SnapKind::Center => SnapGlyph::Circle,
        SnapKind::Nearest => SnapGlyph::Hourglass,
        SnapKind::Quadrant => SnapGlyph::Quadrant,
        SnapKind::Extension => SnapGlyph::Guide,
        SnapKind::Parallel => SnapGlyph::Parallel,
        SnapKind::Tracking(_) | SnapKind::EqualStep => SnapGlyph::Track,
        SnapKind::Grid | SnapKind::Free => SnapGlyph::Plus,
    }
}

/// Edge cache key: costs O(hidden), not O(walls); model edits are caught by
/// `revision` and `total_changes`, hiding/isolation by `Vis::wall_stamp`.
fn edges_stamp(ed: &Editor, vis: &crate::isolate::Vis) -> EdgesStamp {
    let conn = ed.model.sql();
    (
        conn as *const rusqlite::Connection as usize,
        conn.total_changes(),
        ed.revision,
        ed.level,
        if vis.any() { vis.wall_stamp() } else { 0 },
    )
}

#[cfg(test)]
fn ids_print(ids: impl Iterator<Item = i64>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for id in ids {
        h = (h ^ id as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Material class of each wall type: the main (thickest) layer; a type without a material
/// is classified by name.
fn wall_classes(ed: &Editor) -> Vec<(i64, usize)> {
    use crate::section_view::mat_class;
    let other = crate::ui_kit::MAT_CLASSES.len() - 1;
    let mut best: Vec<(i64, String, f64)> = vec![];
    if let Ok(mut st) = ed
        .model
        .sql()
        .prepare("SELECT wtype, material, thickness FROM wall_type_layer ORDER BY wtype, pos")
    {
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        });
        if let Ok(rows) = rows {
            for (id, m, th) in rows.flatten() {
                match best.iter_mut().find(|b| b.0 == id) {
                    Some(b) if th > b.2 => *b = (id, m, th),
                    Some(_) => {}
                    None => best.push((id, m, th)),
                }
            }
        }
    }
    ed.types
        .iter()
        .map(|ty| {
            let by_layer = best
                .iter()
                .find(|b| b.0 == ty.id)
                .map_or(other, |b| mat_class(&b.1));
            let class = if by_layer != other {
                by_layer
            } else if ty.name.starts_with("\u{41d}\u{430}\u{440}\u{443}\u{436}\u{43d}\u{430}\u{44f}") {
                1
            } else {
                mat_class(&ty.name)
            };
            (ty.id, class)
        })
        .collect()
}

/// First-run hint on an empty canvas.
/// Point inside a polygon (crossing parity).
/// Warning marker: yellow triangle with "!" at the wall midpoint.
fn warn_markers(
    p: &egui::Painter,
    ed: &Editor,
    ids: &[i64],
    to_s: impl Fn(V2) -> Pos2,
    ov: &overlay::Palette,
) {
    let fill = ov.warning;
    let ink = ov.paper;
    for w in ed.walls.iter().filter(|w| ids.contains(&w.id)) {
        let c = to_s((w.a + w.b) * 0.5) + egui::vec2(0.0, -12.0);
        let r = 7.0;
        let pts = vec![
            c + egui::vec2(0.0, -r),
            c + egui::vec2(r * 0.95, r * 0.7),
            c + egui::vec2(-r * 0.95, r * 0.7),
        ];
        p.add(egui::Shape::convex_polygon(
            pts,
            fill,
            Stroke::new(overlay::THIN_PX, ov.paper),
        ));
        p.text(
            c + egui::vec2(0.0, 1.5),
            egui::Align2::CENTER_CENTER,
            "!",
            ddc_ui::typo::font(ddc_ui::theme::Role::CanvasMicro),
            ink,
        );
    }
}

#[cfg(test)]
fn inside_poly(p: V2, poly: &[V2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// One calm line at the bottom of the canvas while a drawing tool is active; the text follows
/// the step (first point, next points, closable outline, rectangle corners).
/// Tone of a floor on the viewed level: the projection colour at low alpha (theme-aware).
pub(crate) fn slab_tone(t: &Tokens) -> egui::Color32 {
    t.projected.gamma_multiply(0.10)
}

/// Filled mesh of an outline with holes (first ring outer; further rings and cuts are holes).
/// None for a degenerate or self-crossing outline: the caller still draws its lines.
pub(crate) fn region_fill(
    rings: &[Vec<V2>],
    cuts: &[Vec<V2>],
    to_s: &dyn Fn(V2) -> Pos2,
    color: egui::Color32,
) -> Option<egui::Shape> {
    let outer = rings.first().filter(|r| r.len() >= 3)?;
    let holes: Vec<Vec<V2>> = rings[1..]
        .iter()
        .chain(cuts)
        .filter(|h| h.len() >= 3)
        .cloned()
        .collect();
    let tris = ddc_geom::mesh_ops::triangulate_region(outer, &holes)?;
    let mut m = egui::Mesh::default();
    for p in outer.iter().chain(holes.iter().flatten()) {
        m.colored_vertex(to_s(*p), color);
    }
    for [a, b, c] in tris {
        m.add_triangle(a as u32, b as u32, c as u32);
    }
    (!m.indices.is_empty()).then(|| egui::Shape::mesh(m))
}

/// The start card shows only while the model has no element and no underlay.
fn start_hint_shown(empty_model: bool, has_underlay: bool) -> bool {
    empty_model && !has_underlay
}

fn empty_hint(painter: &egui::Painter, rect: egui::Rect, t: &Tokens, select: bool) {
    let lines = [
        crate::tr(if select { "plan.hint.select" } else { "plan.hint.click" }, &[]),
        crate::tr("plan.hint.typed", &[]),
        crate::tr("plan.hint.keys", &[]),
        crate::tr("plan.hint.room", &[]),
        crate::tr("plan.hint.nav", &[]),
    ];
    let title = egui::FontId::proportional(ddc_ui::theme::type_size::TITLE);
    let body = egui::FontId::proportional(ddc_ui::theme::type_size::BODY);
    let w = 520.0f32.min(rect.width() - 32.0);
    let h = 28.0 + 20.0 * lines.len() as f32 + 16.0;
    let card = egui::Rect::from_min_size(
        egui::pos2(rect.center().x - w / 2.0, rect.top() + 24.0),
        Vec2::new(w, h),
    );
    painter.rect(
        card,
        t.radius as f32 * 2.0,
        t.panel.gamma_multiply(0.96),
        Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );
    let mut y = card.top() + 12.0;
    painter.text(
        egui::pos2(card.left() + 16.0, y),
        egui::Align2::LEFT_TOP,
        crate::tr("plan.hint.title", &[]),
        title,
        t.text,
    );
    y += 28.0;
    for l in lines {
        painter.text(
            egui::pos2(card.left() + 16.0, y),
            egui::Align2::LEFT_TOP,
            l.as_str(),
            body.clone(),
            t.text_dim,
        );
        y += 20.0;
    }
}

fn lock_id() -> egui::Id {
    egui::Id::new("ddc.plan.locked")
}

/// While a form is open over the plan the canvas answers hover only (set once per frame).
pub(crate) fn lock_canvas(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(lock_id(), on));
}

fn canvas_locked(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(lock_id())).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use egui::{self, Pos2};

    #[test]
    fn a_locked_canvas_takes_no_click() {
        let ctx = egui::Context::default();
        assert!(!super::canvas_locked(&ctx));
        super::lock_canvas(&ctx, true);
        assert!(super::canvas_locked(&ctx));
        super::lock_canvas(&ctx, false);
        assert!(!super::canvas_locked(&ctx));
    }

    /// One step hint on screen: the status bar owns it, the canvas draws no second pill.
    #[test]
    fn the_status_bar_is_the_only_step_hint() {
        use crate::tools::{arm, Kind};
        use ddc_ui::i18n::Lang;
        let mut a = crate::layout_contract::app_in(Lang::En);
        a.ed.set_mode(super::Mode::Polyline);
        assert_eq!(a.tool_hint(), Some(("wall", "first")));
        for k in [Kind::Floor, Kind::Roof, Kind::Stair, Kind::Ceiling] {
            arm(&mut a.ed, k, Lang::En);
            let h = a.tool_hint().map(|h| h.0);
            assert_ne!(h, Some("wall"), "{k:?}");
        }
    }

    #[test]
    fn floor_fill_covers_the_outline_and_skips_the_hole() {
        use ddc_geom::v2;
        let to_s = |p: ddc_geom::V2| egui::pos2(p.x as f32 * 10.0, p.y as f32 * 10.0);
        let outer = vec![v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0)];
        let hole = vec![v2(1.0, 1.0), v2(1.0, 2.0), v2(2.0, 2.0), v2(2.0, 1.0)];
        let c = egui::Color32::from_rgba_unmultiplied(10, 20, 30, 40);
        let egui::Shape::Mesh(m) = super::region_fill(&[outer.clone()], &[hole], &to_s, c).unwrap()
        else {
            panic!("mesh expected")
        };
        let area: f32 = m
            .indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|i| m.vertices[t[i] as usize].pos);
                ((b - a).x * (c - a).y - (b - a).y * (c - a).x).abs() / 2.0
            })
            .sum();
        assert!((area - (2400.0 - 100.0)).abs() < 1.0, "{area}");
        assert!(super::region_fill(&[vec![v2(0.0, 0.0), v2(1.0, 0.0)]], &[], &to_s, c).is_none());
        // visible on paper in both themes, and lighter than a projection line
        for mode in [ddc_ui::Mode::Light, ddc_ui::Mode::Dark] {
            let th = crate::ui_kit::from_theme(&ddc_ui::Theme::new(mode, ddc_ui::Density::Compact));
            let tone = super::slab_tone(&th);
            assert!(tone.a() > 0 && tone.a() < th.projected.a(), "{tone:?}");
        }
    }

    #[test]
    fn room_tag_moves_off_slope_arrow() {
        use egui::{self, Vec2};
        let (a, b) = (egui::pos2(100.0, 200.0), egui::pos2(300.0, 200.0));
        let size = Vec2::new(90.0, 34.0);
        let c = super::tag_clear_of(egui::pos2(200.0, 205.0), size, &[(a, b)]);
        let r = egui::Rect::from_center_size(c, size);
        assert!(
            (r.top() - 200.0).abs() >= 20.0 && (r.bottom() - 200.0).abs() >= 20.0,
            "{r:?}"
        );
        assert!(r.top() > 200.0 || r.bottom() < 200.0);
        // a tag far from the arrow does not move
        let far = egui::pos2(200.0, 400.0);
        assert_eq!(super::tag_clear_of(far, size, &[(a, b)]), far);
    }

    #[test]
    fn a_moved_canvas_keeps_the_model_under_the_same_pixel() {
        use super::{keep_on_screen, Pos2, V2};
        let to_s = |c: Pos2, center: V2, s: f64, p: V2| {
            Pos2::new(
                c.x + ((p.x - center.x) * s) as f32,
                c.y - ((p.y - center.y) * s) as f32,
            )
        };
        let (center, s) = (ddc_geom::v2(3.0, 2.0), 80.0);
        let (c0, c1) = (Pos2::new(700.0, 450.0), Pos2::new(584.0, 410.0));
        let moved = keep_on_screen(center, s, c0, c1);
        for p in [ddc_geom::v2(0.0, 0.0), ddc_geom::v2(12.5, -4.0)] {
            let (a, b) = (to_s(c0, center, s, p), to_s(c1, moved, s, p));
            assert!((a - b).length() < 1e-3, "{a:?} {b:?}");
        }
    }

    #[test]
    fn room_tag_area_is_clear_inside_faces() {
        use crate::wall_tool::{Event, Mode};
        use ddc_geom::v2;
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        let r = super::rooms(&ed);
        assert_eq!(r.len(), 1, "{r:?}");
        let (c, area, size) = r[0];
        let t = ed
            .types
            .iter()
            .find(|t| t.id == ed.walls[0].wall_type)
            .unwrap()
            .thickness();
        assert!(
            (c.x - 3.0).abs() < 1e-6 && (c.y - 2.0).abs() < 1e-6,
            "{c:?}"
        );
        assert!((area - (6.0 - t) * (4.0 - t)).abs() < 1e-6, "{area}");
        assert!((size - (4.0 - t)).abs() < 1e-6, "{size}");
    }

    #[test]
    fn room_tags_split_by_t_junction_partition() {
        use crate::wall_tool::{Event, Mode};
        use ddc_geom::v2;
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        let ty = ed.walls[0].wall_type;
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a: (2.0, 0.0),
                b: (2.0, 4.0),
                level: ed.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: ty,
                offset0: None,
            })
            .unwrap();
        ed.reload().unwrap();
        let t = ed.types.iter().find(|x| x.id == ty).unwrap().thickness();
        let mut r = super::rooms(&ed);
        r.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
        assert_eq!(r.len(), 2, "{r:?}");
        let total: f64 = r.iter().map(|x| x.1).sum();
        assert!(
            (total - (6.0 - 2.0 * t) * (4.0 - t)).abs() < 1e-6,
            "{total}"
        );
        let model: f64 = ed
            .model
            .room_regions(ed.level)
            .unwrap()
            .iter()
            .map(|x| x.area)
            .sum();
        assert!((total - model).abs() < 1e-9);
        // each room's label stays inside its own half, away from the partition
        assert!(r[0].0.x < 2.0 - t && r[1].0.x > 2.0 + t, "{r:?}");
    }

    #[test]
    fn ids_print_tells_shown_wall_sets_apart() {
        let print = |ids: &[i64]| super::ids_print(ids.iter().copied());
        assert_eq!(print(&[1, 2, 3]), print(&[1, 2, 3]));
        assert_ne!(print(&[1, 2, 3]), print(&[1, 2]));
        assert_ne!(print(&[1, 2]), print(&[1, 3]));
    }

    /// A plan that shrank into a narrower pane, or a level whose walls lie elsewhere, is refitted
    /// into the real canvas, with a margin on every side.
    #[test]
    fn fit_in_puts_the_whole_level_inside_the_canvas() {
        use crate::wall_tool::{Event, Mode};
        use ddc_geom::v2;
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(12.0, 5.0)));
        ed.handle(Event::Release(v2(12.0, 5.0)));
        let mut v = super::PlanView::default();
        let wide = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0));
        v.fit_in(&ed, wide.size());
        assert!(!v.is_cut(&ed, wide));
        // the same view in a pane half as wide cuts the plan off; the refit brings it back
        let narrow = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 700.0));
        assert!(v.is_cut(&ed, narrow));
        v.fit_in(&ed, narrow.size());
        assert!(!v.is_cut(&ed, narrow));
        // a view panned far away is cut as well
        v.center = v2(500.0, 500.0);
        assert!(v.is_cut(&ed, narrow));
    }

    fn two_walls() -> (crate::editor::Editor, crate::editor::Sel, crate::editor::Sel) {
        use crate::editor::Sel;
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        crate::modify_tools::tests::walls(
            &mut ed,
            &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 2.0), (4.0, 2.0))],
        );
        let (a, b) = (Sel::Wall(ed.walls[0].id), Sel::Wall(ed.walls[1].id));
        (ed, a, b)
    }

    #[test]
    fn plan_edges_stamp_not_recomputed_per_frame() {
        let (mut ed, a, _) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        vis.hide(&[a]);
        let s = super::edges_stamp(&ed, &vis);
        // wall list is not read: dropping walls without a revision bump keeps the stamp
        ed.walls.clear();
        assert_eq!(super::edges_stamp(&ed, &vis), s);
        assert_eq!(super::edges_stamp(&ed, &vis), s);
    }

    #[test]
    fn plan_edges_stamp_changes_on_hide_toggle() {
        let (ed, a, b) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        let none = super::edges_stamp(&ed, &vis);
        vis.hide(&[a]);
        let ha = super::edges_stamp(&ed, &vis);
        assert_ne!(ha, none);
        vis.reset();
        vis.hide(&[b]);
        assert_ne!(super::edges_stamp(&ed, &vis), ha);
        vis.reset();
        vis.isolate(&ed, &[a]);
        let iso = super::edges_stamp(&ed, &vis);
        assert_ne!(iso, none);
        assert_ne!(iso, ha);
    }

    #[test]
    fn plan_edges_stamp_returns_on_unhide() {
        let (ed, a, _) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        let none = super::edges_stamp(&ed, &vis);
        vis.hide(&[a]);
        vis.reset();
        assert_eq!(super::edges_stamp(&ed, &vis), none);
    }

    #[test]
    fn plan_edges_stamp_changes_on_edit() {
        let (mut ed, a, _) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        vis.hide(&[a]);
        let before = super::edges_stamp(&ed, &vis);
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 5.0), (4.0, 5.0))]);
        assert_ne!(super::edges_stamp(&ed, &vis), before);
    }

    /// A plan with `n` slabs (at the cut and above it), masses-free, bodies and an underlay.
    fn layered_plan(n: usize) -> crate::editor::Editor {
        use crate::editor::bodies::{BodyItem, BodyKind};
        use ddc_geom::v2;
        let (mut ed, ..) = two_walls();
        let st = ed
            .model
            .add_slab_type(
                "s",
                &[ddc_model::LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let side = (n as f64).sqrt().ceil() as usize;
        for i in 0..n {
            let (x, y) = ((i % side) as f64 * 0.5, (i / side) as f64 * 0.5);
            ed.model
                .add_slab(&ddc_model::SlabSpec {
                    level: ed.level,
                    slab_type: st,
                    offset: if i % 3 == 0 { 3.5 } else { 0.0 },
                    outer: vec![(x, y), (x + 0.4, y), (x + 0.4, y + 0.4), (x, y + 0.4)],
                    holes: vec![],
                })
                .unwrap();
        }
        ed.reload().unwrap();
        for (k, kind) in [BodyKind::Column, BodyKind::Beam].into_iter().enumerate() {
            let x = k as f64 * 1.5;
            ed.bodies.push(BodyItem {
                kind,
                id: 900 + k as i64,
                category: "Columns".into(),
                name: String::new(),
                hull: vec![v2(x, 1.0), v2(x + 0.3, 1.0), v2(x + 0.3, 1.3), v2(x, 1.3)],
                zmin: ed.elevation,
                zmax: ed.elevation + 3.0,
                mesh: None,
            });
        }
        ed.underlay = vec![vec![vec![v2(-1.0, -1.0), v2(5.0, -1.0), v2(5.0, 3.0)]]];
        ed
    }

    /// Triangles on screen, in paint order: (clip, texture, three vertices).
    fn plan_triangles(
        view: &mut super::PlanView,
        ed: &mut crate::editor::Editor,
        ctx: &egui::Context,
        th: &mut ddc_ui::Theme,
    ) -> Vec<[u32; 13]> {
        let tok = crate::ui_kit::from_theme(th);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(900.0, 640.0))),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| {
            let _ = th.apply(ctx);
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| view.show(ui, ed, &tok));
        });
        let mut tris = vec![];
        for p in ctx.tessellate(out.shapes, out.pixels_per_point) {
            let egui::epaint::Primitive::Mesh(m) = p.primitive else {
                continue;
            };
            let clip = p.clip_rect;
            for t in m.indices.chunks(3) {
                let v: [egui::epaint::Vertex; 3] = std::array::from_fn(|j| m.vertices[t[j] as usize]);
                let bb = egui::Rect::from_points(&v.map(|v| v.pos));
                // a shape wholly off the clip is skipped by the frame but kept in a baked mesh
                if !bb.intersects(clip) {
                    continue;
                }
                let mut row = [0u32; 13];
                row[0] = clip.min.x.to_bits() ^ clip.max.y.to_bits();
                for (k, v) in v.iter().enumerate() {
                    row[1 + k * 4] = v.pos.x.to_bits();
                    row[2 + k * 4] = v.pos.y.to_bits();
                    row[3 + k * 4] = u32::from_le_bytes(v.color.to_array());
                    row[4 + k * 4] = v.uv.x.to_bits() ^ v.uv.y.to_bits().rotate_left(16);
                }
                tris.push(row);
            }
        }
        tris
    }

    #[test]
    fn baked_plan_layers_match_shape_by_shape_painting() {
        let mut ed = layered_plan(60);
        assert!(ed.slabs.len() == 60 && !ed.bodies.is_empty());
        let (ctx, mut th) = crate::layout_contract::driven_ctx();
        let mut live = super::PlanView {
            bake: false,
            ..Default::default()
        };
        live.center = ddc_geom::v2(1.5, 1.5);
        let mut baked = super::PlanView::default();
        baked.center = live.center;
        let want = plan_triangles(&mut live, &mut ed, &ctx, &mut th);
        assert!(want.len() > 1000, "{} triangles", want.len());
        for pass in 0..3 {
            let got = plan_triangles(&mut baked, &mut ed, &ctx, &mut th);
            assert_eq!(got.len(), want.len(), "pass {pass}");
            assert!(got == want, "pass {pass}: baked layers paint differently");
        }
        assert!(baked.slab_mesh.is_some() && baked.body_mesh.is_some());
        // a style change rebuilds: the selection is part of the key
        let k = baked.slab_mesh.as_ref().unwrap().0;
        ed.selection = ed.slabs.first().map(|s| crate::editor::Sel::Slab(s.id)).into_iter().collect();
        live.bake = false;
        let want = plan_triangles(&mut live, &mut ed, &ctx, &mut th);
        let got = plan_triangles(&mut baked, &mut ed, &ctx, &mut th);
        assert!(got == want, "after a selection change");
        assert_ne!(baked.slab_mesh.as_ref().unwrap().0, k);
    }

    #[test]
    fn baked_plan_layers_save_frame_time() {
        let mut ed = layered_plan(4000);
        let (ctx, mut th) = crate::layout_contract::driven_ctx();
        let mut time = |bake: bool| {
            let mut v = super::PlanView {
                bake,
                ..Default::default()
            };
            v.center = ddc_geom::v2(15.0, 15.0);
            v.scale = 25.0;
            plan_triangles(&mut v, &mut ed, &ctx, &mut th);
            let t0 = web_time::Instant::now();
            // run + tessellate: what a still frame costs (egui repaints on pointer moves)
            for _ in 0..10 {
                plan_triangles(&mut v, &mut ed, &ctx, &mut th);
            }
            t0.elapsed() / 10
        };
        let (off, on) = (time(false), time(true));
        eprintln!("plan 4000 slabs, frame: shape by shape {off:?}, baked {on:?}");
    }

    fn frame(
        ed: &mut crate::editor::Editor,
        vis: &crate::isolate::Vis,
        c: &mut crate::isolate::MaskCache<super::EdgesStamp>,
    ) -> usize {
        let k = super::edges_stamp(ed, vis);
        let st = vis.stash_cached(ed, k, c);
        let shown = ed.walls.len();
        crate::isolate::Vis::restore(ed, st);
        shown
    }

    #[test]
    fn plan_stash_not_rebuilt_per_frame() {
        let (mut ed, a, _) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        vis.hide(&[a]);
        let mut c = Default::default();
        for _ in 0..5 {
            assert_eq!(frame(&mut ed, &vis, &mut c), 1);
            assert_eq!(ed.walls.len(), 2);
        }
        assert_eq!(c.1, 1);
    }

    #[test]
    fn plan_stash_rebuilt_on_edit_and_hide() {
        let (mut ed, a, b) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        vis.hide(&[a]);
        let mut c = Default::default();
        assert_eq!(frame(&mut ed, &vis, &mut c), 1);
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 5.0), (4.0, 5.0))]);
        assert_eq!(frame(&mut ed, &vis, &mut c), 2);
        assert_eq!(c.1, 2);
        vis.hide(&[b]);
        assert_eq!(frame(&mut ed, &vis, &mut c), 1);
        vis.reset();
        vis.isolate(&ed, &[b]);
        assert_eq!(frame(&mut ed, &vis, &mut c), 1);
        assert_eq!(c.1, 4);
    }

    #[test]
    fn plan_stash_rebuilt_on_undo_and_level_switch() {
        let (mut ed, a, b) = two_walls();
        let mut vis = crate::isolate::Vis::default();
        vis.hide(&[a]);
        let mut c = Default::default();
        // the fixture walls bypass the editor stack: an editor delete gives undo a step
        ed.selection = vec![b];
        ed.delete_selection();
        assert_eq!(ed.walls.len(), 1);
        assert_eq!(frame(&mut ed, &vis, &mut c), 0);
        let built = c.1;
        ed.undo();
        assert_eq!(frame(&mut ed, &vis, &mut c), 1);
        assert!(c.1 > built, "undo must rebuild masks");
        let built = c.1;
        ed.redo();
        frame(&mut ed, &vis, &mut c);
        assert!(c.1 > built, "redo must rebuild masks");
        let lv = ed.level;
        let other = ed.levels().iter().map(|l| l.0).find(|&i| i != lv);
        if let Some(o) = other {
            let before = c.1;
            ed.set_level(o);
            frame(&mut ed, &vis, &mut c);
            assert!(c.1 > before, "level switch must rebuild masks");
        }
    }

    #[test]
    fn start_hint_appears_on_empty_model_and_goes_with_content() {
        assert!(super::start_hint_shown(true, false));
        assert!(!super::start_hint_shown(false, false), "element drawn");
        assert!(!super::start_hint_shown(true, true), "underlay loaded");
        // a real model: empty at start, not empty after a wall
        let mut ed = crate::editor::Editor::start_project().unwrap();
        assert!(ed.walls.is_empty());
        crate::modify_tools::tests::walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0))]);
        assert!(!ed.walls.is_empty());
    }

    #[test]
    fn only_model_rooms_are_labelled() {
        use crate::editor::levels::RoomItem;
        let r = RoomItem {
            id: 5,
            name: "Hall".into(),
            number: "101".into(),
            rings: vec![],
        };
        assert_eq!(super::placed_label(Some(5), &[r.clone()]).as_deref(), Some("101 Hall"));
        assert_eq!(super::placed_label(None, &[r.clone()]), None);
        assert_eq!(super::placed_label(Some(5), &[]), None, "zero-room model");
    }

    #[test]
    fn snap_glyphs_differ_by_meaning() {
        use super::{snap_glyph, SnapGlyph, SnapKind};
        assert_eq!(snap_glyph(SnapKind::Endpoint), SnapGlyph::Square);
        assert_eq!(snap_glyph(SnapKind::Midpoint), SnapGlyph::Triangle);
        assert_eq!(snap_glyph(SnapKind::Intersection), SnapGlyph::Cross);
        assert_eq!(snap_glyph(SnapKind::Nearest), SnapGlyph::Hourglass);
        assert_eq!(snap_glyph(SnapKind::WallFace), SnapGlyph::Slab);
    }

    #[test]
    fn every_snap_kind_has_its_own_glyph() {
        use super::{snap_glyph, SnapKind as K};
        // Grid and Free share the plus: Free is never drawn.
        let kinds = [
            K::Endpoint,
            K::Midpoint,
            K::Perpendicular,
            K::WallAxis,
            K::WallFace,
            K::Angle(45),
            K::Grid,
            K::Intersection,
            K::GridAxis,
            K::RefPlane,
            K::WorkPlane,
            K::Nearest,
            K::AlignX,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(snap_glyph(*a), snap_glyph(*b), "{a:?} vs {b:?}");
            }
        }
        assert_eq!(snap_glyph(K::AlignX), snap_glyph(K::AlignXY));
        assert_eq!(snap_glyph(K::AlignY), snap_glyph(K::AlignXY));
    }

    #[test]
    fn snap_kind_center_has_circle_glyph() {
        use super::{snap_glyph, SnapGlyph, SnapKind};
        assert_eq!(snap_glyph(SnapKind::Center), SnapGlyph::Circle);
    }

    #[test]
    fn point_in_wall_outline() {
        use super::inside_poly;
        use ddc_geom::v2;
        let sq = [v2(0., 0.), v2(6., 0.), v2(6., 0.2), v2(0., 0.2)];
        assert!(inside_poly(v2(3., 0.1), &sq));
        assert!(!inside_poly(v2(3., 0.3), &sq));
    }

    #[test]
    fn grid_step_readable() {
        assert_eq!(super::grid_step(60.0), 0.5);
        assert_eq!(super::grid_step(20.0), 1.0);
        assert_eq!(super::grid_step(1000.0), 0.1);
    }

    #[test]
    fn default_zoom_shows_metre_and_decimetre_grid() {
        let d = super::PlanView::default();
        assert_eq!(super::grid_levels(d.scale), (0.1, 1.0));
        assert_eq!(super::grid_levels(20.0), (1.0, 10.0));
    }

    #[test]
    fn scale_bar_nice_lengths() {
        use crate::units::Units::{Imperial, Metric};
        assert_eq!(super::scale_bar_len(80.0, Metric), 2.0);
        assert_eq!(super::scale_bar_len(1000.0, Metric), 0.2);
        crate::with_test_lang(true, || {
            assert_eq!(super::bar_label(0.5, Metric), "500\u{a0}\u{43c}\u{43c}");
            assert_eq!(super::bar_label(2.0, Metric), "2\u{a0}000\u{a0}\u{43c}\u{43c}");
            assert_eq!(super::bar_label(20.0, Metric), "20\u{a0}000\u{a0}\u{43c}\u{43c}");
        });
        // English groups thousands like the property fields: "2,000 mm", never "2 000 mm"
        crate::with_test_lang(false, || {
            assert_eq!(super::bar_label(0.5, Metric), "500\u{a0}mm");
            assert_eq!(super::bar_label(2.0, Metric), "2,000\u{a0}mm");
            assert_eq!(super::bar_label(20.0, Metric), "20,000\u{a0}mm");
        });
        let l = super::scale_bar_len(80.0, Imperial);
        assert!((l - 5.0 * crate::units::FOOT).abs() < 1e-9);
        assert_eq!(super::bar_label(l, Imperial), "5'-0\"");
    }

    #[test]
    fn plan_pinch_zoom_keeps_cursor_point() {
        use ddc_geom::v2;
        let (c0, s0, anchor) = (v2(3.0, -2.0), 40.0, v2(10.0, 5.0));
        let (c1, s1) = super::zoom_about(c0, s0, 1.5, anchor);
        assert!((s1 - 60.0).abs() < 1e-9);
        // the anchor keeps its screen offset from the centre
        let (a, b) = ((anchor.x - c0.x) * s0, (anchor.x - c1.x) * s1);
        assert!((a - b).abs() < 1e-6);
        let (a, b) = ((anchor.y - c0.y) * s0, (anchor.y - c1.y) * s1);
        assert!((a - b).abs() < 1e-6);
        // clamped at the limit
        assert_eq!(super::zoom_about(c0, 1900.0, 10.0, anchor).1, 2000.0);
    }
}

/// Slab slope arrow: through the outline center along the rise, labelled with the slope in percent.
fn slope_arrow(
    painter: &egui::Painter,
    ring: &[V2],
    g: V2,
    pct: f64,
    to_s: &impl Fn(V2) -> Pos2,
    col: Color32,
) -> (Pos2, Pos2) {
    let n = ring.len().max(1) as f64;
    let c = ring.iter().fold(v2(0.0, 0.0), |acc, &p| acc + p) * (1.0 / n);
    let d = g * (1.0 / g.len());
    let ext = ring
        .iter()
        .map(|&p| ((p - c).x * d.x + (p - c).y * d.y).abs())
        .fold(0.0, f64::max);
    let half = (ext * 0.6).max(0.3);
    let (a, b) = (to_s(c - d * half), to_s(c + d * half));
    let st = Stroke::new(1.4, col);
    painter.line_segment([a, b], st);
    let dir = (b - a).normalized();
    let perp = Vec2::new(-dir.y, dir.x);
    painter.add(egui::Shape::convex_polygon(
        vec![b, b - dir * 11.0 + perp * 4.5, b - dir * 11.0 - perp * 4.5],
        col,
        Stroke::NONE,
    ));
    painter.circle_filled(a, 2.5, col);
    let mut rot = dir.y.atan2(dir.x);
    if rot.abs() > std::f32::consts::FRAC_PI_2 {
        rot -= std::f32::consts::PI.copysign(rot);
    }
    let g = painter.layout_no_wrap(
        crate::units::fmt_percent(pct),
        egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
        col,
    );
    let ld = Vec2::angled(rot);
    let lp = Vec2::new(ld.y, -ld.x);
    let mid = a + (b - a) * 0.5;
    let pos = mid - ld * (g.size().x / 2.0) + lp * (g.size().y + 3.0);
    painter.add(egui::epaint::TextShape::new(pos, g, col).with_angle(rot));
    (a, b)
}

/// A room tag (center `c`, size `size`) is shifted across the slope arrow if it would land
/// on the arrow and its label (a ±22 px band around the arrow).
fn tag_clear_of(c: Pos2, size: Vec2, arrows: &[(Pos2, Pos2)]) -> Pos2 {
    let mut c = c;
    for &(a, b) in arrows {
        let d = (b - a).normalized();
        let n = d.rot90();
        let r = egui::Rect::from_center_size(c, size).expand(4.0);
        let hits = (0..=24).any(|k| r.contains(a + (b - a) * (k as f32 / 24.0)));
        if !hits {
            continue;
        }
        // half the tag extent across the arrow plus the arrow-and-label band
        let half = (size.x * n.x.abs() + size.y * n.y.abs()) / 2.0;
        let side = if (c - a).dot(n) >= 0.0 { 1.0 } else { -1.0 };
        let along = (c - a).dot(n);
        c += n * (side * (half + 26.0) - along);
    }
    c
}

/// Outline edge by rounded endpoints (endpoint order is normalized).
type EdgeKey = ((i64, i64), (i64, i64));
/// Join, change counter, revision, level, fingerprint of the shown walls.
type EdgesStamp = (usize, u64, u64, i64, u64);

/// Ring without reflex corners (either orientation): convex fill is exact.
fn convex(r: &[V2]) -> bool {
    let n = r.len();
    let (mut pos, mut neg) = (false, false);
    for i in 0..n {
        let (a, b, c) = (r[i], r[(i + 1) % n], r[(i + 2) % n]);
        let z = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        pos |= z > 1e-12;
        neg |= z < -1e-12;
    }
    n >= 3 && !(pos && neg)
}

/// Rooms of the plan: regions between wall faces from the model arrangement (T-junctions and
/// partitions split rooms), label point (pole of inaccessibility), clear area, smaller extent.
pub fn rooms(ed: &Editor) -> Vec<(V2, f64, f64)> {
    type RoomKey = (usize, u64, u64, i64);
    thread_local! {
        static CACHE: std::cell::RefCell<Option<(RoomKey, Vec<(V2, f64, f64)>)>> =
            const { std::cell::RefCell::new(None) };
    }
    let c = ed.model.sql();
    let key: RoomKey = (
        c as *const rusqlite::Connection as usize,
        c.total_changes(),
        ed.revision,
        ed.level,
    );
    if let Some(v) = CACHE.with(|x| {
        x.borrow()
            .as_ref()
            .filter(|x| x.0 == key)
            .map(|x| x.1.clone())
    }) {
        return v;
    }
    let out: Vec<(V2, f64, f64)> = ed
        .model
        .room_regions(ed.level)
        .unwrap_or_default()
        .iter()
        .filter(|r| r.outer.len() >= 3 && r.area > 1e-9)
        .map(|r| {
            let (mut lo, mut hi) = (r.outer[0], r.outer[0]);
            for q in &r.outer {
                lo = v2(lo.x.min(q.x), lo.y.min(q.y));
                hi = v2(hi.x.max(q.x), hi.y.max(q.y));
            }
            (
                label_point(r, lo, hi),
                r.area,
                (hi.x - lo.x).min(hi.y - lo.y),
            )
        })
        .collect();
    CACHE.with(|x| *x.borrow_mut() = Some((key, out.clone())));
    out
}

/// Distance from `p` to the nearest edge of `rings`.
fn edge_dist(p: V2, rings: &[&[V2]]) -> f64 {
    let mut best = f64::INFINITY;
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            let d = b - a;
            let l2 = d.x * d.x + d.y * d.y;
            let u = if l2 > 0.0 {
                (((p.x - a.x) * d.x + (p.y - a.y) * d.y) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            best = best.min((p.x - a.x - u * d.x).hypot(p.y - a.y - u * d.y));
        }
    }
    best
}

/// Pole of inaccessibility (cell refinement); the centroid wins when it is as clear,
/// so symmetric rooms keep the tag in the middle.
fn label_point(r: &ddc_geom::rooms::Region, lo: V2, hi: V2) -> V2 {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
    struct Cell(f64, V2, f64, f64);
    impl PartialEq for Cell {
        fn eq(&self, o: &Self) -> bool {
            self.0 == o.0
        }
    }
    impl Eq for Cell {}
    impl PartialOrd for Cell {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Cell {
        fn cmp(&self, o: &Self) -> Ordering {
            self.0.total_cmp(&o.0)
        }
    }
    let rings: Vec<&[V2]> = std::iter::once(r.outer.as_slice())
        .chain(r.holes.iter().map(Vec::as_slice))
        .collect();
    let dist = |p: V2| {
        let d = edge_dist(p, &rings);
        if r.contains(p) {
            d
        } else {
            -d
        }
    };
    let (mut a2, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for ring in &rings {
        for i in 0..ring.len() {
            let (p0, p1) = (ring[i], ring[(i + 1) % ring.len()]);
            let cr = p0.x * p1.y - p1.x * p0.y;
            a2 += cr;
            cx += (p0.x + p1.x) * cr;
            cy += (p0.y + p1.y) * cr;
        }
    }
    let mid = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    let centroid = if a2.abs() > 1e-12 {
        v2(cx / (3.0 * a2), cy / (3.0 * a2))
    } else {
        mid
    };
    let cell = (hi.x - lo.x).min(hi.y - lo.y);
    if cell <= 0.0 {
        return centroid;
    }
    let prec = (cell / 200.0).max(1e-4);
    let mk = |c: V2, h: f64| {
        let d = dist(c);
        Cell(d + h * std::f64::consts::SQRT_2, c, h, d)
    };
    let mut q = BinaryHeap::new();
    let h0 = cell / 2.0;
    let mut x = lo.x;
    while x < hi.x {
        let mut y = lo.y;
        while y < hi.y {
            q.push(mk(v2(x + h0, y + h0), h0));
            y += cell;
        }
        x += cell;
    }
    let mut best = (dist(centroid), centroid);
    if dist(mid) > best.0 {
        best = (dist(mid), mid);
    }
    let mut n = 0;
    while let Some(c) = q.pop() {
        if c.3 > best.0 {
            best = (c.3, c.1);
        }
        n += 1;
        if c.0 - best.0 <= prec || n > 4000 {
            continue;
        }
        let h = c.2 / 2.0;
        for (dx, dy) in [(-h, -h), (h, -h), (-h, h), (h, h)] {
            q.push(mk(v2(c.1.x + dx, c.1.y + dy), h));
        }
    }
    if dist(centroid) >= best.0 - prec {
        centroid
    } else {
        best.1
    }
}
