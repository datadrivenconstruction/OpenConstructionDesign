// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cause of a failed file write, enough for plain words in any language: from a typed io error,
//! or from the text of one that already became a `String` on its way out of a worker.

use std::io;

/// Why a write did not happen. `Other` keeps the system text for the details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoKind {
    /// Not enough free space (or quota) on the drive.
    Full,
    /// The folder or file is read-only, or there is no permission to write there.
    Denied,
    /// Another program holds the file open.
    Locked,
    /// The folder is no longer there.
    Missing,
    /// The drive or network share does not answer.
    Offline,
    /// The path is longer than the system allows.
    TooLong,
    Other,
}

/// Windows system error codes. A pure table, so it is tested on every platform.
pub fn win_kind(code: i32) -> Option<IoKind> {
    match code {
        // file / path not found, invalid drive
        2 | 3 | 15 => Some(IoKind::Missing),
        // access denied, write-protected medium
        5 | 19 => Some(IoKind::Denied),
        // sharing and lock violations, a mapped section holds the file
        32 | 33 | 1224 => Some(IoKind::Locked),
        // disk full (handle and volume forms), quota
        39 | 112 | 1295 => Some(IoKind::Full),
        // device not ready, network path gone or broken, net name deleted, unreachable
        21 | 51 | 53 | 59 | 64 | 67 | 1231 => Some(IoKind::Offline),
        // file name or path too long
        206 => Some(IoKind::TooLong),
        _ => None,
    }
}

/// POSIX codes shared by Linux and macOS, plus the Linux-only numbers.
pub fn unix_kind(code: i32) -> Option<IoKind> {
    match code {
        // EPERM, EACCES, EROFS
        1 | 13 | 30 => Some(IoKind::Denied),
        // ENOENT, ENOTDIR
        2 | 20 => Some(IoKind::Missing),
        // ETXTBSY
        26 => Some(IoKind::Locked),
        // ENOSPC
        28 => Some(IoKind::Full),
        // EIO on a gone mount
        5 => Some(IoKind::Offline),
        _ if cfg!(target_os = "linux") => match code {
            36 => Some(IoKind::TooLong),
            122 => Some(IoKind::Full),
            112 | 113 | 116 => Some(IoKind::Offline),
            _ => None,
        },
        _ if cfg!(target_os = "macos") => match code {
            63 => Some(IoKind::TooLong),
            69 => Some(IoKind::Full),
            64 | 65 | 70 => Some(IoKind::Offline),
            _ => None,
        },
        _ => None,
    }
}

/// Raw OS codes mean different things per platform: each has its own table.
pub fn raw_kind(code: i32) -> Option<IoKind> {
    if cfg!(windows) {
        win_kind(code)
    } else if cfg!(unix) {
        unix_kind(code)
    } else {
        None
    }
}

pub fn io_kind(e: &io::Error) -> IoKind {
    if let Some(k) = e.raw_os_error().and_then(raw_kind) {
        return k;
    }
    use io::ErrorKind as K;
    match e.kind() {
        K::PermissionDenied | K::ReadOnlyFilesystem => IoKind::Denied,
        K::NotFound | K::NotADirectory => IoKind::Missing,
        K::StorageFull => IoKind::Full,
        K::ResourceBusy | K::ExecutableFileBusy => IoKind::Locked,
        K::HostUnreachable | K::NetworkUnreachable | K::NetworkDown => IoKind::Offline,
        K::StaleNetworkFileHandle => IoKind::Offline,
        // a wrapped error without a code still prints the system words
        _ => kind_of_text(&e.to_string()),
    }
}

/// Cause of a model error while it is still typed: the io error inside, then the database
/// result code, the text only when neither names it.
pub fn model_kind(e: &ddc_model::Error) -> IoKind {
    use rusqlite::ErrorCode as C;
    match e {
        ddc_model::Error::Io(io) => io_kind(io),
        ddc_model::Error::Db(rusqlite::Error::SqliteFailure(f, _)) => match f.code {
            C::DiskFull => IoKind::Full,
            C::ReadOnly | C::PermissionDenied => IoKind::Denied,
            C::DatabaseBusy | C::DatabaseLocked => IoKind::Locked,
            _ => kind_of_text(&e.to_string()),
        },
        _ => kind_of_text(&e.to_string()),
    }
}

/// The OS code in `... (os error N)`, the form `io::Error` prints.
fn os_code(s: &str) -> Option<i32> {
    let at = s.rfind("(os error ")? + "(os error ".len();
    let rest = &s[at..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

/// Cause from an error that is only text now: the OS code first, then the database's own
/// words (it reports a full disk or a read-only file without an OS code).
pub fn kind_of_text(s: &str) -> IoKind {
    if let Some(k) = os_code(s).and_then(raw_kind) {
        return k;
    }
    let low = s.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    if has(&["database or disk is full", "no space left", "disk is full", "not enough space"]) {
        IoKind::Full
    } else if has(&["database is locked", " is in use", "used by another process"]) {
        IoKind::Locked
    } else if has(&["readonly database", "read-only", "permission denied", "access is denied"]) {
        IoKind::Denied
    } else if has(&["name too long", "filename or extension is too long"]) {
        IoKind::TooLong
    } else if has(&["network path was not found", "network name", "device is not ready"]) {
        IoKind::Offline
    } else {
        IoKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_codes_name_the_cause() {
        for (code, k) in [
            (32, IoKind::Locked),
            (33, IoKind::Locked),
            (112, IoKind::Full),
            (5, IoKind::Denied),
            (3, IoKind::Missing),
            (53, IoKind::Offline),
            (206, IoKind::TooLong),
        ] {
            assert_eq!(win_kind(code), Some(k), "{code}");
            let text = format!("cannot write x.ddc (os error {code})");
            if cfg!(windows) {
                assert_eq!(kind_of_text(&text), k, "{text}");
                assert_eq!(io_kind(&io::Error::from_raw_os_error(code)), k, "{code}");
            }
        }
        assert_eq!(win_kind(87), None);
    }

    #[test]
    fn posix_codes_name_the_cause() {
        for (code, k) in [
            (28, IoKind::Full),
            (13, IoKind::Denied),
            (30, IoKind::Denied),
            (2, IoKind::Missing),
        ] {
            assert_eq!(unix_kind(code), Some(k), "{code}");
            if cfg!(unix) {
                assert_eq!(io_kind(&io::Error::from_raw_os_error(code)), k, "{code}");
            }
        }
    }

    #[test]
    fn error_kind_without_code_still_counts() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(io_kind(&denied), IoKind::Denied);
        assert_eq!(io_kind(&io::Error::from(io::ErrorKind::NotFound)), IoKind::Missing);
        assert_eq!(io_kind(&io::Error::other("boom")), IoKind::Other);
        assert_eq!(io_kind(&io::Error::from(io::ErrorKind::StorageFull)), IoKind::Full);
        let ro = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
        assert_eq!(io_kind(&ro), IoKind::Denied);
        let busy = io::Error::from(io::ErrorKind::ResourceBusy);
        assert_eq!(io_kind(&busy), IoKind::Locked);
        // wrapped without a code: the words still count
        assert_eq!(io_kind(&io::Error::other("database is locked")), IoKind::Locked);
    }

    #[test]
    fn typed_model_errors_use_the_code_not_the_words() {
        let db = |code| {
            let f = rusqlite::ffi::Error { code, extended_code: 0 };
            ddc_model::Error::Db(rusqlite::Error::SqliteFailure(f, Some("odd".into())))
        };
        assert_eq!(model_kind(&db(rusqlite::ErrorCode::DiskFull)), IoKind::Full);
        assert_eq!(model_kind(&db(rusqlite::ErrorCode::ReadOnly)), IoKind::Denied);
        assert_eq!(model_kind(&db(rusqlite::ErrorCode::DatabaseBusy)), IoKind::Locked);
        let io = ddc_model::Error::Io(io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(model_kind(&io), IoKind::Full);
        assert_eq!(model_kind(&ddc_model::Error::Invalid("odd".into())), IoKind::Other);
    }

    #[test]
    fn database_words_name_the_cause() {
        for (text, k) in [
            ("database or disk is full", IoKind::Full),
            ("attempt to write a readonly database", IoKind::Denied),
            ("database is locked", IoKind::Locked),
            ("invalid: C:/p/x.ddc is in use", IoKind::Locked),
            ("permission denied", IoKind::Denied),
            ("something odd happened", IoKind::Other),
            ("weird (os error 99999)", IoKind::Other),
            ("", IoKind::Other),
        ] {
            assert_eq!(kind_of_text(text), k, "{text}");
        }
    }

    #[test]
    fn os_code_is_read_from_the_last_parenthesis() {
        assert_eq!(os_code("a (os error 3) then (os error 112)"), Some(112));
        assert_eq!(os_code("no code"), None);
        assert_eq!(os_code("(os error x)"), None);
    }
}
