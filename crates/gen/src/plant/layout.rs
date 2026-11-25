// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rule-based layout: units in two rows facing a central aisle, each group together; spacing
//! from the maintenance clearances and the room the pipe stubs need; a walkway clear of stairs
//! and stubs; platforms with handrails and a ladder or stair where a valve is out of reach; the
//! terminal points on the east wall; the rack over the walkway; the hall around it all.

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::route3d::seg_seg_dist;
use serde_json::{json, Value as J};

use crate::project::rules::rule;

use super::equip;
use super::route::{self, Piping, SupportKind};
use super::size;
use super::topo::Topo;
use super::{box_at, p3, EqKind, Equip, Row, Spot};

/// Thickness of a platform deck and of the outer walls, m.
pub(super) const DECK_T: f64 = 0.05;
/// Room between a platform post and a pipe stub, m.
const POST_GAP: f64 = 0.05;
pub(super) const WALL: f64 = 0.3;
/// Depth of a ladder off the platform edge without and with a cage.
const LADDER_D: f64 = 0.2;
/// Half section of a steel member for clash checks, m.
pub const STEEL_R: f64 = 0.03;
/// A terminal row keeps this share of a pitch to its band ends.
const HALF: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct Hall {
    /// Inner faces of the walls.
    pub x0: f64,
    pub x1: f64,
    pub y0: f64,
    pub y1: f64,
    /// Clear height to the roof structure.
    pub z: f64,
    /// The walkway band along x.
    pub walk: (f64, f64),
    /// Rack tiers in use and the first tier.
    pub tiers: Vec<f64>,
    pub rack_z: f64,
    /// Axis level of the drain layer below the floor before its fall.
    pub drain_z: f64,
}

impl Hall {
    pub fn to_json(&self) -> J {
        json!({"x": [size::r2(self.x0), size::r2(self.x1)], "y": [size::r2(self.y0), size::r2(self.y1)],
               "height": size::r2(self.z), "walkway": [size::r2(self.walk.0), size::r2(self.walk.1)],
               "tiers": self.tiers.iter().map(|z| size::r2(*z)).collect::<Vec<_>>(),
               "drain_z": size::r2(self.drain_z), "wall": WALL})
    }
    pub fn length(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn width(&self) -> f64 {
        self.y1 - self.y0
    }
}

/// The wall round liquid storage: its rectangle on the wall axis, its height, the volume it holds
/// and the tanks inside.
#[derive(Clone, Debug, PartialEq)]
pub struct Bund {
    pub lo: V3,
    pub hi: V3,
    pub h: f64,
    pub volume: f64,
    pub tanks: Vec<String>,
    /// Volume of the largest tank, m3.
    pub big: f64,
}

impl Bund {
    pub fn to_json(&self) -> J {
        json!({"lo": p3(self.lo), "hi": p3(self.hi), "height": size::r2(self.h), "volume_m3": size::r2(self.volume),
               "tanks": self.tanks, "largest_m3": size::r2(self.big)})
    }
}

/// The units a bund holds: the tanks of a process line.
pub(super) fn bunded(t: &Topo) -> Vec<usize> {
    if !t.is_process() {
        return vec![];
    }
    (0..t.eqs.len()).filter(|&i| t.eqs[i].kind == EqKind::Tank).collect()
}

fn bund_of(t: &Topo) -> Option<Bund> {
    let ids = bunded(t);
    if ids.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (v3(f64::INFINITY, f64::INFINITY, 0.0), v3(f64::NEG_INFINITY, f64::NEG_INFINITY, 0.0));
    let (mut inside, mut big) = (0.0, 0.0_f64);
    for &i in &ids {
        let e = &t.eqs[i];
        for (a, b) in e.body() {
            lo = v3(lo.x.min(a.x), lo.y.min(a.y), 0.0);
            hi = v3(hi.x.max(b.x), hi.y.max(b.y), 0.0);
        }
        inside += std::f64::consts::PI * e.dims.d * e.dims.d / 4.0;
        big = big.max(e.duty.iter().find(|d| d.0 == "volume_m3").map_or(0.0, |d| d.1));
    }
    let h = rule("plant.bund.h");
    let need = rule("plant.bund.volume") * big;
    let m = size::bund_margin(hi.x - lo.x, hi.y - lo.y, inside, h, need, rule("plant.bund.margin"));
    let (lo, hi) = (v3(lo.x - m, lo.y - m, 0.0), v3(hi.x + m, hi.y + m, 0.0));
    let volume = ((hi.x - lo.x) * (hi.y - lo.y) - inside) * h;
    Some(Bund { lo, hi, h, volume, tanks: ids.iter().map(|&i| t.eqs[i].tag.clone()).collect(), big })
}

/// A space kept free: a unit's maintenance clearance or the walkway.
#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub tag: String,
    /// `front`, `back`, `left`, `right`, `tube_pull`, `walkway`.
    pub kind: &'static str,
    pub lo: V3,
    pub hi: V3,
}

/// A ladder or a stair from the floor to a platform.
#[derive(Clone, Debug, PartialEq)]
pub struct Access {
    /// `ladder` or `stair`.
    pub kind: &'static str,
    pub foot: V3,
    pub top: V3,
    pub width: f64,
    pub caged: bool,
    /// Rungs or treads.
    pub steps: usize,
    /// Envelope.
    pub lo: V3,
    pub hi: V3,
}

/// An operating platform beside a unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Platform {
    pub tag: String,
    /// The unit it serves.
    pub serves: String,
    /// Deck plan and level.
    pub lo: V3,
    pub hi: V3,
    pub deck: f64,
    pub access: Access,
    /// The second way up at the opposite end, of the other kind (a stair and a ladder).
    pub extra: Vec<Access>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SteelKind {
    Post,
    Beam,
    Rail,
    KneeRail,
    Toe,
    Stile,
    Rung,
    Hoop,
    Stringer,
    Tread,
    Rod,
}

impl SteelKind {
    pub fn name(self) -> &'static str {
        match self {
            SteelKind::Post => "post",
            SteelKind::Beam => "beam",
            SteelKind::Rail => "handrail",
            SteelKind::KneeRail => "knee rail",
            SteelKind::Toe => "toe board",
            SteelKind::Stile => "ladder stile",
            SteelKind::Rung => "rung",
            SteelKind::Hoop => "cage hoop",
            SteelKind::Stringer => "stair stringer",
            SteelKind::Tread => "tread",
            SteelKind::Rod => "hanger rod",
        }
    }
}

/// A steel member as its axis.
#[derive(Clone, Debug, PartialEq)]
pub struct Steel {
    pub kind: SteelKind,
    pub a: V3,
    pub b: V3,
    /// What it belongs to: a platform tag, `rack`, or a line number.
    pub of: String,
    /// The line a hanger or stand carries: its own pipe touches it.
    pub line: Option<usize>,
}

impl Steel {
    pub fn len(&self) -> f64 {
        (self.b - self.a).len()
    }
}

/// A flue from a boiler's spigot through the roof.
#[derive(Clone, Debug, PartialEq)]
pub struct Flue {
    pub tag: String,
    pub d: f64,
    pub at: V3,
    pub top: f64,
}

/// The frame of an outdoor rack, fixed before routing: portals of two posts across the road at
/// every bay, a beam under each tier.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Rack {
    pub xs: Vec<f64>,
    pub y0: f64,
    pub y1: f64,
    /// A beam's axis below its tier: the largest pipe, a shoe of half the gap, the steel.
    pub drop: f64,
}

impl Rack {
    pub fn beams(&self, tiers: &[f64]) -> Vec<f64> {
        tiers.iter().map(|z| z - self.drop).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Lay {
    pub hall: Hall,
    pub zones: Vec<Zone>,
    pub platforms: Vec<Platform>,
    pub steel: Vec<Steel>,
    pub flues: Vec<Flue>,
    /// Containment wall round the process tanks.
    pub bund: Option<Bund>,
    /// Tier pitch and the bay of the rack.
    pub tier: f64,
    /// Aisle gap and the spacing of the first pair of units, for the explanation.
    pub aisle_gap: f64,
    pub spacing: Option<(String, String, f64)>,
    /// Outdoors: the rack on its own portals.
    pub rack: Option<Rack>,
}

impl Lay {
    pub fn tiers(&self, n: usize) -> Vec<f64> {
        (0..n).map(|k| self.hall.rack_z + k as f64 * self.tier).collect()
    }
}

/// Sides of a unit, local: front (-y), back (+y), left (-x), right (+x).
const FRONT: usize = 0;
const BACK: usize = 1;
const LEFT: usize = 2;
const RIGHT: usize = 3;

fn side_of(d: V3) -> Option<usize> {
    if d.y < -0.5 {
        Some(FRONT)
    } else if d.y > 0.5 {
        Some(BACK)
    } else if d.x < -0.5 {
        Some(LEFT)
    } else if d.x > 0.5 {
        Some(RIGHT)
    } else {
        None
    }
}

/// A platform in the unit's own coordinates before placing.
struct LocalPlatform {
    lo: V3,
    hi: V3,
    deck: f64,
    /// Outward normal of the side it stands on and the direction its access leaves along.
    n: V3,
    u: V3,
    stair: bool,
    /// The stair is too long for the room: a steep ship ladder stands in its place.
    ship: bool,
    caged: bool,
    run: f64,
    /// The second exit at the opposite end: stair, caged, run.
    second: Option<(bool, bool, f64)>,
}

/// What a unit needs on each side: maintenance zone, and the hard room of stubs, platforms and
/// access.
struct Need {
    zone: [f64; 4],
    hard: [f64; 4],
    plat: Option<LocalPlatform>,
}

/// Pitch of a ship ladder, degrees, and whether it was raised: it must stand steeper than a stair.
pub(super) fn ship_angle() -> (f64, bool) {
    let (stair, ship) = (rule("plant.stair.angle"), rule("plant.stair.ship_angle"));
    if ship > stair { (ship, false) } else { (ship.max(stair + 10.0), true) }
}

fn need(e: &Equip, t: &Topo) -> Need {
    let (lo, hi) = e.extent();
    let mut hard = [0.0; 4];
    for n in &e.nozzles {
        let Some(s) = side_of(n.dir) else { continue };
        let g = route::geo(&t.lines[n.line], &t.eqs);
        let edge = match s {
            FRONT => -lo.y,
            BACK => hi.y,
            LEFT => -lo.x,
            _ => hi.x,
        };
        let face = n.at.dot(n.dir);
        hard[s] = f64::max(hard[s], (face - edge).max(0.0) + g.reach() + g.min_leg + g.clear);
    }
    let reach = rule("plant.access.reach");
    let ops: Vec<V3> = equip::ops(e, &t.lines).into_iter().filter(|o| o.z > reach + 1e-9).collect();
    let mut plat = None;
    if !ops.is_empty() {
        let top = ops.iter().map(|o| o.z).fold(0.0, f64::max);
        let mut deck = top - rule("plant.access.op_h");
        let (gap, w) = (rule("plant.platform.gap"), rule("plant.platform.width"));
        let front = e.kind == EqKind::Exchanger;
        // a side stub under the platform's rails lifts the deck over it
        let rail = rule("plant.platform.handrail");
        let side = if front { FRONT } else { RIGHT };
        for _ in 0..e.nozzles.len() {
            let lift = e
                .nozzles
                .iter()
                .filter(|n| side_of(n.dir) == Some(side))
                .map(|n| (n.at.z, route::geo(&t.lines[n.line], &t.eqs).clear))
                .filter(|&(z, c)| z + c + 2.0 * STEEL_R + POST_GAP > deck - DECK_T && z - c - 2.0 * STEEL_R < deck + rail)
                .map(|(z, c)| z + c + DECK_T + 2.0 * STEEL_R + POST_GAP)
                .fold(deck, f64::max);
            if lift <= deck {
                break;
            }
            deck = lift;
        }
        let (plo, phi, n, u) = if front {
            (v3(lo.x, lo.y - gap - w, deck - DECK_T), v3(hi.x, lo.y - gap, deck), v3(0.0, -1.0, 0.0), v3(1.0, 0.0, 0.0))
        } else {
            (v3(hi.x + gap, lo.y, deck - DECK_T), v3(hi.x + gap + w, hi.y, deck), v3(1.0, 0.0, 0.0), v3(0.0, -1.0, 0.0))
        };
        let stair = deck >= rule("plant.access.stair_from");
        let ship = stair && deck / rule("plant.stair.angle").to_radians().tan() > rule("plant.stair.max_run");
        let caged = !stair && deck > rule("plant.ladder.cage_from");
        let run = if ship {
            deck / ship_angle().0.to_radians().tan()
        } else if stair {
            deck / rule("plant.stair.angle").to_radians().tan()
        } else {
            LADDER_D + if caged { rule("plant.ladder.hoop") } else { 0.0 }
        };
        let s_n = side_of(n).unwrap_or(RIGHT);
        let s_u = side_of(u).unwrap_or(FRONT);
        hard[s_n] = hard[s_n].max(gap + w + gap);
        hard[s_u] = hard[s_u].max(run + gap);
        // two means of access above the reach height: the other kind at the opposite end
        let second = (rule("plant.platform_exits") >= 2.0 && deck > reach).then(|| {
            // a ship ladder is no second kind of way up: the other end gets the caged ladder
            let st = !stair && deck / rule("plant.stair.angle").to_radians().tan() <= rule("plant.stair.max_run");
            let cg = !st && deck > rule("plant.ladder.cage_from");
            let r = if st {
                deck / rule("plant.stair.angle").to_radians().tan()
            } else {
                LADDER_D + if cg { rule("plant.ladder.hoop") } else { 0.0 }
            };
            let s_o = side_of(v3(-u.x, -u.y, -u.z)).unwrap_or(BACK);
            hard[s_o] = hard[s_o].max(r + gap);
            (st, cg, r)
        });
        plat = Some(LocalPlatform { lo: plo, hi: phi, deck, n, u, stair, ship, caged, run, second });
    }
    let c = e.clear;
    Need { zone: [c.front, c.back, c.left, c.right], hard, plat }
}

/// World x extent and west/east sides of a unit turned `rot` (0 or 2).
fn x_sides(e: &Equip, rot: u8) -> (f64, f64, usize, usize) {
    let (lo, hi) = e.extent();
    if rot == 0 {
        (lo.x, hi.x, LEFT, RIGHT)
    } else {
        (-hi.x, -lo.x, RIGHT, LEFT)
    }
}

/// Groups in blocks that share a row: each pump set beside the unit it draws from, before a
/// group that unit leads and after it otherwise, so the suction stays short.
fn by_source(t: &Topo, groups: Vec<super::topo::Group>) -> Vec<Vec<super::topo::Group>> {
    let source = |g: &super::topo::Group| -> Option<usize> {
        let e = &t.eqs[*g.members.first()?];
        let n = e.nozzles.iter().find(|n| e.kind == EqKind::Pump && n.spot == Spot::Suction)?;
        let u = t.lines[n.line].root.eq;
        (t.eqs[u].kind.in_row() && t.eqs[u].kind != EqKind::Pump).then_some(u)
    };
    let (pumps, rest): (Vec<_>, Vec<_>) = groups.into_iter().partition(|g| source(g).is_some());
    let mut blocks: Vec<Vec<super::topo::Group>> = rest.into_iter().map(|g| vec![g]).collect();
    for g in pumps {
        let u = source(&g).expect("a pump set with a source");
        match blocks.iter().position(|b| b.iter().any(|h| h.members.contains(&u))) {
            Some(k) => {
                let at = blocks[k].iter().position(|h| h.members.contains(&u)).expect("the source group");
                let h = &blocks[k][at];
                let lead = h.members.len() > 1 && h.members[0] == u;
                blocks[k].insert(if lead { at } else { at + 1 }, g);
            }
            None => blocks.push(vec![g]),
        }
    }
    blocks
}

pub(super) fn place(t: &mut Topo) -> Result<Lay, String> {
    let wall_c = rule("plant.clear.wall");
    let aisle = if t.outdoor { rule("plant.rack.width_out") } else { rule("plant.aisle.width") };
    let headroom = rule("plant.access.headroom");
    let needs: Vec<Option<Need>> = t.eqs.iter().map(|e| e.kind.in_row().then(|| need(e, t))).collect();

    // groups into rows: fixed ones first, the rest to the shorter row
    let mut groups = t.groups.clone();
    for (i, e) in t.eqs.iter().enumerate() {
        if e.kind.in_row() && !groups.iter().any(|g| g.members.contains(&i)) {
            groups.push(super::topo::Group { key: "single", members: vec![i], row: None });
        }
    }
    let width = |i: usize| {
        let (lo, hi) = t.eqs[i].extent();
        let n = needs[i].as_ref().expect("a unit in a row has needs");
        hi.x - lo.x + n.zone[LEFT].max(n.hard[LEFT]) + n.zone[RIGHT].max(n.hard[RIGHT])
    };
    let blocks = if t.outdoor { by_source(t, groups) } else { groups.into_iter().map(|g| vec![g]).collect() };
    let mut rows: [Vec<usize>; 2] = [vec![], vec![]];
    let mut len = [0.0, 0.0];
    for pass in [true, false] {
        for b in &blocks {
            let fixed = b.iter().find_map(|g| g.row);
            if fixed.is_some() != pass {
                continue;
            }
            let r = match fixed {
                Some(Row::North) => 0,
                Some(Row::South) => 1,
                _ => usize::from(len[1] < len[0]),
            };
            for &m in b.iter().flat_map(|g| &g.members) {
                rows[r].push(m);
                len[r] += width(m);
            }
        }
    }

    // along x: each pair keeps the larger zone, or the stubs of both
    let mut ends = [0.0f64, 0.0];
    let mut spacing = None;
    for (r, row) in rows.iter().enumerate() {
        let rot = if r == 0 { 0 } else { 2 };
        let mut cursor = 0.0;
        let mut prev: Option<(usize, usize)> = None;
        for &i in row {
            let (wlo, whi, west, east) = x_sides(&t.eqs[i], rot);
            let n = needs[i].as_ref().expect("needs");
            let gap = match prev {
                None => wall_c.max(n.zone[west]).max(n.hard[west]),
                Some((p, pe)) => {
                    let m = needs[p].as_ref().expect("needs");
                    let g = m.zone[pe].max(n.zone[west]).max(m.hard[pe] + n.hard[west]);
                    if spacing.is_none() {
                        spacing = Some((t.eqs[p].tag.clone(), t.eqs[i].tag.clone(), g));
                    }
                    g
                }
            };
            cursor += gap;
            t.eqs[i].rot = rot;
            t.eqs[i].row = if r == 0 { Row::North } else { Row::South };
            t.eqs[i].at.x = cursor - wlo;
            cursor = t.eqs[i].at.x + whi;
            prev = Some((i, east));
        }
        if let Some((p, pe)) = prev {
            let m = needs[p].as_ref().expect("needs");
            cursor += wall_c.max(m.zone[pe]).max(m.hard[pe]);
        }
        ends[r] = cursor;
    }

    // across: fronts aligned, the aisle wide enough for both front zones, the walkway beyond the
    // stairs and stubs
    let front = |r: usize, hard: bool| {
        rows[r]
            .iter()
            .map(|&i| {
                let n = needs[i].as_ref().expect("needs");
                if hard {
                    n.hard[FRONT]
                } else {
                    n.zone[FRONT]
                }
            })
            .fold(0.0, f64::max)
    };
    let (f_n, f_s, s_n, s_s) = (front(0, false), front(1, false), front(0, true), front(1, true));
    let gap = aisle.max(f_n + f_s).max(s_n + aisle + s_s);
    let (y_fs, y_fn) = (0.0, gap);
    let mid = (y_fs + s_s + y_fn - s_n) / 2.0;
    let walk = (mid - aisle / 2.0, mid + aisle / 2.0);
    let mut y1 = walk.1 + wall_c;
    let mut y0 = walk.0 - wall_c;
    for &i in &rows[0] {
        let (lo, hi) = t.eqs[i].extent();
        let n = needs[i].as_ref().expect("needs");
        t.eqs[i].at.y = y_fn - lo.y;
        y1 = y1.max(t.eqs[i].at.y + hi.y + wall_c.max(n.zone[BACK]).max(n.hard[BACK]));
    }
    for &i in &rows[1] {
        let (lo, hi) = t.eqs[i].extent();
        let n = needs[i].as_ref().expect("needs");
        t.eqs[i].at.y = y_fs + lo.y;
        y0 = y0.min(t.eqs[i].at.y - hi.y - wall_c.max(n.zone[BACK]).max(n.hard[BACK]));
    }

    // the units beside the tank farm stand clear of its bund wall
    let keep = WALL + wall_c;
    for r in 0..2 {
        for side in [false, true] {
            let Some(b) = bund_of(t) else { break };
            let is_tank = |i: &usize| b.tanks.contains(&t.eqs[*i].tag);
            let (Some(first), Some(last)) = (rows[r].iter().position(is_tank), rows[r].iter().rposition(is_tank)) else { continue };
            let x_of = |i: usize, hi: bool| {
                let bs = t.eqs[i].body();
                if hi { bs.iter().map(|b| b.1.x).fold(f64::NEG_INFINITY, f64::max) } else { bs.iter().map(|b| b.0.x).fold(f64::INFINITY, f64::min) }
            };
            let (need, from) = if side {
                (rows[r][last + 1..].iter().map(|&i| b.hi.x + keep - x_of(i, false)).fold(0.0, f64::max), last + 1)
            } else {
                (rows[r][..first].iter().map(|&i| x_of(i, true) + keep - b.lo.x).fold(0.0, f64::max), first)
            };
            if need > 0.0 {
                for &i in &rows[r][from..] {
                    t.eqs[i].at.x += need;
                }
                ends[r] += need;
            }
        }
    }

    // and its wall keeps off the walkway: the row of the tanks steps back
    if let Some(b) = bund_of(t) {
        let north = rows[0].iter().any(|i| b.tanks.contains(&t.eqs[*i].tag));
        let d = if north { walk.1 + keep - b.lo.y } else { b.hi.y + keep - walk.0 };
        if d > 0.0 {
            let r = if north { 0 } else { 1 };
            for &i in &rows[r] {
                t.eqs[i].at.y += if north { d } else { -d };
            }
            if north {
                y1 += d;
            } else {
                y0 -= d;
            }
        }
    }

    // terminals on the east wall, beside the walkway, stacked up if the wall is short
    let terms: Vec<usize> = (0..t.eqs.len()).filter(|&i| t.eqs[i].kind == EqKind::Terminal).collect();
    let tgeo: Vec<route::Geo> = terms.iter().map(|&i| route::geo(&t.lines[t.eqs[i].nozzles[0].line], &t.eqs)).collect();
    let strip = terms
        .iter()
        .zip(&tgeo)
        .map(|(&i, g)| -t.eqs[i].extent().0.y + g.reach() + g.clear)
        .fold(0.0, f64::max);
    let module = rule("plant.hall.module");
    let x1 = size::round_up(ends[0].max(ends[1]) + strip, module);
    let pitch = tgeo.iter().map(|g| 2.0 * g.clear).fold(0.0, f64::max);
    let width = size::round_up(y1 - y0, module);
    y1 = y0 + width;
    let z_t = rule("plant.terminal.z");
    let bands = [(y0 + pitch * HALF, walk.0 - pitch * HALF), (walk.1 + pitch * HALF, y1 - pitch * HALF)];
    let mut slots = vec![];
    let mut level = 0;
    while slots.len() < terms.len() {
        let before = slots.len();
        for b in bands {
            let mut y = b.0;
            while y <= b.1 + 1e-9 && slots.len() < terms.len() {
                slots.push(v3(x1, y, z_t + level as f64 * pitch));
                y += pitch;
            }
        }
        if slots.len() == before && level > terms.len() {
            return Err("the east wall has no room for the terminal points".into());
        }
        level += 1;
    }
    for (k, &i) in terms.iter().enumerate() {
        t.eqs[i].at = slots[k];
        t.eqs[i].rot = 3;
    }
    if let Some(i) = t.eqs.iter().position(|e| e.kind == EqKind::Sump) {
        let (_, hi) = t.eqs[i].extent();
        t.eqs[i].at = v3(x1 - hi.x, (walk.0 + walk.1) / 2.0, 0.0);
    }

    // rack above everything that stands in the hall
    let c = t.lines.iter().map(|l| route::geo(l, &t.eqs).clear).fold(0.0, f64::max);
    let mut tops = headroom;
    for e in t.eqs.iter().filter(|e| e.kind.in_row()) {
        tops = tops.max(e.at.z + e.extent().1.z);
        for n in &e.nozzles {
            if n.dir.z > 0.5 {
                let g = route::geo(&t.lines[n.line], &t.eqs);
                tops = tops.max(e.at.z + n.at.z + g.reach());
            }
        }
    }

    // platforms, access and their steel, in world coordinates
    let mut platforms = vec![];
    let mut steel = vec![];
    for (i, n) in needs.iter().enumerate() {
        let Some(p) = n.as_ref().and_then(|n| n.plat.as_ref()) else { continue };
        let e = &t.eqs[i];
        let tag = format!("PL-{}", platforms.len() + 1);
        let pf = platform(e, t, p, &tag, &mut steel);
        tops = tops.max(pf.access.hi.z).max(pf.deck + rule("plant.platform.handrail"));
        platforms.push(pf);
    }
    // outdoors the rack stands over its road beside the units; indoors it clears everything
    let rack_z = if t.outdoor {
        t.rack_m.unwrap_or(rule("plant.rack.z_out")).max(headroom + c + rule("plant.rack.clear"))
    } else {
        tops + c + rule("plant.rack.clear")
    };
    let drain = t.lines.iter().find(|l| l.svc == super::Svc::Dr);
    let drain_z = -(rule("plant.drain.cover") + drain.map_or(0.0, route::env_of));

    let mut zones = vec![];
    for (i, n) in needs.iter().enumerate() {
        let Some(n) = n else { continue };
        let e = &t.eqs[i];
        let (lo, hi) = e.extent();
        let z1 = headroom;
        let tube = e.kind == EqKind::Exchanger;
        let boxes = [
            ("front", v3(lo.x, lo.y - n.zone[FRONT], 0.0), v3(hi.x, lo.y, z1)),
            ("back", v3(lo.x, hi.y, 0.0), v3(hi.x, hi.y + n.zone[BACK], z1)),
            (if tube { "tube_pull" } else { "left" }, v3(lo.x - n.zone[LEFT], lo.y, 0.0), v3(lo.x, hi.y, z1)),
            ("right", v3(hi.x, lo.y, 0.0), v3(hi.x + n.zone[RIGHT], hi.y, z1)),
        ];
        for (kind, a, b) in boxes {
            if (b - a).x.min((b - a).y) > 1e-9 {
                let (wl, wh) = box_at(a, b, e.rot, e.at);
                zones.push(Zone { tag: e.tag.clone(), kind, lo: wl, hi: wh });
            }
        }
    }
    zones.push(Zone { tag: String::new(), kind: "walkway", lo: v3(0.0, walk.0, 0.0), hi: v3(x1, walk.1, headroom) });

    let flues = t
        .eqs
        .iter()
        .filter_map(|e| e.flue.map(|d| Flue { tag: e.tag.clone(), d, at: e.world(equip::flue_spigot(e)), top: 0.0 }))
        .collect();
    Ok(Lay {
        hall: Hall { x0: 0.0, x1, y0, y1, z: 0.0, walk, tiers: vec![], rack_z, drain_z },
        zones,
        platforms,
        steel,
        flues,
        bund: None,
        tier: rule(if t.outdoor { "plant.rack.tier_out" } else { "plant.rack.tier" }),
        aisle_gap: gap,
        spacing,
        rack: t.outdoor.then(|| {
            let bay = rule("plant.rack.bay");
            let n = ((x1 / bay).ceil() as usize).max(1);
            let pitch = x1 / n as f64;
            let inset = wall_c.min((walk.1 - walk.0) / 4.0);
            Rack { xs: (0..n).map(|k| pitch * (k as f64 + 0.5)).collect(), y0: walk.0 + inset, y1: walk.1 - inset, drop: c + STEEL_R }
        }),
    })
}

/// The platform of a unit with posts, deck beams, handrails, knee rails and toe boards on its
/// open sides, and its ladder (with a cage when tall) or stair at the access end.
fn platform(e: &Equip, t: &Topo, p: &LocalPlatform, tag: &str, steel: &mut Vec<Steel>) -> Platform {
    let deck = p.deck;
    let (rail, knee, toe) = (rule("plant.platform.handrail"), rule("plant.platform.knee"), rule("plant.platform.toe"));
    let post = rule("plant.platform.post");
    let mut put = |kind: SteelKind, a: V3, b: V3| {
        steel.push(Steel { kind, a: e.world(a), b: e.world(b), of: tag.to_string(), line: None });
    };
    let z = |v: V3, z: f64| v3(v.x, v.y, z);
    let stubs: Vec<(V3, V3, f64)> = e
        .nozzles
        .iter()
        .filter(|n| n.dir.z.abs() < 0.5 && n.at.z < deck)
        .map(|n| {
            let g = route::geo(&t.lines[n.line], &t.eqs);
            (n.at, n.at + n.dir * (g.reach() + g.min_leg + g.clear), g.clear)
        })
        .collect();
    // deck corners: the access end is the one the access direction points at
    let (lo, hi) = (p.lo, p.hi);
    let corners = [v3(lo.x, lo.y, deck), v3(hi.x, lo.y, deck), v3(hi.x, hi.y, deck), v3(lo.x, hi.y, deck)];
    for k in 0..4 {
        put(SteelKind::Beam, corners[k], corners[(k + 1) % 4]);
    }
    // the open edges: all but the one against the unit and the access end
    let mid = |a: V3, b: V3| (a + b) * 0.5;
    let centre = mid(corners[0], corners[2]);
    for k in 0..4 {
        let (a, b) = (corners[k], corners[(k + 1) % 4]);
        let out = mid(a, b) - centre;
        let toward_unit = out.dot(p.n) < -1e-9;
        let access_end = out.dot(p.u) > 1e-9 || (p.second.is_some() && out.dot(p.u) < -1e-9);
        if toward_unit || access_end {
            continue;
        }
        put(SteelKind::Rail, z(a, deck + rail), z(b, deck + rail));
        put(SteelKind::KneeRail, z(a, deck + knee), z(b, deck + knee));
        put(SteelKind::Toe, z(a, deck + toe), z(b, deck + toe));
        let l = (b - a).len();
        let m = ((l / post).ceil() as usize).max(1);
        let along = (b - a) * (1.0 / l.max(1e-9));
        for j in 0..=m {
            let q0 = a + (b - a) * (j as f64 / m as f64);
            // a post steps along its edge, staying on it, off the stubs that pass under the deck
            let clear = |q: V3| stubs.iter().all(|(s0, s1, c)| seg_seg_dist(z(q, 0.0), z(q, deck), *s0, *s1) >= 2.0 * c + STEEL_R + POST_GAP);
            let q = (0..=(2.0 * l / POST_GAP) as usize)
                .map(|k| k.div_ceil(2) as f64 * POST_GAP * if k % 2 == 0 { 1.0 } else { -1.0 })
                .map(|d| q0 + along * d)
                .find(|&q| (q - a).dot(along) >= -1e-9 && (q - a).dot(along) <= l + 1e-9 && clear(q))
                .unwrap_or(q0);
            put(SteelKind::Post, z(q, 0.0), z(q, deck + rail));
        }
    }
    // access at the end the access direction points at, and the second at the opposite end
    let mut exits: Vec<Access> = vec![];
    let mut want = vec![(p.u, p.stair, p.caged, p.run, p.ship)];
    if let Some((st, cg, r)) = p.second {
        want.push((v3(-p.u.x, -p.u.y, -p.u.z), st, cg, r, false));
    }
    for (dir, is_stair, caged, run, ship) in want {
        let end_pts: Vec<V3> = (0..4)
            .map(|k| (corners[k], corners[(k + 1) % 4]))
            .find(|(a, b)| (mid(*a, *b) - centre).dot(dir) > 1e-9)
            .map(|(a, b)| vec![a, b])
            .unwrap_or_default();
        let (ea, eb) = (end_pts[0], end_pts[1]);
        let em = mid(ea, eb);
        let width = (eb - ea).len();
        let (kind, steps, foot, a_lo, a_hi) = if is_stair {
            let w = rule("plant.stair.width").min(width);
            let rise = rule("plant.stair.rise");
            let n = ((deck / rise).ceil() as usize).max(2);
            let foot = z(em + dir * run, 0.0);
            let side = (eb - ea).normalized() * (w / 2.0);
            for s in [-1.0, 1.0] {
                let o = side * s;
                put(SteelKind::Stringer, foot + o, z(em + o, deck));
                put(SteelKind::Rail, z(foot + o, rail), z(em + o, deck + rail));
            }
            for k in 1..n {
                let f = k as f64 / n as f64;
                let c = z(em + dir * (run * (1.0 - f)), deck * f);
                put(SteelKind::Tread, c - side, c + side);
            }
            let (l1, h1) = env_box(&[ea, eb, ea + dir * run, eb + dir * run], 0.0, deck + rail);
            (if ship { "ship_ladder" } else { "stair" }, n - 1, foot, l1, h1)
        } else {
            let w = rule("plant.ladder.width").min(width);
            let side = (eb - ea).normalized() * (w / 2.0);
            let at = em + dir * LADDER_D;
            for s in [-1.0, 1.0] {
                put(SteelKind::Stile, z(at + side * s, 0.0), z(at + side * s, deck + rail));
            }
            let rung = rule("plant.ladder.rung");
            let n = ((deck / rung).floor() as usize).max(1);
            for k in 1..=n {
                let h = k as f64 * rung;
                put(SteelKind::Rung, z(at - side, h), z(at + side, h));
            }
            if caged {
                let hoop = rule("plant.ladder.hoop");
                let step = rule("plant.ladder.hoop_step");
                let mut h = rule("plant.access.headroom");
                while h <= deck + rail + 1e-9 {
                    let back = at + dir * hoop;
                    put(SteelKind::Hoop, z(at - side, h), z(back - side, h));
                    put(SteelKind::Hoop, z(back - side, h), z(back + side, h));
                    put(SteelKind::Hoop, z(back + side, h), z(at + side, h));
                    h += step;
                }
            }
            let (l1, h1) = env_box(&[ea, eb, ea + dir * run, eb + dir * run], 0.0, deck + rail);
            ("ladder", n, z(at, 0.0), l1, h1)
        };
        let (wlo, whi) = box_at(a_lo, a_hi, e.rot, e.at);
        exits.push(Access { kind, foot: e.world(foot), top: e.world(z(em, deck)), width, caged, steps, lo: wlo, hi: whi });
    }
    let (plo, phi) = box_at(p.lo, v3(p.hi.x, p.hi.y, deck + rail), e.rot, e.at);
    let access = exits.remove(0);
    Platform { tag: tag.to_string(), serves: e.tag.clone(), lo: plo, hi: phi, deck, access, extra: exits }
}

fn env_box(pts: &[V3], z0: f64, z1: f64) -> (V3, V3) {
    let mut lo = v3(f64::INFINITY, f64::INFINITY, z0);
    let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, z1);
    for p in pts {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), z0);
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), z1);
    }
    (lo, hi)
}

/// After routing: the hall height over the highest pipe, the flues through the roof, and the
/// steel of the rack trapezes, hangers and stands.
pub(super) fn finish(lay: &mut Lay, t: &Topo, piping: &Piping) {
    let mut top = lay.hall.rack_z;
    for s in &piping.segs {
        top = top.max(s.a.z.max(s.b.z) + s.env);
    }
    for e in t.eqs.iter().filter(|e| e.kind.in_row()) {
        top = top.max(e.at.z + e.extent().1.z);
    }
    for p in &lay.platforms {
        top = top.max(p.hi.z).max(p.access.hi.z);
        for x in &p.extra {
            top = top.max(x.hi.z);
        }
    }
    let z = size::round_up(top + rule("plant.hall.roof_clear"), rule("plant.route.step"));
    lay.hall.z = z;
    lay.hall.tiers = piping.tiers.clone();
    lay.bund = bund_of(t);
    for f in &mut lay.flues {
        f.top = z + WALL + rule("plant.flue.above_roof");
    }
    for tz in &piping.trapezes {
        for &zb in &tz.beams {
            lay.steel.push(Steel { kind: SteelKind::Beam, a: v3(tz.x, tz.y0, zb), b: v3(tz.x, tz.y1, zb), of: "rack".into(), line: None });
        }
        let low = tz.beams.iter().copied().fold(f64::INFINITY, f64::min);
        let high = tz.beams.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        for y in [tz.y0, tz.y1] {
            // outdoors the trapeze stands on two posts; indoors it hangs from the roof
            if t.outdoor {
                lay.steel.push(Steel { kind: SteelKind::Post, a: v3(tz.x, y, 0.0), b: v3(tz.x, y, high), of: "rack".into(), line: None });
            } else {
                lay.steel.push(Steel { kind: SteelKind::Rod, a: v3(tz.x, y, low), b: v3(tz.x, y, z), of: "rack".into(), line: None });
            }
        }
    }
    // portals tied along the rack at the top beam level
    if t.outdoor {
        for w in piping.trapezes.windows(2) {
            let top = w[0].beams.iter().chain(&w[1].beams).copied().fold(f64::NEG_INFINITY, f64::max);
            for (ya, yb) in [(w[0].y0, w[1].y0), (w[0].y1, w[1].y1)] {
                lay.steel.push(Steel { kind: SteelKind::Beam, a: v3(w[0].x, ya, top), b: v3(w[1].x, yb, top), of: "rack".into(), line: None });
            }
        }
    }
    for s in &piping.supports {
        let num = t.lines[s.line].number();
        match s.kind {
            SupportKind::Hanger => lay.steel.push(Steel {
                kind: SteelKind::Rod,
                a: s.at + v3(0.0, 0.0, s.env),
                b: v3(s.at.x, s.at.y, z),
                of: num,
                line: Some(s.line),
            }),
            SupportKind::Stand => lay.steel.push(Steel {
                kind: SteelKind::Post,
                a: v3(s.at.x, s.at.y, 0.0),
                b: s.at - v3(0.0, 0.0, s.env),
                of: num,
                line: Some(s.line),
            }),
            _ => {}
        }
    }
}

pub(super) fn platform_json(p: &Platform) -> J {
    json!({"tag": p.tag, "serves": p.serves, "lo": p3(p.lo), "hi": p3(p.hi), "deck": size::r2(p.deck),
           "access": {"kind": p.access.kind, "foot": p3(p.access.foot), "top": p3(p.access.top),
                      "width": size::r2(p.access.width), "caged": p.access.caged, "steps": p.access.steps},
           "exits": p.extra.iter().map(|a| json!({"kind": a.kind, "foot": p3(a.foot), "top": p3(a.top), "width": size::r2(a.width), "caged": a.caged, "steps": a.steps})).collect::<Vec<_>>()})
}
