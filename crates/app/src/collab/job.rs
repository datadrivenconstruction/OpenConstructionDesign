//! Background job of a collab window: cooperative cancel, elapsed time and the working body
//! (spinner, elapsed, Cancel). A cancelled job never hands its result back, and its work
//! folder is discarded by the rules of `workdir`; the user's project is never touched here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use web_time::{Duration, Instant};

use ddc_ui::i18n::Catalog;

use super::workdir::discard;
use crate::ui_kit as kit;

/// Cancel flag the worker checks between its steps.
#[derive(Clone, Default)]
pub(crate) struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn set(&self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobState {
    Running,
    Done,
    Cancelled,
    /// The worker stopped without a result (panic).
    Lost,
}

pub(crate) enum Polled<T> {
    Pending,
    Done(T),
    Lost,
}

pub(crate) struct Job<T> {
    rx: Option<Receiver<T>>,
    cancel: Cancel,
    started: Instant,
    state: JobState,
    work: PathBuf,
    #[cfg_attr(not(test), allow(dead_code))]
    handle: Option<JoinHandle<Option<()>>>,
}

impl<T: Send + 'static> Job<T> {
    /// Runs `f` on its own thread. `f` returns None when it saw the cancel flag.
    pub fn spawn(work: PathBuf, f: impl FnOnce(&Cancel) -> Option<T> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Cancel::default();
        let flag = cancel.clone();
        let dir = work.clone();
        let crashed = work.clone();
        // works in its own folder of copies and sends a result; the open model is not touched.
        // A crash drops the sender: poll reports Lost, the folder is discarded.
        let handle = crate::jobs::spawn_guarded(
            "collab",
            move || {
                let out = f(&flag);
                match out {
                    Some(v) if !flag.is_set() => {
                        // the receiver is gone (cancel or drop raced the check): nobody reads it
                        if let Err(e) = tx.send(v) {
                            drop(e);
                            discard(&dir);
                        }
                    }
                    late => {
                        // the result may hold open copies in the folder: drop it first
                        drop(late);
                        discard(&dir);
                    }
                }
            },
            move |_| {
                discard(&crashed);
            },
        )
        .ok();
        Job {
            rx: Some(rx),
            cancel,
            started: Instant::now(),
            state: JobState::Running,
            work,
            handle,
        }
    }
}

impl<T> Job<T> {
    pub fn state(&self) -> JobState {
        self.state
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Result once the worker is done. After a cancel it stays Pending forever.
    pub fn poll(&mut self) -> Polled<T> {
        if self.state != JobState::Running {
            return Polled::Pending;
        }
        let Some(rx) = &self.rx else {
            return Polled::Pending;
        };
        match rx.try_recv() {
            Ok(v) => {
                self.state = JobState::Done;
                self.rx = None;
                Polled::Done(v)
            }
            Err(TryRecvError::Empty) => Polled::Pending,
            Err(TryRecvError::Disconnected) => {
                self.state = JobState::Lost;
                self.rx = None;
                // the worker died mid-step; whatever it left is not used by anyone
                discard(&self.work);
                Polled::Lost
            }
        }
    }

    /// Stops the worker at its next check; a result that already arrived is dropped unread.
    pub fn cancel(&mut self) {
        if self.state != JobState::Running {
            return;
        }
        self.cancel.set();
        self.state = JobState::Cancelled;
        if let Some(rx) = self.rx.take() {
            if let Ok(v) = rx.try_recv() {
                drop(v);
                discard(&self.work);
            }
        }
    }

    /// Waits for the worker thread (tests only: the UI never blocks on it).
    #[cfg(test)]
    pub fn join(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        if self.state == JobState::Running {
            self.cancel.set();
            if let Some(rx) = self.rx.take() {
                if let Ok(v) = rx.try_recv() {
                    drop(v);
                    discard(&self.work);
                }
            }
        }
    }
}

/// Elapsed time as `m:ss`, or `h:mm:ss` from one hour on.
pub(crate) fn elapsed_text(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Spinner with the job text, elapsed time and Cancel. True when Cancel was pressed.
pub(crate) fn working_body(ui: &mut egui::Ui, cat: &Catalog, text: &str, elapsed: Duration) -> bool {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.horizontal(|ui| {
        ddc_ui::widgets::surface::spinner(ui, &th);
        kit::elided(ui, text);
    });
    let t = elapsed_text(elapsed);
    let weak = kit::weak(ui);
    ui.label(egui::RichText::new(cat.tf("collab.elapsed", &[("t", &t)])).color(weak));
    kit::dialog_footer(ui, |ui| kit::cancel_button(ui, cat.t("collab.cancel")).clicked())
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_time::SystemTime;

    fn scratch(name: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let p = std::env::temp_dir().join(format!("ddc-job-test-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn wait<T>(job: &mut Job<T>) -> Polled<T> {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match job.poll() {
                Polled::Pending if Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                p => return p,
            }
        }
    }

    #[test]
    fn job_cancel_before_finish_drops_result_and_archives_copies() {
        let root = scratch("cancel");
        let work = root.join("ddc-collab-1-0-merge");
        let (go, gate) = mpsc::channel::<()>();
        let dir = work.clone();
        let mut job = Job::spawn(work.clone(), move |_c| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("their.ddc"), b"copy").unwrap();
            let _ = gate.recv();
            Some(42)
        });
        job.cancel();
        assert_eq!(job.state(), JobState::Cancelled);
        go.send(()).unwrap();
        job.join();
        assert!(matches!(job.poll(), Polled::Pending), "late result is ignored");
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(!work.exists());
        let kept = root.join("_archive").join("ddc-collab").join(work.file_name().unwrap());
        assert_eq!(std::fs::read(kept.join("their.ddc")).unwrap(), b"copy");
    }

    #[test]
    fn job_cancel_is_seen_by_worker() {
        let root = scratch("flag");
        let (go, gate) = mpsc::channel::<()>();
        let (saw_tx, saw) = mpsc::channel::<bool>();
        let mut job: Job<u8> = Job::spawn(root.join("w"), move |c| {
            let _ = gate.recv();
            let _ = saw_tx.send(c.is_set());
            if c.is_set() {
                None
            } else {
                Some(1)
            }
        });
        job.cancel();
        go.send(()).unwrap();
        job.join();
        assert!(saw.recv().unwrap());
        assert!(!root.join("w").exists(), "no folder was made, none appears");
    }

    #[test]
    fn job_finishes_normally() {
        let root = scratch("done");
        let mut job = Job::spawn(root.join("w"), |_c| Some("ok"));
        assert!(matches!(wait(&mut job), Polled::Done("ok")));
        assert_eq!(job.state(), JobState::Done);
        job.cancel();
        assert_eq!(job.state(), JobState::Done, "cancel after the result changes nothing");
    }

    #[test]
    fn job_panic_is_lost_not_pending() {
        let root = scratch("lost");
        let mut job: Job<u8> = Job::spawn(root.join("w"), |_c| panic!("worker"));
        assert!(matches!(wait(&mut job), Polled::Lost));
        assert_eq!(job.state(), JobState::Lost);
    }

    #[test]
    fn job_dropped_with_result_waiting_archives_copies() {
        let root = scratch("drop");
        let work = root.join("ddc-collab-1-2-merge");
        let dir = work.clone();
        let mut job = Job::spawn(work.clone(), move |_c| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("their.ddc"), b"copy").unwrap();
            Some(7)
        });
        job.join();
        drop(job);
        assert!(!work.exists());
        let kept = root.join("_archive").join("ddc-collab").join(work.file_name().unwrap());
        assert!(kept.join("their.ddc").is_file());
    }

    #[test]
    fn job_lost_archives_what_the_worker_left() {
        let root = scratch("lostdir");
        let work = root.join("ddc-collab-1-3-merge");
        let dir = work.clone();
        let mut job: Job<u8> = Job::spawn(work.clone(), move |_c| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("their.ddc"), b"copy").unwrap();
            panic!("worker")
        });
        assert!(matches!(wait(&mut job), Polled::Lost));
        assert!(!work.exists());
    }

    #[test]
    fn elapsed_text_formats() {
        assert_eq!(elapsed_text(Duration::ZERO), "0:00");
        assert_eq!(elapsed_text(Duration::from_millis(7_900)), "0:07");
        assert_eq!(elapsed_text(Duration::from_secs(65)), "1:05");
        assert_eq!(elapsed_text(Duration::from_secs(59 * 60 + 59)), "59:59");
        assert_eq!(elapsed_text(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(elapsed_text(Duration::from_secs(3 * 3600 + 2 * 60 + 9)), "3:02:09");
    }
}
