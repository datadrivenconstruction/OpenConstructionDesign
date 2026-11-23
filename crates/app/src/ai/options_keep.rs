// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Where design option recipes live and what happens to their branch copies. Recipes are rows of an
//! `ai_option` table (outside undo).
//! Branch copies `opt-<id>.db` are a cache next to the recipes. A copy leaves its folder only
//! when its recipe is deleted or discarded, and then it is moved to an `_archive` folder next to
//! the options folder, never removed. The AI worker keeps its recipes in the session
//! `recipes.db` only (saved project or not); the Options tab and Apply read them there, and
//! Apply copies the chosen recipe into the open model.

use ddc_model::{Model, OptionRec, OptionStatus, OptionStore, RecipeStore};
use serde_json::{json, Value as J};
use std::fs;
use std::path::{Path, PathBuf};
use web_time::{SystemTime, UNIX_EPOCH};

const SIDECARS: [&str; 4] = ["", "-wal", "-shm", "-journal"];

/// Folder of the branch copies of one project file.
pub fn options_dir(project: &Path) -> PathBuf {
    let stem = project
        .file_stem()
        .map_or_else(|| "project".into(), |s| s.to_string_lossy().into_owned());
    project.with_file_name(format!("{stem}.options"))
}

/// Archive folder for removed copies: next to the options folder.
pub fn archive_dir(options_dir: &Path) -> PathBuf {
    options_dir
        .parent()
        .map_or_else(|| PathBuf::from("_archive"), |p| p.join("_archive"))
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // another volume: the original goes only after the copy is complete
    fs::copy(from, to)?;
    fs::remove_file(from)
}

/// Moves a branch copy (and its sidecar files) to `_archive`; `None` when there is no copy.
/// The archived name carries a time stamp, so a reused option id never overwrites an older one.
pub fn archive_copy(copy: &Path) -> Result<Option<PathBuf>, String> {
    if !copy.is_file() {
        return Ok(None);
    }
    let dir = copy
        .parent()
        .map_or_else(|| PathBuf::from("_archive"), archive_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let name = copy
        .file_name()
        .map_or_else(|| "opt.db".into(), |n| n.to_string_lossy().into_owned());
    let to = dir.join(format!("{}-{}", stamp(), name));
    for suffix in SIDECARS {
        let from = with_suffix(copy, suffix);
        if from.is_file() {
            move_file(&from, &with_suffix(&to, suffix))
                .map_err(|e| format!("cannot archive {}: {e}", from.display()))?;
        }
    }
    Ok(Some(to))
}

/// Deletes a recipe. Its copy in `dir` goes to `_archive` first, so a failed move keeps the
/// recipe. Returns the archived path, if there was a copy.
pub fn delete_option(store: &OptionStore, dir: &Path, id: i64) -> Result<Option<PathBuf>, String> {
    store
        .get(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no option recipe {id}"))?;
    let moved = archive_copy(&dir.join(format!("opt-{id}.db")))?;
    store.delete(id).map_err(|e| e.to_string())?;
    Ok(moved)
}

/// Metric names listed per option; the values stay in the option's own tools.
const MAX_METRIC_KEYS: usize = ddc_ai::limits::METRIC_KEYS.us();

/// Recipe folder and recipe database of the AI worker: always inside the session folder, for a
/// saved project too. The worker never opens the project file for writing; Apply copies the
/// recipe into the open model on the UI thread.
pub fn locate(session_dir: &Path) -> (PathBuf, PathBuf) {
    (session_dir.to_path_buf(), session_dir.join("recipes.db"))
}

/// Accepted or merged through a card: the option is part of the model.
pub fn applied(s: OptionStatus) -> bool {
    matches!(s, OptionStatus::Accepted | OptionStatus::Merged)
}

fn status_name(s: OptionStatus) -> &'static str {
    match s {
        OptionStatus::Draft => "draft",
        OptionStatus::Accepted => "accepted",
        OptionStatus::Merged => "merged",
        OptionStatus::Rejected => "rejected",
    }
}

/// One line per recipe for `list_options`. `head` is the base model's journal head; a recipe
/// whose base differs is stale and cannot be applied until regenerated.
pub fn option_row(r: &OptionRec, head: (i64, i64)) -> J {
    let mut keys: Vec<String> = r
        .metrics
        .as_deref()
        .and_then(|m| serde_json::from_str::<J>(m).ok())
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();
    keys.sort();
    let more = keys.len().saturating_sub(MAX_METRIC_KEYS);
    keys.truncate(MAX_METRIC_KEYS);
    let mut row = json!({
        "id": r.id,
        "set": r.set,
        "name": r.name,
        "status": status_name(r.status),
        "stale": (r.base_head, r.base_stamp) != head,
        "metrics": keys,
        "why": r.why,
    });
    if more > 0 {
        row["metrics_more"] = json!(more);
    }
    row
}

/// Rows of `list_options`, by id. `m` is the opened option base: it gives the head, and a recipe
/// Apply copied into it reads as accepted there (same rule as the Options tab).
pub fn list_rows(m: &Model, store: &OptionStore, set: Option<&str>) -> Result<Vec<J>, String> {
    let head = m.ai_option_head().map_err(|e| e.to_string())?;
    let recs = store.list(set).map_err(|e| e.to_string())?;
    Ok(recs
        .iter()
        .map(|r| match model_twin(m, r).filter(|t| applied(t.status)) {
            Some(t) => option_row(&OptionRec { status: t.status, ..r.clone() }, head),
            None => option_row(r, head),
        })
        .collect())
}

/// Discard of `discard_option` and of the Options tab: the row stays, marked rejected, and its
/// copy in `dir` (if any) goes to `_archive`. Refuses an accepted option (it is part of the model
/// now) and one already discarded. Returns the archived path, if there was a copy.
pub fn discard(store: &dyn RecipeStore, dir: Option<&Path>, id: i64) -> Result<Option<PathBuf>, String> {
    let rec = store
        .get_recipe(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no option {id}: call list_options"))?;
    match rec.status {
        OptionStatus::Accepted => {
            return Err(format!(
                "option {id} is accepted: it is already in the model and cannot be discarded"
            ))
        }
        OptionStatus::Merged => {
            return Err(format!("option {id} is merged: it is already in the model and cannot be discarded"))
        }
        OptionStatus::Rejected => return Err(format!("option {id} is already discarded")),
        OptionStatus::Draft => {}
    }
    let moved = match dir {
        Some(d) => archive_copy(&d.join(format!("opt-{id}.db")))?,
        None => None,
    };
    store.reject_recipe(id).map_err(|e| e.to_string())?;
    Ok(moved)
}

/// Where the Options tab and Apply find recipes: `None` when they are rows of the open model
/// itself (no AI session), else the session store as `locate` names it.
pub fn ui_store(model: &Model, session_dir: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    let session = session_dir?;
    let (dir, db) = locate(session);
    (model.db_path().map(PathBuf::from).as_deref() != Some(db.as_path())).then_some((dir, db))
}

/// Options folder of the live AI session (`<session>/options`), if a session has copies.
pub fn live_session_dir() -> Option<PathBuf> {
    super::session::with_session(|s| {
        s.as_ref()
            .and_then(|u| u.base_path())
            .and_then(Path::parent)
            .map(|d| d.join("options"))
    })
}

/// The open model's row holding the same recipe as `r` (a store recipe copied in by Apply).
pub fn model_twin(m: &Model, r: &OptionRec) -> Option<OptionRec> {
    m.ai_option_list(Some(&r.set)).ok()?.into_iter().find(|t| {
        (t.name.as_str(), t.base_head, t.base_stamp, t.envelope.as_str(), t.seed, t.params.as_str())
            == (r.name.as_str(), r.base_head, r.base_stamp, r.envelope.as_str(), r.seed, r.params.as_str())
    })
}

#[cfg(test)]
#[path = "options_keep_tests.rs"]
mod tests;
