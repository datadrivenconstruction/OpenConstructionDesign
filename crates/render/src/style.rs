// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D visual styles and the face-tag bits they use: screen-door transparency (categories,
//! x-ray) and section fill (poche) carried as a colour directly in the tag. Tag layout:
//! bit 31 — lines; 30/29 — network colour/shell; 28 — section fill (+ 0xRRGGBB);
//! 24–27 — category number 0…15 (coverage — the `FrameUniforms::cats` table).

/// How faces are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VisualStyle {
    /// Lit faces, shadows and occlusion, with edges. Key `shaded`: the look of older projects.
    #[default]
    ShadedEdges,
    /// Lit faces without edges.
    Shaded,
    /// Procedural surfaces, sky reflection, shadows and occlusion by performance level.
    Realistic,
    /// Category colour without shadows or highlights — identical from any side.
    Consistent,
    /// Paper-coloured faces, only edges are visible (hidden ones are occluded).
    HiddenLine,
    /// Edges only, faces are not drawn.
    Wireframe,
    /// Everything is semi-transparent (edges are visible through).
    XRay,
}

impl VisualStyle {
    /// Order of the switcher: from lines to full surfaces, then the special ones.
    pub const ALL: [VisualStyle; 7] = [
        VisualStyle::Wireframe,
        VisualStyle::HiddenLine,
        VisualStyle::Shaded,
        VisualStyle::ShadedEdges,
        VisualStyle::Realistic,
        VisualStyle::Consistent,
        VisualStyle::XRay,
    ];
    /// Code in `FrameUniforms::style[0]`. The lit styles share 0: they differ by `fx` and the
    /// face pipeline, so depth, occlusion and shadow caches stay valid across them.
    pub fn code(self) -> f32 {
        match self {
            VisualStyle::ShadedEdges | VisualStyle::Shaded | VisualStyle::Realistic => 0.0,
            VisualStyle::Consistent => 1.0,
            VisualStyle::HiddenLine => 2.0,
            VisualStyle::Wireframe => 3.0,
            VisualStyle::XRay => 4.0,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            VisualStyle::ShadedEdges => "shaded",
            VisualStyle::Shaded => "shaded_plain",
            VisualStyle::Realistic => "realistic",
            VisualStyle::Consistent => "consistent",
            VisualStyle::HiddenLine => "hidden_line",
            VisualStyle::Wireframe => "wireframe",
            VisualStyle::XRay => "xray",
        }
    }
    /// Crease edges and silhouette belong to this style.
    pub fn has_edges(self) -> bool {
        !matches!(self, VisualStyle::Shaded | VisualStyle::Realistic)
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.key() == s)
    }
    /// Shadows and ambient occlusion make sense (otherwise the frame has no effects). In x-ray
    /// the depth is screen-door (Bayer) — occlusion computed from it is noisy, so effects are off.
    pub fn effects(self) -> bool {
        matches!(
            self,
            VisualStyle::ShadedEdges | VisualStyle::Shaded | VisualStyle::Realistic
        )
    }
}

/// Section fill: `TAG_CAP | 0xRRGGBB` (sRGB), drawn unlit.
pub const TAG_CAP: u32 = 0x1000_0000;
const ALPHA_MASK: u32 = crate::renderer::CAT_MASK;

pub fn cap_tag(rgb: [u8; 3]) -> u32 {
    TAG_CAP | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32
}

pub fn is_cap(tag: u32) -> bool {
    tag & 0xF000_0000 == TAG_CAP
}

/// Coverage of a category face under the view rule: hidden — 0, transparency `pct` % — 1 − pct
/// (at least 1/16, so a transparent category does not vanish completely).
pub fn coverage(visible: bool, pct: u8) -> f32 {
    if !visible {
        return 0.0;
    }
    (1.0 - f32::from(pct.min(100)) / 100.0).max(1.0 / 16.0)
}

/// Tag without the category number bits (bits 24–27; palette colour) and, for plain tags, without
/// the texture layer (bits 16–23, `texture::with_layer`); same as `base_tag` in mesh.wgsl.
pub fn base_tag(tag: u32) -> u32 {
    if tag & 0x8000_0000 != 0 {
        tag
    } else if tag & 0xF000_0000 == 0 {
        tag & !(ALPHA_MASK | 0x00FF_0000)
    } else {
        tag & !ALPHA_MASK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_codes_roundtrip() {
        for s in VisualStyle::ALL {
            assert_eq!(VisualStyle::parse(s.key()), Some(s));
            assert!((0.0..=4.0).contains(&s.code()));
        }
        assert_eq!(VisualStyle::parse("nope"), None);
        // what older projects saved keeps its look; lit styles do not disturb depth caches
        assert_eq!(VisualStyle::parse("shaded"), Some(VisualStyle::ShadedEdges));
        assert_eq!(VisualStyle::default(), VisualStyle::ShadedEdges);
        assert_eq!(VisualStyle::Realistic.code(), VisualStyle::Shaded.code());
    }

    #[test]
    fn tag_bits_do_not_collide() {
        use crate::renderer::with_cat;
        let t = with_cat(11, 5);
        assert_eq!(base_tag(t), 11);
        // a textured slab is still a slab
        let tx = with_cat(crate::texture::with_layer(11, 4), 5);
        assert_eq!(base_tag(tx), 11);
        assert_eq!((t >> 24) & 15, 5);
        // network colour (bit 30) and fill (bit 28) are not affected by the category number
        let rgb = 0x4000_0000 | 0x00AB_CDEF;
        let t = with_cat(rgb, 15);
        assert_eq!(t & 0x00FF_FFFF, 0x00AB_CDEF);
        assert_eq!(base_tag(t), rgb);
        let c = cap_tag([200, 10, 255]);
        assert!(is_cap(c) && is_cap(with_cat(c, 3)) && !is_cap(t) && !is_cap(11));
        assert_eq!(with_cat(c, 3) & 0x00FF_FFFF, 0x00C8_0AFF);
        assert_eq!(
            with_cat(crate::scene::TAG_EDGE, 2) & 0xF0FF_FFFF,
            crate::scene::TAG_EDGE
        );
    }

    #[test]
    fn coverage_from_rules_and_xray_has_no_effects() {
        assert_eq!(coverage(false, 0), 0.0);
        assert_eq!(coverage(true, 0), 1.0);
        assert!((coverage(true, 40) - 0.6).abs() < 1e-6);
        assert!(coverage(true, 100) > 0.0);
        assert!(VisualStyle::Shaded.effects() && !VisualStyle::XRay.effects());
        assert!(VisualStyle::Realistic.effects() && VisualStyle::ShadedEdges.effects());
    }
}
