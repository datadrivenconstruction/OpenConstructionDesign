//! Move/rotate gizmo: X/Y/Z axis arrows, XY/XZ/YZ plane squares, rotation rings
//! about the axes. Constant size in pixels. Dragging yields an [`Xform`] with a step
//! (length is the grid step, angle 5°, Shift 15°); a snap point is projected onto the constraint;
//! a typed value sets the length/angle exactly.

use ddc_render::glam::Vec3;
use ddc_render::Ray;
use egui::Pos2;

use super::math::{angle_about, ray_line, ray_plane, round_to, Cam};
use super::xform::Xform;

pub const AXIS_PX: f32 = 90.0;
pub const PLANE_PX: (f32, f32) = (22.0, 40.0);
pub const RING_PX: f32 = 64.0;
const HIT_PX: f32 = 7.0;
const RING_SEG: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    /// Arrow of axis 0/1/2 (X/Y/Z).
    Axis(u8),
    /// Plane square with its normal along axis 0/1/2 (YZ/XZ/XY).
    Plane(u8),
    /// Rotation ring about axis 0/1/2.
    Ring(u8),
}

pub fn axis(i: u8) -> Vec3 {
    match i {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    }
}

/// The two axes of the plane with normal `i`.
pub fn plane_axes(i: u8) -> (Vec3, Vec3) {
    match i {
        0 => (Vec3::Y, Vec3::Z),
        1 => (Vec3::X, Vec3::Z),
        _ => (Vec3::X, Vec3::Y),
    }
}

/// Handle geometry in the scene for drawing and hit testing.
pub struct Shape {
    pub origin: Vec3,
    /// Meters per pixel at the gizmo origin.
    pub s: f32,
}

impl Shape {
    pub fn new(cam: &Cam, origin: Vec3) -> Shape {
        Shape {
            origin,
            s: cam.m_per_px(origin),
        }
    }

    pub fn axis_end(&self, i: u8) -> Vec3 {
        self.origin + axis(i) * AXIS_PX * self.s
    }

    pub fn plane_quad(&self, i: u8) -> [Vec3; 4] {
        let (u, v) = plane_axes(i);
        let (a, b) = (PLANE_PX.0 * self.s, PLANE_PX.1 * self.s);
        let o = self.origin;
        [
            o + u * a + v * a,
            o + u * b + v * a,
            o + u * b + v * b,
            o + u * a + v * b,
        ]
    }

    pub fn ring(&self, i: u8) -> Vec<Vec3> {
        let (u, v) = plane_axes(i);
        let r = RING_PX * self.s;
        (0..=RING_SEG)
            .map(|k| {
                let a = k as f32 / RING_SEG as f32 * std::f32::consts::TAU;
                self.origin + (u * a.cos() + v * a.sin()) * r
            })
            .collect()
    }

    /// Handle under the cursor: arrows first, then squares, then rings; `rings` says which rings
    /// are shown (bit `i` is the ring about axis `i`, see [`RINGS_ALL`]).
    pub fn hit(&self, cam: &Cam, pos: Pos2, rings: u8) -> Option<Handle> {
        let seg = |a: Vec3, b: Vec3| -> f32 {
            match (cam.to_screen(a), cam.to_screen(b)) {
                (Some(a), Some(b)) => seg_px(pos, a, b),
                _ => f32::INFINITY,
            }
        };
        let mut best: Option<(f32, Handle)> = None;
        for i in 0..3u8 {
            let d = seg(self.origin, self.axis_end(i));
            if d <= HIT_PX && best.is_none_or(|b| d < b.0) {
                best = Some((d, Handle::Axis(i)));
            }
        }
        if best.is_some() {
            return best.map(|b| b.1);
        }
        for i in 0..3u8 {
            let q = self.plane_quad(i).map(|p| cam.to_screen(p));
            if let [Some(a), Some(b), Some(c), Some(d)] = q {
                if in_quad(pos, [a, b, c, d]) {
                    return Some(Handle::Plane(i));
                }
            }
        }
        for i in (0..3u8).filter(|i| rings & (1 << i) != 0) {
            let r = self.ring(i);
            let d = r
                .windows(2)
                .map(|w| seg(w[0], w[1]))
                .fold(f32::INFINITY, f32::min);
            if d <= HIT_PX && best.is_none_or(|b| d < b.0) {
                best = Some((d, Handle::Ring(i)));
            }
        }
        best.map(|b| b.1)
    }
}

/// All rings; Z only means rotation in plan (tilt exists only for beams and columns).
pub const RINGS_ALL: u8 = 0b111;
pub const RINGS_Z: u8 = 0b100;

/// Distance from a point to a segment on screen.
pub fn seg_px(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 < 1e-9 {
        0.0
    } else {
        ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
    };
    (a + ab * t - p).length()
}

fn in_quad(p: Pos2, q: [Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for k in 0..4 {
        let (a, b) = (q[k], q[(k + 1) % 4]);
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    sign != 0.0
}

/// An ongoing gizmo handle drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub handle: Handle,
    pub origin: Vec3,
    /// Constraint point under the cursor at press time.
    pub start: Vec3,
}

/// Handle constraint point under the ray.
pub fn on_constraint(h: Handle, origin: Vec3, r: &Ray) -> Option<Vec3> {
    match h {
        Handle::Axis(i) => ray_line(r, origin, axis(i)).map(|s| origin + axis(i) * s),
        Handle::Plane(i) | Handle::Ring(i) => ray_plane(r, origin, axis(i)).map(|h| h.1),
    }
}

/// Projection of a snap point onto the handle constraint.
pub fn project(h: Handle, origin: Vec3, p: Vec3) -> Vec3 {
    match h {
        Handle::Axis(i) => origin + axis(i) * (p - origin).dot(axis(i)),
        Handle::Plane(i) | Handle::Ring(i) => p - axis(i) * (p - origin).dot(axis(i)),
    }
}

impl Drag {
    pub fn begin(h: Handle, origin: Vec3, r: &Ray) -> Option<Drag> {
        on_constraint(h, origin, r).map(|start| Drag {
            handle: h,
            origin,
            start,
        })
    }

    pub fn is_rotate(&self) -> bool {
        matches!(self.handle, Handle::Ring(_))
    }

    /// Transform with the cursor on ray `r` (or at snap point `snap`).
    /// `step` is the length step (0: none), `ang` the angle step in degrees (0: none).
    pub fn xform(&self, r: &Ray, snap: Option<Vec3>, step: f32, ang: f32) -> Option<Xform> {
        let p = match snap {
            Some(s) => project(self.handle, self.origin, s),
            None => on_constraint(self.handle, self.origin, r)?,
        };
        let quant = snap.is_none();
        // with a snap the shift is measured from the element point (gizmo origin), not from the press point
        let from = if snap.is_some() {
            self.origin
        } else {
            self.start
        };
        Some(match self.handle {
            Handle::Axis(i) => {
                let mut d = (p - from).dot(axis(i));
                if quant && step > 0.0 {
                    d = round_to(d, step);
                }
                Xform::Move(axis(i) * d)
            }
            Handle::Plane(i) => {
                let (u, v) = plane_axes(i);
                let d = p - from;
                let (mut du, mut dv) = (d.dot(u), d.dot(v));
                if quant && step > 0.0 {
                    du = round_to(du, step);
                    dv = round_to(dv, step);
                }
                Xform::Move(u * du + v * dv)
            }
            Handle::Ring(i) => {
                let mut a =
                    angle_about(axis(i), self.start - self.origin, p - self.origin).to_degrees();
                if ang > 0.0 && quant {
                    a = round_to(a, ang);
                }
                Xform::Rotate {
                    pivot: self.origin,
                    axis: axis(i),
                    angle: a.to_radians(),
                }
            }
        })
    }

    /// Exact value: length (m) along the current move direction, or an angle (degrees).
    pub fn typed(&self, cur: &Xform, v: f32) -> Xform {
        match (self.handle, cur) {
            (Handle::Ring(i), _) => Xform::Rotate {
                pivot: self.origin,
                axis: axis(i),
                angle: v.to_radians(),
            },
            (Handle::Axis(i), Xform::Move(d)) => {
                let s = if d.dot(axis(i)) < 0.0 { -1.0 } else { 1.0 };
                Xform::Move(axis(i) * v * s)
            }
            (Handle::Plane(i), Xform::Move(d)) => {
                let dir = d.normalize_or(plane_axes(i).0);
                Xform::Move(dir * v)
            }
            (Handle::Axis(i) | Handle::Plane(i), _) => Xform::Move(axis(i) * v),
        }
    }

    /// Current value for the temporary dimension: length, or angle in degrees.
    pub fn value(cur: &Xform) -> f32 {
        match *cur {
            Xform::Move(d) => d.length(),
            Xform::Rotate { angle, .. } => angle.to_degrees(),
            Xform::Mirror { .. } | Xform::Array { .. } => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Rect};

    fn cam() -> Cam {
        let mut c = ddc_render::OrbitCamera::default();
        c.target = Vec3::ZERO;
        Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)))
    }

    #[test]
    fn handles_are_hit_where_drawn() {
        // in the default view the XY square's diagonal lies under the Z arrow, and arrows take priority
        let mut c = ddc_render::OrbitCamera::default();
        c.yaw = -1.7;
        let cam = Cam::new(&c, Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0)));
        let g = Shape::new(&cam, Vec3::ZERO);
        assert!(g.s > 0.0);
        for i in 0..3u8 {
            let tip = (g.origin + axis(i) * AXIS_PX * g.s * 0.8, Handle::Axis(i));
            let at = cam.to_screen(tip.0).unwrap();
            assert_eq!(g.hit(&cam, at, RINGS_ALL), Some(tip.1), "axis {i}");
        }
        let q = g.plane_quad(2);
        let c = cam.to_screen((q[0] + q[2]) * 0.5).unwrap();
        assert_eq!(g.hit(&cam, c, RINGS_ALL), Some(Handle::Plane(2)));
        // Z ring: a point on the ring far from the arrows
        let r = g.ring(2);
        let p = cam.to_screen(r[RING_SEG * 5 / 8]).unwrap();
        assert_eq!(g.hit(&cam, p, RINGS_ALL), Some(Handle::Ring(2)));
        assert_eq!(g.hit(&cam, p, RINGS_Z), Some(Handle::Ring(2)));
        assert_eq!(g.hit(&cam, p, 0), None);
        assert_eq!(g.hit(&cam, pos2(5.0, 5.0), RINGS_ALL), None);
        // the X ring is hidden by the mask: nothing can be picked on it
        let rx = g.ring(0);
        let px = cam.to_screen(rx[RING_SEG * 5 / 8]).unwrap();
        assert_ne!(g.hit(&cam, px, RINGS_Z), Some(Handle::Ring(0)));
    }

    #[test]
    fn axis_drag_steps_and_typed_value() {
        let cam = cam();
        let o = Vec3::ZERO;
        let at = |p: Vec3| cam.ray(cam.to_screen(p).unwrap());
        let d = Drag::begin(Handle::Axis(0), o, &at(Vec3::new(0.5, 0.0, 0.0))).unwrap();
        let x = d
            .xform(&at(Vec3::new(1.73, 0.0, 0.0)), None, 0.1, 5.0)
            .unwrap();
        let Xform::Move(v) = x else { panic!() };
        assert!(
            (v.x - 1.2).abs() < 1e-3 && v.y == 0.0 && v.z == 0.0,
            "{v:?}"
        );
        // a snap sets the point without rounding, projected onto the axis
        let x = d
            .xform(&at(Vec3::ZERO), Some(Vec3::new(2.345, 7.0, 1.0)), 0.1, 5.0)
            .unwrap();
        // measured from the gizmo origin: the element point lands on the snap
        assert_eq!(x, Xform::Move(Vec3::new(2.345, 0.0, 0.0)));
        let d2 = Drag::begin(
            Handle::Plane(2),
            Vec3::new(1.0, 1.0, 0.0),
            &at(Vec3::new(1.3, 1.2, 0.0)),
        )
        .unwrap();
        let x = d2
            .xform(&at(Vec3::ZERO), Some(Vec3::new(4.0, 5.0, 9.0)), 0.1, 5.0)
            .unwrap();
        assert_eq!(
            x,
            Xform::Move(Vec3::new(3.0, 4.0, 0.0)),
            "snap minus the base point"
        );
        // typed value: the direction stays the current one
        let t = d.typed(&Xform::Move(Vec3::new(-0.3, 0.0, 0.0)), 2.0);
        assert_eq!(t, Xform::Move(Vec3::new(-2.0, 0.0, 0.0)));
        assert!((Drag::value(&t) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn ring_drag_quantizes_angle() {
        let cam = cam();
        let o = Vec3::ZERO;
        let at = |p: Vec3| cam.ray(cam.to_screen(p).unwrap());
        let d = Drag::begin(Handle::Ring(2), o, &at(Vec3::new(1.0, 0.0, 0.0))).unwrap();
        assert!(d.is_rotate());
        let a = 32f32.to_radians();
        let x = d
            .xform(&at(Vec3::new(a.cos(), a.sin(), 0.0)), None, 0.1, 5.0)
            .unwrap();
        let Xform::Rotate { angle, axis, .. } = x else {
            panic!()
        };
        assert_eq!(axis, Vec3::Z);
        assert!(
            (angle.to_degrees() - 30.0).abs() < 1e-3,
            "{}",
            angle.to_degrees()
        );
        let t = d.typed(&x, 90.0);
        assert!((Drag::value(&t) - 90.0).abs() < 1e-4);
    }

    #[test]
    fn plane_drag_and_screen_helpers() {
        let cam = cam();
        let at = |p: Vec3| cam.ray(cam.to_screen(p).unwrap());
        let d = Drag::begin(Handle::Plane(2), Vec3::ZERO, &at(Vec3::new(0.2, 0.2, 0.0))).unwrap();
        let x = d
            .xform(&at(Vec3::new(1.21, -0.79, 0.0)), None, 0.5, 0.0)
            .unwrap();
        let Xform::Move(v) = x else { panic!() };
        assert!((v - Vec3::new(1.0, -1.0, 0.0)).length() < 1e-3, "{v:?}");
        assert!((seg_px(pos2(5.0, 5.0), pos2(0.0, 0.0), pos2(10.0, 0.0)) - 5.0).abs() < 1e-6);
        let sq = [
            pos2(0.0, 0.0),
            pos2(4.0, 0.0),
            pos2(4.0, 4.0),
            pos2(0.0, 4.0),
        ];
        assert!(in_quad(pos2(2.0, 2.0), sq) && !in_quad(pos2(5.0, 2.0), sq));
    }
}
