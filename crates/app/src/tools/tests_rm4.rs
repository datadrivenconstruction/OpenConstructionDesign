// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! rm4: a room in every closed region of the level, one undo step, with a report.

use super::room::{place_all, AutoReport};
use super::{arm, Kind};
use crate::editor::Editor;
use ddc_model::{RoomSpec, WallSpec};
use ddc_ui::i18n::{Catalog, Lang};

type Seg = ((f64, f64), (f64, f64));

fn ed() -> Editor {
    Editor::new_in_memory().unwrap()
}

fn walls(e: &mut Editor, segs: &[Seg]) {
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

/// 12 x 4 box split by walls at x = 4 and x = 8: three closed regions.
const THREE: [Seg; 6] = [
    ((0.0, 0.0), (12.0, 0.0)),
    ((12.0, 0.0), (12.0, 4.0)),
    ((12.0, 4.0), (0.0, 4.0)),
    ((0.0, 4.0), (0.0, 0.0)),
    ((4.0, 0.0), (4.0, 4.0)),
    ((8.0, 0.0), (8.0, 4.0)),
];

/// Open U far from the box: three walls, top side missing.
const U: [Seg; 3] = [
    ((20.0, 0.0), (24.0, 0.0)),
    ((24.0, 0.0), (24.0, 4.0)),
    ((20.0, 0.0), (20.0, 4.0)),
];

/// (name, number, x, y, height) of the level's rooms by id.
fn rows(e: &Editor) -> Vec<(String, String, f64, f64, f64)> {
    e.model
        .sql()
        .prepare("SELECT name,number,px,py,height FROM room WHERE level=?1 ORDER BY id")
        .unwrap()
        .query_map([e.level], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn add(e: &mut Editor, number: &str, p: (f64, f64)) {
    e.model
        .add_room(&RoomSpec {
            level: e.level,
            name: format!("Kept {number}"),
            number: number.into(),
            point: p,
            height: 2.7,
        })
        .unwrap();
    e.reload().unwrap();
}

fn x_of(r: &(String, String, f64, f64, f64)) -> f64 {
    r.2
}

#[test]
fn place_all_fills_every_enclosed_region() {
    let mut e = ed();
    walls(&mut e, &THREE);
    walls(&mut e, &U);
    let r = place_all(&mut e).unwrap();
    assert_eq!(
        r,
        AutoReport {
            placed: 3,
            occupied: 0,
            open: 1,
        }
    );
    let got = rows(&e);
    assert_eq!(got.len(), 3);
    // one room per region, none in the open U
    let mut cells: Vec<i32> = got.iter().map(|r| (x_of(r) / 4.0).floor() as i32).collect();
    cells.sort();
    assert_eq!(cells, vec![0, 1, 2]);
    for (_, _, x, y, h) in &got {
        assert!(*y > 0.0 && *y < 4.0 && *x > 0.0 && *x < 12.0, "({x}, {y})");
        assert!(*h > 0.0);
    }
    assert_eq!(e.rooms.len(), 3, "every placed room has a closed outline");
    let s = crate::tr("room_place.placed", &[("n", "3"), ("m", "0"), ("k", "1")]);
    assert_eq!(r.status(), s);
}

#[test]
fn existing_rooms_not_duplicated() {
    let mut e = ed();
    walls(&mut e, &THREE);
    add(&mut e, "1", (6.0, 2.0));
    let r = place_all(&mut e).unwrap();
    assert_eq!((r.placed, r.occupied, r.open), (2, 1, 0));
    let got = rows(&e);
    assert_eq!(got.len(), 3);
    assert_eq!(
        got.iter().filter(|r| (4.0..8.0).contains(&x_of(r))).count(),
        1,
        "the middle region keeps its single room"
    );
    // a second run places nothing and says every region already has a room
    let again = place_all(&mut e).unwrap();
    assert_eq!((again.placed, again.occupied, again.open), (0, 3, 0));
    assert_eq!(rows(&e).len(), 3);
    assert_eq!(
        again.status(),
        crate::tr("room_place.all_had", &[("n", "0"), ("m", "3"), ("k", "0")])
    );
}

#[test]
fn numbers_continue_sequence() {
    let mut e = ed();
    walls(&mut e, &THREE);
    add(&mut e, "1", (2.0, 2.0));
    add(&mut e, "3", (10.0, 2.0));
    // a fourth region above the first one
    walls(
        &mut e,
        &[
            ((0.0, 4.0), (0.0, 8.0)),
            ((0.0, 8.0), (4.0, 8.0)),
            ((4.0, 8.0), (4.0, 4.0)),
        ],
    );
    let r = place_all(&mut e).unwrap();
    assert_eq!(r.placed, 2);
    let base = crate::tr("tool.room.name", &[]);
    let new: Vec<(String, String)> = rows(&e)[2..]
        .iter()
        .map(|r| (r.0.clone(), r.1.clone()))
        .collect();
    // numbers continue after the level's count (as one click would): 4, then 5; no duplicates
    let mut nums: Vec<&str> = new.iter().map(|r| r.1.as_str()).collect();
    nums.sort();
    assert_eq!(nums, vec!["4", "5"]);
    for (name, number) in &new {
        assert_eq!(name, &format!("{base} {number}"));
    }
}

#[test]
fn place_all_uses_room_tool_options() {
    let mut e = ed();
    walls(&mut e, &THREE);
    arm(&mut e, Kind::Room, Lang::En);
    {
        let a = e.model_tool_mut().unwrap();
        a.opts.height = 2.5;
        a.opts.name = "Office".into();
    }
    place_all(&mut e).unwrap();
    let got = rows(&e);
    assert_eq!(got.len(), 3);
    for (name, _, _, _, h) in &got {
        assert_eq!(name, "Office");
        assert!((h - 2.5).abs() < 1e-12);
    }
}

#[test]
fn place_all_one_undo() {
    let mut e = ed();
    walls(&mut e, &THREE);
    add(&mut e, "1", (2.0, 2.0));
    place_all(&mut e).unwrap();
    assert_eq!(rows(&e).len(), 3);
    e.undo();
    let left = rows(&e);
    assert_eq!(left.len(), 1, "one undo removes every placed room, nothing else");
    assert_eq!(left[0].1, "1");
    e.redo();
    assert_eq!(rows(&e).len(), 3);
    assert_eq!(e.rooms.len(), 3);
}

#[test]
fn no_regions_says_nothing_placed() {
    let mut e = ed();
    let undo_before = e.can_undo();
    let r = place_all(&mut e).unwrap();
    assert_eq!(r, AutoReport::default());
    assert_eq!(r.status(), crate::tr("room_place.none", &[]));
    assert_eq!(e.can_undo(), undo_before, "nothing placed: no undo step");
    // open loops are reported, never closed by guess
    walls(&mut e, &U);
    let r = place_all(&mut e).unwrap();
    assert_eq!(
        r,
        AutoReport {
            placed: 0,
            occupied: 0,
            open: 1,
        }
    );
    assert!(rows(&e).is_empty());
    assert_eq!(
        r.status(),
        crate::tr("room_place.none_open", &[("n", "0"), ("m", "0"), ("k", "1")])
    );
}

#[test]
fn rm4_keys_exist_in_both_locales() {
    for lang in [Lang::Ru, Lang::En] {
        let c = Catalog::new(lang);
        for k in [
            "cmd.arch.room_auto",
            "cmd.arch.room_auto.desc",
            "room_place.placed",
            "room_place.all_had",
            "room_place.none",
            "room_place.none_open",
        ] {
            assert!(c.table(lang).contains_key(k), "{lang:?}: missing {k}");
        }
    }
    let r = ddc_ui::Registry::builtin();
    assert!(r.all().iter().any(|c| c.id == "arch.room_auto"));
}

#[test]
fn place_all_twice_changes_nothing_and_adds_no_undo_step() {
    let mut e = ed();
    walls(&mut e, &THREE);
    place_all(&mut e).unwrap();
    let head = e.model.change_head();
    let first = rows(&e);
    let again = place_all(&mut e).unwrap();
    assert_eq!((again.placed, again.occupied), (0, 3));
    assert_eq!(rows(&e), first, "same rooms, same numbers");
    assert_eq!(e.model.change_head(), head, "the model journal did not move");
    // one undo removes the single batch, a second one reaches the walls, not a phantom step
    e.undo();
    assert!(rows(&e).is_empty());
    let n: i64 = e
        .model
        .sql()
        .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 6);
}

#[test]
fn locked_room_value_survives_recompute_and_auto_place() {
    use ddc_model::{Edit, PVal, ParamSet, ParamSpec, ParamType};
    let mut e = ed();
    walls(&mut e, &THREE);
    add(&mut e, "1", (6.0, 2.0));
    let room: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM room", [], |r| r.get(0))
        .unwrap();
    let param = e
        .model
        .add_param(&ParamSpec {
            name: "Finish".into(),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["room".into()],
        })
        .unwrap();
    let text = |s: &str| Some(PVal::Text(s.into()));
    e.model
        .set_param_values(&[ParamSet {
            param,
            kind: "room".into(),
            id: room,
            value: text("paint"),
        }])
        .unwrap();
    e.model.set_param_lock(&[("room", room, "Finish")]).unwrap();
    e.reload().unwrap();
    let kept = |e: &Editor| e.model.param_value(param, "room", room).unwrap();
    assert_eq!(kept(&e), text("paint"));
    // a wall edit recomputes the region; the value stays, also through undo and redo
    let wall: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM wall ORDER BY id LIMIT 1", [], |r| r.get(0))
        .unwrap();
    e.run_op(crate::editor::OpFn(std::sync::Arc::new(move |m| m.delete_wall(wall))))
        .unwrap();
    assert_eq!(kept(&e), text("paint"));
    e.undo();
    assert_eq!(kept(&e), text("paint"));
    e.redo();
    e.undo();
    // auto-place on the same level leaves the occupied region and its values alone
    place_all(&mut e).unwrap();
    assert_eq!(kept(&e), text("paint"));
    // a workbook edit of the locked value is dropped
    e.apply_xlsx(
        vec![Edit::ParamValue {
            param: "Finish".into(),
            kind: "room".into(),
            id: room,
            value: text("tiles"),
        }],
        "x",
    )
    .unwrap();
    assert_eq!(kept(&e), text("paint"));
}
