// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Dimensions as model objects (`dim`): references to wall faces/axes and opening edges; geometry
//! is recomputed after every edit (`dim_geo`, derived). A reference to a deleted element makes
//! an "orphan": the dimension keeps its last known geometry and the `orphan` flag.
//! Auto-dimensioning: exterior faces (a ray outward hits no other walls) get chains of
//! openings, axes and overall size along a face of any orientation; arc walls get arc lengths and radius.

use std::collections::BTreeMap;

use ddc_geom::arc::ArcAxis;
use ddc_geom::dim::{self, Linear};
use ddc_geom::{v2, Tol, V2};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

type P = (f64, f64);

/// Anchor point/object of a dimension. `face` 0 is the `offset0` face, 1 is `offset0 + thickness`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DimRef {
    /// Non-associative point, m.
    Point(P),
    /// Wall face at its start (`end` 0) or end (1).
    WallEnd { wall: i64, face: u8, end: u8 },
    /// Opening edge (`edge` 0 is start, 1 is end) on a face of the host wall.
    OpeningEdge { opening: i64, edge: u8, face: u8 },
    /// End of the wall axis.
    AxisEnd { wall: i64, end: u8 },
    /// Axis of a straight wall as a line (angular dimension).
    AxisLine { wall: i64 },
    /// Face of an arc wall as a circle (radius).
    ArcFace { wall: i64, face: u8 },
    /// Bottom (`top` false) or top of the wall: an elevation mark.
    WallLevel { wall: i64, top: bool },
    /// Intersection of a straight grid axis with a straight wall axis.
    GridAt { grid: i64, wall: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DimKind {
    /// refs: [a, b] or [a, b, line base]; `dir` (0,0) means along a→b.
    Linear,
    /// refs: [AxisLine, AxisLine].
    Angular,
    /// refs: [ArcFace].
    Radial,
    /// refs: [point, point] on one arc wall (WallEnd/OpeningEdge).
    ArcLength,
    /// refs: [WallLevel, Point].
    Spot,
}

impl DimKind {
    fn code(self) -> &'static str {
        match self {
            DimKind::Linear => "linear",
            DimKind::Angular => "angular",
            DimKind::Radial => "radial",
            DimKind::ArcLength => "arc",
            DimKind::Spot => "spot",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "linear" => DimKind::Linear,
            "angular" => DimKind::Angular,
            "radial" => DimKind::Radial,
            "arc" => DimKind::ArcLength,
            "spot" => DimKind::Spot,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DimSpec {
    pub level: i64,
    pub kind: DimKind,
    pub refs: Vec<DimRef>,
    /// Measuring direction of a linear dimension; (0, 0) means along the points.
    pub dir: P,
    /// +1 puts the line left of the direction, -1 right (for an arc: +1 is outward).
    pub side: f64,
    /// Offset of the line from the base in model space, m.
    pub offset: f64,
    /// Chain tier: the sheet adds `first + tier·step` mm; < 0 means no paper offset.
    pub tier: i32,
    /// Created by auto-dimensioning (overwritten on a re-run).
    pub auto: bool,
}

/// Computed dimension geometry in model coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum DimGeom {
    Linear(Linear),
    Angular {
        center: P,
        radius: f64,
        a0: f64,
        a1: f64,
        value: f64,
    },
    /// Face arc of radius `radius`; angles in degrees, counter-clockwise from `a0` to `a1`.
    Arc {
        center: P,
        radius: f64,
        a0: f64,
        a1: f64,
        value: f64,
    },
    Radial {
        center: P,
        radius: f64,
        angle: f64,
    },
    Spot {
        at: P,
        value: f64,
    },
}

impl DimGeom {
    /// Value: length/radius, m; angle, degrees; elevation, m.
    pub fn value(&self) -> f64 {
        match self {
            DimGeom::Linear(l) => l.value(),
            DimGeom::Angular { value, .. }
            | DimGeom::Arc { value, .. }
            | DimGeom::Spot { value, .. } => *value,
            DimGeom::Radial { radius, .. } => *radius,
        }
    }
    fn text(&self) -> String {
        let j = |v: &[f64]| {
            v.iter()
                .map(|x| format!("{x:?}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        match self {
            DimGeom::Linear(l) => format!(
                "L {}",
                j(&[
                    l.a.x, l.a.y, l.b.x, l.b.y, l.dir.x, l.dir.y, l.n.x, l.n.y, l.base.x, l.base.y
                ])
            ),
            DimGeom::Angular {
                center,
                radius,
                a0,
                a1,
                value,
            } => {
                format!("G {}", j(&[center.0, center.1, *radius, *a0, *a1, *value]))
            }
            DimGeom::Arc {
                center,
                radius,
                a0,
                a1,
                value,
            } => {
                format!("A {}", j(&[center.0, center.1, *radius, *a0, *a1, *value]))
            }
            DimGeom::Radial {
                center,
                radius,
                angle,
            } => {
                format!("R {}", j(&[center.0, center.1, *radius, *angle]))
            }
            DimGeom::Spot { at, value } => format!("S {}", j(&[at.0, at.1, *value])),
        }
    }
    fn parse(s: &str) -> Option<Self> {
        let mut it = s.split(' ');
        let k = it.next()?;
        let v: Vec<f64> = it.map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let n = |c: usize| (v.len() == c).then_some(());
        Some(match k {
            "L" => {
                n(10)?;
                DimGeom::Linear(Linear {
                    a: v2(v[0], v[1]),
                    b: v2(v[2], v[3]),
                    dir: v2(v[4], v[5]),
                    n: v2(v[6], v[7]),
                    base: v2(v[8], v[9]),
                })
            }
            "G" | "A" => {
                n(6)?;
                let (center, radius, a0, a1, value) = ((v[0], v[1]), v[2], v[3], v[4], v[5]);
                if k == "G" {
                    DimGeom::Angular {
                        center,
                        radius,
                        a0,
                        a1,
                        value,
                    }
                } else {
                    DimGeom::Arc {
                        center,
                        radius,
                        a0,
                        a1,
                        value,
                    }
                }
            }
            "R" => {
                n(4)?;
                DimGeom::Radial {
                    center: (v[0], v[1]),
                    radius: v[2],
                    angle: v[3],
                }
            }
            "S" => {
                n(3)?;
                DimGeom::Spot {
                    at: (v[0], v[1]),
                    value: v[2],
                }
            }
            _ => return None,
        })
    }
}

/// A level dimension with geometry; `orphan` means the reference no longer resolves and the geometry is the last known.
#[derive(Clone, Debug, PartialEq)]
pub struct Annot {
    pub id: i64,
    pub spec: DimSpec,
    pub geom: DimGeom,
    pub orphan: bool,
}

pub(crate) fn ref_text(r: &DimRef) -> String {
    match *r {
        DimRef::Point((x, y)) => format!("P {x:?} {y:?}"),
        DimRef::WallEnd { wall, face, end } => format!("W {wall} {face} {end}"),
        DimRef::OpeningEdge {
            opening,
            edge,
            face,
        } => format!("O {opening} {edge} {face}"),
        DimRef::AxisEnd { wall, end } => format!("X {wall} {end}"),
        DimRef::AxisLine { wall } => format!("N {wall}"),
        DimRef::ArcFace { wall, face } => format!("C {wall} {face}"),
        DimRef::WallLevel { wall, top } => format!("Z {wall} {}", top as u8),
        DimRef::GridAt { grid, wall } => format!("G {grid} {wall}"),
    }
}

pub(crate) fn parse_ref(s: &str) -> Option<DimRef> {
    let v: Vec<&str> = s.split(' ').collect();
    let i = |k: usize| v.get(k)?.parse::<i64>().ok();
    let u = |k: usize| v.get(k)?.parse::<u8>().ok().filter(|x| *x <= 1);
    Some(match (v.first()?, v.len()) {
        (&"P", 3) => DimRef::Point((v[1].parse().ok()?, v[2].parse().ok()?)),
        (&"W", 4) => DimRef::WallEnd {
            wall: i(1)?,
            face: u(2)?,
            end: u(3)?,
        },
        (&"O", 4) => DimRef::OpeningEdge {
            opening: i(1)?,
            edge: u(2)?,
            face: u(3)?,
        },
        (&"X", 3) => DimRef::AxisEnd {
            wall: i(1)?,
            end: u(2)?,
        },
        (&"N", 2) => DimRef::AxisLine { wall: i(1)? },
        (&"C", 3) => DimRef::ArcFace {
            wall: i(1)?,
            face: u(2)?,
        },
        (&"Z", 3) => DimRef::WallLevel {
            wall: i(1)?,
            top: u(2)? == 1,
        },
        (&"G", 3) => DimRef::GridAt {
            grid: i(1)?,
            wall: i(2)?,
        },
        _ => return None,
    })
}

pub(crate) fn refs_text(rs: &[DimRef]) -> String {
    rs.iter().map(ref_text).collect::<Vec<_>>().join(";")
}

struct W {
    a: P,
    b: P,
    offset0: f64,
    total: f64,
    base: f64,
    height: f64,
    arc: Option<ArcAxis>,
}

impl W {
    fn len(&self) -> f64 {
        match &self.arc {
            Some(ax) => ax.length(),
            None => ((self.b.0 - self.a.0).powi(2) + (self.b.1 - self.a.1).powi(2)).sqrt(),
        }
    }
    fn face(&self, f: u8) -> f64 {
        self.offset0 + if f == 1 { self.total } else { 0.0 }
    }
    fn at(&self, t: f64, o: f64) -> V2 {
        match &self.arc {
            Some(ax) => {
                let a = ax.angle_at(t);
                let r = ax.radius_at(o);
                v2(ax.center.x + r * a.cos(), ax.center.y + r * a.sin())
            }
            None => {
                let (dx, dy) = (self.b.0 - self.a.0, self.b.1 - self.a.1);
                let l = (dx * dx + dy * dy).sqrt();
                let (d, n) = ((dx / l, dy / l), (-dy / l, dx / l));
                v2(self.a.0 + d.0 * t + n.0 * o, self.a.1 + d.1 * t + n.1 * o)
            }
        }
    }
}

/// Cache of walls/openings for one resolution pass.
pub(crate) struct Ctx<'a> {
    pub(crate) db: &'a Connection,
    walls: BTreeMap<i64, Option<W>>,
}

/// A wall as the measurements see it: straight axis, base elevation, and the two face offsets.
pub(crate) struct WallFrame {
    pub a: P,
    pub b: P,
    pub base: f64,
    pub height: f64,
    pub straight: bool,
    pub face: [f64; 2],
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(db: &'a Connection) -> Self {
        Ctx {
            db,
            walls: BTreeMap::new(),
        }
    }

    pub(crate) fn frame(&mut self, id: i64) -> Result<Option<WallFrame>, Error> {
        Ok(self.wall(id)?.map(|w| WallFrame {
            a: w.a,
            b: w.b,
            base: w.base,
            height: w.height,
            straight: w.arc.is_none(),
            face: [w.face(0), w.face(1)],
        }))
    }

    /// A point reference in model space: the host wall (if any) and x, y, z (z is the wall base).
    pub(crate) fn point3(&mut self, r: &DimRef) -> Result<Option<(Option<i64>, [f64; 3])>, Error> {
        Ok(match self.point(r)? {
            None => None,
            Some((wall, _, p)) => {
                let z = match wall {
                    Some(w) => self.wall(w)?.map_or(0.0, |w| w.base),
                    None => 0.0,
                };
                Some((wall, [p.x, p.y, z]))
            }
        })
    }
}

impl Ctx<'_> {
    fn wall(&mut self, id: i64) -> Result<Option<&W>, Error> {
        if !self.walls.contains_key(&id) {
            let row: Option<([f64; 6], Option<f64>, f64, i64, f64)> = self
                .db
                .query_row(
                    "SELECT w.ax,w.ay,w.bx,w.by,w.base_offset,w.height,w.offset0,w.bulge,w.wtype,l.elevation
                     FROM wall w JOIN level l ON l.id=w.level WHERE w.id=?1",
                    [id],
                    |r| {
                        let mut f = [0.0; 6];
                        for (k, s) in f.iter_mut().enumerate() {
                            *s = r.get(k)?;
                        }
                        f[4] += r.get::<_, f64>(9)?;
                        Ok((f, r.get(6)?, r.get(7)?, r.get(8)?, 0.0))
                    },
                )
                .optional()?;
            let w = match row {
                None => None,
                Some((f, off, bulge, wt, _)) => {
                    let total: f64 = self.db.query_row(
                        "SELECT COALESCE(SUM(thickness),0) FROM wall_type_layer WHERE wtype=?1",
                        [wt],
                        |r| r.get(0),
                    )?;
                    let arc = if bulge != 0.0 {
                        ArcAxis::new(v2(f[0], f[1]), v2(f[2], f[3]), bulge, Tol::default()).ok()
                    } else {
                        None
                    };
                    Some(W {
                        a: (f[0], f[1]),
                        b: (f[2], f[3]),
                        offset0: off.unwrap_or(-total / 2.0),
                        total,
                        base: f[4],
                        height: f[5],
                        arc,
                    })
                }
            };
            self.walls.insert(id, w);
        }
        Ok(self.walls[&id].as_ref())
    }

    /// Point reference to (wall or None, parameter along the axis, point).
    fn point(&mut self, r: &DimRef) -> Result<Option<(Option<i64>, f64, V2)>, Error> {
        Ok(match *r {
            DimRef::Point((x, y)) => Some((None, 0.0, v2(x, y))),
            DimRef::WallEnd { wall, face, end } => self.wall(wall)?.map(|w| {
                let t = if end == 1 { w.len() } else { 0.0 };
                (Some(wall), t, w.at(t, w.face(face)))
            }),
            DimRef::AxisEnd { wall, end } => self.wall(wall)?.map(|w| {
                let t = if end == 1 { w.len() } else { 0.0 };
                (Some(wall), t, w.at(t, 0.0))
            }),
            DimRef::OpeningEdge {
                opening,
                edge,
                face,
            } => {
                let o: Option<(i64, f64, f64)> = self
                    .db
                    .query_row(
                        "SELECT wall,along,width FROM opening WHERE id=?1",
                        [opening],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                match o {
                    None => None,
                    Some((wid, along, width)) => self.wall(wid)?.map(|w| {
                        let t = along + if edge == 1 { width } else { 0.0 };
                        (Some(wid), t, w.at(t, w.face(face)))
                    }),
                }
            }
            DimRef::GridAt { grid, wall } => {
                let g: Option<(f64, f64, f64, f64, f64)> = self
                    .db
                    .query_row(
                        "SELECT ax,ay,bx,by,bulge FROM grid WHERE id=?1",
                        [grid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?;
                let Some((gx, gy, hx, hy, _)) = g.filter(|g| g.4 == 0.0) else {
                    return Ok(None);
                };
                self.wall(wall)?.filter(|w| w.arc.is_none()).and_then(|w| {
                    let (p, d) = (v2(w.a.0, w.a.1), v2(w.b.0 - w.a.0, w.b.1 - w.a.1));
                    let (q, e) = (v2(gx, gy), v2(hx - gx, hy - gy));
                    let den = d.x * e.y - d.y * e.x;
                    if den.abs() < 1e-12 * d.len() * e.len() {
                        return None;
                    }
                    let s = ((q.x - p.x) * e.y - (q.y - p.y) * e.x) / den;
                    let t = s * d.len();
                    Some((Some(wall), t, p + d * s))
                })
            }
            _ => None,
        })
    }

    fn resolve(&mut self, s: &DimSpec) -> Result<Option<DimGeom>, Error> {
        let bad = || Error::Invalid(format!("{} dimension: wrong references", s.kind.code()));
        Ok(match s.kind {
            DimKind::Linear => {
                if !(2..=3).contains(&s.refs.len()) {
                    return Err(bad());
                }
                let mut ps = vec![];
                for r in &s.refs {
                    match self.point(r)? {
                        Some(p) => ps.push(p.2),
                        None if matches!(
                            r,
                            DimRef::AxisLine { .. }
                                | DimRef::ArcFace { .. }
                                | DimRef::WallLevel { .. }
                        ) =>
                        {
                            return Err(bad())
                        }
                        None => return Ok(None),
                    }
                }
                let dir = (s.dir != (0.0, 0.0)).then(|| v2(s.dir.0, s.dir.1));
                Linear::new(ps[0], ps[1], dir, s.side, ps.get(2).copied(), s.offset)
                    .map(DimGeom::Linear)
            }
            DimKind::Angular => {
                let [DimRef::AxisLine { wall: w1 }, DimRef::AxisLine { wall: w2 }] = s.refs[..]
                else {
                    return Err(bad());
                };
                let line = |c: &mut Self, id| -> Result<Option<(V2, V2)>, Error> {
                    Ok(c.wall(id)?
                        .filter(|w| w.arc.is_none())
                        .map(|w| (v2(w.a.0, w.a.1), v2(w.b.0 - w.a.0, w.b.1 - w.a.1))))
                };
                let (Some(l1), Some(l2)) = (line(self, w1)?, line(self, w2)?) else {
                    return Ok(None);
                };
                dim::angular(l1.0, l1.1, l2.0, l2.1).map(|a| DimGeom::Angular {
                    center: (a.center.x, a.center.y),
                    radius: s.offset.abs().max(1e-3),
                    a0: a.a0,
                    a1: a.a1,
                    value: a.value,
                })
            }
            DimKind::Radial => {
                let [DimRef::ArcFace { wall, face }] = s.refs[..] else {
                    return Err(bad());
                };
                self.wall(wall)?.and_then(|w| {
                    let ax = w.arc.as_ref()?;
                    Some(DimGeom::Radial {
                        center: (ax.center.x, ax.center.y),
                        radius: ax.radius_at(w.face(face)),
                        angle: dim::deg(ax.point_at(w.len() / 2.0) - ax.center),
                    })
                })
            }
            DimKind::ArcLength => {
                if s.refs.len() != 2 {
                    return Err(bad());
                }
                let (Some(p), Some(q)) = (self.point(&s.refs[0])?, self.point(&s.refs[1])?) else {
                    return Ok(None);
                };
                let (Some(wid), Some(wq)) = (p.0, q.0) else {
                    return Err(bad());
                };
                if wid != wq {
                    return Err(bad());
                }
                let face = match s.refs[0] {
                    DimRef::WallEnd { face, .. } | DimRef::OpeningEdge { face, .. } => face,
                    _ => return Err(bad()),
                };
                let w = self.wall(wid)?.ok_or_else(bad)?;
                let Some(ax) = w.arc.as_ref() else {
                    return Err(bad());
                };
                let r = ax.radius_at(w.face(face));
                let (t0, t1) = (ax.angle_at(p.1.min(q.1)), ax.angle_at(p.1.max(q.1)));
                let (a0, a1) = if ax.sweep > 0.0 { (t0, t1) } else { (t1, t0) };
                Some(DimGeom::Arc {
                    center: (ax.center.x, ax.center.y),
                    radius: r,
                    a0: dim::deg(v2(a0.cos(), a0.sin())),
                    a1: dim::deg(v2(a1.cos(), a1.sin())),
                    value: dim::arc_length(r, t0, t1),
                })
            }
            DimKind::Spot => {
                let [DimRef::WallLevel { wall, top }, DimRef::Point(at)] = s.refs[..] else {
                    return Err(bad());
                };
                self.wall(wall)?.map(|w| DimGeom::Spot {
                    at,
                    value: w.base + if top { w.height } else { 0.0 },
                })
            }
        })
    }
}

fn check(s: &DimSpec) -> Result<(), Error> {
    let fin = [s.dir.0, s.dir.1, s.side, s.offset]
        .iter()
        .all(|v| v.is_finite());
    let pts = s.refs.iter().all(|r| match r {
        DimRef::Point(p) => p.0.is_finite() && p.1.is_finite(),
        _ => true,
    });
    if !fin || !pts {
        return Err(Error::Invalid("dimension values must be finite".into()));
    }
    Ok(())
}

fn read_spec(r: &rusqlite::Row) -> rusqlite::Result<(i64, Option<DimSpec>)> {
    let id: i64 = r.get(0)?;
    let kind: String = r.get(2)?;
    let refs: String = r.get(3)?;
    let refs: Option<Vec<DimRef>> = refs
        .split(';')
        .filter(|x| !x.is_empty())
        .map(parse_ref)
        .collect();
    Ok((
        id,
        DimKind::parse(&kind).zip(refs).map(|(kind, refs)| DimSpec {
            level: r.get(1).unwrap_or(0),
            kind,
            refs,
            dir: (r.get(4).unwrap_or(0.0), r.get(5).unwrap_or(0.0)),
            side: r.get(6).unwrap_or(1.0),
            offset: r.get(7).unwrap_or(0.0),
            tier: r.get(8).unwrap_or(-1),
            auto: r.get::<_, i64>(9).unwrap_or(0) != 0,
        }),
    ))
}

const COLS: &str = "id,level,kind,refs,dx,dy,side,offset,tier,auto";

fn insert(db: &Connection, s: &DimSpec) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO dim(level,kind,refs,dx,dy,side,offset,tier,auto) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![s.level, s.kind.code(), refs_text(&s.refs), s.dir.0, s.dir.1, s.side, s.offset, s.tier, s.auto as i64],
    )?;
    Ok(db.last_insert_rowid())
}

/// Derived `dim_geo`: resolvable dimensions get new geometry, the rest become orphans keeping the old.
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute(
        "DELETE FROM dim_geo WHERE dim NOT IN (SELECT id FROM dim)",
        [],
    )?;
    let rows: Vec<(i64, Option<DimSpec>)> = {
        let mut st = db.prepare(&format!(
            "SELECT {COLS} FROM dim WHERE kind<>'tag' ORDER BY id"
        ))?;
        let v = st.query_map([], read_spec)?.collect::<Result<_, _>>()?;
        v
    };
    let mut cx = Ctx {
        db,
        walls: BTreeMap::new(),
    };
    for (id, s) in rows {
        let g = match &s {
            Some(s) => cx.resolve(s).ok().flatten(),
            None => None,
        };
        match g {
            Some(g) => {
                db.execute(
                    "INSERT OR REPLACE INTO dim_geo(dim,orphan,geom) VALUES (?1,0,?2)",
                    params![id, g.text()],
                )?;
            }
            None => {
                db.execute("UPDATE dim_geo SET orphan=1 WHERE dim=?1", [id])?;
            }
        }
    }
    Ok(())
}

/// Wall footprint (outline along the faces) for checking exterior faces.
struct Foot {
    id: i64,
    ring: Vec<V2>,
}

impl Model {
    pub fn add_dim(&mut self, s: &DimSpec) -> Result<i64, Error> {
        check(s)?;
        let mut cx = Ctx {
            db: &self.db,
            walls: BTreeMap::new(),
        };
        if cx.resolve(s)?.is_none() {
            return Err(Error::Invalid(
                "dimension references missing elements".into(),
            ));
        }
        self.op("dim_add", |db| {
            let id = insert(db, s)?;
            Ok((id, id, Inv::default().delete("dim", "id", id)))
        })
    }

    /// A dimension chain in one operation (one undo step).
    pub fn add_dims(&mut self, specs: &[DimSpec]) -> Result<Vec<i64>, Error> {
        let mut cx = Ctx {
            db: &self.db,
            walls: BTreeMap::new(),
        };
        for s in specs {
            check(s)?;
            if cx.resolve(s)?.is_none() {
                return Err(Error::Invalid(
                    "dimension references missing elements".into(),
                ));
            }
        }
        self.op("dim_add", |db| {
            let mut inv = Inv::default();
            let mut ids = vec![];
            for s in specs {
                let id = insert(db, s)?;
                inv = inv.delete("dim", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn update_dim(&mut self, id: i64, s: &DimSpec) -> Result<(), Error> {
        check(s)?;
        let mut cx = Ctx {
            db: &self.db,
            walls: BTreeMap::new(),
        };
        if cx.resolve(s)?.is_none() {
            return Err(Error::Invalid(
                "dimension references missing elements".into(),
            ));
        }
        self.op("dim_update", |db| {
            let inv = Inv::default().restore(db, "dim", "id", id)?;
            let n = db.execute(
                "UPDATE dim SET level=?2,kind=?3,refs=?4,dx=?5,dy=?6,side=?7,offset=?8,tier=?9,auto=?10 WHERE id=?1",
                params![id, s.level, s.kind.code(), refs_text(&s.refs), s.dir.0, s.dir.1, s.side, s.offset, s.tier, s.auto as i64],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such dimension {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_dim(&mut self, id: i64) -> Result<(), Error> {
        self.op("dim_delete", |db| {
            let inv = Inv::default().restore(db, "dim", "id", id)?;
            if db.execute("DELETE FROM dim WHERE id=?1", [id])? == 0 {
                return Err(Error::Invalid(format!("no such dimension {id}")));
            }
            Ok(((), id, inv))
        })
    }

    /// Level dimensions in ascending id order (rows with unrecognised references are skipped).
    pub fn dims(&self, level: i64) -> Result<Vec<Annot>, Error> {
        let mut st = self.db.prepare(
            "SELECT d.id,d.level,d.kind,d.refs,d.dx,d.dy,d.side,d.offset,d.tier,d.auto,g.orphan,g.geom
             FROM dim d JOIN dim_geo g ON g.dim=d.id WHERE d.level=?1 ORDER BY d.id",
        )?;
        let rows = st.query_map([level], |r| {
            let (id, s) = read_spec(r)?;
            Ok((id, s, r.get::<_, i64>(10)? != 0, r.get::<_, String>(11)?))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, s, orphan, g) = row?;
            if let (Some(spec), Some(geom)) = (s, DimGeom::parse(&g)) {
                out.push(Annot {
                    id,
                    spec,
                    geom,
                    orphan,
                });
            }
        }
        Ok(out)
    }

    /// Auto-dimensions a level: replaces earlier auto dimensions (one undoable operation).
    /// Returns the ids of the new dimensions.
    pub fn auto_dimension(&mut self, level: i64) -> Result<Vec<i64>, Error> {
        let specs = auto_specs(&self.db, level)?;
        self.op("dim_auto", |db| {
            let old: Vec<i64> = {
                let mut st =
                    db.prepare("SELECT id FROM dim WHERE level=?1 AND auto=1 ORDER BY id")?;
                let v = st
                    .query_map([level], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                v
            };
            let mut inv = Inv::default();
            let mut ids = vec![];
            for s in &specs {
                let id = insert(db, s)?;
                inv = inv.then(Inv::default().delete("dim", "id", id));
                ids.push(id);
            }
            for id in old {
                inv = inv.restore(db, "dim", "id", id)?;
                db.execute("DELETE FROM dim WHERE id=?1", [id])?;
            }
            Ok((ids, level, inv))
        })
    }
}

/// Facade candidate: a straight wall face with outward normal `n`.
struct Facade {
    dir: V2,
    n: V2,
    /// Position of the face line: p·n.
    h: f64,
    walls: Vec<(i64, u8)>,
}

fn auto_specs(db: &Connection, level: i64) -> Result<Vec<DimSpec>, Error> {
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM wall WHERE level=?1 ORDER BY id")?;
        let v = st
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut cx = Ctx {
        db,
        walls: BTreeMap::new(),
    };
    let mut feet = vec![];
    for &id in &ids {
        let Some(w) = cx.wall(id)? else { continue };
        let (o0, o1) = (w.face(0), w.face(1));
        let n = if w.arc.is_some() { 32 } else { 1 };
        let l = w.len();
        let mut ring: Vec<V2> = (0..=n).map(|k| w.at(l * k as f64 / n as f64, o0)).collect();
        ring.extend((0..=n).rev().map(|k| w.at(l * k as f64 / n as f64, o1)));
        feet.push(Foot { id, ring });
    }
    // Step outward from the face and face-coplanarity tolerance, m.
    let (step, eps) = (1e-4, 1e-6);
    // Exterior face: a ray outward from the face midpoint hits no other walls.
    let exterior = |id: i64, p: V2, n: V2| {
        let o = p + n * step;
        feet.iter()
            .filter(|f| f.id != id)
            .all(|f| !dim::ray_hits(o, n, &f.ring, 0.0))
    };
    let mut facades: Vec<Facade> = vec![];
    let mut arcs: Vec<(i64, u8)> = vec![];
    for &id in &ids {
        let Some(w) = cx.wall(id)? else { continue };
        let l = w.len();
        if l < eps || w.total <= 0.0 {
            continue;
        }
        for face in [0u8, 1] {
            let (o, oth) = (w.face(face), w.face(1 - face));
            let m = w.at(l / 2.0, o);
            let inward = w.at(l / 2.0, oth) - m;
            let nl = inward.len();
            let n = inward * (-1.0 / nl);
            if !exterior(id, m, n) {
                continue;
            }
            if w.arc.is_some() {
                arcs.push((id, face));
                continue;
            }
            // n rotated by -90 degrees: "left of dir" points outward
            let dir = v2(n.y, -n.x);
            let h = m.dot(n);
            match facades
                .iter_mut()
                .find(|f| (f.n - n).len() < 1e-9 && (f.h - h).abs() < eps)
            {
                Some(f) => f.walls.push((id, face)),
                None => facades.push(Facade {
                    dir,
                    n,
                    h,
                    walls: vec![(id, face)],
                }),
            }
        }
    }
    let ops: Vec<(i64, i64)> = {
        let mut st = db.prepare(
            "SELECT o.id,o.wall FROM opening o JOIN wall w ON w.id=o.wall WHERE w.level=?1 ORDER BY o.id",
        )?;
        let v = st
            .query_map([level], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    let lin = |refs: Vec<DimRef>, f: &Facade, tier: i32| DimSpec {
        level,
        kind: DimKind::Linear,
        refs,
        dir: (f.dir.x, f.dir.y),
        side: 1.0,
        offset: 0.0,
        tier,
        auto: true,
    };
    for f in &facades {
        let base = DimRef::WallEnd {
            wall: f.walls[0].0,
            face: f.walls[0].1,
            end: 0,
        };
        let mut ends: Vec<(DimRef, V2)> = vec![];
        let mut opens: Vec<(DimRef, V2)> = vec![];
        for &(wid, face) in &f.walls {
            for end in [0u8, 1] {
                let r = DimRef::WallEnd {
                    wall: wid,
                    face,
                    end,
                };
                ends.push((
                    r,
                    cx.point(&r)?
                        .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                        .2,
                ));
            }
            for &(oid, _) in ops.iter().filter(|o| o.1 == wid) {
                for edge in [0u8, 1] {
                    let r = DimRef::OpeningEdge {
                        opening: oid,
                        edge,
                        face,
                    };
                    opens.push((
                        r,
                        cx.point(&r)?
                            .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                            .2,
                    ));
                }
            }
        }
        let t = |p: V2| p.dot(f.dir);
        let lo = ends
            .iter()
            .min_by(|a, b| t(a.1).total_cmp(&t(b.1)))
            .ok_or_else(|| Error::Invalid("dimension face has no walls".into()))?
            .0;
        let hi = ends
            .iter()
            .max_by(|a, b| t(a.1).total_cmp(&t(b.1)))
            .ok_or_else(|| Error::Invalid("dimension face has no walls".into()))?
            .0;
        let (tlo, thi) = (
            t(cx.point(&lo)?
                .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                .2),
            t(cx.point(&hi)?
                .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                .2),
        );
        let push_chain = |pts: Vec<(DimRef, V2)>, tier: i32, out: &mut Vec<DimSpec>| {
            let vs: Vec<V2> = pts.iter().map(|p| p.1).collect();
            for (i, j) in dim::chain(&vs, f.dir, 1e-6) {
                out.push(lin(vec![pts[i].0, pts[j].0, base], f, tier));
            }
        };
        let mut tier = 0;
        if !opens.is_empty() {
            let mut pts = vec![
                (
                    lo,
                    cx.point(&lo)?
                        .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                        .2,
                ),
                (
                    hi,
                    cx.point(&hi)?
                        .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                        .2,
                ),
            ];
            pts.extend(opens);
            push_chain(pts, tier, &mut out);
            tier += 1;
        }
        // Axes of walls abutting the face (axis end no farther than the thicknesses from the face line).
        let mut axes: Vec<(DimRef, V2)> = vec![];
        let fthick = f
            .walls
            .iter()
            .filter_map(|x| cx.walls[&x.0].as_ref())
            .map(|x| x.total)
            .fold(0.0, f64::max);
        for &id in &ids {
            let Some(w) = cx.wall(id)? else { continue };
            let reach = w.total + fthick + 1e-6;
            let len = w.len();
            for end in [0u8, 1] {
                let p = w.at(if end == 1 { len } else { 0.0 }, 0.0);
                let tp = t(p);
                if (f.h - p.dot(f.n)).abs() <= reach && tp > tlo - 1e-6 && tp < thi + 1e-6 {
                    axes.push((DimRef::AxisEnd { wall: id, end }, p));
                }
            }
        }
        let distinct = {
            let vs: Vec<V2> = axes.iter().map(|p| p.1).collect();
            dim::chain(&vs, f.dir, 1e-6).len()
        };
        if distinct >= 2 {
            push_chain(axes, tier, &mut out);
            tier += 1;
        }
        out.push(lin(vec![lo, hi, base], f, tier));
    }
    for (wid, face) in arcs {
        let mut pts: Vec<(DimRef, f64)> = vec![];
        for end in [0u8, 1] {
            let r = DimRef::WallEnd {
                wall: wid,
                face,
                end,
            };
            pts.push((
                r,
                cx.point(&r)?
                    .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                    .1,
            ));
        }
        let ends = (pts[0].0, pts[1].0);
        for &(oid, _) in ops.iter().filter(|o| o.1 == wid) {
            for edge in [0u8, 1] {
                let r = DimRef::OpeningEdge {
                    opening: oid,
                    edge,
                    face,
                };
                pts.push((
                    r,
                    cx.point(&r)?
                        .ok_or_else(|| Error::Invalid("dimension reference lost".into()))?
                        .1,
                ));
            }
        }
        let arc = |a: DimRef, b: DimRef, tier: i32| DimSpec {
            level,
            kind: DimKind::ArcLength,
            refs: vec![a, b],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 0.0,
            tier,
            auto: true,
        };
        let mut tier = 0;
        if pts.len() > 2 {
            pts.sort_by(|a, b| a.1.total_cmp(&b.1));
            pts.dedup_by(|b, a| (b.1 - a.1).abs() < 1e-6);
            for k in 1..pts.len() {
                out.push(arc(pts[k - 1].0, pts[k].0, 0));
            }
            tier = 1;
        }
        out.push(arc(ends.0, ends.1, tier));
        out.push(DimSpec {
            level,
            kind: DimKind::Radial,
            refs: vec![DimRef::ArcFace { wall: wid, face }],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 0.0,
            tier: -1,
            auto: true,
        });
    }
    Ok(out)
}

#[path = "annot_chain.rs"]
mod chain;
#[path = "annot_tags.rs"]
mod tags;
pub(crate) use tags::{cascade, issues as tag_issues};
pub use tags::{next_mark, tag_size, Tag, TagCat, TagHost, TagSpec, TagText};

#[cfg(test)]
#[path = "annot_tests.rs"]
mod tests;
