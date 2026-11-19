// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tests of the ribbon set `assets/icons3` (tools/icons/ribbon.py; `--check` runs the same bars).
use super::*;
use crate::theme::{Mode, Tokens};

const DIR: Direction = Direction::Ribbon;

fn svg_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            if p.is_dir() {
                svg_count(&p)
            } else {
                usize::from(p.extension().is_some_and(|x| x == "svg"))
            }
        })
        .sum()
}

#[test]
fn ribbon_set_is_complete_and_embedded() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons3");
    assert_eq!(
        crate::fragments::ICONS_RIBBON.len(),
        svg_count(std::path::Path::new(dir)),
        "every svg under assets/icons3 is embedded"
    );
    let mut missing = Vec::new();
    for n in names() {
        for m in MASTERS {
            if svg(DIR, m, n).is_none() {
                missing.push(format!("{m}/{n}"));
            }
        }
    }
    assert!(missing.is_empty(), "ribbon masters missing: {missing:?}");
    assert_eq!(crate::fragments::ICONS_RIBBON.len(), names().len() * 3);
}

/// Every command with an icon slot (ribbon, menus, palette, panels) has a ribbon glyph at every
/// master: the default set never falls back to the dashed placeholder.
#[test]
fn every_command_with_an_icon_has_a_ribbon_glyph() {
    let mut bad = Vec::new();
    let mut n = 0;
    for c in crate::Registry::builtin().all() {
        if c.icon.is_empty() {
            continue;
        }
        n += 1;
        let (d, bare) = resolve_with(set("ribbon").unwrap().style(), &c.icon, 20);
        assert_eq!(d, DIR, "the ribbon set draws the ribbon glyphs");
        if !MASTERS.iter().all(|m| svg(DIR, *m, bare).is_some()) {
            bad.push(format!("{} '{}'", c.id, c.icon));
        }
    }
    assert!(n > 300, "commands with icons: {n}");
    assert!(bad.is_empty(), "commands without a ribbon glyph: {bad:?}");
}

/// `data/command_icons.json` binds glyphs to commands that ship without one (assemblies): each
/// bound glyph exists in every direction and master, and a bound command takes it.
#[test]
fn bound_command_icons_exist_everywhere() {
    let map = crate::commands::command_icons();
    assert!(map.contains_key("mod.asm_select") && map.contains_key("mod.asm_disassemble"));
    let mut seen: Vec<&String> = map.values().collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), map.len(), "one glyph per bound command");
    for icon in map.values() {
        for d in Direction::ALL.iter().chain([&DIR]) {
            for m in MASTERS {
                assert!(svg(*d, m, icon).is_some(), "{}/{m}/{icon}", d.dir());
            }
        }
    }
    let r = crate::Registry::builtin();
    for (id, icon) in map {
        if let Some(c) = r.all().iter().find(|c| &c.id == id) {
            assert_eq!(&c.icon, icon, "{id}");
        }
    }
}

/// Colours only from the `icon.*` role tokens (light values in the files), defined in both
/// themes and unique, so the dark swap is unambiguous. Masks may use #000 / #fff.
#[test]
fn ribbon_colours_come_from_role_tokens() {
    let t = Tokens::embedded().unwrap();
    let light: Vec<&String> = t
        .palette(Mode::Light)
        .iter()
        .filter(|(k, _)| k.starts_with("icon."))
        .map(|(_, v)| v)
        .collect();
    assert!(light.len() >= 20, "role tokens: {}", light.len());
    let mut u = light.clone();
    u.sort();
    u.dedup();
    assert_eq!(u.len(), light.len(), "icon.* light values must be unique");
    for k in t.palette(Mode::Light).keys().filter(|k| k.starts_with("icon.")) {
        assert!(t.palette(Mode::Dark).contains_key(k), "{k} missing in dark");
    }
    assert_eq!(role_colours(false).len(), light.len());
    for (key, s) in crate::fragments::ICONS_RIBBON {
        let m = key.split('/').next().unwrap();
        assert!(s.contains(&format!(r#"viewBox="0 0 {m} {m}""#)), "{key}: grid");
        assert!(!s.contains("<text") && !s.contains("style="), "{key}");
        for part in s.split('#').skip(1) {
            let hex: String = part.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            let quoted = part[hex.len()..].starts_with('"');
            if !quoted || hex == "000" || hex == "fff" {
                continue; // a mask id reference, or the mask colours
            }
            let hex = format!("#{hex}");
            assert!(light.iter().any(|v| **v == hex), "{key}: colour {hex} is not a role token");
        }
        // a coloured glyph is colour only; a neutral one is ink only
        assert!(is_mono(s) || !s.contains("currentColor"), "{key}");
    }
}

fn lin(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lum(r: u8, g: u8, b: u8) -> f32 {
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// Every glyph renders at 16 / 20 / 24 / 32 in both themes, stands out of the panel of that
/// theme, and a coloured glyph changes with the theme.
#[test]
fn ribbon_icons_render_in_both_themes() {
    let t = Tokens::embedded().unwrap();
    let mut bad = Vec::new();
    for n in names() {
        let mono = is_mono(svg(DIR, 24, n).unwrap());
        for px in [16u32, 20, 24, 32] {
            let mut imgs = Vec::new();
            for dark in [false, true] {
                let img = rasterize_as(DIR, n, px, Layer::Colour(dark))
                    .unwrap_or_else(|e| panic!("{n}@{px}: {e:?}"));
                assert_eq!(img.size, [px as usize, px as usize]);
                let ink = img.pixels.iter().filter(|p| p.a() > 0).count();
                let frac = ink as f32 / (px * px) as f32;
                if !(frac > 0.03 && frac < 0.85) {
                    bad.push(format!("{n}@{px} dark={dark}: ink {frac:.2}"));
                }
                if !mono {
                    let mode = if dark { Mode::Dark } else { Mode::Light };
                    let bg = t.color(mode, "bg.panel").unwrap();
                    let lb = lum(bg.r(), bg.g(), bg.b());
                    let vis = img
                        .pixels
                        .iter()
                        .filter(|p| p.a() > 153)
                        .filter(|p| {
                            let a = p.a() as f32 / 255.0;
                            let un = |c: u8| ((c as f32 / a).round().min(255.0)) as u8;
                            let lp = lum(un(p.r()), un(p.g()), un(p.b()));
                            (lp.max(lb) + 0.05) / (lp.min(lb) + 0.05) >= 2.2
                        })
                        .count();
                    if (vis as f32) < px as f32 * 0.75 {
                        bad.push(format!("{n}@{px} dark={dark}: {vis} px stand out"));
                    }
                }
                imgs.push(img);
            }
            if !mono && imgs[0].pixels == imgs[1].pixels {
                bad.push(format!("{n}@{px}: same in both themes"));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// The disabled look is grey and keeps the coverage of the glyph.
#[test]
fn disabled_ribbon_glyph_is_grey() {
    for n in ["wall", "door", "spark", "delete", "dimension"] {
        for dark in [false, true] {
            let c = rasterize_as(DIR, n, 20, Layer::Colour(dark)).unwrap();
            let g = rasterize_as(DIR, n, 20, Layer::Gray(dark)).unwrap();
            assert!(g.pixels.iter().all(|p| p.r() == p.g() && p.g() == p.b()), "{n}");
            assert!(c.pixels.iter().zip(&g.pixels).all(|(a, b)| a.a() == b.a()), "{n}");
            assert!(c.pixels.iter().any(|p| p.r() != p.b()), "{n}: coloured at rest");
        }
    }
    let m = rasterize_as(DIR, "wall", 20, Layer::All).unwrap();
    assert!(m.pixels.iter().all(|p| p.r() == p.a()), "the All layer is a white mask");
}

#[test]
fn ribbon_icons_stay_inside_the_live_field() {
    for n in names() {
        let c = rasterize_as(DIR, n, 20, Layer::All).unwrap();
        let max = (0..400usize)
            .filter(|i| i % 20 == 0 || i % 20 == 19 || i / 20 == 0 || i / 20 == 19)
            .map(|i| c.pixels[i].a())
            .max()
            .unwrap_or(0);
        assert!(max < 26, "{n}: ink {max} on the 1 px margin at 20");
    }
}

/// No two glyphs collapse into one at the compact (16) and ribbon (20) sizes, compared in
/// colour: the role colour is part of what tells them apart.
#[test]
fn ribbon_icons_differ_at_small_sizes() {
    for px in [16u32, 20] {
        let imgs: Vec<(&str, Vec<f32>)> = names()
            .iter()
            .map(|n| {
                let img = rasterize_as(DIR, n, px, Layer::Colour(false)).unwrap();
                let v = img
                    .pixels
                    .iter()
                    .flat_map(|p| [p.r(), p.g(), p.b(), p.a()])
                    .map(|c| c as f32 / 255.0)
                    .collect();
                (*n, v)
            })
            .collect();
        let mut bad = Vec::new();
        for (i, (a, ma)) in imgs.iter().enumerate() {
            for (b, mb) in &imgs[i + 1..] {
                let (mut diff, mut union) = (0.0f32, 0.0f32);
                for (x, y) in ma.iter().zip(mb) {
                    diff += (x - y).abs();
                    union += x.max(*y);
                }
                let d = diff / union.max(f32::EPSILON);
                if d < 0.03 {
                    bad.push(format!("{a} ~ {b}: {d:.3}"));
                }
            }
        }
        assert!(bad.is_empty(), "look alike at {px} px: {bad:?}");
    }
}

/// Fingerprints `(count, FNV-1a 64 of "%.3f;" per number)` of number runs of path data that came
/// from a third-party icon set (the former select arrow, undo and redo, every master and source
/// form). The set is drawn in-house: no SVG and no generator source may carry them again.
const FOREIGN_PATHS: &[(usize, u64)] = &[
    (6, 0x8f8d40b0d8f24d03),
    (14, 0x28bd215e3aaa2826),
    (14, 0x3a9da23735b4a9cb),
    (14, 0x3b44f010e66aac2b),
    (14, 0x7e60328baae961fd),
    (14, 0xcb4e5348dfe9d444),
    (14, 0xe7b2acadd2403b73),
    (17, 0x482833bf737b3027),
    (17, 0x51049e36573f4cbb),
    (17, 0x6477045790d4c893),
    (17, 0x7841906979c20cda),
    (17, 0xa071aad4bb9b1de3),
    (17, 0xa32ed94870381fa1),
];

/// Every number of `s` in order (`-?digits(.digits)?`, as the fingerprints were taken).
fn numbers(s: &str) -> Vec<f64> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let neg = b[i] == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit);
        if !(neg || b[i].is_ascii_digit()) {
            i += 1;
            continue;
        }
        let start = i;
        i += usize::from(neg);
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        out.push(s[start..i].parse().unwrap());
    }
    out
}

fn fnv(nums: &[f64]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for n in nums {
        for c in format!("{n:.3};").bytes() {
            h ^= c as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn foreign_runs(name: &str, s: &str, bad: &mut Vec<String>) {
    let nums = numbers(s);
    for (len, hash) in FOREIGN_PATHS {
        if nums.windows(*len).any(|w| fnv(w) == *hash) {
            bad.push(format!("{name}: path data {hash:016x}"));
        }
    }
}

#[test]
fn no_third_party_path_data_in_the_sets_or_the_generator() {
    assert_eq!(fnv(&[1.0, 2.5]), fnv(&numbers("M1 2.5")));
    assert_eq!(numbers("x-1.25,3 v4.0"), vec![-1.25, 3.0, 4.0]);
    let mut bad = Vec::new();
    for (key, s) in crate::fragments::ICONS_V4.iter().chain(crate::fragments::ICONS_RIBBON) {
        for d in s.split(" d=\"").skip(1) {
            foreign_runs(key, d.split('"').next().unwrap_or(""), &mut bad);
        }
    }
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/icons");
    let mut py = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "py") {
            py += 1;
            let src = std::fs::read_to_string(&p).unwrap();
            foreign_runs(&p.display().to_string(), &src, &mut bad);
        }
    }
    assert!(py >= 7, "generator sources: {py}");
    assert!(bad.is_empty(), "third-party path data: {bad:?}");
}

/// Oblique building elements: the 16 master is its own drawing (whole-pixel edges), not the
/// 24 file scaled down.
#[test]
fn ribbon_has_its_own_16_master() {
    for n in ["wall", "floor", "column", "beam", "stair", "roof", "door", "window", "dimension"] {
        let s16 = svg(DIR, 16, n).unwrap();
        let s24 = svg(DIR, 24, n).unwrap();
        let own = rasterize_colour(s16, 16, false).unwrap();
        let scaled = rasterize_colour(s24, 16, false).unwrap();
        assert_ne!(own.pixels, scaled.pixels, "{n}");
        let part = |img: &ColorImage| img.pixels.iter().filter(|p| p.a() > 0 && p.a() < 255).count();
        let (a, b) = (part(&own), part(&scaled));
        assert!(a * 3 < b * 2, "{n}@16 blurs: {a} partial pixels, the scaled 24 has {b}");
    }
}
