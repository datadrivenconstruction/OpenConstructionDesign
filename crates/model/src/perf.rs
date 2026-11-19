// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Phase timings for benches and timing tests: off until [`enable`], then every [`span`] adds
//! its wall time and one call to its phase, process-wide; [`take`] reads and clears them.
//! Off, a span costs one atomic load.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use web_time::Instant;

static ON: AtomicBool = AtomicBool::new(false);
static ACC: Mutex<Vec<Phase>> = Mutex::new(Vec::new());

/// One phase: name, total wall time in milliseconds, number of spans.
#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub name: &'static str,
    pub ms: f64,
    pub calls: u64,
}

/// Turns the recording on or off (the totals stay until [`take`]).
pub fn enable(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// Times the enclosing scope as phase `name` when recording is on.
#[must_use = "the span measures until it is dropped"]
pub struct Span(Option<(&'static str, Instant)>);

pub fn span(name: &'static str) -> Span {
    Span(ON.load(Ordering::Relaxed).then(|| (name, Instant::now())))
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((name, t)) = self.0.take() else {
            return;
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let Ok(mut acc) = ACC.lock() else {
            return;
        };
        match acc.iter_mut().find(|p| p.name == name) {
            Some(p) => {
                p.ms += ms;
                p.calls += 1;
            }
            None => acc.push(Phase { name, ms, calls: 1 }),
        }
    }
}

/// The phases recorded since the last call, in first-seen order; clears them.
pub fn take() -> Vec<Phase> {
    ACC.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_add_up_only_while_on() {
        take();
        drop(span("perf.test.off"));
        enable(true);
        drop(span("perf.test.on"));
        drop(span("perf.test.on"));
        enable(false);
        let got = take();
        assert!(got.iter().all(|p| p.name != "perf.test.off"));
        let on = got.iter().find(|p| p.name == "perf.test.on").unwrap();
        assert_eq!(on.calls, 2);
        assert!(on.ms >= 0.0);
    }
}
