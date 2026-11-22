// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parity gate of views: each linear, hosted and modify script replayed through the plan and
//! through the 3D view (default orbit, level work plane) leaves the same journal.
use ddc_app::interact::parity::assert_parity;
use ddc_app::interact::script::parse;
use ddc_app::interact::ViewKind;

const PLAN: ViewKind = ViewKind::Plan { level: 0 };

fn script(name: &str) -> ddc_app::interact::script::Script {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix").join(name);
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    parse(&src).unwrap_or_else(|e| panic!("{name}:{e}"))
}

#[test]
fn linear_families_plan_eq_3d() {
    // wall (chain, arc, rectangle, typed room), room, floor, column, beam, grid, reference plane
    let names = [
        "wall.ixs",
        "arc.ixs",
        "wall_rect.ixs",
        "wall_room.ixs",
        "room.ixs",
        "floor.ixs",
        "column.ixs",
        "beam.ixs",
        "grid.ixs",
        "refplane.ixs",
    ];
    let mut bad = vec![];
    for n in names {
        if let Err(d) = assert_parity(&script(n), &[PLAN, ViewKind::View3d]) {
            bad.push(format!("{n}: {d}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn a_script_can_name_its_view() {
    // the same wall drawn through a 3D view named in the script
    let s = parse("view(3d) tool(WA) click(0,0) click(6,0) key(Escape) → walls=1 journal=+1").unwrap();
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let r = ddc_app::interact::script::run(&mut ed, &s);
    assert!(r.ok(), "{:?}", r.failures);
    let w = &ed.walls[0];
    assert!((w.b.x - w.a.x - 6.0).abs() < 1e-9 && w.a.y.abs() < 1e-9);
}

#[test]
fn hosted_plan_eq_3d() {
    // door and window on a wall by the face the 3D ray meets; a free component on the level
    let mut bad = vec![];
    for n in ["door.ixs", "window.ixs", "component.ixs"] {
        if let Err(d) = assert_parity(&script(n), &[PLAN, ViewKind::View3d]) {
            bad.push(format!("{n}: {d}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn all_scripts_all_views() {
    // every .ixs through each view of the mask a fresh editor has (no section view in it yet)
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ix");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".ixs"))
        .collect();
    names.sort();
    let mut bad = vec![];
    for n in &names {
        if let Err(d) = assert_parity(&script(n), &[PLAN, ViewKind::View3d]) {
            bad.push(format!("{n}: {d}"));
        }
    }
    assert!(bad.is_empty(), "{} of {}:\n{}", bad.len(), names.len(), bad.join("\n"));
}

#[test]
fn modify_plan_eq_3d() {
    // one modify session and one grip session in both views: MV (levels 1 and 2), CO, RO, MM, AR, wall end, floor corner
    let names = [
        "modify_move.ixs",
        "modify_move_level2.ixs",
        "modify_copy.ixs",
        "modify_rotate.ixs",
        "modify_mirror.ixs",
        "modify_array.ixs",
        "grips_wall_end.ixs",
        "grips_slab_vertex.ixs",
    ];
    let mut bad = vec![];
    for n in names {
        if let Err(d) = assert_parity(&script(n), &[PLAN, ViewKind::View3d]) {
            bad.push(format!("{n}: {d}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
