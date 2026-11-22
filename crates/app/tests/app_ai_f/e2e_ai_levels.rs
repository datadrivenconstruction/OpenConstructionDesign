// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Levels, automatic rooms and door flips end to end: each call is a draft, its Apply is one
//! undo step, and the model ends exactly where the ribbon's own call leaves a twin of the
//! fixture. Refusals are coded and leave the model untouched.

use crate::{e2e_ai_common, e2e_common};

use ddc_ai::registry::{self, ErrorCode};
use ddc_ai::ToolOutcome;
use ddc_app::editor::Editor;
use ddc_model::ColumnSpec;
use e2e_ai_common::*;
use e2e_common::{assert_same, dump_except};
use serde_json::{json, Value as J};

/// A closed 8 x 6 m room outline on the first level with a door in its south wall, and an
/// empty second level "Upper" at 3 m.
struct Fx {
    ed: Editor,
    ground: i64,
    upper: i64,
    door: i64,
}

fn fixture() -> Fx {
    let mut ed = Editor::new_in_memory().unwrap();
    let ground = ed.level;
    let pts = [(0.0, 0.0), (8.0, 0.0), (8.0, 6.0), (0.0, 6.0)];
    let mut walls = vec![];
    for i in 0..4 {
        walls.push(add_wall(&mut ed, ground, pts[i], pts[(i + 1) % 4]));
    }
    let door = add_door(&mut ed, walls[0], 2.0);
    let upper = add_level(&mut ed, "Upper", 3.0);
    Fx { ed, ground, upper, door }
}

/// Runs one call, applies it and compares the model with a twin changed by `ui`.
fn same_as_ui(name: &str, args: impl Fn(&Fx) -> J, ui: impl Fn(&mut Fx)) -> J {
    let mut fx = fixture();
    let before = live_hash(&fx.ed.model);
    let call = tool(name, args(&fx));
    let (mut s, provider) = run(&mut fx.ed, vec![call]);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 1, "{name}: {results:?}");
    assert!(!results[0].1, "{name}: {}", results[0].0);
    assert_eq!(live_hash(&fx.ed.model), before, "{name}: a draft never touches the model");
    apply_and_check(&mut s, &mut fx.ed);
    let mut twin = fixture();
    assert_eq!((twin.ground, twin.upper, twin.door), (fx.ground, fx.upper, fx.door));
    ui(&mut twin);
    assert_same(
        &dump_except(&fx.ed.model, NOT_MODEL),
        &dump_except(&twin.ed.model, NOT_MODEL),
        name,
    );
    parsed(&results[0].0)
}

fn created(reply: &J) -> Vec<String> {
    reply["created"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn lv(id: i64) -> String {
    format!("level:{id}")
}

#[test]
fn flips_match_the_flip_commands() {
    for (flip, hand) in [("hand", true), ("facing", false)] {
        let args = |f: &Fx| {
            json!({"title": "Flip", "target": {"refs": [format!("door:{}", f.door)]}, "flip": flip})
        };
        same_as_ui("flip_openings", args, |t| {
            ddc_app::grips_opening::flip(&mut t.ed.model, &[t.door], hand).unwrap()
        });
    }
}

#[test]
fn create_level_matches_the_level_panel() {
    let args = |_: &Fx| json!({"title": "Roof level", "name": "Roof", "elevation": 6.5});
    let reply = same_as_ui("create_level", args, |t| {
        t.ed.try_add_level("Roof", 6.5).unwrap();
    });
    let made = created(&reply);
    assert_eq!(made.len(), 1, "{reply}");
    assert!(made[0].starts_with("level:"), "{reply}");
}

#[test]
fn edit_level_matches_the_level_panel() {
    let args = |_: &Fx| json!({"title": "Raise", "level": "Upper", "elevation": 3.3});
    same_as_ui("edit_level", args, |t| t.ed.try_edit_level(t.upper, "Upper", 3.3).unwrap());
    let args = |f: &Fx| json!({"title": "Rename", "level": lv(f.upper), "name": "First"});
    same_as_ui("edit_level", args, |t| t.ed.try_edit_level(t.upper, "First", 3.0).unwrap());
}

#[test]
fn delete_level_matches_the_level_panel() {
    let args = |f: &Fx| json!({"title": "Drop", "level": lv(f.upper)});
    same_as_ui("delete_level", args, |t| assert!(t.ed.delete_level(t.upper)));
}

#[test]
fn place_rooms_matches_the_automatic_room_command() {
    let args = |f: &Fx| json!({"title": "Rooms", "level": lv(f.ground)});
    let reply = same_as_ui("place_rooms", args, |t| {
        t.ed.level = t.ground;
        let rep = ddc_app::tools::room::place_all(&mut t.ed).unwrap();
        assert_eq!(rep.placed, 1);
    });
    let made = created(&reply);
    assert_eq!(made.len(), 1, "{reply}");
    assert!(made[0].starts_with("room:"), "{reply}");
}

/// Two columns on the first level, the first with a footing already: one footing is placed.
#[test]
fn place_footings_matches_the_footings_under_columns_command() {
    let columns = |ed: &mut Editor, lv: i64| {
        let ct = ed.model.add_catalog_member_type("R 400x400").unwrap();
        for at in [(0.0, 0.0), (8.0, 0.0)] {
            ed.model
                .add_column(&ColumnSpec {
                    level: lv,
                    top_level: None,
                    member_type: ct,
                    structural: true,
                    at,
                    base_offset: 0.0,
                    top_offset: 3.0,
                    rotation: 0.0,
                    top_at: None,
                })
                .unwrap();
        }
        let (cols, spec) = ddc_app::tools::structx::footings_plan(ed, lv);
        ed.model.footings_under_columns(&cols[..1], &spec).unwrap();
        ed.reload().unwrap();
    };
    let mut fx = fixture();
    columns(&mut fx.ed, fx.ground);
    let before = live_hash(&fx.ed.model);
    let args = json!({"title": "Footings", "level": lv(fx.ground)});
    let (mut s, provider) = run(&mut fx.ed, vec![tool("place_footings", args.clone())]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    assert_eq!(created(&parsed(&results[0].0)).len(), 1, "{}", results[0].0);
    assert_eq!(live_hash(&fx.ed.model), before, "a draft never touches the model");
    apply_and_check(&mut s, &mut fx.ed);
    let mut twin = fixture();
    columns(&mut twin.ed, twin.ground);
    let (cols, spec) = ddc_app::tools::structx::footings_plan(&twin.ed, twin.ground);
    assert_eq!(cols.len(), 1);
    twin.ed.model.footings_under_columns(&cols, &spec).unwrap();
    let (a, b) = (dump_except(&fx.ed.model, NOT_MODEL), dump_except(&twin.ed.model, NOT_MODEL));
    assert_same(&a, &b, "place_footings");
    // every column has its footing now: the same call changes nothing
    let (_s, provider) = run(&mut fx.ed, vec![tool("place_footings", args)]);
    let e = error_of(&tool_results(&provider)[0].0);
    assert_eq!(e.code, ErrorCode::NoEffect);
}

/// The terrain the terrain tool makes from the same points: earth, its thickness, its name.
fn ui_terrain(t: &mut Fx, points: Vec<(f64, f64, f64)>, boundary: Vec<(f64, f64)>) {
    let n = t.ed.model.toposolids().unwrap().len() + 1;
    let spec = ddc_model::TopoSpec {
        name: format!("{} {n}", t.ed.topo.t("topo.default_name")),
        thickness: t.ed.topo.opts.thickness,
        material: ddc_app::topo::EARTH.to_string(),
        points,
        breaklines: vec![],
        boundary,
    };
    t.ed.model.add_toposolid(&spec).unwrap();
}

#[test]
fn create_terrain_matches_the_terrain_tool() {
    let pts = vec![(-5.0, -5.0, 0.0), (20.0, -5.0, 1.5), (20.0, 15.0, 2.5), (-5.0, 15.0, 0.5), (7.0, 5.0, 1.2)];
    let p = pts.clone();
    let args = move |_: &Fx| {
        let list: Vec<J> = p.iter().map(|q| json!([q.0, q.1, q.2])).collect();
        json!({"title": "Ground", "points": list})
    };
    let reply = same_as_ui("create_terrain", args, |t| ui_terrain(t, pts.clone(), vec![]));
    let made = created(&reply);
    assert_eq!(made.len(), 1, "{reply}");
    assert!(made[0].starts_with("toposolid:"), "{reply}");
}

/// A plane over an outline: the corners lie on it, `z` at the centre, falling `slope` %
/// towards `downhill`; here 10 % to +x over 20 m, so 1 m either side of the centre.
#[test]
fn create_terrain_lays_a_sloped_plane_over_an_outline() {
    let ring = vec![(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (0.0, 10.0)];
    let r = ring.clone();
    let args = move |_: &Fx| {
        let b: Vec<J> = r.iter().map(|q| json!([q.0, q.1])).collect();
        json!({"title": "Slope", "boundary": b, "z": 50.0, "slope": 10.0, "downhill": 0.0})
    };
    let pts = vec![(0.0, 0.0, 51.0), (20.0, 0.0, 49.0), (20.0, 10.0, 49.0), (0.0, 10.0, 51.0)];
    same_as_ui("create_terrain", args, |t| ui_terrain(t, pts.clone(), ring.clone()));
}

fn error_of(text: &str) -> ddc_ai::registry::CmdError {
    assert!(text.starts_with("{\"untrusted\":\"data\""), "{text}");
    registry::error_of(&ToolOutcome::error(text)).expect("an error")
}

/// Refusals: the code, the argument where one is at fault, no item, the model untouched.
#[test]
fn refusals_are_coded_and_leave_the_model_alone() {
    let ids = fixture();
    let (ground, upper) = (lv(ids.ground), lv(ids.upper));
    let cases: Vec<(&str, J, ErrorCode, Option<&str>)> = vec![
        ("create_level", json!({"title": "x", "name": "Upper", "elevation": 9.0}),
            ErrorCode::InvalidArgs, Some("args.name")),
        ("create_level", json!({"title": "x", "name": "  ", "elevation": 9.0}),
            ErrorCode::InvalidArgs, Some("args.name")),
        ("edit_level", json!({"title": "x", "level": upper}),
            ErrorCode::InvalidArgs, Some("args.name")),
        ("edit_level", json!({"title": "x", "level": upper, "elevation": 3.0}),
            ErrorCode::NoEffect, None),
        ("edit_level", json!({"title": "x", "level": "Nowhere", "name": "A"}),
            ErrorCode::NotFound, None),
        ("delete_level", json!({"title": "x", "level": "level:999"}),
            ErrorCode::NotFound, None),
        ("place_rooms", json!({"title": "x", "level": ground, "height": -1.0}),
            ErrorCode::InvalidArgs, Some("args.height")),
        ("place_rooms", json!({"title": "x", "level": upper}),
            ErrorCode::NoEffect, None),
        ("create_terrain", json!({"title": "x", "points": [[0.0, 0.0, 0.0], [5.0, 0.0, 1.0]]}),
            ErrorCode::InvalidArgs, Some("args.points")),
        ("create_terrain", json!({"title": "x", "boundary": [[0.0, 0.0], [5.0, 0.0], [5.0, 5.0]], "slope": 150.0}),
            ErrorCode::InvalidArgs, Some("args.slope")),
    ];
    for (name, a, code, path) in cases {
        let mut fx = fixture();
        assert_eq!((fx.ground, fx.upper), (ids.ground, ids.upper));
        let before = live_hash(&fx.ed.model);
        let (s, provider) = run(&mut fx.ed, vec![tool(name, a.clone())]);
        let results = tool_results(&provider);
        assert!(results[0].1, "{name} {a}: {results:?}");
        let e = error_of(&results[0].0);
        assert_eq!(e.code, code, "{name} {a}: {}", results[0].0);
        assert_eq!(e.path.as_deref(), path, "{name} {a}: {}", results[0].0);
        assert!(s.plan().is_none_or(|p| p.items.is_empty()), "{name}: no item stays");
        assert_eq!(live_hash(&fx.ed.model), before, "{name}");
    }
}

/// A wall cannot be flipped and the last level cannot be deleted: refused, nothing changes.
#[test]
fn wrong_targets_are_refused() {
    let mut fx = fixture();
    let before = live_hash(&fx.ed.model);
    let flip = json!({"title": "x", "target": {"refs": ["wall:1"]}, "flip": "hand"});
    let drop_upper = json!({"title": "x", "level": lv(fx.upper)});
    let drop_ground = json!({"title": "x", "level": lv(fx.ground)});
    let calls = vec![
        tool("flip_openings", flip),
        tool("delete_level", drop_upper),
        tool("delete_level", drop_ground),
    ];
    let (s, provider) = run(&mut fx.ed, calls);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results[0].1, "a wall has no hinge: {}", results[0].0);
    assert!(!results[1].1, "{}", results[1].0);
    assert!(results[2].1, "the last level stays: {}", results[2].0);
    assert!(results[2].0.contains("last level"), "{}", results[2].0);
    assert_eq!(s.plan().map(|p| p.items.len()), Some(1));
    assert_eq!(live_hash(&fx.ed.model), before);
}

/// A wrong argument type never reaches the handler: the gate codes it.
#[test]
fn schema_violations_are_refused_by_the_gate() {
    for (name, args) in [
        ("flip_openings", json!({"title": "x", "target": {"refs": ["door:1"]}, "flip": "up"})),
        ("create_level", json!({"title": "x", "name": "A", "elevation": "high"})),
        ("delete_level", json!({"title": "x", "level": 2})),
    ] {
        let mut fx = fixture();
        let (_s, provider) = run(&mut fx.ed, vec![tool(name, args)]);
        let results = tool_results(&provider);
        let e = error_of(&results[0].0);
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{name}: {}", results[0].0);
    }
}

/// A column with a top level stops at that level: no storey-height offset on top, and the
/// plan of a storey it passes shows it.
#[test]
fn column_with_a_top_level_stops_at_that_level() {
    let mut fx = fixture();
    let mid = add_level(&mut fx.ed, "Mid", 1.5);
    fx.ed.model.add_catalog_member_type("R 400x400").unwrap();
    let item = json!({"at": [4.0, 3.0], "level": fx.ground, "top_level": fx.upper,
        "structural": true, "type": "R 400x400"});
    let args = json!({"title": "Column", "cat": "column", "items": [item]});
    let (mut s, provider) = run(&mut fx.ed, vec![tool("create_elements", args)]);
    let results = tool_results(&provider);
    assert!(!results[0].1, "{}", results[0].0);
    apply_and_check(&mut s, &mut fx.ed);
    let id: i64 = fx
        .ed
        .model
        .sql()
        .query_row("SELECT MAX(id) FROM col", [], |r| r.get(0))
        .unwrap();
    let c = fx.ed.model.column(id).unwrap();
    assert_eq!(c.top_level, Some(fx.upper));
    assert_eq!(c.top_offset, 0.0, "the top level is the top");
    let mesh = fx.ed.model.member_mesh("column", id).unwrap();
    let zmax = mesh.positions.iter().map(|p| p.z).fold(f64::MIN, f64::max);
    assert!((zmax - 3.0).abs() < 1e-6, "column top {zmax} != upper level 3.0");
    fx.ed.set_level(mid);
    assert!(fx.ed.bodies.iter().all(|b| b.id != id), "3D takes it from its own level");
    assert!(
        fx.ed.bodies_through.iter().any(|b| b.id == id),
        "the plan of a storey the column passes shows it"
    );
    fx.ed.set_level(fx.upper);
    assert!(fx.ed.bodies_through.iter().all(|b| b.id != id), "it ends at the upper level");
}

/// G29: a six-level car park as a model plans it in one turn — five `create_level` calls, then
/// floors and a column grid on those planned levels by name — applies with 6 levels, 6 decks
/// and 36 columns from the ground to the top deck.
#[test]
fn planned_levels_carry_their_floors_and_columns() {
    let mut ed = Editor::new_in_memory().unwrap();
    let ground = ed.level;
    let names = ["P1", "P2", "P3", "P4", "P5"];
    let mut calls = vec![];
    for (i, n) in names.iter().enumerate() {
        calls.push(tool("create_level", json!({"title": n, "name": n, "elevation": 3.0 * (i as f64 + 1.0)})));
    }
    let outer = json!([[0, 0], [40, 0], [40, 40], [0, 40]]);
    let mut floors = vec![json!({"outer": outer, "level": ground})];
    floors.extend(names.iter().map(|n| json!({"outer": outer, "level": n})));
    calls.push(tool("create_elements", json!({"title": "Decks", "cat": "floor", "items": floors})));
    let cols: Vec<J> = (0..36)
        .map(|k| json!({"at": [8.0 * (k % 6) as f64, 8.0 * (k / 6) as f64], "level": ground, "top_level": "P5"}))
        .collect();
    calls.push(tool("create_elements", json!({"title": "Columns", "cat": "column", "items": cols})));
    let (mut s, provider) = run(&mut ed, calls);
    for (text, err) in tool_results(&provider) {
        assert!(!err, "{text}");
    }
    apply_and_check(&mut s, &mut ed);
    let n = |t: &str| -> i64 { ed.model.sql().query_row(&format!("SELECT count(*) FROM {t}"), [], |r| r.get(0)).unwrap() };
    assert_eq!((n("level"), n("slab"), n("col")), (6, 6, 36));
}
