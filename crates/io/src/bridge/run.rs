// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spawns a bridge, streams its frames to a handler, enforces timeouts, cancel and exit checks.
//! The child is killed on any error, including handler errors and drop.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use web_time::{Duration, Instant};

use ddc_bridge_proto::{Curve, Element, Frame, FrameReader, ProtoError, CANCEL_LINE, PROTOCOL};

use super::BridgeError;

/// Totals one import may reach; above any of them the run stops with [`BridgeError::TooLarge`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Bytes read from the bridge's stdout.
    pub bytes: u64,
    pub elements: u64,
    /// Plan points and mesh positions over all elements.
    pub vertices: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 2 << 30,
            elements: 5_000_000,
            vertices: 200_000_000,
        }
    }
}

fn vertices(e: &Element) -> u64 {
    let n = match e {
        Element::Wall { axis, .. } => match axis {
            Curve::Line { .. } => 2,
            _ => 3,
        },
        Element::Opening { .. } => 0,
        Element::Slab { outer, holes, .. } => {
            outer.len() + holes.iter().map(Vec::len).sum::<usize>()
        }
        Element::Room { boundary, .. } => boundary.len(),
        Element::Generic { positions, .. } => positions.len(),
    };
    n as u64
}

struct Counting<R>(R, Arc<AtomicU64>);

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(b)?;
        self.1.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

#[derive(Clone)]
pub struct RunOptions {
    /// Longest silence between frames before the bridge counts as hung.
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
    /// Time a bridge gets to exit after End or after a cancel request.
    pub grace: Duration,
    pub view: Option<String>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub max_frame: u32,
    pub limits: Limits,
    /// Extra arguments after the protocol ones (test faults of the sample bridge).
    pub extra_args: Vec<String>,
    /// Memory ceiling of the child process, bytes (job object on Windows); a breach ends the
    /// run as a failure.
    pub memory_limit: Option<u64>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(120),
            total_timeout: Duration::from_secs(4 * 3600),
            grace: Duration::from_secs(3),
            view: None,
            cancel: None,
            max_frame: ddc_bridge_proto::MAX_FRAME,
            limits: Limits::default(),
            extra_args: vec![],
            memory_limit: None,
        }
    }
}

pub(crate) struct KillOnDrop(pub(crate) Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl KillOnDrop {
    fn wait_for(&mut self, d: Duration) -> Option<std::process::ExitStatus> {
        let end = Instant::now() + d;
        loop {
            if let Ok(Some(s)) = self.0.try_wait() {
                return Some(s);
            }
            if Instant::now() >= end {
                let _ = self.0.kill();
                let _ = self.0.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Polls every `poll` until the child exits, `stop()` asks to stop it, or `timeout` passes;
    /// a stopped child is killed and reaped. The one wait loop of every helper process.
    pub(crate) fn supervise(
        &mut self,
        timeout: Duration,
        poll: Duration,
        mut stop: impl FnMut() -> bool,
    ) -> std::io::Result<Waited> {
        let start = Instant::now();
        let ended = loop {
            if let Some(s) = self.0.try_wait()? {
                return Ok(Waited::Exited(s));
            }
            if stop() {
                break Waited::Stopped;
            }
            if start.elapsed() >= timeout {
                break Waited::TimedOut;
            }
            std::thread::sleep(poll);
        };
        let _ = self.0.kill();
        let _ = self.0.wait();
        Ok(ended)
    }
}

/// How [`KillOnDrop::supervise`] ended.
#[derive(Debug)]
pub(crate) enum Waited {
    Exited(std::process::ExitStatus),
    Stopped,
    TimedOut,
}

/// Minimal environment: the bridge sees no host secrets, only what a process needs to start.
/// Working directory = the executable's (private, signed-only) folder; PATH = system folders, so
/// no library is found anywhere the package does not control.
fn command(exe: &Path, input: &Path, o: &RunOptions) -> Command {
    let mut c = Command::new(exe);
    c.arg("--protocol").arg(PROTOCOL.to_string());
    c.arg("--minor").arg(ddc_bridge_proto::MINOR.to_string());
    c.arg("--input").arg(input);
    if let Some(v) = &o.view {
        c.arg("--view").arg(v);
    }
    c.args(&o.extra_args);
    confine_command(&mut c, exe);
    c
}

/// Same trust model for every signed process: the reduced environment and folder rules above.
pub(crate) fn confine_command(c: &mut Command, exe: &Path) {
    c.env_clear();
    for k in ["SystemRoot", "SYSTEMROOT", "TEMP", "TMP", "LANG"] {
        if let Some(v) = std::env::var_os(k) {
            c.env(k, v);
        }
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        let r = std::path::PathBuf::from(root);
        if let Ok(p) = std::env::join_paths([r.join("System32"), r]) {
            c.env("PATH", p);
        }
    }
    #[cfg(not(windows))]
    c.env("PATH", "/usr/bin:/bin");
    match exe.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(d) => c.current_dir(d),
        None => c.current_dir(std::env::temp_dir()),
    };
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
}

/// Runs the bridge; `on_frame` sees every frame from Hello to End in order.
pub fn run(
    exe: &Path,
    input: &Path,
    o: &RunOptions,
    on_frame: &mut dyn FnMut(Frame) -> Result<(), BridgeError>,
) -> Result<(), BridgeError> {
    let child = command(exe, input, o)
        .spawn()
        .map_err(|e| BridgeError::Spawn(e.to_string()))?;
    let _job = o
        .memory_limit
        .and_then(|m| super::limit::confine(&child, m));
    let mut child = KillOnDrop(child);
    let mut stdin = child.0.stdin.take();
    let stdout = child.0.stdout.take().expect("piped");
    let mut stderr = child.0.stderr.take().expect("piped");
    let err_tail = std::thread::spawn(move || {
        let mut keep = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            keep.extend_from_slice(&buf[..n]);
            if keep.len() > 8192 {
                keep.drain(..keep.len() - 8192);
            }
        }
        String::from_utf8_lossy(&keep).trim().to_string()
    });
    // bounded queue: a fast bridge waits for the importer, memory stays flat
    let (tx, rx) = sync_channel::<Result<Option<Frame>, ProtoError>>(32);
    let max_frame = o.max_frame;
    let lim = o.limits;
    let bytes = Arc::new(AtomicU64::new(0));
    let counted = Counting(stdout, bytes.clone());
    let over = {
        let b = bytes.clone();
        move || b.load(Ordering::Relaxed) > lim.bytes
    };
    let reader_over = over.clone();
    std::thread::spawn(move || {
        let mut r = FrameReader::new(std::io::BufReader::with_capacity(1 << 16, counted));
        r.max_frame = max_frame;
        loop {
            let f = r.next_frame();
            let stop = !matches!(f, Ok(Some(_))) || reader_over();
            if tx.send(f).is_err() || stop {
                return;
            }
        }
    });

    let start = Instant::now();
    let mut cancel_sent: Option<Instant> = None;
    let mut first = true;
    let tick = Duration::from_millis(50);
    let mut last = Instant::now();
    let (mut elements, mut verts) = (0u64, 0u64);
    let fail = |child: &mut KillOnDrop, e: BridgeError| {
        let _ = child.0.kill();
        Err(e)
    };
    loop {
        let cancelled = o.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
        if cancelled && cancel_sent.is_none() {
            if let Some(w) = stdin.as_mut() {
                let _ = w.write_all(CANCEL_LINE);
                let _ = w.flush();
            }
            stdin = None;
            cancel_sent = Some(Instant::now());
        }
        if let Some(t) = cancel_sent {
            if t.elapsed() >= o.grace {
                return fail(&mut child, BridgeError::Cancelled);
            }
        }
        if start.elapsed() >= o.total_timeout {
            return fail(&mut child, BridgeError::Timeout("total time limit".into()));
        }
        if last.elapsed() >= o.idle_timeout {
            return fail(
                &mut child,
                BridgeError::Timeout(format!("no data for {:?}", o.idle_timeout)),
            );
        }
        let got = rx.recv_timeout(tick);
        if over() {
            return fail(
                &mut child,
                BridgeError::TooLarge(format!("more than {} bytes", lim.bytes)),
            );
        }
        if let Ok(Ok(Some(Frame::Elements(v)))) = &got {
            elements += v.len() as u64;
            verts += v.iter().map(vertices).sum::<u64>();
            if elements > lim.elements {
                let m = format!("more than {} elements", lim.elements);
                return fail(&mut child, BridgeError::TooLarge(m));
            }
            if verts > lim.vertices {
                let m = format!("more than {} vertices", lim.vertices);
                return fail(&mut child, BridgeError::TooLarge(m));
            }
        }
        match got {
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return fail(&mut child, BridgeError::Protocol("reader stopped".into()))
            }
            Ok(Ok(Some(f))) => {
                last = Instant::now();
                if cancel_sent.is_some() {
                    continue; // drain until the bridge exits or grace runs out
                }
                if first && !matches!(f, Frame::Hello(_)) {
                    return fail(
                        &mut child,
                        BridgeError::Protocol("first frame is not Hello".into()),
                    );
                }
                first = false;
                let end = matches!(f, Frame::End(_));
                if let Err(e) = on_frame(f) {
                    return fail(&mut child, e);
                }
                if end {
                    drop(stdin);
                    let st = child.wait_for(o.grace);
                    return match st {
                        Some(s) if s.success() => Ok(()),
                        Some(s) => Err(BridgeError::Crashed(format!(
                            "exit {:?} after End: {}",
                            s.code(),
                            err_tail.join().unwrap_or_default()
                        ))),
                        None => Err(BridgeError::Timeout("did not exit after End".into())),
                    };
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) if cancel_sent.is_some() => {
                child.wait_for(o.grace);
                return Err(BridgeError::Cancelled);
            }
            Ok(Ok(None)) | Ok(Err(ProtoError::Truncated)) => {
                let st = child.wait_for(o.grace);
                let tail = err_tail.join().unwrap_or_default();
                return Err(match st {
                    Some(s) if s.success() => {
                        BridgeError::Protocol("stream ended without End".into())
                    }
                    Some(s) => BridgeError::Crashed(format!("exit {:?}: {tail}", s.code())),
                    None => BridgeError::Crashed(format!("output closed, process hung: {tail}")),
                });
            }
            Ok(Err(e)) => return fail(&mut child, BridgeError::Protocol(e.to_string())),
        }
    }
}
