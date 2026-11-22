// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Variants: seed-deterministic, the best never worse than one plain run, chosen far apart,
//! and the rules the brief or the user fixed are left alone.

use ddc_gen::project::score::score;
use ddc_gen::project::variants::{axes, compare, pair_min, variants, VaryCfg};
use ddc_gen::project::{build_project, parse_spec};
use serde_json::json;
use std::cmp::Ordering;

const BRIEFS: [&str; 4] = ["office", "hotel", "residential", "clinic"];

#[test]
fn the_same_seed_gives_the_same_variants() {
    let b = json!({"typology": "office", "floors": 2});
    let cfg = VaryCfg { seed: 7, ..VaryCfg::default() };
    let (x, y) = (variants(&b, &cfg).unwrap(), variants(&b, &cfg).unwrap());
    let sig = |v: &ddc_gen::project::variants::Variants| -> Vec<(u64, String)> {
        v.chosen.iter().map(|c| (c.fingerprint, format!("{:?}", c.set))).collect()
    };
    assert_eq!(sig(&x), sig(&y));
    assert_eq!(x.tried, y.tried);
}

#[test]
fn the_best_variant_is_never_worse_than_a_single_run() {
    for t in BRIEFS {
        let b = json!({"typology": t, "floors": 2});
        let s = parse_spec(&b).unwrap();
        let single = score(&s, &build_project(&s).unwrap());
        let v = variants(&b, &VaryCfg::default()).unwrap();
        let best = &v.chosen[0];
        assert_ne!(best.score.compare(&single), Ordering::Greater, "{t}: {:?}", best.set);
    }
}

#[test]
fn variants_are_far_apart() {
    for t in BRIEFS {
        let b = json!({"typology": t, "floors": 2});
        let cfg = VaryCfg::default();
        let v = variants(&b, &cfg).unwrap();
        assert_eq!(v.chosen.len(), 3, "{t}: tried {} refused {:?}", v.tried, v.failed);
        let d = pair_min(&v.axes, &v.chosen).unwrap();
        assert!(d + 1e-9 >= cfg.min_distance, "{t}: {d}");
        let mut prints: Vec<u64> = v.chosen.iter().map(|c| c.fingerprint).collect();
        prints.dedup();
        assert_eq!(prints.len(), 3, "{t}");
        for w in v.chosen.windows(2) {
            assert_ne!(w[0].score.compare(&w[1].score), Ordering::Greater, "{t}: best first");
        }
    }
}

#[test]
fn fixed_rules_stay_as_the_brief_and_the_user_set_them() {
    let b = json!({"typology": "office", "floors": 2, "design": {"massing": "l"}, "rules": {"grid.span": 6.0}});
    let s = parse_spec(&b).unwrap();
    let ids: Vec<&str> = axes(&b, &s, &["facade.system".to_string()]).iter().map(|a| a.rule).collect();
    assert_eq!(ids, vec!["depth.band"]);
    let cfg = VaryCfg { fixed: vec!["facade.system".into()], ..VaryCfg::default() };
    let v = variants(&b, &cfg).unwrap();
    for c in &v.chosen {
        assert_eq!(c.spec.design.massing.name(), "l");
        assert_eq!(c.spec.grid, 6.0);
        assert_eq!(c.spec.design.facade, s.design.facade);
    }
}

#[test]
fn the_comparison_says_what_differs_and_why_it_ranks_lower() {
    let v = variants(&json!({"typology": "office", "floors": 2}), &VaryCfg::default()).unwrap();
    let j = compare(&v);
    let rows = j["variants"].as_array().unwrap();
    assert_eq!(rows.len(), v.chosen.len());
    assert!(rows[0]["changes_vs_best"].as_object().unwrap().is_empty());
    for r in &rows[1..] {
        assert!(!r["changes_vs_best"].as_object().unwrap().is_empty(), "{r}");
        assert!(r["why_not_best"].is_array() && r["score"]["strong"].is_number(), "{r}");
    }
}
