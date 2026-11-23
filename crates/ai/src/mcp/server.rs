// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Own server on `TcpListener`: one request per connection, loopback only, every check before any
//! work. Runs only because the person turned it on: nothing here starts by itself.

use super::auth::{self, bearer, host_allowed, origin_allowed, token_matches};
use super::wire::{read_body, read_head, Head, ReadError, Resp};
use super::{rpc, McpCall, McpConfig};
use crate::secret::{Secret, SecretError};
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use web_time::{Duration, Instant};

const ENDPOINT: &str = "/mcp";
const ACCEPT_POLL: Duration = Duration::from_millis(15);
const READ_SLICE: Duration = Duration::from_millis(250);
const WRITE_TIMEOUT: Duration = crate::limits::MCP_WRITE_TIMEOUT.dur();
const REFUSE_WRITE_TIMEOUT: Duration = crate::limits::MCP_REFUSE_TIMEOUT.dur();
const DRAIN_BYTES: u64 = 1 << 20;
const DRAIN_WAIT: Duration = Duration::from_millis(100);
/// There is no open connection to watch: an agent counts as connected while it keeps calling.
const CONNECTED_WINDOW: Duration = Duration::from_secs(120);
const MAX_CLIENTS: usize = crate::limits::MCP_CLIENTS.us();
const MAX_SESSIONS: usize = crate::limits::MCP_SESSIONS.us();
const SESSION_ID_BYTES: usize = 16;
/// Reads kept while the editor does not drain them; the oldest go first.
const MAX_RESOURCE_READS: usize = crate::limits::MCP_RESOURCE_READS.us();
const BIND_TRIES: usize = 8;

#[derive(Clone, Debug)]
pub struct McpLimits {
    pub max_body: usize,
    pub max_head: usize,
    pub max_connections: usize,
    pub rate_per_s: f64,
    pub rate_burst: f64,
    pub read_timeout: Duration,
    pub call_wait: Duration,
}

impl Default for McpLimits {
    fn default() -> Self {
        McpLimits {
            max_body: 4 << 20,
            max_head: 16 << 10,
            max_connections: 8,
            rate_per_s: 40.0,
            rate_burst: 80.0,
            read_timeout: Duration::from_secs(10),
            call_wait: Duration::from_secs(120),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClientSeen {
    pub name: String,
    pub calls: u64,
    pub last_seen: Instant,
}

struct Current {
    token: Secret,
    revoked: Arc<AtomicBool>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

struct Session {
    client: String,
    last_seen: Instant,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Shared {
    current: RwLock<Current>,
    tx: Mutex<mpsc::Sender<McpCall>>,
    port: u16,
    pub(crate) limits: McpLimits,
    stop: AtomicBool,
    conns: AtomicUsize,
    in_flight: AtomicUsize,
    bucket: Mutex<Bucket>,
    sessions: Mutex<HashMap<String, Session>>,
    clients: Mutex<HashMap<String, ClientSeen>>,
    connects: Mutex<Vec<String>>,
    /// `(client, uri)` of every guide read, oldest first, until the editor takes them for the audit.
    resource_reads: Mutex<Vec<(String, String)>>,
    waker: Mutex<Option<Waker>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct CallGuard<'a>(&'a Shared);

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.0.wake();
    }
}

impl Shared {
    pub(crate) fn revoked_flag(&self) -> Arc<AtomicBool> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .revoked
            .clone()
    }

    fn token_ok(&self, given: &str) -> bool {
        token_matches(
            &self
                .current
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .token,
            given.as_bytes(),
        )
    }

    fn wake(&self) {
        let waker = lock(&self.waker).clone();
        if let Some(w) = waker {
            w();
        }
    }

    pub(crate) fn send(&self, call: McpCall) -> Result<(), ()> {
        let sent = lock(&self.tx).send(call).map_err(|_| ());
        self.wake();
        sent
    }

    pub(crate) fn call_started(&self) -> CallGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        CallGuard(self)
    }

    pub(crate) fn note_client(&self, name: &str) {
        let now = Instant::now();
        let mut clients = lock(&self.clients);
        clients.retain(|_, c| now.duration_since(c.last_seen) < CONNECTED_WINDOW);
        let is_new = !clients.contains_key(name);
        if is_new && clients.len() >= MAX_CLIENTS {
            if let Some(oldest) = clients
                .values()
                .min_by_key(|c| c.last_seen)
                .map(|c| c.name.clone())
            {
                clients.remove(&oldest);
            }
        }
        let seen = clients
            .entry(name.to_string())
            .or_insert_with(|| ClientSeen {
                name: name.to_string(),
                calls: 0,
                last_seen: now,
            });
        seen.calls += 1;
        seen.last_seen = now;
        drop(clients);
        if is_new {
            lock(&self.connects).push(name.to_string());
            self.wake();
        }
    }

    pub(crate) fn note_resource_read(&self, client: &str, uri: &str) {
        let mut reads = lock(&self.resource_reads);
        if reads.len() >= MAX_RESOURCE_READS {
            reads.remove(0);
        }
        reads.push((client.to_string(), uri.to_string()));
        drop(reads);
        self.wake();
    }

    pub(crate) fn open_session(&self, client: &str) -> Result<String, SecretError> {
        let id = auth::random_text(SESSION_ID_BYTES)?;
        let mut sessions = lock(&self.sessions);
        if sessions.len() >= MAX_SESSIONS {
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|s| s.1.last_seen)
                .map(|s| s.0.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            id.clone(),
            Session {
                client: client.to_string(),
                last_seen: Instant::now(),
            },
        );
        Ok(id)
    }

    pub(crate) fn session_client(&self, id: &str) -> Option<String> {
        let mut sessions = lock(&self.sessions);
        let s = sessions.get_mut(id)?;
        s.last_seen = Instant::now();
        Some(s.client.clone())
    }

    fn close_session(&self, id: &str) -> bool {
        let closed = lock(&self.sessions).remove(id);
        let Some(s) = closed else {
            return false;
        };
        let still_open = lock(&self.sessions).values().any(|o| o.client == s.client);
        if !still_open {
            lock(&self.clients).remove(&s.client);
            self.wake();
        }
        true
    }

    fn take_rate_token(&self) -> bool {
        let now = Instant::now();
        let mut b = lock(&self.bucket);
        let refill = now.duration_since(b.at).as_secs_f64() * self.limits.rate_per_s;
        b.tokens = (b.tokens + refill).min(self.limits.rate_burst);
        b.at = now;
        if b.tokens < 1.0 {
            return false;
        }
        b.tokens -= 1.0;
        true
    }
}

pub struct McpHandle {
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
}

impl McpHandle {
    pub fn port(&self) -> u16 {
        self.shared.port
    }

    /// The old token answers 401 from this moment; calls it left in the queue no longer run.
    pub fn set_token(&self, t: Secret) {
        {
            let mut current = self
                .shared
                .current
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            current.revoked.store(true, Ordering::SeqCst);
            *current = Current {
                token: t,
                revoked: Arc::new(AtomicBool::new(false)),
            };
        }
        lock(&self.shared.sessions).clear();
        lock(&self.shared.clients).clear();
        self.shared.wake();
    }

    /// Agents seen recently: what the indicator in the editor shows.
    pub fn clients(&self) -> Vec<ClientSeen> {
        let now = Instant::now();
        let mut clients = lock(&self.shared.clients);
        clients.retain(|_, c| now.duration_since(c.last_seen) < CONNECTED_WINDOW);
        let mut out: Vec<ClientSeen> = clients.values().cloned().collect();
        out.sort_by(|a, b| {
            b.last_seen
                .cmp(&a.last_seen)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// Clients that appeared since the last look, for the audit log.
    pub fn take_connects(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.shared.connects))
    }

    /// `(client, uri)` of the guides read since the last look, in order, for the audit log: the
    /// first guide an agent read is what the trigger evals measure.
    pub fn take_resource_reads(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *lock(&self.shared.resource_reads))
    }

    /// Calls waiting for the editor: the UI keeps drawing frames while there are any.
    pub fn in_flight(&self) -> usize {
        self.shared.in_flight.load(Ordering::SeqCst)
    }

    /// Called from server threads when the editor has something to do; an idle UI draws no frames.
    pub fn set_waker(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.shared.waker) = Some(Arc::new(f));
    }

    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.revoked_flag().store(true, Ordering::SeqCst);
        if let Some(t) = self.acceptor.take() {
            let _ = t.join();
        }
    }
}

/// Both loopback addresses on one port; a machine without IPv6 still gets the IPv4 one.
fn bind_loopback(port: u16) -> io::Result<(Vec<TcpListener>, u16)> {
    for _ in 0..BIND_TRIES {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let bound = v4.local_addr()?.port();
        match TcpListener::bind((Ipv6Addr::LOCALHOST, bound)) {
            Ok(v6) => return Ok((vec![v4, v6], bound)),
            Err(e) if e.kind() == ErrorKind::AddrInUse => {
                if port != 0 {
                    return Err(e);
                }
            }
            Err(_) => return Ok((vec![v4], bound)),
        }
    }
    Err(io::Error::new(
        ErrorKind::AddrInUse,
        "no free port on both loopback addresses",
    ))
}

pub fn serve(cfg: McpConfig, tx: mpsc::Sender<McpCall>) -> io::Result<McpHandle> {
    serve_with(cfg, McpLimits::default(), tx)
}

pub fn serve_with(
    cfg: McpConfig,
    limits: McpLimits,
    tx: mpsc::Sender<McpCall>,
) -> io::Result<McpHandle> {
    let (listeners, port) = bind_loopback(cfg.port)?;
    for l in &listeners {
        l.set_nonblocking(true)?;
    }
    let shared = Arc::new(Shared {
        current: RwLock::new(Current {
            token: cfg.token,
            revoked: Arc::new(AtomicBool::new(false)),
        }),
        tx: Mutex::new(tx),
        port,
        bucket: Mutex::new(Bucket {
            tokens: limits.rate_burst,
            at: Instant::now(),
        }),
        limits,
        stop: AtomicBool::new(false),
        conns: AtomicUsize::new(0),
        in_flight: AtomicUsize::new(0),
        sessions: Mutex::new(HashMap::new()),
        clients: Mutex::new(HashMap::new()),
        connects: Mutex::new(Vec::new()),
        resource_reads: Mutex::new(Vec::new()),
        waker: Mutex::new(None),
    });
    let for_thread = shared.clone();
    let acceptor = thread::Builder::new()
        .name("mcp-accept".to_string())
        .spawn(move || accept_loop(&listeners, &for_thread))?;
    Ok(McpHandle {
        shared,
        acceptor: Some(acceptor),
    })
}

fn accept_loop(listeners: &[TcpListener], shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        let mut idle = true;
        for l in listeners {
            match l.accept() {
                Ok((stream, peer)) => {
                    idle = false;
                    admit(stream, peer.ip(), shared);
                }
                // nothing waiting, or a client that left between connect and accept
                Err(_) => {}
            }
        }
        if idle {
            thread::sleep(ACCEPT_POLL);
        }
    }
}

struct ConnGuard(Arc<Shared>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.conns.fetch_sub(1, Ordering::SeqCst);
    }
}

fn admit(mut stream: TcpStream, peer: IpAddr, shared: &Arc<Shared>) {
    if !peer.is_loopback() {
        return;
    }
    // a socket accepted from a non-blocking listener may inherit the mode
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let before = shared.conns.fetch_add(1, Ordering::SeqCst);
    let guard = ConnGuard(shared.clone());
    if before >= shared.limits.max_connections {
        let _ = stream.set_write_timeout(Some(REFUSE_WRITE_TIMEOUT));
        let _ = Resp::empty(503).with("Retry-After", "1").write(&mut stream);
        // closing with the request unread resets the connection and the client loses the 503
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let mut sink = [0u8; 4096];
        for _ in 0..16 {
            if !matches!(std::io::Read::read(&mut stream, &mut sink), Ok(n) if n > 0) {
                break;
            }
        }
        return;
    }
    let spawned = thread::Builder::new()
        .name("mcp-conn".to_string())
        .spawn(move || {
            let shared = guard.0.clone();
            handle_conn(&shared, stream);
            drop(guard);
        });
    drop(spawned);
}

fn handle_conn(shared: &Shared, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_SLICE));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let deadline = Instant::now() + shared.limits.read_timeout;
    let resp = match read_head(&mut stream, shared.limits.max_head, deadline) {
        Ok((head, rest)) => route(shared, &head, rest, &mut stream, deadline),
        Err(e) => match refusal(&e) {
            Some(r) => r,
            None => return,
        },
    };
    let _ = resp.write(&mut stream);
    let _ = stream.flush();
    linger(&mut stream);
}

fn refusal(e: &ReadError) -> Option<Resp> {
    match e {
        ReadError::TooLarge => Some(Resp::empty(431)),
        ReadError::Bad => Some(Resp::empty(400)),
        ReadError::Timeout => Some(Resp::empty(408)),
        ReadError::Closed => None,
    }
}

/// Closing with unread data resets the connection and can destroy the answer on its way out.
fn linger(stream: &mut TcpStream) {
    if stream.shutdown(Shutdown::Write).is_err() {
        return;
    }
    let until = Instant::now() + DRAIN_WAIT;
    let mut sink = [0u8; 4096];
    let mut left = DRAIN_BYTES;
    let _ = stream.set_read_timeout(Some(DRAIN_WAIT));
    while left > 0 && Instant::now() < until {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => return,
            Ok(n) => left = left.saturating_sub(n as u64),
        }
    }
}

fn path_of(target: &str) -> &str {
    target.split('?').next().unwrap_or(target)
}

fn forbidden() -> Resp {
    Resp::json(
        403,
        &json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "forbidden"}}),
    )
}

/// The order matters: a foreign page gets 403 even with a good token, and nothing without the
/// token learns about limits, paths or methods.
fn route(
    shared: &Shared,
    head: &Head,
    rest: Vec<u8>,
    stream: &mut TcpStream,
    deadline: Instant,
) -> Resp {
    if shared.stop.load(Ordering::SeqCst) {
        return Resp::empty(503);
    }
    if !shared.take_rate_token() {
        return Resp::empty(429).with("Retry-After", "1");
    }
    if head.count("host") != 1
        || !head
            .get("host")
            .is_some_and(|h| host_allowed(h, shared.port))
    {
        return forbidden();
    }
    if head.count("origin") > 1
        || head
            .get("origin")
            .is_some_and(|o| !origin_allowed(o, shared.port))
    {
        return forbidden();
    }
    let authorized = head.count("authorization") == 1
        && head
            .get("authorization")
            .and_then(bearer)
            .is_some_and(|t| shared.token_ok(t));
    if !authorized {
        return Resp::empty(401).with("WWW-Authenticate", "Bearer");
    }
    if path_of(&head.target) != ENDPOINT {
        return Resp::empty(404);
    }
    match head.method.as_str() {
        "POST" => post(shared, head, rest, stream, deadline),
        "DELETE" => match head.get("mcp-session-id") {
            Some(id) if shared.close_session(id) => Resp::empty(204),
            _ => Resp::empty(405).with("Allow", "POST"),
        },
        _ => Resp::empty(405).with("Allow", "POST"),
    }
}

fn post(
    shared: &Shared,
    head: &Head,
    rest: Vec<u8>,
    stream: &mut TcpStream,
    deadline: Instant,
) -> Resp {
    let json_type = head
        .get("content-type")
        .and_then(|c| c.split(';').next())
        .is_some_and(|c| c.trim().eq_ignore_ascii_case("application/json"));
    if !json_type {
        return Resp::empty(415);
    }
    if head.get("transfer-encoding").is_some() || head.count("content-length") == 0 {
        return Resp::empty(411);
    }
    if head.count("content-length") > 1 {
        return Resp::empty(400);
    }
    let Some(len) = head
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return Resp::empty(400);
    };
    if len > shared.limits.max_body {
        return Resp::empty(413);
    }
    let body = match read_body(stream, rest, len, deadline) {
        Ok(b) => b,
        Err(e) => return refusal(&e).unwrap_or_else(|| Resp::empty(400)),
    };
    rpc::dispatch(shared, head, &body)
}
