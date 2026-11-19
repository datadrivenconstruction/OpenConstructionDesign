// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Structural system from the brief (`design.structure`): a timber brief builds the same plan
//! with CLT and glulam types, a frame stands columns on the grid while bearing walls do not,
//! the brief check asks for the system, a wrong word is refused.

use ddc_gen::project::{brief, build_project, parse_spec, structure, Structure};
use serde_json::json;

fn section(design: serde_json::Value, features: serde_json::Value) -> serde_json::Value {
    json!({"typology": "apartment_section", "floors": 10, "design": design, "features": features})
}

fn columns(structure: &str) -> Vec<usize> {
    let s = parse_spec(&section(json!({"structure": structure}), json!([]))).unwrap();
    let p = build_project(&s).unwrap_or_else(|e| panic!("{structure}: {e}"));
    p.storeys.iter().map(|k| k.layout.columns.len()).collect()
}

#[test]
fn a_timber_brief_builds_clt_types_and_the_check_asks_for_them() {
    let s = parse_spec(&section(json!({"structure": "timber"}), json!(["timber", "clt"]))).unwrap();
    assert_eq!(s.design.structure, Structure::Timber);
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    let c = brief::check(&s, &p);
    assert!(c.repair.is_none(), "{}", c.report);
    assert!(c.report.to_string().contains("CLT"), "{}", c.report);

    // the same plan as concrete: only the types change
    let mut v0 = section(json!({}), json!(["timber"]));
    v0["rules"] = json!({"structure.timber_from_brief": false});
    let s0 = parse_spec(&v0).unwrap();
    assert_eq!(s0.design.structure, Structure::Concrete);
    let p0 = build_project(&s0).unwrap();
    assert_eq!(p0.storeys.len(), p.storeys.len());
    let c0 = brief::check(&s0, &p0);
    assert_eq!(c0.repair, Some(json!({"design": {"structure": "timber"}})), "{}", c0.report);

    // glulam asks for the frame
    let c1 = brief::check(&s, &p);
    assert!(c1.repair.is_none());
    let s2 = parse_spec(&section(json!({"structure": "timber"}), json!(["glulam"]))).unwrap();
    let c2 = brief::check(&s2, &build_project(&s2).unwrap());
    assert_eq!(c2.repair, Some(json!({"design": {"structure": "timber_frame"}})), "{}", c2.report);

    let t = structure::system(Structure::Timber);
    assert!(t.floor.layers.iter().any(|l| l.0.contains("Timber")));
}

#[test]
fn a_timber_feature_alone_picks_the_timber_structure() {
    for (feature, want) in [("clt", Structure::Timber), ("wood", Structure::Timber), ("glulam", Structure::TimberFrame), ("post_and_beam", Structure::TimberFrame)] {
        let s = parse_spec(&section(json!({}), json!([feature]))).unwrap();
        assert_eq!(s.design.structure, want, "{feature}");
        let p = build_project(&s).unwrap_or_else(|e| panic!("{feature}: {e}"));
        assert!(brief::check(&s, &p).repair.is_none(), "{feature}");
    }
    // glulam beside CLT is the frame, in either order
    for f in [json!(["clt", "glulam"]), json!(["glulam", "clt"])] {
        let s = parse_spec(&section(json!({}), f)).unwrap();
        assert_eq!(s.design.structure, Structure::TimberFrame);
    }
    // a structure the brief names itself wins
    let s = parse_spec(&section(json!({"structure": "masonry"}), json!(["timber"]))).unwrap();
    assert_eq!(s.design.structure, Structure::Masonry);
}

#[test]
fn a_frame_stands_columns_on_every_storey_and_bearing_walls_none() {
    for bearing in ["concrete", "timber", "masonry"] {
        assert!(columns(bearing).iter().all(|n| *n == 0), "{bearing}: {:?}", columns(bearing));
    }
    for frame in ["frame", "timber_frame"] {
        let n = columns(frame);
        assert!(n.iter().all(|k| *k >= 4), "{frame}: {n:?}");
        assert!(n.iter().sum::<usize>() >= 60, "{frame}: {n:?}");
    }
    let s = parse_spec(&section(json!({"structure": "timber_frame"}), json!([]))).unwrap();
    let p = build_project(&s).unwrap();
    for k in &p.storeys {
        let l = &k.layout;
        for c in &l.columns {
            for h in &l.slab_holes {
                let (x0, x1) = h.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
                let (y0, y1) = h.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
                assert!(!(c.at.0 > x0 && c.at.0 < x1 && c.at.1 > y0 && c.at.1 < y1), "column in a hole at {:?}", c.at);
            }
        }
    }
    assert!(structure::system(Structure::TimberFrame).column_material.contains("Glulam"));
}

#[test]
fn an_unknown_structure_is_refused_with_the_choices() {
    let e = parse_spec(&section(json!({"structure": "bamboo"}), json!([]))).unwrap_err().to_string();
    assert!(e.contains("design.structure"), "{e}");
}
