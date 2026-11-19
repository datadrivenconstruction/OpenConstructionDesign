// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Splits the faces of a layered slab, ceiling or roof mesh by layer, so each layer can take its
//! own texture: the top face shows the top layer, the bottom face the bottom layer and the edges
//! show the layers as bands. Bands need horizontal layer joints, so they are made only on a flat
//! element; on a sloped one the edges keep a single (the thickest) layer.

use ddc_render::SceneMesh;

type P = [f32; 3];

/// Pieces of `m` per layer. `layers` are thickness and face tag, top layer first; `edge` is the
/// tag of edge faces that cannot be banded. Empty when there is nothing to split (under two
/// layers, no triangles, a non-finite mesh). Triangle area is kept: edge triangles that cross a
/// joint are cut there.
pub fn split(m: &SceneMesh, layers: &[(f32, u32)], edge: u32) -> Vec<SceneMesh> {
    if layers.len() < 2 || m.triangles.is_empty() {
        return vec![];
    }
    let pos = |i: u32| m.positions.get(i as usize).copied();
    let top = m
        .positions
        .iter()
        .map(|p| p[2])
        .fold(f32::NEG_INFINITY, f32::max);
    if !top.is_finite() || m.positions.iter().any(|p| p.iter().any(|v| !v.is_finite())) {
        return vec![];
    }
    let tris: Vec<[P; 3]> = m
        .triangles
        .iter()
        .filter_map(|t| Some([pos(t[0])?, pos(t[1])?, pos(t[2])?]))
        .collect();
    let nz_of = |t: &[P; 3]| -> Option<f32> {
        let (u, v) = (sub(t[1], t[0]), sub(t[2], t[0]));
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        (len > 1e-12).then(|| n[2] / len)
    };
    let flat = tris
        .iter()
        .filter_map(nz_of)
        .filter(|nz| nz.abs() > 0.5)
        .all(|nz| nz.abs() > 0.9999);
    // joints, top down: below layer 0, below layer 1, ...
    let mut joints = vec![];
    let mut z = top;
    for l in &layers[..layers.len() - 1] {
        z -= l.0;
        joints.push(z);
    }
    let mut out: Vec<(u32, Vec<P>, Vec<[u32; 3]>)> = vec![];
    let mut put = |tag: u32, poly: &[P]| {
        if poly.len() < 3 || area2(poly) < 1e-10 {
            return;
        }
        let k = match out.iter().position(|o| o.0 == tag) {
            Some(k) => k,
            None => {
                out.push((tag, vec![], vec![]));
                out.len() - 1
            }
        };
        let (_, ps, ts) = &mut out[k];
        let base = ps.len() as u32;
        ps.extend_from_slice(poly);
        for j in 1..poly.len() as u32 - 1 {
            ts.push([base, base + j, base + j + 1]);
        }
    };
    for t in &tris {
        let Some(nz) = nz_of(t) else { continue };
        if nz > 0.5 {
            put(layers[0].1, t);
        } else if nz < -0.5 {
            put(layers[layers.len() - 1].1, t);
        } else if flat {
            for (i, l) in layers.iter().enumerate() {
                let hi = if i == 0 { f32::INFINITY } else { joints[i - 1] };
                let lo = if i == layers.len() - 1 {
                    f32::NEG_INFINITY
                } else {
                    joints[i]
                };
                let mut poly = t.to_vec();
                if hi.is_finite() {
                    poly = clip(&poly, hi, false);
                }
                if lo.is_finite() {
                    poly = clip(&poly, lo, true);
                }
                put(l.1, &poly);
            }
        } else {
            put(edge, t);
        }
    }
    out.into_iter()
        .map(|(tag, positions, triangles)| SceneMesh {
            element: m.element,
            tag,
            positions,
            triangles,
        })
        .collect()
}

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Twice the area of a convex or fan-triangulable polygon.
fn area2(poly: &[P]) -> f32 {
    let mut a = 0.0;
    for j in 1..poly.len() - 1 {
        let (u, v) = (sub(poly[j], poly[0]), sub(poly[j + 1], poly[0]));
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        a += (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    }
    a
}

/// The part of the polygon on one side of the plane `z`: above (`keep_above`) or below it.
fn clip(poly: &[P], z: f32, keep_above: bool) -> Vec<P> {
    let inside = |p: &P| if keep_above { p[2] >= z } else { p[2] <= z };
    let mut out = vec![];
    for (i, a) in poly.iter().enumerate() {
        let b = &poly[(i + 1) % poly.len()];
        if inside(a) {
            out.push(*a);
        }
        if inside(a) != inside(b) {
            let k = (f64::from(z) - f64::from(a[2])) / (f64::from(b[2]) - f64::from(a[2]));
            let l = |i: usize| (f64::from(a[i]) + (f64::from(b[i]) - f64::from(a[i])) * k) as f32;
            out.push([l(0), l(1), z]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Box `w` x `d` x `h` with its top at z = `h`, outward winding.
    fn slab(w: f32, d: f32, h: f32) -> SceneMesh {
        let p = vec![
            [0.0, 0.0, 0.0],
            [w, 0.0, 0.0],
            [w, d, 0.0],
            [0.0, d, 0.0],
            [0.0, 0.0, h],
            [w, 0.0, h],
            [w, d, h],
            [0.0, d, h],
        ];
        let t = vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
        ];
        SceneMesh {
            element: 5,
            tag: 11,
            positions: p,
            triangles: t,
        }
    }

    fn area(m: &SceneMesh) -> f32 {
        m.triangles
            .iter()
            .map(|t| area2(&[m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]]) / 2.0)
            .sum()
    }

    fn of(out: &[SceneMesh], tag: u32) -> &SceneMesh {
        out.iter().find(|m| m.tag == tag).expect("tag present")
    }

    #[test]
    fn top_bottom_and_edge_bands_take_their_layers() {
        let m = slab(4.0, 2.0, 0.3);
        let out = split(&m, &[(0.1, 100), (0.2, 200)], 999);
        assert!(out.iter().all(|o| o.element == 5));
        // top face 4 x 2, bottom face 4 x 2, each in its own layer plus the edge band
        let top = of(&out, 100);
        let bottom = of(&out, 200);
        assert!(top.positions.iter().any(|p| p[2] == 0.3));
        assert!(bottom.positions.iter().any(|p| p[2] == 0.0));
        // edge band of layer 0: perimeter 12 x 0.1; layer 1: 12 x 0.2
        assert!((area(top) - (8.0 + 12.0 * 0.1)).abs() < 1e-3, "{}", area(top));
        assert!((area(bottom) - (8.0 + 12.0 * 0.2)).abs() < 1e-3, "{}", area(bottom));
        assert!(out.iter().all(|o| o.tag != 999));
        // nothing is lost or invented
        let total: f32 = out.iter().map(area).sum();
        assert!((total - area(&m)).abs() < 1e-3);
    }

    #[test]
    fn edge_pieces_stay_inside_their_band() {
        let m = slab(1.0, 1.0, 1.0);
        let out = split(&m, &[(0.25, 1), (0.5, 2), (0.25, 3)], 9);
        let mid = of(&out, 2);
        let side: Vec<_> = mid.positions.iter().filter(|p| p[2] > 0.0 && p[2] < 1.0).collect();
        assert!(!side.is_empty());
        for p in &mid.positions {
            // the middle layer has no face of its own on top or bottom
            assert!(p[2] >= 0.25 - 1e-5 && p[2] <= 0.75 + 1e-5, "{p:?}");
        }
    }

    #[test]
    fn sloped_elements_keep_one_edge_layer() {
        // a wedge: top face tilted by 0.1 over 1 m, vertical ends
        let mut m = slab(1.0, 1.0, 0.5);
        m.positions[5][2] = 0.6;
        m.positions[6][2] = 0.6;
        let out = split(&m, &[(0.1, 10), (0.4, 20)], 77);
        let edge = of(&out, 77);
        assert!(area(edge) > 0.0);
        // the tilted top still takes the top layer, the flat bottom the bottom layer
        assert!(out.iter().any(|o| o.tag == 10) && out.iter().any(|o| o.tag == 20));
        let total: f32 = out.iter().map(area).sum();
        assert!((total - area(&m)).abs() < 1e-3);
    }

    #[test]
    fn nothing_to_split_gives_empty() {
        let m = slab(1.0, 1.0, 1.0);
        assert!(split(&m, &[(1.0, 1)], 0).is_empty());
        assert!(split(&m, &[], 0).is_empty());
        let empty = SceneMesh {
            element: 0,
            tag: 0,
            positions: vec![],
            triangles: vec![],
        };
        assert!(split(&empty, &[(0.5, 1), (0.5, 2)], 0).is_empty());
        let mut nan = slab(1.0, 1.0, 1.0);
        nan.positions[0][0] = f32::NAN;
        assert!(split(&nan, &[(0.5, 1), (0.5, 2)], 0).is_empty());
    }
}
