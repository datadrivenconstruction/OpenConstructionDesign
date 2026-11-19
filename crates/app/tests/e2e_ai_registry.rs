//! The command registry end to end: a scripted model calls registered commands through the real
//! worker. An edit is a draft until Apply, Apply is one undo step, and every failure is coded.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::Script;
use ddc_ai::registry::{self, ErrorCode, Kind};
use ddc_ai::ToolOutcome;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};

/// A library component on the level of `walls()`: free-standing at (x, 1.0), or, when its family
/// is wall-hosted, on the first wall (y = 0 to the right) at `x` along it, facing the room.
fn put(ed: &mut Editor, family: &str, ty: &str, x: f64, z: f64) -> i64 {
    let m = &mut ed.model;
    let f = m.load_library_family(family).unwrap();
    let ct = m.comp_type_id(f, ty).unwrap().unwrap();
    let mut s = ddc_model::CompSpec::at(ed.level, ct, x, 1.0);
    s.at.2 = z;
    match m.add_component(&s) {
        Ok(id) => return id,
        Err(e) if e.to_string().contains("wall-hosted") => {}
        Err(e) => panic!("{family}: {e}"),
    }
    let wall: i64 = m
        .sql()
        .query_row("SELECT id FROM wall ORDER BY id", [], |r| r.get(0))
        .unwrap();
    s.host = ddc_model::CompHost::Wall(wall);
    s.at = (x, 0.1, z);
    m.add_component(&s).unwrap()
}

/// `walls()` with a WC and a washbasin, a distribution board and three sockets.
fn rooms_with_fixtures() -> (Editor, i64) {
    let mut ed = walls();
    put(&mut ed, "WC", "Floor 650", 2.0, 0.0);
    put(&mut ed, "Washbasin", "500", 3.5, 0.8);
    let panel = put(&mut ed, "Distribution Board", "400x600", 6.0, 1.2);
    for x in [7.0, 8.0, 9.0] {
        put(&mut ed, "Socket Outlet", "Single", x, 0.3);
    }
    (ed, panel)
}

/// The model a scenario of `id` starts from; the second value is a scenario's own element id
/// (the panel, or the plain box mass).
fn fixture(id: &str) -> (Editor, i64) {
    match id {
        "route_mep" | "supply_fixtures" | "power_devices" => rooms_with_fixtures(),
        "edit_mass" | "mass_to_family" => {
            let (ed, plain, _) = masses();
            (ed, plain)
        }
        _ => (walls(), 0),
    }
}

fn code_of(text: &str) -> ErrorCode {
    assert!(text.starts_with("{\"untrusted\":\"data\""), "{text}");
    let out = ToolOutcome::error(text);
    registry::error_of(&out).expect("an error").code
}

#[test]
fn an_edit_is_a_draft_and_apply_is_one_undo_step() {
    let mut ed = walls();
    let before = live_hash(&ed.model);
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool(
                "add_param",
                json!({"title": "Param", "name": "Rating", "ptype": "text", "cats": ["wall"]}),
            ),
            tool(
                "update_elements",
                json!({"title": "Set", "target": {"filter": "wall"}, "set": {"Rating": "A"}}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert_eq!(results.len(), 2, "{results:?}");
    for (text, is_error) in &results {
        assert!(!is_error, "{text}");
    }
    // the plan-item result has the shape the registry publishes
    let v: J = serde_json::from_str(&results[1].0).unwrap();
    assert!(v["item"].as_u64().is_some_and(|n| n >= 1), "{v}");
    assert!(v["changes"]["changed"].as_u64().is_some(), "{v}");
    assert_eq!(live_hash(&ed.model), before, "a draft never touches the model");
    apply_and_check(&mut s, &mut ed);
}

/// Edit commands whose Apply is proved in another suite, with the test that proves it.
const APPLY_ELSEWHERE: &[(&str, &str)] = &[
    ("run_generator", "e2e_ai_create: generate"),
    ("apply_fix", "e2e_ai_flow: Cap::Check"),
    ("define_rule", "e2e_ai_checks"),
    ("ops_propose", "e2e_ai_mcp"),
    ("define_family", "e2e_ai_geometry: a_rural_toilet_is_defined_placed_and_applied"),
    ("create_geometry", "e2e_ai_geometry: a_rural_toilet_is_defined_placed_and_applied"),
    ("run_script", "e2e_ai_script: a_script_is_a_draft_and_its_rerun_touches_only_its_own"),
    ("rerun_recipe", "e2e_ai_script: a_script_is_a_draft_and_its_rerun_touches_only_its_own"),
    ("generate_project", "e2e_ai_project: a_nine_storey_section_is_drafted_applied"),
    ("revise_project", "e2e_ai_project: a_revision_rebuilds_the_same_item_and_applies_once"),
    ("generate_block", "e2e_ai_block: a_twelve_plot_block_is_drafted_and_applied_in_one_step"),
    ("plan_station", "e2e_ai_station: a_deep_station_is_planned_on_new_levels_and_applied"),
    ("set_rule", "e2e_ai_rules: rules_are_set_locked_explained_and_applied_in_one_step"),
    ("lock", "e2e_ai_rules: rules_are_set_locked_explained_and_applied_in_one_step"),
    ("unlock", "e2e_ai_rules: rules_are_set_locked_explained_and_applied_in_one_step"),
    ("regenerate", "e2e_ai_rules: a_change_a_storey_lock_forbids_is_reported_not_applied"),
    ("connect_members", "e2e_ai_steel: every_gazebo_node_is_placed_in_one_request"),
    ("flip_openings", "e2e_ai_levels: flips"),
    ("delete_level", "e2e_ai_levels: delete_level"),
    ("place_rooms", "e2e_ai_levels: place_rooms"),
    ("place_footings", "e2e_ai_levels: place_footings"),
    ("create_terrain", "e2e_ai_levels: create_terrain"),
];
/// Edit commands that do not write the model in an undo step: they save a card, an option copy
/// or a requirement set the person confirms on its own.
const NOT_AN_UNDO_STEP: &[&str] = &[
    "generate_options",
    "discard_option",
    "create_issue",
    "load_requirements",
];

/// One scenario per edit command proved here: its tool calls on the fixture of `walls()`.
fn scenario(id: &str, ed: &Editor, panel: i64) -> Vec<Script> {
    let wall: i64 = ed
        .model
        .sql()
        .query_row("SELECT id FROM wall ORDER BY id", [], |r| r.get(0))
        .unwrap();
    let wtype: i64 = ed
        .model
        .sql()
        .query_row("SELECT wtype FROM wall ORDER BY id", [], |r| r.get(0))
        .unwrap();
    let new_type = || {
        tool(
            "create_type",
            json!({"title": "Type", "cat": "wall", "name": "W300",
                "fields": {"layers": [{"material": "Concrete", "thickness": 0.3}]}}),
        )
    };
    let param = || {
        tool(
            "add_param",
            json!({"title": "Param", "name": "Rating", "ptype": "text", "cats": ["wall"]}),
        )
    };
    match id {
        "create_elements" => vec![tool(
            "create_elements",
            json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 8], "b": [5, 8]}]}),
        )],
        "update_elements" => vec![
            param(),
            tool(
                "update_elements",
                json!({"title": "Set", "target": {"filter": "wall"}, "set": {"Rating": "A"}}),
            ),
        ],
        "delete_elements" => vec![tool(
            "delete_elements",
            json!({"title": "Delete", "target": {"refs": [format!("wall:{wall}")]}}),
        )],
        "create_type" => vec![new_type()],
        "add_param" => vec![param()],
        "replace_type" => vec![
            new_type(),
            tool(
                "replace_type",
                json!({"title": "Retype", "target": {"refs": [format!("wall:{wall}")]},
                    "to": "wall_type:n1"}),
            ),
        ],
        "create_grids" => vec![tool(
            "create_grids",
            json!({"title": "Grid", "x": {"count": 3, "spacing": 6.0},
                "y": {"count": 2, "spacing": 5.0}}),
        )],
        "edit_wall_type" => vec![tool(
            "edit_wall_type",
            json!({"title": "Thicker", "type": format!("wall_type:{wtype}"),
                "action": "resize", "layer": 1, "thickness": 0.25}),
        )],
        "load_family" => vec![tool(
            "load_family",
            json!({"title": "Family", "def": ddc_model::library_sources()[0]}),
        )],
        "copy_types_from" => {
            let mut src = Editor::new_in_memory().unwrap();
            src.model.add_wall_type("Imported W", &concrete(0.35)).unwrap();
            let path = e2e_common::temp_file("types_src.ddc");
            src.model.snapshot_to(&path).unwrap();
            vec![tool(
                "copy_types_from",
                json!({"title": "Copy", "path": path.to_string_lossy(), "names": ["Imported W"]}),
            )]
        }
        "place_fixtures" => vec![tool(
            "place_fixtures",
            // a free placement: wall-hosted families need a host the tool does not take
            json!({"title": "Sink", "items": [{"family": "Sink", "type": "600x450", "at": [3, 3]}]}),
        )],
        "route_mep" => vec![tool(
            "route_mep",
            json!({"title": "Cold", "kind": "cold", "from": {"riser": [8.0, 4.0]}}),
        )],
        "supply_fixtures" => vec![tool(
            "supply_fixtures",
            json!({"title": "Water", "riser": [8.0, 4.0]}),
        )],
        "power_devices" => vec![tool(
            "power_devices",
            json!({"title": "Power", "panel": format!("comp:{panel}")}),
        )],
        "measure_save" => vec![tool(
            id,
            json!({"title": "Gap", "kind": "distance", "view": format!("level:{}", ed.level),
                   "anchors": [format!("wall:{wall}@face0"), [5.0, 6.0, 0.0]]}),
        )],
        // `panel` is the plain box mass of `masses()`
        "edit_mass" => vec![tool(
            id,
            json!({"title": "Taller", "mass": format!("mass:{panel}"), "action": "push_pull",
                   "faces": ["top"], "distance": 3.0}),
        )],
        "mass_to_family" => {
            let other: i64 = ed
                .model
                .sql()
                .query_row("SELECT id FROM mass WHERE id<>?1 ORDER BY id LIMIT 1", [panel], |r| r.get(0))
                .unwrap();
            vec![tool(
                id,
                json!({"title": "Family", "mass": format!("mass:{other}"), "name": "Block"}),
            )]
        }
        _ => modify_scenario(id, wall, ed.level),
    }
}

/// Scenarios of the modify and level commands on the same fixture.
fn modify_scenario(id: &str, wall: i64, level: i64) -> Vec<Script> {
    let w = json!({"refs": [format!("wall:{wall}")]});
    let args = match id {
        "move_elements" => json!({"by": [0.0, 1.0]}),
        "copy_elements" => json!({"offsets": [[0.0, 3.0]]}),
        "rotate_elements" => json!({"center": [0.0, 0.0], "angle": 90.0}),
        "mirror_elements" => json!({"a": [0.0, -2.0], "b": [10.0, -2.0], "copy": true}),
        "scale_elements" => json!({"center": [0.0, 0.0], "factor": 0.5}),
        "array_elements" => json!({"mode": "linear", "count": 3, "step": [0.0, 2.0]}),
        "offset_wall" => json!({"distance": 1.0, "side": "left"}),
        "split_wall" => json!({"at": [4.0, 0.0]}),
        "pin_elements" => json!({"pinned": true}),
        "join_elements" => {
            let both = [format!("wall:{wall}"), format!("wall:{}", wall + 1)];
            json!({"target": {"refs": both}, "action": "join"})
        }
        "create_level" => json!({"name": "Level 2", "elevation": 3.0}),
        "edit_level" => json!({"level": format!("level:{level}"), "elevation": 0.5}),
        _ => return vec![],
    };
    let mut a = json!({"title": id, "target": w});
    if id.ends_with("_level") {
        a = json!({"title": id});
    }
    for (k, v) in args.as_object().unwrap() {
        a[k] = v.clone();
    }
    vec![tool(id, a)]
}

/// Every edit command of the registry: its draft leaves the model alone and its Apply is one
/// undo step, undone exactly; or it is listed with where that is proved, or why it is not one.
#[test]
fn every_edit_command_applies_as_one_undo_step() {
    let edits: Vec<&str> = registry::commands()
        .iter()
        .filter(|c| c.kind == Kind::Edit)
        .map(|c| c.id())
        .collect();
    let mut proved = Vec::new();
    for id in &edits {
        if APPLY_ELSEWHERE.iter().any(|e| e.0 == *id)
            || NOT_AN_UNDO_STEP.contains(id)
        {
            continue;
        }
        let (mut ed, panel) = fixture(id);
        let tools = scenario(id, &ed, panel);
        assert!(!tools.is_empty(), "{id}: a new edit command needs a scenario here");
        let before = live_hash(&ed.model);
        let (mut s, provider) = run(&mut ed, tools);
        let results = tool_results(&provider);
        assert!(results.iter().all(|r| !r.1), "{id}: {results:?}");
        assert_eq!(live_hash(&ed.model), before, "{id}: a draft never touches the model");
        apply_and_check(&mut s, &mut ed);
        proved.push(*id);
    }
    assert_eq!(
        proved.len() + APPLY_ELSEWHERE.len() + NOT_AN_UNDO_STEP.len(),
        edits.len(),
        "{proved:?}"
    );
    assert!(proved.len() >= 10, "{proved:?}");
    for (id, _) in APPLY_ELSEWHERE {
        assert!(edits.contains(id), "{id} is no longer an edit command");
    }
}

#[test]
fn a_valid_edit_that_changes_nothing_is_an_error_not_an_empty_item() {
    let mut ed = walls();
    let before = live_hash(&ed.model);
    let (s, provider) = run(
        &mut ed,
        vec![
            tool(
                "add_param",
                json!({"title": "Param", "name": "Load", "ptype": "number", "cats": ["wall"]}),
            ),
            // no wall has a load yet, so "ten percent more" leaves every wall out
            tool(
                "update_elements",
                json!({"title": "More", "target": {"filter": "wall"}, "set": {"Load": "+10%"}}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(!results[0].1, "{results:?}");
    assert!(results[1].1, "{results:?}");
    assert_eq!(code_of(&results[1].0), ErrorCode::NoEffect, "{}", results[1].0);
    let e = registry::error_of(&ToolOutcome::error(results[1].0.clone())).unwrap();
    assert_eq!(e.refs.len(), 2, "each left-out wall is named: {e:?}");
    assert!(e.refs.iter().all(|r| r.starts_with("wall:")), "{e:?}");
    let items = s.plan().map_or(0, |p| p.items.len());
    assert_eq!(items, 1, "only the parameter became an item; no empty item stays");
    assert_eq!(live_hash(&ed.model), before);
}

#[test]
fn a_filter_that_matches_nothing_is_not_found() {
    let mut ed = walls();
    let before = live_hash(&ed.model);
    let (s, provider) = run(
        &mut ed,
        vec![tool(
            "delete_elements",
            json!({"title": "Nothing", "target": {"filter": "door"}}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert_eq!(code_of(&results[0].0), ErrorCode::NotFound, "{}", results[0].0);
    assert!(s.plan().is_none_or(|p| p.items.is_empty()), "no empty item stays");
    assert_eq!(live_hash(&ed.model), before);
}

#[test]
fn every_failure_reaching_the_model_is_coded() {
    let mut ed = walls();
    let (s, provider) = run(
        &mut ed,
        vec![
            tool("execute_sql", json!({"sql": "DROP TABLE wall"})),
            tool("delete_elements", json!({"title": 5})),
            tool("get_element", json!({"ref": "wall:999999"})),
        ],
    );
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3, "{results:?}");
    assert_eq!(code_of(&results[0].0), ErrorCode::UnknownCommand);
    assert_eq!(code_of(&results[1].0), ErrorCode::InvalidArgs);
    let bad = registry::error_of(&ToolOutcome::error(results[1].0.clone())).unwrap();
    assert!(bad.path.as_deref().is_some_and(|p| p.starts_with("args")), "{bad:?}");
    assert!(results[2].1, "{results:?}");
    assert_eq!(code_of(&results[2].0), ErrorCode::NotFound, "{}", results[2].0);
    assert!(s.plan().is_none());
}

#[test]
fn air_route_sizes_by_friction_and_reports_the_critical_path() {
    let mut ed = walls();
    let ahu = put(&mut ed, "Air Handling Unit", "2000x1000", 2.0, 0.0);
    let d: Vec<i64> = [5.0, 8.0]
        .iter()
        .map(|x| put(&mut ed, "Supply Diffuser", "300x300", *x, 2.4))
        .collect();
    for c in &d {
        ed.model.mep_set_port_flow(*c, "Air", Some(400.0)).unwrap();
    }
    let to: Vec<String> = d.iter().map(|c| format!("comp:{c}/Air")).collect();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool(
                "route_mep",
                json!({"title": "SA", "kind": "supply_air", "from": {"port": format!("comp:{ahu}/Supply")},
                       "to": to, "friction_pa_m": 1.0, "insulation_mm": 25, "plenum": [2.5, 3.0]}),
            ),
            tool(
                "route_mep",
                json!({"title": "Bad", "kind": "supply_air", "from": {"port": format!("comp:{ahu}/Supply")},
                       "to": to, "plenum": [3.0, 2.7]}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    assert!(results[0].0.contains("critical_pa"), "{}", results[0].0);
    assert!(results[1].1, "{}", results[1].0);
    apply_and_check(&mut s, &mut ed);
    let runs: Vec<_> = ed
        .model
        .mep_runs(None)
        .unwrap()
        .into_iter()
        .map(|id| ed.model.mep_run(id).unwrap())
        .collect();
    // 800 m3/h at 1 Pa/m: D315 main; every duct carries the 25 mm insulation
    assert!(runs.iter().any(|r| matches!(r.section.shape,
        ddc_model::MepShape::Round { d } if (d - 0.315).abs() < 1e-9)), "{runs:?}");
    assert!(runs.iter().all(|r| (r.section.insulation - 0.025).abs() < 1e-12));
}
