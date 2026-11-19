// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use ddc_geom::csg::BoolOp;
use ddc_geom::sketch_dims::ProfileSketch;
use ddc_geom::sketch_solve::Geo;
use serde_json::json;

fn s(x: &str) -> String {
    x.to_string()
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L0", 0.0).unwrap();
    (m, l)
}

fn rect(w: &str, h: &str) -> Kind {
    Kind::Sketch {
        plane: PlaneKind::Xy,
        offset: s("0"),
        shape: Shape::Rect {
            cx: s("0"),
            cy: s("0"),
            w: s(w),
            h: s(h),
        },
    }
}

/// Width = 2*Depth plate, extruded H, top-front edge chamfered by C (nodes 1, 2, 3).
fn block() -> Timeline {
    let mut t = Timeline::default();
    t.set_param("Depth", "1m").unwrap();
    t.set_param("Width", "2*Depth").unwrap();
    t.set_param("H", "3m").unwrap();
    t.set_param("C", "100mm").unwrap();
    let sk = t.add("", rect("Width", "Depth")).unwrap();
    let e = t
        .add("", Kind::Extrude {
            sketch: sk,
            dist: s("H"),
            start: s("0"),
        })
        .unwrap();
    t.add("", Kind::Modify {
        op: s("chamfer"),
        input: e,
        faces: vec![ftag(e, 1), ftag(e, 2)],
        args: vec![(s("d"), s("C"))],
    })
    .unwrap();
    t
}

fn vol(m: &Model, id: i64) -> f64 {
    m.mass_body(id).unwrap().volume()
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9 * b.abs().max(1.0), "{a} != {b}");
}

#[test]
fn timeline_round_trips_through_json() {
    let mut t = block();
    let mut ps = ProfileSketch::default();
    let p: Vec<usize> = [(0.0, 0.0), (1.0, 0.0), (1.0, 0.5), (0.0, 0.5)]
        .iter()
        .map(|q| ps.point(q.0, q.1))
        .collect();
    for i in 0..4 {
        ps.seg(p[i], p[(i + 1) % 4], i == 3);
    }
    ps.rule(Geo::Horizontal((p[0], p[1])));
    ps.rule(Geo::EqualGaps(vec![(p[0], p[1]), (p[2], p[3])]));
    ps.place_dim(p[0], p[1], [0.5, -0.5]).unwrap();
    let solved = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Yz,
            offset: s("-2m"),
            shape: Shape::Solved(ps),
        })
        .unwrap();
    let c = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Xz,
            offset: s("0"),
            shape: Shape::Circle {
                cx: s("1m"),
                cy: s("0.5m"),
                r: s("0.25m"),
                seg: 12,
            },
        })
        .unwrap();
    let pl = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Xy,
            offset: s("1m"),
            shape: Shape::Poly(vec![(s("0"), s("0")), (s("1m"), s("0")), (s("0"), s("H/3"))]),
        })
        .unwrap();
    let rv = t
        .add("", Kind::Revolve {
            sketch: c,
            angle: s("90deg"),
            seg: 16,
        })
        .unwrap();
    t.add("", Kind::Sweep {
        sketch: solved,
        path: vec![[s("0"), s("0"), s("0")], [s("0"), s("0"), s("H")]],
    })
    .unwrap();
    t.add("", Kind::Loft {
        sketches: vec![1, pl],
    })
    .unwrap();
    let mv = t
        .add("", Kind::Move {
            input: rv,
            d: [s("1m"), s("-Depth"), s("0")],
            turn: s("15deg"),
        })
        .unwrap();
    t.add("", Kind::Boolean {
        op: BoolOp::Intersect,
        a: 3,
        b: mv,
    })
    .unwrap();
    t.suppress(rv, true).unwrap();
    t.set_marker(Some(mv)).unwrap();
    let v = to_json(&t);
    let back = from_json(&serde_json::from_str(&v.to_string()).unwrap()).unwrap();
    assert_eq!(back, t);
    let f = Form::Timeline(Box::new(t));
    assert_eq!(Form::from_json(&f.to_json()).unwrap(), f);
    // a broken structure is refused on load
    let mut bad = to_json(&block());
    bad["nodes"][1]["def"]["sketch"] = json!(9);
    assert!(from_json(&bad).is_err());
}

#[test]
fn parameter_edit_rebuilds_and_undo_restores() {
    let (mut m, l) = model();
    let id = m.add_mass_timeline(l, "Block", block()).unwrap();
    let v0 = vol(&m, id);
    near(v0, 2.0 * 1.0 * 3.0 - 0.005 * 2.0);
    m.edit_mass_timeline(id, |t| t.set_param("Depth", "1.5m")).unwrap();
    near(vol(&m, id), 3.0 * 1.5 * 3.0 - 0.005 * 3.0);
    m.undo().unwrap();
    near(vol(&m, id), v0);
    let t = m.mass_timeline(id).unwrap().unwrap();
    assert_eq!(t, block());
}

#[test]
fn refused_edits_change_nothing() {
    let (mut m, l) = model();
    let id = m.add_mass_timeline(l, "Block", block()).unwrap();
    let n = m.journal_len().unwrap();
    assert!(m.edit_mass_timeline(id, |t| t.set_param("Depth", "Width/2")).is_err());
    assert!(m.edit_mass_timeline(id, |t| t.move_to(2, 0)).is_err());
    // a timeline that builds nothing is refused too
    assert!(m.edit_mass_timeline(id, |t| t.suppress(1, true)).is_err());
    assert_eq!(m.journal_len().unwrap(), n);
    assert_eq!(m.mass_timeline(id).unwrap().unwrap(), block());
}

#[test]
fn suppress_and_unsuppress_are_undo_steps() {
    let (mut m, l) = model();
    let id = m.add_mass_timeline(l, "Block", block()).unwrap();
    let v0 = vol(&m, id);
    m.edit_mass_timeline(id, |t| t.suppress(3, true)).unwrap();
    near(vol(&m, id), 6.0);
    let r = m.mass_timeline_build(id).unwrap().unwrap();
    assert_eq!(r.status(3), Some(&FeatureStatus::Suppressed));
    m.edit_mass_timeline(id, |t| t.suppress(3, false)).unwrap();
    near(vol(&m, id), v0);
    m.undo().unwrap();
    near(vol(&m, id), 6.0);
    m.undo().unwrap();
    near(vol(&m, id), v0);
}

#[test]
fn direct_edits_on_top_keep_the_timeline_editable() {
    let (mut m, l) = model();
    let id = m.add_mass_timeline(l, "Block", block()).unwrap();
    // the timeline takes ops 1..=3, the push/pull gets op 4
    assert_eq!(m.mass(id).unwrap().form.op_kinds(), vec!["sketch", "extrude", "chamfer"]);
    assert_eq!(m.next_mass_op(id).unwrap(), 4);
    let v0 = vol(&m, id);
    m.edit_mass_form(id, |b| Form::PushPull {
        base: b,
        face: ftag(2, 0),
        d: 0.5,
        keys: vec![],
    })
    .unwrap();
    let v1 = vol(&m, id);
    near((v1 - v0).abs(), 2.0 * 1.0 * 0.5);
    m.edit_mass_timeline(id, |t| t.set_param("H", "4m")).unwrap();
    near(vol(&m, id) - v1, 2.0 * 1.0 * 1.0);
    assert!(matches!(m.mass(id).unwrap().form, Form::PushPull { .. }));
}

#[test]
fn plain_history_converts_to_an_equal_timeline() {
    let (mut m, l) = model();
    let ring = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 2.0), (0.0, 2.0)];
    // stable edge ids out of order: side of ring edge 0 is local 2 + 3
    let form = Form::Fillet {
        base: Box::new(Form::Bool {
            op: BoolOp::Subtract,
            a: Box::new(Form::Extrude {
                ring: ring.clone(),
                z0: 0.0,
                h: 3.0,
                ids: vec![3, 1, 0, 2],
            }),
            b: Box::new(Form::Move {
                base: Box::new(Form::Cuboid {
                    lo: (0.0, 0.0, -1.0),
                    hi: (1.0, 1.0, 4.0),
                }),
                d: (1.5, 0.5, 0.0),
            }),
        }),
        faces: (ftag(1, 1), ftag(1, 2 + 3)),
        r: 0.2,
        seg: 6,
    };
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "Plain".into(),
            void: false,
            inplace: true,
            form,
        })
        .unwrap();
    let v0 = vol(&m, id);
    m.mass_to_timeline(id).unwrap();
    near(vol(&m, id), v0);
    let t = m.mass_timeline(id).unwrap().unwrap();
    let kinds: Vec<&str> = t.nodes.iter().map(|n| n.kind.name()).collect();
    assert_eq!(kinds, vec!["sketch", "extrude", "sketch", "extrude", "move", "boolean", "fillet"]);
    // the converted literals are now editable
    m.edit_mass_timeline(id, |t| t.set_arg(2, "dist", "4m", standard())).unwrap();
    assert!(vol(&m, id) > v0);
    m.undo().unwrap();
    m.undo().unwrap();
    assert!(matches!(m.mass(id).unwrap().form, Form::Fillet { .. }));
    near(vol(&m, id), v0);
}

#[test]
fn revolve_about_an_off_origin_axis_converts() {
    let (mut m, l) = model();
    let form = Form::Revolve {
        profile: vec![(0.5, 0.0), (1.0, 0.0), (1.0, 2.0), (0.5, 2.0)],
        axis: (3.0, -1.0),
        angle: std::f64::consts::PI,
        seg: 24,
        ids: vec![],
    };
    let t = form.to_timeline().unwrap();
    let mut c = Cache::default();
    let r = rebuild(&t, standard(), &mut c, 0).unwrap();
    let want = form.eval().unwrap();
    near(r.body.as_ref().unwrap().volume(), want.volume());
    assert_eq!(r.body.unwrap().bbox(), want.bbox());
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "Arch".into(),
            void: false,
            inplace: true,
            form,
        })
        .unwrap();
    m.mass_to_timeline(id).unwrap();
    assert!(Form::SweptBlend {
        p0: vec![],
        p1: vec![],
        path: vec![],
        ids: vec![],
    }
    .to_timeline()
    .is_err());
}

fn east_of(b: &Body, op: u32) -> u32 {
    b.polys
        .iter()
        .find(|p| tag_op(p.tag) == op && p.plane.n.x > 1.0 - 1e-9)
        .expect("an east face of the op")
        .tag
}

#[test]
fn stored_face_keys_keep_references_through_ten_edits() {
    let (mut m, l) = model();
    let mut t = Timeline::default();
    for (k, v) in [("W", "2m"), ("D", "1m"), ("H", "1m"), ("P", "500mm")] {
        t.set_param(k, v).unwrap();
    }
    let sk = t.add("", rect("W", "D")).unwrap();
    let e = t
        .add("", Kind::Extrude {
            sketch: sk,
            dist: s("H"),
            start: s("0"),
        })
        .unwrap();
    let pp = t
        .add("", Kind::Modify {
            op: s("push_pull"),
            input: e,
            faces: vec![ftag(e, 1)],
            args: vec![(s("d"), s("P"))],
        })
        .unwrap();
    let id = m.add_mass_timeline(l, "Keyed", t).unwrap();
    // the saved form carries the keys its first build made
    let keys = m.mass_timeline(id).unwrap().unwrap().node(pp).unwrap().keys.clone();
    assert_eq!(keys.len(), 4);
    let east = east_of(&m.mass_body(id).unwrap(), pp);
    let mut ch = 0;
    m.edit_mass_timeline(id, |t| {
        ch = t.add("", Kind::Modify {
            op: s("chamfer"),
            input: pp,
            faces: vec![ftag(e, 1), east],
            args: vec![(s("d"), s("100mm"))],
        })?;
        Ok(())
    })
    .unwrap();
    for i in 0..10 {
        let (w, p) = (2.0 + 0.25 * i as f64, 0.3 + 0.07 * i as f64);
        m.edit_mass_timeline(id, |t| {
            t.set_param("W", &format!("{w}m"))?;
            t.set_param("P", &format!("{p}m"))
        })
        .unwrap();
        let t = m.mass_timeline(id).unwrap().unwrap();
        assert_eq!(t.node(pp).unwrap().keys, keys, "edit {i}");
        let r = m.mass_timeline_build(id).unwrap().unwrap();
        assert!(r.status(ch).is_some_and(|s| s.ok()), "edit {i}: {:?}", r.first_error());
        assert_eq!(east_of(&m.mass_body(id).unwrap(), pp), east, "edit {i}");
        near(vol(&m, id), w * 1.0 * (1.0 + p) - 0.005 * 1.0);
    }
    // undo walks back through the same names
    m.undo().unwrap();
    assert_eq!(m.mass_timeline(id).unwrap().unwrap().node(pp).unwrap().keys, keys);
}

#[test]
fn keyed_direct_edits_convert_with_their_face_names() {
    let (mut m, l) = model();
    let pushed = Form::PushPull {
        base: Box::new(Form::Cuboid {
            lo: (0.0, 0.0, 0.0),
            hi: (2.0, 1.0, 1.0),
        }),
        face: ftag(1, 1),
        d: 0.5,
        keys: vec![],
    };
    let east = east_of(&pushed.eval().unwrap(), 2);
    let form = Form::Chamfer {
        base: Box::new(pushed),
        faces: (ftag(1, 1), east),
        d: 0.1,
    };
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "Pushed".into(),
            void: false,
            inplace: true,
            form,
        })
        .unwrap();
    let v0 = vol(&m, id);
    near(v0, 2.0 * 1.0 * 1.5 - 0.005);
    m.mass_to_timeline(id).unwrap();
    near(vol(&m, id), v0);
    let t = m.mass_timeline(id).unwrap().unwrap();
    let pp = t.nodes.iter().find(|n| n.kind.name() == "push_pull").unwrap();
    assert_eq!(pp.keys.len(), 4);
    // the push distance is now a parameter-ready literal; the chamfer stays on its edge
    let pid = pp.id;
    m.edit_mass_timeline(id, |t| t.set_arg(pid, "d", "800mm", standard())).unwrap();
    near(vol(&m, id), 2.0 * 1.0 * 1.8 - 0.005);
}

#[test]
fn a_family_defined_by_a_timeline_places_instances_with_their_own_values() {
    use ddc_geom::component::{Category, Value};
    let (mut m, l) = model();
    let mut t = Timeline::default();
    for (k, v) in [("W", "1m"), ("D", "W / 2"), ("H", "750mm")] {
        t.set_param(k, v).unwrap();
    }
    let sk = t.add("", rect("W", "D")).unwrap();
    t.add("", Kind::Extrude {
        sketch: sk,
        dist: s("H"),
        start: s("0"),
    })
    .unwrap();
    let fam = m.load_timeline_family("Desk block", Category::Furniture, &t).unwrap();
    assert_eq!(m.family_timeline(fam).unwrap(), Some(t.clone()));
    let ctype = m.comp_types(fam).unwrap()[0].0;
    let mut ids = vec![];
    for (x, w) in [(0.0, None), (3.0, Some(1.6)), (6.0, Some(2.4))] {
        let mut c = crate::CompSpec::at(l, ctype, x, 0.0);
        if let Some(w) = w {
            c.values = vec![(s("W"), Value::Num(w))];
        }
        ids.push((m.add_component(&c).unwrap(), w.unwrap_or(1.0)));
    }
    for (id, w) in ids {
        let q = m.component_quantities(id).unwrap();
        near(q.volume, w * (w / 2.0) * 0.75);
        near(q.bbox[1].1 - q.bbox[0].1, w / 2.0);
    }
    // the family's parameter set is the timeline's
    let names: Vec<String> = m.family(fam).unwrap().params.iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, vec![s("W"), s("D"), s("H")]);
}

fn slab_10_6_9() -> Form {
    Form::Extrude {
        ring: vec![(0., 0.), (10., 0.), (10., 6.), (0., 6.)],
        z0: 0.0,
        h: 9.0,
        ids: vec![],
    }
}

/// A mass with `top` on the 10 x 6 x 9 block, converted: same body, then one argument edit
/// gives `edited` and undo returns to the converted body. Returns the converted timeline.
fn converts(top: impl FnOnce(Box<Form>) -> Form, node: &str, arg: &str, text: &str, edited: f64) -> Timeline {
    let (mut m, l) = model();
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "M".into(),
            void: false,
            inplace: true,
            form: top(Box::new(slab_10_6_9())),
        })
        .unwrap();
    let direct = m.mass_body(id).unwrap();
    m.mass_to_timeline(id).unwrap();
    let t = m.mass_timeline(id).unwrap().unwrap();
    let conv = m.mass_body(id).unwrap();
    near(conv.volume(), direct.volume());
    assert_eq!(conv.polys.len(), direct.polys.len());
    let n = t.nodes.iter().find(|n| n.kind.name() == node).unwrap().id;
    let before = m.mass_timeline(id).unwrap();
    m.edit_mass_timeline(id, |t| t.set_arg(n, arg, text, standard())).unwrap();
    let r = m.mass_timeline_build(id).unwrap().unwrap();
    assert!(r.status(n).is_some_and(|s| s.ok()), "{:?}", r.first_error());
    assert!((vol(&m, id) - edited).abs() < 2e-2, "{} != {edited}", vol(&m, id));
    m.undo().unwrap();
    assert_eq!(m.mass_timeline(id).unwrap(), before);
    near(vol(&m, id), direct.volume());
    t
}

#[test]
fn region_pushes_convert_and_follow_their_distance() {
    let top = ftag(1, 1);
    let region = vec![(2., 2., 9.), (4., 2., 9.), (4., 4., 9.), (2., 4., 9.)];
    let r = region.clone();
    converts(
        |b| Form::PushTo {
            base: b,
            face: top,
            region: r,
            to: PushGoal::Dist(3.0),
            keys: vec![],
        },
        "push_region",
        "d",
        "5m",
        540.0 + 4.0 * 5.0,
    );
    // a pocket up to a point converts to its distance
    converts(
        |b| Form::PushTo {
            base: b,
            face: top,
            region,
            to: PushGoal::Point((0.0, 0.0, 7.0)),
            keys: vec![],
        },
        "push_region",
        "d",
        "-1m",
        540.0 - 4.0,
    );
    // the whole roof: its new sides are named by stored keys
    let t = converts(
        |b| Form::PushTo {
            base: b,
            face: top,
            region: vec![],
            to: PushGoal::Dist(2.0),
            keys: vec![],
        },
        "push_region",
        "d",
        "1m",
        600.0,
    );
    let pr = t.nodes.iter().find(|n| n.kind.name() == "push_region").unwrap();
    assert_eq!(pr.keys.len(), 4, "whole-face sides are named by keys");
}

#[test]
fn push_to_a_face_and_through_convert_and_follow_the_target() {
    let (mut m, l) = model();
    let cub = |lo: (f64, f64, f64), hi: (f64, f64, f64)| Box::new(Form::Cuboid { lo, hi });
    let form = Form::PushTo {
        base: Box::new(Form::Bool {
            op: BoolOp::Union,
            a: cub((0., 0., 0.), (2., 2., 1.)),
            b: cub((4., 0., 0.), (6., 2., 3.)),
        }),
        face: ftag(1, 1),
        region: vec![],
        to: PushGoal::Face(ftag(2, 1)),
        keys: vec![],
    };
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "Up to".into(),
            void: false,
            inplace: true,
            form,
        })
        .unwrap();
    near(vol(&m, id), 24.0);
    m.mass_to_timeline(id).unwrap();
    near(vol(&m, id), 24.0);
    // the target block grows: the push follows its top face
    let t = m.mass_timeline(id).unwrap().unwrap();
    let high = t.nodes.iter().filter(|n| n.kind.name() == "extrude").nth(1).unwrap().id;
    m.edit_mass_timeline(id, |t| t.set_arg(high, "dist", "5m", standard())).unwrap();
    near(vol(&m, id), 40.0);
    m.undo().unwrap();
    near(vol(&m, id), 24.0);

    let through = Form::PushTo {
        base: cub((0., 0., 0.), (2., 2., 1.)),
        face: ftag(1, 1),
        region: vec![(0.5, 0.5, 1.), (1.5, 0.5, 1.), (1.5, 1.5, 1.), (0.5, 1.5, 1.)],
        to: PushGoal::Through,
        keys: vec![],
    };
    let id = m
        .add_mass(&MassSpec {
            level: l,
            name: "Hole".into(),
            void: false,
            inplace: true,
            form: through,
        })
        .unwrap();
    m.mass_to_timeline(id).unwrap();
    near(vol(&m, id), 3.0);
    // a taller block: the hole still goes through
    let t = m.mass_timeline(id).unwrap().unwrap();
    let e = t.nodes.iter().find(|n| n.kind.name() == "extrude").unwrap().id;
    m.edit_mass_timeline(id, |t| t.set_arg(e, "dist", "2m", standard())).unwrap();
    near(vol(&m, id), 6.0);
}

#[test]
fn edge_blends_convert_and_follow_their_size() {
    let edges = vec![
        (ftag(1, 2), ftag(1, 3)),
        (ftag(1, 3), ftag(1, 4)),
        (ftag(1, 4), ftag(1, 5)),
        (ftag(1, 5), ftag(1, 2)),
    ];
    let e = edges.clone();
    converts(
        |b| Form::Blends {
            base: b,
            edges: e,
            round: true,
            size: 1.0,
            seg: 32,
        },
        "blend",
        "r",
        "500mm",
        540.0 - 4.0 * 9.0 * 0.25 * (1.0 - std::f64::consts::PI / 4.0),
    );
    converts(
        |b| Form::Blends {
            base: b,
            edges,
            round: false,
            size: 1.0,
            seg: 2,
        },
        "blend",
        "r",
        "2m",
        540.0 - 4.0 * 9.0 * 2.0,
    );
}

#[test]
fn draft_converts_and_follows_its_angle() {
    let t = 2f64.to_radians().tan();
    converts(
        |b| Form::Draft {
            base: b,
            faces: (2..6).map(|k| ftag(1, k)).collect(),
            at: (0., 0., 0.),
            pull: (0., 0., 1.),
            angle: 3f64.to_radians(),
        },
        "draft",
        "angle",
        "2deg",
        540.0 - 1296.0 * t + 972.0 * t * t,
    );
}
