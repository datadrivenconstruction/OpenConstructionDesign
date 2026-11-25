// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A tiling of rectangular rooms becomes walls, doors and windows. Coordinates are whole
//! millimetres, so both sides of a wall agree exactly and walls never overlap.

use super::rules::{rule, rule_mm};
use super::{PRole, PRoom, RoomKind};
use crate::layout::DOOR_H;
use crate::{GLayout, GOpening, GRoom, GWall, GenError, WallRole};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

pub(crate) type Mm = i64;

pub(crate) use ddc_geom::to_mm as mm;

pub(crate) fn m(x: Mm) -> f64 {
    x as f64 / 1000.0
}

/// Clear distance from a door or window to the nearest wall junction.
fn edge_clear() -> Mm {
    rule_mm("facade.edge_clear")
}

/// Twice the clearance, so evenly spaced windows keep it at both ends.
fn win_gap() -> Mm {
    2 * edge_clear()
}

/// Widest window a short facade may take before the room counts as dim.
fn max_win_w() -> f64 {
    rule("facade.win_max_w")
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cell {
    pub x0: Mm,
    pub y0: Mm,
    pub x1: Mm,
    pub y1: Mm,
    pub kind: RoomKind,
    pub name: String,
    pub owner: PRole,
    pub asked: Option<f64>,
    pub grown: bool,
    /// Needs daylight: a window on the facade, else the plan is refused.
    pub lit: bool,
}

impl Cell {
    pub fn area(&self) -> f64 {
        m(self.x1 - self.x0) * m(self.y1 - self.y0)
    }
}

/// Window size of a typology: width, height, sill.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WinSize {
    pub w: f64,
    pub h: f64,
    pub sill: f64,
}

/// Window of the residential typologies.
pub(crate) fn res_win() -> WinSize {
    WinSize {
        w: rule("facade.res_win_w"),
        h: rule("facade.res_win_h"),
        sill: rule("facade.res_sill"),
    }
}

/// One straight wall: along x when `horiz`, at `at`, from `a` to `b` (a < b).
#[derive(Clone, Copy, Debug)]
struct Piece {
    horiz: bool,
    at: Mm,
    a: Mm,
    b: Mm,
}

fn role_of(p: &Cell, q: &Cell) -> WallRole {
    let core = |c: &Cell| {
        matches!(
            c.kind,
            RoomKind::Corridor | RoomKind::Stair | RoomKind::Lift
        )
    };
    if core(p) || core(q) {
        WallRole::Corridor
    } else if p.owner != q.owner {
        WallRole::Party
    } else {
        WallRole::Partition
    }
}

/// Walls on every line of the tiling, cut where the role changes.
fn walls_of(cells: &[Cell]) -> (Vec<Piece>, Vec<WallRole>) {
    let mut pieces = vec![];
    let mut roles = vec![];
    for horiz in [true, false] {
        // line -> breakpoints along it
        let mut lines: BTreeMap<Mm, Vec<Mm>> = BTreeMap::new();
        for c in cells {
            let (l0, l1, s0, s1) = if horiz {
                (c.y0, c.y1, c.x0, c.x1)
            } else {
                (c.x0, c.x1, c.y0, c.y1)
            };
            for l in [l0, l1] {
                let e = lines.entry(l).or_default();
                e.push(s0);
                e.push(s1);
            }
        }
        for (at, mut br) in lines {
            br.sort_unstable();
            br.dedup();
            let mut run: Option<(Mm, Mm, WallRole)> = None;
            for w in br.windows(2) {
                let mid2 = w[0] + w[1];
                let side = |below: bool| {
                    cells.iter().find(|c| {
                        let (l, s0, s1) = match (horiz, below) {
                            (true, true) => (c.y1, c.x0, c.x1),
                            (true, false) => (c.y0, c.x0, c.x1),
                            (false, true) => (c.x1, c.y0, c.y1),
                            (false, false) => (c.x0, c.y0, c.y1),
                        };
                        l == at && 2 * s0 < mid2 && mid2 < 2 * s1
                    })
                };
                let role = match (side(true), side(false)) {
                    (None, None) => None,
                    (Some(p), Some(q)) => Some(role_of(p, q)),
                    _ => Some(WallRole::Exterior),
                };
                // a wall running straight through this point would cross: cut there
                let edge_at = |below: bool| {
                    cells.iter().any(|c| {
                        let l = if below { c.y1 } else { c.y0 };
                        horiz && l == at && (c.x0 == w[0] || c.x1 == w[0])
                    })
                };
                // a stair enclosure is its own wall, so the fire rating stays on the stair
                let stair_end = cells.iter().any(|c| {
                    let (l0, l1, s0, s1) = if horiz { (c.y0, c.y1, c.x0, c.x1) } else { (c.x0, c.x1, c.y0, c.y1) };
                    c.kind == RoomKind::Stair && (l0 == at || l1 == at) && (s0 == w[0] || s1 == w[0])
                });
                let cross = edge_at(true) && edge_at(false) || stair_end;
                run = match (run, role) {
                    (Some((a, b, r)), Some(nr)) if b == w[0] && r == nr && !cross => {
                        Some((a, w[1], r))
                    }
                    (prev, nr) => {
                        if let Some((a, b, r)) = prev {
                            pieces.push(Piece { horiz, at, a, b });
                            roles.push(r);
                        }
                        nr.map(|r| (w[0], w[1], r))
                    }
                };
            }
            if let Some((a, b, r)) = run {
                pieces.push(Piece { horiz, at, a, b });
                roles.push(r);
            }
        }
    }
    (pieces, roles)
}

fn find_piece(pieces: &[Piece], horiz: bool, at: Mm, lo: Mm, hi: Mm) -> Option<usize> {
    pieces
        .iter()
        .position(|p| p.horiz == horiz && p.at == at && p.a <= lo && hi <= p.b)
}

/// The edge two cells share: (horizontal line?, line, from, to).
fn shared(p: &Cell, q: &Cell) -> Option<(bool, Mm, Mm, Mm)> {
    let span = |a0: Mm, a1: Mm, b0: Mm, b1: Mm| (a0.max(b0), a1.min(b1));
    if p.y1 == q.y0 || p.y0 == q.y1 {
        let at = if p.y1 == q.y0 { p.y1 } else { p.y0 };
        let (lo, hi) = span(p.x0, p.x1, q.x0, q.x1);
        return (hi > lo).then_some((true, at, lo, hi));
    }
    if p.x1 == q.x0 || p.x0 == q.x1 {
        let at = if p.x1 == q.x0 { p.x1 } else { p.x0 };
        let (lo, hi) = span(p.y0, p.y1, q.y0, q.y1);
        return (hi > lo).then_some((false, at, lo, hi));
    }
    None
}

fn door_width(p: &Cell, q: &Cell) -> f64 {
    let wet = |c: &Cell| matches!(c.kind, RoomKind::Bath | RoomKind::Wc | RoomKind::Storage);
    let wide = |c: &Cell| {
        matches!(
            c.kind,
            RoomKind::Classroom
                | RoomKind::Lobby
                | RoomKind::Stair
                | RoomKind::Meeting
                | RoomKind::Ward
                | RoomKind::Parking
                | RoomKind::Retail
                | RoomKind::Dining
                | RoomKind::Workshop
                | RoomKind::Gym
        )
    };
    if wet(p) || wet(q) {
        rule("room.door_wet")
    } else if wide(p) || wide(q) || p.owner != q.owner {
        rule("room.door_wide")
    } else {
        rule("room.door")
    }
}

/// Doors may join rooms of one owner, and an apartment hall to the public corridor.
fn may_join(p: &Cell, q: &Cell) -> bool {
    match (p.owner, q.owner) {
        (a, b) if a == b => true,
        (PRole::Public, PRole::Apartment(_)) => {
            p.kind == RoomKind::Corridor && q.kind == RoomKind::Hall
        }
        (PRole::Apartment(_), PRole::Public) => {
            q.kind == RoomKind::Corridor && p.kind == RoomKind::Hall
        }
        _ => false,
    }
}

pub(crate) struct Tiled {
    pub layout: GLayout,
    pub rooms: Vec<PRoom>,
}

/// An edge of the tiling: (horizontal line?, line, from, to).
type Edge = (bool, Mm, Mm, Mm);

/// Puts doors and windows into the wall pieces under them.
struct Opener<'a> {
    pieces: &'a [Piece],
    win: WinSize,
}

impl Opener<'_> {
    fn put(&self, l: &mut GLayout, e: Edge, width: f64, door: bool) -> Result<(), GenError> {
        let (horiz, at, lo, hi) = e;
        let k = find_piece(self.pieces, horiz, at, lo, hi).ok_or_else(|| {
            GenError::Infeasible(format!("no wall under an opening at {}", m(at)))
        })?;
        let center = m(lo + hi) / 2.0 - m(self.pieces[k].a);
        let (height, sill) = if door && width >= gate_w() {
            (gate_h(), 0.0)
        } else if door {
            (DOOR_H, 0.0)
        } else {
            (self.win.h, self.win.sill)
        };
        l.openings.push(GOpening {
            wall: k,
            along: center - width / 2.0,
            door,
            width,
            height,
            sill,
            ty: None,
        });
        Ok(())
    }
}

/// Walls, one door into every room along the shortest way from the corridor, and windows on
/// the facade of every habitable room by the one-eighth daylight rule.
pub(crate) fn tile(
    cells: &[Cell],
    numbers: Vec<String>,
    w: Mm,
    d: Mm,
    win: WinSize,
    entrance: bool,
) -> Result<Tiled, GenError> {
    let (pieces, roles) = walls_of(cells);
    let mut l = GLayout::default();
    for (p, r) in pieces.iter().zip(&roles) {
        let (a, b) = if p.horiz {
            ((m(p.a), m(p.at)), (m(p.b), m(p.at)))
        } else {
            ((m(p.at), m(p.a)), (m(p.at), m(p.b)))
        };
        l.walls.push(GWall::line(a, b, *r));
    }
    let op = Opener {
        pieces: &pieces,
        win,
    };
    doors(cells, &op, &mut l)?;
    if entrance {
        let e = entrance_edge(cells, w, d)
            .ok_or_else(|| GenError::Infeasible("no facade for the entrance".into()))?;
        op.put(&mut l, e, entrance_w(), true)?;
        // vehicle gates: a parking deck or a warehouse hall opens on its back facade
        for c in cells.iter().filter(|c| gated(c) && c.y1 == d) {
            if c.x1 - c.x0 >= mm(gate_w()) + 2 * edge_clear() {
                op.put(&mut l, (true, d, c.x0, c.x1), gate_w(), true)?;
            }
        }
    }
    // daylight short of the rule is measured and named by the gates, on the final geometry;
    // a storey whose walls take no windows (below grade) lays none
    if win.h > 0.0 && win.w > 0.0 {
        for c in cells {
            windows(c, w, d, &op, &mut l)?;
        }
    }
    let mut rooms = Vec::with_capacity(cells.len());
    for (c, number) in cells.iter().zip(numbers) {
        let (xa, ya, xb, yb) = (m(c.x0), m(c.y0), m(c.x1), m(c.y1));
        l.rooms.push(GRoom {
            name: c.name.clone(),
            apartment: match c.owner {
                PRole::Apartment(a) => Some(a),
                PRole::Public => None,
            },
            point: ((xa + xb) / 2.0, (ya + yb) / 2.0),
            polygon: vec![(xa, ya), (xb, ya), (xb, yb), (xa, yb)],
        });
        rooms.push(PRoom {
            kind: c.kind,
            number,
            owner: c.owner,
            asked: c.asked,
            grown: c.grown,
        });
    }
    Ok(Tiled { layout: l, rooms })
}

fn entrance_w() -> f64 {
    rule("egress.entrance_width")
}

fn gate_w() -> f64 {
    rule("facade.gate_w")
}

/// Clear height of a vehicle gate; every typology with a bay has storeys above 3 m.
fn gate_h() -> f64 {
    rule("facade.gate_h")
}

fn gated(c: &Cell) -> bool {
    c.kind == RoomKind::Parking || (c.kind == RoomKind::Storage && c.area() >= rule("room.hall_area"))
}

/// The entrance: into a public lobby on a facade, south first, else a public hall, else a
/// corridor end.
fn entrance_edge(cells: &[Cell], w: Mm, d: Mm) -> Option<Edge> {
    let fits = |lo: Mm, hi: Mm| hi - lo >= mm(entrance_w()) + 2 * edge_clear();
    for kind in [RoomKind::Lobby, RoomKind::Hall] {
        for c in cells.iter().filter(|c| c.kind == kind && c.owner == PRole::Public) {
            let edges = [
                (c.y0 == 0, (true, 0, c.x0, c.x1)),
                (c.y1 == d, (true, d, c.x0, c.x1)),
                (c.x0 == 0, (false, 0, c.y0, c.y1)),
                (c.x1 == w, (false, w, c.y0, c.y1)),
            ];
            if let Some((_, e)) = edges.iter().find(|(on, e)| *on && fits(e.2, e.3)) {
                return Some(*e);
            }
        }
    }
    let corridor = cells.iter().filter(|c| c.kind == RoomKind::Corridor);
    corridor.into_iter().find_map(|c| {
        if c.x0 == 0 {
            Some((false, 0, c.y0, c.y1))
        } else if c.x1 == w {
            Some((false, w, c.y0, c.y1))
        } else {
            None
        }
    })
}

/// A door candidate in the heap: circulation parents first, then the longer shared edge,
/// then the parent reached earlier and the lower room index, as a plain scan would pick.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Cand {
    parent_circulates: bool,
    len: Mm,
    parent_pos: Reverse<usize>,
    room: Reverse<usize>,
    parent: usize,
}

/// One door into every room: a spanning tree grown from the corridors over the rooms that
/// may be joined and share a wall long enough for the door, best edge first.
/// A door on the long side of a narrow wet room goes to one end of the shared wall, so the
/// fixtures fit at the other end.
fn to_end(c: &Cell, e: Edge, width: f64) -> Edge {
    let (along, across) = if e.0 {
        (c.x1 - c.x0, c.y1 - c.y0)
    } else {
        (c.y1 - c.y0, c.x1 - c.x0)
    };
    let need = mm(width) + 2 * edge_clear();
    if !matches!(c.kind, RoomKind::Wc | RoomKind::Bath) || along <= across || e.3 - e.2 <= need {
        return e;
    }
    (e.0, e.1, e.2, e.2 + need)
}

fn doors(cells: &[Cell], op: &Opener<'_>, l: &mut GLayout) -> Result<(), GenError> {
    let n = cells.len();
    let mut adj: Vec<Vec<(usize, Edge)>> = vec![vec![]; n];
    for i in 0..n {
        for j in 0..n {
            let (p, q) = (&cells[i], &cells[j]);
            if i == j || !may_join(p, q) {
                continue;
            }
            if let Some(e) = shared(p, q) {
                if e.3 - e.2 >= mm(door_width(p, q)) + 2 * edge_clear() {
                    adj[i].push((j, e));
                }
            }
        }
    }
    let mut pos: Vec<Option<usize>> = vec![None; n];
    let mut heap = BinaryHeap::new();
    let mut reached = 0;
    let mut reach = |i: usize, pos: &mut Vec<Option<usize>>, heap: &mut BinaryHeap<Cand>| {
        pos[i] = Some(reached);
        reached += 1;
        for &(j, e) in &adj[i] {
            if pos[j].is_none() {
                heap.push(Cand {
                    parent_circulates: cells[i].kind.circulation(),
                    len: e.3 - e.2,
                    parent_pos: Reverse(reached - 1),
                    room: Reverse(j),
                    parent: i,
                });
            }
        }
    };
    for (i, c) in cells.iter().enumerate() {
        if c.kind == RoomKind::Corridor {
            reach(i, &mut pos, &mut heap);
        }
    }
    // a floor without a corridor (a basement under a plate whose ring corridor does not square
    // into the parking grid) is reached from its stairs and lifts
    if pos.iter().all(Option::is_none) {
        for (i, c) in cells.iter().enumerate() {
            if matches!(c.kind, RoomKind::Stair | RoomKind::Lift) {
                reach(i, &mut pos, &mut heap);
            }
        }
    }
    if pos.iter().all(Option::is_none) {
        return Err(GenError::Infeasible("no corridor on the floor".into()));
    }
    while let Some(c) = heap.pop() {
        let (i, j) = (c.parent, c.room.0);
        if pos[j].is_some() {
            continue;
        }
        let e = shared(&cells[i], &cells[j]).ok_or_else(|| {
            GenError::Infeasible(format!("rooms {} and {} lost their wall", i, j))
        })?;
        let w = door_width(&cells[i], &cells[j]);
        op.put(l, to_end(&cells[j], e, w), w, true)?;
        reach(j, &mut pos, &mut heap);
    }
    if let Some(k) = pos.iter().position(Option::is_none) {
        return Err(GenError::Infeasible(format!(
            "room {} ({}) cannot get a door: no shared wall long enough with a reachable room",
            cells[k].name,
            m(cells[k].x1 - cells[k].x0)
        )));
    }
    Ok(())
}

/// Windows on the facade edges of a habitable room, longest edge first, until one eighth of
/// the floor area is glazed or the facade is used up.
fn windows(c: &Cell, w: Mm, d: Mm, op: &Opener<'_>, l: &mut GLayout) -> Result<(), GenError> {
    if !c.lit {
        return Ok(());
    }
    let win = op.win;
    let want = c.area() * rule("facade.glazing_share");
    let step = rule("facade.win_module");
    let mut edges: Vec<Edge> = vec![];
    if c.y0 == 0 {
        edges.push((true, 0, c.x0, c.x1));
    }
    if c.y1 == d {
        edges.push((true, d, c.x0, c.x1));
    }
    if c.x0 == 0 {
        edges.push((false, 0, c.y0, c.y1));
    }
    if c.x1 == w {
        edges.push((false, w, c.y0, c.y1));
    }
    edges.sort_by_key(|e| Reverse(e.3 - e.2));
    let mut glazed = 0.0;
    for (horiz, at, lo, hi) in edges {
        if glazed >= want - 1e-9 {
            break;
        }
        let len = hi - lo;
        let ww = if len < mm(win.w) + 2 * edge_clear() {
            rule("facade.win_min_w")
        } else {
            win.w
        };
        let fit = (len - 2 * edge_clear() + win_gap()) / (mm(ww) + win_gap());
        if fit < 1 {
            continue;
        }
        let need = ((want - glazed) / (ww * win.h)).ceil().max(1.0) as Mm;
        let k = need.min(fit);
        // a short facade takes wider windows, in module steps up to what the wall holds
        let mut ww = ww;
        if k < need {
            let room = m(len - 2 * edge_clear() - (k - 1) * win_gap()) / k as f64;
            let wide = ((want - glazed) / (k as f64 * win.h) / step).ceil() * step;
            ww = wide.min((room / step).floor() * step).min(max_win_w()).max(ww);
        }
        let wmm = mm(ww);
        for t in 0..k {
            let center = lo + (len * (2 * t + 1)) / (2 * k);
            op.put(
                l,
                (horiz, at, center - wmm / 2, center + wmm / 2),
                ww,
                false,
            )?;
            glazed += ww * win.h;
        }
    }
    if glazed == 0.0 {
        return Err(GenError::Infeasible(format!(
            "habitable room {} has no facade for a window",
            c.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A habitable room on a storey without windows (a basement) tiles without any: the
    /// hotel's staff changing room below grade was refused for a facade it cannot have.
    #[test]
    fn a_storey_without_windows_tiles_its_habitable_rooms_without_any() {
        let c = Cell { x0: 0, y0: 0, x1: 8000, y1: 5000, kind: RoomKind::Staff, name: "Staff".into(), owner: PRole::Public, asked: Some(40.0), grown: false, lit: RoomKind::Staff.habitable() };
        let corr = Cell { y0: 5000, y1: 6500, kind: RoomKind::Corridor, name: "Corridor".into(), asked: None, ..c.clone() };
        let t = tile(&[c, corr], vec!["B01".into(), "B02".into()], 8000, 6500, WinSize { w: 0.0, h: 0.0, sill: 0.0 }, false).expect("tiled");
        assert!(t.layout.openings.iter().all(|o| o.door), "no window below grade");
    }
}
