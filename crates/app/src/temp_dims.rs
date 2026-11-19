//! Temporary dimensions for modeling tools: from a support to a point, and segment length.
//! Supports are grid axes, reference planes and wall axes. Entering a value moves the point along the dimension.

use ddc_geom::{v2, V2};

/// Dimension from `a` (support) to `b` (movable point); value is the length, m.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempDim {
    pub a: V2,
    pub b: V2,
    pub value: f64,
}

impl TempDim {
    fn new(a: V2, b: V2) -> Self {
        TempDim {
            a,
            b,
            value: (b - a).len(),
        }
    }

    /// Point at which the dimension equals `v` (direction is preserved).
    pub fn retarget(&self, v: f64) -> V2 {
        let d = self.b - self.a;
        let l = d.len();
        if l < 1e-9 {
            return self.a + v2(v, 0.0);
        }
        self.a + d * (v / l)
    }
}

/// Beyond this distance supports yield no temporary dimension, m.
pub const REACH: f64 = 30.0;

fn foot(p: V2, a: V2, b: V2) -> Option<(V2, V2)> {
    let d = b - a;
    let l = d.len();
    if l < 1e-9 {
        return None;
    }
    let u = d * (1.0 / l);
    Some((a + u * (p - a).dot(u), u))
}

/// Nearest supports to the point, at most one per direction (up to two dimensions).
pub fn for_point(p: V2, refs: &[(V2, V2)]) -> Vec<TempDim> {
    let mut c: Vec<(f64, V2, V2)> = refs
        .iter()
        .filter_map(|&(a, b)| foot(p, a, b).map(|(f, u)| ((p - f).len(), f, u)))
        .filter(|(d, ..)| *d > 1e-6 && *d < REACH)
        .collect();
    c.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(TempDim, V2)> = vec![];
    for (_, f, u) in c {
        if out.iter().all(|(_, w)| u.cross(*w).abs() > 0.3) {
            out.push((TempDim::new(f, p), u));
        }
        if out.len() == 2 {
            break;
        }
    }
    out.into_iter().map(|x| x.0).collect()
}

/// Segment a→b: length (first) and distance to the nearest parallel support.
pub fn for_segment(a: V2, b: V2, refs: &[(V2, V2)]) -> Vec<TempDim> {
    if (b - a).len() < 1e-6 {
        return for_point(b, refs);
    }
    let dir = b - a;
    let mut out = vec![TempDim::new(a, b)];
    let par = refs
        .iter()
        .filter(|(p, q)| {
            let d = *q - *p;
            d.len() > 1e-9 && (d.cross(dir) / (d.len() * dir.len())).abs() < 0.02
        })
        .filter_map(|&(p, q)| foot(b, p, q).map(|(f, _)| ((b - f).len(), f)))
        .filter(|(d, _)| *d > 1e-6 && *d < REACH)
        .min_by(|x, y| x.0.total_cmp(&y.0));
    if let Some((_, f)) = par {
        out.push(TempDim::new(f, b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_gets_two_orthogonal_dims() {
        let refs = [
            (v2(0.0, -10.0), v2(0.0, 10.0)),
            (v2(1.0, -10.0), v2(1.0, 10.0)),
            (v2(-10.0, 0.0), v2(10.0, 0.0)),
        ];
        let d = for_point(v2(1.5, 2.0), &refs);
        assert_eq!(d.len(), 2);
        assert!((d[0].value - 0.5).abs() < 1e-9, "nearest x reference first");
        assert!((d[1].value - 2.0).abs() < 1e-9);
        let q = d[0].retarget(1.2);
        assert!((q - v2(2.2, 2.0)).len() < 1e-9);
    }

    #[test]
    fn segment_length_and_parallel_offset() {
        let refs = [(v2(0.0, 0.0), v2(10.0, 0.0))];
        let d = for_segment(v2(1.0, 3.0), v2(5.0, 3.0), &refs);
        assert!((d[0].value - 4.0).abs() < 1e-9);
        assert!((d[1].value - 3.0).abs() < 1e-9);
        assert!((d[0].retarget(6.0) - v2(7.0, 3.0)).len() < 1e-9);
    }
}
