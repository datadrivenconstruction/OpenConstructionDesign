//! Partial replan. Items a live edit conflicts with leave the plan with their conflict and their
//! intent; the agent drafts them again and the new items take the same places. The other items
//! stay as compiled: nothing they read changed, so nothing about them is recompiled.

use super::fmt::Out;
use super::plan::{PlanItem, PlanState};
use super::session::{Core, Done};
use ddc_ai::registry::{CmdError, ErrorCode};
use ddc_ai::CallCx;
use serde_json::{json, Value as J};
use std::collections::HashSet;

/// Automatic replans one item may go through; after that the person decides.
pub const MAX_TRIES: u8 = 2;

/// `ids` and every item that depends on one of them, in plan order.
pub fn with_dependents(items: &[PlanItem], ids: &[u32]) -> Vec<u32> {
    let mut out: HashSet<u32> = ids.iter().copied().collect();
    // a refilled item sits before items older than it: passes until nothing joins
    loop {
        let before = out.len();
        for it in items {
            if !out.contains(&it.id) && it.depends_on.iter().any(|d| out.contains(d)) {
                out.insert(it.id);
            }
        }
        if out.len() == before {
            break;
        }
    }
    items.iter().map(|i| i.id).filter(|i| out.contains(i)).collect()
}

/// A new item takes the first place its own actor's `replan_items` freed, else goes last.
pub fn place(st: &mut PlanState, item: PlanItem) {
    let Some(at) = st.refill.iter().position(|r| r.2 == item.actor) else {
        st.items.push(item);
        return;
    };
    let Some((before, tries, _)) = st.refill.remove(at) else {
        return;
    };
    st.tries.insert(item.id, tries);
    let at = before
        .and_then(|b| st.items.iter().position(|i| i.id == b))
        .unwrap_or(st.items.len());
    st.items.insert(at, item);
}

/// The automatic turn always drafts: whatever the person's last mode, Apply stays theirs.
pub const AUTO_MODE: ddc_ai::Mode = ddc_ai::Mode::Propose;

/// A turn the person stopped or that failed starts no automatic turn: it would spend tokens
/// on a dialog that just broke off.
pub fn after_turn<E>(res: &Result<ddc_ai::TurnEnd, E>) -> bool {
    matches!(res, Ok(ddc_ai::TurnEnd::Answered | ddc_ai::TurnEnd::LimitHit(_)))
}

/// The text of the automatic turn after a live edit held items, once per item and only while
/// the items have attempts left.
pub fn auto_text(c: &mut Core) -> Option<String> {
    if !c.auto_replan {
        return None;
    }
    let fresh: Vec<(u32, String)> = c
        .plan
        .items
        .iter()
        .filter(|i| !c.prompted.contains(&i.id))
        .filter(|i| c.plan.tries.get(&i.id).copied().unwrap_or(0) < MAX_TRIES)
        .filter_map(|i| c.plan.held.get(&i.id).map(|t| (i.id, t.clone())))
        .collect();
    if fresh.is_empty() {
        return None;
    }
    c.prompted.extend(fresh.iter().map(|f| f.0));
    let items: Vec<u32> = fresh.iter().map(|f| f.0).collect();
    let conflict: Vec<J> = fresh
        .iter()
        .map(|(id, t)| json!({"item": id, "conflict": t}))
        .collect();
    Some(format!(
        "The live model changed under plan items. Call replan_items with them, then add each \
         again as the person meant it, or say why it cannot be done.\n{}",
        json!({"conflict": conflict, "items": items})
    ))
}

fn ids_of(a: &J) -> Result<Vec<u32>, String> {
    let bad = || CmdError::wire(ErrorCode::InvalidArgs, "items must be a list of item numbers");
    let list = a["items"].as_array().ok_or_else(bad)?;
    let ids: Vec<u32> = list
        .iter()
        .map(|x| x.as_u64().and_then(|x| u32::try_from(x).ok()).ok_or_else(bad))
        .collect::<Result<_, _>>()?;
    if ids.is_empty() {
        return Err(bad());
    }
    Ok(ids)
}

pub fn replan_items(c: &mut Core, a: &J, cx: &CallCx<'_>) -> Result<Done, String> {
    let asked = ids_of(a)?;
    if let Some(id) = asked.iter().find(|id| !c.plan.items.iter().any(|i| i.id == **id)) {
        return Err(CmdError::wire(ErrorCode::NotFound, format!("no item {id}")));
    }
    let gone = with_dependents(&c.plan.items, &asked);
    let tries = gone
        .iter()
        .map(|i| c.plan.tries.get(i).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
    if tries >= MAX_TRIES {
        return Err(CmdError::wire(
            ErrorCode::Limit,
            format!("these items were drafted again {MAX_TRIES} times; the person decides"),
        ));
    }
    let mut out_items = Vec::new();
    let mut why = Vec::new();
    for (n, it) in c.plan.items.iter().enumerate() {
        if !gone.contains(&it.id) {
            continue;
        }
        let conflict = c.plan.held.get(&it.id).cloned().or_else(|| it.conflict.clone());
        let units = c.plan.units.get(&it.id).copied();
        let op = match super::lower::lower_op(&it.op, units) {
            Ok(ops) => serde_json::to_value(ops).unwrap_or(J::Null),
            Err(_) => json!(super::lower::op_kind(&it.op)),
        };
        why.push(format!("{}: {}", it.id, conflict.as_deref().unwrap_or("asked")));
        out_items.push(json!({
            "item": it.id, "title": it.title, "why": it.why, "conflict": conflict, "op": op,
        }));
        // the place it leaves: before the next item that stays
        let before = c.plan.items[n + 1..]
            .iter()
            .find(|i| !gone.contains(&i.id))
            .map(|i| i.id);
        c.plan.refill.push_back((before, tries + 1, cx.actor.to_string()));
    }
    // a held item has no steps; only items compiled on the copy need the plan replayed
    let compiled = c
        .plan
        .steps
        .iter()
        .any(|(id, s)| gone.contains(id) && !s.is_empty());
    let st = &mut c.plan;
    st.items.retain(|i| !gone.contains(&i.id));
    st.steps.retain(|(id, _)| !gone.contains(id));
    st.slot_owner.retain(|_, owner| !gone.contains(owner));
    for id in &gone {
        st.held.remove(id);
        st.declared.remove(id);
        st.skips.remove(id);
        st.units.remove(id);
        st.tries.remove(id);
    }
    let list = gone.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    c.audit_as(
        cx.actor,
        "replan",
        Some(format!("replan {list}")),
        Some(why.join("\n")),
        "ok",
    );
    if compiled {
        c.replan().map_err(|e| e.to_string())?;
    }
    let mut o = Out::new(c.max_chars);
    o.put("items", &json!(out_items));
    o.put("replayed", &json!(compiled));
    o.put("left", &json!(c.plan.items.len()));
    Ok(o.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, deps: &[u32]) -> PlanItem {
        by(id, deps, "ai")
    }

    fn by(id: u32, deps: &[u32], actor: &str) -> PlanItem {
        PlanItem {
            id,
            title: format!("i{id}"),
            why: String::new(),
            op: super::super::plan::PlanOp::Delete {
                target: super::super::plan::Targets::Refs(vec![]),
            },
            checked: true,
            depends_on: deps.to_vec(),
            actor: actor.to_string(),
            conflict: None,
            sets: Default::default(),
        }
    }

    #[test]
    fn dependents_follow_the_chain_in_plan_order() {
        let items = vec![item(1, &[]), item(2, &[1]), item(3, &[]), item(4, &[2]), item(5, &[3])];
        assert_eq!(with_dependents(&items, &[1]), vec![1, 2, 4]);
        assert_eq!(with_dependents(&items, &[3]), vec![3, 5]);
    }

    #[test]
    fn new_items_take_the_freed_places_in_order() {
        let mut st = PlanState {
            items: vec![item(1, &[]), item(4, &[])],
            ..PlanState::default()
        };
        st.refill.push_back((Some(4), 1, "ai".into()));
        st.refill.push_back((Some(4), 1, "ai".into()));
        st.refill.push_back((None, 1, "ai".into()));
        for id in [7, 8, 9, 10] {
            place(&mut st, item(id, &[]));
        }
        let order: Vec<u32> = st.items.iter().map(|i| i.id).collect();
        assert_eq!(order, vec![1, 7, 8, 4, 9, 10]);
        assert_eq!(st.tries.get(&7), Some(&1));
        assert_eq!(st.tries.get(&10), None, "past the freed places an item is new");
    }

    #[test]
    fn another_actors_item_takes_no_freed_place() {
        let mut st = PlanState {
            items: vec![item(1, &[]), item(4, &[])],
            ..PlanState::default()
        };
        st.refill.push_back((Some(4), 1, "ai".into()));
        place(&mut st, by(7, &[], "mcp:agent-b"));
        assert_eq!(st.items.last().map(|i| i.id), Some(7), "a foreign item goes last");
        assert_eq!(st.tries.get(&7), None, "and inherits no attempts");
        assert_eq!(st.refill.len(), 1, "the place stays for its own actor");
        place(&mut st, item(8, &[]));
        let order: Vec<u32> = st.items.iter().map(|i| i.id).collect();
        assert_eq!(order, vec![1, 8, 4, 7]);
        assert_eq!(st.tries.get(&8), Some(&1));
    }

    #[test]
    fn dependents_found_when_a_refilled_item_sits_before_its_base() {
        // 9 refilled the place before 1 yet depends on 5, which depends on 1
        let items = vec![item(9, &[5]), item(1, &[]), item(5, &[1]), item(6, &[])];
        assert_eq!(with_dependents(&items, &[1]), vec![9, 1, 5]);
    }

    #[test]
    fn a_stopped_or_failed_turn_starts_no_automatic_one() {
        use ddc_ai::TurnEnd;
        assert!(after_turn::<()>(&Ok(TurnEnd::Answered)));
        assert!(after_turn::<()>(&Ok(TurnEnd::LimitHit("wall"))));
        assert!(!after_turn::<()>(&Ok(TurnEnd::Cancelled)));
        assert!(!after_turn(&Err::<TurnEnd, _>("provider down")));
        assert_eq!(AUTO_MODE, ddc_ai::Mode::Propose);
    }
}
