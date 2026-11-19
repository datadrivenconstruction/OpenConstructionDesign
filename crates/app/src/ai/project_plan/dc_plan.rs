// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The data centre fit-out of a project in the model: its units as library components slot
//! by slot, then its chilled water pipes and cable trays as one network build with the
//! cooling units' and chillers' connectors linked. Kept as they are while nothing they depend
//! on changed, like the ducts.

use super::*;
use ddc_gen::project::dc::{GDc, Net};

/// The slots the fit-out asks for.
pub(super) fn asked(dc: &GDc, out: &mut BTreeSet<u32>) {
    out.extend((0..dc.units.len() as u32).map(|i| slots::DC_UNITS + i));
    out.extend((0..dc.runs.len() as u32).map(|i| slots::DC_RUNS + i));
}

/// The component type of a library family type, loading the family when the model lacks it.
fn comp_type(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    known: &mut BTreeMap<(String, String), i64>,
    family: &str,
    ty: &str,
) -> Result<i64, PlanError> {
    let key = (family.to_string(), ty.to_string());
    if let Some(id) = known.get(&key) {
        return Ok(*id);
    }
    let fam = match cx.ed.model.family_id(family)? {
        Some(id) => id,
        None => {
            let head = format!("family {family}");
            let def = ddc_model::library_sources()
                .into_iter()
                .find(|s| s.lines().any(|x| x.trim() == head))
                .ok_or_else(|| PlanError(format!("no library family {family}")))?;
            push(cx, out, ModelStep::Family { def: def.to_string(), overwrite: false })?;
            cx.ed.model.family_id(family)?.ok_or_else(|| PlanError(format!("family {family} was not loaded")))?
        }
    };
    let sql = "SELECT id FROM comp_type WHERE family=?1 AND name=?2";
    let id: i64 = cx
        .ed
        .model
        .sql()
        .query_row(sql, rusqlite::params![fam, ty], |r| r.get(0))
        .map_err(|_| PlanError(format!("family {family} has no type {ty}")))?;
    known.insert(key, id);
    Ok(id)
}

pub(super) fn sync(
    cx: &mut CompileCx<'_>,
    out: &mut Vec<ModelStep>,
    sy: &mut Sync,
    spec: &ProjectSpec,
    dc: &GDc,
    levels: &[i64],
) -> Result<(), PlanError> {
    use ddc_model::{MepBuild, MepDomain, MepSection, MepShape, RunSpec, SysRef};
    if dc.units.len() > slots::DC_UNITS_MAX as usize || dc.runs.len() > slots::DC_RUNS_MAX as usize {
        return Err("the data centre is larger than its slot blocks".into());
    }
    let level_of = |k: usize| levels.get(k).copied().ok_or_else(|| PlanError(format!("the data centre names storey {k}")));
    let mut known = BTreeMap::new();
    let mut wants = vec![];
    for (i, u) in dc.units.iter().enumerate() {
        let id = comp_type(cx, out, &mut known, &u.family, &u.ty)?;
        wants.push(Want {
            rel: slots::DC_UNITS + i as u32,
            table: "comp",
            elem: NewElem::Comp {
                level: level_of(u.storey)?,
                ctype: type_ref("comp_type", id),
                at: (u.at.0, u.at.1, u.z),
                rotation: u.rot,
                host: None,
                scale: 1.0,
                min_clear: 0.0,
            },
            role: None,
            wall_rel: None,
        });
    }
    sync_group(cx, out, sy, wants)?;
    let sys = |n: Net| SysRef::New(Net::ALL.iter().position(|x| *x == n).unwrap_or(0));
    let mut links = vec![];
    for l in &dc.links {
        let Some(c) = cx.slots.get(&(sy.slot0 + slots::DC_UNITS + l.unit as u32)).copied() else { continue };
        links.push((c, l.connector.clone(), sys(l.net)));
    }
    let intent = fnv(&format!("{:?}|{links:?}", dc.runs));
    let range = slots::DC_RUNS..slots::DC_RUNS + slots::DC_RUNS_MAX;
    let prev: Vec<(u32, Rec)> = sy.prev.recs.range(range).map(|(r, x)| (*r, x.clone())).collect();
    let mut kept = prev.len() == dc.runs.len() && !prev.is_empty();
    for (_, r) in &prev {
        kept &= r.intent == intent && row_hash(&cx.ed.model, &r.table, r.id)?.is_some_and(|now| now == r.row);
    }
    if kept {
        sy.recs.extend(prev);
        return Ok(());
    }
    for (rel, r) in prev {
        match row_hash(&cx.ed.model, &r.table, r.id)? {
            None => {}
            Some(now) if now != r.row => {
                let why = "changed by hand after Apply: kept although the revision rebuilds the data centre";
                skip(cx, &r.table, r.id, why);
                sy.recs.insert(rel, r);
            }
            Some(_) => delete(cx, out, &r)?,
        }
    }
    if dc.runs.is_empty() {
        return Ok(());
    }
    let base = elevation_of(cx, levels[0])?;
    let mut runs = vec![];
    for r in &dc.runs {
        let level = level_of(r.storey)?;
        let pts: Vec<(f64, f64, f64)> = r.points.iter().map(|q| (q.0, q.1, base + q.2)).collect();
        let mut rs = if r.dn > 0 {
            RunSpec::pipe(0, level, ddc_geom::mep::PipeMaterial::Steel, r.dn, pts)?
        } else {
            let sec = MepSection::new(MepDomain::CableTray, MepShape::Rect { w: r.tray.0, h: r.tray.1 });
            RunSpec::new(0, level, sec, pts)
        };
        rs.caps = r.caps;
        runs.push((sys(r.net), rs));
    }
    let build = MepBuild {
        systems: Net::ALL.iter().map(|n| (n.name(spec.ru), n.kind())).collect(),
        runs,
        links,
        ..MepBuild::default()
    };
    // the links read the units' placement: settle the quantities the item holds back
    cx.ed.model.settle_derived()?;
    cx.ed.model.hold_derived();
    push(cx, out, ModelStep::Mep { build: Box::new(build), expect: vec![] })?;
    let Some(ModelStep::Mep { expect, .. }) = out.last() else {
        return Err("the data centre network was not built".into());
    };
    let expect = expect.clone();
    for (t, id) in &expect {
        cx.declared.insert((t.to_string(), *id));
    }
    let ids = expect.iter().filter(|k| k.0 == "mep_run").map(|k| k.1);
    for (i, id) in ids.enumerate() {
        let rel = slots::DC_RUNS + i as u32;
        sy.recs.insert(rel, Rec { table: "mep_run".to_string(), id, intent, row: 0 });
        sy.fresh.insert(rel);
    }
    Ok(())
}
