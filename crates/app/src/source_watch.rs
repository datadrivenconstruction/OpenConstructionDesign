// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Source file watch: at project open and on window focus (at most every 5 s) the files the
//! project was imported from are looked at. Stat only; a file whose size or time differ is hashed
//! on a worker. Same hash: the stat is refreshed silently. A moved file with the same hash is
//! rebound silently. A different hash shows a strip with Update model / Compare / Ignore this
//! version. Nothing here changes the model without a click.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use web_time::{Duration, Instant};

use ddc_model::source::file::{
    hash_file, judge, locate, needs_hash, rel_path, stat_of, FileStamp, Found, SourceFile, Stat,
    Verdict,
};
use ddc_model::source::MASS_CHANGE;
use ddc_model::{Error, Model};
use ddc_ui::theme::Role;
use ddc_ui::widgets::{self, Tone};

use crate::editor::OpFn;
use crate::jobs::{Job, Poll};
use crate::shell::EditorApp;

/// Shortest time between two looks at the files.
pub const EVERY: Duration = Duration::from_secs(5);

/// What the worker found for one source.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    /// Same content; the stat is new (nothing is hashed next time).
    Current { source: String, stat: Stat },
    /// Same content at another place: path, key and relative path follow.
    Moved {
        source: String,
        path: PathBuf,
        stamp: FileStamp,
    },
    /// No hash was recorded before schema 39 and the file is not newer than the import.
    Adopt {
        source: String,
        path: PathBuf,
        stamp: FileStamp,
    },
    /// Another version than the last import / our last write / the ignored one.
    Changed {
        source: String,
        path: PathBuf,
        stamp: FileStamp,
    },
    /// Nowhere to be found (stored path, relative path, beside the project).
    Missing { source: String },
}

impl Out {
    pub fn source(&self) -> &str {
        match self {
            Out::Current { source, .. }
            | Out::Moved { source, .. }
            | Out::Adopt { source, .. }
            | Out::Changed { source, .. }
            | Out::Missing { source } => source,
        }
    }
}

/// One source's file, as the strip shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Changed { path: PathBuf, stamp: FileStamp },
    Missing,
}

/// Looks at one source file. `known` is the (path, stat) of a version already judged changed:
/// not hashed again until it changes.
pub fn check_one(
    f: &SourceFile,
    project_dir: Option<&Path>,
    known: Option<&(PathBuf, Stat)>,
    stop: &dyn Fn() -> bool,
) -> Option<Out> {
    if !f.has_file() {
        return None;
    }
    let Some(found) = locate(f, project_dir) else {
        return Some(Out::Missing {
            source: f.source.clone(),
        });
    };
    let path = found.path().to_path_buf();
    let Ok(stat) = stat_of(&path) else {
        return Some(Out::Missing {
            source: f.source.clone(),
        });
    };
    let stored = matches!(found, Found::Stored(_));
    if stored && !needs_hash(f, &stat) {
        return None;
    }
    if known.is_some_and(|(p, s)| *p == path && *s == stat) {
        return None;
    }
    let stamp = FileStamp {
        hash: hash_file(&path, stop).ok()?,
        stat,
    };
    let source = f.source.clone();
    Some(match judge(f, &stamp.hash, stat.mtime) {
        Verdict::Changed => Out::Changed {
            source,
            path,
            stamp,
        },
        Verdict::Adopt => Out::Adopt {
            source,
            path,
            stamp,
        },
        Verdict::Current | Verdict::Ignored if stored => Out::Current { source, stat },
        Verdict::Current | Verdict::Ignored => Out::Moved {
            source,
            path,
            stamp,
        },
    })
}

#[derive(Default)]
pub struct Watch {
    last: Option<Instant>,
    was_focused: bool,
    project: Option<PathBuf>,
    rx: Option<Receiver<Vec<Out>>>,
    pub items: std::collections::BTreeMap<String, Item>,
    /// (path, stat) of versions already judged changed, so they are not hashed again.
    known: HashMap<String, (PathBuf, Stat)>,
    /// Re-import of an IFC source in the background: (plan job, source).
    update: Option<(Job<Arc<ddc_io::ifc::Plan>>, String)>,
}

/// Cheap triggers; the work itself runs on a thread.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    poll_check(a);
    poll_update(a, ctx);
    let focused = ctx.input(|i| i.focused);
    let gained = focused && !a.src.watch.was_focused;
    a.src.watch.was_focused = focused;
    let project = a.ed.project.path().map(Path::to_path_buf);
    let opened = a.src.watch.last.is_none() || a.src.watch.project != project;
    let due = a.src.watch.last.is_none_or(|t| t.elapsed() >= EVERY);
    if a.src.watch.rx.is_none() && (opened || (gained && due)) {
        a.src.watch.project = project;
        start_check(a);
    }
    strip(a, ctx);
    if a.src.watch.rx.is_some() || a.src.watch.update.is_some() {
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

fn start_check(a: &mut EditorApp) {
    a.src.watch.last = Some(Instant::now());
    let rows = a.ed.model.source_files().unwrap_or_default();
    if rows.iter().all(|r| !r.has_file()) {
        a.src.watch.items.clear();
        return;
    }
    let dir =
        a.ed.project
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
    let known = a.src.watch.known.clone();
    let (tx, rx) = channel();
    a.src.watch.rx = Some(rx);
    // checks files on disk only; a crash drops the sender and poll_check reads "no news"
    let _ = crate::jobs::spawn_guarded(
        "source-watch",
        move || {
            let outs: Vec<Out> = rows
                .iter()
                .filter_map(|f| check_one(f, dir.as_deref(), known.get(&f.source), &|| false))
                .collect();
            let _ = tx.send(outs);
        },
        |_| {},
    );
}

fn poll_check(a: &mut EditorApp) {
    let Some(rx) = &a.src.watch.rx else { return };
    let outs = match rx.try_recv() {
        Ok(o) => o,
        Err(std::sync::mpsc::TryRecvError::Empty) => return,
        Err(_) => Vec::new(),
    };
    a.src.watch.rx = None;
    let checked: Vec<String> =
        a.ed.model
            .source_files()
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.source)
            .collect();
    let missing: Vec<String> = outs
        .iter()
        .filter(|o| matches!(o, Out::Missing { .. }))
        .map(|o| o.source().to_string())
        .collect();
    let project_dir =
        a.ed.project
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
    for o in outs {
        apply(a, o, project_dir.as_deref());
    }
    // a version judged changed stays while its stat stays; a source that is back is clean
    let w = &mut a.src.watch;
    let known = &w.known;
    w.items.retain(|s, it| {
        checked.contains(s)
            && match it {
                Item::Changed { .. } => known.contains_key(s),
                Item::Missing => missing.contains(s),
            }
    });
}

fn rel_of(dir: Option<&Path>, path: &Path) -> Option<String> {
    rel_path(dir?, path)
}

/// Bookkeeping writes (not undoable, no modified mark): the model's own record of the file.
fn apply(a: &mut EditorApp, o: Out, dir: Option<&Path>) {
    let w = &mut a.src.watch;
    let m = &a.ed.model;
    match o {
        Out::Current { source, stat } => {
            w.known.remove(&source);
            w.items.remove(&source);
            let _ = m.refresh_source_stat(&source, &stat);
        }
        Out::Moved {
            source,
            path,
            stamp,
        } => {
            w.known.remove(&source);
            w.items.remove(&source);
            let key = ddc_io::bridge::file_identity(&path);
            let rel = rel_of(dir, &path);
            let _ = m.rebind_source(&source, &path.to_string_lossy(), &key, rel.as_deref());
            let _ = m.refresh_source_stat(&source, &stamp.stat);
        }
        Out::Adopt {
            source,
            path,
            stamp,
        } => {
            w.known.remove(&source);
            w.items.remove(&source);
            let rel = rel_of(dir, &path);
            let _ = m.stamp_source(&source, &stamp, rel.as_deref());
        }
        Out::Changed {
            source,
            path,
            stamp,
        } => {
            w.known.insert(source.clone(), (path.clone(), stamp.stat));
            w.items.insert(source, Item::Changed { path, stamp });
        }
        Out::Missing { source } => {
            w.known.remove(&source);
            w.items.insert(source, Item::Missing);
        }
    }
}

fn name_of(a: &EditorApp, source: &str) -> String {
    let p =
        a.ed.model
            .source_file(source)
            .ok()
            .flatten()
            .map(|f| f.path)
            .unwrap_or_default();
    Path::new(&p)
        .file_name()
        .map_or_else(|| source.to_string(), |n| n.to_string_lossy().to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Act {
    Update(String),
    Compare(String),
    Ignore(String, String),
    Locate(String),
    Dismiss(String),
}

/// The strip's buttons in reading order with their tone: `Accent` is the one primary action,
/// the rest is quiet. "Dismiss" is drawn as a close glyph.
pub(crate) fn actions(item: &Item) -> Vec<(&'static str, Tone)> {
    match item {
        Item::Changed { .. } => vec![
            ("src.watch.update", Tone::Accent),
            ("src.watch.compare", Tone::Neutral),
            ("src.watch.ignore", Tone::Neutral),
        ],
        Item::Missing => vec![("src.watch.locate", Tone::Accent), ("src.watch.dismiss", Tone::Neutral)],
    }
}

/// What the button named by its i18n key does.
fn act_for(key: &str, source: &str, item: &Item) -> Option<Act> {
    let s = source.to_string();
    match (key, item) {
        ("src.watch.update", _) => Some(Act::Update(s)),
        ("src.watch.compare", _) => Some(Act::Compare(s)),
        ("src.watch.ignore", Item::Changed { stamp, .. }) => Some(Act::Ignore(s, stamp.hash.clone())),
        ("src.watch.locate", _) => Some(Act::Locate(s)),
        ("src.watch.dismiss", _) => Some(Act::Dismiss(s)),
        _ => None,
    }
}

/// Top edge of the strip: just under the ribbon's tab row and band.
pub(crate) fn strip_top() -> f32 {
    use ddc_ui::theme::{ribbon, space};
    ribbon::TAB_H + ribbon::BAND_H + space::SM
}

/// Draws the strip and returns the button the user pressed this frame; changes nothing itself.
fn strip_ui(
    ctx: &egui::Context,
    cat: &ddc_ui::Catalog,
    source: &str,
    name: &str,
    item: &Item,
    more: usize,
    busy: bool,
) -> Option<Act> {
    use ddc_ui::theme::{layout, space};
    use ddc_ui::widgets::Button;
    let th = ddc_ui::theme::of(ctx);
    let width = layout::DIALOG_WIDE.min(ctx.screen_rect().width() - 2.0 * space::LG).max(240.0);
    let list = actions(item);
    let mut act = None;
    egui::Area::new(egui::Id::new("src_watch_strip"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, strip_top()])
        .order(ddc_ui::z::Z::Canvas.order())
        .show(ctx, |ui| {
            crate::ui_kit::banner::banner(ui, &th, Tone::Warning, |ui| {
                ui.set_width(width - 2.0 * space::MD);
                let text = match item {
                    Item::Changed { .. } => cat.tf("src.watch.changed", &[("name", name)]),
                    Item::Missing => cat.tf("src.watch.missing", &[("name", name)]),
                };
                ui.horizontal(|ui| {
                    // the first widget lands at the far right: close glyph, then the primary
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if list.iter().any(|(k, _)| *k == "src.watch.dismiss")
                            && widgets::close_button(ui, &cat.t("src.watch.dismiss")).clicked()
                        {
                            act = act_for("src.watch.dismiss", source, item);
                        }
                        for (key, tone) in list.iter().filter(|(k, _)| *k != "src.watch.dismiss") {
                            let label = cat.t(key);
                            let r = if *tone == Tone::Accent {
                                let enabled = !(busy && *key == "src.watch.update");
                                crate::ui_kit::primary_button(ui, label, enabled)
                            } else {
                                Button::quiet(&label).show(ui, &th)
                            };
                            if r.clicked() {
                                act = act_for(key, source, item);
                            }
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            widgets::elided_label(ui, &text, th.font(Role::Body), th.sem().text_primary);
                        });
                    });
                });
                if more > 0 {
                    ui.label(
                        th.rich(Role::Caption, &cat.tf("src.watch.more", &[("n", &more.to_string())]))
                            .color(th.sem().text_secondary),
                    );
                }
            });
        });
    act
}

/// The strip: first problem file, the rest as a count.
fn strip(a: &mut EditorApp, ctx: &egui::Context) {
    let Some((source, item)) = a
        .src
        .watch
        .items
        .iter()
        .next()
        .map(|(s, i)| (s.clone(), i.clone()))
    else {
        return;
    };
    let more = a.src.watch.items.len() - 1;
    let name = name_of(a, &source);
    let busy = a.src.watch.update.is_some();
    let act = strip_ui(ctx, &a.sh.cat, &source, &name, &item, more, busy);
    match act {
        Some(Act::Update(s)) => update(a, &s),
        Some(Act::Compare(s)) => {
            a.src.source = Some(s);
            a.src.panel = true;
        }
        Some(Act::Ignore(s, hash)) => {
            let _ = a.ed.model.ignore_source_version(&s, &hash);
            a.src.watch.items.remove(&s);
            a.src.watch.known.remove(&s);
        }
        Some(Act::Locate(s)) => locate_dialog(a, &s),
        Some(Act::Dismiss(s)) => {
            a.src.watch.items.remove(&s);
        }
        None => {}
    }
}

/// "Locate": the user names the file; the same hash rebinds, another one is a change.
fn locate_dialog(a: &mut EditorApp, source: &str) {
    let Some(f) = a.ed.model.source_file(source).ok().flatten() else {
        return;
    };
    let mut d = crate::file_pick::Dlg::new().set_title(a.sh.cat.t("src.pick"));
    if let Some(n) = Path::new(&f.path).file_name() {
        d = d.set_file_name(n.to_string_lossy());
    }
    let Some(path) = d.pick_file() else { return };
    let Ok(stamp) = FileStamp::of(&path) else {
        return;
    };
    let dir =
        a.ed.project
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
    let out = match judge(&f, &stamp.hash, stamp.stat.mtime) {
        Verdict::Changed => Out::Changed {
            source: source.to_string(),
            path: path.clone(),
            stamp,
        },
        _ => Out::Moved {
            source: source.to_string(),
            path: path.clone(),
            stamp,
        },
    };
    if matches!(out, Out::Changed { .. }) {
        // keep the old path until the model is updated from the new file
        a.src.watch.known.remove(source);
    }
    apply(a, out, dir.as_deref());
}

/// Re-reads an IFC source on a worker; the merge runs on the UI thread as one undo step.
fn update(a: &mut EditorApp, source: &str) {
    let Some(Item::Changed { path, .. }) = a.src.watch.items.get(source).cloned() else {
        return;
    };
    reimport_ifc(a, &path, source);
}

pub fn is_ifc(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ifc" | "ifczip"))
}

/// Starts the background read of `path` for the IFC source `source` (the source panel and the
/// strip both come here). The plan carries the file's hash and stat.
pub fn reimport_ifc(a: &mut EditorApp, path: &Path, source: &str) {
    if a.src.watch.update.is_some() {
        a.ed.status = Some(a.sh.cat.t("src.status.busy"));
        return;
    }
    let p = path.to_path_buf();
    let job = Job::spawn("job.title.import", move |c: &Arc<crate::jobs::Progress>| {
        ddc_io::ifc::plan_file(&p, c)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    });
    a.src.watch.update = Some((job, source.to_string()));
}

fn poll_update(a: &mut EditorApp, ctx: &egui::Context) {
    let Some((job, _)) = &mut a.src.watch.update else {
        return;
    };
    match job.poll() {
        Poll::Running => ctx.request_repaint_after(Duration::from_millis(100)),
        Poll::Cancelled => a.src.watch.update = None,
        Poll::Failed(e) => {
            a.src.watch.update = None;
            a.ed.status = Some(a.sh.cat.tf("src.status.failed", &[("error", &e)]));
        }
        Poll::Done(pl) => {
            let Some((_, source)) = a.src.watch.update.take() else {
                return;
            };
            apply_update(a, pl, &source);
        }
    }
}

fn apply_update(a: &mut EditorApp, pl: Arc<ddc_io::ifc::Plan>, source: &str) {
    let policy = crate::source_ui::policy(&a.src);
    let out: Arc<Mutex<Option<ddc_io::ifc::ImportReport>>> = Arc::default();
    let o = out.clone();
    let f = OpFn(Arc::new(move |m: &mut Model| {
        let r = ddc_io::ifc::commit_plan_with(m, &pl, policy)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        if let Ok(mut g) = o.lock() {
            *g = Some(r);
        }
        Ok(())
    }));
    match a.ed.run_op(f) {
        Ok(()) => {
            let r = out
                .lock()
                .ok()
                .and_then(|mut g| g.take())
                .unwrap_or_default();
            let name = name_of(a, source);
            a.ed.status = Some(a.sh.cat.tf(
                "src.watch.done",
                &[
                    ("name", &name),
                    ("updated", &r.updated.to_string()),
                    ("kept", &r.kept_local.to_string()),
                    ("conflicts", &r.conflicts.to_string()),
                    ("locked", &r.locked.len().to_string()),
                ],
            ));
            a.src.watch.items.remove(source);
            a.src.watch.known.remove(source);
            a.src.source = Some(source.to_string());
            if r.conflicts > 0 {
                a.src.conflicts = true;
            }
        }
        Err(e) if e.to_string().contains(MASS_CHANGE) => {
            a.ed.status = Some(a.sh.cat.t("src.status.mass_change"));
        }
        Err(e) => {
            a.ed.status = Some(
                a.sh.cat
                    .tf("src.status.failed", &[("error", &e.to_string())]),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::source::file::hash_bytes;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-o2-watch-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn row(path: &Path, bytes: &[u8]) -> SourceFile {
        let st = stat_of(path).unwrap();
        SourceFile {
            source: "ifc:A".into(),
            path: path.to_string_lossy().to_string(),
            content_hash: hash_bytes(bytes),
            size: st.size,
            mtime: st.mtime,
            imported_at: 1,
            ..Default::default()
        }
    }

    #[test]
    fn equal_stat_is_not_hashed() {
        let d = dir("stat");
        let p = d.join("a.ifc");
        std::fs::write(&p, b"one").unwrap();
        let f = row(&p, b"one");
        // a stop flag that fires on the first block would fail the check if it hashed
        assert_eq!(check_one(&f, None, None, &|| true), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn touched_but_equal_content_only_refreshes_the_stat() {
        let d = dir("touch");
        let p = d.join("a.ifc");
        std::fs::write(&p, b"one").unwrap();
        let mut f = row(&p, b"one");
        f.mtime -= 5000;
        match check_one(&f, None, None, &|| false) {
            Some(Out::Current { stat, .. }) => assert_eq!(stat, stat_of(&p).unwrap()),
            o => panic!("{o:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn changed_content_is_reported_once() {
        let d = dir("chg");
        let p = d.join("a.ifc");
        std::fs::write(&p, b"one").unwrap();
        let f = row(&p, b"one");
        std::fs::write(&p, b"two!").unwrap();
        let Some(Out::Changed { path, stamp, .. }) = check_one(&f, None, None, &|| false) else {
            panic!("not changed")
        };
        // already judged: not hashed again while the stat stays
        let known = (path, stamp.stat);
        assert_eq!(check_one(&f, None, Some(&known), &|| true), None);
        // our own write is not a change
        let mut g = f.clone();
        g.written_hash = hash_bytes(b"two!");
        assert!(matches!(
            check_one(&g, None, None, &|| false),
            Some(Out::Current { .. })
        ));
        // an ignored version is not a change
        let mut h = f.clone();
        h.ignored_hash = hash_bytes(b"two!");
        assert!(matches!(
            check_one(&h, None, None, &|| false),
            Some(Out::Current { .. })
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn moved_file_with_same_content_rebinds_and_a_missing_one_is_quiet() {
        let d = dir("mv");
        let proj = d.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let old = d.join("gone").join("House.ifc");
        let mut f = SourceFile {
            source: "ifc:A".into(),
            path: old.to_string_lossy().to_string(),
            content_hash: hash_bytes(b"x"),
            imported_at: 1,
            ..Default::default()
        };
        assert_eq!(
            check_one(&f, Some(&proj), None, &|| false),
            Some(Out::Missing {
                source: "ifc:A".into()
            })
        );
        std::fs::write(proj.join("House.ifc"), b"x").unwrap();
        assert!(matches!(
            check_one(&f, Some(&proj), None, &|| false),
            Some(Out::Moved { .. })
        ));
        std::fs::write(proj.join("House.ifc"), b"y").unwrap();
        assert!(matches!(
            check_one(&f, Some(&proj), None, &|| false),
            Some(Out::Changed { .. })
        ));
        f.path.clear();
        f.rel_path.clear();
        assert_eq!(
            check_one(&f, Some(&proj), None, &|| false),
            None,
            "memory import: nothing to watch"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ifc_extension() {
        assert!(is_ifc(Path::new("a/B.IFC")));
        assert!(is_ifc(Path::new("b.ifczip")));
        assert!(!is_ifc(Path::new("b.rvt")));
    }

    fn changed() -> Item {
        Item::Changed { path: PathBuf::from("a.ifc"), stamp: FileStamp { hash: "h1".into(), ..Default::default() } }
    }

    #[test]
    fn strip_has_one_primary_per_state() {
        for item in [changed(), Item::Missing] {
            let list = actions(&item);
            assert_eq!(list.iter().filter(|(_, t)| *t == Tone::Accent).count(), 1, "{list:?}");
        }
        assert_eq!(actions(&changed())[0], ("src.watch.update", Tone::Accent));
        assert_eq!(actions(&Item::Missing)[0], ("src.watch.locate", Tone::Accent));
    }

    #[test]
    fn strip_buttons_dispatch_the_same_actions() {
        let it = changed();
        assert_eq!(act_for("src.watch.update", "ifc:A", &it), Some(Act::Update("ifc:A".into())));
        assert_eq!(act_for("src.watch.compare", "ifc:A", &it), Some(Act::Compare("ifc:A".into())));
        assert_eq!(
            act_for("src.watch.ignore", "ifc:A", &it),
            Some(Act::Ignore("ifc:A".into(), "h1".into()))
        );
        let gone = Item::Missing;
        assert_eq!(act_for("src.watch.locate", "ifc:A", &gone), Some(Act::Locate("ifc:A".into())));
        assert_eq!(act_for("src.watch.dismiss", "ifc:A", &gone), Some(Act::Dismiss("ifc:A".into())));
        assert_eq!(act_for("src.watch.ignore", "ifc:A", &gone), None);
        for item in [changed(), Item::Missing] {
            for (key, _) in actions(&item) {
                assert!(act_for(key, "s", &item).is_some(), "{key} has an action");
            }
        }
    }

    fn warm() -> egui::Context {
        let ctx = egui::Context::default();
        let mut th = ddc_ui::Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |c| {
                th.apply(c);
            });
        }
        ctx
    }

    #[test]
    fn idle_strip_renders_en_ru_and_clicks_nothing() {
        let ctx = warm();
        let long = "A very long source file name that would not fit a narrow window at all.ifc";
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for w in [1280.0_f32, 640.0] {
                for (item, more, busy) in [(changed(), 0, false), (changed(), 3, true), (Item::Missing, 2, false)] {
                    for _ in 0..2 {
                        let input = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 800.0))),
                            ..Default::default()
                        };
                        let mut out = Some(Act::Dismiss("x".into()));
                        let _ = ctx.run(input, |c| out = strip_ui(c, &cat, "ifc:A", long, &item, more, busy));
                        assert_eq!(out, None, "no action without the user");
                    }
                    let r = ctx
                        .memory(|m| m.area_rect(egui::Id::new("src_watch_strip")))
                        .expect("strip shown");
                    assert!(r.width() <= w, "strip {} wider than screen {w}", r.width());
                    assert!(r.min.y >= strip_top() - 1.0);
                }
            }
        }
    }

    #[test]
    fn strip_uses_no_raw_widgets() {
        let src = include_str!("source_watch.rs");
        let prod = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        for raw in ["ui.button(", "egui::Button::new", "ui.add_enabled(", "ui.small_button(", "Frame::popup", "ui.weak("] {
            assert!(!prod.contains(raw), "raw widget {raw} in the source strip");
        }
    }
}
