// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! UI-free 3D navigation: camera poses and smooth transitions between them, orbit around a pivot
//! (selection or the point under the cursor), view cube (faces, edges, corners — 26 directions),
//! walk/fly (WASD) with speed scaled to the model size.

use crate::camera::{OrbitCamera, Ray};
use crate::scene::Aabb;
use glam::{Mat3, Quat, Vec2, Vec3};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// Pitch limit when rotating with the mouse (slightly under 90°).
pub const PITCH_MAX: f32 = 1.55;
/// Pitch limit of preset views (cube, saved): straight down/up — the frame "up" axis
/// is taken from the azimuth (`OrbitCamera::up`).
pub const PITCH_TOP: f32 = FRAC_PI_2;
/// Walking speed, m/s (multiplier — `[`/`]`, Shift).
pub const WALK_SPEED: f32 = 1.4;
/// Speed limit for walking and flying, m/s.
pub const MAX_SPEED: f32 = 40.0;

/// Orbit camera placement without the projection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

impl OrbitCamera {
    pub fn pose(&self) -> Pose {
        Pose {
            target: self.target,
            yaw: self.yaw,
            pitch: self.pitch,
            distance: self.distance,
        }
    }

    pub fn set_pose(&mut self, p: &Pose) {
        self.target = p.target;
        self.yaw = p.yaw;
        self.pitch = p.pitch.clamp(-PITCH_TOP, PITCH_TOP);
        self.distance = p.distance.max(1e-3);
    }

    /// "Show all" pose for a given view direction (projection — the current one).
    pub fn fit_pose(&self, b: &Aabb, aspect: f32, yaw: f32, pitch: f32) -> Pose {
        let mut c = self.clone();
        c.yaw = yaw;
        c.pitch = pitch.clamp(-PITCH_TOP, PITCH_TOP);
        c.fit(b, aspect);
        c.pose()
    }

    /// Camera "right" axis (horizontal: no roll).
    pub fn right(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(-sy, cy, 0.0)
    }

    /// Orbit around a pivot: the camera rotates as a rigid body around `pivot`
    /// (first pitch around the "right" axis, then rotation around Z) — the pivot stays
    /// at the same screen point.
    pub fn orbit_about(&mut self, pivot: Vec3, dx_px: f32, dy_px: f32) {
        let (y0, p0) = (self.yaw, self.pitch);
        let right0 = self.right();
        self.orbit(dx_px, dy_px);
        let (dyaw, dpitch) = (self.yaw - y0, self.pitch - p0);
        if dyaw == 0.0 && dpitch == 0.0 {
            return;
        }
        // rotating around "right" by −dθ raises the eye direction by +dθ
        let r = Quat::from_rotation_z(dyaw) * Quat::from_axis_angle(right0, -dpitch);
        self.target = pivot + r * (self.target - pivot);
    }

    /// First-person look: the eye stays put, the view direction turns.
    pub fn look(&mut self, dx_px: f32, dy_px: f32) {
        let e = self.eye();
        self.orbit(dx_px, dy_px);
        self.target = e - self.back() * self.distance;
    }

    /// Eye-based camera: same viewpoint, target at distance `d` along the view direction.
    pub fn to_first_person(&mut self, d: f32) {
        let e = self.eye();
        self.distance = d.max(0.05);
        self.target = e - self.back() * self.distance;
    }

    /// Walk/fly step: `m` — input axes (−1…1), speed in m/s, `dt` in s. When walking,
    /// forward motion is horizontal (eye level does not change except for "up/down").
    pub fn walk(&mut self, m: Move, speed: f32, dt: f32, fly: bool) {
        let look = -self.back();
        let fwd = if fly {
            look
        } else {
            Vec3::new(look.x, look.y, 0.0).normalize_or(Vec3::Z.cross(self.right()))
        };
        let d = fwd * m.fwd + self.right() * m.right + Vec3::Z * m.up;
        if d != Vec3::ZERO {
            self.target += d * speed * dt;
        }
    }
}

/// Walk input axes: forward (W/S), right (D/A), up (E/Q).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Move {
    pub fwd: f32,
    pub right: f32,
    pub up: f32,
}

/// Base speed: walking — a human pace, flying — scaled to the model size (diagonal / 12 s,
/// from 1 m/s to `MAX_SPEED`).
pub fn walk_speed(bounds: Option<&Aabb>, fly: bool) -> f32 {
    if !fly {
        return WALK_SPEED;
    }
    bounds
        .map(|b| (b.max - b.min).length() / 12.0)
        .unwrap_or(1.5)
        .clamp(1.0, MAX_SPEED)
}

/// Frame speed with multipliers, at most `MAX_SPEED`.
pub fn clamped_speed(base: f32, mult: f32) -> f32 {
    (base * mult).clamp(0.05, MAX_SPEED)
}

/// Smooth transition between poses (smoothstep, shortest turn in azimuth,
/// distance interpolated geometrically: zooming in and out look equally fast to the eye).
#[derive(Clone, Copy, Debug)]
pub struct Transition {
    pub from: Pose,
    pub to: Pose,
    pub t: f32,
    pub dur: f32,
}

/// Default transition duration, s.
pub const TRANSITION_S: f32 = 0.35;

impl Transition {
    pub fn new(from: Pose, to: Pose, dur: f32) -> Self {
        Self {
            from,
            to,
            t: 0.0,
            dur: dur.max(1e-3),
        }
    }
    /// Pose after step `dt`; `true` — the transition is finished.
    pub fn step(&mut self, dt: f32) -> (Pose, bool) {
        self.t = (self.t + dt.max(0.0)).min(self.dur);
        let s = self.t / self.dur;
        let done = s >= 1.0;
        let p = if done {
            self.to
        } else {
            lerp_pose(&self.from, &self.to, s * s * (3.0 - 2.0 * s))
        };
        (p, done)
    }
}

/// Angle `a` normalised to (−π, π].
pub fn wrap_angle(a: f32) -> f32 {
    let r = (a + PI).rem_euclid(TAU) - PI;
    if r <= -PI {
        r + TAU
    } else {
        r
    }
}

pub fn lerp_pose(a: &Pose, b: &Pose, s: f32) -> Pose {
    let dy = wrap_angle(b.yaw - a.yaw);
    let (da, db) = (a.distance.max(1e-4).ln(), b.distance.max(1e-4).ln());
    Pose {
        target: a.target.lerp(b.target, s),
        yaw: a.yaw + dy * s,
        pitch: a.pitch + (b.pitch - a.pitch) * s,
        distance: (da + (db - da) * s).exp(),
    }
}

/// View-cube zone: components −1/0/1 along X, Y, Z (face — one nonzero, edge — two, corner — three).
pub type Zone = [i8; 3];

/// Fraction of the cube half-edge taken by edge and corner zones.
pub const CUBE_BAND: f32 = 0.36;

/// Zone for a point on the surface of the cube [−1, 1]³.
pub fn cube_zone(p: Vec3, band: f32) -> Zone {
    let lim = 1.0 - band;
    let c = |v: f32| {
        if v >= lim {
            1
        } else if v <= -lim {
            -1
        } else {
            0
        }
    };
    let z = [c(p.x), c(p.y), c(p.z)];
    if z == [0, 0, 0] {
        // point inside (imprecision): nearest face
        let a = p.abs();
        let k = if a.x >= a.y && a.x >= a.z {
            0
        } else if a.y >= a.z {
            1
        } else {
            2
        };
        let mut f = [0i8; 3];
        f[k] = if p[k] >= 0.0 { 1 } else { -1 };
        return f;
    }
    z
}

/// Ray intersection with the cube [−1, 1]³: entry parameter.
pub fn ray_cube(r: &Ray) -> Option<f32> {
    let (mut t0, mut t1) = (f32::NEG_INFINITY, f32::INFINITY);
    for k in 0..3 {
        let (o, d) = (r.origin[k], r.dir[k]);
        if d.abs() < 1e-9 {
            if o.abs() > 1.0 {
                return None;
            }
            continue;
        }
        let (a, b) = ((-1.0 - o) / d, (1.0 - o) / d);
        t0 = t0.max(a.min(b));
        t1 = t1.min(a.max(b));
    }
    (t0 <= t1 && t1 >= 0.0).then_some(t0.max(0.0))
}

/// Cube zone under a screen point: `rot` — view rotation (world → camera), `local` — offset
/// from the cube centre on screen in its half-sizes (y up). The cube is drawn orthographically.
pub fn cube_pick(rot: Mat3, local: Vec2, band: f32) -> Option<Zone> {
    let inv = rot.transpose();
    let r = Ray {
        origin: inv * Vec3::new(local.x, local.y, 4.0),
        dir: inv * Vec3::NEG_Z,
    };
    let t = ray_cube(&r)?;
    Some(cube_zone(r.at(t), band))
}

/// Direction to the eye for a zone (normalised).
pub fn zone_dir(z: Zone) -> Vec3 {
    Vec3::new(z[0] as f32, z[1] as f32, z[2] as f32).normalize_or(Vec3::Z)
}

/// Azimuth and pitch of a camera looking from direction `dir` (target to eye). For a view
/// straight down/up the azimuth is rounded to the nearest quarter turn from `yaw_now`.
pub fn orient(dir: Vec3, yaw_now: f32) -> (f32, f32) {
    let d = dir.normalize_or(Vec3::Z);
    if d.truncate().length() < 1e-4 {
        let q = FRAC_PI_2;
        let yaw = ((yaw_now + q) / q).round() * q - q;
        return (wrap_angle(yaw), PITCH_TOP * d.z.signum());
    }
    (d.y.atan2(d.x), d.z.asin().clamp(-PITCH_TOP, PITCH_TOP))
}

/// Part of a cube face (normal `face`) occupied by a zone: four corners in cube coordinates;
/// `None` — the zone does not touch this face.
pub fn zone_on_face(face: Zone, z: Zone, band: f32) -> Option<[Vec3; 4]> {
    let k = face.iter().position(|c| *c != 0)?;
    if z[k] != face[k] {
        return None;
    }
    let (u, v) = ((k + 1) % 3, (k + 2) % 3);
    let range = |c: i8| match c {
        -1 => (-1.0, -1.0 + band),
        1 => (1.0 - band, 1.0),
        _ => (-1.0 + band, 1.0 - band),
    };
    let ((u0, u1), (v0, v1)) = (range(z[u]), range(z[v]));
    let mut out = [Vec3::ZERO; 4];
    for (i, (a, b)) in [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
        .into_iter()
        .enumerate()
    {
        let mut p = Vec3::ZERO;
        p[k] = face[k] as f32;
        p[u] = a;
        p[v] = b;
        out[i] = p;
    }
    Some(out)
}

/// Step arrows around the view cube.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// To the face that lies that way on screen.
    Up,
    Down,
    Left,
    Right,
    /// A quarter turn about the vertical axis; in a top view the picture turns clockwise.
    TurnCw,
    TurnCcw,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Up,
        Step::Down,
        Step::Left,
        Step::Right,
        Step::TurnCw,
        Step::TurnCcw,
    ];
}

/// Face of the cube nearest to direction `v` (largest component wins, ties go to Z, then Y).
pub fn nearest_face(v: Vec3) -> Zone {
    let a = v.abs();
    let k = if a.z >= a.x && a.z >= a.y {
        2
    } else if a.y >= a.x {
        1
    } else {
        0
    };
    let mut f = [0i8; 3];
    f[k] = if v[k] >= 0.0 { 1 } else { -1 };
    f
}

/// The face the camera looks from when its direction `back` (target to eye) is within `tol`
/// radians of that face's normal; `None` for edge, corner and free views.
pub fn face_of(back: Vec3, tol: f32) -> Option<Zone> {
    let f = nearest_face(back);
    let ang = back.normalize_or(Vec3::Z).dot(zone_dir(f)).clamp(-1.0, 1.0).acos();
    (ang <= tol).then_some(f)
}

/// Yaw and pitch after a step arrow from a camera at `yaw`/`pitch`. Face steps orbit to the
/// face along the screen axis; turns add a quarter to the yaw and keep the pitch.
pub fn step_pose(yaw: f32, pitch: f32, s: Step) -> (f32, f32) {
    let q = FRAC_PI_2;
    let cam = OrbitCamera {
        yaw,
        pitch,
        ..Default::default()
    };
    let (r, u) = (cam.right_axis(), cam.up());
    let to = |v: Vec3| orient(zone_dir(nearest_face(v)), yaw);
    match s {
        Step::Up => to(u),
        Step::Down => to(-u),
        Step::Left => to(-r),
        Step::Right => to(r),
        Step::TurnCw => (wrap_angle(yaw + q), pitch),
        Step::TurnCcw => (wrap_angle(yaw - q), pitch),
    }
}

/// Intersection of two boxes (empty — `None`).
pub fn intersect(a: &Aabb, b: &Aabb) -> Option<Aabb> {
    let r = Aabb {
        min: a.min.max(b.min),
        max: a.max.min(b.max),
    };
    (r.min.x <= r.max.x && r.min.y <= r.max.y && r.min.z <= r.max.z).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> OrbitCamera {
        OrbitCamera {
            target: Vec3::new(3.0, -2.0, 1.0),
            ..Default::default()
        }
    }

    #[test]
    fn orbit_about_keeps_pivot_on_screen() {
        for ortho in [false, true] {
            let mut c = cam();
            c.ortho = ortho;
            let pivot = Vec3::new(5.0, 1.0, 0.5);
            let before = c.project(pivot, 1.4);
            for (dx, dy) in [(40.0, 0.0), (0.0, -25.0), (-70.0, 33.0)] {
                c.orbit_about(pivot, dx, dy);
                let after = c.project(pivot, 1.4);
                assert!(
                    (before.truncate() - after.truncate()).length() < 1e-3,
                    "{before:?} {after:?}"
                );
            }
            // rotation is a rigid body motion: the eye-to-pivot distance does not change
            let d0 = (cam().eye() - pivot).length();
            assert!(((c.eye() - pivot).length() - d0).abs() < 1e-3);
        }
    }

    #[test]
    fn orbit_about_at_pitch_limit_is_stable() {
        let mut c = cam();
        c.pitch = PITCH_MAX;
        let t = c.target;
        c.orbit_about(Vec3::ZERO, 0.0, 50.0);
        assert!((c.target - t).length() < 1e-5);
    }

    #[test]
    fn look_keeps_eye() {
        let mut c = cam();
        let e = c.eye();
        c.look(30.0, -12.0);
        assert!((c.eye() - e).length() < 1e-4);
    }

    #[test]
    fn walk_moves_horizontally_and_first_person_keeps_eye() {
        let mut c = cam();
        let e = c.eye();
        c.to_first_person(1.0);
        assert!((c.eye() - e).length() < 1e-4 && (c.distance - 1.0).abs() < 1e-6);
        let z = c.eye().z;
        c.walk(
            Move {
                fwd: 1.0,
                ..Default::default()
            },
            2.0,
            0.5,
            false,
        );
        assert!((c.eye().z - z).abs() < 1e-5);
        assert!(((c.eye() - e).length() - 1.0).abs() < 1e-4);
        let before = c.eye();
        c.walk(
            Move {
                fwd: 1.0,
                ..Default::default()
            },
            1.0,
            1.0,
            true,
        );
        assert!((c.eye() - before).dot(-c.back()) > 0.99);
        let big = Aabb {
            min: Vec3::ZERO,
            max: Vec3::new(1200.0, 500.0, 30.0),
        };
        let speed = walk_speed(Some(&big), true);
        assert!(speed <= MAX_SPEED && speed > 20.0);
        // walking — a human pace everywhere; multipliers do not exceed the limit
        assert_eq!(walk_speed(Some(&big), false), WALK_SPEED);
        assert_eq!(walk_speed(None, true), 1.5);
        assert_eq!(clamped_speed(30.0, 16.0), MAX_SPEED);
    }

    #[test]
    fn transition_ends_exactly_and_takes_short_way() {
        let a = Pose {
            target: Vec3::ZERO,
            yaw: 3.0,
            pitch: 0.2,
            distance: 1.0,
        };
        let b = Pose {
            target: Vec3::X,
            yaw: -3.0,
            pitch: 1.0,
            distance: 100.0,
        };
        let mut t = Transition::new(a, b, 0.3);
        let (mid, done) = t.step(0.15);
        assert!(!done);
        // the shortest path 3 → −3 goes through π, not through 0
        assert!(wrap_angle(mid.yaw).abs() > 3.0, "{}", mid.yaw);
        assert!((mid.distance - 10.0).abs() < 0.5, "{}", mid.distance);
        let (end, done) = t.step(1.0);
        assert!(done);
        assert_eq!(end, b);
    }

    #[test]
    fn zones_faces_edges_corners() {
        assert_eq!(cube_zone(Vec3::new(0.1, -0.2, 1.0), CUBE_BAND), [0, 0, 1]);
        assert_eq!(cube_zone(Vec3::new(0.9, -0.1, 1.0), CUBE_BAND), [1, 0, 1]);
        assert_eq!(
            cube_zone(Vec3::new(-0.9, -0.95, 1.0), CUBE_BAND),
            [-1, -1, 1]
        );
        // front view (looking along +Y): the screen centre is the "front" face (−Y)
        let mut c = OrbitCamera {
            yaw: -FRAC_PI_2,
            pitch: 0.0,
            ..Default::default()
        };
        let rot = Mat3::from_mat4(c.view());
        assert_eq!(cube_pick(rot, Vec2::ZERO, CUBE_BAND), Some([0, -1, 0]));
        assert_eq!(
            cube_pick(rot, Vec2::new(0.9, 0.0), CUBE_BAND),
            Some([1, -1, 0])
        );
        assert_eq!(
            cube_pick(rot, Vec2::new(0.9, 0.9), CUBE_BAND),
            Some([1, -1, 1])
        );
        assert_eq!(cube_pick(rot, Vec2::new(3.0, 0.0), CUBE_BAND), None);
        // clicking a zone sets the camera so that the zone is again under the cube centre
        for z in [[1, 0, 0], [0, 1, 1], [-1, 1, 1], [1, -1, -1]] {
            let (yaw, pitch) = orient(zone_dir(z), c.yaw);
            c.yaw = yaw;
            c.pitch = pitch;
            let rot = Mat3::from_mat4(c.view());
            assert_eq!(cube_pick(rot, Vec2::ZERO, CUBE_BAND), Some(z), "{z:?}");
        }
    }

    #[test]
    fn top_view_snaps_yaw_to_quarter() {
        let (yaw, pitch) = orient(Vec3::Z, -1.4);
        assert!((yaw + FRAC_PI_2).abs() < 1e-5 && pitch == PITCH_TOP);
        let (yaw, _) = orient(Vec3::Z, 0.2);
        assert!(yaw.abs() < 1e-5);
        let (_, pitch) = orient(Vec3::NEG_Z, 0.0);
        assert_eq!(pitch, -PITCH_TOP);
        // straight down in ortho: the box's side faces are not visible (their projection is a segment)
        let mut c = OrbitCamera {
            ortho: true,
            ..Default::default()
        };
        c.set_pose(&Pose {
            target: Vec3::ZERO,
            yaw: -FRAC_PI_2,
            pitch: PITCH_TOP,
            distance: 10.0,
        });
        let a = c.project(Vec3::new(1.0, 1.0, 0.0), 1.0);
        let b = c.project(Vec3::new(1.0, 1.0, 3.0), 1.0);
        assert!((a.truncate() - b.truncate()).length() < 1e-5, "{a:?} {b:?}");
    }

    fn back_of(yaw: f32, pitch: f32) -> Vec3 {
        OrbitCamera {
            yaw,
            pitch,
            ..Default::default()
        }
        .back()
    }

    #[test]
    fn face_of_only_for_face_views() {
        assert_eq!(face_of(Vec3::Z, 0.01), Some([0, 0, 1]));
        assert_eq!(face_of(back_of(-FRAC_PI_2, 0.0), 0.01), Some([0, -1, 0]));
        assert_eq!(face_of(Vec3::new(1.0, 1.0, 0.0), 0.05), None);
        assert_eq!(face_of(Vec3::new(1.0, -1.0, 1.0), 0.05), None);
        assert_eq!(face_of(Vec3::new(1.0, 0.02, 0.0), 0.05), Some([1, 0, 0]));
    }

    #[test]
    fn step_arrows_go_to_the_face_on_that_side_of_the_screen() {
        let front = (-FRAC_PI_2, 0.0);
        let go = |from: (f32, f32), s: Step| {
            let (y, p) = step_pose(from.0, from.1, s);
            face_of(back_of(y, p), 1e-3)
        };
        assert_eq!(go(front, Step::Up), Some([0, 0, 1]), "front, up: top");
        assert_eq!(go(front, Step::Down), Some([0, 0, -1]), "front, down: bottom");
        assert_eq!(go(front, Step::Right), Some([1, 0, 0]), "front, right: right");
        assert_eq!(go(front, Step::Left), Some([-1, 0, 0]), "front, left: left");
        // from the top the screen's up is the back of the model
        let top = (-FRAC_PI_2, PITCH_TOP);
        assert_eq!(go(top, Step::Up), Some([0, 1, 0]), "top, up: back");
        assert_eq!(go(top, Step::Down), Some([0, -1, 0]), "top, down: front");
        assert_eq!(go(top, Step::Right), Some([1, 0, 0]), "top, right: right");
        // stepping there and back returns to the start face
        for s in [(Step::Up, Step::Down), (Step::Left, Step::Right)] {
            let a = step_pose(front.0, front.1, s.0);
            let b = step_pose(a.0, a.1, s.1);
            assert_eq!(face_of(back_of(b.0, b.1), 1e-3), Some([0, -1, 0]), "{s:?}");
        }
        // a step from a top view lands on a side face with a level camera
        let (_, p) = step_pose(top.0, top.1, Step::Left);
        assert!(p.abs() < 1e-5);
    }

    #[test]
    fn turn_arrows_rotate_a_quarter_and_keep_the_pitch() {
        let top = OrbitCamera {
            yaw: -FRAC_PI_2,
            pitch: PITCH_TOP,
            ..Default::default()
        };
        let (y, p) = step_pose(top.yaw, top.pitch, Step::TurnCw);
        assert_eq!(p, PITCH_TOP);
        let turned = OrbitCamera {
            yaw: y,
            pitch: p,
            ..Default::default()
        };
        // clockwise on screen: what was up now lies to the right
        assert!((turned.right_axis() - top.up()).length() < 1e-4);
        let (y2, _) = step_pose(y, p, Step::TurnCcw);
        assert!(wrap_angle(y2 - top.yaw).abs() < 1e-5);
        // four turns come back
        let mut yaw = 0.3;
        for _ in 0..4 {
            yaw = step_pose(yaw, 0.4, Step::TurnCw).0;
        }
        assert!(wrap_angle(yaw - 0.3).abs() < 1e-4);
    }

    #[test]
    fn zone_patches_on_faces() {
        let top = [0, 0, 1];
        let p = zone_on_face(top, top, 0.4).unwrap();
        assert!(p.iter().all(|q| q.z == 1.0 && q.x.abs() <= 0.6 + 1e-6));
        let edge = zone_on_face(top, [1, 0, 1], 0.4).unwrap();
        assert!(edge.iter().all(|q| q.x >= 0.6 - 1e-6));
        assert!(zone_on_face(top, [1, 0, 0], 0.4).is_none());
        assert!(zone_on_face([1, 0, 0], [1, 0, 1], 0.4).is_some());
    }

    #[test]
    fn box_intersection() {
        let a = Aabb {
            min: Vec3::ZERO,
            max: Vec3::splat(2.0),
        };
        let b = Aabb {
            min: Vec3::splat(1.0),
            max: Vec3::splat(3.0),
        };
        assert_eq!(intersect(&a, &b).unwrap().min, Vec3::splat(1.0));
        let c = Aabb {
            min: Vec3::splat(5.0),
            max: Vec3::splat(6.0),
        };
        assert!(intersect(&a, &c).is_none());
    }
}
