//! Every country pack builds the small house and the apartment block of the bench with its own
//! numbers: the pack's norms reach the rule set, and the built project has no hard failure.

#[path = "../bench/suite.rs"]
mod suite;

use ddc_gen::project::rules::{norms_from, Layer};
use ddc_gen::project::{build_project, check_project, parse_spec};
use ddc_model::checks::norm_pack::PackSet;

const BRIEFS: [&str; 2] = ["house_2_storeys", "section_5"];

fn country_packs(set: &PackSet) -> Vec<String> {
    set.ids()
        .into_iter()
        .filter(|id| set.resolve(id).is_ok_and(|p| p.meta["country"].is_string()))
        .map(String::from)
        .collect()
}

#[test]
fn every_country_builds_a_house_and_a_block_under_its_own_pack() {
    let set = PackSet::embedded();
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    let ids = country_packs(&set);
    assert!(ids.len() >= 28, "{} country packs", ids.len());
    let dir = suite::crate_dir().join("data/bench");
    let mut bad = vec![];
    for id in &ids {
        let pack = set.resolve(id).unwrap();
        let norms = norms_from(|k| pack.dim_cite(k));
        for name in BRIEFS {
            let mut b = suite::read_brief(&dir.join(format!("{name}.json")));
            b.spec["norms"] = norms.clone();
            let s = match parse_spec(&b.spec) {
                Ok(s) => s,
                Err(e) => {
                    bad.push(format!("{id}/{name}: spec refused: {e}"));
                    continue;
                }
            };
            for rule in ["egress.max_travel", "light.glazing_floor", "fire.compartment_area"] {
                if s.rules.source(rule) != Some(Layer::Norm) && s.rules.unlimited(rule).is_none() {
                    bad.push(format!("{id}/{name}: {rule} is not set by the pack"));
                }
            }
            if let Some(r) = norms["core.max_riser"]["value"].as_f64() {
                if s.max_riser > r + 1e-9 {
                    bad.push(format!("{id}/{name}: riser {} over the pack's {r}", s.max_riser));
                }
            }
            let p = match build_project(&s) {
                Ok(p) => p,
                Err(e) => {
                    bad.push(format!("{id}/{name}: not built: {e}"));
                    continue;
                }
            };
            let g = check_project(&s, &p);
            if !g.failures.is_empty() {
                bad.push(format!("{id}/{name}: {:?}", g.failures.iter().take(3).collect::<Vec<_>>()));
            }
            let row = suite::measure(&b);
            if row.refused.is_some() || row.get("hard_failures") != Some(0.0) {
                bad.push(format!("{id}/{name}: bench row {:?} {:?}", row.refused, row.get("hard_failures")));
            }
        }
    }
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn every_country_riser_limit_reaches_the_rules_even_a_loose_one() {
    let set = PackSet::embedded();
    let dir = suite::crate_dir().join("data/bench");
    for (id, limit) in [("nl", 0.21), ("vn", 0.22), ("jp", 0.2), ("tr", 0.16)] {
        let norms = norms_from(|k| set.resolve(id).unwrap().dim_cite(k));
        let mut b = suite::read_brief(&dir.join("section_5.json"));
        b.spec["norms"] = norms;
        let s = parse_spec(&b.spec).unwrap();
        assert_eq!(s.rules.resolve("core.max_riser"), Some((limit, Layer::Norm)), "{id}");
        assert!(s.rules.cite("core.max_riser").unwrap().starts_with(&format!("{id} stair.riser_max")), "{id}");
        // a maximum: the stair keeps its comfortable riser when the country allows a steeper one
        assert!((s.max_riser - limit.min(0.18)).abs() < 1e-9, "{id}: {}", s.max_riser);
        let why = ddc_gen::project::why::explain_project(&s, &build_project(&s).unwrap());
        assert!(why.iter().any(|w| w.rule == "core.max_riser" && w.source.contains(id)), "{id}");
    }
}

#[test]
fn a_country_stair_width_raises_the_flights_and_the_check_holds_them_to_it() {
    let dir = suite::crate_dir().join("data/bench");
    let mut b = suite::read_brief(&dir.join("house_2_storeys.json"));
    let cite = "xx stair.width_min.house: Test code, art. 2";
    b.spec["norms"] = serde_json::json!({"core.stair_width_house": {"value": 1.1, "cite": cite}});
    let s = parse_spec(&b.spec).unwrap();
    assert!((s.stair_width - 1.1).abs() < 1e-9, "{}", s.stair_width);
    let p = build_project(&s).unwrap();
    assert!(p.storeys.iter().flat_map(|st| &st.layout.stairs).all(|x| (x.width - 1.1).abs() < 1e-9));
    assert!(!ddc_gen::project::nav::check(&s, &p).iter().any(|f| f.rule == "core.stair_width_house"));
    let why = ddc_gen::project::why::explain_project(&s, &p);
    assert!(why.iter().any(|w| w.rule == "core.stair_width_house" && w.source.contains(cite)), "{why:?}");
    // a narrower flight the brief asks for is its own choice, and the check names the clause
    b.spec["stair"] = serde_json::json!({"width": 0.95});
    let s = parse_spec(&b.spec).unwrap();
    assert!((s.stair_width - 0.95).abs() < 1e-9);
    let p = build_project(&s).unwrap();
    let f = ddc_gen::project::nav::check(&s, &p);
    let w = f.iter().find(|f| f.rule == "core.stair_width_house").expect("width finding");
    assert_eq!(w.cite.as_deref(), Some(cite));
    // the apartment block reads the general minimum, not the house one
    let mut b = suite::read_brief(&dir.join("section_5.json"));
    b.spec["norms"] = serde_json::json!({"core.stair_width": {"value": 1.4, "cite": "xx stair.width_min: Test code, art. 3"},
                                         "core.stair_width_house": {"value": 1.1, "cite": cite}});
    assert!((parse_spec(&b.spec).unwrap().stair_width - 1.4).abs() < 1e-9);
}

#[test]
fn a_stricter_country_riser_lowers_the_stair_and_a_brief_riser_wins() {
    let dir = suite::crate_dir().join("data/bench");
    let mut b = suite::read_brief(&dir.join("house_2_storeys.json"));
    b.spec["norms"] = serde_json::json!({"core.max_riser": {"value": 0.17, "cite": "xx stair.riser_max: Test code, art. 1"}});
    let s = parse_spec(&b.spec).unwrap();
    assert!((s.max_riser - 0.17).abs() < 1e-9, "{}", s.max_riser);
    let p = build_project(&s).unwrap();
    assert!(check_project(&s, &p).failures.is_empty());
    b.spec["stair"] = serde_json::json!({"max_riser": 0.19});
    assert!((parse_spec(&b.spec).unwrap().max_riser - 0.19).abs() < 1e-9);
}

#[test]
fn full_country_packs_set_the_riser_and_build_the_block_without_hard_failures() {
    let set = PackSet::embedded();
    let dir = suite::crate_dir().join("data/bench");
    // before these packs stated stair.riser_max the generator kept its own 0.18 m for them
    for (id, limit) in [("de", 0.19), ("us", 0.1778), ("uk", 0.17), ("fr", 0.17)] {
        let pack = set.resolve(id).unwrap();
        let norms = norms_from(|k| pack.dim_cite(k));
        let mut b = suite::read_brief(&dir.join("section_5.json"));
        b.spec["norms"] = norms;
        let s = parse_spec(&b.spec).unwrap();
        assert_eq!(s.rules.resolve("core.max_riser"), Some((limit, Layer::Norm)), "{id}");
        assert!((s.max_riser - limit.min(0.18)).abs() < 1e-9, "{id}: {}", s.max_riser);
        let p = build_project(&s).unwrap_or_else(|e| panic!("{id}: {e}"));
        let g = check_project(&s, &p);
        assert!(g.failures.is_empty(), "{id}: {:?}", g.failures);
        let risers: Vec<f64> = p.storeys.iter().flat_map(|st| &st.layout.stairs).map(|x| x.max_riser).collect();
        assert!(!risers.is_empty() && risers.iter().all(|r| *r <= limit + 1e-9), "{id}: {risers:?}");
    }
}
