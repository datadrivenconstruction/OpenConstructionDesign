// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! GPU side of the image textures: one mipmapped 2D array, a sampler and the parameter table, bound
//! as group 2 of the realistic pipeline only. Other styles never touch it.

use crate::anchor::{AnchorSet, ROW_TEXELS};
use crate::texture::{mip_chain, mip_levels, TextureSet, LAYER_PX, MAX_LAYERS};

pub(crate) struct TextureBank {
    pub bgl: wgpu::BindGroupLayout,
    pub bg: wgpu::BindGroup,
    /// Layers on the GPU; 0 means flat colours (nothing loaded, or the device said no).
    pub layers: u32,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    params: wgpu::Buffer,
    /// Element anchors (`anchor.rs`); kept across reloads of the image array.
    anchors: AnchorGpu,
}

struct AnchorGpu {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
}

impl AnchorGpu {
    /// One zero row: no element has an anchor.
    fn none(device: &wgpu::Device) -> Self {
        Self::make(device, None, 1)
    }

    fn make(device: &wgpu::Device, data: Option<(&wgpu::Queue, &[f32])>, rows: u32) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-tex-anchor"),
            size: wgpu::Extent3d {
                width: ROW_TEXELS,
                height: rows,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if let Some((queue, texels)) = data {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(texels),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW_TEXELS * 16),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: ROW_TEXELS,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self { _tex: tex, view }
    }
}

impl TextureBank {
    pub fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ddc-tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        })
    }

    /// Nothing loaded: a one-pixel array that the shader never reads (`layers == 0`).
    pub fn empty(device: &wgpu::Device) -> Self {
        Self::build(device, Self::layout(device), 1, 1, 1, AnchorGpu::none(device))
    }

    fn build(
        device: &wgpu::Device,
        bgl: wgpu::BindGroupLayout,
        px: u32,
        mips: u32,
        layers: u32,
        anchors: AnchorGpu,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-tex"),
            size: wgpu::Extent3d {
                width: px,
                height: px,
                depth_or_array_layers: layers,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ddc-tex"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ddc-tex-params"),
            size: (MAX_LAYERS * 16) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bg = Self::bind(device, &bgl, &view, &sampler, &params, &anchors.view);
        Self {
            bgl,
            bg,
            layers: 0,
            sampler,
            texture,
            view,
            params,
            anchors,
        }
    }

    fn bind(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        params: &wgpu::Buffer,
        anchors: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ddc-tex"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(anchors),
                },
            ],
        })
    }

    /// Replaces the element anchors; the image array stays.
    pub fn set_anchors(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, set: &AnchorSet) {
        let texels = set.texels();
        self.anchors = AnchorGpu::make(device, Some((queue, &texels)), set.rows());
        self.bg = Self::bind(
            device,
            &self.bgl,
            &self.view,
            &self.sampler,
            &self.params,
            &self.anchors.view,
        );
    }

    /// Replaces the array. False when the device refused (memory, limits): the bank is then the
    /// empty one and the style shows flat colours.
    pub fn load(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, set: &TextureSet) -> bool {
        let fit = (device.limits().max_texture_array_layers as usize).min(MAX_LAYERS);
        let n = set.layers.len().min(fit);
        if n == 0 {
            let keep = std::mem::replace(&mut self.anchors, AnchorGpu::none(device));
            *self = Self::build(device, self.bgl.clone(), 1, 1, 1, keep);
            return set.layers.is_empty();
        }
        // layers without pixels: the images are not decoded yet (no realistic view is on screen)
        if set.layers.iter().take(n).any(|l| l.rgba.is_empty()) {
            let keep = std::mem::replace(&mut self.anchors, AnchorGpu::none(device));
            *self = Self::build(device, self.bgl.clone(), 1, 1, 1, keep);
            return true;
        }
        let px = LAYER_PX;
        let mips = mip_levels(px);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let keep = std::mem::replace(&mut self.anchors, AnchorGpu::none(device));
        let mut bank = Self::build(device, self.bgl.clone(), px, mips, n as u32, keep);
        let want = (px as usize) * (px as usize) * 4;
        for (i, l) in set.layers.iter().take(n).enumerate() {
            if l.rgba.len() != want {
                continue;
            }
            for (lvl, data) in mip_chain(&l.rgba, px).iter().enumerate() {
                let side = (px >> lvl).max(1);
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &bank.texture,
                        mip_level: lvl as u32,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: i as u32,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(side * 4),
                        rows_per_image: Some(side),
                    },
                    wgpu::Extent3d {
                        width: side,
                        height: side,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        let mut table = set.params();
        for row in table.iter_mut().skip(n) {
            *row = [0.0; 4];
        }
        queue.write_buffer(&bank.params, 0, bytemuck::cast_slice(&table));
        let invalid = pollster::block_on(device.pop_error_scope());
        let oom = pollster::block_on(device.pop_error_scope());
        if invalid.is_some() || oom.is_some() {
            let keep = std::mem::replace(&mut bank.anchors, AnchorGpu::none(device));
            *self = Self::build(device, bank.bgl.clone(), 1, 1, 1, keep);
            return false;
        }
        bank.layers = n as u32;
        *self = bank;
        true
    }
}
