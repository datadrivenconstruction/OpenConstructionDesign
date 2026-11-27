// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Edit tools for opened drawings: change the text of a picked text, move picked primitives to an
//! existing or new layer, draw lines and polylines into a drawing with snaps. Each edit is one
//! undo step ([`Step::geometry`]), marks the drawing edited, and reaches the DXF export through
//! [`Drawings::export_into`].

use std::collections::HashMap;

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{Pen, Pick, ULayer, ULine, UPoly, Underlay};
use egui::{self, Color32, Painter, Pos2, Stroke};

use super::history::Step;
use super::{Drawing, Drawings, LayerState};

mod grips;
mod add;
mod modify;
mod snap;

/// Characters a DXF layer name cannot hold.
const BAD_CHARS: &str = "<>/\\\":;?*|=,`";
/// Points closer than this (m) are one point.
const SAME: f64 = 1e-9;

fn tr(key: &str) -> String {
    crate::tr(key, &[])
}

/// Layer dropdown of the kit. `head` is an inert first row (a prompt, or the current name when
/// it is not in `names`); returns the picked layer name only when the user chose one.
pub(crate) fn layer_dropdown(
    ui: &mut egui::Ui,
    salt: &str,
    names: &[String],
    cur: &str,
    head: Option<&str>,
) -> Option<String> {
    let th = ddc_ui::theme::of(ui.ctx());
    let skip = usize::from(head.is_some());
    let mut items: Vec<&str> = head.into_iter().collect();
    items.extend(names.iter().map(String::as_str));
    let mut ix = names.iter().position(|n| n == cur).map_or(0, |p| p + skip);
    let r = ddc_ui::widgets::dropdown(ui, &th, salt, &mut ix, &items, None);
    if !r.changed() {
        return None;
    }
    ix.checked_sub(skip).and_then(|p| names.get(p)).cloned()
}

/// Field buffers and the draw mode of the edit tools.
#[derive(Default)]
pub struct EditUi {
    /// Draw mode: points clicked so far (model metres, snapped).
    pub draw: Option<Vec<V2>>,
    text: String,
    /// (drawing id, text index) the text field was loaded from.
    text_of: Option<(u64, usize)>,
    layer: String,
    /// Offset distance, side click and join tolerance.
    modify: modify::ModifyUi,
    /// Grip drag, typed length and the vertex selected for Delete.
    grip: grips::GripUi,
    /// Trim and extend boundary and mode.
    trim: modify::trim::TrimUi,
    /// Text, circle and arc shapes of the draw mode.
    add: add::AddUi,
    /// Mirror and scale tool fields.
    pub xform: super::xform::XformUi,
    /// Fillet tool: radius and the first picked line.
    fillet: modify::fillet::FilletUi,
}

impl Drawing {
    /// Layer the drawing tools draw on: the chosen one while the drawing has it, else "0",
    /// else the first layer of the file.
    pub fn current_layer(&self) -> String {
        let has = |n: &str| self.u.layers.iter().any(|l| l.name == n);
        if !self.cur_layer.is_empty() && has(&self.cur_layer) {
            return self.cur_layer.clone();
        }
        match self.u.layers.first() {
            Some(l) if !has("0") => l.name.clone(),
            _ => "0".into(),
        }
    }

    /// An edit was made: the drawing takes a generation no earlier state has had.
    pub(crate) fn mark_edited(&mut self) {
        self.next_gen += 1;
        self.gen = self.next_gen;
        self.dirty = self.gen != self.saved_gen;
    }

    /// Name for lists, with a mark once the drawing was edited.
    pub fn title(&self) -> String {
        if self.dirty {
            crate::tr("dwg_edit.edited", &[("name", &self.name)])
        } else {
            self.name.clone()
        }
    }

    /// Layer list and counts after the primitives or the layer table changed: user settings of
    /// kept layers stay, layers new in the file are added, emptied layers gone from it dropped.
    pub(crate) fn sync_layers(&mut self) {
        let u = &self.u;
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for l in u
            .lines
            .iter()
            .map(|x| &x.layer)
            .chain(u.arcs.iter().map(|x| &x.layer))
            .chain(u.texts.iter().map(|x| &x.layer))
            .chain(u.fills.iter().map(|x| &x.layer))
        {
            *counts.entry(l.as_str()).or_default() += 1;
        }
        let in_file = |n: &str| u.layers.iter().any(|l| l.name == n);
        // dropped layers keep their display state for the time they come back (redo)
        let mut gone = vec![];
        self.layers.retain(|l| {
            let keep = in_file(&l.name) || counts.contains_key(l.name.as_str());
            if !keep {
                gone.push(l.clone());
            }
            keep
        });
        for l in gone {
            self.parked.insert(l.name.clone(), l);
        }
        for l in &mut self.layers {
            l.count = counts.get(l.name.as_str()).copied().unwrap_or(0);
        }
        for fl in &u.layers {
            let n = counts.get(fl.name.as_str()).copied().unwrap_or(0);
            if self.layers.iter().any(|l| l.name == fl.name) || (n == 0 && fl.name == "0") {
                continue;
            }
            let mut st = LayerState {
                name: fl.name.clone(),
                visible: !fl.off,
                frozen: fl.frozen,
                file_color: fl.color,
                color: None,
                halftone: false,
                count: n,
            };
            if let Some(p) = self.parked.remove(&fl.name) {
                st.visible = p.visible;
                st.frozen = p.frozen;
                st.color = p.color;
                st.halftone = p.halftone;
            }
            self.layers.push(st);
        }
        self.idx = self
            .layers
            .iter()
            .enumerate()
            .map(|(k, l)| (l.name.clone(), k))
            .collect();
    }
}

/// Layers of the picked primitives, in pick order.
fn pick_layers<'a>(u: &'a Underlay, pk: &Pick) -> Vec<&'a str> {
    let mut v: Vec<&str> = vec![];
    v.extend(pk.lines.iter().filter_map(|&i| u.lines.get(i)).map(|x| x.layer.as_str()));
    v.extend(pk.arcs.iter().filter_map(|&i| u.arcs.get(i)).map(|x| x.layer.as_str()));
    v.extend(pk.texts.iter().filter_map(|&i| u.texts.get(i)).map(|x| x.layer.as_str()));
    v.extend(pk.closed.iter().filter_map(|&i| u.closed.get(i)).map(|x| x.layer.as_str()));
    v.extend(pk.fills.iter().filter_map(|&i| u.fills.get(i)).map(|x| x.layer.as_str()));
    v
}

fn set_layer(u: &mut Underlay, pk: &Pick, name: &str) {
    for &i in &pk.lines {
        if let Some(x) = u.lines.get_mut(i) {
            x.layer = name.to_string();
        }
    }
    for &i in &pk.arcs {
        if let Some(x) = u.arcs.get_mut(i) {
            x.layer = name.to_string();
        }
    }
    for &i in &pk.texts {
        if let Some(x) = u.texts.get_mut(i) {
            x.layer = name.to_string();
        }
    }
    for &i in &pk.closed {
        if let Some(x) = u.closed.get_mut(i) {
            x.layer = name.to_string();
        }
    }
    for &i in &pk.fills {
        if let Some(x) = u.fills.get_mut(i) {
            x.layer = name.to_string();
        }
    }
}

/// Pen and dash flag for a new line on `layer`: the most common among the layer's lines, else a
/// pen of the layer's own color and weight.
fn layer_pen(u: &mut Underlay, layer: &str) -> (u32, bool) {
    let mut n: HashMap<(u32, bool), usize> = HashMap::new();
    for l in u.lines.iter().filter(|l| l.layer == layer) {
        *n.entry((l.pen, l.dashed)).or_default() += 1;
    }
    if let Some((k, _)) = n.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))) {
        return k;
    }
    let def = u.layers.iter().find(|l| l.name == layer);
    let pen = Pen {
        color: def.and_then(|l| l.color),
        weight_mm: def.map_or(0.0, |l| l.weight_mm),
        dash: vec![],
    };
    // pen 0 is the default pen of the file's own primitives: never take its index
    if u.pens.is_empty() {
        u.pens.push(Pen::default());
    }
    if let Some(k) = u.pens.iter().skip(1).position(|p| *p == pen) {
        return (k as u32 + 1, false);
    }
    u.pens.push(pen);
    ((u.pens.len() - 1) as u32, false)
}

/// Foot of the perpendicular from `p` on segment `ab`.
fn foot(p: V2, a: V2, b: V2) -> V2 {
    let d = b - a;
    let l2 = d.dot(d);
    if l2 < 1e-18 {
        return a;
    }
    a + d * ((p - a).dot(d) / l2).clamp(0.0, 1.0)
}

impl Drawings {
    /// (drawing, text index) when the pick holds exactly one text.
    fn picked_text_ix(&self) -> Option<(usize, usize)> {
        let (k, pk) = self.pick.as_ref()?;
        (pk.texts.len() == 1).then(|| (*k, pk.texts[0]))
    }

    /// Text of the one picked text.
    pub fn picked_text(&self) -> Option<&str> {
        let (k, i) = self.picked_text_ix()?;
        self.list.get(k)?.u.texts.get(i).map(|t| t.text.as_str())
    }

    fn drop_recog(&mut self, k: usize) {
        if self.recog.as_ref().is_some_and(|r| r.drawing == k) {
            self.recog = None;
        }
    }

    /// Write `text` into the one picked text. Empty or unchanged text makes no step.
    pub fn edit_text(&mut self, text: &str) -> Result<String, String> {
        let Some((k, i)) = self.picked_text_ix() else {
            return Err(tr("dwg_edit.st.text_one"));
        };
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let Some(old) = d.u.texts.get(i).map(|t| t.text.clone()) else {
            return Err(tr("dwg_edit.st.text_one"));
        };
        if text.trim().is_empty() {
            return Err(tr("dwg_edit.st.text_empty"));
        }
        if text == old {
            return Err(tr("dwg_edit.st.text_same"));
        }
        self.steps.push(Step::geometry(d));
        d.u.texts[i].text = text.to_string();
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(crate::tr("dwg_edit.st.text_set", &[("text", text)]))
    }

    /// Move the picked primitives to layer `name` (any case of an existing name). A new layer
    /// takes the color, weight and line type of the first picked primitive's layer.
    pub fn move_pick_to_layer(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(tr("dwg_edit.st.layer_empty"));
        }
        if name.chars().any(|c| c.is_control() || BAD_CHARS.contains(c)) {
            return Err(crate::tr("dwg_edit.st.layer_bad", &[("chars", BAD_CHARS)]));
        }
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let layers = pick_layers(&d.u, &pk);
        let Some(src) = layers.first().map(|s| s.to_string()) else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let key = name.to_uppercase();
        let existing = d
            .u
            .layers
            .iter()
            .find(|l| l.name.to_uppercase() == key)
            .map(|l| l.name.clone());
        let target = existing.clone().unwrap_or_else(|| name.to_string());
        if layers.iter().all(|l| *l == target) {
            return Err(crate::tr("dwg_edit.st.layer_same", &[("layer", &target)]));
        }
        let n = layers.len();
        self.steps.push(Step::geometry(d));
        if existing.is_none() {
            let from = d.u.layers.iter().find(|l| l.name == src);
            let layer = ULayer {
                name: target.clone(),
                color: from.and_then(|l| l.color),
                off: false,
                frozen: false,
                linetype: from.map(|l| l.linetype.clone()).unwrap_or_default(),
                weight_mm: from.map_or(0.0, |l| l.weight_mm),
            };
            d.u.layers.push(layer);
        }
        set_layer(&mut d.u, &pk, &target);
        let shown = d.layer(&src).map(|l| (l.color, l.halftone));
        d.sync_layers();
        if existing.is_none() {
            // the display override of the source layer goes along with the copy
            if let (Some((color, half)), Some(&j)) = (shown, d.idx.get(&target)) {
                d.layers[j].color = color;
                d.layers[j].halftone = half;
            }
        }
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        let key = if existing.is_some() {
            "dwg_edit.st.layer_moved"
        } else {
            "dwg_edit.st.layer_created"
        };
        Ok(crate::tr(
            key,
            &[("n", &n.to_string()), ("layer", &target)],
        ))
    }

    /// Add a line or polyline (`closed`: back to the first point) to the selected drawing on its
    /// current layer. Points are model metres.
    pub fn add_path(&mut self, pts: &[V2], closed: bool) -> Result<String, String> {
        if pts.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        let mut v: Vec<V2> = vec![];
        for p in pts {
            if v.last().is_none_or(|q| (*p - *q).len() > SAME) {
                v.push(*p);
            }
        }
        if closed && v.len() > 1 && (v[0] - v[v.len() - 1]).len() <= SAME {
            v.pop();
        }
        if v.len() < 2 {
            return Err(tr("dwg_edit.st.path_short"));
        }
        if closed && v.len() < 3 {
            return Err(tr("dwg_edit.st.path_closed_short"));
        }
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
        let (pen, dashed) = layer_pen(&mut d.u, &layer);
        let n = if closed { v.len() } else { v.len() - 1 };
        for j in 0..n {
            let (a, b) = (v[j], v[(j + 1) % v.len()]);
            d.u.lines.push(ULine {
                layer: layer.clone(),
                a: (a.x, a.y),
                b: (b.x, b.y),
                dashed,
                pen,
            });
        }
        if closed {
            d.u.closed.push(UPoly {
                layer: layer.clone(),
                pts: v.iter().map(|p| (p.x, p.y)).collect(),
                circle: None,
            });
        }
        d.sync_layers();
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(crate::tr(
            "dwg_edit.st.path_added",
            &[("n", &n.to_string()), ("layer", &layer)],
        ))
    }

    /// The selected drawing, when it is shown on `level`.
    fn draw_target(&self, level: i64) -> bool {
        self.list
            .get(self.sel)
            .is_some_and(|d| d.level == level && d.visible)
    }

    /// Click in draw mode: add a snapped point. `true` when the click was taken.
    pub fn draw_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        self.edit.modify.clicked_with(tol);
        if self.offset_click(level, p, tol)
            || self.fillet_click(level, p, tol)
            || self.trim_click(level, p, tol)
        {
            return true;
        }
        if self.edit.draw.is_none() || !self.draw_target(level) {
            return false;
        }
        let (q, _) = self.snap_point(level, p, tol);
        if let Some(v) = self.edit.draw.as_mut() {
            v.push(q);
        }
        self.shape_clicked();
        true
    }

    /// Write the clicked points as one line or polyline; the draw mode stays on for the next.
    /// Refused points stay for the user to continue.
    pub fn draw_finish(&mut self, closed: bool) -> Result<String, String> {
        let pts = self.edit.draw.as_mut().map(std::mem::take).unwrap_or_default();
        let r = self.add_path(&pts, closed);
        if r.is_err() {
            if let Some(v) = self.edit.draw.as_mut() {
                *v = pts;
            }
        }
        r
    }

    /// The line being drawn, with a rubber band to the snapped cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_draw(
        &self,
        p: &Painter,
        to_s: &dyn Fn(V2) -> Pos2,
        level: i64,
        accent: Color32,
        hover: Option<V2>,
        tol: f64,
        ov: &crate::overlay::Palette,
    ) {
        if self.edit.draw.is_none() && self.edit.modify.through_armed() {
            if let Some((h, kind)) = hover.map(|h| self.snap_point(level, h, tol)) {
                let g = crate::plan_view::snap_glyph(kind);
                crate::overlay::snap_glyph(p, to_s(h), g, ov);
            }
            return;
        }
        let Some(pts) = &self.edit.draw else { return };
        if !self.draw_target(level) {
            return;
        }
        let st = Stroke::new(2.0, accent);
        let mut s: Vec<Pos2> = pts.iter().map(|q| to_s(*q)).collect();
        if let Some((h, kind)) = hover.map(|h| self.snap_point(level, h, tol)) {
            s.push(to_s(h));
            let g = crate::plan_view::snap_glyph(kind);
            crate::overlay::snap_glyph(p, to_s(h), g, ov);
        }
        if s.len() >= 2 {
            p.add(egui::Shape::line(s, st));
        }
        for q in pts {
            p.circle_filled(to_s(*q), 2.5, accent);
        }
    }

    /// Edit tools in the layers window. Returns a status line.
    pub fn edit_ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut status = None;
        let cur = self.picked_text_ix().and_then(|(k, i)| {
            let id = self.list.get(k)?.id;
            Some(((id, i), self.picked_text()?.to_string()))
        });
        match cur {
            Some((key, text)) => {
                if self.edit.text_of != Some(key) {
                    self.edit.text = text;
                    self.edit.text_of = Some(key);
                }
                ui.horizontal(|ui| {
                    ui.label(tr("dwg_edit.text"));
                    let r = ui
                        .add(egui::TextEdit::singleline(&mut self.edit.text).desired_width(220.0))
                        .on_hover_text(tr("dwg_edit.text_tip"));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let t = self.edit.text.clone();
                        status = Some(self.edit_text(&t).unwrap_or_else(|e| e));
                    }
                });
            }
            None => self.edit.text_of = None,
        }
        let picked = self.pick.as_ref().filter(|p| !p.1.is_empty()).map(|p| p.0);
        if let Some(k) = picked {
            let names: Vec<String> = self
                .list
                .get(k)
                .map(|d| d.u.layers.iter().map(|l| l.name.clone()).collect())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(tr("dwg_edit.layer"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.edit.layer)
                        .desired_width(160.0)
                        .hint_text(tr("dwg_edit.layer_hint")),
                );
                let pick = tr("dwg_edit.layer_pick");
                let cur = self.edit.layer.clone();
                if let Some(n) = layer_dropdown(ui, "dwg8_layer", &names, &cur, Some(&pick)) {
                    self.edit.layer = n;
                }
                let th = ddc_ui::theme::of(ui.ctx());
                if ddc_ui::widgets::Button::secondary(&tr("dwg_edit.to_layer"))
                    .show(ui, &th)
                    .on_hover_text(tr("dwg_edit.to_layer_tip"))
                    .clicked()
                {
                    let n = self.edit.layer.clone();
                    status = Some(self.move_pick_to_layer(&n).unwrap_or_else(|e| e));
                }
            });
        }
        ui.separator();
        if let Some(d) = self.list.get_mut(self.sel) {
            let cur = d.current_layer();
            let names: Vec<String> = d.u.layers.iter().map(|l| l.name.clone()).collect();
            ui.horizontal(|ui| {
                ui.label(tr("dwg_edit.cur_layer"));
                let head = (!names.contains(&cur)).then_some(cur.as_str());
                if let Some(n) = layer_dropdown(ui, "dwg8_cur", &names, &cur, head) {
                    d.cur_layer = n;
                }
            });
        }
        let mut on = self.edit.draw.is_some();
        if ui
            .checkbox(&mut on, tr("dwg_edit.draw"))
            .on_hover_text(tr("dwg_edit.draw_tip"))
            .changed()
        {
            self.edit.draw = on.then(Vec::new);
            if on {
                self.pick_mode = false;
                self.pick = None;
                self.rearm_tool();
            }
        }
        if let Some(s) = self.modify_ui(ui) {
            status = Some(s);
        }
        if let Some(s) = self.trim_ui(ui) {
            status = Some(s);
        }
        if let Some(s) = self.add_ui(ui) {
            status = Some(s);
        }
        if let Some(s) = self.fillet_ui(ui) {
            status = Some(s);
        }
        let line = self.edit.add.is_line();
        if let Some(n) = self.edit.draw.as_ref().map(|v| v.len()).filter(|_| line) {
            let th = ddc_ui::theme::of(ui.ctx());
            ui.horizontal(|ui| {
                ui.label(crate::tr("dwg_edit.draw_points", &[("n", &n.to_string())]));
                let fin = ui
                    .add_enabled_ui(n >= 2, |ui| ddc_ui::widgets::Button::primary(&tr("dwg_edit.finish")).show(ui, &th))
                    .inner
                    .on_disabled_hover_text(tr("dwg_edit.finish_off"));
                if fin.clicked() {
                    status = Some(self.draw_finish(false).unwrap_or_else(|e| e));
                }
                let close = ui
                    .add_enabled_ui(n >= 3, |ui| ddc_ui::widgets::Button::secondary(&tr("dwg_edit.close")).show(ui, &th))
                    .inner
                    .on_disabled_hover_text(tr("dwg_edit.close_off"));
                if close.clicked() {
                    status = Some(self.draw_finish(true).unwrap_or_else(|e| e));
                }
                if ddc_ui::widgets::Button::quiet(&tr("dwg_edit.cancel")).show(ui, &th).clicked() {
                    if let Some(v) = self.edit.draw.as_mut() {
                        v.clear();
                    }
                }
            });
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::super::Origin;
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

    fn sync(ed: &mut Editor, ds: &mut Drawings) {
        ds.sync_undo(ed);
    }

    fn undo(ed: &mut Editor, ds: &mut Drawings) {
        ed.undo();
        ds.sync_undo(ed);
    }

    fn redo(ed: &mut Editor, ds: &mut Drawings) {
        ed.redo();
        ds.sync_undo(ed);
    }

    fn pick_text(ds: &mut Drawings, s: &str) -> usize {
        let i = ds.list[0].u.texts.iter().position(|t| t.text == s).unwrap();
        ds.pick = Some((
            0,
            Pick {
                texts: vec![i],
                ..Default::default()
            },
        ));
        i
    }

    fn wall_line(ds: &Drawings) -> usize {
        ds.list[0]
            .u
            .lines
            .iter()
            .position(|l| l.layer == "A-WALL")
            .unwrap()
    }

    fn export(ds: &Drawings) -> DxfDocument {
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        doc
    }

    fn texts(doc: &DxfDocument) -> Vec<(String, String)> {
        doc.entities
            .iter()
            .filter_map(|e| match e {
                Entity::Text(t) => Some((t.text.clone(), t.common.layer.clone())),
                _ => None,
            })
            .collect()
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    /// A LINE on `layer` between file points `a` and `b`, either direction.
    fn has_line(doc: &DxfDocument, layer: &str, a: (f64, f64), b: (f64, f64)) -> bool {
        doc.entities.iter().any(|e| match e {
            Entity::Line(x) if x.common.layer == layer => {
                let (s, t) = ((x.start[0], x.start[1]), (x.end[0], x.end[1]));
                (near(s, a) && near(t, b)) || (near(s, b) && near(t, a))
            }
            _ => false,
        })
    }

    #[test]
    fn picked_text_edit_one_undo() {
        let (mut ed, mut ds) = setup();
        let i = pick_text(&mut ds, "101");
        assert_eq!(ds.picked_text(), Some("101"));
        let n0 = ed.undo_len();
        let v0 = ds.version;
        // refused with a reason: empty, blank, unchanged; no step, nothing marked
        for bad in ["", "   ", "101"] {
            let e = ds.edit_text(bad).unwrap_err();
            assert!(!e.is_empty());
        }
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.version, v0);
        assert!(!ds.list[0].dirty);
        assert_eq!(ds.list[0].title(), "p");
        ds.edit_text("Kitchen 101").unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one edit, one step");
        assert_eq!(ds.list[0].u.texts[i].text, "Kitchen 101");
        assert!(ds.list[0].dirty);
        assert_ne!(ds.list[0].title(), "p", "the edited drawing is marked");
        undo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.texts[i].text, "101");
        assert_eq!(ed.undo_len(), n0);
        redo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.texts[i].text, "Kitchen 101");
        // two texts or none picked: refused
        ds.pick = Some((
            0,
            Pick {
                texts: vec![0, 1],
                ..Default::default()
            },
        ));
        assert!(ds.picked_text().is_none());
        assert!(ds.edit_text("x").is_err());
        ds.pick = None;
        assert!(ds.edit_text("x").is_err());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
    }

    #[test]
    fn text_edit_reaches_dxf_export() {
        let (_, mut ds) = setup();
        let before = texts(&export(&ds));
        let at = before.iter().filter(|t| t.0 == "101").count();
        assert_eq!(at, 1);
        pick_text(&mut ds, "101");
        ds.edit_text("Room 201").unwrap();
        let doc = export(&ds);
        let after = texts(&doc);
        assert_eq!(after.len(), before.len());
        assert!(after.contains(&("Room 201".to_string(), "A-ANNO-TEXT".to_string())));
        assert!(!after.iter().any(|t| t.0 == "101"));
        // and reads back from the written document
        let back = Underlay::from_dxf(&doc, 0.001);
        assert!(back.texts.iter().any(|t| t.text == "Room 201"));
        assert!(!back.texts.iter().any(|t| t.text == "101"));
    }

    #[test]
    fn move_pick_to_new_layer_exported() {
        let (mut ed, mut ds) = setup();
        let i = wall_line(&ds);
        let (a, b) = (ds.list[0].u.lines[i].a, ds.list[0].u.lines[i].b);
        ds.pick = Some((
            0,
            Pick {
                lines: vec![i],
                ..Default::default()
            },
        ));
        let n0 = ed.undo_len();
        let layers0 = ds.list[0].u.layers.len();
        let walls0 = ds.list[0].layer("A-WALL").unwrap().count;
        assert!(ds.move_pick_to_layer("  ").is_err());
        assert!(ds.move_pick_to_layer("A/B").is_err());
        assert!(ds.move_pick_to_layer("a-wall").is_err(), "already there, any case");
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert!(!ds.list[0].dirty);

        ds.move_pick_to_layer(" A-WALL-NEW ").unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one edit, one step");
        let d = &ds.list[0];
        assert!(d.dirty);
        assert_eq!(d.u.lines[i].layer, "A-WALL-NEW");
        assert_eq!(d.u.layers.len(), layers0 + 1);
        let fl = |n: &str| d.u.layers.iter().find(|l| l.name == n).unwrap().clone();
        let (src, new) = (fl("A-WALL"), fl("A-WALL-NEW"));
        assert_eq!(
            (new.color, new.weight_mm, &new.linetype),
            (src.color, src.weight_mm, &src.linetype)
        );
        assert!(!new.off && !new.frozen);
        let st = d.layer("A-WALL-NEW").unwrap();
        assert_eq!(st.count, 1);
        assert!(st.shown());
        assert_eq!(d.layer("A-WALL").unwrap().count + 1, walls0);

        // the layer table entry looks like its source; the line sits on it, file coordinates
        let doc = export(&ds);
        let lw = |n: &str| doc.layers.iter().find(|l| l.name == n).unwrap().clone();
        let (src, new) = (lw("A-WALL"), lw("A-WALL-NEW"));
        assert_eq!((new.color, &new.linetype, &new.extra), (src.color, &src.linetype, &src.extra));
        let f = |q: (f64, f64)| (q.0 * 1000.0, q.1 * 1000.0);
        assert!(has_line(&doc, "A-WALL-NEW", f(a), f(b)));
        assert!(!has_line(&doc, "A-WALL", f(a), f(b)));

        // an existing layer in any case: no new table entry
        ds.move_pick_to_layer("a-furn").unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        assert_eq!(ds.list[0].u.lines[i].layer, "A-FURN");
        assert_eq!(ds.list[0].u.layers.len(), layers0 + 1);
        assert!(has_line(&export(&ds), "A-FURN", f(a), f(b)));
    }

    #[test]
    fn add_line_to_drawing_roundtrips() {
        let (mut ed, mut ds) = setup();
        ds.sel = 0;
        ds.list[0].cur_layer = "A-FURN".into();
        assert_eq!(ds.list[0].current_layer(), "A-FURN");
        let (lines0, closed0) = (ds.list[0].u.lines.len(), ds.list[0].u.closed.len());
        let n0 = ed.undo_len();
        assert!(ds.add_path(&[v2(1.0, 1.0)], false).is_err());
        assert!(ds.add_path(&[v2(1.0, 1.0), v2(1.0, 1.0)], false).is_err());
        assert!(ds.add_path(&[v2(0.0, 0.0), v2(1.0, 0.0)], true).is_err());
        assert!(ds.add_path(&[v2(f64::NAN, 0.0), v2(1.0, 0.0)], false).is_err());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert!(!ds.list[0].dirty);

        ds.add_path(&[v2(20.0, 1.0), v2(23.0, 1.0), v2(23.0, 3.0)], false)
            .unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one polyline, one step");
        let d = &ds.list[0];
        assert!(d.dirty);
        assert_eq!(d.u.lines.len(), lines0 + 2);
        assert!(d.u.lines[lines0..].iter().all(|l| l.layer == "A-FURN"));
        let ring = [v2(30.0, 0.0), v2(32.0, 0.0), v2(32.0, 2.0), v2(30.0, 2.0)];
        ds.add_path(&ring, true).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        assert_eq!(ds.list[0].u.lines.len(), lines0 + 6);
        assert_eq!(ds.list[0].u.closed.len(), closed0 + 1);

        // written to the DXF and read back on the same layer and spots
        let doc = export(&ds);
        let back = Underlay::from_dxf(&doc, 0.001);
        let has = |a: (f64, f64), b: (f64, f64)| {
            back.lines.iter().any(|l| {
                l.layer == "A-FURN"
                    && ((near(l.a, a) && near(l.b, b)) || (near(l.a, b) && near(l.b, a)))
            })
        };
        assert!(has((20.0, 1.0), (23.0, 1.0)));
        assert!(has((23.0, 1.0), (23.0, 3.0)));
        for j in 0..4 {
            let (p, q) = (ring[j], ring[(j + 1) % 4]);
            assert!(has((p.x, p.y), (q.x, q.y)), "ring edge {j}");
        }

        undo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.lines.len(), lines0 + 2);
        assert_eq!(ds.list[0].u.closed.len(), closed0);
        undo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.lines.len(), lines0);
        redo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.lines.len(), lines0 + 2);
    }

    #[test]
    fn draw_clicks_snap_and_finish() {
        let (mut ed, mut ds) = setup();
        let level = ed.level;
        let l = ds.list[0].u.lines[wall_line(&ds)].clone();
        assert!(!ds.draw_click(level, v2(0.0, 0.0), 0.05), "off unless the draw mode is on");
        ds.edit.draw = Some(vec![]);
        assert!(!ds.draw_click(level + 1000, v2(0.0, 0.0), 0.05), "other level");
        let end = v2(l.a.0, l.a.1);
        assert!(ds.draw_click(level, end + v2(0.003, -0.002), 0.05));
        let got = ds.edit.draw.as_ref().unwrap()[0];
        assert!((got - end).len() < 1e-12, "snapped to the line end: {got:?}");
        let free = v2(50.0, 50.0);
        assert!(ds.draw_click(level, free, 0.05));
        assert_eq!(ds.edit.draw.as_ref().unwrap()[1], free, "nothing near: as clicked");
        // a click near a point of the line being drawn snaps onto it
        assert_eq!(ds.snap_point(level, free + v2(0.01, 0.0), 0.05).0, free);
        let n0 = ed.undo_len();
        let lines0 = ds.list[0].u.lines.len();
        ds.draw_finish(false).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        assert_eq!(ds.list[0].u.lines.len(), lines0 + 1);
        assert_eq!(ds.edit.draw.as_deref(), Some(&[][..]), "mode stays on, points cleared");
        // too few points: refused, the point stays
        assert!(ds.draw_click(level, free, 0.05));
        assert!(ds.draw_finish(false).is_err());
        assert_eq!(ds.edit.draw.as_ref().map(|v| v.len()), Some(1));
        // the draw mode ends with the window like pick and trace
        ds.layers_open = false;
        assert!(ds.sync_tool(crate::drawing::tool_sig(&ed)).is_some());
        assert!(ds.edit.draw.is_none());
    }

    #[test]
    fn undo_restores_text_and_layer() {
        let (mut ed, mut ds) = setup();
        let states0 = ds.list[0].layers.clone();
        let layers0 = ds.list[0].u.layers.clone();
        let ti = pick_text(&mut ds, "102");
        ds.edit_text("Hall").unwrap();
        let li = wall_line(&ds);
        ds.pick = Some((
            0,
            Pick {
                lines: vec![li],
                texts: vec![ti],
                ..Default::default()
            },
        ));
        ds.move_pick_to_layer("NOTES").unwrap();
        sync(&mut ed, &mut ds);
        let at = |ds: &Drawings| {
            let d = &ds.list[0];
            (
                d.u.texts[ti].text.clone(),
                d.u.texts[ti].layer.clone(),
                d.u.lines[li].layer.clone(),
            )
        };
        let edited = ("Hall".to_string(), "NOTES".to_string(), "NOTES".to_string());
        assert_eq!(at(&ds), edited);
        assert_eq!(ds.list[0].layer("NOTES").unwrap().count, 2);

        undo(&mut ed, &mut ds);
        assert_eq!(
            at(&ds),
            ("Hall".into(), "A-ANNO-TEXT".into(), "A-WALL".into())
        );
        assert!(ds.list[0].layer("NOTES").is_none(), "the new layer leaves with undo");
        assert_eq!(ds.list[0].u.layers, layers0);
        undo(&mut ed, &mut ds);
        assert_eq!(
            at(&ds),
            ("102".into(), "A-ANNO-TEXT".into(), "A-WALL".into())
        );
        assert_eq!(ds.list[0].layers, states0, "layer list and counts as opened");

        redo(&mut ed, &mut ds);
        redo(&mut ed, &mut ds);
        assert_eq!(at(&ds), edited);
        assert_eq!(ds.list[0].layer("NOTES").unwrap().count, 2);
        let doc = export(&ds);
        assert!(texts(&doc).contains(&("Hall".to_string(), "NOTES".to_string())));
    }

    #[test]
    fn strings_are_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let cyrillic = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let c = Catalog::new(Lang::En);
        let keys = [
            "dwg_edit.edited",
            "dwg_edit.st.text_one",
            "dwg_edit.st.text_empty",
            "dwg_edit.st.text_same",
            "dwg_edit.st.text_set",
            "dwg_edit.st.layer_empty",
            "dwg_edit.st.layer_bad",
            "dwg_edit.st.layer_same",
            "dwg_edit.st.layer_moved",
            "dwg_edit.st.layer_created",
            "dwg_edit.st.path_bad",
            "dwg_edit.st.path_short",
            "dwg_edit.st.path_closed_short",
            "dwg_edit.st.path_added",
            "dwg_edit.text",
            "dwg_edit.text_tip",
            "dwg_edit.layer",
            "dwg_edit.layer_hint",
            "dwg_edit.to_layer",
            "dwg_edit.to_layer_tip",
            "dwg_edit.cur_layer",
            "dwg_edit.draw",
            "dwg_edit.draw_tip",
            "dwg_edit.draw_points",
            "dwg_edit.finish",
            "dwg_edit.close",
            "dwg_edit.cancel",
        ];
        for k in keys {
            let (en, ru) = (c.t_in(Lang::En, k), c.t_in(Lang::Ru, k));
            assert_ne!(en, k, "missing in en");
            assert!(!cyrillic(&en), "{k}: {en}");
            assert!(cyrillic(&ru), "{k}: missing in ru: {ru}");
        }
    }

    fn pick_wall(ds: &mut Drawings) {
        let i = wall_line(ds);
        ds.pick = Some((
            0,
            Pick {
                lines: vec![i],
                ..Default::default()
            },
        ));
    }

    #[test]
    fn undo_all_edits_clears_edited_mark() {
        let (mut ed, mut ds) = setup();
        pick_text(&mut ds, "101");
        ds.edit_text("Kitchen 101").unwrap();
        sync(&mut ed, &mut ds);
        pick_wall(&mut ds);
        ds.move_pick_to_layer("A-WALL-NEW").unwrap();
        sync(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
        undo(&mut ed, &mut ds);
        assert!(ds.list[0].dirty, "one edit is still in");
        assert_ne!(ds.list[0].title(), "p");
        undo(&mut ed, &mut ds);
        assert!(!ds.list[0].dirty, "back at the opened state");
        assert_eq!(ds.list[0].title(), "p");
        redo(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
        undo(&mut ed, &mut ds);
        assert!(!ds.list[0].dirty);
        // a new edit after undo is an edit, never equal to a state seen before
        pick_text(&mut ds, "101");
        ds.edit_text("Hall 101").unwrap();
        sync(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
        undo(&mut ed, &mut ds);
        assert!(!ds.list[0].dirty);
    }

    #[test]
    fn redo_after_save_marks_edited_again() {
        let (mut ed, mut ds) = setup();
        pick_text(&mut ds, "101");
        ds.edit_text("Kitchen 101").unwrap();
        sync(&mut ed, &mut ds);
        undo(&mut ed, &mut ds);
        assert!(!ds.list[0].dirty);
        // saved at the opened state, then redo moves away from the file
        let snap = ds.generations();
        ds.mark_saved(&snap);
        redo(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
        assert_ne!(ds.list[0].title(), "p");
        // saved after redo: clean, and undo goes away from the file again
        let snap = ds.generations();
        ds.mark_saved(&snap);
        assert!(!ds.list[0].dirty);
        undo(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
    }

    #[test]
    fn export_sets_saved_generation() {
        let (mut ed, mut ds) = setup();
        pick_text(&mut ds, "101");
        ds.edit_text("Kitchen 101").unwrap();
        sync(&mut ed, &mut ds);
        assert!(ds.list[0].dirty);
        // the file is written from this state; an edit made while the job runs stays edited
        let snap = ds.generations();
        pick_text(&mut ds, "Kitchen 101");
        ds.edit_text("Hall 101").unwrap();
        sync(&mut ed, &mut ds);
        ds.mark_saved(&snap);
        assert!(ds.list[0].dirty, "the later edit is not in the file");
        // saved from the live state: not edited
        let snap = ds.generations();
        ds.mark_saved(&snap);
        assert!(!ds.list[0].dirty);
        assert_eq!(ds.list[0].title(), "p");
        // a drawing gone from the list is skipped, not an error
        ds.mark_saved(&[(u64::MAX, 7)]);
        assert!(!ds.list[0].dirty);
    }

    #[test]
    fn new_layer_override_survives_undo_redo() {
        let (mut ed, mut ds) = setup();
        pick_wall(&mut ds);
        ds.move_pick_to_layer("A-WALL-NEW").unwrap();
        sync(&mut ed, &mut ds);
        let k = ds.list[0].idx["A-WALL-NEW"];
        ds.list[0].layers[k].color = Some([1, 2, 3]);
        ds.list[0].layers[k].halftone = true;
        ds.list[0].layers[k].visible = false;
        ds.list[0].layers[k].frozen = true;
        undo(&mut ed, &mut ds);
        assert!(ds.list[0].layer("A-WALL-NEW").is_none());
        redo(&mut ed, &mut ds);
        let st = ds.list[0].layer("A-WALL-NEW").expect("the layer is back");
        assert_eq!(st.color, Some([1, 2, 3]));
        assert!(st.halftone && !st.visible && st.frozen);
        assert_eq!(st.count, 1);
        // the index follows the list
        let k = ds.list[0].idx["A-WALL-NEW"];
        assert_eq!(ds.list[0].layers[k].name, "A-WALL-NEW");
        // settings of layers that stayed are untouched
        undo(&mut ed, &mut ds);
        redo(&mut ed, &mut ds);
        assert_eq!(ds.list[0].layer("A-WALL-NEW").unwrap().color, Some([1, 2, 3]));
    }
}
