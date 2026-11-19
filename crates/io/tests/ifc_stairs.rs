// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stairs, ramps and railings through IFC: classes, parts, exact return, fitted foreign stairs,
//! and a warning (never silence) for what cannot be written or read natively.

use ddc_geom::stair::Shape;
use ddc_io::ifc::{export_ifc, export_ifc_with_warnings, import_ifc};
use ddc_model::{Model, RailingHost, RailingSpec, RampSpec, StairSpec};

fn count(t: &str, ent: &str) -> usize {
    t.matches(&format!("={ent}(")).count()
}

fn ids(m: &Model, table: &str) -> Vec<i64> {
    let mut st = m
        .sql()
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
        .unwrap();
    let v = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    v
}

fn straight_only() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let a = m.add_level("L1", 0.0).unwrap();
    let b = m.add_level("L2", 3.0).unwrap();
    m.add_stair(&StairSpec::straight(a, b, (1.0, 1.0), 0.3))
        .unwrap();
    m
}

fn model() -> Model {
    let mut m = Model::open(":memory:").unwrap();
    let a = m.add_level("L1", 0.0).unwrap();
    let b = m.add_level("L2", 3.0).unwrap();
    let c = m.add_level("L1.5", 0.5).unwrap();
    let s1 = m
        .add_stair(&StairSpec::straight(a, b, (1.0, 1.0), 0.3))
        .unwrap();
    let mut l = StairSpec::straight(a, b, (8.0, 8.0), 0.0);
    l.shape = Shape::L {
        left: true,
        first: Some(8),
    };
    m.add_stair(&l).unwrap();
    m.add_stair_railings(s1, &RailingSpec::new(a, RailingHost::Path(vec![])))
        .unwrap();
    m.add_railing(&RailingSpec::new(
        a,
        RailingHost::Path(vec![(0.0, 5.0, 0.0), (4.0, 5.0, 0.0)]),
    ))
    .unwrap();
    m.add_ramp(&RampSpec::new(a, Some(c), vec![(10.0, 0.0), (16.0, 0.0)]))
        .unwrap();
    m
}

#[test]
fn stairs_ramps_and_railings_are_written_as_their_own_classes() {
    let t = export_ifc(&model()).unwrap();
    assert_eq!(count(&t, "IFCSTAIR"), 2);
    assert_eq!(count(&t, "IFCSTAIRFLIGHT"), 3);
    assert!(t
        .lines()
        .any(|l| l.contains("=IFCSLAB(") && l.contains(".LANDING.")));
    assert_eq!(count(&t, "IFCRAILING"), 3);
    assert_eq!(count(&t, "IFCRAMP"), 1);
    for p in ["'DDC_Stair'", "'DDC_Railing'", "'DDC_Ramp'"] {
        assert!(t.contains(p), "{p}");
    }
    assert!(t.contains(".QUARTER_TURN_STAIR.") && t.contains(".STRAIGHT_RUN_STAIR."));
}

#[test]
fn the_rows_come_back_exactly() {
    let src = model();
    let t = export_ifc(&src).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!((rep.stairs, rep.railings, rep.ramps), (2, 3, 1));
    assert_eq!(rep.meshes, 0, "nothing falls back to a mesh");

    let (a, b) = (ids(&src, "stair"), ids(&back, "stair"));
    assert_eq!(b.len(), 2);
    for (x, y) in a.iter().zip(&b) {
        let (mut s, mut r) = (src.stair(*x).unwrap(), back.stair(*y).unwrap());
        (s.level, s.top_level, r.level, r.top_level) = (0, 0, 0, 0);
        assert_eq!(s, r);
        let (lx, ly) = (src.stair_layout(*x).unwrap(), back.stair_layout(*y).unwrap());
        assert_eq!(lx.risers, ly.risers);
        assert!(((lx.z.1 - lx.z.0) - (ly.z.1 - ly.z.0)).abs() < 1e-9);
    }

    let (a, b) = (ids(&src, "railing"), ids(&back, "railing"));
    assert_eq!(b.len(), 3);
    let stairs_back = ids(&back, "stair");
    for (x, y) in a.iter().zip(&b) {
        let (mut s, mut r) = (src.railing(*x).unwrap(), back.railing(*y).unwrap());
        (s.level, r.level) = (0, 0);
        match (&s.host, &r.host) {
            (RailingHost::Stair { side: p, .. }, RailingHost::Stair { stair, side: q }) => {
                assert_eq!(p, q);
                assert!(stairs_back.contains(stair), "host is an imported stair");
                (s.host, r.host) = (RailingHost::Path(vec![]), RailingHost::Path(vec![]));
            }
            _ => {}
        }
        assert_eq!(s, r);
    }

    let (x, y) = (ids(&src, "ramp")[0], ids(&back, "ramp")[0]);
    let (mut s, mut r) = (src.ramp(x).unwrap(), back.ramp(y).unwrap());
    (s.level, s.top_level, r.level, r.top_level) = (0, None, 0, None);
    assert_eq!(s, r);
}

#[test]
fn a_second_round_trip_writes_the_same_elements() {
    let t = export_ifc(&model()).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, t.as_bytes()).unwrap();
    let t2 = export_ifc(&back).unwrap();
    for e in ["IFCSTAIR", "IFCSTAIRFLIGHT", "IFCRAILING", "IFCRAMP"] {
        assert_eq!(count(&t, e), count(&t2, e), "{e}");
    }
}

#[test]
fn an_unbuildable_stair_is_reported_and_not_written() {
    let m = model();
    m.sql()
        .execute("UPDATE level SET elevation=-1 WHERE name='L2'", [])
        .unwrap();
    let (t, warnings) = export_ifc_with_warnings(&m).unwrap();
    assert_eq!(count(&t, "IFCSTAIR"), 0);
    assert!(warnings
        .iter()
        .any(|w| w.contains("stair") && w.contains("not exported")));
}

#[test]
fn a_stair_from_another_file_is_fitted_and_listed_as_approximate() {
    let src = straight_only();
    let t = export_ifc(&src)
        .unwrap()
        .replace("'DDC_Stair'", "'Other_Stair'");
    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.stairs, 1);
    assert_eq!(rep.meshes, 0, "the flight is not meshed twice");
    assert!(rep
        .approximated
        .keys()
        .any(|k| k.starts_with("IFCSTAIR") && k.contains("fitted")));
    let want = src.stair_layout(ids(&src, "stair")[0]).unwrap();
    let got = back.stair_layout(ids(&back, "stair")[0]).unwrap();
    assert_eq!(got.risers, want.risers);
    assert!(((got.z.1 - got.z.0) - (want.z.1 - want.z.0)).abs() < 1e-3);
    assert!((got.flights[0].width - want.flights[0].width).abs() < 1e-3);
}

#[test]
fn a_ramp_without_our_data_stays_a_mesh_and_says_so() {
    let t = export_ifc(&model())
        .unwrap()
        .replace("'DDC_Ramp'", "'Other_Ramp'");
    let mut back = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(rep.ramps, 0);
    assert!(rep.meshes > 0);
    assert!(
        rep.skipped
            .keys()
            .chain(rep.as_mesh.keys())
            .any(|k| k.starts_with("IFCRAMP")),
        "{:?}",
        rep.skipped
    );
}

/// Input of `tools/validate_ifc.py` in CI: DDC_IFC_SAMPLE_DIR names the folder for the file and the
/// per-class counts the model side expects in it.
#[test]
fn writes_the_sample_for_the_external_validator() {
    let Ok(dir) = std::env::var("DDC_IFC_SAMPLE_DIR") else {
        return;
    };
    let m = model();
    let (mut flights, mut landings) = (0, 0);
    for id in ids(&m, "stair") {
        let l = m.stair_layout(id).unwrap();
        (flights, landings) = (flights + l.flights.len(), landings + l.landings.len());
    }
    let (mut rflights, mut rlandings) = (0, 0);
    for id in ids(&m, "ramp") {
        let l = m.ramp_layout(id).unwrap();
        (rflights, rlandings) = (rflights + l.flights.len(), rlandings + l.landings.len());
    }
    let counts = format!(
        "{{\"IFCSTAIR\": {}, \"IFCSTAIRFLIGHT\": {flights}, \"IFCRAILING\": {}, \"IFCRAMP\": {},          \"IFCRAMPFLIGHT\": {rflights}, \"IFCSLAB\": {}}}
",
        ids(&m, "stair").len(),
        ids(&m, "railing").len(),
        ids(&m, "ramp").len(),
        landings + rlandings,
    );
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/stairs.ifc"), export_ifc(&m).unwrap()).unwrap();
    std::fs::write(format!("{dir}/stairs.counts.json"), counts).unwrap();
}

#[test]
fn a_spiral_stair_comes_back_with_its_core_and_turn() {
    let mut src = Model::open(":memory:").unwrap();
    let a = src.add_level("L1", 0.0).unwrap();
    let b = src.add_level("L2", 3.0).unwrap();
    let mut sp = StairSpec::straight(a, b, (1.0, 1.0), 0.3);
    sp.width = 1.2;
    sp.max_riser = 0.18;
    sp.shape = Shape::Spiral {
        radius: 1.2,
        core: 0.3,
        turn: 6.25,
    };
    let id = src.add_stair(&sp).unwrap();
    let t = export_ifc(&src).unwrap();
    let mut back = Model::open(":memory:").unwrap();
    import_ifc(&mut back, t.as_bytes()).unwrap();
    let y = ids(&back, "stair")[0];
    let (mut s, mut r) = (src.stair(id).unwrap(), back.stair(y).unwrap());
    (s.level, s.top_level, r.level, r.top_level) = (0, 0, 0, 0);
    assert_eq!(s, r);
}
