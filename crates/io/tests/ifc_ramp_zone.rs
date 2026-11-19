//! Ramps and zones in IFC: class, body, group composition, determinism.

use ddc_io::ifc::export_ifc;
use ddc_model::{Model, RampSpec, RoomSpec, ZoneSpec};

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let a = m.add_level("L1", 0.0).unwrap();
    let b = m.add_level("L2", 0.5).unwrap();
    m.add_ramp(&RampSpec::new(a, Some(b), vec![(0.0, 0.0), (6.0, 0.0)]))
        .unwrap();
    let room = m
        .add_room(&RoomSpec {
            level: a,
            name: "R".into(),
            number: "1".into(),
            point: (1.0, 1.0),
            height: 2.7,
        })
        .unwrap();
    m.add_zone(&ZoneSpec {
        name: "Fire 1".into(),
        rooms: vec![room],
    })
    .unwrap();
    m
}

#[test]
fn ramp_exports_with_body_and_quantities() {
    let t = export_ifc(&model()).unwrap();
    assert_eq!(t.matches("=IFCRAMP(").count(), 1);
    assert!(t.contains(".STRAIGHT_RUN_RAMP."));
    assert!(t.contains("'DDC_Ramp'"));
    assert_eq!(t.matches("=IFCRAMPFLIGHT(").count(), 1);
    let agg = t
        .lines()
        .filter(|l| l.contains("=IFCRELAGGREGATES("))
        .count();
    assert!(agg >= 1);
}

#[test]
fn long_ramp_aggregates_flights_and_landings() {
    let mut m = Model::open(":memory:").unwrap();
    let a = m.add_level("L1", 0.0).unwrap();
    let b = m.add_level("L2", 1.0).unwrap();
    m.add_ramp(&RampSpec::new(a, Some(b), vec![(0.0, 0.0), (20.0, 0.0)]))
        .unwrap();
    let t = export_ifc(&m).unwrap();
    assert_eq!(t.matches("=IFCRAMP(").count(), 1);
    assert_eq!(t.matches("=IFCRAMPFLIGHT(").count(), 2);
    assert!(t
        .lines()
        .any(|l| l.contains("=IFCSLAB(") && l.contains(".LANDING.")));
    assert!(t.contains(".TWO_STRAIGHT_RUN_RAMP."));
}

#[test]
fn broken_ramp_is_skipped_with_a_warning() {
    let m = model();
    m.sql()
        .execute("UPDATE level SET elevation=-1 WHERE name='L2'", [])
        .unwrap();
    let (t, warnings) = ddc_io::ifc::export_ifc_with_warnings(&m).unwrap();
    assert_eq!(t.matches("=IFCRAMP(").count(), 0);
    assert!(warnings
        .iter()
        .any(|w| w.contains("ramp") && w.contains("not exported")));
}

#[test]
fn zone_groups_its_spaces() {
    let t = export_ifc(&model()).unwrap();
    assert_eq!(t.matches("=IFCZONE(").count(), 1);
    assert!(t.contains("'Fire 1'"));
    let rel = t
        .lines()
        .filter(|l| l.contains("=IFCRELASSIGNSTOGROUP("))
        .count();
    assert_eq!(rel, 1);
}

#[test]
fn export_is_deterministic() {
    assert_eq!(export_ifc(&model()).unwrap(), export_ifc(&model()).unwrap());
}
