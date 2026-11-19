// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
use std::fmt;

/// What went wrong talking to the ERP. Messages carry the ERP's `detail` text at most, never a
/// response body, a token or a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// 426, or an answer without a common contract major: the client stops talking to this ERP
    /// for the rest of the session and does not retry (contract §8). The ERP's majors.
    Unsupported { supported: Vec<u32> },
    /// The link file is not a valid `ddc-link/1`.
    BadLink(String),
    /// 410: the code was used or has expired; a new link is needed.
    CodeGone,
    /// 429: too many attempts; seconds to wait when the ERP says.
    Throttled { retry_after: Option<u64> },
    /// 401: the token is invalid or expired (the caller forgets it).
    Unauthorized,
    /// No token for this project in the key store.
    NoToken,
    /// 403 with the ERP's reason.
    Forbidden(String),
    /// 404 with the ERP's reason.
    NotFound(String),
    /// 409 on taking the lock: someone else edits the project.
    LockHeld { held_by: String, expires_at: String },
    /// 409: this editor no longer holds the lock (heartbeat or upload). Publish waits until the
    /// lock is taken again.
    LockLost,
    /// 409 on upload: the parent is not the chain tip any more; re-base and publish again.
    ParentMoved { latest: Option<String> },
    /// Another 409.
    Conflict(String),
    /// 422: the package was refused (hash, schema, content).
    Rejected(String),
    /// 413: the package is over the ERP's limit.
    TooLarge(String),
    /// Any other status.
    Server { status: u16, detail: String },
    /// No answer: address, network, TLS, timeout.
    Network(String),
    /// The answer is not what the contract says.
    Protocol(String),
    /// The OS key store failed or does not exist here.
    Store(String),
    /// A local file could not be read or written.
    Io(String),
}

impl LinkError {
    /// The ERP stays unusable for this session (426).
    pub fn is_stop(&self) -> bool {
        matches!(self, LinkError::Unsupported { .. })
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::Unsupported { supported } => {
                let list: Vec<String> = supported.iter().map(u32::to_string).collect();
                write!(
                    f,
                    "the ERP speaks link version {} and this editor speaks {}: update the editor (or the ERP)",
                    if list.is_empty() { "?".into() } else { list.join(", ") },
                    crate::LINK_VERSIONS.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
                )
            }
            LinkError::BadLink(m) => write!(f, "not a valid link file: {m}"),
            LinkError::CodeGone => f.write_str("the link was already used or has expired: open a new one from the ERP"),
            LinkError::Throttled { retry_after: Some(s) } => write!(f, "too many attempts: try again in {s} s"),
            LinkError::Throttled { retry_after: None } => f.write_str("too many attempts: try again later"),
            LinkError::Unauthorized => f.write_str("the ERP no longer accepts this editor's access: open a new link from the ERP"),
            LinkError::NoToken => f.write_str("no access to this ERP project is stored: open a link from the ERP"),
            LinkError::Forbidden(m) => write!(f, "not allowed: {m}"),
            LinkError::NotFound(m) => write!(f, "not found: {m}"),
            LinkError::LockHeld { held_by, expires_at } => write!(f, "{held_by} is editing this project (until {expires_at})"),
            LinkError::LockLost => f.write_str("the editing lock was lost: take it again before publishing"),
            LinkError::ParentMoved { .. } => f.write_str("a newer revision is in the ERP: publish again to build on it"),
            LinkError::Conflict(m) => write!(f, "conflict: {m}"),
            LinkError::Rejected(m) => write!(f, "the ERP refused the revision: {m}"),
            LinkError::TooLarge(m) => write!(f, "the revision is over the ERP's size limit: {m}"),
            LinkError::Server { status, detail } => write!(f, "ERP error {status}: {detail}"),
            LinkError::Network(m) => write!(f, "no connection to the ERP: {m}"),
            LinkError::Protocol(m) => write!(f, "unexpected answer from the ERP: {m}"),
            LinkError::Store(m) => write!(f, "key store: {m}"),
            LinkError::Io(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for LinkError {}
