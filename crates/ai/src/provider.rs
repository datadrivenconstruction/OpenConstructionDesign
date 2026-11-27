// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Neutral chat types and the provider trait; wire formats live in `proto_*`.

pub use crate::gateway::{Balance, ModelInfo};
use serde_json::{json, Value as J};
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

/// One model answer may not hold more calls or larger arguments than this, whatever the
/// server claims. Anything over them ends the step instead of growing memory.
pub(crate) const MAX_CALLS_PER_STEP: usize = crate::limits::CALLS_PER_STEP.us();
pub(crate) const MAX_TOOL_ARGS_BYTES: usize = crate::limits::TOOL_ARGS_BYTES.us();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, PartialEq)]
pub struct ImageData {
    pub mime: &'static str,
    pub bytes: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

impl fmt::Debug for ImageData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ImageData({} {}x{}, {} bytes)",
            self.mime,
            self.width,
            self.height,
            self.bytes.len()
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    Image(ImageData),
    ToolCall {
        id: String,
        name: String,
        args: J,
    },
    ToolResult {
        id: String,
        content: String,
        images: Vec<ImageData>,
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Message {
            role,
            parts: vec![Part::Text(text.into())],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: J,
}

#[derive(Clone, Copy, Debug)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [ToolSchema],
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}

/// An empty `TextDelta` is a heartbeat: a stalled connection still gets the sink polled
/// about every 50 ms, which is what bounds cancellation latency. Consumers ignore it.
/// `Sent` is the request body as it goes on the wire, key removed and images as a size
/// note; it comes when the body starts leaving, so a blocked request never produces one.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Sent(String),
    TextDelta(String),
    ToolStart { id: String, name: String },
    ToolArgsDelta { id: String, json: String },
    ToolEnd { id: String },
    Usage(Usage),
    Finish(FinishReason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Filtered,
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// All input, cached or not: limits and totals count every token the model read.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The part of `input_tokens` read from the provider's prompt cache (billed at a discount).
    pub cache_read_tokens: u64,
    /// The part of `input_tokens` written to that cache (billed at a premium).
    pub cache_write_tokens: u64,
}

/// Lifetime of a cache entry the wire asks for. The write price below belongs to this value:
/// 1.25x for "5m", 2x for "1h". Change both together.
pub const CACHE_TTL: &str = "5m";
/// Cache prices as multiples of the plain input price (the published ones for this format).
pub const CACHE_READ_PRICE: f64 = 0.1;
pub const CACHE_WRITE_PRICE: f64 = 1.25;

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(other.cache_write_tokens);
    }

    /// Input tokens counted at the plain price: cache reads at 0.1, writes at 1.25 of it.
    /// A server that reports more cache than input cannot make this negative.
    pub fn input_price_units(&self) -> f64 {
        let read = self.cache_read_tokens.min(self.input_tokens);
        let write = self.cache_write_tokens.min(self.input_tokens - read);
        let fresh = self.input_tokens - read - write;
        fresh as f64 + read as f64 * CACHE_READ_PRICE + write as f64 * CACHE_WRITE_PRICE
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

fn retry_suffix(s: Option<u32>) -> String {
    s.map(|s| format!(" (retry after {s} s)")).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed")]
    Auth,
    #[error("rate limited{}", retry_suffix(*.retry_after_s))]
    RateLimited { retry_after_s: Option<u32> },
    #[error("request exceeds the model context")]
    ContextTooLong,
    #[error("model does not support tool calling")]
    NoTools,
    #[error("request blocked: {0}")]
    Blocked(String),
    /// The key's own spending limit is used up; the account may still have money.
    #[error("key spending limit reached: {0}")]
    KeyLimit(String),
    /// The account has no money left for this request.
    #[error("account balance used up: {0}")]
    NoCredit(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("http status {status}: {body}")]
    Http { status: u16, body: String },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("cancelled")]
    Cancelled,
}

/// `Flow::Stop` from the sink makes `stream` return `Err(ProviderError::Cancelled)`.
pub trait Provider: Send + Sync {
    fn stream(
        &self,
        req: &ChatRequest<'_>,
        sink: &mut dyn FnMut(StreamEvent) -> Flow,
    ) -> Result<(), ProviderError>;
    fn list_models(&self) -> Result<Vec<String>, ProviderError>;
    /// The model list with prices and context sizes where the server reports them.
    fn list_model_infos(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(self.list_models()?.into_iter().map(ModelInfo::named).collect())
    }
    /// Money left on the account and the key; `None` when the server does not say.
    fn balance(&self) -> Result<Option<Balance>, ProviderError> {
        Ok(None)
    }
}

/// Readable form of a request for the "what was sent" window: images become a size note.
pub fn dump_request(req: &ChatRequest<'_>) -> String {
    let messages: Vec<J> = req.messages.iter().map(dump_message).collect();
    let tools: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
    let doc = json!({ "model": req.model, "max_output_tokens": req.max_output_tokens, "tools": tools, "messages": messages });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

pub(crate) fn image_note(i: &ImageData) -> String {
    format!("[image {}x{}, {} bytes]", i.width, i.height, i.bytes.len())
}

fn dump_message(m: &Message) -> J {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let parts: Vec<J> = m
        .parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => json!(t),
            Part::Image(i) => json!(image_note(i)),
            Part::ToolCall { id, name, args } => json!({ "call": name, "id": id, "args": args }),
            Part::ToolResult {
                id,
                content,
                images,
                is_error,
            } => {
                let notes: Vec<String> = images.iter().map(image_note).collect();
                json!({ "result": id, "content": content, "images": notes, "is_error": is_error })
            }
        })
        .collect();
    json!({ "role": role, "parts": parts })
}

/// Every image a request carries, wherever it sits in the messages.
pub(crate) fn images_of(messages: &[Message]) -> Vec<&ImageData> {
    let mut out = Vec::new();
    for p in messages.iter().flat_map(|m| m.parts.iter()) {
        match p {
            Part::Image(i) => out.push(i),
            Part::ToolResult { images, .. } => out.extend(images.iter()),
            _ => {}
        }
    }
    out
}

/// Marks where program data starts and ends. Text written before this value existed
/// cannot contain it, so data cannot close its own block. Stable per conversation:
/// a new value per request would break prompt caching on local servers.
pub(crate) fn boundary_nonce() -> String {
    format!("{:016x}", RandomState::new().build_hasher().finish())
}

pub(crate) fn image_base64(i: &ImageData) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(i.bytes.as_slice())
}

pub(crate) fn join_text(parts: &[Part]) -> String {
    let mut out = String::new();
    for p in parts {
        if let Part::Text(t) = p {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_arithmetic_saturates() {
        let mut u = Usage {
            input_tokens: u64::MAX,
            output_tokens: 1,
            ..Usage::default()
        };
        assert_eq!(u.total(), u64::MAX);
        u.add(Usage {
            input_tokens: 5,
            output_tokens: u64::MAX,
            ..Usage::default()
        });
        assert_eq!(
            u,
            Usage {
                input_tokens: u64::MAX,
                output_tokens: u64::MAX,
                ..Usage::default()
            }
        );
    }

    #[test]
    fn cache_reads_are_cheap_and_writes_dear() {
        let u = Usage {
            input_tokens: 1000,
            output_tokens: 0,
            cache_read_tokens: 800,
            cache_write_tokens: 100,
        };
        // 100 fresh + 800 * 0.1 + 100 * 1.25
        assert!((u.input_price_units() - 305.0).abs() < 1e-9);
        let plain = Usage {
            input_tokens: 1000,
            ..Usage::default()
        };
        assert_eq!(plain.input_price_units(), 1000.0);
        // a server reporting more cache than input stays within the input
        let odd = Usage {
            input_tokens: 10,
            cache_read_tokens: 50,
            cache_write_tokens: 50,
            ..Usage::default()
        };
        assert!((odd.input_price_units() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn images_are_found_in_both_places() {
        let img = ImageData {
            mime: "image/png",
            bytes: Arc::new(vec![1]),
            width: 1,
            height: 1,
        };
        let msgs = vec![
            Message {
                role: Role::User,
                parts: vec![Part::Image(img.clone())],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    id: "a".to_string(),
                    content: String::new(),
                    images: vec![img],
                    is_error: false,
                }],
            },
        ];
        assert_eq!(images_of(&msgs).len(), 2);
    }

    #[test]
    fn boundary_values_differ() {
        assert_ne!(boundary_nonce(), boundary_nonce());
        assert_eq!(boundary_nonce().len(), 16);
    }
}
