// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan dimensions: linear (aligned/directional), chain, angular, arc length, radius.
//! Geometry in model coordinates only; paper offsets and arrowheads are set by the sheet.

use crate::math::{v2, V2};

/// Linear dimension: projections of `a`, `b` onto direction `dir`; the dimension line passes through `base`
/// (before the paper offset along `n`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Linear {
    pub a: V2,
    pub b: V2,
    pub dir: V2,
    pub n: V2,
    pub base: V2,
}

impl Linear {
    /// `dir` is the measuring direction (None means `a→b`); `side` > 0 puts the line left of `dir`;
    /// `base` None means the farther of `a`, `b` along `n`; `offset` shifts the line along `n`, in metres.
    pub fn new(
        a: V2,
        b: V2,
        dir: Option<V2>,
        side: f64,
        base: Option<V2>,
        offset: f64,
    ) -> Option<Self> {
        let d = dir.unwrap_or(b - a);
        let l = d.len();
        if !(l > 1e-12) {
            return None;
        }
        let dir = v2(d.x / l, d.y / l);
        let s = if side < 0.0 { -1.0 } else { 1.0 };
        let n = v2(-dir.y * s, dir.x * s);
        let b0 = base.unwrap_or(if (a - b).dot(n) >= 0.0 { a } else { b });
        Some(Linear {
            a,
            b,
            dir,
            n,
            base: b0 + n * offset,
        })
    }
    pub fn value(&self) -> f64 {
        (self.b - self.a).dot(self.dir).abs()
    }
    /// Dimension-line endpoints with an extra shift `extra` along `n`.
    pub fn line(&self, extra: f64) -> (V2, V2) {
        let o = self.base + self.n * extra;
        let p = |q: V2| o + self.dir * (q - o).dot(self.dir);
        (p(self.a), p(self.b))
    }
}

/// Chain: points ordered by projection onto `dir`; coincident ones (closer than `tol`) merge into one;
/// the result is index pairs of the original points for neighbouring segments.
pub fn chain(pts: &[V2], dir: V2, tol: f64) -> Vec<(usize, usize)> {
    let mut ix: Vec<(f64, usize)> = pts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dot(dir), i))
        .collect();
    ix.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    ix.dedup_by(|b, a| (b.0 - a.0).abs() < tol);
    ix.windows(2).map(|w| (w[0].1, w[1].1)).collect()
}

/// Angle in degrees [0, 360) with last-bit noise suppressed.
pub fn deg(v: V2) -> f64 {
    let a = v.y.atan2(v.x).to_degrees();
    let a = if a < 0.0 { a + 360.0 } else { a };
    (a * 1e9).round() / 1e9 % 360.0
}

/// Angular dimension between lines `p1+d1·t` and `p2+d2·t`: the vertex is their intersection; the
/// counter-clockwise arc from `a0` to `a1` (degrees) spans the smaller angle between rays `d1`, `d2`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Angular {
    pub center: V2,
    pub a0: f64,
    pub a1: f64,
    pub value: f64,
}

pub fn angular(p1: V2, d1: V2, p2: V2, d2: V2) -> Option<Angular> {
    let c = d1.cross(d2);
    if c.abs() < 1e-12 * d1.len() * d2.len() {
        return None;
    }
    let t = (p2 - p1).cross(d2) / c;
    let center = p1 + d1 * t;
    let (g1, g2) = (deg(d1), deg(d2));
    let ccw = (g2 - g1).rem_euclid(360.0);
    let (a0, a1, v) = if ccw <= 180.0 {
        (g1, g2, ccw)
    } else {
        (g2, g1, 360.0 - ccw)
    };
    Some(Angular {
        center,
        a0,
        a1,
        value: (v * 1e9).round() / 1e9,
    })
}

/// Length of an arc of radius `r` between angles `t0`, `t1` (radians).
pub fn arc_length(r: f64, t0: f64, t1: f64) -> f64 {
    r * (t1 - t0).abs()
}

/// Does the ray `o + d·t`, t > eps, intersect the closed loop `ring`?
pub fn ray_hits(o: V2, d: V2, ring: &[V2], eps: f64) -> bool {
    (0..ring.len()).any(|i| {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        let e = q - p;
        let den = d.cross(e);
        if den.abs() < 1e-15 {
            return false;
        }
        let w = p - o;
        let t = w.cross(e) / den;
        let u = w.cross(d) / den;
        t > eps && (-1e-12..=1.0 + 1e-12).contains(&u)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_aligned_skewed_and_offset() {
        // 3-4-5: aligned value 5, line 0.5 left of a→b
        let l = Linear::new(v2(0.0, 0.0), v2(3.0, 4.0), None, 1.0, None, 0.5).unwrap();
        assert!((l.value() - 5.0).abs() < 1e-12);
        let (p, q) = l.line(0.0);
        assert!((p - v2(-0.4, 0.3)).len() < 1e-12 && (q - v2(2.6, 4.3)).len() < 1e-12);
        // horizontal projection of the same points: 3; line passes through the farther point
        let h = Linear::new(
            v2(0.0, 0.0),
            v2(3.0, 4.0),
            Some(v2(1.0, 0.0)),
            1.0,
            None,
            0.0,
        )
        .unwrap();
        assert_eq!(h.value(), 3.0);
        assert_eq!(h.line(1.0), (v2(0.0, 5.0), v2(3.0, 5.0)));
        assert!(Linear::new(v2(1.0, 1.0), v2(1.0, 1.0), None, 1.0, None, 0.0).is_none());
    }

    #[test]
    fn chain_sorts_and_merges() {
        let d = v2(0.6, 0.8);
        let pts = [
            v2(6.0, 8.0),
            v2(0.0, 0.0),
            v2(3.0, 4.0),
            v2(3.0 + 1e-7, 4.0),
            v2(1.5, 2.0),
        ];
        assert_eq!(chain(&pts, d, 1e-6), vec![(1, 4), (4, 2), (2, 0)]);
    }

    #[test]
    fn angular_picks_smaller_angle() {
        let a = angular(v2(0.0, 0.0), v2(1.0, 0.0), v2(5.0, 1.0), v2(-1.0, -1.0)).unwrap();
        assert!((a.center - v2(4.0, 0.0)).len() < 1e-12);
        assert_eq!((a.a0, a.a1, a.value), (225.0, 0.0, 135.0));
        let b = angular(
            v2(0.0, 0.0),
            v2(1.0, 0.0),
            v2(0.0, 0.0),
            v2(-0.5, 3f64.sqrt() / 2.0),
        )
        .unwrap();
        assert_eq!((b.a0, b.a1, b.value), (0.0, 120.0, 120.0));
        assert!(angular(v2(0.0, 0.0), v2(1.0, 0.0), v2(0.0, 1.0), v2(-2.0, 0.0)).is_none());
    }

    #[test]
    fn arc_and_ray() {
        assert!(
            (arc_length(5.0, 0.0, std::f64::consts::PI) - 5.0 * std::f64::consts::PI).abs() < 1e-12
        );
        let sq = [v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0), v2(0.0, 1.0)];
        assert!(ray_hits(v2(-1.0, 0.5), v2(1.0, 0.0), &sq, 1e-9));
        assert!(!ray_hits(v2(-1.0, 0.5), v2(-1.0, 0.0), &sq, 1e-9));
        assert!(!ray_hits(v2(-1.0, 1.5), v2(1.0, 0.0), &sq, 1e-9));
    }
}
