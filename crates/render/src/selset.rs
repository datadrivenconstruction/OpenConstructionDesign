// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The whole selection for the 3D shaders: sorted, merged element ranges in a small integer
//! texture (one range per texel), binary searched per fragment. A texture, not a storage buffer:
//! the GL fallback device has no storage buffers in fragment shaders. Uploaded only on change.

use std::sync::Arc;

/// Texels per texture row; within the smallest 2D texture limit of every backend.
pub const ROW: u32 = 2048;
/// Bytes per texel (`Rg32Uint`: start, end).
pub const TEXEL_BYTES: u32 = 8;
/// Most ranges one texture holds; the rest of a larger set is not highlighted.
pub const MAX_RANGES: usize = (ROW as usize) * (ROW as usize);
#[cfg(feature = "gpu")]
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Uint;

/// Sorted ranges without empty, overlapping or touching neighbours.
pub fn merge_ranges(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    v.retain(|r| r.0 < r.1);
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// Already in `merge_ranges` form.
pub fn is_merged(r: &[(u32, u32)]) -> bool {
    r.iter().all(|x| x.0 < x.1) && r.windows(2).all(|w| w[0].1 < w[1].0)
}

/// Element `e` lies in a merged set (the shader's binary search).
pub fn contains(r: &[(u32, u32)], e: u32) -> bool {
    let k = r.partition_point(|x| x.0 <= e);
    k > 0 && e < r[k - 1].1
}

/// Some element of `[lo, hi)` lies in a merged set.
pub fn overlaps(r: &[(u32, u32)], lo: u32, hi: u32) -> bool {
    let k = r.partition_point(|x| x.1 <= lo);
    lo < hi && k < r.len() && r[k].0 < hi
}

/// `[first start, last end)` of a merged set.
pub fn bounds(r: &[(u32, u32)]) -> Option<(u32, u32)> {
    Some((r.first()?.0, r.last()?.1))
}

/// A new set differs from the uploaded one: the same `Arc` is checked without a compare.
pub fn changed(old: &Arc<[(u32, u32)]>, new: &Arc<[(u32, u32)]>) -> bool {
    !Arc::ptr_eq(old, new) && old[..] != new[..]
}

/// Texture rows a set of `n` ranges needs (at least one).
pub fn rows_used(n: usize) -> u32 {
    n.min(MAX_RANGES).div_ceil(ROW as usize).max(1) as u32
}

/// Allocated rows for `n` ranges: a power of two, so a growing selection re-creates rarely.
pub fn rows_alloc(n: usize) -> u32 {
    rows_used(n).next_power_of_two().min(ROW)
}

/// Texel data of the used rows: (start, end) pairs, the tail of the last row zero.
pub fn pack(r: &[(u32, u32)]) -> Vec<u32> {
    let n = r.len().min(MAX_RANGES);
    let mut out = vec![0u32; rows_used(n) as usize * ROW as usize * 2];
    for (i, (a, b)) in r[..n].iter().enumerate() {
        out[i * 2] = *a;
        out[i * 2 + 1] = *b;
    }
    out
}

#[cfg(feature = "gpu")]
/// The selection set on the GPU and its change counter (part of the frame uniforms, so a
/// changed set redraws a resting frame).
pub struct SelGpu {
    tex: wgpu::Texture,
    pub view: wgpu::TextureView,
    rows: u32,
    ranges: Arc<[(u32, u32)]>,
    generation: u32,
    dirty: bool,
}

#[cfg(feature = "gpu")]
impl SelGpu {
    pub fn new(device: &wgpu::Device) -> Self {
        let (tex, view) = Self::texture(device, 1);
        Self {
            tex,
            view,
            rows: 1,
            ranges: Arc::from(Vec::new()),
            generation: 0,
            dirty: false,
        }
    }

    fn texture(device: &wgpu::Device, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ddc-selection"),
            size: wgpu::Extent3d {
                width: ROW,
                height: rows,
                depth_or_array_layers: 1,
            },
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

    /// New set; `false` when it equals the current one (nothing to upload).
    pub fn set(&mut self, r: Arc<[(u32, u32)]>) -> bool {
        if !changed(&self.ranges, &r) {
            return false;
        }
        self.ranges = if is_merged(&r) {
            r
        } else {
            Arc::from(merge_ranges(r.to_vec()))
        };
        self.generation = self.generation.wrapping_add(1);
        self.dirty = true;
        true
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Uploads a changed set; `true` when the texture was re-created (the bind group follows).
    pub fn sync(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        let n = self.ranges.len();
        let grown = rows_used(n) > self.rows;
        if grown {
            self.rows = rows_alloc(n);
            (self.tex, self.view) = Self::texture(device, self.rows);
        }
        if n >= 2 {
            let rows = rows_used(n);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&pack(&self.ranges)),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ROW * TEXEL_BYTES),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: ROW,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
        }
        grown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_empty_one_many() {
        assert!(merge_ranges(vec![]).is_empty());
        assert_eq!(merge_ranges(vec![(3, 4)]), vec![(3, 4)]);
        assert_eq!(
            merge_ranges(vec![(9, 10), (1, 2), (5, 7)]),
            vec![(1, 2), (5, 7), (9, 10)]
        );
    }

    #[test]
    fn merge_overlap_adjacent_and_empty_ranges() {
        // touching and overlapping ranges fold, empty and inverted ones drop
        assert_eq!(
            merge_ranges(vec![(4, 6), (0, 2), (2, 3), (5, 9), (7, 7), (12, 11), (9, 10)]),
            vec![(0, 3), (4, 10)]
        );
        assert_eq!(merge_ranges(vec![(0, 10), (2, 3)]), vec![(0, 10)]);
        assert!(is_merged(&merge_ranges(vec![(8, 9), (1, 4), (3, 6)])));
        assert!(!is_merged(&[(0, 2), (2, 3)]));
        assert!(!is_merged(&[(3, 4), (0, 1)]));
    }

    #[test]
    fn membership_matches_binary_search_of_shader() {
        let r = merge_ranges(vec![(10, 12), (2, 3), (20, 25)]);
        let inside: Vec<u32> = (0..30).filter(|e| contains(&r, *e)).collect();
        assert_eq!(inside, vec![2, 10, 11, 20, 21, 22, 23, 24]);
        assert!(!contains(&[], 0));
        assert!(overlaps(&r, 0, 3) && overlaps(&r, 24, 99) && overlaps(&r, 11, 12));
        assert!(!overlaps(&r, 3, 10) && !overlaps(&r, 25, 99) && !overlaps(&r, 5, 5));
        assert_eq!(bounds(&r), Some((2, 25)));
        assert_eq!(bounds(&[]), None);
    }

    #[test]
    fn changed_selection_detection() {
        let a: Arc<[(u32, u32)]> = Arc::from(vec![(1, 2), (5, 6)]);
        assert!(!changed(&a, &a.clone()));
        // equal content in a new allocation (rebuilt each frame) is no change
        assert!(!changed(&a, &Arc::from(vec![(1, 2), (5, 6)])));
        assert!(changed(&a, &Arc::from(vec![(1, 2), (4, 6)])));
        assert!(changed(&a, &Arc::from(Vec::new())));
        let e: Arc<[(u32, u32)]> = Arc::from(Vec::new());
        assert!(!changed(&e, &Arc::from(Vec::new())));
    }

    #[test]
    fn uniform_layout_keeps_16_byte_alignment() {
        use crate::renderer::FrameUniforms;
        let size = std::mem::size_of::<FrameUniforms>();
        assert_eq!(size % 16, 0);
        // the new field is last, as in the shader struct
        assert_eq!(std::mem::offset_of!(FrameUniforms, wind), size - 16);
        assert_eq!(std::mem::offset_of!(FrameUniforms, sel_n), size - 32);
        assert_eq!(std::mem::offset_of!(FrameUniforms, fx), size - 48);
    }

    #[test]
    fn uniforms_take_set_bounds_and_count() {
        use crate::renderer::FrameUniforms;
        use crate::scene::NO_SELECTION;
        let cam = crate::camera::OrbitCamera::default();
        let base = FrameUniforms::new(&cam, 1.0, None).with_ranges(Some((7, 8)), Some((40, 41)));
        // none or one range: `with_ranges` decides, no texture search
        let one = base.with_selection_set(&[(7, 8)], 3);
        assert_eq!(one.sel, [7, 8, 40, 41]);
        assert_eq!(one.sel_n, [0, ROW, 3, 0]);
        let none = FrameUniforms::new(&cam, 1.0, None).with_selection_set(&[], 0);
        assert_eq!(none.sel, [NO_SELECTION; 4]);
        assert_eq!(none.sel_n[0], 0);
        // many: bounds of the set, hover kept
        let many = base.with_selection_set(&[(2, 3), (7, 8), (30, 32)], 9);
        assert_eq!(many.sel, [2, 32, 40, 41]);
        assert_eq!(many.sel_n, [3, ROW, 9, 0]);
    }

    #[test]
    fn shaders_read_the_selection_texture() {
        let common = include_str!("common.wgsl");
        assert!(common.contains("@group(0) @binding(1) var sel_tex: texture_2d<u32>;"));
        assert!(common.contains("sel_n: vec4<u32>"));
        assert!(common.contains("fn is_selected(e: u32) -> bool"));
        // every fragment path uses the set test, none the single range of old
        for (name, src) in [
            ("mesh", include_str!("mesh.wgsl")),
            ("realistic", include_str!("realistic.wgsl")),
        ] {
            assert!(src.contains("is_selected(i.elem)"), "{name}");
            assert!(!src.contains("u.sel.x"), "{name}");
        }
        assert_eq!(include_str!("mesh.wgsl").matches("is_selected(i.elem)").count(), 2);
    }

    #[test]
    fn texture_rows_and_packing() {
        assert_eq!(rows_used(0), 1);
        assert_eq!(rows_used(ROW as usize), 1);
        assert_eq!(rows_used(ROW as usize + 1), 2);
        // 10k separate elements: 5 rows, 8 allocated
        assert_eq!(rows_used(10_000), 5);
        assert_eq!(rows_alloc(10_000), 8);
        assert_eq!(rows_alloc(usize::MAX), ROW);
        // bytes per row: whole texels, a multiple of the copy alignment
        assert_eq!((ROW * TEXEL_BYTES) % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
        let p = pack(&[(1, 2), (5, 9)]);
        assert_eq!(p.len(), ROW as usize * 2);
        assert_eq!(&p[..5], &[1, 2, 5, 9, 0]);
        let many: Vec<(u32, u32)> = (0..10_000u32).map(|i| (i * 2, i * 2 + 1)).collect();
        let p = pack(&many);
        assert_eq!(p.len(), 5 * ROW as usize * 2);
        assert_eq!(&p[19_998..20_000], &[19_998, 19_999]);
    }
}
