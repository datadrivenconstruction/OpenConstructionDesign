// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A project file that cannot be read is no dead end: the copies kept for it are offered with
//! their dates, the newest usable one first. The damaged file is only read, never written.

use std::path::{Path, PathBuf};
use web_time::{SystemTime, UNIX_EPOCH};

use ddc_ui::theme::{layout, space};
use ddc_ui::Catalog;
use egui::{self, RichText};

use super::Notice;
use crate::shell::recover_win;
use crate::shell::EditorApp;
use crate::ui_kit as kit;

/// A copy kept for the project that still reads as a project.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Kept {
    pub path: PathBuf,
    pub at: SystemTime,
}

pub(super) struct Damaged {
    pub path: PathBuf,
    pub detail: String,
    /// The newest of `_ddc_backup/<name>.prev`, `.original` and `<name>.bak-v*`.
    pub prev: Option<Kept>,
    /// Newest usable autosave snapshot of this project and how many usable ones there are.
    pub snaps: Option<(SystemTime, usize)>,
    /// Save mark and drawing signature of the failed Open: edits made since still ask.
    pub since: Option<(crate::project::SaveMark, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Act {
    OpenPrev,
    Snapshots,
    ShowFolder,
    Close,
}

/// Copies the save pipeline and format upgrades keep for `path`, newest first.
pub(super) fn kept_copies(path: &Path) -> Vec<Kept> {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return vec![];
    };
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let bak = dir.join(crate::save::job::BACKUP_DIR);
    let mut found = vec![bak.join(format!("{name}.prev")), bak.join(format!("{name}.original"))];
    let pre = format!("{name}.bak-v");
    if let Ok(rd) = std::fs::read_dir(dir) {
        found.extend(rd.flatten().map(|e| e.path()).filter(|p| {
            p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&pre))
        }));
    }
    let mut out: Vec<Kept> = found
        .into_iter()
        .filter(|p| p.is_file() && ddc_model::probe_project(p).is_ok())
        .filter_map(|p| {
            let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some(Kept { path: p, at })
        })
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out
}

/// What can stand in for the damaged `path`; `autosave` is the snapshot folder.
pub(super) fn find(path: &Path, detail: String, autosave: Option<&Path>) -> Damaged {
    let prev = kept_copies(path).into_iter().next();
    let snaps = autosave.and_then(|d| recover_win::all_for(d, path)).and_then(|r| {
        let good: Vec<_> = r.snaps.iter().filter(|s| s.damage.is_none()).collect();
        let newest = good.iter().map(|s| s.ms).max()?;
        let at = UNIX_EPOCH + std::time::Duration::from_millis(newest as u64);
        Some((at, good.len()))
    });
    Damaged { path: path.to_path_buf(), detail, prev, snaps, since: None }
}

/// Footer actions, the far-right (primary, Enter) first: the newest usable copy leads.
pub(super) fn actions(d: &Damaged) -> Vec<Act> {
    let mut v = match (&d.prev, d.snaps) {
        (Some(p), Some((s, _))) if s > p.at => vec![Act::Snapshots, Act::OpenPrev],
        (Some(_), Some(_)) => vec![Act::OpenPrev, Act::Snapshots],
        (Some(_), None) => vec![Act::OpenPrev],
        (None, Some(_)) => vec![Act::Snapshots],
        (None, None) => vec![],
    };
    v.push(Act::Close);
    v
}

fn label(c: &Catalog, act: Act) -> String {
    c.t(match act {
        Act::OpenPrev => "project.damaged.open_prev",
        Act::Snapshots => "project.damaged.snaps",
        Act::ShowFolder => "xlsx_readonly.show_in_folder",
        Act::Close => "project.damaged.close",
    })
}

/// Body lines: the cause, then each kept copy with its date.
pub(super) fn lines(c: &Catalog, d: &Damaged, ru: bool) -> Vec<String> {
    let detail = crate::ui_err::humanize(&d.detail, ru);
    let mut v = vec![c.tf("project.damaged.body", &[("detail", detail.as_str())])];
    if let Some(p) = &d.prev {
        let name = super::file_name(&p.path);
        let date = recover_win::fmt_time(p.at);
        v.push(c.tf("project.damaged.prev", &[("name", name.as_str()), ("date", date.as_str())]));
    }
    if let Some((at, n)) = d.snaps {
        let (date, n) = (recover_win::fmt_time(at), n.to_string());
        v.push(c.tf("project.damaged.snap", &[("date", date.as_str()), ("count", n.as_str())]));
    }
    if d.prev.is_none() && d.snaps.is_none() {
        v.push(c.t("project.damaged.none"));
    }
    v
}

pub(super) fn window(a: &mut EditorApp, ctx: &egui::Context, d: Damaged) {
    let ru = a.sh.cat.lang == ddc_ui::Lang::Ru;
    let (c, th) = (&a.sh.cat, &a.sh.theme);
    let acts = actions(&d);
    let body = lines(c, &d, ru);
    let mut pick = None;
    let m = kit::modal_dialog("project.damaged", ctx, |ui| {
        ui.set_width(layout::DIALOG_FORM);
        kit::modal_title(ui, c.t("project.damaged.title"));
        for l in &body {
            ui.label(RichText::new(l.as_str()).color(th.c("text.secondary")));
            ui.add_space(space::XS);
        }
        // the folder holds the file and its kept copies: a way out without leaving the dialog
        if kit::secondary_button(ui, label(c, Act::ShowFolder)).clicked() {
            pick = Some(Act::ShowFolder);
        }
        kit::dialog_footer(ui, |ui| {
            for (i, act) in acts.iter().enumerate() {
                let text = label(c, *act);
                let hit = if i == 0 {
                    kit::primary_action(ui, text, None)
                } else if *act == Act::Close {
                    kit::cancel_button(ui, text).clicked()
                } else {
                    kit::secondary_button(ui, text).clicked()
                };
                if hit {
                    pick = Some(*act);
                }
            }
        });
    });
    if pick.is_none() && m.should_close() {
        pick = Some(Act::Close);
    }
    match pick {
        None => a.project_ui.notice = Some(Notice::Damaged(d)),
        Some(act) => run(a, act, d),
    }
}

/// A kept copy or snapshot comes back bound to no file, so Save asks for a name: the damaged
/// file and its `.prev` are never replaced or rotated.
pub(super) fn run(a: &mut EditorApp, act: Act, d: Damaged) {
    match act {
        Act::Close => {}
        Act::OpenPrev => {
            if let Some(p) = d.prev {
                super::start_open(a, p.path, true);
                if let Some(super::Running::Open(j)) = a.project_ui.job.as_mut() {
                    // adopted unbound: Save never writes the copy or the damaged file
                    j.recovered_from = Some(d.path.clone());
                    // edits made while the failed Open ran were never asked about: Adopt asks
                    if let Some((mark, drawings)) = d.since {
                        j.mark = mark;
                        j.drawings = drawings;
                    }
                }
            }
        }
        Act::Snapshots => {
            let dir = a.prefs.autosave_folder();
            a.recover = dir.and_then(|dir| recover_win::all_for(&dir, &d.path)).map(|mut r| {
                r.damaged = true;
                r
            });
        }
        Act::ShowFolder => {
            crate::shell::xlsx_head::show_in_folder(&d.path);
            a.project_ui.notice = Some(Notice::Damaged(d));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::Lang;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-dmg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn project(path: &Path) {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        ed.save_project_as(path).unwrap();
    }

    #[test]
    fn a_damaged_file_with_a_prev_offers_it_first() {
        let d = scratch("prev");
        let file = d.join("a.ddc");
        std::fs::write(&file, b"not a database").unwrap();
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        project(&bak.join("a.ddc.prev"));
        let before = std::fs::read(&file).unwrap();
        let dm = find(&file, "file is not a database".into(), None);
        assert_eq!(dm.prev.as_ref().map(|k| k.path.clone()), Some(bak.join("a.ddc.prev")));
        assert_eq!(actions(&dm), vec![Act::OpenPrev, Act::Close]);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let l = lines(&c, &dm, lang == Lang::Ru);
            assert_eq!(l.len(), 2, "cause and the copy");
            let date = recover_win::fmt_time(dm.prev.as_ref().unwrap().at);
            assert!(l[1].contains("a.ddc.prev") && l[1].contains(&date), "{}", l[1]);
            assert_ne!(label(&c, Act::OpenPrev), "project.damaged.open_prev");
        }
        assert_eq!(std::fs::read(&file).unwrap(), before, "the damaged file is not touched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn without_a_prev_there_is_no_open_prev() {
        let d = scratch("none");
        let file = d.join("a.ddc");
        std::fs::write(&file, b"not a database").unwrap();
        // a .prev that is damaged too is no offer
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        std::fs::write(bak.join("a.ddc.prev"), b"junk").unwrap();
        let dm = find(&file, "x".into(), Some(&d.join("auto")));
        assert!(dm.prev.is_none());
        assert_eq!(actions(&dm), vec![Act::Close]);
        let c = Catalog::new(Lang::En);
        assert_eq!(lines(&c, &dm, false).last().cloned(), Some(c.t("project.damaged.none")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_newer_snapshot_leads_over_an_older_prev() {
        let d = scratch("snap");
        let auto = d.join("auto");
        let file = d.join("a.ddc");
        let bak = d.join(crate::save::job::BACKUP_DIR);
        std::fs::create_dir_all(&bak).unwrap();
        project(&bak.join("a.ddc.prev"));
        std::fs::write(&file, b"not a database").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let m = ddc_model::Model::open(":memory:").unwrap();
        m.autosave_as(&auto.join(recover_win::autosave_name(Some(&file))), 3).unwrap();
        let dm = find(&file, "x".into(), Some(&auto));
        assert_eq!(dm.snaps.map(|s| s.1), Some(1), "older than the damaged file still counts");
        assert_eq!(actions(&dm), vec![Act::Snapshots, Act::OpenPrev, Act::Close]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
