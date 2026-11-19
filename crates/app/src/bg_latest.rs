// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Latest-wins background builds (plans, sections, sheets): a new model revision cancels the
//! stale job, the UI keeps showing the last finished result and swaps to the new one in a single
//! assignment when it lands. Results of a superseded revision are dropped, never shown.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// Stop flag handed to the build; a cancelled build should return soon with any value.
#[derive(Clone, Debug, Default)]
pub struct Stop(Arc<AtomicBool>);

impl Stop {
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn set(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Flight<K, T> {
    key: K,
    stop: Stop,
    rx: Receiver<Result<T, String>>,
}

/// What the view should draw now.
#[derive(Debug, PartialEq)]
pub struct Shown<'a, K, T> {
    pub key: &'a K,
    pub value: &'a Arc<T>,
    /// False while a newer revision is still being built.
    pub fresh: bool,
}

pub struct Latest<K, T> {
    done: Option<(K, Arc<T>)>,
    flight: Option<Flight<K, T>>,
    /// Last build error of the wanted key (the previous result stays on screen).
    pub error: Option<String>,
    wanted: Option<K>,
    /// Builds started and stale ones cancelled (for tests and the perf panel).
    pub started: usize,
    pub cancelled: usize,
}

impl<K, T> Default for Latest<K, T> {
    fn default() -> Self {
        Self { done: None, flight: None, error: None, wanted: None, started: 0, cancelled: 0 }
    }
}

impl<K: Clone + PartialEq + Send + 'static, T: Send + Sync + 'static> Latest<K, T> {
    /// Asks for the result of `key`. Nothing starts when it is shown or building already;
    /// otherwise the stale build is cancelled and `build` runs on its own thread.
    pub fn request<F>(&mut self, key: K, build: F)
    where
        F: FnOnce(&Stop) -> Result<T, String> + Send + 'static,
    {
        self.wanted = Some(key.clone());
        if self.done.as_ref().is_some_and(|d| d.0 == key) {
            self.cancel_flight();
            return;
        }
        if self.flight.as_ref().is_some_and(|f| f.key == key) {
            return;
        }
        self.cancel_flight();
        self.error = None;
        let (tx, rx) = channel();
        let stop = Stop::default();
        let s2 = stop.clone();
        let tx2 = tx.clone();
        let spawned = crate::jobs::spawn_guarded(
            "ddc-bg-latest",
            move || {
                let _ = tx.send(build(&s2));
            },
            move |msg| {
                let _ = tx2.send(Err(msg));
            },
        );
        match spawned {
            Ok(_) => {
                self.started += 1;
                self.flight = Some(Flight { key, stop, rx });
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn cancel_flight(&mut self) {
        if let Some(f) = self.flight.take() {
            f.stop.set();
            self.cancelled += 1;
        }
    }

    /// Shows a result built in place (first paint of a new subject): no stale picture of
    /// something else is ever on screen. A running build of another key is cancelled.
    pub fn put(&mut self, key: K, value: T) -> Arc<T> {
        if self.flight.as_ref().is_some_and(|f| f.key != key) {
            self.cancel_flight();
        }
        let v = Arc::new(value);
        self.wanted = Some(key.clone());
        self.done = Some((key, v.clone()));
        self.error = None;
        v
    }

    /// Takes a finished build; true when the shown result changed. Call once per frame.
    pub fn poll(&mut self) -> bool {
        let Some(f) = &self.flight else {
            return false;
        };
        let r = match f.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(crate::jobs::internal_error()),
        };
        let f = self.flight.take().expect("flight");
        if f.stop.is_set() || self.wanted.as_ref() != Some(&f.key) {
            return false;
        }
        match r {
            Ok(v) => {
                self.done = Some((f.key, Arc::new(v)));
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    /// Blocks until the running build ends (tests, export and print paths).
    pub fn wait(&mut self) -> bool {
        self.wait_for(None)
    }

    /// Waits at most `budget` (None: until it ends): a quick build lands in the same frame.
    pub fn wait_for(&mut self, budget: Option<std::time::Duration>) -> bool {
        let Some(f) = &self.flight else {
            return false;
        };
        let r = match budget {
            None => f.rx.recv().unwrap_or_else(|_| Err(crate::jobs::internal_error())),
            Some(d) => match f.rx.recv_timeout(d) {
                Ok(r) => r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return false,
                Err(_) => Err(crate::jobs::internal_error()),
            },
        };
        let f = self.flight.take().expect("flight");
        if f.stop.is_set() || self.wanted.as_ref() != Some(&f.key) {
            return false;
        }
        match r {
            Ok(v) => {
                self.done = Some((f.key, Arc::new(v)));
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    /// The last finished result, stale or not.
    pub fn shown(&self) -> Option<Shown<'_, K, T>> {
        self.done.as_ref().map(|(k, v)| Shown {
            key: k,
            value: v,
            fresh: self.wanted.as_ref() == Some(k),
        })
    }

    /// The key last asked for (shown or building).
    pub fn wanted(&self) -> Option<&K> {
        self.wanted.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.flight.is_some()
    }
}

impl<K, T> Drop for Latest<K, T> {
    fn drop(&mut self) {
        if let Some(f) = &self.flight {
            f.stop.0.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    #[test]
    fn keeps_the_old_result_until_the_new_one_lands() {
        let mut l: Latest<u64, String> = Latest::default();
        l.request(1, |_| Ok("r1".into()));
        assert!(l.wait());
        assert_eq!(l.shown().unwrap().value.as_str(), "r1");
        let (go, gate) = sync_channel::<()>(0);
        l.request(2, move |_| {
            gate.recv().unwrap();
            Ok("r2".into())
        });
        let s = l.shown().unwrap();
        assert_eq!((s.value.as_str(), s.fresh), ("r1", false), "old picture while building");
        assert!(!l.poll());
        go.send(()).unwrap();
        assert!(l.wait());
        let s = l.shown().unwrap();
        assert_eq!((*s.key, s.value.as_str(), s.fresh), (2, "r2", true));
    }

    #[test]
    fn a_new_revision_cancels_the_stale_build_and_drops_its_result() {
        let mut l: Latest<u64, u64> = Latest::default();
        let (seen_tx, seen) = channel::<bool>();
        l.request(1, move |stop| {
            while !stop.is_set() {
                std::thread::sleep(Duration::from_millis(1));
            }
            seen_tx.send(true).unwrap();
            Ok(111)
        });
        l.request(2, |_| Ok(222));
        assert_eq!(l.cancelled, 1);
        assert!(seen.recv_timeout(Duration::from_secs(10)).unwrap(), "stale build saw stop");
        assert!(l.wait());
        assert_eq!(**l.shown().unwrap().value, 222);
        assert_eq!(l.started, 2);
    }

    #[test]
    fn same_key_is_not_rebuilt_and_errors_keep_the_last_result() {
        let mut l: Latest<u64, u64> = Latest::default();
        l.request(1, |_| Ok(1));
        l.request(1, |_| Ok(99));
        assert_eq!(l.started, 1);
        assert!(l.wait());
        l.request(1, |_| Ok(99));
        assert_eq!(l.started, 1, "shown key: nothing to do");
        l.request(2, |_| Err("bad view".into()));
        assert!(!l.wait());
        assert_eq!(l.error.as_deref(), Some("bad view"));
        let s = l.shown().unwrap();
        assert_eq!((**s.value, s.fresh), (1, false));
    }

    #[test]
    fn a_panicking_build_reports_an_internal_error() {
        let mut l: Latest<u8, u8> = Latest::default();
        l.request(1, |_| panic!("boom"));
        assert!(!l.wait());
        assert!(l.error.as_deref().is_some_and(crate::jobs::is_internal_error));
        assert!(l.shown().is_none());
    }
}
