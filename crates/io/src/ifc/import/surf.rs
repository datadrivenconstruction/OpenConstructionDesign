//! Curved faces of advanced breps on spheres, tori, swept surfaces (revolution, linear extrusion)
//! and B-spline surfaces (plain and rational):
//! the bounded region is triangulated in the surface parameters and refined until chords stay
//! within the curve tolerance (7.5 degree steps). Boundary edges are kept as read so faces stay
//! watertight with their neighbours; any failure leaves the caller to flatten the face.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use crate::ifc::reader::{Entity, Value};
use super::tess::{polyline, sub, Tri};
use super::{add, cross, dot, mul, norm, Frame, Rd, Why, V3};

/// Triangle budget of one curved face; past it the rest is emitted unrefined.
const FACE_TRIS: usize = 1 << 18;
const BAD: Why = "bad surface";
const OFF: Why = "face off its surface";

fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn finite(p: V3) -> bool {
    p.iter().all(|c| c.is_finite())
}
/// `d` brought into [-pi, pi].
fn wrap(d: f64) -> f64 {
    d - TAU * (d / TAU).round()
}
/// Chord tolerance of a surface of characteristic size `size`, as for 7.5 degree arc steps.
fn chord_tol(size: f64) -> f64 {
    size * (1.0 - (PI / 48.0).cos())
}

pub(super) struct Spline {
    du: usize,
    dv: usize,
    nu: usize,
    nv: usize,
    /// Homogeneous control points (x·w, y·w, z·w, w), row `i` (u) major.
    ctrl: Vec<[f64; 4]>,
    ku: Vec<f64>,
    kv: Vec<f64>,
    closed_u: bool,
    closed_v: bool,
    size: f64,
}

pub(super) enum Surf {
    Sphere { fr: Frame, r: f64 },
    Torus { fr: Frame, big: f64, r: f64 },
    Spline(Box<Spline>),
    Sweep(Box<Sweep>),
}

enum Mode {
    /// Rotation about the axis through `o` along unit `k`; angles are measured from `x` towards `y`.
    Rev { o: V3, k: V3, x: V3, y: V3 },
    /// Translation along the unit direction `d` (signed by the depth).
    Lin { d: V3 },
}

/// Surface swept from a sampled profile: parameters (angle or distance, profile arc length).
pub(super) struct Sweep {
    prof: Vec<V3>,
    /// Profile projected to the (radius, height) half plane or along the extrusion.
    key: Vec<V3>,
    cum: Vec<f64>,
    closed: bool,
    mode: Mode,
    /// Length per unit of the first parameter.
    su: f64,
    size: f64,
}

/// `d` brought into [-l/2, l/2].
fn wrap_to(d: f64, l: f64) -> f64 {
    d - l * (d / l).round()
}

impl Sweep {
    fn total(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }
    fn at(&self, s: f64) -> V3 {
        let l = self.total();
        let s = if self.closed { s.rem_euclid(l) } else { s.clamp(0.0, l) };
        let i = self
            .cum
            .partition_point(|c| *c <= s)
            .saturating_sub(1)
            .min(self.prof.len() - 2);
        let w = (s - self.cum[i]) / (self.cum[i + 1] - self.cum[i]);
        add(self.prof[i], mul(sub(self.prof[i + 1], self.prof[i]), w))
    }
    fn eval(&self, u: f64, s: f64) -> V3 {
        let p = self.at(s);
        match &self.mode {
            Mode::Rev { o, k, .. } => {
                let q = sub(p, *o);
                let (c, sn) = (u.cos(), u.sin());
                let r = add(mul(q, c), mul(cross(*k, q), sn));
                add(*o, add(r, mul(*k, dot(*k, q) * (1.0 - c))))
            }
            Mode::Lin { d } => add(p, mul(*d, u)),
        }
    }
    fn key_of(mode: &Mode, p: V3) -> V3 {
        match mode {
            Mode::Rev { o, k, .. } => {
                let q = sub(p, *o);
                let h = dot(*k, q);
                [len(sub(q, mul(*k, h))), h, 0.0]
            }
            Mode::Lin { d } => sub(p, mul(*d, dot(*d, p))),
        }
    }
    /// Angle of `p` about the axis (None on the axis).
    fn angle(&self, p: V3) -> Option<f64> {
        let Mode::Rev { o, k, x, y } = &self.mode else {
            return None;
        };
        let q = sub(p, *o);
        let r = sub(q, mul(*k, dot(*k, q)));
        (len(r) > 1e-7 * self.size).then(|| dot(r, *y).atan2(dot(r, *x)))
    }
    /// Profile arc length nearest to `p` in the projection.
    fn invert(&self, p: V3) -> f64 {
        let q = Self::key_of(&self.mode, p);
        let mut best = (f64::INFINITY, 0.0);
        for i in 0..self.key.len() - 1 {
            let (a, b) = (self.key[i], self.key[i + 1]);
            let ab = sub(b, a);
            let l2 = dot(ab, ab);
            let t = if l2 > 0.0 {
                (dot(sub(q, a), ab) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = len(sub(q, add(a, mul(ab, t))));
            if d < best.0 {
                best = (d, self.cum[i] + t * (self.cum[i + 1] - self.cum[i]));
            }
        }
        best.1
    }

    fn rings(&self, rings: &[Vec<V3>]) -> Result<Vec<UvRing>, Why> {
        let tol = 0.02 * self.size + 1e-9;
        let l = self.total();
        let angular = matches!(self.mode, Mode::Rev { .. });
        let mut out = vec![];
        for ring in rings {
            let mut ss = Vec::with_capacity(ring.len());
            let mut us: Vec<Option<f64>> = Vec::with_capacity(ring.len());
            for p in ring {
                if !finite(*p) {
                    return Err(OFF);
                }
                let s = self.invert(*p);
                ss.push(s);
                let pr = self.at(s);
                us.push(match &self.mode {
                    Mode::Rev { .. } => match (self.angle(*p), self.angle(pr)) {
                        (Some(a), Some(b)) => Some(wrap(a - b)),
                        _ => None,
                    },
                    Mode::Lin { d } => Some(dot(*d, sub(*p, pr))),
                });
            }
            let Some(first) = us.iter().flatten().next().copied() else {
                return Err(OFF);
            };
            let mut uv = UvRing::default();
            let (mut cu, mut ru, mut cs, mut rs) = (first, first, ss[0], ss[0]);
            for i in 0..ring.len() {
                match (angular, us[i]) {
                    (true, Some(r)) => {
                        cu += wrap(r - ru);
                        ru = r;
                    }
                    (false, Some(r)) => cu = r,
                    _ => {}
                }
                if self.closed {
                    cs += wrap_to(ss[i] - rs, l);
                } else {
                    cs = ss[i];
                }
                rs = ss[i];
                if len(sub(self.eval(cu, cs), ring[i])) > tol {
                    return Err(OFF);
                }
                uv.push([cu, cs], Some(ring[i]), true);
            }
            let (u0, s0) = (uv.pts[0][0], uv.pts[0][1]);
            let net_u = if angular {
                cu + wrap(us[0].unwrap_or(ru) - ru) - u0
            } else {
                0.0
            };
            let net_s = if self.closed {
                cs + wrap_to(ss[0] - rs, l) - s0
            } else {
                0.0
            };
            if net_u.abs() > PI || net_s.abs() > l / 2.0 {
                return Err("face winds round its surface");
            }
            out.push(uv);
        }
        Ok(out)
    }
}

/// Boundary ring in surface parameters: `orig` is the read point a vertex snaps back to,
/// `fixed[i]` marks the edge i -> i+1 as a read boundary edge (never split).
#[derive(Default)]
struct UvRing {
    pts: Vec<[f64; 2]>,
    orig: Vec<Option<V3>>,
    fixed: Vec<bool>,
}

impl UvRing {
    fn push(&mut self, q: [f64; 2], o: Option<V3>, fixed: bool) {
        self.pts.push(q);
        self.orig.push(o);
        self.fixed.push(fixed);
    }
}

fn knot_vector(m: &Value, k: &Value, n: usize, p: usize) -> Result<Vec<f64>, Why> {
    let (m, k) = (m.as_list().ok_or(BAD)?, k.as_list().ok_or(BAD)?);
    if m.len() != k.len() {
        return Err(BAD);
    }
    let mut out = vec![];
    for (m, k) in m.iter().zip(k) {
        let (m, k) = (m.as_f64().ok_or(BAD)?, k.as_f64().ok_or(BAD)?);
        if !((1.0..=64.0).contains(&m) && k.is_finite()) {
            return Err(BAD);
        }
        out.extend(std::iter::repeat_n(k, m as usize));
        if out.len() > n + p + 1 {
            return Err(BAD);
        }
    }
    if out.len() != n + p + 1 || out.windows(2).any(|w| w[1] < w[0]) || !(out[n] > out[p]) {
        return Err(BAD);
    }
    Ok(out)
}

/// Knot span of `t` (clamped to the domain) for degree `p` and `n` control points.
fn span(p: usize, n: usize, u: &[f64], t: f64) -> usize {
    let mut k = p;
    while k < n - 1 && u[k + 1] <= t {
        k += 1;
    }
    k
}

/// De Boor on homogeneous points `d` (the p+1 points of span `k`).
fn de_boor4(p: usize, mut d: Vec<[f64; 4]>, u: &[f64], k: usize, t: f64) -> [f64; 4] {
    for r in 1..=p {
        for j in (r..=p).rev() {
            let (lo, hi) = (u[j + k - p], u[j + 1 + k - r]);
            let a = if hi > lo { (t - lo) / (hi - lo) } else { 0.0 };
            let prev = d[j - 1];
            for (x, y) in d[j].iter_mut().zip(prev) {
                *x = (1.0 - a) * y + a * *x;
            }
        }
    }
    d[p]
}

impl Spline {
    fn domain(&self) -> ((f64, f64), (f64, f64)) {
        (
            (self.ku[self.du], self.ku[self.nu]),
            (self.kv[self.dv], self.kv[self.nv]),
        )
    }
    fn eval(&self, u: f64, v: f64) -> V3 {
        let ((u0, u1), (v0, v1)) = self.domain();
        let (u, v) = (u.clamp(u0, u1), v.clamp(v0, v1));
        if !(u.is_finite() && v.is_finite()) {
            return [f64::NAN; 3];
        }
        let ki = span(self.du, self.nu, &self.ku, u);
        let kj = span(self.dv, self.nv, &self.kv, v);
        let rows: Vec<[f64; 4]> = (ki - self.du..=ki)
            .map(|i| {
                let row = &self.ctrl[i * self.nv..(i + 1) * self.nv];
                de_boor4(
                    self.dv,
                    row[kj - self.dv..=kj].to_vec(),
                    &self.kv,
                    kj,
                    v,
                )
            })
            .collect();
        let q = de_boor4(self.du, rows, &self.ku, ki, u);
        if !(q[3].abs() > 1e-300) {
            return [f64::NAN; 3];
        }
        [q[0] / q[3], q[1] / q[3], q[2] / q[3]]
    }
    /// Parameter samples with their points, `g` per direction.
    fn grid(&self, g: usize) -> Vec<(f64, f64, V3)> {
        let ((u0, u1), (v0, v1)) = self.domain();
        let mut out = Vec::with_capacity((g + 1) * (g + 1));
        for i in 0..=g {
            let u = u0 + (u1 - u0) * i as f64 / g as f64;
            for j in 0..=g {
                let v = v0 + (v1 - v0) * j as f64 / g as f64;
                out.push((u, v, self.eval(u, v)));
            }
        }
        out
    }
    /// Closest parameters of `p` (grid start, then Gauss-Newton), with the distance left.
    fn invert(&self, grid: &[(f64, f64, V3)], p: V3) -> (f64, f64, f64) {
        let ((u0, u1), (v0, v1)) = self.domain();
        let (mut u, mut v, mut best) = (u0, v0, f64::INFINITY);
        for &(gu, gv, q) in grid {
            let d = len(sub(q, p));
            if d < best {
                (u, v, best) = (gu, gv, d);
            }
        }
        let (hu, hv) = ((u1 - u0) * 1e-7, (v1 - v0) * 1e-7);
        let (mut bu, mut bv) = (u, v);
        for _ in 0..30 {
            let s = self.eval(u, v);
            let du_h = if u + hu <= u1 { hu } else { -hu };
            let dv_h = if v + hv <= v1 { hv } else { -hv };
            let su = mul(sub(self.eval(u + du_h, v), s), 1.0 / du_h);
            let sv = mul(sub(self.eval(u, v + dv_h), s), 1.0 / dv_h);
            let r = sub(s, p);
            let (a11, a12, a22) = (dot(su, su), dot(su, sv), dot(sv, sv));
            let (b1, b2) = (dot(su, r), dot(sv, r));
            let det = a11 * a22 - a12 * a12;
            if !(det.abs() > 1e-300) || !det.is_finite() {
                break;
            }
            let su_step = ((a22 * b1 - a12 * b2) / det).clamp(-(u1 - u0) / 4.0, (u1 - u0) / 4.0);
            let sv_step = ((a11 * b2 - a12 * b1) / det).clamp(-(v1 - v0) / 4.0, (v1 - v0) / 4.0);
            if !(su_step.is_finite() && sv_step.is_finite()) {
                break;
            }
            u = (u - su_step).clamp(u0, u1);
            v = (v - sv_step).clamp(v0, v1);
            let d = len(sub(self.eval(u, v), p));
            if d < best {
                (bu, bv, best) = (u, v, d);
            }
            if su_step.abs() < hu * 1e-3 && sv_step.abs() < hv * 1e-3 {
                break;
            }
        }
        (bu, bv, best)
    }
}

/// Seam values of a closed direction take the end nearer their last off-seam neighbour.
fn fix_seam(vals: &mut [f64], lo: f64, hi: f64) {
    let eps = (hi - lo) * 1e-6;
    let on = |x: f64| x - lo < eps || hi - x < eps;
    let raw = vals.to_vec();
    let n = raw.len();
    for i in 0..n {
        if !on(raw[i]) {
            continue;
        }
        if let Some(a) = (1..n).map(|d| raw[(i + n - d) % n]).find(|x| !on(*x)) {
            vals[i] = if a - lo < hi - a { lo } else { hi };
        }
    }
}

impl Surf {
    fn eval(&self, u: f64, v: f64) -> V3 {
        match self {
            Surf::Sphere { fr, r } => fr.apply([
                r * v.cos() * u.cos(),
                r * v.cos() * u.sin(),
                r * v.sin(),
            ]),
            Surf::Torus { fr, big, r } => {
                let k = big + r * v.cos();
                fr.apply([k * u.cos(), k * u.sin(), r * v.sin()])
            }
            Surf::Spline(s) => s.eval(u, v),
            Surf::Sweep(s) => s.eval(u, v),
        }
    }
    /// Characteristic size (chord tolerance base).
    fn size(&self) -> f64 {
        match self {
            Surf::Sphere { r, .. } | Surf::Torus { r, .. } => *r,
            Surf::Spline(s) => s.size,
            Surf::Sweep(s) => s.size,
        }
    }

    /// Boundary rings in parameters plus the length per parameter unit in u and v.
    fn rings_uv(&self, rings: &[Vec<V3>], sense: bool) -> Result<(Vec<UvRing>, f64, f64), Why> {
        match self {
            Surf::Sphere { fr, r } => Ok((sphere_rings(fr, *r, rings, sense)?, *r, *r)),
            Surf::Torus { fr, big, r } => Ok((torus_rings(fr, *big, *r, rings)?, *big, *r)),
            Surf::Spline(s) => spline_rings(s, rings),
            Surf::Sweep(s) => Ok((s.rings(rings)?, s.su, 1.0)),
        }
    }
}

fn local(fr: &Frame, p: V3) -> V3 {
    let q = sub(p, fr.o);
    [dot(q, fr.x), dot(q, fr.y), dot(q, fr.z)]
}

/// Sphere rings in (longitude, latitude). A vertex on a pole becomes a pole edge running the
/// way the region lies; a ring wound once round the axis is closed over the pole on its side.
fn sphere_rings(fr: &Frame, r: f64, rings: &[Vec<V3>], sense: bool) -> Result<Vec<UvRing>, Why> {
    let mut out = vec![];
    let mut wound = 0;
    for ring in rings {
        let n = ring.len();
        let mut loc = Vec::with_capacity(n);
        for p in ring {
            let q = local(fr, *p);
            if !finite(q) || (len(q) - r).abs() > 0.02 * r + 1e-9 {
                return Err(OFF);
            }
            let h = q[0].hypot(q[1]);
            let lon = if h > 1e-7 * r {
                q[1].atan2(q[0])
            } else {
                f64::NAN
            };
            loc.push((lon, q[2].atan2(h)));
        }
        let Some(start) = loc.iter().position(|q| q.0.is_finite()) else {
            return Err(OFF);
        };
        let mut uv = UvRing::default();
        let (mut cur, mut raw) = (loc[start].0, loc[start].0);
        for k in 0..n {
            let i = (start + k) % n;
            let (lon, lat) = loc[i];
            if lon.is_finite() {
                cur += wrap(lon - raw);
                raw = lon;
                uv.push([cur, lat], Some(ring[i]), true);
                continue;
            }
            let next = (1..=n)
                .map(|d| loc[(i + d) % n].0)
                .find(|l| l.is_finite())
                .unwrap_or(raw);
            uv.push([cur, lat], Some(ring[i]), false);
            // region on the left in (lon, lat) iff the face follows the surface normal
            cur += if (lat > 0.0) == sense {
                -(raw - next).rem_euclid(TAU)
            } else {
                (next - raw).rem_euclid(TAU)
            };
            raw = next;
            uv.push([cur, lat], Some(ring[i]), true);
        }
        let first = uv.pts[0][0];
        let close = cur + wrap(loc[start].0 - raw);
        let net = close - first;
        if net.abs() > PI {
            wound += 1;
            if wound > 1 {
                return Err("face winds round its surface");
            }
            let pole = if (net > 0.0) == sense {
                FRAC_PI_2
            } else {
                -FRAC_PI_2
            };
            let lat0 = uv.pts[0][1];
            uv.push([close, lat0], Some(ring[start]), false);
            uv.push([close, pole], None, false);
            uv.push([first, pole], None, false);
        }
        out.push(uv);
    }
    Ok(out)
}

/// Torus rings in (angle round the axis, angle round the tube); both must close.
fn torus_rings(fr: &Frame, big: f64, r: f64, rings: &[Vec<V3>]) -> Result<Vec<UvRing>, Why> {
    let mut out = vec![];
    for ring in rings {
        let mut uv = UvRing::default();
        let mut prev: Option<(f64, f64, f64, f64)> = None;
        for p in ring {
            let q = local(fr, *p);
            let h = q[0].hypot(q[1]);
            let rho = h - big;
            if !finite(q) || h < 1e-9 * big || (rho.hypot(q[2]) - r).abs() > 0.02 * r + 1e-9 {
                return Err(OFF);
            }
            let (a, b) = (q[1].atan2(q[0]), q[2].atan2(rho));
            let (u, v) = match prev {
                Some((pu, pv, pa, pb)) => (pu + wrap(a - pa), pv + wrap(b - pb)),
                None => (a, b),
            };
            prev = Some((u, v, a, b));
            uv.push([u, v], Some(*p), true);
        }
        if let (Some((u, v, a, b)), Some(f)) = (prev, uv.pts.first()) {
            let (cu, cv) = (u + wrap(f[0] - a) - f[0], v + wrap(f[1] - b) - f[1]);
            // `f` is the raw first angle pair, so the closing offsets are the net windings
            if cu.abs() > PI || cv.abs() > PI {
                return Err("face winds round its surface");
            }
        }
        out.push(uv);
    }
    Ok(out)
}

fn spline_rings(s: &Spline, rings: &[Vec<V3>]) -> Result<(Vec<UvRing>, f64, f64), Why> {
    let ((u0, u1), (v0, v1)) = s.domain();
    let g = (s.nu.max(s.nv) * 4).clamp(8, 48);
    let grid = s.grid(g);
    if grid.iter().any(|q| !finite(q.2)) {
        return Err("surface does not evaluate");
    }
    // length per parameter unit, averaged over grid lines
    let at = |i: usize, j: usize| grid[i * (g + 1) + j].2;
    let (mut lu, mut lv) = (0.0, 0.0);
    for i in 0..=g {
        for j in 0..g {
            lu += len(sub(at(j + 1, i), at(j, i)));
            lv += len(sub(at(i, j + 1), at(i, j)));
        }
    }
    let su = lu / (g + 1) as f64 / (u1 - u0);
    let sv = lv / (g + 1) as f64 / (v1 - v0);
    if !(su > 1e-12 && sv > 1e-12 && su.is_finite() && sv.is_finite()) {
        return Err(BAD);
    }
    let fit = 0.01 * s.size + 1e-9;
    let mut out = vec![];
    for ring in rings {
        let (mut us, mut vs) = (vec![], vec![]);
        for p in ring {
            let (u, v, d) = s.invert(&grid, *p);
            if !(d <= fit) {
                return Err(OFF);
            }
            us.push(u);
            vs.push(v);
        }
        if s.closed_u {
            fix_seam(&mut us, u0, u1);
        }
        if s.closed_v {
            fix_seam(&mut vs, v0, v1);
        }
        let n = ring.len();
        for i in 0..n {
            let j = (i + 1) % n;
            if (s.closed_u && (us[j] - us[i]).abs() > 0.5 * (u1 - u0))
                || (s.closed_v && (vs[j] - vs[i]).abs() > 0.5 * (v1 - v0))
            {
                return Err("face crosses the seam");
            }
        }
        let mut uv = UvRing::default();
        for i in 0..n {
            uv.push([us[i], vs[i]], Some(ring[i]), true);
        }
        out.push(uv);
    }
    Ok((out, su, sv))
}

type Key = [u64; 2];
fn key(q: V3) -> Key {
    [q[0].to_bits(), q[1].to_bits()]
}
fn edge_key(a: Key, b: Key) -> (Key, Key) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}
fn mid(a: V3, b: V3) -> V3 {
    mul(add(a, b), 0.5)
}

/// Mesh of a face on `s` bounded by `rings` (outer first). Errors leave flattening to the caller.
pub(super) fn surface_face(s: &Surf, rings: &[Vec<V3>], sense: bool) -> Result<Tri, Why> {
    let (uv, su, sv) = s.rings_uv(rings, sense)?;
    let size = s.size();
    if !(size > 0.0 && size.is_finite()) {
        return Err(BAD);
    }
    let mut snap: HashMap<Key, V3> = HashMap::new();
    let mut fixed: HashSet<(Key, Key)> = HashSet::new();
    let mut flat_rings: Vec<Vec<V3>> = vec![];
    for (ri, r) in uv.iter().enumerate() {
        let mut pts: Vec<(V3, Option<V3>, bool)> = vec![];
        for i in 0..r.pts.len() {
            let q = [r.pts[i][0] * su, r.pts[i][1] * sv, 0.0];
            if !finite(q) {
                return Err(BAD);
            }
            if let Some(l) = pts.last_mut() {
                if l.0 == q {
                    *l = (q, r.orig[i].or(l.1), r.fixed[i]);
                    continue;
                }
            }
            pts.push((q, r.orig[i], r.fixed[i]));
        }
        while pts.len() > 1 && pts[0].0 == pts[pts.len() - 1].0 {
            if let Some(l) = pts.pop() {
                pts[0].1 = pts[0].1.or(l.1);
            }
        }
        if pts.len() < 3 {
            if ri == 0 {
                return Err("face does not triangulate");
            }
            continue;
        }
        for (i, p) in pts.iter().enumerate() {
            if let Some(o) = p.1 {
                snap.entry(key(p.0)).or_insert(o);
            }
            if p.2 {
                fixed.insert(edge_key(key(p.0), key(pts[(i + 1) % pts.len()].0)));
            }
        }
        flat_rings.push(pts.into_iter().map(|p| p.0).collect());
    }
    let mut flat = Tri::default();
    flat.poly(&flat_rings);
    if flat.t.is_empty() {
        return Err("face does not triangulate");
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in &flat.p {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let min_len = (hi[0] - lo[0]).max(hi[1] - lo[1]) / 128.0;
    let tol = chord_tol(size);
    let at = |q: V3| s.eval(q[0] / su, q[1] / sv);
    let split = |a: V3, b: V3| -> Result<bool, Why> {
        if len(sub(a, b)) <= min_len || fixed.contains(&edge_key(key(a), key(b))) {
            return Ok(false);
        }
        let (pa, pb, pm) = (at(a), at(b), at(mid(a, b)));
        if !(finite(pa) && finite(pb) && finite(pm)) {
            return Err("surface does not evaluate");
        }
        Ok(len(sub(pm, mid(pa, pb))) > tol)
    };
    let mut m = Tri::default();
    let emit = |t: [V3; 3], m: &mut Tri| -> Result<(), Why> {
        let v = t.map(|q| snap.get(&key(q)).copied().unwrap_or_else(|| at(q)));
        if !v.iter().all(|p| finite(*p)) {
            return Err("surface does not evaluate");
        }
        m.tri(v[0], v[1], v[2]);
        Ok(())
    };
    let mut stack: Vec<[V3; 3]> = flat
        .t
        .iter()
        .rev()
        .map(|t| t.map(|i| flat.p[i as usize]))
        .collect();
    // edge decisions depend on the edge alone, so neighbours split alike and stay conforming
    while let Some(t) = stack.pop() {
        if m.t.len() + stack.len() >= FACE_TRIS {
            emit(t, &mut m)?;
            continue;
        }
        let sp = [split(t[0], t[1])?, split(t[1], t[2])?, split(t[2], t[0])?];
        let e = |i: usize| mid(t[i % 3], t[(i + 1) % 3]);
        match sp.iter().filter(|x| **x).count() {
            0 => emit(t, &mut m)?,
            1 => {
                let i = sp.iter().position(|x| *x).unwrap_or(0);
                let (a, b, c, ab) = (t[i], t[(i + 1) % 3], t[(i + 2) % 3], e(i));
                stack.extend([[a, ab, c], [ab, b, c]]);
            }
            2 => {
                let i = (0..3).find(|&i| sp[i] && sp[(i + 1) % 3]).unwrap_or(0);
                let (a, b, c) = (t[i], t[(i + 1) % 3], t[(i + 2) % 3]);
                let (ab, bc) = (e(i), e(i + 1));
                stack.extend([[ab, b, bc], [a, ab, bc], [a, bc, c]]);
            }
            _ => {
                let (ab, bc, ca) = (e(0), e(1), e(2));
                stack.extend([[t[0], ab, ca], [ab, t[1], bc], [ca, bc, t[2]], [ab, bc, ca]]);
            }
        }
    }
    Ok(m)
}

impl Rd<'_> {
    /// Evaluable surface of a face, or why not.
    pub(super) fn surface(&self, id: u32) -> Result<Surf, Why> {
        let e = self.ent(id, &[])?;
        let pos = |i: usize| -> Result<f64, Why> {
            let v = Self::arg(e, i).as_f64().ok_or(BAD)? * self.scale;
            if v > 0.0 && v.is_finite() {
                Ok(v)
            } else {
                Err(BAD)
            }
        };
        match self.f.type_of(id).unwrap_or("") {
            "IFCSPHERICALSURFACE" => Ok(Surf::Sphere {
                fr: self.axis_placement(Self::arg(e, 0))?,
                r: pos(1)?,
            }),
            "IFCTOROIDALSURFACE" => {
                let (big, r) = (pos(1)?, pos(2)?);
                if r >= big {
                    return Err(BAD);
                }
                Ok(Surf::Torus {
                    fr: self.axis_placement(Self::arg(e, 0))?,
                    big,
                    r,
                })
            }
            "IFCBSPLINESURFACEWITHKNOTS" => Ok(Surf::Spline(Box::new(self.spline(e, false)?))),
            "IFCRATIONALBSPLINESURFACEWITHKNOTS" => {
                Ok(Surf::Spline(Box::new(self.spline(e, true)?)))
            }
            "IFCSURFACEOFREVOLUTION" => Ok(Surf::Sweep(Box::new(self.sweep(e, false)?))),
            "IFCSURFACEOFLINEAREXTRUSION" => Ok(Surf::Sweep(Box::new(self.sweep(e, true)?))),
            _ => Err("unsupported surface"),
        }
    }

    /// Swept surface over the profile curve of the swept-surface entity `e`.
    fn sweep(&self, e: &Entity, linear: bool) -> Result<Sweep, Why> {
        let pid = Self::reference(e, 0)?;
        let pe = self.ent(pid, &[])?;
        let closed_def = match self.f.type_of(pid).unwrap_or("") {
            "IFCARBITRARYCLOSEDPROFILEDEF" => true,
            "IFCARBITRARYOPENPROFILEDEF" | "IFCCENTERLINEPROFILEDEF" => false,
            _ => return Err("unsupported swept curve"),
        };
        let fr = self.axis_placement(Self::arg(e, 1))?;
        let mut prof: Vec<V3> = polyline(&self.segs(Self::reference(pe, 2)?, 0)?)
            .into_iter()
            .map(|p| fr.apply(p))
            .collect();
        if closed_def && prof.len() > 1 && len(sub(prof[0], prof[prof.len() - 1])) >= 1e-9 {
            prof.push(prof[0]);
        }
        if prof.len() < 2 || !prof.iter().all(|p| finite(*p)) {
            return Err(BAD);
        }
        let closed = prof.len() > 2 && len(sub(prof[0], prof[prof.len() - 1])) < 1e-9;
        let mut cum = vec![0.0];
        for w in prof.windows(2) {
            cum.push(cum[cum.len() - 1] + len(sub(w[1], w[0])));
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &prof {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let size = len(sub(hi, lo));
        if !(size > 0.0 && size.is_finite()) {
            return Err(BAD);
        }
        let mode = if linear {
            let d = self.direction(Self::arg(e, 2))?.ok_or(BAD)?;
            let depth = Self::arg(e, 3).as_f64().ok_or(BAD)?;
            if !(depth != 0.0 && depth.is_finite()) {
                return Err(BAD);
            }
            let d = norm(fr.dir(d)).ok_or(BAD)?;
            Mode::Lin {
                d: mul(d, depth.signum()),
            }
        } else {
            let ax = self.ent(Self::reference(e, 2)?, &["IFCAXIS1PLACEMENT"])?;
            let o = fr.apply(self.point(Self::reference(ax, 0)?)?);
            let k = self.direction(Self::arg(ax, 1))?.unwrap_or([0.0, 0.0, 1.0]);
            let k = norm(fr.dir(k)).ok_or(BAD)?;
            let perp = |c: V3| norm(sub(c, mul(k, dot(k, c))));
            let x = perp(fr.x).or_else(|| perp(fr.y)).ok_or(BAD)?;
            Mode::Rev {
                o,
                k,
                x,
                y: cross(k, x),
            }
        };
        let key: Vec<V3> = prof.iter().map(|p| Sweep::key_of(&mode, *p)).collect();
        let su = if linear {
            1.0
        } else {
            key.iter().map(|q| q[0]).fold(0.0, f64::max)
        };
        if !(su > 0.0 && su.is_finite()) {
            return Err(BAD);
        }
        Ok(Sweep {
            prof,
            key,
            cum,
            closed,
            mode,
            su,
            size,
        })
    }

    fn spline(&self, e: &Entity, rational: bool) -> Result<Spline, Why> {
        let deg = |i: usize| -> Result<usize, Why> {
            let d = Self::arg(e, i).as_f64().ok_or(BAD)?;
            if (1.0..=16.0).contains(&d) && d.fract() == 0.0 {
                Ok(d as usize)
            } else {
                Err(BAD)
            }
        };
        let (du, dv) = (deg(0)?, deg(1)?);
        let rows = Self::arg(e, 2).as_list().ok_or(BAD)?;
        let nu = rows.len();
        let nv = rows.first().and_then(Value::as_list).map_or(0, <[Value]>::len);
        if nu <= du || nv <= dv || nu * nv > 1 << 16 {
            return Err(BAD);
        }
        let weights: &[Value] = if rational {
            Self::arg(e, 12).as_list().ok_or(BAD)?
        } else {
            &[]
        };
        if rational && weights.len() != nu {
            return Err(BAD);
        }
        let mut ctrl = Vec::with_capacity(nu * nv);
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for (i, row) in rows.iter().enumerate() {
            let row = row.as_list().filter(|r| r.len() == nv).ok_or(BAD)?;
            let wrow: &[Value] = if rational {
                weights[i].as_list().filter(|r| r.len() == nv).ok_or(BAD)?
            } else {
                &[]
            };
            for (j, c) in row.iter().enumerate() {
                let p = self.point(c.as_ref().ok_or(BAD)?)?;
                let w = if rational {
                    wrow[j].as_f64().ok_or(BAD)?
                } else {
                    1.0
                };
                if !(w > 0.0 && w.is_finite() && finite(p)) {
                    return Err(BAD);
                }
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
                ctrl.push([p[0] * w, p[1] * w, p[2] * w, w]);
            }
        }
        let ku = knot_vector(Self::arg(e, 7), Self::arg(e, 9), nu, du)?;
        let kv = knot_vector(Self::arg(e, 8), Self::arg(e, 10), nv, dv)?;
        let size = len(sub(hi, lo));
        if !(size > 0.0 && size.is_finite()) {
            return Err(BAD);
        }
        Ok(Spline {
            du,
            dv,
            nu,
            nv,
            ctrl,
            ku,
            kv,
            closed_u: Self::arg(e, 4).as_enum() == Some("T"),
            closed_v: Self::arg(e, 5).as_enum() == Some("T"),
            size,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    const HEAD: &str = "#1=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#2=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#6=IFCUNITASSIGNMENT((#1,#2));
#7=IFCPROJECT('0000000000000000000P01',$,'P',$,$,$,$,(#13),#6);
#8=IFCCARTESIANPOINT((0.,0.,0.));
#9=IFCAXIS2PLACEMENT3D(#8,$,$);
#10=IFCLOCALPLACEMENT($,#9);
#11=IFCBUILDINGSTOREY('0000000000000000000S01',$,'L1',$,$,#10,$,$,.ELEMENT.,0.);
#12=IFCDIRECTION((0.,1.));
#13=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-05,#9,#12);
";

    struct B {
        s: String,
        n: u32,
        products: Vec<u32>,
    }

    impl B {
        fn new() -> B {
            B {
                s: String::new(),
                n: 100,
                products: vec![],
            }
        }
        fn add(&mut self, e: &str) -> u32 {
            self.n += 1;
            writeln!(self.s, "#{}={};", self.n, e).unwrap();
            self.n
        }
        fn point(&mut self, p: [f64; 3]) -> u32 {
            self.add(&format!("IFCCARTESIANPOINT(({:?},{:?},{:?}))", p[0], p[1], p[2]))
        }
        /// Product whose body is one advanced face with `bound` on `surface`.
        fn face_product(&mut self, bound: u32, surface: u32) {
            let f = self.add(&format!("IFCADVANCEDFACE((#{bound}),#{surface},.T.)"));
            let sh = self.add(&format!("IFCOPENSHELL((#{f}))"));
            let sm = self.add(&format!("IFCSHELLBASEDSURFACEMODEL((#{sh}))"));
            let rep = self.add(&format!(
                "IFCSHAPEREPRESENTATION(#13,'Body','SurfaceModel',(#{sm}))"
            ));
            let pds = self.add(&format!("IFCPRODUCTDEFINITIONSHAPE($,$,(#{rep}))"));
            let lp = self.add("IFCLOCALPLACEMENT(#10,#9)");
            let g = format!("'{:0>22}'", self.products.len() + 1);
            let id = self.add(&format!(
                "IFCFURNISHINGELEMENT({g},$,'f',$,$,#{lp},#{pds},$,$)"
            ));
            self.products.push(id);
        }
        /// Upper hemisphere of radius 1 bounded by its equator.
        fn hemisphere(&mut self, radius: &str) {
            let p = self.point([1.0, 0.0, 0.0]);
            let v = self.add(&format!("IFCVERTEXPOINT(#{p})"));
            let c = self.add("IFCCIRCLE(#9,1.)");
            let ec = self.add(&format!("IFCEDGECURVE(#{v},#{v},#{c},.T.)"));
            let oe = self.add(&format!("IFCORIENTEDEDGE(*,*,#{ec},.T.)"));
            let lp = self.add(&format!("IFCEDGELOOP((#{oe}))"));
            let b = self.add(&format!("IFCFACEOUTERBOUND(#{lp},.T.)"));
            let s = self.add(&format!("IFCSPHERICALSURFACE(#9,{radius})"));
            self.face_product(b, s);
        }
        /// Biquadratic patch over [0,2]^2 with the middle control point raised to z = 1.
        fn patch(&mut self, umults: &str) {
            let mut rows = vec![];
            for i in 0..3 {
                let mut row = vec![];
                for j in 0..3 {
                    let z = if i == 1 && j == 1 { 1.0 } else { 0.0 };
                    row.push(format!("#{}", self.point([i as f64, j as f64, z])));
                }
                rows.push(format!("({})", row.join(",")));
            }
            let s = self.add(&format!(
                "IFCBSPLINESURFACEWITHKNOTS(2,2,({}),.UNSPECIFIED.,.F.,.F.,.F.,{umults},(3,3),(0.,1.),(0.,1.),.UNSPECIFIED.)",
                rows.join(",")
            ));
            let c: Vec<String> = [[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]
                .iter()
                .map(|q| format!("#{}", self.point([q[0], q[1], 0.0])))
                .collect();
            let lp = self.add(&format!("IFCPOLYLOOP(({}))", c.join(",")));
            let b = self.add(&format!("IFCFACEOUTERBOUND(#{lp},.T.)"));
            self.face_product(b, s);
        }
        /// Product with one advanced face on `surface` bounded by a polyloop through `pts`.
        fn loop_face(&mut self, pts: &[[f64; 3]], surface: u32) {
            let ids: Vec<String> = pts
                .iter()
                .map(|q| {
                    let r = |x: f64| (x * 1e9).round() / 1e9 + 0.0;
                    format!("#{}", self.point([r(q[0]), r(q[1]), r(q[2])]))
                })
                .collect();
            let lp = self.add(&format!("IFCPOLYLOOP(({}))", ids.join(",")));
            let b = self.add(&format!("IFCFACEOUTERBOUND(#{lp},.T.)"));
            self.face_product(b, surface);
        }
        /// Profile `IFCARBITRARYOPENPROFILEDEF` over the upper half of a circle of radius
        /// `r` centred at (`cx`, 0) in the profile plane.
        fn half_circle_profile(&mut self, cx: f64, r: f64) -> u32 {
            let c = self.add(&format!("IFCCARTESIANPOINT(({cx:?},0.))"));
            let pl = self.add(&format!("IFCAXIS2PLACEMENT2D(#{c},$)"));
            let ci = self.add(&format!("IFCCIRCLE(#{pl},{r:?})"));
            let tc = self.add(&format!(
                "IFCTRIMMEDCURVE(#{ci},(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(3.141592653589793)),.T.,.PARAMETER.)"
            ));
            self.add(&format!("IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#{tc})"))
        }
        fn finish(&self) -> String {
            let l: Vec<String> = self.products.iter().map(|i| format!("#{i}")).collect();
            format!(
                "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('t.ifc','2026-01-01T00:00:00',(''),(''),'','','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{HEAD}{}#99=IFCRELCONTAINEDINSPATIALSTRUCTURE('0000000000000000000R99',$,$,$,({}),#11);\nENDSEC;\nEND-ISO-10303-21;\n",
                self.s,
                l.join(",")
            )
        }
    }

    fn area(m: &super::super::PMesh) -> f64 {
        m.indices
            .chunks(3)
            .map(|t| {
                let p = |i: u32| m.positions[i as usize];
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                super::len(super::super::cross(super::sub(b, a), super::sub(c, a))) / 2.0
            })
            .sum()
    }

    fn flattened(p: &super::super::Plan) -> usize {
        p.report
            .approximated
            .iter()
            .filter(|(k, _)| k.ends_with("curved face flattened"))
            .map(|(_, n)| *n)
            .sum()
    }

    #[test]
    fn advanced_face_sphere_area_within_2pct() {
        let mut b = B::new();
        b.hemisphere("1.");
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(flattened(&p), 0, "{:?}", p.report.approximated);
        let m = &p.meshes[0];
        let a = area(m);
        assert!((a / super::TAU - 1.0).abs() < 0.02, "area {a}");
        let zmax = m.positions.iter().map(|q| q[2]).fold(f64::MIN, f64::max);
        assert!((zmax - 1.0).abs() < 0.01, "dome top {zmax}");
        for q in &m.positions {
            let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
            assert!((r - 1.0).abs() < 1e-9 && q[2] > -1e-9, "{q:?}");
        }
        // same input, same mesh
        let p2 = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p2.meshes[0].positions, m.positions);
        assert_eq!(p2.meshes[0].indices, m.indices);
    }
    #[test]
    fn bspline_surface_face_not_flat() {
        let mut b = B::new();
        b.patch("(3,3)");
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(flattened(&p), 0, "{:?}", p.report.approximated);
        let m = &p.meshes[0];
        assert!(m.indices.len() / 3 > 2);
        let zmax = m.positions.iter().map(|q| q[2]).fold(f64::MIN, f64::max);
        // the patch centre lies at z = 1/4
        assert!(zmax > 0.2 && zmax <= 0.25 + 1e-9, "zmax {zmax}");
        assert!(area(m) > 4.0, "a raised patch has more area than its square");
    }

    #[test]
    fn bad_surface_falls_back_flattened() {
        let mut b = B::new();
        b.patch("(2,3)");
        b.hemisphere("-1.");
        b.hemisphere("0.");
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 3, "{:?}", p.report);
        assert_eq!(flattened(&p), 3, "{:?}", p.report.approximated);
        assert!(p
            .report
            .approximated
            .keys()
            .any(|k| k.ends_with(super::BAD)));
        for m in &p.meshes {
            assert!(m.positions.iter().all(|q| q[2].abs() < 1e-9));
            assert!(!m.indices.is_empty());
        }
        assert!((area(&p.meshes[0]) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn surface_of_revolution_quarter_torus_area() {
        let (pi, h) = (super::PI, super::FRAC_PI_2);
        let mut b = B::new();
        let prof = b.half_circle_profile(1.0, 0.25);
        // profile plane = XZ, local y = world Z; the axis is local y through the origin
        let dz = b.add("IFCDIRECTION((0.,-1.,0.))");
        let dx = b.add("IFCDIRECTION((1.,0.,0.))");
        let pos = b.add(&format!("IFCAXIS2PLACEMENT3D(#8,#{dz},#{dx})"));
        let dy = b.add("IFCDIRECTION((0.,1.,0.))");
        let ax = b.add(&format!("IFCAXIS1PLACEMENT(#8,#{dy})"));
        let s = b.add(&format!("IFCSURFACEOFREVOLUTION(#{prof},#{pos},#{ax})"));
        // region u in [0, pi/2], upper half of the tube
        let n = 12;
        let f = |i: usize| i as f64 / n as f64;
        let mut ring = vec![];
        for i in 0..n {
            let u = h * f(i);
            ring.push([1.25 * u.cos(), 1.25 * u.sin(), 0.0]);
        }
        for i in 0..n {
            let a = pi * f(i);
            ring.push([0.0, 1.0 + 0.25 * a.cos(), 0.25 * a.sin()]);
        }
        for i in 0..n {
            let u = h - h * f(i);
            ring.push([0.75 * u.cos(), 0.75 * u.sin(), 0.0]);
        }
        for i in 0..n {
            let a = pi - pi * f(i);
            ring.push([1.0 + 0.25 * a.cos(), 0.0, 0.25 * a.sin()]);
        }
        b.loop_face(&ring, s);
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(flattened(&p), 0, "{:?}", p.report.approximated);
        let m = &p.meshes[0];
        let want = pi * pi / 8.0;
        assert!((area(m) / want - 1.0).abs() < 0.03, "area {} vs {want}", area(m));
        for q in &m.positions {
            let d = ((q[0].hypot(q[1]) - 1.0).hypot(q[2]) - 0.25).abs();
            assert!(d < 0.003 && q[2] > -1e-9, "{q:?}");
        }
    }

    #[test]
    fn linear_extrusion_arc_face_not_flat() {
        let mut b = B::new();
        let prof = b.half_circle_profile(0.0, 1.0);
        let dz = b.add("IFCDIRECTION((0.,0.,1.))");
        let s = b.add(&format!("IFCSURFACEOFLINEAREXTRUSION(#{prof},$,#{dz},2.)"));
        // sparse boundary: the arc interior must come from the surface, not from the ring
        let ring = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [-1.0, 0.0, 2.0],
            [0.0, 1.0, 2.0],
            [1.0, 0.0, 2.0],
        ];
        b.loop_face(&ring, s);
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(flattened(&p), 0, "{:?}", p.report.approximated);
        let m = &p.meshes[0];
        assert!(m.positions.len() > ring.len(), "{}", m.positions.len());
        for q in &m.positions {
            let r = q[0].hypot(q[1]);
            assert!(r > 0.99 && r < 1.0 + 1e-9, "{q:?}");
            assert!(q[2] > -1e-9 && q[2] < 2.0 + 1e-9 && q[1] > -1e-9, "{q:?}");
        }
    }

    #[test]
    fn unknown_swept_curve_keeps_flatten_note() {
        let mut b = B::new();
        let o = b.point([0.0, 0.0, 0.0]);
        let d = b.add("IFCDIRECTION((1.,0.,0.))");
        let v = b.add(&format!("IFCVECTOR(#{d},1.)"));
        let line = b.add(&format!("IFCLINE(#{o},#{v})"));
        let prof = b.add(&format!("IFCARBITRARYOPENPROFILEDEF(.CURVE.,$,#{line})"));
        let dz = b.add("IFCDIRECTION((0.,0.,1.))");
        let s = b.add(&format!("IFCSURFACEOFLINEAREXTRUSION(#{prof},$,#{dz},1.)"));
        let ring = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ];
        b.loop_face(&ring, s);
        let p = super::super::plan_bytes(b.finish().as_bytes()).unwrap();
        assert_eq!(p.meshes.len(), 1, "{:?}", p.report);
        assert_eq!(flattened(&p), 1, "{:?}", p.report.approximated);
        assert!(p
            .report
            .approximated
            .keys()
            .any(|k| k.ends_with("unsupported curve")));
        assert!(!p.meshes[0].indices.is_empty());
    }
}
