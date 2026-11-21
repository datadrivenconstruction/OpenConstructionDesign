// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Site trees: species come from the library's Tree family by the rule `site.tree_mix`;
//! neighbours differ in species and turn.

use ddc_gen::project::{build_project, check_project, parse_spec, GProject};
use serde_json::json;

fn site(mix: Option<&str>, trees: u32) -> GProject {
    in_season(mix, None, trees)
}

fn in_season(mix: Option<&str>, season: Option<&str>, trees: u32) -> GProject {
    let mut v = json!({"typology": "office", "floors": 3, "design": {"trees": trees, "paving": 3.0}});
    let mut rules = serde_json::Map::new();
    if let Some(m) = mix {
        rules.insert("site.tree_mix".into(), json!(m));
    }
    if let Some(se) = season {
        rules.insert("site.tree_season".into(), json!(se));
    }
    v["rules"] = serde_json::Value::Object(rules);
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    assert!(check_project(&s, &p).passed());
    p
}

fn trees(p: &GProject) -> Vec<(&'static str, f64)> {
    p.storeys[0].layout.fixtures.iter().filter(|f| f.family == "Tree").map(|f| (f.ty, f.rotation)).collect()
}

#[test]
fn trees_are_of_many_species_and_turned_differently() {
    let t = trees(&site(None, 12));
    assert!(t.len() >= 6, "{t:?}");
    // the street row is an avenue of one species; off it a species repeats only once the mix is
    // used up, so the first dozen are all different
    let mut count: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for x in &t {
        *count.entry(x.0).or_default() += 1;
    }
    let avenue = count.iter().max_by_key(|c| *c.1).map(|c| *c.0).unwrap();
    let rest: Vec<_> = t.iter().filter(|x| x.0 != avenue).take(12).map(|x| x.0).collect();
    let kinds: std::collections::BTreeSet<_> = rest.iter().collect();
    assert!(count[avenue] >= 3 && kinds.len() == rest.len() && rest.len() >= 5, "{t:?}");
    let turns: std::collections::BTreeSet<u64> = t.iter().map(|x| x.1.to_bits()).collect();
    assert_eq!(turns.len(), t.len());
    assert!(t.iter().all(|x| x.0 != "Large" && x.0 != "Small"));
}

#[test]
fn the_mix_rule_picks_the_groups() {
    let palm = |m: &str| trees(&site(Some(m), 16)).iter().any(|x| x.0.contains("Palm"));
    assert!(!palm("conifer") && !palm("mixed") && !palm("broadleaf"));
    let arid = trees(&site(Some("arid"), 16));
    let ok = ["Poplar", "Cypress", "Hornbeam", "Columnar Oak", "Date Palm", "Fan Palm", "Acacia", "Olive", "Stone Pine"];
    assert!(arid.iter().all(|x| ok.iter().any(|n| x.0.starts_with(n))), "{arid:?}");
    let conifer = trees(&site(Some("conifer"), 8));
    assert!(conifer.iter().all(|x| ["Spruce", "Fir", "Poplar", "Cypress", "Hornbeam", "Columnar Oak", "Scots Pine", "Larch"].iter().any(|n| x.0.starts_with(n))), "{conifer:?}");
}

#[test]
fn the_same_brief_plants_the_same_trees() {
    assert_eq!(trees(&site(None, 9)), trees(&site(None, 9)));
}

#[test]
fn the_season_rule_dresses_the_trees_and_keeps_the_evergreens() {
    let names = |se: &str| -> Vec<&'static str> { trees(&in_season(Some("mixed"), Some(se), 24)).into_iter().map(|x| x.0).collect() };
    let (summer, autumn, winter, spring) = (names("summer"), names("autumn"), names("winter"), names("spring"));
    let tagged = |v: &[&str], t: &str| v.iter().any(|n| n.ends_with(t));
    assert!(!tagged(&summer, " Autumn") && !tagged(&summer, " Bare") && !tagged(&summer, " Spring"), "{summer:?}");
    assert!(tagged(&autumn, " Autumn") && !tagged(&autumn, " Bare"), "{autumn:?}");
    assert!(tagged(&winter, " Bare") && !tagged(&winter, " Autumn"), "{winter:?}");
    assert!(tagged(&spring, " Spring"), "{spring:?}");
    // the same site has the same number of trees in every season
    assert!(summer.len() == autumn.len() && autumn.len() == winter.len() && winter.len() == spring.len(), "{summer:?}
{autumn:?}
{winter:?}
{spring:?}");
}

fn scales(rules: serde_json::Value) -> Vec<f64> {
    let mut v = json!({"typology": "office", "floors": 3, "design": {"trees": 14, "paving": 3.0}});
    v["rules"] = rules;
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    p.storeys[0].layout.fixtures.iter().filter(|f| f.family == "Tree").map(|f| f.scale).collect()
}

#[test]
fn trees_differ_in_size_within_the_jitter_and_the_seed_keeps_them() {
    let a = scales(json!({}));
    assert!(a.len() >= 6, "{a:?}");
    assert!(a.iter().all(|s| (0.8 - 1e-9..=1.2 + 1e-9).contains(s)), "{a:?}");
    assert!(a.iter().any(|s| *s < 0.95) && a.iter().any(|s| *s > 1.05), "{a:?}");
    assert_eq!(a, scales(json!({})), "the same site keeps its sizes");
    let wide = scales(json!({"site.tree_size_jitter": 0.4}));
    assert!(wide.iter().all(|s| (0.6 - 1e-9..=1.4 + 1e-9).contains(s)) && wide.iter().any(|s| *s > 1.2 || *s < 0.8), "{wide:?}");
    assert!(scales(json!({"site.tree_size_jitter": 0.0})).iter().all(|s| *s == 1.0));
}

fn planted(rules: serde_json::Value, trees: u32) -> Vec<ddc_gen::GFixture> {
    let mut v = json!({"typology": "office", "floors": 3, "design": {"trees": trees, "paving": 3.0}});
    v["rules"] = rules;
    let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
    let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
    p.storeys[0].layout.fixtures.iter().filter(|f| f.family == "Tree").cloned().collect()
}

#[test]
fn neighbouring_crowns_never_meet() {
    for rules in [json!({}), json!({"site.tree_size_jitter": 0.5, "site.tree_rows": "both", "site.row_spacing": 6.0})] {
        let t = planted(rules.clone(), 16);
        assert!(t.len() >= 6, "{}", t.len());
        for (i, a) in t.iter().enumerate() {
            for b in &t[i + 1..] {
                let d = ((a.at.0 - b.at.0).powi(2) + (a.at.1 - b.at.1).powi(2)).sqrt();
                let r = |f: &ddc_gen::GFixture| ddc_gen::project::tree_dims(f.ty).unwrap().1 * f.scale / 2.0;
                assert!(d + 1e-6 >= r(a) + r(b), "{} and {} {d:.2} m apart, crowns {:.2} + {:.2}", a.ty, b.ty, r(a), r(b));
            }
        }
    }
}

#[test]
fn the_trunk_under_the_crown_keeps_the_rule_minimum() {
    let t = planted(json!({}), 12);
    for f in &t {
        let (h, _, clear) = ddc_gen::project::tree_dims(f.ty).unwrap();
        let eff = (clear * f.scale).max(f.min_clear);
        assert!(f.min_clear <= 1.8 + 1e-9 && f.min_clear <= 0.4 * h * f.scale + 1e-9, "{} {}", f.ty, f.min_clear);
        assert!(eff >= 1.8f64.min(0.4 * h * f.scale) - 1e-9, "{}: clear {eff}", f.ty);
    }
    let low = planted(json!({"site.tree_min_clear": 0.0}), 12);
    assert!(low.iter().all(|f| f.min_clear == 0.0));
    let high = planted(json!({"site.tree_min_clear": 3.0}), 12);
    assert!(high.iter().all(|f| f.min_clear > 1.8 || f.min_clear >= 0.4 * ddc_gen::project::tree_dims(f.ty).unwrap().0 * f.scale - 1e-9));
}

/// No crown reaches the building: every crown keeps `site.tree_building_clear` from the walls
/// plus the roof overhang (the eaves), at its own scaled size.
#[test]
fn crowns_keep_clear_of_the_walls_and_the_eaves() {
    for (rules, clear) in [(json!({}), 1.0), (json!({"site.tree_building_clear": 3.0, "site.tree_size_jitter": 0.5}), 3.0)] {
        let mut v = json!({"typology": "house", "floors": 2, "design": {"trees": 12, "paving": 1.5}});
        v["rules"] = rules;
        let p = build_project(&parse_spec(&v).unwrap()).unwrap();
        let l = &p.storeys[0].layout;
        let seg = |q: (f64, f64), a: (f64, f64), b: (f64, f64)| {
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let t = (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
            ((q.0 - a.0 - t * dx).powi(2) + (q.1 - a.1 - t * dy).powi(2)).sqrt()
        };
        let trees: Vec<_> = l.fixtures.iter().filter(|f| f.family == "Tree").collect();
        assert!(!trees.is_empty());
        for f in trees {
            let d = (0..l.slab.len()).map(|i| seg(f.at, l.slab[i], l.slab[(i + 1) % l.slab.len()])).fold(f64::MAX, f64::min);
            let r = ddc_gen::project::tree_dims(f.ty).unwrap().1 * f.scale / 2.0;
            assert!(d + 1e-6 >= r + clear + 0.5, "{} at {:?}: crown {r:.2} m, {d:.2} m from the building", f.ty, f.at);
        }
    }
}

/// The ring trees stand in groups with open ground between: the distances to the nearest
/// neighbour spread, unlike a ruled row, and no species follows itself round the ring.
#[test]
fn trees_stand_in_groups_not_in_a_ruled_ring() {
    let t = planted(json!({"site.tree_rows": "none"}), 12);
    assert!(t.len() >= 8, "{}", t.len());
    let near: Vec<f64> = t
        .iter()
        .map(|a| t.iter().filter(|b| !std::ptr::eq(a, *b)).map(|b| (a.at.0 - b.at.0).hypot(a.at.1 - b.at.1)).fold(f64::MAX, f64::min))
        .collect();
    let mean = near.iter().sum::<f64>() / near.len() as f64;
    let sd = (near.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / near.len() as f64).sqrt();
    assert!(sd / mean > 0.1, "nearest neighbours {near:?}");
    // a tree left out where it did not fit may bring two of a species together, once at most
    let twins = t.windows(2).filter(|w| ddc_gen::project::species_of(w[0].ty) == ddc_gen::project::species_of(w[1].ty)).count();
    assert!(twins <= 1, "{:?}", t.iter().map(|f| f.ty).collect::<Vec<_>>());
}

/// A street row is planted of one species whose crown fits the spacing, so no row tree is
/// shrunk to fit; the season keeps the species.
#[test]
fn a_street_row_is_an_avenue_of_one_species_that_fits_its_spacing() {
    for season in ["summer", "winter"] {
        let t = planted(json!({"site.tree_size_jitter": 0.0, "site.tree_season": season}), 14);
        let mut rows: std::collections::BTreeMap<i64, Vec<&ddc_gen::GFixture>> = std::collections::BTreeMap::new();
        for f in &t {
            rows.entry((f.at.1 * 100.0).round() as i64).or_default().push(f);
        }
        let (_, row) = rows.iter().max_by_key(|r| r.1.len()).unwrap();
        assert!(row.len() >= 4, "{season}: {:?}", t.iter().map(|f| (f.ty, f.at)).collect::<Vec<_>>());
        let sp = ddc_gen::project::species_of(row[0].ty);
        assert!(row.iter().all(|f| ddc_gen::project::species_of(f.ty) == sp), "{season}: {:?}", row.iter().map(|f| f.ty).collect::<Vec<_>>());
        assert!(t.iter().all(|f| f.scale == 1.0), "{season}: {:?}", t.iter().map(|f| (f.ty, f.scale)).collect::<Vec<_>>());
    }
}
