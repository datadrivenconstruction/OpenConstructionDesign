// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An ERP that speaks contract `ddc-erp-link/1` over real HTTP on 127.0.0.1, for tests only
//! (feature `mock-erp`). It follows the module router of the ERP (`erp-module/design/router.py`):
//! version header and 426, single-use codes (410), project-scoped `ddcl_` tokens (401), lock with
//! holder (409), heartbeat that never re-takes a lost lock (409 `lock lost`), upload only with the
//! lock, hash and schema checks (422), parent = chain tip (409). Every request body the editor
//! sends and every answer it gets is checked against the contract schemas; a mismatch is
//! recorded in [`State::contract_errors`], so a test fails on the shape, not only on the flow.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use ddc_io::contract::{validate, validate_with, Doc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const EXCHANGE: &str = include_str!("../../io/contract/schemas/exchange.schema.json");
const LOCK: &str = include_str!("../../io/contract/schemas/lock.schema.json");
const REVISION_VIEW: &str = include_str!("../../io/contract/schemas/revision-view.schema.json");
const BRIEF: &str = include_str!("../../io/contract/examples/brief.de-residential.json");
const NOW: &str = "2026-11-13T12:00:00Z";
const LATER: &str = "2026-11-13T12:10:00Z";

/// One request as the ERP saw it.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub status: u16,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    /// The linked editor's user.
    Us,
    /// Someone else, by display name.
    Other(String),
}

#[derive(Debug, Default)]
pub struct State {
    /// code → used
    pub codes: HashMap<String, bool>,
    pub tokens: Vec<String>,
    pub lock: Option<Holder>,
    /// revision-view documents, oldest first
    pub revisions: Vec<Value>,
    /// (revision id, path) → bytes, as uploaded
    pub files: HashMap<(String, String), Vec<u8>>,
    /// raw multipart bodies of every upload
    pub uploads: Vec<Vec<u8>>,
    pub requests: Vec<Recorded>,
    pub contract_errors: Vec<String>,
    /// majors this ERP speaks (`[1]` by default; `[2]` makes it refuse the editor)
    pub supported: Vec<u32>,
    pub ai_key: Option<String>,
    pub can_publish: bool,
    /// lock taken / heartbeats answered
    pub lock_takes: u32,
    pub heartbeats: u32,
    /// set by the test: the ERP drops the lock (as after a crash or a manager's release)
    pub drop_lock: bool,
    /// the brief the context carries; the contract example when `None`
    pub brief: Option<Value>,
}

/// The contract's German residential brief, as the ERP would hand it over.
pub fn example_brief() -> Value {
    serde_json::from_str(BRIEF).expect("brief example")
}

pub struct MockErp {
    pub url: String,
    pub project_id: String,
    pub user: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    port: u16,
    thread: Option<JoinHandle<()>>,
}

fn rand_text(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0u64;
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        let mut x = h.finish();
        for _ in 0..10 {
            if out.len() < n {
                out.push(A[(x & 63) as usize] as char);
            }
            x >>= 6;
        }
        i += 1;
    }
    out
}

/// A valid lower-case UUID made from `n`.
pub fn uuid(n: usize) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

/// A small `.ddcrev` that follows the contract: the example manifest with these files.
/// `files`: (path, role, bytes); the required roles are added with stand-in bytes when missing.
pub fn package(project_id: &str, revision_id: &str, parent: Option<&str>, files: &[(&str, &str, &[u8])]) -> Vec<u8> {
    build_package(project_id, revision_id, parent, files, None)
}

/// Same, but the manifest lists a wrong sha256 for `lie_about`.
pub fn package_lying(project_id: &str, revision_id: &str, parent: Option<&str>, lie_about: &str) -> Vec<u8> {
    build_package(project_id, revision_id, parent, &[], Some(lie_about))
}

fn build_package(project_id: &str, revision_id: &str, parent: Option<&str>, files: &[(&str, &str, &[u8])], lie: Option<&str>) -> Vec<u8> {
    use std::io::Write as _;
    let mut m: Value = serde_json::from_str(include_str!("../../io/contract/examples/manifest.json")).expect("manifest example");
    m["revision_id"] = json!(revision_id);
    m["project_ref"]["erp_project_id"] = json!(project_id);
    m["parent_revision_id"] = json!(parent);
    m.as_object_mut().expect("object").remove("checks");
    let mut all: Vec<(String, String, Vec<u8>)> = files.iter().map(|(p, r, b)| (p.to_string(), r.to_string(), b.to_vec())).collect();
    for (p, r) in [("model.ddc", "source"), ("geometry.glb", "geometry"), ("elements.parquet", "elements")] {
        if !all.iter().any(|f| f.1 == r) {
            all.push((p.into(), r.into(), format!("stand-in {p}").into_bytes()));
        }
    }
    if parent.is_some() && !all.iter().any(|f| f.1 == "diff") {
        all.push(("diff.json".into(), "diff".into(), b"{}".to_vec()));
    }
    m["files"] = all
        .iter()
        .map(|(p, r, b)| {
            let mut sha: String = Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect();
            if lie == Some(p.as_str()) {
                sha = "0".repeat(64);
            }
            json!({"path": p, "role": r, "sha256": sha, "bytes": b.len()})
        })
        .collect();
    validate(Doc::Revision, &m).expect("mock package manifest follows the contract");
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    w.start_file("manifest.json", o).expect("zip");
    w.write_all(m.to_string().as_bytes()).expect("zip");
    for (p, _, b) in &all {
        w.start_file(p.as_str(), o).expect("zip");
        w.write_all(b).expect("zip");
    }
    w.finish().expect("zip").into_inner()
}

fn sub_schema(text: &str, part: &str) -> Value {
    let root: Value = serde_json::from_str(text).expect("schema");
    let mut s = root["properties"][part].clone();
    s["$defs"] = root["$defs"].clone();
    s
}

impl MockErp {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let state = Arc::new(Mutex::new(State { supported: vec![1], can_publish: true, ..State::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let project_id = "746402cb-14a8-430c-b3b9-9c6908fa75d7".to_string();
        let (st, sp, pid) = (state.clone(), stop.clone(), project_id.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if sp.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let (st, pid) = (st.clone(), pid.clone());
                std::thread::spawn(move || serve(conn, &st, &pid));
            }
        });
        MockErp {
            url: format!("http://127.0.0.1:{port}"),
            project_id,
            user: "Planer".into(),
            state,
            stop,
            port,
            thread: Some(thread),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fresh single-use code, as `POST /link/code` would issue.
    pub fn issue_code(&self) -> String {
        let c = rand_text(43);
        self.state().codes.insert(c.clone(), false);
        c
    }

    /// A `.ddclink` document with a fresh code; checked against `link.schema.json`.
    pub fn link_file(&self, action: &str, revision_id: Option<&str>) -> String {
        let mut v = json!({
            "schema": "ddc-link/1",
            "erp_url": self.url,
            "project_id": self.project_id,
            "project_name": "Wohnhaus Beispielstraße",
            "code": self.issue_code(),
            "expires_at": LATER,
            "action": action,
        });
        if let Some(r) = revision_id {
            v["revision_id"] = json!(r);
        }
        validate(Doc::Link, &v).expect("mock link file follows the contract");
        v.to_string()
    }

    pub fn set_ai_key(&self, key: &str) {
        self.state().ai_key = Some(key.to_string());
    }

    /// Accepts a revision as a manager would: the previous published one is archived.
    pub fn accept(&self, revision_id: &str) {
        let mut s = self.state();
        for r in s.revisions.iter_mut() {
            if r["status"] == "published" {
                r["status"] = json!("archived");
            }
        }
        // accepting a revision supersedes the waiting revisions it was built on
        let mut cur = revision_id.to_string();
        while let Some(r) = s.revisions.iter_mut().find(|r| r["revision_id"] == cur.as_str()) {
            if cur == revision_id {
                r["status"] = json!("published");
            } else if r["status"] == "shared" {
                r["status"] = json!("archived");
            } else {
                break;
            }
            match r["parent_revision_id"].as_str() {
                Some(p) => cur = p.to_string(),
                None => break,
            }
        }
    }

    /// Wraps a converter model as the ERP's R16 does: a `ddc-import/1` record on the chain tip, no files.
    pub fn wrap_import(&self, project_id: &str) -> String {
        let mut s = self.state();
        let n = s.revisions.len() + 1;
        let rid = uuid(500 + n);
        let parent = s.revisions.iter().rev().find(|r| in_play(r)).map(|r| r["revision_id"].clone()).unwrap_or(Value::Null);
        let model = uuid(1000 + n);
        s.revisions.push(json!({
            "revision_id": rid, "project_id": project_id, "number": n, "parent_revision_id": parent,
            "status": "shared", "source_kind": "imported", "depth_level": null, "brief_version": null,
            "message": "Haus.ifc", "bim_model_id": model, "author_user_id": uuid(7), "decided_by": null,
            "decided_at": null, "decision_comment": null, "package_sha256": "d".repeat(64), "package_bytes": 4096,
            "created_at": NOW,
            "manifest": {"schema": "ddc-import/1", "revision_id": rid, "bim_model_id": model,
                         "source": {"kind": "imported", "imported_from": "Haus.ifc", "model_format": "ifc"},
                         "counts": {"elements": 3, "storeys": 2}, "keys": {"verbatim": 3, "mapped": 0},
                         "restore": {"parent_model_id": null, "metadata": {}}, "files": []},
        }));
        rid
    }

    pub fn reject(&self, revision_id: &str) {
        if let Some(r) = self.state().revisions.iter_mut().find(|r| r["revision_id"] == revision_id) {
            r["status"] = json!("rejected");
        }
    }
}

fn in_play(r: &Value) -> bool {
    r["status"] == "shared" || r["status"] == "published"
}

/// Contract §8: newest revision in play that is no import record and whose waiting parents lead to
/// the published one (or to nothing while the published one is an import or missing).
fn design_tip(revs: &[Value]) -> Option<Value> {
    let live: Vec<&Value> = revs.iter().filter(|r| in_play(r)).collect();
    let imported = |r: &Value| r["manifest"]["schema"] == "ddc-import/1";
    let published = live.iter().find(|r| r["status"] == "published");
    let viable = |start: &Value| {
        let Some(mut r) = live.iter().copied().find(|x| x["revision_id"] == start["revision_id"]) else { return false };
        while r["status"] == "shared" {
            if r["parent_revision_id"].is_null() {
                return published.is_none_or(|p| imported(p));
            }
            match live.iter().find(|x| x["revision_id"] == r["parent_revision_id"]) {
                Some(x) => r = x,
                None => return false,
            }
        }
        true
    };
    live.iter().rev().find(|r| !imported(r) && viable(r)).map(|r| r["revision_id"].clone())
}

impl Drop for MockErp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_req(s: &TcpStream) -> Option<Req> {
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let (method, path) = (it.next()?.to_string(), it.next()?.to_string());
    let mut headers = vec![];
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
    let mut body = vec![];
    if get("Transfer-Encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let mut sz = String::new();
            r.read_line(&mut sz).ok()?;
            let n = usize::from_str_radix(sz.trim(), 16).ok()?;
            let mut chunk = vec![0; n + 2];
            r.read_exact(&mut chunk).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    } else if let Some(n) = get("Content-Length").and_then(|v| v.parse::<usize>().ok()) {
        body = vec![0; n];
        r.read_exact(&mut body).ok()?;
    }
    Some(Req { method, path, headers, body })
}

struct Resp {
    status: u16,
    body: Vec<u8>,
    ctype: &'static str,
}

fn j(status: u16, v: Value) -> Resp {
    Resp { status, body: v.to_string().into_bytes(), ctype: "application/json" }
}

fn detail(status: u16, d: Value) -> Resp {
    j(status, json!({ "detail": d }))
}

fn serve(mut conn: TcpStream, st: &Mutex<State>, pid: &str) {
    let Some(req) = read_req(&conn) else { return };
    let resp = {
        let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
        let resp = route(&req, &mut s, pid);
        s.requests.push(Recorded {
            method: req.method.clone(),
            path: req.path.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
            status: resp.status,
        });
        resp
    };
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nX-DDC-Link: 1\r\nConnection: close\r\n\r\n",
        resp.status,
        resp.ctype,
        resp.body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(&resp.body);
    let _ = conn.flush();
}

/// `link_version` of the ERP (auth.py): absent = fine; else canonical majors, one in common.
fn version_ok(h: Option<&str>, supported: &[u32]) -> bool {
    let Some(h) = h else { return true };
    let parts: Vec<&str> = h.split(',').map(str::trim).collect();
    let canon = |p: &str| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'));
    if !parts.iter().all(|p| canon(p)) {
        return false;
    }
    parts.iter().filter_map(|p| p.parse::<u32>().ok()).any(|m| supported.contains(&m))
}

fn check(s: &mut State, what: &str, r: Result<(), Vec<String>>) {
    if let Err(e) = r {
        s.contract_errors.push(format!("{what}: {e:?}"));
    }
}

fn lock_view(s: &State) -> Value {
    match &s.lock {
        None => Value::Null,
        Some(Holder::Us) => json!({"held_by": "Planer", "mine": true, "expires_at": LATER}),
        Some(Holder::Other(n)) => json!({"held_by": n, "mine": false, "expires_at": LATER}),
    }
}

fn route(req: &Req, s: &mut State, pid: &str) -> Resp {
    let Some(path) = req.path.strip_prefix(crate::API) else {
        return detail(404, json!("Not Found"));
    };
    if !version_ok(req.header("X-DDC-Link"), &s.supported) {
        return detail(426, json!({"message": "Unsupported X-DDC-Link version", "supported": s.supported}));
    }
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let m = req.method.as_str();
    if (m, seg.as_slice()) == ("POST", ["link", "exchange"].as_slice()) {
        return exchange(req, s, pid);
    }
    // every other route needs the project token
    let token = req.header("Authorization").and_then(|a| a.strip_prefix("Bearer ")).unwrap_or_default();
    if !s.tokens.iter().any(|t| t == token) {
        return detail(401, json!("Link token invalid or expired"));
    }
    match (m, seg.as_slice()) {
        ("GET", ["projects", p, "context"]) if *p == pid => context(s, pid),
        ("POST", ["projects", p, "lock"]) if *p == pid => take_lock(s),
        ("POST", ["projects", p, "lock", "heartbeat"]) if *p == pid => heartbeat(s),
        ("DELETE", ["projects", p, "lock"]) if *p == pid => {
            if s.lock == Some(Holder::Us) {
                s.lock = None;
            }
            Resp { status: 204, body: vec![], ctype: "application/json" }
        }
        ("POST", ["projects", p, "revisions"]) if *p == pid => upload(req, s, pid),
        ("GET", ["projects", p, "revisions"]) if *p == pid => {
            let mut v = s.revisions.clone();
            v.reverse();
            j(200, Value::Array(v))
        }
        ("GET", ["revisions", r]) => match s.revisions.iter().find(|x| x["revision_id"] == *r) {
            Some(v) => j(200, v.clone()),
            None => detail(404, json!("Revision not found")),
        },
        ("GET", ["revisions", r, "files", rest @ ..]) => {
            let p = rest.join("/").replace("%20", " ");
            match s.files.get(&(r.to_string(), p)) {
                Some(b) => Resp { status: 200, body: b.clone(), ctype: "application/octet-stream" },
                None => detail(404, json!("File not in this revision")),
            }
        }
        ("GET" | "POST" | "DELETE", ["projects", ..]) => detail(404, json!("Project not found")),
        _ => detail(404, json!("Not Found")),
    }
}

fn exchange(req: &Req, s: &mut State, pid: &str) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return detail(422, json!([{"loc": ["body"], "msg": "invalid json"}]));
    };
    let r = validate_with(&sub_schema(EXCHANGE, "request"), &body);
    check(s, "exchange request", r);
    let code = body["code"].as_str().unwrap_or_default().to_string();
    match s.codes.get_mut(&code) {
        Some(used) if !*used => *used = true,
        _ => return detail(410, json!("Link code used or expired")),
    }
    let token = format!("ddcl_{}", rand_text(43));
    s.tokens.push(token.clone());
    let out = json!({"token": token, "expires_at": "2026-12-13T12:00:00Z", "project_id": pid});
    let r = validate_with(&sub_schema(EXCHANGE, "response"), &out);
    check(s, "exchange response", r);
    j(200, out)
}

fn context(s: &mut State, pid: &str) -> Resp {
    let mut brief = s.brief.clone().unwrap_or_else(example_brief);
    brief["project_id"] = json!(pid);
    let latest = s.revisions.last().map(|r| json!({"revision_id": r["revision_id"], "number": r["number"], "status": r["status"]}));
    let ai = s.ai_key.as_ref().map(|k| {
        json!({"protocol": "chat-completions", "endpoint": "https://ai.example.invalid/v1", "model": "erp-default",
               "api_key": k, "local_only": false, "expires_at": "2026-11-13T20:00:00Z"})
    });
    let out = json!({
        "schema": "ddc-context/1",
        "link_versions": s.supported,
        "project": {"id": pid, "name": "Wohnhaus Beispielstraße", "country": "DE", "currency": "EUR", "locale": "de"},
        "permissions": {"can_publish": s.can_publish, "can_accept": false, "can_generate": true},
        "lock": lock_view(s),
        "latest_revision": latest,
        "brief": brief,
        "ai": ai,
    });
    let r = validate(Doc::Context, &out);
    check(s, "context", r);
    j(200, out)
}

fn take_lock(s: &mut State) -> Resp {
    if !s.can_publish {
        return detail(403, json!("Your role may not publish to this project"));
    }
    if let Some(Holder::Other(n)) = &s.lock {
        return detail(409, json!({"message": "Project is being edited", "held_by": n, "expires_at": LATER}));
    }
    s.lock = Some(Holder::Us);
    s.lock_takes += 1;
    let v = lock_view(s);
    let r = validate_with(&serde_json::from_str(LOCK).expect("lock schema"), &v);
    check(s, "lock", r);
    j(200, v)
}

fn heartbeat(s: &mut State) -> Resp {
    if s.drop_lock {
        s.drop_lock = false;
        s.lock = None;
    }
    if s.lock != Some(Holder::Us) {
        return detail(409, json!("lock lost"));
    }
    s.heartbeats += 1;
    j(200, lock_view(s))
}

/// The `package` part of a multipart body.
fn package_part(req: &Req) -> Option<Vec<u8>> {
    let ct = req.header("Content-Type")?;
    let b = ct.split(';').map(str::trim).find_map(|p| p.strip_prefix("boundary="))?.trim_matches('"');
    let delim = format!("--{b}");
    let body = &req.body;
    let find = |hay: &[u8], needle: &[u8], from: usize| hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from);
    let mut at = find(body, delim.as_bytes(), 0)?;
    loop {
        let start = at + delim.len();
        if body[start..].starts_with(b"--") {
            return None;
        }
        let head_end = find(body, b"\r\n\r\n", start)? + 4;
        let head = String::from_utf8_lossy(&body[start..head_end]).to_string();
        let next = find(body, format!("\r\n{delim}").as_bytes(), head_end)?;
        if head.contains("name=\"package\"") {
            return Some(body[head_end..next].to_vec());
        }
        at = next + 2;
    }
}

fn reject(status: u16, msg: &str) -> Resp {
    detail(status, json!(msg))
}

fn upload(req: &Req, s: &mut State, pid: &str) -> Resp {
    if s.lock != Some(Holder::Us) {
        return reject(409, "Take the project lock before publishing a revision");
    }
    let Some(pkg) = package_part(req) else {
        return detail(422, json!([{"loc": ["body", "package"], "msg": "Field required"}]));
    };
    s.uploads.push(req.body.clone());
    let mut z = match zip::ZipArchive::new(std::io::Cursor::new(pkg.clone())) {
        Ok(z) => z,
        Err(_) => return reject(422, "package is not a zip archive"),
    };
    let mut all: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for i in 0..z.len() {
        let Ok(mut e) = z.by_index(i) else { return reject(422, "unreadable zip entry") };
        let mut b = vec![];
        if e.read_to_end(&mut b).is_err() {
            return reject(422, "unreadable zip entry");
        }
        let Ok(name) = e.name().map(|n| n.into_owned()) else { return reject(422, "unreadable zip entry") };
        all.insert(name, b);
    }
    let Some(m) = all.get("manifest.json").and_then(|b| serde_json::from_slice::<Value>(b).ok()) else {
        return reject(422, "manifest.json missing or not JSON");
    };
    if let Err(e) = validate(Doc::Revision, &m) {
        return detail(422, json!(e));
    }
    if m["project_ref"]["erp_project_id"] != pid {
        return reject(422, "manifest is for another project");
    }
    let mut listed = vec!["manifest.json".to_string()];
    for f in m["files"].as_array().into_iter().flatten() {
        let p = f["path"].as_str().unwrap_or_default();
        let Some(data) = all.get(p) else { return reject(422, &format!("{p} listed but missing")) };
        let sha: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        if Some(sha.as_str()) != f["sha256"].as_str() || Some(data.len() as u64) != f["bytes"].as_u64() {
            return reject(422, &format!("{p}: sha256 mismatch"));
        }
        listed.push(p.to_string());
    }
    if let Some(x) = all.keys().find(|k| !listed.contains(k)) {
        return reject(422, &format!("{x} is not listed in the manifest"));
    }
    let rid = m["revision_id"].as_str().unwrap_or_default().to_string();
    if s.revisions.iter().any(|r| r["revision_id"] == rid.as_str()) {
        return reject(409, "This revision was already uploaded");
    }
    let tip = design_tip(&s.revisions);
    let parent = m["parent_revision_id"].clone();
    if tip.clone().unwrap_or(Value::Null) != parent {
        return detail(409, json!({"message": "parent_revision_id must be the newest revision that is neither rejected nor archived nor an imported model",
                                   "latest_published": tip}));
    }
    let n = s.revisions.len() + 1;
    let sha: String = Sha256::digest(&pkg).iter().map(|b| format!("{b:02x}")).collect();
    let view = json!({
        "revision_id": rid,
        "project_id": pid,
        "number": n,
        "parent_revision_id": parent,
        "status": "shared",
        "source_kind": m["source"]["kind"],
        "depth_level": m["depth_level"],
        "brief_version": m["source"].get("brief_version").cloned().unwrap_or(Value::Null),
        "message": m.get("message").cloned().unwrap_or(json!("")),
        "bim_model_id": uuid(1000 + n),
        "author_user_id": uuid(7),
        "decided_by": null,
        "decided_at": null,
        "decision_comment": null,
        "package_sha256": sha,
        "package_bytes": pkg.len(),
        "created_at": NOW,
        "manifest": m,
    });
    let r = validate_with(&serde_json::from_str(REVISION_VIEW).expect("revision-view schema"), &view);
    check(s, "revision view", r);
    for (p, b) in all {
        if p != "manifest.json" {
            s.files.insert((rid.clone(), p), b);
        }
    }
    s.revisions.push(view.clone());
    j(201, view)
}
