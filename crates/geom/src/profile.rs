//! Planar profile: outer loop counter-clockwise, holes clockwise.

use crate::math::{v2, Tol, V2};
use crate::predicates::{coord_in_range, point_in_ring, segments_meet, shared_end_overlap};

#[derive(Debug, PartialEq)]
pub enum ProfileError {
    /// A vertex coordinate is non-finite or outside the exact-predicate range.
    BadCoordinate {
        ring: usize,
        vertex: usize,
    },
    TooFewPoints {
        ring: usize,
    },
    ZeroLengthEdge {
        ring: usize,
        edge: usize,
    },
    ZeroArea {
        ring: usize,
    },
    /// Edges `a` and `b` of the loop cross, touch or overlap.
    SelfIntersection {
        ring: usize,
        a: usize,
        b: usize,
    },
    /// Hole `hole` (index in the hole list) is not strictly inside the outer loop.
    HoleOutside {
        hole: usize,
    },
    /// Holes `a` and `b` cross, touch or are nested.
    HolesOverlap {
        a: usize,
        b: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub outer: Vec<V2>,
    pub holes: Vec<Vec<V2>>,
}

pub fn signed_area(pts: &[V2]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| pts[i].cross(pts[(i + 1) % n])).sum::<f64>() * 0.5
}

/// First pair of edges from two different rings sharing a point. O(n·m); a sweep-line could replace it.
fn rings_meet(p: &[V2], q: &[V2]) -> bool {
    let (n, m) = (p.len(), q.len());
    (0..n).any(|i| (0..m).any(|j| segments_meet(p[i], p[(i + 1) % n], q[j], q[(j + 1) % m])))
}

impl Profile {
    /// Validates the loops and normalises orientation; errors name the loop and edges.
    pub fn new(outer: Vec<V2>, holes: Vec<Vec<V2>>, tol: Tol) -> Result<Self, ProfileError> {
        Self::build(outer, holes, tol, false)
    }

    /// As `new`, but a near-collinear back turn (`ring_spikes`) is let through: for outlines
    /// saved before such spikes were refused, so legacy data still draws and stays editable.
    pub fn new_keeping_spikes(
        outer: Vec<V2>,
        holes: Vec<Vec<V2>>,
        tol: Tol,
    ) -> Result<Self, ProfileError> {
        Self::build(outer, holes, tol, true)
    }

    fn build(
        mut outer: Vec<V2>,
        mut holes: Vec<Vec<V2>>,
        tol: Tol,
        spikes_ok: bool,
    ) -> Result<Self, ProfileError> {
        for (ring, pts) in std::iter::once(&mut outer)
            .chain(holes.iter_mut())
            .enumerate()
        {
            check_ring(ring, pts, tol, spikes_ok)?;
            if (signed_area(pts) > 0.0) != (ring == 0) {
                pts.reverse();
            }
        }
        check_holes(&outer, &holes)?;
        Ok(Self { outer, holes })
    }

    /// Rectangle w×h; zero or non-finite sizes yield a typed error.
    pub fn rectangle(w: f64, h: f64) -> Result<Self, ProfileError> {
        Self::new(
            vec![v2(0.0, 0.0), v2(w, 0.0), v2(w, h), v2(0.0, h)],
            vec![],
            Tol::default(),
        )
    }

    /// Area net of holes.
    pub fn area(&self) -> f64 {
        self.rings().map(|r| signed_area(r)).sum()
    }

    pub fn rings(&self) -> impl Iterator<Item = &Vec<V2>> {
        std::iter::once(&self.outer).chain(self.holes.iter())
    }
}

fn check_ring(ring: usize, pts: &[V2], tol: Tol, spikes_ok: bool) -> Result<(), ProfileError> {
    let n = pts.len();
    if n < 3 {
        return Err(ProfileError::TooFewPoints { ring });
    }
    if let Some(vertex) = pts
        .iter()
        .position(|p| !(coord_in_range(p.x) && coord_in_range(p.y)))
    {
        return Err(ProfileError::BadCoordinate { ring, vertex });
    }
    for edge in 0..n {
        if (pts[(edge + 1) % n] - pts[edge]).len() <= tol.length {
            return Err(ProfileError::ZeroLengthEdge { ring, edge });
        }
    }
    if signed_area(pts).abs() <= tol.length * tol.length {
        return Err(ProfileError::ZeroArea { ring });
    }
    // O(n²) is enough for BIM profiles (tens of points); larger inputs need a sweep-line.
    for a in 0..n {
        let (a0, a1) = (pts[a], pts[(a + 1) % n]);
        for b in a + 1..n {
            let (b0, b1) = (pts[b], pts[(b + 1) % n]);
            let bad = if b == a + 1 {
                // Adjacent: shared vertex a1; only a reversal back along the edge is forbidden.
                shared_end_overlap(a1, a0, b1) || (!spikes_ok && back_turn(a0, a1, b1, tol))
            } else if a == 0 && b == n - 1 {
                shared_end_overlap(a0, a1, b0) || (!spikes_ok && back_turn(b0, a0, a1, tol))
            } else {
                // Non-adjacent: any shared point (including a repeated vertex, figure-eight).
                segments_meet(a0, a1, b0, b1)
            };
            if bad {
                return Err(ProfileError::SelfIntersection { ring, a, b });
            }
        }
    }
    Ok(())
}

/// Turn back at `q` within the angle tolerance: a 45-degree snap leaves the reversed vertex
/// ~1e-16 off the ray, so the exact predicate alone let that sliver spike through.
fn back_turn(p: V2, q: V2, r: V2, tol: Tol) -> bool {
    let (u, v) = (q - p, r - q);
    let cross = u.x * v.y - u.y * v.x;
    u.x * v.x + u.y * v.y < 0.0 && cross.abs() <= tol.angle * u.len() * v.len()
}

/// Tips of the spikes of a closed ring: vertices where the outline turns back along itself.
pub fn ring_spikes(pts: &[V2], tol: Tol) -> Vec<usize> {
    let n = pts.len();
    if n < 3 {
        return vec![];
    }
    (0..n)
        .filter(|&i| back_turn(pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n], tol))
        .collect()
}

/// The ring without its spikes: each tip is dropped (the outline then runs straight on), and
/// a vertex left on top of its neighbour goes too. Stops at three vertices.
pub fn remove_spikes(pts: &[V2], tol: Tol) -> Vec<V2> {
    let mut out = pts.to_vec();
    while out.len() > 3 {
        if let Some(&i) = ring_spikes(&out, tol).first() {
            out.remove(i);
            continue;
        }
        let n = out.len();
        match (0..n).find(|&i| (out[(i + 1) % n] - out[i]).len() <= tol.length) {
            Some(i) => {
                out.remove((i + 1) % n);
            }
            None => break,
        }
    }
    out
}

/// Every hole is strictly inside the outer loop; holes do not touch each other.
fn check_holes(outer: &[V2], holes: &[Vec<V2>]) -> Result<(), ProfileError> {
    for (h, hole) in holes.iter().enumerate() {
        if rings_meet(outer, hole) || point_in_ring(hole[0], outer) != 1 {
            return Err(ProfileError::HoleOutside { hole: h });
        }
    }
    for a in 0..holes.len() {
        for b in a + 1..holes.len() {
            let (p, q) = (&holes[a], &holes[b]);
            if rings_meet(p, q) || point_in_ring(p[0], q) != -1 || point_in_ring(q[0], p) != -1 {
                return Err(ProfileError::HolesOverlap { a, b });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bow_tie_is_rejected_with_location() {
        let r = Profile::new(
            vec![v2(0., 0.), v2(2., 2.), v2(2., 0.), v2(0., 1.)],
            vec![],
            Tol::default(),
        );
        assert_eq!(
            r.unwrap_err(),
            ProfileError::SelfIntersection {
                ring: 0,
                a: 0,
                b: 2
            }
        );
    }

    #[test]
    fn orientation_is_normalised_and_holes_subtract() {
        let outer = vec![v2(0., 0.), v2(0., 2.), v2(3., 2.), v2(3., 0.)];
        let hole = vec![v2(1., 0.5), v2(2., 0.5), v2(2., 1.5), v2(1., 1.5)];
        let p = Profile::new(outer, vec![hole], Tol::default()).unwrap();
        assert!((p.area() - 5.0).abs() < 1e-12);
    }

    #[test]
    fn zero_length_edge_is_named() {
        let r = Profile::new(
            vec![v2(0., 0.), v2(0., 0.), v2(1., 0.), v2(0., 1.)],
            vec![],
            Tol::default(),
        );
        assert_eq!(
            r.unwrap_err(),
            ProfileError::ZeroLengthEdge { ring: 0, edge: 0 }
        );
    }

    fn sq(x: f64, y: f64, s: f64) -> Vec<V2> {
        vec![v2(x, y), v2(x + s, y), v2(x + s, y + s), v2(x, y + s)]
    }

    fn err(outer: Vec<V2>, holes: Vec<Vec<V2>>) -> ProfileError {
        Profile::new(outer, holes, Tol::default()).unwrap_err()
    }

    #[test]
    fn collinear_overlap_is_rejected() {
        // Vertex (2,0) lies on edge 0; then the edge overlaps along the x axis.
        let r = vec![
            v2(0., 0.),
            v2(4., 0.),
            v2(4., 1.),
            v2(2., 1.),
            v2(2., 0.),
            v2(1., 0.),
            v2(0., -1.),
        ];
        assert!(matches!(
            err(r, vec![]),
            ProfileError::SelfIntersection { ring: 0, .. }
        ));
        // Reversal back along the adjacent edge (spike).
        let spike = vec![v2(0., 0.), v2(4., 0.), v2(2., 0.), v2(2., 2.)];
        assert_eq!(
            err(spike, vec![]),
            ProfileError::SelfIntersection {
                ring: 0,
                a: 0,
                b: 1
            }
        );
    }

    /// Creation fuzz seed 3, op 60 (loop Floor): a 45-degree snap back along the last edge left
    /// the turn vertex a hair off the ray; the slab was written with a sliver spike.
    #[test]
    fn near_collinear_back_turn_is_rejected() {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let (a, b) = (v2(1., 1.), v2(4., 4.));
        let c = b + v2(-s, -s) * 1.5;
        let c = v2(c.x, c.y + 1e-15);
        assert!(!crate::predicates::shared_end_overlap(b, a, c));
        let r = Profile::new(vec![a, b, c, v2(0., 3.)], vec![], Tol::default());
        assert!(
            matches!(r, Err(ProfileError::SelfIntersection { ring: 0, a: 0, b: 1 })),
            "{r:?}"
        );
        // the same spike across the ring's wrap is caught too
        let r = Profile::new(vec![b, c, v2(0., 3.), a], vec![], Tol::default());
        assert!(
            matches!(r, Err(ProfileError::SelfIntersection { ring: 0, a: 0, b: 3 })),
            "{r:?}"
        );
        // a genuine shallow turn stays valid
        let ok = vec![v2(0., 0.), v2(4., 0.), v2(8., 1e-3), v2(4., 3.)];
        assert!(Profile::new(ok, vec![], Tol::default()).is_ok());
    }

    /// A saved spike: still readable, its tip is found, and the clean-up gives a valid ring.
    #[test]
    fn legacy_spike_is_kept_found_and_cleaned() {
        let s = std::f64::consts::FRAC_1_SQRT_2;
        let (a, b) = (v2(1., 1.), v2(4., 4.));
        let c = b + v2(-s, -s) * 1.5;
        let c = v2(c.x, c.y + 1e-15);
        let ring = vec![a, b, c, v2(0., 3.)];
        let tol = Tol::default();
        assert!(Profile::new(ring.clone(), vec![], tol).is_err());
        assert!(Profile::new_keeping_spikes(ring.clone(), vec![], tol).is_ok());
        assert_eq!(ring_spikes(&ring, tol), vec![1]);
        let clean = remove_spikes(&ring, tol);
        assert_eq!(clean, vec![a, c, v2(0., 3.)]);
        assert!(ring_spikes(&clean, tol).is_empty());
        assert!(Profile::new(clean, vec![], tol).is_ok());
        // a ring without spikes comes back unchanged
        let sq = vec![v2(0., 0.), v2(2., 0.), v2(2., 2.), v2(0., 2.)];
        assert_eq!(remove_spikes(&sq, tol), sq);
    }

    #[test]
    fn figure_eight_repeated_vertex_is_rejected() {
        let r = vec![
            v2(0., 0.),
            v2(1., 1.),
            v2(2., 0.),
            v2(2., 2.),
            v2(1., 1.),
            v2(0., 2.),
        ];
        assert!(matches!(
            err(r, vec![]),
            ProfileError::SelfIntersection { ring: 0, .. }
        ));
    }

    #[test]
    fn vertex_touching_edge_is_rejected() {
        let r = vec![v2(0., 0.), v2(4., 0.), v2(4., 2.), v2(2., 0.), v2(0., 2.)];
        assert!(matches!(
            err(r, vec![]),
            ProfileError::SelfIntersection { ring: 0, .. }
        ));
    }

    #[test]
    fn holes_outside_or_touching_outer_are_named() {
        let o = || sq(0., 0., 10.);
        assert_eq!(
            err(o(), vec![sq(1., 1., 1.), sq(20., 20., 1.)]),
            ProfileError::HoleOutside { hole: 1 }
        );
        assert_eq!(
            err(o(), vec![sq(0., 2., 1.)]),
            ProfileError::HoleOutside { hole: 0 }
        );
        assert_eq!(
            err(o(), vec![sq(9., 9., 2.)]),
            ProfileError::HoleOutside { hole: 0 }
        );
        assert_eq!(
            err(sq(1., 1., 1.), vec![o()]),
            ProfileError::HoleOutside { hole: 0 }
        );
    }

    #[test]
    fn overlapping_touching_nested_holes_are_named() {
        let o = || sq(0., 0., 10.);
        assert_eq!(
            err(o(), vec![sq(1., 1., 2.), sq(2., 2., 2.)]),
            ProfileError::HolesOverlap { a: 0, b: 1 }
        );
        assert_eq!(
            err(o(), vec![sq(1., 1., 1.), sq(2., 1., 1.)]),
            ProfileError::HolesOverlap { a: 0, b: 1 }
        );
        assert_eq!(
            err(o(), vec![sq(1., 1., 1.), sq(2., 2., 1.)]),
            ProfileError::HolesOverlap { a: 0, b: 1 }
        );
        assert_eq!(
            err(o(), vec![sq(5., 5., 1.), sq(1., 1., 6.)]),
            ProfileError::HolesOverlap { a: 0, b: 1 }
        );
        assert!(Profile::new(o(), vec![sq(1., 1., 1.), sq(3., 1., 1.)], Tol::default()).is_ok());
    }

    #[test]
    fn degenerate_rectangle_is_error_not_panic() {
        assert!(Profile::rectangle(0.0, 1.0).is_err());
        assert_eq!(
            Profile::rectangle(f64::NAN, 1.0).unwrap_err(),
            ProfileError::BadCoordinate { ring: 0, vertex: 1 }
        );
        assert!(Profile::rectangle(1e-200, 1.0).is_err());
        assert!((Profile::rectangle(2.0, 3.0).unwrap().area() - 6.0).abs() < 1e-12);
    }
}
