// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing to detail (`drawing.to_detail`): the picked drawing primitives, or every primitive on a
//! shown layer of the drawing, become 2D detail items on the current plan view in one undo step.
//! Lines and polylines become detail lines (arcs flattened within [`ARC_SAG`]), texts become text
//! notes with their height, rotation and alignment, hatches and solid fills become regions with
//! their holes. Line styles are matched by the pen's weight and dash pattern (created when no
//! style has them), text styles by height. The drawing itself is only read.

use std::f64::consts::{FRAC_PI_2, TAU};
use std::sync::Arc;

use ddc_io::drawing_model::{Pick, UFill, UText, Underlay};
use ddc_model::{
    AnnoBatchItem, AnnoShape, AnnoView, AnnoXf, LineStyle, RegionFill, TextAlign, TextNote,
    TextStyle, ViewKey,
};

use super::{Drawing, Drawings};
use crate::editor::{Editor, OpFn};
use crate::shell::EditorApp;

type P = (f64, f64);

/// Largest distance between an arc and the chords replacing it, m.
pub const ARC_SAG: f64 = 0.0002;
/// Most chords for one full circle.
const ARC_MAX_SEGS: usize = 1024;
/// Line ends closer than this (m) continue one polyline.
const JOIN_EPS: f64 = 1e-9;
/// A dot of a dash pattern gets this length on the sheet (mm) so it stays visible.
const DOT_MM: f64 = 0.1;
/// Dash used for a dashed line whose pen has no pattern, mm on the sheet.
const DASH_MM: [f64; 2] = [2.0, 1.0];
/// Line pitch of a text note in text heights: the model's note layout uses the same value.
const PITCH: f64 = 1.4;
/// Hatch without pattern lines: angle (deg) and spacing (mm on the sheet).
const HATCH_DEFAULT: (f64, f64) = (45.0, 3.0);
/// Style sizes are rounded to this (mm) so pattern values read from a file match saved styles.
const STYLE_ROUND: f64 = 1e-3;

/// What one conversion makes.
#[derive(Debug, Default)]
pub struct Detail {
    pub items: Vec<AnnoBatchItem>,
    pub lines: usize,
    pub texts: usize,
    pub regions: usize,
}

fn round_mm(v: f64) -> f64 {
    (v / STYLE_ROUND).round() / (1.0 / STYLE_ROUND)
}

fn near(a: P, b: P) -> bool {
    (a.0 - b.0).abs() <= JOIN_EPS && (a.1 - b.1).abs() <= JOIN_EPS
}

/// Dash pattern of a pen (m in the model; + dash, - gap, 0 dot) as sheet mm alternating dash,
/// gap, ... starting with a dash; empty for a solid line.
pub fn sheet_dash(dash: &[f32], scale: f64) -> Vec<f64> {
    let mut runs: Vec<(f64, bool)> = vec![];
    for &v in dash {
        let v = v as f64;
        if !v.is_finite() {
            continue;
        }
        let on = v >= 0.0;
        let mm = if on && v == 0.0 {
            DOT_MM
        } else {
            v.abs() * 1000.0 / scale
        };
        match runs.last_mut() {
            Some(r) if r.1 == on => r.0 += mm,
            _ => runs.push((mm, on)),
        }
    }
    if runs.len() > 1 && runs[0].1 == runs[runs.len() - 1].1 {
        let last = runs.pop().unwrap_or_default();
        runs[0].0 += last.0;
    }
    if runs.len() < 2 {
        return vec![];
    }
    if !runs[0].1 {
        runs.rotate_left(1);
    }
    runs.iter().map(|r| round_mm(r.0)).collect()
}

/// Line style for a pen: `None` (the view default) when the pen carries no weight and no dash.
pub fn line_style(u: &Underlay, pen: u32, dashed: bool, scale: f64) -> Option<LineStyle> {
    let p = u.pen(pen);
    let mut dash = sheet_dash(&p.dash, scale);
    if dash.is_empty() && dashed {
        dash = DASH_MM.to_vec();
    }
    let w = p.weight_mm as f64;
    let has_w = w.is_finite() && w > 0.0;
    if !has_w && dash.is_empty() {
        return None;
    }
    let weight_mm = if has_w {
        round_mm(w).max(STYLE_ROUND)
    } else {
        LineStyle::default().weight_mm
    };
    let name = if dash.is_empty() {
        format!("drawing {weight_mm:.2}")
    } else {
        format!("drawing {weight_mm:.2} dashed")
    };
    Some(LineStyle {
        id: 0,
        name,
        weight_mm,
        dash,
    })
}

/// Arc points from `a0` to `a1` (rad, counter-clockwise) with chords within [`ARC_SAG`]; a full
/// circle comes back closed without the repeated first point.
pub fn arc_points(c: P, r: f64, a0: f64, a1: f64) -> Option<(Vec<P>, bool)> {
    let sweep = a1 - a0;
    if !(r.is_finite() && r > 0.0 && sweep.is_finite() && sweep > 0.0) {
        return None;
    }
    let full = sweep >= TAU - 1e-9;
    let step = if r <= ARC_SAG {
        FRAC_PI_2
    } else {
        2.0 * (1.0 - ARC_SAG / r).acos()
    };
    let min = if full { 8 } else { 2 };
    let max = ((ARC_MAX_SEGS as f64 * sweep / TAU).ceil() as usize).max(min);
    let n = ((sweep / step).ceil() as usize).clamp(min, max);
    let mut pts: Vec<P> = (0..=n)
        .map(|k| {
            let t = if k == n {
                a1
            } else {
                a0 + sweep * k as f64 / n as f64
            };
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect();
    if full {
        pts.pop();
    }
    Some((pts, full))
}

/// Anchor of the text position in its block, 1-9 (rows top to bottom, columns left to right):
/// the attachment of a multi-line text, the justification of a single line (baseline is 7).
fn anchor(t: &UText) -> u8 {
    let f = &t.fmt;
    if f.mtext {
        return if (1..=9).contains(&f.attach) { f.attach } else { 1 };
    }
    let col = match f.halign {
        1 | 4 => 1,
        2 => 2,
        _ => 0,
    };
    let row = match f.valign {
        3 => 0,
        2 => 1,
        _ => 2,
    };
    (row * 3 + col + 1) as u8
}

/// Text note whose first line sits where the drawing's text does. Returns the note and the text
/// height (m), or `None` for an empty or degenerate text.
pub fn text_note(t: &UText) -> Option<(TextNote, f64)> {
    let h = t.height;
    if t.text.trim().is_empty() || !(h.is_finite() && h > 0.0) || !t.rotation.is_finite() {
        return None;
    }
    let a = anchor(t);
    let (row, col) = ((a - 1) / 3, (a - 1) % 3);
    let n = t.text.split('\n').count().max(1);
    let total = h * (1.0 + (n - 1) as f64 * PITCH);
    // the note is anchored at the top of its lines
    let up = match row {
        0 => 0.0,
        1 => total / 2.0,
        _ => total,
    };
    let (s, c) = t.rotation.sin_cos();
    let mut note = TextNote::new((t.pos.0 - s * up, t.pos.1 + c * up), &t.text);
    note.rot_deg = t.rotation.to_degrees();
    note.align = match col {
        0 => TextAlign::Left,
        1 => TextAlign::Center,
        _ => TextAlign::Right,
    };
    Some((note, h))
}

fn clean_ring(r: &[P]) -> Option<Vec<P>> {
    let mut out: Vec<P> = Vec::with_capacity(r.len());
    for &p in r {
        if !(p.0.is_finite() && p.1.is_finite()) {
            return None;
        }
        if out.last().is_none_or(|q| !near(*q, p)) {
            out.push(p);
        }
    }
    while out.len() > 1 && near(out[0], out[out.len() - 1]) {
        out.pop();
    }
    (out.len() >= 3).then_some(out)
}

fn gray(c: Option<[u8; 3]>) -> u8 {
    c.map_or(0, |[r, g, b]| {
        (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64)
            .round()
            .clamp(0.0, 255.0) as u8
    })
}

/// Hatch angle, spacing and cross from the pattern lines of a fill.
pub fn hatch_fill(segs: &[(P, P)], scale: f64) -> RegionFill {
    let deg_of = |a: P, b: P| (b.1 - a.1).atan2(b.0 - a.0).to_degrees().rem_euclid(180.0);
    let bin_of = |a: P, b: P| ((deg_of(a, b) * 10.0).round() as i64).rem_euclid(1800);
    // length per 0.1 degree bin
    let mut bins: std::collections::BTreeMap<i64, f64> = Default::default();
    for &(a, b) in segs {
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        if l.is_finite() && l > JOIN_EPS {
            *bins.entry(bin_of(a, b)).or_default() += l;
        }
    }
    let Some((&main, &w1)) = bins
        .iter()
        .max_by(|x, y| x.1.total_cmp(y.1).then(y.0.cmp(x.0)))
    else {
        let (deg, step_mm) = HATCH_DEFAULT;
        return RegionFill::Hatch {
            deg,
            step_mm,
            cross: false,
        };
    };
    let bin_gap = |b: i64, to: i64| {
        let d = (b - to).rem_euclid(1800);
        d.min(1800 - d)
    };
    let near_main = |a: P, b: P| {
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        l.is_finite() && l > JOIN_EPS && bin_gap(bin_of(a, b), main) <= 2
    };
    // exact angle: length-weighted mean of the main direction's lines around its bin
    let base = main as f64 / 10.0;
    let (mut sum, mut wsum) = (0.0, 0.0);
    for &(a, b) in segs.iter().filter(|(a, b)| near_main(*a, *b)) {
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        sum += l * ((deg_of(a, b) - base + 90.0).rem_euclid(180.0) - 90.0);
        wsum += l;
    }
    let mut deg = (base + if wsum > 0.0 { sum / wsum } else { 0.0 }).rem_euclid(180.0);
    if deg > 180.0 - 1e-9 {
        deg = 0.0;
    }
    let cross = bins
        .iter()
        .any(|(&b, &w)| bin_gap(b, (main + 900).rem_euclid(1800)) <= 5 && w >= 0.25 * w1);
    let (s, c) = deg.to_radians().sin_cos();
    let mut offs: Vec<f64> = segs
        .iter()
        .filter(|(a, b)| near_main(*a, *b))
        .map(|(a, b)| -s * (a.0 + b.0) / 2.0 + c * (a.1 + b.1) / 2.0)
        .collect();
    offs.sort_by(f64::total_cmp);
    offs.dedup_by(|x, y| (*x - *y).abs() <= 1e-7);
    let mut gaps: Vec<f64> = offs.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(f64::total_cmp);
    let step_mm = gaps
        .get(gaps.len() / 2)
        .map(|g| g * 1000.0 / scale)
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(HATCH_DEFAULT.1);
    RegionFill::Hatch {
        deg,
        step_mm,
        cross,
    }
}

/// Region for a fill: outer contour first, holes after; `None` when the outer contour is bad.
pub fn region(u: &Underlay, f: &UFill, scale: f64) -> Option<AnnoShape> {
    let mut loops = f.loops.iter();
    let outer = clean_ring(loops.next()?)?;
    let mut rings = vec![outer];
    rings.extend(loops.filter_map(|r| clean_ring(r)));
    let fill = if f.solid {
        RegionFill::Solid {
            gray: gray(u.pen(f.pen).color),
        }
    } else {
        hatch_fill(&f.segs, scale)
    };
    Some(AnnoShape::Region {
        rings,
        fill,
        // a fill's boundary is not drawn in the source: its lines, if any, come separately
        boundary: false,
    })
}

fn sorted(v: &[usize], len: usize) -> Vec<usize> {
    let mut v: Vec<usize> = v.iter().copied().filter(|&i| i < len).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn push_line(out: &mut Detail, pts: Vec<P>, closed: bool, line: Option<LineStyle>) {
    out.items.push(AnnoBatchItem {
        shape: AnnoShape::Line { pts, closed },
        text: None,
        line,
    });
    out.lines += 1;
}

/// Polyline started by line `first` of `u`; closed when it ends where it starts.
fn flush(out: &mut Detail, u: &Underlay, scale: f64, cur: Option<(usize, Vec<P>)>) {
    let Some((first, mut pts)) = cur else {
        return;
    };
    let l = &u.lines[first];
    let closed = pts.len() >= 4 && near(pts[0], pts[pts.len() - 1]);
    if closed {
        pts.pop();
    }
    push_line(out, pts, closed, line_style(u, l.pen, l.dashed, scale));
}

fn shown_idx<'a>(d: &Drawing, n: usize, layer: impl Fn(usize) -> &'a str) -> Vec<usize> {
    (0..n).filter(|&i| d.shown(layer(i))).collect()
}

/// Detail items for `pick` of `d`, or for every primitive on a shown layer when `pick` is `None`.
/// `scale` is the view's annotation scale (100 for 1:100).
pub fn convert(d: &Drawing, pick: Option<&Pick>, scale: f64) -> Detail {
    let u = &d.u;
    let (lines, arcs, texts, fills) = match pick {
        Some(pk) => (
            sorted(&pk.lines, u.lines.len()),
            sorted(&pk.arcs, u.arcs.len()),
            sorted(&pk.texts, u.texts.len()),
            sorted(&pk.fills, u.fills.len()),
        ),
        None => (
            shown_idx(d, u.lines.len(), |i| u.lines[i].layer.as_str()),
            shown_idx(d, u.arcs.len(), |i| u.arcs[i].layer.as_str()),
            shown_idx(d, u.texts.len(), |i| u.texts[i].layer.as_str()),
            shown_idx(d, u.fills.len(), |i| u.fills[i].layer.as_str()),
        ),
    };
    let mut out = Detail::default();
    // consecutive segments of one layer and pen meeting end to start were one polyline
    let mut cur: Option<(usize, Vec<P>)> = None;
    let mut prev: Option<usize> = None;
    for i in lines {
        let l = &u.lines[i];
        if near(l.a, l.b) {
            continue;
        }
        let joins = match (&cur, prev) {
            (Some((first, pts)), Some(p)) => {
                let f = &u.lines[*first];
                p + 1 == i
                    && f.layer == l.layer
                    && f.pen == l.pen
                    && f.dashed == l.dashed
                    && pts.last().is_some_and(|q| near(*q, l.a))
            }
            _ => false,
        };
        if joins {
            if let Some((_, pts)) = cur.as_mut() {
                pts.push(l.b);
            }
        } else {
            flush(&mut out, u, scale, cur.take());
            cur = Some((i, vec![l.a, l.b]));
        }
        prev = Some(i);
    }
    flush(&mut out, u, scale, cur.take());
    for i in arcs {
        let a = &u.arcs[i];
        if let Some((pts, closed)) = arc_points(a.c, a.r, a.a0, a.a1) {
            push_line(&mut out, pts, closed, line_style(u, a.pen, false, scale));
        }
    }
    for i in fills {
        if let Some(shape) = region(u, &u.fills[i], scale) {
            out.items.push(AnnoBatchItem {
                shape,
                text: None,
                line: None,
            });
            out.regions += 1;
        }
    }
    for i in texts {
        if let Some((note, h)) = text_note(&u.texts[i]) {
            let height_mm = h * 1000.0 / scale;
            out.items.push(AnnoBatchItem {
                shape: AnnoShape::Text(note),
                text: Some(TextStyle {
                    name: format!("drawing {:.2}", round_mm(height_mm)),
                    height_mm,
                    ..TextStyle::default()
                }),
                line: None,
            });
            out.texts += 1;
        }
    }
    out
}

/// Plan point -> 2D system of a section or elevation, as steps of [`AnnoXf`]: the view runs
/// along the view line (x = distance from its start) with elevations up (y). A plan drawing has
/// no height of its own, so it is folded into the view plane about the view line at the
/// elevation of its level: the offset across the line becomes the height above that level.
/// `None` when the view is gone or its line or the level has no usable value.
pub fn section_fold(ed: &Editor, view: i64, level: i64) -> Option<[AnnoXf; 3]> {
    let spec = ed.model.view_def(view).ok()?;
    let (dx, dy) = (spec.b.0 - spec.a.0, spec.b.1 - spec.a.1);
    let len = dx.hypot(dy);
    let z0 = ed.levels().into_iter().find(|l| l.0 == level)?.2;
    let ok = [spec.a.0, spec.a.1, len, z0].iter().all(|v| v.is_finite()) && len > 1e-9;
    ok.then(|| {
        [
            AnnoXf::Move((-spec.a.0, -spec.a.1)),
            AnnoXf::Rotate {
                o: (0.0, 0.0),
                a: -dy.atan2(dx),
            },
            AnnoXf::Move((0.0, z0)),
        ]
    })
}

impl Drawings {
    /// The drawing the command works on: the picked one, else the chosen or first drawing on
    /// `level`; with its pick (`None` for the whole drawing).
    fn detail_source(&self, level: i64) -> Option<(usize, Option<&Pick>)> {
        if let Some((k, pk)) = &self.pick {
            if self.list.get(*k).is_some_and(|d| d.level == level) {
                return Some((*k, Some(pk)));
            }
        }
        if self.list.get(self.sel).is_some_and(|d| d.level == level) {
            return Some((self.sel, None));
        }
        self.on_level(level).next().map(|(k, _)| (k, None))
    }

    /// `drawing.to_detail` on the current plan view.
    pub fn to_detail(&self, ed: &mut Editor) -> String {
        let v = ViewKey::Plan(ed.level);
        self.to_detail_in(ed, v)
    }

    /// Detail items from the picked primitives (or the whole drawing) in `key`'s view, one undo
    /// step; the drawing is not changed. 3D views are refused. Returns the status line.
    pub fn to_detail_in(&self, ed: &mut Editor, key: ViewKey) -> String {
        let view = match key {
            ViewKey::Plan(l) => AnnoView::Plan(l),
            ViewKey::Section(id) => AnnoView::Section(id),
            ViewKey::ThreeD => return crate::tr("detail_items.st.bad_view", &[]),
        };
        let Some((k, pick)) = self.detail_source(ed.level) else {
            return crate::tr("detail_items.st.no_drawing", &[]);
        };
        let scale = ed
            .model
            .anno_scale(view)
            .ok()
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(100.0);
        // a section or elevation has its own 2D system: refuse when it cannot be taken
        let fold = match key {
            ViewKey::Section(id) => {
                match section_fold(ed, id, self.list[k].level) {
                    Some(f) => Some(f),
                    None => return crate::tr("detail_items.st.no_transform", &[]),
                }
            }
            _ => None,
        };
        let mut out = convert(&self.list[k], pick, scale);
        if out.items.is_empty() {
            return crate::tr("detail_items.st.nothing", &[]);
        }
        if let Some(f) = fold {
            for it in &mut out.items {
                it.shape = f.iter().fold(it.shape.clone(), |s, x| s.transformed(*x));
            }
        }
        let items = Arc::new(out.items);
        let top = ed.model.op_top().ok();
        let f = move |m: &mut ddc_model::Model| m.add_annos_batch(view, &items).map(|_| ());
        let counts = [
            ("lines", out.lines.to_string()),
            ("texts", out.texts.to_string()),
            ("regions", out.regions.to_string()),
        ];
        let counts = [
            (counts[0].0, counts[0].1.as_str()),
            (counts[1].0, counts[1].1.as_str()),
            (counts[2].0, counts[2].1.as_str()),
        ];
        match ed.run_op(OpFn(Arc::new(f))) {
            Ok(()) => crate::tr("detail_items.st.done", &counts),
            // the step may have committed and only the reload failed: that is a warning
            Err(e) if top.is_some() && ed.model.op_top().ok() != top => crate::tr(
                "detail_items.st.warn",
                &[
                    ("lines", counts[0].1),
                    ("texts", counts[1].1),
                    ("regions", counts[2].1),
                    ("e", &e.to_string()),
                ],
            ),
            Err(e) => crate::tr("detail_items.st.failed", &[("e", &e.to_string())]),
        }
    }
}

fn h_to_detail(app: &mut EditorApp) {
    // a sheet hides the plan or section behind it: refused like 3D, nothing lands unseen
    // a drafting view tab hides it too
    let msg = if crate::sheet_view::active().is_some()
        || crate::drafting_view::current(&app.ed).is_some()
    {
        crate::tr("detail_items.st.bad_view", &[])
    } else {
        let key = crate::view_gfx_ui::active_view(app);
        app.plan.drawings.to_detail_in(&mut app.ed, key)
    };
    app.ed.status = Some(msg);
}

pub fn register(app: &mut EditorApp) {
    app.register_command("drawing.to_detail", h_to_detail);
}

#[cfg(test)]
mod tests;
