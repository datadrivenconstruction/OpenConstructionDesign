// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Graphics backend choice: env override → probe in preference order (Windows: DX12, Vulkan, GL),
//! `--safe-graphics` → software adapter. Result is logged to `<data dir>/logs/startup.log`.
#[cfg(feature = "gui")]
use eframe::egui_wgpu::{self, wgpu};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[cfg(feature = "gui")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pick {
    pub backends: wgpu::Backends,
    /// Software (fallback) adapter, e.g. the system rasterizer on Windows.
    pub software: bool,
}

#[cfg(feature = "gui")]
/// Candidates in order. Old GL drivers on integrated GPUs crash inside the driver, so GL is last.
pub fn candidates(env: Option<wgpu::Backends>, safe: bool) -> Vec<Pick> {
    let p = |backends, software| Pick { backends, software };
    if let Some(b) = env {
        return vec![p(b, safe)];
    }
    if safe {
        return if cfg!(windows) {
            vec![p(wgpu::Backends::DX12, true), p(wgpu::Backends::GL, true)]
        } else {
            vec![p(wgpu::Backends::all(), true)]
        };
    }
    if cfg!(windows) {
        vec![
            p(wgpu::Backends::DX12, false),
            p(wgpu::Backends::VULKAN, false),
            p(wgpu::Backends::GL, false),
        ]
    } else if cfg!(target_os = "macos") {
        vec![p(wgpu::Backends::METAL, false)]
    } else {
        vec![
            p(wgpu::Backends::VULKAN, false),
            p(wgpu::Backends::GL, false),
        ]
    }
}

#[cfg(feature = "gui")]
/// Order after a driver crash, hardware GL never included. Position 0 equals the normal first
/// choice, so a start that crashed there moves on to position 1.
pub fn recovery_ladder() -> Vec<Pick> {
    let p = |backends, software| Pick { backends, software };
    if cfg!(windows) {
        vec![
            p(wgpu::Backends::DX12, false),
            p(wgpu::Backends::VULKAN, false),
            p(wgpu::Backends::DX12, true),
            p(wgpu::Backends::GL, true),
        ]
    } else if cfg!(target_os = "macos") {
        vec![p(wgpu::Backends::METAL, false), p(wgpu::Backends::all(), true)]
    } else {
        vec![p(wgpu::Backends::VULKAN, false), p(wgpu::Backends::all(), true)]
    }
}

#[cfg(feature = "gui")]
/// Where a chosen back end stands on the ladder. Anything off the ladder (hardware GL of the normal
/// order) is charged to the step before software, so its crash goes straight to software.
pub fn ladder_position(ladder: &[Pick], pick: Pick) -> u32 {
    if let Some(i) = ladder.iter().position(|p| *p == pick) {
        return i as u32;
    }
    let soft = ladder
        .iter()
        .position(|p| p.software)
        .unwrap_or(ladder.len());
    soft.saturating_sub(1) as u32
}

static WINDOW_SAMPLES: AtomicU32 = AtomicU32::new(4);

/// Multisampling of the window's own pass. The renderer's output pipeline must match it.
pub fn window_samples() -> u32 {
    WINDOW_SAMPLES.load(Ordering::Relaxed)
}

pub fn set_window_samples(n: u32) {
    WINDOW_SAMPLES.store(n.max(1), Ordering::Relaxed);
}

#[cfg(feature = "gui")]
pub fn instance(pick: Pick) -> wgpu::Instance {
    // validation layers of some drivers crash without a window
    instance_with(pick, wgpu::InstanceFlags::empty())
}

#[cfg(feature = "gui")]
/// As `instance`, with explicit flags: tests turn the API validation layer on.
pub fn instance_with(pick: Pick, flags: wgpu::InstanceFlags) -> wgpu::Instance {
    wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: pick.backends,
        flags,
        ..Default::default()
    })
}

#[cfg(feature = "gui")]
pub fn describe(a: &wgpu::Adapter) -> String {
    let i = a.get_info();
    format!(
        "{} ({:?}, {:?}, driver {} {})",
        i.name, i.backend, i.device_type, i.driver, i.driver_info
    )
}

#[cfg(feature = "gui")]
/// Adapter + device for `pick`, or why not.
pub fn open(pick: Pick) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    open_with(pick, wgpu::InstanceFlags::empty())
}

#[cfg(feature = "gui")]
/// As `open`, on an instance with `flags`.
pub fn open_with(
    pick: Pick,
    flags: wgpu::InstanceFlags,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    #[cfg(target_arch = "wasm32")]
    {
        // no blocking wait in the browser: the page's device comes from eframe's runner
        let _ = (pick, flags);
        return Err("offscreen device not available in the browser".into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let inst = instance_with(pick, flags);
    #[cfg(not(target_arch = "wasm32"))]
    pollster::block_on(async {
        let adapter = inst
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: pick.software,
                compatible_surface: None,
            })
            .await
            .map_err(|e| format!("no adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&device_descriptor(&adapter))
            .await
            .map_err(|e| format!("no device: {e}"))?;
        Ok((adapter, device, queue))
    })
}

#[cfg(feature = "gui")]
/// First candidate whose device opens; `log` receives one line per attempt.
pub fn choose(cands: &[Pick], log: &mut dyn FnMut(String)) -> Option<(Pick, String)> {
    for &c in cands {
        match open(c) {
            Ok((a, _, _)) => {
                let d = describe(&a);
                log(format!(
                    "ok   {:?} software={} → {d}",
                    c.backends, c.software
                ));
                return Some((c, d));
            }
            Err(e) => log(format!(
                "fail {:?} software={}: {e}",
                c.backends, c.software
            )),
        }
    }
    None
}

#[cfg(feature = "gui")]
/// Software steps of the recovery ladder not among `tried`: the last resort when every normal
/// candidate failed.
pub fn fallback_after(tried: &[Pick]) -> Vec<Pick> {
    recovery_ladder()
        .into_iter()
        .filter(|p| p.software && !tried.contains(p))
        .collect()
}

#[cfg(feature = "gui")]
/// A probe outcome: adapter description, or why it failed.
pub type Probe<'a> = &'a mut dyn FnMut(Pick) -> Result<String, String>;

#[cfg(feature = "gui")]
/// First candidate from `start` whose device opens, then (with `fallback`) the software steps
/// not tried yet. Ok: pick, description, index, candidate count; Err: the last failure.
pub fn pick_from(
    cands: &[Pick],
    start: usize,
    fallback: bool,
    probe: Probe<'_>,
    log: &mut dyn FnMut(String),
) -> Result<(Pick, String, usize, usize), String> {
    let mut last = String::from("no graphics back end to try");
    for (i, &c) in cands.iter().enumerate().skip(start) {
        match probe(c) {
            Ok(d) => return Ok((c, d, i, cands.len())),
            Err(e) => last = e,
        }
    }
    if fallback {
        for c in fallback_after(cands) {
            log(format!("fallback {:?} software={}", c.backends, c.software));
            match probe(c) {
                // past the end: a window failure here has no next candidate to relaunch on
                Ok(d) => return Ok((c, d, cands.len(), cands.len())),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
}

#[cfg(feature = "gui")]
/// The real probe for `pick_from`: opens the device, one log line per attempt.
pub fn probe_device(pick: Pick, log: &mut dyn FnMut(String)) -> Result<String, String> {
    let r = open(pick).map(|(a, _, _)| describe(&a));
    match &r {
        Ok(d) => log(format!(
            "ok   {:?} software={} → {d}",
            pick.backends, pick.software
        )),
        Err(e) => log(format!(
            "fail {:?} software={}: {e}",
            pick.backends, pick.software
        )),
    }
    r
}

/// Device lost or a fatal device error, set from wgpu callbacks, taken by the frame loop.
#[derive(Clone, Default)]
pub struct LostFlag(Arc<std::sync::Mutex<Option<String>>>);

impl LostFlag {
    pub fn mark(&self, reason: String) {
        if let Ok(mut g) = self.0.lock() {
            g.get_or_insert(reason);
        }
    }
    pub fn take(&self) -> Option<String> {
        self.0.lock().ok().and_then(|mut g| g.take())
    }
}

#[cfg(feature = "gui")]
/// Routes device loss and out-of-memory / internal errors to `flag` instead of a panic.
/// Validation errors are bugs: they still panic in debug builds and are logged in release.
pub fn watch_device(device: &wgpu::Device, flag: &LostFlag) {
    let f = flag.clone();
    device.set_device_lost_callback(move |reason, msg| {
        // Destroyed comes from our own teardown, not from the driver
        if reason != wgpu::DeviceLostReason::Destroyed {
            f.mark(format!("device lost: {msg}"));
        }
    });
    let f = flag.clone();
    device.on_uncaptured_error(Box::new(move |e| match e {
        wgpu::Error::Validation { .. } => {
            eprintln!("[gfx] validation error: {e}");
            debug_assert!(false, "wgpu validation error: {e}");
        }
        other => f.mark(format!("device error: {other}")),
    }));
}

/// Buttons of a native message box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Buttons {
    Ok,
    YesNo,
    YesNoCancel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    Yes,
    No,
    Cancel,
}

/// A native error box with no graphics device behind it; stderr where there is none.
#[cfg(windows)]
pub fn message_box(title: &str, body: &str, buttons: Buttons) -> Answer {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (t, b) = (w(title), w(body));
    // MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST
    let kind = 0x10 | 0x1_0000 | 0x4_0000;
    let kind = kind
        | match buttons {
            Buttons::Ok => 0,
            Buttons::YesNoCancel => 3,
            Buttons::YesNo => 4,
        };
    // SAFETY: both strings are NUL-terminated UTF-16 buffers alive for the call; no owner window.
    match unsafe { MessageBoxW(0, b.as_ptr(), t.as_ptr(), kind) } {
        6 => Answer::Yes,
        7 => Answer::No,
        _ => Answer::Cancel,
    }
}

#[cfg(not(windows))]
pub fn message_box(title: &str, body: &str, _buttons: Buttons) -> Answer {
    eprintln!("{title}\n\n{body}");
    Answer::Cancel
}

/// Opens a file in its default app (the startup log); failures are ignored.
pub fn open_path(p: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
}

#[cfg(feature = "gui")]
/// Default device, but buffers as large as the adapter allows: big imported models exceed 256 MiB.
pub fn device_descriptor(a: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    let base = if a.get_info().backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits {
            max_texture_dimension_2d: 8192.min(a.limits().max_texture_dimension_2d),
            max_buffer_size: a.limits().max_buffer_size,
            ..base
        },
        ..Default::default()
    }
}

#[cfg(feature = "gui")]
/// eframe setup restricted to one backend (and the software adapter if asked).
pub fn wgpu_options(pick: Pick) -> egui_wgpu::WgpuConfiguration {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::default();
    setup.instance_descriptor.backends = pick.backends;
    setup.instance_descriptor.flags = wgpu::InstanceFlags::empty();
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    setup.device_descriptor = Arc::new(device_descriptor);
    if pick.software {
        setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
            adapters
                .iter()
                .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
                .or(adapters.first())
                .cloned()
                .ok_or_else(|| "no adapter".to_string())
        }));
    }
    egui_wgpu::WgpuConfiguration {
        wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// `%APPDATA%\ddc-editor` (same root as bridges), else `~/.config/ddc-editor`.
pub fn data_dir() -> Option<PathBuf> {
    ddc_ai::profile::default_data_dir()
}

/// Startup log, rewritten on each launch; failures to write are ignored.
pub struct StartupLog {
    file: Option<std::fs::File>,
    pub path: Option<PathBuf>,
}

impl StartupLog {
    pub fn open() -> Self {
        let path = data_dir().map(|d| d.join("logs").join("startup.log"));
        // a relaunch after a device loss keeps the lost session's lines
        let keep = std::env::var_os("DDC_GFX_RESETS").is_some();
        let file = path.as_ref().and_then(|p| {
            std::fs::create_dir_all(p.parent()?).ok()?;
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(keep)
                .truncate(!keep)
                .open(p)
                .ok()
        });
        Self { file, path }
    }

    /// One line appended to the startup log of this run, from outside the start.
    pub fn append(s: &str) {
        eprintln!("[startup] {s}");
        if let Some(p) = data_dir().map(|d| d.join("logs").join("startup.log")) {
            let f = std::fs::OpenOptions::new().append(true).open(p);
            if let Ok(mut f) = f {
                let _ = writeln!(f, "{s}");
            }
        }
    }
    pub fn line(&mut self, s: &str) {
        eprintln!("[startup] {s}");
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{s}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_never_uses_hardware_gl_and_a_gl_crash_goes_to_software() {
        let l = recovery_ladder();
        assert!(l
            .iter()
            .all(|p| p.software || !p.backends.contains(wgpu::Backends::GL)));
        let last = l.len() as u32 - 1;
        assert_eq!(ladder_position(&l, l[0]), 0);
        assert_eq!(ladder_position(&l, l[last as usize]), last);
        let hard_gl = Pick {
            backends: wgpu::Backends::GL,
            software: false,
        };
        let next = crate::gfx_guard::next_rung(0, Some(ladder_position(&l, hard_gl)), last);
        assert!(l[next as usize].software);
    }

    #[test]
    fn order_prefers_dx12_then_vulkan_then_gl() {
        let c = candidates(None, false);
        if cfg!(windows) {
            let b: Vec<_> = c.iter().map(|p| p.backends).collect();
            assert_eq!(
                b,
                [
                    wgpu::Backends::DX12,
                    wgpu::Backends::VULKAN,
                    wgpu::Backends::GL
                ]
            );
        }
        assert!(c.iter().all(|p| !p.software));
    }

    #[test]
    fn no_hardware_adapter_falls_back_to_software() {
        let cands = candidates(None, false);
        let mut tried = Vec::new();
        let mut probe = |p: Pick| {
            tried.push(p);
            if p.software {
                Ok("soft".to_string())
            } else {
                Err("no adapter: none".to_string())
            }
        };
        let (p, d, i, n) = pick_from(&cands, 0, true, &mut probe, &mut |_| {}).unwrap();
        assert!(p.software && d == "soft");
        // past the end, so a window failure on it is final
        assert_eq!((i, n), (cands.len(), cands.len()));
        assert!(tried[..cands.len()].iter().all(|p| !p.software));
    }

    #[test]
    fn nothing_opens_reports_the_last_reason_and_no_fallback_when_off() {
        let cands = candidates(None, false);
        let mut n = 0;
        let mut probe = |_: Pick| {
            n += 1;
            Err(format!("fail {n}"))
        };
        let e = pick_from(&cands, 0, true, &mut probe, &mut |_| {}).unwrap_err();
        assert!(e.starts_with("fail"));
        let mut calls = 0;
        let mut none = |_: Pick| {
            calls += 1;
            Err("x".to_string())
        };
        assert!(pick_from(&cands, 0, false, &mut none, &mut |_| {}).is_err());
        assert_eq!(calls, cands.len());
        // safe start already tried every software step: nothing new to fall back on
        assert!(fallback_after(&recovery_ladder()).is_empty());
    }

    #[test]
    fn lost_flag_keeps_the_first_reason_once() {
        let f = LostFlag::default();
        assert_eq!(f.take(), None);
        f.clone().mark("device lost: reset".into());
        f.mark("second".into());
        assert_eq!(f.take().as_deref(), Some("device lost: reset"));
        assert_eq!(f.take(), None);
    }

    #[test]
    fn env_override_wins_and_safe_is_software() {
        let c = candidates(Some(wgpu::Backends::VULKAN), false);
        assert_eq!(
            c,
            vec![Pick {
                backends: wgpu::Backends::VULKAN,
                software: false
            }]
        );
        assert!(candidates(None, true).iter().all(|p| p.software));
    }
}
