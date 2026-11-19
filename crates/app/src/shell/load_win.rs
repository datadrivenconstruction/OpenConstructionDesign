// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The loading window of every open and import: it shows in the frame the file is picked or
//! dropped, before the reader starts, and follows the real pipeline: stages with finished ones
//! checked, a bar from real counts, MB read or elements of the total, what is read now, elapsed
//! and remaining time, Cancel. A failure names its stage and the reason; a finished load shows
//! a short summary, then closes by itself (not while the pointer rests on it).
use super::EditorApp;
use crate::jobs::{clock, stage_index, Ended, JobView};
use crate::ui_kit::{self as kit, fmt_size};
use ddc_ui::theme::{layout, Role};
use ddc_ui::widgets::{self, shell_kit, Button};
use ddc_ui::{Catalog, Lang, Theme};
use eframe::egui::{self, Sense, Vec2};
use web_time::{Duration, Instant};

/// Window key (geometry file and egui id).
pub const KEY: &str = "ddc_load";
/// The picture of the job is taken at most this often, s (about ten updates a second).
pub const REFRESH: f64 = 0.1;
/// A finished load stays this long with its summary, s.
pub const DONE_HOLD: f64 = 3.0;
/// A cancelled load stays this long, s.
pub const CANCEL_HOLD: f64 = 2.5;
const STAGE_H: f32 = 22.0;
const INFO_H: f32 = 76.0;
const BTN_W: f32 = 112.0;
/// Last row of every stage list.
const DONE_STAGE: &str = "job.stage.done";

/// What a reader may name as its current work; anything else is the reader's own text.
pub const KINDS: &[&str] = &[
    "job.kind.wall",
    "job.kind.opening",
    "job.kind.slab",
    "job.kind.roof",
    "job.kind.ceiling",
    "job.kind.stair",
    "job.kind.curtain_wall",
    "job.kind.room",
    "job.kind.grid",
    "job.kind.mep",
    "job.kind.comp",
    "job.kind.mesh",
    "job.kind.other",
    "job.kind.level",
    "job.kind.types",
    "job.kind.elements",
    "job.kind.props",
    "job.kind.layers",
    "job.kind.blocks",
    "job.kind.entities",
    "job.kind.header",
];

/// How the load ended.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    Done {
        /// Elements this import added, when its report says.
        added: Option<usize>,
        /// Elements the project holds now (an open, or an import without a report).
        total: usize,
        warnings: usize,
        /// The import report can be opened.
        log: bool,
    },
    Failed {
        stage: &'static str,
        message: String,
    },
    Cancelled,
}

#[derive(Default)]
pub struct LoadWin {
    view: Option<JobView>,
    /// When `view` was taken.
    at: Option<Instant>,
    /// When the job was first seen: a result strip older than this is not its result.
    started: Option<Instant>,
    end: Option<(End, Instant)>,
    /// Pass the window was last drawn in: drawn once per pass, wherever the job started.
    drawn: Option<u64>,
    hovered: bool,
}

/// What the user pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Cancel,
    Close,
    Log,
}

impl LoadWin {
    /// Takes this frame's picture of the running load; true when the load just ended.
    /// A new picture is taken at most every [`REFRESH`] s, at once on a stage or cancel change.
    pub fn step(&mut self, now: Instant, live: Option<JobView>) -> bool {
        match live {
            Some(v) => {
                if self.view.is_none() || self.end.is_some() {
                    // a new load: an outcome recorded before it is not its own
                    let _ = crate::jobs::take_load_end();
                    *self = LoadWin {
                        drawn: self.drawn,
                        started: Some(now),
                        ..LoadWin::default()
                    };
                }
                let fresh = match (&self.view, self.at) {
                    (Some(o), Some(at)) => {
                        o.stage != v.stage
                            || o.cancelling != v.cancelling
                            || o.file != v.file
                            || now.duration_since(at).as_secs_f64() >= REFRESH
                    }
                    _ => true,
                };
                if fresh {
                    self.view = Some(v);
                    self.at = Some(now);
                }
                false
            }
            None => self.view.is_some() && self.end.is_none(),
        }
    }

    /// Layout probes and screenshots: a window that shows `view`, ended as `end`.
    #[cfg(test)]
    pub(crate) fn parked(view: JobView, end: Option<End>) -> Self {
        let now = Instant::now();
        LoadWin {
            view: Some(view),
            at: Some(now),
            started: Some(now),
            end: end.map(|e| (e, now)),
            ..LoadWin::default()
        }
    }

    /// The picture shown now, if any.
    pub fn view(&self) -> Option<&JobView> {
        self.view.as_ref()
    }

    pub fn end(&self) -> Option<&End> {
        self.end.as_ref().map(|e| &e.0)
    }

    /// Whether an ended load leaves the screen now.
    fn closes(&self, now: Instant) -> bool {
        let Some((e, at)) = &self.end else {
            return false;
        };
        let ago = now.duration_since(*at).as_secs_f64();
        match e {
            End::Failed { .. } => false,
            End::Done { .. } => ago >= DONE_HOLD && !self.hovered,
            End::Cancelled => ago >= CANCEL_HOLD && !self.hovered,
        }
    }
}

/// The running open or import, from whichever door started it.
pub fn live(app: &EditorApp) -> Option<JobView> {
    app.big
        .load_view()
        .or_else(|| crate::project_ui::open_view(app))
        // the Import form draws its own progress while it is open
        .or_else(|| {
            (!app.import_open)
                .then(|| super::tasks_ui::import_view(app))
                .flatten()
        })
}

/// The loading window shows the running job (the task window leaves it alone).
pub fn covers(app: &EditorApp) -> bool {
    live(app).is_some()
}

/// How the load seen last as `v` ended: the job's own outcome, else its result strip.
fn outcome(app: &EditorApp, v: &JobView, since: Option<Instant>) -> End {
    let ru = app.sh.cat.lang == Lang::Ru;
    let failed = |message: String| End::Failed {
        stage: v.stage,
        message: crate::ui_err::humanize(&message, ru),
    };
    match crate::jobs::take_load_end() {
        Some(e) if e.ended == Ended::Cancelled => return End::Cancelled,
        Some(e) if e.ended == Ended::Failed => {
            return failed(e.reason.unwrap_or_else(|| app.sh.cat.t("job.failed")))
        }
        // a module import has no job record: a cancel it was asked for is how it ended
        None if v.cancelling => return End::Cancelled,
        _ => {}
    }
    let strip = since.and_then(|t| crate::open::summary::strip_since(app, t));
    let total = |s: &ddc_io::import_summary::ImportSummary| s.native_total() + s.mesh_total();
    match strip {
        Some(Err(message)) => failed(message),
        Some(Ok(s)) => End::Done {
            added: Some(total(&s)),
            total: total(&s),
            warnings: s.notes_total,
            log: true,
        },
        None => {
            let s = crate::open::summary::from_model(&app.ed.model, "");
            End::Done {
                added: None,
                total: total(&s),
                warnings: 0,
                log: false,
            }
        }
    }
}

/// Per frame (after the jobs were polled, and again at the end of the frame for a job a
/// command or a drop started in it): the loading window.
pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    if app.chrome.load.drawn == Some(pass) {
        return;
    }
    let now = Instant::now();
    let live = live(app);
    if app.chrome.load.step(now, live) {
        if let Some(v) = app.chrome.load.view.clone() {
            let e = outcome(app, &v, app.chrome.load.started);
            app.chrome.load.end = Some((e, now));
        }
    }
    if app.chrome.load.closes(now) {
        close(app, pass);
        return;
    }
    draw(app, ctx);
}

/// The window as the state holds it (no polling): layout probes and screenshots use it too.
pub fn draw(app: &mut EditorApp, ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    let Some(v) = app.chrome.load.view.clone() else {
        return;
    };
    let end = app.chrome.load.end().cloned();
    let mut act = None;
    let (th, c) = (&app.sh.theme, &app.sh.cat);
    let shown = kit::Dialog::hud(KEY, c.t(v.title), egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .size([layout::DIALOG_FORM, 0.0])
        .stable()
        .no_title_bar()
        .show(ctx, |ui| body(ui, th, c, &v, end.as_ref(), &mut act));
    app.chrome.load.drawn = Some(pass);
    app.chrome.load.hovered = shown.is_some_and(|s| s.response.contains_pointer());
    ctx.request_repaint_after(Duration::from_millis((REFRESH * 1000.0) as u64));
    match act {
        Some(Act::Cancel) => cancel(app),
        Some(Act::Close) => close(app, pass),
        Some(Act::Log) => {
            close(app, pass);
            app.run("file.import_review");
        }
        None => {}
    }
}

fn close(app: &mut EditorApp, pass: u64) {
    app.chrome.load = LoadWin {
        drawn: Some(pass),
        ..LoadWin::default()
    };
}

/// Stops the running load at its next check; the model stays as it was.
pub fn cancel(app: &mut EditorApp) {
    if app.big.loading() {
        app.big.cancel_job();
    } else if crate::project_ui::open_view(app).is_some() {
        crate::project_ui::cancel_open(app);
    } else if let Some(j) = &app.import_job {
        j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The text of what the stage works on now: a known kind in the UI language, else as given.
pub fn detail_text(c: &Catalog, detail: &str) -> String {
    if KINDS.contains(&detail) {
        c.t(detail)
    } else {
        detail.to_string()
    }
}

/// `5,230 of 12,480`, `12.4 MB of 186.0 MB read`, or what was done when the total is unknown.
pub fn counts_text(c: &Catalog, v: &JobView) -> String {
    let ru = c.lang == Lang::Ru;
    let (d, t) = v.counts;
    let n = |x: u64| {
        if v.bytes {
            fmt_size(c, x)
        } else {
            crate::open::summary::grp(x as usize, ru)
        }
    };
    if t > 0 {
        let key = if v.bytes {
            "load.count.bytes"
        } else {
            "load.count.items"
        };
        c.tf(key, &[("d", n(d.min(t)).as_str()), ("t", n(t).as_str())])
    } else if d > 0 {
        c.tf("load.count.some", &[("d", n(d).as_str())])
    } else {
        String::new()
    }
}

/// `Elapsed 0:12 · about 0:30 left`; no estimate while the amount of work is unknown.
pub fn time_text(c: &Catalog, v: &JobView) -> String {
    let mut s = c.tf("load.elapsed", &[("t", clock(v.elapsed).as_str())]);
    let left = match v.eta {
        Some(t) => Some(c.tf("load.left", &[("t", clock(t).as_str())])),
        None if !v.indeterminate => Some(c.t("load.estimating")),
        None => None,
    };
    if let Some(l) = left {
        s = format!("{s} · {l}");
    }
    s
}

/// The summary lines of an ended load.
pub fn end_lines(c: &Catalog, e: &End) -> Vec<String> {
    let ru = c.lang == Lang::Ru;
    match e {
        End::Done {
            added,
            total,
            warnings,
            ..
        } => {
            let first = match added {
                Some(n) => c.tf(
                    "load.done.added",
                    &[("n", &crate::open::summary::grp(*n, ru))],
                ),
                None => c.tf(
                    "load.done.total",
                    &[("n", &crate::open::summary::grp(*total, ru))],
                ),
            };
            let second = if *warnings > 0 {
                c.tf("load.done.warnings", &[("n", &warnings.to_string())])
            } else {
                c.t("load.done.clean")
            };
            vec![first, second]
        }
        End::Failed { stage, message } => {
            vec![
                c.tf("load.failed.at", &[("stage", &c.t(stage))]),
                message.clone(),
            ]
        }
        End::Cancelled => vec![c.t("load.cancelled")],
    }
}

fn body(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    v: &JobView,
    end: Option<&End>,
    act: &mut Option<Act>,
) {
    let primary = th.c("text.primary");
    let sec = th.c("text.secondary");
    let gap = th.d().gap;
    let mut title = c.t(v.title);
    if let Some(f) = &v.file {
        title = format!("{title} · {f}");
    }
    let g = shell_kit::elided(
        ui,
        &title,
        th.font(Role::BodyStrong),
        primary,
        ui.available_width(),
    );
    let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, primary);
    resp.on_hover_text(&title);
    kit::fixed_row(ui, "load_size", |ui| {
        if let Some(n) = v.size {
            widgets::elided_label(ui, &fmt_size(c, n), th.font(Role::Caption), sec);
        }
    });
    ui.add_space(gap);
    stages(ui, th, c, v, end);
    ui.add_space(gap);
    let done = matches!(end, Some(End::Done { .. }));
    let mut bar = v.clone();
    if done {
        bar.fraction = 1.0;
        bar.indeterminate = false;
    }
    ui.horizontal(|ui| {
        let pc_w = 48.0;
        let w = (ui.available_width() - pc_w - ui.spacing().item_spacing.x).max(0.0);
        let (track, _) = ui.allocate_exact_size(Vec2::new(w, 18.0), Sense::hover());
        let rect = egui::Rect::from_center_size(track.center(), Vec2::new(w, 6.0));
        shell_kit::paint_bar(ui, th, rect, (!bar.indeterminate).then_some(bar.fraction));
        let pc = if bar.indeterminate {
            String::new()
        } else {
            format!("{:.0} %", bar.fraction.clamp(0.0, 1.0) * 100.0)
        };
        kit::fixed_cell(ui, pc_w, |ui| {
            widgets::elided_label(ui, &pc, th.font(Role::Numeric), primary);
        });
    });
    ui.add_space(gap);
    kit::zone(ui, "load_info", INFO_H, |ui| match end {
        None => {
            let rows = [
                counts_text(c, v),
                if v.detail.is_empty() {
                    String::new()
                } else {
                    c.tf("load.now", &[("what", &detail_text(c, &v.detail))])
                },
                time_text(c, v),
            ];
            for row in rows {
                kit::fixed_row(ui, "load_row", |ui| {
                    widgets::elided_label(ui, &row, th.font(Role::Body), sec);
                });
            }
            if !v.cancellable {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(c.t("job.no_cancel"))
                            .font(th.font(Role::Caption))
                            .color(sec),
                    )
                    .wrap(),
                );
            }
        }
        Some(e) => {
            let col = match e {
                End::Failed { .. } => th.c("danger"),
                End::Cancelled => th.c("warning"),
                End::Done { .. } => primary,
            };
            for (i, line) in end_lines(c, e).into_iter().enumerate() {
                let tone = if i == 0 { col } else { sec };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .font(th.font(Role::Body))
                            .color(tone),
                    )
                    .wrap(),
                );
            }
        }
    });
    ui.add_space(gap);
    footer(ui, th, c, v, end, act);
}

fn footer(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    v: &JobView,
    end: Option<&End>,
    act: &mut Option<Act>,
) {
    let size = Vec2::new(ui.available_width(), ui.spacing().interact_size.y);
    let lay = egui::Layout::right_to_left(egui::Align::Center);
    ui.allocate_ui_with_layout(size, lay, |ui| match end {
        None if v.cancellable => {
            let label = if v.cancelling {
                c.t("job.cancelling")
            } else {
                c.t("job.cancel")
            };
            let r = ui.add_enabled_ui(!v.cancelling, |ui| {
                Button::secondary(&label).min_width(BTN_W).show(ui, th)
            });
            if r.inner.clicked() {
                *act = Some(Act::Cancel);
            }
        }
        None => {}
        Some(e) => {
            let close = c.t("load.close");
            let log = matches!(e, End::Done { log: true, .. });
            let main = if log {
                Button::secondary(&close).min_width(BTN_W).show(ui, th)
            } else {
                Button::primary(&close).min_width(BTN_W).show(ui, th)
            };
            if main.clicked() {
                *act = Some(Act::Close);
            }
            if log {
                let label = c.t("load.log");
                if Button::primary(&label)
                    .min_width(BTN_W)
                    .show(ui, th)
                    .clicked()
                {
                    *act = Some(Act::Log);
                }
            }
        }
    });
}

/// How a stage row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Done,
    Current,
    Failed,
    Ahead,
}

/// Mark of row `i` of `n` rows (the last is "Done") with the job at `cur`.
pub fn mark(i: usize, n: usize, cur: usize, end: Option<&End>) -> Mark {
    let last = n.saturating_sub(1);
    match end {
        Some(End::Done { .. }) => Mark::Done,
        Some(End::Failed { .. }) | Some(End::Cancelled) if i == cur => Mark::Failed,
        _ if i < cur => Mark::Done,
        None if i == cur && i < last => Mark::Current,
        _ => Mark::Ahead,
    }
}

fn stages(ui: &mut egui::Ui, th: &Theme, c: &Catalog, v: &JobView, end: Option<&End>) {
    let one = [v.stage];
    let list: &[&str] = if v.stages.is_empty() { &one } else { v.stages };
    let cur = stage_index(list, v.stage);
    let n = list.len() + 1;
    for (i, s) in list.iter().copied().chain([DONE_STAGE]).enumerate() {
        let m = mark(i, n, cur, end);
        let (icon, text) = match m {
            Mark::Done => (th.c("success"), th.c("text.secondary")),
            Mark::Current => (th.c("accent"), th.c("text.primary")),
            Mark::Failed => (th.c("danger"), th.c("text.primary")),
            Mark::Ahead => (th.c("text.disabled"), th.c("text.disabled")),
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(18.0, STAGE_H), Sense::hover());
            let p = ui.painter();
            let o = r.center();
            let stroke = egui::Stroke::new(1.6, icon);
            match m {
                Mark::Done => {
                    let pts = [
                        o + Vec2::new(-4.5, 0.0),
                        o + Vec2::new(-1.5, 3.0),
                        o + Vec2::new(4.5, -3.5),
                    ];
                    p.line_segment([pts[0], pts[1]], stroke);
                    p.line_segment([pts[1], pts[2]], stroke);
                }
                Mark::Current => {
                    p.circle_filled(o, 4.5, icon);
                }
                Mark::Failed => {
                    let d = 3.5;
                    p.line_segment([o + Vec2::new(-d, -d), o + Vec2::new(d, d)], stroke);
                    p.line_segment([o + Vec2::new(-d, d), o + Vec2::new(d, -d)], stroke);
                }
                Mark::Ahead => {
                    p.circle_stroke(o, 3.5, egui::Stroke::new(1.0, icon));
                }
            }
            let font = th.font(if m == Mark::Current {
                Role::BodyStrong
            } else {
                Role::Body
            });
            widgets::elided_label(ui, &c.t(s), font, text);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(stage: &'static str, d: u64, t: u64) -> JobView {
        JobView {
            title: "job.title.import",
            stage,
            stages: crate::bigfile::STAGES_IFC,
            fraction: 0.5,
            indeterminate: t == 0,
            counts: (d, t),
            cancellable: true,
            file: Some("a.ifc".into()),
            ..JobView::default()
        }
    }

    #[test]
    fn the_picture_is_taken_ten_times_a_second_and_at_once_on_a_stage_change() {
        let mut w = LoadWin::default();
        let t0 = Instant::now();
        assert!(!w.step(t0, Some(job("job.stage.read", 1, 10))));
        assert_eq!(w.view().unwrap().counts, (1, 10));
        let t1 = t0 + Duration::from_millis(30);
        w.step(t1, Some(job("job.stage.read", 2, 10)));
        assert_eq!(w.view().unwrap().counts, (1, 10), "held under 100 ms");
        w.step(t1, Some(job("job.stage.plan", 0, 5)));
        assert_eq!(
            w.view().unwrap().stage,
            "job.stage.plan",
            "a new stage shows at once"
        );
        let t2 = t1 + Duration::from_millis(120);
        w.step(t2, Some(job("job.stage.plan", 3, 5)));
        assert_eq!(w.view().unwrap().counts, (3, 5));
        assert!(w.step(t2, None), "the end is seen once");
    }

    #[test]
    fn rows_check_what_is_done_and_mark_where_it_stopped() {
        let n = 4;
        assert_eq!(mark(0, n, 1, None), Mark::Done);
        assert_eq!(mark(1, n, 1, None), Mark::Current);
        assert_eq!(mark(2, n, 1, None), Mark::Ahead);
        assert_eq!(mark(3, n, 1, None), Mark::Ahead);
        let failed = End::Failed {
            stage: "job.stage.plan",
            message: "x".into(),
        };
        assert_eq!(mark(1, n, 1, Some(&failed)), Mark::Failed);
        assert_eq!(mark(0, n, 1, Some(&failed)), Mark::Done);
        let done = End::Done {
            added: Some(1),
            total: 1,
            warnings: 0,
            log: false,
        };
        assert!((0..n).all(|i| mark(i, n, 1, Some(&done)) == Mark::Done));
    }

    #[test]
    fn counters_read_bytes_and_elements_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let mut v = job("job.stage.commit", 5_230, 12_480);
            let s = counts_text(&c, &v);
            assert!(
                s.contains("12") && s.contains("480") && s.contains("230"),
                "{s}"
            );
            v.bytes = true;
            v.counts = (12 << 20, 186 << 20);
            let s = counts_text(&c, &v);
            assert!(s.contains("186") && s.contains("12"), "{s}");
            v.counts = (0, 0);
            assert_eq!(counts_text(&c, &v), "");
            v.detail = "job.kind.wall".into();
            assert_ne!(detail_text(&c, &v.detail), "job.kind.wall");
            assert_eq!(detail_text(&c, "Walls (module)"), "Walls (module)");
            v.elapsed = 12.0;
            v.eta = Some(30.0);
            let t = time_text(&c, &v);
            assert!(t.contains("0:12") && t.contains("0:30"), "{t}");
        }
    }

    #[test]
    fn every_key_the_window_shows_is_in_both_catalogs() {
        let mut keys: Vec<&str> = KINDS.to_vec();
        keys.extend([
            DONE_STAGE,
            "job.stage.parse",
            "job.stage.check",
            "load.count.bytes",
            "load.count.items",
            "load.count.some",
            "load.now",
            "load.elapsed",
            "load.left",
            "load.estimating",
            "load.close",
            "load.log",
            "load.done.added",
            "load.done.total",
            "load.done.warnings",
            "load.done.clean",
            "load.failed.at",
            "load.cancelled",
        ]);
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            for k in &keys {
                assert_ne!(c.t(k), *k, "{k} missing in {lang:?}");
            }
        }
    }

    const DXF: &str = "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n0\n20\n0\n11\n5\n21\n5\n\
                       0\nENDSEC\n0\nEOF\n";

    /// A drawing whose reader waits (`<file>.hold`) until the test lets it go.
    fn held_dxf(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_imp1_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("plan.dxf");
        std::fs::write(&p, DXF).unwrap();
        std::fs::write(p.with_extension("hold"), b"").unwrap();
        p
    }

    fn release(p: &std::path::Path) {
        std::fs::remove_file(p.with_extension("hold")).unwrap();
    }

    fn drive(a: &mut EditorApp, ctx: &egui::Context, th: &mut Theme) {
        let id = egui::Id::new(("ddc.dialog", KEY));
        let t = Instant::now();
        while a.big.load_view().is_some() {
            assert!(t.elapsed() < Duration::from_secs(30), "the job never ended");
            std::thread::sleep(Duration::from_millis(10));
            let mut f = |ctx: &egui::Context| crate::bigfile::frame(a, ctx);
            crate::layout_contract::pass(ctx, th, id, vec![], &mut f);
        }
    }

    #[test]
    fn the_window_is_up_before_the_reader_reads_a_byte() {
        let p = held_dxf("gate");
        let mut a = crate::layout_contract::app_in(Lang::En);
        let (ctx, mut th) = crate::layout_contract::driven_ctx();
        let id = egui::Id::new(("ddc.dialog", KEY));
        let mut f = |ctx: &egui::Context| {
            crate::bigfile::start(&mut a, p.clone());
            window(&mut a, ctx);
        };
        crate::layout_contract::pass(&ctx, &mut th, id, vec![], &mut f);
        let v = a
            .chrome
            .load
            .view()
            .expect("the window state exists in the pick frame")
            .clone();
        assert_eq!(v.counts, (0, 0), "nothing read yet");
        assert_eq!(v.stage, "job.stage.start");
        assert_eq!(v.file.as_deref(), Some("plan.dxf"));
        assert_eq!(v.size, Some(DXF.len() as u64));
        assert!(a.chrome.load.drawn.is_some(), "drawn in the same pass");
        // the first pass of a dialog only sizes it; the next one paints, the reader still held
        let mut f = |ctx: &egui::Context| window(&mut a, ctx);
        let probe = crate::layout_contract::pass(&ctx, &mut th, id, vec![], &mut f);
        assert_eq!(a.chrome.load.view().unwrap().counts, (0, 0), "still nothing read");
        assert!(
            probe.text.iter().any(|t| t.contains("plan.dxf")),
            "{:?}",
            probe.text
        );
        release(&p);
        drive(&mut a, &ctx, &mut th);
        assert!(
            matches!(a.chrome.load.end(), Some(End::Done { .. })),
            "{:?}",
            a.chrome.load.end()
        );
    }

    #[test]
    fn a_cancel_from_the_window_leaves_the_model_as_it_was() {
        let p = held_dxf("cancel");
        let mut a = crate::layout_contract::app_in(Lang::Ru);
        let head = a.ed.model.change_head();
        let (ctx, mut th) = crate::layout_contract::driven_ctx();
        let id = egui::Id::new(("ddc.dialog", KEY));
        let mut f = |ctx: &egui::Context| {
            crate::bigfile::start(&mut a, p.clone());
            window(&mut a, ctx);
        };
        crate::layout_contract::pass(&ctx, &mut th, id, vec![], &mut f);
        cancel(&mut a);
        release(&p);
        drive(&mut a, &ctx, &mut th);
        assert_eq!(a.chrome.load.end(), Some(&End::Cancelled));
        assert_eq!(a.ed.model.change_head(), head, "the model is unchanged");
    }

    #[test]
    fn done_closes_by_itself_failed_waits_for_the_user() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs_f64(DONE_HOLD + 0.1);
        let mut w = LoadWin::default();
        w.end = Some((
            End::Done {
                added: None,
                total: 3,
                warnings: 0,
                log: false,
            },
            t0,
        ));
        assert!(!w.closes(t0));
        assert!(w.closes(later));
        w.hovered = true;
        assert!(!w.closes(later), "not while the pointer rests on it");
        w.hovered = false;
        w.end = Some((
            End::Failed {
                stage: "job.stage.read",
                message: "x".into(),
            },
            t0,
        ));
        assert!(!w.closes(later + Duration::from_secs(60)));
    }
}
