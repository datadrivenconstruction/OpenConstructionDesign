// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The server external agents connect to: Streamable HTTP on loopback, a stdio bridge, the same
//! tool registry as the chat. The live model is reachable only through the editor's worker.

mod auth;
mod rpc;
mod server;
pub mod stdio;
pub mod test_client;
pub mod tools;
mod wire;

#[cfg(test)]
mod tests;

pub use auth::{generate_token, issue_token, load_token, reveal, TOKEN_ID};
pub use rpc::{LEGACY_VERSIONS, MODERN_VERSIONS};
pub use server::{serve, serve_with, ClientSeen, McpHandle, McpLimits};

use crate::catalog::{self, Client, Target, Tier, TierPolicy};
use crate::convo::{Mode, ToolOutcome};
use crate::secret::Secret;
use crate::J;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};

/// Every write of an external agent waits in its own draft for the person's Apply.
/// `AutoApply` is only what older projects stored under `mcp.policy` ("auto"): it is read as
/// `Confirm`, so such a file opens with drafts and nothing is rewritten on open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpPolicy {
    AutoApply,
    #[default]
    Confirm,
}

impl McpPolicy {
    /// Whatever is stored, an agent's write is a draft.
    pub fn from_setting(_value: Option<&str>) -> McpPolicy {
        McpPolicy::Confirm
    }

    pub fn setting(self) -> &'static str {
        match self {
            McpPolicy::AutoApply => "auto",
            McpPolicy::Confirm => "confirm",
        }
    }

    /// The same for every target: the main model and an option's copy alike.
    pub fn effective(self, _target: Target) -> McpPolicy {
        McpPolicy::Confirm
    }

    /// The tier ceiling of an MCP session: never above what a draft needs.
    pub fn ceiling(self, target: Target) -> Tier {
        let tiers = TierPolicy { auto_apply: false };
        catalog::ceiling(Client::Mcp, Mode::Propose, tiers, target)
    }
}

pub struct McpConfig {
    pub port: u16,
    pub token: Secret,
}

const WAITING: u8 = 0;
const STARTED: u8 = 1;
const ABANDONED: u8 = 2;

/// Keeps a call the client gave up on, or whose token was revoked, from reaching the model later.
#[derive(Clone, Debug)]
pub struct CallGate {
    state: Arc<AtomicU8>,
    revoked: Arc<AtomicBool>,
}

impl CallGate {
    pub fn new(revoked: Arc<AtomicBool>) -> Self {
        CallGate {
            state: Arc::new(AtomicU8::new(WAITING)),
            revoked,
        }
    }

    pub fn open() -> Self {
        Self::new(Arc::new(AtomicBool::new(false)))
    }

    /// The worker takes the call. False: the client gave up or access was revoked.
    pub fn start(&self) -> bool {
        !self.revoked()
            && self
                .state
                .compare_exchange(WAITING, STARTED, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }

    /// The client stops waiting. False: the worker already took the call, so its answer counts.
    pub fn abandon(&self) -> bool {
        self.state
            .compare_exchange(WAITING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn revoked(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }
}

pub struct McpCall {
    pub client: String,
    pub name: String,
    pub args: J,
    pub reply: mpsc::Sender<ToolOutcome>,
    pub gate: CallGate,
    /// The calling agent's language for refusal texts (`refusal::LANGS`), not the editor's.
    pub lang: &'static str,
}

impl McpCall {
    pub fn new(
        client: impl Into<String>,
        name: impl Into<String>,
        args: J,
        reply: mpsc::Sender<ToolOutcome>,
    ) -> Self {
        McpCall {
            client: client.into(),
            name: name.into(),
            args,
            reply,
            gate: CallGate::open(),
            lang: "en",
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn policy_defaults_to_confirm() {
        assert_eq!(McpPolicy::default(), McpPolicy::Confirm);
        for v in [None, Some("confirm"), Some("other"), Some(""), Some("auto")] {
            assert_eq!(McpPolicy::from_setting(v), McpPolicy::Confirm, "{v:?}");
        }
    }

    #[test]
    fn mcp_default_never_writes_main() {
        for p in [McpPolicy::default(), McpPolicy::AutoApply] {
            for t in [Target::Main, Target::Draft] {
                assert_eq!(p.effective(t), McpPolicy::Confirm);
                assert!(p.ceiling(t) <= Tier::T2Apply);
            }
        }
    }

    #[test]
    fn an_old_auto_value_reads_as_draft() {
        let p = McpPolicy::from_setting(Some("auto"));
        assert_eq!(p.effective(Target::Draft), McpPolicy::Confirm);
        assert_eq!(p.ceiling(Target::Draft), Tier::T2Apply);
    }

    #[test]
    fn a_gate_lets_exactly_one_side_win() {
        let g = CallGate::open();
        assert!(g.start());
        assert!(
            !g.abandon(),
            "the worker took it: the client must wait for the answer"
        );
        let g = CallGate::open();
        assert!(g.abandon());
        assert!(!g.start(), "the client gave up: the call must not run");
        let flag = Arc::new(AtomicBool::new(false));
        let g = CallGate::new(flag.clone());
        flag.store(true, Ordering::SeqCst);
        assert!(!g.start(), "a revoked token stops what is still queued");
    }
}
