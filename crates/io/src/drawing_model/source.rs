// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Underlay: lines, arcs, texts, fills and block inserts in metres with a layer name and pen
//! (colour, weight, dashes), from a DXF or a PDF page.

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};

use crate::dxf::{
    block_kind, decode_unicode, expand_inserts, mtext_plain, text_plain, BlockKind, Common,
    DxfDocument, Entity as DEntity, ExpandOptions, ExpandReport, LwPolyline,
};
use super::textfmt::TextFmt;
use crate::pdf::import::{model_entities, Geom, ImportError, ImportOptions, PageDrawing};

#[derive(Clone, Debug, PartialEq)]
pub struct ULine {
    pub layer: String,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub dashed: bool,
    /// Index into [`Underlay::pens`] (0 — the default pen).
    pub pen: u32,
}

/// An arc counter-clockwise from `a0` to `a1` (rad, a1 > a0); a circle — `a1 = a0 + 2π`.
#[derive(Clone, Debug, PartialEq)]
pub struct UArc {
    pub layer: String,
    pub c: (f64, f64),
    pub r: f64,
    pub a0: f64,
    pub a1: f64,
    pub pen: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UText {
    pub layer: String,
    pub pos: (f64, f64),
    pub height: f64,
    /// Radians.
    pub rotation: f64,
    pub text: String,
    pub pen: u32,
    /// Justification, style and multi-line details; default for a plain text.
    pub fmt: super::textfmt::TextFmt,
}

/// Line style: colour (`None` — foreground colour), weight, dashes (m; + dash, − gap).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Pen {
    pub color: Option<[u8; 3]>,
    pub weight_mm: f32,
    pub dash: Vec<f32>,
}

/// A closed outline (polyline or circle) — for rooms and columns.
#[derive(Clone, Debug, PartialEq)]
pub struct UPoly {
    pub layer: String,
    pub pts: Vec<(f64, f64)>,
    /// Radius, if the outline is a circle.
    pub circle: Option<f64>,
}

/// Hatching or a solid fill: outlines (the first is outer), pattern segments.
#[derive(Clone, Debug, PartialEq)]
pub struct UFill {
    pub layer: String,
    pub loops: Vec<Vec<(f64, f64)>>,
    pub solid: bool,
    pub pattern: String,
    pub segs: Vec<((f64, f64), (f64, f64))>,
    pub pen: u32,
}

/// An insert of a named block (m, rad).
#[derive(Clone, Debug, PartialEq)]
pub struct UInsert {
    pub name: String,
    pub layer: String,
    pub pos: (f64, f64),
    pub rotation: f64,
    /// Block extents along its X and Y axes, m.
    pub size: (f64, f64),
    pub center: (f64, f64),
    pub attribs: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ULayer {
    pub name: String,
    pub color: Option<[u8; 3]>,
    /// The layer is turned off in the file (negative colour).
    pub off: bool,
    pub frozen: bool,
    pub linetype: String,
    pub weight_mm: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Underlay {
    pub lines: Vec<ULine>,
    pub arcs: Vec<UArc>,
    pub texts: Vec<UText>,
    /// Door/window blocks found by name among the expanded INSERTs.
    pub block_hints: Vec<UBlockHint>,
    pub pens: Vec<Pen>,
    pub closed: Vec<UPoly>,
    pub fills: Vec<UFill>,
    pub inserts: Vec<UInsert>,
    pub layers: Vec<ULayer>,
    /// Sheets of the file; only model space goes to the underlay.
    pub layouts: Vec<String>,
    /// Paper-space entities that did not reach the underlay.
    pub paper_skipped: usize,
    /// Primitive types without plan geometry → count (named in the import report).
    pub unknown: std::collections::BTreeMap<String, usize>,
    /// Block definitions and top-level INSERTs of the file, written back as blocks on export.
    pub blocks: super::blocks::UBlocks,
    /// Text styles of the file (font, width factor), written back for styled texts.
    pub text_styles: Vec<crate::dxf::TextStyle>,
}

/// A selection of underlay primitives: indices by kind.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pick {
    pub lines: Vec<usize>,
    pub arcs: Vec<usize>,
    pub texts: Vec<usize>,
    pub closed: Vec<usize>,
    pub fills: Vec<usize>,
}

impl Pick {
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn len(&self) -> usize {
        self.lines.len() + self.arcs.len() + self.texts.len() + self.closed.len() + self.fills.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UBlockHint {
    pub name: String,
    pub layer: String,
    pub kind: BlockKind,
    pub pos: (f64, f64),
    /// Radians.
    pub rotation: f64,
    /// Block size along its X and Y axes, m.
    pub size: (f64, f64),
}

type P = (f64, f64);

/// Extrusion direction (groups 210/220/230) of an entity: `Some(true)` - (0,0,-1), x is mirrored;
/// `Some(false)` - normal; `None` - the plane is not horizontal, takes no part in the plan.
pub(super) fn plan_flip(c: &Common) -> Option<bool> {
    let mut n = [0.0, 0.0, 1.0];
    for p in &c.extra {
        if let 210..=230 = p.code {
            if let Ok(v) = p.value.trim().parse::<f64>() {
                n[(p.code / 10 - 21) as usize] = v;
            }
        }
    }
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len.is_nan() || len <= 1e-9 {
        return Some(false);
    }
    if (n[0] * n[0] + n[1] * n[1]).sqrt() / len > 1e-6 {
        return None;
    }
    Some(n[2] < 0.0)
}

fn is_solid(lt: &str) -> bool {
    matches!(
        lt.to_ascii_uppercase().as_str(),
        "" | "CONTINUOUS" | "BYLAYER" | "BYBLOCK"
    )
}

/// Colour of palette index 1..=255; 7 is the foreground colour (`None`).
pub fn aci_rgb(i: i32) -> Option<[u8; 3]> {
    const BASE: [[u8; 3]; 10] = [
        [0, 0, 0],
        [255, 0, 0],
        [255, 255, 0],
        [0, 255, 0],
        [0, 255, 255],
        [0, 0, 255],
        [255, 0, 255],
        [255, 255, 255],
        [128, 128, 128],
        [192, 192, 192],
    ];
    let i = i.abs();
    match i {
        7 => None,
        1..=9 => Some(BASE[i as usize]),
        10..=249 => {
            let k = i - 10;
            let h = (k / 10) as f64 * 15.0;
            let v = [1.0, 1.0, 0.8, 0.8, 0.6, 0.6, 0.5, 0.5, 0.3, 0.3][(k % 10) as usize];
            let s = if k % 2 == 1 { 0.5 } else { 1.0 };
            Some(hsv(h, s, v))
        }
        250..=255 => {
            let g = [51, 91, 132, 173, 214, 255][(i - 250) as usize];
            Some([g, g, g])
        }
        _ => None,
    }
}

fn hsv(h: f64, s: f64, v: f64) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let q = |t: f64| ((t + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [q(r), q(g), q(b)]
}

fn true_rgb(v: i32) -> [u8; 3] {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

fn extra_i(c: &[crate::dxf::Pair], code: i32) -> Option<i32> {
    c.iter()
        .find(|p| p.code == code)
        .and_then(|p| p.value.trim().parse().ok())
}

fn extra_f(c: &[crate::dxf::Pair], code: i32) -> Option<f64> {
    c.iter()
        .find(|p| p.code == code)
        .and_then(|p| p.value.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Layers, linetypes and pens of the file: an entity's pen with ByLayer / ByBlock taken into account.
struct Pens<'a> {
    doc: &'a DxfDocument,
    unit: f64,
    ltscale: f64,
    layers: HashMap<String, (i32, Option<i32>, String, i32)>,
    pens: Vec<Pen>,
    index: HashMap<String, u32>,
}

impl<'a> Pens<'a> {
    fn new(doc: &'a DxfDocument, unit: f64) -> Self {
        let layers = doc
            .layers
            .iter()
            .map(|l| {
                (
                    l.name.to_uppercase(),
                    (
                        l.color,
                        extra_i(&l.extra, 420),
                        l.linetype.clone(),
                        extra_i(&l.extra, 370).unwrap_or(-3),
                    ),
                )
            })
            .collect();
        Pens {
            doc,
            unit,
            ltscale: doc.ltscale(),
            layers,
            pens: vec![Pen::default()],
            index: HashMap::new(),
        }
    }

    fn dash(&self, name: &str, scale: f64) -> Vec<f32> {
        if is_solid(name) {
            return vec![];
        }
        let Some(lt) = self
            .doc
            .ltypes
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
        else {
            // linetype not defined: a nominal 6/3 mm dash
            return vec![0.006, -0.003];
        };
        let k = self.ltscale * scale * self.unit;
        let d: Vec<f32> = lt
            .pattern
            .iter()
            .map(|v| (v * k) as f32)
            .filter(|v| v.is_finite())
            .collect();
        if d.iter().all(|v| *v == 0.0) {
            vec![]
        } else {
            d
        }
    }

    fn pen(&mut self, c: &Common) -> u32 {
        let lay = self.layers.get(&c.layer.to_uppercase()).cloned();
        let (lcolor, ltrue, lltype, lweight) = lay.unwrap_or((7, None, String::new(), -3));
        let by_layer = matches!(c.color, Some(0) | Some(256));
        let color = match (c.true_color, c.color) {
            (Some(t), _) if !by_layer => Some(true_rgb(t)),
            (_, Some(i)) if (1..=255).contains(&i) => aci_rgb(i),
            _ => ltrue.map(true_rgb).or_else(|| aci_rgb(lcolor)),
        };
        let lt = match c.linetype.as_deref() {
            Some(l) if !l.eq_ignore_ascii_case("BYLAYER") && !l.eq_ignore_ascii_case("BYBLOCK") => {
                l.to_string()
            }
            _ => lltype,
        };
        let scale = extra_f(&c.extra, 48).unwrap_or(1.0);
        let dash = self.dash(&lt, scale);
        let w = match c.lineweight {
            Some(w) if w >= 0 => w,
            Some(-2) => -3,
            _ => lweight,
        };
        let weight_mm = if w >= 0 { w as f32 / 100.0 } else { 0.25 };
        let key = format!("{color:?}|{weight_mm}|{dash:?}");
        if let Some(&k) = self.index.get(&key) {
            return k;
        }
        let k = self.pens.len() as u32;
        self.pens.push(Pen {
            color,
            weight_mm,
            dash,
        });
        self.index.insert(key, k);
        k
    }
}

/// Text height in metres: the entity height, else the text style height, else 2.5 mm times the
/// file's linetype scale (a plotted default). Non-finite or non-positive heights count as unset.
fn text_height(doc: &DxfDocument, h: f64, style: Option<&str>, unit: f64) -> f64 {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if ok(h) {
        return h * unit;
    }
    let from_style = style
        .and_then(|n| doc.styles.iter().find(|s| s.name.eq_ignore_ascii_case(n.trim())))
        .map(|s| s.height)
        .filter(|v| ok(*v));
    match from_style {
        Some(v) => v * unit,
        None if h.is_finite() => 0.0025 * doc.ltscale(),
        None => h,
    }
}

/// Metres per drawing unit: `$INSUNITS`; without it — from typical sizes (the gap of parallel
/// line pairs as a wall thickness of 0.08–0.6 m, door block widths of 0.6–1.2 m, and windows)
/// and a plausible plan extent on each axis after expanding blocks; `$MEASUREMENT` = 0 —
/// inches/feet.
pub fn guess_unit(doc: &DxfDocument) -> f64 {
    if let Some(u) = doc.unit_meters() {
        return u;
    }
    let ex = expand_inserts(
        doc,
        &ExpandOptions {
            max_entities: 200_000,
            ..ExpandOptions::default()
        },
    );
    let mut pts: Vec<P> = vec![];
    let mut segs: Vec<(P, P)> = vec![];
    for e in &ex.entities {
        match e {
            DEntity::Line(l) => {
                let (a, b) = ((l.start[0], l.start[1]), (l.end[0], l.end[1]));
                pts.extend([a, b]);
                segs.push((a, b));
            }
            DEntity::LwPolyline(pl) => {
                let v: Vec<P> = pl.vertices.iter().map(|v| (v.x, v.y)).collect();
                pts.extend(v.iter().copied());
                segs.extend(v.windows(2).map(|w| (w[0], w[1])));
                if pl.closed() && v.len() > 2 {
                    segs.push((v[v.len() - 1], v[0]));
                }
            }
            DEntity::Circle(c) => {
                pts.push((c.center[0] - c.radius, c.center[1] - c.radius));
                pts.push((c.center[0] + c.radius, c.center[1] + c.radius));
            }
            DEntity::Arc(a) => pts.push((a.center[0], a.center[1])),
            _ => {}
        }
    }
    for f in &ex.fills {
        pts.extend(f.loops.iter().flatten().copied());
    }
    let span = axis_span(&pts);
    let openings: Vec<(BlockKind, f64)> = ex
        .inserts
        .iter()
        .filter_map(|i| Some((block_kind(&i.name)?, i.size.0.abs().max(i.size.1.abs()))))
        .collect();
    let gaps = wall_gaps(&mut segs);
    let imperial = doc.measurement() == Some(0);
    // centimetres only with a clear majority (small details of a mm drawing give false votes)
    let cands: Vec<(f64, f64)> = if imperial {
        vec![(0.0254, 1.0), (0.3048, 1.0)]
    } else {
        vec![(0.001, 1.0), (1.0, 1.0), (0.01, 0.5)]
    };
    let fallback = match (imperial, span > 2000.0) {
        (true, _) => 0.0254,
        (false, true) => 0.001,
        (false, false) => 1.0,
    };
    let mut best = (fallback, unit_votes(&gaps, &openings, span, fallback));
    for &(u, w) in &cands {
        let v = w * unit_votes(&gaps, &openings, span, u);
        if v > best.1 {
            best = (u, v);
        }
    }
    best.0
}

/// The larger of the X and Y extents (X and Y outliers are not mixed).
fn axis_span(pts: &[P]) -> f64 {
    let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
    for p in pts.iter().filter(|p| p.0.is_finite() && p.1.is_finite()) {
        lo = (lo.0.min(p.0), lo.1.min(p.1));
        hi = (hi.0.max(p.0), hi.1.max(p.1));
    }
    if lo.0 > hi.0 {
        return 0.0;
    }
    (hi.0 - lo.0).max(hi.1 - lo.1)
}

/// For each of the longest segments: the distance to the nearest parallel overlapping one
/// and the overlap length (drawing units).
fn wall_gaps(segs: &mut Vec<(P, P)>) -> Vec<(f64, f64)> {
    let len = |s: &(P, P)| (s.1 .0 - s.0 .0).hypot(s.1 .1 - s.0 .1);
    segs.retain(|s| {
        [s.0 .0, s.0 .1, s.1 .0, s.1 .1]
            .iter()
            .all(|v| v.is_finite())
            && len(s) > 0.0
    });
    segs.sort_by(|a, b| len(b).total_cmp(&len(a)));
    segs.truncate(1500);
    let mut out = vec![];
    for (i, &(a, b)) in segs.iter().enumerate() {
        let l1 = len(&(a, b));
        let u = ((b.0 - a.0) / l1, (b.1 - a.1) / l1);
        let mut near: Option<(f64, f64)> = None;
        for (j, &(c, d)) in segs.iter().enumerate() {
            if i == j {
                continue;
            }
            let l2 = len(&(c, d));
            if (u.0 * (d.1 - c.1) - u.1 * (d.0 - c.0)).abs() > 0.02 * l2 {
                continue;
            }
            let lat = ((c.0 - a.0) * -u.1 + (c.1 - a.1) * u.0).abs();
            if lat <= 1e-9 * l1.max(1.0) {
                continue;
            }
            let t0 = (c.0 - a.0) * u.0 + (c.1 - a.1) * u.1;
            let t1 = (d.0 - a.0) * u.0 + (d.1 - a.1) * u.1;
            let ov = t0.max(t1).min(l1) - t0.min(t1).max(0.0);
            if ov > 0.0 && near.is_none_or(|n| lat < n.0) {
                near = Some((lat, ov));
            }
        }
        out.extend(near);
    }
    out
}

/// Votes for unit `u` (m): walls and openings of typical sizes, half a vote — a plan extent of
/// 1 m…5 km (a coordinate outlier spoils it, so it is not decisive).
fn unit_votes(gaps: &[(f64, f64)], openings: &[(BlockKind, f64)], span: f64, u: f64) -> f64 {
    let plausible = if (1.0..=5000.0).contains(&(span * u)) {
        0.5
    } else {
        0.0
    };
    let walls = gaps
        .iter()
        .filter(|(d, ov)| (0.08..=0.6).contains(&(d * u)) && ov * u >= 0.3)
        .count() as f64;
    let doors = openings
        .iter()
        .filter(|(k, w)| match k {
            BlockKind::Door => (0.6..=1.2).contains(&(w * u)),
            BlockKind::Window => (0.4..=3.0).contains(&(w * u)),
        })
        .count() as f64;
    plausible + walls + 3.0 * doors
}

/// A copy of the document with `\U+XXXX` decoded in layer and block names (pre-2007 files);
/// `None` if there are no such names.
fn decoded_names(doc: &DxfDocument) -> Option<DxfDocument> {
    let esc = |s: &str| s.contains("\\U+") || s.contains("\\u+");
    let ent_has = |e: &DEntity| match e {
        DEntity::Insert(i) => esc(&i.common.layer) || esc(&i.block),
        DEntity::Unknown(r) => r.iter().skip(1).any(|p| p.code == 8 && esc(&p.value)),
        e => ent_common(e).is_some_and(|c| esc(&c.layer)),
    };
    let needed = doc.layers.iter().any(|l| esc(&l.name))
        || doc
            .blocks
            .iter()
            .any(|b| esc(&b.name) || b.entities.iter().any(|e| ent_has(e)))
        || doc.entities.iter().any(|e| ent_has(e));
    if !needed {
        return None;
    }
    let mut d = doc.clone();
    for l in &mut d.layers {
        l.name = decode_unicode(&l.name);
    }
    for b in &mut d.blocks {
        b.name = decode_unicode(&b.name);
        b.entities.iter_mut().for_each(fix_names);
        for a in &mut b.attdefs {
            a.common.layer = decode_unicode(&a.common.layer);
        }
    }
    d.entities.iter_mut().for_each(fix_names);
    Some(d)
}

fn fix_names(e: &mut DEntity) {
    match e {
        DEntity::Insert(i) => {
            i.common.layer = decode_unicode(&i.common.layer);
            i.block = decode_unicode(&i.block);
            for a in &mut i.attribs {
                a.common.layer = decode_unicode(&a.common.layer);
            }
        }
        DEntity::Unknown(r) => {
            for p in r.iter_mut().skip(1).filter(|p| p.code == 8) {
                p.value = decode_unicode(&p.value);
            }
        }
        e => {
            if let Some(c) = ent_common_mut(e) {
                c.layer = decode_unicode(&c.layer);
            }
        }
    }
}

fn ent_common(e: &DEntity) -> Option<&Common> {
    Some(match e {
        DEntity::Line(x) => &x.common,
        DEntity::LwPolyline(x) => &x.common,
        DEntity::Circle(x) => &x.common,
        DEntity::Arc(x) => &x.common,
        DEntity::Text(x) => &x.common,
        DEntity::MText(x) => &x.common,
        DEntity::Insert(x) => &x.common,
        DEntity::AttDef(x) => &x.common,
        DEntity::Unknown(_) => return None,
    })
}

fn ent_common_mut(e: &mut DEntity) -> Option<&mut Common> {
    Some(match e {
        DEntity::Line(x) => &mut x.common,
        DEntity::LwPolyline(x) => &mut x.common,
        DEntity::Circle(x) => &mut x.common,
        DEntity::Arc(x) => &mut x.common,
        DEntity::Text(x) => &mut x.common,
        DEntity::MText(x) => &mut x.common,
        DEntity::Insert(x) => &mut x.common,
        DEntity::AttDef(x) => &mut x.common,
        DEntity::Unknown(_) => return None,
    })
}

/// Apply `f` to the listed items of `v`, in place or on appended copies; returns the result indices.
fn apply<T: Clone>(
    v: &mut Vec<T>,
    idx: &[usize],
    copy: bool,
    f: &mut dyn FnMut(&mut T),
) -> Vec<usize> {
    let mut out = vec![];
    for &k in idx {
        let Some(x) = v.get(k) else { continue };
        if copy {
            let mut y = x.clone();
            f(&mut y);
            v.push(y);
            out.push(v.len() - 1);
        } else {
            f(&mut v[k]);
            out.push(k);
        }
    }
    out
}

/// Similarity map of the plan: `x' = a*x + b*y + tx`, `y' = c*x + d*y + ty`, with
/// `|det| = k^2` for a uniform scale `k`. A negative determinant is a mirror.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub m: [f64; 4],
    pub t: (f64, f64),
}

impl Affine2 {
    /// Reflection across the line through `a` and `b`; `None` when they coincide.
    pub fn mirror(a: (f64, f64), b: (f64, f64)) -> Option<Affine2> {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        if !(l2 > 1e-24) || !l2.is_finite() {
            return None;
        }
        let (cc, ss) = ((dx * dx - dy * dy) / l2, 2.0 * dx * dy / l2);
        // R = [[cc, ss], [ss, -cc]] about the point a
        Some(Affine2 {
            m: [cc, ss, ss, -cc],
            t: (a.0 - cc * a.0 - ss * a.1, a.1 - ss * a.0 + cc * a.1),
        })
    }

    /// Uniform scale by `k` about `base`; `None` unless `k` is finite and positive.
    pub fn scale(base: (f64, f64), k: f64) -> Option<Affine2> {
        if !(k > 0.0) || !k.is_finite() {
            return None;
        }
        Some(Affine2 {
            m: [k, 0.0, 0.0, k],
            t: (base.0 * (1.0 - k), base.1 * (1.0 - k)),
        })
    }

    pub fn det(&self) -> f64 {
        self.m[0] * self.m[3] - self.m[1] * self.m[2]
    }

    pub fn is_mirror(&self) -> bool {
        self.det() < 0.0
    }

    /// Length factor.
    pub fn k(&self) -> f64 {
        self.det().abs().sqrt()
    }

    pub fn pt(&self, p: (f64, f64)) -> (f64, f64) {
        (
            self.m[0] * p.0 + self.m[1] * p.1 + self.t.0,
            self.m[2] * p.0 + self.m[3] * p.1 + self.t.1,
        )
    }

    /// Angle of the image of the direction at angle `a`.
    pub fn ang(&self, a: f64) -> f64 {
        let (s, c) = a.sin_cos();
        (self.m[2] * c + self.m[3] * s).atan2(self.m[0] * c + self.m[1] * s)
    }
}

impl Underlay {
    /// `unit` - metres per drawing unit (mm: 0.001). Blocks (INSERT) are expanded with default limits.
    pub fn from_dxf(doc: &DxfDocument, unit: f64) -> Underlay {
        Self::from_dxf_with(doc, unit, &ExpandOptions::default()).0
    }

    /// Unit from `$INSUNITS` (or from the extent); returns it too.
    pub fn from_dxf_auto(doc: &DxfDocument) -> (Underlay, f64) {
        let unit = guess_unit(doc);
        (Self::from_dxf(doc, unit), unit)
    }

    /// Like `from_dxf`, with a configurable block-expansion budget; the report notes truncation.
    pub fn from_dxf_with(
        doc: &DxfDocument,
        unit: f64,
        opt: &ExpandOptions,
    ) -> (Underlay, ExpandReport) {
        let decoded = decoded_names(doc);
        let doc = decoded.as_ref().unwrap_or(doc);
        let (mut u, rep) = Self::build(doc, unit, opt);
        u.blocks = super::blocks::capture(doc, unit);
        (u, rep)
    }

    /// The underlay of a document whose names are already decoded; blocks are not captured.
    pub(super) fn build(
        doc: &DxfDocument,
        unit: f64,
        opt: &ExpandOptions,
    ) -> (Underlay, ExpandReport) {
        let ex = expand_inserts(doc, opt);
        let dashed_layers: HashSet<&str> = doc
            .layers
            .iter()
            .filter(|l| !is_solid(&l.linetype))
            .map(|l| l.name.as_str())
            .collect();
        let dashed = |c: &Common| match c.linetype.as_deref() {
            Some(lt) if !lt.eq_ignore_ascii_case("BYLAYER") => !is_solid(lt),
            _ => dashed_layers.contains(c.layer.as_str()),
        };
        let mut pens = Pens::new(doc, unit);
        let p = |v: [f64; 3]| (v[0] * unit, v[1] * unit);
        let mut u = Underlay::default();
        u.text_styles = doc.styles.clone();
        for e in &ex.entities {
            match e {
                DEntity::Line(l) => {
                    let pen = pens.pen(&l.common);
                    u.line(
                        &l.common.layer,
                        p(l.start),
                        p(l.end),
                        dashed(&l.common),
                        pen,
                    )
                }
                DEntity::LwPolyline(pl) => {
                    let pen = pens.pen(&pl.common);
                    u.lwpoly(pl, unit, dashed(&pl.common), pen)
                }
                DEntity::Circle(ci) => {
                    let Some(flip) = plan_flip(&ci.common) else {
                        continue;
                    };
                    let mut c = p(ci.center);
                    if flip {
                        c.0 = -c.0;
                    }
                    let r = ci.radius * unit;
                    if !(r.is_finite() && r > 0.0 && c.0.is_finite() && c.1.is_finite()) {
                        continue;
                    }
                    let pen = pens.pen(&ci.common);
                    u.arcs.push(UArc {
                        layer: ci.common.layer.clone(),
                        c,
                        r,
                        a0: 0.0,
                        a1: TAU,
                        pen,
                    });
                    u.closed.push(UPoly {
                        layer: ci.common.layer.clone(),
                        pts: crate::dxf::lower::circle_ring(c, r),
                        circle: Some(r),
                    });
                }
                DEntity::Arc(a) => {
                    let Some(flip) = plan_flip(&a.common) else {
                        continue;
                    };
                    // Extrusion (0,0,-1): x is mirrored, angles 180-a, direction reversed.
                    let (s, e) = if flip {
                        (180.0 - a.end_angle, 180.0 - a.start_angle)
                    } else {
                        (a.start_angle, a.end_angle)
                    };
                    let mut c = p(a.center);
                    if flip {
                        c.0 = -c.0;
                    }
                    let a0 = s.to_radians().rem_euclid(TAU);
                    let mut a1 = e.to_radians().rem_euclid(TAU);
                    if a1 <= a0 {
                        a1 += TAU;
                    }
                    let pen = pens.pen(&a.common);
                    u.arcs.push(UArc {
                        layer: a.common.layer.clone(),
                        c,
                        r: a.radius * unit,
                        a0,
                        a1,
                        pen,
                    });
                }
                DEntity::Text(t) => {
                    let pen = pens.pen(&t.common);
                    u.text(
                        &t.common.layer,
                        p(t.pos),
                        text_height(doc, t.height, t.style.as_deref(), unit),
                        t.rotation.to_radians(),
                        &text_plain(&t.text),
                        pen,
                        TextFmt::from_text(t, unit),
                    )
                }
                DEntity::MText(t) => {
                    let pen = pens.pen(&t.common);
                    u.text(
                        &t.common.layer,
                        p(t.pos),
                        text_height(doc, t.height, t.style.as_deref(), unit),
                        t.rotation.to_radians(),
                        &mtext_plain(&t.text),
                        pen,
                        TextFmt::from_mtext(t, unit),
                    )
                }
                DEntity::Unknown(r) => {
                    *u.unknown.entry(r[0].value.clone()).or_default() += 1;
                }
                _ => {}
            }
        }
        let sc = |q: (f64, f64)| (q.0 * unit, q.1 * unit);
        for f in &ex.fills {
            let pen = pens.pen(&f.common);
            u.fills.push(UFill {
                layer: f.common.layer.clone(),
                loops: f
                    .loops
                    .iter()
                    .map(|l| l.iter().map(|q| sc(*q)).collect())
                    .collect(),
                solid: f.solid,
                pattern: f.pattern.clone(),
                segs: f.segs.iter().map(|(a, b)| (sc(*a), sc(*b))).collect(),
                pen,
            });
        }
        u.inserts = ex
            .inserts
            .iter()
            .map(|i| UInsert {
                name: i.name.clone(),
                layer: i.layer.clone(),
                pos: sc(i.pos),
                rotation: i.rotation.to_radians(),
                size: (i.size.0 * unit, i.size.1 * unit),
                center: sc(i.center),
                attribs: i.attribs.clone(),
            })
            .collect();
        u.block_hints = ex
            .hints
            .iter()
            .map(|h| UBlockHint {
                name: h.name.clone(),
                layer: h.layer.clone(),
                kind: h.kind,
                pos: (h.pos.0 * unit, h.pos.1 * unit),
                rotation: h.rotation.to_radians(),
                size: (h.scale.0 * unit, h.scale.1 * unit),
            })
            .collect();
        u.layers = doc
            .layers
            .iter()
            .map(|l| ULayer {
                name: l.name.clone(),
                color: extra_i(&l.extra, 420)
                    .map(true_rgb)
                    .or_else(|| aci_rgb(l.color)),
                off: l.color < 0,
                frozen: l.flags & 1 != 0,
                linetype: l.linetype.clone(),
                weight_mm: extra_i(&l.extra, 370)
                    .filter(|w| *w >= 0)
                    .map_or(0.25, |w| w as f32 / 100.0),
            })
            .collect();
        // layers absent from the table but present in the geometry
        let mut seen: HashSet<String> = u.layers.iter().map(|l| l.name.clone()).collect();
        let used: Vec<String> = u
            .lines
            .iter()
            .map(|l| &l.layer)
            .chain(u.arcs.iter().map(|a| &a.layer))
            .chain(u.texts.iter().map(|t| &t.layer))
            .chain(u.fills.iter().map(|f| &f.layer))
            .cloned()
            .collect();
        for name in used {
            if seen.insert(name.clone()) {
                u.layers.push(ULayer {
                    name,
                    color: None,
                    off: false,
                    frozen: false,
                    linetype: String::new(),
                    weight_mm: 0.25,
                });
            }
        }
        u.pens = pens.pens;
        u.layouts = doc.layouts();
        u.paper_skipped = ex.lower.paper;
        for (k, v) in &ex.lower.unsupported_types {
            *u.unknown.entry(k.clone()).or_default() += v;
        }
        (u, ex.report)
    }

    /// A PDF page at scale `opt.scale` (model units are metres).
    pub fn from_pdf(page: &PageDrawing, opt: &ImportOptions) -> Result<Underlay, ImportError> {
        let mut u = Underlay {
            pens: vec![Pen::default()],
            ..Default::default()
        };
        for e in model_entities(page, opt)? {
            let layer = page.layers.get(e.layer).map_or("PDF", |s| s.as_str());
            match e.geom {
                Geom::Line { a, b } => u.line(layer, (a[0], a[1]), (b[0], b[1]), e.dashed, 0),
                Geom::Polyline {
                    pts,
                    bulges,
                    closed,
                } => {
                    let v: Vec<P> = pts.iter().map(|p| (p[0], p[1])).collect();
                    u.poly(layer, &v, &bulges, closed, e.dashed, 0);
                }
                Geom::Arc { c, r, a0, a1 } => u.arcs.push(UArc {
                    layer: layer.into(),
                    c: (c[0], c[1]),
                    r,
                    a0,
                    a1,
                    pen: 0,
                }),
                Geom::Text {
                    pos,
                    height,
                    rotation,
                    string,
                } => u.text(layer, (pos[0], pos[1]), height, rotation, &string, 0, TextFmt::default()),
            }
        }
        Ok(u)
    }

    /// Extents of all geometry (m).
    pub fn bbox(&self) -> Option<(P, P)> {
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        let mut add = |q: P| {
            lo = (lo.0.min(q.0), lo.1.min(q.1));
            hi = (hi.0.max(q.0), hi.1.max(q.1));
        };
        self.lines.iter().for_each(|l| {
            add(l.a);
            add(l.b)
        });
        self.arcs.iter().for_each(|a| {
            add((a.c.0 - a.r, a.c.1 - a.r));
            add((a.c.0 + a.r, a.c.1 + a.r))
        });
        self.fills
            .iter()
            .flat_map(|f| f.loops.iter().flatten())
            .for_each(|q| add(*q));
        (lo.0 <= hi.0).then_some((lo, hi))
    }

    /// Shift the whole underlay by `d` (m).
    pub fn translate(&mut self, d: P) {
        let m = |q: &mut P| {
            q.0 += d.0;
            q.1 += d.1
        };
        for l in &mut self.lines {
            m(&mut l.a);
            m(&mut l.b);
        }
        self.arcs.iter_mut().for_each(|a| m(&mut a.c));
        self.texts.iter_mut().for_each(|t| m(&mut t.pos));
        self.block_hints.iter_mut().for_each(|h| m(&mut h.pos));
        for c in &mut self.closed {
            c.pts.iter_mut().for_each(&m);
        }
        for f in &mut self.fills {
            f.loops.iter_mut().flatten().for_each(&m);
            for s in &mut f.segs {
                m(&mut s.0);
                m(&mut s.1);
            }
        }
        for i in &mut self.inserts {
            m(&mut i.pos);
            m(&mut i.center);
        }
        self.blocks.transform((0.0, 0.0), 0.0, d);
    }

    /// Rotate by `rot` (rad) about `pivot`, then shift by `d`: the whole underlay.
    pub fn transform(&mut self, pivot: P, rot: f64, d: P) {
        let all = Pick {
            lines: (0..self.lines.len()).collect(),
            arcs: (0..self.arcs.len()).collect(),
            texts: (0..self.texts.len()).collect(),
            closed: (0..self.closed.len()).collect(),
            fills: (0..self.fills.len()).collect(),
        };
        self.transform_pick(&all, pivot, rot, d, false);
        let (s, c) = rot.sin_cos();
        let m = |q: &mut P| {
            let (x, y) = (q.0 - pivot.0, q.1 - pivot.1);
            *q = (pivot.0 + c * x - s * y + d.0, pivot.1 + s * x + c * y + d.1);
        };
        for h in &mut self.block_hints {
            m(&mut h.pos);
            h.rotation += rot;
        }
        for i in &mut self.inserts {
            m(&mut i.pos);
            m(&mut i.center);
            i.rotation += rot;
        }
        self.blocks.transform(pivot, rot, d);
    }

    /// Rotate and shift the selected primitives; `copy` — add transformed copies.
    /// Returns a selection pointing at the result (the copies or the primitives themselves).
    pub fn transform_pick(&mut self, pick: &Pick, pivot: P, rot: f64, d: P, copy: bool) -> Pick {
        let (s, c) = rot.sin_cos();
        let m = |q: &mut P| {
            let (x, y) = (q.0 - pivot.0, q.1 - pivot.1);
            *q = (pivot.0 + c * x - s * y + d.0, pivot.1 + s * x + c * y + d.1);
        };
        Pick {
            lines: apply(&mut self.lines, &pick.lines, copy, &mut |l| {
                m(&mut l.a);
                m(&mut l.b)
            }),
            arcs: apply(&mut self.arcs, &pick.arcs, copy, &mut |a| {
                m(&mut a.c);
                a.a0 += rot;
                a.a1 += rot;
            }),
            texts: apply(&mut self.texts, &pick.texts, copy, &mut |t| {
                m(&mut t.pos);
                t.rotation += rot;
                t.fmt.rotate(s, c);
            }),
            closed: apply(&mut self.closed, &pick.closed, copy, &mut |p| {
                p.pts.iter_mut().for_each(m)
            }),
            fills: apply(&mut self.fills, &pick.fills, copy, &mut |f| {
                f.loops.iter_mut().flatten().for_each(m);
                for sg in &mut f.segs {
                    m(&mut sg.0);
                    m(&mut sg.1);
                }
            }),
        }
    }

    /// Mirror or uniform scale of the picked primitives (`copy`: transformed copies are added).
    /// A mirror turns arcs so they still run counter-clockwise, keeps texts readable and
    /// reverses rings and loops. Returns the pick of the result.
    pub fn transform_pick_affine(&mut self, pick: &Pick, xf: &Affine2, copy: bool) -> Pick {
        let m = |q: &mut P| *q = xf.pt(*q);
        let mir = xf.is_mirror();
        let k = xf.k();
        Pick {
            lines: apply(&mut self.lines, &pick.lines, copy, &mut |l| {
                m(&mut l.a);
                m(&mut l.b)
            }),
            arcs: apply(&mut self.arcs, &pick.arcs, copy, &mut |a| {
                let sweep = a.a1 - a.a0;
                let (n0, n1) = (xf.ang(a.a0), xf.ang(a.a1));
                let s0 = if mir { n1 } else { n0 }.rem_euclid(TAU);
                // rem_euclid can round a tiny negative angle up to TAU
                a.a0 = if s0 >= TAU { 0.0 } else { s0 };
                a.a1 = a.a0 + sweep;
                m(&mut a.c);
                a.r *= k;
            }),
            texts: apply(&mut self.texts, &pick.texts, copy, &mut |t| {
                m(&mut t.pos);
                t.height *= k;
                t.fmt.linear(xf.m, k);
                let mut r = xf.ang(t.rotation);
                if mir && r.cos() < -1e-9 {
                    r += PI;
                }
                t.rotation = r.sin().atan2(r.cos());
            }),
            closed: apply(&mut self.closed, &pick.closed, copy, &mut |p| {
                p.pts.iter_mut().for_each(m);
                if mir {
                    p.pts.reverse();
                }
                if let Some(r) = p.circle.as_mut() {
                    *r *= k;
                }
            }),
            fills: apply(&mut self.fills, &pick.fills, copy, &mut |f| {
                for l in &mut f.loops {
                    l.iter_mut().for_each(m);
                    if mir {
                        l.reverse();
                    }
                }
                for sg in &mut f.segs {
                    m(&mut sg.0);
                    m(&mut sg.1);
                }
            }),
        }
    }

    /// Delete the selected primitives.
    pub fn remove_pick(&mut self, pick: &Pick) {
        fn drop<T>(v: &mut Vec<T>, idx: &[usize]) {
            let set: HashSet<usize> = idx.iter().copied().collect();
            let mut k = 0;
            v.retain(|_| {
                k += 1;
                !set.contains(&(k - 1))
            });
        }
        drop(&mut self.lines, &pick.lines);
        drop(&mut self.arcs, &pick.arcs);
        drop(&mut self.texts, &pick.texts);
        drop(&mut self.closed, &pick.closed);
        drop(&mut self.fills, &pick.fills);
    }

    /// Pen by index (unknown — the default).
    pub fn pen(&self, k: u32) -> Pen {
        self.pens.get(k as usize).cloned().unwrap_or_default()
    }

    fn line(&mut self, layer: &str, a: P, b: P, dashed: bool, pen: u32) {
        if [a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite()) {
            self.lines.push(ULine {
                layer: layer.into(),
                a,
                b,
                dashed,
                pen,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        layer: &str,
        pos: P,
        height: f64,
        rotation: f64,
        text: &str,
        pen: u32,
        fmt: TextFmt,
    ) {
        if pos.0.is_finite() && pos.1.is_finite() && height.is_finite() && rotation.is_finite() {
            self.texts.push(UText {
                layer: layer.into(),
                pos,
                height,
                rotation,
                text: text.into(),
                pen,
                fmt,
            });
        }
    }

    fn lwpoly(&mut self, pl: &LwPolyline, unit: f64, dashed: bool, pen: u32) {
        let Some(flip) = plan_flip(&pl.common) else {
            return;
        };
        let sgn = if flip { -1.0 } else { 1.0 };
        let (pts, bulges, closed) = clean_vertices(
            pl.vertices
                .iter()
                .map(|v| ((sgn * v.x * unit, v.y * unit), v.bulge * sgn)),
            pl.closed(),
        );
        let layer = &pl.common.layer;
        if closed && pts.len() >= 3 {
            let mut ring = vec![];
            for k in 0..pts.len() {
                ring.push(pts[k]);
                let b = bulges[k];
                if b == 0.0 {
                    continue;
                }
                let Some((c, r, a0, sweep)) = bulge_arc(pts[k], pts[(k + 1) % pts.len()], b) else {
                    continue;
                };
                let n = ((sweep.abs() / (PI / 16.0)).ceil() as usize).clamp(2, 256);
                for j in 1..n {
                    let t = a0 + sweep * j as f64 / n as f64;
                    ring.push((c.0 + r * t.cos(), c.1 + r * t.sin()));
                }
            }
            if ring.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
                self.closed.push(UPoly {
                    layer: layer.clone(),
                    pts: ring,
                    circle: None,
                });
            }
        }
        self.poly(layer, &pts, &bulges, closed, dashed, pen);
    }

    /// Polyline: an arc approximation (5+ vertices on one circle) or a set of straight segments;
    /// segments with bulge yield arcs.
    fn poly(
        &mut self,
        layer: &str,
        pts: &[P],
        bulges: &[f64],
        closed: bool,
        dashed: bool,
        pen: u32,
    ) {
        let straight = bulges.iter().all(|&b| b == 0.0);
        if straight && !closed {
            if let Some((c, r, a0, a1)) = arc_from_polyline(pts) {
                self.arcs.push(UArc {
                    layer: layer.into(),
                    c,
                    r,
                    a0,
                    a1,
                    pen,
                });
                return;
            }
        }
        let n = pts.len();
        let m = if closed { n } else { n.saturating_sub(1) };
        for i in 0..m {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let bulge = bulges.get(i).copied().unwrap_or(0.0);
            if bulge == 0.0 {
                self.line(layer, a, b, dashed, pen);
                continue;
            }
            let Some((c, r, a0, sweep)) = bulge_arc(a, b, bulge) else {
                // repeated vertex: no arc
                continue;
            };
            let (s, w) = if sweep >= 0.0 {
                (a0, sweep)
            } else {
                (a0 + sweep, -sweep)
            };
            let s = s.rem_euclid(TAU);
            self.arcs.push(UArc {
                layer: layer.into(),
                c,
                r,
                a0: s,
                a1: s + w,
                pen,
            });
        }
    }
}

/// Arc of a segment with bulge: centre, radius, start angle, signed sweep (rad);
/// `None` for a degenerate chord or non-finite values.
fn bulge_arc(a: P, b: P, bulge: f64) -> Option<(P, f64, f64, f64)> {
    let th = 4.0 * bulge.atan();
    let ch = (b.0 - a.0).hypot(b.1 - a.1);
    if !(ch > 1e-12 && th.abs() > 1e-12 && ch.is_finite() && th.is_finite()) {
        return None;
    }
    let r = ch / (2.0 * (th / 2.0).sin().abs());
    let h = r * (th / 2.0).cos() * th.signum();
    let (nx, ny) = (-(b.1 - a.1) / ch, (b.0 - a.0) / ch);
    let c = ((a.0 + b.0) / 2.0 + nx * h, (a.1 + b.1) / 2.0 + ny * h);
    let a0 = (a.1 - c.1).atan2(a.0 - c.0);
    [c.0, c.1, r, a0]
        .iter()
        .all(|v| v.is_finite())
        .then_some((c, r, a0, th))
}

/// Vertices without repeats (the bulge is taken from the last of the coinciding ones — the segment starts from it) and
/// without non-finite ones; an open polyline with coinciding ends (≥ 3 distinct vertices) counts as closed.
fn clean_vertices(vs: impl Iterator<Item = (P, f64)>, closed: bool) -> (Vec<P>, Vec<f64>, bool) {
    const TOL: f64 = 1e-6;
    let same = |a: P, b: P| (a.0 - b.0).abs() <= TOL && (a.1 - b.1).abs() <= TOL;
    let (mut pts, mut bulges): (Vec<P>, Vec<f64>) = (vec![], vec![]);
    for (p, b) in vs {
        if !(p.0.is_finite() && p.1.is_finite()) {
            continue;
        }
        let b = if b.is_finite() { b } else { 0.0 };
        match pts.last() {
            Some(&q) if same(p, q) => *bulges.last_mut().unwrap() = b,
            _ => {
                pts.push(p);
                bulges.push(b);
            }
        }
    }
    let mut closed = closed;
    if pts.len() >= 4 && same(pts[0], pts[pts.len() - 1]) {
        pts.pop();
        bulges.pop();
        closed = true;
    } else if closed && pts.len() >= 2 && same(pts[0], pts[pts.len() - 1]) {
        pts.pop();
        bulges.pop();
    }
    (pts, bulges, closed)
}

/// A circle from polyline vertices, if all lie on it and run in one direction.
fn arc_from_polyline(pts: &[P]) -> Option<(P, f64, f64, f64)> {
    let n = pts.len();
    if n < 5 || pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    let (a, b, c) = (pts[0], pts[n / 2], pts[n - 1]);
    let d = 2.0 * (a.0 * (b.1 - c.1) + b.0 * (c.1 - a.1) + c.0 * (a.1 - b.1));
    if d.abs() < 1e-12 {
        return None;
    }
    let sq = |p: P| p.0 * p.0 + p.1 * p.1;
    let ux = (sq(a) * (b.1 - c.1) + sq(b) * (c.1 - a.1) + sq(c) * (a.1 - b.1)) / d;
    let uy = (sq(a) * (c.0 - b.0) + sq(b) * (a.0 - c.0) + sq(c) * (b.0 - a.0)) / d;
    let r = (a.0 - ux).hypot(a.1 - uy);
    if !(r.is_finite() && r > 1e-6) {
        return None;
    }
    let mut total = 0.0;
    let first = (a.1 - uy).atan2(a.0 - ux);
    let mut prev = first;
    let mut sign = 0.0;
    for w in pts.windows(2) {
        let (p, q) = (w[0], w[1]);
        let rq = (q.0 - ux).hypot(q.1 - uy);
        if (rq - r).abs() > 0.01 * r + 1e-4 || (q.0 - p.0).hypot(q.1 - p.1) > 0.7 * r {
            return None;
        }
        let ang = (q.1 - uy).atan2(q.0 - ux);
        let mut dlt = ang - prev;
        while dlt > PI {
            dlt -= TAU;
        }
        while dlt < -PI {
            dlt += TAU;
        }
        if dlt == 0.0 || (sign != 0.0 && dlt * sign < 0.0) {
            return None;
        }
        sign = dlt.signum();
        total += dlt;
        prev = ang;
    }
    if total.abs() >= TAU - 0.1 {
        return None;
    }
    let (s, sweep) = if total > 0.0 {
        (first, total)
    } else {
        (prev, -total)
    };
    let a0 = s.rem_euclid(TAU);
    Some(((ux, uy), r, a0, a0 + sweep))
}
