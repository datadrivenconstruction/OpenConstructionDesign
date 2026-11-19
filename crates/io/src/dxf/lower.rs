//! Raw records the document keeps verbatim (POLYLINE/VERTEX/SEQEND, ELLIPSE, SPLINE, HATCH,
//! SOLID/TRACE, DIMENSION, LEADER) turned into drawable entities: polylines, fills and inserts of
//! the anonymous dimension blocks. Curves are evaluated on the true curve within a chord tolerance.

use std::f64::consts::TAU;

use super::entities::*;
use super::Pair;

/// Hatch pattern line family (OCS, already scaled and rotated by the hatch).
#[derive(Debug, Clone, PartialEq)]
pub struct PatLine {
    /// Degrees.
    pub angle: f64,
    pub base: (f64, f64),
    pub offset: (f64, f64),
    /// Dash lengths: positive — dash, negative — gap, zero — dot.
    pub dashes: Vec<f64>,
}

/// HATCH, SOLID or TRACE: boundary loops (OCS of the entity), solid flag and pattern lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub common: Common,
    pub loops: Vec<Vec<(f64, f64)>>,
    pub solid: bool,
    pub pattern: String,
    pub lines: Vec<PatLine>,
    /// Pattern segments (filled while expanding, world coordinates).
    pub segs: Vec<((f64, f64), (f64, f64))>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lowered {
    Ent(Entity),
    Fill(Fill),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LowerReport {
    /// Raw records turned into geometry.
    pub lowered: usize,
    /// Records with code 67 = 1 (paper space) left out.
    pub paper: usize,
    /// Raw records with no plan geometry (3DSOLID, IMAGE, MLEADER…).
    pub unsupported: usize,
    /// Those records by type name.
    pub unsupported_types: std::collections::BTreeMap<String, usize>,
}

fn f(p: &Pair) -> f64 {
    p.value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

fn i(p: &Pair) -> i64 {
    p.value.trim().parse::<i64>().unwrap_or(0)
}

fn get(rec: &[Pair], code: i32) -> Option<&Pair> {
    rec.iter().skip(1).find(|p| p.code == code)
}

fn getf(rec: &[Pair], code: i32, d: f64) -> f64 {
    get(rec, code).map_or(d, f)
}

/// Code 67 = 1: the entity lives in paper space.
pub fn in_paper(e: &Entity) -> bool {
    let paper = |ps: &[Pair]| ps.iter().any(|p| p.code == 67 && p.value.trim() == "1");
    match e {
        Entity::Line(x) => paper(&x.common.extra),
        Entity::LwPolyline(x) => paper(&x.common.extra),
        Entity::Circle(x) => paper(&x.common.extra),
        Entity::Arc(x) => paper(&x.common.extra),
        Entity::Text(x) => paper(&x.common.extra),
        Entity::MText(x) => paper(&x.common.extra),
        Entity::Insert(x) => paper(&x.common.extra),
        Entity::AttDef(x) => paper(&x.common.extra),
        Entity::Unknown(r) => paper(r),
    }
}

/// Layer/linetype/colour/weight of a raw record; `wcs` drops the extrusion (the geometry is world).
fn common_of(rec: &[Pair], wcs: bool) -> Common {
    let mut c = Common::default();
    for p in rec.iter().skip(1) {
        match p.code {
            8 => c.layer = p.value.clone(),
            6 => c.linetype = Some(p.value.clone()),
            62 => c.color = Some(i(p) as i32),
            420 => c.true_color = Some(i(p) as i32),
            370 => c.lineweight = Some(i(p) as i32),
            48 | 67 => c.extra.push(p.clone()),
            210 | 220 | 230 if !wcs => c.extra.push(p.clone()),
            _ => {}
        }
    }
    c
}

fn poly(common: Common, pts: Vec<(f64, f64)>, bulges: Option<Vec<f64>>, closed: bool) -> Entity {
    let b = bulges.unwrap_or_default();
    Entity::LwPolyline(LwPolyline {
        common,
        flags: closed as i32,
        vertices: pts
            .iter()
            .enumerate()
            .map(|(k, &(x, y))| LwVertex {
                x,
                y,
                bulge: b.get(k).copied().unwrap_or(0.0),
                ..Default::default()
            })
            .collect(),
    })
}

/// Lowers one entity list (model space, a block body). Typed entities pass through; POLYLINE
/// sequences consume their VERTEX/SEQEND records. `paper`: keep paper-space records too.
pub fn lower_list(list: &[Entity], paper: bool, rep: &mut LowerReport) -> Vec<Lowered> {
    let mut out = vec![];
    let mut k = 0;
    while k < list.len() {
        let e = &list[k];
        k += 1;
        if !paper && in_paper(e) {
            rep.paper += 1;
            if matches!(e, Entity::Unknown(r) if r[0].value == "POLYLINE") {
                while k < list.len() && is_rec(&list[k], &["VERTEX", "SEQEND"]) {
                    k += 1;
                }
            }
            continue;
        }
        let Entity::Unknown(rec) = e else {
            out.push(Lowered::Ent(e.clone()));
            continue;
        };
        let before = out.len();
        match rec[0].value.as_str() {
            "POLYLINE" => {
                let mut verts: Vec<&[Pair]> = vec![];
                while k < list.len() {
                    match &list[k] {
                        Entity::Unknown(v) if v[0].value == "VERTEX" => verts.push(v),
                        Entity::Unknown(v) if v[0].value == "SEQEND" => {
                            k += 1;
                            break;
                        }
                        _ => break,
                    }
                    k += 1;
                }
                polyline(rec, &verts, &mut out);
            }
            "VERTEX" | "SEQEND" => continue,
            "ELLIPSE" => out.extend(ellipse(rec, f64::INFINITY).map(Lowered::Ent)),
            "SPLINE" => out.extend(spline(rec, f64::INFINITY).map(Lowered::Ent)),
            "HATCH" => out.extend(hatch(rec).map(Lowered::Fill)),
            "SOLID" | "TRACE" => out.extend(solid(rec).map(Lowered::Fill)),
            "DIMENSION" | "ARC_DIMENSION" => {
                if let Some(b) = get(rec, 2).filter(|p| !p.value.trim().is_empty()) {
                    out.push(Lowered::Ent(Entity::Insert(Insert {
                        common: common_of(rec, false),
                        block: b.value.clone(),
                        ..Default::default()
                    })));
                }
            }
            // Kept verbatim; INSERT expansion draws their annot stand-ins (arrowheads, frames).
            "LEADER" | "MULTILEADER" | "MLEADER" | "IMAGE" | "3DSOLID" | "REGION" | "BODY"
            | "ACIDBLOCKREFERENCE" | "ACAD_TABLE" => {
                if !super::annot::lower(rec, &Default::default()).is_empty() {
                    out.push(Lowered::Ent(e.clone()));
                }
            }
            _ => {}
        }
        if out.len() > before {
            rep.lowered += 1;
        } else {
            rep.unsupported += 1;
            *rep.unsupported_types
                .entry(rec[0].value.clone())
                .or_default() += 1;
        }
    }
    out
}

fn is_rec(e: &Entity, kinds: &[&str]) -> bool {
    matches!(e, Entity::Unknown(r) if kinds.contains(&r[0].value.as_str()))
}

/// All (x, y) of point code `base` in order.
fn points(rec: &[Pair], base: i32) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = vec![];
    for p in rec.iter().skip(1) {
        if p.code == base {
            out.push((f(p), 0.0));
        } else if p.code == base + 10 {
            if let Some(l) = out.last_mut() {
                l.1 = f(p);
            }
        }
    }
    out
}

fn polyline(rec: &[Pair], verts: &[&[Pair]], out: &mut Vec<Lowered>) {
    let flags = getf(rec, 70, 0.0) as i64;
    let closed = flags & 1 != 0;
    let xy = |v: &[Pair]| (getf(v, 10, 0.0), getf(v, 20, 0.0));
    let vflag = |v: &[Pair]| getf(v, 70, 0.0) as i64;
    if flags & 64 != 0 {
        // polyface: vertices (flag 64|128 with coordinates) then faces (flag 128 only, 71..74)
        let c = common_of(rec, true);
        let pts: Vec<(f64, f64)> = verts
            .iter()
            .filter(|v| vflag(v) & 64 != 0)
            .map(|v| xy(v))
            .collect();
        for face in verts.iter().filter(|v| vflag(v) & 64 == 0) {
            let idx: Vec<i64> = (71..=74)
                .filter_map(|c| get(face, c).map(i))
                .filter(|v| *v != 0)
                .collect();
            for (n, &a) in idx.iter().enumerate() {
                let b = idx[(n + 1) % idx.len()];
                // negative index: invisible edge
                if a < 0 || idx.len() < 2 {
                    continue;
                }
                let (pa, pb) = (
                    pts.get(a as usize - 1),
                    pts.get(b.unsigned_abs() as usize - 1),
                );
                if let (Some(&a), Some(&b)) = (pa, pb) {
                    out.push(Lowered::Ent(Entity::Line(Line {
                        common: c.clone(),
                        start: [a.0, a.1, 0.0],
                        end: [b.0, b.1, 0.0],
                    })));
                }
            }
        }
        return;
    }
    if flags & 16 != 0 {
        // polygon mesh M×N: rows and columns
        let (m, n) = (getf(rec, 71, 0.0) as usize, getf(rec, 72, 0.0) as usize);
        let pts: Vec<(f64, f64)> = verts.iter().map(|v| xy(v)).collect();
        if m == 0 || n == 0 || pts.len() < m * n {
            return;
        }
        let c = common_of(rec, true);
        for r in 0..m {
            let row: Vec<_> = (0..n).map(|j| pts[r * n + j]).collect();
            out.push(Lowered::Ent(poly(c.clone(), row, None, flags & 32 != 0)));
        }
        for j in 0..n {
            let col: Vec<_> = (0..m).map(|r| pts[r * n + j]).collect();
            out.push(Lowered::Ent(poly(c.clone(), col, None, closed)));
        }
        return;
    }
    // 2D (OCS) or 3D (world) polyline; spline frame control points (flag 16) are not on the curve
    let three_d = flags & 8 != 0;
    let on_curve: Vec<&&[Pair]> = verts.iter().filter(|v| vflag(v) & 16 == 0).collect();
    let pts: Vec<(f64, f64)> = on_curve.iter().map(|v| xy(v)).collect();
    let bulges: Vec<f64> = on_curve.iter().map(|v| getf(v, 42, 0.0)).collect();
    if pts.len() >= 2 {
        out.push(Lowered::Ent(poly(
            common_of(rec, three_d),
            pts,
            (!three_d).then_some(bulges),
            closed,
        )));
    }
}

/// Segments for an arc sweep `span` (rad) at radius `r` with chord sag ≤ 1e-3·r.
/// `sag` (drawing units) caps the absolute sag; infinity leaves only the relative bound.
fn steps(span: f64, r: f64, sag: f64) -> usize {
    let rel = (sag / r.max(1e-300)).clamp(1e-6, 1e-3);
    let dt = 2.0 * (1.0f64 - rel).acos();
    ((span.abs() / dt).ceil() as usize).clamp(4, 4096)
}

fn ellipse(rec: &[Pair], sag: f64) -> Option<Entity> {
    let c = (getf(rec, 10, 0.0), getf(rec, 20, 0.0));
    let m = [getf(rec, 11, 0.0), getf(rec, 21, 0.0), getf(rec, 31, 0.0)];
    let n = [
        getf(rec, 210, 0.0),
        getf(rec, 220, 0.0),
        getf(rec, 230, 1.0),
    ];
    let ratio = getf(rec, 40, 1.0);
    let (t0, mut t1) = (getf(rec, 41, 0.0), getf(rec, 42, TAU));
    let r = (m[0] * m[0] + m[1] * m[1] + m[2] * m[2]).sqrt();
    if r <= 0.0 || ratio <= 0.0 {
        return None;
    }
    // minor axis = ratio · (n × m)
    let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-300);
    let n = [n[0] / nl, n[1] / nl, n[2] / nl];
    let minor = (
        ratio * (n[1] * m[2] - n[2] * m[1]),
        ratio * (n[2] * m[0] - n[0] * m[2]),
    );
    while t1 <= t0 {
        t1 += TAU;
    }
    let full = (t1 - t0 - TAU).abs() < 1e-9;
    let k = steps(t1 - t0, r, sag);
    let last = if full { k } else { k + 1 };
    let pts: Vec<(f64, f64)> = (0..last)
        .map(|j| {
            let t = t0 + (t1 - t0) * j as f64 / k as f64;
            (
                c.0 + m[0] * t.cos() + minor.0 * t.sin(),
                c.1 + m[1] * t.cos() + minor.1 * t.sin(),
            )
        })
        .collect();
    Some(poly(common_of(rec, true), pts, None, full))
}

/// ELLIPSE or SPLINE record as an editable polyline; `sag`: max chord sag in drawing units.
pub(crate) fn curve(rec: &[Pair], sag: f64) -> Option<Entity> {
    match rec.first()?.value.trim() {
        "ELLIPSE" => ellipse(rec, sag),
        "SPLINE" => spline(rec, sag),
        _ => None,
    }
}

/// Rational B-spline (de Boor in homogeneous coordinates).
pub(crate) struct Nurbs {
    pub degree: usize,
    pub knots: Vec<f64>,
    pub ctrl: Vec<(f64, f64)>,
    pub weights: Vec<f64>,
}

impl Nurbs {
    /// Missing or inconsistent knots become a clamped uniform vector; weights default to 1.
    pub fn new(
        degree: usize,
        mut knots: Vec<f64>,
        ctrl: Vec<(f64, f64)>,
        mut weights: Vec<f64>,
    ) -> Option<Self> {
        let n = ctrl.len();
        let p = degree.clamp(1, 11).min(n.saturating_sub(1));
        if n < 2 || p == 0 {
            return None;
        }
        let ok = knots.len() == n + p + 1 && knots.windows(2).all(|w| w[1] >= w[0]);
        if !ok {
            knots = (0..n + p + 1)
                .map(|j| j.saturating_sub(p).min(n - p) as f64)
                .collect();
        }
        if weights.len() != n || weights.iter().any(|w| !(*w > 0.0)) {
            weights = vec![1.0; n];
        }
        Some(Nurbs {
            degree: p,
            knots,
            ctrl,
            weights,
        })
    }

    pub fn domain(&self) -> (f64, f64) {
        (self.knots[self.degree], self.knots[self.ctrl.len()])
    }

    pub fn at(&self, t: f64) -> (f64, f64) {
        let p = self.degree;
        let n = self.ctrl.len();
        let (lo, hi) = self.domain();
        let t = t.clamp(lo, hi);
        let mut s = p;
        while s + 1 < n && self.knots[s + 1] <= t {
            s += 1;
        }
        let mut d: Vec<[f64; 3]> = (0..=p)
            .map(|j| {
                let k = s + j - p;
                let w = self.weights[k];
                [self.ctrl[k].0 * w, self.ctrl[k].1 * w, w]
            })
            .collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let k = j + s - p;
                let den = self.knots[k + p + 1 - r] - self.knots[k];
                let a = if den.abs() < 1e-300 {
                    0.0
                } else {
                    (t - self.knots[k]) / den
                };
                for c in 0..3 {
                    d[j][c] = (1.0 - a) * d[j - 1][c] + a * d[j][c];
                }
            }
        }
        let w = d[p][2];
        (d[p][0] / w, d[p][1] / w)
    }

    /// Points on the curve: every knot span subdivided until the chord sag is within `tol`.
    pub fn points(&self, tol: f64) -> Vec<(f64, f64)> {
        let (lo, hi) = self.domain();
        let mut spans: Vec<f64> = self.knots[self.degree..=self.ctrl.len()]
            .iter()
            .copied()
            .filter(|k| *k >= lo && *k <= hi)
            .collect();
        spans.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
        let mut out = vec![self.at(lo)];
        let mut budget = 20_000usize;
        for w in spans.windows(2) {
            let quarter = (w[1] - w[0]) / 4.0;
            for q in 0..4 {
                let (a, b) = (w[0] + quarter * q as f64, w[0] + quarter * (q + 1) as f64);
                self.refine(a, b, tol, 0, &mut out, &mut budget);
            }
        }
        out
    }

    fn refine(
        &self,
        a: f64,
        b: f64,
        tol: f64,
        depth: u32,
        out: &mut Vec<(f64, f64)>,
        budget: &mut usize,
    ) {
        let (pa, pb, pm) = (self.at(a), self.at(b), self.at((a + b) / 2.0));
        let chord = ((pa.0 + pb.0) / 2.0 - pm.0).hypot((pa.1 + pb.1) / 2.0 - pm.1);
        if depth < 10 && chord > tol && *budget > 0 {
            *budget -= 1;
            self.refine(a, (a + b) / 2.0, tol, depth + 1, out, budget);
            self.refine((a + b) / 2.0, b, tol, depth + 1, out, budget);
        } else {
            out.push(pb);
        }
    }
}

fn bbox_diag(pts: &[(f64, f64)]) -> f64 {
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for p in pts {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    (hi.0 - lo.0).hypot(hi.1 - lo.1)
}

/// Uniform Catmull-Rom through fit points (8 pieces per span).
fn through(fit: &[(f64, f64)], closed: bool) -> Vec<(f64, f64)> {
    let n = fit.len();
    if n < 3 {
        return fit.to_vec();
    }
    let at = |k: isize| -> (f64, f64) {
        if closed {
            fit[k.rem_euclid(n as isize) as usize]
        } else {
            fit[k.clamp(0, n as isize - 1) as usize]
        }
    };
    let spans = if closed { n } else { n - 1 };
    let mut out = vec![fit[0]];
    for s in 0..spans as isize {
        let (p0, p1, p2, p3) = (at(s - 1), at(s), at(s + 1), at(s + 2));
        for j in 1..=8 {
            let t = j as f64 / 8.0;
            let (t2, t3) = (t * t, t * t * t);
            let c = |a: f64, b: f64, c: f64, d: f64| {
                0.5 * (2.0 * b
                    + (-a + c) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (-a + 3.0 * b - 3.0 * c + d) * t3)
            };
            out.push((c(p0.0, p1.0, p2.0, p3.0), c(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    if closed {
        out.pop();
    }
    out
}

fn spline(rec: &[Pair], sag: f64) -> Option<Entity> {
    let flags = getf(rec, 70, 0.0) as i64;
    let closed = flags & 1 != 0;
    let degree = getf(rec, 71, 3.0) as usize;
    let knots: Vec<f64> = rec.iter().filter(|p| p.code == 40).map(f).collect();
    let weights: Vec<f64> = rec.iter().filter(|p| p.code == 41).map(f).collect();
    let ctrl = points(rec, 10);
    let fit = points(rec, 11);
    let c = common_of(rec, true);
    let pts = match Nurbs::new(degree, knots, ctrl.clone(), weights) {
        Some(s) if ctrl.len() > degree.max(1) => s.points((bbox_diag(&ctrl) * 5e-4).min(sag).max(1e-9)),
        _ if fit.len() >= 2 => through(&fit, closed),
        _ => return None,
    };
    let mut pts = pts;
    // closed curve: the last point repeats the first
    if closed && pts.len() > 2 {
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        if (a.0 - b.0).hypot(a.1 - b.1) <= 1e-9 * bbox_diag(&pts).max(1.0) {
            pts.pop();
        }
    }
    (pts.len() >= 2).then(|| poly(c, pts, None, closed))
}

/// Sequential reader over the body of a HATCH record.
struct Cur<'a> {
    p: &'a [Pair],
    i: usize,
}

impl Cur<'_> {
    /// Value of the next pair with `code`, looking at most a few pairs ahead.
    fn f(&mut self, code: i32) -> Option<f64> {
        for k in self.i..(self.i + 6).min(self.p.len()) {
            if self.p[k].code == code {
                self.i = k + 1;
                return Some(f(&self.p[k]));
            }
        }
        None
    }
    fn n(&mut self, code: i32) -> usize {
        self.f(code).map_or(0, |v| v.max(0.0) as usize).min(100_000)
    }
}

fn arc_pts(c: (f64, f64), r: f64, s: f64, e: f64, ccw: bool) -> Vec<(f64, f64)> {
    let (s, mut e) = (s.to_radians(), e.to_radians());
    while e <= s {
        e += TAU;
    }
    let k = steps(e - s, r, f64::INFINITY);
    let sign = if ccw { 1.0 } else { -1.0 };
    (0..=k)
        .map(|j| {
            let t = sign * (s + (e - s) * j as f64 / k as f64);
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect()
}

fn bulge_pts(a: (f64, f64), b: (f64, f64), bulge: f64) -> Vec<(f64, f64)> {
    let th = 4.0 * bulge.atan();
    let ch = (b.0 - a.0).hypot(b.1 - a.1);
    if ch <= 0.0 || th.abs() < 1e-12 {
        return vec![];
    }
    let r = ch / (2.0 * (th / 2.0).sin().abs());
    let h = r * (th / 2.0).cos() * th.signum();
    let (nx, ny) = (-(b.1 - a.1) / ch, (b.0 - a.0) / ch);
    let c = ((a.0 + b.0) / 2.0 + nx * h, (a.1 + b.1) / 2.0 + ny * h);
    let a0 = (a.1 - c.1).atan2(a.0 - c.0);
    let k = steps(th, r, f64::INFINITY);
    (1..k)
        .map(|j| {
            let t = a0 + th * j as f64 / k as f64;
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect()
}

fn hatch_loop(cur: &mut Cur) -> Option<Vec<(f64, f64)>> {
    let flags = cur.f(92)? as i64;
    let mut pts: Vec<(f64, f64)> = vec![];
    if flags & 2 != 0 {
        let has_bulge = cur.f(72).unwrap_or(0.0) != 0.0;
        let _closed = cur.f(73);
        let n = cur.n(93);
        let mut raw: Vec<((f64, f64), f64)> = vec![];
        for _ in 0..n {
            let x = cur.f(10)?;
            let y = cur.f(20)?;
            let b = if has_bulge {
                cur.f(42).unwrap_or(0.0)
            } else {
                0.0
            };
            raw.push(((x, y), b));
        }
        for k in 0..raw.len() {
            let (a, b) = (raw[k], raw[(k + 1) % raw.len()]);
            pts.push(a.0);
            if a.1 != 0.0 {
                pts.extend(bulge_pts(a.0, b.0, a.1));
            }
        }
    } else {
        let n = cur.n(93);
        for _ in 0..n {
            let ty = cur.f(72)? as i64;
            let seg: Vec<(f64, f64)> = match ty {
                1 => {
                    let a = (cur.f(10)?, cur.f(20)?);
                    let b = (cur.f(11)?, cur.f(21)?);
                    vec![a, b]
                }
                2 => {
                    let c = (cur.f(10)?, cur.f(20)?);
                    let r = cur.f(40)?;
                    let (s, e) = (cur.f(50)?, cur.f(51)?);
                    let ccw = cur.f(73).unwrap_or(1.0) != 0.0;
                    arc_pts(c, r, s, e, ccw)
                }
                3 => {
                    let c = (cur.f(10)?, cur.f(20)?);
                    let m = (cur.f(11)?, cur.f(21)?);
                    let ratio = cur.f(40)?;
                    let (s, e) = (cur.f(50)?, cur.f(51)?);
                    let ccw = cur.f(73).unwrap_or(1.0) != 0.0;
                    let r = m.0.hypot(m.1);
                    let minor = (-m.1 * ratio, m.0 * ratio);
                    arc_pts((0.0, 0.0), 1.0, s, e, ccw)
                        .into_iter()
                        .map(|(u, v)| (c.0 + m.0 * u + minor.0 * v, c.1 + m.1 * u + minor.1 * v))
                        .filter(|_| r > 0.0)
                        .collect()
                }
                4 => {
                    let degree = cur.f(94)? as usize;
                    let _rational = cur.f(73);
                    let _periodic = cur.f(74);
                    let nk = cur.n(95);
                    let nc = cur.n(96);
                    let mut knots = vec![];
                    for _ in 0..nk {
                        knots.push(cur.f(40)?);
                    }
                    let mut ctrl = vec![];
                    let mut weights = vec![];
                    for _ in 0..nc {
                        ctrl.push((cur.f(10)?, cur.f(20)?));
                        if let Some(w) = (cur.p.get(cur.i).is_some_and(|p| p.code == 42))
                            .then(|| cur.f(42))
                            .flatten()
                        {
                            weights.push(w);
                        }
                    }
                    if cur.p.get(cur.i).is_some_and(|p| p.code == 97) {
                        let nf = cur.n(97);
                        for _ in 0..nf {
                            cur.f(11);
                            cur.f(21);
                        }
                        for c in [12, 22, 13, 23] {
                            if cur.p.get(cur.i).is_some_and(|p| p.code == c) {
                                cur.f(c);
                            }
                        }
                    }
                    let tol = (bbox_diag(&ctrl) * 5e-4).max(1e-9);
                    Nurbs::new(degree, knots, ctrl, weights).map_or(vec![], |s| s.points(tol))
                }
                _ => return None,
            };
            // edges chain end to start: drop a repeated joint
            let mut it = seg.into_iter();
            if let (Some(first), Some(&last)) = (it.next(), pts.last()) {
                if (first.0 - last.0).hypot(first.1 - last.1) > 1e-9 {
                    pts.push(first);
                }
            } else if let Some(first) = pts.is_empty().then_some(()).and(None::<(f64, f64)>) {
                pts.push(first);
            }
            pts.extend(it);
        }
        // first point of the very first edge was skipped by the chaining above when `pts` was empty
        if let (Some(&a), Some(&b)) = (pts.first(), pts.last()) {
            if pts.len() > 2 && (a.0 - b.0).hypot(a.1 - b.1) <= 1e-9 {
                pts.pop();
            }
        }
    }
    // source boundary objects
    if cur.p.get(cur.i).is_some_and(|p| p.code == 97) {
        let n = cur.n(97);
        for _ in 0..n {
            if cur.p.get(cur.i).is_some_and(|p| p.code == 330) {
                cur.i += 1;
            }
        }
    }
    (pts.len() >= 3).then_some(pts)
}

/// HATCH: boundary loops, solid flag, pattern lines (as stored in the entity).
pub fn hatch(rec: &[Pair]) -> Option<Fill> {
    let body = &rec[1..];
    let start = body.iter().position(|p| p.code == 91)?;
    let solid = body[..start]
        .iter()
        .find(|p| p.code == 70)
        .is_some_and(|p| i(p) == 1);
    let pattern = body[..start]
        .iter()
        .find(|p| p.code == 2)
        .map_or(String::new(), |p| p.value.trim().to_string());
    let mut cur = Cur { p: body, i: start };
    let nloops = cur.n(91);
    let mut loops = vec![];
    for _ in 0..nloops {
        let save = cur.i;
        match hatch_loop(&mut cur) {
            Some(l) => loops.push(l),
            None => {
                // skip to the next loop marker
                cur.i = save + 1;
                while cur.i < body.len() && body[cur.i].code != 92 && body[cur.i].code != 75 {
                    cur.i += 1;
                }
            }
        }
    }
    let mut lines = vec![];
    if !solid {
        if let Some(k) = body[cur.i..].iter().position(|p| p.code == 78) {
            cur.i += k;
            let n = cur.n(78);
            for _ in 0..n {
                let Some(angle) = cur.f(53) else { break };
                let base = (cur.f(43).unwrap_or(0.0), cur.f(44).unwrap_or(0.0));
                let offset = (cur.f(45).unwrap_or(0.0), cur.f(46).unwrap_or(0.0));
                let nd = cur.n(79);
                let mut dashes = vec![];
                for _ in 0..nd {
                    dashes.extend(cur.f(49));
                }
                lines.push(PatLine {
                    angle,
                    base,
                    offset,
                    dashes,
                });
            }
        }
    }
    (!loops.is_empty()).then(|| Fill {
        common: common_of(rec, false),
        loops,
        solid,
        pattern,
        lines,
        segs: vec![],
    })
}

fn solid(rec: &[Pair]) -> Option<Fill> {
    let p = |k: i32| (getf(rec, 10 + k, f64::NAN), getf(rec, 20 + k, f64::NAN));
    let (a, b, c) = (p(0), p(1), p(2));
    let d = get(rec, 13).map_or(c, |_| p(3));
    let pts = vec![a, b, d, c];
    if pts.iter().any(|q| !q.0.is_finite() || !q.1.is_finite()) {
        return None;
    }
    let mut ring: Vec<(f64, f64)> = vec![];
    for q in pts {
        if ring
            .last()
            .is_none_or(|l: &(f64, f64)| (l.0 - q.0).hypot(l.1 - q.1) > 1e-12)
        {
            ring.push(q);
        }
    }
    (ring.len() >= 3).then(|| Fill {
        common: common_of(rec, false),
        loops: vec![ring],
        solid: true,
        pattern: "SOLID".into(),
        lines: vec![],
        segs: vec![],
    })
}

/// HATCH record with polyline boundary loops (no bulges) — what the 2D model tables give back.
pub fn hatch_record(c: &Common, loops: &[Vec<(f64, f64)>], pattern: &str) -> Vec<Pair> {
    let solid = pattern.eq_ignore_ascii_case("SOLID") || pattern.is_empty();
    let mut v = vec![
        Pair::new(0, "HATCH"),
        Pair::new(100, "AcDbEntity"),
        Pair::new(8, c.layer.clone()),
    ];
    if let Some(x) = c.color {
        v.push(Pair::new(62, x.to_string()));
    }
    if let Some(x) = c.true_color {
        v.push(Pair::new(420, x.to_string()));
    }
    let num = |x: f64| format!("{x}");
    v.extend([
        Pair::new(100, "AcDbHatch"),
        Pair::new(10, "0.0"),
        Pair::new(20, "0.0"),
        Pair::new(30, "0.0"),
        Pair::new(210, "0.0"),
        Pair::new(220, "0.0"),
        Pair::new(230, "1.0"),
        Pair::new(
            2,
            if solid {
                "SOLID".to_string()
            } else {
                pattern.to_string()
            },
        ),
        Pair::new(70, if solid { "1" } else { "0" }),
        Pair::new(71, "0"),
        Pair::new(91, loops.len().to_string()),
    ]);
    for (k, l) in loops.iter().enumerate() {
        // outermost first, the rest are holes
        v.push(Pair::new(92, if k == 0 { "3" } else { "2" }));
        v.push(Pair::new(72, "0"));
        v.push(Pair::new(73, "1"));
        v.push(Pair::new(93, l.len().to_string()));
        for p in l {
            v.push(Pair::new(10, num(p.0)));
            v.push(Pair::new(20, num(p.1)));
        }
        v.push(Pair::new(97, "0"));
    }
    v.extend([Pair::new(75, "1"), Pair::new(76, "1")]);
    if !solid {
        v.extend([
            Pair::new(52, "0.0"),
            Pair::new(41, "1.0"),
            Pair::new(77, "0"),
            Pair::new(78, "0"),
        ]);
    }
    v.push(Pair::new(98, "0"));
    v
}

/// Signed area of a ring.
pub fn ring_area(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|k| {
            let (a, b) = (r[k], r[(k + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

/// Pattern line segments of `lines` clipped to the loops (even-odd); at most `cap` segments,
/// `None` when the pattern would be denser than that.
pub fn pattern_segments(
    loops: &[Vec<(f64, f64)>],
    lines: &[PatLine],
    cap: usize,
) -> Option<Vec<((f64, f64), (f64, f64))>> {
    let mut out = vec![];
    let pts = loops.iter().flatten();
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for p in pts {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    if lo.0 > hi.0 {
        return Some(out);
    }
    for pl in lines {
        let (s, c) = pl.angle.to_radians().sin_cos();
        let d = (c, s);
        let nrm = (-s, c);
        // spacing between parallel lines and the shift along them per line
        let step = pl.offset.0 * nrm.0 + pl.offset.1 * nrm.1;
        let shift = pl.offset.0 * d.0 + pl.offset.1 * d.1;
        if step.abs() < 1e-12 {
            continue;
        }
        let proj = |p: (f64, f64), v: (f64, f64)| (p.0 - pl.base.0) * v.0 + (p.1 - pl.base.1) * v.1;
        let corners = [lo, (hi.0, lo.1), hi, (lo.0, hi.1)];
        let (mut nmin, mut nmax) = (f64::MAX, f64::MIN);
        let (mut tmin, mut tmax) = (f64::MAX, f64::MIN);
        for q in corners {
            nmin = nmin.min(proj(q, nrm));
            nmax = nmax.max(proj(q, nrm));
            tmin = tmin.min(proj(q, d));
            tmax = tmax.max(proj(q, d));
        }
        let (k0, k1) = (
            (nmin / step).min(nmax / step).floor() as i64,
            (nmin / step).max(nmax / step).ceil() as i64,
        );
        if (k1 - k0) as usize > cap {
            return None;
        }
        let period: f64 = pl.dashes.iter().map(|x| x.abs()).sum();
        for k in k0..=k1 {
            let o = (
                pl.base.0 + nrm.0 * step * k as f64,
                pl.base.1 + nrm.1 * step * k as f64,
            );
            // line through o along d: crossings with loop edges
            let mut ts: Vec<f64> = vec![];
            for l in loops {
                let n = l.len();
                for e in 0..n {
                    let (a, b) = (l[e], l[(e + 1) % n]);
                    let (ea, eb) = (
                        (a.0 - o.0) * nrm.0 + (a.1 - o.1) * nrm.1,
                        (b.0 - o.0) * nrm.0 + (b.1 - o.1) * nrm.1,
                    );
                    if (ea > 0.0) != (eb > 0.0) {
                        let u = ea / (ea - eb);
                        let p = (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u);
                        ts.push((p.0 - o.0) * d.0 + (p.1 - o.1) * d.1);
                    }
                }
            }
            ts.sort_by(f64::total_cmp);
            let phase = shift * k as f64;
            for w in ts.chunks(2) {
                let [t0, t1] = w else { continue };
                let at = |t: f64| (o.0 + d.0 * t, o.1 + d.1 * t);
                if pl.dashes.is_empty() || period <= 1e-12 {
                    out.push((at(*t0), at(*t1)));
                } else {
                    // dash pattern starts at the line origin shifted by k·shift
                    let mut t = phase + ((t0 - phase) / period).floor() * period;
                    let mut guard = 0;
                    while t < *t1 && guard < 100_000 {
                        for &dl in &pl.dashes {
                            let len = dl.abs();
                            let (a, b) = (t.max(*t0), (t + len).min(*t1));
                            if dl >= 0.0 && b >= a && t + len >= *t0 {
                                let b = if dl == 0.0 { a } else { b };
                                if dl > 0.0 || a <= *t1 {
                                    out.push((at(a), at(b)));
                                }
                            }
                            t += len;
                            if t >= *t1 {
                                break;
                            }
                        }
                        guard += 1;
                    }
                }
                if out.len() > cap {
                    return None;
                }
            }
        }
        let _ = (tmin, tmax);
    }
    Some(out)
}

/// A whole circle as a closed loop (used for round columns and dots).
pub fn circle_ring(c: (f64, f64), r: f64) -> Vec<(f64, f64)> {
    let k = steps(TAU, r, f64::INFINITY);
    (0..k)
        .map(|j| {
            let t = TAU * j as f64 / k as f64;
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, pairs: &[(i32, &str)]) -> Entity {
        let mut v = vec![Pair::new(0, kind)];
        v.extend(pairs.iter().map(|(c, s)| Pair::new(*c, *s)));
        Entity::Unknown(v)
    }

    fn one(e: Vec<Entity>) -> Vec<Lowered> {
        lower_list(&e, false, &mut LowerReport::default())
    }

    fn lw(l: &Lowered) -> &LwPolyline {
        match l {
            Lowered::Ent(Entity::LwPolyline(p)) => p,
            o => panic!("not a polyline: {o:?}"),
        }
    }

    #[test]
    fn old_polyline_sequence() {
        let ents = vec![
            rec("POLYLINE", &[(8, "W"), (66, "1"), (70, "1")]),
            rec("VERTEX", &[(10, "0"), (20, "0"), (42, "1")]),
            rec("VERTEX", &[(10, "2"), (20, "0")]),
            rec("VERTEX", &[(10, "2"), (20, "3")]),
            rec("SEQEND", &[]),
            rec("POLYLINE", &[(8, "P"), (70, "8")]),
            rec("VERTEX", &[(10, "1"), (20, "1"), (30, "5")]),
            rec("VERTEX", &[(10, "4"), (20, "5"), (30, "6")]),
            rec("SEQEND", &[]),
        ];
        let r = one(ents);
        assert_eq!(r.len(), 2);
        let a = lw(&r[0]);
        assert!(a.closed());
        assert_eq!(a.common.layer, "W");
        assert_eq!(a.vertices.len(), 3);
        assert_eq!(a.vertices[0].bulge, 1.0);
        let b = lw(&r[1]);
        assert_eq!((b.vertices[1].x, b.vertices[1].y), (4.0, 5.0));
        assert!(!b.closed());
    }

    #[test]
    fn ellipse_on_curve_and_closed() {
        let r = one(vec![rec(
            "ELLIPSE",
            &[
                (10, "1"),
                (20, "2"),
                (11, "3"),
                (21, "0"),
                (40, "0.5"),
                (41, "0"),
                (42, "6.283185307179586"),
            ],
        )]);
        let p = lw(&r[0]);
        assert!(p.closed());
        for v in &p.vertices {
            let (u, w) = ((v.x - 1.0) / 3.0, (v.y - 2.0) / 1.5);
            assert!((u * u + w * w - 1.0).abs() < 1e-9);
        }
        // sag within 1e-3 of the major radius
        assert!(p.vertices.len() >= 60);
        // half ellipse, open, ends exact
        let r = one(vec![rec(
            "ELLIPSE",
            &[
                (10, "0"),
                (20, "0"),
                (11, "2"),
                (21, "0"),
                (40, "0.5"),
                (41, "0"),
                (42, "3.141592653589793"),
            ],
        )]);
        let p = lw(&r[0]);
        assert!(!p.closed());
        let l = p.vertices.last().unwrap();
        assert!((l.x + 2.0).abs() < 1e-9 && l.y.abs() < 1e-9);
    }

    #[test]
    fn spline_interpolates_quadratic_exactly() {
        // clamped quadratic Bezier (0,0) (1,2) (2,0): y = 4t(1-t), x = 2t
        let r = one(vec![rec(
            "SPLINE",
            &[
                (71, "2"),
                (40, "0"),
                (40, "0"),
                (40, "0"),
                (40, "1"),
                (40, "1"),
                (40, "1"),
                (10, "0"),
                (20, "0"),
                (10, "1"),
                (20, "2"),
                (10, "2"),
                (20, "0"),
            ],
        )]);
        let p = lw(&r[0]);
        for v in &p.vertices {
            let t = v.x / 2.0;
            assert!((v.y - 4.0 * t * (1.0 - t)).abs() < 1e-9, "{} {}", v.x, v.y);
        }
        assert_eq!((p.vertices[0].x, p.vertices.last().unwrap().x), (0.0, 2.0));
        assert!(p.vertices.len() > 8);
        // rational quarter circle: weights 1, √2/2, 1
        let s = Nurbs::new(
            2,
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec![(1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            vec![1.0, std::f64::consts::FRAC_1_SQRT_2, 1.0],
        )
        .unwrap();
        for q in s.points(1e-6) {
            assert!((q.0.hypot(q.1) - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn hatch_edges_and_pattern() {
        let pairs: Vec<(i32, &str)> = vec![
            (8, "H"),
            (10, "0"),
            (20, "0"),
            (30, "0"),
            (2, "ANSI31"),
            (70, "0"),
            (71, "0"),
            (91, "2"),
            (92, "1"),
            (93, "4"),
            (72, "1"),
            (10, "0"),
            (20, "0"),
            (11, "4"),
            (21, "0"),
            (72, "1"),
            (10, "4"),
            (20, "0"),
            (11, "4"),
            (21, "4"),
            (72, "1"),
            (10, "4"),
            (20, "4"),
            (11, "0"),
            (21, "4"),
            (72, "1"),
            (10, "0"),
            (20, "4"),
            (11, "0"),
            (21, "0"),
            (97, "0"),
            (92, "2"),
            (72, "0"),
            (73, "1"),
            (93, "4"),
            (10, "1"),
            (20, "1"),
            (10, "1"),
            (20, "3"),
            (10, "3"),
            (20, "3"),
            (10, "3"),
            (20, "1"),
            (97, "0"),
            (75, "1"),
            (76, "1"),
            (52, "0"),
            (41, "1"),
            (77, "0"),
            (78, "1"),
            (53, "0"),
            (43, "0"),
            (44, "0"),
            (45, "0"),
            (46, "1"),
            (79, "0"),
            (98, "0"),
        ];
        let r = one(vec![rec("HATCH", &pairs)]);
        let Lowered::Fill(h) = &r[0] else { panic!() };
        assert_eq!(h.loops.len(), 2);
        assert_eq!(h.loops[0].len(), 4);
        assert!((ring_area(&h.loops[0]).abs() - 16.0).abs() < 1e-12);
        assert!(!h.solid);
        assert_eq!(h.lines.len(), 1);
        // horizontal lines y=1..3 cross the hole: two pieces each; y=0 and 4 lie on the edge
        let segs = pattern_segments(&h.loops, &h.lines, 10_000).unwrap();
        let total: f64 = segs.iter().map(|(a, b)| (b.0 - a.0).hypot(b.1 - a.1)).sum();
        // y=1 and y=3 run along hole edges (ambiguous), y=2 gives 1+1
        assert!(segs
            .iter()
            .any(|(a, b)| a.1 == 2.0 && (b.0 - a.0).abs() == 1.0));
        assert!(total > 0.0);
        let back = hatch(&hatch_record(&Common::on_layer("H"), &h.loops, "SOLID")).unwrap();
        assert!(back.solid);
        assert_eq!(back.loops, h.loops);
    }

    #[test]
    fn solid_dimension_leader_paper() {
        let ents = vec![
            rec(
                "SOLID",
                &[
                    (10, "0"),
                    (20, "0"),
                    (11, "1"),
                    (21, "0"),
                    (12, "0"),
                    (22, "1"),
                    (13, "1"),
                    (23, "1"),
                ],
            ),
            rec("DIMENSION", &[(8, "DIM"), (2, "*D1")]),
            rec(
                "LEADER",
                &[
                    (10, "0"),
                    (20, "0"),
                    (10, "1"),
                    (20, "1"),
                    (10, "2"),
                    (20, "1"),
                ],
            ),
            rec("LINE", &[(67, "1")]),
            rec("3DSOLID", &[]),
        ];
        let mut rep = LowerReport::default();
        let r = lower_list(&ents, false, &mut rep);
        assert_eq!(r.len(), 3);
        let Lowered::Fill(s) = &r[0] else { panic!() };
        // corner order 1,2,4,3 gives a square, not a bow tie
        assert!((ring_area(&s.loops[0]).abs() - 1.0).abs() < 1e-12);
        assert!(matches!(&r[1], Lowered::Ent(Entity::Insert(i)) if i.block == "*D1"));
        assert!(matches!(&r[2], Lowered::Ent(Entity::Unknown(p)) if p[0].value == "LEADER"));
        assert_eq!((rep.paper, rep.unsupported, rep.lowered), (1, 1, 3));
    }
}
