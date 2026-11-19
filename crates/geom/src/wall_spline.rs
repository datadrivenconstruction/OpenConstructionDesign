// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spline wall axes (geometry plan W5): a planar NURBS axis, its offsets for the wall layers
//! (true offset sampled, then refit by a cubic through the samples until it stays within a
//! tolerance), the exact area of a layer by Gauss-Legendre and the layer's footprint ring.
//!
//! With the left normal n and signed curvature k (positive turning left), the offset at
//! distance d moves with speed |C'|(1 - d k), so the layer between offsets a < b has the area
//! (b - a) L - (b^2 - a^2) / 2 * T, with L the axis length and T = integral of k ds the total
//! turning. Both integrals are taken by 5-point Gauss-Legendre on every knot span.

use crate::math::{v2, v3, V2, V3};
use crate::nurbs::Curve;

/// Gauss-Legendre nodes and weights on [-1, 1], 5 points.
const GL: [(f64, f64); 5] = [
    (0.0, 0.568_888_888_888_888_9),
    (-0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (0.538_469_310_105_683_1, 0.478_628_670_499_366_5),
    (-0.906_179_845_938_664, 0.236_926_885_056_189_1),
    (0.906_179_845_938_664, 0.236_926_885_056_189_1),
];
/// Parts per knot span for the quadrature.
const PARTS: usize = 8;
/// Curvature samples per knot span for the offset check.
const CHECK: usize = 64;
/// Most offset samples in all before the refit gives up (the interpolation solve is dense).
const MAX_SAMPLES: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum SplineError {
    /// Control points not on one horizontal plane, or not finite.
    NotPlanar,
    /// Zero length or a cusp (vanishing tangent).
    Degenerate,
    /// The offset folds: |d| reaches the radius of curvature on the inner side.
    Folds { offset: f64, radius: f64 },
    /// The refit did not reach the tolerance.
    Refit { deviation: f64 },
}

/// Planar wall axis: a NURBS curve with every control point at z = 0.
#[derive(Clone, Debug, PartialEq)]
pub struct SplineAxis {
    curve: Curve,
}

fn xy(p: V3) -> V2 {
    v2(p.x, p.y)
}

impl SplineAxis {
    pub fn new(curve: Curve) -> Result<SplineAxis, SplineError> {
        if curve.ctrl().iter().any(|p| !(p.x.is_finite() && p.y.is_finite()) || p.z != 0.0) {
            return Err(SplineError::NotPlanar);
        }
        let s = SplineAxis { curve };
        let (a, b) = s.curve.domain();
        let cusp = s.params(CHECK).into_iter().any(|t| !(s.curve.derivs(t, 1)[1].len() > 1e-12));
        if !(b > a) || cusp || !(s.length() > 1e-9) {
            return Err(SplineError::Degenerate);
        }
        Ok(s)
    }

    /// Cubic through plan points (chord-length parameters).
    pub fn through(pts: &[V2]) -> Result<SplineAxis, SplineError> {
        let p3: Vec<V3> = pts.iter().map(|p| v3(p.x, p.y, 0.0)).collect();
        let c = Curve::interpolate(&p3, 3).map_err(|_| SplineError::Degenerate)?;
        SplineAxis::new(c)
    }

    pub fn curve(&self) -> &Curve {
        &self.curve
    }

    /// Distinct knots of the domain.
    fn spans(&self) -> Vec<f64> {
        let c = &self.curve;
        let mut b: Vec<f64> = c.knots()[c.degree()..=c.ctrl().len()].to_vec();
        b.dedup();
        b
    }

    /// `per` parameters per knot span, both ends included.
    fn params(&self, per: usize) -> Vec<f64> {
        let b = self.spans();
        let mut ts = vec![b[0]];
        for w in b.windows(2) {
            ts.extend((1..=per).map(|k| w[0] + (w[1] - w[0]) * k as f64 / per as f64));
        }
        ts
    }

    /// Gauss-Legendre integral of `f(t)` over the domain.
    fn integrate(&self, f: impl Fn(f64) -> f64) -> f64 {
        let mut s = 0.0;
        for w in self.spans().windows(2) {
            for k in 0..PARTS {
                let x0 = w[0] + (w[1] - w[0]) * k as f64 / PARTS as f64;
                let x1 = w[0] + (w[1] - w[0]) * (k + 1) as f64 / PARTS as f64;
                let (h, c) = (0.5 * (x1 - x0), 0.5 * (x1 + x0));
                s += GL.iter().map(|&(x, wg)| wg * h * f(c + h * x)).sum::<f64>();
            }
        }
        s
    }

    pub fn point(&self, t: f64) -> V2 {
        xy(self.curve.point(t))
    }

    /// Unit left normal at `t`.
    pub fn normal(&self, t: f64) -> V2 {
        let d = self.curve.derivs(t, 1)[1];
        let l = (d.x * d.x + d.y * d.y).sqrt();
        v2(-d.y / l, d.x / l)
    }

    /// Signed curvature at `t` (positive turning left).
    pub fn curvature(&self, t: f64) -> f64 {
        let d = self.curve.derivs(t, 2);
        let (p, q) = (d[1], d[2]);
        let s = (p.x * p.x + p.y * p.y).sqrt();
        (p.x * q.y - p.y * q.x) / (s * s * s)
    }

    /// Axis length.
    pub fn length(&self) -> f64 {
        self.integrate(|t| {
            let d = self.curve.derivs(t, 1)[1];
            (d.x * d.x + d.y * d.y).sqrt()
        })
    }

    /// Total turning of the tangent, rad (integral of the signed curvature along the axis).
    pub fn turning(&self) -> f64 {
        self.integrate(|t| {
            let d = self.curve.derivs(t, 2);
            let (p, q) = (d[1], d[2]);
            (p.x * q.y - p.y * q.x) / (p.x * p.x + p.y * p.y)
        })
    }

    /// Exact area of the layer between the left offsets `a` and `b`.
    pub fn layer_area(&self, a: f64, b: f64) -> f64 {
        let (a, b) = (a.min(b), a.max(b));
        (b - a) * self.length() - 0.5 * (b * b - a * a) * self.turning()
    }

    /// Refuses an offset `d` that reaches the radius of curvature on its side (1 - d k <= 0).
    pub fn check_offset(&self, d: f64) -> Result<(), SplineError> {
        for t in self.params(CHECK) {
            let k = self.curvature(t);
            if !(1.0 - d * k > 1e-9) {
                return Err(SplineError::Folds { offset: d, radius: 1.0 / k.abs() });
            }
        }
        Ok(())
    }

    /// True offset point at `t`.
    pub fn offset_point(&self, t: f64, d: f64) -> V2 {
        let (p, n) = (self.point(t), self.normal(t));
        v2(p.x + d * n.x, p.y + d * n.y)
    }

    /// The offset at distance `d` (left positive) as a cubic refit through samples of the true
    /// offset, refined until every check point between samples lies within `tol` of the fit.
    /// Returns the curve and its largest measured deviation.
    pub fn offset(&self, d: f64, tol: f64) -> Result<(Curve, f64), SplineError> {
        self.check_offset(d)?;
        let mut per = 4;
        let mut last = f64::INFINITY;
        let spans = self.spans().len() - 1;
        while per * spans <= MAX_SAMPLES {
            let ts = self.params(per);
            let pts: Vec<V3> = ts.iter().map(|&t| {
                let q = self.offset_point(t, d);
                v3(q.x, q.y, 0.0)
            }).collect();
            let fit = Curve::interpolate(&pts, 3).map_err(|_| SplineError::Degenerate)?;
            let dev = deviation(&fit, &pts, |k, f| {
                let t = ts[k] + (ts[k + 1] - ts[k]) * f;
                let q = self.offset_point(t, d);
                v3(q.x, q.y, 0.0)
            });
            if dev <= tol {
                return Ok((fit, dev));
            }
            last = dev;
            per *= 2;
        }
        Err(SplineError::Refit { deviation: last })
    }

    /// Plan points of the true offset at `d`, from start to end, chord sag within `sag`.
    pub fn offset_polyline(&self, d: f64, sag: f64) -> Vec<V2> {
        let ts = self.params(4);
        let mut out = vec![self.offset_point(ts[0], d)];
        for w in ts.windows(2) {
            self.subdivide(d, sag, w[0], w[1], 0, &mut out);
        }
        out
    }

    fn subdivide(&self, d: f64, sag: f64, t0: f64, t1: f64, depth: usize, out: &mut Vec<V2>) {
        let (p, q) = (self.offset_point(t0, d), self.offset_point(t1, d));
        let tm = 0.5 * (t0 + t1);
        let m = self.offset_point(tm, d);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let l = (dx * dx + dy * dy).sqrt();
        let h = if l > 0.0 { ((m.x - p.x) * dy - (m.y - p.y) * dx).abs() / l } else { 0.0 };
        if depth < 16 && h > sag {
            self.subdivide(d, sag, t0, tm, depth + 1, out);
            self.subdivide(d, sag, tm, t1, depth + 1, out);
        } else {
            out.push(q);
        }
    }

    /// Footprint ring of the layer between offsets `a` and `b`: offset `a` forward, offset
    /// `b` back, closed by the straight end caps (counter-clockwise when a < b).
    pub fn layer_ring(&self, a: f64, b: f64, sag: f64) -> Vec<V2> {
        let (a, b) = (a.min(b), a.max(b));
        let mut r = self.offset_polyline(b, sag);
        r.reverse();
        let mut lo = self.offset_polyline(a, sag);
        lo.append(&mut r);
        lo
    }
}

/// One plan piece of a fitted axis: from `a` to `b`, `bulge` = tan(sweep / 4) (0: straight).
pub type ArcPiece = (V2, V2, f64);

/// Most biarc intervals before the fit gives up.
const MAX_BIARCS: usize = 4096;

/// Signed angle from `u` to `v`.
fn angle(u: V2, v: V2) -> f64 {
    u.cross(v).atan2(u.dot(v))
}

/// Biarc between `p0` (unit tangent `t0`) and `p1` (unit tangent `t1`) with equal tangent
/// legs; the two arcs meet tangent-continuously. None when the data admit no biarc.
fn biarc(p0: V2, t0: V2, p1: V2, t1: V2) -> Option<[ArcPiece; 2]> {
    let v = p1 - p0;
    if !(v.len() > 1e-12) {
        return None;
    }
    let t = t0 + t1;
    let den = 2.0 * (1.0 - t0.dot(t1));
    let vt = v.dot(t);
    let d = if den.abs() < 1e-12 {
        let q = 4.0 * v.dot(t1);
        if q.abs() < 1e-15 {
            return None;
        }
        v.dot(v) / q
    } else {
        (-vt + (vt * vt + den * v.dot(v)).sqrt()) / den
    };
    if !(d.is_finite() && d > 0.0) {
        return None;
    }
    let m = ((p0 + t0 * d) + (p1 - t1 * d)) * 0.5;
    let b0 = (0.5 * angle(t0, m - p0)).tan();
    let b1 = (0.5 * angle(p1 - m, t1)).tan();
    // below the smallest arc bulge the piece is stored straight (sag under 1e-9 of its chord)
    let b = |x: f64| if x.abs() < crate::arc::MIN_BULGE { 0.0 } else { x };
    Some([(p0, m, b(b0)), (m, p1, b(b1))])
}

/// Distance from `p` to the arc piece.
pub fn piece_dist(p: V2, (a, b, bulge): ArcPiece) -> f64 {
    let seg = |a: V2, b: V2| {
        let e = b - a;
        let t = ((p - a).dot(e) / e.dot(e)).clamp(0.0, 1.0);
        (p - (a + e * t)).len()
    };
    let Ok(ax) = crate::arc::ArcAxis::new(a, b, bulge, crate::Tol::default()) else {
        return seg(a, b);
    };
    if bulge == 0.0 {
        return seg(a, b);
    }
    let q = p - ax.center;
    // angle of p from the start, measured along the sweep
    let mut f = (q.y.atan2(q.x) - ax.start) * ax.sweep.signum();
    f = f.rem_euclid(2.0 * std::f64::consts::PI);
    if f <= ax.sweep.abs() {
        (q.len() - ax.radius).abs()
    } else {
        (p - a).len().min((p - b).len())
    }
}

impl SplineAxis {
    /// Unit tangent at `t`.
    pub fn tangent(&self, t: f64) -> V2 {
        let d = self.curve.derivs(t, 1)[1];
        let l = (d.x * d.x + d.y * d.y).sqrt();
        v2(d.x / l, d.y / l)
    }

    /// Tangent-continuous chain of arcs within `tol` of the axis: biarcs between axis points,
    /// each interval halved until 7 check points of the axis inside it lie within `tol`.
    pub fn biarcs(&self, tol: f64) -> Result<Vec<ArcPiece>, SplineError> {
        let mut stack: Vec<(f64, f64)> = self.params(2).windows(2).rev().map(|w| (w[0], w[1])).collect();
        let mut out: Vec<ArcPiece> = vec![];
        let mut worst: f64 = 0.0;
        while let Some((t0, t1)) = stack.pop() {
            let fit = biarc(self.point(t0), self.tangent(t0), self.point(t1), self.tangent(t1));
            let dev = fit.map_or(f64::INFINITY, |pair| {
                (1..8)
                    .map(|k| {
                        let p = self.point(t0 + (t1 - t0) * k as f64 / 8.0);
                        piece_dist(p, pair[0]).min(piece_dist(p, pair[1]))
                    })
                    .fold(0.0, f64::max)
            });
            let tiny = (t1 - t0) < 1e-9 * (self.curve.domain().1 - self.curve.domain().0);
            if dev <= tol || tiny || out.len() + stack.len() > MAX_BIARCS {
                if dev > tol {
                    worst = worst.max(dev);
                }
                if let Some(pair) = fit {
                    out.extend(pair);
                }
                continue;
            }
            let tm = 0.5 * (t0 + t1);
            stack.push((tm, t1));
            stack.push((t0, tm));
        }
        if worst > tol {
            return Err(SplineError::Refit { deviation: worst });
        }
        Ok(out)
    }
}

/// Largest distance from `true_at(k, f)` (a point of the true curve at fraction f between
/// samples k and k+1) to the fit, at f = 1/4, 1/2, 3/4 of every interval. The fit parameter
/// of each sample is its chord-length parameter; the foot is found by Newton from the
/// interpolated parameter, kept inside the interval's parameter range.
fn deviation(fit: &Curve, pts: &[V3], true_at: impl Fn(usize, f64) -> V3) -> f64 {
    let total: f64 = pts.windows(2).map(|w| (w[1] - w[0]).len()).sum();
    let mut u = vec![0.0];
    let mut run = 0.0;
    for w in pts.windows(2) {
        run += (w[1] - w[0]).len();
        u.push(run / total);
    }
    let mut worst: f64 = 0.0;
    for k in 0..pts.len() - 1 {
        let lo = if k > 0 { u[k - 1] } else { u[k] };
        let hi = if k + 2 < u.len() { u[k + 2] } else { u[k + 1] };
        for f in [0.25, 0.5, 0.75] {
            let q = true_at(k, f);
            let mut s = u[k] + (u[k + 1] - u[k]) * f;
            for _ in 0..30 {
                let d = fit.derivs(s, 2);
                let e = d[0] - q;
                let g = e.dot(d[1]);
                let h = d[1].dot(d[1]) + e.dot(d[2]);
                if !(h > 0.0) {
                    break;
                }
                let ns = (s - g / h).clamp(lo, hi);
                let done = (ns - s).abs() < 1e-15;
                s = ns;
                if done {
                    break;
                }
            }
            worst = worst.max((fit.point(s) - q).len());
        }
    }
    worst
}

#[cfg(test)]
#[path = "wall_spline_tests.rs"]
mod tests;
