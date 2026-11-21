// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Site trees through the tool path: generate_project plants the library's Tree species, the
//! climate words of the brief pick the mix, and the trees differ in species and size.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;
#[path = "app2_fx/mod.rs"]
mod app2_fx;

use app2_fx::fx;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;
use std::collections::BTreeSet;

/// Library type names of the Tree comps the model holds, in order of id.
fn planted(climate: Option<&str>, rules: serde_json::Value) -> Vec<String> {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let mut spec = json!({"typology": "office", "floors": 3, "design": {"trees": 12, "paving": 3.0}, "rules": rules});
    if let Some(c) = climate {
        spec["site"] = json!({"climate": c});
    }
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    apply_and_check(&mut s, &mut ed);
    let mut st = ed
        .model
        .sql()
        .prepare(
            "SELECT t.name FROM comp c JOIN comp_type t ON t.id=c.ctype \
             JOIN comp_family f ON f.id=t.family WHERE f.name='Tree' ORDER BY c.id",
        )
        .unwrap();
    st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|x| x.unwrap()).collect()
}

fn species(names: &[String]) -> BTreeSet<String> {
    names.iter().map(|n| n.split(' ').next().unwrap_or(n).to_string()).collect()
}

#[test]
fn a_plain_brief_plants_a_mixed_wood_of_library_species() {
    let t = planted(None, json!({}));
    assert!(t.len() >= 6, "{t:?}");
    assert!(t.iter().all(|n| n != "Oak" && n != "Large" && n != "Small"), "{t:?}");
    assert!(species(&t).len() >= 5, "many species: {t:?}");
    // the street row is an avenue of one type; off it neighbours differ
    let most = t.iter().max_by_key(|n| t.iter().filter(|m| m == n).count()).unwrap();
    assert!(t.windows(2).all(|w| w[0] != w[1] || &w[0] == most), "neighbours differ: {t:?}");
    assert_eq!(t, planted(None, json!({})), "the same brief plants the same trees");
}

#[test]
fn north_and_mountain_words_plant_conifers() {
    let ok = ["Spruce", "Fir", "Poplar", "Cypress", "Hornbeam", "Scots", "Larch"];
    for c in ["a northern lodge", "Mountain village"] {
        let t = planted(Some(c), json!({}));
        assert!(t.len() >= 6 && t.iter().all(|n| ok.iter().any(|o| n.starts_with(o))), "{c}: {t:?}");
        assert!(t.iter().any(|n| n.starts_with("Spruce") || n.starts_with("Fir")), "{c}: {t:?}");
    }
}

#[test]
fn tropical_words_plant_palms_and_a_named_mix_wins() {
    let t = planted(Some("tropical resort"), json!({}));
    assert!(t.iter().any(|n| n.contains("Palm")), "{t:?}");
    assert!(species(&t).len() >= 3, "{t:?}");
    let t = planted(Some("tropical resort"), json!({"site.tree_mix": "conifer"}));
    assert!(t.iter().all(|n| !n.contains("Palm")), "the brief's own rule wins: {t:?}");
}

#[test]
fn each_planted_tree_carries_its_seeded_scale() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let spec = json!({"typology": "office", "floors": 3, "design": {"trees": 12, "paving": 3.0}});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    assert!(tool_results(&provider).iter().all(|r| !r.1));
    apply_and_check(&mut s, &mut ed);
    let ids = comps_of(&ed.model, &["Tree"]);
    assert!(ids.len() >= 6, "{ids:?}");
    let scales: Vec<f64> = ids
        .iter()
        .map(|&i| {
            let c = ed.model.component(i).unwrap();
            match c.values.iter().find(|v| v.0 == "Scale") {
                Some((_, ddc_geom::component::Value::Num(x))) => *x,
                _ => 1.0,
            }
        })
        .collect();
    assert!(scales.iter().all(|s| (0.8 - 1e-9..=1.2 + 1e-9).contains(s)), "{scales:?}");
    assert!(scales.iter().any(|s| (s - 1.0).abs() > 0.05), "trees keep different sizes: {scales:?}");
    // the clear trunk is floored by the rule, as an instance value
    for &i in &ids {
        let c = ed.model.component(i).unwrap();
        let m = c.values.iter().find(|v| v.0 == "MinClear").map(|v| match v.1 {
            ddc_geom::component::Value::Num(x) => x,
            _ => 0.0,
        });
        assert!(m.is_some_and(|x| x > 0.5 && x <= 1.8 + 1e-9), "{m:?}");
    }
}

/// One planted site read back: the trees (type, plan point, Scale, MinClear) and the hedge
/// lengths (plan point, turn).
struct Site {
    trees: Vec<(String, f64, f64, f64, f64)>,
    hedge: Vec<(f64, f64, f64)>,
}

fn landscape(brief: &str) -> Site {
    landscape_on(brief, json!({}))
}

fn landscape_on(brief: &str, rules: serde_json::Value) -> Site {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let spec = json!({"typology": "office", "floors": 3, "design": {"trees": 14, "paving": 3.0}, "site": {"brief": brief}, "rules": rules});
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{brief}: {results:?}");
    apply_and_check(&mut s, &mut ed);
    let rows = |family: &str| -> Vec<(String, f64, f64, f64, Vec<(String, ddc_geom::component::Value)>)> {
        let mut st = ed
            .model
            .sql()
            .prepare(
                "SELECT c.id, t.name, c.x, c.y, c.rotation FROM comp c JOIN comp_type t ON t.id=c.ctype \
                 JOIN comp_family f ON f.id=t.family WHERE f.name=?1 ORDER BY c.id",
            )
            .unwrap();
        let v: Vec<(i64, String, f64, f64, f64)> =
            st.query_map([family], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap().map(|x| x.unwrap()).collect();
        v.into_iter().map(|(id, n, x, y, rot)| (n, x, y, rot, ed.model.component(id).unwrap().values)).collect()
    };
    let num = |v: &[(String, ddc_geom::component::Value)], k: &str, d: f64| match v.iter().find(|x| x.0 == k) {
        Some((_, ddc_geom::component::Value::Num(x))) => *x,
        _ => d,
    };
    Site {
        trees: rows("Tree").into_iter().map(|(n, x, y, _, v)| (n, x, y, num(&v, "Scale", 1.0), num(&v, "MinClear", 0.0))).collect(),
        hedge: rows("Hedge").into_iter().map(|(_, x, y, rot, _)| (x, y, rot)).collect(),
    }
}

/// Plan symbol lines of the library tree type, beyond the trunk circle.
fn symbol_lines(ty: &str) -> usize {
    let src = ddc_model::library_sources().into_iter().find(|s| s.lines().any(|l| l.trim() == "family Tree")).unwrap();
    let f = ddc_geom::component::Family::parse(src).unwrap();
    let r = f.resolve(&f.family_type(ty).unwrap().values, &[]).unwrap();
    f.build(&r, ddc_geom::component::Lod::Fine).unwrap().plan.len() - 12
}

#[test]
fn a_park_brief_in_three_languages_plants_the_named_species_and_a_street_hedge() {
    let ru_park = fx("trees_ru_park");
    let briefs = [
        "park with oaks, birches and a hedge along the street",
        ru_park.as_str(),
        "Park mit Eichen, Birken und einer Hecke entlang der Straße",
    ];
    for brief in briefs {
        // the street is on the west: a hedge "along the street" runs there, not on the default south
        let s = landscape_on(brief, json!({"site.street_side": "west"}));
        let count = |sp: &str| s.trees.iter().filter(|t| t.0.starts_with(&format!("{sp} "))).count();
        assert!(count("Oak") > 0 && count("Birch") > 0, "{brief}: {:?}", s.trees.iter().map(|t| &t.0).collect::<Vec<_>>());
        assert_eq!(count("Oak") + count("Birch"), s.trees.len(), "{brief}: only the named species");
        // no two crowns meet
        let crown = |t: &(String, f64, f64, f64, f64)| ddc_gen::project::tree_dims(&t.0.clone()).map(|d| d.1 * t.3 / 2.0).unwrap();
        for (i, a) in s.trees.iter().enumerate() {
            for b in &s.trees[i + 1..] {
                let d = ((a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt();
                assert!(d + 1e-6 >= crown(a) + crown(b), "{brief}: {} and {} {d:.2} m apart", a.0, b.0);
            }
        }
        // the clear trunk keeps the rule minimum (or 0.4 of the height of a small tree)
        for t in &s.trees {
            let (h, _, clear) = ddc_gen::project::tree_dims(&t.0).unwrap();
            let floor = 1.8f64.min(0.4 * h * t.3);
            assert!((clear * t.3).max(t.4) + 1e-9 >= floor, "{brief}: {} clear {}", t.0, t.4);
        }
        // the hedge is one row along the street edge (west): every length at the lowest x, turned along y
        assert!(s.hedge.len() >= 3, "{brief}: {} hedge lengths", s.hedge.len());
        let west = s.hedge.iter().map(|h| h.0).fold(f64::MAX, f64::min);
        assert!(s.hedge.iter().all(|h| (h.0 - west).abs() < 0.6 && h.2.cos().abs() < 1e-6), "{brief}: {:?}", s.hedge);
        // deciduous trees draw a scalloped crown (outline and spokes), not a bare circle
        for t in &s.trees {
            assert!(symbol_lines(&t.0) >= 100, "{}: {}", t.0, symbol_lines(&t.0));
        }
    }
}

#[test]
fn a_conifer_brief_draws_stars_in_plan() {
    let s = landscape("a mountain lodge park with spruces");
    assert!(!s.trees.is_empty());
    assert!(s.trees.iter().all(|t| t.0.starts_with("Spruce ")), "{:?}", s.trees.iter().map(|t| &t.0).collect::<Vec<_>>());
    for t in &s.trees {
        assert_eq!(symbol_lines(&t.0), 33 + 9, "{}: a star of 22 edges, 11 rays and the trunk dot", t.0);
    }
}

#[test]
fn a_named_edge_and_the_climate_fallback_come_from_the_brief() {
    // "the north edge" puts the hedge on the highest y and is no northern climate: broadleaf trees
    let n = landscape("park with a hedge along the north edge");
    assert!(n.hedge.len() >= 3);
    let top = n.hedge.iter().map(|h| h.1).fold(f64::MIN, f64::max);
    assert!(n.hedge.iter().all(|h| (h.1 - top).abs() < 0.6), "{:?}", n.hedge);
    // no species words: the climate's mix plants the trees
    let ok = ["Spruce", "Fir", "Poplar", "Cypress", "Hornbeam", "Scots", "Larch"];
    assert!(n.trees.iter().any(|t| !ok.iter().any(|o| t.0.starts_with(o))), "{:?}", n.trees.iter().map(|t| &t.0).collect::<Vec<_>>());
    let m = landscape("a mountain park");
    assert!(m.trees.len() >= 6 && m.trees.iter().all(|t| ok.iter().any(|o| t.0.starts_with(o))), "{:?}", m.trees.iter().map(|t| &t.0).collect::<Vec<_>>());
    // no hedge word, no hedge edge: the default hedge runs round the plot
    let all = landscape("a quiet park");
    let (lo, hi) = all.hedge.iter().fold((f64::MAX, f64::MIN), |a, h| (a.0.min(h.0), a.1.max(h.0)));
    assert!(hi - lo > 5.0, "{lo}..{hi}");
}
