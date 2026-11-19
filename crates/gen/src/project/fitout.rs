// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Appliances of a dwelling. The fit-out rules place them (the kitchen run takes the cooker,
//! dishwasher and fridge, the bath or kitchen the washing machine); here a dwelling whose
//! rooms left the washing machine out gets one where a free spot against a plain wall
//! remains, clear of the furniture, of the door swings and of the space in front of it.
//! Nothing a dwelling already has is added again.

use super::svc::{self, Room, Side, R};
use super::{GProject, PRole, ProjectSpec, RoomKind, Typology};
use crate::GFixture;

/// Families counted as the appliances of a dwelling.
pub(crate) const APPLIANCES: [&str; 4] = ["Cooker", "Dishwasher", "Washing Machine", "Refrigerator"];

const W: f64 = 0.6;
/// Kept free in front of an appliance to open and load it.
const FRONT: f64 = 0.6;

/// Where nothing may stand: fixtures on the floor and the swing of every door.
fn blocked(p: &GProject, k: usize, room: &Room) -> Vec<R> {
    let l = &p.storeys[k].layout;
    let mut out: Vec<R> = l
        .fixtures
        .iter()
        .filter(|f| !matches!(f.family, "Supply Diffuser" | "Exhaust Grille" | "Ceiling Light"))
        .filter_map(svc::footprint)
        .collect();
    for sd in &room.sides {
        for g in sd.gaps.iter().filter(|g| g.door) {
            let w = g.s1 - g.s0;
            out.push(svc::bbox(&[sd.at(g.s0, 0.0), sd.at(g.s1, w)]));
        }
    }
    out
}

/// The appliance box at `s` along side `j` when it and the space in front of it are free.
fn spot(room: &Room, j: usize, s: f64, taken: &[R]) -> Option<R> {
    let sd: &Side = &room.sides[j];
    if sd.half <= 0.0 || s < 0.0 || s + W > sd.len + svc::EPS || !sd.free(s, s + W, 0.0, 0.0, 0.9) {
        return None;
    }
    let body = svc::bbox(&[sd.at(s, 0.0), sd.at(s + W, W)]);
    let front = svc::bbox(&[sd.at(s, W), sd.at(s + W, W + FRONT)]);
    let inside = |r: &R| r[0] >= room.clear[0] - svc::EPS && r[1] >= room.clear[1] - svc::EPS && r[2] <= room.clear[2] + svc::EPS && r[3] <= room.clear[3] + svc::EPS;
    let clear = |r: &R| !taken.iter().any(|t| svc::hit(t, r, -svc::EPS));
    (inside(&body) && inside(&front) && clear(&body) && clear(&front)).then_some(body)
}

fn dist(a: &R, b: &R) -> f64 {
    let dx = (b[0] - a[2]).max(a[0] - b[2]).max(0.0);
    let dy = (b[1] - a[3]).max(a[1] - b[3]).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// Puts one appliance in the room, nearest to the `near` boxes when there are any.
fn put(room: &Room, fam: &'static str, near: &[R], taken: &mut Vec<R>) -> Option<GFixture> {
    let mut best: Option<(f64, usize, f64, R)> = None;
    for j in 0..4 {
        let len = room.sides[j].len;
        let mut s = 0.0;
        while s + W <= len + svc::EPS {
            if let Some(b) = spot(room, j, s, taken) {
                let score = near.iter().map(|n| dist(n, &b)).fold(f64::INFINITY, f64::min);
                let score = if score.is_finite() { score } else { 0.0 };
                if best.as_ref().is_none_or(|x| score < x.0 - svc::EPS) {
                    best = Some((score, j, s, b));
                }
            }
            s += 0.05;
        }
    }
    let (_, j, s, b) = best?;
    let sd = &room.sides[j];
    taken.push(b);
    let at = sd.at(s + W / 2.0, W / 2.0);
    Some(GFixture { scale: 1.0, min_clear: 0.0, family: fam, ty: "600", at, rotation: sd.back_rot(), wall: None, z: 0.0 })
}

fn fixtures_in(p: &GProject, k: usize, room: &Room, fam: &str) -> Vec<R> {
    p.storeys[k]
        .layout
        .fixtures
        .iter()
        .filter(|f| f.family == fam && svc::inside(f.at, &svc::grow(&room.clear, 0.02)))
        .filter_map(svc::footprint)
        .collect()
}

/// Gives every dwelling without a washing machine one, in a bath room, else the kitchen, and
/// records how many appliances the project has. Returns how many were added here.
pub(crate) fn appliances(spec: &ProjectSpec, p: &mut GProject) -> usize {
    let mut added = 0;
    if matches!(spec.typology, Typology::House | Typology::ApartmentSection) {
        let rooms: Vec<Vec<Room>> = p.storeys.iter().map(svc::rooms).collect();
        // dwellings that have one anywhere in the project, on any storey
        let mut washer: Vec<PRole> = vec![];
        for (k, rs) in rooms.iter().enumerate() {
            for room in rs {
                if !washer.contains(&room.owner) && !fixtures_in(p, k, room, "Washing Machine").is_empty() {
                    washer.push(room.owner);
                }
            }
        }
        for kind in [RoomKind::Bath, RoomKind::Kitchen] {
            for (k, rs) in rooms.iter().enumerate() {
                let ours = |r: &&Room| super::furnish::dwelling_of(r.owner, spec.typology).is_some();
                for room in rs.iter().filter(|r| r.kind == kind).filter(ours) {
                    if washer.contains(&room.owner) {
                        continue;
                    }
                    let mut taken = blocked(p, k, room);
                    let near = fixtures_in(p, k, room, if kind == RoomKind::Bath { "Washbasin" } else { "Kitchen Counter" });
                    if let Some(f) = put(room, "Washing Machine", &near, &mut taken) {
                        p.storeys[k].layout.fixtures.push(f);
                        washer.push(room.owner);
                        added += 1;
                    }
                }
            }
        }
    }
    let all = p.storeys.iter().flat_map(|s| &s.layout.fixtures).filter(|f| APPLIANCES.contains(&f.family)).count();
    p.metrics.push(("appliances", all as f64));
    added
}
