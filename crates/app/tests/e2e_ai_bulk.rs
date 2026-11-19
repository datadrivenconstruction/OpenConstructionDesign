// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Replace all doors on floor 3 with EI30", in the two shapes a model has: doors in walls take a
//! parameter, placed door families take another type.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::Script;
use ddc_ai::Mode;
use ddc_app::ai::rows;
use ddc_app::editor::Editor;
use ddc_app::units::Units;
use ddc_model::{CompHost, CompSpec, Model};
use e2e_ai_common::*;
use serde_json::json;
use std::collections::BTreeSet;

const DOOR_FAMILY: &str = r#"family Door
category generic
host floor
param Width length type 900mm
param Depth length type 100mm
param Height length type 2100mm
param Leaf material type "Oak"
ref Left x = -Width / 2
ref Right x = Width / 2
ref Front y = -Depth / 2
ref Back y = Depth / 2
type "EI15" Width=900mm
type "EI30" Width=1000mm
extrude Panel rect(Left, Front, Right, Back) z(0, Height) mat(Leaf) show(3d, elev)
symbol rect(Left, Front, Right, Back)
"#;

struct House {
    ed: Editor,
    floor2: i64,
    floor3: i64,
    doors2: BTreeSet<i64>,
    doors3: BTreeSet<i64>,
}

fn house() -> House {
    let mut ed = Editor::new_in_memory().unwrap();
    let floor2 = add_level(&mut ed, "Floor 2", 3.0);
    let floor3 = add_level(&mut ed, "Floor 3", 6.0);
    let first = ed.level;
    let w1 = add_wall(&mut ed, first, (0.0, 0.0), (12.0, 0.0));
    add_door(&mut ed, w1, 2.0);
    let w2 = add_wall(&mut ed, floor2, (0.0, 0.0), (12.0, 0.0));
    let doors2 = [2.0, 6.0]
        .iter()
        .map(|x| add_door(&mut ed, w2, *x))
        .collect();
    let w3 = add_wall(&mut ed, floor3, (0.0, 0.0), (12.0, 0.0));
    let doors3 = [2.0, 6.0, 10.0]
        .iter()
        .map(|x| add_door(&mut ed, w3, *x))
        .collect();
    House {
        ed,
        floor2,
        floor3,
        doors2,
        doors3,
    }
}

fn doors_with(ed: &Editor, name: &str, value: &str) -> BTreeSet<i64> {
    rows::rows(ed)
        .iter()
        .filter(|r| r.cat == "door" && r.props.iter().any(|p| p.0 == name && p.1 == value))
        .map(|r| r.id())
        .collect()
}

fn add_param() -> Script {
    tool(
        "add_param",
        json!({"title": "Add the rating", "name": "Fire rating", "ptype": "text", "cats": ["door"]}),
    )
}

#[test]
fn doors_in_walls_on_floor_3_get_the_rating_and_nothing_else_moves() {
    let mut h = house();
    let before = live_hash(&h.ed.model);
    let (mut s, provider) = run(
        &mut h.ed,
        vec![
            tool(
                "query_elements",
                json!({"filter": "door level=3", "fields": ["width"]}),
            ),
            add_param(),
            tool(
                "update_elements",
                json!({"title": "Doors of floor 3 become EI30", "target": {"set": "s1"}, "set": {"Fire rating": "EI30"}}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["total"], json!(3));

    let plan = plan_of(&s);
    assert_eq!(plan.items.len(), 2);
    assert_eq!(
        live_hash(&h.ed.model),
        before,
        "the plan must not touch the live model"
    );

    apply_and_check(&mut s, &mut h.ed);
    assert_eq!(doors_with(&h.ed, "fire rating", "EI30"), h.doors3);
}

#[test]
fn a_new_parameter_has_one_guid_on_the_copy_and_on_the_live_model() {
    let mut h = house();
    let (mut s, _p) = run(&mut h.ed, vec![add_param()]);
    apply_and_check(&mut s, &mut h.ed);
    let guid = |m: &Model| -> String {
        m.sql()
            .query_row(
                "SELECT guid FROM pparam WHERE name='Fire rating'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let work = Model::open_copy(&s.work_path().unwrap().to_string_lossy()).unwrap();
    assert_eq!(guid(&h.ed.model), guid(&work));
}

#[test]
fn a_value_for_a_parameter_nobody_added_is_an_error() {
    let mut h = house();
    let (s, provider) = run(
        &mut h.ed,
        vec![tool(
            "update_elements",
            json!({"title": "No", "target": {"filter": "door"}, "set": {"Fire rating": "EI30"}}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(s.plan().is_none());
}

#[test]
fn a_locked_value_stops_the_ai_item_and_says_why() {
    let mut h = house();
    h.ed.model
        .add_params(&[ddc_model::ParamSpec {
            name: "Fire rating".into(),
            ptype: ddc_model::ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["door".into()],
        }])
        .unwrap();
    let door = *h.doors3.iter().next().unwrap();
    h.ed.model
        .apply_edits(&[ddc_model::Edit::ParamValue {
            param: "Fire rating".into(),
            kind: "opening".into(),
            id: door,
            value: Some(ddc_model::PVal::Text("EI15".into())),
        }])
        .unwrap();
    h.ed.model
        .set_param_lock(&[("opening", door, "Fire rating")])
        .unwrap();
    h.ed.revision += 1;
    let before = live_hash(&h.ed.model);
    let (s, provider) = run(
        &mut h.ed,
        vec![tool(
            "update_elements",
            json!({"title": "EI30", "target": {"filter": "door level=3"}, "set": {"Fire rating": "EI30"}}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("locked"), "{results:?}");
    assert!(s.plan().is_none());
    assert_eq!(live_hash(&h.ed.model), before);
    assert_eq!(doors_with(&h.ed, "fire rating", "EI15"), BTreeSet::from([door]));
    assert!(doors_with(&h.ed, "fire rating", "EI30").is_empty());
}

/// With a basement the third level by elevation is not the one named "Floor 3".
#[test]
fn a_level_number_counts_by_elevation_and_a_quoted_name_is_exact() {
    let mut h = house();
    add_level(&mut h.ed, "Basement", -3.0);
    let (_s, provider) = run(
        &mut h.ed,
        vec![
            tool("query_elements", json!({"filter": "door level=3"})),
            tool("query_elements", json!({"filter": "door level=\"Floor 3\""})),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["total"], json!(2));
    assert_eq!(parsed(&results[1].0)["total"], json!(3));
}

/// 1.2 m is frozen as text in project units: a replay under other units must read it as written.
#[test]
fn a_change_of_units_does_not_change_what_an_item_means() {
    let mut h = house();
    h.ed.set_units(Units::Metric);
    let provider = ddc_ai::fake::ScriptedProvider::new(vec![
        vec![tool(
            "update_elements",
            json!({"title": "Wider", "target": {"filter": "door level=3"}, "set": {"width": 1.2}}),
        )],
        vec![Script::Text("done".to_string())],
        vec![Script::Text("nothing else".to_string())],
    ]);
    let mut s = start(&mut h.ed, cfg(provider));
    turn(&mut s, &mut h.ed, "wider doors", Mode::Propose);
    h.ed.set_units(Units::Imperial);
    turn(&mut s, &mut h.ed, "anything else?", Mode::Propose);
    s.set_checked(vec![(1, true)]);
    assert!(s.wait_idle(&mut h.ed, WAIT));
    apply_and_check(&mut s, &mut h.ed);
    for id in &h.doors3 {
        let w: f64 =
            h.ed.model
                .sql()
                .query_row("SELECT width FROM opening WHERE id=?1", [*id], |r| r.get(0))
                .unwrap();
        assert!((w - 1.2).abs() < 1e-6, "{w}");
    }
}

#[test]
fn an_unticked_item_is_not_applied() {
    let mut h = house();
    let (mut s, _p) = run(
        &mut h.ed,
        vec![
            add_param(),
            tool(
                "update_elements",
                json!({"title": "Floor 3", "target": {"filter": "door level=3"}, "set": {"Fire rating": "EI30"}}),
            ),
            tool(
                "update_elements",
                json!({"title": "Floor 2", "target": {"filter": "door level=2"}, "set": {"Fire rating": "EI60"}}),
            ),
        ],
    );
    assert_eq!(plan_of(&s).items.len(), 3);
    s.set_checked(vec![(3, false)]);
    assert!(
        !s.can_apply(),
        "Apply must wait until the worker recompiled the ticks"
    );
    assert!(s.wait_idle(&mut h.ed, WAIT));
    apply_and_check(&mut s, &mut h.ed);
    assert_eq!(doors_with(&h.ed, "fire rating", "EI30"), h.doors3);
    assert!(
        doors_with(&h.ed, "fire rating", "EI60").is_empty(),
        "item 3 was unticked: {:?}",
        h.doors2
    );
}

#[test]
fn keyboard_verdicts_apply_only_what_was_accepted() {
    use ddc_app::ai::ui::review::{outcome, ReviewAct};
    let mut h = house();
    let (mut s, _p) = run(
        &mut h.ed,
        vec![
            add_param(),
            tool(
                "update_elements",
                json!({"title": "Floor 3", "target": {"filter": "door level=3"}, "set": {"Fire rating": "EI30"}}),
            ),
            tool(
                "update_elements",
                json!({"title": "Floor 2", "target": {"filter": "door level=2"}, "set": {"Fire rating": "EI60"}}),
            ),
        ],
    );
    let before = live_hash(&h.ed.model);
    // Esc leaves the check marks alone
    let plan = plan_of(&s);
    let esc = outcome(ReviewAct::Leave, &plan.items, &plan.diff, Some(3));
    assert!(esc.check.is_empty() && !esc.apply);
    // R on the last row rejects it; nothing reaches the model before Apply
    let r = outcome(ReviewAct::Reject, &plan.items, &plan.diff, Some(3));
    assert_eq!(r.check, vec![(3, false)]);
    s.set_checked(r.check);
    assert!(s.wait_idle(&mut h.ed, WAIT));
    assert_eq!(live_hash(&h.ed.model), before);
    apply_and_check(&mut s, &mut h.ed);
    assert_eq!(doors_with(&h.ed, "fire rating", "EI30"), h.doors3);
    assert!(doors_with(&h.ed, "fire rating", "EI60").is_empty());
}

#[test]
fn unticking_a_parameter_unticks_what_depends_on_it() {
    let mut h = house();
    let (mut s, _p) = run(
        &mut h.ed,
        vec![
            add_param(),
            tool(
                "update_elements",
                json!({"title": "Floor 3", "target": {"filter": "door level=3"}, "set": {"Fire rating": "EI30"}}),
            ),
        ],
    );
    s.set_checked(vec![(1, false)]);
    assert!(s.wait_idle(&mut h.ed, WAIT));
    let plan = plan_of(&s);
    assert!(!plan.items[1].checked);
    assert!(plan.items[1].conflict.is_some());
    assert!(plan.diff.is_empty());
    assert!(!s.can_apply());
}

fn families(ed: &mut Editor, floor2: i64, floor3: i64) -> (i64, Vec<i64>, Vec<i64>) {
    let fam = ed.model.load_family(DOOR_FAMILY).unwrap();
    let types = ed.model.comp_types(fam).unwrap();
    let id_of = |n: &str| types.iter().find(|t| t.1 == n).unwrap().0;
    let (ei15, ei30) = (id_of("EI15"), id_of("EI30"));
    let spec = |level: i64, x: f64| CompSpec {
        level,
        ctype: ei15,
        at: (x, 1.0, 0.0),
        rotation: 0.0,
        mirror: false,
        tilt: None,
        host: CompHost::None,
        values: vec![],
    };
    let on2 = ed.model.add_components(&[spec(floor2, 1.0)]).unwrap();
    let on3 = ed
        .model
        .add_components(&[spec(floor3, 1.0), spec(floor3, 3.0)])
        .unwrap();
    ed.reload().unwrap();
    (ei30, on2, on3)
}

fn ctype(m: &Model, id: i64) -> i64 {
    m.component(id).unwrap().ctype
}

#[test]
fn placed_door_families_on_floor_3_change_their_type() {
    let mut h = house();
    let (ei30, on2, on3) = families(&mut h.ed, h.floor2, h.floor3);
    let (mut s, provider) = run(
        &mut h.ed,
        vec![
            tool("query_elements", json!({"filter": "comp level=3"})),
            tool(
                "replace_type",
                json!({"title": "Doors of floor 3 become EI30", "target": {"set": "s1"}, "to": format!("comp_type:{ei30}")}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["total"], json!(2));

    apply_and_check(&mut s, &mut h.ed);
    for id in on3 {
        assert_eq!(ctype(&h.ed.model, id), ei30);
    }
    for id in on2 {
        assert_ne!(ctype(&h.ed.model, id), ei30);
    }
}

#[test]
fn doors_in_walls_refuse_a_type_with_a_clear_reason() {
    let mut h = house();
    let (ei30, _, _) = families(&mut h.ed, h.floor2, h.floor3);
    let (s, provider) = run(
        &mut h.ed,
        vec![tool(
            "replace_type",
            json!({"title": "No", "target": {"filter": "door level=3"}, "to": format!("comp_type:{ei30}")}),
        )],
    );
    let results = tool_results(&provider);
    assert!(results[0].1, "{results:?}");
    assert!(results[0].0.contains("no types"), "{}", results[0].0);
    assert!(s.plan().is_none());
}
