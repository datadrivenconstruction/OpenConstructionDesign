//! Progress and cancel shared by a background reader and the UI thread: lock-free counters,
//! a cancel flag (also handed to the bridge as `RunOptions::cancel`), the current stage as an
//! i18n key and the share of the whole job the stage covers.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Error text of a cancelled reader (callers check [`Progress::is_cancelled`] first).
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, Copy, PartialEq)]
struct Phase {
    key: &'static str,
    lo: f32,
    hi: f32,
}

#[derive(Debug)]
pub struct Progress {
    cancel: Arc<AtomicBool>,
    done: AtomicU64,
    total: AtomicU64,
    phase: Mutex<Phase>,
    /// Highest share shown so far (f32 bits): a stage that restarts never moves the bar back.
    high: AtomicU32,
    /// The current stage counts bytes (else elements, rows, files).
    bytes: AtomicBool,
    /// What is being worked on now: a category or level (catalog key or the reader's text).
    detail: Mutex<String>,
    /// Every stage entered, in order, without repeats in a row.
    trail: Mutex<Vec<&'static str>>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            phase: Mutex::new(Phase {
                key: "job.stage.start",
                lo: 0.0,
                hi: 1.0,
            }),
            high: AtomicU32::new(0),
            bytes: AtomicBool::new(false),
            detail: Mutex::new(String::new()),
            trail: Mutex::new(Vec::new()),
        }
    }
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The same flag for APIs that take `Arc<AtomicBool>`.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// New stage `key` covering `[lo, hi]` of the whole job; counters, unit and detail restart.
    pub fn phase(&self, key: &'static str, lo: f32, hi: f32) {
        self.note(self.fraction());
        if let Ok(mut p) = self.phase.lock() {
            *p = Phase {
                key,
                lo: lo.clamp(0.0, 1.0),
                hi: hi.clamp(lo.clamp(0.0, 1.0), 1.0),
            };
        }
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        self.bytes.store(false, Ordering::Relaxed);
        if let Ok(mut d) = self.detail.lock() {
            d.clear();
        }
        if let Ok(mut t) = self.trail.lock() {
            if t.last() != Some(&key) {
                t.push(key);
            }
        }
    }

    /// The counters of the current stage are bytes (shown as MB read of the total).
    pub fn set_bytes(&self, on: bool) {
        self.bytes.store(on, Ordering::Relaxed);
    }

    pub fn is_bytes(&self) -> bool {
        self.bytes.load(Ordering::Relaxed)
    }

    /// What is being worked on now; cheap when unchanged.
    pub fn set_detail(&self, text: &str) {
        if let Ok(mut d) = self.detail.lock() {
            if d.as_str() != text {
                d.clear();
                d.push_str(text);
            }
        }
    }

    pub fn detail(&self) -> String {
        self.detail.lock().map(|d| d.clone()).unwrap_or_default()
    }

    /// Stages entered so far, in order.
    pub fn trail(&self) -> Vec<&'static str> {
        self.trail.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// The whole job is done: the bar shows 100 %.
    pub fn finish(&self) {
        self.note(1.0);
    }

    fn note(&self, f: f32) {
        // positive f32 bits order like the numbers
        self.high.fetch_max(f.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_total(&self, t: u64) {
        self.total.store(t, Ordering::Relaxed);
    }

    pub fn set(&self, d: u64) {
        self.done.store(d, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    /// (done, total) of the current stage.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    pub fn stage(&self) -> &'static str {
        self.phase
            .lock()
            .map(|p| p.key)
            .unwrap_or("job.stage.start")
    }

    /// Share of the whole job done, 0..=1, never lower than shown before: a stage that
    /// restarts or a cache check that hands over to a full read holds the bar where it was.
    pub fn fraction(&self) -> f32 {
        let f = self.raw_fraction();
        self.note(f);
        f.max(f32::from_bits(self.high.load(Ordering::Relaxed)))
    }

    /// The share from the current stage alone; a stage without a total counts as its start.
    fn raw_fraction(&self) -> f32 {
        let p = self.phase.lock().map(|p| *p).unwrap_or(Phase {
            key: "",
            lo: 0.0,
            hi: 1.0,
        });
        let (d, t) = self.counts();
        let f = if t > 0 {
            (d.min(t) as f64 / t as f64) as f32
        } else {
            0.0
        };
        (p.lo + (p.hi - p.lo) * f).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_map_into_the_whole_job() {
        let p = Progress::new();
        p.phase("a", 0.0, 0.5);
        p.set_total(10);
        p.set(5);
        assert!((p.fraction() - 0.25).abs() < 1e-6);
        p.phase("b", 0.5, 1.0);
        assert_eq!(p.counts(), (0, 0));
        assert!((p.fraction() - 0.5).abs() < 1e-6);
        p.set_total(4);
        p.add(8);
        assert!((p.fraction() - 1.0).abs() < 1e-6, "done past total clamps");
        assert_eq!(p.trail(), vec!["a", "b"]);
        assert_eq!(p.stage(), "b");
        assert!(!p.is_cancelled());
        let f = p.cancel_flag();
        p.cancel();
        assert!(f.load(Ordering::Relaxed));
    }

    #[test]
    fn the_share_never_goes_back_and_ends_at_one() {
        let p = Progress::new();
        p.phase("job.stage.read", 0.0, 0.1);
        p.set_total(10);
        p.set(10);
        assert!((p.fraction() - 0.1).abs() < 1e-6);
        // a full read after a cache check starts again at 0 of its own range
        p.phase("job.stage.read", 0.0, 0.45);
        p.set_total(100);
        p.set(5);
        assert!((p.fraction() - 0.1).abs() < 1e-6, "held, not back to 2 %");
        p.set(100);
        assert!((p.fraction() - 0.45).abs() < 1e-6);
        let mut last = 0.0;
        for (i, key) in ["job.stage.plan", "job.stage.commit"].into_iter().enumerate() {
            let lo = 0.45 + i as f32 * 0.25;
            p.phase(key, lo, lo + 0.25);
            p.set_total(7);
            for d in 0..=7 {
                p.set(d);
                let f = p.fraction();
                assert!(f >= last, "{f} < {last}");
                last = f;
            }
        }
        p.finish();
        assert!((p.fraction() - 1.0).abs() < 1e-6);
        assert_eq!(
            p.trail(),
            vec!["job.stage.read", "job.stage.plan", "job.stage.commit"]
        );
    }

    #[test]
    fn unit_and_detail_belong_to_the_stage() {
        let p = Progress::new();
        p.phase("job.stage.read", 0.0, 0.5);
        p.set_bytes(true);
        p.set_detail("job.kind.wall");
        assert!(p.is_bytes());
        assert_eq!(p.detail(), "job.kind.wall");
        p.phase("job.stage.commit", 0.5, 1.0);
        assert!(!p.is_bytes());
        assert_eq!(p.detail(), "");
    }
}
