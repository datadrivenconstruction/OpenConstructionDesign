//! Exact minimum distance between two triangle meshes (clearance), with the closest pair of
//! points. Touching or crossing meshes are at distance 0. Brute force with box pruning; a pair
//! of meshes whose triangle product exceeds `MAX_PAIRS` is refused, so a caller falls back to
//! boxes and says so.

use crate::math::{v3, V3};
use crate::mesh::Mesh;

/// Triangle pairs one call may test before it gives up.
pub const MAX_PAIRS: u64 = 25_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Closest {
    pub dist: f64,
    pub a: V3,
    pub b: V3,
}

type Tri = [V3; 3];

fn tris(m: &Mesh) -> Vec<Tri> {
    m.triangles
        .iter()
        .filter_map(|t| {
            let p = |i: u32| m.positions.get(i as usize).copied();
            Some([p(t[0])?, p(t[1])?, p(t[2])?])
        })
        .collect()
}

fn bounds(t: &Tri) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in t {
        for (k, v) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[k] = lo[k].min(v);
            hi[k] = hi[k].max(v);
        }
    }
    (lo, hi)
}

fn box_gap(a: &([f64; 3], [f64; 3]), b: &([f64; 3], [f64; 3])) -> f64 {
    let mut s = 0.0;
    for k in 0..3 {
        let d = (a.0[k] - b.1[k]).max(b.0[k] - a.1[k]).max(0.0);
        s += d * d;
    }
    s.sqrt()
}

/// Closest point of triangle `abc` to `p`.
pub fn closest_on_triangle(p: V3, a: V3, b: V3, c: V3) -> V3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + (c - b) * w;
    }
    let den = 1.0 / (va + vb + vc);
    a + ab * (vb * den) + ac * (vc * den)
}

/// Closest points of segments `p1q1` and `p2q2`.
pub fn closest_segments(p1: V3, q1: V3, p2: V3, q2: V3) -> (V3, V3) {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    let (s, t);
    if a <= 1e-24 && e <= 1e-24 {
        return (p1, p2);
    }
    if a <= 1e-24 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-24 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut s0 = if den > 1e-18 { ((b * f - c * e) / den).clamp(0.0, 1.0) } else { 0.0 };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

/// Does the segment `pq` pass through the triangle `abc`?
fn segment_hits(p: V3, q: V3, t: &Tri) -> bool {
    let d = q - p;
    let e1 = t[1] - t[0];
    let e2 = t[2] - t[0];
    let h = d.cross(e2);
    let det = e1.dot(h);
    if det.abs() < 1e-18 {
        return false;
    }
    let f = 1.0 / det;
    let s = p - t[0];
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return false;
    }
    let qv = s.cross(e1);
    let v = f * d.dot(qv);
    if v < 0.0 || u + v > 1.0 {
        return false;
    }
    (0.0..=1.0).contains(&(f * e2.dot(qv)))
}

fn tri_pair(x: &Tri, y: &Tri) -> Closest {
    let mut best = Closest { dist: f64::MAX, a: x[0], b: y[0] };
    let mut take = |a: V3, b: V3| {
        let d = (a - b).len();
        if d < best.dist {
            best = Closest { dist: d, a, b };
        }
    };
    for p in x {
        take(*p, closest_on_triangle(*p, y[0], y[1], y[2]));
    }
    for p in y {
        take(closest_on_triangle(*p, x[0], x[1], x[2]), *p);
    }
    for i in 0..3 {
        for j in 0..3 {
            let (a, b) = closest_segments(x[i], x[(i + 1) % 3], y[j], y[(j + 1) % 3]);
            take(a, b);
        }
    }
    let crosses = (0..3).any(|i| segment_hits(x[i], x[(i + 1) % 3], y))
        || (0..3).any(|i| segment_hits(y[i], y[(i + 1) % 3], x));
    if crosses {
        let m = (best.a + best.b) * 0.5;
        return Closest { dist: 0.0, a: m, b: m };
    }
    best
}

/// Minimum distance of two meshes; `None` when a mesh is empty or the pair count exceeds
/// `MAX_PAIRS`.
pub fn mesh_distance(a: &Mesh, b: &Mesh) -> Option<Closest> {
    let (ta, tb) = (tris(a), tris(b));
    if ta.is_empty() || tb.is_empty() || (ta.len() as u64) * (tb.len() as u64) > MAX_PAIRS {
        return None;
    }
    let bb: Vec<_> = tb.iter().map(bounds).collect();
    let mut best = Closest { dist: f64::MAX, a: v3(0.0, 0.0, 0.0), b: v3(0.0, 0.0, 0.0) };
    for x in &ta {
        let bx = bounds(x);
        for (y, by) in tb.iter().zip(&bb) {
            if box_gap(&bx, by) >= best.dist {
                continue;
            }
            let c = tri_pair(x, y);
            if c.dist < best.dist {
                best = c;
                if best.dist == 0.0 {
                    return Some(best);
                }
            }
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slab(z0: f64, z1: f64) -> Mesh {
        let p = |x: f64, y: f64, z: f64| v3(x, y, z);
        let positions = vec![
            p(0.0, 0.0, z0),
            p(1.0, 0.0, z0),
            p(1.0, 1.0, z0),
            p(0.0, 1.0, z0),
            p(0.0, 0.0, z1),
            p(1.0, 0.0, z1),
            p(1.0, 1.0, z1),
            p(0.0, 1.0, z1),
        ];
        let q = [
            [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7], [0, 1, 5], [0, 5, 4],
            [1, 2, 6], [1, 6, 5], [2, 3, 7], [2, 7, 6], [3, 0, 4], [3, 4, 7],
        ];
        Mesh { positions, triangles: q.to_vec(), face_of: vec![0; 12] }
    }

    #[test]
    fn parallel_boxes_are_the_gap_apart() {
        let d = mesh_distance(&slab(0.0, 1.0), &slab(1.25, 2.0)).unwrap();
        assert!((d.dist - 0.25).abs() < 1e-12, "{d:?}");
    }

    #[test]
    fn touching_and_crossing_boxes_are_at_zero() {
        assert_eq!(mesh_distance(&slab(0.0, 1.0), &slab(1.0, 2.0)).unwrap().dist, 0.0);
        assert_eq!(mesh_distance(&slab(0.0, 1.0), &slab(0.5, 2.0)).unwrap().dist, 0.0);
    }

    #[test]
    fn empty_mesh_has_no_distance() {
        let e = Mesh { positions: vec![], triangles: vec![], face_of: vec![] };
        assert!(mesh_distance(&e, &slab(0.0, 1.0)).is_none());
    }

    #[test]
    fn a_sliver_gap_is_measured_to_the_micron() {
        let d = mesh_distance(&slab(0.0, 1.0), &slab(1.0005, 2.0)).unwrap();
        assert!((d.dist - 0.0005).abs() < 1e-12);
    }
}
