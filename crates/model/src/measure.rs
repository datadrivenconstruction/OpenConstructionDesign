// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Saved associative measurements (`measure`): distance, clear gap between faces, angle, area,
//! perimeter, volume, slope. Each pin names a stable feature of an element (a wall face, a mass
//! face by its history tag, a level, a room) plus a fallback point.
//!
//! Journal safety: `measure` holds only what the user made (kind, view, pins) and goes through
//! `Model::op`. The value, the state and the fallback memory live in `measure_geo` (derived, like
//! `dim_geo`) and `measure_ref` (which element a measurement reads). A pin whose feature is gone
//! is not replaced by a neighbour: the measurement turns orphan, shows its fallback points and
//! keeps its last value; the element coming back (undo) makes it live again.
//! The stored `measure.state` is `live`; `driving` is reserved for the driving dimension.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value as J};

use crate::annot::{parse_ref, ref_text, Ctx, DimRef};
use crate::ops::Inv;
use crate::{Error, Model, ViewKey};

pub type P3 = [f64; 3];

/// A stable feature of an element that a measurement reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Anchor {
    Point(P3),
    /// Wall faces and ends, axes, opening edges, grid crossings (as dimensions use them).
    Dim(DimRef),
    /// A mass face by its history tag; `op` is the kind of the operation that made it.
    MassFace { mass: i64, ftag: u32, op: String },
    /// Top or bottom plane of a slab (`kind` is `"slab"`).
    SlabFace { kind: String, id: i64, top: bool },
    Level { id: i64 },
    Room { id: i64 },
    /// Corner of the bounding box of an element (`wall`, `mass`, `slab`): bit 0 x max, 1 y max, 2 z max.
    ElemBox { kind: String, id: i64, corner: u8 },
}

/// An anchor and the point shown while it cannot be found.
#[derive(Clone, Debug, PartialEq)]
pub struct Pin {
    pub anchor: Anchor,
    pub fallback: P3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureKind {
    Distance,
    /// Distance along the common normal of two parallel faces.
    Clear,
    Angle,
    Area,
    Perimeter,
    Volume,
    Slope,
}

impl MeasureKind {
    /// Unit of the value: m, m2, m3, deg, percent.
    pub fn unit(self) -> &'static str {
        match self {
            MeasureKind::Distance | MeasureKind::Clear | MeasureKind::Perimeter => "m",
            MeasureKind::Area => "m2",
            MeasureKind::Volume => "m3",
            MeasureKind::Angle => "deg",
            MeasureKind::Slope => "percent",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            MeasureKind::Distance => "distance",
            MeasureKind::Clear => "clear",
            MeasureKind::Angle => "angle",
            MeasureKind::Area => "area",
            MeasureKind::Perimeter => "perimeter",
            MeasureKind::Volume => "volume",
            MeasureKind::Slope => "slope",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "distance" => MeasureKind::Distance,
            "clear" => MeasureKind::Clear,
            "angle" => MeasureKind::Angle,
            "area" => MeasureKind::Area,
            "perimeter" => MeasureKind::Perimeter,
            "volume" => MeasureKind::Volume,
            "slope" => MeasureKind::Slope,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeasureSpec {
    pub kind: MeasureKind,
    pub view: ViewKey,
    pub pins: Vec<Pin>,
    pub label: String,
}

/// Why a measurement has no number. Never a zero in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// This many pins lost their feature.
    Orphan(usize),
    /// The faces or points are not in the plane the kind needs (clear gap of non-parallel faces).
    NotPlanar,
    Unsupported(&'static str),
}

impl Reason {
    /// Machine text of the reason (`orphan:2`, `not_planar`, `unsupported:<what>`).
    pub fn code(&self) -> String {
        match self {
            Reason::Orphan(n) => format!("orphan:{n}"),
            Reason::NotPlanar => "not_planar".into(),
            Reason::Unsupported(s) => format!("unsupported:{s}"),
        }
    }
}

impl Value {
    /// The number, a state word (`live`, `orphan`, `unavailable`) and the reason text ("" when live).
    pub fn parts(&self) -> (Option<f64>, &'static str, String) {
        match self {
            Value::Ok(v) => (Some(*v), "live", String::new()),
            Value::Unavailable(r @ Reason::Orphan(_)) => (None, "orphan", r.code()),
            Value::Unavailable(r) => (None, "unavailable", r.code()),
        }
    }
}

impl MeasureSpec {
    /// Text of the view: `level:<id>`, `section:<id>`, `3d`.
    pub fn view_code(&self) -> String {
        match self.view {
            ViewKey::Plan(l) => format!("level:{l}"),
            ViewKey::Section(v) => format!("section:{v}"),
            ViewKey::ThreeD => "3d".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// Metres, square metres, cubic metres, degrees (angle) or percent (slope).
    Ok(f64),
    Unavailable(Reason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureState {
    Live,
    Orphan,
    Unavailable,
}

/// A stored measurement with what the last recompute found.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureInfo {
    pub id: i64,
    pub spec: MeasureSpec,
    pub state: MeasureState,
    pub value: Value,
    /// The last number it had (kept while orphan or unavailable).
    pub last: Option<f64>,
    /// One point per pin: the feature now, or the remembered fallback.
    pub points: Vec<P3>,
    /// A driving measurement holds its number as a constraint (its twin in `constraint_rule`).
    pub driving: bool,
}

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS measure(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, view_kind TEXT NOT NULL,
    view_ref INTEGER NOT NULL, anchors TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'live',
    label TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS measure_view ON measure(view_kind, view_ref);
CREATE TABLE IF NOT EXISTS measure_geo(measure INTEGER PRIMARY KEY, state TEXT NOT NULL, value REAL,
    geom TEXT NOT NULL, fb TEXT NOT NULL, reason TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS measure_ref(measure INTEGER NOT NULL, kind TEXT NOT NULL, elem INTEGER NOT NULL,
    PRIMARY KEY(measure, kind, elem)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS measure_ref_elem ON measure_ref(kind, elem);
";

/// Creates the tables (idempotent); never writes rows, so a read-only open stays read-only.
/// Migration note: a Distance between two wall axes used to be the distance between their start
/// points; it is now the distance between the lines. `info_of` computes such a live measurement
/// from the model when it is read, and the stored number is replaced by the next regeneration.
pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// A Distance between two wall axes: its stored number may come from the old point-to-point rule.
fn axis_pair(spec: &MeasureSpec) -> bool {
    spec.kind == MeasureKind::Distance
        && matches!(
            spec.pins.as_slice(),
            [Pin { anchor: Anchor::Dim(DimRef::AxisLine { .. }), .. }, Pin { anchor: Anchor::Dim(DimRef::AxisLine { .. }), .. }]
        )
}

pub(crate) fn has_geo(db: &Connection) -> Result<bool, Error> {
    Ok(db
        .prepare_cached("SELECT 1 FROM sqlite_master WHERE type='table' AND name='measure_geo'")?
        .exists([])?)
}

pub(crate) fn row_exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db
        .prepare_cached(&format!("SELECT 1 FROM {table} WHERE id=?1"))?
        .exists([id])?)
}

// ---- storage text -------------------------------------------------------------------------

fn p3_json(p: &P3) -> J {
    json!([p[0], p[1], p[2]])
}

fn p3_of(j: &J) -> Option<P3> {
    let a = j.as_array()?;
    (a.len() == 3).then_some(())?;
    Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
}

fn anchor_json(a: &Anchor) -> J {
    match a {
        Anchor::Point(p) => json!({"t": "pt", "p": p3_json(p)}),
        Anchor::Dim(r) => json!({"t": "dim", "r": ref_text(r)}),
        Anchor::MassFace { mass, ftag, op } => {
            json!({"t": "mf", "mass": mass, "ftag": ftag, "op": op})
        }
        Anchor::SlabFace { kind, id, top } => json!({"t": "sf", "kind": kind, "id": id, "top": top}),
        Anchor::Level { id } => json!({"t": "lv", "id": id}),
        Anchor::Room { id } => json!({"t": "rm", "id": id}),
        Anchor::ElemBox { kind, id, corner } => {
            json!({"t": "eb", "kind": kind, "id": id, "corner": corner})
        }
    }
}

fn anchor_of(j: &J) -> Option<Anchor> {
    let id = || j.get("id")?.as_i64();
    let s = |k: &str| j.get(k)?.as_str().map(str::to_string);
    Some(match j.get("t")?.as_str()? {
        "pt" => Anchor::Point(p3_of(j.get("p")?)?),
        "dim" => Anchor::Dim(parse_ref(j.get("r")?.as_str()?)?),
        "mf" => Anchor::MassFace {
            mass: j.get("mass")?.as_i64()?,
            ftag: u32::try_from(j.get("ftag")?.as_u64()?).ok()?,
            op: s("op")?,
        },
        "sf" => Anchor::SlabFace {
            kind: s("kind")?,
            id: id()?,
            top: j.get("top")?.as_bool()?,
        },
        "lv" => Anchor::Level { id: id()? },
        "rm" => Anchor::Room { id: id()? },
        "eb" => Anchor::ElemBox {
            kind: s("kind")?,
            id: id()?,
            corner: u8::try_from(j.get("corner")?.as_u64()?).ok()?,
        },
        _ => return None,
    })
}

fn pins_text(pins: &[Pin]) -> String {
    J::Array(
        pins.iter()
            .map(|p| json!({"a": anchor_json(&p.anchor), "f": p3_json(&p.fallback)}))
            .collect(),
    )
    .to_string()
}

fn pins_of(text: &str) -> Option<Vec<Pin>> {
    let j: J = serde_json::from_str(text).ok()?;
    j.as_array()?
        .iter()
        .map(|p| {
            Some(Pin {
                anchor: anchor_of(p.get("a")?)?,
                fallback: p3_of(p.get("f")?)?,
            })
        })
        .collect()
}

fn pts_text(ps: &[P3]) -> String {
    J::Array(ps.iter().map(p3_json).collect()).to_string()
}

fn pts_of(text: &str) -> Vec<P3> {
    serde_json::from_str::<J>(text)
        .ok()
        .and_then(|j| j.as_array()?.iter().map(p3_of).collect())
        .unwrap_or_default()
}

fn view_of(kind: &str, r: i64) -> Option<ViewKey> {
    Some(match kind {
        "level" => ViewKey::Plan(r),
        "view_def" => ViewKey::Section(r),
        "3d" => ViewKey::ThreeD,
        _ => return None,
    })
}

const REASONS: &[&str] = &[
    "clear_needs_faces",
    "axis_curved",
    "arc_face",
    "slab_kind",
    "elem_kind",
    "room_unbounded",
    "angle_needs_lines",
    "area_of",
    "perimeter_of",
    "volume_of",
    "slope_run",
    "points_degenerate",
];

fn intern(s: &str) -> &'static str {
    REASONS.iter().copied().find(|r| *r == s).unwrap_or("unsupported")
}

fn reason_text(v: &Value) -> String {
    match v {
        Value::Ok(_) => String::new(),
        Value::Unavailable(Reason::Orphan(n)) => format!("orphan:{n}"),
        Value::Unavailable(Reason::NotPlanar) => "planar".into(),
        Value::Unavailable(Reason::Unsupported(s)) => format!("unsup:{s}"),
    }
}

fn value_of(state: &str, value: Option<f64>, reason: &str) -> Value {
    match state {
        "live" => value.map_or(Value::Unavailable(Reason::Unsupported("unsupported")), Value::Ok),
        _ => Value::Unavailable(match reason.split_once(':') {
            Some(("orphan", n)) => Reason::Orphan(n.parse().unwrap_or(1)),
            Some(("unsup", s)) => Reason::Unsupported(intern(s)),
            _ => {
                if reason == "planar" {
                    Reason::NotPlanar
                } else {
                    Reason::Unsupported("unsupported")
                }
            }
        }),
    }
}

// ---- anchors → geometry -------------------------------------------------------------------

/// What a pin resolved to; only the parts its feature has.
#[derive(Default)]
struct R {
    p: P3,
    /// Unit normal of a planar face.
    n: Option<P3>,
    /// Unit direction of a line feature.
    d: Option<P3>,
    area: Option<f64>,
    perim: Option<f64>,
    vol: Option<f64>,
    /// An infinite line (a wall axis): a distance between two of them is between the lines.
    line: bool,
}

enum Fail {
    Orphan,
    Unsupported(&'static str),
}

type Res = Result<R, Fail>;

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: P3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: P3) -> Option<P3> {
    let l = norm(a);
    (l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

fn ring_area_perim(r: &[(f64, f64)]) -> (f64, f64) {
    let (mut a, mut p) = (0.0, 0.0);
    for i in 0..r.len() {
        let (u, v) = (r[i], r[(i + 1) % r.len()]);
        a += u.0 * v.1 - v.0 * u.1;
        p += ((v.0 - u.0).powi(2) + (v.1 - u.1).powi(2)).sqrt();
    }
    (a.abs() / 2.0, p)
}

fn level_elev(db: &Connection, id: i64) -> Result<Option<f64>, Error> {
    Ok(db
        .prepare_cached("SELECT elevation FROM level WHERE id=?1")?
        .query_row([id], |r| r.get(0))
        .optional()?)
}

/// Rings of a slab (outer first), top and thickness; `None` if the slab is gone.
type SlabRings = (Vec<Vec<(f64, f64)>>, f64, f64);

fn slab_rings(db: &Connection, id: i64) -> Result<Option<SlabRings>, Error> {
    let row: Option<(f64, f64)> = db
        .prepare_cached(
            "SELECT lv.elevation + s.offset, COALESCE((SELECT SUM(thickness) FROM slab_type_layer \
             WHERE stype=s.stype),0) FROM slab s JOIN level lv ON lv.id=s.level WHERE s.id=?1",
        )?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((top, th)) = row else {
        return Ok(None);
    };
    let mut st = db.prepare_cached(
        "SELECT ring,x,y FROM slab_vertex WHERE slab=?1 ORDER BY ring,idx",
    )?;
    let rows = st.query_map([id], |r| {
        Ok((r.get::<_, i64>(0)?, (r.get::<_, f64>(1)?, r.get::<_, f64>(2)?)))
    })?;
    let mut rings: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
    for r in rows {
        let (k, p) = r?;
        rings.entry(k).or_default().push(p);
    }
    Ok(Some((rings.into_values().collect(), top, th)))
}

fn wall_dir(f: &crate::annot::WallFrame) -> Option<(P3, P3)> {
    let d = unit([f.b.0 - f.a.0, f.b.1 - f.a.1, 0.0])?;
    Some((d, [-d[1], d[0], 0.0]))
}

fn box_corner(lo: P3, hi: P3, corner: u8) -> P3 {
    let pick = |bit: u8, i: usize| if corner >> bit & 1 == 1 { hi[i] } else { lo[i] };
    [pick(0, 0), pick(1, 1), pick(2, 2)]
}

fn resolve(cx: &mut Ctx, a: &Anchor) -> Result<Res, Error> {
    let db = cx.db;
    Ok(match a {
        Anchor::Point(p) => Ok(R {
            p: *p,
            ..R::default()
        }),
        Anchor::Dim(r) => return resolve_dim(cx, r),
        Anchor::MassFace { mass, ftag, op } => match crate::mass::anchor_face(db, *mass, *ftag, op)? {
            None => Err(Fail::Orphan),
            Some(f) => {
                let Some(first) = f.outline.first().and_then(|o| o.first()) else {
                    return Ok(Err(Fail::Orphan));
                };
                let perim: f64 = f
                    .outline
                    .iter()
                    .map(|ring| {
                        (0..ring.len())
                            .map(|i| {
                                let (u, v) = (ring[i], ring[(i + 1) % ring.len()]);
                                norm(sub([v.0, v.1, v.2], [u.0, u.1, u.2]))
                            })
                            .sum::<f64>()
                    })
                    .sum();
                let curved = f.class == crate::mass::FaceClass::Curved;
                Ok(R {
                    p: [first.0, first.1, first.2],
                    n: (!curved).then(|| unit([f.normal.0, f.normal.1, f.normal.2])).flatten(),
                    area: Some(f.area),
                    perim: Some(perim),
                    ..R::default()
                })
            }
        },
        Anchor::SlabFace { kind, id, top } => {
            if kind != "slab" {
                return Ok(Err(Fail::Unsupported("slab_kind")));
            }
            match slab_rings(db, *id)? {
                None => Err(Fail::Orphan),
                Some((rings, t, th)) => {
                    let z = if *top { t } else { t - th };
                    let (mut area, mut perim) = (0.0, 0.0);
                    for (k, r) in rings.iter().enumerate() {
                        let (a, p) = ring_area_perim(r);
                        area += if k == 0 { a } else { -a };
                        perim += p;
                    }
                    let first = rings.first().and_then(|r| r.first()).copied().unwrap_or((0.0, 0.0));
                    Ok(R {
                        p: [first.0, first.1, z],
                        n: Some([0.0, 0.0, 1.0]),
                        area: Some(area),
                        perim: Some(perim),
                        ..R::default()
                    })
                }
            }
        }
        Anchor::Level { id } => match level_elev(db, *id)? {
            None => Err(Fail::Orphan),
            Some(e) => Ok(R {
                p: [0.0, 0.0, e],
                n: Some([0.0, 0.0, 1.0]),
                ..R::default()
            }),
        },
        Anchor::Room { id } => {
            let row: Option<(f64, f64, f64)> = db
                .prepare_cached(
                    "SELECT px,py,(SELECT elevation FROM level WHERE id=room.level) FROM room WHERE id=?1",
                )?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            let Some((x, y, z)) = row else {
                return Ok(Err(Fail::Orphan));
            };
            let q: Option<(f64, f64, f64)> = db
                .prepare_cached("SELECT area,perimeter,volume FROM room_qty WHERE room=?1")?
                .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            match q {
                None => Err(Fail::Unsupported("room_unbounded")),
                Some((area, perim, vol)) => Ok(R {
                    p: [x, y, z],
                    area: Some(area),
                    perim: Some(perim),
                    vol: Some(vol),
                    ..R::default()
                }),
            }
        }
        Anchor::ElemBox { kind, id, corner } => match kind.as_str() {
            "wall" => match cx.frame(*id)? {
                None => Err(Fail::Orphan),
                Some(f) => {
                    let Some((_, n)) = wall_dir(&f) else {
                        return Ok(Err(Fail::Unsupported("elem_kind")));
                    };
                    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
                    for e in [f.a, f.b] {
                        for off in f.face {
                            let q = [e.0 + n[0] * off, e.1 + n[1] * off, f.base];
                            let t = [q[0], q[1], f.base + f.height];
                            for v in [q, t] {
                                for i in 0..3 {
                                    lo[i] = lo[i].min(v[i]);
                                    hi[i] = hi[i].max(v[i]);
                                }
                            }
                        }
                    }
                    Ok(R {
                        p: box_corner(lo, hi, *corner),
                        ..R::default()
                    })
                }
            },
            "mass" => match crate::mass::anchor_body(db, *id)? {
                None => Err(Fail::Orphan),
                Some(b) => match b.bbox() {
                    None => Err(Fail::Unsupported("elem_kind")),
                    Some((lo, hi)) => Ok(R {
                        p: box_corner([lo.x, lo.y, lo.z], [hi.x, hi.y, hi.z], *corner),
                        vol: Some(b.volume()),
                        ..R::default()
                    }),
                },
            },
            "slab" => match slab_rings(db, *id)? {
                None => Err(Fail::Orphan),
                Some((rings, top, th)) => {
                    let (mut lo, mut hi) = ([f64::MAX, f64::MAX, top - th], [f64::MIN, f64::MIN, top]);
                    for p in rings.iter().flatten() {
                        lo[0] = lo[0].min(p.0);
                        lo[1] = lo[1].min(p.1);
                        hi[0] = hi[0].max(p.0);
                        hi[1] = hi[1].max(p.1);
                    }
                    let vol: f64 = db
                        .prepare_cached("SELECT COALESCE(SUM(volume),0) FROM slab_qty WHERE slab=?1")?
                        .query_row([id], |r| r.get(0))?;
                    Ok(R {
                        p: box_corner(lo, hi, *corner),
                        vol: Some(vol),
                        ..R::default()
                    })
                }
            },
            _ => Err(Fail::Unsupported("elem_kind")),
        },
    })
}

fn resolve_dim(cx: &mut Ctx, r: &DimRef) -> Result<Res, Error> {
    match *r {
        DimRef::Point((x, y)) => Ok(Ok(R {
            p: [x, y, 0.0],
            ..R::default()
        })),
        DimRef::AxisLine { wall } => {
            let Some(f) = cx.frame(wall)? else {
                return Ok(Err(Fail::Orphan));
            };
            if !f.straight {
                return Ok(Err(Fail::Unsupported("axis_curved")));
            }
            let d = wall_dir(&f).map(|v| v.0);
            Ok(Ok(R {
                p: [f.a.0, f.a.1, f.base],
                d,
                line: true,
                ..R::default()
            }))
        }
        DimRef::ArcFace { .. } => Ok(Err(Fail::Unsupported("arc_face"))),
        DimRef::WallLevel { wall, top } => {
            let Some(f) = cx.frame(wall)? else {
                return Ok(Err(Fail::Orphan));
            };
            Ok(Ok(R {
                p: [
                    (f.a.0 + f.b.0) / 2.0,
                    (f.a.1 + f.b.1) / 2.0,
                    f.base + if top { f.height } else { 0.0 },
                ],
                n: Some([0.0, 0.0, 1.0]),
                ..R::default()
            }))
        }
        DimRef::WallEnd { .. }
        | DimRef::AxisEnd { .. }
        | DimRef::OpeningEdge { .. }
        | DimRef::GridAt { .. } => {
            let Some((host, p)) = cx.point3(r)? else {
                return Ok(Err(Fail::Orphan));
            };
            let (mut n, mut d) = (None, None);
            if let Some(w) = host {
                if let Some(f) = cx.frame(w)?.filter(|f| f.straight) {
                    if let Some((dir, nrm)) = wall_dir(&f) {
                        d = Some(dir);
                        // a face is a plane; an axis end or a grid crossing is not
                        if matches!(r, DimRef::WallEnd { .. } | DimRef::OpeningEdge { .. }) {
                            n = Some(nrm);
                        }
                    }
                }
            }
            Ok(Ok(R { p, n, d, ..R::default() }))
        }
    }
}

// ---- values -------------------------------------------------------------------------------

fn polygon(rs: &[&R]) -> Result<(f64, f64), Reason> {
    if rs.len() < 3 {
        return Err(Reason::Unsupported("points_degenerate"));
    }
    let mut nrm = [0.0; 3];
    let mut perim = 0.0;
    for i in 0..rs.len() {
        let (u, v) = (rs[i].p, rs[(i + 1) % rs.len()].p);
        let c = cross(u, v);
        for k in 0..3 {
            nrm[k] += c[k] / 2.0;
        }
        perim += norm(sub(v, u));
    }
    let area = norm(nrm);
    let Some(nu) = unit(nrm) else {
        return Err(Reason::Unsupported("points_degenerate"));
    };
    let scale = rs.iter().map(|r| norm(sub(r.p, rs[0].p))).fold(1.0, f64::max);
    if rs.iter().any(|r| dot(sub(r.p, rs[0].p), nu).abs() > 1e-6 * scale) {
        return Err(Reason::NotPlanar);
    }
    Ok((area, perim))
}

fn compute(kind: MeasureKind, rs: &[&R]) -> Value {
    let fail = |r| Value::Unavailable(r);
    match kind {
        MeasureKind::Distance if rs[0].line && rs[1].line => {
            let (Some(d1), Some(d2)) = (rs[0].d, rs[1].d) else {
                return fail(Reason::Unsupported("axis_curved"));
            };
            let (Some(d1), Some(d2)) = (unit(d1), unit(d2)) else {
                return fail(Reason::Unsupported("axis_curved"));
            };
            if norm(cross(d1, d2)) > 1e-6 {
                return fail(Reason::Unsupported("axes_not_parallel"));
            }
            Value::Ok(norm(cross(sub(rs[1].p, rs[0].p), d1)))
        }
        MeasureKind::Distance => Value::Ok(norm(sub(rs[0].p, rs[1].p))),
        MeasureKind::Clear => {
            let (Some(n1), Some(n2)) = (rs[0].n, rs[1].n) else {
                return fail(Reason::Unsupported("clear_needs_faces"));
            };
            if norm(cross(n1, n2)) > 1e-9 {
                return fail(Reason::NotPlanar);
            }
            Value::Ok(dot(sub(rs[1].p, rs[0].p), n1).abs())
        }
        MeasureKind::Angle => {
            let (u, v) = if rs.len() == 3 {
                (sub(rs[0].p, rs[1].p), sub(rs[2].p, rs[1].p))
            } else {
                let (Some(a), Some(b)) = (rs[0].d, rs[1].d) else {
                    return fail(Reason::Unsupported("angle_needs_lines"));
                };
                (a, b)
            };
            match (unit(u), unit(v)) {
                (Some(u), Some(v)) => Value::Ok(dot(u, v).clamp(-1.0, 1.0).acos().to_degrees()),
                _ => fail(Reason::Unsupported("points_degenerate")),
            }
        }
        MeasureKind::Area => match rs {
            [one] => one
                .area
                .map_or(fail(Reason::Unsupported("area_of")), Value::Ok),
            _ => polygon(rs).map_or_else(fail, |(a, _)| Value::Ok(a)),
        },
        MeasureKind::Perimeter => match rs {
            [one] => one
                .perim
                .map_or(fail(Reason::Unsupported("perimeter_of")), Value::Ok),
            _ => polygon(rs).map_or_else(fail, |(_, p)| Value::Ok(p)),
        },
        MeasureKind::Volume => rs[0]
            .vol
            .map_or(fail(Reason::Unsupported("volume_of")), Value::Ok),
        MeasureKind::Slope => {
            let d = sub(rs[1].p, rs[0].p);
            let run = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if run < 1e-9 {
                fail(Reason::Unsupported("slope_run"))
            } else {
                Value::Ok(100.0 * d[2].abs() / run)
            }
        }
    }
}

struct Out {
    value: Value,
    pts: Vec<P3>,
    fb: Vec<P3>,
}

fn evaluate(cx: &mut Ctx, spec: &MeasureSpec, old_fb: &[P3]) -> Result<Out, Error> {
    let mut rs: Vec<Option<R>> = vec![];
    let (mut pts, mut orphans, mut unsup) = (vec![], 0usize, None);
    for (i, pin) in spec.pins.iter().enumerate() {
        let memory = old_fb.get(i).copied().unwrap_or(pin.fallback);
        match resolve(cx, &pin.anchor)? {
            Ok(r) => {
                pts.push(r.p);
                rs.push(Some(r));
            }
            Err(f) => {
                match f {
                    Fail::Orphan => orphans += 1,
                    Fail::Unsupported(w) => {
                        unsup.get_or_insert(w);
                    }
                }
                pts.push(memory);
                rs.push(None);
            }
        }
    }
    let value = if orphans > 0 {
        Value::Unavailable(Reason::Orphan(orphans))
    } else if let Some(w) = unsup {
        Value::Unavailable(Reason::Unsupported(w))
    } else {
        let all: Vec<&R> = rs.iter().flatten().collect();
        compute(spec.kind, &all)
    };
    // the memory of a pin follows its feature while it is found
    let fb = pts.clone();
    Ok(Out { value, pts, fb })
}

fn check(s: &MeasureSpec) -> Result<(), Error> {
    let n = s.pins.len();
    let ok = match s.kind {
        MeasureKind::Distance | MeasureKind::Clear | MeasureKind::Slope => n == 2,
        MeasureKind::Angle => n == 2 || n == 3,
        MeasureKind::Area | MeasureKind::Perimeter => n == 1 || n >= 3,
        MeasureKind::Volume => n == 1,
    };
    if !ok {
        return Err(Error::Invalid(format!(
            "{} measurement: wrong number of pins ({n})",
            s.kind.code()
        )));
    }
    let fin = s.pins.iter().all(|p| {
        p.fallback.iter().all(|v| v.is_finite())
            && match &p.anchor {
                Anchor::Point(q) => q.iter().all(|v| v.is_finite()),
                _ => true,
            }
    });
    if !fin {
        return Err(Error::Invalid("measurement points must be finite".into()));
    }
    Ok(())
}

// ---- derived rows -------------------------------------------------------------------------

fn refs_of(db: &Connection, pins: &[Pin]) -> Result<BTreeSet<(&'static str, i64)>, Error> {
    let mut out = BTreeSet::new();
    for p in pins {
        match &p.anchor {
            Anchor::Point(_) => {}
            Anchor::Dim(r) => match *r {
                DimRef::Point(_) => {}
                DimRef::WallEnd { wall, .. }
                | DimRef::AxisEnd { wall, .. }
                | DimRef::AxisLine { wall }
                | DimRef::ArcFace { wall, .. }
                | DimRef::WallLevel { wall, .. } => {
                    out.insert(("wall", wall));
                }
                DimRef::GridAt { grid, wall } => {
                    out.insert(("wall", wall));
                    out.insert(("grid", grid));
                }
                DimRef::OpeningEdge { opening, .. } => {
                    out.insert(("opening", opening));
                    let host: Option<i64> = db
                        .prepare_cached("SELECT wall FROM opening WHERE id=?1")?
                        .query_row([opening], |r| r.get(0))
                        .optional()?;
                    if let Some(h) = host {
                        out.insert(("wall", h));
                    }
                }
            },
            Anchor::MassFace { mass, .. } => {
                out.insert(("mass", *mass));
            }
            Anchor::SlabFace { id, .. } => {
                out.insert(("slab", *id));
            }
            Anchor::Level { id } => {
                out.insert(("level", *id));
            }
            Anchor::Room { id } => {
                out.insert(("room", *id));
            }
            Anchor::ElemBox { kind, id, .. } => {
                let k = match kind.as_str() {
                    "wall" => "wall",
                    "mass" => "mass",
                    "slab" => "slab",
                    _ => continue,
                };
                out.insert((k, *id));
            }
        }
    }
    Ok(out)
}

type Row = (i64, Option<MeasureSpec>);

fn read_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let id: i64 = r.get(0)?;
    let kind: String = r.get(1)?;
    let vk: String = r.get(2)?;
    let vr: i64 = r.get(3)?;
    let pins: String = r.get(4)?;
    let label: String = r.get(5)?;
    let spec = MeasureKind::parse(&kind)
        .zip(view_of(&vk, vr))
        .zip(pins_of(&pins))
        .map(|((kind, view), pins)| MeasureSpec {
            kind,
            view,
            pins,
            label,
        });
    Ok((id, spec))
}

const COLS: &str = "id,kind,view_kind,view_ref,anchors,label";

type Memory = (Vec<P3>, Option<f64>);

fn memory(db: &Connection, id: i64) -> Result<Memory, Error> {
    let m: Option<(String, Option<f64>)> = db
        .prepare_cached("SELECT fb,value FROM measure_geo WHERE measure=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(m.map_or((vec![], None), |(fb, v)| (pts_of(&fb), v)))
}

fn write_geo(db: &Connection, id: i64, out: &Out, last: Option<f64>) -> Result<(), Error> {
    let (state, value) = match out.value {
        Value::Ok(v) => ("live", Some(v)),
        Value::Unavailable(Reason::Orphan(_)) => ("orphan", last),
        Value::Unavailable(_) => ("unavail", last),
    };
    db.prepare_cached(
        "INSERT OR REPLACE INTO measure_geo(measure,state,value,geom,fb,reason) VALUES (?1,?2,?3,?4,?5,?6)",
    )?
    .execute(params![
        id,
        state,
        value,
        pts_text(&out.pts),
        pts_text(&out.fb),
        reason_text(&out.value)
    ])?;
    Ok(())
}

fn write_refs(db: &Connection, id: i64, spec: &MeasureSpec) -> Result<(), Error> {
    db.prepare_cached("DELETE FROM measure_ref WHERE measure=?1")?
        .execute([id])?;
    let mut ins = db.prepare_cached("INSERT OR IGNORE INTO measure_ref(measure,kind,elem) VALUES (?1,?2,?3)")?;
    for (k, e) in refs_of(db, &spec.pins)? {
        ins.execute(params![id, k, e])?;
    }
    Ok(())
}

fn recompute(db: &Connection, cx: &mut Ctx, id: i64, spec: Option<&MeasureSpec>) -> Result<(), Error> {
    let (fb, last) = memory(db, id)?;
    match spec {
        Some(s) => {
            let out = evaluate(cx, s, &fb)?;
            let last = if let Value::Ok(v) = out.value { Some(v) } else { last };
            write_geo(db, id, &out, last)
        }
        // an unreadable row (a newer kind): nothing to compute, keep what was there
        None => Ok(()),
    }
}

/// Full recompute of every measurement (after a recompute that was not partial).
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    if !has_geo(db)? {
        return Ok(());
    }
    db.execute("DELETE FROM measure_geo WHERE measure NOT IN (SELECT id FROM measure)", [])?;
    db.execute("DELETE FROM measure_ref", [])?;
    let rows: Vec<Row> = {
        let mut st = db.prepare(&format!("SELECT {COLS} FROM measure ORDER BY id"))?;
        let v = st.query_map([], read_row)?.collect::<Result<_, _>>()?;
        v
    };
    let mut cx = Ctx::new(db);
    for (id, spec) in rows {
        if let Some(s) = &spec {
            write_refs(db, id, s)?;
        }
        recompute(db, &mut cx, id, spec.as_ref())?;
    }
    Ok(())
}

/// What a partial recompute changed.
pub(crate) struct Touch<'a> {
    pub walls: &'a BTreeSet<i64>,
    pub slabs: &'a BTreeSet<i64>,
    pub rooms: &'a BTreeSet<i64>,
    /// Rooms follow walls and slabs: every room measurement is read again.
    pub rooms_any: bool,
    /// Measurements whose rows were written.
    pub measures: &'a BTreeSet<i64>,
}

/// Partial recompute: the measurements that read a changed element, and the changed rows.
pub(crate) fn refresh(db: &Connection, t: &Touch) -> Result<(), Error> {
    if !has_geo(db)? {
        return Ok(());
    }
    let nothing = t.walls.is_empty()
        && t.slabs.is_empty()
        && t.rooms.is_empty()
        && !t.rooms_any
        && t.measures.is_empty();
    if nothing {
        return Ok(());
    }
    let mut ids: BTreeSet<i64> = t.measures.clone();
    {
        let mut q = db.prepare_cached("SELECT measure FROM measure_ref WHERE kind=?1 AND elem=?2")?;
        for (kind, set) in [("wall", t.walls), ("slab", t.slabs), ("room", t.rooms)] {
            for &e in set {
                for m in q.query_map(params![kind, e], |r| r.get::<_, i64>(0))? {
                    ids.insert(m?);
                }
            }
        }
        if t.rooms_any {
            let mut all = db.prepare_cached("SELECT DISTINCT measure FROM measure_ref WHERE kind='room'")?;
            for m in all.query_map([], |r| r.get::<_, i64>(0))? {
                ids.insert(m?);
            }
        }
    }
    let mut cx = Ctx::new(db);
    for id in ids {
        let row: Option<Row> = db
            .prepare_cached(&format!("SELECT {COLS} FROM measure WHERE id=?1"))?
            .query_row([id], read_row)
            .optional()?;
        match row {
            None => {
                db.prepare_cached("DELETE FROM measure_geo WHERE measure=?1")?.execute([id])?;
                db.prepare_cached("DELETE FROM measure_ref WHERE measure=?1")?.execute([id])?;
            }
            Some((id, spec)) => {
                if let (Some(s), true) = (&spec, t.measures.contains(&id)) {
                    write_refs(db, id, s)?;
                }
                recompute(db, &mut cx, id, spec.as_ref())?;
            }
        }
    }
    Ok(())
}

// ---- model API ----------------------------------------------------------------------------

fn info_of(db: &Connection, id: i64, spec: MeasureSpec) -> Result<MeasureInfo, Error> {
    let g: Option<(String, Option<f64>, String, String)> = db
        .prepare_cached("SELECT state,value,geom,reason FROM measure_geo WHERE measure=?1")?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    // a stored live number of an axis pair is not trusted: it may be from the old rule
    let g = g.filter(|g| !(axis_pair(&spec) && g.0 == "live"));
    let Some((state, value, geom, reason)) = g else {
        // no derived row yet (a file written before this step): compute without storing
        let mut cx = Ctx::new(db);
        let out = evaluate(&mut cx, &spec, &[])?;
        let last = if let Value::Ok(v) = out.value { Some(v) } else { None };
        return Ok(MeasureInfo {
            id,
            state: match out.value {
                Value::Ok(_) => MeasureState::Live,
                Value::Unavailable(Reason::Orphan(_)) => MeasureState::Orphan,
                Value::Unavailable(_) => MeasureState::Unavailable,
            },
            value: out.value,
            last,
            points: out.pts,
            spec,
            driving: twin_rule(db, id)?.is_some(),
        });
    };
    Ok(MeasureInfo {
        id,
        spec,
        state: match state.as_str() {
            "live" => MeasureState::Live,
            "orphan" => MeasureState::Orphan,
            _ => MeasureState::Unavailable,
        },
        value: value_of(&state, value, &reason),
        last: value,
        points: pts_of(&geom),
        driving: twin_rule(db, id)?.is_some(),
    })
}

/// The twin rule of a driving measurement: its id and the held number.
fn twin_rule(db: &Connection, measure: i64) -> Result<Option<(i64, f64)>, Error> {
    Ok(db
        .prepare_cached("SELECT id, value FROM constraint_rule WHERE kind='measure' AND walls=?1")?
        .query_row([measure.to_string()], |r| Ok((r.get(0)?, r.get::<_, Option<f64>>(1)?.unwrap_or(0.0))))
        .optional()?)
}

/// The measurement a twin rule belongs to.
pub(crate) fn twin_measure(db: &Connection, rule: i64) -> Option<i64> {
    db.query_row(
        "SELECT walls FROM constraint_rule WHERE id=?1 AND kind='measure'",
        [rule],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
}

/// The constraint a driving measurement stands for, holding `target` at the supports as they are
/// now: wall faces become a distance lock between the axes (the face offsets are constant under
/// moves), levels a level gap. None when the measurement is not of a drivable shape.
pub(crate) fn twin_spec(
    db: &Connection,
    rule_walls: &str,
    target: f64,
) -> Result<Option<crate::ConstraintSpec>, Error> {
    use crate::{ConstraintKind, ConstraintSpec};
    let Ok(id) = rule_walls.trim().parse::<i64>() else { return Ok(None) };
    let row: Option<Row> = db
        .prepare_cached(&format!("SELECT {COLS} FROM measure WHERE id=?1"))?
        .query_row([id], read_row)
        .optional()?;
    let Some((_, Some(spec))) = row else { return Ok(None) };
    if spec.pins.len() != 2 {
        return Ok(None);
    }
    match (&spec.pins[0].anchor, &spec.pins[1].anchor) {
        (
            Anchor::Dim(DimRef::WallEnd { wall: a, .. }),
            Anchor::Dim(DimRef::WallEnd { wall: b, .. }),
        ) if a != b && spec.kind == MeasureKind::Clear => {
            let mut cx = Ctx::new(db);
            let Value::Ok(g0) = evaluate(&mut cx, &spec, &[])?.value else { return Ok(None) };
            let lock = ConstraintSpec { kind: ConstraintKind::LockDist, walls: vec![*a, *b], value: None };
            let Some(d0) = crate::constraints::current_value(db, &lock)? else { return Ok(None) };
            let sign = if d0 < 0.0 { -1.0 } else { 1.0 };
            Ok(Some(ConstraintSpec { value: Some(sign * (target + d0.abs() - g0)), ..lock }))
        }
        (Anchor::Dim(DimRef::AxisLine { wall: a }), Anchor::Dim(DimRef::AxisLine { wall: b }))
            if a != b && spec.kind == MeasureKind::Distance =>
        {
            let lock = ConstraintSpec { kind: ConstraintKind::LockDist, walls: vec![*a, *b], value: None };
            let Some(d0) = crate::constraints::current_value(db, &lock)? else { return Ok(None) };
            let sign = if d0 < 0.0 { -1.0 } else { 1.0 };
            Ok(Some(ConstraintSpec { value: Some(sign * target), ..lock }))
        }
        (Anchor::Level { id: a }, Anchor::Level { id: b })
            if a != b && matches!(spec.kind, MeasureKind::Distance | MeasureKind::Clear) =>
        {
            let (Some(za), Some(zb)) = (level_elev(db, *a)?, level_elev(db, *b)?) else {
                return Ok(None);
            };
            let sign = if zb < za { -1.0 } else { 1.0 };
            Ok(Some(ConstraintSpec {
                kind: ConstraintKind::LevelGap,
                walls: vec![*a, *b],
                value: Some(sign * target),
            }))
        }
        _ => Ok(None),
    }
}

impl Model {
    /// A mass-face anchor for the face `ftag` as it is now (records the operation kind).
    pub fn mass_face_anchor(&self, mass: i64, ftag: u32) -> Result<Anchor, Error> {
        match crate::mass::anchor_face_op(&self.db, mass, ftag)? {
            Some(op) => Ok(Anchor::MassFace { mass, ftag, op }),
            None => Err(Error::Invalid(format!("mass {mass}: no face {ftag}"))),
        }
    }

    /// Pins for anchors at their present positions (the fallback of each is its point now).
    pub fn measure_pins(&self, anchors: &[Anchor]) -> Result<Vec<Pin>, Error> {
        let mut cx = Ctx::new(&self.db);
        let mut out = vec![];
        for a in anchors {
            match resolve(&mut cx, a)? {
                Ok(r) => out.push(Pin {
                    anchor: a.clone(),
                    fallback: r.p,
                }),
                Err(Fail::Orphan) => {
                    return Err(Error::Invalid("measurement references missing elements".into()))
                }
                Err(Fail::Unsupported(w)) => {
                    return Err(Error::Invalid(format!("measurement anchor unsupported: {w}")))
                }
            }
        }
        Ok(out)
    }

    /// The number a spec would have now; nothing is written.
    pub fn eval_measure_spec(&self, spec: &MeasureSpec) -> Value {
        if check(spec).is_err() {
            return Value::Unavailable(Reason::Unsupported("points_degenerate"));
        }
        let mut cx = Ctx::new(&self.db);
        match evaluate(&mut cx, spec, &[]) {
            Ok(o) => o.value,
            Err(_) => Value::Unavailable(Reason::Unsupported("points_degenerate")),
        }
    }

    /// Saves a measurement; refused when a pin names an element that does not exist.
    pub fn add_measure(&mut self, s: &MeasureSpec) -> Result<i64, Error> {
        Ok(self.add_measures(std::slice::from_ref(s))?[0])
    }

    /// Several measurements as one journal step.
    pub fn add_measures(&mut self, specs: &[MeasureSpec]) -> Result<Vec<i64>, Error> {
        let mut cx = Ctx::new(&self.db);
        for s in specs {
            check(s)?;
            for p in &s.pins {
                if matches!(resolve(&mut cx, &p.anchor)?, Err(Fail::Orphan)) {
                    return Err(Error::Invalid("measurement references missing elements".into()));
                }
            }
        }
        self.op("measure_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            let mut ins = db.prepare_cached(
                "INSERT INTO measure(kind,view_kind,view_ref,anchors,state,label) VALUES (?1,?2,?3,?4,'live',?5)",
            )?;
            for s in specs {
                let (vk, vr) = s.view.parts();
                ins.execute(params![s.kind.code(), vk, vr, pins_text(&s.pins), s.label])?;
                let id = db.last_insert_rowid();
                inv = inv.delete("measure", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn delete_measure(&mut self, id: i64) -> Result<(), Error> {
        self.op("measure_delete", |db| {
            let mut inv = Inv::default().restore(db, "measure", "id", id)?;
            // the twin rule goes in the same step
            if let Some((rule, _)) = twin_rule(db, id)? {
                inv = inv.restore(db, "constraint_rule", "id", rule)?;
                db.execute("DELETE FROM constraint_rule WHERE id=?1", [rule])?;
            }
            if db.execute("DELETE FROM measure WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("no such measurement {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// Renames a measurement (one journal step).
    pub fn set_measure_label(&mut self, id: i64, label: &str) -> Result<(), Error> {
        self.op("measure_label", |db| {
            let inv = Inv::default().restore(db, "measure", "id", id)?;
            if db.execute("UPDATE measure SET label=?2 WHERE id=?1", params![id, label])? == 0 {
                return Err(Error::Invalid(format!("no such measurement {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// The stored measurement with its state, value and display points.
    pub fn measure_info(&self, id: i64) -> Result<MeasureInfo, Error> {
        let row: Option<Row> = self
            .db
            .prepare_cached(&format!("SELECT {COLS} FROM measure WHERE id=?1"))?
            .query_row([id], read_row)
            .optional()?;
        match row {
            Some((id, Some(spec))) => info_of(&self.db, id, spec),
            _ => Err(Error::Invalid(format!("no such measurement {id}"))),
        }
    }

    /// The number of a stored measurement, or why there is none.
    pub fn measure_value(&self, id: i64) -> Result<Value, Error> {
        Ok(self.measure_info(id)?.value)
    }

    /// Every stored measurement, by id (rows of an unknown kind are skipped).
    pub fn all_measures(&self) -> Result<Vec<MeasureInfo>, Error> {
        let rows: Vec<Row> = {
            let mut st = self
                .db
                .prepare_cached(&format!("SELECT {COLS} FROM measure ORDER BY id"))?;
            let v = st.query_map([], read_row)?.collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for (id, spec) in rows {
            if let Some(s) = spec {
                out.push(info_of(&self.db, id, s)?);
            }
        }
        Ok(out)
    }

    /// Measurements of a view, by id (rows of an unknown kind are skipped).
    pub fn measures_in_view(&self, view: ViewKey) -> Result<Vec<MeasureInfo>, Error> {
        let (vk, vr) = view.parts();
        let rows: Vec<Row> = {
            let mut st = self.db.prepare_cached(&format!(
                "SELECT {COLS} FROM measure WHERE view_kind=?1 AND view_ref=?2 ORDER BY id"
            ))?;
            let v = st.query_map(params![vk, vr], read_row)?.collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for (id, spec) in rows {
            if let Some(s) = spec {
                out.push(info_of(&self.db, id, s)?);
            }
        }
        Ok(out)
    }
}

/// Which support of a driven measurement moves; the other stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

fn unsupported(why: &str) -> Error {
    Error::Invalid(format!("unsupported: {why}"))
}

impl Model {
    /// Drives a measurement to `value`. The `moving` support goes along the measured direction,
    /// the opposite one stays. Wall faces and levels: the measurement becomes a driving one (a
    /// twin rule in `constraint_rule`), and the constraint solver brings everything linked along
    /// (`=`, parallel, distance locks, level gaps); it keeps holding the number afterwards. A mass
    /// face is pushed or pulled. One journal step; a request the constraints cannot meet is a
    /// `Conflict` that lists them and leaves the model as it was.
    pub fn drive_measure(&mut self, id: i64, value: f64, moving: Side) -> Result<(), Error> {
        if !(value.is_finite() && value > 0.0) {
            return Err(Error::Invalid("driving value must be positive".into()));
        }
        let info = self.measure_info(id)?;
        let spec = &info.spec;
        if !matches!(spec.kind, MeasureKind::Distance | MeasureKind::Clear) || spec.pins.len() != 2 {
            return Err(unsupported(&format!("{} cannot be driven", spec.kind.code())));
        }
        let cur = match info.value {
            Value::Ok(v) => v,
            Value::Unavailable(r) => return Err(unsupported(&format!("no number ({})", r.code()))),
        };
        let (mi, fi) = if moving == Side::A { (0, 1) } else { (1, 0) };
        let (mv, fx) = (spec.pins[mi].anchor.clone(), spec.pins[fi].anchor.clone());
        let kind = spec.kind;
        match (&mv, &fx) {
            (Anchor::MassFace { mass, ftag, .. }, _) => {
                let (rm, rf) = {
                    let mut cx = Ctx::new(&self.db);
                    match (resolve(&mut cx, &mv)?, resolve(&mut cx, &fx)?) {
                        (Ok(a), Ok(b)) => (a, b),
                        _ => return Err(unsupported("support not found")),
                    }
                };
                let Some(n) = rm.n else { return Err(unsupported("curved face")) };
                let s = dot(sub(rm.p, rf.p), n);
                // the face moves along its normal: outward when it is the far one, and outward
                // when the supports coincide (zero gap)
                let sign = if s.abs() < 1e-9 { 1.0 } else { s.signum() };
                let d = sign * (value - cur);
                let (mass, ftag) = (*mass, *ftag);
                self.edit_mass_form(mass, |f| crate::Form::PushPull { base: f, face: ftag, d, keys: vec![] })
            }
            (Anchor::Dim(DimRef::WallEnd { wall: wm, .. }), Anchor::Dim(DimRef::WallEnd { wall: wf, .. }))
                if wm != wf && kind == MeasureKind::Clear =>
            {
                let (wf, target) = (*wf, value);
                self.drive_with_twin(id, target, move |db| {
                    crate::constraints::propagate_refs(db, &[crate::constraints::Ref::Wall(wf)])
                })
            }
            (Anchor::Level { id: lm }, Anchor::Level { id: lf }) if lm != lf => {
                let lf = *lf;
                self.drive_with_twin(id, value, move |db| crate::constraints::propagate_levels(db, lf))
            }
            (Anchor::Dim(DimRef::AxisLine { wall: wm }), Anchor::Dim(DimRef::AxisLine { wall: wf }))
                if wm != wf && kind == MeasureKind::Distance =>
            {
                let wf = *wf;
                self.drive_with_twin(id, value, move |db| {
                    crate::constraints::propagate_refs(db, &[crate::constraints::Ref::Wall(wf)])
                })
            }
            (Anchor::Dim(DimRef::AxisLine { .. }), _) | (_, Anchor::Dim(DimRef::AxisLine { .. })) => {
                Err(unsupported("axis_anchor"))
            }
            (Anchor::Dim(DimRef::WallEnd { .. }), Anchor::Dim(DimRef::WallEnd { .. })) => {
                Err(unsupported("wall_points_need_clear"))
            }
            _ => Err(unsupported("only wall faces, levels and mass faces can be driven")),
        }
    }

    /// Writes or updates the twin rule and runs `solve` with the fixed support pinned.
    fn drive_with_twin(
        &mut self,
        id: i64,
        target: f64,
        solve: impl FnOnce(&Connection) -> Result<Inv, Error>,
    ) -> Result<(), Error> {
        self.op("measure_drive", |db| {
            let mut inv = Inv::default();
            let rule = match twin_rule(db, id)? {
                Some((rule, _)) => {
                    inv = inv.restore(db, "constraint_rule", "id", rule)?;
                    db.execute("UPDATE constraint_rule SET value=?2 WHERE id=?1", params![rule, target])?;
                    rule
                }
                None => {
                    db.execute(
                        "INSERT INTO constraint_rule(kind, walls, value) VALUES ('measure', ?1, ?2)",
                        params![id.to_string(), target],
                    )?;
                    let rule = db.last_insert_rowid();
                    inv = inv.delete("constraint_rule", "id", rule);
                    rule
                }
            };
            if twin_spec(db, &id.to_string(), target)?.is_none() {
                return Err(unsupported("supports cannot be held"));
            }
            let _ = rule;
            let inv = inv.then(solve(db)?);
            Ok(((), id, inv))
        })
    }

    /// Makes a measurement driving at its present number (or releases it): the number then holds
    /// as a constraint. One journal step.
    pub fn set_measure_driving(&mut self, id: i64, on: bool) -> Result<(), Error> {
        let info = self.measure_info(id)?;
        if on == info.driving {
            return Ok(());
        }
        let Value::Ok(v) = info.value else {
            return Err(unsupported("no number to hold"));
        };
        if on {
            self.drive_with_twin(id, v, |_| Ok(Inv::default()))
        } else {
            self.op("measure_release", |db| {
                let Some((rule, _)) = twin_rule(db, id)? else {
                    return Ok(((), id, Inv::default()));
                };
                let inv = Inv::default().restore(db, "constraint_rule", "id", rule)?;
                db.execute("DELETE FROM constraint_rule WHERE id=?1", [rule])?;
                Ok(((), id, inv))
            })
        }
    }
}
