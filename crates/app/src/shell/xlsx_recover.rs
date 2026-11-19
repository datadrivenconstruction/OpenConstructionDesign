//! An export that replaces the linked book moves the old book to `Book.xlsx.bak` and only then
//! puts the new one (`Book.export-part.xlsx`) in its place. A crash, a power cut or a failed
//! move between the two leaves no `Book.xlsx`: these are the leftovers found beside it, and the
//! two ways back, each chosen by the user and never done silently.
use std::path::{Path, PathBuf};

use super::xlsx_relink::book_id;

/// What an interrupted export left beside the missing book `dest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    pub dest: PathBuf,
    /// The new book: it opens and names this project.
    pub part: Option<PathBuf>,
    /// The newest kept old book (`Book.xlsx.bak`, `Book.xlsx.2.bak`, ...).
    pub bak: Option<PathBuf>,
}

/// Generation of `file` as a backup of the book named `name`: 1 for `name.bak`, n for
/// `name.n.bak`; None for any other file.
fn generation(name: &str, file: &str) -> Option<u32> {
    if file == format!("{name}.bak") {
        return Some(1);
    }
    let mid = file.strip_prefix(name)?.strip_prefix('.')?.strip_suffix(".bak")?;
    mid.parse::<u32>().ok().filter(|n| *n >= 2)
}

/// The newest backup of `dest` on the disk: latest write time, then name generation. A backup
/// takes the first free generation, so after `Book.xlsx.bak` is deleted the next one is
/// `.bak` again while older `.2.bak`/`.3.bak` still sit beside it.
pub fn newest_backup(dest: &Path) -> Option<PathBuf> {
    let name = dest.file_name()?.to_string_lossy().into_owned();
    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let g = generation(&name, &e.file_name().to_string_lossy())?;
            let p = e.path();
            let m = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            Some(((m.modified().ok(), g), p))
        })
        .max_by_key(|(k, _)| *k)
        .map(|(_, p)| p)
}

/// The leftovers of an interrupted export into `dest`; None while `dest` exists or nothing is
/// left. The part counts only when it opens (a cut-off write does not) and names project `uid`.
pub fn interrupted(dest: &Path, uid: Option<i64>) -> Option<Leftover> {
    if dest.exists() {
        return None;
    }
    let part = Some(super::xlsx_job::part_of(dest))
        .filter(|p| p.is_file())
        .filter(|p| uid.is_some() && book_id(p).is_some_and(|(project, _)| project == uid));
    let bak = newest_backup(dest);
    (part.is_some() || bak.is_some()).then(|| Leftover { dest: dest.to_path_buf(), part, bak })
}

fn shown(p: &Path) -> String {
    p.display().to_string()
}

/// Card text: what was found and where.
pub fn line(l: &Leftover, ru: bool) -> String {
    match (&l.part, &l.bak) {
        (Some(p), Some(b)) => crate::tr_lang(ru, "xlsx_recover.both", &[("part", &shown(p)), ("bak", &shown(b))]),
        (Some(p), None) => crate::tr_lang(ru, "xlsx_recover.part_only", &[("part", &shown(p))]),
        (None, Some(b)) => crate::tr_lang(ru, "xlsx_recover.bak_only", &[("bak", &shown(b))]),
        (None, None) => String::new(),
    }
}

/// Status text after a move back into place failed while the old book is at `bak`.
pub fn restore_failed(ru: bool, bak: &Path, err: &str) -> String {
    crate::tr_lang(ru, "xlsx_recover.restore_failed", &[("path", &shown(bak)), ("err", err)])
}

/// Marks an export failure whose old book could not be moved back: the rest is the `.bak` path.
pub const NOT_RESTORED: &str = "xs30:not-restored:";

/// The failure text an export returns when the old book stays at `bak`.
pub fn not_restored(bak: &Path) -> String {
    format!("{NOT_RESTORED}{}", bak.display())
}

/// The export failure in words when it is a [`not_restored`] one.
pub fn not_restored_text(e: &str, ru: bool) -> Option<String> {
    let bak = e.strip_prefix(NOT_RESTORED)?;
    Some(crate::tr_lang(ru, "xlsx_recover.not_restored", &[("path", bak)]))
}

/// The new book goes into place; the old one stays at its `.bak` name. Nothing is replaced: a
/// book back at `dest` refuses, and a failed move keeps the part.
pub fn finish(l: &Leftover, ru: bool) -> Result<String, String> {
    let Some(part) = &l.part else {
        return Err(crate::tr_lang(ru, "xlsx_recover.nothing", &[]));
    };
    if l.dest.exists() {
        return Err(crate::tr_lang(ru, "xlsx_recover.dest_back", &[("path", &shown(&l.dest))]));
    }
    ddc_io::fsutil::sync_file(part)
        .and_then(|_| ddc_io::fsutil::rename_durably(part, &l.dest))
        .map_err(|e| crate::tr_lang(ru, "xlsx_recover.finish_failed", &[("path", &shown(part)), ("err", &e.to_string())]))?;
    Ok(match &l.bak {
        Some(b) => crate::tr_lang(ru, "xlsx_recover.finished_kept", &[("bak", &shown(b))]),
        None => crate::tr_lang(ru, "xlsx_recover.finished", &[]),
    })
}

/// The old book goes back to `dest`; the part stays where it is. A failure names the `.bak`.
pub fn restore(l: &Leftover, ru: bool) -> Result<String, String> {
    let Some(bak) = &l.bak else {
        return Err(crate::tr_lang(ru, "xlsx_recover.nothing", &[]));
    };
    if l.dest.exists() {
        return Err(crate::tr_lang(ru, "xlsx_recover.dest_back", &[("path", &shown(&l.dest))]));
    }
    ddc_io::fsutil::rename_durably(bak, &l.dest).map_err(|e| restore_failed(ru, bak, &e.to_string()))?;
    Ok(crate::tr_lang(ru, "xlsx_recover.restored", &[]))
}

#[cfg(test)]
#[path = "xlsx_recover_tests.rs"]
mod tests;
