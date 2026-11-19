// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::super::tests::{envelope, group, hash, model};
use ddc_ops::Code;
use super::*;

use ddc_ops::{Envelope, Op, Unit, Value};
use serde_json::json;

use crate::{BeamSpec, ColumnSpec, LayerSpec, MemberShape, ParamSpec, SlabSpec};

struct Fx {
    lv: i64,
    l2: i64,
    wt: i64,
    mt: i64,
    wall: i64,
    col: i64,
    beam: i64,
}

fn param(name: &str, ptype: ParamType, cats: &[&str]) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        ptype,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: cats.iter().map(|c| c.to_string()).collect(),
    }
}

fn fixture() -> (Model, Fx) {
    let mut m = model();
    let wt: i64 = m
        .sql()
        .query_row("SELECT id FROM wall_type WHERE name=?1", [super::super::tests::WT], |r| r.get(0))
        .unwrap();
    let lv = m.add_level("set-L1", 0.0).unwrap();
    let l2 = m.add_level("set-L2", 3.0).unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let mt = m
        .add_member_type("set-C300", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete")
        .unwrap();
    let col = m
        .add_column(&ColumnSpec {
            level: lv,
            top_level: Some(l2),
            member_type: mt,
            structural: true,
            at: (1.0, 1.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    let beam = m
        .add_beam(&BeamSpec {
            level: l2,
            member_type: mt,
            a: (0.0, 1.0, 0.0),
            b: (6.0, 1.0, 0.0),
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        })
        .unwrap();
    m.add_param(&param("Fire rating", ParamType::Text, &["wall", "col", "beam"])).unwrap();
    m.add_param(&param("Clear span", ParamType::Length, &["beam"])).unwrap();
    (m, Fx { lv, l2, wt, mt, wall, col, beam })
}

fn lm(v: f64) -> Value {
    Value::num(v, Unit::M)
}

fn fl(name: &str, v: Value) -> Field {
    Field::new(name, v)
}

fn set(cat: Cat, id: i64, fields: Vec<Field>) -> Op {
    Op::Set { target: Targets::one(Ref::id(cat, id)), fields }
}

fn one(ops: Vec<Op>) -> Envelope {
    envelope(vec![group(1, &[], ops)])
}

fn param_of(m: &Model, name: &str, kind: &str, id: i64) -> Option<PVal> {
    let d = m.param_by_name(name).unwrap().unwrap();
    m.param_value_of(&d, kind, id).unwrap()
}

fn wall_height(m: &Model, id: i64) -> (f64, f64) {
    m.sql()
        .query_row("SELECT ax, height FROM wall WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
}

/// Applies, checks one journal entry, runs `check`, undoes and checks the model is as before.
fn set_and_undo(m: &mut Model, env: &Envelope, check: impl FnOnce(&Model)) {
    let h0 = hash(m);
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(env).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    assert!(!r.changed.is_empty());
    check(m);
    m.undo().unwrap();
    assert_eq!(hash(m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
}

/// Like [`set_and_undo`] for an op that only creates: a fresh row is reported as created
/// only (287f0f26), so the receipt has one created ref and nothing changed.
fn create_and_undo(m: &mut Model, env: &Envelope, check: impl FnOnce(&Model)) {
    let h0 = hash(m);
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(env).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    assert_eq!(r.created.len(), 1);
    assert!(r.changed.is_empty(), "{:?}", r.changed);
    check(m);
    m.undo().unwrap();
    assert_eq!(hash(m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
}

#[test]
fn wall_set_and_undo() {
    let (mut m, fx) = fixture();
    let env = one(vec![set(
        Cat::Wall,
        fx.wall,
        vec![
            fl("height", Value::num(4500.0, Unit::Mm)),
            fl("ax", lm(0.5)),
            fl("Fire rating", Value::text("EI60")),
        ],
    )]);
    set_and_undo(&mut m, &env, |m| {
        let (ax, h) = wall_height(m, fx.wall);
        assert!((ax - 0.5).abs() < 1e-12 && (h - 4.5).abs() < 1e-12);
        assert_eq!(param_of(m, "Fire rating", "wall", fx.wall), Some(PVal::Text("EI60".into())));
    });
}

#[test]
fn column_set_and_undo() {
    let (mut m, fx) = fixture();
    let env = one(vec![set(
        Cat::Column,
        fx.col,
        vec![
            fl("x", lm(2.0)),
            fl("rotation", Value::num(90.0, Unit::Deg)),
            fl("structural", Value::plain(json!(false))),
            fl("top_level", Value::plain(serde_json::Value::Null)),
            fl("top_offset", lm(2.8)),
        ],
    )]);
    set_and_undo(&mut m, &env, |m| {
        let c = m.column(fx.col).unwrap();
        assert_eq!(c.at, (2.0, 1.0));
        assert!((c.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(!c.structural);
        assert_eq!(c.top_level, None);
        assert_eq!(c.top_offset, 2.8);
    });
}

#[test]
fn beam_set_and_undo() {
    let (mut m, fx) = fixture();
    let env = one(vec![set(
        Cat::Beam,
        fx.beam,
        vec![
            fl("az", lm(0.2)),
            fl("zjust", Value::text("neg")),
            fl("join_end", Value::text("miter")),
            fl("Clear span", Value::num(5800.0, Unit::Mm)),
        ],
    )]);
    set_and_undo(&mut m, &env, |m| {
        let b = m.beam(fx.beam).unwrap();
        assert_eq!(b.a.2, 0.2);
        assert_eq!(b.zjust, Just::Neg);
        assert_eq!(b.join_end, BeamJoin::Miter);
        assert_eq!(param_of(m, "Clear span", "beam", fx.beam), Some(PVal::Num(5.8)));
    });
}

#[test]
fn parameter_set_and_clear_and_undo() {
    let (mut m, fx) = fixture();
    m.apply_envelope(&one(vec![set(Cat::Column, fx.col, vec![fl("Fire rating", Value::text("R90"))])]))
        .unwrap();
    let env = one(vec![set(
        Cat::Column,
        fx.col,
        vec![fl("param.Fire rating", Value::plain(serde_json::Value::Null))],
    )]);
    set_and_undo(&mut m, &env, |m| {
        assert_eq!(param_of(m, "Fire rating", "col", fx.col), None);
    });
    assert_eq!(param_of(&m, "Fire rating", "col", fx.col), Some(PVal::Text("R90".into())));
}

#[test]
fn unknown_field_rejects_the_whole_envelope() {
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let env = envelope(vec![
        group(1, &[], vec![set(Cat::Wall, fx.wall, vec![fl("height", lm(5.0))])]),
        group(2, &[1], vec![set(Cat::Column, fx.col, vec![fl("colour", Value::text("red"))])]),
    ]);
    let e = m.apply_envelope(&env).unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert_eq!((e.group, e.op), (Some(2), Some(0)));
    assert!(e.to_string().contains("`colour`"), "{e}");
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
}

#[test]
fn bad_units_and_refs_name_the_field() {
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let cases = [
        (Cat::Wall, fx.wall, fl("height", Value::num(3.0, Unit::Deg)), "`height`", Code::Invalid),
        (Cat::Wall, fx.wall, fl("ax", Value::plain(json!(1.0))), "`ax`", Code::Invalid),
        (Cat::Column, fx.col, fl("rotation", lm(1.0)), "`rotation`", Code::Invalid),
        (Cat::Beam, fx.beam, fl("yjust", Value::text("left")), "`yjust`", Code::Invalid),
        (Cat::Beam, fx.beam, fl("member_type", Value::plain(json!(9999))), "`member_type`", Code::NotFound),
        (Cat::Beam, fx.beam, fl("Clear span", Value::num(2.0, Unit::M2)), "`Clear span`", Code::Invalid),
        (Cat::Wall, fx.wall, fl("param.Nope", Value::text("x")), "`param.Nope`", Code::NotFound),
    ];
    for (cat, id, f, name, code) in cases {
        let e = m.apply_envelope(&one(vec![set(cat, id, vec![f])])).unwrap_err();
        assert_eq!(e.code, code, "{name}: {e}");
        assert!(e.to_string().contains(name), "{name}: {e}");
    }
    assert_eq!(hash(&m), h0);
}

#[test]
fn field_named_like_a_parameter_is_refused_not_guessed() {
    let (mut m, fx) = fixture();
    m.add_param(&param("height", ParamType::Length, &["wall"])).unwrap();
    let h0 = hash(&m);
    let e = m
        .apply_envelope(&one(vec![set(Cat::Wall, fx.wall, vec![fl("height", lm(4.0))])]))
        .unwrap_err();
    assert!(e.to_string().contains("param.height"), "{e}");
    assert_eq!(hash(&m), h0);
    m.apply_envelope(&one(vec![set(Cat::Wall, fx.wall, vec![fl("param.height", lm(4.0))])]))
        .unwrap();
    assert_eq!(wall_height(&m, fx.wall).1, 3.0);
    assert_eq!(param_of(&m, "height", "wall", fx.wall), Some(PVal::Num(4.0)));
}

/// A slab, an opening in the fixture wall and a room, plus a second slab type: (slab, stype2, opening, room).
fn hosted(m: &mut Model, fx: &Fx) -> (i64, i64, i64, i64) {
    let layer = || vec![LayerSpec { material: "concrete".into(), thickness: 0.2 }];
    let st = m.add_slab_type("set-floor", &layer()).unwrap();
    let st2 = m.add_slab_type("set-floor2", &layer()).unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: fx.lv,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0., 0.), (6., 0.), (6., 4.), (0., 4.)],
            holes: vec![],
        })
        .unwrap();
    let opening = m
        .add_opening(
            fx.wall,
            &OpeningSpec { along: 1.0, width: 0.9, sill: 0.0, height: 2.1, kind: "door".into() },
        )
        .unwrap();
    let room = m
        .add_room(&RoomSpec {
            level: fx.lv,
            name: "Hall".into(),
            number: "101".into(),
            point: (2.0, 2.0),
            height: 3.0,
        })
        .unwrap();
    (slab, st2, opening, room)
}

#[test]
fn slab_opening_room_set_and_undo() {
    let (mut m, fx) = fixture();
    let (slab, st2, opening, room) = hosted(&mut m, &fx);
    let env = one(vec![
        set(Cat::Slab, slab, vec![fl("slab_type", Value::plain(json!(st2))), fl("offset", lm(-0.05))]),
        set(
            Cat::Opening,
            opening,
            vec![fl("width", Value::num(1200.0, Unit::Mm)), fl("kind", Value::text("window")), fl("sill", lm(0.9))],
        ),
        set(
            Cat::Room,
            room,
            vec![fl("name", Value::text("Lobby")), fl("number", Value::text("")), fl("level", Value::plain(json!(fx.l2)))],
        ),
    ]);
    set_and_undo(&mut m, &env, |m| {
        let (st, off): (i64, f64) = m
            .sql()
            .query_row("SELECT stype, offset FROM slab WHERE id=?1", [slab], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(st, st2);
        assert!((off + 0.05).abs() < 1e-12);
        let (w, sill, along, kind): (f64, f64, f64, String) = m
            .sql()
            .query_row("SELECT width, sill, along, kind FROM opening WHERE id=?1", [opening], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert!((w - 1.2).abs() < 1e-12 && (sill - 0.9).abs() < 1e-12 && (along - 1.0).abs() < 1e-12);
        assert_eq!(kind, "window");
        let (lv, name, number, h): (i64, String, String, f64) = m
            .sql()
            .query_row("SELECT level, name, number, height FROM room WHERE id=?1", [room], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!((lv, name.as_str(), number.as_str()), (fx.l2, "Lobby", ""));
        assert!((h - 3.0).abs() < 1e-12);
    });
}

fn swing_of(m: &Model, id: i64) -> (bool, bool, f64) {
    m.sql()
        .query_row("SELECT swing_side, swing_hinge, width FROM opening WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
}

#[test]
fn swing_only_row_one_edit() {
    let (mut m, fx) = fixture();
    let (_, _, opening, _) = hosted(&mut m, &fx);
    let (s0, h0, w0) = swing_of(&m, opening);
    let env = one(vec![set(Cat::Opening, opening, vec![fl("swing_side", Value::plain(json!(!s0)))])]);
    set_and_undo(&mut m, &env, |m| {
        assert_eq!(swing_of(m, opening), (!s0, h0, w0));
    });
    assert_eq!(swing_of(&m, opening), (s0, h0, w0));
}

fn kind_of(m: &Model, id: i64) -> String {
    m.sql().query_row("SELECT kind FROM opening WHERE id=?1", [id], |r| r.get(0)).unwrap()
}

#[test]
fn kind_door_to_window_receipt_is_window() {
    let (mut m, fx) = fixture();
    let (_, _, opening, _) = hosted(&mut m, &fx);
    let env = one(vec![set(Cat::Door, opening, vec![fl("kind", Value::plain(json!("window")))])]);
    let r = m.apply_envelope(&env).unwrap();
    assert_eq!(kind_of(&m, opening), "window");
    assert_eq!(r.changed, vec![Ref::id(Cat::Window, opening)]);
    let back = one(vec![set(Cat::Window, opening, vec![fl("kind", Value::plain(json!("door")))])]);
    assert_eq!(m.apply_envelope(&back).unwrap().changed, vec![Ref::id(Cat::Door, opening)]);
    let v = one(vec![set(Cat::Door, opening, vec![fl("kind", Value::plain(json!("void")))])]);
    assert_eq!(m.apply_envelope(&v).unwrap().changed, vec![Ref::id(Cat::Door, opening)]);
}

#[test]
fn geometry_and_swing_in_one_row() {
    let (mut m, fx) = fixture();
    let (_, _, opening, _) = hosted(&mut m, &fx);
    let (s0, h0, _) = swing_of(&m, opening);
    let env = one(vec![set(
        Cat::Door,
        opening,
        vec![fl("kind", Value::plain(json!("window"))), fl("width", lm(1.2)), fl("swing_side", Value::plain(json!(!s0)))],
    )]);
    let r = m.apply_envelope(&env).unwrap();
    assert_eq!(r.changed, vec![Ref::id(Cat::Window, opening)]);
    assert_eq!(swing_of(&m, opening), (!s0, h0, 1.2));
    assert_eq!(kind_of(&m, opening), "window");
}

#[test]
fn swing_bad_value_rejected() {
    let (mut m, fx) = fixture();
    let (_, _, opening, _) = hosted(&mut m, &fx);
    let h0 = hash(&m);
    for name in ["swing_side", "swing_hinge"] {
        let e = m
            .apply_envelope(&one(vec![set(Cat::Opening, opening, vec![fl(name, Value::text("left"))])]))
            .unwrap_err();
        assert_eq!(e.code, Code::Invalid, "{name}: {e}");
        assert!(e.to_string().contains(name), "{name}: {e}");
    }
    assert_eq!(hash(&m), h0);
}

#[test]
fn slab_opening_room_bad_values_name_the_field() {
    let (mut m, fx) = fixture();
    let (slab, _, opening, room) = hosted(&mut m, &fx);
    let h0 = hash(&m);
    let cases = [
        (Cat::Slab, slab, fl("slab_type", Value::plain(json!(9999))), "`slab_type`", Code::NotFound),
        (Cat::Slab, slab, fl("offset", Value::plain(json!(0.1))), "`offset`", Code::Invalid),
        (Cat::Opening, opening, fl("width", lm(0.0)), "`width`", Code::Invalid),
        (Cat::Opening, opening, fl("kind", Value::text("hatch")), "`kind`", Code::Invalid),
        (Cat::Opening, opening, fl("wall", Value::plain(json!(9999))), "`wall`", Code::NotFound),
        (Cat::Room, room, fl("height", lm(-1.0)), "`height`", Code::Invalid),
        (Cat::Room, room, fl("number", Value::plain(json!(101))), "`number`", Code::Invalid),
        (Cat::Room, room, fl("area", lm(1.0)), "`area`", Code::Unsupported),
    ];
    for (cat, id, f, name, code) in cases {
        let e = m.apply_envelope(&one(vec![set(cat, id, vec![f])])).unwrap_err();
        assert_eq!(e.code, code, "{name}: {e}");
        assert!(e.to_string().contains(name), "{name}: {e}");
    }
    assert_eq!(hash(&m), h0);
}

#[test]
fn type_targets_are_refused() {
    let (mut m, fx) = fixture();
    let e = m
        .apply_envelope(&one(vec![set(Cat::Type, fx.wt, vec![fl("Fire rating", Value::text("x"))])]))
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.to_string().contains("wtype:"), "{e}");
}

#[test]
fn set_slab_type_param() {
    let (mut m, _) = fixture();
    let st = m
        .add_slab_type("set-S200", &[crate::LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    let mut p = param("Fire class", ParamType::Text, &["slab"]);
    p.per_type = true;
    let pid = m.add_param(&p).unwrap();
    let env = one(vec![set(
        Cat::SlabType,
        st,
        vec![fl("Fire class", Value::text("REI90")), fl("name", Value::text("set-S200 fire"))],
    )]);
    set_and_undo(&mut m, &env, |m| {
        let vals = m.param_values(pid).unwrap();
        assert_eq!(vals.get(&("slab_type".to_owned(), st)), Some(&PVal::Text("REI90".into())));
        let (name, layers): (String, i64) = m
            .sql()
            .query_row(
                "SELECT name, (SELECT count(*) FROM slab_type_layer WHERE stype=?1) FROM slab_type WHERE id=?1",
                [st],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((name.as_str(), layers), ("set-S200 fire", 1));
    });
}

#[test]
fn set_member_type_name_unique_refused() {
    let (mut m, fx) = fixture();
    m.add_member_type("set-C400", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete").unwrap();
    let h0 = hash(&m);
    let e = m
        .apply_envelope(&one(vec![set(Cat::MemberType, fx.mt, vec![fl("name", Value::text("set-C400"))])]))
        .unwrap_err();
    assert_eq!(e.code, Code::Invalid);
    assert!(e.to_string().contains("`name`") && e.to_string().contains("set-C400"), "{e}");
    assert_eq!(hash(&m), h0);
    // its own name again is no clash
    m.apply_envelope(&one(vec![set(Cat::MemberType, fx.mt, vec![fl("name", Value::text("set-C300"))])]))
        .unwrap();
}

#[test]
fn unknown_type_field_rejected_with_name() {
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let e = m
        .apply_envelope(&one(vec![set(Cat::WallType, fx.wt, vec![fl("thickness", lm(0.3))])]))
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert!(e.to_string().contains("`thickness`") && e.to_string().contains("wtype:"), "{e}");
    assert_eq!(hash(&m), h0);
}

// Field lists exactly as `io/src/xlsx/ops_adapter.rs` writes them for `Edit::{Wall,Column,Beam}`.

fn opt_m(v: Option<f64>) -> Value {
    v.map_or_else(|| Value::plain(serde_json::Value::Null), lm)
}

fn adapter_wall(s: &WallSpec, bulge: f64) -> Vec<Field> {
    vec![
        fl("level", Value::plain(json!(s.level))),
        fl("ax", lm(s.a.0)),
        fl("ay", lm(s.a.1)),
        fl("bx", lm(s.b.0)),
        fl("by", lm(s.b.1)),
        fl("base_offset", lm(s.base_offset)),
        fl("height", lm(s.height)),
        fl("wall_type", Value::plain(json!(s.wall_type))),
        fl("offset0", opt_m(s.offset0)),
        fl("bulge", Value::plain(json!(bulge))),
    ]
}

fn adapter_column(s: &ColumnSpec) -> Vec<Field> {
    vec![
        fl("level", Value::plain(json!(s.level))),
        fl("top_level", s.top_level.map_or_else(|| Value::plain(serde_json::Value::Null), |t| Value::plain(json!(t)))),
        fl("member_type", Value::plain(json!(s.member_type))),
        fl("structural", Value::plain(json!(s.structural))),
        fl("x", lm(s.at.0)),
        fl("y", lm(s.at.1)),
        fl("base_offset", lm(s.base_offset)),
        fl("top_offset", lm(s.top_offset)),
        fl("rotation", Value::num(s.rotation, Unit::Rad)),
        fl("top_at_x", opt_m(s.top_at.map(|p| p.0))),
        fl("top_at_y", opt_m(s.top_at.map(|p| p.1))),
    ]
}

fn adapter_beam(s: &BeamSpec) -> Vec<Field> {
    let low = |x: String| Value::text(&x.to_lowercase());
    vec![
        fl("level", Value::plain(json!(s.level))),
        fl("member_type", Value::plain(json!(s.member_type))),
        fl("ax", lm(s.a.0)),
        fl("ay", lm(s.a.1)),
        fl("az", lm(s.a.2)),
        fl("bx", lm(s.b.0)),
        fl("by", lm(s.b.1)),
        fl("bz", lm(s.b.2)),
        fl("bulge", Value::plain(json!(s.bulge))),
        fl("yjust", low(format!("{:?}", s.yjust))),
        fl("zjust", low(format!("{:?}", s.zjust))),
        fl("rotation", Value::num(s.rotation, Unit::Rad)),
        fl("start_ext", lm(s.start_ext)),
        fl("end_ext", lm(s.end_ext)),
        fl("join_start", low(format!("{:?}", s.join_start))),
        fl("join_end", low(format!("{:?}", s.join_end))),
    ]
}

#[test]
fn xlsx_shaped_envelope_matches_the_import_path() {
    let (mut old, fx) = fixture();
    let (mut new, _) = fixture();
    let h0 = hash(&old);
    assert_eq!(hash(&new), h0);
    let wall = WallSpec {
        a: (0.0, 0.0),
        b: (7.25, 0.4),
        level: fx.l2,
        base_offset: 0.1,
        height: 2.7,
        wall_type: fx.wt,
        offset0: Some(0.05),
    };
    let mut col = old.column(fx.col).unwrap();
    col.at = (1.5, 2.0);
    col.rotation = 0.3;
    col.top_offset = -0.2;
    col.top_at = Some((1.7, 2.1));
    let mut beam = old.beam(fx.beam).unwrap();
    beam.b = (6.5, 1.0, 0.3);
    beam.yjust = Just::Neg;
    beam.join_start = BeamJoin::Cope;
    beam.start_ext = 0.15;
    assert_eq!(col.level, fx.lv);
    assert_eq!(beam.member_type, fx.mt);
    let edits = vec![
        Edit::Wall { id: fx.wall, spec: wall.clone(), bulge: 0.0 },
        Edit::Column { id: fx.col, spec: col.clone() },
        Edit::Beam { id: fx.beam, spec: beam.clone() },
        Edit::ParamValue {
            param: "Fire rating".into(),
            kind: "wall".into(),
            id: fx.wall,
            value: Some(PVal::Text("EI90".into())),
        },
        Edit::ParamValue {
            param: "Clear span".into(),
            kind: "beam".into(),
            id: fx.beam,
            value: Some(PVal::Num(6.2)),
        },
    ];
    old.apply_edits(&edits).unwrap();
    // groups as the adapter builds them: one per run of one kind, each after the previous
    let env = envelope(vec![
        group(1, &[], vec![set(Cat::Wall, fx.wall, adapter_wall(&wall, 0.0))]),
        group(2, &[1], vec![set(Cat::Column, fx.col, adapter_column(&col))]),
        group(3, &[2], vec![set(Cat::Beam, fx.beam, adapter_beam(&beam))]),
        group(
            4,
            &[3],
            vec![
                set(Cat::Wall, fx.wall, vec![fl("Fire rating", Value::text("EI90"))]),
                set(Cat::Beam, fx.beam, vec![fl("Clear span", Value::plain(json!(6.2)))]),
            ],
        ),
    ]);
    new.apply_envelope(&env).unwrap();
    assert_ne!(hash(&new), h0);
    assert_eq!(hash(&new), hash(&old));
    old.undo().unwrap();
    new.undo().unwrap();
    assert_eq!(hash(&old), h0);
    assert_eq!(hash(&new), h0);
}

// ---- type.rename / type.set (TypeOp) ----

fn type_op(t: ddc_ops::TypeOp) -> Envelope {
    one(vec![Op::Type(t)])
}

fn wall_type_row(m: &Model, wt: i64) -> (String, String, f64) {
    m.sql()
        .query_row(
            "SELECT t.name, l.material, l.thickness FROM wall_type t \
             JOIN wall_type_layer l ON l.wtype=t.id AND l.pos=0 WHERE t.id=?1",
            [wt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn type_rename_via_envelope() {
    use ddc_ops::TypeOp;
    let (mut m, fx) = fixture();
    let env = type_op(TypeOp::Rename { target: Ref::id(Cat::WallType, fx.wt), name: "set-W200 renamed".into() });
    set_and_undo(&mut m, &env, |m| {
        assert_eq!(wall_type_row(m, fx.wt).0, "set-W200 renamed");
    });
    // a bare v1 `type:N` still means a wall type here, and a name ref works too
    let env = type_op(TypeOp::Rename { target: Ref::id(Cat::Type, fx.wt), name: "set-W200 v1".into() });
    let r = m.apply_envelope(&env).unwrap();
    assert_eq!(r.changed, vec![Ref::id(Cat::WallType, fx.wt)]);
    let env = type_op(TypeOp::Rename { target: Ref::name(Cat::WallType, "set-W200 v1"), name: "set-W200 by name".into() });
    m.apply_envelope(&env).unwrap();
    assert_eq!(wall_type_row(&m, fx.wt).0, "set-W200 by name");
    // a clash names the `name` member of the op
    let other = m
        .add_wall_type("set-W300", &[crate::LayerSpec { material: "Brick".into(), thickness: 0.3 }])
        .unwrap();
    let h0 = hash(&m);
    let e = m
        .apply_envelope(&type_op(TypeOp::Rename { target: Ref::id(Cat::WallType, other), name: "set-W200 by name".into() }))
        .unwrap_err();
    assert_eq!(e.code, Code::Invalid);
    assert_eq!(e.path, "/groups/0/ops/0/name");
    assert_eq!(hash(&m), h0);
    // not a type row
    let e = m
        .apply_envelope(&type_op(TypeOp::Rename { target: Ref::id(Cat::Wall, fx.wall), name: "x".into() }))
        .unwrap_err();
    assert_eq!(e.code, Code::NotFound);
    assert_eq!(e.path, "/groups/0/ops/0/target");
}

#[test]
fn wall_type_layer_thickness_set() {
    use ddc_ops::TypeOp;
    let (mut m, fx) = fixture();
    let mut p = param("Acoustic class", ParamType::Text, &["wall"]);
    p.per_type = true;
    let pid = m.add_param(&p).unwrap();
    let env = type_op(TypeOp::Set {
        target: Ref::id(Cat::WallType, fx.wt),
        fields: vec![
            fl("layer.0.thickness", Value::num(250.0, Unit::Mm)),
            fl("layer.0.material", Value::text("Brick")),
            fl("name", Value::text("set-W250 brick")),
            fl("Acoustic class", Value::text("B")),
        ],
    });
    set_and_undo(&mut m, &env, |m| {
        let (name, material, thickness) = wall_type_row(m, fx.wt);
        assert_eq!((name.as_str(), material.as_str()), ("set-W250 brick", "Brick"));
        assert!((thickness - 0.25).abs() < 1e-12);
        let vals = m.param_values(pid).unwrap();
        assert_eq!(vals.get(&("wall_type".to_owned(), fx.wt)), Some(&PVal::Text("B".into())));
    });
    // the same fields through plain `set` on `wtype:` give the same rows
    let r = m
        .apply_envelope(&one(vec![set(Cat::WallType, fx.wt, vec![fl("layer.0.thickness", lm(0.3))])]))
        .unwrap();
    assert_eq!(r.changed, vec![Ref::id(Cat::WallType, fx.wt)]);
    assert!((wall_type_row(&m, fx.wt).2 - 0.3).abs() < 1e-12);
}

#[test]
fn bad_layer_index_rejected_with_path() {
    use ddc_ops::TypeOp;
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let bad = |field: Field| {
        type_op(TypeOp::Set {
            target: Ref::id(Cat::WallType, fx.wt),
            fields: vec![fl("name", Value::text("set-W200 ok")), field],
        })
    };
    let e = m.apply_envelope(&bad(fl("layer.3.thickness", lm(0.3)))).unwrap_err();
    assert_eq!(e.code, Code::NotFound);
    assert_eq!(e.path, "/groups/0/ops/0/fields/1");
    let t = e.to_string();
    assert!(t.contains("`layer.3.thickness`") && t.contains("layers: 0"), "{t}");
    let e = m.apply_envelope(&bad(fl("layer.x.thickness", lm(0.3)))).unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/fields/1"));
    let e = m.apply_envelope(&bad(fl("layer.0.color", Value::text("red")))).unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::Unsupported, "/groups/0/ops/0/fields/1"));
    let e = m.apply_envelope(&bad(fl("layer.0.thickness", lm(0.0)))).unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/fields/1"));
    // the valid rename in the same op did not land
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    // layers are wall type fields only
    let st = m
        .add_slab_type("set-S100", &[crate::LayerSpec { material: "Concrete".into(), thickness: 0.1 }])
        .unwrap();
    let h1 = hash(&m);
    let e = m
        .apply_envelope(&type_op(TypeOp::Set {
            target: Ref::id(Cat::SlabType, st),
            fields: vec![fl("layer.0.thickness", lm(0.3))],
        }))
        .unwrap_err();
    assert_eq!(e.code, Code::Unsupported);
    assert_eq!(hash(&m), h1);
}

// ---- type.duplicate ----

fn dup(slot: u32, from: Ref, name: &str, fields: Vec<Field>) -> Op {
    Op::Type(ddc_ops::TypeOp::Duplicate { slot, from, name: name.into(), fields })
}

fn wall_layer_rows(m: &Model, wt: i64) -> Vec<(String, f64)> {
    let mut st = m
        .sql()
        .prepare("SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos")
        .unwrap();
    let rows = st.query_map([wt], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    rows.map(|r| r.unwrap()).collect()
}

#[test]
fn type_duplicate_wall_copies_layers() {
    let (mut m, fx) = fixture();
    let env = one(vec![dup(1, Ref::id(Cat::WallType, fx.wt), "set-W200 copy", vec![])]);
    let before = wall_layer_rows(&m, fx.wt);
    create_and_undo(&mut m, &env, |m| {
        let new: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type WHERE name='set-W200 copy'", [], |r| r.get(0))
            .unwrap();
        assert_ne!(new, fx.wt);
        assert_eq!(wall_layer_rows(m, new), before);
        assert_eq!(wall_layer_rows(m, fx.wt), before);
    });
    // slab and member types copy too
    let st = m
        .add_slab_type("set-S100", &[LayerSpec { material: "Concrete".into(), thickness: 0.1 }])
        .unwrap();
    let r = m
        .apply_envelope(&one(vec![
            dup(1, Ref::id(Cat::SlabType, st), "set-S100 copy", vec![]),
            dup(2, Ref::id(Cat::MemberType, fx.mt), "set-C300 copy", vec![]),
        ]))
        .unwrap();
    assert_eq!(r.created.len(), 2);
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT count(*) FROM slab_type_layer l JOIN slab_type t ON t.id=l.stype \
             WHERE t.name='set-S100 copy' AND l.material='Concrete'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let (shape, mat): (String, String) = m
        .sql()
        .query_row("SELECT shape, material FROM member_type WHERE name='set-C300 copy'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(mat, "Concrete");
    let orig: String = m
        .sql()
        .query_row("SELECT shape FROM member_type WHERE id=?1", [fx.mt], |r| r.get(0))
        .unwrap();
    assert_eq!(shape, orig);
}

#[test]
fn type_duplicate_then_set_by_slot() {
    let (mut m, fx) = fixture();
    let new = Ref::New { slot: 1 };
    let env = one(vec![
        dup(
            1,
            Ref::id(Cat::WallType, fx.wt),
            "set-W250 copy",
            vec![fl("layer.0.thickness", Value::num(250.0, Unit::Mm))],
        ),
        Op::Type(ddc_ops::TypeOp::Set {
            target: new,
            fields: vec![fl("layer.0.material", Value::text("Brick"))],
        }),
    ]);
    let orig = wall_layer_rows(&m, fx.wt);
    set_and_undo(&mut m, &env, |m| {
        let id: i64 = m
            .sql()
            .query_row("SELECT id FROM wall_type WHERE name='set-W250 copy'", [], |r| r.get(0))
            .unwrap();
        let rows = wall_layer_rows(m, id);
        assert_eq!(rows[0].0, "Brick");
        assert!((rows[0].1 - 0.25).abs() < 1e-12);
        // the source type is untouched
        assert_eq!(wall_layer_rows(m, fx.wt), orig);
    });
}

#[test]
fn type_duplicate_name_taken_refused() {
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let taken: String = m
        .sql()
        .query_row("SELECT name FROM wall_type WHERE id=?1", [fx.wt], |r| r.get(0))
        .unwrap();
    let e = m
        .apply_envelope(&one(vec![dup(1, Ref::id(Cat::WallType, fx.wt), &taken, vec![])]))
        .unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/name"));
    // a blank name and a non-type source are refused too
    let e = m
        .apply_envelope(&one(vec![dup(1, Ref::id(Cat::WallType, fx.wt), "  ", vec![])]))
        .unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/name"));
    let e = m
        .apply_envelope(&one(vec![dup(1, Ref::id(Cat::Wall, fx.wall), "set-W x", vec![])]))
        .unwrap_err();
    assert_eq!((e.code, e.path.as_str()), (Code::NotFound, "/groups/0/ops/0/from"));
    // a bad field after the copy rolls the copy back
    let e = m
        .apply_envelope(&one(vec![dup(
            1,
            Ref::id(Cat::WallType, fx.wt),
            "set-W y",
            vec![fl("layer.9.thickness", lm(0.3))],
        )]))
        .unwrap_err();
    assert_eq!(e.code, Code::NotFound);
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
}

// ---- type.delete ----

fn del_type(target: Ref, replace_with: Option<Ref>) -> Op {
    Op::Type(ddc_ops::TypeOp::Delete { target, replace_with })
}

fn count(m: &Model, sql: &str) -> i64 {
    m.sql().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn spare_layers() -> Vec<LayerSpec> {
    vec![LayerSpec { material: "Concrete".into(), thickness: 0.2 }]
}

#[test]
fn type_delete_unused() {
    let (mut m, _fx) = fixture();
    let w = m.add_wall_type("set-W-spare", &spare_layers()).unwrap();
    let s = m.add_slab_type("set-S-spare", &spare_layers()).unwrap();
    let mt = m.add_member_type("set-M-spare", &MemberShape::Rect { b: 0.2, h: 0.2 }, "Steel").unwrap();
    for (cat, id, table) in [
        (Cat::WallType, w, "wall_type"),
        (Cat::SlabType, s, "slab_type"),
        (Cat::MemberType, mt, "member_type"),
    ] {
        let env = one(vec![del_type(Ref::id(cat, id), None)]);
        let h0 = hash(&m);
        let n0 = m.journal_len().unwrap();
        let r = m.apply_envelope(&env).unwrap();
        assert_eq!(r.deleted, vec![Ref::id(cat, id)]);
        assert_eq!(m.journal_len().unwrap(), n0 + 1);
        assert_eq!(count(&m, &format!("SELECT count(*) FROM {table} WHERE id={id}")), 0);
        m.undo().unwrap();
        assert_eq!(hash(&m), h0);
        assert_eq!(count(&m, &format!("SELECT count(*) FROM {table} WHERE id={id}")), 1);
    }
}

#[test]
fn type_delete_in_use_refused_with_count() {
    let (mut m, fx) = fixture();
    let (slab, _st2, _op, _room) = hosted(&mut m, &fx);
    let st: i64 = count(&m, &format!("SELECT stype FROM slab WHERE id={slab}"));
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    for (cat, id, want) in [
        (Cat::WallType, fx.wt, "1 wall"),
        (Cat::SlabType, st, "1 slab"),
        (Cat::MemberType, fx.mt, "2 instance(s)"),
    ] {
        let e = m.apply_envelope(&one(vec![del_type(Ref::id(cat, id), None)])).unwrap_err();
        assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/target"));
        let msg = e.got.clone().unwrap_or_default();
        assert!(msg.contains(want), "{msg}");
        assert!(msg.contains("replace_with"), "{msg}");
        assert_eq!(hash(&m), h0);
        assert_eq!(m.journal_len().unwrap(), n0);
    }
}

#[test]
fn type_delete_replace_retypes_one_undo() {
    let (mut m, fx) = fixture();
    let (slab, st2, _op, _room) = hosted(&mut m, &fx);
    let st: i64 = count(&m, &format!("SELECT stype FROM slab WHERE id={slab}"));
    let w2 = m.add_wall_type("set-W-other", &spare_layers()).unwrap();
    let m2 = m.add_member_type("set-M-other", &MemberShape::Rect { b: 0.4, h: 0.4 }, "Concrete").unwrap();
    let env = one(vec![
        del_type(Ref::id(Cat::WallType, fx.wt), Some(Ref::id(Cat::WallType, w2))),
        del_type(Ref::id(Cat::SlabType, st), Some(Ref::id(Cat::SlabType, st2))),
        del_type(Ref::id(Cat::MemberType, fx.mt), Some(Ref::id(Cat::MemberType, m2))),
    ]);
    set_and_undo(&mut m, &env, |m| {
        assert_eq!(count(m, &format!("SELECT wtype FROM wall WHERE id={}", fx.wall)), w2);
        assert_eq!(count(m, &format!("SELECT stype FROM slab WHERE id={slab}")), st2);
        assert_eq!(count(m, &format!("SELECT mtype FROM col WHERE id={}", fx.col)), m2);
        assert_eq!(count(m, &format!("SELECT mtype FROM beam WHERE id={}", fx.beam)), m2);
        assert_eq!(
            count(m, &format!("SELECT count(*) FROM wall_type WHERE id={}", fx.wt)),
            0
        );
        assert_eq!(count(m, &format!("SELECT count(*) FROM slab_type WHERE id={st}")), 0);
        assert_eq!(count(m, &format!("SELECT count(*) FROM member_type WHERE id={}", fx.mt)), 0);
    });
    assert_eq!(count(&m, &format!("SELECT wtype FROM wall WHERE id={}", fx.wall)), fx.wt);
    assert_eq!(count(&m, &format!("SELECT mtype FROM col WHERE id={}", fx.col)), fx.mt);
}

#[test]
fn type_delete_bad_replacement_refused() {
    let (mut m, fx) = fixture();
    let w2 = m.add_wall_type("set-W-other", &spare_layers()).unwrap();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let wt = Ref::id(Cat::WallType, fx.wt);
    // a type of another kind, the type itself, a missing row, a non-type target
    for (target, with, code, path) in [
        (wt.clone(), Ref::id(Cat::MemberType, fx.mt), Code::Invalid, "replace_with"),
        (wt.clone(), wt.clone(), Code::Invalid, "replace_with"),
        (wt.clone(), Ref::id(Cat::WallType, 99_999), Code::NotFound, "replace_with"),
        (Ref::id(Cat::Wall, fx.wall), Ref::id(Cat::WallType, w2), Code::NotFound, "target"),
    ] {
        let e = m.apply_envelope(&one(vec![del_type(target, Some(with))])).unwrap_err();
        assert_eq!((e.code, e.path.as_str()), (code, format!("/groups/0/ops/0/{path}").as_str()));
        assert_eq!(hash(&m), h0);
        assert_eq!(m.journal_len().unwrap(), n0);
    }
}

#[test]
fn type_delete_failure_later_in_envelope_rolls_back_retype() {
    let (mut m, fx) = fixture();
    let w2 = m.add_wall_type("set-W-other", &spare_layers()).unwrap();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let e = m
        .apply_envelope(&one(vec![
            del_type(Ref::id(Cat::WallType, fx.wt), Some(Ref::id(Cat::WallType, w2))),
            // the member type is still in use and has no replacement
            del_type(Ref::id(Cat::MemberType, fx.mt), None),
        ]))
        .unwrap_err();
    assert_eq!(e.code, Code::Invalid);
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    assert_eq!(count(&m, &format!("SELECT wtype FROM wall WHERE id={}", fx.wall)), fx.wt);
}

#[test]
fn layer_material_blank_refused() {
    use ddc_ops::TypeOp;
    let (mut m, fx) = fixture();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    for blank in ["   ", "", "\t\n"] {
        let env = type_op(TypeOp::Set {
            target: Ref::id(Cat::WallType, fx.wt),
            fields: vec![fl("name", Value::text("set-W200 ok")), fl("layer.0.material", Value::text(blank))],
        });
        let e = m.apply_envelope(&env).unwrap_err();
        assert_eq!((e.code, e.path.as_str()), (Code::Invalid, "/groups/0/ops/0/fields/1"));
        let t = e.to_string();
        assert!(t.contains("`layer.0.material` needs non-empty text"), "{t}");
        assert_eq!(hash(&m), h0);
        assert_eq!(m.journal_len().unwrap(), n0);
    }
}

#[test]
fn layer_material_trimmed_kept() {
    use ddc_ops::TypeOp;
    let (mut m, fx) = fixture();
    let env = type_op(TypeOp::Set {
        target: Ref::id(Cat::WallType, fx.wt),
        fields: vec![fl("layer.0.material", Value::text("  Brick  "))],
    });
    m.apply_envelope(&env).unwrap();
    assert_eq!(wall_type_row(&m, fx.wt).1, "Brick");
}

#[test]
fn set_name_two_types_refused() {
    let (mut m, fx) = fixture();
    let other = m
        .add_wall_type("set-W300", &[crate::LayerSpec { material: "Brick".into(), thickness: 0.3 }])
        .unwrap();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let both = Targets::Refs { refs: vec![Ref::id(Cat::WallType, fx.wt), Ref::id(Cat::WallType, other)] };
    let env = one(vec![
        set(Cat::Wall, fx.wall, vec![fl("height", lm(3.5))]),
        Op::Set { target: both, fields: vec![fl("name", Value::text("set-W same"))] },
    ]);
    let e = m.apply_envelope(&env).unwrap_err();
    assert_eq!(e.code, Code::Invalid);
    let t = e.to_string();
    assert!(t.contains("`name`") && t.contains("2 type rows"), "{t}");
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    // the same row listed twice is one rename
    let twice = Targets::Refs { refs: vec![Ref::id(Cat::WallType, fx.wt), Ref::id(Cat::WallType, fx.wt)] };
    m.apply_envelope(&one(vec![Op::Set { target: twice, fields: vec![fl("name", Value::text("set-W once"))] }]))
        .unwrap();
    assert_eq!(wall_type_row(&m, fx.wt).0, "set-W once");
}

#[test]
fn instance_param_on_type_row_refused() {
    let (mut m, fx) = fixture();
    m.add_param(&param("set-Mark", ParamType::Text, &["wall"])).unwrap();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    for field in ["set-Mark", "param.set-Mark"] {
        let env = one(vec![set(
            Cat::WallType,
            fx.wt,
            vec![fl("name", Value::text("set-W200 marked")), fl(field, Value::text("A1"))],
        )]);
        let e = m.apply_envelope(&env).unwrap_err();
        assert_eq!(e.code, Code::Invalid);
        let t = e.to_string();
        assert!(t.contains("`set-Mark`") && t.contains("instance parameter"), "{t}");
        assert_eq!(hash(&m), h0);
        assert_eq!(m.journal_len().unwrap(), n0);
    }
    // on the wall itself it lands
    m.apply_envelope(&one(vec![set(Cat::Wall, fx.wall, vec![fl("set-Mark", Value::text("A1"))])])).unwrap();
    assert_eq!(param_of(&m, "set-Mark", "wall", fx.wall), Some(PVal::Text("A1".into())));
}

fn type_param(m: &mut Model, name: &str, cats: &[&str]) {
    let mut p = param(name, ParamType::Text, cats);
    p.per_type = true;
    m.add_param(&p).unwrap();
}

/// One `set` with own fields, an instance and a type parameter: the type parameter lands on the
/// wall's type, the rest on the wall, all in one undo step (an XLSX row or an AI assign).
#[test]
fn mixed_type_and_instance_params_split_in_one_step() {
    let (mut m, fx) = fixture();
    type_param(&mut m, "Acoustic class", &["wall"]);
    let env = one(vec![set(
        Cat::Wall,
        fx.wall,
        vec![
            fl("height", lm(3.5)),
            fl("Fire rating", Value::text("EI60")),
            fl("param.Acoustic class", Value::text("B")),
        ],
    )]);
    set_and_undo(&mut m, &env, |m| {
        assert_eq!(wall_height(m, fx.wall).1, 3.5);
        assert_eq!(param_of(m, "Fire rating", "wall", fx.wall), Some(PVal::Text("EI60".into())));
        assert_eq!(param_of(m, "Acoustic class", "wall_type", fx.wt), Some(PVal::Text("B".into())));
        // no own value on the wall row; the element still shows its type's value
        let d = m.param_by_name("Acoustic class").unwrap().unwrap();
        assert_eq!(m.param_value(d.id, "wall", fx.wall).unwrap(), None);
        assert_eq!(param_of(m, "Acoustic class", "wall", fx.wall), Some(PVal::Text("B".into())));
    });
    let r = m.apply_envelope(&env).unwrap();
    assert!(r.changed.contains(&Ref::id(Cat::WallType, fx.wt)), "{:?}", r.changed);
    assert!(r.changed.contains(&Ref::id(Cat::Wall, fx.wall)), "{:?}", r.changed);
}

#[test]
fn type_param_on_elements_of_one_type_written_once() {
    let (mut m, fx) = fixture();
    type_param(&mut m, "Load class", &["col", "beam"]);
    let both = Targets::Refs { refs: vec![Ref::id(Cat::Column, fx.col), Ref::id(Cat::Beam, fx.beam)] };
    let env = one(vec![Op::Set {
        target: both,
        fields: vec![fl("Load class", Value::text("S2")), fl("Fire rating", Value::text("R90"))],
    }]);
    let r = m.apply_envelope(&env).unwrap();
    let mt = Ref::id(Cat::MemberType, fx.mt);
    assert_eq!(r.changed.iter().filter(|c| **c == mt).count(), 1, "{:?}", r.changed);
    assert_eq!(param_of(&m, "Load class", "member_type", fx.mt), Some(PVal::Text("S2".into())));
    assert_eq!(param_of(&m, "Fire rating", "col", fx.col), Some(PVal::Text("R90".into())));
    assert_eq!(param_of(&m, "Fire rating", "beam", fx.beam), Some(PVal::Text("R90".into())));
}

#[test]
fn type_param_with_type_change_refused() {
    let (mut m, fx) = fixture();
    type_param(&mut m, "Acoustic class", &["wall"]);
    let other = m.add_wall_type("set-W250", &[LayerSpec { material: "Brick".into(), thickness: 0.25 }]).unwrap();
    let h0 = hash(&m);
    let n0 = m.journal_len().unwrap();
    let env = one(vec![
        set(Cat::Wall, fx.wall, vec![fl("Fire rating", Value::text("EI30"))]),
        set(
            Cat::Wall,
            fx.wall,
            vec![fl("wall_type", Value::plain(json!(other))), fl("Acoustic class", Value::text("C"))],
        ),
    ]);
    let e = m.apply_envelope(&env).unwrap_err();
    assert_eq!(e.code, Code::Invalid);
    let t = e.to_string();
    assert!(t.contains("`Acoustic class`") && t.contains("`wall_type`"), "{t}");
    // the whole envelope is refused: the first op's edit is gone too, and no undo step is left
    assert_eq!(hash(&m), h0);
    assert_eq!(m.journal_len().unwrap(), n0);
    assert_eq!(param_of(&m, "Fire rating", "wall", fx.wall), None);
}

fn fsum(m: &Model, sql: &str) -> f64 {
    m.sql().query_row(sql, [], |r| r.get::<_, Option<f64>>(0)).unwrap().unwrap_or(0.0)
}

/// Deletes `from` replacing it with `to` (one undo step), checks the derived total moved off
/// `before` in the same step, and returns to `before` after undo.
fn retype_recomputes(m: &mut Model, cat: Cat, from: i64, to: i64, sql: &str, before: f64) -> f64 {
    let env = one(vec![del_type(Ref::id(cat, from), Some(Ref::id(cat, to)))]);
    let mut after = 0.0;
    set_and_undo(m, &env, |m| {
        after = fsum(m, sql);
        assert!((after - before).abs() > 1e-6, "derived data stale: {after} vs {before}");
    });
    assert!((fsum(m, sql) - before).abs() < 1e-9);
    after
}

#[test]
fn delete_slab_type_replace_recomputes_thickness() {
    let (mut m, fx) = fixture();
    let thin = m.add_slab_type("set-S-thin", &spare_layers()).unwrap();
    let thick = m
        .add_slab_type("set-S-thick", &[LayerSpec { material: "Concrete".into(), thickness: 0.35 }])
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: fx.lv,
            slab_type: thin,
            offset: 0.0,
            outer: vec![(0., 0.), (6., 0.), (6., 4.), (0., 4.)],
            holes: vec![],
        })
        .unwrap();
    let sql = format!("SELECT SUM(volume) FROM slab_qty WHERE slab={slab}");
    let before = fsum(&m, &sql);
    assert!((before - 24.0 * 0.2).abs() < 1e-9);
    let after = retype_recomputes(&mut m, Cat::SlabType, thin, thick, &sql, before);
    assert!((after - 24.0 * 0.35).abs() < 1e-9, "{after}");
}

#[test]
fn delete_column_type_replace_recomputes_section() {
    let (mut m, fx) = fixture();
    let big = m.add_member_type("set-C500", &MemberShape::Rect { b: 0.5, h: 0.5 }, "Concrete").unwrap();
    let sql = format!("SELECT SUM(volume) FROM member_qty WHERE kind='column' AND ref={}", fx.col);
    let before = fsum(&m, &sql);
    let after = retype_recomputes(&mut m, Cat::MemberType, fx.mt, big, &sql, before);
    assert!(after > before);
}

#[test]
fn delete_beam_type_replace_recomputes_section() {
    let (mut m, fx) = fixture();
    let big = m.add_member_type("set-B500", &MemberShape::Rect { b: 0.5, h: 0.5 }, "Concrete").unwrap();
    let sql = format!("SELECT SUM(volume) FROM member_qty WHERE kind='beam' AND ref={}", fx.beam);
    let before = fsum(&m, &sql);
    let after = retype_recomputes(&mut m, Cat::MemberType, fx.mt, big, &sql, before);
    assert!(after > before);
}

#[test]
fn delete_wall_type_replace_recomputes_quantities() {
    let (mut m, fx) = fixture();
    let thick = m
        .add_wall_type("set-W-thick", &[LayerSpec { material: "Concrete".into(), thickness: 0.77 }])
        .unwrap();
    let sql = format!("SELECT SUM(volume) FROM wall_qty WHERE wall={}", fx.wall);
    let before = fsum(&m, &sql);
    let after = retype_recomputes(&mut m, Cat::WallType, fx.wt, thick, &sql, before);
    assert!(after > before);
}

#[test]
fn delete_slab_type_replace_recomputes_roof() {
    let (mut m, fx) = fixture();
    let thin = m.add_slab_type("set-R-thin", &spare_layers()).unwrap();
    let thick = m
        .add_slab_type("set-R-thick", &[LayerSpec { material: "Concrete".into(), thickness: 0.35 }])
        .unwrap();
    let roof = m
        .add_roof(&crate::RoofSpec {
            level: fx.l2,
            slab_type: thin,
            offset: 0.0,
            overhang: 0.0,
            fascia: 0.2,
            kind: crate::RoofKind::Footprint {
                ring: vec![(0., 0.), (6., 0.), (6., 4.), (0., 4.)],
                slopes: vec![Some(30.0); 4],
                mansard: None,
            },
        })
        .unwrap();
    let sql = format!("SELECT SUM(volume) FROM roof_qty WHERE roof={roof}");
    let before = fsum(&m, &sql);
    let after = retype_recomputes(&mut m, Cat::SlabType, thin, thick, &sql, before);
    assert!(after > before);
}
