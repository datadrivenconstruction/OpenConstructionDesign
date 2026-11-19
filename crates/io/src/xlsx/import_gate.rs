//! Progress/cancel hook of the import: asked at the start of each sheet and every `GATE_ROWS`
//! rows, always before the model is touched, so a stop leaves the model as it was.
use super::export::Hook;

const GATE_ROWS: usize = 2000;
/// Sheets of the main import that are read row by row.
const SHEETS: f32 = 8.0;

struct Gate {
    hook: Hook,
    done: f32,
}

thread_local! {
    static GATE: std::cell::RefCell<Option<Gate>> = const { std::cell::RefCell::new(None) };
}

/// Removes the hook when dropped, also on an early return or a panic.
pub(crate) struct GateGuard;

impl Drop for GateGuard {
    fn drop(&mut self) {
        GATE.with(|g| *g.borrow_mut() = None);
    }
}

/// Installs `hook` for the calling thread (None: no hook).
pub(crate) fn install(hook: Option<Hook>) -> GateGuard {
    GATE.with(|g| *g.borrow_mut() = hook.map(|hook| Gate { hook, done: 0.0 }));
    GateGuard
}

/// Drops the hook: used once the model is about to change and a stop is no longer safe.
pub(crate) fn off() {
    GATE.with(|g| *g.borrow_mut() = None);
}

/// A new read of the book starts: progress counts from zero again.
pub(crate) fn begin() {
    GATE.with(|g| {
        if let Some(g) = g.borrow_mut().as_mut() {
            g.done = 0.0;
        }
    });
}

fn ask(part: f32, advance: bool) -> Result<(), String> {
    GATE.with(|g| match g.borrow_mut().as_mut() {
        None => Ok(()),
        Some(g) => {
            if advance {
                g.done += 1.0;
            }
            let f = ((g.done - 1.0 + part) / SHEETS).clamp(0.0, 0.99);
            if (g.hook)(f) {
                Ok(())
            } else {
                Err(crate::job::CANCELLED.to_string())
            }
        }
    })
}

/// At the start of each sheet.
pub(crate) fn sheet() -> Result<(), String> {
    ask(0.0, true)
}

/// In a row loop: asks the hook every `GATE_ROWS` rows.
pub(crate) fn rows(i: usize, n: usize) -> Result<(), String> {
    if i > 0 && i % GATE_ROWS == 0 {
        ask(i as f32 / n.max(1) as f32, false)?;
    }
    Ok(())
}
