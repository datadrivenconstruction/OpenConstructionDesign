//! Windows integration without extra crates: reduced-motion setting and the native title bar
//! (dark mode, rounded corners). Other platforms: no-ops.

/// «Show animations in Windows» off → the UI turns motion off.
pub fn reduced_motion() -> bool {
    imp::reduced_motion()
}

/// Dark or light native title bar and rounded corners on the editor window.
pub fn style_title_bar(title: &str, dark: bool) {
    imp::style_title_bar(title, dark)
}

/// Primary work area and the whole desktop, in logical points: `(work, desktop)` as
/// `[left, top, right, bottom]`. None where the system gives no answer.
pub fn screens() -> Option<([f32; 4], [f32; 4])> {
    imp::screens()
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;

    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, ini: u32) -> i32;
        fn FindWindowW(class: *const u16, name: *const u16) -> *mut c_void;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
        fn GetDpiForSystem() -> u32;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: *mut c_void, attr: u32, pv: *const c_void, cb: u32) -> i32;
    }

    pub fn screens() -> Option<([f32; 4], [f32; 4])> {
        let mut r = [0i32; 4];
        // SAFETY: SPI_GETWORKAREA writes one RECT (four i32) into `r`.
        let ok = unsafe { SystemParametersInfoW(0x0030, 0, r.as_mut_ptr().cast(), 0) };
        if ok == 0 {
            return None;
        }
        // SAFETY: plain queries without pointers.
        let (vx, vy, vw, vh, dpi) = unsafe {
            (
                GetSystemMetrics(76),
                GetSystemMetrics(77),
                GetSystemMetrics(78),
                GetSystemMetrics(79),
                GetDpiForSystem(),
            )
        };
        if vw <= 0 || vh <= 0 {
            return None;
        }
        let k = if dpi >= 96 { 96.0 / dpi as f32 } else { 1.0 };
        let f = |a: [i32; 4]| a.map(|v| v as f32 * k);
        Some((f(r), f([vx, vy, vx + vw, vy + vh])))
    }

    pub fn reduced_motion() -> bool {
        let mut on: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into `on`.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut on as *mut i32).cast(),
                0,
            )
        };
        ok != 0 && on == 0
    }

    pub fn style_title_bar(title: &str, dark: bool) {
        let name: Vec<u16> = title.encode_utf16().chain([0]).collect();
        // SAFETY: null-terminated UTF-16 title; the handle is checked before use.
        let hwnd = unsafe { FindWindowW(std::ptr::null(), name.as_ptr()) };
        if hwnd.is_null() {
            return;
        }
        // only our own window (another app may share the title)
        let mut pid = 0u32;
        // SAFETY: valid window handle, `pid` is a plain out-parameter.
        let _ = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != std::process::id() {
            return;
        }
        let v: u32 = u32::from(dark);
        let r: u32 = DWMWCP_ROUND;
        // SAFETY: both attributes take a 4-byte value; failures (older Windows) are ignored.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&v as *const u32).cast(),
                4,
            );
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&r as *const u32).cast(),
                4,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn reduced_motion() -> bool {
        false
    }
    pub fn style_title_bar(_: &str, _: bool) {}
    pub fn screens() -> Option<([f32; 4], [f32; 4])> {
        None
    }
}
