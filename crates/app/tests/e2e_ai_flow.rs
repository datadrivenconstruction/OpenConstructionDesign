//! The whole assistant workflow for each of the four capabilities, with a scripted model and no
//! network: request, draft, preview, Apply, undo; Reject; edits of the draft; a model that
//! changes meanwhile; broken or partial answers; invented elements; a very large plan; timeout,
//! cancel and no connection. Whatever happens, the live model moves only on Apply, in one undo
//! step; a refused Apply leaves model, undo stack and file as they were; every failure has its
//! words in English and Russian.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::{
    ChatRequest, FinishReason, Flow, Limits, Message, Mode, Part, Provider, ProviderError,
    StreamEvent, TurnEnd,
};
use ddc_app::ai::plan::{fingerprint, NewElem, PlanOp};
use ddc_app::ai::value_edit::{Field, Num, Value};
use ddc_app::query::Val;
use ddc_app::ai::ui::logic::{self, ErrKind};
use ddc_app::ai::ui::steps;
use ddc_app::ai::validate::{IssueLevel, LARGE_PLAN};
use ddc_app::ai::{AiError, SessionCfg, UiEvent, UiSession, ViewCtx};
use ddc_app::editor::{Editor, OpFn, Sel};
use ddc_model::checks::{CheckRule, CheckScope, Finding, Fix, Severity};
use ddc_model::{Error, Model, PVal, WallSpec};
use e2e_ai_common::*;
use e2e_common::{count, dump_except, hash_of, ids, temp_file};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- the four capabilities

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cap {
    /// Bulk edits by words.
    Bulk,
    /// Creation from a description.
    Create,
    /// Model checks with fixes.
    Check,
    /// Questions and data: never a plan.
    Data,
}

const WRITES: [Cap; 3] = [Cap::Bulk, Cap::Create, Cap::Check];

/// Doors narrower than 0.8 m; the fix removes them.
struct NarrowDoors;

impl CheckRule for NarrowDoors {
    fn id(&self) -> &'static str {
        "test.narrow_door"
    }
    fn run(&self, m: &Model, scope: &CheckScope, out: &mut Vec<Finding>) -> Result<(), Error> {
        let mut st = m
            .sql()
            .prepare("SELECT id FROM opening WHERE kind='door' AND width<0.8 ORDER BY id")?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        for id in rows {
            let key = ("opening".to_string(), id?);
            if let CheckScope::Elements(list) = scope {
                if !list.contains(&key) {
                    continue;
                }
            }
            out.push(Finding {
                rule: "test.narrow_door",
                severity: Severity::Error,
                elements: vec![key.clone()],
                at: None,
                measure: None,
                msg_key: "test.narrow",
                args: vec![],
                fix: Fix::Delete(vec![key]),
            });
        }
        Ok(())
    }
}

static NARROW: NarrowDoors = NarrowDoors;

fn tools(cap: Cap) -> Vec<Script> {
    match cap {
        Cap::Bulk => vec![
            tool(
                "add_param",
                json!({"title": "Param", "name": "Rating", "ptype": "text", "cats": ["wall"]}),
            ),
            tool(
                "update_elements",
                json!({"title": "Set", "target": {"filter": "wall"}, "set": {"Rating": "A"}}),
            ),
        ],
        Cap::Create => vec![
            tool(
                "create_elements",
                json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, 5], "b": [6, 5]}]}),
            ),
            tool(
                "create_elements",
                json!({"title": "Door", "cat": "door",
                    "items": [{"wall": "wall:n1", "along": 1.0}]}),
            ),
        ],
        Cap::Check => vec![
            tool("run_checks", json!({"rules": ["test.narrow_door"]})),
            tool("apply_fix", json!({"findings": ["f1"]})),
        ],
        Cap::Data => vec![tool("get_context", json!({}))],
    }
}

fn mode(cap: Cap) -> Mode {
    if cap == Cap::Data {
        Mode::ReadOnly
    } else {
        Mode::Propose
    }
}

/// A wall with two doors, one of them too narrow; in memory or in a project file.
fn fixture(in_file: bool) -> (Editor, Option<PathBuf>) {
    let mut ed = Editor::new_in_memory().unwrap();
    let mut path = None;
    if in_file {
        let p = temp_file("ai_flow.db");
        ed.model.snapshot_to(&p).unwrap();
        let units = ed.tool.units;
        ed.replace_model(Model::open(&p.to_string_lossy()).unwrap())
            .unwrap();
        ed.set_units(units);
        let level = ed.levels()[0].0;
        ed.set_level(level);
        path = Some(p);
    }
    let level = ed.level;
    let wall = add_wall(&mut ed, level, (0.0, 0.0), (10.0, 0.0));
    let narrow = add_door(&mut ed, wall, 2.0);
    add_door(&mut ed, wall, 6.0);
    let spec = ddc_model::OpeningSpec {
        along: 2.0,
        width: 0.6,
        sill: 0.0,
        height: 2.1,
        kind: "door".to_string(),
    };
    ed.model.update_opening(narrow, &spec).unwrap();
    ed.reload().unwrap();
    (ed, path)
}

fn cfg_for(cap: Cap, provider: Arc<ScriptedProvider>) -> SessionCfg {
    let c = cfg(provider);
    if cap == Cap::Check {
        with_rules(c, vec![&NARROW])
    } else {
        c
    }
}

fn session(
    ed: &mut Editor,
    cap: Cap,
    turns: Vec<Vec<Script>>,
) -> (UiSession, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(turns);
    let s = start(ed, cfg_for(cap, provider.clone()));
    (s, provider)
}

/// One request of the capability: the model calls its tools, then closes with a text.
fn ask(ed: &mut Editor, cap: Cap) -> (UiSession, Arc<ScriptedProvider>) {
    let (mut s, p) = session(ed, cap, script(tools(cap)));
    turn(&mut s, ed, "go", mode(cap));
    (s, p)
}

/// What Apply of the capability's plan must have done to the live model.
fn assert_applied(cap: Cap, ed: &Editor) {
    match cap {
        Cap::Bulk => assert_eq!(count(&ed.model, "wall"), 1),
        Cap::Create => {
            assert_eq!(count(&ed.model, "wall"), 2);
            assert_eq!(count(&ed.model, "opening"), 3);
        }
        Cap::Check => assert_eq!(count(&ed.model, "opening"), 1),
        Cap::Data => unreachable!("a question has no plan"),
    }
}

// ---------------------------------------------------------------- invariants

/// Everything that must stay as it was until Apply, and after a refused Apply.
#[derive(Debug, PartialEq)]
struct Live {
    model: u64,
    head: u64,
    ops: i64,
    top: i64,
    undo: bool,
    redo: bool,
    /// The project file read back on its own connection, assistant records left out.
    file: Option<u64>,
}

fn live(ed: &Editor, file: Option<&Path>) -> Live {
    Live {
        model: live_hash(&ed.model),
        head: ed.model.change_head(),
        ops: count(&ed.model, "op"),
        top: ed.model.op_top().unwrap(),
        undo: ed.can_undo(),
        redo: ed.can_redo(),
        file: file.map(|p| {
            let m = Model::open_copy(&p.to_string_lossy()).unwrap();
            hash_of(&dump_except(&m, NOT_MODEL))
        }),
    }
}

fn cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
}

/// A text the person reads: present in both catalogs, and the Russian one is Russian.
fn said(key: &str) {
    let en = ddc_ui::Catalog::new(ddc_ui::Lang::En).t(key);
    let ru = ddc_ui::Catalog::new(ddc_ui::Lang::Ru).t(key);
    assert_ne!(en, key, "no English text for {key}");
    assert_ne!(ru, key, "no Russian text for {key}");
    assert!(cyrillic(&ru), "{key}: the Russian text is not Russian: {ru:?}");
    assert!(!cyrillic(&en), "{key}: the English text is not English: {en:?}");
}

/// A failure as the panel titles it, with its words in both languages.
fn seen(e: &AiError) -> ErrKind {
    let k = logic::classify(e);
    said(logic::err_key(k));
    k
}

/// A failed tool step as the step list explains it, in both languages.
fn step_said(err: Option<&str>) -> steps::Why {
    let w = steps::why(err.unwrap_or(""));
    let en = steps::why_text(&ddc_ui::Catalog::new(ddc_ui::Lang::En), w);
    let ru = steps::why_text(&ddc_ui::Catalog::new(ddc_ui::Lang::Ru), w);
    assert!(cyrillic(&ru) && !cyrillic(&en), "{w:?}: en {en:?}, ru {ru:?}");
    w
}

/// Every failure, limit and failed tool step of the events, each with words in en and ru.
fn signals(events: &[UiEvent]) -> Vec<ErrKind> {
    let mut out = vec![];
    for e in events {
        match e {
            UiEvent::Failed(err) => out.push(seen(err)),
            UiEvent::TurnDone(end) => {
                if let Some(what) = logic::turn_end_limit(end) {
                    said(logic::err_key(ErrKind::TurnLimit));
                    said(logic::turn_limit_key(what));
                    out.push(ErrKind::TurnLimit);
                }
            }
            UiEvent::Tool {
                ok: Some(false),
                err,
                ..
            } => {
                step_said(err.as_deref());
            }
            _ => {}
        }
    }
    out
}

fn failed_steps(events: &[UiEvent]) -> Vec<Option<String>> {
    events
        .iter()
        .filter_map(|e| match e {
            UiEvent::Tool {
                ok: Some(false),
                err,
                ..
            } => Some(err.clone()),
            _ => None,
        })
        .collect()
}

/// A refused Apply: the error is the expected one, it is seen, and nothing moved.
fn refused(s: &mut UiSession, ed: &mut Editor, file: Option<&Path>, want: AiError) {
    let before = live(ed, file);
    assert_eq!(s.apply(ed), Err(want.clone()));
    seen(&want);
    assert_eq!(live(ed, file), before, "a refused Apply ({want:?}) moved something");
}

fn edit_live(ed: &mut Editor, y: f64) {
    let spec = WallSpec {
        a: (20.0, y),
        b: (25.0, y),
        level: ed.level,
        base_offset: 0.0,
        height: 3.0,
        wall_type: ed.types[0].id,
        offset0: None,
    };
    ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        m.add_walls(std::slice::from_ref(&spec)).map(|_| ())
    })))
    .unwrap();
}

// ---------------------------------------------------------------- providers beyond the script

/// Plays the script first, then hangs on every later call until the turn is stopped: a slow
/// server, a timeout, a cancel.
struct ThenHang {
    first: Arc<ScriptedProvider>,
    scripted: usize,
    calls: AtomicUsize,
    entered: Mutex<mpsc::Sender<()>>,
}

impl Provider for ThenHang {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.scripted {
            return self.first.stream(req, sink);
        }
        let _ = self.entered.lock().unwrap().send(());
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            if matches!(sink(StreamEvent::TextDelta(String::new())), Flow::Stop) {
                return Err(ProviderError::Cancelled);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}

fn then_hang(tools: Vec<Script>) -> (Arc<ThenHang>, mpsc::Receiver<()>) {
    let turns = if tools.is_empty() { vec![] } else { script(tools) };
    let scripted = turns.len();
    let (tx, rx) = mpsc::channel();
    let p = Arc::new(ThenHang {
        first: ScriptedProvider::new(turns),
        scripted,
        calls: AtomicUsize::new(0),
        entered: Mutex::new(tx),
    });
    (p, rx)
}

/// Raw stream events, as a server that breaks off or sends broken JSON would.
struct Raw {
    turns: Mutex<VecDeque<(Vec<StreamEvent>, Option<ProviderError>)>>,
    last: Mutex<Vec<Message>>,
}

impl Provider for Raw {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        *self.last.lock().unwrap() = req.messages.to_vec();
        let Some((events, end)) = self.turns.lock().unwrap().pop_front() else {
            return Err(ProviderError::Protocol("script exhausted".to_string()));
        };
        for e in events {
            if matches!(sink(e), Flow::Stop) {
                return Err(ProviderError::Cancelled);
            }
        }
        match end {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec![])
    }
}

fn raw(turns: Vec<(Vec<StreamEvent>, Option<ProviderError>)>) -> Arc<Raw> {
    Arc::new(Raw {
        turns: Mutex::new(turns.into()),
        last: Mutex::new(vec![]),
    })
}

fn call(id: &str, name: &str, args: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolStart {
            id: id.to_string(),
            name: name.to_string(),
        },
        StreamEvent::ToolArgsDelta {
            id: id.to_string(),
            json: args.to_string(),
        },
        StreamEvent::ToolEnd { id: id.to_string() },
    ]
}

fn raw_errors(p: &Raw) -> Vec<String> {
    p.last
        .lock()
        .unwrap()
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|x| match x {
            Part::ToolResult {
                content,
                is_error: true,
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn with_provider(cap: Cap, p: Arc<dyn Provider>, limits: Limits) -> SessionCfg {
    let mut c = cfg_for(cap, ScriptedProvider::new(vec![]));
    c.provider = p;
    c.limits = limits;
    c
}

fn view_with(ed: &Editor, selection: Vec<Sel>) -> ViewCtx {
    let mut v = view_of(ed);
    v.selection = selection;
    v
}

// ---------------------------------------------------------------- request, draft, Apply, undo

#[test]
fn each_capability_drafts_previews_and_applies_in_one_undo_step() {
    for cap in WRITES {
        let (mut ed, _) = fixture(false);
        let before = live(&ed, None);
        let (mut s, provider) = ask(&mut ed, cap);
        let results = tool_results(&provider);
        assert!(results.iter().all(|r| !r.1), "{cap:?}: {results:?}");
        assert_eq!(live(&ed, None), before, "{cap:?}: the draft reached the live model");
        let plan = plan_of(&s);
        assert!(!plan.items.is_empty(), "{cap:?}: no items on the card");
        assert!(!plan.diff.is_empty(), "{cap:?}: the preview shows no change");
        assert!(s.can_apply(), "{cap:?}: {:?}", plan.issues);
        assert!(signals(&s.drain_events()).is_empty(), "{cap:?}: a clean run reports a failure");
        apply_and_check(&mut s, &mut ed);
        assert_applied(cap, &ed);
        // the card is spent: a second Apply is refused and writes nothing
        let after = live(&ed, None);
        let again = s.apply(&mut ed);
        assert!(again.is_err(), "{cap:?}: the same card applied twice");
        seen(&again.unwrap_err());
        assert_eq!(live(&ed, None), after, "{cap:?}");
    }
}

#[test]
fn a_question_answers_and_never_offers_an_apply() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let (mut s, provider) = ask(&mut ed, Cap::Data);
    assert!(tool_results(&provider).iter().all(|r| !r.1));
    assert!(s.plan().is_none());
    assert!(!s.can_apply());
    refused(&mut s, &mut ed, None, AiError::EmptyPlan);
    assert_eq!(live(&ed, None), before);
}

#[test]
fn the_selection_reaches_the_model_and_only_it_is_changed() {
    let (mut ed, _) = fixture(false);
    let level = ed.level;
    let first = ids(&ed.model, "wall")[0];
    let picked = add_wall(&mut ed, level, (0.0, 4.0), (6.0, 4.0));
    let provider = ScriptedProvider::new(script(vec![tool(
        "delete_elements",
        json!({"title": "Delete the selected", "target": {"selection": true}}),
    )]));
    let mut s = start(&mut ed, cfg(provider.clone()));
    let view = view_with(&ed, vec![Sel::Wall(picked)]);
    s.send_turn(&ed, "delete this".to_string(), view, Mode::Propose)
        .unwrap();
    assert!(s.wait_idle(&mut ed, WAIT));
    let reqs = provider.requests();
    let note = reqs[0]
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .any(|p| matches!(p, Part::Text(t) if t.contains("selected=1")));
    assert!(note, "the model was not told what is selected");
    let plan = plan_of(&s);
    assert_eq!(plan.diff.len(), 1, "{:?}", plan.diff);
    assert_eq!(plan.diff[0].key.1, picked);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(ids(&ed.model, "wall"), vec![first]);
}

// ---------------------------------------------------------------- Reject

#[test]
fn reject_drops_the_draft_and_it_never_comes_back() {
    for cap in WRITES {
        let (mut ed, _) = fixture(false);
        let before = live(&ed, None);
        // new-element slots keep counting after a Reject, so the second wall gets no door
        let mut again = tools(cap);
        if cap == Cap::Create {
            again.truncate(1);
        }
        let want = if cap == Cap::Bulk { 2 } else { 1 };
        let mut turns = script(tools(cap));
        turns.extend(script(again));
        let (mut s, _p) = session(&mut ed, cap, turns);
        turn(&mut s, &mut ed, "go", mode(cap));
        s.reject();
        assert!(s.wait_idle(&mut ed, WAIT));
        assert!(s.plan().is_none(), "{cap:?}: the card stayed after Reject");
        refused(&mut s, &mut ed, None, AiError::EmptyPlan);
        assert_eq!(live(&ed, None), before, "{cap:?}");
        // the next request starts from nothing: the rejected items do not return with it
        turn(&mut s, &mut ed, "again", mode(cap));
        assert_eq!(plan_of(&s).items.len(), want, "{cap:?}: rejected items came back");
        apply_and_check(&mut s, &mut ed);
        if cap == Cap::Create {
            assert_eq!(count(&ed.model, "wall"), 2);
            assert_eq!(count(&ed.model, "opening"), 2, "the rejected door was built");
        } else {
            assert_applied(cap, &ed);
        }
    }
}

// ---------------------------------------------------------------- edits of the draft

#[test]
fn an_unticked_door_stays_out_and_the_wall_still_goes_in() {
    let (mut ed, _) = fixture(false);
    let (mut s, _p) = ask(&mut ed, Cap::Create);
    let plan = plan_of(&s);
    let door = plan.items[1].id;
    assert_eq!(plan.items[1].depends_on, vec![plan.items[0].id]);
    s.set_checked(vec![(door, false)]);
    assert!(s.wait_idle(&mut ed, WAIT));
    assert!(!plan_of(&s).items[1].checked);
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), 2);
    assert_eq!(count(&ed.model, "opening"), 2, "the unticked door was built");
}

#[test]
fn a_follow_up_request_edits_the_draft_and_apply_takes_the_edited_one() {
    let (mut ed, _) = fixture(false);
    let mut turns = script(tools(Cap::Bulk));
    turns.extend(script(vec![tool("drop_item", json!({"item": 2}))]));
    let (mut s, _p) = session(&mut ed, Cap::Bulk, turns);
    turn(&mut s, &mut ed, "add a rating and set it", Mode::Propose);
    let full = plan_of(&s).diff.len();
    let before = live(&ed, None);
    turn(&mut s, &mut ed, "keep only the parameter", Mode::Propose);
    let plan = plan_of(&s);
    assert_eq!(plan.items.len(), 1, "{:?}", plan.items);
    assert!(plan.diff.len() <= full);
    assert_eq!(live(&ed, None), before, "editing the draft touched the live model");
    assert!(s.redraft().is_some(), "the revision is marked");
    assert!(!s.can_apply(), "a revision nobody looked at applies");
    apply_and_check(&mut s, &mut ed);
}

// ---------------------------------------------------------------- the model changes meanwhile

#[test]
fn a_live_edit_refuses_the_old_draft_then_rebases_it() {
    for cap in WRITES {
        let (mut ed, file) = fixture(true);
        let file = file.as_deref();
        let (mut s, _p) = ask(&mut ed, cap);
        let shown = s.plan_rev();
        edit_live(&mut ed, 30.0);
        refused(&mut s, &mut ed, file, AiError::Stale);
        assert!(s.wait_idle(&mut ed, WAIT));
        let events = s.drain_events();
        assert!(
            events.iter().any(|e| matches!(e, UiEvent::Replanned)),
            "{cap:?}: the draft was not rebased"
        );
        // the card the person looked at is gone: Apply of that revision is refused too
        if s.plan_rev() != shown {
            let before = live(&ed, file);
            assert_eq!(s.apply_rev(&mut ed, shown), Err(AiError::ReviewNeeded), "{cap:?}");
            seen(&AiError::ReviewNeeded);
            assert_eq!(live(&ed, file), before, "{cap:?}");
        }
        apply_and_check(&mut s, &mut ed);
        match cap {
            Cap::Create => assert_eq!(count(&ed.model, "wall"), 3),
            Cap::Check => assert_eq!(count(&ed.model, "opening"), 1),
            _ => assert_eq!(count(&ed.model, "wall"), 2),
        }
    }
}

// ---------------------------------------------------------------- values corrected on the card

fn wall_b(op: &PlanOp) -> (f64, f64) {
    match op {
        PlanOp::Create { items, .. } => match &items[0] {
            NewElem::Wall { b, .. } => *b,
            other => panic!("not a wall: {other:?}"),
        },
        other => panic!("not a create: {other:?}"),
    }
}

fn lit(op: &PlanOp) -> String {
    match op {
        PlanOp::Assign { set, .. } => match &set[0].val {
            Val::Lit(t) => t.clone(),
            other => panic!("not a literal: {other:?}"),
        },
        other => panic!("not an assign: {other:?}"),
    }
}

/// The person's correction of one value: it reaches the draft only, as a new revision.
fn correct(s: &mut UiSession, ed: &mut Editor, item: u32, field: Field, value: Value) {
    let (rev, before) = (s.plan_rev(), live(ed, None));
    s.set_value(item, field, value);
    assert!(s.wait_idle(ed, WAIT));
    assert!(s.plan_rev() > rev, "the corrected card is a new revision");
    assert_eq!(live(ed, None), before, "a corrected value reached the live model");
}

#[test]
fn a_corrected_length_is_drafted_then_applied_in_one_undo_step() {
    let (mut ed, _) = fixture(false);
    let (mut s, _p) = ask(&mut ed, Cap::Create);
    let wall = plan_of(&s).items[0].id;
    let length = Field::Create {
        index: 0,
        num: Num::Length,
    };
    correct(&mut s, &mut ed, wall, length, Value::Metres(5.5));
    let plan = plan_of(&s);
    let b = wall_b(&plan.items[0].op);
    assert!((b.0 - 5.5).abs() < 1e-9 && (b.1 - 5.0).abs() < 1e-9, "{b:?}");
    assert!(s.can_apply(), "{:?}", plan.issues);
    let events = s.drain_events();
    assert!(signals(&events).is_empty(), "a valid value reported a failure");
    assert!(!events.iter().any(|e| matches!(e, UiEvent::ValueRefused { .. })));
    apply_and_check(&mut s, &mut ed);
    let longest: f64 = ed
        .model
        .sql()
        .query_row("SELECT MAX(bx - ax) FROM wall WHERE ay = 5.0", [], |r| r.get(0))
        .unwrap();
    assert!((longest - 5.5).abs() < 1e-9, "the built wall is {longest} m");
    said("ai.value.was");
    said("ai.value.tip");
}

#[test]
fn a_corrected_word_is_drafted_and_applied() {
    let (mut ed, _) = fixture(false);
    let (mut s, _p) = ask(&mut ed, Cap::Bulk);
    let set = plan_of(&s).items[1].id;
    let rating = Field::Assign("Rating".to_string());
    correct(&mut s, &mut ed, set, rating, Value::Text("B".to_string()));
    assert_eq!(lit(&plan_of(&s).items[1].op), "B");
    apply_and_check(&mut s, &mut ed);
    let def = ed.model.param_by_name("Rating").unwrap().expect("the parameter");
    let vals = ed.model.param_values(def.id).unwrap();
    assert!(!vals.is_empty());
    assert!(vals.values().all(|v| *v == PVal::Text("B".to_string())), "{vals:?}");
}

#[test]
fn a_value_the_plan_cannot_take_is_refused_and_the_draft_keeps_the_old_one() {
    let (mut ed, _) = fixture(false);
    let (mut s, _p) = ask(&mut ed, Cap::Create);
    let wall = plan_of(&s).items[0].id;
    let was = wall_b(&plan_of(&s).items[0].op);
    let _ = s.drain_events();
    let before = live(&ed, None);
    let length = Field::Create {
        index: 0,
        num: Num::Length,
    };
    // past the card's own check: a wall of no length does not compile
    s.set_value(wall, length, Value::Metres(0.0));
    assert!(s.wait_idle(&mut ed, WAIT));
    let events = s.drain_events();
    let why = events.iter().find_map(|e| match e {
        UiEvent::ValueRefused { item, why } if *item == wall => Some(why.clone()),
        _ => None,
    });
    assert!(why.is_some_and(|w| !w.is_empty()), "no refusal: {}", events.len());
    said("ai.value.refused");
    said("ai.value.bad");
    let plan = plan_of(&s);
    assert_eq!(wall_b(&plan.items[0].op), was, "the refused value stayed in the draft");
    assert!(plan.items[0].conflict.is_none(), "{:?}", plan.items[0].conflict);
    assert_eq!(live(&ed, None), before, "a refused value moved something");
    // the card is still whole and applies as it was drafted
    assert!(s.can_apply(), "{:?}", plan.issues);
    apply_and_check(&mut s, &mut ed);
    assert_applied(Cap::Create, &ed);
}

#[test]
fn a_rebased_card_marks_the_rows_the_live_edit_changed() {
    let (mut ed, file) = fixture(true);
    let file = file.as_deref();
    let (mut s, _p) = ask(&mut ed, Cap::Bulk);
    assert!(s.rebased().is_none(), "a first draft is not a rebase");
    let (param, set) = (plan_of(&s).items[0].id, plan_of(&s).items[1].id);
    edit_live(&mut ed, 30.0);
    refused(&mut s, &mut ed, file, AiError::Stale);
    assert!(s.wait_idle(&mut ed, WAIT));
    let d = s.rebased().expect("the rebase is told row by row").clone();
    assert!(d.marks(set), "the Set row now reaches the new wall: {d:?}");
    assert!(!d.marks(param), "the parameter did not change: {d:?}");
    said("ai.plan.rebased");
    said("ai.plan.apply_updated");
    said("ai.plan.keys_anywhere");
    let before = live(&ed, file);
    apply_and_check(&mut s, &mut ed);
    assert_ne!(live(&ed, file), before);
    assert_eq!(count(&ed.model, "wall"), 2);
    // the next request starts unmarked
    assert!(s.rebased().is_none(), "the marks outlived Apply");
}

// ---------------------------------------------------------------- broken and partial answers

#[test]
fn broken_tool_arguments_become_a_seen_step_error_and_no_plan() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let mut first = call(
        "c1",
        "create_elements",
        r#"{"title": "W", "cat": "wall", "items": [{"a": [0, 5"#,
    );
    first.push(StreamEvent::Finish(FinishReason::ToolCalls));
    let p = raw(vec![
        (first, None),
        (
            vec![
                StreamEvent::TextDelta("sorry".to_string()),
                StreamEvent::Finish(FinishReason::Stop),
            ],
            None,
        ),
    ]);
    let mut s = start(&mut ed, with_provider(Cap::Create, p.clone(), Limits::default()));
    turn(&mut s, &mut ed, "a wall", Mode::Propose);
    assert!(!raw_errors(&p).is_empty(), "the broken call was not answered with an error");
    assert!(s.plan().is_none());
    let events = s.drain_events();
    signals(&events);
    assert!(!failed_steps(&events).is_empty(), "the failed step is not on the panel");
    assert_eq!(live(&ed, None), before);
}

#[test]
fn a_stream_cut_mid_call_fails_visibly_and_keeps_the_earlier_draft() {
    let (mut ed, _) = fixture(false);
    let mut good = call(
        "c1",
        "create_elements",
        r#"{"title": "Wall", "cat": "wall", "items": [{"a": [0, 5], "b": [6, 5]}]}"#,
    );
    good.push(StreamEvent::Finish(FinishReason::ToolCalls));
    let mut cut = vec![StreamEvent::TextDelta("Adding a door".to_string())];
    cut.extend(call("c2", "create_elements", r#"{"title": "Door", "cat": "do"#));
    cut.pop();
    let p = raw(vec![
        (good, None),
        (
            vec![
                StreamEvent::TextDelta("done".to_string()),
                StreamEvent::Finish(FinishReason::Stop),
            ],
            None,
        ),
        (
            cut,
            Some(ProviderError::Protocol("stream ended early".to_string())),
        ),
    ]);
    let mut s = start(&mut ed, with_provider(Cap::Create, p, Limits::default()));
    turn(&mut s, &mut ed, "a wall", Mode::Propose);
    let kept = fingerprint(&plan_of(&s).diff);
    s.drain_events();
    let before = live(&ed, None);
    turn(&mut s, &mut ed, "and a door", Mode::Propose);
    let kinds = signals(&s.drain_events());
    assert!(kinds.contains(&ErrKind::Protocol), "{kinds:?}");
    assert!(logic::can_retry(ErrKind::Protocol), "a broken stream offers a retry");
    assert_eq!(live(&ed, None), before);
    assert_eq!(fingerprint(&plan_of(&s).diff), kept, "the cut call changed the draft");
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), 2);
}

#[test]
fn an_answer_cut_by_the_output_limit_says_so_and_changes_nothing() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let mut first = call("c1", "create_elements", r#"{"title": "W", "cat": "wa"#);
    first.push(StreamEvent::Finish(FinishReason::Length));
    let p = raw(vec![
        (first, None),
        (
            vec![
                StreamEvent::TextDelta("and then".to_string()),
                StreamEvent::Finish(FinishReason::Length),
            ],
            None,
        ),
    ]);
    let mut s = start(&mut ed, with_provider(Cap::Create, p, Limits::default()));
    turn(&mut s, &mut ed, "many walls", Mode::Propose);
    let events = s.drain_events();
    let kinds = signals(&events);
    assert!(!kinds.is_empty(), "a cut answer ended silently: {events:?}");
    assert!(s.plan().is_none());
    assert_eq!(live(&ed, None), before);
}

#[test]
fn wrong_types_and_unknown_tools_are_refused_one_by_one() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let (mut s, provider) = session(
        &mut ed,
        Cap::Create,
        script(vec![
            tool(
                "create_elements",
                json!({"title": "W", "cat": "wall", "items": "lots"}),
            ),
            tool("paint_it_gold", json!({})),
            tool(
                "update_elements",
                json!({"title": "Set", "target": {"filter": "wall"}, "set": 7}),
            ),
        ]),
    );
    turn(&mut s, &mut ed, "go", Mode::Propose);
    let results = tool_results(&provider);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(results.iter().all(|r| r.1), "{results:?}");
    assert!(s.plan().is_none());
    signals(&s.drain_events());
    refused(&mut s, &mut ed, None, AiError::EmptyPlan);
    assert_eq!(live(&ed, None), before);
}

// ---------------------------------------------------------------- invented elements

#[test]
fn a_tool_naming_a_missing_element_is_an_error_for_every_capability() {
    let missing = [
        (
            Cap::Bulk,
            tool(
                "delete_elements",
                json!({"title": "Delete", "target": {"refs": ["wall:999999"]}}),
            ),
        ),
        (
            Cap::Create,
            tool(
                "create_elements",
                json!({"title": "Door", "cat": "door",
                    "items": [{"wall": "wall:999999", "along": 1.0}]}),
            ),
        ),
        (
            Cap::Create,
            tool(
                "create_elements",
                json!({"title": "Door", "cat": "door",
                    "items": [{"wall": "wall:n9", "along": 1.0}]}),
            ),
        ),
        (Cap::Check, tool("apply_fix", json!({"findings": ["f9"]}))),
    ];
    for (cap, call) in missing {
        let (mut ed, _) = fixture(false);
        let before = live(&ed, None);
        let (mut s, provider) = session(&mut ed, cap, script(vec![call]));
        turn(&mut s, &mut ed, "go", Mode::Propose);
        let results = tool_results(&provider);
        assert!(results[0].1, "{cap:?}: {results:?}");
        assert!(s.plan().is_none(), "{cap:?}: an invented element made a plan");
        signals(&s.drain_events());
        refused(&mut s, &mut ed, None, AiError::EmptyPlan);
        assert_eq!(live(&ed, None), before, "{cap:?}");
    }
}

#[test]
fn a_missing_element_reads_as_not_found_on_the_step() {
    let (mut ed, _) = fixture(false);
    let (mut s, _p) = session(
        &mut ed,
        Cap::Bulk,
        script(vec![tool(
            "delete_elements",
            json!({"title": "Delete", "target": {"refs": ["wall:999999"]}}),
        )]),
    );
    turn(&mut s, &mut ed, "go", Mode::Propose);
    let failed = failed_steps(&s.drain_events());
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(step_said(failed[0].as_deref()), steps::Why::NotFound, "{failed:?}");
}

#[test]
fn an_invented_element_in_a_later_request_leaves_the_draft_alone() {
    let (mut ed, _) = fixture(false);
    let mut turns = script(tools(Cap::Bulk));
    turns.extend(script(vec![tool(
        "delete_elements",
        json!({"title": "Delete", "target": {"refs": ["wall:999999"]}}),
    )]));
    let (mut s, _p) = session(&mut ed, Cap::Bulk, turns);
    turn(&mut s, &mut ed, "rate the walls", Mode::Propose);
    let kept = fingerprint(&plan_of(&s).diff);
    let items = plan_of(&s).items.len();
    turn(&mut s, &mut ed, "and delete that one", Mode::Propose);
    assert_eq!(plan_of(&s).items.len(), items);
    assert_eq!(fingerprint(&plan_of(&s).diff), kept);
    apply_and_check(&mut s, &mut ed);
}

// ---------------------------------------------------------------- a very large plan

#[test]
fn a_very_large_plan_waits_for_permission_and_then_applies_in_one_step() {
    let (mut ed, file) = fixture(true);
    let file = file.as_deref();
    let level = ed.level;
    let specs: Vec<WallSpec> = (0..LARGE_PLAN + 20)
        .map(|i| WallSpec {
            a: (0.0, 10.0 + i as f64),
            b: (4.0, 10.0 + i as f64),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: ed.types[0].id,
            offset0: None,
        })
        .collect();
    ed.model.add_walls(&specs).unwrap();
    ed.reload().unwrap();
    let (mut s, _p) = ask(&mut ed, Cap::Bulk);
    let plan = plan_of(&s);
    assert!(plan.diff.len() > LARGE_PLAN, "{}", plan.diff.len());
    let blocks: Vec<&str> = plan
        .issues
        .iter()
        .filter(|i| i.level == IssueLevel::Block)
        .map(|i| i.text_key)
        .collect();
    assert!(blocks.contains(&"ai.issue.too_large"), "{blocks:?}");
    for k in &blocks {
        said(k);
    }
    assert!(!s.can_apply());
    refused(&mut s, &mut ed, file, AiError::Blocked);
    apply_and_check(&mut s, &mut ed);
}

#[test]
fn more_items_than_one_call_may_create_are_refused_whole() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let items: Vec<_> = (0..ddc_ai::catalog::MAX_CREATE_ITEMS + 1)
        .map(|i| json!({"a": [0, 20 + i], "b": [3, 20 + i]}))
        .collect();
    let (mut s, provider) = session(
        &mut ed,
        Cap::Create,
        script(vec![tool(
            "create_elements",
            json!({"title": "Many", "cat": "wall", "items": items}),
        )]),
    );
    turn(&mut s, &mut ed, "lots of walls", Mode::Propose);
    assert!(tool_results(&provider)[0].1);
    assert!(s.plan().is_none(), "a part of the batch made it to the card");
    assert_eq!(live(&ed, None), before);
}

// ---------------------------------------------------------------- timeout, cancel, connection

#[test]
fn a_model_that_never_answers_hits_the_time_limit_and_says_so() {
    let (mut ed, _) = fixture(false);
    let before = live(&ed, None);
    let (p, _entered) = then_hang(vec![]);
    let limits = Limits {
        max_wall_s: 1,
        ..Limits::default()
    };
    let mut s = start(&mut ed, with_provider(Cap::Bulk, p, limits));
    let t = Instant::now();
    turn(&mut s, &mut ed, "go", Mode::Propose);
    assert!(t.elapsed() < Duration::from_secs(30), "the limit did not stop the turn");
    let events = s.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::TurnDone(TurnEnd::LimitHit("wall")))),
        "{events:?}"
    );
    assert!(signals(&events).contains(&ErrKind::TurnLimit));
    assert_eq!(logic::turn_limit_key("wall"), "ai.err.limit_time");
    assert!(s.plan().is_none());
    assert_eq!(live(&ed, None), before);
}

#[test]
fn cancel_while_the_model_thinks_keeps_the_draft_and_apply_waits_for_the_end() {
    for cap in WRITES {
        let (mut ed, _) = fixture(false);
        let (p, entered) = then_hang(tools(cap));
        let mut s = start(&mut ed, with_provider(cap, p, Limits::default()));
        turn(&mut s, &mut ed, "go", mode(cap));
        let kept = fingerprint(&plan_of(&s).diff);
        s.drain_events();
        let view = view_of(&ed);
        s.send_turn(&ed, "more".to_string(), view, Mode::Propose)
            .unwrap();
        entered.recv_timeout(WAIT).expect("the second request reached the model");
        assert!(!s.can_apply(), "{cap:?}: Apply is offered while the model works");
        refused(&mut s, &mut ed, None, AiError::Busy);
        s.cancel();
        assert!(s.wait_idle(&mut ed, WAIT), "{cap:?}: cancel did not end the turn");
        let events = s.drain_events();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::TurnDone(TurnEnd::Cancelled))),
            "{cap:?}: {events:?}"
        );
        assert!(signals(&events).is_empty(), "{cap:?}: a cancel is not a failure");
        assert_eq!(fingerprint(&plan_of(&s).diff), kept, "{cap:?}: cancel changed the draft");
        apply_and_check(&mut s, &mut ed);
        assert_applied(cap, &ed);
    }
}

fn err_turns(e: ProviderError, n: usize) -> Vec<Vec<Script>> {
    (0..n).map(|_| vec![Script::Err(e.clone())]).collect()
}

#[test]
fn no_connection_is_retried_then_seen_and_the_draft_survives() {
    let (mut ed, _) = fixture(false);
    let mut turns = script(tools(Cap::Bulk));
    // the first try and both automatic retries
    turns.extend(err_turns(ProviderError::Network("connection refused".to_string()), 3));
    let (mut s, provider) = session(&mut ed, Cap::Bulk, turns);
    turn(&mut s, &mut ed, "rate the walls", Mode::Propose);
    let kept = fingerprint(&plan_of(&s).diff);
    s.drain_events();
    let before = live(&ed, None);
    turn(&mut s, &mut ed, "and more", Mode::Propose);
    assert_eq!(provider.remaining(), 0, "the retries were not made");
    let kinds = signals(&s.drain_events());
    assert_eq!(kinds, vec![ErrKind::Network]);
    assert!(logic::can_retry(ErrKind::Network));
    assert_eq!(live(&ed, None), before);
    assert_eq!(fingerprint(&plan_of(&s).diff), kept);
    apply_and_check(&mut s, &mut ed);
}

#[test]
fn every_server_failure_has_a_title_and_a_next_step() {
    let cases = [
        (ProviderError::Auth, ErrKind::Auth),
        (
            ProviderError::Http {
                status: 500,
                body: "internal".to_string(),
            },
            ErrKind::Server,
        ),
        (
            ProviderError::Protocol("bad frame".to_string()),
            ErrKind::Protocol,
        ),
        (
            ProviderError::Blocked("policy".to_string()),
            ErrKind::Blocked,
        ),
    ];
    for (e, want) in cases {
        let (mut ed, _) = fixture(false);
        let before = live(&ed, None);
        let (mut s, _p) = session(&mut ed, Cap::Bulk, err_turns(e.clone(), 1));
        turn(&mut s, &mut ed, "go", Mode::Propose);
        let kinds = signals(&s.drain_events());
        assert_eq!(kinds, vec![want], "{e:?}");
        assert!(
            logic::can_retry(want) || logic::goes_to_settings(want),
            "{e:?}: the person is left without a way on"
        );
        assert!(s.plan().is_none());
        assert_eq!(live(&ed, None), before, "{e:?}");
    }
}
