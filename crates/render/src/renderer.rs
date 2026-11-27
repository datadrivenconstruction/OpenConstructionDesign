// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Frame pipeline: sun shadow map, depth pre-pass, ambient occlusion
//! (half or full resolution), the main MSAA pass into an internal frame (meshes, edges,
//! ground with shadow, glass back to front) and output of the frame into the window pass.
//! `prepare` records commands into internal targets, `draw` only composites the finished frame.

use crate::camera::OrbitCamera;
#[cfg(feature = "gpu")]
use crate::graphics::{AoMode, FrameFx, Health, Limits};
use crate::scene::{Aabb, NO_SELECTION};
#[cfg(feature = "gpu")]
use crate::scene::Scene;
#[cfg(feature = "gpu")]
use crate::vpack::GpuVertex;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};

#[cfg(feature = "gpu")]
/// Target formats of the pass the renderer draws into.
#[derive(Clone, Copy, Debug)]
pub struct Targets {
    pub color: wgpu::TextureFormat,
    pub depth: Option<wgpu::TextureFormat>,
    pub samples: u32,
}

/// 3D effects quality: off, medium (half-resolution occlusion, 1024 shadows),
/// high (full resolution, 2048 shadows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Off,
    Medium,
    High,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FrameUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    /// Sun camera (set by the renderer from the scene bounds).
    pub light_vp: [[f32; 4]; 4],
    pub light_dir: [f32; 4],
    pub eye: [f32; 4],
    /// [start, end) of selected elements and [start, end) of elements under the cursor.
    pub sel: [u32; 4],
    /// Selection colour (linear RGB), shared with the plan.
    pub accent: [f32; 4],
    /// Hover colour (linear RGB), shared with the plan.
    pub hover: [f32; 4],
    /// x: dark theme, y: effects, z: occlusion quality 0/1/2, w: sun shadows.
    pub params: [f32; 4],
    /// Section box: w > 0.5 — enabled.
    pub clip_min: [f32; 4],
    pub clip_max: [f32; 4],
    /// xy: frame size (pixels), z: occlusion frame divisor, w: ground level.
    pub view: [f32; 4],
    /// x: visual style (`VisualStyle::code`), y: face coverage fraction in x-ray,
    /// z > 0.5: section fills are not drawn, w: ortho virtual-eye offset backward.
    pub style: [f32; 4],
    /// Category coverage 0…15 (tag bits 24–27): 1 — as is, 0 — hidden, in between —
    /// screen-door transparency. Category 0 — no rules.
    pub cats: [[f32; 4]; 4],
    /// x: crease edges of unselected chunks, y: silhouette in the composite, w: surface detail
    /// of the realistic style. Set by `Renderer::prepare` from `FrameFx`.
    pub fx: [f32; 4],
    /// Whole selection (`selset`): x ranges in the selection texture (0: `sel.xy` alone
    /// decides), y texels per row, z set generation, w reserved. Set by `Renderer::prepare`.
    pub sel_n: [u32; 4],
    /// x: sway of foliage, m at 10 m (0 keeps it still), y: time, s. Set by `Renderer::prepare`.
    pub wind: [f32; 4],
}

/// Category number in the vertex tag (bits 24–27; 0 — no category).
pub const CAT_SHIFT: u32 = 24;
pub const CAT_MASK: u32 = 0x0F00_0000;

/// Vertex tag with category number `cat` (0…15).
pub fn with_cat(tag: u32, cat: u8) -> u32 {
    (tag & !CAT_MASK) | (u32::from(cat & 15) << CAT_SHIFT)
}

/// Seconds since the first call, wrapped to keep the sine's argument small.
fn wind_time() -> f32 {
    static T0: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    (T0.get_or_init(web_time::Instant::now).elapsed().as_secs_f32()) % 600.0
}

impl FrameUniforms {
    pub fn new(cam: &OrbitCamera, aspect: f32, selected: Option<u32>) -> Self {
        let l = Vec3::new(0.62, 0.3, -0.85).normalize();
        let vp = cam.view_proj(aspect);
        Self {
            view_proj: vp.to_cols_array_2d(),
            inv_view_proj: vp.inverse().to_cols_array_2d(),
            light_vp: Mat4::IDENTITY.to_cols_array_2d(),
            light_dir: [l.x, l.y, l.z, 0.58],
            // xyz: the frame's origin point (in ortho — the virtual eye), w: camera distance
            eye: cam.render_eye().extend(cam.distance.max(1.0)).into(),
            sel: match selected {
                Some(e) => [e, e.saturating_add(1), NO_SELECTION, NO_SELECTION],
                None => [NO_SELECTION; 4],
            },
            accent: [0.05, 0.25, 0.85, 1.0],
            hover: [0.004, 0.27, 0.38, 1.0],
            params: [0.0, 1.0, 2.0, 1.0],
            clip_min: [0.0; 4],
            clip_max: [0.0; 4],
            view: [1.0, 1.0, 1.0, 0.0],
            style: [0.0, 0.45, 0.0, cam.eye_offset()],
            cats: [[1.0; 4]; 4],
            fx: [1.0, 1.0, 0.0, 1.0],
            sel_n: [0, crate::selset::ROW, 0, 0],
            wind: [0.0; 4],
        }
    }
    /// Whole selection as a merged set: two or more ranges replace `sel.xy` by their bounds
    /// (a cheap reject) and the shader searches the set; fewer leave `with_ranges` as is.
    pub fn with_selection_set(mut self, set: &[(u32, u32)], generation: u32) -> Self {
        let n = set.len().min(crate::selset::MAX_RANGES);
        self.sel_n = [0, crate::selset::ROW, generation, 0];
        if n >= 2 {
            if let Some((a, b)) = crate::selset::bounds(&set[..n]) {
                self.sel[0] = a;
                self.sel[1] = b;
                self.sel_n[0] = n as u32;
            }
        }
        self
    }
    /// Category coverage by number (0 is always 1): visibility and transparency from view rules
    /// without rebuilding the scene.
    pub fn with_categories(mut self, cov: &[f32; 16]) -> Self {
        for (k, c) in cov.iter().enumerate() {
            self.cats[k / 4][k % 4] = if k == 0 { 1.0 } else { c.clamp(0.0, 1.0) };
        }
        self
    }
    /// Coverage of category `k` (as in the shader).
    pub fn category_coverage(&self, k: u8) -> f32 {
        let k = usize::from(k & 15);
        self.cats[k / 4][k % 4]
    }
    /// Do not draw section fills (the box is being dragged, the fills are stale).
    pub fn with_caps_hidden(mut self, hidden: bool) -> Self {
        self.style[2] = f32::from(u8::from(hidden));
        self
    }
    /// Visual style of faces (shaded, colours, hidden lines, wireframe, x-ray).
    pub fn with_style(mut self, s: crate::style::VisualStyle) -> Self {
        self.style[0] = s.code();
        self
    }
    /// Theme and effects: dark palette for mesh/edges; effects — high quality or off.
    pub fn with_look(self, dark: bool, effects: bool) -> Self {
        self.with_theme(dark)
            .with_quality(if effects { Quality::High } else { Quality::Off })
    }
    pub fn with_theme(mut self, dark: bool) -> Self {
        self.params[0] = f32::from(u8::from(dark));
        self
    }
    pub fn with_quality(mut self, q: Quality) -> Self {
        let on = f32::from(u8::from(q != Quality::Off));
        self.params[1] = on;
        // medium: sun shadows without ambient occlusion (the main cost on weak GPUs)
        self.params[2] = if q == Quality::High { 2.0 } else { 0.0 };
        self.params[3] = on;
        self
    }
    pub fn quality(&self) -> Quality {
        match (self.params[1] > 0.5, self.params[2] > 0.5) {
            (false, _) => Quality::Off,
            (true, false) => Quality::Medium,
            (true, true) => Quality::High,
        }
    }
    /// Section box: everything outside it is not drawn (and casts no shadow).
    pub fn with_clip(mut self, b: Option<Aabb>) -> Self {
        match b {
            Some(b) => {
                self.clip_min = b.min.extend(1.0).into();
                self.clip_max = b.max.extend(1.0).into();
            }
            None => self.clip_min[3] = 0.0,
        }
        self
    }
    /// Selection and hover highlight by element ranges (curtain wall — mullions and panels).
    pub fn with_ranges(mut self, sel: Option<(u32, u32)>, hover: Option<(u32, u32)>) -> Self {
        let (a, b) = sel.unwrap_or((NO_SELECTION, NO_SELECTION));
        let (c, d) = hover.unwrap_or((NO_SELECTION, NO_SELECTION));
        self.sel = [a, b, c, d];
        self
    }
    /// Selection accent from the theme's sRGB colour.
    pub fn with_accent(mut self, rgb: [u8; 3]) -> Self {
        self.accent = srgb_linear(rgb);
        self
    }
    /// Hover colour from the theme's sRGB colour.
    pub fn with_hover(mut self, rgb: [u8; 3]) -> Self {
        self.hover = srgb_linear(rgb);
        self
    }
}

#[cfg(feature = "gpu")]
/// Group 0 of every pipeline: the frame uniforms and the selection texture.
fn frame_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    sel: &wgpu::TextureView,
    anim: &wgpu::TextureView,
    leaves: &crate::leaves::LeafGpu,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ddc-frame"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(sel),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(anim),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&leaves.view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(&leaves.sampler),
            },
        ],
    })
}

fn srgb_linear(rgb: [u8; 3]) -> [f32; 4] {
    let l = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    [l(rgb[0]), l(rgb[1]), l(rgb[2]), 1.0]
}

#[cfg(feature = "gpu")]
// compressed vertex (`vpack`): 21 bits per axis in u16x4, octahedral normal, element, tag
const VATTR: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Uint16x4, 1 => Snorm16x2, 2 => Uint32, 3 => Uint32];
#[cfg(feature = "gpu")]
const IATTR: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4];
#[cfg(feature = "gpu")]
const FRAME: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
#[cfg(feature = "gpu")]
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
#[cfg(feature = "gpu")]
const AO: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

#[cfg(feature = "gpu")]
/// What a frame's targets were allocated for: equal means no reallocation.
#[derive(Clone, Copy, PartialEq)]
struct FrameKey {
    size: (u32, u32),
    samples: u32,
    ao_div: u32,
    ao_on: bool,
    /// Scaled frames are filtered up to the window; full-size ones stay exact.
    filtered: bool,
    shadow_px: u32,
}

#[cfg(feature = "gpu")]
/// Internal frame targets. Replacing a `Frame` releases the old textures with its views.
struct Frame {
    key: FrameKey,
    depth1: wgpu::TextureView,
    ao: wgpu::TextureView,
    /// Multisampled colour; `None` at 1x, where the main pass draws into `color` directly.
    msaa: Option<wgpu::TextureView>,
    msaa_depth: wgpu::TextureView,
    color: wgpu::TextureView,
    /// `color` is new and has never been cleared (see `init_resolve_target`).
    fresh: bool,
    main_bg: wgpu::BindGroup,
    ssao_bg: wgpu::BindGroup,
    comp_bg: wgpu::BindGroup,
}

#[cfg(feature = "gpu")]
struct Layouts {
    main: wgpu::BindGroupLayout,
    ssao: wgpu::BindGroupLayout,
    comp: wgpu::BindGroupLayout,
    lin: wgpu::Sampler,
    near: wgpu::Sampler,
    cmp: wgpu::Sampler,
}

#[cfg(feature = "gpu")]
/// Main-pass pipelines of one sample count, built when first needed. The realistic one has its
/// own module and may fail alone.
struct MainSet {
    samples: u32,
    mesh: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    lines: wgpu::RenderPipeline,
    ground: wgpu::RenderPipeline,
    real: Option<wgpu::RenderPipeline>,
}

#[cfg(feature = "gpu")]
#[derive(Clone, Copy)]
struct P<'a> {
    layout: &'a wgpu::PipelineLayout,
    module: &'a wgpu::ShaderModule,
    entry: (&'a str, Option<&'a str>),
    mesh: bool,
    topology: wgpu::PrimitiveTopology,
    color: Option<(wgpu::TextureFormat, Option<wgpu::BlendState>)>,
    depth: Option<(wgpu::TextureFormat, bool, wgpu::CompareFunction)>,
    bias: wgpu::DepthBiasState,
    samples: u32,
}

#[cfg(feature = "gpu")]
fn build_pipeline(device: &wgpu::Device, p: P) -> wgpu::RenderPipeline {
    let mesh_buffers = [
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VATTR,
        },
        wgpu::VertexBufferLayout {
            array_stride: 64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &IATTR,
        },
    ];
    let targets = [p.color.map(|(format, blend)| wgpu::ColorTargetState {
        format,
        blend,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(p.entry.0),
        layout: Some(p.layout),
        vertex: wgpu::VertexState {
            module: p.module,
            entry_point: Some(p.entry.0),
            compilation_options: Default::default(),
            buffers: if p.mesh { &mesh_buffers } else { &[] },
        },
        fragment: p.entry.1.map(|e| wgpu::FragmentState {
            module: p.module,
            entry_point: Some(e),
            compilation_options: Default::default(),
            targets: if p.color.is_some() { &targets } else { &[] },
        }),
        primitive: wgpu::PrimitiveState {
            topology: p.topology,
            ..Default::default()
        },
        depth_stencil: p
            .depth
            .map(|(format, write, compare)| wgpu::DepthStencilState {
                format,
                depth_write_enabled: write,
                depth_compare: compare,
                stencil: Default::default(),
                bias: p.bias,
            }),
        multisample: wgpu::MultisampleState {
            count: p.samples,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview: None,
        cache: None,
    })
}

#[cfg(feature = "gpu")]
/// All of `ps` at once, one thread each: the DX12 shader compiler takes ~0.2 s per pipeline on
/// one core. Error scopes belong to the device, not the thread, so one scope covers the batch;
/// `None` if anything in it was rejected (the caller then builds one by one to see what).
fn build_parallel(device: &wgpu::Device, ps: &[P]) -> Option<Vec<wgpu::RenderPipeline>> {
    // no threads in the browser, and GL has one context per device: the caller builds one by one
    if cfg!(target_arch = "wasm32") || is_gl(device) {
        return None;
    }
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out: Vec<wgpu::RenderPipeline> = std::thread::scope(|s| {
        let hs: Vec<_> = ps
            .iter()
            .map(|&p| s.spawn(move || build_pipeline(device, p)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    pollster::block_on(device.pop_error_scope())
        .is_none()
        .then_some(out)
}

#[cfg(feature = "gpu")]
fn is_gl(device: &wgpu::Device) -> bool {
    #[cfg(any(windows, target_os = "linux"))]
    // SAFETY: only asks which backend the device is; the raw device is not used
    return unsafe { device.as_hal::<wgpu::hal::api::Gles, _, _>(|d| d.is_some()) };
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = device;
        false
    }
}

#[cfg(feature = "gpu")]
/// Runs `f` inside an error scope; false means the device rejected what it made.
fn scoped<T>(device: &wgpu::Device, filter: wgpu::ErrorFilter, f: impl FnOnce() -> T) -> (T, bool) {
    device.push_error_scope(filter);
    let v = f();
    let ok = pollster::block_on(device.pop_error_scope()).is_none();
    (v, ok)
}

#[cfg(feature = "gpu")]
// faces sit slightly back so edges on them do not flicker
/// Sample counts of the main sets built with the renderer (`FrameFx::samples` is 1 or 4).
const WARM_SAMPLES: [u32; 2] = [1, 4];

#[cfg(feature = "gpu")]
const FACE_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: -2,
    slope_scale: -1.5,
    clamp: 0.0,
};

#[cfg(feature = "gpu")]
impl MainSet {
    fn build(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        module: &wgpu::ShaderModule,
        samples: u32,
    ) -> Self {
        let ps = Self::descs(layout, module, samples);
        let built = build_parallel(device, &ps)
            .unwrap_or_else(|| ps.iter().map(|&p| build_pipeline(device, p)).collect());
        let [mesh, glass, lines, ground]: [wgpu::RenderPipeline; 4] =
            built.try_into().unwrap_or_else(|_| unreachable!("four descriptors"));
        Self {
            samples,
            mesh,
            glass,
            lines,
            ground,
            real: None,
        }
    }

    /// mesh, glass, lines, ground
    fn descs<'a>(
        layout: &'a wgpu::PipelineLayout,
        module: &'a wgpu::ShaderModule,
        samples: u32,
    ) -> [P<'a>; 4] {
        let tri = wgpu::PrimitiveTopology::TriangleList;
        // reversed depth: nearer is greater
        let less = wgpu::CompareFunction::Greater;
        let less_eq = wgpu::CompareFunction::GreaterEqual;
        let blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        let none = wgpu::DepthBiasState::default();
        let mp = |entry, mesh, topology, color, depth: (bool, wgpu::CompareFunction), bias| P {
            layout,
            module,
            entry,
            mesh,
            topology,
            color: Some((FRAME, color)),
            depth: Some((DEPTH, depth.0, depth.1)),
            bias,
            samples,
        };
        [
            mp(("vs", Some("fs")), true, tri, None, (true, less), FACE_BIAS),
            mp(
                ("vs", Some("fs_glass")),
                true,
                tri,
                blend,
                (false, less),
                none,
            ),
            mp(
                ("vs", Some("fs")),
                true,
                wgpu::PrimitiveTopology::LineList,
                blend,
                (true, less_eq),
                none,
            ),
            mp(
                ("vs_ground", Some("fs_ground")),
                false,
                tri,
                blend,
                (false, less_eq),
                none,
            ),
        ]
    }
}

#[cfg(feature = "gpu")]
pub struct Renderer {
    sets: Vec<MainSet>,
    failed_samples: Vec<u32>,
    shader: wgpu::ShaderModule,
    main_layout: wgpu::PipelineLayout,
    /// The main layout plus the texture group; only the realistic pipeline uses it.
    real_layout: wgpu::PipelineLayout,
    tex: crate::texture_gpu::TextureBank,
    tex_seen: u64,
    anchor_seen: u64,
    real_module: Option<wgpu::ShaderModule>,
    limits: Limits,
    health: Health,
    /// Budgets of the next frames; `None` derives them from `FrameUniforms::quality`.
    fx: Option<FrameFx>,
    /// Lives apart from `Frame`: a scale step must not redraw the shadow map.
    shadow_map: Option<(u32, wgpu::TextureView)>,
    shadow_pipeline: wgpu::RenderPipeline,
    prepass_pipeline: wgpu::RenderPipeline,
    prepass_ground: wgpu::RenderPipeline,
    ssao_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    layouts: Layouts,
    frame: Option<Frame>,
    /// Sun shadows are drawn (by the quality of the last frame).
    pub shadow: bool,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Group 0 layout: re-creating the selection texture rebuilds `bind_group` with it.
    frame_bgl: wgpu::BindGroupLayout,
    /// The whole selection (`selset`), uploaded on change.
    sel: crate::selset::SelGpu,
    /// Show-as-built animation state (`reveal`); idle while no timeline is set.
    anim: crate::reveal::AnimGpu,
    /// Leaf card pictures (`leaves`), uploaded with the first frame.
    leaves: crate::leaves::LeafGpu,
    reveal_rev: u64,
    /// Meshes in chunks by cell: partial upload, culling, level of detail (`chunks`).
    pub chunks: crate::chunks::Chunks,
    bounds: Option<Aabb>,
    /// Occlusion culling (enabled by the app), previous frame's camera, shadow-map key.
    hiz: Option<crate::hiz::HizCapture>,
    last_cam: Option<(Mat4, Vec3)>,
    shadow_key: Option<ShadowKey>,
    /// What the last frame drew: uniforms (camera, style, section box), size, chunk generation
    /// and uploads, selection generation.
    rest_key: Option<RestKey>,
    /// Frames actually drawn (the resting ones skipped are not counted).
    redraws: u64,
    /// Benchmark: an idle frame is redrawn, otherwise the idle phase would measure empty frames.
    pub redraw_at_rest: bool,
    /// The 3D view the fields above belong to; the other views wait in `parked`.
    view: u32,
    parked: std::collections::HashMap<u32, ViewState>,
    /// Bumped when the selection texture is re-created; `bg_epoch` is that of `bind_group`.
    sel_epoch: u64,
    bg_epoch: u64,
}

#[cfg(feature = "gpu")]
/// Per-view state of a 3D view that is not the one `prepare` works on now.
struct ViewState {
    frame: Option<Frame>,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bg_epoch: u64,
    hiz: Option<crate::hiz::HizCapture>,
    last_cam: Option<(Mat4, Vec3)>,
    rest_key: Option<RestKey>,
    motion: crate::perf::Motion,
    fx: Option<FrameFx>,
    /// The frame's bind groups hold this view's own shadow map.
    shadow_map: Option<(u32, wgpu::TextureView)>,
    shadow_key: Option<ShadowKey>,
}

#[cfg(feature = "gpu")]
// uniforms, size, chunk generation, upload epoch, selection-set generation
type RestKey = (Vec<u8>, (u32, u32), u64, u64, u32);

#[cfg(feature = "gpu")]
fn uniform_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ddc-frame"),
        size: std::mem::size_of::<FrameUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

#[cfg(feature = "gpu")]
/// What the shadow map depends on: if unchanged, the frame does not redraw it.
#[derive(Clone, Copy, PartialEq)]
struct ShadowKey {
    light_vp: [[f32; 4]; 4],
    clip: [[f32; 4]; 2],
    cats: [[f32; 4]; 4],
    style: [f32; 4],
    generation: u64,
    epoch: u64,
}

#[cfg(feature = "gpu")]
/// The depth snapshot is not always usable: section, x-ray, wireframe and semi-transparent categories
/// show what the depth considers occluded; underground the ground itself occludes.
fn occlusion_allowed(u: &FrameUniforms, ground: f32) -> bool {
    u.clip_min[3] <= 0.5
        && u.style[0] <= 2.5
        && u.eye[2] > ground
        && u.cats.iter().flatten().all(|&c| c <= 0.0 || c >= 1.0)
}

#[cfg(feature = "gpu")]
impl Renderer {
    pub fn new(device: &wgpu::Device, t: Targets, scene: &Scene) -> Self {
        // a failed module or pipeline must not reach the device's fatal error handler
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let common = include_str!("common.wgsl");
        let module = |label, body: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{common}\n{body}").into()),
            })
        };
        let shader = module("ddc-mesh", include_str!("mesh.wgsl"));
        let post = module("ddc-post", include_str!("post.wgsl"));
        let frag = wgpu::ShaderStages::FRAGMENT;
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: frag,
            ty,
            count: None,
        };
        let tex = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let depth_tex = tex(wgpu::TextureSampleType::Depth);
        // the fullscreen passes load depth texels, which GL allows only from a float binding
        let depth_load = tex(wgpu::TextureSampleType::Float { filterable: false });
        let color_tex = tex(wgpu::TextureSampleType::Float { filterable: true });
        let filt = wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering);
        let bgl = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let frame_bgl = bgl(
            "ddc-frame",
            &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                entry(1, tex(wgpu::TextureSampleType::Uint)),
                // show-as-built animation: read by the vertex stage (offset) and fragments
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: tex(wgpu::TextureSampleType::Float { filterable: false }),
                    count: None,
                },
                // leaf card pictures: cut-out in the depth passes too
                entry(
                    3,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                entry(4, filt),
            ],
        );
        let main_bgl = bgl(
            "ddc-main",
            &[
                entry(0, depth_tex),
                entry(1, color_tex),
                entry(2, filt),
                entry(3, depth_tex),
                entry(
                    4,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                ),
            ],
        );
        let ssao_bgl = bgl("ddc-ssao", &[entry(0, depth_load)]);
        let comp_bgl = bgl(
            "ddc-comp",
            &[entry(0, depth_load), entry(1, color_tex), entry(2, filt)],
        );
        let sampler = |filter, compare| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: filter,
                min_filter: filter,
                compare,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            })
        };
        let layouts = Layouts {
            lin: sampler(wgpu::FilterMode::Linear, None),
            near: sampler(wgpu::FilterMode::Nearest, None),
            cmp: sampler(
                wgpu::FilterMode::Linear,
                Some(wgpu::CompareFunction::GreaterEqual),
            ),
            main: main_bgl,
            ssao: ssao_bgl,
            comp: comp_bgl,
        };
        let uniforms = uniform_buffer(device);
        let sel = crate::selset::SelGpu::new(device);
        let anim = crate::reveal::AnimGpu::new(device);
        let leaves = crate::leaves::LeafGpu::new(device);
        let bind_group = frame_group(device, &frame_bgl, &uniforms, &sel.view, &anim.view, &leaves);
        let pl = |groups: &[&wgpu::BindGroupLayout]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: groups,
                push_constant_ranges: &[],
            })
        };
        let depth_only = pl(&[&frame_bgl]);
        let main_layout = pl(&[&frame_bgl, &layouts.main]);
        let tex = crate::texture_gpu::TextureBank::empty(device);
        let real_layout = pl(&[&frame_bgl, &layouts.main, &tex.bgl]);
        let ssao_layout = pl(&[&frame_bgl, &layouts.ssao]);
        let comp_layout = pl(&[&frame_bgl, &layouts.comp]);
        let tri = wgpu::PrimitiveTopology::TriangleList;
        // reversed depth: nearer is greater
        let less = wgpu::CompareFunction::Greater;
        let shadow_p = P {
            layout: &depth_only,
            module: &shader,
            entry: ("vs_light", Some("fs_clip")),
            mesh: true,
            topology: tri,
            color: None,
            depth: Some((DEPTH, true, less)),
            bias: wgpu::DepthBiasState {
                constant: -3,
                slope_scale: -2.5,
                clamp: 0.0,
            },
            samples: 1,
        };
        let prepass_p = P {
            layout: &depth_only,
            module: &shader,
            entry: ("vs", Some("fs_clip")),
            mesh: true,
            topology: tri,
            color: None,
            depth: Some((DEPTH, true, less)),
            bias: Default::default(),
            samples: 1,
        };
        let ground_p = P {
            entry: ("vs_ground", None),
            mesh: false,
            ..prepass_p
        };
        let ssao_p = P {
            layout: &ssao_layout,
            module: &post,
            entry: ("vs_full", Some("fs_ssao")),
            mesh: false,
            topology: tri,
            color: Some((AO, None)),
            depth: None,
            bias: Default::default(),
            samples: 1,
        };
        let comp_p = P {
            layout: &comp_layout,
            module: &post,
            entry: ("vs_full", Some("fs_composite")),
            mesh: false,
            topology: tri,
            color: Some((
                t.color,
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            )),
            depth: t.depth.map(|f| (f, false, wgpu::CompareFunction::Always)),
            bias: Default::default(),
            samples: t.samples.max(1),
        };
        // the first frame needs the main sets of 1 and (at medium quality and up) 4 samples:
        // compiled here with the rest, side by side, instead of one by one on that frame
        let mut batch = vec![shadow_p, prepass_p, ground_p, ssao_p, comp_p];
        for s in WARM_SAMPLES {
            batch.extend(MainSet::descs(&main_layout, &shader, s));
        }
        let mut warm_sets = vec![];
        let (shadow_pipeline, prepass_pipeline, prepass_ground, ssao_pipeline, composite_pipeline);
        let (shadow_ok, ssao_ok);
        if let Some(v) = build_parallel(device, &batch) {
            let mut it = v.into_iter();
            let mut next = || it.next().unwrap_or_else(|| unreachable!("one per descriptor"));
            (shadow_pipeline, prepass_pipeline, prepass_ground) = (next(), next(), next());
            (ssao_pipeline, composite_pipeline) = (next(), next());
            for samples in WARM_SAMPLES {
                warm_sets.push(MainSet {
                    samples,
                    mesh: next(),
                    glass: next(),
                    lines: next(),
                    ground: next(),
                    real: None,
                });
            }
            (shadow_ok, ssao_ok) = (true, true);
        } else {
            // something was rejected: one by one, so a failed effect turns off only itself
            let one = |p| scoped(device, wgpu::ErrorFilter::Validation, || build_pipeline(device, p));
            let (sp, sok) = one(shadow_p);
            let (ap, aok) = one(ssao_p);
            (shadow_pipeline, ssao_pipeline, shadow_ok, ssao_ok) = (sp, ap, sok, aok);
            prepass_pipeline = build_pipeline(device, prepass_p);
            prepass_ground = build_pipeline(device, ground_p);
            composite_pipeline = build_pipeline(device, comp_p);
        }
        let base_ok = pollster::block_on(device.pop_error_scope()).is_none();
        let mut limits = Limits::default();
        if !shadow_ok {
            limits.max_shadow = 0;
        }
        if !ssao_ok {
            limits.ao = false;
        }
        let health = Health {
            usable: base_ok,
            effects_failed: !shadow_ok || !ssao_ok,
            ..Health::default()
        };
        let mut chunks = crate::chunks::Chunks::default();
        chunks.sync(device, scene);
        Self {
            sets: warm_sets,
            failed_samples: vec![],
            shader,
            main_layout,
            real_layout,
            tex,
            tex_seen: 0,
            anchor_seen: 0,
            real_module: None,
            limits,
            health,
            fx: None,
            shadow_map: None,
            shadow_pipeline,
            prepass_pipeline,
            prepass_ground,
            ssao_pipeline,
            composite_pipeline,
            layouts,
            frame: None,
            shadow: true,
            uniforms,
            bind_group,
            frame_bgl,
            sel,
            chunks,
            bounds: scene.bounds,
            hiz: None,
            last_cam: None,
            shadow_key: None,
            rest_key: None,
            redraws: 0,
            redraw_at_rest: false,
            view: 0,
            parked: Default::default(),
            anim,
            leaves,
            reveal_rev: 0,
            sel_epoch: 0,
            bg_epoch: 0,
        }
    }

    /// Makes `slot` the view the next `prepare` draws: each 3D view keeps its own targets,
    /// uniforms, occlusion snapshot and rest frame, so several views share one frame.
    pub fn select_view(&mut self, device: &wgpu::Device, slot: u32) {
        if slot == self.view {
            return;
        }
        let next = match self.parked.remove(&slot) {
            Some(s) => s,
            None => {
                let uniforms = uniform_buffer(device);
                let bind_group = frame_group(
                    device,
                    &self.frame_bgl,
                    &uniforms,
                    &self.sel.view,
                    &self.anim.view,
                    &self.leaves,
                );
                ViewState {
                    frame: None,
                    uniforms,
                    bind_group,
                    bg_epoch: self.sel_epoch,
                    hiz: self.hiz.is_some().then(|| crate::hiz::HizCapture::new(device)),
                    last_cam: None,
                    rest_key: None,
                    motion: Default::default(),
                    fx: self.fx,
                    shadow_map: None,
                    shadow_key: None,
                }
            }
        };
        let mut motion = next.motion;
        self.chunks.swap_motion(&mut motion);
        let old = ViewState {
            frame: self.frame.take(),
            uniforms: std::mem::replace(&mut self.uniforms, next.uniforms),
            bind_group: std::mem::replace(&mut self.bind_group, next.bind_group),
            bg_epoch: self.bg_epoch,
            hiz: std::mem::replace(&mut self.hiz, next.hiz),
            last_cam: self.last_cam.take(),
            rest_key: self.rest_key.take(),
            motion,
            fx: std::mem::replace(&mut self.fx, next.fx),
            shadow_map: std::mem::replace(&mut self.shadow_map, next.shadow_map),
            shadow_key: std::mem::replace(&mut self.shadow_key, next.shadow_key),
        };
        self.parked.insert(self.view, old);
        self.frame = next.frame;
        self.last_cam = next.last_cam;
        self.rest_key = next.rest_key;
        self.bg_epoch = next.bg_epoch;
        self.view = slot;
        // the selection texture was re-created while this view was parked
        if self.bg_epoch != self.sel_epoch {
            self.bind_group = self.frame_group_now(device);
            self.bg_epoch = self.sel_epoch;
        }
    }

    /// Group 0 of the current view from the current uniforms, selection and animation textures.
    fn frame_group_now(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        frame_group(device, &self.frame_bgl, &self.uniforms, &self.sel.view, &self.anim.view, &self.leaves)
    }

    /// One frame of the show-as-built animation (`Timeline::sample`, `sample_tint`); `None`
    /// ends it, and the shaders go back to reading nothing. A changed frame redraws a resting
    /// view because the frame uniforms carry its count and counter.
    pub fn set_reveal(&mut self, frame: Option<(&[crate::reveal::ElemAnim], &[crate::reveal::ElemTint])>) {
        self.anim.state.set(frame);
    }

    /// `set_reveal` for a caller that offers the same frame on every repaint and every view:
    /// a frame with the revision already shown is skipped (0 with `None` ends the animation).
    pub fn set_reveal_rev(
        &mut self,
        rev: u64,
        frame: Option<(&[crate::reveal::ElemAnim], &[crate::reveal::ElemTint])>,
    ) {
        if self.reveal_rev == rev {
            return;
        }
        self.reveal_rev = rev;
        self.anim.state.set(frame);
    }

    /// Rows of the animation texture; 1 is the idle stand-in.
    pub fn reveal_rows(&self) -> u32 {
        self.anim.rows()
    }

    /// The animation is on: its texture is read by the shaders.
    pub fn reveal_active(&self) -> bool {
        self.anim.state.count() > 0
    }

    /// Every view draws its next frame again (new scene, look or textures).
    fn unrest(&mut self) {
        self.rest_key = None;
        for v in self.parked.values_mut() {
            v.rest_key = None;
        }
    }

    /// Drops the targets of a 3D view that is gone.
    pub fn forget_view(&mut self, slot: u32) {
        self.parked.remove(&slot);
    }

    /// The view `prepare` drew last.
    pub fn current_view(&self) -> u32 {
        self.view
    }

    /// Frames drawn so far, all views together; a resting view adds none.
    pub fn redraws(&self) -> u64 {
        self.redraws
    }

    /// 3D views with targets of their own (the current one and the parked ones).
    pub fn view_count(&self) -> usize {
        1 + self.parked.len()
    }

    /// Composites view `slot` prepared this frame.
    pub fn draw_view(&self, pass: &mut wgpu::RenderPass<'_>, slot: u32) {
        if slot == self.view {
            return self.draw(pass);
        }
        if !self.health.usable {
            return;
        }
        let Some(v) = self.parked.get(&slot) else {
            return;
        };
        let Some(f) = v.frame.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &v.bind_group, &[]);
        pass.set_bind_group(1, &f.comp_bg, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Occlusion culling from the depth snapshot of earlier frames (needs non-blocking buffer
    /// readback: not for GL).
    pub fn set_occlusion(&mut self, device: &wgpu::Device, on: bool) {
        self.chunks.cfg.occlusion = on;
        if on && self.hiz.is_none() {
            self.hiz = Some(crate::hiz::HizCapture::new(device));
        }
    }

    /// Whole selection as merged element ranges (`selset::merge_ranges`); uploaded by the next
    /// `prepare` only when it differs from the current set.
    pub fn set_selection(&mut self, ranges: std::sync::Arc<[(u32, u32)]>) {
        self.sel.set(ranges);
    }

    /// Budgets of the next frames; applies at once, without touching geometry.
    pub fn set_fx(&mut self, fx: Option<FrameFx>) {
        if self.fx != fx {
            self.fx = fx;
            // the resting shortcut compares uniforms only: a new look must be drawn (the look
            // is the current view's own, the other views keep theirs)
            self.rest_key = None;
        }
    }

    /// Largest sample count the adapter supports for the frame's formats.
    pub fn set_max_samples(&mut self, n: u32) {
        self.limits.max_samples = self.limits.max_samples.min(n.max(1));
    }

    pub fn health(&self) -> Health {
        self.health
    }

    /// Sample count of the current frame targets; `None` before the first frame.
    pub fn frame_samples(&self) -> Option<u32> {
        self.frame.as_ref().map(|f| f.key.samples)
    }

    /// New scene: only chunks with changed content are uploaded to the GPU.
    pub fn set_scene(&mut self, device: &wgpu::Device, scene: &Scene) {
        self.chunks.sync(device, scene);
        self.bounds = scene.bounds;
        self.unrest();
    }

    /// Depth snapshot of a previous frame usable for a camera `vp` (city layer refinement).
    pub fn occlusion_snapshot(&self, vp: &Mat4) -> Option<&crate::perf::HiZ> {
        self.hiz.as_ref().and_then(|h| h.usable(vp))
    }

    /// Frame and memory limits for the actual adapter.
    pub fn set_adapter(&mut self, info: &wgpu::AdapterInfo) {
        self.chunks
            .set_gpu_class(crate::perf::GpuClass::of(info.device_type));
        // GL takes flat values from the last vertex of a triangle, the others — from the first
        self.chunks
            .set_provoke_last(info.backend == wgpu::Backend::Gl);
    }

    /// Scene instances: each matrix draws the whole mesh once more (the basis for instancing).
    /// Err with a reason when nothing was uploaded (previous instances stay). Matrices past
    /// the device buffer limit are dropped with a logged reason.
    pub fn set_instances(&mut self, device: &wgpu::Device, m: &[Mat4]) -> Result<(), String> {
        let limit = device.limits().max_buffer_size;
        let plan = crate::buflimit::plan_rows(m.len() as u64, 64, limit);
        if plan.rows == 0 {
            return Err(format!(
                "instances refused: {} matrices, buffer limit {limit} bytes",
                m.len()
            ));
        }
        let cols: Vec<[f32; 16]> = m[..plan.rows as usize]
            .iter()
            .map(|m| m.to_cols_array())
            .collect();
        // each chunk holds its own copy (matrix × its compression decode): a size probe
        let probe = crate::chunks::try_buffer(
            device,
            "ddc-instances",
            bytemuck::cast_slice(&cols),
            wgpu::BufferUsages::VERTEX,
        )
        .map_err(|e| format!("instances refused: {e:?}"))?;
        probe.destroy();
        if plan.dropped > 0 {
            eprintln!(
                "instances: {} of {} matrices dropped, buffer limit reached",
                plan.dropped,
                m.len()
            );
        }
        self.chunks.set_globals(cols);
        self.shadow_key = None;
        for v in self.parked.values_mut() {
            v.shadow_key = None;
        }
        self.unrest();
        Ok(())
    }

    pub fn vertex_count(&self) -> u32 {
        self.chunks.vertex_count()
    }

    /// Sun camera: orthographic projection over the scene bounds and their projection onto the ground along the light.
    fn light_matrix(&self, l: Vec3, ground: f32) -> Mat4 {
        let Some(b) = self.bounds else {
            return Mat4::IDENTITY;
        };
        let c = (b.min + b.max) * 0.5;
        let r = (b.max - b.min).length().max(1.0);
        let view = Mat4::look_at_rh(c - l * r * 2.0, c, Vec3::Z);
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..8 {
            let p = Vec3::new(
                if i & 1 == 0 { b.min.x } else { b.max.x },
                if i & 2 == 0 { b.min.y } else { b.max.y },
                if i & 4 == 0 { b.min.z } else { b.max.z },
            );
            let g = p + l * ((p.z - ground) / -l.z).max(0.0);
            for q in [p, g] {
                let v = view.transform_point3(q);
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        let pad = 0.5;
        // reversed depth, as for the camera: near plane → 1
        Mat4::orthographic_rh(
            lo.x - pad,
            hi.x + pad,
            lo.y - pad,
            hi.y + pad,
            -lo.z + pad,
            -hi.z - pad,
        ) * view
    }

    /// The pipelines for a sample count, built once; `None` if the device rejected them.
    fn ensure_set(&mut self, device: &wgpu::Device, samples: u32) -> Option<usize> {
        if let Some(i) = self.sets.iter().position(|s| s.samples == samples) {
            return Some(i);
        }
        if self.failed_samples.contains(&samples) {
            return None;
        }
        let (set, ok) = scoped(device, wgpu::ErrorFilter::Validation, || {
            MainSet::build(device, &self.main_layout, &self.shader, samples)
        });
        if !ok {
            self.failed_samples.push(samples);
            return None;
        }
        self.sets.push(set);
        Some(self.sets.len() - 1)
    }

    /// The realistic pipeline of a set; on failure the style falls back to shaded for good.
    fn ensure_real(&mut self, device: &wgpu::Device, set: usize) -> bool {
        if self.sets[set].real.is_some() {
            return true;
        }
        if !self.limits.realistic {
            return false;
        }
        if self.real_module.is_none() {
            let src = format!(
                "{}\n{}\n{}",
                include_str!("common.wgsl"),
                include_str!("mesh.wgsl"),
                include_str!("realistic.wgsl")
            );
            let (m, ok) = scoped(device, wgpu::ErrorFilter::Validation, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("ddc-realistic"),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                })
            });
            if !ok {
                self.limits.realistic = false;
                self.health.realistic_failed = true;
                return false;
            }
            self.real_module = Some(m);
        }
        let samples = self.sets[set].samples;
        let (p, ok) = match &self.real_module {
            Some(module) => scoped(device, wgpu::ErrorFilter::Validation, || {
                build_pipeline(
                    device,
                    P {
                        layout: &self.real_layout,
                        module,
                        entry: ("vs", Some("fs_real")),
                        mesh: true,
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        color: Some((FRAME, None)),
                        depth: Some((DEPTH, true, wgpu::CompareFunction::Greater)),
                        bias: FACE_BIAS,
                        samples,
                    },
                )
            }),
            None => return false,
        };
        if !ok {
            self.limits.realistic = false;
            self.health.realistic_failed = true;
            return false;
        }
        self.sets[set].real = Some(p);
        true
    }

    /// `shadow_px` is the map in use; the 1x1 placeholder of "no shadows" counts as none.
    fn out_of_memory(&mut self, shadow_px: u32) {
        let used = if shadow_px > 1 { shadow_px } else { 0 };
        self.limits.on_out_of_memory(used);
        self.health.out_of_memory = true;
    }

    /// The shadow map; a failed allocation keeps the old one and narrows the limits.
    fn ensure_shadow(&mut self, device: &wgpu::Device, px: u32) {
        let px = px.max(1);
        if self.shadow_map.as_ref().is_some_and(|m| m.0 == px) {
            return;
        }
        let (view, ok) = scoped(device, wgpu::ErrorFilter::OutOfMemory, || {
            target(
                device,
                "ddc-shadow",
                (px, px),
                DEPTH,
                1,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        });
        if ok {
            self.shadow_map = Some((px, view));
            self.shadow_key = None;
        } else {
            self.out_of_memory(px);
        }
    }

    /// Frame targets for `key`. On failure the old frame, and so the old image, stays.
    fn ensure_frame(&mut self, device: &wgpu::Device, key: FrameKey) {
        if self.frame.as_ref().is_some_and(|f| f.key == key) {
            return;
        }
        let Some((_, shadow)) = self.shadow_map.as_ref() else {
            return;
        };
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let sampled = rt | wgpu::TextureUsages::TEXTURE_BINDING;
        let size = key.size;
        let ao_size = if key.ao_on {
            (size.0.div_ceil(key.ao_div), size.1.div_ceil(key.ao_div))
        } else {
            (1, 1)
        };
        let l = &self.layouts;
        let (frame, ok) = scoped(device, wgpu::ErrorFilter::OutOfMemory, || {
            let depth1 = target(device, "ddc-depth1", size, DEPTH, 1, sampled);
            let ao = target(device, "ddc-ao", ao_size, AO, 1, sampled);
            let msaa =
                (key.samples > 1).then(|| target(device, "ddc-msaa", size, FRAME, key.samples, rt));
            let msaa_depth = target(device, "ddc-msaa-depth", size, DEPTH, key.samples, rt);
            let color = target(device, "ddc-color", size, FRAME, 1, sampled);
            let bg = |layout, entries: &[wgpu::BindGroupEntry]| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout,
                    entries,
                })
            };
            let view = |binding, v| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(v),
            };
            let smp = |binding, s| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Sampler(s),
            };
            let main_bg = bg(
                &l.main,
                &[
                    view(0, &depth1),
                    view(1, &ao),
                    smp(2, &l.lin),
                    view(3, shadow),
                    smp(4, &l.cmp),
                ],
            );
            let ssao_bg = bg(&l.ssao, &[view(0, &depth1)]);
            let up = if key.filtered { &l.lin } else { &l.near };
            let comp_bg = bg(&l.comp, &[view(0, &depth1), view(1, &color), smp(2, up)]);
            Frame {
                key,
                depth1,
                ao,
                msaa,
                msaa_depth,
                color,
                fresh: true,
                main_bg,
                ssao_bg,
                comp_bg,
            }
        });
        if !ok {
            self.out_of_memory(key.shadow_px);
            return;
        }
        self.last_cam = None;
        self.rest_key = None;
        self.frame = Some(frame);
    }

    /// Glass re-sorting when the eye has moved.
    fn sort_glass(&mut self, queue: &wgpu::Queue, eye: Vec3) {
        self.chunks.sort_glass(queue, eye);
    }

    /// Records the frame into internal targets: `size` — viewport area size in pixels.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        u: &FrameUniforms,
        size: (u32, u32),
    ) {
        let started = web_time::Instant::now();
        if !self.health.usable {
            return;
        }
        self.leaves.upload(queue);
        let q = u.quality();
        let full = (size.0.max(1), size.1.max(1));
        let asked = self.fx.unwrap_or_else(|| FrameFx::from_quality(q));
        let mut fx;
        // each failed allocation narrows the limits, so this ends; after a failure with an older
        // frame in place, that frame goes on and its size and sample count rule
        let mut tries = 0;
        let key = loop {
            fx = self.limits.apply(asked);
            if self.ensure_set(device, fx.samples).is_none() {
                self.limits.max_samples = 1;
                fx.samples = 1;
                if self.ensure_set(device, 1).is_none() {
                    self.health.usable = false;
                    return;
                }
            }
            self.ensure_shadow(device, fx.shadow_px);
            let want = FrameKey {
                size: fx.scaled(full),
                samples: fx.samples,
                ao_div: fx.ao_alloc.divisor(),
                ao_on: fx.ao_alloc != AoMode::Off,
                filtered: fx.scale < 1.0,
                shadow_px: self.shadow_map.as_ref().map_or(1, |m| m.0),
            };
            self.ensure_frame(device, want);
            if let Some(f) = self.frame.as_ref() {
                break f.key;
            }
            tries += 1;
            if tries >= 16 {
                return;
            }
        };
        if let Some(f) = self.frame.as_mut().filter(|f| f.fresh) {
            f.fresh = false;
            init_resolve_target(enc, &f.color);
        }
        let size = key.size;
        let Some(set) = self.sets.iter().position(|s| s.samples == key.samples) else {
            return;
        };
        let real = fx.realistic && self.ensure_real(device, set);
        // textures are uploaded only once the realistic style is on screen
        let tex_on = real && fx.textures;
        if tex_on {
            // once seen, images stay decoded: several views must not flip it back and forth
            crate::texture::set_wanted(true);
            if let Some((rev, set)) = crate::texture::current(self.tex_seen) {
                self.tex_seen = rev;
                let ok = self.tex.load(device, queue, &set);
                self.health.textures_failed = !ok;
                self.unrest();
            }
            if let Some((rev, set)) = crate::anchor::current(self.anchor_seen) {
                self.anchor_seen = rev;
                self.tex.set_anchors(device, queue, &set);
                self.unrest();
            }
        }
        let tex_on = tex_on && self.tex.layers > 0;
        // ready before motion needs it: compiling during a drag would hitch
        if key.samples > 1 && !self.chunks.moving() {
            if let Some(low) = self.ensure_set(device, 1) {
                if real {
                    self.ensure_real(device, low);
                }
            }
        }
        let ground = self.bounds.map_or(0.0, |b| b.min.z.min(0.0)) - 0.012;
        let sel_new = self.sel.sync(device, queue);
        let anim_new = self.anim.sync(device, queue);
        if sel_new || anim_new {
            self.bind_group = self.frame_group_now(device);
            self.sel_epoch += 1;
            self.bg_epoch = self.sel_epoch;
        }
        let mut u = (*u).with_selection_set(self.sel.ranges(), self.sel.generation());
        u.sel_n[3] = self.anim.state.tag();
        let l = Vec3::from_slice(&u.light_dir[..3]);
        u.light_vp = self.light_matrix(l, ground).to_cols_array_2d();
        u.view = [size.0 as f32, size.1 as f32, key.ao_div as f32, ground];
        let effects = u.params[1] > 0.5;
        u.params[2] = if effects { fx.ao.code() } else { 0.0 };
        u.params[3] = f32::from(u8::from(effects && fx.shadow_px > 0));
        u.wind = if fx.wind > 0.0 { [fx.wind, wind_time(), 0.0, 0.0] } else { [0.0; 4] };
        u.fx = [
            f32::from(u8::from(fx.edges)),
            f32::from(u8::from(fx.contour)),
            f32::from(u8::from(tex_on)),
            f32::from(u8::from(fx.materials)),
        ];
        if self.bounds.is_none() || self.chunks.total_triangles() > self.chunks.cfg.shadow_tris {
            u.params[3] = 0.0;
        }
        self.shadow = u.params[3] > 0.5;
        let vp = Mat4::from_cols_array_2d(&u.view_proj);
        let eye = Vec3::from_slice(&u.eye[..3]);
        let occlusion = self.chunks.cfg.occlusion
            && self.chunks.total_triangles() >= self.chunks.cfg.occlusion_min_tris
            && occlusion_allowed(&u, ground);
        if occlusion {
            if let (Some(h), Some(f), Some((pvp, peye))) =
                (self.hiz.as_mut(), self.frame.as_ref(), self.last_cam)
            {
                let key = crate::hiz::Key {
                    clip: [u.clip_min, u.clip_max],
                    cats: u.cats,
                    style: u.style[0],
                    scene: self.chunks.occluder_rev(),
                };
                h.step(device, queue, &f.depth1, size, pvp, peye, key);
            }
        } else if let Some(h) = self.hiz.as_mut() {
            h.clear();
        }
        // an idle frame stays on screen for a long time: a snapshot from another camera is not acceptable in it
        let resting = self.chunks.resting_next();
        let hiz_now = self
            .hiz
            .as_ref()
            .filter(|_| occlusion)
            .and_then(|h| h.usable(&vp))
            .filter(|z| !resting || z.same_view(&vp));
        self.chunks
            .set_hot(u.sel, self.sel.ranges(), self.sel.generation());
        self.chunks
            .set_cat_rules(u.cats.iter().flatten().any(|&c| c < 1.0));
        if self.fx.is_some() {
            self.chunks.cfg.tol_px = fx.tol_px;
            self.chunks.cfg.moving_scale = fx.moving_scale;
        }
        self.chunks.cfg.draw_distance = self
            .bounds
            .map_or(0.0, |b| (b.max - b.min).length() * fx.draw_distance);
        // the window size, not the scaled one: a scale step must not change the level of detail
        self.chunks.cull(vp, full, eye, hiz_now);
        self.chunks.pump(device, queue);
        // while a large model is moving — no corner occlusion and no shadow recomputation
        let lean = self.chunks.is_big() && self.chunks.moving();
        let ssao = u.params[2] > 0.5 && !lean;
        if lean {
            u.params[2] = 0.0;
        }
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
        self.sort_glass(queue, eye);
        let shadow_key = ShadowKey {
            light_vp: u.light_vp,
            clip: [u.clip_min, u.clip_max],
            cats: u.cats,
            style: u.style,
            generation: self.chunks.generation(),
            epoch: self.chunks.upload_epoch(),
        };
        let shadow_due = self.shadow
            && self.shadow_key != Some(shadow_key)
            && ((fx.shadows_live && (!self.chunks.is_big() || self.chunks.settled()))
                || self.shadow_key.is_none());
        if shadow_due {
            self.shadow_key = Some(shadow_key);
        }
        self.last_cam = Some((vp, eye));
        let rest = (
            bytemuck::bytes_of(&u).to_vec(),
            size,
            self.chunks.generation(),
            self.chunks.upload_epoch(),
            self.sel.generation(),
        );
        let same = self.rest_key.as_ref() == Some(&rest);
        self.rest_key = Some(rest);
        // a full idle frame is already on screen: the same passes over the whole model would give the same pixels
        // every view keeps its own frame, so a resting pane is never drawn again
        if same
            && !shadow_due
            && !self.redraw_at_rest
            && self.chunks.rest_drawn()
            && self.chunks.frame_stats().pending == 0
        {
            let mut stats = self.chunks.frame_stats();
            stats.cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
            crate::perf::publish(stats);
            return;
        }
        self.redraws += 1;
        let Some(f) = self.frame.as_ref() else {
            return;
        };
        let pipes = &self.sets[set];
        if let (true, Some((_, shadow_view))) = (shadow_due, self.shadow_map.as_ref()) {
            let mut pass = depth_pass(enc, shadow_view);
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            self.chunks.draw_opaque(&mut pass, true);
        }
        {
            let mut pass = depth_pass(enc, &f.depth1);
            pass.set_pipeline(&self.prepass_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            self.chunks.draw_prepass(&mut pass);
            // ground in depth: occlusion at wall bases and a silhouette outline on it
            pass.set_pipeline(&self.prepass_ground);
            pass.draw(0..6, 0..1);
        }
        if ssao {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ddc-ssao"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &f.ao,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.ssao_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_bind_group(1, &f.ssao_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ddc-main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: f.msaa.as_ref().unwrap_or(&f.color),
                resolve_target: f.msaa.as_ref().map(|_| &f.color),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: if f.msaa.is_some() {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &f.msaa_depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_bind_group(1, &f.main_bg, &[]);
        let faces = pipes.real.as_ref().filter(|_| real).unwrap_or(&pipes.mesh);
        pass.set_pipeline(faces);
        if real {
            pass.set_bind_group(2, &self.tex.bg, &[]);
        }
        self.chunks.draw_opaque(&mut pass, false);
        self.chunks.draw_impostors(&mut pass);
        pass.set_pipeline(&pipes.lines);
        self.chunks.draw_lines(&mut pass, fx.edges);
        if q != Quality::Off {
            pass.set_pipeline(&pipes.ground);
            pass.draw(0..6, 0..1);
        }
        if self.chunks.has_glass() {
            pass.set_pipeline(&pipes.glass);
            self.chunks.draw_glass(&mut pass);
        }
        drop(pass);
        let mut stats = self.chunks.frame_stats();
        stats.cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
        crate::perf::publish(stats);
    }

    /// Composites the finished frame onto the viewport area (premultiplied alpha over the background).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if !self.health.usable {
            return;
        }
        let Some(f) = self.frame.as_ref() else {
            return;
        };
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_bind_group(1, &f.comp_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(feature = "gpu")]
fn target(
    device: &wgpu::Device,
    label: &'static str,
    (w, h): (u32, u32),
    format: wgpu::TextureFormat,
    samples: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

#[cfg(feature = "gpu")]
/// Clears a new single-sample target that an MSAA pass will resolve into. On DX12 a placed
/// render target holds undefined data until a clear; resolving into it first is undefined
/// behaviour, and some drivers corrupt the process heap on it.
pub fn init_resolve_target(enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("ddc-init-resolve"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
}

#[cfg(feature = "gpu")]
fn depth_pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("ddc-depth"),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}
