// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View cube in the 3D corner: faces, edges and corners (26 directions) with the zone under the
//! cursor highlighted; a click smoothly moves to the view from that side and "zooms to fit"; dragging
//! the cube rotates the camera; the house icon is the home view (right click: set current / reset).
//! Around the cube is a compass ring: N/E/S/W labels are rotated by the site's `north_deg`,
//! dragging the ring rotates the camera around Z only. Projection is switched by the view-bar
//! button (`overlay.rs`); it is not duplicated here. Colors come from tokens only.

use super::Nav;
use crate::viewport::{inside_convex, Viewport};
use ddc_render::glam::{Mat3, Vec2, Vec3};
use ddc_render::nav::{self, Pose, Step, Zone, CUBE_BAND};
use ddc_render::Scene;
use egui::{self, Color32, CornerRadius, Pos2, Sense, Stroke};

/// Cube faces: normal and label key.
const FACES: [(Zone, &str); 6] = [
    ([0, 0, 1], "v3d.cube.top"),
    ([0, 0, -1], "v3d.cube.bottom"),
    ([0, -1, 0], "v3d.cube.front"),
    ([0, 1, 0], "v3d.cube.back"),
    ([1, 0, 0], "v3d.cube.right"),
    ([-1, 0, 0], "v3d.cube.left"),
];

const SIZE: f32 = 40.0;

struct Face {
    depth: f32,
    n: Zone,
    label: &'static str,
    pts: Vec<Pos2>,
}

fn zv(z: Zone) -> Vec3 {
    Vec3::new(z[0] as f32, z[1] as f32, z[2] as f32)
}

/// Compass ring: inner and outer screen radius around the cube centre.
const RING_IN: f32 = SIZE * 1.3;
const RING_OUT: f32 = SIZE * 1.62;
/// Farthest screen distance of a cube corner from the centre (cube scale `SIZE * 0.62`,
/// corner at `sqrt(3)` half-edges); it stays inside the ring so the two hit zones are disjoint.
const CUBE_R: f32 = SIZE * 0.62 * 1.732_051;
/// Alpha of the hover token over a face; labels keep contrast >= 3:1 (test).
const HOVER_ALPHA: f32 = 0.35;

/// Compass directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cardinal {
    N,
    E,
    S,
    W,
}

impl Cardinal {
    pub const ALL: [Cardinal; 4] = [Cardinal::N, Cardinal::E, Cardinal::S, Cardinal::W];

    /// Quarter turns counter-clockwise from north.
    fn quarters(self) -> f32 {
        match self {
            Cardinal::N => 0.0,
            Cardinal::W => 1.0,
            Cardinal::S => 2.0,
            Cardinal::E => -1.0,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Cardinal::N => "v3d.compass.n",
            Cardinal::E => "v3d.compass.e",
            Cardinal::S => "v3d.compass.s",
            Cardinal::W => "v3d.compass.w",
        }
    }
}

/// Model-plane angle of true north in radians, counter-clockwise from +X, in [0, 2pi).
/// `north_deg` is measured from +Y counter-clockwise (as in the plan view's north arrow).
pub fn north_angle(north_deg: f64) -> f32 {
    let d = if north_deg.is_finite() { north_deg } else { 0.0 };
    (90.0 + d).rem_euclid(360.0).to_radians() as f32
}

/// Model-plane angle of one compass direction, radians counter-clockwise from +X.
pub fn cardinal_angle(north_deg: f64, c: Cardinal) -> f32 {
    north_angle(north_deg) + c.quarters() * std::f32::consts::FRAC_PI_2
}

/// A compass mark: direction on screen (unit vector, y down) from the cube centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub card: Cardinal,
    pub dir: Vec2,
}

/// Screen directions of N/E/S/W for the view rotation `rot` (world to camera).
pub fn compass_ticks(north_deg: f64, rot: Mat3) -> [Tick; 4] {
    Cardinal::ALL.map(|card| {
        let a = cardinal_angle(north_deg, card);
        let r = rot * Vec3::new(a.cos(), a.sin(), 0.0);
        let dir = Vec2::new(r.x, -r.y).try_normalize().unwrap_or(Vec2::NEG_Y);
        Tick { card, dir }
    })
}

/// Position of a mark on a circle of `radius` around `c`.
pub fn tick_pos(c: Pos2, t: &Tick, radius: f32) -> Pos2 {
    c + egui::Vec2::new(t.dir.x, t.dir.y) * radius
}

/// True when a point at distance `d` from the cube centre is on the compass ring.
pub fn ring_hit(d: f32) -> bool {
    (RING_IN..=RING_OUT).contains(&d)
}

/// What a pointer press grabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Cube,
    Ring,
}

/// Cube wins when a face is under the pointer; otherwise the ring; otherwise nothing.
pub fn grab_at(dist: f32, on_cube: bool) -> Option<Grab> {
    if on_cube {
        Some(Grab::Cube)
    } else if ring_hit(dist) {
        Some(Grab::Ring)
    } else {
        None
    }
}

/// Camera yaw change for a pointer moved `from` -> `to` around `c` on the ring: the ring
/// follows the pointer (seen from above clockwise on screen is yaw up; from below it flips).
pub fn ring_yaw_delta(c: Pos2, from: Pos2, to: Pos2, below: bool) -> f32 {
    let a = |p: Pos2| (p.y - c.y).atan2(p.x - c.x);
    let mut d = a(to) - a(from);
    let pi = std::f32::consts::PI;
    if d > pi {
        d -= 2.0 * pi;
    } else if d < -pi {
        d += 2.0 * pi;
    }
    if below {
        -d
    } else {
        d
    }
}

/// View direction (target to eye) for the cube zone at `local` (half-edges from the centre,
/// y up) under view rotation `rot`.
pub fn view_dir_at(rot: Mat3, local: Vec2) -> Option<Vec3> {
    nav::cube_pick(rot, local, CUBE_BAND).map(nav::zone_dir)
}

/// Face colour for a depth (0 edge-on, 1 facing the viewer) from the cube face tokens.
/// Smallest label size a cube face may use; below it the label is dropped, not squeezed.
pub const FACE_LABEL_MIN_PX: f32 = 7.5;

/// Font size so a label `w` px wide at `base` px fits `room` px (with a margin), or None.
pub fn fit_label_px(base: f32, w: f32, room: f32) -> Option<f32> {
    let room = room * 0.86;
    if w <= room {
        return Some(base);
    }
    let px = base * room / w.max(1e-3);
    (px >= FACE_LABEL_MIN_PX).then_some(px)
}

/// Width of a projected face along the horizontal line through `mid` (its edge crossings).
pub fn face_span(pts: &[Pos2], mid: Pos2) -> f32 {
    let mut xs = vec![];
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        if (a.y - mid.y) * (b.y - mid.y) <= 0.0 && (a.y - b.y).abs() > 1e-6 {
            xs.push(a.x + (mid.y - a.y) / (b.y - a.y) * (b.x - a.x));
        }
    }
    match (xs.iter().copied().reduce(f32::min), xs.iter().copied().reduce(f32::max)) {
        (Some(lo), Some(hi)) => hi - lo,
        _ => 0.0,
    }
}

pub fn face_fill(lo: Color32, hi: Color32, depth: f32) -> Color32 {
    lo.lerp_to_gamma(hi, depth.clamp(0.0, 1.0))
}

/// Gap between the compass ring and the canvas edge.
pub const CUBE_MARGIN: f32 = 16.0;

/// Cube centre in the top-right corner of the canvas, ring clear of the edges by `CUBE_MARGIN`.
pub fn center(rect: egui::Rect) -> Pos2 {
    Pos2::new(
        rect.right() - RING_OUT - CUBE_MARGIN,
        rect.top() + RING_OUT + CUBE_MARGIN,
    )
}

/// Distance of the face-step arrows from the cube centre (on the rim of the compass ring,
/// so the top and right ones stay inside the canvas margin).
const ARROW_R: f32 = RING_OUT + 2.0;
/// Offset of the turn arrows along both screen axes (upper corners, clear of the ring).
const TURN_XY: f32 = 54.0;
/// Hit square of one arrow, points.
pub const ARROW_HIT: f32 = 18.0;
/// The arrows show while the pointer is this close to the cube centre.
const ARROW_NEAR: f32 = RING_OUT * 1.5;
/// A view counts as a face view within this angle of the face normal, radians.
pub const FACE_TOL: f32 = 0.02;

/// Screen place of a step arrow around the cube centre `c`.
pub fn arrow_at(c: Pos2, s: Step) -> Pos2 {
    let (r, t) = (ARROW_R, TURN_XY);
    c + match s {
        Step::Up => egui::vec2(0.0, -r),
        Step::Down => egui::vec2(0.0, r),
        Step::Left => egui::vec2(-r, 0.0),
        Step::Right => egui::vec2(r, 0.0),
        Step::TurnCw => egui::vec2(t, -t),
        Step::TurnCcw => egui::vec2(-t, -t),
    }
}

/// Tooltip key of a step arrow.
pub fn arrow_tip(s: Step) -> &'static str {
    match s {
        Step::Up => "view_cube.cube.up",
        Step::Down => "view_cube.cube.down",
        Step::Left => "view_cube.cube.left",
        Step::Right => "view_cube.cube.right",
        Step::TurnCw => "view_cube.cube.cw",
        Step::TurnCcw => "view_cube.cube.ccw",
    }
}

/// Arrows on offer: turns always, face steps only while looking straight at a face.
pub fn arrows_for(back: Vec3) -> Vec<Step> {
    let face = nav::face_of(back, FACE_TOL).is_some();
    Step::ALL
        .into_iter()
        .filter(|s| face || matches!(s, Step::TurnCw | Step::TurnCcw))
        .collect()
}

/// Pose after a step arrow: same target and distance, new direction.
pub fn step_target(p: Pose, s: Step) -> Pose {
    let (yaw, pitch) = nav::step_pose(p.yaw, p.pitch, s);
    Pose { yaw, pitch, ..p }
}

/// Rows of the cube's right-click menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAct {
    Persp,
    Ortho,
    Home,
    Fit,
    SetHome,
    ResetHome,
}

fn paint_arrow(p: &egui::Painter, at: Pos2, s: Step, ink: Color32, bg: Color32) {
    p.circle_filled(at, ARROW_HIT * 0.5, bg);
    let dir = match s {
        Step::Up => egui::vec2(0.0, -1.0),
        Step::Down => egui::vec2(0.0, 1.0),
        Step::Left => egui::vec2(-1.0, 0.0),
        Step::Right => egui::vec2(1.0, 0.0),
        Step::TurnCw | Step::TurnCcw => {
            // a 3/4 arc with a head at its end, turning the way of the arrow
            let cw = s == Step::TurnCw;
            let (r, n) = (5.0, 14);
            let a0 = -std::f32::consts::FRAC_PI_2;
            let sweep = if cw { 1.5 } else { -1.5 } * std::f32::consts::PI;
            let pts: Vec<Pos2> = (0..=n)
                .map(|i| {
                    let a = a0 + sweep * i as f32 / n as f32;
                    at + egui::vec2(a.cos(), a.sin()) * r
                })
                .collect();
            let end = *pts.last().unwrap_or(&at);
            let a1 = a0 + sweep;
            let tang = egui::vec2(-a1.sin(), a1.cos()) * if cw { 1.0 } else { -1.0 };
            let norm = egui::vec2(-tang.y, tang.x);
            p.add(egui::Shape::line(pts, Stroke::new(1.5, ink)));
            p.add(egui::Shape::convex_polygon(
                vec![end + tang * 3.5, end + norm * 3.0, end - norm * 3.0],
                ink,
                Stroke::NONE,
            ));
            return;
        }
    };
    let n = egui::vec2(-dir.y, dir.x);
    p.add(egui::Shape::convex_polygon(
        vec![at + dir * 5.0, at - dir * 3.0 + n * 5.0, at - dir * 3.0 - n * 5.0],
        ink,
        Stroke::NONE,
    ));
}

/// Draws the cube and handles input; true — the cursor is over the cube or its buttons (the click
/// does not go to model picking).
pub fn show(
    v: &mut Viewport,
    ui: &mut egui::Ui,
    rect: egui::Rect,
    scene: &Scene,
    aspect: f32,
) -> bool {
    let c = center(rect);
    let rot = Mat3::from_mat4(v.camera.view());
    let scale = SIZE * 0.62;
    let proj = |p: Vec3| {
        let r = rot * p;
        (c + egui::Vec2::new(r.x, -r.y) * scale, r.z)
    };
    let th = ddc_ui::theme::of(ui.ctx());
    let tone = ddc_ui::theme::sem(v.dark);
    let (ink, edge, hot) = (tone.cube_ink, tone.cube_edge, tone.hover);
    let cube_id = ui.id().with("viewcube");
    let resp = ui.interact(
        egui::Rect::from_center_size(c, egui::Vec2::splat(RING_OUT * 2.0)),
        cube_id,
        Sense::click_and_drag(),
    );
    let hover = resp.hover_pos();
    let p = ui.painter_at(rect);
    // soft elevation shadow under the cube
    p.add(
        th.shadow(2).as_shape(
            egui::Rect::from_center_size(c, egui::Vec2::splat(CUBE_R * 1.5)),
            CornerRadius::same(255),
        ),
    );

    // visible faces (normal towards the viewer), far ones first
    let mut faces: Vec<Face> = FACES
        .iter()
        .filter_map(|&(n, label)| {
            let nz = zv(n);
            let depth = (rot * nz).z;
            if depth <= 0.05 {
                return None;
            }
            let (u, w) = if nz.z.abs() > 0.5 {
                (Vec3::X, Vec3::Y)
            } else {
                (nz.cross(Vec3::Z), Vec3::Z)
            };
            let pts = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(a, b)| proj(nz + u * a + w * b).0)
                .to_vec();
            Some(Face {
                depth,
                n,
                label,
                pts,
            })
        })
        .collect();
    faces.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    let on_cube = hover.is_some_and(|h| faces.iter().any(|f| inside_convex(h, &f.pts)));
    // what the pointer grabs: fixed at press for the whole drag
    let grab_id = cube_id.with("grab");
    let now = hover.and_then(|h| grab_at((h - c).length(), on_cube));
    if resp.drag_started() {
        ui.data_mut(|d| d.insert_temp(grab_id, now));
    }
    let grab: Option<Grab> = if resp.dragged() {
        ui.data(|d| d.get_temp::<Option<Grab>>(grab_id)).flatten()
    } else {
        now
    };
    let zone = hover
        .filter(|_| on_cube && !resp.dragged())
        .and_then(|h| {
            let local = Vec2::new((h.x - c.x) / scale, -(h.y - c.y) / scale);
            nav::cube_pick(rot, local, CUBE_BAND)
        });

    if let Some(i) = crate::nav_cursor::cursor_for(
        crate::nav_cursor::Drag::None,
        false,
        false,
        zone.is_some(),
    ) {
        ui.ctx().set_cursor_icon(i);
    }

    // compass ring: bezel, rim lines, N/E/S/W marks turned by the site's north
    let ring_mid = (RING_IN + RING_OUT) * 0.5;
    let ring_hot = grab == Some(Grab::Ring);
    p.circle_stroke(
        c,
        ring_mid,
        Stroke::new(
            RING_OUT - RING_IN,
            tone.cube_face_lo.gamma_multiply(if ring_hot { 0.55 } else { 0.8 }),
        ),
    );
    if ring_hot {
        p.circle_stroke(
            c,
            ring_mid,
            Stroke::new(RING_OUT - RING_IN, hot.gamma_multiply(HOVER_ALPHA)),
        );
    }
    p.circle_stroke(c, RING_IN, Stroke::new(1.0, edge));
    p.circle_stroke(c, RING_OUT, Stroke::new(1.0, edge));
    for t in compass_ticks(v.nav.north_deg, rot) {
        let north = t.card == Cardinal::N;
        let at = tick_pos(c, &t, ring_mid);
        if north {
            // needle-head just outside the label, pointing out of the ring
            let d = egui::Vec2::new(t.dir.x, t.dir.y);
            let n = egui::Vec2::new(-d.y, d.x);
            let base = c + d * (RING_OUT - 2.0);
            p.add(egui::Shape::convex_polygon(
                vec![base + d * 5.0, base + n * 3.5, base - n * 3.5],
                tone.accent,
                Stroke::NONE,
            ));
        }
        p.text(
            at,
            egui::Align2::CENTER_CENTER,
            v.nav.t(t.card.label_key()),
            ddc_ui::typo::font(if north {
                ddc_ui::theme::Role::CanvasTag
            } else {
                ddc_ui::theme::Role::CanvasMicro
            }),
            ink,
        );
    }

    for f in &faces {
        let fill = face_fill(tone.cube_face_lo, tone.cube_face_hi, f.depth);
        p.add(egui::Shape::convex_polygon(
            f.pts.clone(),
            fill,
            Stroke::new(1.0, edge),
        ));
        if let Some(z) = zone {
            if let Some(q) = nav::zone_on_face(f.n, z, CUBE_BAND) {
                let pts = q.map(|x| proj(x).0).to_vec();
                p.add(egui::Shape::convex_polygon(
                    pts,
                    hot.gamma_multiply(HOVER_ALPHA),
                    Stroke::NONE,
                ));
            }
        }
    }
    for f in &faces {
        if f.depth > 0.35 {
            let mid = f.pts.iter().fold(egui::Vec2::ZERO, |a, q| a + q.to_vec2()) / 4.0;
            let base = ddc_ui::typo::font(ddc_ui::theme::Role::CanvasMicro);
            let text = v.nav.t(f.label).to_owned();
            let w = p.layout_no_wrap(text.clone(), base.clone(), ink).size().x;
            // the label shrinks to its face (a turned face is narrow) or is left out
            if let Some(px) = fit_label_px(base.size, w, face_span(&f.pts, mid.to_pos2())) {
                p.text(
                    mid.to_pos2(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::new(px, base.family.clone()),
                    ink,
                );
            }
        }
    }
    // axes X/Y/Z from the cube corner, along its edges
    for (axis, col, l) in [
        (Vec3::X, tone.axis_x, "X"),
        (Vec3::Y, tone.axis_y, "Y"),
        (Vec3::Z, tone.axis_z, "Z"),
    ] {
        let o = proj(Vec3::splat(-1.0)).0;
        let e = proj(Vec3::splat(-1.0) + axis * 2.0).0;
        p.line_segment([o, e], Stroke::new(1.6, col));
        let tip = proj(Vec3::splat(-1.0) + axis * 2.35).0;
        p.text(
            tip,
            egui::Align2::CENTER_CENTER,
            l,
            ddc_ui::typo::font(ddc_ui::theme::Role::CanvasTag),
            col,
        );
    }

    // home icon
    let home = egui::Rect::from_center_size(
        c + egui::Vec2::new(-SIZE * 1.45, SIZE * 1.45),
        egui::Vec2::splat(th.d().control + 4.0),
    );
    let home_tip = v.nav.t("v3d.home.tip").to_owned();
    let home_resp = ui
        .scope_builder(egui::UiBuilder::new().max_rect(home), |ui| {
            ddc_ui::widgets::glyph_button(ui, "home-3d", true, &home_tip)
        })
        .inner;
    let set_home = v.nav.t("v3d.home.set").to_owned();
    let reset_home = v.nav.t("v3d.home.reset").to_owned();
    let mut home_cmd = None;
    home_resp.context_menu(|ui| {
        let row = |ui: &mut egui::Ui, label: &str| {
            ddc_ui::widgets::menu_item(ui, &th, label, "", ddc_ui::widgets::MenuTone::Normal, true)
                .clicked()
        };
        if row(ui, &set_home) {
            home_cmd = Some(true);
            ui.close();
        }
        if row(ui, &reset_home) {
            home_cmd = Some(false);
            ui.close();
        }
    });
    match home_cmd {
        Some(true) => v.nav.home = Some((v.camera.pose(), v.camera.ortho)),
        Some(false) => v.nav.home = None,
        None => {}
    }
    let extents = Nav::extents(scene.focus.or(scene.bounds), v.section_box);
    if home_resp.clicked() {
        v.nav.go_home(&mut v.camera, extents, aspect);
    }
    // orbit around the model centre, and the automatic orbit for presentations
    let mut orbit_hot = false;
    for (k, (icon, key)) in [("layout-focus", "cmd.view.orbit_center"), ("rotate", "cmd.view.orbit_auto")].into_iter().enumerate() {
        let on = if k == 0 { v.nav.center_orbit } else { v.nav.auto_orbit };
        let r = egui::Rect::from_center_size(
            home.center() - egui::Vec2::new((th.d().control + 6.0) * (k as f32 + 1.0), 0.0),
            home.size(),
        );
        let tip = v.nav.t(key).to_owned();
        let resp = ui
            .scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                if on {
                    let bg = ui.visuals().selection.bg_fill;
                    ui.painter().rect_filled(r, 4.0, bg);
                }
                ddc_ui::widgets::glyph_button(ui, icon, true, &tip)
            })
            .inner;
        orbit_hot |= resp.hovered();
        if resp.clicked() {
            if k == 0 {
                v.nav.center_orbit = !v.nav.center_orbit;
            } else {
                v.nav.auto_orbit = !v.nav.auto_orbit;
                v.nav.walk = None;
            }
        }
    }

    // step arrows: shown while the pointer is near the cube, never during a drag
    let near = ui
        .input(|i| i.pointer.hover_pos())
        .is_some_and(|h| rect.contains(h) && (h - c).length() <= ARROW_NEAR);
    let mut arrow_hot = false;
    let mut step = None;
    if near && !resp.dragged() {
        for s in arrows_for(v.camera.back()) {
            let at = arrow_at(c, s);
            let hit = egui::Rect::from_center_size(at, egui::Vec2::splat(ARROW_HIT));
            let r = ui.interact(hit, cube_id.with(arrow_tip(s)), Sense::click());
            let tip = v.nav.t(arrow_tip(s)).to_owned();
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip.clone()));
            let bg = if r.hovered() {
                tone.cube_face_lo.lerp_to_gamma(hot.to_opaque(), HOVER_ALPHA)
            } else {
                tone.cube_face_lo
            };
            paint_arrow(&p, at, s, ink, bg);
            arrow_hot |= r.hovered();
            if r.on_hover_text(tip).clicked() {
                step = Some(s);
            }
        }
    }
    if let Some(s) = step {
        v.nav.walk = None;
        let to = step_target(v.camera.pose(), s);
        v.nav.go(&v.camera, to);
    }

    // drag: cube orbits the view centre, ring turns around Z only
    if resp.dragged() {
        let d = resp.drag_delta();
        match grab {
            Some(Grab::Cube) => {
                v.nav.anim = None;
                v.camera.orbit(d.x, d.y);
            }
            Some(Grab::Ring) => {
                if let Some(to) = resp.interact_pointer_pos() {
                    v.nav.anim = None;
                    v.camera.yaw += ring_yaw_delta(c, to - d, to, v.camera.pitch < 0.0);
                }
            }
            None => {}
        }
    }
    let resp = if grab == Some(Grab::Ring) {
        resp.on_hover_text(v.nav.t("v3d.compass.tip"))
    } else {
        resp.on_hover_text(v.nav.t("v3d.cube.tip"))
    };
    // right click: projection (the view bar's switch), home and fit
    let persp = !v.camera.ortho;
    let rows: Vec<(MenuAct, String, bool, bool)> = [
        (MenuAct::Persp, "view_cube.cube.persp", persp, true),
        (MenuAct::Ortho, "view_cube.cube.ortho", !persp, true),
        (MenuAct::Home, "view_cube.cube.home", false, true),
        (MenuAct::Fit, "view_cube.cube.fit", false, extents.is_some()),
        (MenuAct::SetHome, "v3d.home.set", false, true),
        (MenuAct::ResetHome, "v3d.home.reset", false, v.nav.home.is_some()),
    ]
    .into_iter()
    .map(|(a, k, on, en)| (a, v.nav.t(k).to_owned(), on, en))
    .collect();
    let mut menu = None;
    resp.context_menu(|ui| {
        for (i, (act, label, on, enabled)) in rows.iter().enumerate() {
            if i == 2 || i == 4 {
                ddc_ui::widgets::menu_divider(ui, &th);
            }
            let icon = on.then_some("check");
            let tone = ddc_ui::widgets::MenuTone::Normal;
            if ddc_ui::widgets::menu_item_icon(ui, &th, icon, label, "", tone, *enabled).clicked() {
                menu = Some(*act);
                ui.close();
            }
        }
    });
    match menu {
        Some(MenuAct::Persp) => super::overlay::set_ortho(v, false),
        Some(MenuAct::Ortho) => super::overlay::set_ortho(v, true),
        Some(MenuAct::Home) => v.nav.go_home(&mut v.camera, extents, aspect),
        Some(MenuAct::Fit) => {
            v.nav.walk = None;
            v.nav.fit_animated(&mut v.camera, extents, aspect);
        }
        Some(MenuAct::SetHome) => v.nav.home = Some((v.camera.pose(), v.camera.ortho)),
        Some(MenuAct::ResetHome) => v.nav.home = None,
        None => {}
    }
    if resp.clicked() && on_cube {
        let dir = hover.and_then(|h| {
            view_dir_at(rot, Vec2::new((h.x - c.x) / scale, -(h.y - c.y) / scale))
        });
        if let Some(dir) = dir {
            v.nav.walk = None;
            v.nav.view_from(&mut v.camera, dir, extents, aspect);
        }
    }
    grab.is_some() || resp.dragged() || home_resp.hovered() || orbit_hot || arrow_hot
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_render::OrbitCamera;
    use ddc_ui::theme::contrast;
    use std::f32::consts::{FRAC_PI_2, PI};

    #[test]
    fn face_labels_shrink_to_their_face_or_drop() {
        // fits: unchanged
        assert_eq!(fit_label_px(10.0, 30.0, 60.0), Some(10.0));
        // a bit wide: shrinks so the text fits the face with a margin
        let px = fit_label_px(10.0, 50.0, 48.0).unwrap();
        assert!(px < 10.0 && 50.0 * px / 10.0 <= 48.0 * 0.86 + 1e-3, "{px}");
        // far too wide (edge-on face): left out
        assert_eq!(fit_label_px(10.0, 50.0, 10.0), None);
        // span of an axis-aligned square through its centre
        let sq = [
            Pos2::new(0.0, 0.0),
            Pos2::new(40.0, 0.0),
            Pos2::new(40.0, 20.0),
            Pos2::new(0.0, 20.0),
        ];
        assert!((face_span(&sq, Pos2::new(20.0, 10.0)) - 40.0).abs() < 1e-4);
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Angles equal modulo a full turn.
    fn same_angle(a: f32, b: f32) -> bool {
        near((a - b).sin(), 0.0) && (a - b).cos() > 0.0
    }

    fn rot_of(yaw: f32, pitch: f32) -> Mat3 {
        let cam = OrbitCamera {
            yaw,
            pitch,
            ..OrbitCamera::default()
        };
        Mat3::from_mat4(cam.view())
    }

    fn tick(ts: &[Tick; 4], c: Cardinal) -> Vec2 {
        ts.iter().find(|t| t.card == c).unwrap().dir
    }

    /// All 26 zones of the cube.
    fn zones() -> Vec<Zone> {
        let mut v = vec![];
        for x in -1..=1i8 {
            for y in -1..=1i8 {
                for z in -1..=1i8 {
                    if [x, y, z] != [0, 0, 0] {
                        v.push([x, y, z]);
                    }
                }
            }
        }
        v
    }

    /// Screen offset (half-edges, y up) of the middle of zone `z` on its most visible face.
    fn zone_mid(rot: Mat3, z: Zone) -> Option<Vec2> {
        let face = (0..3)
            .filter(|&k| z[k] != 0)
            .map(|k| {
                let mut f = [0i8; 3];
                f[k] = z[k];
                f
            })
            .max_by(|a, b| (rot * zv(*a)).z.total_cmp(&(rot * zv(*b)).z))?;
        if (rot * zv(face)).z < 0.15 {
            return None;
        }
        let q = nav::zone_on_face(face, z, CUBE_BAND)?;
        let mid = q.iter().fold(Vec3::ZERO, |a, p| a + *p) / 4.0;
        Some((rot * mid).truncate())
    }

    #[test]
    fn cube_pick_of_every_zone_gives_the_camera_of_that_zone() {
        let b = ddc_render::Aabb {
            min: Vec3::new(-4.0, -3.0, 0.0),
            max: Vec3::new(6.0, 5.0, 9.0),
        };
        let starts = [
            (-2.2, 0.6),
            (0.7, 0.5),
            (2.6, -0.4),
            (-FRAC_PI_2, 0.0),
            (-FRAC_PI_2, nav::PITCH_TOP),
            (0.0, -nav::PITCH_TOP),
            (PI * 0.25, 0.61),
        ];
        for (yaw, pitch) in starts {
            for ortho in [false, true] {
                let start = OrbitCamera {
                    yaw,
                    pitch,
                    ortho,
                    ..OrbitCamera::default()
                };
                let rot = Mat3::from_mat4(start.view());
                let mut tried = 0;
                for z in zones() {
                    let Some(local) = zone_mid(rot, z) else {
                        continue;
                    };
                    tried += 1;
                    let tag = format!("start ({yaw}, {pitch}) ortho={ortho} zone {z:?}");
                    assert_eq!(nav::cube_pick(rot, local, CUBE_BAND), Some(z), "{tag}");
                    let dir = view_dir_at(rot, local).expect("a zone is under the point");
                    let mut cam = start.clone();
                    let mut n = Nav::default();
                    n.view_from(&mut cam, dir, Some(b), 1.5);
                    let (pose, done) = n.anim.take().expect("an animated move").step(10.0);
                    assert!(done, "{tag}");
                    cam.set_pose(&pose);
                    assert!(
                        (cam.back() - nav::zone_dir(z)).length() < 1e-3,
                        "{tag}: looks from {:?}",
                        cam.back()
                    );
                    assert_eq!(cam.ortho, ortho, "{tag}: the projection stays");
                    // the whole model is framed: its centre is the target
                    assert!((cam.target - (b.min + b.max) * 0.5).length() < 1e-3, "{tag}");
                }
                assert!(tried >= 9, "({yaw}, {pitch}): only {tried} zones visible");
            }
        }
    }

    #[test]
    fn step_arrows_show_by_view_and_move_the_camera() {
        let front = OrbitCamera {
            yaw: -FRAC_PI_2,
            pitch: 0.0,
            ..OrbitCamera::default()
        };
        assert_eq!(arrows_for(front.back()).len(), 6, "a face view gets all arrows");
        assert_eq!(
            arrows_for(OrbitCamera::default().back()),
            vec![Step::TurnCw, Step::TurnCcw],
            "a free view only turns"
        );
        let up = step_target(front.pose(), Step::Up);
        assert_eq!((up.target, up.distance), (front.target, front.distance));
        let mut cam = front.clone();
        cam.set_pose(&up);
        assert_eq!(nav::face_of(cam.back(), FACE_TOL), Some([0, 0, 1]));
        let right = step_target(front.pose(), Step::Right);
        cam.set_pose(&right);
        assert_eq!(nav::face_of(cam.back(), FACE_TOL), Some([1, 0, 0]));
    }

    #[test]
    fn arrows_stay_on_the_canvas_apart_from_ring_home_and_each_other() {
        let r = egui::Rect::from_min_size(Pos2::new(0.0, 0.0), egui::Vec2::new(420.0, 300.0));
        let c = center(r);
        let home = c + egui::Vec2::new(-SIZE * 1.45, SIZE * 1.45);
        let boxes: Vec<egui::Rect> = Step::ALL
            .iter()
            .map(|s| egui::Rect::from_center_size(arrow_at(c, *s), egui::Vec2::splat(ARROW_HIT)))
            .collect();
        for (i, a) in boxes.iter().enumerate() {
            assert!(r.contains_rect(*a), "{:?} leaves the canvas", Step::ALL[i]);
            let d = (a.center() - c).length();
            assert!(d - ARROW_HIT * 0.5 > CUBE_R, "{:?} covers the cube", Step::ALL[i]);
            assert!((a.center() - home).length() > ARROW_HIT, "{:?} on home", Step::ALL[i]);
            for b in &boxes[i + 1..] {
                assert!(!a.intersects(*b), "{a:?} {b:?}");
            }
            assert!(d <= ARROW_NEAR, "{:?} shows only near the cube", Step::ALL[i]);
        }
    }

    #[test]
    fn arrow_and_menu_strings_exist_in_both_languages() {
        let (en, ru) = (
            ddc_ui::Catalog::new(ddc_ui::Lang::En),
            ddc_ui::Catalog::new(ddc_ui::Lang::Ru),
        );
        let menu = [
            "view_cube.cube.persp",
            "view_cube.cube.ortho",
            "view_cube.cube.home",
            "view_cube.cube.fit",
        ];
        for k in Step::ALL.map(arrow_tip).into_iter().chain(menu) {
            assert!(super::super::LABEL_KEYS.contains(&k), "{k}");
            assert_ne!(en.t(k), k);
            assert_ne!(ru.t(k), k);
            assert_ne!(en.t(k), ru.t(k), "{k}");
        }
    }

    #[test]
    fn ring_keeps_the_canvas_margin() {
        let r = egui::Rect::from_min_size(Pos2::new(10.0, 20.0), egui::Vec2::new(800.0, 600.0));
        let c = center(r);
        assert_eq!(r.right() - (c.x + RING_OUT), CUBE_MARGIN);
        assert_eq!((c.y - RING_OUT) - r.top(), CUBE_MARGIN);
    }

    #[test]
    fn north_angle_for_zero_quarter_and_wrapped_values() {
        assert!(near(north_angle(0.0), FRAC_PI_2));
        assert!(near(north_angle(90.0), PI));
        assert!(near(north_angle(-90.0), 0.0));
        assert!(near(north_angle(360.0), FRAC_PI_2));
        assert!(near(north_angle(-450.0), 0.0));
        assert!(near(north_angle(f64::NAN), FRAC_PI_2));
        let a = north_angle(359.0);
        assert!((0.0..2.0 * PI).contains(&a));
        assert!(same_angle(a, FRAC_PI_2 - 1.0_f32.to_radians()));
        assert!(same_angle(north_angle(-1.0), a));
    }

    #[test]
    fn cardinals_are_a_quarter_turn_apart() {
        for deg in [0.0, 90.0, 359.0, -30.0] {
            let n = cardinal_angle(deg, Cardinal::N);
            assert!(same_angle(cardinal_angle(deg, Cardinal::W), n + FRAC_PI_2));
            assert!(same_angle(cardinal_angle(deg, Cardinal::S), n + PI));
            assert!(same_angle(cardinal_angle(deg, Cardinal::E), n - FRAC_PI_2));
        }
        // north 0: N is +Y and E is +X in the model plane
        assert!(same_angle(cardinal_angle(0.0, Cardinal::E), 0.0));
    }

    #[test]
    fn top_view_places_north_by_site_angle() {
        let top = Mat3::IDENTITY; // x right, y up on screen
        let t0 = compass_ticks(0.0, top);
        assert!((tick(&t0, Cardinal::N) - Vec2::NEG_Y).length() < 1e-4);
        assert!((tick(&t0, Cardinal::E) - Vec2::X).length() < 1e-4);
        assert!((tick(&t0, Cardinal::S) - Vec2::Y).length() < 1e-4);
        assert!((tick(&t0, Cardinal::W) - Vec2::NEG_X).length() < 1e-4);
        // north 90 deg counter-clockwise from +Y points along -X
        let t90 = compass_ticks(90.0, top);
        assert!((tick(&t90, Cardinal::N) - Vec2::NEG_X).length() < 1e-4);
        let tm = compass_ticks(-90.0, top);
        assert!((tick(&tm, Cardinal::N) - Vec2::X).length() < 1e-4);
        // 359 is one degree clockwise of straight up: slightly right of the top
        let t359 = compass_ticks(359.0, top);
        let n = tick(&t359, Cardinal::N);
        assert!(n.x > 0.0 && n.y < 0.0 && n.x < 0.05);
    }

    #[test]
    fn north_label_sits_on_the_ring_at_its_direction() {
        let c = Pos2::new(100.0, 100.0);
        let ts = compass_ticks(0.0, Mat3::IDENTITY);
        let n = ts.iter().find(|t| t.card == Cardinal::N).unwrap();
        let at = tick_pos(c, n, 50.0);
        assert!(near(at.x, 100.0) && near(at.y, 50.0));
        for t in compass_ticks(37.0, rot_of(-2.2, 0.6)) {
            assert!(near(t.dir.length(), 1.0));
        }
    }

    #[test]
    fn camera_yaw_turns_the_marks_with_the_scene() {
        // front view (eye at -Y), pitched down: north (+Y) is up, east (+X) is right
        let ts = compass_ticks(0.0, rot_of(-FRAC_PI_2, 0.6));
        assert!(tick(&ts, Cardinal::N).y < 0.0 && tick(&ts, Cardinal::S).y > 0.0);
        assert!(tick(&ts, Cardinal::E).x > 0.0 && tick(&ts, Cardinal::W).x < 0.0);
        // a horizon-level view never yields a NaN mark
        for t in compass_ticks(0.0, rot_of(-FRAC_PI_2, 0.0)) {
            assert!(t.dir.is_finite() && near(t.dir.length(), 1.0));
        }
    }

    #[test]
    fn ring_and_cube_hit_zones_are_disjoint() {
        assert!(CUBE_R < RING_IN && RING_IN < RING_OUT);
        for i in 0..=200 {
            let d = i as f32 * 0.5;
            assert_eq!(grab_at(d, true), Some(Grab::Cube));
            let g = grab_at(d, false);
            assert_eq!(g.is_some(), ring_hit(d), "d = {d}");
            if d <= CUBE_R {
                assert_eq!(g, None, "inside the cube footprint nothing is a ring");
            }
        }
        assert_eq!(grab_at((RING_IN + RING_OUT) * 0.5, false), Some(Grab::Ring));
        assert_eq!(grab_at(RING_OUT + 1.0, false), None);
    }

    #[test]
    fn ring_drag_turns_yaw_around_the_pointer_angle() {
        let c = Pos2::new(0.0, 0.0);
        let right = Pos2::new(10.0, 0.0);
        let down = Pos2::new(0.0, 10.0);
        assert!(near(ring_yaw_delta(c, right, down, false), FRAC_PI_2));
        assert!(near(ring_yaw_delta(c, down, right, false), -FRAC_PI_2));
        assert!(near(ring_yaw_delta(c, right, down, true), -FRAC_PI_2));
        assert!(near(ring_yaw_delta(c, right, right, false), 0.0));
        // across the +-180 degree seam the step stays small
        let a = Pos2::new(-10.0, -1.0);
        let b = Pos2::new(-10.0, 1.0);
        assert!(ring_yaw_delta(c, a, b, false).abs() < 0.3);
    }

    #[test]
    fn face_hit_gives_the_view_direction() {
        let front = rot_of(-FRAC_PI_2, 0.0);
        let d = view_dir_at(front, Vec2::ZERO).unwrap();
        assert!((d - Vec3::NEG_Y).length() < 1e-4);
        let edge = view_dir_at(front, Vec2::new(0.95, 0.0)).unwrap();
        assert!(edge.x > 0.5 && edge.y < -0.5 && edge.z.abs() < 1e-4);
        let top = rot_of(-FRAC_PI_2, FRAC_PI_2);
        let d = view_dir_at(top, Vec2::ZERO).unwrap();
        assert!((d - Vec3::Z).length() < 1e-3);
        assert!(view_dir_at(front, Vec2::new(3.0, 3.0)).is_none());
    }

    #[test]
    fn cube_tokens_exist_in_both_themes_with_readable_labels() {
        for dark in [false, true] {
            let t = ddc_ui::theme::sem(dark);
            for face in [t.cube_face_lo, t.cube_face_hi] {
                assert!(contrast(t.cube_ink, face) >= 3.0, "dark={dark} label");
                let hovered = face.lerp_to_gamma(t.hover.to_opaque(), HOVER_ALPHA);
                assert!(contrast(t.cube_ink, hovered) >= 3.0, "dark={dark} hovered label");
            }
            assert_ne!(t.hover, t.cube_face_lo);
            assert_ne!(t.cube_edge, t.cube_face_hi);
            assert_eq!(face_fill(t.cube_face_lo, t.cube_face_hi, 0.0), t.cube_face_lo);
            assert_eq!(face_fill(t.cube_face_lo, t.cube_face_hi, 1.0), t.cube_face_hi);
            assert_eq!(face_fill(t.cube_face_lo, t.cube_face_hi, 5.0), t.cube_face_hi);
        }
        let (l, d) = (ddc_ui::theme::sem(false), ddc_ui::theme::sem(true));
        assert_ne!(l.cube_face_hi, d.cube_face_hi);
        assert_ne!(l.cube_ink, d.cube_ink);
    }

    #[test]
    fn compass_and_home_strings_exist_in_both_languages() {
        let (en, ru) = (
            ddc_ui::Catalog::new(ddc_ui::Lang::En),
            ddc_ui::Catalog::new(ddc_ui::Lang::Ru),
        );
        for c in Cardinal::ALL {
            let k = c.label_key();
            assert!(super::super::LABEL_KEYS.contains(&k), "{k}");
            assert_ne!(en.t(k), k);
            assert_ne!(ru.t(k), k);
            assert_ne!(en.t(k), ru.t(k), "{k}");
        }
        for k in ["v3d.home.tip", "v3d.compass.tip"] {
            assert_ne!(en.t(k), k);
            assert_ne!(ru.t(k), k);
        }
    }

    #[test]
    fn only_the_view_bar_toggles_projection() {
        let bar = super::super::overlay::bar_items(false, false);
        let toggles = bar.iter().filter(|b| b.tip.starts_with("v3d.proj.")).count();
        assert_eq!(toggles, 1);
        for k in ["v3d.persp", "v3d.ortho"] {
            assert!(!super::super::LABEL_KEYS.contains(&k), "{k} was the cube's duplicate");
        }
        // the cube source flips the camera projection nowhere outside this test module
        let src = include_str!("cube.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        // home view stores `camera.ortho`; only a write would be a toggle
        assert!(!body.contains("ortho ="), "cube must not own a projection toggle");
    }
}
