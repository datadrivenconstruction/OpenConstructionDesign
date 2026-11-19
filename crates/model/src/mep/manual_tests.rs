// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use super::*;
use crate::CompSpec;

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

fn model() -> (Model, i64) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    (m, l)
}

fn round_duct(s: i64, l: i64, d: f64, pts: Vec<(f64, f64, f64)>) -> RunSpec {
    RunSpec::new(s, l, Section::new(Domain::Duct, Shape::Round { d }), pts)
}

fn conduit(s: i64, l: i64, d: f64, pts: Vec<(f64, f64, f64)>) -> RunSpec {
    RunSpec::new(s, l, Section::new(Domain::Conduit, Shape::Round { d }), pts)
}

fn device(family: &str, ty: &str, l: i64, at: (f64, f64, f64), system: Option<i64>) -> DeviceSpec {
    let mut place = CompSpec::at(l, 0, at.0, at.1);
    place.at.2 = at.2;
    DeviceSpec {
        family: family.into(),
        ty: ty.into(),
        place,
        system,
        flow: None,
    }
}

fn has(c: &MepSystemCheck, f: impl Fn(&MepIssue) -> bool) -> bool {
    c.issues.iter().any(f)
}

#[test]
fn port_table_exists_at_current_schema() {
    let (m, _) = model();
    assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
    assert!(m.mep_port_links(None).unwrap().is_empty());
}

#[test]
fn draw_adds_system_elbow_and_tee_with_single_undo_each() {
    let (mut m, l) = model();
    m.mep_draw(
        "SA",
        SystemKind::SupplyAir,
        &round_duct(
            0,
            l,
            0.2,
            vec![(0.0, 0.0, 3.0), (5.0, 0.0, 3.0), (5.0, 4.0, 3.0)],
        ),
    )
    .unwrap();
    let net = m.mep_network(None).unwrap();
    assert_eq!(net.count(FittingKind::Elbow), 1);
    m.mep_draw(
        "SA",
        SystemKind::SupplyAir,
        &round_duct(0, l, 0.2, vec![(2.5, 3.0, 3.0), (2.5, 0.0, 3.0)]),
    )
    .unwrap();
    assert_eq!(m.mep_systems().unwrap().len(), 1);
    assert_eq!(m.mep_network(None).unwrap().count(FittingKind::Tee), 1);
    m.undo().unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 1);
    m.undo().unwrap();
    assert!(m.mep_runs(None).unwrap().is_empty());
    assert!(
        m.mep_systems().unwrap().is_empty(),
        "system goes with its first run"
    );
}

#[test]
fn draw_that_cannot_be_built_leaves_nothing() {
    let (mut m, l) = model();
    let r = m.mep_draw(
        "SA",
        SystemKind::SupplyAir,
        &round_duct(
            0,
            l,
            0.4,
            vec![(0.0, 0.0, 3.0), (0.05, 0.0, 3.0), (0.05, 4.0, 3.0)],
        ),
    );
    assert!(r.is_err());
    assert!(m.mep_systems().unwrap().is_empty());
    assert!(m.mep_runs(None).unwrap().is_empty());
}

#[test]
fn routed_points_handle_height_and_slope() {
    let plan = [(0.0, 0.0), (10.0, 0.0)];
    let flat = routed_points(&plan, 3.0, 2.0, None, None, 0.3);
    assert_eq!(flat.len(), 2);
    assert!(close(flat[1].2, 2.8, 1e-12));
    // far end: vertical leg down to the network
    let far = routed_points(&plan, 3.0, 2.0, None, Some(1.0), 0.3);
    assert_eq!(far.len(), 3);
    assert!(close(far[1].2, 2.8, 1e-12));
    assert!(close(far[2].2, 1.0, 1e-12));
    // no room for an elbow: the last leg tilts to the end height
    let near = routed_points(&plan, 3.0, 2.0, None, Some(2.7), 0.3);
    assert_eq!(near.len(), 2);
    assert!(close(near[1].2, 2.7, 1e-12));
    let start = routed_points(&plan, 3.0, 0.0, Some(1.0), None, 0.3);
    assert_eq!(start.len(), 3);
    assert!(close(start[0].2, 1.0, 1e-12) && close(start[1].2, 3.0, 1e-12));
    assert_eq!(
        routed_points(&plan, 3.0, 0.0, Some(2.95), None, 0.3)[0].2,
        2.95
    );
}

#[test]
fn slope_keeps_vertical_legs_and_is_idempotent() {
    let p = mep::apply_slope(
        &[v3(0.0, 0.0, 3.0), v3(10.0, 0.0, 3.0), v3(10.0, 0.0, 1.0)],
        1.0,
    );
    assert!(close(p[1].z, 2.9, 1e-12));
    assert!(close(p[2].z, 0.9, 1e-12));
    let q = mep::apply_slope(&p, 1.0);
    assert!(close(q[2].z, p[2].z, 1e-12));
}

#[test]
fn sloped_branch_joins_a_sloped_main_with_a_tee() {
    let (mut m, l) = model();
    let san = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            san,
            l,
            PipeMaterial::Pvc,
            100,
            vec![(0.0, 0.0, 0.5), (10.0, 0.0, 0.4)],
        )
        .unwrap(),
    )
    .unwrap();
    let pts = routed_points(&[(5.0, 3.0), (5.0, 0.0)], 0.6, 2.0, None, Some(0.45), 0.3);
    let branch = RunSpec::pipe(san, l, PipeMaterial::Pvc, 50, pts).unwrap();
    m.add_mep_run(&branch).unwrap();
    assert_eq!(m.mep_network(Some(san)).unwrap().count(FittingKind::Tee), 1);
    let c = m.mep_check(san).unwrap();
    assert!(!has(&c, |i| matches!(i, MepIssue::SlopeReverse { .. })));
    assert!(!has(&c, |i| matches!(i, MepIssue::SlopeLow { .. })));
    assert!(!has(&c, |i| matches!(i, MepIssue::Broken { .. })));
}

#[test]
fn placing_a_terminal_connects_it_in_one_undo() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
    ))
    .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(sa),
        ))
        .unwrap();
    assert_eq!(p.linked, vec!["Air".to_string()]);
    assert_eq!(p.runs.len(), 1);
    let port = m.world_port(p.comp, "Air").unwrap();
    assert!(close(port.at.2, 2.9, 1e-9));
    assert!(close(m.mep_port_flow(p.comp, "Air").unwrap(), 400.0, 1e-9));
    let net = m.mep_network(Some(sa)).unwrap();
    assert_eq!(net.count(FittingKind::Tee), 1);
    assert!(net
        .nodes
        .iter()
        .any(|n| n.port == Some((p.comp, "Air".into()))));
    let links = m.mep_port_links(Some(sa)).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].stub, Some(p.runs[0]));
    let c = m.mep_check(sa).unwrap();
    assert!(close(c.total, 400.0, 1e-9));
    assert!(c.devices[0].connected);
    // the neck faces up, the stub leaves it sideways: said out loud, not an error
    let note = c
        .issues
        .iter()
        .find(|i| matches!(i, MepIssue::PortApproach { .. }))
        .expect("approach note");
    assert!(!note.is_error());

    m.undo().unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 1);
    assert!(m.component(p.comp).is_err());
    assert!(m.mep_port_links(None).unwrap().is_empty());
}

#[test]
fn incompatible_system_is_not_connected() {
    let (mut m, l) = model();
    let cw = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(cw),
        ))
        .unwrap();
    assert!(p.linked.is_empty());
    assert!(m.mep_attach(p.comp, "Air", Some(cw)).is_err());
    assert!(m.mep_port_links(None).unwrap().is_empty());
}

#[test]
fn place_connects_while_derived_tables_are_held() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let d = device("Supply Diffuser", "600x600", l, (3.0, 2.0, 2.7), Some(sa));
    let p = m.with_derived_held(|m| m.mep_place(&d)).unwrap();
    assert_eq!(p.linked, vec!["Air".to_string()]);
    let held = m.component_quantities(p.comp).unwrap().placement.origin;
    assert!(close(held.0, 3.0, 1e-9) && close(held.1, 2.0, 1e-9), "{held:?}");
}

#[test]
fn disconnect_removes_the_stub_and_undo_restores_it() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
    ))
    .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(sa),
        ))
        .unwrap();
    m.mep_detach(p.comp, "Air").unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 1);
    assert!(m.mep_port_links(None).unwrap().is_empty());
    assert!(m.mep_detach(p.comp, "Air").is_err(), "already disconnected");
    m.undo().unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 2);
    assert_eq!(m.mep_port_links(Some(sa)).unwrap().len(), 1);
    // connecting again goes through the same path
    m.mep_detach(p.comp, "Air").unwrap();
    let again = m.mep_attach(p.comp, "Air", None).unwrap();
    assert!(again.is_some());
    assert_eq!(m.mep_port_links(Some(sa)).unwrap().len(), 1);
}

#[test]
fn run_ends_detach_and_reconnect() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.3,
        vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)],
    ))
    .unwrap();
    let br = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.2,
            vec![(5.0, 3.0, 3.0), (5.0, 0.0, 3.0)],
        ))
        .unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 1);
    m.mep_detach_end(br, 1).unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 0);
    assert_eq!(m.mep_check(sa).unwrap().parts, 2);
    m.mep_connect_end(br, 1, None).unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 1);
    assert_eq!(m.mep_run(br).unwrap().points.len(), 2);
    assert_eq!(m.mep_check(sa).unwrap().parts, 1);
}

#[test]
fn lights_link_without_a_run_and_count_as_load() {
    let (mut m, l) = model();
    let pw = m.add_mep_system("PW", SystemKind::Power).unwrap();
    let p = m
        .mep_place(&device(
            "Ceiling Light",
            "Round 600",
            l,
            (2.0, 2.0, 2.7),
            Some(pw),
        ))
        .unwrap();
    assert_eq!(p.linked, vec!["Power".to_string()]);
    assert!(p.runs.is_empty());
    let c = m.mep_check(pw).unwrap();
    assert!(close(c.total, 36.0, 1e-9));
    assert_eq!(c.errors(), 0);
    assert!(c.devices[0].connected);
    assert_eq!(c.unit_key(), "mep.unit.power");
}

#[test]
fn air_check_reports_speed_and_suggests_a_size() {
    let (mut m, l) = model();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (3.0, 0.0, 2.7),
            None,
        ))
        .unwrap();
    let at = m.world_port(p.comp, "Air").unwrap().at;
    let run = m
        .mep_draw(
            "SA",
            SystemKind::SupplyAir,
            &round_duct(0, l, 0.1, vec![(0.0, 0.0, at.2), at]),
        )
        .unwrap();
    let sa = m.mep_run(run).unwrap().system;
    assert_eq!(
        m.mep_port_links(Some(sa)).unwrap().len(),
        1,
        "touching port joins"
    );
    m.mep_set_port_flow(p.comp, "Air", Some(800.0)).unwrap();
    let c = m.mep_check(sa).unwrap();
    assert!(close(c.total, 800.0, 1e-9));
    assert!(close(c.runs[0].flow, 800.0, 1e-9));
    let sugg = c.issues.iter().find_map(|i| match i {
        MepIssue::Undersized { suggest, .. } => Some(suggest.clone()),
        _ => None,
    });
    assert_eq!(sugg.as_deref(), Some("Ø200"));
    assert!(has(&c, |i| matches!(i, MepIssue::PortSize { .. })));
    assert_eq!(c.errors(), 0, "speed is a guide value, not an error");
    assert!(has(&c, |i| i.key() == "mep.issue.undersized"
        && i.args().iter().any(|a| a.0 == "suggest")));

    let mut s = m.mep_run(run).unwrap();
    s.section.shape = Shape::Round { d: 0.25 };
    m.update_mep_run(run, &s).unwrap();
    let c = m.mep_check(sa).unwrap();
    assert_eq!(c.errors(), 0, "{:?}", c.issues);
    assert!(c.runs[0].speed.is_some_and(|v| v < 8.0));
    assert!(!has(&c, |i| matches!(i, MepIssue::Undersized { .. })));
    assert!(has(&c, |i| matches!(i, MepIssue::OpenEnd { end: 0, .. })));
}

#[test]
fn broken_and_flat_networks_are_reported() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.2,
        vec![(0.0, 0.0, 3.0), (4.0, 0.0, 3.0)],
    ))
    .unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.2,
        vec![(0.0, 5.0, 3.0), (4.0, 5.0, 3.0)],
    ))
    .unwrap();
    let c = m.mep_check(sa).unwrap();
    assert_eq!(c.parts, 2);
    assert!(has(&c, |i| matches!(i, MepIssue::Broken { parts: 2 })));
    assert!(has(&c, |i| matches!(i, MepIssue::NoFlow)));

    let san = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    let flat = m
        .add_mep_run(
            &RunSpec::pipe(
                san,
                l,
                PipeMaterial::Pvc,
                100,
                vec![(0.0, 9.0, 1.0), (3.0, 9.0, 1.0)],
            )
            .unwrap(),
        )
        .unwrap();
    let c = m.mep_check(san).unwrap();
    assert!(has(
        &c,
        |i| matches!(i, MepIssue::SlopeLow { run, .. } if *run == flat)
    ));
    assert!(!c.issues.iter().all(|i| i.is_error()));
}

#[test]
fn branch_rising_towards_the_main_is_a_reverse_slope() {
    let (mut m, l) = model();
    let san = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            san,
            l,
            PipeMaterial::Pvc,
            100,
            vec![(0.0, 0.0, 0.5), (10.0, 0.0, 0.4)],
        )
        .unwrap(),
    )
    .unwrap();
    let branch = m
        .add_mep_run(
            &RunSpec::pipe(
                san,
                l,
                PipeMaterial::Pvc,
                50,
                vec![(5.0, 3.0, 0.3), (5.0, 0.0, 0.45)],
            )
            .unwrap(),
        )
        .unwrap();
    let c = m.mep_check(san).unwrap();
    let reversed: Vec<i64> = c
        .issues
        .iter()
        .filter_map(|i| match i {
            MepIssue::SlopeReverse { run } => Some(*run),
            _ => None,
        })
        .collect();
    assert_eq!(reversed, vec![branch], "the branch, not the main");
    assert!(c
        .issues
        .iter()
        .any(|i| i.is_error() && i.run() == Some(branch)));
}

#[test]
fn hump_inside_a_run_is_a_reverse_slope() {
    let (mut m, l) = model();
    let san = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    let run = m
        .add_mep_run(
            &RunSpec::pipe(
                san,
                l,
                PipeMaterial::Pvc,
                100,
                vec![(0.0, 0.0, 0.5), (5.0, 0.0, 0.6), (10.0, 0.0, 0.4)],
            )
            .unwrap(),
        )
        .unwrap();
    let c = m.mep_check(san).unwrap();
    assert!(has(
        &c,
        |i| matches!(i, MepIssue::SlopeReverse { run: r } if *r == run)
    ));
}

#[test]
fn even_fall_along_bends_is_not_a_reverse_slope() {
    let (mut m, l) = model();
    let san = m.add_mep_system("SAN", SystemKind::Sanitary).unwrap();
    m.add_mep_run(
        &RunSpec::pipe(
            san,
            l,
            PipeMaterial::Pvc,
            100,
            vec![(0.0, 0.0, 0.5), (5.0, 0.0, 0.4), (5.0, 5.0, 0.3)],
        )
        .unwrap(),
    )
    .unwrap();
    let c = m.mep_check(san).unwrap();
    assert!(!has(&c, |i| matches!(i, MepIssue::SlopeReverse { .. })));
}

#[test]
fn zero_flow_of_connected_devices_has_its_own_text() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
    ))
    .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(sa),
        ))
        .unwrap();
    m.mep_set_port_flow(p.comp, "Air", Some(0.0)).unwrap();
    let c = m.mep_check(sa).unwrap();
    assert!(has(&c, |i| matches!(i, MepIssue::ZeroFlow)));
    assert!(!has(&c, |i| matches!(i, MepIssue::NoFlow)));
}

#[test]
fn size_change_at_a_corner_gets_an_elbow_and_a_transition() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 3.0), (6.0, 0.0, 3.0)],
    ))
    .unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.2,
        vec![(6.0, 0.0, 3.0), (6.0, 4.0, 3.0)],
    ))
    .unwrap();
    let net = m.mep_network(Some(sa)).unwrap();
    assert_eq!(net.count(FittingKind::Elbow), 1);
    assert_eq!(net.count(FittingKind::Transition), 1);
    let el = net
        .fittings
        .iter()
        .find(|f| f.kind == FittingKind::Elbow)
        .unwrap();
    assert_eq!(el.size, "Ø200", "the elbow takes the smaller section");
    let tr = net
        .fittings
        .iter()
        .find(|f| f.kind == FittingKind::Transition)
        .unwrap();
    assert!(tr.at.0 < 6.0 && close(tr.at.1, 0.0, 1e-9), "{:?}", tr.at);
    let meshes = m.mep_meshes(Some(sa)).unwrap();
    assert!(meshes.iter().all(|x| x.2.is_watertight()));
    // a straight joint stays a plain transition
    let ex = m.add_mep_system("EX", SystemKind::ExhaustAir).unwrap();
    m.add_mep_run(&round_duct(
        ex,
        l,
        0.4,
        vec![(0.0, 9.0, 3.0), (6.0, 9.0, 3.0)],
    ))
    .unwrap();
    m.add_mep_run(&round_duct(
        ex,
        l,
        0.2,
        vec![(6.0, 9.0, 3.0), (9.0, 9.0, 3.0)],
    ))
    .unwrap();
    let net = m.mep_network(Some(ex)).unwrap();
    assert_eq!(net.count(FittingKind::Elbow), 0);
    assert_eq!(net.count(FittingKind::Transition), 1);
}

#[test]
fn broken_network_does_not_block_new_work() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    // each run is fine alone, the joint has no room for an elbow: as an import would leave it
    let bad = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.4,
            vec![(0.0, 0.0, 3.0), (0.05, 0.0, 3.0)],
        ))
        .unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.05, 0.0, 3.0), (0.05, 4.0, 3.0)],
    ))
    .unwrap();
    assert!(m.mep_network(Some(sa)).is_err());
    let run = m
        .mep_draw(
            "SA",
            SystemKind::SupplyAir,
            &round_duct(0, l, 0.2, vec![(0.0, 9.0, 3.0), (6.0, 9.0, 3.0)]),
        )
        .unwrap();
    assert_eq!(m.mep_run(run).unwrap().system, sa);
    m.mep_cap_end(bad, 0, true).unwrap();
}

#[test]
fn stub_may_not_leave_a_sideways_port_through_the_device() {
    let main = [v3(0.0, 0.0, 1.0), v3(2.0, 0.0, 1.0)];
    let (p, d) = (v3(3.0, 0.0, 1.0), v3(1.0, 0.0, 0.0));
    let paths = super::manual::attach_paths(p, d, 0.1, &main, 0.0, true);
    assert!(paths
        .iter()
        .all(|x| x.len() < 2 || (x[1] - p).normalized().dot(d) > 0.0));
    // facing the main the straight stub stays
    let paths = super::manual::attach_paths(p, v3(-1.0, 0.0, 0.0), 0.1, &main, 0.0, true);
    assert!(paths.iter().any(|x| x.len() == 2));
}

#[test]
fn conduit_overload_names_the_current() {
    let (mut m, l) = model();
    let p = m
        .mep_place(&device(
            "Ceiling Light",
            "Round 300",
            l,
            (2.0, 2.0, 2.7),
            None,
        ))
        .unwrap();
    let at = m.world_port(p.comp, "Power").unwrap().at;
    let run = m
        .mep_draw(
            "PW",
            SystemKind::Power,
            &conduit(0, l, 0.02, vec![(0.0, 2.0, at.2), at]),
        )
        .unwrap();
    let pw = m.mep_run(run).unwrap().system;
    m.mep_set_port_flow(p.comp, "Power", Some(5000.0)).unwrap();
    let c = m.mep_check(pw).unwrap();
    assert!(close(c.total, 5000.0, 1e-9));
    assert!(has(
        &c,
        |i| matches!(i, MepIssue::Overload { amps, .. } if *amps > 16.0)
    ));
}

#[test]
fn deleting_a_component_or_system_drops_its_links_and_undo_returns_them() {
    let (mut m, l) = model();
    let pw = m.add_mep_system("PW", SystemKind::Power).unwrap();
    let p = m
        .mep_place(&device(
            "Ceiling Light",
            "Round 300",
            l,
            (2.0, 2.0, 2.7),
            Some(pw),
        ))
        .unwrap();
    m.delete_component(p.comp).unwrap();
    assert!(m.mep_port_links(None).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.mep_port_links(Some(pw)).unwrap().len(), 1);
    m.delete_mep_system(pw).unwrap();
    assert!(m.mep_port_links(None).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.mep_port_links(Some(pw)).unwrap().len(), 1);
}

#[test]
fn route_from_a_port_to_a_run_at_height() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let main = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.4,
            vec![(0.0, 0.0, 3.2), (8.0, 0.0, 3.2)],
        ))
        .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (4.0, 4.0, 2.7),
            None,
        ))
        .unwrap();
    let run = m
        .mep_route(&RouteSpec {
            from: RouteEnd::Port {
                comp: p.comp,
                connector: "Air".into(),
            },
            to: RouteEnd::Run {
                run: main,
                at: (1.0, 0.0, 3.2),
            },
            z: 3.0,
            slope: 0.0,
            level: l,
            system: None,
            template: None,
        })
        .unwrap();
    let links = m.mep_port_links(Some(sa)).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].stub, None, "a route is not a stub");
    assert!(m.mep_run(run).is_ok());
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 1);
    m.undo().unwrap();
    assert_eq!(m.mep_runs(None).unwrap(), vec![main]);
    assert!(m.mep_port_links(None).unwrap().is_empty());
}

#[test]
fn disconnect_keeps_an_auto_route_and_what_joins_it() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (4.0, 4.0, 2.7),
            None,
        ))
        .unwrap();
    let at = m.world_port(p.comp, "Air").unwrap().at;
    let run = m
        .mep_route(&RouteSpec {
            from: RouteEnd::Port {
                comp: p.comp,
                connector: "Air".into(),
            },
            to: RouteEnd::Free { x: 20.0, y: 4.0 },
            z: at.2,
            slope: 0.0,
            level: l,
            system: Some(sa),
            template: None,
        })
        .unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.1,
        vec![(10.0, 8.0, at.2), (10.0, 4.0, at.2)],
    ))
    .unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 1);
    m.mep_detach(p.comp, "Air").unwrap();
    assert_eq!(m.mep_runs(Some(sa)).unwrap().len(), 2, "the route stays");
    assert!(m.mep_port_links(None).unwrap().is_empty());
    let moved = m.mep_run(run).unwrap().points[0];
    assert!(moved.0 > at.0 + 1e-3, "{moved:?}");
    assert_eq!(m.mep_network(Some(sa)).unwrap().count(FittingKind::Tee), 1);
    m.undo().unwrap();
    assert_eq!(m.mep_port_links(Some(sa)).unwrap().len(), 1);
    assert!(close(m.mep_run(run).unwrap().points[0].0, at.0, 1e-9));
}

#[test]
fn route_refuses_a_port_of_another_network() {
    let (mut m, l) = model();
    let cw = m.add_mep_system("CW", SystemKind::ColdWater).unwrap();
    let main = m
        .add_mep_run(
            &RunSpec::pipe(
                cw,
                l,
                PipeMaterial::Steel,
                50,
                vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)],
            )
            .unwrap(),
        )
        .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (4.0, 4.0, 2.7),
            None,
        ))
        .unwrap();
    let r = m.mep_route(&RouteSpec {
        from: RouteEnd::Port {
            comp: p.comp,
            connector: "Air".into(),
        },
        to: RouteEnd::Run {
            run: main,
            at: (1.0, 0.0, 3.0),
        },
        z: 3.0,
        slope: 0.0,
        level: l,
        system: None,
        template: None,
    });
    assert!(r.is_err());
    assert_eq!(m.mep_runs(None).unwrap(), vec![main]);
    assert!(m.mep_port_links(None).unwrap().is_empty());
}

#[test]
fn port_on_the_middle_of_a_run_is_refused_and_repeat_keeps_the_stub() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (4.0, 4.0, 2.7),
            None,
        ))
        .unwrap();
    let at = m.world_port(p.comp, "Air").unwrap().at;
    let main = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.4,
            vec![(0.0, at.1, at.2), (8.0, at.1, at.2)],
        ))
        .unwrap();
    assert!(m.mep_attach(p.comp, "Air", Some(sa)).is_err());
    assert!(m.mep_port_links(None).unwrap().is_empty());
    m.delete_mep_run(main).unwrap();

    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, at.2), (8.0, 0.0, at.2)],
    ))
    .unwrap();
    let stub = m.mep_attach(p.comp, "Air", Some(sa)).unwrap();
    assert!(stub.is_some());
    assert!(m.mep_attach(p.comp, "Air", Some(sa)).is_err());
    assert_eq!(m.mep_port_links(Some(sa)).unwrap()[0].stub, stub);
}

#[test]
fn stub_references_follow_deleted_runs_components_and_systems() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
    ))
    .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(sa),
        ))
        .unwrap();
    let stub = p.runs[0];
    // the stub goes: the number is free to be reused
    m.delete_mep_run(stub).unwrap();
    assert_eq!(m.mep_port_links(Some(sa)).unwrap()[0].stub, None);
    m.undo().unwrap();
    assert_eq!(m.mep_port_links(Some(sa)).unwrap()[0].stub, Some(stub));
    // the device goes with its stub
    m.delete_component(p.comp).unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 1);
    assert!(m.mep_port_links(None).unwrap().is_empty());
    m.undo().unwrap();
    assert_eq!(m.mep_runs(None).unwrap().len(), 2);
    assert_eq!(m.mep_port_links(Some(sa)).unwrap()[0].stub, Some(stub));
    // the system goes, the typed flow stays
    m.mep_set_port_flow(p.comp, "Air", Some(123.0)).unwrap();
    m.delete_mep_system(sa).unwrap();
    let links = m.mep_port_links(None).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!((links[0].system, links[0].stub), (None, None));
    assert_eq!(links[0].flow, Some(123.0));
    m.undo().unwrap();
    assert_eq!(m.mep_port_links(Some(sa)).unwrap()[0].stub, Some(stub));
}

#[test]
fn disconnect_removes_the_stub_unless_something_joins_it() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(
        sa,
        l,
        0.4,
        vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)],
    ))
    .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "600x600",
            l,
            (3.0, 2.0, 2.7),
            Some(sa),
        ))
        .unwrap();
    let stub = m.mep_run(p.runs[0]).unwrap();
    let mid = (
        (stub.points[0].0 + stub.points[stub.points.len() - 1].0) / 2.0,
        (stub.points[0].1 + stub.points[stub.points.len() - 1].1) / 2.0,
        stub.points[0].2,
    );
    let branch = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.1,
            vec![(mid.0 + 3.0, mid.1, mid.2), mid],
        ))
        .unwrap();
    m.mep_detach(p.comp, "Air").unwrap();
    assert!(m.mep_run(p.runs[0]).is_ok(), "a branch joins it");
    m.undo().unwrap();
    m.delete_mep_run(branch).unwrap();
    m.mep_detach(p.comp, "Air").unwrap();
    assert!(m.mep_run(p.runs[0]).is_err());
}

#[test]
fn free_route_needs_a_system_and_a_section() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let mut spec = RouteSpec {
        from: RouteEnd::Free { x: 0.0, y: 0.0 },
        to: RouteEnd::Free { x: 6.0, y: 4.0 },
        z: 3.0,
        slope: 0.0,
        level: l,
        system: None,
        template: None,
    };
    assert!(m.mep_route(&spec).is_err());
    spec.system = Some(sa);
    assert!(m.mep_route(&spec).is_err());
    spec.template = Some(round_duct(0, l, 0.2, vec![]));
    let run = m.mep_route(&spec).unwrap();
    assert_eq!(m.mep_run(run).unwrap().points.len(), 3);
    assert_eq!(
        m.mep_network(Some(sa)).unwrap().count(FittingKind::Elbow),
        1
    );
}

#[test]
fn cap_closes_an_open_end() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let r = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.2,
            vec![(0.0, 0.0, 3.0), (4.0, 0.0, 3.0)],
        ))
        .unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().open_ends.len(), 2);
    m.mep_cap_end(r, 1, true).unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().open_ends.len(), 1);
    m.undo().unwrap();
    assert_eq!(m.mep_network(Some(sa)).unwrap().open_ends.len(), 2);
}

#[test]
fn insulation_shows_the_inner_line_in_plan() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let r = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.2,
            vec![(0.0, 0.0, 1.0), (4.0, 0.0, 1.0)],
        ))
        .unwrap();
    assert!(m.mep_plan_insulation(l, Detail::Double).unwrap().is_empty());
    m.mep_set_system_insulation(sa, 0.03).unwrap();
    assert!(close(m.mep_run(r).unwrap().section.insulation, 0.03, 1e-12));
    let inner = m.mep_plan_insulation(l, Detail::Double).unwrap();
    assert!(!inner.is_empty());
    assert!(inner.iter().all(|p| matches!(p.prim, PlanPrim::Edge(_))));
    m.undo().unwrap();
    assert_eq!(m.mep_run(r).unwrap().section.insulation, 0.0);
    assert!(m.mep_set_system_insulation(sa, -1.0).is_err());
}

#[test]
fn snap_prefers_ports_then_run_ends_then_axes() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let r = m
        .add_mep_run(&round_duct(
            sa,
            l,
            0.2,
            vec![(0.0, 0.0, 2.0), (6.0, 0.0, 2.0)],
        ))
        .unwrap();
    let p = m
        .mep_place(&device(
            "Supply Diffuser",
            "300x300",
            l,
            (6.05, 0.0, 1.8),
            None,
        ))
        .unwrap();
    let set = m.mep_snap_set().unwrap();
    let all = |_: Domain, _: Option<i64>| true;
    let zs = (0.0, 3.0);
    match set.pick((6.05, 0.0), 0.2, zs, &all) {
        Some(MepHit::Port(w)) => assert_eq!(w.comp, p.comp),
        o => panic!("{o:?}"),
    }
    match set.pick((0.05, 0.0), 0.2, zs, &all) {
        Some(MepHit::RunEnd { run, end, .. }) => assert_eq!((run, end), (r, 0)),
        o => panic!("{o:?}"),
    }
    match set.pick((3.0, 0.1), 0.2, zs, &all) {
        Some(MepHit::RunMid { at, .. }) => assert!(close(at.0, 3.0, 1e-9)),
        o => panic!("{o:?}"),
    }
    assert!(set.pick((3.0, 0.1), 0.2, (5.0, 6.0), &all).is_none());
    let none = |_: Domain, _: Option<i64>| false;
    assert!(set.pick((3.0, 0.1), 0.2, zs, &none).is_none());
}

#[test]
fn new_library_families_have_expected_ports_and_defaults() {
    let (mut m, l) = model();
    let ahu = m
        .mep_place(&device(
            "Air Handling Unit",
            "2000x1000",
            l,
            (0.0, 0.0, 0.0),
            None,
        ))
        .unwrap();
    let ports = m.component_ports(Some(ahu.comp)).unwrap();
    assert_eq!(ports.len(), 2);
    assert!(close(ports[0].at.2, 1.4, 1e-9));
    assert!(m.port_defaults(ahu.comp).unwrap()["Supply"].1, "source");
    let grille = m
        .mep_place(&device(
            "Exhaust Grille",
            "400x400",
            l,
            (0.0, 3.0, 2.7),
            None,
        ))
        .unwrap();
    assert!(close(
        m.mep_port_flow(grille.comp, "Air").unwrap(),
        200.0,
        1e-9
    ));
}

/// A diffuser under a main duct with the stub the placement drew; returns (model, system, comp, stub).
fn diffuser_on_main() -> (Model, i64, i64, i64) {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    m.add_mep_run(&round_duct(sa, l, 0.4, vec![(0.0, 0.0, 2.9), (8.0, 0.0, 2.9)]))
        .unwrap();
    let p = m
        .mep_place(&device("Supply Diffuser", "600x600", l, (3.0, 2.0, 2.7), Some(sa)))
        .unwrap();
    (m, sa, p.comp, p.runs[0])
}

fn stub_end_at_port(m: &Model, comp: i64, stub: i64) -> bool {
    let port = m.world_port(comp, "Air").unwrap();
    let s = m.mep_run(stub).unwrap();
    let e = s.pts()[0];
    (e - v3(port.at.0, port.at.1, port.at.2)).len() < PORT_TOL
}

#[test]
fn lifting_a_device_drags_its_stub_and_undo_is_one_step() {
    let (mut m, _, comp, stub) = diffuser_on_main();
    assert!(stub_end_at_port(&m, comp, stub));
    let before = m.mep_run(stub).unwrap();
    let mut s = m.component(comp).unwrap();
    s.at.2 += 0.1;
    m.mep_update_device(comp, &s).unwrap();
    assert!(stub_end_at_port(&m, comp, stub), "the stub must follow the connector");
    assert_ne!(m.mep_run(stub).unwrap().points, before.points);
    m.undo().unwrap();
    assert_eq!(m.mep_run(stub).unwrap(), before);
    assert_eq!(m.component(comp).unwrap().at.2, 2.7);
}

#[test]
fn moving_a_device_in_plan_drags_its_stub_and_copy_stays_free() {
    let (mut m, _, comp, stub) = diffuser_on_main();
    let before = m.mep_run(stub).unwrap();
    m.move_elements(&[crate::Target::Component(comp)], (1.0, 0.0))
        .unwrap();
    assert!(stub_end_at_port(&m, comp, stub));
    m.undo().unwrap();
    assert_eq!(m.mep_run(stub).unwrap(), before);

    let runs = m.mep_runs(None).unwrap().len();
    let links = m.mep_port_links(None).unwrap().len();
    let new = m
        .copy_elements(&[crate::Target::Component(comp)], &[(0.0, 1.0)])
        .unwrap();
    assert_eq!(new.len(), 1);
    assert_eq!(m.mep_runs(None).unwrap().len(), runs, "a copy brings no run");
    assert_eq!(m.mep_port_links(None).unwrap().len(), links, "a copy is not connected");
    assert_eq!(m.mep_run(stub).unwrap(), before, "the original stays connected");
}

#[test]
fn a_run_on_a_device_connector_cannot_be_lifted_away() {
    let (mut m, _, comp, stub) = diffuser_on_main();
    let before = m.mep_run(stub).unwrap();
    let mut s = before.clone();
    s.points.iter_mut().for_each(|p| p.2 += 0.5);
    let e = m.mep_edit_run(stub, &s).unwrap_err();
    assert_eq!(e.to_string(), Error::Invalid("mep.err.follow_device".into()).to_string());
    assert_eq!(m.mep_run(stub).unwrap(), before);
    assert!(stub_end_at_port(&m, comp, stub));
}

#[test]
fn lifting_a_main_drags_the_branch_that_joins_it() {
    let (mut m, l) = model();
    let sa = m.add_mep_system("SA", SystemKind::SupplyAir).unwrap();
    let main = m
        .add_mep_run(&round_duct(sa, l, 0.3, vec![(0.0, 0.0, 3.0), (8.0, 0.0, 3.0)]))
        .unwrap();
    let br = m
        .add_mep_run(&round_duct(sa, l, 0.2, vec![(4.0, 0.0, 3.0), (4.0, 5.0, 3.0)]))
        .unwrap();
    let before = m.mep_run(br).unwrap();
    let mut s = m.mep_run(main).unwrap();
    s.points.iter_mut().for_each(|p| p.2 += 0.5);
    m.mep_edit_run(main, &s).unwrap();
    let b = m.mep_run(br).unwrap();
    assert!(close(b.points[0].2, 3.5, 1e-9), "{:?}", b.points);
    assert!(close(b.points[1].2, 3.0, 1e-9), "the far end stays");
    m.undo().unwrap();
    assert_eq!(m.mep_run(br).unwrap(), before);
    assert!(close(m.mep_run(main).unwrap().points[0].2, 3.0, 1e-9));
}

#[test]
fn a_device_type_without_the_connector_is_refused_while_a_run_sits_on_it() {
    let (mut m, _, comp, stub) = diffuser_on_main();
    let before = m.mep_run(stub).unwrap();
    let fam = m.load_library_family("Sink").unwrap();
    let mut s = m.component(comp).unwrap();
    s.ctype = m.comp_types(fam).unwrap()[0].0;
    let e = m.mep_update_device(comp, &s).unwrap_err();
    assert_eq!(e.to_string(), Error::Invalid("mep.err.follow_port_lost".into()).to_string());
    assert_eq!(m.mep_run(stub).unwrap(), before);
}
