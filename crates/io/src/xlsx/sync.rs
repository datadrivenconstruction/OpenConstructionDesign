// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Live link to a workbook: a file stamp (modification time + size) and polling no more often than `POLL`.
//! When the workbook changes, the window rebuilds the preview without the wizard.

use std::path::{Path, PathBuf};
use web_time::{Duration, Instant, SystemTime};

pub const POLL: Duration = Duration::from_secs(1);
/// Gap between the checks of a changed file, and how many equal stamps in a row mean that
/// the program saving it has finished.
pub const SETTLE_STEP: Duration = Duration::from_millis(500);
pub const SETTLE_CHECKS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
}

impl Stamp {
    pub fn of(path: &Path) -> Option<Stamp> {
        let m = std::fs::metadata(path).ok()?;
        Some(Stamp {
            mtime: ddc_model::file_time(&m),
            len: m.len(),
        })
    }
}

/// The workbook changed after `seen`. No file (a spreadsheet editor saves via a temporary
/// file and rename) — not changed yet: wait until the file appears.
pub fn changed(seen: Option<Stamp>, now: Option<Stamp>) -> bool {
    match (seen, now) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(a), Some(b)) => a != b,
    }
}

/// The last exported or read workbook.
#[derive(Clone, Debug)]
pub struct Watch {
    pub path: PathBuf,
    pub seen: Option<Stamp>,
    next: Option<Instant>,
    /// Changed stamp seen and how many checks in a row saw it unchanged.
    pend: Option<(Stamp, u32)>,
    step: Option<Instant>,
}

impl Watch {
    pub fn new(path: &Path) -> Self {
        Watch {
            path: path.to_path_buf(),
            seen: Stamp::of(path),
            next: None,
            pend: None,
            step: None,
        }
    }

    /// Whether it is time to look at the file (no more often than `POLL`).
    pub fn due(&mut self, now: Instant) -> bool {
        match self.next {
            Some(t) if now < t => false,
            _ => {
                self.next = Some(now + POLL);
                true
            }
        }
    }

    /// A new stamp if it is time to check and the workbook changed; `stamp` — reads the file stamp.
    pub fn check(
        &mut self,
        now: Instant,
        stamp: impl FnOnce(&Path) -> Option<Stamp>,
    ) -> Option<Stamp> {
        if !self.due(now) {
            return None;
        }
        let s = stamp(&self.path);
        if changed(self.seen, s) {
            s
        } else {
            None
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<Stamp> {
        self.check(now, Stamp::of)
    }

    /// The stamp of a changed book once it stopped changing: a file still being saved (size or
    /// time moving) is waited for, so the book is read once. Unchanged file: nothing.
    pub fn settled(&mut self, now: Instant, stamp: impl FnOnce(&Path) -> Option<Stamp>) -> Option<Stamp> {
        if self.step.is_some_and(|t| now < t) {
            return None;
        }
        let s = stamp(&self.path);
        if !changed(self.seen, s) {
            self.pend = None;
            self.step = Some(now + POLL);
            return None;
        }
        let s = s?;
        self.step = Some(now + SETTLE_STEP);
        let n = match self.pend {
            Some((p, n)) if p == s => n + 1,
            _ => 1,
        };
        if n >= SETTLE_CHECKS {
            self.pend = None;
            self.step = Some(now + POLL);
            Some(s)
        } else {
            self.pend = Some((s, n));
            None
        }
    }

    /// The workbook is accounted for (read or exported).
    pub fn mark(&mut self, s: Option<Stamp>) {
        self.seen = s;
    }
}
