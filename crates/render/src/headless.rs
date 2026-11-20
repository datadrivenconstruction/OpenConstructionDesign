// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Offscreen render without a window: frame timing and pixel readback — for reports and tests.

use crate::camera::OrbitCamera;
use crate::renderer::{FrameUniforms, Quality, Renderer, Targets};
use crate::scene::{Scene, NO_SELECTION};
use web_time::Instant;

#[derive(Debug)]
pub struct Report {
    pub adapter: String,
    pub frames: u32,
    pub avg_ms: f64,
    pub fps: f64,
    /// RGBA of the pixel at `probe` after the last frame.
    pub probe_rgba: [u8; 4],
}

pub const CLEAR: [u8; 4] = [245, 246, 248, 255];

pub fn run(
    scene: &Scene,
    cam: &OrbitCamera,
    size: (u32, u32),
    frames: u32,
    selected: Option<u32>,
    probe: (u32, u32),
) -> Result<Report, String> {
    run_with(scene, cam, size, frames, selected, probe, Quality::High)
}

/// `run` with a given effects quality (frame timing per quality level).
pub fn run_with(
    scene: &Scene,
    cam: &OrbitCamera,
    size: (u32, u32),
    frames: u32,
    selected: Option<u32>,
    probe: (u32, u32),
    quality: Quality,
) -> Result<Report, String> {
    pollster::block_on(run_async(
        scene, cam, size, frames, selected, &[], probe, quality,
    ))
}

/// One frame with a whole selection set (merged ranges, `selset`).
pub fn run_selection(
    scene: &Scene,
    cam: &OrbitCamera,
    size: (u32, u32),
    set: &[(u32, u32)],
    probe: (u32, u32),
) -> Result<Report, String> {
    let first = set.first().map(|r| r.0);
    pollster::block_on(run_async(
        scene,
        cam,
        size,
        1,
        first,
        set,
        probe,
        Quality::High,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_async(
    scene: &Scene,
    cam: &OrbitCamera,
    (w, h): (u32, u32),
    frames: u32,
    selected: Option<u32>,
    set: &[(u32, u32)],
    (px, py): (u32, u32),
    quality: Quality,
) -> Result<Report, String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
        // validation layers of some drivers crash without a window
        flags: wgpu::InstanceFlags::empty(),
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("no adapter: {e}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .map_err(|e| format!("no device: {e}"))?;
    let color_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let depth_format = wgpu::TextureFormat::Depth32Float;
    let tex = |format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = tex(
        color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = tex(depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let (cv, dv) = (
        color.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );
    let mut renderer = Renderer::new(
        &device,
        Targets {
            color: color_format,
            depth: Some(depth_format),
            samples: 1,
        },
        scene,
    );
    renderer.set_adapter(&adapter.get_info());
    renderer.set_selection(std::sync::Arc::from(set.to_vec()));
    // the report and pixel probe must not depend on the pace of background building
    renderer.chunks.build_all(&device);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let aspect = w as f32 / h as f32;
    let clear = wgpu::Color {
        r: srgb_to_linear(CLEAR[0]),
        g: srgb_to_linear(CLEAR[1]),
        b: srgb_to_linear(CLEAR[2]),
        a: 1.0,
    };
    let mut cam = cam.clone();
    let frames = frames.max(1);
    let t0 = Instant::now();
    for f in 0..frames {
        // slight orbit so the frames are not identical
        if f > 0 {
            cam.orbit(0.5, 0.0);
        }
        let mut enc = device.create_command_encoder(&Default::default());
        let u = FrameUniforms::new(&cam, aspect, selected).with_quality(quality);
        renderer.prepare(&device, &queue, &mut enc, &u, (w, h));
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
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
            renderer.draw(&mut pass);
        }
        if f + 1 == frames {
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &color,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: px.min(w - 1),
                        y: py.min(h - 1),
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(256),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit([enc.finish()]);
        device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| format!("poll: {e}"))?;
    }
    let total = t0.elapsed().as_secs_f64();
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait)
        .map_err(|e| format!("poll: {e}"))?;
    let px = {
        let d = slice.get_mapped_range();
        [d[0], d[1], d[2], d[3]]
    };
    readback.unmap();
    Ok(Report {
        adapter: format!(
            "{} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        ),
        frames,
        avg_ms: total * 1000.0 / frames as f64,
        fps: frames as f64 / total,
        probe_rgba: px,
    })
}

/// One finished frame: RGBA8 rows, sRGB-encoded, top row first.
#[derive(Clone, Debug)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// How a frame of `render_rgba` looks beyond the camera.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    /// Element ranges `[start, end)` drawn in the selection colour.
    pub highlight: &'a [(u32, u32)],
    /// Selection colour; `None` keeps the app's.
    pub accent: Option<[u8; 4]>,
    /// Section box `[x0, y0, z0, x1, y1, z1]`: only what lies inside is drawn.
    pub clip: Option<[f32; 6]>,
    pub quality: Quality,
}

impl Default for Look<'_> {
    fn default() -> Self {
        Look {
            highlight: &[],
            accent: None,
            clip: None,
            quality: Quality::High,
        }
    }
}

/// `render_rgba_on` on a device of its own: for callers that have none.
pub fn render_rgba(
    scene: &Scene,
    cam: &OrbitCamera,
    size: (u32, u32),
    look: &Look<'_>,
) -> Result<Image, String> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("no adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| format!("no device: {e}"))?;
        render_rgba_on(&device, &queue, &adapter.get_info(), scene, cam, size, look)
    })
}

/// The scene from `cam` into RGBA pixels on the caller's device: the renderer of the window,
/// no window. Two passes, so effects that settle over a frame have settled.
pub fn render_rgba_on(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    info: &wgpu::AdapterInfo,
    scene: &Scene,
    cam: &OrbitCamera,
    (w, h): (u32, u32),
    look: &Look<'_>,
) -> Result<Image, String> {
    if w == 0 || h == 0 {
        return Err("empty frame".into());
    }
    let color_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let depth_format = wgpu::TextureFormat::Depth32Float;
    let tex = |format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = tex(
        color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = tex(depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let (cv, dv) = (
        color.create_view(&Default::default()),
        depth.create_view(&Default::default()),
    );
    let mut renderer = Renderer::new(
        device,
        Targets {
            color: color_format,
            depth: Some(depth_format),
            samples: 1,
        },
        scene,
    );
    renderer.set_adapter(info);
    renderer.set_selection(std::sync::Arc::from(look.highlight.to_vec()));
    renderer.chunks.build_all(device);
    let row = (w * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: row as u64 * h as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut u = FrameUniforms::new(cam, w as f32 / h as f32, None).with_quality(look.quality);
    match look.highlight {
        [] => {}
        [r] => u.sel = [r.0, r.1, NO_SELECTION, NO_SELECTION],
        set => u = u.with_selection_set(set, 1),
    }
    if let Some(c) = look.accent {
        let l = |v: u8| srgb_to_linear(v) as f32;
        u.accent = [l(c[0]), l(c[1]), l(c[2]), 1.0];
    }
    if let Some(b) = look.clip {
        u.clip_min = [b[0], b[1], b[2], 1.0];
        u.clip_max = [b[3], b[4], b[5], 1.0];
    }
    let clear = wgpu::Color {
        r: srgb_to_linear(CLEAR[0]),
        g: srgb_to_linear(CLEAR[1]),
        b: srgb_to_linear(CLEAR[2]),
        a: 1.0,
    };
    for pass_no in 0..2 {
        let mut enc = device.create_command_encoder(&Default::default());
        renderer.prepare(device, queue, &mut enc, &u, (w, h));
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-offscreen-rgba"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &cv,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
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
            renderer.draw(&mut pass);
        }
        if pass_no == 1 {
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &color,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
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
        }
        queue.submit([enc.finish()]);
        device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| format!("poll: {e}"))?;
    }
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait)
        .map_err(|e| format!("poll: {e}"))?;
    let rgba = {
        let d = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let at = y * row as usize;
            out.extend_from_slice(&d[at..at + (w * 4) as usize]);
        }
        out
    };
    readback.unmap();
    Ok(Image { w, h, rgba })
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Aabb, SceneMesh};
    use glam::Vec3;

    /// Several 3D views on one renderer as the app drives them: every view prepared on its own
    /// slot with a submit of its own, then all composited (as egui paints after all prepares).
    struct Views {
        device: wgpu::Device,
        queue: wgpu::Queue,
        r: Renderer,
        size: (u32, u32),
        targets: Vec<(wgpu::Texture, wgpu::TextureView, wgpu::TextureView)>,
        class: crate::perf::GpuClass,
    }

    impl Views {
        fn new(scene: &Scene, n: usize, size: (u32, u32)) -> Result<Views, String> {
            pollster::block_on(async {
                let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
                    flags: wgpu::InstanceFlags::empty(),
                    ..Default::default()
                });
                // WGPU_ADAPTER_NAME picks an adapter by name (a software one reproduces CI runners)
                let adapter = wgpu::util::initialize_adapter_from_env_or_default(&instance, None)
                    .await
                    .map_err(|e| format!("no adapter: {e}"))?;
                let class = crate::perf::GpuClass::of(adapter.get_info().device_type);
                let (device, queue) = adapter
                    .request_device(&wgpu::DeviceDescriptor::default())
                    .await
                    .map_err(|e| format!("no device: {e}"))?;
                let tex = |format, usage| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                };
                let targets = (0..n)
                    .map(|_| {
                        let c = tex(
                            wgpu::TextureFormat::Rgba8UnormSrgb,
                            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                        );
                        let d = tex(wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::RENDER_ATTACHMENT);
                        let (cv, dv) = (c.create_view(&Default::default()), d.create_view(&Default::default()));
                        (c, cv, dv)
                    })
                    .collect();
                let mut r = Renderer::new(
                    &device,
                    Targets {
                        color: wgpu::TextureFormat::Rgba8UnormSrgb,
                        depth: Some(wgpu::TextureFormat::Depth32Float),
                        samples: 1,
                    },
                    scene,
                );
                r.set_adapter(&adapter.get_info());
                r.chunks.build_all(&device);
                Ok(Views { device, queue, r, size, targets, class })
            })
        }

        /// One app frame: the image of every view, RGBA rows.
        fn frame(&mut self, cams: &[OrbitCamera]) -> Vec<Vec<u8>> {
            let (w, h) = self.size;
            let aspect = w as f32 / h as f32;
            for (i, cam) in cams.iter().enumerate() {
                self.r.select_view(&self.device, i as u32);
                self.r.set_fx(None);
                let u = FrameUniforms::new(cam, aspect, None).with_quality(Quality::High);
                let mut enc = self.device.create_command_encoder(&Default::default());
                self.r.prepare(&self.device, &self.queue, &mut enc, &u, (w, h));
                self.queue.submit([enc.finish()]);
            }
            let bufs: Vec<wgpu::Buffer> = (0..cams.len())
                .map(|_| {
                    self.device.create_buffer(&wgpu::BufferDescriptor {
                        label: None,
                        size: u64::from(w * h * 4),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    })
                })
                .collect();
            let mut enc = self.device.create_command_encoder(&Default::default());
            for (i, (c, cv, dv)) in self.targets.iter().enumerate().take(cams.len()) {
                {
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: cv,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: dv,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                store: wgpu::StoreOp::Discard,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    self.r.draw_view(&mut pass, i as u32);
                }
                enc.copy_texture_to_buffer(
                    c.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &bufs[i],
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 4),
                            rows_per_image: Some(h),
                        },
                    },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
            }
            self.queue.submit([enc.finish()]);
            bufs.iter()
                .map(|b| {
                    let s = b.slice(..);
                    s.map_async(wgpu::MapMode::Read, |_| {});
                    self.device.poll(wgpu::PollType::Wait).unwrap();
                    let v = s.get_mapped_range().to_vec();
                    b.unmap();
                    v
                })
                .collect()
        }
    }

    /// Three quads, elements 1, 3, 5, along x.
    fn three_quads() -> Scene {
        let quad = |element: u32, x: f32| SceneMesh {
            element,
            tag: 0,
            positions: vec![[x - 1., -1., 0.], [x + 1., -1., 0.], [x + 1., 1., 0.], [x - 1., 1., 0.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        Scene::new(vec![quad(1, -3.), quad(3, 0.), quad(5, 3.)])
    }

    /// Camera from above framing x in `lo`..`hi`, world x along the picture columns (yaw -90°:
    /// the default azimuth turned the strip by ~36°, so picture columns did not separate the quads).
    /// The strip fills the 2:1 test frames with a 5% margin; the bounding-sphere fit alone left
    /// each quad at ~170 pixels, too few to measure a dither share.
    fn cam_over(lo: f32, hi: f32) -> OrbitCamera {
        let mut cam = OrbitCamera { pitch: 1.5, yaw: -std::f32::consts::FRAC_PI_2, ..Default::default() };
        cam.fit(&Aabb { min: Vec3::new(lo, -1., 0.), max: Vec3::new(hi, 1., 0.) }, 1.0);
        let (aspect, half_y) = (2.0, 1.0);
        cam.distance = ((hi - lo) * 0.5 / aspect).max(half_y) * 1.05 / (cam.fov_y * 0.5).tan();
        cam
    }

    fn diff(a: &[u8], b: &[u8]) -> u8 {
        a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
    }

    /// Two views with different cameras give different pictures, each the same as that camera
    /// alone on a renderer of its own: no view's targets, uniforms or buffer writes leak.
    #[test]
    fn two_views_keep_their_own_pictures() {
        let s = three_quads();
        let cams = [cam_over(-4., -2.), cam_over(-4., 4.)];
        let rest = crate::perf::LADDER as usize + 3;
        let mut both = match Views::new(&s, 2, (64, 64)) {
            Ok(v) => v,
            Err(e) => return eprintln!("offscreen skipped: {e}"),
        };
        let mut pics = vec![];
        for _ in 0..rest {
            pics = both.frame(&cams);
        }
        assert!(diff(&pics[0], &pics[1]) > 40, "the two cameras show different pictures");
        for (i, cam) in cams.iter().enumerate() {
            let mut one = Views::new(&s, 1, (64, 64)).unwrap();
            let mut alone = vec![];
            for _ in 0..rest {
                alone = one.frame(std::slice::from_ref(cam));
            }
            assert!(diff(&pics[i], &alone[0]) <= 2, "view {i} is its own camera's picture");
        }
    }

    /// At rest no view is drawn again, and turning one camera redraws that view only.
    #[test]
    fn resting_views_are_not_drawn_again() {
        let s = three_quads();
        let mut cams = [cam_over(-4., -2.), cam_over(-4., 4.)];
        let mut v = match Views::new(&s, 2, (64, 64)) {
            Ok(v) => v,
            Err(e) => return eprintln!("offscreen skipped: {e}"),
        };
        for _ in 0..crate::perf::LADDER + 3 {
            v.frame(&cams);
        }
        let n = v.r.redraws();
        let still = v.frame(&cams);
        for _ in 0..5 {
            assert_eq!(v.frame(&cams), still, "the kept frames are shown");
        }
        assert_eq!(v.r.redraws(), n, "no view redrawn at rest");
        cams[1].orbit(4.0, 0.0);
        v.frame(&cams);
        assert_eq!(v.r.redraws(), n + 1, "only the turned view is drawn");
        for _ in 0..crate::perf::LADDER + 3 {
            let before = v.r.redraws();
            v.frame(&cams);
            assert!(v.r.redraws() <= before + 1, "the other view stays at rest");
        }
        let n = v.r.redraws();
        v.frame(&cams);
        assert_eq!(v.r.redraws(), n, "both at rest again");
    }

    #[test]
    fn offscreen_draws_selected_quad() {
        let s = Scene::new(vec![SceneMesh {
            element: 3,
            tag: 0,
            positions: vec![[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        }]);
        let mut cam = OrbitCamera {
            pitch: 1.5,
            ..Default::default()
        };
        cam.fit(
            &Aabb {
                min: Vec3::new(-1., -1., 0.),
                max: Vec3::new(1., 1., 0.),
            },
            1.0,
        );
        match run(&s, &cam, (64, 64), 1, Some(3), (32, 32)) {
            Ok(r) => {
                assert_ne!(r.probe_rgba, CLEAR, "{r:?}");
                assert!(
                    r.probe_rgba[2] > r.probe_rgba[0],
                    "highlight is the (blue) accent: {r:?}"
                );
            }
            Err(e) => eprintln!("offscreen skipped: {e}"),
        }
    }

    #[test]
    fn offscreen_highlights_every_range_of_the_set() {
        // three quads, elements 1, 3, 5; the set {1, 5} skips the middle one
        let quad = |element: u32, x: f32| SceneMesh {
            element,
            tag: 0,
            positions: vec![
                [x - 1., -1., 0.],
                [x + 1., -1., 0.],
                [x + 1., 1., 0.],
                [x - 1., 1., 0.],
            ],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        let s = Scene::new(vec![quad(1, -3.), quad(3, 0.), quad(5, 3.)]);
        let (w, h) = (192u32, 64u32);
        let aspect = w as f32 / h as f32;
        let mut cam = OrbitCamera {
            pitch: 1.5,
            ..Default::default()
        };
        cam.fit(
            &Aabb {
                min: Vec3::new(-4., -1., 0.),
                max: Vec3::new(4., 1., 0.),
            },
            aspect,
        );
        let vp = cam.view_proj(aspect);
        let pixel = |x: f32| {
            let c = vp * Vec3::new(x, 0., 0.).extend(1.0);
            let (nx, ny) = (c.x / c.w, c.y / c.w);
            (
                ((nx * 0.5 + 0.5) * w as f32) as u32,
                ((0.5 - ny * 0.5) * h as f32) as u32,
            )
        };
        let set = crate::selset::merge_ranges(vec![(5, 6), (1, 2)]);
        // lighting differs across the frame, so each quad is compared with its own unselected
        // pixel, not with another quad
        let (mut seen, mut plain) = (vec![], vec![]);
        for x in [-3., 0., 3.] {
            for (sel, out) in [(&set[..], &mut seen), (&[][..], &mut plain)] {
                match run_selection(&s, &cam, (w, h), sel, pixel(x)) {
                    Ok(r) => out.push(r.probe_rgba),
                    Err(e) => {
                        crate::reveal_export::gpu_skipped(&e.to_string());
                        return;
                    }
                }
            }
        }
        // the accent is blue: selected faces lean more to blue than their unselected render
        let blue = |p: [u8; 4]| i32::from(p[2]) - i32::from(p[0]);
        let shift = |i: usize| blue(seen[i]) - blue(plain[i]);
        assert!(shift(0) > 15, "first range: {seen:?} vs {plain:?}");
        assert!(shift(2) > 15, "second range: {seen:?} vs {plain:?}");
        let moved = (0..3).map(|c| (i32::from(seen[1][c]) - i32::from(plain[1][c])).abs());
        assert!(moved.max() <= Some(2), "the quad outside the set stays plain: {seen:?} {plain:?}");
        assert!(seen.iter().chain(&plain).all(|p| *p != CLEAR), "{seen:?} {plain:?}");
    }

    // ---- show-as-built (reveal) ----

    use crate::reveal::{Ease, Effect, ElemAnim, Timeline, Track};

    fn track(a: u32, b: u32, t0: u32, t1: u32, effect: Effect) -> Track {
        Track { elements: (a, b), t0, t1, effect, ease: Ease::Linear }
    }

    /// The picture of one view after the resting ladder, with `tl` sampled at `frame` (none: no
    /// reveal at all). Every picture comes from a renderer of its own, so each goes the same way.
    fn pic_at(s: &Scene, tl: Option<(&Timeline, u32)>) -> Option<Vec<u8>> {
        let mut v = match Views::new(s, 1, (128, 64)) {
            Ok(v) => v,
            Err(e) => {
                crate::reveal_export::gpu_skipped(&e.to_string());
                return None;
            }
        };
        if let Some((tl, f)) = tl {
            v.r.set_reveal(Some((&tl.sample(f)[..], &tl.sample_tint(f)[..])));
            assert!(v.r.reveal_active());
        }
        let cam = cam_over(-4., 4.);
        let mut out = vec![];
        for _ in 0..crate::perf::LADDER as usize + 3 {
            out = v.frame(std::slice::from_ref(&cam)).remove(0);
        }
        Some(out)
    }

    /// After the last frame the picture is byte for byte the static one, though the shader
    /// reads the animation texture for every element (appear, move and tint all ended).
    #[test]
    fn end_frame_equals_static_bytes() {
        let s = three_quads();
        let tl = Timeline::new(
            30,
            vec![
                track(0, 6, 0, 60, Effect::Appear { rise: 2.0 }),
                track(2, 4, 10, 80, Effect::Move { from: Vec3::new(1.0, 0.5, 0.0) }),
                track(0, 6, 20, 100, Effect::Tint { rgb: [200, 40, 40] }),
            ],
        );
        let (Some(plain), Some(end)) = (pic_at(&s, None), pic_at(&s, Some((&tl, tl.len)))) else {
            return;
        };
        assert_eq!(plain, end, "the end frame is the static frame");
        let beyond = pic_at(&s, Some((&tl, tl.len + 500))).unwrap();
        assert_eq!(plain, beyond);
        let start = pic_at(&s, Some((&tl, 0))).unwrap();
        assert!(diff(&plain, &start) > 40, "at frame 0 the quads are not there");
    }

    /// Pixels that differ clearly from `hidden` (the quads are bright against the black clear).
    fn lit(p: &[u8], hidden: &[u8]) -> Vec<bool> {
        p.chunks(4)
            .zip(hidden.chunks(4))
            .map(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(x, y)| x.abs_diff(*y) > 30))
            .collect()
    }

    /// Reference frames t0, t25, t50, t100 of one timeline, derived from the picture itself:
    /// the fade shows as the share of dither pixels (4x4 threshold: exactly alpha), the move as
    /// the centre of the lit pixels walking from the offset to the rest place.
    #[test]
    fn reference_frames_t0_t25_t50_t100() {
        let s = three_quads();
        // element 1 fades in over 100 frames, in place; element 3 slides in from the left
        let tl = Timeline::new(
            30,
            vec![
                track(1, 2, 0, 100, Effect::Appear { rise: 0.0 }),
                track(3, 4, 0, 100, Effect::Move { from: Vec3::new(-1.0, 0.0, 0.0) }),
            ],
        );
        let at = |f| pic_at(&s, Some((&tl, f)));
        let (Some(p0), Some(p25), Some(p50), Some(p100)) = (at(0), at(25), at(50), at(100)) else {
            return;
        };
        // picture column of world x on the view the frame draws (128 x 64)
        let (w, h) = (128usize, 64usize);
        let vp = cam_over(-4., 4.).view_proj(w as f32 / h as f32);
        let col = |x: f32| {
            let c = vp * Vec3::new(x, 0., 0.).extend(1.0);
            ((c.x / c.w * 0.5 + 0.5) * w as f32) as usize
        };
        // element 1 is hidden at t0, so its pixels are where t100 differs from t0 in its columns
        let rest = lit(&p100, &p0);
        let left: Vec<usize> = (0..w * h)
            .filter(|i| rest[*i] && (col(-4.) + 1..col(-2.) - 1).contains(&(i % w)))
            .collect();
        assert!(left.len() > 200, "element 1 covers pixels: {}", left.len());
        let share = |p: &[u8]| {
            let l = lit(p, &p0);
            left.iter().filter(|i| l[**i]).count() as f32 / left.len() as f32
        };
        assert_eq!(share(&p0), 0.0);
        assert!((share(&p25) - 0.25).abs() < 0.06, "t25 share {}", share(&p25));
        assert!((share(&p50) - 0.50).abs() < 0.06, "t50 share {}", share(&p50));
        assert!(share(&p100) > 0.99, "t100 share {}", share(&p100));
        // element 3: mean column of its pixels between elements 1 and 5, found against the
        // picture with element 3 hidden (a brightness cut also takes the light background)
        let gone = Timeline::new(30, vec![track(3, 4, 0, 100, Effect::Appear { rise: 0.0 })]);
        let Some(bg) = pic_at(&s, Some((&gone, 0))) else { return };
        let centre = |p: &[u8]| {
            let l = lit(p, &bg);
            let band: Vec<usize> = (0..w * h).filter(|i| (col(-2.) + 1..col(2.) - 1).contains(&(i % w)) && l[*i]).collect();
            assert!(band.len() > 100, "element 3 covers pixels: {}", band.len());
            band.iter().map(|i| (i % w) as f32).sum::<f32>() / band.len().max(1) as f32
        };
        let (c0, c50, c100) = (centre(&p0), centre(&p50), centre(&p100));
        assert!(c0 < c100 - 3.0, "starts left of its place: {c0} vs {c100}");
        assert!((c50 - (c0 + c100) / 2.0).abs() < 1.5, "halfway at t50: {c0} {c50} {c100}");
        // a frame is repeatable to a bit
        let again = at(50).unwrap();
        assert!(diff(&p50, &again) <= 1);
    }

    /// Without a timeline nothing is allocated or read: the counter is zero, the texture is the
    /// one-row stand-in, and a timeline set and taken off leaves the static picture.
    #[test]
    fn no_timeline_no_extra_binding() {
        let cam = crate::camera::OrbitCamera::default();
        let u = FrameUniforms::new(&cam, 1.0, None).with_selection_set(&[], 0);
        assert_eq!(u.sel_n[3], 0);
        let s = three_quads();
        let Ok(mut v) = Views::new(&s, 1, (128, 64)) else { return };
        assert!(!v.r.reveal_active());
        assert_eq!(v.r.reveal_rows(), 1);
        let plain = pic_at(&s, None).unwrap();
        v.r.set_reveal(Some((&[ElemAnim::REST; 8][..], &[][..])));
        v.r.set_reveal(None);
        assert!(!v.r.reveal_active());
        let cam = cam_over(-4., 4.);
        let mut after = vec![];
        for _ in 0..crate::perf::LADDER as usize + 3 {
            after = v.frame(std::slice::from_ref(&cam)).remove(0);
        }
        assert_eq!(plain, after);
    }

    /// Allowed cost of a timeline frame over a static one, as a factor, per adapter class.
    fn timeline_cost_budget(class: crate::perf::GpuClass) -> f64 {
        match class {
            // software rasterisation: the screen-door reveal measured 2.0x (332 vs 664 ms, min of 5), +25% margin
            crate::perf::GpuClass::Cpu => 2.5,
            crate::perf::GpuClass::Integrated | crate::perf::GpuClass::Discrete => 1.10,
        }
    }

    #[test]
    fn timeline_cost_budget_follows_the_adapter_class() {
        use crate::perf::GpuClass;
        assert_eq!(timeline_cost_budget(GpuClass::of(wgpu::DeviceType::DiscreteGpu)), 1.10);
        assert_eq!(timeline_cost_budget(GpuClass::of(wgpu::DeviceType::IntegratedGpu)), 1.10);
        assert_eq!(timeline_cost_budget(GpuClass::of(wgpu::DeviceType::VirtualGpu)), 1.10);
        assert_eq!(timeline_cost_budget(GpuClass::of(wgpu::DeviceType::Cpu)), 2.5);
    }

    /// A 10k-element scene with a running timeline costs at most a tenth more than the static
    /// one on a GPU; a software adapter has its own budget (`timeline_cost_budget`).
    #[test]
    fn frame_with_timeline_costs_at_most_a_tenth_more() {
        let s = crate::synth::generate(&crate::synth::SynthSpec {
            elements: 10_000,
            ..Default::default()
        });
        let Ok(mut v) = Views::new(&s, 1, (256, 128)) else { return };
        v.r.redraw_at_rest = true;
        let cam = {
            let mut c = OrbitCamera::default();
            c.fit(&s.bounds.unwrap(), 2.0);
            c
        };
        let tl = Timeline::new(
            30,
            (0..10u32)
                .map(|k| track(k * 1000, k * 1000 + 1000, k * 5, k * 5 + 40, Effect::Appear { rise: 3.0 }))
                .collect(),
        );
        let run = |v: &mut Views, n: u32, reveal: bool| {
            let t = Instant::now();
            for f in 0..n {
                if reveal {
                    let k = 20 + f % 40;
                    v.r.set_reveal(Some((&tl.sample(k)[..], &tl.sample_tint(k)[..])));
                }
                v.frame(std::slice::from_ref(&cam));
            }
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        };
        run(&mut v, 8, false);
        run(&mut v, 8, true);
        let (mut plain, mut anim) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            v.r.set_reveal(None);
            plain = plain.min(run(&mut v, 20, false));
            anim = anim.min(run(&mut v, 20, true));
        }
        let k = timeline_cost_budget(v.class);
        eprintln!("{:?} budget {k}: static {plain:.2} ms, with timeline {anim:.2} ms", v.class);
        assert!(anim <= plain * k + 0.5, "{:?}: static {plain:.2} ms, with timeline {anim:.2} ms", v.class);
    }

    /// One quad facing the camera, element 3.
    fn facing_quad() -> (Scene, OrbitCamera) {
        let s = Scene::new(vec![SceneMesh {
            element: 3,
            tag: 0,
            positions: vec![[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        }]);
        let mut cam = OrbitCamera {
            pitch: 1.5,
            ..Default::default()
        };
        cam.fit(
            &Aabb {
                min: Vec3::new(-1., -1., 0.),
                max: Vec3::new(1., 1., 0.),
            },
            1.0,
        );
        (s, cam)
    }

    #[test]
    fn rgba_frame_has_the_size_and_the_highlight_colour() {
        let (s, cam) = facing_quad();
        let plain = match render_rgba(&s, &cam, (96, 64), &Look::default()) {
            Ok(i) => i,
            Err(e) => return eprintln!("offscreen skipped: {e}"),
        };
        assert_eq!((plain.w, plain.h), (96, 64));
        assert_eq!(plain.rgba.len(), 96 * 64 * 4);
        let at = |i: &Image, x: usize, y: usize| {
            let k = (y * i.w as usize + x) * 4;
            [i.rgba[k], i.rgba[k + 1], i.rgba[k + 2]]
        };
        // the corner is background, the middle is the quad
        assert_ne!(at(&plain, 48, 32), at(&plain, 1, 1));
        let red = Look {
            highlight: &[(3, 4)],
            accent: Some([220, 30, 30, 255]),
            ..Look::default()
        };
        let hot = render_rgba(&s, &cam, (96, 64), &red).unwrap();
        let lean = |p: [u8; 3]| i32::from(p[0]) - i32::from(p[2]);
        let shift = lean(at(&hot, 48, 32)) - lean(at(&plain, 48, 32));
        assert!(shift > 15, "highlighted pixel leans to the accent: {shift}");
        assert_eq!(at(&hot, 1, 1), at(&plain, 1, 1), "the background is untouched");
        let outside = Look {
            clip: Some([5., 5., -1., 6., 6., 1.]),
            ..Look::default()
        };
        let cut = render_rgba(&s, &cam, (96, 64), &outside).unwrap();
        // probe between ground grid lines (1 m step): the frame centre is the origin, where the
        // grid's axis lines cross, and the grid is not part of the model the section box cuts
        let c = cam.view_proj(96.0 / 64.0) * Vec3::new(0.5, 0.5, 0.0).extend(1.0);
        let (x, y) = (((c.x / c.w * 0.5 + 0.5) * 96.0) as usize, ((0.5 - c.y / c.w * 0.5) * 64.0) as usize);
        assert_ne!(at(&plain, x, y), at(&plain, 1, 1), "the probe lies on the quad");
        // the ground under the model is shaded by distance from the look-at point, so the
        // background at the probe may differ from the corner's by a unit
        let gone = |p: [u8; 3]| p.iter().zip(at(&cut, 1, 1)).all(|(a, b)| a.abs_diff(b) <= 2);
        assert!(gone(at(&cut, x, y)), "everything outside the section box is gone: {:?} vs {:?}", at(&cut, x, y), at(&cut, 1, 1));
        assert!(!gone(at(&plain, x, y)), "the probe tells the quad from the background");
    }
}
