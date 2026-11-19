//! Orbit camera (Z up), rays from the screen, zoom to cursor, "show all".

use crate::scene::Aabb;
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3,
}

impl Ray {
    pub fn at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }
}

#[derive(Clone, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    /// Scene radius — for the far clip plane.
    pub scene_radius: f32,
    /// Orthographic projection: half frame height = `distance · tan(fov_y / 2)`, same as
    /// the perspective at the target depth — switching does not change the scale at the target.
    pub ortho: bool,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: -2.2,
            pitch: 0.6,
            distance: 10.0,
            fov_y: 45f32.to_radians(),
            scene_radius: 10.0,
            ortho: false,
        }
    }
}

impl OrbitCamera {
    /// Direction from the target to the eye.
    pub fn back(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(cp * cy, cp * sy, sp)
    }
    pub fn eye(&self) -> Vec3 {
        self.target + self.back() * self.distance
    }
    /// "Right" axis (horizontal, from the azimuth: also valid when looking straight down).
    pub fn right_axis(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(-sy, cy, 0.0)
    }
    /// Frame "up" axis: non-degenerate at pitch ±90°.
    pub fn up(&self) -> Vec3 {
        self.right_axis().cross(-self.back())
    }
    /// Ortho depth: from the virtual eye to the far plane — `2·ortho_depth()`.
    fn ortho_depth(&self) -> f32 {
        self.distance + 4.0 * self.scene_radius + 10.0
    }
    /// The point the frame is built from: in perspective — the eye, in ortho — a virtual eye
    /// behind the near plane (everything visible is in front of it; shaders compute from it).
    pub fn render_eye(&self) -> Vec3 {
        if self.ortho {
            self.target + self.back() * self.ortho_depth()
        } else {
            self.eye()
        }
    }
    /// Offset of the ortho virtual eye back from the orbit eye (0 in perspective).
    pub fn eye_offset(&self) -> f32 {
        if self.ortho {
            self.ortho_depth() - self.distance
        } else {
            0.0
        }
    }
    pub fn view(&self) -> Mat4 {
        let e = self.render_eye();
        Mat4::look_at_rh(e, e - self.back(), self.up())
    }
    /// Reversed depth (near → 1, far → 0): float precision where it is needed;
    /// perspective without a far plane.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        if self.ortho {
            let h = self.distance * (self.fov_y * 0.5).tan();
            let w = h * aspect.max(1e-3);
            let depth = self.ortho_depth();
            return Mat4::orthographic_rh(-w, w, -h, h, 2.0 * depth, 0.0);
        }
        let near = (self.distance * 0.002).max(0.005);
        Mat4::perspective_infinite_reverse_rh(self.fov_y, aspect.max(1e-3), near)
    }
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }
    /// Ray through a point in NDC (x, y ∈ [-1, 1], y up). Computed from the camera axes rather than
    /// the inverse matrix: with reversed depth and a distant eye the latter loses precision.
    pub fn ray(&self, ndc_x: f32, ndc_y: f32, aspect: f32) -> Ray {
        let (fwd, right, up) = (-self.back(), self.right_axis(), self.up());
        let t = (self.fov_y * 0.5).tan();
        let a = aspect.max(1e-3);
        if self.ortho {
            let h = self.distance * t;
            return Ray {
                origin: self.render_eye() + right * (ndc_x * h * a) + up * (ndc_y * h),
                dir: fwd,
            };
        }
        Ray {
            origin: self.eye(),
            dir: (fwd + right * (ndc_x * t * a) + up * (ndc_y * t)).normalize(),
        }
    }
    pub fn project(&self, p: Vec3, aspect: f32) -> Vec3 {
        self.view_proj(aspect).project_point3(p)
    }
    pub fn orbit(&mut self, dx_px: f32, dy_px: f32) {
        self.yaw -= dx_px * 0.008;
        // a view straight down (pitch 90°) is not reset to the rotation limit
        let lim = crate::nav::PITCH_MAX.max(self.pitch.abs().min(std::f32::consts::FRAC_PI_2));
        self.pitch = (self.pitch + dy_px * 0.008).clamp(-lim, lim);
    }
    /// Pan: the point at the target depth follows the cursor.
    pub fn pan(&mut self, dx_px: f32, dy_px: f32, viewport_h_px: f32) {
        let scale = 2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_h_px.max(1.0);
        self.target += (-self.right_axis() * dx_px + self.up() * dy_px) * scale;
    }
    /// Camera homothety centred at `p`: the point `p` stays under the cursor.
    pub fn zoom_about(&mut self, p: Vec3, factor: f32) {
        let nd = (self.distance * factor.clamp(0.05, 20.0)).clamp(0.05, 1.0e6);
        let f = nd / self.distance;
        self.target = p + (self.target - p) * f;
        self.distance = nd;
    }
    /// Ray point at the target depth — the zoom anchor when nothing is under the cursor.
    pub fn focus_point(&self, ray: &Ray) -> Vec3 {
        let fwd = -self.back();
        let c = ray.dir.dot(fwd).max(1e-3);
        ray.at((self.target - ray.origin).dot(fwd) / c)
    }
    pub fn fit(&mut self, b: &Aabb, aspect: f32) {
        let r = ((b.max - b.min).length() * 0.5).max(0.1);
        let half_v = self.fov_y * 0.5;
        let half_h = (half_v.tan() * aspect.max(1e-3)).atan();
        self.target = (b.min + b.max) * 0.5;
        self.distance = if self.ortho {
            r / (half_v.tan() * aspect.clamp(1e-3, 1.0)) * 1.05
        } else {
            r / half_v.min(half_h).sin() * 1.05
        };
        self.scene_radius = r;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn center_ray_points_at_target() {
        let c = OrbitCamera::default();
        let r = c.ray(0.0, 0.0, 1.5);
        assert!(close(r.dir, -c.back(), 1e-4));
        assert!(close(c.focus_point(&r), c.target, 1e-3));
    }

    #[test]
    fn project_unproject_roundtrip() {
        let c = OrbitCamera::default();
        let p = Vec3::new(1.0, -2.0, 0.5);
        let ndc = c.project(p, 1.3);
        let r = c.ray(ndc.x, ndc.y, 1.3);
        let t = (p - r.origin).length();
        assert!(close(r.at(t), p, 1e-3));
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut c = OrbitCamera::default();
        let p = Vec3::new(2.0, 1.0, 0.0);
        let before = c.project(p, 1.6);
        c.zoom_about(p, 0.5);
        let after = c.project(p, 1.6);
        assert!((before.x - after.x).abs() < 1e-4 && (before.y - after.y).abs() < 1e-4);
        assert!((c.distance - 5.0).abs() < 1e-4);
    }

    #[test]
    fn pan_moves_old_target_with_cursor() {
        let mut c = OrbitCamera::default();
        let t0 = c.target;
        c.pan(100.0, 0.0, 800.0);
        let ndc = c.project(t0, 1.0);
        assert!((ndc.x - 0.25).abs() < 1e-3, "{ndc:?}");
        assert!(ndc.y.abs() < 1e-3);
    }

    #[test]
    fn ortho_fit_zoom_and_parallel_rays() {
        let b = Aabb {
            min: Vec3::new(-5.0, -3.0, 0.0),
            max: Vec3::new(8.0, 4.0, 6.0),
        };
        for aspect in [0.6, 1.0, 1.8] {
            let mut c = OrbitCamera {
                ortho: true,
                ..Default::default()
            };
            c.fit(&b, aspect);
            for i in 0..8 {
                let p = Vec3::new(
                    if i & 1 == 0 { b.min.x } else { b.max.x },
                    if i & 2 == 0 { b.min.y } else { b.max.y },
                    if i & 4 == 0 { b.min.z } else { b.max.z },
                );
                let n = c.project(p, aspect);
                assert!(n.x.abs() <= 1.0 && n.y.abs() <= 1.0 && (0.0..1.0).contains(&n.z));
            }
            let (r0, r1) = (c.ray(-0.7, 0.2, aspect), c.ray(0.5, -0.9, aspect));
            assert!(close(r0.dir, r1.dir, 1e-4) && close(r0.dir, -c.back(), 1e-4));
            let p = Vec3::new(2.0, 1.0, 1.0);
            let before = c.project(p, aspect);
            c.zoom_about(p, 0.5);
            let after = c.project(p, aspect);
            assert!((before.x - after.x).abs() < 1e-4 && (before.y - after.y).abs() < 1e-4);
        }
    }

    #[test]
    fn ortho_scale_matches_perspective_at_target() {
        let mut c = OrbitCamera::default();
        let p = c.target + Vec3::new(0.0, 0.0, 1.0);
        let a = c.project(p, 1.0).y;
        c.ortho = true;
        let b = c.project(p, 1.0).y;
        assert!((a - b).abs() < 0.05, "{a} {b}");
    }

    #[test]
    fn reversed_depth_near_is_one_and_far_keeps_precision() {
        let mut c = OrbitCamera {
            distance: 1.0,
            ..Default::default()
        };
        let f = -c.back();
        let z = |c: &OrbitCamera, d: f32| c.project(c.eye() + f * d, 1.0).z;
        // near is closer to 1, far to 0; depths at 300 and 300.5 m are distinguishable in float
        assert!(z(&c, 0.5) > z(&c, 50.0) && z(&c, 50.0) > 0.0);
        let (a, b) = (z(&c, 300.0), z(&c, 300.5));
        assert!(a > b && b > 0.0, "{a} {b}");
        assert!(c.project(c.eye() - f * 2.0, 1.0).z < 0.0, "behind the eye");
        c.ortho = true;
        assert!(z(&c, 0.0) < 1.0 && z(&c, 0.0) > z(&c, 5.0));
    }

    #[test]
    fn ortho_render_eye_sees_everything_in_front_of_it() {
        let mut c = OrbitCamera {
            ortho: true,
            distance: 0.05,
            scene_radius: 30.0,
            ..Default::default()
        };
        c.target = Vec3::new(5.0, 5.0, 2.0);
        let e = c.render_eye();
        // a point behind the orbit eye (inside the model) is in front of the virtual eye and in frame
        let p = c.eye() + c.back() * 20.0;
        assert!((p - e).dot(-c.back()) > 0.0);
        let n = c.project(p, 1.0);
        assert!(n.z > 0.0 && n.z < 1.0, "{n:?}");
        assert!((c.eye_offset() - (e - c.eye()).length()).abs() < 1e-3);
        let r = c.ray(0.0, 0.0, 1.0);
        assert!((r.origin - e).length() < 1e-2);
        c.ortho = false;
        assert_eq!(c.render_eye(), c.eye());
        assert_eq!(c.eye_offset(), 0.0);
    }

    #[test]
    fn straight_down_view_is_not_degenerate() {
        let mut c = OrbitCamera {
            pitch: std::f32::consts::FRAC_PI_2,
            ortho: true,
            ..Default::default()
        };
        let v = c.view();
        assert!(v.to_cols_array().iter().all(|x| x.is_finite()));
        let up = c.up();
        assert!(up.z.abs() < 1e-5 && (up.length() - 1.0).abs() < 1e-5);
        let t = c.target;
        c.pan(0.0, 100.0, 800.0);
        assert!((c.target - t).length() > 0.0 && (c.target.z - t.z).abs() < 1e-4);
    }

    #[test]
    fn fit_shows_whole_box() {
        let b = Aabb {
            min: Vec3::new(-50.0, -10.0, 0.0),
            max: Vec3::new(30.0, 40.0, 3.0),
        };
        for aspect in [0.5, 1.0, 2.0] {
            let mut c = OrbitCamera::default();
            c.fit(&b, aspect);
            for i in 0..8 {
                let p = Vec3::new(
                    if i & 1 == 0 { b.min.x } else { b.max.x },
                    if i & 2 == 0 { b.min.y } else { b.max.y },
                    if i & 4 == 0 { b.min.z } else { b.max.z },
                );
                let n = c.project(p, aspect);
                assert!(n.x.abs() <= 1.0 && n.y.abs() <= 1.0 && (0.0..1.0).contains(&n.z));
            }
        }
    }
}
