//! Merge window: file choice, background preparation, preview with counters, per-conflict choice,
//! apply as one undo step. A file with no shared history is simply refused.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddc_model::merge::{
    prepare_merge, preview_file, Lineage, MergeConflict, MergeError, MergePlan, MergeReport,
    Prepared,
};
use ddc_model::source::{show, Pick};
use ddc_ui::notify::Level;
use egui::{self, RichText};

use super::job::{working_body, Cancel, Job, Polled};
use super::{elided, file_label, kind_label, num, pick_project, select, work_dir};
use crate::editor::OpFn;
use crate::shell::EditorApp;
use crate::ui_kit as kit;

type Ready = Result<(Prepared, Option<MergePlan>), MergeError>;

#[derive(Default)]
enum Stage {
    #[default]
    Closed,
    Working(PathBuf, Job<Ready>),
    Refused(String),
    Review(Box<Review>),
}

struct Review {
    prepared: Prepared,
    plan: MergePlan,
    picks: HashMap<(String, i64), Pick>,
    error: Option<String>,
}

#[derive(Default)]
pub struct MergeWin {
    stage: Stage,
    /// Start pressed: the command cannot store the state itself, so it is taken out for the frame.
    pending: Option<PathBuf>,
}

pub(super) fn probe(w: &MergeWin) -> super::WinProbe {
    use super::WinProbe as P;
    match &w.stage {
        Stage::Closed if w.pending.is_some() => P::Working,
        Stage::Closed => P::Closed,
        Stage::Working(..) => P::Working,
        Stage::Refused(m) => P::Refused(m.clone()),
        Stage::Review(r) => P::Shown {
            source: r.prepared.source.clone(),
            count: r.plan.conflicts.len(),
        },
    }
}

pub fn start(a: &mut EditorApp) {
    if let Some(p) = pick_project(a, "merge.pick") {
        a.collab.merge.pending = Some(p);
    }
}

fn begin(w: &mut MergeWin, a: &mut EditorApp, path: PathBuf) {
    let line = match a.ed.model.merge_lineage() {
        Ok(l) => l,
        Err(e) => {
            w.stage = Stage::Refused(e.to_string());
            return;
        }
    };
    let model = a.ed.model.db_path();
    let source = path.clone();
    let work = work_dir("merge");
    let dir = work.clone();
    let job = Job::spawn(work, move |c| worker(&line, &source, &dir, model.as_deref(), c));
    w.stage = Stage::Working(path, job);
}

/// Background part: reads only the colleague's copy and the model file. None once cancelled.
fn worker(
    line: &Lineage,
    source: &std::path::Path,
    work: &std::path::Path,
    model: Option<&str>,
    c: &Cancel,
) -> Option<Ready> {
    worker_steps(line, source, work, model, c, || {})
}

/// `worker` with a hook between the copy and the preview (tests gate the mid-step there).
fn worker_steps(
    line: &Lineage,
    source: &std::path::Path,
    work: &std::path::Path,
    model: Option<&str>,
    c: &Cancel,
    mid: impl FnOnce(),
) -> Option<Ready> {
    if c.is_set() {
        return None;
    }
    let p = match prepare_merge(line, source, work) {
        Ok(p) => p,
        Err(e) => return Some(Err(e)),
    };
    mid();
    if c.is_set() {
        return None;
    }
    let plan = match model {
        Some(m) => match preview_file(m, &p) {
            Ok(plan) => Some(plan),
            Err(e) => return Some(Err(MergeError::from(e))),
        },
        None => None,
    };
    Some(Ok((p, plan)))
}

/// Cancel during Working: the job stops, the window closes, nothing reaches the model.
fn cancel_work(w: &mut MergeWin) -> Option<Job<Ready>> {
    if !matches!(w.stage, Stage::Working(..)) {
        return None;
    }
    let Stage::Working(_, mut job) = std::mem::take(&mut w.stage) else {
        return None;
    };
    job.cancel();
    Some(job)
}

fn refusal(a: &EditorApp, e: &MergeError) -> String {
    let c = &a.sh.cat;
    match e {
        MergeError::NoCommonBase => c.t("merge.no_base"),
        MergeError::Newer { .. } => c.t("merge.newer"),
        MergeError::Damaged(_) => c.t("merge.damaged"),
        MergeError::Model(e) => c.tf("merge.failed", &[("e", &e.to_string())]),
    }
}

fn poll(w: &mut MergeWin, a: &mut EditorApp, ctx: &egui::Context) {
    let Stage::Working(_, job) = &mut w.stage else {
        return;
    };
    ctx.request_repaint_after(Duration::from_millis(100));
    let got = match job.poll() {
        Polled::Done(v) => v,
        Polled::Pending => return,
        Polled::Lost => {
            w.stage = Stage::Refused(a.sh.cat.tf("merge.failed", &[("e", &crate::jobs::internal_error())]));
            return;
        }
    };
    w.stage = match got {
        Ok((prepared, plan)) => {
            // in-memory model: preview on its own connection
            let plan = match plan {
                Some(p) => Ok(p),
                None => a.ed.model.merge_preview(&prepared),
            };
            match plan {
                Ok(plan) => Stage::Review(Box::new(Review {
                    prepared,
                    plan,
                    picks: HashMap::new(),
                    error: None,
                })),
                Err(e) => Stage::Refused(a.sh.cat.tf("merge.failed", &[("e", &e.to_string())])),
            }
        }
        Err(e) => Stage::Refused(refusal(a, &e)),
    };
}

fn counts(ui: &mut egui::Ui, a: &EditorApp, plan: &MergePlan) {
    let c = &a.sh.cat;
    egui::Grid::new("ddc.merge.counts")
        .num_columns(5)
        .striped(true)
        .show(ui, |ui| {
            for k in [
                "",
                "merge.added",
                "merge.changed",
                "merge.deleted",
                "merge.conflicts",
            ] {
                if k.is_empty() {
                    ui.label("");
                } else {
                    ui.label(RichText::new(c.t(k)).strong());
                }
            }
            ui.end_row();
            ui.label(RichText::new(c.t("merge.total")).strong());
            num(ui, plan.added);
            num(ui, plan.changed);
            num(ui, plan.deleted);
            num(ui, plan.conflicts.len());
            ui.end_row();
            for t in &plan.by_table {
                elided(ui, &kind_label(a, &t.table));
                num(ui, t.added);
                num(ui, t.changed);
                num(ui, t.deleted);
                num(ui, t.conflicts);
                ui.end_row();
            }
        });
}

fn case_text(a: &EditorApp, cf: &MergeConflict) -> String {
    a.sh.cat.t(&format!("merge.case.{}", cf.case.key()))
}

enum Row {
    Show(String, i64),
}

fn conflicts(ui: &mut egui::Ui, a: &EditorApp, r: &mut Review, out: &mut Vec<Row>) {
    let c = &a.sh.cat;
    ui.horizontal(|ui| {
        ui.strong(c.tf(
            "merge.resolve",
            &[("n", &r.plan.conflicts.len().to_string())],
        ));
        if ui.button(c.t("merge.all_mine")).clicked() {
            r.picks.clear();
        }
        if ui.button(c.t("merge.all_theirs")).clicked() {
            for cf in &r.plan.conflicts {
                r.picks.insert((cf.table.clone(), cf.id), Pick::Theirs);
            }
        }
    });
    let plan = &r.plan;
    let picks = &mut r.picks;
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y + 6.0;
    egui::ScrollArea::vertical()
        .id_salt("ddc.merge.conflicts")
        .max_height(260.0)
        .auto_shrink([false, true])
        .show_rows(ui, row_h, plan.conflicts.len(), |ui, range| {
            for cf in &plan.conflicts[range] {
                let key = (cf.table.clone(), cf.id);
                let mut pick = picks.get(&key).copied().unwrap_or(Pick::Mine);
                ui.push_id((&cf.table, cf.id), |ui| {
                    ui.horizontal(|ui| {
                        let name = format!("{} #{}", kind_label(a, &cf.table), cf.id);
                        if ui.link(&name).on_hover_text(c.t("merge.show")).clicked() {
                            out.push(Row::Show(cf.table.clone(), cf.id));
                        }
                        ui.label(RichText::new(case_text(a, cf)).color(a.sh.theme.c("warning")));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.selectable_value(&mut pick, Pick::Theirs, c.t("merge.theirs"));
                            ui.selectable_value(&mut pick, Pick::Mine, c.t("merge.mine"));
                        });
                    });
                    if !cf.fields.is_empty() {
                        let text: Vec<String> = cf
                            .fields
                            .iter()
                            .map(|f| {
                                c.tf(
                                    "merge.field",
                                    &[
                                        ("f", &f.field),
                                        ("mine", &show(&f.mine)),
                                        ("theirs", &show(&f.theirs)),
                                    ],
                                )
                            })
                            .collect();
                        ui.add(
                            egui::Label::new(
                                RichText::new(text.join("; "))
                                    .color(a.sh.theme.c("text.secondary")),
                            )
                            .truncate(),
                        )
                        .on_hover_text(text.join("\n"));
                    }
                });
                if pick == Pick::Mine {
                    picks.remove(&key);
                } else {
                    picks.insert(key, pick);
                }
            }
        });
}

fn apply(a: &mut EditorApp, r: &Review) -> Result<MergeReport, String> {
    let p = r.prepared.clone();
    let plan = r.plan.clone();
    let picks: Vec<(String, i64, Pick)> = r
        .picks
        .iter()
        .map(|((t, id), k)| (t.clone(), *id, *k))
        .collect();
    let out: Arc<Mutex<Option<MergeReport>>> = Arc::default();
    let slot = out.clone();
    let f = OpFn(Arc::new(move |m: &mut ddc_model::Model| {
        let rep = m.merge_apply(&p, &plan, &picks)?;
        if let Ok(mut g) = slot.lock() {
            *g = Some(rep);
        }
        Ok(())
    }));
    a.ed.run_op(f).map_err(|e| e.to_string())?;
    Ok(out
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .unwrap_or_default())
}

pub fn frame(w: &mut MergeWin, a: &mut EditorApp, ctx: &egui::Context) {
    if let Some(p) = w.pending.take() {
        begin(w, a, p);
    }
    poll(w, a, ctx);
    let title = a.sh.cat.t("merge.title");
    let mut close = false;
    let mut cancel = false;
    let mut applied = None;
    let mut rows = vec![];
    match &mut w.stage {
        Stage::Closed => {}
        Stage::Working(path, job) => {
            let text = a.sh.cat.tf("merge.working", &[("file", &file_label(path))]);
            let mut open = true;
            kit::Dialog::new("collab.merge.work", title.as_str(), kit::DialogSize::Confirm)
                .id(egui::Id::new("ddc_merge"))
                .open(&mut open)
                .show(ctx, |ui| {
                    cancel = working_body(ui, &a.sh.cat, &text, job.elapsed());
                });
            cancel |= !open;
        }
        Stage::Refused(msg) => {
            let mut open = true;
            kit::Dialog::new("collab.merge.refused", title.as_str(), kit::DialogSize::Confirm)
                .id(egui::Id::new("ddc_merge"))
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.colored_label(a.sh.theme.c("warning"), msg.as_str());
                    ui.add_space(a.sh.theme.d().gap);
                    kit::dialog_footer(ui, |ui| {
                        if kit::primary_action(ui, a.sh.cat.t("merge.close"), None) {
                            close = true;
                        }
                    });
                });
            close |= !open;
        }
        Stage::Review(r) => {
            let mut open = true;
            kit::Dialog::new("collab.merge", title.as_str(), kit::DialogSize::Wide)
                .id(egui::Id::new("ddc_merge"))
                .open(&mut open)
                .show(ctx, |ui| {
                    elided(ui, &r.prepared.source.display().to_string());
                    ui.add_space(a.sh.theme.d().gap);
                    if r.plan.nothing {
                        ui.label(a.sh.cat.t("merge.nothing"));
                    } else {
                        counts(ui, a, &r.plan);
                        if ui.button(a.sh.cat.t("collab_diff.reveal.start")).clicked() {
                            crate::reveal::request(crate::reveal::from_merge(&r.plan), false);
                        }
                    }
                    if !r.plan.conflicts.is_empty() {
                        ui.separator();
                        conflicts(ui, a, r, &mut rows);
                    }
                    if let Some(e) = &r.error {
                        ui.colored_label(a.sh.theme.c("warning"), e);
                    }
                    ui.add_space(a.sh.theme.d().gap);
                    kit::dialog_footer(ui, |ui| {
                        if r.plan.nothing {
                            if kit::primary_action(ui, a.sh.cat.t("merge.close"), None) {
                                close = true;
                            }
                        } else {
                            if kit::primary_action(ui, a.sh.cat.t("merge.apply"), None) {
                                applied = Some(());
                            }
                            if kit::cancel_button(ui, a.sh.cat.t("merge.cancel")).clicked() {
                                close = true;
                            }
                        }
                    });
                });
            close |= !open;
        }
    }
    if cancel && cancel_work(w).is_some() {
        a.sh.toasts.push(Level::Info, a.sh.cat.t("collab.cancelled"), None);
    }
    for Row::Show(t, id) in rows {
        select(a, &t, id);
    }
    if applied.is_some() {
        if let Stage::Review(r) = &mut w.stage {
            match apply(a, r) {
                Ok(rep) => {
                    let msg = a.sh.cat.tf(
                        "merge.done",
                        &[
                            ("added", &rep.added.to_string()),
                            ("changed", &rep.changed.to_string()),
                            ("deleted", &rep.deleted.to_string()),
                            ("skipped", &rep.skipped.to_string()),
                        ],
                    );
                    let msg = if rep.locked > 0 {
                        let more = a.sh.cat.tf("merge.locked", &[("n", &rep.locked.to_string())]);
                        format!("{msg} {more}")
                    } else {
                        msg
                    };
                    a.sh.toasts.push(Level::Info, msg, None);
                    close = true;
                }
                Err(e) => r.error = Some(a.sh.cat.tf("merge.failed", &[("e", &e)])),
            }
        }
    }
    if close {
        w.stage = Stage::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::job::JobState;
    use ddc_model::Model;
    use std::path::Path;
    use web_time::{Instant, SystemTime};

    fn scratch(name: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("ddc-mw-test-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// My project and a colleague's copy with one shared history; mine is closed, so its
    /// bytes on disk are final.
    fn pair(d: &Path) -> (Lineage, PathBuf, PathBuf) {
        let mine = d.join("mine.ddc");
        let theirs = d.join("theirs.ddc");
        let mut m = Model::open(mine.to_str().unwrap()).unwrap();
        m.add_level("L1", 0.0).unwrap();
        m.snapshot_to(&theirs).unwrap();
        let line = m.merge_lineage().unwrap();
        drop(m);
        (line, mine, theirs)
    }

    #[test]
    fn merge_cancel_while_working_leaves_project_unchanged() {
        let d = scratch("cancel");
        let (line, mine, theirs) = pair(&d);
        let before = std::fs::read(&mine).unwrap();
        let work = d.join("ddc-collab-1-0-merge");
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (m, wk, src) = (mine.to_string_lossy().into_owned(), work.clone(), theirs.clone());
        let job = Job::spawn(work.clone(), move |c| {
            let _ = gate.recv();
            worker(&line, &src, &wk, Some(&m), c)
        });
        let mut w = MergeWin {
            stage: Stage::Working(theirs, job),
            pending: None,
        };
        let mut job = cancel_work(&mut w).expect("was working");
        assert!(matches!(w.stage, Stage::Closed));
        assert_eq!(job.state(), JobState::Cancelled);
        go.send(()).unwrap();
        job.join();
        assert!(matches!(job.poll(), Polled::Pending), "late result is ignored");
        assert_eq!(std::fs::read(&mine).unwrap(), before);
        assert!(!work.exists(), "cancelled before any copy was made");
        assert!(cancel_work(&mut w).is_none(), "nothing to cancel once closed");
    }

    #[test]
    fn merge_cancel_after_copy_archives_it_and_leaves_project_unchanged() {
        let d = scratch("midstep");
        let (line, mine, theirs) = pair(&d);
        let before = std::fs::read(&mine).unwrap();
        let work = d.join("ddc-collab-1-2-merge");
        let (copied_tx, copied) = std::sync::mpsc::channel::<()>();
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (m, wk, src) = (mine.to_string_lossy().into_owned(), work.clone(), theirs.clone());
        let job = Job::spawn(work.clone(), move |c| {
            worker_steps(&line, &src, &wk, Some(&m), c, move || {
                let _ = copied_tx.send(());
                let _ = gate.recv();
            })
        });
        let mut w = MergeWin {
            stage: Stage::Working(theirs, job),
            pending: None,
        };
        copied.recv().expect("copy step reached");
        assert!(work.exists(), "the copy is in the work folder");
        let mut job = cancel_work(&mut w).expect("was working");
        go.send(()).unwrap();
        job.join();
        assert!(matches!(job.poll(), Polled::Pending), "late result is ignored");
        assert_eq!(std::fs::read(&mine).unwrap(), before);
        assert!(!work.exists());
        let kept = d.join("_archive").join("ddc-collab").join(work.file_name().unwrap());
        assert!(kept.is_dir(), "the copy is archived, not deleted");
    }

    #[test]
    fn merge_job_finishes_normally() {
        let d = scratch("done");
        let (line, mine, theirs) = pair(&d);
        let before = std::fs::read(&mine).unwrap();
        let work = d.join("ddc-collab-1-1-merge");
        let wk = work.clone();
        let mut job = Job::spawn(work.clone(), move |c| worker(&line, &theirs, &wk, None, c));
        let end = Instant::now() + Duration::from_secs(30);
        let got = loop {
            match job.poll() {
                Polled::Done(v) => break v,
                Polled::Lost => panic!("worker lost"),
                Polled::Pending => {
                    assert!(Instant::now() < end, "merge job timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        assert_eq!(job.state(), JobState::Done);
        let (prepared, plan) = got.expect("shared history");
        assert!(plan.is_none(), "no model file given: preview runs on the UI side");
        assert!(prepared.their.starts_with(&work) && prepared.their.is_file());
        assert_eq!(std::fs::read(&mine).unwrap(), before);
    }
}
