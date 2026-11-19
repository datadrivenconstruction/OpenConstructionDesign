// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Where the project token lives: the OS key store, under `ddc-link/<erp host>/<project_id>`
//! (contract §7). Never a file.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{ErpUrl, LinkError, Secret};

/// Key-store id of a project's token. The host keeps its port: two ERPs on one machine
/// differ only by it.
pub fn store_id(erp: &ErpUrl, project_id: &str) -> String {
    format!("ddc-link/{}/{}", erp.authority, project_id)
}

pub trait TokenStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Option<Secret>, LinkError>;
    fn set(&self, id: &str, token: &Secret) -> Result<(), LinkError>;
    fn remove(&self, id: &str) -> Result<(), LinkError>;
}

/// The OS vault (Windows Credential Manager, macOS keychain). Elsewhere it refuses, and the
/// caller keeps the token for the session only.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsKeyring;

const SERVICE: &str = "ddc-link";

#[cfg(any(windows, target_os = "macos"))]
fn entry(id: &str) -> Result<keyring::Entry, LinkError> {
    // Windows: the credential's target name is the id itself, as the contract writes it.
    #[cfg(windows)]
    let e = keyring::Entry::new_with_target(id, SERVICE, id);
    #[cfg(not(windows))]
    let e = keyring::Entry::new(id, SERVICE);
    e.map_err(|e| LinkError::Store(e.to_string()))
}

#[cfg(any(windows, target_os = "macos"))]
impl TokenStore for OsKeyring {
    fn get(&self, id: &str) -> Result<Option<Secret>, LinkError> {
        match entry(id)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(LinkError::Store(e.to_string())),
        }
    }

    fn set(&self, id: &str, token: &Secret) -> Result<(), LinkError> {
        entry(id)?.set_password(token.expose()).map_err(|e| LinkError::Store(e.to_string()))
    }

    fn remove(&self, id: &str) -> Result<(), LinkError> {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(LinkError::Store(e.to_string())),
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
impl TokenStore for OsKeyring {
    fn get(&self, _: &str) -> Result<Option<Secret>, LinkError> {
        Ok(None)
    }

    fn set(&self, _: &str, _: &Secret) -> Result<(), LinkError> {
        Err(LinkError::Store("no key store on this system: the link lasts for this session".into()))
    }

    fn remove(&self, _: &str) -> Result<(), LinkError> {
        Ok(())
    }
}

/// Process memory only: tests, and a stand-in where no vault exists.
#[derive(Debug, Default)]
pub struct MemoryTokens(Mutex<HashMap<String, Secret>>);

impl MemoryTokens {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.0.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        v.sort();
        v
    }
}

impl TokenStore for MemoryTokens {
    fn get(&self, id: &str) -> Result<Option<Secret>, LinkError> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned())
    }

    fn set(&self, id: &str, token: &Secret) -> Result<(), LinkError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), token.clone());
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), LinkError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The real vault: write, read back, remove. The id is unique to this run.
    #[test]
    fn os_keyring_round_trip() {
        let erp = ErpUrl::parse("http://127.0.0.1:1").unwrap();
        let pid = format!("00000000-0000-4000-8000-{:012x}", std::process::id());
        let id = store_id(&erp, &pid);
        assert_eq!(id, format!("ddc-link/127.0.0.1:1/{pid}"));
        let s = OsKeyring;
        s.set(&id, &Secret::new("ddcl_TEST_ONLY_round_trip_value_0123456789")).unwrap();
        assert_eq!(s.get(&id).unwrap().unwrap().expose(), "ddcl_TEST_ONLY_round_trip_value_0123456789");
        s.remove(&id).unwrap();
        assert!(s.get(&id).unwrap().is_none());
        s.remove(&id).unwrap();
    }
}
