//! Window: rebated frame, mullions and a transom of T section, opening sashes with their own
//! glazing, fixed glazing in the frame, glazing beads and handles at fine detail, an outside
//! sill with a drip and an inside board. Outside is −y; the placement may mirror it.

use super::opening::{Placement, SashLayout, WindowType};
use super::prim::{chamfered, elev_sweep, prism, rod, rounded, X, Y, Z};
use super::rules::DetailRules;
use super::{round_sides, Detail, Lod, Mat};
use crate::math::{v2, v3, V2};

/// Glass edge cover inside a rebate, gap of a sash to the frame, bead size.
const BITE: f64 = 0.012;
const SASH_GAP: f64 = 0.002;
const BEAD: f64 = 0.014;
/// Sill and board thickness.
const SILL_T: f64 = 0.035;
const BOARD_T: f64 = 0.025;

/// A glazing field: its clear rectangle and whether it opens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub x: (f64, f64),
    pub z: (f64, f64),
    pub opens: bool,
}

/// Fields of a window after the frame, mullions and transom.
pub fn fields(t: &WindowType, w: f64, h: f64, r: &DetailRules) -> Vec<Field> {
    let f = face(t, w, h);
    let bar = t.bar.min(w / 8.0);
    let inner = w - 2.0 * f;
    let n = t.columns(inner, r);
    let cell = (inner - (n - 1) as f64 * bar) / n as f64;
    let rows: Vec<(f64, f64, bool)> = match t.transom(h - 2.0 * f, r) {
        Some(tl) => vec![(f, h - f - tl - bar, true), (h - f - tl, h - f, false)],
        None => vec![(f, h - f, true)],
    };
    let mut out = vec![];
    for (z0, z1, main) in rows {
        for k in 0..n {
            let x0 = f + k as f64 * (cell + bar);
            let opens = main
                && match t.sashes {
                    SashLayout::AllFixed => false,
                    SashLayout::AllCasements => true,
                    SashLayout::OuterCasements => n == 1 || k == 0 || k == n - 1,
                };
            out.push(Field {
                x: (x0, x0 + cell),
                z: (z0, z1),
                opens,
            });
        }
    }
    out
}

fn face(t: &WindowType, w: f64, h: f64) -> f64 {
    t.frame.face.min(w / 5.0).min(h / 5.0).max(0.01)
}

pub fn build(t: &WindowType, p: &Placement, lod: Lod, r: &DetailRules) -> Detail {
    let mut d = Detail::default();
    let (w, h) = (p.width.max(0.1), p.height.max(0.1));
    let half = (p.wall_t / 2.0).max(0.03);
    let f = face(t, w, h);
    let fd = t.frame.depth.min(2.0 * half);
    let c = (-t.setback).clamp(-half + fd / 2.0, half - fd / 2.0);
    let (y0, y1) = (c - fd / 2.0, c + fd / 2.0);
    let sd = t.frame.stop_depth.min(fd / 2.0);
    let st = t.frame.stop;
    // frame: clockwise outline, the stop on the outside edge of the inner face
    let path = [v2(0.0, 0.0), v2(0.0, h), v2(w, h), v2(w, 0.0)];
    let prof: Vec<V2> = match lod {
        Lod::Coarse => vec![v2(0.0, y0), v2(f, y0), v2(f, y1), v2(0.0, y1)],
        _ => {
            let ch = if lod == Lod::Fine { 0.004 } else { 0.0 };
            let mut p = vec![v2(0.0, y0), v2(f + st, y0), v2(f + st, y0 + sd), v2(f, y0 + sd)];
            if ch > 0.0 {
                p.extend([v2(f, y1 - ch), v2(f - ch, y1)]);
            } else {
                p.push(v2(f, y1));
            }
            p.push(v2(0.0, y1));
            p
        }
    };
    elev_sweep(d.part(Mat::Frame), &path, true, &prof);
    let fl = fields(t, w, h, r);
    let bar = t.bar.min(w / 8.0);
    bars(&mut d, &fl, bar, (y0, y1), sd, st, lod);
    let gt = t.glass_t.min(fd * 0.6);
    for fd_ in &fl {
        field(&mut d, t, fd_, (y0, y1), sd, gt, lod);
    }
    if let Some(proj) = t.sill {
        sill(&mut d, w, y0, -half - proj.max(0.0), lod);
    }
    if let Some(proj) = t.board {
        board(&mut d, w, y1, half + proj.max(0.0), lod);
    }
    if p.swing_flip {
        d.mirror(1, 0.0);
    }
    d
}

/// Mullions between fields of a row and transoms between rows, T section at medium and fine.
fn bars(d: &mut Detail, fl: &[Field], bar: f64, y: (f64, f64), sd: f64, st: f64, lod: Lod) {
    let sec = |half_w: f64| -> Vec<V2> {
        if lod == Lod::Coarse {
            return vec![v2(-half_w, y.0), v2(half_w, y.0), v2(half_w, y.1), v2(-half_w, y.1)];
        }
        vec![
            v2(-half_w - st, y.0),
            v2(half_w + st, y.0),
            v2(half_w + st, y.0 + sd),
            v2(half_w, y.0 + sd),
            v2(half_w, y.1),
            v2(-half_w, y.1),
            v2(-half_w, y.0 + sd),
            v2(-half_w - st, y.0 + sd),
        ]
    };
    let m = d.part(Mat::Frame);
    // mullions: between neighbours in a row
    for a in fl {
        if let Some(b) = fl.iter().find(|b| b.z == a.z && (b.x.0 - a.x.1 - bar).abs() < 1e-9) {
            let xc = (a.x.1 + b.x.0) / 2.0;
            prism(m, &sec(bar / 2.0), v3(xc, 0.0, a.z.0), X, Y, Z * (a.z.1 - a.z.0));
        }
    }
    // a transom: between the rows, across the whole inner width
    let mut zs: Vec<(f64, f64)> = fl.iter().map(|f| f.z).collect();
    zs.dedup();
    if zs.len() == 2 {
        let (lo, hi) = (zs[0].1, zs[1].0);
        let x0 = fl.iter().map(|f| f.x.0).fold(f64::MAX, f64::min);
        let x1 = fl.iter().map(|f| f.x.1).fold(f64::MIN, f64::max);
        let zc = (lo + hi) / 2.0;
        let s: Vec<V2> = sec((hi - lo) / 2.0).iter().map(|q| v2(q.x + zc, q.y)).collect();
        // section (z, y) run along x: the T stands the same way as the mullions'
        prism(m, &s, v3(x0, 0.0, 0.0), Z, Y, X * (x1 - x0));
    }
}

/// Glazing of one field: a sash with its glass, or glass bedded in the frame.
fn field(d: &mut Detail, t: &WindowType, fl: &Field, y: (f64, f64), sd: f64, gt: f64, lod: Lod) {
    let (x0, x1, z0, z1) = (fl.x.0, fl.x.1, fl.z.0, fl.z.1);
    if x1 - x0 < 0.02 || z1 - z0 < 0.02 {
        return;
    }
    let (gx0, gx1, gz0, gz1, gy) = if fl.opens && lod != Lod::Coarse {
        // sash behind the stop, its outline over the clear opening
        let sw = t.sash.min((x1 - x0) / 4.0).min((z1 - z0) / 4.0);
        let (sy0, sy1) = (y.0 + sd + SASH_GAP, y.1 - 0.004);
        let path = [
            v2(x0 + SASH_GAP, z0 + SASH_GAP),
            v2(x0 + SASH_GAP, z1 - SASH_GAP),
            v2(x1 - SASH_GAP, z1 - SASH_GAP),
            v2(x1 - SASH_GAP, z0 + SASH_GAP),
        ];
        let prof = if lod == Lod::Fine {
            chamfered(0.0, sy0, sw, sy1, 0.003)
        } else {
            vec![v2(0.0, sy0), v2(sw, sy0), v2(sw, sy1), v2(0.0, sy1)]
        };
        elev_sweep(d.part(Mat::Frame), &path, true, &prof);
        if lod == Lod::Fine {
            // handle on the lock stile, inside face, mid-height
            let n = round_sides(lod, true);
            let (hx, hz) = (x1 - SASH_GAP - sw / 2.0, (z0 + z1) / 2.0);
            let m = d.part(Mat::Metal);
            rod(m, v3(hx, sy1, hz), v3(hx, sy1 + 0.012, hz), 0.016, n);
            rod(m, v3(hx, sy1 + 0.03, hz + 0.01), v3(hx, sy1 + 0.03, hz - 0.11), 0.009, n);
            rod(m, v3(hx, sy1 + 0.012, hz), v3(hx, sy1 + 0.03, hz), 0.009, n);
        }
        let gy = (sy0 + sy1) / 2.0;
        (x0 + sw - BITE, x1 - sw + BITE, z0 + sw - BITE, z1 - sw + BITE, gy)
    } else {
        let gy = y.0 + sd + gt / 2.0 + 0.004;
        (x0 - BITE, x1 + BITE, z0 - BITE, z1 + BITE, gy)
    };
    if gx1 - gx0 < 0.01 || gz1 - gz0 < 0.01 {
        return;
    }
    let pane = chamfered(gx0, gy - gt / 2.0, gx1, gy + gt / 2.0, 0.0);
    prism(d.part(Mat::Glass), &pane, v3(0.0, 0.0, gz0), X, Y, Z * (gz1 - gz0));
    if lod == Lod::Fine {
        // bead inside the glass, round on its exposed edge, mitred at the corners
        let (bx0, bx1, bz0, bz1) = (gx0 + BITE, gx1 - BITE, gz0 + BITE, gz1 - BITE);
        let path = [v2(bx0, bz0), v2(bx0, bz1), v2(bx1, bz1), v2(bx1, bz0)];
        let face = gy + gt / 2.0;
        let bp: Vec<V2> = rounded(0.0, 0.0, BEAD, BEAD, BEAD * 0.5, 2)
            .iter()
            .map(|q| v2(q.x, face + q.y))
            .collect();
        elev_sweep(d.part(Mat::Frame), &path, true, &bp);
    }
}

/// Outside sill: falls away from the frame, nose past the wall face, drip under it.
fn sill(d: &mut Detail, w: f64, y_frame: f64, y_nose: f64, lod: Lod) {
    let ext = 0.03;
    let sec: Vec<V2> = match lod {
        Lod::Coarse => vec![v2(y_frame, 0.0), v2(y_nose, -0.012), v2(y_nose, -SILL_T), v2(y_frame, -SILL_T)],
        Lod::Medium => vec![
            v2(y_frame, 0.005),
            v2(y_nose, -0.012),
            v2(y_nose, -SILL_T),
            v2(y_frame, -SILL_T),
        ],
        Lod::Fine => vec![
            v2(y_frame, 0.005),
            v2(y_nose + 0.003, -0.012),
            v2(y_nose, -0.015),
            v2(y_nose, -SILL_T),
            v2(y_nose + 0.015, -SILL_T),
            v2(y_nose + 0.015, -SILL_T + 0.006),
            v2(y_nose + 0.022, -SILL_T + 0.006),
            v2(y_nose + 0.022, -SILL_T),
            v2(y_frame, -SILL_T),
        ],
    };
    prism(d.part(Mat::Stone), &sec, v3(-ext, 0.0, 0.0), Y, Z, X * (w + 2.0 * ext));
}

/// Inside board: from the frame to past the wall face, round nose at fine.
fn board(d: &mut Detail, w: f64, y_frame: f64, y_nose: f64, lod: Lod) {
    if y_nose - y_frame < 0.005 {
        return;
    }
    let ext = 0.03;
    let sec: Vec<V2> = if lod == Lod::Fine {
        rounded(y_frame - 0.004, -BOARD_T, y_nose, 0.0, 0.008, 2)
    } else {
        chamfered(y_frame - 0.004, -BOARD_T, y_nose, 0.0, 0.0)
    };
    prism(d.part(Mat::Board), &sec, v3(-ext, 0.0, 0.0), Y, Z, X * (w + 2.0 * ext));
}
