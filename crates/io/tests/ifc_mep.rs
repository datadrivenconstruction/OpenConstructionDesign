// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! MEP networks through IFC: segment and fitting classes, ports, systems, exact round-trip.

use ddc_io::ifc::{export_ifc, import_ifc};
use ddc_model::{
    CompSpec, DeviceSpec, FittingKind, MepDomain, MepSection, MepShape, Model, PipeMaterial,
    RunSpec, SystemKind,
};

fn count(t: &str, ent: &str) -> usize {
    t.matches(&format!("={ent}(")).count()
}

fn snapshot(m: &Model) -> Vec<String> {
    let names: std::collections::HashMap<i64, String> = m
        .mep_systems()
        .unwrap()
        .into_iter()
        .map(|s| (s.id, format!("{}:{}", s.name, s.kind.name())))
        .collect();
    let mut v: Vec<String> = m
        .mep_runs(None)
        .unwrap()
        .into_iter()
        .map(|id| {
            let mut r = m.mep_run(id).unwrap();
            let sys = names[&r.system].clone();
            r.system = 0;
            r.level = 0;
            format!("{sys} {r:?}")
        })
        .collect();
    v.sort();
    v
}

#[test]
fn networks_round_trip_exactly() {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let sa = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    let cw = m.add_mep_system("CW-1", SystemKind::ColdWater).unwrap();
    let san = m.add_mep_system("SAN-1", SystemKind::Sanitary).unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
    duct.wall = 0.0008;
    duct.insulation = 0.03;
    m.add_mep_run(&RunSpec::new(
        sa,
        l1,
        duct,
        vec![(0.0, 0.0, 2.7), (6.0, 0.0, 2.7), (6.0, 4.0, 2.7)],
    ))
    .unwrap();
    let oval = MepSection::new(MepDomain::Duct, MepShape::Oval { w: 0.5, h: 0.25 });
    m.add_mep_run(&RunSpec::new(
        sa,
        l2,
        oval,
        vec![(0.0, 1.0, 5.7), (5.0, 1.0, 5.7)],
    ))
    .unwrap();
    let mut main = RunSpec::pipe(
        cw,
        l1,
        PipeMaterial::Copper,
        25,
        vec![(0.0, 2.0, 0.5), (8.0, 2.0, 0.5)],
    )
    .unwrap();
    main.caps = (true, false);
    m.add_mep_run(&main).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            cw,
            l1,
            PipeMaterial::Copper,
            15,
            vec![(4.0, 2.0, 0.5), (4.0, 4.0, 0.5)],
        )
        .unwrap(),
    )
    .unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            cw,
            l1,
            PipeMaterial::Copper,
            15,
            vec![(8.0, 2.0, 0.5), (8.0, 2.0, 3.5)],
        )
        .unwrap(),
    )
    .unwrap();
    let mut s = RunSpec::pipe(
        san,
        l1,
        PipeMaterial::Pvc,
        100,
        vec![(0.0, 5.0, -0.2), (5.0, 5.0, -0.2)],
    )
    .unwrap();
    s.slope = 2.0;
    m.add_mep_run(&s).unwrap();
    let conduit = MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.02 });
    m.add_mep_run(&RunSpec::new(
        pw,
        l1,
        conduit,
        vec![(1.0, 1.0, 2.9), (3.0, 1.0, 2.9)],
    ))
    .unwrap();
    let net = m.mep_network(None).unwrap();
    assert_eq!(net.count(FittingKind::Tee), 1);
    let fittings = net.fittings.len();

    let t = export_ifc(&m).unwrap();
    assert_eq!(count(&t, "IFCDUCTSEGMENT"), 2);
    assert_eq!(count(&t, "IFCPIPESEGMENT"), 4);
    assert_eq!(count(&t, "IFCCABLECARRIERSEGMENT"), 1);
    assert!(t.contains(".CONDUITSEGMENT."));
    assert_eq!(
        count(&t, "IFCDUCTFITTING")
            + count(&t, "IFCPIPEFITTING")
            + count(&t, "IFCCABLECARRIERFITTING"),
        fittings
    );
    assert!(t.contains(".JUNCTION.") && t.contains(".BEND."));
    assert_eq!(count(&t, "IFCDISTRIBUTIONSYSTEM"), 4);
    assert!(t.contains(".DOMESTICCOLDWATER.") && t.contains(".SEWAGE."));
    assert_eq!(count(&t, "IFCDISTRIBUTIONPORT"), 14);
    // shared end of the main and the riser
    assert_eq!(count(&t, "IFCRELCONNECTSPORTS"), 1);

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.mep_runs, 7);
    assert!(
        !rep.skipped
            .keys()
            .any(|k| k.contains("SEGMENT") || k.contains("FITTING") || k.contains("PORT")),
        "{:?}",
        rep.skipped
    );
    assert_eq!(snapshot(&back), snapshot(&m));
    let nb = back.mep_network(None).unwrap();
    assert_eq!(nb.fittings.len(), fittings);
    // a repeated export matches in entity count
    let t2 = export_ifc(&back).unwrap();
    assert_eq!(count(&t2, "IFCPIPEFITTING"), count(&t, "IFCPIPEFITTING"));
}

#[test]
fn placed_devices_export_with_their_classes_and_ports() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let sa = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    let san = m.add_mep_system("SAN-1", SystemKind::Sanitary).unwrap();
    let duct = MepSection::new(MepDomain::Duct, MepShape::Round { d: 0.315 });
    m.add_mep_run(&RunSpec::new(
        sa,
        l,
        duct,
        vec![(0.0, 0.0, 3.2), (8.0, 0.0, 3.2)],
    ))
    .unwrap();
    m.add_mep_run(&RunSpec::new(
        pw,
        l,
        MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.025 }),
        vec![(0.0, 3.0, 3.0), (6.0, 3.0, 3.0)],
    ))
    .unwrap();
    let place = |x: f64, y: f64, z: f64| {
        let mut s = CompSpec::at(l, 0, x, y);
        s.at.2 = z;
        s
    };
    let air = m
        .mep_place(&DeviceSpec {
            family: "Supply Diffuser".into(),
            ty: "600x600".into(),
            place: place(3.0, 2.0, 2.7),
            system: Some(sa),
            flow: None,
        })
        .unwrap();
    assert_eq!(air.linked, vec!["Air".to_string()]);
    assert_eq!(air.runs.len(), 1);
    m.mep_place(&DeviceSpec {
        family: "Ceiling Light".into(),
        ty: "Round 300".into(),
        place: place(2.0, 3.0, 2.7),
        system: Some(pw),
        flow: None,
    })
    .unwrap();
    let sink_type = m_first_type(&mut m, "Sink");
    m.mep_place(&DeviceSpec {
        family: "Sink".into(),
        ty: sink_type,
        place: place(5.0, 5.0, 0.0),
        system: Some(san),
        flow: None,
    })
    .unwrap();

    let t = export_ifc(&m).unwrap();
    assert_eq!(count(&t, "IFCAIRTERMINAL"), 1);
    assert_eq!(count(&t, "IFCLIGHTFIXTURE"), 1);
    assert_eq!(count(&t, "IFCSANITARYTERMINAL"), 1);
    assert!(t.contains("CONDUITSEGMENT"));
    assert!(t.contains("'DDC_MepPort'"));
    assert!(t.contains(":Air'"));
    // the diffuser port connects to its branch end
    assert!(count(&t, "IFCRELCONNECTSPORTS") >= 1);
    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert!(rep.warnings.iter().all(|w| !w.contains("DDC_MepPort")));
    assert_eq!(snapshot(&back), snapshot(&m));
}

fn m_first_type(m: &mut Model, family: &str) -> String {
    let f = m.load_library_family(family).unwrap();
    m.comp_types(f).unwrap()[0].1.clone()
}

#[test]
fn routed_air_system_exports_flows_sizes_and_equipment() {
    use ddc_model::{LayerSpec, NetReq, RouteFrom, SlabSpec};
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
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
        let mut place = CompSpec::at(l, 0, at.0, at.1);
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
    let mut to = vec![];
    for (x, y) in [(5.0, 2.0), (5.0, 6.0), (9.0, 2.0), (9.0, 6.0)] {
        let c = put(&mut m, "Supply Diffuser", "300x300", (x, y, 2.7));
        m.mep_set_port_flow(c, "Air", Some(200.0)).unwrap();
        to.push((c, "Air".to_string()));
    }
    put(&mut m, "VAV Box", "250", (3.0, 7.0, 2.9));
    let mut r = NetReq::new(
        "SA",
        SystemKind::SupplyAir,
        l,
        RouteFrom::Port {
            comp: ahu,
            connector: "Supply".into(),
        },
    );
    r.to = to;
    r.friction = Some(1.0);
    r.insulation = 0.025;
    r.plenum = Some((2.7, 3.4));
    let p = m.mep_plan_net(&r).unwrap();
    assert!(p.metrics.failed.is_empty(), "{:?}", p.metrics.failed);
    m.mep_build(&p.build).unwrap();

    let t = export_ifc(&m).unwrap();
    assert_eq!(count(&t, "IFCAIRTERMINAL"), 4);
    assert_eq!(count(&t, "IFCAIRTERMINALBOX"), 1);
    assert_eq!(count(&t, "IFCUNITARYEQUIPMENT"), 1);
    // each terminal port carries its 200 m3/h; the D315 main and its insulation are written
    assert_eq!(t.matches("IFCREAL(200.").count(), 4, "flows");
    assert!(t.contains("'0.315'"), "main size");
    assert!(t.contains("'0.025'"), "insulation");
    assert!(count(&t, "IFCDUCTSEGMENT") >= 5);
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(snapshot(&back), snapshot(&m));
}

/// Cable trays read back: a riser tray with its elbows, a tray reduced to conduit, and the
/// file is clean STEP, valid by the schema table and equal to the model after import.
#[test]
fn cable_trays_round_trip() {
    use ddc_io::ifc::{check_step, validate_ifc};
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    let tray = MepSection::new(MepDomain::CableTray, MepShape::Rect { w: 0.1, h: 0.06 });
    m.add_mep_run(&RunSpec::new(
        pw,
        l1,
        tray,
        vec![(0.0, 0.0, 2.9), (4.0, 0.0, 2.9), (4.0, 0.0, 5.9)],
    ))
    .unwrap();
    m.add_mep_run(&RunSpec::new(
        pw,
        l2,
        tray,
        vec![(4.0, 0.0, 5.9), (4.0, 3.0, 5.9)],
    ))
    .unwrap();
    let conduit = MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.032 });
    m.add_mep_run(&RunSpec::new(
        pw,
        l2,
        conduit,
        vec![(4.0, 3.0, 5.9), (4.0, 6.0, 5.9)],
    ))
    .unwrap();
    let fittings = m.mep_network(None).unwrap().fittings.len();

    let t = export_ifc(&m).unwrap();
    assert_eq!(count(&t, "IFCCABLECARRIERSEGMENT"), 3);
    assert!(t.contains(".CABLETRAYSEGMENT."));
    assert!(!t.contains("NaN") && !t.contains("inf"), "non-finite numbers in the file");
    check_step(&t).unwrap();
    let v = validate_ifc(t.as_bytes());
    assert!(v.is_clean(), "{}", v.brief(10));

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.mep_runs, 3);
    assert!(
        !rep.skipped
            .keys()
            .any(|k| k.contains("SEGMENT") || k.contains("FITTING") || k.contains("PORT")),
        "{:?}",
        rep.skipped
    );
    assert_eq!(snapshot(&back), snapshot(&m));
    assert_eq!(back.mep_network(None).unwrap().fittings.len(), fittings);
}

/// Trays, ducts, pipes and conduit together, with tees, a width change, a riser and a zigzag:
/// the file has no non-finite number, is clean STEP and reads back with the same run and
/// fitting counts (a NaN coordinate once made the whole file unreadable).
#[test]
fn mixed_networks_round_trip_with_equal_counts() {
    use ddc_io::ifc::{check_step, validate_ifc};
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.0).unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    let sa = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    let cw = m.add_mep_system("CW-1", SystemKind::ColdWater).unwrap();
    let tray = |w: f64| MepSection::new(MepDomain::CableTray, MepShape::Rect { w, h: 0.06 });
    // main tray with a tee branch, a narrower continuation, a riser and a zigzag
    let runs = [
        (l1, tray(0.3), vec![(0.0, 0.0, 2.9), (10.0, 0.0, 2.9)]),
        (l1, tray(0.1), vec![(5.0, 0.0, 2.9), (5.0, 6.0, 2.9)]),
        (l1, tray(0.2), vec![(10.0, 0.0, 2.9), (10.0, 4.0, 2.9), (10.0, 4.0, 5.9)]),
        (l2, tray(0.2), vec![(10.0, 4.0, 5.9), (14.0, 4.0, 5.9), (14.0, 8.0, 6.4), (18.0, 8.0, 6.4)]),
    ];
    for (lv, sec, pts) in runs {
        m.add_mep_run(&RunSpec::new(pw, lv, sec, pts)).unwrap();
    }
    let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
    duct.wall = 0.0008;
    m.add_mep_run(&RunSpec::new(
        sa,
        l1,
        duct,
        vec![(0.0, 2.0, 2.5), (6.0, 2.0, 2.5), (6.0, 5.0, 2.5)],
    ))
    .unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            cw,
            l1,
            PipeMaterial::Copper,
            25,
            vec![(0.0, 3.0, 0.5), (8.0, 3.0, 0.5), (8.0, 3.0, 3.5)],
        )
        .unwrap(),
    )
    .unwrap();
    m.add_mep_run(&RunSpec::new(
        pw,
        l1,
        MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.025 }),
        vec![(1.0, 1.0, 2.9), (3.0, 1.0, 2.9), (3.0, 1.0, 1.0)],
    ))
    .unwrap();
    let runs_n = m.mep_runs(None).unwrap().len();
    let fittings = m.mep_network(None).unwrap().fittings.len();
    assert!(fittings >= 6, "{fittings} fittings");

    let t = export_ifc(&m).unwrap();
    assert!(!t.contains("NaN") && !t.contains("inf"), "non-finite numbers in the file");
    check_step(&t).unwrap();
    let v = validate_ifc(t.as_bytes());
    assert!(v.is_clean(), "{}", v.brief(10));
    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.mep_runs, runs_n);
    assert_eq!(back.mep_runs(None).unwrap().len(), runs_n);
    assert_eq!(back.mep_network(None).unwrap().fittings.len(), fittings);
    assert_eq!(snapshot(&back), snapshot(&m));
}/// A larger tray and conduit network with a tee, a riser and a tilted run: one carrier segment
/// per run, no non-finite number anywhere, and the file reads back.
#[test]
fn tray_and_conduit_network_exports_finite_with_one_segment_per_run() {
    use ddc_io::ifc::{check_step, validate_ifc};
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    let tray = MepSection::new(MepDomain::CableTray, MepShape::Rect { w: 0.3, h: 0.1 });
    let conduit = MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.025 });
    let runs: Vec<(MepSection, Vec<(f64, f64, f64)>)> = vec![
        (tray, vec![(0.0, 0.0, 2.8), (10.0, 0.0, 2.8)]),
        (tray, vec![(5.0, 0.0, 2.8), (5.0, 6.0, 2.8)]),
        (tray, vec![(10.0, 0.0, 2.8), (10.0, 0.0, 0.4), (12.0, 0.0, 0.4)]),
        (conduit, vec![(5.0, 6.0, 2.8), (5.0, 6.0, 1.2)]),
        (conduit, vec![(5.0, 3.0, 2.8), (8.0, 3.0, 2.3), (8.0, 7.0, 2.3)]),
    ];
    for (s, pts) in &runs {
        m.add_mep_run(&RunSpec::new(pw, l1, *s, pts.clone())).unwrap();
    }

    let t = export_ifc(&m).unwrap();
    assert_eq!(count(&t, "IFCCABLECARRIERSEGMENT"), runs.len());
    for bad in ["NaN", "nan", "inf", "INF"] {
        assert!(!t.contains(bad), "{bad} in the file");
    }
    check_step(&t).unwrap();
    let v = validate_ifc(t.as_bytes());
    assert!(v.is_clean(), "{}", v.brief(10));

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.mep_runs, runs.len());
}

/// A duplicated vertex (zero-length segment) is dropped on the way in, so the file stays finite
/// and is the same as for the clean run.
#[test]
fn duplicate_point_in_a_run_exports_like_the_clean_run() {
    use ddc_io::ifc::{check_step, validate_ifc};
    let build = |pts: Vec<(f64, f64, f64)>| {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let s = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
        let tray = MepSection::new(MepDomain::CableTray, MepShape::Rect { w: 0.2, h: 0.08 });
        m.add_mep_run(&RunSpec::new(s, l, tray, pts)).unwrap();
        export_ifc(&m).unwrap()
    };
    let t = build(vec![(0.0, 0.0, 2.8), (4.0, 0.0, 2.8), (4.0, 0.0, 2.8), (4.0, 3.0, 2.8)]);
    for bad in ["NaN", "nan", "inf", "INF"] {
        assert!(!t.contains(bad), "{bad} in the file");
    }
    assert_eq!(count(&t, "IFCCABLECARRIERSEGMENT"), 1);
    check_step(&t).unwrap();
    assert!(validate_ifc(t.as_bytes()).is_clean());
    let clean = build(vec![(0.0, 0.0, 2.8), (4.0, 0.0, 2.8), (4.0, 3.0, 2.8)]);
    assert_eq!(count(&t, "IFCCABLECARRIERFITTING"), count(&clean, "IFCCABLECARRIERFITTING"));
}
