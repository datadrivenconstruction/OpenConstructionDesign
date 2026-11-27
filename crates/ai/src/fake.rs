// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Scripted provider for tests at every level: no network, no keys.

use crate::profile::{AuthKind, Profile, Protocol, ToolCalling};
use crate::provider::{
    dump_request, ChatRequest, FinishReason, Flow, Message, Part, Provider, ProviderError, Role,
    StreamEvent, ToolSchema, Usage,
};
use serde_json::{json, Value as J};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub enum Script {
    Text(String),
    Tool { name: String, args: J },
    Err(ProviderError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordedRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}

/// What a profile with `ToolCalling::JsonInText` talks through, around a scripted model.
pub fn text_tools(inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
    Arc::new(crate::proto_text_tools::TextToolsProvider::new(inner))
}

/// Each inner `Vec<Script>` answers one `stream` call. Running out of script is an error,
/// so a test that makes more requests than it planned fails loudly.
pub struct ScriptedProvider {
    turns: Mutex<VecDeque<Vec<Script>>>,
    requests: Mutex<Vec<RecordedRequest>>,
    usage: Usage,
    stream_no: AtomicU64,
}

impl ScriptedProvider {
    pub fn new(turns: Vec<Vec<Script>>) -> Arc<Self> {
        Self::with_usage(
            turns,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
        )
    }

    pub fn with_usage(turns: Vec<Vec<Script>>, usage: Usage) -> Arc<Self> {
        Arc::new(ScriptedProvider {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(Vec::new()),
            usage,
            stream_no: AtomicU64::new(0),
        })
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn remaining(&self) -> usize {
        self.turns.lock().unwrap().len()
    }
}

fn send(sink: &mut dyn FnMut(StreamEvent) -> Flow, e: StreamEvent) -> Result<(), ProviderError> {
    match sink(e) {
        Flow::Continue => Ok(()),
        Flow::Stop => Err(ProviderError::Cancelled),
    }
}

/// Arguments arrive in two fragments, as they do from a real stream.
fn split_json(json: &str) -> (&str, &str) {
    let mut mid = json.len() / 2;
    while !json.is_char_boundary(mid) {
        mid += 1;
    }
    json.split_at(mid)
}

impl Provider for ScriptedProvider {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        self.requests.lock().unwrap().push(RecordedRequest {
            model: req.model.to_string(),
            messages: req.messages.to_vec(),
            tools: req.tools.to_vec(),
            max_output_tokens: req.max_output_tokens,
            temperature: req.temperature,
        });
        let Some(script) = self.turns.lock().unwrap().pop_front() else {
            return Err(ProviderError::Protocol("script exhausted".to_string()));
        };
        send(sink, StreamEvent::Sent(dump_request(req)))?;
        let n = self.stream_no.fetch_add(1, Ordering::Relaxed);
        let mut called = false;
        for (i, step) in script.into_iter().enumerate() {
            match step {
                Script::Text(t) => send(sink, StreamEvent::TextDelta(t))?,
                Script::Tool { name, args } => {
                    let id = format!("call_{n}_{i}");
                    called = true;
                    send(
                        sink,
                        StreamEvent::ToolStart {
                            id: id.clone(),
                            name,
                        },
                    )?;
                    let json = args.to_string();
                    let (a, b) = split_json(&json);
                    send(
                        sink,
                        StreamEvent::ToolArgsDelta {
                            id: id.clone(),
                            json: a.to_string(),
                        },
                    )?;
                    send(
                        sink,
                        StreamEvent::ToolArgsDelta {
                            id: id.clone(),
                            json: b.to_string(),
                        },
                    )?;
                    send(sink, StreamEvent::ToolEnd { id })?;
                }
                Script::Err(e) => return Err(e),
            }
        }
        send(sink, StreamEvent::Usage(self.usage))?;
        send(
            sink,
            StreamEvent::Finish(if called {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }),
        )
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec!["scripted".to_string()])
    }
}

/// What the model saw, as one number: the count of messages, the person's texts and the calls
/// the model itself made (name and arguments), and whether each tool result was an error.
/// Tool result texts and ids are left out, so a run that only differs in them still replays.
pub fn request_fingerprint(req: &ChatRequest<'_>) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for x in b {
            h ^= u64::from(*x);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&(req.messages.len() as u64).to_le_bytes());
    for m in req.messages {
        for p in &m.parts {
            match p {
                Part::Text(t) if matches!(m.role, Role::User) => {
                    eat(b"T");
                    eat(without_nonce(t).as_bytes());
                }
                Part::ToolCall { name, args, .. } => {
                    eat(b"C");
                    eat(name.as_bytes());
                    eat(args.to_string().as_bytes());
                }
                Part::ToolResult { is_error, .. } => eat(if *is_error { b"E" } else { b"R" }),
                _ => {}
            }
        }
    }
    h
}

/// The text with the conversation's random boundary nonce taken out of its `[context <nonce>]`
/// and `[/context <nonce>]` markers: a new conversation draws a new one, the person's words stay.
fn without_nonce(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some((at, len)) = ["[context ", "[/context "].iter().filter_map(|m| rest.find(m).map(|i| (i, m.len()))).min() {
        out.push_str(&rest[..at + len]);
        rest = &rest[at + len..];
        let hex = rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len());
        if rest[hex..].starts_with(']') {
            rest = &rest[hex..];
        }
    }
    out.push_str(rest);
    out
}

/// One answer of the model as it was recorded, with the request it answered.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub fingerprint: u64,
    pub usage: Usage,
    pub script: Vec<Script>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recording {
    pub steps: Vec<Step>,
}

impl Recording {
    pub fn to_json(&self) -> J {
        let steps: Vec<J> = self
            .steps
            .iter()
            .map(|s| {
                let script: Vec<J> = s
                    .script
                    .iter()
                    .filter_map(|x| match x {
                        Script::Text(t) => Some(json!({"text": t})),
                        Script::Tool { name, args } => Some(json!({"tool": name, "args": args})),
                        Script::Err(_) => None,
                    })
                    .collect();
                json!({
                    "fp": format!("{:016x}", s.fingerprint),
                    "usage": [s.usage.input_tokens, s.usage.output_tokens, s.usage.cache_read_tokens, s.usage.cache_write_tokens],
                    "script": script,
                })
            })
            .collect();
        json!({"steps": steps})
    }

    pub fn from_json(j: &J) -> Result<Recording, String> {
        let mut steps = vec![];
        for (i, s) in j["steps"].as_array().ok_or("no steps")?.iter().enumerate() {
            let fp = s["fp"].as_str().ok_or(format!("step {i}: no fp"))?;
            let fingerprint = u64::from_str_radix(fp, 16).map_err(|e| format!("step {i}: fp: {e}"))?;
            let u: Vec<u64> = s["usage"]
                .as_array()
                .map(|a| a.iter().filter_map(J::as_u64).collect())
                .unwrap_or_default();
            let at = |k: usize| u.get(k).copied().unwrap_or(0);
            let usage = Usage {
                input_tokens: at(0),
                output_tokens: at(1),
                cache_read_tokens: at(2),
                cache_write_tokens: at(3),
                charged_micros: None,
            };
            let mut script = vec![];
            for x in s["script"].as_array().ok_or(format!("step {i}: no script"))? {
                if let Some(t) = x["text"].as_str() {
                    script.push(Script::Text(t.to_string()));
                } else if let Some(n) = x["tool"].as_str() {
                    script.push(Script::Tool { name: n.to_string(), args: x["args"].clone() });
                } else {
                    return Err(format!("step {i}: unknown script item"));
                }
            }
            steps.push(Step { fingerprint, usage, script });
        }
        Ok(Recording { steps })
    }
}

/// Wraps a real provider and keeps what it answered, to replay the run later without it.
pub struct RecordingProvider {
    inner: Arc<dyn Provider>,
    steps: Mutex<Vec<Step>>,
}

impl RecordingProvider {
    pub fn new(inner: Arc<dyn Provider>) -> Arc<Self> {
        Arc::new(RecordingProvider { inner, steps: Mutex::new(Vec::new()) })
    }

    pub fn recording(&self) -> Recording {
        Recording { steps: self.steps.lock().unwrap().clone() }
    }
}

impl Provider for RecordingProvider {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        let fingerprint = request_fingerprint(req);
        let mut script: Vec<Script> = vec![];
        // id, index in `script`, argument text so far
        let mut calls: Vec<(String, usize, String)> = vec![];
        let mut usage = Usage::default();
        self.inner.stream(req, &mut |e| {
            match &e {
                StreamEvent::TextDelta(t) if !t.is_empty() => match script.last_mut() {
                    Some(Script::Text(s)) => s.push_str(t),
                    _ => script.push(Script::Text(t.clone())),
                },
                StreamEvent::ToolStart { id, name } => {
                    script.push(Script::Tool { name: name.clone(), args: J::Null });
                    calls.push((id.clone(), script.len() - 1, String::new()));
                }
                StreamEvent::ToolArgsDelta { id, json } => {
                    if let Some(c) = calls.iter_mut().find(|c| &c.0 == id) {
                        c.2.push_str(json);
                    }
                }
                StreamEvent::Usage(u) => usage = *u,
                _ => {}
            }
            sink(e)
        })?;
        for (_, at, text) in calls {
            let parsed = if text.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&text)
                    .map_err(|e| ProviderError::Protocol(format!("recorded arguments are not JSON: {e}")))?
            };
            if let Script::Tool { args, .. } = &mut script[at] {
                *args = parsed;
            }
        }
        self.steps.lock().unwrap().push(Step { fingerprint, usage, script });
        Ok(())
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.inner.list_models()
    }
}

/// Answers from a recording. A request that differs from the recorded one, or one more than
/// recorded, is a `replay_drift` error: the run never passes by quietly diverging.
pub struct ReplayProvider {
    steps: Mutex<VecDeque<Step>>,
    drift: Mutex<Option<String>>,
    asked: AtomicU64,
}

pub const REPLAY_DRIFT: &str = "replay_drift";

impl ReplayProvider {
    pub fn new(r: Recording) -> Arc<Self> {
        Arc::new(ReplayProvider {
            steps: Mutex::new(r.steps.into()),
            drift: Mutex::new(None),
            asked: AtomicU64::new(0),
        })
    }

    /// Why the run left the recording, if it did.
    pub fn drift(&self) -> Option<String> {
        self.drift.lock().unwrap().clone()
    }

    pub fn remaining(&self) -> usize {
        self.steps.lock().unwrap().len()
    }

    fn fail(&self, why: String) -> ProviderError {
        let why = format!("{REPLAY_DRIFT}: {why}");
        *self.drift.lock().unwrap() = Some(why.clone());
        ProviderError::Protocol(why)
    }
}

impl Provider for ReplayProvider {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        let n = self.asked.fetch_add(1, Ordering::Relaxed);
        let Some(step) = self.steps.lock().unwrap().pop_front() else {
            return Err(self.fail(format!("request {n} has no recorded answer")));
        };
        if step.fingerprint != request_fingerprint(req) {
            return Err(self.fail(format!("request {n} differs from the recorded one")));
        }
        send(sink, StreamEvent::Sent(dump_request(req)))?;
        let mut called = false;
        for (i, item) in step.script.into_iter().enumerate() {
            match item {
                Script::Text(t) => send(sink, StreamEvent::TextDelta(t))?,
                Script::Tool { name, args } => {
                    called = true;
                    let id = format!("rp_{n}_{i}");
                    send(sink, StreamEvent::ToolStart { id: id.clone(), name })?;
                    send(sink, StreamEvent::ToolArgsDelta { id: id.clone(), json: args.to_string() })?;
                    send(sink, StreamEvent::ToolEnd { id })?;
                }
                Script::Err(e) => return Err(e),
            }
        }
        send(sink, StreamEvent::Usage(step.usage))?;
        send(
            sink,
            StreamEvent::Finish(if called { FinishReason::ToolCalls } else { FinishReason::Stop }),
        )
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        Ok(vec!["replay".to_string()])
    }
}

/// The refusal a failed tool reply carries, read the way the model reads it: its code and its
/// detail (parsed as JSON when it is JSON, else the words).
pub fn refusal_in(reply: &J) -> Option<(String, J)> {
    let r = reply.get("refused")?;
    let code = r["code"].as_str()?.to_string();
    let detail = match r["detail"].as_str() {
        Some(d) => serde_json::from_str(d).unwrap_or_else(|_| J::String(d.to_string())),
        None => J::Null,
    };
    Some((code, detail))
}

pub fn refusal_of(reply: &str) -> Option<(String, J)> {
    refusal_in(&serde_json::from_str(reply).ok()?)
}

/// A local profile for tests.
pub fn profile() -> Profile {
    Profile {
        id: "test".to_string(),
        name: "Test".to_string(),
        protocol: Protocol::ChatCompletions,
        base_url: "http://127.0.0.1:9/v1".to_string(),
        model: "scripted".to_string(),
        auth: AuthKind::None,
        extra_headers: Vec::new(),
        tool_calling: ToolCalling::Native,
        images: true,
        context_tokens: 200_000,
        max_output_tokens: 4096,
        price_in_per_mtok: None,
        price_out_per_mtok: None,
        executor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Role;
    use serde_json::json;

    fn run(p: &ScriptedProvider) -> (Result<(), ProviderError>, Vec<StreamEvent>) {
        let msgs = vec![Message::text(Role::User, "hi")];
        let req = ChatRequest {
            model: "m",
            messages: &msgs,
            tools: &[],
            max_output_tokens: 3,
            temperature: Some(0.5),
        };
        let mut got = Vec::new();
        let r = p.stream(&req, &mut |e| {
            if !matches!(e, StreamEvent::Sent(_)) {
                got.push(e);
            }
            Flow::Continue
        });
        (r, got)
    }

    #[test]
    fn the_fingerprint_ignores_the_context_nonce_but_not_the_words() {
        let fp = |t: &str| {
            let msgs = vec![Message::text(Role::User, t)];
            let req = ChatRequest { model: "m", messages: &msgs, tools: &[], max_output_tokens: 3, temperature: None };
            request_fingerprint(&req)
        };
        let note = |n: &str, w: &str| format!("[context {n}]\nselected=0\n[/context {n}]\n{w}");
        assert_eq!(fp(&note("0123456789abcdef", "a wall")), fp(&note("fedcba9876543210", "a wall")));
        assert_ne!(fp(&note("0123456789abcdef", "a wall")), fp(&note("0123456789abcdef", "two walls")));
        assert_eq!(without_nonce("[context 9f]x[/context 9f] [context of]"), "[context ]x[/context ] [context of]");
    }

    #[test]
    fn plays_the_script_and_records_the_request() {
        let p = ScriptedProvider::new(vec![vec![
            Script::Text("a".to_string()),
            Script::Tool {
                name: "t".to_string(),
                args: json!({ "k": "\u{0437}\u{043d}\u{0430}\u{0447}\u{0435}\u{043d}\u{0438}\u{0435}" }),
            },
        ]]);
        let (r, got) = run(&p);
        assert_eq!(r, Ok(()));
        assert_eq!(got[0], StreamEvent::TextDelta("a".to_string()));
        assert_eq!(
            got[1],
            StreamEvent::ToolStart {
                id: "call_0_1".to_string(),
                name: "t".to_string()
            }
        );
        let mut json = String::new();
        for e in &got {
            if let StreamEvent::ToolArgsDelta { json: part, .. } = e {
                json.push_str(part);
            }
        }
        assert_eq!(
            serde_json::from_str::<J>(&json).unwrap(),
            json!({ "k": "\u{0437}\u{043d}\u{0430}\u{0447}\u{0435}\u{043d}\u{0438}\u{0435}" })
        );
        assert_eq!(
            got.last(),
            Some(&StreamEvent::Finish(FinishReason::ToolCalls))
        );
        let seen = p.requests();
        assert_eq!(seen[0].model, "m");
        assert_eq!(seen[0].max_output_tokens, 3);
        assert_eq!(seen[0].messages[0], Message::text(Role::User, "hi"));
    }

    #[test]
    fn scripted_error_and_exhaustion() {
        let p = ScriptedProvider::new(vec![vec![Script::Err(ProviderError::Auth)]]);
        assert_eq!(run(&p).0, Err(ProviderError::Auth));
        assert_eq!(p.remaining(), 0);
        assert!(matches!(run(&p).0, Err(ProviderError::Protocol(_))));
        assert_eq!(p.requests().len(), 2);
    }

    #[test]
    fn sink_stop_cancels() {
        let p = ScriptedProvider::new(vec![vec![Script::Text("a".to_string())]]);
        let msgs = vec![Message::text(Role::User, "hi")];
        let req = ChatRequest {
            model: "m",
            messages: &msgs,
            tools: &[],
            max_output_tokens: 3,
            temperature: None,
        };
        assert_eq!(
            p.stream(&req, &mut |_| Flow::Stop),
            Err(ProviderError::Cancelled)
        );
    }

    /// A scripted model offers a family twice: the first text is refused with the report, the
    /// corrected one becomes a draft. The host is the contract only: no model, no journal.
    #[test]
    fn fake_provider_draft_roundtrip() {
        use crate::convo::{CallCx, CancelToken, Conversation, Limits, Mode, ToolHost, ToolOutcome, TurnEnd};
        use crate::family_draft::draft;
        use crate::provider::Part;

        #[derive(Default)]
        struct Host {
            drafts: Vec<String>,
        }
        impl ToolHost for Host {
            fn call(&mut self, name: &str, args: &J, _cx: &CallCx<'_>) -> ToolOutcome {
                let spec = crate::catalog::find(name).expect("in the catalog");
                if let Err(e) = crate::catalog::validate(spec, args) {
                    return crate::registry::CmdError::from_text(&e).outcome();
                }
                match draft(args, &|_| None) {
                    Ok(d) => {
                        self.drafts.push(d.name.clone());
                        ToolOutcome::ok(json!({ "draft_id": d.name, "report": d.report }).to_string())
                    }
                    Err(r) => crate::registry::CmdError::from_text(&r.wire()).outcome(),
                }
            }
        }
        let good = "family AHU
category equipment
param W length type 2000mm
param D length type 900mm
param H length type 1200mm
extrude Body rect(0, 0, W, D) z(0, H)
";
        let bad = good.replace("rect(0, 0, W, D)", "rect(0, 0, W, Depth)").replace("param H ", "param H2 ");
        let tool = |text: &str| Script::Tool {
            name: "family_draft".into(),
            args: json!({ "text": text }),
        };
        let p = ScriptedProvider::new(vec![
            vec![tool(&bad)],
            vec![tool(good)],
            vec![Script::Text("done".into())],
        ]);
        let mut c = Conversation::new(p.clone(), profile(), Limits::default(), CancelToken::new());
        // the word "family" offers family_draft only through the subject hint (off by default)
        c.set_intent_hint(true);
        let mut host = Host::default();
        let end = c.run_turn("make an air handling unit family".into(), String::new(), Mode::Propose, &mut host, &mut |_| {});
        assert_eq!(end, Ok(TurnEnd::Answered));
        assert!(p.requests()[0].tools.iter().any(|t| t.name == "family_draft"));
        assert_eq!(host.drafts, ["AHU"]);
        let results: Vec<(String, bool)> = c
            .messages()
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|x| match x {
                Part::ToolResult { content, is_error, .. } => Some((content.clone(), *is_error)),
                _ => None,
            })
            .collect();
        assert_eq!(results.len(), 2);
        let (code, detail) = refusal_of(&results[0].0).expect("a typed refusal");
        assert!(results[0].1 && code == "draft_invalid", "{}", results[0].0);
        assert_eq!(detail["findings"][0]["code"], "unknown_ref");
        assert!(!results[1].1 && results[1].0.contains("\"draft_id\":\"AHU\""), "{}", results[1].0);
    }

    const PUMP: &str = "family Pump
param W length type 1200mm
ref C x = 0
ref L x ~ -500mm
ref R x ~ 500mm
eq L C R
dim W L R
extrude Frame rect(L, -300mm, R, 300mm) z(0, 100mm)
";

    struct RepairHost {
        repair: crate::family_repair::RepairState,
        drafted: Vec<String>,
    }
    impl crate::convo::ToolHost for RepairHost {
        fn call(&mut self, name: &str, args: &J, _cx: &crate::convo::CallCx<'_>) -> crate::convo::ToolOutcome {
            assert_eq!(name, "family_draft");
            match self.repair.draft(args, &|_| None) {
                Ok(d) => {
                    self.drafted.push(d.name.clone());
                    crate::convo::ToolOutcome::ok(json!({ "draft_id": d.name }).to_string())
                }
                Err(w) => crate::registry::CmdError::from_text(&w).outcome(),
            }
        }
    }

    /// Runs scripted drafts through a conversation; returns the host and the tool replies.
    fn repair_run(
        texts: &[String],
        policy: crate::family_repair::RepairPolicy,
    ) -> (RepairHost, Vec<(String, bool)>) {
        use crate::convo::{CancelToken, Conversation, Limits, Mode};
        use crate::provider::Part;
        let mut turns: Vec<Vec<Script>> = texts
            .iter()
            .map(|t| vec![Script::Tool { name: "family_draft".into(), args: json!({ "text": t }) }])
            .collect();
        turns.push(vec![Script::Text("done".into())]);
        let p = ScriptedProvider::new(turns);
        let mut c = Conversation::new(p, profile(), Limits::default(), CancelToken::new());
        let mut host = RepairHost { repair: crate::family_repair::RepairState::new(policy), drafted: vec![] };
        let _ = c.run_turn("a pump family".into(), String::new(), Mode::Propose, &mut host, &mut |_| {});
        let replies = c
            .messages()
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|x| match x {
                Part::ToolResult { content, is_error, .. } => Some((content.clone(), *is_error)),
                _ => None,
            })
            .collect();
        (host, replies)
    }

    #[test]
    fn repair_fixes_conflict_in_two_rounds() {
        let conflict = PUMP.replace("dim W L R", "dim W L R
dim 500mm L C");
        let unknown = PUMP.replace("z(0, 100mm)", "z(0, Nope)");
        let (host, r) = repair_run(&[conflict, unknown, PUMP.to_string()], Default::default());
        assert_eq!(host.drafted, ["Pump"]);
        assert_eq!(r.len(), 3);
        let (c0, d0) = refusal_of(&r[0].0).expect("a typed refusal");
        assert!(r[0].1 && c0 == "draft_invalid", "{}", r[0].0);
        assert_eq!(d0["findings"][0]["code"], "sketch_conflict");
        assert!(d0["findings"][0]["excerpt"].as_array().is_some_and(|e| !e.is_empty()), "{d0}");
        let (c1, d1) = refusal_of(&r[1].0).expect("a typed refusal");
        assert!(r[1].1 && c1 == "draft_invalid", "{}", r[1].0);
        assert_eq!(d1["findings"][0]["code"], "unknown_ref");
        assert_eq!(d1["repair"]["round"], 2);
        assert!(!r[2].1);
        // the reply carries lines around the finding, not the whole text again
        assert!(!d0.to_string().contains("family Pump\nparam W"), "the whole text is not sent again");
    }

    #[test]
    fn repair_stops_on_repeated_findings() {
        let unknown = PUMP.replace("z(0, 100mm)", "z(0, Nope)");
        let (host, r) = repair_run(&[unknown.clone(), unknown], Default::default());
        assert!(host.drafted.is_empty());
        assert_eq!(refusal_of(&r[0].0).unwrap().0, "draft_invalid");
        let (c, d) = refusal_of(&r[1].0).expect("a typed refusal");
        assert_eq!((c.as_str(), &d["repair_stopped"]), ("repair_stopped", &json!("same_findings")), "{}", r[1].0);
        assert!(d["ask_the_person"].as_array().is_some_and(|a| !a.is_empty()), "{d}");
    }

    #[test]
    fn repair_stops_after_the_rounds() {
        let a = PUMP.replace("z(0, 100mm)", "z(0, Nope)");
        // the check stops at the first error: b fails in parsing, on another line than a
        let b = PUMP.replace("z(0, 100mm)", "z(0, Nada)").replace("eq L C R", "eqq L C R");
        let c3 = PUMP.replace("rect(L,", "rect(Q,");
        let (_, r) = repair_run(&[a, b, c3], Default::default());
        assert_eq!(refusal_of(&r[0].0).unwrap().0, "draft_invalid");
        assert_eq!(refusal_of(&r[1].0).unwrap().0, "draft_invalid");
        let (c, d) = refusal_of(&r[2].0).expect("a typed refusal");
        assert_eq!((c.as_str(), &d["repair_stopped"]), ("repair_stopped", &json!("rounds")), "{}", r[2].0);
    }

    #[test]
    fn repair_respects_token_budget() {
        let policy = crate::family_repair::RepairPolicy { max_tokens: 10, ..Default::default() };
        let a = PUMP.replace("z(0, 100mm)", "z(0, Nope)");
        let (_, r) = repair_run(&[a], policy);
        let (c, d) = refusal_of(&r[0].0).expect("a typed refusal");
        assert_eq!((c.as_str(), &d["repair_stopped"]), ("repair_stopped", &json!("token_budget")), "{}", r[0].0);
    }

    /// Every statement of the reference is one the parser knows (the reference is the parser's
    /// own documentation, verbatim).
    #[test]
    fn dsl_reference_matches_parser() {
        let md = crate::family_repair::DSL_REFERENCE;
        for src in [include_str!("../../geom/src/component.rs"), include_str!("../../geom/src/component/ext.rs")] {
            for l in src.lines().take_while(|l| l.starts_with("//!")) {
                let l = l.trim_start_matches("//!").trim_start_matches(' ');
                assert!(md.contains(l), "the reference lacks: {l}");
            }
        }
        let mut heads = std::collections::BTreeSet::new();
        let mut in_block = false;
        for l in md.lines() {
            if l.starts_with("```") {
                in_block = !in_block;
            } else if in_block {
                let code = l.split('#').next().unwrap_or("").trim();
                if let Some(h) = code.split_whitespace().next() {
                    heads.insert(h.to_string());
                }
            }
        }
        assert!(heads.len() >= 15, "{heads:?}");
        for h in heads {
            if h == "end" || h == "..." || h == "begin" {
                continue;
            }
            let e = ddc_geom::component::Family::parse(&format!("family X
{h} x
"));
            let msg = e.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(!msg.contains("unknown statement"), "{h}: {msg}");
        }
    }
}
