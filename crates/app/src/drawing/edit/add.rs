// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Draw-mode shapes beyond lines: text, circles and arcs added to the selected drawing on its
//! current layer. Each is one undo step ([`Step::geometry`]) and reaches the DXF export through
//! [`Drawings::export_into`] like [`Drawings::add_path`].

use std::f64::consts::TAU;

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{UArc, ULayer, UPoly, UText};

use super::super::history::Step;
use super::super::Drawings;
use super::{layer_pen, tr, SAME};

/// Radii above this (m) are a straight line in disguise: an arc through three almost collinear
/// points is refused.
const MAX_R: f64 = 1e7;

/// What the next clicks of the draw mode make.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    #[default]
    Line,
    Text,
    Circle,
    Arc,
}

/// Field buffers of the shape tools.
pub(super) struct AddUi {
    pub(super) shape: Shape,
    text: String,
    /// Typed circle radius (m); empty: the second click gives it.
    radius: String,
    /// Drawing scale N of 1:N, for the default text height.
    scale: f64,
    /// Result of the last canvas click, shown by the next panel frame.
    note: Option<String>,
}

impl Default for AddUi {
    fn default() -> Self {
        Self {
            shape: Shape::Line,
            text: String::new(),
            radius: String::new(),
            scale: 100.0,
            note: None,
        }
    }
}

impl AddUi {
    pub(super) fn is_line(&self) -> bool {
        self.shape == Shape::Line
    }

    /// The typed radius: `None` while the field is empty, NaN for text that is no number.
    fn typed_radius(&self) -> Option<f64> {
        let t = self.radius.trim();
        if t.is_empty() {
            return None;
        }
        Some(
            crate::units::parse_locale_number(t, crate::units::ui_decimal_mark())
                .unwrap_or(f64::NAN),
        )
    }
}

/// Angle of `p` seen from `c`.
fn ang(c: V2, p: V2) -> f64 {
    (p.y - c.y).atan2(p.x - c.x)
}

/// Angle folded into [0, TAU), the range imported arcs and trim use.
fn norm_ang(a: f64) -> f64 {
    let r = a.rem_euclid(TAU);
    // a tiny negative input rounds up to TAU itself
    if r >= TAU { 0.0 } else { r }
}

/// Center and radius of the circle through three points; `None` when they are collinear.
fn circumcircle(a: V2, b: V2, c: V2) -> Option<(V2, f64)> {
    let d = 2.0 * (a.x * (b.y - c.y) + b.x * (c.y - a.y) + c.x * (a.y - b.y));
    if !d.is_finite() || d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a.dot(a), b.dot(b), c.dot(c));
    let ux = (a2 * (b.y - c.y) + b2 * (c.y - a.y) + c2 * (a.y - b.y)) / d;
    let uy = (a2 * (c.x - b.x) + b2 * (a.x - c.x) + c2 * (b.x - a.x)) / d;
    let o = v2(ux, uy);
    let r = (a - o).len();
    (r.is_finite() && r > SAME && r <= MAX_R).then_some((o, r))
}

fn finite(p: V2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

impl Drawings {
    /// Start an edit on the selected drawing: the undo step is pushed and the current layer
    /// exists in the drawing. Returns (drawing index, layer, pen).
    fn begin_shape(&mut self) -> Result<(usize, String, u32), String> {
        let k = self.sel;
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.no_drawing"));
        };
        let layer = d.current_layer();
        self.steps.push(Step::geometry(d));
        if !d.u.layers.iter().any(|l| l.name == layer) {
            d.u.layers.push(ULayer {
                name: layer.clone(),
                color: None,
                off: false,
                frozen: false,
                linetype: String::new(),
                weight_mm: 0.0,
            });
        }
        let (pen, _) = layer_pen(&mut d.u, &layer);
        Ok((k, layer, pen))
    }

    fn end_shape(&mut self, k: usize) {
        if let Some(d) = self.list.get_mut(k) {
            d.sync_layers();
            d.mark_edited();
        }
        self.drop_recog(k);
        self.touch();
    }

    /// Add `text` at `p` (model metres) on the current layer. The height is that of the layer's
    /// last text, else 2.5 mm of paper at the drawing scale.
    pub fn add_text(&mut self, p: V2, text: &str) -> Result<String, String> {
        if !finite(p) {
            return Err(tr("dwg_add.st.point_bad"));
        }
        if text.trim().is_empty() {
            return Err(tr("dwg_edit.st.text_empty"));
        }
        if self.list.get(self.sel).is_none() {
            return Err(tr("dlg.dw.st.no_drawing"));
        }
        let scale = self.edit.add.scale;
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 100.0 };
        let (k, layer, pen) = self.begin_shape()?;
        let d = &mut self.list[k];
        let last = d
            .u
            .texts
            .iter()
            .rev()
            .find(|t| t.layer == layer && t.height.is_finite() && t.height > 0.0);
        let height = last.map_or(0.0025 * scale, |t| t.height);
        let pen = last.map_or(pen, |t| t.pen);
        d.u.texts.push(UText {
            layer: layer.clone(),
            pos: (p.x, p.y),
            height,
            rotation: 0.0,
            text: text.to_string(),
            pen,
            fmt: Default::default(),
        });
        self.end_shape(k);
        Ok(crate::tr("dwg_add.st.text_added", &[("layer", &layer)]))
    }

    /// Add a circle (center `c`, radius `r`, model metres) on the current layer.
    pub fn add_circle(&mut self, c: V2, r: f64) -> Result<String, String> {
        if !finite(c) || !r.is_finite() || r <= SAME {
            return Err(tr("dwg_add.st.radius_bad"));
        }
        if self.list.get(self.sel).is_none() {
            return Err(tr("dlg.dw.st.no_drawing"));
        }
        let (k, layer, pen) = self.begin_shape()?;
        let d = &mut self.list[k];
        d.u.arcs.push(UArc {
            layer: layer.clone(),
            c: (c.x, c.y),
            r,
            a0: 0.0,
            a1: TAU,
            pen,
        });
        d.u.closed.push(UPoly {
            layer: layer.clone(),
            pts: ddc_io::dxf::lower::circle_ring((c.x, c.y), r),
            circle: Some(r),
        });
        self.end_shape(k);
        Ok(crate::tr("dwg_add.st.circle_added", &[("layer", &layer)]))
    }

    /// Add the arc from `p0` through `p1` to `p2` (model metres) on the current layer. It turns
    /// counter-clockwise when the three points do, clockwise otherwise.
    pub fn add_arc(&mut self, p0: V2, p1: V2, p2: V2) -> Result<String, String> {
        if !(finite(p0) && finite(p1) && finite(p2)) {
            return Err(tr("dwg_add.st.point_bad"));
        }
        let Some((c, r)) = circumcircle(p0, p1, p2) else {
            return Err(tr("dwg_add.st.arc_bad"));
        };
        if self.list.get(self.sel).is_none() {
            return Err(tr("dlg.dw.st.no_drawing"));
        }
        let ccw = (p1 - p0).cross(p2 - p0) > 0.0;
        // arcs are stored counter-clockwise: a clockwise travel swaps the ends
        let (s, e) = if ccw { (p0, p2) } else { (p2, p0) };
        let a0 = norm_ang(ang(c, s));
        let a1 = a0 + (ang(c, e) - ang(c, s)).rem_euclid(TAU);
        let (k, layer, pen) = self.begin_shape()?;
        self.list[k].u.arcs.push(UArc {
            layer: layer.clone(),
            c: (c.x, c.y),
            r,
            a0,
            a1,
            pen,
        });
        self.end_shape(k);
        Ok(crate::tr("dwg_add.st.arc_added", &[("layer", &layer)]))
    }

    /// After a canvas click in a shape mode: make the shape once its points are there.
    pub(super) fn shape_clicked(&mut self) {
        let pts = self.edit.draw.clone().unwrap_or_default();
        let radius = self.edit.add.typed_radius();
        let shape = self.edit.add.shape;
        let r = match (shape, pts.as_slice()) {
            (Shape::Text, [p, ..]) => {
                let t = self.edit.add.text.clone();
                self.add_text(*p, &t)
            }
            (Shape::Circle, [c, ..]) if radius.is_some() => {
                self.add_circle(*c, radius.unwrap_or(f64::NAN))
            }
            (Shape::Circle, [c, e, ..]) => self.add_circle(*c, (*e - *c).len()),
            (Shape::Arc, [a, b, c, ..]) => self.add_arc(*a, *b, *c),
            _ => return,
        };
        let keep = match (&r, shape) {
            // a text without words keeps its point for the typed text
            (Err(_), Shape::Text) => pts.len(),
            (Err(_), _) => pts.len().saturating_sub(1),
            _ => 0,
        };
        if let Some(v) = self.edit.draw.as_mut() {
            v.truncate(keep);
        }
        self.edit.add.note = Some(r.unwrap_or_else(|e| e));
    }

    /// Shape picker and fields of the draw mode. Returns a status line.
    pub(super) fn add_ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        self.edit.draw.as_ref()?;
        let mut status = self.edit.add.note.take();
        let before = self.edit.add.shape;
        ui.horizontal(|ui| {
            for (s, key) in [
                (Shape::Line, "dwg_add.line"),
                (Shape::Text, "dwg_add.text"),
                (Shape::Circle, "dwg_add.circle"),
                (Shape::Arc, "dwg_add.arc"),
            ] {
                ui.selectable_value(&mut self.edit.add.shape, s, tr(key));
            }
        });
        if before != self.edit.add.shape {
            if let Some(v) = self.edit.draw.as_mut() {
                v.clear();
            }
        }
        let n = self.edit.draw.as_ref().map_or(0, |v| v.len());
        match self.edit.add.shape {
            Shape::Line => return status,
            Shape::Text => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.edit.add.text)
                            .desired_width(180.0)
                            .hint_text(tr("dwg_add.text_hint")),
                    );
                    ui.label(tr("dwg_add.scale"));
                    ui.add(
                        egui::DragValue::new(&mut self.edit.add.scale)
                            .range(1.0..=10000.0)
                            .prefix("1:"),
                    )
                    .on_hover_text(tr("dwg_add.scale_tip"));
                });
                ui.add_enabled_ui(n >= 1, |ui| {
                    if ui.button(tr("dwg_add.add_text")).clicked() {
                        self.shape_clicked();
                        status = self.edit.add.note.take();
                    }
                });
            }
            Shape::Circle => {
                ui.horizontal(|ui| {
                    ui.label(tr("dwg_add.radius"));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.edit.add.radius)
                            .desired_width(80.0)
                            .hint_text(tr("dwg_add.radius_hint")),
                    )
                    .on_hover_text(tr("dwg_add.radius_tip"));
                });
                ui.add_enabled_ui(n >= 1 && self.edit.add.typed_radius().is_some(), |ui| {
                    if ui.button(tr("dwg_add.add_circle")).clicked() {
                        self.shape_clicked();
                        status = self.edit.add.note.take();
                    }
                });
            }
            Shape::Arc => {
                ui.label(crate::tr("dwg_add.arc_points", &[("n", &n.min(3).to_string())]));
            }
        }
        if ui.button(tr("dwg_edit.cancel")).clicked() {
            if let Some(v) = self.edit.draw.as_mut() {
                v.clear();
            }
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::Origin;
    use super::*;
    use crate::editor::Editor;
    use ddc_io::drawing_model as dm;
    use ddc_io::dxf::{DxfDocument, Entity};

    fn setup() -> (Editor, Drawings) {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        let doc = dm::fixture::sample_plan(0);
        ds.add(&doc, "p", None, ed.level, None, Origin::AsIs, v2(0.0, 0.0));
        assert!((ds.list[0].unit - 0.001).abs() < 1e-12, "the fixture is in millimetres");
        (ed, ds)
    }

    fn export(ds: &Drawings) -> DxfDocument {
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        doc
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn circles(doc: &DxfDocument, layer: &str) -> Vec<(f64, f64, f64)> {
        doc.entities
            .iter()
            .filter_map(|e| match e {
                Entity::Circle(c) if c.common.layer == layer => {
                    Some((c.center[0], c.center[1], c.radius))
                }
                _ => None,
            })
            .collect()
    }

    fn arcs(doc: &DxfDocument, layer: &str) -> Vec<(f64, f64, f64, f64, f64)> {
        doc.entities
            .iter()
            .filter_map(|e| match e {
                Entity::Arc(a) if a.common.layer == layer => Some((
                    a.center[0],
                    a.center[1],
                    a.radius,
                    a.start_angle,
                    a.end_angle,
                )),
                _ => None,
            })
            .collect()
    }

    // the layer box offers only layers the drawing has, so a test layer is made first
    fn draw_on_new_layer(ds: &mut Drawings, name: &str) {
        ds.list[0].u.layers.push(ULayer {
            name: name.into(),
            color: None,
            off: false,
            frozen: false,
            linetype: String::new(),
            weight_mm: 0.0,
        });
        ds.list[0].cur_layer = name.into();
    }

    #[test]
    fn add_text_on_current_layer_exported() {
        let (mut ed, mut ds) = setup();
        ds.list[0].cur_layer = "A-WALL".into();
        let n0 = ed.undo_len();
        let h_layer = ds.list[0].u.texts.iter().rev().find(|t| t.layer == "A-WALL").map(|t| t.height);
        ds.add_text(v2(1.0, 2.0), "Note").unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1);
        let t = ds.list[0].u.texts.last().unwrap().clone();
        assert_eq!((t.text.as_str(), t.layer.as_str(), t.pos), ("Note", "A-WALL", (1.0, 2.0)));
        match h_layer {
            Some(h) => assert!(near(t.height, h), "the layer's last text height"),
            None => assert!(near(t.height, 0.25), "2.5 mm at 1:100"),
        }
        let doc = export(&ds);
        let got = doc.entities.iter().find_map(|e| match e {
            Entity::Text(x) if x.text == "Note" => Some(x.clone()),
            _ => None,
        });
        let x = got.expect("the text is exported");
        assert_eq!(x.common.layer, "A-WALL");
        assert!(near(x.pos[0], 1000.0) && near(x.pos[1], 2000.0), "file millimetres: {:?}", x.pos);
        // a layer without texts takes the scale
        ds.list[0].cur_layer = "NEW-T".into();
        ds.edit.add.scale = 50.0;
        ds.add_text(v2(0.0, 0.0), "S").unwrap();
        let t = ds.list[0].u.texts.last().unwrap();
        assert!(near(t.height, 0.125), "2.5 mm at 1:50: {}", t.height);
        assert!(ds.add_text(v2(0.0, 0.0), "  ").is_err(), "empty text refused");
    }

    #[test]
    fn add_circle_typed_radius() {
        let (mut ed, mut ds) = setup();
        draw_on_new_layer(&mut ds, "A-COL");
        ds.edit.draw = Some(vec![]);
        ds.edit.add.shape = Shape::Circle;
        ds.edit.add.radius = "0,3".into();
        let level = ed.level;
        let n0 = ed.undo_len();
        assert!(ds.draw_click(level, v2(10.0, 20.0), 0.0));
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1, "one click with a typed radius is one step");
        let a = ds.list[0].u.arcs.last().unwrap().clone();
        assert_eq!((a.layer.as_str(), a.c, a.r), ("A-COL", (10.0, 20.0), 0.3));
        assert!(ds.list[0].u.closed.iter().any(|c| c.circle == Some(0.3)));
        let c = circles(&export(&ds), "A-COL");
        assert_eq!(c.len(), 1, "a circle, not an arc and not a polyline");
        assert!(near(c[0].0, 10000.0) && near(c[0].1, 20000.0) && near(c[0].2, 300.0), "{c:?}");
        // no typed radius: the second click gives it
        ds.edit.add.radius.clear();
        let n_arcs = ds.list[0].u.arcs.len();
        assert!(ds.draw_click(level, v2(50.123, 60.456), 0.0));
        assert_eq!(ds.list[0].u.arcs.len(), n_arcs, "the center click adds nothing yet");
        assert!(ds.draw_click(level, v2(50.123, 62.456), 0.0));
        let a = ds.list[0].u.arcs.last().unwrap();
        assert!(near(a.r, 2.0) && a.c == (50.123, 60.456));
        assert_eq!(ds.list[0].u.arcs.len(), n_arcs + 1);
    }

    #[test]
    fn add_arc_three_points_ccw_and_cw() {
        let (mut ed, mut ds) = setup();
        draw_on_new_layer(&mut ds, "A-ARC");
        let n0 = ed.undo_len();
        // counter-clockwise: (1,0) -> (0,1) -> (-1,0) around the origin
        ds.add_arc(v2(1.0, 0.0), v2(0.0, 1.0), v2(-1.0, 0.0)).unwrap();
        // clockwise: the same points the other way round
        ds.add_arc(v2(-1.0, 0.0), v2(0.0, 1.0), v2(1.0, 0.0)).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 2);
        let u = &ds.list[0].u;
        let (a, b) = (&u.arcs[u.arcs.len() - 2], &u.arcs[u.arcs.len() - 1]);
        for x in [a, b] {
            assert!(near(x.r, 1.0) && near(x.c.0, 0.0) && near(x.c.1, 0.0));
            assert!(x.a1 > x.a0 && x.a1 - x.a0 < std::f64::consts::TAU);
        }
        assert!(near(a.a0, 0.0) && near(a.a1, std::f64::consts::PI), "ccw keeps the ends");
        assert!(
            near(b.a0, 0.0) && near(b.a1, std::f64::consts::PI),
            "cw swaps the ends: still the upper half"
        );
        // the other side: (1,0) -> (0,-1) -> (-1,0) is clockwise, the lower half
        ds.add_arc(v2(1.0, 0.0), v2(0.0, -1.0), v2(-1.0, 0.0)).unwrap();
        let c = ds.list[0].u.arcs.last().unwrap();
        assert!(near(c.a0, std::f64::consts::PI) && near(c.a1, std::f64::consts::TAU));
        let got = arcs(&export(&ds), "A-ARC");
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|g| near(g.2, 1000.0)));
        assert!(near(got[2].3, 180.0) && near(got[2].4, 0.0), "{got:?}");
    }

    #[test]
    fn arc_a0_normalised_ccw_and_cw() {
        let (_ed, mut ds) = setup();
        ds.add_arc(v2(1.0, 0.0), v2(0.0, 1.0), v2(-1.0, 0.0)).unwrap();
        ds.add_arc(v2(-1.0, 0.0), v2(0.0, 1.0), v2(1.0, 0.0)).unwrap();
        ds.add_arc(v2(0.0, -1.0), v2(1.0, 0.0), v2(0.0, 1.0)).unwrap();
        for x in &ds.list[0].u.arcs[ds.list[0].u.arcs.len() - 3..] {
            assert!(x.a0 >= 0.0 && x.a0 < std::f64::consts::TAU, "a0 {}", x.a0);
        }
    }

    #[test]
    fn arc_below_x_axis_a0_positive() {
        let (_ed, mut ds) = setup();
        // ccw from (0,-1) through (1,0) to (0,1): the start sits below the x axis
        ds.add_arc(v2(0.0, -1.0), v2(1.0, 0.0), v2(0.0, 1.0)).unwrap();
        let a = ds.list[0].u.arcs.last().unwrap();
        assert!(near(a.a0, 3.0 * std::f64::consts::FRAC_PI_2), "a0 {}", a.a0);
        assert!(near(a.a1 - a.a0, std::f64::consts::PI));
    }

    #[test]
    fn sweep_unchanged_after_normalise() {
        let (_ed, mut ds) = setup();
        // quarter ccw starting below the axis, then the same ends clockwise (three quarters)
        let h = std::f64::consts::FRAC_1_SQRT_2;
        ds.add_arc(v2(0.0, -1.0), v2(h, -h), v2(1.0, 0.0))
            .unwrap();
        let a = ds.list[0].u.arcs.last().unwrap();
        assert!(near(a.a1 - a.a0, std::f64::consts::FRAC_PI_2), "sweep {}", a.a1 - a.a0);
        ds.add_arc(v2(0.0, -1.0), v2(-1.0, 0.0), v2(1.0, 0.0)).unwrap();
        let b = ds.list[0].u.arcs.last().unwrap();
        assert!(near(b.a1 - b.a0, 3.0 * std::f64::consts::FRAC_PI_2), "sweep {}", b.a1 - b.a0);
        assert!(b.a0 >= 0.0 && b.a0 < std::f64::consts::TAU);
    }

    #[test]
    fn add_refuses_zero_radius() {
        let (mut ed, mut ds) = setup();
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        assert!(ds.add_circle(v2(0.0, 0.0), 0.0).is_err());
        assert!(ds.add_circle(v2(0.0, 0.0), -1.0).is_err());
        assert!(ds.add_circle(v2(0.0, 0.0), f64::NAN).is_err());
        assert!(ds.add_circle(v2(f64::NAN, 0.0), 1.0).is_err());
        // three points on one line, and a repeated point
        assert!(ds.add_arc(v2(0.0, 0.0), v2(1.0, 0.0), v2(2.0, 0.0)).is_err());
        assert!(ds.add_arc(v2(0.0, 0.0), v2(0.0, 0.0), v2(2.0, 0.0)).is_err());
        // a typed radius of zero or no number
        let level = ed.level;
        ds.edit.draw = Some(vec![]);
        ds.edit.add.shape = Shape::Circle;
        for t in ["0", "abc"] {
            ds.edit.add.radius = t.into();
            assert!(ds.draw_click(level, v2(5.0, 5.0), 0.0));
            assert_eq!(ds.edit.draw.as_ref().map(|v| v.len()), Some(0), "the bad click is dropped");
        }
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0, "no step for a refused shape");
        assert_eq!(ds.list[0].u, before, "the drawing is untouched");
        assert!(!ds.list[0].dirty);
    }

    #[test]
    fn shape_strings_are_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let cyrillic = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let c = Catalog::new(Lang::En);
        let keys = [
            "line", "text", "circle", "arc", "text_hint", "scale", "scale_tip", "add_text",
            "radius", "radius_hint", "radius_tip", "add_circle", "arc_points", "st.point_bad",
            "st.radius_bad", "st.arc_bad", "st.text_added", "st.circle_added", "st.arc_added",
        ];
        for k in keys {
            let k = format!("dwg_add.{k}");
            let (en, ru) = (c.t_in(Lang::En, &k), c.t_in(Lang::Ru, &k));
            assert_ne!(en, k, "missing in en");
            assert!(!cyrillic(&en), "{k}: {en}");
            assert!(cyrillic(&ru), "{k}: missing in ru: {ru}");
        }
    }

    #[test]
    fn add_one_undo_each() {
        let (mut ed, mut ds) = setup();
        let before = ds.list[0].u.clone();
        let n0 = ed.undo_len();
        ds.add_text(v2(1.0, 1.0), "T").unwrap();
        ds.add_circle(v2(2.0, 2.0), 0.5).unwrap();
        ds.add_arc(v2(1.0, 0.0), v2(0.0, 1.0), v2(-1.0, 0.0)).unwrap();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 3);
        let full = ds.list[0].u.clone();
        for k in (0..3).rev() {
            ed.undo();
            ds.sync_undo(&mut ed);
            let u = &ds.list[0].u;
            assert_eq!(u.texts.len(), before.texts.len() + usize::from(k > 0));
            assert_eq!(u.arcs.len(), before.arcs.len() + usize::from(k == 2));
        }
        assert_eq!(ds.list[0].u, before, "three undos restore the drawing");
        for _ in 0..3 {
            ed.redo();
            ds.sync_undo(&mut ed);
        }
        assert_eq!(ds.list[0].u, full, "three redos bring it all back");
    }
}
