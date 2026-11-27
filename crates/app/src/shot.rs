// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Offscreen UI frame → PNG: the same egui code and paint callbacks as the window, without one.
#[cfg(feature = "gui")]
use crate::gfx::{self, Pick};
#[cfg(not(feature = "gui"))]
use crate::RenderState;
#[cfg(feature = "gui")]
use eframe::egui_wgpu::{self, wgpu, RenderState};
#[cfg(feature = "gui")]
use std::sync::Arc;

#[cfg(feature = "gui")]
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
#[cfg(feature = "gui")]
pub const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub const SAMPLES: u32 = 4;

/// Headless build: no window or GPU, so no render state exists to paint with.
#[cfg(not(feature = "gui"))]
#[allow(clippy::too_many_arguments)]
pub fn frame(
    rs: &RenderState,
    _: &egui::Context,
    _: (u32, u32),
    _: f32,
    _: usize,
    _: Option<(f32, f32)>,
    _: impl FnMut(&egui::Context, usize),
) -> Vec<u8> {
    match *rs {}
}

#[cfg(feature = "gui")]
/// Headless render state shaped like the window's (same formats, MSAA and depth).
pub fn render_state(pick: Pick) -> Result<RenderState, String> {
    render_state_with(pick, wgpu::InstanceFlags::empty())
}

#[cfg(feature = "gui")]
/// As `render_state`, on an instance with `flags` (tests: the API validation layer).
pub fn render_state_with(pick: Pick, flags: wgpu::InstanceFlags) -> Result<RenderState, String> {
    let (adapter, device, queue) = gfx::open_with(pick, flags)?;
    let renderer = egui_wgpu::Renderer::new(&device, FORMAT, Some(DEPTH), SAMPLES, false);
    Ok(RenderState {
        adapter,
        #[cfg(not(target_arch = "wasm32"))]
        available_adapters: Vec::new(),
        device,
        queue,
        target_format: FORMAT,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
    })
}

#[cfg(feature = "gui")]
/// Run `frames` UI passes (layout settles), paint the last one; returns RGBA8 rows.
/// `(w, h)` are physical pixels; `scale` is the display scale factor (points = pixels / scale),
/// fed to egui exactly as the window's native pixels-per-point.
#[allow(clippy::too_many_arguments)]
pub fn frame(
    rs: &RenderState,
    ctx: &egui::Context,
    (w, h): (u32, u32),
    scale: f32,
    frames: usize,
    pointer: Option<(f32, f32)>,
    ui: impl FnMut(&egui::Context, usize),
) -> Vec<u8> {
    let ev = move |_: &egui::Context, _: usize| {
        pointer
            .map(|(x, y)| vec![egui::Event::PointerMoved(egui::pos2(x, y))])
            .unwrap_or_default()
    };
    frame_ev(rs, ctx, (w, h), scale, frames, ev, ui)
}

/// As [`frame`], with the input events of pass `i` from `events(ctx, i)` (clicks, keys).
pub fn frame_ev(
    rs: &RenderState,
    ctx: &egui::Context,
    size: (u32, u32),
    scale: f32,
    frames: usize,
    events: impl FnMut(&egui::Context, usize) -> Vec<egui::Event>,
    ui: impl FnMut(&egui::Context, usize),
) -> Vec<u8> {
    frame_ev_at(rs, ctx, size, scale, frames, 0.0, events, ui)
}

/// [`frame_ev`] with the input clock starting at `t0` seconds: passes of one context keep
/// time moving forward, so separate clicks are not read as a double click.
#[allow(clippy::too_many_arguments)]
pub fn frame_ev_at(
    rs: &RenderState,
    ctx: &egui::Context,
    (w, h): (u32, u32),
    scale: f32,
    frames: usize,
    t0: f64,
    mut events: impl FnMut(&egui::Context, usize) -> Vec<egui::Event>,
    mut ui: impl FnMut(&egui::Context, usize),
) -> Vec<u8> {
    let mut last = None;
    let mut free = Vec::new();
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    for i in 0..frames.max(1) {
        let mut viewports = egui::ViewportIdMap::default();
        viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(scale),
                ..Default::default()
            },
        );
        let input = egui::RawInput {
            viewports,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(w as f32 / scale, h as f32 / scale),
            )),
            max_texture_side: Some(rs.device.limits().max_texture_dimension_2d as usize),
            time: Some(t0 + i as f64 * 0.016),
            events: events(ctx, i),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| ui(ctx, i));
        let mut r = rs.renderer.write();
        for (id, d) in &out.textures_delta.set {
            r.update_texture(&rs.device, &rs.queue, *id, d);
        }
        drop(r);
        let jobs = ctx.tessellate(out.shapes, out.pixels_per_point);
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: out.pixels_per_point,
        };
        free.extend(out.textures_delta.free);
        last = Some((jobs, sd));
    }
    let (jobs, sd) = last.expect("one frame");
    let pixels = paint(rs, (w, h), &jobs, &sd);
    let mut r = rs.renderer.write();
    for id in &free {
        r.free_texture(id);
    }
    pixels
}

#[cfg(feature = "gui")]
fn paint(
    rs: &RenderState,
    (w, h): (u32, u32),
    jobs: &[egui::ClippedPrimitive],
    sd: &egui_wgpu::ScreenDescriptor,
) -> Vec<u8> {
    let dev = &rs.device;
    let tex = |format, samples, usage| {
        dev.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let msaa = tex(FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let depth = tex(DEPTH, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let out = tex(
        FORMAT,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let (mv, dv, ov) = (
        msaa.create_view(&Default::default()),
        depth.create_view(&Default::default()),
        out.create_view(&Default::default()),
    );
    if let Some(r) = rs
        .renderer
        .write()
        .callback_resources
        .get_mut::<ddc_render::Renderer>()
    {
        r.chunks.one_shot(dev);
    }
    let mut enc = dev.create_command_encoder(&Default::default());
    ddc_render::init_resolve_target(&mut enc, &ov);
    let mut bufs = rs
        .renderer
        .write()
        .update_buffers(dev, &rs.queue, &mut enc, jobs, sd);
    {
        let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("shot"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &mv,
                resolve_target: Some(&ov),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &dv,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let mut pass = pass.forget_lifetime();
        rs.renderer.read().render(&mut pass, jobs, sd);
    }
    let row = (w * 4).div_ceil(256) * 256;
    let buf = dev.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        out.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    bufs.push(enc.finish());
    rs.queue.submit(bufs);
    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = dev.poll(wgpu::PollType::Wait);
    let data = slice.get_mapped_range();
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let s = y * row as usize;
        px.extend_from_slice(&data[s..s + w as usize * 4]);
    }
    px
}

/// `--ai-panel-w N`: the AI panel width of the shot, held to the range the setting allows.
pub fn ai_panel_w(argv: &[String]) -> Option<f32> {
    let i = argv.iter().position(|s| s == "--ai-panel-w")?;
    // num-lint: a command-line argument for scripts, never typed into the UI
    let w = argv.get(i + 1)?.parse::<f32>().ok().filter(|w| w.is_finite())?;
    let (lo, hi) = crate::ai::ui::PANEL_W;
    Some(w.clamp(lo, hi))
}

/// `--ai-mode chat|build|review`: the shot opens on that AI tab.
pub fn ai_mode(argv: &[String]) -> Option<crate::ai::ui::modes::AiMode> {
    use crate::ai::ui::modes::AiMode;
    let i = argv.iter().position(|s| s == "--ai-mode")?;
    match argv.get(i + 1)?.as_str() {
        "chat" => Some(AiMode::Chat),
        "build" => Some(AiMode::Build),
        "review" => Some(AiMode::Review),
        _ => None,
    }
}

/// `--fold ribbon=full|tabs|hidden,left=0|1,right=0|1,options=0|1,status=0|1,
/// ai=open|work|preview,clean=0|1`: the chrome folded as given (shots of the folds). Unknown
/// parts are an error.
pub fn apply_fold(app: &mut crate::shell::EditorApp, argv: &[String]) -> Result<(), String> {
    use crate::shell::fold::{AiFold, RibbonMode};
    let Some(spec) = argv
        .iter()
        .position(|s| s == "--fold")
        .and_then(|i| argv.get(i + 1))
    else {
        return Ok(());
    };
    for part in spec.split(',').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').ok_or(format!("--fold: {part}"))?;
        let on = v == "1";
        match k {
            "ribbon" => {
                let m = match v {
                    "full" => RibbonMode::Full,
                    "tabs" => RibbonMode::Tabs,
                    "hidden" => RibbonMode::Hidden,
                    _ => return Err(format!("--fold: {part}")),
                };
                app.set_ribbon_mode(m);
            }
            "left" => app.prefs.browser_open = on,
            "right" => app.prefs.props_open = on,
            "options" => app.prefs.fold.options_bar = on,
            "status" => app.prefs.fold.status_bar = on,
            "ai" => app.prefs.fold.ai = AiFold::parse(v).ok_or(format!("--fold: {part}"))?,
            "clean" => app.chrome.fold.clean = on,
            _ => return Err(format!("--fold: {part}")),
        }
    }
    Ok(())
}

/// `--run CMD` (repeatable): runs the commands in order before the shot.
pub fn run_commands(app: &mut crate::shell::EditorApp, argv: &[String]) -> Result<(), String> {
    for w in argv.windows(2).filter(|w| w[0] == "--run") {
        if !app.run(&w[1]) {
            return Err(format!("--run: {} is not a command", w[1]));
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn ai_mode_arg() {
    use crate::ai::ui::modes::AiMode;
    let a = |s: &[&str]| ai_mode(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    assert_eq!(a(&["x", "--ai-mode", "build"]), Some(AiMode::Build));
    assert_eq!(a(&["x", "--ai-mode", "review"]), Some(AiMode::Review));
    assert_eq!(a(&["x", "--ai-mode", "other"]), None);
    assert_eq!(a(&["x"]), None);
}

#[cfg(test)]
#[test]
fn ai_panel_width_arg() {
    let a = |s: &[&str]| ai_panel_w(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    assert_eq!(a(&["x", "--ai-panel-w", "280"]), Some(280.0));
    assert_eq!(a(&["--ai-panel-w", "10"]), Some(crate::ai::ui::PANEL_W.0));
    assert_eq!(a(&["--ai-panel-w", "NaN"]), None);
    assert_eq!(a(&["--ai-panel-w"]), None);
    assert_eq!(a(&["--lang", "ru"]), None);
}

pub fn save_png(path: &std::path::Path, (w, h): (u32, u32), rgba: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}
