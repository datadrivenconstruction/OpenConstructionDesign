// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The only place that talks to the native file dialogs. Builder methods mirror the dialog crate;
//! the terminal calls return `None` (or `false`) without touching the OS while the screenshot
//! flag is set, so an offscreen run can never block on a picker. A build without the `gui`
//! feature has no window to parent a dialog: every call is blocked there.
use std::path::{Path, PathBuf};

#[cfg(feature = "gui")]
use rfd::FileDialog;

/// Headless build: the builder keeps nothing, and the pickers are never reached (always blocked).
#[cfg(not(feature = "gui"))]
struct FileDialog;

#[cfg(not(feature = "gui"))]
impl FileDialog {
    fn new() -> Self {
        Self
    }
    fn add_filter(self, _: impl Into<String>, _: &[impl ToString]) -> Self {
        self
    }
    fn set_title(self, _: impl Into<String>) -> Self {
        self
    }
    fn set_directory<P: AsRef<Path>>(self, _: P) -> Self {
        self
    }
    fn set_file_name(self, _: impl Into<String>) -> Self {
        self
    }
    fn pick_file(self) -> Option<PathBuf> {
        None
    }
    fn pick_files(self) -> Option<Vec<PathBuf>> {
        None
    }
    fn save_file(self) -> Option<PathBuf> {
        None
    }
    fn pick_folder(self) -> Option<PathBuf> {
        None
    }
}

thread_local! {
    /// Headless script of this thread: `Some` means no native dialog is ever opened here, and
    /// each file pick takes the next preset path (`None` once the queue is empty).
    static SCRIPT: std::cell::RefCell<Option<std::collections::VecDeque<PathBuf>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: from now on this thread's pickers answer from `paths` (in order) instead of a
/// native dialog. An empty list makes every picker cancel without touching the OS.
pub fn script_picks(paths: Vec<PathBuf>) {
    SCRIPT.with(|s| *s.borrow_mut() = Some(paths.into()));
}

/// Ends the scripted mode of this thread.
pub fn clear_script() {
    SCRIPT.with(|s| *s.borrow_mut() = None);
}

/// `Some(answer)` while scripted; `None` means "ask the OS".
fn scripted() -> Option<Option<PathBuf>> {
    SCRIPT.with(|s| s.borrow_mut().as_mut().map(|q| q.pop_front()))
}

/// Native file dialog builder with a screenshot-mode gate.
pub struct Dlg {
    inner: FileDialog,
    blocked: bool,
}

impl Dlg {
    pub fn new() -> Self {
        Self::with_blocked(crate::shot_press::native_blocked())
    }

    /// Explicit gate, for tests that must not flip the global screenshot flag.
    pub fn with_blocked(blocked: bool) -> Self {
        Self { inner: FileDialog::new(), blocked: blocked || cfg!(not(feature = "gui")) }
    }

    pub fn add_filter(mut self, name: impl Into<String>, exts: &[impl ToString]) -> Self {
        self.inner = self.inner.add_filter(name, exts);
        self
    }

    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.inner = self.inner.set_title(title);
        self
    }

    pub fn set_directory<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.inner = self.inner.set_directory(dir);
        self
    }

    pub fn set_file_name(mut self, name: impl Into<String>) -> Self {
        self.inner = self.inner.set_file_name(name);
        self
    }

    pub fn pick_file(self) -> Option<PathBuf> {
        if let Some(p) = scripted() {
            return p;
        }
        let Self { inner, blocked } = self;
        crate::shot_press::guarded(blocked, || inner.pick_file())
    }

    pub fn pick_files(self) -> Option<Vec<PathBuf>> {
        if scripted().is_some() {
            return None;
        }
        let Self { inner, blocked } = self;
        crate::shot_press::guarded(blocked, || inner.pick_files())
    }

    pub fn save_file(self) -> Option<PathBuf> {
        if scripted().is_some() {
            return None;
        }
        let Self { inner, blocked } = self;
        crate::shot_press::guarded(blocked, || inner.save_file())
    }

    pub fn pick_folder(self) -> Option<PathBuf> {
        if scripted().is_some() {
            return None;
        }
        let Self { inner, blocked } = self;
        crate::shot_press::guarded(blocked, || inner.pick_folder())
    }
}

impl Default for Dlg {
    fn default() -> Self {
        Self::new()
    }
}

/// Yes/No question; `false` without a dialog in screenshot mode.
pub fn confirm_yes_no(title: impl Into<String>, text: impl Into<String>) -> bool {
    confirm_gated(crate::shot_press::native_blocked(), title, text)
}

fn confirm_gated(blocked: bool, title: impl Into<String>, text: impl Into<String>) -> bool {
    if blocked {
        return false;
    }
    #[cfg(feature = "gui")]
    {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(text)
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
    }
    #[cfg(not(feature = "gui"))]
    {
        let _ = (title, text);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_helpers_return_none_without_os() {
        assert!(Dlg::with_blocked(true).add_filter("X", &["x"]).set_title("t").pick_file().is_none());
        assert!(Dlg::with_blocked(true).pick_files().is_none());
        assert!(Dlg::with_blocked(true).set_file_name("a.x").set_directory(".").save_file().is_none());
        assert!(Dlg::with_blocked(true).pick_folder().is_none());
        assert!(!confirm_gated(true, "t", "d"));
    }

    fn scan(dir: &Path, needle: &str, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, needle, hits);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("file_pick.rs") {
                if std::fs::read_to_string(&p).is_ok_and(|s| s.contains(needle)) {
                    hits.push(p.display().to_string());
                }
            }
        }
    }

    #[test]
    fn no_direct_dialog_calls_outside_module() {
        let needle = concat!("rf", "d::");
        let mut hits = Vec::new();
        scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), needle, &mut hits);
        assert!(hits.is_empty(), "direct dialog calls: {hits:?}");
    }
}
