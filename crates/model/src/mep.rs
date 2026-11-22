// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! MEP networks (schema 16): systems `mep_system` (type, color) and runs `mep_run` (network kind,
//! section, material and DN, wall, insulation, lining, elbow radius, slope, axis points in
//! world coordinates, end caps). A system's network is computed on the fly: nodes are coinciding
//! ends, a run end at the middle of another (tee), and component connections; fittings follow
//! from the branch count and sections: elbow, tee, cross, transition, cap.

use std::collections::HashMap;

use ddc_geom::math::{v2, v3, V2, V3};
use ddc_geom::mep::clash::{self, Body, Convex};
use ddc_geom::mep::{
    self, Detail, Domain, FittingKind, Piece, PipeMaterial, PlanPrim, Plane, Qty, Section, Shape,
    SystemKind,
};
use ddc_geom::mesh::Mesh;
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Model};

/// End coincidence tolerance, m.
const TOL: f64 = 1e-3;
/// Component connection: tolerance to the connection point, m.
const PORT_TOL: f64 = 5e-3;

fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn p3(p: V3) -> (f64, f64, f64) {
    (p.x, p.y, p.z)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunSpec {
    pub system: i64,
    pub level: i64,
    pub section: Section,
    pub material: Option<PipeMaterial>,
    pub dn: Option<u32>,
    /// Elbow radius along the axis; None means by material/network-kind rule.
    pub bend: Option<f64>,
    /// Slope, % downward from the first point (drainage); applied to the points on write.
    pub slope: f64,
    /// Axis points, world coordinates (z is the absolute elevation).
    pub points: Vec<(f64, f64, f64)>,
    /// Caps at the start and end.
    pub caps: (bool, bool),
}

impl RunSpec {
    pub fn new(system: i64, level: i64, section: Section, points: Vec<(f64, f64, f64)>) -> Self {
        RunSpec {
            system,
            level,
            section,
            material: None,
            dn: None,
            bend: None,
            slope: 0.0,
            points,
            caps: (false, false),
        }
    }
    /// Pipe from the material catalog: outer diameter and wall by DN.
    pub fn pipe(
        system: i64,
        level: i64,
        material: PipeMaterial,
        dn: u32,
        points: Vec<(f64, f64, f64)>,
    ) -> Result<Self, Error> {
        let s = material
            .by_dn(dn)
            .ok_or_else(|| Error::Invalid(format!("no DN{dn} in {} catalog", material.name())))?;
        let mut sec = Section::new(Domain::Pipe, Shape::Round { d: s.od });
        sec.wall = s.wall;
        let mut r = RunSpec::new(system, level, sec, points);
        r.material = Some(material);
        r.dn = Some(dn);
        Ok(r)
    }
    fn pts(&self) -> Vec<V3> {
        let p: Vec<V3> = self.points.iter().map(|&(x, y, z)| v3(x, y, z)).collect();
        if self.slope != 0.0 {
            mep::apply_slope(&p, self.slope)
        } else {
            p
        }
    }
    /// Elbow radius on the axis: the run's own, its catalogue pipe's, else the section default.
    pub fn radius(&self, normal_along_up: bool) -> f64 {
        if let Some(r) = self.bend {
            return r;
        }
        if let (Some(m), Some(dn)) = (self.material, self.dn) {
            if let Some(s) = m.by_dn(dn) {
                return m.bend_radius(&s);
            }
        }
        mep::default_bend_radius(&self.section, normal_along_up)
    }
    /// Drops the corner of any leg too short for its elbows (see `mep::fit_bends`); a run
    /// whose elbows fit keeps its points exactly.
    pub fn fit_bends(&mut self) {
        let mut p: Vec<V3> = self.points.iter().map(|&(x, y, z)| v3(x, y, z)).collect();
        let n = p.len();
        mep::fit_bends(&mut p, &|u| self.radius(u), (0.0, 0.0));
        if p.len() != n {
            self.points = p.into_iter().map(p3).collect();
        }
    }
    /// Shortest segment that fits an elbow, m.
    pub fn min_leg(&self) -> f64 {
        2.5 * self.radius(false)
    }
    /// Size label: DN100, Ø200, 400x200, 400x200 oval.
    pub fn size_label(&self) -> String {
        size_label(&self.section, self.dn)
    }
}

fn size_label(s: &Section, dn: Option<u32>) -> String {
    let mm = |v: f64| format!("{}", (v * 1000.0 * 10.0).round() / 10.0);
    if let Some(dn) = dn {
        return format!("DN{dn}");
    }
    match s.shape {
        Shape::Round { d } => format!("Ø{}", mm(d)),
        Shape::Rect { w, h } => format!("{}x{}", mm(w), mm(h)),
        Shape::Oval { w, h } => format!("{}x{} oval", mm(w), mm(h)),
    }
}

fn shape_cols(s: Shape) -> (&'static str, f64, f64) {
    match s {
        Shape::Round { d } => ("round", d, d),
        Shape::Rect { w, h } => ("rect", w, h),
        Shape::Oval { w, h } => ("oval", w, h),
    }
}

fn shape_of(k: &str, w: f64, h: f64) -> Result<Shape, Error> {
    Ok(match k {
        "round" => Shape::Round { d: w },
        "rect" => Shape::Rect { w, h },
        "oval" => Shape::Oval { w, h },
        o => return invalid(format!("unknown section shape {o}")),
    })
}

/// Axis points as text: "x y z;x y z" (exact decimal representation).
pub fn points_text(p: &[(f64, f64, f64)]) -> String {
    p.iter()
        .map(|(x, y, z)| format!("{x:?} {y:?} {z:?}"))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn parse_points(s: &str) -> Option<Vec<(f64, f64, f64)>> {
    s.split(';')
        .map(|t| {
            let v: Vec<f64> = t
                .split_whitespace()
                .map(|x| x.parse().ok())
                .collect::<Option<_>>()?;
            match v[..] {
                [x, y, z] if v.iter().all(|a| a.is_finite()) => Some((x, y, z)),
                _ => None,
            }
        })
        .collect()
}

/// A network system.
#[derive(Clone, Debug, PartialEq)]
pub struct MepSystem {
    pub id: i64,
    pub name: String,
    pub kind: SystemKind,
    pub color: [u8; 3],
}

/// A network fitting with quantities.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitting {
    pub kind: FittingKind,
    pub at: (f64, f64, f64),
    pub runs: Vec<i64>,
    pub size: String,
    pub qty: Qty,
}

/// A network graph node.
#[derive(Clone, Debug, PartialEq)]
pub struct MepNode {
    pub at: (f64, f64, f64),
    /// Branches: (run, end 0/1 or tap point 2).
    pub arms: Vec<(i64, u8)>,
    /// Connected component and connection name.
    pub port: Option<(i64, String)>,
    pub fitting: Option<FittingKind>,
}

/// Network graph: nodes, edges (a run between nodes), fittings, open ends.
#[derive(Clone, Debug, Default)]
pub struct MepNetwork {
    pub nodes: Vec<MepNode>,
    /// (run, start node, end node).
    pub edges: Vec<(i64, usize, usize)>,
    pub fittings: Vec<Fitting>,
    /// Ends with no connection and no cap: (run, point).
    pub open_ends: Vec<(i64, (f64, f64, f64))>,
    /// Route pieces (straights and elbows) with section: for bodies, plan, intersections.
    pub pieces: Vec<(i64, Piece, Section)>,
    /// Fitting bodies: (index in `fittings`, mesh).
    pub fitting_meshes: Vec<(usize, Mesh)>,
    /// Straight parts of tees/crosses and caps, for intersections and plan.
    pub fitting_pieces: Vec<(i64, Piece, Section)>,
}

impl MepNetwork {
    /// Number of connected components of the graph.
    pub fn parts(&self) -> usize {
        self.parts_with(&[])
    }
    /// Connected parts when the node pairs `extra` are joined too (units inline on a run).
    pub fn parts_with(&self, extra: &[(usize, usize)]) -> usize {
        let mut up: Vec<usize> = (0..self.nodes.len()).collect();
        fn find(u: &mut [usize], x: usize) -> usize {
            let mut r = x;
            while u[r] != r {
                r = u[r];
            }
            u[x] = r;
            r
        }
        for (a, b) in self.edges.iter().map(|e| (e.1, e.2)).chain(extra.iter().copied()) {
            let (ra, rb) = (find(&mut up, a), find(&mut up, b));
            up[ra] = rb;
        }
        (0..self.nodes.len())
            .filter(|&i| find(&mut up, i) == i)
            .count()
    }
    pub fn count(&self, k: FittingKind) -> usize {
        self.fittings.iter().filter(|f| f.kind == k).count()
    }
}

/// Quantity row by size.
#[derive(Clone, Debug, PartialEq)]
pub struct SizeRow {
    pub system: i64,
    pub domain: Domain,
    pub size: String,
    /// Length of straight runs (excluding fittings), m.
    pub length: f64,
    pub surface: f64,
    pub volume: f64,
    pub insulation: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MepQty {
    pub sizes: Vec<SizeRow>,
    /// (fitting kind, size, count).
    pub fittings: Vec<(&'static str, String, usize)>,
    /// Everything together: straights and fittings.
    pub total: Qty,
}

/// Intersection of a network run with a structure.
#[derive(Clone, Debug, PartialEq)]
pub struct MepClash {
    pub run: i64,
    /// wall | slab | column | beam
    pub other: &'static str,
    pub other_id: i64,
    pub volume: f64,
}

/// Network plan primitive.
#[derive(Clone, Debug, PartialEq)]
pub struct MepPlanPrim {
    pub run: i64,
    pub color: [u8; 3],
    pub prim: PlanPrim,
}

/// Component connection in world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldPort {
    pub comp: i64,
    pub name: String,
    pub domain: Domain,
    pub system: Option<SystemKind>,
    pub at: (f64, f64, f64),
    pub dir: (f64, f64, f64),
    pub size: f64,
}

/// Shortest segment a run keeps; closer consecutive points are one point. Equal to the node
/// snapping `TOL`, so no segment is kept that the network would join into one node.
const MIN_SEGMENT: f64 = TOL;

/// Run with repeated consecutive points dropped; refused when a point is not finite or fewer
/// than two distinct points remain.
fn dedup_points(s: &RunSpec) -> Result<RunSpec, Error> {
    if s.points.iter().any(|p| !(p.0.is_finite() && p.1.is_finite() && p.2.is_finite())) {
        return invalid("run point is not a finite number");
    }
    let mut out = s.clone();
    out.points.clear();
    for p in &s.points {
        let same = out.points.last().is_some_and(|q| {
            ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2) + (p.2 - q.2).powi(2)).sqrt() < MIN_SEGMENT
        });
        if !same {
            out.points.push(*p);
        }
    }
    if out.points.len() < 2 {
        return invalid("run has no length: all its points coincide");
    }
    Ok(out)
}

fn check_spec(db: &Connection, s: &RunSpec) -> Result<(), Error> {
    let kind: Option<String> = db
        .query_row("SELECT kind FROM mep_system WHERE id=?1", [s.system], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(kind) = kind else {
        return invalid(format!("no such system {}", s.system));
    };
    let kind = SystemKind::parse(&kind).ok_or_else(|| Error::Invalid("bad system kind".into()))?;
    if !kind.domains().contains(&s.section.domain) {
        return invalid(format!(
            "{} system cannot carry {}",
            kind.name(),
            s.section.domain.name()
        ));
    }
    if db.query_row("SELECT count(*) FROM level WHERE id=?1", [s.level], |r| {
        r.get::<_, i64>(0)
    })? == 0
    {
        return invalid(format!("no level {}", s.level));
    }
    let sec = &s.section;
    if !sec.shape.valid()
        || ![sec.wall, sec.insulation, sec.lining, s.slope]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
        || !sec.inner().valid()
        || s.bend.is_some_and(|b| !(b.is_finite() && b > 0.0))
    {
        return invalid("bad section, thickness, bend or slope");
    }
    mep::route(&s.pts(), &|u| s.radius(u), (0.0, 0.0))
        .map_err(|e| Error::Geom(format!("run: {e}")))?;
    Ok(())
}

/// An edge of a network between its node fittings. A leg too short for the elbows and the
/// fittings at its ends gives its corner to the next leg; a straight still too short for the
/// fittings at both ends lets them share it, so a short piece between fittings never fails.
fn route_fitted(
    pts: &[V3],
    radius: &dyn Fn(bool) -> f64,
    trim: (f64, f64),
) -> Result<Vec<Piece>, mep::RouteError> {
    match mep::route(pts, radius, trim) {
        Err(mep::RouteError::TooShort { .. }) => {}
        r => return r,
    }
    let mut p = pts.to_vec();
    mep::fit_bends(&mut p, radius, trim);
    match mep::route(&p, radius, trim) {
        Err(mep::RouteError::TooShort { .. }) if p.len() == 2 => {}
        r => return r,
    }
    let len = (p[1] - p[0]).len();
    let k = len / (trim.0 + trim.1).max(1e-12) * (1.0 - 1e-9);
    mep::route(&p, radius, (trim.0 * k, trim.1 * k))
}

fn write_run(db: &Connection, s: &RunSpec, id: Option<i64>) -> Result<i64, Error> {
    let (k, w, h) = shape_cols(s.section.shape);
    let pts: Vec<(f64, f64, f64)> = s.pts().into_iter().map(p3).collect();
    db.execute(
        "INSERT OR REPLACE INTO mep_run(id,system,level,domain,shape,w,h,wall,insulation,lining,\
         material,dn,bend,slope,points,cap0,cap1) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            id,
            s.system,
            s.level,
            s.section.domain.name(),
            k,
            w,
            h,
            s.section.wall,
            s.section.insulation,
            s.section.lining,
            s.material.map(|m| m.name()),
            s.dn,
            s.bend,
            s.slope,
            points_text(&pts),
            s.caps.0 as i64,
            s.caps.1 as i64
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// Writes a run without journaling (for importers inside their own operation).
pub fn insert_mep_run(db: &Connection, s: &RunSpec) -> Result<i64, Error> {
    write_run(db, s, None)
}

/// Rewrites run `id` in place without journaling (re-import; the row is not deleted).
pub fn replace_mep_run(db: &Connection, id: i64, s: &RunSpec) -> Result<(), Error> {
    write_run(db, s, Some(id)).map(|_| ())
}

pub(crate) fn read_run(db: &Connection, id: i64) -> Result<RunSpec, Error> {
    let row = db
        .query_row(
            "SELECT system,level,domain,shape,w,h,wall,insulation,lining,material,dn,bend,slope,\
             points,cap0,cap1 FROM mep_run WHERE id=?1",
            [id],
            |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?),
                    (r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                    [
                        r.get::<_, f64>(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ],
                    (
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, Option<u32>>(10)?,
                        r.get::<_, Option<f64>>(11)?,
                        r.get::<_, f64>(12)?,
                    ),
                    r.get::<_, String>(13)?,
                    (r.get::<_, i64>(14)?, r.get::<_, i64>(15)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no such run {id}")))?;
    let ((system, level), (dom, shape), [w, h, wall, ins, lin], (mat, dn, bend, slope), pts, caps) =
        row;
    let domain = Domain::parse(&dom).ok_or_else(|| Error::Invalid(format!("bad domain {dom}")))?;
    let mut section = Section::new(domain, shape_of(&shape, w, h)?);
    section.wall = wall;
    section.insulation = ins;
    section.lining = lin;
    Ok(RunSpec {
        system,
        level,
        section,
        material: mat.as_deref().and_then(PipeMaterial::parse),
        dn,
        bend,
        slope,
        points: parse_points(&pts)
            .ok_or_else(|| Error::Invalid(format!("run {id}: bad points")))?,
        caps: (caps.0 != 0, caps.1 != 0),
    })
}

/// Run ends by grid cell: tap candidates without scanning all pairs.
struct EndGrid {
    cell: f64,
    map: HashMap<(i64, i64, i64), Vec<(usize, u8)>>,
}

impl EndGrid {
    fn new(pts: &[Vec<V3>]) -> Self {
        let cell = 1.0;
        let mut map: HashMap<(i64, i64, i64), Vec<(usize, u8)>> = HashMap::new();
        for (j, p) in pts.iter().enumerate() {
            for (k, e) in [(0u8, p[0]), (1, *p.last().expect("points"))] {
                map.entry(Self::key(cell, e)).or_default().push((j, k));
            }
        }
        EndGrid { cell, map }
    }
    fn key(cell: f64, p: V3) -> (i64, i64, i64) {
        (
            (p.x / cell).floor() as i64,
            (p.y / cell).floor() as i64,
            (p.z / cell).floor() as i64,
        )
    }
    /// Ends in the cells of segment boxes (with tolerance), ascending by (run, end).
    fn near(&self, pts: &[V3]) -> Vec<(usize, u8)> {
        let mut out = vec![];
        for w in pts.windows(2) {
            let lo = Self::key(
                self.cell,
                v3(w[0].x.min(w[1].x), w[0].y.min(w[1].y), w[0].z.min(w[1].z)) - v3(TOL, TOL, TOL),
            );
            let hi = Self::key(
                self.cell,
                v3(w[0].x.max(w[1].x), w[0].y.max(w[1].y), w[0].z.max(w[1].z)) + v3(TOL, TOL, TOL),
            );
            let n = (hi.0 - lo.0 + 1) * (hi.1 - lo.1 + 1) * (hi.2 - lo.2 + 1);
            if n > self.map.len() as i64 {
                out.extend(self.map.values().flatten().copied());
                continue;
            }
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        out.extend(self.map.get(&(x, y, z)).into_iter().flatten().copied());
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Nearest point on a polyline: (point, segment index, distance).
fn nearest_on(pts: &[V3], q: V3) -> (V3, usize, f64) {
    let mut best = (pts[0], 0, f64::INFINITY);
    for k in 0..pts.len() - 1 {
        let (a, b) = (pts[k], pts[k + 1]);
        let d = b - a;
        let t = ((q - a).dot(d) / d.dot(d)).clamp(0.0, 1.0);
        let p = a + d * t;
        let dist = (q - p).len();
        if dist < best.2 {
            best = (p, k, dist);
        }
    }
    best
}

struct Edge {
    run: i64,
    pts: Vec<V3>,
    spec: usize,
    /// Setbacks for fittings at the ends.
    trim: [f64; 2],
    nodes: [usize; 2],
    /// A true run end (0/1) or a tap (2).
    end_tag: [u8; 2],
}

impl Model {
    pub fn add_mep_system(&mut self, name: &str, kind: SystemKind) -> Result<i64, Error> {
        if name.trim().is_empty() {
            return invalid("system name is empty");
        }
        let c = crate::material_look::service_rgb(name, kind);
        let rgb = (c[0] as i64) << 16 | (c[1] as i64) << 8 | c[2] as i64;
        self.op("mep_system_add", |db| {
            db.execute(
                "INSERT INTO mep_system(name,kind,color) VALUES (?1,?2,?3)",
                params![name, kind.name(), rgb],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("mep_system", "id", id)))
        })
    }

    pub fn mep_systems(&self) -> Result<Vec<MepSystem>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id,name,kind,color FROM mep_system ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = vec![];
        for x in rows {
            let (id, name, k, c) = x?;
            out.push(MepSystem {
                id,
                name,
                kind: SystemKind::parse(&k)
                    .ok_or_else(|| Error::Invalid(format!("bad system kind {k}")))?,
                color: [(c >> 16) as u8, (c >> 8) as u8, c as u8],
            });
        }
        Ok(out)
    }

    /// Deletes a system together with its runs.
    pub fn delete_mep_system(&mut self, id: i64) -> Result<(), Error> {
        self.op("mep_system_delete", |db| {
            let inv = Inv::default()
                .restore(db, "mep_system", "id", id)?
                .restore(db, "mep_run", "system", id)?;
            db.execute("DELETE FROM mep_run WHERE system=?1", [id])?;
            if db.execute("DELETE FROM mep_system WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such system {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn add_mep_run(&mut self, s: &RunSpec) -> Result<i64, Error> {
        let s = &dedup_points(s)?;
        check_spec(&self.db, s)?;
        self.op("mep_run_add", |db| {
            let id = write_run(db, s, None)?;
            Ok((id, id, Inv::default().delete("mep_run", "id", id)))
        })
    }

    /// Vertical riser at (x, y) from the elevation of level `from` + `dz0` to level `to` + `dz1`.
    pub fn add_mep_riser(
        &mut self,
        mut s: RunSpec,
        x: f64,
        y: f64,
        (from, dz0): (i64, f64),
        (to, dz1): (i64, f64),
    ) -> Result<i64, Error> {
        let el = |l: i64| -> Result<f64, Error> {
            self.db
                .query_row("SELECT elevation FROM level WHERE id=?1", [l], |r| r.get(0))
                .optional()?
                .ok_or_else(|| Error::Invalid(format!("no level {l}")))
        };
        let (z0, z1) = (el(from)? + dz0, el(to)? + dz1);
        s.level = from;
        s.slope = 0.0;
        s.points = vec![(x, y, z0), (x, y, z1)];
        self.add_mep_run(&s)
    }

    pub fn update_mep_run(&mut self, id: i64, s: &RunSpec) -> Result<(), Error> {
        let s = &dedup_points(s)?;
        check_spec(&self.db, s)?;
        self.op("mep_run_edit", |db| {
            let inv = Inv::default().restore(db, "mep_run", "id", id)?;
            // REPLACE in place, no DELETE: the delete trigger would drop the run's clash statuses
            let n: i64 = db.query_row("SELECT count(*) FROM mep_run WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
            if n == 0 {
                return invalid(format!("no such run {id}"));
            }
            write_run(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_mep_run(&mut self, id: i64) -> Result<(), Error> {
        self.op("mep_run_delete", |db| {
            let inv = Inv::default().restore(db, "mep_run", "id", id)?;
            if db.execute("DELETE FROM mep_run WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such run {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn mep_run(&self, id: i64) -> Result<RunSpec, Error> {
        read_run(&self.db, id)
    }

    /// Runs of a system (all when None) by id.
    pub fn mep_runs(&self, system: Option<i64>) -> Result<Vec<i64>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id FROM mep_run WHERE ?1 IS NULL OR system=?1 ORDER BY id")?;
        let r = st.query_map([system], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Component connections (all when None) in world coordinates.
    pub fn component_ports(&self, comp: Option<i64>) -> Result<Vec<WorldPort>, Error> {
        let ids: Vec<i64> = {
            let mut st = self.db.prepare(
                "SELECT c.id FROM comp c JOIN comp_type t ON t.id=c.ctype \
                 JOIN comp_family f ON f.id=t.family \
                 WHERE (?1 IS NULL OR c.id=?1) AND instr(f.def, 'connector') > 0 ORDER BY c.id",
            )?;
            let r = st.query_map([comp], |r| r.get(0))?;
            r.collect::<Result<_, _>>()?
        };
        let mut out = vec![];
        for id in ids {
            let (fam, res, _) = self.component_built(id, ddc_geom::component::Lod::Coarse)?;
            let pl = self.component_quantities(id)?.placement;
            let ports = fam
                .ports(&res)
                .map_err(|e| Error::Invalid(format!("component {id}: {e}")))?;
            for p in ports {
                let at = pl.apply(p3(p.at));
                let o = pl.apply((0.0, 0.0, 0.0));
                let d = pl.apply(p3(p.dir));
                out.push(WorldPort {
                    comp: id,
                    name: p.name,
                    domain: p.domain,
                    system: p.system,
                    at,
                    dir: (d.0 - o.0, d.1 - o.1, d.2 - o.2),
                    size: p.size,
                });
            }
        }
        Ok(out)
    }

    /// Network graph of a system (all systems when None).
    pub fn mep_network(&self, system: Option<i64>) -> Result<MepNetwork, Error> {
        self.mep_network_with(system, &self.component_ports(None)?)
    }

    /// Network graph with already-read connections (checking all systems reads them once).
    fn mep_network_with(
        &self,
        system: Option<i64>,
        ports: &[WorldPort],
    ) -> Result<MepNetwork, Error> {
        let ids = self.mep_runs(system)?;
        let specs: Vec<RunSpec> = ids
            .iter()
            .map(|&i| read_run(&self.db, i))
            .collect::<Result<_, _>>()?;
        // consecutive coinciding points give zero-length segments: the direction is NaN in all fittings
        let pts: Vec<Vec<V3>> = specs.iter().map(|s| dedup_pts(s.pts())).collect();
        // taps: an end of one run at the middle of another in the same system; candidates are
        // ends from grid cells along the run (order j, end as in a full scan)
        let mut splits: Vec<Vec<(usize, V3)>> = vec![vec![]; ids.len()];
        let ends = EndGrid::new(&pts);
        for (i, pi) in pts.iter().enumerate() {
            for (j, end) in ends.near(pi) {
                if i == j || specs[i].system != specs[j].system {
                    continue;
                }
                let pj = &pts[j];
                {
                    let e = if end == 0 {
                        pj[0]
                    } else {
                        *pj.last().expect("points")
                    };
                    let (q, k, d) = nearest_on(pi, e);
                    let near_end = (q - pi[0]).len() < TOL || (q - *pi.last().unwrap()).len() < TOL;
                    if d < TOL && !near_end && !splits[i].iter().any(|s| (s.1 - q).len() < TOL) {
                        splits[i].push((k, q));
                    }
                }
            }
        }
        let mut edges: Vec<Edge> = vec![];
        for (i, p) in pts.iter().enumerate() {
            let mut sp = splits[i].clone();
            sp.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(((a.1 - p[a.0]).len()).total_cmp(&(b.1 - p[b.0]).len()))
            });
            let mut cur = vec![p[0]];
            let mut tag0 = 0u8;
            let mut k = 0;
            for seg in 0..p.len() - 1 {
                while k < sp.len() && sp[k].0 == seg {
                    cur.push(sp[k].1);
                    edges.push(Edge {
                        run: ids[i],
                        pts: std::mem::take(&mut cur),
                        spec: i,
                        trim: [0.0; 2],
                        nodes: [0; 2],
                        end_tag: [tag0, 2],
                    });
                    cur = vec![sp[k].1];
                    tag0 = 2;
                    k += 1;
                }
                cur.push(p[seg + 1]);
            }
            edges.push(Edge {
                run: ids[i],
                pts: cur,
                spec: i,
                trim: [0.0; 2],
                nodes: [0; 2],
                end_tag: [tag0, 1],
            });
        }
        // nodes by tolerance grid
        let mut net = MepNetwork::default();
        let mut grid: HashMap<(i64, i64, i64), Vec<usize>> = HashMap::new();
        let key = |p: V3| {
            (
                (p.x / TOL).floor() as i64,
                (p.y / TOL).floor() as i64,
                (p.z / TOL).floor() as i64,
            )
        };
        let mut node_pts: Vec<V3> = vec![];
        let mut arms: Vec<Vec<(usize, usize)>> = vec![];
        #[allow(clippy::needless_range_loop)]
        for ei in 0..edges.len() {
            for end in 0..2 {
                let p = if end == 0 {
                    edges[ei].pts[0]
                } else {
                    *edges[ei].pts.last().unwrap()
                };
                let (kx, ky, kz) = key(p);
                let mut found = None;
                'f: for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for &n in grid.get(&(kx + dx, ky + dy, kz + dz)).into_iter().flatten() {
                                if (node_pts[n] - p).len() < TOL {
                                    found = Some(n);
                                    break 'f;
                                }
                            }
                        }
                    }
                }
                let n = found.unwrap_or_else(|| {
                    node_pts.push(p);
                    arms.push(vec![]);
                    grid.entry((kx, ky, kz))
                        .or_default()
                        .push(node_pts.len() - 1);
                    node_pts.len() - 1
                });
                edges[ei].nodes[end] = n;
                arms[n].push((ei, end));
            }
        }
        for (n, &at) in node_pts.iter().enumerate() {
            let dom = specs[edges[arms[n][0].0].spec].section.domain;
            let port = ports.iter().find(|p| {
                let d = v3(p.at.0, p.at.1, p.at.2) - at;
                d.len() < PORT_TOL
                    && (p.domain == dom
                        || matches!(
                            (p.domain, dom),
                            (
                                Domain::Conduit | Domain::CableTray,
                                Domain::Conduit | Domain::CableTray
                            )
                        ))
            });
            net.nodes.push(MepNode {
                at: p3(at),
                arms: arms[n]
                    .iter()
                    .map(|&(e, end)| (edges[e].run, edges[e].end_tag[end]))
                    .collect(),
                port: port.map(|p| (p.comp, p.name.clone())),
                fitting: None,
            });
        }
        // direction in which a branch leaves the node
        let depart = |e: &Edge, end: usize| -> V3 {
            let p = &e.pts;
            if end == 0 {
                (p[1] - p[0]).normalized()
            } else {
                (p[p.len() - 2] - p[p.len() - 1]).normalized()
            }
        };
        for n in 0..node_pts.len() {
            let at = node_pts[n];
            let a = &arms[n];
            let secs: Vec<Section> = a.iter().map(|x| specs[edges[x.0].spec].section).collect();
            let labels: Vec<String> = a
                .iter()
                .map(|x| specs[edges[x.0].spec].size_label())
                .collect();
            let sec = |k: usize| secs[k];
            let label = |k: usize| labels[k].clone();
            let runs: Vec<i64> = {
                let mut r: Vec<i64> = a.iter().map(|x| edges[x.0].run).collect();
                r.dedup();
                r
            };
            let fit =
                |kind: FittingKind, size: String, qty: Qty, mesh: Mesh, net: &mut MepNetwork| {
                    net.nodes[n].fitting = Some(kind.clone());
                    net.fittings.push(Fitting {
                        kind,
                        at: p3(at),
                        runs: runs.clone(),
                        size,
                        qty,
                    });
                    net.fitting_meshes.push((net.fittings.len() - 1, mesh));
                };
            match a.len() {
                1 => {
                    let (e, end) = a[0];
                    let s = &specs[edges[e].spec];
                    let tag = edges[e].end_tag[end];
                    let capped = (tag == 0 && s.caps.0) || (tag == 1 && s.caps.1);
                    if net.nodes[n].port.is_some() {
                    } else if capped {
                        let l = mep::cap_length(&s.section);
                        edges[e].trim[end] += l;
                        let d = depart(&edges[e], end);
                        let f = mep::start_frame(d);
                        let piece = Piece::Straight {
                            a: at,
                            b: at + d * l,
                            frame: f,
                        };
                        let mut q = mep::qty_along(&s.section, l);
                        q.volume = 0.0;
                        net.fitting_pieces.push((edges[e].run, piece, s.section));
                        let mesh = mep::sweep(s.section.envelope(), &piece.samples(8), 24);
                        fit(FittingKind::Cap, label(0), q, mesh, &mut net);
                    } else {
                        net.open_ends.push((edges[e].run, p3(at)));
                    }
                }
                2 => {
                    let (d0, d1) = (
                        depart(&edges[a[0].0], a[0].1),
                        depart(&edges[a[1].0], a[1].1),
                    );
                    let (s0, s1) = (sec(0), sec(1));
                    let turn = PI_M - d0.dot(d1).clamp(-1.0, 1.0).acos();
                    if s0 != s1 && turn > CORNER_MIN_TURN && turn < PI_M - CORNER_MIN_TURN {
                        // elbow of the smaller section at the corner, transition on the larger branch: there is
                        // usually room, and the transition rings are perpendicular to its axis
                        let (ks, kb) = if s0.envelope().area() <= s1.envelope().area() {
                            (0, 1)
                        } else {
                            (1, 0)
                        };
                        let (ds, db) = if ks == 0 { (d0, d1) } else { (d1, d0) };
                        let (ss, sb) = (sec(ks), sec(kb));
                        let inc = -ds;
                        let axis = inc.cross(db).normalized();
                        let fr = mep::start_frame(inc);
                        let r = specs[edges[a[ks].0].spec].radius(axis.cross(fr.up).len() < 1e-6);
                        let t = r * (turn / 2.0).tan();
                        let l = mep::transition_length(&ss, &sb);
                        edges[a[ks].0].trim[a[ks].1] += t;
                        edges[a[kb].0].trim[a[kb].1] += t + l;
                        let start = at + ds * t;
                        let piece = Piece::Elbow {
                            center: start + axis.cross(inc) * r,
                            start,
                            end: at + db * t,
                            corner: at,
                            axis,
                            radius: r,
                            angle: turn,
                            frame: fr,
                        };
                        net.pieces.push((edges[a[ks].0].run, piece, ss));
                        fit(
                            FittingKind::Elbow,
                            label(ks),
                            mep::qty_along(&ss, piece.length()),
                            mep::sweep(ss.envelope(), &piece.samples(8), 24),
                            &mut net,
                        );
                        let mesh = mep::loft(
                            ss.envelope(),
                            sb.envelope(),
                            at + db * t,
                            at + db * (t + l),
                            mep::start_frame(db),
                            32,
                        );
                        fit(
                            FittingKind::Transition,
                            format!("{}-{}", label(ks), label(kb)),
                            mep::transition_qty(&ss, &sb, l),
                            mesh,
                            &mut net,
                        );
                        if let Some(f) = net.fittings.last_mut() {
                            f.at = p3(at + db * (t + l / 2.0));
                        }
                    } else if s0 != s1 {
                        let l = mep::transition_length(&s0, &s1);
                        edges[a[0].0].trim[a[0].1] += l / 2.0;
                        edges[a[1].0].trim[a[1].1] += l / 2.0;
                        let f = mep::start_frame(d1);
                        let mesh = mep::loft(
                            s0.envelope(),
                            s1.envelope(),
                            at + d0 * (l / 2.0),
                            at + d1 * (l / 2.0),
                            f,
                            32,
                        );
                        fit(
                            FittingKind::Transition,
                            format!("{}-{}", label(0), label(1)),
                            mep::transition_qty(&s0, &s1, l),
                            mesh,
                            &mut net,
                        );
                    } else if turn > 1e-6 {
                        let s = &specs[edges[a[0].0].spec];
                        let inc = -d0;
                        let axis = inc.cross(d1).normalized();
                        let fr = mep::start_frame(inc);
                        let r = s.radius(axis.cross(fr.up).len() < 1e-6);
                        let t = r * (turn / 2.0).tan();
                        edges[a[0].0].trim[a[0].1] += t;
                        edges[a[1].0].trim[a[1].1] += t;
                        let start = at + d0 * t;
                        let piece = Piece::Elbow {
                            center: start + axis.cross(inc) * r,
                            start,
                            end: at + d1 * t,
                            corner: at,
                            axis,
                            radius: r,
                            angle: turn,
                            frame: fr,
                        };
                        net.pieces.push((edges[a[0].0].run, piece, s0));
                        fit(
                            FittingKind::Elbow,
                            label(0),
                            mep::qty_along(&s0, piece.length()),
                            mep::sweep(s0.envelope(), &piece.samples(8), 24),
                            &mut net,
                        );
                    }
                }
                k @ 3.. => {
                    let ds: Vec<V3> = a.iter().map(|&(e, end)| depart(&edges[e], end)).collect();
                    // the main line is the most opposed pair
                    let mut best = (0, 1, 2.0);
                    for i in 0..k {
                        for j in i + 1..k {
                            let c = ds[i].dot(ds[j]);
                            if c < best.2 {
                                best = (i, j, c);
                            }
                        }
                    }
                    let main = sec(best.0);
                    let leg = mep::junction_leg(&main);
                    let half = main.envelope().wh().0.max(main.envelope().wh().1) / 2.0;
                    let mut q = Qty::default();
                    let mut ms = vec![];
                    for (i, &(e, end)) in a.iter().enumerate() {
                        edges[e].trim[end] += leg;
                        let s = sec(i);
                        let piece = Piece::Straight {
                            a: at,
                            b: at + ds[i] * leg,
                            frame: mep::start_frame(ds[i]),
                        };
                        let along = if i == best.0 || i == best.1 {
                            leg
                        } else {
                            leg - half
                        };
                        q += mep::qty_along(&s, along.max(0.0));
                        net.fitting_pieces.push((edges[e].run, piece, s));
                        ms.push(mep::sweep(s.envelope(), &piece.samples(8), 24));
                    }
                    // more legs than four (a riser through a crossing) still meet in one cross
                    let kind = if k == 3 {
                        FittingKind::Tee
                    } else {
                        FittingKind::Cross
                    };
                    fit(kind, label(best.0), q, mep::merge(&ms), &mut net);
                }
                k => {
                    return Err(Error::Geom(format!(
                        "node at {:?}: {k} branches",
                        p3(at)
                    )))
                }
            }
        }
        for e in &edges {
            let s = &specs[e.spec];
            let pieces = route_fitted(&e.pts, &|u| s.radius(u), (e.trim[0], e.trim[1]))
                .map_err(|x| {
                    let len: f64 = e.pts.windows(2).map(|w| (w[1] - w[0]).len()).sum();
                    let pts: Vec<String> = e
                        .pts
                        .iter()
                        .map(|p| format!("({:.4}, {:.4}, {:.4})", p.x, p.y, p.z))
                        .collect();
                    Error::Geom(format!(
                        "run {}: {x} (axis {len:.3} m, end trims {:.3} and {:.3} m, radius {:.3} m, \
                         points {})",
                        e.run,
                        e.trim[0],
                        e.trim[1],
                        s.radius(false),
                        pts.join(" ")
                    ))
                })?;
            for p in pieces {
                if let Piece::Elbow { .. } = p {
                    net.fittings.push(Fitting {
                        kind: FittingKind::Elbow,
                        at: p3(match p {
                            Piece::Elbow { corner, .. } => corner,
                            _ => unreachable!(),
                        }),
                        runs: vec![e.run],
                        size: s.size_label(),
                        qty: mep::qty_along(&s.section, p.length()),
                    });
                    let m = mep::sweep(s.section.envelope(), &p.samples(8), 24);
                    net.fitting_meshes.push((net.fittings.len() - 1, m));
                }
                net.pieces.push((e.run, p, s.section));
            }
            net.edges.push((e.run, e.nodes[0], e.nodes[1]));
        }
        Ok(net)
    }

    /// Quantities: straight length by size, fittings by kind, total.
    pub fn mep_quantities(&self, system: Option<i64>) -> Result<MepQty, Error> {
        let net = self.mep_network(system)?;
        let mut sizes: Vec<SizeRow> = vec![];
        let mut total = Qty::default();
        let mut sys_of: HashMap<i64, (i64, String)> = HashMap::new();
        for id in self.mep_runs(system)? {
            let s = read_run(&self.db, id)?;
            sys_of.insert(id, (s.system, s.size_label()));
        }
        for (run, p, sec) in &net.pieces {
            if let Piece::Straight { .. } = p {
                let q = mep::qty_along(sec, p.length());
                total += q;
                let (sys, size) = sys_of[run].clone();
                match sizes
                    .iter_mut()
                    .find(|r| r.system == sys && r.size == size && r.domain == sec.domain)
                {
                    Some(r) => {
                        r.length += q.length;
                        r.surface += q.surface;
                        r.volume += q.volume;
                        r.insulation += q.insulation;
                    }
                    None => sizes.push(SizeRow {
                        system: sys,
                        domain: sec.domain,
                        size,
                        length: q.length,
                        surface: q.surface,
                        volume: q.volume,
                        insulation: q.insulation,
                    }),
                }
            }
        }
        let mut fittings: Vec<(&'static str, String, usize)> = vec![];
        for f in &net.fittings {
            total += f.qty;
            match fittings
                .iter_mut()
                .find(|x| x.0 == f.kind.name() && x.1 == f.size)
            {
                Some(x) => x.2 += 1,
                None => fittings.push((f.kind.name(), f.size.clone(), 1)),
            }
        }
        sizes.sort_by(|a, b| (a.system, &a.size).cmp(&(b.system, &b.size)));
        fittings.sort();
        Ok(MepQty {
            sizes,
            fittings,
            total,
        })
    }

    /// Network bodies: route pieces and node fittings with the system color.
    pub fn mep_meshes(&self, system: Option<i64>) -> Result<Vec<(i64, [u8; 3], Mesh)>, Error> {
        let net = self.mep_network(system)?;
        let colors = self.run_colors()?;
        let mut out = vec![];
        for (run, p, s) in &net.pieces {
            if let Piece::Straight { .. } = p {
                out.push((
                    *run,
                    colors[run],
                    mep::sweep(s.envelope(), &p.samples(8), 24),
                ));
            }
        }
        for (i, m) in net.fitting_meshes {
            let run = net.fittings[i].runs[0];
            out.push((run, colors[&run], m));
        }
        Ok(out)
    }

    fn run_colors(&self) -> Result<HashMap<i64, [u8; 3]>, Error> {
        let sys: HashMap<i64, [u8; 3]> = self
            .mep_systems()?
            .into_iter()
            .map(|s| (s.id, s.color))
            .collect();
        let mut st = self.db.prepare("SELECT id, system FROM mep_run")?;
        let r = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = HashMap::new();
        for x in r {
            let (id, s) = x?;
            out.insert(id, sys.get(&s).copied().unwrap_or([0, 0, 0]));
        }
        Ok(out)
    }

    /// Network intersections with structures (walls, slabs, columns, beams), volume > `min_volume`.
    /// (run, structure) pairs with the total volume; ordered by run, kind and id.
    pub fn mep_clashes(
        &self,
        system: Option<i64>,
        min_volume: f64,
    ) -> Result<Vec<MepClash>, Error> {
        let net = self.mep_network(system)?;
        let mut a_runs = vec![];
        let mut a = vec![];
        for (run, p, s) in net.pieces.iter().chain(&net.fitting_pieces) {
            let ring = s.envelope().outline_equal_area(24);
            let samples = p.samples(4);
            let mut pieces = vec![];
            for w in samples.windows(2) {
                let ((p0, f), (p1, _)) = (w[0], w[1]);
                if (p1 - p0).len() > 1e-9 {
                    pieces.push((Convex::segment(&ring, p0, p1, f.side, f.up), 1.0));
                }
            }
            a_runs.push(*run);
            a.push(Body::new(pieces));
        }
        let (b_ids, b) = self.structure_bodies()?;
        let mut acc: Vec<MepClash> = vec![];
        for h in clash::clashes(&a, &b, 0.0) {
            let (run, (kind, id)) = (a_runs[h.a], b_ids[h.b]);
            match acc
                .iter_mut()
                .find(|c| c.run == run && c.other == kind && c.other_id == id)
            {
                Some(c) => c.volume += h.volume,
                None => acc.push(MepClash {
                    run,
                    other: kind,
                    other_id: id,
                    volume: h.volume,
                }),
            }
        }
        acc.retain(|c| c.volume > min_volume);
        acc.sort_by(|x, y| (x.run, x.other, x.other_id).cmp(&(y.run, y.other, y.other_id)));
        Ok(acc)
    }

    /// Structure bodies for intersections: walls (arc walls by chords), slabs (with
    /// openings, no slope), columns and beams (body extents).
    pub(crate) fn structure_bodies(&self) -> Result<Bodies, Error> {
        let (mut ids, mut out) = (vec![], vec![]);
        {
            let mut st = self.db.prepare(
                "SELECT w.id,w.ax,w.ay,w.bx,w.by,w.base_offset+l.elevation,w.height,w.offset0,w.bulge,\
                 (SELECT SUM(thickness) FROM wall_type_layer WHERE wtype=w.wtype) \
                 FROM wall w JOIN level l ON l.id=w.level ORDER BY w.id",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                    (r.get::<_, f64>(5)?, r.get::<_, f64>(6)?),
                    r.get::<_, Option<f64>>(7)?,
                    r.get::<_, f64>(8)?,
                    r.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
                ))
            })?;
            let mut opens: HashMap<i64, Vec<[f64; 4]>> = HashMap::new();
            {
                let mut so = self
                    .db
                    .prepare("SELECT wall, along, width, sill, height FROM opening")?;
                let r = so.query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                    ))
                })?;
                for x in r {
                    let (w, o) = x?;
                    opens.entry(w).or_default().push(o);
                }
            }
            for x in rows {
                let (id, [ax, ay, bx, by], (z0, h), off0, bulge, t) = x?;
                let off = off0.map(|o| o - t / 2.0).unwrap_or(0.0);
                let (a, b) = (v2(ax, ay), v2(bx, by));
                let pts = arc_points(a, b, bulge);
                // an opening's `along` is the arc length; arc_points chords are equal, so linear along the chord
                let seg = arc_len(a, b, bulge) / (pts.len() - 1) as f64;
                let mut pieces = vec![];
                for (k, w) in pts.windows(2).enumerate() {
                    // a zero-length wall has no volume, so it is absent from clashes
                    let fp = clash::wall_footprint(w[0], w[1], t, off);
                    if fp.len() < 3 || !(seg > 1e-9) {
                        continue;
                    }
                    pieces.push((Convex::prism(&fp, z0, z0 + h), 1.0));
                    // openings (doors, windows, sleeves) are subtracted: same outline, part of the chord
                    let s0 = seg * k as f64;
                    for &[along, width, sill, oh] in opens.get(&id).into_iter().flatten() {
                        let (u0, u1) = (along.max(s0), (along + width).min(s0 + seg));
                        let (zl, zh) = (z0 + sill.max(0.0), (z0 + sill + oh).min(z0 + h));
                        if u1 - u0 > 1e-9 && zh - zl > 1e-9 {
                            let at = |u: f64| w[0] + (w[1] - w[0]) * ((u - s0) / seg);
                            let ofp = clash::wall_footprint(at(u0), at(u1), t, off);
                            if ofp.len() >= 3 {
                                pieces.push((Convex::prism(&ofp, zl, zh), -1.0));
                            }
                        }
                    }
                }
                ids.push(("wall", id));
                out.push(Body::new(pieces));
            }
        }
        let slabs: Vec<(i64, f64, f64)> = {
            let mut st = self.db.prepare(
                "SELECT s.id, l.elevation + s.offset, \
                 (SELECT SUM(thickness) FROM slab_type_layer WHERE stype=s.stype) \
                 FROM slab s JOIN level l ON l.id=s.level ORDER BY s.id",
            )?;
            let r = st.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                ))
            })?;
            r.collect::<Result<_, _>>()?
        };
        for (id, top, t) in slabs {
            let mut rings: Vec<Vec<V2>> = vec![];
            let mut st = self
                .db
                .prepare("SELECT ring,x,y FROM slab_vertex WHERE slab=?1 ORDER BY ring, idx")?;
            let r = st.query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?;
            for x in r {
                let (ring, x, y) = x?;
                while rings.len() <= ring as usize {
                    rings.push(vec![]);
                }
                rings[ring as usize].push(v2(x, y));
            }
            if let Some((outer, holes)) = rings.split_first() {
                ids.push(("slab", id));
                out.push(Body::prism_with_holes(outer, holes, top - t, top));
            }
        }
        // one build of all members: per-id lookups would rebuild every node each time
        for (kind, id, _, m) in self.member_meshes()? {
            let b = clash::Aabb::of(m.positions.iter().copied());
            let rect = [
                v2(b.min.x, b.min.y),
                v2(b.max.x, b.min.y),
                v2(b.max.x, b.max.y),
                v2(b.min.x, b.max.y),
            ];
            ids.push((kind, id));
            out.push(Body::solid(Convex::prism(&rect, b.min.z, b.max.z)));
        }
        Ok((ids, out))
    }

    /// Network plan of a level: pieces passing between this level's elevation and the next level's.
    pub fn mep_plan(&self, level: i64, detail: Detail) -> Result<Vec<MepPlanPrim>, Error> {
        let levels = crate::walls::levels(&self.db)?;
        let el = levels
            .iter()
            .find(|l| l.0 == level)
            .map(|l| l.1)
            .ok_or_else(|| Error::Invalid(format!("no level {level}")))?;
        let top = levels
            .iter()
            .map(|l| l.1)
            .filter(|&e| e > el + 1e-9)
            .fold(f64::INFINITY, f64::min);
        let net = self.mep_network(None)?;
        let colors = self.run_colors()?;
        let mut out = vec![];
        for (run, p, s) in net.pieces.iter().chain(&net.fitting_pieces) {
            let (a, b) = p.ends();
            let (z0, z1) = (a.z.min(b.z), a.z.max(b.z));
            if z1 < el - 1e-9 || z0 >= top {
                continue;
            }
            for prim in mep::plan(s.envelope(), p, detail, 24) {
                out.push(MepPlanPrim {
                    run: *run,
                    color: colors[run],
                    prim,
                });
            }
        }
        Ok(out)
    }

    /// Network section by plane `n·x = c`: outlines of straight runs.
    pub fn mep_section(&self, n: (f64, f64, f64), c: f64) -> Result<Vec<(i64, Contour)>, Error> {
        let plane = Plane {
            n: v3(n.0, n.1, n.2).normalized(),
            c: c / v3(n.0, n.1, n.2).len(),
        };
        let net = self.mep_network(None)?;
        let mut out = vec![];
        for (run, p, s) in &net.pieces {
            if let Some(r) = mep::section_cut(s.envelope(), p, plane, 24) {
                out.push((*run, r.into_iter().map(p3).collect()));
            }
        }
        Ok(out)
    }

    /// Connects a component to the nearest run of the system: a stub from the connection point
    /// along its direction, then vertical and horizontal legs to the run (a tap is a tee).
    pub fn mep_connect(&mut self, comp: i64, connector: &str, system: i64) -> Result<i64, Error> {
        let port = self
            .component_ports(Some(comp))?
            .into_iter()
            .find(|p| p.name == connector)
            .ok_or_else(|| {
                Error::Invalid(format!("component {comp} has no connector {connector}"))
            })?;
        let mut best: Option<(V3, f64, RunSpec)> = None;
        let p = v3(port.at.0, port.at.1, port.at.2);
        let d = v3(port.dir.0, port.dir.1, port.dir.2).normalized();
        for id in self.mep_runs(Some(system))? {
            let s = read_run(&self.db, id)?;
            if s.section.domain != port.domain {
                continue;
            }
            let (q, _, dist) = nearest_on(&s.pts(), p);
            if best.as_ref().is_none_or(|b| dist < b.1) {
                best = Some((q, dist, s));
            }
        }
        let Some((_, _, main)) = best else {
            return invalid(format!("system {system} has no {} run", port.domain.name()));
        };
        let mp = main.pts();
        let stub = (port.size * 3.0).max(0.1);
        let a = p + d * stub;
        let (q0, _, _) = nearest_on(&mp, a);
        let b = v3(a.x, a.y, q0.z);
        let (q, _, _) = nearest_on(&mp, b);
        let mut pts = vec![p, a];
        for x in [b, q] {
            if (x - *pts.last().unwrap()).len() > TOL {
                pts.push(x);
            }
        }
        let mut spec = match (main.material, main.section.domain) {
            (Some(m), Domain::Pipe) => {
                let dn = (port.size * 1000.0).round() as u32;
                let size = m
                    .sizes()
                    .into_iter()
                    .find(|s| s.dn >= dn)
                    .ok_or_else(|| Error::Invalid(format!("no DN{dn} in {}", m.name())))?;
                RunSpec::pipe(system, main.level, m, size.dn, vec![])?
            }
            _ => {
                let mut sec = main.section;
                sec.shape = Shape::Round { d: port.size };
                RunSpec::new(system, main.level, sec, vec![])
            }
        };
        spec.level = main.level;
        spec.section.insulation = main.section.insulation;
        spec.points = pts.into_iter().map(p3).collect();
        self.add_mep_run(&spec)
    }
}

/// Structure bodies: (kind, id) and bodies.
type Bodies = (Vec<(&'static str, i64)>, Vec<Body>);
/// Section outline in world coordinates.
pub type Contour = Vec<(f64, f64, f64)>;

fn dedup_pts(p: Vec<V3>) -> Vec<V3> {
    let mut out: Vec<V3> = Vec::with_capacity(p.len());
    for q in p {
        if out.last().is_none_or(|l| (q - *l).len() > 1e-9) {
            out.push(q);
        }
    }
    out
}

const PI_M: f64 = std::f64::consts::PI;
/// Joint bend between different sections from which an elbow is needed; run slopes give less, rad.
const CORNER_MIN_TURN: f64 = 5.0 * PI_M / 180.0;

/// Wall axis length: chord or arc (bulge = tan(θ/4)).
fn arc_len(a: V2, b: V2, bulge: f64) -> f64 {
    let c = (b - a).len();
    if bulge.abs() < 1e-12 || c < 1e-12 {
        return c;
    }
    let th = 4.0 * bulge.atan();
    (c / (2.0 * (th / 2.0).sin()) * th).abs()
}

/// Wall axis points: straight is its ends; an arc (bulge = tan(θ/4)) is 8 chords.
fn arc_points(a: V2, b: V2, bulge: f64) -> Vec<V2> {
    if bulge.abs() < 1e-12 {
        return vec![a, b];
    }
    let th = 4.0 * bulge.atan();
    let c = b - a;
    let l = c.len();
    let r = l / (2.0 * (th / 2.0).sin());
    let mid = (a + b) * 0.5;
    let n = v2(-c.y / l, c.x / l);
    let h = r * (th / 2.0).cos();
    let center = mid + n * h;
    let a0 = (a.y - center.y).atan2(a.x - center.x);
    (0..=8)
        .map(|k| {
            let t = a0 + th * k as f64 / 8.0;
            center + v2(r.abs() * t.cos(), r.abs() * t.sin())
        })
        .collect()
}

mod check;
mod edit;
mod follow;
pub use follow::{take_follow_problems, FollowGhost};
mod manual;
mod split;
pub use split::{run_axis_length, NewSize};
pub use edit::{
    nearest_dn, run_slope_pct, start_z, with_dn, with_duct_shape, with_elevation, with_material,
    with_slope, DuctShape,
};
pub use check::{MepDeviceRow, MepIssue, MepRunFlow, MepSystemCheck};
pub(crate) use manual::{migrate as migrate_ports, prune_ports};
pub use manual::{
    routed_points, DeviceSpec, MepHit, MepSnapSet, Placed, PortLink, RouteEnd, RouteSpec,
};

mod autoroute;
mod riser_join;
pub use ddc_geom::mep::duct_sizing;
mod build;
mod cable;
mod circuit;
mod wires;
pub use wires::{bow as wire_bow, cable_mesh, CircuitWire, CABLE_DRAWN_R};
mod single_line;
pub use single_line::{Feeder, SingleLine};
pub use cable::{
    cable_log_csv, AmpTable, CableInput, CableRow, CableRules, Conductor, Laying, LoadType,
    Standard, RuleFinding, Sized as CableSized,
};
pub use autoroute::{NetReq, Planned, RouteFrom, RouteMetrics, VARIANTS};
pub use build::{Built, MepBuild, SysRef};
pub(crate) use circuit::{migrate as migrate_circuits, prune as prune_circuits};
pub use circuit::{
    auto_size, preview_rules, Circuit, CircuitDevice, CircuitIssue, CircuitKind, CircuitReport,
    CircuitSpec, CableTrace, PanelSchedule,
};

mod clash_set;
pub(crate) use clash_set::{clash_begin, clash_gone, clash_track};
pub use clash_set::{
    is_mep, ClashFix, ClashHit, ClashInput, ClashItem, ClashMerge, ClashParams, ClashSide,
    ClashStatus, ElKey, CLASH_CATS,
};

#[cfg(test)]
mod manual_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod follow_tests;
#[cfg(test)]
mod autoroute_tests;
#[cfg(test)]
mod hvac_tests;
#[cfg(test)]
mod build_tests;
