// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use ddc_model::{Form, MassSpec, RoomSpec, WallSpec};
use std::time::Instant;

fn wall(ed: &Editor, a: (f64, f64), b: (f64, f64)) -> WallSpec {
    WallSpec {
        a,
        b,
        level: ed.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ed.types[0].id,
        offset0: None,
    }
}

fn view(kind: ViewKind, name: &str, level: Option<i64>) -> ViewRef {
    ViewRef {
        kind,
        name: name.into(),
        level,
    }
}

fn input(kind: ViewKind, hits: Vec<(Sel, [f64; 3])>) -> PointInput {
    PointInput {
        view: view(kind, "v", None),
        hits,
        region: None,
        pose: None,
        crop: None,
    }
}

fn mass(ed: &mut Editor) -> i64 {
    let id = ed
        .model
        .add_mass(&MassSpec {
            level: ed.level,
            name: "m".into(),
            void: false,
            inplace: false,
            form: Form::Cuboid {
                lo: (0.0, 0.0, 0.0),
                hi: (4.0, 3.0, 2.0),
            },
        })
        .unwrap();
    ed.reload().unwrap();
    id
}

fn room_fixture() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let ws = [
        wall(&ed, (0.0, 0.0), (4.0, 0.0)),
        wall(&ed, (4.0, 0.0), (4.0, 4.0)),
        wall(&ed, (4.0, 4.0), (0.0, 4.0)),
        wall(&ed, (0.0, 4.0), (0.0, 0.0)),
    ];
    let ids = ed.model.add_walls(&ws).unwrap();
    let room = ed
        .model
        .add_room(&RoomSpec {
            level: ed.level,
            name: "R".into(),
            number: "1".into(),
            point: (2.0, 2.0),
            height: 3.0,
        })
        .unwrap();
    ed.reload().unwrap();
    (ed, ids[0], room)
}

#[test]
fn point_wall_3d_golden() {
    let (ed, w, room) = room_fixture();
    let mut inp = input(ViewKind::View3d, vec![(Sel::Wall(w), [2.0, 0.0, 1.5])]);
    inp.pose = Some(PoseResolved {
        target: [2.0, 2.0, 1.0],
        yaw: 0.5,
        pitch: 0.25,
        distance: 10.0,
    });
    let c = collect(&ed, &inp);
    let t = &c.targets[0];
    assert_eq!(t.el, ElRef { cat: "wall".into(), id: IdOrNew::Id(w) });
    assert_eq!(t.family, "wall");
    assert_eq!(t.level, Some(ed.level));
    assert_eq!(t.face, None);
    assert_eq!(t.hit, [2.0, 0.0, 1.5]);
    let j = to_json(&c);
    assert_eq!(j["view"]["kind"], "3d");
    assert_eq!(j["pose"]["distance"], 10.0);
    assert_eq!(j["targets"][0]["el"], format!("wall:{w}"));
    assert_eq!(j["targets"][0]["hit"], json!([2.0, 0.0, 1.5]));
    assert_eq!(j["targets"][0]["face"], J::Null);
    assert_eq!(j["token"], json!(c.token.0));
    let m = message(&c, "why a warning here?");
    assert!(m.starts_with("```point_ctx\n{"));
    assert!(m.ends_with("```\nwhy a warning here?"));
    let _ = room;
}

#[test]
fn point_face_label_mass() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = mass(&mut ed);
    let faces = ed.model.mass_faces(id).unwrap();
    let tag_of = |n: (f64, f64, f64)| {
        faces
            .iter()
            .find(|f| (f.normal.0 - n.0).abs() + (f.normal.1 - n.1).abs() + (f.normal.2 - n.2).abs() < 1e-6)
            .unwrap()
            .tag
    };
    let side = collect(&ed, &input(ViewKind::View3d, vec![(Sel::Mass(id), [4.0, 1.5, 1.0])]));
    let top = collect(&ed, &input(ViewKind::View3d, vec![(Sel::Mass(id), [2.0, 1.5, 2.0])]));
    assert_eq!(side.targets[0].face, Some(FaceLabel(tag_of((1.0, 0.0, 0.0)))));
    assert_eq!(top.targets[0].face, Some(FaceLabel(tag_of((0.0, 0.0, 1.0)))));
    assert_ne!(side.targets[0].face, top.targets[0].face);
    // a hit off every face carries no label
    let air = collect(&ed, &input(ViewKind::View3d, vec![(Sel::Mass(id), [9.0, 9.0, 9.0])]));
    assert_eq!(air.targets[0].face, None);
    // the label survives an unrelated edit
    ed.model.add_walls(&[wall(&ed, (20.0, 0.0), (24.0, 0.0))]).unwrap();
    ed.reload().unwrap();
    let again = collect(&ed, &input(ViewKind::View3d, vec![(Sel::Mass(id), [4.0, 1.5, 1.0])]));
    assert_eq!(again.targets[0].face, side.targets[0].face);
    assert_ne!(again.token, side.token);
}

#[test]
fn point_room_plan() {
    let (ed, w, room) = room_fixture();
    let inside = collect(&ed, &input(ViewKind::Plan, vec![(Sel::Wall(w), [2.0, 2.0, 0.0])]));
    assert_eq!(inside.targets[0].room, Some(room));
    let outside = collect(&ed, &input(ViewKind::Plan, vec![(Sel::Wall(w), [9.0, 9.0, 0.0])]));
    assert_eq!(outside.targets[0].room, None);
    let own = collect(&ed, &input(ViewKind::Plan, vec![(Sel::Room(room), [2.0, 2.0, 0.0])]));
    assert_eq!(own.targets[0].room, Some(room));
    assert_eq!(own.targets[0].family, "room");
    assert!(own.targets[0].qty.iter().any(|(k, v)| k == "area" && (*v - 16.0).abs() < 1.0));
}

#[test]
fn point_box_50_cap() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ws: Vec<WallSpec> = (0..60).map(|k| wall(&ed, (k as f64 * 5.0, 0.0), (k as f64 * 5.0 + 4.0, 0.0))).collect();
    let ids = ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
    let mut hits: Vec<_> = ids.iter().map(|&i| (Sel::Wall(i), [0.0; 3])).collect();
    hits.push(hits[0]);
    let mut inp = input(ViewKind::Plan, hits);
    inp.region = Some(Region {
        min: [0.0, -1.0],
        max: [300.0, 1.0],
        level: Some(ed.level),
    });
    let _ = collect(&ed, &inp); // warm the row cache as the session does
    let best = (0..3)
        .map(|_| {
            let t = Instant::now();
            let c = collect(&ed, &inp);
            assert_eq!(c.targets.len(), MAX_TARGETS);
            assert_eq!(c.dropped, 10);
            t.elapsed()
        })
        .min()
        .unwrap();
    assert!(best.as_millis() <= 10, "collect took {best:?}");
    let j = to_json(&collect(&ed, &inp));
    assert_eq!(j["targets_dropped"], 10);
    assert_eq!(j["region"]["max"], json!([300.0, 1.0]));
}

#[test]
fn point_props_capped() {
    let (mut ed, w, _) = room_fixture();
    for k in 0..40 {
        ed.model
            .set_prop("wall", w, &format!("p{k:02}"), Some((rusqlite::types::Value::Real(1.0), "")))
            .unwrap();
    }
    ed.reload().unwrap();
    let c = collect(&ed, &input(ViewKind::Plan, vec![(Sel::Wall(w), [2.0, 0.0, 0.0])]));
    assert!(c.targets[0].props.len() <= MAX_PROPS);
}

#[test]
fn point_section_and_sheet() {
    let (ed, w, room) = room_fixture();
    for (kind, name) in [(ViewKind::Section, "A-A"), (ViewKind::Sheet, "A101")] {
        let mut inp = input(kind, vec![(Sel::Wall(w), [2.0, 0.0, 1.5]), (Sel::Room(room), [2.0, 2.0, 0.0])]);
        inp.view = view(kind, name, Some(ed.level));
        inp.crop = Some(ImageRef("frame-1".into()));
        let c = collect(&ed, &inp);
        let j = to_json(&c);
        let want = json!({
            "view": { "kind": kind.as_str(), "name": name, "level": ed.level },
            "pose": J::Null,
            "region": J::Null,
            "crop": "frame-1",
            "token": c.token.0,
            "targets": [
                {
                    "el": format!("wall:{w}"), "family": "wall",
                    "type": c.targets[0].type_name, "level": ed.level, "room": room,
                    "hit": [2.0, 0.0, 1.5], "face": J::Null,
                    "props": j["targets"][0]["props"], "qty": j["targets"][0]["qty"],
                },
                {
                    "el": format!("room:{room}"), "family": "room",
                    "type": c.targets[1].type_name, "level": ed.level, "room": room,
                    "hit": [2.0, 2.0, 0.0], "face": J::Null,
                    "props": j["targets"][1]["props"], "qty": j["targets"][1]["qty"],
                },
            ],
            "targets_dropped": 0,
        });
        assert_eq!(j, want);
    }
}

#[test]
fn point_esc_leaves_nothing() {
    let (ed, w, _) = room_fixture();
    let rev = ed.revision;
    let changes = ed.model.sql().total_changes();
    let mut m = PointMode::default();
    m.toggle();
    assert!(m.active());
    m.point(collect(&ed, &input(ViewKind::Plan, vec![(Sel::Wall(w), [2.0, 0.0, 0.0])])));
    m.esc();
    assert_eq!(m, PointMode::Off);
    assert_eq!(m.send("hi"), None);
    // a click outside the mode is ignored; sending ends the mode
    m.point(collect(&ed, &input(ViewKind::Plan, vec![])));
    assert_eq!(m, PointMode::Off);
    m.toggle();
    assert_eq!(m.send("plain").as_deref(), Some("plain"));
    assert!(!m.active());
    assert_eq!((ed.revision, ed.model.sql().total_changes()), (rev, changes));
}
