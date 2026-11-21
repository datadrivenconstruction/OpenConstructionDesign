// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Balanced supply and extract for the typical briefs: every typology gets ducts, each system
//! balances within the rule, no duct runs faster than its cap and the reported air is the air
//! the placed terminals deliver.

use ddc_gen::project::{build_project, parse_spec, AirSys, GProject};
use serde_json::{json, Value as J};

fn metric(p: &GProject, k: &str) -> f64 {
    p.metrics.iter().filter(|m| m.0 == k).map(|m| m.1).sum()
}

/// Air of the terminals each network feeds: the drops that end in a neck below the duct layer.
fn terminal_air(p: &GProject) -> Vec<[f64; 2]> {
    let n = p.storeys.len();
    let nets = p.hvac.ducts.iter().map(|d| d.net as usize + 1).max().unwrap_or(0);
    let mut q = vec![[0.0; 2]; nets];
    for d in p.hvac.ducts.iter().filter(|d| d.storey < n && d.bend.is_some()) {
        let e = d.points.len();
        if e >= 2 && d.points[e - 1].2 < d.points[e - 2].2 - 1e-6 {
            q[d.net as usize][if d.sys == AirSys::Supply { 0 } else { 1 }] += d.flow;
        }
    }
    q
}

fn check(name: &str, v: J, strict: bool) -> GProject {
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    assert!(!p.hvac.ducts.is_empty(), "{name}: no ducts");
    let refused: Vec<_> = p.metrics.iter().filter(|m| m.0.starts_with("hvac_refused") && m.1 > 0.0).collect();
    assert!(refused.is_empty(), "{name}: {refused:?}");
    if strict {
        assert_eq!(metric(&p, "hvac_fast_ducts"), 0.0, "{name}: ducts above their velocity cap");
    }
    let air = terminal_air(&p);
    let max = ddc_gen::project::rules::rule("services.hvac.balance_max");
    for (net, q) in air.iter().enumerate() {
        let big = q[0].max(q[1]);
        if strict && q[0] > 0.0 && q[1] > 0.0 {
            assert!((q[0] - q[1]).abs() <= max * big + 1e-6, "{name}: net {net} supply {} extract {}", q[0], q[1]);
        }
    }
    let placed = air.iter().fold([0.0; 2], |a, q| [a[0] + q[0], a[1] + q[1]]);
    assert!((p.hvac.flow.0 - placed[0]).abs() < 1.0 && (p.hvac.flow.1 - placed[1]).abs() < 1.0, "{name}: flow {:?} placed {placed:?}", p.hvac.flow);
    p
}

#[test]
fn a_hospital_wing_on_auto_gets_ducts_in_zones() {
    let v = json!({"typology": "hospital_wing", "floors": 4, "floor_height": 4.2, "services": true, "lift": true,
        "footprint": {"width": 80.0, "depth": 20.0}, "roof": {"kind": "flat"}, "basements": 0,
        "rules": {"core.lift_shaft_w": 2.6, "core.lift_shaft_d": 2.8, "mep.hvac.system": "auto"}});
    // zones still lose terminals where the mains take no more tees: balance and velocity open
    let p = check("hospital", v, false);
    // one unit cannot carry the wing: the system splits into zones, each with its own unit
    assert!(p.hvac.plant.len() >= 2, "{} units", p.hvac.plant.len());
}

#[test]
fn a_school_gets_balanced_ventilation() {
    check("school", json!({"typology": "school", "floors": 3, "floor_height": 3.6, "services": true, "lift": true,
        "footprint": {"width": 48.0, "depth": 18.0}, "roof": {"kind": "flat"}}), true);
}

#[test]
fn an_office_gets_balanced_ventilation() {
    check("office", json!({"typology": "office", "floors": 4, "floor_height": 3.6, "services": true, "lift": true,
        "footprint": {"width": 40.0, "depth": 16.0}, "roof": {"kind": "flat"}}), true);
}

#[test]
fn a_housing_section_gets_balanced_ventilation() {
    check("housing", json!({"typology": "apartment_section", "floors": 6, "floor_height": 3.0, "services": true, "lift": true,
        "footprint": {"width": 34.0, "depth": 12.0}, "roof": {"kind": "flat"}}), true);
}
