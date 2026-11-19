// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A follow-up message revises the open draft: what changed since the person last looked, which
//! check marks carry over, and whether Apply may target the revision on show.

use super::kit::{self, space, Button};
use super::{Action, Cx};
use crate::ai::plan::PlanItem;
use crate::ai::value_edit::{self, Field, Value};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::Role;
use egui::{self, RichText, Ui};
use std::collections::BTreeMap;

/// Titles listed in the strip's tooltip before "+N more".
pub const TIP_TITLES: usize = 12;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanDelta {
    /// `(id, title)` on the new card.
    pub added: Vec<(u32, String)>,
    /// `(old id, title)` gone from the card.
    pub removed: Vec<(u32, String)>,
    /// `(new id, title)`: the same item with other content.
    pub changed: Vec<(u32, String)>,
}

impl PlanDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// Rows the card marks until the person acknowledges the revision.
    pub fn marks(&self, id: u32) -> bool {
        self.added.iter().chain(&self.changed).any(|(i, _)| *i == id)
    }

    pub fn note(&self) -> Note {
        Note {
            added: self.added.len(),
            removed: self.removed.len(),
            changed: self.changed.len(),
        }
    }

    /// Hover text: one title per line, marked like the counts.
    pub fn tip(&self, cat: &Catalog) -> String {
        let all: Vec<String> = self
            .added
            .iter()
            .map(|(_, t)| format!("+ {t}"))
            .chain(self.changed.iter().map(|(_, t)| format!("~ {t}")))
            .chain(self.removed.iter().map(|(_, t)| format!("- {t}")))
            .collect();
        let mut lines: Vec<String> = all.iter().take(TIP_TITLES).cloned().collect();
        if all.len() > TIP_TITLES {
            let more = (all.len() - TIP_TITLES).to_string();
            lines.push(cat.tf("ai.redraft.more", &[("n", &more)]));
        }
        lines.join("\n")
    }
}

/// The counts of an acknowledged revision, kept in the chat history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Note {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

impl Note {
    pub fn to_row(&self) -> String {
        format!("+{} -{} ~{}", self.added, self.removed, self.changed)
    }

    pub fn from_row(text: &str) -> Option<Note> {
        let mut it = text.split_whitespace();
        let mut num = |p: char| -> Option<usize> { it.next()?.strip_prefix(p)?.parse().ok() };
        let n = Note {
            added: num('+')?,
            removed: num('-')?,
            changed: num('~')?,
        };
        (it.next().is_none() && n != Note::default()).then_some(n)
    }

    /// `+2 added · -1 removed · ~3 changed`, zero parts left out.
    pub fn counts(&self, cat: &Catalog) -> String {
        [
            ("ai.redraft.added", self.added),
            ("ai.redraft.removed", self.removed),
            ("ai.redraft.changed", self.changed),
        ]
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| cat.tf(k, &[("n", &n.to_string())]))
        .collect::<Vec<_>>()
        .join("  \u{b7}  ")
    }

    /// The strip line on the card.
    pub fn strip(&self, cat: &Catalog) -> String {
        cat.tf("ai.redraft.strip", &[("what", &self.counts(cat))])
    }

    /// The chat line once the person acknowledged the revision.
    pub fn history(&self, cat: &Catalog) -> String {
        cat.tf("ai.redraft.note", &[("what", &self.counts(cat))])
    }
}

/// What decides "changed"; the check mark is the person's, so it never counts.
/// The elements `keyed` folded in are compared apart: a side that knows none matches any.
fn sig(i: &PlanItem) -> (String, &str, String) {
    (title_key(&i.title), split_keys(&i.why).0.trim(), format!("{:?}", i.op))
}

const KEYS: char = '\u{1f}';

fn split_keys(why: &str) -> (&str, &str) {
    why.split_once(KEYS).unwrap_or((why, ""))
}

fn same(a: &PlanItem, b: &PlanItem) -> bool {
    let (ka, kb) = (split_keys(&a.why).1, split_keys(&b.why).1);
    sig(a) == sig(b) && (ka.is_empty() || kb.is_empty() || ka == kb)
}

fn title_key(t: &str) -> String {
    t.trim().to_lowercase()
}

/// `delta` plus the title matches whose content stayed: `(old id, new id)`.
fn delta_and_moves(old: &[PlanItem], new: &[PlanItem]) -> (PlanDelta, Vec<(u32, u32)>) {
    let mut d = PlanDelta::default();
    let mut moved = Vec::new();
    if old.is_empty() {
        return (d, moved);
    }
    let by_id: BTreeMap<u32, &PlanItem> = old.iter().map(|i| (i.id, i)).collect();
    // items whose id left the card: a new item with the same title is their redraft
    let mut orphans: Vec<&PlanItem> = old
        .iter()
        .filter(|o| !new.iter().any(|n| n.id == o.id))
        .collect();
    for n in new {
        if let Some(o) = by_id.get(&n.id) {
            if !same(o, n) {
                d.changed.push((n.id, n.title.clone()));
            }
            continue;
        }
        let key = title_key(&n.title);
        let hit = if key.is_empty() {
            None
        } else {
            orphans.iter().position(|o| title_key(&o.title) == key)
        };
        match hit {
            Some(p) => {
                let o = orphans.remove(p);
                if same(o, n) {
                    moved.push((o.id, n.id));
                } else {
                    d.changed.push((n.id, n.title.clone()));
                }
            }
            None => d.added.push((n.id, n.title.clone())),
        }
    }
    d.removed = orphans.into_iter().map(|o| (o.id, o.title.clone())).collect();
    (d, moved)
}

/// Keyed by item id, with the title as fallback for an item the assistant re-added.
/// An empty `old` is the first draft: no strip.
pub fn plan_delta(old: &[PlanItem], new: &[PlanItem]) -> PlanDelta {
    delta_and_moves(old, new).0
}

/// Ticks to send for items this revision touched: a changed item comes back checked, a re-added
/// unchanged one keeps the person's choice. Items the revision left alone are never touched.
fn tick_fixes(
    seen: &[PlanItem],
    last: &[PlanItem],
    d: &PlanDelta,
    moved: &[(u32, u32)],
    new: &[PlanItem],
) -> Vec<(u32, bool)> {
    let fresh = |n: &PlanItem| match last.iter().find(|l| l.id == n.id) {
        Some(l) => !same(l, n),
        None => true,
    };
    let was_checked = |id: u32| {
        last.iter()
            .chain(seen)
            .find(|i| i.id == id)
            .map(|i| i.checked)
    };
    let mut out = Vec::new();
    for n in new.iter().filter(|n| fresh(*n)) {
        if d.changed.iter().any(|(id, _)| *id == n.id) {
            if !n.checked {
                out.push((n.id, true));
            }
        } else if let Some((old, _)) = moved.iter().find(|(_, id)| *id == n.id) {
            if let Some(on) = was_checked(*old).filter(|on| *on != n.checked) {
                out.push((n.id, on));
            }
        }
    }
    out
}

/// What the person last looked at and how the card on show differs from it.
#[derive(Clone, Debug, Default)]
pub struct Redraft {
    /// The card as of the last look; empty before the first draft.
    seen: Vec<PlanItem>,
    /// The card on show.
    last: Vec<PlanItem>,
    delta: PlanDelta,
    /// Bumped on every card that arrives (never reset): Apply and "Got it" name the one shown.
    rev: u64,
}

/// Items with the elements each one touches folded into what decides "changed": the same title
/// and params on other elements is another item, so a check mark never carries over to it.
pub fn keyed(items: &[PlanItem], diff: &[crate::ai::plan::ElemDiff]) -> Vec<PlanItem> {
    items
        .iter()
        .map(|i| {
            let mut keys: Vec<String> = diff
                .iter()
                .filter(|d| d.item == Some(i.id))
                .map(|d| format!("{}:{}", d.key.0, d.key.1))
                .collect();
            keys.sort();
            keys.dedup();
            let mut k = i.clone();
            if !keys.is_empty() {
                k.why = format!("{}{KEYS}{}", i.why.trim(), keys.join(","));
            }
            k
        })
        .collect()
}

impl Redraft {
    /// A message goes out: the card on show is what the person looked at, unless a revision they
    /// have not acknowledged is still pending; then the strip keeps counting from the older look.
    pub fn on_send(&mut self) {
        if self.delta.is_empty() {
            self.seen = self.last.clone();
        }
    }

    /// A new card arrived; returns the ticks to send back.
    pub fn on_plan(&mut self, items: &[PlanItem]) -> Vec<(u32, bool)> {
        let rev = self.rev + 1;
        if items.is_empty() {
            // no card: the next draft is a first draft again
            *self = Redraft {
                rev,
                ..Redraft::default()
            };
            return vec![];
        }
        let items = &self.remembered(items);
        let (d, moved) = delta_and_moves(&self.seen, items);
        let fixes = tick_fixes(&self.seen, &self.last, &d, &moved, items);
        self.delta = d;
        self.last = items.to_vec();
        self.rev = rev;
        fixes
    }

    /// An unticked item compiles to nothing: it keeps the elements it showed while ticked.
    fn remembered(&self, items: &[PlanItem]) -> Vec<PlanItem> {
        items
            .iter()
            .map(|n| {
                let mut n = n.clone();
                if split_keys(&n.why).1.is_empty() {
                    let old = self.last.iter().chain(&self.seen).find(|o| o.id == n.id);
                    if let Some(o) = old.filter(|o| sig(o) == sig(&n)) {
                        n.why.clone_from(&o.why);
                    }
                }
                n
            })
            .collect()
    }

    /// Revision of the card on show.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// A card is on show (a follow-up that empties it is worth a note).
    pub fn showing(&self) -> bool {
        !self.last.is_empty()
    }

    /// "Got it": the card on show becomes the last look. The counts go to the chat history.
    /// `rev` names the card the person saw; a newer one arrived meanwhile stays unacknowledged.
    pub fn acknowledge(&mut self, rev: Option<u64>) -> Option<Note> {
        if self.delta.is_empty() || rev.is_some_and(|r| r != self.rev) {
            return None;
        }
        let note = self.delta.note();
        self.seen = self.last.clone();
        self.delta = PlanDelta::default();
        Some(note)
    }

    pub fn clear(&mut self) {
        *self = Redraft {
            rev: self.rev,
            ..Redraft::default()
        };
    }

    pub fn delta(&self) -> &PlanDelta {
        &self.delta
    }

    pub fn pending(&self) -> bool {
        !self.delta.is_empty()
    }

    /// Apply targets only the revision the person has seen.
    pub fn apply_armed(&self) -> bool {
        apply_armed(&self.delta, false)
    }

    /// The person's own value edits count as seen, field by field: a row whose only change is
    /// theirs needs no "Got it", and anything else the agent changed on it keeps its mark.
    pub fn adopt(&mut self, items: &[PlanItem], own: &[OwnEdit]) {
        for e in own {
            let Some(n) = items.iter().find(|i| i.id == e.item) else {
                continue;
            };
            for s in self.seen.iter_mut().filter(|s| s.id == e.item) {
                let mut mine = s.clone();
                if !value_edit::write(&mut mine.op, &e.field, &e.value) {
                    continue;
                }
                if same(&mine, n) {
                    let checked = s.checked;
                    *s = n.clone();
                    s.checked = checked;
                } else {
                    s.op = mine.op;
                }
            }
        }
    }
}

/// One value the person corrected on the card.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnEdit {
    pub item: u32,
    pub field: Field,
    pub value: Value,
}

/// What the card tracks beside the redraft: the person's own value edits on their way to the
/// worker, and which rows a recompile against the changed live model touched.
#[derive(Clone, Debug, Default)]
pub struct CardTrack {
    /// An edit the card shows once the worker has acknowledged the `u64`.
    own: Vec<(OwnEdit, u64)>,
    /// The card before the copy was resynced; the recompiled card is compared with it.
    before_sync: Option<Vec<PlanItem>>,
    /// Rows the recompile changed; kept until Apply, Reject or the next request.
    rebased: PlanDelta,
}

impl CardTrack {
    pub fn own_edit(&mut self, edit: OwnEdit, ack: u64) {
        self.own.push((edit, ack));
    }

    /// The own edits the card with `ack` carries.
    pub fn take_own(&mut self, ack: u64) -> Vec<OwnEdit> {
        let (done, wait): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.own).into_iter().partition(|(_, a)| *a <= ack);
        self.own = wait;
        done.into_iter().map(|(e, _)| e).collect()
    }

    /// The copy was resynced with `items` on show; the next card is the recompiled one.
    pub fn on_synced(&mut self, items: Vec<PlanItem>) {
        self.before_sync = Some(items);
    }

    pub fn on_plan(&mut self, items: &[PlanItem]) {
        if let Some(old) = self.before_sync.take() {
            let d = plan_delta(&old, items);
            if !d.is_empty() {
                self.rebased = d;
            }
        }
    }

    pub fn rebased(&self) -> Option<&PlanDelta> {
        Some(&self.rebased).filter(|d| !d.is_empty())
    }

    pub fn clear(&mut self) {
        *self = CardTrack::default();
    }
}

/// Armed when nothing changed since the last look, or the change was acknowledged.
pub fn apply_armed(delta: &PlanDelta, seen: bool) -> bool {
    delta.is_empty() || seen
}

/// Top of the card: the counts with titles on hover, removed items folded away, and "Got it",
/// which runs the card's review path and arms Apply for the revision on show.
pub fn strip(ui: &mut Ui, cx: &Cx<'_>, d: &PlanDelta, acts: &mut Vec<Action>) {
    if d.is_empty() {
        return;
    }
    let (th, cat) = (cx.th, cx.cat);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        kit::badge(ui, th, &cat.t("ai.redraft.badge"), kit::Tone::Accent);
        ui.add(
            egui::Label::new(
                RichText::new(d.note().strip(cat))
                    .font(th.font(Role::Label))
                    .color(th.c("text.primary")),
            )
            .wrap(),
        )
        .on_hover_text(d.tip(cat));
    });
    if !d.removed.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(cat.tf(
                "ai.redraft.removed_list",
                &[("n", &d.removed.len().to_string())],
            ))
            .font(th.font(Role::Caption))
            .color(th.c("text.secondary")),
        )
        .id_salt("ddc.ai.redraft.removed")
        .default_open(false)
        .show(ui, |ui| {
            for (_, t) in &d.removed {
                ui.add(
                    egui::Label::new(
                        RichText::new(t)
                            .font(th.font(Role::Caption))
                            .color(th.c("text.disabled"))
                            .strikethrough(),
                    )
                    .truncate(),
                )
                .on_hover_text(t);
            }
        });
    }
    if Button::secondary(&cat.t("ai.redraft.ack"))
        .small()
        .tooltip(&cat.t("ai.redraft.ack.tip"), "")
        .show(ui, th)
        .clicked()
    {
        acts.push(Action::Review);
    }
    ui.add_space(space::XS);
}

/// The accent dot before an added or changed row's title.
pub fn row_mark(ui: &mut Ui, cx: &Cx<'_>) {
    let size = egui::vec2(space::SM, space::SM);
    let (rect, r) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size.x * 0.3, cx.th.c("accent"));
    r.on_hover_text(cx.cat.t("ai.redraft.mark.tip"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::plan::{PlanOp, Targets};

    fn item(id: u32, title: &str, why: &str, on: bool) -> PlanItem {
        PlanItem {
            id,
            title: title.to_string(),
            why: why.to_string(),
            op: PlanOp::Delete {
                target: Targets::Selection,
            },
            checked: on,
            depends_on: vec![],
            actor: String::new(),
            conflict: None,
            sets: Default::default(),
        }
    }

    fn draft1() -> Vec<PlanItem> {
        vec![
            item(1, "Walls 200", "a", true),
            item(2, "Doors", "b", false),
            item(3, "Slab", "c", true),
        ]
    }

    #[test]
    fn counts_added_removed_changed() {
        let new = vec![
            item(1, "Walls 200", "a", true),
            item(3, "Slab", "thicker", true),
            item(4, "Roof", "d", true),
            item(5, "Stairs", "e", true),
        ];
        let d = plan_delta(&draft1(), &new);
        assert_eq!(d.added, vec![(4, "Roof".to_string()), (5, "Stairs".to_string())]);
        assert_eq!(d.removed, vec![(2, "Doors".to_string())]);
        assert_eq!(d.changed, vec![(3, "Slab".to_string())]);
        assert_eq!(
            d.note(),
            Note {
                added: 2,
                removed: 1,
                changed: 1
            }
        );
        assert!(d.marks(4) && d.marks(3) && !d.marks(1) && !d.marks(2));
    }

    #[test]
    fn title_fallback_matches_a_re_added_item() {
        let new = vec![
            item(1, "Walls 200", "a", true),
            item(7, "doors ", "b", true),
            item(8, "Slab", "other", true),
        ];
        let d = plan_delta(&draft1(), &new);
        assert!(d.added.is_empty(), "{d:?}");
        assert!(d.removed.is_empty(), "{d:?}");
        assert_eq!(d.changed, vec![(8, "Slab".to_string())]);
    }

    #[test]
    fn no_strip_on_first_draft_or_same_card() {
        assert!(plan_delta(&[], &draft1()).is_empty());
        assert!(plan_delta(&draft1(), &draft1()).is_empty());
        let mut r = Redraft::default();
        r.on_send();
        assert!(r.on_plan(&draft1()).is_empty());
        assert!(!r.pending());
        assert!(r.apply_armed());
    }

    #[test]
    fn tick_only_difference_is_not_changed() {
        let mut ticked = draft1();
        ticked[0].checked = false;
        ticked[1].checked = true;
        assert!(plan_delta(&draft1(), &ticked).is_empty());
    }

    fn wall(len: f64, height: f64) -> PlanItem {
        PlanItem {
            op: PlanOp::Create {
                slot0: 1,
                items: vec![crate::ai::plan::NewElem::Wall {
                    level: 1,
                    a: (0.0, 0.0),
                    b: (len, 0.0),
                    bulge: 0.0,
                    base_offset: 0.0,
                    height,
                    wtype: None,
                }],
            },
            ..item(1, "Wall", "w", true)
        }
    }

    /// The card the person saw, then one carrying their height edit as `new`.
    fn after_own_edit(new: PlanItem) -> Redraft {
        let mut r = Redraft::default();
        r.on_plan(&[wall(5.0, 3.0)]);
        r.on_send();
        let height = Field::Create {
            index: 0,
            num: value_edit::Num::Height,
        };
        let mine = OwnEdit {
            item: 1,
            field: height,
            value: Value::Metres(4.0),
        };
        let mut t = CardTrack::default();
        t.own_edit(mine.clone(), 3);
        assert!(t.take_own(2).is_empty(), "not acknowledged yet");
        let taken = t.take_own(3);
        assert_eq!(taken, vec![mine]);
        let new = [new];
        r.adopt(&new, &taken);
        r.on_plan(&new);
        r
    }

    #[test]
    fn an_own_value_edit_never_hides_the_agents_change_on_the_same_row() {
        // only the person's value changed: their own row needs no "Got it"
        let r = after_own_edit(wall(5.0, 4.0));
        assert!(!r.pending(), "{:?}", r.delta());
        // the agent lengthened the wall while the person set its height: the row stays marked
        let r = after_own_edit(wall(7.0, 4.0));
        assert!(r.delta().marks(1), "the agent's change was hidden: {:?}", r.delta());
        assert!(!r.apply_armed(), "Apply waits for the person to see the agent's change");
    }

    fn revised(r: &mut Redraft, new: &[PlanItem]) -> Vec<(u32, bool)> {
        r.on_plan(&draft1());
        r.on_send();
        r.on_plan(new)
    }

    #[test]
    fn unchanged_items_keep_check_state() {
        let mut r = Redraft::default();
        // item 2 stays unticked by id; "Slab" comes back under a new id with the same content
        let new = vec![
            item(1, "Walls 200", "a", true),
            item(2, "Doors", "b", false),
            item(9, "Slab", "c", true),
            item(4, "Roof", "d", true),
        ];
        let mut first = draft1();
        first[2].checked = false;
        r.on_plan(&first);
        r.on_send();
        let fixes = r.on_plan(&new);
        assert_eq!(fixes, vec![(9, false)], "re-added Slab keeps the person's untick");
        assert!(!fixes.iter().any(|(id, _)| *id == 2), "item 2 is left alone");
        assert_eq!(r.delta().added, vec![(4, "Roof".to_string())]);
    }

    #[test]
    fn changed_item_resets_to_checked_with_highlight() {
        let mut r = Redraft::default();
        let new = vec![
            item(1, "Walls 200", "a", true),
            item(2, "Doors", "wider", false),
            item(3, "Slab", "c", true),
        ];
        assert_eq!(revised(&mut r, &new), vec![(2, true)]);
        assert!(r.delta().marks(2));
        // the worker's reply with the tick: no second reset, still highlighted
        let mut ticked = new.clone();
        ticked[1].checked = true;
        assert!(r.on_plan(&ticked).is_empty());
        assert!(r.delta().marks(2));
        // the person unticks it again: their choice stands
        ticked[1].checked = false;
        assert!(r.on_plan(&ticked).is_empty());
    }

    #[test]
    fn apply_on_stale_revision_refused() {
        let mut r = Redraft::default();
        let new = vec![item(1, "Walls 200", "a", true)];
        revised(&mut r, &new);
        assert!(r.pending());
        assert!(!r.apply_armed(), "the revision on show was not seen");
        assert!(!apply_armed(r.delta(), false));
        assert!(apply_armed(r.delta(), true));
    }

    #[test]
    fn acknowledging_clears_the_strip() {
        let mut r = Redraft::default();
        let new = vec![item(1, "Walls 200", "a", true), item(4, "Roof", "d", true)];
        revised(&mut r, &new);
        let note = r.acknowledge(Some(r.rev())).unwrap();
        assert_eq!(
            note,
            Note {
                added: 1,
                removed: 2,
                changed: 0
            }
        );
        assert!(!r.pending() && r.apply_armed());
        assert_eq!(r.acknowledge(None), None, "nothing left to acknowledge");
        // the next message counts from the acknowledged card
        r.on_send();
        assert!(r.on_plan(&new).is_empty());
        assert!(!r.pending());
    }

    #[test]
    fn unacknowledged_strip_keeps_counting_from_the_older_look() {
        let mut r = Redraft::default();
        revised(&mut r, &[item(1, "Walls 200", "a", true)]);
        r.on_send();
        r.on_plan(&[item(1, "Walls 200", "a", true), item(4, "Roof", "d", true)]);
        let n = r.delta().note();
        assert_eq!((n.added, n.removed), (1, 2), "{n:?}");
    }

    #[test]
    fn empty_card_starts_over() {
        let mut r = Redraft::default();
        revised(&mut r, &[item(1, "Walls 200", "a", true)]);
        assert!(r.on_plan(&[]).is_empty());
        assert!(!r.pending());
        r.on_send();
        r.on_plan(&draft1());
        assert!(!r.pending(), "a draft after an empty card is a first draft");
    }

    #[test]
    fn stale_acknowledge_is_refused_and_rev_never_rewinds() {
        let mut r = Redraft::default();
        revised(&mut r, &[item(1, "Walls 200", "a", true)]);
        let seen = r.rev();
        r.on_send();
        r.on_plan(&[item(1, "Walls 200", "a", true), item(4, "Roof", "d", true)]);
        assert!(r.rev() > seen);
        assert_eq!(r.acknowledge(Some(seen)), None, "a newer card arrived");
        assert!(r.pending());
        let before = r.rev();
        r.on_plan(&[]);
        assert!(r.rev() > before, "an empty card is a revision too");
        r.clear();
        assert!(r.rev() > before, "clear keeps the counter");
    }

    fn on(item: u32, id: i64) -> crate::ai::plan::ElemDiff {
        crate::ai::plan::ElemDiff {
            key: ("wall".to_string(), id),
            kind: ddc_model::RowKind::Changed,
            item: Some(item),
            level: None,
            label: String::new(),
            cells: vec![],
        }
    }

    #[test]
    fn same_title_on_other_elements_does_not_carry_the_untick() {
        // ticked it showed wall 10; unticked it compiles to nothing, so its card has no rows
        let unticked = |r: &mut Redraft| {
            r.on_plan(&keyed(&[item(1, "Walls 200", "a", true)], &[on(1, 10)]));
            r.on_plan(&keyed(&[item(1, "Walls 200", "a", false)], &[]));
            r.on_send();
        };
        let next = vec![item(5, "Walls 200", "a", true)];
        let mut r = Redraft::default();
        unticked(&mut r);
        let fixes = r.on_plan(&keyed(&next, &[on(5, 11)]));
        assert!(fixes.is_empty(), "other walls: no untick carried {fixes:?}");
        assert!(r.delta().marks(5));
        // the same elements under a new id keep the person's untick
        let mut r = Redraft::default();
        unticked(&mut r);
        let fixes = r.on_plan(&keyed(&next, &[on(5, 10)]));
        assert_eq!(fixes, vec![(5, false)]);
        assert!(!r.delta().marks(5));
        // the person's own tick on the same card is never a change
        let mut r = Redraft::default();
        unticked(&mut r);
        assert!(r.on_plan(&keyed(&[item(1, "Walls 200", "a", false)], &[])).is_empty());
        assert!(r.delta().is_empty());
    }

    #[test]
    fn note_row_round_trip() {
        let n = Note {
            added: 2,
            removed: 1,
            changed: 3,
        };
        assert_eq!(n.to_row(), "+2 -1 ~3");
        assert_eq!(Note::from_row(&n.to_row()), Some(n));
        assert_eq!(Note::from_row("garbage"), None);
        assert_eq!(Note::from_row("+1 -0"), None);
        assert_eq!(Note::from_row("+0 -0 ~0"), None);
        assert_eq!(Note::from_row("+1 -0 ~0 x"), None);
    }

    #[test]
    fn strings_exist_in_both_languages() {
        // a local catalog per language: the global UI language is never touched
        let en = Catalog::new(ddc_ui::Lang::En);
        for k in [
            "ai.redraft.added",
            "ai.redraft.removed",
            "ai.redraft.changed",
            "ai.redraft.strip",
            "ai.redraft.note",
            "ai.redraft.more",
            "ai.redraft.ack",
            "ai.redraft.ack.tip",
            "ai.redraft.removed_list",
            "ai.redraft.review_first",
            "ai.redraft.mark.tip",
            "ai.redraft.badge",
            "ai.redraft.emptied",
        ] {
            for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
                assert!(en.table(lang).contains_key(k), "{k} in {lang:?}");
            }
        }
        let n = Note {
            added: 2,
            removed: 0,
            changed: 3,
        };
        let line = n.strip(&en);
        assert!(line.contains("+2") && line.contains("~3") && !line.contains("-0"), "{line}");
        assert!(!line.contains('{'), "{line}");
        let d = PlanDelta {
            added: (0..20).map(|i| (i, format!("t{i}"))).collect(),
            ..PlanDelta::default()
        };
        assert_eq!(d.tip(&en).lines().count(), TIP_TITLES + 1);
    }
}
