// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Manual network editing (schema 37). `mep_port` holds component connections: system, flow
//! overriding the family value, and the feeder run. Each action is one journal entry
//! (`mep_group`) plus a check of the system's network: an action that breaks the graph (no room
//! for a fitting) is rolled back entirely. User messages are `mep.err.*` keys; the view supplies the text.

use std::collections::HashMap;

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{self, Detail, Domain, PipeMaterial, PlanPrim, Section, Shape, SystemKind};
use rusqlite::{params, Connection, OptionalExtension};

use super::{
    check_spec, invalid, nearest_on, p3, read_run, MepPlanPrim, RunSpec, WorldPort, PORT_TOL, TOL,
};
use crate::ops::{merge_ops, Inv};
use crate::{CompSpec, Error, Lod, Model};

/// Minimum departure from a run on "disconnect", m.
const PULL_BACK: f64 = 0.1;
/// Slope of a drainage feeder toward the main, fraction.
const WASTE_FALL: f64 = 0.02;

const DDL: &str =
    "CREATE TABLE IF NOT EXISTS mep_port(id INTEGER PRIMARY KEY, comp INTEGER NOT NULL,
 connector TEXT NOT NULL, system INTEGER, flow REAL, stub INTEGER, UNIQUE(comp, connector));
CREATE INDEX IF NOT EXISTS mep_port_system ON mep_port(system);";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    Ok(())
}

/// Connections are fixed up in the record of the operation that deleted their target. No
/// component: the row goes away together with the feeder. No system: the link is dropped and the
/// set flow stays (as with "disconnect"). No feeder: the reference is dropped, since the run
/// number may go to a new run.
pub(crate) fn prune_ports(db: &Connection) -> Result<Inv, Error> {
    let rows: Vec<PortLink> = db
        .prepare_cached(
            "SELECT id,comp,connector,system,flow,stub FROM mep_port \
             WHERE comp NOT IN (SELECT id FROM comp) \
             OR (system IS NOT NULL AND system NOT IN (SELECT id FROM mep_system)) \
             OR (stub IS NOT NULL AND stub NOT IN (SELECT id FROM mep_run))",
        )?
        .query_map([], link_of)?
        .collect::<Result<_, _>>()?;
    let exists = |sql: &str, id: i64| -> Result<bool, Error> {
        Ok(db.query_row(sql, [id], |r| r.get::<_, i64>(0))? != 0)
    };
    let mut inv = Inv::default();
    for r in rows {
        inv = inv.restore(db, "mep_port", "id", r.id)?;
        let comp_gone = !exists("SELECT EXISTS(SELECT 1 FROM comp WHERE id=?1)", r.comp)?;
        let sys_gone = match r.system {
            Some(s) => !exists("SELECT EXISTS(SELECT 1 FROM mep_system WHERE id=?1)", s)?,
            None => false,
        };
        if comp_gone {
            if let Some(stub) = r.stub {
                if stub_is_leaf(db, stub)? {
                    inv = inv.restore(db, "mep_run", "id", stub)?;
                    db.execute("DELETE FROM mep_run WHERE id=?1", [stub])?;
                }
            }
            db.execute("DELETE FROM mep_port WHERE id=?1", [r.id])?;
        } else if sys_gone && r.flow.is_none() {
            db.execute("DELETE FROM mep_port WHERE id=?1", [r.id])?;
        } else if sys_gone {
            db.execute(
                "UPDATE mep_port SET system=NULL, stub=NULL WHERE id=?1",
                [r.id],
            )?;
        } else {
            db.execute("UPDATE mep_port SET stub=NULL WHERE id=?1", [r.id])?;
        }
    }
    Ok(inv)
}

/// Nothing is tapped into the feeder: it can be removed without detaching other runs.
fn stub_is_leaf(db: &Connection, stub: i64) -> Result<bool, Error> {
    let Ok(s) = read_run(db, stub) else {
        return Ok(false);
    };
    let pts = s.pts();
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let others: Vec<i64> = db
        .prepare_cached("SELECT id FROM mep_run WHERE system=?1 AND id<>?2")?
        .query_map(params![s.system, stub], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in others {
        let o = read_run(db, id)?.pts();
        for e in [o[0], o[o.len() - 1]] {
            let (q, _, d) = nearest_on(&pts, e);
            if d < TOL && (q - first).len() >= TOL && (q - last).len() >= TOL {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// A component's connection to a system.
#[derive(Clone, Debug, PartialEq)]
pub struct PortLink {
    pub id: i64,
    pub comp: i64,
    pub connector: String,
    pub system: Option<i64>,
    /// Flow or power overriding the family value.
    pub flow: Option<f64>,
    /// Feeder created by the connection: deleted on disconnect.
    pub stub: Option<i64>,
}

fn link_of(r: &rusqlite::Row) -> rusqlite::Result<PortLink> {
    Ok(PortLink {
        id: r.get(0)?,
        comp: r.get(1)?,
        connector: r.get(2)?,
        system: r.get(3)?,
        flow: r.get(4)?,
        stub: r.get(5)?,
    })
}

fn port_row(db: &Connection, comp: i64, connector: &str) -> Result<Option<PortLink>, Error> {
    Ok(db
        .query_row(
            "SELECT id,comp,connector,system,flow,stub FROM mep_port WHERE comp=?1 AND connector=?2",
            params![comp, connector],
            link_of,
        )
        .optional()?)
}

pub(super) fn link_port(
    db: &Connection,
    comp: i64,
    connector: &str,
    system: i64,
    stub: Option<i64>,
) -> Result<Inv, Error> {
    match port_row(db, comp, connector)? {
        Some(r) => {
            let inv = Inv::default().restore(db, "mep_port", "id", r.id)?;
            db.execute(
                "UPDATE mep_port SET system=?1, stub=?2 WHERE id=?3",
                params![system, stub, r.id],
            )?;
            Ok(inv)
        }
        None => {
            db.execute(
                "INSERT INTO mep_port(comp,connector,system,stub) VALUES (?1,?2,?3,?4)",
                params![comp, connector, system, stub],
            )?;
            Ok(Inv::default().delete("mep_port", "id", db.last_insert_rowid()))
        }
    }
}

fn v(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

/// Without repeats and collinear points: such a route has no extra zero-length segments.
fn clean(pts: &[V3]) -> Vec<V3> {
    let mut out: Vec<V3> = vec![];
    for &p in pts {
        if out.last().is_some_and(|l| (*l - p).len() < TOL) {
            continue;
        }
        while out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (u, w) = ((b - a).normalized(), (p - b).normalized());
            if u.dot(w) > 1.0 - 1e-9 {
                out.pop();
            } else {
                break;
            }
        }
        out.push(p);
    }
    out
}

fn plan_len(a: V3, b: V3) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

pub(super) fn same_net(a: Domain, b: Domain) -> bool {
    a == b
        || matches!(
            (a, b),
            (
                Domain::Conduit | Domain::CableTray,
                Domain::Conduit | Domain::CableTray
            )
        )
}

pub(super) fn accepts(port: &WorldPort, kind: SystemKind) -> bool {
    kind.domains().contains(&port.domain) && port.system.is_none_or(|k| k == kind)
}

/// Route points from plan points at elevation `z`: slope `slope` (%) lowers each segment; at ends
/// with a different elevation (network, connection) there is a vertical joint, and if it is
/// shorter than `min_leg` and cannot fit an elbow, the end is pulled in by tilting the last segment.
pub fn routed_points(
    plan: &[(f64, f64)],
    z: f64,
    slope: f64,
    start: Option<f64>,
    end: Option<f64>,
    min_leg: f64,
) -> Vec<(f64, f64, f64)> {
    let mut out = vec![];
    let Some(&(x0, y0)) = plan.first() else {
        return out;
    };
    let mut zc = z;
    if let Some(s) = start {
        if (s - z).abs() < min_leg {
            zc = s;
        } else {
            out.push((x0, y0, s));
        }
    }
    out.push((x0, y0, zc));
    for w in plan.windows(2) {
        zc -= slope / 100.0 * (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
        out.push((w[1].0, w[1].1, zc));
    }
    if let (Some(e), Some(&(xn, yn))) = (end, plan.last()) {
        if (e - zc).abs() >= min_leg {
            out.push((xn, yn, e));
        } else if let Some(l) = out.last_mut() {
            l.2 = e;
        }
    }
    out
}

/// Pick points in the plan: connections and run axes, a snapshot at build time.
#[derive(Clone, Debug, Default)]
pub struct MepSnapSet {
    pub ports: Vec<WorldPort>,
    runs: Vec<(i64, i64, Domain, Vec<V3>)>,
}

/// What is under the cursor.
#[derive(Clone, Debug, PartialEq)]
pub enum MepHit {
    Port(WorldPort),
    RunEnd {
        run: i64,
        end: u8,
        system: i64,
        at: (f64, f64, f64),
    },
    RunMid {
        run: i64,
        system: i64,
        at: (f64, f64, f64),
    },
}

impl MepHit {
    pub fn at(&self) -> (f64, f64, f64) {
        match self {
            MepHit::Port(p) => p.at,
            MepHit::RunEnd { at, .. } | MepHit::RunMid { at, .. } => *at,
        }
    }
    /// System of the run; a connection has none.
    pub fn system(&self) -> Option<i64> {
        match self {
            MepHit::Port(_) => None,
            MepHit::RunEnd { system, .. } | MepHit::RunMid { system, .. } => Some(*system),
        }
    }
    pub fn route_end(&self) -> RouteEnd {
        match self {
            MepHit::Port(p) => RouteEnd::Port {
                comp: p.comp,
                connector: p.name.clone(),
            },
            MepHit::RunEnd { run, at, .. } | MepHit::RunMid { run, at, .. } => {
                RouteEnd::Run { run: *run, at: *at }
            }
        }
    }
}

impl MepSnapSet {
    /// Nearest in the plan within radius `r` at elevation in `zs`: a connection, then a run end,
    /// then a point on an axis. `keep(kind, run system)` filters out foreign networks.
    pub fn pick(
        &self,
        q: (f64, f64),
        r: f64,
        zs: (f64, f64),
        keep: &dyn Fn(Domain, Option<i64>) -> bool,
    ) -> Option<MepHit> {
        let dist = |p: V3| (p.x - q.0).hypot(p.y - q.1);
        let in_z = |z: f64| z >= zs.0 - 1e-6 && z <= zs.1 + 1e-6;
        let port = self
            .ports
            .iter()
            .filter(|p| in_z(p.at.2) && keep(p.domain, None) && dist(v(p.at)) <= r)
            .min_by(|a, b| dist(v(a.at)).total_cmp(&dist(v(b.at))));
        if let Some(p) = port {
            return Some(MepHit::Port(p.clone()));
        }
        let mut end: Option<(f64, MepHit)> = None;
        let mut mid: Option<(f64, MepHit)> = None;
        for (run, sys, dom, pts) in &self.runs {
            if !keep(*dom, Some(*sys)) {
                continue;
            }
            for (e, p) in [(0u8, pts[0]), (1, pts[pts.len() - 1])] {
                let d = dist(p);
                if in_z(p.z) && d <= r && end.as_ref().is_none_or(|b| d < b.0) {
                    let hit = MepHit::RunEnd {
                        run: *run,
                        end: e,
                        system: *sys,
                        at: p3(p),
                    };
                    end = Some((d, hit));
                }
            }
            for w in pts.windows(2) {
                let l2 = (w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2);
                if l2 < 1e-12 {
                    continue;
                }
                let t = (((q.0 - w[0].x) * (w[1].x - w[0].x) + (q.1 - w[0].y) * (w[1].y - w[0].y))
                    / l2)
                    .clamp(0.0, 1.0);
                let p = w[0] + (w[1] - w[0]) * t;
                let d = dist(p);
                if in_z(p.z) && d <= r && mid.as_ref().is_none_or(|b| d < b.0) {
                    let hit = MepHit::RunMid {
                        run: *run,
                        system: *sys,
                        at: p3(p),
                    };
                    mid = Some((d, hit));
                }
            }
        }
        end.or(mid).map(|x| x.1)
    }
}

/// End of an auto-route.
#[derive(Clone, Debug, PartialEq)]
pub enum RouteEnd {
    Free { x: f64, y: f64 },
    Port { comp: i64, connector: String },
    Run { run: i64, at: (f64, f64, f64) },
}

/// Auto-route: an orthogonal polyline at elevation `z` with slope `slope`; system and section come
/// from the run or connection at the ends, otherwise from the fields.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteSpec {
    pub from: RouteEnd,
    pub to: RouteEnd,
    pub z: f64,
    pub slope: f64,
    pub level: i64,
    pub system: Option<i64>,
    pub template: Option<RunSpec>,
}

/// Library appliance: position and an optional connection to a system.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceSpec {
    pub family: String,
    pub ty: String,
    pub place: CompSpec,
    pub system: Option<i64>,
    pub flow: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placed {
    pub comp: i64,
    pub linked: Vec<String>,
    pub runs: Vec<i64>,
}

struct End {
    at: V3,
    dir: Option<V3>,
    port: Option<WorldPort>,
    run: Option<RunSpec>,
    z: Option<f64>,
}

impl Model {
    /// A compound action is one journal entry; an error rolls back everything done.
    pub(super) fn mep_group<T>(
        &mut self,
        kind: &str,
        f: impl FnOnce(&mut Model) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let head = |m: &Model| -> Result<i64, Error> {
            Ok(m.db.query_row("SELECT COALESCE(MAX(seq),0) FROM op", [], |r| r.get(0))?)
        };
        let before = head(self)?;
        match f(&mut *self) {
            Ok(v) => {
                merge_ops(&mut self.db, kind, before)?;
                Ok(v)
            }
            Err(e) => {
                while head(self)? > before {
                    self.undo()?;
                }
                Err(e)
            }
        }
    }

    /// Whether the system's network builds now: remembered before the action for `mep_valid`.
    pub(super) fn net_builds(&self, system: i64) -> bool {
        self.mep_network(Some(system)).is_ok()
    }

    /// The action did not break the network. An already broken network (import, an edit that
    /// bypassed the check) does not block work: the system check will report it.
    pub(super) fn mep_valid(&self, system: i64, was_ok: bool) -> Result<(), Error> {
        if was_ok {
            self.mep_network(Some(system)).map(|_| ())
        } else {
            Ok(())
        }
    }

    pub(super) fn world_port(&self, comp: i64, connector: &str) -> Result<WorldPort, Error> {
        self.component_ports(Some(comp))?
            .into_iter()
            .find(|p| p.name == connector)
            .ok_or_else(|| Error::Invalid(format!("component {comp} has no connector {connector}")))
    }

    fn system_kind(&self, system: i64) -> Result<SystemKind, Error> {
        self.mep_systems()?
            .into_iter()
            .find(|s| s.id == system)
            .map(|s| s.kind)
            .ok_or_else(|| Error::Invalid(format!("no such system {system}")))
    }

    /// Snapshot for picking in the plan.
    pub fn mep_snap_set(&self) -> Result<MepSnapSet, Error> {
        let mut runs = vec![];
        for id in self.mep_runs(None)? {
            let s = read_run(&self.db, id)?;
            runs.push((id, s.system, s.section.domain, s.pts()));
        }
        Ok(MepSnapSet {
            ports: self.component_ports(None)?,
            runs,
        })
    }

    /// Connections of a system (all when None).
    pub fn mep_port_links(&self, system: Option<i64>) -> Result<Vec<PortLink>, Error> {
        let mut st = self.db.prepare(
            "SELECT id,comp,connector,system,flow,stub FROM mep_port \
             WHERE ?1 IS NULL OR system=?1 ORDER BY comp, connector",
        )?;
        let r = st.query_map([system], link_of)?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Flow/power of a component's connections: (family value, whether it is a source).
    pub(super) fn port_defaults(&self, comp: i64) -> Result<HashMap<String, (f64, bool)>, Error> {
        let (fam, res, _) = self.component_built(comp, Lod::Coarse)?;
        let source = res.num("Source").is_some_and(|v| v > 0.5);
        Ok(fam
            .connectors
            .iter()
            .map(|c| {
                let electric = matches!(c.domain, Domain::Conduit | Domain::CableTray);
                let flow = res
                    .num(&format!("Flow{}", c.name))
                    .or_else(|| res.num("Flow"))
                    .or_else(|| if electric { res.num("Power") } else { None })
                    .unwrap_or(0.0);
                (c.name.clone(), (flow, source))
            })
            .collect())
    }

    /// Flow or power of a connection: the set value or the family value.
    pub fn mep_port_flow(&self, comp: i64, connector: &str) -> Result<f64, Error> {
        if let Some(f) = port_row(&self.db, comp, connector)?.and_then(|r| r.flow) {
            return Ok(f);
        }
        Ok(self
            .port_defaults(comp)?
            .get(connector)
            .map_or(0.0, |d| d.0))
    }

    /// Connection flow overriding the family value (None means from the family again).
    pub fn mep_set_port_flow(
        &mut self,
        comp: i64,
        connector: &str,
        flow: Option<f64>,
    ) -> Result<(), Error> {
        if flow.is_some_and(|f| !(f.is_finite() && f >= 0.0)) {
            return invalid("flow must be a non-negative number");
        }
        self.world_port(comp, connector)?;
        self.op("mep_port_flow", |db| {
            let inv = match port_row(db, comp, connector)? {
                Some(r) => {
                    let inv = Inv::default().restore(db, "mep_port", "id", r.id)?;
                    db.execute(
                        "UPDATE mep_port SET flow=?1 WHERE id=?2",
                        params![flow, r.id],
                    )?;
                    inv
                }
                None => {
                    db.execute(
                        "INSERT INTO mep_port(comp,connector,flow) VALUES (?1,?2,?3)",
                        params![comp, connector, flow],
                    )?;
                    Inv::default().delete("mep_port", "id", db.last_insert_rowid())
                }
            };
            Ok(((), comp, inv))
        })
    }

    /// A route in one action: the system by name (created if missing); connections at the ends
    /// are linked to it.
    pub fn mep_draw(&mut self, name: &str, kind: SystemKind, s: &RunSpec) -> Result<i64, Error> {
        self.mep_group("mep_draw", |m| {
            let found = m.mep_systems()?.into_iter().find(|x| x.name == name);
            let system = match found {
                Some(x) => x.id,
                None => m.add_mep_system(name, kind)?,
            };
            let was_ok = m.net_builds(system);
            let id = m.add_mep_run(&RunSpec {
                system,
                ..s.clone()
            })?;
            m.mep_valid(system, was_ok)?;
            m.link_touching(id, system)?;
            Ok(id)
        })
    }

    /// Connections touched by run ends join the run's system.
    fn link_touching(&mut self, run: i64, system: i64) -> Result<(), Error> {
        let s = read_run(&self.db, run)?;
        let pts = s.pts();
        let ends = [pts[0], pts[pts.len() - 1]];
        let kind = self.system_kind(system)?;
        for p in self.component_ports(None)? {
            let hit = ends.iter().any(|e| (*e - v(p.at)).len() < PORT_TOL);
            if hit && same_net(p.domain, s.section.domain) && accepts(&p, kind) {
                self.op("mep_port_link", |db| {
                    Ok(((), p.comp, link_port(db, p.comp, &p.name, system, None)?))
                })?;
            }
        }
        Ok(())
    }

    fn stub_spec(
        &self,
        system: i64,
        level: i64,
        port: &WorldPort,
        main: Option<&RunSpec>,
    ) -> Result<RunSpec, Error> {
        let mut spec = match port.domain {
            Domain::Pipe => {
                let m = main.and_then(|x| x.material).unwrap_or(PipeMaterial::Steel);
                let dn = (port.size * 1000.0).round() as u32;
                let size = m
                    .sizes()
                    .into_iter()
                    .find(|s| s.dn >= dn)
                    .ok_or_else(|| Error::Invalid("mep.err.no_size".into()))?;
                RunSpec::pipe(system, level, m, size.dn, vec![])?
            }
            Domain::Duct => {
                let mut sec = Section::new(Domain::Duct, Shape::Round { d: port.size });
                sec.wall = main.map_or(sec.wall, |x| x.section.wall);
                RunSpec::new(system, level, sec, vec![])
            }
            _ => RunSpec::new(
                system,
                level,
                Section::new(Domain::Conduit, Shape::Round { d: port.size }),
                vec![],
            ),
        };
        spec.section.insulation = main.map_or(0.0, |x| x.section.insulation);
        Ok(spec)
    }

    /// The first route variant with which the system's network builds.
    fn add_first_valid(&mut self, spec: &RunSpec, paths: Vec<Vec<V3>>) -> Result<i64, Error> {
        let mut last = Error::Invalid("mep.err.no_room".into());
        let was_ok = self.net_builds(spec.system);
        for pts in paths {
            if pts.len() < 2 {
                continue;
            }
            let s = RunSpec {
                points: pts.into_iter().map(p3).collect(),
                ..spec.clone()
            };
            if let Err(e) = check_spec(&self.db, &s) {
                last = e;
                continue;
            }
            let id = self.add_mep_run(&s)?;
            match self.mep_valid(s.system, was_ok) {
                Ok(()) => return Ok(id),
                Err(e) => {
                    last = e;
                    self.undo()?;
                }
            }
        }
        Err(last)
    }

    /// Connect: a feeder from the connection point to the nearest run of a suitable network (a tee
    /// at the tap), for cable conduits too. No system given: the one whose run is closer. No runs
    /// in the given system: only the link to it. Returns the new run.
    pub fn mep_attach(
        &mut self,
        comp: i64,
        connector: &str,
        system: Option<i64>,
    ) -> Result<Option<i64>, Error> {
        let port = self.world_port(comp, connector)?;
        self.mep_group("mep_connect", |m| m.attach_in(&port, system))
    }

    fn attach_in(&mut self, port: &WorldPort, system: Option<i64>) -> Result<Option<i64>, Error> {
        let row = port_row(&self.db, port.comp, &port.name)?;
        if row.as_ref().is_some_and(|r| r.stub.is_some()) {
            return invalid("mep.err.already_connected");
        }
        let kinds: HashMap<i64, SystemKind> = self
            .mep_systems()?
            .into_iter()
            .map(|s| (s.id, s.kind))
            .collect();
        if let Some(s) = system {
            let k = *kinds
                .get(&s)
                .ok_or_else(|| Error::Invalid(format!("no such system {s}")))?;
            if !accepts(port, k) {
                return invalid("mep.err.wrong_system");
            }
        }
        let p = v(port.at);
        let mut best: Option<(f64, RunSpec)> = None;
        for id in self.mep_runs(system)? {
            let s = read_run(&self.db, id)?;
            let fits = kinds.get(&s.system).is_some_and(|k| accepts(port, *k));
            if !fits || !same_net(s.section.domain, port.domain) {
                continue;
            }
            let (_, _, dist) = nearest_on(&s.pts(), p);
            if best.as_ref().is_none_or(|b| dist < b.0) {
                best = Some((dist, s));
            }
        }
        let comp = port.comp;
        let link = |m: &mut Model, sys: i64, stub: Option<i64>| {
            m.op("mep_port_link", |db| {
                Ok(((), comp, link_port(db, comp, &port.name, sys, stub)?))
            })
        };
        let Some((dist, main)) = best else {
            let Some(s) = system else {
                return invalid("mep.err.no_network");
            };
            link(&mut *self, s, None)?;
            return Ok(None);
        };
        if dist < PORT_TOL {
            if row.as_ref().is_some_and(|r| r.system == Some(main.system)) {
                return invalid("mep.err.already_connected");
            }
            // network nodes exist only at ends and taps: a link in the middle of a run is impossible
            let pts = main.pts();
            let on_end = (pts[0] - p).len() < PORT_TOL || (pts[pts.len() - 1] - p).len() < PORT_TOL;
            let on_node = on_end
                || self
                    .mep_network(Some(main.system))
                    .is_ok_and(|n| n.nodes.iter().any(|x| (v(x.at) - p).len() < PORT_TOL));
            if !on_node {
                return invalid("mep.err.port_on_run");
            }
            link(&mut *self, main.system, None)?;
            return Ok(None);
        }
        let level = self.component(comp)?.level;
        let mut spec = self.stub_spec(main.system, level, port, Some(&main))?;
        spec.level = main.level;
        let d = v(port.dir).normalized();
        let stub = (port.size * 3.0).max(0.1);
        let fall = if kinds.get(&main.system) == Some(&SystemKind::Sanitary) {
            WASTE_FALL
        } else {
            0.0
        };
        let electric = matches!(port.domain, Domain::Conduit | Domain::CableTray);
        let paths = attach_paths(p, d, stub, &main.pts(), fall, !electric);
        let id = self.add_first_valid(&spec, paths)?;
        link(&mut *self, main.system, Some(id))?;
        Ok(Some(id))
    }

    /// Disconnect a component: the feeder is deleted; the end of a foreign run is moved away from
    /// the connection. A row with a set flow stays without a system.
    pub fn mep_detach(&mut self, comp: i64, connector: &str) -> Result<(), Error> {
        let port = self.world_port(comp, connector)?;
        self.mep_group("mep_disconnect", |m| {
            let row = port_row(&m.db, comp, connector)?;
            let mut done = false;
            let stub = row.as_ref().and_then(|r| r.stub);
            // the appliance may have been moved: the feeder is removed even without touching. A branch
            // tapped into it is not detached; then the feeder only moves away from the connection
            if let Some(id) = stub {
                if stub_is_leaf(&m.db, id)? {
                    m.delete_mep_run(id)?;
                    done = true;
                }
            }
            let touches = |s: &RunSpec| -> Option<u8> {
                let pts = s.pts();
                if (pts[0] - v(port.at)).len() < PORT_TOL {
                    Some(0)
                } else if (pts[pts.len() - 1] - v(port.at)).len() < PORT_TOL {
                    Some(1)
                } else {
                    None
                }
            };
            for id in m.mep_runs(None)? {
                let s = read_run(&m.db, id)?;
                if !same_net(s.section.domain, port.domain) {
                    continue;
                }
                if let Some(end) = touches(&s) {
                    m.pull_back(id, s, end)?;
                    done = true;
                }
            }
            if let Some(r) = row {
                done |= r.system.is_some();
                m.op("mep_port_unlink", |db| {
                    let inv = Inv::default().restore(db, "mep_port", "id", r.id)?;
                    if r.flow.is_some() {
                        db.execute(
                            "UPDATE mep_port SET system=NULL, stub=NULL WHERE id=?1",
                            [r.id],
                        )?;
                    } else {
                        db.execute("DELETE FROM mep_port WHERE id=?1", [r.id])?;
                    }
                    Ok(((), comp, inv))
                })?;
            }
            if !done {
                return invalid("mep.err.not_connected");
            }
            Ok(())
        })
    }

    fn pull_back(&mut self, id: i64, mut s: RunSpec, end: u8) -> Result<(), Error> {
        let mut pts = s.pts();
        let (i, j) = if end == 0 {
            (0, 1)
        } else {
            (pts.len() - 1, pts.len() - 2)
        };
        let leg = pts[j] - pts[i];
        let by = PULL_BACK.min(leg.len() / 3.0);
        pts[i] = pts[i] + leg.normalized() * by;
        s.points = pts.into_iter().map(p3).collect();
        self.update_mep_run(id, &s)
    }

    /// Detach a run end: the end moves away from the node and becomes open.
    pub fn mep_detach_end(&mut self, run: i64, end: u8) -> Result<(), Error> {
        let s = read_run(&self.db, run)?;
        self.mep_group("mep_disconnect", |m| m.pull_back(run, s, end))
    }

    /// Extend a run end to the nearest other run of the system (or a given one).
    pub fn mep_connect_end(&mut self, run: i64, end: u8, target: Option<i64>) -> Result<(), Error> {
        let spec = read_run(&self.db, run)?;
        let pts = spec.pts();
        let e = if end == 0 { pts[0] } else { pts[pts.len() - 1] };
        let mut best: Option<(f64, Vec<V3>)> = None;
        for id in self.mep_runs(Some(spec.system))? {
            if id == run || target.is_some_and(|t| t != id) {
                continue;
            }
            let o = read_run(&self.db, id)?;
            if !same_net(o.section.domain, spec.section.domain) {
                continue;
            }
            let op = o.pts();
            let (_, _, dist) = nearest_on(&op, e);
            if best.as_ref().is_none_or(|b| dist < b.0) {
                best = Some((dist, op));
            }
        }
        let Some((_, main)) = best else {
            return invalid("mep.err.no_other_run");
        };
        let q0 = nearest_on(&main, e).0;
        let b = v3(e.x, e.y, q0.z);
        let q = nearest_on(&main, b).0;
        self.mep_group("mep_connect", |m| {
            let was_ok = m.net_builds(spec.system);
            for tail in [vec![b, q], vec![q]] {
                let mut np = pts.clone();
                if end == 0 {
                    for t in tail {
                        np.insert(0, t);
                    }
                } else {
                    np.extend(tail);
                }
                let s = RunSpec {
                    points: clean(&np).into_iter().map(p3).collect(),
                    slope: 0.0,
                    ..spec.clone()
                };
                if check_spec(&m.db, &s).is_err() {
                    continue;
                }
                m.update_mep_run(run, &s)?;
                if m.mep_valid(s.system, was_ok).is_ok() {
                    return Ok(());
                }
                m.undo()?;
            }
            invalid("mep.err.no_room")
        })
    }

    /// Cap at a run end.
    pub fn mep_cap_end(&mut self, run: i64, end: u8, on: bool) -> Result<(), Error> {
        let mut s = read_run(&self.db, run)?;
        if end == 0 {
            s.caps.0 = on;
        } else {
            s.caps.1 = on;
        }
        self.mep_group("mep_cap", |m| {
            let was_ok = m.net_builds(s.system);
            m.update_mep_run(run, &s)?;
            m.mep_valid(s.system, was_ok)
        })
    }

    /// Insulation thickness of all runs of a system in one action.
    pub fn mep_set_system_insulation(&mut self, system: i64, t: f64) -> Result<(), Error> {
        if !(t.is_finite() && t >= 0.0) {
            return invalid("insulation must be a non-negative length");
        }
        self.op("mep_insulation", |db| {
            let inv = Inv::default().restore(db, "mep_run", "system", system)?;
            db.execute(
                "UPDATE mep_run SET insulation=?1 WHERE system=?2",
                params![t, system],
            )?;
            Ok(((), system, inv))
        })
    }

    /// Lines of the duct itself under the insulation: `mep_plan` already draws the insulation outline.
    pub fn mep_plan_insulation(
        &self,
        level: i64,
        detail: Detail,
    ) -> Result<Vec<MepPlanPrim>, Error> {
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
        // the plan is built on every revision while insulation is rare: avoid an unneeded network graph
        let any: i64 = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM mep_run WHERE insulation>0)",
            [],
            |r| r.get(0),
        )?;
        if any == 0 {
            return Ok(vec![]);
        }
        let net = self.mep_network(None)?;
        let colors = self.run_colors()?;
        let mut out = vec![];
        for (run, p, s) in &net.pieces {
            let (a, b) = p.ends();
            let (z0, z1) = (a.z.min(b.z), a.z.max(b.z));
            if s.insulation <= 0.0 || z1 < el - 1e-9 || z0 >= top {
                continue;
            }
            for prim in mep::plan(s.outer(), p, detail, 24) {
                if matches!(prim, PlanPrim::Edge(_)) {
                    out.push(MepPlanPrim {
                        run: *run,
                        color: colors[run],
                        prim,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Place a library appliance and connect all suitable connections to the system.
    pub fn mep_place(&mut self, d: &DeviceSpec) -> Result<Placed, Error> {
        self.mep_group("mep_place", |m| {
            let fid = m.load_library_family(&d.family)?;
            let ctype = m
                .comp_type_id(fid, &d.ty)?
                .ok_or_else(|| Error::Invalid(format!("no type {}", d.ty)))?;
            let comp = m.add_component(&CompSpec {
                ctype,
                ..d.place.clone()
            })?;
            let mut out = Placed {
                comp,
                ..Placed::default()
            };
            if m.regen.held {
                // derived tables wait for the one recompute of an Apply; the ports need the
                // placement of this instance now (the later recompute writes the same row)
                crate::components::regenerate_ids(&m.db, &[comp].into())?;
            }
            let ports = m.component_ports(Some(comp))?;
            if let Some(flow) = d.flow {
                for p in &ports {
                    m.mep_set_port_flow(comp, &p.name, Some(flow))?;
                }
            }
            if let Some(sys) = d.system {
                let kind = m.system_kind(sys)?;
                for p in ports.iter().filter(|p| accepts(p, kind)) {
                    if let Some(r) = m.attach_in(p, Some(sys))? {
                        out.runs.push(r);
                    }
                    out.linked.push(p.name.clone());
                }
            }
            Ok(out)
        })
    }

    fn route_end(&self, e: &RouteEnd) -> Result<End, Error> {
        Ok(match e {
            RouteEnd::Free { x, y } => End {
                at: v3(*x, *y, 0.0),
                dir: None,
                port: None,
                run: None,
                z: None,
            },
            RouteEnd::Port { comp, connector } => {
                let p = self.world_port(*comp, connector)?;
                let d = v(p.dir).normalized();
                End {
                    at: v(p.at),
                    dir: (d.z.abs() <= 0.7).then_some(d),
                    z: Some(p.at.2),
                    port: Some(p),
                    run: None,
                }
            }
            RouteEnd::Run { run, at } => End {
                at: v(*at),
                dir: None,
                port: None,
                run: Some(read_run(&self.db, *run)?),
                z: Some(at.2),
            },
        })
    }

    /// Auto-route between two ends: orthogonal at an elevation and with the spec's slope; ports at
    /// the ends are connected to the route's system.
    pub fn mep_route(&mut self, r: &RouteSpec) -> Result<i64, Error> {
        if !(r.z.is_finite() && r.slope.is_finite() && r.slope >= 0.0) {
            return invalid("mep.err.bad_route");
        }
        let (a, b) = (self.route_end(&r.from)?, self.route_end(&r.to)?);
        let from_run = a.run.as_ref().or(b.run.as_ref());
        let system = from_run
            .map(|s| s.system)
            .or(r.system)
            .ok_or_else(|| Error::Invalid("mep.err.no_system".into()))?;
        for x in [&a, &b] {
            if x.run.as_ref().is_some_and(|s| s.system != system) {
                return invalid("mep.err.two_systems");
            }
        }
        let port = a.port.as_ref().or(b.port.as_ref());
        let mut spec = match (from_run, port, &r.template) {
            (Some(s), _, _) => s.clone(),
            (None, Some(p), t) => self.stub_spec(system, r.level, p, t.as_ref())?,
            (None, None, Some(t)) => t.clone(),
            _ => return invalid("mep.err.no_section"),
        };
        let kind = self.system_kind(system)?;
        for p in [&a, &b].into_iter().filter_map(|x| x.port.as_ref()) {
            if !accepts(p, kind) || !same_net(p.domain, spec.section.domain) {
                return invalid("mep.err.wrong_system");
            }
        }
        spec.system = system;
        spec.level = r.level;
        spec.slope = 0.0;
        spec.caps = (false, false);
        spec.points.clear();
        let min_leg = spec.min_leg();
        // the departure fits an elbow; otherwise a horizontal port always yields a too-short segment
        let stub = min_leg.max(0.15);
        let off = |e: &End| e.dir.map_or(e.at, |d| e.at + d * stub);
        let (a1, b1) = (off(&a), off(&b));
        let mut paths = vec![];
        for corner in [(b1.x, a1.y), (a1.x, b1.y)] {
            let plan = plan_clean(&[(a1.x, a1.y), corner, (b1.x, b1.y)]);
            let mid = routed_points(&plan, r.z, r.slope, a.z, b.z, min_leg);
            let mut pts: Vec<V3> = vec![];
            if a.dir.is_some() {
                pts.push(a.at);
            }
            pts.extend(mid.into_iter().map(v));
            if b.dir.is_some() {
                pts.push(b.at);
            }
            paths.push(clean(&pts));
        }
        // the leg that would lie along the target run goes last
        if let RouteEnd::Run { run, at } = &r.to {
            let along = read_run(&self.db, *run)?.pts();
            let (_, k, _) = nearest_on(&along, v(*at));
            let d = (along[k + 1] - along[k]).normalized();
            let overlaps = |p: &Vec<V3>| {
                p.len() >= 2 && {
                    let l = (p[p.len() - 1] - p[p.len() - 2]).normalized();
                    l.dot(d).abs() > 0.999
                }
            };
            paths.sort_by_key(overlaps);
        }
        self.mep_group("mep_route", |m| {
            let id = m.add_first_valid(&spec, paths)?;
            // the route is an independent run, not a feeder: "disconnect" only moves it away
            for e in [&a, &b] {
                if let Some(p) = &e.port {
                    m.op("mep_port_link", |db| {
                        Ok(((), p.comp, link_port(db, p.comp, &p.name, system, None)?))
                    })?;
                }
            }
            Ok(id)
        })
    }
}

fn plan_clean(p: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let pts: Vec<V3> = p.iter().map(|q| v3(q.0, q.1, 0.0)).collect();
    clean(&pts).into_iter().map(|q| (q.x, q.y)).collect()
}

/// Feeder variants: from the connection (a horizontal one with departure `stub`), a vertical to
/// the main's elevation, a horizontal to it; `fall` is the slope of the horizontal toward the main
/// (fraction). `strict` excludes variants leaving a horizontal connection backwards, through the
/// appliance body.
pub(super) fn attach_paths(
    p: V3,
    d: V3,
    stub: f64,
    main: &[V3],
    fall: f64,
    strict: bool,
) -> Vec<Vec<V3>> {
    let a = if d.z.abs() > 0.7 { p } else { p + d * stub };
    let q0 = nearest_on(main, a).0;
    let q = nearest_on(main, v3(a.x, a.y, q0.z)).0;
    let b = v3(a.x, a.y, q.z + fall * plan_len(a, q));
    let mut paths = vec![clean(&[p, a, b, q]), clean(&[p, a, q]), clean(&[p, q])];
    if strict && d.z.abs() <= 0.7 {
        paths.retain(|x| x.len() < 2 || (x[1] - p).normalized().dot(d) >= -0.1);
    }
    paths
}
