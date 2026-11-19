//! Arc walls: the axis is a circular arc (a, b, bulge).
//!
//! bulge = tan(angle/4), > 0 is counter-clockwise. Layers lie towards the left normal of the axis, so
//! a layer face at offset o is a circle of radius R − σ·o (σ = sign of bulge). Footprints are rings with
//! arc edges (`BulgeRing`), areas and volumes are computed analytically; chords are only for
//! display and for the room solver (with a segment correction).

use crate::cut::{cut_prism, CutBody, CutError, Cutter};
use crate::grid::{bbox_of, Bbox, BoxGrid};
use crate::layered::{layer_footprints, Cap, JoinError, LayerQty, Opening, Strip};
use crate::math::{v2, Tol, V2};
use crate::rooms::Region;

use std::f64::consts::PI;

/// Largest chord sagitta when tessellating arcs, m.
pub const CHORD_SAG: f64 = 1e-3;
/// Smallest |bulge|: below it the arc is a straight line (angle < 4e-9 rad).
pub const MIN_BULGE: f64 = 1e-9;
/// Largest |bulge|: sweep angle up to 4·atan(1e3) ≈ 359.8°.
pub const MAX_BULGE: f64 = 1e3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArcError {
    NonFinite,
    /// Endpoints coincide.
    ZeroChord,
    /// |bulge| < MIN_BULGE: the arc is a straight line.
    Straight,
    /// |bulge| > MAX_BULGE: nearly a full circle.
    TooLarge,
    /// The face of layer `layer` passes through the arc center or beyond it.
    CrossesCenter {
        layer: usize,
    },
    /// Three collinear points, or a radius smaller than half the chord.
    NoCircle,
}

/// Arc axis: from `a` to `b`, bulge = tan(angle/4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArcAxis {
    pub a: V2,
    pub b: V2,
    pub bulge: f64,
    pub center: V2,
    pub radius: f64,
    /// Angle of point `a` from the center.
    pub start: f64,
    /// Signed arc sweep angle (> 0 is counterclockwise).
    pub sweep: f64,
}

fn perp(v: V2) -> V2 {
    v2(-v.y, v.x)
}

fn unit(v: V2) -> V2 {
    v * (1.0 / v.len())
}

fn dir_at(a: f64) -> V2 {
    v2(a.cos(), a.sin())
}

/// Angle `x` normalized to (ref − π, ref + π].
fn near_angle(x: f64, r: f64) -> f64 {
    let mut d = (x - r) % (2.0 * PI);
    if d > PI {
        d -= 2.0 * PI;
    } else if d <= -PI {
        d += 2.0 * PI;
    }
    r + d
}

impl ArcAxis {
    pub fn new(a: V2, b: V2, bulge: f64, tol: Tol) -> Result<Self, ArcError> {
        if ![a.x, a.y, b.x, b.y, bulge].iter().all(|v| v.is_finite()) {
            return Err(ArcError::NonFinite);
        }
        let ch = b - a;
        let c = ch.len();
        if c <= tol.length {
            return Err(ArcError::ZeroChord);
        }
        if bulge.abs() < MIN_BULGE {
            return Err(ArcError::Straight);
        }
        if bulge.abs() > MAX_BULGE {
            return Err(ArcError::TooLarge);
        }
        let sweep = 4.0 * bulge.atan();
        let radius = c * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
        let d = c * (1.0 - bulge * bulge) / (4.0 * bulge);
        let center = (a + b) * 0.5 + perp(ch * (1.0 / c)) * d;
        let r = a - center;
        Ok(Self {
            a,
            b,
            bulge,
            center,
            radius,
            start: r.y.atan2(r.x),
            sweep,
        })
    }

    /// Arc through three points: start, a point on the arc, end.
    pub fn through(a: V2, m: V2, b: V2, tol: Tol) -> Result<Self, ArcError> {
        let (u, w) = (a - m, b - m);
        let (lu, lw) = (u.len(), w.len());
        if !(lu > tol.length && lw > tol.length) {
            return Err(ArcError::ZeroChord);
        }
        let cr = (b - a).cross(m - a);
        let ang = (u.dot(w) / (lu * lw)).clamp(-1.0, 1.0).acos();
        if cr.abs() <= tol.length * (b - a).len() || !ang.is_finite() {
            return Err(ArcError::NoCircle);
        }
        // m to the right of chord a→b: the arc is counterclockwise.
        let sign = if cr < 0.0 { 1.0 } else { -1.0 };
        Self::new(a, b, sign * ((PI - ang) / 2.0).tan(), tol)
    }

    /// Minor arc of the given radius; `ccw` is the direction from `a` to `b`.
    pub fn with_radius(a: V2, b: V2, radius: f64, ccw: bool, tol: Tol) -> Result<Self, ArcError> {
        let h = (b - a).len() / 2.0;
        if !radius.is_finite() {
            return Err(ArcError::NonFinite);
        }
        if radius < h || h <= tol.length {
            return Err(ArcError::NoCircle);
        }
        let half = (h / radius).asin();
        let b4 = (half / 2.0).tan();
        Self::new(a, b, if ccw { b4 } else { -b4 }, tol)
    }

    pub fn sigma(&self) -> f64 {
        self.sweep.signum()
    }
    pub fn length(&self) -> f64 {
        self.radius * self.sweep.abs()
    }
    /// Radius of the face offset `o` to the left of the axis.
    pub fn radius_at(&self, o: f64) -> f64 {
        self.radius - self.sigma() * o
    }
    /// Angle of the point at axis distance `s` from `a`.
    pub fn angle_at(&self, s: f64) -> f64 {
        self.start + self.sigma() * s / self.radius
    }
    pub fn point_at(&self, s: f64) -> V2 {
        self.center + dir_at(self.angle_at(s)) * self.radius
    }
    /// Unit tangent along the axis direction at distance `s`.
    pub fn tangent_at(&self, s: f64) -> V2 {
        perp(dir_at(self.angle_at(s))) * self.sigma()
    }
    /// Axis length from `a` to the projection of `p` onto the circle (angle taken near the arc midpoint).
    pub fn param_of(&self, p: V2) -> f64 {
        let r = p - self.center;
        let ang = near_angle(r.y.atan2(r.x), self.start + self.sweep / 2.0);
        (ang - self.start) * self.sigma() * self.radius
    }
    /// Point `p` strictly inside the arc (not at the endpoints) within `tol.length`.
    pub fn contains(&self, p: V2, tol: Tol) -> Option<f64> {
        let r = p - self.center;
        if (r.len() - self.radius).abs() > tol.length {
            return None;
        }
        let ang = near_angle(r.y.atan2(r.x), self.start + self.sweep / 2.0);
        let s = (ang - self.start) * self.sigma() * self.radius;
        (s > tol.length && s < self.length() - tol.length).then_some(s)
    }
    /// Arc bounding box expanded by `pad`.
    pub fn bbox(&self, pad: f64) -> Bbox {
        let mut pts = vec![self.a, self.b];
        let (lo, hi) = if self.sweep > 0.0 {
            (self.start, self.start + self.sweep)
        } else {
            (self.start + self.sweep, self.start)
        };
        let mut k = (lo / (PI / 2.0)).ceil();
        while k * PI / 2.0 < hi {
            pts.push(self.center + dir_at(k * PI / 2.0) * self.radius);
            k += 1.0;
        }
        bbox_of(&pts, pad)
    }
}

/// Wall axis length: along the arc when |bulge| ≥ MIN_BULGE, otherwise along the chord.
pub fn axis_length(a: V2, b: V2, bulge: f64) -> f64 {
    match ArcAxis::new(a, b, bulge, Tol::default()) {
        Ok(ax) => ax.length(),
        Err(_) => (b - a).len(),
    }
}

/// Ring with arc edges: edge i runs from `pts[i]` to `pts[i+1]` with `bulges[i]` (0 is a segment).
#[derive(Clone, Debug, PartialEq)]
pub struct BulgeRing {
    pub pts: Vec<V2>,
    pub bulges: Vec<f64>,
}

/// Arc from chord p→q and bulge: (center, radius, angle of p, signed sweep angle).
fn chord_arc(p: V2, q: V2, b: f64) -> (V2, f64, f64, f64) {
    let ch = q - p;
    let c = ch.len();
    let r = c * (1.0 + b * b) / (4.0 * b.abs());
    let d = c * (1.0 - b * b) / (4.0 * b);
    let center = (p + q) * 0.5 + perp(ch * (1.0 / c)) * d;
    let v = p - center;
    (center, r, v.y.atan2(v.x), 4.0 * b.atan())
}

/// Midpoint of arc edge p→q (for bulge = 0, the segment midpoint).
pub fn edge_mid(p: V2, q: V2, b: f64) -> V2 {
    if !edge_ok(p, q, b) {
        return (p + q) * 0.5;
    }
    let (c, r, a0, th) = chord_arc(p, q, b);
    c + dir_at(a0 + th / 2.0) * r
}

/// Angle step at which the chord sagitta is at most `sag` (and at most π/8).
fn max_step(r: f64, sag: f64) -> f64 {
    if r <= sag {
        return PI / 8.0;
    }
    (2.0 * (1.0 - sag / r).acos()).min(PI / 8.0)
}

fn edge_ok(p: V2, q: V2, b: f64) -> bool {
    b.abs() >= MIN_BULGE && (q - p).len() > 0.0 && b.is_finite()
}

impl BulgeRing {
    pub fn from_polygon(pts: Vec<V2>) -> Self {
        let n = pts.len();
        Self {
            pts,
            bulges: vec![0.0; n],
        }
    }

    /// Signed area (counterclockwise > 0), exact including arc segments.
    pub fn area(&self) -> f64 {
        let n = self.pts.len();
        let mut s = 0.0;
        for i in 0..n {
            let (p, q, b) = (self.pts[i], self.pts[(i + 1) % n], self.bulges[i]);
            s += p.cross(q) / 2.0;
            if edge_ok(p, q, b) {
                let (_, r, _, th) = chord_arc(p, q, b);
                s += b.signum() * r * r / 2.0 * (th.abs() - th.abs().sin());
            }
        }
        s
    }

    pub fn perimeter(&self) -> f64 {
        let n = self.pts.len();
        (0..n)
            .map(|i| {
                let (p, q, b) = (self.pts[i], self.pts[(i + 1) % n], self.bulges[i]);
                if edge_ok(p, q, b) {
                    let (_, r, _, th) = chord_arc(p, q, b);
                    r * th.abs()
                } else {
                    (q - p).len()
                }
            })
            .sum()
    }

    /// Polygon: arc vertices lie exactly on the circles, chord sagitta ≤ `sag`.
    pub fn tessellate(&self, sag: f64) -> Vec<V2> {
        let n = self.pts.len();
        let mut out = Vec::with_capacity(n * 4);
        for i in 0..n {
            let (p, q, b) = (self.pts[i], self.pts[(i + 1) % n], self.bulges[i]);
            out.push(p);
            if edge_ok(p, q, b) {
                arc_points(p, q, b, sag, &mut out);
            }
        }
        out
    }

    /// Ring in counterclockwise order.
    pub fn ccw(mut self) -> Self {
        if self.area() < 0.0 {
            let n = self.pts.len();
            self.pts.reverse();
            // edge i (pts[i]→pts[i+1]) becomes edge n-2-i after reversal, with the sign flipped.
            let old = self.bulges.clone();
            for i in 0..n {
                self.bulges[i] = -old[(2 * n - 2 - i) % n];
            }
        }
        self
    }
}

/// Interior points of arc p→q (endpoints excluded).
/// Chords of edge p→q (bulge `b`) for a sag within `sag`: 1 for a straight edge.
pub(crate) fn edge_parts(p: V2, q: V2, b: f64, sag: f64) -> usize {
    if !edge_ok(p, q, b) {
        return 1;
    }
    let (_, r, _, th) = chord_arc(p, q, b);
    (th.abs() / max_step(r, sag)).ceil().max(1.0) as usize
}

/// Point `j / k` of the way along edge p→q (bulge `b`), by angle on an arc.
pub(crate) fn edge_point(p: V2, q: V2, b: f64, j: usize, k: usize) -> V2 {
    let t = j as f64 / k as f64;
    if !edge_ok(p, q, b) {
        return p + (q - p) * t;
    }
    let (c, r, a0, th) = chord_arc(p, q, b);
    c + dir_at(a0 + th * t) * r
}

fn arc_points(p: V2, q: V2, b: f64, sag: f64, out: &mut Vec<V2>) {
    let (c, r, a0, th) = chord_arc(p, q, b);
    let k = (th.abs() / max_step(r, sag)).ceil().max(1.0) as usize;
    for j in 1..k {
        out.push(c + dir_at(a0 + th * j as f64 / k as f64) * r);
    }
}

/// Layer boundary or end: a line (point, direction) or a circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Line { p: V2, d: V2 },
    Circle { c: V2, r: f64 },
}

/// Intersection of two curves nearest to `near`.
pub(crate) fn meet(x: Curve, y: Curve, near: V2, tol: Tol) -> Option<V2> {
    let pick = |v: [Option<V2>; 2]| -> Option<V2> {
        v.into_iter()
            .flatten()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .min_by(|p, q| (*p - near).len().total_cmp(&(*q - near).len()))
    };
    match (x, y) {
        (Curve::Line { p: p1, d: d1 }, Curve::Line { p: p2, d: d2 }) => {
            let den = d1.cross(d2);
            if den.abs() <= tol.angle.max(1e-12) * d1.len() * d2.len() {
                return None;
            }
            pick([Some(p1 + d1 * ((p2 - p1).cross(d2) / den)), None])
        }
        (Curve::Line { p, d }, Curve::Circle { c, r })
        | (Curve::Circle { c, r }, Curve::Line { p, d }) => {
            let d = unit(d);
            let f = p - c;
            let bq = f.dot(d);
            let disc = bq * bq - (f.dot(f) - r * r);
            // tangency within tolerance: a single point
            let disc = if disc < 0.0 && disc > -2.0 * r * tol.length {
                0.0
            } else {
                disc
            };
            if disc < 0.0 {
                return None;
            }
            let sq = disc.sqrt();
            pick([Some(p + d * (-bq - sq)), Some(p + d * (-bq + sq))])
        }
        (Curve::Circle { c: c1, r: r1 }, Curve::Circle { c: c2, r: r2 }) => {
            let v = c2 - c1;
            let dd = v.len();
            if dd <= tol.length {
                return None;
            }
            let a = (r1 * r1 - r2 * r2 + dd * dd) / (2.0 * dd);
            let h2 = r1 * r1 - a * a;
            let h2 = if h2 < 0.0 && h2 > -2.0 * r1 * tol.length {
                0.0
            } else {
                h2
            };
            if h2 < 0.0 {
                return None;
            }
            let (e, h) = (v * (1.0 / dd), h2.sqrt());
            let m = c1 + e * a;
            pick([Some(m + perp(e) * h), Some(m - perp(e) * h)])
        }
    }
}

/// Wall for `arc_walls`: straight when `bulge == 0`.
#[derive(Clone, Copy)]
enum Axis {
    Line,
    Arc(ArcAxis),
}

/// End of a wall at a node with an arc: one shared by all layers.
#[derive(Clone, Copy, Debug, PartialEq)]
enum EndCap {
    Plain,
    Cut(Curve),
}

/// Footprints of all walls (straight and arc): `[wall][layer][piece]`, plus fillers of nodes with 3+ straight walls.
#[derive(Debug)]
pub struct Walls {
    pub rings: Vec<Vec<Vec<BulgeRing>>>,
    pub fillers: Vec<Vec<V2>>,
    pub filler_owners: Vec<(usize, usize)>,
}

fn face(s: &Strip, ax: &Axis, o: f64) -> Curve {
    match ax {
        Axis::Line => Curve::Line {
            p: s.a + s.normal() * o,
            d: s.dir(),
        },
        Axis::Arc(c) => Curve::Circle {
            c: c.center,
            r: c.radius_at(o),
        },
    }
}

/// Wall end: point, tangent from the node into the wall, face offsets (min, max) in the frame
/// of that tangent (left normal).
struct EndInfo {
    u: V2,
    /// Offsets in wall coordinates corresponding to min/max in the end frame.
    o_min: f64,
    o_max: f64,
}

fn end_info(s: &Strip, ax: &Axis, at_b: bool) -> EndInfo {
    let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
    let (lo, hi) = (s.offset0, s.offset0 + total);
    let t = match ax {
        Axis::Line => s.dir(),
        Axis::Arc(c) => c.tangent_at(if at_b { c.length() } else { 0.0 }),
    };
    if at_b {
        // the tangent from the node points against the axis: the left side of the end is the right side of the axis
        EndInfo {
            u: t * -1.0,
            o_min: hi,
            o_max: lo,
        }
    } else {
        EndInfo {
            u: t,
            o_min: lo,
            o_max: hi,
        }
    }
}

fn remap(e: JoinError, map: &[usize]) -> JoinError {
    use JoinError::*;
    let m = |i: usize| map.get(i).copied().unwrap_or(i);
    match e {
        NonFinite { strip } => NonFinite { strip: m(strip) },
        ZeroLength { strip } => ZeroLength { strip: m(strip) },
        NoLayers { strip } => NoLayers { strip: m(strip) },
        Overlapping { a, b } => Overlapping { a: m(a), b: m(b) },
        Degenerate {
            strip,
            layer,
            cause,
        } => Degenerate {
            strip: m(strip),
            layer,
            cause,
        },
        OpeningOutside { strip, opening } => OpeningOutside {
            strip: m(strip),
            opening,
        },
        NearlyParallel { a, b } => NearlyParallel { a: m(a), b: m(b) },
        CutOverlap { strip, layer, a, b } => CutOverlap {
            strip: m(strip),
            layer,
            a: m(a),
            b: m(b),
        },
        Arc { strip, cause } => Arc {
            strip: m(strip),
            cause,
        },
        e => e,
    }
}

/// Solves the joins of a network of straight and arc walls. `bulges[i] == 0` is a straight wall (as in `join`).
/// Nodes consisting only of straight walls are solved by `join`; nodes with an arc: an L of two ends gets a
/// straight-line join through exact face intersections, a T trims the abutting wall at the near host face. Deterministic.
pub fn arc_walls(strips: &[Strip], bulges: &[f64], tol: Tol) -> Result<Walls, JoinError> {
    arc_walls_with(strips, bulges, &[], tol)
}

/// `arc_walls` with join overrides for straight walls (`layered::join_with`).
pub fn arc_walls_with(
    strips: &[Strip],
    bulges: &[f64],
    prefs: &[crate::layered::JoinPrefs],
    tol: Tol,
) -> Result<Walls, JoinError> {
    let mut axes = Vec::with_capacity(strips.len());
    for (i, s) in strips.iter().enumerate() {
        let b = bulges.get(i).copied().unwrap_or(0.0);
        if b == 0.0 {
            axes.push(Axis::Line);
            continue;
        }
        let finite = [s.offset0, s.base, s.height].iter().all(|v| v.is_finite())
            && s.layers.iter().all(|l| l.thickness.is_finite());
        if !finite {
            return Err(JoinError::NonFinite { strip: i });
        }
        if s.layers.is_empty() {
            return Err(JoinError::NoLayers { strip: i });
        }
        let ax =
            ArcAxis::new(s.a, s.b, b, tol).map_err(|cause| JoinError::Arc { strip: i, cause })?;
        for (layer, o) in s.bounds().iter().enumerate() {
            if ax.radius_at(*o) <= tol.length {
                return Err(JoinError::Arc {
                    strip: i,
                    cause: ArcError::CrossesCenter {
                        layer: layer.min(s.layers.len() - 1),
                    },
                });
            }
        }
        axes.push(Axis::Arc(ax));
    }
    let line_idx: Vec<usize> = (0..strips.len())
        .filter(|&i| matches!(axes[i], Axis::Line))
        .collect();
    let lines: Vec<Strip> = line_idx.iter().map(|&i| strips[i].clone()).collect();
    let lp: Vec<crate::layered::JoinPrefs> = line_idx
        .iter()
        .map(|&i| prefs.get(i).copied().unwrap_or([None, None]))
        .collect();
    let joined = crate::layered::join_with(&lines, &lp, tol).map_err(|e| remap(e, &line_idx))?;
    let mut line_pieces: Vec<Option<Vec<crate::layered::LayerPieces>>> = vec![None; strips.len()];
    for (k, p) in line_idx.iter().zip(joined.pieces) {
        line_pieces[*k] = Some(p);
    }

    let caps = arc_caps(strips, &axes, tol);

    let mut rings = Vec::with_capacity(strips.len());
    for (i, s) in strips.iter().enumerate() {
        let cap = caps[i];
        match axes[i] {
            Axis::Arc(ax) => rings.push(arc_rings(s, &ax, cap, i, tol)?),
            Axis::Line => {
                let mut pieces = line_pieces[i].take().unwrap_or_default();
                let circle = cap
                    .iter()
                    .any(|c| matches!(c, EndCap::Cut(Curve::Circle { .. })));
                let single = pieces.iter().all(|p| p.len() == 1);
                if circle && single {
                    rings.push(line_rings(s, cap, i, tol)?);
                    continue;
                }
                for (end, c) in cap.iter().enumerate() {
                    let EndCap::Cut(cv) = c else { continue };
                    let lc = line_cap(s, *cv, end == 1, tol);
                    for layer in pieces.iter_mut() {
                        let k = if end == 0 { 0 } else { layer.len() - 1 };
                        layer[k][end] = lc;
                    }
                }
                let fp = layer_footprints(s, &pieces, i, tol)?;
                rings.push(
                    fp.into_iter()
                        .map(|ps| {
                            ps.into_iter()
                                .map(|p| BulgeRing::from_polygon(p.outer))
                                .collect()
                        })
                        .collect(),
                );
            }
        }
    }
    let filler_owners = joined
        .filler_owners
        .iter()
        .map(|&(w, l)| (line_idx[w], l))
        .collect();
    Ok(Walls {
        rings,
        fillers: joined.fillers,
        filler_owners,
    })
}

/// End of a straight wall against a curve: a line through the meeting points of the extreme faces.
fn line_cap(s: &Strip, cv: Curve, at_b: bool, tol: Tol) -> Option<Cap> {
    match cv {
        Curve::Line { p, d } => Some(Cap { p, d }),
        Curve::Circle { .. } => {
            let end = if at_b { s.b } else { s.a };
            let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
            let f = |o: f64| meet(face(s, &Axis::Line, o), cv, end + s.normal() * o, tol);
            let (p, q) = (f(s.offset0)?, f(s.offset0 + total)?);
            ((q - p).len() > tol.length).then_some(Cap { p, d: q - p })
        }
    }
}

/// Wall end trims at nodes that include an arc.
fn arc_caps(strips: &[Strip], axes: &[Axis], tol: Tol) -> Vec<[EndCap; 2]> {
    let n = strips.len();
    let mut caps = vec![[EndCap::Plain; 2]; n];
    if !axes.iter().any(|a| matches!(a, Axis::Arc(_))) {
        return caps;
    }
    let is_arc = |i: usize| matches!(axes[i], Axis::Arc(_));
    let point = |i: usize, at_b: bool| if at_b { strips[i].b } else { strips[i].a };
    let ends: Vec<(usize, bool)> = (0..n).flat_map(|i| [(i, false), (i, true)]).collect();
    let end_boxes: Vec<Bbox> = ends
        .iter()
        .map(|&(i, b)| bbox_of(&[point(i, b)], tol.length))
        .collect();
    let end_grid = BoxGrid::new(&end_boxes);
    let wall_boxes: Vec<Bbox> = (0..n)
        .map(|i| {
            let s = &strips[i];
            match axes[i] {
                Axis::Line => bbox_of(&[s.a, s.b], tol.length),
                Axis::Arc(ax) => ax.bbox(tol.length),
            }
        })
        .collect();
    let wall_grid = BoxGrid::new(&wall_boxes);
    let mut used = vec![false; ends.len()];
    let mut cand = vec![];
    for k in 0..ends.len() {
        if used[k] {
            continue;
        }
        let node = point(ends[k].0, ends[k].1);
        end_grid.query(&end_boxes[k], &mut cand);
        let group: Vec<usize> = cand
            .iter()
            .copied()
            .filter(|&m| {
                m >= k && !used[m] && (point(ends[m].0, ends[m].1) - node).len() <= tol.length
            })
            .collect();
        group.iter().for_each(|&m| used[m] = true);
        let any_arc = group.iter().any(|&m| is_arc(ends[m].0));
        match group.len() {
            2 if any_arc => {
                let (e1, e2) = (ends[group[0]], ends[group[1]]);
                if e1.0 == e2.0 {
                    continue;
                }
                if let Some(c) = corner(strips, axes, e1, e2, node, tol) {
                    caps[e1.0][e1.1 as usize] = EndCap::Cut(c);
                    caps[e2.0][e2.1 as usize] = EndCap::Cut(c);
                }
            }
            1 => {
                let (i, at_b) = ends[group[0]];
                wall_grid.query(&end_boxes[k], &mut cand);
                for &j in &cand {
                    if j == i || !(is_arc(i) || is_arc(j)) {
                        continue;
                    }
                    if let Some(c) = tee(strips, axes, (i, at_b), j, node, tol) {
                        caps[i][at_b as usize] = EndCap::Cut(c);
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    caps
}

/// L node of two ends: the end line passes through the intersections of face pairs bounding one sector.
fn corner(
    strips: &[Strip],
    axes: &[Axis],
    e1: (usize, bool),
    e2: (usize, bool),
    node: V2,
    tol: Tol,
) -> Option<Curve> {
    let (s1, s2) = (&strips[e1.0], &strips[e2.0]);
    let (i1, i2) = (
        end_info(s1, &axes[e1.0], e1.1),
        end_info(s2, &axes[e2.0], e2.1),
    );
    let cr = i1.u.cross(i2.u);
    if cr.abs() <= 1e-9 {
        // tangent continuation: shared square end along the normal; opposing ends get no join
        return (i1.u.dot(i2.u) < 0.0).then_some(Curve::Line {
            p: node,
            d: perp(i1.u),
        });
    }
    let pa = meet(
        face(s1, &axes[e1.0], i1.o_max),
        face(s2, &axes[e2.0], i2.o_min),
        node,
        tol,
    )?;
    let pb = meet(
        face(s1, &axes[e1.0], i1.o_min),
        face(s2, &axes[e2.0], i2.o_max),
        node,
        tol,
    )?;
    let d = pb - pa;
    (d.len() > tol.length).then_some(Curve::Line { p: pa, d })
}

/// T node: end `stem` on the axis of host `host`; the end follows the host face nearest the abutting wall.
fn tee(
    strips: &[Strip],
    axes: &[Axis],
    stem: (usize, bool),
    host: usize,
    node: V2,
    tol: Tol,
) -> Option<Curve> {
    let h = &strips[host];
    let n_h = match axes[host] {
        Axis::Line => {
            let d = h.dir();
            let t = (node - h.a).dot(d);
            let off = (node - h.a).cross(d).abs();
            if off > tol.length || t <= tol.length || t >= h.length() - tol.length {
                return None;
            }
            h.normal()
        }
        Axis::Arc(ax) => {
            let s = ax.contains(node, tol)?;
            perp(ax.tangent_at(s))
        }
    };
    let u = end_info(&strips[stem.0], &axes[stem.0], stem.1).u;
    let total: f64 = h.layers.iter().map(|l| l.thickness).sum();
    let o = if u.dot(n_h) > 0.0 {
        h.offset0 + total
    } else {
        h.offset0
    };
    Some(face(h, &axes[host], o))
}

/// Point of the face at offset `o` on the end: radial, or the meeting with the end line.
fn end_point(f: Curve, default: V2, cap: EndCap, tol: Tol) -> V2 {
    match cap {
        EndCap::Plain => default,
        EndCap::Cut(c) => meet(f, c, default, tol).unwrap_or(default),
    }
}

/// bulge of the end edge between two points (minor arc for a circle).
fn cap_bulge(cap: EndCap, p: V2, q: V2) -> f64 {
    match cap {
        EndCap::Cut(Curve::Circle { c, .. }) => {
            let (u, w) = (p - c, q - c);
            let d = near_angle(w.y.atan2(w.x), u.y.atan2(u.x)) - u.y.atan2(u.x);
            (d / 4.0).tan()
        }
        _ => 0.0,
    }
}

fn arc_rings(
    s: &Strip,
    ax: &ArcAxis,
    cap: [EndCap; 2],
    strip: usize,
    tol: Tol,
) -> Result<Vec<Vec<BulgeRing>>, JoinError> {
    let bounds = s.bounds();
    let a1 = ax.start + ax.sweep;
    let pt = |o: f64, end: usize| -> V2 {
        let r = ax.radius_at(o);
        let ang = if end == 0 { ax.start } else { a1 };
        let f = Curve::Circle { c: ax.center, r };
        end_point(f, ax.center + dir_at(ang) * r, cap[end], tol)
    };
    let arc_b = |p: V2, q: V2| -> Option<f64> {
        let (u, w) = (p - ax.center, q - ax.center);
        let (ap, aq) = (u.y.atan2(u.x), w.y.atan2(w.x));
        let ap = near_angle(ap, ax.start);
        let aq = near_angle(aq, ap + ax.sweep);
        let d = aq - ap;
        (d * ax.sweep > 0.0 && d.abs() < 2.0 * PI).then(|| (d / 4.0).tan())
    };
    let mut out = Vec::with_capacity(s.layers.len());
    for (layer, w) in bounds.windows(2).enumerate() {
        let (p0, p1, q1, q0) = (pt(w[0], 0), pt(w[0], 1), pt(w[1], 1), pt(w[1], 0));
        let (Some(bl), Some(bh)) = (arc_b(p0, p1), arc_b(q0, q1)) else {
            return Err(JoinError::Arc {
                strip,
                cause: ArcError::CrossesCenter { layer },
            });
        };
        out.push(vec![BulgeRing {
            pts: vec![p0, p1, q1, q0],
            bulges: vec![
                bl,
                cap_bulge(cap[1], p1, q1),
                -bh,
                cap_bulge(cap[0], q0, p0),
            ],
        }]);
    }
    Ok(out)
}

/// Straight wall with a circular end (abuts an arc): one piece per layer.
fn line_rings(
    s: &Strip,
    cap: [EndCap; 2],
    strip: usize,
    tol: Tol,
) -> Result<Vec<Vec<BulgeRing>>, JoinError> {
    let n = s.normal();
    let pt = |o: f64, end: usize| {
        let base = if end == 0 { s.a } else { s.b } + n * o;
        end_point(face(s, &Axis::Line, o), base, cap[end], tol)
    };
    s.bounds()
        .windows(2)
        .enumerate()
        .map(|(layer, w)| {
            let (p0, p1, q1, q0) = (pt(w[0], 0), pt(w[0], 1), pt(w[1], 1), pt(w[1], 0));
            let r = BulgeRing {
                pts: vec![p0, p1, q1, q0],
                bulges: vec![
                    0.0,
                    cap_bulge(cap[1], p1, q1),
                    0.0,
                    cap_bulge(cap[0], q0, p0),
                ],
            };
            if r.area().abs() <= tol.length * tol.length {
                return Err(JoinError::Degenerate {
                    strip,
                    layer,
                    cause: crate::profile::ProfileError::ZeroArea { ring: 0 },
                });
            }
            Ok(vec![r])
        })
        .collect()
}

/// Layer quantities: plan area is the exact ring area; volume = area × h − opening deductions.
/// For an arc, an opening [along, along+width] on the axis removes (width/R)·t·r_mid·h from the layer.
pub fn wall_quantities(
    s: &Strip,
    bulge: f64,
    rings: &[Vec<BulgeRing>],
    openings: &[Opening],
    strip: usize,
) -> Result<Vec<LayerQty>, JoinError> {
    let ax = if bulge == 0.0 {
        None
    } else {
        Some(
            ArcAxis::new(s.a, s.b, bulge, Tol::default())
                .map_err(|cause| JoinError::Arc { strip, cause })?,
        )
    };
    let len = ax.map_or(s.length(), |a| a.length());
    for (k, o) in openings.iter().enumerate() {
        let fits = o.along >= 0.0
            && o.along + o.width <= len
            && o.sill >= 0.0
            && o.sill + o.height <= s.height;
        if !fits || o.width <= 0.0 || o.height <= 0.0 {
            return Err(JoinError::OpeningOutside { strip, opening: k });
        }
    }
    let cut: f64 = openings.iter().map(|o| o.width * o.height).sum();
    let bounds = s.bounds();
    Ok(rings
        .iter()
        .zip(&s.layers)
        .enumerate()
        .map(|(k, (f, l))| {
            let area: f64 = f.iter().map(|r| r.area().abs()).sum();
            let ratio = ax.map_or(1.0, |a| {
                a.radius_at(0.5 * (bounds[k] + bounds[k + 1])) / a.radius
            });
            LayerQty {
                plan_area: area,
                volume: area * s.height - cut * l.thickness * ratio,
            }
        })
        .collect())
}

/// Layer solids of an arc wall for display: the ring is split into convex quadrilaterals along chords,
/// openings are cut out; the `volume` of each solid is its analytic share of the layer volume.
pub fn arc_bodies(
    s: &Strip,
    bulge: f64,
    rings: &[Vec<BulgeRing>],
    openings: &[Opening],
    tol: Tol,
) -> Result<Vec<Vec<Result<CutBody, CutError>>>, JoinError> {
    let ax =
        ArcAxis::new(s.a, s.b, bulge, tol).map_err(|cause| JoinError::Arc { strip: 0, cause })?;
    let q = wall_quantities(s, bulge, rings, openings, 0)?;
    let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
    let (r_a, r_b) = (ax.radius_at(s.offset0), ax.radius_at(s.offset0 + total));
    let (r_in, r_out) = (r_a.min(r_b) - 1e-3, r_a.max(r_b) + 1e-3);
    let cutters: Vec<Cutter> = openings
        .iter()
        .map(|o| {
            let (t0, t1) = (ax.angle_at(o.along), ax.angle_at(o.along + o.width));
            let k = ((t1 - t0).abs() / max_step(r_out, CHORD_SAG))
                .ceil()
                .max(1.0) as usize;
            let mut ring = vec![];
            for j in 0..=k {
                ring.push(ax.center + dir_at(t0 + (t1 - t0) * j as f64 / k as f64) * r_out);
            }
            for j in (0..=k).rev() {
                ring.push(ax.center + dir_at(t0 + (t1 - t0) * j as f64 / k as f64) * r_in.max(0.0));
            }
            Cutter {
                ring,
                holes: vec![],
                z0: s.base + o.sill,
                z1: s.base + o.sill + o.height,
            }
        })
        .collect();
    let refs: Vec<&Cutter> = cutters.iter().collect();
    Ok(rings
        .iter()
        .zip(&q)
        .map(|(layer, lq)| {
            let mut bodies = vec![];
            for r in layer {
                let (lo, hi) = (
                    side(r.pts[0], r.pts[1], r.bulges[0]),
                    side(r.pts[3], r.pts[2], -r.bulges[2]),
                );
                let k = lo.len().max(hi.len()) - 1;
                let lo = resample(&lo, k);
                let hi = resample(&hi, k);
                for j in 0..k {
                    let quad = [lo[j], lo[j + 1], hi[j + 1], hi[j]];
                    bodies.push(cut_prism(&quad, s.base, s.base + s.height, &refs, tol));
                }
            }
            let mesh_vol: f64 = bodies.iter().flatten().map(|b| b.volume).sum();
            if mesh_vol > 0.0 {
                let k = lq.volume / mesh_vol;
                for b in bodies.iter_mut().flatten() {
                    b.volume *= k;
                }
            }
            bodies
        })
        .collect())
}

/// Points of arc p→q including endpoints.
fn side(p: V2, q: V2, b: f64) -> Vec<V2> {
    let mut v = vec![p];
    if edge_ok(p, q, b) {
        arc_points(p, q, b, CHORD_SAG, &mut v);
    }
    v.push(q);
    v
}

/// Polyline of k segments by length parameter (points lie on the source polyline).
fn resample(pts: &[V2], k: usize) -> Vec<V2> {
    if pts.len() == k + 1 {
        return pts.to_vec();
    }
    let n = pts.len() - 1;
    (0..=k)
        .map(|j| {
            let t = j as f64 * n as f64 / k as f64;
            let i = (t.floor() as usize).min(n - 1);
            let f = t - i as f64;
            pts[i] + (pts[i + 1] - pts[i]) * f
        })
        .collect()
}

/// Face of an arc wall for the room solver: the circle and the longest tessellation chord.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArcFace {
    pub center: V2,
    pub radius: f64,
}

/// Extreme faces of an arc wall (for room area correction).
pub fn arc_faces(s: &Strip, bulge: f64) -> Vec<ArcFace> {
    let Ok(ax) = ArcAxis::new(s.a, s.b, bulge, Tol::default()) else {
        return vec![];
    };
    let total: f64 = s.layers.iter().map(|l| l.thickness).sum();
    [s.offset0, s.offset0 + total]
        .iter()
        .map(|&o| ArcFace {
            center: ax.center,
            radius: ax.radius_at(o),
        })
        .filter(|f| f.radius > 0.0)
        .collect()
}

/// Area and perimeter correction for a region due to tessellation chords lying on circular faces:
/// (Δarea, Δperimeter). A chord qualifies if both ends are on the circle and it is no longer than the step.
pub fn arc_correction(region: &Region, faces: &[ArcFace]) -> (f64, f64) {
    if faces.is_empty() {
        return (0.0, 0.0);
    }
    let boxes: Vec<Bbox> = faces
        .iter()
        .map(|f| {
            let r = f.radius;
            [
                f.center.x - r,
                f.center.y - r,
                f.center.x + r,
                f.center.y + r,
            ]
        })
        .collect();
    let grid = BoxGrid::new(&boxes);
    let mut cand = vec![];
    let (mut da, mut dp) = (0.0, 0.0);
    for ring in std::iter::once(&region.outer).chain(&region.holes) {
        let n = ring.len();
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let c = (q - p).len();
            if c <= 0.0 {
                continue;
            }
            grid.query(&bbox_of(&[p, q], 1e-6), &mut cand);
            for &k in &cand {
                let f = faces[k];
                let eps = 1e-6 * f.radius.max(1.0);
                let on = |x: V2| ((x - f.center).len() - f.radius).abs() <= eps;
                let step = 2.0 * f.radius * (max_step(f.radius, CHORD_SAG) / 2.0).sin();
                if !(on(p) && on(q) && c <= step * (1.0 + 1e-6) + eps) {
                    continue;
                }
                let th = 2.0 * (c / (2.0 * f.radius)).min(1.0).asin();
                let seg = f.radius * f.radius / 2.0 * (th - th.sin());
                let left = (q - p).cross(f.center - p) > 0.0;
                da += if left { seg } else { -seg };
                dp += f.radius * th - c;
                break;
            }
        }
    }
    (da, dp)
}

/// Points of the circle (centre `c`, radius `r`) where the tangent from `p` touches it;
/// `None` when `p` is on or inside the circle.
pub fn tangent_points(c: V2, r: f64, p: V2) -> Option<[V2; 2]> {
    let d = p - c;
    let l = d.len();
    if r <= 0.0 || l <= r * (1.0 + 1e-12) {
        return None;
    }
    let (base, off) = (d.y.atan2(d.x), (r / l).acos());
    Some([c + dir_at(base + off) * r, c + dir_at(base - off) * r])
}

/// Points of the arc at 0, 90, 180 and 270 degrees (those inside its sweep).
pub fn quadrants(ax: &ArcAxis) -> Vec<V2> {
    let (n, sg) = (ax.sweep.abs(), ax.sweep.signum());
    (0..4)
        .filter_map(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_2;
            let along = ((a - ax.start) * sg).rem_euclid(std::f64::consts::TAU);
            (along <= n + 1e-12).then(|| ax.center + dir_at(a) * ax.radius)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layered::Layer;

    fn strip(a: V2, b: V2, t: f64) -> Strip {
        Strip {
            a,
            b,
            layers: vec![Layer::new(t)],
            offset0: -t / 2.0,
            base: 0.0,
            height: 3.0,
        }
    }

    fn near(a: f64, b: f64, eps: f64) {
        assert!((a - b).abs() < eps, "{a} vs {b}");
    }

    #[test]
    fn semicircle_wall_exact() {
        let s = strip(v2(3.0, 0.0), v2(-3.0, 0.0), 0.2);
        let w = arc_walls(&[s.clone()], &[1.0], Tol::default()).unwrap();
        let ax = ArcAxis::new(s.a, s.b, 1.0, Tol::default()).unwrap();
        near(ax.length(), 3.0 * PI, 1e-12);
        near(ax.center.len(), 0.0, 1e-12);
        let q = wall_quantities(&s, 1.0, &w.rings[0], &[], 0).unwrap();
        near(q[0].plan_area, 0.6 * PI, 1e-12);
        near(q[0].volume, 1.8 * PI, 1e-12);
        let o = Opening {
            along: 4.0,
            width: 1.0,
            sill: 0.0,
            height: 2.0,
        };
        let q = wall_quantities(&s, 1.0, &w.rings[0], &[o], 0).unwrap();
        near(q[0].volume, 1.8 * PI - 0.4, 1e-12);
        let bodies = arc_bodies(&s, 1.0, &w.rings[0], &[o], Tol::default()).unwrap();
        let v: f64 = bodies[0].iter().map(|b| b.as_ref().unwrap().volume).sum();
        near(v, 1.8 * PI - 0.4, 1e-9);
        let o2 = Opening { along: 9.0, ..o };
        assert_eq!(
            wall_quantities(&s, 1.0, &w.rings[0], &[o2], 0),
            Err(JoinError::OpeningOutside {
                strip: 0,
                opening: 0
            })
        );
    }

    #[test]
    fn off_center_layer_opening() {
        // layer [0, 0.3] to the left (toward the center when bulge>0): r_mid = 3 - 0.15
        let mut s = strip(v2(3.0, 0.0), v2(-3.0, 0.0), 0.3);
        s.offset0 = 0.0;
        let w = arc_walls(&[s.clone()], &[1.0], Tol::default()).unwrap();
        let o = Opening {
            along: 1.0,
            width: 1.5,
            sill: 0.5,
            height: 2.0,
        };
        let q = wall_quantities(&s, 1.0, &w.rings[0], &[o], 0).unwrap();
        let area = PI / 2.0 * (9.0 - 2.7 * 2.7);
        near(q[0].plan_area, area, 1e-12);
        near(
            q[0].volume,
            area * 3.0 - 1.5 / 3.0 * 0.3 * 2.85 * 2.0,
            1e-12,
        );
    }

    #[test]
    fn tangent_line_continuation() {
        let b = (PI / 8.0).tan();
        let arc = strip(v2(3.0, 0.0), v2(0.0, 3.0), 0.2);
        let line = strip(v2(0.0, 3.0), v2(-2.0, 3.0), 0.2);
        let w = arc_walls(&[arc, line], &[b, 0.0], Tol::default()).unwrap();
        let sum: f64 = w
            .rings
            .iter()
            .flatten()
            .flatten()
            .map(|r| r.area().abs())
            .sum();
        near(sum, 0.2 * (1.5 * PI + 2.0), 1e-12);
    }

    #[test]
    fn room_in_semicircle_and_chord() {
        let tol = Tol::default();
        let arc = strip(v2(3.0, 0.0), v2(-3.0, 0.0), 0.2);
        let chord = strip(v2(-3.0, 0.0), v2(3.0, 0.0), 0.2);
        let ss = [arc.clone(), chord];
        let w = arc_walls(&ss, &[1.0, 0.0], tol).unwrap();
        let polys: Vec<Vec<V2>> = w
            .rings
            .iter()
            .flatten()
            .flatten()
            .map(|r| r.tessellate(CHORD_SAG))
            .collect();
        let a = crate::rooms::arrange(&polys, tol);
        let g = a.rooms.iter().find(|g| g.contains(v2(0.0, 1.0))).unwrap();
        let (da, dp) = arc_correction(g, &arc_faces(&arc, 1.0));
        let r: f64 = 2.9;
        let exact = r * r * (0.1 / r).acos() - 0.1 * (r * r - 0.01).sqrt();
        near(g.area + da, exact, 1e-9);
        let x = (r * r - 0.01).sqrt();
        near(
            g.perimeter + dp,
            r * (PI - 2.0 * (0.1 / r).asin()) + 2.0 * x,
            1e-9,
        );
        // union of walls = half-strip ring + chord rectangle with no overlap
        let sum: f64 = w
            .rings
            .iter()
            .flatten()
            .flatten()
            .map(|r| r.area().abs())
            .sum();
        let tess: f64 = polys
            .iter()
            .map(|p| crate::profile::signed_area(p).abs())
            .sum();
        assert!(sum > tess);
    }

    #[test]
    fn corner_arc_line_matches_fine_polygon() {
        // arc (3,0)→(0,3) and a straight wall (0,3)→(0,6) at an angle: join with no gap or overlap
        let b = (PI / 8.0).tan();
        let arc = strip(v2(3.0, 0.0), v2(0.0, 3.0), 0.2);
        let line = strip(v2(0.0, 3.0), v2(0.0, 6.0), 0.2);
        let w = arc_walls(&[arc, line], &[b, 0.0], Tol::default()).unwrap();
        let ra = &w.rings[0][0][0];
        let rl = &w.rings[1][0][0];
        // shared end: inner and outer points coincide
        let share = |p: V2| rl.pts.iter().any(|q| (*q - p).len() < 1e-12);
        assert!(share(ra.pts[1]) && share(ra.pts[2]), "{ra:?} {rl:?}");
        // inner arc face r=3.1 (bulge>0: left side toward the center...) meets the line x=±0.1
        for p in [ra.pts[1], ra.pts[2]] {
            let r = p.len();
            assert!((r - 2.9).abs() < 1e-12 || (r - 3.1).abs() < 1e-12);
            assert!((p.x.abs() - 0.1).abs() < 1e-12);
        }
    }

    #[test]
    fn arc_arc_corner_and_tee() {
        let tol = Tol::default();
        let b = (PI / 8.0).tan();
        // two quarter arcs meeting at an angle
        let a1 = strip(v2(3.0, 0.0), v2(0.0, 3.0), 0.2);
        let a2 = strip(v2(0.0, 3.0), v2(3.0, 6.0), 0.2);
        let w = arc_walls(&[a1, a2], &[b, -b], tol).unwrap();
        let (r1, r2) = (&w.rings[0][0][0], &w.rings[1][0][0]);
        let share = |p: V2| r2.pts.iter().any(|q| (*q - p).len() < 1e-12);
        assert!(share(r1.pts[1]) && share(r1.pts[2]));
        // T: a straight wall butts against the semicircle from outside
        let arc = strip(v2(3.0, 0.0), v2(-3.0, 0.0), 0.2);
        let stem = strip(v2(0.0, 6.0), v2(0.0, 3.0), 0.2);
        let w = arc_walls(&[arc, stem], &[1.0, 0.0], tol).unwrap();
        let r = &w.rings[1][0][0];
        for p in [r.pts[1], r.pts[2]] {
            near(p.len(), 3.1, 1e-12);
        }
        let (x, r_out) = (0.1f64, 3.1f64);
        let under = x * (r_out * r_out - x * x).sqrt() + r_out * r_out * (x / r_out).asin();
        near(r.area().abs(), 0.2 * 6.0 - under, 1e-9);
    }

    #[test]
    fn degenerate_arcs_are_typed_errors() {
        let tol = Tol::default();
        let s = strip(v2(0.0, 0.0), v2(0.0, 0.0), 0.2);
        assert!(matches!(
            arc_walls(&[s], &[1.0], tol),
            Err(JoinError::Arc {
                strip: 0,
                cause: ArcError::ZeroChord
            })
        ));
        let s = strip(v2(0.0, 0.0), v2(1.0, 0.0), 0.2);
        assert!(matches!(
            arc_walls(&[s.clone()], &[f64::NAN], tol),
            Err(JoinError::Arc { .. })
        ));
        assert!(matches!(
            arc_walls(&[s.clone()], &[1e-12], tol),
            Err(JoinError::Arc {
                cause: ArcError::Straight,
                ..
            })
        ));
        assert!(matches!(
            arc_walls(&[s.clone()], &[1e9], tol),
            Err(JoinError::Arc {
                cause: ArcError::TooLarge,
                ..
            })
        ));
        // R = 0.5, thickness 2 centered: the face lies beyond the center
        let mut t = strip(v2(0.0, 0.0), v2(1.0, 0.0), 2.0);
        t.offset0 = -1.0;
        assert!(matches!(
            arc_walls(&[t], &[1.0], tol),
            Err(JoinError::Arc {
                cause: ArcError::CrossesCenter { .. },
                ..
            })
        ));
        assert_eq!(
            ArcAxis::through(v2(0.0, 0.0), v2(1.0, 0.0), v2(2.0, 0.0), tol),
            Err(ArcError::NoCircle)
        );
    }

    #[test]
    fn three_points_and_radius() {
        let tol = Tol::default();
        let ax = ArcAxis::through(v2(3.0, 0.0), v2(0.0, 3.0), v2(-3.0, 0.0), tol).unwrap();
        near(ax.bulge, 1.0, 1e-12);
        let ax = ArcAxis::through(v2(-3.0, 0.0), v2(0.0, 3.0), v2(3.0, 0.0), tol).unwrap();
        near(ax.bulge, -1.0, 1e-12);
        let ax = ArcAxis::with_radius(v2(3.0, 0.0), v2(0.0, 3.0), 3.0, true, tol).unwrap();
        near(ax.bulge, (PI / 8.0).tan(), 1e-12);
        near(ax.center.len(), 0.0, 1e-12);
        assert!(ArcAxis::with_radius(v2(0.0, 0.0), v2(4.0, 0.0), 1.0, true, tol).is_err());
    }

    #[test]
    fn ring_ccw_keeps_area() {
        let r = BulgeRing {
            pts: vec![v2(-3.0, 0.0), v2(3.0, 0.0)],
            bulges: vec![-1.0, 0.0],
        };
        near(r.area(), -4.5 * PI, 1e-12);
        let c = r.ccw();
        near(c.area(), 4.5 * PI, 1e-12);
        near(c.perimeter(), 3.0 * PI + 6.0, 1e-12);
    }

    #[test]
    fn deterministic_and_budget_10k() {
        let tol = Tol::default();
        let n = 10_000;
        let mut ss = vec![];
        let mut bs = vec![];
        for i in 0..n {
            let (x, y) = ((i % 100) as f64 * 10.0, (i / 100) as f64 * 10.0);
            ss.push(strip(v2(x + 3.0, y), v2(x - 3.0, y), 0.2));
            bs.push(1.0);
        }
        let t = web_time::Instant::now();
        let w = arc_walls(&ss, &bs, tol).unwrap();
        let mut v = 0.0;
        for (i, s) in ss.iter().enumerate() {
            v += wall_quantities(s, 1.0, &w.rings[i], &[], i).unwrap()[0].volume;
        }
        let dt = t.elapsed();
        near(v, n as f64 * 1.8 * PI, 1e-6);
        if !cfg!(debug_assertions) {
            assert!(dt.as_secs_f64() < 1.0, "{dt:?}");
        }
        let w2 = arc_walls(&ss, &bs, tol).unwrap();
        assert_eq!(w.rings, w2.rings);
    }

    #[test]
    fn tangent_from_point_touches_at_right_angle() {
        let (c, r, p) = (v2(1.0, 2.0), 2.0, v2(7.0, 5.0));
        for q in tangent_points(c, r, p).expect("outside") {
            assert!(((q - c).len() - r).abs() < 1e-9);
            assert!((q - c).dot(p - q).abs() < 1e-9);
        }
    }

    #[test]
    fn tangent_from_point_inside_or_on_is_none() {
        assert!(tangent_points(v2(0.0, 0.0), 1.0, v2(0.5, 0.0)).is_none());
        assert!(tangent_points(v2(0.0, 0.0), 1.0, v2(1.0, 0.0)).is_none());
    }

    #[test]
    fn quadrants_of_a_half_circle_and_a_clockwise_arc() {
        let tol = Tol::default();
        let up = ArcAxis::new(v2(4.0, 0.0), v2(0.0, 0.0), 1.0, tol).unwrap();
        let q = quadrants(&up);
        assert_eq!(q.len(), 3, "{q:?}");
        assert!(q.iter().all(|p| ((*p - up.center).len() - up.radius).abs() < 1e-9));
        let cw = ArcAxis::new(v2(0.0, 0.0), v2(4.0, 0.0), 1.0, tol).unwrap();
        assert_eq!(quadrants(&cw).len(), 3);
        assert_ne!(
            quadrants(&up).iter().any(|p| (p.y - 2.0).abs() < 1e-9),
            quadrants(&cw).iter().any(|p| (p.y - 2.0).abs() < 1e-9)
        );
    }
}
