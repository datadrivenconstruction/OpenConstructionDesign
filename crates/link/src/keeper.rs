//! Keeps the project lock alive: a heartbeat every [`HEARTBEAT`] on its own thread (contract §8).
//! A lost lock is reported, never re-taken behind the user's back.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{Client, LinkError};

/// The ERP's lock lives 600 s; the editor renews it every 120 s.
pub const HEARTBEAT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    /// This editor holds the lock until `expires_at` (ERP time).
    Mine { expires_at: String },
    /// The ERP says this editor no longer holds it: Publish waits until it is taken again.
    Lost,
    /// The ERP stopped talking to this editor (426).
    Stopped { supported: Vec<u32> },
    /// The ERP no longer accepts the token.
    Unauthorized,
    /// Released by the user.
    Released,
}

struct Shared {
    state: Mutex<(LockState, Option<String>)>,
    stop: Mutex<bool>,
    wake: Condvar,
}

/// Owns the heartbeat thread; dropping it stops the thread (the lock is not released: call
/// [`Keeper::release`] for that).
pub struct Keeper {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    client: Client,
    project_id: String,
}

impl Keeper {
    /// Starts after a successful `take_lock`; `expires_at` is what it returned.
    pub fn start(client: Client, project_id: &str, expires_at: String, every: Duration) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new((LockState::Mine { expires_at }, None)),
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        let (s, c, p) = (shared.clone(), client.clone(), project_id.to_string());
        let thread = std::thread::Builder::new()
            .name("ddc-link-heartbeat".into())
            .spawn(move || run(&s, &c, &p, every))
            .ok();
        Keeper { shared, thread, client, project_id: project_id.to_string() }
    }

    /// The lock as last seen, and the last transient error (network), if any.
    pub fn state(&self) -> (LockState, Option<String>) {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_mine(&self) -> bool {
        matches!(self.state().0, LockState::Mine { .. })
    }

    /// The ERP said the lock is gone (an upload got 409): the heartbeat stops too.
    pub fn mark_lost(&self) {
        self.set(LockState::Lost, None);
        self.halt();
    }

    fn set(&self, st: LockState, err: Option<String>) {
        *self.shared.state.lock().unwrap_or_else(|e| e.into_inner()) = (st, err);
    }

    fn halt(&self) {
        *self.shared.stop.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.shared.wake.notify_all();
    }

    /// Stops the heartbeat and gives the lock back to the ERP.
    pub fn release(mut self) -> Result<(), LinkError> {
        self.halt();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let r = self.client.release_lock(&self.project_id);
        self.set(LockState::Released, None);
        r
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.halt();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(s: &Shared, client: &Client, project_id: &str, every: Duration) {
    loop {
        {
            let stop = s.stop.lock().unwrap_or_else(|e| e.into_inner());
            let (stop, _) = s.wake.wait_timeout_while(stop, every, |st| !*st).unwrap_or_else(|e| e.into_inner());
            if *stop {
                return;
            }
        }
        let next = match client.heartbeat(project_id) {
            Ok(l) if l.mine => (LockState::Mine { expires_at: l.expires_at }, None),
            Ok(_) | Err(LinkError::LockLost) | Err(LinkError::LockHeld { .. }) => (LockState::Lost, None),
            Err(LinkError::Unsupported { supported }) => (LockState::Stopped { supported }, None),
            Err(LinkError::Unauthorized) => (LockState::Unauthorized, None),
            // Transient: the ERP keeps the lock 600 s, the next beat may get through.
            Err(e) => {
                let mut g = s.state.lock().unwrap_or_else(|e| e.into_inner());
                g.1 = Some(e.to_string());
                continue;
            }
        };
        let done = !matches!(next.0, LockState::Mine { .. });
        *s.state.lock().unwrap_or_else(|e| e.into_inner()) = next;
        if done {
            return;
        }
    }
}
