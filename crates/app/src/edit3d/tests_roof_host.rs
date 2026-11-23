// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof in 3D (plan roof tool through the 3D pointer), attach and join/cut by picking: each
//! commit is one undo step.

use super::host3d::{self, Op, State, Step};
use super::pick::{Cat, Target};
use super::*;
use crate::interact::{Btn, Input, Key, Mods, View3dAdapter, ViewAdapter};
use ddc_geom::{v2, v3};
use ddc_model::{LayerSpec, RoofKind, RoofSpec, SlabSpec};

fn view(ed: &Editor) -> View3dAdapter {
    let cam = ddc_render::OrbitCamera {
        distance: 40.0,
        ..Default::default()
    };
    View3dAdapter::new(
        &cam,
        [0.0, 0.0],
        [1600.0, 1200.0],
        ed.elevation,
        crate::interact::WorkPlane::level(ed.level, ed.elevation),
    )
}

fn roofs(ed: &Editor) -> Vec<i64> {
    let mut st = ed
        .model
        .sql()
        .prepare("SELECT id FROM roof ORDER BY id")
        .unwrap();
    let v = st.query_map([], |r| r.get(0)).unwrap().flatten().collect();
    v
}

/// Arms the plan roof tool the way the 3D view does (`plan_route::hotkey`).
fn arm_roof(ed: &mut Editor) {
    assert!(crate::interact::script::arm(
        ed,
        plan_route::hotkey(create::Kind::Roof)
    ));
    assert!(crate::interact::sessions::model_tool::get(ed)
        .is_some_and(|a| a.kind == crate::tools::Kind::Roof));
}

fn click3d(ed: &mut Editor, ad: &View3dAdapter, p: ddc_geom::V3) {
    let s = ad.project_at(p).expect("on screen");
    let ptr = wall_pick_pointer(ed, ad.pointer_at(s));
    ed.input(Input::Move(ptr));
    ed.input(Input::Click(ptr, Btn::Left));
}

#[test]
fn roof_footprint_sketched_in_3d_with_one_edge_not_sloped() {
    let mut ed = Editor::new_in_memory().unwrap();
    arm_roof(&mut ed);
    let ad = view(&ed);
    let z = ed.elevation;
    for (x, y) in [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)] {
        click3d(&mut ed, &ad, v3(x, y, z));
    }
    // Space: the last edge does not define a slope
    ed.input(Input::Char(' '));
    let undo = ed.undo_len();
    ed.input(Input::Key(Key::Enter, Mods::NONE));
    let r = roofs(&ed);
    assert_eq!(r.len(), 1, "{:?}", ed.status);
    assert_eq!(ed.undo_len(), undo + 1, "one undo step");
    let RoofKind::Footprint { ring, slopes, .. } = ed.model.roof(r[0]).unwrap().kind else {
        panic!("footprint roof");
    };
    assert_eq!(ring.len(), 4);
    assert_eq!(
        slopes.iter().filter(|s| s.is_none()).count(),
        1,
        "{slopes:?}"
    );
    ed.undo();
    assert!(roofs(&ed).is_empty());
}

#[test]
fn roof_by_picking_wall_faces_in_3d() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (a, b, c, d) = (v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0));
    ed.commit_walls(vec![(a, b), (b, c), (c, d), (d, a)]);
    assert_eq!(ed.walls.len(), 4);
    arm_roof(&mut ed);
    crate::interact::sessions::model_tool::get_mut(&mut ed)
        .unwrap()
        .opts
        .walls = true;
    let ad = view(&ed);
    // the middle of each wall at mid height: the ray meets the face, not the level plane
    for (p, q) in [(a, b), (b, c), (c, d), (d, a)] {
        let m = (p + q) * 0.5;
        let f = crate::interact::host::face_point(&ed, m, ad.eye, 1.5).expect("wall face");
        click3d(&mut ed, &ad, f);
    }
    assert_eq!(
        crate::interact::sessions::model_tool::get(&ed)
            .unwrap()
            .picked
            .len(),
        4
    );
    ed.input(Input::Key(Key::Enter, Mods::NONE));
    assert_eq!(roofs(&ed).len(), 1, "{:?}", ed.status);
}

#[test]
fn roof_off_the_level_plane_is_refused() {
    let mut c = create::Create::new(create::Kind::Roof);
    let ctx = create::Ctx {
        level: 1,
        wall_type: 1,
        height: 3.0,
        close_tol: create::CLOSE_TOL,
        levels: vec![],
    };
    let wp = plane::WorkPlane::level(1, 2.0);
    let e = c.click(&ctx, &wp, Vec3::ZERO).unwrap_err();
    assert!(
        matches!(&e, ddc_model::Error::Invalid(k) if k == "e3d.err.roof_level"),
        "{e:?}"
    );
    assert_ne!(err_text(false, &e), "e3d.err.roof_level");
}

struct Site {
    ed: Editor,
    wall: i64,
    roof: i64,
    slab: i64,
}

fn site() -> Site {
    let mut ed = Editor::new_in_memory().unwrap();
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let st = ed.model.add_slab_type("S host3d", &layer).unwrap();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(4.0, 0.0))]);
    let wall = ed.walls[0].id;
    let level = ed.level;
    let roof = ed
        .model
        .add_roof(&RoofSpec {
            level,
            slab_type: st,
            offset: 3.0,
            overhang: 0.0,
            fascia: 0.2,
            kind: RoofKind::Footprint {
                ring: vec![(-1.0, -1.0), (5.0, -1.0), (5.0, 4.0), (-1.0, 4.0)],
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        })
        .unwrap();
    let slab = ed
        .model
        .add_slab(&SlabSpec {
            level,
            slab_type: st,
            offset: 0.0,
            outer: vec![(-1.0, -1.0), (5.0, -1.0), (5.0, 4.0), (-1.0, 4.0)],
            holes: vec![],
        })
        .unwrap();
    ed.reload().unwrap();
    Site {
        ed,
        wall,
        roof,
        slab,
    }
}

fn top_roof(ed: &Editor, w: i64) -> Option<i64> {
    ed.model
        .sql()
        .query_row("SELECT top_roof FROM wall WHERE id=?1", [w], |r| r.get(0))
        .unwrap()
}

#[test]
fn attach_top_to_roof_picked_in_3d_is_one_undo_step() {
    let Site {
        mut ed, wall, roof, ..
    } = site();
    let (w, r) = (Target::new(Cat::Wall, wall), Target::new(Cat::Roof, roof));
    let mut st = State::default();
    // no walls yet: the roof is refused, live
    assert_eq!(st.check(Op::AttachTop, r), Err("e3d.err.attach_walls"));
    // the selected wall starts the attach
    st.start(Op::AttachTop, &[w]);
    assert_eq!(st.walls, vec![wall]);
    assert_eq!(st.check(Op::AttachTop, r), Ok(Step::Commit));
    assert_eq!(st.check(Op::AttachBase, r), Err("e3d.err.attach_base_roof"));
    assert_eq!(
        st.check(Op::AttachTop, Target::new(Cat::Room, 1)),
        Err("e3d.err.attach_target")
    );
    let undo = ed.undo_len();
    assert_eq!(
        st.click(&mut ed, Op::AttachTop, r).ok(),
        Some("e3d.done.attach")
    );
    assert_eq!(top_roof(&ed, wall), Some(roof));
    assert_eq!(ed.undo_len(), undo + 1);
    assert!(st.walls.is_empty(), "the tool starts over");
    ed.undo();
    assert_eq!(top_roof(&ed, wall), None);
}

#[test]
fn attach_base_to_floor_picks_walls_in_the_tool() {
    let Site {
        mut ed, wall, slab, ..
    } = site();
    let mut st = State::default();
    st.start(Op::AttachBase, &[]);
    let w = Target::new(Cat::Wall, wall);
    assert_eq!(
        st.click(&mut ed, Op::AttachBase, w).ok(),
        Some("e3d.pick.target")
    );
    // a second click on the wall leaves it out again
    assert_eq!(
        st.click(&mut ed, Op::AttachBase, w).ok(),
        Some("e3d.pick.target")
    );
    assert!(st.walls.is_empty());
    st.click(&mut ed, Op::AttachBase, w).unwrap();
    let undo = ed.undo_len();
    assert_eq!(
        st.click(&mut ed, Op::AttachBase, Target::new(Cat::Slab, slab))
            .ok(),
        Some("e3d.done.attach")
    );
    assert_eq!(ed.undo_len(), undo + 1);
    let n: i64 = ed
        .model
        .sql()
        .query_row(
            "SELECT count(*) FROM wall_attach WHERE wall=?1 AND side='base'",
            [wall],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn join_unjoin_and_cut_by_two_picks() {
    let Site {
        mut ed,
        wall,
        roof,
        slab,
    } = site();
    let (w, r, s) = (
        Target::new(Cat::Wall, wall),
        Target::new(Cat::Roof, roof),
        Target::new(Cat::Slab, slab),
    );
    assert_eq!(host3d::item(r), Some(("roof".to_string(), roof)));
    assert_eq!(host3d::item(Target::new(Cat::Room, 1)), None);
    let mut st = State::default();
    assert_eq!(st.click(&mut ed, Op::Join, w).ok(), Some("e3d.pick.second"));
    assert_eq!(st.check(Op::Join, w), Err("e3d.err.join_same"));
    let undo = ed.undo_len();
    assert_eq!(st.click(&mut ed, Op::Join, s).ok(), Some("e3d.done.join"));
    assert_eq!(ed.undo_len(), undo + 1);
    assert_eq!(ed.model.joins().unwrap().len(), 1);
    st.click(&mut ed, Op::Unjoin, s).unwrap();
    assert_eq!(
        st.click(&mut ed, Op::Unjoin, w).ok(),
        Some("e3d.done.unjoin")
    );
    assert!(ed.model.joins().unwrap().is_empty());
    // the roof cuts the wall
    st.click(&mut ed, Op::Cut, r).unwrap();
    assert_eq!(st.click(&mut ed, Op::Cut, w).ok(), Some("e3d.done.cut"));
    let j = ed.model.joins().unwrap();
    assert_eq!(
        j,
        vec![(("roof".to_string(), roof), ("wall".to_string(), wall))]
    );
    ed.undo();
    assert!(ed.model.joins().unwrap().is_empty());
}

#[test]
fn roof_extrusion_runs_on_a_wall_selected_in_3d() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.commit_walls(vec![(v2(0.0, 0.0), v2(6.0, 0.0))]);
    let w = ed.walls[0].id;
    let mut a = EditorApp::new(ed, None, "test".into());
    let mut e = std::mem::take(&mut a.view.e3);
    e.set_sel(&mut a.ed, vec![Target::new(Cat::Wall, w)]);
    a.view.e3 = e;
    assert!(a.run("arch.roof_extrusion"));
    assert_eq!(roofs(&a.ed).len(), 1, "{:?}", a.ed.status);
}

#[test]
fn new_tools_light_their_commands_and_have_hints() {
    for op in Op::ALL {
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        assert!(a.run(op.command()), "{}", op.command());
        assert_eq!(a.active_command(), Some(op.command()));
        assert_eq!(a.view.e3.scope(), crate::interact::Scope::Pointing);
        a.view.e3.escape();
        assert_eq!(a.view.e3.tool, Tool::Select);
    }
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    assert!(a.run("e3d.roof"));
    assert_eq!(a.active_command(), Some("e3d.roof"));
    for ru in [false, true] {
        for k in [
            "e3d.pick.target",
            "e3d.pick.second",
            "e3d.done.attach",
            "e3d.done.join",
            "e3d.done.unjoin",
            "e3d.done.cut",
        ] {
            assert_ne!(tr(ru, k), k);
        }
    }
}
