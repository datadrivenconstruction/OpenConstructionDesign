// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Print preview logic without drawing: the page is composed on a worker thread and applied only
//! while its request is current; view math (fit, zoom about the cursor, pan limits); page
//! stepping; mapping between dialog controls and `Print` options.

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use ddc_model::PAPERS;
use ddc_ui::widgets::Tone;
use egui::{Pos2, Rect, Vec2};

use crate::jobs::{Poll, Progress, CANCELLED};

/// Zoom is relative to the fitted page: 1.0 shows the whole page.
pub(super) const ZOOM_MIN: f32 = 0.5;
pub(super) const ZOOM_MAX: f32 = 16.0;
/// Empty border around the fitted page, px.
const FIT_PAD: f32 = 16.0;
/// A zoomed page may be dragged until this much of it is still inside the area, px.
const KEEP: f32 = 48.0;

/// One background computation; dropping it abandons the result.
pub(super) struct Worker<T> {
    ctl: Arc<Progress>,
    rx: Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Worker<T> {
    pub fn spawn<F>(work: F) -> Self
    where
        F: FnOnce(&Arc<Progress>) -> Result<T, String> + Send + 'static,
    {
        let ctl = Progress::new();
        let (tx, rx) = channel();
        let c = ctl.clone();
        let on_panic = tx.clone();
        // a failed spawn drops the senders, which `poll` reports as a failure
        let _ = crate::jobs::spawn_guarded(
            "print-preview",
            move || {
                let _ = tx.send(work(&c));
            },
            move |note| {
                let _ = on_panic.send(Err(note));
            },
        );
        Self { ctl, rx }
    }
}

impl<T> Worker<T> {
    pub fn cancel(&self) {
        self.ctl.cancel();
    }

    pub fn poll(&mut self) -> Poll<T> {
        match self.rx.try_recv() {
            Err(TryRecvError::Empty) => Poll::Running,
            Err(TryRecvError::Disconnected) => Poll::Failed("worker ended".into()),
            Ok(_) if self.ctl.is_cancelled() => Poll::Cancelled,
            Ok(Ok(v)) => Poll::Done(v),
            Ok(Err(e)) if e == CANCELLED => Poll::Cancelled,
            Ok(Err(e)) => Poll::Failed(e),
        }
    }
}

/// What the preview area shows for the current request.
pub(super) enum Shown<'a, T> {
    Ready(&'a T),
    Failed(&'a str),
    Building,
    Stopped,
}

/// The finished result and at most one running build, both tied to the request key they were
/// made for: a result for any other key is dropped.
pub(super) struct Slot<K, T> {
    shown: Option<(K, Result<T, String>)>,
    running: Option<(K, Worker<T>)>,
    stopped: Option<K>,
}

impl<K, T> Default for Slot<K, T> {
    fn default() -> Self {
        Self {
            shown: None,
            running: None,
            stopped: None,
        }
    }
}

impl<K: Clone + PartialEq, T: Send + 'static> Slot<K, T> {
    /// Call every frame with the current request: collects a finished build, cancels one made for
    /// an older request, and starts a build when nothing current exists.
    pub fn drive(&mut self, want: &K, start: impl FnOnce(&K) -> Worker<T>) {
        if let Some((k, mut w)) = self.running.take() {
            match w.poll() {
                Poll::Running if &k == want => self.running = Some((k, w)),
                Poll::Running => w.cancel(),
                Poll::Done(v) if &k == want => self.shown = Some((k, Ok(v))),
                Poll::Failed(e) if &k == want => self.shown = Some((k, Err(e))),
                Poll::Cancelled if &k == want => self.stopped = Some(k),
                _ => {}
            }
        }
        let have = self.shown.as_ref().is_some_and(|s| &s.0 == want);
        let held = self.stopped.as_ref() == Some(want);
        if !have && !held && self.running.is_none() {
            self.running = Some((want.clone(), start(want)));
        }
    }

    pub fn view(&self, want: &K) -> Shown<'_, T> {
        match &self.shown {
            Some((k, Ok(v))) if k == want => Shown::Ready(v),
            Some((k, Err(e))) if k == want => Shown::Failed(e),
            _ if self.stopped.as_ref() == Some(want) => Shown::Stopped,
            _ => Shown::Building,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Cancel from the user: the build is abandoned and not restarted for this request.
    pub fn stop(&mut self) {
        if let Some((k, w)) = self.running.take() {
            w.cancel();
            self.stopped = Some(k);
        }
    }

    /// Build again: forget a stopped request or a failure.
    pub fn retry(&mut self) {
        self.stopped = None;
        self.shown = None;
    }

    /// Window closed: no build may outlive it.
    pub fn abandon(&mut self) {
        if let Some((_, w)) = self.running.take() {
            w.cancel();
        }
    }
}

/// Page after stepping by `delta`, kept inside `0..pages`.
pub(super) fn step_page(page: usize, pages: usize, delta: i32) -> usize {
    let last = pages.saturating_sub(1) as i64;
    (page as i64 + delta as i64).clamp(0, last) as usize
}

/// Per-axis pan limit: a page smaller than the area stays inside it, a larger one may be dragged
/// until `KEEP` px of it remain visible.
fn pan_limit(size: f32, area: f32) -> f32 {
    if size >= area {
        (size - area) / 2.0 + KEEP.min(area / 2.0)
    } else {
        (area - size) / 2.0
    }
}

/// Page rectangle in area coordinates (origin at the area's top-left corner). `zoom` 1.0 fits the
/// page into the area with a margin; `pan` moves the page centre off the area centre and is
/// clamped.
pub(super) fn preview_fit(page: Vec2, area: Vec2, zoom: f32, pan: Vec2) -> Rect {
    let page = page.max(Vec2::splat(1e-3));
    let avail = (area - Vec2::splat(FIT_PAD * 2.0)).max(Vec2::splat(1.0));
    let fit = (avail.x / page.x).min(avail.y / page.y).max(0.0);
    let size = page * fit * zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    let pan = Vec2::new(
        pan.x.clamp(-pan_limit(size.x, area.x), pan_limit(size.x, area.x)),
        pan.y.clamp(-pan_limit(size.y, area.y), pan_limit(size.y, area.y)),
    );
    Rect::from_center_size(Pos2::ZERO + area / 2.0 + pan, size)
}

/// New zoom and pan after scaling by `factor` with `cursor` (relative to the area centre) as the
/// fixed point: the page point under the cursor stays under it.
pub(super) fn zoom_about(zoom: f32, pan: Vec2, cursor: Vec2, factor: f32) -> (f32, Vec2) {
    let z = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    let next = (z * factor).clamp(ZOOM_MIN, ZOOM_MAX);
    (next, cursor - (cursor - pan) * (next / z))
}

/// Dialog notice under the preview: a build failure or content that did not fit the sheet.
pub(super) struct Notice {
    pub tone: Tone,
    pub title: String,
    pub lines: Vec<String>,
}

pub(super) fn notice(ru: bool, failed: Option<&str>, warnings: &[String]) -> Option<Notice> {
    if let Some(e) = failed {
        return Some(Notice {
            tone: Tone::Danger,
            title: super::msg(ru, "sheets.print.build_failed", &[]),
            lines: vec![e.to_string()],
        });
    }
    if warnings.is_empty() {
        return None;
    }
    Some(Notice {
        tone: Tone::Warning,
        title: super::msg(
            ru,
            "sheets.print.warn_count",
            &[("n", &warnings.len().to_string())],
        ),
        lines: warnings.to_vec(),
    })
}

/// Paper dropdown: item 0 is "as on sheet", then `PAPERS` in order.
pub(super) fn paper_index(p: &Option<String>) -> usize {
    p.as_ref()
        .and_then(|s| PAPERS.iter().position(|x| x == s))
        .map_or(0, |i| i + 1)
}

pub(super) fn paper_at(i: usize) -> Option<String> {
    i.checked_sub(1)
        .and_then(|k| PAPERS.get(k))
        .map(|s| s.to_string())
}

/// Orientation pills: 0 landscape, 1 portrait.
pub(super) fn orient_index(landscape: bool) -> usize {
    usize::from(!landscape)
}

pub(super) fn orient_is_landscape(i: usize) -> bool {
    i == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use web_time::{Duration, Instant};

    const PAGE: Vec2 = Vec2::new(420.0, 297.0);
    const AREA: Vec2 = Vec2::new(800.0, 600.0);

    fn settle<T: Send + 'static>(s: &mut Slot<u32, T>, want: u32, start: &dyn Fn(u32) -> Worker<T>) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            s.drive(&want, |k| start(*k));
            if !s.is_running() || Instant::now() > end {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A worker that finishes with `value` once released.
    fn gated(value: u32) -> (Worker<u32>, Sender<()>) {
        let (tx, rx) = channel::<()>();
        let w = Worker::spawn(move |_| {
            let _ = rx.recv();
            Ok(value)
        });
        (w, tx)
    }

    #[test]
    fn page_stepping_stays_inside_the_run() {
        assert_eq!(step_page(0, 0, -1), 0);
        assert_eq!(step_page(0, 0, 1), 0);
        assert_eq!(step_page(0, 3, -1), 0);
        assert_eq!(step_page(1, 3, 1), 2);
        assert_eq!(step_page(2, 3, 1), 2);
        assert_eq!(step_page(7, 3, 0), 2);
        assert_eq!(step_page(1, 3, i32::MAX), 2);
        assert_eq!(step_page(1, 3, i32::MIN), 0);
    }

    #[test]
    fn fit_keeps_aspect_margin_and_centre() {
        let r = preview_fit(PAGE, AREA, 1.0, Vec2::ZERO);
        assert!((r.width() / r.height() - PAGE.x / PAGE.y).abs() < 1e-3);
        assert!(r.min.x >= FIT_PAD - 1e-3 && r.min.y >= FIT_PAD - 1e-3);
        assert!(r.max.x <= AREA.x - FIT_PAD + 1e-3 && r.max.y <= AREA.y - FIT_PAD + 1e-3);
        assert!((r.center().x - AREA.x / 2.0).abs() < 1e-3);
        assert!((r.center().y - AREA.y / 2.0).abs() < 1e-3);
        // a portrait page in a landscape area is limited by height
        let p = preview_fit(Vec2::new(210.0, 297.0), AREA, 1.0, Vec2::ZERO);
        assert!((p.height() - (AREA.y - FIT_PAD * 2.0)).abs() < 1e-3);
    }

    #[test]
    fn zoom_scales_and_is_limited() {
        let one = preview_fit(PAGE, AREA, 1.0, Vec2::ZERO).width();
        assert!((preview_fit(PAGE, AREA, 2.0, Vec2::ZERO).width() - 2.0 * one).abs() < 1e-2);
        assert!((preview_fit(PAGE, AREA, 100.0, Vec2::ZERO).width() - ZOOM_MAX * one).abs() < 1e-2);
        assert!((preview_fit(PAGE, AREA, 0.0, Vec2::ZERO).width() - ZOOM_MIN * one).abs() < 1e-2);
        let (z, _) = zoom_about(1.0, Vec2::ZERO, Vec2::ZERO, 1e6);
        assert_eq!(z, ZOOM_MAX);
        let (z, _) = zoom_about(1.0, Vec2::ZERO, Vec2::ZERO, 1e-6);
        assert_eq!(z, ZOOM_MIN);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let cursor = Vec2::new(90.0, -60.0);
        let before = preview_fit(PAGE, AREA, 1.0, Vec2::ZERO);
        let at = |r: Rect| {
            let p = AREA / 2.0 + cursor - r.min.to_vec2();
            Vec2::new(p.x / r.width(), p.y / r.height())
        };
        let (z, pan) = zoom_about(1.0, Vec2::ZERO, cursor, 2.0);
        let after = preview_fit(PAGE, AREA, z, pan);
        let (a, b) = (at(before), at(after));
        assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
    }

    #[test]
    fn pan_is_clamped_so_the_page_stays_reachable() {
        let far = Vec2::new(1e5, -1e5);
        for zoom in [0.5, 1.0, 4.0] {
            let r = preview_fit(PAGE, AREA, zoom, far);
            let area = Rect::from_min_size(Pos2::ZERO, AREA);
            let seen = r.intersect(area);
            assert!(seen.width() > 0.0 && seen.height() > 0.0, "zoom {zoom}");
        }
        // a page smaller than the area cannot leave it
        let r = preview_fit(PAGE, AREA, 0.5, far);
        assert!(r.min.x >= -1e-3 && r.max.x <= AREA.x + 1e-3);
        assert!(r.min.y >= -1e-3 && r.max.y <= AREA.y + 1e-3);
        // degenerate sizes do not produce NaN
        let r = preview_fit(Vec2::ZERO, Vec2::ZERO, 1.0, Vec2::ZERO);
        assert!(r.min.x.is_finite() && r.width().is_finite());
    }

    #[test]
    fn a_request_change_starts_one_job() {
        let spawned = Arc::new(AtomicUsize::new(0));
        let mut s: Slot<u32, u32> = Slot::default();
        let count = spawned.clone();
        let start = move |k: u32| {
            count.fetch_add(1, Ordering::SeqCst);
            Worker::spawn(move |_| Ok(k * 10))
        };
        for _ in 0..5 {
            settle(&mut s, 1, &start);
        }
        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        assert!(matches!(s.view(&1), Shown::Ready(10)));
        for _ in 0..5 {
            settle(&mut s, 2, &start);
        }
        assert_eq!(spawned.load(Ordering::SeqCst), 2);
        assert!(matches!(s.view(&2), Shown::Ready(20)));
        // the old request shows nothing as ready
        assert!(matches!(s.view(&1), Shown::Building));
    }

    #[test]
    fn a_stale_result_is_discarded() {
        let mut s: Slot<u32, u32> = Slot::default();
        let (old, release_old) = gated(111);
        let mut old = Some(old);
        s.drive(&1, |_| old.take().unwrap());
        // the request moves on while the first build is still running
        let (new, release_new) = gated(222);
        let mut new = Some(new);
        s.drive(&2, |_| new.take().unwrap());
        let _ = release_old.send(());
        let _ = release_new.send(());
        let end = Instant::now() + Duration::from_secs(5);
        while s.is_running() && Instant::now() < end {
            s.drive(&2, |_| unreachable!("no second start"));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(s.view(&2), Shown::Ready(222)));
        assert!(matches!(s.view(&1), Shown::Building));
    }

    #[test]
    fn a_result_for_an_older_request_never_lands() {
        let mut s: Slot<u32, u32> = Slot::default();
        let (w, release) = gated(5);
        let mut w = Some(w);
        s.drive(&1, |_| w.take().unwrap());
        let _ = release.send(());
        std::thread::sleep(Duration::from_millis(50));
        // the next frame already asks for another request: the finished one is dropped
        let mut started = false;
        s.drive(&2, |_| {
            started = true;
            Worker::spawn(|_| Ok(6))
        });
        assert!(started);
        assert!(!matches!(s.view(&1), Shown::Ready(_)));
    }

    #[test]
    fn cancel_stops_the_build_and_keeps_it_stopped() {
        let spawned = Arc::new(AtomicUsize::new(0));
        let mut s: Slot<u32, u32> = Slot::default();
        let count = spawned.clone();
        let start = move |_k: u32| {
            count.fetch_add(1, Ordering::SeqCst);
            Worker::spawn(|c| {
                let end = Instant::now() + Duration::from_secs(5);
                while !c.is_cancelled() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(CANCELLED.to_string())
            })
        };
        s.drive(&1, |k| start(*k));
        assert!(s.is_running());
        s.stop();
        assert!(!s.is_running());
        assert!(matches!(s.view(&1), Shown::Stopped));
        // frames after Cancel do not start it again
        for _ in 0..3 {
            s.drive(&1, |k| start(*k));
        }
        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        s.retry();
        s.drive(&1, |k| start(*k));
        assert_eq!(spawned.load(Ordering::SeqCst), 2);
        s.abandon();
        assert!(!s.is_running());
    }

    #[test]
    fn failure_is_kept_and_becomes_a_callout() {
        let mut s: Slot<u32, u32> = Slot::default();
        let start = |_k: u32| Worker::<u32>::spawn(|_| Err("boom".to_string()));
        settle(&mut s, 1, &start);
        let Shown::Failed(e) = s.view(&1) else {
            panic!("expected a failure");
        };
        let n = notice(false, Some(e), &[]).expect("notice");
        assert_eq!(n.tone, Tone::Danger);
        assert_eq!(n.lines, vec!["boom".to_string()]);
        assert!(!n.title.is_empty() && !n.title.starts_with("sheets."));
        // a panic in the worker is a failure too
        let mut s: Slot<u32, u32> = Slot::default();
        let start = |_k: u32| Worker::<u32>::spawn(|_| panic!("worker"));
        settle(&mut s, 1, &start);
        assert!(matches!(s.view(&1), Shown::Failed(_)));
    }

    #[test]
    fn warnings_become_one_callout() {
        assert!(notice(false, None, &[]).is_none());
        let w = vec!["a".to_string(), "b".to_string()];
        let n = notice(false, None, &w).expect("notice");
        assert_eq!(n.tone, Tone::Warning);
        assert_eq!(n.lines, w);
        assert!(n.title.contains('2'));
    }

    #[test]
    fn paper_and_orientation_round_trip() {
        for i in 0..=PAPERS.len() {
            assert_eq!(paper_index(&paper_at(i)), i);
        }
        assert_eq!(paper_at(0), None);
        assert_eq!(paper_at(PAPERS.len() + 1), None);
        assert_eq!(paper_index(&Some("no such paper".into())), 0);
        for l in [true, false] {
            assert_eq!(orient_is_landscape(orient_index(l)), l);
        }
    }
}
