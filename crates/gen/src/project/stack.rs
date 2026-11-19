//! Vertical skeleton: what runs through the storeys is decided once for the whole building,
//! before any storey is laid out. The core, the lift shaft and the column grid come from the
//! one frame every storey of a bar shares (`build.rs`); the wet stack comes from here. Wet
//! rooms sit on the risers beside the core on every storey, and a storey under a wet storey
//! that asked for none carries the stack's toilets down (`mep.wet_stack_fill`). The checks
//! report where the skeleton broke, each naming its rule: wet rooms off the stack
//! (`mep.riser_offset`), shafts that move (`core.stack`), columns on no column
//! (`grid.transfer`).

use super::nav::{dist, finding, Finding};
use super::why::{RuleFix, Why};
use super::{names, GProject, ProjectSpec, RoomKind, RoomReq, Typology};
use serde_json::json;

type P = (f64, f64);

pub fn is_wet(k: RoomKind) -> bool {
    matches!(k, RoomKind::Bath | RoomKind::Wc)
}

/// The rooms every storey lays out, and the storeys given the wet stack of a storey above.
#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    pub storeys: Vec<Vec<RoomReq>>,
    /// `(storey, from)`: the storey took the toilets of storey `from`'s wet rooms.
    pub filled: Vec<(usize, usize)>,
}

/// A wet room of the stack as a toilet of the same size: a storey that asked for no wet
/// rooms gets toilets, and the same size keeps it on the riser.
fn toilet(r: &RoomReq, ru: bool) -> RoomReq {
    if r.kind == RoomKind::Wc {
        return r.clone();
    }
    RoomReq {
        name: names::kind_name(RoomKind::Wc, ru).to_string(),
        kind: RoomKind::Wc,
        area: r.area,
        count: r.count,
        tags: Default::default(),
    }
}

/// The skeleton of a spec: the brief's rooms per storey with the wet stack carried down.
pub fn skeleton(spec: &ProjectSpec) -> Skeleton {
    let n = spec.floors as usize;
    let mut storeys: Vec<Vec<RoomReq>> = (0..n).map(|k| spec.floor_rooms(k)).collect();
    let mut filled = vec![];
    // apartments carry their own wet rooms, the same on every storey
    if spec.rules.get("mep.wet_stack_fill") != 0.0 && spec.typology != Typology::ApartmentSection {
        let wet = |rooms: &[RoomReq]| rooms.iter().any(|r| is_wet(r.kind) && r.count > 0);
        let mut above: Option<usize> = None;
        // top down, so a stack carried to one storey carries on to the storeys under it
        for k in (0..n).rev() {
            if !wet(&storeys[k]) {
                let Some(a) = above else { continue };
                let stack: Vec<RoomReq> = storeys[a]
                    .iter()
                    .filter(|r| is_wet(r.kind) && r.count > 0)
                    .map(|r| toilet(r, spec.ru))
                    .collect();
                storeys[k].extend(stack);
                filled.push((k, a));
            }
            above = Some(k);
        }
        filled.reverse();
    }
    Skeleton { storeys, filled }
}

impl ProjectSpec {
    /// Rooms laid out on storey `k` (from 0): those asked, plus the wet stack the skeleton
    /// carries down to it.
    pub fn storey_rooms(&self, k: usize) -> Vec<RoomReq> {
        skeleton(self)
            .storeys
            .get(k)
            .cloned()
            .unwrap_or_else(|| self.floor_rooms(k))
    }
}

/// The reasons the skeleton gives: which storeys carry the stack of which.
pub fn explain(spec: &ProjectSpec) -> Vec<Why> {
    skeleton(spec)
        .filled
        .iter()
        .map(|&(k, a)| {
            Why::new(
                format!("storey:{k}"),
                "mep.wet_stack_fill",
                json!(true),
                spec.rules.source("mep.wet_stack_fill").map_or("builtin", |l| l.name()).to_string(),
                "wet_stack",
                vec![("k", k.to_string()), ("a", a.to_string())],
            )
        })
        .collect()
}

fn mean(poly: &[P]) -> P {
    let n = poly.len().max(1) as f64;
    (
        poly.iter().map(|p| p.0).sum::<f64>() / n,
        poly.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

fn over(at: P, below: &[P], reach: f64) -> bool {
    below.iter().any(|b| dist(*b, at) <= reach + 1e-9)
}

/// Wet rooms off the stack, shafts that move and columns on no column; one finding per rule.
pub(super) fn check(spec: &ProjectSpec, p: &GProject, out: &mut Vec<Finding>) {
    let reach = spec.rules.get("mep.riser_offset");
    let tol = spec.rules.get("core.stack");
    let wet = |k: usize| -> Vec<(String, P)> {
        let st = &p.storeys[k];
        st.layout
            .rooms
            .iter()
            .zip(&st.rooms)
            .filter(|(_, r)| is_wet(r.kind))
            .map(|(g, r)| (r.number.clone(), mean(&g.polygon)))
            .collect()
    };
    let (mut off, mut wet_all, mut first_off): (Vec<String>, usize, Option<usize>) = (vec![], 0, None);
    let (mut moved, mut shafts, mut first_moved) = (0usize, 0usize, None);
    let (mut loose, mut cols, mut first_loose) = (0usize, 0usize, None);
    for k in 1..p.storeys.len() {
        let below: Vec<P> = wet(k - 1).into_iter().map(|w| w.1).collect();
        for (num, at) in wet(k) {
            wet_all += 1;
            if !over(at, &below, reach) {
                off.push(num);
                first_off.get_or_insert(k);
            }
        }
        let (lo, hi) = (&p.storeys[k - 1].layout, &p.storeys[k].layout);
        let under: Vec<P> = lo.shafts.iter().map(|s| mean(&s.ring)).collect();
        for s in &hi.shafts {
            shafts += 1;
            if !over(mean(&s.ring), &under, tol) {
                moved += 1;
                first_moved.get_or_insert(k);
            }
        }
        let under: Vec<P> = lo.columns.iter().map(|c| c.at).collect();
        for c in &hi.columns {
            cols += 1;
            if !over(c.at, &under, tol) {
                loose += 1;
                first_loose.get_or_insert(k);
            }
        }
    }
    if let Some(k) = first_off {
        let shown: Vec<&str> = off.iter().take(super::nav::SHOWN).map(String::as_str).collect();
        // the fix exists only while the stack is not carried down
        let fix = if spec.rules.get("mep.wet_stack_fill") == 0.0 {
            RuleFix::keyed("mep.wet_stack_fill", json!(true), "fix_wet_stack", vec![])
            .ok()
        } else {
            None
        };
        out.push(finding(
            spec,
            "mep.riser_offset",
            Some(k),
            off.len() as f64 / wet_all.max(1) as f64,
            format!(
                "{} of {wet_all} wet room(s) stand more than {reach} m from any wet room on the storey below ({})",
                off.len(),
                shown.join(", ")
            ),
            fix,
        ));
    }
    if let Some(k) = first_moved {
        out.push(finding(
            spec,
            "core.stack",
            Some(k),
            moved as f64 / shafts.max(1) as f64,
            format!("{moved} of {shafts} shaft(s) do not stand over a shaft of the storey below within {tol} m"),
            None,
        ));
    }
    let allowed = spec.rules.get("grid.transfer");
    if let Some(k) = first_loose.filter(|_| loose as f64 > allowed) {
        out.push(finding(
            spec,
            "grid.transfer",
            Some(k),
            (loose as f64 - allowed) / cols.max(1) as f64,
            format!("{loose} of {cols} column(s) stand on no column below, {allowed} allowed on transfer beams"),
            None,
        ));
    }
}
