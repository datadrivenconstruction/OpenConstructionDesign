// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View only (browser on a tablet or phone): opening a model and looking around never writes
//! the database. SQLite would refuse such a write under `query_only`, so a read path that
//! writes a cache would fail there; the same walk on a writable copy shows none is tried.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_app::editor::Editor;
use ddc_model::Model;
use e2e_ai_common::*;

/// Two storeys with walls and a door: what the walk has to draw.
fn project_bytes() -> Vec<u8> {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    for (i, name) in ["L1", "L2"].iter().enumerate() {
        let level = add_level(&mut ed, name, 3.0 * i as f64);
        let w = add_wall(&mut ed, level, (0.0, 0.0), (6.0, 0.0));
        add_wall(&mut ed, level, (6.0, 0.0), (6.0, 4.0));
        add_door(&mut ed, w, 2.0);
    }
    ed.model.to_bytes().unwrap()
}

/// As the page opens it: the model from bytes, then view only.
fn open(bytes: &[u8], view_only: bool) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.replace_model(Model::from_bytes(bytes).unwrap()).unwrap();
    ed.model.set_query_only(view_only).unwrap();
    ed
}

/// (total_changes, data_version, op journal top, the file's bytes)
fn stamp(m: &Model) -> (i64, i64, i64, Vec<u8>) {
    let db = m.sql();
    let total: i64 = db.query_row("SELECT total_changes()", [], |r| r.get(0)).unwrap();
    let version: i64 = db.query_row("PRAGMA data_version", [], |r| r.get(0)).unwrap();
    (total, version, m.op_top().unwrap(), m.to_bytes().unwrap())
}

/// Every storey, plan and 3D, a pick, the whole building.
fn walk(ed: &mut Editor) {
    let levels: Vec<i64> = ed.levels().iter().map(|l| l.0).collect();
    assert!(levels.len() >= 2, "{levels:?}");
    for id in levels {
        ed.set_level(id);
        ed.reload().unwrap();
        let _ = ed.scene();
        ed.select_element(Some(0));
        ed.select_element(None);
    }
    let _ = ed.scene_all();
    let _ = ed.scene_report();
}

#[test]
fn opening_and_looking_around_never_writes_the_database() {
    let bytes = project_bytes();
    for view_only in [true, false] {
        let mut ed = open(&bytes, view_only);
        assert_eq!(ed.model.query_only(), view_only);
        let before = stamp(&ed.model);
        walk(&mut ed);
        walk(&mut ed);
        let after = stamp(&ed.model);
        let what = if view_only { "view only" } else { "writable copy" };
        assert_eq!(after.0, before.0, "{what}: total_changes moved");
        assert_eq!(after.1, before.1, "{what}: data_version moved");
        assert_eq!(after.2, before.2, "{what}: the journal grew");
        assert!(after.3 == before.3, "{what}: the file's bytes changed");
    }
}
