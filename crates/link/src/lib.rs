// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Client of the ERP link, contract `ddc-erp-link/1` (§7 link file, §8 HTTP surface).
//!
//! `.ddclink` file → one-time code → project token (OS key store only) → context, lock, revisions.
//! Nothing here touches the editor model; the editor calls it only while a link is open.
//! Secrets (code, token, the AI key of the context) are [`Secret`]s: no `Display`, `Debug`
//! prints `***`, no `Serialize`, and no error message ever quotes a response body.

mod client;
mod context;
mod error;
mod keeper;
mod linkfile;
#[cfg(feature = "mock-erp")]
pub mod mock;
mod secret;
mod token;

pub use client::{Client, ClientInfo, Exchanged, RevisionView, API, LINK_VERSIONS};
pub use context::{AiProfile, Context, LatestRevision, Lock, Permissions, Project};
pub use error::LinkError;
pub use keeper::{Keeper, LockState, HEARTBEAT};
pub use linkfile::{Action, ErpUrl, LinkFile};
pub use secret::Secret;
pub use token::{store_id, MemoryTokens, OsKeyring, TokenStore};

use std::path::Path;

/// A project the editor talks to: where, which, and the client holding its token.
#[derive(Clone, Debug)]
pub struct Linked {
    pub erp: ErpUrl,
    pub project_id: String,
    pub client: Client,
    /// The token could not go to the OS key store (none on this system, or it refused):
    /// it lives in this session only, and the link must be opened again after a restart.
    pub session_only: bool,
}

impl Linked {
    /// Key-store id of this project's token.
    pub fn store_id(&self) -> String {
        store_id(&self.erp, &self.project_id)
    }

    /// The ERP no longer accepts the token (401): it leaves the key store.
    pub fn forget(&self, store: &dyn TokenStore) -> Result<(), LinkError> {
        store.remove(&self.store_id())
    }
}

/// Opens a `.ddclink`: exchanges its code for a token, keeps the token in `store`, deletes the
/// file (the code is single use, contract §7). A used or expired code deletes the file too;
/// a network failure keeps it, so the user can try again within the code's five minutes.
pub fn connect(path: &Path, store: &dyn TokenStore, info: &ClientInfo) -> Result<(Linked, LinkFile), LinkError> {
    let text = std::fs::read_to_string(path).map_err(|e| LinkError::BadLink(format!("{}: {e}", path.display())))?;
    let link = LinkFile::parse(&text)?;
    let client = Client::new(&link.erp)?;
    let ex = match client.exchange(&link.code, info) {
        Ok(ex) => ex,
        Err(e) => {
            if matches!(e, LinkError::CodeGone) {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };
    if ex.project_id != link.project_id {
        return Err(LinkError::Protocol("the token is for another project than the link file".into()));
    }
    let _ = std::fs::remove_file(path);
    let id = store_id(&link.erp, &link.project_id);
    let session_only = store.set(&id, &ex.token).is_err();
    let linked = Linked {
        erp: link.erp.clone(),
        project_id: link.project_id.clone(),
        client: client.with_token(ex.token),
        session_only,
    };
    Ok((linked, link))
}

/// A project linked before: its token from `store`. `Ok(None)` when the store has none.
pub fn resume(erp: &ErpUrl, project_id: &str, store: &dyn TokenStore) -> Result<Option<Linked>, LinkError> {
    linkfile::check_uuid(project_id)?;
    let Some(token) = store.get(&store_id(erp, project_id))? else {
        return Ok(None);
    };
    Ok(Some(Linked {
        erp: erp.clone(),
        project_id: project_id.to_string(),
        client: Client::new(erp)?.with_token(token),
        session_only: false,
    }))
}
