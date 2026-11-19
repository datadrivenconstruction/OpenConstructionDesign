//! Tests of the generated icon set (ICONS_V4.md); `tools/icons/check.py` runs the same bars
//! before a regeneration is committed.
use super::*;

const BASE: [&str; 3] = ["outline", "duotone", "filled"];

fn coverage(svg: &str, px: u32) -> Vec<f32> {
    let img = rasterize_svg(svg, px).unwrap_or_else(|e| panic!("{e:?}"));
    img.pixels.iter().map(|p| p.a() as f32 / 255.0).collect()
}

/// Coverage of `name` in `dir` at `px` from the master `master_for(px)` picks.
fn mask(dir: Direction, name: &str, px: u32) -> Vec<f32> {
    let svg = svg(dir, master_for(px), name).unwrap_or_else(|| panic!("{name}: no file"));
    coverage(svg, px)
}

fn share_differing(a: &[f32], b: &[f32]) -> f32 {
    let (mut diff, mut union) = (0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        diff += (x - y).abs();
        union += x.max(*y);
    }
    diff / union.max(f32::EPSILON)
}

fn svg_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            if p.is_dir() {
                svg_files(&p)
            } else {
                usize::from(p.extension().is_some_and(|x| x == "svg"))
            }
        })
        .sum()
}

#[test]
fn every_file_is_embedded_and_every_name_has_all_masters() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/design/icons_v4");
    assert_eq!(
        crate::fragments::ICONS_V4.len(),
        svg_files(std::path::Path::new(dir)),
        "every svg under docs/design/icons_v4 is embedded"
    );
    assert!(names().len() >= 400, "set shrank: {}", names().len());
    let mut missing = Vec::new();
    for n in names() {
        for d in Direction::ALL {
            for m in MASTERS {
                if svg(d, m, n).is_none() {
                    missing.push(format!("{}/{m}/{n}", d.dir()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "size masters missing: {missing:?}");
    assert_eq!(crate::fragments::ICONS_V4.len(), names().len() * 9);
}

#[test]
fn icons_rasterize() {
    let mut bad = Vec::new();
    for n in names() {
        for d in Direction::ALL {
            // the filled variant is denser by nature
            // filled and duotone cover their key surface; the outline bar keeps line glyphs light
            let max = if d == Direction::Outline { 0.7 } else { 0.8 };
            for px in [16, 20, 30, 40] {
                let img = rasterize_as(d, n, px, Layer::All)
                    .unwrap_or_else(|e| panic!("{n}@{px}: {e:?}"));
                assert_eq!(img.size, [px as usize, px as usize]);
                let ink = img.pixels.iter().filter(|p| p.a() > 0).count();
                let frac = ink as f32 / (px * px) as f32;
                if !(frac > 0.03 && frac < max) {
                    bad.push(format!("{}/{n}@{px}: ink {frac:.2}", d.dir()));
                }
                // white mask (premultiplied: r == a), the duotone accent included
                assert!(img.pixels.iter().all(|p| p.r() == p.a()), "{n}");
            }
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
    assert!(matches!(rasterize("nope", 16), Err(IconError::Unknown(_))));
    assert!(rasterize_svg("<svg", 16).is_err());
}

#[test]
#[cfg(not(debug_assertions))] // wall-clock budget is meaningless in a loaded debug build
fn icon_raster_budget() {
    let t = std::time::Instant::now();
    for name in names() {
        rasterize(name, 24).unwrap();
    }
    let per = t.elapsed().as_secs_f64() / names().len() as f64;
    assert!(per < 0.02, "{:.3} ms per icon", per * 1e3);
}

#[test]
fn every_command_has_an_embedded_icon() {
    let r = crate::Registry::builtin();
    let missing: Vec<_> = r
        .all()
        .iter()
        .filter(|c| !c.icon.is_empty() && source(&c.icon).is_none())
        .map(|c| format!("{} '{}'", c.id, c.icon))
        .collect();
    assert!(missing.is_empty(), "commands without icon: {missing:?}");
    assert!(placeholder(16).pixels.iter().any(|p| p.a() > 0));
}

/// Ribbon (R), context bar (C) and radial (M) commands never fall back to the dashed
/// placeholder: every direction at every master.
#[test]
fn ribbon_commands_have_icons() {
    let mut bad = Vec::new();
    for c in crate::Registry::builtin().all() {
        let shown = c
            .surfaces
            .iter()
            .any(|s| matches!(s.as_str(), "R" | "C" | "M"));
        if !shown {
            continue;
        }
        let all = Direction::ALL
            .iter()
            .all(|d| MASTERS.iter().all(|m| svg(*d, *m, &c.icon).is_some()));
        if c.icon.is_empty() || !all {
            bad.push(format!("{} '{}'", c.id, c.icon));
        }
    }
    assert!(bad.is_empty(), "ribbon commands without a full icon: {bad:?}");
}

/// Chrome close buttons draw this icon instead of a font glyph.
#[test]
fn close_svg_embedded() {
    let svg24 = source("close").expect("close icon");
    assert!(svg24.contains(r#"viewBox="0 0 24 24""#));
    let img = rasterize("close", 16).unwrap();
    assert!(img.pixels.iter().any(|p| p.a() > 0));
}

/// One tool on two tabs without an `alias` link: the only allowed shares besides aliases.
const SAME_TOOL: &[(&str, &str)] = &[
    ("mass.extrude", "arch.solid_extrusion"),
    ("mass.blend", "arch.solid_blend"),
    ("mass.revolve", "arch.solid_revolve"),
    ("mass.sweep", "arch.solid_sweep"),
    ("mass.swept_blend", "arch.solid_swept_blend"),
    ("arch.section", "view.section"),
    ("view.section_box_fit", "view.section_box"),
    ("ctx.mep.fitting", "mep.fitting"),
    // the Data tab repeats the File menu entries
    ("data.export_ifc", "file.save_ifc"),
    ("file.save_ifc_as", "file.save_ifc"),
    ("mgr.import_cad", "file.import"),
    ("mgr.level", "arch.level"),
    // one "Unjoin geometry" on the Modify and Massing tabs
    ("mod.unjoin", "mass.unjoin"),
    // view settings and update notes share a family glyph
    ("view.range", "view.filters"),
    ("help.check_update", "help.whats_new"),
    // the Help panel opens the same key list as the Manage tab
    ("help.shortcuts", "mgr.keymap"),
    // the assistant: its panel and "ask" are one tool
    ("ai.panel", "ai.ask"),
];

#[test]
fn different_commands_do_not_share_an_icon() {
    let r = crate::Registry::builtin();
    let by_id: HashMap<&str, &crate::commands::Command> =
        r.all().iter().map(|c| (c.id.as_str(), c)).collect();
    let tool = |id: &str| -> String {
        let same = SAME_TOOL.iter().find(|(a, _)| *a == id).map(|(_, b)| *b);
        let alias = by_id.get(id).and_then(|c| c.alias.as_deref());
        same.or(alias).unwrap_or(id).to_string()
    };
    let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
    for c in r.all() {
        if !c.icon.is_empty() {
            owners
                .entry(c.icon.as_str())
                .or_default()
                .push(c.id.as_str());
        }
    }
    let mut bad = Vec::new();
    for (icon, ids) in &owners {
        let first = tool(ids[0]);
        if ids.iter().any(|id| tool(*id) != first) {
            bad.push(format!("{icon}: {ids:?}"));
        }
    }
    bad.sort();
    assert!(bad.is_empty(), "draw a new icon or link by alias: {bad:?}");
    for c in r.all() {
        let target = c.alias.as_deref().and_then(|a| by_id.get(a));
        if let Some(t) = target.filter(|_| !c.icon.is_empty()) {
            assert_eq!(c.icon, t.icon, "{} must look like its alias {}", c.id, t.id);
        }
    }
}

/// One weight per master on the root, tinted by the theme, no text; colours only in masks and
/// (duotone) the accent the app re-tints.
#[test]
fn icon_style_is_uniform() {
    for (key, s) in crate::fragments::ICONS_V4 {
        let mut parts = key.split('/');
        let (dir, m) = (parts.next().unwrap(), parts.next().unwrap());
        assert!(BASE.contains(&dir), "{key}");
        assert!(s.contains(&format!(r#"viewBox="0 0 {m} {m}""#)), "{key}: grid");
        let w = if m == "16" { "1" } else { "1.5" };
        assert!(
            s.starts_with("<svg") && s.contains(&format!(r#"stroke-width="{w}""#)),
            "{key}: weight {w}"
        );
        assert!(s.contains("currentColor"), "{key}: tinted by theme");
        assert!(!s.contains("<text") && !s.contains("style="), "{key}");
        let colours = s.matches('#').count();
        let allowed = s.matches("#000").count()
            + s.matches("#fff").count()
            + s.matches(FILE_ACCENT).count()
            + s.matches("url(#").count();
        assert_eq!(colours, allowed, "{key}: a fixed colour");
        assert!(dir == "duotone" || !s.contains(FILE_ACCENT), "{key}: accent outside B");
    }
}


#[test]
fn icons_stay_inside_the_live_field() {
    for n in names() {
        for d in Direction::ALL {
            let c = mask(d, n, 20);
            let edge = (0..400usize)
                .filter(|i| i % 20 == 0 || i % 20 == 19 || i / 20 == 0 || i / 20 == 19);
            let max = edge.map(|i| c[i]).fold(0.0, f32::max);
            assert!(max < 0.1, "{}/{n}: ink {max:.2} on the 1 px margin", d.dir());
        }
    }
}

fn all_masks(d: Direction, px: u32) -> Vec<(&'static str, Vec<f32>)> {
    names().iter().map(|n| (*n, mask(d, n, px))).collect()
}

/// Glyphs drawn the same on purpose (one AI glyph); mirrors `TWINS` in tools/icons/check.py.
const TWINS: &[(&str, &str)] = &[("spark", "ai-chat")];

fn alike(m: &[(&str, Vec<f32>)], bar: f32) -> Vec<String> {
    let twin = |a: &str, b: &str| TWINS.iter().any(|t| *t == (a, b) || *t == (b, a));
    let mut bad = Vec::new();
    for (i, (a, ma)) in m.iter().enumerate() {
        for (b, mb) in &m[i + 1..] {
            if twin(*a, *b) {
                continue;
            }
            let d = share_differing(ma, mb);
            if d < bar {
                bad.push(format!("{a} ~ {b}: {d:.3}"));
            }
        }
    }
    bad
}

/// Share of differing ink at ribbon size; twins of one family stay above the bar.
#[test]
fn icons_differ_at_ribbon_size() {
    for d in [Direction::Outline, Direction::Filled] {
        let bad = alike(&all_masks(d, 20), 0.08);
        assert!(bad.is_empty(), "{} look alike at 20 px: {bad:?}", d.dir());
    }
}

/// Compact ribbon draws the 16 master: no two icons may collapse into one blob there.
#[test]
fn icons_differ_at_compact_size() {
    let bad = alike(&all_masks(Direction::Outline, 16), 0.04);
    assert!(bad.is_empty(), "look alike at 16 px: {bad:?}");
}

/// The active variant reads differently from the outline; a line-only glyph (no surface to
/// fill: undo, section, level arrows) keeps the outline file unchanged by the rules.
#[test]
fn filled_variant_differs_from_outline() {
    let mut bad = Vec::new();
    for n in names() {
        for m in MASTERS {
            let (o, f) = (
                svg(Direction::Outline, m, n).unwrap(),
                svg(Direction::Filled, m, n).unwrap(),
            );
            if o == f {
                // a surface is what duotone tints at 20 %; solid marks and badge halos are not
                let duo = svg(Direction::Duotone, m, n).unwrap();
                assert!(
                    !duo.contains("fill-opacity"),
                    "{n}@{m}: has a surface but no filled drawing"
                );
                continue;
            }
            let d = share_differing(&coverage(o, m), &coverage(f, m));
            if d < 0.02 {
                bad.push(format!("{n}@{m}: {d:.3}"));
            }
        }
    }
    assert!(bad.is_empty(), "fill looks like the outline: {bad:?}");
}

/// Roof commands sit side by side in one menu: a stricter bar at both sizes.
#[test]
fn roof_icons_are_distinct_at_16_and_20() {
    let roofs: Vec<&str> = names()
        .iter()
        .copied()
        .filter(|n| n.starts_with("roof"))
        .collect();
    assert!(roofs.len() >= 14, "roof family shrank: {}", roofs.len());
    for px in [16u32, 20] {
        let m: Vec<_> = roofs
            .iter()
            .map(|n| (*n, mask(Direction::Outline, n, px)))
            .collect();
        let bad = alike(&m, 0.15);
        assert!(bad.is_empty(), "roof glyphs alike at {px} px: {bad:?}");
    }
}

/// Roof tiles carry a short label of their own, unique within the family, so the type
/// reads without the tooltip.
#[test]
fn roof_commands_have_short_labels() {
    let r = crate::Registry::builtin();
    for (lang, name) in [(crate::Lang::En, "en"), (crate::Lang::Ru, "ru")] {
        let cat = crate::Catalog::new(lang);
        let mut seen: HashMap<String, String> = HashMap::new();
        for c in r
            .all()
            .iter()
            .filter(|c| c.icon.starts_with("roof") && c.id != "arch.roof")
        {
            let key = format!("{}.short", c.label_key());
            assert!(cat.t(&key) != key, "{name} {}: no short label", c.id);
            let l = crate::widgets::ribbon::short_label(&cat, c);
            assert!(l.chars().count() <= 16, "{name} {}: '{l}' too long", c.id);
            if let Some(prev) = seen.insert(l.clone(), c.id.clone()) {
                panic!("{name}: '{l}' used by {prev} and {}", c.id);
            }
        }
    }
}

/// 20 px is the 20 master 1:1, never the 24 master shrunk by 5/6.
#[test]
fn size_picks_its_optical_master() {
    for (px, m) in [(14, 16), (16, 16), (19, 16), (20, 20), (23, 20), (24, 24), (48, 24)] {
        assert_eq!(master_for(px), m, "{px}");
    }
    for n in ["wall", "select", "close"] {
        let s20 = svg(Direction::Outline, 20, n).unwrap();
        let img = rasterize_as(Direction::Outline, n, 20, Layer::All).unwrap();
        assert_eq!(img.pixels, rasterize_svg(s20, 20).unwrap().pixels, "{n}");
        let s24 = svg(Direction::Outline, 24, n).unwrap();
        assert_ne!(img.pixels, rasterize_svg(s24, 20).unwrap().pixels, "{n}");
    }
}

/// `(kind, fixed coordinate, from, to)` of the axis-aligned runs in a path (M L H V A Q Z h).
fn axis_runs(d: &str) -> Vec<(char, f32, f32, f32)> {
    let mut toks: Vec<(char, Vec<f32>)> = Vec::new();
    for (i, ch) in d.char_indices() {
        if "MLHVAQZh".contains(ch) {
            let rest = &d[i + 1..];
            let end = rest.find(|c: char| "MLHVAQZh".contains(c)).unwrap_or(rest.len());
            let nums = rest[..end]
                .split([' ', ','])
                .filter(|s| !s.is_empty())
                .map(|s| s.parse::<f32>().unwrap())
                .collect();
            toks.push((ch, nums));
        }
    }
    let (mut out, mut x, mut y, mut sx, mut sy) = (Vec::new(), 0f32, 0f32, 0f32, 0f32);
    let mut seg = |out: &mut Vec<_>, x0: f32, y0: f32, x1: f32, y1: f32| {
        if (y1 - y0).abs() < 1e-4 && (x1 - x0).abs() > 1e-4 {
            out.push(('H', y0, x0.min(x1), x0.max(x1)));
        } else if (x1 - x0).abs() < 1e-4 && (y1 - y0).abs() > 1e-4 {
            out.push(('V', x0, y0.min(y1), y0.max(y1)));
        }
    };
    for (c, v) in toks {
        match c {
            'M' => {
                (x, y) = (v[0], v[1]);
                (sx, sy) = (x, y);
            }
            'L' => {
                seg(&mut out, x, y, v[0], v[1]);
                (x, y) = (v[0], v[1]);
            }
            'H' => {
                seg(&mut out, x, y, v[0], y);
                x = v[0];
            }
            'V' => {
                seg(&mut out, x, y, x, v[0]);
                y = v[0];
            }
            'A' => (x, y) = (v[5], v[6]),
            'Q' => (x, y) = (v[2], v[3]),
            'h' => x += v[0],
            _ => {
                seg(&mut out, x, y, sx, sy);
                (x, y) = (sx, sy);
            }
        }
    }
    out
}

/// The stroked paths of a file, outside masks.
/// Pixels of a 20-master drawing that the badge halo knocks out of the base (the black paths of
/// its mask): the design removes ink there, so the edge test samples only the strokes it keeps.
fn halo_pixels(svg20: &str) -> Vec<bool> {
    let (Some(a), Some(b)) = (svg20.find("<mask"), svg20.find("</mask>")) else {
        return vec![false; 400];
    };
    let body = &svg20[a..b];
    let body = &body[body.find('>').unwrap() + 1..];
    let cut: String = body
        .split('<')
        .filter(|t| t.starts_with("path "))
        .map(|t| format!("<{}", t.replace("#000", "#fff")))
        .collect();
    let s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"20\" \
         viewBox=\"0 0 20 20\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{cut}</svg>"
    );
    coverage(&s, 20).iter().map(|v| *v > 0.02).collect()
}

fn stroked_paths(svg: &str) -> Vec<String> {
    let mut s = svg.to_string();
    while let (Some(a), Some(b)) = (s.find("<mask"), s.find("</mask>")) {
        s.replace_range(a..b + 7, "");
    }
    s.split("<path ")
        .skip(1)
        .map(|p| &p[..p.find("/>").unwrap()])
        .filter(|a| !a.contains("fill=") && !a.contains(r#"stroke="none""#))
        .map(|a| {
            let i = a.find("d=\"").unwrap() + 3;
            a[i..i + a[i..].find('"').unwrap()].to_string()
        })
        .collect()
}

/// Rendered at 20 px, every horizontal / vertical stroke of 3 px or more puts its outer edge on
/// a whole pixel: the pixel inside that edge is solid along the run (clear of corner joins) and
/// the one outside it is clean somewhere along it. A 24 master shrunk to 20 fails both.
#[test]
fn stroke_edges_land_on_whole_pixels() {
    let mut bad = Vec::new();
    let mut runs = 0;
    for n in names() {
        let svg20 = svg(Direction::Outline, 20, n).unwrap();
        let img = coverage(svg20, 20);
        let halo = halo_pixels(svg20);
        let at = |row: i32, col: i32| -> Option<f32> {
            ((0..20).contains(&row) && (0..20).contains(&col))
                .then(|| img[(row * 20 + col) as usize])
        };
        let cut = |row: i32, col: i32| {
            (0..20).contains(&row) && (0..20).contains(&col) && halo[(row * 20 + col) as usize]
        };
        for d in stroked_paths(svg20) {
            for (kind, c, a, b) in axis_runs(&d) {
                if b - a < 3.0 {
                    continue;
                }
                let f = ((c.rem_euclid(1.0)) * 100.0).round() as i32;
                let (inside, outside) = match f {
                    75 => ((c - 0.75) as i32, (c - 0.75) as i32 - 1),
                    25 => ((c + 0.75) as i32 - 1, (c + 0.75) as i32),
                    _ => {
                        bad.push(format!("{n}: {kind} at {c} off the pixel grid"));
                        continue;
                    }
                };
                let (t0, t1) = ((a + 1.5).ceil() as i32, (b - 1.5).floor() as i32);
                if t0 >= t1 {
                    continue;
                }
                let (mut full, mut clean, mut seen) = (true, false, 0);
                for t in t0..t1 {
                    let cell = |k: i32| if kind == 'H' { (k, t) } else { (t, k) };
                    let ((ri, ci), (ro, co)) = (cell(inside), cell(outside));
                    if cut(ri, ci) || cut(ro, co) {
                        continue;
                    }
                    seen += 1;
                    let px = |k: i32| if kind == 'H' { at(k, t) } else { at(t, k) };
                    if px(inside).is_some_and(|v| v < 0.9) {
                        full = false;
                    }
                    if px(outside).is_none_or(|v| v <= 0.1) {
                        clean = true;
                    }
                }
                if seen == 0 {
                    continue;
                }
                runs += 1;
                if !full || !clean {
                    bad.push(format!("{n}: {kind} {c} [{a}, {b}] full {full} clean {clean}"));
                }
            }
        }
    }
    assert!(runs > 1000, "too few runs sampled: {runs}");
    assert!(bad.is_empty(), "strokes off whole pixels at 20 px: {bad:?}");
}

#[test]
fn style_is_one_data_switch() {
    let st = style();
    assert_eq!(
        st,
        Style {
            base: Direction::Duotone,
            active: Direction::Duotone,
            large: Direction::Duotone
        },
        "icon style: the two-tone set (A3) is the default everywhere"
    );
    assert_eq!(resolve("wall", 20), (Direction::Duotone, "wall"));
    assert_eq!(resolve("wall.fill", 20), (Direction::Duotone, "wall"));
    assert_eq!(resolve("wall", 48), (Direction::Duotone, "wall"));
    let studio = set("studio").unwrap().style();
    assert_eq!(resolve_with(studio, "wall", 20), (Direction::Outline, "wall"));
    assert_eq!(resolve_with(studio, "wall.fill", 20), (Direction::Filled, "wall"));
    assert_eq!(resolve_with(studio, "wall", 48), (Direction::Duotone, "wall"));
    let b: Style =
        serde_json::from_str(r#"{"base":"duotone","active":"duotone","large":"duotone"}"#).unwrap();
    for (n, px) in [("wall", 16), ("wall.fill", 20), ("wall", 32)] {
        assert_eq!(resolve_with(b, n, px).0, Direction::Duotone, "{n}@{px}");
    }
    assert!(serde_json::from_str::<Style>(r#"{"base":"outline"}"#).is_err());
}

/// Duotone paints in two passes: ink without the accent, then the accent alone; together they
/// cover what the whole file covers.
#[test]
fn duotone_splits_into_ink_and_accent() {
    let mut with_accent = 0;
    for n in names() {
        let s = svg(Direction::Duotone, 20, n).unwrap();
        let ink = split_layer(s, Layer::Ink);
        let acc = split_layer(s, Layer::Accent);
        assert!(!ink.contains(FILE_ACCENT), "{n}");
        assert!(ink.starts_with("<svg") && acc.ends_with("</svg>\n"), "{n}");
        if !s.contains(FILE_ACCENT) {
            continue;
        }
        with_accent += 1;
        let (all, i, a) = (coverage(s, 20), coverage(&ink, 20), coverage(&acc, 20));
        assert!(a.iter().any(|v| *v > 0.0), "{n}: empty accent");
        for k in 0..400 {
            let over = 1.0 - (1.0 - i[k]) * (1.0 - a[k]);
            assert!((over - all[k]).abs() < 0.06, "{n} px {k}: {over} vs {}", all[k]);
        }
    }
    assert!(with_accent > 100, "duotone surfaces: {with_accent}");
}

#[test]
fn tint_roles_follow_the_theme() {
    for mode in [crate::theme::Mode::Light, crate::theme::Mode::Dark] {
        let th = crate::Theme::new(mode, crate::theme::Density::Compact);
        let ink = tint(&th, Tint::Rest);
        assert_eq!(ink, th.c("text.primary"));
        assert_eq!(tint(&th, Tint::Active), th.c("accent"));
        let off = tint(&th, Tint::Disabled);
        let want = (ink.a() as f32 * DISABLED_INK).round() as i32;
        assert!((off.a() as i32 - want).abs() <= 1, "{mode:?}: {off:?}");
    }
    let light = crate::Theme::new(crate::theme::Mode::Light, crate::theme::Density::Compact);
    let dark = crate::Theme::new(crate::theme::Mode::Dark, crate::theme::Density::Compact);
    assert_ne!(tint(&light, Tint::Rest), tint(&dark, Tint::Rest));
}
