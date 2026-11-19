// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Profile sweeps along edges: fascia board, gutter, slab edge, wall cornice/molding,
//! rustication (subtracted from the wall). The profile is a closed polyline with bulge in local coordinates (x outward from the path,
//! y up). The path is a plan polyline with arcs (bulge) and elevations; the profile is plumb (the profile
//! plane contains the vertical), and at kinks a mitre along the bisector (x → x/cos(α/2)) no longer than
//! `MITER_LIMIT`·x is used, with an outer bevel for sharper angles; the inner side is compressed on tight arcs so that
//! rings do not invert. Open ends get a perpendicular end face. Outward is to the right of the path.

use crate::arc::BulgeRing;
use crate::cut::Cutter;
use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::poly;

/// Chord sagitta used when subdividing profile and path arcs.
pub const SWEEP_SAG: f64 = 5e-4;

#[derive(Clone, Debug, PartialEq)]
pub enum SweepError {
    /// Profile: fewer than 3 vertices, non-finite coordinates, zero area or self-intersection.
    BadProfile,
    /// Path: fewer than 2 points, non-finite coordinates or a vertical segment.
    BadPath,
}

/// Profile: vertices (x, y, bulge of the edge to the next vertex).
#[derive(Clone, Debug, PartialEq)]
pub struct SectionProfile {
    pub pts: Vec<(f64, f64, f64)>,
}

impl SectionProfile {
    pub fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            pts: vec![(x0, y0, 0.0), (x1, y0, 0.0), (x1, y1, 0.0), (x0, y1, 0.0)],
        }
    }

    pub fn ring(&self) -> BulgeRing {
        BulgeRing {
            pts: self.pts.iter().map(|p| v2(p.0, p.1)).collect(),
            bulges: self.pts.iter().map(|p| p.2).collect(),
        }
    }

    pub fn validate(&self) -> Result<(), SweepError> {
        if self.pts.len() < 3
            || !self
                .pts
                .iter()
                .all(|p| p.0.is_finite() && p.1.is_finite() && p.2.is_finite() && p.2.abs() <= 1e3)
        {
            return Err(SweepError::BadProfile);
        }
        let o = self.outline();
        if self.area() < 1e-10 || poly::simple_ring(&o, 1e-9).is_err() {
            return Err(SweepError::BadProfile);
        }
        Ok(())
    }

    /// Profile polygon, counter-clockwise (arcs subdivided).
    pub fn outline(&self) -> Vec<V2> {
        let r = self.ring().ccw();
        let mut o = r.tessellate(SWEEP_SAG);
        o.dedup_by(|a, b| (*a - *b).len() < 1e-12);
        while o.len() > 1 && (o[0] - o[o.len() - 1]).len() < 1e-12 {
            o.pop();
        }
        o
    }

    /// Exact area (including arc segments).
    pub fn area(&self) -> f64 {
        self.ring().area().abs()
    }

    /// The part of the profile inside the host (x ≤ 0), which is what rustication subtracts.
    pub fn inside(&self) -> Vec<V2> {
        poly::clip_half(&self.outline(), v2(1.0, 0.0), 0.0)
    }

    pub fn shifted(&self, dx: f64, dy: f64) -> Self {
        Self {
            pts: self.pts.iter().map(|p| (p.0 + dx, p.1 + dy, p.2)).collect(),
        }
    }

    /// JSON text: `[[x, y, bulge], ...]`.
    pub fn to_json(&self) -> String {
        let items: Vec<String> = self
            .pts
            .iter()
            .map(|p| format!("[{:?},{:?},{:?}]", p.0, p.1, p.2))
            .collect();
        format!("[{}]", items.join(","))
    }

    /// Parses `[[x, y], [x, y, bulge], ...]` (bulge defaults to 0).
    pub fn from_json(s: &str) -> Option<Self> {
        let t = s.trim();
        let inner = t.strip_prefix('[')?.strip_suffix(']')?.trim();
        let mut pts = vec![];
        if inner.is_empty() {
            return Some(Self { pts });
        }
        let mut rest = inner;
        loop {
            let open = rest.find('[')?;
            let close = rest[open..].find(']')? + open;
            let nums: Vec<f64> = rest[open + 1..close]
                .split(',')
                .map(|x| x.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .ok()?;
            match nums.as_slice() {
                [x, y] => pts.push((*x, *y, 0.0)),
                [x, y, b] => pts.push((*x, *y, *b)),
                _ => return None,
            }
            rest = rest[close + 1..].trim_start();
            if rest.is_empty() {
                break;
            }
            rest = rest.strip_prefix(',')?;
        }
        Some(Self { pts })
    }
}

/// Sweep kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SweepKind {
    Fascia,
    Gutter,
    SlabEdge,
    /// Cornice, molding or skirting along a wall face.
    WallSweep,
    /// Rustication: subtracted from the wall.
    Reveal,
}

impl SweepKind {
    pub const ALL: [SweepKind; 5] = [
        SweepKind::Fascia,
        SweepKind::Gutter,
        SweepKind::SlabEdge,
        SweepKind::WallSweep,
        SweepKind::Reveal,
    ];
    pub fn name(self) -> &'static str {
        match self {
            SweepKind::Fascia => "fascia",
            SweepKind::Gutter => "gutter",
            SweepKind::SlabEdge => "slab_edge",
            SweepKind::WallSweep => "wall_sweep",
            SweepKind::Reveal => "reveal",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }
    pub fn subtracts(self) -> bool {
        self == SweepKind::Reveal
    }
}

/// Built-in profile library: (name, default kind, profile). A model's own profiles
/// with the same name replace the built-in one.
pub fn library() -> Vec<(&'static str, SweepKind, SectionProfile)> {
    let q = (std::f64::consts::PI / 8.0).tan();
    vec![
        (
            "fascia_200",
            SweepKind::Fascia,
            SectionProfile::rect(0.0, -0.2, 0.025, 0.0),
        ),
        (
            "gutter_half_round_150",
            SweepKind::Gutter,
            SectionProfile {
                pts: vec![
                    (0.025, -0.05, 1.0),
                    (0.175, -0.05, 0.0),
                    (0.17, -0.05, -1.0),
                    (0.03, -0.05, 0.0),
                ],
            },
        ),
        (
            "slab_edge_200x450",
            SweepKind::SlabEdge,
            SectionProfile::rect(0.0, -0.45, 0.2, 0.0),
        ),
        (
            "cornice_ovolo",
            SweepKind::WallSweep,
            SectionProfile {
                pts: vec![
                    (0.0, 0.0, 0.0),
                    (0.03, 0.0, 0.0),
                    (0.03, 0.02, q),
                    (0.13, 0.12, 0.0),
                    (0.15, 0.12, 0.0),
                    (0.15, 0.16, 0.0),
                    (0.0, 0.16, 0.0),
                ],
            },
        ),
        (
            "base_100x20",
            SweepKind::WallSweep,
            SectionProfile::rect(0.0, 0.0, 0.02, 0.1),
        ),
        (
            "reveal_20x20",
            SweepKind::Reveal,
            SectionProfile::rect(-0.02, -0.01, 0.0, 0.01),
        ),
        (
            "reveal_v_30",
            SweepKind::Reveal,
            SectionProfile {
                pts: vec![(0.0, -0.015, 0.0), (0.0, 0.015, 0.0), (-0.015, 0.0, 0.0)],
            },
        ),
    ]
}

/// Plan path with elevations: segment i runs from `pts[i]` to `pts[i+1]` with `bulges[i]` (plan arc,
/// elevation linear along the length); a closed path adds a segment from the last point to the first.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub pts: Vec<V3>,
    pub bulges: Vec<f64>,
    pub closed: bool,
}

fn p2(p: V3) -> V2 {
    v2(p.x, p.y)
}

impl Path {
    pub fn open(pts: Vec<V3>) -> Self {
        let n = pts.len().saturating_sub(1);
        Self {
            pts,
            bulges: vec![0.0; n],
            closed: false,
        }
    }

    pub fn closed(pts: Vec<V3>) -> Self {
        let n = pts.len();
        Self {
            pts,
            bulges: vec![0.0; n],
            closed: true,
        }
    }

    fn segs(&self) -> usize {
        if self.closed {
            self.pts.len()
        } else {
            self.pts.len().saturating_sub(1)
        }
    }

    fn bulge(&self, i: usize) -> f64 {
        self.bulges.get(i).copied().unwrap_or(0.0)
    }

    /// Reversed traversal (arcs flip sign).
    pub fn reversed(&self) -> Self {
        let mut pts = self.pts.clone();
        pts.reverse();
        let m = self.segs();
        let mut bulges: Vec<f64> = (0..m).map(|i| -self.bulge(i)).collect();
        if self.closed {
            // segment i (i→i+1) becomes segment n−2−i after reversal; the closing segment stays the closing one
            let n = pts.len();
            let old = bulges.clone();
            for (i, b) in bulges.iter_mut().enumerate() {
                *b = old[(2 * n - 2 - i) % n];
            }
        } else {
            bulges.reverse();
        }
        Self {
            pts,
            bulges,
            closed: self.closed,
        }
    }

    /// Path points (arcs subdivided); for a closed path the first point is not repeated.
    pub fn tessellate(&self) -> Vec<V3> {
        self.tessellate_sag(SWEEP_SAG)
    }

    fn tessellate_sag(&self, sag: f64) -> Vec<V3> {
        let n = self.pts.len();
        let mut out: Vec<V3> = vec![];
        for i in 0..self.segs() {
            let (p, q) = (self.pts[i], self.pts[(i + 1) % n]);
            let b = self.bulge(i);
            let plan = BulgeRing {
                pts: vec![p2(p), p2(q)],
                bulges: vec![b, 0.0],
            }
            .tessellate(sag);
            let mut acc = vec![0.0];
            for w in plan.windows(2) {
                acc.push(acc[acc.len() - 1] + (w[1] - w[0]).len());
            }
            acc.push(acc[acc.len() - 1] + (p2(q) - plan[plan.len() - 1]).len());
            let total = acc[acc.len() - 1].max(1e-300);
            for (k, a) in plan.iter().enumerate() {
                let z = p.z + (q.z - p.z) * acc[k] / total;
                out.push(v3(a.x, a.y, z));
            }
        }
        if !self.closed {
            if let Some(&l) = self.pts.last() {
                out.push(l);
            }
        }
        out.dedup_by(|a, b| (*a - *b).len() < 1e-9);
        if self.closed {
            while out.len() > 1 && (out[0] - out[out.len() - 1]).len() < 1e-9 {
                out.pop();
            }
        }
        out
    }

    /// Length along the axis (arcs exact, rise measured along the chord of the arc length).
    pub fn length(&self) -> f64 {
        let n = self.pts.len();
        (0..self.segs())
            .map(|i| {
                let (p, q) = (self.pts[i], self.pts[(i + 1) % n]);
                let l = crate::arc::axis_length(p2(p), p2(q), self.bulge(i));
                l.hypot(q.z - p.z)
            })
            .sum()
    }
}

/// Mitre length limit (as a multiple of the x offset): sharper angles get the outer corner bevelled.
pub const MITER_LIMIT: f64 = 4.0;
/// Largest chord sagitta of rustication cutters: coarser than the body, but bounded.
pub const CUTTER_SAG: f64 = 2e-3;
/// Fraction of a segment by which a profile's inner point may slide along it at a kink.
const INNER_SHARE: f64 = 0.45;

fn rn(d: V2) -> V2 {
    v2(d.y, -d.x)
}

/// Right-hand horizontal normal of a segment (outward).
fn side(a: V3, b: V3) -> Option<V2> {
    let d = p2(b) - p2(a);
    let l = d.len();
    (l > 1e-9).then(|| v2(d.y / l, -d.x / l))
}

/// Mitre vector at a vertex between segments with normals `s0`, `s1`: an x offset along it
/// lands on both offset lines (bisector). Its length is at most `MITER_LIMIT`; on a
/// reversal it runs along the first segment.
pub fn miter(s0: V2, s1: V2) -> V2 {
    let sum = s0 + s1;
    let l = sum.len();
    if l < 1e-12 {
        return v2(-s0.y, s0.x) * MITER_LIMIT;
    }
    let k = 1.0 + s0.dot(s1);
    sum * (1.0 / k).min(MITER_LIMIT / l)
}

/// Validated path points for a given arc sagitta.
fn points(path: &Path, sag: f64) -> Result<Vec<V3>, SweepError> {
    if path.pts.len() < 2
        || !path
            .pts
            .iter()
            .all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite())
    {
        return Err(SweepError::BadPath);
    }
    let pts = path.tessellate_sag(sag);
    if pts.len() < 2 || path.closed && pts.len() < 3 {
        return Err(SweepError::BadPath);
    }
    Ok(pts)
}

/// Path points and the mitre vector at each point.
pub fn frames(path: &Path) -> Result<(Vec<V3>, Vec<V2>), SweepError> {
    let pts = points(path, SWEEP_SAG)?;
    let n = pts.len();
    let m = if path.closed { n } else { n - 1 };
    let sides: Vec<V2> = (0..m)
        .map(|i| side(pts[i], pts[(i + 1) % n]))
        .collect::<Option<_>>()
        .ok_or(SweepError::BadPath)?;
    let mv = (0..n)
        .map(|k| {
            if path.closed {
                miter(sides[(k + m - 1) % m], sides[k % m])
            } else if k == 0 {
                sides[0]
            } else if k == n - 1 {
                sides[m - 1]
            } else {
                miter(sides[k - 1], sides[k])
            }
        })
        .collect();
    Ok((pts, mv))
}

/// Profile ring at a path point: profile point (x, y) → p + dir·x, elevation p.z + y; dir = `out`
/// on the outer side of the kink (x·turn > 0), otherwise `inn`.
#[derive(Clone, Copy, Debug)]
struct Station {
    p: V3,
    out: V2,
    inn: V2,
    turn: f64,
    /// Second bevel ring: its inner side is shared with the previous ring.
    bevel: bool,
    /// Inner-side compression caused by short neighbouring segments (tight arc), ≤ 1.
    shrink: f64,
}

impl Station {
    fn at(&self, x: f64) -> V2 {
        let s = if x * self.turn > 0.0 {
            self.out
        } else {
            self.inn
        };
        v2(self.p.x + s.x * x, self.p.y + s.y * x)
    }

    fn shared(&self, x: f64) -> bool {
        self.bevel && x * self.turn <= 0.0
    }
}

/// Kink between segments (direction, length): mitre up to `MITER_LIMIT`, sharper gets a two-ring
/// bevel; the inner side is shortened so adjacent rings do not overlap.
fn corner(p: V3, (d0, l0): (V2, f64), (d1, l1): (V2, f64), xr: (f64, f64), out: &mut Vec<Station>) {
    let (s0, s1) = (rn(d0), rn(d1));
    let c = s0.dot(s1).clamp(-1.0, 1.0);
    let turn = if d0.cross(d1) >= 0.0 { 1.0 } else { -1.0 };
    let sum = s0 + s1;
    let bh = if sum.len() > 1e-12 {
        sum * (1.0 / sum.len())
    } else {
        d0 * turn
    };
    let ch = ((1.0 + c) * 0.5).max(0.0).sqrt();
    let sh = ((1.0 - c) * 0.5).max(0.0).sqrt();
    let full = if ch > 1e-12 { 1.0 / ch } else { f64::INFINITY };
    // largest profile offset on the inner side of the kink
    let xin = (if turn > 0.0 { -xr.0 } else { xr.1 }).max(0.0);
    let mut g = full.min(MITER_LIMIT);
    let mut shrink = 1.0;
    if xin * sh > 1e-300 {
        let gl = INNER_SHARE * l0.min(l1) / (xin * sh);
        if full <= MITER_LIMIT {
            shrink = (gl / full).min(1.0);
        }
        g = g.min(gl);
    }
    let inn = bh * g;
    if full <= MITER_LIMIT {
        out.push(Station {
            p,
            out: bh * full,
            inn,
            turn,
            bevel: false,
            shrink,
        });
    } else {
        out.push(Station {
            p,
            out: s0,
            inn,
            turn,
            bevel: false,
            shrink,
        });
        out.push(Station {
            p,
            out: s1,
            inn,
            turn,
            bevel: true,
            shrink,
        });
    }
}

/// Profile rings along path points; `xr` is the profile's x range.
fn stations(pts: &[V3], closed: bool, xr: (f64, f64)) -> Result<Vec<Station>, SweepError> {
    let n = pts.len();
    if n < 2 || closed && n < 3 {
        return Err(SweepError::BadPath);
    }
    let m = if closed { n } else { n - 1 };
    let segs: Vec<(V2, f64)> = (0..m)
        .map(|i| {
            let d = p2(pts[(i + 1) % n]) - p2(pts[i]);
            let l = d.len();
            (l > 1e-9).then(|| (d * (1.0 / l), l))
        })
        .collect::<Option<_>>()
        .ok_or(SweepError::BadPath)?;
    let mut out = Vec::with_capacity(n + 2);
    for (k, &p) in pts.iter().enumerate() {
        let prev = (closed || k > 0).then(|| segs[(k + m - 1) % m]);
        let next = (closed || k < m).then(|| segs[k % m]);
        match (prev, next) {
            (Some(a), Some(b)) => corner(p, a, b, xr, &mut out),
            (Some((d, _)), None) | (None, Some((d, _))) => {
                let s = rn(d);
                out.push(Station {
                    p,
                    out: s,
                    inn: s,
                    turn: 1.0,
                    bevel: false,
                    shrink: 1.0,
                });
            }
            (None, None) => return Err(SweepError::BadPath),
        }
    }
    // end faces are compressed like the neighbouring ring: on a tight arc the end face does not pass through the centre
    let l = out.len();
    if !closed && l > 2 {
        let (a, b) = (out[1], out[l - 2]);
        out[0].turn = a.turn;
        out[0].inn = out[0].inn * a.shrink;
        out[l - 1].turn = b.turn;
        out[l - 1].inn = out[l - 1].inn * b.shrink;
    }
    Ok(out)
}

fn x_range(r: &[V2]) -> (f64, f64) {
    r.iter()
        .fold((0.0f64, 0.0f64), |(a, b), p| (a.min(p.x), b.max(p.x)))
}

/// Sweep body: profile rings at every path point, with the ends of an open path capped.
/// Faces: sides are tagged by span number, end caps by the next two numbers.
pub fn sweep(path: &Path, prof: &SectionProfile) -> Result<Mesh, SweepError> {
    prof.validate()?;
    let ring = prof.outline();
    let st = stations(&points(path, SWEEP_SAG)?, path.closed, x_range(&ring))?;
    let k = ring.len();
    let mut m = Mesh::default();
    let mut rows: Vec<Vec<u32>> = Vec::with_capacity(st.len());
    for s in &st {
        let prev = rows.last();
        let mut row = Vec::with_capacity(k);
        for (i, q) in ring.iter().enumerate() {
            match prev {
                Some(pr) if s.shared(q.x) => row.push(pr[i]),
                _ => {
                    let a = s.at(q.x);
                    row.push(m.positions.len() as u32);
                    m.positions.push(v3(a.x, a.y, s.p.z + q.y));
                }
            }
        }
        rows.push(row);
    }
    let nr = rows.len();
    let spans = if path.closed { nr } else { nr - 1 };
    for r in 0..spans {
        let (u, w) = (&rows[r], &rows[(r + 1) % nr]);
        for i in 0..k {
            let j = (i + 1) % k;
            let (a, b, c, d) = (u[i], u[j], w[i], w[j]);
            // a bevel shares its inner points, so degenerate triangles are not needed
            if a != c {
                m.triangles.push([a, c, b]);
                m.face_of.push(r as u32);
            }
            if b != d {
                m.triangles.push([b, c, d]);
                m.face_of.push(r as u32);
            }
        }
    }
    if !path.closed {
        let (first, last) = (&rows[0], &rows[nr - 1]);
        let (f0, f1) = (spans as u32, spans as u32 + 1);
        for t in poly::triangulate(&ring) {
            m.triangles.push([first[t[0]], first[t[1]], first[t[2]]]);
            m.face_of.push(f0);
            m.triangles.push([last[t[2]], last[t[1]], last[t[0]]]);
            m.face_of.push(f1);
        }
    }
    // profile winding relative to the path is decided by the sign of the volume
    if m.volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
    }
    Ok(m)
}

/// Sweep quantities: axis length, profile area, volume (from the body) and outer surface.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepQty {
    pub length: f64,
    pub section: f64,
    pub volume: f64,
    pub surface: f64,
}

pub fn quantities(path: &Path, prof: &SectionProfile) -> Result<SweepQty, SweepError> {
    let m = sweep(path, prof)?;
    Ok(SweepQty {
        length: path.length(),
        section: prof.area(),
        volume: m.volume(),
        surface: m.area(),
    })
}

/// Volume that rustication removes from the host: the body of the profile part with x ≤ 0 along the path.
pub fn reveal_volume(path: &Path, prof: &SectionProfile) -> Result<f64, SweepError> {
    let ins = prof.inside();
    if ins.len() < 3 || poly::area(&ins).abs() < 1e-12 {
        return Ok(0.0);
    }
    let p = SectionProfile {
        pts: ins.iter().map(|q| (q.x, q.y, 0.0)).collect(),
    };
    Ok(sweep(path, &p)?.volume())
}

/// Removes intermediate points lying on the line between their neighbours (in plan and in elevation).
fn merge_collinear(pts: Vec<V3>, closed: bool) -> Vec<V3> {
    let n = pts.len();
    if n < 3 {
        return pts;
    }
    let mut out: Vec<V3> = vec![pts[0]];
    for k in 1..n {
        if k + 1 == n && !closed {
            out.push(pts[k]);
            break;
        }
        let (a, next) = (out[out.len() - 1], pts[(k + 1) % n]);
        let (v, w) = (next - a, pts[k] - a);
        let l = v.len();
        let straight = l > 0.0
            && v.cross(w).len() <= 1e-9 * l
            && w.dot(v) > 0.0
            && (next - pts[k]).dot(v) > 0.0;
        if !straight {
            out.push(pts[k]);
        }
    }
    out
}

/// Rustication cutters for wall cut-outs: the parts of the profile inside the wall (x ≤ 0) as height strips;
/// strip depth = area of the part in the strip / strip height (the cut volume matches the exact one
/// on straight segments). A rectangular profile is one strip, exact. Outside, an `overcut` margin is added.
/// Cutter elevations are relative to `z_ref` (path elevation minus `z_ref`). Consecutive straights form one cutter,
/// arcs use a sagitta of at most `CUTTER_SAG`.
pub fn reveal_cutters(
    path: &Path,
    prof: &SectionProfile,
    bands: usize,
    overcut: f64,
    z_ref: f64,
) -> Result<Vec<Cutter>, SweepError> {
    prof.validate()?;
    let ins = prof.inside();
    let a_in = poly::area(&ins).abs();
    if ins.len() < 3 || a_in < 1e-12 {
        return Ok(vec![]);
    }
    let (y0, y1) = ins
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.y), b.max(p.y)));
    let xmin = ins.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let rect = (a_in - (y1 - y0) * (-xmin)).abs() < 1e-9 * a_in.max(1e-6);
    let nb = if rect { 1 } else { bands.max(1) };
    // chord sagitta is a tenth of the rustication depth, clamped to [SWEEP_SAG, CUTTER_SAG]
    let sag = (0.1 * -xmin).clamp(SWEEP_SAG, CUTTER_SAG);
    let pts = merge_collinear(points(path, sag)?, path.closed);
    let st = stations(&pts, path.closed, (xmin, overcut.max(0.0)))?;
    let ns = st.len();
    let spans = if path.closed { ns } else { ns - 1 };
    let mut out = vec![];
    for b in 0..nb {
        let ya = y0 + (y1 - y0) * b as f64 / nb as f64;
        let yb = y0 + (y1 - y0) * (b + 1) as f64 / nb as f64;
        let band = poly::clip_half(&poly::clip_half(&ins, v2(0.0, -1.0), -ya), v2(0.0, 1.0), yb);
        let a = poly::area(&band).abs();
        if band.len() < 3 || a < 1e-14 || yb - ya < 1e-9 {
            continue;
        }
        let depth = a / (yb - ya);
        for r in 0..spans {
            let (sp, sq) = (&st[r], &st[(r + 1) % ns]);
            let z = (sp.p.z + sq.p.z) / 2.0 - z_ref;
            // a pair of bevel rings yields a wedge where the corner is cut off
            let mut ring = vec![sp.at(overcut), sq.at(overcut), sq.at(-depth), sp.at(-depth)];
            ring.dedup_by(|a, b| (*a - *b).len() < 1e-12);
            while ring.len() > 1 && (ring[0] - ring[ring.len() - 1]).len() < 1e-12 {
                ring.pop();
            }
            if ring.len() < 3 || poly::area(&ring).abs() < 1e-14 {
                continue;
            }
            out.push(Cutter {
                ring,
                holes: vec![],
                z0: z + ya,
                z1: z + yb,
            });
        }
    }
    Ok(out)
}

/// Assembles oriented segments into chains (the end of one = the start of the next):
/// (points, closed). Chains from segments with no predecessor come first, then cycles.
pub fn chain(segs: &[(V3, V3)], eps: f64) -> Vec<(Vec<V3>, bool)> {
    let n = segs.len();
    let near = |a: V3, b: V3| (a - b).len() <= eps;
    let starts: Vec<usize> = (0..n)
        .filter(|&k| !(0..n).any(|j| j != k && near(segs[j].1, segs[k].0)))
        .collect();
    let mut used = vec![false; n];
    let mut out = vec![];
    for s in starts.into_iter().chain(0..n) {
        if used[s] {
            continue;
        }
        used[s] = true;
        let mut pts = vec![segs[s].0, segs[s].1];
        while let Some(j) = (0..n).find(|&j| !used[j] && near(segs[j].0, pts[pts.len() - 1])) {
            used[j] = true;
            pts.push(segs[j].1);
        }
        let closed = pts.len() > 3 && near(pts[pts.len() - 1], pts[0]);
        if closed {
            pts.pop();
        }
        out.push((pts, closed));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Tol;

    fn near(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol * b.abs().max(1.0), "{a} vs {b}");
    }

    #[test]
    fn closed_rectangle_volume_is_offset_perimeter_times_section() {
        // outward is to the right of a counter-clockwise traversal: offset perimeter P + 8x
        let (w, h, bw, bh) = (6.0, 4.0, 0.025, 0.2);
        let path = Path::closed(vec![
            v3(0.0, 0.0, 3.0),
            v3(w, 0.0, 3.0),
            v3(w, h, 3.0),
            v3(0.0, h, 3.0),
        ]);
        let prof = SectionProfile::rect(0.0, -bh, bw, 0.0);
        let m = sweep(&path, &prof).unwrap();
        let p = 2.0 * (w + h);
        near(m.volume(), bh * (p * bw + 4.0 * bw * bw), 1e-9);
        assert!(m.is_watertight());
        let q = quantities(&path, &prof).unwrap();
        near(q.length, p, 1e-12);
        near(q.section, bw * bh, 1e-12);
    }

    #[test]
    fn miter_lies_on_bisector_and_l_volume_is_exact() {
        let path = Path::open(vec![
            v3(0.0, 0.0, 0.0),
            v3(4.0, 0.0, 0.0),
            v3(4.0, 3.0, 0.0),
        ]);
        let (w, h) = (0.3, 0.5);
        let prof = SectionProfile::rect(0.0, 0.0, w, h);
        let (pts, mv) = frames(&path).unwrap();
        assert_eq!(pts.len(), 3);
        // mitre at the corner: (1, −1), profile points on the bisector (normal d1 + d2)
        near(mv[1].x, 1.0, 1e-12);
        near(mv[1].y, -1.0, 1e-12);
        let m = sweep(&path, &prof).unwrap();
        let ring = prof.outline().len();
        for i in 0..ring {
            let p = m.positions[ring + i];
            near((p.x - 4.0) + (p.y - 0.0), 0.0, 1e-12);
        }
        // length at offset x: 7 + 2x ⇒ V = h(7w + w²)
        near(m.volume(), h * (7.0 * w + w * w), 1e-9);
        assert!(m.is_watertight());
    }

    #[test]
    fn miter_inside_corner_shortens() {
        // right turn: the outer side (right) is the inner corner, length 7 − 2x
        let path = Path::open(vec![
            v3(0.0, 0.0, 0.0),
            v3(4.0, 0.0, 0.0),
            v3(4.0, -3.0, 0.0),
        ]);
        let (w, h) = (0.3, 0.5);
        let m = sweep(&path, &SectionProfile::rect(0.0, 0.0, w, h)).unwrap();
        near(m.volume(), h * (7.0 * w - w * w), 1e-9);
    }

    #[test]
    fn follows_arc_circle() {
        let r = 5.0;
        let path = Path {
            pts: vec![v3(r, 0.0, 0.0), v3(-r, 0.0, 0.0)],
            bulges: vec![1.0, 1.0],
            closed: true,
        };
        let (w, h) = (0.1, 0.2);
        let m = sweep(&path, &SectionProfile::rect(0.0, 0.0, w, h)).unwrap();
        let exact = 2.0 * std::f64::consts::PI * h * (r * w + w * w / 2.0);
        near(m.volume(), exact, 2e-4);
        near(path.length(), 2.0 * std::f64::consts::PI * r, 1e-12);
        assert!(m.is_watertight());
    }

    #[test]
    fn arc_profile_area_and_gutter_validate() {
        for (name, _, p) in library() {
            p.validate().unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
        let g = &library()[1].2;
        let exact = std::f64::consts::PI / 2.0 * (0.075f64.powi(2) - 0.07f64.powi(2));
        near(g.area(), exact, 1e-9);
        let poly_a = poly::area(&g.outline());
        near(poly_a, exact, 1e-3);
    }

    #[test]
    fn json_round_trip() {
        let p = &library()[3].2;
        let back = SectionProfile::from_json(&p.to_json()).unwrap();
        assert_eq!(&back, p);
        let q = SectionProfile::from_json("[[0,0],[1,0],[1,1,0.5]]").unwrap();
        assert_eq!(q.pts[2], (1.0, 1.0, 0.5));
        assert!(SectionProfile::from_json("[[0,0],[1]]").is_none());
        assert!(SectionProfile::from_json("{}").is_none());
    }

    #[test]
    fn reveal_cutter_subtracts_exact_volume() {
        // wall piece 5 × 0.3 × 3, face y = 0 looks toward −y: path +x, outward to the right (−y)
        let fp = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 0.3), v2(0.0, 0.3)];
        // path longer than the piece: cutter ends do not coincide with the piece ends
        let path = Path::open(vec![v3(-1.0, 0.0, 1.5), v3(6.0, 0.0, 1.5)]);
        let prof = SectionProfile::rect(-0.02, -0.01, 0.0, 0.01);
        let cs = reveal_cutters(&path, &prof, 4, 0.01, 0.0).unwrap();
        assert_eq!(cs.len(), 1);
        let refs: Vec<&Cutter> = cs.iter().collect();
        let body = crate::cut::cut_prism(&fp, 0.0, 3.0, &refs, Tol::default()).unwrap();
        let removed = 0.02 * 0.02 * 5.0;
        near(body.volume, 4.5 - removed, 1e-9);
        near(
            reveal_volume(&path, &prof).unwrap(),
            0.02 * 0.02 * 7.0,
            1e-9,
        );
        assert!(body.mesh.is_watertight());
    }

    #[test]
    fn v_reveal_bands_preserve_area() {
        let path = Path::open(vec![v3(0.0, 0.0, 1.0), v3(2.0, 0.0, 1.0)]);
        let prof = &library()[6].2;
        let cs = reveal_cutters(&path, prof, 6, 0.0, 0.0).unwrap();
        assert_eq!(cs.len(), 6);
        let vol: f64 = cs
            .iter()
            .map(|c| poly::area(&c.ring).abs() * (c.z1 - c.z0))
            .sum();
        near(vol, prof.area() * 2.0, 1e-9);
        // the part outside the wall (x > 0) is not subtracted
        let half = SectionProfile::rect(-0.01, 0.0, 0.01, 0.02);
        near(
            reveal_volume(&path, &half).unwrap(),
            0.01 * 0.02 * 2.0,
            1e-9,
        );
    }

    #[test]
    fn chain_joins_oriented_segments() {
        let a = v3(0.0, 0.0, 0.0);
        let b = v3(1.0, 0.0, 0.0);
        let c = v3(1.0, 1.0, 0.0);
        let d = v3(0.0, 1.0, 0.0);
        let ch = chain(&[(c, d), (a, b), (b, c)], 1e-9);
        assert_eq!(ch, vec![(vec![a, b, c, d], false)]);
        let ring = chain(&[(c, d), (a, b), (d, a), (b, c)], 1e-9);
        assert_eq!(ring.len(), 1);
        assert!(ring[0].1);
        assert_eq!(ring[0].0.len(), 4);
    }

    #[test]
    fn bad_inputs_are_typed_errors() {
        let p = SectionProfile::rect(0.0, 0.0, 0.1, 0.1);
        assert_eq!(
            sweep(&Path::open(vec![v3(0.0, 0.0, 0.0)]), &p).unwrap_err(),
            SweepError::BadPath
        );
        // vertical segment
        assert_eq!(
            sweep(&Path::open(vec![v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0)]), &p).unwrap_err(),
            SweepError::BadPath
        );
        let bad = SectionProfile {
            pts: vec![(0.0, 0.0, 0.0), (1.0, 0.0, 0.0)],
        };
        assert_eq!(bad.validate(), Err(SweepError::BadProfile));
        let nan = SectionProfile::rect(0.0, 0.0, f64::NAN, 1.0);
        assert_eq!(nan.validate(), Err(SweepError::BadProfile));
    }

    #[test]
    fn reversed_path_keeps_geometry() {
        let path = Path {
            pts: vec![v3(0.0, 0.0, 0.0), v3(4.0, 0.0, 0.0), v3(4.0, 3.0, 0.0)],
            bulges: vec![0.3, 0.0],
            closed: false,
        };
        near(path.reversed().length(), path.length(), 1e-12);
        let a: Vec<V3> = path.tessellate();
        let mut b = path.reversed().tessellate();
        b.reverse();
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert!((*p - *q).len() < 1e-9);
        }
    }

    /// Plan distance from a point to the path polyline.
    fn dist_to_path(p: V3, path: &[V3]) -> f64 {
        path.windows(2)
            .map(|w| {
                let (a, b, q) = (p2(w[0]), p2(w[1]), p2(p));
                let d = b - a;
                let t = ((q - a).dot(d) / d.dot(d)).clamp(0.0, 1.0);
                (q - (a + d * t)).len()
            })
            .fold(f64::INFINITY, f64::min)
    }

    fn corner_path(deg: f64, turn: f64) -> Path {
        // interior angle between segments is deg degrees
        let th = (180.0 - deg).to_radians();
        let c = v3(5.0, 0.0, 0.0);
        Path::open(vec![
            v3(0.0, 0.0, 0.0),
            c,
            v3(c.x + 5.0 * th.cos(), turn * 5.0 * th.sin(), 0.0),
        ])
    }

    #[test]
    fn open_path_cap_faces_follow_side_faces() {
        let path = Path::open(vec![v3(0.0, 0.0, 0.0), v3(3.0, 0.0, 0.0)]);
        let m = sweep(&path, &SectionProfile::rect(0.0, 0.0, 0.1, 0.2)).unwrap();
        let max = *m.face_of.iter().max().unwrap();
        assert!(max < u32::MAX / 2);
        // mesh merging takes max + 1, without overflow
        assert_eq!(max.checked_add(1), Some(3));
        let ids: std::collections::BTreeSet<u32> = m.face_of.iter().copied().collect();
        assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(m.face_of.iter().filter(|&&f| f == 1).count(), 2);
        assert_eq!(m.face_of.iter().filter(|&&f| f == 2).count(), 2);
        assert!(m.is_watertight());
    }

    #[test]
    fn acute_corner_is_beveled_without_spikes() {
        let prof = SectionProfile::rect(-0.1, 0.0, 0.1, 0.2);
        let ext = 0.1;
        for turn in [1.0, -1.0] {
            let path = corner_path(20.0, turn);
            let m = sweep(&path, &prof).unwrap();
            assert!(m.is_watertight());
            assert!(m.volume() > 0.0);
            let axis = path.tessellate();
            for p in &m.positions {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                let d = dist_to_path(*p, &axis);
                assert!(d <= MITER_LIMIT * ext + 1e-9, "spike {d} at {p:?}");
            }
            // at a corner, no farther from the vertex than the mitre limit
            let c = v3(5.0, 0.0, 0.0);
            let near_corner = m
                .positions
                .iter()
                .filter(|p| (p2(**p) - p2(c)).len() < 1.0)
                .count();
            assert!(near_corner > 0);
            for p in m.positions.iter().filter(|p| (p2(**p) - p2(c)).len() < 1.0) {
                assert!((p2(*p) - p2(c)).len() <= MITER_LIMIT * ext + 1e-9);
            }
        }
        // a right angle has no bevel: ring count = path point count
        let l = sweep(&corner_path(90.0, 1.0), &prof).unwrap();
        assert_eq!(l.positions.len(), 3 * prof.outline().len());
    }

    #[test]
    fn near_reversal_has_no_nan_or_spike() {
        let prof = SectionProfile::rect(-0.1, 0.0, 0.1, 0.2);
        for turn in [1.0, -1.0] {
            let path = corner_path(1.0, turn);
            let m = sweep(&path, &prof).unwrap();
            assert!(m.is_watertight());
            let axis = path.tessellate();
            for p in &m.positions {
                assert!(p.x.is_finite() && p.y.is_finite() && p.z.is_finite());
                assert!(dist_to_path(*p, &axis) <= MITER_LIMIT * 0.1 + 1e-9);
            }
            assert!(m.volume().is_finite() && m.volume() > 0.0);
        }
        let mv = miter(v2(0.0, -1.0), v2(0.0, 1.0));
        assert!(mv.x.is_finite() && mv.len() <= MITER_LIMIT + 1e-12);
        assert!(miter(v2(0.0, -1.0), v2(1.0, 0.0)).len() <= MITER_LIMIT);
    }

    #[test]
    fn tight_arc_inner_side_does_not_invert() {
        // profile deeper inward than the arc radius: inner points are compressed, rings do not overlap
        let r = 0.5;
        let path = Path {
            pts: vec![v3(r, 0.0, 0.0), v3(0.0, r, 0.0)],
            bulges: vec![(std::f64::consts::PI / 8.0).tan(), 0.0],
            closed: false,
        };
        // counter-clockwise traversal: the inner side (toward the centre) is x < 0
        let prof = SectionProfile::rect(-0.8, 0.0, 0.1, 0.2);
        let m = sweep(&path, &prof).unwrap();
        assert!(m.is_watertight());
        assert!(m.volume() > 0.0);
        let ring = prof.outline();
        let k = ring.len();
        let rows = m.positions.len() / k;
        let inner = ring.iter().position(|q| q.x < -0.79 && q.y < 0.1).unwrap();
        // inner points follow the path direction (the angle about the centre does not decrease)
        let ang = |p: V3| p.y.atan2(p.x);
        assert!(rows > 3);
        for w in 0..rows - 1 {
            let (a, b) = (m.positions[w * k + inner], m.positions[(w + 1) * k + inner]);
            assert!(a.x.hypot(a.y) > 0.0 && a.x.hypot(a.y) <= r + 1e-9);
            assert!(ang(b) > ang(a) - 1e-9, "{a:?} -> {b:?}");
        }
    }

    #[test]
    fn collinear_cutters_merge_and_arcs_are_coarser() {
        let path = Path::open(vec![
            v3(0.0, 0.0, 1.0),
            v3(1.0, 0.0, 1.0),
            v3(3.0, 0.0, 1.0),
            v3(6.0, 0.0, 1.0),
        ]);
        let prof = SectionProfile::rect(-0.02, -0.01, 0.0, 0.01);
        let cs = reveal_cutters(&path, &prof, 4, 0.05, 0.0).unwrap();
        assert_eq!(cs.len(), 1);
        near(poly::area(&cs[0].ring).abs(), 6.0 * 0.07, 1e-9);
        let v = &library()[6].2;
        assert_eq!(reveal_cutters(&path, v, 4, 0.0, 0.0).unwrap().len(), 4);
        // R5 arc of length 10 m: previously ≈ 71 segments per strip
        let (r, th) = (5.0f64, 1.0f64);
        let arc = Path {
            pts: vec![
                v3(r * th.cos(), -r * th.sin(), 0.0),
                v3(r * th.cos(), r * th.sin(), 0.0),
            ],
            bulges: vec![(th / 2.0).tan(), 0.0],
            closed: false,
        };
        let ca = reveal_cutters(&arc, &prof, 4, 0.05, 0.0).unwrap();
        assert!(ca.len() >= 10 && ca.len() <= 40, "{}", ca.len());
        // sagitta is bounded: midpoints of the cutter's outer chords are within CUTTER_SAG of the circle
        for c in &ca {
            let m = (c.ring[0] + c.ring[1]) * 0.5;
            assert!(
                (m.len() - (r + 0.05)).abs() <= CUTTER_SAG + 1e-6,
                "{}",
                m.len()
            );
        }
    }
}
