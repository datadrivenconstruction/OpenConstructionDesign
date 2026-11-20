// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Slab shape editing: change a vertex/point elevation, add a point, a split line,
//! a drain sump with automatic slope; variable or constant thickness. Over the plan: fill by dz,
//! drain arrows on each face (downslope) and live volume/top-area readouts.
//! Enter commits a single `apply_ext` record (one undo step); Esc leaves the model unchanged.

use crate::editor::Editor;
use crate::tools::Rec;
use crate::ui_kit::Tokens;
use crate::units::{DecimalMark, InputError};
use crate::wall_tool::{Event, Key};
use ddc_geom::shape_edit::{ShapeLine, ShapeMesh, SlabShape, Thickness};
use ddc_geom::{v2, V2};
use egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeTool {
    /// Click a contour vertex or point to set its `dz`.
    Modify,
    AddPoint,
    SplitLine,
    /// Drain sump: the point is lowered by `dz` (depth), with lines to the contour vertices.
    Drain,
}

impl ShapeTool {
    pub fn label(self) -> String {
        self.label_in(crate::ui_is_ru())
    }

    pub fn label_in(self, ru: bool) -> String {
        let k = match self {
            ShapeTool::Modify => "sketch_edit.shape_modify",
            ShapeTool::AddPoint => "sketch_edit.shape_add",
            ShapeTool::SplitLine => "sketch_edit.shape_split",
            ShapeTool::Drain => "sketch_edit.shape_drain",
        };
        crate::tr_lang(ru, k, &[])
    }
}

#[derive(Clone, Debug)]
pub struct ShapeEditor {
    pub slab: i64,
    pub rings: Vec<Vec<V2>>,
    pub thickness: f64,
    pub shape: SlabShape,
    pub tool: ShapeTool,
    /// Elevation for the next click, m (negative is down; for a sump, the depth).
    pub dz: f64,
    pub pending: Option<V2>,
    pub cursor: Option<V2>,
    pub mesh: Option<ShapeMesh>,
    pub error: Option<String>,
    pub radius: f64,
    /// Selected sub-element (handle): the elevation is set by a typed number + Enter.
    pub grip: Option<Grip>,
    /// Dragging a point/line-end handle (button held).
    pub drag: Option<Grip>,
    /// Typed elevation, kept raw; read on Enter in the project units with `mark`.
    pub input: String,
    pub units: crate::units::Units,
    /// Decimal mark of the language `input` is read in.
    pub mark: crate::units::DecimalMark,
}

/// Shape sub-element: contour vertex (ring, index), point, line end (line, end b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    Vertex(usize, usize),
    Point(usize),
    LineEnd(usize, bool),
}

impl ShapeEditor {
    pub fn new(slab: i64, rings: Vec<Vec<V2>>, thickness: f64, shape: Option<SlabShape>) -> Self {
        let mut s = Self {
            slab,
            rings,
            thickness,
            shape: shape.unwrap_or_default(),
            tool: ShapeTool::Modify,
            dz: -0.05,
            pending: None,
            cursor: None,
            mesh: None,
            error: None,
            radius: 0.2,
            grip: None,
            drag: None,
            input: String::new(),
            units: crate::units::active_units(),
            mark: crate::units::ui_decimal_mark(),
        };
        s.rebuild();
        s
    }

    pub fn rebuild(&mut self) {
        match self.shape.build(&self.rings) {
            Ok(m) => {
                self.mesh = Some(m);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(crate::tr(
                    "sketch_edit.shape_build_failed",
                    &[("err", &format!("{e:?}"))],
                ))
            }
        }
    }

    pub fn set_mode(&mut self, mode: Thickness) {
        self.shape.mode = mode;
        self.rebuild();
    }

    /// (volume, top area).
    pub fn quantities(&self) -> Option<(f64, f64)> {
        self.mesh
            .as_ref()
            .map(|m| (m.volume(self.thickness), m.top_area()))
    }

    fn near_vertex(&self, p: V2) -> Option<(usize, usize)> {
        self.rings
            .iter()
            .enumerate()
            .flat_map(|(r, ring)| ring.iter().enumerate().map(move |(k, q)| (r, k, *q)))
            .filter(|(_, _, q)| (*q - p).len() <= self.radius)
            .min_by(|a, b| (a.2 - p).len().total_cmp(&(b.2 - p).len()))
            .map(|(r, k, _)| (r, k))
    }

    /// Handle under a point: added points and line ends first, then contour vertices.
    pub fn grip_at(&self, p: V2) -> Option<Grip> {
        let r = self.radius;
        if let Some(i) = self.shape.points.iter().position(|q| (q.0 - p).len() <= r) {
            return Some(Grip::Point(i));
        }
        for (i, l) in self.shape.lines.iter().enumerate() {
            if (l.a - p).len() <= r {
                return Some(Grip::LineEnd(i, false));
            }
            if (l.b - p).len() <= r {
                return Some(Grip::LineEnd(i, true));
            }
        }
        self.near_vertex(p).map(|(r, k)| Grip::Vertex(r, k))
    }

    /// Sub-element elevation (dz, m).
    pub fn set_grip_dz(&mut self, g: Grip, dz: f64) {
        match g {
            Grip::Vertex(r, k) => {
                let sh = &mut self.shape.ring_dz;
                if sh.len() <= r {
                    sh.resize(r + 1, vec![]);
                }
                if sh[r].len() < self.rings[r].len() {
                    sh[r].resize(self.rings[r].len(), 0.0);
                }
                sh[r][k] = dz;
            }
            Grip::Point(i) => self.shape.points[i].1 = dz,
            Grip::LineEnd(i, b) => {
                let l = &mut self.shape.lines[i];
                if b {
                    l.dzb = dz;
                } else {
                    l.dza = dz;
                }
            }
        }
    }

    pub fn handle(&mut self, e: Event) -> Option<bool> {
        match e {
            Event::Move(p) => {
                self.cursor = Some(p);
                // a point or line-end handle follows the cursor; contour vertices stay fixed
                match self.drag {
                    Some(Grip::Point(i)) => self.shape.points[i].0 = p,
                    Some(Grip::LineEnd(i, b)) => {
                        let l = &mut self.shape.lines[i];
                        if b {
                            l.b = p;
                        } else {
                            l.a = p;
                        }
                    }
                    _ => {}
                }
                if matches!(self.drag, Some(Grip::Point(_) | Grip::LineEnd(..))) {
                    self.rebuild();
                }
            }
            Event::Press(p) if self.tool == ShapeTool::Modify => {
                self.drag = self.grip_at(p).filter(|g| !matches!(g, Grip::Vertex(..)));
            }
            Event::Release(_) => self.drag = None,
            Event::Char(c) if elev_char(c, &self.input) => {
                if self.input.len() < 32 {
                    self.input.push(c);
                }
            }
            Event::Key(Key::Enter) if !self.input.trim().is_empty() => {
                let t = self.input.trim().to_string();
                match crate::units::parse_typed_signed(&t, self.units, self.mark) {
                    Ok(dz) => {
                        self.input.clear();
                        self.dz = dz;
                        if let Some(g) = self.grip {
                            self.set_grip_dz(g, self.dz);
                            self.rebuild();
                        }
                    }
                    // the text stays for correction; dz and the shape are untouched
                    Err(e) => self.error = Some(elev_refused(&e, &t, self.mark)),
                }
            }
            Event::Click(p) => {
                self.click(p);
                self.rebuild();
            }
            Event::Key(Key::Backspace) if self.input.pop().is_some() => {}
            Event::Key(Key::Backspace) => {
                self.grip = None;
                if self.shape.lines.pop().is_none() {
                    self.shape.points.pop();
                }
                self.rebuild();
            }
            Event::Key(Key::Tab) => {
                let m = if self.shape.mode == Thickness::Varying {
                    Thickness::Constant
                } else {
                    Thickness::Varying
                };
                self.set_mode(m);
            }
            Event::Key(Key::Enter) => return Some(true),
            Event::Key(Key::Escape) if self.pending.take().is_none() => return Some(false),
            _ => {}
        }
        None
    }

    fn click(&mut self, p: V2) {
        match self.tool {
            ShapeTool::Modify => match self.grip_at(p) {
                // a click sets the current elevation and selects the handle for number entry
                Some(g) => {
                    self.set_grip_dz(g, self.dz);
                    self.grip = Some(g);
                }
                None => {
                    self.grip = None;
                    self.error = Some(crate::tr("sketch_edit.shape_no_target", &[]));
                }
            },
            ShapeTool::AddPoint => self.shape.points.push((p, self.dz)),
            ShapeTool::SplitLine => match self.pending.take() {
                None => self.pending = Some(p),
                Some(a) => {
                    let z = |q: V2| self.mesh.as_ref().and_then(|m| m.dz_at(q)).unwrap_or(0.0);
                    let (dza, dzb) = (z(a), z(p));
                    self.shape.lines.push(ShapeLine { a, b: p, dza, dzb });
                }
            },
            ShapeTool::Drain => {
                let mode = self.shape.mode;
                self.shape = SlabShape::drain(&self.rings[0], p, self.dz.abs());
                self.shape.mode = mode;
            }
        }
    }

    pub fn record(&self) -> Result<Rec, String> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let (id, sh) = (self.slab, self.shape.clone());
        Ok(Rec::new("slab shape", move |m| {
            m.set_slab_shape(id, Some(&sh))?;
            Ok(1)
        }))
    }
}

/// Enter slab shape editing.
pub fn begin(ed: &mut Editor, slab: i64) -> Result<(), String> {
    let rings: Vec<Vec<V2>> = ed
        .model
        .slab_rings(slab)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| r.into_iter().map(|(x, y)| v2(x, y)).collect())
        .collect();
    let t: f64 = ed
        .model
        .sql()
        .query_row(
            "SELECT COALESCE(SUM(l.thickness),0) FROM slab s JOIN slab_type_layer l ON l.stype=s.stype WHERE s.id=?1",
            [slab],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let shape = ed.model.slab_shape(slab).map_err(|e| e.to_string())?;
    let mut s = ShapeEditor::new(slab, rings, t, shape);
    s.radius = ed.tool.radius.max(1e-3);
    s.units = ed.tool.units;
    ed.shape = Some(s);
    Ok(())
}

pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let Some(mut s) = ed.shape.take() else {
        return false;
    };
    match s.handle(e) {
        None => ed.shape = Some(s),
        Some(false) => ed.status = Some(crate::tr("sketch_edit.shape_cancelled", &[])),
        Some(true) => match s
            .record()
            .and_then(|r| ed.apply_ext(r).map_err(|e| e.to_string()))
        {
            Ok(()) => ed.status = Some(crate::tr("sketch_edit.shape_applied", &[])),
            Err(m) => {
                s.error = Some(m.clone());
                ed.status = Some(m);
                ed.shape = Some(s);
            }
        },
    }
    true
}

/// Colour by dz: blue below zero, warm above; saturation by |dz| / `span`.
fn tint(dz: f64, span: f64, t: &Tokens) -> Color32 {
    let k = (dz.abs() / span.max(1e-6)).min(1.0) as f32;
    let base = if dz < 0.0 {
        t.sem().canvas_dz_below
    } else {
        t.sem().canvas_dz_above
    };
    base.gamma_multiply(0.12 + 0.5 * k)
}

pub fn paint(p: &egui::Painter, rect: Rect, ed: &Editor, t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    let Some(s) = ed.shape.as_ref() else { return };
    let small = egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION);
    if let Some(m) = &s.mesh {
        let span = m.dz.iter().map(|z| z.abs()).fold(0.0, f64::max);
        for tri in &m.tris {
            let pts: Vec<Pos2> = tri.iter().map(|&k| to_s(m.pts[k])).collect();
            let avg = (m.dz[tri[0]] + m.dz[tri[1]] + m.dz[tri[2]]) / 3.0;
            p.add(egui::Shape::convex_polygon(
                pts.clone(),
                tint(avg, span, t),
                Stroke::new(0.6, t.text_dim.gamma_multiply(ddc_ui::sem::fade::MESH_EDGE)),
            ));
            // drain: downhill along the face gradient
            let (a, b, c) = (m.pts[tri[0]], m.pts[tri[1]], m.pts[tri[2]]);
            let (za, zb, zc) = (m.dz[tri[0]], m.dz[tri[1]], m.dz[tri[2]]);
            let det = (b - a).cross(c - a);
            if det.abs() > 1e-12 {
                let gx = ((zb - za) * (c.y - a.y) - (zc - za) * (b.y - a.y)) / det;
                let gy = ((zc - za) * (b.x - a.x) - (zb - za) * (c.x - a.x)) / det;
                let g = gx.hypot(gy);
                if g > 1e-6 {
                    let cen = (a + b + c) * (1.0 / 3.0);
                    let (sc, d) = (to_s(cen), Vec2::new(-gx as f32, gy as f32) / g as f32);
                    let tip = sc + d * 14.0;
                    p.line_segment([sc - d * 10.0, tip], Stroke::new(1.4, t.text));
                    let n = Vec2::new(-d.y, d.x);
                    p.add(egui::Shape::convex_polygon(
                        vec![tip, tip - d * 6.0 + n * 3.5, tip - d * 6.0 - n * 3.5],
                        t.text,
                        Stroke::NONE,
                    ));
                    p.text(
                        sc + n * 10.0,
                        egui::Align2::CENTER_CENTER,
                        format!("{:.1} %", g * 100.0),
                        small.clone(),
                        t.text_dim,
                    );
                }
            }
        }
        // points with a non-zero elevation
        for (k, q) in m.pts.iter().enumerate() {
            if m.dz[k] != 0.0 {
                let c = to_s(*q);
                p.circle_filled(c, 4.0, t.accent);
                p.text(
                    c + Vec2::new(6.0, -6.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{:+.0}", m.dz[k] * 1000.0),
                    small.clone(),
                    t.text,
                );
            }
        }
    }
    for l in &s.shape.lines {
        p.line_segment([to_s(l.a), to_s(l.b)], Stroke::new(1.8, t.accent));
    }
    for ring in &s.rings {
        let pts: Vec<Pos2> = ring.iter().map(|&q| to_s(q)).collect();
        p.add(egui::Shape::closed_line(
            pts.clone(),
            Stroke::new(2.0, t.accent),
        ));
        for q in pts {
            p.rect_stroke(
                Rect::from_center_size(q, Vec2::splat(7.0)),
                1.0,
                Stroke::new(1.2, t.accent),
                egui::StrokeKind::Middle,
            );
        }
    }
    // selected handle: a ring and the elevation entry field
    let gp = s.grip.and_then(|g| match g {
        Grip::Vertex(r, k) => s.rings.get(r).and_then(|v| v.get(k)).copied(),
        Grip::Point(i) => s.shape.points.get(i).map(|q| q.0),
        Grip::LineEnd(i, b) => s.shape.lines.get(i).map(|l| if b { l.b } else { l.a }),
    });
    if let Some(q) = gp {
        let c = to_s(q);
        p.circle_stroke(c, 9.0, Stroke::new(2.0, t.accent));
        let label = elev_label(&s.input, s.units, crate::ui_is_ru());
        let r = Rect::from_min_size(c + Vec2::new(12.0, 8.0), Vec2::new(110.0, 22.0));
        p.rect_filled(r, ddc_ui::theme::radius::MD, t.panel);
        p.rect_stroke(r, 4.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        p.text(
            r.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
            t.text,
        );
    }
    if let (Some(a), Some(c)) = (s.pending, s.cursor) {
        p.extend(egui::Shape::dashed_line(
            &[to_s(a), to_s(c)],
            Stroke::new(1.2, t.ov().preview),
            6.0,
            4.0,
        ));
    }
    let bar = Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, 12.0),
        Vec2::new(rect.width().min(760.0) - 24.0, 58.0),
    );
    p.rect_filled(bar, ddc_ui::theme::radius::MD, t.panel);
    p.rect_stroke(
        bar,
        6.0,
        Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );
    let f = egui::FontId::proportional(ddc_ui::theme::type_size::BODY);
    p.text(
        bar.left_top() + Vec2::new(12.0, 10.0),
        egui::Align2::LEFT_TOP,
        hud_title(
            s.tool,
            s.dz,
            s.shape.mode,
            crate::ui_is_ru(),
            crate::units::active_units(),
        ),
        f.clone(),
        t.text,
    );
    let (msg, col) = match (&s.error, s.quantities()) {
        (Some(e), _) => (e.clone(), t.ov().error),
        (None, Some((v, a))) => (
            hud_qty(v, a, crate::ui_is_ru(), crate::units::active_units()),
            t.text_dim,
        ),
        _ => (String::new(), t.text_dim),
    };
    p.text(
        bar.left_top() + Vec2::new(12.0, 32.0),
        egui::Align2::LEFT_TOP,
        msg,
        f,
        col,
    );
}

/// Top line of the mode bar: tool, dz and thickness mode in project units.
pub fn hud_title(
    tool: ShapeTool,
    dz: f64,
    mode: Thickness,
    ru: bool,
    u: crate::units::Units,
) -> String {
    let mode = match mode {
        Thickness::Varying => "sketch_edit.shape_varying",
        Thickness::Constant => "sketch_edit.shape_constant",
    };
    crate::tr_lang(
        ru,
        "sketch_edit.shape_hud",
        &[
            ("tool", &tool.label_in(ru)),
            ("dz", &crate::unit_text::len(dz, ru, u)),
            ("mode", &crate::tr_lang(ru, mode, &[])),
        ],
    )
}

/// The elevation box keeps any printable character as typed (a dropped one could turn
/// `300 mm` into inches); the parser refuses what is not a length, with a reason.
fn elev_char(c: char, input: &str) -> bool {
    !c.is_control() && !(c.is_whitespace() && input.is_empty())
}

/// Visible reason a typed elevation was not taken, in the language of `mark`.
pub fn elev_refused(e: &InputError, t: &str, mark: DecimalMark) -> String {
    let ru = mark == DecimalMark::Comma;
    // one value only: the pair-separator hint of the tool bar does not apply here
    crate::tr_lang(ru, "host_shape.shape.bad_elev", &[("t", t), ("why", e.text(ru).as_str())])
}

/// Elevation box text in the project units: a prompt, or the typed text with the unit a bare
/// number is read in (mm in metric, inches in imperial).
pub fn elev_label(input: &str, u: crate::units::Units, ru: bool) -> String {
    let imperial = u == crate::units::Units::Imperial;
    let t = input.trim();
    if t.is_empty() {
        let k = if imperial { "host_shape.shape.elev_prompt_in" } else { "host_shape.shape.elev_prompt" };
        return crate::tr_lang(ru, k, &[]);
    }
    if t.ends_with(|c: char| c.is_alphabetic() || c == '\'' || c == '"') {
        return t.to_string();
    }
    let k = if imperial { "host_shape.shape.elev_in" } else { "host_shape.shape.elev_mm" };
    crate::tr_lang(ru, k, &[("v", t)])
}

/// Bottom line: volume and top area in project units.
pub fn hud_qty(v: f64, a: f64, ru: bool, u: crate::units::Units) -> String {
    crate::tr_lang(
        ru,
        "sketch_edit.shape_qty",
        &[
            ("v", &crate::unit_text::vol(v, ru, u)),
            ("a", &crate::unit_text::area(a, ru, u)),
        ],
    )
}

/// Scene of the `--tool-demo E3` screenshot: a 12x8 slab with a sump and a ridge line.
pub fn demo(app: &mut crate::shell::EditorApp, id: &str) -> Option<Result<(), String>> {
    if id != "E3" {
        return None;
    }
    let r = (|| -> Result<(), String> {
        let m = &mut app.ed.model;
        let st = m
            .add_slab_type(
                crate::tr("sketch_edit.seed_roof_slab", &[]).as_str(),
                &[ddc_model::LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.25,
                }],
            )
            .map_err(|e| e.to_string())?;
        let s = m
            .add_slab(&ddc_model::SlabSpec {
                level: app.ed.level,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (12.0, 0.0), (12.0, 8.0), (0.0, 8.0)],
                holes: vec![],
            })
            .map_err(|e| e.to_string())?;
        app.ed.reload().map_err(|e| e.to_string())?;
        app.plan.center = v2(6.0, 4.0);
        app.plan.scale = 52.0;
        app.ed.tool.radius = 12.0 / 52.0;
        begin(&mut app.ed, s)?;
        let e = app.ed.shape.as_mut().unwrap();
        e.dz = -0.08;
        e.tool = ShapeTool::Drain;
        e.handle(Event::Click(v2(4.0, 4.0)));
        e.tool = ShapeTool::SplitLine;
        e.dz = -0.04;
        e.cursor = Some(v2(10.5, 6.5));
        e.pending = Some(v2(8.0, 2.0));
        Ok(())
    })();
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{Units, FOOT, INCH};

    const EN: DecimalMark = DecimalMark::Point;
    const RU: DecimalMark = DecimalMark::Comma;

    fn square() -> Vec<Vec<V2>> {
        vec![vec![v2(0.0, 0.0), v2(10.0, 0.0), v2(10.0, 10.0), v2(0.0, 10.0)]]
    }

    /// Types `text` into a fresh editor with an explicit language and units, then Enter.
    fn typed(text: &str, u: Units, m: DecimalMark) -> ShapeEditor {
        let mut s = ShapeEditor::new(1, square(), 0.3, None);
        s.units = u;
        s.mark = m;
        s.grip = Some(Grip::Point(0));
        s.shape.points.push((v2(5.0, 5.0), 0.0));
        text.chars().for_each(|c| {
            s.handle(Event::Char(c));
        });
        assert_eq!(s.handle(Event::Key(Key::Enter)), None);
        s
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn typed_elevation_reads_project_units_and_locale() {
        let cases: [(&str, Units, DecimalMark, f64); 14] = [
            ("1'", Units::Imperial, EN, FOOT),
            ("1'", Units::Imperial, RU, FOOT),
            ("1'-6\"", Units::Imperial, EN, 1.5 * FOOT),
            ("12", Units::Imperial, EN, 12.0 * INCH),
            ("300 mm", Units::Imperial, EN, 0.3),
            ("1 500", Units::Metric, EN, 1.5),
            ("1 500", Units::Metric, RU, 1.5),
            ("1\u{a0}500", Units::Metric, RU, 1.5),
            // English comma groups thousands: 1500 mm, never 1.5 mm
            ("1,500", Units::Metric, EN, 1.5),
            ("1,5", Units::Metric, RU, 0.0015),
            ("1,500", Units::Metric, RU, 0.0015),
            ("-50", Units::Metric, EN, -0.05),
            ("\u{2212}1,5 m", Units::Metric, RU, -1.5),
            ("0", Units::Metric, EN, 0.0),
        ];
        for (t, u, m, want) in cases {
            let s = typed(t, u, m);
            assert!(near(s.dz, want), "{t:?} {u:?} {m:?}: {}", s.dz);
            assert!(near(s.shape.points[0].1, want), "{t:?}: grip not set");
            assert!(s.input.is_empty() && s.error.is_none(), "{t:?}");
        }
    }

    #[test]
    fn ambiguous_or_bad_elevation_is_refused_visibly() {
        let cases: [(&str, Units, DecimalMark); 7] = [
            ("1,5", Units::Metric, EN),
            ("1,5 m", Units::Metric, EN),
            ("6000,4000", Units::Metric, RU),
            ("abc", Units::Metric, EN),
            ("1.5 yd", Units::Metric, EN),
            ("3 0000", Units::Metric, RU),
            ("12,5\"", Units::Imperial, EN),
        ];
        for (t, u, m) in cases {
            let s = typed(t, u, m);
            assert!(near(s.dz, -0.05), "{t:?} {m:?}: elevation changed to {}", s.dz);
            assert!(near(s.shape.points[0].1, 0.0), "{t:?}: grip changed");
            assert_eq!(s.input, t, "{t:?}: refused text stays for correction");
            let why = s.error.unwrap_or_default();
            let raw = |k: &str| why.contains(&[k, "."].concat());
            assert!(!why.is_empty() && !raw("host_shape") && !raw("pair_input"), "{why}");
            assert!(why.contains(t), "{t:?}: the hint names the text: {why}");
        }
        // the hint speaks the language of the mark, whatever the UI language is
        let en = elev_refused(&InputError::CommaDecimal, "1,5", EN);
        let ru = elev_refused(&InputError::CommaDecimal, "1,5", RU);
        assert_ne!(en, ru);
        assert!(en.is_ascii(), "{en}");
        assert!(ru.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)), "{ru}");
    }

    #[test]
    fn elevation_box_shows_the_project_unit() {
        for ru in [false, true] {
            let mm = elev_label("150", Units::Metric, ru);
            let inch = elev_label("150", Units::Imperial, ru);
            assert!(mm.starts_with("150") && mm != "150" && mm != inch, "{mm} / {inch}");
            assert_eq!(elev_label("1'-6\"", Units::Imperial, ru), "1'-6\"");
            assert_eq!(elev_label("300 mm", Units::Imperial, ru), "300 mm");
            let pm = elev_label("", Units::Metric, ru);
            let pi = elev_label("", Units::Imperial, ru);
            assert!(!pm.is_empty() && !pi.is_empty() && pm != pi);
            for s in [mm, inch, pm, pi] {
                let raw = |k: &str| s.contains(&[k, "."].concat());
                assert!(!raw("host_shape"), "raw key: {s}");
            }
        }
        assert_ne!(elev_label("", Units::Imperial, false), elev_label("", Units::Imperial, true));
    }

    #[test]
    fn refused_elevation_then_escape_leaves_the_model_untouched() {
        let mut ed = Editor::new_as_app().unwrap();
        let st = ed
            .model
            .add_slab_type(
                "s",
                &[ddc_model::LayerSpec {
                    material: "c".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        let s = ed
            .model
            .add_slab(&ddc_model::SlabSpec {
                level: ed.level,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
                holes: vec![],
            })
            .unwrap();
        ed.reload().unwrap();
        let before = format!("{:?}", ed.model.slab_shape(s).unwrap());
        begin(&mut ed, s).unwrap();
        {
            let e = ed.shape.as_mut().unwrap();
            e.units = Units::Metric;
            e.mark = EN;
            e.grip = Some(Grip::Vertex(0, 2));
            "1,5".chars().for_each(|c| {
                e.handle(Event::Char(c));
            });
        }
        // Enter with refused text keeps the mode open and applies nothing
        ed.handle(Event::Key(Key::Enter));
        let e = ed.shape.as_ref().expect("still editing");
        assert!(e.error.is_some() && e.input == "1,5");
        assert_eq!(format!("{:?}", ed.model.slab_shape(s).unwrap()), before);
        ed.handle(Event::Key(Key::Escape));
        assert!(ed.shape.is_none());
        assert_eq!(format!("{:?}", ed.model.slab_shape(s).unwrap()), before);
        // the same corner typed in English with a thousands group: 1500 mm, one apply
        begin(&mut ed, s).unwrap();
        {
            let e = ed.shape.as_mut().unwrap();
            e.units = Units::Metric;
            e.mark = EN;
            e.grip = Some(Grip::Vertex(0, 2));
            "1,500".chars().for_each(|c| {
                e.handle(Event::Char(c));
            });
            e.handle(Event::Key(Key::Enter));
            assert!(near(e.dz, 1.5), "{}", e.dz);
        }
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.shape.is_none());
        let sh = ed.model.slab_shape(s).unwrap().expect("shape stored");
        assert!(near(sh.ring_dz[0][2], 1.5), "{:?}", sh.ring_dz);
        ed.undo();
        assert_eq!(format!("{:?}", ed.model.slab_shape(s).unwrap()), before);
    }

    #[test]
    fn drain_then_finish_is_one_undo_with_exact_volume() {
        let mut ed = Editor::new_as_app().unwrap();
        let st = ed
            .model
            .add_slab_type(
                "s",
                &[ddc_model::LayerSpec {
                    material: "c".into(),
                    thickness: 0.3,
                }],
            )
            .unwrap();
        let s = ed
            .model
            .add_slab(&ddc_model::SlabSpec {
                level: ed.level,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
                holes: vec![],
            })
            .unwrap();
        ed.reload().unwrap();
        begin(&mut ed, s).unwrap();
        {
            let e = ed.shape.as_mut().unwrap();
            e.tool = ShapeTool::Drain;
            e.dz = 0.06;
        }
        ed.handle(Event::Click(v2(5.0, 5.0)));
        let (v, _) = ed.shape.as_ref().unwrap().quantities().unwrap();
        assert!((v - 28.0).abs() < 1e-12);
        // constant thickness (Tab): volume equals that of a flat slab
        ed.handle(Event::Key(Key::Tab));
        assert!((ed.shape.as_ref().unwrap().quantities().unwrap().0 - 30.0).abs() < 1e-12);
        ed.handle(Event::Key(Key::Tab));
        // raise the corner by 0.03
        {
            let e = ed.shape.as_mut().unwrap();
            e.tool = ShapeTool::Modify;
            e.dz = 0.03;
        }
        ed.handle(Event::Click(v2(10.0, 10.0)));
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.shape.is_none());
        let (v, _) = ed.model.slab_shape_quantities(s).unwrap();
        // corner (10,10) raised: gain is 0.03 * (area of the two triangles at it = 50) / 3
        assert!((v - (28.0 + 0.03 * 50.0 / 3.0)).abs() < 1e-12, "{v}");
        ed.undo();
        let (v, _) = ed.model.slab_shape_quantities(s).unwrap();
        assert!((v - 30.0).abs() < 1e-12);
    }
}
