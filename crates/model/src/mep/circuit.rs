//! Electrical circuits (schema 48): a panel (a component with a power connector) feeds numbered
//! circuits, each a list of device connectors with a load. The physical carrier is the panel's
//! power network (conduits and trays); circuit cable lengths, currents and the voltage drop are
//! measured along it from the panel to each device. Single-phase drop 2·rho·sum(I·L)/S,
//! three-phase sqrt(3)·rho·sum(I·L)/S, power factor 1; copper.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use rusqlite::{params, Connection, OptionalExtension};

use super::{
    invalid, nearest_on, read_run, CableInput, CableRules, Conductor, MepNetwork, Standard,
};
use crate::ops::Inv;
use crate::schema::has_col;
use crate::{Error, Model};

const DDL: &str = "CREATE TABLE IF NOT EXISTS elec_circuit(id INTEGER PRIMARY KEY,
 panel INTEGER NOT NULL, number INTEGER NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL,
 system INTEGER, voltage REAL NOT NULL, poles INTEGER NOT NULL, breaker REAL NOT NULL,
 cable REAL NOT NULL, max_drop REAL NOT NULL, cos_phi REAL NOT NULL DEFAULT 0,
 laying TEXT NOT NULL DEFAULT 'conduit_wall', ambient_c REAL NOT NULL DEFAULT 30,
 grouped INTEGER NOT NULL DEFAULT 1, conductor TEXT NOT NULL DEFAULT 'cu',
 load_type TEXT NOT NULL DEFAULT '', UNIQUE(panel, number));
CREATE TABLE IF NOT EXISTS elec_circuit_dev(id INTEGER PRIMARY KEY, circuit INTEGER NOT NULL,
 comp INTEGER NOT NULL, connector TEXT NOT NULL, load REAL, UNIQUE(comp, connector));
CREATE INDEX IF NOT EXISTS elec_circuit_dev_circuit ON elec_circuit_dev(circuit);";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    // databases made before the power factor and laying columns
    if !has_col(db, "elec_circuit", "cos_phi")? {
        db.execute_batch(
            "ALTER TABLE elec_circuit ADD COLUMN cos_phi REAL NOT NULL DEFAULT 0;
             ALTER TABLE elec_circuit ADD COLUMN laying TEXT NOT NULL DEFAULT 'conduit_wall';",
        )?;
    }
    if !has_col(db, "elec_circuit", "ambient_c")? {
        db.execute_batch(
            "ALTER TABLE elec_circuit ADD COLUMN ambient_c REAL NOT NULL DEFAULT 30;
             ALTER TABLE elec_circuit ADD COLUMN grouped INTEGER NOT NULL DEFAULT 1;",
        )?;
    }
    if !has_col(db, "elec_circuit", "conductor")? {
        db.execute_batch(
            "ALTER TABLE elec_circuit ADD COLUMN conductor TEXT NOT NULL DEFAULT 'cu';
             ALTER TABLE elec_circuit ADD COLUMN load_type TEXT NOT NULL DEFAULT '';",
        )?;
    }
    Ok(())
}

/// What a circuit feeds: sockets and other power, or lighting (lights and switches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CircuitKind {
    Power,
    Lighting,
}

impl CircuitKind {
    pub fn name(self) -> &'static str {
        match self {
            CircuitKind::Power => "power",
            CircuitKind::Lighting => "lighting",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [CircuitKind::Power, CircuitKind::Lighting]
            .into_iter()
            .find(|k| k.name() == s)
    }
    /// Breaker, A, and cable, mm2, a circuit of this kind gets by default.
    pub fn defaults(self) -> (f64, f64) {
        match self {
            CircuitKind::Power => (16.0, 2.5),
            CircuitKind::Lighting => (10.0, 1.5),
        }
    }
}

/// A device on a circuit: component connector and its load, W (None: the family's value).
#[derive(Clone, Debug, PartialEq)]
pub struct CircuitDevice {
    pub comp: i64,
    pub connector: String,
    pub load: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CircuitSpec {
    pub panel: i64,
    /// Way number on the panel; 0 takes the next free one.
    pub number: u32,
    pub name: String,
    pub kind: CircuitKind,
    /// The power network the cables run in.
    pub system: Option<i64>,
    pub voltage: f64,
    /// 1 single-phase, 3 three-phase.
    pub poles: u32,
    pub breaker: f64,
    /// Conductor cross-section, mm2.
    pub cable: f64,
    /// Highest voltage drop allowed, %.
    pub max_drop: f64,
    /// Power factor of the load.
    pub cos_phi: f64,
    /// Load type of the cable table (SP 256), the source of the default power factor.
    pub load_type: String,
    pub conductor: Conductor,
    /// Laying method id of the cable table.
    pub laying: String,
    /// Ambient temperature along the cable, C.
    pub ambient_c: f64,
    /// Circuits laid bunched together with this one, itself included.
    pub grouped: u32,
    pub devices: Vec<CircuitDevice>,
}

impl CircuitSpec {
    pub fn new(panel: i64, kind: CircuitKind, devices: Vec<CircuitDevice>) -> Self {
        let (breaker, cable) = kind.defaults();
        let rules = CableRules::shared();
        CircuitSpec {
            panel,
            number: 0,
            name: String::new(),
            kind,
            system: None,
            voltage: 230.0,
            poles: 1,
            breaker,
            cable,
            max_drop: rules.max_drop(kind),
            cos_phi: rules.cos_phi(kind),
            load_type: rules.default_load_type(kind).to_string(),
            conductor: Conductor::Cu,
            laying: "conduit_wall".into(),
            ambient_c: rules.ambient_ref_c(),
            grouped: 1,
            devices,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Circuit {
    pub id: i64,
    pub spec: CircuitSpec,
}

/// A finding of the circuit check. Values in A, %, W.
#[derive(Clone, Debug, PartialEq)]
pub enum CircuitIssue {
    /// The load current is above the breaker rating.
    Overload { current: f64, breaker: f64 },
    /// The voltage drop to the farthest device is above the limit.
    Drop { pct: f64, max: f64, comp: i64 },
    /// No network path from the panel to the device.
    NotRouted { comp: i64 },
}

impl CircuitIssue {
    /// Message key `mep.circuit.*` for the view.
    pub fn key(&self) -> &'static str {
        match self {
            CircuitIssue::Overload { .. } => "mep.circuit.overload",
            CircuitIssue::Drop { .. } => "mep.circuit.drop",
            CircuitIssue::NotRouted { .. } => "mep.circuit.not_routed",
        }
    }
    pub fn args(&self) -> Vec<(&'static str, String)> {
        let f1 = |v: f64| format!("{v:.1}");
        match self {
            CircuitIssue::Overload { current, breaker } => {
                vec![("current", f1(*current)), ("breaker", f1(*breaker))]
            }
            CircuitIssue::Drop { pct, max, comp } => vec![
                ("pct", f1(*pct)),
                ("max", f1(*max)),
                ("comp", comp.to_string()),
            ],
            CircuitIssue::NotRouted { comp } => vec![("comp", comp.to_string())],
        }
    }
}

/// Measured circuit: load, current, cable to the farthest device, voltage drop.
#[derive(Clone, Debug, PartialEq)]
pub struct CircuitReport {
    pub id: i64,
    pub number: u32,
    pub name: String,
    pub kind: CircuitKind,
    pub poles: u32,
    pub breaker: f64,
    pub cable: f64,
    pub devices: usize,
    pub load: f64,
    pub current: f64,
    /// Cable path from the panel to the farthest device, m (0 when not routed).
    pub length: f64,
    pub drop: f64,
    pub issues: Vec<CircuitIssue>,
    /// Cable-table rule results (EL-*).
    pub rules: Vec<super::RuleFinding>,
    pub cos_phi: f64,
    pub laying: String,
    pub ambient_c: f64,
    pub grouped: u32,
    pub load_type: String,
    pub conductor: Conductor,
    /// The reactance of the section is an estimate (the standard has no value for it).
    pub x0_estimated: bool,
}

/// A panel schedule: one row per circuit and the totals.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelSchedule {
    pub panel: i64,
    pub rows: Vec<CircuitReport>,
    pub load: f64,
    /// Current of the busiest phase, A.
    pub current: f64,
    /// Current per phase L1..L3, A: three-pole circuits load all three, single-pole ones are
    /// spread over the phases, the biggest first onto the lightest phase.
    pub phases: [f64; 3],
}

/// Voltage drop, %, of an edited circuit: the measured drop of the saved one, scaled by what the
/// edit changes (poles, voltage, section, power factor, load). The route stays the measured one.
fn rescaled_drop(old: &CircuitSpec, rep: &CircuitReport, new: &CircuitSpec, load: f64) -> f64 {
    let rules = CableRules::shared();
    let unit = |s: &CircuitSpec, load: f64| {
        let amps = CableRules::current(s.poles, s.voltage, s.cos_phi, load);
        rules.volt_drop(s.conductor, s.poles, s.cable, s.cos_phi, amps) / s.voltage
    };
    let before = unit(old, rep.load);
    if before <= 0.0 {
        return 0.0;
    }
    rep.drop * unit(new, load) / before
}

/// The rule inputs of a circuit spec with `load` W over `length` m.
fn input(
    standard: Standard,
    s: &CircuitSpec,
    load: f64,
    length: f64,
    measured_drop: Option<f64>,
) -> CableInput {
    CableInput {
        kind: s.kind,
        standard,
        voltage: s.voltage,
        poles: s.poles,
        load,
        cos_phi: s.cos_phi,
        material: s.conductor,
        length,
        breaker: s.breaker,
        section: s.cable,
        laying: s.laying.clone(),
        ambient_c: s.ambient_c,
        grouped: s.grouped,
        max_drop: Some(s.max_drop),
        measured_drop,
    }
}

/// Rule results for an edited circuit before it is saved. `rep` is the report of the saved one.
pub fn preview_rules(
    standard: Standard,
    old: &CircuitSpec,
    rep: &CircuitReport,
    new: &CircuitSpec,
    load: f64,
) -> Vec<super::RuleFinding> {
    let measured = (rep.length > 0.0).then(|| rescaled_drop(old, rep, new, load));
    CableRules::shared().check(&input(standard, new, load, rep.length, measured))
}

/// Smallest breaker and section for an edited circuit (whole load at the full route length).
pub fn auto_size(
    standard: Standard,
    new: &CircuitSpec,
    load: f64,
    length: f64,
) -> Option<super::CableSized> {
    CableRules::shared().size(&input(standard, new, load, length, None))
}

/// Phase currents of a set of circuits (see `PanelSchedule::phases`).
pub fn phase_currents(rows: &[CircuitReport]) -> [f64; 3] {
    let mut ph = [0.0; 3];
    for r in rows.iter().filter(|r| r.poles == 3) {
        ph.iter_mut().for_each(|p| *p += r.current);
    }
    let mut one: Vec<&CircuitReport> = rows.iter().filter(|r| r.poles != 3).collect();
    one.sort_by(|a, b| b.current.total_cmp(&a.current).then(a.number.cmp(&b.number)));
    for r in one {
        let k = (0..3).min_by(|&a, &b| ph[a].total_cmp(&ph[b])).unwrap_or(0);
        ph[k] += r.current;
    }
    ph
}

fn spec_of(r: &rusqlite::Row) -> rusqlite::Result<(i64, CircuitSpec)> {
    let kind: String = r.get(4)?;
    Ok((
        r.get(0)?,
        CircuitSpec {
            panel: r.get(1)?,
            number: r.get::<_, i64>(2)? as u32,
            name: r.get(3)?,
            kind: CircuitKind::parse(&kind).unwrap_or(CircuitKind::Power),
            system: r.get(5)?,
            voltage: r.get(6)?,
            poles: r.get::<_, i64>(7)? as u32,
            breaker: r.get(8)?,
            cable: r.get(9)?,
            max_drop: r.get(10)?,
            cos_phi: {
                let c: f64 = r.get(11)?;
                // rows from before the column take the default of their kind
                if c > 0.0 {
                    c
                } else {
                    CableRules::shared()
                        .cos_phi(CircuitKind::parse(&kind).unwrap_or(CircuitKind::Power))
                }
            },
            laying: r.get(12)?,
            ambient_c: r.get(13)?,
            grouped: r.get::<_, i64>(14)?.max(1) as u32,
            conductor: Conductor::parse(&r.get::<_, String>(15)?).unwrap_or(Conductor::Cu),
            load_type: {
                let t: String = r.get(16)?;
                if t.is_empty() {
                    CableRules::shared()
                        .default_load_type(CircuitKind::parse(&kind).unwrap_or(CircuitKind::Power))
                        .to_string()
                } else {
                    t
                }
            },
            devices: vec![],
        },
    ))
}

const COLS: &str =
    "id,panel,number,name,kind,system,voltage,poles,breaker,cable,max_drop,cos_phi,laying,\
     ambient_c,grouped,conductor,load_type";

fn devices_of(db: &Connection, id: i64) -> Result<Vec<CircuitDevice>, Error> {
    let mut st = db.prepare_cached(
        "SELECT comp,connector,load FROM elec_circuit_dev WHERE circuit=?1 ORDER BY id",
    )?;
    let r = st.query_map([id], |r| {
        Ok(CircuitDevice {
            comp: r.get(0)?,
            connector: r.get(1)?,
            load: r.get(2)?,
        })
    })?;
    Ok(r.collect::<Result<_, _>>()?)
}

/// Writes a circuit inside an operation; the inverse removes it.
pub(super) fn insert_circuit(db: &Connection, s: &CircuitSpec) -> Result<(i64, Inv), Error> {
    check_spec(s)?;
    let panel: Option<i64> = db
        .query_row("SELECT id FROM comp WHERE id=?1", [s.panel], |r| r.get(0))
        .optional()?;
    if panel.is_none() {
        return invalid(format!("circuit: no panel component {}", s.panel));
    }
    insert_checked(db, s)
}

/// The numbers of a circuit: what both a new circuit and an edited one must satisfy.
fn check_spec(s: &CircuitSpec) -> Result<(), Error> {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    if !(ok(s.voltage) && ok(s.breaker) && ok(s.cable) && ok(s.max_drop))
        || !(s.poles == 1 || s.poles == 3)
    {
        return invalid("circuit: voltage, breaker, cable and drop above zero, poles 1 or 3");
    }
    if !(ok(s.cos_phi) && s.cos_phi <= 1.0) {
        return invalid("circuit: power factor above 0 and up to 1");
    }
    if s.poles == 3 && !(380.0..=420.0).contains(&s.voltage) {
        return invalid("circuit: three poles are a 400 V line voltage, not a phase voltage");
    }
    if !(s.ambient_c.is_finite() && (-40.0..=100.0).contains(&s.ambient_c)) || s.grouped == 0 {
        return invalid("circuit: ambient temperature -40..100 C and at least one circuit in a group");
    }
    if !s.load_type.is_empty() && CableRules::shared().cos_phi_of(&s.load_type).is_none() {
        return invalid(format!("circuit: unknown load type {}", s.load_type));
    }
    if !CableRules::shared().laying.iter().any(|l| l.id == s.laying) {
        return invalid(format!("circuit: unknown laying method {}", s.laying));
    }
    if s.devices.iter().any(|d| d.load.is_some_and(|l| !(l.is_finite() && l >= 0.0))) {
        return invalid("circuit: a load must be a number not below zero");
    }
    Ok(())
}

fn insert_checked(db: &Connection, s: &CircuitSpec) -> Result<(i64, Inv), Error> {
    let number = if s.number == 0 {
        db.query_row(
            "SELECT COALESCE(MAX(number),0)+1 FROM elec_circuit WHERE panel=?1",
            [s.panel],
            |r| r.get::<_, i64>(0),
        )?
    } else {
        s.number as i64
    };
    let taken: i64 = db.query_row(
        "SELECT count(*) FROM elec_circuit WHERE panel=?1 AND number=?2",
        params![s.panel, number],
        |r| r.get(0),
    )?;
    if taken > 0 {
        return invalid(format!("circuit: way {number} of panel {} is taken", s.panel));
    }
    let name = if s.name.trim().is_empty() {
        format!("{}-{number}", s.panel)
    } else {
        s.name.clone()
    };
    db.execute(
        "INSERT INTO elec_circuit(panel,number,name,kind,system,voltage,poles,breaker,cable,\
         max_drop,cos_phi,laying,ambient_c,grouped,conductor,load_type) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![
            s.panel,
            number,
            name,
            s.kind.name(),
            s.system,
            s.voltage,
            s.poles,
            s.breaker,
            s.cable,
            s.max_drop,
            s.cos_phi,
            s.laying,
            s.ambient_c,
            s.grouped,
            s.conductor.name(),
            s.load_type
        ],
    )?;
    let id = db.last_insert_rowid();
    let mut inv = Inv::default()
        .delete("elec_circuit", "id", id)
        .delete("elec_circuit_dev", "circuit", id);
    for d in &s.devices {
        // a device moves to the new circuit: the old row comes back on undo
        if let Some(old) = db
            .query_row(
                "SELECT id FROM elec_circuit_dev WHERE comp=?1 AND connector=?2",
                params![d.comp, d.connector],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            inv = Inv::default()
                .restore(db, "elec_circuit_dev", "id", old)?
                .then(inv);
            db.execute("DELETE FROM elec_circuit_dev WHERE id=?1", [old])?;
        }
        db.execute(
            "INSERT INTO elec_circuit_dev(circuit,comp,connector,load) VALUES (?1,?2,?3,?4)",
            params![id, d.comp, d.connector, d.load],
        )?;
    }
    Ok((id, inv))
}

/// Circuits of deleted panels and devices of deleted components go in the same entry.
pub(crate) fn prune(db: &Connection) -> Result<Inv, Error> {
    let mut inv = Inv::default();
    let gone: Vec<i64> = db
        .prepare_cached("SELECT id FROM elec_circuit WHERE panel NOT IN (SELECT id FROM comp)")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in gone {
        inv = inv
            .restore(db, "elec_circuit", "id", id)?
            .restore(db, "elec_circuit_dev", "circuit", id)?;
        db.execute("DELETE FROM elec_circuit_dev WHERE circuit=?1", [id])?;
        db.execute("DELETE FROM elec_circuit WHERE id=?1", [id])?;
    }
    let devs: Vec<i64> = db
        .prepare_cached(
            "SELECT id FROM elec_circuit_dev WHERE comp NOT IN (SELECT id FROM comp)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for id in devs {
        inv = inv.restore(db, "elec_circuit_dev", "id", id)?;
        db.execute("DELETE FROM elec_circuit_dev WHERE id=?1", [id])?;
    }
    db.execute(
        "UPDATE elec_circuit SET system=NULL WHERE system IS NOT NULL \
         AND system NOT IN (SELECT id FROM mep_system)",
        [],
    )?;
    Ok(inv)
}

/// Length of a run between two of its points, along its axis.
fn along(pts: &[ddc_geom::math::V3], a: ddc_geom::math::V3, b: ddc_geom::math::V3) -> f64 {
    let pos = |p| {
        let (q, k, _) = nearest_on(pts, p);
        let before: f64 = pts[..=k].windows(2).map(|w| (w[1] - w[0]).len()).sum();
        before + (q - pts[k]).len()
    };
    (pos(a) - pos(b)).abs()
}

/// Shortest-path tree of a network from one node: (parent node, edge length) per node.
fn tree_from(
    m: &Model,
    net: &MepNetwork,
    root: usize,
) -> Result<Vec<Option<(usize, f64)>>, Error> {
    let mut adj: Vec<Vec<(usize, f64)>> = vec![vec![]; net.nodes.len()];
    let mut pts: HashMap<i64, Vec<ddc_geom::math::V3>> = HashMap::new();
    for &(run, a, b) in &net.edges {
        if let std::collections::hash_map::Entry::Vacant(e) = pts.entry(run) {
            e.insert(read_run(&m.db, run)?.pts());
        }
        let p = &pts[&run];
        let v = |q: (f64, f64, f64)| ddc_geom::math::v3(q.0, q.1, q.2);
        let l = along(p, v(net.nodes[a].at), v(net.nodes[b].at));
        adj[a].push((b, l));
        adj[b].push((a, l));
    }
    Ok(shortest_tree(&adj, root))
}

/// Dijkstra over an adjacency list of (neighbour, length): (parent, edge length) per node, None
/// for the root and for nodes it does not reach. Lengths must not be negative; they order as
/// their bit patterns.
pub(crate) fn shortest_tree(adj: &[Vec<(usize, f64)>], root: usize) -> Vec<Option<(usize, f64)>> {
    let mut dist = vec![f64::INFINITY; adj.len()];
    let mut parent: Vec<Option<(usize, f64)>> = vec![None; adj.len()];
    dist[root] = 0.0;
    let mut q = BinaryHeap::from([Reverse((0f64.to_bits(), root))]);
    while let Some(Reverse((d, u))) = q.pop() {
        if d > dist[u].to_bits() {
            continue;
        }
        for &(w, l) in &adj[u] {
            let nd = dist[u] + l;
            if nd < dist[w] - 1e-9 {
                dist[w] = nd;
                parent[w] = Some((u, l));
                q.push(Reverse((nd.to_bits(), w)));
            }
        }
    }
    parent
}

impl Model {
    /// A circuit with its devices: one operation.
    pub fn add_circuit(&mut self, s: &CircuitSpec) -> Result<i64, Error> {
        self.op("elec_circuit_add", |db| {
            let (id, inv) = insert_circuit(db, s)?;
            Ok((id, id, inv))
        })
    }

    /// Replaces the electrical properties of a circuit and the load overrides of its devices,
    /// one undo step. Panel, way number, system and the device list stay.
    pub fn update_circuit(&mut self, id: i64, s: &CircuitSpec) -> Result<(), Error> {
        check_spec(s)?;
        if s.name.trim().is_empty() {
            return invalid("circuit: the name must not be empty");
        }
        self.op("elec_circuit_update", |db| {
            let inv = Inv::default()
                .restore(db, "elec_circuit", "id", id)?
                .restore(db, "elec_circuit_dev", "circuit", id)?;
            let n = db.execute(
                "UPDATE elec_circuit SET name=?1,kind=?2,voltage=?3,poles=?4,breaker=?5,cable=?6,\
                 max_drop=?7,cos_phi=?8,laying=?9,ambient_c=?10,grouped=?11,conductor=?12,\
                 load_type=?13 WHERE id=?14",
                params![
                    s.name.trim(),
                    s.kind.name(),
                    s.voltage,
                    s.poles,
                    s.breaker,
                    s.cable,
                    s.max_drop,
                    s.cos_phi,
                    s.laying,
                    s.ambient_c,
                    s.grouped,
                    s.conductor.name(),
                    s.load_type,
                    id
                ],
            )?;
            if n == 0 {
                return invalid(format!("no circuit {id}"));
            }
            for d in &s.devices {
                db.execute(
                    "UPDATE elec_circuit_dev SET load=?1 WHERE circuit=?2 AND comp=?3 \
                     AND connector=?4",
                    params![d.load, id, d.comp, d.connector],
                )?;
            }
            Ok(((), id, inv))
        })
    }

    /// Per device of a circuit: the load in use and the family's own value, W.
    pub fn circuit_device_loads(&self, c: &Circuit) -> Result<Vec<(f64, f64)>, Error> {
        c.spec
            .devices
            .iter()
            .map(|d| {
                let fam = self.mep_port_flow(d.comp, &d.connector)?;
                Ok((d.load.unwrap_or(fam), fam))
            })
            .collect()
    }

    /// The ampacity table family of the project; PUE until set.
    pub fn elec_standard(&self) -> Result<Standard, Error> {
        let v: Option<i64> = self
            .db
            .query_row("SELECT value FROM meta WHERE key='elec_standard'", [], |r| r.get(0))
            .optional()?;
        Ok(if v == Some(1) { Standard::Iec } else { Standard::Pue })
    }

    /// Sets the table family, one undo step.
    pub fn set_elec_standard(&mut self, s: Standard) -> Result<(), Error> {
        let v = (s == Standard::Iec) as i64;
        self.op("elec_standard", |db| {
            let old: Option<i64> = db
                .query_row("SELECT value FROM meta WHERE key='elec_standard'", [], |r| r.get(0))
                .optional()?;
            let inv = match old {
                Some(o) => Inv::default().sql(
                    "UPDATE meta SET value=?1 WHERE key='elec_standard'",
                    vec![rusqlite::types::Value::Integer(o)],
                ),
                None => Inv::default().sql("DELETE FROM meta WHERE key='elec_standard'", vec![]),
            };
            db.execute(
                "INSERT INTO meta(key,value) VALUES ('elec_standard',?1) \
                 ON CONFLICT(key) DO UPDATE SET value=?1",
                [v],
            )?;
            Ok(((), 0, inv))
        })
    }

    pub fn delete_circuit(&mut self, id: i64) -> Result<(), Error> {
        self.op("elec_circuit_delete", |db| {
            let inv = Inv::default()
                .restore(db, "elec_circuit", "id", id)?
                .restore(db, "elec_circuit_dev", "circuit", id)?;
            db.execute("DELETE FROM elec_circuit_dev WHERE circuit=?1", [id])?;
            if db.execute("DELETE FROM elec_circuit WHERE id=?1", [id])? == 0 {
                return invalid(format!("no circuit {id}"));
            }
            Ok(((), id, inv))
        })
    }

    /// Circuits of one panel (all at None), by panel and way number.
    pub fn circuits(&self, panel: Option<i64>) -> Result<Vec<Circuit>, Error> {
        let rows: Vec<(i64, CircuitSpec)> = self
            .db
            .prepare(&format!(
                "SELECT {COLS} FROM elec_circuit WHERE ?1 IS NULL OR panel=?1 \
                 ORDER BY panel, number"
            ))?
            .query_map([panel], spec_of)?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(|(id, mut spec)| {
                spec.devices = devices_of(&self.db, id)?;
                Ok(Circuit { id, spec })
            })
            .collect()
    }

    /// Panels with circuits, by id.
    pub fn panels(&self) -> Result<Vec<i64>, Error> {
        Ok(self
            .db
            .prepare("SELECT DISTINCT panel FROM elec_circuit ORDER BY panel")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// Load of one device connector, W: the circuit's value or the family's.
    fn device_load(&self, d: &CircuitDevice) -> Result<f64, Error> {
        match d.load {
            Some(l) => Ok(l),
            None => self.mep_port_flow(d.comp, &d.connector),
        }
    }

    /// Measures one circuit along its network.
    pub fn circuit_report(&self, c: &Circuit) -> Result<CircuitReport, Error> {
        let mut r = self.circuit_report_base(c)?;
        let s = &c.spec;
        // the drop measured along the network is the one drop; the rule judges that number
        r.rules = CableRules::shared().check(&CableInput {
            kind: s.kind,
            standard: self.elec_standard()?,
            voltage: s.voltage,
            poles: s.poles,
            load: r.load,
            cos_phi: s.cos_phi,
            material: s.conductor,
            length: r.length,
            breaker: s.breaker,
            section: s.cable,
            laying: s.laying.clone(),
            ambient_c: s.ambient_c,
            grouped: s.grouped,
            max_drop: Some(s.max_drop),
            measured_drop: (r.length > 0.0).then_some(r.drop),
        });
        Ok(r)
    }

    fn circuit_report_base(&self, c: &Circuit) -> Result<CircuitReport, Error> {
        let s = &c.spec;
        let loads: Vec<f64> = s
            .devices
            .iter()
            .map(|d| self.device_load(d))
            .collect::<Result<_, _>>()?;
        let load: f64 = loads.iter().sum();
        let rules = CableRules::shared();
        let amps = |w: f64| CableRules::current(s.poles, s.voltage, s.cos_phi, w);
        let current = amps(load);
        let mut out = CircuitReport {
            id: c.id,
            number: s.number,
            name: s.name.clone(),
            kind: s.kind,
            poles: s.poles,
            breaker: s.breaker,
            cable: s.cable,
            devices: s.devices.len(),
            load,
            current,
            length: 0.0,
            drop: 0.0,
            issues: vec![],
            rules: vec![],
            cos_phi: s.cos_phi,
            laying: s.laying.clone(),
            ambient_c: s.ambient_c,
            grouped: s.grouped,
            load_type: s.load_type.clone(),
            conductor: s.conductor,
            x0_estimated: rules.x0(s.conductor, s.cable).1,
        };
        if current > s.breaker + 1e-9 {
            out.issues.push(CircuitIssue::Overload {
                current,
                breaker: s.breaker,
            });
        }
        let net = match s.system {
            Some(sys) => self.mep_network(Some(sys)).ok(),
            None => None,
        };
        let root = net.as_ref().and_then(|n| {
            n.nodes
                .iter()
                .position(|x| x.port.as_ref().is_some_and(|p| p.0 == s.panel))
        });
        let (Some(net), Some(root)) = (net, root) else {
            for d in &s.devices {
                out.issues.push(CircuitIssue::NotRouted { comp: d.comp });
            }
            return Ok(out);
        };
        let parent = tree_from(self, &net, root)?;
        // current through each node's edge to its parent, from this circuit's devices only
        let mut through = vec![0.0; net.nodes.len()];
        let mut ends: Vec<(i64, usize)> = vec![];
        for (d, l) in s.devices.iter().zip(&loads) {
            let node = net.nodes.iter().position(|x| {
                x.port
                    .as_ref()
                    .is_some_and(|p| p.0 == d.comp && p.1 == d.connector)
            });
            let Some(mut u) = node.filter(|&u| u == root || parent[u].is_some()) else {
                out.issues.push(CircuitIssue::NotRouted { comp: d.comp });
                continue;
            };
            ends.push((d.comp, u));
            let i = amps(*l);
            while let Some((p, _)) = parent[u] {
                through[u] += i;
                u = p;
            }
        }
        let mut worst: Option<(f64, i64)> = None;
        for (comp, mut u) in ends {
            let (mut len, mut il) = (0.0, 0.0);
            while let Some((p, l)) = parent[u] {
                len += l;
                il += through[u] * l;
                u = p;
            }
            out.length = out.length.max(len);
            let pct = rules.volt_drop(s.conductor, s.poles, s.cable, s.cos_phi, il) / s.voltage
                * 100.0;
            if worst.is_none_or(|w| pct > w.0) {
                worst = Some((pct, comp));
            }
        }
        if let Some((pct, comp)) = worst {
            out.drop = pct;
            if pct > s.max_drop + 1e-9 {
                out.issues.push(CircuitIssue::Drop {
                    pct,
                    max: s.max_drop,
                    comp,
                });
            }
        }
        Ok(out)
    }

    /// Network route length from the panel to each device of a circuit, m (None: not routed).
    pub(super) fn circuit_device_lengths(
        &self,
        c: &Circuit,
    ) -> Result<Vec<(i64, Option<f64>)>, Error> {
        let s = &c.spec;
        let none = || s.devices.iter().map(|d| (d.comp, None)).collect();
        let Some(sys) = s.system else { return Ok(none()) };
        let Ok(net) = self.mep_network(Some(sys)) else { return Ok(none()) };
        let Some(root) = net
            .nodes
            .iter()
            .position(|x| x.port.as_ref().is_some_and(|p| p.0 == s.panel))
        else {
            return Ok(none());
        };
        let parent = tree_from(self, &net, root)?;
        let mut out = vec![];
        for d in &s.devices {
            let node = net.nodes.iter().position(|x| {
                x.port
                    .as_ref()
                    .is_some_and(|p| p.0 == d.comp && p.1 == d.connector)
            });
            let len = node.filter(|&u| u == root || parent[u].is_some()).map(|mut u| {
                let mut len = 0.0;
                while let Some((p, l)) = parent[u] {
                    len += l;
                    u = p;
                }
                len
            });
            out.push((d.comp, len));
        }
        Ok(out)
    }

    /// Panel schedule: every circuit measured, and the totals.
    pub fn panel_schedule(&self, panel: i64) -> Result<PanelSchedule, Error> {
        let rows = self
            .circuits(Some(panel))?
            .iter()
            .map(|c| self.circuit_report(c))
            .collect::<Result<Vec<_>, _>>()?;
        let load = rows.iter().map(|r| r.load).sum();
        let phases = phase_currents(&rows);
        Ok(PanelSchedule {
            panel,
            current: phases.iter().copied().fold(0.0, f64::max),
            phases,
            load,
            rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(number: u32, poles: u32, current: f64) -> CircuitReport {
        CircuitReport {
            id: number as i64,
            number,
            name: String::new(),
            kind: CircuitKind::Power,
            poles,
            breaker: 63.0,
            cable: 10.0,
            devices: 1,
            load: 0.0,
            current,
            length: 0.0,
            drop: 0.0,
            issues: vec![],
            rules: vec![],
            cos_phi: 0.9,
            laying: "conduit_wall".into(),
            ambient_c: 30.0,
            grouped: 1,
            load_type: "office".into(),
            conductor: Conductor::Cu,
            x0_estimated: false,
        }
    }

    #[test]
    fn panel_current_is_per_phase() {
        // three single-pole circuits of 10 A sit on three phases: the busiest phase carries
        // 10 A, not the 30 A of a plain sum
        let ph = phase_currents(&[row(1, 1, 10.0), row(2, 1, 10.0), row(3, 1, 10.0)]);
        assert_eq!(ph, [10.0, 10.0, 10.0]);
        // a 12 A three-pole circuit loads every phase: 12 + (10 | 8 | 6) with the 10 A first
        let ph = phase_currents(&[row(1, 3, 12.0), row(2, 1, 8.0), row(3, 1, 10.0), row(4, 1, 6.0)]);
        assert_eq!(ph, [22.0, 20.0, 18.0]);
    }

    /// Total length from a node up to the root.
    fn up(parent: &[Option<(usize, f64)>], mut u: usize) -> f64 {
        let mut len = 0.0;
        while let Some((p, l)) = parent[u] {
            len += l;
            u = p;
        }
        len
    }

    #[test]
    fn shortest_tree_takes_the_short_way_round() {
        // 0-1 is 10 long, but 0-2-1 is 3 + 4 = 7; node 3 hangs off 1 (2 more); 4 is cut off
        let e = |a: usize, b: usize, l: f64, adj: &mut Vec<Vec<(usize, f64)>>| {
            adj[a].push((b, l));
            adj[b].push((a, l));
        };
        let mut adj = vec![vec![]; 5];
        e(0, 1, 10.0, &mut adj);
        e(0, 2, 3.0, &mut adj);
        e(2, 1, 4.0, &mut adj);
        e(1, 3, 2.0, &mut adj);
        let t = shortest_tree(&adj, 0);
        assert_eq!(t[0], None);
        assert_eq!(t[1], Some((2, 4.0)));
        assert_eq!(up(&t, 1), 7.0);
        assert_eq!(up(&t, 3), 9.0);
        assert_eq!(t[4], None, "unreached");
        // a cheaper way found late replaces the first one: 0-1 7, 0-2 2, 2-1 1 beats it to 3
        let mut adj = vec![vec![]; 3];
        e(0, 1, 7.0, &mut adj);
        e(0, 2, 2.0, &mut adj);
        e(2, 1, 1.0, &mut adj);
        assert_eq!(up(&shortest_tree(&adj, 0), 1), 3.0);
    }

    #[test]
    fn old_databases_gain_the_new_columns_with_defaults() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE elec_circuit(id INTEGER PRIMARY KEY, panel INTEGER NOT NULL,
             number INTEGER NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, system INTEGER,
             voltage REAL NOT NULL, poles INTEGER NOT NULL, breaker REAL NOT NULL,
             cable REAL NOT NULL, max_drop REAL NOT NULL, UNIQUE(panel, number));
             INSERT INTO elec_circuit VALUES (1,5,1,'a','power',NULL,230,1,16,2.5,4);",
        )
        .unwrap();
        migrate(&db).unwrap();
        migrate(&db).unwrap();
        let (cos, lay, amb, grp): (f64, String, f64, i64) = db
            .query_row(
                "SELECT cos_phi,laying,ambient_c,grouped FROM elec_circuit",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        // 0 stands for "the default of the kind", read back as the table value
        assert_eq!((cos, lay.as_str(), amb, grp), (0.0, "conduit_wall", 30.0, 1));
        let (_, spec) = db
            .query_row(&format!("SELECT {COLS} FROM elec_circuit"), [], spec_of)
            .unwrap();
        assert_eq!(spec.cos_phi, 0.85);
        assert_eq!((spec.load_type.as_str(), spec.conductor), ("office", Conductor::Cu));
        assert_eq!((spec.ambient_c, spec.grouped), (30.0, 1));
    }

    #[test]
    fn default_limits_come_from_the_table() {
        let c = CircuitSpec::new(1, CircuitKind::Lighting, vec![]);
        // SP 256: 3 % to lighting; fluorescent lighting cos 0.92
        assert_eq!((c.max_drop, c.cos_phi, c.poles, c.voltage), (3.0, 0.92, 1, 230.0));
        assert_eq!(c.load_type, "light_fluorescent");
        let p = CircuitSpec::new(1, CircuitKind::Power, vec![]);
        assert_eq!((p.max_drop, p.cos_phi), (4.0, 0.85));
        assert_eq!((p.ambient_c, p.grouped), (30.0, 1));
    }
}
