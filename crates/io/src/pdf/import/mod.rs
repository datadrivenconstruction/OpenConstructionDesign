//! Vector PDF import: own file parser, page →
//! 2D entities in sheet mm → model at the chosen scale in one undoable operation.
//! Order:`PdfFile::parse` → `page(i)` → `entities` → `import_page` / `propose_walls`.

mod content;
mod filters;
mod lexer;
mod objects;

use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;
use std::rc::Rc;

use ddc_geom::recognize::{recognize_walls, Options as WallOptions, Recognized, Seg};
use ddc_geom::v2;
use ddc_model::Model;
use rusqlite::params;

use content::{mul, Interp, M};
use lexer::{Dict, Obj};
use objects::Document;

pub type P = [f64; 2];

#[derive(Debug)]
pub enum ImportError {
    NotPdf,
    Encrypted,
    Malformed(String),
    Filter(String),
    Unsupported(String),
    NoPage { index: usize, count: usize },
    Scale(String),
    Model(ddc_model::Error),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPdf => write!(f, "PDF import: not a PDF file (no %PDF- header)"),
            Self::Encrypted => write!(f, "PDF import: encrypted, not supported"),
            Self::Malformed(m) => write!(f, "PDF import: malformed file: {m}"),
            Self::Filter(m) => write!(f, "PDF import: stream filter: {m}"),
            Self::Unsupported(m) => write!(f, "PDF import: unsupported {m}"),
            Self::NoPage { index, count } => {
                write!(
                    f,
                    "PDF import: page {index} out of range (file has {count})"
                )
            }
            Self::Scale(m) => write!(f, "PDF import: scale: {m}"),
            Self::Model(e) => write!(f, "PDF import: model: {e}"),
        }
    }
}
impl std::error::Error for ImportError {}
impl From<ddc_model::Error> for ImportError {
    fn from(e: ddc_model::Error) -> Self {
        Self::Model(e)
    }
}
impl From<rusqlite::Error> for ImportError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Model(e.into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PathSeg {
    Line(P),
    /// Two control points and the end.
    Cubic(P, P, P),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubPath {
    pub start: P,
    pub segs: Vec<PathSeg>,
    pub closed: bool,
}

/// A painted path. Coordinates, width and dash are in sheet mm.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub subpaths: Vec<SubPath>,
    pub stroke: bool,
    pub fill: bool,
    pub rgb: [u8; 3],
    pub width_mm: f64,
    pub dash: Vec<f64>,
    pub layer: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub pos: P,
    pub height_mm: f64,
    /// Degrees counter-clockwise.
    pub rotation_deg: f64,
    pub string: String,
    pub rgb: [u8; 3],
    pub layer: usize,
}

/// Page content in sheet mm (origin bottom-left of the visible area, Y up, /Rotate applied).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageDrawing {
    pub width_mm: f64,
    pub height_mm: f64,
    /// Layer names; indices are `Shape::layer`, `TextRun::layer`.
    pub layers: Vec<String>,
    pub shapes: Vec<Shape>,
    pub texts: Vec<TextRun>,
    /// A budget tripped (operations, points, form depth) — part of the content was skipped.
    pub truncated: bool,
}

struct PageRef {
    dict: Rc<Obj>,
    res: Rc<Obj>,
    bbox: [f64; 4],
    rotate: i64,
}

pub struct PdfFile {
    doc: Document,
    pages: Vec<PageRef>,
}

const MAX_PAGES: usize = 100_000;

fn rect(o: &Obj, doc: &Document) -> Option<[f64; 4]> {
    let a = o.array()?;
    let v: Vec<f64> = a.iter().filter_map(|x| doc.resolve(x).num()).collect();
    let [x0, y0, x1, y1] = v[..] else { return None };
    let r = [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)];
    (r[2] - r[0] > 1e-6 && r[3] - r[1] > 1e-6).then_some(r)
}

impl PdfFile {
    pub fn parse(data: &[u8]) -> Result<Self, ImportError> {
        let doc = Document::parse(data)?;
        let root = doc.get_in(&doc.trailer, b"Root");
        let pages_root = root
            .dict()
            .map(|d| doc.get_in(d, b"Pages"))
            .ok_or_else(|| ImportError::Malformed("catalog is not a dictionary".into()))?;
        let mut f = PdfFile { doc, pages: vec![] };
        let mut seen = HashSet::new();
        let root_ref = root.dict().and_then(|d| d.get(b"Pages")).cloned();
        f.walk(pages_root, root_ref, None, None, None, 0, 0, &mut seen);
        if f.pages.is_empty() {
            return Err(ImportError::Malformed("no pages".into()));
        }
        Ok(f)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        node: Rc<Obj>,
        raw: Option<Obj>,
        res: Option<Rc<Obj>>,
        mbox: Option<[f64; 4]>,
        cbox: Option<[f64; 4]>,
        rot: i64,
        depth: usize,
        seen: &mut HashSet<u32>,
    ) {
        if depth > 64 || self.pages.len() >= MAX_PAGES {
            return;
        }
        if let Some(Obj::Ref(n, _)) = raw {
            if !seen.insert(n) {
                return;
            }
        }
        let Some(d) = node.dict() else { return };
        let doc = &self.doc;
        let r = doc.get_in(d, b"Resources");
        let res = if r.dict().is_some() { Some(r) } else { res };
        let mbox = rect(&doc.get_in(d, b"MediaBox"), doc).or(mbox);
        let cbox = rect(&doc.get_in(d, b"CropBox"), doc).or(cbox);
        let rot = doc.get_in(d, b"Rotate").int().unwrap_or(rot);
        let kids = doc.get_in(d, b"Kids");
        let is_page = doc.get_in(d, b"Type").name() == Some(b"Page");
        match kids.array() {
            Some(k) if !is_page => {
                for kid in k {
                    let node = self.doc.resolve(kid);
                    self.walk(
                        node,
                        Some(kid.clone()),
                        res.clone(),
                        mbox,
                        cbox,
                        rot,
                        depth + 1,
                        seen,
                    );
                }
            }
            _ => {
                let mb = mbox.unwrap_or([0.0, 0.0, 612.0, 792.0]);
                // Visible area: CropBox ∩ MediaBox.
                let bbox = cbox
                    .map(|c| {
                        [
                            c[0].max(mb[0]),
                            c[1].max(mb[1]),
                            c[2].min(mb[2]),
                            c[3].min(mb[3]),
                        ]
                    })
                    .filter(|b| b[2] - b[0] > 1e-6 && b[3] - b[1] > 1e-6)
                    .unwrap_or(mb);
                self.pages.push(PageRef {
                    dict: node.clone(),
                    res: res.unwrap_or_else(|| Rc::new(Obj::Dict(Dict::default()))),
                    bbox,
                    rotate: rot.rem_euclid(360) / 90 * 90,
                });
            }
        }
    }

    /// Total limit of decoded bytes per import.
    pub fn set_decode_budget(&self, bytes: usize) {
        self.doc.set_decode_budget(bytes);
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_ref(&self, i: usize) -> Result<&PageRef, ImportError> {
        self.pages.get(i).ok_or(ImportError::NoPage {
            index: i,
            count: self.pages.len(),
        })
    }

    /// Visible page size in mm (rotation applied).
    pub fn page_size_mm(&self, i: usize) -> Result<(f64, f64), ImportError> {
        let p = self.page_ref(i)?;
        let (w, h) = (p.bbox[2] - p.bbox[0], p.bbox[3] - p.bbox[1]);
        let k = 25.4 / 72.0;
        Ok(if p.rotate % 180 == 0 {
            (w * k, h * k)
        } else {
            (h * k, w * k)
        })
    }

    /// Parses the content of page `i` (zero-based).
    pub fn page(&self, i: usize) -> Result<PageDrawing, ImportError> {
        let p = self.page_ref(i)?;
        let doc = &self.doc;
        let Some(d) = p.dict.dict() else {
            return Err(ImportError::Malformed(format!(
                "page {i} is not a dictionary"
            )));
        };
        let mut data = vec![];
        let c = doc.get_in(d, b"Contents");
        let parts: Vec<Rc<Obj>> = match &*c {
            Obj::Array(a) => a.iter().map(|x| doc.resolve(x)).collect(),
            Obj::Stream(..) => vec![c.clone()],
            _ => vec![],
        };
        for s in parts {
            if let Obj::Stream(sd, raw) = &*s {
                // A broken content part does not ruin the others.
                if let Ok(v) = doc.decode_stream(sd, raw) {
                    // A copy into the shared buffer also spends budget (repeated references).
                    doc.charge(v.len() + 1)?;
                    data.extend_from_slice(&v);
                    data.push(b'\n');
                } else if doc.budget_exhausted() {
                    return Err(ImportError::Filter(
                        "total decoded size budget exceeded".into(),
                    ));
                }
            }
        }
        let [x0, y0, x1, y1] = p.bbox;
        let (w, h) = (x1 - x0, y1 - y0);
        let t: M = [1.0, 0.0, 0.0, 1.0, -x0, -y0];
        let r: M = match p.rotate {
            90 => [0.0, -1.0, 1.0, 0.0, 0.0, w],
            180 => [-1.0, 0.0, 0.0, -1.0, w, h],
            270 => [0.0, 1.0, -1.0, 0.0, h, 0.0],
            _ => content::ID,
        };
        let k = 25.4 / 72.0;
        let base = mul(&mul(&t, &r), &[k, 0.0, 0.0, k, 0.0, 0.0]);
        let (wm, hm) = self.page_size_mm(i)?;
        let out = PageDrawing {
            width_mm: wm,
            height_mm: hm,
            ..Default::default()
        };
        let mut it = Interp::new(doc, out);
        it.run_page(&data, p.res.clone(), base);
        if doc.budget_exhausted() {
            return Err(ImportError::Filter(
                "total decoded size budget exceeded".into(),
            ));
        }
        Ok(it.out)
    }
}

/// Curve recognition tolerances (sheet mm).
#[derive(Clone, Copy, Debug)]
pub struct CurveOptions {
    /// Bezier deviation from a circle: max(abs_tol, rel_tol·r).
    pub arc_abs_tol: f64,
    pub arc_rel_tol: f64,
    /// Chord when splitting non-circular curves into segments.
    pub flatten_tol: f64,
}

impl Default for CurveOptions {
    fn default() -> Self {
        Self {
            arc_abs_tol: 0.01,
            arc_rel_tol: 1e-3,
            flatten_tol: 0.05,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Geom {
    Line {
        a: P,
        b: P,
    },
    /// `bulges[i]` — bulge of segment i → i+1 (tan(angle/4), positive — counter-clockwise).
    Polyline {
        pts: Vec<P>,
        bulges: Vec<f64>,
        closed: bool,
    },
    /// Counter-clockwise from a0 to a1 (rad); a full circle is 0..2π.
    Arc {
        c: P,
        r: f64,
        a0: f64,
        a1: f64,
    },
    /// Rotation in radians.
    Text {
        pos: P,
        height: f64,
        rotation: f64,
        string: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    pub geom: Geom,
    pub layer: usize,
    pub rgb: [u8; 3],
    /// Line width on the sheet, mm (not scaled).
    pub width_mm: f64,
    pub dashed: bool,
    pub filled: bool,
}

#[derive(Clone, Copy, Debug)]
enum Piece {
    Line(P),
    /// Centre, radius, angle (signed), end.
    Arc(P, f64, f64, P),
}

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}
fn dist(a: P, b: P) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}
fn cubic(p0: P, p1: P, p2: P, p3: P, t: f64) -> P {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [
        a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0],
        a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1],
    ]
}
fn signed_angle(c: P, a: P, b: P) -> f64 {
    let (u, v) = (sub(a, c), sub(b, c));
    (u[0] * v[1] - u[1] * v[0]).atan2(u[0] * v[0] + u[1] * v[1])
}
fn seg_dist(p: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
    if l2 < 1e-300 {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0);
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

/// Cubic → arc, if all probes lie on the circle through the ends and the midpoint.
fn fit_arc(p0: P, p1: P, p2: P, p3: P, o: &CurveOptions) -> Option<Piece> {
    let m = cubic(p0, p1, p2, p3, 0.5);
    let (ax, ay) = (p0[0], p0[1]);
    let (bx, by) = (m[0] - ax, m[1] - ay);
    let (cx, cy) = (p3[0] - ax, p3[1] - ay);
    let d = 2.0 * (bx * cy - by * cx);
    let scale = (bx * bx + by * by).max(cx * cx + cy * cy);
    if d.abs() <= 1e-12 * scale || scale < 1e-24 {
        return None;
    }
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let c = [ax + (cy * b2 - by * c2) / d, ay + (bx * c2 - cx * b2) / d];
    let r = dist(c, p0);
    // A nearly flat curve gives a huge r: the tolerance does not grow above abs_tol,
    // and r ≫ chord is rejected.
    let chord = dist(p0, p3).max(dist(p0, m));
    if !r.is_finite() || r > 50.0 * chord {
        return None;
    }
    let tol = o
        .arc_abs_tol
        .max((o.arc_rel_tol * r).min(o.arc_abs_tol * 10.0));
    for i in 1..8 {
        let q = cubic(p0, p1, p2, p3, i as f64 / 8.0);
        if (dist(q, c) - r).abs() > tol {
            return None;
        }
    }
    let sweep = signed_angle(c, p0, m) + signed_angle(c, m, p3);
    if !sweep.is_finite() || sweep.abs() < 1e-9 || !r.is_finite() {
        return None;
    }
    Some(Piece::Arc(c, r, sweep, p3))
}

fn flatten(p0: P, p1: P, p2: P, p3: P, o: &CurveOptions, out: &mut Vec<Piece>) {
    let dd = |a: P, b: P, c: P| (a[0] - 2.0 * b[0] + c[0]).hypot(a[1] - 2.0 * b[1] + c[1]);
    let d = dd(p0, p1, p2).max(dd(p1, p2, p3));
    let n = (0.75 * d / o.flatten_tol.max(1e-9)).sqrt().ceil();
    let n = if n.is_finite() {
        n.clamp(1.0, 256.0) as usize
    } else {
        1
    };
    for i in 1..=n {
        out.push(Piece::Line(cubic(p0, p1, p2, p3, i as f64 / n as f64)));
    }
}

fn norm_angle(a: f64) -> f64 {
    let a = a.rem_euclid(TAU);
    if a >= TAU {
        0.0
    } else {
        a
    }
}

fn arc_geom(c: P, r: f64, start: P, sweep: f64) -> Geom {
    let a = (start[1] - c[1]).atan2(start[0] - c[0]);
    let (a0, a1) = if sweep > 0.0 {
        (a, a + sweep)
    } else {
        (a + sweep, a)
    };
    Geom::Arc {
        c,
        r,
        a0: norm_angle(a0),
        a1: norm_angle(a1),
    }
}

fn subpath_geom(sp: &SubPath, o: &CurveOptions) -> Option<Geom> {
    let mut pieces = vec![];
    let mut cur = sp.start;
    let eps = 1e-9;
    for s in &sp.segs {
        match *s {
            PathSeg::Line(p) => {
                if dist(p, cur) > eps {
                    pieces.push(Piece::Line(p));
                    cur = p;
                }
            }
            PathSeg::Cubic(c1, c2, e) => {
                let flat = seg_dist(c1, cur, e).max(seg_dist(c2, cur, e));
                if flat <= o.arc_abs_tol.min(o.flatten_tol) * 1e-3 {
                    if dist(e, cur) > eps {
                        pieces.push(Piece::Line(e));
                    }
                } else if let Some(a) = fit_arc(cur, c1, c2, e, o) {
                    pieces.push(a);
                } else {
                    flatten(cur, c1, c2, e, o, &mut pieces);
                }
                cur = e;
            }
        }
    }
    if pieces.is_empty() {
        return None;
    }
    let closes = dist(cur, sp.start) <= eps;
    // Circle: all pieces are arcs of one centre and radius, angles sum to 2π, the contour is closed.
    if let Some(Piece::Arc(c, r, _, _)) = pieces.first().copied() {
        let tol = o.arc_abs_tol.max(o.arc_rel_tol * r);
        let mut total = 0.0;
        let same = pieces.iter().all(|p| match *p {
            Piece::Arc(c2, r2, s, _) => {
                total += s;
                dist(c, c2) <= tol && (r - r2).abs() <= tol
            }
            Piece::Line(_) => false,
        });
        if same {
            if closes && (total.abs() - TAU).abs() < 1e-3 {
                let (cs, rs) = pieces
                    .iter()
                    .fold(([0.0, 0.0], 0.0), |(a, rr), p| match *p {
                        Piece::Arc(c2, r2, _, _) => ([a[0] + c2[0], a[1] + c2[1]], rr + r2),
                        _ => (a, rr),
                    });
                let n = pieces.len() as f64;
                return Some(Geom::Arc {
                    c: [cs[0] / n, cs[1] / n],
                    r: rs / n,
                    a0: 0.0,
                    a1: TAU,
                });
            }
            if total.abs() < TAU && !sp.closed {
                return Some(arc_geom(c, r, sp.start, total));
            }
        }
    }
    if pieces.len() == 1 && !sp.closed {
        if let Piece::Line(p) = pieces[0] {
            return Some(Geom::Line { a: sp.start, b: p });
        }
    }
    let mut pts = vec![sp.start];
    let mut bulges = vec![];
    for p in &pieces {
        match *p {
            Piece::Line(e) => {
                bulges.push(0.0);
                pts.push(e);
            }
            Piece::Arc(_, _, s, e) => {
                bulges.push((s / 4.0).tan());
                pts.push(e);
            }
        }
    }
    let closed = sp.closed || (closes && pts.len() > 2);
    if closed {
        if closes {
            pts.pop();
        } else {
            bulges.push(0.0);
        }
    } else {
        bulges.push(0.0);
    }
    if pts.len() < 2 {
        return None;
    }
    Some(Geom::Polyline {
        pts,
        bulges,
        closed,
    })
}

impl PageDrawing {
    /// Page entities in sheet mm. Paths without a stroke (fills) are governed by `fills`.
    pub fn entities(&self, o: &CurveOptions, fills: bool) -> Vec<Entity> {
        let mut out = vec![];
        for sh in &self.shapes {
            if !sh.stroke && !fills {
                continue;
            }
            for sp in &sh.subpaths {
                if let Some(geom) = subpath_geom(sp, o) {
                    out.push(Entity {
                        geom,
                        layer: sh.layer,
                        rgb: sh.rgb,
                        width_mm: sh.width_mm,
                        dashed: !sh.dash.is_empty(),
                        filled: sh.fill,
                    });
                }
            }
        }
        for t in &self.texts {
            out.push(Entity {
                geom: Geom::Text {
                    pos: t.pos,
                    height: t.height_mm,
                    rotation: t.rotation_deg.to_radians(),
                    string: t.string.clone(),
                },
                layer: t.layer,
                rgb: t.rgb,
                width_mm: 0.0,
                dashed: false,
                filled: true,
            });
        }
        out
    }
}

/// Scale: sheet mm → model units (metres).
#[derive(Clone, Copy, Debug)]
pub enum Scale {
    /// 1:N — 1 mm on the sheet = N mm in reality.
    Ratio(f64),
    /// Multiplier sheet mm → model unit.
    Factor(f64),
    /// Two sheet points (mm) and the known model length between them.
    Calibrate { a: P, b: P, length: f64 },
}

impl Scale {
    pub fn factor(&self) -> Result<f64, ImportError> {
        let f = match *self {
            Scale::Ratio(n) => n / 1000.0,
            Scale::Factor(f) => f,
            Scale::Calibrate { a, b, length } => {
                let d = dist(a, b);
                if d.is_nan() || d <= 1e-9 {
                    return Err(ImportError::Scale("calibration points coincide".into()));
                }
                length / d
            }
        };
        if f.is_finite() && f > 0.0 {
            Ok(f)
        } else {
            Err(ImportError::Scale(format!(
                "factor {f} must be finite and positive"
            )))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ImportOptions {
    pub scale: Scale,
    pub curves: CurveOptions,
    /// Import fills without a stroke (hatches, outlined text) as outlines.
    pub fills: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            scale: Scale::Ratio(1.0),
            curves: CurveOptions::default(),
            fills: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub layers: usize,
    pub lines: usize,
    pub polylines: usize,
    pub arcs: usize,
    pub texts: usize,
    pub truncated: bool,
}

fn scaled(g: &Geom, k: f64) -> Geom {
    let s = |p: &P| [p[0] * k, p[1] * k];
    match g {
        Geom::Line { a, b } => Geom::Line { a: s(a), b: s(b) },
        Geom::Polyline {
            pts,
            bulges,
            closed,
        } => Geom::Polyline {
            pts: pts.iter().map(s).collect(),
            bulges: bulges.clone(),
            closed: *closed,
        },
        Geom::Arc { c, r, a0, a1 } => Geom::Arc {
            c: s(c),
            r: r * k,
            a0: *a0,
            a1: *a1,
        },
        Geom::Text {
            pos,
            height,
            rotation,
            string,
        } => Geom::Text {
            pos: s(pos),
            height: height * k,
            rotation: *rotation,
            string: string.clone(),
        },
    }
}

/// Page entities in model units.
pub fn model_entities(page: &PageDrawing, opt: &ImportOptions) -> Result<Vec<Entity>, ImportError> {
    let k = opt.scale.factor()?;
    let mut v = page.entities(&opt.curves, opt.fills);
    for e in &mut v {
        e.geom = scaled(&e.geom, k);
    }
    Ok(v)
}

/// Line weight code (hundredths of mm) from the standard series.
fn lineweight(mm: f64) -> i32 {
    const W: [i32; 24] = [
        0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158,
        200, 211,
    ];
    let h = if mm.is_finite() { mm * 100.0 } else { 25.0 };
    W.iter()
        .copied()
        .min_by(|a, b| (*a as f64 - h).abs().total_cmp(&(*b as f64 - h).abs()))
        .unwrap_or(25)
}

/// Imports a page into the model as one journal operation (`Model::undo` removes everything).
/// Layers with the same name are reused; outside any OCG — the "PDF" layer.
pub fn import_page(
    model: &mut Model,
    page: &PageDrawing,
    opt: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    let ents = model_entities(page, opt)?;
    let rep = model.batch("draw_import_pdf", |db| {
        let mut rep = ImportReport {
            truncated: page.truncated,
            ..Default::default()
        };
        let mut layers: HashMap<String, i64> = HashMap::new();
        {
            let mut st = db.prepare("SELECT name, id FROM d_layer")?;
            for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (n, id) = r?;
                layers.insert(n, id);
            }
        }
        let mut ids = vec![];
        for name in &page.layers {
            let id = match layers.get(name) {
                Some(&id) => id,
                None => {
                    db.prepare_cached(
                        "INSERT INTO d_layer(name,color,linetype,weight) VALUES (?1,7,'CONTINUOUS',0.25)",
                    )?
                    .execute([name])?;
                    rep.layers += 1;
                    let id = db.last_insert_rowid();
                    layers.insert(name.clone(), id);
                    id
                }
            };
            ids.push(id);
        }
        for e in &ents {
            let Some(&layer) = ids.get(e.layer) else { continue };
            let tc = (e.rgb[0] as i64) << 16 | (e.rgb[1] as i64) << 8 | e.rgb[2] as i64;
            let lt: Option<&str> = e.dashed.then_some("DASHED");
            let lw = lineweight(e.width_mm);
            match &e.geom {
                Geom::Line { a, b } => {
                    db.prepare_cached(
                        "INSERT INTO d_line(layer,x1,y1,x2,y2,true_color,linetype,lineweight) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    )?
                    .execute(params![layer, a[0], a[1], b[0], b[1], tc, lt, lw])?;
                    rep.lines += 1;
                }
                Geom::Polyline { pts, bulges, closed } => {
                    db.prepare_cached(
                        "INSERT INTO d_polyline(layer,closed,true_color,linetype,lineweight) \
                         VALUES (?1,?2,?3,?4,?5)",
                    )?
                    .execute(params![layer, closed, tc, lt, lw])?;
                    let id = db.last_insert_rowid();
                    let mut st = db.prepare_cached("INSERT INTO d_vertex VALUES (?1,?2,?3,?4,?5)")?;
                    for (i, p) in pts.iter().enumerate() {
                        let b = bulges.get(i).copied().unwrap_or(0.0);
                        st.execute(params![id, i as i64, p[0], p[1], b])?;
                    }
                    rep.polylines += 1;
                }
                Geom::Arc { c, r, a0, a1 } => {
                    db.prepare_cached(
                        "INSERT INTO d_arc(layer,cx,cy,r,a0,a1,true_color,linetype,lineweight) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    )?
                    .execute(params![layer, c[0], c[1], r, a0, a1, tc, lt, lw])?;
                    rep.arcs += 1;
                }
                Geom::Text {
                    pos,
                    height,
                    rotation,
                    string,
                } => {
                    db.prepare_cached(
                        "INSERT INTO d_text(layer,x,y,height,rotation,content,true_color) \
                         VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    )?
                    .execute(params![layer, pos[0], pos[1], height, rotation, string, tc])?;
                    rep.texts += 1;
                }
            }
        }
        Ok(rep)
    })?;
    Ok(rep)
}

/// Parses the file and imports page `index`.
pub fn import_file(
    model: &mut Model,
    data: &[u8],
    index: usize,
    opt: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    let f = PdfFile::parse(data)?;
    let page = f.page(index)?;
    import_page(model, &page, opt)
}

/// Wall proposal from the page's straight segments (in model units); the model is not changed.
pub fn propose_walls(
    page: &PageDrawing,
    opt: &ImportOptions,
    walls: &WallOptions,
) -> Result<Recognized, ImportError> {
    let mut segs = vec![];
    for e in model_entities(page, opt)? {
        match e.geom {
            Geom::Line { a, b } => segs.push(Seg {
                a: v2(a[0], a[1]),
                b: v2(b[0], b[1]),
            }),
            Geom::Polyline {
                pts,
                bulges,
                closed,
            } => {
                let n = pts.len();
                let m = if closed { n } else { n - 1 };
                for i in 0..m {
                    if bulges.get(i).copied().unwrap_or(0.0) == 0.0 {
                        let (a, b) = (pts[i], pts[(i + 1) % n]);
                        segs.push(Seg {
                            a: v2(a[0], a[1]),
                            b: v2(b[0], b[1]),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok(recognize_walls(&segs, walls))
}

#[cfg(test)]
mod tests;
