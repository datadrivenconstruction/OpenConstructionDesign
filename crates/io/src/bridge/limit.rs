//! Memory facts and the memory ceiling of a bridge child. On Windows the child joins a job
//! object whose job-wide memory limit stops a runaway decode; the job dies with the guard.
//! Elsewhere both are no-ops (`None`).

/// Physical memory, bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
}

/// Share of physical memory a bridge child may use (percent).
pub const CHILD_SHARE: u64 = 70;

/// The child's ceiling for a machine with `total` bytes.
pub fn child_limit(total: u64) -> u64 {
    total / 100 * CHILD_SHARE
}

#[cfg(windows)]
mod sys {
    use super::Memory;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct MemStatus {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_ext: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        per_process_time: i64,
        per_job_time: i64,
        flags: u32,
        min_ws: usize,
        max_ws: usize,
        active: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters([u64; 6]);

    #[repr(C)]
    #[derive(Default)]
    struct Extended {
        basic: Basic,
        io: IoCounters,
        process_limit: usize,
        job_limit: usize,
        peak_process: usize,
        peak_job: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(s: *mut MemStatus) -> i32;
        fn CreateJobObjectW(attrs: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: i32,
            info: *mut c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
    }

    const EXTENDED_LIMIT_CLASS: i32 = 9;
    const LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
    const KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

    pub fn memory() -> Option<Memory> {
        let mut s = MemStatus {
            length: std::mem::size_of::<MemStatus>() as u32,
            load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page: 0,
            avail_page: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_ext: 0,
        };
        // SAFETY: `s` is a properly sized, initialised MEMORYSTATUSEX.
        let ok = unsafe { GlobalMemoryStatusEx(&mut s) };
        (ok != 0).then_some(Memory {
            total: s.total_phys,
            available: s.avail_phys,
        })
    }

    pub struct Guard(*mut c_void);

    // the handle is only closed in Drop
    unsafe impl Send for Guard {}

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn confine(child: &std::process::Child, max_bytes: u64) -> Option<Guard> {
        in_job(child, LIMIT_JOB_MEMORY | KILL_ON_JOB_CLOSE, max_bytes)
    }

    pub fn tie(child: &std::process::Child) -> Option<Guard> {
        in_job(child, KILL_ON_JOB_CLOSE, 0)
    }

    fn in_job(child: &std::process::Child, flags: u32, max_bytes: u64) -> Option<Guard> {
        // SAFETY: plain Win32 calls on handles owned here and by `child`.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info = Extended::default();
            info.basic.flags = flags;
            info.job_limit = max_bytes.min(usize::MAX as u64) as usize;
            let set = SetInformationJobObject(
                job,
                EXTENDED_LIMIT_CLASS,
                (&mut info as *mut Extended).cast(),
                std::mem::size_of::<Extended>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle().cast()) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Guard(job))
        }
    }
}

#[cfg(not(windows))]
mod sys {
    use super::Memory;

    pub fn memory() -> Option<Memory> {
        None
    }

    pub struct Guard;

    pub fn confine(_: &std::process::Child, _: u64) -> Option<Guard> {
        None
    }

    pub fn tie(_: &std::process::Child) -> Option<Guard> {
        None
    }
}

pub use sys::Guard;

/// Physical memory now; `None` where it cannot be asked.
pub fn memory() -> Option<Memory> {
    sys::memory()
}

/// Puts `child` under a memory ceiling; keep the guard as long as the child runs.
pub fn confine(child: &std::process::Child, max_bytes: u64) -> Option<Guard> {
    sys::confine(child, max_bytes)
}

/// Ties `child` to this process with no memory ceiling: it dies with the guard, even when this
/// process dies without running any `Drop`. Keep the guard as long as the child runs.
pub fn tie(child: &std::process::Child) -> Option<Guard> {
    sys::tie(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn a_tied_child_dies_with_the_guard() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 60 127.0.0.1 >NUL"])
            .spawn()
            .unwrap();
        let guard = tie(&child).expect("job");
        assert!(child.try_wait().unwrap().is_none());
        let started = web_time::Instant::now();
        drop(guard);
        child.wait().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn child_gets_seventy_percent() {
        assert_eq!(child_limit(16 << 30), (16u64 << 30) / 100 * 70);
        assert_eq!(child_limit(0), 0);
    }
}
