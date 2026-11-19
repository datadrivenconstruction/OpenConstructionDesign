// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A non-horizontal wall top: a polyline (u, h) in the wall's unrolled view, with u along the axis from `a` and h above the bottom.
//! Points are ascending in u; equal u means a step; beyond the end points the top continues horizontally.
//! Volume is the exact integral of h over the layer plan; the display body is a mesh cut at the
//! profile's kinks and steps, with the top face on the polyline: the mesh volume equals the integral.

use crate::math::{v2, V2, V3};
use crate::mesh::Mesh;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Height of the top above the wall bottom at axis point `u` (at a step, the left value).
pub fn height_at(top: &[(f64, f64)], u: f64) -> f64 {
    let (Some(first), Some(last)) = (top.first(), top.last()) else {
        return 0.0;
    };
    if u <= first.0 {
        return first.1;
    }
    if u >= last.0 {
        return last.1;
    }
    for w in top.windows(2) {
        let (p, q) = (w[0], w[1]);
        if u >= p.0 && u <= q.0 && q.0 > p.0 {
            return p.1 + (q.1 - p.1) * (u - p.0) / (q.0 - p.0);
        }
    }
    last.1
}

/// Area and first moment ∫u dA of a polygon in (u, v) coordinates.
fn moments(r: &[(f64, f64)]) -> (f64, f64) {
    let (mut a, mut m) = (0.0, 0.0);
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        let c = p.0 * q.1 - q.0 * p.1;
        a += c;
        m += (p.0 + q.0) * c;
    }
    (a / 2.0, m / 6.0)
}

/// Clipping a polygon by the half-plane `sign·(u − at) ≥ 0`.
fn clip(r: &[(f64, f64)], at: f64, sign: f64) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        let (dp, dq) = (sign * (p.0 - at), sign * (q.0 - at));
        if dp >= 0.0 {
            out.push(p);
        }
        if (dp >= 0.0) != (dq >= 0.0) {
            let t = dp / (dp - dq);
            out.push((at, p.1 + (q.1 - p.1) * t));
        }
    }
    out
}

/// ∫ h(u) dA over plan ring `ring` (any winding) of a wall with its axis from `a` along unit `dir`.
pub fn integral(ring: &[V2], a: V2, dir: V2, top: &[(f64, f64)]) -> f64 {
    let n = v2(-dir.y, dir.x);
    let uv: Vec<(f64, f64)> = ring
        .iter()
        .map(|&p| ((p - a).dot(dir), (p - a).dot(n)))
        .collect();
    let (area, _) = moments(&uv);
    let sign = area.signum();
    if top.is_empty() || area == 0.0 {
        return 0.0;
    }
    // spans: (−∞, u0], [u_i, u_i+1], [u_n, +∞)
    let mut total = 0.0;
    let mut piece = |lo: Option<f64>, hi: Option<f64>, h: &dyn Fn(f64, f64) -> f64| {
        let mut r = uv.clone();
        if let Some(lo) = lo {
            r = clip(&r, lo, 1.0);
        }
        if let Some(hi) = hi {
            r = clip(&r, hi, -1.0);
        }
        if r.len() >= 3 {
            let (a, m) = moments(&r);
            total += sign * h(a, m);
        }
    };
    let (f, l) = (top[0], top[top.len() - 1]);
    piece(None, Some(f.0), &|a, _| a * f.1);
    for w in top.windows(2) {
        let (p, q) = (w[0], w[1]);
        if q.0 - p.0 > 1e-12 {
            let k = (q.1 - p.1) / (q.0 - p.0);
            piece(Some(p.0), Some(q.0), &|a, m| a * (p.1 - k * p.0) + k * m);
        }
    }
    piece(Some(l.0), None, &|a, _| a * l.1);
    total
}

/// Linear pieces of the top on [u0, u1], extended beyond the ends: (ua, ha, ub, hb), ua < ub.
fn pieces(top: &[(f64, f64)], u0: f64, u1: f64) -> Vec<(f64, f64, f64, f64)> {
    let (Some(&f), Some(&l)) = (top.first(), top.last()) else {
        return vec![];
    };
    let mut segs = vec![(f64::NEG_INFINITY, f.1, f.0, f.1)];
    segs.extend(
        top.windows(2)
            .filter(|w| w[1].0 > w[0].0)
            .map(|w| (w[0].0, w[0].1, w[1].0, w[1].1)),
    );
    segs.push((l.0, l.1, f64::INFINITY, l.1));
    let at = |s: (f64, f64, f64, f64), u: f64| {
        if s.0.is_finite() && s.2.is_finite() {
            s.1 + (s.3 - s.1) * (u - s.0) / (s.2 - s.0)
        } else {
            s.1
        }
    };
    segs.into_iter()
        .filter_map(|s| {
            let (a, b) = (s.0.max(u0), s.2.min(u1));
            (b > a).then(|| (a, at(s, a), b, at(s, b)))
        })
        .collect()
}

/// Minimum top height over the axis span [u0, u1].
pub fn min_height(top: &[(f64, f64)], u0: f64, u1: f64) -> f64 {
    pieces(top, u0, u1)
        .iter()
        .flat_map(|p| [p.1, p.3])
        .fold(f64::INFINITY, f64::min)
}

/// Area of the part of the unrolled rectangle [u0, u1] × [z0, z1] under the top (exact clipping).
pub fn area_under(top: &[(f64, f64)], u0: f64, u1: f64, z0: f64, z1: f64) -> f64 {
    let mut total = 0.0;
    for (ua, ha, ub, hb) in pieces(top, u0, u1) {
        let mut us = vec![ua, ub];
        for z in [z0, z1] {
            if (ha - z) * (hb - z) < 0.0 {
                us.push(ua + (ub - ua) * (z - ha) / (hb - ha));
            }
        }
        us.sort_by(f64::total_cmp);
        let g = |u: f64| (ha + (hb - ha) * (u - ua) / (ub - ua)).clamp(z0, z1) - z0;
        total += us
            .windows(2)
            .map(|w| (w[1] - w[0]) * (g(w[0]) + g(w[1])) / 2.0)
            .sum::<f64>();
    }
    total
}

fn u_of(p: V3, a: V2, dir: V2) -> f64 {
    (v2(p.x, p.y) - a).dot(dir)
}

/// Cuts the mesh with the plane u = c; the new edge vertex is a top vertex if both endpoints are top.
fn slice(mesh: &mut Mesh, top_v: &mut Vec<bool>, a: V2, dir: V2, c: f64, eps: f64) {
    let mut cache = HashMap::<(u32, u32), u32>::new();
    let (mut tris, mut faces) = (vec![], vec![]);
    let old = std::mem::take(&mut mesh.triangles);
    for (t, &f) in old.iter().zip(&mesh.face_of) {
        let d: Vec<f64> = t
            .iter()
            .map(|&i| {
                let d = u_of(mesh.positions[i as usize], a, dir) - c;
                if d.abs() <= eps {
                    0.0
                } else {
                    d
                }
            })
            .collect();
        if !(d.iter().any(|&x| x < 0.0) && d.iter().any(|&x| x > 0.0)) {
            tris.push(*t);
            faces.push(f);
            continue;
        }
        let mut poly: Vec<(u32, f64)> = vec![];
        for k in 0..3 {
            let (i, j) = (t[k], t[(k + 1) % 3]);
            let (di, dj) = (d[k], d[(k + 1) % 3]);
            poly.push((i, di));
            if di * dj < 0.0 {
                let v = *cache.entry((i.min(j), i.max(j))).or_insert_with(|| {
                    let (p, q) = (mesh.positions[i as usize], mesh.positions[j as usize]);
                    mesh.positions.push(p + (q - p) * (di / (di - dj)));
                    top_v.push(top_v[i as usize] && top_v[j as usize]);
                    (mesh.positions.len() - 1) as u32
                });
                poly.push((v, 0.0));
            }
        }
        for side in [-1.0, 1.0] {
            let part: Vec<u32> = poly
                .iter()
                .filter(|(_, d)| side * d >= 0.0)
                .map(|p| p.0)
                .collect();
            for k in 1..part.len().saturating_sub(1) {
                tris.push([part[0], part[k], part[k + 1]]);
                faces.push(f);
            }
        }
    }
    mesh.triangles = tris;
    mesh.face_of = faces;
}

/// Step at u = c: top vertices on the plane are duplicated (copies go to the right),
/// and the gap is closed with a vertical face. Returns the copies.
fn split_step(
    mesh: &mut Mesh,
    top_v: &mut Vec<bool>,
    a: V2,
    dir: V2,
    c: f64,
    eps: f64,
) -> Vec<(u32, u32)> {
    let on = |m: &Mesh, i: u32| (u_of(m.positions[i as usize], a, dir) - c).abs() <= eps;
    let mut dup = HashMap::<u32, u32>::new();
    for i in 0..mesh.positions.len() as u32 {
        if top_v[i as usize] && on(mesh, i) {
            mesh.positions.push(mesh.positions[i as usize]);
            top_v.push(true);
            dup.insert(i, (mesh.positions.len() - 1) as u32);
        }
    }
    for t in &mut mesh.triangles {
        let cu: f64 = t
            .iter()
            .map(|&i| u_of(mesh.positions[i as usize], a, dir))
            .sum::<f64>()
            / 3.0;
        if cu > c {
            for i in t.iter_mut() {
                if let Some(&j) = dup.get(i) {
                    *i = j;
                }
            }
        }
    }
    // open edges on the step plane → patch contours
    let mut edges = HashSet::<(u32, u32)>::new();
    for t in &mesh.triangles {
        for k in 0..3 {
            edges.insert((t[k], t[(k + 1) % 3]));
        }
    }
    let mut nxt = BTreeMap::<u32, u32>::new();
    for &(p, q) in &edges {
        if !edges.contains(&(q, p)) && on(mesh, p) && on(mesh, q) {
            nxt.insert(q, p);
        }
    }
    let face = mesh.face_of.iter().copied().max().map_or(0, |f| f + 1);
    while let Some((&s, _)) = nxt.iter().next() {
        let mut ring = vec![s];
        let mut cur = nxt.remove(&s).unwrap_or(s);
        while cur != s {
            ring.push(cur);
            match nxt.remove(&cur) {
                Some(n) => cur = n,
                None => break,
            }
        }
        let pos = |i: u32| mesh.positions[i as usize];
        // degenerate ears first (vertices on one vertical), then a fan
        let mut k = 0;
        while ring.len() > 3 && k < ring.len() {
            let n = ring.len();
            let (p, q, r) = (ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]);
            if (pos(q) - pos(p)).cross(pos(r) - pos(p)).len() <= eps * eps {
                mesh.triangles.push([p, q, r]);
                mesh.face_of.push(face);
                ring.remove(k);
                k = 0;
            } else {
                k += 1;
            }
        }
        for k in 1..ring.len().saturating_sub(1) {
            mesh.triangles.push([ring[0], ring[k], ring[k + 1]]);
            mesh.face_of.push(face);
        }
    }
    dup.into_iter().collect()
}

/// Snaps the body top onto the polyline: cuts the mesh at the profile's kinks and steps and moves only
/// the top-face vertices (z = `base + height`, normal up); other faces at the same
/// elevation (the underside of a lintel above the wall) are left untouched. Mesh volume = [`integral`].
pub fn apply(mesh: &mut Mesh, a: V2, dir: V2, base: f64, height: f64, top: &[(f64, f64)]) {
    let zt = base + height;
    let ez = 1e-6 * zt.abs().max(1.0);
    let n0 = mesh.positions.len();
    let mut top_v = vec![false; mesh.positions.len()];
    for t in &mesh.triangles {
        let [p, q, r] = t.map(|i| mesh.positions[i as usize]);
        let flat = [p, q, r].iter().all(|v| (v.z - zt).abs() <= ez);
        if flat && (q - p).cross(r - p).z > 0.0 {
            for &i in t {
                top_v[i as usize] = true;
            }
        }
    }
    if !top_v.iter().any(|&b| b) {
        return;
    }
    let (lo, hi) = mesh
        .positions
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &p| {
            let u = u_of(p, a, dir);
            (l.min(u), h.max(u))
        });
    let eps = 1e-9 * (hi - lo).abs().max(1.0);
    let inside = |u: f64| u > lo + eps && u < hi - eps;
    let mut cuts: Vec<f64> = top.iter().map(|p| p.0).filter(|&u| inside(u)).collect();
    cuts.dedup_by(|x, y| (*x - *y).abs() <= eps);
    for &c in &cuts {
        slice(mesh, &mut top_v, a, dir, c, eps);
    }
    let mut fixed = HashMap::<u32, f64>::new();
    for &c in &cuts {
        let at: Vec<f64> = top
            .iter()
            .filter(|t| (t.0 - c).abs() <= eps)
            .map(|t| t.1)
            .collect();
        let (hl, hr) = (at[0], at[at.len() - 1]);
        if (hr - hl).abs() > ez {
            for (l, r) in split_step(mesh, &mut top_v, a, dir, c, eps) {
                fixed.insert(l, hl);
                fixed.insert(r, hr);
            }
        }
    }
    for (i, p) in mesh.positions.iter_mut().enumerate() {
        if top_v[i] {
            // at an end face with a step, the value from the body's side is used
            let u = u_of(*p, a, dir);
            let at = || top.iter().filter(|t| (t.0 - u).abs() <= eps).map(|t| t.1);
            let end = if u <= lo + eps {
                at().next_back()
            } else if u >= hi - eps {
                at().next()
            } else {
                None
            };
            let h = fixed
                .get(&(i as u32))
                .copied()
                .or(end)
                .unwrap_or_else(|| height_at(top, u));
            p.z = base + h;
        }
    }
    // slice points inside a side face stayed at the old height: refill it from its outline
    crate::face_fill::refill_faces(mesh, &top_v, n0);
}

/// Snaps the body bottom onto the polyline `base` (u, dz from `zb`): the mirror of [`apply`]; the bottom face
/// (z = `zb`, normal down) becomes the top face after reflecting in z.
pub fn apply_base(mesh: &mut Mesh, a: V2, dir: V2, zb: f64, base: &[(f64, f64)]) {
    if base.is_empty() || base.iter().all(|p| p.1 == 0.0) {
        return;
    }
    let flip = |m: &mut Mesh| {
        for p in m.positions.iter_mut() {
            p.z = -p.z;
        }
        for t in m.triangles.iter_mut() {
            t.swap(1, 2);
        }
    };
    flip(mesh);
    let neg: Vec<(f64, f64)> = base.iter().map(|&(u, d)| (u, -d)).collect();
    apply(mesh, a, dir, -zb, 0.0, &neg);
    flip(mesh);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, x1: f64, y0: f64, y1: f64) -> Vec<V2> {
        vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
    }

    #[test]
    fn flat_profile_equals_prism() {
        let r = rect(0.0, 4.0, 0.0, 0.3);
        let v = integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &[(0.0, 3.0), (4.0, 3.0)]);
        assert!((v - 4.0 * 0.3 * 3.0).abs() < 1e-12);
    }

    #[test]
    fn gable_and_step_integrate_exactly() {
        // gable: 3 m at the edges, 5 m mid-wall for a 4 m wall, thickness 0.2
        let r = rect(0.0, 4.0, -0.1, 0.1);
        let g = [(0.0, 3.0), (2.0, 5.0), (4.0, 3.0)];
        let v = integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &g);
        assert!((v - 0.2 * (4.0 * 3.0 + 4.0)).abs() < 1e-12, "{v}");
        // step 2 → 3 at u = 1, plan wider than the profile: edges are horizontal
        let s = [(0.0, 2.0), (1.0, 2.0), (1.0, 3.0), (2.0, 3.0)];
        let r = rect(-1.0, 3.0, 0.0, 1.0);
        let v = integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &s);
        assert!((v - (2.0 * 2.0 + 2.0 * 3.0)).abs() < 1e-12, "{v}");
        assert_eq!(height_at(&s, 1.0), 2.0);
        assert_eq!(height_at(&s, 1.5), 3.0);
    }

    #[test]
    fn mitred_and_rotated_footprint() {
        // axis at 90°, plan is a trapezoid (mitre), clockwise winding
        let r = vec![v2(0.0, 0.0), v2(0.2, 0.2), v2(0.2, 3.8), v2(0.0, 4.0)];
        let g = [(0.0, 3.0), (4.0, 5.0)];
        let v = integral(&r, v2(0.0, 0.0), v2(0.0, 1.0), &g);
        // h(u) = 3 + u/2; area 0.76, ū = 2 (symmetry)
        assert!((v - 0.76 * 4.0).abs() < 1e-12, "{v}");
    }

    fn prism(x0: f64, x1: f64, h: f64) -> Mesh {
        let tol = crate::Tol::default();
        let p = crate::Profile::new(rect(x0, x1, -0.1, 0.1), vec![], tol).unwrap();
        crate::Solid::extrude(&p, h, tol).triangulate().unwrap()
    }

    #[test]
    fn mesh_volume_matches_integral() {
        let r = rect(0.0, 4.0, -0.1, 0.1);
        for g in [
            vec![(0.0, 3.0), (2.0, 5.0), (4.0, 3.0)],
            vec![(0.0, 2.0), (1.5, 2.0), (1.5, 3.5), (5.0, 3.5)],
            vec![(-1.0, 3.0), (1.0, 2.0), (1.0, 4.0), (3.0, 4.0), (3.0, 2.5)],
            vec![(0.0, 4.0), (0.0, 2.0), (4.0, 2.5)],
        ] {
            let mut m = prism(0.0, 4.0, 3.0);
            apply(&mut m, v2(0.0, 0.0), v2(1.0, 0.0), 0.0, 3.0, &g);
            let q = integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &g);
            assert!(m.is_watertight(), "{g:?}");
            assert!(
                (m.volume() - q).abs() < 1e-9,
                "{g:?}: {} vs {q}",
                m.volume()
            );
        }
    }

    #[test]
    fn base_on_sloped_slab_matches_integral() {
        // bottom follows a sloped slab 0 → 0.4, polyline top: volume = ∫(top − bottom)
        let r = rect(0.0, 4.0, -0.1, 0.1);
        let (g, b) = (
            [(0.0, 3.0), (2.0, 5.0), (4.0, 3.0)],
            [(0.0, 0.0), (4.0, 0.4)],
        );
        let mut m = prism(0.0, 4.0, 3.0);
        apply(&mut m, v2(0.0, 0.0), v2(1.0, 0.0), 0.0, 3.0, &g);
        apply_base(&mut m, v2(0.0, 0.0), v2(1.0, 0.0), 0.0, &b);
        let q = integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &g)
            - integral(&r, v2(0.0, 0.0), v2(1.0, 0.0), &b);
        assert!(m.is_watertight());
        assert!((m.volume() - q).abs() < 1e-9, "{} vs {q}", m.volume());
    }

    #[test]
    fn downward_face_at_top_level_untouched() {
        // lintel above a wall: its underside at the top elevation faces down, so it is not a top face
        let mut m = prism(0.0, 4.0, 3.0);
        let lintel = prism(1.0, 2.0, 1.0);
        let off = m.positions.len() as u32;
        m.positions.extend(
            lintel
                .positions
                .iter()
                .map(|p| crate::math::v3(p.x, p.y, p.z + 3.0)),
        );
        m.triangles
            .extend(lintel.triangles.iter().map(|t| t.map(|i| i + off)));
        m.face_of.extend(&lintel.face_of);
        let g = [(0.0, 3.0), (2.0, 5.0), (4.0, 3.0)];
        apply(&mut m, v2(0.0, 0.0), v2(1.0, 0.0), 0.0, 3.0, &g);
        let low = m.positions[off as usize..off as usize + lintel.positions.len()]
            .iter()
            .filter(|p| (p.z - 3.0).abs() < 1e-12)
            .count();
        assert_eq!(low, 4);
    }

    #[test]
    fn opening_cut_under_profile() {
        let g = [(0.0, 3.0), (2.0, 5.0), (4.0, 3.0)];
        assert_eq!(min_height(&g, 1.0, 3.0), 4.0);
        assert_eq!(min_height(&g, -2.0, 0.5), 3.0);
        // opening 1..3 × 3.5..4.5 under the gable: minus two triangles above the slopes
        let a = area_under(&g, 1.0, 3.0, 3.5, 4.5);
        let exact = 2.0 - 2.0 * (0.5 * 0.5 * 0.5);
        assert!((a - exact).abs() < 1e-12, "{a}");
        assert!((area_under(&g, 1.0, 3.0, 0.0, 1.0) - 2.0).abs() < 1e-12);
        assert_eq!(area_under(&g, 0.0, 4.0, 6.0, 7.0), 0.0);
    }
}

/// Like [`apply`] for a wall with an arc axis: u is the length along the axis, profile kinks are radial planes.
pub fn apply_arc(mesh: &mut Mesh, ax: &crate::arc::ArcAxis, base: f64, height: f64, top: &[(f64, f64)]) {
    let zt = base + height;
    let ez = 1e-6 * zt.abs().max(1.0);
    let n0 = mesh.positions.len();
    let mut top_v = vec![false; mesh.positions.len()];
    for t in &mesh.triangles {
        let [p, q, r] = t.map(|i| mesh.positions[i as usize]);
        let flat = [p, q, r].iter().all(|v| (v.z - zt).abs() <= ez);
        if flat && (q - p).cross(r - p).z > 0.0 {
            for &i in t {
                top_v[i as usize] = true;
            }
        }
    }
    if !top_v.iter().any(|&b| b) {
        return;
    }
    let len = ax.length();
    let mut cuts: Vec<f64> = top.iter().map(|p| p.0).filter(|&u| u > 1e-9 && u < len - 1e-9).collect();
    cuts.dedup_by(|x, y| (*x - *y).abs() <= 1e-9);
    for &c in &cuts {
        // the radial plane through the centre at axis length c
        slice(mesh, &mut top_v, ax.center, ax.tangent_at(c), 0.0, 1e-9);
    }
    for (i, p) in mesh.positions.iter_mut().enumerate() {
        if top_v[i] {
            let u = ax.param_of(v2(p.x, p.y)).clamp(0.0, len);
            p.z = base + height_at(top, u);
        }
    }
    crate::face_fill::refill_faces(mesh, &top_v, n0);
}

/// ∫ h(u) dA over the plan ring of an arc wall (u is length along the axis): in 1° sectors and at kinks;
/// `exact` is the exact ring area (chords give slightly less), corrected by the area ratio.
pub fn integral_arc(ring: &[V2], exact: f64, ax: &crate::arc::ArcAxis, top: &[(f64, f64)]) -> f64 {
    let poly = |pts: &[V2]| {
        let mut s = 0.0;
        for i in 0..pts.len() {
            s += pts[i].cross(pts[(i + 1) % pts.len()]);
        }
        s / 2.0
    };
    let whole = poly(ring).abs();
    if top.is_empty() || whole <= 0.0 {
        return 0.0;
    }
    let len = ax.length();
    let mut us = vec![0.0, len];
    us.extend(top.iter().map(|p| p.0).filter(|&u| u > 1e-9 && u < len - 1e-9));
    us.sort_by(f64::total_cmp);
    us.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let step = (1f64.to_radians() * ax.radius).max(1e-4);
    let half = |pts: Vec<V2>, s: f64, fwd: bool| -> Vec<V2> {
        let t = ax.tangent_at(s) * if fwd { 1.0 } else { -1.0 };
        let mut out = vec![];
        for i in 0..pts.len() {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            let (fp, fq) = ((p - ax.center).dot(t), (q - ax.center).dot(t));
            if fp >= 0.0 {
                out.push(p);
            }
            if (fp >= 0.0) != (fq >= 0.0) {
                out.push(p + (q - p) * (fp / (fp - fq)));
            }
        }
        out
    };
    let mut total = 0.0;
    for pair in us.windows(2) {
        let n = ((pair[1] - pair[0]) / step).ceil().max(1.0) as usize;
        for k in 0..n {
            let s0 = pair[0] + (pair[1] - pair[0]) * k as f64 / n as f64;
            let s1 = pair[0] + (pair[1] - pair[0]) * (k + 1) as f64 / n as f64;
            let mut pts = ring.to_vec();
            if s0 > 1e-9 {
                pts = half(pts, s0, true);
            }
            if s1 < len - 1e-9 {
                pts = half(pts, s1, false);
            }
            if pts.len() >= 3 {
                let h = 0.5 * (height_at(top, s0 + 1e-7) + height_at(top, s1 - 1e-7));
                total += poly(&pts).abs() * h;
            }
        }
    }
    total * exact / whole
}
