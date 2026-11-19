//! Sloped slab top: the plane `z = z(p0) + gx·(x − x0) + gy·(y − y0)`.
//! Thickness is measured vertically, so volume = plan area × thickness (a shear preserves volume).

use crate::math::{v2, Tol, V2};
use crate::mesh::{Mesh, MeshError};
use crate::{Profile, Solid};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub p0: V2,
    pub gx: f64,
    pub gy: f64,
}

impl Plane {
    /// Rise relative to `p0`.
    pub fn rise(&self, p: V2) -> f64 {
        self.gx * (p.x - self.p0.x) + self.gy * (p.y - self.p0.y)
    }
    /// Plane with the given gradient anchored at the lowest outline vertex: the rise is
    /// non-negative everywhere, and the result does not depend on which vertex starts the outline.
    pub fn lowest_on(ring: &[V2], gx: f64, gy: f64) -> Option<Plane> {
        let h = |p: &V2| gx * p.x + gy * p.y;
        let p0 = ring.iter().min_by(|a, b| h(a).total_cmp(&h(b)))?;
        Some(Plane { p0: *p0, gx, gy })
    }
    /// Slope arrow: the top rises by `rise` from `tail` to `head`.
    pub fn arrow(tail: V2, head: V2, rise: f64) -> Option<Plane> {
        let d = head - tail;
        let l2 = d.dot(d);
        (l2 > 1e-12 && rise.is_finite()).then(|| Plane {
            p0: tail,
            gx: rise * d.x / l2,
            gy: rise * d.y / l2,
        })
    }
    /// Plane through vertex elevations (least squares); `None` if there are fewer than three
    /// points, they are collinear, or the deviation exceeds `tol` (surface not planar). Elevations are relative to the first point.
    pub fn through(pts: &[(V2, f64)], tol: f64) -> Option<(Plane, f64)> {
        let (p0, z0) = *pts.first()?;
        let (mut sxx, mut sxy, mut syy, mut sxz, mut syz) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(p, z) in pts {
            let (x, y, z) = (p.x - p0.x, p.y - p0.y, z - z0);
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
            sxz += x * z;
            syz += y * z;
        }
        let det = sxx * syy - sxy * sxy;
        if det.abs() <= 1e-12 * (sxx * syy).max(1e-12) {
            return None;
        }
        let gx = (sxz * syy - syz * sxy) / det;
        let gy = (syz * sxx - sxz * sxy) / det;
        let pl = Plane { p0, gx, gy };
        let ok = pts.iter().all(|&(p, z)| (z0 + pl.rise(p) - z).abs() <= tol);
        ok.then_some((pl, z0))
    }
    pub fn is_flat(&self) -> bool {
        self.gx == 0.0 && self.gy == 0.0
    }
    /// Sloped face area / plan area.
    pub fn area_factor(&self) -> f64 {
        (1.0 + self.gx * self.gx + self.gy * self.gy).sqrt()
    }
}

/// Slab solid: top at `p0` at elevation `top`, slope `plane`, vertical thickness `t`.
pub fn slab_mesh(
    p: &Profile,
    top: f64,
    plane: &Plane,
    t: f64,
    tol: Tol,
) -> Result<Mesh, MeshError> {
    let mut m = Solid::extrude(p, t, tol).triangulate()?;
    for q in &mut m.positions {
        q.z += top - t + plane.rise(v2(q.x, q.y));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sloped_slab_volume_is_plan_area_times_thickness() {
        let p = Profile::new(
            vec![v2(0., 0.), v2(6., 0.), v2(6., 4.), v2(0., 4.)],
            vec![],
            Tol::default(),
        )
        .unwrap();
        // 2 % along x: 0.12 m rise over 6 m
        let pl = Plane::arrow(v2(0., 0.), v2(6., 0.), 0.12).unwrap();
        assert!((pl.gx - 0.02).abs() < 1e-15 && pl.gy == 0.0);
        let m = slab_mesh(&p, 3.0, &pl, 0.2, Tol::default()).unwrap();
        assert!(m.is_watertight());
        assert!((m.volume() - 4.8).abs() < 1e-12, "{}", m.volume());
        let zmax = m.positions.iter().map(|q| q.z).fold(f64::MIN, f64::max);
        assert!((zmax - 3.12).abs() < 1e-12);
        let pts = [
            (v2(0., 0.), 3.0),
            (v2(6., 0.), 3.12),
            (v2(6., 4.), 3.12),
            (v2(0., 4.), 3.0),
        ];
        let (q, z0) = Plane::through(&pts, 1e-9).unwrap();
        assert!((q.gx - 0.02).abs() < 1e-12 && q.gy.abs() < 1e-12 && z0 == 3.0);
        let warped = [
            (v2(0., 0.), 3.0),
            (v2(6., 0.), 3.12),
            (v2(6., 4.), 3.0),
            (v2(0., 4.), 3.0),
        ];
        assert!(Plane::through(&warped, 1e-3).is_none());
    }
}
