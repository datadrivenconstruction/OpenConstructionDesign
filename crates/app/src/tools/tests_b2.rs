// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! B2: room by a click inside a closed wall loop.

use super::{arm, Kind};
use crate::editor::Editor;
use crate::wall_tool::{Event, Mode};
use ddc_geom::v2;
use ddc_model::WallSpec;
use ddc_ui::i18n::{Catalog, Lang};

fn ed() -> Editor {
    Editor::new_in_memory().unwrap()
}

fn rooms(e: &Editor) -> i64 {
    e.model
        .sql()
        .query_row("SELECT COUNT(*) FROM room", [], |r| r.get(0))
        .unwrap()
}

fn click(e: &mut Editor, x: f64, y: f64) {
    e.handle(Event::Move(v2(x, y)));
    e.handle(Event::Click(v2(x, y)));
}

fn wall_segs(e: &mut Editor, segs: &[((f64, f64), (f64, f64))]) {
    let wall_type = e.current_type().unwrap().id;
    for &(a, b) in segs {
        e.model
            .add_wall(&WallSpec {
                a,
                b,
                level: e.level,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            })
            .unwrap();
    }
    e.reload().unwrap();
}

const BOX: [((f64, f64), (f64, f64)); 4] = [
    ((0.0, 0.0), (6.0, 0.0)),
    ((6.0, 0.0), (6.0, 4.0)),
    ((6.0, 4.0), (0.0, 4.0)),
    ((0.0, 4.0), (0.0, 0.0)),
];

fn msg(key: &str) -> String {
    Catalog::new(Lang::En).t(key)
}

#[test]
fn click_inside_walls_makes_one_room_with_one_undo() {
    let mut e = ed();
    wall_segs(&mut e, &BOX);
    let before = rooms(&e);
    arm(&mut e, Kind::Room, Lang::En);
    click(&mut e, 3.0, 2.0);
    assert_eq!(rooms(&e), before + 1);
    let (name, number, h): (String, String, f64) = e
        .model
        .sql()
        .query_row("SELECT name,number,height FROM room", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((name.as_str(), number.as_str()), ("Room 1", "1"));
    assert!(h > 0.0);
    assert_eq!(e.rooms.len(), 1, "the room has a closed outline");
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(rooms(&e), before, "one undo removes the room");
    e.redo();
    assert_eq!(rooms(&e), before + 1);
}

#[test]
fn open_region_is_refused_with_a_message() {
    let mut e = ed();
    wall_segs(&mut e, &BOX[..3]);
    arm(&mut e, Kind::Room, Lang::En);
    click(&mut e, 3.0, 2.0);
    assert_eq!(rooms(&e), 0);
    assert_eq!(e.status.as_deref(), Some(msg("tool.err.room_open").as_str()));
    click(&mut e, 20.0, 20.0);
    assert_eq!(rooms(&e), 0, "outside any walls");
}

#[test]
fn second_click_in_the_same_loop_is_refused() {
    let mut e = ed();
    wall_segs(&mut e, &BOX);
    arm(&mut e, Kind::Room, Lang::En);
    click(&mut e, 3.0, 2.0);
    click(&mut e, 2.0, 1.0);
    assert_eq!(rooms(&e), 1);
    assert_eq!(e.status.as_deref(), Some(msg("tool.err.room_exists").as_str()));
}

#[test]
fn named_room_uses_the_option_and_tool_stays_armed() {
    let mut e = ed();
    wall_segs(&mut e, &BOX);
    arm(&mut e, Kind::Room, Lang::En);
    e.model_tool_mut().unwrap().opts.name = "Hall".into();
    click(&mut e, 3.0, 2.0);
    let name: String = e
        .model
        .sql()
        .query_row("SELECT name FROM room", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Hall");
    assert!(e.model_tool().is_some(), "repeatable tool stays armed");
}

#[test]
fn room_keys_exist_in_both_locales() {
    for lang in [Lang::Ru, Lang::En] {
        let c = Catalog::new(lang);
        for k in [
            "tool.room",
            "tool.room.name",
            "hint.room.place",
            "room_tool.room.name",
            "tool.err.room_open",
            "tool.err.room_exists",
            "tool.err.room_check",
        ] {
            assert!(c.table(lang).contains_key(k), "{lang:?}: missing {k}");
        }
    }
}
