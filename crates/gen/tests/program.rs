// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The programme as data and presets in place of typology templates.

use ddc_gen::project::presets::{self, Preset};
use ddc_gen::project::program::{Access, Floor, Light, Program};
use ddc_gen::project::rules::Layer;
use ddc_gen::project::{build_project, parse_spec, RoomKind, Typology};
use serde_json::json;

#[test]
fn a_program_reads_back_what_it_writes() {
    let v = json!({"schema": "gen_program.v1", "floors": 4, "spaces": [
        {"id": "rx", "kind": "lobby", "area": 80, "floor": "ground", "access": "public"},
        {"kind": "office", "area": {"min": 16, "max": 24}, "count": 6, "floor": "typical",
         "light": {"daylight_share": 0.2}, "height_min": 3.0, "adjacent_to": ["meeting", {"ref": "rx", "weight": 3}]},
        {"kind": "technical", "area": 12, "floor": 0, "access": "staff", "light": "none"}
    ]});
    let p = Program::from_json(&v).unwrap();
    assert_eq!(p.floors, Some(4));
    let o = &p.spaces[1];
    assert_eq!((o.area, o.count, o.floor, o.light), ((16.0, 24.0), 6, Floor::Typical, Light::Share(0.2)));
    assert_eq!(o.target_area(), 20.0);
    assert_eq!(p.spaces[2].access, Access::Staff);
    assert_eq!(p.spaces[2].floor, Floor::Level(0));
    assert_eq!(Program::from_json(&p.to_json()).unwrap(), p);
    // a wish counts from both sides and by id
    assert_eq!(p.adjacency(RoomKind::Office, RoomKind::Lobby), 3.0);
    assert_eq!(p.adjacency(RoomKind::Meeting, RoomKind::Office), 1.0);
}

#[test]
fn every_program_problem_names_its_path() {
    let v = json!({"spaces": [{"kind": "spaceship", "area": 5},
        {"kind": "office", "area": {"min": 30, "max": 10}, "floor": "roof", "colour": 1}]});
    let e = Program::from_json(&v).unwrap_err();
    for want in ["spaces[0].kind", "spaces[1].area", "spaces[1].floor", "spaces[1].colour"] {
        assert!(e.iter().any(|x| x.starts_with(want)), "{want} in {e:?}");
    }
    let s = parse_spec(&json!({"typology": "office", "program": {"spaces": [{"kind": "x", "area": 5}]}}));
    assert_eq!(s.unwrap_err().0[0].path, "program.spaces[0].kind");
}

#[test]
fn the_brief_program_gives_the_rooms() {
    let s = parse_spec(&json!({"typology": "office", "floors": 3, "program": {"spaces": [
        {"kind": "office", "area": {"min": 16, "max": 24}, "count": 6, "floor": "typical"},
        {"kind": "lobby", "area": 80, "floor": "ground"}]}}))
    .unwrap();
    let typical: Vec<(RoomKind, f64, u32)> = s.rooms.iter().map(|r| (r.kind, r.area, r.count)).collect();
    assert_eq!(typical, vec![(RoomKind::Office, 20.0, 6)]);
    assert_eq!(s.ground_rooms.iter().map(|r| r.kind).collect::<Vec<_>>(), vec![RoomKind::Lobby]);
    assert_eq!(s.program.spaces.len(), 2);
    build_project(&s).unwrap();
    // without one the spec carries the typology's programme as data
    let d = parse_spec(&json!({"typology": "school", "floors": 2})).unwrap();
    assert!(d.program.spaces.iter().any(|x| x.kind == RoomKind::Classroom));
}

#[test]
fn a_preset_is_data_under_the_brief() {
    let mine = json!({"name": "my office", "base": "office", "brief": {"floors": 2},
        "rules": {"corridor.width": 2.2, "room.door": 1.0},
        "program": {"spaces": [{"kind": "office", "area": 18, "count": 8}]}});
    let s = parse_spec(&json!({"preset": mine})).unwrap();
    assert_eq!((s.typology, s.floors, s.corridor), (Typology::Office, 2, 2.2));
    assert_eq!(s.rules.resolve("room.door"), Some((1.0, Layer::Preset)));
    assert_eq!(s.template.as_deref(), Some("my office"));
    let b = parse_spec(&json!({"preset": mine, "floors": 3, "rules": {"corridor.width": 2.0}})).unwrap();
    assert_eq!((b.floors, b.corridor), (3, 2.0));
    assert_eq!(b.rules.source("corridor.width"), Some(Layer::Brief));
    build_project(&b).unwrap();
    assert!(parse_spec(&json!({"preset": "no_such"})).is_err());
}

#[test]
fn a_preset_by_name_builds_as_its_typology_did() {
    let a = build_project(&parse_spec(&json!({"typology": "hotel", "floors": 4})).unwrap()).unwrap();
    let b = build_project(&parse_spec(&json!({"preset": "hotel", "floors": 4})).unwrap()).unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    let office = parse_spec(&json!({"preset": "office", "floors": 2})).unwrap();
    assert_eq!(office.template, None);
    let all: Vec<String> = presets::builtin().into_iter().map(|p: Preset| p.name).collect();
    for want in ["house", "office", "hotel", "warehouse"] {
        assert!(all.iter().any(|n| n == want), "{want} in {all:?}");
    }
}
