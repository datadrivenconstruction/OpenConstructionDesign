//! An agent on its own thread, the real server on loopback, the real session and live model here.
//! The main thread stands in for the UI: it pumps until the agent's steps are done.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;

use ddc_ai::fake::ScriptedProvider;
use ddc_ai::mcp::test_client::Client;
use ddc_ai::mcp::{self, McpPolicy};
use ddc_ai::{MemoryStore, SecretStore};
use ddc_ai::Mode;
use ddc_app::ai::mcp_bridge::{session_reset, set_policy, McpBridge};
use ddc_app::ai::session::{live_policy, queued_note, AgentDraftView};
use ddc_app::ai::{AiError, SessionCfg, UiSession};
use ddc_app::editor::{Editor, OpFn};
use ddc_model::{Edit, Model};
use e2e_ai_common::*;
use e2e_common::{count, ids};
use serde_json::{json, Value as J};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The audit actor and the draft key of the default test connection.
const ACTOR: &str = "mcp:test-agent";
const OTHER: &str = "mcp:agent-b";

struct Rig {
    ed: Editor,
    wall: i64,
    bridge: McpBridge,
    session: UiSession,
    client: Client,
}

impl Rig {
    /// An option's own copy: an agent's writes still wait in its draft for the person's Apply.
    fn new() -> Rig {
        Rig::on(as_draft(cfg(ScriptedProvider::new(vec![]))))
    }

    /// The person's own project with nothing stored: the state every existing file opens in.
    fn main() -> Rig {
        Rig::on(cfg(ScriptedProvider::new(vec![])))
    }

    /// The live project with a custom wait for Apply.
    fn with_ttl(ttl: Duration) -> Rig {
        let mut c = cfg(ScriptedProvider::new(vec![]));
        c.confirm_ttl = ttl;
        Rig::on(c)
    }

    fn on(c: SessionCfg) -> Rig {
        Rig::with(c, |_, _| {})
    }

    /// `prep(ed, wall)` shapes the model before the session copies it.
    fn with(c: SessionCfg, prep: impl FnOnce(&mut Editor, i64)) -> Rig {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let wall = add_wall(&mut ed, level, (0.0, 0.0), (5.0, 0.0));
        prep(&mut ed, wall);
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let bridge = McpBridge::start(0, store.clone()).unwrap();
        let token = mcp::reveal(&mcp::load_token(store.as_ref()).unwrap().unwrap());
        let client = Client::new(bridge.port(), token);
        let session = start(&mut ed, c);
        Rig {
            ed,
            wall,
            bridge,
            session,
            client,
        }
    }

    fn tick(&mut self) {
        self.bridge.pump(&mut self.session, &self.ed, None);
        self.session.pump_core(&mut self.ed);
    }

    /// Runs `steps` as the agent while this thread keeps the editor alive.
    fn agent<T: Send + 'static>(&mut self, steps: impl FnOnce(Client) -> T + Send + 'static) -> T {
        let client = self.client.clone();
        let job = std::thread::spawn(move || steps(client));
        let until = Instant::now() + WAIT;
        while !job.is_finished() {
            self.tick();
            assert!(Instant::now() < until, "the agent got no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
        self.tick();
        job.join().expect("the agent's steps")
    }

    fn call(&mut self, name: &'static str, args: J) -> (J, bool) {
        self.agent(move |c| c.tool(name, args))
    }

    fn set(&self, policy: McpPolicy) {
        let id = self.session.session_id().to_string();
        set_policy(&self.ed, &id, policy).unwrap();
    }

    fn settle(&mut self) {
        assert!(self.session.wait_idle(&mut self.ed, WAIT));
    }

    /// The card's picture of `actor`'s draft newer than revision `after`, once it has arrived.
    fn draft_after(&mut self, actor: &str, after: u64) -> Arc<AgentDraftView> {
        let until = Instant::now() + WAIT;
        loop {
            self.tick();
            if let Some(d) = self.session.agent_draft(actor).filter(|d| d.rev > after) {
                return d.clone();
            }
            assert!(Instant::now() < until, "no draft picture for {actor}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn draft_of(&mut self, actor: &str) -> Arc<AgentDraftView> {
        self.draft_after(actor, 0)
    }

    /// The person's Apply on the card as it shows now.
    fn apply(&mut self, actor: &str) -> Result<i64, AiError> {
        self.settle();
        let rev = self.draft_of(actor).rev;
        let res = self.session.apply_agent_draft(&mut self.ed, actor, rev);
        self.settle();
        res
    }

    /// The person deletes `wall` by hand and the session's copy catches up.
    fn person_deletes(&mut self, wall: i64) {
        self.ed
            .run_op(OpFn(Arc::new(move |m: &mut Model| {
                m.apply_edits(&[Edit::Delete { table: "wall", id: wall }])
            })))
            .unwrap();
        let until = Instant::now() + WAIT;
        while self.ed.model.plan_stale(self.session.head()) {
            self.tick();
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Ticks for `ms` with nobody acting: nothing may land meanwhile.
    fn idle(&mut self, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            self.tick();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn new_wall(y: f64) -> J {
    json!({"title": "Wall", "cat": "wall", "items": [{"a": [0, y], "b": [5, y]}]})
}

/// The chat's family and geometry tools are the agent's too: same schemas, same handlers.
#[test]
fn an_agent_defines_a_draft_family_through_the_chat_tools() {
    let mut r = Rig::new();
    let listed = r.agent(|c| c.request("tools/list", json!({})).unwrap().json());
    let tools = listed["result"]["tools"].as_array().unwrap().clone();
    for n in ["define_family", "create_geometry"] {
        assert!(tools.iter().any(|t| t["name"] == n), "{n}");
    }
    let before = live_hash(&r.ed.model);
    let def = json!({"name": "AiBench", "category": "generic", "host": "none",
        "params": [{"name": "Width", "type": "length", "scope": "type", "unit": "mm",
                    "default": 1200, "min": 400, "max": 3000}],
        "forms": [{"name": "Top", "extrude": {"rect": ["0", "0", "Width", "400mm"]},
                   "z": ["400mm", "450mm"]}],
        "tests": [{"Width": 800}]});
    let (reply, is_error) = r.call("define_family", json!({"title": "Bench", "family": def}));
    assert!(!is_error, "{reply}");
    assert_eq!(reply["status"], "pending_user", "{reply}");
    assert_eq!(live_hash(&r.ed.model), before, "nothing before the person's Apply");
    let bare = json!({"name": "Bad", "params": [{"name": "W", "type": "length"}]});
    let (_, bad) = r.call("define_family", json!({"title": "Bad", "family": bare}));
    assert!(bad, "units are required for an agent too");
    r.apply(ACTOR).unwrap();
    let id = r.ed.model.family_id("AiBench").unwrap().expect("the family landed");
    assert!(r.ed.model.family_is_draft(id).unwrap());
}

fn audit_has(ed: &Editor, event: &str, actor: &str) -> bool {
    ed.model
        .audit_list(0, 500)
        .unwrap()
        .iter()
        .any(|e| e.2.event == event && e.2.actor == actor)
}

fn apply_note(ed: &Editor) -> String {
    let rows = ed.model.audit_list(0, 500).unwrap();
    let apply = rows
        .iter()
        .find(|e| e.2.event == "apply" && e.2.actor == "user" && e.2.status == "ok")
        .expect("the person's apply");
    apply.2.text.clone().unwrap_or_default()
}

#[test]
fn an_agent_write_waits_in_its_draft_until_one_apply() {
    let mut r = Rig::new();
    let listed = r.agent(|c| c.request("tools/list", json!({})).unwrap().json());
    assert!(listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "create_elements"));

    let before = live_hash(&r.ed.model);
    let ops = count(&r.ed.model, "op");
    let (reply, is_error) = r.call("create_elements", new_wall(9.0));
    assert!(!is_error, "{reply}");
    assert_eq!(reply["status"], "pending_user", "{reply}");
    assert_eq!(reply["queued"], true, "{reply}");
    assert_eq!(reply["untrusted"], "data", "every reply is fenced");
    let number = u32::try_from(reply["draft"].as_u64().unwrap()).unwrap();
    assert_eq!(reply["note"], queued_note(number));
    assert_eq!(live_hash(&r.ed.model), before, "nothing reaches the model before Apply");
    assert_eq!(count(&r.ed.model, "op"), ops);
    assert!(!r.ed.can_undo());
    r.tick();
    assert!(audit_has(&r.ed, "mcp_connect", ACTOR));
    assert_eq!(r.bridge.clients()[0].name, "test-agent");

    let d = r.draft_of(ACTOR);
    assert_eq!((d.name.as_str(), d.number, d.ops.len()), ("test-agent", number, 1));
    assert_eq!(d.ops[0].created, 1, "the card counts the elements of each operation");
    assert_eq!(r.session.agent_writes_waiting(), 1);

    let (read, _) = r.call("get_context", json!({}));
    assert_eq!(read["untrusted"], "data");
    assert_eq!(read["draft_view"], true, "a read says it sees the draft: {read}");
    assert_eq!(live_hash(&r.ed.model), before);

    r.apply(ACTOR).unwrap();
    assert_eq!(count(&r.ed.model, "wall"), 2);
    assert_eq!(count(&r.ed.model, "op"), ops + 1, "Apply is one journal row");
    assert!(r.session.agent_draft(ACTOR).is_none());
    assert_eq!(r.session.agent_batches("test-agent"), 1);
    let note = apply_note(&r.ed);
    assert!(note.contains(&format!("draft=#{number}")), "{note}");
    let (status, _) = r.call("draft_status", json!({}));
    assert_eq!(status["state"], "none", "{status}");
    assert_eq!(status["last"]["state"], "applied", "{status}");

    r.ed.undo();
    assert_eq!(
        live_hash(&r.ed.model),
        before,
        "one undo step, the exact hash"
    );
    assert!(!r.ed.can_undo());
}

/// A draft that grew after the person looked needs a new look; the Apply names the proposer.
#[test]
fn a_draft_that_grew_after_the_look_needs_a_new_look() {
    let mut r = Rig::new();
    let (reply, _) = r.call("create_elements", new_wall(9.0));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    let seen = r.draft_of(ACTOR);
    let (reply, _) = r.call("create_elements", new_wall(12.0));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    let grown = r.draft_after(ACTOR, seen.rev);
    r.settle();
    let res = r.session.apply_agent_draft(&mut r.ed, ACTOR, seen.rev);
    assert!(matches!(res, Err(AiError::ReviewNeeded)), "{res:?}");
    assert_eq!(count(&r.ed.model, "wall"), 1, "nothing lands from an old look");

    r.session.apply_agent_draft(&mut r.ed, ACTOR, grown.rev).unwrap();
    r.settle();
    assert_eq!(count(&r.ed.model, "wall"), 3);
    let note = apply_note(&r.ed);
    assert!(note.contains("proposer=agent:test-agent"), "{note}");
    assert!(note.contains("approver=user"), "{note}");
}

#[test]
fn a_wrong_token_a_foreign_origin_and_an_oversize_request_change_nothing() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let codes = r.agent(|c| {
        let wrong = Client::new(c.port, "wrong").request("tools/call", json!({"name": "delete_elements", "arguments": {"title": "x", "target": {"filter": "wall"}}})).unwrap().status;
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "delete_elements", "arguments": {"title": "x", "target": {"filter": "wall"}}, "_meta": c.meta()}}).to_string();
        let mut head = c.base_headers(call.len());
        head.push(("MCP-Protocol-Version".to_string(), "2026-07-28".to_string()));
        head.push(("Mcp-Method".to_string(), "tools/call".to_string()));
        head.push(("Mcp-Name".to_string(), "delete_elements".to_string()));
        head.push(("Origin".to_string(), "https://evil.example".to_string()));
        let foreign = c.raw("POST", "/mcp", &head, call.as_bytes()).unwrap().status;
        let huge = c.raw("POST", "/mcp", &c.base_headers(5 << 20), b"").unwrap().status;
        (wrong, foreign, huge)
    });
    assert_eq!(codes, (401, 403, 413));
    assert_eq!(live_hash(&r.ed.model), before);
    assert!(!r.ed.can_undo());
    assert!(
        r.bridge.clients().is_empty(),
        "none of them counts as a connected agent"
    );
}

#[test]
fn a_draft_older_than_the_persons_edit_is_blocked_until_a_recheck() {
    let mut r = Rig::new();
    let wall = r.wall;
    r.call("create_elements", new_wall(9.0));
    let d = r.draft_of(ACTOR);
    r.person_deletes(wall);
    let edited = live_hash(&r.ed.model);
    assert!(r.session.agent_draft_stale(&r.ed, ACTOR));
    let res = r.session.apply_agent_draft(&mut r.ed, ACTOR, d.rev);
    assert!(matches!(res, Err(AiError::Stale)), "{res:?}");
    assert_eq!(live_hash(&r.ed.model), edited, "a stale draft never lands");
    let (status, _) = r.call("draft_status", json!({}));
    assert_eq!(status["state"], "pending_user", "{status}");

    r.session.recheck_agent_draft(&r.ed, ACTOR);
    r.settle();
    let fresh = r.draft_after(ACTOR, d.rev);
    assert!(!r.session.agent_draft_stale(&r.ed, ACTOR));
    r.session
        .apply_agent_draft(&mut r.ed, ACTOR, fresh.rev)
        .unwrap();
    r.settle();
    assert_eq!(count(&r.ed.model, "wall"), 1, "the person's delete and the agent's wall");
    r.ed.undo();
    assert_eq!(live_hash(&r.ed.model), edited, "one step back to the person's own edit");
}

#[test]
fn a_draft_whose_wall_the_person_deleted_never_lands() {
    let mut r = Rig::new();
    let wall = r.wall;
    let args = json!({"title": "Delete it", "target": {"refs": [format!("wall:{wall}")]}});
    let (queued, _) = r.call("delete_elements", args);
    assert_eq!(queued["status"], "pending_user", "{queued}");
    let d = r.draft_of(ACTOR);
    assert_eq!(d.ops[0].deleted, 1);
    r.person_deletes(wall);
    let after_edit = live_hash(&r.ed.model);
    assert!(r.session.apply_agent_draft(&mut r.ed, ACTOR, d.rev).is_err());
    r.settle();
    r.session.recheck_agent_draft(&r.ed, ACTOR);
    r.settle();
    if let Some(d) = r.session.agent_draft(ACTOR).cloned() {
        let _ = r.session.apply_agent_draft(&mut r.ed, ACTOR, d.rev);
        r.settle();
    }
    assert_eq!(live_hash(&r.ed.model), after_edit, "nothing of the draft reached the model");
}

#[test]
fn the_element_limit_refuses_and_an_agent_cannot_lift_it() {
    use ddc_app::ai::validate::{plan_issues, PlanSize, Target, LARGE_PLAN};
    let size = PlanSize {
        touched: LARGE_PLAN + 1,
        deleted: 0,
        created: 0,
    };
    let issues = plan_issues(&[], size, Target::Main, false);
    assert!(issues
        .iter()
        .any(|i| i.level == ddc_app::ai::IssueLevel::Block));
    let tools = mcp::tools::list_json().to_string();
    assert!(
        !tools.contains("allow_large"),
        "only the person's card can lift the limit"
    );
}

#[test]
fn an_error_inside_an_atomic_changeset_rolls_the_whole_group_back() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (begun, _) = r.call("changeset_begin", json!({"atomic": true}));
    assert_eq!(begun["atomic"], true, "{begun}");
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let mut args = new_wall(9.0);
    args["changeset"] = json!(handle);
    assert_eq!(
        r.call("create_elements", args.clone()).0["status"],
        "pending_user"
    );

    let mut broken = args.clone();
    broken["no_such_field"] = json!(1);
    let (failed, is_error) = r.call("create_elements", broken);
    assert!(is_error);
    assert_eq!(failed["status"], "rolled_back", "{failed}");
    assert_eq!(failed["untrusted"], "data");

    let (late, late_error) = r.call("changeset_commit", json!({"changeset": handle}));
    assert!(late_error);
    assert_eq!(late["status"], "no_changeset");
    let (late_write, late_error) = r.call("create_elements", args);
    assert!(
        late_error,
        "a call of a dead group must not become an immediate write"
    );
    assert_eq!(late_write["status"], "no_changeset");
    assert_eq!(live_hash(&r.ed.model), before);
    assert!(!r.ed.can_undo());
    r.settle();
    r.tick();
    assert_eq!(r.session.agent_writes_waiting(), 0, "the group's items left the draft");

    assert_eq!(
        r.call("create_elements", new_wall(9.0)).0["status"],
        "pending_user",
        "and the agent can go on"
    );
    assert_eq!(live_hash(&r.ed.model), before);
}

#[test]
fn a_failed_write_in_a_plain_changeset_keeps_the_earlier_writes_and_the_group() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (begun, _) = r.call("changeset_begin", json!({}));
    assert_eq!(begun["atomic"], false, "{begun}");
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let mut args = new_wall(9.0);
    args["changeset"] = json!(handle);
    assert_eq!(r.call("create_elements", args.clone()).0["status"], "pending_user");

    // a bad argument and a write that fails inside the tool: each reports itself alone
    let mut broken = args.clone();
    broken["no_such_field"] = json!(1);
    let (failed, is_error) = r.call("create_elements", broken);
    assert!(is_error);
    assert_eq!(failed["status"], "op_failed", "{failed}");
    assert_eq!(failed["tool"], "create_elements", "{failed}");
    assert_eq!(failed["code"], "invalid_args", "{failed}");
    assert_eq!(failed["kept_ops"], 1, "the earlier write stays: {failed}");
    let mut unknown = args.clone();
    unknown["cat"] = json!("no_such_category");
    let (failed2, is_error2) = r.call("create_elements", unknown);
    assert!(is_error2);
    assert_eq!(failed2["status"], "op_failed", "{failed2}");
    assert_eq!(failed2["kept_ops"], 1, "{failed2}");

    let mut second = new_wall(12.0);
    second["changeset"] = json!(handle);
    assert_eq!(
        r.call("create_elements", second).0["status"],
        "pending_user",
        "the group is still open after a failure"
    );
    let (done, err) = r.call("changeset_commit", json!({"changeset": handle}));
    assert!(!err, "{done}");
    assert_eq!(done["status"], "pending_user", "{done}");
    assert_eq!(done["items"].as_array().map(Vec::len), Some(2), "{done}");
    r.settle();
    assert_eq!(r.draft_of(ACTOR).ops.len(), 2, "both good writes wait for Apply");
    assert_eq!(live_hash(&r.ed.model), before, "nothing reached the model");
}

#[test]
fn request_apply_puts_the_draft_before_the_person_who_alone_applies_it() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (none, none_err) = r.call("request_apply", json!({}));
    assert!(none_err, "{none}");
    assert_eq!(none["status"], "none", "{none}");

    assert_eq!(r.call("create_elements", new_wall(9.0)).0["status"], "pending_user");
    r.settle();
    r.draft_of(ACTOR);
    r.session.drain_events();
    let (asked, err) = r.call("request_apply", json!({}));
    assert!(!err, "{asked}");
    assert_eq!(asked["status"], "awaiting_user", "{asked}");
    assert_eq!(asked["blocked"], json!([]), "{asked}");
    assert_eq!(asked["ops"], 1, "{asked}");
    r.tick();
    let shown = r
        .session
        .drain_events()
        .into_iter()
        .any(|e| matches!(e, ddc_app::ai::UiEvent::AgentApplyAsked(ref a) if a == ACTOR));
    assert!(shown, "the card is put before the person");
    assert_eq!(live_hash(&r.ed.model), before, "asking writes nothing");
    let (st, _) = r.call("draft_status", json!({}));
    assert_eq!(st["state"], "pending_user", "{st}");

    r.apply(ACTOR).unwrap();
    r.settle();
    r.tick();
    assert_ne!(live_hash(&r.ed.model), before, "the person's Apply wrote it");
    let (st, _) = r.call("draft_status", json!({}));
    assert_eq!(st["last"]["state"], "applied", "{st}");
    assert!(
        mcp::tools::list_json()
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["name"] != "apply_draft"),
        "no agent tool applies on its own"
    );
}

#[test]
fn a_changeset_is_one_undo_step_that_restores_the_exact_hash() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (begun, _) = r.call("changeset_begin", json!({"title": "Two walls"}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    for y in [8.0, 9.0] {
        let mut args = new_wall(y);
        args["changeset"] = json!(handle);
        assert_eq!(r.call("create_elements", args).0["status"], "pending_user");
        assert_eq!(
            live_hash(&r.ed.model),
            before,
            "nothing reaches the live model before the commit"
        );
    }
    let (other, _) = r.call("create_elements", new_wall(7.0));
    assert_eq!(
        other["status"], "busy",
        "a call outside the open group waits"
    );

    let (reply, _) = r.call("changeset_commit", json!({"changeset": handle}));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    assert_eq!(reply["items"].as_array().unwrap().len(), 2, "{reply}");
    assert_eq!(live_hash(&r.ed.model), before, "a commit only closes the group");
    r.apply(ACTOR).unwrap();
    assert_eq!(count(&r.ed.model, "wall"), 3);
    r.ed.undo();
    assert_eq!(live_hash(&r.ed.model), before);
    assert!(!r.ed.can_undo(), "one group, one step");
}

#[test]
fn aborting_leaves_nothing() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (begun, _) = r.call("changeset_begin", json!({}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let mut args = new_wall(9.0);
    args["changeset"] = json!(handle);
    r.call("create_elements", args);
    assert_eq!(
        r.call("changeset_abort", json!({"changeset": handle})).0["status"],
        "aborted"
    );
    assert_eq!(live_hash(&r.ed.model), before);
    r.settle();
    r.tick();
    assert_eq!(r.session.agent_writes_waiting(), 0);
    assert_eq!(
        r.call("create_elements", new_wall(9.0)).0["status"],
        "pending_user"
    );
}

#[test]
fn an_old_stored_auto_still_queues_every_write() {
    let mut r = Rig::new();
    r.set(McpPolicy::AutoApply);
    assert_eq!(live_policy(&r.ed), McpPolicy::Confirm, "an old auto reads as a draft");
    let id = r.session.session_id().to_string();
    assert_eq!(session_reset(&r.ed, &id), Ok(false), "nothing to switch off");
    let before = live_hash(&r.ed.model);
    let (reply, _) = r.call("create_elements", new_wall(8.0));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    let (begun, _) = r.call("changeset_begin", json!({}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let mut args = new_wall(9.0);
    args["changeset"] = json!(handle);
    r.call("create_elements", args);
    let (reply, _) = r.call("changeset_commit", json!({"changeset": handle}));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    r.idle(200);
    assert_eq!(live_hash(&r.ed.model), before, "a draft only");
    assert_eq!(r.draft_of(ACTOR).ops.len(), 2);
}

#[test]
fn a_draft_waits_for_the_person_past_the_confirm_wait() {
    let mut r = Rig::with_ttl(Duration::from_millis(300));
    let before = live_hash(&r.ed.model);
    let (reply, _) = r.call("create_elements", new_wall(9.0));
    assert_eq!(reply["status"], "pending_user", "{reply}");
    r.idle(700);
    assert_eq!(live_hash(&r.ed.model), before);
    assert_eq!(r.draft_of(ACTOR).ops.len(), 1, "a draft does not time out");
    r.apply(ACTOR).unwrap();
    assert_eq!(count(&r.ed.model, "wall"), 2);
}

#[test]
fn discarding_a_draft_leaves_the_model_as_it_was() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    r.call("create_elements", new_wall(9.0));
    r.draft_of(ACTOR);
    r.session.discard_agent_draft(ACTOR);
    r.settle();
    r.tick();
    assert!(r.session.agent_draft(ACTOR).is_none());
    assert_eq!(live_hash(&r.ed.model), before);
    assert!(!r.ed.can_undo());
    let (status, _) = r.call("draft_status", json!({}));
    assert_eq!(status["last"]["state"], "discarded", "{status}");
    assert!(audit_has(&r.ed, "reject", ACTOR) || audit_has(&r.ed, "reject", "user"));

    // the agent may drop its own draft too, but never apply it
    r.call("create_elements", new_wall(8.0));
    r.draft_of(ACTOR);
    let (dropped, _) = r.call("discard_draft", json!({}));
    assert_eq!(dropped["status"], "discarded", "{dropped}");
    r.settle();
    r.tick();
    assert!(r.session.agent_draft(ACTOR).is_none());
    assert_eq!(live_hash(&r.ed.model), before);
    let (none, _) = r.call("discard_draft", json!({}));
    assert_eq!(none["status"], "none", "{none}");
}

#[test]
fn revoking_shuts_the_agent_out_at_once_and_drops_its_open_group() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let (begun, _) = r.call("changeset_begin", json!({}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let mut args = new_wall(9.0);
    args["changeset"] = json!(handle);
    r.call("create_elements", args);

    r.bridge.revoke(Some(&r.session), &r.ed).unwrap();
    r.tick();
    let old = r.agent(|c| c.request("tools/list", json!({})).unwrap().status);
    assert_eq!(old, 401);
    assert!(r.bridge.clients().is_empty(), "the indicator goes off");
    let new_token = mcp::reveal(&r.bridge.token().unwrap().unwrap());
    r.client = Client::new(r.bridge.port(), new_token);
    assert_eq!(
        r.call("create_elements", new_wall(9.0)).0["status"],
        "pending_user",
        "the open group is gone"
    );
    assert_eq!(live_hash(&r.ed.model), before, "nothing reached the model");
    assert!(audit_has(&r.ed, "mcp_policy", "user"));
}

#[test]
fn an_agent_cannot_reach_settings_consent_tokens_or_autonomy() {
    let mut r = Rig::new();
    let names = [
        "set_ai_policy",
        "mcp_policy",
        "set_policy",
        "rotate_token",
        "grant_consent",
        "run_sql",
        "read_file",
        "http_get",
        "allow_large",
    ];
    let codes = r.agent(move |c| {
        names.map(|n| {
            c.request(
                "tools/call",
                json!({"name": n, "arguments": {"mcp.policy": "auto"}}),
            )
            .unwrap()
            .json()["error"]["code"]
                .as_i64()
        })
    });
    assert!(codes.iter().all(|c| *c == Some(-32602)), "{codes:?}");

    let token_before = r.bridge.token().unwrap();
    r.set(McpPolicy::Confirm);
    // the arguments of a real tool are closed: a smuggled field fails and changes no state
    let (reply, is_error) = r.call("get_context", json!({"mcp.policy": "auto"}));
    assert!(is_error, "{reply}");
    assert_eq!(live_policy(&r.ed), McpPolicy::Confirm);
    assert_eq!(r.bridge.token().unwrap(), token_before);
    r.tick();
    let rows = r.ed.model.audit_list(0, 500).unwrap();
    assert!(!rows
        .iter()
        .any(|e| e.2.event == "mcp_policy" && e.2.actor.starts_with("mcp:")));
}

#[test]
fn an_old_file_without_the_key_loads_as_confirm() {
    let ed = Editor::new_in_memory().unwrap();
    assert_eq!(ed.model.ai_policy("mcp.policy").unwrap(), None);
    assert_eq!(live_policy(&ed), McpPolicy::Confirm);
}

/// find_missing says "N of M lack X" with the refs, and hands back a set to fill.
#[test]
fn find_missing_counts_the_gaps_and_hands_back_a_set() {
    let mut second = 0;
    let mut r = Rig::with(cfg(ScriptedProvider::new(vec![])), |ed, wall| {
        let level = ed.level;
        second = add_wall(ed, level, (0.0, 4.0), (5.0, 4.0));
        ed.model
            .add_param(&ddc_model::ParamSpec {
                name: "Fire".into(),
                ptype: ddc_model::ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["wall".into()],
            })
            .unwrap();
        let v = Some(ddc_model::PVal::Text("EI60".into()));
        let e = Edit::ParamValue { param: "Fire".into(), kind: "wall".into(), id: wall, value: v };
        ed.model.apply_edits(&[e]).unwrap();
    });
    let before = live_hash(&r.ed.model);
    let (reply, is_error) = r.call("find_missing", json!({"category": "wall", "param": "Fire"}));
    assert!(!is_error, "{reply}");
    let text = reply.to_string();
    assert!(text.contains("1 of 2 wall lack Fire"), "{text}");
    assert!(text.contains(&format!("wall:{second}")), "{text}");
    assert!(!text.contains(&format!("wall:{}\"", r.wall)), "the filled wall is not listed");
    assert!(text.contains("update_elements"), "{text}");
    let (bad, is_error) = r.call("find_missing", json!({"category": "door", "param": "Fire"}));
    assert!(is_error, "{bad}");
    assert_eq!(live_hash(&r.ed.model), before, "a read writes nothing");
}


/// Every agent call passes the registry gate, and every call that did not run says so with a
/// code: bad arguments with their path, a write held back by an open group, a dead handle.
#[test]
fn every_agent_failure_carries_a_registry_code() {
    use ddc_ai::registry::ErrorCode;
    let code = |v: &J| ErrorCode::parse(v["code"].as_str().unwrap_or_default());
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);

    let (bad, is_error) = r.call("create_elements", json!({"title": 5}));
    assert!(is_error, "{bad}");
    assert_eq!(code(&bad), Some(ErrorCode::InvalidArgs), "{bad}");
    assert!(bad["path"].as_str().is_some_and(|p| p.starts_with("args")), "{bad}");

    let (bad, is_error) = r.call("changeset_commit", json!({}));
    assert!(is_error, "{bad}");
    assert_eq!(code(&bad), Some(ErrorCode::InvalidArgs), "{bad}");

    let (begun, _) = r.call("changeset_begin", json!({}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let (held, is_error) = r.call("create_elements", new_wall(7.0));
    assert!(is_error, "a write held back by an open group did not run: {held}");
    assert_eq!(held["status"], "busy", "{held}");
    assert_eq!(code(&held), Some(ErrorCode::Busy), "{held}");
    let (again, is_error) = r.call("changeset_begin", json!({}));
    assert!(is_error, "{again}");
    assert_eq!(code(&again), Some(ErrorCode::Busy), "{again}");
    r.call("changeset_abort", json!({"changeset": handle.clone()}));

    let (dead, is_error) = r.call("changeset_commit", json!({"changeset": handle}));
    assert!(is_error, "{dead}");
    assert_eq!(code(&dead), Some(ErrorCode::NotFound), "{dead}");
    assert_eq!(live_hash(&r.ed.model), before);
}

fn wall_ids(ed: &Editor) -> Vec<i64> {
    ids(&ed.model, "wall")
}

/// The commit's receipt is the card's own numbers: counts, quantity deltas, IFC classes.
#[test]
fn receipt_matches_diff() {
    let mut r = Rig::main();
    let (begun, _) = r.call("changeset_begin", json!({"title": "Forty walls"}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let items: Vec<J> = (0..40)
        .map(|i| json!({"a": [0.0, 10.0 + i as f64], "b": [5.0, 10.0 + i as f64]}))
        .collect();
    let args = json!({"title": "Walls", "cat": "wall", "items": items, "changeset": handle});
    assert_eq!(r.call("create_elements", args).0["status"], "pending_user");
    let (reply, _) = r.call("changeset_commit", json!({"changeset": handle}));
    let rc = &reply["receipt"];
    assert_eq!(rc["counts"], json!({"created": 40, "changed": 0, "deleted": 0}), "{reply}");
    assert_eq!(rc["applied"], false);
    assert!(rc["token"].as_str().is_some_and(|t| t.starts_with("c:")), "{rc}");
    assert_eq!(rc["ifc"], json!([{"class": "IfcWall", "count": 40}]), "{rc}");
    let walls = rc["qty"].as_array().unwrap().iter().find(|q| q["key"] == "walls").expect("walls delta");
    assert_eq!(walls["after"].as_f64().unwrap() - walls["before"].as_f64().unwrap(), 40.0, "{rc}");
    assert!(rc.to_string().len() <= 8 * 1024);
    // the card shows what the receipt said
    let card = r.draft_of(ACTOR);
    let made = card.plan.diff.iter().filter(|d| d.kind == ddc_model::RowKind::Created).count();
    assert_eq!(made, 40);
    r.apply(ACTOR).unwrap();
    assert_eq!(count(&r.ed.model, "wall"), 41);
}

/// `total` walls, the agent deletes the first `gone` in one group: the commit's receipt names
/// the refusal with the counts and Apply is refused; the model stays as it was.
fn agent_deletes(total: usize, gone: usize) {
    let mut r = Rig::with(cfg(ScriptedProvider::new(vec![])), |ed, _| {
        let level = ed.level;
        for i in 0..total - 1 {
            add_wall(ed, level, (0.0, 20.0 + i as f64), (4.0, 20.0 + i as f64));
        }
    });
    let before = live_hash(&r.ed.model);
    let refs: Vec<String> = wall_ids(&r.ed).iter().take(gone).map(|i| format!("wall:{i}")).collect();
    let (begun, _) = r.call("changeset_begin", json!({}));
    let handle = begun["changeset"].as_str().unwrap().to_string();
    let args = json!({"title": "Many", "target": {"refs": refs}, "changeset": handle});
    r.call("delete_elements", args);
    let (reply, _) = r.call("changeset_commit", json!({"changeset": handle}));
    let b = &reply["receipt"]["blocked"];
    assert_eq!(b["code"], "mass_delete_blocked", "{reply}");
    assert_eq!(b["args"], json!({"n": gone.to_string(), "of": total.to_string()}), "{reply}");
    assert!(r.apply(ACTOR).is_err(), "{gone} of {total} never lands without the person");
    assert_eq!(live_hash(&r.ed.model), before);
}

/// An agent that deletes the whole model, or nine tenths of it, gets the refusal's numbers.
#[test]
fn mass_delete_blocks_agent() {
    agent_deletes(60, 60);
    agent_deletes(1000, 900);
}

/// The person's own "Allow large" lifts a big deletion, one undo step; a wipe stays blocked.
#[test]
fn mass_delete_person_ok_and_wipe_always_blocked() {
    let fixture = || {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        for i in 0..80 {
            add_wall(&mut ed, level, (0.0, i as f64), (4.0, i as f64));
        }
        ed
    };
    let mut ed = fixture();
    let ids: Vec<String> = wall_ids(&ed).iter().take(60).map(|i| format!("wall:{i}")).collect();
    let (mut s, _) = run(&mut ed, vec![tool("delete_elements", json!({"title": "Sixty", "target": {"refs": ids}}))]);
    assert!(!s.can_apply(), "60 of 80 needs the person");
    let keys: Vec<&str> = plan_of(&s).issues.iter().map(|i| i.text_key).collect();
    assert!(keys.contains(&"ai.issue.mass_delete"), "{keys:?}");
    s.allow_large(true);
    assert!(s.wait_idle(&mut ed, WAIT));
    apply_and_check(&mut s, &mut ed);
    assert_eq!(count(&ed.model, "wall"), 20);

    let mut ed = fixture();
    let (mut s, _) = run(&mut ed, vec![tool("delete_elements", json!({"title": "All", "target": {"filter": "wall"}}))]);
    s.allow_large(true);
    assert!(s.wait_idle(&mut ed, WAIT));
    assert!(!s.can_apply(), "the wipe stays blocked after Allow large");
    let keys: Vec<&str> = plan_of(&s).issues.iter().map(|i| i.text_key).collect();
    assert!(keys.contains(&"ai.issue.wipe"), "{keys:?}");
    assert_eq!(count(&ed.model, "wall"), 80);
}

impl Rig {
    /// `call` as the second connection.
    fn call_b(&mut self, name: &'static str, args: J) -> (J, bool) {
        self.client.name = "agent-b".to_string();
        let out = self.call(name, args);
        self.client.name = "test-agent".to_string();
        out
    }
}

fn wall_height(wall: i64, mm: &str) -> J {
    json!({"title": "Wall height", "target": {"refs": [format!("wall:{wall}")]}, "set": {"height": mm}})
}

/// Two agents whose drafts share no rows: the second is checked against the first's Apply by
/// its sets, not refused as stale, and both land.
#[test]
fn two_agents_disjoint_both_apply() {
    let mut r = Rig::new();
    let (a, _) = r.call("create_elements", new_wall(9.0));
    assert_eq!(a["status"], "pending_user", "{a}");
    let (b, _) = r.call_b("create_elements", new_wall(12.0));
    assert_eq!(b["status"], "pending_user", "{b}");
    let b0 = r.draft_of(OTHER);
    r.apply(ACTOR).unwrap();
    let b1 = r.draft_after(OTHER, b0.rev);
    assert!(b1.ops.iter().all(|o| o.conflict.is_none()), "{:?}", b1.ops);
    assert!(!r.session.agent_draft_stale(&r.ed, OTHER));
    r.apply(OTHER).unwrap();
    assert_eq!(count(&r.ed.model, "wall"), 3, "both drafts landed");
}

/// Two agents on the same wall: the second draft's item conflicts and names the row.
#[test]
fn two_agents_overlap_second_conflicts() {
    let mut r = Rig::new();
    let wall = r.wall;
    r.call("update_elements", wall_height(wall, "3500"));
    r.call_b("update_elements", wall_height(wall, "4200"));
    let b0 = r.draft_of(OTHER);
    r.apply(ACTOR).unwrap();
    let b1 = r.draft_after(OTHER, b0.rev);
    let c = b1.ops[0].conflict.clone().unwrap_or_default();
    assert!(c.contains("plan_conflict") && c.contains(&format!("wall:{wall}")), "{c}");
    assert!(b1.blocked());
}

/// The second Apply never overwrites the first: whatever the timing, the model keeps A's value.
#[test]
fn no_lost_update() {
    let mut r = Rig::new();
    let wall = r.wall;
    r.call("update_elements", wall_height(wall, "3500"));
    r.call_b("update_elements", wall_height(wall, "4200"));
    let b0 = r.draft_of(OTHER);
    r.apply(ACTOR).unwrap();
    let after_a = live_hash(&r.ed.model);
    // the look from before A's Apply
    assert!(r.session.apply_agent_draft(&mut r.ed, OTHER, b0.rev).is_err());
    r.settle();
    let b1 = r.draft_after(OTHER, b0.rev);
    let res = r.session.apply_agent_draft(&mut r.ed, OTHER, b1.rev);
    assert!(matches!(res, Err(AiError::Blocked)), "{res:?}");
    r.settle();
    assert_eq!(live_hash(&r.ed.model), after_a, "B's write did not replace A's");
}

/// An external agent's family text is checked and waits for the person; it adds no plan item,
/// never opens the editor by itself and leaves the agent's draft alone.
#[test]
fn an_agents_family_draft_waits_for_the_person() {
    let mut r = Rig::new();
    let before = live_hash(&r.ed.model);
    let ops = count(&r.ed.model, "op");
    let text = "family Bench
category furniture
param Width length type 1200mm
type \"B\" Width=1200mm
extrude Top rect(0, 0, Width, 400mm) z(0, 40mm)
";
    let (bad, bad_err) = r.call("family_draft", json!({ "text": text.replace("z(0, 40mm)", "z(0, Thick)") }));
    let bad = match bad.as_str() {
        Some(t) => serde_json::from_str(t).unwrap_or(bad.clone()),
        None => bad.clone(),
    };
    let (code, detail) = ddc_ai::fake::refusal_in(&bad).expect("a typed refusal");
    assert!(bad_err && code == "draft_invalid", "{bad}");
    assert_eq!(detail["findings"][0]["code"], "unknown_ref");
    let (reply, is_error) = r.call("family_draft", json!({ "text": text }));
    assert!(!is_error, "{reply}");
    assert!(reply.to_string().contains("pending_user"), "{reply}");
    r.settle();
    let queued: Vec<bool> = r
        .session
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            ddc_app::ai::session::UiEvent::FamilyDraft { queued, .. } => Some(queued),
            _ => None,
        })
        .collect();
    assert_eq!(queued, [true], "one offer, flagged as queued");
    assert!(r.session.agent_draft(ACTOR).is_none(), "no plan item, no draft card");
    assert_eq!(live_hash(&r.ed.model), before);
    assert_eq!(count(&r.ed.model, "op"), ops);
}
