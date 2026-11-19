// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Background jobs (open, import, cache builds): the work runs on its own thread and reports
//! through a shared [`Progress`]; the UI thread polls each frame, draws a bar with the stage,
//! ETA and a cancel button, and applies the result itself (the model is only changed there).
//! A cancelled job yields [`Poll::Cancelled`] and whatever it produced is dropped.

use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use web_time::Instant;

pub use ddc_io::job::{Progress, CANCELLED};

/// State of a job seen from the UI thread.
#[derive(Debug, PartialEq)]
pub enum Poll<T> {
    Running,
    Done(T),
    Failed(String),
    Cancelled,
}

/// Remaining time from the progress rate over a sliding window (seconds).
#[derive(Clone, Debug, Default)]
pub struct Eta {
    samples: VecDeque<(f64, f32)>,
}

impl Eta {
    /// Window of the rate estimate, s.
    pub const WINDOW: f64 = 8.0;

    pub fn push(&mut self, t: f64, fraction: f32) {
        if let Some(&(lt, lf)) = self.samples.back() {
            if fraction < lf {
                // a stage restarted its counters: the old rate says nothing
                self.samples.clear();
            } else if t - lt < 0.1 {
                return;
            }
        }
        self.samples.push_back((t, fraction));
        while self.samples.len() > 2 && t - self.samples[0].0 > Self::WINDOW {
            self.samples.pop_front();
        }
    }

    /// Seconds left, once the window holds ≥ 1 s of steady progress.
    pub fn left(&self) -> Option<f64> {
        let (&(t0, f0), &(t1, f1)) = (self.samples.front()?, self.samples.back()?);
        let dt = t1 - t0;
        let df = (f1 - f0) as f64;
        if dt < 1.0 || df <= 1e-6 {
            return None;
        }
        Some(((1.0 - f1 as f64).max(0.0) * dt / df).min(360_000.0))
    }
}

/// `m:ss` or `h:mm:ss` (no words: the label comes from the catalog).
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// One frame's picture of a running job for the status bar and the stage window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobView {
    /// Size of the source file, bytes.
    pub size: Option<u64>,
    /// What the stage works on now (catalog key or the reader's own text); empty when unknown.
    pub detail: String,
    /// `counts` are bytes.
    pub bytes: bool,
    pub title: &'static str,
    pub stage: &'static str,
    /// Catalog keys of all stages in order.
    pub stages: &'static [&'static str],
    pub fraction: f32,
    /// No amount known: no percentage is shown.
    pub indeterminate: bool,
    pub elapsed: f64,
    /// Time left, shown only after the first 10 %.
    pub eta: Option<f64>,
    pub counts: (u64, u64),
    pub cancelling: bool,
    /// The work checks the cancel flag; if not, the UI says the stage can't be stopped.
    pub cancellable: bool,
    pub file: Option<String>,
    /// Set on the held frame after the job left: how it ended.
    pub end: Option<JobEnd>,
}

/// Position of `stage` in `stages`; an unknown stage counts as the first.
pub fn stage_index(stages: &[&str], stage: &str) -> usize {
    stages.iter().position(|s| *s == stage).unwrap_or(0)
}

/// A job shows only after this long, so short saves and imports don't flash, s.
pub const CALM_SHOW_AFTER: f64 = 0.3;
/// Once shown, a job stays at least this long, s.
pub const CALM_MIN_SHOWN: f64 = 0.6;
/// The done state stays this long after the job ended, s.
pub const CALM_DONE_HOLD: f64 = 0.4;

/// Failed and cancelled results stay on screen this long so Retry can be reached, s.
pub const CALM_FAIL_HOLD: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Done,
    Failed,
    Cancelled,
}

/// How a job ended, kept where the status bar reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct JobEnd {
    pub ended: Ended,
    pub reason: Option<String>,
    /// Command that starts the same work again, where one exists.
    pub retry: Option<&'static str>,
}

impl JobEnd {
    pub fn done() -> Self {
        Self {
            ended: Ended::Done,
            reason: None,
            retry: None,
        }
    }
}

thread_local! {
    // polls and the status bar both run on the UI thread
    static LAST_END: std::cell::RefCell<Option<JobEnd>> = const { std::cell::RefCell::new(None) };
}

/// Command that repeats the job with this title; not every job has one.
fn retry_cmd(title: &str) -> Option<&'static str> {
    matches!(title, "save.job.save" | "project.job.save").then_some("file.save")
}

/// Records how the job with `title` ended. `poll` does it for every job; a caller whose job
/// returned `Ok` with a failure inside records the failure over it.
pub fn record_end(title: &str, ended: Ended, reason: Option<String>) {
    let retry = (ended != Ended::Done).then(|| retry_cmd(title)).flatten();
    let e = JobEnd {
        ended,
        reason,
        retry,
    };
    LOAD_END.with(|g| *g.borrow_mut() = Some(e.clone()));
    LAST_END.with(|g| *g.borrow_mut() = Some(e));
}

thread_local! {
    // the loading window reads its own copy: the status bar takes `LAST_END`
    static LOAD_END: std::cell::RefCell<Option<JobEnd>> = const { std::cell::RefCell::new(None) };
}

/// The outcome recorded since the last call, for the loading window.
pub fn take_load_end() -> Option<JobEnd> {
    LOAD_END.with(|g| g.borrow_mut().take())
}

/// The outcome recorded since the last call.
pub fn take_end() -> Option<JobEnd> {
    LAST_END.with(|g| g.borrow_mut().take())
}

/// What the calm rule looks at in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CalmIn {
    /// Age of the running job, s.
    pub elapsed: f64,
    /// How long it has been on screen; `None` before the first shown frame.
    pub shown_for: Option<f64>,
    /// Cancelling or waiting for the user: shown at once.
    pub urgent: bool,
    /// How the job ended and how long ago, s.
    pub ended: Option<(Ended, f64)>,
}

/// Whether the job is drawn this frame.
pub fn calm_visible(i: CalmIn) -> bool {
    if i.urgent {
        return true;
    }
    match (i.shown_for, i.ended) {
        (None, None) => i.elapsed >= CALM_SHOW_AFTER,
        (None, Some((e, _))) => e != Ended::Done,
        (Some(_), None) => true,
        (Some(_), Some((e, ago))) if e != Ended::Done => ago < CALM_FAIL_HOLD,
        (Some(s), Some((_, ago))) => s < CALM_MIN_SHOWN || ago < CALM_DONE_HOLD,
    }
}

pub struct Job<T> {
    /// Catalog key of the title (`job.title.*`).
    pub title: &'static str,
    pub ctl: Arc<Progress>,
    rx: Receiver<Result<T, String>>,
    started: Instant,
    eta: Eta,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `work` on a new thread. `work` checks `ctl.is_cancelled()` at its own pace.
    pub fn spawn<F>(title: &'static str, work: F) -> Self
    where
        F: FnOnce(&Arc<Progress>) -> Result<T, String> + Send + 'static,
    {
        let ctl = Progress::new();
        let (tx, rx) = channel();
        let c = ctl.clone();
        let tx_panic = tx.clone();
        let spawned = spawn_guarded(
            &format!("job-{title}"),
            move || {
                let _ = tx.send(work(&c));
            },
            move |msg| {
                let _ = tx_panic.send(Err(msg));
            },
        );
        if let Err(e) = spawned {
            // the receiver sees a closed channel and reports the failure
            log_spawn(&e);
        }
        Self {
            title,
            ctl,
            rx,
            started: Instant::now(),
            eta: Eta::default(),
        }
    }
}

fn log_spawn(e: &std::io::Error) {
    eprintln!("job thread: {e}");
}

/// The text a user sees when a background job crashed (UI language).
pub fn internal_error() -> String {
    crate::tr("job.internal_error", &[])
}

/// Whether `text` is the crash message in either UI language (tests do not flip the language).
pub fn is_internal_error(text: &str) -> bool {
    [false, true]
        .into_iter()
        .any(|ru| text == crate::tr_lang(ru, "job.internal_error", &[]))
}

thread_local! {
    /// Message and place of the last panic on this thread, set by the hook below.
    static LAST_PANIC: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// Test-only: the next guarded spawn from this thread with this name panics at once.
    #[cfg(test)]
    static INJECT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn payload_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-text panic".to_string()
    }
}

/// Chains a hook that remembers the panic location for [`spawn_guarded`]; the previous hook
/// (crash note, default message) still runs.
fn install_panic_note() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let at = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let text = payload_text(info.payload());
            LAST_PANIC.with(|c| *c.borrow_mut() = Some(format!("{text} at {at}")));
            prev(info);
        }));
    });
}

/// Runs `work` on a named thread. A panic is caught: its text and location go to the log,
/// then `on_panic` gets the localized "internal error, model unchanged" so the owner of the
/// channel can report it instead of leaving the receiver to see a dead sender.
/// The thread must not write the model: results are applied on the main thread.
/// `Ok(None)` from the handle means the work panicked.
pub fn spawn_guarded<R, F, P>(
    name: &str,
    work: F,
    on_panic: P,
) -> std::io::Result<std::thread::JoinHandle<Option<R>>>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
    P: FnOnce(String) + Send + 'static,
{
    spawn_guarded_sized(name, None, work, on_panic)
}

/// [`spawn_guarded`] with an explicit stack size, for deep recursion.
pub fn spawn_guarded_sized<R, F, P>(
    name: &str,
    stack: Option<usize>,
    work: F,
    on_panic: P,
) -> std::io::Result<std::thread::JoinHandle<Option<R>>>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
    P: FnOnce(String) + Send + 'static,
{
    install_panic_note();
    let name = name.to_string();
    #[cfg(test)]
    let inject = INJECT.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_deref() == Some(name.as_str()) {
            c.take()
        } else {
            None
        }
    });
    let label = name.clone();
    let mut b = std::thread::Builder::new().name(name);
    if let Some(n) = stack {
        b = b.stack_size(n);
    }
    b.spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            if inject.is_some() {
                panic!("injected panic");
            }
            work()
        }));
        match run {
            Ok(r) => Some(r),
            Err(p) => {
                let note = LAST_PANIC
                    .with(|c| c.borrow_mut().take())
                    .unwrap_or_else(|| payload_text(&*p));
                eprintln!("background job `{label}` panicked: {note}");
                on_panic(internal_error());
                None
            }
        }
    })
}

/// A named worker with a big stack inside a `thread::scope`; the scope joins it.
pub fn spawn_scoped_sized<'scope, 'env, T, F>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    name: &str,
    stack: usize,
    work: F,
) -> std::io::Result<std::thread::ScopedJoinHandle<'scope, T>>
where
    T: Send + 'scope,
    F: FnOnce() -> T + Send + 'scope,
{
    std::thread::Builder::new().name(name.to_string()).stack_size(stack).spawn_scoped(scope, work)
}

/// Test-only: makes the next [`spawn_guarded`] named `name` from this thread panic.
#[cfg(test)]
pub fn inject_panic(name: &str) {
    INJECT.with(|c| *c.borrow_mut() = Some(name.to_string()));
}

impl<T> Job<T> {
    pub fn cancel(&self) {
        self.ctl.cancel();
    }

    pub fn fraction(&self) -> f32 {
        self.ctl.fraction()
    }

    /// Catalog key of the current stage.
    pub fn stage(&self) -> &'static str {
        self.ctl.stage()
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn eta(&self) -> Option<f64> {
        self.eta.left()
    }

    pub fn poll(&mut self) -> Poll<T> {
        self.poll_with(true)
    }

    /// For a job whose result cannot be taken back (a file already replaced): a result that is
    /// ready counts even when Cancel came too late to stop it.
    pub fn poll_finished(&mut self) -> Poll<T> {
        self.poll_with(false)
    }

    fn poll_with(&mut self, cancel_wins: bool) -> Poll<T> {
        let t = self.elapsed();
        let f = self.fraction();
        self.eta.push(t, f);
        let p = match self.rx.try_recv() {
            Err(TryRecvError::Empty) => Poll::Running,
            Err(TryRecvError::Disconnected) => Poll::Failed(internal_error()),
            Ok(_) if cancel_wins && self.ctl.is_cancelled() => Poll::Cancelled,
            Ok(Ok(v)) => Poll::Done(v),
            // only the bare marker: an error text that merely mentions it is a failure
            Ok(Err(e)) if e == CANCELLED => Poll::Cancelled,
            Ok(Err(e)) => Poll::Failed(e),
        };
        match &p {
            Poll::Running => {}
            Poll::Done(_) => record_end(self.title, Ended::Done, None),
            Poll::Cancelled => record_end(self.title, Ended::Cancelled, None),
            Poll::Failed(e) => record_end(self.title, Ended::Failed, Some(e.clone())),
        }
        p
    }

    /// This frame's picture of the job.
    pub fn view(&self, stages: &'static [&'static str], file: Option<String>) -> JobView {
        let counts = self.ctl.counts();
        let fraction = self.fraction();
        JobView {
            title: self.title,
            stage: self.stage(),
            stages,
            fraction,
            indeterminate: counts.1 == 0,
            elapsed: self.elapsed(),
            eta: if fraction >= 0.1 { self.eta() } else { None },
            counts,
            cancelling: self.ctl.is_cancelled(),
            cancellable: true,
            file,
            end: None,
            size: None,
            detail: self.ctl.detail(),
            bytes: self.ctl.is_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn wait<T>(j: &mut Job<T>) -> Poll<T> {
        let t = Instant::now();
        loop {
            match j.poll() {
                Poll::Running => {
                    assert!(t.elapsed() < Duration::from_secs(20), "job hung");
                    std::thread::sleep(Duration::from_millis(2));
                }
                p => return p,
            }
        }
    }

    #[test]
    fn eta_follows_a_steady_rate_and_resets_on_a_new_stage() {
        let mut e = Eta::default();
        assert_eq!(e.left(), None);
        for k in 0..=20 {
            e.push(k as f64 * 0.5, k as f32 * 0.025);
        }
        // 0.5 done after 10 s → 10 s left
        let l = e.left().unwrap();
        assert!((l - 10.0).abs() < 0.2, "{l}");
        e.push(10.5, 0.1);
        assert_eq!(e.left(), None, "restarted stage");
        assert_eq!(clock(65.0), "1:05");
        assert_eq!(clock(3725.4), "1:02:05");
    }

    #[test]
    fn job_reports_result_failure_and_panic() {
        let mut j = Job::spawn("job.title.open", |c| {
            c.phase("job.stage.read", 0.0, 1.0);
            c.set_total(4);
            c.set(4);
            Ok(42)
        });
        assert_eq!(wait(&mut j), Poll::Done(42));
        assert!((j.fraction() - 1.0).abs() < 1e-6);
        let mut j: Job<()> = Job::spawn("job.title.open", |_| Err("bad file".into()));
        assert_eq!(wait(&mut j), Poll::Failed("bad file".into()));
        let mut j: Job<()> = Job::spawn("job.title.open", |_| panic!("boom"));
        let Poll::Failed(e) = wait(&mut j) else {
            panic!("a panic must be a failure");
        };
        assert!(is_internal_error(&e), "{e}");
    }

    #[test]
    fn guarded_spawn_reports_a_panic_once_and_keeps_a_result() {
        let (tx, rx) = channel::<String>();
        let tx2 = tx.clone();
        let h = spawn_guarded("t-panic", || -> u8 { panic!("boom") }, move |m| {
            let _ = tx2.send(m);
        })
        .unwrap();
        assert_eq!(h.join().unwrap(), None);
        assert!(is_internal_error(&rx.recv().unwrap()));
        let h = spawn_guarded("t-ok", || 7u8, move |m| drop(tx.send(m))).unwrap();
        assert_eq!(h.join().unwrap(), Some(7));
        assert!(rx.try_recv().is_err(), "no report without a panic");
    }

    #[test]
    fn injected_panic_hits_only_the_named_spawn_of_this_thread() {
        inject_panic("t-named");
        let h = spawn_guarded("t-other", || 1u8, |_| {}).unwrap();
        assert_eq!(h.join().unwrap(), Some(1));
        let h = spawn_guarded("t-named", || 2u8, |_| {}).unwrap();
        assert_eq!(h.join().unwrap(), None);
        let h = spawn_guarded("t-named", || 3u8, |_| {}).unwrap();
        assert_eq!(h.join().unwrap(), Some(3), "one shot");
    }

    #[test]
    fn the_message_exists_in_both_locales() {
        let (en, ru) = (
            crate::tr_lang(false, "job.internal_error", &[]),
            crate::tr_lang(true, "job.internal_error", &[]),
        );
        assert!(en.starts_with("Internal error") && en != ru);
        assert_ne!(ru, "job.internal_error");
    }

    /// No raw thread start outside this module: a crash there would hang or lie.
    #[test]
    fn no_raw_thread_spawn_outside_jobs() {
        fn walk(dir: &std::path::Path, bad: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, bad);
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".rs") || name == "jobs.rs" || name.ends_with("_tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let mut lines = text.lines().peekable();
                while let Some(l) = lines.next() {
                    // everything from `#[cfg(test)] mod ...` on is test code
                    if l.trim() == "#[cfg(test)]"
                        && lines.peek().is_some_and(|n| n.trim_start().starts_with("mod "))
                    {
                        break;
                    }
                    let t = l.trim_start();
                    if !t.starts_with("//") && (l.contains("thread::spawn") || l.contains("thread::Builder")) {
                        bad.push(format!("{}: {t}", p.display()));
                    }
                }
            }
        }
        let mut bad = vec![];
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut bad);
        assert!(bad.is_empty(), "use jobs::spawn_guarded:\n{}", bad.join("\n"));
    }

    #[test]
    fn poll_records_how_the_job_ended() {
                let mut j: Job<()> = Job::spawn("project.job.save", |_| Err("disk full".into()));
        assert!(matches!(wait(&mut j), Poll::Failed(_)));
        let e = take_end().expect("recorded");
        assert_eq!(e.ended, Ended::Failed);
        assert_eq!(e.reason.as_deref(), Some("disk full"));
        assert_eq!(e.retry, Some("file.save"));
        assert!(take_end().is_none());
        let mut j: Job<()> = Job::spawn("job.title.import", |_| Err(CANCELLED.into()));
        assert_eq!(wait(&mut j), Poll::Cancelled);
        let e = take_end().expect("recorded");
        assert_eq!((e.ended, e.retry), (Ended::Cancelled, None));
        let mut j = Job::spawn("project.job.save", |_| Ok(1));
        assert_eq!(wait(&mut j), Poll::Done(1));
        assert_eq!(take_end().map(|e| e.ended), Some(Ended::Done));
    }

    #[test]
    fn cancel_stops_the_worker_and_drops_its_result() {
        let mut j = Job::spawn("job.title.import", |c| {
            let mut n = 0u64;
            while !c.is_cancelled() {
                n += 1;
                std::thread::sleep(Duration::from_millis(1));
                if n > 20_000 {
                    return Err("not cancelled".into());
                }
            }
            Ok(n)
        });
        assert_eq!(j.poll(), Poll::Running);
        j.cancel();
        assert_eq!(wait(&mut j), Poll::Cancelled);
        let mut j: Job<()> = Job::spawn("job.title.import", |_| Err(CANCELLED.into()));
        assert_eq!(wait(&mut j), Poll::Cancelled);
    }
}
