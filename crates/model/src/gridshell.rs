// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Diagrids, gridshells and cable nets in the model: one journaled row per structure (host,
//! pattern, member types, node size) with a node table (position, normal, host parameters,
//! support) and a bar table (end nodes, role, continuous line). Members and nodes are built
//! from the node table at regeneration, so moving one node moves only the members at it.
//! Diagrid: straight members rolled to their end normals between ball nodes (the sphere-node
//! connection sizes the ball and sets the members back). Gridshell: smooth laths along a C1
//! curve through their line of nodes, framed torsion-free from the end normals, with clamp
//! nodes. Cable net: straight cables between clamps. Form finding rewrites the node table in
//! one operation (force density, or dynamic relaxation with lath bending).

use std::collections::HashMap;

use ddc_geom::arc::BulgeRing;
use ddc_geom::formfind::{force_density, relax, EdgeLaw, Relax};
use ddc_geom::frame3::{self, FrameMode, Law, Seg, Smooth};
use ddc_geom::gsolid::{self, Form, Path3, Section};
use ddc_geom::host::{Host, HostSurface};
use ddc_geom::member::{Just, Member, Path, Shape};
use ddc_geom::steel::gridshell::{generate, BarRole, GBar, GNode, Grid, NO_LINE};
use ddc_geom::steel::{self, ConnKind, End, Params};
use ddc_geom::{v2, v3, V2, V3};
use rusqlite::types::Value;
use rusqlite::{params, Connection};

pub use ddc_geom::steel::gridshell::{GridKind, Pattern as GridPattern, Supports as GridSupports};

use crate::ops::Inv;
use crate::struct_ext::Part;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS gridshell(id INTEGER PRIMARY KEY, level INTEGER NOT NULL,
    kind TEXT NOT NULL, host TEXT NOT NULL, pattern TEXT NOT NULL, nu INTEGER NOT NULL,
    nv INTEGER NOT NULL, supports TEXT NOT NULL, mtype INTEGER NOT NULL, edge_type INTEGER,
    node_d REAL NOT NULL);
CREATE TABLE IF NOT EXISTS gridshell_node(shell INTEGER NOT NULL, idx INTEGER NOT NULL,
    x REAL NOT NULL, y REAL NOT NULL, z REAL NOT NULL, nx REAL NOT NULL, ny REAL NOT NULL,
    nz REAL NOT NULL, u REAL NOT NULL, v REAL NOT NULL, fixed INTEGER NOT NULL,
    PRIMARY KEY(shell, idx));
CREATE TABLE IF NOT EXISTS gridshell_bar(shell INTEGER NOT NULL, idx INTEGER NOT NULL,
    a INTEGER NOT NULL, b INTEGER NOT NULL, role TEXT NOT NULL, line INTEGER NOT NULL,
    PRIMARY KEY(shell, idx));
";

/// Gap kept between members on a node and between nodes, m.
pub const NODE_GAP: f64 = 0.01;
const MAX_NODES: usize = 20_000;

/// A grid structure on a host surface. Host coordinates are from the level (z up from it).
#[derive(Clone, Debug, PartialEq)]
pub struct GridShellSpec {
    pub level: i64,
    pub kind: GridKind,
    pub host: Host,
    pub pattern: GridPattern,
    pub nu: usize,
    pub nv: usize,
    pub supports: GridSupports,
    pub member_type: i64,
    /// Section of the boundary members; None takes `member_type`.
    pub edge_type: Option<i64>,
    /// Ball or clamp diameter, m; 0 sizes ball nodes from the catalogue and clamps from the
    /// members.
    pub node_d: f64,
}

/// One node as stored: position and normal (z from the level), host parameters, support.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridShellNode {
    pub p: V3,
    pub n: V3,
    pub uv: (f64, f64),
    pub fixed: bool,
}

/// Form finding of a whole grid (one operation, undoable).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FormFind {
    /// Force density `q` (N/m) on grid members, `edge_q` on boundary members, `load` (N) along
    /// z on each free node: a cable net hangs, an upward load gives a compression shell.
    Density { q: f64, edge_q: f64, load: f64 },
    /// Dynamic relaxation of elastic members (EA, N) straight at their current length and
    /// laths of bending stiffness EI (N m2), with `load` (N) along z on each free node.
    Bending { ea: f64, ei: f64, load: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormFindReport {
    /// Largest out-of-balance force after the solve, N.
    pub residual: f64,
    /// Relaxation steps (0 for the linear solve).
    pub steps: usize,
    /// Axial forces, N (tension positive): least and greatest.
    pub min_force: f64,
    pub max_force: f64,
    /// Largest node movement, m.
    pub moved: f64,
}

/// Geometry facts of one grid as built now.
#[derive(Clone, Debug, PartialEq)]
pub struct GridShellReport {
    pub nodes: usize,
    pub bars: usize,
    pub supports: usize,
    /// Sum of member axis lengths, m.
    pub length: f64,
    /// Largest turn between a straight member web and an end normal, rad.
    pub max_twist: f64,
    /// Node clashes and node details that could not be built (English).
    pub clashes: Vec<String>,
}

/// A host the way a person draws it (the ribbon tool, ops and scripts), in level coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridForm {
    /// Spherical cap on a base circle of `radius` at `center`, `rise` at most the radius;
    /// `oculus` is the radius of the top ring (a grid cannot meet in the pole), 0 takes a
    /// tenth of the base radius.
    Dome { center: V3, radius: f64, rise: f64, oculus: f64 },
    /// Circular barrel vault: `a` to `b` is the level axis at the springing line, `span`
    /// across it, `rise` at the crown at most half the span.
    Vault { a: V3, b: V3, span: f64, rise: f64 },
    /// Saddle over an `lx` x `ly` rectangle centred on `center`, turned by `angle` (rad) about
    /// the vertical, corners at +-`rise`.
    Hypar { center: V3, lx: f64, ly: f64, rise: f64, angle: f64 },
    /// Flat grid over a `w` x `h` rectangle from `corner`, turned by `angle` (rad).
    Plane { corner: V3, w: f64, h: f64, angle: f64 },
    /// Dish from its `vertex`: rim `radius`, `depth` to the rim plane, axis tilted by `tilt`
    /// (rad) toward the plan direction `azimuth` (rad); `hub` is the inner ring, 0 a tenth.
    Paraboloid { vertex: V3, radius: f64, depth: f64, hub: f64, tilt: f64, azimuth: f64 },
}

impl GridForm {
    /// The analytic host; the message names the value that cannot make one.
    pub fn host(&self) -> Result<Host, Error> {
        match *self {
            GridForm::Dome { center, radius, rise, oculus } => Host::dome(center, radius, rise, oculus),
            GridForm::Vault { a, b, span, rise } => Host::vault(a, b, span, rise),
            GridForm::Hypar { center, lx, ly, rise, angle } => Host::hypar_at(center, lx, ly, rise, angle),
            GridForm::Plane { corner, w, h, angle } => Host::plane_at(corner, w, h, angle),
            GridForm::Paraboloid { vertex, radius, depth, hub, tilt, azimuth } => {
                Host::paraboloid(vertex, radius, depth, hub, tilt, azimuth)
            }
        }
        .map_err(Error::Invalid)
    }

    /// Where the grid stands when nothing else is asked: a dome on its springing ring, a
    /// vault and a flat grid on their edges, a saddle on its four corners.
    pub fn default_supports(&self) -> GridSupports {
        match self {
            // a dish stands on its hub ring, the first lattice row
            GridForm::Dome { .. } | GridForm::Paraboloid { .. } => GridSupports::Base,
            GridForm::Vault { .. } | GridForm::Plane { .. } => GridSupports::Edges,
            GridForm::Hypar { .. } => GridSupports::Corners,
        }
    }
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn shape_of(db: &Connection, id: i64) -> Result<Option<(Shape, String)>, Error> {
    let mut st = db.prepare_cached("SELECT shape, material FROM member_type WHERE id=?1")?;
    let mut rows = st.query([id])?;
    Ok(match rows.next()? {
        Some(r) => {
            let (s, m): (String, String) = (r.get(0)?, r.get(1)?);
            Shape::parse(&s).map(|sh| (sh, m))
        }
        None => None,
    })
}

fn check(db: &Connection, s: &GridShellSpec) -> Result<(), Error> {
    s.host.check().map_err(Error::Invalid)?;
    for t in std::iter::once(s.member_type).chain(s.edge_type) {
        if shape_of(db, t)?.is_none() {
            return invalid(format!("no member type {t}"));
        }
    }
    if !(s.node_d.is_finite() && s.node_d >= 0.0) {
        return invalid("node diameter must be zero (sized) or positive".into());
    }
    let lv: i64 = db.query_row("SELECT count(*) FROM level WHERE id=?1", [s.level], |r| r.get(0))?;
    if lv == 0 {
        return invalid(format!("no level {}", s.level));
    }
    Ok(())
}

fn grid_of(s: &GridShellSpec) -> Result<Grid, Error> {
    let g = generate(&s.host, s.pattern, s.nu, s.nv, s.supports).map_err(Error::Invalid)?;
    if g.nodes.len() > MAX_NODES {
        return invalid(format!(
            "{} nodes is over the {MAX_NODES} a grid may have",
            g.nodes.len()
        ));
    }
    if g.nodes.iter().all(|n| !n.fixed) {
        return invalid("the grid has no supported node".into());
    }
    Ok(g)
}

const SEL: &str = "SELECT id, level, kind, host, pattern, nu, nv, supports, mtype, edge_type, node_d \
                   FROM gridshell";

fn row(r: &rusqlite::Row) -> rusqlite::Result<(i64, Option<GridShellSpec>)> {
    let kind: String = r.get(2)?;
    let host: String = r.get(3)?;
    let pat: String = r.get(4)?;
    let sup: String = r.get(7)?;
    let spec = (|| {
        Some(GridShellSpec {
            level: r.get(1).ok()?,
            kind: GridKind::parse(&kind)?,
            host: Host::parse(&host)?,
            pattern: GridPattern::parse(&pat)?,
            nu: r.get::<_, i64>(5).ok()? as usize,
            nv: r.get::<_, i64>(6).ok()? as usize,
            supports: GridSupports::parse(&sup)?,
            member_type: r.get(8).ok()?,
            edge_type: r.get(9).ok()?,
            node_d: r.get(10).ok()?,
        })
    })();
    Ok((r.get(0)?, spec))
}

fn spec_of(db: &Connection, id: i64) -> Result<GridShellSpec, Error> {
    let (_, s) = db
        .query_row(&format!("{SEL} WHERE id=?1"), [id], row)
        .map_err(|_| Error::Invalid(format!("no such grid {id}")))?;
    s.ok_or_else(|| Error::Invalid(format!("grid {id} has an unreadable definition")))
}

fn nodes_of(db: &Connection, id: i64) -> Result<Vec<GridShellNode>, Error> {
    let mut st = db.prepare_cached(
        "SELECT x, y, z, nx, ny, nz, u, v, fixed FROM gridshell_node WHERE shell=?1 ORDER BY idx",
    )?;
    let rows = st.query_map([id], |r| {
        Ok(GridShellNode {
            p: v3(r.get(0)?, r.get(1)?, r.get(2)?),
            n: v3(r.get(3)?, r.get(4)?, r.get(5)?),
            uv: (r.get(6)?, r.get(7)?),
            fixed: r.get::<_, i64>(8)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn bars_of(db: &Connection, id: i64) -> Result<Vec<GBar>, Error> {
    let mut st = db.prepare_cached(
        "SELECT a, b, role, line FROM gridshell_bar WHERE shell=?1 ORDER BY idx",
    )?;
    let rows = st.query_map([id], |r| {
        let role: String = r.get(2)?;
        let line: i64 = r.get(3)?;
        Ok(GBar {
            a: r.get::<_, i64>(0)? as u32,
            b: r.get::<_, i64>(1)? as u32,
            role: BarRole::parse(&role).unwrap_or(BarRole::Grid),
            line: if line < 0 { NO_LINE } else { line as u32 },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn write_node(db: &Connection, id: i64, i: usize, n: &GridShellNode) -> Result<(), Error> {
    db.prepare_cached(
        "INSERT OR REPLACE INTO gridshell_node(shell,idx,x,y,z,nx,ny,nz,u,v,fixed) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
    )?
    .execute(params![
        id, i as i64, n.p.x, n.p.y, n.p.z, n.n.x, n.n.y, n.n.z, n.uv.0, n.uv.1, n.fixed as i64
    ])?;
    Ok(())
}

fn insert(db: &Connection, s: &GridShellSpec, g: &Grid) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO gridshell(level,kind,host,pattern,nu,nv,supports,mtype,edge_type,node_d) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            s.level,
            s.kind.key(),
            s.host.text(),
            s.pattern.key(),
            s.nu as i64,
            s.nv as i64,
            s.supports.key(),
            s.member_type,
            s.edge_type,
            s.node_d
        ],
    )?;
    let id = db.last_insert_rowid();
    for (i, n) in g.nodes.iter().enumerate() {
        write_node(
            db,
            id,
            i,
            &GridShellNode {
                p: n.p,
                n: n.n,
                uv: n.uv,
                fixed: n.fixed,
            },
        )?;
    }
    let mut st = db.prepare_cached(
        "INSERT INTO gridshell_bar(shell,idx,a,b,role,line) VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    for (k, b) in g.bars.iter().enumerate() {
        let line = if b.line == NO_LINE { -1 } else { b.line as i64 };
        st.execute(params![id, k as i64, b.a as i64, b.b as i64, b.role.key(), line])?;
    }
    Ok(id)
}

/// Stored grid (nodes shifted by `dz`), topology regenerated for the faces.
fn stored_grid(db: &Connection, id: i64, dz: f64) -> Result<(GridShellSpec, Grid), Error> {
    let s = spec_of(db, id)?;
    let nodes = nodes_of(db, id)?;
    let bars = bars_of(db, id)?;
    let fresh = generate(&s.host, s.pattern, s.nu, s.nv, s.supports).map_err(Error::Invalid)?;
    let same = fresh.nodes.len() == nodes.len()
        && fresh.bars.len() == bars.len()
        && fresh.bars.iter().zip(&bars).all(|(x, y)| x.a == y.a && x.b == y.b);
    let g = Grid {
        nodes: nodes
            .iter()
            .map(|n| GNode {
                p: n.p + v3(0.0, 0.0, dz),
                n: n.n,
                uv: n.uv,
                fixed: n.fixed,
            })
            .collect(),
        bars,
        cells: if same { fresh.cells } else { vec![] },
    };
    Ok((s, g))
}

fn level_z(db: &Connection, level: i64) -> Result<f64, Error> {
    Ok(crate::walls::levels(db)?
        .into_iter()
        .find(|l| l.0 == level)
        .map_or(0.0, |l| l.1))
}

impl Model {
    /// Generates a grid on its host and stores it (one operation).
    pub fn add_gridshell(&mut self, s: &GridShellSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        let g = grid_of(s)?;
        self.op("member_gridshell_add", |db| {
            let id = insert(db, s, &g)?;
            let inv = Inv::default()
                .delete("gridshell_bar", "shell", id)
                .delete("gridshell_node", "shell", id)
                .delete("gridshell", "id", id);
            Ok((id, id, inv))
        })
    }

    /// Member type named as a catalogue section ("CHS 88.9x5"): the stored row of that name,
    /// else a new steel type from the catalogue.
    pub fn member_type_for_section(&mut self, name: &str) -> Result<i64, Error> {
        match self.member_types()?.into_iter().find(|t| t.1 == name) {
            Some(t) => Ok(t.0),
            None => self.add_catalog_member_type(name),
        }
    }

    /// Every grid on `level` (all levels when None), by id.
    pub fn gridshells(&self, level: Option<i64>) -> Result<Vec<i64>, Error> {
        self.struct_ids("gridshell", level)
    }

    pub fn gridshell(&self, id: i64) -> Result<GridShellSpec, Error> {
        spec_of(&self.db, id)
    }

    pub fn gridshell_nodes(&self, id: i64) -> Result<Vec<GridShellNode>, Error> {
        nodes_of(&self.db, id)
    }

    /// Members as (start node, end node, role, line; `u32::MAX` for bracing).
    pub fn gridshell_bars(&self, id: i64) -> Result<Vec<(u32, u32, &'static str, u32)>, Error> {
        Ok(bars_of(&self.db, id)?
            .into_iter()
            .map(|b| (b.a, b.b, b.role.key(), b.line))
            .collect())
    }

    /// Moves node `idx` to `p` (z from the level); `snap` puts it on the host at the closest
    /// point and takes the host normal there. Only the members at the node follow.
    pub fn move_gridshell_node(&mut self, id: i64, idx: usize, p: V3, snap: bool) -> Result<(), Error> {
        if !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()) {
            return invalid("node position must be finite".into());
        }
        let s = spec_of(&self.db, id)?;
        let nodes = nodes_of(&self.db, id)?;
        let Some(old) = nodes.get(idx).copied() else {
            return invalid(format!("grid {id} has no node {idx}"));
        };
        let new = if snap {
            let (u, v) = s.host.project(p);
            GridShellNode {
                p: s.host.point(u, v),
                n: s.host.normal(u, v),
                uv: (u, v),
                fixed: old.fixed,
            }
        } else {
            GridShellNode { p, ..old }
        };
        self.op("member_gridshell_node", |db| {
            let inv = Inv::default().restore_where(
                db,
                "gridshell_node",
                "shell=?1 AND idx=?2",
                &[Value::Integer(id), Value::Integer(idx as i64)],
            )?;
            write_node(db, id, idx, &new)?;
            Ok(((), id, inv))
        })
    }

    /// Form finding of every free node; normals follow the new faces. One operation.
    pub fn form_find_gridshell(&mut self, id: i64, f: &FormFind) -> Result<FormFindReport, Error> {
        let (_, mut g) = stored_grid(&self.db, id, 0.0)?;
        let start = g.positions();
        let e = g.edges();
        let fixed = g.fixed();
        let fin = |v: f64| v.is_finite();
        let (pos, force, residual, steps) = match *f {
            FormFind::Density { q, edge_q, load } => {
                if !(fin(load) && q > 0.0 && edge_q > 0.0 && fin(q) && fin(edge_q)) {
                    return invalid("force densities must be positive and the load finite".into());
                }
                let qs: Vec<f64> = g
                    .bars
                    .iter()
                    .map(|b| if b.role == BarRole::Edge { edge_q } else { q })
                    .collect();
                let load = vec![v3(0.0, 0.0, load); start.len()];
                let r = force_density(&start, &e, &qs, &fixed, &load)
                    .map_err(|x| Error::Geom(x.to_string()))?;
                (r.pos, r.force, r.residual, 0)
            }
            FormFind::Bending { ea, ei, load } => {
                if !(fin(load) && ea > 0.0 && ei > 0.0 && fin(ea) && fin(ei)) {
                    return invalid("EA and EI must be positive and the load finite".into());
                }
                let law: Vec<EdgeLaw> = e
                    .iter()
                    .map(|&(a, b)| EdgeLaw::Elastic {
                        ea,
                        l0: (start[b as usize] - start[a as usize]).len(),
                    })
                    .collect();
                let bends = g.bends(ei);
                let load = vec![v3(0.0, 0.0, load); start.len()];
                let scale = load.iter().map(|p| p.len()).fold(ea * 1e-9, f64::max);
                let r = relax(
                    &start,
                    &Relax {
                        edges: &e,
                        law: &law,
                        bends: &bends,
                        fixed: &fixed,
                        load: &load,
                        tol: scale * 1e-6,
                        max_steps: 400_000,
                    },
                )
                .map_err(|x| Error::Geom(x.to_string()))?;
                (r.pos, r.force, r.residual, r.steps)
            }
        };
        let moved = pos
            .iter()
            .zip(&start)
            .map(|(a, b)| (*a - *b).len())
            .fold(0.0, f64::max);
        for (n, p) in g.nodes.iter_mut().zip(&pos) {
            n.p = *p;
        }
        g.normals_from_cells();
        let report = FormFindReport {
            residual,
            steps,
            min_force: force.iter().copied().fold(f64::INFINITY, f64::min),
            max_force: force.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            moved,
        };
        self.op("member_gridshell_form", |db| {
            let inv = Inv::default().restore(db, "gridshell_node", "shell", id)?;
            for (i, n) in g.nodes.iter().enumerate() {
                write_node(
                    db,
                    id,
                    i,
                    &GridShellNode {
                        p: n.p,
                        n: n.n,
                        uv: n.uv,
                        fixed: n.fixed,
                    },
                )?;
            }
            Ok(((), id, inv))
        })?;
        Ok(report)
    }

    /// Counts, member length, the largest member twist and the node clashes of a grid.
    pub fn gridshell_report(&self, id: i64) -> Result<GridShellReport, Error> {
        let (s, g) = stored_grid(&self.db, id, 0.0)?;
        let built = build_one(&self.db, id, &s, &g, 0.0)?;
        Ok(GridShellReport {
            nodes: g.nodes.len(),
            bars: g.bars.len(),
            supports: g.nodes.iter().filter(|n| n.fixed).count(),
            length: g
                .bars
                .iter()
                .map(|b| (g.nodes[b.b as usize].p - g.nodes[b.a as usize].p).len())
                .sum(),
            max_twist: (0..g.bars.len()).map(|k| g.bar_twist(k)).fold(0.0, f64::max),
            clashes: built.iter().flat_map(|p| p.warn.iter().cloned()).collect(),
        })
    }
}

/// Section of a member type for a sweep.
fn section(sh: &Shape) -> Section {
    let (outer, hole) = sh.rings();
    Section {
        outer: BulgeRing::from_polygon(outer),
        holes: hole.into_iter().map(BulgeRing::from_polygon).collect(),
    }
}

/// Section turn of a straight member so its local z (section height) lies along `w`; the
/// unturned axes are those of `ddc_geom::member` (y = Z x axis, or X on a vertical axis).
fn roll_for(u: V3, w: V3) -> f64 {
    let h = v3(0.0, 0.0, 1.0).cross(u);
    let ey = if h.len() < 1e-9 {
        v3(1.0, 0.0, 0.0)
    } else {
        h.normalized()
    };
    let ez = u.cross(ey);
    (-w.dot(ey)).atan2(w.dot(ez))
}

fn signed(a: V3, b: V3, t: V3) -> f64 {
    a.cross(b).dot(t).atan2(a.dot(b))
}

fn square_to(n: V3, t: V3) -> Option<V3> {
    let w = n - t * n.dot(t);
    (w.len() > 1e-9).then(|| w.normalized())
}

const LATH_CHORD: f64 = 2e-3;
const LATH_ANGLE: f64 = 0.05;

/// Smooth lath from node a to node b through `via`, rolled so the section height lies along
/// the normal at a, and twisted linearly to meet the normal at b.
fn lath(pa: V3, pb: V3, via: Vec<V3>, na: V3, nb: V3) -> Result<Smooth, String> {
    let mut sm = Smooth {
        start: pa,
        segs: vec![Seg::Spline { via, to: pb }],
        frame: FrameMode::Rmf,
        roll: 0.0,
        twist: Law::constant(0.0),
        scale: Law::constant(1.0),
        role: frame3::Role::Member,
    };
    let st = frame3::stations(&sm, LATH_CHORD, LATH_ANGLE).map_err(|e| e.to_string())?;
    let s0 = st.st[0];
    if let Some(w) = square_to(na, s0.t) {
        sm.roll = signed(s0.v, w, s0.t);
    }
    // the rolled frame at the far end, then the twist that meets the normal there
    let st = frame3::stations(&sm, LATH_CHORD, LATH_ANGLE).map_err(|e| e.to_string())?;
    let s1 = st.st[st.st.len() - 1];
    if let Some(w) = square_to(nb, s1.t) {
        sm.twist = Law::Linear(0.0, signed(s1.v, w, s1.t));
    }
    Ok(sm)
}

fn circle(d: f64) -> BulgeRing {
    BulgeRing {
        pts: vec![v2(d / 2.0, 0.0), v2(-d / 2.0, 0.0)],
        bulges: vec![1.0, 1.0],
    }
}

fn hull_part(mut p: Part, meshes: Vec<ddc_geom::mesh::Mesh>) -> Part {
    let xy: Vec<V2> = meshes
        .iter()
        .flat_map(|m| m.positions.iter().map(|q| v2(q.x, q.y)))
        .collect();
    let plan = crate::struct_ext::hull(&xy);
    if plan.len() >= 3 {
        p.plan = vec![plan];
    }
    p.meshes = meshes;
    p
}

fn bar_role(kind: GridKind, r: BarRole) -> &'static str {
    match (kind, r) {
        (GridKind::CableNet, _) => "cable",
        (_, BarRole::Edge) => "edge",
        (_, BarRole::Brace) => "brace",
        (GridKind::Gridshell, BarRole::Grid) => "lath",
        (GridKind::Diagrid, BarRole::Grid) => "member",
    }
}

/// Parts of one grid: members first (part = bar index), then nodes (part = bars + node).
fn build_one(db: &Connection, id: i64, s: &GridShellSpec, g: &Grid, _elev: f64) -> Result<Vec<Part>, Error> {
    let (Some((msh, mmat)), edge) = (shape_of(db, s.member_type)?, s.edge_type) else {
        return Ok(vec![]);
    };
    let (esh, emat) = match edge {
        Some(t) => shape_of(db, t)?.unwrap_or((msh.clone(), mmat.clone())),
        None => (msh.clone(), mmat.clone()),
    };
    let sec = |b: &GBar| -> (&Shape, &str) {
        if b.role == BarRole::Edge {
            (&esh, &emat)
        } else {
            (&msh, &mmat)
        }
    };
    let half = |sh: &Shape| {
        let (w, h) = sh.size();
        w.max(h) / 2.0
    };
    let bar_r: Vec<f64> = g.bars.iter().map(|b| half(sec(b).0)).collect();
    let rmax = bar_r.iter().copied().fold(0.0, f64::max);
    let adj = g.adjacency();
    let nb = g.bars.len();
    let mut warn: Vec<Vec<String>> = vec![vec![]; g.nodes.len()];
    // node details first: ball nodes set the member ends back
    // (bar, at its start)
    let mut setback: HashMap<(usize, bool), f64> = HashMap::new();
    let mut node_parts: Vec<Part> = vec![];
    for (i, nd) in g.nodes.iter().enumerate() {
        let mut p = Part::new("gridshell", id, s.level, "node", &mmat);
        p.part = (nb + i) as i64;
        let mut meshes = vec![];
        let mut volume = 0.0;
        let mut built = false;
        if s.kind == GridKind::Diagrid && adj[i].len() >= 2 {
            let members: Vec<Member> = adj[i]
                .iter()
                .map(|&k| {
                    let b = &g.bars[k as usize];
                    let (a, c) = (g.nodes[b.a as usize].p, g.nodes[b.b as usize].p);
                    line_member(sec(b).0, a, c, g.bar_up(k as usize))
                })
                .collect();
            let prm = Params {
                ball: (s.node_d > 0.0).then_some(s.node_d),
                ..Params::default()
            };
            match steel::build(ConnKind::SphereNode, &members, &prm) {
                Ok(conn) => {
                    for q in &conn.parts {
                        if let Ok(m) = gsolid::mesh(&q.form, &q.frame, gsolid::VIEW_SAG) {
                            meshes.push(m);
                        }
                        volume += gsolid::quantities(&q.form).map_or(0.0, |x| x.volume);
                    }
                    for t in &conn.trims {
                        let k = adj[i][t.member] as usize;
                        setback.insert((k, t.end == End::Start), t.setback);
                    }
                    p.material = conn
                        .parts
                        .first()
                        .map_or(mmat.clone(), |q| q.material.split(';').next().unwrap_or("").to_string());
                    built = true;
                }
                Err(e) => warn[i].push(format!("node {i}: {}", e.0)),
            }
        }
        if !built {
            // clamp: a cylinder along the normal through the node
            let d = if s.node_d > 0.0 { s.node_d } else { 2.5 * rmax };
            let hgt = 4.0 * rmax.max(d / 4.0);
            let [x, y, z] = g.node_frame(i, &adj);
            let fr = gsolid::Frame {
                o: nd.p - z * (hgt / 2.0),
                x,
                y,
                z,
            };
            let form = Form::Extrusion {
                profile: Section {
                    outer: circle(d),
                    holes: vec![],
                },
                start: 0.0,
                end: hgt,
            };
            if let Ok(m) = gsolid::mesh(&form, &fr, gsolid::VIEW_SAG) {
                meshes.push(m);
            }
            volume = gsolid::quantities(&form).map_or(0.0, |x| x.volume);
            p.size = [d, d, hgt];
        }
        p.volume = volume;
        p.area = meshes.iter().map(mesh_area).sum();
        node_parts.push(hull_part(p, meshes));
    }
    let (clash_d, sleeve_r) = match s.kind {
        // members end on the ball: their sleeves must not overlap on it
        GridKind::Diagrid => (s.node_d, bar_r.clone()),
        // laths and cables run on through the clamp: only node spacing counts
        _ => (if s.node_d > 0.0 { s.node_d } else { 2.5 * rmax }, vec![0.0; nb]),
    };
    for c in g.clashes(clash_d, &sleeve_r, NODE_GAP) {
        warn[c.node as usize].push(c.text);
    }
    let lines = g.line_nbrs();
    let mut out = vec![];
    for (k, b) in g.bars.iter().enumerate() {
        let (sh, mat) = sec(b);
        let role = bar_role(s.kind, b.role);
        let (na, nbn) = (g.nodes[b.a as usize], g.nodes[b.b as usize]);
        let curved = s.kind == GridKind::Gridshell && b.line != NO_LINE;
        if curved {
            let via = g.lath_via(k, &lines);
            match lath(na.p, nbn.p, via, na.n, nbn.n) {
                Ok(sm) => {
                    let len = sm.length().unwrap_or(0.0);
                    let form = Form::Sweep {
                        profile: section(sh),
                        path: Path3::smooth(sm),
                    };
                    let mesh = gsolid::mesh(&form, &gsolid::Frame::default(), gsolid::VIEW_SAG);
                    let q = gsolid::quantities(&form);
                    if let (Ok(mesh), Ok(q)) = (mesh, q) {
                        let mut p = Part::new("gridshell", id, s.level, role, mat);
                        p.part = k as i64;
                        p.length = len;
                        p.volume = q.volume;
                        p.area = q.area;
                        let (w, h) = sh.size();
                        p.size = [len, w, h];
                        out.push(hull_part(p, vec![mesh]));
                        continue;
                    }
                    warn[b.a as usize].push(format!("lath {k} could not be swept along its curve; built straight"));
                }
                Err(e) => warn[b.a as usize].push(format!("lath {k}: {e}; built straight")),
            }
        }
        let e = (nbn.p - na.p).normalized();
        let sa = setback.get(&(k, true)).copied().unwrap_or(0.0);
        let se = setback.get(&(k, false)).copied().unwrap_or(0.0);
        let m = line_member(sh, na.p + e * sa, nbn.p - e * se, g.bar_up(k));
        let mut p = crate::framing::mpart("gridshell", id, s.level, k as i64, role, mat, &m);
        p.part = k as i64;
        out.push(p);
    }
    for (mut p, w) in node_parts.into_iter().zip(warn) {
        p.warn = w;
        out.push(p);
    }
    Ok(out)
}

fn mesh_area(m: &ddc_geom::mesh::Mesh) -> f64 {
    m.triangles
        .iter()
        .map(|t| {
            let (a, b, c) = (
                m.positions[t[0] as usize],
                m.positions[t[1] as usize],
                m.positions[t[2] as usize],
            );
            (b - a).cross(c - a).len() / 2.0
        })
        .sum()
}

fn line_member(sh: &Shape, a: V3, b: V3, up: V3) -> Member {
    let u = (b - a).normalized();
    Member {
        shape: sh.clone(),
        path: Path::Line { a, b },
        rotation: roll_for(u, up),
        yjust: Just::Center,
        zjust: Just::Center,
        column: false,
        start: None,
        end: None,
    }
}

/// Every grid of the model as parts (world coordinates).
pub(crate) fn build(db: &Connection) -> Result<Vec<Part>, Error> {
    let ids: Vec<i64> = {
        let mut st = db.prepare("SELECT id FROM gridshell ORDER BY id")?;
        let r = st.query_map([], |r| r.get(0))?;
        r.collect::<Result<_, _>>()?
    };
    let mut out = vec![];
    for id in ids {
        let Ok(s) = spec_of(db, id) else {
            continue;
        };
        let elev = level_z(db, s.level)?;
        let (s, g) = stored_grid(db, id, elev)?;
        out.extend(build_one(db, id, &s, &g, elev)?);
    }
    Ok(out)
}

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

#[cfg(test)]
#[path = "gridshell_tests.rs"]
mod tests;
