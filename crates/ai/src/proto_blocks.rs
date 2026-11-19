// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ContentBlocks` wire format: `POST {base}/messages`, SSE of typed content blocks.
//! The version header this format requires comes from `Profile::extra_headers`.

use crate::http::{stream_error, StreamDecoder, Wire};
use crate::provider::{
    image_base64, ChatRequest, FinishReason, ImageData, Message, Part, ProviderError, Role,
    StreamEvent, ToolSchema, Usage, MAX_CALLS_PER_STEP,
};
use crate::sse::SseEvent;
use serde_json::{json, Value as J};
use std::collections::HashMap;

pub(crate) struct BlocksWire;

static CACHE_READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static CACHE_WRITE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Process-wide cached input tokens (read, written) seen so far, for measurement.
pub(crate) fn cache_totals() -> (u64, u64) {
    use std::sync::atomic::Ordering::Relaxed;
    (CACHE_READ.load(Relaxed), CACHE_WRITE.load(Relaxed))
}

impl Wire for BlocksWire {
    fn path(&self) -> &'static str {
        "messages"
    }

    fn encode(&self, req: &ChatRequest<'_>) -> J {
        let (system, mut messages) = encode_messages(req.messages);
        mark_cache_points(&mut messages);
        let mut body = json!({ "model": req.model, "max_tokens": req.max_output_tokens, "stream": true, "messages": messages });
        if !system.is_empty() {
            // The prefix is tools, then system, then messages: one point here covers the first two.
            let block = json!({ "type": "text", "text": system, "cache_control": cache_point() });
            body["system"] = J::Array(vec![block]);
        }
        if !req.tools.is_empty() {
            let mut tools: Vec<J> = req.tools.iter().map(encode_tool).collect();
            // a group loaded mid-chat changes the system point; this one, found by lookback,
            // keeps the core tools cached (4 points in all: tools, system, two messages)
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = cache_point();
            }
            body["tools"] = J::Array(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!((f64::from(t) * 1000.0).round() / 1000.0);
        }
        body
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(BlocksDecoder::default())
    }
}

fn cache_point() -> J {
    json!({ "type": "ephemeral", "ttl": crate::provider::CACHE_TTL })
}

/// The history only grows, so the end of the last two messages is where the next request's
/// shared prefix ends; the older of the two survives a step that added many blocks.
fn mark_cache_points(messages: &mut [J]) {
    for m in messages.iter_mut().rev().take(2) {
        if let Some(last) = m["content"].as_array_mut().and_then(|c| c.last_mut()) {
            last["cache_control"] = cache_point();
        }
    }
}

fn encode_tool(t: &ToolSchema) -> J {
    json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })
}

fn image_block(i: &ImageData) -> J {
    json!({ "type": "image", "source": { "type": "base64", "media_type": i.mime, "data": image_base64(i) } })
}

fn encode_part(p: &Part) -> Option<J> {
    match p {
        // This format rejects empty text blocks.
        Part::Text(t) if t.is_empty() => None,
        Part::Text(t) => Some(json!({ "type": "text", "text": t })),
        Part::Image(i) => Some(image_block(i)),
        Part::ToolCall { id, name, args } => {
            let input = if args.is_object() {
                args.clone()
            } else {
                json!({})
            };
            Some(json!({ "type": "tool_use", "id": id, "name": name, "input": input }))
        }
        Part::ToolResult {
            id,
            content,
            images,
            is_error,
        } => {
            let mut blocks = Vec::new();
            if !content.is_empty() {
                blocks.push(json!({ "type": "text", "text": content }));
            }
            blocks.extend(images.iter().map(image_block));
            let mut r = json!({ "type": "tool_result", "tool_use_id": id, "content": blocks });
            if *is_error {
                r["is_error"] = J::Bool(true);
            }
            Some(r)
        }
    }
}

/// Tool answers travel as `user` content; neighbouring messages of one role merge
/// because the format expects roles to alternate.
fn encode_messages(msgs: &[Message]) -> (String, Vec<J>) {
    let mut system = String::new();
    let mut out: Vec<(&'static str, Vec<J>)> = Vec::new();
    for m in msgs {
        let role = match m.role {
            Role::System => {
                let text = crate::provider::join_text(&m.parts);
                if !system.is_empty() && !text.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&text);
                continue;
            }
            Role::Assistant => "assistant",
            Role::User | Role::Tool => "user",
        };
        let content: Vec<J> = m.parts.iter().filter_map(encode_part).collect();
        if content.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((r, existing)) if *r == role => existing.extend(content),
            _ => out.push((role, content)),
        }
    }
    let messages = out
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    (system, messages)
}

#[derive(Default)]
pub(crate) struct BlocksDecoder {
    open_tools: HashMap<u64, String>,
    usage: Usage,
    fresh: u64,
    cache_read: u64,
    cache_write: u64,
    saw_usage: bool,
    stop: Option<FinishReason>,
    called: bool,
    closed: bool,
}

fn map_stop(r: &str) -> FinishReason {
    match r {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "tool_use" => FinishReason::ToolCalls,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::Filtered,
        other => FinishReason::Other(other.to_string()),
    }
}

impl BlocksDecoder {
    fn read_usage(&mut self, u: &J) {
        // Cached input is billed apart but is still input: limits and totals count all of it.
        for (key, slot) in [
            ("input_tokens", &mut self.fresh),
            ("cache_read_input_tokens", &mut self.cache_read),
            ("cache_creation_input_tokens", &mut self.cache_write),
        ] {
            if let Some(n) = u.get(key).and_then(J::as_u64) {
                *slot = n;
                self.saw_usage = true;
            }
        }
        self.usage.input_tokens = self
            .fresh
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write);
        self.usage.cache_read_tokens = self.cache_read;
        self.usage.cache_write_tokens = self.cache_write;
        if let Some(n) = u.get("output_tokens").and_then(J::as_u64) {
            self.usage.output_tokens = n;
            self.saw_usage = true;
        }
    }

    fn close(&mut self, out: &mut Vec<StreamEvent>) {
        if self.closed {
            return;
        }
        self.closed = true;
        let mut left: Vec<(u64, String)> = self.open_tools.drain().collect();
        left.sort();
        for (_, id) in left {
            out.push(StreamEvent::ToolEnd { id });
        }
        if self.saw_usage {
            CACHE_READ.fetch_add(self.cache_read, std::sync::atomic::Ordering::Relaxed);
            CACHE_WRITE.fetch_add(self.cache_write, std::sync::atomic::Ordering::Relaxed);
            out.push(StreamEvent::Usage(self.usage));
        }
        let reason = self.stop.take().unwrap_or(if self.called {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        });
        out.push(StreamEvent::Finish(reason));
    }

    fn on_block_start(&mut self, v: &J, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let idx = v.get("index").and_then(J::as_u64).unwrap_or(0);
        let Some(block) = v.get("content_block") else {
            return Ok(());
        };
        match block.get("type").and_then(J::as_str) {
            Some("tool_use") => {
                if self.open_tools.len() >= MAX_CALLS_PER_STEP {
                    return Err(ProviderError::Protocol(
                        "too many tool calls in one answer".to_string(),
                    ));
                }
                let id = block
                    .get("id")
                    .and_then(J::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(J::as_str)
                    .unwrap_or("")
                    .to_string();
                self.called = true;
                self.open_tools.insert(idx, id.clone());
                out.push(StreamEvent::ToolStart {
                    id: id.clone(),
                    name,
                });
                // Arguments normally stream as deltas; the opening block holds `{}`.
                if let Some(input) = block
                    .get("input")
                    .filter(|i| i.as_object().map_or(false, |o| !o.is_empty()))
                {
                    out.push(StreamEvent::ToolArgsDelta {
                        id,
                        json: input.to_string(),
                    });
                }
            }
            Some("text") => {
                if let Some(t) = block
                    .get("text")
                    .and_then(J::as_str)
                    .filter(|t| !t.is_empty())
                {
                    out.push(StreamEvent::TextDelta(t.to_string()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn on_block_delta(&mut self, v: &J, out: &mut Vec<StreamEvent>) {
        let idx = v.get("index").and_then(J::as_u64).unwrap_or(0);
        let Some(delta) = v.get("delta") else { return };
        match delta.get("type").and_then(J::as_str) {
            Some("text_delta") => {
                if let Some(t) = delta
                    .get("text")
                    .and_then(J::as_str)
                    .filter(|t| !t.is_empty())
                {
                    out.push(StreamEvent::TextDelta(t.to_string()));
                }
            }
            Some("input_json_delta") => {
                let part = delta.get("partial_json").and_then(J::as_str).unwrap_or("");
                if let (Some(id), false) = (self.open_tools.get(&idx), part.is_empty()) {
                    out.push(StreamEvent::ToolArgsDelta {
                        id: id.clone(),
                        json: part.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
}

impl StreamDecoder for BlocksDecoder {
    fn on_sse(&mut self, ev: &SseEvent, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(false);
        }
        let v: J = serde_json::from_str(data)
            .map_err(|e| ProviderError::Protocol(format!("bad chunk: {e}")))?;
        match v.get("type").and_then(J::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                    self.read_usage(u);
                }
            }
            "content_block_start" => self.on_block_start(&v, out)?,
            "content_block_delta" => self.on_block_delta(&v, out),
            "content_block_stop" => {
                let idx = v.get("index").and_then(J::as_u64).unwrap_or(0);
                if let Some(id) = self.open_tools.remove(&idx) {
                    out.push(StreamEvent::ToolEnd { id });
                }
            }
            "message_delta" => {
                if let Some(r) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(J::as_str)
                {
                    self.stop = Some(map_stop(r));
                }
                if let Some(u) = v.get("usage") {
                    self.read_usage(u);
                }
            }
            "message_stop" => {
                self.close(out);
                return Ok(true);
            }
            "error" => return Err(stream_error(v.get("error").unwrap_or(&J::Null))),
            _ => {}
        }
        Ok(false)
    }

    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.closed || self.stop.is_some() {
            self.close(out);
            return Ok(());
        }
        Err(ProviderError::Network("stream ended early".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::{collapse, random_cuts, SseParser};
    use std::sync::Arc;

    fn decode(bytes: &[u8], seed: u64) -> Result<Vec<StreamEvent>, ProviderError> {
        let mut parser = SseParser::new();
        let mut d = BlocksDecoder::default();
        let mut out = Vec::new();
        for chunk in random_cuts(bytes, seed) {
            for ev in parser.feed(chunk) {
                if d.on_sse(&ev, &mut out)? {
                    return Ok(collapse(out));
                }
            }
        }
        for ev in parser.finish() {
            if d.on_sse(&ev, &mut out)? {
                return Ok(collapse(out));
            }
        }
        d.finish(&mut out)?;
        Ok(collapse(out))
    }

    fn check_all_cuts(bytes: &[u8], want: &[StreamEvent]) {
        let with_crlf = String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("\r\n", "\n")
            .replace('\n', "\r\n")
            .into_bytes();
        for seed in 0..200 {
            assert_eq!(decode(bytes, seed).unwrap(), want, "seed {seed}");
            assert_eq!(decode(&with_crlf, seed).unwrap(), want, "crlf seed {seed}");
        }
    }

    #[test]
    fn text_stream_survives_any_cut() {
        let want = [
            StreamEvent::TextDelta("\u{041d}\u{0430}\u{0439}\u{0434}\u{0435}\u{043d}\u{043e} 412 \u{0434}\u{0432}\u{0435}\u{0440}\u{0435}\u{0439}.".to_string()),
            StreamEvent::Usage(Usage {
                input_tokens: 120,
                output_tokens: 9,
                ..Usage::default()
            }),
            StreamEvent::Finish(FinishReason::Stop),
        ];
        check_all_cuts(include_bytes!("../tests/data/blocks_text.sse"), &want);
    }

    #[test]
    fn tool_stream_survives_any_cut() {
        let want = [
            StreamEvent::TextDelta("\u{0418}\u{0449}\u{0443}.".to_string()),
            StreamEvent::ToolStart {
                id: "call_a".to_string(),
                name: "query_elements".to_string(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_a".to_string(),
                json: "{\"filter\":\"\u{0434}\u{0432}\u{0435}\u{0440}\u{044c} level=3\"}".to_string(),
            },
            StreamEvent::ToolEnd {
                id: "call_a".to_string(),
            },
            StreamEvent::ToolStart {
                id: "call_b".to_string(),
                name: "get_context".to_string(),
            },
            StreamEvent::ToolEnd {
                id: "call_b".to_string(),
            },
            StreamEvent::Usage(Usage {
                input_tokens: 200,
                output_tokens: 31,
                ..Usage::default()
            }),
            StreamEvent::Finish(FinishReason::ToolCalls),
        ];
        check_all_cuts(include_bytes!("../tests/data/blocks_tools.sse"), &want);
    }

    #[test]
    fn eof_without_stop_reason_is_an_error() {
        let s = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n";
        assert!(matches!(
            decode(s.as_bytes(), 0),
            Err(ProviderError::Network(_))
        ));
        let s = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":2}}\n\n";
        assert_eq!(
            decode(s.as_bytes(), 0).unwrap(),
            vec![
                StreamEvent::Usage(Usage {
                    input_tokens: 0,
                    output_tokens: 2,
                    ..Usage::default()
                }),
                StreamEvent::Finish(FinishReason::Length)
            ]
        );
    }

    #[test]
    fn error_event_is_typed() {
        let s = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n";
        assert_eq!(
            decode(s.as_bytes(), 0),
            Err(ProviderError::RateLimited {
                retry_after_s: None
            })
        );
    }

    #[test]
    fn open_tool_blocks_are_capped() {
        let mut d = BlocksDecoder::default();
        let mut out = Vec::new();
        for i in 0..MAX_CALLS_PER_STEP {
            let v = json!({ "index": i, "content_block": { "type": "tool_use", "id": format!("c{i}"), "name": "x" } });
            d.on_block_start(&v, &mut out).unwrap();
        }
        let v = json!({ "index": u64::MAX, "content_block": { "type": "tool_use", "id": "more", "name": "x" } });
        assert!(matches!(
            d.on_block_start(&v, &mut out),
            Err(ProviderError::Protocol(_))
        ));
    }

    fn png() -> ImageData {
        ImageData {
            mime: "image/png",
            bytes: Arc::new(vec![1, 2, 3]),
            width: 4,
            height: 5,
        }
    }

    #[test]
    fn snapshot_travels_inside_the_tool_result() {
        let msgs = vec![
            Message::text(Role::System, "sys"),
            Message::text(Role::User, "go"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall {
                    id: "a".to_string(),
                    name: "view_snapshot".to_string(),
                    args: json!({}),
                }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: "a".to_string(),
                    content: "ok".to_string(),
                    images: vec![png()],
                    is_error: false,
                }],
            },
        ];
        let (system, m) = encode_messages(&msgs);
        assert_eq!(system, "sys");
        assert_eq!(m.len(), 3);
        let result = &m[2]["content"][0];
        assert_eq!(m[2]["role"], "user");
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "a");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(result["content"][1]["source"]["data"], "AQID");
        assert!(result.get("is_error").is_none());
    }

    #[test]
    fn neighbouring_user_messages_merge_and_empty_text_is_dropped() {
        let msgs = vec![
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: "a".to_string(),
                    content: "bad".to_string(),
                    images: vec![],
                    is_error: true,
                }],
            },
            Message::text(Role::User, "next"),
            Message::text(Role::Assistant, ""),
        ];
        let (_, m) = encode_messages(&msgs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["content"][0]["is_error"], true);
        assert_eq!(m[0]["content"][1]["text"], "next");
    }

    #[test]
    fn request_body_shape() {
        let msgs = vec![
            Message::text(Role::System, "s"),
            Message::text(Role::User, "u"),
        ];
        let tools = vec![ToolSchema {
            name: "t".to_string(),
            description: "d".to_string(),
            input_schema: json!({ "type": "object" }),
        }];
        let req = ChatRequest {
            model: "m",
            messages: &msgs,
            tools: &tools,
            max_output_tokens: 7,
            temperature: None,
        };
        let b = BlocksWire.encode(&req);
        assert_eq!(b["system"][0]["text"], "s");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["system"][0]["cache_control"]["ttl"], crate::provider::CACHE_TTL);
        assert_eq!(b["max_tokens"], 7);
        assert_eq!(b["stream"], true);
        assert_eq!(b["messages"][0]["content"][0]["text"], "u");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["tools"][0]["cache_control"]["type"], "ephemeral", "last tool");
    }

    #[test]
    fn the_last_two_messages_end_in_cache_points() {
        let mut m = vec![
            json!({ "role": "user", "content": [{ "type": "text", "text": "a" }] }),
            json!({ "role": "assistant", "content": [{ "type": "text", "text": "b" },
                                                      { "type": "text", "text": "c" }] }),
            json!({ "role": "user", "content": [{ "type": "text", "text": "d" }] }),
        ];
        mark_cache_points(&mut m);
        assert!(m[0]["content"][0].get("cache_control").is_none());
        assert!(m[1]["content"][0].get("cache_control").is_none());
        assert_eq!(m[1]["content"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(m[2]["content"][0]["cache_control"]["type"], "ephemeral");
        mark_cache_points(&mut []);
    }

    #[test]
    fn cached_input_counts_in_the_input_total() {
        let s = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\
                 \"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":20,\
                 \"output_tokens\":1}}}\n\n\
                 data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\
                 \"usage\":{\"output_tokens\":4}}\n\n";
        let ev = decode(s.as_bytes(), 0).unwrap();
        assert!(ev.contains(&StreamEvent::Usage(Usage {
            input_tokens: 127,
            output_tokens: 4,
            cache_read_tokens: 100,
            cache_write_tokens: 20,
        })));
    }

    #[test]
    fn non_object_tool_args_are_sent_as_an_empty_object() {
        let p = Part::ToolCall {
            id: "a".to_string(),
            name: "t".to_string(),
            args: J::Null,
        };
        assert_eq!(encode_part(&p).unwrap()["input"], json!({}));
    }
}
