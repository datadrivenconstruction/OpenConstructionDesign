// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Synthetic model for benchmarks: towers with repeated bodies (instances), a small share of
//! unique meshes, glass and occluder shells. The scene is deterministic: numbers from different
//! runs are comparable. Goals and measurement order — `docs/design/PERF_3D.md`.

use crate::camera::OrbitCamera;
use crate::chunks::{box_mesh, Inst, Instanced};
use crate::graphics::FrameFx;
use crate::perf::FrameStats;
use crate::renderer::{FrameUniforms, Quality, Renderer, Targets};
use crate::scene::{tag_rgb, Aabb, Scene, SceneMesh};
use crate::style::VisualStyle;
use glam::{Mat4, Vec3};
use std::sync::Arc;
use web_time::Instant;

const ROW: usize = 50;
const PER_FLOOR: usize = ROW * ROW;
const FLOORS: usize = 30;
const PER_TOWER: usize = PER_FLOOR * FLOORS;
const FLOOR_H: f32 = 3.5;
const TOWER_STEP: f32 = 70.0;
/// Smaller scenes have no slabs or shells: their count is comparable to the element count.
const SHELL_MIN: usize = 10_000;
const FRAC: f32 = 0.618_034;

#[derive(Clone, Copy, Debug)]
pub struct SynthSpec {
    pub elements: usize,
    pub tris_per_element: usize,
    pub protos: usize,
    /// Share of unique meshes among placed bodies, %.
    pub unique_percent: usize,
    /// Every such prototype is glass (0 — no glass).
    pub glass_every: usize,
}

impl Default for SynthSpec {
    fn default() -> Self {
        Self {
            elements: 200_000,
            tris_per_element: 100,
            protos: 64,
            unique_percent: 5,
            glass_every: 8,
        }
    }
}

/// Sphere built from cube faces: `12·s²` triangles, faces point outward.
fn sphere(r: Vec3, s: u32, at: Vec3, element: u32, tag: u32) -> SceneMesh {
    const FACES: [(Vec3, Vec3, Vec3); 6] = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let s = s.max(1);
    let side = s + 1;
    let mut positions = Vec::with_capacity((6 * side * side) as usize);
    let mut triangles = Vec::with_capacity((12 * s * s) as usize);
    for (f, (n, tu, tv)) in FACES.iter().enumerate() {
        let base = f as u32 * side * side;
        for j in 0..side {
            for i in 0..side {
                let (u, v) = (
                    i as f32 / s as f32 * 2.0 - 1.0,
                    j as f32 / s as f32 * 2.0 - 1.0,
                );
                let p = (*n + *tu * u + *tv * v).normalize() * r + at;
                positions.push([p.x, p.y, p.z]);
            }
        }
        for j in 0..s {
            for i in 0..s {
                let a = base + j * side + i;
                let (b, c, d) = (a + 1, a + side, a + side + 1);
                triangles.push([a, b, c]);
                triangles.push([b, d, c]);
            }
        }
    }
    SceneMesh {
        element,
        tag,
        positions,
        triangles,
    }
}

/// Deviation of a sphere of radius `r` at mesh step `s` per cube face.
fn chord_error(r: f32, s: u32) -> f32 {
    let half = std::f32::consts::FRAC_PI_4 / s.max(1) as f32;
    r * (1.0 - half.cos())
}

fn subdivisions(tris: usize) -> u32 {
    ((tris.max(12) as f32 / 12.0).sqrt().round() as u32).max(1)
}

fn proto_size(p: usize) -> Vec3 {
    let r = 0.25 + 0.05 * (p % 5) as f32;
    Vec3::new(r, r, r * (1 + p % 3) as f32)
}

fn proto_tag(p: usize, spec: &SynthSpec) -> u32 {
    if spec.glass_every > 0 && p % spec.glass_every == spec.glass_every - 1 {
        return tag_rgb([90, 160, 220], true);
    }
    let h = (p as f32 * FRAC).fract();
    tag_rgb(
        [120 + (h * 110.0) as u8, 150, 200 - (h * 90.0) as u8],
        false,
    )
}

fn group_of(p: usize, spec: &SynthSpec, s: u32, inst: Vec<Inst>) -> Instanced {
    let (size, tag) = (proto_size(p), proto_tag(p, spec));
    let reach = size.max_element();
    let mesh = |s: u32| Arc::new(sphere(size, s, Vec3::ZERO, 0, tag));
    let mut lods = vec![];
    let mut last = s;
    for level in [s / 2, s / 4] {
        if level >= 1 && level < last {
            lods.push((mesh(level), chord_error(reach, level)));
            last = level;
        }
    }
    Instanced::new(p as u64 + 1, mesh(s), inst).with_lods(lods)
}

fn place(i: usize, side: usize) -> Vec3 {
    let tower = i / PER_TOWER;
    let floor = (i % PER_TOWER) / PER_FLOOR;
    let cell = i % PER_FLOOR;
    Vec3::new(
        (tower % side) as f32 * TOWER_STEP + (cell % ROW) as f32,
        (tower / side) as f32 * TOWER_STEP + (cell / ROW) as f32,
        floor as f32 * FLOOR_H + 1.2,
    )
}

fn tower_side(towers: usize) -> usize {
    ((towers.max(1) as f32).sqrt().ceil() as usize).max(1)
}

/// Slabs of all towers and solid shells of the even ones: a fair occluder for culling, while the odd
/// towers can be seen through.
fn shells(towers: usize, side: usize, first: u32) -> Vec<SceneMesh> {
    let tag = tag_rgb([190, 190, 185], false);
    let extent = (ROW - 1) as f32;
    let mut out = vec![];
    let mut add = |min: Vec3, max: Vec3| {
        let element = first + out.len() as u32;
        out.push(box_mesh(&Aabb { min, max }, element, tag));
    };
    for t in 0..towers {
        let o = Vec3::new(
            (t % side) as f32 * TOWER_STEP,
            (t / side) as f32 * TOWER_STEP,
            0.0,
        );
        let (lo, hi) = (
            o - Vec3::new(1.0, 1.0, 0.0),
            o + Vec3::new(extent + 1.0, extent + 1.0, 0.0),
        );
        for f in 0..FLOORS {
            let z = f as f32 * FLOOR_H;
            add(Vec3::new(lo.x, lo.y, z), Vec3::new(hi.x, hi.y, z + 0.25));
        }
        if t % 2 == 0 {
            let top = FLOORS as f32 * FLOOR_H;
            add(Vec3::new(lo.x, lo.y, 0.0), Vec3::new(hi.x, lo.y + 0.3, top));
            add(Vec3::new(lo.x, hi.y - 0.3, 0.0), Vec3::new(hi.x, hi.y, top));
            add(Vec3::new(lo.x, lo.y, 0.0), Vec3::new(lo.x + 0.3, hi.y, top));
            add(Vec3::new(hi.x - 0.3, lo.y, 0.0), Vec3::new(hi.x, hi.y, top));
        }
    }
    out
}

fn shell_count(towers: usize) -> usize {
    towers * FLOORS + towers.div_ceil(2) * 4
}

/// Scene of `spec.elements` elements numbered `1..=elements`.
pub fn generate(spec: &SynthSpec) -> Scene {
    let towers = spec.elements.div_ceil(PER_TOWER).max(1);
    let side = tower_side(towers);
    let shell_n = if spec.elements >= SHELL_MIN {
        shell_count(towers)
    } else {
        0
    };
    let placed = spec.elements.saturating_sub(shell_n);
    let s = subdivisions(spec.tris_per_element);
    let protos = spec.protos.max(1);
    let every = match spec.unique_percent.min(100) {
        0 => usize::MAX,
        p => 100 / p,
    };
    let mut rows: Vec<Vec<Inst>> = vec![vec![]; protos];
    let mut meshes = vec![];
    for i in 0..placed {
        let at = place(i, side);
        let element = i as u32 + 1;
        let p = (i.wrapping_mul(2_654_435_761) >> 16) % protos;
        if i % every == 0 {
            meshes.push(Arc::new(sphere(
                proto_size(p),
                s,
                at,
                element,
                proto_tag(p, spec),
            )));
            continue;
        }
        let m = Mat4::from_translation(at)
            * Mat4::from_rotation_z((i % 4) as f32 * std::f32::consts::FRAC_PI_2);
        rows[p].push(Inst { m, element });
    }
    if shell_n > 0 {
        meshes.extend(
            shells(towers, side, placed as u32 + 1)
                .into_iter()
                .map(Arc::new),
        );
    }
    let groups: Vec<Instanced> = rows
        .into_iter()
        .enumerate()
        .filter(|(_, r)| !r.is_empty())
        .map(|(p, r)| group_of(p, spec, s, r))
        .collect();
    Scene::shared(meshes).with_instanced(groups)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Phase {
    pub frames: u32,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    /// CPU part of the frame (cull, uploads, recording, submit); the rest is waiting on the GPU.
    pub cpu_p50_ms: f64,
    pub cpu_p95_ms: f64,
    /// Counters of the last frame of the phase.
    pub last: FrameStats,
}

#[derive(Clone, Debug, Default)]
pub struct BenchReport {
    pub adapter: String,
    /// `vulkan`, `dx12`, `gl`, ...
    pub backend: String,
    /// Instance, adapter and device.
    pub device_ms: f64,
    /// Before the first frame: renderer creation and splitting into chunks.
    pub setup_ms: f64,
    /// The first frame on the GPU, with what it compiles and uploads.
    pub first_frame_ms: f64,
    /// From the first frame to the end of refinement with a still camera.
    pub settle_ms: f64,
    pub settle_frames: u32,
    pub moving: Phase,
    pub still: Phase,
}

fn phase(ms: &mut [f64], last: FrameStats) -> Phase {
    if ms.is_empty() {
        return Phase {
            last,
            ..Phase::default()
        };
    }
    ms.sort_by(f64::total_cmp);
    let n = ms.len();
    Phase {
        frames: n as u32,
        avg_ms: ms.iter().sum::<f64>() / n as f64,
        p50_ms: ms[n / 2],
        p95_ms: ms[(n * 95 / 100).min(n - 1)],
        last,
        ..Phase::default()
    }
}

/// `phase` of whole frames, with the CPU percentiles from `(total, cpu)` samples.
pub(crate) fn phase_split(samples: &[(f64, f64)], last: FrameStats) -> Phase {
    let mut cpu: Vec<f64> = samples.iter().map(|s| s.1).collect();
    let c = phase(&mut cpu, last);
    let mut total: Vec<f64> = samples.iter().map(|s| s.0).collect();
    Phase {
        cpu_p50_ms: c.p50_ms,
        cpu_p95_ms: c.p95_ms,
        ..phase(&mut total, last)
    }
}

pub(crate) const RIG_COLOR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub(crate) const RIG_DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Offscreen device and targets of the benchmarks.
pub(crate) struct Rig {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) info: wgpu::AdapterInfo,
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    pub(crate) size: (u32, u32),
    quality: Quality,
    style: VisualStyle,
}

/// Instance, adapter, device and offscreen targets of `(w, h)`.
pub(crate) async fn rig(
    backends: wgpu::Backends,
    (w, h): (u32, u32),
    quality: Quality,
    style: VisualStyle,
) -> Result<Rig, String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends,
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
    let info = adapter.get_info();
    let view = |format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
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
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let (color, depth) = (view(RIG_COLOR), view(RIG_DEPTH));
    Ok(Rig {
        device,
        queue,
        info,
        color,
        depth,
        size: (w, h),
        quality,
        style,
    })
}

impl Rig {
    pub(crate) fn targets(&self) -> Targets {
        Targets {
            color: RIG_COLOR,
            depth: Some(RIG_DEPTH),
            samples: 1,
        }
    }

    /// A frame to completion on the GPU: the time includes the graphics card's work. `(total, cpu)` ms, cpu up
    /// to the submit.
    pub(crate) fn frame(&self, r: &mut Renderer, cam: &OrbitCamera) -> (f64, f64) {
        let t = Instant::now();
        let aspect = self.size.0 as f32 / self.size.1 as f32;
        let mut enc = self.device.create_command_encoder(&Default::default());
        let u = FrameUniforms::new(cam, aspect, None)
            .with_quality(self.quality)
            .with_style(self.style);
        r.prepare(&self.device, &self.queue, &mut enc, &u, self.size);
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-synth"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.color,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            r.draw(&mut pass);
        }
        self.queue.submit([enc.finish()]);
        let cpu = t.elapsed().as_secs_f64() * 1000.0;
        let _ = self.device.poll(wgpu::PollType::Wait);
        (t.elapsed().as_secs_f64() * 1000.0, cpu)
    }

    /// Still-camera frames until refinement ends: time, frame count and time of the first one.
    fn settle(&self, r: &mut Renderer, cam: &OrbitCamera) -> (f64, u32, f64) {
        const MAX: u32 = 2000;
        let t = Instant::now();
        let mut n = 0;
        let mut first = 0.0;
        while n < MAX {
            let (ms, _) = self.frame(r, cam);
            if n == 0 {
                first = ms;
            }
            n += 1;
            if !r.chunks.frame_stats().refining {
                break;
            }
        }
        (t.elapsed().as_secs_f64() * 1000.0, n, first)
    }
}

/// Three phases: refinement from scratch, camera motion, still camera after refinement.
pub fn bench(
    scene: &Scene,
    size: (u32, u32),
    frames: u32,
    quality: Quality,
) -> Result<BenchReport, String> {
    pollster::block_on(bench_async(
        scene,
        size,
        frames,
        quality,
        VisualStyle::default(),
        None,
        env_backends(),
    ))
}

/// `bench` on the given back ends (the app's own choice, not the wgpu default).
pub fn bench_on(
    scene: &Scene,
    size: (u32, u32),
    frames: u32,
    quality: Quality,
    backends: wgpu::Backends,
) -> Result<BenchReport, String> {
    pollster::block_on(bench_async(
        scene,
        size,
        frames,
        quality,
        VisualStyle::default(),
        None,
        backends,
    ))
}

pub(crate) fn env_backends() -> wgpu::Backends {
    wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY)
}

/// `bench` for one visual style with explicit frame budgets (`--gfx-bench`).
pub fn bench_fx(
    scene: &Scene,
    size: (u32, u32),
    frames: u32,
    style: VisualStyle,
    fx: FrameFx,
) -> Result<BenchReport, String> {
    let quality = if style.effects() {
        Quality::High
    } else {
        Quality::Off
    };
    pollster::block_on(bench_async(
        scene,
        size,
        frames,
        quality,
        style,
        Some(fx),
        env_backends(),
    ))
}

async fn bench_async(
    scene: &Scene,
    (w, h): (u32, u32),
    frames: u32,
    quality: Quality,
    style: VisualStyle,
    fx: Option<FrameFx>,
    backends: wgpu::Backends,
) -> Result<BenchReport, String> {
    let t_dev = Instant::now();
    let rig = rig(backends, (w, h), quality, style).await?;
    let info = rig.info.clone();
    let device_ms = t_dev.elapsed().as_secs_f64() * 1000.0;
    let t0 = Instant::now();
    let mut renderer = Renderer::new(&rig.device, rig.targets(), scene);
    renderer.set_occlusion(&rig.device, info.backend != wgpu::Backend::Gl);
    renderer.set_adapter(&info);
    renderer.redraw_at_rest = true;
    renderer.set_fx(fx);
    let setup_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut cam = OrbitCamera::default();
    if let Some(b) = scene.bounds {
        cam.fit(&b, w as f32 / h as f32);
    }
    let (settle_ms, settle_frames, first_frame_ms) = rig.settle(&mut renderer, &cam);
    let frames = frames.max(1);
    let ms: Vec<(f64, f64)> = (0..frames)
        .map(|_| {
            cam.orbit(0.5, 0.0);
            rig.frame(&mut renderer, &cam)
        })
        .collect();
    let moving = phase_split(&ms, renderer.chunks.frame_stats());
    rig.settle(&mut renderer, &cam);
    let ms: Vec<(f64, f64)> = (0..frames)
        .map(|_| rig.frame(&mut renderer, &cam))
        .collect();
    let still = phase_split(&ms, renderer.chunks.frame_stats());
    Ok(BenchReport {
        adapter: format!("{} ({:?})", info.name, info.backend),
        backend: format!("{:?}", info.backend).to_lowercase(),
        first_frame_ms,
        device_ms,
        setup_ms,
        settle_ms,
        settle_frames,
        moving,
        still,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn spec(elements: usize) -> SynthSpec {
        SynthSpec {
            elements,
            tris_per_element: 48,
            protos: 8,
            ..SynthSpec::default()
        }
    }

    fn elements_of(s: &Scene) -> Vec<u32> {
        let mut v: Vec<u32> = s.meshes.iter().map(|m| m.element).collect();
        for g in &s.instanced {
            v.extend(g.inst.iter().map(|i| i.element));
        }
        v
    }

    #[test]
    fn every_element_once_with_sequential_ids() {
        for n in [10usize, 2_000, 20_000] {
            let scene = generate(&spec(n));
            let ids = elements_of(&scene);
            assert_eq!(ids.len(), n);
            assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), n);
            assert_eq!(ids.iter().copied().min(), Some(1));
            assert_eq!(ids.iter().copied().max(), Some(n as u32));
        }
    }

    #[test]
    fn scene_is_deterministic_and_has_bounds() {
        let (a, b) = (generate(&spec(3000)), generate(&spec(3000)));
        assert_eq!(a.bounds.map(|x| x.max), b.bounds.map(|x| x.max));
        assert_eq!(elements_of(&a), elements_of(&b));
        assert!(a.bounds.is_some_and(|x| !x.is_empty()));
    }

    #[test]
    fn unique_share_and_glass_follow_the_spec() {
        let scene = generate(&spec(2000));
        assert_eq!(scene.meshes.len(), 100, "5 % of the bodies are unique");
        assert!(scene
            .instanced
            .iter()
            .any(|g| crate::scene::is_translucent(g.proto.tag)));
    }

    #[test]
    fn sphere_size_and_outward_faces() {
        let m = sphere(Vec3::ONE, 3, Vec3::ZERO, 1, 0);
        assert_eq!(m.triangles.len(), 12 * 9);
        for t in &m.triangles {
            let [a, b, c] = t.map(|i| Vec3::from(m.positions[i as usize]));
            assert!((b - a).cross(c - a).dot(a + b + c) > 0.0);
        }
    }

    #[test]
    fn lods_get_coarser_and_their_error_grows() {
        let g = group_of(0, &spec(1), subdivisions(1200), vec![]);
        let mut tris = g.proto.triangles.len();
        let mut err = 0.0;
        assert_eq!(g.lods.len(), 2);
        for (m, e) in &g.lods {
            assert!(m.triangles.len() < tris);
            assert!(*e > err);
            tris = m.triangles.len();
            err = *e;
        }
    }

    #[test]
    fn phase_percentile_is_inside_the_samples() {
        let mut ms: Vec<f64> = (1..=100).map(f64::from).collect();
        let p = phase(&mut ms, FrameStats::default());
        assert_eq!(p.frames, 100);
        assert_eq!(p.p95_ms, 96.0);
        assert!((p.avg_ms - 50.5).abs() < 1e-9);
        assert_eq!(phase(&mut [], FrameStats::default()).frames, 0);
    }
}
