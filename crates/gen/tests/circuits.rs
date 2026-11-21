// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The circuits of the power design checked as an engineer checks a circuit list: every device
//! on one circuit, points and load within the limits, the cable holding its breaker, voltage
//! drop within budget, residual current devices where the norms ask, and the derived drawings.

use std::collections::HashMap;
use std::sync::OnceLock;

use ddc_gen::project::{build_project, parse_spec, schedule_csv, svc_ports, voltage_drop, CircuitUse, GCircuit, GProject, SvcSys};
use serde_json::{json, Value};

fn briefs() -> Vec<Value> {
    vec![
        json!({"typology": "house", "floors": 2, "services": true}),
        json!({"typology": "apartment_section", "floors": 5, "services": true}),
        json!({"typology": "office", "floors": 4, "services": true}),
        json!({"typology": "school", "floors": 3, "services": true}),
    ]
}

fn projects() -> &'static Vec<(String, GProject)> {
    static P: OnceLock<Vec<(String, GProject)>> = OnceLock::new();
    P.get_or_init(|| briefs().into_iter().map(|b| (b["typology"].as_str().unwrap().to_string(), build_project(&parse_spec(&b).unwrap()).unwrap())).collect())
}

fn housing(t: &str) -> bool {
    t == "house" || t == "apartment_section"
}

fn family<'a>(p: &'a GProject, d: &(usize, usize, &str)) -> &'a str {
    p.storeys[d.0].layout.fixtures[d.1].family
}

/// Iz of copper PVC cable, laying method C (DIN VDE 0298-4 Table 3): two and three loaded conductors.
fn iz(section: f64, poles: u32) -> f64 {
    let t = [(1.5, 19.5, 17.5), (2.5, 27.0, 24.0), (4.0, 36.0, 32.0), (6.0, 46.0, 41.0), (10.0, 63.0, 57.0), (16.0, 85.0, 76.0), (25.0, 112.0, 96.0), (35.0, 138.0, 119.0), (50.0, 168.0, 144.0), (70.0, 213.0, 184.0), (95.0, 258.0, 223.0)];
    let r = t.iter().find(|r| (r.0 - section).abs() < 1e-9).unwrap_or_else(|| panic!("section {section} not in the table"));
    if poles > 1 { r.2 } else { r.1 }
}

#[test]
fn every_figure_summed() {
    for (t, p) in projects() {
        let c = &p.services.circuits;
        let fin: Vec<&GCircuit> = c.iter().filter(|c| c.use_ != CircuitUse::Feeder).collect();
        let worst = fin.iter().map(|c| c.drop_total).fold(0.0, f64::max);
        let metres: f64 = c.iter().map(|c| c.length).sum();
        let mut cables: HashMap<String, usize> = HashMap::new();
        for x in c {
            *cables.entry(x.cable()).or_default() += 1;
        }
        let mut cables: Vec<_> = cables.into_iter().collect();
        cables.sort();
        eprintln!("== {t}: boards {} circuits {} (final {}) rcds {} cable {metres:.0} m worst drop {worst:.2} % {cables:?}", p.services.boards.len(), c.len(), fin.len(), p.services.boards.iter().map(|b| b.rcds.len()).sum::<usize>());
        for i in &p.services.issues {
            eprintln!("   {i}");
        }
    }
}

/// Self-check 3: every device in exactly one circuit, every circuit on a board that exists.
#[test]
fn every_device_is_on_exactly_one_circuit() {
    for (t, p) in projects() {
        let sv = &p.services;
        let mut on: HashMap<(usize, usize, &str), usize> = HashMap::new();
        for c in &sv.circuits {
            assert!(!c.devices.is_empty(), "{t}: circuit {} has no device", c.name);
            assert!(sv.boards.iter().any(|b| b.at == c.board), "{t}: circuit {} on no board", c.name);
            for d in &c.devices {
                *on.entry(*d).or_default() += 1;
            }
        }
        let main: Vec<(usize, usize)> = sv.boards.iter().filter(|b| b.parent.is_none()).map(|b| b.at).collect();
        assert_eq!(main.len(), 1, "{t}: one main board");
        for l in sv.links.iter().filter(|l| l.sys == SvcSys::Power) {
            if main.contains(&(l.storey, l.fixture)) {
                continue;
            }
            let n = on.get(&(l.storey, l.fixture, l.connector)).copied().unwrap_or(0);
            let f = &p.storeys[l.storey].layout.fixtures[l.fixture];
            assert_eq!(n, 1, "{t}: st{} {} {} {} on {n} circuits", l.storey, f.family, f.ty, l.connector);
        }
        // and nothing on a circuit that is not linked
        for d in on.keys() {
            assert!(sv.links.iter().any(|l| (l.storey, l.fixture, l.connector) == *d), "{t}: {d:?} unlinked");
        }
        // ways numbered 1..n per board
        for b in &sv.boards {
            let mut w: Vec<u32> = sv.circuits.iter().filter(|c| c.board == b.at).map(|c| c.number).collect();
            w.sort();
            assert_eq!(w, (1..=w.len() as u32).collect::<Vec<_>>(), "{t}: ways of {}", b.label);
        }
    }
}

/// Self-check 5: points per circuit and the diversified load within 0.9 U In; lighting and
/// sockets never share a circuit; dedicated appliances alone.
#[test]
fn circuits_keep_their_points_and_load_limits() {
    for (t, p) in projects() {
        let sock_max = if housing(t) { 8 } else { 6 };
        for c in &p.services.circuits {
            let fams: Vec<&str> = c.devices.iter().map(|d| family(p, d)).collect();
            let lum = fams.iter().filter(|f| **f == "Ceiling Light").count();
            let sock = fams.iter().filter(|f| **f == "Socket Outlet").count();
            match c.use_ {
                CircuitUse::Lighting | CircuitUse::Emergency => {
                    assert!(lum <= 12 && sock == 0, "{t}: {} has {lum} luminaires {sock} sockets", c.name);
                    assert!(c.demand <= 1500.0 + 1e-6, "{t}: {} {} W", c.name, c.demand);
                    assert!(fams.iter().all(|f| *f == "Ceiling Light" || *f == "Light Switch"), "{t}: {} {fams:?}", c.name);
                }
                CircuitUse::Sockets | CircuitUse::Worktop | CircuitUse::Wet | CircuitUse::Essential => {
                    assert!(sock <= sock_max && lum == 0, "{t}: {} has {sock} sockets", c.name);
                    assert_eq!(c.points, sock, "{t}: {}", c.name);
                }
                CircuitUse::Appliance => assert_eq!(c.devices.len(), 1, "{t}: {}", c.name),
                CircuitUse::Feeder => {}
            }
            let v = if c.poles > 1 { 400.0 * 3f64.sqrt() } else { 230.0 };
            assert!(c.demand <= 0.9 * v * c.breaker + 1e-6, "{t}: {} {} W on {}", c.name, c.demand, c.protection());
            assert!(c.demand <= c.load + 1e-6, "{t}: {} demand above load", c.name);
        }
    }
}

/// Self-check 6: voltage drop from the main board to the last device within 3 %, each feeder
/// within 0.5 %; the drop is the one of the length, current and section.
#[test]
fn voltage_drop_stays_within_the_budget() {
    // the ABB length table at 3 %, 230 V, 16 A: 1.5 mm2 17 m, 2.5 mm2 28 m
    assert!((voltage_drop(1, 17.0, 16.0, 1.5) - 3.0).abs() < 0.2, "{}", voltage_drop(1, 17.0, 16.0, 1.5));
    assert!((voltage_drop(1, 28.0, 16.0, 2.5) - 3.0).abs() < 0.2, "{}", voltage_drop(1, 28.0, 16.0, 2.5));
    assert!((voltage_drop(3, 34.0, 16.0, 1.5) - 3.0).abs() < 0.2, "{}", voltage_drop(3, 34.0, 16.0, 1.5));
    for (t, p) in projects() {
        let sv = &p.services;
        let feeder_to = |b: (usize, usize)| sv.circuits.iter().find(|c| c.use_ == CircuitUse::Feeder && (c.devices[0].0, c.devices[0].1) == b);
        for c in &sv.circuits {
            assert!(c.length > 0.0, "{t}: {} has no route", c.name);
            if c.use_ == CircuitUse::Feeder {
                assert!(c.drop <= 0.5 + 1e-9, "{t}: {} drops {:.2} %", c.name, c.drop);
            } else {
                assert!(c.drop_total <= 3.0 + 1e-9, "{t}: {} drops {:.2} % over {:.1} m on {}", c.name, c.drop_total, c.length, c.cable());
                let want = voltage_drop(c.poles, c.length, c.breaker, c.section);
                assert!((c.drop - want).abs() < 1e-9, "{t}: {}", c.name);
            }
            let up = feeder_to(c.board).map_or(0.0, |f| f.drop_total);
            assert!((c.drop_total - up - c.drop).abs() < 1e-9, "{t}: {} total drop does not add up", c.name);
        }
    }
}

/// Self-check 5: 30 mA RCDs on the socket and appliance circuits, every final circuit of a
/// dwelling; bathroom circuits on their own RCD, a refrigerator on its own; no more than 6
/// circuits behind a four-pole RCD and 2 behind a two-pole one; none on the safety supply.
#[test]
fn rcds_guard_sockets_and_wet_rooms() {
    for (t, p) in projects() {
        let sv = &p.services;
        for b in &sv.boards {
            let on = |n: u32| sv.circuits.iter().filter(|c| c.board == b.at && c.rcd == Some(n)).collect::<Vec<_>>();
            for r in &b.rcds {
                let cs = on(r.number);
                assert!(!cs.is_empty(), "{t}: {} RCD{} guards nothing", b.label, r.number);
                assert!(r.ma <= 30.0, "{t}: {} RCD{} {} mA", b.label, r.number, r.ma);
                let cap = if r.poles == 4 { 6 } else { 2 };
                assert!(cs.len() <= cap, "{t}: {} RCD{} {}P guards {}", b.label, r.number, r.poles, cs.len());
                assert!(r.poles == 4 || cs.iter().all(|c| c.poles == 1), "{t}: {} RCD{} two-pole on three phases", b.label, r.number);
                let wet = cs.iter().filter(|c| c.use_ == CircuitUse::Wet).count();
                assert!(wet == 0 || wet == cs.len(), "{t}: {} RCD{} mixes bathroom circuits", b.label, r.number);
                let cold = cs.iter().filter(|c| c.use_ == CircuitUse::Appliance && family(p, &c.devices[0]) == "Refrigerator").count();
                assert!(cold == 0 || cs.len() == 1, "{t}: {} RCD{} shares a refrigerator", b.label, r.number);
            }
        }
        for c in &sv.circuits {
            let must = match c.use_ {
                CircuitUse::Sockets | CircuitUse::Worktop | CircuitUse::Wet | CircuitUse::Appliance => true,
                CircuitUse::Lighting => housing(t),
                _ => false,
            };
            let never = matches!(c.use_, CircuitUse::Feeder | CircuitUse::Emergency);
            assert!(!must || c.rcd.is_some(), "{t}: {} has no RCD", c.name);
            assert!(!never || c.rcd.is_none(), "{t}: {} behind an RCD", c.name);
            if let Some(n) = c.rcd {
                let b = sv.boards.iter().find(|b| b.at == c.board).unwrap();
                assert!(b.rcds.iter().any(|r| r.number == n), "{t}: {} on a missing RCD", c.name);
            }
        }
    }
}

/// The cable holds its breaker (Iz >= In), the breaker the design current; lighting on 1.5 mm2
/// at least, the rest on 2.5; the cooker on three phases, NYM-J 5x2.5 behind 3xB16.
#[test]
fn cables_are_sized_for_their_protection() {
    let mut cookers = 0;
    for (t, p) in projects() {
        for c in &p.services.circuits {
            assert!(iz(c.section, c.poles) >= c.breaker, "{t}: {} {} on {}", c.name, c.protection(), c.cable());
            assert_eq!(c.cores, c.poles + 2, "{t}: {}", c.name);
            let light = matches!(c.use_, CircuitUse::Lighting | CircuitUse::Emergency);
            assert!(c.section >= if light { 1.5 } else { 2.5 }, "{t}: {} {}", c.name, c.cable());
            assert!(c.breaker >= if light { 10.0 } else { 16.0 }, "{t}: {} {}", c.name, c.protection());
            if c.use_ == CircuitUse::Feeder {
                assert!(c.breaker >= 63.0 && c.poles == 3, "{t}: {} {}", c.name, c.protection());
            }
            if c.use_ == CircuitUse::Appliance && family(p, &c.devices[0]) == "Cooker" {
                cookers += 1;
                assert_eq!((c.poles, c.protection().as_str()), (3, "3xB16"), "{t}: {}", c.name);
                assert!(c.cable().starts_with("NYM-J 5x"), "{t}: {}", c.cable());
            }
            if c.use_ == CircuitUse::Lighting && c.section == 1.5 {
                assert_eq!(c.cable(), "NYM-J 3x1.5");
            }
        }
    }
    assert!(cookers > 0, "no cooker circuit");
}

/// Final circuits are named by the rooms they serve.
#[test]
fn circuits_are_named_by_room() {
    for (t, p) in projects() {
        for c in p.services.circuits.iter().filter(|c| matches!(c.use_, CircuitUse::Lighting | CircuitUse::Sockets)) {
            let names: Vec<&str> = c.devices.iter().filter_map(|d| {
                let l = &p.storeys[d.0].layout;
                let f = &l.fixtures[d.1];
                l.rooms.iter().find(|r| {
                    let (xs, ys): (Vec<f64>, Vec<f64>) = r.polygon.iter().copied().unzip();
                    let inside = |v: &[f64], q: f64| q > v.iter().copied().fold(f64::MAX, f64::min) && q < v.iter().copied().fold(f64::MIN, f64::max);
                    inside(&xs, f.at.0) && inside(&ys, f.at.1)
                }).map(|r| r.name.as_str())
            }).collect();
            assert!(names.iter().any(|n| !n.is_empty() && c.name.contains(n)), "{t}: {} names none of {names:?}", c.name);
        }
    }
}

/// The circuit list of every board: a row per circuit with protection, RCD and cable.
#[test]
fn the_schedule_lists_every_circuit() {
    for (t, p) in projects() {
        let csv = schedule_csv(p);
        let rows: Vec<Vec<&str>> = csv.lines().skip(1).map(|l| l.split(';').collect()).collect();
        assert_eq!(rows.len(), p.services.circuits.len(), "{t}");
        assert!(rows.iter().all(|r| r.len() == 13), "{t}: ragged rows");
        for c in &p.services.circuits {
            let b = p.services.boards.iter().find(|b| b.at == c.board).unwrap();
            let r = rows.iter().find(|r| r[0] == b.label && r[1] == c.number.to_string()).unwrap_or_else(|| panic!("{t}: {} not listed", c.name));
            assert_eq!((r[2], r[4], r[6]), (c.name.as_str(), c.protection().as_str(), c.cable().as_str()), "{t}");
            assert_eq!(r[5].is_empty(), c.rcd.is_none(), "{t}: {}", c.name);
        }
    }
}

/// Circuit lines on the plans: one home run per circuit and storey from its first device
/// towards the board, with the board and circuit label and a tick per conductor, and a line
/// to every further device; none with the wires switched off.
#[test]
fn plans_draw_home_runs_and_circuit_lines() {
    for (t, p) in projects() {
        let sv = &p.services;
        for (ci, c) in sv.circuits.iter().enumerate().filter(|c| c.1.use_ != CircuitUse::Feeder) {
            let w: Vec<_> = sv.wires.iter().filter(|w| w.circuit == ci).collect();
            let mut storeys: Vec<usize> = c.devices.iter().map(|d| d.0).collect();
            storeys.sort();
            storeys.dedup();
            let homes: Vec<_> = w.iter().filter(|w| w.home).collect();
            assert_eq!(homes.len(), storeys.len(), "{t}: {} home runs", c.name);
            let b = sv.boards.iter().find(|b| b.at == c.board).unwrap();
            let tag = c.name.split(' ').next().unwrap();
            for h in &homes {
                assert_eq!(h.label, format!("{} / {tag}", b.label), "{t}");
                assert_eq!(h.ticks, c.cores, "{t}: {}", c.name);
                let l = (h.points[1].0 - h.points[0].0).hypot(h.points[1].1 - h.points[0].1);
                assert!(l > 0.0 && l <= 1.0 + 1e-9, "{t}: home run {l} m");
            }
            // a line per further device unless two share a point
            let lines = w.len() - homes.len();
            assert!(lines < c.devices.len() && lines + storeys.len() + 2 >= c.devices.len(), "{t}: {} {lines} lines for {} devices", c.name, c.devices.len());
            for x in &w {
                assert!(x.points.iter().all(|q| q.0.is_finite() && q.1.is_finite()) && x.points.len() >= 2, "{t}");
            }
        }
    }
    let off = build_project(&parse_spec(&json!({"typology": "house", "floors": 2, "services": {"wires": false}})).unwrap()).unwrap();
    assert!(off.services.wires.is_empty() && !off.services.circuits.is_empty());
}

/// Cable traces in 3D are off unless asked; asked, each circuit gets one from its board's
/// connector along the carriers to a device connector, every vertex on a power run.
#[test]
fn traces_follow_the_carriers_from_the_board() {
    let (_, house) = &projects()[0];
    assert!(house.services.traces.is_empty(), "traces on by default");
    let p = build_project(&parse_spec(&json!({"typology": "house", "floors": 2, "services": {"traces": true}})).unwrap()).unwrap();
    let sv = &p.services;
    assert_eq!(sv.circuits, house.services.circuits, "traces change the circuits");
    assert_eq!(sv.traces.len(), sv.circuits.len());
    let on_run = |q: (f64, f64, f64)| {
        sv.elec.iter().any(|r| r.points.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
            let l2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
            let s = if l2 > 0.0 { (((q.0 - a.0) * d.0 + (q.1 - a.1) * d.1 + (q.2 - a.2) * d.2) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let e = (a.0 + d.0 * s - q.0, a.1 + d.1 * s - q.1, a.2 + d.2 * s - q.2);
            (e.0 * e.0 + e.1 * e.1 + e.2 * e.2).sqrt() < 5e-3
        }))
    };
    let port = |k: usize, i: usize, n: &str| {
        let st = &p.storeys[k];
        svc_ports(&st.layout.fixtures[i]).into_iter().find(|c| c.name == n).map(|c| (c.at.0, c.at.1, st.elevation + c.at.2)).unwrap()
    };
    let near = |a: (f64, f64, f64), b: (f64, f64, f64)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt() < 5e-3;
    for tr in &sv.traces {
        let c = &sv.circuits[tr.circuit];
        assert!(near(tr.points[0], port(c.board.0, c.board.1, "Main")), "{} trace starts off the board", c.name);
        let end = *tr.points.last().unwrap();
        assert!(c.devices.iter().any(|d| near(end, port(d.0, d.1, d.2))), "{} trace ends off its devices", c.name);
        for q in &tr.points {
            assert!(on_run(*q), "{} trace leaves the carriers at {q:?}", c.name);
        }
        let len: f64 = tr.points.windows(2).map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2) + (w[1].2 - w[0].2).powi(2)).sqrt()).sum();
        assert!(len <= c.length + 1e-6, "{} trace {len} m longer than the route {} m", c.name, c.length);
    }
}
