// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Worker threads for cooking projects and loading units. The frame thread replaces the waiting
//! list every update (highest rank first); jobs already running are not queued twice. Results come
//! back over a channel and are taken without waiting.

use super::cook::{cook, Cooked};
use super::project::{place, Placed, Project};
use glam::Vec3;
use std::collections::HashSet;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Job {
    Cook(u32),
    Load(u32, u32),
}

pub enum Done {
    Cooked(u32, Box<Cooked>),
    Loaded(u32, u32, Placed),
}

/// What a worker needs to know about a project.
#[derive(Clone)]
pub struct Spec {
    pub project: Arc<dyn Project>,
    pub off: Vec3,
    pub base: u32,
    pub salt: u64,
}

/// Runs one job on the calling thread.
pub fn run(job: Job, specs: &[Spec]) -> Done {
    match job {
        Job::Cook(p) => {
            let s = &specs[p as usize];
            Done::Cooked(
                p,
                Box::new(cook(s.project.as_ref(), s.off, s.base, s.salt)),
            )
        }
        Job::Load(p, u) => {
            let s = &specs[p as usize];
            let c = s.project.load(u as usize);
            Done::Loaded(p, u, place(c, s.off, s.base, s.salt ^ u as u64))
        }
    }
}

#[derive(Default)]
struct Queue {
    waiting: Vec<(f32, Job)>,
    running: HashSet<Job>,
    stop: bool,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

pub struct Pool {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<Done>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    pub fn new(workers: usize, specs: Arc<Vec<Spec>>) -> Self {
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue::default()),
            cv: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel();
        let threads = (0..workers.max(1))
            .map(|i| {
                let shared = shared.clone();
                let specs = specs.clone();
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("ddc-city-{i}"))
                    .spawn(move || worker(&shared, &specs, &tx))
                    .expect("spawn city worker")
            })
            .collect();
        Self {
            shared,
            rx,
            threads,
        }
    }

    /// Replaces the waiting jobs; a job already running is skipped.
    pub fn set(&self, jobs: Vec<(f32, Job)>) {
        let mut q = self.shared.q.lock().unwrap_or_else(|e| e.into_inner());
        q.waiting = jobs
            .into_iter()
            .filter(|(_, j)| !q.running.contains(j))
            .collect();
        drop(q);
        self.shared.cv.notify_all();
    }

    /// Finished jobs since the last call.
    pub fn poll(&self) -> Vec<Done> {
        self.rx.try_iter().collect()
    }

    /// Jobs waiting or running.
    pub fn busy(&self) -> usize {
        let q = self.shared.q.lock().unwrap_or_else(|e| e.into_inner());
        q.waiting.len() + q.running.len()
    }
}

fn worker(shared: &Shared, specs: &[Spec], tx: &mpsc::Sender<Done>) {
    loop {
        let job = {
            let mut q = shared.q.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.stop {
                    return;
                }
                if !q.waiting.is_empty() {
                    break;
                }
                q = shared.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            let best = q
                .waiting
                .iter()
                .enumerate()
                .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let (_, job) = q.waiting.swap_remove(best);
            q.running.insert(job);
            job
        };
        let done = run(job, specs);
        // the result is sent before the job leaves `running`: an update between the two never
        // sees the job as neither running nor finished
        let gone = tx.send(done).is_err();
        shared
            .q
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .remove(&job);
        if gone {
            return;
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared
            .q
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stop = true;
        self.shared.cv.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
