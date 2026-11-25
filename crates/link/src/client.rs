// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! HTTP surface of the ERP module `design` (contract §8): version header on every call,
//! bearer token, the ERP error convention `{"detail": ...}` mapped to [`LinkError`].

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ddc_net::{Body, NetError, Policy, Request, Response, Transport};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{Context, ErpUrl, LinkError, Lock, Secret};

/// Prefix of the module's routes under the ERP address.
pub const API: &str = "/api/v1/design";
/// Contract majors this client speaks, sent as `X-DDC-Link`.
pub const LINK_VERSIONS: &[u32] = &[1];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// The ERP ingests the package before it answers the upload.
const UPLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);
const ERROR_BODY_MAX: u64 = 1 << 20;
const JSON_BODY_MAX: u64 = 64 << 20;
/// A revision file larger than this is refused rather than held in memory.
const FILE_MAX: u64 = 4 << 30;

/// Who is calling, sent once with the code exchange.
#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
    pub os: String,
}

impl ClientInfo {
    pub fn editor(version: &str) -> Self {
        ClientInfo { name: "ddc-editor".into(), version: version.chars().take(40).collect(), os: std::env::consts::OS.into() }
    }
}

/// `POST /link/exchange` 200.
#[derive(Debug)]
pub struct Exchanged {
    pub token: Secret,
    pub expires_at: String,
    pub project_id: String,
}

#[derive(Deserialize)]
struct ExchangeBody {
    token: Secret,
    expires_at: String,
    project_id: String,
}

/// `revision-view` (contract `schemas/revision-view.schema.json`), the fields the editor reads.
#[derive(Clone, Debug, Deserialize)]
pub struct RevisionView {
    pub revision_id: String,
    pub project_id: String,
    pub number: u32,
    pub parent_revision_id: Option<String>,
    pub status: String,
    pub source_kind: String,
    pub brief_version: Option<i64>,
    /// `null` only for an imported model wrapped without a stated level (contract §9a).
    pub depth_level: Option<String>,
    pub message: String,
    pub bim_model_id: Option<String>,
    pub package_sha256: String,
    pub package_bytes: u64,
    pub created_at: String,
    pub manifest: Value,
}

impl RevisionView {
    /// Waiting for acceptance or accepted: the only states a new revision may build on.
    pub fn in_play(&self) -> bool {
        self.status == "shared" || self.status == "published"
    }

    /// A converter model wrapped by the ERP: its manifest is a `ddc-import/1` record with no files,
    /// so there is no `model.ddc` to open and no element table to diff against.
    pub fn imported(&self) -> bool {
        self.manifest["schema"] == "ddc-import/1"
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    Exchange,
    TakeLock,
    Heartbeat,
    Upload,
    Other,
}

/// One ERP, one token. Clones share the stop flag: a 426 on any of them stops all.
#[derive(Clone)]
pub struct Client {
    base: String,
    erp: ErpUrl,
    net: Arc<dyn Transport>,
    token: Option<Secret>,
    stopped: Arc<Mutex<Option<Vec<u32>>>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("token", &self.token)
            .field("stopped", &self.stopped_with())
            .finish()
    }
}

/// No redirects: a bearer token must not follow one to another host. A loopback ERP is reached
/// directly and only on this machine, never through a proxy.
fn transport(erp: &ErpUrl, read: Duration) -> Result<Arc<dyn Transport>, LinkError> {
    let p = Policy {
        https_only: !erp.loopback,
        loopback_only: erp.loopback,
        redirects: 0,
        connect_timeout: CONNECT_TIMEOUT,
        read_timeout: read,
        write_timeout: WRITE_TIMEOUT,
    };
    ddc_net::transport(&p).map_err(|e| LinkError::Network(e.0))
}

fn versions_header() -> String {
    LINK_VERSIONS.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

/// `{"detail": ...}` as one line: a string, a list of validation items, or an object's message.
fn detail_text(d: &Value) -> String {
    let s = match d {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|i| match (i["loc"].as_array(), i["msg"].as_str()) {
                (Some(loc), Some(m)) => {
                    let at: Vec<String> = loc.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), String::from)).collect();
                    format!("{}: {m}", at.join("."))
                }
                (_, Some(m)) => m.to_string(),
                _ => i.as_str().map_or_else(|| i.to_string(), String::from),
            })
            .collect::<Vec<_>>()
            .join("; "),
        Value::Object(o) => o.get("message").and_then(Value::as_str).map_or_else(|| d.to_string(), String::from),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    s.chars().take(500).collect()
}

/// Largest page the ERP serves for the revision list.
const REVISION_PAGE: usize = 500;

/// One page of the revision list: its items and the total when the answer is the paged envelope.
fn revision_page(v: Value) -> Result<(Vec<RevisionView>, Option<usize>), LinkError> {
    let bad = |_| LinkError::Protocol("the revision list does not follow the contract".into());
    match v {
        Value::Array(_) => Ok((serde_json::from_value(v).map_err(bad)?, None)),
        Value::Object(mut o) => {
            let total = o.get("total").and_then(Value::as_u64).map(|t| t as usize);
            let items = serde_json::from_value(o.remove("items").unwrap_or(Value::Null)).map_err(bad)?;
            Ok((items, total))
        }
        _ => Err(LinkError::Protocol("the revision list does not follow the contract".into())),
    }
}

/// A lost lock: the structured `detail.code` when the ERP sends one, else its known texts
/// (heartbeat "lock lost", upload "Take the project lock before publishing a revision").
fn lock_lost(call: Call, d: &Value, text: &str) -> bool {
    if let Some(code) = d["code"].as_str() {
        return code == "lock_lost";
    }
    text.eq_ignore_ascii_case("lock lost") || (call == Call::Upload && text.starts_with("Take the project lock"))
}

fn read_limited(r: Response, max: u64) -> Result<Vec<u8>, LinkError> {
    let mut buf = Vec::new();
    r.into_reader().take(max + 1).read_to_end(&mut buf).map_err(|e| LinkError::Network(e.to_string()))?;
    if buf.len() as u64 > max {
        return Err(LinkError::Protocol(format!("answer over {max} bytes")));
    }
    Ok(buf)
}

/// Parses a JSON answer. serde's messages can quote a value (a key, a token): only the
/// position is kept.
fn parse<T: DeserializeOwned>(what: &str, bytes: &[u8]) -> Result<T, LinkError> {
    serde_json::from_slice(bytes).map_err(|e| {
        LinkError::Protocol(format!("{what} does not follow the contract (line {}, column {})", e.line(), e.column()))
    })
}

/// Percent-encodes a manifest path for the URL; `/` stays (the route takes a path).
fn encode_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn boundary() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::from("ddc-link-");
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

impl Client {
    pub fn new(erp: &ErpUrl) -> Result<Self, LinkError> {
        Ok(Client {
            base: format!("{}{API}", erp.base),
            erp: erp.clone(),
            net: transport(erp, READ_TIMEOUT)?,
            token: None,
            stopped: Arc::new(Mutex::new(None)),
        })
    }

    pub fn with_token(mut self, token: Secret) -> Self {
        self.token = Some(token);
        self
    }

    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// The ERP's majors when a 426 stopped this client, else `None`.
    pub fn stopped_with(&self) -> Option<Vec<u32>> {
        self.stopped.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn stop(&self, supported: Vec<u32>) -> LinkError {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = Some(supported.clone());
        LinkError::Unsupported { supported }
    }

    fn request(&self, method: &str, path: &str) -> Result<Request, LinkError> {
        if let Some(s) = self.stopped_with() {
            return Err(LinkError::Unsupported { supported: s });
        }
        let mut r = Request::new(method, format!("{}{path}", self.base)).header("X-DDC-Link", &versions_header());
        if let Some(t) = &self.token {
            r = r.header("Authorization", &format!("Bearer {}", t.expose()));
        }
        Ok(r)
    }

    fn finish(&self, call: Call, res: Result<Response, NetError>) -> Result<Response, LinkError> {
        match res {
            Ok(r) if r.status >= 400 => Err(self.status_error(call, r.status, r)),
            Ok(r) => {
                let v = r.header("X-DDC-Link").map(str::trim).map(str::to_string);
                match v.as_deref().map(str::parse::<u32>) {
                    Some(Ok(m)) if LINK_VERSIONS.contains(&m) => Ok(r),
                    Some(Ok(m)) => Err(self.stop(vec![m])),
                    _ => Err(LinkError::Protocol(
                        "the answer has no link version: is the address an ERP with the Design module?".into(),
                    )),
                }
            }
            Err(e) => Err(LinkError::Network(e.0)),
        }
    }

    fn status_error(&self, call: Call, code: u16, r: Response) -> LinkError {
        let retry_after = r.header("Retry-After").and_then(|s| s.trim().parse().ok());
        let body: Value = read_limited(r, ERROR_BODY_MAX).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let d = &body["detail"];
        let text = detail_text(d);
        match code {
            426 => {
                let s = d["supported"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect()).unwrap_or_default();
                self.stop(s)
            }
            401 => LinkError::Unauthorized,
            403 => LinkError::Forbidden(text),
            404 => LinkError::NotFound(text),
            410 if call == Call::Exchange => LinkError::CodeGone,
            413 => LinkError::TooLarge(text),
            422 => LinkError::Rejected(text),
            429 => LinkError::Throttled { retry_after },
            409 => {
                if let Some(h) = d["held_by"].as_str() {
                    if call == Call::TakeLock || call == Call::Heartbeat || call == Call::Upload {
                        return LinkError::LockHeld { held_by: h.to_string(), expires_at: d["expires_at"].as_str().unwrap_or_default().to_string() };
                    }
                }
                if d.get("latest_published").is_some() && call == Call::Upload {
                    return LinkError::ParentMoved { latest: d["latest_published"].as_str().map(String::from) };
                }
                if lock_lost(call, d, &text) && call != Call::TakeLock {
                    LinkError::LockLost
                } else {
                    LinkError::Conflict(text)
                }
            }
            _ => LinkError::Server { status: code, detail: text },
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, what: &str) -> Result<T, LinkError> {
        let r = self.request("GET", path)?.header("Accept", "application/json");
        let r = self.finish(Call::Other, self.net.send(r))?;
        parse(what, &read_limited(r, JSON_BODY_MAX)?)
    }

    fn post_json<T: DeserializeOwned>(&self, call: Call, path: &str, body: Option<&Value>, what: &str) -> Result<T, LinkError> {
        let r = self.request("POST", path)?.header("Accept", "application/json");
        let r = match body {
            Some(b) => r.header("Content-Type", "application/json").body(Body::Bytes(b.to_string().into_bytes())),
            None => r,
        };
        let r = self.finish(call, self.net.send(r))?;
        parse(what, &read_limited(r, JSON_BODY_MAX)?)
    }

    /// `POST /link/exchange`: the code from the link file for a project token.
    pub fn exchange(&self, code: &Secret, info: &ClientInfo) -> Result<Exchanged, LinkError> {
        let body = json!({"code": code.expose(), "client": {"name": info.name, "version": info.version, "os": info.os}});
        let b: ExchangeBody = self.post_json(Call::Exchange, "/link/exchange", Some(&body), "the exchange answer")?;
        let t = b.token.expose();
        if !t.starts_with("ddcl_") || t.len() < 37 {
            return Err(LinkError::Protocol("the exchange answer carries no link token".into()));
        }
        crate::linkfile::check_uuid(&b.project_id).map_err(|_| LinkError::Protocol("the exchange answer has a bad project id".into()))?;
        Ok(Exchanged { token: b.token, expires_at: b.expires_at, project_id: b.project_id })
    }

    /// `GET /projects/{id}/context`. The AI block stays in the returned value only.
    pub fn context(&self, project_id: &str) -> Result<Context, LinkError> {
        let c: Context = self.get_json(&format!("/projects/{project_id}/context"), "the project context")?;
        if c.schema != "ddc-context/1" {
            return Err(LinkError::Protocol(format!("context schema '{}'", c.schema.chars().take(40).collect::<String>())));
        }
        if !c.link_versions.iter().any(|v| LINK_VERSIONS.contains(v)) {
            return Err(self.stop(c.link_versions.clone()));
        }
        if c.project.id != project_id {
            return Err(LinkError::Protocol("the context is for another project".into()));
        }
        Ok(c)
    }

    /// `POST /projects/{id}/lock`: 409 with the holder when someone else edits.
    pub fn take_lock(&self, project_id: &str) -> Result<Lock, LinkError> {
        self.post_json(Call::TakeLock, &format!("/projects/{project_id}/lock"), Some(&json!({})), "the lock")
    }

    /// `POST /projects/{id}/lock/heartbeat`: [`LinkError::LockLost`] when the lock is gone (the ERP
    /// never re-takes it on a heartbeat).
    pub fn heartbeat(&self, project_id: &str) -> Result<Lock, LinkError> {
        self.post_json(Call::Heartbeat, &format!("/projects/{project_id}/lock/heartbeat"), None, "the lock")
    }

    /// `DELETE /projects/{id}/lock`.
    pub fn release_lock(&self, project_id: &str) -> Result<(), LinkError> {
        let r = self.request("DELETE", &format!("/projects/{project_id}/lock"))?;
        self.finish(Call::Other, self.net.send(r)).map(|_| ())
    }

    /// `GET /projects/{id}/revisions`, newest first: every page of the `{items, total, offset, limit}`
    /// envelope (an older ERP answers a bare list in one go).
    pub fn revisions(&self, project_id: &str) -> Result<Vec<RevisionView>, LinkError> {
        let mut all = Vec::new();
        loop {
            let path = format!("/projects/{project_id}/revisions?offset={}&limit={REVISION_PAGE}", all.len());
            let page: Value = self.get_json(&path, "the revision list")?;
            let (items, total) = revision_page(page)?;
            let got = items.len();
            all.extend(items);
            match total {
                Some(t) if got > 0 && all.len() < t => {}
                _ => return Ok(all),
            }
        }
    }

    /// `GET /revisions/{id}`.
    pub fn revision(&self, revision_id: &str) -> Result<RevisionView, LinkError> {
        crate::linkfile::check_uuid(revision_id)?;
        self.get_json(&format!("/revisions/{revision_id}"), "the revision")
    }

    /// The parent of the next upload (contract §8): the newest revision waiting or published that is
    /// not an import record (no element table to diff against) and can still be accepted: its waiting
    /// parents lead to the published one, or to nothing while the published one is an import or missing.
    pub fn chain_tip(&self, project_id: &str) -> Result<Option<RevisionView>, LinkError> {
        let mut v = self.revisions(project_id)?;
        v.retain(RevisionView::in_play);
        v.sort_by(|a, b| b.number.cmp(&a.number));
        let published = v.iter().find(|r| r.status == "published");
        let viable = |start: &RevisionView| -> bool {
            let Some(mut r) = v.iter().find(|x| x.revision_id == start.revision_id) else { return false };
            while r.status == "shared" {
                let Some(p) = r.parent_revision_id.as_deref() else {
                    return published.is_none_or(RevisionView::imported);
                };
                match v.iter().find(|x| x.revision_id == p) {
                    Some(x) => r = x,
                    None => return false, // built on a rejected or archived revision
                }
            }
            true
        };
        Ok(v.iter().find(|r| !r.imported() && viable(r)).cloned())
    }

    /// `GET /revisions/{id}/files/{path}`: one file listed in the revision's manifest.
    pub fn download(&self, revision_id: &str, path: &str) -> Result<Vec<u8>, LinkError> {
        crate::linkfile::check_uuid(revision_id)?;
        if path.is_empty() || path.starts_with('/') || path.split('/').any(|s| s == ".." || s.is_empty()) {
            return Err(LinkError::Protocol(format!("unsafe file path '{path}'")));
        }
        let r = self.request("GET", &format!("/revisions/{revision_id}/files/{}", encode_path(path)))?;
        let r = self.finish(Call::Other, self.net.send(r))?;
        read_limited(r, FILE_MAX)
    }

    /// `POST /projects/{id}/revisions`, multipart field `package`. The file is streamed.
    pub fn upload(&self, project_id: &str, package: &Path) -> Result<RevisionView, LinkError> {
        let f = std::fs::File::open(package).map_err(|e| LinkError::Io(format!("{}: {e}", package.display())))?;
        let len = f.metadata().map_err(|e| LinkError::Io(format!("{}: {e}", package.display())))?.len();
        let b = boundary();
        let head = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"package\"; filename=\"revision.ddcrev\"\r\nContent-Type: application/zip\r\n\r\n"
        );
        let tail = format!("\r\n--{b}--\r\n");
        let total = head.len() as u64 + len + tail.len() as u64;
        let body = std::io::Cursor::new(head.into_bytes()).chain(f).chain(std::io::Cursor::new(tail.into_bytes()));
        let up = transport(&self.erp, UPLOAD_READ_TIMEOUT)?;
        let r = self
            .request("POST", &format!("/projects/{project_id}/revisions"))?
            .header("Accept", "application/json")
            .header("Content-Type", &format!("multipart/form-data; boundary={b}"))
            .body(Body::Reader(Box::new(body), total));
        let r = self.finish(Call::Upload, up.send(r))?;
        parse("the uploaded revision", &read_limited(r, JSON_BODY_MAX)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_forms() {
        assert_eq!(detail_text(&json!("lock lost")), "lock lost");
        assert_eq!(detail_text(&json!({"message": "Project is being edited", "held_by": "A"})), "Project is being edited");
        assert_eq!(
            detail_text(&json!([{"loc": ["body", "code"], "msg": "field required", "type": "missing"}])),
            "body.code: field required"
        );
        assert_eq!(detail_text(&Value::Null), "");
    }

    #[test]
    fn an_imported_revision_parses() {
        let v: RevisionView = parse("x", include_bytes!("../../io/contract/examples/revision-view.imported.json")).unwrap();
        assert!(v.imported() && v.in_play());
        assert_eq!((v.depth_level, v.source_kind.as_str(), v.brief_version), (None, "imported", None));
        let tip = serde_json::from_str::<Value>(include_str!("../../io/contract/examples/revision-view.json")).unwrap();
        let tip: RevisionView = serde_json::from_value(tip).unwrap();
        assert!(!tip.imported() && tip.depth_level.is_some());
    }

    #[test]
    fn revision_pages_and_bare_list() {
        let one = serde_json::from_str::<Value>(include_str!("../../io/contract/examples/revision-view.json")).unwrap();
        let (items, total) = revision_page(json!({"items": [one.clone()], "total": 51, "offset": 50, "limit": 50})).unwrap();
        assert_eq!((items.len(), total), (1, Some(51)));
        let (items, total) = revision_page(json!([one.clone(), one])).unwrap();
        assert_eq!((items.len(), total), (2, None));
        assert!(revision_page(json!("x")).is_err());
    }

    #[test]
    fn lock_lost_by_code_then_text() {
        assert!(lock_lost(Call::Heartbeat, &json!({"code": "lock_lost", "message": "anything"}), "anything"));
        assert!(!lock_lost(Call::Heartbeat, &json!({"code": "other", "message": "lock lost"}), "lock lost"));
        assert!(lock_lost(Call::Heartbeat, &json!("lock lost"), "lock lost"));
        assert!(lock_lost(Call::Upload, &json!("Take the project lock first"), "Take the project lock first"));
        assert!(!lock_lost(Call::Heartbeat, &json!("Take the project lock first"), "Take the project lock first"));
    }

    #[test]
    fn paths_and_boundaries() {
        assert_eq!(encode_path("sheets/Plan EG.pdf"), "sheets/Plan%20EG.pdf");
        assert_eq!(encode_path("elements.parquet"), "elements.parquet");
        let (a, b) = (boundary(), boundary());
        assert_ne!(a, b);
        assert_eq!(a.len(), "ddc-link-".len() + 32);
    }
}
