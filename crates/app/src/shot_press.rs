// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Screenshot mode helpers: native OS dialogs are never opened (a picker returns `None` and the
//! in-app window is shown instead), and `--press <command-id>` runs a command before capture
//! within a frame budget, so an offscreen capture can never hang.
use std::sync::atomic::{AtomicBool, Ordering};

static SCREENSHOT: AtomicBool = AtomicBool::new(false);

/// Frames rendered before capture when `--press` is not given.
pub const DEFAULT_FRAMES: usize = 8;
/// Hard cap on `--press-frames`; asking for more is an error, not a long run.
pub const MAX_FRAMES: usize = 60;

/// Called once by the screenshot entry point.
pub fn set_screenshot_mode(on: bool) {
    SCREENSHOT.store(on, Ordering::SeqCst);
}

/// True while rendering offscreen: no native dialog may be opened.
pub fn native_blocked() -> bool {
    SCREENSHOT.load(Ordering::SeqCst)
}

/// Run a native picker unless `blocked`; a blocked picker returns `None` and `f` is not called.
pub fn guarded<T>(blocked: bool, f: impl FnOnce() -> Option<T>) -> Option<T> {
    if blocked {
        None
    } else {
        f()
    }
}

/// [`guarded`] with the global screenshot flag; wrap every `rfd` call in it.
pub fn guard<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    guarded(native_blocked(), f)
}

/// Parsed `--press <command-id> [--press-frames N]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    pub id: String,
    pub frames: usize,
}

/// `Ok(None)` when `--press` is absent; `Err` for a missing id or a bad/over-budget frame count.
pub fn parse(argv: &[String]) -> Result<Option<Press>, String> {
    let Some(i) = argv.iter().position(|s| s == "--press") else {
        return Ok(None);
    };
    let id = match argv.get(i + 1) {
        Some(s) if !s.is_empty() && !s.starts_with("--") => s.clone(),
        _ => return Err("--press needs a command id, e.g. --press file.import".into()),
    };
    let frames = match argv.iter().position(|s| s == "--press-frames") {
        None => DEFAULT_FRAMES,
        Some(j) => {
            let n: usize = argv
                .get(j + 1)
                .and_then(|s| s.parse().ok())
                .ok_or("--press-frames needs a whole number")?;
            if n == 0 || n > MAX_FRAMES {
                return Err(format!("--press-frames must be 1..={MAX_FRAMES}, got {n}"));
            }
            n
        }
    };
    Ok(Some(Press { id, frames }))
}

/// Run the pressed command through `exec` (true = command exists); unknown id is an error.
pub fn run(p: &Press, exec: &mut dyn FnMut(&str) -> bool) -> Result<(), String> {
    if exec(&p.id) {
        Ok(())
    } else {
        Err(format!("--press: unknown command id '{}'", p.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn blocked_picker_never_calls_os() {
        let mut called = false;
        let r: Option<u8> = guarded(true, || {
            called = true;
            Some(1)
        });
        assert!(r.is_none());
        assert!(!called);
        assert_eq!(guarded(false, || Some(7)), Some(7));
    }

    #[test]
    fn press_absent_is_none() {
        assert_eq!(parse(&v(&["x", "--screenshot", "a.png"])), Ok(None));
    }

    #[test]
    fn press_parses_id_and_default_budget() {
        let p = parse(&v(&["--press", "file.import"])).unwrap().unwrap();
        assert_eq!(p.id, "file.import");
        assert_eq!(p.frames, DEFAULT_FRAMES);
    }

    #[test]
    fn press_missing_id_is_error() {
        assert!(parse(&v(&["--press"])).is_err());
        assert!(parse(&v(&["--press", "--theme", "light"])).is_err());
    }

    #[test]
    fn press_frame_budget_enforced() {
        let ok = parse(&v(&["--press", "a.b", "--press-frames", "20"])).unwrap().unwrap();
        assert_eq!(ok.frames, 20);
        assert!(parse(&v(&["--press", "a.b", "--press-frames", "0"])).is_err());
        let over = (MAX_FRAMES + 1).to_string();
        assert!(parse(&v(&["--press", "a.b", "--press-frames", &over])).is_err());
        assert!(parse(&v(&["--press", "a.b", "--press-frames", "x"])).is_err());
    }

    #[test]
    fn press_runs_known_and_rejects_unknown() {
        let p = Press { id: "file.import".into(), frames: 8 };
        let mut seen = Vec::new();
        run(&p, &mut |id| {
            seen.push(id.to_string());
            true
        })
        .unwrap();
        assert_eq!(seen, ["file.import"]);
        let bad = Press { id: "no.such".into(), frames: 8 };
        let e = run(&bad, &mut |_| false).unwrap_err();
        assert!(e.contains("no.such"));
    }

    #[test]
    fn help_documents_press() {
        assert!(crate::headless_file::HELP.contains("--press"));
        assert!(crate::headless_file::HELP.contains("--press-frames"));
    }
}
