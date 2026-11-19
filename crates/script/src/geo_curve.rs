//! Curves and placement of the geometry language: `curve.*` constructors (circle, ellipse,
//! arc through 3 points, helix, spline, Bezier, NURBS, function, offset), the methods every
//! path has (point, tangent, frame, divide, measure, slice, reverse, closest), surface
//! normals and curvature, and patterns along curves, over surfaces and on grids.
//! A curve is a `PathV`: dense straight segments; arcs of `arc()` are expanded when sampled.

use std::f64::consts::PI;

use mlua::{FromLua, Function, Lua, Table, UserDataMethods, Value};

use crate::blocks::{v, Opts};
use crate::blocks_surf::Surf;
use crate::stdlib::{builder, MAX_COPY_POINTS};
use crate::values::{err, FormV, LResult, Mat, PathV, V3, UP};

/// Points one constructed curve may hold.
const MAX_CURVE: usize = 20_000;

// ---- sampling ----

/// The path as straight segments; arcs are cut at 7.5 degrees.
pub(crate) fn flatten(p: &PathV) -> Vec<V3> {
    let n = p.pts.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..p.segs() {
        let (a, c) = (p.pts[i], p.pts[(i + 1) % n]);
        let b = p.bulges.get(i).copied().unwrap_or(0.0);
        out.push(a);
        let d = c.sub(a);
        let chord = d.x.hypot(d.y);
        if b.abs() > 1e-12 && chord > 1e-12 {
            let th = 4.0 * b.atan();
            let r = chord / (2.0 * (th / 2.0).sin().abs());
            let h = (chord / 2.0) / (th / 2.0).tan();
            let (nx, ny) = (-d.y / chord, d.x / chord);
            let (cx, cy) = ((a.x + c.x) / 2.0 + nx * h, (a.y + c.y) / 2.0 + ny * h);
            let a0 = (a.y - cy).atan2(a.x - cx);
            let k = ((th.abs() / (PI / 24.0)).ceil() as usize).max(2);
            for j in 1..k {
                let s = j as f64 / k as f64;
                let ang = a0 + th * s;
                out.push(V3::new(cx + r * ang.cos(), cy + r * ang.sin(), a.z + d.z * s));
            }
        }
    }
    if !p.closed {
        if let Some(l) = p.pts.last() {
            out.push(*l);
        }
    }
    out
}

/// A path by arc length.
pub(crate) struct Poly {
    pub pts: Vec<V3>,
    cum: Vec<f64>,
    pub closed: bool,
}

impl Poly {
    pub fn of(p: &PathV) -> Poly {
        Poly::from_points(flatten(p), p.closed)
    }

    pub fn from_points(mut pts: Vec<V3>, closed: bool) -> Poly {
        if closed {
            if let Some(f) = pts.first().copied() {
                pts.push(f);
            }
        }
        let mut out: Vec<V3> = Vec::with_capacity(pts.len());
        for p in pts {
            if out.last().is_none_or(|l| l.sub(p).len() > 1e-9) {
                out.push(p);
            }
        }
        let mut cum = vec![0.0];
        for w in out.windows(2) {
            cum.push(cum[cum.len() - 1] + w[1].sub(w[0]).len());
        }
        Poly { pts: out, cum, closed }
    }

    pub fn total(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    pub fn usable(&self) -> bool {
        self.pts.len() >= 2 && self.total() > 1e-9
    }

    /// Point and unit tangent at arc length `s`.
    pub fn at_len(&self, s: f64) -> (V3, V3) {
        let n = self.pts.len();
        let s = s.clamp(0.0, self.total());
        let i = self.cum.partition_point(|&c| c <= s).saturating_sub(1).min(n - 2);
        let l = self.cum[i + 1] - self.cum[i];
        let t = ((s - self.cum[i]) / l).clamp(0.0, 1.0);
        let d = self.pts[i + 1].sub(self.pts[i]);
        (self.pts[i].add(d.scale(t)), d.unit().unwrap_or(UP))
    }

    /// Point and tangent at fraction `t` (0..1) of the length.
    pub fn at(&self, t: f64) -> (V3, V3) {
        self.at_len(t.clamp(0.0, 1.0) * self.total())
    }

    /// Fraction of the length of the point nearest to `p`, and the distance.
    pub fn closest(&self, p: V3) -> (f64, f64) {
        let (mut best, mut at) = (f64::MAX, 0.0);
        for (i, w) in self.pts.windows(2).enumerate() {
            let d = w[1].sub(w[0]);
            let l2 = d.dot(d);
            let t = if l2 > 0.0 { (p.sub(w[0]).dot(d) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let q = w[0].add(d.scale(t));
            let dist = q.sub(p).len();
            if dist < best {
                best = dist;
                at = self.cum[i] + t * l2.sqrt();
            }
        }
        (at / self.total().max(1e-12), best)
    }

    /// The part between fractions `t0` and `t1`.
    pub fn slice(&self, t0: f64, t1: f64) -> Vec<V3> {
        let (s0, s1) = (t0.clamp(0.0, 1.0) * self.total(), t1.clamp(0.0, 1.0) * self.total());
        let mut out = vec![self.at_len(s0).0];
        for (i, c) in self.cum.iter().enumerate() {
            if *c > s0 + 1e-9 && *c < s1 - 1e-9 {
                out.push(self.pts[i]);
            }
        }
        out.push(self.at_len(s1).0);
        out
    }
}

pub(crate) fn path_from(pts: Vec<V3>, closed: bool) -> PathV {
    let segs = if closed { pts.len() } else { pts.len().saturating_sub(1) };
    PathV { pts, bulges: vec![0.0; segs], closed }
}

fn checked(f: &str, pts: Vec<V3>, closed: bool) -> LResult<PathV> {
    if pts.len() < 2 {
        return err(format!("{f}: the curve has fewer than 2 points"));
    }
    if pts.len() > MAX_CURVE {
        return err(format!("{f}: {} points; use fewer segments (at most {MAX_CURVE})", pts.len()));
    }
    if pts.iter().any(|p| !p.finite()) {
        return err(format!("{f}: a point is not finite"));
    }
    Ok(path_from(pts, closed))
}

/// A path from a path value or a list of points.
pub(crate) fn path_val(lua: &Lua, x: Value, f: &str, k: &str) -> LResult<PathV> {
    match &x {
        Value::UserData(u) if u.is::<PathV>() => Ok(u.borrow::<PathV>()?.clone()),
        Value::Table(t) => {
            let mut pts = vec![];
            for p in t.clone().sequence_values::<Value>() {
                pts.push(V3::from_lua(p?, lua)?);
            }
            checked(f, pts, false)
        }
        other => err(format!(
            "{f}: {k} needs a curve (curve.spline{{...}}, polyline(...), arc(...)) or a list of \
             points like {{vec3(0,0,0), vec3(5,0,3)}}, got {}",
            other.type_name()
        )),
    }
}

fn list_val(lua: &Lua, x: Value, f: &str, k: &str) -> LResult<Vec<V3>> {
    match x {
        Value::Table(t) => {
            let mut pts = vec![];
            for p in t.sequence_values::<Value>() {
                pts.push(V3::from_lua(p?, lua)?);
            }
            Ok(pts)
        }
        Value::UserData(u) if u.is::<PathV>() => {
            let p = u.borrow::<PathV>()?;
            Ok(flatten(&p))
        }
        other => err(format!(
            "{f}: {k} needs a list of points like {{vec3(0,0,0), vec3(5,0,3)}}, got {}",
            other.type_name()
        )),
    }
}

// ---- constructors ----

fn plane_basis(n: V3) -> (V3, V3) {
    let n = n.unit().unwrap_or(UP);
    let u = if n.cross(UP).len() < 1e-9 {
        V3::new(1.0, 0.0, 0.0)
    } else {
        UP.cross(n).unit().unwrap_or(V3::new(1.0, 0.0, 0.0))
    };
    (u, n.cross(u))
}

fn need(f: &str, k: &str, x: Option<f64>, example: &str) -> LResult<f64> {
    match x {
        Some(x) if x.is_finite() && x > 0.0 => Ok(x),
        _ => err(format!("{f}: {k} is required and positive, e.g. {example}")),
    }
}

fn opt_len(o: &Opts, k: &'static str) -> LResult<Option<f64>> {
    if matches!(o.value(k)?, Value::Nil) {
        Ok(None)
    } else {
        o.len(k, 0.0).map(Some)
    }
}

fn opt_num(o: &Opts, k: &'static str, lo: f64, hi: f64) -> LResult<Option<f64>> {
    if matches!(o.value(k)?, Value::Nil) {
        Ok(None)
    } else {
        o.num(k, lo, lo, hi).map(Some)
    }
}

/// Circle, ellipse and their arcs.
fn conic(lua: &Lua, t: Option<Table>, circle: bool) -> LResult<PathV> {
    let f: &'static str = if circle { "curve.circle" } else { "curve.ellipse" };
    let o = Opts::new(f, t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let normal = o.point("normal", UP)?;
    let radius = opt_len(&o, "radius")?;
    let a = if circle { radius } else { opt_len(&o, "a")?.or(radius) };
    let b = if circle { radius } else { opt_len(&o, "b")?.or(a) };
    let rot = o.ang("rotation", 0.0)?;
    let start = o.ang("start", 0.0)?;
    let sweep = o.ang("sweep", 2.0 * PI)?;
    let n = o.int("segments", 96, 4, 4000)? as usize;
    o.done()?;
    let ex = if circle {
        "curve.circle{center = vec3(0,0,0), radius = 5}"
    } else {
        "curve.ellipse{center = vec3(0,0,0), a = 8, b = 4}"
    };
    let (a, b) = (need(f, if circle { "radius" } else { "a" }, a, ex)?, need(f, "b", b, ex)?);
    if normal.unit().is_none() {
        return err(format!("{f}: normal is a zero vector"));
    }
    if sweep.abs() < 1e-9 {
        return err(format!("{f}: sweep is zero"));
    }
    let (u0, w0) = plane_basis(normal);
    let (s, k) = rot.sin_cos();
    let (u, w) = (u0.scale(k).add(w0.scale(s)), w0.scale(k).sub(u0.scale(s)));
    let full = sweep.abs() >= 2.0 * PI - 1e-9;
    let steps = if full { n } else { n + 1 };
    let pts = (0..steps)
        .map(|i| {
            let span = if full { 2.0 * PI } else { sweep.abs() };
            let ang = start + sweep.signum() * span * (i as f64 / n as f64);
            c.add(u.scale(a * ang.cos())).add(w.scale(b * ang.sin()))
        })
        .collect();
    checked(f, pts, full)
}

fn helix(lua: &Lua, t: Option<Table>) -> LResult<PathV> {
    let f = "curve.helix";
    let o = Opts::new(f, t, lua)?;
    let c = o.point("center", v(0.0, 0.0, 0.0))?;
    let r0 = opt_len(&o, "radius")?;
    let r1 = opt_len(&o, "radius_end")?.or(r0);
    let pitch = opt_len(&o, "pitch")?;
    let turns = opt_num(&o, "turns", 0.01, 1000.0)?;
    let height = opt_len(&o, "height")?;
    let start = o.ang("start", 0.0)?;
    let cw = o.flag("clockwise", false)?;
    let per = o.int("segments", 48, 8, 360)? as usize;
    o.done()?;
    let ex = "curve.helix{radius = 3, pitch = 4, turns = 5}";
    let r0 = need(f, "radius", r0, ex)?;
    let r1 = need(f, "radius_end", r1, ex)?;
    let (turns, height) = match (pitch, turns, height) {
        (Some(p), Some(t), _) => (t, p * t),
        (Some(p), None, Some(h)) if p > 0.0 => (h / p, h),
        (None, Some(t), Some(h)) => (t, h),
        _ => return err(format!("{f}: give two of pitch, turns, height, e.g. {ex}")),
    };
    let n = ((turns * per as f64).ceil() as usize).max(2);
    let dir = if cw { -1.0 } else { 1.0 };
    let pts = (0..=n)
        .map(|i| {
            let s = i as f64 / n as f64;
            let ang = start + dir * 2.0 * PI * turns * s;
            let r = r0 + (r1 - r0) * s;
            V3::new(c.x + r * ang.cos(), c.y + r * ang.sin(), c.z + height * s)
        })
        .collect();
    checked(f, pts, false)
}

/// Barry-Goldman evaluation of a Catmull-Rom span; `alpha` 0.5 is the centripetal form.
fn catmull(p: &[V3; 4], alpha: f64, t: f64) -> V3 {
    let mut ts = [0.0; 4];
    for i in 1..4 {
        let d = p[i].sub(p[i - 1]).len().max(1e-9);
        ts[i] = ts[i - 1] + d.powf(alpha);
    }
    let tt = ts[1] + (ts[2] - ts[1]) * t;
    let mix = |a: V3, b: V3, ta: f64, tb: f64| {
        if (tb - ta).abs() < 1e-12 {
            a
        } else {
            a.scale((tb - tt) / (tb - ta)).add(b.scale((tt - ta) / (tb - ta)))
        }
    };
    let a1 = mix(p[0], p[1], ts[0], ts[1]);
    let a2 = mix(p[1], p[2], ts[1], ts[2]);
    let a3 = mix(p[2], p[3], ts[2], ts[3]);
    let b1 = mix(a1, a2, ts[0], ts[2]);
    let b2 = mix(a2, a3, ts[1], ts[3]);
    mix(b1, b2, ts[1], ts[2])
}

fn spline_points(pts: &[V3], closed: bool, per: usize, alpha: f64) -> Vec<V3> {
    let mut q: Vec<V3> = vec![];
    for p in pts {
        if q.last().is_none_or(|l| l.sub(*p).len() > 1e-9) {
            q.push(*p);
        }
    }
    let n = q.len();
    if n < 3 {
        return q;
    }
    let at = |i: i64| -> V3 {
        if closed {
            q[i.rem_euclid(n as i64) as usize]
        } else if i < 0 {
            q[0].scale(2.0).sub(q[1])
        } else if i >= n as i64 {
            q[n - 1].scale(2.0).sub(q[n - 2])
        } else {
            q[i as usize]
        }
    };
    let spans = if closed { n } else { n - 1 };
    let mut out = vec![];
    for s in 0..spans {
        let w = [at(s as i64 - 1), at(s as i64), at(s as i64 + 1), at(s as i64 + 2)];
        for j in 0..per {
            out.push(catmull(&w, alpha, j as f64 / per as f64));
        }
    }
    if !closed {
        out.push(q[n - 1]);
    }
    out
}

fn spline(lua: &Lua, (pts, t): (Value, Option<Table>)) -> LResult<PathV> {
    let f = "curve.spline";
    let o = Opts::new(f, t, lua)?;
    let closed = o.flag("closed", false)?;
    let per = o.int("segments", 16, 1, 400)? as usize;
    let alpha = o.num("alpha", 0.5, 0.0, 1.0)?;
    o.done()?;
    let pts = list_val(lua, pts, f, "the first argument")?;
    if pts.len() < 2 || (closed && pts.len() < 3) {
        return err(format!(
            "{f}: needs 2 or more points (3 when closed): curve.spline({{vec3(0,0,0), vec3(5,2,1), vec3(9,0,3)}}, {{closed = false}})"
        ));
    }
    checked(f, spline_points(&pts, closed, per, alpha), closed)
}

fn bezier(lua: &Lua, (pts, t): (Value, Option<Table>)) -> LResult<PathV> {
    let f = "curve.bezier";
    let o = Opts::new(f, t, lua)?;
    let n = o.int("segments", 64, 2, 4000)? as usize;
    o.done()?;
    let p = list_val(lua, pts, f, "the first argument")?;
    if p.len() < 2 || p.len() > 60 {
        return err(format!("{f}: needs 2..60 control points: curve.bezier({{a, b, c, d}})"));
    }
    let pts = (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let mut w = p.clone();
            while w.len() > 1 {
                w = w.windows(2).map(|x| x[0].add(x[1].sub(x[0]).scale(t))).collect();
            }
            w[0]
        })
        .collect();
    checked(f, pts, false)
}

/// Basis functions of degree `p` at `u` in knot span `k` (Piegl and Tiller A2.2).
fn basis(k: usize, u: f64, p: usize, kn: &[f64]) -> Vec<f64> {
    let mut nb = vec![0.0; p + 1];
    let (mut left, mut right) = (vec![0.0; p + 1], vec![0.0; p + 1]);
    nb[0] = 1.0;
    for j in 1..=p {
        left[j] = u - kn[k + 1 - j];
        right[j] = kn[k + j] - u;
        let mut saved = 0.0;
        for r in 0..j {
            let d = right[r + 1] + left[j - r];
            let t = if d.abs() < 1e-14 { 0.0 } else { nb[r] / d };
            nb[r] = saved + right[r + 1] * t;
            saved = left[j - r] * t;
        }
        nb[j] = saved;
    }
    nb
}

fn nurbs(lua: &Lua, t: Option<Table>) -> LResult<PathV> {
    let f = "curve.nurbs";
    let o = Opts::new(f, t, lua)?;
    let pts = o.points("points")?;
    let w = o.table("weights")?;
    let degree = o.int("degree", 3, 1, 8)? as usize;
    let n = o.int("segments", 96, 2, 4000)? as usize;
    o.done()?;
    let ex = "curve.nurbs{points = {a, b, c, d}, weights = {1, 2, 2, 1}, degree = 3}";
    let Some(pts) = pts else {
        return err(format!("{f}: points are required, e.g. {ex}"));
    };
    let m = pts.len();
    if m < degree + 1 {
        return err(format!("{f}: degree {degree} needs {} or more points, got {m}", degree + 1));
    }
    let wt: Vec<f64> = match w {
        None => vec![1.0; m],
        Some(t) => t.sequence_values::<f64>().collect::<Result<Vec<f64>, mlua::Error>>()?,
    };
    if wt.len() != m || wt.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
        return err(format!("{f}: weights are one positive number per point ({m}); e.g. {ex}"));
    }
    // clamped uniform knots
    let nk = m + degree + 1;
    let mut kn = vec![0.0; nk];
    let inner = m - degree;
    for (i, x) in kn.iter_mut().enumerate() {
        *x = if i <= degree { 0.0 } else if i >= m { 1.0 } else { (i - degree) as f64 / inner as f64 };
    }
    let out = (0..=n)
        .map(|i| {
            let u = i as f64 / n as f64;
            let mut k = degree;
            while k + 1 < m && u >= kn[k + 1] {
                k += 1;
            }
            let nb = basis(k, u, degree, &kn);
            let (mut acc, mut ws) = (V3::new(0.0, 0.0, 0.0), 0.0);
            for (j, b) in nb.iter().enumerate() {
                let idx = k - degree + j;
                let ww = b * wt[idx];
                acc = acc.add(pts[idx].scale(ww));
                ws += ww;
            }
            acc.scale(1.0 / ws)
        })
        .collect();
    checked(f, out, false)
}

fn from_fn(lua: &Lua, (func, t0, t1, n, closed): (Function, f64, f64, Option<i64>, Option<bool>)) -> LResult<PathV> {
    let f = "curve.fn";
    let n = n.unwrap_or(64);
    if !(2..=4000).contains(&n) || !(t0.is_finite() && t1.is_finite()) || t0 == t1 {
        return err(format!(
            "{f}: curve.fn(function(t) return vec3(...) end, t0, t1, segments): t0 ~= t1 finite, segments 2..4000"
        ));
    }
    let closed = closed.unwrap_or(false);
    let steps = if closed { n } else { n + 1 };
    let mut pts = Vec::with_capacity(steps as usize);
    for i in 0..steps {
        let t = t0 + (t1 - t0) * i as f64 / n as f64;
        let p: Value = func.call(t)?;
        pts.push(V3::from_lua(p, lua)?);
    }
    checked(f, pts, closed)
}

/// Circle through three points, from the first to the third by way of the second.
fn arc3(lua: &Lua, (a, b, c, t): (V3, V3, V3, Option<Table>)) -> LResult<PathV> {
    let f = "curve.arc3";
    let o = Opts::new(f, t, lua)?;
    let n = o.int("segments", 48, 2, 4000)? as usize;
    o.done()?;
    let (ab, ac) = (b.sub(a), c.sub(a));
    let nrm = ab.cross(ac);
    let n2 = nrm.dot(nrm);
    if n2 < 1e-12 {
        return err(format!("{f}: the three points lie on one line; use polyline(a, b, c)"));
    }
    let ctr = a.add(
        nrm.cross(ab).scale(ac.dot(ac)).add(ac.cross(nrm).scale(ab.dot(ab))).scale(1.0 / (2.0 * n2)),
    );
    let r = ctr.sub(a).len();
    let nu = nrm.unit().unwrap_or(UP);
    let u = a.sub(ctr).scale(1.0 / r);
    let w = nu.cross(u);
    let ang = |p: V3| {
        let d = p.sub(ctr);
        d.dot(w).atan2(d.dot(u)).rem_euclid(2.0 * PI)
    };
    let (tb, tc) = (ang(b), ang(c));
    let sweep = if tb < tc { tc } else { tc - 2.0 * PI };
    let pts = (0..=n)
        .map(|i| {
            let th = sweep * i as f64 / n as f64;
            ctr.add(u.scale(r * th.cos())).add(w.scale(r * th.sin()))
        })
        .collect();
    checked(f, pts, false)
}

/// Planar offset in plan (x, y) by `d` (left of travel positive), mitred at vertices.
fn offset(lua: &Lua, (p, d): (Value, f64)) -> LResult<PathV> {
    let f = "curve.offset";
    let path = path_val(lua, p, f, "the first argument")?;
    if !d.is_finite() {
        return err(format!("{f}: distance must be a finite number: curve.offset(path, 0.5)"));
    }
    let pts = Poly::of(&path);
    let q: Vec<V3> = if path.closed { pts.pts[..pts.pts.len() - 1].to_vec() } else { pts.pts.clone() };
    let n = q.len();
    if n < 2 {
        return err(format!("{f}: the curve is a point"));
    }
    let seg = |i: usize| -> V3 {
        let dd = q[(i + 1) % n].sub(q[i]);
        V3::new(-dd.y, dd.x, 0.0).unit().unwrap_or(V3::new(0.0, 0.0, 0.0))
    };
    let out = (0..n)
        .map(|i| {
            let prev = if i == 0 && !path.closed { None } else { Some(seg((i + n - 1) % n)) };
            let next = if i + 1 == n && !path.closed { None } else { Some(seg(i)) };
            let dir = match (prev, next) {
                (Some(a), Some(b)) => {
                    let k = 1.0 + a.dot(b);
                    a.add(b).scale(1.0 / k.max(0.25))
                }
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => V3::new(0.0, 0.0, 0.0),
            };
            q[i].add(dir.scale(d))
        })
        .collect();
    checked(f, out, path.closed)
}

pub(crate) fn install(lua: &Lua, g: &Table) -> LResult<()> {
    let c = lua.create_table()?;
    c.set("circle", lua.create_function(|lua, t: Option<Table>| conic(lua, t, true))?)?;
    c.set("ellipse", lua.create_function(|lua, t: Option<Table>| conic(lua, t, false))?)?;
    c.set("helix", lua.create_function(helix)?)?;
    c.set("spline", lua.create_function(spline)?)?;
    c.set("bezier", lua.create_function(bezier)?)?;
    c.set("nurbs", lua.create_function(nurbs)?)?;
    c.set("fn", lua.create_function(from_fn)?)?;
    c.set("arc3", lua.create_function(arc3)?)?;
    c.set("offset", lua.create_function(offset)?)?;
    c.set(
        "line",
        lua.create_function(|_, (a, b): (V3, V3)| checked("curve.line", vec![a, b], false))?,
    )?;
    c.set(
        "polyline",
        lua.create_function(|lua, (p, closed): (Value, Option<bool>)| {
            let pts = list_val(lua, p, "curve.polyline", "the first argument")?;
            checked("curve.polyline", pts, closed.unwrap_or(false))
        })?,
    )?;
    g.set("curve", c)?;
    g.set("pattern_curve", lua.create_function(pattern_curve)?)?;
    g.set("pattern_surface", lua.create_function(pattern_surface)?)?;
    g.set("pattern_grid", lua.create_function(pattern_grid)?)?;
    Ok(())
}

// ---- path methods ----

fn frame_of(p: V3, tan: V3, mode: &str) -> Mat {
    let (x, y, z) = match mode {
        "up" => {
            let h = V3::new(tan.x, tan.y, 0.0).unit().unwrap_or(V3::new(1.0, 0.0, 0.0));
            (h, UP.cross(h), UP)
        }
        _ => {
            let x = UP.cross(tan).unit().unwrap_or(V3::new(1.0, 0.0, 0.0));
            (x, tan.cross(x), tan)
        }
    };
    Mat::from_basis(x, y, z, p)
}

fn mode_of(f: &str, m: Option<String>) -> LResult<String> {
    let m = m.unwrap_or_else(|| "tangent".into());
    match m.as_str() {
        "tangent" | "up" | "fixed" => Ok(m),
        other => err(format!("{f}: align `{other}` is not tangent, up or fixed")),
    }
}

pub(crate) fn path_methods<M: UserDataMethods<PathV>>(m: &mut M) {
    m.add_method("is_closed", |_, p, ()| Ok(p.closed));
    m.add_method("count", |_, p, ()| Ok(p.pts.len()));
    m.add_method("point", |_, p, t: f64| Ok(Poly::of(p).at(t).0));
    m.add_method("tangent", |_, p, t: f64| Ok(Poly::of(p).at(t).1));
    // placement frame: z along the tangent, or "up": z vertical, x along the curve
    m.add_method("frame", |_, p, (t, mode): (f64, Option<String>)| {
        let mode = mode_of("path:frame", mode)?;
        let (q, tan) = Poly::of(p).at(t);
        Ok(if mode == "fixed" { Mat::translate(q) } else { frame_of(q, tan, &mode) })
    });
    m.add_method("divide", |_, p, n: i64| {
        if !(1..=100_000).contains(&n) {
            return err("path:divide(n): n is 1..100000, e.g. path:divide(10) gives 11 points");
        }
        let poly = Poly::of(p);
        Ok((0..=n).map(|i| poly.at(i as f64 / n as f64).0).collect::<Vec<_>>())
    });
    m.add_method("measure", |_, p, (step, from): (f64, Option<f64>)| {
        let poly = Poly::of(p);
        if !(step.is_finite() && step > 1e-6) || poly.total() / step > 100_000.0 {
            return err("path:measure(step): step is a positive length, at most 100000 points, e.g. path:measure(2.5)");
        }
        let mut out = vec![];
        let mut s = from.unwrap_or(0.0).max(0.0);
        while s <= poly.total() + 1e-9 {
            out.push(poly.at_len(s).0);
            s += step;
        }
        Ok(out)
    });
    m.add_method("points", |_, p, ()| Ok(flatten(p)));
    m.add_method("reverse", |_, p, ()| {
        let mut pts = flatten(p);
        pts.reverse();
        Ok(path_from(pts, p.closed))
    });
    m.add_method("slice", |_, p, (t0, t1): (f64, f64)| {
        let poly = Poly::of(p);
        if !poly.usable() || t0 >= t1 {
            return err("path:slice(t0, t1): 0 <= t0 < t1 <= 1, e.g. path:slice(0.25, 0.75)");
        }
        Ok(path_from(poly.slice(t0, t1), false))
    });
    m.add_method("resample", |_, p, n: i64| {
        if !(1..=20_000).contains(&n) {
            return err("path:resample(n): n is 1..20000 segments");
        }
        let poly = Poly::of(p);
        let k = if p.closed { n } else { n + 1 };
        let pts = (0..k).map(|i| poly.at(i as f64 / n as f64).0).collect();
        Ok(path_from(pts, p.closed))
    });
    m.add_method("closest", |_, p, q: V3| {
        let (t, d) = Poly::of(p).closest(q);
        Ok((t, d))
    });
    // signed plan area of a closed path (counter-clockwise positive)
    m.add_method("area", |_, p, ()| {
        let q = flatten(p);
        let n = q.len();
        Ok((0..n).map(|i| q[i].x * q[(i + 1) % n].y - q[(i + 1) % n].x * q[i].y).sum::<f64>() / 2.0)
    });
    m.add_method("bounds", |lua, p, ()| {
        let q = flatten(p);
        let (mut lo, mut hi) = (q[0], q[0]);
        for x in &q {
            lo = V3::new(lo.x.min(x.x), lo.y.min(x.y), lo.z.min(x.z));
            hi = V3::new(hi.x.max(x.x), hi.y.max(x.y), hi.z.max(x.z));
        }
        let t = lua.create_table()?;
        t.set("min", lo)?;
        t.set("max", hi)?;
        Ok(t)
    });
}

// ---- surfaces ----

type AtFn<'a> = &'a dyn Fn(f64, f64) -> V3;

/// First derivatives and the unit normal at (u, w).
pub(crate) fn derivs(at: AtFn, u: f64, w: f64) -> (V3, V3, V3) {
    let h = 1e-4;
    let (u0, u1) = ((u - h).max(0.0), (u + h).min(1.0));
    let (w0, w1) = ((w - h).max(0.0), (w + h).min(1.0));
    let ru = at(u1, w).sub(at(u0, w)).scale(1.0 / (u1 - u0));
    let rv = at(u, w1).sub(at(u, w0)).scale(1.0 / (w1 - w0));
    let n = ru.cross(rv).unit().unwrap_or(UP);
    (ru, rv, n)
}

/// Gaussian and mean curvature (1/m) at (u, w); the sign of the mean follows the normal.
pub(crate) fn curvature(at: AtFn, u: f64, w: f64) -> (f64, f64) {
    let h = 1e-3;
    let (u, w) = (u.clamp(h, 1.0 - h), w.clamp(h, 1.0 - h));
    let (ru, rv, n) = derivs(at, u, w);
    let c = at(u, w);
    let ruu = at(u + h, w).sub(c.scale(2.0)).add(at(u - h, w)).scale(1.0 / (h * h));
    let rvv = at(u, w + h).sub(c.scale(2.0)).add(at(u, w - h)).scale(1.0 / (h * h));
    let ruv = at(u + h, w + h)
        .sub(at(u + h, w - h))
        .sub(at(u - h, w + h))
        .add(at(u - h, w - h))
        .scale(1.0 / (4.0 * h * h));
    let (e, f, g) = (ru.dot(ru), ru.dot(rv), rv.dot(rv));
    let (l, mm, nn) = (n.dot(ruu), n.dot(ruv), n.dot(rvv));
    let d = (e * g - f * f).max(1e-18);
    ((l * nn - mm * mm) / d, (e * nn - 2.0 * f * mm + g * l) / (2.0 * d))
}

pub(crate) fn surf_frame(at: AtFn, u: f64, w: f64, mode: &str) -> Mat {
    let p = at(u, w);
    let (ru, _, n) = derivs(at, u, w);
    match mode {
        "up" => {
            let x = V3::new(ru.x, ru.y, 0.0).unit().unwrap_or(V3::new(1.0, 0.0, 0.0));
            Mat::from_basis(x, UP.cross(x), UP, p)
        }
        _ => {
            let x = ru.sub(n.scale(ru.dot(n))).unit().unwrap_or(V3::new(1.0, 0.0, 0.0));
            Mat::from_basis(x, n.cross(x), n, p)
        }
    }
}

pub(crate) fn surf_methods<M: UserDataMethods<Surf>>(m: &mut M) {
    m.add_method("normal", |_, s, (u, w): (f64, f64)| Ok(derivs(&|a, b| s.at(a, b), u, w).2));
    m.add_method("curvature", |_, s, (u, w): (f64, f64)| {
        let (k, h) = curvature(&|a, b| s.at(a, b), u, w);
        Ok((k, h))
    });
    m.add_method("frame", |_, s, (u, w, mode): (f64, f64, Option<String>)| {
        let mode = mode_of("surface:frame", mode)?;
        Ok(surf_frame(&|a, b| s.at(a, b), u, w, &mode))
    });
    m.add_method("area", |_, s, n: Option<i64>| {
        let n = n.unwrap_or(40).clamp(2, 400) as usize;
        let mut sum = 0.0;
        for i in 0..n {
            for j in 0..n {
                let (u, w) = ((i as f64 + 0.5) / n as f64, (j as f64 + 0.5) / n as f64);
                let (ru, rv, _) = derivs(&|a, b| s.at(a, b), u, w);
                sum += ru.cross(rv).len() / (n * n) as f64;
            }
        }
        Ok(sum)
    });
}

// ---- patterns ----

fn charge(lua: &Lua, form: &FormV, n: usize, f: &str) -> LResult<()> {
    let cost = form.weight().saturating_mul(n);
    let mut b = builder(lua)?;
    let total = b.copy_points.saturating_add(cost);
    if total > MAX_COPY_POINTS {
        return err(format!(
            "{f}: {n} copies need {cost} points, {} already copied in this run; at most {MAX_COPY_POINTS}",
            b.copy_points
        ));
    }
    b.copy_points = total;
    Ok(())
}

fn place(f: &str, form: &FormV, m: &Mat) -> LResult<FormV> {
    form.transform(m).or_else(|e| err(format!("{f}: {e}")))
}

/// `pattern_curve(form, path, n, {align, from, to})`: copies of a form modelled at the origin,
/// each placed on the frame of the curve (`tangent`: local z along the curve, `up`: upright
/// and turned along the curve, `fixed`: moved only).
fn pattern_curve(lua: &Lua, (form, p, n, t): (FormV, Value, i64, Option<Table>)) -> LResult<Vec<FormV>> {
    let f = "pattern_curve";
    let o = Opts::new(f, t, lua)?;
    let align = mode_of(f, Some(o.text("align", "tangent")?))?;
    let t0 = o.num("from", 0.0, 0.0, 1.0)?;
    let t1 = o.num("to", 1.0, 0.0, 1.0)?;
    o.done()?;
    if !(1..=100_000).contains(&n) {
        return err("pattern_curve(form, path, n): n is 1..100000, e.g. pattern_curve(post, rail, 20)");
    }
    let path = path_val(lua, p, f, "the path")?;
    let poly = Poly::of(&path);
    if !poly.usable() {
        return err(format!("{f}: the path has no length"));
    }
    let n = n as usize;
    charge(lua, &form, n, f)?;
    let span = if path.closed && t0 == 0.0 && t1 == 1.0 { n } else { (n - 1).max(1) };
    (0..n)
        .map(|i| {
            let t = t0 + (t1 - t0) * i as f64 / span as f64;
            let (q, tan) = poly.at(t);
            let m = if align == "fixed" { Mat::translate(q) } else { frame_of(q, tan, &align) };
            place(f, &form, &m)
        })
        .collect()
}

/// A surface: `surf.*{...}` or a function(u, v) returning a point.
fn surface_at(x: Value, f: &str) -> LResult<Box<dyn Fn(f64, f64) -> V3>> {
    match x {
        Value::UserData(u) if u.is::<Surf>() => {
            let s = u.borrow::<Surf>()?.clone();
            Ok(Box::new(move |a, b| s.at(a, b)))
        }
        Value::Function(func) => Ok(Box::new(move |a, b| {
            func.call::<V3>((a, b)).unwrap_or(V3::new(f64::NAN, f64::NAN, f64::NAN))
        })),
        other => err(format!(
            "{f}: the surface is surf.dome{{...}}, surf.loft{{...}} or function(u, v) returning vec3, got {}",
            other.type_name()
        )),
    }
}

/// `pattern_surface(form, surface, nu, nv, {align, margin})`: copies on a grid of (u, v).
fn pattern_surface(
    lua: &Lua,
    (form, s, nu, nv, t): (FormV, Value, i64, i64, Option<Table>),
) -> LResult<Vec<FormV>> {
    let f = "pattern_surface";
    let o = Opts::new(f, t, lua)?;
    let align = o.choice("align", "normal", &["normal", "up", "fixed"])?;
    let margin = o.num("margin", 0.0, 0.0, 0.49)?;
    o.done()?;
    if !(1..=2000).contains(&nu) || !(1..=2000).contains(&nv) || nu * nv > 100_000 {
        return err("pattern_surface(form, surface, nu, nv): nu, nv >= 1 and nu * nv <= 100000, e.g. pattern_surface(lamp, roof, 8, 6)");
    }
    let at = surface_at(s, f)?;
    let (nu, nv) = (nu as usize, nv as usize);
    charge(lua, &form, nu * nv, f)?;
    let mut out = Vec::with_capacity(nu * nv);
    for i in 0..nu {
        for j in 0..nv {
            let fr = |k: usize, n: usize| margin + (1.0 - 2.0 * margin) * (k as f64 + 0.5) / n as f64;
            let (u, w) = (fr(i, nu), fr(j, nv));
            let p = at(u, w);
            if !p.finite() {
                return err(format!("{f}: the surface is not finite at u {u:.3}, v {w:.3}"));
            }
            let m = if align == "fixed" { Mat::translate(p) } else { surf_frame(&*at, u, w, &align) };
            out.push(place(f, &form, &m)?);
        }
    }
    Ok(out)
}

/// `pattern_grid(form, {nx, ny, nz, dx, dy, dz})`: copies on a 3D grid from the form's place.
fn pattern_grid(lua: &Lua, (form, t): (FormV, Option<Table>)) -> LResult<Vec<FormV>> {
    let f = "pattern_grid";
    let o = Opts::new(f, t, lua)?;
    let nx = o.int("nx", 1, 1, 100_000)? as usize;
    let ny = o.int("ny", 1, 1, 100_000)? as usize;
    let nz = o.int("nz", 1, 1, 100_000)? as usize;
    let (dx, dy, dz) = (o.len("dx", 0.0)?, o.len("dy", 0.0)?, o.len("dz", 0.0)?);
    o.done()?;
    if nx * ny * nz > 100_000 {
        return err("pattern_grid: nx * ny * nz is at most 100000");
    }
    if nx * ny * nz < 2 {
        return err("pattern_grid{nx = 5, ny = 3, dx = 2, dy = 2}: at least two copies (nx * ny * nz >= 2)");
    }
    charge(lua, &form, nx * ny * nz, f)?;
    let mut out = Vec::with_capacity(nx * ny * nz);
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let d = V3::new(i as f64 * dx, j as f64 * dy, k as f64 * dz);
                out.push(place(f, &form, &Mat::translate(d))?);
            }
        }
    }
    Ok(out)
}
