// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stairs (IfcStair with IfcStairFlight, landing IfcSlab and stringer IfcMember parts through
//! IfcRelAggregates) and railings (IfcRailing). The exact model rows go to `DDC_Stair` /
//! `DDC_Railing`, so a round trip is exact; a stair or railing that does not build is skipped
//! with a warning. Each element and its parts get an IfcMaterial; stair counts and sizes also go
//! to `Pset_StairCommon`.

use ddc_geom::stair::Shape;
use ddc_model::{Model, RailingHost};

use super::mep::{body, pset};
use super::{list, r, real, s, text, Ctx, Geo, NONE};

pub(super) fn num(v: f64) -> String {
    format!("{v:?}")
}

fn opt(v: Option<f64>) -> String {
    v.map_or("-".into(), num)
}

pub(super) fn ftext(v: &str) -> String {
    format!("IFCTEXT({})", text(v))
}

pub(super) fn level_name(model: &Model, id: i64) -> String {
    model
        .sql()
        .query_row("SELECT name FROM level WHERE id=?1", [id], |r| r.get(0))
        .unwrap_or_default()
}

/// One IfcMaterial per name (shared), associated with the element and its parts.
pub(super) fn material(c: &mut Ctx, kind: &str, id: i64, name: &str, ents: &[u32]) {
    if name.is_empty() {
        return;
    }
    let m = match c.mat_ids.get(name) {
        Some(&m) => m,
        None => {
            let m = c.s.add("IFCMATERIAL", &[text(name), s(NONE), s(NONE)]);
            c.mat_ids.insert(name.to_string(), m);
            m
        }
    };
    c.rel(
        "IFCRELASSOCIATESMATERIAL",
        &format!("{kind}-mat"),
        id,
        &[list(ents), r(m)],
    );
}

/// Identity placement of one stair part, relative to the stair's own placement.
pub(super) fn part_placement(c: &mut Ctx, parent: u32) -> u32 {
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    c.placement(Some(parent), ax)
}

fn predefined(shape: &Shape) -> &'static str {
    match shape {
        Shape::Straight => ".STRAIGHT_RUN_STAIR.",
        Shape::L { .. } => ".QUARTER_TURN_STAIR.",
        Shape::U { .. } => ".HALF_TURN_STAIR.",
        Shape::Spiral { .. } => ".SPIRAL_STAIR.",
    }
}

pub(super) fn stairs(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let num_t = |v: f64| format!("IFCREAL({})", real(v));
    for id in model.stairs(level).map_err(|e| e.to_string())? {
        let sp = model.stair(id).map_err(|e| e.to_string())?;
        let Ok(l) = model.stair_layout(id) else {
            c.g.warnings
                .push(format!("stair {id} cannot be built: not exported"));
            continue;
        };
        let parts = l.meshes();
        let m = ddc_geom::mep::merge(&parts);
        if m.triangles.is_empty() {
            c.g.warnings
                .push(format!("stair {id} has no body: not exported"));
            continue;
        }
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        // meshes come per flight (then its two stringers), then the landings
        let per = if l.stringer.is_some() { 3 } else { 1 };
        let flights = l.flights.len();
        let split = parts.len() == flights * per + l.landings.len() + l.winders.len();
        let own = if split {
            s(NONE)
        } else {
            r(body(c, g, &m, elev))
        };
        let q = l.quantities();
        let ent = c.rooted(
            "IFCSTAIR",
            "stair",
            id,
            &format!("Stair {id}"),
            &[s(NONE), r(pl), own, s(NONE), s(predefined(&l.shape))],
        );
        let mut all = vec![ent];
        if split {
            let mut kids = Vec::with_capacity(parts.len());
            for (k, part) in parts.iter().enumerate() {
                let pds = r(body(c, g, part, elev));
                let ppl = part_placement(c, pl);
                let (class, kind, label, rest): (&str, &str, String, Vec<String>) =
                    if k < flights * per {
                        let fi = k / per;
                        if k % per == 0 {
                            let n = l.flights[fi].risers;
                            (
                                "IFCSTAIRFLIGHT",
                                "stair-flight",
                                format!("Flight {}", fi + 1),
                                vec![
                                    n.to_string(),
                                    n.saturating_sub(1).to_string(),
                                    real(l.riser),
                                    real(l.tread),
                                    s(".STRAIGHT."),
                                ],
                            )
                        } else {
                            (
                                "IFCMEMBER",
                                "stair-stringer",
                                format!("Stringer {}", k - fi * per),
                                vec![s(".STRINGER.")],
                            )
                        }
                    } else if k < flights * per + l.landings.len() {
                        (
                            "IFCSLAB",
                            "stair-landing",
                            format!("Landing {}", k - flights * per + 1),
                            vec![s(".LANDING.")],
                        )
                    } else {
                        // a spiral's treads: one winder flight each, after the landings
                        (
                            "IFCSTAIRFLIGHT",
                            "stair-winder",
                            format!("Winder {}", k - flights * per - l.landings.len() + 1),
                            vec![1.to_string(), 1.to_string(), real(l.riser), real(l.tread), s(".SPIRAL.")],
                        )
                    };
                let mut args = vec![s(NONE), r(ppl), pds, s(NONE)];
                args.extend(rest);
                kids.push(c.rooted(class, &format!("{kind}/{k}"), id, &label, &args));
            }
            c.rel("IFCRELAGGREGATES", "stair-parts", id, &[r(ent), list(&kids)]);
            all.extend(kids);
        }
        material(c, "stair", id, &sp.material, &all);
        let len = |v: f64| format!("IFCPOSITIVELENGTHMEASURE({})", real(v));
        pset(
            c,
            "stair-common",
            id,
            "Pset_StairCommon",
            ent,
            vec![
                ("NumberOfRiser", format!("IFCINTEGER({})", q.risers)),
                ("NumberOfTreads", format!("IFCINTEGER({})", q.treads)),
                ("RiserHeight", len(q.riser)),
                ("TreadLength", len(q.tread)),
            ],
        );
        let (kind, left, first, gap) = match sp.shape {
            Shape::Straight => ("straight", false, None, 0.0),
            Shape::L { left, first } => ("l", left, first, 0.0),
            Shape::U { left, first, gap } => ("u", left, first, gap),
            Shape::Spiral { .. } => ("spiral", false, None, 0.0),
        };
        let (radius, core, turn) = match sp.shape {
            Shape::Spiral { radius, core, turn } => (radius, core, turn),
            _ => (0.0, 0.0, 0.0),
        };
        let spec = [
            num(sp.base_offset),
            num(sp.top_offset),
            num(sp.origin.0),
            num(sp.origin.1),
            num(sp.angle),
            num(sp.width),
            num(sp.max_riser),
            opt(sp.tread),
            num(sp.landing),
            num(sp.thickness),
            opt(sp.stringer.map(|v| v.0)),
            opt(sp.stringer.map(|v| v.1)),
        ]
        .join(" ");
        let f = vec![
            ("Spec", ftext(&spec)),
            ("Shape", ftext(kind)),
            ("Left", ftext(if left { "1" } else { "0" })),
            ("First", ftext(&first.map_or("-".into(), |v| v.to_string()))),
            ("Gap", ftext(&num(gap))),
            ("Radius", ftext(&num(radius))),
            ("Core", ftext(&num(core))),
            ("Turn", ftext(&num(turn))),
            ("TopLevel", ftext(&level_name(model, sp.top_level))),
            ("Material", ftext(&sp.material)),
            ("Risers", format!("IFCINTEGER({})", q.risers)),
            ("Treads", format!("IFCINTEGER({})", q.treads)),
            ("RiserHeight", num_t(q.riser)),
            ("TreadLength", num_t(q.tread)),
            ("Width", num_t(sp.width)),
            ("Volume", num_t(q.volume)),
        ];
        pset(c, "stair-pset", id, "DDC_Stair", ent, f);
        elems.push(ent);
    }
    Ok(())
}

pub(super) fn railings(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    for id in model.railings(level).map_err(|e| e.to_string())? {
        let sp = model.railing(id).map_err(|e| e.to_string())?;
        let parts = model.railing_meshes(id).unwrap_or_default();
        let m = ddc_geom::mep::merge(&parts);
        if m.triangles.is_empty() {
            c.g.warnings
                .push(format!("railing {id} cannot be built: not exported"));
            continue;
        }
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let pds = body(c, g, &m, elev);
        let ent = c.rooted(
            "IFCRAILING",
            "railing",
            id,
            &format!("Railing {id}"),
            &[s(NONE), r(pl), r(pds), s(NONE), s(".BALUSTRADE.")],
        );
        material(c, "railing", id, &sp.material, &[ent]);
        let spec = [
            sp.height,
            sp.post_spacing,
            sp.baluster_spacing,
            sp.post,
            sp.baluster,
            sp.rail.0,
            sp.rail.1,
        ]
        .map(num)
        .join(" ");
        let mut f = vec![("Spec", ftext(&spec)), ("Material", ftext(&sp.material))];
        match &sp.host {
            RailingHost::Stair { stair, side } => {
                let guid = c.g.get("stair", *stair);
                f.push(("Host", ftext("stair")));
                f.push(("HostStair", ftext(guid.trim_matches('\''))));
                f.push(("Side", ftext(&side.to_string())));
            }
            RailingHost::Path(p) => {
                let pts: Vec<String> = p
                    .iter()
                    .map(|q| format!("{},{},{}", num(q.0), num(q.1), num(q.2)))
                    .collect();
                f.push(("Host", ftext("path")));
                f.push(("Path", ftext(&pts.join(";"))));
            }
        }
        let q = model.railing_quantities(id).map_err(|e| e.to_string())?;
        f.push(("Length", format!("IFCREAL({})", real(q.length))));
        f.push(("Posts", format!("IFCINTEGER({})", q.posts)));
        f.push(("Balusters", format!("IFCINTEGER({})", q.balusters)));
        pset(c, "railing-pset", id, "DDC_Railing", ent, f);
        elems.push(ent);
    }
    Ok(())
}
