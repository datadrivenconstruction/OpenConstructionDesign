// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A standalone vertical storage tank: a flat-bottomed shell under a cone roof, flanged nozzles
//! on the shell at different heights and directions and one on the roof, a manway near the
//! bottom, a caged ladder up the shell and a ring foundation under it. Each nozzle and the
//! manway is its own element, so it can be tagged, scheduled and piped later.

use std::collections::BTreeMap;
use std::f64::consts::{FRAC_PI_2, PI};

use ddc_geom::math::{v3, V3};
use serde_json::json;

use crate::project::rules::rule;

use super::equip::F;
use super::layout::STEEL_R;
use super::model::with_type;
use super::why::{add, s2};
use super::{
    size, Brief, Clear, Dims, EqKind, Equip, Hall, Kit, KitComp, KitFamily, KitFooting, Piping, Plant, Report, Row, Schedules, Spec,
    Totals,
};

const TAG: &str = "TK-1";
/// Nozzles of an atmospheric tank are flanged to this pressure class.
const PN: u32 = 16;
/// How far a nozzle neck stands out past half its flange, and how deep it sits in the shell, m.
const NECK: f64 = 0.15;
const EMBED: f64 = 0.02;
/// Shell nozzles keep this share of the shell height from its bottom and top.
const SIDE_K: f64 = 0.15;
/// The roof nozzle stands at this share of the radius from the axis.
const ROOF_AT: f64 = 0.5;
/// Manway: sill above the bottom, how far it stands out, its cover's thickness and rim, m.
const MANWAY_SILL: f64 = 0.3;
const MANWAY_OUT: f64 = 0.3;
const COVER_T: f64 = 0.03;
const COVER_RIM: f64 = 0.08;
/// Gap between the shell and the ladder rungs, m.
const LADDER_OFF: f64 = 0.2;

/// Where each thing stands round the shell: the manway first, then the shell nozzles, then the
/// ladder, evenly spaced, so no two share a direction.
fn slots(n_shell: usize) -> Vec<f64> {
    let n = n_shell + 2;
    (0..n).map(|k| FRAC_PI_2 + k as f64 * 2.0 * PI / n as f64).collect()
}

fn nozzle_family(dn: u32, up: bool) -> KitFamily {
    let name = format!("Plant_nozzle_flanged_DN{dn}_PN{}_{}", PN, if up { "U" } else { "H" });
    let mut f = F::named(&name, "equipment", "IfcPipeFitting NOTDEFINED");
    let od = size::pipe(Spec::C1, dn).od;
    let fl = size::flange_od(dn, PN);
    let ft = size::flange_t(dn);
    let proj = fl / 2.0 + NECK;
    let axis = if up { v3(0.0, 0.0, 1.0) } else { v3(1.0, 0.0, 0.0) };
    f.cyl("Neck", axis * -EMBED, axis * (proj - ft), od / 2.0, "Steel", false);
    f.cyl("Flange", axis * (proj - ft), axis * proj, fl / 2.0, "Steel", false);
    f.connector("N", "pipe", axis * proj, axis, od);
    KitFamily { name, text: with_type(f.s), own: false }
}

fn manway_family(md: f64) -> KitFamily {
    let name = format!("Plant_manway_flanged_D{}", (md * 1000.0).round() as i64);
    let mut f = F::named(&name, "equipment", "IfcPipeFitting NOTDEFINED");
    let x = v3(1.0, 0.0, 0.0);
    f.cyl("Neck", x * -EMBED, x * (MANWAY_OUT - COVER_T), md / 2.0, "Steel", false);
    f.cyl("Cover", x * (MANWAY_OUT - COVER_T), x * MANWAY_OUT, md / 2.0 + COVER_RIM, "Steel", false);
    KitFamily { name, text: with_type(f.s), own: false }
}

/// Rails and rungs up to `h`, the rails on past it as a hand-hold, and a cage of hoops and
/// straps from the headroom up when the ladder is taller than the rules allow uncaged.
fn ladder_family(name: &str, h: f64) -> (KitFamily, usize, bool) {
    let mut f = F::named(name, "generic", "IfcMember NOTDEFINED");
    let w = rule("plant.ladder.width") / 2.0;
    let top = h + rule("plant.platform.handrail");
    for y in [-w, w] {
        f.bar("Rail", v3(0.0, y, 0.0), v3(0.0, y, top), STEEL_R, "Steel");
    }
    let rung = rule("plant.ladder.rung");
    let n = (h / rung).floor() as usize;
    for k in 1..=n {
        let z = k as f64 * rung;
        f.bar("Rung", v3(0.0, -w, z), v3(0.0, w, z), STEEL_R, "Steel");
    }
    let caged = h > rule("plant.ladder.cage_from");
    if caged {
        let (hoop, step, z0) = (rule("plant.ladder.hoop"), rule("plant.ladder.hoop_step"), rule("plant.access.headroom"));
        let mut z = z0;
        while z <= top {
            let c = [v3(0.0, -w, z), v3(hoop, -w, z), v3(hoop, w, z), v3(0.0, w, z)];
            for p in c.windows(2) {
                f.bar("Hoop", p[0], p[1], STEEL_R, "Steel");
            }
            z += step;
        }
        for y in [-w, 0.0, w] {
            f.bar("Strap", v3(hoop, y, z0), v3(hoop, y, top), STEEL_R, "Steel");
        }
    }
    (KitFamily { name: name.to_string(), text: with_type(f.s), own: true }, n, caged)
}

fn on_shell(r: f64, a: f64, z: f64) -> V3 {
    v3(r * a.cos(), r * a.sin(), z)
}

pub(super) fn build(brief: Brief) -> Result<Plant, String> {
    let d = brief.diameter_m.unwrap_or_else(|| rule("plant.tank.d"));
    let h = brief.height_m.unwrap_or_else(|| rule("plant.tank.hd") * d);
    let n = brief.nozzles.unwrap_or_else(|| rule("plant.tank.nozzles").round() as usize);
    let md = rule("plant.tank.manway_d");
    if h < MANWAY_SILL + md + NECK {
        return Err(format!("a tank {h} m tall has no room for its {md} m manway"));
    }
    let r = d / 2.0;
    let rise = rule("plant.tank.roof") * d;
    let vol = PI * r * r * h;
    let dn = size::pipe(Spec::C1, rule("plant.tank.nozzle_dn").round() as u32).dn;
    let (n_shell, roof) = if n >= 2 { (n - 1, true) } else { (1, false) };
    let at = slots(n_shell);
    let mut why = vec![];

    let mut e = Equip {
        tag: TAG.to_string(),
        kind: EqKind::Tank,
        role: "storage",
        standby: false,
        duty: vec![("volume_m3", vol)],
        dims: Dims { d, h, ..Dims::default() },
        nozzles: vec![],
        family: String::new(),
        boxes: vec![(v3(-r, -r, 0.0), v3(r, r, h + rise))],
        clear: Clear::default(),
        flue: None,
        at: v3(0.0, 0.0, 0.0),
        rot: 0,
        row: Row::North,
    };
    let unit = e.family_name();
    let mut f = F::named(&unit, "equipment", EqKind::Tank.ifc());
    f.revolve("Shell", &[(0.0, 0.0), (r, 0.0), (r, h), (0.0, h + rise)], "Steel");
    e.family = f.s;

    let mut kit = Kit::default();
    let mut fams: BTreeMap<String, KitFamily> = BTreeMap::new();
    fams.insert(unit.clone(), KitFamily { name: unit.clone(), text: with_type(e.family.clone()), own: true });
    kit.comps.push(KitComp { key: format!("unit:{TAG}"), family: unit, at: e.at, rotation: 0.0, ports: vec![] });

    let mut heights = vec![];
    for k in 0..n_shell {
        let t = if n_shell > 1 { k as f64 / (n_shell - 1) as f64 } else { 0.0 };
        let z = h * (SIDE_K + (1.0 - 2.0 * SIDE_K) * t);
        let fam = nozzle_family(dn, false);
        kit.comps.push(KitComp { key: format!("nozzle:N{}", k + 1), family: fam.name.clone(), at: on_shell(r, at[k + 1], z), rotation: at[k + 1], ports: vec![] });
        fams.insert(fam.name.clone(), fam);
        heights.push(s2(z));
    }
    if roof {
        let a = at[n_shell + 1];
        let rho = ROOF_AT * r;
        let z = h + rise * (1.0 - rho / r);
        let fam = nozzle_family(dn, true);
        kit.comps.push(KitComp { key: format!("nozzle:N{n}"), family: fam.name.clone(), at: on_shell(rho, a, z), rotation: 0.0, ports: vec![] });
        fams.insert(fam.name.clone(), fam);
        heights.push(s2(z));
    }
    let mw = manway_family(md);
    kit.comps.push(KitComp { key: "manway:M1".into(), family: mw.name.clone(), at: on_shell(r, at[0], MANWAY_SILL + md / 2.0), rotation: at[0], ports: vec![] });
    fams.insert(mw.name.clone(), mw);
    add(&mut why, TAG, "plant.tank.nozzle_dn", "plant_tank_nozzles", vec![("tag", TAG.to_string()), ("n", n.to_string()), ("dn", dn.to_string()), ("z", heights.join(", ")), ("md", s2(md))]);

    let la = at[n_shell + 1];
    let (lf, rungs, caged) = ladder_family(&format!("Ladder_{}", TAG.replace('-', "_")), h);
    kit.comps.push(KitComp { key: format!("ladder:{TAG}"), family: lf.name.clone(), at: on_shell(r + LADDER_OFF, la, 0.0), rotation: la, ports: vec![] });
    fams.insert(lf.name.clone(), lf);
    let (rid, key) = if caged { ("plant.ladder.cage_from", "plant_ladder_cage") } else { ("plant.ladder.rung", "plant_ladder") };
    add(&mut why, TAG, rid, key, vec![("tag", TAG.to_string()), ("n", rungs.to_string())]);

    let (rw, depth) = (rule("plant.tank.ring_w"), rule("plant.tank.ring_depth"));
    let bulge = (PI / 8.0).tan();
    for k in 0..4 {
        let (a, b) = (k as f64 * FRAC_PI_2, (k + 1) as f64 * FRAC_PI_2);
        kit.footings.push(KitFooting { key: format!("ring:{TAG}:{k}"), a: on_shell(r, a, 0.0), b: on_shell(r, b, 0.0), bulge, width: rw, depth });
    }
    add(&mut why, TAG, "plant.tank.ring_w", "plant_tank_ring", vec![("tag", TAG.to_string()), ("d", s2(d)), ("w", s2(rw)), ("depth", s2(depth))]);

    let rid = match (brief.diameter_m, brief.height_m) {
        (None, _) => "plant.tank.d",
        (_, None) => "plant.tank.hd",
        _ => "plant.tank.roof",
    };
    add(&mut why, TAG, rid, "plant_storage_tank", vec![("tag", TAG.to_string()), ("d", s2(d)), ("h", s2(h)), ("vol", s2(vol)), ("rise", s2(rise))]);
    kit.families = fams.into_values().collect();

    let ext = r + rule("plant.clear.side");
    let hall = Hall { x0: -ext, x1: ext, y0: -ext, y1: ext, z: h + rise, walk: (0.0, 0.0), tiers: vec![], rack_z: 0.0, drain_z: 0.0 };
    let schedules = Schedules {
        equipment: vec![json!({"tag": TAG, "kind": EqKind::Tank.name(), "d": s2(d), "h": s2(h), "roof_rise": s2(rise),
                               "volume_m3": s2(vol), "nozzles": n, "nozzle_dn": dn, "manway_d": s2(md)})],
        lines: vec![],
        valves: vec![],
        instruments: vec![],
        fittings: vec![],
        supports: vec![],
        steel: vec![],
        platforms: vec![],
        totals: Totals::default(),
    };
    let report = Report { ok: true, issues: vec![], nozzles: n, connected: 0, pipe_pairs: 0, min_margin: 0.0, valves: 0, valves_in_reach: 0 };
    Ok(Plant {
        brief,
        equipment: vec![e],
        lines: vec![],
        hall,
        zones: vec![],
        platforms: vec![],
        steel: vec![],
        flues: vec![],
        bund: None,
        piping: Piping::default(),
        schedules,
        report,
        why,
        kit,
    })
}
