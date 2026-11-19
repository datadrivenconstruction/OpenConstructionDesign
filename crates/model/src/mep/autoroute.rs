// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Network planning: a request names a system, a source (a port, a new riser, a run) and target
//! connectors; the planner routes a tree on the 3D lattice of `route3d` against the structure
//! (walls crossed with sleeves, slabs only vertically with holes, columns, beams, doors and
//! windows never) and the runs already there, sizes every piece by the flow downstream, gives
//! gravity drains their fall and yields a `MepBuild` with metrics. Nothing is written: the
//! caller applies the build (one undo step) or shows the variants.

use std::collections::HashMap;

use ddc_geom::math::{v2, v3, V2, V3};
use ddc_geom::mep::clash;
use ddc_geom::mep::route3d::{
    self, Index, Obstacle, Pass, Routed, RoutedIndex, Rules, Source, Terminal, Tree,
};
use ddc_geom::mep::{Domain, PipeMaterial, Section, Shape, SystemKind};

use super::build::{MepBuild, SysRef};
use super::duct_sizing;
use super::circuit::{CircuitDevice, CircuitKind, CircuitSpec};
use super::riser_join::{join_riser, stack_of, Join, Stack};
use super::{arc_len, arc_points, invalid, read_run, RunSpec, WorldPort};
use crate::{Error, Model, OpeningSpec};

/// Number of route variants: shortest, fewest fittings, along the grid axes.
pub const VARIANTS: usize = 3;
/// Gap kept to structure and other runs, m.
const GAP: f64 = 0.05;

/// Routing numbers kept as data: design velocities, conduit sizes, the cost weights of each
/// variant and the conduit overrides.
#[derive(serde::Deserialize)]
struct RouteTable {
    /// Design velocity of water, m/s (the check's limit is 1.5).
    water_velocity_m_s: f64,
    air_velocity_m_s: f64,
    max_states: usize,
    /// A riser of the same system this near in plan is the same column, m (one per shaft).
    riser_join_m: f64,
    /// (largest cable count, conduit diameter mm); 0 takes everything above.
    conduit_mm: Vec<(usize, f64)>,
    variants: Vec<VariantCost>,
    power: PowerCost,
}

#[derive(serde::Deserialize)]
struct VariantCost {
    bend_cost: f64,
    axis_gain: Option<f64>,
    wall_cost: Option<f64>,
}

#[derive(serde::Deserialize)]
struct PowerCost {
    in_wall: f64,
    wall_cost: f64,
}

fn route_table() -> &'static RouteTable {
    static T: std::sync::OnceLock<RouteTable> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/route_rules.json"))
            .expect("route_rules.json is valid")
    })
}
/// Simultaneity of drains: Q = K * sqrt(sum of fixture flows), l/s.
const DRAIN_K: f64 = 0.5;
const MANNING_N: f64 = 0.013;
const ROUND_DUCT_MM: [u32; 12] = duct_sizing::ROUND_MM;
/// Devices per circuit at most.
const PER_CIRCUIT: usize = 8;

/// Where a network starts.
#[derive(Clone, Debug, PartialEq)]
pub enum RouteFrom {
    /// A component connector (a panel, an air handling unit, a boiler).
    Port { comp: i64, connector: String },
    /// A new riser at this plan point, through the level.
    Riser { x: f64, y: f64 },
    /// A run already in the model: branches tee into it.
    Run(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetReq {
    /// System name: an existing system of that name is extended.
    pub name: String,
    pub kind: SystemKind,
    pub level: i64,
    pub from: RouteFrom,
    /// Target connectors: (component, connector).
    pub to: Vec<(i64, String)>,
    /// Rectangular ducts instead of round ones.
    pub rect: bool,
    /// Fixed size, mm: DN of pipes, diameter or width of ducts, diameter of conduits.
    pub size: Option<f64>,
    /// Fall of gravity drains, %.
    pub slope: f64,
    /// Which of the `VARIANTS`.
    pub variant: usize,
    /// Air ducts: equal-friction rate, Pa/m (None: the fixed velocity of the rules).
    pub friction: Option<f64>,
    /// Air ducts: velocity cap of equal-friction sizing, m/s.
    pub v_max: f64,
    /// Air ducts: thickness of insulation, m; it widens the clearance kept to everything.
    pub insulation: f64,
    /// Ceiling plenum (top of the suspended ceiling, underside of beams/slab), absolute z, m:
    /// horizontal runs stay inside it, terminals below are reached by drops.
    pub plenum: Option<(f64, f64)>,
}

impl NetReq {
    pub fn new(name: &str, kind: SystemKind, level: i64, from: RouteFrom) -> Self {
        NetReq {
            name: name.to_string(),
            kind,
            level,
            from,
            to: vec![],
            rect: false,
            size: None,
            slope: 2.0,
            variant: 0,
            friction: None,
            v_max: 8.0,
            insulation: 0.0,
            plenum: None,
        }
    }
}

/// What a planned network costs and how it sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RouteMetrics {
    pub variant: usize,
    pub runs: usize,
    pub length: f64,
    pub elbows: usize,
    pub tees: usize,
    pub sleeves: usize,
    pub holes: usize,
    /// Pieces that come closer to a column, beam, door or window than their size allows.
    pub clashes: usize,
    pub z_min: f64,
    pub z_max: f64,
    /// Least fall of a drain piece, %.
    pub min_slope: Option<f64>,
    /// Targets no route reached.
    pub failed: Vec<(i64, String)>,
    /// Air ducts: friction and elbow loss of the worst path from the source to a terminal, Pa.
    pub critical_pa: Option<f64>,
    /// The terminal at the end of that path.
    pub critical_to: Option<(i64, String)>,
}

impl RouteMetrics {
    fn add(&mut self, o: &RouteMetrics) {
        let first = self.runs == 0;
        self.runs += o.runs;
        self.length += o.length;
        self.elbows += o.elbows;
        self.tees += o.tees;
        self.clashes += o.clashes;
        self.z_min = if first { o.z_min } else { self.z_min.min(o.z_min) };
        self.z_max = if first { o.z_max } else { self.z_max.max(o.z_max) };
        self.min_slope = match (self.min_slope, o.min_slope) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.failed.extend(o.failed.iter().cloned());
        if o.critical_pa > self.critical_pa {
            self.critical_pa = o.critical_pa;
            self.critical_to = o.critical_to.clone();
        }
    }
    /// Lower is better: metres plus fittings and crossings in equivalent metres.
    pub fn score(&self) -> f64 {
        self.length
            + 1.0 * (self.elbows + self.tees) as f64
            + 2.0 * (self.sleeves + self.holes) as f64
            + 1000.0 * (self.failed.len() + self.clashes) as f64
    }
}

/// A planned network: the build to apply and its metrics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Planned {
    pub build: MepBuild,
    pub metrics: RouteMetrics,
}

#[derive(Clone)]
struct WallInfo {
    id: i64,
    /// Chords: start, end, arc length at the start.
    chords: Vec<(V2, V2, f64)>,
    t: f64,
    off: f64,
    z0: f64,
    h: f64,
}

#[derive(Clone)]
struct SlabInfo {
    id: i64,
    level: i64,
    ring: Vec<V2>,
    holes: Vec<(V2, V2)>,
    z0: f64,
    z1: f64,
}

/// The structure and the runs around a plan, read once.
#[derive(Clone)]
struct Site {
    obstacles: Vec<Obstacle>,
    walls: Vec<WallInfo>,
    slabs: Vec<SlabInfo>,
    /// Segments of runs in the model and of runs planned so far.
    others: Vec<Routed>,
    /// Risers by system name: in the model and planned so far.
    stacks: Vec<Stack>,
    levels: Vec<(i64, f64)>,
    ports: HashMap<(i64, String), WorldPort>,
    axes: (Vec<f64>, Vec<f64>),
    lo: V2,
    hi: V2,
}

fn pv(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

fn tp(p: V3) -> (f64, f64, f64) {
    (p.x, p.y, p.z)
}

/// Half the envelope across, for clearances and sleeves.
fn half(s: &Section) -> f64 {
    match s.envelope() {
        Shape::Round { d } => d / 2.0,
        Shape::Rect { w, h } | Shape::Oval { w, h } => w.hypot(h) / 2.0,
    }
}

fn material_of(kind: SystemKind) -> PipeMaterial {
    match kind {
        SystemKind::ColdWater | SystemKind::HotWater | SystemKind::MedicalGas => PipeMaterial::Copper,
        SystemKind::Sanitary => PipeMaterial::Pvc,
        _ => PipeMaterial::Steel,
    }
}

fn domain_of(kind: SystemKind) -> Domain {
    match kind {
        SystemKind::Power => Domain::Conduit,
        k => k.domains()[0],
    }
}

/// Distance of the kind's layer below the ceiling limit, m: networks stack instead of meeting.
fn layer_drop(kind: SystemKind) -> f64 {
    match kind {
        SystemKind::Power => 0.0,
        SystemKind::Sprinkler => 0.1,
        SystemKind::ColdWater => 0.2,
        SystemKind::HotWater => 0.3,
        SystemKind::Heating => 0.4,
        SystemKind::ChilledWater => 0.45,
        SystemKind::Sanitary => 0.0,
        SystemKind::SupplyAir => 0.5,
        SystemKind::ReturnAir => 0.5,
        SystemKind::ExhaustAir => 0.5,
        SystemKind::MedicalGas => 0.55,
    }
}

/// Half-full capacity of a drain (Manning), l/s.
fn drain_capacity(d: f64, pct: f64) -> f64 {
    let a = std::f64::consts::PI * d * d / 8.0;
    a * (d / 4.0).powf(2.0 / 3.0) * (pct / 100.0).sqrt() / MANNING_N * 1000.0
}

/// What flows downstream of a piece: summed flow, largest single flow, largest port, devices.
#[derive(Clone, Copy, Debug, Default)]
struct Load {
    flow: f64,
    peak: f64,
    port: f64,
    count: usize,
}

impl Load {
    fn plus(self, o: Load) -> Load {
        Load {
            flow: self.flow + o.flow,
            peak: self.peak.max(o.peak),
            port: self.port.max(o.port),
            count: self.count + o.count,
        }
    }
}

/// Friction loss, Pa/m, of `q` m3/h in a duct section.
fn duct_friction(sec: &Section, q: f64) -> (f64, f64) {
    match sec.shape {
        Shape::Round { d } => (
            duct_sizing::friction_round(q, d),
            duct_sizing::velocity(q, std::f64::consts::PI * d * d / 4.0),
        ),
        Shape::Rect { w, h } | Shape::Oval { w, h } => {
            (duct_sizing::friction_rect(q, w, h), duct_sizing::velocity(q, w * h))
        }
    }
}

/// Worst source-to-terminal loss of a routed air tree, Pa, and the branch it ends on.
/// Branch points run from the terminal (0) to the join (length); `pieces[i]` holds the cuts
/// and the load between them.
fn critical(
    req: &NetReq,
    tree: &Tree,
    kids: &[Vec<(f64, usize)>],
    pieces: &[(Vec<f64>, Vec<Load>)],
) -> Result<Option<(f64, usize)>, Error> {
    let n = tree.branches.len();
    // loss along branch i from parameter s up to its join
    let from = |i: usize, s: f64| -> Result<f64, Error> {
        let (cuts, loads) = &pieces[i];
        let pts = &tree.branches[i].points;
        let mut sum = 0.0;
        for k in 0..loads.len() {
            let (a, b) = (cuts[k].max(s), cuts[k + 1]);
            if b <= a {
                continue;
            }
            let sec = size_for(req, loads[k])?.0;
            sum += duct_friction(&sec, loads[k].flow).0 * (b - a);
        }
        let mut at = 0.0;
        for w in pts.windows(3) {
            at += (w[1] - w[0]).len();
            let (u, v) = ((w[1] - w[0]).normalized(), (w[2] - w[1]).normalized());
            if at + 1e-9 < s || u.cross(v).len() < 1e-6 {
                continue;
            }
            let k = cuts.iter().rposition(|c| *c <= at + 1e-9).unwrap_or(0).min(loads.len() - 1);
            let sec = size_for(req, loads[k])?.0;
            let vel = duct_friction(&sec, loads[k].flow).1;
            sum += duct_sizing::ZETA_ELBOW * duct_sizing::dynamic(vel);
        }
        Ok(sum)
    };
    let mut join_at = vec![(None, 0.0); n];
    for (p, k) in kids.iter().enumerate() {
        for &(at, c) in k {
            join_at[c] = (Some(p), at);
        }
    }
    let mut best: Option<(f64, usize)> = None;
    for i in 0..n {
        if tree.failed.contains(&tree.branches[i].target) {
            continue;
        }
        let mut sum = from(i, 0.0)?;
        let mut cur = i;
        let mut guard = 0;
        while let (Some(p), at) = join_at[cur] {
            sum += from(p, at)?;
            cur = p;
            guard += 1;
            if guard > n {
                break;
            }
        }
        if best.is_none_or(|b| sum > b.0) {
            best = Some((sum, i));
        }
    }
    Ok(best)
}

/// The section of a piece carrying `l`.
fn size_for(req: &NetReq, l: Load) -> Result<(Section, Option<PipeMaterial>, Option<u32>), Error> {
    let domain = domain_of(req.kind);
    match domain {
        Domain::Pipe => {
            let m = material_of(req.kind);
            let sizes = m.sizes();
            let fixed = req.size.map(|s| s.round() as u32);
            let min_dn = (l.port * 1000.0 - 1e-6).max(0.0);
            let pick = sizes.iter().find(|x| match fixed {
                Some(dn) => x.dn >= dn,
                None => {
                    let d = x.id();
                    let ok = if req.kind == SystemKind::Sanitary {
                        let q = (DRAIN_K * l.flow.sqrt()).max(l.peak);
                        drain_capacity(d, req.slope.max(0.5)) >= q
                    } else {
                        std::f64::consts::PI * d * d / 4.0 * route_table().water_velocity_m_s * 1000.0 >= l.flow
                    };
                    ok && x.dn as f64 >= min_dn
                }
            });
            let x = pick.or(sizes.last()).copied().ok_or_else(|| {
                Error::Invalid(format!("no sizes in {} catalog", m.name()))
            })?;
            let mut sec = Section::new(Domain::Pipe, Shape::Round { d: x.od });
            sec.wall = x.wall;
            Ok((sec, Some(m), Some(x.dn)))
        }
        Domain::Duct => {
            let area = l.flow / 3600.0 / route_table().air_velocity_m_s;
            let shape = match (req.rect, req.size, req.friction) {
                (false, Some(d), _) => Shape::Round { d: d / 1000.0 },
                (false, None, Some(f)) => Shape::Round {
                    d: duct_sizing::pick_round(l.flow, f, req.v_max, l.port, &ROUND_DUCT_MM),
                },
                (true, None, Some(f)) => {
                    let h_max = req.plenum.map_or(0.4, |(lo, hi)| {
                        (hi - lo - 2.0 * (GAP + req.insulation)).clamp(0.1, 0.4)
                    });
                    let (w, h) = duct_sizing::pick_rect(l.flow, f, req.v_max, h_max, 0.1);
                    Shape::Rect { w: w.max(l.port), h }
                }
                (false, None, None) => {
                    let d = ROUND_DUCT_MM
                        .iter()
                        .map(|d| *d as f64 / 1000.0)
                        .find(|d| std::f64::consts::PI * d * d / 4.0 >= area && *d >= l.port)
                        .unwrap_or(1.0);
                    Shape::Round { d }
                }
                (true, size, _) => {
                    let h = 0.2f64.max(l.port.min(0.4));
                    let w = size.map(|s| s / 1000.0).unwrap_or_else(|| {
                        ((area / h / 0.05).ceil() * 0.05).max(0.1).max(l.port)
                    });
                    Shape::Rect { w, h }
                }
            };
            let mut sec = Section::new(Domain::Duct, shape);
            sec.wall = 0.0008;
            sec.insulation = req.insulation.max(0.0);
            Ok((sec, None, None))
        }
        _ => {
            let d = req.size.map(|s| s / 1000.0).unwrap_or_else(|| {
                route_table()
                    .conduit_mm
                    .iter()
                    .find(|(n, _)| *n == 0 || l.count <= *n)
                    .map_or(0.05, |x| x.1 / 1000.0)
            });
            Ok((Section::new(Domain::Conduit, Shape::Round { d: d.max(l.port) }), None, None))
        }
    }
}

fn spec_of(level: i64, sec: (Section, Option<PipeMaterial>, Option<u32>), pts: &[V3]) -> RunSpec {
    let mut r = RunSpec::new(0, level, sec.0, pts.iter().map(|p| tp(*p)).collect());
    r.material = sec.1;
    r.dn = sec.2;
    r
}

/// Arc length of the point `q` on the polyline.
fn param_on(p: &[V3], q: V3) -> f64 {
    let (mut best, mut at, mut acc) = (f64::INFINITY, 0.0, 0.0);
    for w in p.windows(2) {
        let d = w[1] - w[0];
        let l = d.len();
        let t = if l > 1e-12 {
            ((q - w[0]).dot(d) / (l * l)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let e = (w[0] + d * t - q).len();
        if e < best {
            best = e;
            at = acc + t * l;
        }
        acc += l;
    }
    at
}

/// The polyline between arc lengths s0 and s1.
fn slice(p: &[V3], s0: f64, s1: f64) -> Vec<V3> {
    let mut out = vec![];
    let mut acc = 0.0;
    for w in p.windows(2) {
        let l = (w[1] - w[0]).len();
        let at = |s: f64| w[0] + (w[1] - w[0]) * ((s - acc) / l).clamp(0.0, 1.0);
        if acc + l >= s0 - 1e-9 && acc <= s1 + 1e-9 && l > 1e-12 {
            let a = at(s0.max(acc));
            let b = at(s1.min(acc + l));
            if out.last().is_none_or(|q: &V3| (*q - a).len() > 1e-9) {
                out.push(a);
            }
            if (b - a).len() > 1e-9 {
                out.push(b);
            }
        }
        acc += l;
    }
    route3d::clean(&out)
}

impl Model {
    fn read_site(&self) -> Result<Site, Error> {
        let mut obstacles = vec![];
        let mut walls = vec![];
        let mut lo = v2(f64::INFINITY, f64::INFINITY);
        let mut hi = v2(f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut grow = |p: V2| {
            lo = v2(lo.x.min(p.x), lo.y.min(p.y));
            hi = v2(hi.x.max(p.x), hi.y.max(p.y));
        };
        let mut opens: HashMap<i64, Vec<([f64; 4], String)>> = HashMap::new();
        {
            let mut so = self
                .db
                .prepare("SELECT wall, along, width, sill, height, kind FROM opening")?;
            let r = so.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                    r.get::<_, String>(5)?,
                ))
            })?;
            for x in r {
                let (w, o, k) = x?;
                opens.entry(w).or_default().push((o, k));
            }
        }
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
        for x in rows {
            let (id, [ax, ay, bx, by], (z0, h), off0, bulge, t) = x?;
            let off = off0.map(|o| o - t / 2.0).unwrap_or(0.0);
            let (a, b) = (v2(ax, ay), v2(bx, by));
            let pts = arc_points(a, b, bulge);
            let seg = arc_len(a, b, bulge) / (pts.len() - 1).max(1) as f64;
            let mut chords = vec![];
            for (k, w) in pts.windows(2).enumerate() {
                let fp = clash::wall_footprint(w[0], w[1], t, off);
                if fp.len() < 3 || !(seg > 1e-9) {
                    continue;
                }
                fp.iter().for_each(|p| grow(*p));
                let s0 = seg * k as f64;
                chords.push((w[0], w[1], s0));
                obstacles.push(Obstacle {
                    ring: fp,
                    z0,
                    z1: z0 + h,
                    pass: Pass::Wall,
                    tag: id,
                });
                // doors and windows are never crossed
                for ([along, width, sill, oh], kind) in opens.get(&id).into_iter().flatten() {
                    if kind == "void" {
                        continue;
                    }
                    let (u0, u1) = (along.max(s0), (along + width).min(s0 + seg));
                    if u1 - u0 > 1e-9 {
                        let at = |u: f64| w[0] + (w[1] - w[0]) * ((u - s0) / seg);
                        let ofp = clash::wall_footprint(at(u0), at(u1), t + 0.4, off - 0.2);
                        obstacles.push(Obstacle {
                            ring: ofp,
                            z0: z0 + sill,
                            z1: z0 + sill + oh,
                            pass: Pass::Block,
                            tag: id,
                        });
                    }
                }
            }
            walls.push(WallInfo {
                id,
                chords,
                t,
                off,
                z0,
                h,
            });
        }
        let mut slabs = vec![];
        let rows: Vec<(i64, i64, f64, f64)> = self
            .db
            .prepare(
                "SELECT s.id, s.level, l.elevation + s.offset, \
                 (SELECT SUM(thickness) FROM slab_type_layer WHERE stype=s.stype) \
                 FROM slab s JOIN level l ON l.id=s.level ORDER BY s.id",
            )?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, level, top, t) in rows {
            let rings = self.slab_rings(id)?;
            let Some(outer) = rings.first() else { continue };
            let ring: Vec<V2> = outer.iter().map(|p| v2(p.0, p.1)).collect();
            let bb = |r: &[V2]| {
                r.iter().fold(
                    (v2(f64::INFINITY, f64::INFINITY), v2(f64::NEG_INFINITY, f64::NEG_INFINITY)),
                    |(a, b), p| (v2(a.x.min(p.x), a.y.min(p.y)), v2(b.x.max(p.x), b.y.max(p.y))),
                )
            };
            let (l, h) = bb(&ring);
            grow(l);
            grow(h);
            let holes = rings[1..]
                .iter()
                .map(|r| bb(&r.iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>()))
                .collect();
            // a concave outline counts by its box: runs only cross slabs vertically anyway
            let (lo, hi) = (v3(l.x, l.y, top - t), v3(h.x, h.y, top));
            obstacles.push(Obstacle::aabb(lo, hi, Pass::Slab, id));
            slabs.push(SlabInfo {
                id,
                level,
                ring,
                holes,
                z0: top - t,
                z1: top,
            });
        }
        for (_, id, _, m) in self.member_meshes()? {
            let b = clash::Aabb::of(m.positions.iter().copied());
            obstacles.push(Obstacle::aabb(b.min, b.max, Pass::Block, id));
        }
        let mut levels: Vec<(i64, f64)> =
            self.level_list()?.into_iter().map(|l| (l.0, l.2)).collect();
        levels.sort_by(|a, b| a.1.total_cmp(&b.1));
        let zs: Vec<f64> = levels.iter().map(|l| l.1).collect();
        let names: HashMap<i64, String> =
            self.mep_systems()?.into_iter().map(|s| (s.id, s.name)).collect();
        let (mut others, mut stacks) = (vec![], vec![]);
        for id in self.mep_runs(None)? {
            let s = read_run(&self.db, id)?;
            let r = half(&s.section);
            for w in s.pts().windows(2) {
                others.push(Routed {
                    a: w[0],
                    b: w[1],
                    clearance: r,
                });
                if let Some(st) = names.get(&s.system).and_then(|n| stack_of(n, w[0], w[1], &zs)) {
                    stacks.push(st);
                }
            }
        }
        let mut ports = HashMap::new();
        for p in self.component_ports(None)? {
            grow(v2(p.at.0, p.at.1));
            ports.insert((p.comp, p.name.clone()), p);
        }
        let mut axes = (vec![], vec![]);
        for g in self.grids()? {
            let (a, b) = (g.spec.a, g.spec.b);
            if g.spec.bulge != 0.0 {
                continue;
            }
            if (a.0 - b.0).abs() < 1e-6 {
                axes.0.push(a.0);
            } else if (a.1 - b.1).abs() < 1e-6 {
                axes.1.push(a.1);
            }
        }
        if !lo.x.is_finite() {
            lo = v2(0.0, 0.0);
            hi = v2(0.0, 0.0);
        }
        Ok(Site {
            obstacles,
            walls,
            slabs,
            others,
            stacks,
            levels,
            ports,
            axes,
            lo: lo - v2(2.0, 2.0),
            hi: hi + v2(2.0, 2.0),
        })
    }

    /// Plans one network; `VARIANTS` gives the alternatives by `req.variant`.
    pub fn mep_plan_net(&self, req: &NetReq) -> Result<Planned, Error> {
        self.mep_plan_all(std::slice::from_ref(req))
    }

    /// The variants of one network, in order.
    pub fn mep_plan_variants(&self, req: &NetReq) -> Result<Vec<Planned>, Error> {
        let site = self.read_site()?;
        (0..VARIANTS)
            .map(|v| {
                let req = NetReq { variant: v, ..req.clone() };
                self.plan_site(std::slice::from_ref(&req), site.clone())
            })
            .collect()
    }

    /// Several networks planned together: each keeps clear of the ones before it.
    pub fn mep_plan_all(&self, reqs: &[NetReq]) -> Result<Planned, Error> {
        self.plan_site(reqs, self.read_site()?)
    }

    fn plan_site(&self, reqs: &[NetReq], mut site: Site) -> Result<Planned, Error> {
        let mut out = Planned::default();
        for req in reqs {
            let (b, m) = self.plan_one(&mut site, req)?;
            out.build.append(b);
            out.metrics.add(&m);
            out.metrics.variant = req.variant;
        }
        finish(&site, &mut out);
        Ok(out)
    }

    fn plan_one(&self, site: &mut Site, req: &NetReq) -> Result<(MepBuild, RouteMetrics), Error> {
        if req.to.is_empty() {
            return invalid("mep.err.no_targets");
        }
        if !(req.slope.is_finite() && (0.0..=10.0).contains(&req.slope)) {
            return invalid("mep.err.bad_route");
        }
        let li = site
            .levels
            .iter()
            .position(|l| l.0 == req.level)
            .ok_or_else(|| Error::Invalid(format!("no level {}", req.level)))?;
        let zl = site.levels[li].1;
        let zn = site.levels.get(li + 1).map(|l| l.1);
        let below = |level: i64, dflt: f64| {
            site.slabs
                .iter()
                .filter(|s| s.level == level)
                .map(|s| s.z0)
                .fold(None, |a: Option<f64>, z| Some(a.map_or(z, |a| a.min(z))))
                .unwrap_or(dflt)
        };
        let ceiling = match site.levels.get(li + 1) {
            Some(n) => below(n.0, n.1 - 0.25),
            None => zl + 3.0,
        };
        let floor_under = below(req.level, zl - 0.25);
        // targets and what each carries
        let mut targets = vec![];
        let mut loads = vec![];
        for (comp, conn) in &req.to {
            let p = site
                .ports
                .get(&(*comp, conn.clone()))
                .ok_or_else(|| {
                    Error::Invalid(format!("component {comp} has no connector {conn}"))
                })?;
            if !req.kind.domains().contains(&p.domain) && !(super::manual::same_net(
                p.domain,
                domain_of(req.kind),
            )) {
                return invalid(format!("mep.err.wrong_system {comp} {conn}"));
            }
            let d = pv(p.dir);
            targets.push(Terminal {
                at: pv(p.at),
                dir: (d.len() > 1e-9).then(|| d.normalized()),
            });
            let flow = self.mep_port_flow(*comp, conn)?;
            loads.push(Load {
                flow,
                peak: flow,
                port: p.size,
                count: 1,
            });
        }
        let total = loads.iter().fold(Load::default(), |a, l| a.plus(*l));
        let trunk = size_for(req, total)?;
        let r_max = half(&trunk.0);
        let min_leg = spec_of(req.level, trunk, &[]).min_leg().max(0.2);
        let clearance = r_max + GAP;
        let gravity = req.kind == SystemKind::Sanitary;
        let top = if gravity {
            floor_under - clearance
        } else if let Some((lo, hi)) = req.plenum {
            // the trunk with its insulation and hanger gap must fit the plenum height
            let v = match trunk.0.envelope() {
                Shape::Round { d } => d / 2.0,
                Shape::Rect { h, .. } | Shape::Oval { h, .. } => h / 2.0,
            } + GAP;
            if hi.min(ceiling) - v < lo + v - 1e-9 {
                return invalid("mep.err.plenum_low");
            }
            hi.min(ceiling) - v
        } else {
            ceiling - clearance - layer_drop(req.kind)
        };
        let mut build = MepBuild::default();
        let sys = match self.mep_systems()?.into_iter().find(|s| s.name == req.name) {
            Some(s) if s.kind != req.kind => return invalid("mep.err.system_kind"),
            Some(s) => SysRef::Id(s.id),
            None => {
                build.systems.push((req.name.clone(), req.kind));
                SysRef::New(0)
            }
        };
        let mut extra_runs: Vec<RunSpec> = vec![];
        let src = match &req.from {
            RouteFrom::Port { comp, connector } => site
                .ports
                .get(&(*comp, connector.clone()))
                .map_or(v2(0.0, 0.0), |p| v2(p.at.0, p.at.1)),
            RouteFrom::Riser { x, y } => v2(*x, *y),
            RouteFrom::Run(id) => {
                let p = read_run(&self.db, *id)?.pts();
                v2(p[0].x, p[0].y)
            }
        };
        // the farthest target along the axes: what the fall has to climb at most
        let reach = targets
            .iter()
            .map(|t| t.at)
            .fold(0.0f64, |a, p| a.max((p.x - src.x).abs() + (p.y - src.y).abs()));
        let mut layer = if gravity {
            top - req.slope / 100.0 * reach.min(40.0) * 1.1
        } else {
            top
        };
        let mut tree = Tree::default();
        let mut riser: Option<(V3, V3)> = None;
        let mut joints: (Option<V3>, Option<V3>) = (None, None);
        let table = route_table();
        let ix = Index::new(site.obstacles.clone());
        let base_others = RoutedIndex::new(site.others.clone());
        for _attempt in 0..3 {
            let (source, ris, ends) = match &req.from {
                RouteFrom::Port { comp, connector } => {
                    let p = site.ports.get(&(*comp, connector.clone())).ok_or_else(|| {
                        Error::Invalid(format!("component {comp} has no connector {connector}"))
                    })?;
                    let d = pv(p.dir);
                    let t = Terminal {
                        at: pv(p.at),
                        dir: (d.len() > 1e-9).then(|| d.normalized()),
                    };
                    (Source::Terminal(t), None, (None, None))
                }
                RouteFrom::Riser { x, y } => {
                    let (z0, z1) = if gravity {
                        (layer - 0.6, zl + 0.3)
                    } else {
                        let lowest = site
                            .slabs
                            .iter()
                            .filter(|s| s.level == req.level)
                            .map(|s| s.z0)
                            .fold(zl, f64::min);
                        (lowest - 0.4, top)
                    };
                    let at = v2(*x, *y);
                    let near = table.riser_join_m;
                    match join_riser(&site.stacks, &req.name, at, near, (z0, z1), layer) {
                        Join::Through(a, b) => (Source::Lines(vec![(a, b)]), None, (None, None)),
                        Join::New { a, b, below, above } => {
                            (Source::Lines(vec![(a, b)]), Some((a, b)), (below, above))
                        }
                    }
                }
                RouteFrom::Run(id) => {
                    let s = read_run(&self.db, *id)?;
                    let p = s.pts();
                    let lines = p.windows(2).map(|w| (w[0], w[1])).collect();
                    (Source::Lines(lines), None, (None, None))
                }
            };
            let mut rules = Rules::new(clearance, min_leg, vec![layer], (site.lo, site.hi));
            rules.zone = (layer - 0.01, layer + 0.01);
            rules.axes = site.axes.clone();
            rules.max_states = table.max_states;
            let v = table.variants.get(req.variant).unwrap_or(&table.variants[0]);
            rules.bend_cost = v.bend_cost;
            rules.axis_gain = v.axis_gain.unwrap_or(rules.axis_gain);
            rules.wall_cost = v.wall_cost.unwrap_or(rules.wall_cost);
            if req.kind == SystemKind::Power {
                // conduits are chased in walls
                rules.in_wall = table.power.in_wall;
                rules.wall_cost = table.power.wall_cost;
            }
            let mut others = base_others.clone();
            if let Some((a, b)) = ris {
                // a riser of this system: other systems' risers keep clear of it; the stack it
                // continues meets it at the joints
                let clear = match ends {
                    (Some(j0), Some(j1)) => {
                        let m = v3(a.x, a.y, (a.z + b.z) / 2.0);
                        others.clear(a, m, clearance, Some(j0))
                            && others.clear(m, b, clearance, Some(j1))
                    }
                    (j0, j1) => others.clear(a, b, clearance, j0.or(j1)),
                };
                if !clear {
                    return invalid("mep.err.riser_blocked");
                }
            }
            tree = route3d::route_tree(&ix, &rules, &source, &targets, &mut others);
            riser = ris;
            joints = ends;
            if !gravity {
                break;
            }
            let mut t = tree.clone();
            route3d::apply_fall(&mut t, &[layer], req.slope);
            let zmax = t
                .branches
                .iter()
                .flat_map(|b| b.points.iter())
                .filter(|p| p.z < floor_under)
                .map(|p| p.z)
                .fold(f64::NEG_INFINITY, f64::max);
            tree = t;
            if zmax <= top + 1e-6 {
                break;
            }
            layer -= zmax - top + 0.02;
        }
        // pieces: each branch split where others join it, sized by what flows downstream
        let n = tree.branches.len();
        let mut kids: Vec<Vec<(f64, usize)>> = vec![vec![]; n];
        for (i, b) in tree.branches.iter().enumerate() {
            if let Some(p) = b.parent {
                kids[p].push((param_on(&tree.branches[p].points, b.join), i));
            }
        }
        for k in &mut kids {
            k.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let mut sub = vec![Load::default(); n];
        fn subtree(i: usize, t: &Tree, kids: &[Vec<(f64, usize)>], l: &[Load], s: &mut [Load]) {
            let mut acc = l[t.branches[i].target];
            for &(_, k) in &kids[i] {
                subtree(k, t, kids, l, s);
                acc = acc.plus(s[k]);
            }
            s[i] = acc;
        }
        for i in 0..n {
            if tree.branches[i].parent.is_none() {
                subtree(i, &tree, &kids, &loads, &mut sub);
            }
        }
        let mut runs: Vec<RunSpec> = vec![];
        let mut pieces: Vec<(Vec<f64>, Vec<Load>)> = vec![];
        for (i, b) in tree.branches.iter().enumerate() {
            let total_len = route3d::poly_len(&b.points);
            let mut cuts = vec![0.0];
            let mut load = loads[b.target];
            let mut piece_loads = vec![];
            for &(at, k) in &kids[i] {
                piece_loads.push(load);
                cuts.push(at);
                load = load.plus(sub[k]);
            }
            piece_loads.push(load);
            cuts.push(total_len);
            pieces.push((cuts.clone(), piece_loads.clone()));
            // equal sizes stay one run: the joins become tees on it
            let mut start = 0;
            let mut cur = size_for(req, piece_loads[0])?;
            for j in 1..=piece_loads.len() {
                let next = if j < piece_loads.len() {
                    Some(size_for(req, piece_loads[j])?)
                } else {
                    None
                };
                if next.as_ref().is_some_and(|s| s.0 == cur.0) {
                    continue;
                }
                let pts = slice(&b.points, cuts[start], cuts[j]);
                if pts.len() >= 2 {
                    runs.push(spec_of(req.level, cur, &pts));
                }
                if let Some(s) = next {
                    cur = s;
                    start = j;
                }
            }
        }
        if let Some((a, b)) = riser {
            let mut r = spec_of(req.level, trunk, &[a, b]);
            r.caps = (joints.0.is_none(), joints.1.is_none());
            extra_runs.push(r);
            let zs: Vec<f64> = site.levels.iter().map(|l| l.1).collect();
            site.stacks.extend(stack_of(&req.name, a, b, &zs));
        }
        runs.extend(extra_runs);
        // metrics and the next request's clearances
        let mut m = RouteMetrics {
            variant: req.variant,
            runs: runs.len(),
            ..RouteMetrics::default()
        };
        let (bends, tees) = tree.fittings();
        m.elbows = bends;
        m.tees = tees;
        m.failed = tree.failed.iter().map(|&i| req.to[i].clone()).collect();
        if domain_of(req.kind) == Domain::Duct {
            if let Some((pa, b)) = critical(req, &tree, &kids, &pieces)? {
                m.critical_pa = Some(pa);
                m.critical_to = Some(req.to[tree.branches[b].target].clone());
            }
        }
        let (mut zmin, mut zmax) = (f64::INFINITY, f64::NEG_INFINITY);
        let ix = Index::new(
            site.obstacles
                .iter()
                .filter(|o| o.pass == Pass::Block)
                .cloned()
                .collect(),
        );
        for r in &runs {
            let p = r.pts();
            let rad = half(&r.section);
            m.length += route3d::poly_len(&p);
            for w in p.windows(2) {
                zmin = zmin.min(w[0].z.min(w[1].z));
                zmax = zmax.max(w[0].z.max(w[1].z));
                site.others.push(Routed {
                    a: w[0],
                    b: w[1],
                    clearance: rad,
                });
                let hit = ix.obstacles.iter().any(|o| {
                    w[0].z.max(w[1].z) > o.z0 - rad
                        && w[0].z.min(w[1].z) < o.z1 + rad
                        && route3d::seg_ring_dist(&o.ring, v2(w[0].x, w[0].y), v2(w[1].x, w[1].y))
                            < rad - 1e-6
                });
                m.clashes += hit as usize;
                let run = v2(w[1].x - w[0].x, w[1].y - w[0].y).len();
                let dz = (w[1].z - w[0].z).abs();
                if gravity && run >= 0.3 && dz < run {
                    let pct = dz / run * 100.0;
                    m.min_slope = Some(m.min_slope.map_or(pct, |s: f64| s.min(pct)));
                }
            }
        }
        m.z_min = if zmin.is_finite() { zmin } else { 0.0 };
        m.z_max = if zmax.is_finite() { zmax } else { 0.0 };
        build.runs = runs.into_iter().map(|r| (sys, r)).collect();
        for (i, (comp, conn)) in req.to.iter().enumerate() {
            if !tree.failed.contains(&i) {
                build.links.push((*comp, conn.clone(), sys));
            }
        }
        if let RouteFrom::Port { comp, connector } = &req.from {
            build.links.push((*comp, connector.clone(), sys));
        }
        Ok((build, m))
    }

    /// Power for devices from a panel: one conduit network routed from the panel's connector,
    /// the devices grouped into circuits (lighting and power apart, at most `PER_CIRCUIT` each,
    /// within 80 % of the breaker), numbered after the panel's circuits.
    pub fn mep_plan_power(
        &self,
        panel: i64,
        devices: &[(i64, String)],
        level: i64,
        variant: usize,
    ) -> Result<Planned, Error> {
        let ports = self.component_ports(Some(panel))?;
        let main = ports
            .iter()
            .find(|p| matches!(p.domain, Domain::Conduit | Domain::CableTray))
            .ok_or_else(|| Error::Invalid("mep.err.no_panel_port".into()))?;
        let name = format!("Power {panel}");
        let mut req = NetReq::new(
            &name,
            SystemKind::Power,
            level,
            RouteFrom::Port {
                comp: panel,
                connector: main.name.clone(),
            },
        );
        req.to = devices.to_vec();
        req.variant = variant;
        let mut out = self.mep_plan_net(&req)?;
        let sys = out.build.runs.first().map(|r| r.0);
        // devices in the order the tree reaches them keep circuits together in plan
        let mut order: Vec<(f64, usize)> = vec![];
        let at = |c: i64, n: &str| -> Result<V3, Error> { Ok(pv(self.world_port(c, n)?.at)) };
        let o = pv(main.at);
        for (i, (c, n)) in devices.iter().enumerate() {
            let p = at(*c, n)?;
            order.push(((p.y - o.y).atan2(p.x - o.x), i));
        }
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let rules = super::CableRules::shared();
        let mut groups: Vec<(CircuitKind, Vec<CircuitDevice>, f64)> = vec![];
        for (_, i) in order {
            let (c, n) = &devices[i];
            if out.metrics.failed.contains(&devices[i]) {
                continue;
            }
            let kind = if self.is_lighting(*c)? {
                CircuitKind::Lighting
            } else {
                CircuitKind::Power
            };
            let load = self.mep_port_flow(*c, n)?;
            let cap = kind.defaults().0 * 230.0 * rules.cos_phi(kind);
            let g = groups
                .iter_mut()
                .rev()
                .find(|g| g.0 == kind)
                .filter(|g| g.1.len() < PER_CIRCUIT && g.2 + load <= cap);
            match g {
                Some(g) => {
                    g.1.push(CircuitDevice {
                        comp: *c,
                        connector: n.clone(),
                        load: None,
                    });
                    g.2 += load;
                }
                None => groups.push((
                    kind,
                    vec![CircuitDevice {
                        comp: *c,
                        connector: n.clone(),
                        load: None,
                    }],
                    load,
                )),
            }
        }
        for (kind, devs, load) in groups {
            let mut spec = CircuitSpec::new(panel, kind, devs);
            // breaker and cable from the table for the load and a first length: the farthest
            // device by axis-parallel distance with the laying slack and reserves
            let mut far = 0.0f64;
            for d in &spec.devices {
                let p = at(d.comp, &d.connector)?;
                far = far.max((p.x - o.x).abs() + (p.y - o.y).abs() + (p.z - o.z).abs());
            }
            let length = rules.cable_length(&spec.laying, far).unwrap_or(far);
            let want = super::CableInput {
                kind,
                standard: self.elec_standard()?,
                voltage: spec.voltage,
                poles: spec.poles,
                load,
                cos_phi: spec.cos_phi,
                material: spec.conductor,
                length,
                breaker: spec.breaker,
                section: spec.cable,
                laying: spec.laying.clone(),
                ambient_c: spec.ambient_c,
                grouped: spec.grouped,
                max_drop: Some(spec.max_drop),
                measured_drop: None,
            };
            if let Some(z) = rules.size(&want) {
                spec.breaker = z.breaker;
                spec.cable = z.section;
            }
            out.build.circuits.push((sys, spec));
        }
        Ok(out)
    }

    /// Connectors on a level a network of `kind` can serve, sources (panels, units) left out;
    /// with `room` (number or name) only those in that room or on its walls.
    pub fn mep_targets(
        &self,
        level: i64,
        kind: SystemKind,
        room: Option<&str>,
    ) -> Result<Vec<(i64, String)>, Error> {
        let outline = match room {
            None => None,
            Some(r) => {
                let id: i64 = self
                    .db
                    .query_row(
                        "SELECT id FROM room WHERE level=?1 AND (number=?2 OR name=?2) \
                         ORDER BY id LIMIT 1",
                        rusqlite::params![level, r],
                        |x| x.get(0),
                    )
                    .map_err(|_| Error::Invalid(format!("no room {r} on level {level}")))?;
                let o = self
                    .room_outline(id)?
                    .ok_or_else(|| Error::Invalid(format!("room {r} is not closed")))?;
                Some(o.into_iter().map(|p| v2(p.0, p.1)).collect::<Vec<_>>())
            }
        };
        let on_level: Vec<i64> = self
            .db
            .prepare("SELECT id FROM comp WHERE level=?1 ORDER BY id")?
            .query_map([level], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut out = vec![];
        let mut sources: HashMap<i64, bool> = HashMap::new();
        for p in self.component_ports(None)? {
            if !on_level.contains(&p.comp) || !super::manual::accepts(&p, kind) {
                continue;
            }
            let src = match sources.get(&p.comp) {
                Some(s) => *s,
                None => {
                    let s = self.port_defaults(p.comp)?.values().any(|d| d.1);
                    sources.insert(p.comp, s);
                    s
                }
            };
            if src {
                continue;
            }
            if let Some(o) = &outline {
                let q = v2(p.at.0, p.at.1);
                let near = (0..o.len()).any(|i| {
                    let (a, b) = (o[i], o[(i + 1) % o.len()]);
                    let d = b - a;
                    let t = ((q - a).dot(d) / d.dot(d).max(1e-18)).clamp(0.0, 1.0);
                    (a + d * t - q).len() < 0.15
                });
                if !inside(o, q) && !near {
                    continue;
                }
            }
            out.push((p.comp, p.name));
        }
        Ok(out)
    }

    /// Highest component id (0 in an empty model): what a placement adds comes after it.
    pub fn last_component(&self) -> Result<i64, Error> {
        Ok(self
            .db
            .query_row("SELECT COALESCE(MAX(id),0) FROM comp", [], |r| r.get(0))?)
    }

    /// A light fitting or a switch: its devices go on lighting circuits.
    fn is_lighting(&self, comp: i64) -> Result<bool, Error> {
        let def: String = self.db.query_row(
            "SELECT f.def FROM comp c JOIN comp_type t ON t.id=c.ctype \
             JOIN comp_family f ON f.id=t.family WHERE c.id=?1",
            [comp],
            |r| r.get(0),
        )?;
        Ok(def.contains("IfcLightFixture") || def.contains("IfcSwitchingDevice"))
    }
}

impl MepBuild {
    /// Appends another build; its new systems follow ours.
    pub fn append(&mut self, o: MepBuild) {
        let n = self.systems.len();
        let map = |r: SysRef| match r {
            SysRef::New(k) => SysRef::New(k + n),
            r => r,
        };
        self.systems.extend(o.systems);
        self.runs.extend(o.runs.into_iter().map(|(r, s)| (map(r), s)));
        self.links
            .extend(o.links.into_iter().map(|(c, k, r)| (c, k, map(r))));
        self.sleeves.extend(o.sleeves);
        self.holes.extend(o.holes);
        self.circuits
            .extend(o.circuits.into_iter().map(|(r, c)| (r.map(map), c)));
    }
}

/// Sleeves in walls and holes in slabs for every run of the build, merged where they overlap.
fn finish(site: &Site, out: &mut Planned) {
    let mut sleeves: Vec<(i64, [f64; 4])> = vec![];
    let mut holes: Vec<(i64, V2, V2)> = vec![];
    for (_, r) in &out.build.runs {
        let rad = half(&r.section) + GAP;
        let p = r.pts();
        for w in p.windows(2) {
            for wall in &site.walls {
                for &(a, b, s0) in &wall.chords {
                    if let Some(rect) = sleeve(wall, a, b, s0, w[0], w[1], rad) {
                        sleeves.push((wall.id, rect));
                    }
                }
            }
            for s in &site.slabs {
                if w[0].z.max(w[1].z) <= s.z0 - rad || w[0].z.min(w[1].z) >= s.z1 + rad {
                    continue;
                }
                // the part of the segment in the slab band, in plan
                let (za, zb) = (s.z0 - rad, s.z1 + rad);
                let d = w[1] - w[0];
                let (t0, t1) = if d.z.abs() > 1e-9 {
                    let (ta, tb) = ((za - w[0].z) / d.z, (zb - w[0].z) / d.z);
                    (ta.min(tb).max(0.0), ta.max(tb).min(1.0))
                } else {
                    (0.0, 1.0)
                };
                if t1 < t0 {
                    continue;
                }
                let (c0, c1) = (w[0] + d * t0, w[0] + d * t1);
                let lo = v2(c0.x.min(c1.x), c0.y.min(c1.y)) - v2(rad, rad);
                let hi = v2(c0.x.max(c1.x), c0.y.max(c1.y)) + v2(rad, rad);
                let mid = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
                let open = s
                    .holes
                    .iter()
                    .any(|h| mid.x > h.0.x && mid.x < h.1.x && mid.y > h.0.y && mid.y < h.1.y);
                if !open && inside(&s.ring, mid) {
                    holes.push((s.id, lo, hi));
                }
            }
        }
    }
    // union of overlapping rectangles, per host
    let merge4 = |v: &mut Vec<(i64, [f64; 4])>| loop {
        let mut done = true;
        'o: for i in 0..v.len() {
            for j in i + 1..v.len() {
                let (a, b) = (v[i].1, v[j].1);
                let meet = a[0] <= b[1] && b[0] <= a[1] && a[2] <= b[3] && b[2] <= a[3];
                if v[i].0 == v[j].0 && meet {
                    v[i].1 = [a[0].min(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].max(b[3])];
                    v.remove(j);
                    done = false;
                    break 'o;
                }
            }
        }
        if done {
            break;
        }
    };
    merge4(&mut sleeves);
    let mut hv: Vec<(i64, [f64; 4])> = holes
        .into_iter()
        .map(|(id, lo, hi)| (id, [lo.x, hi.x, lo.y, hi.y]))
        .collect();
    merge4(&mut hv);
    for (wall, [u0, u1, z0, z1]) in sleeves {
        let w = site.walls.iter().find(|w| w.id == wall);
        let base = w.map_or(0.0, |w| w.z0);
        out.build.sleeves.push((
            wall,
            OpeningSpec {
                along: u0,
                width: u1 - u0,
                sill: z0 - base,
                height: z1 - z0,
                kind: "void".into(),
            },
        ));
    }
    for (slab, [x0, x1, y0, y1]) in hv {
        out.build
            .holes
            .push((slab, vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]));
    }
    out.metrics.sleeves = out.build.sleeves.len();
    out.metrics.holes = out.build.holes.len();
}

/// Strictly inside the ring.
fn inside(ring: &[V2], p: V2) -> bool {
    ddc_geom::predicates::point_in_ring(p, ring) == 1
}

/// The void a segment of radius `rad` needs in one wall chord: [along0, along1, z0, z1].
fn sleeve(w: &WallInfo, a: V2, b: V2, s0: f64, p: V3, q: V3, rad: f64) -> Option<[f64; 4]> {
    let len = (b - a).len();
    let u = (b - a) * (1.0 / len);
    let nrm = v2(-u.y, u.x);
    let fp = clash::wall_footprint(a, b, w.t, w.off);
    let vs: Vec<f64> = fp.iter().map(|x| (*x - a).dot(nrm)).collect();
    let (vmin, vmax) = vs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |m, v| {
        (m.0.min(*v), m.1.max(*v))
    });
    let (vmin, vmax) = (vmin - rad, vmax + rad);
    let pv_ = |x: V3| (v2(x.x, x.y) - a).dot(nrm);
    let pu = |x: V3| (v2(x.x, x.y) - a).dot(u);
    let (v0, v1) = (pv_(p), pv_(q));
    let (t0, t1) = if (v1 - v0).abs() > 1e-12 {
        let (ta, tb) = ((vmin - v0) / (v1 - v0), (vmax - v0) / (v1 - v0));
        (ta.min(tb).max(0.0), ta.max(tb).min(1.0))
    } else if v0 > vmin && v0 < vmax {
        (0.0, 1.0)
    } else {
        return None;
    };
    if t1 <= t0 + 1e-12 && !(t1 >= t0 && (v1 - v0).abs() <= 1e-12) {
        return None;
    }
    let (c0, c1) = (p + (q - p) * t0, p + (q - p) * t1);
    let (u0, u1) = (pu(c0).min(pu(c1)) - rad, pu(c0).max(pu(c1)) + rad);
    let (z0, z1) = (c0.z.min(c1.z) - rad, c0.z.max(c1.z) + rad);
    let (u0, u1) = (u0.max(0.0), u1.min(len));
    let (z0, z1) = (z0.max(w.z0), z1.min(w.z0 + w.h));
    if u1 - u0 < 1e-6 || z1 - z0 < 1e-6 {
        return None;
    }
    Some([s0 + u0, s0 + u1, z0, z1])
}
