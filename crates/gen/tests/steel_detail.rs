//! lattice_tower_300 in detail (and the guyed mast) through the generator: rules from the rule set, every decision
//! explained, the preliminary check reported, edits through rule overrides and an outline.

use ddc_gen::project::rules::{self, RuleSet};
use ddc_gen::project::why_text;
use ddc_gen::steel::{check_rules, hier_rules};
use ddc_gen::{registry, GenError, GenOut, Generator};
use ddc_geom::steel::hier::HierRules;
use ddc_geom::steel::hier_check::CheckRules;
use serde_json::{json, Value as J};

fn gen() -> &'static dyn Generator {
    *registry().iter().find(|g| g.id() == "steel_frame").unwrap()
}

fn metric(m: &[(&'static str, f64)], k: &str) -> f64 {
    m.iter().find(|x| x.0 == k).unwrap_or_else(|| panic!("no metric {k}")).1
}

fn build(p: J) -> (Vec<(&'static str, f64)>, Vec<ddc_gen::project::why::Why>) {
    match gen().build(&p, 1).unwrap() {
        GenOut::Steel { metrics, why, frame, .. } => {
            assert!(frame.validate().is_empty());
            (metrics, why)
        }
        _ => panic!("not steel"),
    }
}

#[test]
fn built_in_rules_are_the_lattice_defaults() {
    let rs = RuleSet::new();
    assert_eq!(hier_rules(&rs, 3), HierRules::default());
    assert_eq!(check_rules(&rs), CheckRules::default());
    for d in rules::defs().iter().filter(|d| d.group == "steel") {
        assert!(!d.why.is_empty(), "{} has no why", d.id);
    }
}

#[test]
fn detailed_tower_reports_levels_and_explains_every_choice() {
    let (m, why) = build(json!({"kind": "lattice_tower_300", "detail": 3}));
    assert_eq!(metric(&m, "lattice_levels"), 3.0);
    assert!(metric(&m, "pieces") >= 300_000.0, "{m:?}");
    assert!(metric(&m, "check_preliminary_max_use") > 0.0);
    let keys: Vec<&str> = why.iter().map(|w| w.key).collect();
    for k in ["steel_skeleton", "steel_latticed", "steel_pattern", "steel_chord_laced", "steel_setback", "steel_plate", "steel_check"] {
        assert!(keys.contains(&k), "no {k} in {keys:?}");
    }
    for w in &why {
        assert!(w.rule.starts_with("steel."), "{}", w.rule);
        assert!(rules::def(&w.rule).is_some(), "{}", w.rule);
        assert!(why_text::template(w.key).is_some(), "{}", w.key);
        assert!(!w.effect.contains('{'), "unfilled slot: {}", w.effect);
    }
    assert!(why.iter().any(|w| w.key == "steel_check" && w.effect.starts_with("preliminary check")));
    // level 1 keeps the light frame: no detail report
    let (m1, w1) = build(json!({"kind": "lattice_tower_300"}));
    assert!(w1.is_empty() && m1.iter().all(|x| x.0 != "pieces"));
    // its sections come from the wind and dead-load sizing: every band carried, sway a small
    // share of the height (preliminary, height / 200 at most)
    assert_eq!(metric(&m1, "over_capacity"), 0.0);
    let sway = metric(&m1, "sway_top_mm");
    assert!(sway > 10.0 && sway < 300_000.0 / 200.0, "sway {sway} mm");
}

#[test]
fn rule_overrides_and_outline_edit_the_tower() {
    let base = build(json!({"kind": "lattice_tower_300", "detail": 2})).0;
    let fewer = build(json!({"kind": "lattice_tower_300", "detail": 2, "rules": {"steel.lat.min_len": 9.0}})).0;
    assert!(metric(&fewer, "latticed_members") < metric(&base, "latticed_members"));
    let (_, why) = build(json!({"kind": "lattice_tower_300", "detail": 2, "rules": {"steel.lat.min_len": 9.0}}));
    assert!(why.iter().any(|w| w.rule == "steel.lat.min_len" && w.source == "brief" && w.value == json!(9.0)));
    let slim = build(json!({"kind": "lattice_tower_300", "detail": 2, "profile": [[0, 40], [57.6, 22], [115.7, 10], [300, 3]]})).0;
    assert_ne!(metric(&slim, "pieces"), metric(&base, "pieces"));
    let refused = |p: J| matches!(gen().build(&p, 1), Err(GenError::Param { .. }));
    assert!(refused(json!({"kind": "lattice_tower_300", "rules": {"steel.lat.min_len": 99.0}})));
    assert!(refused(json!({"kind": "lattice_tower_300", "rules": {"no.such": 1.0}})));
    assert!(refused(json!({"kind": "lattice_tower_300", "rules": {"corridor.width": 1.5}})));
    assert!(refused(json!({"kind": "lattice_tower_300", "profile": [[0, 4]]})));
    assert!(refused(json!({"kind": "lattice_tower_300", "detail": 4})));
    let flat = gen().build(&json!({"kind": "lattice_tower_300", "profile": [[0, 4], [0, 3]]}), 1);
    assert!(matches!(flat, Err(GenError::Infeasible(_))), "{flat:?}");
    // a slim outline keeps its tower: the lift to the top is left out, not the tower refused
    assert!(gen().build(&json!({"kind": "lattice_tower_300", "profile": [[0, 40], [57.6, 22], [115.7, 10], [300, 3]]}), 1).is_ok());
}

#[test]
fn detailed_tower_has_railings_stairs_cabin_and_piers_with_reasons() {
    let (m, why) = build(json!({"kind": "lattice_tower_300", "detail": 2}));
    for k in ["rail_posts", "stair_treads", "lift_guide_members", "cabin_members", "mast_members"] {
        assert!(metric(&m, k) > 0.0, "{k}");
    }
    assert_eq!(metric(&m, "masonry_piers"), 4.0);
    for k in ["steel_rails", "steel_stairs", "steel_lift", "steel_cabin", "steel_piers"] {
        assert!(why.iter().any(|w| w.key == k), "no {k}");
    }
    let (plain, _) = build(json!({"kind": "lattice_tower_300"}));
    assert!(plain.iter().all(|x| x.0 != "rail_posts"));
    let (tall, _) = build(json!({"kind": "lattice_tower_300", "detail": 2, "rules": {"steel.out.mast_height": 40.0}}));
    assert!(metric(&tall, "mast_members") > metric(&m, "mast_members"));
}

#[test]
fn a_guyed_mast_has_its_height_triangle_and_guys() {
    let f = match gen().build(&json!({"kind": "guyed_mast", "mast_height": 80, "side": 1.2, "guys": 3}), 0).unwrap() {
        GenOut::Steel { frame, .. } => frame,
        _ => panic!("not steel"),
    };
    assert!(f.validate().is_empty(), "{:?}", f.validate());
    let top = f.nodes.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!((top - 80.0).abs() < 1e-9, "{top}");
    let cables = f.members.iter().filter(|m| m.role == ddc_geom::steel::lattice::MemRole::Cable).count();
    assert_eq!(cables, 9, "three guys at each of three levels");
    let anchors = f.nodes.iter().filter(|p| p.z.abs() < 1e-9 && (p.x.hypot(p.y) - 50.0).abs() < 1e-6).count();
    assert_eq!(anchors, 3);
    assert!(matches!(gen().build(&json!({"kind": "guyed_mast", "guys": 9}), 0), Err(GenError::Param { .. }) | Err(GenError::Infeasible(_))));
}
