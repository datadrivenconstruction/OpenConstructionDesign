// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Merging a design option into the live model, whole or by parts. The option's plan items are
//! intents: a merge copies the picked ones plus their dependency closure into a new plan card,
//! which `plan::replay` compiles against the copy of the live model. Only the person's Apply of
//! that card writes to the live model, and the same journal entry marks the option merged.

use super::plan::{ElRef, IdOrNew, PlanItem, PlanOp, PlanState, Targets};
use super::sets::Sets;
use ddc_model::{ElKey, OptionStatus};
use std::collections::{BTreeMap, BTreeSet};

/// What the person merges.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// Every ticked item of the option.
    Whole,
    /// Groups of the option: its plan item ids.
    Items(Vec<u32>),
    /// Elements selected in the option's tile (ids of the option copy). An element takes every
    /// item that created or changed it: the element as the option shows it.
    Elements(Vec<ElKey>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeOpts {
    /// A picked part that needs a left-out one pulls it in; off: the merge is refused.
    pub pull_deps: bool,
    /// The live model moved past the option base: compile against it with a note, or refuse.
    pub rebase: bool,
}

impl Default for MergeOpts {
    fn default() -> Self {
        MergeOpts {
            pull_deps: true,
            rebase: true,
        }
    }
}

/// Why a part of the option is not on the card. `code` is the locale key suffix
/// (`ai_opt_merge.left.<code>`).
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    NotPicked,
    /// Unticked on the option card and needed by nothing picked.
    Unticked,
    /// Compiling against the live model failed; the text says what is in the way.
    Conflict(String),
}

impl Reason {
    /// What the card shows next to the unticked item; the panel may translate it by `code`.
    pub fn text(&self) -> &str {
        match self {
            Reason::NotPicked => "not picked from the option",
            Reason::Unticked => "unticked in the option",
            Reason::Conflict(t) => t.as_str(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Reason::NotPicked => "not_picked",
            Reason::Unticked => "unticked",
            Reason::Conflict(_) => "conflict",
        }
    }
}

/// Why no card was made. `code` is the locale key suffix (`ai_opt_merge.refused.<code>`).
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// Rejected or applied whole: a rejected option is revived first.
    NotMergeable(OptionStatus),
    /// The live model moved past the option base and rebasing is off.
    Stale,
    NothingPicked,
    UnknownItem(u32),
    /// The selected element was neither created nor changed by the option.
    NotInOption(ElKey),
    /// `item` needs `needs`, which is not picked, and pulling dependencies is off.
    NeedsLeftOut { item: u32, needs: u32 },
    /// `item` works on a frozen selection set the option session no longer has.
    SetGone { item: u32, set: u32 },
    /// Another plan is on the card, or the worker is busy: nothing is replaced unasked.
    Busy,
    Model(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::NotMergeable(_) => "not_mergeable",
            Refusal::Stale => "stale",
            Refusal::NothingPicked => "nothing_picked",
            Refusal::UnknownItem(_) => "unknown_item",
            Refusal::NotInOption(_) => "not_in_option",
            Refusal::NeedsLeftOut { .. } => "needs_left_out",
            Refusal::SetGone { .. } => "set_gone",
            Refusal::Busy => "busy",
            Refusal::Model(_) => "failed",
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotMergeable(s) => write!(f, "option is {s:?}"),
            Refusal::Stale => f.write_str("the model changed after the option base"),
            Refusal::NothingPicked => f.write_str("nothing picked"),
            Refusal::UnknownItem(i) => write!(f, "no item {i} in the option"),
            Refusal::NotInOption((t, id)) => write!(f, "{t}:{id} is not part of the option"),
            Refusal::NeedsLeftOut { item, needs } => write!(f, "item {item} needs item {needs}"),
            Refusal::SetGone { item, set } => write!(f, "item {item}: set s{set} is gone"),
            Refusal::Busy => f.write_str("another plan is open"),
            Refusal::Model(e) => f.write_str(e),
        }
    }
}

/// The option as its session holds it: items compiled on the option copy.
pub struct MergeSource<'a> {
    pub option: i64,
    pub status: OptionStatus,
    /// `(head, stamp)` of the option base, as `Model::ai_option_head` gives it.
    pub base: (i64, i64),
    pub plan: &'a PlanState,
    pub sets: &'a Sets,
}

/// A new plan card. Reports use the option's item numbers; card item = option item + `item_off`.
pub struct MergeCard {
    pub option: i64,
    pub plan: PlanState,
    /// Option items on the card, picked or pulled.
    pub kept: Vec<u32>,
    /// `(pulled, needed_by)`: dependencies added to the pick.
    pub pulled: Vec<(u32, u32)>,
    pub left_out: Vec<(u32, Reason)>,
    /// The live model moved past the option base: the card is compiled against the live model.
    pub rebased: bool,
    pub item_off: u32,
    pub slot_off: u32,
    /// The person's note why this option, kept in the option history after Apply.
    pub note: Option<String>,
}

/// Builds the card. `numbering` is the `(next_item, next_slot)` of the session the card goes to:
/// its numbers keep counting so the conversation's old item numbers stay unambiguous.
pub fn merge_card(
    src: &MergeSource<'_>,
    pick: &Pick,
    live: (i64, i64),
    opts: MergeOpts,
    numbering: (u32, u32),
    note: Option<String>,
) -> Result<MergeCard, Refusal> {
    if !matches!(src.status, OptionStatus::Draft | OptionStatus::Merged) {
        return Err(Refusal::NotMergeable(src.status));
    }
    let rebased = live != src.base;
    if rebased && !opts.rebase {
        return Err(Refusal::Stale);
    }
    let by_id: BTreeMap<u32, &PlanItem> = src.plan.items.iter().map(|i| (i.id, i)).collect();
    let picked = picked_items(src.plan, &by_id, pick)?;
    if picked.is_empty() {
        return Err(Refusal::NothingPicked);
    }
    let mut kept = picked.clone();
    let mut pulled = vec![];
    let mut stack: Vec<u32> = picked.iter().copied().collect();
    while let Some(id) = stack.pop() {
        for d in &by_id[&id].depends_on {
            if kept.contains(d) {
                continue;
            }
            if !by_id.contains_key(d) {
                return Err(Refusal::UnknownItem(*d));
            }
            if !opts.pull_deps {
                return Err(Refusal::NeedsLeftOut {
                    item: id,
                    needs: *d,
                });
            }
            kept.insert(*d);
            pulled.push((*d, id));
            stack.push(*d);
        }
    }
    pulled.sort_unstable();
    let mut items = vec![];
    let mut left_out = vec![];
    // option order: replay compiles in order and a dependency always comes first
    for it in &src.plan.items {
        let keep = kept.contains(&it.id);
        let mut op = it.op.clone();
        let frozen = freeze_sets(&mut op, src.sets);
        let mut conflict = None;
        if !keep {
            let why = if it.checked {
                Reason::NotPicked
            } else {
                Reason::Unticked
            };
            // left out but on the card, unticked with its reason: ticking it brings it back
            conflict = Some(why.text().to_string());
            left_out.push((it.id, why));
            if frozen.is_err() {
                continue;
            }
        }
        frozen.map_err(|set| Refusal::SetGone { item: it.id, set })?;
        items.push(PlanItem {
            id: it.id,
            title: it.title.clone(),
            why: it.why.clone(),
            op,
            checked: keep,
            depends_on: it.depends_on.clone(),
            actor: it.actor.clone(),
            conflict,
            sets: it.sets.clone(),
        });
    }
    let on_card: BTreeSet<u32> = items.iter().map(|i| i.id).collect();
    let plan = PlanState {
        items,
        next_item: src.plan.next_item,
        next_slot: src.plan.next_slot,
        slot_owner: src.plan.slot_owner.filtered(|o| on_card.contains(&o)),
        variants: src.plan.variants.clone(),
        variant: src.plan.variant,
        allow_large: src.plan.allow_large,
        units: src
            .plan
            .units
            .iter()
            .filter(|(i, _)| on_card.contains(*i))
            .map(|(i, u)| (*i, *u))
            .collect(),
        ..PlanState::default()
    };
    let mut card = MergeCard {
        option: src.option,
        plan,
        kept: kept.into_iter().collect(),
        pulled,
        left_out,
        rebased,
        item_off: 0,
        slot_off: 0,
        note,
    };
    shift_card(&mut card, numbering.0, numbering.1);
    Ok(card)
}

/// Moves the card's item and slot numbers up by `items` and `slots`: the worker does this with
/// its own counters, so numbers already used in the conversation are never reused.
pub fn shift_card(card: &mut MergeCard, items: u32, slots: u32) {
    if items == 0 && slots == 0 {
        return;
    }
    card.item_off += items;
    card.slot_off += slots;
    let st = &mut card.plan;
    for it in st.items.iter_mut() {
        it.id += items;
        it.depends_on.iter_mut().for_each(|d| *d += items);
        renumber(&mut it.op, slots);
    }
    st.next_item += items;
    st.next_slot += slots;
    st.slot_owner.shift(slots, items);
    st.units = st.units.drain().map(|(i, u)| (i + items, u)).collect();
}

fn picked_items(
    st: &PlanState,
    by_id: &BTreeMap<u32, &PlanItem>,
    pick: &Pick,
) -> Result<BTreeSet<u32>, Refusal> {
    match pick {
        Pick::Whole => Ok(st.items.iter().filter(|i| i.checked).map(|i| i.id).collect()),
        Pick::Items(ids) => {
            if let Some(bad) = ids.iter().find(|i| !by_id.contains_key(*i)) {
                return Err(Refusal::UnknownItem(*bad));
            }
            Ok(ids.iter().copied().collect())
        }
        Pick::Elements(keys) => {
            let mut out = BTreeSet::new();
            for k in keys {
                let hit: Vec<u32> = st
                    .declared
                    .iter()
                    .filter(|(id, d)| by_id.contains_key(*id) && d.contains(k))
                    .map(|(id, _)| *id)
                    .collect();
                if hit.is_empty() {
                    return Err(Refusal::NotInOption(k.clone()));
                }
                out.extend(hit);
            }
            Ok(out)
        }
    }
}

/// A set frozen in the option session means nothing to the live session: it travels as ids.
fn freeze_sets(op: &mut PlanOp, sets: &Sets) -> Result<(), u32> {
    let target = match op {
        PlanOp::Assign { target, .. }
        | PlanOp::ReplaceType { target, .. }
        | PlanOp::Delete { target }
        | PlanOp::Modify { target, .. } => target,
        _ => return Ok(()),
    };
    if let Targets::Set(n) = *target {
        let s = sets.get(n).filter(|s| s.replaced.is_none()).ok_or(n)?;
        *target = Targets::Refs(
            s.keys
                .iter()
                .map(|k| ElRef {
                    cat: k.cat.to_string(),
                    id: IdOrNew::Id(k.id),
                })
                .collect(),
        );
    }
    Ok(())
}

fn shift(r: &mut ElRef, by: u32) {
    if let IdOrNew::New(n) = &mut r.id {
        *n += by;
    }
}

fn shift_targets(t: &mut Targets, by: u32) {
    if let Targets::Refs(v) = t {
        v.iter_mut().for_each(|r| shift(r, by));
    }
}

fn renumber(op: &mut PlanOp, by: u32) {
    if by == 0 {
        return;
    }
    match op {
        PlanOp::Assign { target, .. }
        | PlanOp::Delete { target }
        | PlanOp::Modify { target, .. } => shift_targets(target, by),
        PlanOp::ReplaceType { target, to } => {
            shift_targets(target, by);
            shift(to, by);
        }
        PlanOp::Create { slot0, items } => {
            *slot0 += by;
            for e in items.iter_mut() {
                e.refs_mut().into_iter().for_each(|r| shift(r, by));
            }
        }
        PlanOp::Generate { slot0, .. }
        | PlanOp::Geometry { slot0, .. }
        | PlanOp::Script { slot0, .. } => *slot0 += by,
        PlanOp::Project(p) => p.slot0 += by,
        PlanOp::Plant(p) => p.slot0 += by,
        PlanOp::NewType(t) => t.slot += by,
        PlanOp::NewParam(_)
        | PlanOp::LoadFamily { .. }
        | PlanOp::DefineFamily { .. }
        | PlanOp::Fix(_)
        | PlanOp::WallType(_)
        | PlanOp::DrawingSet(..)
        | PlanOp::Mep(_)
        | PlanOp::Tool(_) => {}
    }
}

/// After the card was compiled against the live model: an item that failed there, or lost a
/// dependency that failed, leaves the card and is listed with the reason.
pub fn after_replay(card: &mut MergeCard, st: &PlanState) {
    for it in &st.items {
        let Some(src) = it.id.checked_sub(card.item_off) else {
            continue;
        };
        if !card.kept.contains(&src) || (it.checked && it.conflict.is_none()) {
            continue;
        }
        let why = it.conflict.clone().unwrap_or_else(|| "unticked".to_string());
        card.kept.retain(|k| *k != src);
        card.left_out.push((src, Reason::Conflict(why)));
    }
    card.left_out.sort_by_key(|(id, _)| *id);
}

/// One line for the audit and the option history: stable codes, option item numbers.
pub fn summary(card: &MergeCard) -> String {
    let list = |v: &[u32]| {
        v.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut s = format!("option {}: kept {}", card.option, list(&card.kept[..]));
    if !card.pulled.is_empty() {
        let p: Vec<String> = card.pulled.iter().map(|(a, b)| format!("{a}<-{b}")).collect();
        s += &format!("; pulled {}", p.join(","));
    }
    for (id, why) in &card.left_out {
        s += &format!("; left {id}:{}", why.code());
        if let Reason::Conflict(t) = why {
            s += &format!(" ({t})");
        }
    }
    if card.rebased {
        s += "; rebased on the live model";
    }
    s
}

#[cfg(test)]
#[path = "opt_merge_tests.rs"]
mod tests;
