// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Runs with repeated points or no length: duplicates are dropped, a run with no length is
//! refused with a reason, nothing panics.

use ddc_model::{MepDomain, MepSection, MepShape, Model, RunSpec, SystemKind};

fn setup() -> (Model, i64, i64, MepSection) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    let s = m.add_mep_system("PW-1", SystemKind::Power).unwrap();
    (m, l, s, MepSection::new(MepDomain::CableTray, MepShape::Rect { w: 0.1, h: 0.06 }))
}

#[test]
fn duplicate_consecutive_points_are_dropped() {
    let dup = vec![(0.0, 0.0, 2.8), (0.0, 0.0, 2.8), (4.0, 0.0, 2.8), (4.0, 0.0, 2.8 + 1e-9), (4.0, 3.0, 2.8)];
    let clean = vec![(0.0, 0.0, 2.8), (4.0, 0.0, 2.8), (4.0, 3.0, 2.8)];
    let stored = |pts: Vec<(f64, f64, f64)>| {
        let (mut m, l, s, tray) = setup();
        let id = m.add_mep_run(&RunSpec::new(s, l, tray, pts)).unwrap();
        let net = m.mep_network(None).unwrap();
        assert!(net.pieces.iter().all(|(_, p, _)| !format!("{p:?}").contains("NaN")));
        (m.mep_run(id).unwrap().points, net.edges.len(), net.fittings.len())
    };
    let (p_dup, e_dup, f_dup) = stored(dup);
    let (p_clean, e_clean, f_clean) = stored(clean);
    assert_eq!(p_dup, p_clean);
    assert_eq!(e_dup, e_clean);
    assert_eq!(f_dup, f_clean);
}

#[test]
fn run_with_no_length_is_refused_with_a_reason() {
    let (mut m, l, s, tray) = setup();
    let e = m
        .add_mep_run(&RunSpec::new(s, l, tray, vec![(1.0, 1.0, 2.0), (1.0, 1.0, 2.0), (1.0, 1.0, 2.0)]))
        .unwrap_err();
    assert!(e.to_string().contains("no length"), "{e}");
    assert!(m.add_mep_run(&RunSpec::new(s, l, tray, vec![(1.0, 1.0, 2.0)])).is_err());
    assert!(m.mep_runs(None).unwrap().is_empty());
}

#[test]
fn non_finite_points_are_refused() {
    let (mut m, l, s, tray) = setup();
    let e = m
        .add_mep_run(&RunSpec::new(s, l, tray, vec![(0.0, 0.0, 2.0), (f64::NAN, 0.0, 2.0)]))
        .unwrap_err();
    assert!(e.to_string().contains("finite"), "{e}");
    assert!(m.mep_runs(None).unwrap().is_empty());
}

#[test]
fn update_drops_duplicates_too() {
    let (mut m, l, s, tray) = setup();
    let id = m.add_mep_run(&RunSpec::new(s, l, tray, vec![(0.0, 0.0, 2.0), (3.0, 0.0, 2.0)])).unwrap();
    m.update_mep_run(id, &RunSpec::new(s, l, tray, vec![(0.0, 0.0, 2.0), (3.0, 0.0, 2.0), (3.0, 0.0, 2.0), (3.0, 2.0, 2.0)]))
        .unwrap();
    assert!(m.update_mep_run(id, &RunSpec::new(s, l, tray, vec![(0.0, 0.0, 2.0), (0.0, 0.0, 2.0)])).is_err());
}
