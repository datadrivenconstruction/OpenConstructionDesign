//! Edge curves (lines, circles, ellipses, polylines, B-splines with or without weights) and
//! the geometry readers they share with surfaces. Sampling keeps the chord within the
//! tolerance and every step within [`MAX_STEP`].

use std::f64::consts::{PI, TAU};

use super::math::*;
use super::p21::{Store, P};

/// Largest turn of one tessellation step (curves and surface normals).
pub const MAX_STEP: f64 = 10.0 * PI / 180.0;
/// Highest B-spline degree accepted.
pub const MAXD: usize = 32;

pub fn point(st: &Store, id: u64) -> Result<V, String> {
    let a = st
        .rec(id, "CARTESIAN_POINT")
        .ok_or_else(|| format!("#{id} is {}, not a CARTESIAN_POINT", st.describe(id)))?;
    let c = a.get(1).map(P::nums).unwrap_or_default();
    if c.len() < 2 || c.iter().any(|x| !x.is_finite()) {
        return Err(format!("#{id}: bad coordinates"));
    }
    Ok([c[0], c[1], c.get(2).copied().unwrap_or(0.0)])
}

pub fn direction(st: &Store, id: u64) -> Result<V, String> {
    let a = st
        .rec(id, "DIRECTION")
        .ok_or_else(|| format!("#{id} is {}, not a DIRECTION", st.describe(id)))?;
    let c = a.get(1).map(P::nums).unwrap_or_default();
    let v = [
        c.first().copied().unwrap_or(0.0),
        c.get(1).copied().unwrap_or(0.0),
        c.get(2).copied().unwrap_or(0.0),
    ];
    unit(v).ok_or_else(|| format!("#{id}: zero direction"))
}

fn opt_dir(st: &Store, p: Option<&P>) -> Result<Option<V>, String> {
    match p.and_then(P::id) {
        Some(d) => direction(st, d).map(Some),
        None => Ok(None),
    }
}

/// AXIS2_PLACEMENT_3D as a frame (file units).
pub fn placement(st: &Store, id: u64) -> Result<Ax, String> {
    let a = st
        .rec(id, "AXIS2_PLACEMENT_3D")
        .ok_or_else(|| format!("#{id} is {}, not an AXIS2_PLACEMENT_3D", st.describe(id)))?;
    let o = point(st, a.get(1).and_then(P::id).ok_or_else(|| format!("#{id}: no location"))?)?;
    Ok(Ax::new(o, opt_dir(st, a.get(2))?, opt_dir(st, a.get(3))?))
}

/// AXIS2_PLACEMENT_3D or CARTESIAN_TRANSFORMATION_OPERATOR_3D as a map to the containing
/// context (file units).
pub fn transform(st: &Store, id: u64) -> Result<Xf, String> {
    if st.is(id, "AXIS2_PLACEMENT_3D") {
        return placement(st, id).map(|a| a.xf());
    }
    let a = ["CARTESIAN_TRANSFORMATION_OPERATOR_3D", "CARTESIAN_TRANSFORMATION_OPERATOR"]
        .iter()
        .find_map(|n| st.rec(id, n))
        .ok_or_else(|| format!("#{id} is {}, not a placement", st.describe(id)))?;
    // attribute order differs between schema editions: take directions, origin and scale by type
    let mut dirs: Vec<Option<V>> = vec![];
    let mut origin = None;
    let mut scale = None;
    for p in a.iter().skip(1) {
        match p.id() {
            Some(r) if st.is(r, "DIRECTION") => dirs.push(Some(direction(st, r)?)),
            Some(r) if st.is(r, "CARTESIAN_POINT") => origin = Some(point(st, r)?),
            None if matches!(p, P::Null) && origin.is_none() => dirs.push(None),
            _ => {
                if let Some(s) = p.num() {
                    scale = Some(s);
                }
            }
        }
    }
    let o = origin.ok_or_else(|| format!("#{id}: no local origin"))?;
    let d = |k: usize| dirs.get(k).copied().flatten();
    let ax = Ax::new(o, d(2).or_else(|| match (d(0), d(1)) {
        (Some(x), Some(y)) => unit(cross(x, y)),
        _ => None,
    }), d(0));
    let s = scale.filter(|s| *s > 0.0).unwrap_or(1.0);
    let mut x = ax.xf();
    for row in x.m.iter_mut() {
        for v in row.iter_mut() {
            *v *= s;
        }
    }
    Ok(x)
}

/// Knot vector with its degree (multiplicities expanded).
#[derive(Clone, Debug)]
pub struct Basis {
    pub deg: usize,
    pub k: Vec<f64>,
}

impl Basis {
    pub fn new(deg: f64, mults: &[f64], knots: &[f64], n: usize) -> Result<Basis, String> {
        if !(deg >= 1.0 && deg < MAXD as f64) {
            return Err(format!("unsupported B-spline degree {deg}"));
        }
        let deg = deg as usize;
        if mults.len() != knots.len() {
            return Err("knot multiplicities and knots differ in length".into());
        }
        let mut k = vec![];
        for (&m, &t) in mults.iter().zip(knots) {
            if !(1.0..=64.0).contains(&m) || !t.is_finite() {
                return Err("bad knot multiplicity".into());
            }
            k.extend(std::iter::repeat(t).take(m.round() as usize));
        }
        if k.len() != n + deg + 1 {
            return Err(format!(
                "{} knots do not fit {n} control points of degree {deg}",
                k.len()
            ));
        }
        if k.windows(2).any(|w| w[1] < w[0]) || k[n] <= k[deg] {
            return Err("knots are not increasing".into());
        }
        Ok(Basis { deg, k })
    }

    pub fn n(&self) -> usize {
        self.k.len() - self.deg - 1
    }

    pub fn dom(&self) -> (f64, f64) {
        (self.k[self.deg], self.k[self.n()])
    }

    /// Non-empty knot spans.
    pub fn spans(&self) -> usize {
        (self.deg..self.n()).filter(|&i| self.k[i + 1] > self.k[i]).count()
    }

    pub fn span(&self, t: f64) -> usize {
        let (p, n) = (self.deg, self.n());
        if t >= self.k[n] {
            let mut s = n - 1;
            while s > p && self.k[s] >= self.k[s + 1] {
                s -= 1;
            }
            return s;
        }
        if t <= self.k[p] {
            let mut s = p;
            while s < n - 1 && self.k[s + 1] <= self.k[s] {
                s += 1;
            }
            return s;
        }
        let (mut lo, mut hi) = (p, n);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if t < self.k[mid] {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    /// The `deg + 1` basis functions non-zero on span `s` at `t`.
    pub fn funs(&self, s: usize, t: f64, out: &mut [f64; MAXD]) {
        let p = self.deg;
        let mut left = [0.0; MAXD];
        let mut right = [0.0; MAXD];
        out[0] = 1.0;
        for j in 1..=p {
            left[j] = t - self.k[s + 1 - j];
            right[j] = self.k[s + j] - t;
            let mut saved = 0.0;
            for r in 0..j {
                let den = right[r + 1] + left[j - r];
                let tmp = if den != 0.0 { out[r] / den } else { 0.0 };
                out[r] = saved + right[r + 1] * tmp;
                saved = left[j - r] * tmp;
            }
            out[j] = saved;
        }
    }
}

pub fn weights(p: Option<&P>, n: usize) -> Result<Option<Vec<f64>>, String> {
    let Some(p) = p else { return Ok(None) };
    let w = p.nums();
    if w.len() != n || w.iter().any(|&x| !(x > 0.0 && x.is_finite())) {
        return Err("bad B-spline weights".into());
    }
    Ok(Some(w))
}

#[derive(Clone, Debug)]
pub struct NCurve {
    pub b: Basis,
    pub p: Vec<V>,
    pub w: Option<Vec<f64>>,
}

impl NCurve {
    pub fn at(&self, t: f64) -> V {
        let (t0, t1) = self.b.dom();
        let t = t.clamp(t0, t1);
        let s = self.b.span(t);
        let mut f = [0.0; MAXD];
        self.b.funs(s, t, &mut f);
        let mut acc = [0.0; 3];
        let mut ws = 0.0;
        for (i, &fi) in f.iter().enumerate().take(self.b.deg + 1) {
            let j = s - self.b.deg + i;
            let c = fi * self.w.as_ref().map_or(1.0, |w| w[j]);
            acc = add(acc, mul(self.p[j], c));
            ws += c;
        }
        if ws.abs() > 1e-300 {
            mul(acc, 1.0 / ws)
        } else {
            acc
        }
    }

    /// Parameter of the curve point nearest `p`.
    pub fn invert(&self, p: V) -> f64 {
        let (t0, t1) = self.b.dom();
        let m = (self.b.spans() * 8).clamp(16, 4000);
        let mut best = (f64::MAX, t0);
        for i in 0..=m {
            let t = t0 + (t1 - t0) * i as f64 / m as f64;
            let d = dist(self.at(t), p);
            if d < best.0 {
                best = (d, t);
            }
        }
        let mut t = best.1;
        let h = (t1 - t0) * 1e-6;
        for _ in 0..20 {
            let c = self.at(t);
            let (a, b) = (self.at(t - h), self.at(t + h));
            let d1 = mul(sub(b, a), 0.5 / h);
            let d2 = mul(add(sub(b, mul(c, 2.0)), a), 1.0 / (h * h));
            let r = sub(c, p);
            let f = dot(d1, r);
            let df = dot(d2, r) + dot(d1, d1);
            if df.abs() < 1e-300 {
                break;
            }
            let nt = (t - f / df).clamp(t0, t1);
            let done = (nt - t).abs() < 1e-14 * (t1 - t0);
            t = nt;
            if done {
                break;
            }
        }
        if dist(self.at(t), p) <= best.0 {
            t
        } else {
            best.1
        }
    }
}

#[derive(Clone, Debug)]
pub enum Curve {
    Line,
    Circle(Ax, f64),
    Ellipse(Ax, f64, f64),
    Poly(Vec<V>),
    Spline(NCurve),
}

fn spline(st: &Store, deg: Option<&P>, ctrl: Option<&P>, mults: Option<&P>, knots: Option<&P>, w: Option<&P>) -> Result<Curve, String> {
    let pts = ctrl
        .map(P::ids)
        .unwrap_or_default()
        .into_iter()
        .map(|i| point(st, i))
        .collect::<Result<Vec<V>, String>>()?;
    let b = Basis::new(
        deg.and_then(P::num).unwrap_or(0.0),
        &mults.map(P::nums).unwrap_or_default(),
        &knots.map(P::nums).unwrap_or_default(),
        pts.len(),
    )?;
    let w = weights(w, pts.len())?;
    Ok(Curve::Spline(NCurve { b, p: pts, w }))
}

/// Reads a curve; curves on surfaces and trimmed curves give their 3D basis curve.
pub fn read(st: &Store, id: u64, depth: u32) -> Result<Curve, String> {
    if depth > 8 {
        return Err(format!("#{id}: curve references nest too deep"));
    }
    if let Some(a) = st.rec(id, "B_SPLINE_CURVE") {
        let k = st
            .rec(id, "B_SPLINE_CURVE_WITH_KNOTS")
            .ok_or_else(|| format!("unsupported curve {}", st.describe(id)))?;
        let w = st.rec(id, "RATIONAL_B_SPLINE_CURVE").and_then(|r| r.first());
        return spline(st, a.first(), a.get(1), k.first(), k.get(1), w);
    }
    let kind = st.kind(id).unwrap_or("?");
    let a = st.rec(id, kind).unwrap_or(&[]);
    let num = |i: usize| a.get(i).and_then(P::num).filter(|x| x.is_finite() && *x > 0.0);
    let pl = || placement(st, a.get(1).and_then(P::id).unwrap_or(0));
    match kind {
        "LINE" => Ok(Curve::Line),
        "CIRCLE" => Ok(Curve::Circle(pl()?, num(2).ok_or_else(|| format!("#{id}: bad radius"))?)),
        "ELLIPSE" => Ok(Curve::Ellipse(
            pl()?,
            num(2).ok_or_else(|| format!("#{id}: bad semi axis"))?,
            num(3).ok_or_else(|| format!("#{id}: bad semi axis"))?,
        )),
        "POLYLINE" => {
            let pts = a
                .get(1)
                .map(P::ids)
                .unwrap_or_default()
                .into_iter()
                .map(|i| point(st, i))
                .collect::<Result<Vec<V>, String>>()?;
            if pts.len() < 2 {
                return Err(format!("#{id}: polyline with fewer than two points"));
            }
            Ok(Curve::Poly(pts))
        }
        "B_SPLINE_CURVE_WITH_KNOTS" => spline(st, a.get(1), a.get(2), a.get(6), a.get(7), None),
        "SURFACE_CURVE" | "SEAM_CURVE" | "INTERSECTION_CURVE" | "TRIMMED_CURVE" => {
            let c = a.get(1).and_then(P::id).ok_or_else(|| format!("#{id}: no basis curve"))?;
            read(st, c, depth + 1)
        }
        _ => Err(format!("unsupported curve {}", st.describe(id))),
    }
}

fn conic_steps(span: f64, r: f64, tol: f64) -> usize {
    let chord = if tol < r { 2.0 * (1.0 - tol / r).acos() } else { PI };
    let step = chord.min(MAX_STEP).max(1e-4);
    ((span / step).ceil() as usize).clamp(1, 20_000)
}

/// Points from `a` to `b` along the curve (`sense`: the edge runs with the curve), ends exact.
pub fn sample(c: &Curve, a: V, b: V, sense: bool, tol: f64) -> Vec<V> {
    let mut out = match c {
        Curve::Line => vec![a, b],
        Curve::Circle(ax, r) => conic(ax, *r, *r, a, b, sense, tol),
        Curve::Ellipse(ax, ra, rb) => conic(ax, *ra, *rb, a, b, sense, tol),
        Curve::Poly(pts) => poly(pts, a, b, sense),
        Curve::Spline(n) => nurbs(n, a, b, sense, tol),
    };
    if out.len() < 2 {
        out = vec![a, b];
    }
    out[0] = a;
    let last = out.len() - 1;
    out[last] = b;
    out
}

fn conic(ax: &Ax, ra: f64, rb: f64, a: V, b: V, sense: bool, tol: f64) -> Vec<V> {
    let ang = |p: V| {
        let l = ax.local(p);
        (l[1] / rb).atan2(l[0] / ra)
    };
    let (s, e0) = if sense { (ang(a), ang(b)) } else { (ang(b), ang(a)) };
    let mut e = e0;
    if a == b {
        e = s + TAU;
    } else {
        while e <= s + 1e-12 {
            e += TAU;
        }
    }
    let n = conic_steps(e - s, ra.max(rb), tol);
    let mut v: Vec<V> = (0..=n)
        .map(|i| {
            let t = s + (e - s) * i as f64 / n as f64;
            ax.at(ra * t.cos(), rb * t.sin(), 0.0)
        })
        .collect();
    if !sense {
        v.reverse();
    }
    v
}

fn poly(pts: &[V], a: V, b: V, sense: bool) -> Vec<V> {
    let param = |p: V| {
        let mut best = (f64::MAX, 0.0);
        for (i, w) in pts.windows(2).enumerate() {
            let d = sub(w[1], w[0]);
            let l2 = dot(d, d);
            let t = if l2 > 0.0 { (dot(sub(p, w[0]), d) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let dd = dist(p, lerp(w[0], w[1], t));
            if dd < best.0 {
                best = (dd, i as f64 + t);
            }
        }
        best.1
    };
    let (pa, pb) = if sense { (a, b) } else { (b, a) };
    let (s, e) = (param(pa), param(pb));
    let mut v = vec![pa];
    if s <= e {
        v.extend((0..pts.len()).filter(|&k| (k as f64) > s && (k as f64) < e).map(|k| pts[k]));
    } else {
        v.extend((0..pts.len()).rev().filter(|&k| (k as f64) < s && (k as f64) > e).map(|k| pts[k]));
    }
    v.push(pb);
    if !sense {
        v.reverse();
    }
    v
}

fn nurbs(n: &NCurve, a: V, b: V, sense: bool, tol: f64) -> Vec<V> {
    let (t0, t1) = n.b.dom();
    let (pa, pb) = if sense { (a, b) } else { (b, a) };
    let eps = tol.max(1e-12);
    let s = if dist(pa, n.at(t0)) <= eps { t0 } else { n.invert(pa) };
    let e = if dist(pb, n.at(t1)) <= eps { t1 } else { n.invert(pb) };
    let closed = dist(n.at(t0), n.at(t1)) <= eps;
    let mut v = if e > s + 1e-12 * (t1 - t0) {
        adapt(n, s, e, tol)
    } else if closed {
        let mut v = adapt(n, s, t1, tol);
        v.pop();
        v.extend(adapt(n, t0, e, tol));
        v
    } else {
        let mut v = adapt(n, e, s, tol);
        v.reverse();
        v
    };
    if !sense {
        v.reverse();
    }
    v
}

fn adapt(n: &NCurve, s: f64, e: f64, tol: f64) -> Vec<V> {
    let (t0, t1) = n.b.dom();
    let frac = ((e - s) / (t1 - t0)).clamp(0.0, 1.0);
    let segs = ((n.b.spans() * (n.b.deg + 1)) as f64 * frac).ceil().clamp(2.0, 4000.0) as usize;
    let mut out = vec![n.at(s)];
    for i in 0..segs {
        let a = s + (e - s) * i as f64 / segs as f64;
        let b = s + (e - s) * (i + 1) as f64 / segs as f64;
        let (pa, pb) = (n.at(a), n.at(b));
        split(n, a, pa, b, pb, tol, 0, &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn split(n: &NCurve, a: f64, pa: V, b: f64, pb: V, tol: f64, depth: u32, out: &mut Vec<V>) {
    let m = 0.5 * (a + b);
    let pm = n.at(m);
    let (u, w) = (sub(pm, pa), sub(pb, pm));
    let (lu, lw) = (len(u), len(w));
    let turn = if lu > tol * 1e-3 && lw > tol * 1e-3 {
        (dot(u, w) / (lu * lw)).clamp(-1.0, 1.0).acos()
    } else {
        0.0
    };
    if depth < 14 && (seg_dist(pm, pa, pb) > tol || turn > MAX_STEP * 0.5) {
        split(n, a, pa, m, pm, tol, depth + 1, out);
        split(n, m, pm, b, pb, tol, depth + 1, out);
    } else {
        out.push(pb);
    }
}
