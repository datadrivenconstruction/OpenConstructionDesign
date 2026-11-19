// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `project_info` and `export_ifc` for MCP clients. The export writes a new file into one folder
//! next to the saved project and nothing else: a client never names a path.

use super::mcp_core::reply;
use super::*;
use ddc_ai::mcp::tools as mcp_tools;
use std::sync::Mutex;

static PROJECT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The saved project file the live editor works on (None: not saved yet). Called every frame,
/// so it writes only when the path moved (Save As, New, Open).
pub(super) fn set_project(p: Option<&Path>) {
    if let Ok(mut g) = PROJECT.lock() {
        if g.as_deref() != p {
            *g = p.map(Path::to_path_buf);
        }
    }
}

fn project() -> Option<PathBuf> {
    PROJECT.lock().ok().and_then(|g| g.clone())
}

/// The only folder an agent may write IFC files to: `<project stem>.exports` beside the project.
pub(super) fn export_dir(project: &Path) -> Option<PathBuf> {
    let stem = project.file_stem()?.to_str()?;
    Some(project.parent()?.join(format!("{stem}.exports")))
}

/// `dir/file` when `file` is one plain name ending in .ifc; nothing that could leave `dir`.
pub(super) fn target_in(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let bad = file.is_empty()
        || file.len() > 120
        || file.starts_with('.')
        || file.contains(['/', '\\', ':', '\0'])
        || file.chars().any(char::is_control)
        || file.ends_with([' ', '.']);
    if bad {
        return Err("file must be a plain file name without folders".to_string());
    }
    let ok_ext = Path::new(file)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ifc"));
    if !ok_ext || file.len() <= 4 {
        return Err("file must end in .ifc".to_string());
    }
    Ok(dir.join(file))
}

impl Core {
    /// The saved project file the live editor works on: the folder agent-named relative paths
    /// are read from (None: not saved yet).
    pub(crate) fn project_file(&self) -> Option<PathBuf> {
        project()
    }

    pub(super) fn project_info(&mut self) -> ToolOutcome {
        let t = match self.ed.model.model_totals() {
            Ok(t) => t,
            Err(e) => return ToolOutcome::error(e.to_string()),
        };
        let levels: Vec<J> = self
            .ed
            .levels()
            .into_iter()
            .map(|l| json!({"id": l.0, "name": l.1, "elevation": super::fmt::num(l.2)}))
            .collect();
        let file = project()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .map_or(J::Null, J::from);
        let units = if self.view.units == Units::Metric { "metric" } else { "imperial" };
        let mut counts = json!({
            "walls": t.walls, "doors": t.doors, "windows": t.windows,
            "slabs": t.slabs, "rooms": t.rooms, "curtains": t.curtains,
        });
        // every other counted category, footings included, so no element goes uncounted
        let skip = ["level", "wall", "opening", "slab", "curtain_wall", "room"];
        for (k, n) in ddc_model::category_counts(&self.ed.model) {
            if n > 0 && !skip.contains(&k) {
                counts[k] = json!(n);
            }
        }
        ToolOutcome::ok(reply(&[
            ("file", file),
            ("units", json!(units)),
            ("levels", J::from(levels)),
            ("counts", counts),
        ]))
    }

    pub(super) fn export_ifc_tool(&mut self, args: &J) -> ToolOutcome {
        let refuse = |why: &str| {
            ToolOutcome::ok(reply(&[("status", json!("refused")), ("reason", json!(why))]))
        };
        let Some(proj) = project() else {
            return refuse("save the project first: the export goes into a folder next to it");
        };
        let Some(dir) = export_dir(&proj) else {
            return refuse("no exports folder for this project");
        };
        let file = args["file"].as_str().unwrap_or_default();
        let target = match target_in(&dir, file) {
            Ok(t) => t,
            Err(e) => return refuse(&e),
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return refuse("the exports folder cannot be created");
        }
        if target.exists() {
            return refuse("a file with this name already exists");
        }
        let tmp = dir.join(format!(".{file}.part"));
        let done = ddc_io::ifc::export_ifc_file(&self.ed.model, &tmp, file, None)
            .and_then(|_| std::fs::rename(&tmp, &target).map_err(|e| e.to_string()));
        if let Err(e) = done {
            let _ = std::fs::remove_file(&tmp);
            return ToolOutcome::error(format!("export failed: {e}"));
        }
        let bytes = std::fs::metadata(&target).map_or(0, |m| m.len());
        ToolOutcome::ok(reply(&[
            ("status", json!("exported")),
            ("file", json!(file)),
            ("folder", json!(dir.file_name().map(|n| n.to_string_lossy().into_owned()))),
            ("bytes", json!(bytes)),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_ifc_name_stays_inside_the_folder() {
        let d = Path::new("proj.exports");
        assert_eq!(target_in(d, "a.ifc").unwrap(), d.join("a.ifc"));
        assert_eq!(target_in(d, "Model v2.IFC").unwrap(), d.join("Model v2.IFC"));
        for bad in [
            "", "../x.ifc", "a/b.ifc", "a\\b.ifc", "..\\x.ifc", "/etc/x.ifc", "C:\\x.ifc",
            "C:x.ifc", ".hidden.ifc", "x.txt", "x", ".ifc", "x.ifc.", "x.ifc ", "a\nb.ifc",
            "..", "con:.ifc",
        ] {
            assert!(target_in(d, bad).is_err(), "{bad:?}");
        }
        assert!(target_in(d, &format!("{}.ifc", "a".repeat(130))).is_err());
    }

    #[test]
    fn the_folder_sits_beside_the_project() {
        let p = Path::new("work").join("House.ddc");
        assert_eq!(export_dir(&p).unwrap(), Path::new("work").join("House.exports"));
        assert!(export_dir(Path::new("")).is_none());
    }
}
