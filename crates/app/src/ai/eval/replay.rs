// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Recorded model answers: `<root>/<case id>/steps.json`, one file per case.

use ddc_ai::fake::Recording;
use std::path::{Path, PathBuf};

/// Where the committed recordings live.
pub fn default_root() -> PathBuf {
    super::case::evals_dir().join("replay")
}

fn file(root: &Path, id: &str) -> PathBuf {
    root.join(id).join("steps.json")
}

pub fn exists(root: &Path, id: &str) -> bool {
    file(root, id).is_file()
}

pub fn load(root: &Path, id: &str) -> Result<Recording, String> {
    let p = file(root, id);
    let t = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let j = serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?;
    Recording::from_json(&j).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn save(root: &Path, id: &str, r: &Recording) -> Result<(), String> {
    let p = file(root, id);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&r.to_json()).map_err(|e| e.to_string())?;
    std::fs::write(&p, text + "\n").map_err(|e| format!("{}: {e}", p.display()))
}
