// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Outline sketch mode (floor, roof, ceiling, shaft, opening) with no UI dependencies in
//! the logic: chained segments, three-point arc, ellipse (center, axis, second semi-axis), spline
//! (points, Enter), wall selection with offset, slope arrow, "defines slope" on roof edges.
//! Validation is live: open ends, branches and intersections are highlighted immediately. "Done" is
//! a single `apply_ext` journal entry (one undo step), "Cancel" leaves the model untouched.

use crate::editor::Editor;
use crate::tools::Rec;
use crate::ui_kit::Tokens;
use crate::wall_tool::{Event, Key};
use ddc_geom::sketch::{walls_loop, Sketch, SketchCurve, SketchIssue, WallsLoopError};
use ddc_geom::slope::Plane;
use ddc_geom::{v2, V2};
use ddc_model::{RoofKind, SketchHost, SKETCH_SAG};
use egui::{self, Pos2, Rect, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Draw {
    Line,
    Arc3,
    Ellipse,
    Spline,
    PickWalls,
    SlopeArrow,
}

impl Draw {
    /// Status-line hint key (`hint.sketch.<key>`).
    pub fn key(self) -> &'static str {
        match self {
            Draw::Line => "line",
            Draw::Arc3 => "arc3",
            Draw::Ellipse => "ellipse",
            Draw::Spline => "spline",
            Draw::PickWalls => "pick",
            Draw::SlopeArrow => "arrow",
        }
    }
    pub fn label(self) -> String {
        self.label_in(crate::ui_is_ru())
    }

    pub fn label_in(self, ru: bool) -> String {
        let k = match self {
            Draw::Line => "sketch_edit.sk_line",
            Draw::Arc3 => "sketch_edit.sk_arc3",
            Draw::Ellipse => "sketch_edit.sk_ellipse",
            Draw::Spline => "sketch_edit.sk_spline",
            Draw::PickWalls => "sketch_edit.sk_pick",
            Draw::SlopeArrow => "sketch_edit.sk_slope",
        };
        crate::tr_lang(ru, k, &[])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Finish,
    Cancel,
}

/// Why the selected walls produced no outline; `wall` is the wall id by its position in the selection.
pub fn walls_error(e: WallsLoopError, wall: &dyn Fn(usize) -> i64) -> String {
    use crate::tr;
    let at = |p: V2| format!("({:.2}; {:.2})", p.x, p.y);
    match e {
        WallsLoopError::TooFew => tr("tool.err.walls_few", &[]),
        WallsLoopError::Open(p) => tr("tool.err.walls_open", &[("at", at(p).as_str())]),
        WallsLoopError::Branch(p) => tr("tool.err.walls_branch", &[("at", at(p).as_str())]),
        WallsLoopError::Stray(i) => tr(
            "tool.err.walls_stray",
            &[("id", wall(i).to_string().as_str())],
        ),
        WallsLoopError::OffsetFlips => tr("tool.err.walls_flips", &[]),
    }
}

#[derive(Clone, Debug)]
pub struct SketchMode {
    pub host: SketchHost,
    pub sketch: Sketch,
    pub draw: Draw,
    pub pending: Vec<V2>,
    pub cursor: Option<V2>,
    /// Outline offset from the axes of the selected walls (+ outward).
    pub offset: f64,
    pub picked: Vec<i64>,
    /// Slope arrow (tail, head) and the rise over its length.
    pub arrow: Option<(V2, V2)>,
    pub rise: f64,
    /// Roof: whether a curve defines slope and its angle, by curve index.
    pub defines: Vec<bool>,
    pub slope_deg: f64,
    pub issues: Vec<SketchIssue>,
    pub error: Option<String>,
    /// Capture radius in world units.
    pub radius: f64,
    /// New opening in a host (slab/roof/ceiling; flag — dormer): "Done" creates it.
    pub new_opening: Option<(ddc_model::OpeningHost, bool)>,
    /// New floor: "Done" creates it with this outline.
    pub new_slab: Option<NewSlab>,
    /// Wall links of edges picked in this session (`boundary_link_ui`).
    pub links: Vec<ddc_model::EdgeLink>,
    /// Links the host had when the sketch opened, and edges unlinked since.
    pub linked: Vec<ddc_model::LinkedEdge>,
    pub release: Vec<usize>,
    /// «Whole loop»: one click picks the closed run of walls; `repick` — lost edge to re-link.
    pub loop_pick: bool,
    pub repick: Option<usize>,
}

/// Parameters of the floor created from the sketch.
#[derive(Clone, Debug, PartialEq)]
pub struct NewSlab {
    pub level: i64,
    pub type_name: String,
    pub offset: f64,
    /// Slope gradient from the options (gx, gy); the sketch arrow takes precedence.
    pub slope: Option<(f64, f64)>,
}

impl SketchMode {
    pub fn new(host: SketchHost, existing: Option<Sketch>) -> Self {
        let mut s = Self {
            host,
            sketch: existing.unwrap_or_default(),
            draw: Draw::Line,
            pending: vec![],
            cursor: None,
            offset: 0.0,
            picked: vec![],
            arrow: None,
            rise: 0.0,
            defines: vec![],
            slope_deg: 30.0,
            issues: vec![],
            error: None,
            radius: 0.2,
            new_opening: None,
            new_slab: None,
            links: vec![],
            linked: vec![],
            release: vec![],
            loop_pick: false,
            repick: None,
        };
        s.revalidate();
        s
    }

    /// Start from a rectangular outline (a new element or existing rings).
    pub fn from_ring(host: SketchHost, ring: &[V2]) -> Self {
        let n = ring.len();
        let sk = Sketch {
            curves: (0..n)
                .map(|i| SketchCurve::Line {
                    a: ring[i],
                    b: ring[(i + 1) % n],
                })
                .collect(),
        };
        Self::new(host, Some(sk))
    }

    pub fn set_draw(&mut self, d: Draw) {
        self.draw = d;
        self.pending.clear();
    }

    fn push(&mut self, c: SketchCurve) {
        self.sketch.curves.push(c);
        self.defines.resize(self.sketch.curves.len(), true);
        self.revalidate();
    }

    pub fn revalidate(&mut self) {
        self.defines.resize(self.sketch.curves.len(), true);
        self.issues = match self.sketch.validate() {
            Ok(_) => vec![],
            Err(e) => e,
        };
    }

    /// Indices of curves with errors (for highlighting).
    pub fn bad_curves(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.issues.iter().flat_map(|i| i.curves()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Sketch curve nearest to the point.
    pub fn curve_near(&self, p: V2) -> Option<usize> {
        self.sketch
            .curves
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut pts = c.tessellate(1e-3);
                pts.push(c.end());
                let d = pts
                    .windows(2)
                    .map(|w| seg_dist(p, w[0], w[1]))
                    .fold(f64::MAX, f64::min);
                (i, d)
            })
            .filter(|&(_, d)| d <= self.radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Input. `walls` — axes of the level's straight walls (id, a, b) for "Pick walls".
    pub fn handle(&mut self, e: Event, walls: &[(i64, V2, V2)]) -> Action {
        self.error = None;
        match e {
            Event::Move(p) => {
                self.cursor = Some(self.snap(p));
                Action::None
            }
            Event::Click(p) => {
                let p = self.snap(p);
                self.click(p, walls);
                Action::None
            }
            Event::Key(Key::Escape) => {
                if self.pending.is_empty() && self.picked.is_empty() {
                    Action::Cancel
                } else {
                    self.pending.clear();
                    self.picked.clear();
                    Action::None
                }
            }
            Event::Key(Key::Backspace) => {
                if self.pending.pop().is_none() {
                    self.sketch.curves.pop();
                    let n = self.sketch.curves.len();
                    self.links.retain(|l| l.edge < n);
                    self.revalidate();
                }
                Action::None
            }
            Event::Key(Key::Arc) => {
                self.set_draw(if self.draw == Draw::Arc3 {
                    Draw::Line
                } else {
                    Draw::Arc3
                });
                Action::None
            }
            Event::Char(' ') => {
                // "defines slope" on the edge under the cursor (roof)
                if let Some(i) = self.cursor.and_then(|c| self.curve_near(c)) {
                    self.defines[i] = !self.defines[i];
                }
                Action::None
            }
            Event::Key(Key::Enter) => self.enter(walls),
            _ => Action::None,
        }
    }

    /// Snap to sketch curve endpoints (closes the outline without user tolerances).
    fn snap(&self, p: V2) -> V2 {
        self.sketch
            .curves
            .iter()
            .flat_map(|c| [c.start(), c.end()])
            .chain(self.pending.first().copied())
            .filter(|q| (*q - p).len() <= self.radius)
            .min_by(|a, b| (*a - p).len().total_cmp(&(*b - p).len()))
            .unwrap_or(p)
    }

    fn click(&mut self, p: V2, walls: &[(i64, V2, V2)]) {
        match self.draw {
            Draw::Line => {
                if let Some(&a) = self.pending.last() {
                    if (p - a).len() > 1e-9 {
                        self.push(SketchCurve::Line { a, b: p });
                    }
                    // closed the chain or the whole outline — the chain is finished
                    if (p - self.pending[0]).len() <= 1e-9 || self.issues.is_empty() {
                        self.pending.clear();
                        return;
                    }
                    self.pending = vec![self.pending[0], p];
                } else {
                    self.pending = vec![p];
                }
            }
            Draw::Arc3 => {
                self.pending.push(p);
                if self.pending.len() == 3 {
                    let (a, b, m) = (self.pending[0], self.pending[1], self.pending[2]);
                    match SketchCurve::arc3(a, m, b) {
                        Some(c) => self.push(c),
                        None => self.error = Some(crate::tr("sketch_edit.sk_collinear", &[])),
                    }
                    self.pending.clear();
                }
            }
            Draw::Ellipse => {
                self.pending.push(p);
                if self.pending.len() == 3 {
                    let (c, ax, q) = (self.pending[0], self.pending[1], self.pending[2]);
                    let d = ax - c;
                    let rx = d.len();
                    let ry = if rx > 0.0 {
                        (q - c).cross(d).abs() / rx
                    } else {
                        0.0
                    };
                    if rx > 1e-6 && ry > 1e-6 {
                        self.push(SketchCurve::Ellipse {
                            c,
                            rx,
                            ry,
                            rot: d.y.atan2(d.x),
                            t0: 0.0,
                            sweep: std::f64::consts::TAU,
                        });
                    } else {
                        self.error = Some(crate::tr("sketch_edit.sk_ellipse_bad", &[]));
                    }
                    self.pending.clear();
                }
            }
            Draw::Spline => self.pending.push(p),
            Draw::PickWalls if self.repick.is_some() => {
                if let Err(k) = crate::boundary_link_ui::repick(self, p, walls) {
                    self.error = Some(crate::tr(k, &[]));
                }
            }
            Draw::PickWalls if self.loop_pick => {
                self.picked = crate::boundary_link_ui::loop_at(p, self.radius, walls);
                if self.picked.is_empty() {
                    self.error = Some(crate::tr("boundary_link.no_wall", &[]));
                } else {
                    self.commit_picked(walls);
                }
            }
            Draw::PickWalls => {
                let hit = walls
                    .iter()
                    .map(|&(id, a, b)| (id, seg_dist(p, a, b)))
                    .filter(|&(_, d)| d <= self.radius)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((id, _)) = hit {
                    if let Some(k) = self.picked.iter().position(|&w| w == id) {
                        self.picked.remove(k);
                    } else {
                        self.picked.push(id);
                    }
                }
            }
            Draw::SlopeArrow => {
                self.pending.push(p);
                if self.pending.len() == 2 {
                    self.arrow = Some((self.pending[0], self.pending[1]));
                    self.pending.clear();
                }
            }
        }
    }

    fn enter(&mut self, walls: &[(i64, V2, V2)]) -> Action {
        match self.draw {
            Draw::Spline if !self.pending.is_empty() => {
                if self.pending.len() >= 2 {
                    let ctrl = std::mem::take(&mut self.pending);
                    self.push(SketchCurve::Spline { ctrl });
                }
                self.pending.clear();
                Action::None
            }
            Draw::PickWalls if !self.picked.is_empty() => {
                self.commit_picked(walls);
                Action::None
            }
            _ if !self.pending.is_empty() => {
                self.pending.clear();
                Action::None
            }
            _ => Action::Finish,
        }
    }

    /// Contour from the picked walls; its edges stay linked to them (follow wall moves).
    fn commit_picked(&mut self, walls: &[(i64, V2, V2)]) {
        let axes: Vec<(V2, V2)> = self
            .picked
            .iter()
            .filter_map(|id| walls.iter().find(|w| w.0 == *id).map(|w| (w.1, w.2)))
            .collect();
        match walls_loop(&axes, self.offset) {
            Ok(cs) => {
                let base = self.sketch.curves.len();
                let ls = crate::boundary_link_ui::links_for(&cs, base, &self.picked, walls);
                self.links.extend(ls);
                for c in cs {
                    self.push(c);
                }
                self.picked.clear();
            }
            Err(e) => {
                let picked = &self.picked;
                let text = walls_error(e, &|i| picked.get(i).copied().unwrap_or(0));
                self.error = Some(text);
            }
        }
    }

    /// Error text for the status line.
    pub fn issue_text(&self) -> Option<String> {
        self.issue_text_in(crate::ui_is_ru())
    }

    /// An untouched sketch is not an error: `Empty` is reported only by [`Self::record`] (Finish).
    pub fn issue_text_in(&self, ru: bool) -> Option<String> {
        if self.sketch.curves.is_empty() {
            return None;
        }
        let first = self.issues.first()?;
        let k = match first {
            SketchIssue::Empty => "sketch_edit.sk_empty",
            SketchIssue::Degenerate { .. } => "sketch_edit.sk_short",
            SketchIssue::OpenEnd { .. } => "sketch_edit.sk_open",
            SketchIssue::Branch { .. } => "sketch_edit.sk_branch",
            SketchIssue::Intersect { .. } => "sketch_edit.sk_cross",
            SketchIssue::Nested { .. } => "sketch_edit.sk_nested",
        };
        let what = crate::tr_lang(ru, k, &[]);
        Some(match self.issues.len() {
            1 => what,
            n => crate::tr_lang(
                ru,
                "sketch_edit.sk_more",
                &[("what", &what), ("n", &(n - 1).to_string())],
            ),
        })
    }

    /// Write to the model as a single undo operation. On sketch errors we stay in the mode.
    pub fn record(&self, ed: &Editor) -> Result<Rec, String> {
        let regions = self
            .sketch
            .validate()
            .map_err(|_| {
                self.issue_text()
                    .unwrap_or_else(|| crate::tr("sketch_edit.sk_empty", &[]))
            })?;
        let host = self.host;
        let sk = self.sketch.clone();
        let (links, release) = (self.links.clone(), self.release.clone());
        let plane = self.arrow.and_then(|(t, h)| Plane::arrow(t, h, self.rise));
        if let Some(ns) = self.new_slab.clone() {
            let ring = regions[0].outer.ring(SKETCH_SAG);
            let pairs = |r: &[V2]| r.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>();
            let outer = pairs(&ring);
            let holes: Vec<Vec<(f64, f64)>> = regions[0]
                .holes
                .iter()
                .map(|h| pairs(&h.ring(SKETCH_SAG)))
                .collect();
            let slope = plane.or(ns
                .slope
                .and_then(|(gx, gy)| Plane::lowest_on(&ring, gx, gy)));
            return Ok(Rec::new("slab", move |m| {
                let (slab_type, n) = crate::tools::ensure_slab_type(m, &ns.type_name)?;
                let spec = ddc_model::SlabSpec {
                    level: ns.level,
                    slab_type,
                    offset: ns.offset,
                    outer: outer.clone(),
                    holes: holes.clone(),
                };
                let id = match m.add_slab(&spec) {
                    Ok(id) => id,
                    Err(e) => {
                        for _ in 0..n {
                            m.undo()?;
                        }
                        return Err(e);
                    }
                };
                let k = all_or_nothing(m, n + 1, |m| {
                    m.finish_sketch_links(SketchHost::Slab(id), &sk, &links, &[]).map(|_| ())
                })?;
                match slope {
                    Some(pl) => all_or_nothing(m, k, |m| m.set_slab_slope(id, Some(pl))),
                    None => Ok(k),
                }
            }));
        }
        if let Some((oh, dormer)) = self.new_opening {
            let oh = resolve_host(ed, oh, &regions[0].outer.ring(SKETCH_SAG))
                .ok_or_else(|| crate::tr("host_shape.sketch.no_host", &[]))?;
            let label = if dormer {
                "dormer opening"
            } else {
                "host opening"
            };
            return Ok(Rec::new(label, move |m| {
                m.add_host_opening(oh, &sk, dormer)?;
                Ok(1)
            }));
        }
        match host {
            SketchHost::Slab(id) => Ok(Rec::new("slab sketch", move |m| {
                m.finish_sketch_links(host, &sk, &links, &release)?;
                match plane {
                    Some(pl) => all_or_nothing(m, 1, |m| m.set_slab_slope(id, Some(pl))),
                    None => Ok(1),
                }
            })),
            SketchHost::Roof(id) => {
                let mut spec = ed.model.roof(id).map_err(|e| e.to_string())?;
                let outer = &regions[0].outer;
                // ring and slope of each link by the flag of the source curve
                let mut ring = vec![];
                let mut slopes = vec![];
                for (c, &src) in outer.curves.iter().zip(&outer.source) {
                    let pts = c.tessellate(SKETCH_SAG);
                    let s = self.defines.get(src).copied().unwrap_or(true);
                    for p in pts {
                        ring.push((p.x, p.y));
                        slopes.push(s.then_some(self.slope_deg));
                    }
                }
                let arrow = self.arrow.filter(|_| plane.is_some());
                if let Some((t, h)) = arrow {
                    // true plane from the arrow, not a pitch from the nearest edge
                    spec.kind = RoofKind::Arrow {
                        ring,
                        tail: (t.x, t.y),
                        head: (h.x, h.y),
                        rise: self.rise,
                    };
                } else if let Some(mansard) = match spec.kind {
                    RoofKind::Footprint { mansard, .. } => Some(mansard),
                    RoofKind::Arrow { .. } => Some(None),
                    RoofKind::Extrusion { .. } => None,
                } {
                    spec.kind = RoofKind::Footprint {
                        ring,
                        slopes,
                        mansard,
                    };
                } else {
                    return Err(crate::tr("sketch_edit.sk_roof_extrude", &[]));
                }
                Ok(Rec::new("roof sketch", move |m| {
                    m.finish_sketch_links(host, &sk, &links, &release)?;
                    all_or_nothing(m, 1, |m| m.update_roof(id, &spec))
                }))
            }
            _ => Ok(Rec::new("sketch", move |m| {
                m.finish_sketch_links(host, &sk, &links, &release)?;
                Ok(1)
            })),
        }
    }
}

/// "Done" is atomic: `done` operations are already written; if the next one fails they
/// are rolled back, nothing remains in the model, and the error propagates upward.
fn all_or_nothing(
    m: &mut ddc_model::Model,
    done: usize,
    next: impl FnOnce(&mut ddc_model::Model) -> Result<(), ddc_model::Error>,
) -> Result<usize, ddc_model::Error> {
    match next(m) {
        Ok(()) => Ok(done + 1),
        Err(e) => {
            for _ in 0..done {
                m.undo()?;
            }
            Err(e)
        }
    }
}

fn seg_dist(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.dot(d);
    let t = if l2 > 0.0 {
        ((p - a).dot(d) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - (a + d * t)).len()
}

fn straight_walls(ed: &Editor) -> Vec<(i64, V2, V2)> {
    ed.walls
        .iter()
        .filter(|w| w.bulge == 0.0)
        .map(|w| (w.id, w.a, w.b))
        .collect()
}

/// Enter the host sketch mode: the saved sketch or the floor's current outline.
pub fn begin(ed: &mut Editor, host: SketchHost) {
    let saved = ed.model.sketch_of(host).ok().flatten();
    let mut m = match (saved, host) {
        (Some(s), _) => SketchMode::new(host, Some(s)),
        (None, SketchHost::Slab(id)) => {
            let ring: Vec<V2> = ed
                .model
                .slab_rings(id)
                .ok()
                .and_then(|r| r.into_iter().next())
                .map(|r| r.into_iter().map(|(x, y)| v2(x, y)).collect())
                .unwrap_or_default();
            SketchMode::from_ring(host, &ring)
        }
        (None, _) => SketchMode::new(host, None),
    };
    m.radius = ed.tool.radius.max(1e-3);
    m.linked = ed.model.boundary_links(host).unwrap_or_default();
    ed.sketch = Some(m);
}

/// Host of an opening with id 0: the level element whose plan covers the outline center.
fn resolve_host(
    ed: &Editor,
    h: ddc_model::OpeningHost,
    ring: &[V2],
) -> Option<ddc_model::OpeningHost> {
    use ddc_model::OpeningHost as H;
    let (table, id) = match h {
        H::Slab(i) => ("slab", i),
        H::Roof(i) => ("roof", i),
        H::Ceiling(i) => ("ceiling", i),
    };
    if id != 0 {
        return Some(h);
    }
    // a point strictly inside the outline (for an L shape the middle vertex may lie outside it)
    let n = ring.len().max(1) as f64;
    let c = ddc_geom::rooms::interior_point(&[ring])
        .unwrap_or_else(|| ring.iter().fold(v2(0.0, 0.0), |a, &p| a + p) * (1.0 / n));
    let ids: Vec<i64> = {
        let mut st = ed
            .model
            .sql()
            .prepare(&format!(
                "SELECT id FROM {table} WHERE level=?1 ORDER BY id"
            ))
            .ok()?;
        let v = st
            .query_map([ed.level], |r| r.get(0))
            .ok()?
            .flatten()
            .collect();
        v
    };
    let inside = |pts: &[(f64, f64)]| {
        ddc_geom::poly::contains(&[pts.iter().map(|&(x, y)| v2(x, y)).collect()], c)
    };
    ids.into_iter().find_map(|i| {
        let hit = match h {
            H::Slab(_) => ed
                .model
                .slab_rings(i)
                .ok()?
                .first()
                .is_some_and(|r| inside(r)),
            H::Ceiling(_) => ed
                .model
                .ceiling_view_rings(i)
                .ok()?
                .first()
                .is_some_and(|r| inside(r)),
            H::Roof(_) => ed.model.roof_geometry(i).ok()?.faces.iter().any(|f| {
                let plan: Vec<V2> = f.pts.iter().map(|p| v2(p.x, p.y)).collect();
                f.sloped && ddc_geom::poly::contains(&[plan], c)
            }),
        };
        hit.then_some(match h {
            H::Slab(_) => H::Slab(i),
            H::Roof(_) => H::Roof(i),
            H::Ceiling(_) => H::Ceiling(i),
        })
    })
}

/// Sketch of a new opening in a host (the outline is a single loop; a dormer cuts only the pitches).
pub fn begin_opening(ed: &mut Editor, host: ddc_model::OpeningHost, dormer: bool) {
    let mut m = SketchMode::new(SketchHost::HostOpening(0), None);
    m.new_opening = Some((host, dormer));
    m.radius = ed.tool.radius.max(1e-3);
    ed.sketch = Some(m);
}

/// Sketch of a new floor: arcs, wall selection, slope arrow.
pub fn begin_slab(ed: &mut Editor, slab: NewSlab) {
    let mut m = SketchMode::new(SketchHost::Slab(0), None);
    m.new_slab = Some(slab);
    m.radius = ed.tool.radius.max(1e-3);
    ed.sketch = Some(m);
}

/// Entry from `Editor::handle`: true — the event was consumed by the sketch mode.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    let Some(mut m) = ed.sketch.take() else {
        return false;
    };
    let walls = straight_walls(ed);
    match m.handle(e, &walls) {
        Action::None => ed.sketch = Some(m),
        Action::Cancel => ed.status = Some(crate::tr("sketch_edit.sk_cancelled", &[])),
        Action::Finish => match m
            .record(ed)
            .and_then(|r| ed.apply_ext(r).map_err(|e| e.to_string()))
        {
            Ok(()) => ed.status = Some(crate::tr("sketch_edit.sk_applied", &[])),
            Err(msg) => {
                m.error = Some(msg.clone());
                ed.status = Some(msg);
                ed.sketch = Some(m);
            }
        },
    }
    true
}

/// Mode drawing over the plan: curves, errors, rubber band, arrow, mode bar.
pub fn paint(p: &egui::Painter, rect: Rect, ed: &Editor, t: &Tokens, to_s: &dyn Fn(V2) -> Pos2) {
    let bad_col = t.ov().error;
    let Some(m) = ed.sketch.as_ref() else { return };
    let bad = m.bad_curves();
    let px = |c: &SketchCurve| -> Vec<Pos2> {
        let mut v = c.tessellate(0.005);
        v.push(c.end());
        v.iter().map(|&q| to_s(q)).collect()
    };
    // fill of the completed region
    if m.issues.is_empty() {
        if let Ok(rs) = m.sketch.validate() {
            for r in &rs {
                // consistent triangulation with holes: the holes stay empty
                let rings = r.rings(0.01);
                if let Ok(sm) = ddc_geom::shape_edit::SlabShape::default().build(&rings) {
                    for tri in &sm.tris {
                        p.add(egui::Shape::convex_polygon(
                            tri.iter().map(|&k| to_s(sm.pts[k])).collect(),
                            t.accent.gamma_multiply(0.10),
                            Stroke::NONE,
                        ));
                    }
                }
            }
        }
    }
    for (i, c) in m.sketch.curves.iter().enumerate() {
        let is_bad = bad.binary_search(&i).is_ok();
        let flat =
            matches!(m.host, SketchHost::Roof(_)) && !m.defines.get(i).copied().unwrap_or(true);
        let col = if is_bad { bad_col } else { t.accent };
        let w = if is_bad { 3.0 } else { 2.0 };
        let pts = px(c);
        if flat {
            for s in pts.windows(2) {
                p.extend(egui::Shape::dashed_line(
                    &[s[0], s[1]],
                    Stroke::new(w, col),
                    8.0,
                    5.0,
                ));
            }
        } else {
            p.add(egui::Shape::line(pts.clone(), Stroke::new(w, col)));
        }
        // slope symbol on the roof edge
        if matches!(m.host, SketchHost::Roof(_)) && !flat {
            let mid = c.point(0.5);
            let d = c.velocity(0.5);
            let n = v2(-d.y, d.x) * (1.0 / d.len().max(1e-12));
            let a = to_s(mid);
            let b = to_s(mid + n * (0.6 * m.radius / 0.2).min(1.0));
            p.line_segment([a, b], Stroke::new(1.5, col));
            p.text(
                b,
                egui::Align2::CENTER_CENTER,
                format!("{:.0}°", m.slope_deg),
                egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
                col,
            );
        }
        // endpoints
        for q in [c.start(), c.end()] {
            p.circle_filled(to_s(q), 3.0, col);
        }
    }
    crate::boundary_link_ui::paint_links(p, m, t, to_s);
    for is in &m.issues {
        match is {
            SketchIssue::OpenEnd { at, .. } | SketchIssue::Branch { at, .. } => {
                p.circle_stroke(to_s(*at), 9.0, Stroke::new(2.0, bad_col));
            }
            SketchIssue::Intersect { at, .. } => {
                let c = to_s(*at);
                let k = 7.0;
                p.line_segment(
                    [c - Vec2::splat(k), c + Vec2::splat(k)],
                    Stroke::new(2.5, bad_col),
                );
                p.line_segment(
                    [c + Vec2::new(-k, k), c + Vec2::new(k, -k)],
                    Stroke::new(2.5, bad_col),
                );
            }
            _ => {}
        }
    }
    // selected walls
    for id in &m.picked {
        if let Some(w) = ed.walls.iter().find(|w| w.id == *id) {
            p.line_segment(
                [to_s(w.a), to_s(w.b)],
                Stroke::new(5.0, t.accent.gamma_multiply(0.5)),
            );
        }
    }
    // rubber band
    if let (Some(c), false) = (m.cursor, m.pending.is_empty()) {
        let mut pts: Vec<Pos2> = m.pending.iter().map(|&q| to_s(q)).collect();
        pts.push(to_s(c));
        match m.draw {
            Draw::Spline if !m.pending.is_empty() => {
                let mut ctrl = m.pending.clone();
                ctrl.push(c);
                if ctrl.len() >= 2 {
                    let s = SketchCurve::Spline { ctrl };
                    p.add(egui::Shape::line(px(&s), Stroke::new(1.5, t.ov().preview)));
                }
                for q in &pts {
                    p.circle_stroke(*q, 3.5, Stroke::new(1.0, t.text_dim));
                }
            }
            Draw::Arc3 if m.pending.len() == 2 => {
                if let Some(a) = SketchCurve::arc3(m.pending[0], c, m.pending[1]) {
                    p.add(egui::Shape::line(px(&a), Stroke::new(1.5, t.ov().preview)));
                }
            }
            _ => {
                for s in pts.windows(2) {
                    p.extend(egui::Shape::dashed_line(
                        &[s[0], s[1]],
                        Stroke::new(1.0, t.ov().preview),
                        6.0,
                        4.0,
                    ));
                }
            }
        }
    }
    // slope arrow
    if let Some((a, b)) = m.arrow {
        let (sa, sb) = (to_s(a), to_s(b));
        p.line_segment([sa, sb], Stroke::new(2.0, t.text));
        let d = (sb - sa).normalized();
        let nrm = Vec2::new(-d.y, d.x);
        p.add(egui::Shape::convex_polygon(
            vec![sb, sb - d * 12.0 + nrm * 5.0, sb - d * 12.0 - nrm * 5.0],
            t.text,
            Stroke::NONE,
        ));
        p.text(
            sa - nrm * 12.0,
            egui::Align2::CENTER_CENTER,
            crate::tr("sketch_edit.sk_tail", &[]),
            egui::FontId::proportional(ddc_ui::theme::type_size::CAPTION),
            t.text_dim,
        );
        let pct = 100.0 * m.rise / (b - a).len().max(1e-9);
        p.text(
            sb + nrm * 14.0,
            egui::Align2::CENTER_CENTER,
            crate::tr(
                "sketch_edit.sk_rise",
                &[
                    (
                        "rise",
                        &crate::unit_text::len(
                            m.rise,
                            crate::ui_is_ru(),
                            crate::units::active_units(),
                        ),
                    ),
                    ("pct", &format!("{pct:.1}")),
                ],
            ),
            egui::FontId::proportional(ddc_ui::theme::type_size::LABEL),
            t.text,
        );
    }
    // mode bar
    let (title, msg, bad) = bar_texts(m, crate::ui_is_ru(), crate::units::active_units());
    let bar = bar_rect(rect);
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
        title,
        f.clone(),
        t.text,
    );
    let col = if bad { bad_col } else { t.text_dim };
    p.text(
        bar.left_top() + Vec2::new(12.0, 32.0),
        egui::Align2::LEFT_TOP,
        msg,
        f,
        col,
    );
}

/// Mode bar at the top-left of the canvas; [`bar_buttons`] sits on its right edge.
pub fn bar_rect(rect: Rect) -> Rect {
    Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, 12.0),
        Vec2::new(rect.width().min(720.0) - 24.0, 58.0),
    )
}

/// Rects of the Finish / Cancel buttons inside the mode bar (right-aligned, vertically centred).
pub fn bar_button_rects(rect: Rect) -> (Rect, Rect) {
    let bar = bar_rect(rect);
    let size = Vec2::new(96.0, 28.0);
    let y = bar.center().y - size.y / 2.0;
    let cancel = Rect::from_min_size(egui::pos2(bar.right() - 10.0 - size.x, y), size);
    let finish = Rect::from_min_size(egui::pos2(cancel.left() - 8.0 - size.x, y), size);
    (finish, cancel)
}

/// Visible Finish / Cancel so the mode can be left without knowing Enter / Esc.
pub fn bar_buttons(ui: &mut egui::Ui, rect: Rect, ed: &mut Editor) {
    if ed.sketch.is_none() {
        return;
    }
    let (rf, rc) = bar_button_rects(rect);
    let finish = ui.put(
        rf,
        egui::Button::new(crate::tr("sketch_hints.sk_finish", &[])).fill(ui.visuals().selection.bg_fill),
    );
    let cancel = ui.put(rc, egui::Button::new(crate::tr("sketch_hints.sk_cancel", &[])));
    if finish.clicked() {
        finish_now(ed);
    } else if cancel.clicked() {
        cancel_now(ed);
    }
}

/// Enter until the mode records: first commits picked walls / an open spline, then finishes
/// (or stays with the error shown).
pub fn finish_now(ed: &mut Editor) {
    for _ in 0..3 {
        let busy = ed
            .sketch
            .as_ref()
            .is_some_and(|m| !m.pending.is_empty() || !m.picked.is_empty());
        handle(ed, Event::Key(Key::Enter));
        if !busy {
            break;
        }
    }
}

/// Leave the mode in one action, dropping drawn curves (Esc needs two presses mid-line).
pub fn cancel_now(ed: &mut Editor) {
    if ed.sketch.take().is_some() {
        ed.status = Some(crate::tr("sketch_edit.sk_cancelled", &[]));
    }
}

/// Mode bar texts: title, status line, error flag.
pub fn bar_texts(m: &SketchMode, ru: bool, u: crate::units::Units) -> (String, String, bool) {
    let host = match m.host {
        SketchHost::Slab(_) => "sketch_edit.sk_host_slab",
        SketchHost::Roof(_) => "sketch_edit.sk_host_roof",
        SketchHost::Ceiling(_) => "sketch_edit.sk_host_ceiling",
        SketchHost::Shaft(_) => "sketch_edit.sk_host_shaft",
        SketchHost::Opening(_) | SketchHost::HostOpening(_) => "sketch_edit.sk_host_opening",
    };
    let title = crate::tr_lang(
        ru,
        "sketch_edit.sk_hud",
        &[
            ("what", &crate::tr_lang(ru, host, &[])),
            ("draw", &m.draw.label_in(ru)),
            ("n", &m.sketch.curves.len().to_string()),
        ],
    );
    if let Some(e) = m.error.as_ref() {
        return (title, e.clone(), true);
    }
    if let Some(e) = m.issue_text_in(ru) {
        let msg = crate::tr_lang(ru, "sketch_edit.sk_err_line", &[("e", &e)]);
        return (title, msg, true);
    }
    if m.sketch.curves.is_empty() {
        return (title, crate::tr_lang(ru, "sketch_hints.sk_start", &[]), false);
    }
    let a: f64 = m
        .sketch
        .validate()
        .map(|rs| rs.iter().map(|r| r.area()).sum())
        .unwrap_or(0.0);
    let msg = crate::tr_lang(
        ru,
        "sketch_edit.sk_closed",
        &[("a", &crate::unit_text::area(a, ru, u))],
    );
    (title, msg, false)
}

/// Screenshot scenes `--tool-demo E1..E4` (sketch mode); None — not our scene.
pub fn demo(app: &mut crate::shell::EditorApp, id: &str) -> Option<Result<(), String>> {
    let lvl = app.ed.level;
    let slab = |app: &mut crate::shell::EditorApp, ring: Vec<(f64, f64)>| -> Result<i64, String> {
        let st = app
            .ed
            .model
            .add_slab_type(
                crate::tr("sketch_edit.seed_slab_200", &[]).as_str(),
                &[ddc_model::LayerSpec {
                    material: "concrete".into(),
                    thickness: 0.2,
                }],
            )
            .or_else(|_| {
                app.ed
                    .model
                    .sql()
                    .query_row("SELECT id FROM slab_type LIMIT 1", [], |r| r.get(0))
                    .map_err(ddc_model::Error::from)
            })
            .map_err(|e| e.to_string())?;
        let id = app
            .ed
            .model
            .add_slab(&ddc_model::SlabSpec {
                level: lvl,
                slab_type: st,
                offset: 0.0,
                outer: ring,
                holes: vec![],
            })
            .map_err(|e| e.to_string())?;
        app.ed.reload().map_err(|e| e.to_string())?;
        Ok(id)
    };
    let frame = |app: &mut crate::shell::EditorApp, c: V2, s: f64| {
        app.plan.center = c;
        app.plan.scale = s;
        app.ed.tool.radius = 12.0 / s;
    };
    let r = (|| -> Result<(), String> {
        match id {
            "E1" | "E2" => {
                let s = slab(app, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)])?;
                frame(app, v2(5.5, 3.2), 60.0);
                begin(&mut app.ed, SketchHost::Slab(s));
                let m = app.ed.sketch.as_mut().unwrap();
                // right end is an arc, the hole is an ellipse, the left edge is a spline
                m.sketch.curves = vec![
                    SketchCurve::Line {
                        a: v2(1.0, 0.0),
                        b: v2(9.0, 0.0),
                    },
                    SketchCurve::arc3(v2(9.0, 0.0), v2(12.0, 3.0), v2(9.0, 6.0)).unwrap(),
                    SketchCurve::Line {
                        a: v2(9.0, 6.0),
                        b: v2(1.0, 6.0),
                    },
                    SketchCurve::Spline {
                        ctrl: vec![v2(1.0, 6.0), v2(-0.5, 4.5), v2(1.5, 2.0), v2(1.0, 0.0)],
                    },
                    SketchCurve::Ellipse {
                        c: v2(5.0, 3.0),
                        rx: 1.6,
                        ry: 0.8,
                        rot: 0.35,
                        t0: 0.0,
                        sweep: std::f64::consts::TAU,
                    },
                ];
                if id == "E1" {
                    // errors: the top line falls short, the diagonal line crosses the hole
                    m.sketch.curves[2] = SketchCurve::Line {
                        a: v2(9.0, 6.0),
                        b: v2(1.6, 6.0),
                    };
                    m.sketch.curves.push(SketchCurve::Line {
                        a: v2(3.0, 1.0),
                        b: v2(7.5, 4.6),
                    });
                } else {
                    m.arrow = Some((v2(2.5, 3.0), v2(8.0, 3.0)));
                    m.rise = 0.05;
                }
                m.revalidate();
                m.cursor = Some(v2(3.0, 5.0));
            }
            "E4" => {
                app.ed.set_mode(crate::wall_tool::Mode::Select);
                let st = app
                    .ed
                    .model
                    .add_slab_type(
                        crate::tr("sketch_edit.seed_roofing", &[]).as_str(),
                        &[ddc_model::LayerSpec {
                            material: "roofing".into(),
                            thickness: 0.25,
                        }],
                    )
                    .map_err(|e| e.to_string())?;
                let roof = app
                    .ed
                    .model
                    .add_roof(&ddc_model::RoofSpec {
                        level: lvl,
                        slab_type: st,
                        offset: 3.0,
                        overhang: 0.0,
                        fascia: 0.0,
                        kind: RoofKind::Footprint {
                            ring: vec![(0.0, 0.0), (12.0, 0.0), (12.0, 8.0), (0.0, 8.0)],
                            slopes: vec![Some(30.0); 4],
                            mansard: None,
                        },
                    })
                    .map_err(|e| e.to_string())?;
                app.ed.reload().map_err(|e| e.to_string())?;
                frame(app, v2(6.0, 4.0), 52.0);
                let ring = [v2(0.0, 0.0), v2(12.0, 0.0), v2(12.0, 8.0), v2(0.0, 8.0)];
                let mut m = SketchMode::from_ring(SketchHost::Roof(roof), &ring);
                m.radius = app.ed.tool.radius;
                m.defines[1] = false;
                m.defines[3] = false;
                m.revalidate();
                app.ed.sketch = Some(m);
                app.ed.handle(Event::Move(v2(12.0, 4.0)));
            }
            _ => return Err(String::new()),
        }
        Ok(())
    })();
    match r {
        Err(e) if e.is_empty() => None,
        r => Some(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_closed_rectangle_with_line_chain_then_finish_is_one_undo() {
        let mut ed = Editor::new_in_memory().unwrap();
        let st = ed
            .model
            .add_slab_type(
                "s",
                &[ddc_model::LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let s = ed
            .model
            .add_slab(&ddc_model::SlabSpec {
                level: ed.level,
                slab_type: st,
                offset: 0.0,
                outer: vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
                holes: vec![],
            })
            .unwrap();
        ed.reload().unwrap();
        ed.sketch = Some(SketchMode::new(SketchHost::Slab(s), None));
        ed.sketch.as_mut().unwrap().radius = 0.05;
        for p in [v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 4.0), v2(0.0, 4.0)] {
            ed.handle(Event::Click(p));
        }
        // not closed: Enter ends the chain, a second Enter — "Done" is rejected
        ed.handle(Event::Key(Key::Enter));
        ed.handle(Event::Key(Key::Enter));
        let m = ed.sketch.as_ref().unwrap();
        assert_eq!(m.bad_curves(), vec![0, 2]);
        // two open ends: the text is "open (+1 more)", so match the head, not the whole line
        let err = m.error.as_deref().unwrap_or_default();
        assert!(
            [true, false].iter().any(|ru| err.starts_with(&crate::tr_lang(*ru, "sketch_edit.sk_open", &[]))),
            "{err}"
        );
        // closing with an endpoint snap (click nearby)
        ed.handle(Event::Click(v2(0.0, 4.0)));
        ed.handle(Event::Click(v2(0.02, 0.01)));
        assert!(ed.sketch.as_ref().unwrap().issues.is_empty());
        ed.handle(Event::Key(Key::Enter));
        assert!(ed.sketch.is_none());
        let (v, _) = ed.model.slab_shape_quantities(s).unwrap();
        assert!((v - 20.0 * 0.2).abs() < 1e-12);
        ed.undo();
        let (v, _) = ed.model.slab_shape_quantities(s).unwrap();
        assert!((v - 0.2).abs() < 1e-12, "{v}");
    }

    #[test]
    fn pick_walls_with_offset_and_cancel_leaves_model_untouched() {
        let mut ed = Editor::new_in_memory().unwrap();
        let walls = [
            (1, v2(0.0, 0.0), v2(6.0, 0.0)),
            (2, v2(6.0, 0.0), v2(6.0, 4.0)),
            (3, v2(6.0, 4.0), v2(0.0, 4.0)),
            (4, v2(0.0, 4.0), v2(0.0, 0.0)),
        ];
        let mut m = SketchMode::new(SketchHost::Shaft(1), None);
        m.radius = 0.1;
        m.set_draw(Draw::PickWalls);
        m.offset = -0.1;
        for p in [v2(3.0, 0.0), v2(6.0, 2.0), v2(3.0, 4.0), v2(0.0, 2.0)] {
            m.handle(Event::Click(p), &walls);
        }
        assert_eq!(m.picked, vec![1, 2, 3, 4]);
        assert_eq!(m.handle(Event::Key(Key::Enter), &walls), Action::None);
        let a = m.sketch.validate().unwrap()[0].area();
        assert!((a - 5.8 * 3.8).abs() < 1e-12);
        let before = ed
            .model
            .sql()
            .query_row("SELECT max(seq) FROM op", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap();
        ed.sketch = Some(m);
        ed.handle(Event::Key(Key::Escape));
        assert!(ed.sketch.is_none());
        let after = ed
            .model
            .sql()
            .query_row("SELECT max(seq) FROM op", [], |r| {
                r.get::<_, Option<i64>>(0)
            })
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn arc_ellipse_spline_input() {
        let mut m = SketchMode::new(SketchHost::Opening(1), None);
        m.radius = 1e-3;
        m.set_draw(Draw::Arc3);
        for p in [v2(0.0, 0.0), v2(4.0, 0.0), v2(2.0, -2.0)] {
            m.handle(Event::Click(p), &[]);
        }
        m.set_draw(Draw::Line);
        m.handle(Event::Click(v2(4.0, 0.0)), &[]);
        m.handle(Event::Click(v2(0.0, 0.0)), &[]);
        let a = m.sketch.validate().unwrap()[0].area();
        assert!((a - 2.0 * std::f64::consts::PI).abs() < 1e-12, "{a}");
        m.set_draw(Draw::Ellipse);
        for p in [v2(2.0, -1.0), v2(2.5, -1.0), v2(2.0, -0.8)] {
            m.handle(Event::Click(p), &[]);
        }
        let r = m.sketch.validate().unwrap();
        assert_eq!(r[0].holes.len(), 1);
        assert!(
            (r[0].area() - (2.0 * std::f64::consts::PI - std::f64::consts::PI * 0.5 * 0.2)).abs()
                < 1e-12
        );
        m.set_draw(Draw::Spline);
        for p in [v2(10.0, 0.0), v2(11.0, 1.0), v2(12.0, 0.0)] {
            m.handle(Event::Click(p), &[]);
        }
        m.handle(Event::Key(Key::Enter), &[]);
        assert!(matches!(
            m.sketch.curves.last(),
            Some(SketchCurve::Spline { .. })
        ));
        assert!(!m.issues.is_empty()); // open spline is highlighted
    }

    #[test]
    fn fresh_slab_sketch_shows_no_error() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lvl = ed.level;
        begin_slab(
            &mut ed,
            NewSlab {
                level: lvl,
                type_name: "s".into(),
                offset: 0.0,
                slope: None,
            },
        );
        let m = ed.sketch.as_ref().unwrap();
        assert_eq!(m.issue_text_in(true), None);
        let (_, msg, bad) = bar_texts(m, true, crate::units::Units::Metric);
        assert!(!bad);
        assert_eq!(msg, crate::tr_lang(true, "sketch_hints.sk_start", &[]));
        // Finish on an empty sketch still refuses, with the reason
        finish_now(&mut ed);
        let m = ed.sketch.as_ref().expect("stays in the mode");
        assert_eq!(m.error.as_deref(), Some(crate::tr("sketch_edit.sk_empty", &[]).as_str()));
        cancel_now(&mut ed);
        assert!(ed.sketch.is_none());
    }

    #[test]
    fn finish_button_records_a_drawn_outline() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lvl = ed.level;
        begin_slab(
            &mut ed,
            NewSlab {
                level: lvl,
                type_name: "s".into(),
                offset: 0.0,
                slope: None,
            },
        );
        ed.sketch.as_mut().unwrap().radius = 0.05;
        for p in [v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 4.0), v2(0.0, 4.0), v2(0.0, 0.0)] {
            ed.handle(Event::Click(p));
        }
        finish_now(&mut ed);
        assert!(ed.sketch.is_none(), "{:?}", ed.status);
        let n: i64 = ed
            .model
            .sql()
            .query_row("SELECT COUNT(*) FROM slab", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn bar_buttons_fit_inside_the_bar_without_overlap() {
        for w in [640.0, 900.0, 1400.0] {
            let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(w, 600.0));
            let (f, c) = bar_button_rects(rect);
            let bar = bar_rect(rect);
            assert!(bar.contains_rect(f) && bar.contains_rect(c));
            assert!(!f.intersects(c));
            // the text column (left ~60 %) stays clear
            assert!(f.left() > bar.left() + bar.width() * 0.5, "{w}");
        }
    }
}
