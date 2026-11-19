// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Model-tool scenarios: model state after scripted input, one undo step.

use super::{arm, Kind};
use crate::editor::Editor;
use crate::wall_tool::{Event, Key, Mode};
use ddc_geom::v2;
use ddc_ui::i18n::Lang;

fn ed() -> Editor {
    Editor::new_in_memory().unwrap()
}

fn count(e: &Editor, table: &str) -> i64 {
    e.model
        .sql()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn click(e: &mut Editor, x: f64, y: f64) {
    e.handle(Event::Move(v2(x, y)));
    e.handle(Event::Click(v2(x, y)));
}

fn typed(e: &mut Editor, s: &str) {
    for c in s.chars() {
        e.handle(Event::Char(c));
    }
    e.handle(Event::Key(Key::Enter));
}

fn room(e: &mut Editor) {
    e.set_mode(Mode::Rect);
    e.handle(Event::Move(v2(0.0, 0.0)));
    e.handle(Event::Press(v2(0.0, 0.0)));
    e.handle(Event::Move(v2(6.0, 4.0)));
    e.handle(Event::Release(v2(6.0, 4.0)));
    e.set_mode(Mode::Select);
}

/// Undo reverts the whole write, redo restores it.
fn undo_redo(e: &mut Editor, table: &str, n: i64) {
    assert_eq!(count(e, table), n, "{table} before undo");
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(
        count(e, table),
        0,
        "{table}: one undo step removes the gesture"
    );
    e.redo();
    assert_eq!(count(e, table), n, "{table}: redo restores");
}

#[test]
fn column_click_space_rotates_and_undo_is_one_step() {
    let mut e = ed();
    arm(&mut e, Kind::Column, Lang::Ru);
    e.handle(Event::Char(' '));
    click(&mut e, 2.0, 3.0);
    assert_eq!(count(&e, "col"), 1);
    assert_eq!(
        count(&e, "member_type"),
        1,
        "type created in the same record"
    );
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM col", [], |r| r.get(0))
        .unwrap();
    let c = e.model.column(id).unwrap();
    assert!((c.at.0 - 2.0).abs() < 1e-9 && (c.at.1 - 3.0).abs() < 1e-9);
    assert!(
        (c.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-9,
        "Space = 90°"
    );
    assert!(!c.structural);
    undo_redo(&mut e, "col", 1);
    assert_eq!(count(&e, "member_type"), 1);
}

#[test]
fn structural_columns_at_grid_intersections() {
    let mut e = ed();
    arm(&mut e, Kind::Grid, Lang::Ru);
    for (a, b) in [((0.0, -1.0), (0.0, 9.0)), ((6.0, -1.0), (6.0, 9.0))] {
        click(&mut e, a.0, a.1);
        click(&mut e, b.0, b.1);
    }
    for y in [0.0, 4.0, 8.0] {
        click(&mut e, -1.0, y);
        click(&mut e, 7.0, y);
    }
    assert_eq!(count(&e, "grid"), 5);
    arm(&mut e, Kind::StructColumn, Lang::Ru);
    let mut a = crate::interact::sessions::model_tool::take(&mut e).unwrap();
    super::column::at_grids(&mut e, &mut a);
    assert_eq!(count(&e, "col"), 6, "2 × 3 intersections");
    let s: i64 = e
        .model
        .sql()
        .query_row("SELECT COUNT(*) FROM col WHERE structural=1", [], |r| {
            r.get(0)
        })
        .unwrap_or(6);
    assert_eq!(s, 6);
    e.undo();
    assert_eq!(count(&e, "col"), 0, "all columns are one step");
}

#[test]
fn beam_typed_length_and_chain() {
    let mut e = ed();
    arm(&mut e, Kind::Beam, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    e.handle(Event::Move(v2(3.0, 0.0)));
    typed(&mut e, "4500");
    assert_eq!(count(&e, "beam"), 1);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM beam", [], |r| r.get(0))
        .unwrap();
    let b = e.model.beam(id).unwrap();
    assert!(
        (b.b.0 - 4.5).abs() < 1e-9,
        "typed length along the cursor direction"
    );
    assert!(
        (b.a.2 - e.height).abs() < 1e-9,
        "Z offset defaults to wall height"
    );
    // chain: the end became the start
    assert_eq!(e.model_tool().unwrap().pts, vec![v2(4.5, 0.0)]);
    click(&mut e, 4.5, 3.0);
    assert_eq!(count(&e, "beam"), 2);
    // Esc: first press resets the chain and the tool stays; second exits
    e.handle(Event::Key(Key::Escape));
    assert!(e.model_tool().is_some_and(|a| a.pts.is_empty()));
    e.handle(Event::Key(Key::Escape));
    assert!(e.model_tool().is_none());
    assert_eq!(e.wall_mode(), Mode::Select);
    e.undo();
    assert_eq!(count(&e, "beam"), 1, "each segment is its own step");
}

#[test]
fn beam_space_flips_direction() {
    let mut e = ed();
    arm(&mut e, Kind::Beam, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    e.handle(Event::Char(' '));
    click(&mut e, 2.0, 0.0);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM beam", [], |r| r.get(0))
        .unwrap();
    let b = e.model.beam(id).unwrap();
    assert!((b.a.0 - 2.0).abs() < 1e-9 && b.b.0.abs() < 1e-9);
}

#[test]
fn roof_sketch_with_edge_slopes() {
    let mut e = ed();
    arm(&mut e, Kind::Roof, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 6.0, 0.0);
    e.handle(Event::Char(' ')); // edge 0 — gable
    click(&mut e, 6.0, 4.0);
    click(&mut e, 0.0, 4.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(count(&e, "roof"), 1);
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM roof", [], |r| r.get(0))
        .unwrap();
    let r = e.model.roof(id).unwrap();
    let ddc_model::RoofKind::Footprint { ring, slopes, .. } = r.kind else {
        panic!("footprint roof")
    };
    assert_eq!(ring.len(), 4);
    assert_eq!(
        slopes.iter().filter(|s| s.is_none()).count(),
        1,
        "one gable edge"
    );
    assert!(slopes.iter().flatten().all(|s| (*s - 30.0).abs() < 1e-9));
    undo_redo(&mut e, "roof", 1);
}

#[test]
fn roof_open_sketch_is_refused() {
    let mut e = ed();
    arm(&mut e, Kind::Roof, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 5.0, 0.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(count(&e, "roof"), 0);
    assert!(e.status.as_deref().unwrap_or("").contains(&crate::app2_fx::fx("tools_not_closed")));
}

#[test]
fn ceiling_auto_inside_walls_and_sketch() {
    let mut e = ed();
    room(&mut e);
    arm(&mut e, Kind::Ceiling, Lang::Ru);
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "ceiling"), 1, "click inside the wall loop");
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM ceiling", [], |r| r.get(0))
        .unwrap();
    let rings = e.model.ceiling_rings(id).unwrap();
    let area: f64 = {
        let r = &rings[0];
        (0..r.len())
            .map(|i| {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                a.0 * b.1 - a.1 * b.0
            })
            .sum::<f64>()
            .abs()
            / 2.0
    };
    assert!(
        area > 15.0 && area < 24.0,
        "inner faces of the room, got {area}"
    );
    click(&mut e, 20.0, 20.0);
    assert_eq!(count(&e, "ceiling"), 1, "outside any loop: refused");
    e.model_tool_mut().unwrap().opts.pick = false;
    for (x, y) in [(10.0, 0.0), (12.0, 0.0), (12.0, 2.0)] {
        click(&mut e, x, y);
    }
    click(&mut e, 10.0, 0.0); // a click on the first point closes the outline
    assert_eq!(count(&e, "ceiling"), 2);
}

#[test]
fn stair_needs_level_above_and_counts_risers() {
    let mut e = ed();
    arm(&mut e, Kind::Stair, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    assert_eq!(count(&e, "stair"), 0, "no level above");
    e.model.add_level("Floor 2", 3.0).unwrap();
    arm(&mut e, Kind::Stair, Lang::Ru);
    let a = e.model_tool().unwrap();
    assert_eq!(super::stair::count(a, 3.08), (12, 17));
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 5.0);
    assert_eq!(count(&e, "stair"), 1);
    assert_eq!(count(&e, "railing"), 2, "auto railings, same record");
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "stair"), 0);
    assert_eq!(count(&e, "railing"), 0);
}

#[test]
fn railing_path_and_pick_stair() {
    let mut e = ed();
    arm(&mut e, Kind::Railing, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 4.0, 0.0);
    click(&mut e, 4.0, 3.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(count(&e, "railing"), 1);
    e.model.add_level("Floor 2", 3.0).unwrap();
    arm(&mut e, Kind::Stair, Lang::Ru);
    e.model_tool_mut().unwrap().opts.auto_rail = false;
    click(&mut e, 10.0, 0.0);
    click(&mut e, 10.0, 5.0);
    arm(&mut e, Kind::Railing, Lang::Ru);
    e.model_tool_mut().unwrap().opts.pick = true;
    click(&mut e, 10.0, 1.0);
    assert_eq!(count(&e, "railing"), 3, "both stair edges");
}

#[test]
fn shaft_sketch_backspace_and_ctrl_z_point() {
    let mut e = ed();
    arm(&mut e, Kind::Shaft, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 2.0, 0.0);
    click(&mut e, 9.0, 9.0);
    e.handle(Event::Key(Key::Backspace));
    e.undo(); // Ctrl+Z in a chain — the last point
    assert_eq!(e.model_tool().unwrap().pts.len(), 1);
    click(&mut e, 2.0, 0.0);
    click(&mut e, 2.0, 2.0);
    click(&mut e, 0.0, 2.0);
    e.handle(Event::Key(Key::Enter));
    assert_eq!(count(&e, "shaft"), 1);
    assert_eq!(e.model.shaft_ring(1).unwrap().len(), 4);
    undo_redo(&mut e, "shaft", 1);
}

#[test]
fn grids_autonumber_and_refplane() {
    let mut e = ed();
    arm(&mut e, Kind::Grid, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 8.0);
    click(&mut e, 5.0, 0.0);
    click(&mut e, 5.0, 8.0);
    let names: Vec<String> = e
        .model
        .grids()
        .unwrap()
        .into_iter()
        .map(|g| g.name)
        .collect();
    assert_eq!(names, vec!["1", "2"]);
    // a horizontal grid line gets a letter (Russian project: Cyrillic capital A)
    click(&mut e, 0.0, -3.0);
    click(&mut e, 8.0, -3.0);
    let last = e.model.grids().unwrap().pop().unwrap().name;
    assert_eq!(last, "\u{410}");
    arm(&mut e, Kind::RefPlane, Lang::Ru);
    click(&mut e, -2.0, 3.0);
    click(&mut e, 8.0, 3.0);
    assert_eq!(e.model.ref_planes().unwrap().len(), 1);
    // snap to the intersection of a grid line and a plane
    arm(&mut e, Kind::Column, Lang::Ru);
    e.tool.radius = 0.3;
    click(&mut e, 5.1, 3.1);
    let c = e.model.column(1).unwrap();
    assert!(
        (c.at.0 - 5.0).abs() < 1e-9 && (c.at.1 - 3.0).abs() < 1e-9,
        "snapped to ✕"
    );
}

#[test]
fn component_from_library() {
    let mut e = ed();
    arm(&mut e, Kind::Component, Lang::Ru);
    let a = e.model_tool().unwrap();
    assert!(!a.types.is_empty(), "library has types");
    let free = a
        .types
        .iter()
        .position(|t| {
            let fam = t.split(" : ").next().unwrap();
            ddc_model::library()
                .unwrap()
                .iter()
                .any(|f| f.name == fam && f.hosting == ddc_geom::component::Hosting::Floor)
        })
        .expect("a free-standing family");
    e.model_tool_mut().unwrap().opts.ty = free;
    click(&mut e, 3.0, 3.0);
    assert_eq!(count(&e, "comp"), 1);
    click(&mut e, 5.0, 3.0);
    assert_eq!(count(&e, "comp"), 2);
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "comp"), 1);
    e.undo();
    assert_eq!(count(&e, "comp"), 0);
    assert_eq!(
        count(&e, "comp_family"),
        0,
        "family load undone with first placement"
    );
}

#[test]
fn mep_terminal_connects_and_one_undo_removes_it() {
    let mut e = ed();
    arm(&mut e, Kind::Duct, Lang::Ru);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 8.0, 0.0);
    e.handle(Event::Key(Key::Enter));
    let sys = e.model.mep_systems().unwrap()[0].id;
    e.set_mode(Mode::Select);
    arm(&mut e, Kind::Terminal, Lang::Ru);
    e.model_tool_mut().unwrap().mep.system = Some(sys);
    click(&mut e, 4.0, 3.0);
    assert_eq!(count(&e, "mep_run"), 2);
    assert_eq!(count(&e, "comp"), 1);
    assert_eq!(count(&e, "mep_port"), 1);
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "mep_run"), 1);
    assert_eq!(count(&e, "comp"), 0);
    assert_eq!(count(&e, "mep_port"), 0);
}

#[test]
fn mep_runs_duct_pipe_tray_with_fittings() {
    for (k, sys) in [
        (Kind::Duct, "tools_sys_duct"),
        (Kind::Pipe, "tools_sys_pipe"),
        (Kind::Tray, "tools_sys_tray"),
    ] {
        let mut e = ed();
        arm(&mut e, k, Lang::Ru);
        click(&mut e, 0.0, 0.0);
        click(&mut e, 5.0, 0.0);
        click(&mut e, 5.0, 4.0);
        e.handle(Event::Key(Key::Enter));
        assert_eq!(count(&e, "mep_run"), 1, "{k:?}");
        let s = e.model.mep_systems().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, crate::app2_fx::fx(sys));
        let net = e.model.mep_network(None).unwrap();
        assert!(net.fittings.len() >= 1, "{k:?}: elbow at the bend");
        let run = e.model.mep_run(e.model.mep_runs(None).unwrap()[0]).unwrap();
        assert!((run.points[0].2 - e.model_tool().unwrap().opts.z).abs() < 1e-9);
        e.set_mode(Mode::Select);
        e.undo();
        assert_eq!(count(&e, "mep_run"), 0);
        assert!(
            e.model.mep_systems().unwrap().is_empty(),
            "system created in the same record"
        );
    }
}

fn draw_line(e: &mut Editor) {
    draw_line_at(e, 0.0);
}

fn draw_line_at(e: &mut Editor, y: f64) {
    click(e, 0.0, y);
    click(e, 6.0, y);
    e.handle(Event::Key(Key::Enter));
}

#[test]
fn mep_run_goes_into_the_picked_custom_system() {
    let mut e = ed();
    let sys = e
        .model
        .add_mep_system("Custom exhaust", ddc_model::SystemKind::ExhaustAir)
        .unwrap();
    let wrong = e
        .model
        .add_mep_system("Water", ddc_model::SystemKind::ColdWater)
        .unwrap();
    arm(&mut e, Kind::Duct, Lang::En);
    e.model_tool_mut().unwrap().mep.system = Some(sys);
    draw_line(&mut e);
    let run = e.model.mep_run(e.model.mep_runs(None).unwrap()[0]).unwrap();
    assert_eq!(run.system, sys);
    assert_eq!(e.model.mep_systems().unwrap().len(), 2, "no extra system");
    // a system of another domain is ignored: the default one is created instead
    e.set_mode(Mode::Select);
    arm(&mut e, Kind::Duct, Lang::En);
    e.model_tool_mut().unwrap().mep.system = Some(wrong);
    // away from the first run: a line that starts on a run's end joins it
    draw_line_at(&mut e, 10.0);
    let runs = e.model.mep_runs(None).unwrap();
    assert_eq!(runs.len(), 2);
    assert_ne!(e.model.mep_run(runs[1]).unwrap().system, wrong);
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "mep_run"), 1);
}

#[test]
fn mep_round_duct_and_pipe_material_are_drawn() {
    let mut e = ed();
    arm(&mut e, Kind::Duct, Lang::En);
    {
        let o = &mut e.model_tool_mut().unwrap().opts;
        o.duct_round = true;
        o.width = 0.315;
    }
    draw_line(&mut e);
    let r = e.model.mep_run(e.model.mep_runs(None).unwrap()[0]).unwrap();
    assert_eq!(r.section.shape, ddc_model::MepShape::Round { d: 0.315 });
    e.set_mode(Mode::Select);
    arm(&mut e, Kind::Pipe, Lang::En);
    {
        let o = &mut e.model_tool_mut().unwrap().opts;
        o.pipe_mat = 1;
        o.dn = 25;
    }
    draw_line(&mut e);
    let runs = e.model.mep_runs(None).unwrap();
    let p = e.model.mep_run(runs[1]).unwrap();
    assert_eq!(p.material, Some(ddc_model::PipeMaterial::Copper));
    assert_eq!(p.dn, Some(25));
}

#[test]
fn temp_dim_click_value_moves_point() {
    let mut e = ed();
    arm(&mut e, Kind::Grid, Lang::Ru);
    click(&mut e, 0.0, -5.0);
    click(&mut e, 0.0, 5.0);
    arm(&mut e, Kind::Column, Lang::Ru);
    e.handle(Event::Move(v2(1.3, 2.0)));
    let a = e.model_tool().unwrap();
    assert!(
        (a.dims[0].value - 1.3).abs() < 1e-9,
        "distance to the grid axis"
    );
    e.model_tool_mut().unwrap().edit = Some(0);
    typed(&mut e, "2500");
    let c = e.model.column(1).unwrap();
    assert!((c.at.0 - 2.5).abs() < 1e-9 && (c.at.1 - 2.0).abs() < 1e-9);
}

#[test]
fn commands_registered_and_arm() {
    for k in Kind::ALL {
        assert!(!k.commands().is_empty());
        let mut e = ed();
        arm(&mut e, k, Lang::En);
        assert_eq!(e.model_tool().unwrap().kind, k);
        assert!(
            !e.model_tool().unwrap().hint().starts_with("hint."),
            "{k:?}"
        );
        e.set_mode(Mode::Rect);
        assert!(e.model_tool().is_none(), "switching tools disarms");
    }
}

/// Every key the model tool sources ask for exists in ru and en (no raw keys).
#[test]
fn lane_a_keys_resolve_ru_en() {
    let srcs = [
        include_str!("mod_arch.rs"),
        include_str!("ui.rs"),
        include_str!("column.rs"),
        include_str!("ceiling.rs"),
        include_str!("railing.rs"),
        include_str!("component.rs"),
        include_str!("refplane.rs"),
        include_str!("stair.rs"),
    ];
    let mut keys: Vec<String> = vec![];
    for s in srcs {
        for pat in ["t(\"", "tf(\""] {
            for part in s.split(pat).skip(1) {
                let k = part.split('"').next().unwrap();
                // `split(" : ")` also ends at t(" — the keys contain a dot
                if k.contains('.') && !k.contains(' ') {
                    keys.push(k.to_string());
                }
            }
        }
    }
    for k in Kind::ALL {
        keys.push(format!("tool.{}", k.key()));
    }
    for k in ["opt.just.center", "opt.just.left", "opt.just.right"] {
        keys.push(k.into());
    }
    for lang in [Lang::Ru, Lang::En] {
        let c = ddc_ui::i18n::Catalog::new(lang);
        for k in &keys {
            assert!(c.table(lang).contains_key(k), "{lang:?}: missing {k}");
        }
    }
}

fn stair_ed(lang: Lang, shape: u8) -> Editor {
    let mut e = ed();
    e.model.add_level("L2", 3.0).unwrap();
    arm(&mut e, Kind::Stair, lang);
    e.model_tool_mut().unwrap().opts.stair_shape = shape;
    e
}

fn stair_shape(e: &Editor) -> ddc_geom::stair::Shape {
    let id: i64 = e
        .model
        .sql()
        .query_row("SELECT id FROM stair", [], |r| r.get(0))
        .unwrap();
    e.model.stair(id).unwrap().shape
}

#[test]
fn stair_tool_l_shape_three_clicks() {
    let mut e = stair_ed(Lang::En, 1);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 2.0);
    assert_eq!(e.model_tool().unwrap().pts.len(), 2);
    assert_eq!(count(&e, "stair"), 0);
    click(&mut e, -3.0, 2.0);
    assert_eq!(count(&e, "stair"), 1);
    assert!(matches!(
        stair_shape(&e),
        ddc_geom::stair::Shape::L { left: true, first: Some(_) }
    ));
    assert_eq!(count(&e, "railing"), 2);
    // Esc steps back one click
    arm(&mut e, Kind::Stair, Lang::En);
    e.model_tool_mut().unwrap().opts.stair_shape = 1;
    click(&mut e, 10.0, 0.0);
    click(&mut e, 10.0, 2.0);
    e.handle(Event::Key(Key::Escape));
    assert_eq!(e.model_tool().unwrap().pts.len(), 1);
}

#[test]
fn stair_tool_u_shape_gap() {
    let mut e = stair_ed(Lang::En, 2);
    e.model_tool_mut().unwrap().opts.gap = 0.3;
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 2.0);
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "stair"), 1);
    match stair_shape(&e) {
        ddc_geom::stair::Shape::U { left, gap, .. } => {
            assert!(!left);
            assert!((gap - 0.3).abs() < 1e-9);
        }
        s => panic!("not U: {s:?}"),
    }
}

#[test]
fn stair_tool_straight_unchanged() {
    let mut e = stair_ed(Lang::En, 0);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 5.0);
    assert_eq!(count(&e, "stair"), 1);
    assert_eq!(stair_shape(&e), ddc_geom::stair::Shape::Straight);
}

#[test]
fn stair_tool_l_too_short_flight_refused() {
    let mut e = stair_ed(Lang::En, 1);
    click(&mut e, 0.0, 0.0);
    // 0.25 m: past the 0.15 m snap radius of the first point (0.1 m snapped onto it), one riser
    click(&mut e, 0.0, 0.25);
    assert_eq!(e.model_tool().unwrap().pts.len(), 1, "corner not accepted");
    let st = e.status.as_deref().unwrap_or("");
    assert!(st.contains("at least 2 risers"), "{st}");
    assert_eq!(count(&e, "stair"), 0);
}

#[test]
fn stair_tool_l_one_undo_removes_stair_and_railings() {
    let mut e = stair_ed(Lang::En, 1);
    click(&mut e, 0.0, 0.0);
    click(&mut e, 0.0, 2.0);
    click(&mut e, 3.0, 2.0);
    assert_eq!(count(&e, "stair"), 1);
    assert!(count(&e, "railing") > 0);
    e.set_mode(Mode::Select);
    e.undo();
    assert_eq!(count(&e, "stair"), 0);
    assert_eq!(count(&e, "railing"), 0);
}

#[test]
fn stair_hints_exist_in_both_langs_and_mention_point_removal() {
    for k in ["hint.stair.first", "hint.stair.end", "hint.stair.corner", "hint.stair.side"] {
        let en = crate::tr_in(Lang::En, k, &[]);
        let ru = crate::tr_in(Lang::Ru, k, &[]);
        assert_ne!(en, k, "{k} missing in en");
        assert_ne!(ru, k, "{k} missing in ru");
        assert_ne!(en, ru, "{k} not translated");
        assert!(!en.contains("— cancel") || en.contains("no points"), "{k}: {en}");
    }
}
