// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing set planner: from the model to a `DsetPlan` of floor plans per level, two sections
//! (through the stair and across the main span), four elevations, room, door and window
//! schedules split across sheets, and the sheet list on the first sheet. Views and schedule
//! parts are packed on sheets without overlaps and clear of the title block. Same model and
//! options, same plan; the stable sheet keys let a second run update the same sheets.

use std::collections::BTreeMap;
use std::ops::Range;

use ddc_model::{
    DsetPlan, DsetRef, DsetSchedule, DsetSheet, DsetViewport, GridAxis, Model, PlanView,
    SectionSpec, SheetDef, ViewKind, ViewPrim, ViewRef, ViewportDef, COMPASS, DEFAULT_CUT,
    DSET_LEVELS, DSET_SET, MarkPrefix,
};

use super::output::stamp_from_project;
use super::dset_rooms::{room_note, room_numbers, show_rooms, typical_rooms, Numbers};
use super::dset_tags::settle_tags;
use super::set::{
    area_unit, extent, plan_annotations, title_rect, view_drawing, view_title, Ctx, PlanMarks,
    PAD,
};
use super::table::{continued_title, Layout, Table};
use super::{
    compose_styled, data, num, paper_size, Mark, Pen, SheetDrawing, SheetError, SheetSpec,
    Viewport,
};

/// Model schedules a set carries, in sheet order.
pub const SCHEDULES: [&str; 5] = ["rooms", "doors", "windows", "areas", "finishes"];
/// Paper sizes tried when none is given, smallest first (all landscape).
const PAPERS: [&str; 4] = ["A3", "A2", "A1", "A0"];
/// Scales tried when none is given, finest first.
const SCALES: [f64; 4] = [100.0, 200.0, 400.0, 500.0];
/// Space kept inside the frame, between views, and between schedule parts, mm.
const MARGIN: f64 = 5.0;
const GAP: f64 = 12.0;
const TABLE_GAP: f64 = 8.0;
/// Schedule text height on a sheet, mm (as the sheet draws it).
const TEXT_H: f64 = 2.5;
/// Section lines run this far past the building, m.
const SECTION_OVER: f64 = 1.5;
/// Elevation lines stand this far off the building, m.
const ELEVATION_OFF: f64 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct DsetOptions {
    pub plans: bool,
    pub sections: bool,
    pub elevations: bool,
    pub schedules: bool,
    /// Scale denominator of the plans, and the finest for sections and elevations; None:
    /// 1:100, coarser only when a view fits no paper.
    pub scale: Option<f64>,
    /// Paper name; None: the smallest of A3..A0 every view fits.
    pub paper: Option<String>,
    /// Title block profile: `gost` (GOST R 21.101 form 3) or `iso`.
    pub standard: String,
}

impl Default for DsetOptions {
    fn default() -> Self {
        Self {
            plans: true,
            sections: true,
            elevations: true,
            schedules: true,
            scale: None,
            paper: None,
            standard: "gost".into(),
        }
    }
}

/// A plan and what the planner chose or noticed.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub plan: DsetPlan,
    /// Scale denominator of the floor plans.
    pub plan_scale: f64,
    /// Scale denominator of sections and elevations: the plans' or coarser, so a tall
    /// building does not push its plans to a coarse scale.
    pub view_scale: f64,
    pub paper: String,
    pub warnings: Vec<String>,
}

/// Paper extent of a view around its viewport centre, mm: left, right, bottom, top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Foot {
    pub l: f64,
    pub r: f64,
    pub b: f64,
    pub t: f64,
}

impl Foot {
    fn w(&self) -> f64 {
        self.l + self.r
    }
    fn h(&self) -> f64 {
        self.b + self.t
    }
}

/// Drawing room of a sheet: frame inside [x0, y0, x1, y1] and the title block's left and top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub frame: [f64; 4],
    pub stamp: [f64; 2],
}

impl Region {
    pub fn of(standard: &str, paper: &str) -> Result<Region, SheetError> {
        let st = &data()["standards"][standard];
        if st.is_null() {
            return Err(SheetError(format!("unknown standard '{standard}'")));
        }
        let (w, h) = paper_size(paper, true)?;
        let fr = &st["frame"];
        let f = [
            num(fr, "left"),
            num(fr, "bottom"),
            w - num(fr, "right"),
            h - num(fr, "top"),
        ];
        let sp = &st["stamp"];
        Ok(Region {
            frame: f,
            stamp: [f[2] - num(sp, "w"), f[1] + num(sp, "h")],
        })
    }
    fn left(&self) -> f64 {
        self.frame[0] + MARGIN
    }
    fn right(&self) -> f64 {
        self.frame[2] - MARGIN
    }
    fn bottom(&self) -> f64 {
        self.frame[1] + MARGIN
    }
    fn top(&self) -> f64 {
        self.frame[3] - MARGIN
    }
    /// A box [x0, y0, x1, y1] lies in the frame and clear of the title block (by `gap`).
    fn holds(&self, b: [f64; 4], gap: f64) -> bool {
        let e = 1e-6;
        b[0] >= self.left() - e
            && b[2] <= self.right() + e
            && b[1] >= self.bottom() - e
            && b[3] <= self.top() + e
            && !(b[2] > self.stamp[0] - gap + e && b[1] < self.stamp[1] + gap - e)
    }
    /// A single view of this size can stand on the sheet somewhere.
    fn fits(&self, f: &Foot) -> bool {
        let (w, h) = (self.right() - self.left(), self.top() - self.bottom());
        let beside = self.stamp[0] - GAP - self.left();
        let above = self.top() - self.stamp[1] - GAP;
        f.w() <= w && f.h() <= h && (f.w() <= beside || f.h() <= above)
    }
}

/// «doors@0-24» → ("doors", Some(0..24)); any other key is whole.
pub fn page_key(key: &str) -> (&str, Option<Range<usize>>) {
    let Some((base, r)) = key.rsplit_once('@') else {
        return (key, None);
    };
    let Some((a, b)) = r.split_once('-') else {
        return (key, None);
    };
    match (a.parse::<usize>(), b.parse::<usize>()) {
        (Ok(a), Ok(b)) if a <= b => (base, Some(a..b)),
        _ => (key, None),
    }
}

/// The rows of one part; a part after the first gets the «continued» title.
pub fn page_table(t: Table, rows: Option<Range<usize>>, text: &dyn Fn(&str) -> String) -> Table {
    let Some(r) = rows else {
        return t;
    };
    let n = t.rows.len();
    let r = r.start.min(n)..r.end.min(n);
    let title = if r.start > 0 {
        continued_title(&t.title, &text("sheets.paper.continued"))
    } else {
        t.title.clone()
    };
    let swatches = if t.swatches.len() == n {
        t.swatches[r.clone()].to_vec()
    } else {
        t.swatches
    };
    Table {
        title,
        header: t.header,
        rows: t.rows[r].to_vec(),
        swatches,
    }
}

fn me(e: ddc_model::Error) -> SheetError {
    SheetError(e.to_string())
}

fn grow(bb: &mut [f64; 4], p: [f64; 2]) {
    if p[0].is_finite() && p[1].is_finite() {
        *bb = [
            bb[0].min(p[0]),
            bb[1].min(p[1]),
            bb[2].max(p[0]),
            bb[3].max(p[1]),
        ];
    }
}

/// Paper box of a mark, mm: lines and fills by their points, text by its rotated box.
pub fn mark_box(m: &Mark) -> [f64; 4] {
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    match m {
        Mark::Line { pts, .. } => pts.iter().for_each(|p| grow(&mut bb, *p)),
        Mark::Fill { rings, .. } => rings.iter().flatten().for_each(|p| grow(&mut bb, *p)),
        Mark::Text { pos, h, rot, s, .. } => {
            for q in text_corners(*pos, *h, *rot, s) {
                grow(&mut bb, q);
            }
        }
    }
    bb
}

/// Corners of a text box: baseline start `pos`, height `h`, turned `rot` degrees.
pub fn text_corners(pos: [f64; 2], h: f64, rot: f64, s: &str) -> [[f64; 2]; 4] {
    let w = super::text_w(s, h);
    let (sn, cs) = rot.to_radians().sin_cos();
    let at = |x: f64, y: f64| [pos[0] + x * cs - y * sn, pos[1] + x * sn + y * cs];
    [at(0.0, -0.2 * h), at(w, -0.2 * h), at(w, h), at(0.0, h)]
}

/// View `d` alone at 1:`scale` in the middle of an A0 sheet as viewport 0, with its title,
/// the marks of `views` and the plan `marks`; also the viewport rectangle.
pub(super) fn lone_view(
    d: &PlanView,
    scale: f64,
    standard: &str,
    ctx: &Ctx,
    title: &str,
    views: &[SectionSpec],
    marks: &PlanMarks,
) -> Result<(SheetDrawing, [f64; 4]), SheetError> {
    let mut shown = d.clone();
    super::dset_area::localize_finish(&mut shown, ctx.text);
    let d = &shown;
    let bx = d.bbox;
    let w = (bx[2] - bx[0]) * 1000.0 / scale + 2.0 * PAD;
    let h = (bx[3] - bx[1]) * 1000.0 / scale + 2.0 * PAD;
    let (pw, ph) = paper_size("A0", true)?;
    let c = [pw / 2.0, ph / 2.0];
    let rect = [c[0] - w / 2.0, c[1] - h / 2.0, w, h];
    let spec = SheetSpec {
        paper: "A0".into(),
        landscape: true,
        standard: standard.into(),
        fields: BTreeMap::new(),
        viewports: vec![Viewport {
            view: d,
            scale,
            rect: Some(rect),
            title: String::new(),
        }],
        units: ctx.units,
        area_unit: Some(area_unit(ctx)),
        logo: None,
    };
    let mut dr = compose_styled(&spec, &[])?;
    super::output::plan_view_marks(&mut dr, 0, standard, scale, views)?;
    let below = title_rect(&dr.marks, 0, rect);
    let mut pen = Pen {
        out: &mut dr.marks,
        vp: Some(0),
    };
    view_title(&mut pen, below, "00", title, scale);
    plan_annotations(&mut pen, below, standard, title, marks);
    Ok((dr, rect))
}

/// Paper extent of view `d` at 1:`scale` with its title, measured by drawing it once; `views`
/// are the sections and elevations a plan marks, `marks` the floor levels and the room
/// number note a plan shows beside its title.
pub fn footprint(
    d: &PlanView,
    scale: f64,
    standard: &str,
    ctx: &Ctx,
    title: &str,
    views: &[SectionSpec],
    marks: &PlanMarks,
) -> Result<Foot, SheetError> {
    let mut d = d.clone();
    d.bbox = extent(&d);
    let (dr, rect) = lone_view(&d, scale, standard, ctx, title, views, marks)?;
    let c = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
    let mut bb = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];
    for (vp, m) in &dr.marks {
        if *vp == Some(0) {
            let b = mark_box(m);
            grow(&mut bb, [b[0], b[1]]);
            grow(&mut bb, [b[2], b[3]]);
        }
    }
    Ok(Foot {
        l: c[0] - bb[0],
        r: bb[2] - c[0],
        b: c[1] - bb[1],
        t: bb[3] - c[1],
    })
}

/// Shelf packing of views on sheets: rows from the top left, a new row when the next view
/// does not fit beside, a new sheet when no row fits; a view never reaches the title block.
/// Each sheet's group is then centred where it can be. Returns per sheet (item, centre).
pub fn pack_views(feet: &[Foot], reg: &Region) -> Vec<Vec<(usize, (f64, f64))>> {
    let mut pages: Vec<Vec<(usize, [f64; 4])>> = vec![];
    let mut cur: Vec<(usize, [f64; 4])> = vec![];
    let (mut x, mut row_top, mut row_h) = (reg.left(), reg.top(), 0.0f64);
    for (i, f) in feet.iter().enumerate() {
        loop {
            let b = [x, row_top - f.h(), x + f.w(), row_top];
            if reg.holds(b, GAP) {
                cur.push((i, b));
                x += f.w() + GAP;
                row_h = row_h.max(f.h());
                break;
            }
            if x > reg.left() + 1e-9 {
                row_top -= row_h + GAP;
                x = reg.left();
                row_h = 0.0;
                if row_top - f.h() >= reg.bottom() - 1e-9 {
                    continue;
                }
            }
            if !cur.is_empty() {
                pages.push(std::mem::take(&mut cur));
                (x, row_top, row_h) = (reg.left(), reg.top(), 0.0);
                continue;
            }
            // larger than any free place: top left, the caller has warned
            cur.push((i, [x, row_top - f.h(), x + f.w(), row_top]));
            x += f.w() + GAP;
            row_h = row_h.max(f.h());
            break;
        }
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    pages
        .into_iter()
        .map(|p| {
            let (dx, dy) = centre(&p, reg);
            p.into_iter()
                .map(|(i, b)| {
                    let f = &feet[i];
                    (i, (b[0] + dx + f.l, b[3] + dy - f.t))
                })
                .collect()
        })
        .collect()
}

/// Shift that centres a sheet's group: on the whole sheet, above the title block, or beside it,
/// whichever first keeps every box clear; none if none does.
fn centre(p: &[(usize, [f64; 4])], reg: &Region) -> (f64, f64) {
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (_, b) in p {
        grow(&mut bb, [b[0], b[1]]);
        grow(&mut bb, [b[2], b[3]]);
    }
    let mid = |lo: f64, hi: f64, a: f64, b: f64| (lo + hi) / 2.0 - (a + b) / 2.0;
    let full_x = mid(reg.left(), reg.right(), bb[0], bb[2]);
    let full_y = mid(reg.bottom(), reg.top(), bb[1], bb[3]);
    let above_y = mid(reg.stamp[1] + GAP, reg.top(), bb[1], bb[3]);
    let beside_x = mid(reg.left(), reg.stamp[0] - GAP, bb[0], bb[2]);
    let ok = |dx: f64, dy: f64| {
        p.iter()
            .all(|(_, b)| reg.holds([b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy], GAP))
    };
    let tries = [
        (full_x, full_y),
        (full_x, above_y),
        (beside_x, full_y),
        (beside_x, 0.0),
        (full_x, 0.0),
        (0.0, full_y),
    ];
    tries
        .into_iter()
        .find(|&(dx, dy)| ok(dx, dy))
        .unwrap_or((0.0, 0.0))
}

/// One schedule part on a sheet: sheet index in the flow, key, top left, height.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub page: usize,
    pub key: String,
    pub pos: (f64, f64),
    pub h: f64,
}

fn part_h(t: &Table, rows: Range<usize>, max_w: f64, text: &dyn Fn(&str) -> String) -> (f64, f64) {
    let p = page_table(t.clone(), Some(rows), text);
    let l = Layout::new(&p, &p.title, TEXT_H, max_w, 1e6);
    (l.head_h() + l.heights.iter().sum::<f64>(), l.width())
}

/// Schedules flowed down columns, then across, then onto further sheets; a schedule taller
/// than the room left is cut between rows and goes on with the «continued» title.
pub fn flow_tables(
    tables: &[(String, Table)],
    reg: &Region,
    text: &dyn Fn(&str) -> String,
) -> Vec<Part> {
    let max_w = reg.right() - reg.left();
    let bottom_at = |x1: f64| {
        if x1 > reg.stamp[0] - TABLE_GAP {
            reg.stamp[1] + TABLE_GAP
        } else {
            reg.bottom()
        }
    };
    let mut out = vec![];
    let (mut page, mut x, mut y, mut col_w) = (0usize, reg.left(), reg.top(), 0.0f64);
    for (key, t) in tables {
        let full = Layout::new(t, &t.title, TEXT_H, max_w, 1e6);
        let n = t.rows.len();
        let mut start = 0;
        let mut guard = 0;
        loop {
            guard += 1;
            let width = full.width();
            let fresh_col = y >= reg.top() - 1e-9;
            if x + width > reg.right() + 1e-9 && !(fresh_col && x <= reg.left() + 1e-9) {
                (page, x, y, col_w) = (page + 1, reg.left(), reg.top(), 0.0);
                continue;
            }
            let avail = y - bottom_at(x + width);
            let mut end = start;
            let mut used = full.head_h();
            while end < n && used + full.heights[end] <= avail + 1e-9 {
                used += full.heights[end];
                end += 1;
            }
            // the exact part (its own widths, a continued title) may be taller: back off
            let mut fit = part_h(t, start..end, max_w, text);
            while end > start + 1 && fit.0 > avail + 1e-9 {
                end -= 1;
                fit = part_h(t, start..end, max_w, text);
            }
            let fits = fit.0 <= avail + 1e-9 && (end > start || n == 0);
            if !fits && guard < 4 * (n + 4) {
                if !fresh_col {
                    x += col_w + TABLE_GAP;
                    (y, col_w) = (reg.top(), 0.0);
                } else {
                    (page, x, y, col_w) = (page + 1, reg.left(), reg.top(), 0.0);
                }
                continue;
            }
            if !fits {
                // not even one row fits an empty sheet: place it anyway, cut by the sheet
                end = (start + 1).min(n);
                fit = part_h(t, start..end, max_w, text);
            }
            let k = if start == 0 && end == n {
                key.clone()
            } else {
                format!("{key}@{start}-{end}")
            };
            out.push(Part {
                page,
                key: k,
                pos: (x, y),
                h: fit.0,
            });
            col_w = col_w.max(fit.1);
            y -= fit.0 + TABLE_GAP;
            start = end;
            if start >= n {
                break;
            }
        }
    }
    out
}

/// Sheet list table as the sheet draws it (number, name, revision, date).
fn sheet_list(rows: &[(String, String)], text: &dyn Fn(&str) -> String) -> Table {
    let tr = |k: &str| text(&format!("sheets.paper.{k}"));
    Table {
        title: tr("sheet_list"),
        header: ["col_sheet", "col_name", "col_rev", "col_date"]
            .map(tr)
            .to_vec(),
        rows: rows
            .iter()
            .map(|(n, s)| vec![n.clone(), s.clone(), String::new(), String::new()])
            .collect(),
        swatches: vec![],
    }
}

fn unit(p: (f64, f64)) -> (f64, f64) {
    let l = p.0.hypot(p.1).max(1e-12);
    (p.0 / l, p.1 / l)
}

fn dot(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.0 + a.1 * b.1
}

/// A section line along `dir` through `at`, across the building box `bb` and past it; looks to
/// the side with more building, clipped there.
fn section_through(name: &str, at: (f64, f64), dir: (f64, f64), bb: [f64; 4]) -> SectionSpec {
    let d = unit(dir);
    let n = (-d.1, d.0);
    let m = SECTION_OVER;
    let corners = [
        (bb[0] - m, bb[1] - m),
        (bb[2] + m, bb[1] - m),
        (bb[2] + m, bb[3] + m),
        (bb[0] - m, bb[3] + m),
    ];
    let rel = |c: (f64, f64)| (c.0 - at.0, c.1 - at.1);
    let ts: Vec<f64> = corners.iter().map(|&c| dot(rel(c), d)).collect();
    let ns: Vec<f64> = corners.iter().map(|&c| dot(rel(c), n)).collect();
    let t0 = ts.iter().copied().fold(f64::INFINITY, f64::min);
    let t1 = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let left = ns.iter().copied().fold(0.0, f64::max);
    let right = -ns.iter().copied().fold(0.0, f64::min);
    let pa = (at.0 + d.0 * t0, at.1 + d.1 * t0);
    let pb = (at.0 + d.0 * t1, at.1 + d.1 * t1);
    let (a, b, depth) = if left >= right {
        (pa, pb, left)
    } else {
        (pb, pa, right)
    };
    SectionSpec {
        kind: ViewKind::Section,
        name: name.into(),
        a,
        b,
        depth: depth.max(1.0),
        far_clip: true,
    }
}

/// Middle of the widest bay between axes square to `vertical` inside `[lo, hi]`; the middle
/// of `[lo, hi]` without two such axes.
fn bay_middle(grids: &[GridAxis], vertical: bool, lo: f64, hi: f64) -> f64 {
    let mut v: Vec<f64> = grids
        .iter()
        .filter_map(|g| {
            let d = unit((g.b.0 - g.a.0, g.b.1 - g.a.1));
            if vertical && d.0.abs() < 1e-3 {
                Some(g.a.0)
            } else if !vertical && d.1.abs() < 1e-3 {
                Some(g.a.1)
            } else {
                None
            }
        })
        .filter(|t| *t > lo - 1e-6 && *t < hi + 1e-6)
        .collect();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let best = v.windows(2).max_by(|a, b| {
        (a[1] - a[0])
            .total_cmp(&(b[1] - b[0]))
            .then(b[0].total_cmp(&a[0]))
    });
    match best {
        Some(w) => (w[0] + w[1]) / 2.0,
        None => (lo + hi) / 2.0,
    }
}

/// Snaps a direction within 2 degrees of an axis onto it.
fn snap(d: (f64, f64)) -> (f64, f64) {
    let d = unit(d);
    let lim = 2f64.to_radians().sin();
    if d.1.abs() < lim {
        (d.0.signum(), 0.0)
    } else if d.0.abs() < lim {
        (0.0, d.1.signum())
    } else {
        d
    }
}

/// Views for a model that has none (an imported one): the two sections through the middle
/// bays and the four compass elevations the drawing set would place. Level plans need no row.
/// Returns how many were added; a model with views or with nothing in plan is left alone.
pub fn add_default_views(m: &mut Model) -> Result<usize, SheetError> {
    if !m.view_defs().map_err(me)?.is_empty() {
        return Ok(0);
    }
    let (plans, bb, body) = level_plans(m)?;
    if plans.is_empty() {
        return Ok(0);
    }
    let grids = m.grid_axes(None).map_err(me)?;
    let mut specs = sections(m, bb, &grids)?;
    specs.extend(ddc_model::compass_elevations_around(body, ELEVATION_OFF));
    m.atomic("default_views", |m| {
        for s in &specs {
            m.add_view_def(s)?;
        }
        Ok(())
    })
    .map_err(me)?;
    Ok(specs.len())
}

/// Section 1-1 along the first flight of the lowest stair, 2-2 across it through the middle of
/// the widest bay; with no stair, 1-1 across the short side and 2-2 along the long one.
fn sections(m: &Model, bb: [f64; 4], grids: &[GridAxis]) -> Result<Vec<SectionSpec>, SheetError> {
    let mut stair = None;
    for (lv, ..) in m.level_list().map_err(me)? {
        if let Some(id) = m.stairs(lv).map_err(me)?.first() {
            let s = m.stair(*id).map_err(me)?;
            stair = Some((s.origin, snap((s.angle.cos(), s.angle.sin()))));
            break;
        }
    }
    let mid_x = bay_middle(grids, true, bb[0], bb[2]);
    let mid_y = bay_middle(grids, false, bb[1], bb[3]);
    let along_y = |name: &str| section_through(name, (mid_x, mid_y), (0.0, 1.0), bb);
    let along_x = |name: &str| section_through(name, (mid_x, mid_y), (1.0, 0.0), bb);
    Ok(match stair {
        Some((at, d)) => {
            let first = section_through("1-1", at, d, bb);
            let second = if d.0.abs() >= d.1.abs() {
                along_y("2-2")
            } else {
                along_x("2-2")
            };
            vec![first, second]
        }
        None if bb[2] - bb[0] >= bb[3] - bb[1] => vec![along_y("1-1"), along_x("2-2")],
        None => vec![along_x("1-1"), along_y("2-2")],
    })
}

/// Elevation title by its end axes («1-5») when grid axes cross it, else by compass side.
fn elevation_title(s: &SectionSpec, grids: &[GridAxis], text: &dyn Fn(&str) -> String) -> String {
    let u = unit((s.b.0 - s.a.0, s.b.1 - s.a.1));
    let mut on: Vec<(f64, &str)> = grids
        .iter()
        .filter(|g| dot(unit((g.b.0 - g.a.0, g.b.1 - g.a.1)), u).abs() < 0.3)
        .map(|g| (dot((g.a.0 - s.a.0, g.a.1 - s.a.1), u), g.label.as_str()))
        .collect();
    on.sort_by(|a, b| a.0.total_cmp(&b.0));
    match (on.first(), on.last()) {
        (Some(a), Some(b)) if on.len() >= 2 && !a.1.is_empty() => text("dset.title.elevation_axes")
            .replace("{a}", a.1)
            .replace("{b}", b.1),
        _ => {
            let side = s.name.to_lowercase();
            text(&format!("dset.title.elevation_{side}"))
        }
    }
}

/// A view to place: what it shows, its title, the drawing.
struct Item {
    view: DsetRef,
    title: String,
    draw: PlanView,
    /// Sections and elevations a plan marks.
    shows: Vec<SectionSpec>,
    /// Floor levels a plan marks (one, or every level of a typical floor) and its note.
    marks: PlanMarks,
}

fn foot(i: &Item, scale: f64, std: &str, ctx: &Ctx) -> Result<Foot, SheetError> {
    footprint(&i.draw, scale, std, ctx, &i.title, &i.shows, &i.marks)
}

fn feet(items: &[&Item], scale: f64, std: &str, ctx: &Ctx) -> Result<Vec<Foot>, SheetError> {
    items.iter().map(|i| foot(i, scale, std, ctx)).collect()
}

/// A level's plan: id, name, elevation, drawing.
type LevelPlan = (i64, String, f64, PlanView);

/// A sheet of views: stable key, name, viewports, ids of the levels of a typical floor.
type ViewSheet = (String, String, Vec<DsetViewport>, Option<String>);

/// Scales of the plans and of the other views, and the paper of every sheet.
struct Fit {
    plan_scale: f64,
    view_scale: f64,
    paper: String,
}

/// The drawing set plan for the model; see the module note. `ctx.schedules` gives the model
/// schedules by key, `ctx.text` every word on the sheets.
pub fn plan_drawing_set(m: &Model, opt: &DsetOptions, ctx: &Ctx) -> Result<Planned, SheetError> {
    let text = ctx.text;
    let mut warnings = vec![];
    let grids = m.grid_axes(None).map_err(me)?;
    let (plans, bb, body) = level_plans(m)?;
    if plans.is_empty() {
        return Err(SheetError(text("dset.err.empty")));
    }
    let secs = if opt.sections {
        sections(m, bb, &grids)?
    } else {
        vec![]
    };
    let elevs = if opt.elevations {
        ddc_model::compass_elevations_around(body, ELEVATION_OFF)
    } else {
        vec![]
    };
    let ends: Vec<SectionSpec> = secs.iter().chain(&elevs).cloned().collect();
    let plan_items = if opt.plans {
        plan_items(&plans, &ends, text)
    } else {
        vec![]
    };
    let views: Vec<SectionSpec> = secs.iter().chain(&elevs).cloned().collect();
    let (sec_items, elev_items) = view_items(m, &views, &grids, text)?;
    let plan_refs: Vec<&Item> = plan_items.iter().map(|(i, _)| i).collect();
    let view_refs: Vec<&Item> = sec_items.iter().chain(&elev_items).collect();
    let fit = choose_fit(opt, ctx, &plan_refs, &view_refs, &mut warnings)?;
    let reg = Region::of(&opt.standard, &fit.paper)?;
    let mut view_sheets = plan_sheets(&plan_items, fit.plan_scale, &reg, opt, ctx)?;
    let names: Vec<&str> = secs.iter().map(|s| s.name.as_str()).collect();
    let (vs, std) = (fit.view_scale, &opt.standard);
    view_sheets.extend(packed_sheets(
        &sec_items, "sections", &names, vs, &reg, std, ctx,
    )?);
    view_sheets.extend(packed_sheets(
        &elev_items,
        "elevations",
        &[],
        vs,
        &reg,
        std,
        ctx,
    )?);
    let fields = stamp_from_project(&opt.standard, &m.project_fields().map_err(me)?);
    let def = |number: String, name: String| SheetDef {
        number,
        name,
        paper: fit.paper.clone(),
        landscape: true,
        standard: opt.standard.clone(),
        fields: fields.clone(),
    };
    let sheets = number_sheets(view_sheets, &schedule_tables(opt, m, ctx)?, &reg, text, &def);
    let mut tags = vec![];
    for (i, _) in &plan_items {
        let (sc, std) = (fit.plan_scale, &opt.standard);
        tags.extend(settle_tags(&i.draw, sc, std, ctx, &i.title, &i.shows, &i.marks)?);
    }
    Ok(Planned {
        plan: DsetPlan {
            set_name: text("dset.set_name"),
            views,
            sheets,
            tags,
        },
        plan_scale: fit.plan_scale,
        view_scale: fit.view_scale,
        paper: fit.paper,
        warnings,
    })
}

/// Plans of the levels that draw any model element (walls, forms, components, slabs...), in
/// level order; the extent of the drawings, and of the elements alone, both with every
/// element of the model.
fn level_plans(m: &Model) -> Result<(Vec<LevelPlan>, [f64; 4], [f64; 4]), SheetError> {
    let mut plans = vec![];
    let empty = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let (mut bb, mut body) = (empty, empty);
    for (lv, name, z) in m.level_list().map_err(me)? {
        let vp = ViewportDef {
            sheet: 0,
            view: ViewRef::Plan {
                level: lv,
                cut: DEFAULT_CUT,
            },
            scale: 100.0,
            center: (0.0, 0.0),
            crop: None,
            title: String::new(),
        };
        let d = view_drawing(m, &vp)?;
        if !d.prims.iter().any(draws_element) {
            continue;
        }
        grow(&mut bb, [d.bbox[0], d.bbox[1]]);
        grow(&mut bb, [d.bbox[2], d.bbox[3]]);
        for p in &d.prims {
            element_points(p, &mut |q| grow(&mut body, [q.0, q.1]));
        }
        plans.push((lv, name, z, d));
    }
    if let Some(all) = m.plan_bbox().map_err(me)? {
        for b in [&mut bb, &mut body] {
            grow(b, [all[0], all[1]]);
            grow(b, [all[2], all[3]]);
        }
    }
    if !body[0].is_finite() {
        body = bb;
    }
    Ok((plans, bb, body))
}

/// Points of a model element's outline in plan; door leaves swing out of the body, so not them.
fn element_points(p: &ViewPrim, f: &mut dyn FnMut((f64, f64))) {
    match p {
        ViewPrim::Cut { ring, .. } => ring.iter().for_each(|&q| f(q)),
        ViewPrim::SectionCut { rings, .. } => rings.iter().for_each(|&q| f(q)),
        ViewPrim::Edge { a, b, .. }
        | ViewPrim::WindowLine { a, b, .. }
        | ViewPrim::CurtainLine { a, b, .. } => {
            f(*a);
            f(*b);
        }
        _ => {}
    }
}

/// A primitive of a model element, not an axis, level line, dimension or note.
fn draws_element(p: &ViewPrim) -> bool {
    matches!(
        p,
        ViewPrim::Cut { .. }
            | ViewPrim::SectionCut { .. }
            | ViewPrim::Edge { .. }
            | ViewPrim::DoorLeaf { .. }
            | ViewPrim::WindowLine { .. }
            | ViewPrim::CurtainLine { .. }
    )
}

/// Plan items with the levels each stands for: alike neighbours make one typical floor.
/// `ends` are the sections and elevations the plans mark.
fn plan_items(
    plans: &[LevelPlan],
    ends: &[SectionSpec],
    text: &dyn Fn(&str) -> String,
) -> Vec<(Item, Vec<i64>)> {
    let rooms: Vec<Numbers> = plans.iter().map(|p| room_numbers(&p.3)).collect();
    let mut out = vec![];
    for group in typical_floors(plans, &rooms) {
        let (lv, name, _, d) = &plans[group[0]];
        let mut draw = d.clone();
        let mut note = None;
        let title = match group[..] {
            [_] => text("dset.title.plan").replace("{level}", name),
            [.., last] => {
                let floors: Vec<&Numbers> = group.iter().map(|&k| &rooms[k]).collect();
                if let Some(s) = typical_rooms(&floors) {
                    show_rooms(&mut draw, &s.marks);
                    note = room_note(text, &s, name, &plans[last].1);
                }
                let range = level_range(name, &plans[last].1);
                text("dset.title.plan_typical").replace("{levels}", &range)
            }
            [] => continue,
        };
        let item = Item {
            view: DsetRef::Plan {
                level: *lv,
                cut: DEFAULT_CUT,
            },
            title,
            draw,
            shows: ends.to_vec(),
            marks: PlanMarks {
                levels: group.iter().map(|&k| plans[k].2).collect(),
                note,
            },
        };
        out.push((item, group.iter().map(|&k| plans[k].0).collect()));
    }
    out
}

/// Section and elevation items of `views` (indices are `DsetRef::Def`), sections first.
fn view_items(
    m: &Model,
    views: &[SectionSpec],
    grids: &[GridAxis],
    text: &dyn Fn(&str) -> String,
) -> Result<(Vec<Item>, Vec<Item>), SheetError> {
    let (mut secs, mut elevs) = (vec![], vec![]);
    for (k, s) in views.iter().enumerate() {
        let section = s.kind == ViewKind::Section;
        let item = Item {
            view: DsetRef::Def(k),
            title: if section {
                text("dset.title.section").replace("{name}", &s.name)
            } else {
                elevation_title(s, grids, text)
            },
            draw: m.section_view(s, grids).map_err(me)?,
            shows: vec![],
            marks: PlanMarks::default(),
        };
        if section {
            secs.push(item);
        } else {
            elevs.push(item);
        }
    }
    Ok((secs, elevs))
}

/// Plans lead: the finest scale, then the smallest paper, at which every plan fits a sheet
/// alone; sections and elevations then take the finest scale, not finer than the plans', at
/// which each fits that paper. A view that fits nowhere adds the `too_big` warning once.
fn choose_fit(
    opt: &DsetOptions,
    ctx: &Ctx,
    plans: &[&Item],
    views: &[&Item],
    warnings: &mut Vec<String>,
) -> Result<Fit, SheetError> {
    let scales: Vec<f64> = match opt.scale {
        Some(s) => vec![s],
        None => SCALES.to_vec(),
    };
    let papers: Vec<String> = match &opt.paper {
        Some(p) => vec![p.clone()],
        None => PAPERS.iter().map(|p| p.to_string()).collect(),
    };
    let lead = if plans.is_empty() { views } else { plans };
    let mut chosen = None;
    'find: for &s in &scales {
        let f = feet(lead, s, &opt.standard, ctx)?;
        for p in &papers {
            let reg = Region::of(&opt.standard, p)?;
            if f.iter().all(|f| reg.fits(f)) {
                chosen = Some((s, p.clone()));
                break 'find;
            }
        }
    }
    let too_big = (ctx.text)("dset.warn.too_big");
    let (plan_scale, paper) = match chosen {
        Some(c) => c,
        None => {
            warnings.push(too_big.clone());
            let s = *scales.last().unwrap_or(&100.0);
            (s, papers.last().cloned().unwrap_or_default())
        }
    };
    let reg = Region::of(&opt.standard, &paper)?;
    let coarser = SCALES.iter().copied().filter(|&s| s > plan_scale);
    let mut view_scale = None;
    for s in std::iter::once(plan_scale).chain(coarser) {
        if feet(views, s, &opt.standard, ctx)?
            .iter()
            .all(|f| reg.fits(f))
        {
            view_scale = Some(s);
            break;
        }
    }
    let view_scale = view_scale.unwrap_or_else(|| {
        if !warnings.contains(&too_big) {
            warnings.push(too_big);
        }
        SCALES.iter().copied().fold(plan_scale, f64::max)
    });
    Ok(Fit {
        plan_scale,
        view_scale,
        paper,
    })
}

fn viewport(i: &Item, center: (f64, f64), scale: f64) -> DsetViewport {
    DsetViewport {
        view: i.view,
        scale,
        center,
        title: i.title.clone(),
    }
}

/// One sheet per plan item, keyed by its first level; a typical floor lists its levels.
fn plan_sheets(
    items: &[(Item, Vec<i64>)],
    scale: f64,
    reg: &Region,
    opt: &DsetOptions,
    ctx: &Ctx,
) -> Result<Vec<ViewSheet>, SheetError> {
    let mut out = vec![];
    for (i, lvs) in items {
        let f = feet(&[i], scale, &opt.standard, ctx)?;
        let c = pack_views(&f, reg)[0][0].1;
        let typical = (lvs.len() > 1).then(|| {
            let ids: Vec<String> = lvs.iter().map(|l| l.to_string()).collect();
            ids.join(",")
        });
        let key = format!("plan:{}", lvs[0]);
        out.push((key, i.title.clone(), vec![viewport(i, c, scale)], typical));
    }
    Ok(out)
}

/// Views packed onto as few sheets as they fit, keyed `<key>:<page>`; section sheets name
/// the sections they hold (`names`, by item index).
fn packed_sheets(
    items: &[Item],
    key: &str,
    names: &[&str],
    scale: f64,
    reg: &Region,
    standard: &str,
    ctx: &Ctx,
) -> Result<Vec<ViewSheet>, SheetError> {
    let refs: Vec<&Item> = items.iter().collect();
    let f = feet(&refs, scale, standard, ctx)?;
    let mut out = vec![];
    for (k, page) in pack_views(&f, reg).into_iter().enumerate() {
        let vps: Vec<DsetViewport> = page
            .iter()
            .map(|&(i, c)| viewport(&items[i], c, scale))
            .collect();
        let mut nm = (ctx.text)(&format!("dset.sheet.{key}"));
        if !names.is_empty() {
            let on: Vec<&str> = page.iter().map(|&(i, _)| names[i]).collect();
            nm = format!("{nm} {}", on.join(", "));
        }
        out.push((format!("{key}:{}", k + 1), nm, vps, None));
    }
    Ok(out)
}

/// The model schedules the set lists, those with rows; the area and finishes schedules are
/// built from the model itself, the others come from `ctx.schedules`.
fn schedule_tables(
    opt: &DsetOptions,
    m: &Model,
    ctx: &Ctx,
) -> Result<Vec<(String, Table)>, SheetError> {
    if !opt.schedules {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for k in SCHEDULES {
        let t = match k {
            super::dset_area::AREAS => Some(super::dset_area::areas(m, ctx.text)?),
            super::dset_area::FINISHES => Some(super::dset_area::finishes(m, ctx.text)?),
            _ => (ctx.schedules)(k),
        };
        if let Some(t) = t.filter(|t| !t.rows.is_empty()) {
            out.push((k.to_string(), t));
        }
    }
    Ok(out)
}

/// All sheets in order: the cover with the sheet list, the view sheets, then schedule sheets
/// as many as the sheet list and `tables` flow over; numbered with the set's prefix.
fn number_sheets(
    view_sheets: Vec<ViewSheet>,
    tables: &[(String, Table)],
    reg: &Region,
    text: &dyn Fn(&str) -> String,
    def: &dyn Fn(String, String) -> SheetDef,
) -> Vec<DsetSheet> {
    let prefix = text("dset.prefix");
    let number = |i: usize| format!("{prefix}{}", i + 1);
    let cover = text("dset.sheet.cover");
    let sched = text("dset.sheet.schedules");
    // the sheet list grows with the sheet count it lists: settle in a few rounds
    let mut pages = 1;
    let mut parts = vec![];
    for _ in 0..4 {
        let mut rows = vec![(number(0), cover.clone())];
        for (k, (_, nm, ..)) in view_sheets.iter().enumerate() {
            rows.push((number(k + 1), nm.clone()));
        }
        for k in 1..pages {
            rows.push((number(view_sheets.len() + k), sched.clone()));
        }
        let mut flow = vec![(format!("sheet_list:{DSET_SET}"), sheet_list(&rows, text))];
        flow.extend(tables.iter().cloned());
        parts = flow_tables(&flow, reg, text);
        let need = parts.iter().map(|p| p.page + 1).max().unwrap_or(1);
        if need == pages {
            break;
        }
        pages = need;
    }
    let sched_of = |page: usize| -> Vec<DsetSchedule> {
        parts
            .iter()
            .filter(|p| p.page == page)
            .map(|p| DsetSchedule {
                key: p.key.clone(),
                pos: p.pos,
                max_h: p.h + 1.0,
            })
            .collect()
    };
    let mut sheets = vec![DsetSheet {
        key: "cover".into(),
        def: def(number(0), cover.clone()),
        viewports: vec![],
        schedules: sched_of(0),
    }];
    for (k, (key, nm, vps, typical)) in view_sheets.into_iter().enumerate() {
        let mut d = def(number(k + 1), nm);
        if let Some(t) = typical {
            d.fields.insert(DSET_LEVELS.into(), t);
        }
        sheets.push(DsetSheet {
            key,
            def: d,
            viewports: vps,
            schedules: vec![],
        });
    }
    let first_sched = sheets.len();
    for p in 1..pages {
        sheets.push(DsetSheet {
            key: format!("schedules:{p}"),
            def: def(number(first_sched + p - 1), sched.clone()),
            viewports: vec![],
            schedules: sched_of(p),
        });
    }
    sheets
}

/// Hash of a plan drawing: every primitive with its geometry to the millimetre and its words,
/// without element ids, which differ from floor to floor, in any order. Room numbers are left
/// out (they carry the floor); room names and areas, opening marks and tag places stay in.
fn plan_signature(d: &PlanView) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::mem::discriminant;
    let mm = |v: f64| (v * 1000.0).round() as i64;
    let pt = |p: (f64, f64)| (mm(p.0), mm(p.1));
    let ring = |r: &[(f64, f64)], h: &mut DefaultHasher| {
        r.len().hash(h);
        r.iter().for_each(|&p| pt(p).hash(h));
    };
    let mut each: Vec<u64> = d
        .prims
        .iter()
        .map(|p| {
            let h = &mut DefaultHasher::new();
            discriminant(p).hash(h);
            match p {
                ViewPrim::Cut {
                    layer,
                    material,
                    ring: r,
                    ..
                } => {
                    (layer, material).hash(h);
                    ring(r.as_slice(), &mut *h);
                }
                ViewPrim::DoorLeaf { a, b, .. }
                | ViewPrim::WindowLine { a, b, .. }
                | ViewPrim::CurtainLine { a, b, .. }
                | ViewPrim::Level { a, b, .. } => (pt(*a), pt(*b)).hash(h),
                ViewPrim::DoorSwing {
                    center,
                    radius,
                    a0,
                    a1,
                    ..
                } => (pt(*center), mm(*radius), mm(*a0), mm(*a1)).hash(h),
                ViewPrim::RoomTag {
                    pos,
                    name,
                    area,
                    finish,
                    ..
                } => (pt(*pos), name, mm(*area), finish).hash(h),
                ViewPrim::Grid {
                    label,
                    a,
                    b,
                    bubbles,
                } => (label, pt(*a), pt(*b), bubbles).hash(h),
                ViewPrim::Dim {
                    side,
                    chain,
                    a,
                    b,
                    value,
                } => {
                    discriminant(side).hash(h);
                    (chain, pt(*a), pt(*b), mm(*value)).hash(h);
                }
                ViewPrim::Measure { a, b, value, reason, .. } => {
                    (pt(*a), pt(*b), value.map(mm), reason).hash(h);
                }
                ViewPrim::Tag { tag } => {
                    let t = &tag.text;
                    if tag.cat != ddc_model::TagCat::Room {
                        t.mark.hash(h);
                    }
                    (tag.cat, &t.name, t.area.map(mm), &t.finish, tag.spec.leader).hash(h);
                    (pt(tag.head), pt(tag.anchor)).hash(h);
                }
                ViewPrim::SectionCut {
                    table,
                    material,
                    rings,
                    holes,
                    ..
                } => {
                    (table, material).hash(h);
                    ring(rings.as_slice(), &mut *h);
                    holes.len().hash(h);
                    holes.iter().for_each(|r| ring(r.as_slice(), &mut *h));
                }
                ViewPrim::Edge {
                    table, class, a, b, ..
                } => {
                    discriminant(class).hash(h);
                    (table, pt(*a), pt(*b)).hash(h);
                }
                // model dimensions and view annotations carry their ids: such a plan stands alone
                other => format!("{other:?}").hash(h),
            }
            h.finish()
        })
        .collect();
    each.sort_unstable();
    let mut h = DefaultHasher::new();
    (each, d.bbox.map(mm)).hash(&mut h);
    h.finish()
}

/// Runs of neighbouring levels whose plans draw alike and whose room numbers one plan shows
/// truly for every floor (`rooms`, see [`typical_rooms`]), as indices into `plans`, in order.
fn typical_floors(plans: &[LevelPlan], rooms: &[Numbers]) -> Vec<Vec<usize>> {
    let sigs: Vec<u64> = plans.iter().map(|p| plan_signature(&p.3)).collect();
    let mut out: Vec<Vec<usize>> = vec![];
    for (k, sig) in sigs.iter().enumerate() {
        let joins = |g: &[usize]| {
            let floors: Vec<&Numbers> = g.iter().chain([&k]).map(|&i| &rooms[i]).collect();
            sigs[g[0]] == *sig && typical_rooms(&floors).is_some()
        };
        match out.last_mut() {
            Some(g) if joins(g.as_slice()) => g.push(k),
            _ => out.push(vec![k]),
        }
    }
    out
}

/// "Level 2" and "Level 40" give "Level 2\u{2013}40": the shared words once, then the range.
pub(super) fn level_range(first: &str, last: &str) -> String {
    let common: usize = first
        .char_indices()
        .zip(last.chars())
        .take_while(|((_, a), b)| a == b)
        .map(|((i, a), _)| i + a.len_utf8())
        .last()
        .unwrap_or(0);
    // keep whole numbers: "Level 12" and "Level 13" share "Level ", not "Level 1"
    let cut = first[..common]
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .len();
    if cut == 0 || cut == first.len() || cut == last.len() {
        return format!("{first} \u{2013} {last}");
    }
    // the first name whole, then what the last one adds after the shared words
    format!("{first}\u{2013}{}", &last[cut..])
}

/// Door and window mark prefixes of the drawings' language (catalog `dset.mark.*`).
pub fn mark_prefix(text: &dyn Fn(&str) -> String) -> MarkPrefix {
    MarkPrefix {
        door: text("dset.mark.door"),
        window: text("dset.mark.window"),
    }
}

/// Levels a drawing set tags: those with walls cut by their plan.
pub fn tag_levels(m: &Model) -> Result<Vec<i64>, SheetError> {
    let mut out = vec![];
    for (lv, ..) in m.level_list().map_err(me)? {
        let on = m.level_contents(lv).map_err(me)?;
        if on.iter().any(|(t, _)| *t == "wall") {
            out.push(lv);
        }
    }
    Ok(out)
}

/// Compass names a set titles elevations by, for the catalogue check.
pub fn compass_keys() -> Vec<String> {
    COMPASS
        .iter()
        .map(|c| format!("dset.title.elevation_{}", c.to_lowercase()))
        .collect()
}
