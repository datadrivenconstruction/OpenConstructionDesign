// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Is the workbook open in another program with unsaved edits? The spreadsheet app keeps an
//! owner file `~$<name>` next to an open book and holds the book itself with a share lock.
//! Both are only read here: the owner file is never written, touched or removed.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use web_time::SystemTime;

/// Error text of a read or a replace stopped by an open book; `book_error` shows it as the
/// "open elsewhere" line.
pub const OPEN_ELSEWHERE: &str = "book is open elsewhere (save it there first)";

/// What the owner file and the share lock say about a book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// No sign of another program: read as usual.
    Free,
    /// An owner file older than the book and the book readable: left over or saved since;
    /// read, with a soft note.
    Soft,
    /// A current owner file: the book may hold unsaved edits there. Ask first.
    Ask,
    /// The book itself is share-locked by another program.
    Locked,
}

impl Gate {
    pub fn blocks(self) -> bool {
        matches!(self, Gate::Ask | Gate::Locked)
    }
}

/// The owner file under its full name; long names lose their first characters in the real one,
/// see `owner_files`.
pub fn owner_file(book: &Path) -> PathBuf {
    owner_files(book).remove(0)
}

/// Every name the owner file can have: `~$` plus the file name, or the name without its first
/// one or two characters (the short-name rule of long names). Characters, not bytes.
pub fn owner_files(book: &Path) -> Vec<PathBuf> {
    let name = book.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    (0..3)
        .map(|skip| {
            let rest: String = name.chars().skip(skip).collect();
            book.with_file_name(format!("~${rest}"))
        })
        .collect()
}

fn modified(p: &Path) -> Option<SystemTime> {
    ddc_model::wtime::modified(&std::fs::metadata(p).ok()?).ok()
}

/// A sharing violation: another program holds the file.
pub fn open_elsewhere(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// An I/O failure of a replace as the error text: the open-book line for a sharing or lock
/// violation only; "access denied" (read-only file, no rights) keeps its own text.
pub fn failed(e: &io::Error) -> String {
    if open_elsewhere(e) {
        OPEN_ELSEWHERE.to_string()
    } else {
        e.to_string()
    }
}

/// Error text of a write stopped by a read-only book (file property);
/// `book_error` shows it with the "export to a copy" way out.
pub const READ_ONLY: &str = "book is read-only (file property or folder permission)";

/// Error text of a write stopped by missing rights (folder or file ACL). A copy beside the book
/// fails the same way, so `book_error` shows it with "Save to another folder" instead.
pub const DENIED: &str = "no rights to write in this folder";

/// The file chosen for a copy is taken; shown through `book_error`.
pub const COPY_EXISTS: &str = "the copy file already exists";

/// Why a write over `path` failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// Another program holds the book (sharing or lock violation).
    Held,
    /// The file carries the read-only attribute.
    ReadOnly,
    /// Access denied for another reason (rights of the file or the folder).
    Denied,
    Other(String),
}

/// The book has the read-only attribute.
pub fn read_only(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly())
}

pub fn why(path: &Path, e: &io::Error) -> Why {
    if open_elsewhere(e) {
        Why::Held
    } else if read_only(path) {
        Why::ReadOnly
    } else if e.kind() == io::ErrorKind::PermissionDenied || (cfg!(windows) && e.raw_os_error() == Some(5)) {
        Why::Denied
    } else {
        Why::Other(e.to_string())
    }
}

/// `failed` for a replace of `path`: a read-only book gives the read-only line (copy offered),
/// missing rights the denied one (another folder offered), never the open-elsewhere one.
pub fn failed_at(path: &Path, e: &io::Error) -> String {
    match why(path, e) {
        Why::Held => OPEN_ELSEWHERE.to_string(),
        Why::ReadOnly => READ_ONLY.to_string(),
        Why::Denied => DENIED.to_string(),
        Why::Other(t) => t,
    }
}

fn allowed() -> &'static Mutex<HashSet<PathBuf>> {
    static A: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    A.get_or_init(Default::default)
}

/// "Read anyway": the owner file of this book is ignored for the rest of the session.
pub fn read_anyway(book: &Path) {
    if let Ok(mut a) = allowed().lock() {
        a.insert(book.to_path_buf());
    }
}

/// The soft "lock file left beside the book" note is due once per book and owner file stamp,
/// not on every re-read of the poll.
pub fn soft_note_due(book: &Path) -> bool {
    static N: OnceLock<Mutex<HashSet<(PathBuf, Option<SystemTime>)>>> = OnceLock::new();
    let stamp = owner_files(book).into_iter().find(|p| p.is_file()).and_then(|p| modified(&p));
    N.get_or_init(Default::default)
        .lock()
        .is_ok_and(|mut n| n.insert((book.to_path_buf(), stamp)))
}

fn is_allowed(book: &Path) -> bool {
    allowed().lock().is_ok_and(|a| a.contains(book))
}

fn gate_of(book: &Path, write: bool) -> Gate {
    let opened = if write {
        std::fs::OpenOptions::new().write(true).open(book).map(drop)
    } else {
        std::fs::File::open(book).map(drop)
    };
    if let Err(e) = opened {
        // a missing or unreadable-for-other-reasons book is the reader's own error
        return if open_elsewhere(&e) { Gate::Locked } else { Gate::Free };
    }
    let Some(owner) = owner_files(book).into_iter().find(|p| p.is_file()) else {
        return Gate::Free;
    };
    match (modified(&owner), modified(book)) {
        (Some(o), Some(b)) if o < b => Gate::Soft,
        _ if is_allowed(book) => Gate::Soft,
        _ => Gate::Ask,
    }
}

/// The gate of a read.
pub fn check(book: &Path) -> Gate {
    gate_of(book, false)
}

/// The gate of a replace: the book must also be open for writing.
pub fn check_replace(book: &Path) -> Gate {
    gate_of(book, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_xs12_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_age(p: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
    }

    fn book_and_owner(tag: &str, owner_age: u64, book_age: u64) -> (PathBuf, PathBuf) {
        let d = dir(tag);
        let book = d.join("plan.xlsx");
        std::fs::write(&book, b"book bytes").unwrap();
        let owner = owner_file(&book);
        std::fs::write(&owner, b"owner bytes").unwrap();
        set_age(&book, book_age);
        set_age(&owner, owner_age);
        (book, owner)
    }

    #[test]
    fn owner_names_short_long_and_unicode() {
        let p = Path::new("C:/data/plan.xlsx");
        assert_eq!(owner_file(p).file_name().unwrap(), "~$plan.xlsx");
        let all = owner_files(Path::new("C:/data/Quarterly report.xlsx"));
        let names: Vec<_> = all.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["~$Quarterly report.xlsx", "~$uarterly report.xlsx", "~$arterly report.xlsx"]);
        // characters, never a cut inside a multi-byte one
        let uni = owner_files(Path::new("D:/\u{00dc}bersicht \u{4e2d}\u{6587}.xlsx"));
        assert_eq!(uni[1].file_name().unwrap(), "~$bersicht \u{4e2d}\u{6587}.xlsx");
        assert_eq!(uni[0].parent(), Path::new("D:/\u{00dc}bersicht \u{4e2d}\u{6587}.xlsx").parent());
        assert_eq!(owner_files(Path::new("a")).len(), 3);
    }

    #[test]
    fn current_owner_file_asks_and_absent_reads() {
        let (book, owner) = book_and_owner("ask", 0, 60);
        assert_eq!(check(&book), Gate::Ask);
        assert!(check(&book).blocks());
        std::fs::remove_file(&owner).unwrap();
        assert_eq!(check(&book), Gate::Free);
        assert!(!check(&book).blocks());
        let _ = std::fs::remove_dir_all(book.parent().unwrap());
    }

    #[test]
    fn long_name_owner_file_is_found() {
        let d = dir("long");
        let book = d.join("Quarterly report.xlsx");
        std::fs::write(&book, b"b").unwrap();
        std::fs::write(d.join("~$arterly report.xlsx"), b"o").unwrap();
        assert_eq!(check(&book), Gate::Ask);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_owner_file_warns_softly_and_allows() {
        let (book, _owner) = book_and_owner("stale", 120, 10);
        assert_eq!(check(&book), Gate::Soft);
        assert!(!check(&book).blocks());
        let _ = std::fs::remove_dir_all(book.parent().unwrap());
    }

    #[test]
    fn read_anyway_is_remembered_per_path() {
        let (book, _o) = book_and_owner("anyway", 0, 60);
        let (other, _o2) = book_and_owner("anyway2", 0, 60);
        assert_eq!(check(&book), Gate::Ask);
        read_anyway(&book);
        assert_eq!(check(&book), Gate::Soft);
        assert_eq!(check(&other), Gate::Ask, "another book still asks");
        let _ = std::fs::remove_dir_all(book.parent().unwrap());
        let _ = std::fs::remove_dir_all(other.parent().unwrap());
    }

    #[test]
    fn owner_file_is_never_modified() {
        let (book, owner) = book_and_owner("readonly", 0, 60);
        let before = (std::fs::read(&owner).unwrap(), modified(&owner));
        let _ = (check(&book), check_replace(&book));
        read_anyway(&book);
        let _ = check(&book);
        assert_eq!((std::fs::read(&owner).unwrap(), modified(&owner)), before);
        assert!(owner.is_file(), "not removed");
        let _ = std::fs::remove_dir_all(book.parent().unwrap());
    }

    #[test]
    fn missing_book_is_the_readers_error() {
        assert_eq!(check(Path::new("no/such/dir/book.xlsx")), Gate::Free);
    }

    #[test]
    fn sharing_violation_is_the_open_line() {
        let e = io::Error::from_raw_os_error(32);
        assert_eq!(failed(&e), if cfg!(windows) { OPEN_ELSEWHERE.to_string() } else { e.to_string() });
        assert_eq!(failed(&io::Error::other("boom")), "boom");
        // access denied is a read-only file or missing rights, not an open book
        assert_ne!(failed(&io::Error::from_raw_os_error(5)), OPEN_ELSEWHERE);
    }

    #[test]
    fn soft_note_once_per_owner_stamp() {
        let (book, owner) = book_and_owner("note", 120, 10);
        assert!(soft_note_due(&book));
        assert!(!soft_note_due(&book), "not again on the next poll");
        set_age(&owner, 60);
        assert!(soft_note_due(&book), "a new owner file stamp is a new note");
        let _ = std::fs::remove_dir_all(book.parent().unwrap());
    }

    fn set_read_only(p: &Path, on: bool) {
        let mut perm = std::fs::metadata(p).unwrap().permissions();
        perm.set_readonly(on);
        std::fs::set_permissions(p, perm).unwrap();
    }

    #[test]
    fn why_tells_held_read_only_denied_and_other() {
        let d = dir("why");
        let book = d.join("plan.xlsx");
        std::fs::write(&book, b"x").unwrap();
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(why(&book, &denied), Why::Denied);
        assert_eq!(why(&book, &io::Error::other("boom")), Why::Other("boom".into()));
        if cfg!(windows) {
            assert_eq!(why(&book, &io::Error::from_raw_os_error(32)), Why::Held);
        }
        set_read_only(&book, true);
        assert_eq!(why(&book, &denied), Why::ReadOnly);
        assert_eq!(failed_at(&book, &denied), READ_ONLY);
        if cfg!(windows) {
            // a held book stays held even when it is also read-only
            assert_eq!(why(&book, &io::Error::from_raw_os_error(33)), Why::Held);
            assert_eq!(failed_at(&book, &io::Error::from_raw_os_error(33)), OPEN_ELSEWHERE);
        }
        set_read_only(&book, false);
        // rights without the attribute: another folder is the way out
        assert_eq!(failed_at(&book, &denied), DENIED);
        assert_eq!(failed_at(&book, &io::Error::other("boom")), "boom");
        assert!(!read_only(&d.join("missing.xlsx")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn share_locked_book_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir("share");
        let book = d.join("held.xlsx");
        std::fs::write(&book, b"x").unwrap();
        {
            // held against reading and writing
            let _h = std::fs::OpenOptions::new().read(true).share_mode(0).open(&book).unwrap();
            assert_eq!(check(&book), Gate::Locked);
            assert_eq!(check_replace(&book), Gate::Locked);
        }
        {
            // held against writing only: a read goes through, a replace does not
            let _h = std::fs::OpenOptions::new().read(true).share_mode(1).open(&book).unwrap();
            assert_eq!(check(&book), Gate::Free);
            assert_eq!(check_replace(&book), Gate::Locked);
        }
        assert_eq!(check(&book), Gate::Free);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn denied_is_its_own_line_and_read_only_file_keeps_its_own() {
        let d = dir("denied");
        let book = d.join("plan.xlsx");
        std::fs::write(&book, b"book bytes").unwrap();
        let e = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(why(&book, &e), Why::Denied);
        assert_eq!(failed_at(&book, &e), DENIED);
        assert_ne!(failed_at(&book, &e), READ_ONLY);
        let mut perm = std::fs::metadata(&book).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&book, perm.clone()).unwrap();
        assert_eq!(failed_at(&book, &e), READ_ONLY, "a read-only file keeps the copy line");
        perm.set_readonly(false);
        std::fs::set_permissions(&book, perm).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
