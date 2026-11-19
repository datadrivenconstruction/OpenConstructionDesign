// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The 60 most used commands' icons and the 12 tools that keep a caption in the compact ribbon.
//! Both lists are data for tests and the ribbon; the drawings live in docs/design/icons_v4.

/// Icon names of the 60 most used commands (roof glyphs have their own family test).
pub const TOP60: [&str; 60] = [
    "select",
    "wall",
    "wall-room",
    "door",
    "window",
    "component",
    "column",
    "ceiling",
    "floor",
    "curtain",
    "stair",
    "railing",
    "ramp",
    "opening",
    "room",
    "beam",
    "column-str",
    "wall-bearing",
    "slab",
    "footing",
    "brace",
    "move",
    "copy",
    "rotate",
    "mirror",
    "trim-extend",
    "extend",
    "split",
    "align",
    "offset",
    "array",
    "delete",
    "group",
    "pin",
    "unpin",
    "hide",
    "isolate",
    "reveal",
    "measure",
    "join",
    "paste",
    "cut",
    "clip-copy",
    "eyedropper",
    "dimension",
    "text",
    "tag",
    "leader",
    "spot-elev",
    "detail-line",
    "plan-view",
    "3d-view",
    "section",
    "sheet",
    "zoom-fit",
    "section-box",
    "level",
    "grid",
    "undo",
    "redo",
];

/// Commands whose tile shows a caption under the icon even at compact width.
pub const TOP_LABELLED: [&str; 12] = [
    "arch.wall",
    "arch.door",
    "arch.window",
    "arch.floor",
    "mod.move",
    "mod.copy",
    "mod.rotate",
    "mod.mirror",
    "mod.trim",
    "mod.delete",
    "ann.dim",
    "ann.text",
];

/// Longest compact caption, in characters: the tile stays about as wide as its icon.
pub const TILE_MAX_CHARS: usize = 10;

/// Caption of a compact tile (`cmd.<id>.tile`), `None` for tools that stay icon only.
pub fn tile_label(cat: &crate::Catalog, cmd: &crate::Command) -> Option<String> {
    if !TOP_LABELLED.contains(&cmd.id.as_str()) {
        return None;
    }
    let key = format!("{}.tile", cmd.label_key());
    let s = cat.t(&key);
    Some(if s == key {
        crate::widgets::ribbon::short_label(cat, cmd)
    } else {
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::{names, rasterize};
    use std::collections::HashSet;

    /// Coverage at `px` from the optical master the product draws there.
    fn alpha(name: &str, px: u32) -> Vec<f32> {
        let img = rasterize(name, px).unwrap();
        img.pixels.iter().map(|p| p.a() as f32 / 255.0).collect()
    }

    #[test]
    fn top60_are_unique_and_drawn() {
        let set: HashSet<_> = TOP60.iter().collect();
        assert_eq!(set.len(), TOP60.len(), "duplicate in TOP60");
        for n in TOP60 {
            assert!(names().contains(&n), "no icon file for {n}");
        }
    }

    #[test]
    fn top60_are_all_in_use_by_commands() {
        let r = crate::Registry::builtin();
        let used: HashSet<&str> = r.all().iter().map(|c| c.icon.as_str()).collect();
        // history and selection are not ribbon commands of their own
        let free: HashSet<&str> = ["select", "undo", "redo", "level", "grid"].into();
        for n in TOP60 {
            assert!(used.contains(n) || free.contains(n), "{n}: no command uses it");
        }
    }

    /// At 16 px every glyph keeps solid ink (not just a grey haze) and leaves white inside:
    /// not a blob. Bounds from the measured set with a margin.
    #[test]
    fn top60_read_at_16_px() {
        let mut bad = Vec::new();
        for n in TOP60 {
            let a = alpha(n, 16);
            let solid = a.iter().filter(|v| **v >= 0.8).count();
            let touched = a.iter().filter(|v| **v > 0.2).count();
            if solid < 12 || touched > 156 {
                bad.push(format!("{n}: solid {solid}, touched {touched}"));
            }
        }
        assert!(bad.is_empty(), "weak at 16 px: {bad:?}");
    }

    /// The ambiguity bar at 16 px holds inside the top 60 and against the rest of the set.
    #[test]
    fn top60_are_distinct_at_16_px() {
        let diff = |a: &[f32], b: &[f32]| {
            let (mut d, mut u) = (0.0f32, 0.0f32);
            for (x, y) in a.iter().zip(b) {
                d += (x - y).abs();
                u += x.max(*y);
            }
            d / u.max(f32::EPSILON)
        };
        let all: Vec<(&str, Vec<f32>)> = names().iter().map(|n| (*n, alpha(n, 16))).collect();
        let mut bad = Vec::new();
        for (i, (a, ma)) in all.iter().enumerate() {
            if !TOP60.contains(a) {
                continue;
            }
            for (j, (b, mb)) in all.iter().enumerate() {
                if i == j || (TOP60.contains(b) && j < i) {
                    continue;
                }
                let d = diff(ma, mb);
                if d < 0.10 {
                    bad.push(format!("{a} ~ {b}: {d:.3}"));
                }
            }
        }
        assert!(bad.is_empty(), "top-60 glyph looks like another: {bad:?}");
    }

    #[test]
    fn labelled_tiles_have_short_unique_captions() {
        let reg = crate::Registry::builtin();
        for lang in [crate::Lang::En, crate::Lang::Ru] {
            let cat = crate::Catalog::new(lang);
            let mut seen = HashSet::new();
            for id in TOP_LABELLED {
                let c = reg.get(id).unwrap_or_else(|| panic!("no command {id}"));
                assert!(!c.icon.is_empty(), "{id}: no icon");
                assert!(TOP60.contains(&c.icon.as_str()), "{id}: icon not in TOP60");
                let key = format!("{}.tile", c.label_key());
                assert!(cat.t(&key) != key, "{id}: no tile caption");
                let l = tile_label(&cat, c).unwrap();
                assert!(l.chars().count() <= TILE_MAX_CHARS, "{id}: '{l}' too long");
                assert!(seen.insert(l.clone()), "caption '{l}' used twice");
            }
        }
    }

    #[test]
    fn other_tools_stay_icon_only() {
        let reg = crate::Registry::builtin();
        let cat = crate::Catalog::new(crate::Lang::En);
        let plain = reg.get("mod.align").unwrap();
        assert!(tile_label(&cat, plain).is_none());
        assert_eq!(TOP_LABELLED.iter().collect::<HashSet<_>>().len(), 12);
    }
}
