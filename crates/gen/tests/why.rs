//! Explanations name the rule, value and layer behind each decision; the critic and the brief
//! check answer with rule changes inside the rules' ranges.

use ddc_gen::project::brief::check;
use ddc_gen::project::critique::{parse_reply, Critic, RuleCritic};
use ddc_gen::project::rules::{self, Layer, RuleSet};
use ddc_gen::project::why::{explain, explain_project, fixes_of, spec_patch, RuleFix};
use ddc_gen::project::{build_project, parse_spec, GProject, ProjectSpec};
use serde_json::{json, Value as J};

fn built(v: &J) -> (ProjectSpec, GProject) {
    let s = parse_spec(v).unwrap();
    let p = build_project(&s).unwrap();
    (s, p)
}

#[test]
fn the_explanation_names_rule_value_and_layer() {
    let (s, p) = built(&json!({"typology": "office", "floors": 3,
                               "rules": {"facade.glazing_share": 0.2}}));
    let all = explain_project(&s, &p);
    let band = all.iter().find(|w| w.rule == "depth.band").expect("depth.band");
    assert_eq!(band.target, "massing");
    assert_eq!(band.source, "typology");
    assert!(band.effect.contains("footprint depth"), "{}", band.effect);
    // the width has its reason too, among the first ten a reply shows
    let at = all.iter().position(|w| w.rule == "massing.width").expect("massing.width");
    assert!(at < 10 && all[at].effect.contains("rooms and the core"), "{:?}", all[at]);
    let glaze = all.iter().find(|w| w.rule == "facade.glazing_share").unwrap();
    assert_eq!((glaze.value.clone(), glaze.source.as_str()), (json!(0.2), "brief"));
    assert_eq!(all.iter().filter(|w| w.target.starts_with("storey:")).count(), 3);
    for w in &all {
        assert!(rules::def(&w.rule).is_some(), "{}", w.rule);
    }
    let core = explain(&s, &p, "core");
    assert!(!core.is_empty() && core.iter().all(|w| w.target == "core"));
    assert_eq!(explain(&s, &p, "grid.span").len(), 1);
}

#[test]
fn flag_and_choice_rules_read_and_write_the_brief() {
    let form = rules::def("massing.form").unwrap();
    assert_eq!(form.parse_value(&json!("courtyard")), Some(3.0));
    assert_eq!(form.parse_value(&json!("triangle")), None);
    assert_eq!(form.value_json(1.0), json!("l"));
    let canopy = rules::def("site.canopy").unwrap();
    assert_eq!((canopy.parse_value(&json!(true)), canopy.value_json(0.0)), (Some(1.0), json!(false)));
    let mut rs = RuleSet::new();
    assert!(rs.set_json(Layer::Brief, "facade.system", &json!("glass")).is_err());
    rs.set_json(Layer::Brief, "facade.system", &json!("curtain")).unwrap();
    assert_eq!(rs.value_json("facade.system"), Some(json!("curtain")));
    let back = RuleSet::from_json(&rs.to_json()).unwrap();
    assert_eq!(back.value_json("facade.system"), Some(json!("curtain")));
    let s = parse_spec(&json!({"typology": "office", "floors": 3,
                               "rules": {"massing.form": "l", "site.canopy": true}})).unwrap();
    assert_eq!(s.design.massing.name(), "l");
    assert!(s.design.canopy);
}

#[test]
fn rule_fixes_check_ranges_and_map_to_brief_fields() {
    assert!(RuleFix::new("site.trees", json!(99), "x").is_err());
    assert!(RuleFix::new("no.such", json!(1), "x").is_err());
    let f = vec![
        RuleFix::new("facade.system", json!("fins"), "a").unwrap(),
        RuleFix::new("facade.glazing_share", json!(0.2), "b").unwrap(),
    ];
    let p = spec_patch(&f);
    assert_eq!(p, json!({"design": {"facade": "fins"}, "rules": {"facade.glazing_share": 0.2}}));
    let mut back: Vec<String> = fixes_of(&p, "b").into_iter().map(|x| x.rule).collect();
    back.sort();
    assert_eq!(back, vec!["facade.glazing_share", "facade.system"]);
}

#[test]
fn the_critic_answers_with_rules() {
    let spec = json!({"typology": "office", "floors": 3});
    let c = RuleCritic.critique(&spec, &json!({}), &[]).unwrap();
    let ids: Vec<&str> = c.rules.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(ids, vec!["facade.system", "site.canopy", "site.trees"]);
    assert!(c.rules.iter().all(|f| !f.why.is_empty()));
    assert_eq!(c.patch, json!({"design": {"facade": "fins", "canopy": true, "trees": 6}}));
    let r = parse_reply(r#"{"scores":{"site":4},"rules":{"site.trees":99,"facade.system":"curtain"}}"#).unwrap();
    assert_eq!(r.patch, json!({"design": {"facade": "curtain"}}));
    assert!(r.notes.contains("refused site.trees"), "{}", r.notes);
}

/// What the person chose, as a design field or a rule, the critic leaves alone; the rounds
/// come from a rule, 3 unless the brief says otherwise.
#[test]
fn the_critic_keeps_the_persons_choices() {
    use ddc_gen::project::critique::LoopCfg;
    let spec = json!({"typology": "office", "floors": 3, "design": {"facade": "punched"},
                      "rules": {"site.canopy": false, "layout.refine_rounds": 0}});
    let c = RuleCritic.critique(&spec, &json!({}), &[]).unwrap();
    let ids: Vec<&str> = c.rules.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(ids, vec!["site.trees"]);
    assert_eq!(LoopCfg::of(&spec).unwrap().max_rounds, 0);
    assert_eq!(LoopCfg::of(&json!({"typology": "office"})).unwrap().max_rounds, 3);
    assert!(LoopCfg::of(&json!({"typology": "office", "rules": {"layout.refine_rounds": 9}})).is_err());
}

#[test]
fn the_brief_check_repair_names_its_rules() {
    let (s, p) = built(&json!({"typology": "office", "floors": 4, "features": ["atrium"]}));
    let c = check(&s, &p);
    let mut got: Vec<(String, J)> = c.rules.iter().map(|f| (f.rule.clone(), f.value.clone())).collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, vec![("massing.atrium".into(), json!(true)), ("massing.form".into(), json!("courtyard"))]);
    assert!(c.rules.iter().all(|f| f.why == "atrium"));
    assert_eq!(c.report["rule_repair"].as_array().map(Vec::len), Some(2), "{}", c.report);
}
