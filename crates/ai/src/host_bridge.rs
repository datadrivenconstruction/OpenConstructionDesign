// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI through the host page (browser build): wire `ddc-web-bridge/1` (docs/BROWSER.md,
//! "Start and the JS bridge"). The editor hands one request envelope to `host.ai(request_json)`;
//! the host (the ERP) answers with its own AI profile. No key, address or profile id ever goes
//! into the page: the envelope holds the conversation only.
//!
//! The browser has no threads, so a call cannot wait for the host's promise. [`Mailbox`] is the
//! non-blocking side: a call posts the envelope and answers [`PENDING`]; the page takes the
//! outbox every frame, the reply comes back with [`Mailbox::answer`], and the same request asked
//! again then gets it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as J};

use crate::provider::{
    dump_request, image_base64, ChatRequest, FinishReason, Flow, ImageData, Message, Part, Provider,
    ProviderError, Role, StreamEvent, Usage, MAX_CALLS_PER_STEP, MAX_TOOL_ARGS_BYTES,
};

/// Name and major of the wire; sent and checked on every envelope.
pub const BRIDGE: &str = "ddc-web-bridge/1";
/// The text of the network error a [`Mailbox`] answers while the host works.
pub const PENDING: &str = "waiting for the host page";

/// True for the error a [`Mailbox`] gives before the host has answered.
pub fn is_pending(e: &ProviderError) -> bool {
    matches!(e, ProviderError::Network(m) if m == PENDING)
}

/// What carries one envelope to the host and its reply back.
pub trait HostCall: Send + Sync {
    fn call(&self, request: J) -> Result<J, ProviderError>;
}

fn role(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn image(i: &ImageData) -> J {
    json!({ "mime": i.mime, "width": i.width, "height": i.height, "data_b64": image_base64(i) })
}

fn part(p: &Part) -> J {
    match p {
        Part::Text(t) => json!({ "type": "text", "text": t }),
        Part::Image(i) => {
            let mut v = image(i);
            v["type"] = json!("image");
            v
        }
        Part::ToolCall { id, name, args } => json!({ "type": "tool_call", "id": id, "name": name, "args": args }),
        Part::ToolResult { id, content, images, is_error } => json!({
            "type": "tool_result", "id": id, "content": content, "is_error": is_error,
            "images": images.iter().map(image).collect::<Vec<_>>(),
        }),
    }
}

fn message(m: &Message) -> J {
    json!({ "role": role(m.role), "parts": m.parts.iter().map(part).collect::<Vec<_>>() })
}

/// The `chat` envelope of one request.
pub fn encode_chat(req: &ChatRequest<'_>) -> J {
    json!({
        "bridge": BRIDGE,
        "op": "chat",
        "model": req.model,
        "max_output_tokens": req.max_output_tokens,
        "temperature": req.temperature,
        "messages": req.messages.iter().map(message).collect::<Vec<_>>(),
        "tools": req.tools.iter().map(|t| json!({
            "name": t.name, "description": t.description, "input_schema": t.input_schema,
        })).collect::<Vec<_>>(),
    })
}

fn bad(what: &str) -> ProviderError {
    ProviderError::Protocol(format!("host reply: {what}"))
}

fn text_of(v: &J) -> String {
    v.as_str().unwrap_or_default().chars().take(500).collect()
}

/// A reply envelope's `error` as the provider error the conversation already handles.
fn error_of(e: &J) -> ProviderError {
    let msg = text_of(&e["message"]);
    match e["kind"].as_str().unwrap_or_default() {
        "auth" => ProviderError::Auth,
        "rate_limited" => ProviderError::RateLimited { retry_after_s: e["retry_after_s"].as_u64().map(|s| s.min(u32::MAX as u64) as u32) },
        "context_too_long" => ProviderError::ContextTooLong,
        "no_tools" => ProviderError::NoTools,
        "blocked" => ProviderError::Blocked(msg),
        "network" => ProviderError::Network(msg),
        "http" => ProviderError::Http { status: e["status"].as_u64().unwrap_or(0).min(999) as u16, body: msg },
        "cancelled" => ProviderError::Cancelled,
        _ => ProviderError::Protocol(if msg.is_empty() { "host error".into() } else { msg }),
    }
}

fn checked(reply: &J) -> Result<(), ProviderError> {
    match reply["bridge"].as_str() {
        Some(BRIDGE) => {}
        Some(other) => return Err(bad(&format!("wire '{}', expected {BRIDGE}", other.chars().take(40).collect::<String>()))),
        None => return Err(bad("no bridge field")),
    }
    if let Some(e) = reply.get("error").filter(|e| !e.is_null()) {
        return Err(error_of(e));
    }
    Ok(())
}

/// The stream events of a `chat` reply, in the order a streaming provider gives them.
pub fn decode_chat(reply: &J) -> Result<Vec<StreamEvent>, ProviderError> {
    checked(reply)?;
    let mut out = vec![];
    if let Some(t) = reply["text"].as_str().filter(|t| !t.is_empty()) {
        out.push(StreamEvent::TextDelta(t.to_string()));
    }
    let calls = match &reply["tool_calls"] {
        J::Null => &[][..],
        J::Array(a) => a.as_slice(),
        _ => return Err(bad("tool_calls is not a list")),
    };
    if calls.len() > MAX_CALLS_PER_STEP {
        return Err(bad(&format!("over {MAX_CALLS_PER_STEP} tool calls")));
    }
    let mut ids = BTreeSet::new();
    for c in calls {
        let (Some(id), Some(name)) = (c["id"].as_str(), c["name"].as_str()) else {
            return Err(bad("a tool call without id or name"));
        };
        if !ids.insert(id) {
            return Err(bad("two tool calls with one id"));
        }
        let args = if c["args"].is_null() { "{}".to_string() } else { c["args"].to_string() };
        if args.len() > MAX_TOOL_ARGS_BYTES {
            return Err(bad("tool arguments over the size limit"));
        }
        out.push(StreamEvent::ToolStart { id: id.into(), name: name.into() });
        out.push(StreamEvent::ToolArgsDelta { id: id.into(), json: args });
        out.push(StreamEvent::ToolEnd { id: id.into() });
    }
    let u = &reply["usage"];
    if u.is_object() {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        out.push(StreamEvent::Usage(Usage {
            input_tokens: n("input_tokens"),
            output_tokens: n("output_tokens"),
            cache_read_tokens: n("cache_read_tokens"),
            cache_write_tokens: n("cache_write_tokens"),
        }));
    }
    let finish = match reply["finish"].as_str() {
        Some("stop") | None if calls.is_empty() => FinishReason::Stop,
        Some("tool_calls") | None => FinishReason::ToolCalls,
        Some("stop") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some("filtered") => FinishReason::Filtered,
        Some(o) => FinishReason::Other(o.chars().take(40).collect()),
    };
    out.push(StreamEvent::Finish(finish));
    Ok(out)
}

/// A provider whose model sits behind the host page.
pub struct HostProvider {
    host: Arc<dyn HostCall>,
}

impl HostProvider {
    pub fn new(host: Arc<dyn HostCall>) -> Self {
        HostProvider { host }
    }
}

impl Provider for HostProvider {
    fn stream(&self, req: &ChatRequest<'_>, sink: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<(), ProviderError> {
        let reply = self.host.call(encode_chat(req))?;
        let events = decode_chat(&reply)?;
        // `Sent` only once the host took the request: a pending one has not gone yet
        if sink(StreamEvent::Sent(dump_request(req))) == Flow::Stop {
            return Err(ProviderError::Cancelled);
        }
        for e in events {
            if sink(e) == Flow::Stop {
                return Err(ProviderError::Cancelled);
            }
        }
        Ok(())
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let reply = self.host.call(json!({ "bridge": BRIDGE, "op": "models" }))?;
        checked(&reply)?;
        let a = reply["models"].as_array().ok_or_else(|| bad("models is not a list"))?;
        Ok(a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
    }
}

#[derive(Default)]
struct Boxes {
    next: u64,
    /// Request text -> id, for requests posted and not answered yet.
    asked: BTreeMap<String, u64>,
    outbox: Vec<(u64, J)>,
    replies: BTreeMap<u64, J>,
}

/// Non-blocking [`HostCall`]: requests wait in an outbox for the page, replies in an inbox.
#[derive(Default)]
pub struct Mailbox {
    b: Mutex<Boxes>,
}

impl Mailbox {
    /// Requests posted since the last call, with the id their reply must carry.
    pub fn take_outbox(&self) -> Vec<(u64, J)> {
        std::mem::take(&mut self.b.lock().unwrap_or_else(|e| e.into_inner()).outbox)
    }

    /// The host's reply to request `id`; a reply to no open request is dropped.
    pub fn answer(&self, id: u64, reply: J) {
        let mut b = self.b.lock().unwrap_or_else(|e| e.into_inner());
        if b.asked.values().any(|&v| v == id) {
            b.replies.insert(id, reply);
        }
    }

    /// Requests posted and not yet taken back with their reply.
    pub fn open(&self) -> usize {
        self.b.lock().unwrap_or_else(|e| e.into_inner()).asked.len()
    }
}

impl HostCall for Mailbox {
    fn call(&self, request: J) -> Result<J, ProviderError> {
        let key = request.to_string();
        let mut b = self.b.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&id) = b.asked.get(&key) {
            return match b.replies.remove(&id) {
                Some(r) => {
                    b.asked.remove(&key);
                    Ok(r)
                }
                None => Err(ProviderError::Network(PENDING.into())),
            };
        }
        b.next += 1;
        let id = b.next;
        b.asked.insert(key, id);
        b.outbox.push((id, request));
        Err(ProviderError::Network(PENDING.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolSchema;

    struct Canned(Mutex<Vec<J>>, J);
    impl HostCall for Canned {
        fn call(&self, request: J) -> Result<J, ProviderError> {
            self.0.lock().unwrap().push(request);
            Ok(self.1.clone())
        }
    }

    fn req_parts() -> (Vec<Message>, Vec<ToolSchema>) {
        let msgs = vec![
            Message::text(Role::System, "sys"),
            Message {
                role: Role::User,
                parts: vec![
                    Part::Text("draw".into()),
                    Part::Image(ImageData { mime: "image/png", bytes: Arc::new(vec![1, 2, 3]), width: 2, height: 1 }),
                ],
            },
        ];
        let tools = vec![ToolSchema { name: "add_wall".into(), description: "d".into(), input_schema: json!({"type": "object"}) }];
        (msgs, tools)
    }

    fn run(reply: J) -> (Result<Vec<StreamEvent>, ProviderError>, Vec<J>) {
        let host = Arc::new(Canned(Mutex::new(vec![]), reply));
        let p = HostProvider::new(host.clone());
        let (msgs, tools) = req_parts();
        let req = ChatRequest { model: "m", messages: &msgs, tools: &tools, max_output_tokens: 100, temperature: None };
        let mut ev = vec![];
        let r = p.stream(&req, &mut |e| {
            ev.push(e);
            Flow::Continue
        });
        let sent = host.0.lock().unwrap().clone();
        (r.map(|_| ev), sent)
    }

    #[test]
    fn the_envelope_carries_the_conversation_and_no_secret() {
        let (_, sent) = run(json!({"bridge": BRIDGE, "text": "ok"}));
        let e = &sent[0];
        assert_eq!(e["bridge"], BRIDGE);
        assert_eq!(e["op"], "chat");
        assert_eq!(e["messages"][1]["parts"][1]["type"], "image");
        assert_eq!(e["messages"][1]["parts"][1]["data_b64"], "AQID");
        assert_eq!(e["tools"][0]["name"], "add_wall");
        let keys: Vec<&String> = e.as_object().unwrap().keys().collect();
        for k in ["key", "api_key", "base_url", "profile", "token"] {
            assert!(!keys.iter().any(|x| x.as_str() == k), "{k} in the envelope");
        }
    }

    #[test]
    fn a_reply_becomes_the_events_of_a_streaming_provider() {
        let (r, _) = run(json!({
            "bridge": BRIDGE, "text": "two walls",
            "tool_calls": [{"id": "c1", "name": "add_wall", "args": {"len": 3}}],
            "usage": {"input_tokens": 10, "output_tokens": 4},
        }));
        let ev = r.unwrap();
        assert!(matches!(&ev[0], StreamEvent::Sent(_)));
        assert_eq!(ev[1], StreamEvent::TextDelta("two walls".into()));
        assert_eq!(ev[2], StreamEvent::ToolStart { id: "c1".into(), name: "add_wall".into() });
        assert_eq!(ev[3], StreamEvent::ToolArgsDelta { id: "c1".into(), json: "{\"len\":3}".into() });
        assert_eq!(ev[4], StreamEvent::ToolEnd { id: "c1".into() });
        assert!(matches!(ev[5], StreamEvent::Usage(Usage { input_tokens: 10, output_tokens: 4, .. })));
        assert_eq!(ev[6], StreamEvent::Finish(FinishReason::ToolCalls));
    }

    #[test]
    fn host_errors_map_to_provider_errors_and_a_foreign_wire_is_refused() {
        let err = |e: J| run(json!({"bridge": BRIDGE, "error": e})).0.unwrap_err();
        assert_eq!(err(json!({"kind": "auth"})), ProviderError::Auth);
        assert_eq!(err(json!({"kind": "rate_limited", "retry_after_s": 7})), ProviderError::RateLimited { retry_after_s: Some(7) });
        assert_eq!(err(json!({"kind": "http", "status": 502, "message": "bad gateway"})), ProviderError::Http { status: 502, body: "bad gateway".into() });
        assert!(matches!(run(json!({"text": "x"})).0, Err(ProviderError::Protocol(_))));
        assert!(matches!(run(json!({"bridge": "ddc-web-bridge/2"})).0, Err(ProviderError::Protocol(_))));
        let dup = json!({"bridge": BRIDGE, "tool_calls": [{"id": "a", "name": "n"}, {"id": "a", "name": "n"}]});
        assert!(matches!(run(dup).0, Err(ProviderError::Protocol(_))));
    }

    #[test]
    fn the_mailbox_posts_once_and_hands_the_reply_to_the_same_request() {
        let m = Arc::new(Mailbox::default());
        let p = HostProvider::new(m.clone());
        let (msgs, tools) = req_parts();
        let req = ChatRequest { model: "m", messages: &msgs, tools: &tools, max_output_tokens: 100, temperature: None };
        let mut ev: Vec<StreamEvent> = vec![];
        let mut sink = |e: StreamEvent| {
            ev.push(e);
            Flow::Continue
        };
        assert!(is_pending(&p.stream(&req, &mut sink).unwrap_err()));
        assert!(is_pending(&p.stream(&req, &mut sink).unwrap_err()), "still waiting");
        let out = m.take_outbox();
        assert_eq!(out.len(), 1, "posted once");
        assert!(m.take_outbox().is_empty());
        m.answer(out[0].0 + 100, json!({"bridge": BRIDGE, "text": "stray"}));
        m.answer(out[0].0, json!({"bridge": BRIDGE, "text": "done"}));
        p.stream(&req, &mut sink).unwrap();
        assert!(ev.contains(&StreamEvent::TextDelta("done".into())));
        assert_eq!(m.open(), 0);
    }
}
