use super::*;
use crate::csg::ftag;
use crate::sketch_solve::Geo;

fn reg() -> &'static Registry {
    standard()
}

fn s(x: &str) -> String {
    x.to_string()
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

/// Width = 2*Depth plate, extruded H, top-front edge chamfered by C.
fn block() -> (Timeline, NodeId, NodeId, NodeId) {
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
    let c = t
        .add("", Kind::Modify {
            op: s("chamfer"),
            input: e,
            faces: vec![ftag(e, 1), ftag(e, 2)],
            args: vec![(s("d"), s("C"))],
        })
        .unwrap();
    (t, sk, e, c)
}

fn vol(r: &Rebuild) -> f64 {
    r.body.as_ref().map_or(0.0, |b| b.volume())
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9 * b.abs().max(1.0), "{a} != {b}");
}

#[test]
fn expressions_drive_the_body() {
    let (t, ..) = block();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    assert!(r.nodes.iter().all(|n| n.status.ok()), "{:?}", r.nodes);
    near(vol(&r), 2.0 * 1.0 * 3.0 - 0.1 * 0.1 / 2.0 * 2.0);
    r.body.unwrap().validate(ops::TOL).unwrap();
}

#[test]
fn rebuild_after_param_change_matches_fresh_build() {
    let (mut t, ..) = block();
    let mut cache = Cache::default();
    rebuild(&t, reg(), &mut cache, 0).unwrap();
    for (name, val) in [("Depth", "1.5m"), ("H", "2500mm"), ("C", "0.2m"), ("Depth", "800mm")] {
        t.set_param(name, val).unwrap();
        let inc = rebuild(&t, reg(), &mut cache, 0).unwrap();
        let fresh = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
        assert_eq!(inc.body, fresh.body, "after {name} = {val}");
        let keys = |r: &Rebuild| r.nodes.iter().map(|n| (n.id, n.key)).collect::<Vec<_>>();
        assert_eq!(keys(&inc), keys(&fresh));
    }
    let d = 0.8;
    let (w, h, c) = (2.0 * d, 2.5, 0.2);
    near(vol(&rebuild(&t, reg(), &mut cache, 0).unwrap()), w * d * h - c * c / 2.0 * w);
}

#[test]
fn only_the_changed_node_and_its_dependents_rebuild() {
    let (mut t, sk, e, c) = block();
    let mut cache = Cache::default();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!((r.built, r.reused), (3, 0));
    t.set_param("C", "150mm").unwrap();
    assert_eq!(t.affected(&["C"], reg()), vec![c]);
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!((r.built, r.reused), (1, 2));
    assert_eq!(r.status(c), Some(&Status::Built));
    // Width follows Depth: the whole chain is dirty
    assert_eq!(t.affected(&["Depth"], reg()), vec![sk, e, c]);
    t.set_param("H", "4m").unwrap();
    assert_eq!(t.affected(&["H"], reg()), vec![e, c]);
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!((r.built, r.reused), (2, 1));
    // nothing changed: everything comes from the cache
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!((r.built, r.reused), (0, 3));
}

#[test]
fn expression_cycles_are_rejected_and_change_nothing() {
    let (mut t, ..) = block();
    let before = t.clone();
    let e = t.set_param("Depth", "Width / 2").unwrap_err();
    match e {
        FeatureError::Cycle(c) => {
            assert_eq!(c.first(), c.last());
            assert!(c.contains(&s("Width")) && c.contains(&s("Depth")), "{c:?}");
        }
        e => panic!("{e:?}"),
    }
    assert!(matches!(t.set_param("H", "H + 1m"), Err(FeatureError::Cycle(_))));
    // a longer loop through a new parameter
    t.set_param("A", "H / 3").unwrap();
    let after_a = t.clone();
    assert!(matches!(t.set_param("H", "A * 3"), Err(FeatureError::Cycle(_))));
    assert_eq!(t, after_a);
    assert_eq!(t.params.len(), before.params.len() + 1);
    // unknown names and bad syntax are refused too
    assert!(t.set_param("B", "Nope * 2").is_err());
    assert!(t.set_param("B", "2 *").is_err());
    assert!(t.set_param("2x", "1").is_err());
    assert_eq!(t, after_a);
}

#[test]
fn suppress_and_unsuppress() {
    let (mut t, sk, e, c) = block();
    let mut cache = Cache::default();
    let full = rebuild(&t, reg(), &mut cache, 0).unwrap();
    t.suppress(c, true).unwrap();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!(r.status(c), Some(&Status::Suppressed));
    near(vol(&r), 6.0);
    assert_eq!(r.leaves, vec![c]);
    t.suppress(c, false).unwrap();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!(r.body, full.body);
    assert_eq!(r.built, 0, "unsuppress reuses the cached chamfer");
    // a suppressed sketch takes its extrusion and the chamfer out
    t.suppress(sk, true).unwrap();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!(r.status(e), Some(&Status::Skipped(sk)));
    assert_eq!(r.status(c), Some(&Status::Skipped(e)));
    assert!(r.body.is_none());
}

#[test]
fn suppressed_tool_leaves_the_target() {
    let mut t = Timeline::default();
    let a = t.add("", rect("4m", "4m")).unwrap();
    let ea = t
        .add("", Kind::Extrude {
            sketch: a,
            dist: s("1m"),
            start: s("0"),
        })
        .unwrap();
    let b = t.add("", rect("1m", "1m")).unwrap();
    let eb = t
        .add("", Kind::Extrude {
            sketch: b,
            dist: s("3m"),
            start: s("-1m"),
        })
        .unwrap();
    let cut = t
        .add("", Kind::Boolean {
            op: BoolOp::Subtract,
            a: ea,
            b: eb,
        })
        .unwrap();
    let mut cache = Cache::default();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    near(vol(&r), 16.0 - 1.0);
    assert_eq!(r.leaves, vec![cut]);
    // boolean suppressed: target and tool both show
    t.suppress(cut, true).unwrap();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!(r.leaves, vec![eb, cut]);
    t.suppress(cut, false).unwrap();
    // tool sketch suppressed: the boolean forwards the target
    t.suppress(b, true).unwrap();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert_eq!(r.status(cut), Some(&Status::Skipped(eb)));
    near(vol(&r), 16.0);
}

#[test]
fn reorder_keeps_inputs_before_users() {
    let (mut t, sk, e, c) = block();
    assert!(matches!(t.move_to(e, 0), Err(FeatureError::Order { .. })));
    assert!(matches!(t.move_to(c, 1), Err(FeatureError::Order { .. })));
    let s2 = t.add("", rect("1m", "1m")).unwrap();
    t.move_to(s2, 0).unwrap();
    assert_eq!(t.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), vec![s2, sk, e, c]);
    // removing a used feature is refused, an unused one goes
    assert!(matches!(t.remove(e), Err(FeatureError::InUse(_))));
    t.remove(s2).unwrap();
    assert!(t.node(s2).is_none());
    // ids are not reused
    let s3 = t.add("", rect("1m", "1m")).unwrap();
    assert!(s3 > s2);
}

#[test]
fn roll_back_marker_stops_the_build_and_takes_new_features() {
    let (mut t, _, e, c) = block();
    t.set_marker(Some(e)).unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    assert_eq!(r.status(c), Some(&Status::Rolled));
    near(vol(&r), 6.0);
    // a feature added while rolled back goes right after the marker, which follows it
    let m = t
        .add("", Kind::Move {
            input: e,
            d: [s("1m"), s("0"), s("0")],
            turn: s("0"),
        })
        .unwrap();
    assert_eq!(t.index_of(m), Some(2));
    assert_eq!(t.marker, Some(m));
    t.set_marker(None).unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    assert_eq!(r.leaves, vec![m, c]);
}

#[test]
fn argument_edits_are_checked() {
    let (mut t, sk, e, c) = block();
    assert!(t.set_arg(e, "dist", "Missing + 1m", reg()).is_err());
    assert!(t.set_arg(e, "dist", "(", reg()).is_err());
    assert!(matches!(t.set_arg(e, "nope", "1m", reg()), Err(FeatureError::NoArg(_))));
    t.set_arg(sk, "w", "3*Depth", reg()).unwrap();
    // wrong dimension: refused by the rebuild with a message, the chamfer forwards its input
    t.set_arg(c, "d", "90deg", reg()).unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    assert!(matches!(r.status(c), Some(Status::Failed(m)) if m.contains("length")));
    near(vol(&r), 3.0 * 1.0 * 3.0);
    assert!(matches!(t.remove_param("Depth", reg()), Err(FeatureError::InUse(_))));
    t.set_param("Spare", "1").unwrap();
    t.remove_param("Spare", reg()).unwrap();
}

#[test]
fn revolve_on_a_vertical_sketch() {
    let mut t = Timeline::default();
    let sk = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Xz,
            offset: s("0"),
            shape: Shape::Rect {
                cx: s("1.5"),
                cy: s("0.5"),
                w: s("1"),
                h: s("1"),
            },
        })
        .unwrap();
    t.add("", Kind::Revolve {
        sketch: sk,
        angle: s("360"),
        seg: 64,
    })
    .unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    let n = 64.0;
    let want = n / 2.0 * (std::f64::consts::TAU / n).sin() * (4.0 - 1.0) * 1.0;
    near(vol(&r), want);
    let (lo, hi) = r.body.unwrap().bbox().unwrap();
    near(lo.z, 0.0);
    near(hi.z, 1.0);
}

#[test]
fn loft_and_sweep_build() {
    let mut t = Timeline::default();
    let a = t.add("", rect("2m", "2m")).unwrap();
    let b = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Xy,
            offset: s("3m"),
            shape: Shape::Rect {
                cx: s("0"),
                cy: s("0"),
                w: s("1m"),
                h: s("1m"),
            },
        })
        .unwrap();
    t.add("", Kind::Loft { sketches: vec![a, b] }).unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    // frustum of a square pyramid: h/3 (A1 + A2 + sqrt(A1 A2))
    near(vol(&r), 3.0 / 3.0 * (4.0 + 1.0 + 2.0));
    let mut t = Timeline::default();
    let p = t.add("", rect("0.2m", "0.1m")).unwrap();
    t.add("", Kind::Sweep {
        sketch: p,
        path: vec![[s("0"), s("0"), s("0")], [s("5m"), s("0"), s("0")]],
    })
    .unwrap();
    let r = rebuild(&t, reg(), &mut Cache::default(), 0).unwrap();
    near(vol(&r), 0.2 * 0.1 * 5.0);
}

#[test]
fn constrained_sketch_dimensions_follow_parameters() {
    let mut ps = crate::sketch_dims::ProfileSketch::default();
    let p: Vec<usize> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter()
        .map(|q| ps.point(q.0, q.1))
        .collect();
    for i in 0..4 {
        ps.seg(p[i], p[(i + 1) % 4], false);
    }
    ps.rule(Geo::Fix(p[0], 0.0, 0.0));
    ps.rule(Geo::Horizontal((p[0], p[1])));
    ps.rule(Geo::Horizontal((p[2], p[3])));
    ps.rule(Geo::Vertical((p[1], p[2])));
    ps.rule(Geo::Vertical((p[3], p[0])));
    let (dw, _) = ps.place_dim(p[0], p[1], [0.5, -0.4]).unwrap();
    let (dh, _) = ps.place_dim(p[1], p[2], [1.4, 0.5]).unwrap();
    let mut t = Timeline::default();
    t.set_param("D", "500mm").unwrap();
    t.set_param("W", "4*D").unwrap();
    let sk = t
        .add("", Kind::Sketch {
            plane: PlaneKind::Xy,
            offset: s("0"),
            shape: Shape::Solved(ps),
        })
        .unwrap();
    t.add("", Kind::Extrude {
        sketch: sk,
        dist: s("2m"),
        start: s("0"),
    })
    .unwrap();
    t.set_arg(sk, &format!("dim{dw}"), "W", reg()).unwrap();
    t.set_arg(sk, &format!("dim{dh}"), "D", reg()).unwrap();
    let mut cache = Cache::default();
    near(vol(&rebuild(&t, reg(), &mut cache, 0).unwrap()), 2.0 * 0.5 * 2.0);
    t.set_param("D", "1m").unwrap();
    near(vol(&rebuild(&t, reg(), &mut cache, 0).unwrap()), 4.0 * 1.0 * 2.0);
}

#[test]
fn rule_codec_round_trips() {
    let all = vec![
        Geo::Fix(1, 0.5, -2.0),
        Geo::FixX(2, 1.0),
        Geo::FixY(3, -1.0),
        Geo::Coincident(0, 4),
        Geo::Horizontal((0, 1)),
        Geo::Vertical((1, 2)),
        Geo::Dist(0, 1, 2.5),
        Geo::PointLine(3, (0, 1), 0.25),
        Geo::LineDist((0, 1), (2, 3), 1.0),
        Geo::Parallel((0, 1), (2, 3)),
        Geo::Perp((0, 1), (1, 2)),
        Geo::Angle((0, 1), (1, 2), 0.7),
        Geo::EqualLen((0, 1), (2, 3)),
        Geo::EqualGaps(vec![(0, 1), (2, 3), (4, 5)]),
        Geo::Midpoint(6, (0, 1)),
        Geo::DistX(0, 1, 1.5),
        Geo::DistY(0, 1, -1.5),
    ];
    for g in all {
        let (k, v) = codec::encode(&g);
        assert_eq!(codec::decode(k, &v), Some(g));
    }
    assert_eq!(codec::decode("dist", &[0.0, 1.5, 1.0]), None);
    assert_eq!(codec::decode("nope", &[]), None);
}

#[test]
fn registry_takes_new_operations() {
    struct Lift;
    impl FeatureOp for Lift {
        fn kind(&self) -> &'static str {
            "lift"
        }
        fn args(&self) -> &'static [ArgSpec] {
            &[ArgSpec {
                name: "dz",
                dim: crate::component::expr::LENGTH,
                default: "1m",
            }]
        }
        fn faces(&self) -> (usize, usize) {
            (0, 0)
        }
        fn build(&self, b: &crate::csg::Body, _: &[u32], a: &[f64], _: u32) -> Result<crate::csg::Body, String> {
            Ok(b.clone().translated(crate::math::v3(0.0, 0.0, a[0])))
        }
    }
    let reg = standard_ops().into_iter().fold(Registry::empty(), Registry::with).with(Box::new(Lift));
    assert!(reg.kinds().contains(&"lift") && reg.kinds().contains(&"fillet"));
    let (mut t, _, _, c) = block();
    let l = t
        .add("", Kind::Modify {
            op: s("lift"),
            input: c,
            faces: vec![],
            args: vec![],
        })
        .unwrap();
    t.set_arg(l, "dz", "2m", &reg).unwrap();
    let r = rebuild(&t, &reg, &mut Cache::default(), 0).unwrap();
    let (lo, _) = r.body.unwrap().bbox().unwrap();
    near(lo.z, 2.0);
    // the standard registry does not know it: reported, input forwarded
    let r = rebuild(&t, standard(), &mut Cache::default(), 0).unwrap();
    assert!(matches!(r.status(l), Some(Status::Failed(_))));
}

/// Tags of `b`'s faces made by op `op` whose normal is `n`.
fn faces_facing(b: &crate::csg::Body, op: u32, n: [f64; 3]) -> Vec<u32> {
    let mut v: Vec<u32> = b
        .polys
        .iter()
        .filter(|p| crate::csg::tag_op(p.tag) == op)
        .filter(|p| (p.plane.n.x - n[0]).abs() + (p.plane.n.y - n[1]).abs() + (p.plane.n.z - n[2]).abs() < 1e-6)
        .map(|p| p.tag)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[test]
fn face_references_survive_ten_parameter_changes() {
    let mut t = Timeline::default();
    for (k, v) in [("W", "2m"), ("D", "1m"), ("H", "1m"), ("P", "500mm"), ("C", "100mm")] {
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
    // the top is pushed up: its sides are new faces named by keys (top, neighbour side)
    let pp = t
        .add("", Kind::Modify {
            op: s("push_pull"),
            input: e,
            faces: vec![ftag(e, 1)],
            args: vec![(s("d"), s("P"))],
        })
        .unwrap();
    let mut cache = Cache::default();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    assert!(t.adopt_keys(&r));
    let keys = t.node(pp).unwrap().keys.clone();
    assert_eq!(keys.len(), 4);
    let east = faces_facing(r.body.as_ref().unwrap(), pp, [1.0, 0.0, 0.0]);
    let north = faces_facing(r.body.as_ref().unwrap(), pp, [0.0, 1.0, 0.0]);
    assert_eq!((east.len(), north.len()), (1, 1));
    // the chamfer refers to the pushed top and the new east side
    let ch = t
        .add("", Kind::Modify {
            op: s("chamfer"),
            input: pp,
            faces: vec![ftag(e, 1), east[0]],
            args: vec![(s("d"), s("C"))],
        })
        .unwrap();
    let diag = std::f64::consts::FRAC_1_SQRT_2;
    for i in 0..10 {
        let f = i as f64;
        t.set_param("W", &format!("{}m", 2.0 + 0.3 * f)).unwrap();
        t.set_param("D", &format!("{}m", 1.0 + 0.1 * (i % 3) as f64)).unwrap();
        t.set_param("P", &format!("{}m", 0.2 + 0.1 * f)).unwrap();
        let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
        t.adopt_keys(&r);
        assert_eq!(r.status(ch), Some(&Status::Built), "step {i}: {:?}", r.first_error());
        assert_eq!(t.node(pp).unwrap().keys, keys, "step {i}: keys stay");
        let b = r.body.as_ref().unwrap();
        // the referenced faces are still the east side and the top, the chamfer between them
        assert_eq!(faces_facing(b, pp, [1.0, 0.0, 0.0]), east, "step {i}");
        assert_eq!(faces_facing(b, pp, [0.0, 1.0, 0.0]), north, "step {i}");
        assert_eq!(faces_facing(b, ch, [diag, 0.0, diag]).len(), 1, "step {i}");
        let (w, d, h) = (2.0 + 0.3 * f, 1.0 + 0.1 * (i % 3) as f64, 1.0 + 0.2 + 0.1 * f);
        near(b.volume(), w * d * h - 0.005 * d);
    }
}

#[test]
fn shell_inner_faces_keep_their_names() {
    let (mut t, _, e, c) = block();
    let sh = t
        .add("", Kind::Modify {
            op: s("shell"),
            input: c,
            faces: vec![ftag(e, 1)],
            args: vec![(s("t"), s("C"))],
        })
        .unwrap();
    let mut cache = Cache::default();
    let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
    t.adopt_keys(&r);
    let floor = faces_facing(r.body.as_ref().unwrap(), sh, [0.0, 0.0, 1.0]);
    assert_eq!(floor.len(), 1);
    let keys = t.node(sh).unwrap().keys.clone();
    for depth in ["1.5m", "2m", "800mm"] {
        t.set_param("Depth", depth).unwrap();
        let r = rebuild(&t, reg(), &mut cache, 0).unwrap();
        assert!(!t.adopt_keys(&r));
        assert_eq!(t.node(sh).unwrap().keys, keys);
        assert_eq!(faces_facing(r.body.as_ref().unwrap(), sh, [0.0, 0.0, 1.0]), floor);
    }
}

#[test]
fn timeline_data_round_trips() {
    let (mut t, _, e, _) = block();
    let pp = t
        .add("", Kind::Modify {
            op: s("push_pull"),
            input: e,
            faces: vec![ftag(e, 2)],
            args: vec![(s("d"), s("200mm"))],
        })
        .unwrap();
    let r = rebuild(&t, reg(), &mut Cache::new(16), 0).unwrap();
    assert!(t.adopt_keys(&r));
    assert!(!t.node(pp).unwrap().keys.is_empty());
    let d = json::to_data(&t);
    assert!(d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    assert_eq!(json::from_data(&d).unwrap(), t);
    assert!(json::from_data("not data").is_err());
}

#[test]
fn component_family_from_a_timeline_builds_each_instance_from_its_values() {
    use crate::component::{from_feature, Category, Family, Lod, Value};
    let (t, ..) = block();
    let text = from_feature::family_text("Plinth", Category::Generic, &t).unwrap();
    let f = Family::parse(&text).unwrap();
    match &f.solids[0].shape {
        crate::component::Shape::Feature { tl } => assert_eq!(**tl, t),
        o => panic!("{o:?}"),
    }
    let p = |n: &str| f.params.iter().find(|p| p.name == n).unwrap();
    assert!(p("Depth").instance && p("Depth").formula.is_none());
    assert!(p("Width").formula.is_some());
    let ty = f.types[0].values.clone();
    let mut c = Cache::new(16);
    for d in [1.0, 1.5, 0.8] {
        let r = f.resolve(&ty, &[(s("Depth"), Value::Num(d))]).unwrap();
        near(r.num("Width").unwrap(), 2.0 * d);
        let b = f.build(&r, Lod::Fine).unwrap();
        let mut u = t.clone();
        u.set_param("Depth", &format!("{d}m")).unwrap();
        near(b.volume, vol(&rebuild(&u, reg(), &mut c, 0).unwrap()));
        near(b.bbox[1].x - b.bbox[0].x, 2.0 * d);
        assert!(!b.mesh.triangles.is_empty() && !b.plan.is_empty());
    }
    // a timeline parameter that is not a family name is refused, not renamed
    let mut bad = Timeline::default();
    bad.params.push(Param {
        name: s("2W"),
        expr: s("1m"),
    });
    assert!(from_feature::family_text("Bad", Category::Generic, &bad).is_err());
}
