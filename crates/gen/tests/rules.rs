// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The rule set: built-in file, layers, range checks, the brief's `rules`, and the check that
//! the builder holds no decision number outside the rule set.

use ddc_gen::project::rules::{self, defs, rule, scoped, Layer, RuleSet};
use ddc_gen::project::{build_project, limits, parse_spec};
use serde_json::json;

#[test]
fn the_builtin_set_is_well_formed() {
    assert!(defs().len() >= 60, "{} rules", defs().len());
    let mut aliases = vec![];
    for d in defs() {
        assert!(d.id.starts_with(&format!("{}.", d.group)), "{}", d.id);
        assert!(d.min <= d.max, "{}", d.id);
        if let Some(x) = d.default {
            assert!(d.contains(x), "{} default {x} outside {}", d.id, d.range());
        }
        if let Some(a) = &d.alias {
            assert!(!aliases.contains(a), "alias {a} twice");
            aliases.push(a.clone());
        }
        assert_eq!(d.title_key(), format!("gen.rule.{}.title", d.id));
    }
    for g in ["grid", "depth", "corridor", "core", "egress", "massing", "facade", "site", "mep", "light", "access", "fire"] {
        assert!(rules::groups().contains(&g), "group {g}");
    }
}

#[test]
fn brief_fields_read_their_rules() {
    assert_eq!(limits::FLOOR_HEIGHT.path, "floor_height");
    assert_eq!(limits::FLOOR_HEIGHT.rule, "massing.floor_height");
    assert_eq!((limits::FLOOR_HEIGHT.min, limits::FLOOR_HEIGHT.max), (2.7, 6.0));
    assert_eq!(limits::GRID.default, Some(7.2));
    assert_eq!(limits::max_basements(), 3);
    assert_eq!(limits::max_trees(), 40);
    // two bands and the corridor between them
    let (b, c, d) = (*limits::BAND_DEPTH, *limits::CORRIDOR, *limits::DEPTH);
    assert!((d.min - (2.0 * b.min + c.min)).abs() < 1e-9);
    assert!((d.max - (2.0 * b.max + c.max)).abs() < 1e-9);
    for p in limits::params() {
        assert_eq!(rules::by_alias(p.path).map(|r| r.id.as_str()), Some(p.rule));
    }
}

#[test]
fn layers_override_in_order_and_fall_back() {
    let mut rs = RuleSet::new();
    let id = "corridor.hall_min";
    assert_eq!(rs.resolve(id), Some((1.5, Layer::Builtin)));
    rs.set(Layer::Norm, id, 1.6).unwrap();
    rs.set(Layer::Brief, id, 1.8).unwrap();
    rs.set(Layer::Preset, id, 1.7).unwrap();
    assert_eq!(rs.resolve(id), Some((1.8, Layer::Brief)));
    rs.clear(Layer::Brief, id);
    assert_eq!(rs.resolve(id), Some((1.7, Layer::Preset)));
    rs.clear_layer(Layer::Preset);
    assert_eq!(rs.source(id), Some(Layer::Norm));
    assert!(rs.set(Layer::Builtin, id, 1.6).is_err());
    // a rule only the preset decides has no value until a layer sets one
    assert_eq!(rs.value("depth.band"), None);
    rs.set(Layer::Preset, "depth.band", 6.0).unwrap();
    assert_eq!(rs.get("depth.band"), 6.0);
}

#[test]
fn values_outside_the_range_are_refused() {
    let mut rs = RuleSet::new();
    let e = rs.set(Layer::Project, "corridor.width", 0.5).unwrap_err();
    assert_eq!(e.rule, "corridor.width");
    assert!(e.msg.contains("1.2..4"), "{}", e.msg);
    assert!(rs.set(Layer::Project, "basement.count", 1.5).is_err());
    assert!(rs.set(Layer::Project, "no.such", 1.0).is_err());
    assert!(rs.set(Layer::Project, "corridor.width", f64::NAN).is_err());
    assert!(rs.set_goal(Layer::Project, "corridor.width", Some(-1.0), None).is_err());
    rs.set_goal(Layer::Project, "corridor.width", Some(5.0), Some(true)).unwrap();
    assert_eq!((rs.weight("corridor.width"), rs.hard("corridor.width")), (5.0, true));
}

#[test]
fn a_patch_applies_whole_or_not_at_all() {
    let mut rs = RuleSet::new();
    let bad = json!({"room.door": 1.0, "corridor.width": 99});
    let errs = rs.apply_patch(Layer::Brief, &bad).unwrap_err();
    assert_eq!(errs.len(), 1);
    assert_eq!(rs, RuleSet::new());
    let good = json!({"room.door": 1.0, "grid.span": {"value": 6.0, "weight": 2, "hard": false}});
    rs.apply_patch(Layer::Brief, &good).unwrap();
    assert_eq!(rs.get("room.door"), 1.0);
    assert_eq!(rs.weight("grid.span"), 2.0);
    let back = RuleSet::from_json(&rs.to_json()).unwrap();
    assert_eq!(back, rs);
    assert_eq!(rs.to_json()["schema"], "gen_rules.v1");
    rs.apply_patch(Layer::Brief, &json!({"room.door": null})).unwrap();
    assert_eq!(rs.source("room.door"), Some(Layer::Builtin));
    assert!(RuleSet::from_json(&json!({"builtin": {}})).is_err());
}

#[test]
fn the_builder_reads_the_scoped_set() {
    assert_eq!(rule("room.min_unit"), 1.8);
    let mut rs = RuleSet::new();
    rs.set(Layer::Project, "room.min_unit", 2.4).unwrap();
    let inner = scoped(&rs, || {
        let nested = scoped(&RuleSet::new(), || rule("room.min_unit"));
        (rule("room.min_unit"), nested)
    });
    assert_eq!(inner, (2.4, 1.8));
    assert_eq!(rule("room.min_unit"), 1.8);
}

#[test]
fn the_brief_sets_rules_and_their_brief_fields() {
    let s = parse_spec(&json!({"typology": "office", "floors": 3,
        "rules": {"corridor.width": 2.0, "facade.glazing_share": 0.25}}))
    .unwrap();
    assert_eq!(s.corridor, 2.0);
    assert_eq!(s.rules.resolve("facade.glazing_share"), Some((0.25, Layer::Brief)));
    let e = parse_spec(&json!({"typology": "office", "floors": 3, "rules": {"room.door": 5}}))
        .unwrap_err();
    assert_eq!(e.0[0].path, "rules.room.door");
}

fn glazed(p: &ddc_gen::project::GProject) -> f64 {
    p.storeys
        .iter()
        .flat_map(|s| &s.layout.openings)
        .filter(|o| !o.door)
        .map(|o| o.width * o.height)
        .sum()
}

#[test]
fn a_changed_rule_changes_the_building() {
    let base = json!({"typology": "office", "floors": 2});
    let mut more = base.clone();
    more["rules"] = json!({"facade.glazing_share": 0.25});
    let a = build_project(&parse_spec(&base).unwrap()).unwrap();
    let b = build_project(&parse_spec(&more).unwrap()).unwrap();
    assert!(glazed(&b) > glazed(&a), "{} vs {}", glazed(&b), glazed(&a));
    // the build leaves no rule set behind on the thread
    assert_eq!(rule("facade.glazing_share"), 0.125);
}

/// Numbers that are not decisions: counts, halves, unit conversion, rounding to 10 mm,
/// three-digit room numbers and a float tolerance.
const STRUCTURAL: [&str; 11] = ["0", "1", "2", "3", "4", "0.0", "1.0", "2.0", "0.5", "1e-9", "1000.0"];
/// Per file: tolerances, array lengths and indices, unit conversion, formula constants and the
/// named physical constants of the services.
const EXCEPTIONS: &[(&str, &str, &str)] = &[
    ("lift.rs", "1e-6", "geometric tolerance"),
    ("sprinkler.rs", "60.0", "seconds in a minute"),
    ("sprinkler.rs", "1e-6", "float tolerance"),
    ("fire_check.rs", "1e-6", "float tolerance"),
    ("fire_check.rs", "100.0", "percent"),
    ("build.rs", "9", "round up to whole centimetres"),
    ("build.rs", "10", "round up to whole centimetres"),
    ("build.rs", "99", "room numbers take three digits past 99"),
    ("build.rs", "1.01", "a cell may exceed its area by 1 % of rounding"),
    ("nav.rs", "1e-6", "a door centre on a wall centre line, float noise"),
    ("nav.rs", "6", "room numbers a finding lists"),
    ("nav.rs", "10.0", "fix values in whole decimetres"),
    ("svc.rs", "1e-6", "float tolerance"),
    ("svc.rs", "1e-3", "model joint tolerance"),
    ("svc.rs", "5e-3", "connector tolerance"),
    ("svc.rs", "2e-3", "float tolerance of the clash test"),
    ("svc.rs", "1e-4", "float tolerance of the fall test"),
    ("svc.rs", "1e-12", "float tolerance of the velocity test"),
    ("svc.rs", "6", "box [x0, y0, z0, x1, y1, z1] length"),
    ("svc.rs", "5", "box last index"),
    ("svc.rs", "100.0", "percent"),
    ("svc.rs", "3.0", "Manning exponent 2/3"),
    ("svc.rs", "4.0", "circle area pi d2/4, hydraulic radius d/4"),
    ("svc.rs", "8.0", "half-full circle area pi d2/8"),
    ("svc.rs", "0.013", "Manning roughness of PVC, const N_PVC"),
    ("water.rs", "0.02", "tolerance of the frame match"),
    ("water.rs", "1e-6", "float tolerance"),
    ("water.rs", "100.0", "percent"),
    ("elec.rs", "1e-3", "float tolerance"),
    ("elec.rs", "1e-6", "float tolerance"),
    ("elec.rs", "5e-3", "float tolerance"),
    ("elec.rs", "5", "circuit code table index"),
    ("elec.rs", "100.0", "wall top above any storey, const WALL_TOP"),
    ("elec.rs", "1000000", "circuit key stride, const KEY_STRIDE"),
    ("hvac.rs", "1e-6", "float tolerance"),
    ("hvac.rs", "12", "length of MOVABLE"),
    ("hvac.rs", "6", "length of REFUSALS"),
    ("hvac.rs", "11", "length of SHAFT_ROOMS"),
    ("hvac.rs", "5", "last refusal code"),
    ("hvac.rs", "10.0", "duct length metric rounded to 0.1 m"),
    ("mech/geo.rs", "6", "box [x0, y0, z0, x1, y1, z1] length"),
    ("mech/geo.rs", "5", "box last index"),
    ("mech/geo.rs", "1e-12", "float tolerance of a direction"),
    ("mech/geo.rs", "1e-3", "model joint tolerance of a level"),
    ("mech/loads.rs", "3600.0", "seconds per hour"),
    ("mech/loads.rs", "5", "heating parts, const HEAT_PARTS"),
    ("mech/loads.rs", "7", "cooling parts, const COOL_PARTS"),
    ("mech/loads.rs", "360.0", "full turn, degrees"),
    ("mech/loads.rs", "90.0", "quarter turn between compass points"),
    ("mech/loads.rs", "1e-3", "parallel edge tolerance"),
    ("mech/loads.rs", "1e-6", "float tolerance"),
    ("mech/ducts.rs", "4.0", "circle area pi d2/4"),
    ("mech/ducts.rs", "1e-6", "float tolerance"),
    ("mech/ducts.rs", "1e-3", "float tolerance of a turn"),
    ("mech/ducts.rs", "5e-3", "connector tolerance"),
    ("mech/ducts.rs", "3600.0", "seconds per hour"),
    ("mech/ducts.rs", "5", "box last index"),
    ("mech/pipes.rs", "4.0", "circle area pi d2/4"),
    ("mech/pipes.rs", "3600.0", "seconds per hour"),
    ("mech/pipes.rs", "1e-6", "float tolerance"),
    ("mech/pipes.rs", "1e-3", "float tolerance of a turn"),
    ("mech/pipes.rs", "5e-3", "connector tolerance"),
    ("mech/mod.rs", "3600.0", "seconds per hour"),
    ("mech/mod.rs", "5", "box last index"),
    ("mech/mod.rs", "1e-6", "float tolerance"),
    ("mech/mod.rs", "1e-3", "model joint tolerance of a level"),
    ("mech/mod.rs", "10.0", "metrics rounded to 0.1"),
    ("mech/mod.rs", "100.0", "metrics rounded to 0.01"),
    ("assembly.rs", "1e-6", "float noise of a plan point on a wall axis, const ON_AXIS"),
    ("assembly.rs", "10", "length of Use::ALL"),
    ("svc.rs", "8", "length of SHAFTS"),
    ("elec.rs", "6", "circuit code table index"),
    ("elec.rs", "7", "circuit code table index"),
    ("circuits.rs", "100.0", "percent, const PCT"),
    ("circuits.rs", "8", "length of USES and CODES"),
    ("circuits.rs", "1e-6", "float tolerance"),
    ("circuits.rs", "5e-3", "float tolerance"),
    ("hvac.rs", "7", "length of REFUSALS"),
    ("sprinkler.rs", "4.0", "circle area pi d2/4"),
    ("mech/ducts.rs", "1e-12", "float tolerance of a parallel or empty segment"),
];

/// Numeric literals outside comments, strings and the test module.
fn literals(src: &str) -> Vec<(usize, String)> {
    let code = src.split("#[cfg(test)]").next().unwrap_or("");
    let mut out = vec![];
    for (n, line) in code.lines().enumerate() {
        let mut s = String::new();
        let mut quoted = false;
        for c in line.chars() {
            if c == '"' {
                quoted = !quoted;
            } else if !quoted {
                s.push(c);
            }
        }
        let s = s.split("//").next().unwrap_or("");
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            let prev = if i == 0 { ' ' } else { b[i - 1] };
            if b[i].is_ascii_digit() && !(prev.is_alphanumeric() || prev == '_' || prev == '.') {
                let mut t = String::new();
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '_') {
                    t.push(b[i]);
                    i += 1;
                }
                if i + 1 < b.len() && b[i] == '.' && b[i + 1].is_ascii_digit() {
                    t.push('.');
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        t.push(b[i]);
                        i += 1;
                    }
                }
                if i + 1 < b.len() && b[i] == 'e' && (b[i + 1] == '-' || b[i + 1].is_ascii_digit()) {
                    t.push('e');
                    i += 1;
                    while i < b.len() && (b[i] == '-' || b[i].is_ascii_digit()) {
                        t.push(b[i]);
                        i += 1;
                    }
                }
                while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                    i += 1;
                }
                out.push((n + 1, t.replace('_', "")));
            } else {
                i += 1;
            }
        }
    }
    out
}

const SOURCES: [(&str, &str); 25] = [
    ("build.rs", include_str!("../src/project/build.rs")),
    ("block.rs", include_str!("../src/project/block.rs")),
    ("tile.rs", include_str!("../src/project/tile.rs")),
    ("basement.rs", include_str!("../src/project/basement.rs")),
    ("nav.rs", include_str!("../src/project/nav.rs")),
    ("stack.rs", include_str!("../src/project/stack.rs")),
    ("site.rs", include_str!("../src/project/site.rs")),
    ("split.rs", include_str!("../src/project/site/split.rs")),
    ("svc.rs", include_str!("../src/project/svc.rs")),
    ("water.rs", include_str!("../src/project/water.rs")),
    ("elec.rs", include_str!("../src/project/elec.rs")),
    ("circuits.rs", include_str!("../src/project/circuits.rs")),
    ("hvac.rs", include_str!("../src/project/hvac.rs")),
    ("sprinkler.rs", include_str!("../src/project/sprinkler.rs")),
    ("mech/geo.rs", include_str!("../src/project/mech/geo.rs")),
    ("mech/loads.rs", include_str!("../src/project/mech/loads.rs")),
    ("mech/select.rs", include_str!("../src/project/mech/select.rs")),
    ("mech/equip.rs", include_str!("../src/project/mech/equip.rs")),
    ("mech/ducts.rs", include_str!("../src/project/mech/ducts.rs")),
    ("mech/pipes.rs", include_str!("../src/project/mech/pipes.rs")),
    ("mech/mod.rs", include_str!("../src/project/mech/mod.rs")),
    ("standpipe.rs", include_str!("../src/project/standpipe.rs")),
    ("fire_check.rs", include_str!("../src/project/fire_check.rs")),
    ("lift.rs", include_str!("../src/project/lift.rs")),
    ("assembly.rs", include_str!("../src/project/assembly.rs")),
];

/// The services' rules default to the numbers they replaced in code, so moving them changed
/// no design.
#[test]
fn service_rules_default_to_the_numbers_they_replaced() {
    let text = include_str!("data/services_rule_defaults.json");
    let was: std::collections::BTreeMap<String, f64> = serde_json::from_str(text).unwrap();
    assert!(was.len() >= 200, "{} rules", was.len());
    for (id, x) in &was {
        let d = rules::def(id).unwrap_or_else(|| panic!("rule {id} is not in rules.json"));
        assert_eq!(d.group, "services", "{id}");
        assert_eq!(rule(id), *x, "{id}");
    }
    assert_eq!(defs().iter().filter(|d| d.group == "services").count(), was.len());
}

#[test]
fn the_builder_holds_no_number_outside_the_rules() {
    let mut stray = vec![];
    for (file, src) in SOURCES {
        for (line, lit) in literals(src) {
            let excused = STRUCTURAL.contains(&lit.as_str())
                || EXCEPTIONS.iter().any(|e| e.0 == file && e.1 == lit);
            if !excused {
                stray.push(format!("{file}:{line} {lit}"));
            }
        }
    }
    assert!(stray.is_empty(), "numbers without a rule: {stray:?}");
    assert_eq!(literals("let x = a.0 + 2.5 * y1; // 7.0\n"), vec![(1, "2.5".to_string())]);
}

#[test]
fn every_rule_the_builder_reads_exists_with_a_default() {
    let plate = include_str!("../src/project/plate.rs");
    let all = SOURCES.iter().map(|s| s.1).chain([plate]);
    let mut seen = 0;
    for src in all {
        for pat in ["rule(\"", "rule_mm(\""] {
            for part in src.split(pat).skip(1) {
                let id = part.split('"').next().unwrap_or("");
                let d = rules::def(id).unwrap_or_else(|| panic!("rule {id} is not in rules.json"));
                assert!(d.default.is_some(), "rule {id} is read by the builder but has no default");
                seen += 1;
            }
        }
    }
    assert!(seen >= 30, "{seen} rule reads");
}
