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

/// Air of the terminals each network feeds: the drops that end in a neck below the duct layer,
/// and the cooker hood ducts that rise from their outlet into a branch.
fn terminal_air(p: &GProject) -> Vec<[f64; 2]> {
    let n = p.storeys.len();
    let nets = p.hvac.ducts.iter().map(|d| d.net as usize + 1).max().unwrap_or(0);
    let mut q = vec![[0.0; 2]; nets];
    for d in p.hvac.ducts.iter().filter(|d| d.storey < n && d.points.len() >= 2) {
        let e = d.points.len();
        let drop = d.bend.is_some() && d.points[e - 1].2 < d.points[e - 2].2 - 1e-6;
        let vertical = d.points.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 1e-6 && (w[0].1 - w[1].1).abs() < 1e-6);
        let hood = d.bend.is_none() && d.sys == AirSys::Extract && !vertical && d.points[1].2 > d.points[0].2 + 1e-6;
        if drop || hood {
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
    let reported = [metric(&p, "hvac_supply_m3h"), metric(&p, "hvac_extract_m3h")];
    assert!((reported[0] - placed[0]).abs() < 1.0 && (reported[1] - placed[1]).abs() < 1.0, "{name}: metrics {reported:?} placed {placed:?}");
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

/// The ventilated hospital still drains: every stack and branch of the sanitary system reaches
/// a building drain that leaves the ground slab, though the ducts move the stacks about.
#[test]
fn a_ventilated_hospital_drains_every_stack_to_the_building_drain() {
    use ddc_gen::project::SvcSys;
    let v = json!({"typology": "hospital_wing", "floors": 4, "floor_height": 4.2, "services": true, "lift": true,
        "footprint": {"width": 80.0, "depth": 20.0}, "roof": {"kind": "flat"}, "basements": 0,
        "rules": {"core.lift_shaft_w": 2.6, "core.lift_shaft_d": 2.8}});
    let p = build_project(&parse_spec(&v).unwrap()).unwrap();
    assert!(!p.hvac.ducts.is_empty(), "no ducts");
    let runs: Vec<&Vec<(f64, f64, f64)>> = p.services.water.iter().filter(|r| r.sys == SvcSys::Sanitary).map(|r| &r.points).collect();
    assert!(!runs.is_empty(), "no drains");
    let on = |q: (f64, f64, f64), pts: &[(f64, f64, f64)]| {
        pts.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let d = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
            let l2 = d.0 * d.0 + d.1 * d.1 + d.2 * d.2;
            let t = if l2 < 1e-12 { 0.0 } else { (((q.0 - a.0) * d.0 + (q.1 - a.1) * d.1 + (q.2 - a.2) * d.2) / l2).clamp(0.0, 1.0) };
            ((q.0 - a.0 - d.0 * t).powi(2) + (q.1 - a.1 - d.1 * t).powi(2) + (q.2 - a.2 - d.2 * t).powi(2)).sqrt() < 1e-4
        })
    };
    let mut root: Vec<usize> = (0..runs.len()).collect();
    fn find(r: &mut [usize], i: usize) -> usize {
        if r[i] != i {
            let top = find(r, r[i]);
            r[i] = top;
        }
        r[i]
    }
    for i in 0..runs.len() {
        for j in i + 1..runs.len() {
            let ends = |a: &[(f64, f64, f64)], b: &[(f64, f64, f64)]| [a[0], a[a.len() - 1]].iter().any(|&q| on(q, b));
            if ends(runs[i], runs[j]) || ends(runs[j], runs[i]) {
                let (a, b) = (find(&mut root, i), find(&mut root, j));
                root[a] = b;
            }
        }
    }
    let slab = &p.storeys[0].layout.slab;
    let (x0, x1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
    let (y0, y1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
    let outside = |q: &(f64, f64, f64)| q.2 < 0.0 && (q.0 < x0 - 1e-6 || q.0 > x1 + 1e-6 || q.1 < y0 - 1e-6 || q.1 > y1 + 1e-6);
    let drained: std::collections::BTreeSet<usize> = (0..runs.len()).filter(|&i| runs[i].iter().any(outside)).map(|i| find(&mut root, i)).collect();
    let cut: Vec<_> = (0..runs.len()).filter(|&i| !drained.contains(&find(&mut root, i))).map(|i| runs[i]).collect();
    assert!(cut.is_empty(), "{} of {} drains reach no building drain, first {:?}", cut.len(), runs.len(), cut.first());
}
