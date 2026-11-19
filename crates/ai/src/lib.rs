// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI assistant core: providers, wire protocols, conversation loop, tool catalog, key storage.
//! No UI, no model access: tool execution lives behind `convo::ToolHost`.

pub mod catalog;
pub mod convo;
pub mod egress;
pub mod fake;
pub mod family_draft;
pub mod family_repair;
mod http;
pub mod journal;
pub mod mcp;
pub mod profile;
mod proto_blocks;
mod proto_chat;
mod proto_text_tools;
pub mod provider;
pub mod refusal;
pub mod registry;
pub mod schema;
pub mod secret;
pub mod skills;
mod sse;
pub mod toolset;

pub use convo::{
    char_quarter_tokens, context_budget, estimate_text_tokens, CallCx, CancelToken, Conversation, ConvoEvent, Limits, Mode, ToolHost, ToolOutcome, TurnEnd,
    UsageTotals,
};
pub use egress::{consent_key, EgressGuard};
pub use journal::{StepRecord, TaskJournal};
pub use profile::{build_provider, AuthKind, Executor, Profile, Protocol, StepRole, ToolCalling};
pub use provider::{
    ChatRequest, FinishReason, Flow, ImageData, Message, Part, Provider, ProviderError, Role,
    StreamEvent, ToolSchema, Usage,
};
pub use secret::{MemoryStore, OsStore, Secret, SecretError, SecretStore};

/// Cached input tokens (read, written) summed over the process; `Usage` folds them into input.
pub fn cache_totals() -> (u64, u64) {
    proto_blocks::cache_totals()
}

pub type J = serde_json::Value;
