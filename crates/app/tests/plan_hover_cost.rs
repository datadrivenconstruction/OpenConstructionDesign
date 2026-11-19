// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cost of a plan frame while only the pointer moves over a 1.5k-wall plan: the whole window
//! frame, and its parts that run per move (the tool's move, the hover pick). Prints the numbers;
//! the bound holds a moving-pointer frame near an idle one (no whole-plan rebuild per move).
//! `cargo test -p ddc-app --test plan_hover_cost -- --nocapture` (release for real numbers).

use ddc_app::editor::Editor;
use ddc_app::shell::EditorApp;
use ddc_app::update::{self, Freq};
use ddc_model::WallSpec;
use std::time::Instant;

fn isolate() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ddc_plan_hover_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("APPDATA", &dir);
    });
    update::set_freq(Freq::Never);
}

/// k x k rooms of 6 m: 2 * k * (k + 1) walls (k = 27: 1512).
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

fn frame(a: &mut EditorApp, ctx: &egui::Context, t: &mut f64, events: Vec<egui::Event>) -> f64 {
    *t += 0.016;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
        time: Some(*t),
        events,
        ..Default::default()
    };
    let t0 = Instant::now();
    let out = ctx.run(input, |c| a.ui(c, None));
    // tessellation is part of the frame's cost on the UI thread
    let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
    t0.elapsed().as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
fn moving_the_pointer_over_a_big_plan_costs_about_an_idle_frame() {
    isolate();
    let ru = ddc_app::ui_is_ru();
    let mut a = EditorApp::new(seeded(27), None, "test".into());
    a.chrome.start.open = false;
    assert!(a.ed.walls.len() >= 1500);
    let ctx = egui::Context::default();
    let mut t = 0.0;
    for _ in 0..6 {
        frame(&mut a, &ctx, &mut t, vec![]);
    }
    let n = 40;
    let idle: Vec<f64> = (0..n).map(|_| frame(&mut a, &ctx, &mut t, vec![])).collect();
    let moving: Vec<f64> = (0..n)
        .map(|i| {
            let p = egui::pos2(500.0 + 7.0 * i as f32, 420.0 + 3.0 * i as f32);
            frame(&mut a, &ctx, &mut t, vec![egui::Event::PointerMoved(p)])
        })
        .collect();
    // the parts that run once per move
    let vis = ddc_app::isolate::Vis::default();
    let t0 = Instant::now();
    for i in 0..n {
        let p = ddc_geom::v2(30.0 + 0.37 * i as f64, 40.0 + 0.11 * i as f64);
        let _ = ddc_app::pick_rules::hover_at(&a.ed, &vis, p);
    }
    let pick = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
    let t0 = Instant::now();
    for i in 0..n {
        let p = ddc_geom::v2(30.0 + 0.37 * i as f64, 40.0 + 0.11 * i as f64);
        a.ed.handle(ddc_app::wall_tool::Event::Move(p));
    }
    let mv = t0.elapsed().as_secs_f64() * 1000.0 / n as f64;
    let (idle, moving) = (median(idle), median(moving));
    eprintln!(
        "plan 1.5k walls: idle frame {idle:.2} ms, pointer-move frame {moving:.2} ms, \
         hover pick {pick:.3} ms, tool move {mv:.3} ms"
    );
    assert!(
        moving <= 1.5 * idle + 2.0,
        "a pointer move costs {moving:.2} ms against {idle:.2} ms idle"
    );
    assert_eq!(ddc_app::ui_is_ru(), ru, "the test must not flip the UI language");
}

#[test]
fn hover_asks_the_filter_only_about_walls_in_reach() {
    isolate();
    let ed = seeded(3);
    let w = ed.walls[0].clone();
    let mid = ddc_geom::v2((w.a.x + w.b.x) / 2.0, (w.a.y + w.b.y) / 2.0);
    let vis = ddc_app::isolate::Vis::default();
    assert!(ddc_app::pick_rules::hover_at(&ed, &vis, mid).is_some(), "a wall point hovers");
    assert!(ed.pick_at(mid, &|_| false).is_none(), "a refusing filter hides every wall");
    // the 24 walls of the block are not all asked: only those under the cursor
    let asked = std::cell::Cell::new(0);
    let hit = ed.pick_at(mid, &|_| {
        asked.set(asked.get() + 1);
        true
    });
    assert!(hit.is_some());
    assert!(asked.get() <= 4, "filter asked about {} walls", asked.get());
    let far = ddc_geom::v2(500.0, 500.0);
    assert!(ddc_app::pick_rules::hover_at(&ed, &vis, far).is_none());
}
