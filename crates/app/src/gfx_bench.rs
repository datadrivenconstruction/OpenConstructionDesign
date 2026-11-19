// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `--gfx-bench FILE|synthetic:N [--gfx-csv OUT]`: frame times per visual style and level on a real
//! GPU, against the budgets. Moving frames run at each level's full budgets, without the adaptive
//! hold, so a pass means the level meets its target with no help from the controller.

use crate::editor::Editor;
use ddc_render::graphics::{self, Budgets, Limits, Tier};
use ddc_render::perf::GpuClass;
use ddc_render::synth::{self, BenchReport};
use ddc_render::{Scene, VisualStyle};
use std::fmt::Write as _;
use std::path::Path;

const TIERS: [(Tier, &str); 3] = [
    (Tier::Speed, "speed"),
    (Tier::Balanced, "balanced"),
    (Tier::Quality, "quality"),
];

/// `synthetic:N` or a model file, composed into the 3D scene.
pub fn load(spec: &str) -> Result<Scene, String> {
    if let Some(n) = spec.strip_prefix("synthetic:") {
        let elements = n.parse().map_err(|_| format!("bad element count: {n}"))?;
        return Ok(synth::generate(&synth::SynthSpec {
            elements,
            ..Default::default()
        }));
    }
    let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
    ed.open_project(Path::new(spec))
        .map_err(|e| format!("{spec}: {e:?}"))?;
    Ok(crate::edit3d::scene3d::compose(&mut ed).0)
}

fn adapter_class() -> GpuClass {
    crate::gfx::candidates(None, false)
        .into_iter()
        .find_map(|p| crate::gfx::open(p).ok())
        .map_or(GpuClass::Integrated, |(a, _, _)| {
            GpuClass::of(a.get_info().device_type)
        })
}

fn csv_row(style: &str, level: &str, b: &Budgets, r: &BenchReport, pass: bool) -> String {
    format!(
        "{style},{level},{:.1},{:.2},{:.2},{:.2},{:.2},{}\n",
        b.target_ms, r.moving.avg_ms, r.moving.p95_ms, r.still.avg_ms, r.still.p95_ms, pass
    )
}

pub fn run(spec: &str, csv: Option<&str>, size: (u32, u32), frames: u32) -> Result<(), String> {
    let scene = load(spec)?;
    let class = adapter_class();
    println!(
        "{spec}: {} triangles | {}x{} | {frames} frames per phase | {class:?}",
        scene.triangle_count(),
        size.0,
        size.1
    );
    println!(
        "pass: moving p95 <= level target, resting p95 <= {:.0} ms",
        graphics::BENCH_REST_MS
    );
    println!("style          level     target   moving avg/p95   resting avg/p95");
    let mut out = String::from(
        "style,level,target_ms,moving_avg_ms,moving_p95_ms,rest_avg_ms,rest_p95_ms,pass\n",
    );
    let (mut met, mut total) = (0, 0);
    for style in VisualStyle::ALL {
        for (tier, level) in TIERS {
            let b = Budgets::of(tier, class);
            let top = graphics::ladder(&b)[0];
            let fx = graphics::frame_fx(style, &b, &top, &Limits::default(), false);
            let r = synth::bench_fx(&scene, size, frames, style, fx)?;
            let pass = graphics::bench_pass(&b, r.moving.p95_ms, r.still.p95_ms);
            total += 1;
            met += i32::from(pass);
            println!(
                "{:<14} {level:<9} {:>5.1}   {:>6.1}/{:<7.1}   {:>6.1}/{:<7.1} {}",
                style.key(),
                b.target_ms,
                r.moving.avg_ms,
                r.moving.p95_ms,
                r.still.avg_ms,
                r.still.p95_ms,
                if pass { "ok" } else { "OVER" }
            );
            let _ = write!(out, "{}", csv_row(style.key(), level, &b, &r, pass));
        }
    }
    println!("budgets met: {met} of {total}");
    if let Some(path) = csv {
        std::fs::write(path, out).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}
