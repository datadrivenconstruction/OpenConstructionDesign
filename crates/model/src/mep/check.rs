// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! System check: graph from the source, accumulated flow per run, velocity and capacity with
//! size selection, connectivity, drainage slope. The result is structured; message text is
//! produced by the view from `key` and `args`. Limits and flows are guidelines: no codes are
//! applied and simultaneity is ignored, so exceeding a guideline is a remark, not an error.

use std::collections::HashMap;

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{Domain, PipeMaterial, Shape, SystemKind};

use super::{nearest_on, read_run, MepNetwork, MepSystem, RunSpec, WorldPort, PORT_TOL};
use crate::{Error, Model};

/// Air velocity guideline, m/s.
const AIR_MAX: f64 = 8.0;
/// Domestic water velocity guideline, m/s; not applied to fire networks.
const WATER_MAX: f64 = 1.5;
/// Manning roughness for gravity-flow calculation.
const MANNING_N: f64 = 0.013;
/// Voltage for current from load, V.
const VOLTS: f64 = 230.0;
/// Approximate conduit current: up to 25 mm and larger, A.
const CONDUIT_AMPS: (f64, f64) = (16.0, 32.0);
/// Segments shorter than this are not checked for slope, m.
const SLOPE_MIN_LEG: f64 = 0.3;
/// Departure from a connection along its axis: cosine of the largest deviation.
const APPROACH_MIN_COS: f64 = 0.7;
/// A segment steeper than this fraction and not vertical counts as skewed.
const MAX_TILT: f64 = 0.1;
const ROUND_DUCT_MM: [u32; 12] = [80, 100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000];

/// A check finding: what is wrong and where. Value units are the same as in `MepSystemCheck`.
#[derive(Clone, Debug, PartialEq)]
pub enum MepIssue {
    /// The graph cannot be built: a fitting does not fit or there are too many branches.
    Network {
        message: String,
    },
    Broken {
        parts: usize,
    },
    OpenEnd {
        run: i64,
        end: u8,
    },
    NotConnected {
        comp: i64,
        connector: String,
    },
    WrongSystem {
        comp: i64,
        connector: String,
    },
    NoFlow,
    /// Appliances are connected but have no flow set.
    ZeroFlow,
    Loop,
    Undersized {
        run: i64,
        speed: f64,
        limit: f64,
        suggest: String,
    },
    Overflow {
        run: i64,
        flow: f64,
        capacity: f64,
        suggest: String,
    },
    Overload {
        run: i64,
        amps: f64,
        limit: f64,
    },
    PortSize {
        comp: i64,
        connector: String,
        run: i64,
        port_mm: f64,
        run_mm: f64,
    },
    /// A run reaches a connection off its axis or runs at a skew.
    PortApproach {
        comp: i64,
        connector: String,
        run: i64,
    },
    SlopeLow {
        run: i64,
        pct: f64,
        min: f64,
    },
    SlopeReverse {
        run: i64,
    },
}

fn f1(v: f64) -> String {
    format!("{v:.1}")
}

impl MepIssue {
    /// Message key `mep.issue.*`.
    pub fn key(&self) -> &'static str {
        match self {
            MepIssue::Network { .. } => "mep.issue.network",
            MepIssue::Broken { .. } => "mep.issue.broken",
            MepIssue::OpenEnd { .. } => "mep.issue.open_end",
            MepIssue::NotConnected { .. } => "mep.issue.not_connected",
            MepIssue::WrongSystem { .. } => "mep.issue.wrong_system",
            MepIssue::NoFlow => "mep.issue.no_flow",
            MepIssue::ZeroFlow => "mep.issue.zero_flow",
            MepIssue::Loop => "mep.issue.loop",
            MepIssue::Undersized { suggest, .. } if suggest.is_empty() => {
                "mep.issue.undersized_no_size"
            }
            MepIssue::Undersized { .. } => "mep.issue.undersized",
            MepIssue::Overflow { suggest, .. } if suggest.is_empty() => {
                "mep.issue.overflow_no_size"
            }
            MepIssue::Overflow { .. } => "mep.issue.overflow",
            MepIssue::Overload { .. } => "mep.issue.overload",
            MepIssue::PortSize { .. } => "mep.issue.port_size",
            MepIssue::PortApproach { .. } => "mep.issue.port_approach",
            MepIssue::SlopeLow { .. } => "mep.issue.slope_low",
            MepIssue::SlopeReverse { .. } => "mep.issue.slope_reverse",
        }
    }
    /// Message substitutions.
    pub fn args(&self) -> Vec<(&'static str, String)> {
        match self {
            MepIssue::Network { message } => vec![("message", message.clone())],
            MepIssue::Broken { parts } => vec![("parts", parts.to_string())],
            MepIssue::OpenEnd { run, .. } | MepIssue::SlopeReverse { run } => {
                vec![("run", run.to_string())]
            }
            MepIssue::NotConnected { comp, connector }
            | MepIssue::WrongSystem { comp, connector } => {
                vec![("comp", comp.to_string()), ("connector", connector.clone())]
            }
            MepIssue::NoFlow | MepIssue::ZeroFlow | MepIssue::Loop => vec![],
            MepIssue::Undersized {
                run,
                speed,
                limit,
                suggest,
            } => vec![
                ("run", run.to_string()),
                ("speed", f1(*speed)),
                ("limit", f1(*limit)),
                ("suggest", suggest.clone()),
            ],
            MepIssue::Overflow {
                run,
                flow,
                capacity,
                suggest,
            } => vec![
                ("run", run.to_string()),
                ("flow", f1(*flow)),
                ("capacity", f1(*capacity)),
                ("suggest", suggest.clone()),
            ],
            MepIssue::Overload { run, amps, limit } => vec![
                ("run", run.to_string()),
                ("amps", f1(*amps)),
                ("limit", f1(*limit)),
            ],
            MepIssue::PortSize {
                comp,
                connector,
                run,
                port_mm,
                run_mm,
            } => vec![
                ("comp", comp.to_string()),
                ("connector", connector.clone()),
                ("run", run.to_string()),
                ("port", format!("{port_mm:.0}")),
                ("size", format!("{run_mm:.0}")),
            ],
            MepIssue::PortApproach {
                comp,
                connector,
                run,
            } => vec![
                ("comp", comp.to_string()),
                ("connector", connector.clone()),
                ("run", run.to_string()),
            ],
            MepIssue::SlopeLow { run, pct, min } => vec![
                ("run", run.to_string()),
                ("pct", f1(*pct)),
                ("min", f1(*min)),
            ],
        }
    }
    /// Error: the network is broken or geometrically wrong. Exceeding guidelines is a remark.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            MepIssue::Network { .. }
                | MepIssue::Broken { .. }
                | MepIssue::NotConnected { .. }
                | MepIssue::WrongSystem { .. }
                | MepIssue::SlopeReverse { .. }
        )
    }
    /// The run the finding refers to.
    pub fn run(&self) -> Option<i64> {
        match self {
            MepIssue::OpenEnd { run, .. }
            | MepIssue::Undersized { run, .. }
            | MepIssue::Overflow { run, .. }
            | MepIssue::Overload { run, .. }
            | MepIssue::PortSize { run, .. }
            | MepIssue::PortApproach { run, .. }
            | MepIssue::SlopeLow { run, .. }
            | MepIssue::SlopeReverse { run } => Some(*run),
            _ => None,
        }
    }
    /// The component the finding refers to.
    pub fn comp(&self) -> Option<i64> {
        match self {
            MepIssue::NotConnected { comp, .. }
            | MepIssue::WrongSystem { comp, .. }
            | MepIssue::PortSize { comp, .. }
            | MepIssue::PortApproach { comp, .. } => Some(*comp),
            _ => None,
        }
    }
}

/// A run in the report. Flow is the largest over its parts; the unit comes from the system.
#[derive(Clone, Debug, PartialEq)]
pub struct MepRunFlow {
    pub run: i64,
    pub size: String,
    pub length: f64,
    pub flow: f64,
    /// Velocity, m/s (air and pressurized water).
    pub speed: Option<f64>,
    pub insulation: f64,
}

/// A component connection in the report.
#[derive(Clone, Debug, PartialEq)]
pub struct MepDeviceRow {
    pub comp: i64,
    pub connector: String,
    pub flow: f64,
    pub source: bool,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MepSystemCheck {
    pub system: i64,
    pub name: String,
    pub kind: SystemKind,
    /// Total: air in m³/h, water in l/s, power in W.
    pub total: f64,
    pub length: f64,
    pub parts: usize,
    pub fittings: usize,
    pub runs: Vec<MepRunFlow>,
    pub devices: Vec<MepDeviceRow>,
    pub issues: Vec<MepIssue>,
}

impl MepSystemCheck {
    pub fn errors(&self) -> usize {
        self.issues.iter().filter(|i| i.is_error()).count()
    }
    /// Flow unit key `mep.unit.*`.
    pub fn unit_key(&self) -> &'static str {
        match self.kind {
            SystemKind::SupplyAir | SystemKind::ReturnAir | SystemKind::ExhaustAir => {
                "mep.unit.air"
            }
            SystemKind::Power => "mep.unit.power",
            _ => "mep.unit.water",
        }
    }
}

/// Accumulated flow per edge from the root of each connected component (a tree); an edge closing
/// a cycle gets no flow. `down[e]`: the edge goes from node a to b moving away from the root.
struct Flows {
    edge: Vec<f64>,
    down: Vec<bool>,
    total: f64,
    has_loop: bool,
}

fn flows(n: usize, edges: &[(usize, usize)], load: &[f64], score: &[(u8, f64, f64)]) -> Flows {
    let mut adj: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
    for (e, &(a, b)) in edges.iter().enumerate() {
        adj[a].push((e, b));
        adj[b].push((e, a));
    }
    // the component root is the node with the highest score; component order is deterministic by node index
    let mut part = vec![usize::MAX; n];
    let mut roots = vec![];
    for s in 0..n {
        if part[s] != usize::MAX {
            continue;
        }
        let id = roots.len();
        let mut stack = vec![s];
        part[s] = id;
        let mut best = s;
        while let Some(u) = stack.pop() {
            if score[u] > score[best] {
                best = u;
            }
            for &(_, w) in &adj[u] {
                if part[w] == usize::MAX {
                    part[w] = id;
                    stack.push(w);
                }
            }
        }
        roots.push(best);
    }
    let mut out = Flows {
        edge: vec![0.0; edges.len()],
        down: vec![true; edges.len()],
        total: 0.0,
        has_loop: false,
    };
    let mut seen = vec![false; n];
    let mut used = vec![false; edges.len()];
    for root in roots {
        let mut order = vec![];
        let mut parent: Vec<Option<(usize, usize)>> = vec![None; n];
        seen[root] = true;
        let mut stack = vec![root];
        while let Some(u) = stack.pop() {
            order.push(u);
            for &(e, w) in &adj[u] {
                if used[e] {
                    continue;
                }
                used[e] = true;
                if seen[w] {
                    out.has_loop = true;
                    continue;
                }
                seen[w] = true;
                parent[w] = Some((e, u));
                out.down[e] = edges[e].0 == u;
                stack.push(w);
            }
        }
        let mut sum: Vec<f64> = load.to_vec();
        for &u in order.iter().rev() {
            if let Some((e, p)) = parent[u] {
                out.edge[e] = sum[u];
                sum[p] += sum[u];
            }
        }
        out.total += sum[root];
    }
    out
}

fn nominal_mm(s: &RunSpec) -> f64 {
    if let Some(dn) = s.dn {
        return dn as f64;
    }
    let (w, h) = s.section.shape.wh();
    w.min(h) * 1000.0
}

/// Size whose free cross-section area is at least `area`, as a label like a run's.
fn suggest(s: &RunSpec, area: f64) -> String {
    let m = s.material.unwrap_or(PipeMaterial::Steel);
    if s.section.domain == Domain::Pipe {
        return m
            .sizes()
            .into_iter()
            .find(|x| std::f64::consts::PI * x.id() * x.id() / 4.0 >= area)
            .map_or_else(String::new, |x| format!("DN{}", x.dn));
    }
    match s.section.shape {
        Shape::Round { .. } => ROUND_DUCT_MM
            .iter()
            .find(|d| {
                let d = **d as f64 / 1000.0;
                std::f64::consts::PI * d * d / 4.0 >= area
            })
            .map_or_else(String::new, |d| format!("Ø{d}")),
        Shape::Rect { h, .. } | Shape::Oval { h, .. } => {
            let w = ((area / h * 1000.0 / 50.0).ceil() * 50.0).max(100.0);
            format!("{w:.0}x{:.0}", h * 1000.0)
        }
    }
}

/// Smallest slope of horizontal segments longer than the threshold, %.
fn min_slope(pts: &[(f64, f64, f64)]) -> Option<f64> {
    let mut out: Option<f64> = None;
    for w in pts.windows(2) {
        let run = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
        let dz = (w[1].2 - w[0].2).abs();
        if run >= SLOPE_MIN_LEG && dz < run {
            let pct = dz / run * 100.0;
            out = Some(out.map_or(pct, |o| o.min(pct)));
        }
    }
    out
}

/// Least fall of a run in percent: gravity drains need one, other systems none.
pub(super) fn min_fall(kind: SystemKind, s: &RunSpec) -> f64 {
    match kind {
        SystemKind::Sanitary if s.section.inner().wh().0 < 0.07 => 2.0,
        SystemKind::Sanitary => 1.0,
        _ => 0.0,
    }
}

/// Capacity of a half-full pipe (Manning), l/s.
fn half_full_capacity(d: f64, slope_pct: f64) -> f64 {
    let a = std::f64::consts::PI * d * d / 8.0;
    a * (d / 4.0).powf(2.0 / 3.0) * (slope_pct / 100.0).sqrt() / MANNING_N * 1000.0
}

/// Part of a run's axis between two of its points, going from `a` to `b`.
fn path_between(pts: &[V3], a: V3, b: V3) -> Vec<V3> {
    let (qa, ka, _) = nearest_on(pts, a);
    let (qb, kb, _) = nearest_on(pts, b);
    let mut out = vec![qa];
    if (ka, (qa - pts[ka]).len()) <= (kb, (qb - pts[kb]).len()) {
        out.extend_from_slice(&pts[ka + 1..=kb]);
    } else {
        out.extend(pts[kb + 1..=ka].iter().rev());
    }
    out.push(qb);
    out
}

/// A run reaches a connection off its axis or at a skew. `tag` is the end at the connection.
fn skewed(pts: &[V3], tag: u8, dir: V3) -> bool {
    let leg = if tag == 0 {
        pts[1] - pts[0]
    } else {
        pts[pts.len() - 2] - pts[pts.len() - 1]
    };
    leg.normalized().dot(dir.normalized()) < APPROACH_MIN_COS
        || pts.windows(2).any(|w| {
            let plan = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
            let dz = (w[1].z - w[0].z).abs();
            dz > plan * MAX_TILT && dz * MAX_TILT < plan
        })
}

/// Smallest size passing `flow` at slope `pct`, as a label.
fn suggest_gravity(s: &RunSpec, flow: f64, pct: f64) -> String {
    let m = s.material.unwrap_or(PipeMaterial::Pvc);
    m.sizes()
        .into_iter()
        .find(|x| half_full_capacity(x.id(), pct) >= flow)
        .map_or_else(String::new, |x| format!("DN{}", x.dn))
}

impl Model {
    /// Report for all systems, in id order.
    pub fn mep_check_all(&self) -> Result<Vec<MepSystemCheck>, Error> {
        let ports = self.component_ports(None)?;
        self.mep_systems()?
            .into_iter()
            .map(|s| self.check_with(s, &ports))
            .collect()
    }

    /// Checks a system. A graph that cannot be built is a finding, not a call error.
    pub fn mep_check(&self, system: i64) -> Result<MepSystemCheck, Error> {
        let sys = self
            .mep_systems()?
            .into_iter()
            .find(|s| s.id == system)
            .ok_or_else(|| Error::Invalid(format!("no such system {system}")))?;
        self.check_with(sys, &self.component_ports(None)?)
    }

    /// `mep_check` with the connector list already at hand.
    pub(super) fn mep_check_with_ports(
        &self,
        system: i64,
        ports: &[WorldPort],
    ) -> Result<MepSystemCheck, Error> {
        let sys = self
            .mep_systems()?
            .into_iter()
            .find(|s| s.id == system)
            .ok_or_else(|| Error::Invalid(format!("no such system {system}")))?;
        self.check_with(sys, ports)
    }

    fn check_with(&self, sys: MepSystem, ports: &[WorldPort]) -> Result<MepSystemCheck, Error> {
        let system = sys.id;
        let ids = self.mep_runs(Some(system))?;
        let specs: HashMap<i64, RunSpec> = ids
            .iter()
            .map(|&i| Ok::<_, Error>((i, read_run(&self.db, i)?)))
            .collect::<Result<_, Error>>()?;
        let mut issues = vec![];
        let net = match self.mep_network_with(Some(system), ports) {
            Ok(n) => Some(n),
            Err(e) => {
                issues.push(MepIssue::Network {
                    message: e.to_string(),
                });
                None
            }
        };
        let electric = sys.kind == SystemKind::Power;

        // connections: those linked to and touching the network
        let all_links = self.mep_port_links(None)?;
        let mut links: Vec<(i64, String, Option<f64>)> = all_links
            .iter()
            .filter(|l| l.system == Some(system))
            .map(|l| (l.comp, l.connector.clone(), l.flow))
            .collect();
        if let Some(n) = &net {
            for node in &n.nodes {
                if let Some((c, name)) = &node.port {
                    if !links.iter().any(|l| l.0 == *c && l.1 == *name) {
                        let flow = all_links
                            .iter()
                            .find(|l| l.comp == *c && l.connector == *name)
                            .and_then(|l| l.flow);
                        links.push((*c, name.clone(), flow));
                    }
                }
            }
        }
        let node_at = |n: &MepNetwork, at: (f64, f64, f64)| {
            n.nodes
                .iter()
                .position(|x| (x.at.0 - at.0).hypot(x.at.1 - at.1).hypot(x.at.2 - at.2) < PORT_TOL)
        };
        let n_nodes = net.as_ref().map_or(0, |n| n.nodes.len());
        let mut load = vec![0.0; n_nodes];
        let mut is_source = vec![false; n_nodes];
        let mut devices = vec![];
        let mut off_net = 0.0;
        // nodes each component's connectors meet: a unit inline joins the two it sits between
        let mut at_comp: HashMap<i64, Vec<usize>> = HashMap::new();
        for (comp, connector, over) in &links {
            let Some(port) = ports
                .iter()
                .find(|p| p.comp == *comp && p.name == *connector)
            else {
                continue;
            };
            let (dflt, source) = self
                .port_defaults(*comp)?
                .get(connector)
                .copied()
                .unwrap_or((0.0, false));
            let flow = over.unwrap_or(dflt);
            let node = net.as_ref().and_then(|n| node_at(n, port.at));
            if let Some(k) = node {
                at_comp.entry(*comp).or_default().push(k);
            }
            if !sys.kind.domains().contains(&port.domain)
                || port.system.is_some_and(|k| k != sys.kind)
            {
                issues.push(MepIssue::WrongSystem {
                    comp: *comp,
                    connector: connector.clone(),
                });
            }
            match node {
                Some(k) => {
                    if source {
                        is_source[k] = true;
                    } else {
                        load[k] += flow;
                    }
                    if let Some(n) = &net {
                        let run = n.nodes[k].arms.first().map(|a| a.0);
                        if let Some((r, s)) = run.and_then(|r| specs.get(&r).map(|s| (r, s))) {
                            let (port_mm, run_mm) = (port.size * 1000.0, nominal_mm(s));
                            if port_mm > run_mm + 1.0 {
                                issues.push(MepIssue::PortSize {
                                    comp: *comp,
                                    connector: connector.clone(),
                                    run: r,
                                    port_mm,
                                    run_mm,
                                });
                            }
                        }
                        let arm = n.nodes[k].arms.iter().find(|a| a.1 < 2).copied();
                        if let Some(((r, tag), s)) =
                            arm.and_then(|a| specs.get(&a.0).map(|s| (a, s)))
                        {
                            if !electric && skewed(&s.pts(), tag, v(port.dir)) {
                                issues.push(MepIssue::PortApproach {
                                    comp: *comp,
                                    connector: connector.clone(),
                                    run: r,
                                });
                            }
                        }
                    }
                }
                None if electric => {
                    if !source {
                        off_net += flow;
                    }
                }
                None => issues.push(MepIssue::NotConnected {
                    comp: *comp,
                    connector: connector.clone(),
                }),
            }
            devices.push(MepDeviceRow {
                comp: *comp,
                connector: connector.clone(),
                flow,
                source,
                connected: node.is_some() || electric,
            });
        }

        let mut runs: Vec<MepRunFlow> = ids
            .iter()
            .map(|&id| {
                let s = &specs[&id];
                let length: f64 = s
                    .points
                    .windows(2)
                    .map(|w| {
                        (w[1].0 - w[0].0)
                            .hypot(w[1].1 - w[0].1)
                            .hypot(w[1].2 - w[0].2)
                    })
                    .sum();
                MepRunFlow {
                    run: id,
                    size: s.size_label(),
                    length,
                    flow: 0.0,
                    speed: None,
                    insulation: s.section.insulation,
                }
            })
            .collect();
        let mut total = off_net;
        let (mut parts, mut fittings) = (ids.len().min(1), 0);
        let through: Vec<(usize, usize)> = at_comp
            .values()
            .filter(|k| k.len() == 2 && k[0] != k[1])
            .map(|k| (k[0], k[1]))
            .collect();
        if let Some(n) = &net {
            parts = n.parts_with(&through);
            fittings = n.fittings.len();
            if parts > 1 {
                issues.push(MepIssue::Broken { parts });
            }
            for (run, at) in &n.open_ends {
                let s = &specs[run];
                let pts = s.pts();
                let d0 = (pts[0].x - at.0)
                    .hypot(pts[0].y - at.1)
                    .hypot(pts[0].z - at.2);
                issues.push(MepIssue::OpenEnd {
                    run: *run,
                    end: u8::from(d0 > PORT_TOL),
                });
            }
            let mut edges: Vec<(usize, usize)> = n.edges.iter().map(|e| (e.1, e.2)).collect();
            edges.extend(&through);
            let score = root_scores(n, &specs, sys.kind, &is_source);
            let f = flows(n.nodes.len(), &edges, &load, &score);
            total += f.total;
            if f.has_loop {
                issues.push(MepIssue::Loop);
            }
            for (k, e) in n.edges.iter().enumerate() {
                if let Some(r) = runs.iter_mut().find(|r| r.run == e.0) {
                    r.flow = r.flow.max(f.edge[k]);
                }
            }
            if sys.kind == SystemKind::Sanitary {
                let mut reported = vec![];
                for (k, e) in n.edges.iter().enumerate() {
                    if reported.contains(&e.0) {
                        continue;
                    }
                    let (near, far) = if f.down[k] { (e.1, e.2) } else { (e.2, e.1) };
                    let pts = specs[&e.0].pts();
                    let path = path_between(&pts, v(n.nodes[near].at), v(n.nodes[far].at));
                    // going from the outlet, the elevation does not drop on any segment
                    if path.windows(2).any(|w| w[1].z < w[0].z - 1e-4) {
                        reported.push(e.0);
                        issues.push(MepIssue::SlopeReverse { run: e.0 });
                    }
                }
            }
        }
        if !ids.is_empty() && total <= 0.0 {
            let linked = devices.iter().any(|d: &MepDeviceRow| !d.source);
            issues.push(if linked {
                MepIssue::ZeroFlow
            } else {
                MepIssue::NoFlow
            });
        }
        for r in &mut runs {
            let s = &specs[&r.run];
            run_checks(r, s, sys.kind, &mut issues);
        }
        Ok(MepSystemCheck {
            system,
            name: sys.name,
            kind: sys.kind,
            total,
            length: runs.iter().map(|r| r.length).sum(),
            parts,
            fittings,
            runs,
            devices,
            issues,
        })
    }
}

fn v(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

/// Score of a node as root: the source; otherwise the open end of the largest section, and for
/// drainage the lowest among them (the low end of a dead-end branch is not yet an outlet).
fn root_scores(
    n: &MepNetwork,
    specs: &HashMap<i64, RunSpec>,
    kind: SystemKind,
    source: &[bool],
) -> Vec<(u8, f64, f64)> {
    n.nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            if source[i] {
                return (2, 0.0, 0.0);
            }
            let open = node.arms.len() == 1 && node.port.is_none();
            let area = node
                .arms
                .iter()
                .filter_map(|a| specs.get(&a.0))
                .map(|s| s.section.shape.area())
                .fold(0.0, f64::max);
            let low = if kind == SystemKind::Sanitary {
                -node.at.2
            } else {
                0.0
            };
            (u8::from(open), area, low)
        })
        .collect()
}

fn run_checks(r: &mut MepRunFlow, s: &RunSpec, kind: SystemKind, out: &mut Vec<MepIssue>) {
    let area = s.section.inner().area();
    match kind {
        SystemKind::SupplyAir | SystemKind::ReturnAir | SystemKind::ExhaustAir => {
            let v = r.flow / 3600.0 / area;
            r.speed = Some(v);
            if v > AIR_MAX {
                out.push(MepIssue::Undersized {
                    run: r.run,
                    speed: v,
                    limit: AIR_MAX,
                    suggest: suggest(s, r.flow / 3600.0 / AIR_MAX),
                });
            }
        }
        SystemKind::Power => {
            let amps = r.flow / VOLTS;
            if s.section.domain == Domain::Conduit {
                let d = s.section.shape.wh().0;
                let limit = if d < 0.025 {
                    CONDUIT_AMPS.0
                } else {
                    CONDUIT_AMPS.1
                };
                if amps > limit {
                    out.push(MepIssue::Overload {
                        run: r.run,
                        amps,
                        limit,
                    });
                }
            }
        }
        SystemKind::Sanitary => {
            let min = min_fall(kind, s);
            if let Some(pct) = min_slope(&s.points) {
                if pct + 1e-6 < min {
                    out.push(MepIssue::SlopeLow {
                        run: r.run,
                        pct,
                        min,
                    });
                }
                if pct > 1e-6 {
                    let d = s.section.inner().wh().0;
                    let capacity = half_full_capacity(d, pct);
                    if r.flow > capacity {
                        out.push(MepIssue::Overflow {
                            run: r.run,
                            flow: r.flow,
                            capacity,
                            suggest: suggest_gravity(s, r.flow, pct),
                        });
                    }
                }
            }
        }
        // the domestic water guideline does not apply to fire networks
        SystemKind::Sprinkler | SystemKind::MedicalGas => r.speed = Some(r.flow / 1000.0 / area),
        _ => {
            let v = r.flow / 1000.0 / area;
            r.speed = Some(v);
            if v > WATER_MAX {
                out.push(MepIssue::Undersized {
                    run: r.run,
                    speed: v,
                    limit: WATER_MAX,
                    suggest: suggest(s, r.flow / 1000.0 / WATER_MAX),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_accumulates_flow_from_root() {
        // 0 — 1 — 2, branch 1 — 3; root 0
        let edges = [(0, 1), (1, 2), (3, 1)];
        let load = [0.0, 0.0, 100.0, 50.0];
        let score = [(1, 1.0, 0.0), (0, 0.0, 0.0), (0, 0.0, 0.0), (0, 0.0, 0.0)];
        let f = flows(4, &edges, &load, &score);
        assert_eq!(f.edge, vec![150.0, 100.0, 50.0]);
        assert_eq!(f.total, 150.0);
        assert!(!f.has_loop);
        assert_eq!(f.down, vec![true, true, false]);
    }

    #[test]
    fn cycle_is_flagged_not_counted_twice() {
        let edges = [(0, 1), (1, 2), (2, 0)];
        let score = [(2, 0.0, 0.0), (0, 0.0, 0.0), (0, 0.0, 0.0)];
        let f = flows(3, &edges, &[0.0, 10.0, 10.0], &score);
        assert!(f.has_loop);
        assert_eq!(f.total, 20.0);
    }

    #[test]
    fn capacity_grows_with_size_and_slope() {
        assert!(half_full_capacity(0.1, 2.0) > half_full_capacity(0.05, 2.0));
        assert!(half_full_capacity(0.1, 2.0) > half_full_capacity(0.1, 1.0));
        // 100 mm bore at 2 %, half full: about 3.65 l/s
        assert!((half_full_capacity(0.1, 2.0) - 3.65).abs() < 0.05);
    }

    fn checked(kind: SystemKind, flow: f64, s: &RunSpec) -> (MepRunFlow, Vec<MepIssue>) {
        let mut r = MepRunFlow {
            run: 1,
            size: s.size_label(),
            length: 10.0,
            flow,
            speed: None,
            insulation: 0.0,
        };
        let mut out = vec![];
        run_checks(&mut r, s, kind, &mut out);
        (r, out)
    }

    #[test]
    fn three_wcs_on_dn100_at_two_percent_are_a_hint_not_an_error() {
        let pts = vec![(0.0, 0.0, 0.2), (10.0, 0.0, 0.0)];
        let s = RunSpec::pipe(1, 1, PipeMaterial::Pvc, 100, pts).unwrap();
        let (_, out) = checked(SystemKind::Sanitary, 3.0 * 1.6, &s);
        assert!(matches!(out[..], [MepIssue::Overflow { .. }]), "{out:?}");
        assert!(!out[0].is_error());
        assert_eq!(out[0].key(), "mep.issue.overflow");
    }

    #[test]
    fn four_basins_on_dn15_are_a_hint_and_sprinklers_have_no_speed_limit() {
        let pts = vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)];
        let s = RunSpec::pipe(1, 1, PipeMaterial::Steel, 15, pts).unwrap();
        let (_, out) = checked(SystemKind::ColdWater, 0.4, &s);
        assert!(matches!(out[..], [MepIssue::Undersized { .. }]), "{out:?}");
        assert!(!out[0].is_error());
        let (r, out) = checked(SystemKind::Sprinkler, 0.4, &s);
        assert!(out.is_empty(), "{out:?}");
        assert!(r.speed.is_some_and(|v| v > WATER_MAX));
    }

    #[test]
    fn missing_size_in_the_catalog_has_its_own_text() {
        let pts = vec![(0.0, 0.0, 0.2), (10.0, 0.0, 0.0)];
        let s = RunSpec::pipe(1, 1, PipeMaterial::Pvc, 100, pts).unwrap();
        let (_, out) = checked(SystemKind::Sanitary, 1.0e6, &s);
        assert_eq!(out[0].key(), "mep.issue.overflow_no_size");
        let pts = vec![(0.0, 0.0, 3.0), (10.0, 0.0, 3.0)];
        let s = RunSpec::pipe(1, 1, PipeMaterial::Steel, 15, pts).unwrap();
        let (_, out) = checked(SystemKind::ColdWater, 1.0e6, &s);
        assert_eq!(out[0].key(), "mep.issue.undersized_no_size");
    }

    #[test]
    fn path_between_follows_the_axis_both_ways() {
        let pts = [v3(0.0, 0.0, 0.0), v3(5.0, 0.0, 1.0), v3(10.0, 0.0, 0.0)];
        let (a, b) = (v3(1.0, 0.0, 0.2), v3(9.0, 0.0, 0.2));
        let there = path_between(&pts, a, b);
        assert_eq!(there.len(), 3);
        assert!((there[1] - pts[1]).len() < 1e-12);
        let back = path_between(&pts, b, a);
        assert!((back[0] - there[2]).len() < 1e-12);
        assert!((back[2] - there[0]).len() < 1e-12);
    }
}
