//! Stable part names: text round trip, resolution to geometry, stale refs give reasons.

use ddc_ops::parts::FACE_LOCAL_BITS;
use ddc_ops::*;

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-9)
}

fn ref_(s: &str) -> Ref {
    Ref::parse(s).unwrap_or_else(|e| panic!("{e}"))
}

/// Unit cube made of tagged faces: op 1 faces 0..6 (bottom, top, then sides).
fn cube(z0: f64) -> Shape {
    let t = |local: u32| (1 << FACE_LOCAL_BITS) | local;
    let p = |x: f64, y: f64, z: f64| [x, y, z0 + z];
    Shape::Solid {
        faces: vec![
            TagFace { tag: t(0), pts: vec![p(0., 0., 0.), p(0., 1., 0.), p(1., 1., 0.), p(1., 0., 0.)] },
            TagFace { tag: t(1), pts: vec![p(0., 0., 1.), p(1., 0., 1.), p(1., 1., 1.), p(0., 1., 1.)] },
            TagFace { tag: t(2), pts: vec![p(0., 0., 0.), p(1., 0., 0.), p(1., 0., 1.), p(0., 0., 1.)] },
            TagFace { tag: t(3), pts: vec![p(1., 0., 0.), p(1., 1., 0.), p(1., 1., 1.), p(1., 0., 1.)] },
            TagFace { tag: t(4), pts: vec![p(1., 1., 0.), p(0., 1., 0.), p(0., 1., 1.), p(1., 1., 1.)] },
            TagFace { tag: t(5), pts: vec![p(0., 1., 0.), p(0., 0., 0.), p(0., 0., 1.), p(0., 1., 1.)] },
        ],
    }
}

/// Scratch model: wall 55 along +x, wall 56, slab 9 (square, clockwise on purpose), solid 4,
/// grid A.
fn scratch() -> ShapeMap {
    let mut m = ShapeMap::default();
    m.put(Cat::Wall, 55, Shape::Wall { a: [0., 0., 3.], b: [4., 0., 3.], thickness: 0.2, height: 2.5 })
        .put(Cat::Wall, 56, Shape::Wall { a: [4., 0., 0.], b: [4., 5., 0.], thickness: 0.3, height: 3.0 })
        .put(
            Cat::Slab,
            9,
            Shape::Slab { outline: vec![[0., 0.], [0., 6.], [6., 6.], [6., 0.]], top: 0.0, thickness: 0.25 },
        )
        .put(Cat::Solid, 4, cube(1.0))
        .put(Cat::Grid, 7, Shape::Line { a: [0., -1., 0.], b: [0., 9., 0.] })
        .put(Cat::Annotation, 3, Shape::Empty)
        .name(Cat::Grid, "A", 7);
    m.slots.insert(10, (Cat::Wall, 56));
    m
}

#[test]
fn ref_text_roundtrip_every_form() {
    let forms = [
        "wall:55",
        "wall:-2",
        "new:10",
        "set:north_side",
        "set:\"123\"",
        "set:\"east wing\"",
        "grid:A",
        "grid:A.1-b",
        "level:\"Level 1/2\"",
        "level:\"say \\\"hi\\\" \\\\ x\"",
        "level:\"\"",
        "type:\"42\"",
        "wall:55/face:left",
        "wall:55/face:right",
        "wall:55/face:top",
        "wall:55/face:bottom",
        "wall:55/face:start",
        "wall:55/face:end",
        "solid:4/face:1.0",
        "solid:4/face:7.4095",
        "slab:9/edge:3",
        "slab:9/edge:0/end:a",
        "wall:5/axis",
        "wall:5/axis/end:b",
        "new:10/face:left",
        "grid:A/end:a",
        "level:\"L 1\"/axis",
    ];
    for s in forms {
        let r = ref_(s);
        assert_eq!(r.to_string(), s, "print of {r:?}");
        assert_eq!(r.label(), s);
        assert_eq!(ref_(&r.to_string()), r);
        // The wire form round trips too.
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<Ref>(&j).unwrap(), r);
    }
    // Built refs print in the form that parses back to them.
    for r in [
        Ref::id(Cat::Wall, 55),
        Ref::name(Cat::Level, "Level 1"),
        Ref::name(Cat::Grid, "7"),
        Ref::name(Cat::Grid, "a\"b\\c"),
        Ref::Set { name: "a b".into() },
        Ref::part(Ref::part(Ref::slot(10), "edge:3"), "end:a"),
    ] {
        assert_eq!(ref_(&r.to_string()), r, "{r}");
    }
    assert_eq!(ref_("grid:7"), Ref::id(Cat::Grid, 7));
    assert_eq!(ref_("grid:A"), Ref::name(Cat::Grid, "A"));
    assert_eq!(ref_("solid:4/face:2.5"), Ref::part(Ref::id(Cat::Solid, 4), "face:2.5"));
}

#[test]
fn part_names_roundtrip() {
    for s in ["face:left", "face:right", "face:top", "face:bottom", "face:start", "face:end",
        "face:1.0", "face:3.17", "edge:0", "edge:12", "axis", "end:a", "end:b", "mid", "center"]
    {
        assert_eq!(PartName::parse(s).unwrap().to_string(), s);
    }
    assert_eq!(PartName::parse("face:2.5"), Ok(PartName::Face(FaceName::tag(2, 5))));
    for bad in ["", "face", "face:", "face:middle", "face:0.1", "face:1.4096", "face:1", "edge:-1",
        "edge:03", "edge:+3", "edge:x", "end:c", "axis:1", "corner:1"]
    {
        assert!(PartName::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn bad_ref_text_rejected() {
    for bad in ["", "wall", "wall:", "foo:1", "wall:55/", "wall:55//axis", "wall:55/face:middle",
        "level:\"open", "level:\"x\"y", "grid:a b", "new:x", "new:\"1\"", "set:12", "wall:55 /axis"]
    {
        let e = Ref::parse(bad).expect_err(bad);
        assert!(!e.to_string().is_empty());
    }
    let e = Ref::parse("wall:55/face:middle").unwrap_err();
    assert_eq!(e.at, 8);
}

#[test]
fn wall_face_ref_resolves_offset_plane() {
    let m = scratch();
    let PartGeo::Plane { origin, normal, loops } = resolve(&m, &ref_("wall:55/face:left")).unwrap() else {
        panic!("plane expected");
    };
    // Axis along +x: the left face is offset half the thickness to +y.
    assert!(close(origin, [0., 0.1, 3.]));
    assert!(close(normal, [0., 1., 0.]));
    assert_eq!(loops.len(), 1);
    assert!(loops[0].iter().all(|p| (p[1] - 0.1).abs() < 1e-9));
    assert!(loops[0].iter().any(|p| close(*p, [4., 0.1, 5.5])));

    let PartGeo::Plane { origin, normal, .. } = resolve(&m, &ref_("wall:55/face:right")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(origin, [0., -0.1, 3.]));
    assert!(close(normal, [0., -1., 0.]));

    let PartGeo::Plane { origin, normal, .. } = resolve(&m, &ref_("wall:55/face:top")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(origin, [0., 0., 5.5]));
    assert!(close(normal, [0., 0., 1.]));

    let PartGeo::Plane { normal, .. } = resolve(&m, &ref_("wall:55/face:end")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(normal, [1., 0., 0.]));

    // A slot-created wall resolves through the slot: wall 56 runs along +y, left is -x.
    let PartGeo::Plane { origin, normal, .. } = resolve(&m, &ref_("new:10/face:left")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(origin, [3.85, 0., 0.]));
    assert!(close(normal, [-1., 0., 0.]));

    assert_eq!(resolve(&m, &ref_("wall:55/axis")).unwrap(), PartGeo::Polyline(vec![[0., 0., 3.], [4., 0., 3.]]));
    assert_eq!(resolve(&m, &ref_("wall:55/end:b")).unwrap(), PartGeo::Point([4., 0., 3.]));
}

#[test]
fn slab_edge_ref_is_polyline() {
    let m = scratch();
    assert_eq!(
        resolve(&m, &ref_("slab:9/edge:3")).unwrap(),
        PartGeo::Polyline(vec![[6., 0., 0.], [0., 0., 0.]])
    );
    assert_eq!(resolve(&m, &ref_("slab:9/edge:3/end:b")).unwrap(), PartGeo::Point([0., 0., 0.]));
    // The outline is clockwise; the faces still come out counter-clockwise from outside.
    let PartGeo::Plane { normal, loops, .. } = resolve(&m, &ref_("slab:9/face:bottom")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(normal, [0., 0., -1.]));
    assert!(loops[0].iter().all(|p| (p[2] + 0.25).abs() < 1e-9));
    let e = resolve(&m, &ref_("slab:9/edge:4")).unwrap_err();
    assert_eq!(e, Unresolved::NoSuchEdge { r: "slab:9/edge:4".into(), index: 4, count: 4 });
    assert_eq!(resolve(&m, &ref_("slab:9/face:left")).unwrap_err().code(), "ref_part_not_on_kind");
    assert_eq!(resolve(&m, &ref_("slab:9")).unwrap_err().code(), "ref_needs_part");
}

#[test]
fn solid_faces_by_tag_and_role() {
    let m = scratch();
    let top = resolve(&m, &ref_("solid:4/face:top")).unwrap();
    assert_eq!(resolve(&m, &ref_("solid:4/face:1.1")).unwrap(), top);
    let PartGeo::Plane { normal, .. } = top else { panic!("plane expected") };
    assert!(close(normal, [0., 0., 1.]));
    let PartGeo::Plane { normal, .. } = resolve(&m, &ref_("solid:4/face:1.3")).unwrap() else {
        panic!("plane expected");
    };
    assert!(close(normal, [1., 0., 0.]));

    // Two top faces at the same height: the role name is ambiguous and lists the tags.
    let mut m2 = m.clone();
    let Shape::Solid { mut faces } = cube(0.0) else { unreachable!() };
    let mut extra = faces[1].clone();
    extra.tag = (2 << FACE_LOCAL_BITS) | 1;
    extra.pts.iter_mut().for_each(|p| p[0] += 5.0);
    faces.push(extra);
    m2.put(Cat::Solid, 4, Shape::Solid { faces });
    match resolve(&m2, &ref_("solid:4/face:top")).unwrap_err() {
        Unresolved::Ambiguous { tags, .. } => assert_eq!(tags, vec!["face:1.1", "face:2.1"]),
        e => panic!("{e}"),
    }
}

#[test]
fn grid_by_name_is_its_axis() {
    let m = scratch();
    let axis = PartGeo::Polyline(vec![[0., -1., 0.], [0., 9., 0.]]);
    assert_eq!(resolve(&m, &ref_("grid:A")).unwrap(), axis);
    assert_eq!(resolve(&m, &ref_("grid:A/axis")).unwrap(), axis);
    assert_eq!(resolve(&m, &ref_("grid:A/end:b")).unwrap(), PartGeo::Point([0., 9., 0.]));
    assert_eq!(resolve(&m, &ref_("grid:B")).unwrap_err().code(), "ref_no_name");
    assert_eq!(resolve(&m, &ref_("grid:A/face:left")).unwrap_err().code(), "ref_part_not_on_kind");
}

#[test]
fn names_stable_across_unrelated_edits() {
    let mut m = scratch();
    let refs = ["wall:55/face:left", "slab:9/edge:3", "solid:4/face:top", "solid:4/face:1.3", "grid:A"];
    let before: Vec<PartGeo> = refs.iter().map(|s| resolve(&m, &ref_(s)).unwrap()).collect();
    // Delete one wall, move another element, add a new one, reorder unrelated solid faces.
    m.delete(Cat::Wall, 56);
    m.put(Cat::Grid, 8, Shape::Line { a: [5., -1., 0.], b: [5., 9., 0.] }).name(Cat::Grid, "B", 8);
    m.put(Cat::Column, 1, Shape::Line { a: [1., 1., 0.], b: [1., 1., 3.] });
    if let Some(Shape::Solid { faces }) = m.shapes.get_mut(&(Cat::Solid, 4)) {
        faces.swap(0, 5);
        faces.swap(2, 4);
    }
    let after: Vec<PartGeo> = refs.iter().map(|s| resolve(&m, &ref_(s)).unwrap()).collect();
    assert_eq!(before, after);
}

#[test]
fn stale_face_ref_rejected() {
    let mut m = scratch();
    assert!(resolve(&m, &ref_("solid:4/face:1.3")).is_ok());
    // An edit of the solid removes the +x face (tag 1.3): the name is stale, with a reason.
    if let Some(Shape::Solid { faces }) = m.shapes.get_mut(&(Cat::Solid, 4)) {
        faces.retain(|f| f.tag != ((1 << FACE_LOCAL_BITS) | 3));
    }
    let e = resolve(&m, &ref_("solid:4/face:1.3")).unwrap_err();
    assert_eq!(e.code(), "ref_no_face");
    assert_eq!(e.to_string(), "solid:4/face:1.3: `face:1.3` is gone, an edit of this element removed it");
    // A face tag on a wall is not a wall face.
    assert_eq!(resolve(&m, &ref_("wall:55/face:1.3")).unwrap_err().code(), "ref_part_not_on_kind");
    // A part string that never parsed (built in code) is reported, not resolved.
    let e = resolve(&m, &Ref::part(Ref::id(Cat::Wall, 55), "face:middle")).unwrap_err();
    assert_eq!(e.code(), "ref_bad_part");
}

#[test]
fn stale_ref_after_delete_gives_reason() {
    let mut m = scratch();
    for s in ["wall:55/face:left", "wall:55/axis/end:a", "slab:9/edge:3", "solid:4/face:top"] {
        assert!(resolve(&m, &ref_(s)).is_ok(), "{s}");
    }
    m.delete(Cat::Wall, 55);
    m.delete(Cat::Slab, 9);
    m.delete(Cat::Solid, 4);
    m.delete(Cat::Grid, 7);
    m.slots.clear();
    for (s, code, root) in [
        ("wall:55/face:left", "ref_gone", "wall:55"),
        ("wall:55/axis/end:a", "ref_gone", "wall:55"),
        ("slab:9/edge:3", "ref_gone", "slab:9"),
        ("solid:4/face:top", "ref_gone", "solid:4"),
        ("grid:A", "ref_gone", "grid:A"),
        ("new:10/face:left", "ref_no_slot", "new:10"),
    ] {
        let e = resolve(&m, &ref_(s)).unwrap_err();
        assert_eq!(e.code(), code, "{s}");
        assert!(e.to_string().starts_with(root), "{e}");
    }
    assert_eq!(
        resolve(&m, &ref_("wall:55/face:left")).unwrap_err().to_string(),
        "wall:55 no longer exists in the model"
    );
    assert_eq!(resolve(&m, &ref_("annotation:3")).unwrap_err().code(), "ref_no_geometry");
    assert_eq!(resolve(&m, &ref_("set:north")).unwrap_err().code(), "ref_not_single");
}

#[test]
fn degenerate_geometry_is_a_reason() {
    let mut m = ShapeMap::default();
    m.put(Cat::Wall, 1, Shape::Wall { a: [1., 1., 0.], b: [1., 1., 0.], thickness: 0.2, height: 3.0 })
        .put(Cat::Slab, 2, Shape::Slab { outline: vec![[0., 0.], [1., 0.]], top: 0.0, thickness: 0.2 });
    assert_eq!(resolve(&m, &ref_("wall:1/face:left")).unwrap_err().code(), "ref_degenerate");
    assert_eq!(resolve(&m, &ref_("slab:2/edge:0")).unwrap_err().code(), "ref_degenerate");
}

#[test]
fn check_flags_bad_part_names() {
    let mut env = sample::envelope();
    assert!(env.check().is_empty());
    let g = &mut env.groups[0];
    g.ops.push(Op::Delete { target: Targets::one(Ref::part(Ref::id(Cat::Wall, 55), "face:middle")) });
    assert!(env.check().iter().any(|i| i.code == "bad_part"));
}
