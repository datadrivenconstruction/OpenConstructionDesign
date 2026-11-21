// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan from a picture (plan g9): a raster plan image -> lines and door swings -> the same wall,
//! opening and room proposals a vector drawing gets (`drawing_model::propose`). No learned
//! model: ink by Otsu threshold, filled walls kept as outlines and thin strokes thinned
//! (`raster`), straight runs by a (theta, rho) vote (`lines`), swings by circle fits (`arcs`),
//! then snap to the dominant direction and pixel -> metre by the given scale. The result is a
//! proposal only; writing it is the caller's draft. `synth` renders test plans and scores reads.

mod arcs;
mod lines;
mod raster;
pub mod synth;
#[cfg(test)]
mod tests;

pub use raster::{Gray, THICK_PX};

use crate::drawing_model::{
    propose, Memory, Pen, ProposeOptions, Proposals, UArc, ULine, Underlay, WallTypeInfo,
};

/// Images above this many pixels are refused (about 40 MB of grey).
pub const MAX_PIXELS: usize = 40_000_000;
/// Layer of the lines and arcs read from a picture.
pub const LAYER: &str = "plan";

/// How pixels map to metres.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageScale {
    /// Pixels per metre of the model.
    PxPerM(f64),
    /// A scan or print: dots per inch and the drawing scale 1:N.
    Dpi { dpi: f64, ratio: f64 },
    /// Two image points (pixels, y down) and the real distance between them, metres.
    Calibrate { a: (f64, f64), b: (f64, f64), length: f64 },
    /// Overall width of the drawn walls (left to right), metres.
    Width(f64),
    /// Overall height of the drawn walls (top to bottom), metres.
    Height(f64),
}

/// What the pixel pass found, for the reply and the bench.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadStats {
    pub ink_px: usize,
    pub skeleton_px: usize,
    pub segments: usize,
    pub arcs: usize,
    /// Dominant direction of the drawing, degrees in [0, 90).
    pub angle_deg: f64,
    /// Skeleton pixels no line or swing explained.
    pub leftover_px: usize,
}

pub struct PlanRead {
    /// Lines and swings in metres, origin at the image's bottom-left corner, y up.
    pub underlay: Underlay,
    pub proposals: Proposals,
    pub px_per_m: f64,
    pub height_px: usize,
    pub stats: ReadStats,
}

impl PlanRead {
    /// An image point (pixels, y down) in the metres of `underlay` and `proposals`.
    pub fn to_model(&self, p: (f64, f64)) -> (f64, f64) {
        (p.0 / self.px_per_m, (self.height_px as f64 - p.1) / self.px_per_m)
    }
}

/// Proposal settings for a picture: tolerances of a pixel, piers down to 0.4 m less two pixels
/// (a face end lands on a pixel, so a 0.41 m pier can read 0.39), and only small closed figures
/// (fixtures) dropped as symbols.
pub fn raster_options(px_per_m: f64) -> ProposeOptions {
    let mut o = vector_options();
    let px = 1.0 / px_per_m;
    o.walls.min_length = (o.walls.min_length - 2.0 * px).max(0.5 * o.walls.min_length);
    o.walls.tol = o.walls.tol.max(px);
    o.walls.angle_tol = o.walls.angle_tol.max(0.01);
    o.walls.min_thickness = o.walls.min_thickness.max(3.5 * px).min(o.walls.max_thickness);
    o
}

/// Proposal settings for a plan read from a vector page: as for a drawing, but a closed figure
/// is a symbol only up to 0.6 m (a short wall piece between two openings is a closed loop too).
pub fn vector_options() -> ProposeOptions {
    let mut o = ProposeOptions::default();
    o.walls.min_length = 0.4;
    o.symbol_max_size = 0.6;
    o
}

fn px_per_m(scale: &ImageScale, m: &raster::Masks, w: usize) -> Result<f64, String> {
    let ok = |k: f64| {
        if k.is_finite() && k > 0.0 {
            Ok(k)
        } else {
            Err("the scale must be a positive number".to_string())
        }
    };
    match *scale {
        ImageScale::PxPerM(k) => ok(k),
        ImageScale::Dpi { dpi, ratio } => ok(dpi / 25.4 * 1000.0 / ratio),
        ImageScale::Calibrate { a, b, length } => ok((a.0 - b.0).hypot(a.1 - b.1) / length),
        ImageScale::Width(len) | ImageScale::Height(len) => {
            let walls = if m.thick.iter().any(|&v| v) { &m.thick } else { &m.ink };
            let (x0, y0, x1, y1) = raster::bbox(w, walls).ok_or("the image has no drawing")?;
            let px = if matches!(scale, ImageScale::Width(_)) { x1 - x0 + 1 } else { y1 - y0 + 1 };
            ok(px as f64 / len)
        }
    }
}

/// Moves a segment on the edge of filled ink half a pixel out, onto the real edge; a thin stroke
/// (no fill on either side) stays on its centre line.
fn onto_edge(s: &mut lines::PxSeg, thick: &[bool], w: usize, h: usize) {
    let len = s.len();
    if len <= 0.0 {
        return;
    }
    let n = (-(s.b.1 - s.a.1) / len, (s.b.0 - s.a.0) / len);
    let filled = |x: f64, y: f64| {
        let (xi, yi) = (x.floor(), y.floor());
        xi >= 0.0 && yi >= 0.0 && xi < w as f64 && yi < h as f64 && thick[yi as usize * w + xi as usize]
    };
    let mut side = 0i32;
    for f in [0.25, 0.5, 0.75] {
        let p = (s.a.0 + (s.b.0 - s.a.0) * f, s.a.1 + (s.b.1 - s.a.1) * f);
        let plus = filled(p.0 + 1.5 * n.0, p.1 + 1.5 * n.1);
        let minus = filled(p.0 - 1.5 * n.0, p.1 - 1.5 * n.1);
        side += i32::from(plus && !minus) - i32::from(minus && !plus);
    }
    let k = match side.signum() {
        1 => -0.5,
        -1 => 0.5,
        _ => return,
    };
    s.a = (s.a.0 + n.0 * k, s.a.1 + n.1 * k);
    s.b = (s.b.0 + n.0 * k, s.b.1 + n.1 * k);
}

/// Reads a plan picture into wall, opening and room proposals in metres.
pub fn read_plan(
    img: &Gray,
    scale: &ImageScale,
    types: &[WallTypeInfo],
    opt: Option<&ProposeOptions>,
) -> Result<PlanRead, String> {
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || img.px.len() != w * h {
        return Err("the image is empty".into());
    }
    if w * h > MAX_PIXELS {
        return Err(format!("the image has {} pixels, at most {MAX_PIXELS} are read", w * h));
    }
    let mut m = raster::masks(img);
    let ink_px = m.ink.iter().filter(|&&v| v).count();
    if ink_px == 0 {
        return Err("the image has no drawing".into());
    }
    let k = px_per_m(scale, &m, w)?;
    let po = opt.cloned().unwrap_or_else(|| raster_options(k));
    let skeleton_px = m.skel.iter().filter(|&&v| v).count();

    // long lines first, then swings from what is left, then the short lines
    let mut segs = lines::segments(w, h, &mut m.skel, (0.45 * k).max(10.0), true);
    let (dmin, dmax) = (po.door_width.0 * 0.8, po.door_width.1 * 1.15);
    let mut swings = arcs::arcs(w, h, &m.skel, (dmin * k, dmax * k));
    for a in &swings {
        clear_arc(a, &mut m.skel, w, h);
    }
    segs.extend(lines::segments(w, h, &mut m.skel, (0.3 * k).max(8.0), false));
    for s in &mut segs {
        onto_edge(s, &m.thick, w, h);
    }
    let dom = lines::orthogonalize(&mut segs, 1.5);
    lines::close_corners(&mut segs, 3.0);
    arcs::snap_leaves(&mut swings, &mut segs);
    // a leaf closed onto its face, then snapped to the swing centre, leaves a 1-2 px stub that
    // reads as a dimension tick; the passes take nothing under 8 px, so only snaps leave these
    segs.retain(|s| s.len() >= 3.0);

    let hf = h as f64;
    let pt = |p: (f64, f64)| (p.0 / k, (hf - p.1) / k);
    let mut u = Underlay {
        pens: vec![Pen::default()],
        ..Default::default()
    };
    for s in &segs {
        u.lines.push(ULine { layer: LAYER.into(), a: pt(s.a), b: pt(s.b), dashed: false, pen: 0 });
    }
    // y flips: an image angle a is -a in the model, so [a0, a1] becomes [-a1, -a0]
    for a in &swings {
        u.arcs.push(UArc { layer: LAYER.into(), c: pt(a.c), r: a.r / k, a0: -a.a1, a1: -a.a0, pen: 0 });
    }
    let proposals = propose(&u, types, &po, &Memory::default());
    let stats = ReadStats {
        ink_px,
        skeleton_px,
        segments: segs.len(),
        arcs: swings.len(),
        // the pixel rows run down: a drawing turned by +a in the model leans by -a in the image
        angle_deg: (-dom.to_degrees()).rem_euclid(90.0),
        leftover_px: m.skel.iter().filter(|&&v| v).count(),
    };
    Ok(PlanRead { underlay: u, proposals, px_per_m: k, height_px: h, stats })
}

/// Clears the skeleton pixels on a found swing so the short-line pass does not cut it into chords.
fn clear_arc(a: &arcs::PxArc, skel: &mut [bool], w: usize, h: usize) {
    let (x0, x1) = ((a.c.0 - a.r - 3.0).floor().max(0.0) as usize, ((a.c.0 + a.r + 3.0).ceil() as usize).min(w));
    let (y0, y1) = ((a.c.1 - a.r - 3.0).floor().max(0.0) as usize, ((a.c.1 + a.r + 3.0).ceil() as usize).min(h));
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f64 + 0.5 - a.c.0, y as f64 + 0.5 - a.c.1);
            if (px.hypot(py) - a.r).abs() > 1.5 {
                continue;
            }
            let t = py.atan2(px);
            let mid = (a.a0 + a.a1) / 2.0;
            let d = (t - mid + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
            if d.abs() <= (a.a1 - a.a0) / 2.0 + 0.05 {
                skel[y * w + x] = false;
            }
        }
    }
}
