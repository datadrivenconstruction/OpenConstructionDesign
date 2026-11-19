// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Lays out an annotation into strokes: coordinates are view metres; thicknesses, dashes and text heights are
//! sheet mm. Screen and sheet (PDF, DXF) draw the same result, so label size on paper is the
//! same at any view scale, while detail geometry stays in model dimensions.

use super::{Anno, AnnoShape, DetailFit, LeaderHead, RegionFill, SpotKind, TextAlign, TextNote};

type P = (f64, f64);

/// View scale (denominator), drawing-standard conventions (decimal separator, coordinate-mark axis
/// letters) and the string width function `(text, height mm) -> mm`.
pub struct AnnoCtx<'a> {
    pub scale: f64,
    pub sep: &'a str,
    pub north: &'a str,
    pub east: &'a str,
    pub measure: &'a dyn Fn(&str, f64) -> f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnnoStroke {
    Line {
        pts: Vec<P>,
        closed: bool,
        weight_mm: f64,
        /// Dash, gap, ... in sheet mm; empty means solid.
        dash_mm: Vec<f64>,
    },
    /// Even-odd fill; `mask` uses the sheet colour.
    Fill {
        rings: Vec<Vec<P>>,
        gray: u8,
        mask: bool,
    },
    /// `pos` is the lower-left corner of the first letter.
    Text {
        pos: P,
        h_mm: f64,
        rot_deg: f64,
        s: String,
    },
}

const PITCH: f64 = 1.4;
const SHOULDER_MM: f64 = 2.0;
const ARROW_MM: f64 = 2.5;
const HATCH_WEIGHT_MM: f64 = 0.13;
const CLOUD_ARC_MM: f64 = 10.0;
/// Limit on hatch lines per direction: a large region with a fine step does not hang the output.
const MAX_HATCH: f64 = 4000.0;

fn add(a: P, b: P) -> P {
    (a.0 + b.0, a.1 + b.1)
}
fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn mul(a: P, k: f64) -> P {
    (a.0 * k, a.1 * k)
}
fn len(a: P) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}

/// Word wrap to `wrap` mm; explicit line breaks are preserved.
fn wrap_lines(
    text: &str,
    wrap: Option<f64>,
    h: f64,
    measure: &dyn Fn(&str, f64) -> f64,
) -> Vec<String> {
    let mut out = vec![];
    for para in text.split('\n') {
        let Some(w) = wrap.filter(|w| *w > 0.0) else {
            out.push(para.to_string());
            continue;
        };
        let mut cur = String::new();
        for word in para.split(' ') {
            let cand = if cur.is_empty() {
                word.to_string()
            } else {
                format!("{cur} {word}")
            };
            if !cur.is_empty() && measure(&cand, h) > w {
                out.push(std::mem::replace(&mut cur, word.to_string()));
            } else {
                cur = cand;
            }
        }
        out.push(cur);
    }
    out
}

/// Ring points on the hatch grid: even and odd intersections give segments.
fn hatch_segments(rings: &[Vec<P>], deg: f64, step: f64) -> Vec<(P, P)> {
    let (s, c) = deg.to_radians().sin_cos();
    let rot = |p: P| (p.0 * c + p.1 * s, -p.0 * s + p.1 * c);
    let back = |p: P| (p.0 * c - p.1 * s, p.0 * s + p.1 * c);
    let local: Vec<Vec<P>> = rings
        .iter()
        .map(|r| r.iter().map(|q| rot(*q)).collect())
        .collect();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for q in local.iter().flatten() {
        lo = lo.min(q.1);
        hi = hi.max(q.1);
    }
    if hi <= lo || !step.is_finite() || step <= 0.0 {
        return vec![];
    }
    let step = step.max((hi - lo) / MAX_HATCH);
    let mut out = vec![];
    let mut k = (lo / step).ceil();
    while k * step < hi {
        let y = k * step;
        let mut xs = vec![];
        for r in &local {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if (a.1 > y) != (b.1 > y) {
                    xs.push(a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for p in xs.chunks_exact(2) {
            out.push((back((p[0], y)), back((p[1], y))));
        }
        k += 1.0;
    }
    out
}

/// A dashed-line run a→b following the pattern (dash, gap, ...) in coordinate units.
pub fn dash_segments(a: P, b: P, dash: &[f64]) -> Vec<(P, P)> {
    let l = len(sub(b, a));
    if l <= 0.0 || dash.iter().map(|d| d.max(0.0)).sum::<f64>() <= 0.0 {
        return vec![(a, b)];
    }
    let at = |t: f64| add(a, mul(sub(b, a), t / l));
    let mut out = vec![];
    let (mut t, mut k) = (0.0, 0usize);
    while t < l {
        let e = (t + dash[k % dash.len()].max(0.0)).min(l);
        if k % 2 == 0 {
            out.push((at(t), at(e)));
        }
        t = e;
        k += 1;
    }
    out
}

fn circle(c: P, r: f64) -> Vec<P> {
    (0..10)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 10.0;
            (c.0 + r * a.cos(), c.1 + r * a.sin())
        })
        .collect()
}

fn signed_area(r: &[P]) -> f64 {
    (0..r.len())
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

fn cloud_outline(ring: &[P], chord: f64) -> Vec<P> {
    // arcs bulge outward: for a counter-clockwise ring the exterior side is right of the edge
    let out = if signed_area(ring) >= 0.0 { 1.0 } else { -1.0 };
    let mut pts = vec![];
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
        let d = sub(b, a);
        let l = len(d);
        if l <= 0.0 {
            continue;
        }
        let n = ((l / chord).round() as usize).clamp(1, 400);
        let u = mul(d, 1.0 / l);
        let nrm = (u.1 * out, -u.0 * out);
        let c = l / n as f64;
        for k in 0..n {
            let p0 = add(a, mul(u, c * k as f64));
            for j in 0..6 {
                let t = j as f64 / 6.0;
                let bump = (t * std::f64::consts::PI).sin() * c * 0.3;
                pts.push(add(add(p0, mul(u, c * t)), mul(nrm, bump)));
            }
        }
    }
    pts
}

fn elev_text(e: f64, sep: &str) -> String {
    let num = |v: f64| format!("{v:.3}").replace('.', sep);
    if e.abs() < 5e-4 {
        format!("±{}", num(0.0))
    } else if e > 0.0 {
        format!("+{}", num(e))
    } else {
        format!("-{}", num(-e))
    }
}

struct Out<'a> {
    v: Vec<AnnoStroke>,
    c: &'a AnnoCtx<'a>,
    k: f64,
}

impl Out<'_> {
    fn line(&mut self, pts: Vec<P>, closed: bool, weight_mm: f64, dash_mm: &[f64]) {
        self.v.push(AnnoStroke::Line {
            pts,
            closed,
            weight_mm,
            dash_mm: dash_mm.to_vec(),
        });
    }
    fn fill(&mut self, rings: Vec<Vec<P>>, gray: u8, mask: bool) {
        self.v.push(AnnoStroke::Fill { rings, gray, mask });
    }
    fn text(&mut self, pos: P, h_mm: f64, rot_deg: f64, s: &str, bold: bool) {
        if s.is_empty() {
            return;
        }
        let mut put = |pos: P| {
            self.v.push(AnnoStroke::Text {
                pos,
                h_mm,
                rot_deg,
                s: s.to_string(),
            })
        };
        put(pos);
        if bold {
            let (sn, cs) = rot_deg.to_radians().sin_cos();
            let d = 0.04 * h_mm * self.k;
            put(add(pos, (d * cs, d * sn)));
        }
    }
    /// Arrowhead at `tip`, the line comes from `from`.
    fn head(&mut self, kind: LeaderHead, tip: P, from: P, w: f64) {
        let k = self.k;
        let d = sub(from, tip);
        let l = len(d);
        if l <= 0.0 {
            return;
        }
        let u = mul(d, 1.0 / l);
        let n = (-u.1, u.0);
        match kind {
            LeaderHead::Arrow => {
                let base = add(tip, mul(u, ARROW_MM * k));
                let hw = ARROW_MM / 5.0 * k;
                self.fill(
                    vec![vec![tip, add(base, mul(n, hw)), sub(base, mul(n, hw))]],
                    0,
                    false,
                );
            }
            LeaderHead::Dot => self.fill(vec![circle(tip, 0.6 * k)], 0, false),
            LeaderHead::Tick => {
                let t = mul(add(u, n), 0.9 * k);
                self.line(vec![sub(tip, t), add(tip, t)], false, w * 2.0, &[]);
            }
            LeaderHead::None => {}
        }
    }
}

fn text_note(o: &mut Out, a: &Anno, t: &TextNote) {
    let (k, st) = (o.k, &a.text);
    let measure = o.c.measure;
    let h = st.height_mm;
    let lines = wrap_lines(&t.text, t.wrap_mm, h, measure);
    let widths: Vec<f64> = lines.iter().map(|l| measure(l, h)).collect();
    let w = widths.iter().copied().fold(0.0, f64::max);
    let total = h + (lines.len().saturating_sub(1)) as f64 * h * PITCH;
    let x0 = match t.align {
        TextAlign::Left => 0.0,
        TextAlign::Center => -w / 2.0,
        TextAlign::Right => -w,
    };
    let (sn, cs) = t.rot_deg.to_radians().sin_cos();
    let at = |x: f64, y: f64| add(t.pos, mul((x * cs - y * sn, x * sn + y * cs), k));
    for (i, (l, lw)) in lines.iter().zip(&widths).enumerate() {
        let lx = match t.align {
            TextAlign::Left => x0,
            TextAlign::Center => x0 + (w - lw) / 2.0,
            TextAlign::Right => x0 + w - lw,
        };
        let by = -h - i as f64 * h * PITCH;
        o.text(at(lx, by), h, t.rot_deg, l, st.bold);
        if st.underline && *lw > 0.0 {
            let y = by - 0.18 * h;
            o.line(
                vec![at(lx, y), at(lx + lw, y)],
                false,
                a.line.weight_mm,
                &[],
            );
        }
    }
    let pad = 0.5 * h;
    if st.frame {
        let (l, r, top, bot) = (x0 - pad, x0 + w + pad, pad, -total - pad);
        o.line(
            vec![at(l, bot), at(r, bot), at(r, top), at(l, top)],
            true,
            a.line.weight_mm,
            &[],
        );
    }
    for ld in &t.leaders {
        // the frame side nearest the target, then the shelf, then the bend and the target
        let aim = ld.elbow.unwrap_or(ld.to);
        let d = sub(aim, t.pos);
        let local_x = (d.0 * cs + d.1 * sn) / k.max(1e-12);
        let right = local_x >= x0 + w / 2.0;
        let (sx, dir) = if right {
            (x0 + w + pad, 1.0)
        } else {
            (x0 - pad, -1.0)
        };
        let y = -total / 2.0;
        let mut pts = vec![at(sx, y), at(sx + dir * SHOULDER_MM, y)];
        pts.extend(ld.elbow);
        pts.push(ld.to);
        let n = pts.len();
        let from = pts[n - 2];
        o.line(pts, false, a.line.weight_mm, &[]);
        o.head(t.head, ld.to, from, a.line.weight_mm);
    }
}

fn detail(o: &mut Out, a: &Anno, at: P, rot_deg: f64, length: Option<f64>, flip: bool) {
    let Some(def) = &a.detail else { return };
    let (sn, cs) = rot_deg.to_radians().sin_cos();
    let y = if flip { -1.0 } else { 1.0 };
    let place = |p: P, sx: f64, dx: f64| {
        let (x, py) = (p.0 * sx + dx, p.1 * y);
        add(at, (x * cs - py * sn, x * sn + py * cs))
    };
    let want = length.filter(|l| *l > 0.0).unwrap_or(def.length);
    let (copies, sx) = match def.fit {
        DetailFit::Fixed => (1, 1.0),
        DetailFit::Stretch => (
            1,
            if def.length > 0.0 {
                want / def.length
            } else {
                1.0
            },
        ),
        DetailFit::Repeat => {
            let n = if def.length > 0.0 {
                ((want / def.length).round() as usize).clamp(1, 2000)
            } else {
                1
            };
            (
                n,
                if def.length > 0.0 {
                    want / (n as f64 * def.length)
                } else {
                    1.0
                },
            )
        }
    };
    let step = def.length * sx;
    for i in 0..copies {
        let dx = i as f64 * step;
        for s in &def.strokes {
            let pts: Vec<P> = s.pts.iter().map(|p| place(*p, sx, dx)).collect();
            if s.fill {
                o.fill(vec![pts], 0, false);
            } else {
                o.line(pts, s.closed, a.line.weight_mm, &a.line.dash);
            }
        }
    }
}

fn spot(o: &mut Out, a: &Anno, kind: SpotKind, at: P, head: P) {
    let (k, h) = (o.k, a.text.height_mm);
    let v = a.spot.unwrap_or_default();
    let num = |x: f64| format!("{x:.3}").replace('.', o.c.sep);
    let lines: Vec<String> = match kind {
        SpotKind::Elevation if v.free => vec![format!("~{}", elev_text(v.z, o.c.sep))],
        SpotKind::Elevation => vec![elev_text(v.z, o.c.sep)],
        SpotKind::Coordinate => vec![
            format!("{} {}", o.c.north, num(v.north)),
            format!("{} {}", o.c.east, num(v.east)),
        ],
    };
    let w = lines
        .iter()
        .map(|l| (o.c.measure)(l, h))
        .fold(0.0, f64::max);
    let dir = if head.0 >= at.0 { 1.0 } else { -1.0 };
    let end = add(head, (dir * w * k, 0.0));
    let wt = a.line.weight_mm;
    o.line(vec![at, head, end], false, wt, &[]);
    // triangle with its tip at the point
    let s = 1.4 * k;
    o.line(
        vec![at, add(at, (-s, 2.0 * s)), add(at, (s, 2.0 * s))],
        true,
        wt,
        &[],
    );
    let x = if dir > 0.0 { head.0 } else { head.0 - w * k };
    let gap = 0.6 * k;
    o.text((x, head.1 + gap), h, 0.0, &lines[0], a.text.bold);
    if let Some(second) = lines.get(1) {
        o.text((x, head.1 - gap - h * k), h, 0.0, second, a.text.bold);
    }
}

/// Extent of strokes [x0, y0, x1, y1] in view coordinates; a label counts with its width and rotation.
pub fn strokes_bbox(strokes: &[AnnoStroke], c: &AnnoCtx) -> Option<[f64; 4]> {
    let k = c.scale / 1000.0;
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut add_pt = |p: P| {
        bb[0] = bb[0].min(p.0);
        bb[1] = bb[1].min(p.1);
        bb[2] = bb[2].max(p.0);
        bb[3] = bb[3].max(p.1);
    };
    for st in strokes {
        match st {
            AnnoStroke::Line { pts, .. } => pts.iter().for_each(|p| add_pt(*p)),
            AnnoStroke::Fill { rings, .. } => rings.iter().flatten().for_each(|p| add_pt(*p)),
            AnnoStroke::Text {
                pos,
                h_mm,
                rot_deg,
                s,
            } => {
                let (w, h) = ((c.measure)(s, *h_mm) * k, h_mm * k);
                let (sn, cs) = rot_deg.to_radians().sin_cos();
                for (x, y) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
                    add_pt((pos.0 + x * cs - y * sn, pos.1 + x * sn + y * cs));
                }
            }
        }
    }
    (bb[0] <= bb[2]).then_some(bb)
}

impl Anno {
    /// Annotation strokes at scale `c.scale`.
    pub fn strokes(&self, c: &AnnoCtx) -> Vec<AnnoStroke> {
        let k = c.scale / 1000.0;
        let mut o = Out { v: vec![], c, k };
        match &self.shape {
            AnnoShape::Text(t) => text_note(&mut o, self, t),
            AnnoShape::Line { pts, closed } => {
                o.line(pts.clone(), *closed, self.line.weight_mm, &self.line.dash)
            }
            AnnoShape::Region {
                rings,
                fill,
                boundary,
            } => {
                match *fill {
                    RegionFill::Solid { gray } => o.fill(rings.clone(), gray, false),
                    RegionFill::Mask => o.fill(rings.clone(), 255, true),
                    RegionFill::Hatch {
                        deg,
                        step_mm,
                        cross,
                    } => {
                        let step = step_mm * k;
                        let mut segs = hatch_segments(rings, deg, step);
                        if cross {
                            segs.extend(hatch_segments(rings, deg + 90.0, step));
                        }
                        for (a, b) in segs {
                            o.line(vec![a, b], false, HATCH_WEIGHT_MM, &[]);
                        }
                    }
                }
                if *boundary {
                    for r in rings {
                        o.line(r.clone(), true, self.line.weight_mm, &self.line.dash);
                    }
                }
            }
            AnnoShape::Detail {
                at,
                rot_deg,
                length,
                flip,
                ..
            } => detail(&mut o, self, *at, *rot_deg, *length, *flip),
            AnnoShape::Spot { kind, at, head } => spot(&mut o, self, *kind, *at, *head),
            AnnoShape::Cloud { ring, mark } => {
                let chord = (CLOUD_ARC_MM * k).max(1e-6);
                o.line(
                    cloud_outline(ring, chord),
                    true,
                    self.line.weight_mm.max(0.35),
                    &[],
                );
                if !mark.is_empty() {
                    let h = self.text.height_mm;
                    let side = (h * 2.6) * k;
                    let c0 = add(ring[0], (side * 0.8, side * 0.8));
                    let tri = vec![
                        add(c0, (-side / 2.0, -side * 0.35)),
                        add(c0, (side / 2.0, -side * 0.35)),
                        add(c0, (0.0, side * 0.55)),
                    ];
                    o.line(tri, true, self.line.weight_mm, &[]);
                    let w = (c.measure)(mark, h) * k;
                    o.text(add(c0, (-w / 2.0, -side * 0.2)), h, 0.0, mark, false);
                }
            }
        }
        o.v
    }
}

fn seg_dist(p: P, a: P, b: P) -> f64 {
    let d = sub(b, a);
    let l2 = d.0 * d.0 + d.1 * d.1;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    len(sub(p, add(a, mul(d, t))))
}

/// Hit test against strokes: lines within radius `r`, fills inside the outline, text inside its box.
pub fn stroke_hit(strokes: &[AnnoStroke], p: P, r: f64, c: &AnnoCtx) -> bool {
    let k = c.scale / 1000.0;
    strokes.iter().any(|s| match s {
        AnnoStroke::Line { pts, closed, .. } => {
            let n = pts.len();
            let m = if *closed { n } else { n.saturating_sub(1) };
            (0..m).any(|i| seg_dist(p, pts[i], pts[(i + 1) % n]) <= r)
        }
        AnnoStroke::Fill { rings, .. } => super::inside(p, rings),
        AnnoStroke::Text {
            pos,
            h_mm,
            rot_deg,
            s,
        } => {
            let (sn, cs) = rot_deg.to_radians().sin_cos();
            let d = sub(p, *pos);
            let (x, y) = (d.0 * cs + d.1 * sn, -d.0 * sn + d.1 * cs);
            let (w, h) = ((c.measure)(s, *h_mm) * k, h_mm * k);
            x >= -r && x <= w + r && y >= -r && y <= h + r
        }
    })
}
