// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "Switch to that window": a project locked by another process of this application is shown
//! by bringing that process's window forward, instead of only saying it is open elsewhere.
use std::path::Path;

/// The process holding the lock of project `path`, when it is another one.
pub fn holder(path: &Path) -> Option<u32> {
    ddc_model::ProjectLock::holder_pid(path).filter(|p| *p != std::process::id())
}

/// Brings the main window of process `pid` to the front; false when none was found.
pub fn raise(pid: u32) -> bool {
    sys::raise(pid)
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(f: extern "system" fn(*mut c_void, isize) -> i32, param: isize) -> i32;
        fn GetWindowThreadProcessId(h: *mut c_void, pid: *mut u32) -> u32;
        fn IsWindowVisible(h: *mut c_void) -> i32;
        fn GetWindow(h: *mut c_void, cmd: u32) -> *mut c_void;
        fn IsIconic(h: *mut c_void) -> i32;
        fn ShowWindow(h: *mut c_void, cmd: i32) -> i32;
        fn SetForegroundWindow(h: *mut c_void) -> i32;
    }

    const GW_OWNER: u32 = 4;
    const SW_RESTORE: i32 = 9;

    struct Search {
        pid: u32,
        found: *mut c_void,
    }

    extern "system" fn each(h: *mut c_void, param: isize) -> i32 {
        // SAFETY: `param` is the address of the `Search` that `raise` keeps alive for the call.
        let s = unsafe { &mut *(param as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: plain Win32 queries on a window handle given by the system.
        unsafe {
            GetWindowThreadProcessId(h, &mut pid);
            if pid == s.pid && IsWindowVisible(h) != 0 && GetWindow(h, GW_OWNER).is_null() {
                s.found = h;
                return 0;
            }
        }
        1
    }

    pub fn raise(pid: u32) -> bool {
        let mut s = Search {
            pid,
            found: std::ptr::null_mut(),
        };
        // SAFETY: the callback runs inside this call and only touches `s`.
        unsafe {
            EnumWindows(each, (&mut s as *mut Search) as isize);
            if s.found.is_null() {
                return false;
            }
            if IsIconic(s.found) != 0 {
                ShowWindow(s.found, SW_RESTORE);
            }
            SetForegroundWindow(s.found) != 0
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn raise(_: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lock_no_holder() {
        assert_eq!(holder(Path::new("definitely/not/a/project.ddc")), None);
        assert!(!raise(u32::MAX));
    }
}
