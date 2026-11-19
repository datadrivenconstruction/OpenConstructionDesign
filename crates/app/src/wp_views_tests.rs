// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::edit3d::plane::{clip_line, World};
use crate::modify_tools::tests::walls;
use crate::snap::SnapKind;
use ddc_model::ViewKind;
use ddc_render::glam::DVec3;

const LO: V2 = V2 { x: -50.0, y: -50.0 };
const HI: V2 = V2 { x: 50.0, y: 50.0 };

fn near(a: V2, b: V2) -> bool {
    (a - b).len() < 1e-6
}

/// Plane through `o` with normal `n` (unit length not needed).
fn world(o: (f64, f64, f64), n: (f64, f64, f64)) -> World {
    let n = DVec3::new(n.0, n.1, n.2).normalize();
    let u = if n.z.abs() > 0.999 { DVec3::X } else { DVec3::Z.cross(n).normalize() };
    World { origin: DVec3::new(o.0, o.1, o.2), u, n }
}

fn shown(w: World) -> Option<Shown> {
    let source = crate::edit3d::plane::Source::Face;
    Some(Shown { plane: w, name: "Slope".into(), source, rev: 0 })
}

/// The segment lies on the line `y = k x + c` within the box, end to end.
fn on_line(s: (V2, V2), f: impl Fn(f64) -> f64) {
    for p in [s.0, s.1] {
        assert!((p.y - f(p.x)).abs() < 1e-6, "{p:?}");
    }
}

#[test]
fn clip_keeps_the_part_in_the_box_and_misses_outside() {
    let (a, b) = clip_line(v2(0.0, 1.0), v2(1.0, 0.0), LO, HI).unwrap();
    assert!(near(a, v2(-50.0, 1.0)) && near(b, v2(50.0, 1.0)));
    let (a, b) = clip_line(v2(0.0, 0.0), v2(1.0, 1.0), v2(0.0, 0.0), v2(2.0, 1.0)).unwrap();
    assert!(near(a, v2(0.0, 0.0)) && near(b, v2(1.0, 1.0)));
    assert!(clip_line(v2(0.0, 60.0), v2(1.0, 0.0), LO, HI).is_none());
    assert!(clip_line(v2(0.0, 60.0), v2(1.0, -0.01), v2(0.0, 0.0), v2(1.0, 1.0)).is_none());
}

#[test]
fn plan_trace_of_vertical_inclined_and_horizontal_planes() {
    // vertical plane x = 3: the line x = 3 at any level
    let v = world((3.0, 0.0, 0.0), (1.0, 0.0, 0.0));
    for z in [0.0, 7.5] {
        let (a, b) = v.plan_trace(z, LO, HI).unwrap();
        assert!((a.x - 3.0).abs() < 1e-9 && (b.x - 3.0).abs() < 1e-9);
        assert!((a.y - b.y).abs() > 99.0, "crosses the whole box");
    }
    // a 45 degree slope rising to the north from y = 2 at z = 0: at z the line y = 2 + z
    let s = world((0.0, 2.0, 0.0), (0.0, -1.0, 1.0));
    on_line(s.plan_trace(0.0, LO, HI).unwrap(), |_| 2.0);
    on_line(s.plan_trace(3.0, LO, HI).unwrap(), |_| 5.0);
    assert!((s.z_at(v2(10.0, 6.0)).unwrap() - 4.0).abs() < 1e-9);
    assert!(v.z_at(v2(1.0, 1.0)).is_none());
    // diagonal plane x - y = 1: the line y = x - 1
    let d = world((1.0, 0.0, 0.0), (1.0, -1.0, 0.0));
    on_line(d.plan_trace(0.0, LO, HI).unwrap(), |x| x - 1.0);
    // horizontal planes never cut a plan level
    assert!(world((0.0, 0.0, 3.0), (0.0, 0.0, 1.0)).plan_trace(3.0, LO, HI).is_none());
    // a trace outside the drawn box is not drawn
    assert!(v.plan_trace(0.0, v2(10.0, 10.0), v2(20.0, 20.0)).is_none());
}

#[test]
fn section_trace_in_section_coordinates() {
    // section line west to east along y = 0, looking north; s counts from a = (-10, 0)
    let (a, b) = (v2(-10.0, 0.0), v2(10.0, 0.0));
    // horizontal plane at z = 4: a level line in the section
    let h = world((0.0, 0.0, 4.0), (0.0, 0.0, 1.0));
    let (p, q) = h.section_trace(a, b, LO, HI).unwrap();
    assert!((p.y - 4.0).abs() < 1e-9 && (q.y - 4.0).abs() < 1e-9);
    // vertical plane x = 3 crosses the section at s = 13, any height
    let v = world((3.0, 0.0, 0.0), (1.0, 0.0, 0.0));
    let (p, q) = v.section_trace(a, b, LO, HI).unwrap();
    assert!((p.x - 13.0).abs() < 1e-9 && (q.x - 13.0).abs() < 1e-9);
    assert!((p.y - q.y).abs() > 99.0);
    // plane rising 1:1 to the east from x = 0: z = s - 10 in the section
    let r = world((0.0, 0.0, 0.0), (-1.0, 0.0, 1.0));
    on_line(r.section_trace(a, b, LO, HI).unwrap(), |s| s - 10.0);
    // the reversed line runs s the other way: z = 10 - s
    on_line(r.section_trace(b, a, LO, HI).unwrap(), |s| 10.0 - s);
    // a plane parallel to the section has no trace
    let par = world((0.0, 5.0, 0.0), (0.0, 1.0, 0.0));
    assert!(par.section_trace(a, b, LO, HI).is_none());
}

#[test]
fn world_and_scene_frames_round_trip_across_levels() {
    let w = world((1.0, 2.0, 5.0), (0.0, -1.0, 1.0));
    let p = w.to_scene(3.0, crate::edit3d::plane::Source::Face);
    assert!((p.origin.z - 2.0).abs() < 1e-6, "scene z counts from the level");
    assert!(p.u.dot(p.v).abs() < 1e-6 && p.u.dot(p.n).abs() < 1e-6 && p.v.dot(p.n).abs() < 1e-6);
    let back = p.to_world(3.0);
    assert!((back.origin - w.origin).length() < 1e-5);
    assert!((back.n - w.n).length() < 1e-6);
}

#[test]
fn plan_snaps_onto_the_trace_and_its_crossings() {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, &[((0.0, 0.0), (10.0, 0.0))]);
    let r = 0.2;
    // no work plane: a free point off the wall stays free
    let free = ed.snap_list(v2(4.0, 3.05), None, r, None);
    assert!(free.iter().all(|s| s.kind != SnapKind::WorkPlane));
    // a vertical plane along y = 3: the cursor 5 cm off it lands on it
    ed.work_plane = shown(world((0.0, 3.0, 0.0), (0.0, 1.0, 0.0)));
    let l = ed.snap_list(v2(4.0, 3.05), None, r, None);
    let s = l.iter().find(|s| s.kind == SnapKind::WorkPlane).expect("work plane snap");
    assert!(near(s.p, v2(4.0, 3.0)), "{:?}", s.p);
    assert_eq!(l[0].kind, SnapKind::WorkPlane, "{l:?}");
    // a plane crossing the wall: the crossing with the wall axis wins over the line
    ed.work_plane = shown(world((6.0, 0.0, 0.0), (1.0, 0.0, 0.0)));
    let l = ed.snap_list(v2(6.05, 0.08), None, r, None);
    assert_eq!(l[0].kind, SnapKind::Intersection, "{l:?}");
    assert!(near(l[0].p, v2(6.0, 0.0)), "{:?}", l[0].p);
    // an inclined plane snaps along its trace on the view level only
    ed.work_plane = shown(world((0.0, 5.0, ed.elevation), (0.0, -1.0, 1.0)));
    let l = ed.snap_list(v2(2.0, 5.04), None, r, None);
    let s = l.iter().find(|s| s.kind == SnapKind::WorkPlane).expect("trace on the level");
    assert!(near(s.p, v2(2.0, 5.0)), "{:?}", s.p);
    // a horizontal plane has no plan trace and gives no snap
    ed.work_plane = shown(world((0.0, 0.0, ed.elevation + 1.0), (0.0, 0.0, 1.0)));
    let l = ed.snap_list(v2(4.0, 3.05), None, r, None);
    assert!(l.iter().all(|s| s.kind != SnapKind::WorkPlane));
}

#[test]
fn section_segment_uses_the_view_line() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.work_plane = shown(world((3.0, 0.0, 0.0), (1.0, 0.0, 0.0)));
    let spec = ddc_model::SectionSpec {
        kind: ViewKind::Section,
        name: "A".into(),
        a: (-10.0, 0.0),
        b: (10.0, 0.0),
        depth: 5.0,
        far_clip: false,
    };
    let (p, _) = section_segment(&ed, &spec, LO, HI).unwrap();
    assert!((p.x - 13.0).abs() < 1e-9);
    ed.work_plane = None;
    assert!(section_segment(&ed, &spec, LO, HI).is_none());
}

#[test]
fn the_3d_plane_reaches_the_editor_only_when_the_user_set_one() {
    let mut a = crate::layout_contract::app_in(ddc_ui::Lang::En);
    sync(&mut a);
    assert!(a.ed.work_plane.is_none(), "the level plane is not shown");
    let w = world((0.0, 0.0, 2.0), (1.0, 0.0, 0.0));
    let elev = a.ed.elevation;
    a.view.e3.set_world_plane(&w, crate::edit3d::plane::Source::Face, elev);
    sync(&mut a);
    let s = a.ed.work_plane.clone().expect("shown");
    assert!((s.plane.origin - w.origin).length() < 1e-5);
    assert_eq!(s.name, a.sh.cat.t("view_cube.wp.face"));
    assert!(crate::edit3d::wp_win::save(&mut a, "Gable"));
    sync(&mut a);
    assert_eq!(a.ed.work_plane.as_ref().unwrap().name, "Gable");
    assert!(a.run("e3d.wp_level"));
    sync(&mut a);
    assert!(a.ed.work_plane.is_none());
}
