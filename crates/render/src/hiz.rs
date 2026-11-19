// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Depth snapshot for occlusion culling: 8×8 tiles of the previous frame go to a readback buffer
//! and are read without waiting for the GPU; the snapshot arrives 1–3 frames late (`perf::HiZ`).
//! The pass is a separate command submission at the start of the frame: the frame encoder is
//! submitted by the caller, and the buffer can be mapped only after the copy has been submitted.

use crate::perf::HiZ;
use glam::{Mat4, Vec3};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const TILE: u32 = 8;
const ROW_ALIGN: u32 = 256;
const IDLE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;

struct Target {
    size: (u32, u32),
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    buf: wgpu::Buffer,
    row: u32,
}

/// What the snapshot depends on besides the camera: if it changes, the snapshot is discarded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub clip: [[f32; 4]; 2],
    pub cats: [[f32; 4]; 4],
    pub style: f32,
    pub scene: u64,
}

pub struct HizCapture {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    target: Option<Target>,
    state: Arc<AtomicU8>,
    /// Camera of the in-flight snapshot and its key.
    pending: Option<(Mat4, Vec3, Key)>,
    key: Option<Key>,
    current: Option<HiZ>,
}

/// Tile grid size of a frame of size `size`.
pub fn tiles_of(size: (u32, u32)) -> (u32, u32) {
    (size.0.div_ceil(TILE).max(1), size.1.div_ceil(TILE).max(1))
}

/// Tile rows from the readback buffer bytes (rows are aligned to `row`).
pub fn decode(bytes: &[u8], w: usize, h: usize, row: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let line = &bytes[y * row..y * row + w * 4];
        out.extend(
            line.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        );
    }
    out
}

/// A snapshot may be taken only if the previous frame was drawn with the same key.
pub fn may_capture(prev: Option<&Key>, now: &Key) -> bool {
    prev == Some(now)
}

impl HizCapture {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ddc-hiz"),
            source: wgpu::ShaderSource::Wgsl(include_str!("hiz.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ddc-hiz"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ddc-hiz"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ddc-hiz"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            target: None,
            state: Arc::new(AtomicU8::new(IDLE)),
            pending: None,
            key: None,
            current: None,
        }
    }

    /// Discard the snapshot (culling is off).
    pub fn clear(&mut self) {
        self.current = None;
        self.key = None;
    }

    /// A snapshot usable for frame `vp`.
    pub fn usable(&self, vp: &Mat4) -> Option<&HiZ> {
        self.current.as_ref().filter(|h| h.usable(vp))
    }

    fn ensure_target(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-hiz"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let row = (size.0 * 4).div_ceil(ROW_ALIGN) * ROW_ALIGN;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ddc-hiz"),
            size: u64::from(row) * u64::from(size.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        self.target = Some(Target {
            size,
            tex,
            view,
            buf,
            row,
        });
    }

    /// Per-frame step: collect a finished snapshot and, if the path is free, capture `depth` (the
    /// previous frame with camera `vp`, `eye`). Call before recording the frame into the caller's encoder.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth: &wgpu::TextureView,
        frame: (u32, u32),
        vp: Mat4,
        eye: Vec3,
        key: Key,
    ) {
        let same = may_capture(self.key.as_ref(), &key);
        if !same {
            self.current = None;
            self.key = Some(key);
        }
        let _ = device.poll(wgpu::PollType::Poll);
        if self.state.load(Ordering::Acquire) == MAPPED {
            if let Some(t) = &self.target {
                let (w, h) = (t.size.0 as usize, t.size.1 as usize);
                let tiles = {
                    let view = t.buf.slice(..).get_mapped_range();
                    decode(&view, w, h, t.row as usize)
                };
                t.buf.unmap();
                if let Some((pvp, peye, pkey)) = self.pending.take() {
                    if pkey == key {
                        self.current = Some(HiZ::new(w, h, tiles, pvp, peye));
                    }
                }
            }
            self.state.store(IDLE, Ordering::Release);
        }
        // the previous frame's depth was taken from the old scene: with the new key it would hide too much
        if !same || self.state.load(Ordering::Acquire) != IDLE {
            return;
        }
        self.ensure_target(device, tiles_of(frame));
        let Some(t) = &self.target else {
            return;
        };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ddc-hiz"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth),
            }],
        });
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-hiz"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &t.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(t.row),
                    rows_per_image: Some(t.size.1),
                },
            },
            wgpu::Extent3d {
                width: t.size.0,
                height: t.size.1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        self.state.store(MAPPING, Ordering::Release);
        self.pending = Some((vp, eye, key));
        let state = self.state.clone();
        t.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            state.store(if r.is_ok() { MAPPED } else { IDLE }, Ordering::Release);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_frame() {
        assert_eq!(tiles_of((1280, 720)), (160, 90));
        assert_eq!(tiles_of((1281, 721)), (161, 91));
        assert_eq!(tiles_of((1, 1)), (1, 1));
    }

    #[test]
    fn a_changed_scene_is_not_captured_from_the_old_depth() {
        let k = Key {
            clip: [[0.0; 4]; 2],
            cats: [[1.0; 4]; 4],
            style: 0.0,
            scene: 1,
        };
        let next = Key { scene: 2, ..k };
        assert!(!may_capture(None, &k), "first frame has no previous depth");
        assert!(may_capture(Some(&k), &k));
        assert!(
            !may_capture(Some(&k), &next),
            "the depth still shows the old scene"
        );
        let mut hidden = k;
        hidden.cats[0][1] = 0.0;
        assert!(!may_capture(Some(&k), &hidden), "a hidden category too");
    }

    #[test]
    fn decode_skips_row_padding() {
        let mut b = vec![0u8; 16];
        b[0..4].copy_from_slice(&1.5f32.to_le_bytes());
        b[8..12].copy_from_slice(&2.5f32.to_le_bytes());
        assert_eq!(decode(&b, 1, 2, 8), vec![1.5, 2.5]);
    }
}
