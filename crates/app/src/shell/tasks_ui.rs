// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Long tasks: status bar pill and the stage window. Under 0.3 s nothing shows, then the status
//! bar with cancel; after 10 s, or on a click, the window with stages, remaining time, cancel.
use super::EditorApp;
use crate::jobs::{
    calm_visible, stage_index, CalmIn, Ended, JobEnd, JobView, CALM_SHOW_AFTER,
};
use ddc_ui::theme::{layout, Role};
use ddc_ui::widgets::{shell_kit, Button};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText, Sense, Vec2};
use std::sync::atomic::Ordering;
use web_time::{Duration, Instant};

pub const PILL_AFTER: f64 = CALM_SHOW_AFTER;
pub const WINDOW_AFTER: f64 = 10.0;
const FOLD_W: f32 = 36.0;
const CANCEL_W: f32 = 96.0;

#[derive(Default)]
pub struct TasksUi {
    /// The user folded the window to the status bar.
    pub hidden: bool,
    /// The user asked for the window before the 10 s mark.
    pub expanded: bool,
    /// Height of the window drawn this frame: toasts stack above it.
    pub window_h: f32,
    /// The view canvas of the last frame: the card sits inside it.
    pub canvas: Option<egui::Rect>,
    since: Option<Instant>,
    pub calm: Calm,
}

/// Keeps the pill steady: a late start, a minimum time on screen and a short hold of the last frame at the end.
#[derive(Default)]
pub struct Calm {
    shown_at: Option<f64>,
    ended: Option<(Ended, f64)>,
    end: Option<JobEnd>,
    last: Option<JobView>,
}

impl Calm {
    /// The job to draw this frame. `now` is the UI clock, `live` the running job, `end` the
    /// outcome its poll recorded (read once the job has left `live`).
    pub fn step(
        &mut self,
        now: f64,
        live: Option<JobView>,
        end: Option<JobEnd>,
        expanded: bool,
    ) -> Option<JobView> {
        let mut view = match live {
            Some(v) => {
                self.ended = None;
                self.end = None;
                self.last = Some(v.clone());
                v
            }
            None => {
                // the hold freezes the last picture; failed and cancelled carry their own state
                let mut v = self.last.clone()?;
                let e = self
                    .end
                    .get_or_insert_with(|| end.unwrap_or_else(JobEnd::done))
                    .clone();
                self.ended.get_or_insert((e.ended, now));
                if e.ended != Ended::Done {
                    v.fraction = v.fraction.min(0.99);
                    v.end = Some(e);
                }
                v.cancellable = false;
                v.cancelling = false;
                v.eta = None;
                v
            }
        };
        let shown_for = self.shown_at.map(|s| now - s);
        let visible = expanded
            || calm_visible(CalmIn {
                elapsed: view.elapsed,
                shown_for,
                urgent: view.cancelling,
                ended: self.ended.map(|(e, at)| (e, now - at)),
            });
        if !visible {
            if self.ended.is_some() {
                *self = Calm::default();
            }
            return None;
        }
        self.shown_at.get_or_insert(now);
        view.cancellable &= self.ended.is_none();
        Some(view)
    }
}

pub fn show_pill(elapsed: f64, expanded: bool) -> bool {
    expanded || elapsed >= PILL_AFTER
}

pub fn show_window(elapsed: f64, expanded: bool, hidden: bool) -> bool {
    !hidden && (expanded || elapsed >= WINDOW_AFTER)
}

/// What the user pressed on the status bar pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillAct {
    Open,
    Cancel,
    /// Run this command again.
    Retry(&'static str),
}

/// The running task, if any: a big-file job, else a module import.
pub fn current(app: &EditorApp) -> Option<JobView> {
    app.big
        .view()
        .or_else(|| crate::save::pill::view(app))
        .or_else(|| import_view(app))
}

/// The running module import, which the Import window also draws.
pub fn import_view(app: &EditorApp) -> Option<JobView> {
    let j = app.import_job.as_ref()?;
    let (d, t) = j.progress;
    let elapsed = app
        .chrome
        .tasks
        .since
        .map_or(0.0, |s| s.elapsed().as_secs_f64());
    Some(JobView {
        title: "job.title.import",
        stage: "job.stage.bridge",
        stages: &["job.stage.bridge"],
        fraction: if t > 0 { d as f32 / t as f32 } else { 0.0 },
        indeterminate: t == 0,
        elapsed,
        eta: None,
        counts: (d, t),
        cancelling: j.cancel.load(Ordering::Relaxed),
        cancellable: true,
        end: None,
        file: j
            .input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        ..JobView::default()
    })
}

pub fn cancel(app: &mut EditorApp) {
    if app.big.busy() {
        app.big.cancel_job();
    } else if crate::save::busy(app) {
        crate::save::cancel(app);
    } else if let Some(j) = &app.import_job {
        j.cancel.store(true, Ordering::Relaxed);
    }
}

fn percent(v: &JobView) -> String {
    if v.indeterminate {
        String::new()
    } else {
        format!("{:.0} %", v.fraction.clamp(0.0, 1.0) * 100.0)
    }
}

/// Pill at the right of the status bar. The layout is right-to-left there.
pub fn pill(ui: &mut egui::Ui, th: &Theme, c: &Catalog, v: &JobView) -> Option<PillAct> {
    if let Some(end) = &v.end {
        return end_pill(ui, th, c, v, end);
    }
    let mut act = None;
    if v.cancellable
        && ui
            .add_enabled(
                !v.cancelling,
                egui::Button::new(RichText::new(c.t("job.cancel")).font(th.font(Role::Caption)))
                    .frame(false),
            )
            .clicked()
    {
        act = Some(PillAct::Cancel);
    }
    let (track, _) = ui.allocate_exact_size(Vec2::new(64.0, 18.0), Sense::hover());
    let bar = egui::Rect::from_center_size(track.center(), Vec2::new(64.0, 4.0));
    let fraction = (!v.indeterminate).then_some(v.fraction);
    shell_kit::paint_bar(ui, th, bar, fraction);
    let mut text = c.t(v.title);
    if let Some(f) = &v.file {
        text = format!("{text} · {f}");
    }
    let pc = percent(v);
    if !pc.is_empty() {
        text = format!("{pc}  {text}");
    }
    let g = shell_kit::elided(
        ui,
        &text,
        th.font(Role::Caption),
        th.c("text.primary"),
        170.0,
    );
    let (r, resp) = ui.allocate_exact_size(g.size(), Sense::click());
    let col = th.c("text.primary");
    ui.painter().galley(r.min, g, col);
    let tip = if v.cancellable {
        c.t(v.stage)
    } else {
        format!("{}\n{}", c.t(v.stage), c.t("job.no_cancel"))
    };
    if resp.on_hover_text(tip).clicked() {
        act = act.or(Some(PillAct::Open));
    }
    act
}

/// A failed or cancelled job: its own state, reason on hover, Retry where the job can repeat.
fn end_pill(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    v: &JobView,
    end: &JobEnd,
) -> Option<PillAct> {
    let mut act = None;
    if let Some(cmd) = end.retry {
        let b = egui::Button::new(RichText::new(c.t("job.retry")).font(th.font(Role::Caption)))
            .frame(false);
        if ui.add(b).clicked() {
            act = Some(PillAct::Retry(cmd));
        }
    }
    let (state, col) = if end.ended == Ended::Failed {
        (c.t("job.state.failed"), th.c("danger"))
    } else {
        (c.t("job.state.cancelled"), th.c("warning"))
    };
    let mut text = format!("{state} · {}", c.t(v.title));
    if let Some(f) = &v.file {
        text = format!("{text} · {f}");
    }
    let g = shell_kit::elided(ui, &text, th.font(Role::Caption), col, 220.0);
    let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, col);
    let tip = match &end.reason {
        Some(why) => format!("{text}
{why}"),
        None => text,
    };
    resp.on_hover_text(tip);
    act
}

/// Per frame: keeps the elapsed clock of the current task and draws the stage window.
pub fn window(app: &mut EditorApp, ctx: &egui::Context) {
    app.chrome.tasks.window_h = 0.0;
    let running = app.big.busy() || app.import_job.is_some();
    if !running {
        app.chrome.tasks.since = None;
        app.chrome.tasks.hidden = false;
        app.chrome.tasks.expanded = false;
        return;
    }
    app.chrome.tasks.since.get_or_insert_with(Instant::now);
    let Some(v) = current(app) else { return };
    ctx.request_repaint_after(Duration::from_millis(100));
    if super::load_win::covers(app) {
        // the loading window shows this job from its first frame
        return;
    }
    // the import form draws its own progress while it is open
    let own_form = !app.big.busy() && app.import_open;
    let t = &app.chrome.tasks;
    if own_form || !show_window(v.elapsed, t.expanded, t.hidden) {
        return;
    }
    let mut fold = false;
    let mut stop = false;
    let early = app.open.early.texture();
    let th = &app.sh.theme;
    let c = &app.sh.cat;
    let screen = ctx.screen_rect();
    let canvas = app.chrome.tasks.canvas.filter(|r| r.is_positive()).unwrap_or(screen);
    let room = canvas.width() - 2.0 * ddc_ui::theme::space::MD;
    let shown = crate::ui_kit::Dialog::hud(
        "ddc_task",
        c.t(v.title),
        egui::Align2::RIGHT_BOTTOM,
        card_offset(screen, canvas),
    )
    .size([layout::TOAST_W.min(room), 0.0])
    .stable()
    .no_title_bar()
    .show(ctx, |ui| {
        let wd = ui.available_width();
        body(ui, th, c, &v, Some(&mut fold), &mut stop);
        // plan outlines of what the build has written so far
        if let Some(t) = early {
            ui.add_space(6.0);
            let w = wd.min(t.size_vec2().x);
            let h = w * t.size_vec2().y / t.size_vec2().x.max(1.0);
            ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::new(w, h)));
        }
    });
    if let Some(s) = shown {
        // toasts stack above the card, measured from the status bar
        let row = app.sh.theme.d().row;
        app.chrome.tasks.window_h = (screen.bottom() - s.response.rect.top() - row).max(1.0);
    }
    if fold {
        app.chrome.tasks.hidden = true;
        app.chrome.tasks.expanded = false;
    }
    if stop {
        cancel(app);
    }
}

/// Title row, stages, bar and a line with progress and Cancel. Width comes from the window; a
/// missing `fold` hides the fold button.
pub(crate) fn body(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &Catalog,
    v: &JobView,
    fold: Option<&mut bool>,
    stop: &mut bool,
) {
    ui.horizontal(|ui| {
        let mut title = c.t(v.title);
        if let Some(f) = &v.file {
            title = format!("{title} · {f}");
        }
        let col = th.c("text.primary");
        let w = ui.available_width() - if fold.is_some() { FOLD_W } else { 0.0 };
        let g = shell_kit::elided(ui, &title, th.font(Role::BodyStrong), col, w);
        let (r, resp) = ui.allocate_exact_size(g.size(), Sense::hover());
        ui.painter().galley(r.min, g, col);
        resp.on_hover_text(title);
        if let Some(fold) = fold {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shell_kit::chevron_button(ui, th, false, &c.t("job.minimize")).clicked() {
                    *fold = true;
                }
            });
        }
    });
    ui.add_space(6.0);
    stage_rows(ui, th, c, v);
    ui.add_space(6.0);
    run_bar(ui, th, v);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let w = ui.available_width()
            - if v.cancellable {
                CANCEL_W + ui.spacing().item_spacing.x
            } else {
                0.0
            };
        crate::ui_kit::fixed_cell(ui, w, |ui| {
            let info = run_info(c, v);
            ddc_ui::widgets::elided_label(
                ui,
                &info,
                th.font(Role::Numeric),
                th.c("text.secondary"),
            );
        });
        if v.cancellable {
            let label = if v.cancelling {
                c.t("job.cancelling")
            } else {
                c.t("job.cancel")
            };
            let r = ui.add_enabled_ui(!v.cancelling, |ui| {
                Button::secondary(&label).min_width(CANCEL_W).show(ui, th)
            });
            if r.inner.clicked() {
                *stop = true;
            }
        }
    });
    if !v.cancellable {
        ui.add(
            egui::Label::new(
                RichText::new(c.t("job.no_cancel"))
                    .font(th.font(Role::Caption))
                    .color(th.c("text.secondary")),
            )
            .wrap(),
        );
    }
}

/// The stage list; a job without a list shows its current stage alone.
pub(crate) fn stage_rows(ui: &mut egui::Ui, th: &Theme, c: &Catalog, v: &JobView) {
    let sec = th.c("text.secondary");
    let one = [v.stage];
    let stages: &[&str] = if v.stages.is_empty() { &one } else { v.stages };
    let cur = stage_index(stages, v.stage);
    for (i, s) in stages.iter().enumerate() {
        let (dot_col, text_col) = if i < cur {
            (th.c("success"), sec)
        } else if i == cur {
            (th.c("accent"), th.c("text.primary"))
        } else {
            (th.c("text.disabled"), th.c("text.disabled"))
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::new(14.0, 18.0), Sense::hover());
            if i > cur {
                ui.painter()
                    .circle_stroke(r.center(), 3.5, egui::Stroke::new(1.0, dot_col));
            } else {
                ui.painter().circle_filled(r.center(), 4.0, dot_col);
            }
            let font = th.font(if i == cur {
                Role::BodyStrong
            } else {
                Role::Body
            });
            ddc_ui::widgets::elided_label(ui, &c.t(s), font, text_col);
        });
    }
}

/// The bar across the available width: a pulse when the amount of work is unknown.
pub(crate) fn run_bar(ui: &mut egui::Ui, th: &Theme, v: &JobView) {
    let w = ui.available_width();
    let (track, _) = ui.allocate_exact_size(Vec2::new(w, 6.0), Sense::hover());
    shell_kit::paint_bar(ui, th, track, (!v.indeterminate).then_some(v.fraction));
}

/// Stage, counted work and the time left in words: «Reading elements · 40 of 100 · about
/// 2 min left». The bar beside it already shows the percent.
pub(crate) fn run_info(c: &Catalog, v: &JobView) -> String {
    let mut parts = vec![c.t(v.stage)];
    let (d, t) = v.counts;
    if t > 0 {
        // a byte-counting stage reads in MB, the others count entities, rows or files
        let (key, d, t) = if v.bytes {
            ("job.of.mb", d >> 20, t >> 20)
        } else {
            ("job.of", d, t)
        };
        parts.push(
            c.t(key)
                .replace("{d}", &d.to_string())
                .replace("{t}", &t.to_string()),
        );
    }
    if let Some(left) = v.eta {
        parts.push(time_left(c, left));
    }
    parts.join(" · ")
}

/// Time left rounded the way people say it: under a minute, or whole minutes up.
fn time_left(c: &Catalog, secs: f64) -> String {
    if secs < 60.0 {
        return c.t("job.left.under_minute");
    }
    let min = (secs / 60.0).ceil() as u64;
    c.t("job.left.minutes").replace("{n}", &min.to_string())
}

/// While the task card is open the status bar keeps only a small bar; a click opens the card,
/// the hover names the task.
pub fn mini(ui: &mut egui::Ui, th: &Theme, c: &Catalog, v: &JobView) -> Option<PillAct> {
    let (track, resp) = ui.allocate_exact_size(Vec2::new(64.0, 18.0), Sense::click());
    let bar = egui::Rect::from_center_size(track.center(), Vec2::new(64.0, 4.0));
    shell_kit::paint_bar(ui, th, bar, (!v.indeterminate).then_some(v.fraction));
    let name = c.t(v.title);
    let kind = egui::WidgetType::ProgressIndicator;
    resp.widget_info(|| egui::WidgetInfo::labeled(kind, true, &name));
    let tip = format!("{name}\n{}", run_info(c, v));
    resp.on_hover_text(tip).clicked().then_some(PillAct::Open)
}

/// Where the task card goes: inside the canvas at its bottom right, above the canvas' own
/// bottom row (scale bar, view chips). Offset from the screen's bottom-right corner.
pub fn card_offset(screen: egui::Rect, canvas: egui::Rect) -> Vec2 {
    let edge = ddc_ui::theme::space::MD;
    Vec2::new(
        canvas.right() - screen.right() - edge,
        canvas.bottom() - screen.bottom() - edge - CANVAS_FOOT,
    )
}

/// Height of the canvas' own bottom row the card keeps clear of.
pub const CANVAS_FOOT: f32 = 40.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn job(elapsed: f64, cancelling: bool) -> JobView {
        JobView {
            title: "t",
            stage: "a",
            stages: &["a", "b"],
            fraction: 0.4,
            indeterminate: false,
            elapsed,
            eta: None,
            counts: (0, 0),
            cancelling,
            cancellable: true,
            file: None,
            end: None,
            ..JobView::default()
        }
    }

    #[test]
    fn calm_hides_job_under_300ms() {
        let mut c = Calm::default();
        assert!(c.step(0.0, Some(job(0.0, false)), None, false).is_none());
        assert!(c.step(0.2, Some(job(0.2, false)), None, false).is_none());
        // it ends before the delay: nothing was shown, nothing appears
        assert!(c.step(0.25, None, None, false).is_none());
        assert!(c.step(0.5, None, None, false).is_none());
        assert!(c.step(0.3, Some(job(0.3, false)), None, false).is_some());
    }

    #[test]
    fn calm_keeps_shown_min_time() {
        let mut c = Calm::default();
        assert!(c.step(1.0, Some(job(0.3, false)), None, false).is_some());
        // ends 0.1 s after the first shown frame: the done state stays to 0.6 s
        let d = c.step(1.1, None, None, false).expect("held state");
        assert_eq!(d.fraction, 0.4, "no false 100 %: the outcome is unknown here");
        assert!(!d.cancellable);
        assert_eq!(d.stage, "a");
        assert!(c.step(1.5, None, None, false).is_some());
        assert!(c.step(1.65, None, None, false).is_none());
        assert!(c.step(1.7, None, None, false).is_none());
    }

    #[test]
    fn calm_shows_cancel_and_error_at_once() {
        let mut c = Calm::default();
        assert!(c.step(0.0, Some(job(0.01, true)), None, false).is_some());
        assert!(calm_visible(CalmIn {
            elapsed: 0.0,
            shown_for: None,
            urgent: false,
            ended: Some((Ended::Failed, 0.0)),
        }));
        assert!(!calm_visible(CalmIn {
            elapsed: 0.0,
            shown_for: None,
            urgent: false,
            ended: Some((Ended::Done, 0.0)),
        }));
    }

    #[test]
    fn calm_two_jobs_back_to_back_no_flicker() {
        let mut c = Calm::default();
        assert!(c.step(0.0, Some(job(0.4, false)), None, false).is_some());
        assert!(c.step(0.1, None, None, false).is_some());
        // the next job starts inside the hold and is on screen from its first frame
        assert!(c.step(0.2, Some(job(0.0, false)), None, false).is_some());
        assert!(c.step(0.3, Some(job(0.1, false)), None, false).is_some());
        assert!(c.step(0.4, None, None, false).is_some());
        assert!(c.step(1.5, None, None, false).is_none());
    }

    fn fail(why: &str, ended: Ended) -> JobEnd {
        JobEnd {
            ended,
            reason: Some(why.into()),
            retry: Some("file.save"),
        }
    }

    #[test]
    fn failed_job_ends_failed_not_done() {
        let mut c = Calm::default();
        assert!(c.step(1.0, Some(job(0.5, false)), None, false).is_some());
        let v = c
            .step(1.1, None, Some(fail("disk full", Ended::Failed)), false)
            .expect("shown");
        let e = v.end.expect("failed state");
        assert_eq!(e.ended, Ended::Failed);
        assert_eq!(e.reason.as_deref(), Some("disk full"));
        assert_eq!(e.retry, Some("file.save"));
        assert!(!v.cancellable && v.fraction < 1.0);
        // it outlasts the done hold, so Retry can be reached
        assert!(c.step(3.0, None, None, false).is_some_and(|v| v.end.is_some()));
        assert!(c.step(10.0, None, None, false).is_none());
    }

    #[test]
    fn cancelled_job_ends_cancelled() {
        let mut c = Calm::default();
        assert!(c.step(1.0, Some(job(0.5, true)), None, false).is_some());
        let v = c
            .step(1.2, None, Some(fail("x", Ended::Cancelled)), false)
            .expect("shown");
        assert_eq!(v.end.map(|e| e.ended), Some(Ended::Cancelled));
        assert!(c.step(2.5, None, None, false).is_some());
    }

    #[test]
    fn failed_unshown_job_shows_at_once() {
        let mut c = Calm::default();
        // a job under the show delay: nothing yet
        assert!(c.step(0.0, Some(job(0.05, false)), None, false).is_none());
        let v = c.step(0.1, None, Some(fail("bad", Ended::Failed)), false);
        assert!(v.is_some_and(|v| v.end.is_some()));
        // a plain finish in the same situation stays invisible
        let mut c = Calm::default();
        assert!(c.step(0.0, Some(job(0.05, false)), None, false).is_none());
        assert!(c.step(0.1, None, None, false).is_none());
    }

    #[test]
    fn done_job_keeps_hold() {
        let mut c = Calm::default();
        assert!(c.step(1.0, Some(job(0.5, false)), None, false).is_some());
        let v = c.step(1.7, None, Some(JobEnd::done()), false).expect("held");
        assert!(v.end.is_none(), "a finish carries no failure state");
        assert!(c.step(2.0, None, None, false).is_some());
        assert!(c.step(2.2, None, None, false).is_none());
    }

    #[test]
    fn failed_then_retried_shows_the_new_run() {
        let mut c = Calm::default();
        assert!(c.step(1.0, Some(job(0.5, false)), None, false).is_some());
        assert!(c
            .step(1.1, None, Some(fail("e", Ended::Failed)), false)
            .is_some_and(|v| v.end.is_some()));
        // the retry runs: a live picture with no failed state
        let v = c.step(2.0, Some(job(0.0, false)), None, false).expect("live");
        assert!(v.end.is_none() && v.cancellable);
        // and it finishes normally, with the usual short hold
        let v = c.step(2.5, None, Some(JobEnd::done()), false).expect("held");
        assert!(v.end.is_none());
        assert!(c.step(5.0, None, None, false).is_none());
    }

    #[test]
    fn surfaces_follow_the_time_table() {
        assert!(!show_pill(0.2, false));
        assert!(show_pill(0.3, false));
        assert!(show_pill(0.1, true));
        assert!(!show_window(5.0, false, false));
        assert!(show_window(10.0, false, false));
        assert!(show_window(1.0, true, false));
        assert!(!show_window(30.0, false, true));
    }

    #[test]
    fn percent_is_blank_when_amount_is_unknown() {
        let mut v = JobView {
            title: "t",
            stage: "s",
            stages: &["s"],
            fraction: 0.426,
            indeterminate: false,
            elapsed: 1.0,
            eta: None,
            counts: (0, 0),
            cancelling: false,
            cancellable: true,
            file: None,
            end: None,
            ..JobView::default()
        };
        assert_eq!(percent(&v), "43 %");
        v.indeterminate = true;
        assert_eq!(percent(&v), "");
    }

    #[test]
    fn progress_line_says_stage_counts_and_time_in_words() {
        let mut v = job(5.0, false);
        v.stage = "job.stage.plan";
        v.counts = (40, 100);
        v.eta = Some(75.0);
        let en = Catalog::new(ddc_ui::Lang::En);
        assert_eq!(run_info(&en, &v), "Reading elements · 40 of 100 · about 2 min left");
        let ru = Catalog::new(ddc_ui::Lang::Ru);
        let line = run_info(&ru, &v);
        // "40 of 100", "~2 min" in the UI language
        assert!(line.contains("40 \u{0438}\u{0437} 100 \u{00b7} "), "{line}");
        assert!(line.ends_with("~2 \u{043c}\u{0438}\u{043d}"), "{line}");
        v.eta = Some(20.0);
        assert!(run_info(&en, &v).ends_with("less than a minute left"));
        // no clock that reads 0:00, no percent beside the bar
        v.eta = None;
        v.counts = (0, 0);
        assert_eq!(run_info(&en, &v), "Reading elements");
    }

    #[test]
    fn task_card_sits_inside_the_canvas_above_its_bottom_row() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0));
        let canvas =
            egui::Rect::from_min_max(egui::pos2(236.0, 232.0), egui::pos2(1140.0, 872.0));
        let off = card_offset(screen, canvas);
        let card_br = screen.right_bottom() + off;
        assert!(canvas.contains(card_br), "{card_br:?} outside {canvas:?}");
        assert!(card_br.x <= canvas.right() - 12.0 && card_br.y <= canvas.bottom() - CANVAS_FOOT);
    }
}
