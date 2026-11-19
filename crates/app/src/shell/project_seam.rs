// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The only place the start screen and file drops touch the project-file API
//! (`recent_projects`, `open_project`, `new_project`). Everything else goes through here.
use super::EditorApp;
use crate::units::Units;
use std::path::PathBuf;
use web_time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    pub modified: Option<SystemTime>,
    pub thumbnail: Option<PathBuf>,
}

/// Newest first. Reads no file: `modified` is empty here, what is on disk comes from the
/// off-thread probe (`recent_probe`).
pub fn recent_projects(app: &EditorApp) -> Vec<RecentProject> {
    crate::project::recent_entries(&app.prefs.recent_projects)
        .into_iter()
        .map(|r| RecentProject {
            path: r.path,
            name: r.name,
            modified: r.modified,
            thumbnail: r.thumbnail,
        })
        .collect()
}

/// The project dialogs own the window.
pub fn modal(app: &EditorApp) -> bool {
    app.project_ui.modal()
}

/// Project windows and the close prompt; once per frame on the start screen path as well.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context) {
    crate::drawing::store::frame(app);
    crate::project_ui::frame(app, ctx);
    crate::open::frame(app, ctx);
}

/// New project; `units` is what the chosen start template adds to it. `false`: the user is
/// being asked about unsaved work, the units wait until the new model is really in.
pub fn new_project(app: &mut EditorApp, units: Option<Units>) -> bool {
    let rev = app.ed.revision;
    let handled = app.handlers.contains_key("file.new");
    if handled {
        app.run("file.new");
    }
    let done = !handled || app.ed.revision != rev;
    match units {
        Some(u) if done => app.ed.set_units(u),
        Some(u) => app.chrome.start.pending_units = Some(u),
        None => {}
    }
    done
}

pub fn open_project(app: &mut EditorApp, path: PathBuf) {
    crate::project_ui::request_open(app, path);
}

/// Every file named on the command line, in order.
pub fn file_args(argv: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut prev: Option<&str> = None;
    for a in argv.iter().skip(1) {
        let value = prev.is_some_and(|p| p.starts_with('-') && p != "--safe-graphics");
        if !a.starts_with('-') && !value {
            out.push(PathBuf::from(a));
        }
        prev = Some(a.as_str());
    }
    out
}

/// A start with display flags at most: developer runs (`--rooms`, `--scene`) keep their canvas.
pub fn plain_start(argv: &[String]) -> bool {
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--safe-graphics" => {}
            "--layout" => {
                it.next();
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_on_the_command_line() {
        let f = |v: &[&str]| file_args(&argv(v)).into_iter().next();
        assert_eq!(f(&["exe"]), None);
        assert_eq!(f(&["exe", "a b.ddc"]), Some(PathBuf::from("a b.ddc")));
        assert_eq!(
            f(&["exe", "--safe-graphics", "a.ddc"]),
            Some(PathBuf::from("a.ddc"))
        );
        assert_eq!(f(&["exe", "--layout", "classic"]), None);
        assert_eq!(f(&["exe", "--stream", "x.bin"]), None);
        assert_eq!(
            f(&["exe", "--layout", "classic", "a.ifc"]),
            Some(PathBuf::from("a.ifc"))
        );
    }

    #[test]
    fn every_file_argument() {
        let v = |a: &[&str]| file_args(&argv(a));
        assert!(v(&["exe", "--layout", "classic"]).is_empty());
        assert_eq!(
            v(&["exe", "--safe-graphics", "a.ddc", "b.ifc"]),
            vec![PathBuf::from("a.ddc"), PathBuf::from("b.ifc")]
        );
    }

    #[test]
    fn display_flags_keep_the_start_screen() {
        assert!(plain_start(&argv(&["exe"])));
        assert!(plain_start(&argv(&["exe", "--safe-graphics"])));
        assert!(plain_start(&argv(&[
            "exe",
            "--layout",
            "classic",
            "--safe-graphics"
        ])));
        assert!(!plain_start(&argv(&["exe", "--rooms", "100"])));
        assert!(!plain_start(&argv(&["exe", "a.ddc"])));
    }

    #[test]
    fn stand_ins_end_with_the_merge() {
        let project_ui = concat!(env!("CARGO_MANIFEST_DIR"), "/src/project_ui.rs");
        let marker = ["F1-", "REQUIRED"].concat();
        assert!(
            !Path::new(project_ui).exists() || !include_str!("project_seam.rs").contains(&marker),
            "project_ui is in the tree: wire the stand-ins as the module header says"
        );
    }
}
