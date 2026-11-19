// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Door swings from the skeleton pixels no straight segment took: each 8-connected group is
//! fitted with a circle; a group on a circle of door radius over roughly a quarter turn is a
//! swing. A straight segment from the centre to an arc end is its leaf and is snapped onto it,
//! so the wall finder drops it exactly as it does for a vector drawing.

use super::lines::{PxSeg, P};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PxArc {
    pub c: P,
    pub r: f64,
    /// Counter-clockwise in image axes (y down), a0 < a1, radians.
    pub a0: f64,
    pub a1: f64,
}

/// Circle through the points by algebraic least squares: centre, radius, rms of the distance.
fn circle(pts: &[P]) -> Option<(P, f64, f64)> {
    if pts.len() < 5 {
        return None;
    }
    // centred for conditioning
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
    let (mut suu, mut svv, mut suv, mut suuu, mut svvv, mut suvv, mut svuu) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in pts {
        let (u, v) = (p.0 - mx, p.1 - my);
        suu += u * u;
        svv += v * v;
        suv += u * v;
        suuu += u * u * u;
        svvv += v * v * v;
        suvv += u * v * v;
        svuu += v * u * u;
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-9 {
        return None;
    }
    let (b1, b2) = (0.5 * (suuu + suvv), 0.5 * (svvv + svuu));
    let uc = (b1 * svv - b2 * suv) / det;
    let vc = (suu * b2 - suv * b1) / det;
    let r = (uc * uc + vc * vc + (suu + svv) / n).sqrt();
    let c = (uc + mx, vc + my);
    if !r.is_finite() {
        return None;
    }
    let rms = (pts.iter().map(|p| ((p.0 - c.0).hypot(p.1 - c.1) - r).powi(2)).sum::<f64>() / n).sqrt();
    Some((c, r, rms))
}

/// Swings with radius in `r_px` (pixels) from the leftover skeleton.
pub(crate) fn arcs(w: usize, h: usize, skel: &[bool], r_px: (f64, f64)) -> Vec<PxArc> {
    let mut seen = vec![false; w * h];
    let mut out = vec![];
    let min_pts = (r_px.0 * 0.6).max(8.0) as usize;
    for start in 0..w * h {
        if !skel[start] || seen[start] {
            continue;
        }
        let mut comp = vec![start];
        seen[start] = true;
        let mut k = 0;
        while k < comp.len() {
            let i = comp[k];
            k += 1;
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if skel[j] && !seen[j] {
                        seen[j] = true;
                        comp.push(j);
                    }
                }
            }
        }
        if comp.len() < min_pts {
            continue;
        }
        let mut pts: Vec<P> = comp.iter().map(|&i| ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5)).collect();
        let Some(mut fit) = circle(&pts) else {
            continue;
        };
        // stubs of other strokes touching the swing (a jamb, a leaf end): trim and fit again
        for keep in [2.0, 1.2, 1.0] {
            let (c, r, _) = fit;
            pts.retain(|p| ((p.0 - c.0).hypot(p.1 - c.1) - r).abs() <= keep);
            match circle(&pts) {
                Some(f) => fit = f,
                None => break,
            }
        }
        let (c, r, rms) = fit;
        if rms > 0.8 || r < r_px.0 || r > r_px.1 || (pts.len() as f64) < 0.5 * r {
            continue;
        }
        let mid = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 - c.0, a.1 + p.1 - c.1));
        let am = mid.1.atan2(mid.0);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in &pts {
            let a = (p.1 - c.1).atan2(p.0 - c.0) - am;
            let a = (a + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
            lo = lo.min(a);
            hi = hi.max(a);
        }
        let sweep = hi - lo;
        if !(55f64.to_radians()..=125f64.to_radians()).contains(&sweep) {
            continue;
        }
        out.push(PxArc { c, r, a0: am + lo, a1: am + hi });
    }
    out
}

/// A segment through (near) the centre of a swing that ends at (near) one of its ends is the
/// leaf: it is set to run exactly from the centre to that end, and the arc end to the leaf
/// direction. A part beyond the centre (the jamb drawn in line with the leaf) is kept apart.
pub(crate) fn snap_leaves(arcs: &mut [PxArc], segs: &mut Vec<PxSeg>) {
    for a in arcs.iter_mut() {
        let tol_c = (0.08 * a.r).max(3.0);
        let mut best: Option<(f64, usize, P, bool, P)> = None;
        for (k, s) in segs.iter().enumerate() {
            for (p, q) in [(s.a, s.b), (s.b, s.a)] {
                let sl = (q.0 - p.0).hypot(q.1 - p.1);
                if sl < 1e-9 {
                    continue;
                }
                let u = ((q.0 - p.0) / sl, (q.1 - p.1) / sl);
                // centre off the segment's line, or outside it
                let along = (a.c.0 - p.0) * u.0 + (a.c.1 - p.1) * u.1;
                let off = ((a.c.0 - p.0) * u.1 - (a.c.1 - p.1) * u.0).abs();
                if off > tol_c || along < -tol_c || along > sl {
                    continue;
                }
                let dc = if along < 0.0 { -along } else { 0.0 } + off;
                let len = (q.0 - a.c.0).hypot(q.1 - a.c.1);
                if (len - a.r).abs() > (0.1 * a.r).max(4.0) {
                    continue;
                }
                let ang = (q.1 - a.c.1).atan2(q.0 - a.c.0);
                let diff = |e: f64| {
                    let d = (ang - e).rem_euclid(std::f64::consts::TAU);
                    d.min(std::f64::consts::TAU - d)
                };
                let (d0, d1) = (diff(a.a0), diff(a.a1));
                let (d, end1) = if d1 < d0 { (d1, true) } else { (d0, false) };
                if d > 10f64.to_radians() {
                    continue;
                }
                if best.is_none_or(|b| d + dc / a.r < b.0) {
                    best = Some((d + dc / a.r, k, (ang.cos(), ang.sin()), end1, p));
                }
            }
        }
        let Some((_, k, dir, end1, far)) = best else {
            continue;
        };
        let tip = (a.c.0 + a.r * dir.0, a.c.1 + a.r * dir.1);
        segs[k] = PxSeg { a: a.c, b: tip };
        let back = (far.0 - a.c.0) * dir.0 + (far.1 - a.c.1) * dir.1;
        if back < -1.0 {
            segs.push(PxSeg { a: far, b: a.c });
        }
        let ang = dir.1.atan2(dir.0);
        // keep a0 < a1 with the same sweep side
        if end1 {
            let mut e = ang;
            while e < a.a0 {
                e += std::f64::consts::TAU;
            }
            while e - a.a0 > std::f64::consts::TAU {
                e -= std::f64::consts::TAU;
            }
            a.a1 = e;
        } else {
            let mut e = ang;
            while e > a.a1 {
                e -= std::f64::consts::TAU;
            }
            while a.a1 - e > std::f64::consts::TAU {
                e += std::f64::consts::TAU;
            }
            a.a0 = e;
        }
    }
}
