// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Underlay drawings (.dxf): placement on a level, layers (visibility, freeze, color, halftone),
//! plan rendering, snaps, tracing a line with a wall, and "drawing to BIM" recognition with
//! preview and acceptance as a single operation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{
    self as dm, AcceptOptions, BlockRules, Memory, Pick, Proposals, ProposeOptions, Underlay,
};
use ddc_io::dxf::DxfDocument;
use egui::{self, Color32, Painter, Pos2, Rect, Stroke};

use crate::editor::{Editor, OpFn};
use crate::selection::BoxMode;

pub mod detail;
pub mod edit;
pub mod history;
pub mod store;
pub mod tess;
pub mod xform;
use history::Step;

/// Where to put the drawing origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// File coordinates as they are.
    AsIs,
    /// The bounding box's lower-left corner goes to (0, 0).
    MinToZero,
    /// Lower-left corner goes to the first drawing's corner (floors stacked).
    AlignFirst,
}

impl Origin {
    pub const ALL: [Origin; 3] = [Origin::AsIs, Origin::MinToZero, Origin::AlignFirst];
    pub fn label(self) -> String {
        let k = match self {
            Origin::AsIs => "dlg.dw.origin.as_is",
            Origin::MinToZero => "dlg.dw.origin.min_zero",
            Origin::AlignFirst => "dlg.dw.origin.first",
        };
        crate::tr(k, &[])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerState {
    pub name: String,
    pub visible: bool,
    pub frozen: bool,
    /// Layer color in the file.
    pub file_color: Option<[u8; 3]>,
    /// User-defined color.
    pub color: Option<[u8; 3]>,
    pub halftone: bool,
    /// Number of objects in the layer.
    pub count: usize,
}

impl LayerState {
    pub fn shown(&self) -> bool {
        self.visible && !self.frozen
    }
}

pub struct Drawing {
    /// Stable number within the session: recognition and selection indices are restored by it.
    pub id: u64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub level: i64,
    /// Geometry in model meters (the shift is already applied).
    pub u: Underlay,
    pub unit: f64,
    /// File to model: model point = R(rot)·file point (m) + offset.
    pub offset: V2,
    pub rot: f64,
    pub layers: Vec<LayerState>,
    pub visible: bool,
    pub halftone: bool,
    idx: HashMap<String, usize>,
    /// Layer chosen for drawing into this drawing; see [`Drawing::current_layer`].
    pub cur_layer: String,
    /// Edited since it was last saved: `gen != saved_gen`, kept in step by [`Drawing::mark_edited`],
    /// undo/redo and [`Drawings::mark_saved`].
    pub dirty: bool,
    /// Edit generation of the live state. Every edit takes a fresh value, undo and redo
    /// restore the one their state had, so equal generations mean equal content.
    pub(crate) gen: u64,
    /// Generation last written to a file (the opened state counts as saved).
    pub(crate) saved_gen: u64,
    next_gen: u64,
    /// Display state of layers [`Drawing::sync_layers`] dropped, by name, for their return.
    pub(crate) parked: HashMap<String, LayerState>,
}

impl Drawing {
    fn layer(&self, name: &str) -> Option<&LayerState> {
        self.idx.get(name).map(|&k| &self.layers[k])
    }
    fn shown(&self, name: &str) -> bool {
        self.layer(name).is_none_or(|l| l.shown())
    }

    /// Underlay of visible layers only (for recognition).
    pub fn visible_underlay(&self) -> Underlay {
        let mut u = self.u.clone();
        let on = |l: &str| self.shown(l);
        u.lines.retain(|x| on(&x.layer));
        u.arcs.retain(|x| on(&x.layer));
        u.texts.retain(|x| on(&x.layer));
        u.closed.retain(|x| on(&x.layer));
        u.fills.retain(|x| on(&x.layer));
        u.inserts.retain(|x| on(&x.layer));
        u.block_hints.retain(|x| on(&x.layer));
        u
    }

    pub fn bbox(&self) -> Option<(V2, V2)> {
        self.u.bbox().map(|(a, b)| (v2(a.0, a.1), v2(b.0, b.1)))
    }

    /// Extents for fitting the view: stray far points are ignored.
    pub fn fit_bbox(&self) -> Option<(V2, V2)> {
        robust_bbox(&self.u)
    }
}

/// Drawings whose centre is farther than this from the origin (m) are offered a move to it.
pub const FAR_M: f64 = 500.0;

fn sample_pts(u: &Underlay) -> Vec<V2> {
    let mut v = Vec::with_capacity(u.lines.len() * 2 + u.arcs.len() + u.texts.len());
    for l in &u.lines {
        v.push(v2(l.a.0, l.a.1));
        v.push(v2(l.b.0, l.b.1));
    }
    v.extend(u.arcs.iter().map(|a| v2(a.c.0, a.c.1)));
    v.extend(u.texts.iter().map(|t| v2(t.pos.0, t.pos.1)));
    v
}

/// Full extents, or the 1..99 % extents (padded) when a few stray points stretch the full ones
/// to over 20 times the body of the drawing.
pub fn robust_bbox(u: &Underlay) -> Option<(V2, V2)> {
    let (a, b) = u.bbox()?;
    let full = (v2(a.0, a.1), v2(b.0, b.1));
    let pts = sample_pts(u);
    if pts.len() < 50 {
        return Some(full);
    }
    let Some((lo, hi)) = crate::open::first_view::percentile_bounds(&pts) else {
        return Some(full);
    };
    let (fw, fh) = (full.1.x - full.0.x, full.1.y - full.0.y);
    let (rw, rh) = ((hi.x - lo.x).max(1e-6), (hi.y - lo.y).max(1e-6));
    if fw > 20.0 * rw || fh > 20.0 * rh {
        let pad = v2(rw * 0.02, rh * 0.02);
        Some((lo - pad, hi + pad))
    } else {
        Some(full)
    }
}

/// Shift (m) that brings a far-away drawing's corner to the origin; None when it is near.
pub fn far_shift(u: &Underlay) -> Option<V2> {
    let (lo, hi) = robust_bbox(u)?;
    let c = (lo + hi) * 0.5;
    (c.x.abs().max(c.y.abs()) > FAR_M).then(|| v2(-lo.x, -lo.y))
}

/// Extra placement offset: moves a far drawing to the origin (kept as the placement offset)
/// when `near` is on and the origin choice is "as in the file".
pub fn placement_extra(u: &Underlay, origin: Origin, near: bool, extra: V2) -> V2 {
    match (origin, near, far_shift(u)) {
        (Origin::AsIs, true, Some(s)) => extra + s,
        _ => extra,
    }
}

/// Wall side relative to the traced line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// On the cursor side.
    Cursor,
    /// Axis on the line.
    Center,
    Left,
    Right,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Cursor, Side::Center, Side::Left, Side::Right];
    pub fn label(self) -> String {
        let k = match self {
            Side::Cursor => "dlg.dw.side.cursor",
            Side::Center => "dlg.dw.side.center",
            Side::Left => "dlg.dw.side.left",
            Side::Right => "dlg.dw.side.right",
        };
        crate::tr(k, &[])
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trace {
    pub side: Side,
    /// Extra offset from the line, m.
    pub offset: f64,
}

/// Classes of recognized elements.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Walls,
    Doors,
    Windows,
    Rooms,
    Grids,
    Columns,
}

impl Class {
    pub const ALL: [Class; 6] = [
        Class::Walls,
        Class::Doors,
        Class::Windows,
        Class::Rooms,
        Class::Grids,
        Class::Columns,
    ];
    pub fn label(self) -> String {
        let k = match self {
            Class::Walls => "dlg.dw.class.walls",
            Class::Doors => "dlg.dw.class.doors",
            Class::Windows => "dlg.dw.class.windows",
            Class::Rooms => "dlg.dw.class.rooms",
            Class::Grids => "dlg.dw.class.grids",
            Class::Columns => "dlg.dw.class.columns",
        };
        crate::tr(k, &[])
    }
}

pub struct Recog {
    /// Index in `Drawings::list`; recomputed by `Drawing::id` after a drawing is removed.
    pub drawing: usize,
    pub props: Proposals,
    /// Ids to accept.
    pub on: HashSet<u64>,
}

impl Recog {
    pub fn ids(&self, c: Class) -> Vec<u64> {
        let p = &self.props;
        match c {
            Class::Walls => p.walls.iter().map(|x| x.id).collect(),
            Class::Doors => p
                .openings
                .iter()
                .filter(|o| o.kind == "door")
                .map(|x| x.id)
                .collect(),
            Class::Windows => p
                .openings
                .iter()
                .filter(|o| o.kind != "door")
                .map(|x| x.id)
                .collect(),
            Class::Rooms => p.rooms.iter().map(|x| x.id).collect(),
            Class::Grids => p.grids.iter().map(|x| x.id).collect(),
            Class::Columns => p.columns.iter().map(|x| x.id).collect(),
        }
    }
    pub fn count_on(&self, c: Class) -> (usize, usize) {
        let ids = self.ids(c);
        (
            ids.iter().filter(|i| self.on.contains(i)).count(),
            ids.len(),
        )
    }
    pub fn set_class(&mut self, c: Class, on: bool) {
        for id in self.ids(c) {
            if on {
                self.on.insert(id);
            } else {
                self.on.remove(&id);
            }
        }
    }
    pub fn toggle(&mut self, id: u64) {
        if !self.on.remove(&id) {
            self.on.insert(id);
        }
    }
    /// Accepted ones: an opening without its own wall is not accepted.
    pub fn chosen(&self) -> Vec<u64> {
        let walls: HashSet<u64> = self
            .props
            .walls
            .iter()
            .map(|w| w.id)
            .filter(|i| self.on.contains(i))
            .collect();
        self.props
            .all_ids()
            .into_iter()
            .filter(|i| self.on.contains(i))
            .filter(|i| {
                self.props
                    .openings
                    .iter()
                    .find(|o| o.id == *i)
                    .is_none_or(|o| walls.contains(&o.wall))
            })
            .collect()
    }

    /// Proposal under a point (m): id and a short description.
    pub fn hit(&self, p: V2, tol: f64) -> Option<(u64, String)> {
        let pr = &self.props;
        let wall_at = |id: u64| pr.walls.iter().find(|w| w.id == id);
        for o in &pr.openings {
            let Some(w) = wall_at(o.wall) else { continue };
            let (a, b) = (v2(w.a.0, w.a.1), v2(w.b.0, w.b.1));
            let d = (b - a) * (1.0 / (b - a).len().max(1e-9));
            let (s, e) = (a + d * o.along, a + d * (o.along + o.width));
            if seg_dist(p, s, e) <= w.thickness / 2.0 + tol {
                return Some((
                    o.id,
                    crate::tr(
                        if o.kind == "door" {
                            "dlg.dw.st.hit_door"
                        } else {
                            "dlg.dw.st.hit_window"
                        },
                        &[("mm", &mm(o.width))],
                    ),
                ));
            }
        }
        for c in &pr.columns {
            if (p - v2(c.at.0, c.at.1)).len() <= c.b.max(c.h) / 2.0 + tol {
                return Some((
                    c.id,
                    crate::tr(
                        "dlg.dw.st.hit_column",
                        &[("b", &mm(c.b)), ("h", &mm(c.h))],
                    ),
                ));
            }
        }
        for w in &pr.walls {
            if seg_dist(p, v2(w.a.0, w.a.1), v2(w.b.0, w.b.1)) <= w.thickness / 2.0 + tol {
                let mm = mm(w.thickness);
                return Some((w.id, crate::tr("dlg.dw.st.hit_wall", &[("mm", &mm)])));
            }
        }
        for g in &pr.grids {
            if seg_dist(p, v2(g.a.0, g.a.1), v2(g.b.0, g.b.1)) <= tol {
                return Some((g.id, crate::tr("dlg.dw.st.hit_grid", &[("name", &g.name)])));
            }
        }
        for r in &pr.rooms {
            if (p - v2(r.point.0, r.point.1)).len() <= 0.6 + tol {
                return Some((r.id, crate::tr("dlg.dw.st.hit_room", &[("name", &r.name)])));
            }
        }
        None
    }
}

/// Metres as whole millimetres, for labels.
fn mm(v: f64) -> String {
    format!("{:.0}", v * 1000.0)
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 < 1e-18 {
        0.0
    } else {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    };
    (p - (a + d * t)).len()
}

type P2 = (f64, f64);

/// Closed-rectangle test of a segment (Liang-Barsky, touching counts).
fn seg_hits_rect(a: P2, b: P2, lo: P2, hi: P2) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a.0 - lo.0), (dx, hi.0 - a.0), (-dy, a.1 - lo.1), (dy, hi.1 - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return false;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return false;
                }
                t1 = t1.min(r);
            }
        }
    }
    true
}

fn in_rect(p: P2, lo: P2, hi: P2) -> bool {
    p.0 >= lo.0 && p.0 <= hi.0 && p.1 >= lo.1 && p.1 <= hi.1
}

/// Even-odd containment over all rings.
fn in_rings(p: P2, rings: &[&[P2]]) -> bool {
    let mut c = false;
    for r in rings {
        let n = r.len();
        for i in 0..n {
            let (a, b) = (r[i], r[(i + n - 1) % n]);
            if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
                c = !c;
            }
        }
    }
    c
}

/// Rectangle touches the closed rings (outline or, when `fill`, interior).
fn rings_touch_rect(rings: &[&[P2]], fill: bool, lo: P2, hi: P2) -> bool {
    for r in rings {
        let n = r.len();
        if r.iter().any(|&p| in_rect(p, lo, hi))
            || (0..n).any(|i| seg_hits_rect(r[i], r[(i + 1) % n], lo, hi))
        {
            return true;
        }
    }
    fill && in_rings(lo, rings)
}

/// Counter-clockwise sweep of an arc, 0..=TAU.
fn arc_sweep(a0: f64, a1: f64) -> f64 {
    let s = a1 - a0;
    if s < 0.0 {
        s.rem_euclid(std::f64::consts::TAU)
    } else {
        s.min(std::f64::consts::TAU)
    }
}

fn arc_has(a0: f64, sweep: f64, ang: f64) -> bool {
    (ang - a0).rem_euclid(std::f64::consts::TAU) <= sweep + 1e-12
}

/// Axis-aligned bounds of an arc (end points and the extreme points inside the sweep).
fn arc_bounds(c: P2, r: f64, a0: f64, a1: f64) -> (P2, P2) {
    let s = arc_sweep(a0, a1);
    let at = |t: f64| (c.0 + r * t.cos(), c.1 + r * t.sin());
    let (p, q) = (at(a0), at(a0 + s));
    let (mut lo, mut hi) = ((p.0.min(q.0), p.1.min(q.1)), (p.0.max(q.0), p.1.max(q.1)));
    for k in 0..4 {
        let t = k as f64 * std::f64::consts::FRAC_PI_2;
        if arc_has(a0, s, t) {
            let m = at(t);
            lo = (lo.0.min(m.0), lo.1.min(m.1));
            hi = (hi.0.max(m.0), hi.1.max(m.1));
        }
    }
    (lo, hi)
}

fn arc_touches_rect(c: P2, r: f64, a0: f64, a1: f64, lo: P2, hi: P2) -> bool {
    let (blo, bhi) = arc_bounds(c, r, a0, a1);
    if blo.0 > hi.0 || bhi.0 < lo.0 || blo.1 > hi.1 || bhi.1 < lo.1 {
        return false;
    }
    let s = arc_sweep(a0, a1);
    let at = |t: f64| (c.0 + r * t.cos(), c.1 + r * t.sin());
    if in_rect(at(a0), lo, hi) || in_rect(at(a0 + s), lo, hi) {
        return true;
    }
    // the circle against each rectangle side; the crossing must lie on the swept part
    for (x, ylo, yhi) in [(lo.0, lo.1, hi.1), (hi.0, lo.1, hi.1)] {
        let dx = x - c.0;
        if dx.abs() <= r {
            let h = (r * r - dx * dx).max(0.0).sqrt();
            for y in [c.1 - h, c.1 + h] {
                if y >= ylo && y <= yhi && arc_has(a0, s, (y - c.1).atan2(dx)) {
                    return true;
                }
            }
        }
    }
    for (y, xlo, xhi) in [(lo.1, lo.0, hi.0), (hi.1, lo.0, hi.0)] {
        let dy = y - c.1;
        if dy.abs() <= r {
            let h = (r * r - dy * dy).max(0.0).sqrt();
            for x in [c.0 - h, c.0 + h] {
                if x >= xlo && x <= xhi && arc_has(a0, s, dy.atan2(x - c.0)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Text box: baseline start `pos`, width by an average glyph advance, height `height`.
fn text_quad(t: &dm::UText) -> [P2; 4] {
    let w = t.text.trim().chars().count().max(1) as f64 * 0.6 * t.height;
    let (sn, cs) = t.rotation.sin_cos();
    let (ux, uy, vx, vy) = (cs * w, sn * w, -sn * t.height, cs * t.height);
    let p = t.pos;
    [p, (p.0 + ux, p.1 + uy), (p.0 + ux + vx, p.1 + uy + vy), (p.0 + vx, p.1 + vy)]
}

impl Drawing {
    /// Visible primitives inside (window) or touching (crossing) the box `a`-`b`.
    /// One pass; the cheap geometry test goes before the layer lookup.
    fn box_set(&self, a: V2, b: V2, mode: BoxMode) -> Pick {
        let (lo, hi) = ((a.x.min(b.x), a.y.min(b.y)), (a.x.max(b.x), a.y.max(b.y)));
        let win = mode == BoxMode::Window;
        let u = &self.u;
        let mut pk = Pick::default();
        for (i, l) in u.lines.iter().enumerate() {
            let hit = if win {
                in_rect(l.a, lo, hi) && in_rect(l.b, lo, hi)
            } else {
                seg_hits_rect(l.a, l.b, lo, hi)
            };
            if hit && self.shown(&l.layer) {
                pk.lines.push(i);
            }
        }
        for (i, r) in u.arcs.iter().enumerate() {
            if !(r.r.is_finite() && r.c.0.is_finite() && r.c.1.is_finite()) {
                continue;
            }
            let hit = if win {
                let (blo, bhi) = arc_bounds(r.c, r.r.abs(), r.a0, r.a1);
                in_rect(blo, lo, hi) && in_rect(bhi, lo, hi)
            } else {
                arc_touches_rect(r.c, r.r.abs(), r.a0, r.a1, lo, hi)
            };
            if hit && self.shown(&r.layer) {
                pk.arcs.push(i);
            }
        }
        for (i, t) in u.texts.iter().enumerate() {
            let q = text_quad(t);
            let hit = if win {
                q.iter().all(|&p| in_rect(p, lo, hi))
            } else {
                rings_touch_rect(&[&q[..]], true, lo, hi)
            };
            if hit && self.shown(&t.layer) {
                pk.texts.push(i);
            }
        }
        for (i, c) in u.closed.iter().enumerate() {
            if c.pts.is_empty() {
                continue;
            }
            let hit = if win {
                c.pts.iter().all(|&p| in_rect(p, lo, hi))
            } else {
                rings_touch_rect(&[c.pts.as_slice()], false, lo, hi)
            };
            if hit && self.shown(&c.layer) {
                pk.closed.push(i);
            }
        }
        for (i, f) in u.fills.iter().enumerate() {
            let rings: Vec<&[P2]> = f.loops.iter().map(|l| l.as_slice()).filter(|l| !l.is_empty()).collect();
            if rings.is_empty() {
                continue;
            }
            let hit = if win {
                rings.iter().all(|r| r.iter().all(|&p| in_rect(p, lo, hi)))
            } else {
                rings_touch_rect(&rings, true, lo, hi)
            };
            if hit && self.shown(&f.layer) {
                pk.fills.push(i);
            }
        }
        pk
    }
}

#[derive(Default)]
pub struct Drawings {
    pub list: Vec<Drawing>,
    pub sel: usize,
    pub trace: Option<Trace>,
    pub rules: BlockRules,
    pub recog: Option<Recog>,
    /// Windows: import, layers, recognition.
    pub import_open: bool,
    pub layers_open: bool,
    pub recog_open: bool,
    /// Click-selection mode for underlay primitives.
    pub pick_mode: bool,
    /// Selected primitives: (drawing, indices).
    pub pick: Option<(usize, Pick)>,
    /// Screen start of a box drag in pick mode.
    pub box_from: Option<Pos2>,
    /// Shift X, Y (m) and rotation (°) for "Move / Rotate / Copy".
    pub xf: (f64, f64, f64),
    /// Grows on any change (snap cache).
    pub version: u64,
    /// Edit tools: field buffers and the draw mode.
    pub edit: edit::EditUi,
    next_id: u64,
    /// Finished edits waiting for the editor's undo stack: see [`Drawings::sync_undo`].
    steps: Vec<Step>,
    /// The tool under which tracing/selection were enabled: switching tools turns them off.
    tool_sig: Option<ToolSig>,
    cache: std::cell::RefCell<Option<SnapCache>>,
    /// Tessellated underlays per (drawing, version, zoom bucket): pan and idle frames reuse them.
    tess: std::cell::RefCell<tess::Cache>,
    /// Copy kept in the project file: see [`store`].
    store: store::Stored,
}

/// Snap lines of a level with the layer id of each line (parallel vectors), by version.
type SnapCache = (i64, u64, Arc<Vec<(V2, V2)>>, Arc<Vec<u32>>);

/// Fingerprint of the active plan tool.
pub type ToolSig = (crate::wall_tool::Mode, bool, bool, bool);

pub fn tool_sig(ed: &Editor) -> ToolSig {
    (
        ed.wall_mode(),
        ed.opening_mode,
        ed.tool.curtain,
        ed.section_tool().is_some(),
    )
}

/// Tracing builds walls, so it works only with the Wall tool (outline drawing).
pub fn wall_tool_active(ed: &Editor) -> bool {
    !ed.opening_mode
        && !ed.tool.curtain
        && ed.section_tool().is_none()
        && ed.tool_armed()
        && !crate::annot_text::armed()
}

/// A complete rules file ends with a newline and every data line parses.
fn rules_text_ok(t: &str) -> bool {
    let data = t.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).count();
    t.ends_with('\n') && BlockRules::from_text(t).0.len() == data
}

/// The "block to family" rules file next to the settings.
fn rules_path() -> Option<PathBuf> {
    crate::gfx::data_dir().map(|d| d.join("block_rules.tsv"))
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn dwg_unavailable() -> String {
    crate::tr("dlg.dw.st.dwg_unavailable", &[])
}

/// Read a .dxf/.dwg drawing: the reader detects the format from the content.
pub fn read_drawing(path: &Path) -> Result<DxfDocument, String> {
    read_drawing_with(path, &|| false)
}

/// [`read_drawing`] with a stop check: a stop gives the error text `ddc_io::job::CANCELLED`.
pub fn read_drawing_with(path: &Path, stop: &dyn Fn() -> bool) -> Result<DxfDocument, String> {
    match ext_of(path).as_str() {
        "dxf" | "dwg" => {
            if !ddc_io::dxf::DWG_SUPPORTED && ext_of(path) == "dwg" {
                let head = std::fs::read(path).map_err(|e| e.to_string())?;
                if ddc_io::dxf::looks_like_dwg(&head) {
                    return Err(dwg_unavailable());
                }
            }
            ddc_io::dxf::read_file_with(path, stop).map_err(|e| e.to_string())
        }
        e => Err(crate::tr("dlg.dw.st.unknown_format", &[("ext", e)])),
    }
}

/// Underlay from a file (.dxf/.dwg/.pdf) and meters per unit. `unit` None means from the file;
/// `pdf_ratio` is the PDF sheet scale 1:N, `page` the page number.
pub fn read_underlay(
    path: &Path,
    unit: Option<f64>,
    pdf_ratio: f64,
    page: usize,
) -> Result<(Underlay, f64), String> {
    read_underlay_with(path, unit, pdf_ratio, page, &|| false)
}

/// [`read_underlay`] with a stop check (DXF and DWG poll it while reading).
pub fn read_underlay_with(
    path: &Path,
    unit: Option<f64>,
    pdf_ratio: f64,
    page: usize,
    stop: &dyn Fn() -> bool,
) -> Result<(Underlay, f64), String> {
    if ext_of(path) == "pdf" {
        use ddc_io::pdf::import::{ImportOptions, PdfFile, Scale};
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let f = PdfFile::parse(&bytes).map_err(|e| e.to_string())?;
        let pg = f
            .page(page.min(f.page_count().saturating_sub(1)))
            .map_err(|e| e.to_string())?;
        let opt = ImportOptions {
            scale: Scale::Ratio(pdf_ratio.max(1e-6)),
            ..Default::default()
        };
        let u = Underlay::from_pdf(&pg, &opt).map_err(|e| e.to_string())?;
        return Ok((u, 0.001 * pdf_ratio));
    }
    let doc = read_drawing_with(path, stop)?;
    let unit = unit.unwrap_or_else(|| dm::guess_unit(&doc));
    Ok((Underlay::from_dxf(&doc, unit), unit))
}

impl Drawings {
    /// An underlay is placed or the recognition panel holds proposals: the welcome card must
    /// not cover the canvas then.
    pub fn has_pending(&self) -> bool {
        !self.list.is_empty()
            || self
                .recog
                .as_ref()
                .is_some_and(|r| Class::ALL.iter().any(|c| !r.ids(*c).is_empty()))
    }

    pub fn new() -> Self {
        let rules = rules_path()
            .and_then(|p| crate::safe_file::load_text(&p, &rules_text_ok))
            .map(|t| BlockRules::from_text(&t))
            .filter(|r| !r.0.is_empty())
            .unwrap_or_default();
        Drawings {
            rules,
            ..Default::default()
        }
    }

    pub fn save_rules(&self) -> Result<(), String> {
        let p = rules_path().ok_or_else(|| crate::tr("dlg.dw.st.no_settings_dir", &[]))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut t = self.rules.to_text();
        if t.is_empty() {
            t.push_str("# no rules\n"); // an empty file would look like a torn write
        }
        crate::safe_file::write_atomic(&p, t.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn touch(&mut self) {
        self.version += 1;
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Remove drawing `k`: recognition, selection and the current drawing are carried over by id;
    /// those that belonged to the removed one are reset.
    pub fn remove(&mut self, k: usize) {
        if let Some(d) = self.take(k) {
            self.steps.push(Step::removed(d, k));
        }
    }

    /// The removal itself, with no undo step: undo and redo use it too.
    fn take(&mut self, k: usize) -> Option<Drawing> {
        if k >= self.list.len() {
            return None;
        }
        let id_at = |s: &Self, i: usize| s.list.get(i).map(|d| d.id);
        let rec = self.recog.as_ref().and_then(|r| id_at(self, r.drawing));
        let pick = self.pick.as_ref().and_then(|p| id_at(self, p.0));
        let sel = id_at(self, self.sel);
        let out = self.list.remove(k);
        let gone = out.id;
        let pos = |s: &Self, id: Option<u64>| {
            id.filter(|i| *i != gone)
                .and_then(|i| s.list.iter().position(|d| d.id == i))
        };
        let (rec, pick, sel) = (pos(self, rec), pos(self, pick), pos(self, sel));
        match rec {
            Some(i) => {
                if let Some(r) = self.recog.as_mut() {
                    r.drawing = i;
                }
            }
            None => self.recog = None,
        }
        match pick {
            Some(i) => {
                if let Some(p) = self.pick.as_mut() {
                    p.0 = i;
                }
            }
            None => self.pick = None,
        }
        self.sel = sel.unwrap_or(0);
        self.touch();
        Some(out)
    }

    /// Recognition proposal under a point, only if its drawing is on `level`.
    pub fn recog_hit(&self, level: i64, p: V2, tol: f64) -> Option<(u64, String)> {
        let rc = self.recog.as_ref()?;
        if self.list.get(rc.drawing).is_none_or(|d| d.level != level) {
            return None;
        }
        rc.hit(p, tol)
    }

    /// Turn off tracing and primitive selection.
    pub fn exit_modes(&mut self) {
        self.trace = None;
        self.pick_mode = false;
        self.pick = None;
        self.edit.draw = None;
        self.tool_sig = None;
    }

    /// Once per frame: if the layers window is closed or the tool changed, tracing and selection
    /// are turned off (returns a message for the status line).
    pub fn sync_tool(&mut self, sig: ToolSig) -> Option<String> {
        if self.trace.is_none() && !self.pick_mode && self.edit.draw.is_none() {
            self.tool_sig = None;
            return None;
        }
        if !self.layers_open {
            self.exit_modes();
            return Some(crate::tr("dlg.dw.st.modes_off_closed", &[]));
        }
        match self.tool_sig {
            None => {
                self.tool_sig = Some(sig);
                None
            }
            Some(s) if s != sig => {
                self.exit_modes();
                Some(crate::tr("dlg.dw.st.modes_off_tool", &[]))
            }
            Some(_) => None,
        }
    }

    /// Mode enabled in the layers window: the tool is remembered afresh on the next frame.
    pub fn rearm_tool(&mut self) {
        self.tool_sig = None;
    }

    pub fn on_level(&self, level: i64) -> impl Iterator<Item = (usize, &Drawing)> {
        self.list
            .iter()
            .enumerate()
            .filter(move |(_, d)| d.level == level && d.visible)
    }

    /// Place a document on a level. `unit` is meters per unit (None: from the file).
    pub fn add(
        &mut self,
        doc: &DxfDocument,
        name: &str,
        path: Option<PathBuf>,
        level: i64,
        unit: Option<f64>,
        origin: Origin,
        extra: V2,
    ) -> String {
        let unit = unit.unwrap_or_else(|| dm::guess_unit(doc));
        self.add_underlay(
            Underlay::from_dxf(doc, unit),
            unit,
            name,
            path,
            level,
            origin,
            extra,
        )
    }

    /// Place a ready underlay (m) on a level.
    #[allow(clippy::too_many_arguments)]
    pub fn add_underlay(
        &mut self,
        mut u: Underlay,
        unit: f64,
        name: &str,
        path: Option<PathBuf>,
        level: i64,
        origin: Origin,
        extra: V2,
    ) -> String {
        let lo = u.bbox().map_or(v2(0.0, 0.0), |(a, _)| v2(a.0, a.1));
        let base = match origin {
            Origin::AsIs => v2(0.0, 0.0),
            Origin::MinToZero => v2(-lo.x, -lo.y),
            Origin::AlignFirst => match self.list.first().and_then(|d| d.bbox()) {
                Some((first_lo, _)) => first_lo - lo,
                None => v2(-lo.x, -lo.y),
            },
        };
        let offset = base + extra;
        u.translate((offset.x, offset.y));
        canon_layers(&mut u);
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
        let layers: Vec<LayerState> = u
            .layers
            .iter()
            .map(|l| LayerState {
                name: l.name.clone(),
                visible: !l.off,
                frozen: l.frozen,
                file_color: l.color,
                color: None,
                halftone: false,
                count: counts.get(l.name.as_str()).copied().unwrap_or(0),
            })
            .filter(|l| l.count > 0 || l.name != "0")
            .collect();
        let idx = layers
            .iter()
            .enumerate()
            .map(|(k, l)| (l.name.clone(), k))
            .collect();
        let mut summary = crate::tr(
            "dlg.dw.summary",
            &[
                ("name", name),
                ("lines", &u.lines.len().to_string()),
                ("arcs", &u.arcs.len().to_string()),
                ("texts", &u.texts.len().to_string()),
                ("fills", &u.fills.len().to_string()),
                ("layers", &layers.len().to_string()),
                ("unit", &(unit * 1000.0).to_string()),
            ],
        );
        if u.paper_skipped > 0 {
            let n = u.layouts.len().saturating_sub(1).to_string();
            summary += &crate::tr("dlg.dw.summary_paper", &[("n", &n)]);
        }
        if !u.unknown.is_empty() {
            let mut v: Vec<(&String, &usize)> = u.unknown.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let list: Vec<String> = v.iter().take(8).map(|(k, n)| format!("{k} ×{n}")).collect();
            summary += &crate::tr("dlg.dw.summary_unknown", &[("list", &list.join(", "))]);
            if v.len() > 8 {
                let more = (v.len() - 8).to_string();
                summary += &crate::tr("dlg.dw.summary_more", &[("n", &more)]);
            }
        }
        let id = self.new_id();
        self.list.push(Drawing {
            id,
            name: name.into(),
            path,
            level,
            u,
            unit,
            offset,
            rot: 0.0,
            layers,
            visible: true,
            halftone: false,
            idx,
            cur_layer: String::new(),
            dirty: false,
            gen: 0,
            saved_gen: 0,
            next_gen: 0,
            parked: HashMap::new(),
        });
        self.sel = self.list.len() - 1;
        self.touch();
        summary
    }

    /// Segments of visible layers on a level (for snaps), cached by version.
    pub fn snap_segs(&self, level: i64) -> Arc<Vec<(V2, V2)>> {
        self.snap_data(level).0
    }

    /// Layer id of each line of [`Drawings::snap_segs`] (same order; one id per drawing layer).
    pub fn snap_layers(&self, level: i64) -> Arc<Vec<u32>> {
        self.snap_data(level).1
    }

    fn snap_data(&self, level: i64) -> (Arc<Vec<(V2, V2)>>, Arc<Vec<u32>>) {
        if let Some((l, v, s, ly)) = self.cache.borrow().as_ref() {
            if *l == level && *v == self.version {
                return (s.clone(), ly.clone());
            }
        }
        let mut out = vec![];
        let mut ids = vec![];
        let mut table: std::collections::HashMap<(u64, &str), u32> = Default::default();
        for (_, d) in self.on_level(level) {
            for l in d.u.lines.iter().filter(|l| d.shown(&l.layer)) {
                out.push((v2(l.a.0, l.a.1), v2(l.b.0, l.b.1)));
                let n = table.len() as u32;
                ids.push(*table.entry((d.id, l.layer.as_str())).or_insert(n));
            }
            for a in d.u.arcs.iter().filter(|a| d.shown(&a.layer)) {
                let pts = arc_pts(a.c, a.r, a.a0, a.a1, 16);
                let n = table.len() as u32;
                let id = *table.entry((d.id, a.layer.as_str())).or_insert(n);
                out.extend(pts.windows(2).map(|w| (w[0], w[1])));
                ids.extend(pts.windows(2).map(|_| id));
            }
        }
        let (s, ly) = (Arc::new(out), Arc::new(ids));
        *self.cache.borrow_mut() = Some((level, self.version, s.clone(), ly.clone()));
        (s, ly)
    }

    /// Nearest visible line to a point.
    pub fn pick_line(&self, level: i64, p: V2, tol: f64) -> Option<(V2, V2)> {
        let mut best: Option<(f64, (V2, V2))> = None;
        for (_, d) in self.on_level(level) {
            for l in d.u.lines.iter().filter(|l| d.shown(&l.layer)) {
                let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
                let dd = seg_dist(p, a, b);
                if dd <= tol && best.is_none_or(|x| dd < x.0) {
                    best = Some((dd, (a, b)));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Tracing: a wall of the current type along the line under the cursor. `true` if the click was handled.
    pub fn trace_click(&mut self, ed: &mut Editor, p: V2, tol: f64) -> bool {
        let Some(tr) = self.trace else { return false };
        let Some((a, b)) = self.pick_line(ed.level, p, tol) else {
            ed.status = Some(crate::tr("dlg.dw.st.trace_no_line", &[]));
            return true;
        };
        let half = ed.current_type().map_or(0.1, |t| t.thickness() / 2.0);
        let (a, b) = trace_axis(a, b, p, half, tr);
        ed.commit_walls(vec![(a, b)]);
        true
    }

    /// Recognize the selected drawing (visible layers).
    pub fn recognize(&mut self, ed: &Editor) -> String {
        let Some(d) = self.list.get(self.sel) else {
            return crate::tr("dlg.dw.st.no_drawing", &[]);
        };
        let types = dm::wall_types(&ed.model).unwrap_or_default();
        let lang = crate::doc_lang::doc_lang(&ed.model).code().to_string();
        // untouched default rules name their families in the document language
        let rules = if self.rules == BlockRules::default() {
            BlockRules::for_lang(&lang)
        } else {
            self.rules.clone()
        };
        let opt = ProposeOptions {
            rules,
            lang,
            ..Default::default()
        };
        let mut props = dm::propose(&d.visible_underlay(), &types, &opt, &Memory::default());
        for w in &mut props.walls {
            w.height = ed.height;
        }
        let on: HashSet<u64> = props
            .walls
            .iter()
            .map(|w| (w.id, w.confidence))
            .chain(props.openings.iter().map(|o| (o.id, o.confidence)))
            .chain(props.rooms.iter().map(|r| (r.id, r.confidence)))
            .chain(props.grids.iter().map(|g| (g.id, g.confidence)))
            .chain(props.columns.iter().map(|c| (c.id, c.confidence)))
            .filter(|(_, c)| *c >= 0.5)
            .map(|(i, _)| i)
            .collect();
        let r = &props.report;
        let mut msg = crate::tr(
            "dlg.dw.st.recognized",
            &[
                ("walls", &props.walls.len().to_string()),
                ("doors", &r.doors.to_string()),
                ("windows", &r.windows.to_string()),
                ("rooms", &props.rooms.len().to_string()),
                ("grids", &props.grids.len().to_string()),
                ("columns", &props.columns.len().to_string()),
            ],
        );
        if let Some(note) = unreadable_note(r, crate::ui_lang_now()) {
            msg = format!("{msg}; {note}");
        }
        self.recog = Some(Recog {
            drawing: self.sel,
            props,
            on,
        });
        msg
    }

    /// Create the accepted elements on the drawing's level: one operation, one undo step.
    pub fn accept(&mut self, ed: &mut Editor) -> Result<String, String> {
        let Some(rc) = &self.recog else {
            return Err(crate::tr("dlg.dw.st.nothing_to_accept", &[]));
        };
        let Some(d) = self.list.get(rc.drawing) else {
            return Err(crate::tr("dlg.dw.st.drawing_gone", &[]));
        };
        let ids = rc.chosen();
        if ids.is_empty() {
            return Err(crate::tr("dlg.dw.st.nothing_picked", &[]));
        }
        let level = d.level;
        let mut levels = ed.levels();
        levels.sort_by(|a, b| a.2.total_cmp(&b.2));
        let elev = levels.iter().find(|l| l.0 == level).map_or(0.0, |l| l.2);
        let mut opt = AcceptOptions::new(level);
        opt.lang = crate::doc_lang::doc_lang(&ed.model).code().to_string();
        opt.room_height = ed.height;
        opt.column_height = ed.height;
        opt.top_level = levels.iter().find(|l| l.2 > elev + 1e-6).map(|l| l.0);
        let props = Arc::new(rc.props.clone());
        let n = ids.len();
        let ids = Arc::new(ids);
        let warnings: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let w = warnings.clone();
        let f = OpFn(Arc::new(move |m: &mut ddc_model::Model| {
            let mut mem = Memory::default();
            let r = dm::accept_proposals(m, &props, &ids, &opt, &mut mem)?;
            if let Ok(mut g) = w.lock() {
                *g = r.warnings;
            }
            Ok(())
        }));
        // the result is shown on the drawing's level; on error the level is restored
        let prev = ed.level;
        if level != prev {
            ed.set_level(level);
        }
        if let Err(e) = ed.run_op(f) {
            if level != prev {
                ed.set_level(prev);
            }
            return Err(e.to_string());
        }
        self.recog = None;
        let skipped = warnings.lock().map(|g| g.len()).unwrap_or(0);
        Ok(if skipped == 0 {
            crate::tr("dlg.dw.st.created", &[("n", &n.to_string())])
        } else {
            crate::tr(
                "dlg.dw.st.accepted",
                &[("n", &n.to_string()), ("skipped", &skipped.to_string())],
            )
        })
    }

    /// Nearest primitive of a visible layer to a point: (drawing, single-element selection).
    pub fn pick_at(&self, level: i64, p: V2, tol: f64) -> Option<(usize, Pick)> {
        let mut best: Option<(f64, usize, Pick)> = None;
        let mut take = |d: f64, k: usize, pk: Pick| {
            if d <= tol && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, k, pk));
            }
        };
        let pv = |q: (f64, f64)| v2(q.0, q.1);
        for (k, d) in self.on_level(level) {
            let u = &d.u;
            for (i, l) in u.lines.iter().enumerate().filter(|x| d.shown(&x.1.layer)) {
                take(
                    seg_dist(p, pv(l.a), pv(l.b)),
                    k,
                    Pick {
                        lines: vec![i],
                        ..Default::default()
                    },
                );
            }
            for (i, a) in u.arcs.iter().enumerate().filter(|x| d.shown(&x.1.layer)) {
                let pts = arc_pts(a.c, a.r, a.a0, a.a1, 24);
                let dd = pts
                    .windows(2)
                    .map(|w| seg_dist(p, w[0], w[1]))
                    .fold(f64::MAX, f64::min);
                take(
                    dd,
                    k,
                    Pick {
                        arcs: vec![i],
                        ..Default::default()
                    },
                );
            }
            for (i, t) in u.texts.iter().enumerate().filter(|x| d.shown(&x.1.layer)) {
                take(
                    (p - pv(t.pos)).len() - t.height,
                    k,
                    Pick {
                        texts: vec![i],
                        ..Default::default()
                    },
                );
            }
            for (i, c) in u.closed.iter().enumerate().filter(|x| d.shown(&x.1.layer)) {
                let n = c.pts.len();
                let dd = (0..n)
                    .map(|j| seg_dist(p, pv(c.pts[j]), pv(c.pts[(j + 1) % n])))
                    .fold(f64::MAX, f64::min);
                take(
                    dd,
                    k,
                    Pick {
                        closed: vec![i],
                        ..Default::default()
                    },
                );
            }
            for (i, f) in u.fills.iter().enumerate().filter(|x| d.shown(&x.1.layer)) {
                if let Some(o) = f.loops.first() {
                    let n = o.len();
                    let dd = (0..n)
                        .map(|j| seg_dist(p, pv(o[j]), pv(o[(j + 1) % n])))
                        .fold(f64::MAX, f64::min);
                    take(
                        dd,
                        k,
                        Pick {
                            fills: vec![i],
                            ..Default::default()
                        },
                    );
                }
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// Click in selection mode: add/remove a primitive. `true` if the click was handled.
    pub fn pick_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        if !self.pick_mode {
            return false;
        }
        let Some((k, one)) = self.pick_at(level, p, tol) else {
            self.pick = None;
            return true;
        };
        let cur = match self.pick.take() {
            Some((d, pk)) if d == k => pk,
            _ => Pick::default(),
        };
        let mut pk = cur;
        fn tog(v: &mut Vec<usize>, add: &[usize]) {
            for i in add {
                match v.iter().position(|x| x == i) {
                    Some(j) => {
                        v.remove(j);
                    }
                    None => v.push(*i),
                }
            }
        }
        tog(&mut pk.lines, &one.lines);
        tog(&mut pk.arcs, &one.arcs);
        tog(&mut pk.texts, &one.texts);
        tog(&mut pk.closed, &one.closed);
        tog(&mut pk.fills, &one.fills);
        self.pick = (!pk.is_empty()).then_some((k, pk));
        true
    }

    /// Pick the primitives of the current drawing inside the box `a`-`b`. Window: whole primitive
    /// inside; crossing: touching. `add` (Shift) unites with the pick, `remove` (Ctrl) subtracts
    /// from it, neither replaces it. Returns the status line.
    pub fn pick_box(
        &mut self,
        level: i64,
        a: V2,
        b: V2,
        mode: BoxMode,
        add: bool,
        remove: bool,
    ) -> String {
        let k = self.sel;
        let found = match self.list.get(k).filter(|d| d.level == level && d.visible) {
            Some(d) => d.box_set(a, b, mode),
            None => Pick::default(),
        };
        if found.is_empty() {
            if !(add || remove) {
                self.pick = None;
            }
            return crate::tr("data_export.dwgpick.none", &[]);
        }
        self.merge_pick(k, found, add, remove)
    }

    /// Pick every primitive on the layer of the primitive at `p` in that drawing.
    pub fn pick_layer(&mut self, level: i64, p: V2, tol: f64, add: bool, remove: bool) -> String {
        let Some((k, one)) = self.pick_at(level, p, tol) else {
            if !(add || remove) {
                self.pick = None;
            }
            return crate::tr("data_export.dwgpick.none", &[]);
        };
        let d = &self.list[k];
        let u = &d.u;
        let layer = one
            .lines
            .first()
            .and_then(|&i| u.lines.get(i).map(|x| x.layer.clone()))
            .or_else(|| one.arcs.first().and_then(|&i| u.arcs.get(i).map(|x| x.layer.clone())))
            .or_else(|| one.texts.first().and_then(|&i| u.texts.get(i).map(|x| x.layer.clone())))
            .or_else(|| one.closed.first().and_then(|&i| u.closed.get(i).map(|x| x.layer.clone())))
            .or_else(|| one.fills.first().and_then(|&i| u.fills.get(i).map(|x| x.layer.clone())))
            .unwrap_or_default();
        let on = |l: &str| l == layer && d.shown(l);
        let all = |n: usize, f: &dyn Fn(usize) -> bool| (0..n).filter(|&i| f(i)).collect::<Vec<_>>();
        let found = Pick {
            lines: all(u.lines.len(), &|i| on(&u.lines[i].layer)),
            arcs: all(u.arcs.len(), &|i| on(&u.arcs[i].layer)),
            texts: all(u.texts.len(), &|i| on(&u.texts[i].layer)),
            closed: all(u.closed.len(), &|i| on(&u.closed[i].layer)),
            fills: all(u.fills.len(), &|i| on(&u.fills[i].layer)),
        };
        self.merge_pick(k, found, add, remove);
        let n = self.pick.as_ref().map_or(0, |p| p.1.len());
        crate::tr("data_export.dwgpick.layer", &[("layer", &layer), ("n", &n.to_string())])
    }

    fn merge_pick(&mut self, k: usize, found: Pick, add: bool, remove: bool) -> String {
        let cur = match self.pick.take() {
            Some((d, pk)) if d == k => pk,
            _ => Pick::default(),
        };
        let mix = |cur: Vec<usize>, new: Vec<usize>| -> Vec<usize> {
            if remove {
                let gone: HashSet<usize> = new.into_iter().collect();
                cur.into_iter().filter(|i| !gone.contains(i)).collect()
            } else if add {
                let mut seen: HashSet<usize> = cur.iter().copied().collect();
                let mut v = cur;
                v.extend(new.into_iter().filter(|i| seen.insert(*i)));
                v
            } else {
                new
            }
        };
        let pk = Pick {
            lines: mix(cur.lines, found.lines),
            arcs: mix(cur.arcs, found.arcs),
            texts: mix(cur.texts, found.texts),
            closed: mix(cur.closed, found.closed),
            fills: mix(cur.fills, found.fills),
        };
        let (n, rings) = (pk.len(), pk.closed.len());
        self.pick = (n > 0).then_some((k, pk));
        if rings > 0 {
            return crate::tr("data_export.dwgpick.box_rings", &[("n", &n.to_string()), ("rings", &rings.to_string())]);
        }
        crate::tr("data_export.dwgpick.box", &[("n", &n.to_string())])
    }

    /// Center of the selection's (or whole drawing's) bounding box: the rotation point.
    fn pivot(&self, k: usize, pk: Option<&Pick>) -> V2 {
        let Some(d) = self.list.get(k) else {
            return v2(0.0, 0.0);
        };
        let mut pts: Vec<(f64, f64)> = vec![];
        match pk {
            None => {
                if let Some((a, b)) = d.u.bbox() {
                    pts = vec![a, b];
                }
            }
            Some(pk) => {
                pk.lines
                    .iter()
                    .filter_map(|&i| d.u.lines.get(i))
                    .for_each(|l| pts.extend([l.a, l.b]));
                pk.arcs
                    .iter()
                    .filter_map(|&i| d.u.arcs.get(i))
                    .for_each(|a| pts.push(a.c));
                pk.texts
                    .iter()
                    .filter_map(|&i| d.u.texts.get(i))
                    .for_each(|t| pts.push(t.pos));
                pk.closed
                    .iter()
                    .filter_map(|&i| d.u.closed.get(i))
                    .for_each(|c| pts.extend(c.pts.iter().copied()));
                pk.fills
                    .iter()
                    .filter_map(|&i| d.u.fills.get(i))
                    .for_each(|f| pts.extend(f.loops.iter().flatten().copied()));
            }
        }
        if pts.is_empty() {
            return v2(0.0, 0.0);
        }
        let (mut lo, mut hi) = (pts[0], pts[0]);
        for q in &pts {
            lo = (lo.0.min(q.0), lo.1.min(q.1));
            hi = (hi.0.max(q.0), hi.1.max(q.1));
        }
        v2((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0)
    }

    /// Move/rotate (`copy` to copy) the selection or the whole selected drawing by `xf`.
    pub fn apply_xf(&mut self, whole: bool, copy: bool) -> String {
        let (dx, dy, deg) = self.xf;
        let rot = deg.to_radians();
        // nothing would change: no undo step that would undo nothing
        if !copy && dx == 0.0 && dy == 0.0 && deg == 0.0 {
            return crate::tr("dlg.dw.st.xf_zero", &[]);
        }
        if whole {
            let k = self.sel;
            let c = self.pivot(k, None);
            let Some(d) = self.list.get_mut(k) else {
                return crate::tr("dlg.dw.st.no_drawing", &[]);
            };
            if copy {
                let mut u = d.u.clone();
                u.transform((c.x, c.y), rot, (dx, dy));
                let (offset, r) = compose_xf(d.offset, d.rot, c, rot, v2(dx, dy));
                let copy = Drawing {
                    id: 0,
                    name: crate::tr("dlg.dw.st.copy_name", &[("name", &d.name)]),
                    path: d.path.clone(),
                    level: d.level,
                    u,
                    unit: d.unit,
                    offset,
                    rot: r,
                    layers: d.layers.clone(),
                    visible: true,
                    halftone: d.halftone,
                    idx: d.idx.clone(),
                    cur_layer: d.cur_layer.clone(),
                    dirty: d.dirty,
                    gen: d.gen,
                    saved_gen: d.saved_gen,
                    next_gen: d.next_gen,
                    parked: d.parked.clone(),
                };
                let source = d.id;
                let id = self.new_id();
                self.list.push(Drawing { id, ..copy });
                self.sel = self.list.len() - 1;
                self.steps.push(Step::copied(id, self.sel, source));
            } else {
                self.steps.push(Step::geometry(d));
                d.mark_edited();
                d.u.transform((c.x, c.y), rot, (dx, dy));
                (d.offset, d.rot) = compose_xf(d.offset, d.rot, c, rot, v2(dx, dy));
            }
            self.pick = None;
            self.recog = None;
            self.touch();
            return crate::tr(
                if copy {
                    "dlg.dw.st.copied"
                } else {
                    "dlg.dw.st.moved"
                },
                &[],
            );
        }
        let Some((k, pk)) = self.pick.clone() else {
            return crate::tr("dlg.dw.st.nothing_picked", &[]);
        };
        let c = self.pivot(k, Some(&pk));
        let Some(d) = self.list.get_mut(k) else {
            return crate::tr("dlg.dw.st.drawing_gone", &[]);
        };
        self.steps.push(Step::geometry(d));
        d.mark_edited();
        let out = d.u.transform_pick(&pk, (c.x, c.y), rot, (dx, dy), copy);
        let n = out.len();
        self.pick = Some((k, out));
        self.touch();
        crate::tr(
            if copy {
                "dlg.dw.st.prims_copied"
            } else {
                "dlg.dw.st.prims_changed"
            },
            &[("n", &n.to_string())],
        )
    }

    /// (id, generation) of every drawing now: take it when an export starts, hand it to
    /// [`Drawings::mark_saved`] once the file is written, so edits made meanwhile stay edited.
    pub fn generations(&self) -> Vec<(u64, u64)> {
        self.list.iter().map(|d| (d.id, d.gen)).collect()
    }

    /// The listed drawings were written to a file in the state of `snap`.
    pub fn mark_saved(&mut self, snap: &[(u64, u64)]) {
        for &(id, g) in snap {
            if let Some(d) = self.list.iter_mut().find(|d| d.id == id) {
                d.saved_gen = g;
                d.dirty = d.gen != d.saved_gen;
            }
        }
        self.touch();
    }

    /// Drawings edited since they were last written to a drawing file (export). The project
    /// file is checked by [`Drawings::unsaved_count`].
    pub fn edited(&self) -> usize {
        self.list.iter().filter(|d| d.dirty).count()
    }

    /// Every drawing written into `doc` in its file's coordinates and units, edits included.
    /// A whole-drawing copy keeps its source path: it is placed through the first drawing of
    /// that file, so the copy lands where the user put it instead of on top of the source.
    /// Test support: edits are checked by writing them out; the product never writes DXF.
    /// The `dxf-export` feature opens it to the drawing round-trip integration tests.
    #[cfg(any(test, feature = "dxf-export"))]
    pub fn export_into(&self, doc: &mut DxfDocument) -> dm::WriteReport {
        let mut rep = dm::WriteReport::default();
        // one file has one $INSUNITS: every drawing is written in the first one's unit
        let ok = |u: &f64| u.is_finite() && *u > 0.0;
        let common = doc
            .unit_meters()
            .filter(ok)
            .or_else(|| self.list.iter().map(|d| d.unit).find(ok));
        for d in &self.list {
            let base = d
                .path
                .as_ref()
                .and_then(|p| self.list.iter().find(|o| o.path.as_ref() == Some(p)))
                .unwrap_or(d);
            let pl = dm::Placement {
                offset: (base.offset.x, base.offset.y),
                rot: base.rot,
                unit: common.unwrap_or(d.unit),
            };
            rep.add(&dm::append_underlay(doc, &d.u, &pl));
        }
        rep
    }

    /// Delete the selected underlay primitives.
    pub fn delete_pick(&mut self) -> String {
        let Some((k, pk)) = self.pick.take() else {
            return crate::tr("dlg.dw.st.nothing_picked", &[]);
        };
        let Some(d) = self.list.get_mut(k) else {
            return crate::tr("dlg.dw.st.drawing_gone", &[]);
        };
        self.steps.push(Step::geometry(d));
        d.mark_edited();
        d.u.remove_pick(&pk);
        self.recog = None;
        self.touch();
        crate::tr("dlg.dw.st.prims_deleted", &[("n", &pk.len().to_string())])
    }

    /// Highlight of the selected primitives.
    pub fn paint_pick(&self, p: &Painter, to_s: &dyn Fn(V2) -> Pos2, level: i64, accent: Color32) {
        let Some((k, pk)) = &self.pick else { return };
        let Some(d) = self.list.get(*k).filter(|d| d.level == level && d.visible) else {
            return;
        };
        let st = Stroke::new(3.0, accent.gamma_multiply(0.8));
        let pv = |q: (f64, f64)| to_s(v2(q.0, q.1));
        for l in pk.lines.iter().filter_map(|&i| d.u.lines.get(i)) {
            p.line_segment([pv(l.a), pv(l.b)], st);
        }
        for a in pk.arcs.iter().filter_map(|&i| d.u.arcs.get(i)) {
            let pts: Vec<Pos2> = arc_pts(a.c, a.r, a.a0, a.a1, 48)
                .into_iter()
                .map(to_s)
                .collect();
            p.add(egui::Shape::line(pts, st));
        }
        for t in pk.texts.iter().filter_map(|&i| d.u.texts.get(i)) {
            p.circle_stroke(pv(t.pos), 5.0, st);
        }
        for c in pk.closed.iter().filter_map(|&i| d.u.closed.get(i)) {
            if c.pts.len() > 1 {
                p.add(egui::Shape::closed_line(c.pts.iter().map(|q| pv(*q)).collect(), st));
            }
        }
        for f in pk.fills.iter().filter_map(|&i| d.u.fills.get(i)) {
            if let Some(o) = f.loops.first() {
                p.add(egui::Shape::closed_line(
                    o.iter().map(|q| pv(*q)).collect(),
                    st,
                ));
            }
        }
    }

    /// Extents of the level's drawings for fitting the view (stray points ignored).
    pub fn fit_bbox(&self, level: i64) -> Option<(V2, V2)> {
        let mut r: Option<(V2, V2)> = None;
        for (_, d) in self.on_level(level) {
            if let Some((a, b)) = d.fit_bbox() {
                r = Some(match r {
                    None => (a, b),
                    Some((lo, hi)) => (
                        v2(lo.x.min(a.x), lo.y.min(a.y)),
                        v2(hi.x.max(b.x), hi.y.max(b.y)),
                    ),
                });
            }
        }
        r
    }

    pub fn bbox(&self, level: i64) -> Option<(V2, V2)> {
        let mut r: Option<(V2, V2)> = None;
        for (_, d) in self.on_level(level) {
            if let Some((a, b)) = d.bbox() {
                r = Some(match r {
                    None => (a, b),
                    Some((lo, hi)) => (
                        v2(lo.x.min(a.x), lo.y.min(a.y)),
                        v2(hi.x.max(b.x), hi.y.max(b.y)),
                    ),
                });
            }
        }
        r
    }

    /// Underlays of the level: fills, lines and arcs from the tessellation cache (`tess`), texts
    /// laid out per frame; a view the cache does not take is drawn directly.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        p: &Painter,
        to_s: &dyn Fn(V2) -> Pos2,
        clip: Rect,
        scale: f64,
        level: i64,
        fg: Color32,
        paper: Color32,
    ) {
        let clip = clip.expand(4.0);
        let st = tess::Style {
            fg,
            paper,
            ppp: p.ctx().pixels_per_point(),
            opts: p.ctx().tessellation_options(|o| *o),
        };
        let mut cache = self.tess.borrow_mut();
        for (_, d) in self.on_level(level) {
            if !tess::paint_cached(&mut cache, self.version, d, p, to_s, clip, scale, &st) {
                emit_geometry(d, to_s, scale, Some(clip), fg, paper, &mut |sh, _, _| {
                    p.add(sh);
                });
            }
            paint_texts(d, p, to_s, clip, scale, fg, paper);
        }
    }

    /// Builds of the tessellation cache so far.
    pub fn tess_builds(&self) -> u64 {
        self.tess.borrow().builds
    }

    /// Fill the tessellation cache for the level at `scale` without painting; `true` when every
    /// drawing was already there.
    pub fn tess_warm(&self, level: i64, scale: f64, st: &tess::Style) -> bool {
        let mut cache = self.tess.borrow_mut();
        let mut all = true;
        for (_, d) in self.on_level(level) {
            all &= tess::warm(&mut cache, self.version, d, scale, st);
        }
        all
    }

    /// Recognition preview over the plan: accepted ones in accent color, rejected ones gray.
    pub fn paint_recog(
        &self,
        p: &Painter,
        to_s: &dyn Fn(V2) -> Pos2,
        level: i64,
        accent: Color32,
        dim: Color32,
        hover: Option<u64>,
    ) {
        let Some(rc) = &self.recog else { return };
        if self.list.get(rc.drawing).is_none_or(|d| d.level != level) {
            return;
        }
        let col = |id: u64, base: Color32| -> (Color32, f32) {
            let c = if rc.on.contains(&id) { base } else { dim };
            let w = if hover == Some(id) { 2.5 } else { 1.2 };
            (c, w)
        };
        let pr = &rc.props;
        let wall_c = accent;
        let tags = ddc_ui::theme::sem_of(p.ctx());
        let door_c = tags.canvas_tag_door;
        let win_c = tags.canvas_tag_window;
        let room_c = tags.canvas_tag_room;
        let grid_c = tags.canvas_tag_grid;
        let col_c = tags.canvas_tag_column;
        let quad = |a: V2, b: V2, half: f64| -> Vec<Pos2> {
            let d = b - a;
            let n = v2(-d.y, d.x) * (half / d.len().max(1e-9));
            vec![to_s(a + n), to_s(b + n), to_s(b - n), to_s(a - n)]
        };
        for r in &pr.rooms {
            let (c, w) = col(r.id, room_c);
            if r.boundary.len() >= 3 {
                let pts: Vec<Pos2> = r.boundary.iter().map(|q| to_s(v2(q.0, q.1))).collect();
                p.add(egui::Shape::closed_line(
                    pts,
                    Stroke::new(w, c.gamma_multiply(0.8)),
                ));
            }
            let at = to_s(v2(r.point.0, r.point.1));
            p.circle_filled(at, 4.0, c);
            let label = if r.number.is_empty() {
                r.name.clone()
            } else {
                format!("{} {}", r.number, r.name)
            };
            p.text(
                at + egui::vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                c,
            );
        }
        for wc in &pr.walls {
            let (c, w) = col(wc.id, wall_c);
            let pts = quad(v2(wc.a.0, wc.a.1), v2(wc.b.0, wc.b.1), wc.thickness / 2.0);
            p.add(egui::Shape::convex_polygon(
                pts,
                c.gamma_multiply(0.28),
                Stroke::new(w, c),
            ));
        }
        for o in &pr.openings {
            let Some(wc) = pr.walls.iter().find(|w| w.id == o.wall) else {
                continue;
            };
            let (a, b) = (v2(wc.a.0, wc.a.1), v2(wc.b.0, wc.b.1));
            let d = (b - a) * (1.0 / (b - a).len().max(1e-9));
            let (s, e) = (a + d * o.along, a + d * (o.along + o.width));
            let (c, w) = col(o.id, if o.kind == "door" { door_c } else { win_c });
            p.add(egui::Shape::convex_polygon(
                quad(s, e, wc.thickness / 2.0 + 0.04),
                c.gamma_multiply(0.45),
                Stroke::new(w + 0.5, c),
            ));
        }
        for g in &pr.grids {
            let (c, w) = col(g.id, grid_c);
            let (a, b) = (to_s(v2(g.a.0, g.a.1)), to_s(v2(g.b.0, g.b.1)));
            p.add(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(w, c),
                10.0,
                4.0,
            ));
            let dir = (b - a).normalized();
            let at = b + dir * 14.0;
            p.circle_stroke(at, 11.0, Stroke::new(w, c));
            p.text(
                at,
                egui::Align2::CENTER_CENTER,
                &g.name,
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                c,
            );
        }
        for cc in &pr.columns {
            let (c, w) = col(cc.id, col_c);
            let at = v2(cc.at.0, cc.at.1);
            if let Some(dm) = cc.diameter {
                let r = (to_s(at + v2(dm / 2.0, 0.0)) - to_s(at)).length();
                p.circle(to_s(at), r, c.gamma_multiply(0.5), Stroke::new(w, c));
            } else {
                let (s, cs) = cc.rotation.sin_cos();
                let (u, v) = (v2(cs, s) * (cc.b / 2.0), v2(-s, cs) * (cc.h / 2.0));
                let pts = vec![
                    to_s(at - u - v),
                    to_s(at + u - v),
                    to_s(at + u + v),
                    to_s(at - u + v),
                ];
                p.add(egui::Shape::convex_polygon(
                    pts,
                    c.gamma_multiply(0.5),
                    Stroke::new(w, c),
                ));
            }
        }
    }
}

/// Wall axis along the traced line: side and offset.
/// Rotation `rot` about `c` and shift `d` on top of the transform (offset, r0): the new (offset, r).
fn compose_xf(offset: V2, r0: f64, c: V2, rot: f64, d: V2) -> (V2, f64) {
    let (sn, cs) = rot.sin_cos();
    let q = offset - c;
    (
        v2(q.x * cs - q.y * sn, q.x * sn + q.y * cs) + c + d,
        r0 + rot,
    )
}

/// DXF layer names are case-insensitive: a primitive's layer is mapped to the name in the layer table.
fn canon_layers(u: &mut Underlay) {
    let canon: HashMap<String, String> = u
        .layers
        .iter()
        .map(|l| (l.name.to_uppercase(), l.name.clone()))
        .collect();
    let fix = |l: &mut String| {
        if let Some(c) = canon.get(&l.to_uppercase()) {
            if c != l {
                *l = c.clone();
            }
        }
    };
    u.lines.iter_mut().for_each(|x| fix(&mut x.layer));
    u.arcs.iter_mut().for_each(|x| fix(&mut x.layer));
    u.texts.iter_mut().for_each(|x| fix(&mut x.layer));
    u.closed.iter_mut().for_each(|x| fix(&mut x.layer));
    u.fills.iter_mut().for_each(|x| fix(&mut x.layer));
    u.inserts.iter_mut().for_each(|x| fix(&mut x.layer));
    u.block_hints.iter_mut().for_each(|x| fix(&mut x.layer));
}

pub fn trace_axis(a: V2, b: V2, cursor: V2, half: f64, tr: Trace) -> (V2, V2) {
    let d = b - a;
    let n = v2(-d.y, d.x) * (1.0 / d.len().max(1e-12));
    let left = (cursor - a).dot(n) >= 0.0;
    let k = match tr.side {
        Side::Center => tr.offset,
        Side::Left => half + tr.offset,
        Side::Right => -(half + tr.offset),
        Side::Cursor if left => half + tr.offset,
        Side::Cursor => -(half + tr.offset),
    };
    (a + n * k, b + n * k)
}

fn arc_pts(c: (f64, f64), r: f64, a0: f64, a1: f64, n: usize) -> Vec<V2> {
    (0..=n)
        .map(|k| {
            let t = a0 + (a1 - a0) * k as f64 / n as f64;
            v2(c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect()
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// File color against the theme background: white means the text color; colors too light for a light background are darkened.
fn readable(c: Color32, dark: bool, fg: Color32) -> Color32 {
    let lum = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if (c.r(), c.g(), c.b()) == (255, 255, 255) {
        return fg;
    }
    if !dark && lum > 150.0 {
        mix(c, Color32::BLACK, 0.45)
    } else if dark && lum < 60.0 {
        mix(c, Color32::WHITE, 0.45)
    } else {
        c
    }
}

/// Polyline in pen dashes (m); dashes under 2 px are drawn solid.
fn dashed(pts: &[Pos2], st: Stroke, dash: &[f32], scale: f64) -> egui::Shape {
    let on: f32 = dash.iter().filter(|x| **x > 0.0).sum::<f32>() * scale as f32;
    let off: f32 = dash.iter().filter(|x| **x < 0.0).map(|x| -x).sum::<f32>() * scale as f32;
    if dash.is_empty() || on < 2.0 || off < 1.5 {
        if pts.len() == 2 {
            egui::Shape::line_segment([pts[0], pts[1]], st)
        } else {
            egui::Shape::line(pts.to_vec(), st)
        }
    } else {
        egui::Shape::Vec(egui::Shape::dashed_line(pts, st, on, off))
    }
}

fn layer_color(
    d: &Drawing,
    layer: &str,
    pen: Option<[u8; 3]>,
    fg: Color32,
    paper: Color32,
) -> Color32 {
    let dark = paper.r() < 128;
    let ls = d.layer(layer);
    let rgb = ls
        .and_then(|l| l.color)
        .or(pen)
        .or(ls.and_then(|l| l.file_color));
    let mut c = match rgb {
        None => fg,
        Some([r, g, b]) => readable(Color32::from_rgb(r, g, b), dark, fg),
    };
    if d.halftone || ls.is_some_and(|l| l.halftone) {
        c = mix(c, paper, 0.6);
    }
    c
}

/// Fills, lines and arcs of a drawing as shapes in the screen space of `to_s`; `out` gets the
/// shape, whether it is a fill, and an anchor point. `clip` None keeps everything (cache builds).
fn emit_geometry(
    d: &Drawing,
    to_s: &dyn Fn(V2) -> Pos2,
    scale: f64,
    clip: Option<Rect>,
    fg: Color32,
    paper: Color32,
    out: &mut dyn FnMut(egui::Shape, bool, Pos2),
) {
    let seen = |r: Rect| clip.is_none_or(|c| r.intersects(c));
    let color = |layer: &str, pen: Option<[u8; 3]>| layer_color(d, layer, pen, fg, paper);
    let width = |w: f32| (0.6 + w * 2.2).clamp(0.6, 2.4);
    for f in &d.u.fills {
        if !d.shown(&f.layer) {
            continue;
        }
        let pen = d.u.pen(f.pen);
        let c = color(&f.layer, pen.color);
        if f.solid {
            if let Some(outer) = f.loops.first() {
                let ring: Vec<V2> = outer.iter().map(|q| v2(q.0, q.1)).collect();
                let pts: Vec<Pos2> = ring.iter().map(|q| to_s(*q)).collect();
                if pts.is_empty() || !seen(Rect::from_points(&pts)) {
                    continue;
                }
                let mut mesh = egui::Mesh::default();
                for q in &pts {
                    mesh.colored_vertex(*q, c.gamma_multiply(0.85));
                }
                for t in ddc_geom::poly::triangulate(&ring) {
                    mesh.add_triangle(t[0] as u32, t[1] as u32, t[2] as u32);
                }
                out(egui::Shape::mesh(mesh), true, pts[0]);
            }
        } else {
            let st = Stroke::new(0.6, c.gamma_multiply(0.7));
            for (a, b) in &f.segs {
                let (a, b) = (to_s(v2(a.0, a.1)), to_s(v2(b.0, b.1)));
                if seen(Rect::from_two_pos(a, b)) {
                    out(egui::Shape::line_segment([a, b], st), true, a);
                }
            }
        }
    }
    for l in &d.u.lines {
        if !d.shown(&l.layer) {
            continue;
        }
        let (a, b) = (to_s(v2(l.a.0, l.a.1)), to_s(v2(l.b.0, l.b.1)));
        if !seen(Rect::from_two_pos(a, b)) {
            continue;
        }
        let pen = d.u.pen(l.pen);
        let st = Stroke::new(width(pen.weight_mm), color(&l.layer, pen.color));
        out(dashed(&[a, b], st, &pen.dash, scale), false, a);
    }
    for a in &d.u.arcs {
        if !d.shown(&a.layer) {
            continue;
        }
        let r_px = a.r * scale;
        let lo = to_s(v2(a.c.0 - a.r, a.c.1 + a.r));
        let hi = to_s(v2(a.c.0 + a.r, a.c.1 - a.r));
        if !seen(Rect::from_two_pos(lo, hi)) || r_px < 0.3 {
            continue;
        }
        let n = (((a.a1 - a.a0) * r_px / 5.0).ceil() as usize).clamp(6, 160);
        let pts: Vec<Pos2> = arc_pts(a.c, a.r, a.a0, a.a1, n)
            .into_iter()
            .map(to_s)
            .collect();
        let pen = d.u.pen(a.pen);
        let st = Stroke::new(width(pen.weight_mm), color(&a.layer, pen.color));
        out(dashed(&pts, st, &pen.dash, scale), false, to_s(v2(a.c.0, a.c.1)));
    }
}

/// Texts follow the exact zoom (font size), so they stay out of the cache.
fn paint_texts(
    d: &Drawing,
    p: &Painter,
    to_s: &dyn Fn(V2) -> Pos2,
    clip: Rect,
    scale: f64,
    fg: Color32,
    paper: Color32,
) {
    for t in &d.u.texts {
        let h = (t.height * scale) as f32;
        if h < 3.5 || !d.shown(&t.layer) || t.text.trim().is_empty() {
            continue;
        }
        let (sn, cs) = t.rotation.sin_cos();
        // top-left of the line: up from the baseline by the height
        let top = to_s(v2(t.pos.0 - sn * t.height, t.pos.1 + cs * t.height));
        if !clip.contains(top) && !clip.contains(to_s(v2(t.pos.0, t.pos.1))) {
            continue;
        }
        let pen = d.u.pen(t.pen);
        let c = layer_color(d, &t.layer, pen.color, fg, paper);
        // size capped and rounded: deep zoom does not grow the font atlas
        let fs = (h * 1.2).min(360.0).round();
        let g = p.layout_no_wrap(t.text.clone(), egui::FontId::proportional(fs), c);
        p.add(egui::epaint::TextShape::new(top, g, c).with_angle(-t.rotation as f32));
    }
}

/// Import report line for numbers that read two ways (area labels, block widths); None when all
/// numbers were clear. Only the wording follows the UI language, never the count.
pub fn unreadable_note(r: &dm::Report, ru: bool) -> Option<String> {
    (r.unreadable_numbers > 0).then(|| {
        crate::tr_lang(ru, "drawing_numbers.dw.unreadable", &[("n", &r.unreadable_numbers.to_string())])
    })
}

#[cfg(test)]
mod xs42_tests {
    use super::*;

    #[test]
    fn unreadable_note_in_both_languages_same_count() {
        let clear = dm::Report::default();
        assert_eq!(unreadable_note(&clear, false), None);
        assert_eq!(unreadable_note(&clear, true), None);
        let r = dm::Report {
            unreadable_numbers: 3,
            ..Default::default()
        };
        let en = unreadable_note(&r, false).unwrap();
        let ru = unreadable_note(&r, true).unwrap();
        assert_ne!(en, ru, "both catalogs carry the string");
        for s in [&en, &ru] {
            assert!(s.contains('3') && !s.contains("{n}") && !s.contains("drawing_numbers."), "{s}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_offsets_to_cursor_side() {
        let tr = Trace {
            side: Side::Cursor,
            offset: 0.0,
        };
        let (a, b) = trace_axis(v2(0.0, 0.0), v2(4.0, 0.0), v2(1.0, 0.5), 0.15, tr);
        assert!((a.y - 0.15).abs() < 1e-12 && (b.y - 0.15).abs() < 1e-12);
        let (a, _) = trace_axis(v2(0.0, 0.0), v2(4.0, 0.0), v2(1.0, -0.5), 0.15, tr);
        assert!((a.y + 0.15).abs() < 1e-12);
        let c = Trace {
            side: Side::Center,
            offset: 0.05,
        };
        assert!(
            (trace_axis(v2(0.0, 0.0), v2(0.0, 3.0), v2(5.0, 0.0), 0.1, c)
                .0
                .x
                + 0.05)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn import_place_recognize_accept_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        let doc = dm::fixture::sample_plan(0);
        let msg = ds.add(
            &doc,
            "plan.dxf",
            None,
            ed.level,
            None,
            Origin::MinToZero,
            v2(0.0, 0.0),
        );
        assert!(msg.contains(&crate::app2_fx::fx("drawing_unit_1mm")), "{msg}");
        // the bounding box starts at the axis overhang (−1.6) and is shifted to 0
        let (lo, _) = ds.bbox(ed.level).unwrap();
        assert!(lo.x.abs() < 1e-9 && lo.y.abs() < 1e-9);
        assert!(!ds.snap_segs(ed.level).is_empty());
        // a hidden axes layer is not recognized
        let k = ds.list[0]
            .layers
            .iter()
            .position(|l| l.name == "S-GRID")
            .unwrap();
        ds.list[0].layers[k].visible = false;
        ds.recognize(&ed);
        assert!(ds.recog.as_ref().unwrap().props.grids.is_empty());
        ds.list[0].layers[k].visible = true;
        ds.recognize(&ed);
        let rc = ds.recog.as_mut().unwrap();
        assert_eq!(rc.count_on(Class::Grids), (6, 6));
        rc.set_class(Class::Columns, false);
        assert_eq!(rc.count_on(Class::Columns).0, 0);
        let walls0 = ed.walls.len();
        let msg = ds.accept(&mut ed).unwrap();
        assert!(msg.contains(&crate::app2_fx::fx("drawing_one_op")));
        assert_eq!(ed.walls.len(), walls0 + 5);
        let cols: i64 = ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM col", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cols, 0, "class rejected");
        ed.undo();
        assert_eq!(ed.walls.len(), walls0);
        let grids: i64 = ed
            .model
            .sql()
            .query_row("SELECT count(*) FROM grid", [], |r| r.get(0))
            .unwrap();
        assert_eq!(grids, 0);
    }

    #[test]
    fn move_rotate_copy_underlay_and_primitives() {
        let mut ds = Drawings::default();
        let s = ds.add(
            &dm::fixture::sample_plan(0),
            "p",
            None,
            1,
            None,
            Origin::AsIs,
            v2(0.0, 0.0),
        );
        assert!(!s.contains("not carried over") || s.contains('×'), "{s}");
        let (lo0, hi0) = ds.bbox(1).unwrap();
        ds.xf = (10.0, 0.0, 0.0);
        ds.apply_xf(true, false);
        let (lo1, _) = ds.bbox(1).unwrap();
        assert!((lo1.x - lo0.x - 10.0).abs() < 1e-9);
        // a 90° rotation about the bounding-box center swaps width and height
        ds.xf = (0.0, 0.0, 90.0);
        ds.apply_xf(true, false);
        let (lo2, hi2) = ds.bbox(1).unwrap();
        assert!(((hi2.x - lo2.x) - (hi0.y - lo0.y)).abs() < 1e-6);
        ds.xf = (0.0, 5.0, 0.0);
        ds.apply_xf(true, true);
        assert_eq!(ds.list.len(), 2);
        // one primitive: click selection, copy, delete
        ds.list.remove(1);
        ds.sel = 0;
        ds.pick_mode = true;
        let l = ds.list[0]
            .u
            .lines
            .iter()
            .find(|l| l.layer == "A-WALL")
            .unwrap()
            .clone();
        let mid = v2((l.a.0 + l.b.0) / 2.0, (l.a.1 + l.b.1) / 2.0);
        assert!(ds.pick_click(1, mid, 0.01));
        let n = ds.list[0].u.lines.len();
        ds.xf = (0.0, 1.0, 0.0);
        assert!(ds.apply_xf(false, true).contains('1'));
        assert_eq!(ds.list[0].u.lines.len(), n + 1);
        ds.delete_pick();
        assert_eq!(ds.list[0].u.lines.len(), n);
    }

    #[test]
    fn trace_click_builds_wall_on_face() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        ds.add(
            &dm::fixture::sample_plan(0),
            "p",
            None,
            ed.level,
            None,
            Origin::AsIs,
            v2(0.0, 0.0),
        );
        ds.trace = Some(Trace {
            side: Side::Cursor,
            offset: 0.0,
        });
        let n = ed.walls.len();
        // outer face of the lower wall is y = −0.15, the cursor is inside the building
        assert!(ds.trace_click(&mut ed, v2(5.0, -0.12), 0.05));
        assert_eq!(ed.walls.len(), n + 1);
        let w = ed.walls.last().unwrap();
        let half = ed.current_type().unwrap().thickness() / 2.0;
        assert!((w.a.y - (-0.15 + half)).abs() < 1e-9, "{:?}", w.a);
    }

    fn plan(ds: &mut Drawings, level: i64, name: &str) {
        ds.add(
            &dm::fixture::sample_plan(0),
            name,
            None,
            level,
            None,
            Origin::AsIs,
            v2(0.0, 0.0),
        );
    }

    #[test]
    fn remove_drawing_remaps_recog_and_pick_by_id() {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        plan(&mut ds, ed.level, "a");
        plan(&mut ds, ed.level, "b");
        plan(&mut ds, ed.level, "c");
        ds.sel = 2;
        ds.recognize(&ed);
        ds.pick = Some((
            1,
            Pick {
                lines: vec![0],
                ..Default::default()
            },
        ));
        let (id_b, id_c) = (ds.list[1].id, ds.list[2].id);
        ds.remove(0);
        assert_eq!(ds.recog.as_ref().map(|r| ds.list[r.drawing].id), Some(id_c));
        assert_eq!(ds.pick.as_ref().map(|p| ds.list[p.0].id), Some(id_b));
        assert_eq!(ds.list[ds.sel].id, id_c);
        // the selection's drawing was removed: selection reset, another drawing's recognition kept
        ds.remove(0);
        assert!(ds.pick.is_none());
        assert_eq!(ds.recog.as_ref().map(|r| r.drawing), Some(0));
        ds.remove(0);
        assert!(ds.recog.is_none() && ds.list.is_empty());
    }

    #[test]
    fn pending_follows_underlay_and_proposals() {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        assert!(!ds.has_pending());
        plan(&mut ds, ed.level, "a");
        assert!(ds.has_pending());
        ds.recognize(&ed);
        assert!(ds.has_pending());
        ds.remove(0);
        assert!(!ds.has_pending());
    }

    #[test]
    fn recog_hit_ignores_other_levels() {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        plan(&mut ds, ed.level, "a");
        ds.recognize(&ed);
        let w = ds.recog.as_ref().unwrap().props.walls[0].clone();
        let mid = v2((w.a.0 + w.b.0) / 2.0, (w.a.1 + w.b.1) / 2.0);
        assert!(ds.recog_hit(ed.level, mid, 0.01).is_some());
        assert!(ds.recog_hit(ed.level + 1000, mid, 0.01).is_none());
        ds.list[0].level = ed.level + 1000;
        assert!(ds.recog_hit(ed.level, mid, 0.01).is_none());
    }

    #[test]
    fn trace_and_pick_exit_on_tool_change_and_window_close() {
        let mut ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        ds.layers_open = true;
        ds.trace = Some(Trace {
            side: Side::Cursor,
            offset: 0.0,
        });
        ed.set_mode(crate::wall_tool::Mode::Polyline);
        assert!(wall_tool_active(&ed));
        assert!(ds.sync_tool(tool_sig(&ed)).is_none());
        assert!(ds.sync_tool(tool_sig(&ed)).is_none());
        assert!(ds.trace.is_some());
        ed.set_mode(crate::wall_tool::Mode::Select);
        assert!(!wall_tool_active(&ed));
        assert!(ds.sync_tool(tool_sig(&ed)).is_some());
        assert!(ds.trace.is_none());
        // primitive selection: layers window closed, mode off
        ds.pick_mode = true;
        ds.pick = Some((0, Pick::default()));
        assert!(ds.sync_tool(tool_sig(&ed)).is_none());
        ds.layers_open = false;
        assert!(ds.sync_tool(tool_sig(&ed)).is_some());
        assert!(!ds.pick_mode && ds.pick.is_none());
        // re-enabling remembers the current tool afresh
        ds.layers_open = true;
        ds.pick_mode = true;
        ds.rearm_tool();
        assert!(ds.sync_tool(tool_sig(&ed)).is_none());
        assert!(ds.pick_mode);
    }

    #[test]
    fn layer_names_are_case_insensitive() {
        let doc = dm::fixture::sample_plan(0);
        let mut u = Underlay::from_dxf(&doc, 0.001);
        let i = u.lines.iter().position(|l| l.layer == "A-WALL").unwrap();
        let name = u.lines[i].layer.clone();
        assert!(u.layers.iter().any(|l| l.name == name));
        u.lines[i].layer = "a-Wall".into();
        let mut ds = Drawings::default();
        ds.add_underlay(u, 0.001, "p", None, 1, Origin::AsIs, v2(0.0, 0.0));
        let d = &ds.list[0];
        assert_eq!(d.u.lines[i].layer, name);
        let k = d.layers.iter().position(|l| l.name == name).unwrap();
        ds.list[0].layers[k].visible = false;
        assert!(!ds.list[0]
            .visible_underlay()
            .lines
            .iter()
            .any(|l| l.layer.eq_ignore_ascii_case(&name)));
    }

    #[test]
    fn placement_keeps_rotation() {
        let mut ds = Drawings::default();
        plan(&mut ds, 1, "p");
        let f = ds.list[0].u.lines[0].a;
        ds.xf = (3.0, -2.0, 90.0);
        ds.apply_xf(true, false);
        ds.xf = (1.0, 0.0, 30.0);
        ds.apply_xf(true, true);
        for d in &ds.list {
            let (sn, cs) = d.rot.sin_cos();
            let m = d.u.lines[0].a;
            let e = (
                f.0 * cs - f.1 * sn + d.offset.x,
                f.0 * sn + f.1 * cs + d.offset.y,
            );
            assert!(
                (m.0 - e.0).abs() < 1e-9 && (m.1 - e.1).abs() < 1e-9,
                "{m:?} vs {e:?}"
            );
        }
        assert!((ds.list[1].rot - 120f64.to_radians()).abs() < 1e-12);
        assert_ne!(ds.list[0].id, ds.list[1].id);
    }

    /// Grid lines and circles of the source file, file units.
    fn grid_of(doc: &DxfDocument) -> (Vec<[f64; 4]>, Vec<[f64; 3]>) {
        let (mut l, mut c) = (vec![], vec![]);
        for e in &doc.entities {
            match e {
                ddc_io::dxf::Entity::Line(x) if x.common.layer == "S-GRID" => {
                    l.push([x.start[0], x.start[1], x.end[0], x.end[1]])
                }
                ddc_io::dxf::Entity::Circle(x) if x.common.layer == "S-GRID" => {
                    c.push([x.center[0], x.center[1], x.radius])
                }
                _ => {}
            }
        }
        (l, c)
    }

    fn has_line(v: &[[f64; 4]], x: &[f64; 4]) -> bool {
        let n = |a: f64, b: f64| (a - b).abs() < 1e-6;
        v.iter().any(|y| {
            (n(x[0], y[0]) && n(x[1], y[1]) && n(x[2], y[2]) && n(x[3], y[3]))
                || (n(x[0], y[2]) && n(x[1], y[3]) && n(x[2], y[0]) && n(x[3], y[1]))
        })
    }

    #[test]
    fn export_in_file_coords_after_placement() {
        let src = dm::fixture::sample_plan(0);
        let (lines0, circles0) = grid_of(&src);
        assert!(lines0.len() >= 6 && circles0.len() >= 6);
        let mut ds = Drawings::default();
        ds.add(&src, "p", None, 1, None, Origin::MinToZero, v2(5.0, 7.0));
        assert!(ds.list[0].offset.len() > 1.0, "placed away from the file origin");
        ds.xf = (3.0, -2.0, 37.0);
        ds.apply_xf(true, false);
        let mut doc = DxfDocument::default();
        let rep = ds.export_into(&mut doc);
        // the file's doors and windows stay blocks; placement only, so on the file's own spots
        assert_eq!((rep.blocks, rep.blocks_exploded), (4, 0));
        assert_eq!(rep.blocks, ds.list[0].u.blocks.refs.len());
        let doors = |doc: &DxfDocument| -> Vec<[f64; 3]> {
            doc.entities
                .iter()
                .filter_map(|e| match e {
                    ddc_io::dxf::Entity::Insert(i) if i.block == "DOOR_900" => Some(i.pos),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(doors(&doc), doors(&src));
        let (lines, circles) = grid_of(&doc);
        assert_eq!(lines.len(), lines0.len());
        assert!(lines0.iter().all(|x| has_line(&lines, x)), "lines back on the file's own spots");
        assert_eq!(circles.len(), circles0.len());
        for c in &circles0 {
            assert!(
                circles.iter().any(|d| (0..3).all(|k| (c[k] - d[k]).abs() < 1e-6)),
                "{c:?}"
            );
        }
        // a whole copy of the same file is written where the user put it, not on its source
        ds.list[0].path = Some(PathBuf::from("plan.dxf"));
        ds.xf = (0.0, 5.0, 0.0);
        ds.apply_xf(true, true);
        assert_eq!(ds.list[1].path, ds.list[0].path);
        let mut doc = DxfDocument::default();
        let rep = ds.export_into(&mut doc);
        let (lines, _) = grid_of(&doc);
        assert_eq!(lines.len(), 2 * lines0.len());
        // the copy's blocks are shifted too and share the definitions
        assert_eq!((rep.blocks, rep.block_defs), (8, 2));
        let d = doors(&doc);
        assert_eq!(d.len(), 4);
        let shift = (d[2][0] - d[0][0]).hypot(d[2][1] - d[0][1]);
        assert!((shift - 5000.0).abs() < 1e-6, "{d:?}");
        let on_source = lines.iter().filter(|x| has_line(&lines0, x)).count();
        assert_eq!(on_source, lines0.len(), "the copy is shifted");
    }

    fn ln(layer: &str, a: (f64, f64), b: (f64, f64)) -> dm::ULine {
        dm::ULine { layer: layer.into(), a, b, dashed: false, pen: 0 }
    }

    fn arc(layer: &str, c: (f64, f64), r: f64, a0: f64, a1: f64) -> dm::UArc {
        dm::UArc { layer: layer.into(), c, r, a0, a1, pen: 0 }
    }

    fn txt(layer: &str, pos: (f64, f64)) -> dm::UText {
        dm::UText { layer: layer.into(), pos, height: 0.2, rotation: 0.0, text: "ab".into(), pen: 0, fmt: Default::default() }
    }

    fn fill(layer: &str, ring: Vec<(f64, f64)>) -> dm::UFill {
        dm::UFill {
            layer: layer.into(),
            loops: vec![ring],
            solid: true,
            pattern: String::new(),
            segs: vec![],
            pen: 0,
        }
    }

    fn hide(ds: &mut Drawings, layer: &str) {
        ds.list[0].layers.iter_mut().find(|l| l.name == layer).unwrap().visible = false;
    }

    /// One drawing with hand-made primitives around the box (0,0)-(4,4).
    fn box_scene() -> (Drawings, i64) {
        let mut ds = Drawings::default();
        ds.add(&dm::fixture::sample_plan(0), "p", None, 1, None, Origin::AsIs, v2(0.0, 0.0));
        let u = &mut ds.list[0].u;
        *u = Underlay::default();
        // lines: 0 inside, 1 crossing the right side, 2 outside, 3 touching the top side only,
        // 4 passing through with both ends outside
        u.lines = vec![
            ln("A-WALL", (1.0, 1.0), (2.0, 1.0)),
            ln("A-WALL", (3.0, 2.0), (6.0, 2.0)),
            ln("A-WALL", (5.0, 5.0), (7.0, 5.0)),
            ln("A-DOOR", (1.0, 4.0), (6.0, 4.0)),
            ln("A-DOOR", (-1.0, 3.0), (6.0, 3.0)),
        ];
        // arcs: 0 inside, 1 dips into the box while both end points lie outside,
        // 2 a big circle around the box that never touches it
        u.arcs = vec![
            arc("A-WALL", (2.0, 2.0), 1.0, 0.0, 1.5),
            arc("A-WALL", (2.0, 7.0), 4.0, 4.0, 5.4),
            arc("A-WALL", (2.0, 2.0), 20.0, 0.0, 6.0),
        ];
        // texts: 0 inside, 1 straddling the left side, 2 outside
        u.texts = vec![
            txt("A-ANNO-TEXT", (1.0, 1.0)),
            txt("A-ANNO-TEXT", (-0.1, 1.0)),
            txt("A-ANNO-TEXT", (9.0, 9.0)),
        ];
        // fills: 0 inside, 1 straddling, 2 a big ring that holds the whole box in its interior
        u.fills = vec![
            fill("A-HATCH", vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0)]),
            fill("A-HATCH", vec![(3.0, 3.0), (8.0, 3.0), (8.0, 8.0), (3.0, 8.0)]),
            fill("A-HATCH", vec![(-50.0, -50.0), (50.0, -50.0), (50.0, 50.0), (-50.0, 50.0)]),
        ];
        (ds, 1)
    }

    fn picked(ds: &Drawings) -> (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>) {
        let p = ds.pick.as_ref().map(|p| p.1.clone()).unwrap_or_default();
        (p.lines, p.arcs, p.texts, p.fills)
    }

    #[test]
    fn pick_box_window_whole_only() {
        let (mut ds, lv) = box_scene();
        let msg = ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        assert_eq!(picked(&ds), (vec![0], vec![0], vec![0], vec![0]));
        assert!(msg.contains('4'), "{msg}");
        // a reversed corner order gives the same box
        ds.pick_box(lv, v2(4.0, 4.0), v2(0.0, 0.0), BoxMode::Window, false, false);
        assert_eq!(picked(&ds), (vec![0], vec![0], vec![0], vec![0]));
        // a box that holds nothing whole clears the pick and says so
        let msg = ds.pick_box(lv, v2(10.0, 10.0), v2(11.0, 11.0), BoxMode::Window, false, false);
        assert!(ds.pick.is_none());
        assert_eq!(msg, crate::tr("data_export.dwgpick.none", &[]));
        // hidden layer: never picked
        hide(&mut ds, "A-WALL");
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        assert_eq!(picked(&ds), (vec![], vec![], vec![0], vec![0]));
        // another level: nothing
        let msg = ds.pick_box(lv + 1, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        assert_eq!(msg, crate::tr("data_export.dwgpick.none", &[]));
        assert!(ds.pick.is_none());
    }

    fn ring_scene() -> (Drawings, i64) {
        let (mut ds, lv) = box_scene();
        let u = &mut ds.list[0].u;
        let poly = |layer: &str, pts: Vec<(f64, f64)>| dm::UPoly { layer: layer.into(), pts, circle: None };
        // closed: 0 inside the box, 1 straddling the right side, 2 outside,
        // 3 a big ring holding the whole box in its interior (outline never touched)
        u.closed = vec![
            poly("A-WALL", vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0)]),
            poly("A-WALL", vec![(3.0, 1.0), (6.0, 1.0), (6.0, 3.0), (3.0, 3.0)]),
            poly("A-WALL", vec![(8.0, 8.0), (9.0, 8.0), (9.0, 9.0), (8.0, 9.0)]),
            poly("A-WALL", vec![(-20.0, -20.0), (20.0, -20.0), (20.0, 20.0), (-20.0, 20.0)]),
        ];
        (ds, lv)
    }

    fn rings(ds: &Drawings) -> Vec<usize> {
        ds.pick.as_ref().map(|p| p.1.closed.clone()).unwrap_or_default()
    }

    #[test]
    fn box_window_picks_closed_ring() {
        let (mut ds, lv) = ring_scene();
        let msg = ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        assert_eq!(rings(&ds), vec![0]);
        // 4 primitives of the old scene plus the ring; the count names the rings
        assert_eq!(msg, crate::tr("data_export.dwgpick.box_rings", &[("n", "5"), ("rings", "1")]));
        // a window that holds only part of a ring does not take it
        ds.pick_box(lv, v2(0.0, 0.0), v2(1.5, 4.0), BoxMode::Window, false, false);
        assert!(rings(&ds).is_empty());
        // a ring on a hidden layer is never picked
        hide(&mut ds, "A-WALL");
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        assert!(rings(&ds).is_empty());
    }

    #[test]
    fn box_crossing_picks_ring_by_outline() {
        let (mut ds, lv) = ring_scene();
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Crossing, false, false);
        // 0 inside, 1 straddling; 3 holds the box in its interior only: not touched
        assert_eq!(rings(&ds), vec![0, 1]);
        // Shift adds rings, Ctrl removes them
        ds.pick_box(lv, v2(7.5, 7.5), v2(9.5, 9.5), BoxMode::Crossing, true, false);
        assert_eq!(rings(&ds), vec![0, 1, 2]);
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Crossing, false, true);
        assert_eq!(rings(&ds), vec![2]);
        // a click next to an orphan ring outline picks it
        let (k, one) = ds.pick_at(lv, v2(8.5, 8.01), 0.1).unwrap();
        assert_eq!((k, one.closed), (0, vec![2]));
    }

    #[test]
    fn box_delete_removes_ring_one_undo() {
        use crate::editor::Editor;
        let mut ed = Editor::new_in_memory().unwrap();
        let (mut ds, lv) = ring_scene();
        let (rings0, lines0) = (ds.list[0].u.closed.len(), ds.list[0].u.lines.len());
        let n0 = ed.undo_len();
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        ds.delete_pick();
        ds.sync_undo(&mut ed);
        assert_eq!(ed.undo_len(), n0 + 1, "one delete, one step");
        assert_eq!(ds.list[0].u.closed.len(), rings0 - 1);
        assert!(ds.list[0].u.lines.len() < lines0);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.closed.len(), rings0);
        assert_eq!(ds.list[0].u.lines.len(), lines0);
        ed.redo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.closed.len(), rings0 - 1);
    }

    #[test]
    fn ring_outside_box_untouched() {
        let (mut ds, lv) = ring_scene();
        let before = ds.list[0].u.closed.clone();
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Crossing, false, false);
        ds.xf = (1.0, 0.0, 0.0);
        ds.apply_xf(false, false);
        let after = &ds.list[0].u.closed;
        // rings 0 and 1 moved with the pick; 2 and 3 are identical
        assert_ne!(after[0].pts, before[0].pts);
        assert_ne!(after[1].pts, before[1].pts);
        assert_eq!(after[2].pts, before[2].pts);
        assert_eq!(after[3].pts, before[3].pts);
        // a box far from every ring picks none and changes nothing
        let b = ds.list[0].u.closed.clone();
        ds.pick_box(lv, v2(60.0, 60.0), v2(61.0, 61.0), BoxMode::Crossing, false, false);
        assert!(ds.pick.is_none());
        assert_eq!(ds.list[0].u.closed, b);
    }

    #[test]
    fn pick_box_crossing_touching() {
        let (mut ds, lv) = box_scene();
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Crossing, false, false);
        // lines 0, 1, 3 (touches the top side), 4 (passes through); not 2
        assert_eq!(picked(&ds).0, vec![0, 1, 3, 4]);
        // arcs 0 and 1 (dips into the box); not the big circle
        assert_eq!(picked(&ds).1, vec![0, 1]);
        assert_eq!(picked(&ds).2, vec![0, 1]);
        // fills 0, 1 (corner overlap) and 2 (the box lies in its interior, no edge touched)
        assert_eq!(picked(&ds).3, vec![0, 1, 2]);
        // a box in the hollow of the arc, clear of every outline, touches no arc
        ds.pick_box(lv, v2(1.9, 1.9), v2(2.1, 2.1), BoxMode::Crossing, false, false);
        assert!(picked(&ds).1.is_empty(), "{:?}", picked(&ds));
        // the same small box as a window holds nothing whole
        ds.pick_box(lv, v2(1.9, 1.9), v2(2.1, 2.1), BoxMode::Window, false, false);
        assert!(ds.pick.is_none());
        // the drag direction decides the mode
        assert_eq!(BoxMode::from_drag(0.0, 5.0), BoxMode::Window);
        assert_eq!(BoxMode::from_drag(5.0, 0.0), BoxMode::Crossing);
    }

    #[test]
    fn pick_box_shift_adds_ctrl_removes() {
        let (mut ds, lv) = box_scene();
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        let first = picked(&ds);
        // Shift: the crossing box adds line 1 and keeps the rest
        ds.pick_box(lv, v2(2.5, 1.5), v2(4.0, 2.5), BoxMode::Crossing, true, false);
        let both = picked(&ds);
        assert_eq!(both.0, vec![0, 1]);
        assert_eq!(both.1, first.1);
        // adding the same again changes nothing
        ds.pick_box(lv, v2(2.5, 1.5), v2(4.0, 2.5), BoxMode::Crossing, true, false);
        assert_eq!(picked(&ds), both);
        // Ctrl: subtract what that box touches, line 0 stays
        ds.pick_box(lv, v2(2.5, 1.5), v2(4.0, 2.5), BoxMode::Crossing, false, true);
        assert_eq!(picked(&ds).0, vec![0]);
        // Ctrl with an empty box keeps the pick; a plain empty box clears it (outside fill 2)
        ds.pick_box(lv, v2(60.0, 60.0), v2(61.0, 61.0), BoxMode::Crossing, false, true);
        assert!(ds.pick.is_some());
        ds.pick_box(lv, v2(60.0, 60.0), v2(61.0, 61.0), BoxMode::Crossing, false, false);
        assert!(ds.pick.is_none());
        // removing everything leaves no pick
        ds.pick_box(lv, v2(0.0, 0.0), v2(4.0, 4.0), BoxMode::Window, false, false);
        ds.pick_box(lv, v2(-1.0, -1.0), v2(5.0, 5.0), BoxMode::Window, false, true);
        assert!(ds.pick.is_none());
    }

    #[test]
    fn double_click_picks_layer() {
        let (mut ds, lv) = box_scene();
        ds.list[0].u.lines.push(ln("A-WALL", (30.0, 30.0), (31.0, 30.0)));
        let msg = ds.pick_layer(lv, v2(1.5, 1.0), 0.01, false, false);
        let p = picked(&ds);
        assert_eq!(p.0, vec![0, 1, 2, 5]);
        assert_eq!(p.1, vec![0, 1, 2]);
        assert!(p.2.is_empty() && p.3.is_empty());
        assert!(msg.contains("A-WALL") && msg.contains('7'), "{msg}");
        // Ctrl removes the layer again, Shift adds another
        ds.pick_layer(lv, v2(1.5, 1.0), 0.01, false, true);
        assert!(ds.pick.is_none());
        ds.pick_layer(lv, v2(1.5, 1.0), 0.01, false, false);
        ds.pick_layer(lv, v2(3.5, 4.0), 0.01, true, false);
        assert_eq!(picked(&ds).0, vec![0, 1, 2, 5, 3, 4]);
        // a click on nothing clears
        ds.pick_layer(lv, v2(100.0, 100.0), 0.01, false, false);
        assert!(ds.pick.is_none());
        // a hidden layer is not picked
        hide(&mut ds, "A-DOOR");
        ds.pick_layer(lv, v2(1.5, 1.0), 0.01, false, false);
        assert_eq!(picked(&ds).0, vec![0, 1, 2, 5]);
    }

    #[test]
    fn box_pick_then_delete_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let mut ds = Drawings::default();
        ds.add(&dm::fixture::sample_plan(0), "p", None, lv, None, Origin::AsIs, v2(0.0, 0.0));
        let (n, na) = (ds.list[0].u.lines.len(), ds.list[0].u.arcs.len());
        let (lo, hi) = ds.list[0].u.bbox().unwrap();
        let (lo, hi) = (v2(lo.0 - 1.0, lo.1 - 1.0), v2(hi.0 + 1.0, hi.1 + 1.0));
        ds.pick_box(lv, hi, lo, BoxMode::Crossing, false, false);
        let k = ds.pick.as_ref().unwrap().1.lines.len();
        assert!(k > 1, "the box takes several lines");
        let steps = ed.undo_len();
        ds.delete_pick();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.lines.len(), n - k);
        assert_eq!(ed.undo_len(), steps + 1, "one box delete, one undo step");
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!((ds.list[0].u.lines.len(), ds.list[0].u.arcs.len()), (n, na));
        assert_eq!(ed.undo_len(), steps);
    }

    #[test]
    fn rules_write_atomic_no_tmp_left() {
        let d = std::env::temp_dir().join(format!("ddc-rules-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("block_rules.tsv");
        let text = "DOOR*\tdoor\tFam\t0.9\t2.1\t\n";
        assert!(rules_text_ok(text));
        crate::safe_file::write_atomic(&f, text.as_bytes()).unwrap();
        crate::safe_file::write_atomic(&f, text.as_bytes()).unwrap();
        assert!(!d.join("block_rules.tsv.tmp").exists());
        assert!(d.join("block_rules.tsv.bak").exists());
        let got = crate::safe_file::read_checked(&f, &rules_text_ok).0.unwrap();
        assert_eq!(BlockRules::from_text(&got).0.len(), 1);
        // torn: no trailing newline or an unparseable line is rejected; comment-only is fine
        assert!(!rules_text_ok("DOOR*\tdoor\tFam"));
        assert!(!rules_text_ok("garbage\n"));
        assert!(rules_text_ok("# no rules\n"));
    }

    /// Two files in different units: one $INSUNITS, the second drawing scaled to it.
    #[test]
    fn export_mixed_units_one_scale() {
        let src = dm::fixture::sample_plan(0);
        let (lines0, _) = grid_of(&src);
        let mut ds = Drawings::default();
        ds.add(&src, "mm", None, 1, Some(0.001), Origin::AsIs, v2(0.0, 0.0));
        ds.add(&src, "m", None, 1, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        assert!((doc.unit_meters().unwrap() - 0.001).abs() < 1e-12);
        let (lines, _) = grid_of(&doc);
        assert_eq!(lines.len(), 2 * lines0.len());
        let big: Vec<[f64; 4]> = lines0.iter().map(|x| x.map(|v| v * 1000.0)).collect();
        assert!(big.iter().all(|x| has_line(&lines, x)), "metre drawing written in mm");
    }

    fn far_plan(shift: (f64, f64)) -> Underlay {
        let doc = dm::fixture::sample_plan(0);
        let mut u = Underlay::from_dxf(&doc, 0.001);
        u.translate(shift);
        u
    }

    #[test]
    fn far_drawing_moves_to_origin_and_keeps_offset() {
        let u = far_plan((1200.0, -3000.0));
        let (lo0, _) = u.bbox().unwrap();
        let extra = placement_extra(&u, Origin::AsIs, true, v2(0.0, 0.0));
        let mut ds = Drawings::default();
        ds.add_underlay(u, 0.001, "far", None, 1, Origin::AsIs, extra);
        let (lo, hi) = ds.fit_bbox(1).unwrap();
        assert!(lo.x.abs() < 1.0 && lo.y.abs() < 1.0 && hi.x < FAR_M && hi.y < FAR_M);
        let d = &ds.list[0];
        assert!((d.offset.x + lo0.0).abs() < 1e-9 && (d.offset.y + lo0.1).abs() < 1e-9);
    }

    #[test]
    fn far_offset_round_trips_through_export() {
        let src = dm::fixture::sample_plan(0);
        let (lines0, _) = grid_of(&src);
        let u = far_plan((1200.0, -3000.0));
        let extra = placement_extra(&u, Origin::AsIs, true, v2(0.0, 0.0));
        assert!(extra.len() > 1000.0);
        let mut ds = Drawings::default();
        ds.add_underlay(u, 0.001, "far", None, 1, Origin::AsIs, extra);
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        let (lines, _) = grid_of(&doc);
        assert_eq!(lines.len(), lines0.len());
        // exported back in the file's own far coordinates (millimetres)
        let moved: Vec<[f64; 4]> = lines0
            .iter()
            .map(|x| [x[0] + 1.2e6, x[1] - 3.0e6, x[2] + 1.2e6, x[3] - 3.0e6])
            .collect();
        assert!(moved.iter().all(|x| has_line(&lines, x)));
    }

    #[test]
    fn near_drawing_and_off_switch_unchanged() {
        let u = far_plan((3.0, 2.0));
        let e = v2(0.5, 0.25);
        assert_eq!(placement_extra(&u, Origin::AsIs, true, e), e);
        let far = far_plan((1200.0, -3000.0));
        assert_eq!(placement_extra(&far, Origin::AsIs, false, e), e);
        assert_eq!(placement_extra(&far, Origin::MinToZero, true, e), e);
    }

    #[test]
    fn fit_ignores_stray_outlier() {
        let mut u = far_plan((0.0, 0.0));
        let l = u.lines[0].clone();
        let mut stray = l.clone();
        stray.a = (900_000.0, -900_000.0);
        stray.b = (900_001.0, -900_000.0);
        u.lines.extend(std::iter::repeat(l).take(60));
        u.lines.push(stray);
        let (lo, hi) = robust_bbox(&u).unwrap();
        assert!(hi.x < 1000.0 && lo.y > -1000.0);
    }

    #[test]
    fn one_mm_line_precision_survives_far_placement() {
        // the whole plan is far; a lone far line would not move it (fit ignores strays)
        let mut u = far_plan((3000.0, -3000.0));
        u.lines[0].a = (3001.0, -2999.0);
        u.lines[0].b = (3001.001, -2999.0);
        let extra = placement_extra(&u, Origin::AsIs, true, v2(0.0, 0.0));
        let mut ds = Drawings::default();
        ds.add_underlay(u, 0.001, "far", None, 1, Origin::AsIs, extra);
        let l = &ds.list[0].u.lines[0];
        let len = ((l.b.0 - l.a.0).powi(2) + (l.b.1 - l.a.1).powi(2)).sqrt();
        assert!((len - 0.001).abs() < 1e-6, "{len}");
        assert!(l.a.0.abs() < FAR_M && (l.a.0 as f32) != (l.b.0 as f32));
    }
}
