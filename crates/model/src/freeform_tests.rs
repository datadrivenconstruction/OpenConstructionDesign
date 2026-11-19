// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Freeform element: storage round trip (memory and file), control point edit with undo,
//! refusals, shell quantities, ops lowering (create, edit, thicken, trim, convert, delete).

use super::*;
use ddc_geom::nurbs::Curve;
use ddc_ops::{
    Actor, Cat, ConvertTo, Curve3, CurveFit, Envelope, GeoOp, Group, Len, Op, Ref, ShellSide as OpSide,
    SurfaceBy, Targets, UvTrim, P3,
};
use std::f64::consts::PI;

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    (m, l)
}

fn cylinder(r: f64, h: f64) -> Surface {
    let line = Curve::polyline(&[v3(r, 0.0, 0.0), v3(r, 0.0, h)]).unwrap();
    nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), PI).unwrap()
}

/// Synclastic 4 x 4 net over 12 x 9 m rising 2 m in the middle (a dome-like roof).
fn dome_net() -> Surface {
    let n = 4;
    let mut ctrl = vec![];
    for i in 0..n {
        for j in 0..n {
            let (x, y) = (12.0 * i as f64 / 3.0, 9.0 * j as f64 / 3.0);
            let (a, b) = (i as f64 / 3.0 - 0.5, j as f64 / 3.0 - 0.5);
            ctrl.push(v3(x, y, 3.0 * (1.0 - 2.0 * a * a - 2.0 * b * b)));
        }
    }
    let k = nurbs::clamped_knots(4, 3);
    Surface::new(3, 3, k.clone(), k, 4, 4, ctrl, vec![1.0; 16]).unwrap()
}

fn spec(level: i64, surface: Surface) -> FreeformSpec {
    FreeformSpec {
        level,
        name: "Canopy".into(),
        material: "Glulam".into(),
        surface,
        thickness: 0.0,
        side: ShellSide::Normal,
        trim: Trim::default(),
        origin: "revolve".into(),
    }
}

#[test]
fn stores_and_reads_back_exactly() {
    let (mut m, l) = model();
    let mut s = spec(l, cylinder(2.0, 3.0));
    s.thickness = 0.12;
    s.side = ShellSide::Center;
    s.trim = Trim { outer: None, holes: vec![vec![[0.3, 0.3], [0.6, 0.3], [0.45, 0.7]]] };
    let id = m.add_freeform(&s).unwrap();
    assert_eq!(m.freeform(id).unwrap(), s);
    assert_eq!(m.freeforms(Some(l)).unwrap().len(), 1);
    // through a file: close, open again
    let dir = std::env::temp_dir().join(format!("ddc_ff_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ff.ddc");
    {
        let mut f = Model::open(path.to_str().unwrap()).unwrap();
        let lf = f.add_level("L1", 0.0).unwrap();
        let s2 = FreeformSpec { level: lf, ..s.clone() };
        let id2 = f.add_freeform(&s2).unwrap();
        drop(f);
        let g = Model::open(path.to_str().unwrap()).unwrap();
        assert_eq!(g.freeform(id2).unwrap(), s2);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mesh_and_quantities_follow_the_shell() {
    let (mut m, l) = model();
    let mut s = spec(l, cylinder(2.0, 3.0));
    let id = m.add_freeform(&s).unwrap();
    let q = m.freeform_quantities(id).unwrap();
    assert!((q.area - PI * 2.0 * 3.0).abs() < 1e-6, "{q:?}");
    assert_eq!(q.volume, 0.0);
    // level at +3 m: the world box starts there
    assert!((q.bbox[2] - 3.0).abs() < 1e-9 && (q.bbox[5] - 6.0).abs() < 1e-9, "{q:?}");
    let fm = m.freeform_mesh(id).unwrap();
    assert!(!fm.solid && !fm.loss.is_empty());
    s.thickness = 0.1;
    m.update_freeform(id, &s).unwrap();
    let fm = m.freeform_mesh(id).unwrap();
    assert!(fm.solid && fm.mesh.is_watertight());
    let q = m.freeform_quantities(id).unwrap();
    // half annulus 2.0..2.1 (or 1.9..2.0) of height 3; facets within the chord tolerance
    let outer = 0.5 * PI * (2.1f64.powi(2) - 4.0) * 3.0;
    let inner = 0.5 * PI * (4.0 - 1.9f64.powi(2)) * 3.0;
    let best = (q.volume - outer).abs().min((q.volume - inner).abs());
    assert!(best < 0.01 * outer, "volume {} vs {outer} / {inner}", q.volume);
}

#[test]
fn grip_edit_moves_one_point_and_undoes() {
    let (mut m, l) = model();
    let s = spec(l, dome_net());
    let id = m.add_freeform(&s).unwrap();
    let before = m.freeform(id).unwrap();
    m.move_freeform_point(id, 1, 2, v3(4.0, 6.0, 5.0)).unwrap();
    let after = m.freeform(id).unwrap();
    assert_eq!(after.surface.ctrl()[1 * 4 + 2], v3(4.0, 6.0, 5.0));
    let moved = (0..16).filter(|&k| after.surface.ctrl()[k] != before.surface.ctrl()[k]).count();
    assert_eq!(moved, 1);
    m.undo().unwrap();
    assert_eq!(m.freeform(id).unwrap(), before);
    assert!(m.move_freeform_point(id, 4, 0, v3(0.0, 0.0, 0.0)).is_err());
    // a weight pulls the patch toward its point; one undo step; zero and NaN refused
    m.set_freeform_weight(id, 1, 2, 3.0).unwrap();
    let w = m.freeform(id).unwrap();
    assert_eq!(w.surface.weights()[1 * 4 + 2], 3.0);
    assert_eq!(w.surface.ctrl(), before.surface.ctrl());
    m.undo().unwrap();
    assert_eq!(m.freeform(id).unwrap(), before);
    assert!(m.set_freeform_weight(id, 1, 2, 0.0).is_err());
    assert!(m.set_freeform_weight(id, 1, 2, f64::NAN).is_err());
    assert_eq!(m.freeform(id).unwrap(), before);
}

#[test]
fn refuses_degenerate_net_bad_trim_and_folding_shell() {
    let (mut m, l) = model();
    let k = vec![0.0, 0.0, 1.0, 1.0];
    let line = vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(2.0, 0.0, 0.0), v3(3.0, 0.0, 0.0)];
    let flat = Surface::new(1, 1, k.clone(), k, 2, 2, line, vec![1.0; 4]).unwrap();
    let e = m.add_freeform(&spec(l, flat)).unwrap_err();
    assert!(matches!(e, Error::Degenerate(_)), "{e}");
    let mut s = spec(l, dome_net());
    s.trim = Trim { outer: Some(vec![[0.0, 0.0], [1.5, 0.0], [0.0, 1.0]]), holes: vec![] };
    assert!(m.add_freeform(&s).is_err());
    let mut s = spec(l, cylinder(0.5, 1.0));
    s.thickness = 1.2;
    s.side = ShellSide::Center;
    let e = m.add_freeform(&s).unwrap_err();
    assert!(e.to_string().contains("folds"), "{e}");
    assert!(m.freeforms(None).unwrap().is_empty());
}

fn envelope(ops: Vec<Op>) -> Envelope {
    let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "freeform");
    e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
    e
}

/// Three arched sections at rising heights: a doubly curved (anticlastic) roof shell.
fn roof_op(slot: u32, level: Ref) -> Op {
    let curves = (0..3)
        .map(|k| {
            let y = 5.0 * k as f64;
            let sag = [0.0, -0.8, 0.0][k];
            Curve3 {
                pts: vec![
                    P3::m(0.0, y, 7.0 + sag),
                    P3::m(5.0, y, 9.0 + sag),
                    P3::m(10.0, y, 7.0 + sag),
                ],
                fit: Some(CurveFit::Through),
                degree: Some(2),
            }
        })
        .collect();
    Op::Geometry(GeoOp::Freeform {
        slot,
        level: Some(level),
        surface: SurfaceBy::Loft { curves, degree: Some(2), params: None },
        thickness: Some(Len::mm(200.0)),
        side: Some(OpSide::Reverse),
        trim: None,
        material: Some("Concrete".into()),
        name: Some("Roof shell".into()),
    })
}

#[test]
fn ops_create_edit_thicken_trim_convert() {
    let (mut m, l) = model();
    let lv = Ref::id(Cat::Level, l);
    let r = m.apply_envelope(&envelope(vec![roof_op(0, lv.clone())])).unwrap();
    let id = match &r.created[0].1 {
        Ref::Id { cat: Cat::Freeform, id } => *id,
        o => panic!("{o:?}"),
    };
    let s = m.freeform(id).unwrap();
    assert_eq!(s.origin, "loft");
    // stored above the level (+3 m): the ridge at absolute 9 m sits at 6 m
    let top = s.surface.point(0.5, 0.0);
    assert!((top.z - 6.0).abs() < 1e-9, "{top:?}");
    // doubly curved: the Gaussian curvature at the middle is not zero
    let d = s.surface.derivs(0.5, 0.5, 2);
    let n = s.surface.normal(0.5, 0.5).unwrap();
    let (e, f, g) = (d[1][0].dot(d[1][0]), d[1][0].dot(d[0][1]), d[0][1].dot(d[0][1]));
    let kg = (d[2][0].dot(n) * d[0][2].dot(n) - d[1][1].dot(n).powi(2)) / (e * g - f * f);
    assert!(kg.abs() > 1e-3, "K = {kg}");
    let q = m.freeform_quantities(id).unwrap();
    assert!((q.volume - q.area * 0.2).abs() < 0.02 * q.volume, "{q:?}");

    let ff = Ref::id(Cat::Freeform, id);
    let edit = Op::Geometry(GeoOp::EditPoint { target: ff.clone(), i: 1, j: 1, to: P3::m(5.0, 5.0, 10.0) });
    let thick = Op::Geometry(GeoOp::Thicken { target: ff.clone(), thickness: Len::mm(120.0), side: Some(OpSide::Center) });
    let trim = Op::Geometry(GeoOp::Trim {
        target: ff.clone(),
        trim: Some(UvTrim { outer: None, holes: vec![vec![[0.4, 0.4], [0.6, 0.4], [0.6, 0.6], [0.4, 0.6]]] }),
    });
    let r = m.apply_envelope(&envelope(vec![edit, thick, trim])).unwrap();
    // three edits of one surface: the receipt lists the changed element once
    assert_eq!(r.changed, vec![ff.clone()]);
    let s2 = m.freeform(id).unwrap();
    assert_eq!(s2.surface.ctrl()[s2.surface.nv() + 1], v3(5.0, 5.0, 7.0));
    assert!((s2.thickness - 0.12).abs() < 1e-12 && s2.side == ShellSide::Center);
    assert_eq!(s2.trim.holes.len(), 1);
    let q2 = m.freeform_quantities(id).unwrap();
    assert!(q2.area < q.area, "{q2:?}");
    // one undo restores all three edits
    m.undo().unwrap();
    assert_eq!(m.freeform(id).unwrap(), s);

    let bad = Op::Geometry(GeoOp::EditPoint { target: ff.clone(), i: 9, j: 0, to: P3::m(0.0, 0.0, 0.0) });
    assert!(m.apply_envelope(&envelope(vec![bad])).is_err());

    let conv = Op::Geometry(GeoOp::Convert { target: ff.clone(), to: ConvertTo::Mesh });
    let r = m.apply_envelope(&envelope(vec![conv])).unwrap();
    assert!(r.loss.iter().any(|l| l.contains("triangles")), "{:?}", r.loss);
    assert!(m.freeform(id).is_err());
    m.undo().unwrap();
    assert!(m.freeform(id).is_ok());

    let del = Op::Delete { target: Targets::one(ff) };
    m.apply_envelope(&envelope(vec![del])).unwrap();
    assert!(m.freeforms(None).unwrap().is_empty());
}

/// Network canopy on level `l`; `rise` lifts the middle v-curve off the u-curves.
fn canopy(rise: f64, l: i64) -> Op {
    let h = |x: f64, y: f64| 6.0 + 0.04 * x * (10.0 - x) + 0.03 * y * (10.0 - y);
    let k = [0.0, 5.0, 10.0];
    let cv = |pts: Vec<P3>| Curve3 { pts, fit: Some(CurveFit::Through), degree: Some(2) };
    let u_curves = k.iter().map(|&y| cv(k.iter().map(|&x| P3::m(x, y, h(x, y))).collect())).collect();
    let v_curves = k
        .iter()
        .enumerate()
        .map(|(j, &x)| {
            let dz = if j == 1 { rise } else { 0.0 };
            cv(k.iter().map(|&y| P3::m(x, y, h(x, y) + dz)).collect())
        })
        .collect();
    Op::Geometry(GeoOp::Freeform {
        slot: 0,
        level: Some(Ref::id(Cat::Level, l)),
        surface: SurfaceBy::Network { u_curves, v_curves, degree: Some(3), tolerance: Some(Len::mm(1.0)) },
        thickness: Some(Len::mm(100.0)),
        side: None,
        trim: None,
        material: None,
        name: Some("Canopy".into()),
    })
}

#[test]
fn network_surface_meets_its_curves_and_rebuild_undoes() {
    let (mut m, l) = model();
    let r = m.apply_envelope(&envelope(vec![canopy(0.0, l)])).unwrap();
    let id = match &r.created[0].1 {
        Ref::Id { cat: Cat::Freeform, id } => *id,
        o => panic!("{o:?}"),
    };
    let s = m.freeform(id).unwrap();
    assert_eq!(s.origin, "network");
    // every crossing of the network lies on the stored patch (level at +3 m)
    let h = |x: f64, y: f64| 6.0 + 0.04 * x * (10.0 - x) + 0.03 * y * (10.0 - y);
    for x in [0.0, 5.0, 10.0] {
        for y in [0.0, 5.0, 10.0] {
            let c = s.surface.closest(v3(x, y, h(x, y) - 3.0));
            assert!(c.dist < 1e-4, "{x} {y}: {}", c.dist);
        }
    }
    let ff = Ref::id(Cat::Freeform, id);
    let rb = Op::Geometry(GeoOp::Rebuild { target: ff.clone(), count_u: 6, count_v: 6, degree_u: None, degree_v: Some(2) });
    let r = m.apply_envelope(&envelope(vec![rb])).unwrap();
    assert_eq!(r.changed.len(), 1);
    assert!(r.loss.iter().any(|x| x.contains("rebuild") && x.contains("mm")), "{:?}", r.loss);
    let s2 = m.freeform(id).unwrap();
    let n = &s2.surface;
    assert_eq!((n.nu(), n.nv(), n.deg_u(), n.deg_v()), (6, 6, 3, 2));
    assert!((s2.thickness - s.thickness).abs() < 1e-15);
    m.undo().unwrap();
    assert_eq!(m.freeform(id).unwrap(), s);

    let bad = Op::Geometry(GeoOp::Rebuild { target: ff, count_u: 1, count_v: 6, degree_u: None, degree_v: None });
    assert!(m.apply_envelope(&envelope(vec![bad])).is_err());
    // curves that do not meet are refused with the pair named
    let e = m.apply_envelope(&envelope(vec![canopy(0.5, l)])).unwrap_err();
    assert!(format!("{e:?}").contains("do not meet"), "{e:?}");
}

#[test]
fn step_54_adds_the_freeform_tables_to_a_53_file() {
    assert!(crate::schema::STEPS.contains(&54));
    assert!(crate::schema::VERSION >= 54);
    let d = std::env::temp_dir().join(format!("ddc_s54_mig_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("old.ddc");
    let ps = p.to_str().unwrap();
    let tables = [
        "freeform",
        "panel_system",
        "shell_elem",
        "wall_spline",
        "wall_spline_piece",
        "gridshell",
        "gridshell_node",
        "gridshell_bar",
    ];
    {
        let m = Model::open(ps).unwrap();
        for t in tables {
            m.db.execute_batch(&format!("DROP TABLE {t}")).unwrap();
        }
        m.db.execute("UPDATE meta SET value=53 WHERE key='schema_version'", []).unwrap();
    }
    let m = Model::open(ps).unwrap();
    for t in tables {
        let n: i64 = m
            .db
            .query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1", [t], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "{t} is back after step 54");
    }
    // a 53 file runs step 54 and every later step
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    drop(m);
    let _ = std::fs::remove_dir_all(&d);
}

/// A patch of four sections whose edge columns are given; `mid` bends its middle column.
fn seam_patch(slot: u32, level: Ref, x0: f64, x1: f64, mid: f64, params: Option<Vec<f64>>) -> Op {
    let curves = [0.0, 1.0, 2.5, 3.0]
        .iter()
        .map(|&z| Curve3 {
            pts: vec![
                P3::m(x0, 0.25 * z, 4.0 + z + 0.05 * z * z),
                P3::m(0.5 * (x0 + x1), mid * z, 4.0 + z),
                P3::m(x1, 0.25 * z, 4.0 + z + 0.05 * z * z),
            ],
            fit: Some(CurveFit::Through),
            degree: Some(2),
        })
        .collect();
    Op::Geometry(GeoOp::Freeform {
        slot,
        level: Some(level),
        surface: SurfaceBy::Loft { curves, degree: Some(3), params },
        thickness: None,
        side: None,
        trim: None,
        material: None,
        name: None,
    })
}

fn seam(m: &mut Model, lv: &Ref, params: Option<Vec<f64>>) -> f64 {
    let r = m
        .apply_envelope(&envelope(vec![
            seam_patch(0, lv.clone(), 0.0, 6.0, 3.0, params.clone()),
            seam_patch(1, lv.clone(), 6.0, 12.0, -1.0, params),
        ]))
        .unwrap();
    let ids: Vec<i64> = r
        .created
        .iter()
        .filter_map(|c| match &c.1 {
            Ref::Id { cat: Cat::Freeform, id } => Some(*id),
            _ => None,
        })
        .collect();
    let (a, b) = (m.freeform(ids[0]).unwrap().surface, m.freeform(ids[1]).unwrap().surface);
    (0..=40)
        .map(|i| {
            let v = i as f64 / 40.0;
            (a.point(1.0, v) - b.point(0.0, v)).len()
        })
        .fold(0.0, f64::max)
}

#[test]
fn lofts_with_the_same_params_meet_on_one_edge_in_the_model() {
    let (mut m, l) = model();
    let lv = Ref::id(Cat::Level, l);
    let exact = seam(&mut m, &lv, Some(vec![0.0, 0.25, 0.75, 1.0]));
    assert!(exact <= 1e-6, "shared edge differs by {exact} m");
    let (mut m, l) = model();
    let chord = seam(&mut m, &Ref::id(Cat::Level, l), None);
    assert!(chord > 1e-4, "chord parameters should differ on this data: {chord}");
}

#[test]
fn a_loft_without_params_reads_and_writes_as_before() {
    let old = serde_json::json!({"by": "loft", "degree": 2, "curves": [
        {"pts": [{"x": 0.0, "y": 0.0, "z": 0.0, "unit": "m"}, {"x": 1.0, "y": 0.0, "z": 0.0, "unit": "m"}]},
        {"pts": [{"x": 0.0, "y": 1.0, "z": 0.0, "unit": "m"}, {"x": 1.0, "y": 1.0, "z": 0.0, "unit": "m"}]}]});
    let s: SurfaceBy = serde_json::from_value(old.clone()).unwrap();
    assert!(matches!(&s, SurfaceBy::Loft { params: None, .. }));
    let back = serde_json::to_value(&s).unwrap();
    assert!(back.get("params").is_none(), "{back}");
    let mut with = old;
    with["params"] = serde_json::json!([0.0, 1.0]);
    let s: SurfaceBy = serde_json::from_value(with.clone()).unwrap();
    assert_eq!(serde_json::to_value(&s).unwrap()["params"], with["params"]);
}
