//! Model sheets → drawing: viewports (level plan, section/elevation) at scale with cropping, view captions,
//! schedules, a revision table above the title block, title-block auto fields (name, number, sheet N of M);
//! a sheet set → a multi-page vector PDF.

use std::collections::BTreeMap;

use ddc_model::{
    AnnoView, Model, PlanView, PlanViewSpec, SectionSpec, SheetDef, ViewPlacement, ViewPrim,
    ViewRef, ViewportDef,
};

use super::output::{builtin_table, section_marks, stamp_from_project};
use super::table::{draw_split, Layout, Room, Table};
use super::{
    arc_pts, compose_styled, data, err, num, pdf, text_w, LengthUnits, Mark, Pen, SheetDrawing,
    SheetError, SheetSpec, ViewStyle, Viewport, LEVELS, SCHED, STAMP, TITLES,
};

/// Scale denominator from which sections are filled solid (hatching is indistinguishable on paper).
pub const POCHE_FROM: f64 = 200.0;
pub(super) const POCHE_GRAY: u8 = 64;
/// Margin around an uncropped view, sheet mm.
pub(super) const PAD: f64 = 6.0;

/// Build environment: caption units, schedule tables by key, the sheet's position in the issue.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub units: LengthUnits,
    pub schedules: &'a dyn Fn(&str) -> Option<Table>,
    /// (1-based ordinal, total) for the "Sheet"/"Sheets" fields; None — taken from the model's sheet list.
    pub index: Option<(usize, usize)>,
    /// Printing to another format (name, landscape): the whole sheet is fitted and centred.
    pub paper: Option<(&'a str, bool)>,
    /// Catalog string by key: service labels on the sheet and build messages in the UI language.
    pub text: &'a dyn Fn(&str) -> String,
    /// Views and their placements built once for the whole issue; None — read when the sheet is built.
    pub refs: Option<&'a SheetRefs>,
}

/// What section marks on plans need. The query spans all sheets, so when printing a set
/// it is computed once rather than per sheet.
#[derive(Clone, Debug, Default)]
pub struct SheetRefs {
    pub defs: Vec<(i64, SectionSpec)>,
    pub placements: Vec<ViewPlacement>,
}

impl SheetRefs {
    pub fn load(m: &Model) -> Result<SheetRefs, SheetError> {
        Ok(SheetRefs {
            defs: m.view_defs().map_err(me)?,
            placements: m.view_placements().map_err(me)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacedKind {
    Viewport(i64),
    Schedule(i64),
    Legend(i64),
}

/// A placed object: rectangle [x, y, w, h] on the sheet, mm; for a viewport — the model centre (m)
/// and the scale denominator (sheet point = rect centre + (model − model_c) · 1000 / scale).
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub kind: PlacedKind,
    pub rect: [f64; 4],
    pub model_c: [f64; 2],
    pub scale: f64,
}

pub struct Composed {
    pub drawing: SheetDrawing,
    pub placed: Vec<Placed>,
    /// What did not fit on the sheet: shown on the canvas, in the preview and in the print report.
    pub warnings: Vec<String>,
}

fn me(e: ddc_model::Error) -> SheetError {
    SheetError(e.to_string())
}

/// Drawing of a viewport's view in model coordinates.
pub fn view_drawing(m: &Model, v: &ViewportDef) -> Result<PlanView, SheetError> {
    match v.view {
        ViewRef::Plan { level, .. }
            if m.plan_range_saved(level).map_err(me)?.is_some() =>
        {
            m.plan_view_ranged(level, m.grid_axes(Some(level)).map_err(me)?)
                .map_err(me)
        }
        ViewRef::Plan { level, cut } => m
            .plan_view(&PlanViewSpec {
                level,
                cut_height: cut,
                grids: m.grid_axes(Some(level)).map_err(me)?,
            })
            .map_err(me),
        ViewRef::View(id) => m
            .draw_view_def(id, &m.grid_axes(None).map_err(me)?)
            .map_err(me),
    }
}

/// Extents of view primitives (excluding captions), m; an empty view — a 10 m square at the origin.
pub fn extent(d: &PlanView) -> [f64; 4] {
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut add = |p: (f64, f64)| {
        if p.0.is_finite() && p.1.is_finite() {
            bb = [
                bb[0].min(p.0),
                bb[1].min(p.1),
                bb[2].max(p.0),
                bb[3].max(p.1),
            ];
        }
    };
    for p in &d.prims {
        match p {
            ViewPrim::Cut { ring, .. } => ring.iter().for_each(|&q| add(q)),
            ViewPrim::SectionCut { rings, .. } => rings.iter().for_each(|&q| add(q)),
            ViewPrim::DoorLeaf { a, b, .. }
            | ViewPrim::WindowLine { a, b, .. }
            | ViewPrim::CurtainLine { a, b, .. }
            | ViewPrim::Grid { a, b, .. }
            | ViewPrim::Dim { a, b, .. }
            | ViewPrim::Measure { a, b, .. }
            | ViewPrim::Edge { a, b, .. }
            | ViewPrim::Level { a, b, .. } => {
                add(*a);
                add(*b);
            }
            ViewPrim::DoorSwing { center, radius, .. } => {
                add((center.0 - radius, center.1 - radius));
                add((center.0 + radius, center.1 + radius));
            }
            ViewPrim::RoomTag { pos, .. } => add(*pos),
            ViewPrim::Anno { item } => {
                for q in item.shape.grips() {
                    add(q);
                }
            }
            _ => {}
        }
    }
    if bb[2] - bb[0] > 1e-6 && bb[3] - bb[1] > 1e-6 {
        bb
    } else if bb[0].is_finite() {
        [bb[0] - 1.0, bb[1] - 1.0, bb[2] + 1.0, bb[3] + 1.0]
    } else {
        [-5.0, -5.0, 5.0, 5.0]
    }
}

/// Viewport rectangle on the sheet from its model box `bx`.
fn rect_of(v: &ViewportDef, bx: [f64; 4]) -> [f64; 4] {
    let pad = if v.crop.is_some() { 0.0 } else { PAD };
    let w = (bx[2] - bx[0]) * 1000.0 / v.scale + 2.0 * pad;
    let h = (bx[3] - bx[1]) * 1000.0 / v.scale + 2.0 * pad;
    [v.center.0 - w / 2.0, v.center.1 - h / 2.0, w, h]
}

/// Windows device names: a file "CON.pdf" and the like cannot be created with any extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const MAX_NAME: usize = 120;

/// File name from a template: `{number}`, `{name}`; characters forbidden in names → "_",
/// a Windows device name (CON, NUL, COM1…) gets a "_" prefix; a long name is shortened.
pub fn file_name(pattern: &str, number: &str, name: &str, ext: &str) -> String {
    let raw = pattern.replace("{number}", number).replace("{name}", name);
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    // leaves room for the extension and the temporary name used when writing: the filesystem limit is 255 characters
    let clean: String = clean.trim().chars().take(MAX_NAME).collect();
    let clean = clean.trim().trim_end_matches('.').trim();
    let base = if clean.is_empty() { "sheet" } else { clean };
    let stem = base.split('.').next().unwrap_or("").trim_end();
    let reserved = RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem));
    format!("{}{base}.{ext}", if reserved { "_" } else { "" })
}

/// A unique name within one export (case-insensitive, as in Windows):
/// a duplicate gets the suffix "-2", "-3"… before the extension.
pub fn unique_name(name: String, used: &mut std::collections::HashSet<String>) -> String {
    if used.insert(name.to_lowercase()) {
        return name;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut k = 2;
    loop {
        let n = format!("{stem}-{k}{ext}");
        if used.insert(n.to_lowercase()) {
            return n;
        }
        k += 1;
    }
}

/// Title-block fields the user left empty: name, number, position, scale, latest revision.
fn auto_fields(
    m: &Model,
    id: i64,
    s: &SheetDef,
    vps: &[(i64, ViewportDef)],
    ctx: &Ctx,
) -> Result<BTreeMap<String, String>, SheetError> {
    let mut f = s.fields.clone();
    f.retain(|_, v| !v.trim().is_empty());
    // priority order: the sheet's value, then project fields, then auto values
    for (k, v) in stamp_from_project(&s.standard, &m.project_fields().map_err(me)?) {
        f.entry(k).or_insert(v);
    }
    let (pos, total) = match ctx.index {
        Some(x) => x,
        None => {
            let all = m.sheets().map_err(me)?;
            let k = all.iter().position(|x| x.0 == id).unwrap_or(0);
            (k + 1, all.len())
        }
    };
    let revs = m.revisions(id).map_err(me)?;
    let last = revs.last();
    let mut put = |k: &str, v: String| {
        if !v.is_empty() {
            f.entry(k.to_string()).or_insert(v);
        }
    };
    put("title", s.name.clone());
    let mut scales: Vec<f64> = vps.iter().map(|v| v.1.scale).collect();
    scales.sort_by(f64::total_cmp);
    scales.dedup();
    let gost = s.standard == "gost";
    match scales.as_slice() {
        [] => {}
        [one] => put("scale", format!("1:{one}")),
        _ => put("scale", (ctx.text)("sheets.paper.as_indicated")),
    }
    if gost {
        put("sheet", pos.to_string());
        put("sheets", total.to_string());
        put("date", last.map(|r| r.date.clone()).unwrap_or_default());
    } else {
        put("sheet", format!("{pos}/{total}"));
        put("identification", s.number.clone());
        put(
            "issue_date",
            last.map(|r| r.date.clone()).unwrap_or_default(),
        );
    }
    put("revision", last.map(|r| r.mark.clone()).unwrap_or_default());
    Ok(f)
}

pub(super) fn clip_seg(a: [f64; 2], b: [f64; 2], r: [f64; 4]) -> Option<(f64, f64)> {
    let (d0, d1) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-d0, a[0] - r[0]),
        (d0, r[0] + r[2] - a[0]),
        (-d1, a[1] - r[1]),
        (d1, r[1] + r[3] - a[1]),
    ] {
        if p.abs() < 1e-12 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 < t1).then_some((t0, t1))
}

/// Polygon ∩ rectangle (Sutherland–Hodgman): the rectangle is convex.
fn clip_ring(ring: &[[f64; 2]], r: [f64; 4]) -> Vec<[f64; 2]> {
    let mut pts = ring.to_vec();
    let edges: [(usize, f64, bool); 4] = [
        (0, r[0], true),
        (0, r[0] + r[2], false),
        (1, r[1], true),
        (1, r[1] + r[3], false),
    ];
    for (ax, v, keep_ge) in edges {
        if pts.is_empty() {
            break;
        }
        let inside = |p: &[f64; 2]| if keep_ge { p[ax] >= v } else { p[ax] <= v };
        let mut out = vec![];
        for i in 0..pts.len() {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            let (ip, iq) = (inside(&p), inside(&q));
            if ip {
                out.push(p);
            }
            if ip != iq {
                let t = (v - p[ax]) / (q[ax] - p[ax]);
                out.push([p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]);
            }
        }
        pts = out;
    }
    pts
}

fn inside(p: [f64; 2], r: [f64; 4]) -> bool {
    p[0] >= r[0] && p[0] <= r[0] + r[2] && p[1] >= r[1] && p[1] <= r[1] + r[3]
}

/// Text stays only if its whole box (anchor, line width, height) is inside the crop.
fn text_inside(pos: [f64; 2], h: f64, rot: f64, s: &str, r: [f64; 4]) -> bool {
    let (c, sn) = (rot.to_radians().cos(), rot.to_radians().sin());
    let w = text_w(s, h);
    [[0.0, 0.0], [w, 0.0], [0.0, h], [w, h]].iter().all(|q| {
        inside(
            [pos[0] + q[0] * c - q[1] * sn, pos[1] + q[0] * sn + q[1] * c],
            r,
        )
    })
}

impl SheetDrawing {
    /// Crop of view `k` by a sheet rectangle [x, y, w, h]: lines and fills are clipped, texts
    /// that do not fit the box entirely are dropped; view captions (Titles layer) are left alone.
    pub fn clip(&mut self, k: usize, r: [f64; 4]) {
        let marks = std::mem::take(&mut self.marks);
        for (vp, m) in marks {
            if vp != Some(k) {
                self.marks.push((vp, m));
                continue;
            }
            match m {
                Mark::Line {
                    pts,
                    closed,
                    weight,
                    rgb,
                    layer,
                } => {
                    if pts.iter().all(|&p| inside(p, r)) || layer == TITLES {
                        self.marks.push((
                            vp,
                            Mark::Line {
                                pts,
                                closed,
                                weight,
                                rgb,
                                layer,
                            },
                        ));
                        continue;
                    }
                    let mut path = pts;
                    if closed && path.len() > 2 {
                        path.push(path[0]);
                    }
                    let mut cur: Vec<[f64; 2]> = vec![];
                    let flush = |cur: &mut Vec<[f64; 2]>, out: &mut Vec<(Option<usize>, Mark)>| {
                        if cur.len() >= 2 {
                            out.push((
                                vp,
                                Mark::Line {
                                    pts: std::mem::take(cur),
                                    closed: false,
                                    weight,
                                    rgb,
                                    layer,
                                },
                            ));
                        }
                        cur.clear();
                    };
                    for w in path.windows(2) {
                        let (a, b) = (w[0], w[1]);
                        match clip_seg(a, b, r) {
                            None => flush(&mut cur, &mut self.marks),
                            Some((t0, t1)) => {
                                let at =
                                    |t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                                let (p, q) = (at(t0), at(t1));
                                if t0 > 0.0 || cur.is_empty() {
                                    flush(&mut cur, &mut self.marks);
                                    cur.push(p);
                                }
                                cur.push(q);
                                if t1 < 1.0 {
                                    flush(&mut cur, &mut self.marks);
                                }
                            }
                        }
                    }
                    flush(&mut cur, &mut self.marks);
                }
                Mark::Fill {
                    rings,
                    gray,
                    rgb,
                    layer,
                } => {
                    let rings: Vec<Vec<[f64; 2]>> = rings
                        .iter()
                        .map(|g| clip_ring(g, r))
                        .filter(|g| g.len() >= 3)
                        .collect();
                    if !rings.is_empty() {
                        self.marks.push((
                            vp,
                            Mark::Fill {
                                rings,
                                gray,
                                rgb,
                                layer,
                            },
                        ));
                    }
                }
                Mark::Text {
                    pos, h, rot, ref s, ..
                } if !text_inside(pos, h, rot, s, r) => {}
                m => self.marks.push((vp, m)),
            }
        }
    }
}

/// View title geometry, mm: title and scale text heights, the number bubble's radius, the
/// title line's drop below the view and the text's lift above that line.
const TITLE_H: f64 = 3.5;
const SCALE_H: f64 = 2.5;
const BUBBLE_R: f64 = 4.0;
const TITLE_DROP: f64 = 3.0;
const TITLE_LIFT: f64 = 1.2;

/// Title line of a view under `rect`: where the title text starts, and the line's height.
fn title_line(rect: [f64; 4]) -> (f64, f64) {
    (rect[0] + 2.0 * BUBBLE_R + 2.0, rect[1] - TITLE_DROP - BUBBLE_R)
}

/// View caption under the viewport: number in a circle, name on a shelf line, scale under the shelf.
pub(super) fn view_title(pen: &mut Pen, rect: [f64; 4], detail: &str, title: &str, scale: f64) {
    if title.is_empty() {
        return;
    }
    let (th, sh, r) = (TITLE_H, SCALE_H, BUBBLE_R);
    let (x, cy) = title_line(rect);
    let c = [rect[0] + r, cy];
    pen.line(arc_pts(c, r, 0.0, 360.0), true, 0.35, TITLES);
    let dh = if text_w(detail, 3.0) > 1.6 * r {
        3.0 * 1.6 * r / text_w(detail, 3.0)
    } else {
        3.0
    };
    pen.text(
        [c[0] - text_w(detail, dh) / 2.0, c[1] - dh / 2.0],
        dh,
        0.0,
        detail,
        TITLES,
    );
    let sc = format!("1:{scale}");
    let len = text_w(title, th).max(text_w(&sc, sh)) + 4.0;
    pen.text([x, cy + TITLE_LIFT], th, 0.0, title, TITLES);
    pen.line(vec![[x, cy], [x + len, cy]], false, 0.5, TITLES);
    pen.text([x, cy - TITLE_LIFT - sh], sh, 0.0, &sc, TITLES);
}

/// Finished floor levels of a plan relative to ±0.000, in a frame (GOST 21.501, GOST 21.101),
/// on the title line at the right edge of the view, or past the title when the view is narrow.
/// A typical floor plan lists the levels it stands for, one per line downwards; past
/// [`MARK_LINES`] the first ones, an ellipsis and the last.
pub(super) fn plan_level_mark(
    pen: &mut Pen,
    rect: [f64; 4],
    standard: &str,
    title: &str,
    zs: &[f64],
) {
    if zs.is_empty() {
        return;
    }
    let sep = super::data()["standards"][standard]["decimal"]
        .as_str()
        .unwrap_or(".")
        .to_string();
    let mut lines: Vec<String> = zs
        .iter()
        .map(|&z| super::annot::elev_text(z, &sep))
        .collect();
    if lines.len() > MARK_LINES {
        let last = lines.pop().unwrap_or_default();
        lines.truncate(MARK_LINES - 2);
        lines.push("\u{2026}".into());
        lines.push(last);
    }
    let (th, pad) = (TITLE_H, MARK_PAD);
    let w = lines.iter().map(|s| text_w(s, th)).fold(0.0, f64::max) + 2.0 * pad;
    let (tx, cy) = title_line(rect);
    let after_title = if title.is_empty() {
        rect[0]
    } else {
        tx + text_w(title, TITLE_H) + MARK_GAP
    };
    let x = (rect[0] + rect[2] - w).max(after_title);
    let base = cy + TITLE_LIFT;
    let low = base - MARK_STEP * (lines.len() - 1) as f64;
    let (y0, y1) = (low - pad, base + th + pad);
    let frame = vec![[x, y0], [x + w, y0], [x + w, y1], [x, y1]];
    // the frame is title furniture under the view, past its crop; the values stay level marks
    pen.line(frame, true, 0.25, TITLES);
    for (k, s) in lines.iter().enumerate() {
        pen.text([x + pad, base - MARK_STEP * k as f64], th, 0.0, s, LEVELS);
    }
}

/// What a plan shows beside its title: the floor levels it stands for and, on a typical
/// floor plan, the note that reads its room numbers back to every floor's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanMarks {
    pub levels: Vec<f64>,
    pub note: Option<String>,
}

/// The level mark of a plan and its room number note, under the title and the mark.
pub(super) fn plan_annotations(
    pen: &mut Pen,
    rect: [f64; 4],
    standard: &str,
    title: &str,
    pm: &PlanMarks,
) {
    plan_level_mark(pen, rect, standard, title, &pm.levels);
    let Some(note) = &pm.note else {
        return;
    };
    let (x, cy) = title_line(rect);
    let lines = pm.levels.len().clamp(1, MARK_LINES);
    let mark_low = cy + TITLE_LIFT - MARK_STEP * (lines - 1) as f64 - MARK_PAD;
    let scale_low = cy - TITLE_LIFT - SCALE_H;
    let y = scale_low.min(mark_low) - NOTE_GAP - SCALE_H;
    pen.text([x, y], SCALE_H, 0.0, note, TITLES);
}

/// Gap above a plan's room number note, mm.
const NOTE_GAP: f64 = 2.0;

/// Floor level lines a typical floor plan's mark shows at most.
pub(super) const MARK_LINES: usize = 6;
/// Level mark frame, mm: padding, line pitch, gap after the view title.
const MARK_PAD: f64 = 1.0;
const MARK_STEP: f64 = 5.0;
const MARK_GAP: f64 = 8.0;

/// Floor levels a plan of `level` marks: every level of a typical floor sheet (field
/// `dset_levels`) when the plan is one of them, else the level's own.
fn plan_marks(s: &SheetDef, level: i64, levels: &[(i64, String, f64)]) -> Vec<f64> {
    let z = |id: i64| levels.iter().find(|l| l.0 == id).map(|l| l.2);
    match typical_of(s, level) {
        Some(t) => t.into_iter().filter_map(z).collect(),
        None => z(level).into_iter().collect(),
    }
}

/// The levels of the typical floor sheet `s` when its plan of `level` is one of them.
fn typical_of(s: &SheetDef, level: i64) -> Option<Vec<i64>> {
    let t: Vec<i64> = s
        .fields
        .get(ddc_model::DSET_LEVELS)
        .map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_default();
    (t.len() > 1 && t.contains(&level)).then_some(t)
}

/// Room numbers of a typical floor plan `d`: the part every floor shares and the note
/// (`Ok`), or, when the floors no longer number alike, blank numbers and a warning (`Err`;
/// regenerating the set splits such floors).
fn typical_room_numbers(
    m: &Model,
    v: &ViewportDef,
    typical: &[i64],
    levels: &[(i64, String, f64)],
    d: &mut PlanView,
    ctx: &Ctx,
) -> Result<Result<Option<String>, String>, SheetError> {
    use super::dset_rooms::{room_note, room_numbers, show_rooms, typical_rooms};
    let ViewRef::Plan { cut, .. } = v.view else {
        return Ok(Ok(None));
    };
    let mut floors = vec![];
    for &level in typical {
        let f = ViewportDef {
            view: ViewRef::Plan { level, cut },
            ..v.clone()
        };
        floors.push(room_numbers(&view_drawing(m, &f)?));
    }
    let refs: Vec<_> = floors.iter().collect();
    let name = |id: i64| {
        levels
            .iter()
            .find(|l| l.0 == id)
            .map_or("", |l| l.1.as_str())
    };
    match typical_rooms(&refs) {
        Some(s) => {
            show_rooms(d, &s.marks);
            let (a, b) = (typical[0], typical[typical.len() - 1]);
            Ok(Ok(room_note(ctx.text, &s, name(a), name(b))))
        }
        None => {
            let own = room_numbers(d);
            let blank = own.into_keys().map(|k| (k, String::new())).collect();
            show_rooms(d, &blank);
            Ok(Err((ctx.text)("dset.warn.typical_rooms")))
        }
    }
}

/// Viewport rectangle `rect` reaching down to the lowest mark of view `k`: the title goes
/// below everything the view draws (section heads beyond the dimension chains included).
pub(super) fn title_rect(marks: &[(Option<usize>, Mark)], k: usize, rect: [f64; 4]) -> [f64; 4] {
    let low = marks
        .iter()
        .filter(|(v, _)| *v == Some(k))
        .map(|(_, m)| super::dset::mark_box(m)[1])
        .fold(rect[1], f64::min);
    [rect[0], low, rect[2], rect[3] + rect[1] - low]
}

/// Area unit in the language of the sheet's words.
/// The project's logo for the title block; a stored picture that no longer decodes is skipped.
pub(super) fn project_logo(m: &Model) -> Option<crate::logo::Logo> {
    let bytes = m.project_logo().ok()??;
    crate::logo::Logo::from_png(&bytes).ok()
}

pub(super) fn area_unit(ctx: &Ctx) -> String {
    (ctx.text)("sheets.paper.area_unit")
}

/// The whole model sheet `id`: viewports, captions, revisions, schedules, title-block auto fields.
pub fn compose_sheet(m: &Model, id: i64, ctx: &Ctx) -> Result<Composed, SheetError> {
    let s = m.sheet(id).map_err(me)?;
    let vps = m.viewports(id).map_err(me)?;
    let levels = m.level_list().map_err(me)?;
    let mut views = vec![];
    let mut notes = vec![];
    let mut warnings = vec![];
    for (_, v) in &vps {
        let mut d = view_drawing(m, v)?;
        d.bbox = v.crop.unwrap_or_else(|| extent(&d));
        let typical = match v.view {
            ViewRef::Plan { level, .. } => typical_of(&s, level),
            ViewRef::View(_) => None,
        };
        let note = match typical {
            Some(t) => match typical_room_numbers(m, v, &t, &levels, &mut d, ctx)? {
                Ok(n) => n,
                Err(w) => {
                    warnings.push(w);
                    None
                }
            },
            None => None,
        };
        super::dset_area::localize_finish(&mut d, ctx.text);
        views.push(d);
        notes.push(note);
    }
    let rects: Vec<[f64; 4]> = vps
        .iter()
        .zip(&views)
        .map(|((_, v), d)| rect_of(v, d.bbox))
        .collect();
    let spec = SheetSpec {
        paper: s.paper.clone(),
        landscape: s.landscape,
        standard: s.standard.clone(),
        fields: auto_fields(m, id, &s, &vps, ctx)?,
        viewports: views
            .iter()
            .zip(&vps)
            .zip(&rects)
            .map(|((d, (_, v)), r)| Viewport {
                view: d,
                scale: v.scale,
                rect: Some(*r),
                title: String::new(),
            })
            .collect(),
        units: ctx.units,
        area_unit: Some(area_unit(ctx)),
        logo: project_logo(m),
    };
    let styles: Vec<Option<ViewStyle>> = vps
        .iter()
        .map(|(_, v)| ViewStyle::of(m, &v.view).map(|s| Some(s.with_scale(v.scale))))
        .collect::<Result<_, _>>()
        .map_err(me)?;
    let mut dr = compose_styled(&spec, &styles)?;
    let mut placed = vec![];
    let own;
    let refs = match ctx.refs {
        Some(r) => r,
        None => {
            let plans = vps.iter().any(|v| matches!(v.1.view, ViewRef::Plan { .. }));
            own = if plans {
                SheetRefs::load(m)?
            } else {
                SheetRefs::default()
            };
            &own
        }
    };
    let details = m.viewport_details(id).map_err(me)?;
    for (k, ((vid, v), d)) in vps.iter().zip(&views).enumerate() {
        // marks go before cropping: whatever does not fit the viewport is clipped together with the view
        section_marks(
            &mut dr,
            k,
            v,
            &s.standard,
            &refs.defs,
            &refs.placements,
            rects[k],
        )?;
        if v.crop.is_some() {
            dr.clip(k, rects[k]);
        }
        let below = title_rect(&dr.marks, k, rects[k]);
        let mut pen = Pen {
            out: &mut dr.marks,
            vp: Some(k),
        };
        let detail = details
            .iter()
            .find(|x| x.0 == *vid)
            .map_or("", |x| x.1.as_str());
        view_title(&mut pen, below, detail, &v.title, v.scale);
        if let ViewRef::Plan { level, .. } = v.view {
            let pm = PlanMarks {
                levels: plan_marks(&s, level, &levels),
                note: notes[k].clone(),
            };
            plan_annotations(&mut pen, below, &s.standard, &v.title, &pm);
        }
        let b = d.bbox;
        placed.push(Placed {
            kind: PlacedKind::Viewport(*vid),
            rect: rects[k],
            model_c: [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0],
            scale: v.scale,
        });
    }
    let revs = m.revisions(id).map_err(me)?;
    let d = data();
    let st = &d["standards"][s.standard.as_str()];
    let fr = &st["frame"];
    // a schedule does not overlap the title block and the revision table above it
    let (frame_left, frame_bottom) = (num(fr, "left"), num(fr, "bottom"));
    let right = dr.width - num(fr, "right");
    let stamp_top = frame_bottom + num(&st["stamp"], "h");
    let mut room_top = stamp_top;
    let mut stamp_left = right - num(&st["stamp"], "w");
    if !revs.is_empty() {
        let t = Table {
            title: (ctx.text)("sheets.paper.revisions"),
            header: ["rev_mark", "rev_date", "rev_desc", "rev_by"]
                .map(|k| (ctx.text)(&format!("sheets.paper.{k}")))
                .to_vec(),
            rows: revs
                .iter()
                .map(|r| {
                    vec![
                        r.mark.clone(),
                        r.date.clone(),
                        r.description.clone(),
                        r.author.clone(),
                    ]
                })
                .collect(),
            swatches: vec![],
        };
        let base = stamp_top + 2.0;
        let max_h = dr.height - num(fr, "top") - base;
        let l = Layout::new(&t, &t.title, 2.5, right - frame_left, max_h);
        // not all revisions fit the frame: the latest stay, the rest are reported in a message
        let mut used = l.head_h();
        let mut start = t.rows.len();
        while start > 0 && used + l.heights[start - 1] <= max_h {
            used += l.heights[start - 1];
            start -= 1;
        }
        if start > 0 {
            warnings.push(fill(ctx, "warn_revisions", "", start));
        }
        if l.clipped > 0 {
            warnings.push(fill(ctx, "warn_clipped", &t.title, l.clipped));
        }
        room_top = base + used;
        let tw = l.width();
        stamp_left = stamp_left.min(right - tw);
        let mut pen = Pen {
            out: &mut dr.marks,
            vp: None,
        };
        let all = start..t.rows.len();
        l.part(&mut pen, [right - tw, room_top], &[], all, &t.title, STAMP);
    }
    let room = Room {
        left: 0.0,
        top: dr.height,
        right,
        bottom: frame_bottom,
        stamp: [stamp_left, room_top],
    };
    for (pid, p) in m.schedule_places(id).map_err(me)? {
        // «doors@0-24»: rows 0..24 of the doors schedule, a part split across sheets
        let (key, rows) = super::dset::page_key(&p.schedule);
        let t = match builtin_table(m, key, &s, &vps, ctx)? {
            Some(t) => Some(t),
            None => (ctx.schedules)(key),
        }
        .map(|t| super::dset::page_table(t, rows, ctx.text))
        .unwrap_or_else(|| Table {
            title: p.schedule.clone(),
            header: vec![],
            rows: vec![vec![(ctx.text)("sheets.paper.no_data")]],
            swatches: vec![],
        });
        let sp = m.schedule_split(pid).map_err(me)?;
        let mut pen = Pen {
            out: &mut dr.marks,
            vp: None,
        };
        let drawn = draw_split(
            &mut pen,
            [p.pos.0, p.pos.1],
            &t,
            2.5,
            SCHED,
            sp.max_h,
            sp.gap,
            &room,
            ctx.text,
        );
        if drawn.hidden > 0 {
            warnings.push(fill(ctx, "warn_hidden", &t.title, drawn.hidden));
        }
        if drawn.clipped > 0 {
            warnings.push(fill(ctx, "warn_clipped", &t.title, drawn.clipped));
        }
        placed.push(Placed {
            kind: PlacedKind::Schedule(pid),
            rect: drawn.rect,
            model_c: [0.0, 0.0],
            scale: 1.0,
        });
    }
    for lp in m.legend_places(id).map_err(me)? {
        let view = m.legend_view(lp.legend).map_err(me)?;
        let scale = m.anno_scale(AnnoView::Legend(lp.legend)).map_err(me)?;
        let mut pen = Pen {
            out: &mut dr.marks,
            vp: None,
        };
        super::anno::draw_view(
            &mut pen,
            &view,
            lp.pos,
            scale,
            &super::anno::anno_fmt(&s.standard),
        );
        let (b, k) = (view.bbox, 1000.0 / scale);
        placed.push(Placed {
            kind: PlacedKind::Legend(lp.id),
            rect: [
                lp.pos.0 + b[0] * k,
                lp.pos.1 + b[1] * k,
                (b[2] - b[0]) * k,
                (b[3] - b[1]) * k,
            ],
            model_c: [0.0, 0.0],
            scale: 1.0,
        });
    }
    Ok(Composed {
        drawing: dr,
        placed,
        warnings,
    })
}

/// Build message from the catalog: `{title}` — the table, `{n}` — how many rows.
fn fill(ctx: &Ctx, key: &str, title: &str, n: usize) -> String {
    (ctx.text)(&format!("sheets.paper.{key}"))
        .replace("{title}", title)
        .replace("{n}", &n.to_string())
}

/// Lower limit of line weight when fitting to a smaller format, mm.
const MIN_PRINT_WEIGHT: f64 = 0.1;

impl SheetDrawing {
    /// Fit the sheet into a `w`×`h` mm format: coordinates, weights and text heights share one factor.
    pub fn fit_to(&mut self, w: f64, h: f64) {
        let f = (w / self.width).min(h / self.height);
        let (dx, dy) = ((w - self.width * f) / 2.0, (h - self.height * f) / 2.0);
        let tf = |p: &mut [f64; 2]| *p = [p[0] * f + dx, p[1] * f + dy];
        for (_, m) in &mut self.marks {
            match m {
                Mark::Line { pts, weight, .. } => {
                    pts.iter_mut().for_each(tf);
                    // reduced print: a line thinner than MIN_PRINT_WEIGHT does not print
                    *weight = (*weight * f).max(weight.min(MIN_PRINT_WEIGHT));
                }
                Mark::Text { pos, h, .. } => {
                    tf(pos);
                    *h *= f;
                }
                Mark::Fill { rings, .. } => rings.iter_mut().flatten().for_each(tf),
            }
        }
        for t in &mut self.transforms {
            tf(&mut t.0);
            t.2 /= f;
        }
        if let Some(lg) = &mut self.logo {
            let mut o = [lg.rect[0], lg.rect[1]];
            tf(&mut o);
            lg.rect = [o[0], o[1], lg.rect[2] * f, lg.rect[3] * f];
        }
        (self.width, self.height) = (w, h);
    }
}

/// A sheet for printing with build messages: if another format is given, the drawing is fitted to it
/// and `placed` stays in sheet coordinates.
pub fn print_composed(m: &Model, id: i64, ctx: &Ctx) -> Result<Composed, SheetError> {
    let mut c = compose_sheet(m, id, ctx)?;
    if let Some((p, landscape)) = ctx.paper {
        let (w, h) = super::paper_size(p, landscape)?;
        c.drawing.fit_to(w, h);
    }
    Ok(c)
}

/// A sheet for printing: build and, if another format is given, fitting to it.
pub fn print_sheet(m: &Model, id: i64, ctx: &Ctx) -> Result<SheetDrawing, SheetError> {
    Ok(print_composed(m, id, ctx)?.drawing)
}

/// A sheet set in order → one multi-page PDF (a page per sheet, "Sheet N" numbered within the set).
/// Interactive printing goes through `PdfBook` to a file; here it is the same path into memory.
pub fn export_sheets_pdf(m: &Model, ids: &[i64], ctx: &Ctx) -> Result<Vec<u8>, SheetError> {
    if ids.is_empty() {
        return err("no sheets to print");
    }
    let refs = SheetRefs::load(m)?;
    let bad = |e: pdf::PdfError| SheetError(e.to_string());
    let mut book = pdf::PdfBook::new(Vec::new()).map_err(bad)?;
    for (k, id) in ids.iter().enumerate() {
        let c = Ctx {
            index: Some((k + 1, ids.len())),
            refs: Some(&refs),
            ..*ctx
        };
        let page = print_sheet(m, *id, &c)?.to_pdf_sheet();
        book.add_page(&page).map_err(bad)?;
    }
    book.finish().map_err(bad)
}

/// Pages → PDF bytes.
pub fn to_pdf(pages: &[pdf::Sheet]) -> Result<Vec<u8>, SheetError> {
    pdf::export_pdf(pages).map_err(|e| SheetError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::import::PdfFile;
    use crate::ru_fixtures::ru;
    use crate::sheets::tests_output::ru_text;
    use ddc_model::{
        AnnoShape, CatGfx, GfxOverride, LayerSpec, Leader, LegendRow, LegendSwatch, OpeningSpec,
        RegionFill, Revision, SchedulePlace, SectionSpec, SpotKind, Swing, TextNote, ViewKey,
        ViewKind, WallSpec, DEFAULT_CUT, PAPERS,
    };

    fn house() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.25,
                }],
            )
            .unwrap();
        let c = [(0.0, 0.0), (8.0, 0.0), (8.0, 5.0), (0.0, 5.0)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|k| WallSpec {
                a: c[k],
                b: c[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        m.add_opening_with(
            ids[0],
            &OpeningSpec {
                along: 2.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
            Swing::default(),
        )
        .unwrap();
        let sec = m
            .add_view_def(&SectionSpec {
                kind: ViewKind::Section,
                name: "1-1".into(),
                a: (4.0, -1.0),
                b: (4.0, 6.0),
                depth: 10.0,
                far_clip: false,
            })
            .unwrap();
        (m, lv, sec)
    }

    fn doors(key: &str) -> Option<Table> {
        (key == "doors").then(|| Table {
            title: ru("doors_schedule").into(),
            header: vec!["№".into(), ru("width_mm").into()],
            rows: vec![vec!["1".into(), "900".into()]],
            swatches: vec![],
        })
    }

    fn ctx(f: &dyn Fn(&str) -> Option<Table>) -> Ctx<'_> {
        Ctx {
            units: LengthUnits::Metric,
            schedules: f,
            index: None,
            paper: None,
            text: &ru_text,
            refs: None,
        }
    }

    fn vp(sheet: i64, view: ViewRef, scale: f64, center: (f64, f64)) -> ViewportDef {
        ViewportDef {
            sheet,
            view,
            scale,
            center,
            crop: None,
            title: ru("plan_floor1").into(),
        }
    }

    /// View hiding and overrides reach the sheet, PDF and DXF.
    #[test]
    fn view_graphics_reach_the_sheet() {
        let (mut m, lv, sec) = house();
        let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
        let plan = ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        };
        m.add_viewport(&vp(a, plan, 50.0, (150.0, 170.0))).unwrap();
        m.add_viewport(&vp(a, ViewRef::View(sec), 100.0, (330.0, 200.0)))
            .unwrap();
        let cut_marks = |d: &SheetDrawing, k: usize| {
            d.marks
                .iter()
                .filter(|(v, mk)| {
                    *v == Some(k)
                        && matches!(mk, Mark::Line { layer, .. } if *layer == super::super::CUT)
                })
                .count()
        };
        let none: &dyn Fn(&str) -> Option<Table> = &|_| None;
        let before = compose_sheet(&m, a, &ctx(none)).unwrap().drawing;
        assert!(cut_marks(&before, 1) > 0, "the section cuts walls");
        assert!(before
            .marks
            .iter()
            .all(|(_, mk)| !matches!(mk, Mark::Line { rgb: Some(_), .. })));

        m.edit_view_gfx(ViewKey::Plan(lv), |g| {
            g.categories.insert(
                "wall".into(),
                CatGfx {
                    visible: true,
                    over: GfxOverride {
                        line: Some(0xFF0000),
                        line_weight: Some(4.0),
                        fill: Some(0x00FF00),
                        transparency: Some(50),
                    },
                },
            );
        })
        .unwrap();
        m.set_view_category_visible(ViewKey::Section(sec), "wall", false)
            .unwrap();
        let after = compose_sheet(&m, a, &ctx(none)).unwrap().drawing;

        assert_eq!(cut_marks(&after, 1), 0, "hidden walls leave the sheet");
        let red_cut = after.marks.iter().any(|(v, mk)| {
            *v == Some(0)
                && matches!(mk, Mark::Line { rgb: Some([255, 0, 0]), weight, layer, .. }
                    if *layer == super::super::CUT && (*weight - 1.0).abs() < 1e-9)
        });
        assert!(red_cut, "line colour and weight override");
        let pdf_sheet = after.to_pdf_sheet();
        assert!(pdf_sheet.items.iter().any(|i| matches!(
            i,
            pdf::Item::Polyline {
                rgb: [255, 0, 0],
                ..
            }
        )));
        assert!(pdf_sheet.items.iter().any(|i| matches!(
            i,
            pdf::Item::Fill {
                rgb: [128, 255, 128],
                ..
            }
        )));
    }

    #[test]
    fn model_papers_are_known() {
        for p in PAPERS {
            assert!(super::super::paper_size(p, true).is_ok(), "{p}");
        }
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            file_name("{number} - {name}", "A-101", ru("plan_colon"), "pdf"),
            ru("plan_file_name")
        );
        assert_eq!(file_name("{name}", "", " . ", "dxf"), "sheet.dxf");
        assert_eq!(file_name("{name}", "", "con", "pdf"), "_con.pdf");
        assert_eq!(
            file_name("{name}", "", "LPT1.plan", "pdf"),
            "_LPT1.plan.pdf"
        );
        assert_eq!(file_name("{name}", "", "COM10", "pdf"), "COM10.pdf");
        let long = file_name("{name}", "", &ru("zhe_lc").repeat(1000), "pdf");
        assert!(long.chars().count() <= MAX_NAME + 4, "{long}");
        assert!(long.ends_with(".pdf"));
        let mut used = std::collections::HashSet::new();
        let a = file_name("{number}", "A/1", "", "pdf");
        let b = file_name("{number}", "A_1", "", "pdf");
        assert_eq!(unique_name(a, &mut used), "A_1.pdf");
        assert_eq!(unique_name(b, &mut used), "A_1-2.pdf");
        assert_eq!(unique_name("a_1.pdf".into(), &mut used), "a_1-3.pdf");
    }

    #[test]
    fn clip_cuts_lines_fills_and_texts_to_the_viewport() {
        let mut d = SheetDrawing {
            width: 100.0,
            height: 100.0,
            marks: vec![
                (
                    Some(0),
                    Mark::Line {
                        pts: vec![[0.0, 50.0], [100.0, 50.0]],
                        closed: false,
                        weight: 0.5,
                        rgb: None,
                        layer: 2,
                    },
                ),
                (
                    Some(0),
                    Mark::Fill {
                        rings: vec![vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]],
                        gray: 0,
                        rgb: None,
                        layer: 2,
                    },
                ),
                (
                    Some(0),
                    Mark::Text {
                        pos: [5.0, 5.0],
                        h: 2.5,
                        rot: 0.0,
                        s: "out".into(),
                        layer: 5,
                    },
                ),
                (
                    None,
                    Mark::Text {
                        pos: [5.0, 5.0],
                        h: 2.5,
                        rot: 0.0,
                        s: "frame".into(),
                        layer: 1,
                    },
                ),
            ],
            transforms: vec![([50.0, 50.0], [0.0, 0.0], 0.1)],
            logo: None,
        };
        d.clip(0, [20.0, 20.0, 60.0, 60.0]);
        assert_eq!(d.marks.len(), 3);
        let Mark::Line { pts, .. } = &d.marks[0].1 else {
            panic!()
        };
        assert_eq!(pts.len(), 2);
        assert!(
            (pts[0][0] - 20.0).abs() < 1e-9 && (pts[1][0] - 80.0).abs() < 1e-9,
            "{pts:?}"
        );
        let Mark::Fill { rings, .. } = &d.marks[1].1 else {
            panic!()
        };
        for p in &rings[0] {
            assert!(inside(*p, [20.0, 20.0, 60.0, 60.0]), "{p:?}");
        }
        assert!(matches!(&d.marks[2], (None, Mark::Text { s, .. }) if s == "frame"));
    }

    #[test]
    fn sheet_set_prints_to_multipage_vector_pdf() {
        let (mut m, lv, sec) = house();
        let mut s1 = SheetDef::new("A-101", ru("plan_floor1"));
        s1.standard = "gost".into();
        s1.fields.insert("designation".into(), ru("designation_code").into());
        let a = m.add_sheet(&s1).unwrap();
        let b = m.add_sheet(&SheetDef::new("A-102", ru("site_plan"))).unwrap();
        let plan = ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        };
        let mut cropped = vp(a, plan, 50.0, (150.0, 170.0));
        cropped.crop = Some([-0.5, -0.5, 4.0, 5.5]);
        m.add_viewport(&cropped).unwrap();
        m.add_viewport(&vp(a, ViewRef::View(sec), 100.0, (330.0, 200.0)))
            .unwrap();
        m.set_revisions(
            a,
            &[Revision {
                mark: "1".into(),
                date: "04.11.26".into(),
                description: ru("issue_word").into(),
                author: ru("initials_ai").into(),
            }],
        )
        .unwrap();
        m.add_viewport(&vp(b, plan, 200.0, (150.0, 170.0))).unwrap();
        m.add_schedule_place(&SchedulePlace {
            sheet: b,
            schedule: "doors".into(),
            pos: (300.0, 280.0),
        })
        .unwrap();

        let f = doors;
        let c = ctx(&f);
        // preview: viewports and schedule are placed, cropping keeps the view in the frame
        let ca = compose_sheet(&m, a, &c).unwrap();
        assert_eq!(ca.placed.len(), 2);
        let r = ca.placed[0].rect;
        assert!(
            (r[2] - 90.0).abs() < 1e-9 && (r[3] - 120.0).abs() < 1e-9,
            "{r:?}"
        );
        for (vp, mk) in &ca.drawing.marks {
            if let (Some(0), Mark::Line { pts, layer, .. }) = (vp, mk) {
                if *layer != TITLES {
                    assert!(pts
                        .iter()
                        .all(|p| inside(*p, [r[0] - 1e-6, r[1] - 1e-6, r[2] + 2e-6, r[3] + 2e-6])));
                }
            }
        }
        let cb = compose_sheet(&m, b, &c).unwrap();
        assert!(matches!(cb.placed[1].kind, PlacedKind::Schedule(_)));

        let bytes = export_sheets_pdf(&m, &[a, b], &c).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        let pdf = PdfFile::parse(&bytes).unwrap();
        assert_eq!(pdf.page_count(), 2);
        let (p1, p2) = (pdf.page(0).unwrap(), pdf.page(1).unwrap());
        assert_eq!((p1.width_mm.round(), p1.height_mm.round()), (420.0, 297.0));
        let texts = |p: &crate::pdf::import::PageDrawing| -> Vec<String> {
            p.texts.iter().map(|t| t.string.clone()).collect()
        };
        let (t1, t2) = (texts(&p1), texts(&p2));
        // title block: auto fields "Sheet 1 of 2", sheet name, scale "as indicated", revisions above the block
        for want in [
            ru("designation_code"),
            ru("plan_floor1"),
            ru("changes_word"),
            ru("issue_word"),
            "1:50",
            "1:100",
        ] {
            assert!(t1.iter().any(|t| t == want), "{want} in {t1:?}");
        }
        assert!(
            t1.iter().any(|t| t.starts_with(ru("as_word"))),
            "as indicated: {t1:?}"
        );
        assert!(t1.iter().any(|t| t == "2"), "sheets total");
        for want in [ru("doors_schedule"), "900", "A-102", "2/2", "1:200"] {
            assert!(t2.iter().any(|t| t == want), "{want} in {t2:?}");
        }
        // 1:50 — concrete hatching and a heavy section line; 1:200 — solid section fill
        let fills =
            |p: &crate::pdf::import::PageDrawing| p.shapes.iter().filter(|s| s.fill).count();
        assert_eq!(fills(&p1), 0);
        assert!(fills(&p2) >= 4, "{}", fills(&p2));
        let widths: Vec<f64> = p1
            .shapes
            .iter()
            .filter(|s| s.stroke)
            .map(|s| s.width_mm)
            .collect();
        let near = |w: f64| widths.iter().any(|x| (x - w).abs() < 1e-3);
        assert!(
            near(0.8) && near(0.2),
            "gost 1:50 cut and hatch weights: {widths:?}"
        );
        assert!(p1.shapes.iter().filter(|s| s.stroke).count() > 100);
        assert!(export_sheets_pdf(&m, &[], &c).is_err());
        // printing A3 onto A4: an A4 page, content fitted
        let a4 = Ctx {
            paper: Some(("A4", true)),
            ..c
        };
        let d = print_sheet(&m, b, &a4).unwrap();
        assert_eq!((d.width, d.height), (297.0, 210.0));
        for (_, mk) in &d.marks {
            if let Mark::Line { pts, .. } = mk {
                assert!(pts.iter().all(|p| (-1e-6..=297.0 + 1e-6).contains(&p[0])));
            }
        }
        let one = PdfFile::parse(&export_sheets_pdf(&m, &[b], &a4).unwrap()).unwrap();
        assert_eq!(one.page_count(), 1);
    }

    #[test]
    fn shared_refs_give_the_same_marks_as_reading_them_per_sheet() {
        let (mut m, lv, sec) = house();
        let a = m.add_sheet(&SheetDef::new("A-101", ru("plan_cap"))).unwrap();
        let b = m.add_sheet(&SheetDef::new("A-102", ru("section_word"))).unwrap();
        let plan = ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        };
        m.add_viewport(&vp(a, plan, 100.0, (150.0, 170.0))).unwrap();
        m.add_viewport(&vp(b, ViewRef::View(sec), 100.0, (150.0, 170.0)))
            .unwrap();
        let f = doors;
        let own = compose_sheet(&m, a, &ctx(&f)).unwrap();
        let refs = SheetRefs::load(&m).unwrap();
        let shared = Ctx {
            refs: Some(&refs),
            ..ctx(&f)
        };
        let got = compose_sheet(&m, a, &shared).unwrap();
        assert_eq!(own.drawing.marks, got.drawing.marks);
    }

    #[test]
    fn revisions_that_do_not_fit_keep_the_newest_and_tell_the_user() {
        let (mut m, _, _) = house();
        let a = m.add_sheet(&SheetDef::new("A-101", ru("sheet_word"))).unwrap();
        let revs: Vec<Revision> = (0..300)
            .map(|k| Revision {
                mark: format!("rev{k}"),
                date: "01.01.26".into(),
                description: format!("{} ", ru("change_desc")).repeat(12),
                author: ru("initials_ai").into(),
            })
            .collect();
        m.set_revisions(a, &revs).unwrap();
        let f = doors;
        let c = compose_sheet(&m, a, &ctx(&f)).unwrap();
        let texts: Vec<&str> = c
            .drawing
            .marks
            .iter()
            .filter_map(|(_, mk)| match mk {
                Mark::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"rev299"));
        assert!(!texts.contains(&"rev0"));
        assert!(
            c.warnings.iter().any(|w| w.contains(ru("revisions_gen"))),
            "{:?}",
            c.warnings
        );
        for (_, mk) in &c.drawing.marks {
            if let Mark::Line { pts, .. } = mk {
                assert!(pts.iter().all(|p| p[1] <= c.drawing.height + 1e-6));
            }
        }
    }

    #[test]
    fn deleted_view_leaves_sheet_printable() {
        let (mut m, _, sec) = house();
        let a = m.add_sheet(&SheetDef::new("A-101", ru("sections_word"))).unwrap();
        m.add_viewport(&vp(a, ViewRef::View(sec), 50.0, (200.0, 150.0)))
            .unwrap();
        let f = doors;
        assert_eq!(compose_sheet(&m, a, &ctx(&f)).unwrap().placed.len(), 1);
        m.delete_view_def(sec).unwrap();
        assert!(compose_sheet(&m, a, &ctx(&f)).unwrap().placed.is_empty());
        m.undo().unwrap();
        assert_eq!(compose_sheet(&m, a, &ctx(&f)).unwrap().placed.len(), 1);
    }

    fn marks_on(d: &SheetDrawing, layer: usize) -> usize {
        d.marks
            .iter()
            .filter(|(_, mk)| match mk {
                Mark::Line { layer: l, .. }
                | Mark::Text { layer: l, .. }
                | Mark::Fill { layer: l, .. } => *l == layer,
            })
            .count()
    }

    fn dxf_on(d: &SheetDrawing, layer: &str) -> usize {
        let back = crate::dxf::read_str(&crate::dxf::write_string(&d.to_dxf())).unwrap();
        // viewport marks go in the model, sheet marks in the sheet block
        back.entities
            .iter()
            .chain(back.blocks.iter().flat_map(|b| b.entities.iter()))
            .filter(|e| match e {
                crate::dxf::Entity::Line(x) => x.common.layer == layer,
                crate::dxf::Entity::LwPolyline(x) => x.common.layer == layer,
                crate::dxf::Entity::Text(x) => x.common.layer == layer,
                _ => false,
            })
            .count()
    }

    #[test]
    fn annotations_reach_plan_and_section_on_sheet_pdf_and_dxf() {
        let (mut m, lv, sec) = house();
        let mut note = TextNote::new((2.0, 2.0), ru("note_word"));
        note.leaders.push(Leader {
            to: (4.0, 4.0),
            elbow: None,
        });
        let dashed = m
            .line_styles()
            .unwrap()
            .into_iter()
            .find(|s| !s.dash.is_empty())
            .unwrap()
            .id;
        let plan = AnnoView::Plan(lv);
        m.add_anno(plan, &AnnoShape::Text(note), None, None)
            .unwrap();
        m.add_anno(
            plan,
            &AnnoShape::Line {
                pts: vec![(1.0, 1.0), (4.0, 1.0)],
                closed: false,
            },
            None,
            Some(dashed),
        )
        .unwrap();
        m.add_anno(
            plan,
            &AnnoShape::Region {
                rings: vec![vec![(1.0, 2.0), (3.0, 2.0), (3.0, 4.0), (1.0, 4.0)]],
                fill: RegionFill::Hatch {
                    deg: 45.0,
                    step_mm: 2.0,
                    cross: true,
                },
                boundary: true,
            },
            None,
            None,
        )
        .unwrap();
        m.add_anno(
            plan,
            &AnnoShape::Spot {
                kind: SpotKind::Elevation,
                at: (5.0, 2.0),
                head: (6.0, 3.0),
            },
            None,
            None,
        )
        .unwrap();
        m.add_anno(
            plan,
            &AnnoShape::Cloud {
                ring: vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0)],
                mark: ru("grid_b").into(),
            },
            None,
            None,
        )
        .unwrap();
        m.add_anno(
            AnnoView::Section(sec),
            &AnnoShape::Text(TextNote::new((1.0, 2.0), ru("detail_on_section"))),
            None,
            None,
        )
        .unwrap();
        let a = m
            .add_sheet(&SheetDef::new("A-101", ru("plan_and_section")))
            .unwrap();
        let cut = ViewRef::Plan {
            level: lv,
            cut: DEFAULT_CUT,
        };
        m.add_viewport(&vp(a, cut, 100.0, (120.0, 150.0))).unwrap();
        m.add_viewport(&vp(a, ViewRef::View(sec), 100.0, (300.0, 150.0)))
            .unwrap();
        let f = doors;
        let c = ctx(&f);
        let d = compose_sheet(&m, a, &c).unwrap().drawing;
        assert!(marks_on(&d, super::super::DETAIL) > 10);
        assert!(marks_on(&d, super::super::NOTES) >= 3);
        assert!(dxf_on(&d, "Detail") > 10);
        assert!(dxf_on(&d, "Notes") >= 3);
        let pdf = PdfFile::parse(&export_sheets_pdf(&m, &[a], &c).unwrap()).unwrap();
        let page = pdf.page(0).unwrap();
        let all: Vec<&str> = page.texts.iter().map(|t| t.string.as_str()).collect();
        for want in [ru("note_word"), ru("detail_on_section"), "±0", ru("grid_b")] {
            assert!(all.iter().any(|t| t.contains(want)), "{want}: {all:?}");
        }
    }

    #[test]
    fn annotation_text_height_is_paper_size_at_any_viewport_scale() {
        let (mut m, lv, _) = house();
        m.add_anno(
            AnnoView::Plan(lv),
            &AnnoShape::Text(TextNote::new((2.0, 2.0), ru("cyr_en"))),
            None,
            None,
        )
        .unwrap();
        let f = doors;
        let height = |m: &mut Model, scale: f64| {
            let a = m
                .add_sheet(&SheetDef::new(&format!("S-{scale}"), ru("cyr_el")))
                .unwrap();
            let cut = ViewRef::Plan {
                level: lv,
                cut: DEFAULT_CUT,
            };
            m.add_viewport(&vp(a, cut, scale, (200.0, 150.0))).unwrap();
            let d = compose_sheet(m, a, &ctx(&f)).unwrap().drawing;
            d.marks
                .iter()
                .find_map(|(_, mk)| match mk {
                    Mark::Text { h, layer, .. } if *layer == super::super::NOTES => Some(*h),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(height(&mut m, 50.0), height(&mut m, 200.0));
    }

    #[test]
    fn annotation_format_and_legend_position_come_from_the_standard() {
        use super::super::{anno_fmt, legend_default_pos, paper_size};
        let (iso, gost) = (anno_fmt("iso"), anno_fmt("gost"));
        assert_eq!(
            (iso.sep.as_str(), iso.north.as_str(), iso.east.as_str()),
            (".", "N", "E")
        );
        assert_eq!(
            (gost.sep.as_str(), gost.north.as_str(), gost.east.as_str()),
            (",", ru("cyr_north"), ru("grid_v"))
        );
        let (_, h) = paper_size("A3", true).unwrap();
        let (x, y) = legend_default_pos("A3", true, "iso").unwrap();
        assert!(x > 0.0 && y > 0.0 && y < h);
    }

    #[test]
    fn legend_is_drawn_on_sheet_and_leaves_with_it() {
        let (mut m, _, _) = house();
        let line = m.line_styles().unwrap()[1].id;
        let leg = m.add_legend(ru("legend_word"), 100.0).unwrap();
        m.set_legend_rows(
            leg,
            &[
                LegendRow {
                    swatch: LegendSwatch::Line(line),
                    label: ru("boundary_word").into(),
                },
                LegendRow {
                    swatch: LegendSwatch::Region(RegionFill::Solid { gray: 128 }),
                    label: ru("demolition_word").into(),
                },
            ],
        )
        .unwrap();
        let a = m.add_sheet(&SheetDef::new("A-102", ru("legend_sheet"))).unwrap();
        m.place_legend(a, leg, (30.0, 250.0)).unwrap();
        let f = doors;
        let c = ctx(&f);
        let comp = compose_sheet(&m, a, &c).unwrap();
        let p = comp
            .placed
            .iter()
            .find(|p| matches!(p.kind, PlacedKind::Legend(_)))
            .expect("legend can be selected on the sheet");
        assert!((p.rect[0] - 30.0).abs() < 1e-9 && p.rect[2] > 0.0);
        assert!((p.rect[1] + p.rect[3] - 250.0).abs() < 1e-9 && p.rect[3] > 0.0);
        let d = comp.drawing;
        // Cyrillic in the caption reaches the DXF as is (UTF-8, no \\U+ escapes or code pages)
        assert!(crate::dxf::write_string(&d.to_dxf()).contains(ru("boundary_word")));
        let (mut has_label, mut in_place) = (false, false);
        for (vp, mk) in &d.marks {
            if let Mark::Text { pos, s, layer, .. } = mk {
                if s == ru("boundary_word") && *layer == super::super::NOTES {
                    has_label = vp.is_none();
                    in_place = pos[0] > 30.0 && pos[1] < 250.0;
                }
            }
        }
        assert!(has_label && in_place);
        assert!(dxf_on(&d, "Notes") >= 3);
        let pdf = PdfFile::parse(&export_sheets_pdf(&m, &[a], &c).unwrap()).unwrap();
        let page = pdf.page(0).unwrap();
        assert!(page.texts.iter().any(|t| t.string.contains(ru("demolition_word"))));
        m.delete_legend(leg).unwrap();
        assert_eq!(
            marks_on(
                &compose_sheet(&m, a, &c).unwrap().drawing,
                super::super::NOTES
            ),
            0
        );
    }
}
