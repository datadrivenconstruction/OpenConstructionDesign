use super::tests::{envelope, group, hash, model, WT};
use super::*;

use ddc_ops::{Len, Unit, Value, P3, V3};

fn create_env(through: serde_json::Value) -> Envelope {
    let level = Op::Level(LevelOp::Create { slot: 1, name: "ops-L1".into(), elevation: Len::m(0.0) });
    let wall = Op::Create {
        slot: 2,
        el: NewEl::Wall {
            level: Ref::slot(1),
            a: Place::At { p: P3::m(0.0, 0.0, 0.0) },
            b: Place::At { p: P3::m(12.0, 0.0, 0.0) },
            height: Len::m(3.0),
            top: None,
            ty: Some(Ref::name(Cat::Type, WT)),
            fields: vec![Field::new("through", Value::plain(through))],
        },
    };
    envelope(vec![group(1, &[], vec![level]), group(2, &[1], vec![wall])])
}

fn one(op: Op) -> Envelope {
    envelope(vec![group(1, &[], vec![op])])
}

fn start(m: &Model, s: i64) -> (f64, f64) {
    let c = m.spline_wall(s).unwrap().axis;
    let p = c.point(c.knots()[0]);
    (p.x, p.y)
}

#[test]
fn spline_walls_are_made_and_edited_whole_through_envelopes() {
    let mut m = model();
    let n0 = m.journal_len().unwrap();
    let r = m.apply_envelope(&create_env(serde_json::json!([[4.0, 1.0], [8.0, -1.0]]))).unwrap();
    assert_eq!(m.journal_len().unwrap(), n0 + 1);
    let Some((_, Ref::Id { id: first, .. })) = r.created.iter().find(|(k, _)| *k == 2).cloned() else {
        panic!("no wall: {r:?}");
    };
    let s = m.spline_of_wall(first).unwrap().expect("a piece of a spline wall");
    let n = m.spline_wall_pieces(s).unwrap().len();
    assert!(n >= 2, "{n}");
    // any piece stands for the spline wall: its own fields rewrite it as a whole
    let last = *m.spline_wall_pieces(s).unwrap().last().unwrap();
    let set = |fields| Op::Set { target: Targets::one(Ref::id(Cat::Wall, last)), fields };
    m.apply_envelope(&one(set(vec![Field::new("height", Value::num(3.5, Unit::M))]))).unwrap();
    assert_eq!(m.spline_wall(s).unwrap().height, 3.5);
    let piece = m.spline_wall_pieces(s).unwrap()[0];
    let h: f64 = m.sql().query_row("SELECT height FROM wall WHERE id=?1", [piece], |r| r.get(0)).unwrap();
    assert_eq!(h, 3.5);
    let last = *m.spline_wall_pieces(s).unwrap().last().unwrap();
    let set = |fields| Op::Set { target: Targets::one(Ref::id(Cat::Wall, last)), fields };
    m.apply_envelope(&one(set(vec![Field::new("through", Value::plain(serde_json::json!([[6.0, 3.0]])))])))
        .unwrap();
    let c = m.spline_wall(s).unwrap().axis;
    let k = c.knots();
    let end = c.point(k[k.len() - 1]);
    assert!((end.x - 12.0).abs() < 1e-9 && end.y.abs() < 1e-9);
    assert_eq!(start(&m, s), (0.0, 0.0));
    // geometry of one piece is refused; nothing changes
    let (h0, j0) = (hash(&m), m.journal_len().unwrap());
    let last = *m.spline_wall_pieces(s).unwrap().last().unwrap();
    let bad = Op::Set {
        target: Targets::one(Ref::id(Cat::Wall, last)),
        fields: vec![Field::new("bulge", Value::plain(serde_json::json!(0.3)))],
    };
    assert!(m.apply_envelope(&one(bad)).is_err());
    assert_eq!((hash(&m), m.journal_len().unwrap()), (h0, j0));
    // a transform of one piece moves the whole spline wall
    let piece = m.spline_wall_pieces(s).unwrap()[0];
    let mv = Op::Transform {
        target: Targets::one(Ref::id(Cat::Wall, piece)),
        xf: Xform::Move { by: V3::m(1.0, 2.0, 0.0) },
        copy: false,
    };
    m.apply_envelope(&one(mv)).unwrap();
    let (x, y) = start(&m, s);
    assert!((x - 1.0).abs() < 1e-9 && (y - 2.0).abs() < 1e-9);
    // a new type and a delete through any piece
    let t2 = m.add_wall_type("ops-test-300", &[crate::LayerSpec { material: "Brick".into(), thickness: 0.3 }]).unwrap();
    let piece = m.spline_wall_pieces(s).unwrap()[1];
    let st = Op::SetType { target: Targets::one(Ref::id(Cat::Wall, piece)), to: Ref::id(Cat::WallType, t2) };
    m.apply_envelope(&one(st)).unwrap();
    assert_eq!(m.spline_wall(s).unwrap().wall_type, t2);
    let piece = m.spline_wall_pieces(s).unwrap()[1];
    m.apply_envelope(&one(Op::Delete { target: Targets::one(Ref::id(Cat::Wall, piece)) })).unwrap();
    assert!(m.spline_walls(None).unwrap().is_empty());
    let walls: i64 = m.sql().query_row("SELECT count(*) FROM wall", [], |r| r.get(0)).unwrap();
    assert_eq!(walls, 0);
    m.undo().unwrap();
    assert_eq!(m.spline_wall(s).unwrap().wall_type, t2);
}

#[test]
fn bad_spline_wall_fields_are_rejected_unchanged() {
    for through in [serde_json::json!([]), serde_json::json!([[1.0]]), serde_json::json!("x"), serde_json::json!([[6.0, "a"]])] {
        let mut m = model();
        let (h0, j0) = (hash(&m), m.journal_len().unwrap());
        let e = m.apply_envelope(&create_env(through.clone())).unwrap_err();
        assert!(!matches!(e.code, Code::Internal), "{through}: {e}");
        assert_eq!((hash(&m), m.journal_len().unwrap()), (h0, j0), "{through}");
    }
}
