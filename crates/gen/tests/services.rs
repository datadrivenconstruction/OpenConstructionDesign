// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Power, lighting, water and drainage designed into every typology: a connected network per
//! system, nothing clashing, every wet fixture and every lit room served.

use ddc_gen::project::{build_project, is_info, parse_spec, services_check, services_errors, svc_ports, GProject, SvcSys, Typology};
use serde_json::{json, Value};

fn briefs() -> Vec<Value> {
    vec![
        json!({"typology": "house", "floors": 2, "services": true}),
        json!({"typology": "apartment_section", "floors": 5, "services": true}),
        json!({"typology": "office", "floors": 4, "services": true}),
        json!({"typology": "school", "floors": 3, "services": true}),
    ]
}

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().find(|m| m.0 == k).map_or(-1.0, |m| m.1)
}

#[test]
fn services_every_typology() {
    let mut bad = vec![];
    for b in briefs() {
        let s = parse_spec(&b).unwrap();
        let p = build_project(&s).unwrap();
        let sv = &p.services;
        // errors only: a device left out is told as info and caught below as unlinked
        let faults = services_errors(&p);
        let linked = |k: usize, i: usize, c: &str| sv.links.iter().any(|l| l.storey == k && l.fixture == i && l.connector == c);
        let mut unlinked = vec![];
        for (k, st) in p.storeys.iter().enumerate() {
            for (i, f) in st.layout.fixtures.iter().enumerate() {
                for c in svc_ports(f) {
                    if !linked(k, i, c.name) {
                        unlinked.push(format!("st{k} {} {} {}", f.family, f.ty, c.name));
                    }
                }
            }
        }
        let (pu, wu, lux) = (metric(&p, "power_unserved_rooms"), metric(&p, "water_unserved"), metric(&p, "lux_short_rooms"));
        eprintln!(
            "== {} water {} elec {} links {} circuits {} power_unserved {pu} water_unserved {wu} lux_short {lux} appliances {} faults {}",
            b["typology"],
            sv.water.len(),
            sv.elec.len(),
            sv.links.len(),
            sv.circuits.len(),
            metric(&p, "appliances"),
            faults.len()
        );
        for f in faults.iter().take(25) {
            eprintln!("   {f}");
        }
        for u in unlinked.iter().take(25) {
            eprintln!("   unlinked {u}");
        }
        let ok = faults.is_empty() && unlinked.is_empty() && pu == 0.0 && wu == 0.0 && sv.names.len() == 4 && !sv.circuits.is_empty() && !sv.water.is_empty();
        if !ok {
            bad.push(b["typology"].to_string());
        }
    }
    assert!(bad.is_empty(), "services incomplete: {bad:?}");
}

#[test]
fn appliances_come_once_per_dwelling_from_the_fit_out_and_are_all_connected() {
    const ONCE: [&str; 4] = ["Cooker", "Dishwasher", "Washing Machine", "Refrigerator"];
    for b in [briefs()[0].clone(), briefs()[1].clone()] {
        let s = parse_spec(&b).unwrap();
        let p = build_project(&s).unwrap();
        let sv = &p.services;
        // appliances of each dwelling, by family
        let mut per: std::collections::BTreeMap<(String, &str), usize> = Default::default();
        let mut kitchens = 0;
        for (k, st) in p.storeys.iter().enumerate() {
            let l = &st.layout;
            for (i, f) in l.fixtures.iter().enumerate() {
                let room = l.rooms.iter().position(|r| {
                    let xs = r.polygon.iter().map(|q| q.0);
                    let ys = r.polygon.iter().map(|q| q.1);
                    let (x0, x1) = (xs.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max));
                    let (y0, y1) = (ys.clone().fold(f64::MAX, f64::min), ys.fold(f64::MIN, f64::max));
                    f.at.0 > x0 && f.at.0 < x1 && f.at.1 > y0 && f.at.1 < y1
                });
                if ONCE.contains(&f.family) {
                    let owner = room.map_or("none".to_string(), |r| format!("{:?}", st.rooms[r].owner));
                    *per.entry((owner, f.family)).or_default() += 1;
                    let ports = svc_ports(f);
                    assert!(!ports.is_empty(), "{b}: {} {} has no connector", f.family, f.ty);
                    for c in ports {
                        assert!(
                            sv.links.iter().any(|x| x.storey == k && x.fixture == i && x.connector == c.name),
                            "{b}: st{k} {} {} not connected",
                            f.family,
                            c.name
                        );
                    }
                }
            }
            kitchens += st.rooms.iter().filter(|r| r.kind == ddc_gen::project::RoomKind::Kitchen).count();
        }
        for (k, n) in &per {
            assert_eq!(*n, 1, "{b}: {k:?} placed {n} times");
        }
        // every kitchen has its cooker and fridge from the kitchen run
        for fam in ["Cooker", "Refrigerator"] {
            assert_eq!(per.keys().filter(|k| k.1 == fam).count(), kitchens, "{b}: {fam}");
        }
    }
}

#[test]
fn services_stable_and_optional() {
    let b = json!({"typology": "apartment_section", "floors": 3, "services": true});
    let s = parse_spec(&b).unwrap();
    assert_eq!(build_project(&s).unwrap(), build_project(&s).unwrap());
    let off = parse_spec(&json!({"typology": "apartment_section", "floors": 3})).unwrap();
    let p = build_project(&off).unwrap();
    assert!(p.services.water.is_empty() && p.services.elec.is_empty() && p.services.circuits.is_empty());
    // the rest of the building does not move when services are on
    let q = build_project(&s).unwrap();
    for (a, b) in p.storeys.iter().zip(&q.storeys) {
        assert_eq!(a.layout.walls, b.layout.walls);
        assert_eq!(a.layout.openings, b.layout.openings);
    }
    assert_eq!(p.hvac.ducts, q.hvac.ducts);
}

#[test]
fn every_referenced_system_has_a_name_in_every_typology() {
    for t in Typology::NAMES {
        for sv in [Value::Null, json!(false), json!(true)] {
            let s = parse_spec(&json!({"typology": t, "floors": 2, "lang": "en", "services": sv})).unwrap();
            let p = build_project(&s).unwrap();
            let names = &p.services.names;
            assert_eq!(names.len(), SvcSys::ALL.len(), "{t} services {sv}: {names:?}");
            assert!(names.iter().all(|n| !n.trim().is_empty()), "{t} services {sv}: {names:?}");
            let has = |x: SvcSys| SvcSys::ALL.iter().position(|a| *a == x).is_some_and(|i| i < names.len());
            let sv_ = &p.services;
            let runs = sv_.water.iter().chain(&sv_.elec).map(|r| r.sys);
            let links = sv_.links.iter().map(|l| l.sys);
            for x in runs.chain(links) {
                assert!(has(x), "{t} services {sv}: {x:?} has no name in {names:?}");
            }
            // circuits hang on the power system
            assert!(sv_.circuits.is_empty() || has(SvcSys::Power), "{t} services {sv}");
        }
    }
}

/// Every service run of every building type routes in the model: each segment is at least as
/// long as the elbows, tees and reducers at its two ends take from it, whatever their size.
#[test]
fn every_service_segment_holds_the_fittings_at_its_ends() {
    let mut briefs: Vec<Value> = Typology::NAMES
        .iter()
        .map(|t| json!({"typology": t, "floors": 3, "services": true}))
        .collect();
    briefs.extend(self::briefs());
    let mut bad = vec![];
    for b in briefs {
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        let fittings = |f: &String| f.contains("too short") || f.contains("reversal") || f.contains("branches");
        for f in services_check(&p).into_iter().filter(fittings).take(12) {
            bad.push(format!("{b}: {f}"));
        }
    }
    assert!(bad.is_empty(), "{} faults:\n{}", bad.len(), bad.join("\n"));
}

/// No water connector goes unserved silently: the `water_unserved` metric is exactly the
/// connectors without a link, whether their room, wall or run failed.
#[test]
fn every_unconnected_water_connector_is_counted() {
    for b in briefs() {
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        let sv = &p.services;
        let mut open = 0usize;
        for (k, st) in p.storeys.iter().enumerate() {
            for (i, f) in st.layout.fixtures.iter().enumerate() {
                for c in svc_ports(f).into_iter().filter(|c| c.sys != SvcSys::Power) {
                    if !sv.links.iter().any(|l| l.storey == k && l.fixture == i && l.connector == c.name) {
                        open += 1;
                    }
                }
            }
        }
        assert_eq!(metric(&p, "water_unserved"), open as f64, "{b}: unconnected water connectors not all counted");
        assert!(metric(&p, "water_ports_unplaced") <= open as f64, "{b}");
    }
}

/// A power run that turns down into a connector holds the elbow at the top of the drop: the
/// drop to a ceiling luminaire is never shorter than the bend radius of the row's section.
#[test]
fn every_power_drop_holds_its_elbow() {
    let mut bad = vec![];
    for b in briefs() {
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        for r in &p.services.elec {
            let n = r.points.len();
            if n < 3 {
                continue;
            }
            let (a, z) = (r.points[n - 2], r.points[n - 1]);
            let vertical = (a.0 - z.0).abs() < 1e-9 && (a.1 - z.1).abs() < 1e-9;
            let radius = ddc_geom::mep::default_bend_radius(&r.section, false).max(ddc_geom::mep::default_bend_radius(&r.section, true));
            if vertical && (a.2 - z.2).abs() + 1e-9 < radius {
                bad.push(format!("{b}: drop {:.3} m at {z:?}, elbow radius {radius:.3} m", (a.2 - z.2).abs()));
            }
        }
    }
    assert!(bad.is_empty(), "{} drops too short:\n{}", bad.len(), bad.join("\n"));
}

/// Every typology finds a board stack its riser fits beside, and an appliance with a circuit
/// of its own by the norms (water heater, refrigerator, cooker...) shares it with nothing.
#[test]
fn boards_placed_and_dedicated_appliances_alone() {
    let own = ["Cooker", "Dishwasher", "Washing Machine", "Water Heater", "Refrigerator"];
    let mut bad = vec![];
    for t in Typology::NAMES {
        let b = json!({"typology": t, "floors": 3, "services": true});
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        if p.services.circuits.is_empty() {
            bad.push(format!("{b}: no board placed"));
        }
        for c in &p.services.circuits {
            let fams: Vec<&str> = c.devices.iter().map(|d| p.storeys[d.0].layout.fixtures[d.1].family).collect();
            if fams.len() > 1 && fams.iter().any(|f| own.contains(f)) {
                bad.push(format!("{b}: circuit {} shares a dedicated appliance: {fams:?}", c.name));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Every conduit and cable tray ends at a board, a box or a device: no end capped or open in
/// the air, in every typology and in a tall office tower.
#[test]
fn every_power_run_ends_at_a_board_box_or_device() {
    let mut briefs: Vec<Value> = Typology::NAMES
        .iter()
        .map(|t| json!({"typology": t, "floors": 3, "services": true}))
        .collect();
    briefs.push(json!({"typology": "office", "floors": 12, "services": true}));
    let mut bad = vec![];
    for b in briefs {
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        let loose = |f: &String| f.starts_with("Power") && (f.contains("dangling end") || f.contains("open end"));
        for f in services_check(&p).into_iter().filter(loose).take(6) {
            bad.push(format!("{b}: {f}"));
        }
    }
    assert!(bad.is_empty(), "{} loose ends:\n{}", bad.len(), bad.join("\n"));
}

/// The arm of a ring ends in the drop to its last device: the drop's horizontal holds the
/// elbow off the arm and the elbow down, for the thickest conduit too; the model routes it.
#[test]
fn every_device_drop_horizontal_holds_its_elbows() {
    let mut bad = vec![];
    for t in Typology::NAMES {
        let b = json!({"typology": t, "floors": 3, "services": true});
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        for r in &p.services.elec {
            if r.points.len() != 3 {
                continue;
            }
            let (a, w, z) = (r.points[0], r.points[1], r.points[2]);
            let down = (w.0 - z.0).abs() < 1e-9 && (w.1 - z.1).abs() < 1e-9 && z.2 < w.2;
            let flat = (a.2 - w.2).abs() < 1e-9;
            let radius = ddc_geom::mep::default_bend_radius(&r.section, false).max(ddc_geom::mep::default_bend_radius(&r.section, true));
            let run = (w.0 - a.0).hypot(w.1 - a.1);
            if down && flat && run + 1e-9 < radius {
                bad.push(format!("{b}: horizontal {run:.3} m under elbow radius {radius:.3} m at {a:?}"));
            }
        }
        let short = |f: &String| f.starts_with("Power") && f.contains("too short");
        bad.extend(services_check(&p).into_iter().filter(short).take(6).map(|f| format!("{b}: {f}")));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

fn power_too_short(p: &GProject) -> Vec<String> {
    services_check(p).into_iter().filter(|f| f.starts_with("Power") && f.contains("too short")).take(6).collect()
}

/// A luminaire row leaves the ring along a horizontal that holds its fittings: the first stretch
/// to the first luminaire is never shorter than the tee there, and no power run is too short.
#[test]
fn luminaire_rows_hold_their_elbows() {
    let mut bad = vec![];
    for t in Typology::NAMES {
        let b = json!({"typology": t, "floors": 3, "services": true});
        let p = build_project(&parse_spec(&b).unwrap()).unwrap();
        let lights: Vec<(f64, f64, f64)> = p
            .services
            .links
            .iter()
            .filter(|l| p.storeys[l.storey].layout.fixtures[l.fixture].family == "Ceiling Light")
            .filter_map(|l| svc_ports(&p.storeys[l.storey].layout.fixtures[l.fixture]).into_iter().find(|c| c.name == l.connector))
            .map(|c| c.at)
            .collect();
        for r in p.services.elec.iter().filter(|r| r.points.len() >= 3) {
            let (a, q) = (r.points[0], r.points[1]);
            let row = (a.2 - q.2).abs() < 1e-9 && lights.iter().any(|l| (l.0 - q.0).hypot(l.1 - q.1) < 1e-6);
            let leg = ddc_geom::mep::junction_leg(&r.section);
            if row && (q.0 - a.0).hypot(q.1 - a.1) + 1e-9 < leg {
                bad.push(format!("{b}: row start {a:?} to {q:?} under the tee leg {leg:.3}"));
            }
        }
        bad.extend(power_too_short(&p).into_iter().map(|f| format!("{b}: {f}")));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The tray along a corridor ends at its last tee in the drop or row there, the reducer to
/// conduit and the elbow both held, storey after storey of a tall office.
#[test]
fn corridor_tray_end_holds_reducer_and_elbow() {
    let b = json!({"typology": "office", "floors": 8, "services": true});
    let p = build_project(&parse_spec(&b).unwrap()).unwrap();
    assert!(p.services.elec.iter().any(|r| r.section.domain == ddc_geom::mep::Domain::CableTray), "no corridor tray");
    let bad = power_too_short(&p);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A device the design must leave out is never dropped silently: with the ring squeezed to the
/// wall and stiff conduit, what does not fit shows in the services check as unpowered or unlit,
/// and no segment is left too short for its bends.
#[test]
fn devices_left_out_are_told_in_the_check() {
    let b = json!({"typology": "house", "floors": 2, "services": true,
        "rules": {"services.elec.d_ring": 0.1, "services.elec.conduit_s": 0.05}});
    let p = build_project(&parse_spec(&b).unwrap()).unwrap();
    let faults = services_check(&p);
    let told: Vec<&String> = faults.iter().filter(|f| f.contains("unpowered") || f.contains("unlit")).collect();
    assert!(!told.is_empty(), "the squeeze left nothing out, or left it out silently: {faults:?}");
    assert!(told.iter().all(|f| f.starts_with("Power: info: storey ") && is_info(f)), "{told:?}");
    assert!(services_errors(&p).iter().all(|f| !f.contains("unpowered")));
    assert!(power_too_short(&p).is_empty(), "{:?}", power_too_short(&p));
    assert_eq!(p.services.issues.len(), told.len());
}

#[test]
fn services_follow_origin() {
    let a = parse_spec(&json!({"typology": "house", "floors": 2, "services": true})).unwrap();
    let b = parse_spec(&json!({"typology": "house", "floors": 2, "services": true, "origin": [100.0, -50.0]})).unwrap();
    let (p, q) = (build_project(&a).unwrap(), build_project(&b).unwrap());
    assert_eq!(p.services.elec.len(), q.services.elec.len());
    assert_eq!(p.services.water.len(), q.services.water.len());
    for (r, s) in p.services.water.iter().zip(&q.services.water) {
        for (u, v) in r.points.iter().zip(&s.points) {
            assert!((u.0 + 100.0 - v.0).abs() < 1e-6 && (u.1 - 50.0 - v.1).abs() < 1e-6 && (u.2 - v.2).abs() < 1e-9);
        }
    }
    for (d, e) in p.hvac.ducts.iter().zip(&q.hvac.ducts) {
        for (u, v) in d.points.iter().zip(&e.points) {
            assert!((u.0 + 100.0 - v.0).abs() < 1e-6 && (u.1 - 50.0 - v.1).abs() < 1e-6);
        }
    }
    // errors only: what the design leaves out is info, told the same at either origin
    assert!(services_errors(&q).is_empty(), "{:?}", services_errors(&q));
}

#[test]
fn the_board_room_of_a_house_is_powered_with_its_child_rooms() {
    let p = build_project(&parse_spec(&json!({"typology": "house", "floors": 2, "services": true})).unwrap()).unwrap();
    assert!(!p.services.circuits.is_empty(), "no circuit: {:?}", p.services.issues);
    let board_rooms: Vec<&String> = p.services.issues.iter().filter(|i| i.contains("room 0 Corridor unpowered")).collect();
    assert!(board_rooms.is_empty(), "{board_rooms:?}");
}

#[test]
fn every_power_link_ends_a_run_at_its_connector() {
    let p = build_project(&parse_spec(&json!({"typology": "house", "floors": 2, "services": true})).unwrap()).unwrap();
    let ends: Vec<(f64, f64, f64)> = p.services.elec.iter().flat_map(|r| [r.points[0], *r.points.last().unwrap()]).collect();
    let mut n = 0;
    let mut bad = vec![];
    for l in p.services.links.iter().filter(|l| l.sys == SvcSys::Power) {
        let st = &p.storeys[l.storey];
        let f = &st.layout.fixtures[l.fixture];
        let Some(c) = svc_ports(f).into_iter().find(|c| c.name == l.connector) else { continue };
        let q = (c.at.0, c.at.1, st.elevation + c.at.2);
        n += 1;
        if !ends.iter().any(|e| (e.0 - q.0).hypot(e.1 - q.1).hypot(e.2 - q.2) < 1e-6) {
            bad.push(format!("st{} {} {} at {q:?}", l.storey, f.family, f.ty));
        }
    }
    assert!(n > 0, "no power link");
    assert!(bad.is_empty(), "{} connectors without a run end: {bad:?}", bad.len());
}

#[test]
fn every_room_of_a_house_and_an_office_is_powered() {
    for (t, f) in [("house", 2), ("office", 4)] {
        let p = build_project(&parse_spec(&json!({"typology": t, "floors": f, "services": true})).unwrap()).unwrap();
        let rooms: Vec<&String> = p.services.issues.iter().filter(|i| i.contains("unpowered:") && !i.contains("device")).collect();
        assert_eq!(metric(&p, "power_unserved_rooms"), 0.0, "{t}: {rooms:?}");
    }
}

#[test]
fn the_corridor_of_an_apartment_section_powers_every_floor() {
    for f in [3, 5] {
        let p = build_project(&parse_spec(&json!({"typology": "apartment_section", "floors": f, "services": true})).unwrap()).unwrap();
        let boards: Vec<&String> = p.services.issues.iter().filter(|i| i.contains("Corridor unpowered")).collect();
        assert!(boards.is_empty(), "{f} floors: {boards:?}");
        let errs: Vec<String> = services_errors(&p).into_iter().filter(|e| e.starts_with("Power")).collect();
        assert!(errs.is_empty(), "{f} floors: {errs:?}");
        for k in 0..p.storeys.len() {
            assert!(p.services.circuits.iter().any(|c| c.board.0 == k), "{f} floors: storey {k} has no circuit");
        }
    }
}
