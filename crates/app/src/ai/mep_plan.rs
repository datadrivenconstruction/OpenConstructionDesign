// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! AI plan items of building services: networks routed by the planner, fixtures and devices
//! placed from the library, devices powered from a panel. The AI states the intent; the router
//! builds on the copy at compile time, so Apply replays exactly the build the preview showed.

use super::exec::ModelStep;
use super::plan::{push, CompileCx, PlanError};
use ddc_model::{DeviceSpec, ElKey, MepPlanned, NetReq, RouteFrom};

#[derive(Clone, Debug)]
pub enum MepOp {
    /// Networks routed together: each keeps clear of the ones before it.
    Route(Vec<NetReq>),
    /// A conduit network from a panel to the devices and their circuits.
    Power {
        panel: i64,
        devices: Vec<(i64, String)>,
        level: i64,
        variant: usize,
    },
    /// Fixtures and devices from the library.
    Place(Vec<DeviceSpec>),
}

impl MepOp {
    /// Elements the op names: components it routes to or from.
    pub fn elements(&self) -> Vec<ElKey> {
        let comp = |c: i64| ("comp".to_string(), c);
        match self {
            MepOp::Route(reqs) => {
                let mut v: Vec<ElKey> = vec![];
                for r in reqs {
                    v.extend(r.to.iter().map(|t| comp(t.0)));
                    if let RouteFrom::Port { comp: c, .. } = &r.from {
                        v.push(comp(*c));
                    }
                    if let RouteFrom::Run(id) = &r.from {
                        v.push(("mep_run".to_string(), *id));
                    }
                }
                v.sort();
                v.dedup();
                v
            }
            MepOp::Power { panel, devices, .. } => std::iter::once(comp(*panel))
                .chain(devices.iter().map(|d| comp(d.0)))
                .collect(),
            MepOp::Place(_) => vec![],
        }
    }
}

/// Plans the op on the copy as it is now.
pub fn plan_of(m: &ddc_model::Model, op: &MepOp) -> Result<MepPlanned, PlanError> {
    Ok(match op {
        MepOp::Route(reqs) => m.mep_plan_all(reqs)?,
        MepOp::Power {
            panel,
            devices,
            level,
            variant,
        } => m.mep_plan_power(*panel, devices, *level, *variant)?,
        MepOp::Place(_) => MepPlanned::default(),
    })
}

pub fn compile(
    cx: &mut CompileCx<'_>,
    op: &MepOp,
    out: &mut Vec<ModelStep>,
) -> Result<(), PlanError> {
    if let MepOp::Place(specs) = op {
        let ids = push(
            cx,
            out,
            ModelStep::Devices {
                specs: specs.clone(),
                expect: vec![],
            },
        )?;
        for id in ids {
            cx.declared.insert(("comp".to_string(), id));
        }
        return Ok(());
    }
    let p = plan_of(&cx.ed.model, op)?;
    if p.build.runs.is_empty() {
        return Err(PlanError(format!(
            "no route reached any target ({} failed)",
            p.metrics.failed.len()
        )));
    }
    for k in op.elements() {
        cx.declared.insert(k);
    }
    push(
        cx,
        out,
        ModelStep::Mep {
            build: Box::new(p.build),
            expect: vec![],
        },
    )?;
    if let Some(ModelStep::Mep { expect, .. }) = out.last() {
        for (t, id) in expect {
            cx.declared.insert((t.to_string(), *id));
        }
    }
    Ok(())
}
