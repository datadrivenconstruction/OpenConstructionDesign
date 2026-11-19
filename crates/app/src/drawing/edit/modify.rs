// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Modify tools for opened drawings: offset picked lines and arcs by a typed distance to a
//! clicked side, join touching lines and arcs into one closed polyline, explode polylines into
//! their lines and arcs. Each edit is one undo step ([`Step::geometry`]) and marks the drawing
//! edited like [`Drawings::add_path`].

use std::f64::consts::{PI, TAU};

use ddc_geom::{v2, V2};
use ddc_io::drawing_model::{Pick, UArc, ULine, UPoly, Underlay};

use super::super::history::Step;
use super::super::{seg_dist, Drawings};
use super::{tr, SAME};

pub(super) mod fillet;
pub(super) mod trim;

/// Ends closer than this (m) meet exactly: the DXF export's tolerance for polyline edges.
const MEET: f64 = 1e-6;

/// Distance field, side-click state and the last tolerance of the canvas.
pub(super) struct ModifyUi {
    dist: f64,
    /// The next canvas click picks the offset side.
    armed: bool,
    /// The offset passes through the clicked (snapped) point instead of taking the distance.
    pub(super) through: bool,
    /// Pick tolerance of the last canvas click (m): the join tolerance.
    tol: f64,
    /// Result of the side click, shown by the next panel frame.
    note: Option<String>,
    /// Last takeoff with the drawing, its edit generation and the pick it was made for.
    measured: Option<(u64, u64, [Vec<usize>; 4], Vec<LayerRow>)>,
}

impl Default for ModifyUi {
    fn default() -> Self {
        Self {
            dist: 0.5,
            armed: false,
            through: false,
            tol: 0.01,
            note: None,
            measured: None,
        }
    }
}

impl ModifyUi {
    /// `pick_measure` of pick `pk` in the drawing with stable id `id` (at edit generation `gen`), recomputed only
    /// when the drawing, its generation or the picked ids changed.
    pub(super) fn measure_cached(&mut self, id: u64, gen: u64, u: &Underlay, pk: &Pick) -> Measure {
        rows_total(self.measure_rows_cached(id, gen, u, pk))
    }

    /// The same cached takeoff, one row per layer (sorted by layer name).
    pub(super) fn measure_rows_cached(&mut self, id: u64, gen: u64, u: &Underlay, pk: &Pick) -> &[LayerRow] {
        let ids = [pk.lines.clone(), pk.arcs.clone(), pk.closed.clone(), pk.fills.clone()];
        let hit = matches!(&self.measured, Some((k0, g0, i0, _)) if *k0 == id && *g0 == gen && *i0 == ids);
        if !hit {
            self.measured = Some((id, gen, ids, pick_rows(u, pk)));
        }
        self.measured.as_ref().map_or(&[], |m| m.3.as_slice())
    }

    /// The offset waits for its through point.
    pub(super) fn through_armed(&self) -> bool {
        self.armed && self.through
    }

    /// A canvas click with pick tolerance `tol` (m).
    pub(super) fn clicked_with(&mut self, tol: f64) {
        if tol.is_finite() && tol > 0.0 {
            self.tol = tol;
        }
    }
}

/// Line or arc in travel order.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Geo {
    Line(V2, V2),
    /// Counter-clockwise from `a0` to `a1` (rad); `fwd`: travelled from `a0` to `a1`.
    Arc {
        c: V2,
        r: f64,
        a0: f64,
        a1: f64,
        fwd: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Src {
    Line(usize),
    Arc(usize),
}

/// A picked primitive; `flip`: travelled against its stored direction.
#[derive(Clone, Copy, Debug)]
struct Piece {
    geo: Geo,
    src: Src,
    flip: bool,
}

impl Piece {
    fn rev(&mut self) {
        self.geo.rev();
        self.flip = !self.flip;
    }
}

struct Chain {
    ps: Vec<Piece>,
    closed: bool,
}

fn at(c: V2, r: f64, a: f64) -> V2 {
    v2(c.x + r * a.cos(), c.y + r * a.sin())
}

/// `new` shifted by whole turns to lie within half a turn of `old`.
fn near_angle(old: f64, new: f64) -> f64 {
    old + (new - old + PI).rem_euclid(TAU) - PI
}

fn angle(c: V2, q: V2) -> f64 {
    (q.y - c.y).atan2(q.x - c.x)
}

impl Geo {
    fn is_arc(&self) -> bool {
        matches!(self, Geo::Arc { .. })
    }

    fn start(&self) -> V2 {
        match *self {
            Geo::Line(a, _) => a,
            Geo::Arc { c, r, a0, a1, fwd } => at(c, r, if fwd { a0 } else { a1 }),
        }
    }

    fn end(&self) -> V2 {
        match *self {
            Geo::Line(_, b) => b,
            Geo::Arc { c, r, a0, a1, fwd } => at(c, r, if fwd { a1 } else { a0 }),
        }
    }

    fn rev(&mut self) {
        match self {
            Geo::Line(a, b) => std::mem::swap(a, b),
            Geo::Arc { fwd, .. } => *fwd = !*fwd,
        }
    }

    fn full(&self) -> bool {
        matches!(*self, Geo::Arc { a0, a1, .. } if a1 - a0 >= TAU - 1e-9)
    }

    fn len(&self) -> f64 {
        match *self {
            Geo::Line(a, b) => (b - a).len(),
            Geo::Arc { r, a0, a1, .. } => r * (a1 - a0),
        }
    }

    fn dist(&self, p: V2) -> f64 {
        match *self {
            Geo::Line(a, b) => seg_dist(p, a, b),
            Geo::Arc { c, r, a0, a1, .. } => {
                if (angle(c, p) - a0).rem_euclid(TAU) <= a1 - a0 {
                    ((p - c).len() - r).abs()
                } else {
                    (p - at(c, r, a0)).len().min((p - at(c, r, a1)).len())
                }
            }
        }
    }

    /// Side of `p` to the travel direction: 1 left, -1 right, 0 on the line or circle.
    fn side(&self, p: V2) -> f64 {
        match *self {
            Geo::Line(a, b) => {
                let d = b - a;
                let h = d.cross(p - a) / d.len();
                if h.abs() <= 1e-9 {
                    0.0
                } else {
                    h.signum()
                }
            }
            Geo::Arc { c, r, fwd, .. } => {
                let h = (p - c).len() - r;
                if h.abs() <= 1e-9 {
                    0.0
                } else if (h < 0.0) == fwd {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }

    /// Moved by `s` to the left of travel (negative: right); `None` when an arc's radius
    /// would vanish.
    fn offset(&self, s: f64) -> Option<Geo> {
        match *self {
            Geo::Line(a, b) => {
                let d = b - a;
                let n = v2(-d.y, d.x) * (1.0 / d.len());
                Some(Geo::Line(a + n * s, b + n * s))
            }
            Geo::Arc { c, r, a0, a1, fwd } => {
                let r = if fwd { r - s } else { r + s };
                (r > SAME).then_some(Geo::Arc { c, r, a0, a1, fwd })
            }
        }
    }

    fn set_start(&mut self, q: V2) {
        match self {
            Geo::Line(a, _) => *a = q,
            Geo::Arc { c, a0, a1, fwd, .. } => {
                let t = if *fwd { a0 } else { a1 };
                *t = near_angle(*t, angle(*c, q));
            }
        }
    }

    fn set_end(&mut self, q: V2) {
        match self {
            Geo::Line(_, b) => *b = q,
            Geo::Arc { c, a0, a1, fwd, .. } => {
                let t = if *fwd { a1 } else { a0 };
                *t = near_angle(*t, angle(*c, q));
            }
        }
    }

    /// Still runs the way `was` ran and keeps a length.
    fn kept(&self, was: &Geo) -> bool {
        match (*self, *was) {
            (Geo::Line(a, b), Geo::Line(p, q)) => (b - a).len() > SAME && (b - a).dot(q - p) > 0.0,
            (Geo::Arc { r, a0, a1, .. }, Geo::Arc { .. }) => {
                r * (a1 - a0) > SAME && a1 - a0 <= TAU + 1e-9
            }
            _ => false,
        }
    }
}

/// `a` ends and `b` starts at `x`: arcs turn their end angle, lines take the arc's own end
/// point so the two meet exactly.
fn tie(a: &mut Geo, b: &mut Geo, x: V2) {
    if a.is_arc() {
        a.set_end(x);
    }
    if b.is_arc() {
        b.set_start(x);
    }
    let p = if a.is_arc() {
        a.end()
    } else if b.is_arc() {
        b.start()
    } else {
        x
    };
    if !a.is_arc() {
        a.set_end(p);
    }
    if !b.is_arc() {
        b.set_start(p);
    }
}

pub(super) fn cross_ll(p: V2, d: V2, q: V2, e: V2) -> Vec<V2> {
    let den = d.cross(e);
    if den.abs() <= 1e-12 * d.len() * e.len() {
        return vec![];
    }
    vec![p + d * ((q - p).cross(e) / den)]
}

pub(super) fn cross_lc(p: V2, d: V2, c: V2, r: f64) -> Vec<V2> {
    let l = d.len();
    if l <= 0.0 {
        return vec![];
    }
    let u = d * (1.0 / l);
    let f = p - c;
    let b = f.dot(u);
    let disc = b * b - (f.dot(f) - r * r);
    if disc < 0.0 {
        return vec![];
    }
    let s = disc.sqrt();
    vec![p + u * (-b - s), p + u * (-b + s)]
}

pub(super) fn cross_cc(c1: V2, r1: f64, c2: V2, r2: f64) -> Vec<V2> {
    let d = (c2 - c1).len();
    if d <= 1e-12 || d > r1 + r2 || d < (r1 - r2).abs() {
        return vec![];
    }
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let u = (c2 - c1) * (1.0 / d);
    let m = c1 + u * a;
    let n = v2(-u.y, u.x);
    vec![m + n * h, m - n * h]
}

/// Make offset `a` (ending) and `b` (starting) meet again: lines are extended or trimmed to
/// their crossing, arcs follow their circles. With no crossing a straight bridge is returned.
fn meet(a: &mut Geo, b: &mut Geo) -> Option<Geo> {
    let (e, s) = (a.end(), b.start());
    if (e - s).len() <= MEET {
        tie(a, b, e);
        return None;
    }
    let x = match (*a, *b) {
        (Geo::Line(p, q), Geo::Line(r, t)) => cross_ll(p, q - p, r, t - r),
        (Geo::Line(p, q), Geo::Arc { c, r, .. }) | (Geo::Arc { c, r, .. }, Geo::Line(p, q)) => {
            cross_lc(p, q - p, c, r)
        }
        (Geo::Arc { c: c1, r: r1, .. }, Geo::Arc { c: c2, r: r2, .. }) => {
            cross_cc(c1, r1, c2, r2)
        }
    };
    let mid = (e + s) * 0.5;
    match x
        .into_iter()
        .min_by(|u, v| (*u - mid).len().total_cmp(&(*v - mid).len()))
    {
        Some(x) => {
            tie(a, b, x);
            None
        }
        None => Some(Geo::Line(e, s)),
    }
}

/// Ring points of a closed run of pieces: piece starts, arcs sampled like imported polylines.
fn ring_of(seq: &[Geo]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for g in seq {
        let q = g.start();
        out.push((q.x, q.y));
        if let Geo::Arc { c, r, a0, a1, fwd } = *g {
            let sweep = a1 - a0;
            let n = ((sweep / (PI / 16.0)).ceil() as usize).clamp(2, 256);
            for j in 1..n {
                let k = sweep * j as f64 / n as f64;
                let p = at(c, r, if fwd { a0 + k } else { a1 - k });
                out.push((p.x, p.y));
            }
        }
    }
    out
}

/// Indices of the edges of the closed outline `poly` (layer `layer`) that `g` is, as the export
/// matches it; edge `i` runs from point `i` to point `i + 1`. Empty when `g` is not part of it.
fn member_edges(poly: &UPoly, layer: &str, g: &Geo) -> Vec<usize> {
    if poly.layer != layer || poly.circle.is_some() || poly.pts.len() < 3 {
        return vec![];
    }
    let ring: Vec<V2> = poly.pts.iter().map(|q| v2(q.0, q.1)).collect();
    let n = ring.len();
    let near = |a: V2, b: V2| (a.x - b.x).abs() <= MEET && (a.y - b.y).abs() <= MEET;
    match *g {
        Geo::Line(a, b) => (0..n)
            .filter(|&i| {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                (near(p, a) && near(q, b)) || (near(p, b) && near(q, a))
            })
            .take(1)
            .collect(),
        Geo::Arc { c, r, .. } => {
            let (s, e) = (g.start(), g.end());
            let tol = MEET.max(r * 1e-9);
            let on = |x: V2| ((x - c).len() - r).abs() <= tol;
            for i in (0..n).filter(|&i| near(ring[i], s)) {
                for &step in &[1, n - 1] {
                    let mut j = i;
                    let mut edges = vec![];
                    for k in 1..=n {
                        let prev = j;
                        j = (j + step) % n;
                        edges.push(if step == 1 { prev } else { j });
                        if near(ring[j], e) {
                            // a straight edge between the ends is a line, not this arc
                            if k >= 2 {
                                return edges;
                            }
                            break;
                        }
                        if !on(ring[j]) {
                            break;
                        }
                    }
                }
            }
            vec![]
        }
    }
}

/// `g` is an edge of the closed outline `poly` (layer `layer`) as the export matches it.
fn member(poly: &UPoly, layer: &str, g: &Geo) -> bool {
    !member_edges(poly, layer, g).is_empty()
}

/// The full circle `g` has its circle outline `poly`.
fn circle_of(poly: &UPoly, layer: &str, g: &Geo) -> bool {
    let Geo::Arc { c, r, .. } = *g else {
        return false;
    };
    g.full()
        && poly.layer == layer
        && poly.circle.is_some_and(|pr| (pr - r).abs() <= MEET)
        && poly
            .pts
            .first()
            .is_some_and(|q| ((v2(q.0, q.1) - c).len() - r).abs() <= MEET)
}

fn layer_of<'a>(u: &'a Underlay, s: Src) -> &'a str {
    match s {
        Src::Line(i) => &u.lines[i].layer,
        Src::Arc(i) => &u.arcs[i].layer,
    }
}

/// (pen, dashed) of a source; arcs are never dashed.
fn style_of(u: &Underlay, s: Src) -> (u32, bool) {
    match s {
        Src::Line(i) => (u.lines[i].pen, u.lines[i].dashed),
        Src::Arc(i) => (u.arcs[i].pen, false),
    }
}

/// The picked lines and arcs; texts, fills and outlines are refused with a reason.
fn curves(u: &Underlay, pk: &Pick) -> Result<Vec<Piece>, String> {
    if !(pk.texts.is_empty() && pk.fills.is_empty()) {
        return Err(tr("dwg_modify.st.only_curves"));
    }
    let mut out = vec![];
    let mut seen_l = vec![];
    for &i in &pk.lines {
        let Some(l) = u.lines.get(i) else { continue };
        if seen_l.contains(&i) {
            continue;
        }
        seen_l.push(i);
        let (a, b) = (v2(l.a.0, l.a.1), v2(l.b.0, l.b.1));
        if ![a.x, a.y, b.x, b.y].iter().all(|v| v.is_finite()) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        if (b - a).len() > SAME {
            out.push(Piece {
                geo: Geo::Line(a, b),
                src: Src::Line(i),
                flip: false,
            });
        }
    }
    let mut seen_a = vec![];
    for &i in &pk.arcs {
        let Some(x) = u.arcs.get(i) else { continue };
        if seen_a.contains(&i) {
            continue;
        }
        seen_a.push(i);
        let ok = [x.c.0, x.c.1, x.r, x.a0, x.a1].iter().all(|v| v.is_finite());
        if !(ok && x.r > SAME && x.a1 > x.a0) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        out.push(Piece {
            geo: Geo::Arc {
                c: v2(x.c.0, x.c.1),
                r: x.r,
                a0: x.a0,
                a1: x.a1,
                fwd: true,
            },
            src: Src::Arc(i),
            flip: false,
        });
    }
    if out.is_empty() {
        return Err(tr("dlg.dw.st.nothing_picked"));
    }
    for &r in &pk.closed {
        if u.closed.get(r).is_none() {
            return Err(tr("dlg.dw.st.drawing_gone"));
        }
        if !ring_covered(u, pk, &out, r) {
            let part = u.closed[r].circle.is_none() && ring_marks(u, pk, r).0 > 0;
            return Err(tr(if part {
                "dwg_modify.st.ring_partial"
            } else {
                "dwg_modify.st.only_curves"
            }));
        }
    }
    Ok(out)
}

/// (ring edges with a picked line or arc, ring edge count) of the polygon ring `r`.
fn ring_marks(u: &Underlay, pk: &Pick, r: usize) -> (usize, usize) {
    let Some(c) = u.closed.get(r) else {
        return (0, 0);
    };
    let mut hit = vec![false; c.pts.len()];
    let mut mark = |g: Geo, layer: &str| {
        for e in member_edges(c, layer, &g) {
            if let Some(h) = hit.get_mut(e) {
                *h = true;
            }
        }
    };
    for &i in &pk.lines {
        if let Some(l) = u.lines.get(i) {
            mark(Geo::Line(v2(l.a.0, l.a.1), v2(l.b.0, l.b.1)), &l.layer);
        }
    }
    for &i in &pk.arcs {
        if let Some(a) = u.arcs.get(i) {
            let g = Geo::Arc {
                c: v2(a.c.0, a.c.1),
                r: a.r,
                a0: a.a0,
                a1: a.a1,
                fwd: true,
            };
            mark(g, &a.layer);
        }
    }
    (hit.iter().filter(|&&h| h).count(), hit.len())
}

/// The picked ring `r` is only a box-pick echo of its edges: a circle with its full arc picked,
/// or a polygon whose every edge is covered by a picked line or arc. A stale index is not covered.
fn ring_covered(u: &Underlay, pk: &Pick, ps: &[Piece], r: usize) -> bool {
    let Some(c) = u.closed.get(r) else {
        return false;
    };
    if c.circle.is_some() {
        return ps.iter().any(|p| circle_of(c, layer_of(u, p.src), &p.geo));
    }
    let (hit, total) = ring_marks(u, pk, r);
    total >= 3 && hit == total
}

/// Pieces ordered end to start into chains. Ends within `tol` meet when exactly two ends are
/// there; with `strict`, three or more ends at one point are refused.
fn chains(ps: &[Piece], tol: f64, strict: bool) -> Result<Vec<Chain>, String> {
    let n = ps.len();
    let ends: Vec<V2> = ps.iter().flat_map(|p| [p.geo.start(), p.geo.end()]).collect();
    let near: Vec<Vec<usize>> = (0..2 * n)
        .map(|i| {
            if ps[i / 2].geo.full() {
                return vec![];
            }
            (0..2 * n)
                .filter(|&j| {
                    j / 2 != i / 2 && !ps[j / 2].geo.full() && (ends[i] - ends[j]).len() <= tol
                })
                .collect()
        })
        .collect();
    if strict && near.iter().any(|v| v.len() > 1) {
        return Err(tr("dwg_modify.st.join_branch"));
    }
    let mate: Vec<Option<usize>> = (0..2 * n)
        .map(|i| match near[i][..] {
            [j] if near[j][..] == [i] => Some(j),
            _ => None,
        })
        .collect();
    let mut used = vec![false; n];
    let mut out = vec![];
    for s in 0..n {
        if used[s] {
            continue;
        }
        used[s] = true;
        let mut ch = vec![ps[s]];
        let mut closed = ps[s].geo.full();
        let mut cur = 2 * s + 1;
        while let Some(j) = mate[cur] {
            let k = j / 2;
            if k == s {
                closed = true;
                break;
            }
            if used[k] {
                break;
            }
            used[k] = true;
            let mut p = ps[k];
            if j % 2 == 1 {
                p.rev();
            }
            ch.push(p);
            cur = j ^ 1;
        }
        if !closed {
            let mut cur = 2 * s;
            while let Some(j) = mate[cur] {
                let k = j / 2;
                if used[k] {
                    break;
                }
                used[k] = true;
                let mut p = ps[k];
                if j % 2 == 0 {
                    p.rev();
                }
                ch.insert(0, p);
                cur = j ^ 1;
            }
        }
        out.push(Chain { ps: ch, closed });
    }
    Ok(out)
}

/// Write a moved piece back over its source.
fn write_back(u: &mut Underlay, p: &Piece) {
    match (p.src, p.geo) {
        (Src::Line(i), Geo::Line(a, b)) => {
            let (a, b) = if p.flip { (b, a) } else { (a, b) };
            u.lines[i].a = (a.x, a.y);
            u.lines[i].b = (b.x, b.y);
        }
        (Src::Arc(i), Geo::Arc { a0, a1, .. }) => {
            u.arcs[i].a0 = a0;
            u.arcs[i].a1 = a1;
        }
        _ => {}
    }
}

fn len_text(m: f64) -> String {
    crate::units::display_length(m, crate::units::active_units())
}

/// Takeoff of a pick: objects, summed length and summed ring area.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Measure {
    pub n: usize,
    pub len: f64,
    pub area: f64,
}

fn ring_perimeter(p: &UPoly) -> f64 {
    if let Some(r) = p.circle {
        return TAU * r;
    }
    let n = p.pts.len();
    (0..n)
        .map(|i| (v2(p.pts[(i + 1) % n].0, p.pts[(i + 1) % n].1) - v2(p.pts[i].0, p.pts[i].1)).len())
        .sum()
}

/// Area of a hatch: the outer loop minus the inner loops (shoelace each).
fn fill_area(f: &ddc_io::drawing_model::UFill) -> f64 {
    let shoelace = |l: &Vec<(f64, f64)>| -> f64 {
        let n = l.len();
        (0..n)
            .map(|i| {
                let (a, b) = (l[i], l[(i + 1) % n]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let mut it = f.loops.iter();
    let outer = it.next().map_or(0.0, shoelace);
    (outer - it.map(shoelace).sum::<f64>()).max(0.0)
}

/// Shoelace area; bulged edges count by their chord.
fn ring_area(p: &UPoly) -> f64 {
    if let Some(r) = p.circle {
        return PI * r * r;
    }
    let n = p.pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (p.pts[i], p.pts[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

/// An edge of `ring` joins the ends `a` and `b` of a line on the ring's layer.
fn on_ring(ring: &UPoly, layer: &str, a: (f64, f64), b: (f64, f64)) -> bool {
    let n = ring.pts.len();
    let eq = |p: (f64, f64), q: (f64, f64)| (v2(p.0, p.1) - v2(q.0, q.1)).len() <= MEET;
    ring.layer == layer
        && ring.circle.is_none()
        && (0..n).any(|i| {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            (eq(p, a) && eq(q, b)) || (eq(p, b) && eq(q, a))
        })
}

/// Takeoff of one layer.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LayerRow {
    pub layer: String,
    pub m: Measure,
}

fn rows_total(rows: &[LayerRow]) -> Measure {
    rows.iter().fold(Measure::default(), |t, r| Measure {
        n: t.n + r.m.n,
        len: t.len + r.m.len,
        area: t.area + r.m.area,
    })
}

/// Count, length and area of the pick. A picked ring is one object (its perimeter and area);
/// its own edge lines and full circle arc are not counted again.
pub(super) fn pick_measure(u: &Underlay, pk: &Pick) -> Measure {
    rows_total(&pick_rows(u, pk))
}

/// `pick_measure` split by layer, rows sorted by layer name.
pub(super) fn pick_rows(u: &Underlay, pk: &Pick) -> Vec<LayerRow> {
    let mut by: std::collections::BTreeMap<String, Measure> = Default::default();
    let mut ids = pk.closed.clone();
    ids.sort_unstable();
    ids.dedup();
    let rings: Vec<&UPoly> = ids.iter().filter_map(|&r| u.closed.get(r)).collect();
    for r in &rings {
        let m = by.entry(r.layer.clone()).or_default();
        m.n += 1;
        m.len += ring_perimeter(r);
        m.area += ring_area(r);
    }
    let mut seen = vec![];
    for &i in &pk.lines {
        let Some(l) = u.lines.get(i) else { continue };
        if seen.contains(&i) || rings.iter().any(|r| on_ring(r, &l.layer, l.a, l.b)) {
            continue;
        }
        seen.push(i);
        let m = by.entry(l.layer.clone()).or_default();
        m.n += 1;
        m.len += (v2(l.b.0, l.b.1) - v2(l.a.0, l.a.1)).len();
    }
    seen.clear();
    for &i in &pk.arcs {
        let Some(x) = u.arcs.get(i) else { continue };
        let g = Geo::Arc {
            c: v2(x.c.0, x.c.1),
            r: x.r,
            a0: x.a0,
            a1: x.a1,
            fwd: true,
        };
        if seen.contains(&i) || rings.iter().any(|r| circle_of(r, &x.layer, &g)) {
            continue;
        }
        seen.push(i);
        let m = by.entry(x.layer.clone()).or_default();
        m.n += 1;
        m.len += g.len();
    }
    let mut fids = pk.fills.clone();
    fids.sort_unstable();
    fids.dedup();
    for f in fids.iter().filter_map(|&i| u.fills.get(i)) {
        let m = by.entry(f.layer.clone()).or_default();
        m.n += 1;
        m.area += fill_area(f);
    }
    by.into_iter().map(|(layer, m)| LayerRow { layer, m }).collect()
}

/// The takeoff as tab-separated text for a spreadsheet: header in the UI language with the
/// unit of each column, one row per layer, then a total row. Numbers are plain (no unit, no
/// group separator); the decimal separator follows the language.
pub(super) fn takeoff_tsv(rows: &[LayerRow], ru: bool, units: crate::units::Units, area_dec: usize) -> String {
    use crate::units::Units;
    let (len_key, area_key) = match units {
        Units::Metric => ("dwg_table.unit.mm", "dwg_table.unit.m2"),
        Units::Imperial => ("dwg_table.unit.ft", "dwg_table.unit.ft2"),
    };
    let (lu, au) = (crate::tr_lang(ru, len_key, &[]), crate::tr_lang(ru, area_key, &[]));
    let num = |v: f64, d: usize| {
        let s = crate::units::fmt_fixed(v, d);
        if ru { s.replace('.', ",") } else { s }
    };
    let len_dec = if units == Units::Metric { 0 } else { 3 };
    let len = |m: f64| {
        num(if units == Units::Metric { m * 1000.0 } else { m / crate::units::FOOT }, len_dec)
    };
    let area = |m2: f64| num(crate::units::area_in_units(m2, units), area_dec);
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = format!(
        "{}\t{}\t{}\t{}",
        crate::tr_lang(ru, "dwg_table.col.layer", &[]),
        crate::tr_lang(ru, "dwg_table.col.count", &[]),
        crate::tr_lang(ru, "dwg_table.col.len", &[("unit", &lu)]),
        crate::tr_lang(ru, "dwg_table.col.area", &[("unit", &au)]),
    );
    for r in rows {
        out.push_str(&format!("\n{}\t{}\t{}\t{}", clean(&r.layer), r.m.n, len(r.m.len), area(r.m.area)));
    }
    let t = rows_total(rows);
    out.push_str(&format!(
        "\n{}\t{}\t{}\t{}",
        crate::tr_lang(ru, "dwg_table.total", &[]),
        t.n,
        len(t.len),
        area(t.area)
    ));
    out
}

/// The measure as one label in the UI language and the project units.
pub(super) fn measure_label(m: &Measure) -> String {
    measure_label_in(m, crate::ui_is_ru())
}

fn measure_label_in(m: &Measure, ru: bool) -> String {
    let (n, len) = (m.n.to_string(), len_text(m.len));
    if m.area > 0.0 {
        crate::tr_lang(ru, "dwg_modify.measure_area", &[("n", &n), ("len", &len), ("area", &crate::units::fmt_area(m.area))])
    } else {
        crate::tr_lang(ru, "dwg_modify.measure", &[("n", &n), ("len", &len)])
    }
}

impl Drawings {
    /// Offset the picked lines and arcs so that the result passes through `through`.
    pub fn offset_through(&mut self, through: V2) -> Result<String, String> {
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let ps = curves(&d.u, &pk)?;
        let dist = ps.iter().map(|p| p.geo.dist(through)).fold(f64::INFINITY, f64::min);
        if !(dist.is_finite() && dist > SAME) {
            return Err(tr("dwg_modify.st.on_line"));
        }
        self.offset_pick(dist, through)
    }

    /// Offset the picked lines and arcs by `dist` (m) to the side of `side`: new primitives
    /// on the same layers and pens. Touching pieces are offset as one chain whose corners meet
    /// again; a closed polyline gives a closed polyline.
    pub fn offset_pick(&mut self, dist: f64, side: V2) -> Result<String, String> {
        if !(dist.is_finite() && dist > SAME) {
            return Err(tr("dwg_modify.st.dist_bad"));
        }
        if !(side.x.is_finite() && side.y.is_finite()) {
            return Err(tr("dwg_edit.st.path_bad"));
        }
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        let ps = curves(u, &pk)?;
        let (mut lines, mut arcs, mut polys) = (vec![], vec![], vec![]);
        for ch in chains(&ps, MEET, false)? {
            let Some(near) = ch
                .ps
                .iter()
                .min_by(|a, b| a.geo.dist(side).total_cmp(&b.geo.dist(side)))
            else {
                continue;
            };
            let sg = near.geo.side(side);
            if sg == 0.0 {
                return Err(tr("dwg_modify.st.on_line"));
            }
            let moved: Option<Vec<Geo>> = ch.ps.iter().map(|p| p.geo.offset(sg * dist)).collect();
            let Some(first) = moved else {
                return Err(tr("dwg_modify.st.arc_small"));
            };
            let mut og = first.clone();
            let m = og.len();
            let joints = if ch.closed && m > 1 { m } else { m - 1 };
            let mut bridges: Vec<Option<Geo>> = vec![None; m];
            for i in 0..joints {
                let j = (i + 1) % m;
                let (mut a, mut b) = (og[i], og[j]);
                bridges[i] = meet(&mut a, &mut b);
                og[i] = a;
                og[j] = b;
            }
            if og.iter().zip(&first).any(|(g, was)| !g.kept(was)) {
                return Err(tr("dwg_modify.st.collapse"));
            }
            let mut seq = vec![];
            for (i, g) in og.iter().enumerate() {
                let src = ch.ps[i].src;
                let layer = layer_of(u, src).to_string();
                let (pen, dashed) = style_of(u, src);
                seq.push(*g);
                match *g {
                    Geo::Line(a, b) => lines.push(ULine {
                        layer: layer.clone(),
                        a: (a.x, a.y),
                        b: (b.x, b.y),
                        dashed,
                        pen,
                    }),
                    Geo::Arc { c, r, a0, a1, .. } => {
                        let turns = (a0 / TAU).floor() * TAU;
                        arcs.push(UArc {
                            layer: layer.clone(),
                            c: (c.x, c.y),
                            r,
                            a0: a0 - turns,
                            a1: a1 - turns,
                            pen,
                        });
                    }
                }
                if let Some(Geo::Line(a, b)) = bridges[i] {
                    seq.push(Geo::Line(a, b));
                    lines.push(ULine {
                        layer,
                        a: (a.x, a.y),
                        b: (b.x, b.y),
                        dashed,
                        pen,
                    });
                }
            }
            if !ch.closed {
                continue;
            }
            let layer = layer_of(u, ch.ps[0].src).to_string();
            if m == 1 {
                let src_circle = u.closed.iter().any(|c| circle_of(c, &layer, &ch.ps[0].geo));
                if let (true, Geo::Arc { c, r, .. }) = (src_circle, og[0]) {
                    polys.push(UPoly {
                        layer,
                        pts: ddc_io::dxf::lower::circle_ring((c.x, c.y), r),
                        circle: Some(r),
                    });
                }
                continue;
            }
            let was_poly = u.closed.iter().any(|c| {
                ch.ps
                    .iter()
                    .all(|p| member(c, layer_of(u, p.src), &p.geo))
            });
            if was_poly {
                polys.push(UPoly {
                    layer,
                    pts: ring_of(&seq),
                    circle: None,
                });
            }
        }
        let n = lines.len() + arcs.len();
        if n == 0 {
            return Err(tr("dlg.dw.st.nothing_picked"));
        }
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        self.steps.push(Step::geometry(d));
        d.u.lines.extend(lines);
        d.u.arcs.extend(arcs);
        d.u.closed.extend(polys);
        d.sync_layers();
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(crate::tr(
            "dwg_modify.st.offset_done",
            &[("n", &n.to_string()), ("d", &len_text(dist))],
        ))
    }

    /// Join the picked lines and arcs, touching end to end within `tol` (m), into one closed
    /// polyline: line ends move onto the shared points, arcs stay as they are.
    pub fn join_pick(&mut self, tol: f64) -> Result<String, String> {
        let tol = if tol.is_finite() && tol > 0.0 { tol } else { MEET };
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        let ps = curves(u, &pk)?;
        if ps.len() < 2 {
            return Err(tr("dwg_modify.st.join_few"));
        }
        let layer = layer_of(u, ps[0].src).to_string();
        let pen = style_of(u, ps[0].src).0;
        let dash: Vec<bool> = ps
            .iter()
            .filter(|p| matches!(p.src, Src::Line(_)))
            .map(|p| style_of(u, p.src).1)
            .collect();
        let same = ps
            .iter()
            .all(|p| layer_of(u, p.src) == layer && style_of(u, p.src).0 == pen);
        if !same || dash.iter().any(|x| *x != dash[0]) {
            return Err(tr("dwg_modify.st.join_style"));
        }
        if ps.iter().any(|p| p.geo.full()) {
            return Err(tr("dwg_modify.st.join_circle"));
        }
        if ps.iter().any(|p| p.geo.len() <= tol) {
            return Err(tr("dwg_modify.st.join_short"));
        }
        if u
            .closed
            .iter()
            .any(|c| ps.iter().any(|p| member(c, &layer, &p.geo)))
        {
            return Err(tr("dwg_modify.st.join_in_poly"));
        }
        let mut cs = chains(&ps, tol, true)?;
        if cs.len() != 1 {
            return Err(tr("dwg_modify.st.join_split"));
        }
        let Some(Chain { ps: mut seq, closed }) = cs.pop() else {
            return Err(tr("dwg_modify.st.join_split"));
        };
        if !closed {
            let gap = seq.last().map_or(0.0, |l| (l.geo.end() - seq[0].geo.start()).len());
            return Err(crate::tr("dwg_modify.st.join_open", &[("gap", &len_text(gap))]));
        }
        let m = seq.len();
        for i in 0..m {
            let j = (i + 1) % m;
            let (mut a, mut b) = (seq[i].geo, seq[j].geo);
            let (e, s) = (a.end(), b.start());
            if a.is_arc() && b.is_arc() && (e - s).len() > MEET {
                return Err(tr("dwg_modify.st.join_arcs_gap"));
            }
            let x = if a.is_arc() {
                e
            } else if b.is_arc() {
                s
            } else {
                (e + s) * 0.5
            };
            tie(&mut a, &mut b, x);
            seq[i].geo = a;
            seq[j].geo = b;
        }
        if seq.iter().zip(&ps_by_src(&ps, &seq)).any(|(p, was)| !p.geo.kept(was)) {
            return Err(tr("dwg_modify.st.join_short"));
        }
        let geos: Vec<Geo> = seq.iter().map(|p| p.geo).collect();
        let ring = ring_of(&geos);
        if ring.len() < 3 {
            return Err(tr("dwg_edit.st.path_closed_short"));
        }
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        self.steps.push(Step::geometry(d));
        for p in &seq {
            write_back(&mut d.u, p);
        }
        d.u.closed.push(UPoly {
            layer: layer.clone(),
            pts: ring,
            circle: None,
        });
        d.sync_layers();
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(crate::tr(
            "dwg_modify.st.join_done",
            &[("n", &m.to_string()), ("layer", &layer)],
        ))
    }

    /// Explode the polylines the picked lines and arcs belong to: the outlines go, their lines
    /// and arcs stay and are written on their own.
    pub fn explode_pick(&mut self) -> Result<String, String> {
        let Some((k, pk)) = self.pick.clone() else {
            return Err(tr("dlg.dw.st.nothing_picked"));
        };
        let Some(d) = self.list.get(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        let u = &d.u;
        let ps = curves(u, &pk)?;
        let mut hit = vec![];
        let mut circles = false;
        for (i, c) in u.closed.iter().enumerate() {
            if c.circle.is_some() {
                circles |= ps.iter().any(|p| circle_of(c, layer_of(u, p.src), &p.geo));
            } else if ps.iter().any(|p| member(c, layer_of(u, p.src), &p.geo)) {
                hit.push(i);
            }
        }
        if hit.is_empty() {
            let key = if circles {
                "dwg_modify.st.explode_circle"
            } else {
                "dwg_modify.st.explode_none"
            };
            return Err(tr(key));
        }
        let Some(d) = self.list.get_mut(k) else {
            return Err(tr("dlg.dw.st.drawing_gone"));
        };
        self.steps.push(Step::geometry(d));
        for &i in hit.iter().rev() {
            d.u.closed.remove(i);
        }
        d.sync_layers();
        d.mark_edited();
        self.drop_recog(k);
        self.touch();
        Ok(crate::tr(
            "dwg_modify.st.explode_done",
            &[("n", &hit.len().to_string())],
        ))
    }

    /// Canvas click while the offset waits for its side: offset there. `true` when taken.
    pub(super) fn offset_click(&mut self, level: i64, p: V2, tol: f64) -> bool {
        if !self.edit.modify.armed || self.edit.draw.is_some() {
            return false;
        }
        self.edit.modify.armed = false;
        let shown = self
            .pick
            .as_ref()
            .and_then(|(k, _)| self.list.get(*k))
            .is_some_and(|d| d.level == level && d.visible);
        if !shown {
            return false;
        }
        let r = if self.edit.modify.through {
            let (q, _) = self.snap_point(level, p, tol);
            self.offset_through(q)
        } else {
            self.offset_pick(self.edit.modify.dist, p)
        };
        self.edit.modify.note = Some(r.unwrap_or_else(|e| e));
        true
    }

    /// Offset, join and explode buttons next to Draw. Returns a status line.
    pub(super) fn modify_ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut status = self.edit.modify.note.take();
        let n = self
            .pick
            .as_ref()
            .map_or(0, |(_, p)| p.lines.len() + p.arcs.len());
        let any = self.pick.as_ref().is_some_and(|(_, p)| !p.closed.is_empty() || !p.fills.is_empty());
        if n == 0 || self.edit.draw.is_some() {
            self.edit.modify.armed = false;
        }
        if let Some((k, pk)) = self.pick.as_ref().filter(|_| n > 0 || any) {
            if let Some(d) = self.list.get(*k) {
                let rows = self.edit.modify.measure_rows_cached(d.id, d.gen, &d.u, pk);
                ui.horizontal(|ui| {
                    ui.label(measure_label(&rows_total(rows)));
                    if ui.button(tr("dwg_table.copy")).on_hover_text(tr("dwg_table.copy_tip")).clicked() {
                        let t = takeoff_tsv(rows, crate::ui_is_ru(), crate::units::active_units(), crate::units::precision().0);
                        ui.ctx().copy_text(t);
                        status = Some(tr("dwg_table.copied"));
                    }
                });
            }
        }
        ui.add_enabled_ui(n > 0 && self.edit.draw.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut self.edit.modify.dist)
                        .speed(0.01)
                        .range(0.0..=1.0e6)
                        .suffix(crate::tr("dlg.dw.m", &[])),
                )
                .on_hover_text(tr("dwg_modify.dist_tip"));
                let armed = self.edit.modify.armed;
                let label = if armed {
                    tr("dwg_modify.offset_cancel")
                } else {
                    tr("dwg_modify.offset")
                };
                if ui
                    .selectable_label(armed, label)
                    .on_hover_text(tr("dwg_modify.offset_tip"))
                    .clicked()
                {
                    let dist = self.edit.modify.dist;
                    if armed {
                        self.edit.modify.armed = false;
                    } else if self.edit.modify.through || (dist.is_finite() && dist > SAME) {
                        self.edit.modify.armed = true;
                        status = Some(tr("dwg_modify.st.pick_side"));
                    } else {
                        status = Some(tr("dwg_modify.st.dist_bad"));
                    }
                }
                ui.checkbox(&mut self.edit.modify.through, tr("dwg_snap.through"))
                    .on_hover_text(tr("dwg_snap.through_tip"));
                if ui
                    .button(tr("dwg_modify.join"))
                    .on_hover_text(tr("dwg_modify.join_tip"))
                    .clicked()
                {
                    let tol = self.edit.modify.tol;
                    status = Some(self.join_pick(tol).unwrap_or_else(|e| e));
                }
                if ui
                    .button(tr("dwg_modify.explode"))
                    .on_hover_text(tr("dwg_modify.explode_tip"))
                    .clicked()
                {
                    status = Some(self.explode_pick().unwrap_or_else(|e| e));
                }
            });
        });
        status
    }
}

/// The pieces of `seq` as they were before their ends moved, by source.
fn ps_by_src(ps: &[Piece], seq: &[Piece]) -> Vec<Geo> {
    seq.iter()
        .map(|s| {
            let mut g = ps
                .iter()
                .find(|p| p.src == s.src)
                .map_or(s.geo, |p| p.geo);
            if s.flip {
                g.rev();
            }
            g
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::super::Origin;
    use super::*;
    use crate::editor::Editor;
    use ddc_io::dxf::{Common, DxfDocument, Entity, LwPolyline, LwVertex};

    const EPS: f64 = 1e-9;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < EPS && (a.1 - b.1).abs() < EPS
    }

    /// One drawing on the editor's level, in metres, from `doc`.
    fn setup(doc: &DxfDocument) -> (Editor, Drawings) {
        let ed = Editor::new_in_memory().unwrap();
        let mut ds = Drawings::default();
        ds.add(doc, "m", None, ed.level, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
        ds.sel = 0;
        (ed, ds)
    }

    fn empty() -> (Editor, Drawings) {
        let mut doc = DxfDocument::default();
        doc.entities.push(Entity::Line(ddc_io::dxf::Line {
            common: layer("Z"),
            start: [100.0, 100.0, 0.0],
            end: [101.0, 100.0, 0.0],
        }));
        setup(&doc)
    }

    fn layer(name: &str) -> Common {
        Common {
            layer: name.into(),
            ..Default::default()
        }
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
        u.lines.len() - 1
    }

    fn arc(ds: &mut Drawings, c: (f64, f64), r: f64, a0: f64, a1: f64) -> usize {
        let u = &mut ds.list[0].u;
        u.arcs.push(UArc {
            layer: "Z".into(),
            c,
            r,
            a0,
            a1,
            pen: 0,
        });
        u.arcs.len() - 1
    }

    fn pick(ds: &mut Drawings, lines: &[usize], arcs: &[usize]) {
        ds.pick = Some((
            0,
            Pick {
                lines: lines.to_vec(),
                arcs: arcs.to_vec(),
                ..Default::default()
            },
        ));
    }

    fn polylines(doc: &DxfDocument) -> usize {
        doc.entities
            .iter()
            .filter(|e| matches!(e, Entity::LwPolyline(_)))
            .count()
    }

    fn export(ds: &Drawings) -> DxfDocument {
        let mut doc = DxfDocument::default();
        ds.export_into(&mut doc);
        doc
    }

    #[test]
    fn offset_through_point_snaps_to_circle_centre() {
        let (mut ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (4.0, 0.0));
        arc(&mut ds, (2.0, 3.0), 1.0, 0.0, TAU);
        pick(&mut ds, &[li], &[]);
        sync(&mut ed, &mut ds);
        ds.edit.modify.armed = true;
        ds.edit.modify.through = true;
        let lv = ds.list[0].level;
        assert!(ds.offset_click(lv, v2(2.02, 3.01), 0.1));
        let l = ds.list[0].u.lines.last().unwrap().clone();
        assert!(near(l.a, (0.0, 3.0)) && near(l.b, (4.0, 3.0)), "through the centre: {l:?}");
    }

    #[test]
    fn offset_line_and_arc_side_and_distance() {
        let (mut ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (4.0, 0.0));
        let ai = arc(&mut ds, (10.0, 0.0), 2.0, 0.0, PI);
        let n0 = ed.undo_len();
        pick(&mut ds, &[li], &[]);
        // bad distances: refused, no step
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!ds.offset_pick(bad, v2(1.0, 1.0)).unwrap_err().is_empty());
        }
        // a click on the line has no side
        assert!(ds.offset_pick(0.5, v2(2.0, 0.0)).is_err());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert!(!ds.list[0].dirty);

        let lines0 = ds.list[0].u.lines.len();
        ds.offset_pick(0.5, v2(1.0, 3.0)).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one offset, one step");
        let d = &ds.list[0];
        assert!(d.dirty);
        assert_eq!(d.u.lines.len(), lines0 + 1);
        let l = d.u.lines.last().unwrap().clone();
        assert!(near(l.a, (0.0, 0.5)) && near(l.b, (4.0, 0.5)), "above: {l:?}");
        assert_eq!((l.layer.as_str(), l.pen, l.dashed), ("Z", 0, false));
        // the other side, another distance
        ds.offset_pick(1.25, v2(3.0, -0.01)).unwrap();
        let l = ds.list[0].u.lines.last().unwrap().clone();
        assert!(near(l.a, (0.0, -1.25)) && near(l.b, (4.0, -1.25)), "below: {l:?}");

        // arc: a click outside grows the radius, inside shrinks it; angles stay
        pick(&mut ds, &[], &[ai]);
        let arcs0 = ds.list[0].u.arcs.len();
        ds.offset_pick(0.5, v2(10.0, 5.0)).unwrap();
        let a = ds.list[0].u.arcs.last().unwrap().clone();
        assert!((a.r - 2.5).abs() < EPS && near(a.c, (10.0, 0.0)));
        assert!(a.a0.abs() < EPS && (a.a1 - PI).abs() < EPS);
        ds.offset_pick(0.5, v2(10.0, 1.0)).unwrap();
        let a = ds.list[0].u.arcs.last().unwrap().clone();
        assert!((a.r - 1.5).abs() < EPS, "inside: {a:?}");
        assert_eq!(ds.list[0].u.arcs.len(), arcs0 + 2);
        // inward past the centre: refused with a reason
        sync(&mut ed, &mut ds);
        let n1 = ed.undo_len();
        assert!(!ds.offset_pick(2.0, v2(10.0, 1.0)).unwrap_err().is_empty());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n1);

        // two touching lines: one chain, the corner meets exactly
        let p = line(&mut ds, (20.0, 0.0), (24.0, 0.0));
        let q = line(&mut ds, (24.0, 0.0), (24.0, 3.0));
        pick(&mut ds, &[p, q], &[]);
        let lines1 = ds.list[0].u.lines.len();
        ds.offset_pick(1.0, v2(23.0, 1.0)).unwrap();
        let u = &ds.list[0].u;
        assert_eq!(u.lines.len(), lines1 + 2);
        let (l1, l2) = (&u.lines[lines1], &u.lines[lines1 + 1]);
        assert!(near(l1.a, (20.0, 1.0)) && near(l1.b, (23.0, 1.0)), "{l1:?}");
        assert!(near(l2.a, (23.0, 1.0)) && near(l2.b, (23.0, 3.0)), "{l2:?}");

        // undo takes one offset back at a time
        sync(&mut ed, &mut ds);
        let lines2 = ds.list[0].u.lines.len();
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.lines.len(), lines2 - 2);
    }

    fn box_ring(ds: &mut Drawings, n_edges: usize) {
        let s = ds.list[0].u.lines.len();
        ds.add_path(&[v2(0.0, 0.0), v2(4.0, 0.0), v2(4.0, 4.0), v2(0.0, 4.0)], true)
            .unwrap();
        let ring = ds.list[0].u.closed.len() - 1;
        ds.pick = Some((
            0,
            Pick {
                lines: (s..s + n_edges).collect(),
                closed: vec![ring],
                ..Default::default()
            },
        ));
    }

    #[test]
    fn box_picked_polyline_offsets() {
        let (_ed, mut ds) = empty();
        // one edge of the ring picked: the ring is not covered, refused
        box_ring(&mut ds, 1);
        assert!(!ds.offset_pick(0.5, v2(2.0, -1.0)).unwrap_err().is_empty());
        // the whole polyline picked by a box, in a fresh drawing so the first square is absent
        let (mut ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        // drawing the ring is its own step: count from after it
        sync(&mut ed, &mut ds);
        let closed0 = ds.list[0].u.closed.len();
        let n0 = ed.undo_len();
        ds.offset_pick(0.5, v2(2.0, -1.0)).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1);
        assert_eq!(ds.list[0].u.closed.len(), closed0 + 1, "the copy is a polyline too");
    }

    #[test]
    fn stale_ring_index_refused() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        let (_, mut pk) = ds.pick.clone().unwrap();
        pk.closed = vec![ds.list[0].u.closed.len() + 5];
        assert!(!ring_covered(&ds.list[0].u, &pk, &[], pk.closed[0]));
        ds.pick = Some((0, pk));
        assert_eq!(ds.offset_pick(0.5, v2(2.0, -1.0)).unwrap_err(), tr("dlg.dw.st.drawing_gone"));
    }

    #[test]
    fn ring_with_missing_edge_not_covered() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        // the last edge loses its line: the other three are picked but the ring is not covered
        let last = ds.list[0].u.lines.len() - 1;
        ds.list[0].u.lines.remove(last);
        let (_, mut pk) = ds.pick.clone().unwrap();
        pk.lines = (0..last).collect();
        let r = pk.closed[0];
        assert!(!ring_covered(&ds.list[0].u, &pk, &[], r));
    }

    #[test]
    fn partial_ring_reason_key() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 2);
        let e = ds.offset_pick(0.5, v2(2.0, -1.0)).unwrap_err();
        assert_eq!(e, tr("dwg_modify.st.ring_partial"));
        assert_ne!(e, "dwg_modify.st.ring_partial");
        assert_ne!(e, tr("dwg_modify.st.only_curves"));
    }

    #[test]
    fn explode_keeps_ring_edges() {
        let (mut ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        let lines0 = ds.list[0].u.lines.len();
        let closed0 = ds.list[0].u.closed.len();
        ds.explode_pick().unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ds.list[0].u.closed.len(), closed0 - 1);
        assert_eq!(ds.list[0].u.lines.len(), lines0, "edges stay put");
    }

    #[test]
    fn offset_refuses_text_with_reason() {
        let (mut ed, mut ds) = empty();
        let li = line(&mut ds, (0.0, 0.0), (4.0, 0.0));
        ds.list[0].u.texts.push(ddc_io::drawing_model::UText {
            layer: "Z".into(),
            pos: (1.0, 1.0),
            height: 0.2,
            rotation: 0.0,
            text: "A".into(),
            pen: 0,
            fmt: Default::default(),
        });
        let ti = ds.list[0].u.texts.len() - 1;
        let n0 = ed.undo_len();
        let v0 = ds.version;
        let before = ds.list[0].u.clone();
        for lines in [vec![], vec![li]] {
            ds.pick = Some((
                0,
                Pick {
                    lines,
                    texts: vec![ti],
                    ..Default::default()
                },
            ));
            let e = ds.offset_pick(0.5, v2(1.0, 3.0)).unwrap_err();
            assert_eq!(e, tr("dwg_modify.st.only_curves"));
            assert!(!e.is_empty());
            assert!(ds.join_pick(0.01).is_err());
            assert!(ds.explode_pick().is_err());
        }
        // nothing picked: refused too
        ds.pick = None;
        assert!(!ds.offset_pick(0.5, v2(1.0, 3.0)).unwrap_err().is_empty());
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert_eq!(ds.version, v0);
        assert!(!ds.list[0].dirty);
        assert_eq!(ds.list[0].u, before);
    }

    #[test]
    fn join_lines_into_polyline_one_undo() {
        let (mut ed, mut ds) = empty();
        // a square whose corners miss by up to 4 mm, an arc closing the top
        let a = line(&mut ds, (0.0, 0.0), (4.0, 0.0));
        let b = line(&mut ds, (4.003, 0.002), (4.0, 2.0));
        let c = arc(&mut ds, (2.0, 2.0), 2.0, 0.0, PI);
        let e = line(&mut ds, (0.0, 2.0), (0.0, 0.004));
        let n0 = ed.undo_len();
        let closed0 = ds.list[0].u.closed.len();
        // too few, open, a gap wider than the tolerance: refused, no step
        pick(&mut ds, &[a], &[]);
        assert!(ds.join_pick(0.01).is_err());
        pick(&mut ds, &[a, b], &[]);
        assert!(!ds.join_pick(0.01).unwrap_err().is_empty(), "open chain");
        pick(&mut ds, &[a, b, e], &[c]);
        assert!(ds.join_pick(0.001).is_err(), "gaps wider than the tolerance");
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0);
        assert!(!ds.list[0].dirty);

        ds.join_pick(0.01).unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one join, one step");
        let u = &ds.list[0].u;
        assert!(ds.list[0].dirty);
        assert_eq!(u.closed.len(), closed0 + 1);
        // the ends meet exactly: a and b at their midpoint, e on the arc's end
        let mid = ((4.0 + 4.003) / 2.0, 0.001);
        assert!(near(u.lines[a].b, mid) && near(u.lines[b].a, mid));
        assert!(near(u.lines[e].b, u.lines[a].a), "{:?}", u.lines[e]);
        assert!(near(u.lines[e].b, (0.0, 0.002)));
        assert!(near(u.lines[b].b, (4.0, 2.0)) && near(u.lines[e].a, (0.0, 2.0)));
        assert_eq!(u.arcs[c].r, 2.0);
        // written as one closed polyline with the arc as a bulge
        let doc = export(&ds);
        assert_eq!(polylines(&doc), 1);
        let pl = doc
            .entities
            .iter()
            .find_map(|x| match x {
                Entity::LwPolyline(p) => Some(p.clone()),
                _ => None,
            })
            .unwrap();
        assert!(pl.closed() && pl.vertices.len() == 4);
        assert!(pl.vertices.iter().any(|v| (v.bulge.abs() - 1.0).abs() < 1e-9));
        // joined again: refused, it is one polyline already
        assert!(ds.join_pick(0.01).is_err());

        ed.undo();
        ds.sync_undo(&mut ed);
        let u = &ds.list[0].u;
        assert_eq!(u.closed.len(), closed0);
        assert!(near(u.lines[a].b, (4.0, 0.0)) && near(u.lines[e].b, (0.0, 0.004)));
        assert_eq!(ed.undo_len(), n0);
        ed.redo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.closed.len(), closed0 + 1);
    }

    #[test]
    fn explode_polyline_roundtrip_dxf() {
        // a closed polyline: three straight edges and a half circle on top
        let v = |x: f64, y: f64, bulge: f64| LwVertex {
            x,
            y,
            bulge,
            ..Default::default()
        };
        let mut doc = DxfDocument::default();
        doc.entities.push(Entity::LwPolyline(LwPolyline {
            common: layer("P"),
            flags: 1,
            vertices: vec![v(0.0, 0.0, 0.0), v(4.0, 0.0, 0.0), v(4.0, 2.0, 1.0), v(0.0, 2.0, 0.0)],
        }));
        let (mut ed, mut ds) = setup(&doc);
        let u0 = ds.list[0].u.clone();
        assert_eq!((u0.lines.len(), u0.arcs.len(), u0.closed.len()), (3, 1, 1));
        assert_eq!(polylines(&export(&ds)), 1);
        // nothing a polyline: refused
        let mut lone = DxfDocument::default();
        lone.entities.push(Entity::Line(ddc_io::dxf::Line {
            common: layer("P"),
            start: [9.0, 9.0, 0.0],
            end: [10.0, 9.0, 0.0],
        }));
        let (_, mut other) = setup(&lone);
        other.pick = Some((
            0,
            Pick {
                lines: vec![0],
                ..Default::default()
            },
        ));
        assert!(!other.explode_pick().unwrap_err().is_empty());

        let n0 = ed.undo_len();
        pick(&mut ds, &[1], &[]);
        ds.explode_pick().unwrap();
        sync(&mut ed, &mut ds);
        assert_eq!(ed.undo_len(), n0 + 1, "one explode, one step");
        let d = &ds.list[0];
        assert!(d.dirty);
        assert!(d.u.closed.is_empty());
        assert_eq!((&d.u.lines, &d.u.arcs), (&u0.lines, &u0.arcs), "pieces stay put");
        // written as loose lines and an arc, read back the same with no outline
        let out = export(&ds);
        assert_eq!(polylines(&out), 0);
        let count = |f: fn(&Entity) -> bool| out.entities.iter().filter(|e| f(e)).count();
        assert_eq!(count(|e| matches!(e, Entity::Line(_))), 3);
        assert_eq!(count(|e| matches!(e, Entity::Arc(_))), 1);
        let back = Underlay::from_dxf(&out, 1.0);
        assert!(back.closed.is_empty());
        assert_eq!((back.lines.len(), back.arcs.len()), (3, 1));
        for l in &u0.lines {
            assert!(back
                .lines
                .iter()
                .any(|m| (near(m.a, l.a) && near(m.b, l.b)) || (near(m.a, l.b) && near(m.b, l.a))));
        }
        let (a, b) = (&back.arcs[0], &u0.arcs[0]);
        assert!(near(a.c, b.c) && (a.r - b.r).abs() < EPS);
        // exploded again: nothing left to explode
        assert!(ds.explode_pick().is_err());
        // join puts it back as one polyline; undo of the join, then of the explode
        pick(&mut ds, &[0, 1, 2], &[0]);
        ds.join_pick(0.01).unwrap();
        assert_eq!(polylines(&export(&ds)), 1);
        sync(&mut ed, &mut ds);
        ed.undo();
        ds.sync_undo(&mut ed);
        assert!(ds.list[0].u.closed.is_empty());
        ed.undo();
        ds.sync_undo(&mut ed);
        assert_eq!(ds.list[0].u.closed, u0.closed);
        assert_eq!(ed.undo_len(), n0);
    }

    #[test]
    fn measure_two_lines_and_arc_length() {
        let (_ed, mut ds) = empty();
        let a = line(&mut ds, (0.0, 0.0), (3.0, 4.0));
        let b = line(&mut ds, (3.0, 4.0), (3.0, 6.0));
        let c = arc(&mut ds, (10.0, 0.0), 2.0, 0.0, PI);
        pick(&mut ds, &[a, b, a], &[c]);
        let (_, pk) = ds.pick.clone().unwrap();
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 3);
        assert!((m.len - (5.0 + 2.0 + 2.0 * PI)).abs() < EPS, "{m:?}");
        assert_eq!(m.area, 0.0);
    }

    #[test]
    fn measure_box_ring_area() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        let (_, pk) = ds.pick.clone().unwrap();
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 1, "ring and its edges are one object");
        assert!((m.area - 16.0).abs() < EPS && (m.len - 16.0).abs() < EPS, "{m:?}");
        // one free line besides the ring still counts
        let l = line(&mut ds, (20.0, 0.0), (22.0, 0.0));
        let (k, mut pk) = ds.pick.clone().unwrap();
        pk.lines.push(l);
        ds.pick = Some((k, pk.clone()));
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 2);
        assert!((m.len - 18.0).abs() < EPS);
    }

    fn fill(ds: &mut Drawings, loops: Vec<Vec<(f64, f64)>>) -> usize {
        let u = &mut ds.list[0].u;
        u.fills.push(ddc_io::drawing_model::UFill {
            layer: "Z".into(),
            loops,
            solid: true,
            pattern: String::new(),
            segs: vec![],
            pen: 0,
        });
        u.fills.len() - 1
    }

    fn sq(x: f64, y: f64, s: f64) -> Vec<(f64, f64)> {
        vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s)]
    }

    #[test]
    fn measure_fill_area_with_hole() {
        let (_ed, mut ds) = empty();
        let f = fill(&mut ds, vec![sq(0.0, 0.0, 10.0), sq(2.0, 2.0, 3.0)]);
        ds.pick = Some((0, Pick { fills: vec![f, f], ..Default::default() }));
        let (_, pk) = ds.pick.clone().unwrap();
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 1, "picked twice, counted once");
        assert!((m.area - 91.0).abs() < EPS, "{m:?}");
        assert_eq!(m.len, 0.0);
        // the cache key includes the fills
        let mut mu = ModifyUi::default();
        let g = ds.list[0].gen;
        assert!((mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk).area - 91.0).abs() < EPS);
        let f2 = fill(&mut ds, vec![sq(20.0, 0.0, 2.0)]);
        let pk2 = Pick { fills: vec![f, f2], ..Default::default() };
        let m2 = mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk2);
        assert_eq!(m2.n, 2);
        assert!((m2.area - 95.0).abs() < EPS, "{m2:?}");
    }

    #[test]
    fn measure_ring_only_pick_labelled() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 0);
        let (_, pk) = ds.pick.clone().unwrap();
        assert!(pk.lines.is_empty() && pk.arcs.is_empty());
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 1);
        assert!((m.area - 16.0).abs() < EPS && (m.len - 16.0).abs() < EPS, "{m:?}");
        let s = measure_label_in(&m, false);
        assert!(s.contains('1') && !s.contains('{'), "{s}");
    }

    #[test]
    fn pick_measure_cached_until_edit() {
        let (_ed, mut ds) = empty();
        let a = line(&mut ds, (0.0, 0.0), (3.0, 4.0));
        pick(&mut ds, &[a], &[]);
        let (_, pk) = ds.pick.clone().unwrap();
        let mut mu = ModifyUi::default();
        let g = ds.list[0].gen;
        let m1 = mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk);
        assert!((m1.len - 5.0).abs() < EPS);
        // same generation: the stored result is returned even if the data moved unseen
        ds.list[0].u.lines[a].b = (6.0, 8.0);
        assert_eq!(mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk), m1);
        ds.list[0].mark_edited();
        let g2 = ds.list[0].gen;
        let m2 = mu.measure_cached(ds.list[0].id, g2, &ds.list[0].u, &pk);
        assert!((m2.len - 10.0).abs() < EPS, "{m2:?}");
    }

    #[test]
    fn measure_cache_not_reused_after_drawing_removed() {
        let (ed, mut ds) = empty();
        let a = line(&mut ds, (0.0, 0.0), (3.0, 4.0));
        ds.add(&DxfDocument::default(), "n", None, ed.level, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
        for b in [(0.0, 1.0), (6.0, 8.0)] {
            ds.list[1].u.lines.push(ULine { layer: "Z".into(), a: (0.0, 0.0), b, dashed: false, pen: 0 });
        }
        // same line index in both drawings, same edit generation
        let pk = Pick { lines: vec![a], ..Default::default() };
        let (id0, id1) = (ds.list[0].id, ds.list[1].id);
        assert_ne!(id0, id1);
        let mut mu = ModifyUi::default();
        let m0 = mu.measure_cached(id0, ds.list[0].gen, &ds.list[0].u, &pk);
        assert!((m0.len - 5.0).abs() < EPS);
        // drawing 0 removed: drawing 1 shifts to index 0 with the same picked ids
        ds.take(0).unwrap();
        assert_eq!(ds.list[0].id, id1);
        let d = &ds.list[0];
        let m1 = mu.measure_cached(d.id, d.gen, &d.u, &pk);
        assert!((m1.len - 10.0).abs() < EPS, "{m1:?}");
        // unchanged: cache hit even if the data moved unseen
        ds.list[0].u.lines[a].b = (0.0, 2.0);
        let d = &ds.list[0];
        assert_eq!(mu.measure_cached(d.id, d.gen, &d.u, &pk), m1);
    }

    #[test]
    fn pick_measure_recomputes_on_pick_change() {
        let (_ed, mut ds) = empty();
        let a = line(&mut ds, (0.0, 0.0), (3.0, 4.0));
        let b = line(&mut ds, (0.0, 0.0), (0.0, 2.0));
        pick(&mut ds, &[a], &[]);
        let (_, pk1) = ds.pick.clone().unwrap();
        pick(&mut ds, &[a, b], &[]);
        let (_, pk2) = ds.pick.clone().unwrap();
        let mut mu = ModifyUi::default();
        let g = ds.list[0].gen;
        assert_eq!(mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk1).n, 1);
        let m = mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk2);
        assert_eq!(m.n, 2);
        assert!((m.len - 7.0).abs() < EPS);
    }

    #[test]
    fn closed_ring_picked_twice_counted_once() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        let (k, mut pk) = ds.pick.clone().unwrap();
        let r = pk.closed[0];
        pk.closed.push(r);
        ds.pick = Some((k, pk.clone()));
        let m = pick_measure(&ds.list[0].u, &pk);
        assert_eq!(m.n, 1);
        assert!((m.area - 16.0).abs() < EPS && (m.len - 16.0).abs() < EPS, "{m:?}");
    }

    #[test]
    fn measure_label_both_languages() {
        let m = Measure { n: 2, len: 6.0, area: 16.0 };
        let en = measure_label_in(&m, false);
        let ru = measure_label_in(&m, true);
        assert_ne!(en, ru);
        for s in [&en, &ru] {
            assert!(s.contains('2') && !s.contains('{'), "{s}");
        }
        assert!(!measure_label_in(&Measure { n: 1, len: 1.0, area: 0.0 }, false).contains("{area}"));
    }

    fn on_layer(ds: &mut Drawings, line_ix: usize, layer: &str) {
        ds.list[0].u.lines[line_ix].layer = layer.into();
    }

    #[test]
    fn measure_rows_by_layer() {
        let (_ed, mut ds) = empty();
        let a = line(&mut ds, (0.0, 0.0), (3.0, 4.0));
        let b = line(&mut ds, (0.0, 0.0), (0.0, 2.0));
        let c = arc(&mut ds, (10.0, 0.0), 2.0, 0.0, PI);
        on_layer(&mut ds, b, "Walls");
        let f = fill(&mut ds, vec![sq(0.0, 0.0, 2.0)]);
        ds.pick = Some((0, Pick { lines: vec![a, b], arcs: vec![c], fills: vec![f], ..Default::default() }));
        let (_, pk) = ds.pick.clone().unwrap();
        let rows = pick_rows(&ds.list[0].u, &pk);
        let names: Vec<&str> = rows.iter().map(|r| r.layer.as_str()).collect();
        assert_eq!(names, ["Walls", "Z"]);
        assert_eq!((rows[0].m.n, rows[0].m.len), (1, 2.0));
        assert_eq!(rows[1].m.n, 3);
        assert!((rows[1].m.len - (5.0 + 2.0 * PI)).abs() < EPS);
        assert!((rows[1].m.area - 4.0).abs() < EPS);
        // the cache serves the rows and its total equals pick_measure
        let mut mu = ModifyUi::default();
        let g = ds.list[0].gen;
        assert_eq!(mu.measure_rows_cached(ds.list[0].id, g, &ds.list[0].u, &pk).to_vec(), rows);
        assert_eq!(mu.measure_cached(ds.list[0].id, g, &ds.list[0].u, &pk), pick_measure(&ds.list[0].u, &pk));
    }

    fn row(layer: &str, n: usize, len: f64, area: f64) -> LayerRow {
        LayerRow { layer: layer.into(), m: Measure { n, len, area } }
    }

    #[test]
    fn takeoff_tsv_header_units_and_total() {
        use crate::units::Units;
        let rows = [row("Walls", 2, 1.5, 0.0), row("Slab", 1, 2.0, 12.345)];
        let en = takeoff_tsv(&rows, false, Units::Metric, 2);
        let l: Vec<&str> = en.lines().collect();
        assert_eq!(l[0], "Layer\tCount\tLength, mm\tArea, m\u{b2}");
        assert_eq!(l[1], "Walls\t2\t1500\t0.00");
        assert_eq!(l[2], "Slab\t1\t2000\t12.35");
        assert_eq!(l[3], "Total\t3\t3500\t12.35");
        let ru = takeoff_tsv(&rows, true, Units::Metric, 2);
        assert!(ru.lines().nth(2).unwrap().ends_with("\t12,35"), "{ru}");
        assert!(!ru.lines().next().unwrap().contains("Layer"));
        let imp = takeoff_tsv(&rows[1..], false, Units::Imperial, 2);
        let h = imp.lines().next().unwrap();
        assert!(h.contains("Length, ft") && h.contains("ft\u{b2}"), "{h}");
        // plain numbers: no unit suffix, no group separator
        for line in en.lines().skip(1) {
            for cell in line.split('\t').skip(1) {
                assert!(cell.parse::<f64>().is_ok(), "{cell}");
            }
        }
    }

    #[test]
    fn takeoff_tsv_strips_cr_lf_tab_from_layer_names() {
        let rows = [row("A\r\nB\tC", 1, 1.0, 0.0)];
        let tsv = takeoff_tsv(&rows, false, crate::units::Units::Metric, 2);
        assert!(!tsv.contains('\r'), "{tsv:?}");
        assert!(tsv.lines().nth(1).unwrap().starts_with("A  B C\t"), "{tsv:?}");
    }

    #[test]
    fn takeoff_tsv_no_double_count_ring_edges() {
        let (_ed, mut ds) = empty();
        box_ring(&mut ds, 4);
        let (_, pk) = ds.pick.clone().unwrap();
        let rows = pick_rows(&ds.list[0].u, &pk);
        let tsv = takeoff_tsv(&rows, false, crate::units::Units::Metric, 2);
        let last = tsv.lines().last().unwrap();
        assert_eq!(last, "Total\t1\t16000\t16.00", "{tsv}");
        assert_eq!(tsv.lines().count(), 3, "one layer row plus header and total");
    }
}

#[cfg(test)]
#[path = "modify_fuzz.rs"]
mod modify_fuzz;
