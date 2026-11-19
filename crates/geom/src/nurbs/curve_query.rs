//! Point queries on a [`Curve`] for snapping (geometry plan W10): nearest point, feet of the
//! perpendiculars from a point, tangent points from a point (planar curves), arc length and
//! the parameter at a fraction of it. Roots are bracketed on a dense sample of every knot
//! span, then closed by bisection to machine precision.

use super::free::{breaks, GL};
use super::Curve;
use crate::math::V3;

/// Samples per knot span when bracketing roots.
const PER_SPAN: usize = 48;
/// Bisection steps (the bracket shrinks below 1e-15 of the span).
const BISECT: usize = 64;

/// Roots of `f` on the sample `ts` (sorted): sign changes closed by bisection, exact zeros
/// at samples kept. Duplicates within `1e-12` of the domain are merged.
fn roots(f: &dyn Fn(f64) -> f64, ts: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = vec![];
    let span = ts.last().copied().unwrap_or(0.0) - ts.first().copied().unwrap_or(0.0);
    let mut push = |t: f64| {
        if out.last().is_none_or(|&l| (t - l).abs() > 1e-12 * span.max(1.0)) {
            out.push(t);
        }
    };
    let mut prev = (ts[0], f(ts[0]));
    if prev.1 == 0.0 {
        push(prev.0);
    }
    for &t in &ts[1..] {
        let cur = (t, f(t));
        if cur.1 == 0.0 {
            push(t);
        } else if prev.1 != 0.0 && (prev.1 < 0.0) != (cur.1 < 0.0) {
            let (mut a, mut b, fa) = (prev.0, cur.0, prev.1);
            for _ in 0..BISECT {
                let m = 0.5 * (a + b);
                let fm = f(m);
                if fm == 0.0 {
                    a = m;
                    b = m;
                    break;
                }
                if (fm < 0.0) == (fa < 0.0) {
                    a = m;
                } else {
                    b = m;
                }
            }
            push(0.5 * (a + b));
        }
        prev = cur;
    }
    out
}

impl Curve {
    fn spans(&self) -> Vec<f64> {
        breaks(self.knots(), self.degree(), self.ctrl().len())
    }

    /// Dense parameter sample: `PER_SPAN` points in every knot span, ends included.
    fn sample(&self) -> Vec<f64> {
        let b = self.spans();
        let mut ts = vec![b[0]];
        for w in b.windows(2) {
            ts.extend((1..=PER_SPAN).map(|k| w[0] + (w[1] - w[0]) * k as f64 / PER_SPAN as f64));
        }
        ts
    }

    /// Parameters where the line from `p` meets the curve at a right angle (interior and end
    /// points where (C - p) . C' vanishes).
    pub fn perpendicular_feet(&self, p: V3) -> Vec<f64> {
        let f = |t: f64| {
            let d = self.derivs(t, 1);
            (d[0] - p).dot(d[1])
        };
        roots(&f, &self.sample())
    }

    /// Nearest curve point to `p`: (parameter, point).
    pub fn closest(&self, p: V3) -> (f64, V3) {
        let (a, b) = self.domain();
        let mut best = (a, self.point(a));
        for t in self.perpendicular_feet(p).into_iter().chain([b]) {
            let q = self.point(t);
            if (q - p).len() < (best.1 - p).len() {
                best = (t, q);
            }
        }
        best
    }

    /// Normal of the plane holding every control point within `tol`; None for a straight or
    /// a non-planar curve.
    pub fn plane_normal(&self, tol: f64) -> Option<V3> {
        let c = self.ctrl();
        let o = c[0];
        let far = c.iter().copied().max_by(|a, b| (*a - o).len().total_cmp(&(*b - o).len()))?;
        let e = far - o;
        if !(e.len() > tol) {
            return None;
        }
        let side = c.iter().copied().max_by(|a, b| e.cross(*a - o).len().total_cmp(&e.cross(*b - o).len()))?;
        let n = e.cross(side - o);
        if !(n.len() > tol * e.len()) {
            return None;
        }
        let n = n * (1.0 / n.len());
        c.iter().all(|q| (*q - o).dot(n).abs() <= tol).then_some(n)
    }

    /// Parameters of the points where a line from `p` touches the curve (C - p parallel to
    /// C'), for a planar curve with `p` in its plane (within `tol`); empty otherwise.
    pub fn tangent_points(&self, p: V3, tol: f64) -> Vec<f64> {
        let Some(n) = self.plane_normal(tol) else {
            return vec![];
        };
        if (p - self.ctrl()[0]).dot(n).abs() > tol {
            return vec![];
        }
        let f = |t: f64| {
            let d = self.derivs(t, 1);
            (d[0] - p).cross(d[1]).dot(n)
        };
        // a point on the curve is not a tangent point from outside
        roots(&f, &self.sample()).into_iter().filter(|&t| (self.point(t) - p).len() > tol).collect()
    }

    /// Arc length from the domain start to `t` (Gauss-Legendre, 8 parts per knot span).
    pub fn length_to(&self, t: f64) -> f64 {
        let (a, b) = self.domain();
        let t = t.clamp(a, b);
        let mut s = 0.0;
        for w in self.spans().windows(2) {
            let (lo, hi) = (w[0], w[1].min(t));
            if hi <= lo {
                break;
            }
            for k in 0..8 {
                let (x0, x1) = (lo + (hi - lo) * k as f64 / 8.0, lo + (hi - lo) * (k + 1) as f64 / 8.0);
                let (h, c) = (0.5 * (x1 - x0), 0.5 * (x1 + x0));
                s += GL.iter().map(|&(x, wg)| wg * h * self.derivs(c + h * x, 1)[1].len()).sum::<f64>();
            }
        }
        s
    }

    pub fn length(&self) -> f64 {
        self.length_to(self.domain().1)
    }

    /// Parameter at the fraction `f` (0..1) of the arc length.
    pub fn at_fraction(&self, f: f64) -> f64 {
        // Newton on L(t) - want (L' = |C'|), kept inside a shrinking bracket
        let (mut a, mut b) = self.domain();
        let total = self.length();
        let want = f.clamp(0.0, 1.0) * total;
        let mut t = a + f.clamp(0.0, 1.0) * (b - a);
        for _ in 0..BISECT {
            let g = self.length_to(t) - want;
            if g.abs() <= 1e-13 * total.max(1.0) {
                break;
            }
            if g < 0.0 {
                a = t;
            } else {
                b = t;
            }
            let sp = self.derivs(t, 1)[1].len();
            let nt = t - g / sp;
            t = if sp > 0.0 && nt > a && nt < b { nt } else { 0.5 * (a + b) };
        }
        t
    }
}

#[cfg(test)]
#[path = "tests_curve_query.rs"]
mod tests;
