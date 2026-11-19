// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Import cache: finished builds of a source file with no user edits, kept as ordinary project
//! files so a second open of the same bytes is a copy, not a read. Our own derived data: safe to
//! delete at any time, trimmed least recently used first.
//!
//! Key = SHA-256 of the source + reader version + `schema::VERSION`; a new reader or schema
//! misses by construction. A small index maps (path, size, mtime) to a hash so the common
//! second open does not even read the source.
use ddc_model::source::file::Stat;
use std::path::{Path, PathBuf};
use web_time::SystemTime;

/// Cache size cap, bytes.
pub const CAP: u64 = 5 << 30;

/// Version of what the IFC reader and the commit produce; bump when an import changes.
pub const READER_IFC: &str = "ifc1";
/// Same for the `.rvt` mapper (the decoding module's folder name is part of [`rvt_reader`]).
pub const READER_RVT: &str = "rvt1";

/// Reader tag of an `.rvt` build: mapper version + the module folder name.
pub fn rvt_reader(module_dir: &Path) -> String {
    let m = module_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{READER_RVT}-{m}")
}

pub fn dir() -> Option<PathBuf> {
    // headless acceptance runs must read the source, never a cached copy
    if std::env::var_os("DDC_NO_IMPORT_CACHE").is_some() {
        return None;
    }
    Some(crate::project::app_dir()?.join("import_cache"))
}

pub fn key(hash: &str, reader: &str, schema: i64) -> String {
    let reader: String = reader
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{hash}-{reader}-s{schema}")
}

pub fn entry_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{}", ddc_model::PROJECT_EXT))
}

const INDEX: &str = "index.tsv";

/// Hash remembered for the file at `path` with this size and mtime.
pub fn hash_hint(dir: &Path, path: &Path, stat: Stat) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(INDEX)).ok()?;
    let me = path.to_string_lossy();
    text.lines().rev().find_map(|l| {
        let mut c = l.split('\t');
        let (p, size, mtime, hash) = (c.next()?, c.next()?, c.next()?, c.next()?);
        (p == me && size.parse() == Ok(stat.size) && mtime.parse() == Ok(stat.mtime))
            .then(|| hash.to_string())
    })
}

/// Records the hash of `path`; the index keeps the newest rows only.
pub fn remember(dir: &Path, path: &Path, stat: Stat, hash: &str) {
    const KEEP: usize = 2000;
    let _ = std::fs::create_dir_all(dir);
    let idx = dir.join(INDEX);
    let me = path.to_string_lossy();
    let old = std::fs::read_to_string(&idx).unwrap_or_default();
    let mut rows: Vec<&str> = old
        .lines()
        .filter(|l| l.split('\t').next() != Some(me.as_ref()))
        .collect();
    let new = format!("{me}\t{}\t{}\t{hash}", stat.size, stat.mtime);
    rows.push(&new);
    let from = rows.len().saturating_sub(KEEP);
    let mut text = rows[from..].join("\n");
    text.push('\n');
    let tmp = dir.join(format!("{INDEX}.tmp"));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &idx);
    }
}

/// The cached build for `key`, when present; its use time is refreshed for the LRU.
pub fn lookup(dir: &Path, key: &str) -> Option<PathBuf> {
    let p = entry_path(dir, key);
    let f = std::fs::OpenOptions::new().write(true).open(&p).ok()?;
    let _ = f.set_modified(SystemTime::now());
    Some(p)
}

/// Cached files: (path, size, last use).
pub fn entries(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == ddc_model::PROJECT_EXT)
        })
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((e.path(), md.len(), md.modified().ok()?))
        })
        .collect()
}

/// Which files go, oldest first, until the rest fits in `cap`.
pub fn plan_eviction(files: &[(PathBuf, u64, SystemTime)], cap: u64) -> Vec<PathBuf> {
    let mut all: Vec<&(PathBuf, u64, SystemTime)> = files.iter().collect();
    all.sort_by_key(|f| f.2);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut out = vec![];
    for f in all {
        if total <= cap {
            break;
        }
        total -= f.1;
        out.push(f.0.clone());
    }
    out
}

/// Trims the cache to `cap`; returns the bytes freed.
pub fn trim(dir: &Path, cap: u64) -> u64 {
    let files = entries(dir);
    let sizes: std::collections::HashMap<PathBuf, u64> =
        files.iter().map(|f| (f.0.clone(), f.1)).collect();
    let mut freed = 0;
    for p in plan_eviction(&files, cap) {
        if std::fs::remove_file(&p).is_ok() {
            freed += sizes.get(&p).copied().unwrap_or(0);
        }
    }
    freed
}

/// Settings: "clear the import cache". Returns the bytes freed.
pub fn clear(dir: &Path) -> u64 {
    let mut freed = 0;
    for (p, size, _) in entries(dir) {
        if std::fs::remove_file(&p).is_ok() {
            freed += size;
        }
    }
    let _ = std::fs::remove_file(dir.join(INDEX));
    freed
}

/// Bytes the cache holds.
pub fn size(dir: &Path) -> u64 {
    entries(dir).iter().map(|f| f.1).sum()
}

/// Writes `model` into the cache under `key` (a safe copy, then an atomic rename). A cache that
/// cannot be written is not an error for the open.
pub fn store(
    dir: &Path,
    key: &str,
    model: &ddc_model::Model,
    step: &mut dyn FnMut(u64, u64) -> Result<(), ddc_model::Error>,
) -> Result<(), ddc_model::Error> {
    let _ = std::fs::create_dir_all(dir);
    let to = entry_path(dir, key);
    if to.exists() {
        return Ok(());
    }
    let tmp = dir.join(format!("{key}.partial"));
    match model.save_project_to(&tmp, step) {
        Ok(()) => {
            if std::fs::rename(&tmp, &to).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc-o3-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn key_changes_with_reader_and_schema() {
        let a = key("abc", READER_IFC, 39);
        assert_ne!(a, key("abc", "ifc2", 39));
        assert_ne!(a, key("abc", READER_IFC, 40));
        assert_ne!(a, key("abd", READER_IFC, 39));
        assert!(!key("abc", "a/b c", 1).contains('/'));
    }

    #[test]
    fn index_answers_only_for_the_same_stat() {
        let d = tmp("idx");
        let p = Path::new("C:/m/House.ifc");
        let s = Stat {
            size: 10,
            mtime: 20,
        };
        assert_eq!(hash_hint(&d, p, s), None);
        remember(&d, p, s, "h1");
        assert_eq!(hash_hint(&d, p, s).as_deref(), Some("h1"));
        assert_eq!(
            hash_hint(
                &d,
                p,
                Stat {
                    size: 11,
                    mtime: 20
                }
            ),
            None
        );
        assert_eq!(
            hash_hint(
                &d,
                p,
                Stat {
                    size: 10,
                    mtime: 21
                }
            ),
            None
        );
        // a changed file replaces its row
        let s2 = Stat {
            size: 12,
            mtime: 30,
        };
        remember(&d, p, s2, "h2");
        assert_eq!(hash_hint(&d, p, s2).as_deref(), Some("h2"));
        assert_eq!(hash_hint(&d, p, s), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn eviction_takes_the_oldest_until_it_fits() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let f =
            |n: &str, size: u64, age: u64| (PathBuf::from(n), size, t0 + Duration::from_secs(age));
        let files = vec![f("c", 40, 30), f("a", 40, 10), f("b", 40, 20)];
        assert_eq!(plan_eviction(&files, 120), Vec::<PathBuf>::new());
        assert_eq!(plan_eviction(&files, 80), vec![PathBuf::from("a")]);
        assert_eq!(
            plan_eviction(&files, 50),
            vec![PathBuf::from("a"), PathBuf::from("b")]
        );
        assert_eq!(plan_eviction(&files, 0).len(), 3);
    }

    #[test]
    fn lookup_misses_and_clear_empties() {
        let d = tmp("clear");
        assert!(lookup(&d, "k").is_none());
        std::fs::write(entry_path(&d, "k"), b"x").unwrap();
        assert!(lookup(&d, "k").is_some());
        assert_eq!(size(&d), 1);
        assert_eq!(trim(&d, 10), 0);
        assert_eq!(trim(&d, 0), 1);
        std::fs::write(entry_path(&d, "k"), b"xy").unwrap();
        assert_eq!(clear(&d), 2);
        assert!(lookup(&d, "k").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
