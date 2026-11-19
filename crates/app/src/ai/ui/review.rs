// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Reviewing a draft without the mouse: which key does what on the plan card, and how a verdict
//! turns into check marks. Pure, so the key table is tested without a window. Nothing here writes
//! the model: `Apply` is only a request that goes through the one Apply path.

use super::logic;
use crate::ai::plan::{ElemDiff, PlanItem};
use ddc_model::RowKind;
use egui::{Key, Modifiers};

/// What the frame knows when a key arrives.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReviewCtx {
    /// The person is in the card (clicked a row, or walked it with Alt+arrows) and not in the viewport.
    pub card_focus: bool,
    /// A text field has keyboard focus (chat input, annotation text): letters belong to it.
    pub typing: bool,
    /// Some other widget has focus and would use Space itself.
    pub widget_focus: bool,
    pub can_apply: bool,
    pub has_items: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAct {
    Move(i32),
    Accept,
    Reject,
    Toggle,
    AcceptGroup,
    RejectGroup,
    Apply,
    /// Zoom to the cursor item.
    Zoom,
    /// Focus back to the viewport. Keeps the draft and every check mark.
    Leave,
}

/// Every chord the card knows, longest modifier set first (`Shift+A` before `A`).
pub const CHORDS: [(Key, Modifiers); 12] = [
    (Key::A, Modifiers::SHIFT),
    (Key::R, Modifiers::SHIFT),
    (Key::ArrowDown, Modifiers::ALT),
    (Key::ArrowUp, Modifiers::ALT),
    (Key::Enter, Modifiers::ALT),
    (Key::Enter, Modifiers::COMMAND),
    (Key::J, Modifiers::NONE),
    (Key::K, Modifiers::NONE),
    (Key::A, Modifiers::NONE),
    (Key::R, Modifiers::NONE),
    (Key::Space, Modifiers::NONE),
    (Key::Escape, Modifiers::NONE),
];

fn only(m: Modifiers, alt: bool, shift: bool, cmd: bool) -> bool {
    m.alt == alt && m.shift == shift && (m.command || m.ctrl) == cmd
}

/// The action for a key press, or `None` when the card must leave the key alone.
pub fn review_key(key: Key, mods: Modifiers, ctx: &ReviewCtx) -> Option<ReviewAct> {
    if !ctx.has_items {
        return None;
    }
    // Alt+arrows are no text editing, so they walk the card even from the chat input
    if only(mods, true, false, false) {
        return match key {
            Key::ArrowDown => Some(ReviewAct::Move(1)),
            Key::ArrowUp => Some(ReviewAct::Move(-1)),
            Key::Enter => Some(ReviewAct::Zoom),
            _ => None,
        };
    }
    if ctx.typing || !ctx.card_focus {
        return None;
    }
    if only(mods, false, false, true) {
        return (key == Key::Enter && ctx.can_apply).then_some(ReviewAct::Apply);
    }
    if only(mods, false, true, false) {
        return match key {
            Key::A => Some(ReviewAct::AcceptGroup),
            Key::R => Some(ReviewAct::RejectGroup),
            _ => None,
        };
    }
    if !only(mods, false, false, false) {
        return None;
    }
    match key {
        Key::J => Some(ReviewAct::Move(1)),
        Key::K => Some(ReviewAct::Move(-1)),
        Key::A => Some(ReviewAct::Accept),
        Key::R => Some(ReviewAct::Reject),
        Key::Space if !ctx.widget_focus => Some(ReviewAct::Toggle),
        Key::Escape => Some(ReviewAct::Leave),
        _ => None,
    }
}

/// What a verdict asks of the panel. `check` only ever flips check marks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub cursor: Option<u32>,
    pub check: Vec<(u32, bool)>,
    pub apply: bool,
    pub zoom: Option<u32>,
    pub leave: bool,
}

/// `N accepted of M`: the numbers the Apply button and the footer show.
pub fn accepted(items: &[PlanItem]) -> (usize, usize) {
    (items.iter().filter(|i| i.checked).count(), items.len())
}

/// The kind most of the item's elements have; a tie goes to the first of `+ ~ −`.
pub fn primary_kind(diff: &[ElemDiff], item: u32) -> Option<RowKind> {
    let c = logic::item_counts(diff, item);
    if c.total() == 0 {
        return None;
    }
    let mut best = (RowKind::Created, c.created);
    for cand in [(RowKind::Changed, c.changed), (RowKind::Deleted, c.deleted)] {
        if cand.1 > best.1 {
            best = cand;
        }
    }
    Some(best.0)
}

/// Check marks that make one item `on`, with what it pulls along; nothing if it already is.
pub fn set_item(items: &[PlanItem], id: u32, on: bool) -> Vec<(u32, bool)> {
    match items.iter().find(|i| i.id == id) {
        Some(i) if i.checked != on => logic::toggled(items, id),
        _ => vec![],
    }
}

/// The same for every item of the cursor item's kind group, dependencies included.
pub fn set_group(
    items: &[PlanItem],
    diff: &[ElemDiff],
    id: u32,
    on: bool,
) -> Vec<(u32, bool)> {
    let kind = primary_kind(diff, id);
    let group: Vec<u32> = items
        .iter()
        .filter(|i| primary_kind(diff, i.id) == kind)
        .map(|i| i.id)
        .collect();
    let mut work = items.to_vec();
    let mut out: Vec<(u32, bool)> = vec![];
    for g in group {
        for (cid, v) in set_item(&work, g, on) {
            if let Some(w) = work.iter_mut().find(|w| w.id == cid) {
                w.checked = v;
            }
            out.retain(|(x, _)| *x != cid);
            out.push((cid, v));
        }
    }
    out
}

/// Turns an action into check marks and a new cursor. `Leave` changes nothing.
pub fn outcome(act: ReviewAct, items: &[PlanItem], diff: &[ElemDiff], cursor: Option<u32>) -> Outcome {
    let mut o = Outcome {
        cursor,
        ..Outcome::default()
    };
    let ids: Vec<u32> = items.iter().map(|i| i.id).collect();
    if ids.is_empty() {
        return o;
    }
    let cur = cursor.filter(|c| ids.contains(c)).unwrap_or(ids[0]);
    match act {
        ReviewAct::Move(step) => {
            let n = ids.len() as i32;
            let at = cursor
                .and_then(|c| ids.iter().position(|x| *x == c))
                .map_or(if step > 0 { -1 } else { 0 }, |i| i as i32);
            o.cursor = Some(ids[(at + step).rem_euclid(n) as usize]);
        }
        ReviewAct::Accept => {
            o.cursor = Some(cur);
            o.check = set_item(items, cur, true);
        }
        ReviewAct::Reject => {
            o.cursor = Some(cur);
            o.check = set_item(items, cur, false);
        }
        ReviewAct::Toggle => {
            o.cursor = Some(cur);
            o.check = logic::toggled(items, cur);
        }
        ReviewAct::AcceptGroup => {
            o.cursor = Some(cur);
            o.check = set_group(items, diff, cur, true);
        }
        ReviewAct::RejectGroup => {
            o.cursor = Some(cur);
            o.check = set_group(items, diff, cur, false);
        }
        ReviewAct::Apply => o.apply = true,
        ReviewAct::Zoom => o.zoom = Some(cur),
        ReviewAct::Leave => o.leave = true,
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::{PlanOp, Targets};

    fn ctx() -> ReviewCtx {
        ReviewCtx {
            card_focus: true,
            can_apply: true,
            has_items: true,
            ..ReviewCtx::default()
        }
    }

    fn item(id: u32, on: bool, deps: &[u32]) -> PlanItem {
        PlanItem {
            id,
            title: String::new(),
            why: String::new(),
            op: PlanOp::Delete {
                target: Targets::Selection,
            },
            checked: on,
            depends_on: deps.to_vec(),
            actor: String::new(),
            conflict: None,
            sets: Default::default(),
        }
    }

    fn diff(item: u32, kind: RowKind) -> ElemDiff {
        ElemDiff {
            key: ("wall".to_string(), 1),
            kind,
            item: Some(item),
            level: None,
            label: String::new(),
            cells: vec![],
        }
    }

    #[test]
    fn keys_map_to_review_actions() {
        let c = ctx();
        let k = |key, m| review_key(key, m, &c);
        assert_eq!(k(Key::J, Modifiers::NONE), Some(ReviewAct::Move(1)));
        assert_eq!(k(Key::K, Modifiers::NONE), Some(ReviewAct::Move(-1)));
        assert_eq!(k(Key::ArrowDown, Modifiers::ALT), Some(ReviewAct::Move(1)));
        assert_eq!(k(Key::ArrowUp, Modifiers::ALT), Some(ReviewAct::Move(-1)));
        assert_eq!(k(Key::A, Modifiers::NONE), Some(ReviewAct::Accept));
        assert_eq!(k(Key::R, Modifiers::NONE), Some(ReviewAct::Reject));
        assert_eq!(k(Key::Space, Modifiers::NONE), Some(ReviewAct::Toggle));
        assert_eq!(k(Key::A, Modifiers::SHIFT), Some(ReviewAct::AcceptGroup));
        assert_eq!(k(Key::R, Modifiers::SHIFT), Some(ReviewAct::RejectGroup));
        assert_eq!(k(Key::Enter, Modifiers::COMMAND), Some(ReviewAct::Apply));
        assert_eq!(k(Key::Escape, Modifiers::NONE), Some(ReviewAct::Leave));
        // Ctrl+A is select-all and Ctrl+R belongs to the global keymap
        assert_eq!(k(Key::A, Modifiers::COMMAND), None);
        assert_eq!(k(Key::R, Modifiers::COMMAND), None);
        assert_eq!(k(Key::J, Modifiers::SHIFT), None);
    }

    #[test]
    fn typing_in_a_field_never_triggers_a_review_key() {
        let c = ReviewCtx {
            typing: true,
            ..ctx()
        };
        for (key, m) in CHORDS {
            if m.alt {
                continue;
            }
            assert_eq!(review_key(key, m, &c), None, "{key:?} {m:?}");
        }
    }

    #[test]
    fn outside_the_card_only_alt_arrows_wake_it() {
        let c = ReviewCtx {
            card_focus: false,
            ..ctx()
        };
        for (key, m) in CHORDS {
            if !m.alt {
                assert_eq!(review_key(key, m, &c), None, "{key:?} {m:?}");
            }
        }
        assert_eq!(
            review_key(Key::ArrowDown, Modifiers::ALT, &c),
            Some(ReviewAct::Move(1))
        );
    }

    #[test]
    fn apply_key_needs_something_to_apply() {
        let c = ReviewCtx {
            can_apply: false,
            ..ctx()
        };
        assert_eq!(review_key(Key::Enter, Modifiers::COMMAND, &c), None);
        let none = ReviewCtx {
            has_items: false,
            ..ctx()
        };
        assert_eq!(review_key(Key::A, Modifiers::NONE, &none), None);
    }

    #[test]
    fn a_focused_button_keeps_its_space() {
        let c = ReviewCtx {
            widget_focus: true,
            ..ctx()
        };
        assert_eq!(review_key(Key::Space, Modifiers::NONE, &c), None);
        assert_eq!(review_key(Key::A, Modifiers::NONE, &c), Some(ReviewAct::Accept));
    }

    #[test]
    fn every_card_chord_is_free_in_the_global_keymap() {
        let reg = ddc_ui::commands::Registry::builtin();
        let chord = |key: Key, m: Modifiers| {
            let mut s = String::new();
            if m.command {
                s.push_str("Ctrl+");
            }
            if m.alt {
                s.push_str("Alt+");
            }
            if m.shift {
                s.push_str("Shift+");
            }
            s.push_str(match key {
                Key::Enter => "Enter",
                Key::Escape => "Escape",
                Key::Space => "Space",
                Key::ArrowUp => "Up",
                Key::ArrowDown => "Down",
                other => other.name(),
            });
            s
        };
        for (key, m) in CHORDS {
            let want = chord(key, m);
            let owners: Vec<&str> = reg
                .all()
                .iter()
                .filter(|c| c.shortcuts.iter().any(|s| s.eq_ignore_ascii_case(&want)))
                .map(|c| c.id.as_str())
                .collect();
            // Ctrl+Enter is the Apply command itself; nothing else may own a card chord
            let ok = owners.is_empty() || (want == "Ctrl+Enter" && owners == ["ai.apply"]);
            assert!(ok, "{want} is also bound globally: {owners:?}");
        }
    }

    #[test]
    fn escape_keeps_the_draft_and_every_mark() {
        let items = vec![item(1, true, &[]), item(2, false, &[])];
        let diff = vec![diff(1, RowKind::Created), diff(2, RowKind::Deleted)];
        let o = outcome(ReviewAct::Leave, &items, &diff, Some(2));
        assert!(o.leave && !o.apply);
        assert!(o.check.is_empty());
        assert_eq!(o.cursor, Some(2));
    }

    #[test]
    fn accept_and_reject_flip_one_item_and_are_idempotent() {
        let items = vec![item(1, true, &[]), item(2, false, &[])];
        let d = vec![];
        let o = outcome(ReviewAct::Accept, &items, &d, Some(2));
        assert_eq!(o.check, vec![(2, true)]);
        assert!(outcome(ReviewAct::Accept, &items, &d, Some(1)).check.is_empty());
        assert_eq!(outcome(ReviewAct::Reject, &items, &d, Some(1)).check, vec![(1, false)]);
        assert!(outcome(ReviewAct::Reject, &items, &d, Some(2)).check.is_empty());
        // no cursor yet: the first row
        assert_eq!(outcome(ReviewAct::Reject, &items, &d, None).cursor, Some(1));
    }

    #[test]
    fn rejecting_pulls_dependants_like_the_mouse_does() {
        let items = vec![item(1, true, &[]), item(2, true, &[1])];
        let o = outcome(ReviewAct::Reject, &items, &[], Some(1));
        assert_eq!(o.check, vec![(1, false), (2, false)]);
    }

    #[test]
    fn group_verdict_covers_the_kind_of_the_cursor_row() {
        let items = vec![
            item(1, true, &[]),
            item(2, true, &[]),
            item(3, true, &[]),
        ];
        let diff = vec![
            diff(1, RowKind::Deleted),
            diff(2, RowKind::Created),
            diff(3, RowKind::Deleted),
        ];
        let mut o = outcome(ReviewAct::RejectGroup, &items, &diff, Some(1)).check;
        o.sort();
        assert_eq!(o, vec![(1, false), (3, false)]);
        let mut back = items.clone();
        for (id, v) in &o {
            back.iter_mut().find(|i| i.id == *id).unwrap().checked = *v;
        }
        let mut o = outcome(ReviewAct::AcceptGroup, &back, &diff, Some(3)).check;
        o.sort();
        assert_eq!(o, vec![(1, true), (3, true)]);
    }

    #[test]
    fn counts_say_n_of_m() {
        let items = vec![item(1, true, &[]), item(2, false, &[]), item(3, true, &[])];
        assert_eq!(accepted(&items), (2, 3));
        assert_eq!(accepted(&[]), (0, 0));
    }

    #[test]
    fn the_cursor_wraps_both_ways() {
        let items = vec![item(1, true, &[]), item(2, true, &[]), item(3, true, &[])];
        assert_eq!(outcome(ReviewAct::Move(1), &items, &[], None).cursor, Some(1));
        assert_eq!(outcome(ReviewAct::Move(1), &items, &[], Some(3)).cursor, Some(1));
        assert_eq!(outcome(ReviewAct::Move(-1), &items, &[], Some(1)).cursor, Some(3));
        assert_eq!(outcome(ReviewAct::Move(-1), &items, &[], None).cursor, Some(3));
    }
}
