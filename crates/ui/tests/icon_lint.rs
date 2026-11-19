// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Icon set contract (tools/icons/check.py is the geometry audit): per master one stroke width,
//! viewBox = master, round caps/joins, no raster, kebab-case names, dots are circles, and every
//! icon name referenced from source exists in the set.
use ddc_ui::icons::{self, Direction};
use std::path::{Path, PathBuf};

/// Stroke width of the root `<svg>` per master (physical px).
const WIDTH: [(u32, &str); 3] = [(16, "1"), (20, "1.5"), (24, "1.5")];

fn root_attr<'a>(svg: &'a str, key: &str) -> Option<&'a str> {
    let head = &svg[..svg.find('>')?];
    let i = head.find(&format!(" {key}=\""))? + key.len() + 3;
    Some(&head[i..i + head[i..].find('"')?])
}

#[test]
fn every_file_follows_the_grid_contract() {
    let mut bad = Vec::new();
    for &name in icons::names() {
        let ok = !name.is_empty()
            && name.split('-').all(|p| {
                !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            });
        if !ok {
            bad.push(format!("{name}: name is not kebab-case"));
        }
        for dir in Direction::ALL {
            for (m, w) in WIDTH {
                let Some(svg) = icons::svg(dir, m, name) else {
                    bad.push(format!("{}/{m}/{name}: missing", dir.dir()));
                    continue;
                };
                let id = format!("{}/{m}/{name}", dir.dir());
                let vb = format!("0 0 {m} {m}");
                if root_attr(svg, "viewBox") != Some(vb.as_str()) {
                    bad.push(format!("{id}: viewBox"));
                }
                if root_attr(svg, "stroke-width") != Some(w) {
                    bad.push(format!("{id}: root stroke-width != {w}"));
                }
                if root_attr(svg, "stroke-linecap") != Some("round")
                    || root_attr(svg, "stroke-linejoin") != Some("round")
                {
                    bad.push(format!("{id}: caps/joins"));
                }
                for raster in ["<image", "data:", "base64", "<foreignObject", "<text"] {
                    if svg.contains(raster) {
                        bad.push(format!("{id}: contains {raster}"));
                    }
                }
                // Other stroke widths exist only as knock-out halos inside a <mask>.
                let outside_masks = strip_masks(svg);
                if outside_masks[outside_masks.find('>').unwrap_or(0)..].contains("stroke-width=") {
                    bad.push(format!("{id}: stroke-width outside the root"));
                }
                // A dot is a filled circle, not a zero-length stroke.
                if svg.contains("h0.01") {
                    bad.push(format!("{id}: zero-length stroke dot"));
                }
                if dir == Direction::Outline && root_attr(svg, "fill") != Some("none") {
                    bad.push(format!("{id}: outline root fill"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

fn strip_masks(svg: &str) -> String {
    let mut s = svg.to_string();
    while let Some(a) = s.find("<mask") {
        let Some(b) = s[a..].find("</mask>") else { break };
        s.replace_range(a..a + b + 7, "");
    }
    s
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Literals directly after the call shapes that name an icon.
fn referenced(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pat in ["icon: \"", ".icon(\"", "icons.paint(ui, \"", "icons.get(ctx, \""] {
        for (i, _) in src.match_indices(pat) {
            let rest = &src[i + pat.len()..];
            if let Some(end) = rest.find('"') {
                out.push(rest[..end].to_string());
            }
        }
    }
    out
}

#[test]
fn every_referenced_icon_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    let have: std::collections::HashSet<&str> = icons::names().iter().copied().collect();
    let mut missing = std::collections::BTreeSet::new();
    for f in files {
        if f.ends_with("icon_lint.rs") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&f) else { continue };
        for n in referenced(&src) {
            let base = n.strip_suffix(".fill").unwrap_or(&n);
            let plain = !base.is_empty()
                && base.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if plain && !have.contains(base) {
                missing.insert(format!("{} ({})", base, f.display()));
            }
        }
    }
    assert!(missing.is_empty(), "unknown icons:\n{}", missing.into_iter().collect::<Vec<_>>().join("\n"));
}
