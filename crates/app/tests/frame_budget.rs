// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Idle frames cost nothing: with the model unchanged, a whole window frame (properties panel,
//! project browser, workbook window, AI panel, status bar) runs no SQL statement, and a hover
//! over the same spot runs none either. Statements are counted by the model's `test-hooks`
//! counter. Run with `cargo test -p ddc-app --features test-hooks --test frame_budget`; the
//! 50k-element variant is `-- --ignored` (release build advised).
#![cfg(feature = "test-hooks")]

use ddc_app::editor::{Editor, Sel};
use ddc_app::shell::xlsx_win::XlsxPreview;
use ddc_app::shell::EditorApp;
use ddc_app::update::{self, Freq};
use ddc_model::{query_count, WallSpec};

const CELL: f64 = 6.0;

/// Own user-data folder and no update check: the test touches nothing of the real profile.
fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_frame_budget_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
    });
    update::set_freq(Freq::Never);
}

/// A k x k grid of rooms: 2 * k * (k + 1) walls.
fn seeded(k: usize) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let w = |a: (f64, f64), b: (f64, f64)| WallSpec {
        a,
        b,
        level,
        base_offset: 0.0,
        height: 3.5,
        wall_type: wt,
        offset0: None,
    };
    let mut ws = Vec::new();
    for i in 0..=k {
        let c = i as f64 * CELL;
        for j in 0..k {
            let (a, b) = (j as f64 * CELL, (j + 1) as f64 * CELL);
            ws.push(w((a, c), (b, c)));
            ws.push(w((c, a), (c, b)));
        }
    }
    ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
    ed
}

/// One window frame; returns the SQL statements it ran.
fn frame(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64, events: Vec<egui::Event>) -> u64 {
    *t += 0.016;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        time: Some(*t),
        events,
        ..Default::default()
    };
    query_count::take_log();
    let before = query_count::count();
    let _ = ctx.run(input, |c| a.ui(c, None));
    query_count::count() - before
}

fn idle(what: &str, n: u64) {
    assert!(
        n == 0,
        "{what}: {n} SQL statements in an idle frame; first ones: {:#?}",
        query_count::take_log()
    );
}

fn run(k: usize) {
    isolate();
    let ru = ddc_app::ui_is_ru();
    let mut a = EditorApp::new(seeded(k), None, "test".into());
    a.prefs.browser_open = true;
    a.prefs.props_open = true;
    a.prefs.ai_panel_open = true;
    a.chrome.start.open = false;
    let wall = a.ed.walls[0].id;
    let mut pv = XlsxPreview::linked(std::path::Path::new("frame_budget.xlsx"), false);
    pv.synced = false;
    a.xlsx = Some(pv);
    let ctx = egui::Context::default();
    let mut t = 0.0;

    // the first pass of a context only installs fonts and styles
    frame(&mut a, &ctx, &mut t, vec![]);
    // the first real frame may read the model
    let first = frame(&mut a, &ctx, &mut t, vec![]);
    assert!(first > 0, "the statement counter sees nothing");
    for i in 0..2 {
        idle(&format!("empty selection, frame {}", i + 2), frame(&mut a, &ctx, &mut t, vec![]));
    }

    // a selected wall: the properties panel shows its rows
    a.ed.selection = vec![Sel::Wall(wall)];
    frame(&mut a, &ctx, &mut t, vec![]);
    for i in 0..2 {
        idle(&format!("wall selected, frame {}", i + 2), frame(&mut a, &ctx, &mut t, vec![]));
    }

    // a hover over one spot: the first frame may look, the repeats must not
    let hover = vec![egui::Event::PointerMoved(egui::pos2(800.0, 500.0))];
    frame(&mut a, &ctx, &mut t, hover);
    frame(&mut a, &ctx, &mut t, vec![]);
    for i in 0..2 {
        idle(&format!("hover, repeat {}", i + 1), frame(&mut a, &ctx, &mut t, vec![]));
    }

    assert_eq!(ddc_app::ui_is_ru(), ru, "the test must not flip the UI language");
}

/// The options tab of the AI panel with a stored option, and a selected wall whose project
/// parameter value is locked: both are read once, then idle frames run nothing.
#[test]
fn idle_frames_with_ai_options_and_locked_params() {
    isolate();
    let ru = ddc_app::ui_is_ru();
    let mut ed = seeded(3);
    let wall = ed.walls[0].id;
    ed.model
        .add_params(&[ddc_model::ParamSpec {
            name: "Fire".into(),
            ptype: ddc_model::ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["wall".into()],
        }])
        .unwrap();
    ed.model
        .apply_edits(&[ddc_model::Edit::ParamValue {
            param: "Fire".into(),
            kind: "wall".into(),
            id: wall,
            value: Some(ddc_model::PVal::Text("EI30".into())),
        }])
        .unwrap();
    assert_eq!(ed.model.set_param_lock(&[("wall", wall, "Fire")]).unwrap(), 1);
    let (base_head, base_stamp) = ed.model.ai_option_head().unwrap();
    ed.model
        .ai_option_add(&ddc_model::OptionRec {
            id: 0,
            set: "s".into(),
            name: "A".into(),
            base_head,
            base_stamp,
            envelope: "{}".into(),
            seed: 0,
            params: "{}".into(),
            generator: Some("test".into()),
            metrics: None,
            note: None,
            why: None,
            status: ddc_model::OptionStatus::Draft,
        })
        .unwrap();
    ed.reload().unwrap();
    let mut a = EditorApp::new(ed, None, "test".into());
    a.prefs.props_open = true;
    a.prefs.ai_panel_open = true;
    a.chrome.start.open = false;
    a.ed.selection = vec![Sel::Wall(wall)];
    ddc_app::ai::ui::show_tab(2);
    let ctx = egui::Context::default();
    let mut t = 0.0;

    frame(&mut a, &ctx, &mut t, vec![]);
    assert!(frame(&mut a, &ctx, &mut t, vec![]) > 0, "the statement counter sees nothing");
    for i in 0..2 {
        idle(&format!("options tab and locked value, frame {}", i + 2), frame(&mut a, &ctx, &mut t, vec![]));
    }
    // a model change is seen: the next frame reads again
    a.ed.model.clear_param_lock(&[("wall", wall, "Fire")]).unwrap();
    assert!(frame(&mut a, &ctx, &mut t, vec![]) > 0, "an unlock must be read");
    idle("after the unlock", frame(&mut a, &ctx, &mut t, vec![]));

    assert_eq!(ddc_app::ui_is_ru(), ru, "the test must not flip the UI language");
}

#[test]
fn idle_frames_run_no_queries_2k() {
    run(32);
}

#[test]
#[ignore = "50k walls: slow to seed, run in release"]
fn idle_frames_run_no_queries_50k() {
    run(156);
}
