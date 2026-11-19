//! Construction-sequence reveal: a deterministic per-element timeline (appear, disappear,
//! shift, tint) and its upload to the GPU.
//!
//! Time is the frame number. `Timeline::sample(frame)` is a pure function: displacement and opacity
//! of each element (`ElemAnim`, 16 B) and the tint colour (`ElemTint`, 16 B). The shader reads them by
//! element number from an `Rgba32Float` texture (two texels per element); a texture rather than a
//! buffer: the fallback GL device has no storage buffers in the fragment shader (as with `selset`).
//! While there is no timeline, there is one texel and `sel_n.w` is zero — the shader reads nothing.

use glam::Vec3;

/// Texels per texture row; two texels per element, a pair never splits across rows.
pub const ROW: u32 = 2048;
/// At most this many elements in the texture: rows are also limited to `ROW`.
pub const MAX_ELEMS: usize = (ROW as usize) * (ROW as usize) / 2;
/// Bytes per texel (`Rgba32Float`).
pub const TEXEL_BYTES: u32 = 16;
#[cfg(feature = "gpu")]
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const COUNT_MASK: u32 = 0x00FF_FFFF;

/// Track time curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    Linear,
    OutCubic,
}

impl Ease {
    /// `t` in [0, 1] -> [0, 1]; both curves give 0 at 0 and exactly 1 at 1.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::OutCubic => {
                let k = 1.0 - t;
                1.0 - k * k * k
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    /// The element rises `rise` m from below its place and fades in; before the start it is absent.
    Appear { rise: f32 },
    /// The element sinks `sink` m and fades out; after the end it is absent.
    Vanish { sink: f32 },
    /// The element arrives from offset `from` (relative to its final position) into place.
    Move { from: Vec3 },
    /// The element starts in colour `rgb` (sRGB) and gradually returns to its own colour.
    Tint { rgb: [u8; 3] },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    /// Element numbers [start, end) — as in picking.
    pub elements: (u32, u32),
    /// Start and end frames.
    pub t0: u32,
    pub t1: u32,
    pub effect: Effect,
    pub ease: Ease,
}

/// State of one element in a frame.
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct ElemAnim {
    pub offset: [f32; 3],
    pub alpha: f32,
}

/// Tint colour (linear RGB) and the fraction by which it replaces the element's colour.
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct ElemTint {
    pub rgb: [f32; 3],
    pub k: f32,
}

impl ElemAnim {
    /// The element as in the static scene.
    pub const REST: ElemAnim = ElemAnim { offset: [0.0; 3], alpha: 1.0 };
}

impl ElemTint {
    pub const NONE: ElemTint = ElemTint { rgb: [0.0; 3], k: 0.0 };
}

fn srgb_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    pub fps: u32,
    pub tracks: Vec<Track>,
    /// Length in frames; after it the frame equals the static one.
    pub len: u32,
}

impl Timeline {
    pub fn new(fps: u32, tracks: Vec<Track>) -> Self {
        let len = tracks.iter().map(|t| t.t1.max(t.t0)).max().unwrap_or(0);
        Self { fps, tracks, len }
    }

    /// Number of elements the timeline touches (highest number + 1).
    pub fn element_count(&self) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.elements.0 < t.elements.1)
            .map(|t| t.elements.1 as usize)
            .max()
            .unwrap_or(0)
    }

    /// Fraction of the track completed: 0 before the start, 1 after the end; zero length — instantaneous at the end.
    fn progress(t: &Track, frame: u32) -> f32 {
        if t.t1 <= t.t0 {
            // zero length: instantaneous at the start
            if frame >= t.t0 { 1.0 } else { 0.0 }
        } else if frame >= t.t1 {
            1.0
        } else if frame <= t.t0 {
            0.0
        } else {
            t.ease.apply((frame - t.t0) as f32 / (t.t1 - t.t0) as f32)
        }
    }

    /// Displacement and opacity that the track gives at frame `frame`.
    fn state(t: &Track, frame: u32) -> ([f32; 3], f32) {
        let p = Self::progress(t, frame);
        match t.effect {
            Effect::Appear { rise } => ([0.0, 0.0, -rise * (1.0 - p)], p),
            Effect::Vanish { sink } => ([0.0, 0.0, -sink * p], 1.0 - p),
            Effect::Move { from } => {
                let k = 1.0 - p;
                ([from.x * k, from.y * k, from.z * k], 1.0)
            }
            Effect::Tint { .. } => ([0.0; 3], 1.0),
        }
    }

    /// State of all elements [0, `element_count`) at a frame: displacements add,
    /// opacities multiply; an element not named by any track is unchanged. Pure function.
    pub fn sample(&self, frame: u32) -> Vec<ElemAnim> {
        let n = self.element_count().min(MAX_ELEMS);
        let mut out = vec![ElemAnim::REST; n];
        for t in &self.tracks {
            let (off, alpha) = Self::state(t, frame);
            // track at rest (ended or not yet started, leaving no trace) — the buffer is already correct
            if off == [0.0; 3] && alpha == 1.0 {
                continue;
            }
            let a = (t.elements.0 as usize).min(n);
            let b = (t.elements.1 as usize).min(n);
            if a >= b {
                continue;
            }
            for e in &mut out[a..b] {
                e.offset[0] += off[0];
                e.offset[1] += off[1];
                e.offset[2] += off[2];
                e.alpha *= alpha;
            }
        }
        out
    }

    /// Tinting of elements at a frame: the track colour fades from full (before the start) to zero
    /// (after the end); with several tracks on one element the last one wins.
    pub fn sample_tint(&self, frame: u32) -> Vec<ElemTint> {
        let n = self.element_count().min(MAX_ELEMS);
        let mut out = vec![ElemTint::NONE; n];
        for t in &self.tracks {
            let Effect::Tint { rgb } = t.effect else { continue };
            let k = 1.0 - Self::progress(t, frame);
            if k <= 0.0 {
                continue;
            }
            let v = ElemTint {
                rgb: [srgb_linear(rgb[0]), srgb_linear(rgb[1]), srgb_linear(rgb[2])],
                k,
            };
            let a = (t.elements.0 as usize).min(n);
            let b = (t.elements.1 as usize).min(n);
            if a < b {
                out[a..b].fill(v);
            }
        }
        out
    }

    /// Reverse playback: frame `frame` of the backward run mirrors the forward one, frame `len - frame`
    /// (appearing plays as disappearing, a shift goes back to the original position).
    pub fn sample_reverse(&self, frame: u32) -> Vec<ElemAnim> {
        self.sample(self.len.saturating_sub(frame))
    }

    pub fn sample_tint_reverse(&self, frame: u32) -> Vec<ElemTint> {
        self.sample_tint(self.len.saturating_sub(frame))
    }
}

/// Texture texels: per element (displacement, opacity) and (colour, fraction); the row tail is zeros.
/// If `tint` is shorter than `anim`, the rest have no tint.
pub fn pack(anim: &[ElemAnim], tint: &[ElemTint]) -> Vec<f32> {
    let n = anim.len().min(MAX_ELEMS);
    let rows = rows_used(n) as usize;
    let mut out = vec![0.0f32; rows * ROW as usize * 4];
    for (i, a) in anim[..n].iter().enumerate() {
        let k = i * 8;
        out[k..k + 3].copy_from_slice(&a.offset);
        out[k + 3] = a.alpha;
        if let Some(t) = tint.get(i) {
            out[k + 4..k + 7].copy_from_slice(&t.rgb);
            out[k + 7] = t.k;
        }
    }
    out
}

/// Texture rows for `n` elements (at least one).
pub fn rows_used(n: usize) -> u32 {
    (n.min(MAX_ELEMS) * 2).div_ceil(ROW as usize).max(1) as u32
}

/// Rows allocated: a power of two, so a growing timeline rarely recreates the texture.
pub fn rows_alloc(n: usize) -> u32 {
    rows_used(n).next_power_of_two().min(ROW)
}

/// Reveal frame without a device: element count, change counter and the data to upload.
#[derive(Default)]
pub struct AnimState {
    count: u32,
    generation: u32,
    pending: Option<Vec<f32>>,
}

impl AnimState {
    /// New reveal frame; `None` — the timeline is cleared, the shader reads nothing again.
    pub fn set(&mut self, frame: Option<(&[ElemAnim], &[ElemTint])>) {
        match frame {
            Some((a, t)) if !a.is_empty() => {
                self.count = a.len().min(MAX_ELEMS) as u32;
                self.pending = Some(pack(a, t));
            }
            _ => {
                if self.count == 0 {
                    return;
                }
                self.count = 0;
                self.pending = None;
            }
        }
        self.generation = self.generation.wrapping_add(1);
    }

    /// Number of timeline elements; 0 — no reveal.
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Value of `sel_n.w`: element count and (high 8 bits) the change counter.
    pub fn tag(&self) -> u32 {
        if self.count == 0 {
            0
        } else {
            self.count | ((self.generation & 0xFF) << 24)
        }
    }
}

/// Reveal on the GPU: texture and state.
#[cfg(feature = "gpu")]
pub struct AnimGpu {
    tex: wgpu::Texture,
    pub view: wgpu::TextureView,
    rows: u32,
    pub state: AnimState,
}

#[cfg(feature = "gpu")]
impl AnimGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        let (tex, view) = Self::texture(device, 1);
        Self { tex, view, rows: 1, state: AnimState::default() }
    }

    /// Allocated texture rows.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    fn texture(device: &wgpu::Device, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-reveal"),
            size: wgpu::Extent3d { width: ROW, height: rows, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        (tex, view)
    }

    /// Uploads a new frame; `true` if the texture was recreated (bind groups must be rebuilt).
    pub fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let Some(data) = self.state.pending.take() else { return false };
        let rows = (data.len() / (ROW as usize * 4)) as u32;
        let grown = rows > self.rows;
        if grown {
            self.rows = rows_alloc(self.state.count as usize);
            (self.tex, self.view) = Self::texture(device, self.rows);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW * TEXEL_BYTES),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d { width: ROW, height: rows, depth_or_array_layers: 1 },
        );
        grown
    }
}

/// Element-count mask in `sel_n.w` (as in the shader).
pub const fn tag_count(tag: u32) -> u32 {
    tag & COUNT_MASK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(a: u32, b: u32, t0: u32, t1: u32, effect: Effect, ease: Ease) -> Track {
        Track { elements: (a, b), t0, t1, effect, ease }
    }

    /// Foundations, columns, slabs, walls, roof — 300 elements in batches of 20 frames.
    fn build_300() -> Timeline {
        let mut tr = vec![];
        for k in 0..15u32 {
            let t0 = k * 10;
            tr.push(track(k * 20, k * 20 + 20, t0, t0 + 20, Effect::Appear { rise: 2.0 }, Ease::OutCubic));
        }
        tr.push(track(0, 20, 0, 40, Effect::Tint { rgb: [40, 200, 80] }, Ease::Linear));
        tr.push(track(100, 110, 30, 60, Effect::Move { from: Vec3::new(1.5, 0.0, 0.0) }, Ease::Linear));
        Timeline::new(30, tr)
    }

    #[test]
    fn sample_is_pure_and_repeatable() {
        let tl = build_300();
        assert_eq!(tl.element_count(), 300);
        let a = tl.sample(37);
        // other frames between calls change nothing
        let _ = tl.sample(5);
        let _ = tl.sample_reverse(80);
        let b = tl.sample(37);
        assert_eq!(a, b);
        assert_eq!(tl.sample_tint(20), tl.sample_tint(20));
        assert_eq!(
            bytemuck::cast_slice::<ElemAnim, u8>(&a),
            bytemuck::cast_slice::<ElemAnim, u8>(&b)
        );
        assert_eq!(std::mem::size_of::<ElemAnim>(), 16);
        assert_eq!(std::mem::size_of::<ElemTint>(), 16);
    }

    #[test]
    fn ease_endpoints_are_exact() {
        for e in [Ease::Linear, Ease::OutCubic] {
            assert_eq!(e.apply(0.0), 0.0);
            assert_eq!(e.apply(1.0), 1.0);
            assert_eq!(e.apply(-3.0), 0.0);
            assert_eq!(e.apply(9.0), 1.0);
        }
        assert!(Ease::OutCubic.apply(0.5) > 0.5);
        assert_eq!(Ease::Linear.apply(0.25), 0.25);
    }

    #[test]
    fn appear_rises_and_fades_in_and_rests_after_the_end() {
        let tl = Timeline::new(
            30,
            vec![track(0, 1, 10, 20, Effect::Appear { rise: 3.0 }, Ease::Linear)],
        );
        let at = |f| tl.sample(f)[0];
        assert_eq!(at(0), ElemAnim { offset: [0.0, 0.0, -3.0], alpha: 0.0 });
        assert_eq!(at(10), at(0));
        let mid = at(15);
        assert!((mid.alpha - 0.5).abs() < 1e-6 && (mid.offset[2] + 1.5).abs() < 1e-6);
        assert_eq!(at(20), ElemAnim::REST);
        assert_eq!(at(9999), ElemAnim::REST);
    }

    #[test]
    fn vanish_sinks_and_leaves_move_and_tint_settle() {
        let tl = Timeline::new(
            30,
            vec![
                track(0, 1, 0, 10, Effect::Vanish { sink: 2.0 }, Ease::Linear),
                track(1, 2, 0, 10, Effect::Move { from: Vec3::new(4.0, 0.0, 1.0) }, Ease::Linear),
                track(2, 3, 0, 10, Effect::Tint { rgb: [255, 0, 0] }, Ease::Linear),
            ],
        );
        let s0 = tl.sample(0);
        assert_eq!(s0[0], ElemAnim::REST);
        assert_eq!(s0[1].offset, [4.0, 0.0, 1.0]);
        let s10 = tl.sample(10);
        assert_eq!(s10[0], ElemAnim { offset: [0.0, 0.0, -2.0], alpha: 0.0 });
        assert_eq!(s10[1], ElemAnim::REST);
        assert_eq!(s10[2], ElemAnim::REST);
        let t0 = tl.sample_tint(0);
        assert_eq!(t0[2].k, 1.0);
        assert!((t0[2].rgb[0] - 1.0).abs() < 1e-6 && t0[2].rgb[1] == 0.0);
        assert_eq!(tl.sample_tint(10)[2], ElemTint::NONE);
        assert!((tl.sample_tint(5)[2].k - 0.5).abs() < 1e-6);
    }

    #[test]
    fn tracks_on_one_element_compose() {
        let tl = Timeline::new(
            30,
            vec![
                track(0, 1, 0, 10, Effect::Appear { rise: 2.0 }, Ease::Linear),
                track(0, 1, 0, 10, Effect::Move { from: Vec3::new(2.0, 0.0, 0.0) }, Ease::Linear),
            ],
        );
        let e = tl.sample(5)[0];
        assert_eq!(e.offset, [1.0, 0.0, -1.0]);
        assert_eq!(e.alpha, 0.5);
    }

    #[test]
    fn reverse_mirrors_forward() {
        let tl = build_300();
        for f in [0, 1, 17, 60, 100, tl.len, tl.len + 50] {
            assert_eq!(tl.sample_reverse(f), tl.sample(tl.len.saturating_sub(f)), "frame {f}");
            assert_eq!(tl.sample_tint_reverse(f), tl.sample_tint(tl.len.saturating_sub(f)));
        }
        // reverse playback starts from the static scene and ends empty: everything revealed fades out
        assert!(tl.sample_reverse(0).iter().all(|e| *e == ElemAnim::REST));
        assert!(tl.sample_reverse(tl.len).iter().all(|e| e.alpha == 0.0));
    }

    #[test]
    fn untouched_elements_and_empty_timeline_stay_at_rest() {
        let tl = Timeline::new(30, vec![]);
        assert_eq!(tl.len, 0);
        assert!(tl.sample(7).is_empty());
        let tl = Timeline::new(
            30,
            vec![
                track(5, 6, 0, 10, Effect::Appear { rise: 1.0 }, Ease::Linear),
                // an empty range does not stretch the timeline
                track(900, 900, 0, 10, Effect::Appear { rise: 1.0 }, Ease::Linear),
            ],
        );
        let s = tl.sample(0);
        assert_eq!(s.len(), 6);
        assert!(s[..5].iter().all(|e| *e == ElemAnim::REST));
        assert_eq!(s[5].alpha, 0.0);
    }

    #[test]
    fn pack_two_texels_per_element() {
        let a = [
            ElemAnim { offset: [1.0, 2.0, 3.0], alpha: 0.5 },
            ElemAnim::REST,
        ];
        let t = [ElemTint { rgb: [0.1, 0.2, 0.3], k: 0.75 }];
        let p = pack(&a, &t);
        assert_eq!(p.len(), ROW as usize * 4);
        assert_eq!(&p[..8], &[1.0, 2.0, 3.0, 0.5, 0.1, 0.2, 0.3, 0.75]);
        assert_eq!(&p[8..16], &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        // 1024 elements — one row; the 1025th — a second
        assert_eq!(rows_used(1024), 1);
        assert_eq!(rows_used(1025), 2);
        assert_eq!(rows_alloc(5000), 8);
        #[cfg(feature = "gpu")]
        assert_eq!((ROW * TEXEL_BYTES) % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    }

    #[test]
    fn tag_carries_count_and_generation() {
        let a = [ElemAnim::REST; 3];
        let mut g = AnimState::default();
        assert_eq!(g.tag(), 0);
        g.set(Some((&a[..], &[][..])));
        let t1 = g.tag();
        g.set(Some((&a[..], &[][..])));
        assert_ne!(t1, g.tag(), "every frame of the reveal redraws the resting state");
        assert_eq!(tag_count(g.tag()), 3);
        g.set(None);
        assert_eq!((g.tag(), g.count()), (0, 0));
        // a cleared timeline again leaves the counter alone: an idle frame is not redrawn needlessly
        let off = g.tag();
        g.set(None);
        assert_eq!(g.tag(), off);
    }

    #[test]
    fn sample_100k_elements_fits_the_budget() {
        // 100,000 elements in 50 tracks: some running, some finished, some not yet started
        let tracks: Vec<Track> = (0..50u32)
            .map(|k| track(k * 2000, k * 2000 + 2000, k * 4, k * 4 + 40, Effect::Appear { rise: 1.0 }, Ease::OutCubic))
            .collect();
        let tl = Timeline::new(30, tracks);
        assert_eq!(tl.element_count(), 100_000);
        let _ = tl.sample(100);
        let t = std::time::Instant::now();
        let mut keep = 0;
        for f in [50, 100, 150, 5] {
            keep += tl.sample(f).len();
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 4.0;
        assert_eq!(keep, 400_000);
        // the 2 ms budget is for an optimised build; a debug build is several times slower
        let budget = if cfg!(debug_assertions) { 25.0 } else { 2.0 };
        assert!(ms <= budget, "sample 100k: {ms:.2} ms > {budget} ms");
    }

    #[test]
    fn shader_contract_in_common_wgsl() {
        let c = include_str!("common.wgsl");
        assert!(c.contains("@group(0) @binding(2) var anim_tex: texture_2d<f32>;"));
        assert!(c.contains("const ANIM_ROW: u32 = 2048u;"));
        assert_eq!(ROW, 2048);
        // reading is gated by the counter: without a timeline the shader does not touch the texture
        assert!(c.contains("fn anim_on(e: u32) -> bool { return e < (u.sel_n.w & 0xFFFFFFu); }"));
        assert_eq!(COUNT_MASK, 0xFF_FFFF);
        let m = include_str!("mesh.wgsl");
        assert!(m.contains("anim_off(e)"));
        assert!(!m.contains("face_dropped(i.tag, i.clip.xy)"));
        assert_eq!(m.matches("face_dropped(i.tag, i.clip.xy, i.elem)").count(), 2);
        assert!(include_str!("realistic.wgsl").contains("face_dropped(i.tag, i.clip.xy, i.elem)"));
    }
}
