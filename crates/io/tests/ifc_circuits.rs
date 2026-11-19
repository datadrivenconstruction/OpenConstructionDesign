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
