// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sheet issue: built-in tables (sheet list, legends), project fields in the title block, title-block
//! cells for editing on the canvas, section and elevation marks on plans with a "view/sheet" reference.

use std::collections::BTreeMap;

use ddc_model::{Model, SectionSpec, SheetDef, ViewKind, ViewPlacement, ViewRef, ViewportDef};
use serde_json::Value;

use super::set::{clip_seg, POCHE_FROM, POCHE_GRAY};
use super::table::{Swatch, Table};
use super::{
    arc_pts, dashed, data, format_length, num, paper_size, text, text_w, weights, Mark, Pen,
    SheetError, Weights, DIMS, GRID, TAGS,
};

/// Sheet list key; `sheet_list:<set id>` — the list of one set.
pub const SHEET_LIST: &str = "sheet_list";
pub const LEGEND_MATERIALS: &str = "legend:materials";
pub const LEGEND_WALL_TYPES: &str = "legend:wall_types";
pub const LEGEND_LINES: &str = "legend:lines";
pub const LEGENDS: [&str; 3] = [LEGEND_MATERIALS, LEGEND_WALL_TYPES, LEGEND_LINES];

fn me(e: ddc_model::Error) -> SheetError {
    SheetError(e.to_string())
}

type Text<'a> = &'a dyn Fn(&str) -> String;

fn tr(text: Text, key: &str) -> String {
    text(&format!("sheets.paper.{key}"))
}

/// Project field → title-block cell per profile: (profile, cell, project field).
const PROJECT_MAP: [(&str, &str, &str); 14] = [
    ("gost", "object", "name"),
    ("iso", "legal_owner", "client"),
    ("iso", "department", "organization"),
    ("iso", "tech_ref", "number"),
    ("iso", "creator", "author"),
    ("iso", "approver", "approver"),
    ("iso", "document_type", "name"),
    ("iso", "status", "status"),
    ("gost", "organization", "organization"),
    ("gost", "designation", "number"),
    ("gost", "developer", "author"),
    ("gost", "checker", "checker"),
    ("gost", "approver", "approver"),
    ("gost", "stage", "stage"),
];

/// Cells the build computes itself; a project value with the same name does not override them
/// (e.g. the project issue date and the latest revision date in the title block are different things).
const AUTO_CELLS: [&str; 8] = [
    "title",
    "sheet",
    "sheets",
    "scale",
    "revision",
    "identification",
    "date",
    "issue_date",
];

/// Title-block cell values from project fields; the sheet's own value wins and is applied at build time.
/// The single point where project fields become the title block.
pub fn stamp_from_project(
    standard: &str,
    project: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = project
        .iter()
        .filter(|(k, _)| !AUTO_CELLS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (st, cell, key) in PROJECT_MAP {
        if st == standard {
            if let Some(v) = project.get(key) {
                out.entry(cell.to_string()).or_insert_with(|| v.clone());
            }
        }
    }
    out
}

/// Today's date from the computer clock (year, month, day); None — time zone unavailable.
#[cfg(windows)]
pub fn today_local() -> Option<(i32, u32, u32)> {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: a struct of integers; GetLocalTime only fills it
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut t) };
    Some((t.wYear as i32, t.wMonth as u32, t.wDay as u32))
}

#[cfg(not(windows))]
pub fn today_local() -> Option<(i32, u32, u32)> {
    None
}

/// Title-block cell with a field: `rect` — [x, y, w, h] on the sheet, mm.
#[derive(Clone, Debug, PartialEq)]
pub struct StampCell {
    pub key: String,
    pub label: String,
    pub rect: [f64; 4],
}

/// Title-block cells of a sheet: same positions as in the `compose` build.
pub fn stamp_cells(
    standard: &str,
    paper: &str,
    landscape: bool,
) -> Result<Vec<StampCell>, SheetError> {
    let d = data();
    let Some(st) = d["standards"].get(standard) else {
        return Err(SheetError(format!("unknown standard '{standard}'")));
    };
    let (w, _) = paper_size(paper, landscape)?;
    let fr = &st["frame"];
    let sp = &st["stamp"];
    let sx = w - num(fr, "right") - num(sp, "w");
    let sy = num(fr, "bottom");
    let mut out = vec![];
    for c in sp["cells"].as_array().into_iter().flatten() {
        let Some(key) = c.get("field").and_then(Value::as_str) else {
            continue;
        };
        let label = match text(c, "label") {
            l if l.is_empty() => text(c, "text"),
            l => l,
        };
        out.push(StampCell {
            key: key.to_string(),
            label,
            rect: [sx + num(c, "x"), sy + num(c, "y"), num(c, "w"), num(c, "h")],
        });
    }
    Ok(out)
}

/// Material hatching per profile rules: (angles, spacing, mm); a rule without `match` is the default.
pub fn hatch_rule(material: &str) -> (Vec<f64>, f64) {
    let m = material.to_lowercase();
    let rule = data()["hatch"].as_array().into_iter().flatten().find(|h| {
        let mut ms = h["match"].as_array().into_iter().flatten().peekable();
        ms.peek().is_none() || ms.any(|x| x.as_str().is_some_and(|x| m.contains(x)))
    });
    match rule {
        Some(r) => (
            r["angles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_f64)
                .collect(),
            num(r, "spacing"),
        ),
        None => (vec![45.0], 2.0),
    }
}

/// Material symbol as it appears on this sheet: a small scale is filled solid.
fn material_swatch(material: &str, scale: f64) -> Swatch {
    if scale >= POCHE_FROM {
        return Swatch::Solid { gray: POCHE_GRAY };
    }
    let (angles, step) = hatch_rule(material);
    Swatch::Hatch { angles, step }
}

fn sheet_list(m: &Model, set: Option<i64>, text: Text) -> Result<Table, SheetError> {
    let title = tr(text, "sheet_list");
    let all = m.sheets().map_err(me)?;
    let sheets: Vec<(i64, SheetDef)> = match set {
        None => all,
        Some(id) => {
            let sets = m.sheet_sets().map_err(me)?;
            let Some(s) = sets.into_iter().find(|s| s.0 == id) else {
                return Ok(no_set(title, text));
            };
            s.2.iter()
                .filter_map(|i| all.iter().find(|a| a.0 == *i).cloned())
                .collect()
        }
    };
    let mut rows = vec![];
    for (id, d) in &sheets {
        let revs = m.revisions(*id).map_err(me)?;
        let last = revs.last();
        rows.push(vec![
            d.number.clone(),
            d.name.clone(),
            last.map(|r| r.mark.clone()).unwrap_or_default(),
            last.map(|r| r.date.clone()).unwrap_or_default(),
        ]);
    }
    Ok(Table {
        title,
        header: ["col_sheet", "col_name", "col_rev", "col_date"]
            .map(|k| tr(text, k))
            .to_vec(),
        rows,
        swatches: vec![],
    })
}

/// The set was deleted: the sheet keeps a readable empty list rather than a service key.
fn no_set(title: String, text: Text) -> Table {
    Table {
        title,
        header: vec![],
        rows: vec![vec![tr(text, "no_data")]],
        swatches: vec![],
    }
}

fn legend_lines(w: &Weights, text: Text) -> Table {
    let rows: [(&str, f64); 5] = [
        ("l_cut", w.cut),
        ("l_outline", w.outline),
        ("l_thin", w.thin),
        ("l_hatch", w.hatch),
        ("l_dim", w.dim),
    ];
    Table {
        title: tr(text, "lines"),
        header: vec![String::new(), tr(text, "line"), tr(text, "weight")],
        rows: rows
            .iter()
            .map(|r| vec![String::new(), tr(text, r.0), format!("{:.2}", r.1)])
            .collect(),
        swatches: rows.iter().map(|r| Swatch::Line { weight: r.1 }).collect(),
    }
}

/// A table the sheet builds itself from the model: sheet list and legends; None — a schedule key.
pub(super) fn builtin_table(
    m: &Model,
    key: &str,
    s: &SheetDef,
    vps: &[(i64, ViewportDef)],
    ctx: &super::Ctx,
) -> Result<Option<Table>, SheetError> {
    let text = ctx.text;
    // the legend shows what is visible on the sheet: with several scales — the hatching of the
    // largest view (the small view's fill is recognisable from it, not the other way round)
    let smallest = vps
        .iter()
        .map(|v| v.1.scale)
        .filter(|s| *s > 0.0)
        .fold(f64::INFINITY, f64::min);
    let scale = if smallest.is_finite() { smallest } else { 0.0 };
    if key == SHEET_LIST {
        return sheet_list(m, None, text).map(Some);
    }
    if let Some(set) = key.strip_prefix("sheet_list:") {
        return match set.parse::<i64>() {
            Ok(id) => sheet_list(m, Some(id), text).map(Some),
            Err(_) => Ok(Some(no_set(tr(text, "sheet_list"), text))),
        };
    }
    match key {
        super::dset_area::AREAS => super::dset_area::areas(m, text).map(Some),
        super::dset_area::FINISHES => super::dset_area::finishes(m, text).map(Some),
        LEGEND_MATERIALS => {
            let names = m.material_names().map_err(me)?;
            Ok(Some(Table {
                title: tr(text, "materials"),
                header: vec![String::new(), tr(text, "material")],
                rows: names
                    .iter()
                    .map(|n| vec![String::new(), n.clone()])
                    .collect(),
                swatches: names.iter().map(|n| material_swatch(n, scale)).collect(),
            }))
        }
        LEGEND_WALL_TYPES => {
            let types = m.wall_type_legend().map_err(me)?;
            let mut rows = vec![];
            let mut swatches = vec![];
            for t in &types {
                let total: f64 = t.layers.iter().map(|l| l.thickness).sum();
                let mats: Vec<&str> = t.layers.iter().map(|l| l.material.as_str()).collect();
                rows.push(vec![
                    String::new(),
                    t.name.clone(),
                    format_length(total, ctx.units),
                    mats.join(" / "),
                ]);
                swatches.push(
                    t.layers
                        .first()
                        .map_or(Swatch::None, |l| material_swatch(&l.material, scale)),
                );
            }
            Ok(Some(Table {
                title: tr(text, "wall_types"),
                header: vec![
                    String::new(),
                    tr(text, "type"),
                    tr(text, "thickness"),
                    tr(text, "layers"),
                ],
                rows,
                swatches,
            }))
        }
        LEGEND_LINES => {
            let denom = if scale > 0.0 { scale } else { 100.0 };
            Ok(Some(legend_lines(&weights(&s.standard, denom)?, text)))
        }
        _ => Ok(None),
    }
}

const HEAD_R: f64 = 4.5;
/// Stroke weight of a mark outline and its arrows, mm.
const MARK_WEIGHT: f64 = 0.25;

/// First placement of a view: the mark's reference points to it.
pub fn view_reference(refs: &[ViewPlacement], view_def: i64) -> Option<&ViewPlacement> {
    refs.iter().find(|p| p.view == ViewRef::View(view_def))
}

/// Text no wider than `w`: height is reduced proportionally.
fn fit_h(s: &str, h: f64, w: f64) -> f64 {
    let tw = text_w(s, h);
    if tw > w && tw > 0.0 {
        h * w / tw
    } else {
        h
    }
}

fn centered(pen: &mut Pen, c: [f64; 2], h: f64, s: &str) {
    pen.text([c[0] - text_w(s, h) / 2.0, c[1] - h / 2.0], h, 0.0, s, TAGS);
}

/// Mark circle: a placed view — view number above the line and sheet number below it; an unplaced one —
/// only the view name.
fn head(pen: &mut Pen, c: [f64; 2], name: &str, rf: Option<&ViewPlacement>) {
    pen.line(arc_pts(c, HEAD_R, 0.0, 360.0), true, MARK_WEIGHT, TAGS);
    let inner = HEAD_R * 1.7;
    match rf {
        Some(p) => {
            pen.line(
                vec![[c[0] - HEAD_R, c[1]], [c[0] + HEAD_R, c[1]]],
                false,
                MARK_WEIGHT,
                TAGS,
            );
            let top = fit_h(&p.detail, 2.5, inner);
            centered(pen, [c[0], c[1] + HEAD_R * 0.45], top, &p.detail);
            let low = fit_h(&p.sheet_number, 1.8, inner);
            centered(pen, [c[0], c[1] - HEAD_R * 0.45], low, &p.sheet_number);
        }
        None => {
            let h = fit_h(name, 2.5, inner);
            centered(pen, c, h, name);
        }
    }
}

fn add(p: [f64; 2], d: [f64; 2], k: f64) -> [f64; 2] {
    [p[0] + d[0] * k, p[1] + d[1] * k]
}

fn triangle(pen: &mut Pen, tip: [f64; 2], back: [f64; 2], side: [f64; 2], half: f64) {
    pen.line(
        vec![tip, add(back, side, half), add(back, side, -half)],
        true,
        MARK_WEIGHT,
        TAGS,
    );
}

/// Gap between a view mark and the dimension chains and grid bubbles it keeps beyond, mm.
const BAND_CLEAR: f64 = 1.0;

/// Paper box of the dimension and grid marks of view `k`: view marks keep beyond it.
fn outer_band(marks: &[(Option<usize>, Mark)], k: usize) -> Option<[f64; 4]> {
    let mut bb = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (v, m) in marks {
        let layer = match m {
            Mark::Line { layer, .. } | Mark::Text { layer, .. } | Mark::Fill { layer, .. } => {
                *layer
            }
        };
        if *v == Some(k) && (layer == DIMS || layer == GRID) {
            let b = super::dset::mark_box(m);
            bb = [
                bb[0].min(b[0]),
                bb[1].min(b[1]),
                bb[2].max(b[2]),
                bb[3].max(b[3]),
            ];
        }
    }
    (bb[0] <= bb[2] && bb[1] <= bb[3]).then_some(bb)
}

/// Box of points about the origin.
fn rel_box(pts: &[[f64; 2]], half: f64) -> [f64; 4] {
    pts.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, p| {
            [
                b[0].min(p[0] - half),
                b[1].min(p[1] - half),
                b[2].max(p[0] + half),
                b[3].max(p[1] + half),
            ]
        },
    )
}

/// How far a mark group (`rel`, its box about `p`) moves along unit `dir` to keep `BAND_CLEAR`
/// beyond `band`, mm; 0 when it is clear already.
fn past_band(p: [f64; 2], dir: [f64; 2], rel: [f64; 4], band: [f64; 4]) -> f64 {
    let mut best = f64::INFINITY;
    for ax in 0..2 {
        let (lo, hi) = (p[ax] + rel[ax], p[ax] + rel[ax + 2]);
        let (b0, b1) = (band[ax] - BAND_CLEAR, band[ax + 2] + BAND_CLEAR);
        if hi <= b0 || lo >= b1 {
            return 0.0;
        }
        if dir[ax] > 1e-9 {
            best = best.min((b1 - lo) / dir[ax]);
        } else if dir[ax] < -1e-9 {
            best = best.min((hi - b0) / -dir[ax]);
        }
    }
    if best.is_finite() {
        best
    } else {
        0.0
    }
}

/// View mark in sheet coordinates; the look direction is left of a→b. The `crop` (sheet) limits the section
/// line so circles stay inside the viewport; a mark outside the crop is not drawn.
/// Section ends and the elevation head move out beyond `band` (dimension chains and axis
/// bubbles), so their numbers never lie on those texts.
#[allow(clippy::too_many_arguments)]
fn draw_mark(
    pen: &mut Pen,
    spec: &SectionSpec,
    a: [f64; 2],
    b: [f64; 2],
    rf: Option<&ViewPlacement>,
    wt: &Weights,
    dash: &[f64],
    crop: Option<[f64; 4]>,
    band: Option<[f64; 4]>,
) {
    let m = 2.0 * HEAD_R + 2.0;
    let inset = crop.map(|r| [r[0] + m, r[1] + m, r[2] - 2.0 * m, r[3] - 2.0 * m]);
    let (a, b) = match inset {
        Some(r) if r[2] > 0.0 && r[3] > 0.0 => {
            let Some((t0, t1)) = clip_seg(a, b, r) else {
                return;
            };
            let at = |t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            (at(t0), at(t1))
        }
        Some(_) => return,
        None => (a, b),
    };
    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    if len < 1e-6 {
        return;
    }
    let d = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
    let look = [-d[1], d[0]];
    let push = |p: [f64; 2], dir: [f64; 2], pts: &[[f64; 2]]| match band {
        Some(bd) => add(p, dir, past_band(p, dir, rel_box(pts, 0.0), bd)),
        None => p,
    };
    match spec.kind {
        ViewKind::Section => {
            // what an end draws about its point: the arrow and the head beyond the line
            let group = |out: [f64; 2]| {
                let c = add([0.0, 0.0], out, HEAD_R + 1.5);
                [
                    [0.0, 0.0],
                    add([0.0, 0.0], look, 6.0),
                    add(add([0.0, 0.0], look, 3.5), out, 1.0),
                    add(add([0.0, 0.0], look, 3.5), out, -1.0),
                    [c[0] - HEAD_R, c[1] - HEAD_R],
                    [c[0] + HEAD_R, c[1] + HEAD_R],
                ]
            };
            let back = [-d[0], -d[1]];
            let a = push(a, back, &group(back));
            let b = push(b, d, &group(d));
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let stub = (len / 2.0).min(8.0);
            pen.line(vec![a, add(a, d, stub)], false, wt.cut, TAGS);
            pen.line(vec![b, add(b, d, -stub)], false, wt.cut, TAGS);
            if len > 2.0 * stub {
                for s in dashed(add(a, d, stub), add(b, d, -stub), dash) {
                    pen.line(s.to_vec(), false, wt.thin, TAGS);
                }
            }
            for (p, out) in [(a, [-d[0], -d[1]]), (b, d)] {
                let tip = add(p, look, 6.0);
                pen.line(vec![p, tip], false, MARK_WEIGHT, TAGS);
                triangle(pen, tip, add(p, look, 3.5), out, 1.0);
                head(pen, add(p, out, HEAD_R + 1.5), &spec.name, rf);
            }
        }
        ViewKind::Elevation => {
            let away = [-look[0], -look[1]];
            let c0 = add([0.0, 0.0], away, HEAD_R + 1.5);
            let group = [
                add([0.0, 0.0], look, 1.5),
                add(c0, d, HEAD_R * 0.8),
                add(c0, d, -HEAD_R * 0.8),
                [c0[0] - HEAD_R, c0[1] - HEAD_R],
                [c0[0] + HEAD_R, c0[1] + HEAD_R],
            ];
            let mid = push(add(a, d, len / 2.0), away, &group);
            let c = add(mid, look, -(HEAD_R + 1.5));
            triangle(pen, add(c, look, HEAD_R + 3.0), c, d, HEAD_R * 0.8);
            head(pen, c, &spec.name, rf);
        }
    }
}

/// Marks of all sections and elevations on the plan of viewport `k`. Numbers come from current placements, so
/// references update themselves when sheets are renumbered or a view is moved.
pub(super) fn section_marks(
    dr: &mut super::SheetDrawing,
    k: usize,
    v: &ViewportDef,
    standard: &str,
    defs: &[(i64, SectionSpec)],
    refs: &[ViewPlacement],
    rect: [f64; 4],
) -> Result<(), SheetError> {
    if !matches!(v.view, ViewRef::Plan { .. }) || defs.is_empty() {
        return Ok(());
    }
    let marks: Vec<_> = defs
        .iter()
        .map(|(id, spec)| (spec, view_reference(refs, *id)))
        .collect();
    marks_on(dr, k, standard, v.scale, &marks, v.crop.map(|_| rect))
}

/// Marks of `specs` on plan view `k` as a placed plan shows them (heads without sheet
/// references), to measure how far a plan reaches before it is placed.
pub(super) fn plan_view_marks(
    dr: &mut super::SheetDrawing,
    k: usize,
    standard: &str,
    scale: f64,
    specs: &[SectionSpec],
) -> Result<(), SheetError> {
    let marks: Vec<_> = specs.iter().map(|s| (s, None)).collect();
    marks_on(dr, k, standard, scale, &marks, None)
}

fn marks_on(
    dr: &mut super::SheetDrawing,
    k: usize,
    standard: &str,
    scale: f64,
    marks: &[(&SectionSpec, Option<&ViewPlacement>)],
    crop: Option<[f64; 4]>,
) -> Result<(), SheetError> {
    let wt = weights(standard, scale)?;
    let d = data();
    let dash: Vec<f64> = d["standards"][standard]["grid"]["dash"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    let (sc, mc, s) = dr.transforms[k];
    let tf = |p: (f64, f64)| [sc[0] + (p.0 - mc[0]) / s, sc[1] + (p.1 - mc[1]) / s];
    let mut band = outer_band(&dr.marks, k);
    // sections first; elevation heads then keep beyond the section heads as well
    for kind in [ViewKind::Section, ViewKind::Elevation] {
        let from = dr.marks.len();
        let mut pen = Pen {
            out: &mut dr.marks,
            vp: Some(k),
        };
        for (spec, rf) in marks.iter().filter(|m| m.0.kind == kind) {
            let (a, b) = (tf(spec.a), tf(spec.b));
            draw_mark(&mut pen, spec, a, b, *rf, &wt, &dash, crop, band);
        }
        if let Some(bd) = band.as_mut() {
            for (_, m) in &dr.marks[from..] {
                let b = super::dset::mark_box(m);
                *bd = [
                    bd[0].min(b[0]),
                    bd[1].min(b[1]),
                    bd[2].max(b[2]),
                    bd[3].max(b[3]),
                ];
            }
        }
    }
    Ok(())
}
