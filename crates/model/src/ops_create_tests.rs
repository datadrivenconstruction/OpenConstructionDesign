use super::super::tests::{envelope, group, hash, model, WT};
use super::ROOM_HEIGHT;

use ddc_ops::{Angle, Cat, Code, End, Envelope, Field, Len, NewEl, Op, Place, Ref, Side, Unit, Value, P3};
use serde_json::json;

use crate::{LayerSpec, MemberShape, Model, WallSpec};

/// Level `L0` at 0, level `L1` at 3, one 6 m wall along x on L0, a slab type and a member type.
struct Fx {
    l0: i64,
    l1: i64,
    wall: i64,
    stype: i64,
    mtype: i64,
}

fn fixture() -> (Model, Fx) {
    let mut m = model();
    let l0 = m.add_level("L0", 0.0).unwrap();
    let l1 = m.add_level("L1", 3.0).unwrap();
    let wt: i64 = m.sql().query_row("SELECT id FROM wall_type WHERE name=?1", [WT], |r| r.get(0)).unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l0,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let stype = m
        .add_slab_type("ops-floor", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    let mtype = m.add_member_type("ops-C300", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete").unwrap();
    (m, Fx { l0, l1, wall, stype, mtype })
}

/// wall, along, width, sill, height, kind, swing_side, swing_hinge
type Row = (i64, f64, f64, f64, f64, String, bool, bool);

fn one(ops: Vec<Op>) -> Envelope {
    envelope(vec![group(1, &[], ops)])
}

fn m_(name: &str, v: f64) -> Field {
    Field::new(name, Value::num(v, Unit::M))
}

fn flag(name: &str, b: bool) -> Field {
    Field::new(name, Value::plain(json!(b)))
}

fn on_wall(fx: &Fx, centre: f64, z: f64) -> Place {
    Place::OnHost {
        host: Ref::id(Cat::Wall, fx.wall),
        along: Len::m(centre),
        from_end: End::A,
        side: Side::Center,
        z: Len::m(z),
    }
}

fn door(fx: &Fx, slot: u32, centre: f64, fields: Vec<Field>) -> Op {
    Op::Create {
        slot,
        el: NewEl::Door { host: Ref::id(Cat::Wall, fx.wall), at: on_wall(fx, centre, 0.0), ty: None, fields },
    }
}

fn door_size() -> Vec<Field> {
    vec![m_("width", 0.9), m_("height", 2.1)]
}

fn window(fx: &Fx, slot: u32, centre: f64) -> Op {
    Op::Create {
        slot,
        el: NewEl::Window {
            host: Ref::id(Cat::Wall, fx.wall),
            at: on_wall(fx, centre, 0.9),
            sill: Len::mm(900.0),
            ty: None,
            fields: vec![m_("width", 1.2), m_("height", 1.2)],
        },
    }
}

fn at(x: f64, y: f64, z: f64) -> Place {
    Place::At { p: P3::m(x, y, z) }
}

fn room(fx: &Fx, slot: u32, z: f64) -> Op {
    Op::Create {
        slot,
        el: NewEl::Room { level: Ref::id(Cat::Level, fx.l0), at: at(2.0, 1.0, z), name: "Hall".into(), number: Some("7".into()) },
    }
}

fn slab(fx: &Fx, slot: u32, pts: &[(f64, f64)], holes: serde_json::Value) -> Op {
    Op::Create {
        slot,
        el: NewEl::Slab {
            level: Ref::id(Cat::Level, fx.l0),
            boundary: pts.iter().map(|p| at(p.0, p.1, 0.0)).collect(),
            offset: Len::m(0.0),
            ty: Some(Ref::id(Cat::SlabType, fx.stype)),
            fields: vec![Field::new("holes", Value::plain(holes))],
        },
    }
}

fn column(fx: &Fx, slot: u32, top: Option<i64>, height: f64, fields: Vec<Field>) -> Op {
    Op::Create {
        slot,
        el: NewEl::Column {
            level: Ref::id(Cat::Level, fx.l0),
            at: at(1.0, 2.0, 0.0),
            top: top.map(|t| Ref::id(Cat::Level, t)),
            height: Len::m(height),
            rotation: Angle::rad(0.5),
            ty: Some(Ref::id(Cat::MemberType, fx.mtype)),
            fields,
        },
    }
}

fn created(r: &crate::Receipt, slot: u32) -> (Cat, i64) {
    match r.created.iter().find(|(s, _)| *s == slot) {
        Some((_, Ref::Id { cat, id })) => (*cat, *id),
        other => panic!("slot {slot}: {other:?} in {r:?}"),
    }
}

/// The envelope is refused with `code` and leaves the model and the journal as they were.
fn refused(m: &mut Model, env: &Envelope, code: Code) -> String {
    let (h0, n0) = (hash(m), m.journal_len().unwrap());
    let e = m.apply_envelope(env).unwrap_err();
    assert_eq!(e.code, code, "{e}");
    assert_eq!((hash(m), m.journal_len().unwrap()), (h0, n0), "a refused envelope changed the model: {e}");
    e.to_string()
}

#[test]
fn create_door_and_window_on_wall() {
    let (mut m, fx) = fixture();
    let mut f = door_size();
    f.push(flag("swing_side", true));
    let r = m.apply_envelope(&one(vec![door(&fx, 1, 2.0, f), window(&fx, 2, 4.0)])).unwrap();
    let (cat, d) = created(&r, 1);
    assert_eq!(cat, Cat::Door);
    let (cat, w) = created(&r, 2);
    assert_eq!(cat, Cat::Window);
    let row = |id: i64| -> Row {
        m.sql()
            .query_row(
                "SELECT wall,along,width,sill,height,kind,swing_side,swing_hinge FROM opening WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            )
            .unwrap()
    };
    let (wall, along, width, sill, height, kind, side, hinge) = row(d);
    assert_eq!((wall, kind.as_str(), side, hinge), (fx.wall, "door", true, false));
    // centre 2.0, width 0.9: the stored start is 1.55
    assert!((along - 1.55).abs() < 1e-9 && (width - 0.9).abs() < 1e-9, "{along} {width}");
    assert!(sill.abs() < 1e-9 && (height - 2.1).abs() < 1e-9);
    let (_, along, _, sill, _, kind, side, _) = row(w);
    assert_eq!((kind.as_str(), side), ("window", false));
    assert!((along - 3.4).abs() < 1e-9 && (sill - 0.9).abs() < 1e-9, "{along} {sill}");
}

#[test]
fn create_door_bad_input_rejected_unchanged() {
    let (mut m, fx) = fixture();
    // off the 6 m wall
    refused(&mut m, &one(vec![door(&fx, 1, 5.9, door_size())]), Code::OutsideHost);
    // head above the 3 m wall
    refused(&mut m, &one(vec![door(&fx, 1, 2.0, vec![m_("width", 0.9), m_("height", 3.5)])]), Code::OutsideHost);
    // no width
    let s = refused(&mut m, &one(vec![door(&fx, 1, 2.0, vec![m_("height", 2.1)])]), Code::Invalid);
    assert!(s.contains("width"), "{s}");
    // zero width, a plain number for a length, a field given twice
    refused(&mut m, &one(vec![door(&fx, 1, 2.0, vec![m_("width", 0.0), m_("height", 2.1)])]), Code::Invalid);
    let plain = Field::new("width", Value::plain(json!(0.9)));
    refused(&mut m, &one(vec![door(&fx, 1, 2.0, vec![plain, m_("height", 2.1)])]), Code::Invalid);
    let mut twice = door_size();
    twice.push(m_("width", 1.0));
    refused(&mut m, &one(vec![door(&fx, 1, 2.0, twice)]), Code::Invalid);
    // an unknown field and a door type are not lowered
    let mut extra = door_size();
    extra.push(m_("frame", 0.05));
    let s = refused(&mut m, &one(vec![door(&fx, 1, 2.0, extra)]), Code::Unsupported);
    assert!(s.contains("frame"), "{s}");
    let typed = Op::Create {
        slot: 1,
        el: NewEl::Door {
            host: Ref::id(Cat::Wall, fx.wall),
            at: on_wall(&fx, 2.0, 0.0),
            ty: Some(Ref::id(Cat::Type, 1)),
            fields: door_size(),
        },
    };
    refused(&mut m, &one(vec![typed]), Code::Unsupported);
    // a host that is not a wall
    let on_level = Op::Create {
        slot: 1,
        el: NewEl::Door {
            host: Ref::id(Cat::Level, fx.l0),
            at: on_wall(&fx, 2.0, 0.0),
            ty: None,
            fields: door_size(),
        },
    };
    refused(&mut m, &one(vec![on_level]), Code::NotFound);
}

#[test]
fn create_room_at_point() {
    let (mut m, fx) = fixture();
    let r = m.apply_envelope(&one(vec![room(&fx, 1, 0.0)])).unwrap();
    let (cat, id) = created(&r, 1);
    assert_eq!(cat, Cat::Room);
    let (level, name, number, px, py, h): (i64, String, String, f64, f64, f64) = m
        .sql()
        .query_row("SELECT level,name,number,px,py,height FROM room WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap();
    assert_eq!((level, name.as_str(), number.as_str(), px, py, h), (fx.l0, "Hall", "7", 2.0, 1.0, ROOM_HEIGHT));
    // z off the level is refused; an unknown level too
    refused(&mut m, &one(vec![room(&fx, 1, 1.5)]), Code::Invalid);
    let lost = Op::Create {
        slot: 1,
        el: NewEl::Room { level: Ref::id(Cat::Level, 999), at: at(1.0, 1.0, 0.0), name: "X".into(), number: None },
    };
    refused(&mut m, &one(vec![lost]), Code::NotFound);
}

#[test]
fn create_slab_with_hole() {
    let (mut m, fx) = fixture();
    let outer = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let hole = json!([[[1.0, 1.0], [1.0, 2.0], [2.0, 2.0], [2.0, 1.0]]]);
    let r = m.apply_envelope(&one(vec![slab(&fx, 1, &outer, hole)])).unwrap();
    let (cat, id) = created(&r, 1);
    assert_eq!(cat, Cat::Slab);
    let (level, stype): (i64, i64) =
        m.sql().query_row("SELECT level, stype FROM slab WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((level, stype), (fx.l0, fx.stype));
    let rings: Vec<i64> = m
        .sql()
        .prepare("SELECT count(*) FROM slab_vertex WHERE slab=?1 GROUP BY ring ORDER BY ring")
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rings, vec![4, 4]);
}

#[test]
fn create_slab_bad_input_rejected_unchanged() {
    let (mut m, fx) = fixture();
    let outer = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    // an open boundary: two points
    let s = refused(&mut m, &one(vec![slab(&fx, 1, &outer[..2], json!([]))]), Code::Invalid);
    assert!(s.contains("3 points"), "{s}");
    refused(&mut m, &one(vec![slab(&fx, 1, &outer, json!([[1.0, 2.0]]))]), Code::Invalid);
    refused(&mut m, &one(vec![slab(&fx, 1, &outer, json!("none"))]), Code::Invalid);
    // a slab type ref that is a wall type
    let Op::Create { el: NewEl::Slab { level, boundary, offset, fields, .. }, .. } = slab(&fx, 1, &outer, json!([])) else {
        unreachable!()
    };
    let wrong = Op::Create {
        slot: 1,
        el: NewEl::Slab { level, boundary, offset, ty: Some(Ref::id(Cat::WallType, 1)), fields },
    };
    refused(&mut m, &one(vec![wrong]), Code::NotFound);
}

#[test]
fn create_column_by_height_or_top_level() {
    let (mut m, fx) = fixture();
    let r = m
        .apply_envelope(&one(vec![
            column(&fx, 1, None, 2.8, vec![m_("base_offset", 0.1)]),
            column(&fx, 2, Some(fx.l1), 0.0, vec![m_("top_offset", -0.2), flag("structural", false)]),
        ]))
        .unwrap();
    let (cat, a) = created(&r, 1);
    assert_eq!(cat, Cat::Column);
    let (_, b) = created(&r, 2);
    let ca = m.column(a).unwrap();
    assert_eq!((ca.level, ca.top_level, ca.member_type, ca.structural), (fx.l0, None, fx.mtype, true));
    assert!((ca.base_offset - 0.1).abs() < 1e-9 && (ca.top_offset - 2.9).abs() < 1e-9, "{ca:?}");
    assert!((ca.rotation - 0.5).abs() < 1e-9 && ca.at == (1.0, 2.0) && ca.top_at.is_none());
    let cb = m.column(b).unwrap();
    assert_eq!((cb.top_level, cb.structural), (Some(fx.l1), false));
    assert!((cb.top_offset + 0.2).abs() < 1e-9);
}

#[test]
fn create_column_bad_input_rejected_unchanged() {
    let (mut m, fx) = fixture();
    // no height and no top level: top at the base
    refused(&mut m, &one(vec![column(&fx, 1, None, 0.0, vec![])]), Code::Invalid);
    // both a height and a top level
    refused(&mut m, &one(vec![column(&fx, 1, Some(fx.l1), 3.0, vec![])]), Code::Invalid);
    refused(&mut m, &one(vec![column(&fx, 1, None, -1.0, vec![])]), Code::Invalid);
    // half a slanted top
    refused(&mut m, &one(vec![column(&fx, 1, None, 3.0, vec![m_("top_at_x", 1.5)])]), Code::Invalid);
    refused(&mut m, &one(vec![column(&fx, 1, None, 3.0, vec![m_("width", 0.3)])]), Code::Unsupported);
}

/// A failing op after good creates rolls all of them back; a good envelope is one undo step.
#[test]
fn create_all_or_nothing_and_one_undo() {
    let (mut m, fx) = fixture();
    let outer = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
    let good = || {
        vec![
            door(&fx, 1, 2.0, door_size()),
            window(&fx, 2, 4.0),
            room(&fx, 3, 0.0),
            slab(&fx, 4, &outer, json!([])),
            column(&fx, 5, None, 3.0, vec![]),
        ]
    };
    let off_wall = door(&fx, 6, 5.9, door_size());
    refused(&mut m, &envelope(vec![group(1, &[], good()), group(2, &[1], vec![off_wall])]), Code::OutsideHost);

    let (h0, n0) = (hash(&m), m.journal_len().unwrap());
    let r = m.apply_envelope(&one(good())).unwrap();
    assert_eq!(r.created.len(), 5, "{r:?}");
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    m.undo().unwrap();
    assert_eq!(hash(&m), h0, "one undo removes every created row");
}
