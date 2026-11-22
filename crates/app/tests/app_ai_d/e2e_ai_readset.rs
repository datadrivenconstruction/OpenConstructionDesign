// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Widen the doors of L2": the item remembers what it was built on (the door table by filter,
//! level L2, the host walls and their type) and what it writes (the 12 doors), through a replay.

use crate::e2e_ai_common;

use ddc_app::ai::plan::fingerprint;
use e2e_ai_common::*;
use serde_json::json;
use std::collections::BTreeSet;

#[test]
fn writes_match_steps() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w1 = add_wall(&mut ed, l1, (0.0, 0.0), (30.0, 0.0));
    let l1_door = add_door(&mut ed, w1, 2.0);
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (30.0, 0.0));
    let doors: BTreeSet<i64> = (0..12)
        .map(|i| add_door(&mut ed, w2, 1.5 + 2.2 * f64::from(i)))
        .collect();
    let wtype: i64 = ed
        .model
        .sql()
        .query_row("SELECT wtype FROM wall WHERE id=?1", [w2], |r| r.get(0))
        .unwrap();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("query_elements", json!({"filter": "door level=L2"})),
            tool(
                "update_elements",
                json!({"title": "Wider doors on L2", "target": {"set": "s1"}, "set": {"width": "1000"}}),
            ),
            tool("get_plan", json!({})),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(parsed(&results[0].0)["total"], json!(12));

    let plan = plan_of(&s);
    assert_eq!(plan.items.len(), 1);
    let sets = plan.items[0].sets.clone();
    // read: the door table by filter, level L2, the host wall and its type
    assert!(sets.reads.tables.contains("opening"), "{:?}", sets.reads);
    assert!(sets.reads.covers("level", l2) && sets.reads.covers("wall", w2));
    assert!(sets.reads.covers("wall_type", wtype));
    assert!(!sets.reads.covers("wall", w1) && !sets.reads.covers("level", l1));
    // written: exactly the 12 doors of the opening table, and every row the card shows
    let written: BTreeSet<i64> = sets.writes.rows["opening"].iter().copied().collect();
    assert_eq!(written, doors);
    assert!(!sets.writes.covers("opening", l1_door));
    for (key, _, _) in fingerprint(&plan.diff) {
        assert!(sets.writes.covers(&key.0, key.1), "{key:?} changed but not written");
    }
    // the agent sees the sizes
    let shown = parsed(&results[2].0);
    let item = &shown["items"][0];
    assert_eq!(item["reads"]["tables"], json!(["opening"]), "{shown}");
    assert_eq!(item["writes"]["rows"], json!(12), "{shown}");

    // a change elsewhere in the live model: the copy is made again and the plan replayed
    add_wall(&mut ed, l1, (0.0, 5.0), (8.0, 5.0));
    assert!(s.wait_idle(&mut ed, WAIT));
    let again = plan_of(&s);
    let after = &again.items[0].sets;
    assert!(after.reads.tables.contains("opening") && after.reads.covers("level", l2));
    let written: BTreeSet<i64> = after.writes.rows["opening"].iter().copied().collect();
    assert_eq!(written, doors, "replay writes the same rows");
}

/// A live change during the plan: one elsewhere keeps every item, one inside an item's sets
/// marks that item only.
#[test]
fn live_edits_mark_only_the_items_they_touch() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let l2 = add_level(&mut ed, "L2", 3.0);
    let l5 = add_level(&mut ed, "L5", 12.0);
    let w1 = add_wall(&mut ed, l1, (0.0, 0.0), (10.0, 0.0));
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    let w5 = add_wall(&mut ed, l5, (0.0, 0.0), (10.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0].iter().map(|x| add_door(&mut ed, w2, *x)).collect();
    let (mut s, provider) = run(
        &mut ed,
        vec![
            tool("query_elements", json!({"filter": "door level=L2"})),
            tool(
                "update_elements",
                json!({"title": "Wider doors on L2", "target": {"set": "s1"}, "set": {"width": "1000"}}),
            ),
            tool(
                "update_elements",
                json!({"title": "Taller L1 wall", "target": {"refs": [format!("wall:{w1}")]}, "set": {"height": "3500"}}),
            ),
        ],
    );
    let results = tool_results(&provider);
    assert!(results.iter().all(|r| !r.1), "{results:?}");
    assert_eq!(plan_of(&s).items.len(), 2);

    // a wall moved on L5: the plan is not stale, Apply stays possible
    ed.model.set_wall_axis(w5, (1.0, 0.0), (11.0, 0.0)).unwrap();
    ed.reload().unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let p = plan_of(&s);
    assert!(p.items.iter().all(|i| i.conflict.is_none()), "{:?}", p.items);
    assert!(!p.blocked());

    // a door of item 1 deleted: item 1 conflicts and names it, item 2 stays applicable
    ed.model.delete_opening(doors[0]).unwrap();
    ed.reload().unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let p = plan_of(&s);
    let c = p.items[0].conflict.clone().unwrap_or_default();
    assert!(c.contains("plan_conflict") && c.contains(&format!("opening:{}", doors[0])), "{c}");
    assert!(p.items[1].conflict.is_none(), "{:?}", p.items[1]);
    assert!(p.blocked(), "a ticked item in conflict blocks Apply");
}

fn doors_wider() -> ddc_ai::fake::Script {
    tool(
        "update_elements",
        json!({"title": "Wider doors on L2", "target": {"filter": "door level=L2"}, "set": {"width": "1000"}}),
    )
}

/// A session whose live conflicts start the automatic replan turn; `turns` is the whole script.
fn auto_session(
    ed: &mut ddc_app::editor::Editor,
    turns: Vec<Vec<ddc_ai::fake::Script>>,
) -> (ddc_app::ai::UiSession, std::sync::Arc<ddc_ai::fake::ScriptedProvider>) {
    let provider = ddc_ai::fake::ScriptedProvider::new(turns);
    let mut c = cfg(provider.clone());
    c.auto_replan = true;
    let mut s = start(ed, c);
    turn(&mut s, ed, "go", ddc_ai::Mode::Propose);
    (s, provider)
}

fn replan_reply(p: &ddc_ai::fake::ScriptedProvider) -> Vec<(String, bool)> {
    tool_results(p)
        .into_iter()
        .filter(|r| r.0.contains("replayed") || r.0.contains("drafted again"))
        .collect()
}

#[test]
fn replan_only_conflicted() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w1 = add_wall(&mut ed, l1, (0.0, 0.0), (10.0, 0.0));
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0].iter().map(|x| add_door(&mut ed, w2, *x)).collect();
    let mut turns = script(vec![
        doors_wider(),
        tool(
            "update_elements",
            json!({"title": "Taller L1 wall", "target": {"refs": [format!("wall:{w1}")]}, "set": {"height": "3500"}}),
        ),
    ]);
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]})), doors_wider()]));
    let (mut s, provider) = auto_session(&mut ed, turns);
    assert_eq!(plan_of(&s).items.len(), 2);

    ed.model.delete_opening(doors[0]).unwrap();
    ed.reload().unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    assert_eq!(provider.remaining(), 0, "one automatic turn drafted the item again");
    let replies = replan_reply(&provider);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(!replies[0].1, "{replies:?}");
    let r = parsed(&replies[0].0);
    assert_eq!(r["replayed"], false, "the rest of the plan was not compiled again: {r}");
    assert_eq!(r["items"][0]["title"], "Wider doors on L2", "{r}");
    let c = r["items"][0]["conflict"].as_str().unwrap_or_default();
    assert!(c.contains(&format!("opening:{}", doors[0])), "{c}");

    let p = plan_of(&s);
    let ids: Vec<u32> = p.items.iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![3, 2], "the new item took the place of item 1");
    assert!(p.items.iter().all(|i| i.conflict.is_none()), "{:?}", p.items);
    assert!(!p.blocked());
    let audit = ed.model.audit_list(0, 500).unwrap();
    let replan = audit.iter().find(|e| e.2.event == "replan").expect("a replan audit row");
    let why = replan.2.text.clone().unwrap_or_default();
    assert!(why.contains(&format!("opening:{}", doors[0])), "{why}");
}

#[test]
fn replan_attempts_capped() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (20.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0, 10.0, 14.0]
        .iter()
        .map(|x| add_door(&mut ed, w2, *x))
        .collect();
    let mut turns = script(vec![doors_wider()]);
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]})), doors_wider()]));
    turns.extend(script(vec![tool("replan_items", json!({"items": [2]})), doors_wider()]));
    // left for the person's own turn: no third automatic one
    turns.extend(script(vec![tool("replan_items", json!({"items": [3]}))]));
    let (mut s, provider) = auto_session(&mut ed, turns);
    for d in &doors[..3] {
        ed.model.delete_opening(*d).unwrap();
        ed.reload().unwrap();
        assert!(s.wait_idle(&mut ed, WAIT));
    }
    assert_eq!(provider.remaining(), 2, "two automatic turns, then the person decides");
    let p = plan_of(&s);
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].id, 3);
    let c = p.items[0].conflict.clone().unwrap_or_default();
    assert!(c.contains("plan_conflict"), "{c}");
    assert!(p.blocked());

    turn(&mut s, &mut ed, "draft it again", ddc_ai::Mode::Propose);
    let replies = replan_reply(&provider);
    let last = replies.last().expect("the person's replan call");
    assert!(last.1 && last.0.contains("limit"), "{last:?}");
}

fn wall_taller(w: i64) -> ddc_ai::fake::Script {
    tool(
        "update_elements",
        json!({"title": "Taller wall", "target": {"refs": [format!("wall:{w}")]}, "set": {"height": "3500"}}),
    )
}

/// A compiled item replanned by hand: the rest is replayed; the place it left ends with the
/// turn, so a later item is new and goes last.
#[test]
fn replan_of_a_compiled_item_replays_and_its_place_ends_with_the_turn() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l1 = ed.level;
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w1 = add_wall(&mut ed, l1, (0.0, 0.0), (10.0, 0.0));
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    add_door(&mut ed, w2, 2.0);
    let mut turns = script(vec![wall_taller(w1), doors_wider()]);
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]}))]));
    turns.extend(script(vec![wall_taller(w1)]));
    let (mut s, provider) = auto_session(&mut ed, turns);

    turn(&mut s, &mut ed, "take the wall off", ddc_ai::Mode::Propose);
    let replies = replan_reply(&provider);
    assert_eq!(replies.len(), 1, "{replies:?}");
    let r = parsed(&replies[0].0);
    assert_eq!(r["replayed"], true, "item 1 had steps: the plan was replayed: {r}");
    assert!(r["items"][0]["conflict"].is_null(), "{r}");
    let p = plan_of(&s);
    assert_eq!(p.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
    assert!(p.items[0].conflict.is_none() && !p.blocked());
    assert!(p.diff.iter().all(|d| d.item == Some(2)), "nothing of item 1 is left on the copy");

    turn(&mut s, &mut ed, "the wall again", ddc_ai::Mode::Propose);
    let p = plan_of(&s);
    assert_eq!(p.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 3]);
    assert_eq!(provider.remaining(), 0);
}

/// A live edit while a turn runs: the replan turn starts once that turn ends.
#[test]
fn an_edit_during_a_turn_starts_the_replan_after_it() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0].iter().map(|x| add_door(&mut ed, w2, *x)).collect();
    let mut turns = script(vec![doors_wider()]);
    turns.extend(script(vec![
        tool("get_plan", json!({})),
        tool("get_plan", json!({})),
        tool("get_plan", json!({})),
    ]));
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]})), doors_wider()]));
    let (mut s, provider) = auto_session(&mut ed, turns);

    let view = view_of(&ed);
    s.send_turn(&ed, "look".to_string(), view, ddc_ai::Mode::Propose).unwrap();
    ed.model.delete_opening(doors[0]).unwrap();
    ed.reload().unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    assert_eq!(provider.remaining(), 0, "the replan turn ran");
    let replies = replan_reply(&provider);
    assert_eq!(replies.len(), 1, "{replies:?}");
    let p = plan_of(&s);
    assert_eq!(p.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
    assert!(p.items[0].conflict.is_none() && !p.blocked());
}

/// Holds one `stream` call until the test lets it go: the turn is surely running meanwhile.
struct Held {
    inner: std::sync::Arc<ddc_ai::fake::ScriptedProvider>,
    at: usize,
    calls: std::sync::atomic::AtomicUsize,
    reached: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl ddc_ai::Provider for Held {
    fn stream(
        &self,
        req: &ddc_ai::ChatRequest<'_>,
        sink: &mut dyn FnMut(ddc_ai::StreamEvent) -> ddc_ai::Flow,
    ) -> Result<(), ddc_ai::ProviderError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == self.at {
            let _ = self.reached.lock().unwrap().send(());
            let _ = self.go.lock().unwrap().recv_timeout(WAIT);
        }
        ddc_ai::Provider::stream(&*self.inner, req, sink)
    }
    fn list_models(&self) -> Result<Vec<String>, ddc_ai::ProviderError> {
        ddc_ai::Provider::list_models(&*self.inner)
    }
}

/// The same edit taken in between two tool calls of the turn (`drain_inbox`), whatever the
/// timing of the machine: the replan turn still starts once that turn ends.
#[test]
fn an_edit_drained_mid_turn_starts_the_replan_after_it() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0].iter().map(|x| add_door(&mut ed, w2, *x)).collect();
    let mut turns = script(vec![doors_wider()]);
    turns.extend(script(vec![tool("get_plan", json!({})), tool("get_plan", json!({}))]));
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]})), doors_wider()]));
    let provider = ddc_ai::fake::ScriptedProvider::new(turns);
    let (reached_tx, reached) = mpsc::channel();
    let (go, go_rx) = mpsc::channel();
    let mut c = cfg(provider.clone());
    // calls 0-1 are the first turn; call 3 follows the first get_plan of the second
    c.provider = std::sync::Arc::new(Held {
        inner: provider.clone(),
        at: 3,
        calls: Default::default(),
        reached: std::sync::Mutex::new(reached_tx),
        go: std::sync::Mutex::new(go_rx),
    });
    c.auto_replan = true;
    let mut s = start(&mut ed, c);
    turn(&mut s, &mut ed, "go", ddc_ai::Mode::Propose);

    let view = view_of(&ed);
    s.send_turn(&ed, "look".to_string(), view, ddc_ai::Mode::Propose).unwrap();
    reached.recv_timeout(WAIT).expect("the second turn reached its held call");
    ed.model.delete_opening(doors[0]).unwrap();
    ed.reload().unwrap();
    // past the sync gap the edit is surely sent; the held turn has not taken it yet
    let until = Instant::now() + Duration::from_millis(800);
    while Instant::now() < until {
        s.pump_core(&mut ed);
        std::thread::sleep(Duration::from_millis(2));
    }
    go.send(()).unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    assert_eq!(provider.remaining(), 0, "the replan turn ran");
    assert_eq!(replan_reply(&provider).len(), 1);
    let p = plan_of(&s);
    assert_eq!(p.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
    assert!(p.items[0].conflict.is_none() && !p.blocked());
}

/// The person's last turn was a read-only question: the replan turn still only drafts, with
/// the draft tools, and Apply stays theirs.
#[test]
fn the_automatic_turn_drafts_whatever_the_last_mode() {
    let mut ed = ddc_app::editor::Editor::new_in_memory().unwrap();
    let l2 = add_level(&mut ed, "L2", 3.0);
    let w2 = add_wall(&mut ed, l2, (0.0, 0.0), (10.0, 0.0));
    let doors: Vec<i64> = [2.0, 6.0].iter().map(|x| add_door(&mut ed, w2, *x)).collect();
    let mut turns = script(vec![doors_wider()]);
    turns.extend(script(vec![]));
    turns.extend(script(vec![tool("replan_items", json!({"items": [1]})), doors_wider()]));
    let (mut s, provider) = auto_session(&mut ed, turns);
    turn(&mut s, &mut ed, "what is in the plan?", ddc_ai::Mode::ReadOnly);
    let asked = provider.requests().len();
    let before = live_hash(&ed.model);

    ed.model.delete_opening(doors[0]).unwrap();
    ed.reload().unwrap();
    let edited = live_hash(&ed.model);
    assert_ne!(before, edited);
    assert!(s.wait_idle(&mut ed, WAIT));
    assert_eq!(provider.remaining(), 0, "the replan turn ran");
    let auto = &provider.requests()[asked];
    assert!(auto.tools.iter().any(|t| t.name == "replan_items"), "drafting tools on offer");
    assert_eq!(live_hash(&ed.model), edited, "nothing applied without the person");
    assert_eq!(plan_of(&s).items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2]);
}
