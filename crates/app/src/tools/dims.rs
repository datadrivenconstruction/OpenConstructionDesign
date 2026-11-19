// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Dimensions (TOOLS_UI §1 "Annotations"): DI — linear chain (click walls: nearest end, axis
//! or face by the click side; Tab — axis/face; click in empty space — line position, whole
//! chain in one operation), DA — angular (two walls), DR — radial (arc wall).

use crate::editor::{Editor, OpFn};
use crate::ui_kit::Tokens;
use ddc_geom::{v2, V2};
use ddc_model::{DimKind, DimRef, DimSpec};
use egui::{self, Color32, Pos2, Stroke};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimMode {
    Linear,
    Angular,
    Radial,
}

/// Anchor of a linear dimension reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attach {
    Axis,
    Face,
}

#[derive(Clone, Debug)]
pub struct DimTool {
    pub mode: DimMode,
    pub attach: Attach,
    /// References and their plan points.
    pub refs: Vec<(DimRef, V2)>,
    pub cursor: V2,
}

pub enum DimOut {
    Nothing,
    Done(usize),
    Exit,
    Error(String),
}

/// Wall axis, its half-thickness and id under point `p` (pick radius `r`).
fn wall_at(ed: &Editor, p: V2, r: f64) -> Option<(i64, V2, V2, f64)> {
    ed.walls
        .iter()
        .map(|w| {
            let t = ed
                .types
                .iter()
                .find(|t| t.id == w.wall_type)
                .map_or(0.2, |t| t.thickness());
            let d = (crate::snap::closest_on_segment(p, w.a, w.b).0 - p).len();
            (d - t / 2.0, w.id, w.a, w.b, t / 2.0)
        })
        .filter(|x| x.0 <= r)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| (x.1, x.2, x.3, x.4))
}

impl DimTool {
    pub fn new(mode: DimMode) -> Self {
        Self {
            mode,
            attach: Attach::Face,
            refs: vec![],
            cursor: v2(0.0, 0.0),
        }
    }

    pub fn step(&self) -> &'static str {
        match (self.mode, self.refs.len()) {
            (DimMode::Linear, 0) => "first",
            (DimMode::Linear, 1) => "next",
            (DimMode::Linear, _) => "place",
            (DimMode::Angular, 0) => "first",
            (DimMode::Angular, _) => "next",
            (DimMode::Radial, _) => "first",
        }
    }

    pub fn hint_tool(&self) -> &'static str {
        match self.mode {
            DimMode::Linear => "dim",
            DimMode::Angular => "dim_angular",
            DimMode::Radial => "dim_radial",
        }
    }

    /// Reference under the cursor for a linear dimension: nearest wall end, axis/face by side.
    pub fn pick_ref(&self, ed: &Editor, p: V2, r: f64) -> Option<(DimRef, V2)> {
        let (wall, a, b, half) = wall_at(ed, p, r)?;
        let d = b - a;
        let l = d.len().max(1e-12);
        let n = v2(-d.y / l, d.x / l);
        let end = u8::from((p - b).len() < (p - a).len());
        let e = if end == 0 { a } else { b };
        match self.attach {
            Attach::Axis => Some((DimRef::AxisEnd { wall, end }, e)),
            Attach::Face => {
                let left = (p - a).dot(n) > 0.0;
                let face = u8::from(left);
                Some((
                    DimRef::WallEnd { wall, face, end },
                    e + n * if left { half } else { -half },
                ))
            }
        }
    }

    /// Chain direction: along the larger spread of references.
    pub fn dir(&self) -> V2 {
        let xs = self.refs.iter().map(|r| r.1.x);
        let ys = self.refs.iter().map(|r| r.1.y);
        let span = |it: &mut dyn Iterator<Item = f64>| {
            let v: Vec<f64> = it.collect();
            v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min)
        };
        if span(&mut xs.into_iter()) >= span(&mut ys.into_iter()) {
            v2(1.0, 0.0)
        } else {
            v2(0.0, 1.0)
        }
    }

    /// Chain specs with the line through `at`: adjacent references by projection onto the direction.
    pub fn chain(&self, level: i64, at: V2) -> Vec<DimSpec> {
        let dir = self.dir();
        let pts: Vec<V2> = self.refs.iter().map(|r| r.1).collect();
        ddc_geom::dim::chain(&pts, dir, 1e-6)
            .into_iter()
            .map(|(i, j)| {
                let (a, b) = (pts[i], pts[j]);
                let left = v2(-dir.y, dir.x);
                let side = if (at - a).dot(left) >= 0.0 { 1.0 } else { -1.0 };
                let n = left * side;
                let b0 = if (a - b).dot(n) >= 0.0 { a } else { b };
                DimSpec {
                    level,
                    kind: DimKind::Linear,
                    refs: vec![self.refs[i].0, self.refs[j].0],
                    dir: (dir.x, dir.y),
                    side,
                    offset: (at - b0).dot(n),
                    tier: -1,
                    auto: false,
                }
            })
            .collect()
    }

    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64) -> DimOut {
        match self.mode {
            DimMode::Linear => {
                if let Some(rf) = self.pick_ref(ed, p, r) {
                    if !self.refs.iter().any(|x| x.0 == rf.0) {
                        self.refs.push(rf);
                    }
                    return DimOut::Nothing;
                }
                if self.refs.len() < 2 {
                    return DimOut::Error(crate::tr("edit_panels.dim.min_refs", &[]));
                }
                let specs = self.chain(ed.level, p);
                self.commit(ed, specs)
            }
            DimMode::Angular => {
                let Some((wall, ..)) = wall_at(ed, p, r) else {
                    return DimOut::Nothing;
                };
                self.refs.push((DimRef::AxisLine { wall }, p));
                if self.refs.len() < 2 {
                    return DimOut::Nothing;
                }
                let s = DimSpec {
                    level: ed.level,
                    kind: DimKind::Angular,
                    refs: vec![self.refs[0].0, self.refs[1].0],
                    dir: (0.0, 0.0),
                    side: 1.0,
                    offset: 1.0,
                    tier: -1,
                    auto: false,
                };
                self.commit(ed, vec![s])
            }
            DimMode::Radial => {
                let Some((wall, ..)) = wall_at(ed, p, r) else {
                    return DimOut::Nothing;
                };
                let s = DimSpec {
                    level: ed.level,
                    kind: DimKind::Radial,
                    refs: vec![DimRef::ArcFace { wall, face: 1 }],
                    dir: (0.0, 0.0),
                    side: 1.0,
                    offset: 0.0,
                    tier: -1,
                    auto: false,
                };
                self.commit(ed, vec![s])
            }
        }
    }

    fn commit(&mut self, ed: &mut Editor, specs: Vec<DimSpec>) -> DimOut {
        self.refs.clear();
        let n = specs.len();
        let f = OpFn(Arc::new(move |m| m.add_dims(&specs).map(|_| ())));
        match ed.run_op(f) {
            Ok(()) => DimOut::Done(n),
            Err(e) => DimOut::Error(crate::tr("edit_panels.dim.fail", &[("e", e.to_string().as_str())])),
        }
    }

    /// Esc: first press resets references, second exits.
    pub fn escape(&mut self) -> DimOut {
        if self.refs.is_empty() {
            DimOut::Exit
        } else {
            self.refs.clear();
            DimOut::Nothing
        }
    }

    pub fn tab(&mut self) {
        self.attach = match self.attach {
            Attach::Axis => Attach::Face,
            Attach::Face => Attach::Axis,
        };
    }
}

// ---------------------------------------------------------------------------------------------
// Placed dimensions: drawing of every kind, hit test, selection, delete and drag of the line.

/// Part of a placed dimension under the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Dimension line or arc (drag moves it).
    Line,
    /// Witness line, tick or marker at a measured point.
    Witness,
    /// Value text.
    Label,
}

/// Drawing primitive in model coordinates; a label anchor is its centre.
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Seg(V2, V2, f32, Part),
    Label(V2, String),
}

/// Tick half-size, witness overshoot and label lift in the model, m (as the chain preview).
const TICK: f64 = 0.08;
const OVER: f64 = 0.15;
const LIFT: f64 = 0.12;

fn polar(c: V2, r: f64, deg: f64) -> V2 {
    let a = deg.to_radians();
    c + v2(a.cos(), a.sin()) * r
}

/// Arc from `a0` counter-clockwise to `a1` (degrees) as segments; returns the mid angle.
fn arc_segs(out: &mut Vec<Prim>, c: V2, r: f64, a0: f64, a1: f64) -> f64 {
    let sweep = (a1 - a0).rem_euclid(360.0);
    let sweep = if sweep < 1e-9 { 360.0 } else { sweep };
    let n = ((sweep / 5.0).ceil() as usize).clamp(4, 72);
    let mut prev = polar(c, r, a0);
    for i in 1..=n {
        let q = polar(c, r, a0 + sweep * i as f64 / n as f64);
        out.push(Prim::Seg(prev, q, 1.0, Part::Line));
        prev = q;
    }
    a0 + sweep / 2.0
}

/// Oblique tick across the line at `m` (line direction `d`, normal `n`).
fn tick(out: &mut Vec<Prim>, m: V2, d: V2, n: V2) {
    let k = (d + n) * TICK;
    out.push(Prim::Seg(m - k, m + k, 1.5, Part::Witness));
}

/// Angle text: whole degrees when whole, else one decimal.
fn deg_text(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}\u{b0}", v.round() as i64)
    } else {
        format!("{}\u{b0}", crate::units::fmt_num(v, 1))
    }
}

fn unit(a_deg: f64) -> V2 {
    let a = a_deg.to_radians();
    v2(a.cos(), a.sin())
}

/// Primitives of a dimension geometry; `len` formats a length for the label.
pub fn prims(g: &ddc_model::DimGeom, len: &dyn Fn(f64) -> String) -> Vec<Prim> {
    use ddc_model::DimGeom as G;
    let mut out = vec![];
    match g {
        G::Linear(l) => {
            let (a, b) = l.line(0.0);
            out.push(Prim::Seg(a, b, 1.0, Part::Line));
            for (q, m) in [(l.a, a), (l.b, b)] {
                out.push(Prim::Seg(q, m + l.n * OVER, 0.75, Part::Witness));
                tick(&mut out, m, l.dir, l.n);
            }
            out.push(Prim::Label((a + b) * 0.5 + l.n * LIFT, len(l.value())));
        }
        G::Angular {
            center,
            radius,
            a0,
            a1,
            value,
        } => {
            let c = v2(center.0, center.1);
            let mid = arc_segs(&mut out, c, *radius, *a0, *a1);
            for a in [*a0, *a1] {
                let rad = unit(a);
                out.push(Prim::Seg(c, polar(c, radius + OVER, a), 0.75, Part::Witness));
                tick(&mut out, polar(c, *radius, a), v2(-rad.y, rad.x), rad);
            }
            let at = polar(c, radius + LIFT * 1.5, mid);
            out.push(Prim::Label(at, deg_text(*value)));
        }
        G::Arc {
            center,
            radius,
            a0,
            a1,
            value,
        } => {
            let c = v2(center.0, center.1);
            let mid = arc_segs(&mut out, c, *radius, *a0, *a1);
            for a in [*a0, *a1] {
                let rad = unit(a);
                let m = polar(c, *radius, a);
                out.push(Prim::Seg(m - rad * OVER, m + rad * OVER, 0.75, Part::Witness));
                tick(&mut out, m, v2(-rad.y, rad.x), rad);
            }
            out.push(Prim::Label(polar(c, radius + LIFT * 1.5, mid), len(*value)));
        }
        G::Radial {
            center,
            radius,
            angle,
        } => {
            let c = v2(center.0, center.1);
            let d = unit(*angle);
            let n = v2(-d.y, d.x);
            let m = polar(c, *radius, *angle);
            out.push(Prim::Seg(c, m, 1.0, Part::Line));
            // arrowhead on the arc, cross at the centre
            out.push(Prim::Seg(m, m - (d * 2.0 + n) * TICK, 1.5, Part::Witness));
            out.push(Prim::Seg(m, m - (d * 2.0 - n) * TICK, 1.5, Part::Witness));
            out.push(Prim::Seg(c - d * TICK, c + d * TICK, 0.75, Part::Witness));
            out.push(Prim::Seg(c - n * TICK, c + n * TICK, 0.75, Part::Witness));
            let at = (c + m) * 0.5 + n * LIFT;
            out.push(Prim::Label(at, format!("R {}", len(*radius))));
        }
        G::Spot { at, value } => {
            let p = v2(at.0, at.1);
            let h = TICK * 1.5;
            let (l, r) = (p + v2(-h, h * 1.4), p + v2(h, h * 1.4));
            out.push(Prim::Seg(p, l, 1.0, Part::Witness));
            out.push(Prim::Seg(p, r, 1.0, Part::Witness));
            out.push(Prim::Seg(l, r, 1.0, Part::Witness));
            let e = r + v2(OVER * 4.0, 0.0);
            out.push(Prim::Seg(l, e, 1.0, Part::Line));
            let sign = if *value > 1e-9 { "+" } else { "" };
            let text = format!("{sign}{}", len(*value));
            out.push(Prim::Label((l + e) * 0.5 + v2(0.0, LIFT), text));
        }
    }
    out
}

fn len_fn(ed: &Editor) -> impl Fn(f64) -> String {
    let u = ed.tool.units;
    move |m| crate::units::display_length(m, u)
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    (crate::snap::closest_on_segment(p, a, b).0 - p).len()
}

/// Placed dimension of the current level under `p` (capture radius `r`, m) and its part;
/// the nearest wins, a label counts as distance 0.
pub fn hit(ed: &Editor, p: V2, r: f64) -> Option<(i64, Part)> {
    let ds = ed.model.dims(ed.level).ok()?;
    let len = len_fn(ed);
    // r is 8 px in the model: label box from an approximate caption glyph size
    let px = r / 8.0;
    let mut best: Option<(f64, i64, Part)> = None;
    for d in &ds {
        for pr in prims(&d.geom, &len) {
            let (dist, part) = match pr {
                Prim::Seg(a, b, _, part) => (seg_dist(p, a, b), part),
                Prim::Label(c, s) => {
                    let (hw, hh) = (s.chars().count() as f64 * 3.5 * px, 7.0 * px);
                    let q = p - c;
                    if q.x.abs() > hw || q.y.abs() > hh {
                        continue;
                    }
                    (0.0, Part::Label)
                }
            };
            if dist <= r && best.is_none_or(|b| dist < b.0) {
                best = Some((dist, d.id, part));
            }
        }
    }
    best.map(|b| (b.1, b.2))
}

/// An element the plan click would pick lies under `p`. Rooms and floors cover whole
/// interiors, so they do not hide a dimension drawn over them.
pub fn element_at(
    ed: &Editor,
    p: V2,
    r: f64,
    visible: impl Fn(crate::editor::Sel) -> bool,
) -> bool {
    use crate::editor::Sel;
    crate::selection::candidates(ed, p, r, visible)
        .iter()
        .flatten()
        .any(|s| !matches!(s, Sel::Room(_) | Sel::Slab(_)))
}

/// Geometry and spec offset after dragging the dimension line from `from` to `to`;
/// None: this kind has no movable line (its place follows the referenced elements).
pub fn dragged(a: &ddc_model::Annot, from: V2, to: V2) -> Option<(ddc_model::DimGeom, f64)> {
    use ddc_model::DimGeom as G;
    match &a.geom {
        G::Linear(l) => {
            let d = (to - from).dot(l.n);
            let mut m = *l;
            m.base = l.base + l.n * d;
            Some((G::Linear(m), a.spec.offset + d))
        }
        G::Angular {
            center,
            a0,
            a1,
            value,
            ..
        } => {
            let r = (to - v2(center.0, center.1)).len().max(0.05);
            let g = G::Angular {
                center: *center,
                radius: r,
                a0: *a0,
                a1: *a1,
                value: *value,
            };
            Some((g, r))
        }
        _ => None,
    }
}

/// Selected placed dimension of the plan and the drag of its line.
#[derive(Clone, Debug, Default)]
pub struct DimSel {
    pub id: Option<i64>,
    /// (dimension, press point, current point), model coordinates.
    pub drag: Option<(i64, V2, V2)>,
}

fn find(ed: &Editor, id: i64) -> Option<ddc_model::Annot> {
    ed.model
        .dims(ed.level)
        .ok()?
        .into_iter()
        .find(|a| a.id == id)
}

fn op_err(e: ddc_model::Error) -> String {
    crate::tr("dims.err", &[("e", &e.to_string())])
}

impl DimSel {
    /// An element selection made elsewhere replaces the dimension selection.
    pub fn sync(&mut self, ed: &Editor) {
        if !ed.selection.is_empty() && self.drag.is_none() {
            self.id = None;
        }
    }

    /// Plan click; `elem`: an element is under the cursor and wins. True: the click was taken.
    pub fn click(&mut self, ed: &mut Editor, p: V2, r: f64, elem: bool) -> bool {
        self.drag = None;
        if elem {
            self.id = None;
            return false;
        }
        match hit(ed, p, r) {
            Some((id, _)) => {
                self.id = Some(id);
                ed.selection.clear();
                true
            }
            None => {
                self.id = None;
                false
            }
        }
    }

    /// Press on a dimension line or label selects it and starts the drag. True: taken.
    pub fn press(&mut self, ed: &mut Editor, p: V2, r: f64, elem: bool) -> bool {
        if elem {
            return false;
        }
        match hit(ed, p, r) {
            Some((id, Part::Line | Part::Label)) => {
                self.id = Some(id);
                self.drag = Some((id, p, p));
                ed.selection.clear();
                true
            }
            _ => false,
        }
    }

    pub fn moved(&mut self, p: V2) {
        if let Some(d) = &mut self.drag {
            d.2 = p;
        }
    }

    /// Release: the new offset as one undo step. Ok(false): nothing changed.
    pub fn release(&mut self, ed: &mut Editor) -> Result<bool, String> {
        let Some((id, from, to)) = self.drag.take() else {
            return Ok(false);
        };
        let Some(a) = find(ed, id) else {
            self.id = None;
            return Ok(false);
        };
        let Some((_, offset)) = dragged(&a, from, to) else {
            return Ok(false);
        };
        if (offset - a.spec.offset).abs() < 1e-9 {
            return Ok(false);
        }
        let mut spec = a.spec.clone();
        spec.offset = offset;
        let f = OpFn(Arc::new(move |m| m.update_dim(id, &spec)));
        ed.run_op(f).map(|_| true).map_err(op_err)
    }

    /// Del: removes the selected dimension as one undo step. Ok(false): nothing selected.
    pub fn delete(&mut self, ed: &mut Editor) -> Result<bool, String> {
        self.drag = None;
        let Some(id) = self.id.take() else {
            return Ok(false);
        };
        if find(ed, id).is_none() {
            return Ok(false);
        }
        let f = OpFn(Arc::new(move |m| m.delete_dim(id)));
        ed.run_op(f).map(|_| true).map_err(op_err)
    }

    /// Esc: cancels the drag first, then clears the selection. True: something was cleared.
    pub fn escape(&mut self) -> bool {
        if self.drag.take().is_some() {
            return true;
        }
        self.id.take().is_some()
    }
}

fn paint(
    p: &egui::Painter,
    pr: &[Prim],
    to_s: &dyn Fn(V2) -> Pos2,
    col: Color32,
    extra: f32,
    t: &Tokens,
) {
    for x in pr {
        if let Prim::Seg(a, b, w, _) = x {
            p.line_segment([to_s(*a), to_s(*b)], Stroke::new(w + extra, col));
        }
    }
    for x in pr {
        if let Prim::Label(c, s) = x {
            let g = p.layout_no_wrap(
                s.clone(),
                egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
                col,
            );
            let r = egui::Rect::from_center_size(to_s(*c), g.size());
            p.rect_filled(r.expand(1.0), ddc_ui::theme::radius::SM, t.paper);
            p.galley(r.min, g, col);
        }
    }
}

/// Every placed dimension of the level, each kind; a lost reference shows in the warning
/// colour. Returns how many were drawn.
pub fn draw(p: &egui::Painter, ed: &Editor, to_s: &dyn Fn(V2) -> Pos2, t: &Tokens) -> usize {
    draw_level(p, ed, ed.level, to_s, t)
}

/// The placed dimensions of `level`, drawn as on the live plan of that level.
pub fn draw_level(p: &egui::Painter, ed: &Editor, level: i64, to_s: &dyn Fn(V2) -> Pos2, t: &Tokens) -> usize {
    let Ok(ds) = ed.model.dims(level) else {
        return 0;
    };
    let len = len_fn(ed);
    for d in &ds {
        let col = if d.orphan { t.ov().warning } else { t.text };
        paint(p, &prims(&d.geom, &len), to_s, col, 0.0, t);
    }
    ds.len()
}

/// The selected dimension over the plan: selection colour, live drag preview, grips at the
/// measured points and a move grip on the line.
pub fn draw_selected(
    p: &egui::Painter,
    ed: &Editor,
    sel: &DimSel,
    to_s: &dyn Fn(V2) -> Pos2,
    t: &Tokens,
) {
    use crate::overlay::{grip, Grip};
    use ddc_model::DimGeom as G;
    let Some(id) = sel.id else {
        return;
    };
    let Some(a) = find(ed, id) else {
        return;
    };
    let ov = t.ov();
    let geom = match sel.drag {
        Some((d, from, to)) if d == id => dragged(&a, from, to).map_or(a.geom.clone(), |g| g.0),
        _ => a.geom.clone(),
    };
    paint(p, &prims(&geom, &len_fn(ed)), to_s, ov.selection, 0.75, t);
    let hot = sel.drag.is_some();
    let arrows = |n: V2| Grip::Arrows(egui::vec2(n.x as f32, -n.y as f32).normalized());
    match &geom {
        G::Linear(l) => {
            for q in [l.a, l.b] {
                grip(p, to_s(q), Grip::Square, false, ov);
            }
            let (s, e) = l.line(0.0);
            grip(p, to_s((s + e) * 0.5), arrows(l.n), hot, ov);
        }
        G::Angular {
            center, radius, a0, a1, ..
        } => {
            let mid = a0 + (a1 - a0).rem_euclid(360.0) / 2.0;
            let c = v2(center.0, center.1);
            grip(p, to_s(polar(c, *radius, mid)), arrows(unit(mid)), hot, ov);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modify_tools::tests::walls;

    #[test]
    fn chain_of_three_walls_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        walls(
            &mut ed,
            &[
                ((0.0, 0.0), (0.0, 4.0)),
                ((3.0, 0.0), (3.0, 4.0)),
                ((7.0, 0.0), (7.0, 4.0)),
            ],
        );
        let mut t = DimTool::new(DimMode::Linear);
        t.attach = Attach::Axis;
        for x in [0.0, 3.0, 7.0] {
            assert!(matches!(t.click(&mut ed, v2(x, 0.3), 0.1), DimOut::Nothing));
        }
        assert_eq!(t.refs.len(), 3);
        assert!(matches!(
            t.click(&mut ed, v2(3.5, -1.0), 0.1),
            DimOut::Done(2)
        ));
        let ds = ed.model.dims(ed.level).unwrap();
        let mut v: Vec<f64> = ds
            .iter()
            .map(|d| (d.geom.value() * 1e6).round() / 1e6)
            .collect();
        v.sort_by(f64::total_cmp);
        assert_eq!(v, vec![3.0, 4.0]);
        ed.undo();
        assert!(ed.model.dims(ed.level).unwrap().is_empty());
        ed.redo();
        assert_eq!(ed.model.dims(ed.level).unwrap().len(), 2);
    }

    fn spec(level: i64, kind: DimKind, refs: Vec<DimRef>, offset: f64) -> DimSpec {
        DimSpec {
            level,
            kind,
            refs,
            dir: (0.0, 0.0),
            side: 1.0,
            offset,
            tier: -1,
            auto: false,
        }
    }

    fn add(ed: &mut Editor, s: DimSpec) {
        let f = OpFn(Arc::new(move |m| m.add_dim(&s).map(|_| ())));
        ed.run_op(f).unwrap();
    }

    /// Two walls x = 0 and x = 4 (y 0..4); one linear dimension between their start ends whose
    /// line runs at y = `line_y`. Returns the dimension id.
    fn linear_fixture(ed: &mut Editor, line_y: f64) -> i64 {
        walls(ed, &[((0.0, 0.0), (0.0, 4.0)), ((4.0, 0.0), (4.0, 4.0))]);
        let (w1, w2) = (ed.walls[0].id, ed.walls[1].id);
        let mut s = spec(
            ed.level,
            DimKind::Linear,
            vec![
                DimRef::AxisEnd { wall: w1, end: 0 },
                DimRef::AxisEnd { wall: w2, end: 0 },
            ],
            -line_y,
        );
        // side -1: normal (0, -1), so the line sits at y = -offset
        s.dir = (1.0, 0.0);
        s.side = -1.0;
        add(ed, s);
        let ds = ed.model.dims(ed.level).unwrap();
        assert_eq!(ds.len(), 1);
        let ddc_model::DimGeom::Linear(l) = &ds[0].geom else {
            panic!("linear expected")
        };
        assert!((l.line(0.0).0.y - line_y).abs() < 1e-9);
        ds[0].id
    }

    #[test]
    fn angular_and_radial_dims_drawn() {
        use ddc_model::WallSpec;
        let mut ed = Editor::new_in_memory().unwrap();
        walls(&mut ed, &[((0.0, 0.0), (4.0, 0.0)), ((0.0, 0.0), (0.0, 4.0))]);
        let (w1, w2) = (ed.walls[0].id, ed.walls[1].id);
        let lv = ed.level;
        let ty = ed.current_type().expect("type").id;
        let arc = ed
            .model
            .add_arc_wall(
                &WallSpec {
                    a: (15.0, 0.0),
                    b: (5.0, 0.0),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: ty,
                    offset0: None,
                },
                1.0,
            )
            .unwrap();
        ed.reload().unwrap();
        let lines = vec![DimRef::AxisLine { wall: w1 }, DimRef::AxisLine { wall: w2 }];
        add(&mut ed, spec(lv, DimKind::Angular, lines, 1.0));
        let face = vec![DimRef::ArcFace { wall: arc, face: 1 }];
        add(&mut ed, spec(lv, DimKind::Radial, face, 0.0));
        let ds = ed.model.dims(lv).unwrap();
        assert_eq!(ds.len(), 2);
        let len = len_fn(&ed);
        for d in &ds {
            let pr = prims(&d.geom, &len);
            let segs = pr
                .iter()
                .filter(|x| matches!(x, Prim::Seg(_, _, _, Part::Line)))
                .count();
            let label: Vec<&String> = pr
                .iter()
                .filter_map(|x| match x {
                    Prim::Label(_, s) => Some(s),
                    _ => None,
                })
                .collect();
            assert!(segs >= 1, "{:?} has a dimension line", d.spec.kind);
            assert_eq!(label.len(), 1);
            match d.spec.kind {
                DimKind::Angular => {
                    assert!(segs >= 4, "arc drawn as segments");
                    assert_eq!(label[0], "90\u{b0}");
                }
                DimKind::Radial => assert!(label[0].starts_with("R ")),
                k => panic!("unexpected {k:?}"),
            }
        }
        // the arc of the angular dimension and the radius line are hit like a linear line
        let ang = ds.iter().find(|d| d.spec.kind == DimKind::Angular).unwrap().id;
        let rad = ds.iter().find(|d| d.spec.kind == DimKind::Radial).unwrap().id;
        assert_eq!(hit(&ed, polar(v2(0.0, 0.0), 1.0, 45.0), 0.05), Some((ang, Part::Line)));
        assert_eq!(hit(&ed, v2(10.0, 2.0), 0.05), Some((rad, Part::Line)));
        // painted through the plan painter: both drawn, with real shapes
        let ctx = egui::Context::default();
        let t = crate::ui_kit::light();
        let mut drawn = 0;
        let out = ctx.run(Default::default(), |ctx| {
            let p = ctx.layer_painter(egui::LayerId::background());
            let to_s = |q: V2| egui::pos2(400.0 + q.x as f32 * 40.0, 400.0 - q.y as f32 * 40.0);
            drawn = draw(&p, &ed, &to_s, &t);
        });
        assert_eq!(drawn, 2);
        assert!(out.shapes.len() > 20, "shapes: {}", out.shapes.len());
    }

    #[test]
    fn click_selects_dim_del_deletes_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = linear_fixture(&mut ed, -1.0);
        let lv = ed.level;
        let (p, r) = (v2(2.0, -1.0), 0.1);
        let elem = element_at(&ed, p, r, |_| true);
        assert!(!elem, "nothing but the dimension under the line");
        let steps = ed.undo_len();
        let mut s = DimSel::default();
        assert!(s.click(&mut ed, p, r, elem));
        assert_eq!(s.id, Some(id));
        assert!(ed.selection.is_empty());
        assert_eq!(ed.undo_len(), steps, "selecting is not an edit");
        assert_eq!(s.delete(&mut ed), Ok(true));
        assert_eq!(s.id, None);
        assert!(ed.model.dims(lv).unwrap().is_empty());
        assert_eq!(ed.undo_len(), steps + 1, "one undo step");
        assert_eq!(s.delete(&mut ed), Ok(false), "nothing selected");
        assert_eq!(ed.undo_len(), steps + 1);
        ed.undo();
        let ds = ed.model.dims(lv).unwrap();
        assert_eq!(ds.len(), 1);
        ed.redo();
        assert!(ed.model.dims(lv).unwrap().is_empty());
        ed.undo();
        // Esc clears; a click on empty space clears
        let id = ed.model.dims(lv).unwrap()[0].id;
        assert!(s.click(&mut ed, p, r, false));
        assert_eq!(s.id, Some(id));
        assert!(s.escape());
        assert_eq!(s.id, None);
        assert!(!s.escape());
        assert!(s.click(&mut ed, p, r, false));
        assert!(!s.click(&mut ed, v2(2.0, -3.0), r, false));
        assert_eq!(s.id, None);
        assert_eq!(ed.model.dims(lv).unwrap().len(), 1);
    }

    #[test]
    fn drag_dim_line_changes_offset() {
        let mut ed = Editor::new_in_memory().unwrap();
        let id = linear_fixture(&mut ed, -1.0);
        let lv = ed.level;
        let r = 0.1;
        let steps = ed.undo_len();
        let mut s = DimSel::default();
        // a witness line is not a drag handle
        assert!(!s.press(&mut ed, v2(0.0, -0.5), r, false));
        assert!(s.drag.is_none());
        // press and release on the spot: no edit
        assert!(s.press(&mut ed, v2(1.0, -1.0), r, false));
        assert_eq!(s.release(&mut ed), Ok(false));
        assert_eq!(ed.undo_len(), steps);
        // Esc during the drag cancels it
        assert!(s.press(&mut ed, v2(1.0, -1.0), r, false));
        s.moved(v2(1.0, -4.0));
        assert!(s.escape());
        assert_eq!(s.release(&mut ed), Ok(false));
        assert_eq!(ed.undo_len(), steps);
        // drag 1.5 m away from the walls
        assert!(s.press(&mut ed, v2(1.0, -1.0), r, false));
        assert_eq!(s.id, Some(id));
        s.moved(v2(1.3, -2.0));
        s.moved(v2(1.6, -2.5));
        assert_eq!(s.release(&mut ed), Ok(true));
        assert_eq!(ed.undo_len(), steps + 1, "one undo step");
        let d = &ed.model.dims(lv).unwrap()[0];
        assert_eq!(d.id, id);
        assert!((d.spec.offset - 2.5).abs() < 1e-9, "offset {}", d.spec.offset);
        let ddc_model::DimGeom::Linear(l) = &d.geom else {
            panic!("linear expected")
        };
        assert!((l.line(0.0).0.y + 2.5).abs() < 1e-9);
        assert!((d.geom.value() - 4.0).abs() < 1e-9, "the measured value stays");
        ed.undo();
        let d = &ed.model.dims(lv).unwrap()[0];
        assert!((d.spec.offset - 1.0).abs() < 1e-9);
    }

    #[test]
    fn click_on_wall_still_selects_wall() {
        use crate::editor::Sel;
        use crate::wall_tool::{Event, Mode};
        let mut ed = Editor::new_in_memory().unwrap();
        // the dimension line crosses both walls at y = 2
        let id = linear_fixture(&mut ed, 2.0);
        ed.set_mode(Mode::Select);
        let w1 = ed.walls[0].id;
        let (p, r) = (v2(0.0, 2.0), 0.1);
        assert_eq!(hit(&ed, p, r).map(|h| h.0), Some(id), "the line passes here");
        let elem = element_at(&ed, p, r, |_| true);
        assert!(elem);
        let mut s = DimSel {
            id: Some(id),
            drag: None,
        };
        assert!(!s.press(&mut ed, p, r, elem), "the wall drag wins");
        assert!(!s.click(&mut ed, p, r, elem));
        assert_eq!(s.id, None, "a wall click drops the dimension selection");
        ed.handle(Event::Click(p));
        assert_eq!(ed.selection, vec![Sel::Wall(w1)]);
        // and the dimension line away from the walls still selects the dimension
        let away = element_at(&ed, v2(2.0, 2.0), r, |_| true);
        assert!(s.click(&mut ed, v2(2.0, 2.0), r, away));
        assert_eq!(s.id, Some(id));
        assert!(ed.selection.is_empty());
    }
}
