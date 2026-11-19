//! The chat defines a family and creates geometry without MCP: a rural toilet with its
//! parameters, three placed instances and a flex check; everything waits in the draft.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::Script;
use ddc_app::ai::geo_tools::promote_family;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};

fn toilet() -> J {
    json!({
        "name": "RuralToilet", "category": "generic", "host": "none",
        "params": [
            {"name": "Length", "type": "length", "scope": "type", "unit": "mm",
             "default": 1200, "min": 900, "max": 3000},
            {"name": "Depth", "type": "length", "scope": "type", "unit": "mm", "default": 1200},
            {"name": "Height", "type": "length", "scope": "type", "unit": "mm", "default": 2200},
            {"name": "Overhang", "type": "length", "scope": "type", "unit": "mm",
             "default": 150, "min": 0, "max": 400},
            {"name": "Wall", "type": "length", "scope": "type", "unit": "mm",
             "default": 100, "min": 50, "max": 200},
            {"name": "HoleD", "type": "length", "scope": "type", "unit": "mm",
             "default": 300, "min": 150, "max": 400},
            {"name": "DoorW", "type": "length", "scope": "type", "unit": "mm",
             "default": 700, "min": 500, "max": 900}
        ],
        "forms": [
            {"name": "Shell", "extrude": {"rect": ["0", "0", "Length", "Depth"]},
             "z": ["0", "Height"]},
            {"name": "Cavity", "void": true,
             "extrude": {"rect": ["Wall", "Wall", "Length - Wall", "Depth - Wall"]},
             "z": ["Wall", "Height"]},
            {"name": "SeatHole", "void": true,
             "extrude": {"circle": ["Length / 2", "Depth / 2", "HoleD / 2"]}, "z": ["0", "Wall"]},
            {"name": "Roof",
             "extrude": {"rect": ["-Overhang", "-Overhang", "Length + Overhang", "Depth + Overhang"]},
             "z": ["Height", "Height + 100mm"]},
            {"name": "DoorLeaf",
             "extrude": {"rect": ["Length / 2 - DoorW / 2", "-20mm", "Length / 2 + DoorW / 2", "0"]},
             "z": ["100mm", "2000mm"]}
        ],
        "tests": [{"Length": 900}, {"Length": 3000, "Overhang": 400}, {"HoleD": 150, "Wall": 200}]
    })
}

fn define(def: J) -> Script {
    tool("define_family", json!({"title": "Rural toilet", "family": def}))
}

fn placed() -> Vec<Script> {
    vec![
        define(toilet()),
        tool(
            "create_type",
            json!({"title": "Type", "cat": "comp", "name": "Std",
                   "fields": {"family": "RuralToilet"}}),
        ),
        tool(
            "create_elements",
            json!({"title": "Three toilets", "cat": "comp", "items": [
                {"type": "comp_type:n1", "at": [0, 0, 0]},
                {"type": "comp_type:n1", "at": [4, 0, 0]},
                {"type": "comp_type:n1", "at": [8, 0, 0], "rotation": 1.5707963267948966}
            ]}),
        ),
    ]
}

fn draft_ids(ed: &Editor) -> Vec<(i64, bool)> {
    ed.model
        .families()
        .unwrap()
        .into_iter()
        .map(|f| (f.0, ed.model.family_is_draft(f.0).unwrap()))
        .collect()
}

#[test]
fn a_rural_toilet_is_defined_placed_and_applied_without_mcp() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = live_hash(&ed.model);
    let (mut s, provider) = run(&mut ed, placed());
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["created"], json!(["family:RuralToilet"]));
    assert_eq!(live_hash(&ed.model), before, "0 writes to the live model before Apply");
    assert_eq!(count(&ed.model, "comp"), 0);
    assert_eq!(plan_of(&s).items.len(), 3);

    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "comp"), 3, "three instances");
    let fams = draft_ids(&ed);
    assert_eq!(fams.len(), 1);
    assert!(fams[0].1, "the family stays a draft after Apply");

    // the person's one button: a working family, one undo step
    let ops = count(&ed.model, "op");
    let drafted = live_hash(&ed.model);
    promote_family(&mut ed, "RuralToilet").unwrap();
    assert!(!draft_ids(&ed)[0].1);
    assert_eq!(count(&ed.model, "op"), ops + 1);
    ed.undo();
    assert_eq!(live_hash(&ed.model), drafted);
    assert!(draft_ids(&ed)[0].1);
}

#[test]
fn the_flex_check_runs_before_the_item_exists() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut def = toilet();
    def["tests"] = json!([{"Length": 5000}]);
    let (s, provider) = run(&mut ed, vec![define(def)]);
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("Length"), "{}", results[0].0);
    assert!(s.plan().is_none(), "a refused family leaves no item");
    assert!(ed.model.families().unwrap().is_empty());
}

#[test]
fn a_broken_formula_is_refused_with_its_path() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut def = toilet();
    def["forms"][1]["z"] = json!(["Wall", "Nothing + 1mm"]);
    let (s, provider) = run(&mut ed, vec![define(def)]);
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("forms"), "{}", results[0].0);
    assert!(s.plan().is_none());
}

#[test]
fn units_are_required() {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut no_unit = toilet();
    no_unit["params"][0].as_object_mut().unwrap().remove("unit");
    let mut bare = toilet();
    bare["forms"][0]["z"] = json!(["0", 2200]);
    let (s, provider) = run(&mut ed, vec![define(no_unit), define(bare)]);
    let results = tool_results(&provider);
    assert!(results[0].1 && results[0].0.contains("unit"), "{results:?}");
    assert!(results[1].1 && results[1].0.contains("unit"), "{results:?}");
    assert!(s.plan().is_none());
    // a box with neither an end nor a height has no size: refused
    let (s, provider) = run(
        &mut ed,
        vec![tool(
            "create_geometry",
            json!({"title": "Box", "ops": [{"action": "solid", "level": null,
                "form": {"kind": "extrude", "profile": {"shape": "rect", "w": 2, "h": 1}}}]}),
        )],
    );
    assert!(tool_results(&provider)[0].1);
    assert!(s.plan().is_none());
}

fn rect(w: f64, h: f64) -> J {
    json!({"shape": "rect", "w": {"v": w, "unit": "m"}, "h": {"v": h, "unit": "m"}})
}

fn frame(z: f64) -> J {
    json!({"origin": {"x": 0, "y": 0, "z": z, "unit": "m"},
           "x": {"x": 1, "y": 0, "z": 0}, "z": {"x": 0, "y": 0, "z": 1}})
}

fn extrude(w: f64, h: f64, z0: f64, z1: f64) -> J {
    json!({"kind": "extrude", "profile": rect(w, h), "frame": frame(0.0),
           "start": {"v": z0, "unit": "m"}, "end": {"v": z1, "unit": "m"}})
}

#[test]
fn geometry_is_a_draft_that_one_apply_writes_and_one_undo_removes() {
    let mut ed = Editor::new_in_memory().unwrap();
    let before = live_hash(&ed.model);
    let lvl = json!({"by": "id", "cat": "level", "id": ed.level});
    let (mut s, provider) = run(
        &mut ed,
        vec![tool(
            "create_geometry",
            json!({"title": "Box with a notch", "ops": [
                {"action": "mass", "level": lvl, "form": extrude(2.0, 1.0, 0.0, 2.0)},
                {"action": "boolean", "kind": "cut", "a": {"by": "new", "slot": 0},
                 "b": {"kind": "form", "form": extrude(0.5, 0.5, 0.5, 1.5)}, "keep_tool": true}
            ]}),
        )],
    );
    let results = tool_results(&provider);
    assert!(!results[0].1, "{results:?}");
    assert_eq!(live_hash(&ed.model), before, "0 writes before Apply");
    assert_eq!(count(&ed.model, "mass"), 0);
    apply_and_check(&mut s, &mut ed);
    // the base, the tool form and the linked result
    assert_eq!(count(&ed.model, "mass"), 3);
}

fn extents(ed: &Editor) -> Vec<(f64, f64, f64)> {
    ed.model
        .components(None)
        .unwrap()
        .into_iter()
        .map(|c| {
            let q = ed.model.component_quantities(c).unwrap();
            let [a, b] = q.bbox;
            (b.0 - a.0, b.1 - a.1, q.volume)
        })
        .collect()
}

fn set_length(ed: &mut Editor, ctype: i64, metres: f64) -> Result<(), ddc_model::Error> {
    ed.run_op(ddc_app::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.set_comp_type_values(ctype, &[("Length".into(), ddc_model::ParamValue::Num(metres))])
    })))
}

#[test]
fn changing_a_parameter_regenerates_all_three_placed_instances_in_one_undo() {
    let mut ed = Editor::new_in_memory().unwrap();
    let (mut s, provider) = run(&mut ed, placed());
    assert!(tool_results(&provider).iter().all(|r| !r.1));
    apply_and_check(&mut s, &mut ed);
    let family = ed.model.family_id("RuralToilet").unwrap().unwrap();
    let ctype = ed.model.comp_type_id(family, "Std").unwrap().unwrap();
    let before = extents(&ed);
    assert_eq!(before.len(), 3);
    let hash = live_hash(&ed.model);
    let ops = count(&ed.model, "op");

    set_length(&mut ed, ctype, 2.0).unwrap();
    assert_eq!(count(&ed.model, "op"), ops + 1, "one parameter change, one journal row");
    let after = extents(&ed);
    for (b, a) in before.iter().zip(&after) {
        // 0.8 m longer along the instance's own length axis (the third one is turned 90 degrees)
        let grew = ((a.0 - b.0).abs() - 0.8).abs() < 1e-3 || ((a.1 - b.1).abs() - 0.8).abs() < 1e-3;
        assert!(grew, "extent did not follow Length: {before:?} -> {after:?}");
        assert!(a.2 > b.2, "volume did not follow Length");
    }
    // the flex range holds: a value outside it is refused and changes nothing
    let kept = live_hash(&ed.model);
    assert!(set_length(&mut ed, ctype, 5.0).is_err());
    assert_eq!(live_hash(&ed.model), kept);

    ed.undo();
    assert_eq!(live_hash(&ed.model), hash, "one undo restores all three instances");
    assert_eq!(extents(&ed), before);
}
