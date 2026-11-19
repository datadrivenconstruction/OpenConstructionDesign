// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The wider create path: in one turn, before any Apply, the assistant plans levels below
//! ground, makes its own wall and member types through `create_elements`, and draws walls,
//! columns, a shaft and a grid line on the planned levels by name, the types by name or
//! under their table word. Apply then gives exactly the preview.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::json;

fn scalar(ed: &Editor, q: &str) -> i64 {
    ed.model.sql().query_row(q, [], |r| r.get(0)).unwrap()
}

fn box_walls(level: &str, ty: serde_json::Value) -> serde_json::Value {
    let pts = [(0.0, 0.0), (150.0, 0.0), (150.0, 26.0), (0.0, 26.0)];
    let items: Vec<_> = (0..4)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % 4]);
            let mut o = json!({"a": [a.0, a.1], "b": [b.0, b.1], "level": level, "height": 17.0});
            if let Some((k, v)) = ty.as_object().and_then(|m| m.iter().next()) {
                o[k] = v.clone();
            }
            o
        })
        .collect();
    json!({"title": "Diaphragm walls", "cat": "wall", "items": items})
}

#[test]
fn a_box_below_ground_is_drawn_on_planned_levels_with_its_own_types() {
    let mut ed = Editor::new_in_memory().unwrap();
    let levels_before = count(&ed.model, "level");
    let script = vec![
        tool("create_level", json!({"title": "Concourse", "name": "Concourse", "elevation": -9.0})),
        tool("create_level", json!({"title": "Platform", "name": "Platform", "elevation": -16.0})),
        tool(
            "create_elements",
            json!({"title": "Station types", "cat": "wall_type", "items": [
                {"name": "Diaphragm 800", "layers": [{"material": "concrete", "thickness": 0.8}]},
            ]}),
        ),
        tool(
            "create_elements",
            json!({"title": "Column type", "cat": "member_type", "items": [
                {"name": "Column 800", "shape": "rect 0.8 0.8", "material": "concrete"},
            ]}),
        ),
        // the type under its table word, as a model writes it after describe_category
        tool("create_elements", box_walls("Platform", json!({"wall_type": "Diaphragm 800"}))),
        tool(
            "create_elements",
            json!({"title": "Platform columns", "cat": "column", "items": [
                {"at": [60.0, 13.0], "level": "Platform", "top_level": "Concourse", "member_type": "column 800"},
                {"at": [90.0, 13.0], "level": "Platform", "top_level": "Concourse", "type": "Column 800"},
            ]}),
        ),
        tool(
            "create_elements",
            json!({"title": "Ventilation shaft", "cat": "shaft", "items": [
                {"ring": [[2.0, 2.0], [7.0, 2.0], [7.0, 7.0], [2.0, 7.0]], "level": "Platform", "top_level": "Concourse"},
            ]}),
        ),
        tool(
            "create_elements",
            json!({"title": "Axis", "cat": "grid", "items": [{"name": "A", "a": [0.0, -5.0], "b": [0.0, 31.0]}]}),
        ),
    ];
    let n = script.len();
    let (mut s, provider) = run(&mut ed, script);
    let res = tool_results(&provider);
    assert_eq!(res.len(), n, "{res:?}");
    for (i, (text, err)) in res.iter().enumerate() {
        assert!(!err, "call {i}: {text}");
    }
    assert!(res[2].0.contains("wall_type:n"), "{}", res[2].0);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "level"), levels_before + 2);
    let dia = "SELECT count(*) FROM wall w JOIN wall_type t ON t.id = w.wtype \
               JOIN level l ON l.id = w.level WHERE t.name = 'Diaphragm 800' AND l.name = 'Platform'";
    assert_eq!(scalar(&ed, dia), 4, "four diaphragm walls of the new type on the platform level");
    let cols = "SELECT count(*) FROM col c JOIN member_type t ON t.id = c.mtype WHERE t.name = 'Column 800'";
    assert_eq!(scalar(&ed, cols), 2, "both columns took the new member type");
    assert_eq!(count(&ed.model, "shaft"), 1);
    assert_eq!(count(&ed.model, "grid"), 1);
    let low = "SELECT CAST(min(elevation) * 10 AS INTEGER) FROM level";
    assert_eq!(scalar(&ed, low), -160);
}

#[test]
fn wrong_create_items_are_refused_with_the_reason() {
    let mut ed = Editor::new_in_memory().unwrap();
    let cases = [
        (json!({"title": "x", "cat": "shaft", "items": [{"ring": [[0, 0], [1, 0], [1, 1]]}]}), "top_level"),
        (json!({"title": "x", "cat": "shaft", "items": [{"ring": [[0, 0], [1, 0]], "top_level": 1}]}), "three points"),
        (json!({"title": "x", "cat": "wall", "items": [{"a": [0, 0], "b": [5, 0], "level": "Nowhere"}]}), "no level"),
        (json!({"title": "x", "cat": "grid", "items": [{"name": "B", "a": [0, 0], "b": [0, 0]}]}), "must differ"),
        (json!({"title": "x", "cat": "steel_conn", "items": [{"type": "nonsense", "members": []}]}), "unknown type"),
        (json!({"title": "x", "cat": "wall_type", "items": [{"layers": []}]}), "needs a name"),
    ];
    let (_s, provider) =
        run(&mut ed, cases.iter().map(|c| tool("create_elements", c.0.clone())).collect());
    let res = tool_results(&provider);
    assert_eq!(res.len(), cases.len(), "{res:?}");
    for ((_, want), (text, err)) in cases.iter().zip(&res) {
        assert!(*err && text.contains(want), "want {want}: {text}");
    }
}

#[test]
fn a_types_call_adds_all_its_types_or_none_and_reads_the_offered_fields() {
    let mut ed = Editor::new_in_memory().unwrap();
    let script = vec![
        // the second type is broken: the first must not stay behind
        tool(
            "create_elements",
            json!({"title": "x", "cat": "member_type", "items": [
                {"name": "Kept out", "shape": "rect 0.4 0.4"},
                {"name": "Broken", "shape": "no such shape"},
            ]}),
        ),
        // the fields describe_category offers: slab thickness and material, member section
        tool(
            "create_elements",
            json!({"title": "y", "cat": "slab_type", "items": [{"name": "Base 1200", "thickness": 1.2, "material": "concrete"}]}),
        ),
        tool(
            "create_elements",
            json!({"title": "z", "cat": "member_type", "items": [{"name": "Pier", "section": "rect 0.9 0.9"}]}),
        ),
    ];
    let (mut s, provider) = run(&mut ed, script);
    let res = tool_results(&provider);
    assert_eq!(res.len(), 3, "{res:?}");
    assert!(res[0].1, "{}", res[0].0);
    assert!(!res[1].1 && !res[2].1, "{res:?}");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(scalar(&ed, "SELECT count(*) FROM member_type WHERE name IN ('Kept out', 'Broken')"), 0);
    let base = "SELECT CAST(sum(l.thickness) * 1000 AS INTEGER) FROM slab_type_layer l \
                JOIN slab_type t ON t.id = l.stype WHERE t.name = 'Base 1200'";
    assert_eq!(scalar(&ed, base), 1200);
    assert_eq!(scalar(&ed, "SELECT count(*) FROM member_type WHERE name = 'Pier'"), 1);
}
