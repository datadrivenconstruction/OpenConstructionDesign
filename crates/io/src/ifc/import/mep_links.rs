//! MEP connections read back from IFC: IfcDistributionPort nested in a network segment or a
//! component (IfcRelNests, or IfcRelConnectsPortToElement in IFC2X3), joined by
//! IfcRelConnectsPorts. Run-to-run joins are geometric in the model (ends that meet), so they are
//! checked, not stored; component ports are stored through [`DevicePortSink`]. A reference that
//! does not resolve becomes a report warning, never a failure.

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_model::{Error, Snapshots};
use rusqlite::{params, Connection, OptionalExtension};

use super::{PMep, Plan, Rd};
use crate::ifc::reader::StepFile;

/// How far a port may lie from its run end in the file, m.
const END_TOL: f64 = 0.05;
/// Ends that meet in the model, m (a little looser than the network's own join).
const JOIN_TOL: f64 = 0.01;

/// One end of a connection.
#[derive(Debug, Clone, PartialEq)]
pub enum PEnd {
    /// End `end` (0 first point, 1 last point) of the network run `run` (index in `Plan::meps`).
    Run { run: usize, end: u8 },
    /// Connector of the component `comp` (index in `Plan::comps`), with the system name and flow
    /// the file states for it.
    Port {
        comp: usize,
        connector: String,
        system: String,
        flow: Option<f64>,
    },
}

/// IfcRelConnectsPorts between two resolved ends.
#[derive(Debug, Clone, PartialEq)]
pub struct PMepLink {
    pub a: PEnd,
    pub b: PEnd,
}

/// Entities of the plan that ports can belong to.
pub(super) struct Known<'a> {
    /// segment entity → index in `Plan::meps`
    pub runs: &'a HashMap<u32, usize>,
    /// component entity → index in `Plan::comps`
    pub comps: &'a HashMap<u32, usize>,
    /// fittings are derived from the runs: their ports carry nothing to read
    pub fittings: &'a HashSet<u32>,
}

enum Side {
    End(PEnd),
    Derived,
    Bad(&'static str),
}

/// Distance between a port at `p` and the ends of a run; the nearer end if within [`END_TOL`].
fn run_end(m: &PMep, p: [f64; 3]) -> Option<u8> {
    let (a, b) = (m.spec.points.first()?, m.spec.points.last()?);
    let d = |q: &(f64, f64, f64)| {
        ((p[0] - q.0).powi(2) + (p[1] - q.1).powi(2) + (p[2] - q.2).powi(2)).sqrt()
    };
    let (da, db) = (d(a), d(b));
    (da.min(db) <= END_TOL).then_some(if da <= db { 0 } else { 1 })
}

/// Number of `key` in the property sets `set` of product `id`.
fn pset_num(
    f: &StepFile,
    props: &HashMap<u32, Vec<u32>>,
    id: u32,
    set: &str,
    key: &str,
) -> Option<f64> {
    for ps in props.get(&id).into_iter().flatten() {
        let Some(pe) = f.leaf(*ps) else { continue };
        if f.type_of(*ps) != Some("IFCPROPERTYSET") || Rd::at(&pe, 2).as_str() != Some(set) {
            continue;
        }
        for p in Rd::refs_at(&pe, 4) {
            let Some(pv) = f.leaf(p) else { continue };
            if Rd::at(&pv, 0).as_str() == Some(key) {
                // the value text is also accepted: `IFCLABEL('2.5')`
                let v = Rd::at(&pv, 2);
                return v.as_f64().or_else(|| v.as_str()?.trim().parse().ok());
            }
        }
    }
    None
}

fn pset_text(
    f: &StepFile,
    props: &HashMap<u32, Vec<u32>>,
    id: u32,
    set: &str,
    key: &str,
) -> Option<String> {
    for ps in props.get(&id).into_iter().flatten() {
        let Some(pe) = f.leaf(*ps) else { continue };
        if f.type_of(*ps) != Some("IFCPROPERTYSET") || Rd::at(&pe, 2).as_str() != Some(set) {
            continue;
        }
        for p in Rd::refs_at(&pe, 4) {
            let Some(pv) = f.leaf(p) else { continue };
            if Rd::at(&pv, 0).as_str() == Some(key) {
                return Rd::at(&pv, 2).as_str().map(str::to_string);
            }
        }
    }
    None
}

/// World position of a port (its placement origin).
fn port_at(rd: &mut Rd, port: u32) -> Option<[f64; 3]> {
    let f = rd.f;
    let e = f.get(port)?;
    rd.product_frame(e).ok().map(|fr| fr.o)
}

/// Connections of the file between the runs and components of the plan.
pub(super) fn read(
    rd: &mut Rd,
    props: &HashMap<u32, Vec<u32>>,
    known: &Known,
    meps: &[PMep],
    warnings: &mut Vec<String>,
) -> Vec<PMepLink> {
    let f = rd.f;
    // port → (owner, position in the owner's port list)
    let mut owner: HashMap<u32, (u32, usize)> = HashMap::new();
    let mut rels: Vec<(u32, Option<u32>, Option<u32>)> = vec![];
    let wanted = [
        "IFCRELNESTS",
        "IFCRELCONNECTSPORTTOELEMENT",
        "IFCRELCONNECTSPORTS",
    ];
    for (id, ty) in f.iter().filter(|x| wanted.contains(&x.1)) {
        let Some(a) = f.leaf(id) else { continue };
        match ty {
            "IFCRELNESTS" => {
                let Some(parent) = Rd::at(&a, 4).as_ref() else {
                    continue;
                };
                let ports = Rd::refs_at(&a, 5);
                for (i, p) in ports.into_iter().enumerate() {
                    if f.type_of(p) == Some("IFCDISTRIBUTIONPORT") {
                        owner.entry(p).or_insert((parent, i));
                    }
                }
            }
            "IFCRELCONNECTSPORTTOELEMENT" => {
                if let (Some(p), Some(e)) = (Rd::at(&a, 4).as_ref(), Rd::at(&a, 5).as_ref()) {
                    owner.entry(p).or_insert((e, usize::MAX));
                }
            }
            "IFCRELCONNECTSPORTS" => {
                rels.push((id, Rd::at(&a, 4).as_ref(), Rd::at(&a, 5).as_ref()));
            }
            _ => {}
        }
    }
    if rels.is_empty() {
        return vec![];
    }
    let side = |rd: &mut Rd, port: Option<u32>| -> Side {
        let Some(port) = port else {
            return Side::Bad("a port reference is missing");
        };
        match f.type_of(port) {
            None => return Side::Bad("the port does not exist"),
            Some("IFCDISTRIBUTIONPORT") => {}
            Some(_) => return Side::Bad("the connected entity is not a port"),
        }
        let Some(&(parent, idx)) = owner.get(&port) else {
            return Side::Bad("the port is not nested in an element");
        };
        if known.fittings.contains(&parent) {
            return Side::Derived;
        }
        if let Some(&run) = known.runs.get(&parent) {
            let m = &meps[run];
            let end = match port_at(rd, port) {
                Some(p) => run_end(m, p),
                // no readable placement: the first two ports of a segment are its ends
                None => (idx < 2).then_some(idx as u8),
            };
            return match end {
                Some(end) => Side::End(PEnd::Run { run, end }),
                None => Side::Bad("the port is not at an end of its segment"),
            };
        }
        if let Some(&comp) = known.comps.get(&parent) {
            let Some(connector) = pset_text(f, props, port, "DDC_MepPort", "Connector") else {
                return Side::Bad("a component port has no connector name");
            };
            return Side::End(PEnd::Port {
                comp,
                connector,
                system: pset_text(f, props, port, "DDC_MepPort", "System").unwrap_or_default(),
                flow: pset_num(f, props, port, "DDC_MepPort", "Flow"),
            });
        }
        Side::Bad("the port belongs to an element that is not an imported network element")
    };
    let mut out: Vec<PMepLink> = vec![];
    let mut skipped: BTreeMap<&'static str, (usize, u32)> = BTreeMap::new();
    let mut derived = 0usize;
    for (rel, a, b) in rels {
        let (sa, sb) = (side(rd, a), side(rd, b));
        let mut bad = |why: &'static str| {
            skipped.entry(why).or_insert((0, rel)).0 += 1;
        };
        match (sa, sb) {
            (Side::Bad(w), _) | (_, Side::Bad(w)) => bad(w),
            (Side::Derived, _) | (_, Side::Derived) => derived += 1,
            (Side::End(x), Side::End(y)) => {
                if x == y {
                    bad("a port is connected to itself");
                } else if out
                    .iter()
                    .any(|l| (l.a == x && l.b == y) || (l.a == y && l.b == x))
                {
                    bad("the same connection is stated twice");
                } else {
                    out.push(PMepLink { a: x, b: y });
                }
            }
        }
    }
    for (why, (n, first)) in skipped {
        warnings.push(format!(
            "IfcRelConnectsPorts: {n} connection(s) skipped, {why} (first: #{first})"
        ));
    }
    let _ = derived; // connections of fittings follow from the runs
    out
}

/// Where a component's connector link is stored. The model with manual networks keeps them in
/// `mep_port`; a model without that table cannot hold them.
pub(super) trait DevicePortSink {
    /// Links (`comp`, `connector`) to `system`; `flow` replaces the family value when given.
    /// Ok(false): this model cannot store device links.
    fn link(
        &mut self,
        db: &Connection,
        comp: i64,
        connector: &str,
        system: i64,
        flow: Option<f64>,
    ) -> Result<bool, Error>;
}

/// Writes `mep_port` rows (comp, connector, system, flow, stub) when the table exists. Rows
/// that change are snapshotted, new ones are removed by undo.
pub(super) struct StoredPorts<'a> {
    snaps: &'a mut Snapshots,
    table: Option<bool>,
}

impl<'a> StoredPorts<'a> {
    pub(super) fn new(snaps: &'a mut Snapshots) -> Self {
        StoredPorts { snaps, table: None }
    }
}

impl DevicePortSink for StoredPorts<'_> {
    fn link(
        &mut self,
        db: &Connection,
        comp: i64,
        connector: &str,
        system: i64,
        flow: Option<f64>,
    ) -> Result<bool, Error> {
        let ready = match self.table {
            Some(t) => t,
            None => {
                let n: i64 = db.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='mep_port'",
                    [],
                    |r| r.get(0),
                )?;
                if n > 0 {
                    self.snaps.watermark(db, "mep_port")?;
                }
                self.table = Some(n > 0);
                n > 0
            }
        };
        if !ready {
            return Ok(false);
        }
        self.snaps.save(db, "mep_port", "comp", comp)?;
        let row: Option<i64> = db
            .query_row(
                "SELECT id FROM mep_port WHERE comp=?1 AND connector=?2",
                params![comp, connector],
                |r| r.get(0),
            )
            .optional()?;
        match row {
            Some(id) => {
                db.execute(
                    "UPDATE mep_port SET system=?1, flow=COALESCE(?2, flow) WHERE id=?3",
                    params![system, flow, id],
                )?;
            }
            None => {
                db.execute(
                    "INSERT INTO mep_port(comp,connector,system,flow) VALUES (?1,?2,?3,?4)",
                    params![comp, connector, system, flow],
                )?;
            }
        }
        Ok(true)
    }
}

fn end_point(m: &PMep, end: u8) -> Option<(f64, f64, f64)> {
    if end == 0 {
        m.spec.points.first().copied()
    } else {
        m.spec.points.last().copied()
    }
}

fn dist(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) + (a.2 - b.2).powi(2)).sqrt()
}

/// Distance from `p` to the polyline of a run (the network joins a run end to the middle of
/// another run too).
fn dist_to_run(p: (f64, f64, f64), m: &PMep) -> f64 {
    let mut best = f64::MAX;
    for w in m.spec.points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let ab = (b.0 - a.0, b.1 - a.1, b.2 - a.2);
        let ap = (p.0 - a.0, p.1 - a.1, p.2 - a.2);
        let len2 = ab.0 * ab.0 + ab.1 * ab.1 + ab.2 * ab.2;
        let t = if len2 > 0.0 {
            ((ap.0 * ab.0 + ap.1 * ab.1 + ap.2 * ab.2) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let q = (a.0 + ab.0 * t, a.1 + ab.1 * t, a.2 + ab.2 * t);
        best = best.min(dist(p, q));
    }
    best
}

/// Applies the connections after the runs and components are in the model. `comp_ids` are the
/// rows of `Plan::comps`; `systems` maps the system names of the file to rows. Returns the number
/// of device links stored.
pub(super) fn apply(
    pl: &Plan,
    comp_ids: &[i64],
    systems: &HashMap<&str, i64>,
    db: &Connection,
    sink: &mut dyn DevicePortSink,
    warnings: &mut Vec<String>,
) -> usize {
    let mut stored = 0;
    let mut unsupported = 0;
    let mut apart = 0;
    for l in &pl.mep_links {
        match (&l.a, &l.b) {
            (PEnd::Run { run: r1, end: e1 }, PEnd::Run { run: r2, end: e2 }) => {
                let (m1, m2) = (&pl.meps[*r1], &pl.meps[*r2]);
                let (Some(p), Some(q)) = (end_point(m1, *e1), end_point(m2, *e2)) else {
                    continue;
                };
                // ends meet (or an end touches the other run): the network already joins them
                if dist(p, q) > JOIN_TOL
                    && dist_to_run(p, m2) > JOIN_TOL
                    && dist_to_run(q, m1) > JOIN_TOL
                {
                    apart += 1;
                }
            }
            (PEnd::Port { .. }, PEnd::Port { .. }) => unsupported += 1,
            (
                PEnd::Port {
                    comp,
                    connector,
                    system,
                    flow,
                },
                PEnd::Run { run, .. },
            )
            | (
                PEnd::Run { run, .. },
                PEnd::Port {
                    comp,
                    connector,
                    system,
                    flow,
                },
            ) => {
                let name = pl.meps[*run].system.as_str();
                let Some(&sys) = systems.get(name).or_else(|| systems.get(system.as_str())) else {
                    warnings.push(format!(
                        "connector {connector}: system '{name}' was not imported, link skipped"
                    ));
                    continue;
                };
                match sink.link(db, comp_ids[*comp], connector, sys, *flow) {
                    Ok(true) => stored += 1,
                    Ok(false) => unsupported += 1,
                    Err(e) => warnings.push(format!("connector {connector}: link skipped: {e}")),
                }
            }
        }
    }
    if apart > 0 {
        warnings.push(format!(
            "{apart} run connection(s) in the file join ends that are not at the same point; \
             the model joins only runs that meet, so they stay apart"
        ));
    }
    if unsupported > 0 {
        warnings.push(format!(
            "{unsupported} component connection(s) read from the file were not stored: \
             this model keeps no component links"
        ));
    }
    stored
}

#[cfg(test)]
mod tests {
    use ddc_model::{MepDomain, MepSection, MepShape, Model, RunSpec, SystemKind};

    use super::*;
    use crate::ifc::{export_ifc, import_ifc, plan_bytes};

    fn pmep(points: Vec<(f64, f64, f64)>) -> PMep {
        let sec = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
        PMep {
            system: "SA".into(),
            kind: SystemKind::SupplyAir,
            level: 0,
            spec: RunSpec::new(1, 1, sec, points),
        }
    }

    /// Two ducts that meet at (6, 0, 2.7), one system.
    fn two_runs() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let sa = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
        let sec = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
        m.add_mep_run(&RunSpec::new(
            sa,
            l1,
            sec,
            vec![(0.0, 0.0, 2.7), (6.0, 0.0, 2.7)],
        ))
        .unwrap();
        m.add_mep_run(&RunSpec::new(
            sa,
            l1,
            sec,
            vec![(6.0, 0.0, 2.7), (6.0, 4.0, 2.7)],
        ))
        .unwrap();
        m
    }

    fn count(m: &Model, table: &str) -> i64 {
        m.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// GlobalIds of the segment entities, sorted.
    fn segment_ids(text: &str) -> Vec<String> {
        let mut v: Vec<String> = text
            .lines()
            .filter_map(|l| {
                let (_, rest) = l.split_once("=IFCDUCTSEGMENT('")?;
                Some(rest.get(..22)?.to_string())
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn run_ids_survive_and_a_second_import_adds_nothing() {
        let m = two_runs();
        let text = export_ifc(&m).unwrap();
        let ids = segment_ids(&text);
        assert_eq!(ids.len(), 2);
        assert!(text.contains("=IFCRELCONNECTSPORTS("));

        let mut m2 = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut m2, text.as_bytes()).unwrap();
        assert_eq!(rep.mep_runs, 2);
        assert_eq!(rep.mep_links, 1, "{:?}", rep.warnings);
        assert!(
            !rep.warnings
                .iter()
                .any(|w| w.contains("IfcRelConnectsPorts")),
            "{:?}",
            rep.warnings
        );
        let linked: i64 = m2
            .sql()
            .query_row(
                "SELECT count(*) FROM ext_ref WHERE kind='mep_run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 2);
        // the GlobalIds come back out of the export
        assert_eq!(segment_ids(&export_ifc(&m2).unwrap()), ids);

        let (runs, systems, refs) = (
            count(&m2, "mep_run"),
            count(&m2, "mep_system"),
            count(&m2, "ext_ref"),
        );
        let again = import_ifc(&mut m2, text.as_bytes()).unwrap();
        assert_eq!(count(&m2, "mep_run"), runs);
        assert_eq!(count(&m2, "mep_system"), systems);
        assert_eq!(count(&m2, "ext_ref"), refs);
        assert_eq!(again.updated, 0, "{again:?}");
        assert!(again.unchanged >= 2);
        assert_eq!(again.missing, 0);
        // the network is whole: one node joins the two runs
        assert!(m2
            .mep_network(None)
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.arms.len() == 2));
    }

    #[test]
    fn a_changed_run_is_updated_in_place() {
        let m = two_runs();
        let text = export_ifc(&m).unwrap();
        let mut m2 = Model::open(":memory:").unwrap();
        import_ifc(&mut m2, text.as_bytes()).unwrap();
        let id = m2.mep_runs(None).unwrap()[0];
        // the file changes (the model here stays as imported): the run follows the file
        let mut src = two_runs();
        let sid = src.mep_runs(None).unwrap()[0];
        let mut spec = src.mep_run(sid).unwrap();
        spec.points[0].2 = 3.1;
        src.update_mep_run(sid, &spec).unwrap();
        let newer = export_ifc(&src).unwrap();
        let rep = import_ifc(&mut m2, newer.as_bytes()).unwrap();
        assert_eq!(rep.updated, 1, "{rep:?}");
        assert_eq!(m2.mep_runs(None).unwrap().len(), 2);
        assert_eq!(m2.mep_run(id).unwrap().points[0].2, 3.1);
        // one undo step restores the previous state
        m2.undo().unwrap();
        assert_eq!(m2.mep_run(id).unwrap().points[0].2, 2.7);
    }

    fn break_link(text: &str) -> String {
        text.lines()
            .map(|l| {
                if l.contains("=IFCRELCONNECTSPORTS(") && l.ends_with(",$);") {
                    let body = &l[..l.len() - 4];
                    let i = body.rfind('#').unwrap();
                    format!("{}#99999,$);", &l[..i])
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn broken_references_are_warnings_not_failures() {
        let text = export_ifc(&two_runs()).unwrap();

        // a connection to a port that does not exist
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut m, break_link(&text).as_bytes()).unwrap();
        assert_eq!(rep.mep_runs, 2);
        assert_eq!(rep.mep_links, 0);
        assert!(
            rep.warnings
                .iter()
                .any(|w| w.contains("IfcRelConnectsPorts")),
            "{:?}",
            rep.warnings
        );
        assert_eq!(count(&m, "mep_run"), 2);

        // ports that belong to nothing
        let orphan: String = text
            .lines()
            .filter(|l| !l.contains("=IFCRELNESTS("))
            .collect::<Vec<_>>()
            .join("\n");
        let mut m = Model::open(":memory:").unwrap();
        let rep = import_ifc(&mut m, orphan.as_bytes()).unwrap();
        assert_eq!(rep.mep_runs, 2);
        assert_eq!(rep.mep_links, 0);
        assert!(
            rep.warnings.iter().any(|w| w.contains("not nested")),
            "{:?}",
            rep.warnings
        );
    }

    #[test]
    fn plan_reads_the_connection_between_run_ends() {
        let text = export_ifc(&two_runs()).unwrap();
        let pl = plan_bytes(text.as_bytes()).unwrap();
        assert_eq!(pl.mep_links.len(), 1);
        let l = &pl.mep_links[0];
        let ends: Vec<(usize, u8)> = [&l.a, &l.b]
            .iter()
            .map(|e| match e {
                PEnd::Run { run, end } => (*run, *end),
                PEnd::Port { .. } => panic!("expected run ends"),
            })
            .collect();
        // the first run ends where the second begins, whatever the file order of the runs
        let first = pl
            .meps
            .iter()
            .position(|m| m.spec.points[0].0 == 0.0)
            .unwrap();
        let second = 1 - first;
        assert!(
            ends.contains(&(first, 1)) && ends.contains(&(second, 0)),
            "{ends:?}"
        );
    }

    struct Fake(Vec<(i64, String, i64, Option<f64>)>, bool);

    impl DevicePortSink for Fake {
        fn link(
            &mut self,
            _: &Connection,
            comp: i64,
            connector: &str,
            system: i64,
            flow: Option<f64>,
        ) -> Result<bool, Error> {
            if self.1 {
                self.0.push((comp, connector.into(), system, flow));
            }
            Ok(self.1)
        }
    }

    fn device_plan() -> Plan {
        let mut pl = Plan::default();
        pl.meps = vec![
            pmep(vec![(0.0, 0.0, 2.7), (6.0, 0.0, 2.7)]),
            pmep(vec![(6.0, 0.0, 2.7), (6.0, 4.0, 2.7)]),
        ];
        pl.mep_links = vec![
            PMepLink {
                a: PEnd::Port {
                    comp: 1,
                    connector: "Out".into(),
                    system: "SA".into(),
                    flow: Some(120.0),
                },
                b: PEnd::Run { run: 0, end: 0 },
            },
            // the same pair of runs joined at ends that do not meet
            PMepLink {
                a: PEnd::Run { run: 0, end: 0 },
                b: PEnd::Run { run: 1, end: 1 },
            },
        ];
        pl
    }

    #[test]
    fn component_ports_go_to_the_sink_and_far_run_ends_warn() {
        let pl = device_plan();
        let db = Connection::open_in_memory().unwrap();
        let systems: HashMap<&str, i64> = [("SA", 7)].into();
        let mut sink = Fake(vec![], true);
        let mut w = vec![];
        let n = apply(&pl, &[20, 21], &systems, &db, &mut sink, &mut w);
        assert_eq!(n, 1);
        assert_eq!(
            sink.0.as_slice(),
            &[(21, "Out".to_string(), 7, Some(120.0))]
        );
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("not at the same point"));

        // a model that keeps no component links: one warning, nothing lost silently
        let mut sink = Fake(vec![], false);
        let mut w = vec![];
        assert_eq!(apply(&pl, &[20, 21], &systems, &db, &mut sink, &mut w), 0);
        assert!(w.iter().any(|x| x.contains("not stored")), "{w:?}");
    }

    #[test]
    fn stored_ports_upsert_and_undo() {
        let mut m = Model::open(":memory:").unwrap();
        // the model drops links to things that do not exist: the targets are real here
        let l1 = m.add_level("L1", 0.0).unwrap();
        let s1 = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
        let s2 = m.add_mep_system("SA-2", SystemKind::SupplyAir).unwrap();
        let sec = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.4, h: 0.2 });
        let stub = m
            .add_mep_run(&RunSpec::new(
                s1,
                l1,
                sec,
                vec![(0.0, 0.0, 2.7), (1.0, 0.0, 2.7)],
            ))
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO comp(id,level,ctype,x,y,z,rotation,mirror,vals)                  VALUES (5,?1,1,0,0,0,0,0,'{}')",
                [l1],
            )
            .unwrap();
        m.sql()
            .execute(
                "INSERT INTO mep_port(comp,connector,system,flow,stub) VALUES (5,'In',?1,3.0,?2)",
                params![s1, stub],
            )
            .unwrap();
        m.batch_walls_edit("wall_ports", |db, snaps| {
            let mut s = StoredPorts::new(snaps);
            assert!(s.link(db, 5, "In", s2, None)?);
            assert!(s.link(db, 5, "Out", s2, Some(4.0))?);
            Ok(())
        })
        .unwrap();
        let rows = |m: &Model| -> Vec<(String, Option<i64>, Option<f64>, Option<i64>)> {
            let mut st = m
                .sql()
                .prepare("SELECT connector, system, flow, stub FROM mep_port ORDER BY connector")
                .unwrap();
            let it = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap();
            it.map(|r| r.unwrap()).collect()
        };
        // the existing link keeps its flow and stub, only the system changes
        assert_eq!(
            rows(&m),
            vec![
                ("In".to_string(), Some(s2), Some(3.0), Some(stub)),
                ("Out".to_string(), Some(s2), Some(4.0), None),
            ]
        );
        m.undo().unwrap();
        assert_eq!(
            rows(&m),
            vec![("In".to_string(), Some(s1), Some(3.0), Some(stub))]
        );
    }

    #[test]
    fn a_fresh_model_has_the_table_and_stores() {
        let mut m = Model::open(":memory:").unwrap();
        let stored = m
            .batch_walls_edit("wall_ports", |db, snaps| {
                Ok(StoredPorts::new(snaps).link(db, 5, "In", 2, None)?)
            })
            .unwrap();
        assert!(stored);
    }
}
