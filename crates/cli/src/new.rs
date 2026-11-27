// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc new <out> [--levels N]`: an empty project, the same start the editor gives a new
//! model (levels and the standard wall types), written to a new file.

use std::path::PathBuf;
use web_time::Instant;

use ddc_model::{LayerSpec, Model};
use serde_json::json;

use crate::{guarded, progress, Step, EXIT_FAILED, EXIT_OK, LOG_SCHEMA};

/// Storey height of the levels a new project starts with, metres.
pub const STOREY: f64 = 3.0;
pub const MAX_LEVELS: u32 = 200;

#[derive(Clone, Debug)]
pub struct NewArgs {
    pub out: PathBuf,
    pub levels: u32,
    pub force: bool,
}

/// `levels` levels from elevation 0, `STOREY` apart, and the three wall types of a new
/// project. Returns the lowest level.
pub fn seed(m: &mut Model, levels: u32) -> Result<i64, String> {
    let e = |e: ddc_model::Error| e.to_string();
    let mut first = None;
    for i in 0..levels.max(1) {
        let id = m.add_level(&format!("Level {}", i + 1), f64::from(i) * STOREY).map_err(e)?;
        first.get_or_insert(id);
    }
    let types: [(&str, &[(&str, f64)]); 3] = [
        ("Partition 100", &[("gypsum", 0.1)]),
        ("Masonry 200", &[("masonry", 0.2)]),
        ("Exterior 380", &[("plaster", 0.02), ("masonry", 0.25), ("insulation", 0.1), ("render", 0.01)]),
    ];
    for (name, layers) in types {
        let specs: Vec<LayerSpec> = layers
            .iter()
            .map(|(material, thickness)| LayerSpec { material: (*material).into(), thickness: *thickness })
            .collect();
        m.add_wall_type(name, &specs).map_err(e)?;
    }
    first.ok_or_else(|| "no level was made".into())
}

/// A fresh in-memory project with `levels` levels.
pub fn fresh(levels: u32) -> Result<Model, String> {
    let mut m = Model::open(":memory:").map_err(|e| e.to_string())?;
    seed(&mut m, levels)?;
    Ok(m)
}

fn write(a: &NewArgs) -> Result<(Option<String>, Vec<String>), String> {
    if a.out.exists() && !a.force {
        return Err(format!("{} exists; choose a new name or add --force", a.out.display()));
    }
    let mut m = fresh(a.levels)?;
    let w = crate::run::save(&mut m, &a.out)?;
    Ok((Some(a.out.to_string_lossy().into_owned()), w))
}

/// Returns (stdout text, exit code): 0 written, 2 nothing written.
pub fn run(a: &NewArgs, json_out: bool, quiet: bool) -> (String, i32) {
    let t = Instant::now();
    progress(quiet, &format!("writing {}", a.out.display()));
    let step = Step::of("new", t, guarded(|| write(a)));
    let code = if step.ok { EXIT_OK } else { EXIT_FAILED };
    if json_out {
        let j = json!({
            "schema": LOG_SCHEMA, "command": "new", "exit": code,
            "out": a.out.to_string_lossy(), "levels": a.levels, "steps": [step.to_json()],
        });
        return (j.to_string(), code);
    }
    match &step.error {
        Some(e) => (format!("error: new failed: {e}\n"), code),
        None => (format!("ok: empty project with {} levels, saved {}\n", a.levels, a.out.display()), code),
    }
}

