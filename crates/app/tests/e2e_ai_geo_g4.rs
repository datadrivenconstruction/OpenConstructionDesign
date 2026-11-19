//! Draft checks in the chat: the realism critic answers a geometry step with tasks, its
//! findings show on the plan card, and `measure` always says where its number comes from.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::Script;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

fn rect(w: f64, h: f64) -> J {
    json!({"shape": "rect", "w": {"v": w, "unit": "m"}, "h": {"v": h, "unit": "m"}})
}

fn extrude(w: f64, h: f64, z0: f64, z1: f64) -> J {
    json!({"kind": "extrude", "profile": rect(w, h),
           "frame": {"origin": {"x": 0, "y": 0, "z": 0, "unit": "m"},
                     "x": {"x": 1, "y": 0, "z": 0}, "z": {"x": 0, "y": 0, "z": 1}},
           "start": {"v": z0, "unit": "m"}, "end": {"v": z1, "unit": "m"}})
}

fn two_solids(level: i64) -> Script {
    let lv = json!({"by": "id", "cat": "level", "id": level});
    tool(
        "create_geometry",
        json!({"title": "Box and a sliver", "ops": [
            {"action": "solid", "level": lv, "form": extrude(2.0, 1.0, 0.0, 2.0)},
            {"action": "solid", "level": lv, "form": extrude(2.0, 0.003, 3.0, 4.0)}
        ]}),
    )
}

#[test]
fn the_critic_answers_a_step_and_its_findings_show_on_the_card() {
    let mut ed = Editor::new_in_memory().unwrap();
    let l = add_level(&mut ed, "L1", 0.0);
    let (s, provider) = run(&mut ed, vec![two_solids(l)]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{results:?}");
    assert!(results[0].0.contains("critic cycle 1/3"), "{}", results[0].0);
    assert!(results[0].0.contains("realism"), "{}", results[0].0);
    let plan = plan_of(&s);
    assert!(
        plan.issues.iter().any(|i| i.text_key == "ai.realism.tiny_part"),
        "{:?}",
        plan.issues
    );
}

#[test]
fn every_measured_number_names_its_source() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ask = |kind: &str| {
        tool("measure", json!({"kind": kind, "a": "solid:n1", "b": "solid:n2"}))
    };
    let l = add_level(&mut ed, "L1", 0.0);
    let steps = vec![two_solids(l), ask("clearance"), ask("distance"), ask("volume"), ask("height")];
    let (_s, provider) = run(&mut ed, steps);
    let results = tool_results(&provider);
    for (text, err) in &results[1..] {
        assert!(!err, "{text}");
        let j = parsed(text);
        assert!(["exact", "bbox"].contains(&j["source"].as_str().unwrap_or("")), "{text}");
        assert!(j["value"].is_number(), "{text}");
    }
}
