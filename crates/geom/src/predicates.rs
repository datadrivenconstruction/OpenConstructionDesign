// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Exact predicates: an f64 fast path with an error bound (Shewchuk),
//! falling back to an exact sum of products of the original coordinates when uncertain.

use crate::math::{V2, V3};

/// > 0: `c` is left of a→b, < 0: right, 0: exactly on the line.
pub fn orient2d(a: V2, b: V2, c: V2) -> f64 {
    let detl = (a.x - c.x) * (b.y - c.y);
    let detr = (a.y - c.y) * (b.x - c.x);
    let det = detl - detr;
    let bound = (3.0 + 16.0 * f64::EPSILON) * f64::EPSILON * (detl.abs() + detr.abs());
    if det.abs() > bound {
        return det;
    }
    exact_orient2d(a, b, c)
}

/// Allowed coordinate magnitude for exact predicates: outside the range
/// [`COORD_MIN`, `COORD_MAX`] (except zero) the `two_prod` remainder is lost in subnormals
/// or the product overflows, and the sign is no longer exact.
pub const COORD_MIN: f64 = 1e-150;
pub const COORD_MAX: f64 = 1e150;

/// The coordinate is valid for exact predicates (zero, or magnitude within range).
pub fn coord_in_range(v: f64) -> bool {
    v == 0.0 || (COORD_MIN..=COORD_MAX).contains(&v.abs())
}

/// a*b = hi + lo with no rounding error (for |a|,|b| in the allowed range).
fn two_prod(a: f64, b: f64) -> [f64; 2] {
    debug_assert!(
        coord_in_range(a) && coord_in_range(b),
        "coordinate outside exact range: {a}, {b}"
    );
    let p = a * b;
    [p, a.mul_add(b, -p)]
}

/// a + b = hi + lo with no rounding error.
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

/// Sign-exact sum: each term grows a nonoverlapping expansion (Shewchuk's grow-expansion with
/// zero elimination), O(terms x components) instead of repeated full cascades.
fn exact_sum(v: Vec<f64>) -> f64 {
    let mut h: Vec<f64> = Vec::with_capacity(16);
    for b in v {
        let mut q = b;
        let mut k = 0;
        for i in 0..h.len() {
            let (s, e) = two_sum(q, h[i]);
            q = s;
            if e != 0.0 {
                h[k] = e;
                k += 1;
            }
        }
        h.truncate(k);
        if q != 0.0 {
            h.push(q);
        }
    }
    // Increasing, nonoverlapping components: the largest one determines the sign.
    match h.split_last() {
        Some((top, rest)) => *top + rest.iter().sum::<f64>(),
        None => 0.0,
    }
}

fn exact_orient2d(a: V2, b: V2, c: V2) -> f64 {
    // det = ax*by - ax*cy - cx*by - ay*bx + ay*cx + cy*bx
    let terms = [
        two_prod(a.x, b.y),
        two_prod(-a.x, c.y),
        two_prod(-c.x, b.y),
        two_prod(-a.y, b.x),
        two_prod(a.y, c.x),
        two_prod(c.y, b.x),
    ];
    exact_sum(terms.iter().flatten().copied().collect())
}

/// `p` lies on the closed segment a–b (exact).
pub fn on_segment(a: V2, b: V2, p: V2) -> bool {
    orient2d(a, b, p) == 0.0
        && p.x >= a.x.min(b.x)
        && p.x <= a.x.max(b.x)
        && p.y >= a.y.min(b.y)
        && p.y <= a.y.max(b.y)
}

/// Closed segments a–b and c–d share a point (crossing, touching, overlap).
pub fn segments_meet(a: V2, b: V2, c: V2, d: V2) -> bool {
    let (d1, d2) = (orient2d(a, b, c), orient2d(a, b, d));
    let (d3, d4) = (orient2d(c, d, a), orient2d(c, d, b));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return true;
    }
    on_segment(a, b, c) || on_segment(a, b, d) || on_segment(c, d, a) || on_segment(c, d, b)
}

/// Segments sharing vertex `s` (s–q and s–o) overlap along a common line.
pub fn shared_end_overlap(s: V2, q: V2, o: V2) -> bool {
    orient2d(s, q, o) == 0.0 && (q.x - s.x) * (o.x - s.x) + (q.y - s.y) * (o.y - s.y) > 0.0
}

/// Point position relative to a ring: 1 inside, 0 on the boundary, -1 outside.
/// Parity of ray crossings to the right; duplicated edges (bridges) cancel each other.
pub fn point_in_ring(p: V2, ring: &[V2]) -> i8 {
    let n = ring.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if on_segment(a, b, p) {
            return 0;
        }
        if (a.y > p.y) != (b.y > p.y) {
            let o = orient2d(a, b, p);
            if (o > 0.0) == (b.y > a.y) {
                inside = !inside;
            }
        }
    }
    if inside {
        1
    } else {
        -1
    }
}

/// > 0: `d` is below plane a,b,c (a→b→c counterclockwise viewed from above), < 0: above, 0: exactly on it.
/// Sign of det[a-d; b-d; c-d]; when uncertain, an exact sum of triple products of coordinates.
pub fn orient3d(a: V3, b: V3, c: V3, d: V3) -> f64 {
    let (ad, bd, cd) = (a - d, b - d, c - d);
    let t1 = ad.x * (bd.y * cd.z - bd.z * cd.y);
    let t2 = ad.y * (bd.z * cd.x - bd.x * cd.z);
    let t3 = ad.z * (bd.x * cd.y - bd.y * cd.x);
    let det = t1 + t2 + t3;
    let perm = ad.x.abs() * ((bd.y * cd.z).abs() + (bd.z * cd.y).abs())
        + ad.y.abs() * ((bd.z * cd.x).abs() + (bd.x * cd.z).abs())
        + ad.z.abs() * ((bd.x * cd.y).abs() + (bd.y * cd.x).abs());
    if det.abs() > (7.0 + 56.0 * f64::EPSILON) * f64::EPSILON * perm {
        return det;
    }
    // Two equal points are two equal rows of the 4x4 determinant: exactly zero. Neighbouring
    // triangles of one mesh hit this all the time; the bound above cannot (it is 0 vs 0).
    let same = |p: V3, q: V3| p.x == q.x && p.y == q.y && p.z == q.z;
    if same(a, d) || same(b, d) || same(c, d) || same(a, b) || same(b, c) || same(a, c) {
        return 0.0;
    }
    exact_orient3d(a, b, c, d)
}

/// x·y·z exactly: four terms.
fn three_prod(x: f64, y: f64, z: f64, out: &mut Vec<f64>) {
    let [h, l] = two_prod(x, y);
    out.extend(two_prod(h, z));
    if l != 0.0 {
        out.extend(two_prod(l, z));
    }
}

fn exact_orient3d(a: V3, b: V3, c: V3, d: V3) -> f64 {
    // det of [[ax ay az 1],[bx by bz 1],[cx cy cz 1],[dx dy dz 1]] by Leibniz over 4x4 rows
    let r = [
        [a.x, a.y, a.z],
        [b.x, b.y, b.z],
        [c.x, c.y, c.z],
        [d.x, d.y, d.z],
    ];
    let mut terms = Vec::with_capacity(96);
    // permutations of 4 rows assigning columns x,y,z to rows i,j,k (the 4th row takes the 1)
    for i in 0..4 {
        for j in 0..4 {
            for k in 0..4 {
                if i == j || j == k || i == k {
                    continue;
                }
                let l = 6 - i - j - k;
                let p = [i, j, k, l];
                let mut inv = 0;
                for u in 0..4 {
                    for v in u + 1..4 {
                        if p[u] > p[v] {
                            inv += 1;
                        }
                    }
                }
                let s = if inv % 2 == 0 { 1.0 } else { -1.0 };
                three_prod(s * r[i][0], r[j][1], r[k][2], &mut terms);
            }
        }
    }
    terms.retain(|t| *t != 0.0);
    if terms.is_empty() {
        return 0.0;
    }
    // det4 with a column of ones equals det[a-d; b-d; c-d]
    exact_sum(terms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v2;

    #[test]
    fn orient3d_is_exact_on_plane_far_from_origin() {
        use crate::math::v3;
        let o = 1.0e5;
        let (a, b, c) = (v3(o, o, 1.0), v3(o + 1.0, o, 1.0), v3(o, o + 1.0, 1.0));
        assert_eq!(orient3d(a, b, c, v3(o + 0.3, o + 0.7, 1.0)), 0.0);
        assert!(orient3d(a, b, c, v3(o + 0.3, o + 0.7, 1.0 - 1e-12)) > 0.0);
        assert!(orient3d(a, b, c, v3(o + 0.3, o + 0.7, 1.0 + 1e-12)) < 0.0);
        let (p, q, r) = (v3(0.1, 0.2, 0.3), v3(0.7, 0.1, 0.9), v3(0.3, 0.8, 0.2));
        let f = orient3d(p, q, r, v3(0.5, 0.5, 0.5));
        assert_eq!(
            f.signum(),
            exact_orient3d(p, q, r, v3(0.5, 0.5, 0.5)).signum()
        );
    }

    #[test]
    fn sign_is_exact_far_from_origin() {
        let base = 1.0e6; // 1000 km from the origin
        let a = v2(base, base);
        let b = v2(base + 1.0, base + 1.0);
        assert_eq!(orient2d(a, b, v2(base + 0.5, base + 0.5)), 0.0);
        assert!(orient2d(a, b, v2(base + 0.5, base + 0.5 + 1e-9)) > 0.0);
        assert!(orient2d(a, b, v2(base + 0.5, base + 0.5 - 1e-9)) < 0.0);
    }

    #[test]
    fn near_degenerate_grid_is_consistent() {
        // Classic test: points near the line y=x spaced 1 ulp apart.
        let a = v2(12.0, 12.0);
        let b = v2(24.0, 24.0);
        let ulp = f64::EPSILON * 0.5;
        for i in 0..64 {
            for j in 0..64 {
                let p = v2(0.5 + i as f64 * ulp, 0.5 + j as f64 * ulp);
                let s = orient2d(a, b, p);
                let expect = (j as i64 - i as i64).signum() as f64;
                assert_eq!(s.signum() * (s != 0.0) as i32 as f64, expect, "i={i} j={j}");
            }
        }
    }

    #[test]
    fn exact_at_range_boundary() {
        let (lo, hi) = (COORD_MIN, COORD_MAX);
        assert_eq!(
            orient2d(v2(lo, lo), v2(2.0 * lo, 2.0 * lo), v2(3.0 * lo, 3.0 * lo)),
            0.0
        );
        assert!(orient2d(v2(0., 0.), v2(hi, 0.), v2(hi, hi)) > 0.0);
        assert!(orient2d(v2(lo, 0.), v2(2.0 * lo, 0.), v2(lo, lo)) > 0.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "outside exact range")]
    fn below_range_is_caught_in_debug() {
        let t = 1e-200;
        orient2d(v2(t, t), v2(2.0 * t, 2.0 * t), v2(3.0 * t, 3.0 * t));
    }

    #[test]
    fn point_in_ring_classifies() {
        let sq = [v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)];
        assert_eq!(point_in_ring(v2(1., 1.), &sq), 1);
        assert_eq!(point_in_ring(v2(2., 1.), &sq), 0);
        assert_eq!(point_in_ring(v2(0., 0.), &sq), 0);
        assert_eq!(point_in_ring(v2(3., 0.), &sq), -1);
        assert_eq!(point_in_ring(v2(-1., 2.), &sq), -1);
    }
}
