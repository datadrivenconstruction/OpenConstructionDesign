// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Connection profiles, stored in `<data_dir>/ai_profiles.json` without keys.

use crate::egress::EgressGuard;
use crate::http::HttpProvider;
use crate::proto_blocks::BlocksWire;
use crate::proto_chat::ChatWire;
use crate::proto_text_tools::TextToolsProvider;
use crate::provider::{Provider, ProviderError};
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    ChatCompletions,
    ContentBlocks,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthKind {
    None,
    Bearer,
    Header { name: String },
    /// The key goes as the query parameter `name`; the stored address never holds it.
    Query { name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCalling {
    Native,
    JsonInText,
}

/// A protocol's mandatory headers (for example a version header) come through
/// `extra_headers`: the code names no service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub auth: AuthKind,
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
    pub tool_calling: ToolCalling,
    pub images: bool,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub price_in_per_mtok: Option<f64>,
    #[serde(default)]
    pub price_out_per_mtok: Option<f64>,
    /// The executor role on another model of the same connection; None: every step on `model`.
    #[serde(default)]
    pub executor: Option<Executor>,
}

/// Roles of a turn: the planner (the chosen `model`) reads the request and plans in the first
/// step; the executor runs the steps after a tool round. Same endpoint and key, any provider.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Executor {
    pub model: String,
    #[serde(default)]
    pub price_in_per_mtok: Option<f64>,
    #[serde(default)]
    pub price_out_per_mtok: Option<f64>,
}

impl Profile {
    /// The model of a step: the executor's when one is set and this is not the planning step.
    pub fn model_for(&self, role: StepRole) -> &str {
        match (role, &self.executor) {
            (StepRole::Executor, Some(e)) if !e.model.trim().is_empty() => e.model.trim(),
            _ => &self.model,
        }
    }

    /// Prices of a step's role (per million tokens, in and out); an executor without its own
    /// prices is billed at the profile's.
    pub fn prices_for(&self, role: StepRole) -> (Option<f64>, Option<f64>) {
        let base = (self.price_in_per_mtok, self.price_out_per_mtok);
        match (role, &self.executor) {
            (StepRole::Executor, Some(e)) if !e.model.trim().is_empty() => (
                e.price_in_per_mtok.or(base.0),
                e.price_out_per_mtok.or(base.1),
            ),
            _ => base,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRole {
    Planner,
    Executor,
}

impl StepRole {
    pub fn as_str(self) -> &'static str {
        match self {
            StepRole::Planner => "planner",
            StepRole::Executor => "executor",
        }
    }
}

pub fn build_provider(
    p: &Profile,
    key: Option<Secret>,
    guard: EgressGuard,
) -> Result<Arc<dyn Provider>, ProviderError> {
    if p.auth != AuthKind::None && key.is_none() {
        return Err(ProviderError::Auth);
    }
    let http = match p.protocol {
        Protocol::ChatCompletions => HttpProvider::new(p, key, guard, Box::new(ChatWire))?,
        Protocol::ContentBlocks => HttpProvider::new(p, key, guard, Box::new(BlocksWire))?,
    };
    let provider: Arc<dyn Provider> = Arc::new(http);
    if p.tool_calling == ToolCalling::Native {
        return Ok(provider);
    }
    let wrapped: Arc<dyn Provider> = Arc::new(TextToolsProvider::new(provider));
    Ok(wrapped)
}

#[derive(Serialize, Deserialize)]
struct ProfilesFile {
    version: u32,
    profiles: Vec<Profile>,
}

pub fn profiles_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ai_profiles.json")
}

/// A missing file is an empty list.
pub fn load_profiles(data_dir: &Path) -> io::Result<Vec<Profile>> {
    let text = match std::fs::read_to_string(profiles_path(data_dir)) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let file: ProfilesFile =
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(file.profiles)
}

/// A header name that usually carries a credential. Such a value belongs in the key
/// store, and on the wire it is treated like the key itself.
pub fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "auth",
        "key",
        "token",
        "secret",
        "cookie",
        "passw",
        "credential",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// A header name that labels an id or a setting (organization, project, request), not a
/// credential: a long random-looking value there is an id, only a key-shaped one is a key.
fn names_an_id(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "org",
        "project",
        "tenant",
        "account",
        "workspace",
        "request",
        "trace",
        "version",
        "region",
        "deployment",
        "agent",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// A header that carries a credential, by its name or by its value (a bearer or signed
/// token, a key prefix, a long random-looking string): it never goes to disk in plain text.
/// Under an id-like name only a key-shaped value counts, so an organization or project id
/// keeps a working profile working.
pub fn header_is_secret(name: &str, value: &str) -> bool {
    use crate::secret::{value_is_key_shaped, value_looks_secret};
    let by_value = if names_an_id(name) {
        value_is_key_shaped(value)
    } else {
        value_looks_secret(value)
    };
    is_secret_header(name) || by_value
}

/// Where a list that could not be read went, and whether its key-like text was masked.
#[derive(Debug)]
pub struct Damaged {
    pub path: PathBuf,
    pub scrubbed: bool,
}

/// Moves an unreadable list to `_archive` under a name that warns it may hold keys, with
/// key-like text masked, so names and addresses can still be read back by hand. The masked
/// copy is in place before the plain file goes, so no plain copy ever lands in `_archive`
/// (a failed write or a crash leaves the file where it was, an error says so).
pub fn archive_damaged(data_dir: &Path) -> io::Result<Damaged> {
    let src = profiles_path(data_dir);
    let bytes = std::fs::read(&src)?;
    let dir = data_dir.join("_archive");
    std::fs::create_dir_all(&dir)?;
    let stamp = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let name = |n: u32| match n {
        0 => format!("ai_profiles.damaged-may-hold-keys-{stamp}.json"),
        _ => format!("ai_profiles.damaged-may-hold-keys-{stamp}-{n}.json"),
    };
    let mut n = 0;
    while dir.join(name(n)).exists() {
        n += 1;
    }
    let path = dir.join(name(n));
    let clean = crate::secret::scrub_key_like(&String::from_utf8_lossy(&bytes));
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, clean).and_then(|()| std::fs::rename(&tmp, &path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // its masked copy is kept, so only the plain key text is gone
    std::fs::remove_file(&src)?;
    Ok(Damaged {
        path,
        scrubbed: true,
    })
}

/// `scheme://host:port` of an address: no userinfo, path, query or fragment. What the
/// journal keeps of a service address.
pub fn url_origin(url: &str) -> String {
    let t = url.trim();
    let cut = |s: &str| {
        let end = s
            .find(|c: char| matches!(c, '/' | '?' | '#'))
            .unwrap_or(s.len());
        s[..end].to_string()
    };
    match t.split_once("://") {
        Some((scheme, rest)) => {
            let authority = cut(rest);
            let host = authority.rsplit_once('@').map_or(authority.as_str(), |(_, h)| h);
            format!("{scheme}://{host}")
        }
        // no scheme: `user:pass@host` still loses its userinfo
        None => {
            let authority = cut(t);
            let host = authority.rsplit_once('@').map_or(authority.as_str(), |(_, h)| h);
            crate::secret::redact_url_secrets(host)
        }
    }
}

/// What `split_url_secret` took out of an address.
#[derive(PartialEq, Eq)]
pub struct UrlSecret {
    /// The address without userinfo and credential-like query parameters.
    pub clean: String,
    /// The query parameter that carried the key, when it came from the query.
    pub param: Option<String>,
    /// The key itself; empty when the parameter had no value.
    pub value: String,
}

impl std::fmt::Debug for UrlSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UrlSecret({}, {:?}, ***)", self.clean, self.param)
    }
}

/// Splits the key out of an address typed with one (`?key=...`, `user:pass@`); `None`
/// for an address without one.
pub fn split_url_secret(url: &str) -> Option<UrlSecret> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let a_end = rest
        .find(|c: char| matches!(c, '/' | '?' | '#'))
        .unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(a_end);
    let (host, userinfo) = match authority.rsplit_once('@') {
        Some((u, h)) => (h, Some(u)),
        None => (authority, None),
    };
    let (before_frag, frag) = match tail.find('#') {
        Some(i) => tail.split_at(i),
        None => (tail, ""),
    };
    let (path, query) = before_frag.split_once('?').unwrap_or((before_frag, ""));
    let mut kept = Vec::new();
    let mut found: Option<(String, String)> = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if crate::secret::is_credential_param(k) {
            if found.as_ref().map_or(true, |(_, fv)| fv.is_empty()) {
                found = Some((k.to_string(), v.to_string()));
            }
        } else {
            kept.push(pair);
        }
    }
    if userinfo.is_none() && found.is_none() {
        return None;
    }
    let mut clean = if url.contains("://") {
        format!("{scheme}://{host}{path}")
    } else {
        format!("{host}{path}")
    };
    if !kept.is_empty() {
        clean.push('?');
        clean.push_str(&kept.join("&"));
    }
    clean.push_str(frag);
    let from_userinfo = userinfo.map(|u| match u.split_once(':') {
        Some((_, pass)) if !pass.is_empty() => pass.to_string(),
        Some((user, _)) => user.to_string(),
        None => u.to_string(),
    });
    let (param, value) = match found {
        Some((k, v)) if !v.is_empty() || from_userinfo.is_none() => (Some(k), v),
        Some((k, _)) => (Some(k), from_userinfo.unwrap_or_default()),
        None => (None, from_userinfo.unwrap_or_default()),
    };
    Some(UrlSecret {
        clean,
        param,
        value,
    })
}

/// Takes a key typed into the address out of it; the caller puts the returned key in the store.
/// A key that came as a query parameter goes back as the same parameter, whatever the key mode
/// was: a service that takes `?key=` refuses it as a bearer or header.
pub fn lift_url_secret(p: &mut Profile) -> Option<Secret> {
    let s = split_url_secret(&p.base_url)?;
    p.base_url = s.clean;
    adopt_url_param(p, s.param.as_deref(), !s.value.is_empty());
    (!s.value.is_empty()).then(|| Secret::new(s.value))
}

/// The key mode after a key was lifted from the parameter `param`. `used`: the lifted key
/// becomes the profile key (otherwise only a key-less profile changes mode).
pub fn adopt_url_param(p: &mut Profile, param: Option<&str>, used: bool) {
    if let Some(name) = param {
        if used || p.auth == AuthKind::None {
            p.auth = AuthKind::Query {
                name: name.to_string(),
            };
        }
    }
}

/// Written through a temp file, so a crash cannot leave half a list. The file holds no
/// keys, so a profile with a credential-like header or an address carrying a key is
/// refused instead of written.
pub fn save_profiles(data_dir: &Path, profiles: &[Profile]) -> io::Result<()> {
    if profiles.iter().any(|p| split_url_secret(&p.base_url).is_some()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the service address carries a key: put it in the key field instead",
        ));
    }
    if let Some((name, _)) = profiles
        .iter()
        .flat_map(|p| p.extra_headers.iter())
        .find(|(k, v)| header_is_secret(k, v))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("header {name} looks like a credential: use the profile key instead"),
        ));
    }
    std::fs::create_dir_all(data_dir)?;
    let file = ProfilesFile {
        version: 1,
        profiles: profiles.to_vec(),
    };
    let text = serde_json::to_string_pretty(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let path = profiles_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    let done = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-ai-test-{}-{tag}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn missing_file_is_an_empty_list() {
        let d = temp_dir("missing").join("nothing-here");
        assert!(load_profiles(&d).unwrap().is_empty());
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let d = temp_dir("round");
        let mut p = fake::profile();
        p.auth = AuthKind::Header {
            name: "x-key".to_string(),
        };
        p.extra_headers = vec![("x-version".to_string(), "1".to_string())];
        p.price_in_per_mtok = Some(1.5);
        save_profiles(&d, &[p.clone()]).unwrap();
        assert_eq!(load_profiles(&d).unwrap(), vec![p]);
    }

    #[test]
    fn corrupt_file_is_an_error_not_an_empty_list() {
        let d = temp_dir("corrupt");
        std::fs::write(profiles_path(&d), "{not json").unwrap();
        assert_eq!(
            load_profiles(&d).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn credential_headers_are_never_written_to_the_file() {
        let d = temp_dir("secret-header");
        let mut p = fake::profile();
        p.extra_headers = vec![("X-Api-Key".to_string(), "sk-1".to_string())];
        assert_eq!(
            save_profiles(&d, &[p]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(load_profiles(&d).unwrap().is_empty());
        for name in ["Authorization", "x-auth", "session-token", "Cookie"] {
            assert!(is_secret_header(name), "{name}");
        }
        for name in ["x-version", "content-type", "x-request-source"] {
            assert!(!is_secret_header(name), "{name}");
        }
    }

    // Fixtures only: shaped like keys, never real ones.
    const FAKE_BEARER: &str = "Bearer Qz7fixtureR114bXk2Lp9W";
    const FAKE_SK: &str = "sk-fixture-r114b-not-a-real-key";

    /// A key under an innocent header name is refused by its value; a plain header
    /// with an ordinary value is still written.
    #[test]
    fn key_shaped_header_values_are_never_written() {
        let d = temp_dir("value-header");
        let _ = std::fs::remove_file(profiles_path(&d));
        for (name, value) in [("x-trace", FAKE_BEARER), ("x-custom", FAKE_SK)] {
            let mut p = fake::profile();
            p.extra_headers = vec![(name.to_string(), value.to_string())];
            let e = save_profiles(&d, &[p]).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
            assert!(!e.to_string().contains(value), "the error names no key");
            assert!(!profiles_path(&d).exists(), "nothing was written");
        }
        let mut p = fake::profile();
        p.extra_headers = vec![
            ("x-version".to_string(), "2023-06-01".to_string()),
            ("x-request-source".to_string(), "desktop-editor".to_string()),
            // an id under an id-like name, as dense as a key, is still an id
            ("x-organization".to_string(), "org-Qz7fixtureR115aXk2Lp9W".to_string()),
            ("x-project".to_string(), "fixture0r115a0project0id0not0a0key".to_string()),
        ];
        save_profiles(&d, &[p.clone()]).unwrap();
        assert_eq!(load_profiles(&d).unwrap(), vec![p], "a plain header is kept");
        for (name, value) in [("x-organization", FAKE_SK), ("x-project", FAKE_BEARER)] {
            assert!(header_is_secret(name, value), "a key-shaped value under {name}");
        }
    }

    /// When the masked copy cannot be written the plain file stays where it was and
    /// `_archive` gets no copy of it.
    #[test]
    fn a_failed_archive_leaves_no_plain_copy_in_the_archive() {
        let d = temp_dir("damaged-fail");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("_archive")).unwrap();
        let text = format!("{{\"profiles\":[[\"x-custom\",\"{FAKE_SK}\"]");
        std::fs::write(profiles_path(&d), &text).unwrap();
        // a folder where the masked copy's temp file would go makes the write fail
        let stamp = |s: u64| format!("ai_profiles.damaged-may-hold-keys-{s}.json.tmp");
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for s in now..now + 5 {
            std::fs::create_dir_all(d.join("_archive").join(stamp(s))).unwrap();
        }
        assert!(archive_damaged(&d).is_err());
        assert_eq!(std::fs::read_to_string(profiles_path(&d)).unwrap(), text, "left as is");
        for f in all_files(&d.join("_archive")) {
            let t = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).to_string();
            assert!(!t.contains(FAKE_SK), "{} holds the fixture key", f.display());
        }
    }

    /// Every file under `d`, `_archive` included.
    fn all_files(d: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(d).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                out.extend(all_files(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    /// An unreadable list goes to `_archive` under a warning name with its keys
    /// masked; no file under the data folder keeps one, and names and addresses stay.
    #[test]
    fn an_unreadable_list_is_archived_with_its_keys_masked() {
        const HDR: &str = "fake-SECRETSECRET-r114a-hdr";
        const ADDR: &str = "fake-SECRETSECRET-r114a-addr";
        const JWT: &str = "eyJfixture.eyJub3RyZWFs.r114aSig";
        let d = temp_dir("damaged");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let text = format!(
            "{{\"version\":1,\"profiles\":[{{\"id\":\"p1\",\"name\":\"Office server\",\
             \"base_url\":\"https://models.example.test/v1?key={ADDR}\",\
             \"extra_headers\":[[\"X-Api-Key\",\"{HDR}\"],[\"x-trace\",\"{FAKE_BEARER}\"],\
             [\"x-version\",\"2023-06-01\"],[\"x-sig\",\"{JWT}\"]],\"note\":\"{FAKE_SK}"
        );
        std::fs::write(profiles_path(&d), &text).unwrap();
        assert_eq!(
            load_profiles(&d).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let got = archive_damaged(&d).unwrap();
        assert!(got.scrubbed);
        assert_eq!(got.path.parent().unwrap(), d.join("_archive"));
        let name = got.path.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.contains("may-hold-keys"), "{name}");
        assert!(!profiles_path(&d).exists(), "moved, not copied");
        let files = all_files(&d);
        assert_eq!(files, vec![got.path.clone()], "only the archived copy is left");
        let bearer = FAKE_BEARER.trim_start_matches("Bearer ");
        for f in &files {
            let t = String::from_utf8_lossy(&std::fs::read(f).unwrap()).to_string();
            for fake in [HDR, ADDR, JWT, bearer, FAKE_SK] {
                assert!(!t.contains(fake), "{} keeps a fixture key", f.display());
            }
            for kept in ["Office server", "https://models.example.test/v1", "2023-06-01"] {
                assert!(t.contains(kept), "{kept} is gone");
            }
        }
        // a second damaged list gets its own name, the first copy is kept
        std::fs::write(profiles_path(&d), "{not json").unwrap();
        let second = archive_damaged(&d).unwrap();
        assert_ne!(second.path, got.path);
        assert!(got.path.exists() && second.path.exists());
        assert!(load_profiles(&d).unwrap().is_empty(), "the next start has an empty list");
    }

    const FAKE: &str = "SECRETSECRET";

    #[test]
    fn an_address_with_a_key_is_refused_by_save() {
        let d = temp_dir("url-key");
        for url in [
            format!("https://h.test/v1?key={FAKE}"),
            format!("https://h.test/v1?x=1&api-key={FAKE}"),
            format!("https://u:{FAKE}@h.test/v1"),
        ] {
            let mut p = fake::profile();
            p.base_url = url.clone();
            let e = save_profiles(&d, &[p]).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{url}");
            assert!(e.to_string().contains("key field") && !e.to_string().contains(FAKE));
        }
        assert!(!std::fs::read_to_string(profiles_path(&d)).unwrap_or_default().contains(FAKE));
    }

    #[test]
    fn the_key_is_split_out_of_the_address() {
        let s = split_url_secret(&format!("https://h.test:8443/v1?x=1&key={FAKE}#f")).unwrap();
        assert_eq!(s.clean, "https://h.test:8443/v1?x=1#f");
        assert_eq!((s.param.as_deref(), s.value.as_str()), (Some("key"), FAKE));
        assert!(!format!("{s:?}").contains(FAKE));
        let s = split_url_secret(&format!("https://me:{FAKE}@h.test/v1")).unwrap();
        assert_eq!(s.clean, "https://h.test/v1");
        assert_eq!((s.param, s.value.as_str()), (None, FAKE));
        assert!(split_url_secret("https://h.test/v1?max_tokens=5").is_none());
        assert!(split_url_secret("http://127.0.0.1:11434/v1").is_none());
    }

    #[test]
    fn a_lifted_key_is_sent_as_the_same_parameter() {
        let mut p = fake::profile();
        p.auth = AuthKind::None;
        p.base_url = format!("https://h.test/v1?key={FAKE}");
        let k = lift_url_secret(&mut p).unwrap();
        assert_eq!(k.expose(), FAKE);
        assert_eq!(p.base_url, "https://h.test/v1");
        assert_eq!(p.auth, AuthKind::Query { name: "key".into() });
        assert!(lift_url_secret(&mut p).is_none());
        let d = temp_dir("lifted");
        save_profiles(&d, &[p.clone()]).unwrap();
        assert_eq!(load_profiles(&d).unwrap(), vec![p]);
    }

    #[test]
    fn lifted_query_key_with_bearer_auth() {
        for auth in [AuthKind::Bearer, AuthKind::Header { name: "x-api-key".into() }] {
            let mut p = fake::profile();
            p.auth = auth;
            p.base_url = format!("https://h.test/v1?x=1&api_key={FAKE}");
            let k = lift_url_secret(&mut p).unwrap();
            assert_eq!(k.expose(), FAKE);
            assert_eq!(p.base_url, "https://h.test/v1?x=1");
            assert_eq!(p.auth, AuthKind::Query { name: "api_key".into() });
            assert!(!format!("{p:?}").contains(FAKE));
        }
        // a key from the user part of the address keeps the chosen key mode
        let mut p = fake::profile();
        p.auth = AuthKind::Bearer;
        p.base_url = format!("https://me:{FAKE}@h.test/v1");
        assert_eq!(lift_url_secret(&mut p).unwrap().expose(), FAKE);
        assert_eq!(p.auth, AuthKind::Bearer);
        // a key the caller keeps instead leaves a chosen mode alone
        let mut p = fake::profile();
        p.auth = AuthKind::Bearer;
        adopt_url_param(&mut p, Some("key"), false);
        assert_eq!(p.auth, AuthKind::Bearer);
    }

    #[test]
    fn the_journal_keeps_only_the_origin() {
        let url = format!("https://u:{FAKE}@h.test:8443/v1?key={FAKE}");
        assert_eq!(url_origin(&url), "https://h.test:8443");
        assert_eq!(url_origin("http://[::1]:1/v1"), "http://[::1]:1");
        assert!(!url_origin(&format!("h.test?key={FAKE}")).contains(FAKE));
        assert_eq!(url_origin(&format!("me:{FAKE}@h.test:1/v1")), "h.test:1");
    }

    #[test]
    fn key_required_when_auth_is_set() {
        let mut p = fake::profile();
        p.auth = AuthKind::Bearer;
        assert!(matches!(
            build_provider(&p, None, EgressGuard::default()),
            Err(ProviderError::Auth)
        ));
    }
}
