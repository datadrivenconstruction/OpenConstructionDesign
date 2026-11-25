// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Blocking HTTP transport for streaming chat. A reader thread owns the socket, so a
//! stalled server can never hold up cancellation: the caller polls a channel every 50 ms
//! and on stop simply drops the receiver.

use crate::egress::{is_local_url, EgressGuard};
use crate::profile::{header_is_secret, AuthKind, Profile};
use crate::provider::{
    image_base64, image_note, images_of, ChatRequest, Flow, Provider, ProviderError, StreamEvent,
};
use crate::secret::Secret;
use crate::sse::{SseEvent, SseParser};
use ddc_net::{Body, NetError, Policy, Request, Response, Transport};
use serde_json::Value as J;
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = crate::limits::HTTP_CONNECT_TIMEOUT.dur();
const WRITE_TIMEOUT: Duration = crate::limits::HTTP_WRITE_TIMEOUT.dur();
const LIST_MODELS_TIMEOUT: Duration = crate::limits::LIST_MODELS_TIMEOUT.dur();
const MAX_ERROR_BODY: u64 = crate::limits::ERROR_BODY.n();
const MAX_ERROR_CHARS: usize = crate::limits::ERROR_CHARS.us();
const MAX_MODELS_BODY: u64 = crate::limits::MODELS_BODY.n();

/// One wire format. A new protocol is one new file implementing this.
pub(crate) trait Wire: Send + Sync {
    fn path(&self) -> &'static str;
    fn encode(&self, req: &ChatRequest<'_>) -> J;
    fn decoder(&self) -> Box<dyn StreamDecoder>;
}

/// `on_sse` returns true when the stream is logically complete.
pub(crate) trait StreamDecoder: Send {
    fn on_sse(&mut self, ev: &SseEvent, out: &mut Vec<StreamEvent>) -> Result<bool, ProviderError>;
    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
}

/// Errors reported inside an otherwise successful stream or a 4xx body carry only text.
pub(crate) fn classify_error_text(text: &str) -> Option<ProviderError> {
    let t = text.to_ascii_lowercase();
    if t.contains("rate_limit") || t.contains("rate limit") || t.contains("overloaded") {
        return Some(ProviderError::RateLimited {
            retry_after_s: None,
        });
    }
    if t.contains("context_length")
        || t.contains("context length")
        || t.contains("maximum context")
        || t.contains("prompt is too long")
    {
        return Some(ProviderError::ContextTooLong);
    }
    if t.contains("tool")
        && (t.contains("not support") || t.contains("unsupported") || t.contains("not available"))
    {
        return Some(ProviderError::NoTools);
    }
    None
}

/// An error object from inside a stream. The text may hold the key: `HttpCfg::clean`
/// scrubs and shortens it on the way out, in that order so a cut cannot split the key.
pub(crate) fn stream_error(err: &J) -> ProviderError {
    let text = match err.get("message").and_then(J::as_str) {
        Some(m) => format!("{} {m}", err.get("type").and_then(J::as_str).unwrap_or("")),
        None => err.to_string(),
    };
    classify_error_text(&text).unwrap_or_else(|| ProviderError::Protocol(text.trim().to_string()))
}

fn cap_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

struct HttpCfg {
    agent: Arc<dyn Transport>,
    base: String,
    auth: AuthKind,
    key: Option<Secret>,
    headers: Vec<(String, String)>,
    header_secrets: Vec<String>,
    sends_secret: bool,
    guard: EgressGuard,
}

impl HttpCfg {
    fn authed(&self, mut r: Request) -> Request {
        for (k, v) in &self.headers {
            r = r.header(k, v);
        }
        if let Some(key) = &self.key {
            match &self.auth {
                AuthKind::None => {}
                AuthKind::Bearer => r = r.header("Authorization", &format!("Bearer {}", key.expose())),
                AuthKind::Header { name } => r = r.header(name, key.expose()),
                AuthKind::Query { name } => r.url = with_query(&r.url, name, key.expose()),
            }
        }
        r
    }

    /// A server may echo the key back in an error body.
    fn scrub(&self, s: &str) -> String {
        let mut out = s.to_string();
        if let Some(k) = self.key.as_ref().filter(|k| !k.expose().is_empty()) {
            out = out.replace(k.expose(), "***");
        }
        for v in self.header_secrets.iter().filter(|v| !v.is_empty()) {
            out = out.replace(v.as_str(), "***");
        }
        // a transport error names the address, where a query key is percent-encoded
        if let AuthKind::Query { name } = &self.auth {
            out = crate::secret::redact_query_param(&out, name);
        }
        crate::secret::redact_url_secrets(&out)
    }

    /// The last stop for every error leaving this provider.
    fn clean(&self, e: ProviderError) -> ProviderError {
        let tidy = |s: String| cap_chars(&self.scrub(&s), MAX_ERROR_CHARS);
        match e {
            ProviderError::Network(s) => ProviderError::Network(tidy(s)),
            ProviderError::Protocol(s) => ProviderError::Protocol(tidy(s)),
            ProviderError::Blocked(s) => ProviderError::Blocked(tidy(s)),
            ProviderError::Http { status, body } => ProviderError::Http {
                status,
                body: tidy(body),
            },
            other => other,
        }
    }

    fn status_error(&self, code: u16, resp: Response) -> ProviderError {
        let retry_after_s = resp
            .header("retry-after")
            .and_then(|h| h.trim().parse::<u32>().ok());
        let mut raw = Vec::new();
        let _ = resp
            .into_reader()
            .take(MAX_ERROR_BODY)
            .read_to_end(&mut raw);
        let body = self.scrub(&String::from_utf8_lossy(&raw));
        match code {
            401 | 403 => ProviderError::Auth,
            // Busy servers, like `overloaded` inside a stream: worth waiting and repeating.
            429 | 503 | 529 => ProviderError::RateLimited { retry_after_s },
            400..=499 => match classify_error_text(&body) {
                Some(e @ (ProviderError::ContextTooLong | ProviderError::NoTools)) => e,
                _ => ProviderError::Http {
                    status: code,
                    body: cap_chars(&body, MAX_ERROR_CHARS),
                },
            },
            _ => ProviderError::Http {
                status: code,
                body: cap_chars(&body, MAX_ERROR_CHARS),
            },
        }
    }

    fn call_error(&self, e: NetError) -> ProviderError {
        ProviderError::Network(self.scrub(&e.0))
    }
}

pub(crate) struct HttpProvider {
    cfg: Arc<HttpCfg>,
    wire: Box<dyn Wire>,
}

impl HttpProvider {
    pub fn new(
        p: &Profile,
        key: Option<Secret>,
        guard: EgressGuard,
        wire: Box<dyn Wire>,
    ) -> Result<Self, ProviderError> {
        let base = p.base_url.trim_end_matches('/').to_string();
        let header_secrets: Vec<String> = p
            .extra_headers
            .iter()
            .filter(|(k, v)| header_is_secret(k, v))
            .map(|(_, v)| v.clone())
            .collect();
        // a key still in the address (the store refused it on load) is a secret on the wire too
        let sends_secret = (key.is_some() && p.auth != AuthKind::None)
            || !header_secrets.is_empty()
            || crate::profile::split_url_secret(&base).is_some();
        let agent = build_agent(is_local_url(&base), sends_secret, None)?;
        let cfg = HttpCfg {
            agent,
            base,
            auth: p.auth.clone(),
            key,
            headers: p.extra_headers.clone(),
            header_secrets,
            sends_secret,
            guard,
        };
        Ok(HttpProvider {
            cfg: Arc::new(cfg),
            wire,
        })
    }

    /// The text of the "what was sent" window: the body from the wire, except that image
    /// data becomes a size note and the key is gone.
    fn sent_text(&self, req: &ChatRequest<'_>, mut doc: J) -> String {
        let images = images_of(req.messages);
        if !images.is_empty() {
            let notes: HashMap<String, String> = images
                .iter()
                .map(|i| (image_base64(i), image_note(i)))
                .collect();
            redact_images(&mut doc, &notes);
        }
        self.cfg
            .scrub(&serde_json::to_string(&doc).unwrap_or_default())
    }

    fn stream_inner(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        let url = format!("{}/{}", self.cfg.base, self.wire.path());
        self.cfg.guard.check_send(&url, self.cfg.sends_secret)?;
        let doc = self.wire.encode(req);
        let body =
            serde_json::to_string(&doc).map_err(|e| ProviderError::Protocol(e.to_string()))?;
        let mut sent = Some(self.sent_text(req, doc));
        let (tx, rx) = mpsc::sync_channel::<Chunk>(64);
        let cfg = self.cfg.clone();
        thread::Builder::new()
            .name("ai-http".to_string())
            .spawn(move || read_stream(&cfg, &url, &body, &tx))
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let mut parser = SseParser::new();
        let mut decoder = self.wire.decoder();
        let mut out = Vec::new();
        loop {
            match rx.recv_timeout(POLL) {
                Ok(Chunk::Started) => {
                    if let Some(text) = sent.take() {
                        out.push(StreamEvent::Sent(text));
                        deliver(&mut out, sink)?;
                    }
                }
                Ok(Chunk::Bytes(bytes)) => {
                    for ev in parser.feed(&bytes) {
                        let done = decoder.on_sse(&ev, &mut out)?;
                        deliver(&mut out, sink)?;
                        if done {
                            return Ok(());
                        }
                    }
                    if parser.overflowed() {
                        return Err(ProviderError::Protocol(
                            "stream event is too large".to_string(),
                        ));
                    }
                    // A server sending only comments must not starve cancellation either.
                    heartbeat(sink)?;
                }
                Ok(Chunk::End) => {
                    for ev in parser.finish() {
                        let done = decoder
                            .on_sse(&ev, &mut out)
                            .map_err(|e| cut_short(&ev, e))?;
                        deliver(&mut out, sink)?;
                        if done {
                            return Ok(());
                        }
                    }
                    decoder.finish(&mut out)?;
                    return deliver(&mut out, sink);
                }
                Ok(Chunk::Fail(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => heartbeat(sink)?,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ProviderError::Network(
                        "connection reader stopped".to_string(),
                    ))
                }
            }
        }
    }

    fn list_models_inner(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/models", self.cfg.base);
        self.cfg.guard.check_send(&url, self.cfg.sends_secret)?;
        // No cancel path here, so the call gets its own short read deadline.
        let short = build_agent(is_local_url(&self.cfg.base), self.cfg.sends_secret, Some(LIST_MODELS_TIMEOUT))?;
        let req = self.cfg.authed(Request::new("GET", url));
        let resp = short.send(req).map_err(|e| self.cfg.call_error(e))?;
        let status = resp.status;
        if !(200..300).contains(&status) {
            return Err(self.cfg.status_error(status, resp));
        }
        let mut raw = Vec::new();
        resp.into_reader()
            .take(MAX_MODELS_BODY)
            .read_to_end(&mut raw)
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        let doc: J = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Protocol(format!("model list: {e}")))?;
        Ok(parse_models(&doc))
    }
}

fn redact_images(v: &mut J, notes: &HashMap<String, String>) {
    match v {
        J::String(s) => {
            let data = s.rsplit_once(";base64,").map_or(s.as_str(), |(_, d)| d);
            let head = s.len() - data.len();
            if let Some(note) = notes.get(data) {
                s.truncate(head);
                s.push_str(note);
            }
        }
        J::Array(items) => items.iter_mut().for_each(|x| redact_images(x, notes)),
        J::Object(fields) => fields.values_mut().for_each(|x| redact_images(x, notes)),
        _ => {}
    }
}

/// A connection cut inside the last line reads as a broken chunk, but it is a network
/// failure, and early ones are retried.
fn cut_short(ev: &SseEvent, e: ProviderError) -> ProviderError {
    match e {
        ProviderError::Protocol(_) if serde_json::from_str::<J>(ev.data.trim()).is_err() => {
            ProviderError::Network("stream ended early".to_string())
        }
        other => other,
    }
}

/// TLS and roots come from the OS (corporate networks). A local address never goes
/// through a proxy: that would send local traffic off the machine. `https_only` repeats
/// the guard's rule for keys, so one forgotten check cannot leak one.
fn build_agent(local: bool, sends_secret: bool, read: Option<Duration>) -> Result<Arc<dyn Transport>, ProviderError> {
    let read = read.unwrap_or_else(|| {
        if local {
            crate::limits::HTTP_READ_TIMEOUT_LOCAL.get_dur()
        } else {
            crate::limits::HTTP_READ_TIMEOUT.get_dur()
        }
    });
    // `localhost` passes as local by name; the transport resolves it to this machine only.
    let p = Policy {
        https_only: sends_secret && !local,
        loopback_only: local,
        redirects: 0,
        connect_timeout: CONNECT_TIMEOUT,
        read_timeout: read,
        write_timeout: WRITE_TIMEOUT,
    };
    ddc_net::transport(&p).map_err(|e| ProviderError::Network(e.0))
}

/// `url` with `name=value` added to its query, both percent-encoded.
fn with_query(url: &str, name: &str, value: &str) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    let (head, frag) = url.split_once('#').map_or((url, None), |(h, f)| (h, Some(f)));
    let sep = if head.contains('?') { '&' } else { '?' };
    let mut out = format!("{head}{sep}{}={}", enc(name), enc(value));
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

enum Chunk {
    /// The connection is up and the body is about to leave.
    Started,
    Bytes(Vec<u8>),
    End,
    Fail(ProviderError),
}

/// The HTTP client reads this only after connecting and writing the headers, which is
/// what makes `Chunk::Started` true. If the caller has cancelled by then, the body stays.
struct BodyReader {
    data: io::Cursor<Vec<u8>>,
    tx: SyncSender<Chunk>,
    started: bool,
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.started {
            self.started = true;
            if self.tx.send(Chunk::Started).is_err() {
                return Err(io::Error::other("cancelled"));
            }
        }
        self.data.read(buf)
    }
}

fn read_stream(cfg: &HttpCfg, url: &str, body: &str, tx: &SyncSender<Chunk>) {
    // An explicit length keeps the body identical to a plain send: not chunked.
    let reader = BodyReader {
        data: io::Cursor::new(body.as_bytes().to_vec()),
        tx: tx.clone(),
        started: false,
    };
    let req = cfg
        .authed(Request::new("POST", url))
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .body(Body::Reader(Box::new(reader), body.len() as u64));
    let resp = match cfg.agent.send(req) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Chunk::Fail(cfg.call_error(e)));
            return;
        }
    };
    // Redirects are off, so a 3xx arrives here as a plain response.
    let status = resp.status;
    if !(200..300).contains(&status) {
        let _ = tx.send(Chunk::Fail(cfg.status_error(status, resp)));
        return;
    }
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(Chunk::End);
                return;
            }
            Ok(n) => {
                if tx.send(Chunk::Bytes(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Chunk::Fail(ProviderError::Network(
                    cfg.scrub(&e.to_string()),
                )));
                return;
            }
        }
    }
}

fn deliver(
    out: &mut Vec<StreamEvent>,
    sink: &mut dyn FnMut(StreamEvent) -> Flow,
) -> Result<(), ProviderError> {
    for e in out.drain(..) {
        if sink(e) == Flow::Stop {
            return Err(ProviderError::Cancelled);
        }
    }
    Ok(())
}

fn heartbeat(sink: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<(), ProviderError> {
    match sink(StreamEvent::TextDelta(String::new())) {
        Flow::Continue => Ok(()),
        Flow::Stop => Err(ProviderError::Cancelled),
    }
}

impl Provider for HttpProvider {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError> {
        self.stream_inner(req, sink).map_err(|e| self.cfg.clean(e))
    }

    fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        self.list_models_inner().map_err(|e| self.cfg.clean(e))
    }
}

/// Servers list models as `data[].id` or `models[].{name,id}`.
fn parse_models(doc: &J) -> Vec<String> {
    let list = doc
        .get("data")
        .or_else(|| doc.get("models"))
        .and_then(J::as_array);
    let mut names: Vec<String> = list
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(J::as_str))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use crate::profile::{build_provider, Protocol};
    use crate::provider::{FinishReason, ImageData, Message, Part, Role, ToolSchema, Usage};
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use web_time::Instant;

    const KEY: &str = "sk-test-123";

    struct Server {
        base: String,
        hits: Arc<AtomicUsize>,
        seen: Arc<Mutex<Vec<String>>>,
    }

    fn read_request(s: &mut TcpStream) -> String {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let head_end = data.windows(4).position(|w| w == b"\r\n\r\n");
            if let Some(end) = head_end {
                let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if data.len() >= end + 4 + len {
                    break;
                }
            }
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => data.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    fn serve(response: Vec<u8>, stall_ms: u64) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (h, sn) = (hits.clone(), seen.clone());
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { return };
                h.fetch_add(1, Ordering::SeqCst);
                let req = read_request(&mut s);
                sn.lock().unwrap().push(req);
                let _ = s.write_all(&response);
                let _ = s.flush();
                if stall_ms > 0 {
                    thread::sleep(Duration::from_millis(stall_ms));
                }
            }
        });
        Server {
            base: format!("http://127.0.0.1:{port}/v1"),
            hits,
            seen,
        }
    }

    fn response(status: &str, extra: &str, body: &[u8]) -> Vec<u8> {
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\n{extra}Connection: close\r\nContent-Length: {}\r\n\r\n", body.len());
        let mut out = head.into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn provider(base: &str, protocol: Protocol) -> Arc<dyn Provider> {
        let mut p = fake::profile();
        p.base_url = base.to_string();
        p.protocol = protocol;
        p.auth = AuthKind::Bearer;
        build_provider(&p, Some(Secret::new(KEY)), EgressGuard::default()).unwrap()
    }

    fn run(p: &dyn Provider) -> (Result<(), ProviderError>, Vec<StreamEvent>) {
        let msgs = vec![Message::text(Role::User, "hi")];
        let tools: Vec<ToolSchema> = Vec::new();
        let req = ChatRequest {
            model: "m",
            messages: &msgs,
            tools: &tools,
            max_output_tokens: 100,
            temperature: None,
        };
        let mut events = Vec::new();
        let r = p.stream(&req, &mut |e| {
            if e != StreamEvent::TextDelta(String::new()) && !matches!(e, StreamEvent::Sent(_)) {
                events.push(e);
            }
            Flow::Continue
        });
        (r, events)
    }

    fn run_sent(
        p: &dyn Provider,
        msgs: &[Message],
        tools: &[ToolSchema],
    ) -> (Result<(), ProviderError>, Vec<String>) {
        let req = ChatRequest {
            model: "m",
            messages: msgs,
            tools,
            max_output_tokens: 100,
            temperature: None,
        };
        let mut sent = Vec::new();
        let r = p.stream(&req, &mut |e| {
            if let StreamEvent::Sent(s) = e {
                sent.push(s);
            }
            Flow::Continue
        });
        (r, sent)
    }

    fn wire_body(srv: &Server) -> String {
        let seen = srv.seen.lock().unwrap();
        seen[0].split_once("\r\n\r\n").unwrap().1.to_string()
    }

    fn one_tool() -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "find_x".to_string(),
            description: "finds x".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
        }]
    }

    #[test]
    fn sent_is_the_wire_body_verbatim() {
        let srv = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let (r, sent) = run_sent(p.as_ref(), &[Message::text(Role::User, "hi")], &one_tool());
        assert_eq!(r, Ok(()));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], wire_body(&srv));
        assert!(
            sent[0].contains("finds x") && sent[0].contains("\"stream\":true"),
            "{}",
            sent[0]
        );
    }

    #[test]
    fn sent_never_shows_the_key() {
        let srv = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let (_, sent) = run_sent(
            p.as_ref(),
            &[Message::text(Role::User, format!("my key is {KEY}"))],
            &[],
        );
        assert!(!sent[0].contains(KEY), "{}", sent[0]);
        assert!(sent[0].contains("my key is ***"), "{}", sent[0]);
        assert!(wire_body(&srv).contains(KEY));
    }

    #[test]
    fn sent_shows_images_as_a_note_in_both_protocols() {
        let png = ImageData {
            mime: "image/png",
            bytes: Arc::new(vec![1, 2, 3]),
            width: 4,
            height: 5,
        };
        let msgs = vec![Message {
            role: Role::User,
            parts: vec![Part::Text("hi".to_string()), Part::Image(png)],
        }];
        let cases: [(Protocol, &[u8]); 2] = [
            (
                Protocol::ChatCompletions,
                include_bytes!("../tests/data/chat_text.sse"),
            ),
            (
                Protocol::ContentBlocks,
                include_bytes!("../tests/data/blocks_text.sse"),
            ),
        ];
        for (protocol, stream) in cases {
            let srv = serve(response("200 OK", "", stream), 0);
            let p = provider(&srv.base, protocol);
            let (r, sent) = run_sent(p.as_ref(), &msgs, &[]);
            assert_eq!(r, Ok(()));
            assert!(wire_body(&srv).contains("AQID"));
            assert!(!sent[0].contains("AQID"), "{}", sent[0]);
            assert!(sent[0].contains("[image 4x5, 3 bytes]"), "{}", sent[0]);
        }
    }

    #[test]
    fn json_in_text_sent_shows_what_really_went_out() {
        let srv = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let mut pf = fake::profile();
        pf.base_url = srv.base.clone();
        pf.tool_calling = crate::profile::ToolCalling::JsonInText;
        let p = build_provider(&pf, None, EgressGuard::default()).unwrap();
        let (r, sent) = run_sent(p.as_ref(), &[Message::text(Role::User, "hi")], &one_tool());
        assert_eq!(r, Ok(()));
        assert_eq!(sent[0], wire_body(&srv));
        assert!(sent[0].contains("find_x: finds x"), "{}", sent[0]);
        assert!(!sent[0].contains("\"tools\""), "{}", sent[0]);
    }

    #[test]
    fn nothing_counts_as_sent_when_nothing_left() {
        let mut pf = fake::profile();
        pf.base_url = "https://api.example.invalid/v1".to_string();
        let guard = EgressGuard {
            local_only: true,
            consented_host: None,
        };
        let p = build_provider(&pf, None, guard).unwrap();
        let (r, sent) = run_sent(p.as_ref(), &[Message::text(Role::User, "hi")], &[]);
        assert!(matches!(r, Err(ProviderError::Blocked(_))));
        assert!(sent.is_empty());

        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let p = provider(
            &format!("http://127.0.0.1:{port}/v1"),
            Protocol::ChatCompletions,
        );
        let (r, sent) = run_sent(p.as_ref(), &[Message::text(Role::User, "hi")], &[]);
        assert!(matches!(r, Err(ProviderError::Network(_))), "{r:?}");
        assert!(sent.is_empty());
    }

    /// A transport error names the address: a query key of any name is masked, also when
    /// percent-encoded (it no longer matches the raw key).
    #[test]
    fn a_query_key_in_an_error_address_is_masked() {
        let cfg = HttpCfg {
            agent: build_agent(false, true, None).unwrap(),
            base: "https://h.test/v1".into(),
            auth: AuthKind::Query { name: "x-pass".into() },
            key: Some(Secret::new("fake/SECRET+r107=")),
            headers: vec![],
            header_secrets: vec![],
            sends_secret: true,
            guard: EgressGuard::default(),
        };
        let e = "https://h.test/v1/models?x-pass=fake%2FSECRET%2Br107%3D&v=2: dns failed";
        let out = cfg.scrub(e);
        assert_eq!(out, "https://h.test/v1/models?x-pass=***&v=2: dns failed");
        let u = cfg.scrub("https://me:fake-SECRET-r107u@h.test/v1: refused");
        assert!(!u.contains("SECRET"), "{u}");
    }

    /// A key lifted from `?api_key=` of a bearer profile goes back as that parameter only.
    #[test]
    fn a_lifted_query_key_is_not_sent_as_bearer() {
        let mut p = crate::fake::profile();
        p.auth = AuthKind::Bearer;
        p.base_url = "https://h.test/v1?api_key=fake-SECRET-r107c".into();
        let key = crate::profile::lift_url_secret(&mut p);
        let cfg = HttpCfg {
            agent: build_agent(false, true, None).unwrap(),
            base: p.base_url.clone(),
            auth: p.auth.clone(),
            key,
            headers: vec![],
            header_secrets: vec![],
            sends_secret: true,
            guard: EgressGuard::default(),
        };
        let r = cfg.authed(Request::new("GET", format!("{}/models", cfg.base)));
        assert!(!r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Authorization")));
        for (n, v) in &r.headers {
            assert!(!v.contains("SECRET"), "{n}");
        }
        assert_eq!(r.url.split_once('?').map(|(_, q)| q), Some("api_key=fake-SECRET-r107c"));
        let e = cfg.scrub(&format!("{}: refused", r.url));
        assert!(!e.contains("SECRET"), "{e}");
    }

    /// A key held in the address (the store refused it) never goes out over plain http.
    #[test]
    fn a_key_left_in_the_address_counts_as_a_secret() {
        let mut p = crate::fake::profile();
        p.auth = AuthKind::Bearer;
        p.base_url = "http://h.test/v1?key=fake-SECRET-r111".into();
        let w = Box::new(crate::proto_chat::ChatWire);
        let h = HttpProvider::new(&p, None, EgressGuard::default(), w).unwrap();
        assert!(h.cfg.sends_secret);
        p.base_url = "http://h.test/v1".into();
        let w = Box::new(crate::proto_chat::ChatWire);
        let h = HttpProvider::new(&p, None, EgressGuard::default(), w).unwrap();
        assert!(!h.cfg.sends_secret, "no key, no secret");
    }

    #[test]
    fn body_reader_announces_once_and_stops_when_cancelled() {
        let (tx, rx) = mpsc::sync_channel::<Chunk>(4);
        let mut buf = [0u8; 8];
        let mut body = BodyReader {
            data: io::Cursor::new(b"abc".to_vec()),
            tx: tx.clone(),
            started: false,
        };
        assert_eq!(body.read(&mut buf).unwrap(), 3);
        assert!(matches!(rx.try_recv(), Ok(Chunk::Started)));
        assert_eq!(body.read(&mut buf).unwrap(), 0);
        assert!(rx.try_recv().is_err());
        drop(rx);
        let mut body = BodyReader {
            data: io::Cursor::new(b"abc".to_vec()),
            tx: tx.clone(),
            started: false,
        };
        assert!(body.read(&mut buf).is_err());
    }

    #[test]
    fn key_is_scrubbed_from_errors_inside_a_stream() {
        let chat = format!("data: {{\"error\":{{\"message\":\"invalid key {KEY}\"}}}}\n\n");
        let blocks = format!(
            "data: {{\"type\":\"error\",\"error\":{{\"message\":\"invalid key {KEY}\"}}}}\n\n"
        );
        for (protocol, body) in [
            (Protocol::ChatCompletions, chat),
            (Protocol::ContentBlocks, blocks),
        ] {
            let srv = serve(response("200 OK", "", body.as_bytes()), 0);
            let p = provider(&srv.base, protocol);
            let (r, _) = run(p.as_ref());
            assert!(matches!(r, Err(ProviderError::Protocol(_))), "{r:?}");
            let shown = format!("{r:?}");
            assert!(
                !shown.contains(KEY) && shown.contains("invalid key ***"),
                "{shown}"
            );
        }
    }

    #[test]
    fn shortening_a_stream_error_cannot_expose_part_of_the_key() {
        let pad = "x".repeat(MAX_ERROR_CHARS - 5);
        let body = format!("data: {{\"error\":{{\"message\":\"{pad}{KEY}\"}}}}\n\n");
        let srv = serve(response("200 OK", "", body.as_bytes()), 0);
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let shown = format!("{:?}", run(p.as_ref()).0);
        assert!(!shown.contains("sk-"), "{shown}");
    }

    #[test]
    fn secret_headers_are_scrubbed_like_the_key() {
        let srv = serve(
            response("500 Internal Server Error", "", b"bad hdr-secret given"),
            0,
        );
        let mut pf = fake::profile();
        pf.base_url = srv.base.clone();
        pf.extra_headers = vec![("x-api-key".to_string(), "hdr-secret".to_string())];
        let p = build_provider(&pf, None, EgressGuard::default()).unwrap();
        let shown = format!("{:?}", run(p.as_ref()).0);
        assert!(
            !shown.contains("hdr-secret") && shown.contains("bad ***"),
            "{shown}"
        );
    }

    #[test]
    fn a_key_is_refused_on_plain_http_off_the_machine() {
        let guard = EgressGuard {
            local_only: false,
            consented_host: Some("http://api.example.invalid".to_string()),
        };
        let mut pf = fake::profile();
        pf.base_url = "http://api.example.invalid/v1".to_string();
        pf.auth = AuthKind::Bearer;
        let p = build_provider(&pf, Some(Secret::new(KEY)), guard.clone()).unwrap();
        let blocked = |r: Result<(), ProviderError>| matches!(r, Err(ProviderError::Blocked(ref m)) if m.contains("plain http"));
        assert!(blocked(run(p.as_ref()).0));
        assert!(blocked(p.list_models().map(|_| ())));

        pf.auth = AuthKind::None;
        pf.extra_headers = vec![("x-api-key".to_string(), KEY.to_string())];
        let p = build_provider(&pf, None, guard).unwrap();
        assert!(blocked(run(p.as_ref()).0));
    }

    #[test]
    fn the_agent_refuses_plain_http_on_its_own_when_it_holds_a_key() {
        let e = build_agent(false, true, None)
            .unwrap()
            .send(Request::new("GET", "http://127.0.0.1:9/"))
            .err()
            .expect("refused");
        assert!(e.0.to_ascii_lowercase().contains("https only"), "{e}");
    }

    #[test]
    fn local_names_resolve_only_to_this_machine() {
        use ddc_net::native::loopback_only;
        assert!(loopback_only("127.0.0.1:80")
            .unwrap()
            .iter()
            .all(|a| a.ip().is_loopback()));
        assert!(loopback_only("[::1]:80")
            .unwrap()
            .iter()
            .all(|a| a.ip().is_loopback()));
        assert!(loopback_only("192.0.2.1:80").is_err());
    }

    #[test]
    fn a_cut_inside_the_last_line_is_a_network_error() {
        let srv = serve(response("200 OK", "", b"data: {\"choices\":[{\"de"), 0);
        let p = provider(&srv.base, Protocol::ChatCompletions);
        assert!(matches!(run(p.as_ref()).0, Err(ProviderError::Network(_))));
        // Complete JSON with an error in it stays a server error, not a retryable cut.
        let srv = serve(
            response("200 OK", "", b"data: {\"error\":{\"message\":\"boom\"}}"),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        assert!(matches!(run(p.as_ref()).0, Err(ProviderError::Protocol(_))));
    }

    #[test]
    fn an_endless_line_is_an_error_not_a_memory_leak() {
        let srv = serve(
            response(
                "200 OK",
                "",
                &vec![b'x'; crate::sse::MAX_EVENT_BYTES + 1024],
            ),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        assert!(matches!(run(p.as_ref()).0, Err(ProviderError::Protocol(_))));
    }

    #[test]
    fn chat_stream_end_to_end_sends_auth_and_body() {
        let srv = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let (r, events) = run(p.as_ref());
        assert_eq!(r, Ok(()));
        let text: String = events
            .iter()
            .filter_map(|e| {
                if let StreamEvent::TextDelta(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, "\u{041d}\u{0430}\u{0439}\u{0434}\u{0435}\u{043d}\u{043e} 412 \u{0434}\u{0432}\u{0435}\u{0440}\u{0435}\u{0439}.");
        assert!(events.contains(&StreamEvent::Usage(Usage {
            input_tokens: 120,
            output_tokens: 9,
            ..Usage::default()
        })));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
        let seen = srv.seen.lock().unwrap();
        assert!(
            seen[0].starts_with("POST /v1/chat/completions "),
            "{}",
            seen[0]
        );
        assert!(seen[0]
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {KEY}")));
        assert!(seen[0].contains("\"stream\":true"));
    }

    #[test]
    fn blocks_stream_uses_its_own_path() {
        let srv = serve(
            response(
                "200 OK",
                "",
                include_bytes!("../tests/data/blocks_text.sse"),
            ),
            0,
        );
        let p = provider(&srv.base, Protocol::ContentBlocks);
        let (r, events) = run(p.as_ref());
        assert_eq!(r, Ok(()));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Finish(FinishReason::Stop))
        );
        assert!(srv.seen.lock().unwrap()[0].starts_with("POST /v1/messages "));
    }

    #[test]
    fn header_auth_and_extra_headers_are_sent() {
        let srv = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let mut pf = fake::profile();
        pf.base_url = srv.base.clone();
        pf.auth = AuthKind::Header {
            name: "x-key".to_string(),
        };
        pf.extra_headers = vec![("x-version".to_string(), "7".to_string())];
        let p = build_provider(&pf, Some(Secret::new(KEY)), EgressGuard::default()).unwrap();
        assert_eq!(run(p.as_ref()).0, Ok(()));
        let seen = srv.seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(seen.contains(&format!("x-key: {KEY}")));
        assert!(seen.contains("x-version: 7"));
        assert!(!seen.contains("authorization:"));
    }

    #[test]
    fn statuses_map_to_typed_errors() {
        let cases: Vec<(Vec<u8>, ProviderError)> = vec![
            (response("401 Unauthorized", "", b"no"), ProviderError::Auth),
            (response("403 Forbidden", "", b"no"), ProviderError::Auth),
            (
                response("429 Too Many Requests", "Retry-After: 7\r\n", b"slow"),
                ProviderError::RateLimited {
                    retry_after_s: Some(7),
                },
            ),
            (
                response("429 Too Many Requests", "", b"slow"),
                ProviderError::RateLimited {
                    retry_after_s: None,
                },
            ),
            (
                response("503 Service Unavailable", "Retry-After: 3\r\n", b"busy"),
                ProviderError::RateLimited {
                    retry_after_s: Some(3),
                },
            ),
            (
                response("529 Overloaded", "", b"busy"),
                ProviderError::RateLimited {
                    retry_after_s: None,
                },
            ),
            (
                response("400 Bad Request", "", b"maximum context length is 10"),
                ProviderError::ContextTooLong,
            ),
            (
                response("400 Bad Request", "", b"this model does not support tools"),
                ProviderError::NoTools,
            ),
            (
                response("404 Not Found", "", b"nope"),
                ProviderError::Http {
                    status: 404,
                    body: "nope".to_string(),
                },
            ),
        ];
        for (resp, want) in cases {
            let srv = serve(resp, 0);
            let p = provider(&srv.base, Protocol::ChatCompletions);
            assert_eq!(run(p.as_ref()).0, Err(want));
        }
    }

    #[test]
    fn key_is_scrubbed_from_error_bodies() {
        let body = format!("bad key {KEY} given");
        let srv = serve(
            response("500 Internal Server Error", "", body.as_bytes()),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let (r, _) = run(p.as_ref());
        let shown = format!("{:?} {}", r, r.clone().unwrap_err());
        assert!(!shown.contains(KEY), "{shown}");
        assert!(shown.contains("bad key ***"), "{shown}");
    }

    #[test]
    fn redirect_is_not_followed() {
        let target = serve(
            response("200 OK", "", include_bytes!("../tests/data/chat_text.sse")),
            0,
        );
        let hop = format!("Location: {}/chat/completions\r\n", target.base);
        let srv = serve(response("302 Found", &hop, b""), 0);
        let p = provider(&srv.base, Protocol::ChatCompletions);
        assert!(matches!(
            run(p.as_ref()).0,
            Err(ProviderError::Http { status: 302, .. })
        ));
        assert_eq!(target.hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn guard_blocks_before_any_connection() {
        let mut pf = fake::profile();
        pf.base_url = "http://api.example.invalid/v1".to_string();
        let guard = EgressGuard {
            local_only: true,
            consented_host: None,
        };
        let p = build_provider(&pf, None, guard).unwrap();
        assert!(matches!(run(p.as_ref()).0, Err(ProviderError::Blocked(_))));
        assert!(matches!(p.list_models(), Err(ProviderError::Blocked(_))));
    }

    #[test]
    fn stalled_server_is_cancelled_promptly() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n".to_vec();
        let srv = serve(head, 5000);
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let msgs = vec![Message::text(Role::User, "hi")];
        let req = ChatRequest {
            model: "m",
            messages: &msgs,
            tools: &[],
            max_output_tokens: 10,
            temperature: None,
        };
        let cancel_at = Instant::now() + Duration::from_millis(300);
        let r = p.stream(&req, &mut |_| {
            if Instant::now() >= cancel_at {
                Flow::Stop
            } else {
                Flow::Continue
            }
        });
        assert_eq!(r, Err(ProviderError::Cancelled));
        // The contract is 100 ms; the margin here is for loaded test machines.
        assert!(Instant::now().duration_since(cancel_at) < Duration::from_millis(500));
    }

    #[test]
    fn truncated_stream_is_a_network_error() {
        let srv = serve(
            response(
                "200 OK",
                "",
                b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n",
            ),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        let (r, events) = run(p.as_ref());
        assert!(matches!(r, Err(ProviderError::Network(_))), "{r:?}");
        assert_eq!(events, vec![StreamEvent::TextDelta("x".to_string())]);
    }

    #[test]
    fn list_models_reads_both_shapes() {
        let srv = serve(
            response("200 OK", "", br#"{"data":[{"id":"b"},{"id":"a"}]}"#),
            0,
        );
        let p = provider(&srv.base, Protocol::ChatCompletions);
        assert_eq!(
            p.list_models().unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(srv.seen.lock().unwrap()[0].starts_with("GET /v1/models "));
        let doc = serde_json::json!({ "models": [{ "name": "local:1" }] });
        assert_eq!(parse_models(&doc), vec!["local:1".to_string()]);
        assert!(parse_models(&serde_json::json!({})).is_empty());
    }
}
