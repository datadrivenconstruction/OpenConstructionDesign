//! View adapters: a view supplies only its projection; the adapter turns a screen
//! position into a `Pointer` and a model point back into the screen. Plan, 3D and section are
//! three adapters of one trait, all in f64 model coordinates.
use super::pointer::{Pointer, Ray3, Source, ViewKind, WorkPlane};
use ddc_geom::{v2, v3, V2, V3};

/// Distance from the cut plane at which a section ray starts.
const SECTION_EYE: f64 = 1.0;

pub trait ViewAdapter {
    fn kind(&self) -> ViewKind;
    /// Pointer at a screen position given in f64 (scripts and parity tests skip the f32 rounding).
    fn pointer_at(&self, screen: [f64; 2]) -> Pointer;
    /// Screen position of a model point in f64; `None` behind the camera.
    fn project_at(&self, p: V3) -> Option<[f64; 2]>;
    /// Screen pixels per model metre at `at`.
    fn px_per_m(&self, at: V3) -> f64;
    /// Cut plane of a section view (volumes are drawn as their cut); `None` elsewhere.
    fn cut(&self) -> Option<WorkPlane> {
        None
    }

    fn pointer(&self, screen: [f32; 2]) -> Pointer {
        self.pointer_at([screen[0] as f64, screen[1] as f64])
    }
    fn project(&self, p: V3) -> Option<[f32; 2]> {
        self.project_at(p).map(|s| [s[0] as f32, s[1] as f32])
    }
}

fn screen32(s: [f64; 2]) -> [f32; 2] {
    [s[0] as f32, s[1] as f32]
}

/// Top-down orthographic plan of a level: `scale` px per metre, `center` (model) drawn at
/// `canvas` (screen), screen y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanAdapter {
    pub center: V2,
    pub scale: f64,
    pub canvas: [f64; 2],
    pub level: i64,
    /// Elevation of the level, m.
    pub z: f64,
}

impl PlanAdapter {
    pub fn to_world(&self, s: [f64; 2]) -> V2 {
        v2(
            self.center.x + (s[0] - self.canvas[0]) / self.scale,
            self.center.y - (s[1] - self.canvas[1]) / self.scale,
        )
    }
}

impl ViewAdapter for PlanAdapter {
    fn kind(&self) -> ViewKind {
        ViewKind::Plan { level: self.level }
    }
    fn pointer_at(&self, s: [f64; 2]) -> Pointer {
        let mut p = Pointer::at_plan(self.to_world(s), self.level, self.z, self.scale);
        p.screen = screen32(s);
        p
    }
    fn project_at(&self, p: V3) -> Option<[f64; 2]> {
        Some([
            self.canvas[0] + (p.x - self.center.x) * self.scale,
            self.canvas[1] - (p.y - self.center.y) * self.scale,
        ])
    }
    fn px_per_m(&self, _at: V3) -> f64 {
        self.scale
    }
}

/// Orbit camera of the 3D view in f64. Mirrors `ddc_render::OrbitCamera::ray` (perspective
/// from the eye, orthographic from the virtual eye) so the pointer and the picture agree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View3dAdapter {
    pub eye: V3,
    pub fwd: V3,
    pub right: V3,
    pub up: V3,
    /// tan(fov_y / 2).
    pub tan: f64,
    pub ortho: bool,
    /// Half height of the orthographic frame, m.
    pub half_h: f64,
    /// Screen rectangle: min corner and size, px.
    pub min: [f64; 2],
    pub size: [f64; 2],
    /// Model elevation of the scene origin (scene z is measured from the active level).
    pub z0: f64,
    /// Work plane in model coordinates.
    pub plane: WorkPlane,
}

impl View3dAdapter {
    pub fn new(
        cam: &ddc_render::OrbitCamera,
        min: [f64; 2],
        size: [f64; 2],
        z0: f64,
        plane: WorkPlane,
    ) -> View3dAdapter {
        let (pitch, yaw) = (cam.pitch as f64, cam.yaw as f64);
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let back = v3(cp * cy, cp * sy, sp);
        let right = v3(-sy, cy, 0.0);
        let fwd = -back;
        let up = right.cross(fwd);
        let target = v3(cam.target.x as f64, cam.target.y as f64, cam.target.z as f64 + z0);
        let dist = cam.distance as f64;
        let tan = (cam.fov_y as f64 * 0.5).tan();
        // the virtual orthographic eye sits behind the near plane (`OrbitCamera::render_eye`)
        let back_by = if cam.ortho { dist + 4.0 * cam.scene_radius as f64 + 10.0 } else { dist };
        View3dAdapter {
            eye: target + back * back_by,
            fwd,
            right,
            up,
            tan,
            ortho: cam.ortho,
            half_h: dist * tan,
            min,
            size: [size[0].max(1.0), size[1].max(1.0)],
            z0,
            plane,
        }
    }

    fn aspect(&self) -> f64 {
        (self.size[0] / self.size[1]).max(1e-3)
    }

    fn ndc(&self, s: [f64; 2]) -> (f64, f64) {
        ((s[0] - self.min[0]) / self.size[0] * 2.0 - 1.0, 1.0 - (s[1] - self.min[1]) / self.size[1] * 2.0)
    }
}

impl ViewAdapter for View3dAdapter {
    fn kind(&self) -> ViewKind {
        ViewKind::View3d
    }
    fn pointer_at(&self, s: [f64; 2]) -> Pointer {
        let (x, y) = self.ndc(s);
        let a = self.aspect();
        let ray = if self.ortho {
            let h = self.half_h;
            Ray3 { o: self.eye + self.right * (x * h * a) + self.up * (y * h), d: self.fwd }
        } else {
            let d = self.fwd + self.right * (x * self.tan * a) + self.up * (y * self.tan);
            Ray3 { o: self.eye, d: d.normalized() }
        };
        let at = self.plane.hit(&ray).unwrap_or(ray.at(1.0));
        Pointer {
            ray,
            plane: self.plane,
            px_per_m: self.px_per_m(at),
            view: ViewKind::View3d,
            screen: screen32(s),
            host: None,
        }
    }
    fn project_at(&self, p: V3) -> Option<[f64; 2]> {
        let d = p - self.eye;
        let depth = d.dot(self.fwd);
        if depth <= 0.0 {
            return None;
        }
        let a = self.aspect();
        let (x, y) = if self.ortho {
            (d.dot(self.right) / (self.half_h * a), d.dot(self.up) / self.half_h)
        } else {
            (d.dot(self.right) / (depth * self.tan * a), d.dot(self.up) / (depth * self.tan))
        };
        Some([self.min[0] + (x + 1.0) * 0.5 * self.size[0], self.min[1] + (1.0 - y) * 0.5 * self.size[1]])
    }
    fn px_per_m(&self, at: V3) -> f64 {
        let half_px = self.size[1] * 0.5;
        if self.ortho {
            return half_px / self.half_h.max(1e-9);
        }
        half_px / ((at - self.eye).dot(self.fwd).max(1e-6) * self.tan)
    }
}

/// Section or elevation along the plan line a→b: the drawing is (s, z) with s measured from `a`,
/// the view looks along the left normal of a→b (`editor::sec::look`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionAdapter {
    pub view: i64,
    /// Drawing point (s, z) shown at `canvas`.
    pub center: V2,
    pub scale: f64,
    pub canvas: [f64; 2],
    /// Cut plane: origin `a` at z 0, `u` along a→b, `v` up, `n` the view direction.
    pub plane: WorkPlane,
}

impl SectionAdapter {
    /// `None` for a section line of zero length.
    pub fn new(view: i64, a: V2, b: V2, center: V2, scale: f64, canvas: [f64; 2]) -> Option<SectionAdapter> {
        let plane = WorkPlane::vertical(Source::View(view), a, b)?;
        Some(SectionAdapter { view, center, scale, canvas, plane })
    }

    /// Drawing coordinates (s, z) of a screen position.
    pub fn to_drawing(&self, s: [f64; 2]) -> V2 {
        v2(
            self.center.x + (s[0] - self.canvas[0]) / self.scale,
            self.center.y - (s[1] - self.canvas[1]) / self.scale,
        )
    }
}

impl ViewAdapter for SectionAdapter {
    fn kind(&self) -> ViewKind {
        ViewKind::Section { view: self.view }
    }
    fn pointer_at(&self, s: [f64; 2]) -> Pointer {
        let w = self.plane.from_uv(self.to_drawing(s));
        Pointer {
            ray: Ray3 { o: w - self.plane.n * SECTION_EYE, d: self.plane.n },
            plane: self.plane,
            px_per_m: self.scale,
            view: self.kind(),
            screen: screen32(s),
            host: None,
        }
    }
    fn project_at(&self, p: V3) -> Option<[f64; 2]> {
        let q = self.plane.to_uv(p);
        Some([
            self.canvas[0] + (q.x - self.center.x) * self.scale,
            self.canvas[1] - (q.y - self.center.y) * self.scale,
        ])
    }
    fn px_per_m(&self, _at: V3) -> f64 {
        self.scale
    }
    fn cut(&self) -> Option<WorkPlane> {
        Some(self.plane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: V3, b: V3, tol: f64) -> bool {
        (a - b).len() <= tol
    }

    fn plan() -> PlanAdapter {
        PlanAdapter { center: v2(0.0, 0.0), scale: 100.0, canvas: [0.0, 0.0], level: 1, z: 3.0 }
    }

    fn cam(ortho: bool) -> ddc_render::OrbitCamera {
        ddc_render::OrbitCamera {
            target: ddc_render::glam::Vec3::new(2.0, -1.0, 0.5),
            yaw: -2.2,
            pitch: 0.6,
            distance: 18.0,
            ortho,
            ..Default::default()
        }
    }

    /// 3D view of the level at elevation 3 (scene origin there), work plane on the level.
    fn view3d(ortho: bool) -> View3dAdapter {
        View3dAdapter::new(&cam(ortho), [10.0, 20.0], [800.0, 600.0], 3.0, WorkPlane::level(1, 3.0))
    }

    fn section() -> SectionAdapter {
        SectionAdapter::new(7, v2(1.45, -1.0), v2(1.45, 5.0), v2(0.0, 0.0), 100.0, [0.0, 0.0]).unwrap()
    }

    #[test]
    fn plan_pointer_is_level_point() {
        let p = plan().pointer([400.0, 300.0]);
        assert!(close(p.point().unwrap(), v3(4.0, -3.0, 3.0), 1e-12));
        assert_eq!(p.plan(), Some(v2(4.0, -3.0)));
        assert_eq!(p.view, ViewKind::Plan { level: 1 });
        assert_eq!(p.screen, [400.0, 300.0]);
    }

    #[test]
    fn section_pointer_maps_s_z_to_model() {
        // drawing (s, z) = (2.5, 4) on the cut x = 1.45 that starts at y = -1
        let a = section();
        let p = a.pointer_at([250.0, -400.0]);
        assert!(close(p.point().unwrap(), v3(1.45, 1.5, 4.0), 1e-12));
        assert_eq!(p.view, ViewKind::Section { view: 7 });
        // the section looks along the left normal of a→b
        assert!(close(p.ray.d, v3(-1.0, 0.0, 0.0), 1e-12));
    }

    #[test]
    fn plan_3d_section_same_world_point() {
        let w = v3(1.45, 2.25, 3.0);
        let pl = plan();
        let p_plan = pl.pointer_at(pl.project_at(w).unwrap()).point().unwrap();
        let s = section();
        let p_sec = s.pointer_at(s.project_at(w).unwrap()).point().unwrap();
        assert!(close(p_plan, w, 1e-9), "plan {p_plan:?}");
        assert!(close(p_sec, w, 1e-9), "section {p_sec:?}");
        for ortho in [false, true] {
            let a = view3d(ortho);
            let p_3d = a.pointer_at(a.project_at(w).unwrap()).point().unwrap();
            assert!(close(p_3d, w, 1e-9), "3d ortho={ortho} {p_3d:?}");
            assert!(close(p_3d, p_plan, 1e-9) && close(p_3d, p_sec, 1e-9));
        }
    }

    #[test]
    fn roundtrip_project_pointer() {
        let adapters: [&dyn ViewAdapter; 4] = [&plan(), &view3d(false), &view3d(true), &section()];
        let world = [v3(0.0, 0.0, 3.0), v3(4.0, -3.0, 3.0), v3(1.45, 2.0, 3.0), v3(-2.5, 6.0, 3.0)];
        for a in adapters {
            for &w in &world {
                // model → screen → model, f64 all the way
                let s = a.project_at(w).unwrap();
                let back = a.pointer_at(s);
                let q = back.point().unwrap();
                // a section pointer lands on its cut plane: compare the projection of w there
                let want = match a.kind() {
                    ViewKind::Section { .. } => {
                        let pl = back.plane;
                        pl.from_uv(pl.to_uv(w))
                    }
                    _ => w,
                };
                assert!(close(q, want, 1e-9), "{:?}: {w:?} -> {q:?}", a.kind());
                // screen → model → screen through the f32 screen of the UI
                let s32 = a.project(w).unwrap();
                let again = a.project(a.pointer(s32).point().unwrap()).unwrap();
                assert!((again[0] - s32[0]).abs() <= 1e-3 && (again[1] - s32[1]).abs() <= 1e-3, "{:?}", a.kind());
            }
        }
    }

    #[test]
    fn view3d_matches_render_camera_ray() {
        // the f64 ray agrees with the renderer's f32 ray to float precision
        for ortho in [false, true] {
            let c = cam(ortho);
            let a = View3dAdapter::new(&c, [0.0, 0.0], [800.0, 600.0], 0.0, WorkPlane::level(0, 0.0));
            let r32 = c.ray(0.25, -0.4, 800.0 / 600.0);
            let p = a.pointer_at([(0.25 + 1.0) * 400.0, (1.0 + 0.4) * 300.0]);
            let d = v3(r32.dir.x as f64, r32.dir.y as f64, r32.dir.z as f64);
            assert!(close(p.ray.d, d, 1e-5), "ortho={ortho}");
            let o = v3(r32.origin.x as f64, r32.origin.y as f64, r32.origin.z as f64);
            assert!((o - p.ray.o).cross(p.ray.d).len() < 1e-3, "same line, ortho={ortho}");
        }
    }

    #[test]
    fn ray_along_plane_gives_no_point() {
        let mut p = plan().pointer([10.0, 10.0]);
        p.ray.d = v3(1.0, 0.0, 0.0);
        assert_eq!(p.point(), None);
        assert_eq!(p.plan(), None);
    }

    #[test]
    fn pointer_under_1us() {
        const N: usize = 20_000;
        let bound = if cfg!(debug_assertions) { 20e-6 } else { 1e-6 };
        let adapters: [&dyn ViewAdapter; 3] = [&plan(), &view3d(false), &section()];
        for a in adapters {
            let mut best = f64::MAX;
            for _ in 0..3 {
                let t = std::time::Instant::now();
                let mut acc = 0.0;
                for i in 0..N {
                    let p = a.pointer([(i % 800) as f32, (i % 600) as f32]);
                    acc += p.point().map_or(0.0, |q| q.x);
                }
                std::hint::black_box(acc);
                best = best.min(t.elapsed().as_secs_f64() / N as f64);
            }
            assert!(best <= bound, "{:?}: {:.3} us", a.kind(), best * 1e6);
        }
    }
}
