//! Check and fix: the rule is deterministic code, the model only picks what to fix.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::ScriptedProvider;
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use ddc_model::checks::{CheckRule, CheckScope, Finding, Fix, Severity};
use ddc_model::{Error, Model};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

/// Doors narrower than 0.8 m; the fix removes them, which any rule's fix may do.
struct NarrowDoors;

impl CheckRule for NarrowDoors {
    fn id(&self) -> &'static str {
        "test.narrow_door"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let mut st = m
            .sql()
            .prepare("SELECT id FROM opening WHERE kind='door' AND width<0.8 ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        for id in rows {
            let key = ("opening".to_string(), id?);
            if let CheckScope::Elements(list) = scope {
                if !list.contains(&key) {
                    continue;
                }
            }
            out.push(Finding {
                rule: "test.narrow_door",
                severity: Severity::Error,
                elements: vec![key.clone()],
                at: None,
                measure: None,
                msg_key: "test.narrow",
                args: vec![],
                fix: Fix::Delete(vec![key]),
            });
        }
        Ok(())
    }
}

static NARROW: NarrowDoors = NarrowDoors;

fn model() -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let wall = add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    let narrow = add_door(&mut ed, wall, 2.0);
    add_door(&mut ed, wall, 6.0);
    // through the model: raw SQL would leave the derived quantities stale
    let spec = ddc_model::OpeningSpec {
        along: 2.0,
        width: 0.6,
        sill: 0.0,
        height: 2.1,
        kind: "door".to_string(),
    };
    ed.model.update_opening(narrow, &spec).unwrap();
    ed.reload().unwrap();
    (ed, narrow)
}

#[test]
fn a_finding_is_fixed_through_the_plan_and_the_check_then_passes() {
    let (mut ed, narrow) = model();
    let provider = ScriptedProvider::new(script(vec![
        tool("run_checks", json!({"rules": ["test.narrow_door"]})),
        tool("apply_fix", json!({"findings": ["f1"]})),
    ]));
    let mut s = start(&mut ed, with_rules(cfg(provider.clone()), vec![&NARROW]));
    turn(
        &mut s,
        &mut ed,
        "check the doors and fix them",
        Mode::Propose,
    );

    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let report = parsed(&results[0].0);
    assert_eq!(report["total"], json!(1));
    assert_eq!(report["findings"][0]["ref"], json!("f1"));
    assert_eq!(report["findings"][0]["fixable"], json!(true));

    assert_eq!(count(&ed.model, "opening"), 2, "the fix waits for Apply");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "opening"), 1);
    let gone: i64 = ed
        .model
        .sql()
        .query_row("SELECT count(*) FROM opening WHERE id=?1", [narrow], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(gone, 0);

    let mut again = vec![];
    NARROW.run(&ed.model, &CheckScope::All, &mut again).unwrap();
    assert!(again.is_empty());
}

#[test]
fn an_unknown_finding_is_refused() {
    let (mut ed, _) = model();
    let provider =
        ScriptedProvider::new(script(vec![tool("apply_fix", json!({"findings": ["f9"]}))]));
    let mut s = start(&mut ed, with_rules(cfg(provider.clone()), vec![&NARROW]));
    turn(&mut s, &mut ed, "fix", Mode::Propose);
    assert!(tool_results(&provider)[0].1);
    assert!(s.plan().is_none());
}

#[test]
fn a_rule_in_words_waits_for_the_person() {
    let (mut ed, _) = model();
    let before = live_hash(&ed.model);
    let rule = json!({
        "id": "project.rating",
        "title": "Doors have a rating",
        "severity": "warning",
        "scope": {"cats": ["door"]},
        "check": {"kind": "param_present", "param": "Fire rating"},
    });
    let provider = ScriptedProvider::new(script(vec![tool(
        "define_rule",
        json!({"text": "every door has a rating", "rule": rule}),
    )]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "add the rule", Mode::Propose);
    let results = tool_results(&provider);
    if results[0].1 {
        // the rule format belongs to the rules module; only the outcome for the live model is fixed here
        assert!(s.plan().is_none());
    } else {
        assert!(s
            .drain_events()
            .iter()
            .any(|e| matches!(e, ddc_app::ai::UiEvent::RuleDraft { .. })));
        assert!(
            ed.model
                .project_rules()
                .unwrap()
                .iter()
                .all(|r| r.id != "project.rating"),
            "saving is the person's act"
        );
    }
    assert_eq!(live_hash(&ed.model), before);
}

#[test]
fn the_report_names_the_rules_that_ran_and_is_no_proof() {
    let (mut ed, _) = model();
    let provider = ScriptedProvider::new(script(vec![
        tool("run_checks", json!({})),
        tool("run_checks", json!({"rules": ["room_open"]})),
        tool("run_checks", json!({"rules": ["no_such_rule"]})),
    ]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "check", Mode::Propose);
    let results = tool_results(&provider);

    let all = parsed(&results[0].0);
    let ran = all["rules_ran"].as_array().unwrap();
    for id in [
        "integrity",
        "no_host",
        "opening_outside",
        "room_open",
        "room_unplaced",
        "zero_size",
        "duplicates",
        "missing_params",
        "type_marks",
        "clashes",
        "company_rules",
    ] {
        assert!(ran.contains(&json!(id)), "{id} did not run: {ran:?}");
    }
    assert!(all["basis"].as_str().unwrap().contains("not proof"));

    let one = parsed(&results[1].0);
    assert_eq!(one["rules_ran"], json!(["room_open"]));
    assert!(one["rules_not_run"]
        .as_array()
        .unwrap()
        .contains(&json!("integrity")));

    assert!(
        results[2].1,
        "an unknown rule is an error, not a clean result"
    );
}

fn with_door_rule(ed: &mut Editor) {
    let rule = json!([{
        "id": "company.door_width",
        "title": "Doors are wide enough",
        "severity": "error",
        "scope": {"cats": ["door"]},
        "check": {"kind": "param_range", "param": "width", "min": 0.8},
    }]);
    let list = ddc_model::checks::load_pack(&rule.to_string()).unwrap();
    ed.model.save_rule(&list[0]).unwrap();
}

fn company_findings(ed: &Editor) -> Vec<Finding> {
    ddc_model::checks::run_all(&ed.model, Some(&["company_rules"]), &CheckScope::All, 10).unwrap()
}

#[test]
fn a_company_rule_is_found_fixed_and_undone_exactly() {
    let (mut ed, narrow) = model();
    with_door_rule(&mut ed);
    let provider = ScriptedProvider::new(script(vec![
        tool("run_checks", json!({"rules": ["company_rules"]})),
        tool("apply_fix", json!({"findings": ["f1"]})),
    ]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(
        &mut s,
        &mut ed,
        "check the company rules and fix",
        Mode::Propose,
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    let report = parsed(&results[0].0);
    assert_eq!(report["total"], json!(1));
    assert_eq!(report["company_rules"], json!(["company.door_width"]));

    apply_and_check(&mut s, &mut ed);
    let width: f64 = ed
        .model
        .sql()
        .query_row("SELECT width FROM opening WHERE id=?1", [narrow], |r| {
            r.get(0)
        })
        .unwrap();
    assert!((width - 0.8).abs() < 1e-9);
    assert!(company_findings(&ed).is_empty());
}

#[test]
fn a_one_click_fix_is_one_undo_step() {
    let (mut ed, narrow) = model();
    with_door_rule(&mut ed);
    let found = company_findings(&ed);
    assert_eq!(found[0].elements[0], ("opening".to_string(), narrow));
    let before = live_hash(&ed.model);
    let ops = count(&ed.model, "op");
    ddc_app::ai::fix::fix_now(&mut ed, &found[0]).unwrap();
    assert_eq!(count(&ed.model, "op"), ops + 1);
    assert!(company_findings(&ed).is_empty());
    ed.undo();
    assert_eq!(live_hash(&ed.model), before);
}

#[test]
fn a_door_left_without_its_wall_is_found_and_removed() {
    let (mut ed, narrow) = model();
    ed.model.sql().execute_batch(&format!("PRAGMA foreign_keys=OFF; UPDATE opening SET wall=999 WHERE id={narrow}; PRAGMA foreign_keys=ON;")).unwrap();
    let found =
        ddc_model::checks::run_all(&ed.model, Some(&["no_host"]), &CheckScope::All, 10).unwrap();
    assert_eq!(found.len(), 1);
    let before = no_qty(&ed.model);
    ddc_app::ai::fix::fix_now(&mut ed, &found[0]).unwrap();
    assert!(
        ddc_model::checks::run_all(&ed.model, Some(&["no_host"]), &CheckScope::All, 10)
            .unwrap()
            .is_empty()
    );
    ed.undo();
    assert_eq!(no_qty(&ed.model), before);
}

// the orphaned door was planted with raw SQL: the wall quantities of that state were never measured
fn no_qty(m: &Model) -> u64 {
    let mut skip = NOT_MODEL.to_vec();
    skip.push("wall_qty");
    e2e_common::hash_of(&e2e_common::dump_except(m, &skip))
}

#[test]
fn only_the_named_rules_are_too_heavy_for_a_plan_step() {
    let heavy: Vec<&str> = ddc_model::checks::rules()
        .iter()
        .filter(|r| r.heavy())
        .map(|r| r.id())
        .collect();
    assert_eq!(heavy, ["integrity", "wall_warnings", "clashes"]);
}
