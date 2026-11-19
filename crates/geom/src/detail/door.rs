// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Door: lining with a planted stop, architraves on both faces, threshold, leaves (flush,
//! panelled with recessed fields, or glazed with beads), hinges and handles. Built for a swing
//! to +y with hinges at x = 0, then mirrored for the placement.

use super::opening::{DoorOp, DoorType, Handle, LeafStyle, Placement};
use super::prim::{block, chamfered, elev_sweep, rod, rounded, tube, X, Y, Z};
use super::{round_sides, Detail, Lod, Mat};
use crate::math::{v2, v3, V2, V3};

/// Clearances: leaf to frame, leaf under-cut, gap between two leaves.
const EDGE_GAP: f64 = 0.003;
const UNDERCUT: f64 = 0.008;
const MEET_GAP: f64 = 0.004;
/// Stile and rail widths of a framed leaf.
const STILE: f64 = 0.11;
const TOP_RAIL: f64 = 0.11;
const BOTTOM_RAIL: f64 = 0.2;
const MID_RAIL: f64 = 0.11;
/// Panel recess per face and the moulding bead around it.
const RECESS: f64 = 0.008;
const BEAD: f64 = 0.012;

pub fn build(t: &DoorType, p: &Placement, lod: Lod) -> Detail {
    let mut d = Detail::default();
    let (w, h) = (p.width.max(0.2), p.height.max(0.5));
    let half = (p.wall_t / 2.0).max(0.03);
    let f = t.frame.face.min(w / 6.0).max(0.01);
    lining(&mut d, t, w, h, half, f, lod);
    if lod != Lod::Coarse {
        if let Some((cw, ct)) = t.casing {
            casings(&mut d, w, h, half, f, cw, ct, lod);
        }
    }
    let sill = t.threshold.unwrap_or(0.0).max(0.0);
    if sill > 0.0 {
        threshold(&mut d, w, half, f, sill, lod);
    }
    let lt = t.leaf_t.min(1.6 * half).max(0.02);
    let (x0, x1) = (f + EDGE_GAP, w - f - EDGE_GAP);
    let (z0, z1) = (sill + UNDERCUT, h - f - EDGE_GAP);
    match t.op {
        DoorOp::Swing => {
            // the leaf closes flush with the swing face, against the stop behind it
            let y1 = half - 0.005;
            let leaves: Vec<(f64, f64, bool)> = if t.leaves >= 2 {
                let m = (x0 + x1) / 2.0;
                vec![(x0, m - MEET_GAP / 2.0, false), (m + MEET_GAP / 2.0, x1, true)]
            } else {
                vec![(x0, x1, false)]
            };
            for (a, b, hinge_hi) in leaves {
                leaf(&mut d, t, (a, b), (z0, z1), (y1 - lt, y1), lod);
                if lod != Lod::Coarse {
                    let free = if hinge_hi { a } else { b };
                    let toward_hinge = if hinge_hi { 1.0 } else { -1.0 };
                    handles(&mut d, t, free, toward_hinge, (y1 - lt, y1), lod);
                }
                if lod == Lod::Fine {
                    let edge = if hinge_hi { b } else { a };
                    hinges(&mut d, edge, y1, (z0, z1));
                }
            }
        }
        DoorOp::Sliding => {
            // two panels on two tracks, overlapping at the middle
            let m = (x0 + x1) / 2.0;
            let ov = 0.025;
            let tracks = [(half - 0.005 - lt, half - 0.005), (half - 0.01 - 2.0 * lt, half - 0.01 - lt)];
            leaf(&mut d, t, (x0, m + ov), (z0, z1), tracks[0], lod);
            leaf(&mut d, t, (m - ov, x1), (z0, z1), tracks[1], lod);
            if lod != Lod::Coarse {
                block(
                    d.part(Mat::Metal),
                    v3(f, tracks[1].0 - 0.005, z1 - 0.002),
                    v3(w - f, tracks[0].1, z1 + EDGE_GAP),
                );
                handles(&mut d, t, x0 + 0.08, 1.0, tracks[0], lod);
                handles(&mut d, t, x1 - 0.08, -1.0, tracks[1], lod);
            }
        }
    }
    if p.hinge_flip {
        d.mirror(0, w / 2.0);
    }
    if p.swing_flip {
        d.mirror(1, 0.0);
    }
    d
}

/// Lining over the reveal: jambs and head in one mitred sweep, the stop planted behind the leaf.
fn lining(d: &mut Detail, t: &DoorType, w: f64, h: f64, half: f64, f: f64, lod: Lod) {
    let path = [v2(0.0, 0.0), v2(0.0, h), v2(w, h), v2(w, 0.0)];
    let prof: Vec<V2> = if lod == Lod::Coarse {
        vec![v2(0.0, -half), v2(f, -half), v2(f, half), v2(0.0, half)]
    } else {
        let lt = t.leaf_t.min(1.6 * half).max(0.02);
        let s1 = (half - 0.005 - lt).max(-half + 0.01);
        let s0 = (s1 - t.frame.stop_depth).max(-half + 0.005);
        let st = t.frame.stop;
        vec![
            v2(0.0, -half),
            v2(f, -half),
            v2(f, s0),
            v2(f + st, s0),
            v2(f + st, s1),
            v2(f, s1),
            v2(f, half),
            v2(0.0, half),
        ]
    };
    elev_sweep(d.part(Mat::Frame), &path, false, &prof);
}

/// Architraves on both wall faces, mitred at the head, overlapping the lining edge.
#[allow(clippy::too_many_arguments)]
fn casings(d: &mut Detail, w: f64, h: f64, half: f64, f: f64, cw: f64, ct: f64, lod: Lod) {
    // counter-clockwise around the opening: the profile's +x points away from it
    let path = [v2(w, 0.0), v2(w, h), v2(0.0, h), v2(0.0, 0.0)];
    let x0 = -(f - 0.008).max(0.0);
    for sg in [1.0, -1.0] {
        let prof: Vec<V2> = match lod {
            Lod::Fine => {
                // moulded: quarter-round outer edge, a small quirk at the inner edge
                let r = ct * 0.6;
                let mut p = vec![v2(x0, 0.0), v2(x0 + cw, 0.0)];
                for i in 0..=3 {
                    let a = std::f64::consts::FRAC_PI_2 * i as f64 / 3.0;
                    p.push(v2(x0 + cw - r + r * a.cos(), ct - r + r * a.sin()));
                }
                p.push(v2(x0 + 0.004, ct));
                p.push(v2(x0, ct - 0.004));
                p
            }
            _ => chamfered(x0, 0.0, x0 + cw, ct, ct * 0.4),
        };
        let prof: Vec<V2> = prof.iter().map(|q| v2(q.x, sg * (half + q.y))).collect();
        elev_sweep(d.part(Mat::Frame), &path, false, &prof);
    }
}

/// Threshold across the reveal: bevelled both sides.
fn threshold(d: &mut Detail, w: f64, half: f64, f: f64, th: f64, lod: Lod) {
    let bev = if lod == Lod::Coarse { 0.0 } else { (th * 0.8).min(0.012) };
    let sec = [
        v2(-half, 0.0),
        v2(half, 0.0),
        v2(half - bev, th),
        v2(-half + bev, th),
    ];
    // section in (y, z), run along x
    super::prim::prism(d.part(Mat::Stone), &sec, v3(f, 0.0, 0.0), Y, Z, X * (w - 2.0 * f));
}

/// One leaf over `x`, `z`, between `y.0` (back) and `y.1` (face).
fn leaf(d: &mut Detail, t: &DoorType, x: (f64, f64), z: (f64, f64), y: (f64, f64), lod: Lod) {
    let (a, b) = x;
    let lw = b - a;
    let framed = !matches!(t.leaf, LeafStyle::Flush) && lod != Lod::Coarse && lw > 2.0 * STILE + 0.1;
    if !framed {
        // a flush leaf: arrised edges at medium and fine
        let c = if lod == Lod::Coarse { 0.0 } else { 0.003 };
        let sec = chamfered(y.0, z.0, y.1, z.1, c);
        super::prim::prism(d.part(Mat::Leaf), &sec, v3(a, 0.0, 0.0), Y, Z, X * lw);
        return;
    }
    let (rows, cols, glazed) = match t.leaf {
        LeafStyle::Panels { cols, rows } => (rows.max(1) as usize, cols.max(1) as usize, false),
        LeafStyle::Glazed { rows } => (rows.max(1) as usize, 1, true),
        LeafStyle::Flush => (1, 1, false),
    };
    let lf = d.part(Mat::Leaf);
    // stiles full height, rails between them
    block(lf, v3(a, y.0, z.0), v3(a + STILE, y.1, z.1));
    block(lf, v3(b - STILE, y.0, z.0), v3(b, y.1, z.1));
    let (fx0, fx1) = (a + STILE, b - STILE);
    let (fz0, fz1) = (z.0 + BOTTOM_RAIL, z.1 - TOP_RAIL);
    block(lf, v3(fx0, y.0, z.0), v3(fx1, y.1, fz0));
    block(lf, v3(fx0, y.0, fz1), v3(fx1, y.1, z.1));
    let ch = (fz1 - fz0 - (rows - 1) as f64 * MID_RAIL) / rows as f64;
    let cwid = (fx1 - fx0 - (cols - 1) as f64 * MID_RAIL) / cols as f64;
    if ch < 0.05 || cwid < 0.05 {
        block(lf, v3(fx0, y.0, fz0), v3(fx1, y.1, fz1));
        return;
    }
    for i in 1..rows {
        let zr = fz0 + i as f64 * (ch + MID_RAIL);
        block(lf, v3(fx0, y.0, zr - MID_RAIL), v3(fx1, y.1, zr));
    }
    for j in 1..cols {
        let xr = fx0 + j as f64 * (cwid + MID_RAIL);
        block(lf, v3(xr - MID_RAIL, y.0, fz0), v3(xr, y.1, fz1));
    }
    let mid = (y.0 + y.1) / 2.0;
    for i in 0..rows {
        for j in 0..cols {
            let (px0, pz0) = (fx0 + j as f64 * (cwid + MID_RAIL), fz0 + i as f64 * (ch + MID_RAIL));
            let (px1, pz1) = (px0 + cwid, pz0 + ch);
            if glazed {
                let gt = 0.016_f64.min(y.1 - y.0 - 0.004);
                block(d.part(Mat::Glass), v3(px0, mid - gt / 2.0, pz0), v3(px1, mid + gt / 2.0, pz1));
            } else {
                // recessed field, a raised fielding at fine
                let (p0, p1) = (y.0 + RECESS, y.1 - RECESS);
                let lf = d.part(Mat::Leaf);
                block(lf, v3(px0, p0, pz0), v3(px1, p1, pz1));
                if lod == Lod::Fine && cwid > 0.15 && ch > 0.15 {
                    let r = 0.04;
                    let sec = chamfered(pz0 + r, p0 - 0.004, pz1 - r, p1 + 0.004, 0.004);
                    let sec: Vec<V2> = sec.iter().map(|q| v2(q.y, q.x)).collect();
                    super::prim::prism(lf, &sec, v3(px0 + r, 0.0, 0.0), Y, Z, X * (cwid - 2.0 * r));
                }
            }
            if lod == Lod::Fine {
                // bead around the field on both faces, mitred at the corners
                let path = [v2(px0, pz0), v2(px0, pz1), v2(px1, pz1), v2(px1, pz0)];
                let face_in = if glazed { mid + 0.008 } else { y.1 - RECESS };
                for (face, sg) in [(face_in, 1.0), (y.0 + (y.1 - face_in), -1.0)] {
                    let bp: Vec<V2> = rounded(0.0, 0.0, BEAD, BEAD * 0.8, BEAD * 0.45, 2)
                        .iter()
                        .map(|q| v2(q.x, face + sg * q.y))
                        .collect();
                    elev_sweep(d.part(Mat::Leaf), &path, true, &bp);
                }
            }
        }
    }
}

/// Handles on both faces of a leaf whose free edge is at `free`.
fn handles(d: &mut Detail, t: &DoorType, free: f64, toward_hinge: f64, y: (f64, f64), lod: Lod) {
    let n = round_sides(lod, true);
    let nb = round_sides(lod, false);
    let m = d.part(Mat::Metal);
    let e = free + toward_hinge * 0.065;
    let hz = t.handle_z;
    for (face, out) in [(y.1, 1.0), (y.0, -1.0)] {
        let at = |dy: f64, z: f64| v3(e, face + out * dy, z);
        match t.handle {
            Handle::Lever => {
                rod(m, at(0.0, hz), at(0.009, hz), 0.026, nb);
                let lever = [
                    at(0.009, hz),
                    at(0.06, hz),
                    v3(e + toward_hinge * 0.03, face + out * 0.068, hz),
                    v3(e + toward_hinge * 0.135, face + out * 0.07, hz),
                ];
                tube(m, &lever, 0.0095, n);
                if lod == Lod::Fine {
                    // key rose under the lever
                    rod(m, at(0.0, hz - 0.075), at(0.007, hz - 0.075), 0.02, nb);
                }
            }
            Handle::Pull => {
                let (za, zb) = (hz - 0.35, hz + 0.45);
                let x = free + toward_hinge * 0.09;
                let p = |dy: f64, z: f64| v3(x, face + out * dy, z);
                rod(m, p(0.0, za + 0.05), p(0.06, za + 0.05), 0.01, n);
                rod(m, p(0.0, zb - 0.05), p(0.06, zb - 0.05), 0.01, n);
                rod(m, p(0.06, za), p(0.06, zb), 0.016, nb);
            }
        }
    }
}

/// Three butt hinges on the hinge edge, knuckles proud of the swing face.
fn hinges(d: &mut Detail, edge: f64, face: f64, z: (f64, f64)) {
    let m = d.part(Mat::Metal);
    for zc in [z.0 + 0.22, (z.0 + z.1) / 2.0 + 0.1, z.1 - 0.2] {
        let c: V3 = v3(edge, face + 0.004, zc);
        rod(m, c - Z * 0.05, c + Z * 0.05, 0.0075, 10);
    }
}
