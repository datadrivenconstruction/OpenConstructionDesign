// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A whole planned network written as one journal entry: systems (reused by name), wall sleeves
//! and slab holes for the crossings, runs, connector links and circuits. The networks it touched
//! must still build at the end, or everything is rolled back. The planner (`autoroute`) yields a
//! `MepBuild`; the AI applies it on the draft and replays it on Apply.

use ddc_geom::mep::SystemKind;

use super::circuit::{insert_circuit, CircuitSpec};
use super::manual::link_port;
use super::{check_spec, invalid, write_run, RunSpec};
use crate::ops::Inv;
use crate::{Error, Model, OpeningSpec};

/// A system of the build: an existing one or the n-th of `MepBuild::systems`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SysRef {
    Id(i64),
    New(usize),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MepBuild {
    /// Systems by name and kind: one of that name already in the model is reused.
    pub systems: Vec<(String, SystemKind)>,
    /// Runs; their `system` field is replaced by the reference.
    pub runs: Vec<(SysRef, RunSpec)>,
    /// Component connectors joined to a system: (component, connector, system).
    pub links: Vec<(i64, String, SysRef)>,
    /// Voids cut in walls where runs cross them.
    pub sleeves: Vec<(i64, OpeningSpec)>,
    /// Holes added to slabs (rings in plan) where risers and drops cross them.
    pub holes: Vec<(i64, Vec<(f64, f64)>)>,
    /// Circuits; their `system` field is replaced by the reference.
    pub circuits: Vec<(Option<SysRef>, CircuitSpec)>,
}

/// What a build made, by table, in order: systems, openings, runs, circuits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Built {
    pub systems: Vec<i64>,
    pub openings: Vec<i64>,
    pub runs: Vec<i64>,
    pub circuits: Vec<i64>,
}

impl Built {
    /// (table, id) of everything new, for expectation checks on replay.
    pub fn keys(&self) -> Vec<(&'static str, i64)> {
        let t = |name: &'static str, v: &[i64]| {
            v.iter().map(move |&i| (name, i)).collect::<Vec<_>>()
        };
        [
            t("mep_system", &self.systems),
            t("opening", &self.openings),
            t("mep_run", &self.runs),
            t("elec_circuit", &self.circuits),
        ]
        .concat()
    }
}

impl MepBuild {
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty() && self.links.is_empty() && self.circuits.is_empty()
    }
}

impl Model {
    /// Writes a planned network: one undo step.
    pub fn mep_build(&mut self, b: &MepBuild) -> Result<Built, Error> {
        if b.runs.iter().any(|(r, _)| matches!(r, SysRef::New(i) if *i >= b.systems.len())) {
            return invalid("mep build: system reference out of range");
        }
        self.mep_group("mep_build", |m| {
            let mut out = Built::default();
            let have = m.mep_systems()?;
            let mut sys: Vec<i64> = vec![];
            for (name, kind) in &b.systems {
                match have.iter().find(|s| &s.name == name) {
                    Some(s) if s.kind == *kind => sys.push(s.id),
                    Some(_) => return invalid(format!("system {name} is of another kind")),
                    None => {
                        let id = m.add_mep_system(name, *kind)?;
                        out.systems.push(id);
                        sys.push(id);
                    }
                }
            }
            let id_of = |r: SysRef| match r {
                SysRef::Id(i) => i,
                SysRef::New(k) => sys[k],
            };
            for (wall, o) in &b.sleeves {
                out.openings.push(m.add_opening(*wall, o)?);
            }
            for (slab, ring) in &b.holes {
                m.add_slab_opening(*slab, ring)?;
            }
            let was: Vec<(i64, bool)> = {
                let mut t: Vec<i64> = b.runs.iter().map(|(r, _)| id_of(*r)).collect();
                t.extend(b.links.iter().map(|l| id_of(l.2)));
                t.sort_unstable();
                t.dedup();
                // the connections are read once for all the systems, not once per system
                let ports = m.component_ports(None);
                t.into_iter()
                    .map(|s| {
                        let ok = ports
                            .as_ref()
                            .is_ok_and(|p| m.mep_network_with(Some(s), p).is_ok());
                        (s, ok || out.systems.contains(&s))
                    })
                    .collect()
            };
            let specs: Vec<RunSpec> = b
                .runs
                .iter()
                .map(|(r, s)| {
                    // a planned run from any producer: a leg too short for its elbows gives
                    // its corner to the next leg instead of failing the build
                    let mut s = RunSpec {
                        system: id_of(*r),
                        ..s.clone()
                    };
                    s.fit_bends();
                    s
                })
                .collect();
            for s in &specs {
                check_spec(&m.db, s)?;
            }
            let links: Vec<(i64, String, i64)> = b
                .links
                .iter()
                .map(|(c, n, r)| (*c, n.clone(), id_of(*r)))
                .collect();
            let circuits: Vec<CircuitSpec> = b
                .circuits
                .iter()
                .map(|(r, c)| CircuitSpec {
                    system: r.map(id_of).or(c.system),
                    ..c.clone()
                })
                .collect();
            let (runs, cids) = m.op("mep_build_runs", |db| {
                let mut inv = Inv::default();
                let mut runs = vec![];
                for s in &specs {
                    let id = write_run(db, s, None)?;
                    inv = inv.then(Inv::default().delete("mep_run", "id", id));
                    runs.push(id);
                }
                for (comp, conn, sys) in &links {
                    inv = link_port(db, *comp, conn, *sys, None)?.then(inv);
                }
                let mut cids = vec![];
                for c in &circuits {
                    let (id, i) = insert_circuit(db, c)?;
                    inv = i.then(inv);
                    cids.push(id);
                }
                let head = runs.first().copied().unwrap_or(0);
                Ok(((runs, cids), head, inv))
            })?;
            out.runs = runs;
            out.circuits = cids;
            if was.iter().any(|w| w.1) {
                let ports = m.component_ports(None)?;
                for (s, ok) in was {
                    if ok {
                        m.mep_network_with(Some(s), &ports)?;
                    }
                }
            }
            Ok(out)
        })
    }
}
