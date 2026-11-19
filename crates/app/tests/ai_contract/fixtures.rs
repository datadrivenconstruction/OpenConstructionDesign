// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The projects a case starts from, and the ids a case names by placeholder.

use crate::e2e_ai_common::*;
use crate::e2e_common;
use ddc_app::editor::Editor;
use std::collections::BTreeMap;

/// A closed 8 x 6 house on the ground level with a door, and an upper level.
fn house() -> (Editor, i64, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let ground = ed.level;
    let pts = [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)];
    let walls: Vec<i64> = (0..4).map(|i| add_wall(&mut ed, ground, pts[i], pts[(i + 1) % 4])).collect();
    let door = add_door(&mut ed, walls[0], 2.0);
    let upper = add_level(&mut ed, "Upper", 3.0);
    (ed, door, upper)
}

/// A library component: free-standing at (x, 1.0), or on the first wall when its family is wall-hosted.
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

/// `walls()` with a WC and a washbasin, a distribution board and three sockets; the board's id.
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

/// The project of `base` and the values of the placeholders such as `{wall1}`.
pub fn open(base: &str) -> (Editor, BTreeMap<&'static str, String>) {
    let mut extra = BTreeMap::new();
    let (ed, panel) = match base {
        "walls" => (walls(), 0),
        "house" => {
            let (ed, door, upper) = house();
            extra.insert("door", door.to_string());
            extra.insert("upper", upper.to_string());
            (ed, 0)
        }
        "mep" => rooms_with_fixtures(),
        "mass" => {
            let (ed, plain, tl) = masses();
            extra.insert("mass", plain.to_string());
            extra.insert("tl_mass", tl.to_string());
            (ed, 0)
        }
        other => panic!("unknown base {other}"),
    };
    let q = |sql: &str| -> i64 { ed.model.sql().query_row(sql, [], |r| r.get(0)).unwrap() };
    let mut v = extra;
    v.insert("ground", ed.level.to_string());
    v.insert("wall1", q("SELECT id FROM wall ORDER BY id LIMIT 1").to_string());
    v.insert("wall2", q("SELECT id FROM wall ORDER BY id LIMIT 1 OFFSET 1").to_string());
    v.insert("wtype", q("SELECT wtype FROM wall ORDER BY id LIMIT 1").to_string());
    v.insert("level", ed.level.to_string());
    v.insert("panel", panel.to_string());
    v.insert("family_def", ddc_model::library_sources()[0].to_string());
    let mut src = Editor::new_in_memory().unwrap();
    src.model.add_wall_type("Imported W", &concrete(0.35)).unwrap();
    let path = e2e_common::temp_file("contract_types_src.ddc");
    src.model.snapshot_to(&path).unwrap();
    v.insert("types_src", path.to_string_lossy().replace('\\', "/"));
    (ed, v)
}
