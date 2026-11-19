// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The picking index of a large scene is built off-frame, on a single thread. Until it is ready, the old index
//! does not match the scene (`Index::matches`): hover stays silent, click and box select fall back to exact brute force.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use ddc_render::{Aabb, Instanced, Scene, SceneMesh};

use crate::shell::EditorApp;
use crate::spatial::Index;

struct Request {
    id: u64,
    prev: Option<Arc<Index>>,
    meshes: Vec<Arc<SceneMesh>>,
    boxes: Vec<Aabb>,
    inst: Vec<Instanced>,
}

/// The latest scene waits here: a new one replaces the waiting one, so neither memory nor work piles up.
#[derive(Default)]
struct Slot {
    req: Option<Request>,
    closed: bool,
}

struct Worker {
    slot: Arc<(Mutex<Slot>, Condvar)>,
    done: mpsc::Receiver<(u64, Index)>,
    /// Joined on drop: a thread still running at process exit is torn down mid-flight.
    thread: Option<std::thread::JoinHandle<Option<()>>>,
    latest: Arc<AtomicU64>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        {
            let (m, cv) = &*self.slot;
            m.lock().unwrap_or_else(PoisonError::into_inner).closed = true;
            cv.notify_all();
        }
        // the build in flight goes stale, so the thread skips what it has not started yet
        self.latest.fetch_add(1, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn work(slot: Arc<(Mutex<Slot>, Condvar)>, latest: Arc<AtomicU64>, tx: mpsc::Sender<(u64, Index)>) {
    loop {
        let req = {
            let (m, cv) = &*slot;
            let mut g = m.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if g.closed {
                    return;
                }
                if let Some(r) = g.req.take() {
                    break r;
                }
                g = cv.wait(g).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Request {
            id,
            prev,
            meshes,
            boxes,
            inst,
        } = req;
        let stale = || id != latest.load(Ordering::Relaxed);
        if stale() {
            continue;
        }
        let s = Scene::from_parts(meshes, boxes).with_instanced(inst);
        if stale() {
            continue;
        }
        let ix = Index::update(prev.as_deref(), &s);
        if !stale() && tx.send((id, ix)).is_err() {
            return;
        }
    }
}

#[derive(Default)]
pub struct IndexJob {
    worker: Option<Worker>,
    /// Id of the latest request; a cancelled or superseded thread sees its id is stale.
    latest: Arc<AtomicU64>,
    waiting: Option<u64>,
    #[cfg(test)]
    spawned: usize,
}

impl IndexJob {
    fn ensure_worker(&mut self) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let slot: Arc<(Mutex<Slot>, Condvar)> = Arc::default();
        let (tx, done) = mpsc::channel();
        let (s, latest) = (slot.clone(), self.latest.clone());
        // a crash drops the sender: poll drops the worker and the next request starts a new one
        let job = crate::jobs::spawn_guarded("ddc-pick-index", move || work(s, latest, tx), |_| {});
        let Ok(thread) = job else {
            return false;
        };
        #[cfg(test)]
        {
            self.spawned += 1;
        }
        self.worker = Some(Worker {
            slot,
            done,
            thread: Some(thread),
            latest: self.latest.clone(),
        });
        true
    }

    /// false — the thread failed to start.
    fn start(&mut self, prev: Option<Arc<Index>>, s: &Scene) -> bool {
        if !self.ensure_worker() {
            self.cancel();
            return false;
        }
        let id = self.latest.fetch_add(1, Ordering::Relaxed) + 1;
        let req = Request {
            id,
            prev,
            meshes: s.meshes.clone(),
            boxes: s.boxes().to_vec(),
            inst: s.instanced.clone(),
        };
        if let Some(w) = &self.worker {
            let (m, cv) = &*w.slot;
            m.lock().unwrap_or_else(PoisonError::into_inner).req = Some(req);
            cv.notify_all();
        }
        self.waiting = Some(id);
        true
    }

    /// The result is no longer needed (scene already indexed or being indexed in place).
    fn cancel(&mut self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
        self.waiting = None;
    }

    fn take(&mut self) -> Option<Index> {
        let want = self.waiting?;
        loop {
            let got = self.worker.as_ref()?.done.try_recv();
            match got {
                Ok((id, ix)) if id == want => {
                    self.waiting = None;
                    return Some(ix);
                }
                // result of a superseded request
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => return None,
                // thread panicked: the next request starts a new one
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.waiting = None;
                    self.worker = None;
                    return None;
                }
            }
        }
    }

    pub fn running(&self) -> bool {
        self.waiting.is_some()
    }
}

/// New final scene: small ones are indexed immediately, large ones in the background.
pub fn refresh(app: &mut EditorApp) {
    let prev = app.view.index.clone();
    if prev.as_ref().is_some_and(|ix| ix.matches(&app.scene)) {
        app.view.nav.perf.index.cancel();
        return;
    }
    if crate::spatial::is_big(&app.scene) && app.view.nav.perf.index.start(prev.clone(), &app.scene)
    {
        return;
    }
    app.view.nav.perf.index.cancel();
    let ix = Index::update(prev.as_deref(), &app.scene);
    app.view.index = Some(Arc::new(ix));
}

/// Take the finished index if the scene has not changed since.
pub fn poll(app: &mut EditorApp) {
    if let Some(ix) = app.view.nav.perf.index.take() {
        if ix.matches(&app.scene) {
            app.view.index = Some(Arc::new(ix));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_render::SceneMesh;
    use web_time::{Duration, Instant};

    fn scene() -> Scene {
        Scene::new(vec![SceneMesh {
            element: 1,
            tag: 0,
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            triangles: vec![[0, 1, 2]],
        }])
    }

    fn wait(j: &mut IndexJob) -> Index {
        let t = Instant::now();
        loop {
            if let Some(ix) = j.take() {
                return ix;
            }
            assert!(j.running(), "the worker is gone");
            assert!(t.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_background_index_matches_the_scene_it_was_started_for() {
        let s = scene();
        let mut j = IndexJob::default();
        assert!(j.start(None, &s));
        let ix = wait(&mut j);
        assert!(ix.matches(&s));
        assert!(!j.running());
        assert!(!ix.matches(&scene()), "another scene has other meshes");
    }

    #[test]
    fn a_newer_job_replaces_the_running_one() {
        let (a, b) = (scene(), scene());
        let mut j = IndexJob::default();
        assert!(j.start(None, &a));
        assert!(j.start(None, &b));
        assert!(wait(&mut j).matches(&b));
    }

    #[test]
    fn many_new_scenes_share_one_worker_and_only_the_last_one_counts() {
        let scenes: Vec<Scene> = (0..50).map(|_| scene()).collect();
        let mut j = IndexJob::default();
        for s in &scenes {
            assert!(j.start(None, s));
        }
        assert_eq!(j.spawned, 1);
        assert!(wait(&mut j).matches(&scenes[49]));
        assert!(j.take().is_none());
    }

    #[test]
    fn a_cancelled_job_gives_nothing_and_the_worker_serves_the_next_one() {
        let (a, b) = (scene(), scene());
        let mut j = IndexJob::default();
        assert!(j.start(None, &a));
        j.cancel();
        assert!(!j.running());
        assert!(j.take().is_none());
        assert!(j.start(None, &b));
        assert!(wait(&mut j).matches(&b));
        assert_eq!(j.spawned, 1);
    }
}
