use super::*;
use crate::{LayerSpec, SlabSpec, WallSpec};
use ddc_geom::profile_cat::Quantity;
use ddc_geom::Tol;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

fn named(kind: &str, ps: &[(&str, f64)]) -> ProfileReq {
    ProfileReq::Catalogue(ProfileSpec::Named {
        kind: kind.into(),
        params: ps
            .iter()
            .map(|(n, v)| (n.to_string(), Quantity::new(*v, "mm")))
            .collect(),
    })
}

fn rect_mm(w: f64, h: f64) -> ProfileReq {
    named("rect", &[("width", w), ("height", h)])
}

fn meta(level: Option<i64>) -> GeoMeta {
    GeoMeta {
        level,
        name: "sweep".into(),
        material: "concrete".into(),
        void: false,
        ifc: None,
    }
}

fn sweep(level: Option<i64>, profile: ProfileReq, path: PathReq) -> GeoReq {
    GeoReq::Sweep(SweepReq {
        meta: meta(level),
        profile,
        path,
        twist: 0.0,
        scale: 1.0,
        style: SweepStyle::default(),
    })
}

fn pts(p: &[[f64; 3]], closed: bool) -> PathReq {
    PathReq::Points {
        pts: p.to_vec(),
        bulges: vec![],
        closed,
    }
}

fn solid_id(b: GeoBuilt) -> i64 {
    match b {
        GeoBuilt::Solid(id) => id,
        other => panic!("expected a solid, got {other:?}"),
    }
}

fn mass_id(b: GeoBuilt) -> i64 {
    match b {
        GeoBuilt::Mass(id) => id,
        other => panic!("expected a mass, got {other:?}"),
    }
}

fn one_wall(m: &mut Model, l: i64) -> i64 {
    let wt = m
        .add_wall_type(
            "w",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_walls(&[WallSpec {
        a: (0.0, 0.0),
        b: (6.0, 0.0),
        level: l,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    }])
    .unwrap()[0]
}

fn section(m: &ProfileReq, at: [f64; 3]) -> LoftSection {
    LoftSection {
        profile: m.clone(),
        at,
    }
}

#[test]
fn sweep_along_wall_axis_volume_exact() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let w = one_wall(&mut m, l);
    let ops = m.journal_len().unwrap();
    let id = solid_id(
        m.apply_geo(&sweep(None, rect_mm(300.0, 500.0), PathReq::WallAxis { wall: w, z: 3.0 }))
            .unwrap(),
    );
    assert_eq!(m.journal_len().unwrap(), ops + 1, "one request, one undo step");
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 0.15 * 6.0, 1e-9), "{}", q.volume);
    assert!(close(q.bbox[2], 2.75, 1e-9) && close(q.bbox[5], 3.25, 1e-9));
    assert_eq!(m.solid(id).unwrap().level, l);
    m.undo().unwrap();
    assert!(m.solids(None).unwrap().is_empty());
}

#[test]
fn path_ref_follows_wall_edit() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let w = one_wall(&mut m, l);
    let id = solid_id(
        m.apply_geo(&sweep(None, rect_mm(300.0, 500.0), PathReq::WallAxis { wall: w, z: 3.0 }))
            .unwrap(),
    );
    m.set_wall_axis(w, (0.0, 0.0), (10.0, 0.0)).unwrap();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 0.15 * 10.0, 1e-9), "{}", q.volume);
    match m.solid(id).unwrap().form {
        Form::Sweep { path, .. } => {
            assert!(close(path.pts[1].x, 10.0, 1e-12) && close(path.pts[1].z, 3.0, 1e-12))
        }
        f => panic!("expected a sweep, got {f:?}"),
    }
    m.undo().unwrap();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 0.15 * 6.0, 1e-9), "{}", q.volume);
}

#[test]
fn self_intersecting_sweep_refused_with_reason() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let ops = m.journal_len().unwrap();
    let path = pts(
        &[
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 3.0, 0.0],
            [2.0, -2.0, 0.0],
        ],
        false,
    );
    let r = sweep(Some(l), rect_mm(200.0, 200.0), path);
    match m.apply_geo(&r) {
        Err(Error::Degenerate(msg)) => {
            assert!(msg.starts_with("sweep:") && msg.contains("crosses itself"), "{msg}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(m.plan_geo(&r).is_err());
    assert!(m.solids(None).unwrap().is_empty());
    assert_eq!(m.journal_len().unwrap(), ops, "a refusal writes nothing");
}

#[test]
fn closed_path_makes_closed_solid() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    // profile u in [-0.1, 0.1] across the path, v in [0, 0.4] up: centroid on the path
    let prof = ProfileReq::Section(Section::polygon(vec![
        v2(-0.1, 0.0),
        v2(0.1, 0.0),
        v2(0.1, 0.4),
        v2(-0.1, 0.4),
    ]));
    let square = pts(
        &[
            [0.0, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 4.0, 0.0],
            [0.0, 4.0, 0.0],
        ],
        true,
    );
    let id = solid_id(m.apply_geo(&sweep(Some(l), prof.clone(), square)).unwrap());
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 0.08 * 16.0, 1e-9), "{}", q.volume);
    // sides 0.4 * (16.8 + 15.2), top and bottom 0.2 * 16 each, no end caps
    assert!(close(q.area, 19.2, 1e-9), "{}", q.area);
    let mesh = m.solid_mesh(id).unwrap();
    assert!(mesh.is_watertight());
    assert!(close(mesh.volume().abs(), 1.28, 1e-9));

    // a slab edge is a closed path too
    let st = m
        .add_slab_type(
            "s",
            &[LayerSpec {
                material: "concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: l,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (5.0, 0.0), (5.0, 5.0), (0.0, 5.0)],
            holes: vec![],
        })
        .unwrap();
    let edge = PathReq::SlabEdge {
        slab,
        ring: 0,
        z: 0.0,
    };
    let id = solid_id(m.apply_geo(&sweep(None, prof, edge)).unwrap());
    assert!(m.solid_mesh(id).unwrap().is_watertight());
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, 0.08 * 20.0, 1e-9), "{}", q.volume);
}

#[test]
fn loft_two_sections_closed() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let r = GeoReq::Loft(LoftReq {
        meta: meta(Some(l)),
        sections: vec![
            section(&rect_mm(1000.0, 1000.0), [0.0, 0.0, 3.0]),
            section(&rect_mm(2000.0, 2000.0), [0.0, 0.0, 0.0]),
        ],
        path: None,
    });
    let ops = m.journal_len().unwrap();
    let id = solid_id(m.apply_geo(&r).unwrap());
    assert_eq!(m.journal_len().unwrap(), ops + 1);
    let q = m.solid_quantities(id).unwrap().unwrap();
    // square frustum: h / 3 * (A0 + A1 + sqrt(A0 * A1))
    assert!(close(q.volume, 7.0, 1e-9), "{}", q.volume);
    assert!(close(q.bbox[2], 0.0, 1e-9) && close(q.bbox[5], 3.0, 1e-9));
    assert!(m.solid_mesh(id).unwrap().is_watertight());
    m.undo().unwrap();
    assert!(m.solids(None).unwrap().is_empty());
}

#[test]
fn loft_mismatched_sections_handled_or_refused() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let sq = rect_mm(2000.0, 2000.0);
    let circle = named("circle", &[("diameter", 2000.0)]);
    // 4 vertices against an arc ring: handled
    let two = GeoReq::Loft(LoftReq {
        meta: meta(Some(l)),
        sections: vec![section(&sq, [0.0, 0.0, 0.0]), section(&circle, [0.0, 0.0, 3.0])],
        path: None,
    });
    let id = solid_id(m.apply_geo(&two).unwrap());
    assert!(m.solid_mesh(id).unwrap().is_watertight());
    let v = m.solid_quantities(id).unwrap().unwrap().volume;
    assert!(v > 3.0 * PI && v < 12.0, "{v}");
    // three sections of different vertex counts: one closed mass
    let three = GeoReq::Loft(LoftReq {
        meta: meta(Some(l)),
        sections: vec![
            section(&sq, [0.0, 0.0, 0.0]),
            section(&named("circle", &[("diameter", 1500.0)]), [0.0, 0.0, 2.0]),
            section(&rect_mm(1000.0, 1000.0), [0.5, 0.0, 4.0]),
        ],
        path: None,
    });
    let mid = mass_id(m.apply_geo(&three).unwrap());
    assert!(m.mass_body(mid).unwrap().check_closed(Tol::default()).is_ok());
    // section count the request cannot use: refused with the reason
    let one = GeoReq::Loft(LoftReq {
        meta: meta(Some(l)),
        sections: vec![section(&sq, [0.0, 0.0, 0.0])],
        path: None,
    });
    match m.apply_geo(&one) {
        Err(Error::Invalid(msg)) => assert!(msg.contains("at least two sections"), "{msg}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let along = GeoReq::Loft(LoftReq {
        meta: meta(Some(l)),
        sections: vec![
            section(&sq, [0.0; 3]),
            section(&circle, [0.0; 3]),
            section(&sq, [0.0; 3]),
        ],
        path: Some(pts(&[[0.0, 0.0, 0.0], [6.0, 0.0, 0.0]], false)),
    });
    match m.apply_geo(&along) {
        Err(Error::Invalid(msg)) => assert!(msg.contains("exactly two sections"), "{msg}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    // two sections along a path: a swept blend
    let ok = GeoReq::Loft(LoftReq {
        sections: vec![section(&sq, [0.0; 3]), section(&circle, [0.0; 3])],
        ..match along {
            GeoReq::Loft(l) => l,
            _ => unreachable!(),
        }
    });
    let id = solid_id(m.apply_geo(&ok).unwrap());
    assert!(matches!(m.solid(id).unwrap().form, Form::SweptBlend { .. }));
}

#[test]
fn twist_and_scale_sweeps() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let up = pts(&[[0.0, 0.0, 0.0], [0.0, 0.0, 3.0]], false);
    let mut r = SweepReq {
        meta: meta(Some(l)),
        profile: rect_mm(1000.0, 500.0),
        path: up,
        twist: PI / 2.0,
        scale: 1.0,
        style: SweepStyle::default(),
    };
    let id = mass_id(m.apply_geo(&GeoReq::Sweep(r.clone())).unwrap());
    assert!(m.mass_body(id).unwrap().check_closed(Tol::default()).is_ok());
    m.undo().unwrap();
    assert!(m.masses().unwrap().is_empty());
    // twist along a horizontal path: a smooth solid; twist keeps the area, so V = A * L
    r.path = pts(&[[0.0, 0.0, 0.0], [5.0, 0.0, 0.0]], false);
    let id = solid_id(m.apply_geo(&GeoReq::Sweep(r.clone())).unwrap());
    let v = m.solid_quantities(id).unwrap().unwrap().volume;
    assert!(close(v, 5.0 * 0.5, 1e-9), "{v}");
    m.undo().unwrap();
    // end scale 0.5 along 5 m: V = L * A0 * (1 - 1/2 + 1/12)
    r.twist = 0.0;
    r.scale = 0.5;
    let id = solid_id(m.apply_geo(&GeoReq::Sweep(r)).unwrap());
    let v = m.solid_quantities(id).unwrap().unwrap().volume;
    assert!(close(v, 5.0 * 0.5 * (7.0 / 12.0), 1e-6), "{v}");
}

fn hea300() -> ProfileReq {
    named("i", &[("height", 290.0), ("width", 300.0), ("web", 8.5), ("flange", 14.0)])
}

fn smooth_of(m: &Model, id: i64) -> ddc_geom::frame3::Smooth {
    match m.solid(id).unwrap().form {
        Form::Sweep { path, .. } => path.smooth.expect("a smooth path"),
        other => panic!("expected a sweep, got {other:?}"),
    }
}

#[test]
fn curved_beam_on_an_arc_and_a_spline() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let a = 2.0 * 0.3 * 0.014 + (0.29 - 0.028) * 0.0085;
    // plan arc through three points: chord 10, rise 1 -> radius 13
    let mut r = SweepReq {
        meta: meta(Some(l)),
        profile: hea300(),
        path: PathReq::Curve {
            start: [0.0, 0.0, 3.0],
            segs: vec![Seg::Arc {
                via: v3(5.0, 1.0, 3.0),
                to: v3(10.0, 0.0, 3.0),
            }],
        },
        twist: 0.0,
        scale: 1.0,
        style: SweepStyle {
            role: Role::Beam,
            ..SweepStyle::default()
        },
    };
    let id = solid_id(m.apply_geo(&GeoReq::Sweep(r.clone())).unwrap());
    let sm = smooth_of(&m, id);
    assert_eq!(sm.role, Role::Beam);
    assert!(matches!(sm.segs[0], Seg::Arc { .. }));
    let len = 2.0 * 13.0 * (5.0f64 / 13.0).asin();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, a * len, 1e-9 * a * len), "{} vs {}", q.volume, a * len);
    let mv = m.solid_mesh(id).unwrap().volume();
    assert!(close(mv, a * len, 1e-2 * a * len), "mesh {mv}");
    m.undo().unwrap();
    assert!(m.solids(None).unwrap().is_empty());
    // cambered beam: a spline in the vertical plane
    r.path = PathReq::Curve {
        start: [0.0, 0.0, 3.0],
        segs: vec![Seg::Spline {
            via: vec![v3(3.0, 0.0, 3.08), v3(6.0, 0.0, 3.08)],
            to: v3(9.0, 0.0, 3.0),
        }],
    };
    let id = solid_id(m.apply_geo(&GeoReq::Sweep(r.clone())).unwrap());
    let len = smooth_of(&m, id).length().unwrap();
    let v = m.solid_quantities(id).unwrap().unwrap().volume;
    assert!(close(v, a * len, 1e-9 * a * len), "{v} vs {}", a * len);
    // a curve sharper than the section: refused with the reason, nothing written
    r.path = PathReq::Curve {
        start: [0.0, 0.0, 3.0],
        segs: vec![Seg::Arc {
            via: v3(0.1, 0.1, 3.0),
            to: v3(0.2, 0.0, 3.0),
        }],
    };
    assert!(m.apply_geo(&GeoReq::Sweep(r)).is_err());
    assert_eq!(m.solids(None).unwrap().len(), 1);
}

#[test]
fn spiral_handrail_stays_upright() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let (o, t) = (0.0424_f64, 0.0032_f64);
    let a = PI * (o * o - (o - 2.0 * t).powi(2)) / 4.0;
    let (turns, rise) = (1.5, 3.0);
    let r = SweepReq {
        meta: meta(Some(l)),
        profile: named("tube", &[("outer_diameter", 42.4), ("wall", 3.2)]),
        path: PathReq::Curve {
            start: [1.0, 0.0, 0.9],
            segs: vec![Seg::Helix {
                center: v3(0.0, 0.0, 0.9),
                axis: v3(0.0, 0.0, 1.0),
                angle: 2.0 * PI * turns,
                rise,
            }],
        },
        twist: 0.0,
        scale: 1.0,
        style: SweepStyle {
            frame: Some([0.0, 0.0, 1.0]),
            role: Role::Railing,
            ..SweepStyle::default()
        },
    };
    let id = solid_id(m.apply_geo(&GeoReq::Sweep(r)).unwrap());
    let sm = smooth_of(&m, id);
    assert_eq!(sm.role, Role::Railing);
    assert!(matches!(sm.frame, ddc_geom::frame3::FrameMode::Fixed(_)));
    let len = ((2.0 * PI * turns).powi(2) + rise * rise).sqrt();
    let q = m.solid_quantities(id).unwrap().unwrap();
    assert!(close(q.volume, a * len, 1e-9 * a * len), "{} vs {}", q.volume, a * len);
    let mesh = m.solid_mesh(id).unwrap();
    assert!(close(mesh.volume(), a * len, 1e-2 * a * len), "mesh {}", mesh.volume());
    // the top end sits where the helix ends
    let top = mesh.positions.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!(close(top, 0.9 + rise + o / 2.0, 0.02), "{top}");
    m.undo().unwrap();
    assert!(m.solids(None).unwrap().is_empty());
}
