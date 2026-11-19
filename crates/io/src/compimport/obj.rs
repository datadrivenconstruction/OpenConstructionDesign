// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Wavefront OBJ: `v`, `f` (polygons fanned, negative indices), `usemtl` names materials.

use super::{Axes, Collect, Source};
use ddc_geom::math::V3;

pub fn read(text: &str, axes: Axes) -> Result<Source, String> {
    let mut verts: Vec<V3> = vec![];
    let mut c = Collect::default();
    let mut slot = c.slot("Default");
    let mut out = Source::default();
    let mut bad = 0usize;
    for (n, line) in text.lines().enumerate() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("v") => {
                let p: Vec<f64> = w.take(3).filter_map(|x| x.parse().ok()).collect();
                if p.len() != 3 || p.iter().any(|x| !x.is_finite()) {
                    return Err(format!("line {}: bad vertex", n + 1));
                }
                verts.push(axes.map([p[0], p[1], p[2]]));
            }
            Some("f") => {
                let mut ix = vec![];
                for t in w {
                    let k: i64 = t
                        .split('/')
                        .next()
                        .and_then(|x| x.parse().ok())
                        .ok_or_else(|| format!("line {}: bad face", n + 1))?;
                    let k = if k < 0 { verts.len() as i64 + k } else { k - 1 };
                    if k < 0 || k as usize >= verts.len() {
                        return Err(format!("line {}: face index out of range", n + 1));
                    }
                    ix.push(k as u32);
                }
                if ix.len() < 3 {
                    bad += 1;
                    continue;
                }
                let pts: Vec<V3> = ix.iter().map(|&i| verts[i as usize]).collect();
                let m = pts.len() as u32;
                c.add(slot, &pts, (1..m - 1).map(|k| [0, k, k + 1]));
            }
            Some("usemtl") => slot = c.slot(w.next().unwrap_or("Default")),
            Some("o") if out.name.is_empty() => out.name = w.collect::<Vec<_>>().join(" "),
            _ => {}
        }
    }
    if bad > 0 {
        out.warnings.push(format!("{bad} faces with fewer than 3 corners"));
    }
    out.parts = c.parts();
    Ok(out)
}
