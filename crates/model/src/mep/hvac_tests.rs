// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Air systems: terminal flows, equal-friction sizing, plenum routing, critical path.
use super::*;
use crate::{CompSpec, LayerSpec, SlabSpec};

/// One storey 12 x 8 m, slab above at 3.6 (underside 3.4), an air handling unit on the floor
/// and four 300x300 diffusers in the ceiling at 2.7, each set to 200 m3/h.
fn office() -> (Model, i64, i64, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.6).unwrap();
    let st = m
        .add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    m.add_slab(&SlabSpec {
        level: l2,
        slab_type: st,
        offset: 0.0,
        outer: vec![(0.0, 0.0), (12.0, 0.0), (12.0, 8.0), (0.0, 8.0)],
        holes: vec![],
    })
    .unwrap();
    let put = |m: &mut Model, family: &str, ty: &str, at: (f64, f64, f64)| {
        let mut place = CompSpec::at(l1, 0, at.0, at.1);
        place.at.2 = at.2;
        m.mep_place(&DeviceSpec {
            family: family.into(),
            ty: ty.into(),
            place,
            system: None,
            flow: None,
        })
        .unwrap()
        .comp
    };
    let ahu = put(&mut m, "Air Handling Unit", "2000x1000", (1.5, 4.0, 0.0));
    let mut d = vec![];
    for (x, y) in [(5.0, 2.0), (5.0, 6.0), (9.0, 2.0), (9.0, 6.0)] {
        let c = put(&mut m, "Supply Diffuser", "300x300", (x, y, 2.7));
        m.mep_set_port_flow(c, "Air", Some(200.0)).unwrap();
        d.push(c);
    }
    (m, l1, ahu, d)
}

fn req(l: i64, ahu: i64, d: &[i64]) -> NetReq {
    let mut r = NetReq::new(
        "SA",
        SystemKind::SupplyAir,
        l,
        RouteFrom::Port {
            comp: ahu,
            connector: "Supply".into(),
        },
    );
    r.to = d.iter().map(|c| (*c, "Air".to_string())).collect();
    r.friction = Some(1.0);
    r.plenum = Some((2.7, 3.4));
    r
}

fn round_d(s: &Section) -> f64 {
    match s.shape {
        Shape::Round { d } => d,
        _ => panic!("round expected"),
    }
}

#[test]
fn four_terminals_size_the_main_for_their_sum() {
    let (m, l, ahu, d) = office();
    assert!((m.mep_port_flow(d[0], "Air").unwrap() - 200.0).abs() < 1e-9);
    let p = m.mep_plan_net(&req(l, ahu, &d)).unwrap();
    assert!(p.metrics.failed.is_empty(), "{:?}", p.metrics.failed);
    assert_eq!(p.metrics.clashes, 0);
    let ds: Vec<f64> = p.build.runs.iter().map(|(_, r)| round_d(&r.section)).collect();
    // 800 m3/h at 1 Pa/m: D250 gives 1.065 Pa/m, so D315 (0.341); one terminal: D160 (0.752)
    let max = ds.iter().cloned().fold(0.0, f64::max);
    assert!((max - 0.315).abs() < 1e-9, "{ds:?}");
    let min = ds.iter().cloned().fold(1.0, f64::min);
    assert!((min - 0.16).abs() < 1e-9, "{ds:?}");
    let pa = p.metrics.critical_pa.expect("critical path");
    assert!(pa > 0.0 && pa < 100.0, "{pa}");
    assert!(d.contains(&p.metrics.critical_to.clone().unwrap().0));

    let mut m = m;
    m.mep_build(&p.build).unwrap();
    let sa = m.mep_systems().unwrap()[0].id;
    let c = m.mep_check(sa).unwrap();
    assert!((c.total - 800.0).abs() < 1e-6, "{}", c.total);
}

#[test]
fn horizontal_runs_stay_in_the_plenum_with_insulation() {
    let (m, l, ahu, d) = office();
    let mut r = req(l, ahu, &d);
    r.insulation = 0.025;
    let p = m.mep_plan_net(&r).unwrap();
    assert!(p.metrics.failed.is_empty());
    let mut horizontal = 0;
    for (_, run) in &p.build.runs {
        assert!((run.section.insulation - 0.025).abs() < 1e-12);
        let half = round_d(&run.section) / 2.0 + 0.025;
        for w in run.pts().windows(2) {
            if (w[0].z - w[1].z).abs() < 1e-6 {
                horizontal += 1;
                assert!(w[0].z - half >= 2.7 - 1e-6, "below the ceiling: {}", w[0].z);
                assert!(w[0].z + half <= 3.4 + 1e-6, "into the slab: {}", w[0].z);
            }
        }
    }
    assert!(horizontal > 0);
}

#[test]
fn shallow_plenum_is_refused() {
    let (m, l, ahu, d) = office();
    let mut r = req(l, ahu, &d);
    // D315 trunk + 2 x 50 mm gap does not fit 200 mm
    r.plenum = Some((2.6, 2.8));
    let e = m.mep_plan_net(&r).unwrap_err().to_string();
    assert!(e.contains("mep.err.plenum_low"), "{e}");
}

#[test]
fn velocity_sizing_stays_when_no_friction_is_given() {
    let (m, l, ahu, d) = office();
    let mut r = req(l, ahu, &d);
    r.friction = None;
    r.plenum = None;
    let p = m.mep_plan_net(&r).unwrap();
    // 800 m3/h at 5 m/s needs 0.0444 m2: D250 (0.0491)
    let max = p
        .build
        .runs
        .iter()
        .map(|(_, r)| round_d(&r.section))
        .fold(0.0, f64::max);
    assert!((max - 0.25).abs() < 1e-9, "{max}");
}

#[test]
fn library_has_air_equipment_with_flows() {
    let (mut m, l, _, _) = office();
    let mut place = CompSpec::at(l, 0, 3.0, 3.0);
    place.at.2 = 2.9;
    let vav = m
        .mep_place(&DeviceSpec {
            family: "VAV Box".into(),
            ty: "250".into(),
            place: place.clone(),
            system: None,
            flow: None,
        })
        .unwrap()
        .comp;
    assert!((m.mep_port_flow(vav, "Inlet").unwrap() - 600.0).abs() < 1e-9);
    place.at = (7.0, 3.0, 2.8);
    let fcu = m
        .mep_place(&DeviceSpec {
            family: "Fan Coil Unit".into(),
            ty: "FC-04".into(),
            place,
            system: None,
            flow: None,
        })
        .unwrap()
        .comp;
    assert!((m.mep_port_flow(fcu, "Supply").unwrap() - 680.0).abs() < 1e-9);
    assert!((m.mep_port_flow(fcu, "Return").unwrap() - 680.0).abs() < 1e-9);
}
