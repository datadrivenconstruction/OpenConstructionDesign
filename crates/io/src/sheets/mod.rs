// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sheets: paper format, frame and title block per standard profile, viewports with views
//! at scale. Line weights, text heights and sizes come from `data/drawing_standards.json`
//! by profile (iso, gost) and view scale. Output is vector PDF (our own writer) and DXF.

use std::collections::BTreeMap;

use ddc_model::{LineClass, PlanView, Side, TagCat, ViewPrim};
use serde_json::Value;

use crate::dxf;
use crate::pdf;

mod anno;
mod annot;
mod dset;
mod dset_area;
mod dset_check;
mod dset_rooms;
mod dset_tags;
#[doc(hidden)]
pub mod dset_fixture;
#[cfg(test)]
mod dset_tests;
mod output;
mod set;
mod table;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_output;
mod view_style;

pub use anno::{anno_fmt, view_anno_fmt, AnnoFmt};
pub use annot::text_width;
pub use dset::{
    add_default_views, compass_keys, flow_tables, footprint, mark_box, mark_prefix, pack_views, page_key, page_table,
    plan_drawing_set, tag_levels,
    DsetOptions, Foot, Part, Planned, Region, SCHEDULES,
};
pub use dset_check::{check_set, DsetReport};
pub use output::{
    hatch_rule, stamp_cells, stamp_from_project, today_local, view_reference, StampCell, LEGENDS,
    LEGEND_LINES, LEGEND_MATERIALS, LEGEND_WALL_TYPES, SHEET_LIST,
};
pub use set::{
    compose_sheet, export_sheets_pdf, extent, file_name, print_composed, print_sheet, to_pdf,
    unique_name, view_drawing, Composed, Ctx, Placed, PlacedKind, PlanMarks, SheetRefs,
    POCHE_FROM,
};
pub use table::{plan_columns, split_rows, Plan, Room, Swatch, Table};
pub use view_style::ViewStyle;

const DATA: &str = include_str!("../../data/drawing_standards.json");

/// Sheet layers (identical in PDF and DXF).
pub const LAYERS: [&str; 15] = [
    "Frame",
    "Stamp",
    "Cut",
    "Hatch",
    "Openings",
    "Tags",
    "Dims",
    "Grid",
    "Titles",
    "Projection",
    "Beyond",
    "Levels",
    "Schedules",
    "Detail",
    "Notes",
];
const FRAME: usize = 0;
const STAMP: usize = 1;
const CUT: usize = 2;
const HATCH: usize = 3;
const OPEN: usize = 4;
const TAGS: usize = 5;
const DIMS: usize = 6;
const GRID: usize = 7;
const TITLES: usize = 8;
const PROJ: usize = 9;
const BEYOND: usize = 10;
const LEVELS: usize = 11;
const SCHED: usize = 12;
const DETAIL: usize = 13;
const NOTES: usize = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct SheetError(pub String);

impl std::fmt::Display for SheetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sheet: {}", self.0)
    }
}
impl std::error::Error for SheetError {}

fn err<T>(m: impl Into<String>) -> Result<T, SheetError> {
    Err(SheetError(m.into()))
}

/// Default legend: top-left corner of the frame with a 5 mm inset, sheet mm.
pub fn legend_default_pos(
    paper: &str,
    landscape: bool,
    standard: &str,
) -> Result<(f64, f64), SheetError> {
    let (_, h) = paper_size(paper, landscape)?;
    let d = data();
    let fr = &d["standards"][standard]["frame"];
    Ok((num(fr, "left") + 5.0, h - num(fr, "top") - 5.0))
}

/// Parsed data file: one per process, read for every legend row.
fn data() -> &'static Value {
    static PARSED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(DATA).expect("drawing_standards.json is valid"))
}

fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// A word of the standard's data (title block label, north letter) in the document language:
/// catalog key `sheets.std.<standard>.<name>`; without a catalog or a key, the data's own text.
pub(crate) fn std_word(
    words: Option<&dyn Fn(&str) -> String>,
    standard: &str,
    name: &str,
    own: &str,
) -> String {
    let key = format!("sheets.std.{standard}.{name}");
    match words.map(|w| w(&key)) {
        Some(s) if !s.is_empty() && s != key => s,
        _ => own.to_string(),
    }
}

/// Sheet size in mm (width, height); `landscape` — long side horizontal.
pub fn paper_size(name: &str, landscape: bool) -> Result<(f64, f64), SheetError> {
    let d = data();
    let Some(p) = d["papers"].get(name).and_then(Value::as_array) else {
        return err(format!("unknown paper '{name}'"));
    };
    let (a, b) = (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0));
    let (s, l) = (a.min(b), a.max(b));
    Ok(if landscape { (l, s) } else { (s, l) })
}

/// Format and profile names from the data file.
pub fn papers() -> Vec<String> {
    data()["papers"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}
pub fn standards() -> Vec<String> {
    data()["standards"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Line weights (paper mm) and text heights for a scale.
#[derive(Clone, Debug, PartialEq)]
pub struct Weights {
    pub cut: f64,
    pub outline: f64,
    pub thin: f64,
    pub hatch: f64,
    pub dim: f64,
    pub tag: f64,
    pub dim_text: f64,
    pub grid_text: f64,
}

/// Standard profile; the scale row is selected by denominator `1:denom`.
pub fn weights(standard: &str, denom: f64) -> Result<Weights, SheetError> {
    let d = data();
    let Some(st) = d["standards"].get(standard) else {
        return err(format!("unknown standard '{standard}'"));
    };
    let rows = st["scales"].as_array().cloned().unwrap_or_default();
    let Some(r) = rows.iter().find(|r| denom <= num(r, "max")) else {
        return err("no scale row");
    };
    Ok(Weights {
        cut: num(r, "cut"),
        outline: num(r, "outline"),
        thin: num(r, "thin"),
        hatch: num(r, "hatch"),
        dim: num(r, "dim"),
        tag: num(r, "tag"),
        dim_text: num(r, "dim_text"),
        grid_text: num(r, "grid_text"),
    })
}

/// Project dimension caption units: metric — mm, imperial — feet-inches (1/8").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LengthUnits {
    #[default]
    Metric,
    Imperial,
}

/// Text of a saved measurement on a sheet: the number in sheet units, or the reason when it has
/// none (an orphan keeps its last number, marked with the reason).
pub fn measure_text(value: Option<f64>, unit: &str, reason: &str, u: LengthUnits) -> String {
    let n = value.map(|v| match unit {
        "m" => format_length(v, u),
        "m2" => format!("{v:.2} m2"),
        "m3" => format!("{v:.2} m3"),
        "deg" => format!("{v:.1} deg"),
        _ => format!("{v:.1} %"),
    });
    match (n, reason.is_empty()) {
        (Some(n), true) => n,
        (Some(n), false) => format!("{n} ({reason})"),
        (None, _) => format!("n/a: {reason}"),
    }
}

/// Caption for a model length (m) in project units.
pub fn format_length(m: f64, u: LengthUnits) -> String {
    match u {
        LengthUnits::Metric => format!("{:.0}", m * 1000.0),
        LengthUnits::Imperial => {
            let e = (m / 0.0254 * 8.0).round() as i64;
            let (sign, e) = if e < 0 { ("-", -e) } else { ("", e) };
            let (ft, rem) = (e / 96, e % 96);
            let (inch, fr) = (rem / 8, rem % 8);
            let frac = match fr {
                0 => String::new(),
                2 | 6 => format!(" {}/4", fr / 2),
                4 => " 1/2".to_string(),
                f => format!(" {f}/8"),
            };
            format!("{sign}{ft}'-{inch}{frac}\"")
        }
    }
}

/// Paper length, mm, of a model segment of `m` metres at scale 1:`scale`.
pub fn paper_mm(m: f64, scale: f64) -> f64 {
    m * 1000.0 / scale
}

/// Fits a field value into width `avail`: first reduces height down to `min_h`, then truncates with "…".
fn fit_text(s: &str, h: f64, min_h: f64, avail: f64) -> (f64, String) {
    if s.is_empty() || text_w(s, h) <= avail {
        return (h, s.to_string());
    }
    let min_h = min_h.min(h).max(0.0);
    let h2 = (h * avail / text_w(s, h)).max(min_h);
    if text_w(s, h2) <= avail || h2 <= 0.0 {
        return (h2, s.to_string());
    }
    let mut cut = String::new();
    for c in s.chars() {
        cut.push(c);
        if text_w(&format!("{}…", cut.trim_end()), h2) > avail {
            cut.pop();
            break;
        }
    }
    (h2, format!("{}…", cut.trim_end()))
}

pub struct Viewport<'a> {
    pub view: &'a PlanView,
    /// Scale denominator: 50 → 1:50.
    pub scale: f64,
    /// [x, y, w, h] on the sheet, mm; None — the sheet area above the title block.
    pub rect: Option<[f64; 4]>,
    pub title: String,
}

pub struct SheetSpec<'a> {
    pub paper: String,
    pub landscape: bool,
    pub standard: String,
    /// Title block fields (keys are the profile cells' `field`). `scale` defaults to the first view's.
    pub fields: BTreeMap<String, String>,
    pub viewports: Vec<Viewport<'a>>,
    pub units: LengthUnits,
    /// Area unit in the sheet's language; None: the standard's own.
    pub area_unit: Option<String>,
    /// Company logo for the title block cell of the owner (`legal_owner`, `organization`).
    pub logo: Option<crate::logo::Logo>,
    /// Catalog of the document language for the standard's words; None: the data's own.
    pub words: Option<&'a dyn Fn(&str) -> String>,
}

/// Logo placed on the sheet: `rect` is [x, y, w, h] in sheet mm.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetLogo {
    pub rect: [f64; 4],
    pub image: std::sync::Arc<pdf::PdfImage>,
}

/// Title block cells that hold the logo, by the profile's `field`.
const LOGO_FIELDS: [&str; 2] = ["legal_owner", "organization"];

#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    Line {
        pts: Vec<[f64; 2]>,
        closed: bool,
        weight: f64,
        /// Colour from the view graphics; None — black.
        rgb: Option<[u8; 3]>,
        layer: usize,
    },
    Text {
        pos: [f64; 2],
        h: f64,
        rot: f64,
        s: String,
        layer: usize,
    },
    /// Solid fill (even-odd): the first ring is the outline, the rest are holes; `gray` 0 — black.
    Fill {
        rings: Vec<Vec<[f64; 2]>>,
        gray: u8,
        /// Colour from the view graphics instead of `gray`.
        rgb: Option<[u8; 3]>,
        layer: usize,
    },
}

/// A finished sheet: marks in sheet mm; `vp` — viewport index (None — frame/title block).
#[derive(Clone, Debug, PartialEq)]
pub struct SheetDrawing {
    pub width: f64,
    pub height: f64,
    pub marks: Vec<(Option<usize>, Mark)>,
    /// For each view: sheet point (mm), model point (m), model metres per sheet mm.
    pub transforms: Vec<([f64; 2], [f64; 2], f64)>,
    /// Logo in the title block, if the sheet has one.
    pub logo: Option<SheetLogo>,
}

/// Rounds sheet coordinates to 1e-6 mm: output does not depend on noise in the last bits.
/// Solid fill → HATCH SOLID: rings are polyline boundaries, even-odd rule (style 0).
fn solid_hatch(
    rings: &[Vec<[f64; 2]>],
    rgb: [u8; 3],
    layer: &str,
    f: &dyn Fn([f64; 2]) -> [f64; 2],
) -> dxf::Entity {
    let p = |c: i32, v: String| dxf::Pair::new(c, v);
    let n = |c: i32, v: f64| dxf::Pair::new(c, format!("{v}"));
    let mut out = vec![
        p(0, "HATCH".into()),
        p(100, "AcDbEntity".into()),
        p(8, layer.into()),
    ];
    if rgb != [0, 0, 0] {
        out.push(p(420, true_color(rgb).to_string()));
    }
    out.extend([
        p(100, "AcDbHatch".into()),
        n(10, 0.0),
        n(20, 0.0),
        n(30, 0.0),
        n(210, 0.0),
        n(220, 0.0),
        n(230, 1.0),
        p(2, "SOLID".into()),
        p(70, "1".into()),
        p(71, "0".into()),
        p(91, rings.len().to_string()),
    ]);
    for (k, r) in rings.iter().enumerate() {
        // 2 — polyline boundary, +1 — outer (first ring)
        out.push(p(92, if k == 0 { "3" } else { "2" }.into()));
        out.extend([
            p(72, "0".into()),
            p(73, "1".into()),
            p(93, r.len().to_string()),
        ]);
        for &v in r {
            let q = f(v);
            out.extend([n(10, r6(q[0])), n(20, r6(q[1]))]);
        }
        out.push(p(97, "0".into()));
    }
    out.extend([p(75, "0".into()), p(76, "1".into()), p(98, "0".into())]);
    dxf::Entity::Unknown(out)
}

fn true_color(c: [u8; 3]) -> i32 {
    (i32::from(c[0]) << 16) | (i32::from(c[1]) << 8) | i32::from(c[2])
}

fn r6(v: f64) -> f64 {
    let x = (v * 1e6).round() / 1e6;
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

struct Pen<'a> {
    out: &'a mut Vec<(Option<usize>, Mark)>,
    vp: Option<usize>,
}

impl Pen<'_> {
    fn line(&mut self, pts: Vec<[f64; 2]>, closed: bool, weight: f64, layer: usize) {
        let pts = pts.into_iter().map(|p| [r6(p[0]), r6(p[1])]).collect();
        self.out.push((
            self.vp,
            Mark::Line {
                pts,
                closed,
                weight,
                rgb: None,
                layer,
            },
        ));
    }
    fn text(&mut self, pos: [f64; 2], h: f64, rot: f64, s: &str, layer: usize) {
        if s.is_empty() {
            return;
        }
        self.out.push((
            self.vp,
            Mark::Text {
                pos: [r6(pos[0]), r6(pos[1])],
                h,
                rot,
                s: s.to_string(),
                layer,
            },
        ));
    }
    fn fill(&mut self, rings: Vec<Vec<[f64; 2]>>, gray: u8, layer: usize) {
        let rings = rings
            .into_iter()
            .map(|r| r.into_iter().map(|p| [r6(p[0]), r6(p[1])]).collect())
            .collect();
        self.out.push((
            self.vp,
            Mark::Fill {
                rings,
                gray,
                rgb: None,
                layer,
            },
        ));
    }
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, weight: f64, layer: usize) {
        self.line(
            vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            true,
            weight,
            layer,
        );
    }
}

fn text_w(s: &str, h: f64) -> f64 {
    text_width(s, h)
}

/// Lines of a tag as a sheet shows it: a room's number and name over its area; a door or
/// window mark ("?" when it has none).
/// Lines of a tag; a room's floor finish only when `finish` (the scale allows it).
pub(super) fn tag_lines(tag: &ddc_model::Tag, unit: &str, sep: &str, finish: bool) -> Vec<String> {
    let mark = if tag.text.mark.trim().is_empty() {
        "?"
    } else {
        tag.text.mark.as_str()
    };
    let mut lines = vec![];
    if tag.cat == TagCat::Room {
        lines.push(format!("{mark} {}", tag.text.name).trim().to_string());
        if let Some(a) = tag.text.area {
            lines.push(format!("{} {unit}", fmt_num(a, 2, sep)));
        }
        if let (true, Some(f)) = (finish, tag.text.finish.as_deref().filter(|f| !f.is_empty())) {
            lines.push(f.to_string());
        }
    } else {
        lines.push(mark.to_string());
    }
    lines
}

/// Half width and half height of a tag's frame about its head, mm; a window frame reaches
/// `0.6 * half height` further on each side.
pub(super) fn tag_half(lines: &[String], th: f64) -> (f64, f64) {
    let tw = lines.iter().map(|l| text_w(l, th)).fold(0.0, f64::max);
    (tw / 2.0 + th * 0.45, lines.len() as f64 * th * 0.8 + th * 0.25)
}

fn fmt_num(v: f64, decimals: usize, sep: &str) -> String {
    let s = format!("{v:.decimals$}");
    if sep == "." {
        s
    } else {
        s.replace('.', sep)
    }
}

/// Hatch segments of a polygon: parallel lines at angle `deg` with spacing `step`,
/// inside by the even-odd rule of edge crossings.
/// Hatching of a region with several contours (even-odd: holes are not hatched).
fn hatch_rings(rings: &[Vec<[f64; 2]>], deg: f64, step: f64) -> Vec<[[f64; 2]; 2]> {
    let (s, c) = deg.to_radians().sin_cos();
    // u — along the hatch line, v — across it.
    let uv = |p: [f64; 2]| (p[0] * c + p[1] * s, -p[0] * s + p[1] * c);
    let back = |u: f64, v: f64| [u * c - v * s, u * s + v * c];
    let rs: Vec<Vec<(f64, f64)>> = rings
        .iter()
        .map(|r| r.iter().map(|&p| uv(p)).collect())
        .collect();
    let (vmin, vmax) = rs
        .iter()
        .flatten()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| {
            (a.0.min(p.1), a.1.max(p.1))
        });
    let mut out = vec![];
    let mut k = (vmin / step).floor() as i64 + 1;
    while (k as f64) * step < vmax {
        let v = k as f64 * step;
        let mut xs = vec![];
        for pts in &rs {
            for i in 0..pts.len() {
                let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
                if (p.1 <= v) != (q.1 <= v) {
                    xs.push(p.0 + (v - p.1) / (q.1 - p.1) * (q.0 - p.0));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks_exact(2) {
            if pair[1] - pair[0] > 1e-9 {
                out.push([back(pair[0], v), back(pair[1], v)]);
            }
        }
        k += 1;
    }
    out
}

/// Dashed line by pattern `dash` (dash, gap, …), sheet mm.
fn dashed(a: [f64; 2], b: [f64; 2], dash: &[f64]) -> Vec<[[f64; 2]; 2]> {
    let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    if dash.is_empty() || len <= 0.0 || dash.iter().sum::<f64>() <= 0.0 {
        return vec![[a, b]];
    }
    let at = |t: f64| {
        [
            a[0] + (b[0] - a[0]) * t / len,
            a[1] + (b[1] - a[1]) * t / len,
        ]
    };
    let mut out = vec![];
    let (mut t, mut k) = (0.0, 0);
    while t < len {
        let e = (t + dash[k % dash.len()]).min(len);
        if k % 2 == 0 {
            out.push([at(t), at(e)]);
        }
        t = e;
        k += 1;
    }
    out
}

fn arc_pts(c: [f64; 2], r: f64, a0: f64, a1: f64) -> Vec<[f64; 2]> {
    let sweep = (a1 - a0).rem_euclid(360.0);
    let sweep = if sweep == 0.0 { 360.0 } else { sweep };
    let n = (sweep / 5.625).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let t = (a0 + sweep * i as f64 / n as f64).to_radians();
            [c[0] + r * t.cos(), c[1] + r * t.sin()]
        })
        .collect()
}

/// Section hatching: the material pattern from the view graphics, without it — the data file rules.
fn hatch_section(
    pen: &mut Pen<'_>,
    rs: &[Vec<[f64; 2]>],
    material: &str,
    rules: &[Value],
    style: Option<&ViewStyle>,
    weight: f64,
) {
    if let Some(s) = style {
        s.hatch(pen, rs, material, weight);
        return;
    }
    let m = material.to_lowercase();
    let rule = rules.iter().find(|h| {
        let ms = h["match"].as_array().cloned().unwrap_or_default();
        ms.is_empty() || ms.iter().any(|x| x.as_str().is_some_and(|x| m.contains(x)))
    });
    if let Some(rule) = rule {
        let step = num(rule, "spacing");
        for a in rule["angles"].as_array().into_iter().flatten() {
            for s in hatch_rings(rs, a.as_f64().unwrap_or(45.0), step) {
                pen.line(s.to_vec(), false, weight, HATCH);
            }
        }
    }
}

/// Sheet assembly.
pub fn compose(spec: &SheetSpec) -> Result<SheetDrawing, SheetError> {
    compose_styled(spec, &[])
}

/// Sheet assembly with view graphics (by viewport index).
pub fn compose_styled(
    spec: &SheetSpec,
    styles: &[Option<ViewStyle>],
) -> Result<SheetDrawing, SheetError> {
    let d = data();
    let Some(st) = d["standards"].get(&spec.standard).cloned() else {
        return err(format!("unknown standard '{}'", spec.standard));
    };
    let (w, h) = paper_size(&spec.paper, spec.landscape)?;
    let fr = &st["frame"];
    let (fl, fr_, ft, fb) = (
        num(fr, "left"),
        num(fr, "right"),
        num(fr, "top"),
        num(fr, "bottom"),
    );
    let mut marks = vec![];
    let mut transforms = vec![];
    let mut pen = Pen {
        out: &mut marks,
        vp: None,
    };
    pen.rect(0.0, 0.0, w, h, num(fr, "trim_weight"), FRAME);
    let (ix, iy, iw, ih) = (fl, fb, w - fl - fr_, h - fb - ft);
    pen.rect(ix, iy, iw, ih, num(fr, "weight"), FRAME);

    // The title block sits in the bottom-right corner of the frame.
    let sp = &st["stamp"];
    let (sw, sh) = (num(sp, "w"), num(sp, "h"));
    if sw > iw || sh > ih {
        return err("title block does not fit the paper");
    }
    let (sx, sy) = (ix + iw - sw, iy);
    let (lh, vh) = (num(&st, "stamp_label_h"), num(&st, "stamp_value_h"));
    let min_h = st
        .get("stamp_min_h")
        .and_then(Value::as_f64)
        .unwrap_or(lh.min(vh));
    let mut fields = spec.fields.clone();
    if let Some(v) = spec.viewports.first() {
        fields
            .entry("scale".into())
            .or_insert_with(|| format!("1:{}", v.scale));
    }
    let mut logo_place: Option<SheetLogo> = None;
    for c in sp["cells"].as_array().into_iter().flatten() {
        let (cx, cy, cw, ch) = (sx + num(c, "x"), sy + num(c, "y"), num(c, "w"), num(c, "h"));
        pen.rect(cx, cy, cw, ch, num(fr, "weight"), STAMP);
        let key = text(c, "key");
        let label = std_word(spec.words, &spec.standard, &key, &text(c, "label"));
        let fixed = std_word(spec.words, &spec.standard, &key, &text(c, "text"));
        let value = c
            .get("field")
            .and_then(Value::as_str)
            .and_then(|f| fields.get(f))
            .cloned()
            .unwrap_or_default();
        let vh = c.get("value_h").and_then(Value::as_f64).unwrap_or(vh);
        let mut x = cx + 1.0;
        let holds_logo = c
            .get("field")
            .and_then(Value::as_str)
            .is_some_and(|f| LOGO_FIELDS.contains(&f));
        if let (Some(lg), true, None) = (&spec.logo, holds_logo, &logo_place) {
            // the picture takes the left of the cell under the label; the text keeps the rest
            let top = if label.is_empty() { 0.0 } else { lh + 1.5 };
            let (bw, bh) = (cw * 0.42, ch - 2.0 - top);
            if bw > 1.0 && bh > 1.0 {
                let [_, oy, fw, fh] = lg.fit(bw, bh);
                logo_place = Some(SheetLogo {
                    rect: [cx + 1.0, cy + 1.0 + oy, fw, fh],
                    image: lg.to_pdf_image(),
                });
                x = cx + 1.0 + fw + 1.5;
            }
        }
        if !label.is_empty() {
            pen.text([cx + 1.0, cy + ch - lh - 1.0], lh, 0.0, &label, STAMP);
            let (vh, value) = fit_text(&value, vh, min_h, cx + cw - 1.0 - x);
            pen.text([x, cy + (ch - lh - 1.0 - vh) / 2.0], vh, 0.0, &value, STAMP);
            continue;
        }
        if !fixed.is_empty() {
            pen.text([x, cy + (ch - lh) / 2.0], lh, 0.0, &fixed, STAMP);
            x += text_w(&fixed, lh) + 1.5;
        }
        let (vh, value) = fit_text(&value, vh, min_h, cx + cw - 1.0 - x);
        pen.text([x, cy + (ch - vh) / 2.0], vh, 0.0, &value, STAMP);
    }

    let dim = st["dim"].clone();
    let grid = st["grid"].clone();
    let dash: Vec<f64> = grid["dash"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    let unit = spec.area_unit.clone().unwrap_or_else(|| text(&st, "area_unit"));
    let sep = text(&st, "decimal");
    let fmt = anno::anno_fmt(&spec.standard, spec.words);
    let hatch_rules = d["hatch"].as_array().cloned().unwrap_or_default();
    for (k, vp) in spec.viewports.iter().enumerate() {
        if !(vp.scale.is_finite() && vp.scale > 0.0) {
            return err("scale must be positive");
        }
        let wt = weights(&spec.standard, vp.scale)?;
        let rect = vp.rect.unwrap_or([ix, iy + sh, iw, ih - sh]);
        let bb = vp.view.bbox;
        let model_c = [(bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0];
        let sheet_c = [rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0];
        // Model in metres, sheet in mm: 1 m of model = 1000/denominator sheet mm.
        transforms.push((sheet_c, model_c, vp.scale / 1000.0));
        let tf = |p: (f64, f64)| {
            [
                sheet_c[0] + paper_mm(p.0 - model_c[0], vp.scale),
                sheet_c[1] + paper_mm(p.1 - model_c[1], vp.scale),
            ]
        };
        let mut pen = Pen {
            out: &mut marks,
            vp: Some(k),
        };
        let actx = annot::Ctx {
            dim: &dim,
            wt: &wt,
            scale: vp.scale,
            units: spec.units,
            sep: &sep,
        };
        let mut pending = vec![];
        let mut inner = vec![];
        let style = styles.get(k).and_then(|s| s.as_ref());
        for p in &vp.view.prims {
            let first_mark = pen.out.len();
            let paint = style.and_then(|s| s.paint_of(p));
            // under the hatch: the override fill; at a small scale a solid fill replaces it
            let under = vp.scale < POCHE_FROM && paint.is_some_and(|e| e.over.fill.is_some());
            match p {
                ViewPrim::Anno { item } => anno::draw_anno(&mut pen, &tf, item, vp.scale, &fmt),
                ViewPrim::Cut { material, ring, .. } => {
                    let r: Vec<[f64; 2]> = ring.iter().map(|&q| tf(q)).collect();
                    if vp.scale >= POCHE_FROM {
                        // small scale: hatching is indistinguishable — solid section fill
                        pen.fill(vec![r.clone()], set::POCHE_GRAY, CUT);
                    } else {
                        if under {
                            pen.fill(vec![r.clone()], 255, CUT);
                        }
                        let rs = std::slice::from_ref(&r);
                        hatch_section(&mut pen, rs, material, &hatch_rules, style, wt.hatch);
                    }
                    pen.line(r, true, wt.cut, CUT);
                }
                ViewPrim::DoorLeaf { a, b, .. } => {
                    pen.line(vec![tf(*a), tf(*b)], false, wt.outline, OPEN)
                }
                ViewPrim::DoorSwing {
                    center,
                    radius,
                    a0,
                    a1,
                    ..
                } => pen.line(
                    arc_pts(tf(*center), paper_mm(*radius, vp.scale), *a0, *a1),
                    false,
                    wt.thin,
                    OPEN,
                ),
                ViewPrim::WindowLine { a, b, .. } | ViewPrim::CurtainLine { a, b, .. } => {
                    pen.line(vec![tf(*a), tf(*b)], false, wt.thin, OPEN)
                }
                ViewPrim::RoomTag {
                    pos,
                    name,
                    number,
                    area,
                    cut,
                    finish,
                    ..
                } => {
                    let area = format!("{} {unit}", fmt_num(*area, 2, &sep));
                    // the standard says up to which scale a room tag also names its finish
                    let show = st["tag_finish_max_scale"].as_f64().is_some_and(|m| vp.scale <= m);
                    let mut lines = vec![name.as_str(), number.as_str(), area.as_str()];
                    if let (true, Some(f)) = (show, finish.as_deref().filter(|f| !f.is_empty())) {
                        lines.push(f);
                    }
                    let tag = annot::room_tag(
                        tf(*pos),
                        &lines,
                        wt.tag,
                        TAGS,
                    );
                    // a section names a room only where its tag fits the room's cut on paper
                    let r = tag.rect();
                    let fits = cut.is_none_or(|(w, h)| {
                        r[2] - r[0] <= paper_mm(w, vp.scale) && r[3] - r[1] <= paper_mm(h, vp.scale)
                    });
                    if fits {
                        pending.push(tag);
                    }
                }
                ViewPrim::Annot { tier, geom, .. } => {
                    pending.extend(actx.draw(&mut pen, &tf, *tier, geom));
                }
                ViewPrim::Measure { a, b, value, unit, reason, .. } => {
                    let (pa, pb) = (tf(*a), tf(*b));
                    if (pa[0] - pb[0]).abs() + (pa[1] - pb[1]).abs() > 1e-9 {
                        pen.line(vec![pa, pb], false, wt.dim, DIMS);
                    }
                    let s = measure_text(*value, unit, reason, spec.units);
                    let th = wt.dim_text;
                    let mid = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
                    pen.text([mid[0] - text_w(&s, th) / 2.0, mid[1] + num(&dim, "text_gap")], th, 0.0, &s, DIMS);
                }
                ViewPrim::Tag { tag } => {
                    let show = st["tag_finish_max_scale"].as_f64().is_some_and(|m| vp.scale <= m);
                    let lines = tag_lines(tag, &unit, &sep, show);
                    let th = wt.tag;
                    let (c, at) = (tf(tag.head), tf(tag.anchor));
                    let (hw, hh) = tag_half(&lines, th);
                    let frame = match tag.cat {
                        TagCat::Window => {
                            let k = hh * 0.6;
                            vec![
                                [c[0] - hw - k, c[1]],
                                [c[0] - hw, c[1] - hh],
                                [c[0] + hw, c[1] - hh],
                                [c[0] + hw + k, c[1]],
                                [c[0] + hw, c[1] + hh],
                                [c[0] - hw, c[1] + hh],
                            ]
                        }
                        TagCat::Room => vec![],
                        _ => vec![
                            [c[0] - hw, c[1] - hh],
                            [c[0] + hw, c[1] - hh],
                            [c[0] + hw, c[1] + hh],
                            [c[0] - hw, c[1] + hh],
                        ],
                    };
                    if !frame.is_empty() {
                        pen.line(frame, true, wt.thin, TAGS);
                    }
                    let outside = (at[0] - c[0]).abs() > hw || (at[1] - c[1]).abs() > hh;
                    if tag.spec.leader && outside {
                        let from = [
                            at[0].clamp(c[0] - hw, c[0] + hw),
                            at[1].clamp(c[1] - hh, c[1] + hh),
                        ];
                        pen.line(vec![from, at], false, wt.thin, TAGS);
                    }
                    let n = lines.len() as f64;
                    for (i, l) in lines.iter().enumerate() {
                        let y = c[1] + (n - 1.0) * th * 0.8 - i as f64 * th * 1.6 - th * 0.4;
                        pen.text([c[0] - text_w(l, th) / 2.0, y], th, 0.0, l, TAGS);
                    }
                }
                ViewPrim::Grid {
                    label,
                    a,
                    b,
                    bubbles,
                } => {
                    let (pa, pb) = (tf(*a), tf(*b));
                    for s in dashed(pa, pb, &dash) {
                        pen.line(s.to_vec(), false, wt.thin, GRID);
                    }
                    let len = ((pb[0] - pa[0]).powi(2) + (pb[1] - pa[1]).powi(2))
                        .sqrt()
                        .max(1e-9);
                    let r = num(&grid, "radius");
                    // mark beyond the end along the axis direction: at the end — forward, at the start — backward
                    let ends = [(bubbles.1, pb, 1.0), (bubbles.0, pa, -1.0)];
                    for (on, q, sg) in ends {
                        if !on {
                            continue;
                        }
                        let c = [
                            q[0] + sg * (pb[0] - pa[0]) / len * r,
                            q[1] + sg * (pb[1] - pa[1]) / len * r,
                        ];
                        pen.line(arc_pts(c, r, 0.0, 360.0), true, wt.thin, GRID);
                        let th = wt.grid_text;
                        pen.text(
                            [c[0] - text_w(label, th) / 2.0, c[1] - th / 2.0],
                            th,
                            0.0,
                            label,
                            GRID,
                        );
                    }
                }
                ViewPrim::SectionCut {
                    material,
                    rings,
                    holes,
                    ..
                } => {
                    let mut rs: Vec<Vec<[f64; 2]>> = vec![rings.iter().map(|&q| tf(q)).collect()];
                    rs.extend(holes.iter().map(|h| h.iter().map(|&q| tf(q)).collect()));
                    if vp.scale >= POCHE_FROM {
                        pen.fill(rs.clone(), set::POCHE_GRAY, CUT);
                    } else {
                        if under {
                            pen.fill(rs.clone(), 255, CUT);
                        }
                        hatch_section(&mut pen, &rs, material, &hatch_rules, style, wt.hatch);
                    }
                    for r in rs {
                        pen.line(r, true, wt.cut, CUT);
                    }
                }
                ViewPrim::Edge { class, a, b, .. } => {
                    let (w, l) = match class {
                        LineClass::Projection => (wt.outline, PROJ),
                        LineClass::Beyond | LineClass::Overhead => (wt.thin, BEYOND),
                    };
                    pen.line(vec![tf(*a), tf(*b)], false, w, l);
                }
                ViewPrim::Level {
                    name,
                    elevation,
                    a,
                    b,
                } => {
                    let (pa, pb) = (tf(*a), tf(*b));
                    for s in dashed(pa, pb, &dash) {
                        pen.line(s.to_vec(), false, wt.thin, LEVELS);
                    }
                    // Level mark: a triangle tick above the line end, labels — name and ±0.000.
                    let th = wt.tag;
                    let k = th * 0.8;
                    pen.line(
                        vec![pb, [pb[0] - k, pb[1] + k], [pb[0] + k, pb[1] + k]],
                        true,
                        wt.thin,
                        LEVELS,
                    );
                    let e = if elevation.abs() < 5e-4 {
                        format!("±{}", fmt_num(0.0, 3, &sep))
                    } else if *elevation > 0.0 {
                        format!("+{}", fmt_num(*elevation, 3, &sep))
                    } else {
                        format!("-{}", fmt_num(-elevation, 3, &sep))
                    };
                    pen.text([pb[0] + k + 1.0, pb[1] + k + 0.5], th, 0.0, &e, LEVELS);
                    pen.text([pb[0] + k + 1.0, pb[1] - th - 0.5], th, 0.0, name, LEVELS);
                }
                ViewPrim::Dim {
                    side,
                    chain,
                    a,
                    b,
                    value,
                } if *chain >= ddc_model::INNER_CHAIN => {
                    inner.extend(actx.inner(*side, tf(*a), tf(*b), *value, *chain == ddc_model::RADIUS_CHAIN));
                }
                ViewPrim::Dim {
                    side,
                    chain,
                    a,
                    b,
                    value,
                } => {
                    let n = match side {
                        Side::South => [0.0, -1.0],
                        Side::North => [0.0, 1.0],
                        Side::West => [-1.0, 0.0],
                        Side::East => [1.0, 0.0],
                    };
                    let (pa, pb) = (tf(*a), tf(*b));
                    let o = num(&dim, "first") + *chain as f64 * num(&dim, "step");
                    let (gap, over, ext) = (
                        num(&dim, "gap"),
                        num(&dim, "overshoot"),
                        num(&dim, "extend"),
                    );
                    let off = |p: [f64; 2], t: f64| [p[0] + n[0] * t, p[1] + n[1] * t];
                    let u = [pb[0] - pa[0], pb[1] - pa[1]];
                    let ul = (u[0] * u[0] + u[1] * u[1]).sqrt().max(1e-12);
                    let u = [u[0] / ul, u[1] / ul];
                    let (da, db) = (off(pa, o), off(pb, o));
                    pen.line(
                        vec![
                            [da[0] - u[0] * ext, da[1] - u[1] * ext],
                            [db[0] + u[0] * ext, db[1] + u[1] * ext],
                        ],
                        false,
                        wt.dim,
                        DIMS,
                    );
                    for p in [pa, pb] {
                        pen.line(vec![off(p, gap), off(p, o + over)], false, wt.dim, DIMS);
                    }
                    let sz = num(&dim, "size");
                    for (p, dir) in [(da, 1.0), (db, -1.0)] {
                        if text(&dim, "terminator") == "tick" {
                            let t = [
                                (u[0] + n[0]) * sz / 2.0 / 2f64.sqrt(),
                                (u[1] + n[1]) * sz / 2.0 / 2f64.sqrt(),
                            ];
                            pen.line(
                                vec![[p[0] - t[0], p[1] - t[1]], [p[0] + t[0], p[1] + t[1]]],
                                false,
                                wt.outline,
                                DIMS,
                            );
                        } else {
                            let tip = p;
                            let base = [p[0] + u[0] * dir * sz, p[1] + u[1] * dir * sz];
                            let hw = sz / 6.0;
                            pen.line(
                                vec![
                                    tip,
                                    [base[0] + n[0] * hw, base[1] + n[1] * hw],
                                    [base[0] - n[0] * hw, base[1] - n[1] * hw],
                                ],
                                true,
                                wt.dim,
                                DIMS,
                            );
                        }
                    }
                    let s = format_length(*value, spec.units);
                    let th = wt.dim_text;
                    let tg = num(&dim, "text_gap");
                    let mid = [(da[0] + db[0]) / 2.0, (da[1] + db[1]) / 2.0];
                    let tw = text_w(&s, th);
                    if matches!(side, Side::South | Side::North) {
                        pen.text([mid[0] - tw / 2.0, mid[1] + tg], th, 0.0, &s, DIMS);
                    } else {
                        pen.text([mid[0] - tg, mid[1] - tw / 2.0], th, 90.0, &s, DIMS);
                    }
                }
            }
            if let Some(s) = style {
                s.restyle(pen.out, first_mark, p, paint, &wt);
            }
        }
        annot::flush(&mut pen, pending);
        actx.flush_inner(&mut pen, inner);
        if !vp.title.is_empty() {
            let s = format!("{}  1:{}", vp.title, vp.scale);
            let th = wt.tag + 1.5;
            pen.text(
                [sheet_c[0] - text_w(&s, th) / 2.0, rect[1] + 2.0],
                th,
                0.0,
                &s,
                TITLES,
            );
        }
    }
    Ok(SheetDrawing {
        width: w,
        height: h,
        marks,
        transforms,
        logo: logo_place,
    })
}

impl SheetDrawing {
    pub fn to_pdf_sheet(&self) -> pdf::Sheet {
        let items = self
            .marks
            .iter()
            .map(|(_, m)| match m {
                Mark::Line {
                    pts,
                    closed,
                    weight,
                    rgb,
                    layer,
                } => pdf::Item::Polyline {
                    pts: pts.clone(),
                    closed: *closed,
                    weight_mm: *weight,
                    rgb: rgb.unwrap_or([0, 0, 0]),
                    layer: *layer,
                },
                Mark::Text {
                    pos,
                    h,
                    rot,
                    s,
                    layer,
                } => pdf::Item::Text {
                    pos: *pos,
                    height_mm: *h,
                    rotation: *rot,
                    string: s.clone(),
                    rgb: [0, 0, 0],
                    layer: *layer,
                },
                Mark::Fill {
                    rings,
                    gray,
                    rgb,
                    layer,
                } => pdf::Item::Fill {
                    rings: rings.clone(),
                    rgb: rgb.unwrap_or([*gray; 3]),
                    layer: *layer,
                },
            })
            .collect::<Vec<_>>();
        let mut items = items;
        if let Some(lg) = &self.logo {
            items.push(pdf::Item::Image {
                rect: lg.rect,
                image: lg.image.clone(),
                layer: STAMP,
            });
        }
        pdf::Sheet {
            width_mm: self.width,
            height_mm: self.height,
            layers: LAYERS
                .iter()
                .map(|n| pdf::SheetLayer {
                    name: n.to_string(),
                    visible: true,
                })
                .collect(),
            items,
        }
    }

    pub fn to_pdf(&self) -> Result<Vec<u8>, pdf::PdfError> {
        pdf::export_pdf(&[self.to_pdf_sheet()])
    }

    /// DXF: the whole sheet in paper space (mm), views also in model space 1:1, m
    /// (inverse view transform; annotations in model metres). Test support only.
    #[cfg(any(test, feature = "dxf-write"))]
    pub fn to_dxf(&self) -> dxf::DxfDocument {
        let lw = |w: f64| {
            const OK: [i32; 24] = [
                0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120,
                140, 158, 200, 211,
            ];
            let t = (w * 100.0).round() as i32;
            *OK.iter().min_by_key(|v| (*v - t).abs()).unwrap_or(&0)
        };
        let ent = |m: &Mark, f: &dyn Fn([f64; 2]) -> [f64; 2], k: f64| -> Vec<dxf::Entity> {
            if let Mark::Fill {
                rings,
                gray,
                rgb,
                layer,
            } = m
            {
                // a white mask on a dark CAD background would look like a white blot
                if *gray == 255 && rgb.is_none() {
                    return vec![];
                }
                return vec![solid_hatch(
                    rings,
                    rgb.unwrap_or([*gray; 3]),
                    LAYERS[*layer],
                    f,
                )];
            }
            vec![match m {
                Mark::Line {
                    pts,
                    closed,
                    weight,
                    rgb,
                    layer,
                } => {
                    let mut common = dxf::Common::on_layer(LAYERS[*layer]);
                    common.lineweight = Some(lw(*weight));
                    common.true_color = rgb.map(true_color);
                    if pts.len() == 2 && !closed {
                        let (a, b) = (f(pts[0]), f(pts[1]));
                        dxf::Entity::Line(dxf::Line {
                            common,
                            start: [r6(a[0]), r6(a[1]), 0.0],
                            end: [r6(b[0]), r6(b[1]), 0.0],
                        })
                    } else {
                        dxf::Entity::LwPolyline(dxf::LwPolyline {
                            common,
                            flags: i32::from(*closed),
                            vertices: pts
                                .iter()
                                .map(|&p| {
                                    let q = f(p);
                                    dxf::LwVertex {
                                        x: r6(q[0]),
                                        y: r6(q[1]),
                                        ..Default::default()
                                    }
                                })
                                .collect(),
                        })
                    }
                }
                Mark::Text {
                    pos,
                    h,
                    rot,
                    s,
                    layer,
                } => {
                    let q = f(*pos);
                    dxf::Entity::Text(dxf::Text {
                        common: dxf::Common::on_layer(LAYERS[*layer]),
                        pos: [r6(q[0]), r6(q[1]), 0.0],
                        height: r6(h * k),
                        text: s.clone(),
                        rotation: *rot,
                        style: None,
                    })
                }
                Mark::Fill { .. } => unreachable!("handled above"),
            }]
        };
        let id = |p: [f64; 2]| p;
        let mut paper = dxf::Block::new("*Paper_Space");
        paper.entities = self
            .marks
            .iter()
            .flat_map(|(_, m)| ent(m, &id, 1.0))
            .collect();
        let mut model = vec![];
        for (vp, m) in &self.marks {
            let Some(k) = vp else { continue };
            let (sc, mc, s) = self.transforms[*k];
            let back = move |p: [f64; 2]| [mc[0] + (p[0] - sc[0]) * s, mc[1] + (p[1] - sc[1]) * s];
            model.extend(ent(m, &back, s));
        }
        dxf::DxfDocument {
            header: vec![
                dxf::HeaderVar {
                    name: "$INSUNITS".into(),
                    values: vec![dxf::Pair::new(70, "6")],
                },
                // without this viewers show all lines with one weight
                dxf::HeaderVar {
                    name: "$LWDISPLAY".into(),
                    values: vec![dxf::Pair::new(290, "1")],
                },
            ],
            ltypes: vec![],
            layers: LAYERS
                .iter()
                .map(|n| dxf::Layer {
                    name: n.to_string(),
                    flags: 0,
                    color: 7,
                    linetype: "Continuous".into(),
                    extra: vec![],
                })
                .collect(),
            // TrueType font: default stroke fonts often lack Cyrillic
            styles: vec![dxf::TextStyle {
                name: "Standard".into(),
                flags: 0,
                height: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                font: "NotoSans-Regular.ttf".into(),
                big_font: String::new(),
                extra: vec![],
            }],
            other_tables: vec![],
            blocks: vec![paper],
            entities: model,
            other_sections: vec![],
        }
    }
}
