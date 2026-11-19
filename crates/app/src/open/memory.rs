// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Memory rules of a heavy job on a 16 GB machine: one job at a time (the open flow refuses a
//! second), the bridge child under 70 % of physical memory (`RunOptions::memory_limit`), and
//! when available memory falls under [`LOW`] during a job the mesh budget halves, the LOD build
//! pauses and one notice appears.
use crate::bigfile::MESH_BUDGET;
use crate::shell::EditorApp;
use ddc_io::bridge::limit;
use ddc_ui::notify::Level;
use web_time::{Duration, Instant};

/// Available memory under which a job runs in low-memory mode.
pub const LOW: u64 = 1536 << 20;
/// Above this the mode ends (hysteresis, so it does not flap).
pub const RECOVER: u64 = 2048 << 20;
/// How often a running job asks the system.
pub const EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct State {
    pub low: bool,
    asked: Option<Instant>,
}

/// Mesh budget for the state of the machine.
pub fn mesh_budget(low: bool) -> usize {
    if low {
        MESH_BUDGET / 2
    } else {
        MESH_BUDGET
    }
}

/// The next mode given the available bytes now and the mode so far.
pub fn next_mode(low: bool, available: u64) -> bool {
    if low {
        available < RECOVER
    } else {
        available < LOW
    }
}

/// Ceiling for a bridge child, bytes; `None` where memory cannot be asked.
pub fn child_limit() -> Option<u64> {
    limit::memory().map(|m| limit::child_limit(m.total))
}

/// Per frame while a job runs (called from `open::frame`): asks at most every [`EVERY`].
pub fn tick(a: &mut EditorApp, busy: bool) {
    if !busy {
        if a.open.mem.low {
            a.open.mem.low = false;
            a.big.cache.set_budget(mesh_budget(false));
        }
        a.open.mem.asked = None;
        return;
    }
    if a.open.mem.asked.is_some_and(|t| t.elapsed() < EVERY) {
        return;
    }
    a.open.mem.asked = Some(Instant::now());
    let Some(m) = limit::memory() else {
        return;
    };
    let low = next_mode(a.open.mem.low, m.available);
    if low == a.open.mem.low {
        return;
    }
    a.open.mem.low = low;
    a.big.cache.set_budget(mesh_budget(low));
    if low {
        let msg = a.sh.cat.t("open_guard.mem_low");
        a.sh.toasts.push(Level::Warning, msg, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_mode_has_hysteresis() {
        assert!(!next_mode(false, LOW + 1));
        assert!(next_mode(false, LOW - 1));
        assert!(next_mode(true, LOW + 1), "stays low until memory recovers");
        assert!(!next_mode(true, RECOVER));
    }

    #[test]
    fn low_mode_halves_the_mesh_budget() {
        assert_eq!(mesh_budget(true) * 2, mesh_budget(false));
    }
}
