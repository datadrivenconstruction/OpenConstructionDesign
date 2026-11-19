//! Pure parts of the recovery window: relative time, row picking, danger confirmation.
//! No drawing and no global state, so every rule here is covered by plain unit tests.
use ddc_model::SnapInfo;
use ddc_ui::Catalog;
use egui::{util::IdTypeMap, Id};
use std::path::Path;

/// Age bucket of a snapshot against the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rel {
    Now,
    Minutes(u64),
    Hours(u64),
    Yesterday,
    Days(u64),
}

/// Bucket for `ms` seen at `now_ms`. A snapshot from the future (clock change) reads as now.
pub fn rel_bucket(now_ms: u128, ms: u128) -> Rel {
    let secs = (now_ms.saturating_sub(ms) / 1000) as u64;
    if secs < 60 {
        Rel::Now
    } else if secs < 3600 {
        Rel::Minutes(secs / 60)
    } else if secs < 86_400 {
        Rel::Hours(secs / 3600)
    } else if secs < 172_800 {
        Rel::Yesterday
    } else {
        Rel::Days(secs / 86_400)
    }
}

/// "5 min ago" (or its Russian counterpart) in the language of the catalog.
pub fn rel_time(c: &Catalog, now_ms: u128, ms: u128) -> String {
    match rel_bucket(now_ms, ms) {
        Rel::Now => c.t("recover.ago_now"),
        Rel::Minutes(n) => c.plural("recover.ago_min", n),
        Rel::Hours(n) => c.plural("recover.ago_hour", n),
        Rel::Yesterday => c.t("recover.ago_yesterday"),
        Rel::Days(n) => c.plural("recover.ago_day", n),
    }
}

pub fn restorable(s: &SnapInfo) -> bool {
    s.damage.is_none()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    First,
    Last,
}

/// Row the pick moves to. Damaged rows are skipped; with nothing to move to the pick stays.
pub fn next_pick(snaps: &[SnapInfo], cur: usize, nav: Nav) -> usize {
    let ok = |i: &usize| snaps.get(*i).is_some_and(restorable);
    let found = match nav {
        Nav::Down => (cur.saturating_add(1)..snaps.len()).find(ok),
        Nav::Up => (0..cur.min(snaps.len())).rev().find(ok),
        Nav::First => (0..snaps.len()).find(ok),
        Nav::Last => (0..snaps.len()).rev().find(ok),
    };
    found.unwrap_or(cur)
}

/// What the list shows: the designed empty state, an all-damaged warning, or the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListState {
    Empty,
    AllDamaged,
    Ready,
}

pub fn list_state(snaps: &[SnapInfo]) -> ListState {
    if snaps.is_empty() {
        ListState::Empty
    } else if snaps.iter().all(|s| !restorable(s)) {
        ListState::AllDamaged
    } else {
        ListState::Ready
    }
}

/// The file Restore and Compare may open: never a damaged snapshot.
pub fn restore_target(snaps: &[SnapInfo], pick: usize) -> Option<&Path> {
    snaps
        .get(pick)
        .filter(|s| restorable(s))
        .map(|s| s.path.as_path())
}

/// Enter in the list only moves focus to Restore; it never restores by itself.
pub fn enter_focuses_restore(snaps: &[SnapInfo], pick: usize) -> bool {
    restore_target(snaps, pick).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Danger {
    ArchiveOne,
    DiscardAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ev {
    Ask(Danger),
    Confirm,
    Cancel,
}

/// Two-step destructive action: returns the new pending question and the action to run now.
/// Confirm without a prior question runs nothing.
pub fn confirm_step(pending: Option<Danger>, ev: Ev) -> (Option<Danger>, Option<Danger>) {
    match ev {
        Ev::Ask(d) => (Some(d), None),
        Ev::Confirm => (None, pending),
        Ev::Cancel => (None, None),
    }
}

/// Keeps the pending question between frames. `remove_temp` needs `Default`, so plain `remove`.
pub fn store_pending(m: &mut IdTypeMap, id: Id, d: Option<Danger>) {
    match d {
        Some(d) => m.insert_temp(id, d),
        None => m.remove::<Danger>(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_ui::i18n::Lang;
    use std::path::PathBuf;

    const MIN: u128 = 60_000;
    const NOW: u128 = 10_000_000_000;

    fn snap(ms: u128, bad: bool) -> SnapInfo {
        SnapInfo {
            path: PathBuf::from(format!("s{ms}.ddc")),
            ms,
            bytes: 1024,
            damage: bad.then(|| "integrity check failed".to_string()),
        }
    }

    fn rows(bad: &[bool]) -> Vec<SnapInfo> {
        bad.iter().enumerate().map(|(i, b)| snap(i as u128, *b)).collect()
    }

    #[test]
    fn buckets() {
        assert_eq!(rel_bucket(NOW, NOW - 59_000), Rel::Now);
        assert_eq!(rel_bucket(NOW, NOW + 5 * MIN), Rel::Now);
        assert_eq!(rel_bucket(NOW, NOW - MIN), Rel::Minutes(1));
        assert_eq!(rel_bucket(NOW, NOW - 59 * MIN), Rel::Minutes(59));
        assert_eq!(rel_bucket(NOW, NOW - 60 * MIN), Rel::Hours(1));
        assert_eq!(rel_bucket(NOW, NOW - 23 * 60 * MIN), Rel::Hours(23));
        assert_eq!(rel_bucket(NOW, NOW - 24 * 60 * MIN), Rel::Yesterday);
        assert_eq!(rel_bucket(NOW, NOW - 47 * 60 * MIN), Rel::Yesterday);
        assert_eq!(rel_bucket(NOW, NOW - 48 * 60 * MIN), Rel::Days(2));
        assert_eq!(rel_bucket(NOW, NOW - 10 * 24 * 60 * MIN), Rel::Days(10));
    }

    #[test]
    fn english_text() {
        let c = Catalog::new(Lang::En);
        assert_eq!(rel_time(&c, NOW, NOW - 10_000), "just now");
        assert_eq!(rel_time(&c, NOW, NOW - MIN), "1 min ago");
        assert_eq!(rel_time(&c, NOW, NOW - 5 * MIN), "5 min ago");
        assert_eq!(rel_time(&c, NOW, NOW - 60 * MIN), "1 hour ago");
        assert_eq!(rel_time(&c, NOW, NOW - 3 * 60 * MIN), "3 hours ago");
        assert_eq!(rel_time(&c, NOW, NOW - 30 * 60 * MIN), "yesterday");
        assert_eq!(rel_time(&c, NOW, NOW - 72 * 60 * MIN), "3 days ago");
    }

    #[test]
    fn russian_plurals() {
        let c = Catalog::new(Lang::Ru);
        let cases = [
            (5 * MIN, "rel_5min"),
            (60 * MIN, "rel_1h"),
            (2 * 60 * MIN, "rel_2h"),
            (5 * 60 * MIN, "rel_5h"),
            (11 * 60 * MIN, "rel_11h"),
            (21 * 60 * MIN, "rel_21h"),
            (72 * 60 * MIN, "rel_3d"),
            (5 * 24 * 60 * MIN, "rel_5d"),
            (11 * 24 * 60 * MIN, "rel_11d"),
            (21 * 24 * 60 * MIN, "rel_21d"),
            (30 * 60 * MIN, "rel_yesterday"),
        ];
        for (ago, key) in cases {
            assert_eq!(rel_time(&c, NOW, NOW - ago), crate::app2_fx::fx(key), "{key}");
        }
    }

    #[test]
    fn newest_first_order() {
        let mut v = vec![snap(5, false), snap(9, false), snap(1, true), snap(7, false)];
        super::super::recover_win::newest_first(&mut v);
        let ms: Vec<u128> = v.iter().map(|s| s.ms).collect();
        assert_eq!(ms, [9, 7, 5, 1]);
    }

    #[test]
    fn empty_state() {
        assert_eq!(list_state(&[]), ListState::Empty);
        assert_eq!(list_state(&rows(&[true, true])), ListState::AllDamaged);
        assert_eq!(list_state(&rows(&[true, false])), ListState::Ready);
    }

    #[test]
    fn arrows_skip_damaged_rows_both_ways() {
        let v = rows(&[false, true, true, false, false]);
        assert_eq!(next_pick(&v, 0, Nav::Down), 3);
        assert_eq!(next_pick(&v, 3, Nav::Down), 4);
        assert_eq!(next_pick(&v, 4, Nav::Down), 4, "end: stays");
        assert_eq!(next_pick(&v, 4, Nav::Up), 3);
        assert_eq!(next_pick(&v, 3, Nav::Up), 0);
        assert_eq!(next_pick(&v, 0, Nav::Up), 0, "start: stays");
        assert_eq!(next_pick(&v, 3, Nav::First), 0);
        assert_eq!(next_pick(&v, 0, Nav::Last), 4);
    }

    #[test]
    fn arrows_stay_put_when_nothing_is_restorable() {
        let v = rows(&[true, true]);
        for nav in [Nav::Up, Nav::Down, Nav::First, Nav::Last] {
            assert_eq!(next_pick(&v, 0, nav), 0);
        }
        assert_eq!(next_pick(&[], 0, Nav::Down), 0);
        assert_eq!(next_pick(&v, 9, Nav::Up), 9, "stale pick does not panic");
    }

    #[test]
    fn enter_only_focuses_restore() {
        let v = rows(&[false, true]);
        assert!(enter_focuses_restore(&v, 0));
        assert!(!enter_focuses_restore(&v, 1), "damaged row: nothing to focus");
        // the dialog must not bind Enter to the restore action
        let src = include_str!("recover_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains(concat!("primary", "_action(")));
        assert!(code.contains(concat!("primary", "_button(")));
    }

    #[test]
    fn damaged_snapshot_is_not_restorable() {
        let v = rows(&[false, true]);
        assert!(restore_target(&v, 0).is_some());
        assert!(restore_target(&v, 1).is_none());
        assert!(restore_target(&v, 7).is_none());
    }

    #[test]
    fn discard_needs_a_question_and_a_confirm() {
        // confirming with nothing asked runs nothing
        assert_eq!(confirm_step(None, Ev::Confirm), (None, None));
        let (p, run) = confirm_step(None, Ev::Ask(Danger::DiscardAll));
        assert_eq!((p, run), (Some(Danger::DiscardAll), None));
        // cancel drops the question
        assert_eq!(confirm_step(p, Ev::Cancel), (None, None));
        // confirm runs exactly what was asked, once
        let (p2, run) = confirm_step(p, Ev::Confirm);
        assert_eq!((p2, run), (None, Some(Danger::DiscardAll)));
        assert_eq!(confirm_step(p2, Ev::Confirm), (None, None));
        // a second question replaces the first
        let (p3, _) = confirm_step(Some(Danger::ArchiveOne), Ev::Ask(Danger::DiscardAll));
        assert_eq!(p3, Some(Danger::DiscardAll));
    }

    #[test]
    fn pending_question_survives_frames_and_clears() {
        let mut m = IdTypeMap::default();
        let id = Id::new("t");
        store_pending(&mut m, id, Some(Danger::ArchiveOne));
        assert_eq!(m.get_temp::<Danger>(id), Some(Danger::ArchiveOne));
        store_pending(&mut m, id, Some(Danger::DiscardAll));
        assert_eq!(m.get_temp::<Danger>(id), Some(Danger::DiscardAll));
        store_pending(&mut m, id, None);
        assert_eq!(m.get_temp::<Danger>(id), None);
        store_pending(&mut m, id, None);
    }

    #[test]
    fn recovery_window_uses_the_kit_only() {
        let src = include_str!("recover_win.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for bad in [
            concat!("ui.", "button("),
            concat!("egui::", "Button"),
            concat!("ui.", "checkbox("),
        ] {
            assert!(!code.contains(bad), "raw widget {bad} in recover_win.rs");
        }
        for (n, line) in code.lines().enumerate() {
            if let Some(i) = line.find("add_space(") {
                let next = line[i + "add_space(".len()..].chars().next();
                assert!(
                    !next.is_some_and(|c| c.is_ascii_digit()),
                    "recover_win.rs:{}: numeric spacing",
                    n + 1
                );
            }
        }
    }
}
