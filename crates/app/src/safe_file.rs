// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Crash-safe small settings files: atomic write with a `.bak` of the previous good
//! file, and a load that falls back to the backup and keeps unreadable files aside.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// `name` + `.suffix` next to `p` (the extension is kept: `a.txt` -> `a.txt.bak`).
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".");
    s.push(suffix);
    PathBuf::from(s)
}

/// Temp file in the same dir, `sync_all`, then rename over `path`. The previous
/// non-empty file is first copied to `.bak`, so one good generation always survives.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = with_suffix(path, "tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    if std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
        let _ = std::fs::copy(path, with_suffix(path, "bak"));
    }
    if let Err(e) = ddc_io::fsutil::rename_durably(&tmp, path) {
        // some Windows setups refuse to replace a busy target: the .bak is already
        // in place, so remove + rename is safe
        if path.exists() && std::fs::remove_file(path).is_ok() {
            let r = std::fs::rename(&tmp, path);
            if r.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            return r;
        }
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    /// Main file was unusable; the backup was loaded.
    Restored { file: String, broken: String },
    /// Main file and backup were both unusable; defaults are used.
    Reset { file: String, broken: String },
}

impl Notice {
    pub fn text(&self) -> String {
        match self {
            Notice::Restored { file, broken } => {
                crate::tr("sf.restored", &[("file", file), ("broken", broken)])
            }
            Notice::Reset { file, broken } => {
                crate::tr("sf.reset", &[("file", file), ("broken", broken)])
            }
        }
    }
}

static PENDING: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

/// Notices raised by loads since the last call; the shell shows them once.
pub fn take_notices() -> Vec<Notice> {
    PENDING
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

fn move_aside(path: &Path) -> Option<PathBuf> {
    let mut to = with_suffix(path, "broken");
    let mut n = 1;
    while to.exists() {
        to = with_suffix(path, &format!("broken{n}"));
        n += 1;
    }
    std::fs::rename(path, &to).ok().map(|_| to)
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_ok(p: &Path, ok: &dyn Fn(&str) -> bool) -> Option<String> {
    let t = std::fs::read_to_string(p).ok()?;
    (!t.trim().is_empty() && ok(&t)).then_some(t)
}

/// Text of `path` if it passes `ok`; else of `path.bak`. An unusable main file is moved
/// to `*.broken` (never deleted). A missing main with no backup is a normal first run.
pub fn read_checked(path: &Path, ok: &dyn Fn(&str) -> bool) -> (Option<String>, Option<Notice>) {
    if let Some(t) = read_ok(path, ok) {
        return (Some(t), None);
    }
    let existed = path.exists();
    let broken = if existed { move_aside(path) } else { None };
    let bak_text = read_ok(&with_suffix(path, "bak"), ok);
    let broken = broken.as_deref().map(name_of).unwrap_or_default();
    let file = name_of(path);
    match (bak_text, existed) {
        (Some(t), _) => (Some(t), Some(Notice::Restored { file, broken })),
        (None, true) => (None, Some(Notice::Reset { file, broken })),
        (None, false) => (None, None),
    }
}

/// `read_checked` that also queues the notice for the shell.
pub fn load_text(path: &Path, ok: &dyn Fn(&str) -> bool) -> Option<String> {
    let (t, n) = read_checked(path, ok);
    if let (Some(n), Ok(mut q)) = (n, PENDING.lock()) {
        q.push(n);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-sf-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn ok(t: &str) -> bool {
        t.ends_with('\n') && t.contains('=')
    }

    #[test]
    fn normal_save_keeps_previous_as_bak_and_no_tmp() {
        let d = dir("save");
        let p = d.join("p.txt");
        write_atomic(&p, b"a=1\n").unwrap();
        write_atomic(&p, b"a=2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a=2\n");
        assert_eq!(std::fs::read_to_string(with_suffix(&p, "bak")).unwrap(), "a=1\n");
        assert!(!with_suffix(&p, "tmp").exists());
    }

    #[test]
    fn leftover_tmp_leaves_old_file_intact() {
        let d = dir("interrupted");
        let p = d.join("p.txt");
        write_atomic(&p, b"a=1\n").unwrap();
        std::fs::write(with_suffix(&p, "tmp"), "a=").unwrap(); // crash before rename
        let (t, n) = read_checked(&p, &ok);
        assert_eq!(t.as_deref(), Some("a=1\n"));
        assert!(n.is_none());
        write_atomic(&p, b"a=3\n").unwrap(); // the next save overwrites the stale tmp
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a=3\n");
    }

    #[test]
    fn corrupt_main_good_bak_uses_bak_and_keeps_broken() {
        let d = dir("bak");
        let p = d.join("p.txt");
        write_atomic(&p, b"a=1\n").unwrap();
        write_atomic(&p, b"a=2\n").unwrap();
        std::fs::write(&p, "a=").unwrap(); // truncated
        let (t, n) = read_checked(&p, &ok);
        assert_eq!(t.as_deref(), Some("a=1\n"));
        let Some(Notice::Restored { broken, .. }) = n else {
            panic!("no notice")
        };
        assert_eq!(std::fs::read_to_string(d.join(broken)).unwrap(), "a=");
        assert!(!p.exists());
    }

    #[test]
    fn empty_main_and_then_missing_main_with_bak() {
        let d = dir("empty");
        let p = d.join("p.txt");
        write_atomic(&p, b"a=1\n").unwrap();
        write_atomic(&p, b"a=2\n").unwrap();
        std::fs::write(&p, "").unwrap();
        assert_eq!(read_checked(&p, &ok).0.as_deref(), Some("a=1\n"));
        // main is now moved aside, bak remains: restored again, without a new broken file
        let (t, n) = read_checked(&p, &ok);
        assert_eq!(t.as_deref(), Some("a=1\n"));
        assert!(matches!(n, Some(Notice::Restored { .. })));
    }

    #[test]
    fn both_bad_gives_defaults_notice_and_keeps_file() {
        let d = dir("both");
        let p = d.join("p.txt");
        std::fs::write(&p, "junk").unwrap();
        std::fs::write(with_suffix(&p, "bak"), "").unwrap();
        let (t, n) = read_checked(&p, &ok);
        assert!(t.is_none());
        let Some(Notice::Reset { broken, .. }) = n else {
            panic!("no notice")
        };
        assert_eq!(std::fs::read_to_string(d.join(broken)).unwrap(), "junk");
    }

    #[test]
    fn first_run_is_silent() {
        let d = dir("first");
        let (t, n) = read_checked(&d.join("p.txt"), &ok);
        assert!(t.is_none() && n.is_none());
    }

    #[test]
    fn second_broken_file_gets_its_own_name() {
        let d = dir("two");
        let p = d.join("p.txt");
        std::fs::write(&p, "x").unwrap();
        read_checked(&p, &ok);
        std::fs::write(&p, "y").unwrap();
        read_checked(&p, &ok);
        assert_eq!(std::fs::read_to_string(with_suffix(&p, "broken")).unwrap(), "x");
        assert_eq!(std::fs::read_to_string(with_suffix(&p, "broken1")).unwrap(), "y");
    }
}
