// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scripts window (`mgr.scripts`, SR). A script is an edit envelope file (the one edit language
//! of tools, XLSX, the assistant and MCP agents; docs/design/AI_NATIVE.md section 2): JSON with
//! a title and groups of operations. Scripts live as `*.json` in the `scripts` folder of the
//! settings directory; "Add script" checks a file and copies it there. A script is read strictly
//! (unknown fields and newer versions are refused with the reason), "Preview" is a dry run on a
//! scratch copy (what it would create, change and delete), "Run" applies it as one undo step
//! with the script named as the author in the change journal.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::editor::Editor;
use crate::shell::EditorApp;
use crate::ui_kit::{
    cancel_button, dialog_footer, empty_note, error_note, primary_click, secondary_button,
    section_title, Dialog, DialogSize,
};
use ddc_ops::{Actor, Envelope};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::space;

/// A script file in the folder and what reading it gave.
#[derive(Clone, Debug)]
pub(crate) struct Script {
    pub path: PathBuf,
    pub name: String,
    pub read: Result<Envelope, String>,
}

#[derive(Default)]
struct ScriptsUi {
    open: bool,
    /// Folder listing; `None` reads it again.
    list: Option<Vec<Script>>,
    sel: Option<PathBuf>,
    /// Dry run of the selected script: counts line or the refusal.
    preview: Option<Result<String, String>>,
    msg: Option<String>,
}

thread_local! {
    static ST: RefCell<ScriptsUi> = RefCell::new(ScriptsUi::default());
    /// Folder override for tests.
    static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Folder of the scripts: `<settings>/scripts`.
pub(crate) fn folder() -> Option<PathBuf> {
    DIR.with(|d| d.borrow().clone())
        .or_else(|| crate::gfx::data_dir().map(|d| d.join("scripts")))
}

/// Reads one script file strictly.
pub(crate) fn read(path: &Path) -> Script {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let read = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| Envelope::from_json(&t).map_err(|e| e.to_string()));
    Script {
        path: path.to_path_buf(),
        name,
        read,
    }
}

/// Every `*.json` of the folder, by name; a missing folder is an empty list.
pub(crate) fn list(dir: &Path) -> Vec<Script> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json"))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    v.iter().map(|p| read(p)).collect()
}

/// Copies a checked script file into `dir` under a free name; the new path.
pub(crate) fn add(dir: &Path, src: &Path) -> Result<PathBuf, String> {
    let s = read(src);
    s.read?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem = if s.name.is_empty() { "script".to_string() } else { s.name };
    let to = free_path(dir, &stem);
    std::fs::copy(src, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

/// `<stem>.json` in `dir`, or `<stem> 2.json`, ... : the first name not taken.
pub(crate) fn free_path(dir: &Path, stem: &str) -> PathBuf {
    (1..)
        .map(|k| {
            let n = if k == 1 { format!("{stem}.json") } else { format!("{stem} {k}.json") };
            dir.join(n)
        })
        .find(|p| !p.exists())
        .expect("free name")
}

/// Opens the window with the script at `path` picked.
pub(crate) fn show(path: PathBuf) {
    ST.with(|s| {
        *s.borrow_mut() = ScriptsUi {
            open: true,
            sel: Some(path),
            ..ScriptsUi::default()
        };
    });
}

/// The envelope as a script run: the script is its author.
fn as_script(env: &Envelope, name: &str) -> Envelope {
    let mut e = env.clone();
    e.actor = Actor::Script { name: name.into() };
    e
}

/// "creates 1, changes 2, deletes 0" in the language of `c`, plus what a conversion lost.
fn counts(c: &Catalog, r: &crate::editor::ops_run::OpsReceipt) -> String {
    let n = |v: usize| v.to_string();
    let mut s = c.tf(
        "view_cmds.scripts.counts",
        &[
            ("new", &n(r.created.len() + r.copied.len())),
            ("changed", &n(r.changed.len())),
            ("deleted", &n(r.deleted.len())),
        ],
    );
    if !r.loss.is_empty() {
        s.push_str("; ");
        s.push_str(&r.loss.join("; "));
    }
    s
}

/// Dry run of a script on a scratch copy; the model is not touched.
pub(crate) fn preview(ed: &mut Editor, c: &Catalog, s: &Script) -> Result<String, String> {
    let env = s.read.as_ref().map_err(Clone::clone)?;
    let p = ed
        .preview_ops(as_script(env, &s.name))
        .map_err(|e| e.to_string())?;
    let line = counts(c, &p.receipt);
    ed.cancel_preview(p);
    Ok(line)
}

/// Runs a script: one undo step, or nothing at all with the reason.
pub(crate) fn run(ed: &mut Editor, c: &Catalog, s: &Script) -> Result<String, String> {
    let env = s.read.as_ref().map_err(Clone::clone)?;
    let r = ed.run_ops(as_script(env, &s.name)).map_err(|e| e.to_string())?;
    Ok(c.tf("view_cmds.scripts.done", &[("name", &s.name), ("what", &counts(c, &r))]))
}

fn h_open(_: &mut EditorApp) {
    ST.with(|s| {
        let mut s = s.borrow_mut();
        s.open = true;
        s.list = None;
        s.preview = None;
        s.msg = None;
    });
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("mgr.scripts", h_open);
}

enum Act {
    Pick(PathBuf),
    Add,
    Preview,
    Run,
}

fn list_ui(ui: &mut egui::Ui, st: &ScriptsUi, scripts: &[Script], acts: &mut Vec<Act>) {
    let th = ddc_ui::theme::of(ui.ctx());
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * space::XS;
    let height = (ui.available_height() * 0.45).max(row_h * 3.0);
    egui::ScrollArea::vertical()
        .id_salt("cmd1.scripts.list")
        .auto_shrink([false, true])
        .max_height(height)
        .show(ui, |ui| {
            for s in scripts {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h),
                    egui::Sense::click(),
                );
                let on = st.sel.as_deref() == Some(s.path.as_path());
                let fg = ddc_ui::widgets::paint_state(ui, &th, rect, &resp, on);
                let inner = rect.shrink2(egui::vec2(space::SM, 0.0));
                let title = match &s.read {
                    Ok(e) if !e.title.trim().is_empty() => e.title.clone(),
                    _ => String::new(),
                };
                let right = ui.painter().text(
                    inner.right_center(),
                    egui::Align2::RIGHT_CENTER,
                    &title,
                    th.font(ddc_ui::theme::Role::Small),
                    th.c("text.secondary"),
                );
                let room = (inner.width() - right.width() - space::MD).max(1.0);
                let font = th.font(ddc_ui::theme::Role::Body);
                let g = ddc_ui::widgets::elide(ui, &s.name, font, fg, room);
                let y = inner.center().y - g.size().y / 2.0;
                ui.painter().galley(egui::pos2(inner.left(), y), g, fg);
                if resp.clicked() {
                    acts.push(Act::Pick(s.path.clone()));
                }
            }
        });
}

fn details(ui: &mut egui::Ui, c: &Catalog, st: &ScriptsUi, s: &Script) {
    let sem = ddc_ui::theme::sem_of(ui.ctx());
    section_title(ui, &s.name);
    match &s.read {
        Err(e) => error_note(ui, &c.tf("view_cmds.scripts.unreadable", &[("e", e)])),
        Ok(env) => {
            if !env.title.trim().is_empty() {
                ui.label(egui::RichText::new(&env.title).color(sem.text_primary));
            }
            if let Some(w) = env.why.as_deref().filter(|w| !w.trim().is_empty()) {
                ui.label(egui::RichText::new(w).color(sem.text_secondary));
            }
            for g in &env.groups {
                let n = g.ops.len().to_string();
                let line = c.tf("view_cmds.scripts.group", &[("title", &g.title), ("n", &n)]);
                ui.label(egui::RichText::new(line).color(sem.text_secondary));
            }
        }
    }
    match &st.preview {
        Some(Ok(p)) => {
            ui.add_space(space::XS);
            ui.label(
                egui::RichText::new(c.tf("view_cmds.scripts.preview_line", &[("what", p)]))
                    .color(sem.text_primary),
            );
        }
        Some(Err(e)) => error_note(ui, e),
        None => {}
    }
}

fn body(
    ui: &mut egui::Ui,
    c: &Catalog,
    st: &ScriptsUi,
    scripts: &[Script],
    acts: &mut Vec<Act>,
    close: &mut bool,
) {
    let sem = ddc_ui::theme::sem_of(ui.ctx());
    let dir = folder().map(|d| d.display().to_string()).unwrap_or_default();
    let line = c.tf("view_cmds.scripts.folder", &[("dir", &dir)]);
    ui.label(egui::RichText::new(line).color(sem.text_secondary));
    ui.add_space(space::XS);
    let cur = st
        .sel
        .as_ref()
        .and_then(|p| scripts.iter().find(|s| &s.path == p));
    if scripts.is_empty() {
        empty_note(ui, c.t("view_cmds.scripts.empty"), Some(c.t("view_cmds.scripts.empty_hint").as_str()));
    } else {
        list_ui(ui, st, scripts, acts);
        ui.separator();
        match cur {
            Some(s) => details(ui, c, st, s),
            None => {
                ui.label(egui::RichText::new(c.t("view_cmds.scripts.pick")).color(sem.text_secondary));
            }
        }
    }
    if let Some(m) = &st.msg {
        ui.add_space(space::XS);
        ui.label(egui::RichText::new(m).color(sem.text_secondary));
    }
    dialog_footer(ui, |ui| {
        let why = match cur {
            None => Some(c.t("view_cmds.scripts.pick")),
            Some(Script { read: Err(_), .. }) => Some(c.t("view_cmds.scripts.blocked")),
            Some(_) => None,
        };
        if primary_click(ui, c.t("view_cmds.scripts.run"), why.as_deref()) {
            acts.push(Act::Run);
        }
        let r = ui.add_enabled_ui(why.is_none(), |ui| {
            secondary_button(ui, c.t("view_cmds.scripts.preview"))
        });
        if r.inner.clicked() {
            acts.push(Act::Preview);
        }
        if secondary_button(ui, c.t("view_cmds.scripts.add")).clicked() {
            acts.push(Act::Add);
        }
        if cancel_button(ui, c.t("find_replace.close")).clicked() {
            *close = true;
        }
    });
}

/// Per frame from the shell.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = ST.with(|s| std::mem::take(&mut *s.borrow_mut()));
    if !st.open {
        ST.with(|s| *s.borrow_mut() = st);
        return;
    }
    let scripts = match st.list.take() {
        Some(l) => l,
        None => folder().map(|d| list(&d)).unwrap_or_default(),
    };
    let (mut open, mut close, mut acts) = (true, false, vec![]);
    {
        let c = &a.sh.cat;
        Dialog::new("view_cmds.scripts", c.t("cmd.mgr.scripts"), DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| body(ui, c, &st, &scripts, &mut acts, &mut close));
    }
    st.list = Some(scripts);
    for act in acts {
        act_on(a, &mut st, act);
    }
    st.open = open && !close;
    ST.with(|s| *s.borrow_mut() = st);
}

fn act_on(a: &mut EditorApp, st: &mut ScriptsUi, act: Act) {
    let c = &a.sh.cat;
    let cur = || {
        let p = st.sel.as_ref()?;
        st.list.as_ref()?.iter().find(|s| &s.path == p).cloned()
    };
    match act {
        Act::Pick(p) => {
            st.sel = Some(p);
            st.preview = None;
            st.msg = None;
        }
        Act::Preview => {
            if let Some(s) = cur() {
                st.preview = Some(preview(&mut a.ed, c, &s));
            }
        }
        Act::Run => {
            if let Some(s) = cur() {
                let r = run(&mut a.ed, c, &s);
                st.preview = None;
                st.msg = Some(match r {
                    Ok(m) => {
                        a.ed.status = Some(m.clone());
                        m
                    }
                    Err(e) => c.tf("view_cmds.scripts.refused", &[("e", &e)]),
                });
            }
        }
        Act::Add => {
            let Some(dir) = folder() else { return };
            let pick = crate::file_pick::Dlg::new()
                .set_title(c.t("view_cmds.scripts.add"))
                .add_filter("JSON", &["json"])
                .pick_file();
            let Some(src) = pick else { return };
            st.msg = Some(match add(&dir, &src) {
                Ok(to) => {
                    st.sel = Some(to);
                    st.list = None;
                    c.t("view_cmds.scripts.added")
                }
                Err(e) => c.tf("view_cmds.scripts.unreadable", &[("e", &e)]),
            });
        }
    }
}

/// Window open on `dir` with `sel` picked, for the layout probes; `None` closes it and drops
/// the folder override.
#[cfg(test)]
pub(crate) fn open_for_layout(dir: Option<PathBuf>, sel: Option<&str>) {
    DIR.with(|d| *d.borrow_mut() = dir.clone());
    ST.with(|s| {
        *s.borrow_mut() = ScriptsUi {
            open: dir.is_some(),
            sel: dir.zip(sel).map(|(d, n)| d.join(n)),
            ..ScriptsUi::default()
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_geom::v2;
    use ddc_ui::Lang;
    use std::sync::Arc;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_cmd1_scripts_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn editor() -> Editor {
        crate::editor::ops_run::set_thread_exec(Some(Arc::new(crate::ops_exec::ModelExec)));
        Editor::new_in_memory().unwrap()
    }

    fn wall_script(ed: &Editor, dir: &Path, name: &str) -> PathBuf {
        let mut env = ed.wall_envelope(&[(v2(0.0, 0.0), v2(5.0, 0.0))]).unwrap();
        env.title = "One wall".into();
        let p = dir.join(format!("{name}.json"));
        std::fs::write(&p, env.to_json_pretty().unwrap()).unwrap();
        p
    }

    #[test]
    fn list_reads_json_files_by_name_and_reports_bad_ones() {
        let d = tmp("list");
        let ed = editor();
        wall_script(&ed, &d, "b wall");
        std::fs::write(d.join("a broken.json"), "{\"v\":2,\"nope\":1}").unwrap();
        std::fs::write(d.join("notes.txt"), "x").unwrap();
        let l = list(&d);
        assert_eq!(l.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["a broken", "b wall"]);
        assert!(l[0].read.is_err());
        assert_eq!(l[1].read.as_ref().unwrap().title, "One wall");
        assert!(list(&d.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preview_writes_nothing_and_run_is_one_undo_step() {
        let d = tmp("run");
        let mut ed = editor();
        let s = read(&wall_script(&ed, &d, "wall"));
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let rev = ed.revision;
            let line = preview(&mut ed, &c, &s).unwrap();
            assert!(line.contains('1') && !line.contains("view_cmds."), "{line}");
            assert_eq!(ed.revision, rev);
            assert!(ed.walls.is_empty());
        }
        let c = Catalog::new(Lang::En);
        let m = run(&mut ed, &c, &s).unwrap();
        assert!(m.contains("wall"), "{m}");
        assert_eq!(ed.walls.len(), 1);
        let top = &ed.model.journal(1).unwrap()[0];
        assert_eq!(top.actor, Some(Actor::Script { name: "wall".into() }), "script is the author");
        ed.undo();
        assert!(ed.walls.is_empty(), "the run is one undo step");
        let bad = read(&d.join("missing.json"));
        assert!(run(&mut ed, &c, &bad).is_err());
        assert!(ed.walls.is_empty());
        crate::editor::ops_run::set_thread_exec(None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn add_checks_the_file_and_never_overwrites() {
        let src = tmp("src");
        let d = tmp("dst");
        let ed = editor();
        let p = wall_script(&ed, &src, "wall");
        let a1 = add(&d, &p).unwrap();
        let a2 = add(&d, &p).unwrap();
        assert_ne!(a1, a2);
        assert_eq!(list(&d).len(), 2);
        let bad = src.join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        assert!(add(&d, &bad).is_err());
        assert_eq!(list(&d).len(), 2);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn command_opens_the_window() {
        crate::shot_press::set_screenshot_mode(true);
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        assert!(a.run("mgr.scripts"));
        assert!(ST.with(|s| s.borrow().open));
        ST.with(|s| *s.borrow_mut() = ScriptsUi::default());
    }
}
