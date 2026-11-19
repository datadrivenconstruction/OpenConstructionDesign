// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Text width from the built-in PDF font metrics, rendering of model dimension objects
//! and label layout (room tags, dimension texts) without overlaps.

use std::sync::OnceLock;

use ddc_geom::label::{self, Label, Rect};
use ddc_model::{DimGeom, Side};
use serde_json::Value;

use super::{arc_pts, fmt_num, format_length, num, paper_mm, LengthUnits, Pen, Weights, DIMS};

/// (units per em, code point widths 0..=0xFFFF; 0 — no glyph).
fn metrics() -> &'static (f64, Vec<u16>) {
    static M: OnceLock<(f64, Vec<u16>)> = OnceLock::new();
    M.get_or_init(|| {
        let Ok(face) = ttf_parser::Face::parse(crate::pdf::EMBEDDED_FONT, 0) else {
            return (1000.0, vec![]);
        };
        let mut w = vec![0u16; 0x10000];
        for (c, slot) in w.iter_mut().enumerate() {
            if let Some(g) = char::from_u32(c as u32).and_then(|ch| face.glyph_index(ch)) {
                *slot = face.glyph_hor_advance(g).unwrap_or(0);
            }
        }
        (face.units_per_em().max(1) as f64, w)
    })
}

/// Width of a line of height `h` from the built-in font's glyph widths (no kerning — PDF does not
/// apply it). No glyph: wide characters (CJK) — 1 em, others — 0.55 em.
pub fn text_width(s: &str, h: f64) -> f64 {
    let (upem, w) = metrics();
    let em: f64 = s
        .chars()
        .map(|c| match w.get(c as usize).copied().unwrap_or(0) {
            0 if c as u32 >= 0x2E80 => 1.0,
            0 => 0.55,
            a => a as f64 / upem,
        })
        .sum();
    em * h
}

/// A label awaiting layout: lines (offset from the anchor, text), anchor, rotation, candidates.
pub(super) struct Pending {
    pub lines: Vec<([f64; 2], String)>,
    pub h: f64,
    pub rot: f64,
    pub layer: usize,
    pub moves: Vec<[f64; 2]>,
}

/// Line extents: from the descender (−0.25 h) to the cap height (0.8 h), with rotation.
pub(super) fn text_rect(p: [f64; 2], h: f64, rot: f64, t: &str) -> Rect {
    let (s, c) = rot.to_radians().sin_cos();
    let w = text_width(t, h);
    let mut r = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in [(0.0, -0.25), (w, -0.25), (w, 0.8), (0.0, 0.8)] {
        let y = y * h;
        let q = [p[0] + x * c - y * s, p[1] + x * s + y * c];
        r = [
            r[0].min(q[0]),
            r[1].min(q[1]),
            r[2].max(q[0]),
            r[3].max(q[1]),
        ];
    }
    r
}

impl Pending {
    pub(super) fn rect(&self) -> Rect {
        let mut r = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for (p, t) in &self.lines {
            let q = text_rect(*p, self.h, self.rot, t);
            r = [
                r[0].min(q[0]),
                r[1].min(q[1]),
                r[2].max(q[2]),
                r[3].max(q[3]),
            ];
        }
        r
    }
}

/// Lays out the view's labels and emits them; obstacles are the texts of this view already emitted.
pub(super) fn flush(pen: &mut Pen, items: Vec<Pending>) {
    let obstacles: Vec<Rect> = pen
        .out
        .iter()
        .filter(|(v, _)| *v == pen.vp)
        .filter_map(|(_, m)| match m {
            super::Mark::Text { pos, h, rot, s, .. } => Some(text_rect(*pos, *h, *rot, s)),
            _ => None,
        })
        .collect();
    let labels: Vec<Label> = items
        .iter()
        .map(|p| Label {
            rect: p.rect(),
            moves: p.moves.clone(),
        })
        .collect();
    let pick = label::place(&obstacles, &labels);
    for (p, k) in items.iter().zip(pick) {
        let m = p.moves.get(k).copied().unwrap_or([0.0, 0.0]);
        for (q, t) in &p.lines {
            pen.text([q[0] + m[0], q[1] + m[1]], p.h, p.rot, t, p.layer);
        }
    }
}

fn norm(v: [f64; 2]) -> Option<[f64; 2]> {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    (l > 1e-9).then(|| [v[0] / l, v[1] / l])
}

/// Text rotation along `u`, readable from the bottom/right: (−90°, 90°].
fn readable(u: [f64; 2]) -> f64 {
    let a = u[1].atan2(u[0]).to_degrees();
    let a = if a > 90.0 + 1e-9 {
        a - 180.0
    } else if a <= -90.0 + 1e-9 {
        a + 180.0
    } else {
        a
    };
    (a * 1e9).round() / 1e9
}

/// Text centred on segment `mid` along `u`, offset by `n·gap`; candidates: along the line and on the other side.
fn along(s: String, mid: [f64; 2], u: [f64; 2], n: [f64; 2], h: f64, gap: f64) -> Pending {
    let rot = readable(u);
    let (sn, cs) = rot.to_radians().sin_cos();
    let (d, up) = ([cs, sn], [-sn, cs]);
    let w = text_width(&s, h);
    // the baseline is above the dimension line by gap on the n side
    let side = if up[0] * n[0] + up[1] * n[1] >= 0.0 {
        1.0
    } else {
        -1.0
    };
    let off = if side > 0.0 { gap } else { -(gap + 0.8 * h) };
    let p = [
        mid[0] - d[0] * w / 2.0 + up[0] * off,
        mid[1] - d[1] * w / 2.0 + up[1] * off,
    ];
    let flip = -side * (2.0 * gap + 0.8 * h);
    let k = w + 1.0;
    let moves = vec![
        [0.0, 0.0],
        [d[0] * k, d[1] * k],
        [-d[0] * k, -d[1] * k],
        [up[0] * flip, up[1] * flip],
        [d[0] * 2.0 * k, d[1] * 2.0 * k],
        [-d[0] * 2.0 * k, -d[1] * 2.0 * k],
    ];
    Pending {
        lines: vec![(p, s)],
        h,
        rot,
        layer: DIMS,
        moves,
    }
}

/// An internal dimension waiting for its text to find a free place: the line is drawn only
/// when it does.
pub(super) struct InnerDim {
    da: [f64; 2],
    db: [f64; 2],
    u: [f64; 2],
    n: [f64; 2],
    text: Pending,
}

impl Ctx<'_> {
    /// The internal dimension between paper points `pa` and `pb` facing `side`; None when it
    /// is shorter on paper than the standard's `inner_min`.
    pub fn inner(&self, side: Side, pa: [f64; 2], pb: [f64; 2], value: f64, radius: bool) -> Option<InnerDim> {
        let n = match side {
            Side::South => [0.0, -1.0],
            Side::North => [0.0, 1.0],
            Side::West => [-1.0, 0.0],
            Side::East => [1.0, 0.0],
        };
        let u = norm([pb[0] - pa[0], pb[1] - pa[1]])?;
        let len = (pb[0] - pa[0]).hypot(pb[1] - pa[1]);
        if len < num(self.dim, "inner_min") {
            return None;
        }
        let off = num(self.dim, "inner_gap");
        let sh = |p: [f64; 2]| [p[0] + n[0] * off, p[1] + n[1] * off];
        let (da, db) = (sh(pa), sh(pb));
        let mid = [(da[0] + db[0]) / 2.0, (da[1] + db[1]) / 2.0];
        let text = along(
            format!("{}{}", if radius { "R" } else { "" }, format_length(value, self.units)),
            mid,
            u,
            n,
            self.wt.dim_text,
            num(self.dim, "text_gap"),
        );
        Some(InnerDim { da, db, u, n, text })
    }

    /// Draws the internal dimensions whose text has a place clear of every text already on
    /// the view and of the texts of the dimensions drawn before it; the rest are left out.
    /// Returns how many were drawn.
    pub fn flush_inner(&self, pen: &mut Pen, items: Vec<InnerDim>) -> usize {
        const CLEAR: f64 = 0.3;
        let grow = |r: Rect| [r[0] - CLEAR, r[1] - CLEAR, r[2] + CLEAR, r[3] + CLEAR];
        let hit = |a: &Rect, b: &Rect| a[2].min(b[2]) > a[0].max(b[0]) && a[3].min(b[3]) > a[1].max(b[1]);
        let mut placed: Vec<Rect> = pen
            .out
            .iter()
            .filter(|(v, _)| *v == pen.vp)
            .filter_map(|(_, m)| match m {
                super::Mark::Text { pos, h, rot, s, .. } => Some(grow(text_rect(*pos, *h, *rot, s))),
                _ => None,
            })
            .collect();
        let mut drawn = 0;
        for it in items {
            let r = grow(it.text.rect());
            let Some(m) = it
                .text
                .moves
                .iter()
                .find(|m| !placed.iter().any(|p| hit(&label::shifted(&r, **m), p)))
            else {
                continue;
            };
            placed.push(label::shifted(&r, *m));
            pen.line(vec![it.da, it.db], false, self.wt.dim, DIMS);
            self.tick(pen, it.da, it.u, it.n);
            self.tick(pen, it.db, [-it.u[0], -it.u[1]], it.n);
            for (q, t) in &it.text.lines {
                pen.text([q[0] + m[0], q[1] + m[1]], it.text.h, it.text.rot, t, it.text.layer);
            }
            drawn += 1;
        }
        drawn
    }
}

/// Room tag: three centred lines; candidates — shifts up/down/sideways.
pub(super) fn room_tag(c: [f64; 2], lines: &[&str], h: f64, layer: usize) -> Pending {
    let ls = lines
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let y = c[1] + (lines.len() as f64 - 2.0) * h * 0.8 - i as f64 * h * 1.6;
            ([c[0] - text_width(s, h) / 2.0, y], s.to_string())
        })
        .collect();
    let s = 1.6 * h;
    let moves = vec![
        [0.0, 0.0],
        [0.0, -s],
        [0.0, s],
        [0.0, -2.0 * s],
        [0.0, 2.0 * s],
        [-3.0 * s, 0.0],
        [3.0 * s, 0.0],
    ];
    Pending {
        lines: ls,
        h,
        rot: 0.0,
        layer,
        moves,
    }
}

pub(super) fn elev_text(e: f64, sep: &str) -> String {
    if e.abs() < 5e-4 {
        format!("±{}", fmt_num(0.0, 3, sep))
    } else if e > 0.0 {
        format!("+{}", fmt_num(e, 3, sep))
    } else {
        format!("-{}", fmt_num(-e, 3, sep))
    }
}

pub(super) struct Ctx<'a> {
    pub dim: &'a Value,
    pub wt: &'a Weights,
    pub scale: f64,
    pub units: LengthUnits,
    pub sep: &'a str,
}

impl Ctx<'_> {
    fn tick(&self, pen: &mut Pen, p: [f64; 2], u: [f64; 2], n: [f64; 2]) {
        let sz = num(self.dim, "size");
        if super::text(self.dim, "terminator") == "tick" {
            let t = [
                (u[0] + n[0]) * sz / 2.0 / 2f64.sqrt(),
                (u[1] + n[1]) * sz / 2.0 / 2f64.sqrt(),
            ];
            pen.line(
                vec![[p[0] - t[0], p[1] - t[1]], [p[0] + t[0], p[1] + t[1]]],
                false,
                self.wt.outline,
                DIMS,
            );
        } else {
            let base = [p[0] + u[0] * sz, p[1] + u[1] * sz];
            let hw = sz / 6.0;
            pen.line(
                vec![
                    p,
                    [base[0] + n[0] * hw, base[1] + n[1] * hw],
                    [base[0] - n[0] * hw, base[1] - n[1] * hw],
                ],
                true,
                self.wt.dim,
                DIMS,
            );
        }
    }

    /// Paper offset of row `tier` (< 0 — none).
    fn tier(&self, tier: i32) -> f64 {
        if tier < 0 {
            0.0
        } else {
            num(self.dim, "first") + tier as f64 * num(self.dim, "step")
        }
    }

    /// Draws a dimension; returns the label for layout. `tf` — model (m) → sheet (mm).
    pub fn draw(
        &self,
        pen: &mut Pen,
        tf: &dyn Fn((f64, f64)) -> [f64; 2],
        tier: i32,
        g: &DimGeom,
    ) -> Option<Pending> {
        let (gap, over) = (num(self.dim, "gap"), num(self.dim, "overshoot"));
        let (wd, th, tg) = (self.wt.dim, self.wt.dim_text, num(self.dim, "text_gap"));
        let extra = self.tier(tier);
        match g {
            DimGeom::Linear(l) => {
                let n = [l.n.x, l.n.y];
                let (la, lb) = l.line(0.0);
                let sh = |p: [f64; 2]| [p[0] + n[0] * extra, p[1] + n[1] * extra];
                let (da, db) = (sh(tf((la.x, la.y))), sh(tf((lb.x, lb.y))));
                let (pa, pb) = (tf((l.a.x, l.a.y)), tf((l.b.x, l.b.y)));
                let u = norm([db[0] - da[0], db[1] - da[1]])?;
                pen.line(vec![da, db], false, wd, DIMS);
                for (p, d) in [(pa, da), (pb, db)] {
                    let e = norm([d[0] - p[0], d[1] - p[1]]).unwrap_or(n);
                    let len = ((d[0] - p[0]).powi(2) + (d[1] - p[1]).powi(2)).sqrt();
                    if len > gap {
                        pen.line(
                            vec![
                                [p[0] + e[0] * gap, p[1] + e[1] * gap],
                                [d[0] + e[0] * over, d[1] + e[1] * over],
                            ],
                            false,
                            wd,
                            DIMS,
                        );
                    }
                }
                self.tick(pen, da, u, n);
                self.tick(pen, db, [-u[0], -u[1]], n);
                let mid = [(da[0] + db[0]) / 2.0, (da[1] + db[1]) / 2.0];
                Some(along(
                    format_length(l.value(), self.units),
                    mid,
                    u,
                    n,
                    th,
                    tg,
                ))
            }
            DimGeom::Arc {
                center,
                radius,
                a0,
                a1,
                value,
            } => {
                let c = tf(*center);
                let r0 = paper_mm(*radius, self.scale);
                let rp = r0 + extra;
                pen.line(arc_pts(c, rp, *a0, *a1), false, wd, DIMS);
                let dir = |a: f64| {
                    let t = a.to_radians();
                    [t.cos(), t.sin()]
                };
                for (a, s) in [(*a0, 1.0), (*a1, -1.0)] {
                    let e = dir(a);
                    if extra > gap {
                        pen.line(
                            vec![
                                [c[0] + e[0] * (r0 + gap), c[1] + e[1] * (r0 + gap)],
                                [c[0] + e[0] * (rp + over), c[1] + e[1] * (rp + over)],
                            ],
                            false,
                            wd,
                            DIMS,
                        );
                    }
                    let tan = [-e[1] * s, e[0] * s];
                    self.tick(pen, [c[0] + e[0] * rp, c[1] + e[1] * rp], tan, e);
                }
                let sweep = (a1 - a0).rem_euclid(360.0);
                let e = dir(a0 + sweep / 2.0);
                let mid = [c[0] + e[0] * rp, c[1] + e[1] * rp];
                Some(along(
                    format_length(*value, self.units),
                    mid,
                    [-e[1], e[0]],
                    e,
                    th,
                    tg,
                ))
            }
            DimGeom::Radial {
                center,
                radius,
                angle,
            } => {
                let c = tf(*center);
                let rp = paper_mm(*radius, self.scale);
                let t = angle.to_radians();
                let e = [t.cos(), t.sin()];
                let tip = [c[0] + e[0] * rp, c[1] + e[1] * rp];
                pen.line(vec![c, tip], false, wd, DIMS);
                self.tick(pen, tip, [-e[0], -e[1]], [-e[1], e[0]]);
                let mid = [c[0] + e[0] * rp * 0.5, c[1] + e[1] * rp * 0.5];
                let s = format!("R{}", format_length(*radius, self.units));
                Some(along(s, mid, e, [-e[1], e[0]], th, tg))
            }
            DimGeom::Angular {
                center,
                radius,
                a0,
                a1,
                value,
            } => {
                let c = tf(*center);
                let rp = paper_mm(*radius, self.scale).max(num(self.dim, "first")) + extra;
                pen.line(arc_pts(c, rp, *a0, *a1), false, wd, DIMS);
                let dir = |a: f64| {
                    let t = a.to_radians();
                    [t.cos(), t.sin()]
                };
                for (a, s) in [(*a0, 1.0), (*a1, -1.0)] {
                    let e = dir(a);
                    self.tick(
                        pen,
                        [c[0] + e[0] * rp, c[1] + e[1] * rp],
                        [-e[1] * s, e[0] * s],
                        e,
                    );
                }
                let sweep = (a1 - a0).rem_euclid(360.0);
                let e = dir(a0 + sweep / 2.0);
                let mid = [c[0] + e[0] * rp, c[1] + e[1] * rp];
                Some(along(
                    format!("{}°", fmt_num(*value, 2, self.sep)),
                    mid,
                    [-e[1], e[0]],
                    e,
                    th,
                    tg,
                ))
            }
            DimGeom::Spot { at, value } => {
                let p = tf(*at);
                let k = self.wt.tag * 0.8;
                pen.line(
                    vec![p, [p[0] - k, p[1] + k], [p[0] + k, p[1] + k]],
                    true,
                    self.wt.thin,
                    DIMS,
                );
                pen.line(
                    vec![[p[0] - k, p[1] + k], [p[0] + k + 2.0 * k, p[1] + k]],
                    false,
                    self.wt.thin,
                    DIMS,
                );
                let s = elev_text(*value, self.sep);
                Some(along(
                    s,
                    [
                        p[0] + k + text_width(&elev_text(*value, self.sep), th) / 2.0,
                        p[1] + k,
                    ],
                    [1.0, 0.0],
                    [0.0, 1.0],
                    th,
                    tg,
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_widths_exact() {
        // embedded font advances, 1/1000 em: digits 572, 'R' 622, '.' 268, '±' 572, '°' 428
        let (upem, _) = metrics();
        assert_eq!(*upem, 1000.0);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert!(near(text_width("6000", 2.5), 5.72));
        assert!(near(text_width("R5000", 2.5), 7.275));
        assert!(near(text_width("±0.000", 1.0), 3.128));
        assert!(near(text_width("53.13°", 1.0), 2.984));
        // CJK without a glyph in the primary font: 1 em
        assert!(near(text_width("\u{4E2D}", 2.0), 2.0));
        assert_eq!(text_width("", 3.0), 0.0);
    }

    #[test]
    fn readable_rotation() {
        assert_eq!(readable([1.0, 0.0]), 0.0);
        assert_eq!(readable([-1.0, 0.0]), 0.0);
        assert_eq!(readable([0.0, -1.0]), 90.0);
        assert_eq!(readable([0.0, 1.0]), 90.0);
        assert_eq!(readable([-0.6, -0.8]), 53.130102354);
    }
}
