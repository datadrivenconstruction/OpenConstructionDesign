//! Grips on the one picked drawing primitive: line ends, closed polyline vertices, arc ends and
//! middle, circle quadrants, the text insert point. A drag snaps like the drawing tool and takes
//! a typed length; nothing changes until the drop, which is one undo step. Ctrl-drag on a
//! polyline edge inserts a vertex; Delete removes the selected vertex. Edges of a closed ring
//! move together with its vertices, so the ring stays closed.

use std::f64::consts::{PI, TAU};

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{Pick, UPoly, Underlay};
use egui::{self, Painter, Pos2, Stroke};

use super::super::history::Step;
use super::super::{arc_pts, seg_dist, Drawings};
use super::snap::{pick, Prim};
use super::{foot, tr, SAME};
use crate::overlay::{self, Palette, Plate};
use crate::snap::SnapKind;

/// Ends closer than this (m) are one vertex: the export's tolerance for polyline edges.
const MEET: f64 = 1e-6;

/// One grip of a picked primitive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grip {
    /// End `b` (else `a`) of a line that is no edge of a straight closed ring.
    LineEnd { line: usize, b: bool },
    /// Vertex `i` of a closed ring whose edges are all lines.
    Vertex { ring: usize, i: usize },
    /// A new vertex on edge `i` (vertex `i` to `i + 1`) of such a ring.
    Insert { ring: usize, i: usize },
    /// End `a1` (else `a0`) of an arc.
    ArcEnd { arc: usize, end: bool },
    ArcMid { arc: usize },
    /// Quadrant `q` (0 east, counter-clockwise) of a full circle.
    Quad { arc: usize, q: u8 },
    Text { text: usize },
}

impl Grip {
    /// Only vertices can be selected for Delete.
    fn deletable(self) -> bool {
        matches!(self, Grip::LineEnd { .. } | Grip::Vertex { .. })
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    k: usize,
    /// Generation of the drawing at the press: an edit or undo meanwhile cancels the drop.
    gen: u64,
    grip: Grip,
    /// Grip point at the press (model m).
    from: V2,
    /// Snapped cursor.
    at: V2,
    /// What the cursor snapped to.
    kind: SnapKind,
}

/// Grip drag, typed length and the vertex selected for Delete.
#[derive(Default)]
pub(super) struct GripUi {
    drag: Option<Drag>,
    /// (drawing, generation, grip) of the selected vertex.
    sel: Option<(usize, u64, Grip)>,
    typed: String,
    /// The primary button went down on a grip and is still held.
    hold: bool,
}

fn pv(q: (f64, f64)) -> V2 {
    v2(q.0, q.1)
}

fn same(a: V2, b: V2) -> bool {
    (a.x - b.x).abs() <= MEET && (a.y - b.y).abs() <= MEET
}

fn at(c: V2, r: f64, a: f64) -> V2 {
    v2(c.x + r * a.cos(), c.y + r * a.sin())
}

fn angle(c: V2, q: V2) -> f64 {
    (q.y - c.y).atan2(q.x - c.x)
}

fn full(a0: f64, a1: f64) -> bool {
    a1 - a0 >= TAU - 1e-9
}

/// Counter-clockwise turn from angle `x` to `y`, in [0, 2π).
fn ccw(x: f64, y: f64) -> f64 {
    (y - x).rem_euclid(TAU)
}

/// Arc through `s`, `m`, `e` in that order: (centre, radius, a0, a1) counter-clockwise.
/// `None` when the points are on one line or two of them coincide.
fn arc3(s: V2, m: V2, e: V2) -> Option<(V2, f64, f64, f64)> {
    let (b, c) = (m - s, e - s);
    if b.len() <= SAME || c.len() <= SAME || (e - m).len() <= SAME {
        return None;
    }
    let d = 2.0 * b.cross(c);
    if d.abs() <= 1e-9 * b.len() * c.len() {
        return None;
    }
    let (bb, cc) = (b.dot(b), c.dot(c));
    let ctr = s + v2((c.y * bb - b.y * cc) / d, (b.x * cc - c.x * bb) / d);
    let r = (s - ctr).len();
    if !(r.is_finite() && ctr.x.is_finite() && ctr.y.is_finite()) {
        return None;
    }
    let (ts, tm, te) = (angle(ctr, s), angle(ctr, m), angle(ctr, e));
    let (a0, a1) = if ccw(ts, tm) < ccw(ts, te) {
        (ts, ts + ccw(ts, te))
    } else {
        (te, te + ccw(te, ts))
    };
    Some((ctr, r, a0, a1))
}

/// Line for each edge `j` (vertex `j` to `j + 1`) of the straight closed ring `poly`, with
/// `true` when the line runs the other way. `None` when an edge is no line (an arc, a circle).
fn ring_edges(u: &Underlay, poly: &UPoly) -> Option<Vec<(usize, bool)>> {
    if poly.circle.is_some() || poly.pts.len() < 3 {
        return None;
    }
    let n = poly.pts.len();
    let mut out: Vec<(usize, bool)> = Vec::with_capacity(n);
    for j in 0..n {
        let (p, q) = (pv(poly.pts[j]), pv(poly.pts[(j + 1) % n]));
        let hit = u.lines.iter().enumerate().find_map(|(i, l)| {
            if l.layer != poly.layer || out.iter().any(|e| e.0 == i) {
                return None;
            }
            let (a, b) = (pv(l.a), pv(l.b));
            if same(a, p) && same(b, q) {
                Some((i, false))
            } else if same(a, q) && same(b, p) {
                Some((i, true))
            } else {
                None
            }
        })?;
        out.push(hit);
    }
    Some(out)
}

/// `poly` has an edge between `a` and `b`.
fn has_edge(poly: &UPoly, a: V2, b: V2) -> bool {
    let n = poly.pts.len();
    poly.circle.is_none()
        && n >= 3
        && (0..n).any(|j| {
            let (p, q) = (pv(poly.pts[j]), pv(poly.pts[(j + 1) % n]));
            (same(p, a) && same(q, b)) || (same(p, b) && same(q, a))
        })
}

/// The closed ring line `li` is an edge of, with its edge lines when every edge is a line.
fn ring_of_line(u: &Underlay, li: usize) -> Option<(usize, Option<Vec<(usize, bool)>>)> {
    let l = u.lines.get(li)?;
    let (a, b) = (pv(l.a), pv(l.b));
    u.closed
        .iter()
        .enumerate()
        .find(|(_, c)| c.layer == l.layer && has_edge(c, a, b))
        .map(|(r, c)| (r, ring_edges(u, c)))
}

/// Another ring than `ring` has an edge on one of `lines`: moving them would open it.
fn shared(u: &Underlay, ring: usize, lines: &[usize]) -> bool {
    lines.iter().filter_map(|&i| u.lines.get(i)).any(|l| {
        u.closed.iter().enumerate().any(|(r, c)| {
            r != ring && c.layer == l.layer && has_edge(c, pv(l.a), pv(l.b))
        })
    })
}

/// The arc has its ends on a vertex of a closed ring of its layer: it is an edge of that ring.
fn arc_in_ring(u: &Underlay, ai: usize) -> bool {
    let Some(a) = u.arcs.get(ai) else {
        return false;
    };
    if full(a.a0, a.a1) {
        return false;
    }
    let (s, e) = (at(pv(a.c), a.r, a.a0), at(pv(a.c), a.r, a.a1));
    u.closed.iter().any(|c| {
        c.layer == a.layer
            && c.circle.is_none()
            && c.pts.iter().any(|q| same(pv(*q), s))
            && c.pts.iter().any(|q| same(pv(*q), e))
    })
}

/// The circle outline `poly` belongs to the full circle `(c, r)` of `layer`.
fn circle_poly(poly: &UPoly, layer: &str, c: V2, r: f64) -> bool {
    poly.layer == layer
        && poly.circle.is_some_and(|pr| (pr - r).abs() <= MEET)
        && poly
            .pts
            .first()
            .is_some_and(|q| ((pv(*q) - c).len() - r).abs() <= MEET)
}

/// The pick as one primitive: itself when it has one member; a ring with only its own edge
/// lines (a box over the polyline) counts as its first edge line.
fn one_pick(u: &Underlay, pk: &Pick) -> Option<Pick> {
    if pk.len() == 1 {
        return Some(pk.clone());
    }
    let ring = *pk.closed.first()?;
    let own = pk.closed.len() == 1
        && pk.arcs.is_empty()
        && pk.texts.is_empty()
        && pk.fills.is_empty()
        && !pk.lines.is_empty()
        && pk
            .lines
            .iter()
            .all(|&li| ring_of_line(u, li).is_some_and(|(r, _)| r == ring));
    own.then(|| Pick {
        lines: vec![pk.lines[0]],
        ..Default::default()
    })
}

fn len_text(m: f64) -> String {
    crate::units::display_length(m, crate::units::active_units())
}

impl Drawings {
    /// (drawing, grips with their points) of the one picked primitive shown on `level`.
    fn grips_of(&self, level: i64) -> Option<(usize, Vec<(Grip, V2)>)> {
        if self.edit.draw.is_some() {
            return None;
        }
        let (k, pk) = self.pick.as_ref()?;
        let d = self.list.get(*k).filter(|d| d.level == level && d.visible)?;
        let u = &d.u;
        let pk = &one_pick(u, pk)?;
        let mut out = vec![];
        if let Some(&li) = pk.lines.first() {
            let l = u.lines.get(li).filter(|l| d.shown(&l.layer))?;
            match ring_of_line(u, li) {
                Some((ring, Some(_))) => {
                    for (i, q) in u.closed[ring].pts.iter().enumerate() {
                        out.push((Grip::Vertex { ring, i }, pv(*q)));
                    }
                }
                _ => {
                    out.push((Grip::LineEnd { line: li, b: false }, pv(l.a)));
                    out.push((Grip::LineEnd { line: li, b: true }, pv(l.b)));
                }
            }
        } else if let Some(&ai) = pk.arcs.first() {
            let a = u.arcs.get(ai).filter(|a| d.shown(&a.layer))?;
            let c = pv(a.c);
            if full(a.a0, a.a1) {
                for q in 0..4u8 {
                    let t = f64::from(q) * PI / 2.0;
                    out.push((Grip::Quad { arc: ai, q }, at(c, a.r, t)));
                }
            } else {
                out.push((Grip::ArcEnd { arc: ai, end: false }, at(c, a.r, a.a0)));
                out.push((Grip::ArcMid { arc: ai }, at(c, a.r, (a.a0 + a.a1) / 2.0)));
                out.push((Grip::ArcEnd { arc: ai, end: true }, at(c, a.r, a.a1)));
            }
        } else if let Some(&ti) = pk.texts.first() {
            let t = u.texts.get(ti).filter(|t| d.shown(&t.layer))?;
            out.push((Grip::Text { text: ti }, pv(t.pos)));
        }
        (!out.is_empty()).then_some((*k, out))
    }

    /// Grip under `p` within `tol`: the nearest one, else with `ctrl` a new vertex on the
    /// nearest edge of the picked closed polyline.
    fn grip_hit(&self, level: i64, p: V2, tol: f64, ctrl: bool) -> Option<(usize, Grip, V2)> {
        let (k, gs) = self.grips_of(level)?;
        let mut best: Option<(f64, Grip, V2)> = None;
        for &(g, q) in &gs {
            let dd = (p - q).len();
            if dd <= tol && best.is_none_or(|b| dd < b.0) {
                best = Some((dd, g, q));
            }
        }
        if let Some(b) = best {
            return Some((k, b.1, b.2));
        }
        if !ctrl {
            return None;
        }
        let Some(&(Grip::Vertex { ring, .. }, _)) = gs.first() else {
            return None;
        };
        let pts = &self.list.get(k)?.u.closed.get(ring)?.pts;
        let n = pts.len();
        let mut edge: Option<(f64, usize)> = None;
        for i in 0..n {
            let dd = seg_dist(p, pv(pts[i]), pv(pts[(i + 1) % n]));
            if dd <= tol && edge.is_none_or(|e| dd < e.0) {
                edge = Some((dd, i));
            }
        }
        let (_, i) = edge?;
        let q = foot(p, pv(pts[i]), pv(pts[(i + 1) % n]));
        Some((k, Grip::Insert { ring, i }, q))
    }

    /// Start a drag on the grip under `p`. `true` when a grip was hit.
    pub(super) fn grip_press(&mut self, level: i64, p: V2, tol: f64, ctrl: bool) -> bool {
        let Some((k, grip, from)) = self.grip_hit(level, p, tol, ctrl) else {
            return false;
        };
        let gen = self.list[k].gen;
        self.edit.grip.drag = Some(Drag {
            k,
            gen,
            grip,
            from,
            at: from,
            kind: SnapKind::Free,
        });
        self.edit.grip.typed.clear();
        self.edit.grip.sel = None;
        true
    }

    /// Cursor at `p` during a drag: snap to the drawing, its own moving parts left out.
    pub(super) fn grip_move(&mut self, level: i64, p: V2, tol: f64) {
        if let Some(dr) = self.edit.grip.drag {
            let (q, kind) = self.grip_snap(level, &dr, p, tol);
            if let Some(x) = self.edit.grip.drag.as_mut() {
                x.at = q;
                x.kind = kind;
            }
        }
    }

    /// Snap like [`Drawings::snap_point`] without the segments the drag moves.
    fn grip_snap(&self, level: i64, dr: &Drag, p: V2, tol: f64) -> (V2, SnapKind) {
        let d = self.list.get(dr.k);
        let circle = d.and_then(|d| match dr.grip {
            Grip::ArcEnd { arc, .. } | Grip::ArcMid { arc } | Grip::Quad { arc, .. } => {
                d.u.arcs.get(arc).map(|a| (pv(a.c), a.r))
            }
            _ => None,
        });
        // the segments the drag moves: the line, or the ring edges at the vertex
        let own: Vec<(V2, V2)> = d
            .and_then(|d| {
                let u = &d.u;
                Some(match dr.grip {
                    Grip::LineEnd { line, .. } => {
                        let l = u.lines.get(line)?;
                        vec![(pv(l.a), pv(l.b))]
                    }
                    Grip::Vertex { ring, i } => {
                        let pts = &u.closed.get(ring)?.pts;
                        let n = pts.len();
                        let (p, q) = (pv(pts[(i + n - 1) % n]), pv(pts[(i + 1) % n]));
                        vec![(p, dr.from), (dr.from, q)]
                    }
                    Grip::Insert { ring, i } => {
                        let pts = &u.closed.get(ring)?.pts;
                        vec![(pv(*pts.get(i)?), pv(pts[(i + 1) % pts.len()]))]
                    }
                    _ => vec![],
                })
            })
            .unwrap_or_default();
        let moves = |a: V2, b: V2| match (dr.grip, circle) {
            (Grip::Text { .. }, _) => false,
            (_, Some((c, r))) => {
                let on = |x: V2| ((x - c).len() - r).abs() <= 1e-7 * r.max(1.0);
                on(a) && on(b)
            }
            _ => own
                .iter()
                .any(|&(s, e)| (same(a, s) && same(b, e)) || (same(a, e) && same(b, s))),
        };
        let prims = self.prims(level).filter(|pr| match *pr {
            Prim::Line(a, b) => !moves(a, b),
            Prim::Arc { c, r, .. } => !circle
                .is_some_and(|(c0, r0)| same(c, c0) && (r - r0).abs() <= 1e-7 * r0.max(1.0)),
        });
        pick(prims, &[], p, tol)
    }

    /// Fixed point a typed length is measured from, for the grips that take one.
    fn grip_anchor(&self, dr: &Drag) -> Option<V2> {
        let u = &self.list.get(dr.k)?.u;
        match dr.grip {
            Grip::LineEnd { line, b } => u.lines.get(line).map(|l| pv(if b { l.a } else { l.b })),
            Grip::Vertex { ring, i } => {
                let pts = &u.closed.get(ring)?.pts;
                let n = pts.len();
                Some(pv(pts[(i + n - 1) % n]))
            }
            Grip::Insert { ring, i } => u.closed.get(ring)?.pts.get(i).map(|q| pv(*q)),
            Grip::Quad { arc, .. } => u.arcs.get(arc).map(|a| pv(a.c)),
            Grip::Text { .. } => Some(dr.from),
            Grip::ArcEnd { .. } | Grip::ArcMid { .. } => None,
        }
    }

    /// Drop point of a drag: the cursor, or `len` (m) from the anchor towards it.
    fn grip_target(&self, dr: &Drag, len: Option<f64>) -> Result<V2, String> {
        let Some(l) = len else {
            return Ok(dr.at);
        };
        if !(l.is_finite() && l > SAME) {
            return Err(tr("dwg_grips.st.len_bad"));
        }
        let Some(a) = self.grip_anchor(dr) else {
            return Err(tr("dwg_grips.st.len_none"));
        };
        let dir = dr.at - a;
        if dir.len() <= SAME {
            return Err(tr("dwg_grips.st.len_dir"));
        }
        Ok(a + dir * (l / dir.len()))
    }

    /// End the drag: the typed length when there is one, else the snapped cursor.
    pub(super) fn grip_release(&mut self) -> Option<Result<String, String>> {
        let dr = self.edit.grip.drag.take()?;
        let typed = std::mem::take(&mut self.edit.grip.typed);
        let len = if typed.trim().is_empty() {
            None
        } else {
            match crate::units::parse_edit_length(&typed, crate::units::active_units()) {
                Some(v) => Some(v),
                None => return Some(Err(tr("dwg_grips.st.len_bad"))),
            }
        };
        Some(self.grip_drop(&dr, len))
    }

    /// Write the drag as one undo step; refusals change nothing.
    fn grip_drop(&mut self, dr: &Drag, len: Option<f64>) -> Result<String, String> {
        let Some(d) = self.list.get(dr.k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        if d.gen != dr.gen {
            return Err(tr("dwg_grips.st.changed"));
        }
        let q = self.grip_target(dr, len)?;
        if !(q.x.is_finite() && q.y.is_finite()) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        if !matches!(dr.grip, Grip::Insert { .. }) && (q - dr.from).len() <= SAME {
            return Err(tr("dwg_grips.st.same"));
        }
        self.apply_grip(dr.k, dr.grip, q)
    }

    /// Move grip `g` of drawing `k` to `q`.
    fn apply_grip(&mut self, k: usize, g: Grip, q: V2) -> Result<String, String> {
        let gone = || tr("dwg_grips.st.gone");
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        if let Grip::ArcEnd { arc, .. } | Grip::ArcMid { arc } | Grip::Quad { arc, .. } = g {
            if u.arcs.get(arc).is_none() {
                return Err(gone());
            }
        }
        let key = match g {
            Grip::LineEnd { line, b } => {
                let l = u.lines.get(line).ok_or_else(gone)?;
                if ring_of_line(u, line).is_some() {
                    return Err(tr("dwg_grips.st.ring_arcs"));
                }
                let other = pv(if b { l.a } else { l.b });
                if (q - other).len() <= SAME {
                    return Err(tr("dwg_grips.st.zero"));
                }
                self.steps.push(Step::geometry(d));
                let l = &mut d.u.lines[line];
                if b {
                    l.b = (q.x, q.y);
                } else {
                    l.a = (q.x, q.y);
                }
                "dwg_grips.st.end_moved"
            }
            Grip::Vertex { ring, i } => {
                let poly = u.closed.get(ring).ok_or_else(gone)?;
                let es = ring_edges(u, poly).ok_or_else(|| tr("dwg_grips.st.ring_arcs"))?;
                let n = poly.pts.len();
                if i >= n {
                    return Err(gone());
                }
                let (prev, next) = (pv(poly.pts[(i + n - 1) % n]), pv(poly.pts[(i + 1) % n]));
                if (q - prev).len() <= SAME || (q - next).len() <= SAME {
                    return Err(tr("dwg_grips.st.zero"));
                }
                let (e0, e1) = (es[(i + n - 1) % n], es[i]);
                if shared(u, ring, &[e0.0, e1.0]) {
                    return Err(tr("dwg_grips.st.shared"));
                }
                self.steps.push(Step::geometry(d));
                let u = &mut d.u;
                u.closed[ring].pts[i] = (q.x, q.y);
                // edge i-1 ends at the vertex, edge i starts there
                let l0 = &mut u.lines[e0.0];
                if e0.1 {
                    l0.a = (q.x, q.y);
                } else {
                    l0.b = (q.x, q.y);
                }
                let l1 = &mut u.lines[e1.0];
                if e1.1 {
                    l1.b = (q.x, q.y);
                } else {
                    l1.a = (q.x, q.y);
                }
                "dwg_grips.st.vertex_moved"
            }
            Grip::Insert { ring, i } => {
                let poly = u.closed.get(ring).ok_or_else(gone)?;
                let es = ring_edges(u, poly).ok_or_else(|| tr("dwg_grips.st.ring_arcs"))?;
                let n = poly.pts.len();
                if i >= n {
                    return Err(gone());
                }
                let (s, e) = (pv(poly.pts[i]), pv(poly.pts[(i + 1) % n]));
                if (q - s).len() <= SAME || (q - e).len() <= SAME {
                    return Err(tr("dwg_grips.st.zero"));
                }
                let (li, flip) = es[i];
                if shared(u, ring, &[li]) {
                    return Err(tr("dwg_grips.st.shared"));
                }
                // a box pick holding every edge of the ring keeps covering it
                let full = self.pick.as_ref().is_some_and(|(pk_k, pk)| {
                    *pk_k == k && pk.closed.contains(&ring) && es.iter().all(|e| pk.lines.contains(&e.0))
                });
                self.steps.push(Step::geometry(d));
                let u = &mut d.u;
                let mut rest = u.lines[li].clone();
                let l = &mut u.lines[li];
                // the line keeps the part at vertex i, the copy takes the part to vertex i+1
                if flip {
                    l.a = (q.x, q.y);
                    rest.a = (e.x, e.y);
                    rest.b = (q.x, q.y);
                } else {
                    l.b = (q.x, q.y);
                    rest.a = (q.x, q.y);
                    rest.b = (e.x, e.y);
                }
                u.lines.push(rest);
                let added = u.lines.len() - 1;
                u.closed[ring].pts.insert(i + 1, (q.x, q.y));
                if full {
                    if let Some((_, pk)) = self.pick.as_mut() {
                        pk.lines.push(added);
                    }
                }
                d.sync_layers();
                "dwg_grips.st.vertex_added"
            }
            Grip::ArcEnd { arc, end } => {
                if arc_in_ring(u, arc) {
                    return Err(tr("dwg_grips.st.arc_ring"));
                }
                let a = &u.arcs[arc];
                let c = pv(a.c);
                let (s0, m0, e0) = (
                    at(c, a.r, a.a0),
                    at(c, a.r, (a.a0 + a.a1) / 2.0),
                    at(c, a.r, a.a1),
                );
                let (s, e) = if end { (s0, q) } else { (q, e0) };
                let (nc, r, a0, a1) = arc3(s, m0, e).ok_or_else(|| tr("dwg_grips.st.arc_flat"))?;
                self.steps.push(Step::geometry(d));
                let a = &mut d.u.arcs[arc];
                (a.c, a.r, a.a0, a.a1) = ((nc.x, nc.y), r, a0, a1);
                "dwg_grips.st.arc_changed"
            }
            Grip::ArcMid { arc } => {
                if arc_in_ring(u, arc) {
                    return Err(tr("dwg_grips.st.arc_ring"));
                }
                let a = &u.arcs[arc];
                let c = pv(a.c);
                let (s, e) = (at(c, a.r, a.a0), at(c, a.r, a.a1));
                let (nc, r, a0, a1) = arc3(s, q, e).ok_or_else(|| tr("dwg_grips.st.arc_flat"))?;
                self.steps.push(Step::geometry(d));
                let a = &mut d.u.arcs[arc];
                (a.c, a.r, a.a0, a.a1) = ((nc.x, nc.y), r, a0, a1);
                "dwg_grips.st.arc_changed"
            }
            Grip::Quad { arc, .. } => {
                let a = &u.arcs[arc];
                let c = pv(a.c);
                let r = (q - c).len();
                if r <= SAME {
                    return Err(tr("dwg_grips.st.zero"));
                }
                if (r - a.r).abs() <= SAME {
                    return Err(tr("dwg_grips.st.same"));
                }
                let (layer, r0) = (a.layer.clone(), a.r);
                self.steps.push(Step::geometry(d));
                let u = &mut d.u;
                u.arcs[arc].r = r;
                for poly in u.closed.iter_mut() {
                    if circle_poly(poly, &layer, c, r0) {
                        poly.pts = ddc_io::dxf::lower::circle_ring((c.x, c.y), r);
                        poly.circle = Some(r);
                    }
                }
                d.mark_edited();
                self.drop_recog(k);
                self.touch();
                return Ok(crate::tr("dwg_grips.st.radius", &[("r", &len_text(r))]));
            }
            Grip::Text { text } => {
                if u.texts.get(text).is_none() {
                    return Err(gone());
                }
                self.steps.push(Step::geometry(d));
                d.u.texts[text].pos = (q.x, q.y);
                "dwg_grips.st.text_moved"
            }
        };
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(tr(key))
    }

    /// Click without a drag: select the vertex under `p` for Delete. `true` when on a grip.
    pub(super) fn grip_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        match self.grip_hit(level, p, tol, false) {
            Some((k, g, _)) => {
                let gen = self.list[k].gen;
                self.edit.grip.sel = g.deletable().then_some((k, gen, g));
                true
            }
            None => {
                self.edit.grip.sel = None;
                false
            }
        }
    }

    /// The selected vertex while its drawing is unchanged and its primitive still picked.
    fn grip_sel(&self) -> Option<(usize, Grip)> {
        let (k, gen, g) = self.edit.grip.sel?;
        let d = self.list.get(k)?;
        let picked = self
            .pick
            .as_ref()
            .is_some_and(|(pk, p)| *pk == k && one_pick(&d.u, p).is_some());
        (d.gen == gen && picked).then_some((k, g))
    }

    /// Remove the selected vertex as one undo step: a line keeps its two ends, a closed
    /// polyline at least three vertices.
    pub fn grip_delete(&mut self) -> Result<String, String> {
        let Some((k, g)) = self.grip_sel() else {
            self.edit.grip.sel = None;
            return Err(tr("dwg_grips.st.no_vertex"));
        };
        let Grip::Vertex { ring, i } = g else {
            return Err(tr("dwg_grips.st.vertex_last"));
        };
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        let poly = u.closed.get(ring).ok_or_else(|| tr("dwg_grips.st.gone"))?;
        let es = ring_edges(u, poly).ok_or_else(|| tr("dwg_grips.st.ring_arcs"))?;
        let n = poly.pts.len();
        if i >= n {
            return Err(tr("dwg_grips.st.gone"));
        }
        if n <= 3 {
            return Err(tr("dwg_grips.st.vertex_ring"));
        }
        let (prev, next) = (pv(poly.pts[(i + n - 1) % n]), pv(poly.pts[(i + 1) % n]));
        if (next - prev).len() <= SAME {
            return Err(tr("dwg_grips.st.zero"));
        }
        let (e0, e1) = (es[(i + n - 1) % n], es[i]);
        if shared(u, ring, &[e0.0, e1.0]) {
            return Err(tr("dwg_grips.st.shared"));
        }
        self.steps.push(Step::geometry(d));
        let u = &mut d.u;
        // edge i-1 now runs to vertex i+1; the line of edge i goes
        let l0 = &mut u.lines[e0.0];
        if e0.1 {
            l0.a = (next.x, next.y);
        } else {
            l0.b = (next.x, next.y);
        }
        u.lines.remove(e1.0);
        u.closed[ring].pts.remove(i);
        let shift = |x: usize| if x > e1.0 { x - 1 } else { x };
        let keep = shift(e0.0);
        if let Some((pk_k, pk)) = self.pick.as_mut() {
            if *pk_k == k {
                for x in pk.lines.iter_mut() {
                    *x = if *x == e1.0 { keep } else { shift(*x) };
                }
                pk.lines.sort_unstable();
                pk.lines.dedup();
            }
        }
        d.sync_layers();
        d.mark_edited();
        self.edit.grip.sel = None;
        self.drop_recog(k);
        self.touch();
        Ok(tr("dwg_grips.st.vertex_removed"))
    }

    /// Pointer input on the plan canvas for grips. Returns (taken, status): a taken press,
    /// drag, release or click starts no rectangle and changes no pick.
    pub fn grip_pointer(
        &mut self,
        ui: &egui::Ui,
        resp: &egui::Response,
        level: i64,
        to_w: &dyn Fn(Pos2) -> V2,
        tol: f64,
    ) -> (bool, Option<String>) {
        let prim = egui::PointerButton::Primary;
        let mut status = None;
        if resp.drag_started_by(prim) && self.edit.grip.drag.is_none() {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos());
            let ctrl = ui.input(|i| i.modifiers.command);
            if let Some(o) = origin {
                self.edit.grip.hold = self.grip_press(level, to_w(o), tol, ctrl);
            }
        }
        if self.edit.grip.drag.is_some() {
            let gen = self.edit.grip.drag.and_then(|dr| Some((dr.gen, self.list.get(dr.k)?.gen)));
            if gen.is_none_or(|(a, b)| a != b) || self.grips_of(level).is_none() {
                // undone or edited meanwhile: the drag would write over that
                self.edit.grip.drag = None;
                status = Some(tr("dwg_grips.st.changed"));
            } else if let Some(p) = resp.interact_pointer_pos().or(resp.hover_pos()) {
                self.grip_move(level, to_w(p), tol);
            }
        }
        if self.edit.grip.hold {
            if resp.drag_stopped_by(prim) || !ui.input(|i| i.pointer.primary_down()) {
                self.edit.grip.hold = false;
                if let Some(r) = self.grip_release() {
                    status = Some(r.unwrap_or_else(|e| e));
                }
            }
            return (true, status);
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if self.grip_click(level, to_w(p), tol) {
                    return (true, status);
                }
            }
        }
        (false, status)
    }

    /// Keys for grips: during a drag digits type the length, Backspace edits it, Enter drops,
    /// Esc cancels; with a selected vertex Delete removes it. Consumed keys reach nothing else.
    pub fn grip_keys(&mut self, ui: &egui::Ui) -> Option<String> {
        use egui::{Key, Modifiers};
        if self.edit.grip.drag.is_some() {
            let (esc, enter, back, text) = ui.input_mut(|i| {
                let esc = i.consume_key(Modifiers::NONE, Key::Escape);
                let enter = i.consume_key(Modifiers::NONE, Key::Enter);
                let back = i.consume_key(Modifiers::NONE, Key::Backspace);
                let mut t = String::new();
                i.events.retain(|e| match e {
                    egui::Event::Text(s) => {
                        t.push_str(s);
                        false
                    }
                    _ => true,
                });
                (esc, enter, back, t)
            });
            let typed = &mut self.edit.grip.typed;
            typed.extend(
                text.chars()
                    .filter(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\'' | '"' | '/')),
            );
            if back {
                typed.pop();
            }
            if esc {
                self.edit.grip.drag = None;
                self.edit.grip.typed.clear();
                return Some(tr("dwg_grips.st.cancelled"));
            }
            if enter {
                return self.grip_release().map(|r| r.unwrap_or_else(|e| e));
            }
            return None;
        }
        if self.grip_sel().is_some()
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete))
        {
            return Some(self.grip_delete().unwrap_or_else(|e| e));
        }
        None
    }

    /// Outline of the primitive as the drag would leave it.
    fn grip_preview(&self, dr: &Drag, q: V2) -> Vec<V2> {
        let Some(u) = self.list.get(dr.k).map(|d| &d.u) else {
            return vec![];
        };
        let ring = |ring: usize| u.closed.get(ring).map(|c| &c.pts);
        match dr.grip {
            Grip::LineEnd { line, b } => u
                .lines
                .get(line)
                .map(|l| vec![pv(if b { l.a } else { l.b }), q])
                .unwrap_or_default(),
            Grip::Vertex { ring: r, i } => ring(r)
                .filter(|p| i < p.len())
                .map(|p| {
                    let n = p.len();
                    vec![pv(p[(i + n - 1) % n]), q, pv(p[(i + 1) % n])]
                })
                .unwrap_or_default(),
            Grip::Insert { ring: r, i } => ring(r)
                .filter(|p| i < p.len())
                .map(|p| vec![pv(p[i]), q, pv(p[(i + 1) % p.len()])])
                .unwrap_or_default(),
            Grip::ArcEnd { arc, .. } | Grip::ArcMid { arc } => {
                let Some(a) = u.arcs.get(arc) else {
                    return vec![];
                };
                let c = pv(a.c);
                let (s, m, e) = (
                    at(c, a.r, a.a0),
                    at(c, a.r, (a.a0 + a.a1) / 2.0),
                    at(c, a.r, a.a1),
                );
                let (s, m, e) = match dr.grip {
                    Grip::ArcEnd { end: true, .. } => (s, m, q),
                    Grip::ArcEnd { end: false, .. } => (q, m, e),
                    _ => (s, q, e),
                };
                match arc3(s, m, e) {
                    Some((nc, r, a0, a1)) => arc_pts((nc.x, nc.y), r, a0, a1, 48),
                    None => vec![s, m, e],
                }
            }
            Grip::Quad { arc, .. } => u
                .arcs
                .get(arc)
                .map(|a| {
                    let c = pv(a.c);
                    arc_pts(a.c, (q - c).len(), 0.0, TAU, 64)
                })
                .unwrap_or_default(),
            Grip::Text { .. } => vec![dr.from, q],
        }
    }

    /// Grips of the picked primitive, the drag preview and the typed length.
    pub fn paint_grips(
        &self,
        p: &Painter,
        to_s: &dyn Fn(V2) -> Pos2,
        level: i64,
        ov: &Palette,
        hover: Option<Pos2>,
    ) {
        let Some((k, gs)) = self.grips_of(level) else {
            return;
        };
        let sel = self.grip_sel().filter(|s| s.0 == k).map(|s| s.1);
        let drag = self.edit.grip.drag.filter(|dr| dr.k == k);
        for (g, q) in gs {
            if drag.is_some_and(|dr| dr.grip == g) {
                continue;
            }
            let at = to_s(q);
            let shape = match g {
                Grip::ArcMid { .. } => overlay::Grip::Diamond,
                Grip::Quad { .. } => overlay::Grip::Circle,
                _ => overlay::Grip::Square,
            };
            let hot = sel == Some(g)
                || (drag.is_none()
                    && hover.is_some_and(|h| h.distance(at) < overlay::GRIP_HIT_PX));
            overlay::grip(p, at, shape, hot, ov);
        }
        let Some(dr) = drag else { return };
        let typed = self.edit.grip.typed.trim();
        let len = crate::units::parse_edit_length(typed, crate::units::active_units());
        let q = self.grip_target(&dr, len).unwrap_or(dr.at);
        let line: Vec<Pos2> = self.grip_preview(&dr, q).into_iter().map(to_s).collect();
        if line.len() >= 2 {
            let (on, off) = overlay::GHOST_DASH;
            p.extend(egui::Shape::dashed_line(
                &line,
                Stroke::new(overlay::GHOST_PX, ov.preview),
                on,
                off,
            ));
        }
        overlay::grip(p, to_s(q), overlay::Grip::Square, true, ov);
        if typed.is_empty() && dr.kind != SnapKind::Free {
            overlay::snap_glyph(p, to_s(q), crate::plan_view::snap_glyph(dr.kind), ov);
        }
        if let Some(a) = self.grip_anchor(&dr) {
            let mid = to_s((a + q) * 0.5);
            let (txt, kind) = if typed.is_empty() {
                (len_text((q - a).len()), Plate::Temp)
            } else {
                (format!("{typed}\u{258f}"), Plate::Editing)
            };
            overlay::dim_label(p, mid, txt, kind, ov);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::Origin;
    use super::*;
    use crate::editor::Editor;
    use ddc_io::drawing_model::{Pick, UArc, ULine, UText};
    use ddc_io::dxf::{Common, DxfDocument, Entity};

    const TOL: f64 = 0.01;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    /// A drawing in metres with one far line on layer "Z".
    fn setup() -> (Editor, Drawings) {
        let ed = Editor::new_in_memory().unwrap();
        let mut doc = DxfDocument::default();
        doc.entities.push(Entity::Line(ddc_io::dxf::Line {
            common: Common {
                layer: "Z".into(),
                ..Default::default()
            },
            start: [100.0, 100.0, 0.0],
            end: [101.0, 100.0, 0.0],
        }));
        let mut ds = Drawings::default();
        ds.add(&doc, "m", None, ed.level, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
        ds.sel = 0;
        (ed, ds)
    }

    fn sync(ed: &mut Editor, ds: &mut Drawings) {
        ds.sync_undo(ed);
    }

    fn line(ds: &mut Drawings, a: (f64, f64), b: (f64, f64)) -> usize {
        let u = &mut ds.list[0].u;
        u.lines.push(ULine {
            layer: "Z".into(),
            a,
            b,
            dashed: false,
            pen: 0,
        });
        ds.touch();
        u_len(ds) - 1
    }

    fn u_len(ds: &Drawings) -> usize {
        ds.list[0].u.lines.len()
    }

    fn pick(ds: &mut Drawings, pk: Pick) {
        ds.pick = Some((0, pk));
    }

    fn pick_line(ds: &mut Drawings, i: usize) {
        pick(
            ds,
            Pick {
                lines: vec![i],
                ..Default::default()
            },
        );
    }

    /// Press at `a`, move to `b`, release: the result of the drop.
    fn drag(ds: &mut Drawings, lv: i64, a: V2, b: V2, ctrl: bool) -> Result<String, String> {
        assert!(ds.grip_press(lv, a, TOL, ctrl), "no grip at {a:?}");
        ds.grip_move(lv, b, TOL);
        ds.grip_release().expect("a drag was on")
    }

    /// Closed square polyline drawn with the draw tool; its first line index.
    fn square(ds: &mut Drawings, pts: &[(f64, f64)]) -> usize {
        let n = u_len(ds);
        let v: Vec<V2> = pts.iter().map(|q| v2(q.0, q.1)).collect();
        ds.add_path(&v, true).unwrap();
        n
    }

    fn ring_closed(ds: &Drawings, ring: usize) -> bool {
        let u = &ds.list[0].u;
        ring_edges(u, &u.closed[ring]).is_some_and(|e| e.len() == u.closed[ring].pts.len())
    }

    #[test]
    fn grip_drag_snaps_to_intersection() {
        let (ed, mut ds) = setup();
        let lv = ed.level;
        let l1 = line(&mut ds, (0.0, 0.0), (1.0, 0.0));
        line(&mut ds, (-5.0, -5.0), (7.0, 7.0));
        line(&mut ds, (-3.0, 3.0), (5.0, -5.0));
        pick_line(&mut ds, l1);
        assert!(ds.grip_press(lv, v2(1.0, 0.0), TOL, false));
        ds.grip_move(lv, v2(0.003, 0.004), TOL);
        let dr = ds.edit.grip.drag.unwrap();
        assert_eq!(dr.kind, SnapKind::Intersection);
        assert!(near(dr.at.into_t(), (0.0, 0.0)));
    }

    #[test]
    fn drag_line_end_snaps() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let l1 = line(&mut ds, (0.0, 0.0), (1.0, 0.0));
        line(&mut ds, (2.0, 2.0), (3.0, 2.0));
        pick_line(&mut ds, l1);
        let n0 = ed.undo_len();
        // snaps onto the other line's end
        drag(&mut ds, lv, v2(1.001, 0.0005), v2(2.004, 1.997), false).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one drag, one step");
        let l = &ds.list[0].u.lines[l1];
        assert!(near(l.a, (0.0, 0.0)) && near(l.b, (2.0, 2.0)), "{l:?}");
        assert!(ds.list[0].dirty);
        // onto a midpoint; the dragged line's own midpoint is no snap
        assert!(ds.grip_press(lv, v2(2.0, 2.0), TOL, false));
        ds.grip_move(lv, v2(1.003, 0.998), TOL);
        assert!(near(ds.edit.grip.drag.unwrap().at.into_t(), (1.003, 0.998)));
        ds.grip_move(lv, v2(2.5, 2.004), TOL);
        assert!(near(ds.edit.grip.drag.unwrap().at.into_t(), (2.5, 2.0)));
        // a typed length from the fixed end, towards the cursor
        let dr = ds.edit.grip.drag.take().unwrap();
        ds.grip_drop(&dr, Some(2.5)).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        let l = &ds.list[0].u.lines[l1];
        let (b, dir) = (pv(l.b), v2(2.5, 2.0));
        assert!(((b - pv(l.a)).len() - 2.5).abs() < 1e-9);
        assert!(b.cross(dir).abs() < 1e-9 && b.dot(dir) > 0.0);
        // refused, no step: zero length, unchanged, bad typed length, an edit meanwhile
        let end = pv(ds.list[0].u.lines[l1].b);
        assert!(drag(&mut ds, lv, end, v2(0.0, 0.0), false).is_err());
        assert!(drag(&mut ds, lv, end, end, false).is_err());
        assert!(ds.grip_press(lv, end, TOL, false));
        ds.grip_move(lv, v2(5.0, 5.0), TOL);
        ds.edit.grip.typed = "x".into();
        assert!(ds.grip_release().unwrap().is_err());
        assert!(ds.grip_press(lv, end, TOL, false));
        ds.grip_move(lv, v2(5.0, 5.0), TOL);
        ds.list[0].mark_edited();
        assert!(ds.grip_release().unwrap().is_err(), "changed during the drag");
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        assert!(near(ds.list[0].u.lines[l1].b, (end.x, end.y)));
        // undo walks back one drag at a time
        ed.undo();
        sync(&mut ed, &mut ds);
        assert!(near(ds.list[0].u.lines[l1].b, (2.0, 2.0)));
        ed.undo();
        sync(&mut ed, &mut ds);
        assert!(near(ds.list[0].u.lines[l1].b, (1.0, 0.0)));
    }

    trait IntoT {
        fn into_t(self) -> (f64, f64);
    }
    impl IntoT for V2 {
        fn into_t(self) -> (f64, f64) {
            (self.x, self.y)
        }
    }

    fn export(ds: &Drawings) -> DxfDocument {
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        doc
    }

    /// The closed LWPOLYLINEs of the export, as vertex lists.
    fn rings(doc: &DxfDocument) -> Vec<Vec<(f64, f64)>> {
        doc.entities
            .iter()
            .filter_map(|e| match e {
                Entity::LwPolyline(p) if p.closed() => {
                    Some(p.vertices.iter().map(|v| (v.x, v.y)).collect())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn grips_on_box_picked_single_ring() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let s = square(&mut ds, &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        sync(&mut ed, &mut ds);
        let ring = ds.list[0].u.closed.len() - 1;
        // a box over one edge: that line and the ring
        pick(&mut ds, Pick { lines: vec![s], closed: vec![ring], ..Default::default() });
        assert_eq!(ds.grips_of(lv).unwrap().1.len(), 4);
        // a box over the whole polyline
        pick(
            &mut ds,
            Pick { lines: (s..s + 4).collect(), closed: vec![ring], ..Default::default() },
        );
        assert_eq!(ds.grips_of(lv).unwrap().1.len(), 4);
        // a second primitive: no grips
        let other = line(&mut ds, (10.0, 0.0), (11.0, 0.0));
        pick(
            &mut ds,
            Pick { lines: vec![s, other], closed: vec![ring], ..Default::default() },
        );
        assert!(ds.grips_of(lv).is_none());
    }

    #[test]
    fn drag_polyline_vertex_one_undo() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let s = square(&mut ds, &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        sync(&mut ed, &mut ds);
        let ring = ds.list[0].u.closed.len() - 1;
        pick_line(&mut ds, s);
        let (gk, gs) = ds.grips_of(lv).unwrap();
        assert_eq!(gk, 0);
        assert_eq!(gs.len(), 4, "a grip on every vertex: {gs:?}");
        let n0 = ed.undo_len();
        let lines0 = u_len(&ds);
        drag(&mut ds, lv, v2(4.002, 0.001), v2(5.0, 1.0), false).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one drag, one step");
        let u = &ds.list[0].u;
        assert!(near(u.closed[ring].pts[1], (5.0, 1.0)));
        assert!(near(u.lines[s].b, (5.0, 1.0)) && near(u.lines[s + 1].a, (5.0, 1.0)));
        assert_eq!(u_len(&ds), lines0);
        assert!(ring_closed(&ds, ring), "the ring stays closed");
        let rs = rings(&export(&ds));
        assert_eq!(rs.len(), 1, "still one closed polyline");
        assert!(rs[0].iter().any(|v| near(*v, (5.0, 1.0))));

        // Ctrl-drag on an edge inserts a vertex: one more line, one step
        drag(&mut ds, lv, v2(2.0, 4.003), v2(2.0, 5.0), true).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        let u = &ds.list[0].u;
        assert_eq!(u.closed[ring].pts.len(), 5);
        assert!(near(u.closed[ring].pts[3], (2.0, 5.0)));
        assert_eq!(u_len(&ds), lines0 + 1);
        assert!(ring_closed(&ds, ring));
        let rs = rings(&export(&ds));
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].len(), 5);
        // without Ctrl the edge takes no drag
        assert!(!ds.grip_press(lv, v2(0.0, 2.0), TOL, false));

        ed.undo();
        sync(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.closed[ring].pts.len(), 4);
        assert_eq!(u_len(&ds), lines0);
        ed.undo();
        sync(&mut ed, &mut ds);
        let u = &ds.list[0].u;
        assert!(near(u.closed[ring].pts[1], (4.0, 0.0)) && near(u.lines[s].b, (4.0, 0.0)));
        assert!(ring_closed(&ds, ring));
        ed.redo();
        sync(&mut ed, &mut ds);
        assert!(near(ds.list[0].u.closed[ring].pts[1], (5.0, 1.0)));
    }

    #[test]
    fn arc_mid_grip_keeps_ends() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        ds.list[0].u.arcs.push(UArc {
            layer: "Z".into(),
            c: (0.0, 0.0),
            r: 1.0,
            a0: 0.0,
            a1: PI / 2.0,
            pen: 0,
        });
        ds.touch();
        let ai = ds.list[0].u.arcs.len() - 1;
        pick(
            &mut ds,
            Pick {
                arcs: vec![ai],
                ..Default::default()
            },
        );
        assert_eq!(ds.grips_of(lv).unwrap().1.len(), 3, "two ends and the middle");
        let ends = |ds: &Drawings| {
            let a = &ds.list[0].u.arcs[ai];
            let c = pv(a.c);
            (at(c, a.r, a.a0), at(c, a.r, (a.a0 + a.a1) / 2.0), at(c, a.r, a.a1))
        };
        let n0 = ed.undo_len();
        let h = 0.5f64.sqrt();
        drag(&mut ds, lv, v2(h, h), v2(0.9, 0.9), false).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        let (s, m, e) = ends(&ds);
        let set = |p: V2, q: V2| {
            (near(p.into_t(), (1.0, 0.0)) && near(q.into_t(), (0.0, 1.0)))
                || (near(p.into_t(), (0.0, 1.0)) && near(q.into_t(), (1.0, 0.0)))
        };
        assert!(set(s, e), "the ends stay: {s:?} {e:?}");
        assert!(near(m.into_t(), (0.9, 0.9)), "{m:?}");
        // a mid on the chord makes no arc: refused, no step
        assert!(drag(&mut ds, lv, v2(0.9, 0.9), v2(0.5, 0.5), false).is_err());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        // an end grip: the arc runs through the new end, the other end and the old middle
        let (s, m, e) = ends(&ds);
        let (from, keep) = if near(s.into_t(), (1.0, 0.0)) { (s, e) } else { (e, s) };
        drag(&mut ds, lv, from, v2(1.2, -0.3), false).unwrap();
        let (s2, _, e2) = ends(&ds);
        let a = &ds.list[0].u.arcs[ai];
        let on = |p: V2| ((p - pv(a.c)).len() - a.r).abs() < 1e-9;
        assert!(on(m) && on(keep) && on(v2(1.2, -0.3)));
        assert!(near(s2.into_t(), keep.into_t()) || near(e2.into_t(), keep.into_t()));
        // a circle has quadrant grips that set the radius
        ds.list[0].u.arcs.push(UArc {
            layer: "Z".into(),
            c: (5.0, 5.0),
            r: 1.0,
            a0: 0.0,
            a1: TAU,
            pen: 0,
        });
        ds.touch();
        let ci = ds.list[0].u.arcs.len() - 1;
        pick(
            &mut ds,
            Pick {
                arcs: vec![ci],
                ..Default::default()
            },
        );
        assert_eq!(ds.grips_of(lv).unwrap().1.len(), 4);
        drag(&mut ds, lv, v2(6.0, 5.0), v2(7.0, 5.0), false).unwrap();
        assert!((ds.list[0].u.arcs[ci].r - 2.0).abs() < 1e-12);
        assert_eq!(ds.list[0].u.arcs[ci].c, (5.0, 5.0));
    }

    #[test]
    fn del_vertex_refuses_last_two() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let l = line(&mut ds, (0.0, 0.0), (1.0, 0.0));
        let tri = square(&mut ds, &[(10.0, 0.0), (12.0, 0.0), (11.0, 2.0)]);
        let sq = square(&mut ds, &[(20.0, 0.0), (24.0, 0.0), (24.0, 4.0), (20.0, 4.0)]);
        sync(&mut ed, &mut ds);
        let ring = ds.list[0].u.closed.len() - 1;
        let n0 = ed.undo_len();
        let lines0 = u_len(&ds);
        // nothing selected
        assert!(ds.grip_delete().is_err());
        // a line keeps its two ends
        pick_line(&mut ds, l);
        assert!(ds.grip_click(lv, v2(1.0, 0.0), TOL));
        assert!(ds.grip_delete().is_err());
        // a closed polyline keeps three vertices
        pick_line(&mut ds, tri);
        assert!(ds.grip_click(lv, v2(12.0, 0.0), TOL));
        assert!(ds.grip_delete().is_err());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0, "refusals make no step");
        assert_eq!(u_len(&ds), lines0);
        // a square loses a vertex and stays closed
        pick_line(&mut ds, sq + 1);
        assert!(ds.grip_click(lv, v2(24.0, 4.0), TOL));
        ds.grip_delete().unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one delete, one step");
        let u = &ds.list[0].u;
        assert_eq!(u.closed[ring].pts.len(), 3);
        assert_eq!(u_len(&ds), lines0 - 1);
        assert!(ring_closed(&ds, ring));
        let (_, pk) = ds.pick.as_ref().unwrap();
        assert!(pk.lines.iter().all(|&i| i < u_len(&ds)), "the pick stays valid");
        assert!(ds.grip_delete().is_err(), "the selection went with the vertex");
        ed.undo();
        sync(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.closed[ring].pts.len(), 4);
        assert_eq!(u_len(&ds), lines0);
        assert!(ring_closed(&ds, ring));
    }

    #[test]
    fn box_picked_ring_delete_vertex_then_offset() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let s = square(&mut ds, &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        sync(&mut ed, &mut ds);
        let ring = ds.list[0].u.closed.len() - 1;
        pick(&mut ds, Pick { lines: (s..s + 4).collect(), closed: vec![ring], ..Default::default() });
        assert!(ds.grip_click(lv, v2(0.0, 0.0), TOL));
        ds.grip_delete().unwrap();
        sync(&mut ed, &mut ds);
        let (_, pk) = ds.pick.as_ref().unwrap();
        let mut uniq = pk.lines.clone();
        uniq.dedup();
        assert_eq!(uniq, pk.lines, "sorted and unique");
        assert_eq!(pk.lines.len(), 3);
        let closed0 = ds.list[0].u.closed.len();
        let lines0 = u_len(&ds);
        let n0 = ed.undo_len();
        ds.offset_pick(0.5, v2(10.0, 10.0)).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        assert_eq!(ds.list[0].u.closed.len(), closed0 + 1);
        assert_eq!(u_len(&ds), lines0 + 3, "one closed copy, no duplicate edges");
    }

    #[test]
    fn box_picked_ring_insert_vertex_then_offset() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        let s = square(&mut ds, &[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        sync(&mut ed, &mut ds);
        let ring = ds.list[0].u.closed.len() - 1;
        pick(&mut ds, Pick { lines: (s..s + 4).collect(), closed: vec![ring], ..Default::default() });
        let n0 = ed.undo_len();
        drag(&mut ds, lv, v2(2.0, 0.0), v2(2.0, 0.0), true).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        assert_eq!(ds.list[0].u.closed[ring].pts.len(), 5);
        assert_eq!(ds.pick.as_ref().unwrap().1.lines.len(), 5);
        let closed0 = ds.list[0].u.closed.len();
        let lines0 = u_len(&ds);
        ds.offset_pick(0.5, v2(10.0, 10.0)).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 2);
        assert_eq!(ds.list[0].u.closed.len(), closed0 + 1);
        assert_eq!(u_len(&ds), lines0 + 5);
    }

    #[test]
    fn text_grip_moves_insert() {
        let (mut ed, mut ds) = setup();
        let lv = ed.level;
        ds.list[0].u.texts.push(UText {
            layer: "Z".into(),
            pos: (3.0, 3.0),
            height: 0.2,
            rotation: 0.0,
            text: "A".into(),
            pen: 0,
            fmt: Default::default(),
        });
        ds.touch();
        let ti = ds.list[0].u.texts.len() - 1;
        pick(
            &mut ds,
            Pick {
                texts: vec![ti],
                ..Default::default()
            },
        );
        let n0 = ed.undo_len();
        drag(&mut ds, lv, v2(3.0, 3.0), v2(6.0, 7.0), false).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        let t = &ds.list[0].u.texts[ti];
        assert!(near(t.pos, (6.0, 7.0)) && t.text == "A" && t.height == 0.2);
        // a typed distance along the drag
        let dr_from = v2(6.0, 7.0);
        assert!(ds.grip_press(lv, dr_from, TOL, false));
        ds.grip_move(lv, v2(9.0, 7.0), TOL);
        let dr = ds.edit.grip.drag.take().unwrap();
        ds.grip_drop(&dr, Some(1.0)).unwrap();
        assert!(near(ds.list[0].u.texts[ti].pos, (7.0, 7.0)));
        sync(&mut ed, &mut ds);
        ed.undo();
        sync(&mut ed, &mut ds);
        ed.undo();
        sync(&mut ed, &mut ds);
        assert!(near(ds.list[0].u.texts[ti].pos, (3.0, 3.0)));
        // not with several primitives picked
        let l = line(&mut ds, (0.0, 0.0), (1.0, 0.0));
        pick(
            &mut ds,
            Pick {
                lines: vec![l],
                texts: vec![ti],
                ..Default::default()
            },
        );
        assert!(ds.grips_of(lv).is_none());
    }

    #[test]
    fn grip_strings_in_both_languages() {
        use ddc_ui::i18n::{Catalog, Lang};
        let cyrillic = |s: &str| s.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c));
        let c = Catalog::new(Lang::En);
        for k in [
            "dwg_grips.st.len_bad",
            "dwg_grips.st.len_none",
            "dwg_grips.st.len_dir",
            "dwg_grips.st.changed",
            "dwg_grips.st.same",
            "dwg_grips.st.gone",
            "dwg_grips.st.ring_arcs",
            "dwg_grips.st.zero",
            "dwg_grips.st.shared",
            "dwg_grips.st.arc_ring",
            "dwg_grips.st.arc_flat",
            "dwg_grips.st.end_moved",
            "dwg_grips.st.vertex_moved",
            "dwg_grips.st.vertex_added",
            "dwg_grips.st.vertex_removed",
            "dwg_grips.st.arc_changed",
            "dwg_grips.st.radius",
            "dwg_grips.st.text_moved",
            "dwg_grips.st.no_vertex",
            "dwg_grips.st.vertex_last",
            "dwg_grips.st.vertex_ring",
            "dwg_grips.st.cancelled",
        ] {
            let (en, ru) = (c.t_in(Lang::En, k), c.t_in(Lang::Ru, k));
            assert_ne!(en, k, "missing in en");
            assert!(!cyrillic(&en), "{k}: {en}");
            assert!(cyrillic(&ru), "{k}: missing in ru: {ru}");
        }
    }
}
