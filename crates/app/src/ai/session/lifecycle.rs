//! End of a session: the worker is cancelled and joined with a bounded wait, and snapshot
//! copies of in-memory projects (`ddc-ai-snap-<pid>-<n>.db`) never outlive their session.

use super::{Core, SourceCopy, ToWorker, UiSession};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
#[cfg(test)]
use std::thread;
use std::time::Duration;

/// A snapshot lives from `snapshot_to` to the worker's `adopt`: an older one of another
/// process is left over, a younger one may still be on its way to a running editor.
pub const SNAP_STALE_AGE: Duration = Duration::from_secs(3600);

const SNAP_PREFIX: &str = "ddc-ai-snap-";
const SNAP_SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

impl UiSession {
    /// Ends the session and waits up to `wait` for the worker. `false`: it is still inside a
    /// call and was left to finish alone; the handle is kept, so a later call can still join it.
    pub fn finish(&mut self, wait: Duration) -> bool {
        self.end();
        // a closed inbox wakes a worker blocked on it, dropped repliers one waiting for a shot
        self.tx = mpsc::channel().0;
        self.snapshots.clear();
        let Some(done) = &self.done else {
            return true;
        };
        match done.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!(
                    "assistant worker: still in a call {} ms after the session ended, left to finish alone",
                    wait.as_millis()
                );
                false
            }
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.done = None;
                if let Some(j) = self.join.take() {
                    // the guard already logged the panic text and place
                    if !matches!(j.join(), Ok(Some(()))) {
                        eprintln!("assistant worker: ended with a panic");
                    }
                }
                true
            }
        }
    }
}

impl Core {
    /// What the ended worker never got to: queued snapshots are removed, not adopted.
    pub(super) fn discard_pending(&mut self) {
        while let Some(m) = self.deferred.pop_front() {
            discard_msg(m);
        }
        while let Ok(m) = self.rx.try_recv() {
            discard_msg(m);
        }
    }
}

/// A message no worker will handle: only a snapshot copy made for it is cleaned up.
pub(super) fn discard_msg(m: ToWorker) {
    if let ToWorker::Sync {
        source: SourceCopy::Done(p),
        ..
    } = m
    {
        remove_snap(&p);
    }
}

/// Removes a snapshot and its SQLite sidecars; a path of any other name is never touched.
pub(super) fn remove_snap(p: &Path) {
    if !p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| snap_pid(n).is_some())
    {
        return;
    }
    let _ = fs::remove_file(p);
    for s in SNAP_SIDECARS {
        let mut side = p.as_os_str().to_owned();
        side.push(s);
        let _ = fs::remove_file(PathBuf::from(side));
    }
}

/// The pid in `ddc-ai-snap-<pid>-<n>.db` (or one of its sidecars); `None` for any other name.
fn snap_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(SNAP_PREFIX)?;
    let stem = rest.strip_suffix(".db").or_else(|| {
        SNAP_SIDECARS
            .iter()
            .find_map(|s| rest.strip_suffix(s)?.strip_suffix(".db"))
    })?;
    let (pid, seq) = stem.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid) || !digits(seq) {
        return None;
    }
    pid.parse().ok()
}

/// Removes left-over snapshot files in `dir`: only that exact name pattern, only files, never
/// this process's own (a live session may hold one), never one younger than `min_age`.
pub fn sweep_stale_snapshots(dir: &Path, min_age: Duration) -> usize {
    let me = std::process::id();
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(pid) = name.to_str().and_then(snap_pid) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(meta) = e.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if meta.is_file() && old && fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Once at startup, off the UI thread: a full temp folder must not delay the first frame.
pub fn sweep_on_start() {
    let _ = crate::jobs::spawn_guarded(
        "ai-snap-sweep",
        || {
            let n = sweep_stale_snapshots(&std::env::temp_dir(), SNAP_STALE_AGE);
            if n > 0 {
                eprintln!("assistant: removed {n} stale snapshot files");
            }
        },
        |_| {},
    );
}

#[cfg(test)]
mod tests {
    use super::super::{SessionCfg, UiEvent, ViewCtx, CONFIRM_TTL, JOIN_WAIT};
    use super::*;
    use crate::editor::Editor;
    use ddc_ai::catalog;
    use ddc_ai::fake::{self, Script, ScriptedProvider};
    use ddc_ai::provider::{ChatRequest, FinishReason, Flow, ProviderError, StreamEvent, Usage};
    use ddc_ai::{Limits, Mode, Provider};
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use web_time::{Instant, SystemTime};

    const WAIT: Duration = Duration::from_secs(60);
    static N: AtomicU64 = AtomicU64::new(0);

    fn next() -> u64 {
        N.fetch_add(1, Ordering::Relaxed)
    }

    /// A model call that hangs: `cooperative` stops at the cancel, otherwise only at `release`
    /// and then answers anyway, as a slow network would.
    struct Hold {
        cooperative: bool,
        calls: AtomicU32,
        entered: Mutex<mpsc::Sender<()>>,
        release: Mutex<mpsc::Receiver<()>>,
        left: Mutex<mpsc::Sender<()>>,
    }

    struct Probe {
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        left: mpsc::Receiver<()>,
    }

    fn hold(cooperative: bool) -> (Arc<Hold>, Probe) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (left_tx, left) = mpsc::channel();
        let h = Arc::new(Hold {
            cooperative,
            calls: AtomicU32::new(0),
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
            left: Mutex::new(left_tx),
        });
        (
            h,
            Probe {
                entered,
                release,
                left,
            },
        )
    }

    impl Hold {
        fn until_stopped(&self, sink: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<(), ProviderError> {
            let until = Instant::now() + WAIT;
            while Instant::now() < until {
                if matches!(sink(StreamEvent::TextDelta(".".to_string())), Flow::Stop) {
                    return Err(ProviderError::Cancelled);
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }

        /// Ignores every `Flow::Stop`: the late answer is the worker's to drop.
        fn late_answer(&self, sink: &mut dyn FnMut(StreamEvent) -> Flow) -> Result<(), ProviderError> {
            let _ = self.release.lock().unwrap().recv_timeout(WAIT);
            let id = "late".to_string();
            sink(StreamEvent::TextDelta("late".to_string()));
            sink(StreamEvent::ToolStart {
                id: id.clone(),
                name: "create_walls".to_string(),
            });
            sink(StreamEvent::ToolArgsDelta {
                id: id.clone(),
                json: "{}".to_string(),
            });
            sink(StreamEvent::ToolEnd { id });
            sink(StreamEvent::Usage(Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Usage::default()
            }));
            sink(StreamEvent::Finish(FinishReason::ToolCalls));
            Ok(())
        }
    }

    impl Provider for Hold {
        fn stream(
            &self,
            _req: &ChatRequest<'_>,
            sink: &mut dyn FnMut(StreamEvent) -> Flow,
        ) -> Result<(), ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _ = self.entered.lock().unwrap().send(());
            let r = if self.cooperative {
                self.until_stopped(sink)
            } else {
                self.late_answer(sink)
            };
            let _ = self.left.lock().unwrap().send(());
            r
        }

        fn list_models(&self) -> Result<Vec<String>, ProviderError> {
            Ok(vec![])
        }
    }

    fn cfg(provider: Arc<dyn Provider>) -> SessionCfg {
        SessionCfg {
            provider,
            profile: fake::profile(),
            limits: Limits::default(),
            session: format!("life{}", next()),
            allow_snapshots: Arc::new(AtomicBool::new(false)),
            extra_generators: vec![],
            extra_rules: vec![],
            keep_text: false,
            confirm_ttl: CONFIRM_TTL,
            target: catalog::Target::Main,
            auto_replan: false,
        }
    }

    fn view(ed: &Editor) -> ViewCtx {
        ViewCtx {
            view: "plan".to_string(),
            level: ed.level,
            units: ed.tool.units,
            selection: vec![],
            bounds: None,
            ..ViewCtx::default()
        }
    }

    fn start(ed: &mut Editor, provider: Arc<dyn Provider>) -> UiSession {
        let v = view(ed);
        let mut s = UiSession::start(ed, cfg(provider), v).unwrap();
        assert!(s.wait_idle(ed, WAIT), "the session did not open");
        s
    }

    /// The session's own copy folder: it goes when the worker's `PlanCopies` drops.
    fn copy_dir(s: &UiSession) -> PathBuf {
        let (base, _) = s.paths.clone().expect("the worker reported its copies");
        base.parent().unwrap().to_path_buf()
    }

    fn turn_in_flight(s: &mut UiSession, ed: &mut Editor, probe: &Probe) {
        let v = view(ed);
        s.send_turn(ed, "go".to_string(), v, Mode::Propose).unwrap();
        probe
            .entered
            .recv_timeout(WAIT)
            .expect("the turn reached the model");
    }

    fn eventually(what: &str, ok: impl Fn() -> bool) {
        let until = Instant::now() + WAIT;
        while !ok() {
            assert!(Instant::now() < until, "{what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn snap_name(pid: u32, n: u64) -> String {
        format!("{SNAP_PREFIX}{pid}-{n}.db")
    }

    fn sweep_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-life-{tag}-{}-{}", std::process::id(), next()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(dir: &Path, name: &str, age: Duration) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"x").unwrap();
        let f = fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
        p
    }

    fn other_pid() -> u32 {
        std::process::id().wrapping_add(1)
    }

    #[test]
    fn drop_session_cancels_and_joins_worker() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (p, probe) = hold(true);
        let mut s = start(&mut ed, p);
        let dir = copy_dir(&s);
        turn_in_flight(&mut s, &mut ed, &probe);
        let t = Instant::now();
        drop(s);
        assert!(t.elapsed() < JOIN_WAIT, "a cancelled call is joined, not waited out");
        assert!(probe.left.try_recv().is_ok(), "drop returned before the call ended");
        assert!(!dir.exists(), "the worker was joined: its copies are gone");
    }

    #[test]
    fn drop_waits_at_most_join_wait_then_worker_cleans_up() {
        let mut ed = Editor::new_in_memory().unwrap();
        let (p, probe) = hold(false);
        let mut s = start(&mut ed, p);
        let dir = copy_dir(&s);
        turn_in_flight(&mut s, &mut ed, &probe);
        // a newer copy queued behind the call: the ended worker removes it instead of adopting it
        let snap = std::env::temp_dir().join(snap_name(std::process::id(), 900_000 + next()));
        fs::write(&snap, b"x").unwrap();
        s.send(ToWorker::Sync {
            source: SourceCopy::Done(snap.clone()),
            head: 0,
            changes: None,
        });
        let t = Instant::now();
        drop(s);
        let took = t.elapsed();
        assert!(took >= JOIN_WAIT - Duration::from_millis(100), "returned early: {took:?}");
        assert!(took < JOIN_WAIT + Duration::from_secs(2), "the UI waited {took:?}");
        probe.release.send(()).unwrap();
        probe.left.recv_timeout(WAIT).expect("the call returns");
        eventually("the queued snapshot was left behind", || !snap.exists());
        eventually("the worker's copies were left behind", || !dir.exists());
    }

    #[test]
    fn project_switch_old_worker_lands_nothing() {
        let mut ed1 = Editor::new_in_memory().unwrap();
        let before = (ed1.model.change_head(), ed1.model.op_top().unwrap());
        let (p, probe) = hold(false);
        let mut old = start(&mut ed1, p.clone());
        turn_in_flight(&mut old, &mut ed1, &probe);
        old.pump_core(&mut ed1);
        old.events.clear();
        assert!(!old.finish(Duration::from_millis(50)), "the call is still out");

        let mut ed2 = Editor::new_in_memory().unwrap();
        let fresh = ScriptedProvider::new(vec![vec![Script::Text("fresh".to_string())]]);
        let mut new = start(&mut ed2, fresh);
        probe.release.send(()).unwrap();
        probe.left.recv_timeout(WAIT).expect("the late answer arrives");
        assert!(old.finish(WAIT), "the worker ends once the call returns");

        old.pump_core(&mut ed1);
        assert!(old.events.is_empty(), "{} late events landed", old.events.len());
        assert!(old.plan().is_none(), "a late draft landed");
        assert_eq!(p.calls.load(Ordering::SeqCst), 1, "no request after the stop");
        assert_eq!(
            (ed1.model.change_head(), ed1.model.op_top().unwrap()),
            before,
            "the old project changed"
        );

        let v = view(&ed2);
        new.send_turn(&ed2, "go".to_string(), v, Mode::Propose).unwrap();
        assert!(new.wait_idle(&mut ed2, WAIT));
        let texts: Vec<&str> = new
            .events
            .iter()
            .filter_map(|e| match e {
                UiEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("fresh")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("late")), "{texts:?}");
        assert!(new.plan().is_none());
    }

    #[test]
    fn stale_snap_files_swept_on_start() {
        let d = sweep_dir("stale");
        let old = Duration::from_secs(2 * 3600);
        let gone = [
            touch(&d, &snap_name(other_pid(), 3), old),
            touch(&d, &format!("{}-wal", snap_name(other_pid(), 3)), old),
        ];
        let kept = [
            touch(&d, "ddc-ai-snap-x-1.db", old),
            touch(&d, "ddc-ai-snap-5-6.dbx", old),
            touch(&d, "ddc-ai-snap-5.db", old),
            touch(&d, "notes.db", old),
        ];
        let folder = d.join(format!("ddc-ai-{}-s-0", other_pid()));
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(sweep_stale_snapshots(&d, SNAP_STALE_AGE), 2);
        for p in &gone {
            assert!(!p.exists(), "{p:?} stayed");
        }
        for p in &kept {
            assert!(p.exists(), "{p:?} was removed");
        }
        assert!(folder.exists(), "a copy folder is not a snapshot");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn live_snapshot_not_swept() {
        let d = sweep_dir("live");
        let own = touch(&d, &snap_name(std::process::id(), 7), Duration::from_secs(2 * 3600));
        let young = touch(&d, &snap_name(other_pid(), 8), Duration::from_secs(60));
        assert_eq!(sweep_stale_snapshots(&d, SNAP_STALE_AGE), 0);
        assert!(own.exists(), "this process's snapshot may belong to a live session");
        assert!(young.exists(), "a young snapshot may be on its way to a running editor");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_snap_touches_only_snapshots() {
        let d = sweep_dir("rm");
        let other = touch(&d, "project.db", Duration::ZERO);
        remove_snap(&other);
        assert!(other.exists());
        let snap = touch(&d, &snap_name(other_pid(), 9), Duration::ZERO);
        let wal = touch(&d, &format!("{}-wal", snap_name(other_pid(), 9)), Duration::ZERO);
        remove_snap(&snap);
        assert!(!snap.exists() && !wal.exists());
        let _ = fs::remove_dir_all(&d);
    }
}
