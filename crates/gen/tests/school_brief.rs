// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_gen::project::{build_project, check_project, parse_spec};
use serde_json::json;

/// A plain four-storey courtyard school: the programme is per wing, so twelve classrooms on
/// four wings must share out instead of overflowing the storey's slots.
#[test]
fn a_four_storey_courtyard_school_builds() {
    let rooms = json!([{"kind": "classroom", "area": 60, "count": 12}, {"kind": "gym", "area": 300}]);
    for v in [
        json!({"typology": "school_campus", "floors": 4, "rooms": rooms, "design": {"massing": "courtyard"}}),
        json!({"typology": "school_campus", "floors": 4, "rooms": rooms, "lift": true,
            "design": {"massing": "courtyard", "trees": 12, "paving": 3.0},
            "rules": {"site.path_width": 3.0}}),
        json!({"typology": "school_campus", "floors": 4, "design": {"massing": "courtyard", "trees": 20}}),
    ] {
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
        assert!(p.storeys.len() >= 4, "{v}");
        let names: Vec<String> =
            p.storeys.iter().flat_map(|s| s.layout.rooms.iter().map(|r| r.name.to_lowercase())).collect();
        assert!(names.iter().filter(|n| n.contains("class")).count() >= 12, "{v}: {names:?}");
        assert!(names.iter().any(|n| n.contains("gym") || n.contains("sport")), "{v}: {names:?}");
        let g = check_project(&s, &p);
        assert!(g.passed(), "{v}: {:?}", g.failures);
    }
}

#[test]
fn a_site_rule_under_site_names_its_rules_path() {
    let e = parse_spec(&json!({"typology": "school_campus", "site": {"path_width": 3}})).unwrap_err().to_string();
    assert!(e.contains("\"site.path_width\"") && e.contains("rules"), "{e}");
}
