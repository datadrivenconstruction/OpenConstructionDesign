// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::f64::consts::PI;
use std::time::Instant;

use super::*;
use crate::{CompSpec, LayerSpec, OpeningSpec, SlabSpec, WallSpec};

fn layer(t: f64) -> Vec<LayerSpec> {
    vec![LayerSpec {
        material: "Concrete".into(),
        thickness: t,
    }]
}

fn place(m: &mut Model, family: &str, ty: &str, l: i64, at: (f64, f64, f64), rot: f64) -> i64 {
    let f = m.load_library_family(family).unwrap();
    let ct = m.comp_type_id(f, ty).unwrap().unwrap();
    let mut s = CompSpec::at(l, ct, at.0, at.1);
    s.at.2 = at.2;
    s.rotation = rot;
    match m.add_component(&s) {
        Ok(id) => return id,
        Err(e) if e.to_string().contains("wall-hosted") => {}
        Err(e) => panic!("{e}"),
    }
    // wall-hosted: stand on the nearest wall of the level, facing the room (left of the axis)
    let walls: Vec<(i64, f64, f64, f64, f64)> = {
        let mut st = m
            .sql()
            .prepare("SELECT id,ax,ay,bx,by FROM wall WHERE level=?1")
            .unwrap();
        let rows = st
            .query_map([l], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    };
    let along = |w: &(i64, f64, f64, f64, f64)| {
        let (dx, dy) = (w.3 - w.1, w.4 - w.2);
        let len = dx.hypot(dy);
        let t = (((at.0 - w.1) * dx + (at.1 - w.2) * dy) / len).clamp(0.0, len);
        let (px, py) = (w.1 + dx / len * t, w.2 + dy / len * t);
        (t, (at.0 - px).hypot(at.1 - py))
    };
    let w = walls
        .iter()
        .min_by(|a, b| along(a).1.total_cmp(&along(b).1))
        .expect("a wall to host on");
    s.host = crate::CompHost::Wall(w.0);
    let (dx, dy) = (w.3 - w.1, w.4 - w.2);
    let left = (dx * (at.1 - w.2) - dy * (at.0 - w.1)) / dx.hypot(dy);
    s.at = (along(w).0, left, at.2);
    s.rotation = 0.0;
    m.add_component(&s).unwrap()
}

/// Two storeys of 24 x 12 m; on the upper one a corridor (y 5..7) between two rows of six
/// bathrooms 4 m wide, each with a WC and a washbasin on its outer wall.
fn bathrooms() -> (Model, i64, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let l1 = m.add_level("L1", 0.0).unwrap();
    let l2 = m.add_level("L2", 3.2).unwrap();
    let l3 = m.add_level("L3", 6.4).unwrap();
    let st = m.add_slab_type("S200", &layer(0.2)).unwrap();
    for l in [l2, l3] {
        m.add_slab(&SlabSpec {
            level: l,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, 0.0), (24.0, 0.0), (24.0, 12.0), (0.0, 12.0)],
            holes: vec![],
        })
        .unwrap();
    }
    let wt = m.add_wall_type("W200", &layer(0.2)).unwrap();
    let wall = |m: &mut Model, a, b| {
        m.add_wall(&WallSpec {
            a,
            b,
            level: l2,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap()
    };
    for (a, b) in [
        ((0.0, 0.0), (24.0, 0.0)),
        ((24.0, 0.0), (24.0, 12.0)),
        ((24.0, 12.0), (0.0, 12.0)),
        ((0.0, 12.0), (0.0, 0.0)),
    ] {
        wall(&mut m, a, b);
    }
    for y in [5.0, 7.0] {
        let w = wall(&mut m, (0.0, y), (24.0, y));
        for k in 0..6 {
            m.add_opening(
                w,
                &OpeningSpec {
                    along: 4.0 * k as f64 + 2.6,
                    width: 0.8,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        }
    }
    for k in 1..6 {
        let x = 4.0 * k as f64;
        wall(&mut m, (x, 0.0), (x, 5.0));
        wall(&mut m, (x, 7.0), (x, 12.0));
    }
    let mut fixtures = vec![];
    for k in 0..6 {
        let x = 4.0 * k as f64;
        // south rooms: back wall face y = 0.1; north rooms: y = 11.9, turned
        fixtures.push(place(&mut m, "WC", "Floor 650", l2, (x + 0.8, 0.1, 0.0), 0.0));
        fixtures.push(place(&mut m, "Washbasin", "500", l2, (x + 2.0, 0.1, 0.0), 0.0));
        fixtures.push(place(&mut m, "WC", "Floor 650", l2, (x + 3.2, 11.9, 0.0), PI));
        fixtures.push(place(&mut m, "Washbasin", "500", l2, (x + 2.0, 11.9, 0.0), PI));
    }
    let _ = l1;
    (m, l2, fixtures)
}

fn ports_of(m: &Model, comps: &[i64], kind: SystemKind) -> Vec<(i64, String)> {
    m.component_ports(None)
        .unwrap()
        .into_iter()
        .filter(|p| comps.contains(&p.comp) && p.system == Some(kind))
        .map(|p| (p.comp, p.name))
        .collect()
}

#[test]
fn gate_twelve_bathrooms_water_and_drains_in_one_step() {
    let (mut m, l, fx) = bathrooms();
    let t = Instant::now();
    let mut reqs = vec![];
    for (name, kind, x) in [
        ("Cold", SystemKind::ColdWater, 12.0),
        ("Hot", SystemKind::HotWater, 12.3),
        ("Waste", SystemKind::Sanitary, 11.5),
    ] {
        let mut r = NetReq::new(name, kind, l, RouteFrom::Riser { x, y: 6.0 });
        r.to = ports_of(&m, &fx, kind);
        reqs.push(r);
    }
    assert_eq!(reqs[0].to.len(), 24);
    assert_eq!(reqs[1].to.len(), 12);
    assert_eq!(reqs[2].to.len(), 24);
    let plan = m.mep_plan_all(&reqs).unwrap();
    assert!(plan.metrics.failed.is_empty(), "{:?}", plan.metrics.failed);
    assert_eq!(plan.metrics.clashes, 0);
    let ops = m.journal_len().unwrap();
    let built = m.mep_build(&plan.build).unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert!(secs <= 30.0, "{secs} s");
    assert_eq!(m.journal_len().unwrap(), ops + 1, "one undo step");
    assert_eq!(built.systems.len(), 3);
    // nothing crosses the structure: walls have sleeves, slabs holes
    let clashes = m.mep_clashes(None, 1e-6).unwrap();
    assert!(clashes.is_empty(), "{clashes:?}");
    assert!(plan.metrics.sleeves > 0 && plan.metrics.holes >= 3);
    // drains fall at least 2 % towards the riser, nowhere backwards
    let waste = built.systems[2];
    let check = m.mep_check(waste).unwrap();
    for i in &check.issues {
        assert!(
            !matches!(i, MepIssue::SlopeLow { .. } | MepIssue::SlopeReverse { .. }),
            "{i:?}"
        );
    }
    assert!(plan.metrics.min_slope.is_some_and(|s| s >= 2.0 - 1e-6));
    for s in &built.systems {
        let net = m.mep_network(Some(*s)).unwrap();
        assert_eq!(net.parts(), 1, "system {s} is one network");
    }
    m.undo().unwrap();
    assert!(m.mep_runs(None).unwrap().is_empty());
}

#[test]
fn variants_differ_and_report_metrics() {
    let (m, l, fx) = bathrooms();
    let mut r = NetReq::new("Cold", SystemKind::ColdWater, l, RouteFrom::Riser { x: 12.0, y: 6.0 });
    r.to = ports_of(&m, &fx[..4], SystemKind::ColdWater);
    let v = m.mep_plan_variants(&r).unwrap();
    assert_eq!(v.len(), VARIANTS);
    for p in &v {
        assert!(p.metrics.failed.is_empty());
        assert!(p.metrics.length > 0.0 && p.metrics.tees >= 1);
        assert!(p.metrics.z_max < 6.2, "under the slab above");
    }
    assert!(v[1].metrics.elbows <= v[0].metrics.elbows);
}

/// One room 20 x 10 m: a panel and 20 sockets on its walls.
fn office() -> (Model, i64, i64, Vec<i64>) {
    let mut m = Model::open(":memory:").unwrap();
    let l = m.add_level("L1", 0.0).unwrap();
    m.add_level("L2", 3.2).unwrap();
    let wt = m.add_wall_type("W200", &layer(0.2)).unwrap();
    for (a, b) in [
        ((0.0, 0.0), (20.0, 0.0)),
        ((20.0, 0.0), (20.0, 10.0)),
        ((20.0, 10.0), (0.0, 10.0)),
        ((0.0, 10.0), (0.0, 0.0)),
    ] {
        m.add_wall(&WallSpec {
            a,
            b,
            level: l,
            base_offset: 0.0,
            height: 3.2,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    }
    let panel = place(&mut m, "Distribution Board", "400x600", l, (10.0, 0.1, 1.2), 0.0);
    let mut sockets = vec![];
    for k in 0..10 {
        let x = 1.0 + 2.0 * k as f64;
        sockets.push(place(&mut m, "Socket Outlet", "Single", l, (x, 9.9, 0.3), PI));
    }
    for k in 0..5 {
        let y = 1.5 + 1.8 * k as f64;
        sockets.push(place(&mut m, "Socket Outlet", "Single", l, (0.1, y, 0.3), -PI / 2.0));
        sockets.push(place(&mut m, "Socket Outlet", "Single", l, (19.9, y, 0.3), PI / 2.0));
    }
    (m, l, panel, sockets)
}

#[test]
fn gate_panel_three_circuits_twenty_sockets() {
    let (mut m, l, panel, sockets) = office();
    let devs: Vec<(i64, String)> = sockets.iter().map(|s| (*s, "Power".to_string())).collect();
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    assert!(plan.metrics.failed.is_empty(), "{:?}", plan.metrics.failed);
    assert_eq!(plan.build.circuits.len(), 3);
    let built = m.mep_build(&plan.build).unwrap();
    assert_eq!(built.circuits.len(), 3);
    let s = m.panel_schedule(panel).unwrap();
    assert_eq!(s.rows.len(), 3);
    assert_eq!(s.rows.iter().map(|r| r.devices).sum::<usize>(), 20);
    assert!((s.load - 20.0 * 200.0).abs() < 1e-6, "{}", s.load);
    for r in &s.rows {
        assert_eq!(r.kind, CircuitKind::Power);
        assert!(r.length > 0.0 && r.drop > 0.0, "{r:?}");
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert!(r.current <= r.breaker);
    }
    assert!(m.mep_clashes(None, 1e-6).unwrap().is_empty());
    // a deleted socket leaves its circuit, undo brings it back
    m.delete_component(sockets[0]).unwrap();
    let n: usize = m.circuits(Some(panel)).unwrap().iter().map(|c| c.spec.devices.len()).sum();
    assert_eq!(n, 19);
    m.undo().unwrap();
    let n: usize = m.circuits(Some(panel)).unwrap().iter().map(|c| c.spec.devices.len()).sum();
    assert_eq!(n, 20);
}

#[test]
fn circuit_overload_and_drop_are_reported() {
    let (mut m, _l, panel, sockets) = office();
    let mut c = CircuitSpec::new(
        panel,
        CircuitKind::Power,
        sockets
            .iter()
            .map(|s| CircuitDevice {
                comp: *s,
                connector: "Power".into(),
                load: Some(300.0),
            })
            .collect(),
    );
    c.breaker = 10.0;
    let id = m.add_circuit(&c).unwrap();
    let cs = m.circuits(Some(panel)).unwrap();
    assert_eq!(cs[0].id, id);
    assert_eq!(cs[0].spec.number, 1);
    let r = m.circuit_report(&cs[0]).unwrap();
    assert!((r.load - 6000.0).abs() < 1e-9);
    assert!(r.issues.iter().any(|i| matches!(i, CircuitIssue::Overload { .. })));
    // not routed: no system
    assert!(r.issues.iter().any(|i| matches!(i, CircuitIssue::NotRouted { .. })));
    // way taken
    let mut d = CircuitSpec::new(panel, CircuitKind::Lighting, vec![]);
    d.number = 1;
    assert!(m.add_circuit(&d).is_err());
    m.undo().unwrap();
    assert!(m.circuits(None).unwrap().is_empty());
}

#[test]
fn drop_follows_the_formula_on_a_straight_feed() {
    let (mut m, l, panel, sockets) = office();
    let devs = vec![(sockets[0], "Power".to_string())];
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    m.mep_build(&plan.build).unwrap();
    let s = m.panel_schedule(panel).unwrap();
    let r = &s.rows[0];
    // 200 W at 230 V, cos 0.85 (SP 256 office): I = 200 / (230 * 0.85) = 1.02302 A on 2.5 mm2
    // copper (hot): drop V = 2 * I * L * (0.0225/2.5 * 0.85 + 0.0001 * sin), sin = sqrt(1 - 0.7225)
    assert_eq!(r.cos_phi, 0.85);
    let i = 200.0 / (230.0 * 0.85);
    let want = 2.0 * i * r.length * (0.0225 / 2.5 * 0.85 + 0.0001 * (1.0f64 - 0.7225).sqrt()) / 230.0 * 100.0;
    assert!((r.drop - want).abs() < 1e-9, "{} {}", r.drop, want);
}

#[test]
fn cable_log_has_a_row_per_consumer_with_laying_length() {
    let (mut m, l, panel, sockets) = office();
    let devs: Vec<_> = sockets.iter().map(|s| (*s, "Power".to_string())).collect();
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    m.mep_build(&plan.build).unwrap();
    let rows = m.cable_log(panel, "tray").unwrap();
    assert_eq!(rows.len(), sockets.len());
    let rules = crate::mep::CableRules::builtin();
    for r in &rows {
        assert!(r.route > 0.0);
        assert!((r.length - rules.cable_length("tray", r.route).unwrap()).abs() < 1e-9);
    }
    assert!(m.cable_log(panel, "none").unwrap().is_empty());
}

#[test]
fn panel_schedule_carries_table_rules_and_single_line() {
    let (mut m, l, panel, sockets) = office();
    let devs: Vec<_> = sockets.iter().map(|s| (*s, "Power".to_string())).collect();
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    m.mep_build(&plan.build).unwrap();
    let s = m.panel_schedule(panel).unwrap();
    let ids: Vec<_> = s.rows[0].rules.iter().map(|f| f.rule).collect();
    assert_eq!(ids, ["EL-BRK-01", "EL-BRK-02", "EL-BRK-03", "EL-CAB-01", "EL-DRP-01"]);
    let drop = s.rows[0].rules.iter().find(|f| f.rule == "EL-DRP-01").unwrap();
    assert!((drop.value - s.rows[0].drop).abs() < 1e-9);
    let d = m.single_line(panel).unwrap();
    assert_eq!(d.feeders.len(), s.rows.len());
    assert_eq!(d.connections().len(), s.rows.len());
    assert!(d.to_svg().contains("QF1"));
}

#[test]
fn cable_length_follows_the_built_route_not_the_straight_line() {
    let (mut m, l, panel, sockets) = office();
    let devs: Vec<_> = sockets.iter().map(|s| (*s, "Power".to_string())).collect();
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    m.mep_build(&plan.build).unwrap();
    let at = |comp: i64, port: &str| {
        let p = m.component_ports(Some(comp)).unwrap();
        p.iter().find(|w| w.name == port).map(|w| w.at).unwrap_or(p[0].at)
    };
    let pp = at(panel, "Power");
    let rows = m.cable_log(panel, "conduit_wall").unwrap();
    assert_eq!(rows.len(), sockets.len());
    let mut longer = 0;
    for r in &rows {
        let q = at(r.to, "Power");
        let d = ((pp.0 - q.0).powi(2) + (pp.1 - q.1).powi(2) + (pp.2 - q.2).powi(2)).sqrt();
        assert!(r.route + 1e-6 >= d, "route {} shorter than straight {d}", r.route);
        if r.route > d + 0.5 {
            longer += 1;
        }
    }
    // runs go round the room along the walls: most routes are clearly longer than the chord
    assert!(longer > rows.len() / 2, "{longer} of {}", rows.len());
    // the circuit report measures its farthest device on the same route
    for c in m.circuits(Some(panel)).unwrap() {
        let far = rows
            .iter()
            .filter(|r| r.circuit == c.spec.number)
            .map(|r| r.route)
            .fold(0.0, f64::max);
        let rep = m.circuit_report(&c).unwrap();
        assert!((rep.length - far).abs() < 1e-9, "{} {far}", rep.length);
    }
}

#[test]
fn automatic_circuits_pass_their_own_rules() {
    let (mut m, l, panel, sockets) = office();
    let devs: Vec<_> = sockets.iter().map(|s| (*s, "Power".to_string())).collect();
    let plan = m.mep_plan_power(panel, &devs, l, 0).unwrap();
    m.mep_build(&plan.build).unwrap();
    let s = m.panel_schedule(panel).unwrap();
    for r in &s.rows {
        assert!(r.rules.iter().all(|f| f.ok), "{} {:?}", r.number, r.rules);
        assert!(r.drop <= 5.0);
    }
    // three single-pole circuits spread over three phases
    assert!(s.phases.iter().all(|p| *p > 0.0));
    let sum: f64 = s.rows.iter().map(|r| r.current).sum();
    assert!(s.current < sum);
}

#[test]
fn three_poles_need_line_voltage() {
    let (mut m, _l, panel, sockets) = office();
    let dev = |s: i64| CircuitDevice { comp: s, connector: "Power".into(), load: Some(100.0) };
    let mut c = CircuitSpec::new(panel, CircuitKind::Power, vec![dev(sockets[0])]);
    c.poles = 3;
    assert!(m.add_circuit(&c).is_err(), "3 poles at 230 V");
    c.voltage = 400.0;
    assert!(m.add_circuit(&c).is_ok());
    let mut bad = CircuitSpec::new(panel, CircuitKind::Power, vec![dev(sockets[1])]);
    bad.devices[0].load = Some(f64::NAN);
    assert!(m.add_circuit(&bad).is_err());
    bad.devices[0].load = Some(-5.0);
    assert!(m.add_circuit(&bad).is_err());
}

#[test]
fn ambient_and_group_are_stored_and_derate_the_cable() {
    let (mut m, _l, panel, sockets) = office();
    let dev = |s: i64| CircuitDevice { comp: s, connector: "Power".into(), load: Some(100.0) };
    let mk = |m: &mut Model, s: i64, amb: f64, grp: u32| {
        let mut c = CircuitSpec::new(panel, CircuitKind::Power, vec![dev(s)]);
        c.laying = "tray".into();
        c.ambient_c = amb;
        c.grouped = grp;
        m.add_circuit(&c).unwrap();
    };
    // PUE 1.3.4, 2.5 mm2 copper in a pipe = 27 A: 27 * 1.00 * 1.00 passes a 16 A breaker;
    // 40 C and 6 circuits: 27 * 0.87 * 0.57 = 13.389 A does not
    mk(&mut m, sockets[0], 30.0, 1);
    mk(&mut m, sockets[1], 40.0, 6);
    let cs = m.circuits(Some(panel)).unwrap();
    assert_eq!((cs[1].spec.ambient_c, cs[1].spec.grouped), (40.0, 6));
    let brk2 = |c: &Circuit| {
        let r = m.circuit_report(c).unwrap();
        r.rules.iter().find(|f| f.rule == "EL-BRK-02").unwrap().clone()
    };
    assert!(brk2(&cs[0]).ok);
    let bad = brk2(&cs[1]);
    assert!(!bad.ok && (bad.limit - 13.3893).abs() < 1e-9, "{bad:?}");
    let mut c = CircuitSpec::new(panel, CircuitKind::Power, vec![]);
    c.grouped = 0;
    assert!(m.add_circuit(&c).is_err());
    c.grouped = 1;
    c.ambient_c = f64::NAN;
    assert!(m.add_circuit(&c).is_err());
}

/// Wet rooms stacked over each other: the storey above asks its risers at the same plan points.
/// Its cold and hot risers started 0.15 m below where the ones under them ended and were refused
/// as blocked by them; now each stands on the one below and the column is one network.
#[test]
fn risers_of_the_storey_above_continue_the_ones_below() {
    let (mut m, l2, fx) = bathrooms();
    let l3 = m.level_list().unwrap().into_iter().find(|l| l.1 == "L3").unwrap().0;
    let points = [
        ("Cold", SystemKind::ColdWater, 12.0),
        ("Hot", SystemKind::HotWater, 12.3),
        ("Waste", SystemKind::Sanitary, 11.5),
    ];
    let reqs_of = |m: &Model, l: i64, fx: &[i64]| {
        points
            .iter()
            .map(|&(name, kind, x)| {
                let mut r = NetReq::new(name, kind, l, RouteFrom::Riser { x, y: 6.0 });
                r.to = ports_of(m, fx, kind);
                r
            })
            .collect::<Vec<_>>()
    };
    let plan = m.mep_plan_all(&reqs_of(&m, l2, &fx[..4])).unwrap();
    m.mep_build(&plan.build).unwrap();
    let wt = m.add_wall_type("W200 upper", &layer(0.2)).unwrap();
    m.add_wall(&WallSpec {
        a: (0.0, 0.0),
        b: (24.0, 0.0),
        level: l3,
        base_offset: 0.0,
        height: 3.0,
        wall_type: wt,
        offset0: None,
    })
    .unwrap();
    let up = vec![
        place(&mut m, "WC", "Floor 650", l3, (12.8, 0.1, 0.0), 0.0),
        place(&mut m, "Washbasin", "500", l3, (14.0, 0.1, 0.0), 0.0),
    ];
    let plan = m.mep_plan_all(&reqs_of(&m, l3, &up)).expect("the storey above is not blocked");
    assert!(plan.metrics.failed.is_empty(), "{:?}", plan.metrics.failed);
    assert_eq!(plan.metrics.clashes, 0);
    m.mep_build(&plan.build).unwrap();
    for (name, _, x) in &points[..2] {
        let sys = m.mep_systems().unwrap().into_iter().find(|s| s.name == *name).unwrap().id;
        assert_eq!(m.mep_network(Some(sys)).unwrap().parts(), 1, "{name} is one network");
        // the risers at the point meet end to end: no length of the column is doubled
        let mut spans = vec![];
        for id in m.mep_runs(Some(sys)).unwrap() {
            let p = read_run(m.sql(), id).unwrap().pts();
            for w in p.windows(2) {
                let on = |q: ddc_geom::math::V3| (q.x - x).abs() < 1e-6 && (q.y - 6.0).abs() < 1e-6;
                if on(w[0]) && on(w[1]) {
                    spans.push((w[0].z.min(w[1].z), w[0].z.max(w[1].z)));
                }
            }
        }
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(spans.len() >= 2, "{name}: {spans:?}");
        for w in spans.windows(2) {
            assert!(w[1].0 >= w[0].1 - 1e-6, "{name} overlaps: {spans:?}");
        }
        assert!(spans.windows(2).any(|w| (w[1].0 - w[0].1).abs() < 1e-6), "{name}: {spans:?}");
    }
}

#[test]
fn route_table_has_one_cost_row_per_variant() {
    let t = include_str!("../../data/route_rules.json");
    let v: serde_json::Value = serde_json::from_str(t).unwrap();
    assert_eq!(v["variants"].as_array().unwrap().len(), VARIANTS);
}

/// Every pipe size of every material: the straight into a tee holds the fitting's junction
/// leg and the branch elbow's tangent, so no run built from a plan comes out short at a tee.
#[test]
fn a_tee_leg_holds_the_junction_leg_and_the_elbow_for_every_size() {
    use ddc_geom::mep::{junction_leg, PipeMaterial};
    for m in PipeMaterial::ALL {
        for x in m.sizes() {
            let mut sec = Section::new(Domain::Pipe, Shape::Round { d: x.od });
            sec.wall = x.wall;
            let need = junction_leg(&sec) + m.bend_radius(&x);
            let got = super::autoroute::tee_leg(0, (sec, Some(m), Some(x.dn)));
            assert!(got + 1e-9 >= need, "{m:?} DN{}: {got} < {need}", x.dn);
        }
    }
    for d in [0.1, 0.16, 0.315, 0.63] {
        let sec = Section::new(Domain::Duct, Shape::Round { d });
        let got = super::autoroute::tee_leg(0, (sec, None, None));
        assert!(got + 1e-9 >= d + 1.5 * d, "D{d}: {got}");
    }
}
