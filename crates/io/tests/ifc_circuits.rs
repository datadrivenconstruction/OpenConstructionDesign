// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Electrical circuits through IFC: IfcDistributionCircuit out, circuit rows back.

use ddc_io::ifc::{export_ifc, import_ifc};
use ddc_model::{
    CompHost, LayerSpec, WallSpec, CircuitDevice, CircuitKind, CircuitSpec, CompSpec, DeviceSpec, MepDomain, MepSection,
    MepShape, Model, RunSpec, SystemKind,
};

/// Devices of the library are wall-hosted: x runs along the wall from (0, 0) to (10, 0).
fn place(m: &mut Model, level: i64, wall: i64, family: &str, ty: &str, x: f64, _y: f64) -> i64 {
    let mut at = CompSpec::at(level, 0, x, 0.0);
    at.host = CompHost::Wall(wall);
    at.at.2 = 1.2;
    m.mep_place(&DeviceSpec {
        family: family.into(),
        ty: ty.into(),
        place: at,
        system: None,
        flow: None,
    })
    .unwrap()
    .comp
}

fn dev(comp: i64, load: Option<f64>) -> CircuitDevice {
    CircuitDevice {
        comp,
        connector: "Power".into(),
        load,
    }
}

#[test]
fn circuits_round_trip_through_ifc() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type("W200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let pw = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    m.add_mep_run(&RunSpec::new(
        pw,
        l,
        MepSection::new(MepDomain::Conduit, MepShape::Round { d: 0.025 }),
        vec![(0.0, 3.0, 3.0), (6.0, 3.0, 3.0)],
    ))
    .unwrap();
    let panel = place(&mut m, l, wall, "Distribution Board", "400x600", 0.0, 0.0);
    let s: Vec<i64> = (0..3)
        .map(|i| place(&mut m, l, wall, "Socket Outlet", "Single", 1.0 + i as f64, 2.0))
        .collect();

    let mut a = CircuitSpec::new(
        panel,
        CircuitKind::Power,
        vec![dev(s[0], Some(300.0)), dev(s[1], None)],
    );
    a.number = 1;
    a.name = "Sockets A".into();
    a.system = Some(pw);
    a.breaker = 20.0;
    a.cable = 4.0;
    m.add_circuit(&a).unwrap();
    let mut b = CircuitSpec::new(panel, CircuitKind::Lighting, vec![dev(s[2], None)]);
    b.number = 2;
    b.name = "Lights".into();
    b.poles = 3;
    b.voltage = 400.0;
    b.cos_phi = 0.85;
    b.laying = "tray".into();
    b.ambient_c = 40.0;
    b.grouped = 4;
    b.conductor = ddc_model::Conductor::Al;
    b.load_type = "light_drl_comp".into();
    m.add_circuit(&b).unwrap();

    let t = export_ifc(&m).unwrap();
    assert!(t.contains("IFCDISTRIBUTIONCIRCUIT"));
    assert_eq!(t.matches("=IFCDISTRIBUTIONCIRCUIT(").count(), 2);
    assert!(t.contains("'DDC_Circuit'") && t.contains("'DDC_CircuitLoads'"));

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert!(rep.warnings.iter().all(|w| !w.contains("circuit")), "{:?}", rep.warnings);
    let cs = back.circuits(None).unwrap();
    assert_eq!(cs.len(), 2);
    assert_eq!(cs[0].spec.number, 1);
    assert_eq!(cs[0].spec.name, "Sockets A");
    assert_eq!(cs[0].spec.kind, CircuitKind::Power);
    assert_eq!(cs[0].spec.breaker, 20.0);
    assert_eq!(cs[0].spec.cable, 4.0);
    assert_eq!(cs[0].spec.devices.len(), 2);
    assert_eq!(cs[0].spec.devices[0].load, Some(300.0));
    assert_eq!(cs[0].spec.devices[1].load, None);
    assert_eq!(cs[0].spec.devices[0].connector, "Power");
    let sys = back.mep_systems().unwrap();
    let name = sys.iter().find(|x| Some(x.id) == cs[0].spec.system).map(|x| x.name.as_str());
    assert_eq!(name, Some("PW-1"));
    assert_eq!(cs[1].spec.number, 2);
    assert_eq!(cs[1].spec.kind, CircuitKind::Lighting);
    assert_eq!(cs[1].spec.poles, 3);
    assert_eq!(cs[1].spec.voltage, 400.0);
    assert_eq!(cs[1].spec.cos_phi, 0.85);
    assert_eq!(cs[1].spec.laying, "tray");
    assert_eq!((cs[1].spec.ambient_c, cs[1].spec.grouped), (40.0, 4));
    assert_eq!((cs[0].spec.ambient_c, cs[0].spec.grouped), (30.0, 1));
    assert_eq!(cs[1].spec.conductor, ddc_model::Conductor::Al);
    assert_eq!(cs[1].spec.load_type, "light_drl_comp");
    assert_eq!(cs[0].spec.load_type, "office");
    assert_eq!(cs[1].spec.devices.len(), 1);
    assert_eq!(cs[1].spec.system, None);
    assert_eq!(cs[0].spec.panel, cs[1].spec.panel);

    // a second import changes nothing
    import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(back.circuits(None).unwrap().len(), 2);
}

#[test]
fn cable_traces_round_trip_as_cable_segments_and_wires_reach_the_panel() {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let wt = m
        .add_wall_type("W200", &[LayerSpec { material: "Concrete".into(), thickness: 0.2 }])
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (10.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let panel = place(&mut m, l, wall, "Distribution Board", "400x600", 0.0, 0.0);
    let s: Vec<i64> = (0..3)
        .map(|i| place(&mut m, l, wall, "Socket Outlet", "Single", 3.0 + 2.0 * i as f64, 0.0))
        .collect();
    let mut a = CircuitSpec::new(panel, CircuitKind::Power, s.iter().map(|&c| dev(c, None)).collect());
    a.number = 3;
    a.name = "Sockets".into();
    a.cable = 2.5;
    a.cable_type = "NYM-J 3x2.5".into();
    let axis = vec![(0.0, 0.3, 2.8), (3.0, 0.3, 2.8), (3.0, 0.3, 1.2)];
    a.trace = vec![ddc_model::CableTrace { level: l, points: axis.clone() }];
    let id = m.add_circuit(&a).unwrap();

    // the plan: one chain of two bows, a home run from the nearest socket to the panel
    let w = m.circuit_wires(l).unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].circuit, id);
    assert_eq!(w[0].arcs.len(), 2);
    assert_eq!(w[0].cores, 3);
    assert_eq!(w[0].label, "F3 / NYM-J 3x2.5");
    let near = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9;
    assert!(near(w[0].home[0], (3.0, 0.0)) && near(w[0].home[1], (0.0, 0.0)), "{:?}", w[0].home);
    assert!(near(*w[0].arcs[1].last().unwrap(), (7.0, 0.0)));
    let other = m.add_level("L2", 3.0).unwrap();
    assert!(m.circuit_wires(other).unwrap().is_empty());

    let t = export_ifc(&m).unwrap();
    assert_eq!(t.matches("=IFCCABLESEGMENT(").count(), 1);
    assert!(t.contains("'DDC_Cable'"));
    assert_eq!(t.matches("=IFCCABLECARRIERSEGMENT(").count(), 0, "the cable is no carrier");

    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert!(rep.warnings.iter().all(|w| !w.contains("cable")), "{:?}", rep.warnings);
    let cs = back.circuits(None).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].spec.trace.len(), 1);
    let got = &cs[0].spec.trace[0].points;
    assert_eq!(got.len(), axis.len());
    for (p, q) in got.iter().zip(&axis) {
        assert!((p.0 - q.0).abs() < 1e-9 && (p.1 - q.1).abs() < 1e-9 && (p.2 - q.2).abs() < 1e-9);
    }
    assert_eq!(back.mep_runs(None).unwrap().len(), 0, "no carrier run was made of it");
    // deleting the circuit takes its cable; undo brings it back
    back.delete_circuit(cs[0].id).unwrap();
    assert!(back.circuit_cables(None).unwrap().is_empty());
    back.undo().unwrap();
    assert_eq!(back.circuit_cables(None).unwrap().len(), 1);
}
