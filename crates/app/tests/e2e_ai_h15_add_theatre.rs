// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! H15, one more operating theatre on one floor of an applied hospital wing, through the chat:
//! revise_project with `storey_rooms` places the room on that storey alone (every other storey
//! is locked to what it is), keeping the element ids of the other levels, and the new room
//! gets its supply and extract air.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::Mode;
use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::json;

const STOREY: usize = 2;
const TABLES: [&str; 5] = ["wall", "slab", "room", "opening", "mep_run"];

/// Run ids of every system off `level` (a riser with no level counts), by system name.
fn runs_off(ed: &Editor, level: i64) -> Vec<(String, i64)> {
    let mut st = ed
        .model
        .sql()
        .prepare("SELECT s.name, r.id FROM mep_run r JOIN mep_system s ON s.id = r.system WHERE r.level IS NOT ?1 ORDER BY s.name, r.id")
        .unwrap();
    st.query_map([level], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
}

/// Ids of a table on the levels other than `level`.
fn ids_off(ed: &Editor, table: &str, level: i64) -> Vec<i64> {
    let sql = match table {
        "opening" => "SELECT o.id FROM opening o JOIN wall w ON w.id = o.wall WHERE w.level != ?1 ORDER BY o.id".to_string(),
        t => format!("SELECT id FROM {t} WHERE level != ?1 ORDER BY id"),
    };
    let mut st = ed.model.sql().prepare(&sql).unwrap();
    st.query_map([level], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

#[test]
fn a_theatre_added_on_one_storey_leaves_the_other_levels_alone() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    let spec = json!({"typology": "hospital_wing", "floors": 4});
    let theatre = json!({"name": "Operating theatre", "kind": "technical", "area": 48, "tags": ["operating"]});
    let done = || vec![Script::Text("done".to_string())];
    let provider = ScriptedProvider::new(vec![
        vec![tool("generate_project", json!({"spec": spec}))],
        done(),
        vec![tool("revise_project", json!({"changes": {"storey_rooms": {STOREY.to_string(): [theatre]}}}))],
        done(),
    ]);
    let mut s = start(&mut ed, cfg(provider.clone()));
    turn(&mut s, &mut ed, "hospital wing, four floors", Mode::Propose);
    apply_and_check(&mut s, &mut ed);
    // the storey's level by its name, not by where its elevation falls in the list
    let level: i64 = ed
        .model
        .sql()
        .query_row("SELECT id FROM level WHERE name = ?1", [format!("Level {}", STOREY + 1)], |r| r.get(0))
        .unwrap();
    let before: Vec<Vec<i64>> = TABLES.iter().map(|t| ids_off(&ed, t, level)).collect();
    let runs0 = runs_off(&ed, level);
    let circuits0: Vec<i64> = {
        let mut st = ed.model.sql().prepare("SELECT id FROM elec_circuit ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert!(!runs0.is_empty(), "the wing has runs on other levels");
    let rooms0: i64 = ed.model.sql().query_row("SELECT count(*) FROM room WHERE level = ?1", [level], |r| r.get(0)).unwrap();

    turn(&mut s, &mut ed, "one more operating theatre on the third floor, leave the rest", Mode::Propose);
    let results = tool_results(&provider);
    let (last, err) = results.last().unwrap();
    assert!(!err, "{last}");
    apply_and_check(&mut s, &mut ed);

    for (k, t) in TABLES.iter().enumerate() {
        assert_eq!(ids_off(&ed, t, level), before[k], "{t}: ids of the other levels");
    }
    assert_eq!(runs_off(&ed, level), runs0, "run ids of every system on the other levels, risers included");
    let circuits1: Vec<i64> = {
        let mut st = ed.model.sql().prepare("SELECT id FROM elec_circuit ORDER BY id").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert!(circuits0.iter().all(|c| circuits1.contains(c)), "circuits kept: {circuits0:?} then {circuits1:?}");
    let rooms1: i64 = ed.model.sql().query_row("SELECT count(*) FROM room WHERE level = ?1", [level], |r| r.get(0)).unwrap();
    assert_eq!(rooms1, rooms0 + 1, "one room more on the storey");
    // its air: supply and return/extract runs on that level
    let systems: Vec<String> = {
        let mut st = ed
            .model
            .sql()
            .prepare("SELECT DISTINCT lower(s.name) FROM mep_run r JOIN mep_system s ON s.id = r.system WHERE r.level = ?1")
            .unwrap();
        st.query_map([level], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
    };
    assert!(systems.iter().any(|n| n.contains("supply")), "{systems:?}");
    assert!(systems.iter().any(|n| n.contains("return") || n.contains("extract") || n.contains("exhaust")), "{systems:?}");
}
