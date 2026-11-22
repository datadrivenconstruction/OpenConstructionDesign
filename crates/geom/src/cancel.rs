// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cooperative cancellation for long kernel work. The caller runs the work inside
//! [`scope`] with a [`Token`]; kernel loops call [`requested`] and stop early. A function that
//! returns `Result` turns a stop into its own "cancelled" error; one that does not only leaves
//! its loop, and its nearest fallible caller checks [`requested`] right after it, so a partial
//! result never escapes. Outside a scope nothing is ever cancelled.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared stop flag; clones see the same flag.
#[derive(Clone, Debug, Default)]
pub struct Token(Arc<AtomicBool>);

impl Token {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token over an existing flag (job progress, bridge options).
    pub fn from_flag(f: Arc<AtomicBool>) -> Self {
        Self(f)
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Token>> = const { RefCell::new(None) };
    /// Calls to `requested` on this thread, cancelled or not (tests count kernel work with it).
    static CHECKS: Cell<u64> = const { Cell::new(0) };
    /// Test hook: the scope cancels itself on this check number.
    static TRIP_AT: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Runs `f` with `t` as this thread's token; the previous token comes back afterwards.
pub fn scope<R>(t: &Token, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Token>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            CURRENT.with(|c| *c.borrow_mut() = prev);
        }
    }
    let prev = CURRENT.with(|c| c.borrow_mut().replace(t.clone()));
    let _restore = Restore(prev);
    f()
}

/// This thread's token, for kernel helpers that fan work out to their own threads.
pub fn current() -> Option<Token> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Runs `f` under `t` when there is one (a fan-out worker adopting its caller's token).
pub fn adopt<R>(t: Option<&Token>, f: impl FnOnce() -> R) -> R {
    match t {
        Some(t) => scope(t, f),
        None => f(),
    }
}

/// Whether the work running on this thread should stop.
pub fn requested() -> bool {
    let n = CHECKS.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    CURRENT.with(|c| {
        let c = c.borrow();
        let Some(t) = c.as_ref() else { return false };
        if TRIP_AT.with(|a| a.get()) == Some(n) {
            t.cancel();
        }
        t.is_cancelled()
    })
}

/// `Err(Cancelled)` once a stop is requested.
pub fn check() -> Result<(), Cancelled> {
    if requested() {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Checks made on this thread so far.
pub fn checks() -> u64 {
    CHECKS.with(|c| c.get())
}

/// Test hook: the token of the scope on this thread trips at check number `n` (counted by
/// [`checks`]), so a test can stop the work at any point without sleeps. `None` clears it.
pub fn trip_at(n: Option<u64>) {
    TRIP_AT.with(|a| a.set(n));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_scope_is_per_thread_and_nested_scopes_restore() {
        assert!(!requested(), "no scope, never cancelled");
        let t = Token::new();
        scope(&t, || {
            assert!(!requested());
            t.cancel();
            assert!(requested());
            assert_eq!(check(), Err(Cancelled));
            let inner = Token::new();
            scope(&inner, || assert!(!requested()));
            assert!(requested(), "outer token back");
            let other = std::thread::spawn(requested).join().unwrap();
            assert!(!other, "another thread has no scope");
        });
        assert!(!requested());
    }

    #[test]
    fn cancel_trip_hook_stops_at_the_given_check() {
        let t = Token::new();
        let start = checks();
        trip_at(Some(start + 3));
        scope(&t, || {
            assert!(!requested());
            assert!(!requested());
            assert!(requested());
        });
        trip_at(None);
        assert!(t.is_cancelled());
    }
}
