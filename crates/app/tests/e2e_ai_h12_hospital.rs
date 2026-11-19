//! H12, the German hospital wing with full ventilation, through the chat on the brief's own
//! words: the typology word "Krankenhaus" (an alias of the hospital wing), the brief's feature
//! words ("Lüftung", "Brandschutzklappen") and services on, built by `generate_project`; Apply
//! once, then every oracle of the case on the applied model.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::ai::eval::case::{evals_dir, Case};
use ddc_app::ai::eval::facts::Facts;
use ddc_app::ai::eval::oracle::{evaluate, in_range, EvalCx};
use ddc_app::ai::receipt::WriteReceipt;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const ANSWER: &str = "Krankenhausflügel mit vier Geschossen: Zentralgeräte (AHU) im Technikraum auf dem Dach, \
Zuluft-, Abluft- und Fortluftkanäle nach Volumenstrom dimensioniert (DN 200 bis 800 mm, EN 16798 und DIN 1946-4). \
Fire damper und smoke damper sitzen an den Brandwänden; OP und Isolierzimmer haben Überdruck beziehungsweise \
Unterdruck gegenüber den Fluren, jeder Raum hat einen Zuluftdurchlass.";

#[test]
fn h12_german_hospital_through_the_tools_meets_every_oracle() {
    let case = Case::load(&evals_dir().join("cases").join("H12_de_hospital_hvac.toml")).unwrap();
    // the brief's words, not the English typology name: the alias and the feature words decide
    let brief = &case.turns[0].say;
    assert!(brief.contains("Krankenhausflügel") && brief.contains("Lüftung"), "{brief}");
    let spec = json!({"typology": "Krankenhaus", "floors": 4, "services": true, "lang": "de",
                      "features": ["Lüftung", "Brandschutzklappen", "Isolierzimmer"]});
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"title": "Krankenhausflügel", "spec": spec}))]);
    let res = tool_results(&provider);
    assert!(res.len() == 1 && !res[0].1, "{res:?}");
    assert!(res[0].0.contains("hospital_wing"), "the word Krankenhaus finds the hospital wing: {}", res[0].0);

    let plan = plan_of(&s);
    let issues: Vec<_> = plan.issues.iter().chain(&plan.main_issues).cloned().collect();
    let seq = apply_and_check(&mut s, &mut ed);
    let receipts = vec![WriteReceipt::build(&plan.diff, &plan.qty, &issues, seq as u64, true, &case.id)];
    drop(s);

    let facts = Facts::read(&ed.model).unwrap();
    let mut short = vec![];
    for c in &case.oracle.count {
        match facts.count(&c.spec) {
            Ok(v) if in_range(v as f64, c.min, c.max) => {}
            Ok(v) => short.push(format!("{}: {v} (min {:?}, max {:?})", c.spec, c.min, c.max)),
            Err(e) => short.push(format!("{}: {e}", c.spec)),
        }
    }
    assert!(short.is_empty(), "count oracles: {short:#?}");
    let ifc = ddc_io::ifc::export_ifc(&ed.model).ok();
    let cx = EvalCx { ed: &ed, facts: &facts, before: None, answer: ANSWER, ifc: ifc.as_deref(), errors_before: 0, receipts: &receipts };
    let all = evaluate(&case, &cx);
    let failed: Vec<_> = all.iter().filter(|r| !r.pass).collect();
    assert!(!all.is_empty() && failed.is_empty(), "{failed:#?}");
}
