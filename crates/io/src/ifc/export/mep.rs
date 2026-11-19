// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Networks -> IfcDuctSegment / IfcPipeSegment / IfcCableCarrierSegment per run, one fitting
//! entity per network fitting, IfcDistributionPort at each run end (nested), IfcRelConnectsPorts
//! at shared ends and IfcDistributionSystem groups. Exact run data go to `DDC_MepRun`, so a
//! round trip is exact; fittings are derived and carry `DDC_MepFitting` only. Connected component
//! ports are nested in the component and carry `DDC_MepPort`; links are not read back on import.

use std::collections::BTreeMap;

use ddc_geom::mep::{merge, sweep, FittingKind, Piece};
use ddc_geom::mesh::Mesh;
use ddc_model::{points_text, MepDomain, MepNetwork, MepShape, Model, WorldPort};

use super::{list, r, real, s, text, Ctx, Geo, NONE};

/// Ports per run end and system members, filled level by level.
#[derive(Default)]
pub(super) struct MepOut {
    pub ports: BTreeMap<(i64, u8), u32>,
    pub members: BTreeMap<i64, Vec<u32>>,
    /// Exported component entities, filled by `components`.
    pub comps: BTreeMap<i64, u32>,
    /// Connected device ports waiting for their component's level.
    pub devices: Vec<DevicePort>,
    pub comp_ports: BTreeMap<(i64, String), u32>,
}

/// A connected component port: a stored link or a port touching a network node.
pub(super) struct DevicePort {
    key: i64,
    port: WorldPort,
    system: i64,
    flow: f64,
}

pub(super) fn device_ports(model: &Model, net: &MepNetwork) -> Result<Vec<DevicePort>, String> {
    let e = |x: ddc_model::Error| x.to_string();
    let links = model.mep_port_links(None).map_err(e)?;
    let mut touching: BTreeMap<(i64, String), i64> = BTreeMap::new();
    for n in &net.nodes {
        if let (Some(p), Some(a)) = (&n.port, n.arms.first()) {
            touching.insert(p.clone(), model.mep_run(a.0).map_err(e)?.system);
        }
    }
    if touching.is_empty() && links.iter().all(|l| l.system.is_none()) {
        return Ok(vec![]);
    }
    let mut seen: BTreeMap<i64, i64> = BTreeMap::new();
    let mut out = vec![];
    for port in model.component_ports(None).map_err(e)? {
        let n = seen.entry(port.comp).or_insert(0);
        let key = port.comp * 1000 + *n;
        *n += 1;
        let system = links
            .iter()
            .find(|l| l.comp == port.comp && l.connector == port.name)
            .and_then(|l| l.system)
            .or_else(|| touching.get(&(port.comp, port.name.clone())).copied());
        if let Some(system) = system {
            let flow = model.mep_port_flow(port.comp, &port.name).map_err(e)?;
            out.push(DevicePort {
                key,
                port,
                system,
                flow,
            });
        }
    }
    Ok(out)
}

/// Ports of components exported so far: nested in the component, listed in its system.
pub(super) fn devices(
    c: &mut Ctx,
    model: &Model,
    elev: f64,
    g: &Geo,
    st: &mut MepOut,
) -> Result<(), String> {
    if st.devices.is_empty() {
        return Ok(());
    }
    let systems: BTreeMap<i64, ddc_model::MepSystem> = model
        .mep_systems()
        .map_err(|x| x.to_string())?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    let mut nested: BTreeMap<i64, Vec<u32>> = BTreeMap::new();
    let mut later = vec![];
    for d in std::mem::take(&mut st.devices) {
        let Some(&ent) = st.comps.get(&d.port.comp) else {
            later.push(d);
            continue;
        };
        let Some(sys) = systems.get(&d.system) else {
            continue;
        };
        let (x, y, z) = d.port.at;
        let pp = c.point3(x, y, z - elev);
        let pax = c.axis3(pp, None);
        let ppl = c.placement(Some(g.storey_pl), pax);
        let port = c.rooted(
            "IFCDISTRIBUTIONPORT",
            "mep-dev-port",
            d.key,
            &format!("{}:{}", d.port.comp, d.port.name),
            &[
                s(NONE),
                r(ppl),
                s(NONE),
                s(".SOURCEANDSINK."),
                s(&format!(".{}.", port_type(d.port.domain))),
                s(&format!(".{}.", sys.kind.ifc())),
            ],
        );
        pset(
            c,
            "mep-dev-pset",
            d.key,
            "DDC_MepPort",
            port,
            vec![
                ("Connector", format!("IFCLABEL({})", text(&d.port.name))),
                ("System", format!("IFCLABEL({})", text(&sys.name))),
                ("Flow", format!("IFCREAL({})", real(if d.flow.is_finite() { d.flow } else { 0.0 }))),
            ],
        );
        st.comp_ports
            .insert((d.port.comp, d.port.name.clone()), port);
        nested.entry(d.port.comp).or_default().push(port);
        let members = st.members.entry(d.system).or_default();
        if !members.contains(&ent) {
            members.push(ent);
        }
    }
    st.devices = later;
    for (comp, ports) in nested {
        let ent = st.comps[&comp];
        c.rel("IFCRELNESTS", "mep-dev-nest", comp, &[r(ent), list(&ports)]);
    }
    Ok(())
}

fn segment_class(d: MepDomain) -> (&'static str, &'static str) {
    match d {
        MepDomain::Duct => ("IFCDUCTSEGMENT", "RIGIDSEGMENT"),
        MepDomain::Pipe => ("IFCPIPESEGMENT", "RIGIDSEGMENT"),
        MepDomain::CableTray => ("IFCCABLECARRIERSEGMENT", "CABLETRAYSEGMENT"),
        MepDomain::Conduit => ("IFCCABLECARRIERSEGMENT", "CONDUITSEGMENT"),
    }
}

fn fitting_class(d: MepDomain, k: &FittingKind) -> (&'static str, &'static str) {
    match d {
        MepDomain::Duct => ("IFCDUCTFITTING", k.ifc()),
        MepDomain::Pipe => ("IFCPIPEFITTING", k.ifc()),
        _ => (
            "IFCCABLECARRIERFITTING",
            match k {
                FittingKind::Elbow => "BEND",
                FittingKind::Tee => "TEE",
                FittingKind::Cross => "CROSS",
                FittingKind::Transition => "REDUCER",
                FittingKind::Cap => "NOTDEFINED",
            },
        ),
    }
}

fn port_type(d: MepDomain) -> &'static str {
    match d {
        MepDomain::Duct => "DUCT",
        MepDomain::Pipe => "PIPE",
        _ => "CABLECARRIER",
    }
}

/// The triangles whose three vertices are finite: a NaN or infinite coordinate written as
/// `NaN` makes the whole file unreadable (STEP has no such number), so such a piece of a
/// swept run or a fitting is left out instead.
pub(crate) fn finite_part(m: &Mesh) -> Mesh {
    let ok = |i: u32| {
        let p = m.positions[i as usize];
        p.x.is_finite() && p.y.is_finite() && p.z.is_finite()
    };
    if m.positions.iter().all(|p| p.x.is_finite() && p.y.is_finite() && p.z.is_finite()) {
        return m.clone();
    }
    let mut at = vec![u32::MAX; m.positions.len()];
    let mut out = Mesh::default();
    for (k, t) in m.triangles.iter().enumerate() {
        if !t.iter().all(|&i| ok(i)) {
            continue;
        }
        let mut n = [0u32; 3];
        for (j, &i) in t.iter().enumerate() {
            if at[i as usize] == u32::MAX {
                at[i as usize] = out.positions.len() as u32;
                out.positions.push(m.positions[i as usize]);
            }
            n[j] = at[i as usize];
        }
        out.triangles.push(n);
        out.face_of.push(m.face_of.get(k).copied().unwrap_or(0));
    }
    out
}

pub(crate) fn body(c: &mut Ctx, g: &Geo, m: &Mesh, elev: f64) -> u32 {
    let m = &finite_part(m);
    let fs = super::adv::face_set(c, &m.positions, &m.triangles, elev);
    let rep = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[
            r(g.body_ctx),
            text("Body"),
            text("Tessellation"),
            list(&[fs]),
        ],
    );
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[rep])],
    )
}

pub(super) fn pset(
    c: &mut Ctx,
    kind: &str,
    id: i64,
    name: &str,
    ent: u32,
    fields: Vec<(&str, String)>,
) {
    let props: Vec<u32> = fields
        .into_iter()
        .map(|(n, v)| {
            c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)])
        })
        .collect();
    let set = c.rooted("IFCPROPERTYSET", kind, id, name, &[list(&props)]);
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("{kind}-rel"),
        id,
        &[list(&[ent]), r(set)],
    );
}

/// Runs of `level` and the fittings whose first run is on it.
#[allow(clippy::too_many_arguments)]
pub(super) fn runs(
    c: &mut Ctx,
    model: &Model,
    net: &MepNetwork,
    level: i64,
    elev: f64,
    g: &Geo,
    st: &mut MepOut,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let e = |x: ddc_model::Error| x.to_string();
    let systems: BTreeMap<i64, ddc_model::MepSystem> = model
        .mep_systems()
        .map_err(e)?
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    let label = |v: &str| format!("IFCLABEL({})", text(v));
    let ftext = |v: &str| format!("IFCTEXT({})", text(v));
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    let pl = c.placement(Some(g.storey_pl), ax);
    let mut on_level = BTreeMap::new();
    for id in model.mep_runs(None).map_err(e)? {
        let spec = model.mep_run(id).map_err(e)?;
        if spec.level != level {
            continue;
        }
        let sys = &systems[&spec.system];
        let meshes: Vec<Mesh> = net
            .pieces
            .iter()
            .filter(|p| p.0 == id && matches!(p.1, Piece::Straight { .. }))
            .map(|p| sweep(p.2.envelope(), &p.1.samples(8), 24))
            .collect();
        let pds = body(c, g, &merge(&meshes), elev);
        let (class, pre) = segment_class(spec.section.domain);
        let size = spec.size_label();
        let ent = c.rooted(
            class,
            "mep-run",
            id,
            &format!("{}:{size}", sys.name),
            &[text(&size), r(pl), r(pds), s(NONE), s(&format!(".{pre}."))],
        );
        let (shape, w, h) = match spec.section.shape {
            MepShape::Round { d } => ("round", d, d),
            MepShape::Rect { w, h } => ("rect", w, h),
            MepShape::Oval { w, h } => ("oval", w, h),
        };
        let num = |v: f64| label(&format!("{v:?}"));
        let sec = spec.section;
        pset(
            c,
            "mep-pset",
            id,
            "DDC_MepRun",
            ent,
            vec![
                ("System", label(&sys.name)),
                ("SystemKind", label(sys.kind.name())),
                ("Domain", label(sec.domain.name())),
                ("Shape", label(shape)),
                ("Width", num(w)),
                ("Height", num(h)),
                ("Wall", num(sec.wall)),
                ("Insulation", num(sec.insulation)),
                ("Lining", num(sec.lining)),
                ("Material", label(spec.material.map_or("", |m| m.name()))),
                (
                    "DN",
                    label(&spec.dn.map_or(String::new(), |d| d.to_string())),
                ),
                (
                    "Bend",
                    label(&spec.bend.map_or(String::new(), |b| format!("{b:?}"))),
                ),
                ("Slope", num(spec.slope)),
                ("Points", ftext(&points_text(&spec.points))),
                (
                    "Caps",
                    label(&format!("{} {}", spec.caps.0 as u8, spec.caps.1 as u8)),
                ),
            ],
        );
        // ports at run ends, nested in the segment
        let mut ports = vec![];
        for (end, p) in [
            (0u8, spec.points[0]),
            (1u8, *spec.points.last().expect("points")),
        ] {
            let pp = c.point3(p.0, p.1, p.2 - elev);
            let pax = c.axis3(pp, None);
            let ppl = c.placement(Some(g.storey_pl), pax);
            let port = c.rooted(
                "IFCDISTRIBUTIONPORT",
                "mep-port",
                id * 2 + end as i64,
                &format!("{id}:{end}"),
                &[
                    s(NONE),
                    r(ppl),
                    s(NONE),
                    s(".SOURCEANDSINK."),
                    s(&format!(".{}.", port_type(sec.domain))),
                    s(&format!(".{}.", sys.kind.ifc())),
                ],
            );
            st.ports.insert((id, end), port);
            ports.push(port);
        }
        c.rel("IFCRELNESTS", "mep-nest", id, &[r(ent), list(&ports)]);
        st.members.entry(spec.system).or_default().push(ent);
        on_level.insert(id, (spec.system, sec.domain));
        elems.push(ent);
    }
    for (i, f) in net.fittings.iter().enumerate() {
        let Some(&(sys, dom)) = on_level.get(&f.runs[0]) else {
            continue;
        };
        let Some((_, m)) = net.fitting_meshes.iter().find(|x| x.0 == i) else {
            continue;
        };
        let pds = body(c, g, m, elev);
        let (class, pre) = fitting_class(dom, &f.kind);
        let ent = c.rooted(
            class,
            "mep-fit",
            f.runs[0] * 1_000_000 + i as i64,
            &format!("{} {}", f.kind.name(), f.size),
            &[
                text(&f.size),
                r(pl),
                r(pds),
                s(NONE),
                s(&format!(".{pre}.")),
            ],
        );
        pset(
            c,
            "mep-fit-pset",
            f.runs[0] * 1_000_000 + i as i64,
            "DDC_MepFitting",
            ent,
            vec![
                ("Kind", label(f.kind.name())),
                ("Size", label(&f.size)),
                (
                    "Length",
                    format!(
                        "IFCLENGTHMEASURE({})",
                        real(if f.qty.length.is_finite() { f.qty.length } else { 0.0 })
                    ),
                ),
            ],
        );
        st.members.entry(sys).or_default().push(ent);
        elems.push(ent);
    }
    Ok(())
}

/// Systems as groups and port connections at shared run ends.
pub(super) fn finish(
    c: &mut Ctx,
    model: &Model,
    net: &MepNetwork,
    st: &MepOut,
) -> Result<(), String> {
    for sys in model.mep_systems().map_err(|e| e.to_string())? {
        let Some(m) = st.members.get(&sys.id) else {
            continue;
        };
        let g = c.rooted(
            "IFCDISTRIBUTIONSYSTEM",
            "mep-sys",
            sys.id,
            &sys.name,
            &[s(NONE), s(NONE), s(&format!(".{}.", sys.kind.ifc()))],
        );
        c.rel(
            "IFCRELASSIGNSTOGROUP",
            "mep-group",
            sys.id,
            &[list(m), s(NONE), r(g)],
        );
    }
    let mut k = 0;
    for n in &net.nodes {
        let ends: Vec<u32> = n
            .arms
            .iter()
            .filter(|a| a.1 < 2)
            .filter_map(|a| st.ports.get(a).copied())
            .collect();
        for w in ends.windows(2) {
            k += 1;
            c.rel(
                "IFCRELCONNECTSPORTS",
                "mep-conn",
                k,
                &[r(w[0]), r(w[1]), s(NONE)],
            );
        }
        // a port allows one connection: with several branches at a node — with the first
        let dev = n.port.as_ref().and_then(|p| st.comp_ports.get(p));
        if let (Some(&dp), Some(&end)) = (dev, ends.first()) {
            k += 1;
            c.rel(
                "IFCRELCONNECTSPORTS",
                "mep-dev-conn",
                k,
                &[r(dp), r(end), s(NONE)],
            );
        }
    }
    Ok(())
}

/// Electrical circuits as IfcDistributionCircuit: grouped panel and devices, serving the building.
/// Values go to `DDC_Circuit`, the device list (`"<n>"` = `"<GlobalId>/<connector>"`) to
/// `DDC_CircuitDevices` and the explicit loads (`"<GlobalId>/<connector>"`) to `DDC_CircuitLoads`.
pub(super) fn circuits(
    c: &mut Ctx,
    model: &Model,
    building: u32,
    st: &MepOut,
) -> Result<(), String> {
    let e = |x: ddc_model::Error| x.to_string();
    let systems: BTreeMap<i64, String> = model
        .mep_systems()
        .map_err(e)?
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();
    let label = |v: &str| format!("IFCLABEL({})", text(v));
    let num = |v: f64| label(&format!("{v:?}"));
    for circ in model.circuits(None).map_err(e)? {
        let sp = &circ.spec;
        let Some(&panel) = st.comps.get(&sp.panel) else {
            continue;
        };
        let devs: Vec<_> = sp
            .devices
            .iter()
            .filter(|d| st.comps.contains_key(&d.comp))
            .collect();
        let ent = c.rooted(
            "IFCDISTRIBUTIONCIRCUIT",
            "circuit",
            circ.id,
            &sp.name,
            &[s(NONE), text(&sp.number.to_string()), s(".ELECTRICAL.")],
        );
        let mut members = vec![panel];
        for d in &devs {
            let m = st.comps[&d.comp];
            if !members.contains(&m) {
                members.push(m);
            }
        }
        c.rel(
            "IFCRELASSIGNSTOGROUP",
            "circ-group",
            circ.id,
            &[list(&members), s(NONE), r(ent)],
        );
        c.rel(
            "IFCRELSERVICESBUILDINGS",
            "circ-bld",
            circ.id,
            &[r(ent), list(&[building])],
        );
        let panel_guid = c.g.get("comp", sp.panel).trim_matches('\'').to_string();
        let sys = sp
            .system
            .and_then(|i| systems.get(&i))
            .map_or("", String::as_str);
        pset(
            c,
            "circ-pset",
            circ.id,
            "DDC_Circuit",
            ent,
            vec![
                ("Number", label(&sp.number.to_string())),
                ("Kind", label(sp.kind.name())),
                ("Voltage", num(sp.voltage)),
                ("Poles", label(&sp.poles.to_string())),
                ("Breaker", num(sp.breaker)),
                ("Cable", num(sp.cable)),
                ("MaxDrop", num(sp.max_drop)),
                ("CosPhi", num(sp.cos_phi)),
                ("Laying", label(&sp.laying)),
                ("AmbientC", num(sp.ambient_c)),
                ("Conductor", label(sp.conductor.name())),
                ("LoadType", label(&sp.load_type)),
                ("Grouped", label(&sp.grouped.to_string())),
                ("PanelGuid", label(&panel_guid)),
                ("System", label(sys)),
            ],
        );
        let mut keys: Vec<(String, String)> = vec![];
        let mut loads: Vec<(String, f64)> = vec![];
        for (i, d) in devs.iter().enumerate() {
            let guid = c.g.get("comp", d.comp);
            let key = format!("{}/{}", guid.trim_matches('\''), d.connector);
            keys.push((i.to_string(), key.clone()));
            if let Some(l) = d.load {
                loads.push((key, l));
            }
        }
        if !keys.is_empty() {
            let f = keys.iter().map(|(n, k)| (n.as_str(), label(k))).collect();
            pset(c, "circ-dev-pset", circ.id, "DDC_CircuitDevices", ent, f);
        }
        if !loads.is_empty() {
            let f = loads.iter().map(|(k, l)| (k.as_str(), num(*l))).collect();
            pset(c, "circ-load-pset", circ.id, "DDC_CircuitLoads", ent, f);
        }
    }
    Ok(())
}

#[cfg(test)]
mod finite_tests {
    use super::*;
    use ddc_geom::v3;

    #[test]
    fn non_finite_triangles_are_left_out() {
        let m = Mesh {
            positions: vec![
                v3(0.0, 0.0, 0.0),
                v3(1.0, 0.0, 0.0),
                v3(0.0, 1.0, 0.0),
                v3(f64::NAN, 0.0, 1.0),
            ],
            triangles: vec![[0, 1, 2], [0, 1, 3]],
            face_of: vec![0, 0],
        };
        let f = finite_part(&m);
        assert_eq!((f.positions.len(), f.triangles.len(), f.face_of.len()), (3, 1, 1));
        assert!(f.positions.iter().all(|p| p.x.is_finite() && p.z.is_finite()));
    }
}
