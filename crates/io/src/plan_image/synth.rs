//! Test plans for plan reading: walls with openings drawn the way a plan sheet shows them
//! (walls filled or outlined, a door as leaf and quarter swing, a window as a glass line on the
//! axis), as a picture or as a vector sheet, and the score of a read against the truth:
//! length-weighted overlap of wall axes and the share of openings found.

use crate::drawing_model::{OpeningCand, WallCand};
use crate::pdf::{Item, Sheet, SheetLayer};

use super::Gray;

#[derive(Clone, Debug, PartialEq)]
pub struct SynthWall {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub thickness: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SynthOpening {
    /// Index into the plan's walls.
    pub wall: usize,
    /// Start of the opening from the wall's `a`, metres.
    pub along: f64,
    pub width: f64,
    pub door: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SynthPlan {
    pub walls: Vec<SynthWall>,
    pub openings: Vec<SynthOpening>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallStyle {
    /// Poche: walls filled black.
    Filled,
    /// Wall outlines only, two pixels wide.
    Outline,
}

type P = (f64, f64);

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn len(v: P) -> f64 {
    v.0.hypot(v.1)
}
fn unit(v: P) -> P {
    let l = len(v);
    (v.0 / l, v.1 / l)
}
fn seg_dist(p: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let l2 = ab.0 * ab.0 + ab.1 * ab.1;
    if l2 < 1e-18 {
        return len(sub(p, a));
    }
    let t = (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / l2).clamp(0.0, 1.0);
    len(sub(p, (a.0 + ab.0 * t, a.1 + ab.1 * t)))
}

/// One filled quad of a wall: axis from t0 to t1 of wall `i`, full thickness.
struct Piece {
    a: P,
    d: P,
    n: P,
    t0: f64,
    t1: f64,
    hw: f64,
}

impl Piece {
    fn corners(&self) -> [P; 4] {
        let at = |t: f64, s: f64| {
            (self.a.0 + self.d.0 * t + self.n.0 * s, self.a.1 + self.d.1 * t + self.n.1 * s)
        };
        [at(self.t0, -self.hw), at(self.t1, -self.hw), at(self.t1, self.hw), at(self.t0, self.hw)]
    }
    fn has(&self, p: P) -> bool {
        let r = sub(p, self.a);
        let (t, s) = (r.0 * self.d.0 + r.1 * self.d.1, r.0 * self.n.0 + r.1 * self.n.1);
        t >= self.t0 && t < self.t1 && s >= -self.hw && s < self.hw
    }
}

impl SynthPlan {
    /// Extent of the wall faces: (min, max).
    pub fn bounds(&self) -> (P, P) {
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for p in self.pieces() {
            for c in p.corners() {
                lo = (lo.0.min(c.0), lo.1.min(c.1));
                hi = (hi.0.max(c.0), hi.1.max(c.1));
            }
        }
        (lo, hi)
    }

    /// Wall quads between openings. An end at another wall's end (a corner) runs on to that
    /// wall's far face so the corner closes; an end inside another wall (a tee) stops at its axis.
    fn pieces(&self) -> Vec<Piece> {
        let mut out = vec![];
        for (i, w) in self.walls.iter().enumerate() {
            let l = len(sub(w.b, w.a));
            if l < 1e-9 {
                continue;
            }
            let d = unit(sub(w.b, w.a));
            let n = (-d.1, d.0);
            let ext = |p: P| {
                self.walls
                    .iter()
                    .enumerate()
                    .filter(|(j, o)| *j != i && len(sub(p, o.a)).min(len(sub(p, o.b))) < 1e-6)
                    .map(|(_, o)| o.thickness / 2.0)
                    .fold(0.0, f64::max)
            };
            let mut cuts: Vec<(f64, f64)> = self
                .openings
                .iter()
                .filter(|o| o.wall == i)
                .map(|o| (o.along, o.along + o.width))
                .collect();
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut t = -ext(w.a);
            for (c0, c1) in cuts.into_iter().chain([(l + ext(w.b), f64::INFINITY)]) {
                if c0 > t + 1e-9 {
                    out.push(Piece { a: w.a, d, n, t0: t, t1: c0, hw: w.thickness / 2.0 });
                }
                t = t.max(c1);
            }
        }
        out
    }

    /// Thin strokes of doors (leaf and swing) and windows (glass), as polylines in metres.
    fn symbols(&self) -> Vec<Vec<P>> {
        let mut out = vec![];
        for o in &self.openings {
            let Some(w) = self.walls.get(o.wall) else {
                continue;
            };
            let d = unit(sub(w.b, w.a));
            let n = (-d.1, d.0);
            let on = |t: f64, s: f64| (w.a.0 + d.0 * t + n.0 * s, w.a.1 + d.1 * t + n.1 * s);
            if o.door {
                // hinge on the face at the start of the opening, leaf square to the wall
                let hw = w.thickness / 2.0;
                let h = on(o.along, hw);
                out.push(vec![h, on(o.along, hw + o.width)]);
                let swing: Vec<P> = (0..=24)
                    .map(|k| {
                        let a = std::f64::consts::FRAC_PI_2 * k as f64 / 24.0;
                        (h.0 + o.width * (n.0 * a.cos() + d.0 * a.sin()), h.1 + o.width * (n.1 * a.cos() + d.1 * a.sin()))
                    })
                    .collect();
                out.push(swing);
            } else {
                out.push(vec![on(o.along, 0.0), on(o.along + o.width, 0.0)]);
            }
        }
        out
    }

    /// The plan as a picture at `px_per_m`, with `margin` metres of paper around the walls.
    /// Returns the image and the shift: a plan point `p` reads back at `p + shift`.
    pub fn render(&self, px_per_m: f64, margin: f64, style: WallStyle) -> (Gray, P) {
        let (lo, hi) = self.bounds();
        let k = px_per_m;
        let w = ((hi.0 - lo.0 + 2.0 * margin) * k).ceil() as usize;
        let h = ((hi.1 - lo.1 + 2.0 * margin) * k).ceil() as usize;
        let shift = (margin - lo.0, margin - lo.1);
        let hf = h as f64;
        // image pixel centre -> plan point
        let plan = |x: usize, y: usize| ((x as f64 + 0.5) / k - shift.0, (hf - y as f64 - 0.5) / k - shift.1);
        let to_px = |p: P| ((p.0 + shift.0) * k, hf - (p.1 + shift.1) * k);
        let mut fill = vec![false; w * h];
        for pc in self.pieces() {
            let cs = pc.corners().map(to_px);
            let x0 = cs.iter().map(|c| c.0).fold(f64::INFINITY, f64::min).floor().max(0.0) as usize;
            let x1 = (cs.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max).ceil() as usize).min(w);
            let y0 = cs.iter().map(|c| c.1).fold(f64::INFINITY, f64::min).floor().max(0.0) as usize;
            let y1 = (cs.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max).ceil() as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    if pc.has(plan(x, y)) {
                        fill[y * w + x] = true;
                    }
                }
            }
        }
        let mut img = Gray::new(w, h);
        match style {
            WallStyle::Filled => {
                for (i, &f) in fill.iter().enumerate() {
                    if f {
                        img.px[i] = 0;
                    }
                }
            }
            WallStyle::Outline => {
                // both pixels at each fill edge: a two-pixel line centred on the edge
                for y in 0..h {
                    for x in 0..w {
                        let i = y * w + x;
                        let differs = |dx: isize, dy: isize| {
                            let (nx, ny) = (x as isize + dx, y as isize + dy);
                            let o = nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize && fill[ny as usize * w + nx as usize];
                            o != fill[i]
                        };
                        if differs(-1, 0) || differs(1, 0) || differs(0, -1) || differs(0, 1) {
                            img.px[i] = 0;
                        }
                    }
                }
            }
        }
        for line in self.symbols() {
            for s in line.windows(2) {
                stroke(&mut img, to_px(s[0]), to_px(s[1]), 1.0);
            }
        }
        (img, shift)
    }

    /// The plan on a vector sheet at 1:`ratio` with `margin_mm` around: each wall piece as four
    /// edge lines, doors and windows as polylines. A plan point reads back at `p + shift`.
    pub fn sheet(&self, ratio: f64, margin_mm: f64) -> (Sheet, P) {
        let (lo, hi) = self.bounds();
        let mm = 1000.0 / ratio;
        let shift = (margin_mm / mm - lo.0, margin_mm / mm - lo.1);
        let to = |p: P| [(p.0 + shift.0) * mm, (p.1 + shift.1) * mm];
        let line = |pts: Vec<[f64; 2]>, layer: usize| Item::Polyline {
            pts,
            closed: false,
            weight_mm: if layer == 0 { 0.5 } else { 0.18 },
            rgb: [0, 0, 0],
            layer,
        };
        let mut items = vec![];
        for pc in self.pieces() {
            let c = pc.corners();
            for k in 0..4 {
                items.push(line(vec![to(c[k]), to(c[(k + 1) % 4])], 0));
            }
        }
        for s in self.symbols() {
            items.push(line(s.into_iter().map(to).collect(), 1));
        }
        let sheet = Sheet {
            width_mm: (hi.0 - lo.0) * mm + 2.0 * margin_mm,
            height_mm: (hi.1 - lo.1) * mm + 2.0 * margin_mm,
            layers: vec![
                SheetLayer { name: "A-WALL".into(), visible: true },
                SheetLayer { name: "A-DOOR".into(), visible: true },
            ],
            items,
        };
        (sheet, shift)
    }
}

/// A round-ended stroke `r` pixels from the segment, black.
fn stroke(img: &mut Gray, a: P, b: P, r: f64) {
    let x0 = (a.0.min(b.0) - r - 1.0).floor().max(0.0) as usize;
    let y0 = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as usize;
    let x1 = ((a.0.max(b.0) + r + 1.0).ceil() as usize).min(img.w);
    let y1 = ((a.1.max(b.1) + r + 1.0).ceil() as usize).min(img.h);
    for y in y0..y1 {
        for x in x0..x1 {
            if seg_dist((x as f64 + 0.5, y as f64 + 0.5), a, b) <= r {
                img.px[y * img.w + x] = 0;
            }
        }
    }
}

/// Score of a read against the truth.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    /// Length-weighted intersection over union of the wall axes.
    pub wall_iou: f64,
    /// Share of the truth wall length found.
    pub wall_recall: f64,
    /// Share of the read wall length that is true.
    pub wall_precision: f64,
    pub openings: usize,
    pub openings_found: usize,
    /// Read openings that match no true one.
    pub openings_extra: usize,
}

impl Score {
    pub fn opening_rate(&self) -> f64 {
        if self.openings == 0 {
            1.0
        } else {
            self.openings_found as f64 / self.openings as f64
        }
    }
}

/// Axis `a`-`b` with its thickness, in read coordinates.
struct Axis {
    a: P,
    d: P,
    l: f64,
    t: f64,
}

impl Axis {
    fn new(a: P, b: P, t: f64) -> Option<Axis> {
        let l = len(sub(b, a));
        (l > 1e-9).then(|| Axis { a, d: unit(sub(b, a)), l, t })
    }
    fn at(&self, s: f64) -> P {
        (self.a.0 + self.d.0 * s, self.a.1 + self.d.1 * s)
    }
    /// The point lies on this axis within `tol` and the thicknesses agree within `tol_t`.
    fn covers(&self, p: P, d: P, t: f64, tol: f64, tol_t: f64) -> bool {
        if (self.d.0 * d.1 - self.d.1 * d.0).abs() > 1f64.to_radians().sin() || (self.t - t).abs() > tol_t {
            return false;
        }
        let r = sub(p, self.a);
        let (s, off) = (r.0 * self.d.0 + r.1 * self.d.1, (r.0 * self.d.1 - r.1 * self.d.0).abs());
        off <= tol && s >= -tol && s <= self.l + tol
    }
}

/// Sampled length of `xs` lying on some axis of `ys`.
fn covered(xs: &[Axis], ys: &[Axis], tol: f64, tol_t: f64) -> (f64, f64) {
    let (mut total, mut hit) = (0.0, 0.0);
    for x in xs {
        let n = (x.l / 0.05).ceil().max(1.0) as usize;
        let step = x.l / n as f64;
        for k in 0..n {
            let p = x.at((k as f64 + 0.5) * step);
            total += step;
            if ys.iter().any(|y| y.covers(p, x.d, x.t, tol, tol_t)) {
                hit += step;
            }
        }
    }
    (total, hit)
}

/// Scores read walls and openings against `truth` drawn with `shift` (truth `p` reads at
/// `p + shift`). A wall matches where its axis is within `tol` of a true axis of the same
/// direction and its thickness within `tol_t`; an opening where its kind matches and its
/// centre and width are within `tol` and `tol_t`.
pub fn score(
    truth: &SynthPlan,
    shift: P,
    walls: &[WallCand],
    openings: &[OpeningCand],
    tol: f64,
    tol_t: f64,
) -> Score {
    let mv = |p: P| (p.0 + shift.0, p.1 + shift.1);
    let ts: Vec<Axis> = truth.walls.iter().filter_map(|w| Axis::new(mv(w.a), mv(w.b), w.thickness)).collect();
    let rs: Vec<Axis> = walls.iter().filter_map(|w| Axis::new(w.a, w.b, w.thickness)).collect();
    let (lt, mt) = covered(&ts, &rs, tol, tol_t);
    let (lr, mr) = covered(&rs, &ts, tol, tol_t);
    let m = (mt + mr) / 2.0;
    let union = lt + lr - m;
    let mut s = Score {
        wall_iou: if union > 0.0 { m / union } else { 1.0 },
        wall_recall: if lt > 0.0 { mt / lt } else { 1.0 },
        wall_precision: if lr > 0.0 { mr / lr } else { 1.0 },
        openings: truth.openings.len(),
        ..Score::default()
    };
    // read openings as (kind, centre, width)
    let read: Vec<(bool, P, f64)> = openings
        .iter()
        .filter_map(|o| {
            let w = walls.iter().find(|w| w.id == o.wall)?;
            let ax = Axis::new(w.a, w.b, w.thickness)?;
            Some((o.kind == "door", ax.at(o.along + o.width / 2.0), o.width))
        })
        .collect();
    let mut used = vec![false; read.len()];
    for o in &truth.openings {
        let Some(w) = truth.walls.get(o.wall) else {
            continue;
        };
        let Some(ax) = Axis::new(mv(w.a), mv(w.b), w.thickness) else {
            continue;
        };
        let c = ax.at(o.along + o.width / 2.0);
        let hit = read.iter().enumerate().position(|(k, r)| {
            !used[k] && r.0 == o.door && len(sub(r.1, c)) <= tol && (r.2 - o.width).abs() <= tol_t
        });
        if let Some(k) = hit {
            used[k] = true;
            s.openings_found += 1;
        }
    }
    s.openings_extra = used.iter().filter(|&&u| !u).count();
    s
}
