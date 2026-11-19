// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Family thumbnail: software rasterization of a mesh (front-right-top isometric view,
//! z-buffer, flat shading), RGBA with a transparent background. No GPU, so it works in the model and in tests.

use super::{Family, FamilyError, Lod, Value};
use crate::math::{v3, V3};
use crate::mesh::Mesh;

pub const SIZE: u32 = 96;

/// Effective image size: `render` and `thumbnail` return `clamp_size(size)²·4` bytes.
pub fn clamp_size(size: u32) -> u32 {
    size.clamp(8, 512)
}

/// RGBA `n × n`, n = `clamp_size(size)` (rows top to bottom). An empty mesh gives a transparent image.
pub fn render(mesh: &Mesh, size: u32, rgb: [u8; 3]) -> Vec<u8> {
    let n = clamp_size(size) as usize;
    let mut img = vec![0u8; n * n * 4];
    if mesh.triangles.is_empty() {
        return img;
    }
    // view: the camera looks along d; screen axes u (right) and w (up)
    let d = v3(-1.0, 1.0, -0.8).normalized();
    let u = v3(1.0, 1.0, 0.0).normalized();
    let w = u.cross(d).normalized() * -1.0;
    let w = if w.z < 0.0 { w * -1.0 } else { w };
    let proj = |p: V3| (p.dot(u), p.dot(w), p.dot(d));
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for p in &mesh.positions {
        let (a, b, _) = proj(*p);
        lo = (lo.0.min(a), lo.1.min(b));
        hi = (hi.0.max(a), hi.1.max(b));
    }
    let span = (hi.0 - lo.0).max(hi.1 - lo.1).max(1e-9);
    let k = (n as f64 - 4.0) / span;
    let (cx, cy) = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
    let scr = |p: V3| {
        let (a, b, z) = proj(p);
        (
            (a - cx) * k + n as f64 / 2.0,
            n as f64 / 2.0 - (b - cy) * k,
            z,
        )
    };
    let light = v3(0.4, -0.5, 0.9).normalized();
    let mut zb = vec![f64::MAX; n * n];
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions.get(i as usize).copied().unwrap_or_default());
        let nrm = (b - a).cross(c - a);
        if nrm.len() < 1e-15 {
            continue;
        }
        let nrm = nrm.normalized();
        // a normal pointing away from the camera is a back face; open meshes are drawn on both sides
        let nl = nrm.dot(light).abs();
        let shade = 0.35 + 0.65 * nl;
        let col = rgb.map(|c| (c as f64 * shade).round().clamp(0.0, 255.0) as u8);
        let (p0, p1, p2) = (scr(a), scr(b), scr(c));
        let minx = p0.0.min(p1.0).min(p2.0).floor().max(0.0) as usize;
        let maxx = (p0.0.max(p1.0).max(p2.0).ceil() as usize).min(n - 1);
        let miny = p0.1.min(p1.1).min(p2.1).floor().max(0.0) as usize;
        let maxy = (p0.1.max(p1.1).max(p2.1).ceil() as usize).min(n - 1);
        let area = (p1.0 - p0.0) * (p2.1 - p0.1) - (p2.0 - p0.0) * (p1.1 - p0.1);
        if area.abs() < 1e-12 {
            continue;
        }
        for y in miny..=maxy {
            for x in minx..=maxx {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let w0 = ((p1.0 - px) * (p2.1 - py) - (p2.0 - px) * (p1.1 - py)) / area;
                let w1 = ((p2.0 - px) * (p0.1 - py) - (p0.0 - px) * (p2.1 - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                    continue;
                }
                let z = w0 * p0.2 + w1 * p1.2 + w2 * p2.2;
                let i = y * n + x;
                if z < zb[i] {
                    zb[i] = z;
                    img[i * 4..i * 4 + 4].copy_from_slice(&[col[0], col[1], col[2], 255]);
                }
            }
        }
    }
    img
}

/// Family thumbnail from the first type (or the default values).
pub fn thumbnail(f: &Family, size: u32) -> Result<Vec<u8>, FamilyError> {
    let tv: Vec<(String, Value)> = f
        .types
        .first()
        .map(|t| t.values.clone())
        .unwrap_or_default();
    let r = f.resolve(&tv, &[])?;
    let b = f.build(&r, Lod::Medium)?;
    Ok(render(&b.mesh, size, [176, 160, 140]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_has_opaque_center_and_clear_corner() {
        let f = Family::parse(
            "family Cube\nparam S length type 1m\nextrude B rect(0, 0, S, S) z(0, S)\n",
        )
        .unwrap();
        let img = thumbnail(&f, 64).unwrap();
        assert_eq!(img.len(), 64 * 64 * 4);
        let a = |x: usize, y: usize| img[(y * 64 + x) * 4 + 3];
        assert_eq!(a(32, 32), 255);
        assert_eq!(a(0, 0), 0);
        // out of range: the size is clamped and the buffer length matches it
        for (ask, n) in [(2u32, 8usize), (4000, 512)] {
            assert_eq!(clamp_size(ask) as usize, n);
            assert_eq!(thumbnail(&f, ask).unwrap().len(), n * n * 4);
        }
    }
}
