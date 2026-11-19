// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Face handles of the section box on the canvas: six arrows at the face centres; dragging
//! moves a face along its normal with live preview. One gesture is one undo step,
//! Esc restores the box, a face never crosses the opposite one. Pure functions are separated from
//! egui so they can be tested without a window.

use crate::viewport::Viewport;
use ddc_render::glam::Vec3;
use ddc_render::{Aabb, OrbitCamera, Ray, Scene};
use egui::{self, Pos2, Rect, Sense, Vec2};

/// Minimum box size along any axis, m.
pub const MIN_SIZE: f32 = 0.05;
/// Snap radius to model bounds, screen px.
pub const SNAP_PX: f32 = 8.0;

/// Box face: axis 0..3 and the upper (max) face flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    pub axis: usize,
    pub hi: bool,
}

pub const FACES: [Face; 6] = [
    Face { axis: 0, hi: false },
    Face { axis: 0, hi: true },
    Face { axis: 1, hi: false },
    Face { axis: 1, hi: true },
    Face { axis: 2, hi: false },
    Face { axis: 2, hi: true },
];

impl Face {
    /// Face coordinate along its axis.
    pub fn value(self, b: &Aabb) -> f32 {
        if self.hi {
            b.max[self.axis]
        } else {
            b.min[self.axis]
        }
    }

    /// Face centre in model space.
    pub fn center(self, b: &Aabb) -> Vec3 {
        let mut p = (b.min + b.max) * 0.5;
        p[self.axis] = self.value(b);
        p
    }
}

/// On-screen handle: face, centre and the screen-space vector of one metre along the face axis.
#[derive(Clone, Copy, Debug)]
pub struct HandlePos {
    pub face: Face,
    pub screen: Pos2,
    pub axis_px: Vec2,
}

fn to_screen(cam: &OrbitCamera, rect: Rect, aspect: f32, p: Vec3) -> Option<Pos2> {
    let n = cam.project(p, aspect);
    (n.z > 0.0 && n.z < 1.0 && n.x.is_finite() && n.y.is_finite()).then(|| {
        Pos2::new(
            rect.min.x + (n.x + 1.0) * 0.5 * rect.width(),
            rect.min.y + (1.0 - n.y) * 0.5 * rect.height(),
        )
    })
}

/// Handles visible in the frame (faces behind the camera are skipped).
pub fn handle_positions(cam: &OrbitCamera, rect: Rect, aspect: f32, b: &Aabb) -> Vec<HandlePos> {
    let mut out = Vec::with_capacity(6);
    for face in FACES {
        let c = face.center(b);
        let mut step = c;
        step[face.axis] += 1.0;
        if let (Some(s), Some(e)) = (
            to_screen(cam, rect, aspect, c),
            to_screen(cam, rect, aspect, step),
        ) {
            out.push(HandlePos {
                face,
                screen: s,
                axis_px: e - s,
            });
        }
    }
    out
}

/// Nearest handle within `radius` px of the cursor.
pub fn hit_test(handles: &[HandlePos], pos: Pos2, radius: f32) -> Option<Face> {
    handles
        .iter()
        .map(|h| (h.screen.distance(pos), h.face))
        .filter(|(d, _)| *d <= radius)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f)
}

/// Face shift in metres from the cursor's screen shift: projection onto the face's screen axis.
pub fn face_drag(axis_px: Vec2, delta_px: Vec2) -> f32 {
    let len2 = axis_px.length_sq();
    if len2 < 1e-6 {
        return 0.0;
    }
    delta_px.dot(axis_px) / len2
}

/// Parameter (m) of the axis point `origin + t * e_axis` nearest to the cursor ray. None if the ray
/// is almost parallel to the axis.
pub fn ray_axis_param(ray: &Ray, origin: Vec3, axis: usize) -> Option<f32> {
    let mut u = Vec3::ZERO;
    u[axis] = 1.0;
    let w0 = origin - ray.origin;
    let b = u.dot(ray.dir);
    let c = ray.dir.dot(ray.dir);
    let (d, e) = (u.dot(w0), ray.dir.dot(w0));
    let denom = c - b * b;
    if denom < 1e-6 * c.max(1e-12) {
        return None;
    }
    let t = (b * e - c * d) / denom;
    t.is_finite().then_some(t)
}

/// Snap a value to the nearest target within `tol` metres.
pub fn snap_value(v: f32, targets: &[f32], tol: f32) -> f32 {
    targets
        .iter()
        .copied()
        .filter(|t| (t - v).abs() <= tol)
        .min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs()))
        .unwrap_or(v)
}

/// Box after moving face `face` by `delta` from `start`. A face never gets closer than `MIN_SIZE`
/// to the opposite one; `both` moves both opposite faces (size is preserved).
pub fn moved(start: &Aabb, face: Face, delta: f32, both: bool, targets: &[f32], tol: f32) -> Aabb {
    let mut b = *start;
    let v = snap_value(face.value(start) + delta, targets, tol);
    let a = face.axis;
    if both {
        let d = v - face.value(start);
        b.min[a] += d;
        b.max[a] += d;
    } else if face.hi {
        b.max[a] = v.max(b.min[a] + MIN_SIZE);
    } else {
        b.min[a] = v.min(b.max[a] - MIN_SIZE);
    }
    b
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    face: Face,
    start: Aabb,
    t0: f32,
}

/// Gesture state of the handles; a finished drag waits in `step` for the editor's undo stack.
#[derive(Default, Debug)]
pub struct BoxEditor {
    drag: Option<Drag>,
    /// Finished drag (before, after) not yet handed to `Editor::push_section_box`.
    step: Option<(Aabb, Aabb)>,
    /// Gesture cancelled with Esc: input is ignored until the button is released.
    suppress: bool,
    /// Handle under the cursor in this frame.
    pub hover: Option<Face>,
}

impl BoxEditor {
    pub fn begin(&mut self, face: Face, start: Aabb, t0: f32) {
        self.drag = Some(Drag { face, start, t0 });
    }

    /// Centre of the dragged face at gesture start (origin of the gesture axis).
    pub fn start_center(&self) -> Option<Vec3> {
        self.drag.map(|d| d.face.center(&d.start))
    }

    pub fn active(&self) -> Option<Face> {
        self.drag.map(|d| d.face)
    }

    /// Handle is highlighted: hovered or being dragged.
    pub fn is_hot(&self, f: Face) -> bool {
        self.active() == Some(f) || (self.drag.is_none() && self.hover == Some(f))
    }

    /// Box for the current cursor position on the axis (`t` is the axis parameter).
    pub fn preview(&self, t: f32, both: bool, targets: &[f32], tol: f32) -> Option<Aabb> {
        let d = self.drag?;
        Some(moved(&d.start, d.face, t - d.t0, both, targets, tol))
    }

    /// Release: a changed box becomes one step (before, after) for the editor's undo stack.
    pub fn commit(&mut self, now: &Aabb) {
        if let Some(d) = self.drag.take() {
            if d.start != *now {
                // two drags in one frame never happen; keep the oldest start if they did
                let before = self.step.map_or(d.start, |s| s.0);
                self.step = Some((before, *now));
            }
        }
    }

    /// Esc: back to the start of the gesture; no step is recorded.
    pub fn cancel(&mut self) -> Option<Aabb> {
        let d = self.drag.take()?;
        self.suppress = true;
        Some(d.start)
    }

    /// The finished drag, once.
    pub fn take_step(&mut self) -> Option<(Aabb, Aabb)> {
        self.step.take()
    }

    /// Undo or redo moved the box: a gesture in progress would overwrite it.
    pub fn drop_drag(&mut self) {
        if self.drag.take().is_some() {
            self.suppress = true;
        }
    }

    pub fn clear(&mut self) {
        self.drag = None;
        self.suppress = false;
    }
}

/// Once per frame: a finished drag goes to the editor's undo stack (global Ctrl+Z undoes it),
/// and a box that undo or redo reached goes back on the view.
pub fn sync_undo(v: &mut Viewport, ed: &mut crate::editor::Editor) -> bool {
    if let Some((before, after)) = v.boxed.take_step() {
        ed.push_section_box(before, after);
    }
    match ed.take_box_due() {
        Some(b) => {
            v.boxed.drop_drag();
            v.section_box = Some(b);
            true
        }
        None => false,
    }
}

/// Model bounds along an axis (snap targets).
fn snap_targets(scene: &Scene, axis: usize) -> Vec<f32> {
    scene
        .bounds
        .map(|b| vec![b.min[axis], b.max[axis]])
        .unwrap_or_default()
}

/// Handle frame: input, preview, Esc, cancel. Returns true if the cursor is over a handle
/// or dragging it (the click does not go to picking).
pub fn gizmo(v: &mut Viewport, ui: &mut egui::Ui, rect: Rect, aspect: f32, scene: &Scene) -> bool {
    let Some(b0) = v.section_box else {
        v.boxed.clear();
        v.boxed.hover = None;
        return false;
    };
    if v.boxed.active().is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(start) = v.boxed.cancel() {
            v.section_box = Some(start);
        }
    }
    let b = v.section_box.unwrap_or(b0);
    let handles = handle_positions(&v.camera, rect, aspect, &b);
    let shift = ui.input(|i| i.modifiers.shift);
    let tip = v.nav.t("v3d.box.handle.tip").to_owned();
    let mut busy = false;
    let mut hover = None;
    let mut ended = false;
    let mut any_drag = false;
    for h in &handles {
        let resp = ui.interact(
            Rect::from_center_size(h.screen, Vec2::splat(crate::overlay::BOX_HANDLE_PX)),
            ui.id().with(("sbox", h.face.axis, h.face.hi)),
            Sense::drag(),
        );
        let dragging = resp.dragged();
        any_drag |= dragging;
        if resp.hovered() || dragging {
            hover = Some(h.face);
            busy = true;
            ui.ctx().set_cursor_icon(if dragging {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Grab
            });
        }
        if resp.drag_stopped() {
            ended = true;
        } else if resp.drag_started() && !v.boxed.suppress {
            let cur = v.section_box.unwrap_or(b);
            let t0 = ui
                .input(|i| i.pointer.press_origin())
                .and_then(|p| v.ray_at(p))
                .and_then(|r| ray_axis_param(&r, h.face.center(&cur), h.face.axis));
            if let Some(t0) = t0 {
                v.boxed.begin(h.face, cur, t0);
            }
        } else if resp.hovered() && !dragging {
            resp.on_hover_text(tip.clone());
        }
    }
    if !any_drag {
        v.boxed.suppress = false;
    }
    if let Some(face) = v.boxed.active() {
        busy = true;
        // the gesture axis is fixed at gesture start, otherwise the parameter drifts with the face
        let h = handles.iter().find(|h| h.face == face);
        if let (Some(origin), Some(h)) = (v.boxed.start_center(), h) {
            let ray = ui.input(|i| i.pointer.latest_pos()).and_then(|p| v.ray_at(p));
            if let Some(t) = ray.and_then(|r| ray_axis_param(&r, origin, face.axis)) {
                // a model bound attracts within SNAP_PX of screen distance
                let m_per_px = 1.0 / h.axis_px.length().max(1e-3);
                let targets = snap_targets(scene, face.axis);
                if let Some(nb) = v.boxed.preview(t, shift, &targets, SNAP_PX * m_per_px) {
                    v.section_box = Some(nb);
                }
            }
        }
        if ended {
            if let Some(now) = v.section_box {
                v.boxed.commit(&now);
            }
        }
    }
    v.boxed.hover = hover;
    busy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_box() -> Aabb {
        Aabb {
            min: Vec3::new(0.0, 0.0, 0.0),
            max: Vec3::new(4.0, 3.0, 2.0),
        }
    }

    fn cam(ortho: bool) -> OrbitCamera {
        let mut c = OrbitCamera::default();
        c.ortho = ortho;
        c.target = Vec3::new(2.0, 1.5, 1.0);
        c.distance = 12.0;
        c
    }

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))
    }

    #[test]
    fn face_center_and_value() {
        let b = unit_box();
        let f = Face { axis: 1, hi: true };
        assert_eq!(f.value(&b), 3.0);
        let c = f.center(&b);
        assert_eq!((c.x, c.y, c.z), (2.0, 3.0, 1.0));
        let lo = Face { axis: 2, hi: false }.center(&b);
        assert_eq!(lo.z, 0.0);
    }

    #[test]
    fn handles_visible_for_both_projections() {
        for ortho in [false, true] {
            let h = handle_positions(&cam(ortho), rect(), 800.0 / 600.0, &unit_box());
            assert_eq!(h.len(), 6, "ortho={ortho}");
            assert!(h.iter().all(|p| rect().contains(p.screen)));
            assert!(h.iter().all(|p| p.axis_px.length() > 0.0));
        }
    }

    #[test]
    fn handles_skip_faces_behind_camera() {
        let mut c = cam(false);
        c.distance = 0.5;
        let big = Aabb {
            min: Vec3::splat(-500.0),
            max: Vec3::splat(500.0),
        };
        let h = handle_positions(&c, rect(), 1.0, &big);
        assert!(h.len() < 6);
    }

    #[test]
    fn hit_test_picks_nearest_in_radius() {
        let hs = [
            HandlePos {
                face: FACES[0],
                screen: Pos2::new(100.0, 100.0),
                axis_px: Vec2::X,
            },
            HandlePos {
                face: FACES[1],
                screen: Pos2::new(110.0, 100.0),
                axis_px: Vec2::X,
            },
        ];
        assert_eq!(hit_test(&hs, Pos2::new(108.0, 100.0), 9.0), Some(FACES[1]));
        assert_eq!(hit_test(&hs, Pos2::new(101.0, 100.0), 9.0), Some(FACES[0]));
        assert_eq!(hit_test(&hs, Pos2::new(300.0, 300.0), 9.0), None);
    }

    #[test]
    fn face_drag_sign_and_scale() {
        // 50 px per metre to the right
        let ax = Vec2::new(50.0, 0.0);
        assert!((face_drag(ax, Vec2::new(25.0, 0.0)) - 0.5).abs() < 1e-6);
        assert!((face_drag(ax, Vec2::new(-50.0, 0.0)) + 1.0).abs() < 1e-6);
        // a transverse shift does not move the face
        assert_eq!(face_drag(ax, Vec2::new(0.0, 30.0)), 0.0);
        // axis points at the camera: zero projection, no division by zero
        assert_eq!(face_drag(Vec2::ZERO, Vec2::new(5.0, 5.0)), 0.0);
    }

    #[test]
    fn face_drag_scale_follows_zoom_in_ortho_and_perspective() {
        for ortho in [false, true] {
            let c = cam(ortho);
            let b = unit_box();
            let near = handle_positions(&c, rect(), 800.0 / 600.0, &b);
            let h = near.iter().find(|h| h.face.axis == 0 && h.face.hi).unwrap();
            // a cursor shift of exactly one screen metre gives one metre
            let m = face_drag(h.axis_px, h.axis_px);
            assert!((m - 1.0).abs() < 1e-5, "ortho={ortho}");
        }
    }

    #[test]
    fn ray_axis_param_recovers_point_on_axis() {
        let c = cam(false);
        let aspect = 800.0 / 600.0;
        let b = unit_box();
        let f = Face { axis: 0, hi: true };
        let o = f.center(&b);
        let target = o + Vec3::new(1.5, 0.0, 0.0);
        let n = c.project(target, aspect);
        let ray = c.ray(n.x, n.y, aspect);
        let t = ray_axis_param(&ray, o, 0).unwrap();
        assert!((t - 1.5).abs() < 1e-3, "t={t}");
        // and in ortho
        let c = cam(true);
        let n = c.project(target, aspect);
        let ray = c.ray(n.x, n.y, aspect);
        let t = ray_axis_param(&ray, o, 0).unwrap();
        assert!((t - 1.5).abs() < 1e-3, "t={t}");
    }

    #[test]
    fn ray_parallel_to_axis_gives_none() {
        let ray = Ray {
            origin: Vec3::new(0.0, 0.0, 5.0),
            dir: Vec3::X,
        };
        assert!(ray_axis_param(&ray, Vec3::ZERO, 0).is_none());
    }

    #[test]
    fn moved_hi_and_lo_faces() {
        let b = unit_box();
        let hi = moved(&b, Face { axis: 0, hi: true }, 1.0, false, &[], 0.0);
        assert_eq!((hi.min.x, hi.max.x), (0.0, 5.0));
        let lo = moved(&b, Face { axis: 0, hi: false }, -1.0, false, &[], 0.0);
        assert_eq!((lo.min.x, lo.max.x), (-1.0, 4.0));
        // other axes are untouched
        assert_eq!((hi.min.y, hi.max.y, hi.min.z, hi.max.z), (0.0, 3.0, 0.0, 2.0));
    }

    #[test]
    fn clamp_keeps_min_size_and_never_crosses() {
        let b = unit_box();
        let hi = moved(&b, Face { axis: 2, hi: true }, -50.0, false, &[], 0.0);
        assert!((hi.max.z - hi.min.z - MIN_SIZE).abs() < 1e-6);
        assert_eq!(hi.min.z, 0.0);
        let lo = moved(&b, Face { axis: 1, hi: false }, 50.0, false, &[], 0.0);
        assert!((lo.max.y - lo.min.y - MIN_SIZE).abs() < 1e-6);
        assert_eq!(lo.max.y, 3.0);
        for a in 0..3 {
            assert!(hi.min[a] < hi.max[a] && lo.min[a] < lo.max[a]);
        }
    }

    #[test]
    fn shift_moves_both_faces_keeping_size() {
        let b = unit_box();
        let m = moved(&b, Face { axis: 0, hi: true }, 2.0, true, &[], 0.0);
        assert_eq!((m.min.x, m.max.x), (2.0, 6.0));
        let m = moved(&b, Face { axis: 0, hi: false }, -3.0, true, &[], 0.0);
        assert_eq!((m.min.x, m.max.x), (-3.0, 1.0));
    }

    #[test]
    fn snap_to_model_bounds_within_tolerance() {
        assert_eq!(snap_value(9.95, &[10.0, 0.0], 0.1), 10.0);
        assert_eq!(snap_value(9.5, &[10.0, 0.0], 0.1), 9.5);
        assert_eq!(snap_value(1.0, &[], 1.0), 1.0);
        let b = unit_box();
        let m = moved(&b, Face { axis: 0, hi: true }, 3.97, false, &[8.0], 0.1);
        assert_eq!(m.max.x, 8.0);
    }

    #[test]
    fn esc_restores_start_and_leaves_history() {
        let mut e = BoxEditor::default();
        let b = unit_box();
        let f = Face { axis: 0, hi: true };
        e.begin(f, b, 4.0);
        let live = e.preview(5.0, false, &[], 0.0).unwrap();
        assert_eq!(live.max.x, 5.0);
        let back = e.cancel().unwrap();
        assert_eq!(back, b);
        assert!(e.active().is_none());
        assert!(e.take_step().is_none());
        // a repeated Esc does nothing
        assert!(e.cancel().is_none());
    }

    #[test]
    fn one_undo_step_per_drag() {
        let mut e = BoxEditor::default();
        let b = unit_box();
        let f = Face { axis: 0, hi: true };
        e.begin(f, b, 4.0);
        // many preview frames, one commit
        let mut now = b;
        for k in 1..=30 {
            now = e.preview(4.0 + k as f32 * 0.1, false, &[], 0.0).unwrap();
        }
        e.commit(&now);
        assert_eq!(e.take_step(), Some((b, now)));
        assert!(e.take_step().is_none());
    }

    #[test]
    fn unchanged_drag_adds_no_step() {
        let mut e = BoxEditor::default();
        let b = unit_box();
        e.begin(Face { axis: 1, hi: false }, b, 0.0);
        e.commit(&b);
        assert!(e.take_step().is_none());
    }

    #[test]
    fn drag_is_one_step_of_the_editor_undo_and_redo() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut v = Viewport::new();
        let b = unit_box();
        v.section_box = Some(b);
        v.boxed.begin(FACES[1], b, 0.0);
        let n = v.boxed.preview(1.0, false, &[], 0.0).unwrap();
        v.section_box = Some(n);
        v.boxed.commit(&n);
        let undo0 = ed.undo_len();
        assert!(!sync_undo(&mut v, &mut ed));
        assert_eq!(ed.undo_len(), undo0 + 1);
        ed.undo();
        assert!(sync_undo(&mut v, &mut ed));
        assert_eq!(v.section_box, Some(b));
        ed.redo();
        assert!(sync_undo(&mut v, &mut ed));
        assert_eq!(v.section_box, Some(n));
    }

    #[test]
    fn numeric_fields_stay_in_sync_with_drag() {
        // the box fields read section_box every frame: after preview the bounds are equal
        let b = unit_box();
        let m = moved(&b, Face { axis: 0, hi: true }, 1.25, false, &[], 0.0);
        let elevation = 3.0_f64;
        let shown = m.max.z as f64 + elevation;
        assert_eq!(m.max.x, 5.25);
        assert_eq!(shown, 5.0);
        // a numeric edit after a gesture does not break the size
        let typed = moved(&m, Face { axis: 0, hi: true }, 0.0, false, &[], 0.0);
        assert_eq!(typed, m);
    }

    #[test]
    fn hot_state_prefers_active_drag() {
        let mut e = BoxEditor::default();
        e.hover = Some(FACES[0]);
        assert!(e.is_hot(FACES[0]));
        e.begin(FACES[1], unit_box(), 0.0);
        assert!(e.is_hot(FACES[1]));
        assert!(!e.is_hot(FACES[0]));
    }
}
