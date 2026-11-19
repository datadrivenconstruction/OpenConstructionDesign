// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Roof edge detail: copings on parapet tops, half-round eaves gutters, rain heads and
//! downpipes with offsets, shoes and wall brackets; downpipes spaced by the drainage rule.

use super::prim::{chamfered, prism, rod, tube, Z};
use super::{round_sides, Detail, Lod, Mat};
use crate::math::{v2, v3, V2, V3};

/// Coping thickness, the fall of its top toward the roof, a drip groove under each overhang.
const COPING_T: f64 = 0.04;
const COPING_FALL: f64 = 0.01;
/// Gutter radius and wall, downpipe radius, its distance off the wall face.
pub const GUTTER_R: f64 = 0.0625;
const GUTTER_WALL: f64 = 0.003;
pub const PIPE_R: f64 = 0.05;
pub const PIPE_OFF: f64 = 0.04;

fn h(v: V2) -> V3 {
    v3(v.x, v.y, 0.0)
}

/// Coping over a parapet from `a` to `b` (wall axis) at top `z`; `inside` is the plan side of
/// the roof, where the top falls. Ends run on by half the wall so corners close.
#[allow(clippy::too_many_arguments)]
pub fn coping(d: &mut Detail, a: V2, b: V2, z: f64, wall_t: f64, overhang: f64, inside: V2, lod: Lod) {
    let ab = b - a;
    let l = ab.len();
    if l < 1e-6 {
        return;
    }
    let dir = ab * (1.0 / l);
    let n = if v2(-dir.y, dir.x).dot(inside) >= 0.0 { v2(-dir.y, dir.x) } else { v2(dir.y, -dir.x) };
    let w = wall_t / 2.0 + overhang;
    // section in (s toward the roof, z): high on the outside, falling inward
    let sec: Vec<V2> = match lod {
        Lod::Coarse | Lod::Medium => vec![
            v2(-w, 0.0),
            v2(w, 0.0),
            v2(w, COPING_T - COPING_FALL),
            v2(-w, COPING_T),
        ],
        Lod::Fine => {
            let g = (overhang * 0.5).clamp(0.005, 0.02);
            vec![
                v2(-w, 0.0),
                v2(-w + g - 0.004, 0.0),
                v2(-w + g - 0.004, 0.006),
                v2(-w + g + 0.004, 0.006),
                v2(-w + g + 0.004, 0.0),
                v2(w - g - 0.004, 0.0),
                v2(w - g - 0.004, 0.006),
                v2(w - g + 0.004, 0.006),
                v2(w - g + 0.004, 0.0),
                v2(w, 0.0),
                v2(w, COPING_T - COPING_FALL),
                v2(-w, COPING_T),
            ]
        }
    };
    let ext = wall_t / 2.0;
    let o = h(a - dir * ext) + Z * z;
    prism(d.part(Mat::Stone), &sec, o, h(n), Z, h(dir) * (l + 2.0 * ext));
}

/// Half-round gutter along an eave from `a` to `b` (its back edge), hung on the `out` side.
pub fn gutter(d: &mut Detail, a: V3, b: V3, out: V2, lod: Lod) {
    let ab = b - a;
    let l = ab.len();
    if l < 1e-6 {
        return;
    }
    let dir = ab * (1.0 / l);
    let n = h(out).normalized();
    let k = match lod {
        Lod::Coarse => 2,
        Lod::Medium => 5,
        Lod::Fine => 9,
    };
    // half ring in (s outward, z), open at the top, a bead on the front lip at fine
    let (r0, r1) = (GUTTER_R, GUTTER_R - GUTTER_WALL);
    let mut sec: Vec<V2> = vec![];
    for i in 0..=k {
        let t = std::f64::consts::PI * (1.0 + i as f64 / k as f64);
        sec.push(v2(r0 + r0 * t.cos(), r0 * t.sin()));
    }
    if lod == Lod::Fine {
        sec.push(v2(2.0 * r0 + 0.006, 0.0));
        sec.push(v2(2.0 * r0 + 0.006, 0.008));
        sec.push(v2(2.0 * r0 - GUTTER_WALL, 0.008));
    }
    for i in (0..=k).rev() {
        let t = std::f64::consts::PI * (1.0 + i as f64 / k as f64);
        sec.push(v2(r0 + r1 * t.cos(), r1 * t.sin()));
    }
    prism(d.part(Mat::Metal), &sec, a, n, Z, dir * l);
    if lod != Lod::Coarse {
        // stop ends
        for (p, sgn) in [(a, 1.0), (b, -1.0)] {
            let cap: Vec<V2> = (0..=k)
                .map(|i| {
                    let t = std::f64::consts::PI * (1.0 + i as f64 / k as f64);
                    v2(r0 + r0 * t.cos(), r0 * t.sin())
                })
                .collect();
            prism(d.part(Mat::Metal), &cap, p, n, Z, dir * (sgn * GUTTER_WALL));
        }
    }
}

/// Downpipe from an outlet at `top` (gutter or rain head) down to `bottom_z`, standing `out`
/// off the wall face at `face`: an offset bend at the top, a shoe at the foot, brackets.
pub fn downpipe(d: &mut Detail, top: V3, face: V2, out: V2, bottom_z: f64, lod: Lod) {
    let n = round_sides(lod, false);
    let o = h(out).normalized();
    let x = h(face) + o * (PIPE_OFF + PIPE_R);
    let x = v3(x.x, x.y, 0.0);
    let z_top = top.z - 0.02;
    if z_top - bottom_z < 0.5 {
        return;
    }
    let m = d.part(Mat::Metal);
    let mut path = vec![];
    let horiz = v3(top.x - x.x, top.y - x.y, 0.0);
    if horiz.len() > 0.02 && lod != Lod::Coarse {
        // swan neck: down from the outlet, across at 45°, down the wall
        let drop = horiz.len();
        path.push(v3(top.x, top.y, z_top));
        path.push(v3(top.x, top.y, z_top - 0.1));
        path.push(v3(x.x, x.y, z_top - 0.1 - drop));
    } else {
        path.push(v3(x.x, x.y, z_top));
    }
    let shoe = if lod == Lod::Coarse { 0.0 } else { 0.15 };
    path.push(v3(x.x, x.y, bottom_z + 0.05 + shoe));
    if shoe > 0.0 {
        path.push(v3(x.x, x.y, bottom_z + 0.05) + o * 0.12);
    }
    tube(m, &path, PIPE_R, n);
    if lod == Lod::Fine {
        let mut z = bottom_z + 1.0;
        while z < z_top - 0.5 {
            let c = v3(x.x, x.y, z);
            rod(m, c - Z * 0.02, c + Z * 0.02, PIPE_R + 0.006, n);
            let back = h(face) + o * 0.005;
            rod(m, v3(back.x, back.y, z), c - o * PIPE_R, 0.006, 6);
            z += 2.0;
        }
    }
}

/// Rain head on the outside of a parapet at `at` (wall face point), its top at `z`.
pub fn rain_head(d: &mut Detail, at: V2, out: V2, z: f64, lod: Lod) {
    let o = h(out).normalized();
    let along = v3(-o.y, o.x, 0.0);
    let (w, dep, ht) = (0.25, 0.18, 0.25);
    // tapering hopper in (along, z), run outward
    let sec = if lod == Lod::Coarse {
        chamfered(-w / 2.0, -ht, w / 2.0, 0.0, 0.0)
    } else {
        vec![v2(-0.05, -ht), v2(0.05, -ht), v2(w / 2.0, -ht * 0.4), v2(w / 2.0, 0.0), v2(-w / 2.0, 0.0), v2(-w / 2.0, -ht * 0.4)]
    };
    let base = h(at) + Z * z;
    prism(d.part(Mat::Metal), &sec, base, along, Z, o * dep);
}

/// Downpipe positions along edges (each `(a, b)` with its outward normal): one per `spacing` of
/// total length, each in the middle of its equal share of the run.
pub fn spread(edges: &[(V2, V2, V2)], spacing: f64) -> Vec<(V2, V2)> {
    let total: f64 = edges.iter().map(|e| (e.1 - e.0).len()).sum();
    if total < 1e-6 || edges.is_empty() {
        return vec![];
    }
    let n = ((total / spacing.max(1.0)).ceil() as usize).max(1);
    let step = total / n as f64;
    let mut out = vec![];
    let mut want = step / 2.0;
    let mut run = 0.0;
    for (a, b, nrm) in edges {
        let l = (*b - *a).len();
        while want <= run + l + 1e-9 && out.len() < n {
            let t = ((want - run) / l.max(1e-9)).clamp(0.05, 0.95);
            out.push((*a + (*b - *a) * t, *nrm));
            want += step;
        }
        run += l;
    }
    out
}
