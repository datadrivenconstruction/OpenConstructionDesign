// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every table the model can build is readable by the read tools: describe_category answers for
//! type tables and for shafts, grids and steel connections, and get_element reads rows that the
//! query layer has no row for.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_model::{GridSpec, ShaftSpec};
use e2e_ai_common::*;
use serde_json::{json, Value as J};

fn first_id(ed: &Editor, table: &str) -> Option<i64> {
    ed.model
        .sql()
        .query_row(&format!("SELECT min(id) FROM {table}"), [], |r| r.get(0))
        .ok()
        .flatten()
}

#[test]
fn describe_category_knows_types_shafts_grids_and_connections() {
    let mut ed = Editor::new_in_memory().unwrap();
    let words = [
        "wall_type", "slab_type", "member_type", "comp_type", "shaft", "grid", "steel_conn",
        "shafts", "column_type",
    ];
    let (_s, provider) = run(
        &mut ed,
        words.iter().map(|w| tool("describe_category", json!({"cat": w}))).collect(),
    );
    let res = tool_results(&provider);
    assert_eq!(res.len(), words.len(), "{res:?}");
    for (w, (text, err)) in words.iter().zip(&res) {
        assert!(!err, "{w}: {text}");
        let j: J = serde_json::from_str(text).unwrap_or_else(|_| panic!("{w}: {text}"));
        assert!(j["fields"].is_object(), "{w}: {text}");
    }
}

#[test]
fn get_element_reads_types_shafts_and_grids() {
    let mut ed = Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let l2 = add_level(&mut ed, "L2", 3.0);
    let shaft = ed
        .model
        .add_shaft(&ShaftSpec {
            level: l1,
            top_level: l2,
            base_offset: 0.0,
            top_offset: 0.0,
            ring: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
        })
        .unwrap();
    let grid = ed.model.add_grid(&GridSpec::line("A", (0.0, 0.0), (10.0, 0.0))).unwrap();
    ed.reload().unwrap();
    let wt = ed.types[0].id;
    let mut refs = vec![
        format!("wall_type:{wt}"),
        format!("shaft:{shaft}"),
        format!("grid:{grid}"),
        "shaft:999999".to_string(),
    ];
    if let Some(m) = first_id(&ed, "member_type") {
        refs.push(format!("member_type:{m}"));
    }
    let (_s, provider) = run(
        &mut ed,
        refs.iter().map(|r| tool("get_element", json!({"ref": r}))).collect(),
    );
    let res = tool_results(&provider);
    assert_eq!(res.len(), refs.len(), "{res:?}");
    for (r, (text, err)) in refs.iter().zip(&res) {
        if r == "shaft:999999" {
            assert!(*err && text.contains("not_found"), "{r}: {text}");
            continue;
        }
        assert!(!err, "{r}: {text}");
        assert!(text.contains(r.as_str()), "{r}: {text}");
    }
    assert!(res[0].0.contains("layers"), "wall type lists its layers: {}", res[0].0);
}
