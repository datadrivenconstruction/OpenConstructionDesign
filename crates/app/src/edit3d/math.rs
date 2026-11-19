//! 3D editing geometry without UI state: screen to world (`Cam`), ray vs plane/line/segment,
//! meters per pixel, rounding steps, segment vs screen rectangle.

use ddc_render::glam::{Mat4, Vec3};
use ddc_render::{OrbitCamera, Ray};
use egui::{Pos2, Rect};

/// Frame camera and view rectangle: screen to world conversion (scene, m; z from the active level).
#[derive(Clone, Copy, Debug)]
pub struct Cam {
    pub vp: Mat4,
    pub inv: Mat4,
    pub eye: Vec3,
    pub rect: Rect,
}

impl Cam {
    pub fn new(c: &OrbitCamera, rect: Rect) -> Cam {
        let aspect = rect.width() / rect.height().max(1.0);
        Cam::from_matrix(c.view_proj(aspect), c.eye(), rect)
    }

    pub fn from_matrix(vp: Mat4, eye: Vec3, rect: Rect) -> Cam {
        Cam {
            vp,
            inv: vp.inverse(),
            eye,
            rect,
        }
    }

    pub fn ndc(&self, p: Pos2) -> (f32, f32) {
        let r = self.rect;
        (
            (p.x - r.min.x) / r.width().max(1.0) * 2.0 - 1.0,
            1.0 - (p.y - r.min.y) / r.height().max(1.0) * 2.0,
        )
    }

    /// Ray through a screen point.
    pub fn ray(&self, p: Pos2) -> Ray {
        let (x, y) = self.ndc(p);
        // reversed depth: 1 is the near plane, 0 is infinity (never inverted)
        let p0 = self.inv.project_point3(Vec3::new(x, y, 1.0));
        let p1 = self.inv.project_point3(Vec3::new(x, y, 0.5));
        Ray {
            origin: p0,
            dir: (p1 - p0).normalize_or(Vec3::NEG_Z),
        }
    }

    /// Screen point; None if behind the camera or beyond the far plane.
    pub fn to_screen(&self, p: Vec3) -> Option<Pos2> {
        let n = self.vp.project_point3(p);
        let r = self.rect;
        (n.z > 0.0 && n.z < 1.0 && n.x.is_finite() && n.y.is_finite()).then(|| {
            Pos2::new(
                r.min.x + (n.x + 1.0) * 0.5 * r.width(),
                r.min.y + (1.0 - n.y) * 0.5 * r.height(),
            )
        })
    }

    /// Meters per screen pixel at point `p` (across the view ray).
    pub fn m_per_px(&self, p: Vec3) -> f32 {
        let view = (p - self.eye).normalize_or(Vec3::NEG_Z);
        let side = view.cross(Vec3::Z).normalize_or(Vec3::X);
        match (self.to_screen(p), self.to_screen(p + side)) {
            (Some(a), Some(b)) if (b - a).length() > 1e-6 => 1.0 / (b - a).length(),
            _ => 0.01,
        }
    }

    /// Screen distance from `pos` to a world point; infinity if not visible.
    pub fn px_dist(&self, p: Vec3, pos: Pos2) -> f32 {
        self.to_screen(p)
            .map_or(f32::INFINITY, |s| (s - pos).length())
    }
}

/// Ray-plane intersection (point `o`, normal `n`): (t ≥ 0, point).
pub fn ray_plane(r: &Ray, o: Vec3, n: Vec3) -> Option<(f32, Vec3)> {
    let den = r.dir.dot(n);
    if den.abs() < 1e-7 {
        return None;
    }
    let t = (o - r.origin).dot(n) / den;
    (t >= 0.0 && t.is_finite()).then(|| (t, r.at(t)))
}

/// Parameter `s` of the point on line `o + d·s` nearest to the ray; None if parallel.
pub fn ray_line(r: &Ray, o: Vec3, d: Vec3) -> Option<f32> {
    let w = r.origin - o;
    let (a, b, c) = (r.dir.dot(r.dir), r.dir.dot(d), d.dot(d));
    let (dd, e) = (r.dir.dot(w), d.dot(w));
    let den = a * c - b * b;
    if den.abs() < 1e-9 * (a * c).max(1e-12) {
        return None;
    }
    Some((a * e - b * dd) / den)
}

/// Point of segment a→b nearest to the ray.
pub fn ray_segment(r: &Ray, a: Vec3, b: Vec3) -> Vec3 {
    let d = b - a;
    match ray_line(r, a, d) {
        Some(s) => a + d * s.clamp(0.0, 1.0),
        None => a,
    }
}

/// Nearest points of two segments: (on the first, on the second).
pub fn seg_seg(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> (Vec3, Vec3) {
    let (u, v, w) = (a1 - a0, b1 - b0, a0 - b0);
    let (a, b, c, d, e) = (u.dot(u), u.dot(v), v.dot(v), u.dot(w), v.dot(w));
    let den = a * c - b * b;
    let mut s = if den > 1e-12 {
        ((b * e - c * d) / den).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut t = if c > 1e-12 { (b * s + e) / c } else { 0.0 };
    if t < 0.0 {
        t = 0.0;
        s = if a > 1e-12 {
            (-d / a).clamp(0.0, 1.0)
        } else {
            0.0
        };
    } else if t > 1.0 {
        t = 1.0;
        s = if a > 1e-12 {
            ((b - d) / a).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    (a0 + u * s, b0 + v * t)
}

/// Rotation angle from `from` to `to` about axis `axis` (vectors from the center), rad, counterclockwise.
pub fn angle_about(axis: Vec3, from: Vec3, to: Vec3) -> f32 {
    let n = axis.normalize_or(Vec3::Z);
    let f = from - n * from.dot(n);
    let t = to - n * to.dot(n);
    if f.length_squared() < 1e-12 || t.length_squared() < 1e-12 {
        return 0.0;
    }
    f.cross(t).dot(n).atan2(f.dot(t))
}

/// Rotate a point about an axis through `pivot` (f64, for writing to the model).
pub fn rotate3(p: [f64; 3], pivot: [f64; 3], axis: [f64; 3], angle: f64) -> [f64; 3] {
    let l = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if l < 1e-12 {
        return p;
    }
    let k = [axis[0] / l, axis[1] / l, axis[2] / l];
    let v = [p[0] - pivot[0], p[1] - pivot[1], p[2] - pivot[2]];
    let (s, c) = angle.sin_cos();
    let kv = [
        k[1] * v[2] - k[2] * v[1],
        k[2] * v[0] - k[0] * v[2],
        k[0] * v[1] - k[1] * v[0],
    ];
    let kd = k[0] * v[0] + k[1] * v[1] + k[2] * v[2];
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = pivot[i] + v[i] * c + kv[i] * s + k[i] * kd * (1.0 - c);
    }
    out
}

/// A "round" step 1·2·5·10ᵏ not smaller than `x`.
pub fn nice_step(x: f32) -> f32 {
    if !(x > 0.0) || !x.is_finite() {
        return 0.01;
    }
    let p = 10f32.powf(x.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if p * m >= x * 0.999 {
            return p * m;
        }
    }
    p * 10.0
}

pub fn round_to(v: f32, step: f32) -> f32 {
    if step > 0.0 {
        (v / step).round() * step
    } else {
        v
    }
}

/// A screen segment touches a rectangle (Liang–Barsky).
pub fn seg_hits_rect(a: Pos2, b: Pos2, r: Rect) -> bool {
    if r.contains(a) || r.contains(b) {
        return true;
    }
    let d = b - a;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-d.x, a.x - r.min.x),
        (d.x, r.max.x - a.x),
        (-d.y, a.y - r.min.y),
        (d.y, r.max.y - a.y),
    ] {
        if p.abs() < 1e-9 {
            if q < 0.0 {
                return false;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return false;
        }
    }
    true
}

pub fn f3(p: Vec3) -> [f64; 3] {
    [p.x as f64, p.y as f64, p.z as f64]
}

pub fn v3(p: [f64; 3]) -> Vec3 {
    Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn ray(o: Vec3, d: Vec3) -> Ray {
        Ray {
            origin: o,
            dir: d.normalize(),
        }
    }

    #[test]
    fn ray_plane_hits_and_misses() {
        let r = ray(Vec3::new(1.0, 2.0, 10.0), Vec3::NEG_Z);
        let (t, p) = ray_plane(&r, Vec3::new(0.0, 0.0, 3.0), Vec3::Z).unwrap();
        assert!((t - 7.0).abs() < 1e-5 && (p - Vec3::new(1.0, 2.0, 3.0)).length() < 1e-5);
        assert!(ray_plane(&r, Vec3::ZERO, Vec3::X).is_none(), "parallel");
        assert!(
            ray_plane(&r, Vec3::new(0.0, 0.0, 20.0), Vec3::Z).is_none(),
            "behind"
        );
        // an oblique ray into the vertical plane x = 5
        let r = ray(Vec3::ZERO, Vec3::new(1.0, 1.0, 0.5));
        let (_, p) = ray_plane(&r, Vec3::new(5.0, 0.0, 0.0), Vec3::X).unwrap();
        assert!((p - Vec3::new(5.0, 5.0, 2.5)).length() < 1e-4);
    }

    #[test]
    fn ray_to_axis_parameter() {
        // a ray from above onto the X axis, offset in z
        let r = ray(Vec3::new(3.0, 1.0, 10.0), Vec3::NEG_Z);
        let s = ray_line(&r, Vec3::new(0.0, 0.0, 1.0), Vec3::X).unwrap();
        assert!((s - 3.0).abs() < 1e-4);
        assert!(ray_line(&r, Vec3::ZERO, Vec3::Z).is_none(), "parallel to Z");
        let q = ray_segment(&r, Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0));
        assert!(
            (q - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4,
            "clamped to end"
        );
    }

    #[test]
    fn segment_pair_closest_and_intersection() {
        let (p, q) = seg_seg(
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        assert!(p.length() < 1e-6 && q.length() < 1e-6);
        let (p, q) = seg_seg(
            Vec3::ZERO,
            Vec3::X,
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(3.0, 1.0, 0.0),
        );
        assert!((p - Vec3::X).length() < 1e-6 && (q - Vec3::new(2.0, 1.0, 0.0)).length() < 1e-6);
    }

    #[test]
    fn angles_and_rotation() {
        let a = angle_about(Vec3::Z, Vec3::X, Vec3::Y);
        assert!((a - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        assert!(
            (angle_about(Vec3::Z, Vec3::Y, Vec3::X) + std::f32::consts::FRAC_PI_2).abs() < 1e-5
        );
        let p = rotate3(
            [2.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            std::f64::consts::FRAC_PI_2,
        );
        assert!((p[0] - 1.0).abs() < 1e-12 && (p[1] - 1.0).abs() < 1e-12);
        // rotation about Y: +X goes to −Z
        let p = rotate3(
            [1.0, 0.0, 0.0],
            [0.0; 3],
            [0.0, 1.0, 0.0],
            std::f64::consts::FRAC_PI_2,
        );
        assert!(p[0].abs() < 1e-12 && (p[2] + 1.0).abs() < 1e-12);
    }

    #[test]
    fn steps_and_rounding() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(near(nice_step(0.013), 0.02));
        assert!(near(nice_step(0.3), 0.5));
        assert!(near(nice_step(0.7), 1.0));
        assert!((round_to(1.234, 0.05) - 1.25).abs() < 1e-6);
        assert_eq!(round_to(1.234, 0.0), 1.234);
    }

    #[test]
    fn screen_round_trip() {
        let mut c = OrbitCamera::default();
        c.target = Vec3::new(1.0, 1.0, 0.0);
        let cam = Cam::new(
            &c,
            Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(800.0, 600.0)),
        );
        let p = Vec3::new(1.5, 0.5, 0.7);
        let s = cam.to_screen(p).unwrap();
        let r = cam.ray(s);
        let q = ray_segment(&r, p - Vec3::Z, p + Vec3::Z);
        assert!(
            (q - p).length() < 1e-3,
            "ray through projected point passes the point"
        );
        assert!(cam.m_per_px(p) > 0.0 && cam.m_per_px(p) < 1.0);
    }

    #[test]
    fn rect_crossing() {
        let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0));
        assert!(seg_hits_rect(pos2(-5.0, 5.0), pos2(15.0, 5.0), r));
        assert!(!seg_hits_rect(pos2(-5.0, -5.0), pos2(-1.0, 20.0), r));
        assert!(seg_hits_rect(pos2(5.0, 5.0), pos2(50.0, 50.0), r));
    }
}
