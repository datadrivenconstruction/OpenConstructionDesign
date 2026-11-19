// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Fire protection of whole buildings: every occupied room of a tower covered by heads of its
//! hazard class, standpipes with a landing valve on every storey, and each fire rule changing
//! the design it governs.

use ddc_gen::project::why::explain_project;
use ddc_gen::project::{build_project, fire_check, hazard_of, parse_spec, FireRun, GProject, ProjectSpec, RoomKind};
use serde_json::{json, Value as J};

fn built(v: J) -> (ProjectSpec, GProject) {
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    (s, p)
}

fn office(floors: u32, extra: J) -> J {
    let mut v = json!({"typology": "office", "floors": floors});
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    v
}

fn inside(poly: &[(f64, f64)], q: (f64, f64)) -> bool {
    let mut c = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.1 > q.1) != (b.1 > q.1) && q.0 < (b.0 - a.0) * (q.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn heads(p: &GProject) -> usize {
    p.storeys.iter().flat_map(|s| &s.layout.fixtures).filter(|f| f.family == "Sprinkler Head").count()
}

fn first(issues: &[String]) -> &[String] {
    &issues[..issues.len().min(8)]
}

/// The tower of the live scenario: 40 office storeys. Every occupied room holds a head and
/// the whole design passes the fire check: coverage by class, spacing, area per head, heads
/// under the ceiling and off the columns, pipes off the columns, a landing valve on every
/// storey of every standpipe, one connected network each, and hose reach.
#[test]
fn every_occupied_room_of_a_forty_storey_tower_is_covered() {
    let (_, p) = built(office(40, json!({})));
    assert_eq!(p.storeys.len(), 40);
    let issues = fire_check(&p);
    assert!(issues.is_empty(), "{} faults: {:?}", issues.len(), first(&issues));
    for (k, st) in p.storeys.iter().enumerate() {
        let hs: Vec<(f64, f64)> =
            st.layout.fixtures.iter().filter(|f| f.family == "Sprinkler Head").map(|f| f.at).collect();
        for (i, (r, g)) in st.rooms.iter().zip(&st.layout.rooms).enumerate() {
            if matches!(r.kind, RoomKind::Stair | RoomKind::Lift) {
                continue;
            }
            assert!(hs.iter().any(|h| inside(&g.polygon, *h)), "storey {k} room {i} {:?} has no head", r.kind);
        }
    }
    assert!(heads(&p) >= 400, "{} heads", heads(&p));
    let f = &p.fire;
    assert!(f.covered >= f.area * (1.0 - 1e-9), "{} of {}", f.covered, f.area);
    assert!(!f.standpipes.is_empty(), "no standpipe");
    for sp in &f.standpipes {
        assert_eq!(sp.valves.len(), 40, "{}", sp.name);
        let risers = sp.runs.iter().filter(|r| r.kind == FireRun::Riser).count();
        assert_eq!(risers, 40, "{}", sp.name);
        assert!(sp.runs.iter().all(|r| r.run.pipe.is_some() && r.flow > 0.0));
    }
    assert!(f.reach_gaps.is_empty(), "{:?}", &f.reach_gaps[..f.reach_gaps.len().min(3)]);
}

/// A heavier hazard class for a room kind puts in more heads and sizes the pipes for more
/// water; a lighter one fewer.
#[test]
fn the_hazard_rule_of_a_room_kind_sets_its_heads() {
    let base = office(3, json!({"sprinklers": true}));
    let mut high = base.clone();
    high["rules"] = json!({"fire.hazard.office": "high"});
    let (_, a) = built(base);
    let (_, b) = built(high);
    assert!(heads(&b) > heads(&a), "{} vs {}", heads(&b), heads(&a));
    let flow = |p: &GProject| p.fire.runs.iter().filter(|r| r.kind == FireRun::Drop).map(|r| r.flow).fold(0.0, f64::max);
    assert!(flow(&b) > flow(&a), "{} vs {}", flow(&b), flow(&a));
    assert!(fire_check(&a).is_empty(), "{:?}", first(&fire_check(&a)));
    assert!(fire_check(&b).is_empty(), "{:?}", first(&fire_check(&b)));
}

/// The class's spacing and area rules move the grid: a wider light class spacing and area
/// leave fewer heads.
#[test]
fn the_class_spacing_and_area_rules_set_the_grid() {
    let base = office(2, json!({"sprinklers": true}));
    let mut tight = base.clone();
    tight["rules"] = json!({"fire.light.spacing": 2.5, "fire.light.head_area": 9});
    let (_, a) = built(base);
    let (_, b) = built(tight);
    assert!(heads(&b) > heads(&a), "{} vs {}", heads(&b), heads(&a));
    assert!(fire_check(&b).is_empty(), "{:?}", first(&fire_check(&b)));
    assert_eq!(hazard_of(RoomKind::Office).name(), "light");
}

/// Heads hang under the ceiling: below the head drop under the slab, the deflector rule sets
/// how far under the ceiling face.
#[test]
fn heads_hang_under_the_ceiling() {
    let (_, p) = built(office(2, json!({"sprinklers": true, "rules": {"fire.deflector_below": 0.2}})));
    let (_, q) = built(office(2, json!({"sprinklers": true, "rules": {"fire.deflector_below": 0.3}})));
    let low = |p: &GProject| p.storeys[0].layout.fixtures.iter().filter(|f| f.family == "Sprinkler Head").map(|f| f.z).fold(f64::MAX, f64::min);
    assert!((low(&p) - low(&q) - 0.1).abs() < 1e-6, "{} vs {}", low(&p), low(&q));
    assert!(fire_check(&p).is_empty(), "{:?}", first(&fire_check(&p)));
    assert!(fire_check(&q).is_empty(), "{:?}", first(&fire_check(&q)));
}

/// Standpipes follow the height rule; their valves stand at the valve height rule, one per
/// storey, and the rules explain them.
#[test]
fn standpipes_follow_the_height_rule() {
    let (_, low) = built(office(3, json!({})));
    assert!(low.fire.standpipes.is_empty());
    let (s, p) = built(office(3, json!({"rules": {"fire.standpipe_height": 0, "fire.valve_height": 1.4}})));
    assert!(!p.fire.standpipes.is_empty());
    for sp in &p.fire.standpipes {
        assert_eq!(sp.valves.len(), 3);
        for (k, fx) in &sp.valves {
            let st = &p.storeys[*k];
            let port = ddc_gen::project::svc_ports(&st.layout.fixtures[*fx]).into_iter().find(|c| c.name == "Inlet").unwrap();
            assert!((port.at.2 - 1.4).abs() < 1e-6, "valve port at {}", port.at.2);
        }
        // the foot stands on the inlet of the ground storey
        assert!(sp.links.iter().any(|l| l.storey == 0 && l.connector == "Outlet"));
    }
    assert!(fire_check(&p).is_empty(), "{:?}", first(&fire_check(&p)));
    let keys: Vec<&str> = explain_project(&s, &p).iter().map(|w| w.key).collect();
    assert!(keys.contains(&"standpipes"), "{keys:?}");
}

/// A building over the height rule with no room for a standpipe is a fault, and a short hose
/// leaves rooms out of reach, which the check reports.
#[test]
fn the_hose_reach_rule_is_measured() {
    let (_, p) = built(office(3, json!({"rules": {"fire.standpipe_height": 0, "fire.hose_reach": 10}})));
    assert!(!p.fire.reach_gaps.is_empty());
    assert!(fire_check(&p).iter().any(|i| i.contains("hose reach")));
}

/// The why of a sprinklered building names the hazard classes it used.
#[test]
fn the_hazard_classes_explain_themselves() {
    let (s, p) = built(office(3, json!({"sprinklers": true})));
    let w = explain_project(&s, &p);
    assert!(w.iter().any(|w| w.key == "sprinkler_hazard"), "{:?}", w.iter().map(|w| w.key).collect::<Vec<_>>());
}
