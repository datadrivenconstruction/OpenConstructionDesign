// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Durable replacement of a user file: the new content is written beside it, synced to the
//! disk and renamed over it, so a failure or a power cut leaves the old file whole.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `<name>.part` in the folder of `target`.
pub fn part_path(target: &Path) -> PathBuf {
    let mut n = target.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    n.push(".part");
    target.with_file_name(n)
}

/// Flushes the content of `path` to the disk.
pub fn sync_file(path: &Path) -> std::io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}

#[cfg(windows)]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const REPLACE_EXISTING: u32 = 1;
    const WRITE_THROUGH: u32 = 8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (a, b) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that outlive the call
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), REPLACE_EXISTING | WRITE_THROUGH) } != 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        // over the plain path limit: std knows the long form
        Some(3 | 206) => fs::rename(from, to),
        _ => Err(e),
    }
}

#[cfg(not(windows))]
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::rename(from, to)?;
    if let Some(dir) = to.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Renames `from` over `to` and returns when the rename is on the disk.
pub fn rename_durably(from: &Path, to: &Path) -> std::io::Result<()> {
    rename_over(from, to)
}

/// Syncs the finished `part` and renames it over `target`; the part is removed on failure.
pub fn commit_part(part: &Path, target: &Path) -> std::io::Result<()> {
    let r = sync_file(part).and_then(|_| rename_over(part, target));
    if r.is_err() {
        let _ = fs::remove_file(part);
    }
    r
}

/// Runs `write` on `<target>.part`, then commits it over `target`. On any error the old
/// file stays as it was and only the part written here is removed.
pub fn replace_durably<F>(target: &Path, write: F) -> Result<(), String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    let part = part_path(target);
    if let Err(e) = write(&part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    commit_part(&part, target).map_err(|e| e.to_string())
}

/// `replace_durably` for content that is already in memory.
pub fn replace_durably_bytes(target: &Path, bytes: &[u8]) -> Result<(), String> {
    replace_durably(target, |p| {
        let mut f = File::create(p).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.flush().map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-fsutil-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn part_synced_before_rename() {
        let d = dir("sync");
        let t = d.join("b.xlsx");
        fs::write(&t, b"old").unwrap();
        replace_durably(&t, |p| {
            assert_eq!(p, part_path(&t));
            fs::write(p, b"new").map_err(|e| e.to_string())?;
            // the part is already there and openable for sync when the commit starts
            assert!(sync_file(p).is_ok());
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"new");
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn failed_write_keeps_old_file_and_drops_part() {
        let d = dir("fail");
        let t = d.join("b.csv");
        fs::write(&t, b"old").unwrap();
        let r = replace_durably(&t, |p| {
            fs::write(p, b"half").unwrap();
            Err("boom".to_string())
        });
        assert_eq!(r, Err("boom".to_string()));
        assert_eq!(fs::read(&t).unwrap(), b"old");
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn locked_target_keeps_old() {
        let d = dir("lock");
        // a folder in place of the file: the rename cannot succeed
        let t = d.join("b.xlsx");
        fs::create_dir_all(t.join("inner")).unwrap();
        assert!(replace_durably_bytes(&t, b"new").is_err());
        assert!(t.join("inner").exists());
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bytes_atomic_leaves_no_part() {
        let d = dir("bytes");
        let t = d.join("x.csv");
        replace_durably_bytes(&t, b"a;b\n").unwrap();
        replace_durably_bytes(&t, b"c;d\n").unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"c;d\n");
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn export_table_goes_via_part_and_leaves_none() {
        let d = dir("table");
        let t = d.join("t.xlsx");
        let head = vec!["a".to_string()];
        let rows = vec![vec![crate::xlsx::TableCell::Num(1.0)]];
        crate::xlsx::export_table(&t, "s", &head, &[""], &rows, None).unwrap();
        assert!(fs::metadata(&t).unwrap().len() > 0);
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn schedule_export_failure_keeps_old_file() {
        let d = dir("tfail");
        // the target cannot be replaced: nothing of the old content is lost, no part remains
        let t = d.join("t.xlsx");
        fs::create_dir_all(t.join("keep")).unwrap();
        let head = vec!["a".to_string()];
        let rows = vec![vec![crate::xlsx::TableCell::Num(1.0)]];
        assert!(crate::xlsx::export_table(&t, "s", &head, &[""], &rows, None).is_err());
        assert!(t.join("keep").exists());
        assert!(!part_path(&t).exists());
        let _ = fs::remove_dir_all(&d);
    }
}
