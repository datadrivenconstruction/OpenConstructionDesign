// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Camera for the construction-sequence reveal and windowless frame export: `follow` keeps the
//! active batch in frame, `render_timeline` yields timeline frames one by one (the caller writes PNGs).

use crate::camera::OrbitCamera;
#[cfg(feature = "gpu")]
use crate::renderer::{FrameUniforms, Quality, Renderer, Targets};
use crate::reveal::{Effect, Timeline};
use crate::scene::{Aabb, Scene};

/// Margin around the batch box.
pub const MARGIN: f32 = 1.15;
/// Fraction of the way to the target per call: the camera moves smoothly, without jerks.
pub const EASE: f32 = 0.18;

/// Camera target for a box: the box centre and the distance at which the box with a 15 % margin fits in frame.
fn goal(b: &Aabb, cam: &OrbitCamera, aspect: f32) -> OrbitCamera {
    let mut g = cam.clone();
    g.fit(b, aspect);
    g.distance *= MARGIN;
    g.scene_radius = g.scene_radius.max(cam.scene_radius);
    g
}

/// Next camera: from `cam` toward a view of `batch_box`. Pure: same input — same camera; angles
/// do not change (orbiting stays with the user), target and distance approach the goal by the
/// fraction `EASE` per call. An empty box leaves the camera as is.
pub fn follow(batch_box: &Aabb, cam: &OrbitCamera, aspect: f32) -> OrbitCamera {
    if batch_box.is_empty() {
        return cam.clone();
    }
    let g = goal(batch_box, cam, aspect);
    let mut c = cam.clone();
    c.target = cam.target.lerp(g.target, EASE);
    c.distance = cam.distance + (g.distance - cam.distance) * EASE;
    c.scene_radius = g.scene_radius;
    c
}

/// A camera step with its section-box verdict.
#[derive(Clone, Debug)]
pub struct Followed {
    pub cam: OrbitCamera,
    /// False: the eye could not be taken out of the section box; `cam` is then the input pose.
    pub clear: bool,
}

/// `follow`, but the eye never enters the section box: inside it the cut hides what the camera
/// should show, so the camera backs off along its view line until it is outside. If that takes
/// more than `MAX_BACKOFF` steps the input pose is kept and `clear` is false (no silent stop).
pub fn follow_clear(batch_box: &Aabb, cam: &OrbitCamera, aspect: f32, section: Option<&Aabb>) -> Followed {
    let mut c = follow(batch_box, cam, aspect);
    let Some(sb) = section.filter(|b| !b.is_empty()) else { return Followed { cam: c, clear: true } };
    let inside = |c: &OrbitCamera| {
        let e = c.eye();
        (0..3).all(|k| e[k] > sb.min[k] && e[k] < sb.max[k])
    };
    for _ in 0..MAX_BACKOFF {
        if !inside(&c) {
            return Followed { cam: c, clear: true };
        }
        c.distance = c.distance * 1.1 + 0.1;
    }
    if inside(&c) {
        Followed { cam: cam.clone(), clear: false }
    } else {
        Followed { cam: c, clear: true }
    }
}

pub const MAX_BACKOFF: usize = 64;

/// Whether the box is fully visible (all corners inside the frame).
pub fn in_frustum(b: &Aabb, cam: &OrbitCamera, aspect: f32) -> bool {
    (0..8).all(|i| {
        let p = glam::Vec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        );
        let n = cam.project(p, aspect);
        n.x.abs() <= 1.0 && n.y.abs() <= 1.0
    })
}

impl Timeline {
    /// Box of the batch active at the frame: union of the mesh boxes of elements on active tracks.
    pub fn active_box(&self, scene: &Scene, frame: u32) -> Aabb {
        let mut out = Aabb::EMPTY;
        for t in self.tracks.iter().filter(|t| t.t0 <= frame && frame <= t.t1) {
            for (m, b) in scene.meshes.iter().zip(scene.boxes()) {
                if (t.elements.0..t.elements.1).contains(&m.element) {
                    out.union(b);
                }
            }
        }
        out
    }

    /// "No animation": every track is instantaneous, the timeline and slider remain.
    pub fn instant(&self) -> Timeline {
        let mut t = self.reduced();
        for tr in &mut t.tracks {
            tr.t1 = tr.t0;
        }
        Timeline::new(t.fps, t.tracks)
    }

    /// "Reduce motion": opacity and tint only, no displacement.
    pub fn reduced(&self) -> Timeline {
        let mut t = self.clone();
        for tr in &mut t.tracks {
            tr.effect = match tr.effect {
                Effect::Appear { .. } => Effect::Appear { rise: 0.0 },
                Effect::Vanish { .. } => Effect::Vanish { sink: 0.0 },
                // a move without displacement — fade in place
                Effect::Move { .. } => Effect::Appear { rise: 0.0 },
                e => e,
            };
        }
        t
    }
}

/// Sub-pixel offset (in pixels, -0.5..0.5) of sample `i`: a low-discrepancy sequence, so any
/// prefix covers the pixel evenly and sample 0 is the unshifted frame.
pub fn jitter_offset(i: u32) -> (f32, f32) {
    if i == 0 {
        return (0.0, 0.0);
    }
    let halton = |mut n: u32, b: u32| {
        let (mut f, mut r) = (1.0f32, 0.0f32);
        while n > 0 {
            f /= b as f32;
            r += f * (n % b) as f32;
            n /= b;
        }
        r
    };
    (halton(i, 2) - 0.5, halton(i, 3) - 0.5)
}

fn srgb_to_lin(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn accumulate(sum: &mut [f32], px: &[u8]) {
    let lut: Vec<f32> = (0..=255u8).map(srgb_to_lin).collect();
    for (i, (s, p)) in sum.iter_mut().zip(px).enumerate() {
        *s += if i % 4 == 3 { *p as f32 / 255.0 } else { lut[*p as usize] };
    }
}

fn resolve(sum: &[f32], n: u32) -> Vec<u8> {
    let k = 1.0 / n as f32;
    sum.iter()
        .enumerate()
        .map(|(i, s)| {
            let v = (s * k).clamp(0.0, 1.0);
            let e = if i % 4 == 3 || v <= 0.003_130_8 { v * if i % 4 == 3 { 1.0 } else { 12.92 } } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
            (e * 255.0 + 0.5) as u8
        })
        .collect()
}

/// Export frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgba8Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

/// Windowless timeline render: `next()` yields frames 0..len in order (frame count = timeline length).
#[cfg(feature = "gpu")]
pub struct TimelineRender<'a> {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    color: wgpu::Texture,
    views: (wgpu::TextureView, wgpu::TextureView),
    tl: &'a Timeline,
    cams: &'a dyn Fn(u32) -> OrbitCamera,
    size: (u32, u32),
    frame: u32,
    aa: u32,
}

#[cfg(feature = "gpu")]
pub fn render_timeline<'a>(
    scene: &Scene,
    tl: &'a Timeline,
    cams: &'a dyn Fn(u32) -> OrbitCamera,
    size: (u32, u32),
) -> Result<TimelineRender<'a>, String> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            flags: wgpu::InstanceFlags::empty(),
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
            .map_err(|e| format!("no adapter: {e}"))?;
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
        let color = tex(
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let depth = tex(wgpu::TextureFormat::Depth32Float, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let views = (color.create_view(&Default::default()), depth.create_view(&Default::default()));
        let mut renderer = Renderer::new(
            &device,
            Targets {
                color: wgpu::TextureFormat::Rgba8UnormSrgb,
                depth: Some(wgpu::TextureFormat::Depth32Float),
                samples: 1,
            },
            scene,
        );
        renderer.chunks.build_all(&device);
        Ok(TimelineRender { device, queue, renderer, color, views, tl, cams, size, frame: 0, aa: 1 })
    })
}

#[cfg(feature = "gpu")]
impl TimelineRender<'_> {
    /// One frame of the timeline, any order (the snapshot path renders a single frame).
    pub fn render_frame(&mut self, f: u32) -> Result<Rgba8Image, String> {
        self.one(f)
    }

    /// Samples per exported frame: each is rendered with a sub-pixel shift of the projection and the
    /// results are averaged in linear light (edges, thin lines and shadow borders stop crawling).
    pub fn set_samples(&mut self, n: u32) {
        self.aa = n.clamp(1, 64);
    }

    fn one(&mut self, f: u32) -> Result<Rgba8Image, String> {
        let (w, h) = self.size;
        if self.aa <= 1 {
            let px = self.shot(f, (0.0, 0.0))?;
            return Ok(Rgba8Image { w, h, px });
        }
        let mut sum = vec![0f32; (w * h * 4) as usize];
        for i in 0..self.aa {
            let px = self.shot(f, jitter_offset(i))?;
            accumulate(&mut sum, &px);
        }
        Ok(Rgba8Image { w, h, px: resolve(&sum, self.aa) })
    }

    fn shot(&mut self, f: u32, jit: (f32, f32)) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        let cam = (self.cams)(f);
        let anim = self.tl.sample(f);
        let tint = self.tl.sample_tint(f);
        self.renderer.set_reveal(Some((&anim, &tint)));
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        let u = FrameUniforms::new(&cam, w as f32 / h as f32, None).with_quality(Quality::High).with_jitter(jit.0, jit.1, (w, h));
        self.renderer.prepare(&self.device, &self.queue, &mut enc, &u, (w, h));
        {
            let c = crate::headless::CLEAR;
            let lin = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-timeline"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.views.0,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: lin(c[0]), g: lin(c[1]), b: lin(c[2]), a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.views.1,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer.draw(&mut pass);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait).map_err(|e| format!("poll: {e}"))?;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        {
            let d = buf.slice(..).get_mapped_range();
            for y in 0..h as usize {
                let o = y * row as usize;
                px.extend_from_slice(&d[o..o + (w * 4) as usize]);
            }
        }
        buf.unmap();
        Ok(px)
    }
}

#[cfg(feature = "gpu")]
impl Iterator for TimelineRender<'_> {
    type Item = Result<Rgba8Image, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.frame >= self.tl.len {
            return None;
        }
        let f = self.frame;
        self.frame += 1;
        Some(self.one(f))
    }
}

/// Test without a GPU adapter: loudly marked SKIP and counted; with `DDC_REQUIRE_GPU=1` — a failure.
#[cfg(all(test, feature = "gpu"))]
pub(crate) fn gpu_skipped(why: &str) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
    eprintln!("SKIP (no GPU, #{n}): {why}");
    assert!(std::env::var_os("DDC_REQUIRE_GPU").is_none(), "GPU required but unavailable: {why}");
}

/// Frame hash for golden references (FNV-1a).
pub fn image_hash(i: &Rgba8Image) -> u64 {
    i.px.iter().fold(0xcbf29ce484222325, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reveal::{Ease, Track};
    use glam::Vec3;

    fn bx(a: [f32; 3], b: [f32; 3]) -> Aabb {
        Aabb { min: Vec3::from(a), max: Vec3::from(b) }
    }

    #[test]
    fn jitter_samples_stay_in_the_pixel_and_differ() {
        assert_eq!(jitter_offset(0), (0.0, 0.0));
        let v: Vec<_> = (0..16).map(jitter_offset).collect();
        assert!(v.iter().all(|(x, y)| x.abs() <= 0.5 && y.abs() <= 0.5));
        for (i, a) in v.iter().enumerate() {
            assert!(v[i + 1..].iter().all(|b| a != b));
        }
        let mean = v.iter().fold((0.0, 0.0), |m, p| (m.0 + p.0 / 16.0, m.1 + p.1 / 16.0));
        assert!(mean.0.abs() < 0.1 && mean.1.abs() < 0.1, "{mean:?}");
    }

    #[test]
    fn accumulation_averages_in_linear_light() {
        let (a, b) = ([0u8, 0, 0, 255], [255u8, 255, 255, 255]);
        let mut s = vec![0f32; 4];
        accumulate(&mut s, &a);
        accumulate(&mut s, &b);
        let o = resolve(&s, 2);
        assert!((o[0] as i32 - 188).abs() <= 1, "{o:?}");
        assert_eq!(o[3], 255);
        let mut s = vec![0f32; 4];
        accumulate(&mut s, &b);
        assert_eq!(resolve(&s, 1), b);
    }

    #[test]
    fn follow_keeps_batch_in_frustum() {
        let aspect = 16.0 / 9.0;
        let b = bx([10.0, 4.0, 0.0], [16.0, 9.0, 3.0]);
        let mut cam = OrbitCamera { target: Vec3::new(-30.0, 0.0, 0.0), distance: 5.0, ..Default::default() };
        for _ in 0..80 {
            cam = follow(&b, &cam, aspect);
        }
        assert!(in_frustum(&b, &cam, aspect), "{cam:?}");
        // already in frame: the camera barely moves
        let again = follow(&b, &cam, aspect);
        assert!((again.target - cam.target).length() < 0.05 && (again.distance - cam.distance).abs() < 0.05);
        // no jerks: one step is shorter than the path to the target
        let far = OrbitCamera { target: Vec3::new(-30.0, 0.0, 0.0), ..Default::default() };
        let step = follow(&b, &far, aspect);
        let to_goal = (Vec3::new(13.0, 6.5, 1.5) - far.target).length();
        assert!((step.target - far.target).length() < to_goal * 0.5);
    }

    #[test]
    fn follow_is_deterministic() {
        let b = bx([0.0; 3], [4.0, 4.0, 3.0]);
        let c = OrbitCamera::default();
        assert_eq!(format!("{:?}", follow(&b, &c, 1.7)), format!("{:?}", follow(&b, &c, 1.7)));
        assert_eq!(format!("{:?}", follow(&Aabb::EMPTY, &c, 1.7)), format!("{c:?}"));
    }

    #[test]
    fn reduced_motion_has_no_offsets() {
        let tr = |effect| Track { elements: (0, 3), t0: 0, t1: 20, effect, ease: Ease::Linear };
        let tl = Timeline::new(
            30,
            vec![
                tr(Effect::Appear { rise: 1.5 }),
                tr(Effect::Vanish { sink: 1.5 }),
                tr(Effect::Move { from: Vec3::new(2.0, 0.0, 0.0) }),
            ],
        );
        assert!((0..=tl.len).any(|f| tl.sample(f).iter().any(|a| a.offset != [0.0; 3])));
        let r = tl.reduced();
        for f in 0..=r.len {
            assert!(r.sample(f).iter().all(|a| a.offset == [0.0; 3]), "frame {f}");
        }
        assert!(r.sample(10).iter().any(|a| a.alpha < 1.0), "transparency is kept");
    }

    #[cfg(feature = "gpu")]
    fn quads() -> Scene {
        use crate::scene::SceneMesh;
        let quad = |element: u32, x: f32| SceneMesh {
            element,
            tag: 0,
            positions: vec![[x - 1., -1., 0.], [x + 1., -1., 0.], [x + 1., 1., 0.], [x - 1., 1., 0.]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        };
        Scene::new(vec![quad(1, -3.), quad(3, 0.), quad(5, 3.)])
    }

    #[cfg(feature = "gpu")]
    fn export(tl: &Timeline) -> Option<Vec<Rgba8Image>> {
        let mut cam = OrbitCamera { pitch: 1.5, ..Default::default() };
        cam.fit(&bx([-4., -1., 0.], [4., 1., 0.]), 1.0);
        let cams = move |_f: u32| cam.clone();
        let r = match render_timeline(&quads(), tl, &cams, (64, 64)) {
            Ok(r) => r,
            Err(e) => return { gpu_skipped(&e); None },
        };
        Some(r.map(|i| i.unwrap()).collect())
    }

    #[cfg(feature = "gpu")]
    fn appear_tl() -> Timeline {
        let tr = |a: u32, b: u32, t0: u32| Track {
            elements: (a, b),
            t0,
            t1: t0 + 10,
            effect: Effect::Appear { rise: 1.0 },
            ease: Ease::Linear,
        };
        Timeline::new(30, vec![tr(1, 2, 0), tr(3, 4, 6), tr(5, 6, 12)])
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn export_frame_count_equals_len() {
        let tl = appear_tl();
        let Some(f) = export(&tl) else { return };
        assert_eq!(f.len() as u32, tl.len);
        assert!(f.iter().all(|i| (i.w, i.h, i.px.len()) == (64, 64, 64 * 64 * 4)));
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn export_frames_match_reference_hashes() {
        let tl = appear_tl();
        let (Some(a), Some(b)) = (export(&tl), export(&tl)) else { return };
        let pick = [0, tl.len as usize / 2, tl.len as usize - 1];
        let h = |v: &Vec<Rgba8Image>| pick.map(|i| image_hash(&v[i]));
        assert_eq!(h(&a), h(&b), "the same input gives the same frames");
        let hs = h(&a);
        assert!(hs[0] != hs[1] && hs[1] != hs[2], "the frames of the scale differ: {hs:?}");
    }

    #[test]
    fn follow_clear_keeps_the_eye_out_of_the_section_box() {
        let b = bx([0.0; 3], [4.0, 4.0, 3.0]);
        let cam = OrbitCamera { target: Vec3::new(2.0, 2.0, 1.0), distance: 3.0, ..Default::default() };
        let plain = follow(&b, &cam, 1.6);
        let section = bx([-50.0; 3], [50.0; 3]);
        let f = follow_clear(&b, &cam, 1.6, Some(&section));
        let e = f.cam.eye();
        assert!(f.clear && (0..3).any(|k| e[k] <= section.min[k] || e[k] >= section.max[k]), "{e:?}");
        assert!(f.cam.distance > plain.distance, "the camera backed off");
        let free = follow_clear(&b, &cam, 1.6, None);
        assert!(free.clear && free.cam.distance == plain.distance && free.cam.target == plain.target);
        // a box too big to leave: reported, input pose kept
        let huge = bx([-1e12; 3], [1e12; 3]);
        let stuck = follow_clear(&b, &cam, 1.6, Some(&huge));
        assert!(!stuck.clear && stuck.cam.distance == cam.distance && stuck.cam.target == cam.target);
    }

    #[test]
    fn instant_timeline_has_no_in_between_frames() {
        let tr = |effect| Track { elements: (0, 3), t0: 6, t1: 26, effect, ease: Ease::Linear };
        let tl = Timeline::new(30, vec![tr(Effect::Appear { rise: 1.5 })]).instant();
        assert!(tl.sample(5).iter().all(|a| a.alpha == 0.0), "before the start the element is absent");
        for f in 6..=tl.len.max(8) {
            assert!(tl.sample(f).iter().all(|a| a.alpha == 1.0 && a.offset == [0.0; 3]), "frame {f}");
        }
    }

    // Needs a GPU: shown as `ignored` (not green) where there is none; run with `--ignored` on a
    // machine with an adapter.
    #[cfg(feature = "gpu")]
    #[test]
    #[ignore = "needs a GPU adapter"]
    fn render_frame_works_in_any_order() {
        let tl = appear_tl();
        let Some(seq) = export(&tl) else { panic!("no GPU adapter: this test needs one") };
        let mut cam = OrbitCamera { pitch: 1.5, ..Default::default() };
        cam.fit(&bx([-4., -1., 0.], [4., 1., 0.]), 1.0);
        let cams = move |_f: u32| cam.clone();
        let mut r = render_timeline(&quads(), &tl, &cams, (64, 64)).unwrap();
        for f in [tl.len - 1, tl.len / 2, 0, tl.len / 2] {
            let img = r.render_frame(f).unwrap();
            assert_eq!(image_hash(&img), image_hash(&seq[f as usize]), "frame {f} out of order");
        }
    }
}
