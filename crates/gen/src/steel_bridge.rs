// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What a footbridge needs besides its structure: guard rails on both deck edges, approach
//! ramps or stairs with landings, every number a `steel.bridge.*` rule.

use crate::project::rules::RuleSet;
use ddc_geom::ramp::RampDef;
use ddc_geom::stair::{Shape, StairDef};
use ddc_geom::steel::lattice::{Frame, Guard, MemRole, Stairway};
use ddc_geom::{v2, v3, V2};

/// Counts of what was added, for the metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fitted {
    pub stairs: usize,
    pub stair_risers: usize,
    pub stair_landings: usize,
    pub stair_length: f64,
    pub ramps: usize,
    pub ramp_length: f64,
    pub ramp_landings: usize,
    pub guard_runs: usize,
    pub guard_posts: usize,
    pub guard_length: f64,
}

type Pt = (f64, f64, f64);

/// Length of a guard path in 3D, m.
pub fn path_length(p: &[Pt]) -> f64 {
    p.windows(2).map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2) + (w[1].2 - w[0].2).powi(2)).sqrt()).sum()
}

/// Posts of a guard: one at every vertex and as many between as the spacing needs.
pub fn post_count(p: &[Pt], spacing: f64) -> usize {
    1 + p.windows(2).map(|w| (path_length(w) / spacing - 1e-9).ceil().max(1.0) as usize).sum::<usize>()
}

/// Width of an approach: the rule, or when it is 0 the clear width between the deck guards, so
/// the guards of the approach meet those of the deck without a step.
fn approach_width(rs: &RuleSet, id: &str, width: f64) -> f64 {
    let inner = width - 2.0 * rs.get("steel.bridge.rail_inset");
    let r = rs.get(id);
    if r > 0.0 { r.min(inner) } else { inner }
}

fn add_guard(f: &mut Frame, out: &mut Fitted, rs: &RuleSet, path: Vec<Pt>) {
    let (height, post_spacing, infill_spacing) = (rs.get("steel.bridge.rail_height"), rs.get("steel.bridge.rail_post_spacing"), rs.get("steel.bridge.rail_infill_spacing"));
    out.guard_runs += 1;
    out.guard_posts += post_count(&path, post_spacing);
    out.guard_length += path_length(&path);
    f.guards.push(Guard { path, height, post_spacing, infill_spacing });
}

/// Guard rails along both deck edges, inside the girders, from abutment to abutment.
pub fn guards(f: &mut Frame, rs: &RuleSet, span: f64, width: f64, deck_z: f64) -> Fitted {
    let inset = rs.get("steel.bridge.rail_inset");
    let mut out = Fitted::default();
    for y in [inset, width - inset] {
        add_guard(f, &mut out, rs, vec![(0.0, y, deck_z), (span, y, deck_z)]);
    }
    out
}

/// Plan length of a ramp for `rise`: the run at the slope plus a landing after every full flight.
pub fn ramp_length(rise: f64, slope: f64, max_run: f64, landing: f64) -> f64 {
    let run = rise / slope;
    let flights = (run / max_run - 1e-9).ceil().max(1.0);
    run + (flights - 1.0) * landing
}

/// A ramp down to the ground from each abutment, along the deck axis: at most the rule slope,
/// a landing after every rule rise, guards on both edges (rises with the flights, flat on the
/// landings).
pub fn ramps(f: &mut Frame, rs: &RuleSet, span: f64, width: f64, deck_z: f64) -> Result<Fitted, String> {
    let slope = rs.get("steel.bridge.ramp_slope");
    let (landing, rise_max) = (rs.get("steel.bridge.ramp_landing"), rs.get("steel.bridge.ramp_rise_max"));
    let w = approach_width(rs, "steel.bridge.ramp_width", width);
    let max_run = rise_max / slope;
    let len = ramp_length(deck_z, slope, max_run, landing);
    let mut out = Fitted::default();
    for (foot, top) in [(-len, 0.0), (span + len, span)] {
        let def = RampDef { path: vec![v2(foot, width / 2.0), v2(top, width / 2.0)], base_z: 0.0, rise: deck_z, width: w, slope, thickness: rs.get("steel.bridge.ramp_thickness"), landing, max_run };
        let lay = def.layout()?;
        for side in [-1.0, 1.0] {
            let mut path = vec![];
            for fl in &lay.flights {
                let off: V2 = v2(-fl.dir.y, fl.dir.x) * (side * w / 2.0);
                let (a, b) = (fl.start + off, fl.start + fl.dir * fl.len + off);
                path.extend([(a.x, a.y, fl.z0), (b.x, b.y, fl.z1)]);
            }
            add_guard(f, &mut out, rs, path);
        }
        out.ramps += 1;
        out.ramp_length += len;
        out.ramp_landings += lay.landings.len();
        f.ramps.push(def);
    }
    Ok(out)
}

/// Whether the ramps at the rule slope would be longer than the rule allows for this deck.
pub fn ramp_too_long(rs: &RuleSet, deck_z: f64) -> bool {
    let slope = rs.get("steel.bridge.ramp_slope");
    ramp_length(deck_z, slope, rs.get("steel.bridge.ramp_rise_max") / slope, rs.get("steel.bridge.ramp_landing")) > rs.get("steel.bridge.ramp_max_len")
}

/// Stairs down to the ground from each abutment, along the deck axis, in flights of at most the
/// rule rise with a landing between them; the last tread of the top flight ends at the deck
/// edge. A landing is a frame of beams on four posts with a guard on both sides; the model puts
/// guards on both sides of every flight.
pub fn stairs(f: &mut Frame, rs: &RuleSet, span: f64, width: f64, deck_z: f64) -> Result<Fitted, String> {
    let w = approach_width(rs, "steel.bridge.stair_width", width);
    let (max_rise, landing) = (rs.get("steel.bridge.stair_flight_rise"), rs.get("steel.bridge.stair_landing"));
    let k = (deck_z / max_rise - 1e-9).ceil().max(1.0) as usize;
    let rise = deck_z / k as f64;
    let mut def = StairDef {
        origin: v2(0.0, width / 2.0),
        angle: 0.0,
        base_z: 0.0,
        height: rise,
        width: w,
        max_riser: rs.get("steel.bridge.stair_riser"),
        tread: None,
        landing: w,
        thickness: rs.get("steel.bridge.stair_thickness"),
        stringer: None,
        shape: Shape::Straight,
    };
    let lay = def.layout().map_err(|e| format!("stairs: {e:?}"))?;
    let len = lay.tread * (lay.risers - 1) as f64;
    let (y0, y1) = (width / 2.0 - w / 2.0, width / 2.0 + w / 2.0);
    let (height, post_spacing, infill_spacing) = (rs.get("steel.bridge.rail_height"), rs.get("steel.bridge.rail_post_spacing"), rs.get("steel.bridge.rail_infill_spacing"));
    let mut out = Fitted::default();
    for (dir, angle, x_top) in [(-1.0, 0.0, 0.0), (1.0, std::f64::consts::PI, span)] {
        for t in 0..k {
            let end = x_top + dir * t as f64 * (len + landing);
            let foot = end + dir * len;
            def.origin = v2(foot, width / 2.0);
            def.angle = angle;
            def.base_z = rise * (k - 1 - t) as f64;
            // the guards of the flight, which the model puts on both stair edges
            for edge in def.layout().map_err(|e| format!("stairs: {e:?}"))?.edge_paths() {
                let path: Vec<Pt> = edge.iter().map(|p| (p.x, p.y, p.z)).collect();
                out.guard_runs += 1;
                out.guard_posts += post_count(&path, post_spacing);
                out.guard_length += path_length(&path);
            }
            f.stairs.push(Stairway { def: def.clone(), height, post_spacing, infill_spacing });
            out.stairs += 1;
            out.stair_risers += lay.risers;
            out.stair_length += len;
            if t + 1 < k {
                let (x0, x1, z) = (foot, foot + dir * landing, def.base_z);
                for x in [x0, x1] {
                    f.add(v3(x, y0, z), v3(x, y1, z), "IPE 200", MemRole::Beam, v3(0.0, 0.0, 1.0));
                }
                for y in [y0, y1] {
                    f.add(v3(x0, y, z), v3(x1, y, z), "IPE 200", MemRole::Beam, v3(0.0, 0.0, 1.0));
                    add_guard(f, &mut out, rs, vec![(x0, y, z), (x1, y, z)]);
                    for x in [x0, x1] {
                        f.add(v3(x, y, 0.0), v3(x, y, z), "CHS 114.3x6", MemRole::Column, v3(1.0, 0.0, 0.0));
                    }
                }
                out.stair_landings += 1;
                out.stair_length += landing;
            }
        }
    }
    Ok(out)
}
