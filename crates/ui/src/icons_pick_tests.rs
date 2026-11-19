// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tests of the six pickable icon styles (`data/icon_style.json`; the style pack is written by
//! `tools/icons/styles.py`): complete for every command and glyph, drawn in both themes.
use super::*;

const PICKABLE: [&str; 6] = ["duotone_all", "outline", "color", "hierarchical", "isometric", "monoline"];

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

fn covered(img: &ColorImage) -> usize {
    img.pixels.iter().filter(|p| p.a() > 0).count()
}

/// One image of `name` in `dir` at `px` as a theme draws it: role colours for coloured files,
/// the ink mask for the rest (the caller tints it with the theme's text colour).
fn drawn(dir: Direction, name: &str, px: u32, dark: bool) -> Result<ColorImage, IconError> {
    if dir.is_colour() {
        let s = svg(dir, master_for(px), name).ok_or_else(|| IconError::Unknown(name.into()))?;
        rasterize_colour(s, px, dark)
    } else {
        rasterize_as(dir, name, px, Layer::All)
    }
}

#[test]
fn six_styles_are_offered_and_a3_is_the_default() {
    let ids: Vec<&str> = sets().iter().map(|s| s.id).collect();
    assert_eq!(ids, PICKABLE);
    assert_eq!(style(), set("duotone_all").unwrap().style());
    assert_eq!(style().base, Direction::Duotone);
    // the retired sets still resolve, so saved preferences and looks keep working
    for id in ["ribbon", "studio", "duotone", "filled", "outline_duotone", "filled_outline"] {
        assert!(set(id).is_some_and(|s| s.legacy), "{id}");
        assert!(!ids.contains(&id), "{id} is not offered");
    }
    assert_eq!(all_sets().len(), 12);
}

#[test]
fn style_pack_is_embedded_whole() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons_pick");
    let on_disk = svg_count(std::path::Path::new(dir));
    assert_eq!(crate::fragments::ICONS_PICK.len(), on_disk, "every svg under assets/icons_pick is embedded");
    assert_eq!(on_disk, 4 * 3 * names().len());
}

/// Every command with an icon slot and every glyph of the set exists at every master in every
/// pickable style, and the set's own use (base, active, large) resolves to a file.
#[test]
fn every_command_has_an_icon_in_every_style() {
    let mut bad = Vec::new();
    let mut n = 0;
    for s in sets() {
        for c in crate::Registry::builtin().all() {
            if c.icon.is_empty() {
                continue;
            }
            if s.id == PICKABLE[0] {
                n += 1;
            }
            for px in [16, 20, 24, 48] {
                let (d, bare) = resolve_with(s.style(), &c.icon, px);
                if svg(d, master_for(px), bare).is_none() {
                    bad.push(format!("{} {} '{}' @{px}", s.id, c.id, c.icon));
                }
            }
        }
        for name in names() {
            for d in [s.base, s.active, s.large] {
                for m in MASTERS {
                    if svg(d, m, name).is_none() {
                        bad.push(format!("{} {}/{m}/{name}", s.id, d.dir()));
                    }
                }
            }
        }
    }
    assert!(n > 300, "commands with icons: {n}");
    assert_eq!(names().len(), 439);
    assert!(bad.is_empty(), "{} gaps, first: {:?}", bad.len(), &bad[..bad.len().min(10)]);
}

/// Every glyph of every pickable style draws something in the light and the dark theme; a
/// coloured style really swaps its role colours for the dark theme.
#[test]
fn every_style_draws_every_glyph_in_light_and_dark() {
    let mut blank = Vec::new();
    let mut swapped = 0;
    let mut coloured = 0;
    for s in sets() {
        let d = s.base;
        for name in names() {
            let light = drawn(d, name, 20, false).unwrap_or_else(|e| panic!("{} {name}: {e:?}", s.id));
            let dark = drawn(d, name, 20, true).unwrap_or_else(|e| panic!("{} {name}: {e:?}", s.id));
            if covered(&light) == 0 || covered(&dark) == 0 {
                blank.push(format!("{}/{name}", s.id));
            }
            if d.is_colour() {
                coloured += 1;
                swapped += usize::from(light.pixels != dark.pixels);
            }
        }
    }
    assert!(blank.is_empty(), "blank glyphs: {blank:?}");
    assert!(coloured > 1000 && swapped * 10 > coloured * 9, "dark swap: {swapped}/{coloured}");
}

/// The style pack uses only role tokens (swapped in the dark theme) or the ink of the caller;
/// no file keeps the two-tone file accent.
#[test]
fn style_pack_colours_are_role_tokens() {
    let light: Vec<&str> = role_colours(false).iter().map(|(l, _)| l.as_str()).collect();
    for (key, s) in crate::fragments::ICONS_PICK {
        assert!(!s.contains(FILE_ACCENT), "{key}");
        assert!(!s.contains("<text") && !s.contains("style="), "{key}");
        let colour = !key.starts_with("hierarchical/");
        assert_eq!(is_mono(s), !colour, "{key}: ink or colour, not both");
        for part in s.split('#').skip(1) {
            let hex: String = part.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if !part[hex.len()..].starts_with('"') || hex == "000" || hex == "fff" {
                continue; // a mask id reference, or the mask colours
            }
            let hex = format!("#{hex}");
            assert!(light.iter().any(|v| *v == hex), "{key}: {hex} is not a role token");
        }
    }
}
