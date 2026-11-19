// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tips at the right moment: one short card after the first wall, the first selection and the
//! first save. Each is offered once for good (the mark is stored when it appears), can be
//! dismissed, and all of them can be turned off. The host feeds a [`Probe`] every frame.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

/// Seconds a card stays when nobody touches it.
pub const SHOW_SECS: f64 = 14.0;

/// Tooltip timing and placement (spec 2.14): first show after `TIP_DELAY_S`, instant again
/// within `TIP_REPEAT_S` of the last one; fade `TIP_FADE_S`; `TIP_GAP` from the anchor.
pub const TIP_DELAY_S: f64 = 0.5;
pub const TIP_REPEAT_S: f64 = 1.0;
pub const TIP_FADE_S: f32 = 0.12;
pub const TIP_GAP: f32 = 8.0;

/// Seconds to wait before a tooltip shows, given when the last one was hidden.
pub fn tip_delay(last_hidden: Option<f64>, now: f64) -> f64 {
    match last_hidden {
        Some(t) if now - t <= TIP_REPEAT_S => 0.0,
        _ => TIP_DELAY_S,
    }
}

/// Where a tooltip of `size` goes: `TIP_GAP` below `anchor`, above when there is no room,
/// kept inside `screen`; never overlaps the anchor.
pub fn tip_rect(anchor: egui::Rect, size: egui::Vec2, screen: egui::Rect) -> egui::Rect {
    let below = anchor.bottom() + TIP_GAP;
    let y = if below + size.y <= screen.bottom() || anchor.top() - TIP_GAP - size.y < screen.top() {
        below
    } else {
        anchor.top() - TIP_GAP - size.y
    };
    let x = anchor.left().min(screen.right() - size.x).max(screen.left());
    egui::Rect::from_min_size(egui::pos2(x, y), size)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    FirstWall,
    FirstSelection,
    FirstSave,
}

pub const MOMENTS: &[Moment] = &[Moment::FirstWall, Moment::FirstSelection, Moment::FirstSave];

impl Moment {
    /// Text keys: `tip.ctx.<key>.title` / `tip.ctx.<key>.text`; also the stored mark.
    pub fn key(self) -> &'static str {
        match self {
            Moment::FirstWall => "first_wall",
            Moment::FirstSelection => "first_selection",
            Moment::FirstSave => "first_save",
        }
    }
}

/// Persisted part (`ux.json`) and the card on screen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CtxTips {
    /// Keys of the tips already shown, never offered again.
    #[serde(default)]
    pub seen: BTreeSet<String>,
    /// The user turned contextual tips off.
    #[serde(default)]
    pub off: bool,
    /// Card on screen and the time it was last touched.
    #[serde(skip)]
    pub live: Option<(Moment, f64)>,
}

impl CtxTips {
    /// Show `m` now. False (nothing changes) when tips are off, `m` was shown before or another
    /// card is up.
    pub fn offer(&mut self, m: Moment, now: f64) -> bool {
        if self.off || self.live.is_some() || self.seen.contains(m.key()) {
            return false;
        }
        self.seen.insert(m.key().to_string());
        self.live = Some((m, now));
        true
    }
    pub fn current(&self) -> Option<Moment> {
        self.live.map(|l| l.0)
    }
    /// The pointer is over the card: keep it.
    pub fn touch(&mut self, now: f64) {
        if let Some(l) = &mut self.live {
            l.1 = now;
        }
    }
    /// Close the card by itself after [`SHOW_SECS`].
    pub fn expire(&mut self, now: f64) {
        if matches!(self.live, Some((_, t)) if now - t > SHOW_SECS) {
            self.live = None;
        }
    }
    pub fn dismiss(&mut self) {
        self.live = None;
    }
    /// Switch every contextual tip off (or on again; shown ones stay shown).
    pub fn set_off(&mut self, off: bool) {
        self.off = off;
        if off {
            self.live = None;
        }
    }
}

/// What the editor looks like this frame, as far as the tips care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Probe {
    /// Identity of the open model; a new one starts a new baseline.
    pub project: u64,
    pub level: i64,
    pub walls: usize,
    /// Undo depth: a new wall counts only together with an edit.
    pub undo: usize,
    pub selected: usize,
    /// The project has been written to a file.
    pub saved: bool,
}

/// Turns changes between frames into moments; moments wait here until the card is free.
#[derive(Debug, Default)]
pub struct Watch {
    base: Option<Probe>,
    pub queue: VecDeque<Moment>,
}

impl Watch {
    pub fn step(&mut self, p: Probe) {
        let Some(b) = self.base.replace(p) else {
            return;
        };
        if b.project != p.project {
            self.queue.clear();
            return;
        }
        if p.level == b.level && p.walls > b.walls && p.undo > b.undo {
            self.queue.push_back(Moment::FirstWall);
        }
        if b.selected == 0 && p.selected > 0 {
            self.queue.push_back(Moment::FirstSelection);
        }
        if !b.saved && p.saved {
            self.queue.push_back(Moment::FirstSave);
        }
    }
    /// Next moment worth a card; moments shown before are dropped.
    pub fn next(&mut self, tips: &CtxTips) -> Option<Moment> {
        while let Some(m) = self.queue.pop_front() {
            if !tips.seen.contains(m.key()) {
                return Some(m);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> Probe {
        Probe {
            project: 1,
            ..Default::default()
        }
    }

    #[test]
    fn tooltip_waits_500ms_then_repeats_instantly_for_a_second() {
        assert_eq!(tip_delay(None, 10.0), 0.5);
        assert_eq!(tip_delay(Some(9.5), 10.0), 0.0);
        assert_eq!(tip_delay(Some(9.0), 10.0), 0.0);
        assert_eq!(tip_delay(Some(8.9), 10.0), 0.5);
    }

    #[test]
    fn tooltip_never_overlaps_its_anchor() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let size = egui::vec2(200.0, 40.0);
        for (x, y) in [(10.0, 10.0), (700.0, 300.0), (400.0, 580.0), (790.0, 595.0)] {
            let a = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(24.0, 24.0));
            let t = tip_rect(a, size, screen);
            assert!(!t.intersects(a), "{a:?} {t:?}");
            assert!(screen.contains_rect(t), "{t:?}");
            assert!(t.top() - a.bottom() == TIP_GAP || a.top() - t.bottom() == TIP_GAP);
        }
    }

    #[test]
    fn a_tip_is_offered_once_and_only_one_at_a_time() {
        let mut t = CtxTips::default();
        assert!(t.offer(Moment::FirstWall, 0.0));
        assert!(!t.offer(Moment::FirstSave, 1.0), "a card is up");
        t.dismiss();
        assert!(!t.offer(Moment::FirstWall, 2.0), "never repeated");
        assert!(t.offer(Moment::FirstSave, 2.0));
    }

    #[test]
    fn off_blocks_and_closes_and_the_state_round_trips() {
        let mut t = CtxTips::default();
        assert!(t.offer(Moment::FirstSelection, 0.0));
        t.set_off(true);
        assert!(t.current().is_none());
        assert!(!t.offer(Moment::FirstWall, 1.0));
        let j = serde_json::to_string(&t).unwrap();
        let back: CtxTips = serde_json::from_str(&j).unwrap();
        assert!(back.off && back.seen.contains("first_selection") && back.live.is_none());
        t.set_off(false);
        assert!(t.offer(Moment::FirstWall, 2.0));
        // an old ux.json without the field still loads
        let u = crate::uxstate::UxState::from_json("{}").unwrap();
        assert!(!u.ctx_tips.off && u.ctx_tips.seen.is_empty());
    }

    #[test]
    fn the_card_expires_unless_touched() {
        let mut t = CtxTips::default();
        t.offer(Moment::FirstWall, 0.0);
        t.touch(10.0);
        t.expire(20.0);
        assert!(t.current().is_some());
        t.expire(10.0 + SHOW_SECS + 0.1);
        assert!(t.current().is_none());
    }

    #[test]
    fn moments_come_from_changes_not_from_state() {
        let mut w = Watch::default();
        let mut p = probe();
        p.walls = 5;
        p.selected = 2;
        p.saved = true;
        w.step(p); // first look is the baseline
        w.step(p);
        assert!(w.queue.is_empty(), "an opened project is not a first wall");
        let mut q = p;
        q.walls = 6;
        q.undo = 1;
        w.step(q);
        assert_eq!(w.queue.pop_front(), Some(Moment::FirstWall));
        // a level switch shows other walls without an edit
        let mut l = q;
        l.level = 2;
        l.walls = 9;
        w.step(l);
        assert!(w.queue.is_empty());
        // selection from none to some, save from false to true
        let mut a = l;
        a.selected = 0;
        a.saved = false;
        w.step(a);
        let mut b = a;
        b.selected = 1;
        b.saved = true;
        w.step(b);
        assert_eq!(
            w.queue.iter().copied().collect::<Vec<_>>(),
            vec![Moment::FirstSelection, Moment::FirstSave]
        );
    }

    #[test]
    fn a_new_project_resets_the_baseline() {
        let mut w = Watch::default();
        let mut p = probe();
        w.step(p);
        p.project = 2;
        p.selected = 3;
        p.saved = true;
        w.step(p);
        assert!(w.queue.is_empty());
    }

    #[test]
    fn queued_moments_skip_what_was_shown() {
        let mut w = Watch::default();
        w.queue.push_back(Moment::FirstWall);
        w.queue.push_back(Moment::FirstSave);
        let mut t = CtxTips::default();
        t.offer(Moment::FirstWall, 0.0);
        t.dismiss();
        assert_eq!(w.next(&t), Some(Moment::FirstSave));
        assert_eq!(w.next(&t), None);
    }

    #[test]
    fn fr2_texts_match_in_both_languages() {
        let en: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../data/i18n/en/tip.json")).unwrap();
        let ru: std::collections::BTreeMap<String, String> =
            serde_json::from_str(include_str!("../data/i18n/ru/tip.json")).unwrap();
        assert_eq!(en.keys().collect::<Vec<_>>(), ru.keys().collect::<Vec<_>>());
        for (k, v) in &en {
            // the same placeholders on both sides
            let ph = |s: &str| -> Vec<String> {
                s.split('{').skip(1).filter_map(|x| x.split('}').next()).map(String::from).collect()
            };
            assert_eq!(ph(v), ph(&ru[k]), "{k}");
        }
    }

    #[test]
    fn every_moment_has_text_in_both_languages() {
        for lang in [crate::Lang::En, crate::Lang::Ru] {
            let c = crate::Catalog::new(lang);
            for m in MOMENTS {
                for part in ["title", "text"] {
                    let key = format!("tip.ctx.{}.{part}", m.key());
                    assert!(c.table(lang).contains_key(&key), "{lang:?} {key}");
                }
            }
        }
    }
}
