//! Goals with tiers and weights: the penalty splits over rule ids, a weight scales its goal,
//! and a stronger tier dominates whatever the weaker ones add up to.

use ddc_gen::project::rules::{self, Layer, RuleSet, Tier};
use ddc_gen::project::score::{goal_ids, score, Cost, Score};
use ddc_gen::project::why::explain;
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};
use std::cmp::Ordering;

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap();
    let p = build_project(&s).unwrap();
    (s, p)
}

fn cost(rule: &str, tier: Tier, c: f64) -> Cost {
    Cost { rule: rule.into(), tier, weight: 1.0, target: 0.0, measured: 0.0, miss: c.min(1.0), cost: c, failed: 0, of: 0 }
}

const BRIEFS: [&str; 6] = ["office", "hotel", "clinic", "residential", "school_campus", "mixed_use"];

#[test]
fn every_goal_has_a_tier_and_the_scorer_measures_it() {
    let goals: Vec<&str> = rules::defs().iter().filter(|d| d.group == "goal").map(|d| d.id.as_str()).collect();
    assert!(goals.len() >= 12, "{goals:?}");
    assert_eq!(goals, goal_ids());
    for d in rules::defs() {
        assert_eq!(d.tier.is_some(), d.group == "goal", "{}", d.id);
    }
}

#[test]
fn the_breakdown_sums_to_the_penalty() {
    for t in BRIEFS {
        let (s, p) = built(&json!({"typology": t, "floors": 3}));
        let sc = score(&s, &p);
        assert_eq!(sc.costs.len(), goal_ids().len(), "{t}");
        let by_rule: f64 = sc.costs.iter().map(|c| c.cost).sum();
        let by_tier: f64 = sc.tiers().iter().sum();
        assert!((by_rule - sc.total()).abs() < 1e-9 && (by_tier - sc.total()).abs() < 1e-9, "{t}");
        for c in &sc.costs {
            assert!((0.0..=1.0).contains(&c.miss), "{t} {}", c.rule);
            assert!((c.cost - c.weight * c.miss).abs() < 1e-12, "{t} {}", c.rule);
        }
        assert_eq!(score(&s, &p), sc, "{t}: deterministic");
    }
}

#[test]
fn a_heavier_weight_costs_more() {
    // a corridor share of 5 % cannot be met, so the goal always costs something
    let base = json!({"typology": "office", "floors": 3, "rules": {"goal.circulation": 0.05}});
    let (s, p) = built(&base);
    let one = score(&s, &p).cost_of("goal.circulation");
    assert!(one > 0.0);
    let mut heavy = s.clone();
    heavy.rules.set_goal(Layer::Project, "goal.circulation", Some(4.0), None).unwrap();
    let four = score(&heavy, &p).cost_of("goal.circulation");
    assert!((four - 2.0 * one).abs() < 1e-9, "{one} -> {four}");
    heavy.rules.set_goal(Layer::Brief, "goal.circulation", Some(0.0), None).unwrap();
    assert_eq!(score(&heavy, &p).cost_of("goal.circulation"), 0.0);
}

#[test]
fn weak_goals_never_outweigh_a_stronger_one() {
    let strong = Score { costs: vec![cost("goal.daylight", Tier::Strong, 0.01)] };
    let weak: Vec<Cost> = (0..50).map(|_| cost("goal.compact", Tier::Weak, 100.0)).collect();
    let weak = Score { costs: weak };
    assert_eq!(weak.compare(&strong), Ordering::Less);
    assert!(weak.total() > strong.total());
    let hard = Score { costs: vec![cost("goal.stair_reach", Tier::Hard, 0.001)] };
    assert_eq!(strong.compare(&hard), Ordering::Less);
    assert_eq!(strong.compare(&strong.clone()), Ordering::Equal);
}

#[test]
fn tiers_layer_like_values() {
    let mut rs = RuleSet::new();
    assert_eq!(rs.tier("goal.daylight"), Some(Tier::Strong));
    assert_eq!(rs.tier("grid.span"), None);
    rs.apply_patch(Layer::Brief, &json!({"goal.daylight": {"tier": "weak", "weight": 5}})).unwrap();
    assert_eq!((rs.tier("goal.daylight"), rs.weight("goal.daylight")), (Some(Tier::Weak), 5.0));
    assert!(rs.apply_patch(Layer::Brief, &json!({"goal.daylight": {"tier": "firm"}})).is_err());
    rs.apply_patch(Layer::Brief, &json!({"goal.compact": {"hard": true}})).unwrap();
    assert!(rs.hard("goal.compact"));
    let back = RuleSet::from_json(&rs.to_json()).unwrap();
    assert_eq!(back, rs);
}

#[test]
fn a_tightened_goal_shows_in_the_score_and_its_explanation() {
    let (s, p) = built(&json!({"typology": "office", "floors": 3, "rules": {"goal.circulation": 0.05}}));
    let sc = score(&s, &p);
    let c = sc.limiting().into_iter().find(|c| c.rule == "goal.circulation").expect("limiting");
    assert_eq!(c.tier, Tier::Medium);
    let w = explain(&s, &p, "score");
    assert!(w.iter().any(|w| w.rule == "goal.circulation" && w.source == "brief"), "{w:?}");
    let j = sc.to_json();
    assert!(j["tiers"]["medium"].as_f64().unwrap() > 0.0, "{j}");
    let (s2, p2) = built(&json!({"typology": "office", "floors": 3}));
    let base = score(&s2, &p2);
    let diff = base.contrast(&sc);
    assert!(diff.iter().any(|d| d.0 == "goal.circulation" && d.2 > 0.0), "{diff:?}");
}
