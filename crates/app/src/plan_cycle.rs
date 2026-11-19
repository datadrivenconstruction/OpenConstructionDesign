// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tab cycle over the plan hits under the cursor (rule SEL-4 for the plan).
//! Pure state: the view feeds it pointer positions and the ordered hit list.

use crate::editor::Sel;

/// Pointer travel, px, after which the cycle starts over.
pub const RESET_PX: f32 = 4.0;

#[derive(Clone, Debug, Default)]
pub struct PlanCycle {
    at: Option<(f32, f32)>,
    list: Vec<Sel>,
    idx: usize,
    active: bool,
}

impl PlanCycle {
    /// Call every frame with the pointer: a move over [`RESET_PX`] drops the cycle.
    pub fn moved(&mut self, x: f32, y: f32) {
        if let Some((ax, ay)) = self.at {
            if (x - ax).hypot(y - ay) > RESET_PX {
                self.reset();
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Tab (`back` = Shift+Tab). `hits` is asked only when a cycle starts; nearest first.
    /// With fewer than two hits nothing changes.
    pub fn step(&mut self, x: f32, y: f32, back: bool, hits: impl FnOnce() -> Vec<Sel>) {
        if self.at.is_none() {
            self.list = hits();
            self.at = Some((x, y));
            self.idx = 0;
        }
        let n = self.list.len();
        if n < 2 {
            return;
        }
        self.idx = if !self.active {
            // the first press moves off the click target, or wraps to the last one
            if back {
                n - 1
            } else {
                1
            }
        } else if back {
            (self.idx + n - 1) % n
        } else {
            (self.idx + 1) % n
        };
        self.active = true;
    }

    /// The candidate a click takes; `None` until Tab has been pressed over 2+ hits.
    pub fn chosen(&self) -> Option<Sel> {
        if self.active {
            self.list.get(self.idx).copied()
        } else {
            None
        }
    }

    /// (position, count), 1-based.
    pub fn label(&self) -> Option<(usize, usize)> {
        self.active.then_some((self.idx + 1, self.list.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn five() -> Vec<Sel> {
        vec![
            Sel::Wall(1),
            Sel::Opening(2),
            Sel::Room(3),
            Sel::Slab(4),
            Sel::Wall(5),
        ]
    }

    #[test]
    fn plan_tab_cycles_overlapping_hits() {
        let mut c = PlanCycle::default();
        assert_eq!(c.chosen(), None);
        let mut seen = vec![];
        for _ in 0..5 {
            c.step(10.0, 10.0, false, five);
            seen.push(c.chosen().unwrap());
        }
        assert_eq!(
            seen,
            vec![
                Sel::Opening(2),
                Sel::Room(3),
                Sel::Slab(4),
                Sel::Wall(5),
                Sel::Wall(1)
            ]
        );
        assert_eq!(c.label(), Some((1, 5)));
    }

    #[test]
    fn click_takes_cycled_candidate() {
        let mut c = PlanCycle::default();
        c.step(10.0, 10.0, false, five);
        c.step(11.0, 10.0, false, five);
        c.moved(12.0, 11.0);
        assert_eq!(c.chosen(), Some(Sel::Room(3)));
        assert_eq!(c.label(), Some((3, 5)));
    }

    #[test]
    fn move_resets_cycle() {
        let mut c = PlanCycle::default();
        c.step(10.0, 10.0, false, five);
        c.moved(13.0, 10.0);
        assert!(c.chosen().is_some());
        c.moved(15.0, 10.0);
        assert_eq!(c.chosen(), None);
        assert_eq!(c.label(), None);
    }

    #[test]
    fn tab_once_steps_one_candidate() {
        let mut c = PlanCycle::default();
        c.step(5.0, 5.0, false, five);
        assert_eq!(c.chosen(), Some(Sel::Opening(2)));
        assert_eq!(c.label(), Some((2, 5)));
    }

    #[test]
    fn ctrl_click_cycled_candidate_adds() {
        let mut c = PlanCycle::default();
        c.step(0.0, 0.0, false, five);
        let s = c.chosen().unwrap();
        let cur = [Sel::Wall(9)];
        assert_eq!(
            crate::selection::combine(&cur, &[s], true, false),
            vec![Sel::Wall(9), Sel::Opening(2)]
        );
        assert_eq!(
            crate::selection::combine(&cur, &[s], false, false),
            vec![Sel::Opening(2)]
        );
        assert_eq!(
            crate::selection::combine(&[Sel::Wall(9), s], &[s], false, true),
            vec![Sel::Wall(9)]
        );
    }

    #[test]
    fn shift_tab_steps_back() {
        let mut c = PlanCycle::default();
        c.step(0.0, 0.0, false, five);
        c.step(0.0, 0.0, false, five);
        c.step(0.0, 0.0, true, five);
        assert_eq!(c.chosen(), Some(Sel::Opening(2)));
        let mut d = PlanCycle::default();
        d.step(0.0, 0.0, true, five);
        assert_eq!(d.chosen(), Some(Sel::Wall(5)));
    }

    #[test]
    fn single_hit_tab_is_noop() {
        let mut c = PlanCycle::default();
        c.step(0.0, 0.0, false, || vec![Sel::Wall(1)]);
        assert_eq!(c.chosen(), None);
        assert_eq!(c.label(), None);
        let mut e = PlanCycle::default();
        e.step(0.0, 0.0, false, Vec::new);
        assert_eq!(e.chosen(), None);
    }
}
