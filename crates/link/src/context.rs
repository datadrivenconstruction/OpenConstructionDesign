// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `ddc-context/1` (`GET /projects/{id}/context`). Read-only types: none of them serializes,
//! so the AI block cannot be written anywhere by accident (the contract keeps it in memory).

use serde::Deserialize;
use serde_json::Value;

use crate::Secret;

#[derive(Clone, Debug, Deserialize)]
pub struct Context {
    pub schema: String,
    pub link_versions: Vec<u32>,
    pub project: Project,
    pub permissions: Permissions,
    pub lock: Option<Lock>,
    pub latest_revision: Option<LatestRevision>,
    /// The latest saved brief (`ddc-brief/1`), whole.
    pub brief: Option<Value>,
    #[serde(default)]
    pub ai: Option<AiProfile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub country: Option<String>,
    pub currency: Option<String>,
    pub locale: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub struct Permissions {
    pub can_publish: bool,
    pub can_accept: bool,
    pub can_generate: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Lock {
    pub held_by: String,
    pub mine: bool,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct LatestRevision {
    pub revision_id: String,
    pub number: u32,
    pub status: String,
}

/// The ERP user's own AI connection for this session. In memory only; `api_key` prints as `***`.
#[derive(Clone, Debug, Deserialize)]
pub struct AiProfile {
    /// Wire protocol: `chat-completions`, `messages` or `local`.
    pub protocol: String,
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<Secret>,
    #[serde(default)]
    pub local_only: bool,
    pub expires_at: String,
}
