// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Short measured fill probe for `Auto`: time to shade a small multisampled target to completion
//! on the GPU, reported per megapixel. Any error gives `None` and `pick_tier` stays conservative.

use web_time::Instant;

const SIDE: u32 = 512;
const ROUNDS: usize = 3;
const OVERDRAW: u32 = 3;
/// A slow adapter stops early: the probe must not delay startup.
const MAX_TOTAL_MS: f32 = 150.0;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

const SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    var c = vec3<f32>(p.xy * 0.01, 0.5);
    for (var k = 0; k < 8; k = k + 1) {
        c = fract(c * 1.37 + vec3<f32>(dot(c, c.yzx)));
    }
    return vec4<f32>(c, 1.0);
}
"#;

/// Milliseconds per megapixel, or `None` if the probe could not run.
pub fn measure(device: &wgpu::Device, queue: &wgpu::Queue, samples: u32) -> Option<f32> {
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ddc-probe"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ddc-probe"),
        layout: None,
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
            targets: &[Some(FORMAT.into())],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState {
            count: samples.max(1),
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
        cache: None,
    });
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ddc-probe"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples.max(1),
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    if pollster::block_on(device.pop_error_scope()).is_some() {
        return None;
    }
    let view = target.create_view(&Default::default());
    let run = || {
        let t = Instant::now();
        let mut enc = device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&pipeline);
            pass.draw(0..3, 0..OVERDRAW);
        }
        queue.submit([enc.finish()]);
        let _ = device.poll(wgpu::PollType::Wait);
        t.elapsed().as_secs_f32() * 1000.0
    };
    // the first round pays for driver shader compilation
    let mut total = run();
    let mut ms = vec![];
    for _ in 0..ROUNDS {
        if total > MAX_TOTAL_MS && !ms.is_empty() {
            break;
        }
        let m = run();
        total += m;
        ms.push(m);
    }
    per_megapixel(&mut ms, SIDE)
}

/// Median round time per megapixel.
fn per_megapixel(ms: &mut [f32], side: u32) -> Option<f32> {
    if ms.is_empty() {
        return None;
    }
    ms.sort_by(f32::total_cmp);
    let mpx = (side * side) as f32 / 1.0e6;
    Some(ms[ms.len() / 2] / mpx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_per_megapixel() {
        assert_eq!(per_megapixel(&mut [], SIDE), None);
        let v = per_megapixel(&mut [9.0, 1.0, 2.0], 1000).unwrap();
        assert!((v - 2.0).abs() < 1e-5);
    }
}
