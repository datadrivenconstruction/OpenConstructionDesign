use super::*;
use crate::{LayerSpec, WallSpec};
use ddc_ops::{Actor, Envelope, Group};

/// Wall 10 m along x, 3 m high.
fn wall() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type(
            "W",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let w = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    (m, w)
}

fn opening(along: f64, width: f64, sill: f64, height: f64) -> OpeningSpec {
    OpeningSpec {
        along,
        width,
        sill,
        height,
        kind: "window".into(),
    }
}

#[test]
fn modify_error_maps_to_code() {
    let cases = [
        (ModifyError::Empty, Code::EmptySelection, None),
        (
            ModifyError::NotFound(Target::Wall(4)),
            Code::NotFound,
            Some(Ref::id(Cat::Wall, 4)),
        ),
        (
            ModifyError::Unsupported("rotate", Target::Opening(7)),
            Code::Unsupported,
            Some(Ref::id(Cat::Opening, 7)),
        ),
        (ModifyError::Parallel, Code::Parallel, None),
        (ModifyError::NotParallel, Code::NotParallel, None),
        (ModifyError::BadCount, Code::BadCount, None),
        (
            ModifyError::Degenerate(Target::Curtain(2)),
            Code::Degenerate,
            Some(Ref::id(Cat::CurtainWall, 2)),
        ),
        (
            ModifyError::OpeningOutside(9),
            Code::OutsideHost,
            Some(Ref::id(Cat::Opening, 9)),
        ),
        (
            ModifyError::ComponentOutside(3),
            Code::OutsideHost,
            Some(Ref::id(Cat::Component, 3)),
        ),
        (ModifyError::OffWall(5), Code::OffHost, Some(Ref::id(Cat::Wall, 5))),
        (ModifyError::NotFound(Target::View(1)), Code::NotFound, None),
    ];
    for (e, code, r) in cases {
        let what = format!("{e:?}");
        let rej = modify_rejection(&e);
        assert_eq!(rej.code, code, "{what}");
        assert_eq!(rej.hint_key, code.hint_key());
        assert_eq!(rej.refs, r.into_iter().collect::<Vec<_>>(), "{what}");
        assert_eq!(Error::Modify(e).rejection(), rej, "{what}");
    }
    let r = modify_rejection(&ModifyError::Unsupported("rotate", Target::Opening(7)));
    assert_eq!(r.got.as_deref(), Some("rotate"));
    assert_eq!(modify_rejection(&ModifyError::BadCount).expected.as_deref(), Some(">= 2"));
}

#[test]
fn modify_too_many_maps_to_code() {
    let r = modify_rejection(&ModifyError::TooMany(5));
    assert_eq!(r.code, Code::TooMany);
    assert_eq!(r.expected.as_deref(), Some("<= 5"));
    assert_eq!(r.hint_key, Code::TooMany.hint_key());
}

#[test]
fn model_errors_map_to_codes() {
    let by = vec![("opening", 3), ("dimension", 8)];
    let r = Error::Referenced { kind: "wall", id: 2, by }.rejection();
    assert_eq!((r.code, r.refs.clone()), (Code::Referenced, vec![Ref::id(Cat::Wall, 2)]));
    assert_eq!(r.got.as_deref(), Some("opening:3, dimension:8"));

    let r = Error::OpeningOutsideHost { kind: "slab", host: 6 }.rejection();
    assert_eq!((r.code, r.refs), (Code::OutsideHost, vec![Ref::id(Cat::Slab, 6)]));

    let r = Error::NewerVersion { found: 40, supported: 38 }.rejection();
    assert_eq!((r.code, r.expected.as_deref(), r.got.as_deref()), (Code::NewerFile, Some("<= 38"), Some("40")));

    for (text, code) in [
        ("no such wall 12", Code::NotFound),
        ("curtain wall: no level 4", Code::NotFound),
        ("opening: non-finite dimension", Code::NonFinite),
        ("opening kind 'hatch' (door/window/void)", Code::BadKind),
        ("curtain wall: 400\u{d7}300 cells", Code::TooMany),
        ("curtain wall: arc over a half circle", Code::Geometry),
        ("something new", Code::Invalid),
    ] {
        let r = Error::Invalid(text.into()).rejection();
        assert_eq!(r.code, code, "{text}");
        assert_eq!(r.got.as_deref(), Some(text));
    }
    use crate::sheets::SheetErr;
    for (e, code) in [
        (SheetErr::NoSuch("sheet"), Code::NotFound),
        (SheetErr::BadCut, Code::NonFinite),
        (SheetErr::BadPosition, Code::NonFinite),
        (SheetErr::BadCrop, Code::Invalid),
        (SheetErr::NumberTaken { number: "A1".into(), name: "x".into() }, Code::Conflict),
    ] {
        assert_eq!(Error::Sheet(e.clone()).rejection().code, code, "{e:?}");
    }
    assert_eq!(Error::NothingToUndo.rejection().code, Code::Internal);
    assert_eq!(Error::Conflict("lock 3".into()).rejection().code, Code::Conflict);
}

#[test]
fn out_of_range_states_expected_and_unit() {
    // head 1.0 + 2.5 over a 3 m wall: the sill may go up to 0.5 m
    let r = opening_fit(1, 10.0, 3.0, false, &opening(1.0, 1.2, 1.0, 2.5)).unwrap();
    assert_eq!((r.code, r.path.as_str()), (Code::OutOfRange, "/sill"));
    assert_eq!((r.expected.as_deref(), r.got.as_deref()), (Some("0..0.5 m"), Some("1 m")));
    assert_eq!(r.refs, vec![Ref::id(Cat::Wall, 1)]);

    let r = opening_fit(1, 10.0, 3.0, false, &opening(1.0, 1.2, 0.0, 3.4)).unwrap();
    assert_eq!((r.path.as_str(), r.expected.as_deref(), r.got.as_deref()), ("/height", Some("0..3 m"), Some("3.4 m")));

    let r = opening_fit(1, 10.0, 3.0, false, &opening(9.5, 1.2, 0.0, 2.0)).unwrap();
    assert_eq!((r.path.as_str(), r.expected.as_deref(), r.got.as_deref()), ("/along", Some("0..8.8 m"), Some("9.5 m")));

    let r = opening_fit(1, 10.0, 3.0, false, &opening(0.0, 12.0, 0.0, 2.0)).unwrap();
    assert_eq!((r.path.as_str(), r.expected.as_deref()), ("/width", Some("0..10 m")));

    // a top profile clips the head: only the sill bound remains
    assert!(opening_fit(1, 10.0, 3.0, true, &opening(1.0, 1.2, 1.0, 2.5)).is_none());
    let r = opening_fit(1, 10.0, 3.0, true, &opening(1.0, 1.2, -0.2, 2.5)).unwrap();
    assert_eq!((r.expected.as_deref(), r.got.as_deref()), (Some(">= 0 m"), Some("-0.2 m")));
}

#[test]
fn opening_fit_agrees_with_opening_fits() {
    let (m, w) = wall();
    for o in [
        opening(1.0, 1.2, 0.9, 1.5),
        opening(1.0, 1.2, 1.0, 2.5),
        opening(9.5, 1.2, 0.0, 2.0),
        opening(-0.1, 1.0, 0.0, 2.0),
        opening(0.0, 10.0, 0.0, 3.0),
        opening(2.0, 1.0, -0.1, 2.0),
    ] {
        let fits = m.opening_fits(w, &o).is_ok();
        let r = m.opening_fit_rejection(w, &o).unwrap();
        assert_eq!(fits, r.is_none(), "{o:?}");
        if let Some(r) = r {
            assert_eq!(r.refs, vec![Ref::id(Cat::Wall, w)]);
        }
    }
}

#[test]
fn rejection_path_points_to_field() {
    let (m, w) = wall();
    let mut env = Envelope::new(Actor::User, "windows");
    for id in [10, 20] {
        env.groups.push(Group {
            id,
            title: String::new(),
            ops: Vec::new(),
            depends_on: Vec::new(),
            why: String::new(),
        });
    }
    let r = m
        .opening_fit_rejection(w, &opening(1.0, 1.2, 0.0, 3.5))
        .unwrap()
        .unwrap()
        .under("/el")
        .in_op(&env, 1, 3);
    assert_eq!(r.path, "/groups/1/ops/3/el/height");
    assert_eq!((r.group, r.op), (Some(20), Some(3)));
    assert_eq!(r.expected.as_deref(), Some("0..3 m"));
}
