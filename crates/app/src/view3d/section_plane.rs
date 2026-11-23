// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Temporary section plane of the 3D view: the view is cut by one plane facing the camera and the
//! near half is hidden. It is a section box with five faces outside the model and one face, the
//! plane, inside it: rendering, picking, snaps and the face-drag undo step all come with the box.
//! Only the plane's grip is shown; toggling off brings back the box the view had before.

use super::boxdrag::{Face, MIN_SIZE};
use crate::viewport::Viewport;
use ddc_render::glam::Vec3;
use ddc_render::Aabb;

/// Margin of the five outer faces around the model, m.
pub const PAD: f32 = 1.0;

/// The plane on the view: its face of `frame`, and the section box it replaced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub face: Face,
    /// Box at the toggle; only `face` of the live box may differ from it.
    pub frame: Aabb,
    pub saved: Option<Aabb>,
}

/// Face of the cut for a view direction: the dominant axis; the half on the camera's side goes.
pub fn face_toward(fwd: Vec3) -> Face {
    let a = fwd.abs();
    let axis = if a.x >= a.y && a.x >= a.z {
        0
    } else if a.y >= a.z {
        1
    } else {
        2
    };
    // looking along +axis the camera is on the low side: the kept box starts at the plane
    Face {
        axis,
        hi: fwd[axis] < 0.0,
    }
}

/// Box of a plane at `at` along `face.axis`: the model bounds with a margin, `face` moved to the plane.
pub fn plane_box(bounds: Aabb, face: Face, at: f32) -> Aabb {
    let pad = Vec3::splat(PAD);
    let mut b = Aabb {
        min: bounds.min - pad,
        max: bounds.max + pad,
    };
    let a = face.axis;
    let v = at.clamp(b.min[a] + MIN_SIZE, b.max[a] - MIN_SIZE);
    if face.hi {
        b.max[a] = v;
    } else {
        b.min[a] = v;
    }
    b
}

/// The plane's box is still on the view: only the cut face moved since the toggle. A box set
/// any other way (fit, level, a saved view) ends the plane by itself.
pub fn active(v: &Viewport) -> Option<Face> {
    let c = v.section_plane?;
    let b = v.section_box?;
    let other = |p: Vec3, q: Vec3, hi: bool| {
        (0..3).all(|k| (k == c.face.axis && c.face.hi == hi) || p[k] == q[k])
    };
    (other(b.min, c.frame.min, false) && other(b.max, c.frame.max, true)).then_some(c.face)
}

/// Toggle: on cuts the view through the centre of `around` (selection or model) facing the
/// camera; off restores the box the view had. Returns whether the plane is on.
pub fn toggle(v: &mut Viewport, bounds: Option<Aabb>, around: Option<Aabb>) -> bool {
    if active(v).is_some() {
        let c = v.section_plane.take();
        v.section_box = c.and_then(|c| c.saved);
        v.boxed.clear();
        return false;
    }
    v.section_plane = None;
    let Some(b) = bounds.or(around) else {
        return false;
    };
    let fwd = v.camera.ray(0.0, 0.0, 1.0).dir;
    let face = face_toward(fwd);
    let c = around.unwrap_or(b);
    let at = (c.min[face.axis] + c.max[face.axis]) * 0.5;
    let frame = plane_box(b, face, at);
    v.section_plane = Some(Cut {
        face,
        frame,
        saved: v.section_box,
    });
    v.section_box = Some(frame);
    v.boxed.clear();
    v.nav.caps = true;
    v.nav.level_view = None;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::view3d::boxdrag::{handle_positions, sync_undo, FACES};

    fn model() -> Aabb {
        Aabb {
            min: Vec3::ZERO,
            max: Vec3::new(10.0, 6.0, 3.0),
        }
    }

    fn inside(b: &Aabb, p: Vec3) -> bool {
        (0..3).all(|k| p[k] >= b.min[k] && p[k] <= b.max[k])
    }

    #[test]
    fn face_toward_cuts_the_camera_side() {
        assert_eq!(
            face_toward(Vec3::new(1.0, 0.2, -0.1)),
            Face { axis: 0, hi: false }
        );
        assert_eq!(
            face_toward(Vec3::new(0.1, -0.9, -0.3)),
            Face { axis: 1, hi: true }
        );
        assert_eq!(
            face_toward(Vec3::new(0.1, 0.2, -0.9)),
            Face { axis: 2, hi: true }
        );
    }

    #[test]
    fn plane_box_keeps_the_far_half_and_stays_inside_the_margin() {
        let f = Face { axis: 0, hi: false };
        let b = plane_box(model(), f, 4.0);
        assert!(
            inside(&b, Vec3::new(6.0, 3.0, 1.0)),
            "behind the plane stays"
        );
        assert!(
            !inside(&b, Vec3::new(2.0, 3.0, 1.0)),
            "in front of the plane is cut"
        );
        assert!(
            b.max.x > 10.0 && b.min.y < 0.0 && b.max.z > 3.0,
            "other faces outside the model"
        );
        let far = plane_box(model(), f, 99.0);
        assert!(far.max.x - far.min.x >= MIN_SIZE, "never inverted");
    }

    #[test]
    fn toggle_on_off_restores_the_box_and_a_refit_ends_the_plane() {
        let mut v = Viewport::new();
        let user = Aabb {
            min: Vec3::splat(1.0),
            max: Vec3::splat(2.0),
        };
        v.section_box = Some(user);
        assert!(toggle(&mut v, Some(model()), None));
        let f = active(&v).expect("plane on");
        let b = v.section_box.unwrap();
        let at = f.value(&b);
        let mid = (model().min[f.axis] + model().max[f.axis]) * 0.5;
        assert!((at - mid).abs() < 1e-5, "through the centre");
        // the cut face moves: still the plane
        let mut moved = b;
        if f.hi {
            moved.max[f.axis] -= 0.5;
        } else {
            moved.min[f.axis] += 0.5;
        }
        v.section_box = Some(moved);
        assert_eq!(active(&v), Some(f));
        assert!(!toggle(&mut v, Some(model()), None));
        assert_eq!(v.section_box, Some(user), "the earlier box is back");
        assert_eq!(v.section_plane, None);
        // another box set over the plane (fit, level view) is no longer the plane
        toggle(&mut v, Some(model()), None);
        v.section_box = Some(model());
        assert_eq!(active(&v), None);
        assert!(
            toggle(&mut v, Some(model()), None),
            "the next toggle starts a new plane"
        );
    }

    #[test]
    fn only_the_plane_grip_is_offered() {
        let mut v = Viewport::new();
        v.camera.target = Vec3::new(5.0, 3.0, 1.5);
        v.camera.distance = 30.0;
        toggle(&mut v, Some(model()), None);
        let f = active(&v).unwrap();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let all = handle_positions(&v.camera, rect, 800.0 / 600.0, &v.section_box.unwrap());
        assert!(all.len() > 1);
        let kept = super::super::boxdrag::offered(&v, all);
        assert!(kept.iter().all(|h| h.face == f) && !kept.is_empty());
        assert!(FACES.contains(&f));
    }

    #[test]
    fn plane_drag_is_one_undo_step() {
        let mut v = Viewport::new();
        let mut ed = Editor::new_in_memory().unwrap();
        toggle(&mut v, Some(model()), None);
        let f = active(&v).unwrap();
        let start = v.section_box.unwrap();
        v.boxed.begin(f, start, 0.0);
        let to = v.boxed.preview(0.7, false, &[], 0.0).unwrap();
        v.section_box = Some(to);
        v.boxed.commit(&to);
        let pushes = ed.pushes;
        sync_undo(&mut v, &mut ed);
        assert_eq!(ed.pushes - pushes, 1, "one undo step");
        ed.undo();
        assert!(sync_undo(&mut v, &mut ed));
        assert_eq!(v.section_box, Some(start), "undo puts the plane back");
        assert_eq!(active(&v), Some(f));
    }
}
