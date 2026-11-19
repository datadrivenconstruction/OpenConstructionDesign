//! The three ways to run the same tool calls: the pure core, the MCP wire, the dialog.

use crate::e2e_ai_common::*;
use crate::e2e_common::{assert_same, dump_except, temp_file, TableRows};
use crate::fixtures;
use ddc_ai::fake::ScriptedProvider;
use ddc_ai::mcp::test_client::Client;
use ddc_ai::mcp::{self};
use ddc_ai::registry;
use ddc_ai::{MemoryStore, SecretStore, ToolOutcome};
use ddc_app::ai::exec;
use ddc_app::ai::mcp_bridge::McpBridge;
use ddc_app::ai::session::pure_exec::PureRun;
use ddc_app::editor::Editor;
use serde_json::Value as J;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Call {
    pub tool: String,
    pub args: J,
}

/// What a person could tell apart afterwards, with ids as the base project numbers them.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// One entry per call: "" when it ran, else the registry code.
    pub refusals: Vec<String>,
    /// The journal rows this run added: kind, element, name.
    pub journal: Vec<(String, i64, String)>,
    pub data: Vec<TableRows>,
    /// Elements created, changed, deleted between the base and the end, as a receipt counts them.
    pub receipt: (usize, usize, usize),
}

/// The counts of a write receipt, read from two dumps: rows of the element tables by id.
fn receipt_of(before: &[TableRows], after: &[TableRows]) -> (usize, usize, usize) {
    use std::collections::BTreeMap;
    let rows = |d: &[TableRows]| -> BTreeMap<(String, String), String> {
        d.iter()
            .filter(|t| ddc_app::ai::rows::ELEMENT_TABLES.contains(&t.0.as_str()))
            .flat_map(|(t, v)| {
                v.iter().map(move |r| {
                    let id = r.split('|').next().unwrap_or("").to_string();
                    ((t.clone(), id), r.clone())
                })
            })
            .collect()
    };
    let (b, a) = (rows(before), rows(after));
    let created = a.keys().filter(|k| !b.contains_key(*k)).count();
    let deleted = b.keys().filter(|k| !a.contains_key(*k)).count();
    let changed = a.iter().filter(|(k, v)| b.get(*k).is_some_and(|o| o != *v)).count();
    (created, changed, deleted)
}

pub trait Executor {
    fn name(&self) -> &'static str;
    fn run(&mut self, base: &str, calls: &[Call]) -> Outcome;
}

/// The registry code of a failed reply; "" when the call ran.
fn code(text: &str, is_error: bool) -> String {
    if !is_error {
        return String::new();
    }
    let out = ToolOutcome::error(text);
    registry::error_of(&out).expect("an error").refusal().code.as_str().to_string()
}

fn is_write(tool: &str) -> bool {
    ddc_ai::catalog::find(tool).is_some_and(|s| matches!(s.access, ddc_ai::catalog::Access::Propose))
}

/// Whether a draft of the run can reach Apply: some write call ran.
fn wrote(calls: &[Call], refusals: &[String]) -> bool {
    calls.iter().zip(refusals).any(|(c, r)| r.is_empty() && is_write(&c.tool))
}

/// The state after Apply, the one-undo and redo proofs included.
fn finish(ed: &mut Editor, top: i64, before: &[TableRows], refusals: Vec<String>, applied: bool) -> Outcome {
    let journal: Vec<(String, i64, String)> = {
        let mut q = ed
            .model
            .sql()
            .prepare("SELECT kind, element, COALESCE(name,'') FROM op WHERE seq > ?1 ORDER BY seq")
            .unwrap();
        q.query_map([top], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|x| x.unwrap())
            .collect()
    };
    let data = dump_except(&ed.model, NOT_MODEL);
    if applied {
        let now = live_hash(&ed.model);
        ed.undo();
        assert_same(&dump_except(&ed.model, NOT_MODEL), before, "one undo restores the project");
        ed.redo();
        assert_eq!(live_hash(&ed.model), now, "redo brings the same state back");
    }
    let receipt = receipt_of(before, &data);
    Outcome { refusals, journal, data, receipt }
}

pub struct Pure;

impl Executor for Pure {
    fn name(&self) -> &'static str {
        "pure"
    }

    fn run(&mut self, base: &str, calls: &[Call]) -> Outcome {
        let (mut ed, _) = fixtures::open(base);
        let path = temp_file("contract_pure_base.ddc");
        ed.model.snapshot_to(&path).unwrap();
        let before = dump_except(&ed.model, NOT_MODEL);
        let top = ed.model.op_top().unwrap();
        let c = cfg(ScriptedProvider::new(vec![]));
        let mut run = PureRun::open(c, &path, ed.model.change_head(), view_of(&ed)).unwrap();
        let refusals: Vec<String> = calls
            .iter()
            .map(|c| {
                let out = run.call(&c.tool, &c.args);
                code(&out.text, out.is_error)
            })
            .collect();
        let steps = run.compiled();
        let applied = wrote(calls, &refusals) && !steps.steps.is_empty();
        if applied {
            exec::apply(&mut ed, &steps).unwrap();
        }
        finish(&mut ed, top, &before, refusals, applied)
    }
}

pub struct Chat;

impl Executor for Chat {
    fn name(&self) -> &'static str {
        "chat"
    }

    fn run(&mut self, base: &str, calls: &[Call]) -> Outcome {
        let (mut ed, _) = fixtures::open(base);
        let before = dump_except(&ed.model, NOT_MODEL);
        let top = ed.model.op_top().unwrap();
        let tools = calls.iter().map(|c| tool(&c.tool, c.args.clone())).collect();
        // one model step per call: the turn's limits fit every call (the bad-args case calls each write tool)
        let provider = ScriptedProvider::new(script(tools));
        let mut c = cfg(provider.clone());
        let n = calls.len() as u32;
        c.limits.max_steps = c.limits.max_steps.max(n + 1);
        c.limits.max_tool_calls = c.limits.max_tool_calls.max(n);
        let mut s = start(&mut ed, c);
        turn(&mut s, &mut ed, "go", ddc_ai::Mode::Propose);
        let results = tool_results(&provider);
        assert_eq!(results.len(), calls.len(), "{results:?}");
        let refusals: Vec<String> = results.iter().map(|(t, e)| code(t, *e)).collect();
        let applied = wrote(calls, &refusals) && s.plan().is_some_and(|p| !p.items.is_empty());
        if applied {
            apply_and_check(&mut s, &mut ed);
        }
        // the proofs of `apply_and_check` leave the project redone
        finish(&mut ed, top, &before, refusals, false)
    }
}

pub struct Wire;

const ACTOR: &str = "mcp:test-agent";

struct Rig {
    ed: Editor,
    bridge: McpBridge,
    session: ddc_app::ai::UiSession,
    client: Client,
}

impl Rig {
    fn tick(&mut self) {
        self.bridge.pump(&mut self.session, &self.ed, None);
        self.session.pump_core(&mut self.ed);
    }

    fn calls(&mut self, calls: &[Call]) -> Vec<(J, bool)> {
        let client = self.client.clone();
        let list: Vec<(String, J)> = calls.iter().map(|c| (c.tool.clone(), c.args.clone())).collect();
        let job = std::thread::spawn(move || {
            list.into_iter().map(|(n, a)| client.tool(&n, a)).collect::<Vec<_>>()
        });
        let until = Instant::now() + WAIT;
        while !job.is_finished() {
            self.tick();
            assert!(Instant::now() < until, "the agent got no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
        self.tick();
        job.join().expect("the agent's calls")
    }

    /// The person's Apply of the agent's draft as its card shows it.
    fn apply(&mut self) {
        assert!(self.session.wait_idle(&mut self.ed, WAIT));
        let until = Instant::now() + WAIT;
        let rev = loop {
            self.tick();
            if let Some(d) = self.session.agent_draft(ACTOR) {
                break d.rev;
            }
            assert!(Instant::now() < until, "no draft card");
            std::thread::sleep(Duration::from_millis(2));
        };
        self.session.apply_agent_draft(&mut self.ed, ACTOR, rev).unwrap();
        assert!(self.session.wait_idle(&mut self.ed, WAIT));
    }
}

impl Executor for Wire {
    fn name(&self) -> &'static str {
        "wire"
    }

    fn run(&mut self, base: &str, calls: &[Call]) -> Outcome {
        let (mut ed, _) = fixtures::open(base);
        let before = dump_except(&ed.model, NOT_MODEL);
        let top = ed.model.op_top().unwrap();
        let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
        let bridge = McpBridge::start(0, store.clone()).unwrap();
        let token = mcp::reveal(&mcp::load_token(store.as_ref()).unwrap().unwrap());
        let client = Client::new(bridge.port(), token);
        let session = start(&mut ed, cfg(ScriptedProvider::new(vec![])));
        let mut rig = Rig { ed, bridge, session, client };
        let replies = rig.calls(calls);
        let refusals: Vec<String> = replies.iter().map(|(v, e)| code(&v.to_string(), *e)).collect();
        let applied = wrote(calls, &refusals);
        if applied {
            rig.apply();
        }
        finish(&mut rig.ed, top, &before, refusals, applied)
    }
}
