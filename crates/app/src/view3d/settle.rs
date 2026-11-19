// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! When the 3D view asks for a frame with no input. Refinement that the next frame continues by
//! itself (quality coming back after motion, uploads held by the frame budget, the motion
//! ladder) gets timed frames, for a bounded stretch after the last input, data change or
//! progress. Background work (chunk builds, section fills) wakes the window when it finishes,
//! so nothing polls for it. A settled view asks for nothing.

use std::time::Duration;

/// Longest stretch of timed frames with no input, no new data and no progress, seconds.
pub const MAX_IDLE_POLL_S: f64 = 3.0;
/// Frame step while quality comes back after motion.
pub const QUALITY_STEP: Duration = Duration::from_millis(60);
/// Frame step while uploads or the motion ladder continue.
pub const REFINE_STEP: Duration = Duration::from_millis(30);

/// What still changes on its own in the next frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wants {
    /// Adaptive quality below full, or motion just ended.
    pub quality: bool,
    /// Uploads held by the frame budget or the motion ladder not settled.
    pub refine: bool,
}

/// Tracks the current stretch of timed frames.
#[derive(Clone, Debug, Default)]
pub struct Settle {
    since: Option<f64>,
    data: u64,
    /// The stretch ran out with refinement still asking: timed frames stopped.
    pub gave_up: bool,
}

impl Settle {
    /// The wait before the next frame, `None` for none. `input`: any event or camera jump in
    /// this frame; `data`: the scene's build number; `progress`: this frame uploaded something.
    pub fn next(
        &mut self,
        now: f64,
        input: bool,
        data: u64,
        progress: bool,
        w: Wants,
    ) -> Option<Duration> {
        if input || progress || data != self.data {
            self.since = None;
            self.data = data;
            self.gave_up = false;
        }
        if !w.quality && !w.refine {
            self.since = None;
            return None;
        }
        let since = *self.since.get_or_insert(now);
        if now - since > MAX_IDLE_POLL_S {
            self.gave_up = true;
            return None;
        }
        Some(if w.refine { REFINE_STEP } else { QUALITY_STEP })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFINE: Wants = Wants {
        quality: false,
        refine: true,
    };

    #[test]
    fn a_settled_view_asks_for_nothing() {
        let mut s = Settle::default();
        assert_eq!(s.next(0.0, false, 1, false, Wants::default()), None);
        assert_eq!(s.next(0.0, true, 1, false, Wants::default()), None);
    }

    #[test]
    fn refinement_gets_timed_frames_then_stops_when_stuck() {
        let mut s = Settle::default();
        assert_eq!(s.next(0.0, true, 1, false, REFINE), Some(REFINE_STEP));
        let mut t = 0.0;
        let mut frames = 0;
        while s.next(t, false, 1, false, REFINE).is_some() {
            t += REFINE_STEP.as_secs_f64();
            frames += 1;
            assert!(frames < 1000, "never stops");
        }
        assert!(s.gave_up && t <= MAX_IDLE_POLL_S + 0.1, "stopped at {t}");
        // still nothing without input; an input or new data starts a new stretch
        assert_eq!(s.next(t + 10.0, false, 1, false, REFINE), None);
        assert!(s.next(t + 10.0, true, 1, false, REFINE).is_some() && !s.gave_up);
        let mut s = Settle {
            gave_up: true,
            ..Settle::default()
        };
        assert!(s.next(50.0, false, 2, false, REFINE).is_some());
    }

    #[test]
    fn progress_keeps_a_long_upload_going_and_quality_uses_the_slower_step() {
        let mut s = Settle::default();
        for i in 0..400 {
            let t = i as f64 * 0.03;
            assert_eq!(s.next(t, false, 0, true, REFINE), Some(REFINE_STEP), "at {t}");
        }
        let q = Wants {
            quality: true,
            refine: false,
        };
        assert_eq!(Settle::default().next(0.0, false, 0, false, q), Some(QUALITY_STEP));
    }
}
