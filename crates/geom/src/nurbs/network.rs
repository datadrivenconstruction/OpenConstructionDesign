// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Surfaces from a network of curves (Gordon: skin of the u-curves + skin of the v-curves −
//! tensor interpolation of their crossings) and rebuilding a patch on a new control net.
//!
//! The curves of each family are refitted on one common parameterisation first: a piecewise
//! linear map per curve puts every crossing at the mean parameter of its column, the mapped
//! curve is sampled and interpolated (degree `degree`, the same parameters for the whole
//! family, so one knot vector). The three patches are raised to common degrees and knots and
//! summed. The deviation from the input curves is sampled and returned; above `tol` the
//! network is refused.

use std::fmt;

use super::{averaging_knots, interp_hom, Curve, NurbsError, Surface, H};
use crate::math::V3;

/// Samples per crossing interval when a curve is refitted.
const PER_SPAN: usize = 8;
/// Start grid for the nearest pair of points of two curves.
const PAIR_GRID: usize = 48;
const EPS: f64 = 1e-10;
/// Largest control net of a rebuild in either direction.
pub const MAX_REBUILD: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub surface: Surface,
    /// Largest sampled distance between the surface and the input curves.
    pub deviation: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NetworkError {
    Nurbs(NurbsError),
    /// Each family needs two curves or more.
    TooFewCurves,
    /// u-curve `u` and v-curve `v` do not meet: `gap` is their smallest distance.
    Apart { u: usize, v: usize, gap: f64 },
    /// The crossings along curve `curve` of family `family` ('u' or 'v') are out of order.
    Order { family: char, curve: usize },
    /// The surface leaves the input curves by this much (more than the tolerance).
    Deviation(f64),
    /// Rebuild counts outside degree + 1 ..= [`MAX_REBUILD`].
    Count,
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NetworkError::Nurbs(e) => write!(f, "{e}"),
            NetworkError::TooFewCurves => f.write_str("a curve network needs two u-curves and two v-curves or more"),
            NetworkError::Apart { u, v, gap } => {
                write!(f, "u-curve {u} and v-curve {v} do not meet (gap {gap:.4} m)")
            }
            NetworkError::Order { family, curve } => write!(
                f,
                "the crossings along {family}-curve {curve} are not in the order of the other curves"
            ),
            NetworkError::Deviation(d) => {
                write!(f, "the network surface leaves the curves by {d:.4} m")
            }
            NetworkError::Count => write!(
                f,
                "control point counts must be at least degree + 1 and at most {MAX_REBUILD}"
            ),
        }
    }
}

impl std::error::Error for NetworkError {}

impl From<NurbsError> for NetworkError {
    fn from(e: NurbsError) -> Self {
        NetworkError::Nurbs(e)
    }
}

fn norm(c: &Curve, t: f64) -> f64 {
    let (a, b) = c.domain();
    (t - a) / (b - a)
}

fn at(c: &Curve, s: f64) -> V3 {
    let (a, b) = c.domain();
    c.point(a + (b - a) * s.clamp(0.0, 1.0))
}

/// Nearest points of two curves: (normalized t on `c`, normalized s on `d`, distance).
fn nearest_pair(c: &Curve, d: &Curve) -> (f64, f64, f64) {
    let (ca, cb) = c.domain();
    let (da, db) = d.domain();
    let n = PAIR_GRID;
    let cs: Vec<V3> = (0..=n).map(|k| c.point(ca + (cb - ca) * k as f64 / n as f64)).collect();
    let ds: Vec<V3> = (0..=n).map(|k| d.point(da + (db - da) * k as f64 / n as f64)).collect();
    let (mut bi, mut bj, mut bd) = (0, 0, f64::INFINITY);
    for (i, p) in cs.iter().enumerate() {
        for (j, q) in ds.iter().enumerate() {
            let e = (*p - *q).len();
            if e < bd {
                (bi, bj, bd) = (i, j, e);
            }
        }
    }
    let mut t = ca + (cb - ca) * bi as f64 / n as f64;
    let mut s = da + (db - da) * bj as f64 / n as f64;
    for _ in 0..40 {
        let a = c.derivs(t, 2);
        let b = d.derivs(s, 2);
        let f = a[0] - b[0];
        let g = [f.dot(a[1]), -f.dot(b[1])];
        let h11 = a[1].dot(a[1]) + f.dot(a[2]);
        let h12 = -a[1].dot(b[1]);
        let h22 = b[1].dot(b[1]) - f.dot(b[2]);
        let det = h11 * h22 - h12 * h12;
        let (dt, ds_) = if det > 1e-300 && h11 > 0.0 {
            ((h22 * g[0] - h12 * g[1]) / det, (h11 * g[1] - h12 * g[0]) / det)
        } else {
            let k = 1.0 / (a[1].dot(a[1]) + b[1].dot(b[1])).max(1e-300);
            (g[0] * k, g[1] * k)
        };
        let (t2, s2) = ((t - dt).clamp(ca, cb), (s - ds_).clamp(da, db));
        let step = (t2 - t).abs() / (cb - ca) + (s2 - s).abs() / (db - da);
        (t, s) = (t2, s2);
        if step < 1e-15 {
            break;
        }
    }
    let e = (c.point(t) - d.point(s)).len();
    if e <= bd {
        (norm(c, t), norm(d, s), e)
    } else {
        (bi as f64 / n as f64, bj as f64 / n as f64, bd)
    }
}

/// Common sample parameters: the crossing means `xs` (and 0, 1), `PER_SPAN` steps between.
fn samples(xs: &[f64]) -> Vec<f64> {
    let mut br = vec![0.0];
    for &x in xs {
        if x - br[br.len() - 1] > EPS {
            br.push(x);
        }
    }
    if 1.0 - br[br.len() - 1] > EPS {
        br.push(1.0);
    } else {
        let k = br.len() - 1;
        br[k] = 1.0;
    }
    let mut out = vec![0.0];
    for w in br.windows(2) {
        for k in 1..=PER_SPAN {
            out.push(w[0] + (w[1] - w[0]) * k as f64 / PER_SPAN as f64);
        }
    }
    out
}

/// Monotone cubic map from common parameters to the curve's own (normalized) ones.
fn mapper(xs: &[f64], own: &[f64]) -> impl Fn(f64) -> f64 {
    let mut a = vec![(0.0, 0.0)];
    for (&x, &o) in xs.iter().zip(own) {
        if x <= EPS {
            a[0].1 = o;
        } else if x >= 1.0 - EPS {
            a.push((1.0, o));
        } else {
            a.push((x, o));
        }
    }
    if a[a.len() - 1].0 < 1.0 - EPS {
        a.push((1.0, 1.0));
    }
    // Monotone cubic Hermite (Fritsch-Carlson): no slope jump at the crossings, so the rows
    // sampled through the map stay smooth and a cubic through the samples follows them.
    let n = a.len();
    let sec: Vec<f64> = (1..n)
        .map(|k| {
            let dx = a[k].0 - a[k - 1].0;
            if dx <= EPS {
                0.0
            } else {
                (a[k].1 - a[k - 1].1) / dx
            }
        })
        .collect();
    let mut m = vec![0.0; n];
    for k in 1..n.saturating_sub(1) {
        let (s0, s1) = (sec[k - 1], sec[k]);
        if s0 * s1 > 0.0 {
            let (h0, h1) = (a[k].0 - a[k - 1].0, a[k + 1].0 - a[k].0);
            let (w0, w1) = (2.0 * h1 + h0, h1 + 2.0 * h0);
            m[k] = (w0 + w1) / (w0 / s0 + w1 / s1);
        }
    }
    if n >= 2 {
        m[0] = sec[0];
        m[n - 1] = sec[n - 2];
    }
    move |u: f64| {
        let k = (1..n).find(|&k| u <= a[k].0).unwrap_or(n - 1);
        let ((x0, y0), (x1, y1)) = (a[k - 1], a[k]);
        let h = x1 - x0;
        if h <= EPS {
            return y1;
        }
        let t = ((u - x0) / h).clamp(0.0, 1.0);
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * h * m[k - 1]
            + (-2.0 * t3 + 3.0 * t2) * y1
            + (t3 - t2) * h * m[k]
    }
}

/// Interpolates every row of `rows` (points at `params`) with degree `p`: control rows.
fn interp_rows(params: &[f64], p: usize, rows: &[Vec<V3>]) -> Result<(Vec<f64>, Vec<Vec<V3>>), NurbsError> {
    let knots = averaging_knots(params, p);
    let k = rows[0].len();
    // data[sample][row]
    let data: Vec<Vec<H>> = (0..params.len())
        .map(|s| rows.iter().map(|r| H::of(r[s], 1.0)).collect())
        .collect();
    debug_assert!(rows.iter().all(|r| r.len() == k));
    let sol = interp_hom(params, &knots, p, data)?;
    let out = (0..rows.len())
        .map(|r| sol.iter().map(|row| row[r].point()).collect())
        .collect();
    Ok((knots, out))
}

/// Tensor surface from a net `net[i][j]` (i along u).
fn surf(pu: usize, pv: usize, ku: Vec<f64>, kv: Vec<f64>, net: &[Vec<V3>]) -> Result<Surface, NurbsError> {
    let (nu, nv) = (net.len(), net[0].len());
    let ctrl: Vec<V3> = net.iter().flatten().copied().collect();
    Surface::new(pu, pv, ku, kv, nu, nv, ctrl, vec![1.0; nu * nv])
}

/// Raises the u degree to `p`.
fn elevate_u(s: &Surface, p: usize) -> Result<Surface, NurbsError> {
    if s.deg_u() >= p {
        return Ok(s.clone());
    }
    let (nu, nv) = (s.nu(), s.nv());
    let mut cols = vec![];
    for j in 0..nv {
        let pts = (0..nu).map(|i| s.ctrl()[i * nv + j]).collect();
        let ws = (0..nu).map(|i| s.weights()[i * nv + j]).collect();
        let mut c = Curve::new(s.deg_u(), s.knots_u().to_vec(), pts, ws)?;
        while c.degree() < p {
            c = c.elevated();
        }
        cols.push(c);
    }
    let n2 = cols[0].ctrl().len();
    let mut ctrl = Vec::with_capacity(n2 * nv);
    let mut w = Vec::with_capacity(n2 * nv);
    for i in 0..n2 {
        for c in &cols {
            ctrl.push(c.ctrl()[i]);
            w.push(c.weights()[i]);
        }
    }
    Surface::new(p, s.deg_v(), cols[0].knots().to_vec(), s.knots_v().to_vec(), n2, nv, ctrl, w)
}

fn mult(knots: &[f64], u: f64) -> usize {
    knots.iter().filter(|k| (**k - u).abs() <= EPS).count()
}

/// One u degree and one u knot vector for all patches (shapes unchanged).
fn compatible_u(ss: &[Surface]) -> Result<Vec<Surface>, NurbsError> {
    let p = ss.iter().map(|s| s.deg_u()).max().unwrap_or(1);
    let ss = ss.iter().map(|s| elevate_u(s, p)).collect::<Result<Vec<_>, _>>()?;
    let mut merged: Vec<(f64, usize)> = vec![];
    for s in &ss {
        for &u in s.knots_u() {
            if u <= EPS || u >= 1.0 - EPS {
                continue;
            }
            let m = mult(s.knots_u(), u);
            match merged.iter_mut().find(|(v, _)| (*v - u).abs() <= EPS) {
                Some(e) => e.1 = e.1.max(m),
                None => merged.push((u, m)),
            }
        }
    }
    merged.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut rep = vec![0.0; p + 1];
    for &(u, m) in &merged {
        rep.extend(std::iter::repeat_n(u, m));
    }
    rep.extend(std::iter::repeat_n(1.0, p + 1));
    let mut out = vec![];
    for s in ss {
        let mut add = vec![];
        for &(u, m) in &merged {
            let have = mult(s.knots_u(), u);
            let at = s.knots_u().iter().copied().find(|k| (*k - u).abs() <= EPS).unwrap_or(u);
            add.extend(std::iter::repeat_n(at, m - have.min(m)));
        }
        let r = if add.is_empty() { s } else { s.refine_u(&add)? };
        if r.knots_u().len() != rep.len() {
            return Err(NurbsError::Degenerate);
        }
        out.push(Surface::new(
            p,
            r.deg_v(),
            rep.clone(),
            r.knots_v().to_vec(),
            r.nu(),
            r.nv(),
            r.ctrl().to_vec(),
            r.weights().to_vec(),
        )?);
    }
    Ok(out)
}

fn compatible(ss: &[Surface]) -> Result<Vec<Surface>, NurbsError> {
    let u = compatible_u(ss)?;
    let t: Vec<Surface> = u.iter().map(|s| s.transposed()).collect();
    Ok(compatible_u(&t)?.iter().map(|s| s.transposed()).collect())
}

/// Gordon surface through u-curves `us` (in order across v) and v-curves `vs` (in order across
/// u). Every u-curve must meet every v-curve within `tol`.
pub fn network(us: &[Curve], vs: &[Curve], degree: usize, tol: f64) -> Result<Network, NetworkError> {
    let (n, m) = (us.len(), vs.len());
    if n < 2 || m < 2 {
        return Err(NetworkError::TooFewCurves);
    }
    if degree == 0 {
        return Err(NurbsError::BadDegree.into());
    }
    // crossings: t[i][j] on u-curve i, s[i][j] on v-curve j
    let mut t = vec![vec![0.0; m]; n];
    let mut s = vec![vec![0.0; m]; n];
    for i in 0..n {
        for j in 0..m {
            let (a, b, gap) = nearest_pair(&us[i], &vs[j]);
            if gap > tol {
                return Err(NetworkError::Apart { u: i, v: j, gap });
            }
            (t[i][j], s[i][j]) = (a, b);
        }
    }
    for i in 0..n {
        if t[i].windows(2).any(|w| w[1] <= w[0] + EPS) {
            return Err(NetworkError::Order { family: 'u', curve: i });
        }
    }
    for j in 0..m {
        if (1..n).any(|i| s[i][j] <= s[i - 1][j] + EPS) {
            return Err(NetworkError::Order { family: 'v', curve: j });
        }
    }
    let ub: Vec<f64> = (0..m).map(|j| (0..n).map(|i| t[i][j]).sum::<f64>() / n as f64).collect();
    let vb: Vec<f64> = (0..n).map(|i| s[i].iter().sum::<f64>() / m as f64).collect();
    let (uu, vv) = (samples(&ub), samples(&vb));
    let p = degree.min(uu.len() - 1).min(vv.len() - 1);
    let umaps: Vec<_> = (0..n).map(|i| mapper(&ub, &t[i])).collect();
    let vmaps: Vec<_> = (0..m)
        .map(|j| mapper(&vb, &(0..n).map(|i| s[i][j]).collect::<Vec<_>>()))
        .collect();
    let urows: Vec<Vec<V3>> = (0..n)
        .map(|i| uu.iter().map(|&u| at(&us[i], umaps[i](u))).collect())
        .collect();
    let vrows: Vec<Vec<V3>> = (0..m)
        .map(|j| vv.iter().map(|&v| at(&vs[j], vmaps[j](v))).collect())
        .collect();
    let (ku, uc) = interp_rows(&uu, p, &urows)?; // uc[i] control row of u-curve i
    let (kv, vc) = interp_rows(&vv, p, &vrows)?; // vc[j] control row of v-curve j
    let q = degree.min(n - 1);
    let q2 = degree.min(m - 1);
    // skin of the u-curves across v: columns over i at vb
    let cols: Vec<Vec<V3>> = (0..uc[0].len()).map(|k| uc.iter().map(|r| r[k]).collect()).collect();
    let (kv1, l1c) = interp_cols(&vb, q, &cols)?;
    let l1 = surf(p, q, ku.clone(), kv1.clone(), &l1c)?;
    // skin of the v-curves across u: rows over j at ub, net[j'][k]
    let vcols: Vec<Vec<V3>> = (0..vc[0].len()).map(|k| vc.iter().map(|r| r[k]).collect()).collect();
    let (ku2, l2t) = interp_cols(&ub, q2, &vcols)?; // l2t[k][j']
    let l2net: Vec<Vec<V3>> = (0..ub.len()).map(|jj| l2t.iter().map(|r| r[jj]).collect()).collect();
    let l2 = surf(q2, p, ku2.clone(), kv.clone(), &l2net)?;
    // tensor interpolation of the crossings
    let cross: Vec<Vec<V3>> = (0..m)
        .map(|j| {
            (0..n)
                .map(|i| {
                    let a = curve_of(p, &ku, &uc[i]).map(|c| c.point(ub[j]));
                    let b = curve_of(p, &kv, &vc[j]).map(|c| c.point(vb[i]));
                    match (a, b) {
                        (Ok(a), Ok(b)) => Ok((a + b) * 0.5),
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    }
                })
                .collect::<Result<Vec<V3>, NurbsError>>()
        })
        .collect::<Result<_, _>>()?; // cross[j][i]
    let (_, r) = interp_cols(&ub, q2, &transpose(&cross))?; // r[i][j'] after along u
    let (_, tn) = interp_cols(&vb, q, &transpose(&r))?; // tn[j'][i']
    let tnet = tn; // net[j' along u][i' along v]
    let tp = surf(q2, q, ku2, kv1, &tnet)?;
    let ss = compatible(&[l1, l2, tp])?;
    let (a, b, c) = (&ss[0], &ss[1], &ss[2]);
    let ctrl: Vec<V3> = (0..a.ctrl().len())
        .map(|k| a.ctrl()[k] + b.ctrl()[k] - c.ctrl()[k])
        .collect();
    let surface = Surface::new(
        a.deg_u(),
        a.deg_v(),
        a.knots_u().to_vec(),
        a.knots_v().to_vec(),
        a.nu(),
        a.nv(),
        ctrl,
        vec![1.0; a.nu() * a.nv()],
    )?;
    let mut dev: f64 = 0.0;
    let mids = |xs: &[f64]| -> Vec<f64> {
        let mut o: Vec<f64> = xs.to_vec();
        o.extend(xs.windows(2).map(|w| 0.5 * (w[0] + w[1])));
        o
    };
    for i in 0..n {
        for u in mids(&uu) {
            dev = dev.max((surface.point(u, vb[i]) - at(&us[i], umaps[i](u))).len());
        }
    }
    for j in 0..m {
        for v in mids(&vv) {
            dev = dev.max((surface.point(ub[j], v) - at(&vs[j], vmaps[j](v))).len());
        }
    }
    if dev > tol {
        return Err(NetworkError::Deviation(dev));
    }
    Ok(Network { surface, deviation: dev })
}

fn curve_of(p: usize, knots: &[f64], ctrl: &[V3]) -> Result<Curve, NurbsError> {
    Curve::bspline(p, knots.to_vec(), ctrl.to_vec())
}

fn transpose(a: &[Vec<V3>]) -> Vec<Vec<V3>> {
    (0..a[0].len()).map(|k| a.iter().map(|r| r[k]).collect()).collect()
}

/// Interpolates each column `cols[c]` (one point per parameter) with degree `p`:
/// knots and `out[c]` = control points of column c.
fn interp_cols(params: &[f64], p: usize, cols: &[Vec<V3>]) -> Result<(Vec<f64>, Vec<Vec<V3>>), NurbsError> {
    interp_rows(params, p, cols)
}

/// The patch resampled on an `nu` x `nv` control net of degrees `du`, `dv` (polynomial,
/// domain [0, 1] in both directions, the parameters mapped linearly): returns the patch and
/// the largest sampled deviation from the original.
pub fn rebuild(s: &Surface, nu: usize, nv: usize, du: usize, dv: usize) -> Result<Network, NetworkError> {
    if du == 0 || dv == 0 {
        return Err(NurbsError::BadDegree.into());
    }
    if nu < du + 1 || nv < dv + 1 || nu > MAX_REBUILD || nv > MAX_REBUILD {
        return Err(NetworkError::Count);
    }
    let (u0, u1) = s.domain_u();
    let (v0, v1) = s.domain_v();
    let orig = |a: f64, b: f64| s.point(u0 + (u1 - u0) * a, v0 + (v1 - v0) * b);
    let pu: Vec<f64> = (0..nu).map(|k| k as f64 / (nu - 1) as f64).collect();
    let pv: Vec<f64> = (0..nv).map(|k| k as f64 / (nv - 1) as f64).collect();
    // along v for every u sample, then along u
    let rows: Vec<Vec<V3>> = pu.iter().map(|&a| pv.iter().map(|&b| orig(a, b)).collect()).collect();
    let (kv, rv) = interp_rows(&pv, dv, &rows)?; // rv[i][j'] control along v per u sample
    let (ku, ru) = interp_rows(&pu, du, &transpose(&rv))?; // ru[j'][i']
    let net = transpose(&ru); // net[i'][j']
    let out = surf(du, dv, ku, kv, &net)?;
    let mut dev: f64 = 0.0;
    let k = 2 * nu.max(nv);
    for a in 0..=k {
        for b in 0..=k {
            let (x, y) = (a as f64 / k as f64, b as f64 / k as f64);
            dev = dev.max((out.point(x, y) - orig(x, y)).len());
        }
    }
    Ok(Network { surface: out, deviation: dev })
}

#[cfg(test)]
#[path = "tests_network.rs"]
mod tests;
