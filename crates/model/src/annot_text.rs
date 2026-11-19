//! View annotations (schema 34): text with leaders, detail lines and regions, detail components, elevation and
//! coordinate marks, revision clouds. An `anno` row belongs to one view (level plan,
//! section/elevation, legend); coordinates are view coordinates. Region and line geometry is in model
//! metres; text heights, arrows, hatch spacing and dash patterns are in sheet mm and are multiplied by the
//! view scale ([`layout`]). Every action is one journal operation.

use std::collections::BTreeMap;

use ddc_geom::slope::Plane;
use ddc_geom::v2;
use rusqlite::{params, types::Value as Sql, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::views::ViewPrim;
use crate::{Error, Model};

mod batch;
mod drafting;
mod layout;
mod legend;
#[cfg(test)]
mod tests;

pub use batch::AnnoBatchItem;
pub use drafting::DraftingDef;
pub use layout::{dash_segments, stroke_hit, strokes_bbox, AnnoCtx, AnnoStroke};
pub use legend::{LegendDef, LegendPlace, LegendRow, LegendSwatch};

type P = (f64, f64);

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS anno_text_style(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    height_mm REAL NOT NULL, bold INTEGER NOT NULL, underline INTEGER NOT NULL, frame INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS anno_line_style(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    weight_mm REAL NOT NULL, dash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS anno_detail_def(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS anno(id INTEGER PRIMARY KEY, view_kind TEXT NOT NULL, view_id INTEGER NOT NULL,
    kind TEXT NOT NULL, text_style INTEGER, line_style INTEGER, data TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS anno_view ON anno(view_kind, view_id);
CREATE TABLE IF NOT EXISTS anno_view_scale(view_kind TEXT NOT NULL, view_id INTEGER NOT NULL,
    scale REAL NOT NULL, PRIMARY KEY(view_kind, view_id));
CREATE TABLE IF NOT EXISTS anno_legend(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    scale REAL NOT NULL);
CREATE TABLE IF NOT EXISTS anno_legend_row(legend INTEGER NOT NULL, idx INTEGER NOT NULL,
    swatch TEXT NOT NULL, label TEXT NOT NULL, PRIMARY KEY(legend, idx));
CREATE TABLE IF NOT EXISTS anno_legend_place(id INTEGER PRIMARY KEY, sheet INTEGER NOT NULL,
    legend INTEGER NOT NULL, x REAL NOT NULL, y REAL NOT NULL);
CREATE TABLE IF NOT EXISTS anno_drafting(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    scale REAL NOT NULL);
";

/// (name, height mm, bold, underlined, frame)
const TEXT_SEED: [(&str, f64, bool, bool, bool); 5] = [
    ("text_2_5", 2.5, false, false, false),
    ("text_3_5", 3.5, false, false, false),
    ("text_5", 5.0, false, false, false),
    ("title_7", 7.0, true, true, false),
    ("note_frame", 2.5, false, false, true),
];
/// (name, thickness mm, dash pattern mm)
const LINE_SEED: [(&str, f64, &str); 7] = [
    ("thin", 0.18, ""),
    ("medium", 0.35, ""),
    ("wide", 0.7, ""),
    ("hidden", 0.25, "2 1"),
    ("center", 0.18, "6 1 1 1"),
    ("phantom", 0.18, "8 1 1 1 1 1"),
    ("demolition", 0.35, "1 1"),
];

fn table_exists(db: &Connection, name: &str) -> Result<bool, Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Tables and builtin styles; merge order with other schema steps does not matter.
/// Each builtin set is seeded only when its table is created, also by an upgrade from a file
/// older than the table: a style or detail the user deleted does not come back on open.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    let text = !table_exists(db, "anno_text_style")?;
    let line = !table_exists(db, "anno_line_style")?;
    let detail = !table_exists(db, "anno_detail_def")?;
    db.execute_batch(DDL)?;
    for (name, h, b, u, f) in TEXT_SEED.into_iter().filter(|_| text) {
        db.execute(
            "INSERT OR IGNORE INTO anno_text_style(name,height_mm,bold,underline,frame)
             VALUES (?1,?2,?3,?4,?5)",
            params![name, h, b, u, f],
        )?;
    }
    for (name, w, dash) in LINE_SEED.into_iter().filter(|_| line) {
        db.execute(
            "INSERT OR IGNORE INTO anno_line_style(name,weight_mm,dash) VALUES (?1,?2,?3)",
            params![name, w, dash],
        )?;
    }
    for d in builtin_details().into_iter().filter(|_| detail) {
        db.execute(
            "INSERT OR IGNORE INTO anno_detail_def(name,data) VALUES (?1,?2)",
            params![d.name, def_json(&d).to_string()],
        )?;
    }
    Ok(())
}

/// The view that owns the annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnnoView {
    Plan(i64),
    Section(i64),
    Legend(i64),
    /// Drafting view: a named 2D canvas with annotation only.
    Drafting(i64),
}

impl AnnoView {
    fn code(self) -> (&'static str, i64) {
        match self {
            AnnoView::Plan(i) => ("plan", i),
            AnnoView::Section(i) => ("section", i),
            AnnoView::Legend(i) => ("legend", i),
            AnnoView::Drafting(i) => ("drafting", i),
        }
    }
    fn parse(kind: &str, id: i64) -> Option<Self> {
        Some(match kind {
            "plan" => AnnoView::Plan(id),
            "section" => AnnoView::Section(id),
            "legend" => AnnoView::Legend(id),
            "drafting" => AnnoView::Drafting(id),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaderHead {
    Arrow,
    Dot,
    Tick,
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Leader {
    pub to: P,
    pub elbow: Option<P>,
}

/// Free text: `pos` is the top of the lines per alignment; lines are separated by `\n`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextNote {
    pub pos: P,
    pub rot_deg: f64,
    pub text: String,
    pub align: TextAlign,
    /// Wrap width, sheet mm; None means explicit line breaks only.
    pub wrap_mm: Option<f64>,
    pub head: LeaderHead,
    pub leaders: Vec<Leader>,
}

impl TextNote {
    pub fn new(pos: P, text: &str) -> Self {
        Self {
            pos,
            rot_deg: 0.0,
            text: text.to_string(),
            align: TextAlign::Left,
            wrap_mm: None,
            head: LeaderHead::Arrow,
            leaders: vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionFill {
    /// Fill: 0 is black, 255 is white.
    Solid { gray: u8 },
    /// Masking region: covers everything beneath it with the sheet colour.
    Mask,
    /// Hatch: angle, spacing in sheet mm, cross-hatched.
    Hatch { deg: f64, step_mm: f64, cross: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpotKind {
    Elevation,
    Coordinate,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnnoShape {
    Text(TextNote),
    Line {
        pts: Vec<P>,
        closed: bool,
    },
    Region {
        /// The first ring is the outline, the rest are holes (even-odd).
        rings: Vec<Vec<P>>,
        fill: RegionFill,
        boundary: bool,
    },
    Detail {
        def: i64,
        at: P,
        rot_deg: f64,
        /// Length of a stretched or repeated component, m; None means the definition's length.
        length: Option<f64>,
        flip: bool,
    },
    Spot {
        kind: SpotKind,
        at: P,
        head: P,
    },
    Cloud {
        ring: Vec<P>,
        mark: String,
    },
}

fn pj(p: P) -> Value {
    json!([p.0, p.1])
}
fn jp(v: &Value) -> Option<P> {
    Some((v.get(0)?.as_f64()?, v.get(1)?.as_f64()?))
}
fn psj(v: &[P]) -> Value {
    Value::Array(v.iter().map(|p| pj(*p)).collect())
}
fn jps(v: &Value) -> Option<Vec<P>> {
    v.as_array()?.iter().map(jp).collect()
}

fn fill_json(f: RegionFill) -> Value {
    match f {
        RegionFill::Solid { gray } => json!({"k": "solid", "gray": gray}),
        RegionFill::Mask => json!({"k": "mask"}),
        RegionFill::Hatch {
            deg,
            step_mm,
            cross,
        } => {
            json!({"k": "hatch", "deg": deg, "step": step_mm, "cross": cross})
        }
    }
}

fn json_fill(v: &Value) -> Option<RegionFill> {
    Some(match v["k"].as_str()? {
        "solid" => RegionFill::Solid {
            gray: v["gray"].as_u64()?.min(255) as u8,
        },
        "mask" => RegionFill::Mask,
        "hatch" => RegionFill::Hatch {
            deg: v["deg"].as_f64()?,
            step_mm: v["step"].as_f64()?,
            cross: v["cross"].as_bool()?,
        },
        _ => return None,
    })
}

/// Annotation transform by an edit tool (view coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnnoXf {
    Move(P),
    /// Rotation by `a` radians about `o`.
    Rotate {
        o: P,
        a: f64,
    },
    /// Reflection about the line through `a` and `b`.
    Mirror {
        a: P,
        b: P,
    },
    /// Uniform scale by `k` about `o`: points move, text and symbol sizes stay. A factor that
    /// is not positive gives no finite point, so the shape check refuses it.
    Scale {
        o: P,
        k: f64,
    },
}

impl AnnoXf {
    fn point(self, p: P) -> P {
        match self {
            AnnoXf::Move(d) => (p.0 + d.0, p.1 + d.1),
            AnnoXf::Rotate { o, a } => {
                let (s, c) = a.sin_cos();
                let (x, y) = (p.0 - o.0, p.1 - o.1);
                (o.0 + x * c - y * s, o.1 + x * s + y * c)
            }
            AnnoXf::Mirror { a, b } => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy);
                let (fx, fy) = (a.0 + t * dx, a.1 + t * dy);
                (2.0 * fx - p.0, 2.0 * fy - p.1)
            }
            AnnoXf::Scale { o, k } => {
                if !(k.is_finite() && k > 0.0) {
                    return (f64::NAN, f64::NAN);
                }
                (o.0 + (p.0 - o.0) * k, o.1 + (p.1 - o.1) * k)
            }
        }
    }

    /// Image of direction `deg`, degrees in [-180, 180).
    fn angle(self, deg: f64) -> f64 {
        let d = match self {
            AnnoXf::Move(_) | AnnoXf::Scale { .. } => deg,
            AnnoXf::Rotate { a, .. } => deg + a.to_degrees(),
            AnnoXf::Mirror { a, b } => 2.0 * (b.1 - a.1).atan2(b.0 - a.0).to_degrees() - deg,
        };
        norm_deg(d)
    }
}

fn norm_deg(d: f64) -> f64 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

impl AnnoShape {
    fn code(&self) -> &'static str {
        match self {
            AnnoShape::Text(_) => "text",
            AnnoShape::Line { .. } => "line",
            AnnoShape::Region { .. } => "region",
            AnnoShape::Detail { .. } => "detail",
            AnnoShape::Spot { .. } => "spot",
            AnnoShape::Cloud { .. } => "cloud",
        }
    }

    /// Draw order: regions under lines, labels on top.
    fn order(&self) -> u8 {
        match self {
            AnnoShape::Region { .. } => 0,
            AnnoShape::Detail { .. } => 1,
            AnnoShape::Line { .. } => 2,
            AnnoShape::Cloud { .. } => 3,
            AnnoShape::Spot { .. } => 4,
            AnnoShape::Text(_) => 5,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            AnnoShape::Text(t) => json!({
                "pos": pj(t.pos),
                "rot": t.rot_deg,
                "text": t.text,
                "align": match t.align {
                    TextAlign::Left => "l",
                    TextAlign::Center => "c",
                    TextAlign::Right => "r",
                },
                "wrap": t.wrap_mm,
                "head": match t.head {
                    LeaderHead::Arrow => "arrow",
                    LeaderHead::Dot => "dot",
                    LeaderHead::Tick => "tick",
                    LeaderHead::None => "none",
                },
                "leaders": t.leaders.iter().map(|l| json!({
                    "to": pj(l.to),
                    "elbow": l.elbow.map(pj),
                })).collect::<Vec<_>>(),
            }),
            AnnoShape::Line { pts, closed } => json!({"pts": psj(pts), "closed": closed}),
            AnnoShape::Region {
                rings,
                fill,
                boundary,
            } => json!({
                "rings": rings.iter().map(|r| psj(r)).collect::<Vec<_>>(),
                "fill": fill_json(*fill),
                "boundary": boundary,
            }),
            AnnoShape::Detail {
                def,
                at,
                rot_deg,
                length,
                flip,
            } => json!({"def": def, "at": pj(*at), "rot": rot_deg, "len": length, "flip": flip}),
            AnnoShape::Spot { kind, at, head } => json!({
                "k": if *kind == SpotKind::Elevation { "elev" } else { "coord" },
                "at": pj(*at),
                "head": pj(*head),
            }),
            AnnoShape::Cloud { ring, mark } => json!({"ring": psj(ring), "mark": mark}),
        }
    }

    fn from_json(kind: &str, v: &Value) -> Option<Self> {
        Some(match kind {
            "text" => AnnoShape::Text(TextNote {
                pos: jp(&v["pos"])?,
                rot_deg: v["rot"].as_f64()?,
                text: v["text"].as_str()?.to_string(),
                align: match v["align"].as_str()? {
                    "c" => TextAlign::Center,
                    "r" => TextAlign::Right,
                    _ => TextAlign::Left,
                },
                wrap_mm: v["wrap"].as_f64(),
                head: match v["head"].as_str()? {
                    "dot" => LeaderHead::Dot,
                    "tick" => LeaderHead::Tick,
                    "none" => LeaderHead::None,
                    _ => LeaderHead::Arrow,
                },
                leaders: v["leaders"]
                    .as_array()?
                    .iter()
                    .map(|l| {
                        Some(Leader {
                            to: jp(&l["to"])?,
                            elbow: jp(&l["elbow"]),
                        })
                    })
                    .collect::<Option<_>>()?,
            }),
            "line" => AnnoShape::Line {
                pts: jps(&v["pts"])?,
                closed: v["closed"].as_bool()?,
            },
            "region" => AnnoShape::Region {
                rings: v["rings"]
                    .as_array()?
                    .iter()
                    .map(jps)
                    .collect::<Option<_>>()?,
                fill: json_fill(&v["fill"])?,
                boundary: v["boundary"].as_bool()?,
            },
            "detail" => AnnoShape::Detail {
                def: v["def"].as_i64()?,
                at: jp(&v["at"])?,
                rot_deg: v["rot"].as_f64()?,
                length: v["len"].as_f64(),
                flip: v["flip"].as_bool()?,
            },
            "spot" => AnnoShape::Spot {
                kind: if v["k"].as_str()? == "elev" {
                    SpotKind::Elevation
                } else {
                    SpotKind::Coordinate
                },
                at: jp(&v["at"])?,
                head: jp(&v["head"])?,
            },
            "cloud" => AnnoShape::Cloud {
                ring: jps(&v["ring"])?,
                mark: v["mark"].as_str()?.to_string(),
            },
            _ => return None,
        })
    }

    fn points_mut(&mut self) -> Vec<&mut P> {
        match self {
            AnnoShape::Text(t) => {
                let mut v = vec![&mut t.pos];
                for l in &mut t.leaders {
                    v.push(&mut l.to);
                    if let Some(e) = &mut l.elbow {
                        v.push(e);
                    }
                }
                v
            }
            AnnoShape::Line { pts, .. } => pts.iter_mut().collect(),
            AnnoShape::Region { rings, .. } => rings.iter_mut().flatten().collect(),
            AnnoShape::Detail { at, .. } => vec![at],
            AnnoShape::Spot { at, head, .. } => vec![at, head],
            AnnoShape::Cloud { ring, .. } => ring.iter_mut().collect(),
        }
    }

    /// Anchor points (grips) in a stable order.
    pub fn grips(&self) -> Vec<P> {
        let mut c = self.clone();
        c.points_mut().into_iter().map(|p| *p).collect()
    }

    /// The same shape with grip `i` at point `p`.
    pub fn with_grip(&self, i: usize, p: P) -> Self {
        let mut c = self.clone();
        if let Some(q) = c.points_mut().into_iter().nth(i) {
            *q = p;
        }
        c
    }

    pub fn translated(&self, d: P) -> Self {
        let mut c = self.clone();
        for q in c.points_mut() {
            *q = (q.0 + d.0, q.1 + d.1);
        }
        c
    }

    /// The same shape after a transform. Text stays readable under reflection: a 180° turn
    /// flips the alignment side so the label runs to the reflected side of the insertion point.
    pub fn transformed(&self, x: AnnoXf) -> Self {
        if let AnnoXf::Move(d) = x {
            return self.translated(d);
        }
        let mut c = self.clone();
        for q in c.points_mut() {
            *q = x.point(*q);
        }
        let mirror = matches!(x, AnnoXf::Mirror { .. });
        match &mut c {
            AnnoShape::Text(t) => {
                t.rot_deg = x.angle(t.rot_deg);
                if mirror && (t.rot_deg > 90.0 || t.rot_deg <= -90.0) {
                    t.rot_deg = norm_deg(t.rot_deg + 180.0);
                    t.align = match t.align {
                        TextAlign::Left => TextAlign::Right,
                        TextAlign::Center => TextAlign::Center,
                        TextAlign::Right => TextAlign::Left,
                    };
                }
            }
            AnnoShape::Region {
                fill: RegionFill::Hatch { deg, .. },
                ..
            } => *deg = x.angle(*deg),
            AnnoShape::Detail { rot_deg, flip, .. } => {
                // reflection = rotation by 2α after a flip: R(2α)·F·R(r) = R(2α−r)·F
                *rot_deg = x.angle(*rot_deg);
                *flip ^= mirror;
            }
            _ => {}
        }
        c
    }

    fn check(&self) -> Result<(), Error> {
        let mut c = self.clone();
        if c.points_mut()
            .iter()
            .any(|p| !p.0.is_finite() || !p.1.is_finite())
        {
            return Err(Error::Invalid("annotation point is not finite".into()));
        }
        // NaN would go into JSON as null and the row would become unreadable
        let finite = match self {
            AnnoShape::Text(t) => t.rot_deg.is_finite() && t.wrap_mm.map_or(true, f64::is_finite),
            AnnoShape::Detail {
                rot_deg, length, ..
            } => rot_deg.is_finite() && length.map_or(true, f64::is_finite),
            AnnoShape::Region {
                fill: RegionFill::Hatch { deg, .. },
                ..
            } => deg.is_finite(),
            _ => true,
        };
        if !finite {
            return Err(Error::Invalid("annotation value is not finite".into()));
        }
        let bad = |m: &str| Err(Error::Invalid(m.into()));
        match self {
            AnnoShape::Text(t) if t.text.trim().is_empty() => bad("text is empty"),
            AnnoShape::Line { pts, closed } if pts.len() < 2 + usize::from(*closed) => {
                bad("detail line needs more points")
            }
            AnnoShape::Region { rings, fill, .. } => {
                if rings.is_empty() || rings.iter().any(|r| r.len() < 3) {
                    return bad("region needs a closed contour");
                }
                match fill {
                    RegionFill::Hatch { step_mm, .. }
                        if !step_mm.is_finite() || *step_mm <= 0.0 =>
                    {
                        bad("hatch step must be positive")
                    }
                    _ => Ok(()),
                }
            }
            AnnoShape::Cloud { ring, .. } if ring.len() < 3 => bad("cloud needs a closed contour"),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub id: i64,
    pub name: String,
    pub height_mm: f64,
    pub bold: bool,
    pub underline: bool,
    pub frame: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            height_mm: 2.5,
            bold: false,
            underline: false,
            frame: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineStyle {
    pub id: i64,
    pub name: String,
    pub weight_mm: f64,
    /// Dash, gap, ... in sheet mm; empty means solid.
    pub dash: Vec<f64>,
}

impl Default for LineStyle {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            weight_mm: 0.18,
            dash: vec![],
        }
    }
}

/// How a detail component fits a given length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailFit {
    Fixed,
    Stretch,
    Repeat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetailStroke {
    pub pts: Vec<P>,
    pub closed: bool,
    pub fill: bool,
}

/// A 2D detail component in metres (1:1); the X axis runs along the component.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailDef {
    pub id: i64,
    pub name: String,
    pub length: f64,
    pub fit: DetailFit,
    pub strokes: Vec<DetailStroke>,
}

fn def_json(d: &DetailDef) -> Value {
    json!({
        "len": d.length,
        "fit": match d.fit {
            DetailFit::Fixed => "fixed",
            DetailFit::Stretch => "stretch",
            DetailFit::Repeat => "repeat",
        },
        "strokes": d.strokes.iter().map(|s| json!({
            "pts": psj(&s.pts),
            "closed": s.closed,
            "fill": s.fill,
        })).collect::<Vec<_>>(),
    })
}

fn json_def(id: i64, name: String, v: &Value) -> Option<DetailDef> {
    Some(DetailDef {
        id,
        name,
        length: v["len"].as_f64()?,
        fit: match v["fit"].as_str()? {
            "stretch" => DetailFit::Stretch,
            "repeat" => DetailFit::Repeat,
            _ => DetailFit::Fixed,
        },
        strokes: v["strokes"]
            .as_array()?
            .iter()
            .map(|s| {
                Some(DetailStroke {
                    pts: jps(&s["pts"])?,
                    closed: s["closed"].as_bool()?,
                    fill: s["fill"].as_bool()?,
                })
            })
            .collect::<Option<_>>()?,
    })
}

fn stroke(pts: &[P], closed: bool, fill: bool) -> DetailStroke {
    DetailStroke {
        pts: pts.to_vec(),
        closed,
        fill,
    }
}

fn builtin_details() -> Vec<DetailDef> {
    let def = |name: &str, length, fit, strokes| DetailDef {
        id: 0,
        name: name.to_string(),
        length,
        fit,
        strokes,
    };
    let (bf, tf, tw, h) = (0.1, 0.0085, 0.0056, 0.2);
    let dot: Vec<P> = (0..12)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 12.0;
            (0.01 * a.cos(), 0.01 * a.sin())
        })
        .collect();
    vec![
        def(
            "break_line",
            1.0,
            DetailFit::Stretch,
            vec![stroke(
                &[
                    (0.0, 0.0),
                    (0.46, 0.0),
                    (0.48, 0.06),
                    (0.52, -0.06),
                    (0.54, 0.0),
                    (1.0, 0.0),
                ],
                false,
                false,
            )],
        ),
        def(
            "insulation",
            0.1,
            DetailFit::Repeat,
            vec![stroke(
                &[(0.0, -0.05), (0.05, 0.05), (0.1, -0.05)],
                false,
                false,
            )],
        ),
        def(
            "i_beam_200",
            0.1,
            DetailFit::Fixed,
            vec![stroke(
                &[
                    (0.0, -h / 2.0),
                    (bf, -h / 2.0),
                    (bf, -h / 2.0 + tf),
                    (bf / 2.0 + tw / 2.0, -h / 2.0 + tf),
                    (bf / 2.0 + tw / 2.0, h / 2.0 - tf),
                    (bf, h / 2.0 - tf),
                    (bf, h / 2.0),
                    (0.0, h / 2.0),
                    (0.0, h / 2.0 - tf),
                    (bf / 2.0 - tw / 2.0, h / 2.0 - tf),
                    (bf / 2.0 - tw / 2.0, -h / 2.0 + tf),
                    (0.0, -h / 2.0 + tf),
                ],
                true,
                false,
            )],
        ),
        def(
            "angle_100",
            0.1,
            DetailFit::Fixed,
            vec![stroke(
                &[
                    (0.0, 0.0),
                    (0.1, 0.0),
                    (0.1, 0.01),
                    (0.01, 0.01),
                    (0.01, 0.1),
                    (0.0, 0.1),
                ],
                true,
                false,
            )],
        ),
        def(
            "timber_100",
            0.1,
            DetailFit::Fixed,
            vec![
                stroke(
                    &[(0.0, -0.05), (0.1, -0.05), (0.1, 0.05), (0.0, 0.05)],
                    true,
                    false,
                ),
                stroke(&[(0.0, -0.05), (0.1, 0.05)], false, false),
                stroke(&[(0.0, 0.05), (0.1, -0.05)], false, false),
            ],
        ),
        def(
            "brick_250",
            0.25,
            DetailFit::Repeat,
            vec![stroke(
                &[(0.0, 0.0), (0.25, 0.0), (0.25, 0.065), (0.0, 0.065)],
                true,
                false,
            )],
        ),
        def(
            "anchor_bolt",
            0.2,
            DetailFit::Fixed,
            vec![
                stroke(&[(0.0, 0.0), (0.2, 0.0)], false, false),
                stroke(&[(0.0, -0.015), (0.0, 0.015)], false, false),
                stroke(&[(0.2, 0.0), (0.2, -0.03)], false, false),
            ],
        ),
        def(
            "rebar_dot",
            0.0,
            DetailFit::Fixed,
            vec![stroke(&dot, true, true)],
        ),
    ]
}

/// Mark values at its point: read on every view build and follow the model.
/// `z` is from the project zero, `north`/`east` are from the site base point in its axes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpotValue {
    pub z: f64,
    pub north: f64,
    pub east: f64,
    /// No surface under the point: `z` is the level elevation or the height in a section, and the label gets a "~" prefix.
    pub free: bool,
}

/// An annotation with the styles and data needed to draw it without consulting the model.
#[derive(Clone, Debug, PartialEq)]
pub struct Anno {
    pub id: i64,
    pub view: AnnoView,
    pub shape: AnnoShape,
    pub text_style: Option<i64>,
    pub line_style: Option<i64>,
    pub text: TextStyle,
    pub line: LineStyle,
    pub detail: Option<DetailDef>,
    pub spot: Option<SpotValue>,
}

pub(super) struct Styles {
    text: BTreeMap<i64, TextStyle>,
    line: BTreeMap<i64, LineStyle>,
    defs: BTreeMap<i64, DetailDef>,
}

fn parse_dash(s: &str) -> Vec<f64> {
    s.split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect()
}

fn dash_text(d: &[f64]) -> String {
    d.iter()
        .map(|x| format!("{x:?}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_styles_in(db: &Connection) -> Result<Vec<TextStyle>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id,name,height_mm,bold,underline,frame FROM anno_text_style ORDER BY id",
    )?;
    let v = st
        .query_map([], |r| {
            Ok(TextStyle {
                id: r.get(0)?,
                name: r.get(1)?,
                height_mm: r.get(2)?,
                bold: r.get(3)?,
                underline: r.get(4)?,
                frame: r.get(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn line_styles_in(db: &Connection) -> Result<Vec<LineStyle>, Error> {
    let mut st =
        db.prepare_cached("SELECT id,name,weight_mm,dash FROM anno_line_style ORDER BY id")?;
    let v = st
        .query_map([], |r| {
            Ok(LineStyle {
                id: r.get(0)?,
                name: r.get(1)?,
                weight_mm: r.get(2)?,
                dash: parse_dash(&r.get::<_, String>(3)?),
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn defs_in(db: &Connection) -> Result<Vec<DetailDef>, Error> {
    let mut st = db.prepare_cached("SELECT id,name,data FROM anno_detail_def ORDER BY id")?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(id, name, data)| {
            let v: Value = serde_json::from_str(&data)?;
            json_def(id, name, &v).ok_or_else(|| Error::Invalid(format!("bad detail def {id}")))
        })
        .collect()
}

impl Styles {
    pub(super) fn load(db: &Connection) -> Result<Self, Error> {
        Ok(Self {
            text: text_styles_in(db)?.into_iter().map(|s| (s.id, s)).collect(),
            line: line_styles_in(db)?.into_iter().map(|s| (s.id, s)).collect(),
            defs: defs_in(db)?.into_iter().map(|d| (d.id, d)).collect(),
        })
    }

    /// A row into a drawable annotation (a deleted or unset style falls back to the default style).
    pub(super) fn resolve(
        &self,
        m: &Model,
        id: i64,
        view: AnnoView,
        shape: AnnoShape,
        text_style: Option<i64>,
        line_style: Option<i64>,
    ) -> Result<Anno, Error> {
        let detail = match &shape {
            AnnoShape::Detail { def, .. } => self.defs.get(def).cloned(),
            _ => None,
        };
        let spot = match &shape {
            AnnoShape::Spot { at, .. } => Some(spot_value(m, view, *at)?),
            _ => None,
        };
        Ok(Anno {
            id,
            view,
            shape,
            text_style,
            line_style,
            text: text_style
                .and_then(|i| self.text.get(&i).cloned())
                .unwrap_or_default(),
            line: line_style
                .and_then(|i| self.line.get(&i).cloned())
                .unwrap_or_default(),
            detail,
            spot,
        })
    }
}

fn inside(p: P, rings: &[Vec<P>]) -> bool {
    let mut odd = false;
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
                odd = !odd;
            }
        }
    }
    odd
}

/// A shaft cuts a slab whose top is `z` if it passes through it rather than starting on it.
fn in_shaft(db: &Connection, at: P, z: f64) -> Result<bool, Error> {
    Ok(crate::shafts::shaft_rings(db, z - 0.01, z)?
        .iter()
        .any(|(_, r)| inside(at, &[r.iter().map(|q| (q.x, q.y)).collect()])))
}

/// Tops of slabs and roofs above a point: of level `level` or (None) of the whole model.
fn built_tops(db: &Connection, level: Option<i64>, at: P) -> Result<Vec<f64>, Error> {
    let slabs: Vec<(i64, f64, Option<String>)> = {
        let mut st = db.prepare_cached(
            "SELECT s.id, l.elevation + s.offset, s.slope FROM slab s JOIN level l ON l.id=s.level
             WHERE ?1 IS NULL OR s.level=?1",
        )?;
        let v = st
            .query_map([level], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for (id, base, slope) in slabs {
        if !inside(at, &crate::slabs::rings(db, id)?) {
            continue;
        }
        let plane: Option<Plane> = slope.map(|s| crate::slabs::parse_slope(&s)).transpose()?;
        let z = base + plane.map_or(0.0, |p| p.rise(v2(at.0, at.1)));
        if !in_shaft(db, at, z)? {
            out.push(z);
        }
    }
    let roofs: Vec<i64> = {
        let mut st = db.prepare_cached("SELECT id FROM roof WHERE ?1 IS NULL OR level=?1")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    for id in roofs {
        // a faulty roof shows up in the model check and must not blank the whole view
        let top = crate::roofs::load(db, id)
            .and_then(|s| crate::roofs::cut_geometry(db, id, &s))
            .ok()
            .and_then(|g| g.top_at(v2(at.0, at.1)));
        out.extend(top);
    }
    Ok(out)
}

fn topo_tops(m: &Model, at: P) -> Result<Vec<f64>, Error> {
    let mut out = vec![];
    for (id, _) in m.toposolids()? {
        if let Ok(b) = m.topo_built_shared(id) {
            out.extend(b.tin.z_at(v2(at.0, at.1)));
        }
    }
    Ok(out)
}

/// Mark on a plan: the highest top of the level, otherwise the terrain.
fn plan_surface(m: &Model, level: i64, at: P) -> Result<Option<f64>, Error> {
    let highest = |v: Vec<f64>| v.into_iter().reduce(f64::max);
    Ok(match highest(built_tops(&m.db, Some(level), at)?) {
        Some(z) => Some(z),
        None => highest(topo_tops(m, at)?),
    })
}

/// Snap tolerance of a section point to a surface, m: beyond it the mark counts as free.
const SECTION_SNAP: f64 = 0.3;

/// Mark on a section: the top nearest the point at this plan location, not a frozen click.
fn section_surface(m: &Model, plan: P, z: f64) -> Result<Option<f64>, Error> {
    let mut tops = built_tops(&m.db, None, plan)?;
    tops.extend(topo_tops(m, plan)?);
    Ok(tops
        .into_iter()
        .filter(|t| (t - z).abs() <= SECTION_SNAP)
        .min_by(|p, q| (p - z).abs().total_cmp(&(q - z).abs())))
}

fn spot_value(m: &Model, view: AnnoView, at: P) -> Result<SpotValue, Error> {
    let db = &m.db;
    let (surface, fallback, plan) = match view {
        AnnoView::Plan(level) => {
            let elev: f64 = db
                .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(0.0);
            (plan_surface(m, level, at)?, elev, at)
        }
        AnnoView::Section(id) => {
            let line: Option<(f64, f64, f64, f64)> = db
                .query_row("SELECT ax,ay,bx,by FROM view_def WHERE id=?1", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .optional()?;
            let plan = line.map_or(at, |(ax, ay, bx, by)| {
                let l = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt().max(1e-12);
                (ax + (bx - ax) / l * at.0, ay + (by - ay) / l * at.0)
            });
            (section_surface(m, plan, at.1)?, at.1, plan)
        }
        // a legend is a schematic sample: it has no surface and cannot have one
        AnnoView::Legend(_) => (Some(at.1), at.1, at),
        // a drafting view is not tied to the model: the value is the drawn height
        AnnoView::Drafting(_) => (Some(at.1), at.1, at),
    };
    let site: Option<(f64, f64, f64)> = db
        .query_row(
            "SELECT north_deg, base_x, base_y FROM site_info WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (north, bx, by) = site.unwrap_or((0.0, 0.0, 0.0));
    // project = R(north)·(survey − base); here the inverse: survey = base + R(−north)·project
    let (s, c) = north.to_radians().sin_cos();
    Ok(SpotValue {
        z: surface.unwrap_or(fallback),
        east: bx + plan.0 * c + plan.1 * s,
        north: by - plan.0 * s + plan.1 * c,
        free: surface.is_none(),
    })
}

fn parse_row(kind: &str, data: &str) -> Option<AnnoShape> {
    let v: Value = serde_json::from_str(data).ok()?;
    AnnoShape::from_json(kind, &v)
}

pub(crate) fn annos_in(m: &Model, view: AnnoView) -> Result<Vec<Anno>, Error> {
    let db = &m.db;
    let (k, id) = view.code();
    let rows: Vec<(i64, String, Option<i64>, Option<i64>, String)> = {
        let mut st = db.prepare_cached(
            "SELECT id,kind,text_style,line_style,data FROM anno
             WHERE view_kind=?1 AND view_id=?2 ORDER BY id",
        )?;
        let v = st
            .query_map(params![k, id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        v
    };
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let styles = Styles::load(db)?;
    let mut out = Vec::with_capacity(rows.len());
    for (aid, kind, ts, ls, data) in rows {
        // one corrupted row does not blank the view and sheet; the integrity check reports it
        let Some(shape) = parse_row(&kind, &data) else {
            eprintln!("annotation {aid}: bad data, skipped");
            continue;
        };
        out.push(styles.resolve(m, aid, view, shape, ts, ls)?);
    }
    out.sort_by_key(|a| a.shape.order());
    Ok(out)
}

/// View primitives for the sheet and export (the same PDF and DXF output as the rest of the drawing).
pub(crate) fn view_prims(m: &Model, view: AnnoView) -> Result<Vec<ViewPrim>, Error> {
    Ok(annos_in(m, view)?
        .into_iter()
        .map(|item| ViewPrim::Anno { item })
        .collect())
}

fn view_exists(db: &Connection, view: AnnoView) -> Result<bool, Error> {
    let (sql, id) = match view {
        AnnoView::Plan(i) => ("SELECT count(*) FROM level WHERE id=?1", i),
        AnnoView::Section(i) => ("SELECT count(*) FROM view_def WHERE id=?1", i),
        AnnoView::Legend(i) => ("SELECT count(*) FROM anno_legend WHERE id=?1", i),
        AnnoView::Drafting(i) => ("SELECT count(*) FROM anno_drafting WHERE id=?1", i),
    };
    let n: i64 = db.query_row(sql, [id], |r| r.get(0))?;
    Ok(n > 0)
}

/// Annotations of a deleted view go with it; undo restores them.
pub(crate) fn drop_view(db: &Connection, inv: Inv, view: AnnoView) -> Result<Inv, Error> {
    let (k, id) = view.code();
    let ids: Vec<i64> = {
        let mut st = db.prepare_cached("SELECT id FROM anno WHERE view_kind=?1 AND view_id=?2")?;
        let v = st
            .query_map(params![k, id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut inv = inv;
    for a in ids {
        inv = inv.restore(db, "anno", "id", a)?;
        db.execute("DELETE FROM anno WHERE id=?1", [a])?;
    }
    let scale: Option<f64> = db
        .query_row(
            "SELECT scale FROM anno_view_scale WHERE view_kind=?1 AND view_id=?2",
            params![k, id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(s) = scale {
        inv = inv.sql(
            "INSERT OR REPLACE INTO anno_view_scale(view_kind,view_id,scale) VALUES (?1,?2,?3)",
            vec![Sql::Text(k.into()), Sql::Integer(id), Sql::Real(s)],
        );
        db.execute(
            "DELETE FROM anno_view_scale WHERE view_kind=?1 AND view_id=?2",
            params![k, id],
        )?;
    }
    Ok(inv)
}

fn no_anno(id: i64) -> Error {
    Error::Invalid(format!("no such annotation {id}"))
}

fn view_alive(db: &Connection, kind: &str, id: i64) -> Result<bool, Error> {
    match AnnoView::parse(kind, id) {
        Some(view) => view_exists(db, view),
        None => Ok(false),
    }
}

/// For the integrity check: (table, rowid, description) of rows with unreadable data or without a view.
pub(crate) fn broken_rows(
    db: &Connection,
) -> Result<Vec<(&'static str, i64, &'static str)>, Error> {
    let rows: Vec<(i64, String, i64, String, String)> = {
        let mut st = db.prepare("SELECT id,view_kind,view_id,kind,data FROM anno ORDER BY id")?;
        let v = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for (id, vk, vid, kind, data) in rows {
        if !view_alive(db, &vk, vid)? {
            out.push(("anno", id, "annotation of a missing view"));
        }
        if parse_row(&kind, &data).is_none() {
            out.push(("anno", id, "annotation data unreadable"));
        }
    }
    let scales: Vec<(i64, String, i64)> = {
        let mut st =
            db.prepare("SELECT rowid,view_kind,view_id FROM anno_view_scale ORDER BY rowid")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    for (rowid, vk, vid) in scales {
        if !view_alive(db, &vk, vid)? {
            out.push(("anno_view_scale", rowid, "view scale of a missing view"));
        }
    }
    Ok(out)
}

pub(crate) fn read_shape(db: &Connection, id: i64) -> Result<AnnoShape, Error> {
    let (kind, data): (String, String) = db
        .query_row("SELECT kind,data FROM anno WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .ok_or_else(|| no_anno(id))?;
    parse_row(&kind, &data).ok_or_else(|| Error::Invalid(format!("bad annotation {id}")))
}

/// Every text note with its view (find and replace).
pub(crate) fn text_notes(db: &Connection) -> Result<Vec<(i64, AnnoView, TextNote)>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id,view_kind,view_id,kind,data FROM anno WHERE kind='text' ORDER BY id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = vec![];
    for (id, vk, vid, kind, data) in rows {
        // a damaged row is reported by the integrity check, not here
        if let (Some(view), Some(AnnoShape::Text(t))) =
            (AnnoView::parse(&vk, vid), parse_row(&kind, &data))
        {
            out.push((id, view, t));
        }
    }
    Ok(out)
}

/// Generic edit tools (`Model::move_elements` and the rest): `m` is a turn or a reflection about
/// the origin, `t` the shift after it. The caller snapshots the row for undo.
pub(crate) fn transform_row(
    db: &Connection,
    id: i64,
    m: [[f64; 2]; 2],
    t: P,
) -> Result<(), Error> {
    let shape = read_shape(db, id)?;
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let ang = m[1][0].atan2(m[0][0]);
    let turned = if det < 0.0 {
        let h = ang / 2.0;
        shape.transformed(AnnoXf::Mirror {
            a: (0.0, 0.0),
            b: (h.cos(), h.sin()),
        })
    } else if ang.abs() > 1e-12 {
        shape.transformed(AnnoXf::Rotate { o: (0.0, 0.0), a: ang })
    } else {
        shape
    };
    let out = turned.translated(t);
    out.check()?;
    write_row(db, id, &out)
}

fn write_row(db: &Connection, id: i64, shape: &AnnoShape) -> Result<(), Error> {
    let n = db.execute(
        "UPDATE anno SET kind=?2, data=?3 WHERE id=?1",
        params![id, shape.code(), shape.to_json().to_string()],
    )?;
    if n == 0 {
        return Err(no_anno(id));
    }
    Ok(())
}

impl Model {
    /// Placing an annotation on a view is one operation.
    pub fn add_anno(
        &mut self,
        view: AnnoView,
        shape: &AnnoShape,
        text_style: Option<i64>,
        line_style: Option<i64>,
    ) -> Result<i64, Error> {
        shape.check()?;
        let (k, vid) = view.code();
        self.op("anno_add", |db| {
            if !view_exists(db, view)? {
                return Err(Error::Invalid(format!("no such view {k} {vid}")));
            }
            db.execute(
                "INSERT INTO anno(view_kind,view_id,kind,text_style,line_style,data)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    k,
                    vid,
                    shape.code(),
                    text_style,
                    line_style,
                    shape.to_json().to_string()
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno", "id", id)))
        })
    }

    pub fn update_anno(&mut self, id: i64, shape: &AnnoShape) -> Result<(), Error> {
        shape.check()?;
        self.op("anno_edit", |db| {
            let inv = Inv::default().restore(db, "anno", "id", id)?;
            write_row(db, id, shape)?;
            Ok(((), id, inv))
        })
    }

    pub fn set_anno_styles(
        &mut self,
        id: i64,
        text_style: Option<i64>,
        line_style: Option<i64>,
    ) -> Result<(), Error> {
        self.op("anno_style", |db| {
            let inv = Inv::default().restore(db, "anno", "id", id)?;
            let n = db.execute(
                "UPDATE anno SET text_style=?2, line_style=?3 WHERE id=?1",
                params![id, text_style, line_style],
            )?;
            if n == 0 {
                return Err(no_anno(id));
            }
            Ok(((), id, inv))
        })
    }

    /// Moving a group is one operation.
    pub fn move_annos(&mut self, ids: &[i64], d: (f64, f64)) -> Result<(), Error> {
        self.op("anno_move", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                let shape = read_shape(db, id)?;
                inv = inv.restore(db, "anno", "id", id)?;
                write_row(db, id, &shape.translated(d))?;
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// Rotating or reflecting a group is one operation; a degenerate transform is not applied.
    pub fn transform_annos(&mut self, ids: &[i64], x: AnnoXf) -> Result<(), Error> {
        self.op("anno_transform", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                let shape = read_shape(db, id)?.transformed(x);
                shape.check()?;
                inv = inv.restore(db, "anno", "id", id)?;
                write_row(db, id, &shape)?;
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// Copies of a group, one per transform, on the same view with the same styles; one
    /// operation. New ids follow the order of the originals, and within each, the order of the transforms.
    pub fn copy_annos(&mut self, ids: &[i64], xfs: &[AnnoXf]) -> Result<Vec<i64>, Error> {
        if xfs.is_empty() {
            return Err(Error::Invalid("nothing to copy to".into()));
        }
        self.op("anno_copy", |db| {
            let mut inv = Inv::default();
            let mut out = Vec::with_capacity(ids.len() * xfs.len());
            for &id in ids {
                let shape = read_shape(db, id)?;
                let (vk, vid, ts, ls): (String, i64, Option<i64>, Option<i64>) = db.query_row(
                    "SELECT view_kind,view_id,text_style,line_style FROM anno WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                for &x in xfs {
                    let copy = shape.transformed(x);
                    copy.check()?;
                    db.execute(
                        "INSERT INTO anno(view_kind,view_id,kind,text_style,line_style,data)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![vk, vid, copy.code(), ts, ls, copy.to_json().to_string()],
                    )?;
                    let n = db.last_insert_rowid();
                    inv = inv.delete("anno", "id", n);
                    out.push(n);
                }
            }
            let first = out.first().copied().unwrap_or(0);
            Ok((out, first, inv))
        })
    }

    pub fn delete_annos(&mut self, ids: &[i64]) -> Result<(), Error> {
        self.op("anno_delete", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                inv = inv.restore(db, "anno", "id", id)?;
                if db.execute("DELETE FROM anno WHERE id=?1", [id])? == 0 {
                    return Err(no_anno(id));
                }
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }

    /// View annotations in draw order.
    pub fn annos(&self, view: AnnoView) -> Result<Vec<Anno>, Error> {
        annos_in(self, view)
    }

    /// The view an annotation belongs to (without assembling styles and values).
    pub fn anno_view(&self, id: i64) -> Result<AnnoView, Error> {
        let row: (String, i64) = self
            .db
            .query_row(
                "SELECT view_kind, view_id FROM anno WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| no_anno(id))?;
        AnnoView::parse(&row.0, row.1).ok_or_else(|| Error::Invalid("bad view kind".into()))
    }

    pub fn anno(&self, id: i64) -> Result<Anno, Error> {
        annos_in(self, self.anno_view(id)?)?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| no_anno(id))
    }

    /// Standard of the sheet the view sits on: the first screen in order, or the legend slot.
    pub fn anno_view_standard(&self, view: AnnoView) -> Result<Option<String>, Error> {
        let (_, id) = view.code();
        let sql = match view {
            AnnoView::Plan(_) => {
                "SELECT s.standard FROM viewport v JOIN sheet s ON s.id=v.sheet
                 WHERE v.level=?1 ORDER BY v.id LIMIT 1"
            }
            AnnoView::Section(_) => {
                "SELECT s.standard FROM viewport v JOIN sheet s ON s.id=v.sheet
                 WHERE v.view_def=?1 ORDER BY v.id LIMIT 1"
            }
            AnnoView::Legend(_) => {
                "SELECT s.standard FROM anno_legend_place p JOIN sheet s ON s.id=p.sheet
                 WHERE p.legend=?1 ORDER BY p.id LIMIT 1"
            }
            // drafting views are not placed on sheets yet
            AnnoView::Drafting(_) => return Ok(None),
        };
        Ok(self.db.query_row(sql, [id], |r| r.get(0)).optional()?)
    }

    /// Annotation scale of a view on screen: the set one, else the sheet's first screen, else 1:100.
    pub fn anno_scale(&self, view: AnnoView) -> Result<f64, Error> {
        let (k, id) = view.code();
        let own: Option<f64> = self
            .db
            .query_row(
                "SELECT scale FROM anno_view_scale WHERE view_kind=?1 AND view_id=?2",
                params![k, id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(s) = own {
            return Ok(s);
        }
        let sql = match view {
            AnnoView::Plan(_) => "SELECT scale FROM viewport WHERE level=?1 ORDER BY id LIMIT 1",
            AnnoView::Section(_) => {
                "SELECT scale FROM viewport WHERE view_def=?1 ORDER BY id LIMIT 1"
            }
            AnnoView::Legend(_) => "SELECT scale FROM anno_legend WHERE id=?1",
            AnnoView::Drafting(_) => "SELECT scale FROM anno_drafting WHERE id=?1",
        };
        let placed: Option<f64> = self.db.query_row(sql, [id], |r| r.get(0)).optional()?;
        Ok(placed.unwrap_or(100.0))
    }

    pub fn set_anno_scale(&mut self, view: AnnoView, scale: f64) -> Result<(), Error> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(Error::Invalid("scale must be positive".into()));
        }
        let (k, id) = view.code();
        self.op("anno_scale", |db| {
            let old: Option<f64> = db
                .query_row(
                    "SELECT scale FROM anno_view_scale WHERE view_kind=?1 AND view_id=?2",
                    params![k, id],
                    |r| r.get(0),
                )
                .optional()?;
            let keys = || vec![Sql::Text(k.into()), Sql::Integer(id)];
            let inv = match old {
                Some(s) => {
                    let mut a = keys();
                    a.push(Sql::Real(s));
                    Inv::default().sql(
                        "INSERT OR REPLACE INTO anno_view_scale(view_kind,view_id,scale)
                         VALUES (?1,?2,?3)",
                        a,
                    )
                }
                None => Inv::default().sql(
                    "DELETE FROM anno_view_scale WHERE view_kind=?1 AND view_id=?2",
                    keys(),
                ),
            };
            db.execute(
                "INSERT OR REPLACE INTO anno_view_scale(view_kind,view_id,scale) VALUES (?1,?2,?3)",
                params![k, id, scale],
            )?;
            Ok(((), id, inv))
        })
    }

    pub fn text_styles(&self) -> Result<Vec<TextStyle>, Error> {
        text_styles_in(&self.db)
    }

    pub fn line_styles(&self) -> Result<Vec<LineStyle>, Error> {
        line_styles_in(&self.db)
    }

    pub fn add_text_style(&mut self, s: &TextStyle) -> Result<i64, Error> {
        if !s.height_mm.is_finite() || s.height_mm <= 0.0 || s.name.trim().is_empty() {
            return Err(Error::Invalid(
                "text style needs a name and a height".into(),
            ));
        }
        self.op("anno_text_style", |db| {
            db.execute(
                "INSERT INTO anno_text_style(name,height_mm,bold,underline,frame)
                 VALUES (?1,?2,?3,?4,?5)",
                params![s.name, s.height_mm, s.bold, s.underline, s.frame],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_text_style", "id", id)))
        })
    }

    pub fn add_line_style(&mut self, s: &LineStyle) -> Result<i64, Error> {
        if !s.weight_mm.is_finite() || s.weight_mm <= 0.0 || s.name.trim().is_empty() {
            return Err(Error::Invalid(
                "line style needs a name and a weight".into(),
            ));
        }
        self.op("anno_line_style", |db| {
            db.execute(
                "INSERT INTO anno_line_style(name,weight_mm,dash) VALUES (?1,?2,?3)",
                params![s.name, s.weight_mm, dash_text(&s.dash)],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_line_style", "id", id)))
        })
    }

    pub fn detail_defs(&self) -> Result<Vec<DetailDef>, Error> {
        defs_in(&self.db)
    }

    /// A new detail component (for example from detail lines on a view).
    pub fn add_detail_def(&mut self, d: &DetailDef) -> Result<i64, Error> {
        if d.name.trim().is_empty() || d.strokes.is_empty() {
            return Err(Error::Invalid("detail needs a name and lines".into()));
        }
        self.op("anno_detail_def", |db| {
            db.execute(
                "INSERT INTO anno_detail_def(name,data) VALUES (?1,?2)",
                params![d.name, def_json(d).to_string()],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("anno_detail_def", "id", id)))
        })
    }
}
