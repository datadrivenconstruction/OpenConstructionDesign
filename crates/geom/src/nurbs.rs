//! Own NURBS curves and surfaces (AI_NATIVE.md section 9.2).
//!
//! Curves: de Boor evaluation, rational derivatives of any order, knot insertion (Boehm),
//! exact degree elevation, global interpolation, exact circular arcs. Surfaces: tensor
//! evaluation, rational partial derivatives, normals with pole fallback, skinning (loft),
//! ruled and revolved construction. Tessellation and thickening live in `tess`.
//!
//! Conventions: knot vectors are clamped (end multiplicity degree + 1, interior at most
//! degree); weights are finite and positive; every algorithm runs on homogeneous points so
//! rational shapes stay exact.

use crate::math::{v3, V3};
use std::f64::consts::{FRAC_PI_2, PI, TAU};

mod build;
mod curve_query;
mod free;
mod network;
mod offset;
mod shell;
mod surface;
mod tess;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_free;
mod trim;

pub use build::{loft, loft_through, revolve, ruled, Loft};
pub use free::{clamped_knots, greville, Closest};
pub use network::{network, rebuild, Network, NetworkError, MAX_REBUILD};
pub use offset::{check_offset, offset, Offset};
pub use shell::{dome, layer_offsets, vault};
pub use surface::Surface;
pub use tess::{
    boolean_on_tessellation, tessellate, tessellate_on_grid, thicken, thicken_between, CurveTess,
    Loss, SurfaceMesh, TessTol, Thick,
};
pub use trim::{tessellate_trimmed, Trim};

#[derive(Clone, Debug, PartialEq)]
pub enum NurbsError {
    BadDegree,
    /// Knot count must be control count + degree + 1.
    KnotCount {
        expected: usize,
        got: usize,
    },
    /// Knots must be finite and non-decreasing with a non-empty domain.
    BadKnots,
    /// End multiplicity must be degree + 1 and interior multiplicity at most degree.
    NotClamped,
    /// Weight at this index is not finite or not positive.
    BadWeight(usize),
    /// Control point at this index is not finite.
    BadPoint(usize),
    TooFewPoints,
    /// Coincident data points or a singular interpolation system.
    Degenerate,
    BadAngle,
    BadThickness,
    /// A tessellation vertex has no defined surface normal.
    NoNormal,
}

impl std::fmt::Display for NurbsError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            NurbsError::BadDegree => f.write_str("degree must be at least 1"),
            NurbsError::KnotCount { expected, got } => {
                write!(f, "expected {expected} knots, got {got}")
            }
            NurbsError::BadKnots => f.write_str("knots must be finite and non-decreasing"),
            NurbsError::NotClamped => f.write_str("knot vector must be clamped"),
            NurbsError::BadWeight(i) => write!(f, "weight {i} must be finite and positive"),
            NurbsError::BadPoint(i) => write!(f, "control point {i} is not finite"),
            NurbsError::TooFewPoints => f.write_str("too few control points or curves"),
            NurbsError::Degenerate => f.write_str("degenerate input"),
            NurbsError::BadAngle => f.write_str("angle must be in (0, 2*pi]"),
            NurbsError::BadThickness => f.write_str("thickness must be finite and non-zero"),
            NurbsError::NoNormal => f.write_str("surface normal undefined at a vertex"),
        }
    }
}

impl std::error::Error for NurbsError {}

/// Homogeneous point (w*x, w*y, w*z, w).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct H {
    x: f64,
    y: f64,
    z: f64,
    w: f64,
}

impl H {
    fn of(p: V3, w: f64) -> H {
        H {
            x: p.x * w,
            y: p.y * w,
            z: p.z * w,
            w,
        }
    }
    fn xyz(self) -> V3 {
        v3(self.x, self.y, self.z)
    }
    fn point(self) -> V3 {
        self.xyz() * (1.0 / self.w)
    }
    fn add(self, o: H) -> H {
        H {
            x: self.x + o.x,
            y: self.y + o.y,
            z: self.z + o.z,
            w: self.w + o.w,
        }
    }
    fn scale(self, k: f64) -> H {
        H {
            x: self.x * k,
            y: self.y * k,
            z: self.z * k,
            w: self.w * k,
        }
    }
    /// self * (1 - t) + o * t
    fn lerp(self, o: H, t: f64) -> H {
        self.scale(1.0 - t).add(o.scale(t))
    }
}

fn binom(n: usize, k: usize) -> f64 {
    let mut r = 1.0;
    for i in 0..k {
        r = r * (n - i) as f64 / (i + 1) as f64;
    }
    r
}

/// Validates a clamped knot vector for `n_ctrl` control points of degree `p`.
fn check_knots(knots: &[f64], p: usize, n_ctrl: usize) -> Result<(), NurbsError> {
    if p == 0 {
        return Err(NurbsError::BadDegree);
    }
    if n_ctrl < p + 1 {
        return Err(NurbsError::TooFewPoints);
    }
    let expected = n_ctrl + p + 1;
    if knots.len() != expected {
        return Err(NurbsError::KnotCount {
            expected,
            got: knots.len(),
        });
    }
    if knots.iter().any(|k| !k.is_finite()) || knots.windows(2).any(|w| w[1] < w[0]) {
        return Err(NurbsError::BadKnots);
    }
    if knots[p] >= knots[n_ctrl] {
        return Err(NurbsError::BadKnots);
    }
    let mut i = 0;
    while i < knots.len() {
        let mut j = i;
        while j < knots.len() && knots[j] == knots[i] {
            j += 1;
        }
        let m = j - i;
        let end = i == 0 || j == knots.len();
        if (end && m != p + 1) || (!end && m > p) {
            return Err(NurbsError::NotClamped);
        }
        i = j;
    }
    Ok(())
}

fn check_points(ctrl: &[V3], weights: &[f64]) -> Result<(), NurbsError> {
    if let Some(i) = ctrl
        .iter()
        .position(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
    {
        return Err(NurbsError::BadPoint(i));
    }
    if let Some(i) = weights.iter().position(|w| !(w.is_finite() && *w > 0.0)) {
        return Err(NurbsError::BadWeight(i));
    }
    Ok(())
}

/// Span index k with knots[k] <= u < knots[k+1], clamped to [p, n] (n = last control index).
fn find_span(knots: &[f64], p: usize, n: usize, u: f64) -> usize {
    if u >= knots[n + 1] {
        return n;
    }
    // Written so that NaN also lands here.
    if !(u > knots[p]) {
        return p;
    }
    let k = knots[..=n].partition_point(|&x| x <= u);
    (k - 1).clamp(p, n)
}

/// Non-zero basis functions N[span-p..=span] at u.
fn basis_funs(knots: &[f64], span: usize, u: f64, p: usize) -> Vec<f64> {
    let mut n = vec![0.0; p + 1];
    let mut left = vec![0.0; p + 1];
    let mut right = vec![0.0; p + 1];
    n[0] = 1.0;
    for j in 1..=p {
        left[j] = u - knots[span + 1 - j];
        right[j] = knots[span + j] - u;
        let mut saved = 0.0;
        for r in 0..j {
            let temp = n[r] / (right[r + 1] + left[j - r]);
            n[r] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        n[j] = saved;
    }
    n
}

/// Basis functions and their derivatives up to `nd`: result[k][j] = k-th derivative of
/// N[span-p+j]. Orders above p are zero.
fn ders_basis(knots: &[f64], span: usize, u: f64, p: usize, nd: usize) -> Vec<Vec<f64>> {
    let mut ndu = vec![vec![0.0; p + 1]; p + 1];
    let mut left = vec![0.0; p + 1];
    let mut right = vec![0.0; p + 1];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = u - knots[span + 1 - j];
        right[j] = knots[span + j] - u;
        let mut saved = 0.0;
        for r in 0..j {
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = ndu[r][j - 1] / ndu[j][r];
            ndu[r][j] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    let mut ders = vec![vec![0.0; p + 1]; nd + 1];
    for j in 0..=p {
        ders[0][j] = ndu[j][p];
    }
    let n = nd.min(p);
    let mut a = vec![vec![0.0; p + 1]; 2];
    for r in 0..=p {
        let (mut s1, mut s2) = (0usize, 1usize);
        a[0][0] = 1.0;
        for k in 1..=n {
            let mut d = 0.0;
            let rk = r as isize - k as isize;
            let pk = p - k;
            if r >= k {
                a[s2][0] = a[s1][0] / ndu[pk + 1][rk as usize];
                d = a[s2][0] * ndu[rk as usize][pk];
            }
            let j1: usize = if rk >= -1 { 1 } else { (-rk) as usize };
            let j2: usize = if r as isize - 1 <= pk as isize {
                k - 1
            } else {
                p - r
            };
            for j in j1..=j2 {
                let idx = (rk + j as isize) as usize;
                a[s2][j] = (a[s1][j] - a[s1][j - 1]) / ndu[pk + 1][idx];
                d += a[s2][j] * ndu[idx][pk];
            }
            if r <= pk {
                a[s2][k] = -a[s1][k - 1] / ndu[pk + 1][r];
                d += a[s2][k] * ndu[r][pk];
            }
            ders[k][r] = d;
            std::mem::swap(&mut s1, &mut s2);
        }
    }
    let mut f = p as f64;
    for k in 1..=n {
        for j in 0..=p {
            ders[k][j] *= f;
        }
        f *= (p - k) as f64;
    }
    ders
}

/// Boehm insertion of `u` up to `r` times (capped so multiplicity stays <= p).
fn insert_hom(p: usize, knots: &[f64], pw: &[H], u: f64, r: usize) -> (Vec<f64>, Vec<H>) {
    let n = pw.len() - 1;
    let (a, b) = (knots[p], knots[n + 1]);
    if !(u > a && u < b) {
        return (knots.to_vec(), pw.to_vec());
    }
    // Snap to an existing knot so near-duplicates never create sliver spans.
    let snap = (b - a) * 1e-12;
    let u = knots
        .iter()
        .copied()
        .find(|k| (k - u).abs() <= snap)
        .unwrap_or(u);
    let k = find_span(knots, p, n, u);
    let s = knots.iter().filter(|&&x| x == u).count();
    let r = r.min(p.saturating_sub(s));
    if r == 0 {
        return (knots.to_vec(), pw.to_vec());
    }
    let mut uq = Vec::with_capacity(knots.len() + r);
    uq.extend_from_slice(&knots[..=k]);
    uq.extend(std::iter::repeat_n(u, r));
    uq.extend_from_slice(&knots[k + 1..]);
    let mut qw = vec![H::default(); n + 1 + r];
    qw[..=k - p].copy_from_slice(&pw[..=k - p]);
    for i in k - s..=n {
        qw[i + r] = pw[i];
    }
    let mut rw: Vec<H> = (0..=p - s).map(|i| pw[k - p + i]).collect();
    let mut l = 0;
    for j in 1..=r {
        l = k - p + j;
        for i in 0..=p - j - s {
            let alpha = (u - knots[l + i]) / (knots[i + k + 1] - knots[l + i]);
            rw[i] = rw[i].lerp(rw[i + 1], alpha);
        }
        qw[l] = rw[0];
        qw[k + r - j - s] = rw[p - j - s];
    }
    for i in l + 1..k - s {
        qw[i] = rw[i - l];
    }
    (uq, qw)
}

/// Gauss elimination with partial pivoting; `b[row][col]` holds several right-hand sides.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<Vec<H>>) -> Option<Vec<Vec<H>>> {
    let n = a.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if !(a[piv][col].abs() > 1e-14) {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..n {
            let f = a[r][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for c in col..n {
                let v = a[col][c];
                a[r][c] -= f * v;
            }
            for c in 0..b[r].len() {
                let v = b[col][c];
                b[r][c] = b[r][c].add(v.scale(-f));
            }
        }
    }
    for row in (0..n).rev() {
        for c in 0..b[row].len() {
            let mut s = b[row][c];
            for k in row + 1..n {
                s = s.add(b[k][c].scale(-a[row][k]));
            }
            b[row][c] = s.scale(1.0 / a[row][row]);
        }
    }
    Some(b)
}

/// Chord-length parameters in [0, 1] averaged over columns; `cols[c]` is one sequence
/// of data points. None when the sequence is degenerate.
fn chord_params(cols: &[Vec<V3>]) -> Option<Vec<f64>> {
    let k = cols.first()?.len();
    if k < 2 {
        return None;
    }
    let mut acc = vec![0.0; k];
    let mut used = 0usize;
    for col in cols {
        let total: f64 = col.windows(2).map(|w| (w[1] - w[0]).len()).sum();
        if !(total > 0.0) || !total.is_finite() {
            continue;
        }
        used += 1;
        let mut run = 0.0;
        for i in 1..k {
            run += (col[i] - col[i - 1]).len();
            acc[i] += run / total;
        }
    }
    if used == 0 {
        return None;
    }
    let mut t: Vec<f64> = acc.iter().map(|a| a / used as f64).collect();
    t[0] = 0.0;
    t[k - 1] = 1.0;
    if t.windows(2).any(|w| !(w[1] > w[0])) {
        return None;
    }
    Some(t)
}

/// Clamped knot vector by averaging the parameters (keeps the system non-singular).
fn averaging_knots(params: &[f64], p: usize) -> Vec<f64> {
    let m = params.len() - 1;
    let mut knots = vec![0.0; p + 1];
    for j in 1..=m - p {
        knots.push(params[j..j + p].iter().sum::<f64>() / p as f64);
    }
    knots.extend(std::iter::repeat_n(1.0, p + 1));
    knots
}

/// Solves for control points so the spline of degree `p` on `knots` passes through
/// `data[k][c]` at `params[k]` for every column c.
fn interp_hom(
    params: &[f64],
    knots: &[f64],
    p: usize,
    data: Vec<Vec<H>>,
) -> Result<Vec<Vec<H>>, NurbsError> {
    let k = params.len();
    let mut a = vec![vec![0.0; k]; k];
    for (row, &t) in params.iter().enumerate() {
        let span = find_span(knots, p, k - 1, t);
        for (i, nv) in basis_funs(knots, span, t, p).into_iter().enumerate() {
            a[row][span - p + i] = nv;
        }
    }
    solve(a, data).ok_or(NurbsError::Degenerate)
}

/// Local control coefficients (in units of radius, frame x/y) of an exact rational
/// quadratic arc from angle 0 to `sweep`, with its knots on [0, 1] and weights.
fn unit_arc(sweep: f64) -> (Vec<f64>, Vec<[f64; 2]>, Vec<f64>) {
    let narcs = if sweep <= FRAC_PI_2 + 1e-12 {
        1
    } else if sweep <= PI + 1e-12 {
        2
    } else if sweep <= 1.5 * PI + 1e-12 {
        3
    } else {
        4
    };
    let dth = sweep / narcs as f64;
    let w1 = (dth * 0.5).cos();
    let mut pts = vec![[1.0, 0.0]];
    let mut ws = vec![1.0];
    for k in 1..=narcs {
        let mid = dth * (k as f64 - 0.5);
        let end = dth * k as f64;
        pts.push([mid.cos() / w1, mid.sin() / w1]);
        ws.push(w1);
        pts.push([end.cos(), end.sin()]);
        ws.push(1.0);
    }
    if (sweep - TAU).abs() < 1e-12 {
        *pts.last_mut().unwrap() = [1.0, 0.0];
    }
    let mut knots = vec![0.0; 3];
    for k in 1..narcs {
        let t = k as f64 / narcs as f64;
        knots.push(t);
        knots.push(t);
    }
    knots.extend([1.0; 3]);
    (knots, pts, ws)
}

/// Orthonormal frame from an x direction and a rough y direction.
fn frame(xa: V3, ya: V3) -> Option<(V3, V3)> {
    let lx = xa.len();
    if !(lx > 0.0) || !lx.is_finite() {
        return None;
    }
    let x = xa * (1.0 / lx);
    let y = ya - x * ya.dot(x);
    let ly = y.len();
    if !(ly > 1e-12 * ya.len().max(1e-300)) {
        return None;
    }
    Some((x, y * (1.0 / ly)))
}

/// NURBS curve with a clamped knot vector.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    degree: usize,
    knots: Vec<f64>,
    ctrl: Vec<V3>,
    weights: Vec<f64>,
}

impl Curve {
    pub fn new(
        degree: usize,
        knots: Vec<f64>,
        ctrl: Vec<V3>,
        weights: Vec<f64>,
    ) -> Result<Curve, NurbsError> {
        check_knots(&knots, degree, ctrl.len())?;
        if weights.len() != ctrl.len() {
            return Err(NurbsError::BadWeight(weights.len().min(ctrl.len())));
        }
        check_points(&ctrl, &weights)?;
        Ok(Curve {
            degree,
            knots,
            ctrl,
            weights,
        })
    }

    /// Polynomial B-spline (all weights 1).
    pub fn bspline(degree: usize, knots: Vec<f64>, ctrl: Vec<V3>) -> Result<Curve, NurbsError> {
        let w = vec![1.0; ctrl.len()];
        Curve::new(degree, knots, ctrl, w)
    }

    /// Exact circular arc of radius `r` around `c` in the plane (xa, ya), from angle `a0`
    /// to `a1` (counter-clockwise about xa x ya), parameter domain [0, 1].
    pub fn arc(c: V3, xa: V3, ya: V3, r: f64, a0: f64, a1: f64) -> Result<Curve, NurbsError> {
        let sweep = a1 - a0;
        if !(sweep > 0.0 && sweep <= TAU + 1e-12) {
            return Err(NurbsError::BadAngle);
        }
        if !(r > 0.0) || !r.is_finite() {
            return Err(NurbsError::Degenerate);
        }
        let (x0, y0) = frame(xa, ya).ok_or(NurbsError::Degenerate)?;
        let (cs, sn) = (a0.cos(), a0.sin());
        let x = x0 * cs + y0 * sn;
        let y = y0 * cs - x0 * sn;
        let (knots, pts, ws) = unit_arc(sweep.min(TAU));
        let ctrl = pts
            .iter()
            .map(|q| c + (x * q[0] + y * q[1]) * r)
            .collect();
        Curve::new(2, knots, ctrl, ws)
    }

    /// Full circle as a rational quadratic (9 control points), domain [0, 1].
    pub fn circle(c: V3, xa: V3, ya: V3, r: f64) -> Result<Curve, NurbsError> {
        Curve::arc(c, xa, ya, r, 0.0, TAU)
    }

    /// Degree-1 curve through the points with chord-length knots.
    pub fn polyline(pts: &[V3]) -> Result<Curve, NurbsError> {
        if pts.len() < 2 {
            return Err(NurbsError::TooFewPoints);
        }
        let t = chord_params(&[pts.to_vec()]).ok_or(NurbsError::Degenerate)?;
        let mut knots = vec![0.0];
        knots.extend_from_slice(&t);
        knots.push(1.0);
        Curve::bspline(1, knots, pts.to_vec())
    }

    /// Global interpolation through `pts` (chord-length parameters, averaged knots).
    /// The degree is lowered to `pts.len() - 1` when there are too few points.
    pub fn interpolate(pts: &[V3], degree: usize) -> Result<Curve, NurbsError> {
        if pts.len() < 2 {
            return Err(NurbsError::TooFewPoints);
        }
        if degree == 0 {
            return Err(NurbsError::BadDegree);
        }
        let p = degree.min(pts.len() - 1);
        let t = chord_params(&[pts.to_vec()]).ok_or(NurbsError::Degenerate)?;
        let knots = averaging_knots(&t, p);
        let data = pts.iter().map(|q| vec![H::of(*q, 1.0)]).collect();
        let sol = interp_hom(&t, &knots, p, data)?;
        let hs: Vec<H> = sol.into_iter().map(|row| row[0]).collect();
        Curve::from_hom(p, knots, &hs)
    }

    fn from_hom(p: usize, knots: Vec<f64>, hs: &[H]) -> Result<Curve, NurbsError> {
        if let Some(i) = hs.iter().position(|h| !(h.w > 0.0)) {
            return Err(NurbsError::BadWeight(i));
        }
        let ctrl = hs.iter().map(|h| h.point()).collect();
        let weights = hs.iter().map(|h| h.w).collect();
        Curve::new(p, knots, ctrl, weights)
    }

    pub fn degree(&self) -> usize {
        self.degree
    }
    pub fn knots(&self) -> &[f64] {
        &self.knots
    }
    pub fn ctrl(&self) -> &[V3] {
        &self.ctrl
    }
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    pub fn domain(&self) -> (f64, f64) {
        (self.knots[self.degree], self.knots[self.ctrl.len()])
    }

    fn h(&self, i: usize) -> H {
        H::of(self.ctrl[i], self.weights[i])
    }

    fn hom(&self) -> Vec<H> {
        (0..self.ctrl.len()).map(|i| self.h(i)).collect()
    }

    fn clamp(&self, u: f64) -> f64 {
        let (a, b) = self.domain();
        u.clamp(a, b)
    }

    /// Point at `u` by the de Boor triangle in homogeneous space.
    pub fn point(&self, u: f64) -> V3 {
        let u = self.clamp(u);
        let p = self.degree;
        let k = find_span(&self.knots, p, self.ctrl.len() - 1, u);
        let mut d: Vec<H> = (0..=p).map(|j| self.h(j + k - p)).collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let lo = self.knots[j + k - p];
                let hi = self.knots[j + 1 + k - r];
                let alpha = (u - lo) / (hi - lo);
                d[j] = d[j - 1].lerp(d[j], alpha);
            }
        }
        d[p].point()
    }

    /// Point and derivatives up to order `d`: result[k] = k-th derivative (result[0] = point).
    pub fn derivs(&self, u: f64, d: usize) -> Vec<V3> {
        let u = self.clamp(u);
        let p = self.degree;
        let span = find_span(&self.knots, p, self.ctrl.len() - 1, u);
        let du = d.min(p);
        let nd = ders_basis(&self.knots, span, u, p, du);
        let mut aw = vec![H::default(); d + 1];
        for (k, row) in nd.iter().enumerate().take(du + 1) {
            for (j, &nb) in row.iter().enumerate() {
                aw[k] = aw[k].add(self.h(span - p + j).scale(nb));
            }
        }
        let mut ck = vec![V3::default(); d + 1];
        for k in 0..=d {
            let mut v = aw[k].xyz();
            for i in 1..=k {
                v = v - ck[k - i] * (binom(k, i) * aw[i].w);
            }
            ck[k] = v * (1.0 / aw[0].w);
        }
        ck
    }

    /// Inserts knot `u` up to `r` times; the shape is unchanged.
    pub fn insert_knot(&self, u: f64, r: usize) -> Curve {
        let (knots, hs) = insert_hom(self.degree, &self.knots, &self.hom(), u, r);
        Curve::from_hom(self.degree, knots, &hs).unwrap_or_else(|_| self.clone())
    }

    /// Inserts every knot of `us` once (repeat a value to raise its multiplicity).
    pub fn refine(&self, us: &[f64]) -> Curve {
        let mut knots = self.knots.clone();
        let mut hs = self.hom();
        for &u in us {
            let (k2, h2) = insert_hom(self.degree, &knots, &hs, u, 1);
            knots = k2;
            hs = h2;
        }
        Curve::from_hom(self.degree, knots, &hs).unwrap_or_else(|_| self.clone())
    }

    fn interior_knots(&self) -> Vec<f64> {
        let n = self.ctrl.len();
        let mut v: Vec<f64> = self.knots[self.degree + 1..n].to_vec();
        v.dedup();
        v
    }

    /// Exact degree elevation by one (Bezier split, elevate, rejoin). Interior knots end
    /// with multiplicity equal to the new degree; knot removal is not done.
    pub fn elevated(&self) -> Curve {
        let p = self.degree;
        let interior = self.interior_knots();
        let mut knots = self.knots.clone();
        let mut pw = self.hom();
        for &u in &interior {
            let (k2, p2) = insert_hom(p, &knots, &pw, u, p);
            knots = k2;
            pw = p2;
        }
        let nseg = interior.len() + 1;
        debug_assert_eq!(pw.len(), p * nseg + 1);
        let q = p + 1;
        let mut out = Vec::with_capacity(q * nseg + 1);
        for s in 0..nseg {
            let b = &pw[s * p..=s * p + p];
            for i in 0..=q {
                if s > 0 && i == 0 {
                    continue;
                }
                let t = i as f64 / q as f64;
                let h = if i == 0 {
                    b[0]
                } else if i == q {
                    b[p]
                } else {
                    b[i - 1].scale(t).add(b[i].scale(1.0 - t))
                };
                out.push(h);
            }
        }
        let (a, e) = (knots[0], knots[knots.len() - 1]);
        let mut nk = vec![a; q + 1];
        for &u in &interior {
            nk.extend(std::iter::repeat_n(u, q));
        }
        nk.extend(std::iter::repeat_n(e, q + 1));
        Curve::from_hom(q, nk, &out).unwrap_or_else(|_| self.clone())
    }

    /// Same curve with the domain mapped affinely onto [a, b].
    pub fn reparam(&self, a: f64, b: f64) -> Curve {
        if !(b > a) {
            return self.clone();
        }
        let (a0, b0) = self.domain();
        let s = (b - a) / (b0 - a0);
        let p = self.degree;
        let m = self.knots.len();
        let knots = self
            .knots
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                if i <= p {
                    a
                } else if i >= m - p - 1 {
                    b
                } else {
                    a + (k - a0) * s
                }
            })
            .collect();
        Curve {
            knots,
            ..self.clone()
        }
    }

    pub fn translated(&self, d: V3) -> Curve {
        Curve {
            ctrl: self.ctrl.iter().map(|p| *p + d).collect(),
            ..self.clone()
        }
    }

    /// End point equals start point within `tol`.
    pub fn is_closed(&self, tol: f64) -> bool {
        (self.ctrl[0] - self.ctrl[self.ctrl.len() - 1]).len() <= tol
    }

    fn mult_near(&self, u: f64, eps: f64) -> usize {
        self.knots.iter().filter(|k| (*k - u).abs() <= eps).count()
    }

    fn knot_near(&self, u: f64, eps: f64) -> Option<f64> {
        self.knots.iter().copied().find(|k| (k - u).abs() <= eps)
    }
}

/// Brings curves to one degree and one knot vector on [0, 1] without changing shapes.
pub fn make_compatible(curves: &[Curve]) -> Result<Vec<Curve>, NurbsError> {
    if curves.is_empty() {
        return Err(NurbsError::TooFewPoints);
    }
    let eps = 1e-10;
    let mut cs: Vec<Curve> = curves.iter().map(|c| c.reparam(0.0, 1.0)).collect();
    let p = cs.iter().map(|c| c.degree).max().unwrap_or(1);
    for c in cs.iter_mut() {
        while c.degree < p {
            *c = c.elevated();
        }
    }
    let mut merged: Vec<(f64, usize)> = Vec::new();
    for c in &cs {
        for u in c.interior_knots() {
            if u <= eps || u >= 1.0 - eps {
                continue;
            }
            let m = c.mult_near(u, eps);
            match merged.iter_mut().find(|(v, _)| (*v - u).abs() <= eps) {
                Some(e) => e.1 = e.1.max(m),
                None => merged.push((u, m)),
            }
        }
    }
    merged.sort_by(|a, b| a.0.total_cmp(&b.0));
    for c in cs.iter_mut() {
        for &(u, m) in &merged {
            let have = c.mult_near(u, eps);
            if have < m {
                let at = c.knot_near(u, eps).unwrap_or(u);
                *c = c.insert_knot(at, m - have);
            }
        }
    }
    let mut rep = vec![0.0; p + 1];
    for &(u, m) in &merged {
        rep.extend(std::iter::repeat_n(u, m));
    }
    rep.extend(std::iter::repeat_n(1.0, p + 1));
    for c in cs.iter_mut() {
        if c.knots.len() != rep.len() {
            return Err(NurbsError::Degenerate);
        }
        c.knots = rep.clone();
    }
    Ok(cs)
}
