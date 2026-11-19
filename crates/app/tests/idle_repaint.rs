// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An idle window asks for no frames: once the 3D picture has settled (no refinement left, or
//! refinement stuck past its bound) and nothing moves, a whole window frame requests no repaint.
//! Same harness as `frame_budget.rs`: headless frames of the full window, no graphics device.

use ddc_app::editor::Editor;
use ddc_app::shell::EditorApp;
use ddc_app::update::{self, Freq};
use ddc_model::WallSpec;
use std::time::Duration;

fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_idle_repaint_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
    });
    update::set_freq(Freq::Never);
}

fn seeded(k: usize) -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let mut ws = Vec::new();
    for i in 0..=k {
        let c = i as f64 * 6.0;
        for j in 0..k {
            let (a, b) = (j as f64 * 6.0, (j + 1) as f64 * 6.0);
            for (p, q) in [((a, c), (b, c)), ((c, a), (c, b))] {
                ws.push(WallSpec {
                    a: p,
                    b: q,
                    level,
                    base_offset: 0.0,
                    height: 3.5,
                    wall_type: wt,
                    offset0: None,
                });
            }
        }
    }
    ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
    ed
}

/// One window frame at `t` seconds; returns the repaint it asked for.
fn frame(a: &mut EditorApp, ctx: &egui::Context, t: f64, events: Vec<egui::Event>) -> Duration {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
        time: Some(t),
        events,
        ..Default::default()
    };
    let out = ctx.run(input, |c| a.ui(c, None));
    out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

/// Nothing asked for, or only the slow autosave timer of unsaved edits (seconds apart).
fn quiet(d: Duration) -> bool {
    d >= Duration::from_secs(4)
}

/// Idle frames, each at the time the previous one asked for, until one
/// asks for nothing; returns how many came. Panics past `cap` frames.
fn settle(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64, cap: usize) -> usize {
    for n in 1..=cap {
        let d = frame(a, ctx, *t, vec![]);
        if quiet(d) {
            return n;
        }
        // the delay egui reports is counted from the end of the frame (the frame's own
        // predicted 1/60 s is already spent), so the next frame starts that much later
        *t += d.as_secs_f64() + 1.0 / 60.0;
    }
    panic!("the idle window still asks for frames after {cap}");
}

fn stats(polling: bool) {
    ddc_render::perf::publish(ddc_render::FrameStats {
        polling,
        refining: polling,
        ..Default::default()
    });
}

#[test]
fn idle_3d_view_asks_for_no_repaint_once_refinement_completes() {
    isolate();
    let ru = ddc_app::ui_is_ru();
    let mut a = EditorApp::new(seeded(20), None, "test".into());
    a.chrome.start.open = false;
    a.view3d = true;
    let ctx = egui::Context::default();
    let mut t = 0.0;
    stats(false);
    // first frames build the scene and lay out the panels
    settle(&mut a, &ctx, &mut t, 400);
    for _ in 0..3 {
        t += 5.0;
        assert!(quiet(frame(&mut a, &ctx, t, vec![])), "settled 3D view at {t}");
    }

    // refinement that the next frame continues: timed frames while it lasts
    stats(true);
    t += 1.0;
    let d = frame(&mut a, &ctx, t, vec![egui::Event::PointerMoved(egui::pos2(800.0, 500.0))]);
    assert!(d <= ddc_app::view3d::settle::REFINE_STEP, "refining asks for a frame: {d:?}");
    stats(false);
    let n = settle(&mut a, &ctx, &mut t, 50);
    assert!(n <= 3, "a finished refinement stops at once ({n} frames)");

    // refinement that never finishes with no input is bounded, then the view is quiet
    stats(true);
    t += 1.0;
    frame(&mut a, &ctx, t, vec![egui::Event::PointerMoved(egui::pos2(810.0, 500.0))]);
    let n = settle(&mut a, &ctx, &mut t, 400);
    let bound = ddc_app::view3d::settle::MAX_IDLE_POLL_S;
    let most = (bound / ddc_app::view3d::settle::REFINE_STEP.as_secs_f64()) as usize + 5;
    assert!(n <= most, "stuck refinement stopped after {n} frames");
    t += 5.0;
    assert!(quiet(frame(&mut a, &ctx, t, vec![])));
    stats(false);

    // the plan is idle too
    a.view3d = false;
    t += 1.0;
    settle(&mut a, &ctx, &mut t, 200);
    t += 5.0;
    assert!(quiet(frame(&mut a, &ctx, t, vec![])), "settled plan");
    assert_eq!(ddc_app::ui_is_ru(), ru, "the test must not flip the UI language");
}
