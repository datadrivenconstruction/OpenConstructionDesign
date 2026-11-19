// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Electrical circuits read back from IFC: IfcDistributionCircuit with `DDC_Circuit`, the device
//! list `DDC_CircuitDevices` and the explicit loads `DDC_CircuitLoads` (keys `<GlobalId>/<connector>`
//! of the component products). A circuit whose panel or devices do not resolve is skipped with a
//! warning, never a failure.

use std::collections::HashMap;

use ddc_model::{CircuitKind, Error, Snapshots};
use rusqlite::{params, Connection, OptionalExtension};

use super::{PCircuit, Plan, Rd};
use crate::ifc::reader::StepFile;

pub(super) fn read(
    f: &StepFile,
    props: &HashMap<u32, Vec<u32>>,
    comp_ent: &HashMap<u32, usize>,
    warnings: &mut Vec<String>,
) -> Vec<PCircuit> {
    let guid_of = |id: u32| -> String {
        f.get(id)
            .and_then(|e| Rd::arg(e, 0).as_str())
            .unwrap_or_default()
            .to_string()
    };
    let by_guid: HashMap<String, usize> =
        comp_ent.iter().map(|(&e, &i)| (guid_of(e), i)).collect();
    let mut out = vec![];
    for (id, ty) in f.iter() {
        if ty != "IFCDISTRIBUTIONCIRCUIT" {
            continue;
        }
        let Some(kv) = super::pset(f, props, id, "DDC_Circuit") else {
            continue;
        };
        let name = f
            .get(id)
            .and_then(|e| Rd::arg(e, 2).as_str())
            .unwrap_or_default()
            .to_string();
        let Some(&panel) = kv.get("PanelGuid").and_then(|g| by_guid.get(g)) else {
            warnings.push(format!("circuit '{name}': its panel is not an imported component"));
            continue;
        };
        let kind = kv
            .get("Kind")
            .and_then(|k| CircuitKind::parse(k))
            .unwrap_or(CircuitKind::Power);
        let (d_breaker, d_cable) = kind.defaults();
        let num = |k: &str, d: f64| {
            kv.get(k)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(d)
        };
        let poles: u32 = if kv.get("Poles").is_some_and(|p| p.trim() == "3") { 3 } else { 1 };
        let rules = ddc_model::CableRules::shared();
        let mut voltage = num("Voltage", if poles == 3 { 400.0 } else { 230.0 });
        if poles == 3 && !(380.0..=420.0).contains(&voltage) {
            warnings.push(format!(
                "circuit '{name}': three poles at {voltage} V, taken as 400 V line voltage"
            ));
            voltage = 400.0;
        }
        let load_type = kv
            .get("LoadType")
            .filter(|t| rules.cos_phi_of(t).is_some())
            .cloned()
            .unwrap_or_else(|| rules.default_load_type(kind).to_string());
        let conductor = kv
            .get("Conductor")
            .and_then(|c| ddc_model::Conductor::parse(c.trim()))
            .unwrap_or(ddc_model::Conductor::Cu);
        let cos_phi = kv
            .get("CosPhi")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| *v > 0.0 && *v <= 1.0)
            .or_else(|| rules.cos_phi_of(&load_type))
            .unwrap_or_else(|| rules.cos_phi(kind));
        let laying = kv
            .get("Laying")
            .filter(|l| rules.laying.iter().any(|x| &x.id == *l))
            .cloned()
            .unwrap_or_else(|| "conduit_wall".to_string());
        let ambient_c = kv
            .get("AmbientC")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && (-40.0..=100.0).contains(v))
            .unwrap_or_else(|| rules.ambient_ref_c());
        let grouped = kv
            .get("Grouped")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|g| *g >= 1)
            .unwrap_or(1);
        let loads = super::pset(f, props, id, "DDC_CircuitLoads").unwrap_or_default();
        let mut listed: Vec<(usize, String)> = super::pset(f, props, id, "DDC_CircuitDevices")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(n, v)| Some((n.parse().ok()?, v)))
            .collect();
        listed.sort();
        let mut devices = vec![];
        for (_, key) in listed {
            let Some((guid, conn)) = key.split_once('/') else {
                continue;
            };
            match by_guid.get(guid) {
                Some(&comp) => {
                    // only a finite load of zero or more is a load; anything else is dropped
                    let load = loads.get(&key).and_then(|l| l.trim().parse::<f64>().ok());
                    let load = match load {
                        Some(l) if !(l.is_finite() && l >= 0.0) => {
                            warnings.push(format!(
                                "circuit '{name}': load {l} of {key} is not a number from zero up, \
                                 the family value is used"
                            ));
                            None
                        }
                        l => l,
                    };
                    devices.push((comp, conn.to_string(), load));
                }
                None => warnings.push(format!(
                    "circuit '{name}': device {guid} is not an imported component"
                )),
            }
        }
        out.push(PCircuit {
            panel,
            number: kv.get("Number").and_then(|n| n.trim().parse().ok()).unwrap_or(0),
            name,
            kind,
            system: kv.get("System").cloned().unwrap_or_default(),
            voltage,
            poles,
            breaker: num("Breaker", d_breaker),
            cable: num("Cable", d_cable),
            max_drop: num("MaxDrop", rules.max_drop(kind)),
            cos_phi,
            laying,
            ambient_c,
            grouped,
            conductor,
            load_type,
            devices,
        });
    }
    out
}

/// Writes the circuits after the components and runs are in the model; a way number or a device
/// connector that is already taken keeps its row. Returns the circuits stored.
pub(super) fn apply(
    pl: &Plan,
    comp_ids: &[i64],
    systems: &HashMap<&str, i64>,
    db: &Connection,
    snaps: &mut Snapshots,
    warnings: &mut Vec<String>,
) -> Result<usize, Error> {
    if pl.circuits.is_empty() {
        return Ok(0);
    }
    snaps.watermark(db, "elec_circuit")?;
    snaps.watermark(db, "elec_circuit_dev")?;
    let mut stored = 0;
    for c in &pl.circuits {
        let panel = comp_ids[c.panel];
        let sys = if c.system.is_empty() {
            None
        } else {
            match systems.get(c.system.as_str()) {
                Some(&s) => Some(s),
                None => db
                    .query_row(
                        "SELECT id FROM mep_system WHERE name=?1",
                        [&c.system],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()?,
            }
        };
        let number: i64 = if c.number == 0 {
            db.query_row(
                "SELECT COALESCE(MAX(number),0)+1 FROM elec_circuit WHERE panel=?1",
                [panel],
                |r| r.get(0),
            )?
        } else {
            c.number as i64
        };
        let taken: i64 = db.query_row(
            "SELECT count(*) FROM elec_circuit WHERE panel=?1 AND number=?2",
            params![panel, number],
            |r| r.get(0),
        )?;
        if taken > 0 {
            warnings.push(format!(
                "circuit '{}': way {number} of its panel is taken, skipped",
                c.name
            ));
            continue;
        }
        db.execute(
            "INSERT INTO elec_circuit(panel,number,name,kind,system,voltage,poles,breaker,cable,\
             max_drop,cos_phi,laying,ambient_c,grouped,conductor,load_type) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                panel,
                number,
                c.name,
                c.kind.name(),
                sys,
                c.voltage,
                c.poles,
                c.breaker,
                c.cable,
                c.max_drop,
                c.cos_phi,
                c.laying,
                c.ambient_c,
                c.grouped,
                c.conductor.name(),
                c.load_type
            ],
        )?;
        let id = db.last_insert_rowid();
        for (comp, conn, load) in &c.devices {
            db.execute(
                "INSERT OR IGNORE INTO elec_circuit_dev(circuit,comp,connector,load) \
                 VALUES (?1,?2,?3,?4)",
                params![id, comp_ids[*comp], conn, load],
            )?;
        }
        stored += 1;
    }
    Ok(stored)
}
