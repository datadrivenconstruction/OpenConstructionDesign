// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Recent projects: what is on disk, found out off the UI thread.
//!
//! A recent file may sit on a share or a drive that is switched off; asking the disk for it can
//! take seconds. The start screen therefore never touches those paths itself: [`RecentProbe`]
//! asks a [`Prober`] on a worker thread per path, keeps the answers (cache), gives up waiting
//! after [`TIMEOUT`] and tries again later. A file that is gone ([`State::Missing`]) is told
//! apart from one that cannot be reached right now ([`State::Unavailable`]); nothing here ever
//! changes the recent lists.
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use web_time::{Duration, Instant, SystemTime};

/// How long a probe may take before the card says "not available now".
pub const TIMEOUT: Duration = Duration::from_millis(2500);
/// A found or missing answer is asked again after this long.
pub const FRESH: Duration = Duration::from_secs(60);
/// An unavailable answer is asked again after this long.
pub const RETRY_AFTER: Duration = Duration::from_secs(15);
/// Probe threads alive at once, hung ones included (a backstop over the one-per-place rule).
pub const MAX_IN_FLIGHT: usize = 8;

/// The place a probe can hang on: drive or share plus the first folder (`C:\Users`,
/// `\\server\share\dir`, `/mnt`). At most one probe per place runs at a time.
pub fn place_of(file: &Path) -> PathBuf {
    let mut k = PathBuf::new();
    for c in file.components() {
        k.push(c.as_os_str());
        if matches!(c, Component::Normal(_)) {
            break;
        }
    }
    k
}

/// What one look at the file gave.
#[derive(Clone, Debug, PartialEq)]
pub enum Stat {
    Found {
        modified: Option<SystemTime>,
        size: u64,
    },
    NotFound,
    /// Any other error: offline share, access denied, I/O failure.
    Failed,
}

/// What the start screen knows about a recent file.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Present {
        modified: Option<SystemTime>,
        size: u64,
    },
    /// The file is gone while its folder answers.
    Missing,
    /// The folder, drive or share does not answer (or too slowly); the file may well exist.
    Unavailable,
}

/// The disk side, replaceable in tests.
pub trait Prober: Send + Sync + 'static {
    fn stat(&self, file: &Path) -> Stat;
    /// Whether the folder answers as a folder.
    fn dir_reachable(&self, dir: &Path) -> bool;
}

/// The real disk.
pub struct DiskProber;

impl Prober for DiskProber {
    fn stat(&self, file: &Path) -> Stat {
        match std::fs::metadata(file) {
            Ok(m) => Stat::Found {
                modified: m.modified().ok(),
                size: m.len(),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stat::NotFound,
            Err(_) => Stat::Failed,
        }
    }

    fn dir_reachable(&self, dir: &Path) -> bool {
        std::fs::metadata(dir).is_ok_and(|m| m.is_dir())
    }
}

/// Pure rule: a look at the file and, only when it is not found, whether its folder answers.
pub fn recent_state(stat: Stat, parent_reachable: impl FnOnce() -> bool) -> State {
    match stat {
        Stat::Found { modified, size } => State::Present { modified, size },
        Stat::NotFound if parent_reachable() => State::Missing,
        Stat::NotFound | Stat::Failed => State::Unavailable,
    }
}

fn probe_now(p: &dyn Prober, file: &Path) -> State {
    recent_state(p.stat(file), || {
        // a bare file name has the current folder as its parent
        let dir = file
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        p.dir_reachable(dir)
    })
}

struct Slot {
    state: Option<State>,
    /// When `state` was set.
    at: Instant,
    in_flight: bool,
    /// The running probe passed the timeout: its thread may be parked on a dead share.
    timed_out: bool,
    started: Instant,
}

pub struct RecentProbe {
    prober: Arc<dyn Prober>,
    timeout: Duration,
    slots: HashMap<PathBuf, Slot>,
    tx: Sender<(PathBuf, State)>,
    rx: Receiver<(PathBuf, State)>,
}

impl Default for RecentProbe {
    fn default() -> Self {
        Self::new(Arc::new(DiskProber), TIMEOUT)
    }
}

impl RecentProbe {
    pub fn new(prober: Arc<dyn Prober>, timeout: Duration) -> Self {
        let (tx, rx) = channel();
        Self {
            prober,
            timeout,
            slots: HashMap::new(),
            tx,
            rx,
        }
    }

    /// One frame: takes finished answers, gives up on slow ones, starts the probes that are due.
    /// `paths` are the files on screen. At most one probe per place ([`place_of`]) runs at a
    /// time and at most [`MAX_IN_FLIGHT`] overall, so a dead share costs one parked thread, not
    /// one per file; its other files read as unavailable once that probe timed out.
    pub fn update(&mut self, paths: &[PathBuf], now: Instant) {
        self.collect(now);
        for slot in self.slots.values_mut() {
            if slot.in_flight
                && !slot.timed_out
                && now.saturating_duration_since(slot.started) >= self.timeout
            {
                slot.timed_out = true;
                slot.state = Some(State::Unavailable);
                slot.at = now;
            }
        }
        // place -> its running probe has timed out
        let mut places: HashMap<PathBuf, bool> = HashMap::new();
        for (p, s) in self.slots.iter().filter(|(_, s)| s.in_flight) {
            *places.entry(place_of(p)).or_insert(false) |= s.timed_out;
        }
        let mut running = self.slots.values().filter(|s| s.in_flight).count();
        for p in paths {
            let slot = self.slots.entry(p.clone()).or_insert(Slot {
                state: None,
                at: now,
                in_flight: false,
                timed_out: false,
                started: now,
            });
            if slot.in_flight {
                continue;
            }
            let due = match &slot.state {
                None => true,
                Some(State::Unavailable) => now.saturating_duration_since(slot.at) >= RETRY_AFTER,
                Some(_) => now.saturating_duration_since(slot.at) >= FRESH,
            };
            if !due {
                continue;
            }
            let place = place_of(p);
            match places.get(&place) {
                // the place hangs: no second thread, the file is not reachable right now
                Some(true) => {
                    slot.state = Some(State::Unavailable);
                    slot.at = now;
                    continue;
                }
                Some(false) => continue,
                None if running >= MAX_IN_FLIGHT => continue,
                None => {}
            }
            places.insert(place, false);
            running += 1;
            slot.in_flight = true;
            slot.timed_out = false;
            slot.started = now;
            let (file, tx, prober) = (p.clone(), self.tx.clone(), self.prober.clone());
            let (file_panic, tx_panic) = (p.clone(), self.tx.clone());
            let spawned = crate::jobs::spawn_guarded(
                "recent-probe",
                move || {
                    let st = probe_now(prober.as_ref(), &file);
                    let _ = tx.send((file, st));
                },
                move |_| {
                    let _ = tx_panic.send((file_panic, State::Unavailable));
                },
            );
            if spawned.is_err() {
                slot.in_flight = false;
                slot.state = Some(State::Unavailable);
                slot.at = now;
            }
        }
    }

    fn collect(&mut self, now: Instant) {
        while let Ok((p, st)) = self.rx.try_recv() {
            self.apply(p, st, now);
        }
    }

    fn apply(&mut self, p: PathBuf, st: State, now: Instant) {
        if let Some(slot) = self.slots.get_mut(&p) {
            slot.in_flight = false;
            slot.timed_out = false;
            slot.state = Some(st);
            slot.at = now;
        }
    }

    /// `None`: no answer yet, the card draws a skeleton.
    pub fn state(&self, p: &Path) -> Option<&State> {
        self.slots.get(p)?.state.as_ref()
    }

    /// Something is still being asked within its timeout: the screen polls again soon. A probe
    /// parked on a dead share does not keep the screen repainting.
    pub fn busy(&self) -> bool {
        self.slots.values().any(|s| s.in_flight && !s.timed_out)
    }

    /// "Retry": forget the answer so the next [`update`](Self::update) asks again at once. While
    /// the last probe of this file still hangs the card stays unavailable (no second thread).
    pub fn retry(&mut self, p: &Path, now: Instant) {
        let Some(slot) = self.slots.get_mut(p).filter(|s| !s.in_flight) else {
            return;
        };
        slot.state = None;
        slot.started = now;
        slot.at = now;
    }

    /// Test helper: waits for every running probe to answer (no sleeping).
    #[cfg(test)]
    fn drain(&mut self, now: Instant) {
        while self.slots.values().any(|s| s.in_flight) {
            let Ok((p, st)) = self.rx.recv() else { break };
            self.apply(p, st, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Clone)]
    enum Mode {
        Found,
        Gone,
        /// The folder does not answer either.
        Offline,
        Panic,
    }

    struct Fake {
        mode: Mutex<Mode>,
        calls: AtomicUsize,
        /// When set, `stat` waits for a message first: a share that does not answer.
        gate: Mutex<Option<Receiver<()>>>,
    }

    impl Fake {
        fn new(mode: Mode) -> Arc<Self> {
            Arc::new(Self {
                mode: Mutex::new(mode),
                calls: AtomicUsize::new(0),
                gate: Mutex::new(None),
            })
        }
        fn set(&self, m: Mode) {
            *self.mode.lock().unwrap() = m;
        }
    }

    impl Prober for Arc<Fake> {
        fn stat(&self, _: &Path) -> Stat {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(g) = self.gate.lock().unwrap().take() {
                let _ = g.recv();
            }
            match self.mode.lock().unwrap().clone() {
                Mode::Found => Stat::Found {
                    modified: None,
                    size: 7,
                },
                Mode::Gone | Mode::Offline => Stat::NotFound,
                Mode::Panic => panic!("probe failed on purpose"),
            }
        }
        fn dir_reachable(&self, _: &Path) -> bool {
            !matches!(*self.mode.lock().unwrap(), Mode::Offline)
        }
    }

    fn engine(f: &Arc<Fake>) -> RecentProbe {
        RecentProbe::new(Arc::new(f.clone()), Duration::from_secs(2))
    }

    fn one() -> Vec<PathBuf> {
        vec![PathBuf::from("Z:/share/a.ddc")]
    }

    #[test]
    fn pure_rule_tells_missing_from_unavailable() {
        let found = Stat::Found {
            modified: None,
            size: 3,
        };
        assert_eq!(
            recent_state(found, || panic!("folder not asked")),
            State::Present {
                modified: None,
                size: 3
            }
        );
        assert_eq!(recent_state(Stat::NotFound, || true), State::Missing);
        assert_eq!(recent_state(Stat::NotFound, || false), State::Unavailable);
        assert_eq!(
            recent_state(Stat::Failed, || panic!("folder not asked")),
            State::Unavailable
        );
    }

    #[test]
    fn exists_is_present_and_shows_a_skeleton_first() {
        let f = Fake::new(Mode::Found);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        assert_eq!(e.state(&paths[0]), None);
        e.update(&paths, t0);
        assert!(e.busy());
        e.drain(t0);
        assert_eq!(
            e.state(&paths[0]),
            Some(&State::Present {
                modified: None,
                size: 7
            })
        );
        assert!(!e.busy());
    }

    #[test]
    fn missing_file_in_reachable_folder_is_missing() {
        let f = Fake::new(Mode::Gone);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        assert_eq!(e.state(&paths[0]), Some(&State::Missing));
    }

    #[test]
    fn missing_file_in_dead_folder_is_unavailable() {
        let f = Fake::new(Mode::Offline);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        assert_eq!(e.state(&paths[0]), Some(&State::Unavailable));
    }

    #[test]
    fn slow_probe_times_out_to_unavailable_without_blocking() {
        let f = Fake::new(Mode::Found);
        let (release, gate) = channel();
        *f.gate.lock().unwrap() = Some(gate);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        // the probe is parked; the frame goes on
        e.update(&paths, t0 + Duration::from_secs(1));
        assert_eq!(e.state(&paths[0]), None, "still waiting inside the timeout");
        e.update(&paths, t0 + Duration::from_secs(2));
        assert_eq!(e.state(&paths[0]), Some(&State::Unavailable));
        // the worker may not have reached `stat` yet under a loaded test run: wait for it
        let until = Instant::now() + Duration::from_secs(10);
        while f.calls.load(Ordering::SeqCst) == 0 && Instant::now() < until {
            std::thread::yield_now();
        }
        e.update(&paths, t0 + Duration::from_secs(2));
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "no second probe piles up");
        // the share wakes up: the late answer wins
        release.send(()).unwrap();
        e.drain(t0 + Duration::from_secs(3));
        assert!(matches!(
            e.state(&paths[0]),
            Some(State::Present { size: 7, .. })
        ));
    }

    #[test]
    fn panic_in_probe_is_unavailable_not_a_crash() {
        let f = Fake::new(Mode::Panic);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        assert_eq!(e.state(&paths[0]), Some(&State::Unavailable));
        assert!(!e.busy());
    }

    #[test]
    fn answer_is_cached_until_it_is_stale() {
        let f = Fake::new(Mode::Found);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        for k in 1..10 {
            e.update(&paths, t0 + Duration::from_secs(k));
        }
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "cache hit");
        let later = t0 + FRESH;
        e.update(&paths, later);
        assert!(e.busy(), "stale: asked again");
        assert!(matches!(e.state(&paths[0]), Some(State::Present { .. })));
        e.drain(later);
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn unavailable_is_retried_later_and_recovers() {
        let f = Fake::new(Mode::Offline);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        assert_eq!(e.state(&paths[0]), Some(&State::Unavailable));
        f.set(Mode::Found);
        e.update(&paths, t0 + RETRY_AFTER - Duration::from_secs(1));
        assert!(!e.busy(), "not yet");
        let t1 = t0 + RETRY_AFTER;
        e.update(&paths, t1);
        e.drain(t1);
        assert!(matches!(e.state(&paths[0]), Some(State::Present { .. })));
    }

    #[test]
    fn retry_asks_again_at_once() {
        let f = Fake::new(Mode::Offline);
        let mut e = engine(&f);
        let (paths, t0) = (one(), Instant::now());
        e.update(&paths, t0);
        e.drain(t0);
        f.set(Mode::Found);
        e.retry(&paths[0], t0);
        assert_eq!(e.state(&paths[0]), None);
        e.update(&paths, t0);
        e.drain(t0);
        assert!(matches!(e.state(&paths[0]), Some(State::Present { .. })));
    }

    #[test]
    fn probing_never_changes_the_recent_lists() {
        let f = Fake::new(Mode::Offline);
        let mut e = engine(&f);
        let list = vec!["Z:/share/a.ddc".to_string(), "Z:/share/b.ddc".to_string()];
        let before = list.clone();
        let paths: Vec<PathBuf> = list.iter().map(PathBuf::from).collect();
        let t0 = Instant::now();
        // one probe per place at a time: the second file is asked once the first answered
        for t in [t0, t0 + Duration::from_secs(600)] {
            for _ in 0..2 {
                e.update(&paths, t);
                e.drain(t);
            }
        }
        assert!(paths
            .iter()
            .all(|p| e.state(p) == Some(&State::Unavailable)));
        assert_eq!(list, before, "unavailable entries stay in recents");
    }

    #[test]
    fn a_hung_share_costs_one_thread_and_marks_its_files_unavailable() {
        let f = Fake::new(Mode::Found);
        let (release, gate) = channel();
        *f.gate.lock().unwrap() = Some(gate);
        let mut e = engine(&f);
        let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| PathBuf::from(format!("Z:/share/{n}.ddc")))
            .collect();
        let t0 = Instant::now();
        e.update(&paths, t0);
        while f.calls.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        e.update(&paths, t0 + Duration::from_secs(1));
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "one probe for the place");
        assert!(paths[1..].iter().all(|p| e.state(p).is_none()), "others wait");
        let t1 = t0 + Duration::from_secs(2);
        e.update(&paths, t1);
        assert!(paths.iter().all(|p| e.state(p) == Some(&State::Unavailable)));
        assert!(!e.busy(), "a parked probe does not keep the screen polling");
        e.retry(&paths[0], t1);
        assert_eq!(e.state(&paths[0]), Some(&State::Unavailable), "still hanging: no retry");
        for k in 1..5 {
            e.update(&paths, t1 + RETRY_AFTER * k);
        }
        assert_eq!(f.calls.load(Ordering::SeqCst), 1, "no thread pile on a dead share");
        release.send(()).unwrap();
        e.drain(t1);
        assert!(matches!(e.state(&paths[0]), Some(State::Present { .. })));
    }

    #[test]
    fn places_split_by_share_and_first_folder() {
        let p = |s: &str| place_of(Path::new(s));
        assert_eq!(p("Z:/share/a.ddc"), p("Z:/share/b/c.ddc"));
        assert_ne!(p("Z:/share/a.ddc"), p("Z:/other/a.ddc"));
        assert_eq!(p("a.ddc"), PathBuf::from("a.ddc"));
    }

    #[test]
    fn real_disk_prober_sees_a_file_and_a_gone_one() {
        let dir = std::env::temp_dir().join(format!("ddc-rl4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.ddc");
        std::fs::write(&file, b"abc").unwrap();
        let p = DiskProber;
        assert_eq!(
            probe_now(&p, &file),
            State::Present {
                modified: std::fs::metadata(&file).unwrap().modified().ok(),
                size: 3
            }
        );
        assert_eq!(probe_now(&p, &dir.join("gone.ddc")), State::Missing);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
