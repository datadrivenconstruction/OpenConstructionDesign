//! 3D work plane: level (a horizontal at an elevation), element face, reference plane
//! (a vertical through a line), picked face. Basis (u, v, n): u, v lie in the plane, n is the normal.
//! The plane grid and model axis lines on it serve drawing and snaps.

use ddc_geom::{v2, V2};
use ddc_render::glam::{DVec3, Vec3};
use ddc_render::Ray;

use super::math::ray_plane;

/// Where the plane came from (for the label and recomputation after edits).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// A level (id) with an offset above it, m.
    Level(i64, f32),
    /// The element face under the cursor.
    Face,
    /// A model reference plane (id).
    Ref(i64),
    /// Named work plane of the project (`NamedPlane::id`).
    Named(i64),
    /// Vertical plane through a straight grid axis (`Grid::id`); follows the axis.
    Grid(i64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkPlane {
    pub source: Source,
    pub origin: Vec3,
    pub u: Vec3,
    pub v: Vec3,
    pub n: Vec3,
}

impl Default for WorkPlane {
    fn default() -> Self {
        WorkPlane::level(0, 0.0)
    }
}

impl WorkPlane {
    /// A horizontal plane at scene height `z` (scene z is measured from the active level's elevation).
    pub fn level(id: i64, z: f32) -> WorkPlane {
        WorkPlane {
            source: Source::Level(id, z),
            origin: Vec3::new(0.0, 0.0, z),
            u: Vec3::X,
            v: Vec3::Y,
            n: Vec3::Z,
        }
    }

    /// A face plane: point and normal; u is the face's horizontal (for vertical faces, along the wall).
    pub fn face(point: Vec3, normal: Vec3) -> WorkPlane {
        let n = normal.normalize_or(Vec3::Z);
        let u = if n.z.abs() > 0.999 {
            Vec3::X
        } else {
            Vec3::Z.cross(n).normalize_or(Vec3::X)
        };
        let v = n.cross(u).normalize_or(Vec3::Y);
        WorkPlane {
            source: Source::Face,
            origin: point,
            u,
            v,
            n,
        }
    }

    /// A vertical plane through plan line a→b (a reference plane).
    pub fn vertical(id: i64, a: (f64, f64), b: (f64, f64)) -> Option<WorkPlane> {
        let a3 = Vec3::new(a.0 as f32, a.1 as f32, 0.0);
        let u = (Vec3::new(b.0 as f32, b.1 as f32, 0.0) - a3).normalize_or_zero();
        if u == Vec3::ZERO {
            return None;
        }
        Some(WorkPlane {
            source: Source::Ref(id),
            origin: a3,
            u,
            v: Vec3::Z,
            n: u.cross(Vec3::Z),
        })
    }

    pub fn is_horizontal(&self) -> bool {
        self.n.z.abs() > 0.999
    }

    /// The plane in model coordinates (f64) for `interact::Pointer`; scene z is measured from
    /// `z0`, the elevation of the active level.
    pub fn to_model(&self, z0: f64) -> crate::interact::WorkPlane {
        use crate::interact::pointer::Source as M;
        let f = |v: Vec3| ddc_geom::v3(v.x as f64, v.y as f64, v.z as f64);
        let source = match self.source {
            Source::Level(id, _) => M::Level(id),
            Source::Face => M::Face,
            Source::Ref(id) => M::Ref(id),
            Source::Named(id) => M::Named(id),
            Source::Grid(id) => M::Grid(id),
        };
        let o = f(self.origin);
        crate::interact::WorkPlane {
            origin: ddc_geom::v3(o.x, o.y, o.z + z0),
            u: f(self.u),
            v: f(self.v),
            n: f(self.n),
            source,
        }
    }

    /// Height of a horizontal plane (for creating walls, slabs and columns on it).
    pub fn z(&self) -> f32 {
        self.origin.z
    }

    pub fn hit(&self, r: &Ray) -> Option<Vec3> {
        ray_plane(r, self.origin, self.n).map(|h| h.1)
    }

    pub fn to_uv(&self, p: Vec3) -> (f32, f32) {
        let d = p - self.origin;
        (d.dot(self.u), d.dot(self.v))
    }

    pub fn from_uv(&self, u: f32, v: f32) -> Vec3 {
        self.origin + self.u * u + self.v * v
    }

    /// Projection of a point onto the plane.
    pub fn project(&self, p: Vec3) -> Vec3 {
        p - self.n * (p - self.origin).dot(self.n)
    }

    /// Nearest grid node with pitch `step` in plane coordinates.
    pub fn grid_node(&self, p: Vec3, step: f32) -> Vec3 {
        let (u, v) = self.to_uv(p);
        self.from_uv(
            super::math::round_to(u, step),
            super::math::round_to(v, step),
        )
    }

    /// Grid lines around `c`: n lines on each side with pitch `step`.
    pub fn grid_lines(&self, c: Vec3, step: f32, n: i32) -> Vec<[Vec3; 2]> {
        let g = self.grid_node(c, step);
        let (gu, gv) = self.to_uv(g);
        let half = step * n as f32;
        let mut out = vec![];
        for k in -n..=n {
            let d = k as f32 * step;
            out.push([
                self.from_uv(gu + d, gv - half),
                self.from_uv(gu + d, gv + half),
            ]);
            out.push([
                self.from_uv(gu - half, gv + d),
                self.from_uv(gu + half, gv + d),
            ]);
        }
        out
    }

    /// Trace of the vertical plane of plan axis a→b on this plane (a segment within `ext`).
    pub fn trace(&self, a: (f64, f64), b: (f64, f64), ext: f32) -> Option<[Vec3; 2]> {
        let a3 = Vec3::new(a.0 as f32, a.1 as f32, 0.0);
        let b3 = Vec3::new(b.0 as f32, b.1 as f32, 0.0);
        if self.is_horizontal() {
            let z = self.origin.z;
            return Some([a3.with_z(z), b3.with_z(z)]);
        }
        // vertical plane: the intersection of two verticals is a vertical line
        let d = b3 - a3;
        let den = self.n.x * d.x + self.n.y * d.y;
        if den.abs() < 1e-6 {
            return None;
        }
        let t = (self.origin - a3).dot(self.n) / den;
        let p = a3 + d * t;
        Some([p.with_z(-ext), p.with_z(ext)])
    }

    /// The plane in model coordinates: scene z counts from elevation `elev`.
    pub fn to_world(&self, elev: f64) -> World {
        let o = self.origin.as_dvec3();
        World {
            origin: DVec3::new(o.x, o.y, o.z + elev),
            u: self.u.as_dvec3(),
            n: self.n.as_dvec3(),
        }
    }
}

/// A work plane in model coordinates (z is the absolute elevation), for the views that are not
/// the 3D view: its trace on the plan level and on a section, and the plan snaps along it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct World {
    pub origin: DVec3,
    pub u: DVec3,
    pub n: DVec3,
}

/// Directions closer than this to parallel have no trace, rad (about 0.06 degrees).
const PARALLEL: f64 = 1e-3;

impl World {
    /// The plane in the 3D view's frame, where z counts from elevation `elev`.
    pub fn to_scene(&self, elev: f64, source: Source) -> WorkPlane {
        let n = self.n.as_vec3().normalize_or(Vec3::Z);
        let u = self.u.as_vec3().normalize_or(Vec3::X);
        WorkPlane {
            source,
            origin: DVec3::new(self.origin.x, self.origin.y, self.origin.z - elev).as_vec3(),
            u,
            v: n.cross(u).normalize_or(Vec3::Y),
            n,
        }
    }

    /// Line where this plane meets the plane through `q` with normal `m`: a point on it and
    /// its unit direction. `None` when the two are parallel.
    pub fn cut_line(&self, q: DVec3, m: DVec3) -> Option<(DVec3, DVec3)> {
        let n = self.n.normalize_or_zero();
        let m = m.normalize_or_zero();
        let d = n.cross(m);
        let s = d.length();
        if s < PARALLEL {
            return None;
        }
        // the point of both planes nearest the origin of coordinates
        let (h1, h2) = (n.dot(self.origin), m.dot(q));
        let p = (m.cross(d) * h1 + d.cross(n) * h2) / (s * s);
        Some((p, d / s))
    }

    /// Trace on the horizontal plane at elevation `z`, as a plan segment clipped to the box
    /// `lo`..`hi`. A horizontal work plane has none.
    pub fn plan_trace(&self, z: f64, lo: V2, hi: V2) -> Option<(V2, V2)> {
        let (p, d) = self.cut_line(DVec3::new(0.0, 0.0, z), DVec3::Z)?;
        clip_line(v2(p.x, p.y), v2(d.x, d.y), lo, hi)
    }

    /// Trace on the vertical plane of a section line `a`→`b`, in section coordinates
    /// (s along a→b from `a`, absolute z), clipped to the box `lo`..`hi`. A plane parallel to
    /// the section has none.
    pub fn section_trace(&self, a: V2, b: V2, lo: V2, hi: V2) -> Option<(V2, V2)> {
        let t = (b - a) * (1.0 / (b - a).len().max(1e-12));
        let q = DVec3::new(a.x, a.y, 0.0);
        let (p, d) = self.cut_line(q, DVec3::new(t.y, -t.x, 0.0))?;
        let along = |w: DVec3| w.x * t.x + w.y * t.y;
        let s0 = along(p - q);
        clip_line(v2(s0, p.z), v2(along(d), d.z), lo, hi)
    }

    /// Point of plan point `p` on this plane straight above or below it: the plane's height
    /// there. `None` for a vertical plane.
    pub fn z_at(&self, p: V2) -> Option<f64> {
        let n = self.n;
        if n.z.abs() < PARALLEL {
            return None;
        }
        Some(self.origin.z - (n.x * (p.x - self.origin.x) + n.y * (p.y - self.origin.y)) / n.z)
    }
}

/// Part of the line through `p` along `d` inside the box `lo`..`hi` (Liang-Barsky).
pub fn clip_line(p: V2, d: V2, lo: V2, hi: V2) -> Option<(V2, V2)> {
    let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
    for (pk, dk, l, h) in [(p.x, d.x, lo.x, hi.x), (p.y, d.y, lo.y, hi.y)] {
        if dk.abs() < 1e-12 {
            if pk < l || pk > h {
                return None;
            }
            continue;
        }
        let (a, b) = ((l - pk) / dk, (h - pk) / dk);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    (t0.is_finite() && t1.is_finite() && t1 > t0).then(|| (p + d * t0, p + d * t1))
}

/// The plane other views show and snap to: the user's plane in model coordinates and its name
/// for the label.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub plane: World,
    pub name: String,
    /// Where the plane came from and the model revision the name was read at.
    pub source: Source,
    pub rev: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_plane_hit_and_uv() {
        let wp = WorkPlane::level(1, 3.0);
        let r = Ray {
            origin: Vec3::new(2.0, 1.0, 10.0),
            dir: Vec3::new(0.0, 0.0, -1.0),
        };
        let p = wp.hit(&r).unwrap();
        assert!((p - Vec3::new(2.0, 1.0, 3.0)).length() < 1e-5);
        assert_eq!(wp.to_uv(p), (2.0, 1.0));
        assert!(wp.is_horizontal() && wp.z() == 3.0);
        let g = wp.grid_node(Vec3::new(2.04, 0.96, 3.0), 0.1);
        assert!((g - Vec3::new(2.0, 1.0, 3.0)).length() < 1e-5);
    }

    #[test]
    fn face_plane_basis_is_orthonormal() {
        let wp = WorkPlane::face(Vec3::new(0.0, 5.0, 1.0), Vec3::new(0.0, -1.0, 0.0));
        assert!((wp.u.dot(wp.v)).abs() < 1e-6 && (wp.u.dot(wp.n)).abs() < 1e-6);
        assert!(wp.v.z.abs() > 0.99, "vertical face: v goes up");
        assert!(!wp.is_horizontal());
        let q = wp.project(Vec3::new(1.0, 7.0, 2.0));
        assert!((q.y - 5.0).abs() < 1e-5);
    }

    #[test]
    fn ref_plane_vertical_and_grid_trace() {
        let wp = WorkPlane::vertical(7, (0.0, 0.0), (4.0, 0.0)).unwrap();
        assert_eq!(wp.source, Source::Ref(7));
        assert!(wp.n.y.abs() > 0.99 && wp.v == Vec3::Z);
        let r = Ray {
            origin: Vec3::new(1.0, -10.0, 1.5),
            dir: Vec3::Y,
        };
        assert!((wp.hit(&r).unwrap() - Vec3::new(1.0, 0.0, 1.5)).length() < 1e-5);
        // axis x = 2 meets plane y = 0 along the vertical x = 2
        let tr = wp.trace((2.0, -5.0), (2.0, 5.0), 10.0).unwrap();
        assert!((tr[0].x - 2.0).abs() < 1e-5 && tr[0].y.abs() < 1e-5 && tr[1].z == 10.0);
        assert!(WorkPlane::vertical(1, (1.0, 1.0), (1.0, 1.0)).is_none());
        let lv = WorkPlane::level(1, 2.0);
        let tr = lv.trace((0.0, 0.0), (0.0, 3.0), 1.0).unwrap();
        assert_eq!(tr[1], Vec3::new(0.0, 3.0, 2.0));
    }
}
