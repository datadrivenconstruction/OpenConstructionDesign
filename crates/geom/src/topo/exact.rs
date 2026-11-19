//! Exact in-circle predicate: an f64 filter with an error bound; when the filter is inconclusive,
//! the determinant is expanded into products of four differences, each an exact sum.

use crate::math::V2;

fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

fn two_prod(a: f64, b: f64) -> (f64, f64) {
    let p = a * b;
    (p, a.mul_add(b, -p))
}

/// Exact a − b: high part and remainder (remainder is 0 when the difference is representable).
fn two_diff(a: f64, b: f64) -> [f64; 2] {
    let (s, e) = two_sum(a, -b);
    [s, e]
}

/// Sign-exact sum (two_sum cascade until convergence; the leading term then determines the sign).
fn exact_sum(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    for _ in 0..v.len() {
        let mut changed = false;
        for i in 1..v.len() {
            let (s, e) = two_sum(v[i], v[i - 1]);
            changed |= e != v[i - 1];
            v[i] = s;
            v[i - 1] = e;
        }
        if !changed {
            break;
        }
    }
    let (top, rest) = v.split_last().expect("non-empty");
    *top + rest.iter().sum::<f64>()
}

/// Product of four numbers without rounding (8 terms), multiplied by `sign`.
fn prod4(sign: f64, f: [f64; 4], out: &mut Vec<f64>) {
    let (p, e) = two_prod(f[0] * sign, f[1]);
    let mut a = [0.0; 8];
    let mut n = 0;
    for x in [p, e] {
        let (h, l) = two_prod(x, f[2]);
        a[n] = h;
        a[n + 1] = l;
        n += 2;
    }
    for &x in &a[..4] {
        let (h, l) = two_prod(x, f[3]);
        if h != 0.0 {
            out.push(h);
        }
        if l != 0.0 {
            out.push(l);
        }
    }
}

/// > 0: `d` is strictly inside the circle through a, b, c (counter-clockwise); < 0: outside; 0: on it.
pub fn incircle(a: V2, b: V2, c: V2, d: V2) -> f64 {
    let (adx, ady) = (a.x - d.x, a.y - d.y);
    let (bdx, bdy) = (b.x - d.x, b.y - d.y);
    let (cdx, cdy) = (c.x - d.x, c.y - d.y);
    let bdxcdy = bdx * cdy;
    let cdxbdy = cdx * bdy;
    let alift = adx * adx + ady * ady;
    let cdxady = cdx * ady;
    let adxcdy = adx * cdy;
    let blift = bdx * bdx + bdy * bdy;
    let adxbdy = adx * bdy;
    let bdxady = bdx * ady;
    let clift = cdx * cdx + cdy * cdy;
    let det = alift * (bdxcdy - cdxbdy) + blift * (cdxady - adxcdy) + clift * (adxbdy - bdxady);
    let perm = (bdxcdy.abs() + cdxbdy.abs()) * alift
        + (cdxady.abs() + adxcdy.abs()) * blift
        + (adxbdy.abs() + bdxady.abs()) * clift;
    let e = f64::EPSILON;
    let bound = (10.0 + 96.0 * e) * e * perm;
    if det > bound || -det > bound {
        return det;
    }
    exact_incircle(a, b, c, d)
}

fn exact_incircle(a: V2, b: V2, c: V2, d: V2) -> f64 {
    let ax = two_diff(a.x, d.x);
    let ay = two_diff(a.y, d.y);
    let bx = two_diff(b.x, d.x);
    let by = two_diff(b.y, d.y);
    let cx = two_diff(c.x, d.x);
    let cy = two_diff(c.y, d.y);
    // det = Σ lift_i · cross_jk; lift = x² + y², cross = x_j·y_k − x_k·y_j
    let terms: [(f64, [[f64; 2]; 4]); 12] = [
        (1.0, [ax, ax, bx, cy]),
        (1.0, [ay, ay, bx, cy]),
        (-1.0, [ax, ax, cx, by]),
        (-1.0, [ay, ay, cx, by]),
        (1.0, [bx, bx, cx, ay]),
        (1.0, [by, by, cx, ay]),
        (-1.0, [bx, bx, ax, cy]),
        (-1.0, [by, by, ax, cy]),
        (1.0, [cx, cx, ax, by]),
        (1.0, [cy, cy, ax, by]),
        (-1.0, [cx, cx, bx, ay]),
        (-1.0, [cy, cy, bx, ay]),
    ];
    let mut out = Vec::with_capacity(12 * 8);
    for (sign, f) in terms {
        // (h0 + l0)(h1 + l1)(h2 + l2)(h3 + l3): all combinations of parts, zeros skipped
        for mask in 0u32..16 {
            let pick = |k: usize| f[k][((mask >> k) & 1) as usize];
            let g = [pick(0), pick(1), pick(2), pick(3)];
            if g.iter().any(|&x| x == 0.0) {
                continue;
            }
            prod4(sign, g, &mut out);
        }
    }
    exact_sum(&mut out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v2;

    #[test]
    fn unit_square_corners_are_cocircular() {
        let (a, b, c) = (v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0));
        assert_eq!(incircle(a, b, c, v2(0.0, 1.0)), 0.0);
        assert!(incircle(a, b, c, v2(0.5, 0.5)) > 0.0);
        assert!(incircle(a, b, c, v2(2.0, 2.0)) < 0.0);
    }

    #[test]
    fn exact_far_from_origin() {
        let o = 1.0e6;
        let (a, b, c) = (v2(o, o), v2(o + 0.1, o), v2(o + 0.1, o + 0.1));
        assert_eq!(incircle(a, b, c, v2(o, o + 0.1)), 0.0);
        assert!(incircle(a, b, c, v2(o + 1e-10, o + 0.1 - 1e-10)) > 0.0);
        assert!(incircle(a, b, c, v2(o - 1e-9, o + 0.1 + 1e-9)) < 0.0);
    }
}
