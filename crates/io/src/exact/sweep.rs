// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Exact bodies of the swept forms: sweep and swept blend along a path of lines and arcs, blend
//! between two profiles. Straight path pieces give ruled faces (planes where flat), planar arcs
//! surfaces of revolution, helical arcs and blends along arcs B-spline faces fitted to the exact
//! sweep within a tolerance. Mitred joints between straight pieces are kept; a mitre next to an
//! arc has no exact limit, so such a path is refused and the caller writes a polyhedron.

use std::f64::consts::PI;

use ddc_geom::arc::{BulgeRing, MIN_BULGE};
use ddc_geom::gsolid::{self, Frame, Path3, Section};
use ddc_geom::math::{v2, v3, V2, V3};
use ddc_geom::nurbs;

use super::build::{frame_xf, ring_segs, rings, Bld, Seg};
use super::{Ax3, Body, Curve, Surface};

/// Default largest deviation of a fitted face from the exact sweep, metres.
pub const FIT_TOL: f64 = 1e-5;
/// Most sections of one fitted piece.
const MAX_FIT: usize = 512;
/// Rotation-minimising frame steps per fitted section.
const RMF_STEPS: usize = 64;

fn ne(e: nurbs::NurbsError) -> String {
    format!("B-spline: {e}")
}

// ---------- profile pieces ----------

/// Profile edge at the start (`b`) and at the end (`t`) of the path; equal for a sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pair {
    b: Seg,
    t: Seg,
}

/// Curve of a profile edge in the local plane z = 0.
fn seg_curve(s: &Seg) -> Result<nurbs::Curve, String> {
    match s.arc() {
        None => nurbs::Curve::polyline(&[v3(s.a.x, s.a.y, 0.0), v3(s.b.x, s.b.y, 0.0)]).map_err(ne),
        Some((c, r, sw)) => {
            let xa = (s.a - c) * (1.0 / r);
            let ya = if sw > 0.0 { v2(-xa.y, xa.x) } else { v2(xa.y, -xa.x) };
            nurbs::Curve::arc(v3(c.x, c.y, 0.0), v3(xa.x, xa.y, 0.0), v3(ya.x, ya.y, 0.0), r, 0.0, sw.abs()).map_err(ne)
        }
    }
}

/// One ring of pairs with their curves: the section curve of pair `j` at blend parameter `lam`.
struct Prof {
    pairs: Vec<Pair>,
    fixed: Vec<nurbs::Curve>,
    ruled: Vec<Option<nurbs::Surface>>,
}

impl Prof {
    fn new(pairs: Vec<Pair>) -> Result<Prof, String> {
        let mut fixed = vec![];
        let mut ruled = vec![];
        for p in &pairs {
            let cb = seg_curve(&p.b)?;
            if p.b == p.t {
                ruled.push(None);
            } else {
                ruled.push(Some(nurbs::ruled(&cb, &seg_curve(&p.t)?).map_err(ne)?));
            }
            fixed.push(cb);
        }
        Ok(Prof { pairs, fixed, ruled })
    }
    fn varies(&self) -> bool {
        self.ruled.iter().any(Option::is_some)
    }
    fn section(&self, j: usize, lam: f64) -> Result<nurbs::Curve, String> {
        match &self.ruled[j] {
            None => Ok(self.fixed[j].clone()),
            Some(s) => iso(s, lam),
        }
    }
    fn start(&self, j: usize, lam: f64) -> V3 {
        let p = self.pairs[j];
        let q = p.b.a + (p.t.a - p.b.a) * lam;
        v3(q.x, q.y, 0.0)
    }
    fn both_lines(&self, j: usize) -> bool {
        self.pairs[j].b.arc().is_none() && self.pairs[j].t.arc().is_none()
    }
}

/// The curve v = `lam` of a ruled surface (two control rows).
fn iso(s: &nurbs::Surface, lam: f64) -> Result<nurbs::Curve, String> {
    let (c, w) = (s.ctrl(), s.weights());
    let mut ctrl = Vec::with_capacity(s.nu());
    let mut ws = Vec::with_capacity(s.nu());
    for i in 0..s.nu() {
        let (w0, w1) = (w[i * 2] * (1.0 - lam), w[i * 2 + 1] * lam);
        let ww = w0 + w1;
        ctrl.push((c[i * 2] * w0 + c[i * 2 + 1] * w1) * (1.0 / ww));
        ws.push(ww);
    }
    nurbs::Curve::new(s.deg_u(), s.knots_u().to_vec(), ctrl, ws).map_err(ne)
}

fn mapped(c: &nurbs::Curve, f: impl Fn(V3) -> V3) -> Result<nurbs::Curve, String> {
    nurbs::Curve::new(c.degree(), c.knots().to_vec(), c.ctrl().iter().map(|p| f(*p)).collect(), c.weights().to_vec()).map_err(ne)
}

// ---------- blend correspondence ----------

fn seg_len(s: &Seg) -> f64 {
    match s.arc() {
        None => (s.b - s.a).len(),
        Some((_, r, sw)) => r * sw.abs(),
    }
}

fn seg_at(s: &Seg, f: f64) -> V2 {
    match s.arc() {
        None => s.a + (s.b - s.a) * f,
        Some((c, r, sw)) => {
            let d = s.a - c;
            let a = d.y.atan2(d.x) + sw * f;
            c + v2(a.cos(), a.sin()) * r
        }
    }
}

/// The part of `s` between the length fractions `f0` < `f1`.
fn sub(s: &Seg, f0: f64, f1: f64) -> Seg {
    let a = if f0 <= 0.0 { s.a } else { seg_at(s, f0) };
    let b = if f1 >= 1.0 { s.b } else { seg_at(s, f1) };
    let bulge = match s.arc() {
        None => 0.0,
        Some((_, _, sw)) => (sw * (f1 - f0) / 4.0).tan(),
    };
    Seg { a, b, bulge }
}

fn ccw_from(r: &BulgeRing, first: V2) -> Vec<Seg> {
    let mut segs = ring_segs(r, true);
    let k = (0..segs.len())
        .min_by(|&i, &j| (segs[i].a - first).len().total_cmp(&(segs[j].a - first).len()))
        .unwrap_or(0);
    segs.rotate_left(k);
    segs
}

fn mean(r: &BulgeRing) -> V2 {
    let t = gsolid::tess(r, 1e-4);
    t.iter().fold(v2(0.0, 0.0), |s, p| s + *p) * (1.0 / t.len().max(1) as f64)
}

/// Corresponding pieces of two rings: both counter-clockwise from the first point of `bottom`
/// and the point of `top` lying the same way from its centre, split at the length fractions of
/// either ring.
fn correspond(bottom: &BulgeRing, top: &BulgeRing) -> Result<Vec<Pair>, String> {
    let a = ccw_from(bottom, bottom.pts[0]);
    let mut b = ring_segs(top, true);
    if a.is_empty() || b.is_empty() {
        return Err("empty blend ring".into());
    }
    let da = a[0].a - mean(bottom);
    let cb = mean(top);
    let score = |p: V2| {
        let d = p - cb;
        if d.len() < 1e-12 || da.len() < 1e-12 {
            f64::NEG_INFINITY
        } else {
            d.dot(da) / (d.len() * da.len())
        }
    };
    // best start: a vertex, or a point inside an arc (split there)
    let mut best = (0usize, 0.0f64, f64::NEG_INFINITY);
    for (k, s) in b.iter().enumerate() {
        let n = if s.arc().is_some() { 256 } else { 1 };
        for i in 0..n {
            let f = i as f64 / n as f64;
            let sc = score(seg_at(s, f));
            if sc > best.2 + 1e-12 {
                best = (k, f, sc);
            }
        }
    }
    let (k, f, _) = best;
    if f > 0.0 {
        let s = b[k];
        b.splice(k..=k, [sub(&s, 0.0, f), sub(&s, f, 1.0)]);
        b.rotate_left(k + 1);
    } else {
        b.rotate_left(k);
    }
    let fracs = |segs: &[Seg]| {
        let l: Vec<f64> = segs.iter().map(seg_len).collect();
        let tot: f64 = l.iter().sum::<f64>().max(1e-300);
        let mut acc = vec![0.0];
        for x in &l {
            acc.push(acc.last().copied().unwrap_or(0.0) + x / tot);
        }
        *acc.last_mut().expect("one entry") = 1.0;
        acc
    };
    let (fa, fb) = (fracs(&a), fracs(&b));
    let mut cuts: Vec<f64> = fa.iter().chain(&fb).copied().collect();
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let piece = |segs: &[Seg], fr: &[f64], t0: f64, t1: f64| {
        let m = (t0 + t1) / 2.0;
        let i = fr.partition_point(|&x| x <= m).saturating_sub(1).min(segs.len() - 1);
        let w = (fr[i + 1] - fr[i]).max(1e-300);
        let g0 = ((t0 - fr[i]) / w).clamp(0.0, 1.0);
        let g1 = ((t1 - fr[i]) / w).clamp(0.0, 1.0);
        let g0 = if g0 < 1e-9 { 0.0 } else { g0 };
        let g1 = if g1 > 1.0 - 1e-9 { 1.0 } else { g1 };
        sub(&segs[i], g0, g1)
    };
    Ok(cuts
        .windows(2)
        .map(|w| Pair {
            b: piece(&a, &fa, w[0], w[1]),
            t: piece(&b, &fb, w[0], w[1]),
        })
        .collect())
}

// ---------- path ----------

#[derive(Clone, Copy, Debug)]
enum Piece {
    Line { a: V3, b: V3 },
    Arc { c: V2, r: f64, th0: f64, sw: f64, z0: f64, dz: f64 },
}

impl Piece {
    fn at(&self, s: f64) -> V3 {
        match *self {
            Piece::Line { a, b } => a + (b - a) * s,
            Piece::Arc { c, r, th0, sw, z0, dz } => {
                let t = th0 + sw * s;
                v3(c.x + r * t.cos(), c.y + r * t.sin(), z0 + dz * s)
            }
        }
    }
    fn tan(&self, s: f64) -> V3 {
        match *self {
            Piece::Line { a, b } => (b - a).normalized(),
            Piece::Arc { r, th0, sw, dz, .. } => {
                let t = th0 + sw * s;
                v3(-r * sw * t.sin(), r * sw * t.cos(), dz).normalized()
            }
        }
    }
    fn len(&self) -> f64 {
        match *self {
            Piece::Line { a, b } => (b - a).len(),
            Piece::Arc { r, sw, dz, .. } => (r * sw).hypot(dz),
        }
    }
    fn curved(&self) -> bool {
        matches!(self, Piece::Arc { .. })
    }
    fn flat(&self) -> bool {
        match *self {
            Piece::Line { .. } => true,
            Piece::Arc { dz, .. } => dz.abs() <= 1e-9 * (1.0 + self.len()),
        }
    }
}

fn transport(x: V3, a: V3, b: V3) -> V3 {
    let k = a.cross(b);
    let s = k.len();
    let c = a.dot(b);
    if s < 1e-15 {
        return x;
    }
    let k = k * (1.0 / s);
    x * c + k.cross(x) * s + k * (k.dot(x) * (1.0 - c))
}

fn rot_z(x: V3, a: f64) -> V3 {
    let (c, s) = (a.cos(), a.sin());
    v3(x.x * c - x.y * s, x.x * s + x.y * c, x.z)
}

/// Rotation-minimising frame vector carried along the piece from `s0` to `s1` (double reflection).
fn rmf(p: &Piece, u: V3, s0: f64, s1: f64, steps: usize) -> V3 {
    if let Piece::Arc { sw, .. } = *p {
        if p.flat() {
            return rot_z(u, sw * (s1 - s0));
        }
    }
    let mut u = u;
    for k in 0..steps {
        let a = s0 + (s1 - s0) * k as f64 / steps as f64;
        let b = s0 + (s1 - s0) * (k + 1) as f64 / steps as f64;
        let (x0, x1, t0, t1) = (p.at(a), p.at(b), p.tan(a), p.tan(b));
        let v1 = x1 - x0;
        let c1 = v1.dot(v1);
        if c1 < 1e-300 {
            continue;
        }
        let rl = u - v1 * (2.0 / c1 * v1.dot(u));
        let tl = t0 - v1 * (2.0 / c1 * v1.dot(t0));
        let v2_ = t1 - tl;
        let c2 = v2_.dot(v2_);
        u = if c2 < 1e-300 { rl } else { rl - v2_ * (2.0 / c2 * v2_.dot(rl)) };
    }
    let t = p.tan(s1);
    (u - t * u.dot(t)).normalized()
}

/// Section plane at a path vertex: point, tangent of the incoming piece, plane normal (the
/// bisector at a mitre), profile axes of the incoming piece, blend parameter.
#[derive(Clone, Copy, Debug)]
struct Station {
    p: V3,
    t: V3,
    n: V3,
    u: V3,
    lam: f64,
}

impl Station {
    fn v(&self) -> V3 {
        self.t.cross(self.u)
    }
    /// Profile point (x, y) projected along the tangent onto the section plane.
    fn at(&self, q: V3) -> V3 {
        let w = self.u * q.x + self.v() * q.y;
        let k = -w.dot(self.n) / self.t.dot(self.n);
        self.p + w + self.t * k
    }
    fn normal(&self) -> bool {
        self.n.dot(self.t) > 1.0 - 1e-12
    }
}

struct Run {
    pieces: Vec<Piece>,
    /// Profile x axis at the start of each piece.
    u0: Vec<V3>,
    /// Stations: one per vertex (a closed path shares the first).
    st: Vec<Station>,
    /// Blend parameter at the start of each piece and its growth over the piece.
    lam: Vec<(f64, f64)>,
    closed: bool,
}

fn run(path: &Path3) -> Result<Run, String> {
    let n = path.pts.len();
    if n < 2 || path.bulges.len() + 1 != n {
        return Err("path needs two points and one bulge per segment".into());
    }
    let mut pts = path.pts.clone();
    let closed = (pts[n - 1] - pts[0]).len() < 1e-6;
    if closed {
        pts[n - 1] = pts[0];
    }
    let mut pieces = vec![];
    for i in 0..n - 1 {
        let (a, b) = (pts[i], pts[i + 1]);
        let bg = path.bulges[i];
        let s = Seg {
            a: v2(a.x, a.y),
            b: v2(b.x, b.y),
            bulge: bg,
        };
        pieces.push(match s.arc() {
            Some((c, r, sw)) if bg.abs() >= MIN_BULGE => {
                let d = s.a - c;
                Piece::Arc {
                    c,
                    r,
                    th0: d.y.atan2(d.x),
                    sw,
                    z0: a.z,
                    dz: b.z - a.z,
                }
            }
            _ => Piece::Line { a, b },
        });
    }
    let np = pieces.len();
    let corner = |a: V3, b: V3| a.cross(b).len() > 1e-6 || a.dot(b) < 0.0;
    let t_first = pieces[0].tan(0.0);
    let mut u = v3(0.0, 0.0, 1.0).cross(t_first);
    if u.len() < 1e-9 {
        u = v3(1.0, 0.0, 0.0);
    }
    let mut u = u.normalized();
    let mut u0 = Vec::with_capacity(np);
    let mut u1 = Vec::with_capacity(np);
    for i in 0..np {
        if i > 0 {
            let (tin, tout) = (pieces[i - 1].tan(1.0), pieces[i].tan(0.0));
            if corner(tin, tout) && (pieces[i - 1].curved() || pieces[i].curved()) {
                return Err(format!("path vertex {i}: a corner next to an arc"));
            }
            u = transport(u, tin, tout);
            u = (u - tout * u.dot(tout)).normalized();
        }
        u0.push(u);
        u = rmf(&pieces[i], u, 0.0, 1.0, 4096);
        u1.push(u);
    }
    let lens: Vec<f64> = pieces.iter().map(Piece::len).collect();
    let total: f64 = lens.iter().sum::<f64>().max(1e-300);
    let mut lam = vec![];
    let mut s = 0.0;
    for l in &lens {
        lam.push((s / total, l / total));
        s += l;
    }
    let joint = |i: usize, k: usize, l: f64| {
        // vertex between piece i (incoming) and piece k (outgoing)
        let (tin, tout) = (pieces[i].tan(1.0), pieces[k].tan(0.0));
        Station {
            p: pts[k],
            t: tin,
            n: (tin + tout).normalized(),
            u: u1[i],
            lam: l,
        }
    };
    let mut st = vec![];
    if closed {
        let (tin, tout) = (pieces[np - 1].tan(1.0), t_first);
        if corner(tin, tout) && (pieces[np - 1].curved() || pieces[0].curved()) {
            return Err("path start: a corner next to an arc".into());
        }
        let back = transport(u1[np - 1], tin, tout);
        if (back - u0[0]).len() > 1e-6 {
            return Err("closed path out of one plane".into());
        }
        st.push(joint(np - 1, 0, 0.0));
    } else {
        st.push(Station {
            p: pts[0],
            t: t_first,
            n: t_first,
            u: u0[0],
            lam: 0.0,
        });
    }
    for i in 1..np {
        st.push(joint(i - 1, i, lam[i].0));
    }
    if !closed {
        let t = pieces[np - 1].tan(1.0);
        st.push(Station {
            p: pts[np - 1 + 1],
            t,
            n: t,
            u: u1[np - 1],
            lam: 1.0,
        });
    }
    Ok(Run {
        pieces,
        u0,
        st,
        lam,
        closed,
    })
}

// ---------- fitting ----------

fn find_span(knots: &[f64], p: usize, n_ctrl: usize, t: f64) -> usize {
    let n = n_ctrl - 1;
    if t >= knots[n + 1] {
        return n;
    }
    if t <= knots[p] {
        return p;
    }
    let (mut lo, mut hi) = (p, n + 1);
    let mut mid = (lo + hi) / 2;
    while t < knots[mid] || t >= knots[mid + 1] {
        if t < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
        mid = (lo + hi) / 2;
    }
    mid
}

fn basis(knots: &[f64], p: usize, sp: usize, t: f64) -> Vec<f64> {
    let mut n = vec![0.0; p + 1];
    let (mut left, mut right) = (vec![0.0; p + 1], vec![0.0; p + 1]);
    n[0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[sp + 1 - j];
        right[j] = knots[sp + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let tmp = n[r] / (right[r + 1] + left[j - r]);
            n[r] = saved + right[r + 1] * tmp;
            saved = left[j - r] * tmp;
        }
        n[j] = saved;
    }
    n
}

/// Interpolation matrix of a degree `p` B-spline through uniform parameters, LU factorised.
struct Interp {
    p: usize,
    knots: Vec<f64>,
    lu: Vec<Vec<f64>>,
    perm: Vec<usize>,
}

impl Interp {
    fn new(m: usize) -> Result<Interp, String> {
        let p = 3.min(m - 1);
        let tau: Vec<f64> = (0..m).map(|k| k as f64 / (m - 1) as f64).collect();
        let mut knots = vec![0.0; p + 1];
        for j in 1..m - p {
            knots.push(tau[j..j + p].iter().sum::<f64>() / p as f64);
        }
        knots.extend(std::iter::repeat(1.0).take(p + 1));
        let mut a = vec![vec![0.0; m]; m];
        for (k, &t) in tau.iter().enumerate() {
            let sp = find_span(&knots, p, m, t);
            for (i, b) in basis(&knots, p, sp, t).into_iter().enumerate() {
                a[k][sp - p + i] = b;
            }
        }
        let mut perm: Vec<usize> = (0..m).collect();
        for c in 0..m {
            let r = (c..m).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs())).unwrap_or(c);
            if a[r][c].abs() < 1e-14 {
                return Err("singular interpolation".into());
            }
            a.swap(c, r);
            perm.swap(c, r);
            for r2 in c + 1..m {
                let f = a[r2][c] / a[c][c];
                a[r2][c] = f;
                for k in c + 1..m {
                    a[r2][k] -= f * a[c][k];
                }
            }
        }
        Ok(Interp { p, knots, lu: a, perm })
    }
    fn solve(&self, rhs: &[[f64; 4]]) -> Vec<[f64; 4]> {
        let m = rhs.len();
        let mut x: Vec<[f64; 4]> = self.perm.iter().map(|&i| rhs[i]).collect();
        for r in 0..m {
            for c in 0..r {
                let f = self.lu[r][c];
                for d in 0..4 {
                    x[r][d] -= f * x[c][d];
                }
            }
        }
        for r in (0..m).rev() {
            for c in r + 1..m {
                let f = self.lu[r][c];
                for d in 0..4 {
                    x[r][d] -= f * x[c][d];
                }
            }
            for d in 0..4 {
                x[r][d] /= self.lu[r][r];
            }
        }
        x
    }
}

/// Distance from `q` to the curve, by Newton from the parameter `u0`.
fn dist_to(c: &nurbs::Curve, q: V3, u0: f64) -> f64 {
    let (a, b) = c.domain();
    let mut u = u0.clamp(a, b);
    for _ in 0..16 {
        let d = c.derivs(u, 2);
        let r = d[0] - q;
        let f = r.dot(d[1]);
        let fp = d[1].dot(d[1]) + r.dot(d[2]);
        if fp.abs() < 1e-300 {
            break;
        }
        let nu = (u - f / fp).clamp(a, b);
        let done = (nu - u).abs() < 1e-15;
        u = nu;
        if done {
            break;
        }
    }
    (c.point(u) - q).len()
}

/// Faces of one curved piece fitted through sections; per ring and pair: surface and the
/// curve of the pair's first vertex along the piece.
type Fitted = Vec<Vec<(nurbs::Surface, nurbs::Curve)>>;

fn fit(piece: &Piece, u_start: V3, lam: (f64, f64), profs: &[Prof], tol: f64) -> Result<Fitted, String> {
    let ang = match *piece {
        Piece::Arc { sw, .. } => sw.abs(),
        Piece::Line { .. } => 0.0,
    };
    let mut n = ((ang / (PI / 8.0)).ceil() as usize).max(4);
    loop {
        let mut us = vec![u_start];
        for k in 1..=n {
            let prev = us[k - 1];
            us.push(rmf(piece, prev, (k - 1) as f64 / n as f64, k as f64 / n as f64, RMF_STEPS));
        }
        let place = |tau: f64, u: V3, q: V3| {
            let t = piece.tan(tau);
            piece.at(tau) + u * q.x + t.cross(u) * q.y
        };
        let lam_at = |tau: f64| lam.0 + lam.1 * tau;
        let ip = Interp::new(n + 1)?;
        let mut out: Fitted = vec![];
        let mut worst = 0.0f64;
        for pr in profs {
            let mut ring = vec![];
            for j in 0..pr.pairs.len() {
                let secs: Vec<nurbs::Curve> = (0..=n)
                    .map(|k| pr.section(j, lam_at(k as f64 / n as f64)))
                    .collect::<Result<_, _>>()?;
                let c0 = &secs[0];
                let nu = c0.ctrl().len();
                let mut ctrl = vec![V3::default(); nu * (n + 1)];
                let mut ws = vec![0.0; nu * (n + 1)];
                let mut first: Option<(Vec<V3>, Vec<f64>)> = None;
                for i in 0..nu {
                    let rhs: Vec<[f64; 4]> = (0..=n)
                        .map(|k| {
                            let tau = k as f64 / n as f64;
                            let p = place(tau, us[k], secs[k].ctrl()[i]);
                            let w = secs[k].weights()[i];
                            [p.x * w, p.y * w, p.z * w, w]
                        })
                        .collect();
                    let sol = ip.solve(&rhs);
                    let mut col = (vec![], vec![]);
                    for (k, h) in sol.iter().enumerate() {
                        if !(h[3] > 0.0) {
                            return Err("fitted weight not positive".into());
                        }
                        let p = v3(h[0] / h[3], h[1] / h[3], h[2] / h[3]);
                        ctrl[i * (n + 1) + k] = p;
                        ws[i * (n + 1) + k] = h[3];
                        col.0.push(p);
                        col.1.push(h[3]);
                    }
                    if i == 0 {
                        first = Some(col);
                    }
                }
                let srf = nurbs::Surface::new(c0.degree(), ip.p, c0.knots().to_vec(), ip.knots.clone(), nu, n + 1, ctrl, ws)
                    .map_err(ne)?;
                let (fc, fw) = first.ok_or("empty section")?;
                let track = nurbs::Curve::new(ip.p, ip.knots.clone(), fc, fw).map_err(ne)?;
                // deviation: midway between sections, against the exact section at that point
                let (d0, d1) = c0.domain();
                for k in 0..n {
                    let v = (k as f64 + 0.5) / n as f64;
                    for s in 0..=4 {
                        let u = d0 + (d1 - d0) * s as f64 / 4.0;
                        let x = srf.point(u, v);
                        let g = |tau: f64| (x - piece.at(tau)).dot(piece.tan(tau));
                        let mut tau = v;
                        for _ in 0..8 {
                            let h = 1e-6;
                            let dg = (g(tau + h) - g(tau - h)) / (2.0 * h);
                            if dg.abs() < 1e-300 {
                                break;
                            }
                            tau = (tau - g(tau) / dg).clamp(0.0, 1.0);
                        }
                        let k0 = ((tau * n as f64).floor() as usize).min(n - 1);
                        let uu = rmf(piece, us[k0], k0 as f64 / n as f64, tau, 16);
                        let rel = x - piece.at(tau);
                        let t = piece.tan(tau);
                        let q = v3(rel.dot(uu), rel.dot(t.cross(uu)), 0.0);
                        let off = rel.dot(t).abs();
                        let exact = pr.section(j, lam_at(tau))?;
                        worst = worst.max(dist_to(&exact, q, u).hypot(off));
                    }
                }
                ring.push((srf, track));
            }
            out.push(ring);
        }
        if worst <= tol {
            return Ok(out);
        }
        if n * 2 > MAX_FIT {
            return Err(format!("a swept face deviates {worst:.2e} m with {n} sections"));
        }
        n *= 2;
    }
}

// ---------- body ----------

/// Ruled face between two curves: a plane when both are coplanar lines.
fn ruled_face(a: &nurbs::Curve, b: &nurbs::Curve) -> Result<(Surface, bool), String> {
    if a.degree() == 1 && a.ctrl().len() == 2 && b.degree() == 1 && b.ctrl().len() == 2 {
        let (p0, p1, q0, q1) = (a.ctrl()[0], a.ctrl()[1], b.ctrl()[0], b.ctrl()[1]);
        let n = (p1 - p0).cross(q0 - p0);
        let size = (p1 - p0).len().max((q0 - p0).len()).max((q1 - p0).len());
        if n.len() > 1e-12 * size * size && (q1 - p0).dot(n.normalized()).abs() <= 1e-12 * (1.0 + size) {
            return Ok((Surface::Plane(Ax3::new(p0, n, p1 - p0)), true));
        }
    }
    Ok((Surface::Nurbs(nurbs::ruled(a, b).map_err(ne)?), true))
}

fn swept(profs: Vec<Prof>, path: &Path3, fr: &Frame, tol: f64) -> Result<Body, String> {
    let r = run(path)?;
    let varies = profs.iter().any(Prof::varies);
    if varies && r.closed {
        return Err("a swept blend along a closed path".into());
    }
    let ns = r.st.len();
    let mut k = Bld::default();
    // vertices and section edges at every station
    let mut vid: Vec<Vec<Vec<u32>>> = vec![];
    let mut sec: Vec<Vec<Vec<u32>>> = vec![];
    for st in &r.st {
        let mut vs = vec![];
        let mut es = vec![];
        for pr in &profs {
            let n = pr.pairs.len();
            let v: Vec<u32> = (0..n).map(|j| k.v(st.at(pr.start(j, st.lam)))).collect();
            let mut e = vec![];
            for j in 0..n {
                let (a, b) = (v[j], v[(j + 1) % n]);
                let fixed_arc = if pr.ruled[j].is_none() { pr.pairs[j].b.arc() } else { None };
                e.push(if pr.both_lines(j) {
                    k.edge([a, b], Curve::Line)
                } else if let (Some((c, _, sw)), true) = (fixed_arc, st.normal()) {
                    let nrm = if sw > 0.0 { st.t } else { -st.t };
                    k.arc(a, b, st.at(v3(c.x, c.y, 0.0)), nrm)
                } else {
                    let c = mapped(&pr.section(j, st.lam)?, |q| st.at(q))?;
                    k.edge([a, b], Curve::Nurbs(c))
                });
            }
            vs.push(v);
            es.push(e);
        }
        vid.push(vs);
        sec.push(es);
    }
    let mut faces = vec![];
    for (i, piece) in r.pieces.iter().enumerate() {
        let (ia, ib) = (i, (i + 1) % ns);
        let (sa, sb) = (r.st[ia], r.st[ib]);
        let revolve = piece.curved() && piece.flat() && !varies;
        let fitted = if piece.curved() && !revolve {
            Some(fit(piece, r.u0[i], r.lam[i], &profs, tol)?)
        } else {
            None
        };
        for (ri, pr) in profs.iter().enumerate() {
            let n = pr.pairs.len();
            let lon: Vec<u32> = (0..n)
                .map(|j| {
                    let (a, b) = (vid[ia][ri][j], vid[ib][ri][j]);
                    match (piece, &fitted) {
                        (Piece::Line { .. }, _) => k.edge([a, b], Curve::Line),
                        (Piece::Arc { c, sw, .. }, None) => {
                            let p = k.b.verts[a as usize];
                            let nrm = if *sw > 0.0 { v3(0.0, 0.0, 1.0) } else { v3(0.0, 0.0, -1.0) };
                            k.arc(a, b, v3(c.x, c.y, p.z), nrm)
                        }
                        (_, Some(f)) => k.edge([a, b], Curve::Nurbs(f[ri][j].1.clone())),
                    }
                })
                .collect();
            for j in 0..n {
                let (surface, sense) = match (piece, &fitted) {
                    (_, Some(f)) => (Surface::Nurbs(f[ri][j].0.clone()), true),
                    (Piece::Line { .. }, None) => {
                        let ca = mapped(&pr.section(j, sa.lam)?, |q| sa.at(q))?;
                        let cb = mapped(&pr.section(j, sb.lam)?, |q| sb.at(q))?;
                        ruled_face(&ca, &cb)?
                    }
                    (Piece::Arc { c, sw, .. }, None) => {
                        let ca = mapped(&pr.section(j, sa.lam)?, |q| sa.at(q))?;
                        let axis = if *sw > 0.0 { v3(0.0, 0.0, 1.0) } else { v3(0.0, 0.0, -1.0) };
                        let s = nurbs::revolve(&ca, v3(c.x, c.y, 0.0), axis, sw.abs()).map_err(ne)?;
                        (Surface::Nurbs(s), false)
                    }
                };
                let l = vec![(sec[ia][ri][j], true), (lon[(j + 1) % n], true), (sec[ib][ri][j], false), (lon[j], false)];
                faces.push(k.face(surface, sense, vec![l]));
            }
        }
    }
    if !r.closed {
        let (s0, s1) = (r.st[0], r.st[ns - 1]);
        let start: Vec<Vec<(u32, bool)>> = sec[0].iter().map(|e| e.iter().rev().map(|&x| (x, false)).collect()).collect();
        let end: Vec<Vec<(u32, bool)>> = sec[ns - 1].iter().map(|e| e.iter().map(|&x| (x, true)).collect()).collect();
        faces.push(k.face(Surface::Plane(Ax3::new(s0.p, -s0.t, s0.u)), true, start));
        faces.push(k.face(Surface::Plane(Ax3::new(s1.p, s1.t, s1.u)), true, end));
    }
    k.b.shells = vec![faces];
    k.b.closed = true;
    k.b.check()?;
    k.b.transformed(&frame_xf(fr))
}

/// Profile along the path: ruled faces on lines, surfaces of revolution on flat arcs, fitted
/// B-spline faces on helical arcs.
pub fn sweep(profile: &Section, path: &Path3, fr: &Frame, tol: f64) -> Result<Body, String> {
    let profs = rings(profile)
        .into_iter()
        .map(|r| Prof::new(r.into_iter().map(|s| Pair { b: s, t: s }).collect()))
        .collect::<Result<Vec<_>, _>>()?;
    swept(profs, path, fr, tol)
}

/// Ring changing from `bottom` to `top` along the path by its length.
pub fn swept_blend(bottom: &BulgeRing, top: &BulgeRing, path: &Path3, fr: &Frame, tol: f64) -> Result<Body, String> {
    let prof = Prof::new(correspond(bottom, top)?)?;
    swept(vec![prof], path, fr, tol)
}

/// Ruled faces between `bottom` at z = 0 and `top` at z = `height`.
pub fn blend(bottom: &BulgeRing, top: &BulgeRing, height: f64, fr: &Frame, tol: f64) -> Result<Body, String> {
    let path = Path3::polyline(vec![v3(0.0, 0.0, 0.0), v3(0.0, 0.0, height)]);
    swept_blend(bottom, top, &path, fr, tol)
}
