//! Curtain systems on freeform hosts: storage, quantities of a cylinder facade, following a
//! host edit, deletion with the host and undo, trims, refusals, ops lowering.

use super::*;
use crate::{FreeformSpec, ShellSide};
use ddc_geom::nurbs::{self, Curve, Surface, Trim};
use ddc_geom::v3;
use ddc_ops::{Actor, Cat, Envelope, GeoOp, Group, Len, Mullion, Op, PanelDiv, Ref, Targets};
use std::f64::consts::PI;

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 3.0).unwrap();
    (m, l)
}

fn host(level: i64, surface: Surface) -> FreeformSpec {
    FreeformSpec {
        level,
        name: "Facade".into(),
        material: String::new(),
        surface,
        thickness: 0.0,
        side: ShellSide::Normal,
        trim: Trim::default(),
        origin: "points".into(),
    }
}

/// Half cylinder r = 2 m, 3 m tall, about the z axis.
fn cylinder() -> Surface {
    let line = Curve::polyline(&[v3(2.0, 0.0, 0.0), v3(2.0, 0.0, 3.0)]).unwrap();
    nurbs::revolve(&line, v3(0.0, 0.0, 0.0), v3(0.0, 0.0, 1.0), PI).unwrap()
}

/// Bowed facade: quadratic in plan along u, straight up 6 m along v.
fn bowed() -> Surface {
    let mut ctrl = vec![];
    for (x, y) in [(0.0, 0.0), (5.0, 2.0), (10.0, 0.0)] {
        for z in [0.0, 6.0] {
            ctrl.push(v3(x, y, z));
        }
    }
    let (ku, kv) = (nurbs::clamped_knots(3, 2), nurbs::clamped_knots(2, 1));
    Surface::new(2, 1, ku, kv, 3, 2, ctrl, vec![1.0; 6]).unwrap()
}

fn system(host: i64, n: usize) -> PanelSystemSpec {
    PanelSystemSpec {
        host,
        name: "CW".into(),
        material: "Glass".into(),
        layout: PanelLayout {
            cols: Spacing::Equal(n),
            rows: Rows::Spacing(Spacing::Equal(n)),
            pattern: Pattern::Quad,
            limits: PanelLimits::default(),
            planarize: 0,
        },
        mullion: [0.06, 0.15],
        glass: 0.024,
    }
}

#[test]
fn cylinder_facade_quantities_and_round_trip() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, cylinder())).unwrap();
    let s = system(h, 6);
    let id = m.add_panel_system(&s).unwrap();
    assert_eq!(m.panel_system(id).unwrap(), s);
    assert_eq!(m.panel_systems(Some(l)).unwrap().len(), 1);
    assert_eq!(m.panel_systems_on(h).unwrap(), vec![id]);

    let q = m.panel_system_quantities(id).unwrap();
    let chord = 2.0 * 2.0 * (PI / 12.0).sin();
    assert_eq!((q.panels, q.flat, q.cold_bent, q.hot_bent, q.types), (36, 36, 0, 0, 1));
    // equal arcs come from a sampled length table: 1e-4 relative is the stated accuracy
    let rel = |a: f64, b: f64| (a / b - 1.0).abs();
    assert!(rel(q.area, 6.0 * chord * 3.0) < 1e-4, "{}", q.area);
    assert!((q.glass_volume - q.area * 0.024).abs() < 1e-12);
    assert!(rel(q.mullion_length, 7.0 * 3.0 + 42.0 * chord) < 1e-4, "{}", q.mullion_length);
    assert!((q.mullion_volume - q.mullion_length * 0.06 * 0.15).abs() < 1e-12);
    assert!(q.max_planarity < 1e-9 && q.off_surface == 0.0);

    let g = m.panel_system_geom(id).unwrap();
    assert_eq!((g.glass.len(), g.frames.len()), (36, 84));
    let zmin = g.panels.nodes.iter().map(|p| p.z).fold(f64::MAX, f64::min);
    assert!((zmin - 3.0).abs() < 1e-12, "level elevation applied: {zmin}");
    let a0 = g.panels.panels[g.kept[0]].area;
    assert!((g.glass[0].volume().abs() - a0 * 0.024).abs() < 1e-9);
    assert!(g.glass.iter().chain(&g.frames).all(|x| x.is_watertight()));
    let [a, b] = g.edges[0];
    let len = (g.panels.nodes[b] - g.panels.nodes[a]).len();
    assert!((g.frames[0].volume().abs() - len * 0.06 * 0.15).abs() < 1e-9);
}

#[test]
fn follows_host_edit_and_goes_with_the_host() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, bowed())).unwrap();
    let id = m.add_panel_system(&system(h, 4)).unwrap();
    let before = m.panel_system_geom(id).unwrap().panels.nodes;
    m.move_freeform_point(h, 1, 1, v3(5.0, 4.0, 6.0)).unwrap();
    let after = m.panel_system_geom(id).unwrap().panels.nodes;
    assert_eq!(before.len(), after.len());
    assert!(before.iter().zip(&after).any(|(a, b)| (*a - *b).len() > 0.1));
    m.undo().unwrap();

    m.delete_freeform(h).unwrap();
    assert!(m.panel_system(id).is_err());
    assert!(m.panel_systems(None).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.panel_system(id).unwrap(), system(h, 4));

    let (_, loss) = m.freeform_to_mesh(h).unwrap();
    assert!(loss.iter().any(|x| x.contains("panel systems: 1")), "{loss:?}");
    assert!(m.panel_system(id).is_err());
    m.undo().unwrap();
    assert!(m.panel_system(id).is_ok());
}

#[test]
fn trimmed_host_and_planarized_layout() {
    let (mut m, l) = model();
    let mut f = host(l, bowed());
    f.trim = Trim { outer: None, holes: vec![vec![[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]]] };
    let h = m.add_freeform(&f).unwrap();
    let id = m.add_panel_system(&system(h, 5)).unwrap();
    let g = m.panel_system_geom(id).unwrap();
    // the hole takes the middle 3 x 3 panels and the 12 grid edges inside that block
    assert_eq!((g.kept.len(), g.edges.len()), (16, 60 - 12));
    assert!(g.loss.iter().any(|x| x.starts_with("trim: 9 panels")), "{:?}", g.loss);

    // an anticlastic host is warped on the grid; planar rounds flatten it
    let mut ctrl = vec![];
    for i in 0..3 {
        for j in 0..3 {
            let (x, y) = (5.0 * i as f64, 5.0 * j as f64);
            ctrl.push(v3(x, y, 0.06 * (x - 5.0) * (y - 5.0)));
        }
    }
    let k = nurbs::clamped_knots(3, 2);
    let hy = m.add_freeform(&host(l, Surface::new(2, 2, k.clone(), k, 3, 3, ctrl, vec![1.0; 9]).unwrap())).unwrap();
    let mut s = system(hy, 6);
    let raw = m.add_panel_system(&s).unwrap();
    s.layout.planarize = 200;
    let flat = m.add_panel_system(&s).unwrap();
    let (q0, q1) = (m.panel_system_quantities(raw).unwrap(), m.panel_system_quantities(flat).unwrap());
    assert!(q0.hot_bent + q0.cold_bent > 0);
    assert!(q1.max_planarity < q0.max_planarity, "{} -> {}", q0.max_planarity, q1.max_planarity);
    assert!(q1.off_surface > 0.0);
    assert!(m.panel_system_geom(flat).unwrap().loss.iter().any(|x| x.contains("off the surface")));
}

#[test]
fn refusals() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, bowed())).unwrap();
    let ok = system(h, 4);
    let mut bad = ok.clone();
    bad.mullion = [0.06, 0.0];
    assert!(m.add_panel_system(&bad).is_err());
    let mut bad = ok.clone();
    bad.glass = 0.5;
    assert!(m.add_panel_system(&bad).is_err());
    let mut bad = ok.clone();
    bad.layout.cols = Spacing::Width(0.0);
    assert!(m.add_panel_system(&bad).is_err());
    let mut bad = ok.clone();
    bad.layout.limits.cold = 0.0001;
    assert!(m.add_panel_system(&bad).is_err());
    let mut bad = ok.clone();
    bad.host = 999;
    assert!(m.add_panel_system(&bad).is_err());
    let mut bad = ok;
    bad.layout.rows = Rows::Levels(vec![1.0, 9.0]);
    assert!(m.add_panel_system(&bad).is_err());
    assert!(m.panel_systems(None).unwrap().is_empty());
}

fn envelope(ops: Vec<Op>) -> Envelope {
    let mut e = Envelope::new(Actor::Agent { name: "test".into() }, "panels");
    e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
    e
}

fn panelize(host: Ref, columns: PanelDiv, rows: PanelDiv) -> Op {
    Op::Geometry(GeoOp::Panelize {
        slot: 0,
        host,
        columns,
        rows,
        pattern: None,
        planarize: None,
        mullion: Some(Mullion { width: Len::mm(50.0), depth: Len::mm(200.0) }),
        glass: None,
        flat: None,
        cold: None,
        material: None,
        name: Some("Bowed CW".into()),
    })
}

#[test]
fn ops_panelize_at_floor_heights_and_delete_with_host() {
    let (mut m, l) = model();
    let h = m.add_freeform(&host(l, bowed())).unwrap();
    let host = Ref::id(Cat::Freeform, h);
    let w = || PanelDiv::Width { width: Len::mm(2500.0) };
    let heights = [3.0, 4.5, 6.0, 9.0].iter().map(|&z| Len::m(z)).collect();
    let r = m.apply_envelope(&envelope(vec![panelize(host.clone(), w(), PanelDiv::Heights { heights })])).unwrap();
    let id = match &r.created[0].1 {
        Ref::Id { cat: Cat::PanelSystem, id } => *id,
        o => panic!("{o:?}"),
    };
    let s = m.panel_system(id).unwrap();
    assert_eq!(s.layout.rows, Rows::Levels(vec![0.0, 1.5, 3.0, 6.0]));
    let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
    assert!(near(s.mullion[0], 0.05) && near(s.mullion[1], 0.2) && near(s.glass, 0.024), "{s:?}");
    let g = m.panel_system_geom(id).unwrap();
    // the bowed plan is longer than 10 m, so 2.5 m panels need five columns
    assert_eq!((g.panels.cols, g.panels.rows), (5, 3));
    for (j, z) in [3.0, 4.5, 6.0, 9.0].iter().enumerate() {
        assert!((g.panels.nodes[j * 6 + 2].z - z).abs() < 1e-9);
    }

    // a height above the facade, and heights given for the columns, are refused
    let above = PanelDiv::Heights { heights: vec![Len::m(4.0), Len::m(20.0)] };
    assert!(m.apply_envelope(&envelope(vec![panelize(host.clone(), w(), above)])).is_err());
    let cols = PanelDiv::Heights { heights: vec![Len::m(4.0), Len::m(5.0)] };
    let e = m.apply_envelope(&envelope(vec![panelize(host, cols, PanelDiv::Count { n: 2 })])).unwrap_err();
    assert!(format!("{e:?}").contains("rows only"), "{e:?}");
    assert_eq!(m.panel_systems(None).unwrap().len(), 1);

    let del = Op::Delete { target: Targets::one(Ref::id(Cat::Freeform, h)) };
    let r = m.apply_envelope(&envelope(vec![del])).unwrap();
    assert!(r.deleted.contains(&Ref::id(Cat::PanelSystem, id)), "{:?}", r.deleted);
    assert!(m.panel_system(id).is_err());
    m.undo().unwrap();
    assert!(m.panel_system(id).is_ok());
}
