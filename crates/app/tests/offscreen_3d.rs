// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Offscreen 3D frames (`--screenshot --view 3d`, the assistant's 3D renders) on the adapter the
//! app would pick, with the graphics API validation layer on where the system provides one.
//! A multisampled resolve into a never-cleared target is undefined on DX12 and corrupted the
//! process heap in the driver; the layer reports it as an error, which fails this test.

use ddc_app::editor::Editor;
use ddc_app::shell::EditorApp;
use ddc_app::wall_tool::{Event, Mode};
use ddc_geom::v2;
use eframe::egui_wgpu::{wgpu, RenderState};
use std::sync::Mutex;

static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Error records of the graphics stack; the validation layer reports through them.
struct Capture;

impl log::Log for Capture {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Error
    }
    fn log(&self, r: &log::Record) {
        if r.level() == log::Level::Error {
            if let Ok(mut e) = ERRORS.lock() {
                e.push(format!("{}: {}", r.target(), r.args()));
            }
        }
    }
    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

/// The DX12 layer writes through `OutputDebugString`, which raises a first-chance exception per
/// message; a vectored handler reads it and lets it go on. (The graphics stack's own handler
/// decodes the parameter words instead of the text they point to, so nothing reaches `log`.)
#[cfg(windows)]
mod debug_output {
    use super::ERRORS;

    const PRINT_A: u32 = 0x4001_0006;
    const PRINT_W: u32 = 0x4001_000A;
    const CONTINUE_SEARCH: i32 = 0;

    #[repr(C)]
    struct Record {
        code: u32,
        flags: u32,
        next: *mut Record,
        address: *mut core::ffi::c_void,
        params: u32,
        info: [usize; 15],
    }

    #[repr(C)]
    struct Pointers {
        record: *mut Record,
        context: *mut core::ffi::c_void,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn AddVectoredExceptionHandler(
            first: u32,
            handler: unsafe extern "system" fn(*mut Pointers) -> i32,
        ) -> *mut core::ffi::c_void;
    }

    unsafe extern "system" fn handler(p: *mut Pointers) -> i32 {
        // SAFETY: the system passes valid exception pointers; for the two print codes the
        // parameters are (length, pointer) to the message, the length counting the final NUL
        let r = unsafe { &*(*p).record };
        let text = match (r.code, r.params) {
            (PRINT_A, 2..) if r.info[1] != 0 => {
                let s = unsafe { std::slice::from_raw_parts(r.info[1] as *const u8, r.info[0]) };
                String::from_utf8_lossy(s).into_owned()
            }
            (PRINT_W, 2..) if r.info[1] != 0 => {
                let s = unsafe { std::slice::from_raw_parts(r.info[1] as *const u16, r.info[0]) };
                String::from_utf16_lossy(s)
            }
            _ => return CONTINUE_SEARCH,
        };
        let text = text.trim_end_matches(['\0', '\n', '\r']).to_string();
        if text.starts_with("D3D12 ERROR") || text.starts_with("D3D12 CORRUPTION") {
            if let Ok(mut e) = ERRORS.try_lock() {
                // the wide form falls back to the narrow one: one entry per message
                if e.last() != Some(&text) {
                    e.push(text);
                }
            }
        }
        CONTINUE_SEARCH
    }

    pub fn install() {
        // SAFETY: registers a handler that only reads the record and continues the search
        let h = unsafe { AddVectoredExceptionHandler(1, handler) };
        assert!(!h.is_null(), "vectored exception handler");
    }
}

/// A 6 x 4 m room drawn with the rectangle tool, as `--scene room`.
fn room() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.set_mode(Mode::Rect);
    ed.handle(Event::Move(v2(0.0, 0.0)));
    ed.handle(Event::Press(v2(0.0, 0.0)));
    ed.handle(Event::Move(v2(6.0, 4.0)));
    ed.handle(Event::Release(v2(6.0, 4.0)));
    ed.set_mode(Mode::Select);
    assert_eq!(ed.walls.len(), 4);
    ed.status = None;
    ed
}

struct Shot {
    pixels: Vec<u8>,
    samples: Option<u32>,
    stats: ddc_render::FrameStats,
}

fn shot_3d(rs: &RenderState, level: &str, size: (u32, u32)) -> Shot {
    let mut app = EditorApp::new(room(), Some(rs), "offscreen test".into());
    app.prefs.theme = "light".into();
    app.prefs.gfx_level = level.into();
    app.view3d = true;
    let ctx = egui::Context::default();
    let pixels = ddc_app::shot::frame(rs, &ctx, size, 1.0, 8, None, |c, _| app.ui(c, Some(rs)));
    let r = rs.renderer.read();
    let renderer = r.callback_resources.get::<ddc_render::Renderer>();
    let renderer = renderer.expect("the 3D renderer is installed");
    assert!(renderer.health().usable, "{level}: renderer unusable");
    Shot {
        pixels,
        samples: renderer.frame_samples(),
        stats: ddc_render::perf::published(),
    }
}

fn supports_4x(rs: &RenderState) -> bool {
    [ddc_app::shot::FORMAT, ddc_app::shot::DEPTH].iter().all(|f| {
        rs.adapter
            .get_texture_format_features(*f)
            .flags
            .sample_count_supported(4)
    })
}

#[test]
fn offscreen_3d_frames_pass_api_validation() {
    log::set_logger(&CAPTURE).expect("one logger per test binary");
    log::set_max_level(log::LevelFilter::Error);
    #[cfg(windows)]
    debug_output::install();
    let cands = ddc_app::gfx::candidates(None, false);
    let (pick, desc) = ddc_app::gfx::choose(&cands, &mut |_| {}).expect("graphics adapter");
    let rs = ddc_app::shot::render_state_with(pick, wgpu::InstanceFlags::VALIDATION).unwrap();
    eprintln!("adapter: {desc}");
    let size = (960, 640);
    // speed: 1x frame, only the screenshot's own resolve; quality: the 3D frame resolves too
    for (level, want) in [("speed", 1), ("quality", if supports_4x(&rs) { 4 } else { 1 })] {
        let s = shot_3d(&rs, level, size);
        assert_eq!(s.pixels.len(), (size.0 * size.1 * 4) as usize, "{level}");
        assert_eq!(s.samples, Some(want), "{level}: sample count of the 3D frame");
        assert!(s.stats.drawn > 0 && s.stats.triangles > 0, "{level}: {:?}", s.stats);
        // the 3D canvas holds more than the cleared background
        let first = &s.pixels[..4];
        let differ = s.pixels.chunks_exact(4).filter(|p| *p != first).count();
        assert!(differ > (size.0 * size.1 / 10) as usize, "{level}: {differ} pixels drawn");
    }
    let errors = ERRORS.lock().unwrap().clone();
    assert!(errors.is_empty(), "graphics API errors:\n{}", errors.join("\n"));
}
