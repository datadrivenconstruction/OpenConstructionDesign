// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Section planes of an issue view, in BCF terms: a point on the plane and a direction that
//! points to the clipped (hidden) side. The editor's section box is six axis-aligned planes;
//! planes from other tools may be oblique and are kept as they are.

use serde::{Deserialize, Serialize};

/// Cosine past which a plane counts as axis-aligned.
const AXIS_COS: f64 = 1.0 - 1e-9;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipPlane {
    pub at: [f64; 3],
    pub dir: [f64; 3],
}

/// The box the section planes cut out; a side without a plane is infinite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipBox {
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Planes that are not axis-aligned and so are not part of the box.
    pub oblique: usize,
}

impl ClipPlane {
    pub fn is_valid(&self) -> bool {
        let n = self.dir.iter().map(|v| v * v).sum::<f64>().sqrt();
        self.at.iter().chain(&self.dir).all(|v| v.is_finite()) && n > 1e-9
    }

    /// The six planes of a box; a non-finite side gets no plane.
    pub fn of_box(min: [f64; 3], max: [f64; 3]) -> Vec<ClipPlane> {
        let mut out = Vec::with_capacity(6);
        for axis in 0..3 {
            for (v, sign) in [(min[axis], -1.0), (max[axis], 1.0)] {
                if !v.is_finite() {
                    continue;
                }
                let mut at = [0.0; 3];
                let mut dir = [0.0; 3];
                at[axis] = v;
                dir[axis] = sign;
                out.push(ClipPlane { at, dir });
            }
        }
        out
    }

    /// Axis and side (`-1` hides below, `1` hides above) of an axis-aligned plane.
    fn axis(&self) -> Option<(usize, f64)> {
        let n = self.dir.iter().map(|v| v * v).sum::<f64>().sqrt();
        if n.is_nan() || n <= 1e-9 {
            return None;
        }
        (0..3).find_map(|i| {
            let c = self.dir[i] / n;
            (c.abs() >= AXIS_COS).then_some((i, c.signum()))
        })
    }
}

/// The box the planes leave visible; `None` without planes. Two planes on the same side keep
/// the tighter one.
pub fn clip_box(planes: &[ClipPlane]) -> Option<ClipBox> {
    if planes.is_empty() {
        return None;
    }
    let mut b = ClipBox {
        min: [f64::NEG_INFINITY; 3],
        max: [f64::INFINITY; 3],
        oblique: 0,
    };
    for p in planes {
        match p.axis() {
            Some((i, s)) if s < 0.0 => b.min[i] = b.min[i].max(p.at[i]),
            Some((i, _)) => b.max[i] = b.max[i].min(p.at[i]),
            None => b.oblique += 1,
        }
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_round_trips_through_planes_exactly() {
        let (lo, hi) = ([-1.25, 2.0, -0.5], [10.0, 7.125, 3.3]);
        let planes = ClipPlane::of_box(lo, hi);
        assert_eq!(planes.len(), 6);
        let b = clip_box(&planes).unwrap();
        assert_eq!((b.min, b.max, b.oblique), (lo, hi, 0));
    }

    #[test]
    fn directions_point_to_the_hidden_side() {
        let planes = ClipPlane::of_box([0.0; 3], [1.0; 3]);
        // the plane at x = 0 hides x < 0
        assert!(planes.iter().any(|p| p.at[0] == 0.0 && p.dir == [-1.0, 0.0, 0.0]));
        assert!(planes.iter().any(|p| p.at[2] == 1.0 && p.dir == [0.0, 0.0, 1.0]));
    }

    #[test]
    fn open_sides_oblique_planes_and_tighter_planes() {
        let mut planes = ClipPlane::of_box(
            [f64::NEG_INFINITY, 0.0, f64::NEG_INFINITY],
            [f64::INFINITY, f64::INFINITY, 4.0],
        );
        assert_eq!(planes.len(), 2);
        planes.push(ClipPlane { at: [0.0, 0.0, 3.0], dir: [0.0, 0.0, 2.0] });
        planes.push(ClipPlane { at: [1.0, 1.0, 1.0], dir: [1.0, 1.0, 0.0] });
        let b = clip_box(&planes).unwrap();
        assert_eq!(b.min[1], 0.0);
        assert_eq!(b.max[2], 3.0, "the tighter top plane wins");
        assert_eq!(b.min[0], f64::NEG_INFINITY);
        assert_eq!(b.oblique, 1);
        assert!(clip_box(&[]).is_none());
        assert!(!ClipPlane { at: [0.0; 3], dir: [0.0; 3] }.is_valid());
        assert!(!ClipPlane { at: [f64::NAN, 0.0, 0.0], dir: [1.0, 0.0, 0.0] }.is_valid());
    }
}
