// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Section/elevation view (2D, model s-z coordinates): section fill by material class,
//! projection and "beyond the plane" lines by line-weight tokens, levels with elevations, grids, height chain;
//! zoom/pan; element selection is shared with plan, 3D and the table. View marks on the plan: `plan_marks`.

use crate::editor::sec::{self, display_name, Grip};
use crate::editor::{Editor, Sel};
use crate::ui_kit::Tokens;
use crate::units::format_length;
use ddc_geom::{v2, V2};
use ddc_model::{LineClass, PlanView as Drawing, SectionSpec, ViewKind, ViewPrim};
use egui::{self, Color32, Pos2, Sense, Stroke, Vec2};

#[derive(Default)]
pub struct SectionView {
    pub center: V2,
    /// Pixels per metre.
    pub scale: f64,
    /// View the scale was fitted for.
    fitted: Option<i64>,
    /// (level of a plan or none, view, editor revision, drawing).
    cache: Option<(Option<i64>, i64, u64, Drawing)>,
    /// Shows the plan of this level read-only (a view pane on a level that is not active).
    pub plan_level: Option<i64>,
    /// Tags of the plan level: (level, editor revision, tags).
    tags: Option<(i64, u64, Vec<ddc_model::Tag>)>,
    /// Cursor of the last frame in model coordinates (`interact::SectionAdapter`); none over a plan.
    pub pointer: Option<crate::interact::Pointer>,
    /// "Level" tool: a click places a new level at the cursor elevation.
    pub level_tool: bool,
    /// Section the tool is enabled in; it turns off in any other view.
    pub level_tool_at: Option<i64>,
    /// What an empty card asks of the shell this frame; the shell takes it.
    pub act: Option<crate::section_hud::SvAct>,
}

/// Material class for fill (index into `Tokens::mat`).
pub fn mat_class(m: &str) -> usize {
    let m = m.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| m.contains(k));
    if has(&["concrete", "\u{431}\u{435}\u{442}\u{43e}\u{43d}"]) {
        0
    } else if has(&["masonry", "brick", "\u{43a}\u{438}\u{440}\u{43f}\u{438}\u{447}", "\u{43a}\u{43b}\u{430}\u{434}\u{43a}"]) {
        1
    } else if has(&["insul", "\u{443}\u{442}\u{435}\u{43f}\u{43b}", "wool"]) {
        2
    } else if has(&["glass", "\u{441}\u{442}\u{435}\u{43a}\u{43b}"]) {
        3
    } else if has(&["door", "wood", "timber", "\u{434}\u{435}\u{440}\u{435}\u{432}"]) {
        4
    } else {
        5
    }
}

/// Drawing element -> editor selection.
pub fn sel_of(table: &str, element: i64) -> Option<Sel> {
    match table {
        "wall" => Some(Sel::Wall(element)),
        "opening" => Some(Sel::Opening(element)),
        "slab" => Some(Sel::Slab(element)),
        "curtain_wall" => Some(Sel::Curtain(element)),
        "mep_run" => Some(Sel::Mep(element)),
        "comp" => Some(Sel::Comp(element)),
        "ramp" => crate::editor::structx::sel("ramp", element),
        _ => None,
    }
}

fn selected(ed: &Editor, table: &str, element: i64) -> bool {
    ed.selection.iter().any(|s| match (*s, table) {
        (Sel::Wall(i), "wall") | (Sel::Opening(i), "opening") | (Sel::Slab(i), "slab") => {
            i == element
        }
        (Sel::Curtain(i), "curtain_wall") | (Sel::Panel(i, _), "curtain_wall") => i == element,
        (Sel::Mep(i), "mep_run") | (Sel::Comp(i), "comp") => i == element,
        (Sel::Ext(c, i), "ramp") => {
            i == element && crate::editor::structx::kind_of(c) == Some("ramp")
        }
        _ => false,
    })
}

/// Bounding box of all drawing primitives.
fn extent(d: &Drawing) -> Option<[f64; 4]> {
    let mut bb = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut add = |p: (f64, f64)| {
        bb = [
            bb[0].min(p.0),
            bb[1].min(p.1),
            bb[2].max(p.0),
            bb[3].max(p.1),
        ];
    };
    for p in &d.prims {
        match p {
            ViewPrim::SectionCut { rings, .. } => rings.iter().for_each(|&q| add(q)),
            ViewPrim::Edge { a, b, .. }
            | ViewPrim::Level { a, b, .. }
            | ViewPrim::Grid { a, b, .. }
            | ViewPrim::Dim { a, b, .. } => {
                add(*a);
                add(*b);
            }
            _ => {}
        }
    }
    (bb[0] < bb[2]).then_some(bb)
}

/// Even-odd fill of rings (with holes) using trapezoids between height breakpoints: exact and
/// seamless; a single mesh without anti-aliasing, the outline is drawn on top.
pub fn fill_even_odd(painter: &egui::Painter, rings: &[Vec<Pos2>], col: Color32) {
    let mesh = even_odd_mesh(rings, col);
    if !mesh.is_empty() {
        painter.add(mesh);
    }
}

/// Mesh of the same fill: for callers that tessellate a shape once and draw it over many frames.
pub fn even_odd_mesh(rings: &[Vec<Pos2>], col: Color32) -> egui::Mesh {
    let edges: Vec<(Pos2, Pos2)> = rings
        .iter()
        .flat_map(|r| (0..r.len()).map(move |i| (r[i], r[(i + 1) % r.len()])))
        .filter(|(a, b)| (a.y - b.y).abs() > 1e-4)
        .collect();
    let mut ys: Vec<f32> = rings.iter().flatten().map(|p| p.y).collect();
    ys.sort_by(f32::total_cmp);
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    let x_at = |(a, b): (Pos2, Pos2), y: f32| a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y);
    let mut mesh = egui::Mesh::default();
    for w in ys.windows(2) {
        let (y0, y1) = (w[0], w[1]);
        let ym = (y0 + y1) / 2.0;
        let mut xs: Vec<(f32, (Pos2, Pos2))> = edges
            .iter()
            .filter(|(a, b)| (a.y.min(b.y) <= ym) && (a.y.max(b.y) >= ym))
            .map(|&e| (x_at(e, ym), e))
            .collect();
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in xs.chunks_exact(2) {
            let (l, r) = (pair[0].1, pair[1].1);
            let i = mesh.vertices.len() as u32;
            for p in [
                egui::pos2(x_at(l, y0), y0),
                egui::pos2(x_at(r, y0), y0),
                egui::pos2(x_at(r, y1), y1),
                egui::pos2(x_at(l, y1), y1),
            ] {
                mesh.colored_vertex(p, col);
            }
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
    }
    mesh
}

fn inside(p: V2, rings: &[(f64, f64)], holes: &[Vec<(f64, f64)>]) -> bool {
    let mut c = false;
    for r in std::iter::once(rings).chain(holes.iter().map(|h| h.as_slice())) {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (a.1 > p.y) != (b.1 > p.y) && p.x < a.0 + (b.0 - a.0) * (p.y - a.1) / (b.1 - a.1) {
                c = !c;
            }
        }
    }
    c
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// The view id `show` takes for a level plan: no section has it.
pub const PLAN_ID: i64 = 0;

/// Grid axes of the model as the plan of `level` shows them.
fn level_grids(ed: &Editor, level: i64) -> Vec<ddc_model::GridAxis> {
    let axes = ed.model.plan_grid_axes(level).unwrap_or_default();
    axes.into_iter().map(|g| g.0).collect()
}

impl SectionView {
    /// Re-fit the scale on the next frame.
    /// The section `id` as a view adapter; `c` is the canvas center on screen.
    pub fn adapter(&self, id: i64, spec: &SectionSpec, c: Pos2) -> Option<crate::interact::SectionAdapter> {
        let (a, b) = sec::ends(spec);
        crate::interact::SectionAdapter::new(id, a, b, self.center, self.scale, [c.x as f64, c.y as f64])
    }

    pub fn refit(&mut self) {
        self.fitted = None;
    }

    /// The drawn drawing (cached by view and model revision).
    fn drawing(&mut self, ed: &Editor, id: i64) -> Option<&Drawing> {
        let pl = self.plan_level;
        let fresh = matches!(&self.cache, Some((l, i, r, _)) if *l == pl && *i == id && *r == ed.revision);
        if !fresh {
            let d = match pl {
                Some(level) => ed
                    .model
                    .plan_view_ranged(level, level_grids(ed, level))
                    .ok()
                    .map(crate::view_panes::plan_as_drawing),
                None => ed.draw_view(id),
            };
            // the screen draws annotations from `annot_text::items`; otherwise a deep copy every frame
            self.cache = d.map(|mut d| {
                d.prims.retain(|p| !matches!(p, ViewPrim::Anno { .. }));
                (pl, id, ed.revision, d)
            });
        }
        self.cache.as_ref().map(|c| &c.3)
    }

    /// Tags of `level`, read again after a model edit.
    fn plan_tags(&mut self, ed: &Editor, level: i64) -> &[ddc_model::Tag] {
        let fresh = matches!(&self.tags, Some((l, r, _)) if *l == level && *r == ed.revision);
        if !fresh {
            self.tags = Some((level, ed.revision, ed.model.tags(level).unwrap_or_default()));
        }
        self.tags.as_ref().map_or(&[], |c| c.2.as_slice())
    }

    /// Shows the plan of `level` at `center` and `scale` (the live plan handing its place over).
    pub fn place_plan(&mut self, level: i64, center: V2, scale: f64) {
        self.plan_level = Some(level);
        self.center = center;
        self.scale = scale;
        self.fitted = Some(PLAN_ID);
    }

    /// Shows the plan of `level` (or the view again with `None`); the scale is fitted anew.
    pub fn set_plan(&mut self, level: Option<i64>) {
        if self.plan_level != level {
            self.plan_level = level;
            self.fitted = None;
        }
    }

    /// Fixes the zoom and the centre of view `id`, so a caller knows the pixel of every point.
    pub fn pin(&mut self, id: i64, center: V2, scale: f64) {
        self.center = center;
        self.scale = scale;
        self.fitted = Some(id);
    }

    pub fn fit(&mut self, ed: &Editor, id: i64, size: Vec2) {
        let Some(bb) = self.drawing(ed, id).and_then(extent) else {
            return;
        };
        let (w, h) = ((bb[2] - bb[0]).max(1.0), (bb[3] - bb[1]).max(1.0));
        // left: dimension chain, right: level marks, top: grid bubbles
        let sx = (size.x as f64 - 220.0).max(80.0) / w;
        let sy = (size.y as f64 - 160.0).max(80.0) / h;
        self.scale = sx.min(sy).clamp(4.0, 600.0);
        self.center = v2(
            (bb[0] + bb[2]) / 2.0 - 10.0 / self.scale,
            (bb[1] + bb[3]) / 2.0,
        );
        self.fitted = Some(id);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ed: &mut Editor,
        id: i64,
        t: &Tokens,
        vis: &crate::isolate::Vis,
        ms: &mut Option<crate::tools::measure::MeasureTool>,
    ) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if self.fitted != Some(id) || self.scale <= 0.0 {
            self.fit(ed, id, rect.size());
        }
        // navigation: middle/left drag on empty space pans, wheel zooms to the cursor
        let s = self.scale;
        if resp.dragged_by(egui::PointerButton::Middle)
            || (resp.dragged_by(egui::PointerButton::Primary)
                && !resp.drag_started_by(egui::PointerButton::Primary)
                && !crate::annot_text::dragging())
        {
            let d = resp.drag_delta();
            self.center = self.center - v2(d.x as f64 / s, -d.y as f64 / s);
        }
        let hover = resp.hover_pos();
        let c0 = rect.center();
        if let Some(h) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let before = v2(
                    self.center.x + (h.x - c0.x) as f64 / s,
                    self.center.y - (h.y - c0.y) as f64 / s,
                );
                self.scale = (s * (scroll as f64 * 0.0025).exp()).clamp(4.0, 3000.0);
                let k = 1.0 - s / self.scale;
                self.center = self.center + (before - self.center) * k;
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && crate::ui_kit::canvas_owns_keys(ui.ctx())
            && !ui.ctx().wants_keyboard_input()
        {
            if self.level_tool {
                self.level_tool = false;
            } else if let Some(m) = ms.as_mut().filter(|_| self.plan_level.is_none()) {
                if matches!(m.escape(), crate::tools::measure::MeasureOut::Exit) {
                    *ms = None;
                }
            } else {
                ed.selection.clear();
            }
        }
        let level_tool = self.level_tool && self.plan_level.is_none();
        let plan_level = self.plan_level;
        let (cx, cy, s) = (self.center.x, self.center.y, self.scale);
        let to_s = move |p: (f64, f64)| {
            Pos2::new(
                c0.x + ((p.0 - cx) * s) as f32,
                c0.y - ((p.1 - cy) * s) as f32,
            )
        };
        let to_w = move |p: Pos2| v2(cx + (p.x - c0.x) as f64 / s, cy - (p.y - c0.y) as f64 / s);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.paper);
        let spec = ed.view_spec(id).cloned();
        self.pointer = match (&spec, hover, plan_level) {
            (Some(sp), Some(h), None) => self.adapter(id, sp, c0).map(|a| {
                use crate::interact::ViewAdapter;
                a.pointer([h.x, h.y])
            }),
            _ => None,
        };
        let units = ed.tool.units;
        let drawing = self.drawing(ed, id).cloned();
        let kind = spec.as_ref().map_or(ViewKind::Section, |s| s.kind);
        let hud_name = match plan_level {
            Some(l) => ed.levels().into_iter().find(|x| x.0 == l).map(|x| x.1).unwrap_or_default(),
            None => spec.as_ref().map(display_name).unwrap_or_default(),
        };
        // a sheet with nothing to fit keeps scale 0: the bar needs a real one
        let hud_scale = if self.scale > 0.0 { self.scale } else { 50.0 };
        let hud = drawing
            .is_some()
            .then(|| crate::section_hud::parts(&painter, rect, &hud_name, hud_scale, units, t));
        let mut fit_hot = false;
        if let Some(h) = hud.as_ref().filter(|h| crate::plan_hud::shown(h.hud.fit)) {
            let r = ui.interact(h.hud.fit, resp.id.with("hud.fit"), Sense::click());
            let label = h.fit.clone();
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
            });
            fit_hot = r.hovered();
            if r.clicked() {
                self.refit();
                ui.ctx().request_repaint();
            }
            r.on_hover_text(crate::tr("cmd.view.zoom_fit.desc", &[]));
        }
        if let Some(k) = crate::section_hud::section_empty_kind(drawing.as_ref(), kind) {
            if let Some(h) = &hud {
                crate::section_hud::paint(&painter, h, fit_hot, t);
            }
            let card = egui::Rect::from_min_max(
                Pos2::new(rect.left() + 12.0, rect.center().y - 72.0),
                Pos2::new(rect.right() - 12.0, rect.bottom() - 48.0),
            );
            let ru = crate::ui_lang_now();
            let clicked = ui
                .scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
                    ui.set_clip_rect(card.intersect(rect));
                    crate::ui_kit::empty_state_lang(ui, ru, k.what(), true)
                })
                .inner;
            if clicked {
                self.act = Some(k.action());
            }
            return;
        }
        let Some(mut d) = drawing else {
            return;
        };

        vis.filter_drawing(ed, &mut d);
        let key = plan_level.map_or(ddc_model::ViewKey::Section(id), ddc_model::ViewKey::Plan);
        let gfx = crate::view_gfx_ui::layer(ed, key);
        gfx.paint.retain_prims(&mut d.prims);
        // selection: the section under the cursor, else the nearest edge (5 px)
        let pick = |p: Pos2| -> Option<(&'static str, i64)> {
            let w = to_w(p);
            d.prims
                .iter()
                .rev()
                .find_map(|q| match q {
                    ViewPrim::SectionCut {
                        table,
                        element,
                        rings,
                        holes,
                        ..
                    } if inside(w, rings, holes) => Some((*table, *element)),
                    _ => None,
                })
                .or_else(|| {
                    d.prims
                        .iter()
                        .filter_map(|q| match q {
                            ViewPrim::Edge {
                                table,
                                element,
                                a,
                                b,
                                ..
                            } => Some((seg_dist(p, to_s(*a), to_s(*b)), *table, *element)),
                            _ => None,
                        })
                        .filter(|x| x.0 <= 5.0)
                        .min_by(|x, y| x.0.total_cmp(&y.0))
                        .map(|x| (x.1, x.2))
                })
        };
        let hot = hover.and_then(pick);
        let anno_view = plan_level.map_or(ddc_model::AnnoView::Section(id), ddc_model::AnnoView::Plan);
        let anno_used = crate::annot_text::view_input(ui, &resp, ed, anno_view, &to_w);
        // a hosted family places on the wall under the section pointer
        let hosted = ed.opening_mode && plan_level.is_none() && self.pointer.is_some();
        if let (true, Some(p)) = (hosted, self.pointer) {
            use crate::interact::{Btn, Input};
            ed.input(Input::Move(p));
            if resp.clicked() {
                ed.input(Input::Click(p, Btn::Left));
            }
        }
        // the measure tool picks points of the cut plane (the same record the plan saves)
        let measuring = ms.is_some() && plan_level.is_none();
        // a click while the number field is open belongs to the field, not to the cut
        let field_open = ms.as_ref().is_some_and(|m| m.edit.is_some());
        if let (true, true, Some(m)) = (measuring && !field_open, resp.clicked(), ms.as_mut()) {
            use crate::tools::measure::MeasureOut as O;
            let out = match self.pointer.and_then(|p| p.point()) {
                Some(q) => m.click_point(ed, ddc_model::ViewKey::Section(id), [q.x, q.y, q.z]),
                None => O::Nothing,
            };
            match out {
                O::Exit => *ms = None,
                O::Refused(m) => ed.say(crate::status::Severity::Refusal, m),
                O::Error(m) => ed.say(crate::status::Severity::Error, m),
                O::Done | O::Nothing => {}
            }
        }
        if resp.clicked() && !level_tool && !anno_used && !hosted && !measuring {
            ed.selection = resp
                .interact_pointer_pos()
                .and_then(pick)
                .and_then(|(tb, el)| sel_of(tb, el))
                .into_iter()
                .collect();
        }
        if hot.is_some() && !resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        // layer 1: grids and levels (under the drawing), 2: projections, 3: sections, 4: labels
        let anno = t.projected;
        let lw = t.lw;
        let small = egui::FontId::new(
            ddc_ui::theme::type_size::CAPTION,
            ddc_ui::theme::family(ddc_ui::theme::W_REGULAR),
        );
        let strong = egui::FontId::new(
            ddc_ui::theme::type_size::LABEL,
            ddc_ui::theme::family(ddc_ui::theme::W_SEMIBOLD),
        );
        for p in &d.prims {
            if let ViewPrim::Grid { label, a, b, .. } = p {
                let (pa, pb) = (to_s(*a), to_s(*b));
                let (lo, hi) = if pa.y > pb.y { (pa, pb) } else { (pb, pa) };
                let st = Stroke::new(lw[3], anno.gamma_multiply(0.8));
                dash_dot(&painter, lo, hi, st);
                let r = 11.0;
                let c = hi - Vec2::new(0.0, r);
                painter.circle(c, r, t.paper, Stroke::new(lw[3], anno));
                painter.text(
                    c,
                    egui::Align2::CENTER_CENTER,
                    label,
                    strong.clone(),
                    t.text,
                );
            }
        }
        let rx = d
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Level { b, .. } => Some(to_s(*b).x),
                _ => None,
            })
            .fold(f32::MIN, f32::max);
        for p in &d.prims {
            if let ViewPrim::Level {
                name, elevation, a, ..
            } = p
            {
                let (pa, pb) = (to_s(*a), egui::pos2(rx, to_s(*a).y));
                let st = Stroke::new(lw[3], anno);
                painter.extend(egui::Shape::dashed_line(&[pa, pb], st, 14.0, 4.0));
                // elevation symbol: triangle with its apex on the line, a flag with elevation and name
                let tip = pb + Vec2::new(10.0, 0.0);
                painter.line_segment([pb, tip + Vec2::new(70.0, 0.0)], st);
                painter.add(egui::Shape::convex_polygon(
                    vec![tip, tip + Vec2::new(-6.0, -9.0), tip + Vec2::new(6.0, -9.0)],
                    t.paper,
                    Stroke::new(lw[3], t.text),
                ));
                painter.line_segment(
                    [tip + Vec2::new(0.0, -9.0), tip + Vec2::new(0.0, -26.0)],
                    st,
                );
                painter.line_segment(
                    [tip + Vec2::new(0.0, -26.0), tip + Vec2::new(70.0, -26.0)],
                    st,
                );
                painter.text(
                    tip + Vec2::new(6.0, -27.0),
                    egui::Align2::LEFT_BOTTOM,
                    crate::editor::levels::elevation_mark(
                        *elevation,
                        units,
                        crate::ui_is_ru(),
                    ),
                    strong.clone(),
                    t.text,
                );
                painter.text(
                    tip + Vec2::new(6.0, -12.0),
                    egui::Align2::LEFT_BOTTOM,
                    name,
                    small.clone(),
                    t.text_dim,
                );
            }
        }
        let col_of = |tb: &str, el: i64, base: Color32| {
            if selected(ed, tb, el) {
                t.accent
            } else if hot.is_some_and(|h| h.0 == tb && h.1 == el) {
                t.ov().hover
            } else {
                base
            }
        };
        for p in &d.prims {
            if let ViewPrim::Edge {
                table,
                element,
                class,
                a,
                b,
            } = p
            {
                let (w, base) = match class {
                    LineClass::Projection => (lw[1], t.cut),
                    LineClass::Beyond | LineClass::Overhead => (lw[2], t.beyond),
                };
                let base = gfx.line_at(table, *element, base);
                let col = col_of(table, *element, base);
                let w = gfx.width_at(table, *element, if col == base { w } else { w + 0.5 });
                painter.line_segment([to_s(*a), to_s(*b)], Stroke::new(w, col));
            }
        }
        let hatch_st = Stroke::new(0.6, t.cut.gamma_multiply(0.35));
        for p in &d.prims {
            if let ViewPrim::SectionCut {
                table,
                element,
                material,
                rings,
                holes,
            } = p
            {
                let mut all: Vec<Vec<Pos2>> = vec![rings.iter().map(|&q| to_s(q)).collect()];
                all.extend(holes.iter().map(|h| h.iter().map(|&q| to_s(q)).collect()));
                let k = mat_class(material);
                let sel = selected(ed, table, *element);
                let is_hot = hot == Some((*table, *element));
                let base = gfx
                    .mat_color(material)
                    .or_else(|| crate::mep3d::cut_fill(material))
                    .unwrap_or(t.mat[k]);
                let base = gfx.fill_at(table, *element, base);
                let fill = t.ov().state_fill(sel, base);
                fill_even_odd(&painter, &all, fill);
                if !gfx.coarse() {
                    let h = gfx.mat_hatch(material);
                    let step = if h == "cross" { 9.0 } else { 6.0 };
                    crate::view_gfx_ui::paint_hatch(&painter, &all, h, step, hatch_st);
                }
                let w = gfx.width_at(
                    table,
                    *element,
                    if k == 3 || k == 4 { lw[1] } else { lw[0] },
                );
                let st = t.ov().state(
                    sel,
                    is_hot,
                    Stroke::new(w, gfx.line_at(table, *element, t.cut)),
                );
                for r in &all {
                    let mut pts = r.clone();
                    if let Some(&f) = pts.first() {
                        pts.push(f);
                    }
                    painter.add(egui::Shape::line(pts, st));
                }
            }
        }
        // storey height chain on the left: extension lines, 45-degree ticks, text along the line
        for p in &d.prims {
            if let ViewPrim::Dim {
                chain, a, b, value, ..
            } = p
            {
                let off = 28.0 + 22.0 * *chain as f32;
                let (pa, pb) = (to_s(*a), to_s(*b));
                let x = pa.x - off;
                let (da, db) = (egui::pos2(x, pa.y), egui::pos2(x, pb.y));
                let st = Stroke::new(lw[3], anno);
                for (g, q) in [(pa, da), (pb, db)] {
                    painter.line_segment([g - Vec2::new(4.0, 0.0), q - Vec2::new(4.0, 0.0)], st);
                    painter.line_segment([q + Vec2::new(-4.0, 4.0), q + Vec2::new(4.0, -4.0)], st);
                }
                painter.line_segment([da, db], st);
                let g = painter.layout_no_wrap(
                    format_length(*value, units),
                    egui::FontId::new(
                        ddc_ui::theme::type_size::CAPTION,
                        ddc_ui::theme::family(ddc_ui::theme::W_REGULAR),
                    ),
                    t.text,
                );
                let mid = egui::pos2(x - 3.0, (da.y + db.y) / 2.0 + g.size().x / 2.0);
                painter.add(
                    egui::epaint::TextShape::new(mid - Vec2::new(g.size().y, 0.0), g, t.text)
                        .with_angle(-std::f32::consts::FRAC_PI_2),
                );
            }
        }
        for p in &d.prims {
            if let ViewPrim::RoomTag {
                pos, name, number, ..
            } = p
            {
                let label = if number.is_empty() {
                    name.clone()
                } else {
                    format!("{number} · {name}")
                };
                let c = to_s(*pos);
                let g = painter.layout_no_wrap(label, small.clone(), t.text_dim);
                let r = egui::Rect::from_center_size(c, g.size()).expand2(Vec2::new(4.0, 2.0));
                painter.rect_filled(r, ddc_ui::theme::radius::SM, t.paper.gamma_multiply(0.9));
                painter.galley(r.min + Vec2::new(4.0, 2.0), g, t.text_dim);
            }
        }
        let anno_to_s = |q: V2| to_s((q.x, q.y));
        if let Some(l) = plan_level {
            // dimensions and tags of that level as its live plan draws them
            crate::modify_tools::dims::draw_level(&painter, ed, l, &anno_to_s, t);
            let tags = self.plan_tags(ed, l);
            crate::modify_tools::tags::draw_tag_list(&painter, tags, &anno_to_s, t, None);
        }
        crate::annot_text::draw(ui, &painter, rect, ed, t, &anno_to_s, anno_view);
        // tool preview (session, else the wall draft of the plan) as its cut by this view
        let ad = spec.as_ref().filter(|_| plan_level.is_none()).and_then(|sp| self.adapter(id, sp, c0));
        if let Some(ad) = ad {
            let prims = ed.preview_prims();
            if !prims.0.is_empty() {
                let draws = crate::interact::preview::draws(&prims.0, &ad);
                crate::overlay::paint_preview(&painter, &draws, t.ov(), &|v| {
                    crate::units::display_length(v, units)
                });
            }
        }
        // view title
        if let Some(sp) = &spec {
            let kind = match sp.kind {
                ViewKind::Section => crate::tr("edit_panels.sv.section", &[]),
                ViewKind::Elevation => crate::tr("edit_panels.sv.elevation", &[]),
            };
            let name = display_name(sp);
            let kind_other = match sp.kind {
                ViewKind::Section => crate::tr_lang(!crate::ui_lang_now(), "edit_panels.sv.section", &[]),
                ViewKind::Elevation => crate::tr_lang(!crate::ui_lang_now(), "edit_panels.sv.elevation", &[]),
            };
            let title = if name.starts_with(&kind)
                || name.starts_with(&kind_other)
                || name.to_lowercase().contains(&kind.to_lowercase())
            {
                name
            } else {
                format!("{kind} · {name}")
            };
            let g = painter.layout_no_wrap(title, strong, t.text);
            let r = egui::Rect::from_min_size(rect.min + Vec2::new(16.0, 12.0), g.size())
                .expand2(Vec2::new(8.0, 4.0));
            painter.rect(
                r,
                4.0,
                t.panel,
                Stroke::new(1.0, t.border),
                egui::StrokeKind::Inside,
            );
            painter.galley(rect.min + Vec2::new(16.0, 12.0), g, t.text);
        }
        match plan_level {
            Some(l) => crate::wp_views::paint_level(&painter, ed, l, rect, &to_s, &to_w, t),
            None => crate::wp_views::paint_section(&painter, ed, id, rect, &to_s, &to_w, t),
        }
        if let (None, Some(sp)) = (plan_level, &spec) {
            if let Some(ad) = self.adapter(id, sp, c0) {
                let to_screen = |q: [f64; 3]| crate::tools::measure::section_screen(&ad, q);
                crate::tools::measure::draw_section(&painter, ed, id, &to_screen, t);
            }
        }
        if let Some(h) = &hud {
            crate::section_hud::paint(&painter, h, fit_hot, t);
        }
        if level_tool {
            crate::tools::level_overlay(&painter, rect, t, ed, hover.map(|h| (h, to_w(h).y)));
            if let Some(q) = resp
                .clicked()
                .then(|| resp.interact_pointer_pos())
                .flatten()
            {
                crate::tools::level_place(ed, to_w(q).y);
            }
        }
    }
}

fn dash_dot(p: &egui::Painter, a: Pos2, b: Pos2, st: Stroke) {
    let len = (b - a).length();
    if len < 1.0 {
        return;
    }
    let d = (b - a) / len;
    let mut x = 0.0;
    while x < len {
        let e = (x + 16.0).min(len);
        p.line_segment([a + d * x, a + d * e], st);
        let dot = x + 20.0;
        if dot < len {
            p.line_segment([a + d * dot, a + d * (dot + 1.5).min(len)], st);
        }
        x += 24.0;
    }
}

/// Short view label in a mark: section number or compass-side letter.
pub fn mark_label(s: &SectionSpec) -> String {
    match s.name.as_str() {
        "North" => crate::tr("edit_panels.sv.mark.north", &[]),
        "South" => crate::tr("edit_panels.sv.mark.south", &[]),
        "East" => crate::tr("edit_panels.sv.mark.east", &[]),
        "West" => crate::tr("edit_panels.sv.mark.west", &[]),
        n => n
            .rsplit(' ')
            .next()
            .filter(|w| !w.is_empty() && w.chars().count() <= 3)
            .unwrap_or("?")
            .to_string(),
    }
}

/// Section and elevation marks on the plan; handles on the view under the cursor or being dragged;
/// the draft of the "Section" tool is drawn in the preview colour.
pub fn plan_marks(
    painter: &egui::Painter,
    ed: &Editor,
    to_s: &impl Fn(V2) -> Pos2,
    t: &Tokens,
    px: f64,
    cursor: Option<V2>,
    preview: Color32,
) {
    let near = |s: &SectionSpec| {
        cursor.is_some_and(|c| {
            let (a, b) = sec::ends(s);
            seg_dist(to_s(c), to_s(a), to_s(b)) < 36.0
                || sec::grips(s, px)
                    .iter()
                    .any(|(_, q)| (to_s(*q) - to_s(c)).length() < 24.0)
        })
    };
    for (id, _) in &ed.views {
        let Some(s) = ed.live_view(*id) else { continue };
        let dragging = ed.vdrag.as_ref().is_some_and(|d| d.id == *id);
        let col = if dragging { t.accent } else { t.cut };
        draw_mark(painter, s, to_s, t, col);
        if let Some(l) = crate::sheet_view::xref_label(ed, *id) {
            painter.text(
                to_s(sec::ends(s).1) + Vec2::new(10.0, -14.0),
                egui::Align2::LEFT_BOTTOM,
                l,
                ddc_ui::typo::font(ddc_ui::theme::Role::CanvasLabel),
                t.text_dim,
            );
        }
        if dragging || (!ed.tool_armed() && near(s)) {
            draw_grips(painter, s, to_s, t, px);
        }
    }
    if let Some(tl) = ed.section_tool() {
        if let (Some(a), None, Some(c)) = (tl.a, tl.b, tl.cursor) {
            painter.circle_filled(to_s(a), 3.0, preview);
            painter.circle_filled(to_s(c), 3.0, preview);
        }
        if let Some(s) = ed.section_preview() {
            draw_mark(painter, &s, to_s, t, preview);
        }
    }
}

fn draw_mark(
    painter: &egui::Painter,
    s: &SectionSpec,
    to_s: &impl Fn(V2) -> Pos2,
    t: &Tokens,
    col: Color32,
) {
    let (a, b) = sec::ends(s);
    let n = sec::look(s);
    let (pa, pb) = (to_s(a), to_s(b));
    let len = (pb - pa).length();
    if len < 2.0 {
        return;
    }
    let dir = (pb - pa) / len;
    let nn = (to_s(a + n) - pa).normalized();
    let label = mark_label(s);
    let font = egui::FontId::new(
        ddc_ui::theme::type_size::CAPTION,
        ddc_ui::theme::family(ddc_ui::theme::W_SEMIBOLD),
    );
    match s.kind {
        ViewKind::Section => {
            // view depth: thin dashed line
            let far = [to_s(a + n * s.depth), to_s(b + n * s.depth)];
            let faint = Stroke::new(1.0, col.gamma_multiply(0.28));
            painter.extend(egui::Shape::dashed_line(
                &[pa, far[0], far[1], pb],
                faint,
                5.0,
                4.0,
            ));
            // line: thin dash-dot, thickened dashes at the ends
            dash_dot(painter, pa, pb, Stroke::new(1.0, col));
            let end = 18.0f32.min(len / 2.0);
            painter.line_segment([pa, pa + dir * end], Stroke::new(2.6, col));
            painter.line_segment([pb, pb - dir * end], Stroke::new(2.6, col));
            for (p, out) in [(pa, -dir), (pb, dir)] {
                // viewing arrow at the end and a numbered circle behind it
                let base = p + out * 4.0;
                painter.add(egui::Shape::convex_polygon(
                    vec![base + nn * 14.0, base - out * 5.0, base + out * 5.0],
                    col,
                    Stroke::NONE,
                ));
                let c = p + out * 16.0;
                painter.circle(c, 10.0, t.paper, Stroke::new(1.4, col));
                painter.text(c, egui::Align2::CENTER_CENTER, &label, font.clone(), col);
            }
        }
        ViewKind::Elevation => {
            let m = to_s((a + b) * 0.5);
            let faint = Stroke::new(1.0, col.gamma_multiply(0.22));
            painter.extend(egui::Shape::dashed_line(&[pa, pb], faint, 3.0, 5.0));
            // elevation mark: a circle with a letter and a filled wedge toward the view direction
            let r = 11.0;
            let tip = m + nn * (r + 9.0);
            let side = nn.rot90() * (r * 0.75);
            painter.add(egui::Shape::convex_polygon(
                vec![tip, m + side, m - side],
                col,
                Stroke::NONE,
            ));
            painter.circle(m, r, t.paper, Stroke::new(1.4, col));
            painter.text(m, egui::Align2::CENTER_CENTER, &label, font, col);
        }
    }
}

fn draw_grips(
    painter: &egui::Painter,
    s: &SectionSpec,
    to_s: &impl Fn(V2) -> Pos2,
    t: &Tokens,
    px: f64,
) {
    use crate::overlay::{self, Grip as Shape};
    let ov = t.ov();
    let cursor = painter.ctx().input(|i| i.pointer.hover_pos());
    for (g, q) in sec::grips(s, px) {
        let p = to_s(q);
        let hot = cursor.is_some_and(|h| h.distance(p) < overlay::GRIP_HIT_PX);
        match g {
            Grip::A | Grip::B => {
                if s.kind == ViewKind::Section {
                    overlay::grip(painter, p, Shape::Square, hot, ov);
                }
            }
            Grip::Depth => {
                if s.kind == ViewKind::Section {
                    overlay::grip(painter, p, Shape::Diamond, hot, ov);
                }
            }
            // flip: a double arrow across the line
            Grip::Flip => {
                let dir = to_s(q + sec::look(s)) - p;
                overlay::grip(painter, p, Shape::Arrows(dir), hot, ov);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_classes() {
        assert_eq!(mat_class("Concrete"), 0);
        assert_eq!(mat_class("masonry"), 1);
        assert_eq!(mat_class("insulation"), 2);
        assert_eq!(mat_class("Glass"), 3);
        assert_eq!(mat_class("Door"), 4);
        assert_eq!(mat_class("gypsum"), 5);
    }

    #[test]
    fn even_odd_inside_respects_holes() {
        let r = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)];
        let h = vec![vec![(1.0, 1.0), (1.0, 3.0), (3.0, 3.0), (3.0, 1.0)]];
        assert!(inside(v2(0.5, 0.5), &r, &h));
        assert!(!inside(v2(2.0, 2.0), &r, &h));
    }

    #[test]
    fn mark_labels() {
        let name = "\u{420}\u{430}\u{437}\u{440}\u{435}\u{437} 12";
        let mut s = sec::spec_from(name, v2(0.0, 0.0), v2(1.0, 0.0), None).unwrap();
        assert_eq!(mark_label(&s), "12");
        s.name = "South".into();
        // the cardinal mark follows the UI language of this thread, set explicitly
        assert_eq!(crate::with_test_lang(true, || mark_label(&s)), "\u{42e}");
        assert_eq!(crate::with_test_lang(false, || mark_label(&s)), "S");
    }
}
