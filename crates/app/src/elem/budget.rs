// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Budgets of the contract at 50k elements, run by `tools/bench.ps1` in release. They print the
//! measured time and do not fail on it: budgets are targets until the baseline is recorded and
//! they are tuned. The property commit asserts its budget at 10k walls in release.

use crate::editor::{Editor, OpFn, Sel};
use crate::geom_props::rows;
use crate::selection::{in_box, BoxMode};
use crate::wall_tool::{Event, Mode};
use ddc_geom::v2;
use ddc_model::{LayerSpec, Model, OpeningSpec, WallSpec};
use std::sync::Arc;
use web_time::Instant;

const SIDE_DEFAULT: usize = 224;
const PITCH: f64 = 6.0;

/// `DDC_BENCH_SIDE=100` gives 10 000 walls.
fn side() -> usize {
    std::env::var("DDC_BENCH_SIDE").ok().and_then(|v| v.parse().ok()).unwrap_or(SIDE_DEFAULT)
}

/// About 50k separate walls, none joined to another.
fn big() -> (Editor, i64) {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let layer = [LayerSpec {
        material: "c".into(),
        thickness: 0.2,
    }];
    let wall_type = ed.model.add_wall_type("W c0", &layer).unwrap();
    let mut first = 0;
    let n = side();
    for i in 0..n * n {
        let (x, y) = ((i % n) as f64 * PITCH, (i / n) as f64 * PITCH);
        let id = ed
            .model
            .add_wall(&WallSpec {
                a: (x, y),
                b: (x + 3.0, y),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type,
                offset0: None,
            })
            .unwrap();
        if first == 0 {
            first = id;
        }
    }
    ed.reload().unwrap();
    ed.set_mode(Mode::Select);
    (ed, first)
}

/// `bench.ps1` reads `bench_secs=`: the median leaves out building the model.
fn report(name: &str, target_ms: f64, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    let secs = samples[samples.len() / 2];
    println!("{name}: min {:.3} max {:.3} ms", samples[0] * 1e3, samples[samples.len() - 1] * 1e3);
    println!(
        "{name}: {:.3} ms, target {target_ms} ms bench_secs={secs:.6}",
        secs * 1e3
    );
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_plan_click_50k() {
    let (mut ed, _) = big();
    let samples = (0..50)
        .map(|i| {
            let p = v2(1.5 + (i % side()) as f64 * PITCH, 0.0);
            let t = Instant::now();
            ed.handle(Event::Click(p));
            t.elapsed().as_secs_f64()
        })
        .collect();
    assert!(!ed.selection.is_empty());
    report("plan click", 5.0, samples);
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_box_select_50k() {
    let (ed, _) = big();
    let (a, b) = (v2(-1.0, -1.0), v2(100.0, 100.0));
    let samples = (0..20)
        .map(|_| {
            let t = Instant::now();
            let got: Vec<Sel> = in_box(&ed, a, b, BoxMode::Crossing, |_| true);
            let secs = t.elapsed().as_secs_f64();
            assert!(!got.is_empty());
            secs
        })
        .collect();
    report("box select", 30.0, samples);
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_property_commit_50k() {
    let (mut ed, id) = big();
    let row = rows()
        .into_iter()
        .find(|r| r.kind == "wall" && r.key == "geom.height")
        .expect("wall height row");
    let (get, set) = (row.get, row.set);
    let v0 = get(&ed.model, id).unwrap();
    let h = crate::editor::scoped::hits();
    let samples: Vec<f64> = (0..10)
        .map(|i| {
            let v = v0 + 0.1 * (i + 1) as f64;
            let t = Instant::now();
            ed.run_op(OpFn(Arc::new(move |m: &mut Model| set(m, id, v))))
                .unwrap();
            let secs = t.elapsed().as_secs_f64();
            ed.undo();
            secs
        })
        .collect();
    // commits take the scoped reload, not a pass over every wall of the level
    assert!(crate::editor::scoped::hits() >= h + 9, "scoped reload not taken");
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    report("property commit", 150.0, samples);
    if !cfg!(debug_assertions) && side() <= 100 {
        assert!(median < 0.150, "property commit {:.1} ms at 10k walls", median * 1e3);
    }
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_hover_50k() {
    let (ed, _) = big();
    let samples = (0..50)
        .map(|i| {
            let p = v2(1.5 + (i % side()) as f64 * PITCH, 0.0);
            let t = Instant::now();
            let near = ed.big.plan_visible(&ed, p, p);
            let snaps = ed.snap_list(p, None, ed.tool.radius, None);
            let axes = ed.ref_axes_near(p, crate::temp_dims::REACH, None);
            let secs = t.elapsed().as_secs_f64();
            assert!(near.is_some() && !snaps.is_empty() && !axes.is_empty());
            secs
        })
        .collect();
    report("hover", 2.0, samples);
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_move_wall_50k() {
    let (mut ed, id) = big();
    let samples = (0..10)
        .map(|i| {
            let d = 0.1 * (i + 1) as f64;
            let t = Instant::now();
            ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                m.set_wall_axis(id, (0.0, d), (3.0, d))
            })))
            .unwrap();
            let secs = t.elapsed().as_secs_f64();
            ed.undo();
            secs
        })
        .collect();
    report("move one wall", 150.0, samples);
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn elem_undo_50k() {
    let (mut ed, id) = big();
    let samples = (0..10)
        .map(|i| {
            let d = 0.1 * (i + 1) as f64;
            ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                m.set_wall_axis(id, (0.0, d), (3.0, d))
            })))
            .unwrap();
            let t = Instant::now();
            ed.undo();
            t.elapsed().as_secs_f64()
        })
        .collect();
    report("undo", 150.0, samples);
}

/// A door width commit on the 50k grid (`DDC_BENCH_SIDE=100`: 10k walls) patches the opening
/// list instead of reloading every wall.
#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn budget_door_edit_10k() {
    let (mut ed, wall) = big();
    let spec = |width: f64| OpeningSpec {
        along: 1.5,
        width,
        sill: 0.0,
        height: 2.1,
        kind: "door".into(),
    };
    let door = ed.model.add_opening(wall, &spec(0.9)).unwrap();
    ed.reload().unwrap();
    let h = crate::editor::scoped::hits();
    let samples: Vec<f64> = (0..10)
        .map(|i| {
            let w = 0.8 + 0.02 * (i + 1) as f64;
            let t = Instant::now();
            ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.update_opening(door, &spec(w)))))
                .unwrap();
            let secs = t.elapsed().as_secs_f64();
            ed.undo();
            secs
        })
        .collect();
    assert!(crate::editor::scoped::hits() >= h + 9, "scoped reload not taken");
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    report("door edit", 150.0, samples);
    if !cfg!(debug_assertions) && side() <= 100 {
        assert!(median < 0.150, "door edit {:.1} ms at 10k walls", median * 1e3);
    }
}

#[test]
#[ignore = "release budget, run by tools/bench.ps1"]
fn cw_pick_50k() {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.add_curtains(&crate::editor::cw::tests::random_segs(50_000, 1500.0, 11));
    ed.tool.radius = 0.4;
    ed.cw_pick(v2(1.0, 1.0));
    let samples = (0..200)
        .map(|i| {
            let p = v2((i * 7 % 1500) as f64 + 0.3, (i * 13 % 1500) as f64 + 0.7);
            let t = Instant::now();
            let _ = ed.cw_pick(p);
            t.elapsed().as_secs_f64()
        })
        .collect();
    report("curtain pick", 1.0, samples);
}
