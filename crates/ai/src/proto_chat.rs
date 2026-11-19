//! `ChatCompletions` wire format: `POST {base}/chat/completions`, SSE of `choices[0].delta`.

use crate::http::{stream_error, StreamDecoder, Wire};
use crate::provider::{
    image_base64, join_text, ChatRequest, FinishReason, ImageData, Message, Part, ProviderError,
    Role, StreamEvent, ToolSchema, Usage, MAX_CALLS_PER_STEP, MAX_TOOL_ARGS_BYTES,
};
use crate::sse::SseEvent;
use serde_json::{json, Value as J};

pub(crate) struct ChatWire;

impl Wire for ChatWire {
    fn path(&self) -> &'static str {
        "chat/completions"
    }

    fn encode(&self, req: &ChatRequest<'_>) -> J {
        let mut body = json!({
            "model": req.model,
            "messages": encode_messages(req.messages),
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": req.max_output_tokens,
        });
        if !req.tools.is_empty() {
            body["tools"] = J::Array(req.tools.iter().map(encode_tool).collect());
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!((f64::from(t) * 1000.0).round() / 1000.0);
        }
        body
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(ChatDecoder::default())
    }
}

fn encode_tool(t: &ToolSchema) -> J {
    json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input_schema } })
}

fn image_part(i: &ImageData) -> J {
    json!({ "type": "image_url", "image_url": { "url": format!("data:{};base64,{}", i.mime, image_base64(i)) } })
}

fn encode_user(parts: &[Part]) -> J {
    if parts.iter().all(|p| matches!(p, Part::Text(_))) {
        return json!({ "role": "user", "content": join_text(parts) });
    }
    let content: Vec<J> = parts
        .iter()
        .filter_map(|p| match p {
            Part::Text(t) => Some(json!({ "type": "text", "text": t })),
            Part::Image(i) => Some(image_part(i)),
            _ => None,
        })
        .collect();
    json!({ "role": "user", "content": content })
}

fn encode_assistant(parts: &[Part]) -> J {
    let text = join_text(parts);
    let calls: Vec<J> = parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolCall { id, name, args } => {
                Some(json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } }))
            }
            _ => None,
        })
        .collect();
    let content = if text.is_empty() {
        J::Null
    } else {
        J::String(text)
    };
    let mut m = json!({ "role": "assistant", "content": content });
    if !calls.is_empty() {
        m["tool_calls"] = J::Array(calls);
    }
    m
}

/// A `tool` message cannot carry images, and tool answers must directly follow the
/// call, so snapshots wait for the end of the tool messages and go out as `user`.
fn flush_images(pending: &mut Vec<(String, Vec<ImageData>)>, out: &mut Vec<J>) {
    if pending.is_empty() {
        return;
    }
    let mut content = Vec::new();
    for (id, images) in pending.drain(..) {
        content
            .push(json!({ "type": "text", "text": format!("Image returned by tool call {id}:") }));
        content.extend(images.iter().map(image_part));
    }
    out.push(json!({ "role": "user", "content": content }));
}

fn encode_messages(msgs: &[Message]) -> Vec<J> {
    let mut out = Vec::new();
    let mut pending = Vec::new();
    for m in msgs {
        if m.role != Role::Tool {
            flush_images(&mut pending, &mut out);
        }
        match m.role {
            Role::System => out.push(json!({ "role": "system", "content": join_text(&m.parts) })),
            Role::User => out.push(encode_user(&m.parts)),
            Role::Assistant => out.push(encode_assistant(&m.parts)),
            Role::Tool => {
                for p in &m.parts {
                    if let Part::ToolResult {
                        id,
                        content,
                        images,
                        is_error,
                    } = p
                    {
                        let text = if *is_error {
                            format!("[error] {content}")
                        } else {
                            content.clone()
                        };
                        out.push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
                        if !images.is_empty() {
                            pending.push((id.clone(), images.clone()));
                        }
                    }
                }
            }
        }
    }
    flush_images(&mut pending, &mut out);
    out
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    started: bool,
    ended: bool,
    early_args: String,
}

#[derive(Default)]
pub(crate) struct ChatDecoder {
    calls: Vec<Call>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    closed: bool,
}

impl ChatDecoder {
    fn on_tool_delta(&mut self, tc: &J, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let id = tc.get("id").and_then(J::as_str).unwrap_or("");
        // Some local servers omit `index`: a new id then means a new call.
        let idx = match tc.get("index").and_then(J::as_u64) {
            Some(i) => usize::try_from(i).unwrap_or(usize::MAX),
            None if !id.is_empty() || self.calls.is_empty() => self.calls.len(),
            None => self.calls.len() - 1,
        };
        // The index sizes an allocation: unchecked, one chunk could abort the whole process.
        if idx >= MAX_CALLS_PER_STEP {
            return Err(ProviderError::Protocol(
                "too many tool calls in one answer".to_string(),
            ));
        }
        if self.calls.len() <= idx {
            self.calls.resize_with(idx + 1, Call::default);
        }
        let call = &mut self.calls[idx];
        if call.id.is_empty() && !id.is_empty() {
            call.id = id.to_string();
        }
        let func = tc.get("function");
        if let Some(n) = func.and_then(|f| f.get("name")).and_then(J::as_str) {
            if call.name.is_empty() {
                call.name = n.to_string();
            }
        }
        // Some local servers send `arguments` as an object rather than a string.
        let args = match func.and_then(|f| f.get("arguments")) {
            Some(J::String(s)) => s.clone(),
            Some(J::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        if !call.started && !call.name.is_empty() {
            if call.id.is_empty() {
                call.id = format!("call_{idx}");
            }
            call.started = true;
            out.push(StreamEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
            });
            if !call.early_args.is_empty() {
                out.push(StreamEvent::ToolArgsDelta {
                    id: call.id.clone(),
                    json: std::mem::take(&mut call.early_args),
                });
            }
        }
        if args.is_empty() {
            return Ok(());
        }
        if call.started {
            out.push(StreamEvent::ToolArgsDelta {
                id: call.id.clone(),
                json: args,
            });
        } else if call.early_args.len() + args.len() > MAX_TOOL_ARGS_BYTES {
            return Err(ProviderError::Protocol(
                "tool arguments are too large".to_string(),
            ));
        } else {
            call.early_args.push_str(&args);
        }
        Ok(())
    }

    fn end_tools(&mut self, out: &mut Vec<StreamEvent>) {
        for c in &mut self.calls {
            if c.started && !c.ended {
                c.ended = true;
                out.push(StreamEvent::ToolEnd { id: c.id.clone() });
            }
        }
    }

    fn close(&mut self, out: &mut Vec<StreamEvent>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.end_tools(out);
        if let Some(u) = self.usage {
            out.push(StreamEvent::Usage(u));
        }
        let called = self.calls.iter().any(|c| c.started);
        let reason = self.finish.take().unwrap_or(if called {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        });
        out.push(StreamEvent::Finish(reason));
    }
}

fn map_finish(r: &str) -> FinishReason {
    match r {
        "stop" => FinishReason::Stop,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::Filtered,
        other => FinishReason::Other(other.to_string()),
    }
}

impl StreamDecoder for ChatDecoder {
    fn on_sse(&mut self, ev: &SseEvent, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError> {
        let data = ev.data.trim();
        if data == "[DONE]" {
            self.close(out);
            return Ok(true);
        }
        if data.is_empty() {
            return Ok(false);
        }
        let v: J = serde_json::from_str(data)
            .map_err(|e| ProviderError::Protocol(format!("bad chunk: {e}")))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(stream_error(err));
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = Some(Usage {
                input_tokens: u.get("prompt_tokens").and_then(J::as_u64).unwrap_or(0),
                output_tokens: u.get("completion_tokens").and_then(J::as_u64).unwrap_or(0),
                // This format reports no cache split and has no one cache price, so all input
                // counts at the plain price: the cost shown is an upper bound.
                ..Usage::default()
            });
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Ok(false);
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(t) = delta.get("content").and_then(J::as_str) {
                if !t.is_empty() {
                    out.push(StreamEvent::TextDelta(t.to_string()));
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(J::as_array) {
                for tc in calls {
                    self.on_tool_delta(tc, out)?;
                }
            }
        }
        if let Some(r) = choice.get("finish_reason").and_then(J::as_str) {
            self.finish = Some(map_finish(r));
            self.end_tools(out);
        }
        Ok(false)
    }

    /// A cut connection looks like a clean end of file; only a seen finish reason tells them apart.
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.closed || self.finish.is_some() {
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
        let mut d = ChatDecoder::default();
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

    fn crlf(bytes: &[u8]) -> Vec<u8> {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .replace("\r\n", "\n")
            .replace('\n', "\r\n")
            .into_bytes()
    }

    fn check_all_cuts(bytes: &[u8], want: &[StreamEvent]) {
        let with_crlf = crlf(bytes);
        for seed in 0..200 {
            assert_eq!(decode(bytes, seed).unwrap(), want, "seed {seed}");
            assert_eq!(decode(&with_crlf, seed).unwrap(), want, "crlf seed {seed}");
        }
    }

    fn start(id: &str, name: &str) -> StreamEvent {
        StreamEvent::ToolStart {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn args(id: &str, json: &str) -> StreamEvent {
        StreamEvent::ToolArgsDelta {
            id: id.to_string(),
            json: json.to_string(),
        }
    }

    fn end(id: &str) -> StreamEvent {
        StreamEvent::ToolEnd { id: id.to_string() }
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
        check_all_cuts(include_bytes!("../tests/data/chat_text.sse"), &want);
    }

    #[test]
    fn tool_stream_survives_any_cut() {
        let want = [
            StreamEvent::TextDelta("\u{0418}\u{0449}\u{0443}.".to_string()),
            start("call_a", "query_elements"),
            args("call_a", "{\"filter\":\"\u{0434}\u{0432}\u{0435}\u{0440}\u{044c} level=3\"}"),
            start("call_b", "get_element"),
            args("call_b", r#"{"ref":"door:55"}"#),
            end("call_a"),
            end("call_b"),
            StreamEvent::Usage(Usage {
                input_tokens: 200,
                output_tokens: 31,
                ..Usage::default()
            }),
            StreamEvent::Finish(FinishReason::ToolCalls),
        ];
        check_all_cuts(include_bytes!("../tests/data/chat_tools.sse"), &want);
    }

    #[test]
    fn interleaved_parallel_arguments_stay_with_their_call() {
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"x\",\"arguments\":\"\"}},{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"y\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{}\"}},{\"index\":0,\"function\":{\"arguments\":\"[]\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n",
        );
        let got = decode(s.as_bytes(), 1).unwrap();
        assert_eq!(
            got,
            vec![
                start("a", "x"),
                start("b", "y"),
                args("b", "{}"),
                args("a", "[]"),
                end("a"),
                end("b"),
                StreamEvent::Finish(FinishReason::ToolCalls)
            ]
        );
    }

    #[test]
    fn local_server_quirks_no_index_no_id_object_arguments() {
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"x\",\"arguments\":{\"a\":1}}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let got = decode(s.as_bytes(), 3).unwrap();
        assert_eq!(
            got,
            vec![
                start("call_0", "x"),
                args("call_0", r#"{"a":1}"#),
                end("call_0"),
                StreamEvent::Finish(FinishReason::Stop)
            ]
        );
    }

    #[test]
    fn finish_reasons_map() {
        assert_eq!(map_finish("length"), FinishReason::Length);
        assert_eq!(map_finish("content_filter"), FinishReason::Filtered);
        assert_eq!(map_finish("odd"), FinishReason::Other("odd".to_string()));
    }

    #[test]
    fn eof_after_finish_reason_is_clean_without_it_is_an_error() {
        let ok =
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\n";
        assert!(decode(ok.as_bytes(), 0).is_ok());
        let cut = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
        assert!(matches!(
            decode(cut.as_bytes(), 0),
            Err(ProviderError::Network(_))
        ));
    }

    #[test]
    fn error_inside_stream_is_typed() {
        let s = "data: {\"error\":{\"message\":\"Rate limit reached\",\"type\":\"x\"}}\n\n";
        assert_eq!(
            decode(s.as_bytes(), 0),
            Err(ProviderError::RateLimited {
                retry_after_s: None
            })
        );
        let s = "data: {\"error\":{\"message\":\"boom\"}}\n\n";
        assert!(matches!(
            decode(s.as_bytes(), 0),
            Err(ProviderError::Protocol(_))
        ));
    }

    #[test]
    fn a_hostile_index_is_an_error_not_an_allocation() {
        for index in ["1000000000000", "18446744073709551615", "256"] {
            let s = format!("data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":\"a\",\"function\":{{\"name\":\"x\"}}}}]}}}}]}}\n\n");
            assert!(
                matches!(decode(s.as_bytes(), 0), Err(ProviderError::Protocol(_))),
                "{index}"
            );
        }
        let s = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":255,\"id\":\"a\",\"function\":{\"name\":\"x\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";
        assert!(decode(s.as_bytes(), 0).is_ok());
    }

    #[test]
    fn calls_without_index_cannot_grow_past_the_cap() {
        let mut d = ChatDecoder::default();
        let mut out = Vec::new();
        for i in 0..MAX_CALLS_PER_STEP {
            let tc = json!({ "id": format!("c{i}"), "function": { "name": "x" } });
            d.on_tool_delta(&tc, &mut out).unwrap();
        }
        let tc = json!({ "id": "one-more", "function": { "name": "x" } });
        assert!(matches!(
            d.on_tool_delta(&tc, &mut out),
            Err(ProviderError::Protocol(_))
        ));
    }

    #[test]
    fn arguments_before_the_name_are_capped() {
        let mut d = ChatDecoder::default();
        let mut out = Vec::new();
        let tc =
            json!({ "index": 0, "function": { "arguments": "x".repeat(MAX_TOOL_ARGS_BYTES) } });
        d.on_tool_delta(&tc, &mut out).unwrap();
        let tc = json!({ "index": 0, "function": { "arguments": "x" } });
        assert!(matches!(
            d.on_tool_delta(&tc, &mut out),
            Err(ProviderError::Protocol(_))
        ));
        assert_eq!(d.calls[0].early_args.len(), MAX_TOOL_ARGS_BYTES);
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
    fn snapshot_from_a_tool_follows_all_tool_messages_as_user() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                parts: vec![
                    Part::ToolCall {
                        id: "a".to_string(),
                        name: "view_snapshot".to_string(),
                        args: json!({}),
                    },
                    Part::ToolCall {
                        id: "b".to_string(),
                        name: "get_context".to_string(),
                        args: json!({}),
                    },
                ],
            },
            Message {
                role: Role::Tool,
                parts: vec![
                    Part::ToolResult {
                        id: "a".to_string(),
                        content: "ok".to_string(),
                        images: vec![png()],
                        is_error: false,
                    },
                    Part::ToolResult {
                        id: "b".to_string(),
                        content: "bad".to_string(),
                        images: vec![],
                        is_error: true,
                    },
                ],
            },
        ];
        let e = encode_messages(&msgs);
        assert_eq!(e.len(), 4);
        assert_eq!(e[1]["role"], "tool");
        assert_eq!(e[1]["content"], "ok");
        assert_eq!(e[2]["tool_call_id"], "b");
        assert_eq!(e[2]["content"], "[error] bad");
        assert_eq!(e[3]["role"], "user");
        assert_eq!(e[3]["content"][1]["type"], "image_url");
        assert_eq!(
            e[3]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AQID"
        );
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
            temperature: Some(0.2),
        };
        let b = ChatWire.encode(&req);
        assert_eq!(b["stream"], true);
        assert_eq!(b["max_tokens"], 7);
        assert_eq!(b["temperature"], 0.2);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "u");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
        let no_tools = ChatRequest {
            tools: &[],
            temperature: None,
            ..req
        };
        let b = ChatWire.encode(&no_tools);
        assert!(b.get("tools").is_none() && b.get("temperature").is_none());
    }

    #[test]
    fn assistant_tool_call_arguments_are_a_json_string() {
        let m = Message {
            role: Role::Assistant,
            parts: vec![
                Part::Text("x".to_string()),
                Part::ToolCall {
                    id: "a".to_string(),
                    name: "t".to_string(),
                    args: json!({ "k": 1 }),
                },
            ],
        };
        let e = encode_assistant(&m.parts);
        assert_eq!(e["content"], "x");
        assert_eq!(e["tool_calls"][0]["function"]["arguments"], "{\"k\":1}");
    }
}
