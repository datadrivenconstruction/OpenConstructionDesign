// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use ddc_gen::project::{build_project, check_project, parse_spec};
use ddc_gen::WallRole;

fn inside(r: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = r.len();
    let mut odd = false;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            odd = !odd;
        }
    }
    odd
}
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
        // the site keeps a paved walk at every entrance
        let l = &p.storeys[0].layout;
        let path: Vec<(f64, f64)> =
            l.fixtures.iter().filter(|f| f.family == "Paving Slab" && f.ty == "Path").map(|f| f.at).collect();
        let built = |q: (f64, f64)| inside(&l.slab, q) && !l.slab_holes.iter().any(|h| inside(h, q));
        // every door in an exterior wall is an entrance: open ground on one side of it (the
        // joints of the wings are inner walls)
        let doors: Vec<(f64, f64)> = l
            .openings
            .iter()
            .filter(|o| o.door && l.walls[o.wall].role == WallRole::Exterior)
            .map(|o| {
                let w = &l.walls[o.wall];
                let (d, t) = ((w.b.0 - w.a.0, w.b.1 - w.a.1), o.along + o.width / 2.0);
                let n = d.0.hypot(d.1);
                let at = (w.a.0 + d.0 / n * t, w.a.1 + d.1 / n * t);
                let side = |s: f64| (at.0 - d.1 / n * s, at.1 + d.0 / n * s);
                assert!(!built(side(1.0)) || !built(side(-1.0)), "{v}: the exterior door at {at:?} has building on both sides");
                at
            })
            .collect();
        assert!(!doors.is_empty(), "{v}");
        for d in doors {
            let pads = l.fixtures.iter().filter(|f| f.family == "Paving Slab" && f.ty != "Path").count();
            assert!(
                path.iter().any(|q| (q.0 - d.0).hypot(q.1 - d.1) < 2.5),
                "{v}: no path at the door {d:?} ({} path tiles, {pads} pads, {} fixtures on the ground storey)",
                path.len(),
                l.fixtures.len()
            );
        }
    }
}

#[test]
fn a_site_rule_under_site_names_its_rules_path() {
    let e = parse_spec(&json!({"typology": "school_campus", "site": {"path_width": 3}})).unwrap_err().to_string();
    assert!(e.contains("\"site.path_width\"") && e.contains("rules"), "{e}");
}
