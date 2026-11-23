// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Storeys on any footprint. Rooms line the facade loop in bands `band_depth` deep, cut
//! across the band only where it has its full depth; a corridor fills the plate inside the
//! bands; the core stands in the middle when it fits every floor, else in the band of the
//! widest straight facade; the dark interior beyond the corridor becomes store rooms.
//! Rooms, walls and openings come from one planar arrangement of all zone lines, so
//! neighbouring rooms share exact vertices and every room edge lies on a wall.

use super::arrange::{Arrangement, P};
use super::build::{bank_room, bank_rows, facing_depth, fit_stair, lift_inset, lift_row_full, lift_shaft, stair_def};
use super::footprint::{self, Region};
use super::nav::area;
use super::offset::{self, RingBand};
use super::place::{self, Cap, Fixed, Side};
use super::rules::rule;
use super::tile::{mm, res_win, WinSize};
use super::{names, slots, GProject, PRole, PRoof, PRoom, PStorey, Pin, ProjectSpec, RoofKind, RoomKind, RoomReq, Typology};
use crate::layout::DOOR_H;
use crate::{GLayout, GOpening, GRoom, GShaft, GStair, GWall, GenError, WallRole};
use ddc_geom::{v2, V2};

/// Clearance of a cut from a corner and from another cut of the same loop.
const MARGIN: f64 = 0.3;
/// How far short of its closing depth an inset is probed: at exactly half a plate's width the
/// offset is empty in floating point although the plate holds it, m.
const CLOSE: f64 = 1e-3;
/// Step by which the room bands of a thin plate narrow towards the rule's least depth, m.
const NARROW_STEP: f64 = 0.1;
/// Wall a room keeps on each side of a corner of the band: a window or a door with clearances.
const CORNER_RUN: f64 = 1.6;
/// A turn of the outline sharper than this is a corner, gentler ones are facets of a curve.
const CURVE_TURN: f64 = 0.5;
/// How far a chord of an inner band line may stand off the facets of a curve.
const CHORD_SAG: f64 = 0.15;
/// Narrowest room along the facade at full band depth.
const MIN_ROOM_W: f64 = 2.0;
/// Spare facade shorter than this grows the room before it.
const SPARE_MIN: f64 = 2.4;
const SPARE_MAX: f64 = 6.0;
/// Shared wall a facade room of an apartment needs with its hall for a door.
const HALL_OVERLAP: f64 = 1.25;
const ENTRANCE_W: f64 = 3.0;
/// Interior rooms need this clear width and area, else the space stays corridor.
const STORE_W: f64 = 2.0;
const STORE_AREA: f64 = 10.0;
const WIN_CLEAR: f64 = 0.3;
const WIN_GAP: f64 = 0.6;
const MAX_WIN_W: f64 = 3.0;
const NARROW_CLEAR: f64 = 0.2;
const DOOR_CLEAR: f64 = 0.15;

/// The door of a room of `kind`: its type's width, at least the clear passage the exit and
/// access rules ask plus the leaf the frame takes off.
fn door_width(kind: RoomKind) -> f64 {
    let ty: f64 = match kind {
        RoomKind::Bath | RoomKind::Wc => 0.8,
        RoomKind::Stair | RoomKind::Lift => 1.0,
        _ => 0.9,
    };
    let clear = rule("opening.clear_exit").max(rule("opening.clear_access"));
    ty.max(clear + rule("opening.leaf_loss"))
}

/// Corridor run a room keeps for its door: the widest door of the plan and its jambs.
fn door_run() -> f64 {
    door_width(RoomKind::Stair).max(door_width(RoomKind::Office)) + 2.0 * DOOR_CLEAR
}

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}
fn dot(a: P, b: P) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}
fn to_v(r: &[P]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}
fn to_p(r: &[V2]) -> Vec<P> {
    r.iter().map(|p| (p.x, p.y)).collect()
}
fn rings(r: &Region) -> Vec<Vec<V2>> {
    std::iter::once(&r.outer).chain(&r.holes).map(|x| to_v(x)).collect()
}

/// A local frame: `x` along `u`, `y` along `n`, `n` to the left of `u`.
#[derive(Clone, Copy, Debug)]
struct Frame {
    o: P,
    u: P,
}

impl Frame {
    fn n(&self) -> P {
        (-self.u.1, self.u.0)
    }
    fn at(&self, x: f64, y: f64) -> P {
        let n = self.n();
        (self.o.0 + self.u.0 * x + n.0 * y, self.o.1 + self.u.1 * x + n.1 * y)
    }
    fn local(&self, p: P) -> (f64, f64) {
        let d = sub(p, self.o);
        (dot(d, self.u), dot(d, self.n()))
    }
    fn rect(&self, x0: f64, x1: f64, y0: f64, y1: f64) -> Vec<P> {
        vec![self.at(x0, y0), self.at(x1, y0), self.at(x1, y1), self.at(x0, y1)]
    }
}

/// Stair and lift side by side, their doors on the front (`y = d`).
struct Core {
    f: Frame,
    w: f64,
    d: f64,
    /// `x` ranges of the cells and their kinds.
    cells: Vec<(f64, f64, RoomKind)>,
    stair: GStair,
    hole: Vec<P>,
    /// The second stair of a separated pair, and its slab hole: the core's last cell.
    stair2: Option<(GStair, Vec<P>)>,
    /// The lift shafts of the bank, in one row or two facing rows.
    shafts: Vec<Vec<P>>,
    /// Loop positions on the outer ring when the core stands in the band.
    band: Option<(f64, f64)>,
    /// In the band: the loop positions where each cell starts, then where the last ends.
    seat: Vec<f64>,
    /// In the band: how far the facade bows out behind the core's back, which stands on a chord.
    sag: f64,
    /// The second stair at the plate perimeter when the pair in the core would stand too close.
    end: Option<End>,
}

/// A stair room in the band of the outer ring, at loop positions `t0..t1`.
struct End {
    t0: f64,
    t1: f64,
    /// The door side centre, where the stair meets the corridor.
    door: P,
    stair: GStair,
    hole: Vec<P>,
}

fn core_at(spec: &ProjectSpec, f: Frame, d: f64, pair: bool) -> Result<Core, GenError> {
    let (fit, wmm) = fit_stair(spec, mm(d))?;
    let sw = (wmm as f64 / 1000.0).max(2.0);
    let mut cells = vec![(0.0, sw, RoomKind::Stair)];
    // the bank as the bar plans it: one row, or two facing rows when the core is deep enough
    let (row, places) = bank_rows(spec, mm(d));
    let lw = bank_room(spec, mm(d), 0) as f64 / 1000.0;
    if spec.lift {
        cells.push((sw, sw + lw, RoomKind::Lift));
    }
    let (bw, bd) = (fit.hi.0 - fit.lo.0, fit.hi.1 - fit.lo.1);
    let first_w = cells.last().map_or(sw, |c| c.1);
    let target = ((sw - bw) / 2.0, 0.1);
    let mut stair = stair_def(spec, fit.angle + f.u.1.atan2(f.u.0));
    stair.origin = f.at(target.0 - fit.lo.0, target.1 - fit.lo.1);
    let hole = f.rect(
        (target.0 - 0.05).max(0.0),
        (target.0 + bw + 0.05).min(sw),
        (target.1 - 0.05).max(0.0),
        (target.1 + bd + 0.05).min(d),
    );
    let (sx, sy) = lift_shaft(spec);
    let pitch = sx + rule("core.lift_wall");
    let x0 = sw + (lw - sx - pitch * f64::from(row - 1)) / 2.0;
    let shafts = places
        .iter()
        .map(|&(i, far)| {
            let (x, y) = (x0 + pitch * f64::from(i), if far { d - lift_inset() - sy } else { lift_inset() });
            f.rect(x, x + sx, y, y + sy)
        })
        .collect();
    // a separated pair: a second stair room at the far end of the core, the lifts between
    let stair2 = pair.then(|| {
        cells.push((first_w, first_w + sw, RoomKind::Stair));
        let mut s2 = stair_def(spec, fit.angle + f.u.1.atan2(f.u.0));
        s2.origin = f.at(first_w + target.0 - fit.lo.0, target.1 - fit.lo.1);
        let h2 = f.rect(
            first_w + (target.0 - 0.05).max(0.0),
            first_w + (target.0 + bw + 0.05).min(sw),
            (target.1 - 0.05).max(0.0),
            (target.1 + bd + 0.05).min(d),
        );
        (s2, h2)
    });
    let w = cells.last().map_or(sw, |c| c.1);
    Ok(Core { f, w, d, cells, stair, hole, stair2, shafts, band: None, seat: vec![], sag: 0.0, end: None })
}

/// Whether the core of a plate of `plate_area` m2 takes a separated pair of stairs, by
/// `egress.core_pair`: never, always, or (auto) when a floor's occupants need two exits and
/// the building has `fire.tall_storeys` storeys or more. A plate too deep for the bar plan has no corridor
/// ends to put the second stair at, so the pair stands in the core.
fn core_pair(spec: &ProjectSpec, plate_area: f64) -> bool {
    let mode = rule("egress.core_pair") as usize;
    if mode == 0 || spec.floors < 2 {
        return false;
    }
    let per_person = match spec.typology {
        Typology::Office => rule("egress.occ_office"),
        Typology::School => rule("egress.occ_classroom"),
        Typology::House | Typology::ApartmentSection => rule("egress.occ_residential"),
    };
    // small and low plates keep their one stair; a building of `fire.tall_storeys` storeys or more
    // with two exits' worth of people on a floor takes the pair
    mode == 2 || super::fire::high_rise(spec) || (f64::from(spec.floors) >= rule("fire.tall_storeys") && plate_area / per_person > rule("egress.single_exit_people"))
}

fn in_any(rs: &[Region], p: P) -> bool {
    rs.iter().any(|r| offset::contains(r, p))
}

/// Whether the `w` x `d` rectangle of `f` lies inside one piece of `zs`.
fn fits(f: &Frame, w: f64, d: f64, zs: &[Region]) -> bool {
    let c = f.at(w / 2.0, d / 2.0);
    let Some(z) = zs.iter().find(|z| offset::contains(z, c)) else { return false };
    let steps = |l: f64| ((l / 0.5).ceil() as usize).max(1);
    let (nw, nd) = (steps(w), steps(d));
    for i in 0..=nw {
        let x = w * i as f64 / nw as f64;
        if !offset::contains(z, f.at(x, 0.0)) || !offset::contains(z, f.at(x, d)) {
            return false;
        }
    }
    for j in 0..=nd {
        let y = d * j as f64 / nd as f64;
        if !offset::contains(z, f.at(0.0, y)) || !offset::contains(z, f.at(w, y)) {
            return false;
        }
    }
    std::iter::once(&z.outer).chain(&z.holes).flatten().all(|&q| {
        let (x, y) = f.local(q);
        !(x > 1e-6 && x < w - 1e-6 && y > 1e-6 && y < d - 1e-6)
    })
}

/// The core in the interior of every floor, as near the middle of the ground plate as it
/// goes, turned to one of the long facade directions; the front faces the nearer corridor.
/// A stair alone in the band of `f`, its door on the front (`y = d`): its width and its slab hole.
fn end_stair(spec: &ProjectSpec, f: Frame, d: f64) -> Result<(f64, GStair, Vec<P>), GenError> {
    let (fit, wmm) = fit_stair(spec, mm(d))?;
    let sw = (wmm as f64 / 1000.0).max(2.0);
    let (bw, bd) = (fit.hi.0 - fit.lo.0, fit.hi.1 - fit.lo.1);
    let target = ((sw - bw) / 2.0, 0.1);
    let mut stair = stair_def(spec, fit.angle + f.u.1.atan2(f.u.0));
    stair.origin = f.at(target.0 - fit.lo.0, target.1 - fit.lo.1);
    let hole = f.rect(
        (target.0 - 0.05).max(0.0),
        (target.0 + bw + 0.05).min(sw),
        (target.1 - 0.05).max(0.0),
        (target.1 + bd + 0.05).min(d),
    );
    Ok((sw, stair, hole))
}

fn central(base: &Region, zs: &[Vec<Region>], w: f64, d: f64, target: P) -> Option<Frame> {
    let z0 = zs.first()?;
    if z0.is_empty() || zs.iter().any(Vec::is_empty) {
        return None;
    }
    let n = base.outer.len();
    let mut edges: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let e = sub(base.outer[(i + 1) % n], base.outer[i]);
            (e.0.hypot(e.1), e.1.atan2(e.0))
        })
        .collect();
    edges.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut dirs: Vec<f64> = vec![];
    for a in edges.iter().map(|e| e.1).chain([0.0, std::f64::consts::FRAC_PI_2]) {
        let a = a.rem_euclid(std::f64::consts::PI);
        if dirs.len() < 6 && dirs.iter().all(|b| ((a - b).abs() - std::f64::consts::PI).abs() > 0.02 && (a - b).abs() > 0.02) {
            dirs.push(a);
        }
    }
    let ((x0, y0), (x1, y1)) = z0
        .iter()
        .map(|z| footprint::bbox(&z.outer))
        .reduce(|a, b| ((a.0 .0.min(b.0 .0), a.0 .1.min(b.0 .1)), (a.1 .0.max(b.1 .0), a.1 .1.max(b.1 .1))))?;
    let step = ((x1 - x0).hypot(y1 - y0) / 60.0).max(0.25);
    let mut best: Option<(f64, Frame)> = None;
    for &a in &dirs {
        let u = (a.cos(), a.sin());
        let nn = (-u.1, u.0);
        let mut y = y0;
        while y <= y1 {
            let mut x = x0;
            while x <= x1 {
                let c = (x, y);
                let score = dist(c, target);
                if best.as_ref().is_none_or(|b| score < b.0 - 1e-9) {
                    let f = Frame {
                        o: (c.0 - u.0 * w / 2.0 - nn.0 * d / 2.0, c.1 - u.1 * w / 2.0 - nn.1 * d / 2.0),
                        u,
                    };
                    if zs.iter().all(|z| fits(&f, w, d, z)) {
                        best = Some((score, f));
                    }
                }
                x += step;
            }
            y += step;
        }
    }
    let (_, f) = best?;
    // the front towards the nearer edge of the interior, over all floors
    let reach = |f: &Frame| {
        let (p, nn) = (f.at(w / 2.0, d), f.n());
        zs.iter()
            .map(|z| {
                let mut s = 0.0;
                while s < 500.0 && in_any(z, (p.0 + nn.0 * s, p.1 + nn.1 * s)) {
                    s += 0.1;
                }
                s
            })
            .fold(0.0, f64::max)
    };
    let back = Frame { o: f.at(w, d), u: (-f.u.0, -f.u.1) };
    Some(if reach(&back) < reach(&f) { back } else { f })
}

/// The core in the band of the outer ring `rb` of `base`, one placement for every facade: its
/// back on a chord between two facade points, its depth the band's less the chord's sag, so the
/// rectangle stays inside the band on a straight, faceted or curved facade alike. The band
/// stretch it takes runs between cut positions round the rectangle. The flattest chord wins,
/// then the one with the most cut room on both sides: on a straight facade, the middle of the
/// longest side. `Ok(None)` when no chord holds the core.
fn band_seat(spec: &ProjectSpec, base: &Region, rb: &RingBand, lp: &Lp, d: f64, pair: bool) -> Result<Option<Core>, GenError> {
    let per = rb.perimeter;
    let f0 = Frame { o: (0.0, 0.0), u: (1.0, 0.0) };
    let w0 = core_at(spec, f0, d, pair)?.w;
    let mut widths: Vec<(i64, Option<f64>)> = vec![];
    let mut width = |de: f64| {
        let k = mm(de);
        if let Some(&(_, w)) = widths.iter().find(|x| x.0 == k) {
            return w;
        }
        let w = core_at(spec, f0, de, pair).ok().map(|c| c.w);
        widths.push((k, w));
        w
    };
    let at = |t: f64| {
        let (e, s) = rb.locate(t);
        rb.edges[e].at(s, 0.0)
    };
    // the loop position past `t0` whose facade point is `w` away
    let chord_end = |t0: f64, w: f64| -> Option<f64> {
        let a = at(t0);
        if dist(at(t0 + w), a) >= w - 1e-9 {
            return Some(t0 + w);
        }
        let (mut lo, mut hi) = (t0 + w, t0 + w + 0.1);
        while dist(at(hi), a) < w {
            lo = hi;
            hi += 0.1;
            if hi > t0 + per / 2.0 {
                return None;
            }
        }
        for _ in 0..50 {
            let m = (lo + hi) / 2.0;
            if dist(at(m), a) < w {
                lo = m;
            } else {
                hi = m;
            }
        }
        Some(hi)
    };
    // how far the facade bows out behind the chord; None where it dips inside, off the plate
    let sag_of = |t0: f64, t1: f64, f: &Frame| -> Option<f64> {
        let mut sag: f64 = 0.0;
        for (e, &st) in rb.edges.iter().zip(&rb.start) {
            for k in [-1.0, 0.0, 1.0] {
                let t = st + k * per;
                if t > t0 + 1e-9 && t < t1 - 1e-9 {
                    let y = f.local(e.a).1;
                    if y > 1e-6 {
                        return None;
                    }
                    sag = sag.max(-y);
                }
            }
        }
        Some(sag)
    };
    let spans = &lp.spans[0];
    let span_of = |t: f64| spans.iter().copied().find(|(a, b)| t.rem_euclid(per) >= a - 1e-9 && t.rem_euclid(per) <= b + 1e-9);
    let ring0 = std::slice::from_ref(rb);
    let inner = offset::inset(base, d);
    let mut starts: Vec<f64> = spans.iter().filter(|(a, b)| b - a >= w0).map(|(a, b)| (a + b - w0) / 2.0).collect();
    let n = (per / 0.25).ceil() as usize;
    starts.extend((0..n).map(|i| per * i as f64 / n as f64));
    let mut best: Option<((i64, f64), Core)> = None;
    for t0 in starts {
        // the core's width at the depth its chord leaves, until the chord holds it
        let mut w = w0;
        let mut seat = None;
        for _ in 0..4 {
            let Some(t1) = chord_end(t0, w) else { break };
            let (a, b) = (at(t0), at(t1));
            let l = dist(a, b).max(1e-9);
            let f = Frame { o: a, u: ((b.0 - a.0) / l, (b.1 - a.1) / l) };
            let Some(sag) = sag_of(t0, t1, &f) else { break };
            let de = if sag <= 1e-9 { d } else { ((d - sag) * 1000.0).floor() / 1000.0 };
            let Some(wd) = width(de) else { break };
            if wd <= w + 1e-9 {
                seat = Some((f, sag, de));
                break;
            }
            w = wd;
        }
        let Some((f, sag, de)) = seat else { continue };
        let key_sag = (sag * 100.0).round() as i64;
        if best.as_ref().is_some_and(|b| key_sag > b.0 .0) {
            continue;
        }
        let mut core = core_at(spec, f, de, pair)?;
        // every stair and lift cell opens on the corridor: the plate past its front, half a
        // corridor beyond the band, is the corridor's side of the inset and not another band
        // (a chord by a re-entrant corner faces the other wing's rooms)
        let opens = core.cells.iter().filter(|c| matches!(c.2, RoomKind::Stair | RoomKind::Lift)).all(|c| {
            let x = (c.0 + c.1) / 2.0;
            let p = f.at(x, de + sag + spec.corridor / 2.0);
            in_any(&inner, p)
        });
        if !opens {
            continue;
        }
        // the rectangle inside the plate and the band, and the band stretch round it
        let tc = t0 + core.w / 2.0;
        let (mut ts0, mut ts1) = (t0, t0 + core.w);
        let steps = |l: f64| ((l / 0.25).ceil() as usize).max(1);
        let (nw, nd) = (steps(core.w), steps(de));
        let mut pts: Vec<(P, bool)> = vec![];
        for i in 0..=nw {
            let x = (core.w * i as f64 / nw as f64).clamp(1e-3, core.w - 1e-3);
            pts.push((f.at(x, 1e-3), false));
            pts.push((f.at(x, de - 1e-3), true));
        }
        for j in 0..=nd {
            let y = (de * j as f64 / nd as f64).clamp(1e-3, de - 1e-3);
            pts.push((f.at(1e-3, y), false));
            pts.push((f.at(core.w - 1e-3, y), false));
        }
        let mut ok = true;
        for &(p, front) in &pts {
            let Some((_, t)) = station(ring0, p).filter(|_| offset::contains(base, p) && !(front && in_any(&inner, p))) else {
                ok = false;
                break;
            };
            let t = t + per * ((tc - t) / per).round();
            if t < ts0 - 1e-6 {
                ts0 = t;
            }
            if t > ts1 + 1e-6 {
                ts1 = t;
            }
        }
        if !ok || ts1 - ts0 > core.w + 2.0 * d {
            continue;
        }
        let (Some(s0), Some(s1)) = (lp.prev_ok(Strip::Full, ts0, &[]), lp.next_ok(Strip::Full, ts1, &[])) else { continue };
        let (Some(a0), Some(a1)) = (span_of(s0), span_of(s1)) else { continue };
        if ts0 - s0 > d || s1 - ts1 > d {
            continue;
        }
        let room = (s0.rem_euclid(per) - a0.0).min(a1.1 - s1.rem_euclid(per));
        if best.as_ref().is_some_and(|b| key_sag == b.0 .0 && room < b.0 .1) {
            continue;
        }
        // where each inner cell wall meets the band, as a loop position
        let mut seat = vec![s0];
        for cl in &core.cells[1..] {
            let p = f.at(cl.0, de / 2.0);
            let t = station(ring0, p).map_or(t0 + cl.0, |(_, t)| t + per * ((tc - t) / per).round());
            seat.push(if (t - (t0 + cl.0)).abs() < 1e-6 { t0 + cl.0 } else { t.clamp(s0, s1) });
        }
        seat.push(s1);
        core.band = Some((s0, s1));
        core.seat = seat;
        core.sag = sag;
        best = Some(((key_sag, room), core));
    }
    Ok(best.map(|b| b.1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Strip {
    Full,
    Front,
    Back,
}

/// A stretch of one facade loop that belongs to a room, `t0..t1` unwrapped.
#[derive(Clone, Debug)]
struct Zone {
    room: usize,
    ring: usize,
    t0: f64,
    t1: f64,
    strip: Strip,
}

#[derive(Clone, Debug)]
struct Unit {
    kind: RoomKind,
    name: String,
    owner: PRole,
    asked: Option<f64>,
    grown: bool,
}

/// One facade loop of a floor: where cuts may go and the cuts made.
struct Lp<'a> {
    rb: &'a RingBand,
    spans: [Vec<(f64, f64)>; 3],
    depth: [(f64, f64); 3],
    cuts: Vec<f64>,
}

impl<'a> Lp<'a> {
    fn new(rb: &'a RingBand, d: f64, df: f64) -> Lp<'a> {
        Lp::with_runs(rb, d, df, 0)
    }

    /// `tight` 1 drops the corner runs, 2 the door runs too, for a facade too short for them.
    fn with_runs(rb: &'a RingBand, d: f64, df: f64, tight: u8) -> Lp<'a> {
        let depth = [(0.0, d), (0.0, df), (df, d)];
        let n = rb.edges.len();
        let corner = |i: usize| {
            let (p, q) = (rb.edges[(i + n - 1) % n].u, rb.edges[i].u);
            cross(p, q).atan2(dot(p, q)).abs() > CURVE_TURN
        };
        let spans = depth.map(|(h0, h1)| {
            let mut v = vec![];
            // a room round a corner meets the corridor only past the corner's full depth: a
            // cut there keeps a door's run (widest door and its jambs) on the corridor side,
            // where the edge is long enough for a room as well; short facets keep the margin
            let corr = h1 >= d - 1e-9;
            let run = door_run();
            for (i, (e, st)) in rb.edges.iter().zip(&rb.start).enumerate() {
                let pieces = e.full(h0, h1);
                let last = pieces.len().saturating_sub(1);
                for (j, (a, b)) in pieces.into_iter().enumerate() {
                    let base = if tight < 2 && corr && b - a >= 2.0 * run + MIN_ROOM_W { run } else { MARGIN };
                    // a room wrapping a corner keeps a run of wall past it for its door or window
                    let m = |at: usize, on: bool| if on && tight == 0 && corner(at) { base.max(CORNER_RUN) } else { base };
                    let (a, b) = (a + m(i, j == 0), b - m((i + 1) % n, j == last));
                    if b >= a {
                        v.push((st + a, st + b));
                    }
                }
            }
            v
        });
        Lp { rb, spans, depth, cuts: vec![] }
    }

    fn p(&self) -> f64 {
        self.rb.perimeter
    }

    fn idx(s: Strip) -> usize {
        match s {
            Strip::Full => 0,
            Strip::Front => 1,
            Strip::Back => 2,
        }
    }

    /// The first position from `t` where a cut of `s` may go.
    fn next_span(&self, s: Strip, t: f64) -> Option<f64> {
        let sp = &self.spans[Self::idx(s)];
        let p = self.p();
        let x = t.rem_euclid(p);
        let base = t - x;
        if let Some(&(a, _)) = sp.iter().find(|(_, b)| *b >= x - 1e-9) {
            return Some(base + a.max(x));
        }
        sp.first().map(|&(a, _)| base + p + a)
    }

    /// The first cut position from `t` that keeps clear of the cuts made.
    fn next_ok(&self, s: Strip, t: f64, more: &[f64]) -> Option<f64> {
        let mut x = t;
        for _ in 0..400 {
            let y = self.next_span(s, x)?;
            let near = self.cuts.iter().chain(more).find(|&&c| {
                let dd = (y - c).abs();
                dd > 1e-9 && dd < MARGIN
            });
            match near {
                None => return Some(y),
                Some(&c) if c > y => x = c,
                Some(&c) => x = c + MARGIN,
            }
        }
        None
    }

    /// The last cut position at or before `t`, clear of the cuts made.
    fn prev_ok(&self, s: Strip, t: f64, more: &[f64]) -> Option<f64> {
        let sp = &self.spans[Self::idx(s)];
        let p = self.p();
        let mut x = t;
        for _ in 0..400 {
            let r = x.rem_euclid(p);
            let base = x - r;
            let y = match sp.iter().rev().find(|(a, _)| *a <= r + 1e-9) {
                Some(&(_, b)) => base + b.min(r),
                None => base - p + sp.last()?.1,
            };
            let near = self.cuts.iter().chain(more).find(|&&c| {
                let dd = (y - c).abs();
                dd > 1e-9 && dd < MARGIN
            });
            match near {
                None => return Some(y),
                Some(&c) if c < y => x = c,
                Some(&c) => x = c - MARGIN,
            }
        }
        None
    }

    fn area(&self, s: Strip, t0: f64, t1: f64) -> f64 {
        let (h0, h1) = self.depth[Self::idx(s)];
        if t1 <= t0 {
            return 0.0;
        }
        self.rb.area(h0, h1, t0, t1)
    }

    /// Where a room from `t0` of `area` ends, before `limit`.
    fn solve(&self, s: Strip, t0: f64, area: f64, limit: f64) -> Option<f64> {
        if self.area(s, t0, limit) < area - 1e-9 {
            return None;
        }
        let (mut lo, mut hi) = (t0, limit);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            if self.area(s, t0, mid) < area {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(hi)
    }

    /// Where a room ending at `t1` of `area` starts, after `limit`.
    fn solve_back(&self, s: Strip, t1: f64, area: f64, limit: f64) -> Option<f64> {
        if self.area(s, limit, t1) < area - 1e-9 {
            return None;
        }
        let (mut lo, mut hi) = (limit, t1);
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            if self.area(s, mid, t1) < area {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Some(lo)
    }
}

/// What a floor holds before it is cut: rooms and their stretches of facade.
#[derive(Default)]
struct FloorPlan {
    units: Vec<Unit>,
    zones: Vec<Zone>,
}

impl FloorPlan {
    fn add(&mut self, u: Unit) -> usize {
        self.units.push(u);
        self.units.len() - 1
    }
}

enum Item {
    Entrance,
    Room(RoomReq),
    Flat(super::ApartmentReq),
}

fn public(kind: RoomKind, name: String, asked: Option<f64>) -> Unit {
    Unit { kind, name, owner: PRole::Public, asked, grown: false }
}

/// Lays the program of floor `k` along the facade loops.
struct Planner<'a> {
    spec: &'a ProjectSpec,
    plan: FloorPlan,
    next_apt: &'a mut u32,
}

impl Planner<'_> {
    fn spare_unit(&self) -> Unit {
        let s = &self.spec.spare;
        public(s.kind, if self.spec.ru { &s.name.1 } else { &s.name.0 }.clone(), None)
    }

    /// Fills `cur..end` of a loop with spare rooms, or grows the last room when it is short.
    fn close(&mut self, lp: &mut Lp, ring: usize, cur: f64, end: f64, last: Option<usize>) -> Result<(), GenError> {
        let left = end - cur;
        if left < 1e-6 {
            return Ok(());
        }
        if left < SPARE_MIN {
            if let Some(i) = last {
                // every stretch that ended at the last cut now runs to the loop end
                let edge = self.plan.zones[i].t1;
                for z in self.plan.zones.iter_mut().filter(|z| z.ring == ring && (z.t1 - edge).abs() < 1e-9) {
                    z.t1 = end;
                    if let Some(u) = self.plan.units.get_mut(z.room) {
                        u.grown = true;
                    }
                }
                lp.cuts.retain(|c| (c - edge).abs() > 1e-9);
                return Ok(());
            }
            if left < 1.5 {
                return Err(infeasible(format!(
                    "a courtyard facade of {left:.2} m holds no room: enlarge the courtyard"
                )));
            }
        }
        let n = (left / SPARE_MAX).ceil().max(1.0) as usize;
        let mut a = cur;
        for i in 1..=n {
            let b = if i == n {
                end
            } else {
                match lp.next_ok(Strip::Full, cur + left * i as f64 / n as f64, &[]) {
                    Some(b) if end - b >= SPARE_MIN && b - a >= SPARE_MIN => b,
                    _ => continue,
                }
            };
            let r = self.plan.add(self.spare_unit());
            self.plan.zones.push(Zone { room: r, ring, t0: a, t1: b, strip: Strip::Full });
            lp.cuts.push(b);
            a = b;
        }
        Ok(())
    }

    /// One full-depth room of `area` (or `width` when the builder sizes it) from `cur`.
    fn full(&mut self, lp: &mut Lp, ring: usize, cur: f64, end: f64, u: Unit, width: Option<f64>) -> Option<(f64, usize)> {
        let (t1, grown) = match (width, u.asked) {
            (Some(w), _) => (lp.next_ok(Strip::Full, cur + w, &[])?, false),
            (None, Some(a)) => {
                let t = lp.solve(Strip::Full, cur, a, end)?;
                let t = t.max(cur + MIN_ROOM_W);
                let ok = lp.next_ok(Strip::Full, t, &[])?;
                (ok, ok > t + 1e-6 || lp.area(Strip::Full, cur, ok) > a * 1.0001)
            }
            (None, None) => return None,
        };
        if t1 > end + 1e-9 {
            return None;
        }
        let r = self.plan.add(Unit { grown: u.grown || grown, ..u });
        self.plan.zones.push(Zone { room: r, ring, t0: cur, t1, strip: Strip::Full });
        lp.cuts.push(t1);
        Some((t1, self.plan.zones.len() - 1))
    }

    /// Small rooms tucked behind the front of a host room, the host reaching the corridor
    /// past them.
    fn hosted(&mut self, lp: &mut Lp, ring: usize, cur: f64, end: f64, tucks: &[Unit], host: Unit) -> Option<(f64, usize)> {
        let a_host = host.asked?;
        let mut more = vec![];
        let mut x = cur;
        let mut placed = vec![];
        for t in tucks {
            let want = t.asked?;
            let e = lp.solve(Strip::Back, x, want, end)?;
            let e = e.max(x + 1.25);
            let ok = lp.next_ok(Strip::Back, e, &more)?;
            let grown = ok > e + 1e-6 || lp.area(Strip::Back, x, ok) > want * 1.0001;
            placed.push((x, ok, grown));
            more.push(ok);
            x = ok;
        }
        let front = lp.area(Strip::Front, cur, x);
        if front > 0.7 * a_host {
            return None;
        }
        let e = lp.solve(Strip::Full, x, a_host - front, end)?;
        let e = e.max(x + MIN_ROOM_W);
        let t1 = lp.next_ok(Strip::Full, e, &more)?;
        if t1 > end + 1e-9 {
            return None;
        }
        let grown = lp.area(Strip::Full, x, t1) + front > a_host * 1.0001;
        let h = self.plan.add(Unit { grown: host.grown || grown, ..host });
        self.plan.zones.push(Zone { room: h, ring, t0: cur, t1: x, strip: Strip::Front });
        for (t, (a, b, g)) in tucks.iter().zip(placed) {
            let r = self.plan.add(Unit { grown: t.grown || g, ..t.clone() });
            self.plan.zones.push(Zone { room: r, ring, t0: a, t1: b, strip: Strip::Back });
        }
        self.plan.zones.push(Zone { room: h, ring, t0: x, t1, strip: Strip::Full });
        lp.cuts.extend(more);
        lp.cuts.push(t1);
        Some((t1, self.plan.zones.len() - 1))
    }

    /// An apartment from `cur`: facade rooms in front, hall and wet rooms behind them.
    fn flat(&mut self, lp: &mut Lp, ring: usize, cur: f64, end: f64, a: &super::ApartmentReq) -> Option<(f64, usize)> {
        let ru = self.spec.ru;
        let mut front: Vec<&RoomReq> = vec![];
        let mut back: Vec<&RoomReq> = vec![];
        let mut hall_name = names::kind_name(RoomKind::Hall, ru).to_string();
        let mut rooms: Vec<&RoomReq> = vec![];
        for r in &a.rooms {
            for _ in 0..r.count {
                rooms.push(r);
            }
        }
        for r in rooms {
            match r.kind {
                RoomKind::Hall => hall_name = r.name.clone(),
                k if k.inner() && back.len() < 2 => back.push(r),
                _ => front.push(r),
            }
        }
        if front.is_empty() {
            return None;
        }
        // widest rooms at the ends, where the wet rooms sit behind them
        front.sort_by(|x, y| y.area.total_cmp(&x.area));
        let mut order: Vec<&RoomReq> = vec![];
        let mut tail = vec![];
        for (i, r) in front.into_iter().enumerate() {
            if i == 1 {
                tail.push(r);
            } else {
                order.push(r);
            }
        }
        order.extend(tail);
        back.sort_by(|x, y| x.area.total_cmp(&y.area));
        let mut cuts = vec![];
        let mut x = cur;
        let mut fronts = vec![];
        for (i, r) in order.iter().enumerate() {
            let last = i + 1 == order.len();
            let e = lp.solve(Strip::Front, x, r.area, end)?.max(x + MIN_ROOM_W);
            let ok = if last { lp.next_ok(Strip::Full, e, &cuts)? } else { lp.next_ok(Strip::Front, e, &cuts)? };
            fronts.push((x, ok, ok > e + 1e-6 || lp.area(Strip::Front, x, ok) > r.area * 1.0001));
            cuts.push(ok);
            x = ok;
        }
        let mut stop = x;
        for _ in 0..8 {
            if stop > end + 1e-9 {
                return None;
            }
            for flip in [false, true] {
                if let Some(b) = self.flat_back(lp, cur, stop, &back, &fronts, &cuts, flip) {
                    let n = fronts.len();
                    fronts[n - 1].1 = stop;
                    if stop > x + 1e-9 {
                        fronts[n - 1].2 = true;
                    }
                    *self.next_apt += 1;
                    let owner = PRole::Apartment(*self.next_apt);
                    let mut last_zone = 0;
                    for (r, (t0, t1, g)) in order.iter().zip(&fronts) {
                        let id = self.plan.add(Unit { kind: r.kind, name: r.name.clone(), owner, asked: Some(r.area), grown: *g });
                        let strip = Strip::Front;
                        self.plan.zones.push(Zone { room: id, ring, t0: *t0, t1: *t1, strip });
                    }
                    for (kind, name, asked, t0, t1, g) in b {
                        let id = self.plan.add(Unit { kind, name: if kind == RoomKind::Hall { hall_name.clone() } else { name }, owner, asked, grown: g });
                        self.plan.zones.push(Zone { room: id, ring, t0, t1, strip: Strip::Back });
                        if (t1 - stop).abs() < 1e-9 {
                            last_zone = self.plan.zones.len() - 1;
                        }
                    }
                    lp.cuts.extend(fronts.iter().map(|f| f.1));
                    lp.cuts.extend(self_cuts_back(&self.plan.zones, ring, cur, stop));
                    lp.cuts.sort_by(f64::total_cmp);
                    lp.cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
                    return Some((stop, last_zone));
                }
            }
            // more room behind: the apartment widens by its last facade room
            stop = lp.next_ok(Strip::Full, stop + 1.0, &cuts)?;
        }
        None
    }

    /// The back strip `cur..stop`: wet rooms at its ends, the hall between; `None` when a
    /// facade room would not share a door's width with the hall.
    #[allow(clippy::too_many_arguments)]
    fn flat_back(
        &self,
        lp: &Lp,
        cur: f64,
        stop: f64,
        back: &[&RoomReq],
        fronts: &[(f64, f64, bool)],
        cuts: &[f64],
        flip: bool,
    ) -> Option<Vec<(RoomKind, String, Option<f64>, f64, f64, bool)>> {
        let mut cuts = cuts.to_vec();
        cuts.push(stop);
        let (first, second) = match (back.len(), flip) {
            (0, _) => (None, None),
            (1, false) => (Some(back[0]), None),
            (1, true) => (None, Some(back[0])),
            (_, false) => (Some(back[0]), Some(back[1])),
            (_, true) => (Some(back[1]), Some(back[0])),
        };
        let mut out = vec![];
        let mut h0 = cur;
        if let Some(r) = first {
            let e = lp.solve(Strip::Back, cur, r.area, stop)?.max(cur + 1.25);
            let ok = lp.next_ok(Strip::Back, e, &cuts)?;
            out.push((r.kind, r.name.clone(), Some(r.area), cur, ok, lp.area(Strip::Back, cur, ok) > r.area * 1.0001));
            cuts.push(ok);
            h0 = ok;
        }
        let mut h1 = stop;
        let mut tail = None;
        if let Some(r) = second {
            let s = lp.solve_back(Strip::Back, stop, r.area, h0)?.min(stop - 1.25);
            let ok = lp.prev_ok(Strip::Back, s, &cuts)?;
            tail = Some((r.kind, r.name.clone(), Some(r.area), ok, stop, lp.area(Strip::Back, ok, stop) > r.area * 1.0001));
            h1 = ok;
        }
        if h1 - h0 < 1.5 {
            return None;
        }
        let shared = |a: f64, b: f64| (b.min(h1) - a.max(h0)).max(0.0);
        if fronts.iter().enumerate().any(|(i, f)| {
            let b = if i + 1 == fronts.len() { stop } else { f.1 };
            shared(f.0, b) < HALL_OVERLAP
        }) {
            return None;
        }
        out.push((RoomKind::Hall, String::new(), None, h0, h1, false));
        out.extend(tail);
        Some(out)
    }

    /// `item` placed at the first cut past `cur` where it fits (an apartment that cannot start
    /// at an inner corner starts past it); the stretch it skips becomes spare rooms, or the
    /// last room grows over it, as at the loop end.
    #[allow(clippy::too_many_arguments)]
    fn skip_to(&mut self, lp: &mut Lp, ring: usize, cur: f64, end: f64, last: Option<usize>, item: &Item) -> Option<(f64, usize)> {
        let ru = self.spec.ru;
        let mut s = cur;
        for _ in 0..200 {
            s = lp.next_ok(Strip::Full, s + MARGIN, &[])?;
            if s > end - MIN_ROOM_W {
                return None;
            }
            if s - cur < SPARE_MIN && last.is_none() {
                continue;
            }
            let placed = match item {
                Item::Entrance => {
                    let u = public(RoomKind::Lobby, names::kind_name(RoomKind::Lobby, ru).to_string(), None);
                    self.full(lp, ring, s, end, u, Some(ENTRANCE_W))
                }
                Item::Room(r) => self.full(lp, ring, s, end, public(r.kind, r.name.clone(), Some(r.area)), None),
                Item::Flat(a) => self.flat(lp, ring, s, end, a),
            };
            if let Some(p) = placed {
                self.close(lp, ring, cur, s, last).ok()?;
                if !lp.cuts.iter().any(|c| (c - s).abs() < 1e-9) {
                    lp.cuts.push(s);
                }
                return Some(p);
            }
        }
        None
    }
}

fn self_cuts_back(zones: &[Zone], ring: usize, cur: f64, stop: f64) -> Vec<f64> {
    zones
        .iter()
        .filter(|z| z.ring == ring && z.strip == Strip::Back && z.t0 >= cur - 1e-9 && z.t1 <= stop + 1e-9)
        .flat_map(|z| [z.t0, z.t1])
        .collect()
}

/// Loop start of a ring: the first full-depth cut on its longest edge.
fn loop_start(lp: &Lp) -> Option<f64> {
    let (i, _) = lp.rb.edges.iter().enumerate().max_by(|a, b| a.1.len.total_cmp(&b.1.len))?;
    lp.next_span(Strip::Full, lp.rb.start[i])
}

/// A corner zone goes to the room that fills it best. When the next room in the queue would
/// end where no cut may go (round a corner, past a room's width of facade), the room of the
/// queue that leaves the least area over its own takes the zone instead; the rest keep order.
fn corner_fill(lp: &Lp, queue: &mut std::collections::VecDeque<Item>, cur: f64, end: f64) {
    let fit = |r: &RoomReq| -> Option<(f64, f64)> {
        let t = lp.solve(Strip::Full, cur, r.area, end)?.max(cur + MIN_ROOM_W);
        let ok = lp.next_ok(Strip::Full, t, &[])?;
        (ok <= end + 1e-9).then(|| (ok - t, lp.area(Strip::Full, cur, ok) - r.area))
    };
    let Some(Item::Room(r0)) = queue.front() else { return };
    let Some((jump, w0)) = fit(r0) else { return };
    if jump < MIN_ROOM_W {
        return;
    }
    let best = queue
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(i, it)| match it {
            Item::Room(r) => fit(r).map(|f| (i, f.1)),
            _ => None,
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    if let Some((i, _)) = best.filter(|b| b.1 < w0 - 1e-6) {
        if let Some(it) = queue.remove(i) {
            queue.push_front(it);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_floor_with(
    spec: &ProjectSpec,
    k: usize,
    rbs: &[RingBand],
    d: f64,
    df: f64,
    core: Option<&Core>,
    next_apt: &mut u32,
    tight: u8,
) -> Result<FloorPlan, GenError> {
    let ru = spec.ru;
    let mut items: Vec<Item> = vec![];
    if k == 0 {
        items.push(Item::Entrance);
    }
    let periodic = if spec.typology == Typology::ApartmentSection { spec.periodic_at(k) } else { None };
    if spec.typology == Typology::ApartmentSection && periodic.is_none() {
        for a in &spec.apartments {
            for _ in 0..a.count {
                items.push(Item::Flat(a.clone()));
            }
        }
    } else {
        for r in periodic.unwrap_or_else(|| spec.storey_rooms(k)) {
            for _ in 0..r.count {
                items.push(Item::Room(RoomReq { count: 1, ..r.clone() }));
            }
        }
    }
    let mut pl = Planner { spec, plan: FloorPlan::default(), next_apt };
    let mut lps: Vec<Lp> = rbs.iter().map(|rb| Lp::with_runs(rb, d, df, tight)).collect();
    let mut queue: std::collections::VecDeque<Item> = items.into();
    let es = core.and_then(|c| c.end.as_ref());
    let rings = lps.len();
    for (ring, lp) in lps.iter_mut().enumerate() {
        let (start, end) = match (ring, core.and_then(|c| c.band.map(|b| (c, b)))) {
            (0, Some((c, (t0, t1)))) => {
                for (i, &(x0, x1, kind)) in c.cells.iter().enumerate() {
                    let (a, b) = c.seat.get(i).zip(c.seat.get(i + 1)).map_or((t0 + x0, t0 + x1), |(a, b)| (*a, *b));
                    let r = pl.plan.add(public(kind, names::kind_name(kind, ru).to_string(), None));
                    pl.plan.zones.push(Zone { room: r, ring: 0, t0: a, t1: b, strip: Strip::Full });
                    lp.cuts.extend([a, b]);
                }
                (t1, t0 + lp.p())
            }
            (0, None) if es.is_some() => {
                let e = es.unwrap();
                let r = pl.plan.add(public(RoomKind::Stair, names::kind_name(RoomKind::Stair, ru).to_string(), None));
                pl.plan.zones.push(Zone { room: r, ring: 0, t0: e.t0, t1: e.t1, strip: Strip::Full });
                lp.cuts.extend([e.t0, e.t1]);
                (e.t1, e.t0 + lp.p())
            }
            _ => {
                let s = loop_start(lp).ok_or_else(|| {
                    infeasible("the plate has no straight facade long enough for a room: enlarge it")
                })?;
                lp.cuts.extend([s, s + lp.p()]);
                (s, s + lp.p())
            }
        };
        let mut cur = start;
        let mut last: Option<usize> = None;
        let mut tucks: Vec<Unit> = vec![];
        loop {
            if tucks.is_empty() {
                corner_fill(lp, &mut queue, cur, end);
            }
            let Some(item) = queue.front() else { break };
            let placed = match item {
                Item::Entrance => {
                    let u = public(RoomKind::Lobby, names::kind_name(RoomKind::Lobby, ru).to_string(), None);
                    pl.full(lp, ring, cur, end, u, Some(ENTRANCE_W))
                }
                Item::Room(r) => {
                    let u = public(r.kind, r.name.clone(), Some(r.area));
                    let narrow = lp.solve(Strip::Full, cur, r.area, end).is_some_and(|t| t - cur < MIN_ROOM_W);
                    if narrow && df >= 2.5 && d - df >= 1.2 {
                        tucks.push(u);
                        queue.pop_front();
                        continue;
                    }
                    if tucks.is_empty() {
                        pl.full(lp, ring, cur, end, u, None)
                    } else {
                        let t = std::mem::take(&mut tucks);
                        match pl.hosted(lp, ring, cur, end, &t, u.clone()) {
                            Some(x) => Some(x),
                            None => {
                                // no host for them: each small room takes a narrow full bay
                                let mut at = cur;
                                let mut ok = true;
                                for s in t {
                                    match pl.full(lp, ring, at, end, s, None) {
                                        Some((e, z)) => {
                                            at = e;
                                            last = Some(z);
                                        }
                                        None => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                                if ok {
                                    cur = at;
                                    pl.full(lp, ring, cur, end, u, None)
                                } else {
                                    None
                                }
                            }
                        }
                    }
                }
                Item::Flat(a) => {
                    let a = a.clone();
                    pl.flat(lp, ring, cur, end, &a)
                }
            };
            // on the last loop a stuck item would fail the floor: it takes the facade further on
            let placed = match placed {
                None if tucks.is_empty() && ring + 1 == rings => {
                    queue.front().and_then(|it| pl.skip_to(lp, ring, cur, end, last, it))
                }
                p => p,
            };
            match placed {
                Some((e, z)) => {
                    cur = e;
                    last = Some(z);
                    queue.pop_front();
                }
                None => break,
            }
        }
        // small rooms left at the end of the program take full bays
        for s in std::mem::take(&mut tucks) {
            let (e, z) = pl.full(lp, ring, cur, end, s, None).ok_or_else(|| {
                infeasible(format!("floor {}: the facade is too short for the program", k + 1))
            })?;
            cur = e;
            last = Some(z);
        }
        pl.close(lp, ring, cur, end, last)?;
    }
    if let Some(first) = queue.front() {
        // the why names the first item left and the floor area the bands still had for it
        let first = match first {
            Item::Entrance => "the entrance".to_string(),
            Item::Room(r) => format!("{} of {:.1} m2", r.name, r.area),
            Item::Flat(a) => format!("apartment {} of {} rooms", a.name, a.rooms.len()),
        };
        let used: f64 = pl.plan.zones.iter().map(|z| lps[z.ring].area(z.strip, z.t0, z.t1)).sum();
        let bands: f64 = lps.iter().map(|lp| lp.area(Strip::Full, 0.0, lp.p())).sum();
        let left = queue.len();
        let facade: f64 = rbs.iter().map(|r| r.perimeter).sum();
        return Err(infeasible(format!(
            "floor {}: {left} rooms or apartments of the program do not fit along {facade:.1} m of \
             facade (first left: {first}; the room bands hold {bands:.0} m2, {used:.0} m2 laid out): \
             enlarge the footprint, add floors or shrink the program",
            k + 1
        )));
    }
    Ok(pl.plan)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lab {
    Room(usize),
    Corr(u32),
    Lost,
}

/// Which loop and loop position a band point belongs to: courtyards first.
fn station(rbs: &[RingBand], p: P) -> Option<(usize, f64)> {
    for (ri, rb) in rbs.iter().enumerate().rev() {
        for (e, st) in rb.edges.iter().zip(&rb.start) {
            if crate::check::inside(&e.face, p) {
                let (s, _) = e.local(p);
                return Some((ri, st + s.clamp(0.0, e.len)));
            }
        }
    }
    None
}

fn within(z: &Zone, t: f64, per: f64) -> bool {
    [-1.0, 0.0, 1.0, 2.0].iter().any(|j| {
        let x = t + j * per;
        x >= z.t0 - 1e-12 && x < z.t1
    })
}

struct Floor<'a> {
    region: &'a Region,
    id: Vec<Region>,
    ifront: Vec<Region>,
    rbs: &'a [RingBand],
    plan: &'a FloorPlan,
    core: Option<(&'a Core, usize)>,
    /// A core in the band: its cells are the plan's first rooms, walled across its chord.
    seat: Option<&'a Core>,
    stores: Vec<(Vec<P>, usize)>,
    splits: Vec<(P, P)>,
}

impl Floor<'_> {
    fn cls(&self, p: P) -> Option<Lab> {
        if !offset::contains(self.region, p) {
            return None;
        }
        if let Some((c, first)) = self.core.filter(|(c, _)| c.band.is_none()) {
            let (x, y) = c.f.local(p);
            if x > 0.0 && x < c.w && y > 0.0 && y < c.d {
                let i = c.cells.iter().position(|cl| x < cl.1).unwrap_or(c.cells.len() - 1);
                return Some(Lab::Room(first + i));
            }
        }
        if !in_any(&self.id, p) {
            let Some((ring, t)) = station(self.rbs, p) else { return Some(Lab::Lost) };
            let back = in_any(&self.ifront, p);
            let per = self.rbs[ring].perimeter;
            let z = self.plan.zones.iter().find(|z| {
                z.ring == ring
                    && within(z, t, per)
                    && match z.strip {
                        Strip::Full => true,
                        Strip::Front => !back,
                        Strip::Back => back,
                    }
            });
            // the cell walls of a band core stand square to its chord, not to the facade
            let room = |z: &Zone| match self.seat {
                Some(c) if z.room < c.cells.len() => {
                    let x = c.f.local(p).0;
                    c.cells.iter().position(|cl| x < cl.1).unwrap_or(c.cells.len() - 1)
                }
                _ => z.room,
            };
            return Some(z.map_or(Lab::Lost, |z| Lab::Room(room(z))));
        }
        if let Some((_, r)) = self.stores.iter().find(|(ring, _)| crate::check::inside(ring, p)) {
            return Some(Lab::Room(*r));
        }
        let mut bits = 0u32;
        for (i, (o, dir)) in self.splits.iter().enumerate() {
            if cross(*dir, sub(p, *o)) > 0.0 {
                bits |= 1 << i;
            }
        }
        Some(Lab::Corr(bits))
    }
}

/// The region with each run of gentle facets (a curve's polygon) replaced by chords off it by
/// at most `CHORD_SAG`, through `fixed` points on its rings: a room between two cuts then has
/// one straight wall to the corridor, long enough for its door.
fn chorded(r: &Region, fixed: &[P]) -> Region {
    Region { outer: chord_ring(&r.outer, fixed), holes: r.holes.iter().map(|h| chord_ring(h, fixed)).collect() }
}

fn chord_ring(r0: &[P], fixed: &[P]) -> Vec<P> {
    let n0 = r0.len();
    if n0 < 4 {
        return r0.to_vec();
    }
    let turn = |r: &[P], i: usize| {
        let n = r.len();
        let (p, q) = (sub(r[i], r[(i + n - 1) % n]), sub(r[(i + 1) % n], r[i]));
        cross(p, q).atan2(dot(p, q)).abs()
    };
    // the fixed points on the ring become vertices, kept with the corners
    let mut r: Vec<P> = vec![];
    let mut keep: Vec<usize> = vec![];
    for i in 0..n0 {
        let (a, b) = (r0[i], r0[(i + 1) % n0]);
        if turn(r0, i) > CURVE_TURN {
            keep.push(r.len());
        }
        r.push(a);
        let mut on: Vec<(f64, P)> = fixed
            .iter()
            .filter(|&&q| crate::check::dist_to_seg(q, a, b) < 1e-6)
            .map(|&q| (dist(a, q), q))
            .collect();
        on.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (t, q) in on {
            if t < 1e-6 {
                if !keep.contains(&(r.len() - 1)) {
                    keep.push(r.len() - 1);
                }
            } else if dist(q, b) > 1e-6 && dist(q, r[r.len() - 1]) > 1e-6 {
                keep.push(r.len());
                r.push(q);
            }
        }
    }
    let n = r.len();
    if keep.len() < 2 {
        // a smooth loop: anchor it at a vertex and the one farthest from it
        let k0 = keep.first().copied().unwrap_or(0);
        let far = (0..n).max_by(|&a, &b| dist(r[k0], r[a]).total_cmp(&dist(r[k0], r[b]))).unwrap_or((k0 + n / 2) % n);
        keep = vec![k0.min(far), k0.max(far)];
    }
    let mut out = vec![];
    for (j, &a) in keep.iter().enumerate() {
        let b = keep[(j + 1) % keep.len()];
        let run: Vec<P> = (0..=(b + n - a - 1) % n + 1).map(|k| r[(a + k) % n]).collect();
        let mut part = vec![run[0]];
        douglas(&run, &mut part);
        part.pop();
        out.extend(part);
    }
    if out.len() < 3 || (footprint::signed_area(&out).abs() - footprint::signed_area(&r).abs()).abs() > 0.05 * footprint::signed_area(&r).abs() {
        return r0.to_vec();
    }
    out
}

/// Douglas-Peucker on an open run: pushes the kept points after the first, the last included.
fn douglas(run: &[P], out: &mut Vec<P>) {
    let (a, b) = (run[0], run[run.len() - 1]);
    let far = (1..run.len().saturating_sub(1))
        .map(|k| (k, crate::check::dist_to_seg(run[k], a, b)))
        .max_by(|x, y| x.1.total_cmp(&y.1));
    match far {
        Some((k, dv)) if dv > CHORD_SAG => {
            douglas(&run[..=k], out);
            douglas(&run[k..], out);
        }
        _ => out.push(b),
    }
}

fn ring_segs(r: &[P], out: &mut Vec<(P, P)>) {
    let n = r.len();
    for i in 0..n {
        out.push((r[i], r[(i + 1) % n]));
    }
}

/// A split line through `c` along `dir`, moved off the vertices of `segs` where it can.
fn split_line(c: P, dirs: &[P], segs: &[(P, P)], reach: f64) -> (P, P) {
    let mut best = (f64::MIN, (c, dirs[0]));
    for &dir in dirs {
        let nn = (-dir.1, dir.0);
        for k in [0.0, 0.4, -0.4, 0.8, -0.8, 1.2, -1.2, 1.6, -1.6] {
            let o = (c.0 + nn.0 * k, c.1 + nn.1 * k);
            let clear = segs
                .iter()
                .flat_map(|s| [s.0, s.1])
                .filter(|q| dist(*q, o) < reach)
                .map(|q| cross(dir, sub(q, o)).abs())
                .fold(f64::MAX, f64::min);
            let score = clear.min(0.6) - 0.05 * k.abs();
            if score > best.0 + 1e-9 {
                best = (score, (o, dir));
            }
        }
    }
    best.1
}

struct Chain {
    a: P,
    b: P,
    l: (Option<Lab>, Option<Lab>),
}

impl Chain {
    fn len(&self) -> f64 {
        dist(self.a, self.b)
    }
    fn has(&self, x: Option<Lab>, y: Option<Lab>) -> bool {
        self.l == (x, y) || self.l == (y, x)
    }
}

/// Straight runs of wall between the same two labels with no wall meeting them midway.
fn chains(ar: &Arrangement, labels: &[Option<Lab>]) -> Vec<Chain> {
    let lab = |k: usize| labels[ar.face_of[k]];
    let ne = ar.he.len() / 2;
    let wall: Vec<bool> = (0..ne).map(|e| lab(2 * e) != lab(2 * e + 1)).collect();
    let mut at: Vec<Vec<usize>> = vec![vec![]; ar.v.len()];
    for e in (0..ne).filter(|&e| wall[e]) {
        let (u, v) = ar.he[2 * e];
        at[u].push(e);
        at[v].push(e);
    }
    let key = |e: usize| {
        let (a, b) = (lab(2 * e), lab(2 * e + 1));
        (a, b)
    };
    let same = |e: usize, f: usize| {
        let (a, b) = key(e);
        let (c, d) = key(f);
        (a, b) == (c, d) || (a, b) == (d, c)
    };
    let dir = |e: usize| {
        let (u, v) = ar.he[2 * e];
        let d = sub(ar.v[v], ar.v[u]);
        let l = d.0.hypot(d.1);
        (d.0 / l, d.1 / l)
    };
    let mut done = vec![false; ne];
    let mut out = vec![];
    for e in 0..ne {
        if !wall[e] || done[e] {
            continue;
        }
        done[e] = true;
        let (u, v) = ar.he[2 * e];
        let mut ends = [u, v];
        for (side, start) in [(0usize, u), (1, v)] {
            let (mut cur, mut vx) = (e, start);
            loop {
                if at[vx].len() != 2 {
                    break;
                }
                let f = if at[vx][0] == cur { at[vx][1] } else { at[vx][0] };
                if done[f] || !same(cur, f) || cross(dir(cur), dir(f)).abs() > 1e-9 {
                    break;
                }
                done[f] = true;
                let (a, b) = ar.he[2 * f];
                vx = if a == vx { b } else { a };
                cur = f;
            }
            ends[side] = vx;
        }
        out.push(Chain { a: ar.v[ends[0]], b: ar.v[ends[1]], l: key(e) });
    }
    out
}

fn wall_of(walls: &[GWall], a: P, b: P) -> Option<usize> {
    walls
        .iter()
        .position(|w| crate::check::dist_to_seg(a, w.a, w.b) < 1e-6 && crate::check::dist_to_seg(b, w.a, w.b) < 1e-6)
}

fn put(l: &mut GLayout, c: &Chain, center: f64, width: f64, door: bool, win: WinSize) -> Option<()> {
    let wi = wall_of(&l.walls, c.a, c.b)?;
    let w = &l.walls[wi];
    let t = center / c.len();
    let m = (c.a.0 + (c.b.0 - c.a.0) * t, c.a.1 + (c.b.1 - c.a.1) * t);
    let mut along = dist(w.a, m) - width / 2.0;
    if !door {
        // a window keeps off the mitre of an acute corner of the outline
        let (c0, c1) = crate::check::miter_clear(&l.walls, wi);
        let room = l.walls[wi].length() - c1 - width;
        if room >= c0 {
            let to = along.clamp(c0, room);
            // the shift must not run onto a neighbour on the same wall
            let free = l.openings.iter().filter(|o| o.wall == wi).all(|o| to + width + 0.1 <= o.along || o.along + o.width + 0.1 <= to);
            if free {
                along = to;
            }
        }
    }
    // an opening never runs past its wall: the chain may stand wider than a short facet
    let (mut width, len) = (width, l.walls[wi].length());
    let room = len - 2.0 * crate::check::OPEN_END;
    if width > room {
        width = room;
        if width < if door { 0.7 } else { 0.5 } {
            return None;
        }
    }
    along = along.clamp(crate::check::OPEN_END, len - crate::check::OPEN_END - width);
    // the clamp may have run it onto a neighbour: take the nearest free slot on the chain's own
    // stretch of the wall (the wall may run on past other rooms), or drop it
    let (ta, tb) = (dist(w.a, c.a), dist(w.a, c.b));
    along = free_slot(l, wi, along, width, (ta.min(tb), ta.max(tb)))?;
    let (height, sill) = if door { (DOOR_H, 0.0) } else { (win.h, win.sill) };
    l.openings.push(GOpening { wall: wi, along, door, width, height, sill, ty: None });
    Some(())
}

/// The start nearest `along` where an opening `width` wide on wall `wi` stays within `span`
/// (positions along the wall) and keeps the end margin and the gap to every opening there.
fn free_slot(l: &GLayout, wi: usize, along: f64, width: f64, span: (f64, f64)) -> Option<f64> {
    use crate::check::{OPEN_END, OPEN_GAP};
    let (lo, hi) = (span.0.max(0.0) + OPEN_END, span.1.min(l.walls[wi].length()) - OPEN_END - width);
    let near: Vec<(f64, f64)> = l.openings.iter().filter(|o| o.wall == wi).map(|o| (o.along, o.along + o.width)).collect();
    let ok = |t: f64| t >= lo - 1e-9 && t <= hi + 1e-9 && near.iter().all(|&(a, b)| t + width + OPEN_GAP <= a + 1e-9 || b + OPEN_GAP <= t + 1e-9);
    [along, along.clamp(lo, hi.max(lo))]
        .into_iter()
        .chain(near.iter().flat_map(|&(a, b)| [b + OPEN_GAP, a - OPEN_GAP - width]))
        .filter(|&t| ok(t))
        .min_by(|a, b| (a - along).abs().total_cmp(&(b - along).abs()))
}

/// A door on the longest run between `x` and one of `ys`, a run `front` accepts first.
fn door(
    l: &mut GLayout,
    cs: &[Chain],
    x: Option<Lab>,
    ys: &[Option<Lab>],
    width: f64,
    win: WinSize,
    front: &dyn Fn(&Chain) -> bool,
) -> bool {
    let fit = |c: &&Chain| c.len() >= 0.8 + 2.0 * DOOR_CLEAR;
    let mut ok: Vec<&Chain> = cs.iter().filter(|c| ys.iter().any(|&y| c.has(x, y))).collect();
    ok.sort_by(|a, b| (front(b) && fit(b)).cmp(&(front(a) && fit(a))).then(b.len().total_cmp(&a.len())));
    // the best run may be full of openings already, or not one wall: the next run hosts it
    let mut tried = false;
    for c in ok {
        if tried && !front(c) {
            break;
        }
        tried = true;
        let placed = if c.len() >= width + 2.0 * DOOR_CLEAR {
            put(l, c, c.len() / 2.0, width, true, win)
        } else if c.len() >= 0.8 + 2.0 * DOOR_CLEAR {
            put(l, c, c.len() / 2.0, c.len() - 2.0 * DOOR_CLEAR - 0.05, true, win)
        } else {
            None
        };
        if placed.is_some() {
            return true;
        }
    }
    false
}

/// Windows on the facade runs of a habitable room by the one-eighth daylight rule.
fn windows(l: &mut GLayout, cs: &[Chain], x: Lab, area: f64, win: WinSize) -> bool {
    let want = area / 8.0;
    let mut runs: Vec<&Chain> = cs.iter().filter(|c| c.has(Some(x), None)).collect();
    runs.sort_by(|a, b| b.len().total_cmp(&a.len()));
    let mut glazed = 0.0;
    for c in runs {
        if glazed >= want - 1e-9 {
            break;
        }
        let len = c.len();
        let mut ww = if len < win.w + 2.0 * WIN_CLEAR { 0.9 } else { win.w };
        let fit = ((len - 2.0 * WIN_CLEAR + WIN_GAP) / (ww + WIN_GAP)).floor() as i64;
        if fit < 1 {
            continue;
        }
        let need = ((want - glazed) / (ww * win.h)).ceil().max(1.0) as i64;
        let k = need.min(fit);
        if k < need {
            let room = (len - 2.0 * WIN_CLEAR - (k - 1) as f64 * WIN_GAP) / k as f64;
            let wide = ((want - glazed) / (k as f64 * win.h) / 0.05).ceil() * 0.05;
            ww = wide.min((room / 0.05).floor() * 0.05).min(MAX_WIN_W).max(ww);
        }
        // a window the wall cannot take (a mitre, a door already there) leaves the others: the
        // next run makes up the glazing
        for t in 0..k {
            let center = len * (2 * t + 1) as f64 / (2 * k) as f64;
            if put(l, c, center, ww, false, win).is_some() {
                glazed += ww * win.h;
            }
        }
    }
    if glazed > 0.0 {
        return true;
    }
    // facets of a curve too short for a standard window: one narrow window on the longest
    let Some(c) = cs.iter().filter(|c| c.has(Some(x), None)).max_by(|a, b| a.len().total_cmp(&b.len())) else {
        return false;
    };
    let ww = ((c.len() - 2.0 * NARROW_CLEAR).min(0.9) / 0.05).floor() * 0.05;
    ww >= 0.6 - 1e-9 && put(l, c, c.len() / 2.0, ww, false, win).is_some()
}

/// A point well inside a room outline, for its label.
fn label_point(ring: &[P], probes: &[P]) -> P {
    let clear = |p: P| {
        let n = ring.len();
        (0..n).map(|i| crate::check::dist_to_seg(p, ring[i], ring[(i + 1) % n])).fold(f64::MAX, f64::min)
    };
    let c = footprint::centroid(ring);
    std::iter::once(c)
        .chain(probes.iter().copied())
        .filter(|p| crate::check::inside(ring, *p))
        .max_by(|a, b| clear(*a).total_cmp(&clear(*b)))
        .unwrap_or(c)
}

/// Pieces of `r` on either side of the line through `o` along `dir`.
fn halves(r: &Region, o: P, dir: P) -> Vec<Region> {
    let ((x0, y0), (x1, y1)) = footprint::bbox(&r.outer);
    let big = 4.0 * ((x1 - x0).hypot(y1 - y0) + 10.0);
    let nn = (-dir.1, dir.0);
    let side = |s: f64| {
        let f = Frame { o: (o.0 - dir.0 * big + nn.0 * s.min(0.0) * big, o.1 - dir.1 * big + nn.1 * s.min(0.0) * big), u: dir };
        to_v(&f.rect(0.0, 2.0 * big, 0.0, big))
    };
    let mut out = vec![];
    for s in [1.0, -1.0] {
        if let Some(ps) = ddc_geom::poly::subtract(&rings(r), &[side(s)]) {
            for (outer, holes) in ps {
                if let Some(outer) = footprint::clean(&to_p(&outer), true) {
                    let holes = holes.iter().filter_map(|h| footprint::clean(&to_p(h), false)).collect();
                    out.push(Region { outer, holes });
                }
            }
        }
    }
    out
}

/// The pieces of `r` a disc of radius `w` sweeps (inset, then outset), each kept only when it
/// stays inside `r`.
fn opened(r: &Region, w: f64) -> Vec<Region> {
    let near = |q: P| {
        crate::check::inside(&r.outer, q) || {
            let n = r.outer.len();
            (0..n).any(|i| crate::check::dist_to_seg(q, r.outer[i], r.outer[(i + 1) % n]) < 1e-6)
        }
    };
    offset::inset(r, w)
        .iter()
        .filter(|z| z.holes.is_empty())
        .filter_map(|z| {
            let o = offset::outset(z, w);
            let o = footprint::clean(&o.outer, true)?;
            Some(Region { outer: if o.iter().all(|&q| near(q)) { o } else { r.outer.clone() }, holes: vec![] })
        })
        .collect()
}

/// Outline of the union of two rings: the hole the pair cuts from a large square.
fn union_outer(a: &[P], b: &[P]) -> Vec<P> {
    let ((x0, y0), (x1, y1)) = footprint::bbox(&a.iter().chain(b).copied().collect::<Vec<_>>());
    let m = 10.0 + (x1 - x0).max(y1 - y0);
    let big = vec![v2(x0 - m, y0 - m), v2(x1 + m, y0 - m), v2(x1 + m, y1 + m), v2(x0 - m, y1 + m)];
    ddc_geom::poly::subtract(&[big], &[to_v(a), to_v(b)])
        .into_iter()
        .flatten()
        .flat_map(|(_, holes)| holes)
        .map(|h| to_p(&h))
        .filter_map(|h| footprint::clean(&h, true))
        .max_by(|p, q| footprint::signed_area(p).total_cmp(&footprint::signed_area(q)))
        .unwrap_or_else(|| a.to_vec())
}

fn win_of(t: Typology) -> WinSize {
    match t {
        Typology::House | Typology::ApartmentSection => res_win(),
        Typology::Office | Typology::School => super::build::work_win(),
    }
}

fn role(a: Option<Lab>, b: Option<Lab>, units: &[Unit]) -> Option<WallRole> {
    let (Some(a), Some(b)) = (a, b) else { return Some(WallRole::Exterior) };
    let unit = |l: Lab| match l {
        Lab::Room(i) => units.get(i),
        _ => None,
    };
    let core = |l: Lab| match l {
        Lab::Corr(_) | Lab::Lost => true,
        Lab::Room(_) => unit(l).is_some_and(|u| matches!(u.kind, RoomKind::Stair | RoomKind::Lift | RoomKind::Corridor)),
    };
    if core(a) || core(b) {
        return Some(WallRole::Corridor);
    }
    let owner = |l: Lab| unit(l).map(|u| u.owner);
    match (owner(a), owner(b)) {
        (Some(x), Some(y)) if x != y => Some(WallRole::Party),
        _ => Some(WallRole::Partition),
    }
}

/// Everything a storey needs beside its floor plan.
struct Shared<'a> {
    spec: &'a ProjectSpec,
    core: Option<&'a Core>,
    d: f64,
    df: f64,
    win: WinSize,
    dirs: Vec<P>,
}

/// Whether `region` holds room bands `d` deep round its facades and a corridor `c` wide
/// inside them, in one piece.
fn two_bands(region: &Region, d: f64, c: f64) -> bool {
    offset::inset(region, d).len() == 1 && !offset::inset(region, d + c / 2.0 - CLOSE).is_empty()
}

/// The band depth of a plate: the brief's, or for a plate too thin for two such bands and the
/// corridor on some floor, bands narrowed step by step down to the rule's least depth. A ring
/// of bands lines every facade, so a thinner plate has no single-loaded form here: it is refused.
fn plate_band(spec: &ProjectSpec, regions: &[Region]) -> Result<f64, GenError> {
    let (d0, c) = (spec.band_depth, spec.corridor);
    let lo = super::rules::def("depth.band").map_or(3.0, |r| r.min);
    let holds = |d: f64| regions.iter().all(|r| two_bands(r, d, c));
    let mut d = d0;
    while d >= lo - 1e-9 {
        if holds(d) {
            return Ok(d);
        }
        d = ((d - NARROW_STEP) * 100.0).round() / 100.0;
    }
    Err(infeasible(format!(
        "the plate is too thin for two {d0} m room bands and a {c} m corridor, and for bands narrowed \
         to {lo} m: keep it at least {:.1} m across everywhere",
        2.0 * lo + c
    )))
}

/// A storey with door and corner runs kept at every cut; a program too long for that facade
/// takes the cuts at the bare margin before the storey is refused.
fn storey(sh: &Shared, k: usize, region: &Region, slab: Vec<P>, next_apt: &mut u32) -> Result<PStorey, GenError> {
    let first = *next_apt;
    let mut r = storey_with(sh, k, region, slab.clone(), next_apt, 0);
    for tight in 1..=2 {
        let Err(e) = r else { break };
        *next_apt = first;
        r = storey_with(sh, k, region, slab.clone(), next_apt, tight).map_err(|_| e);
    }
    r
}

fn storey_with(sh: &Shared, k: usize, region: &Region, slab: Vec<P>, next_apt: &mut u32, tight: u8) -> Result<PStorey, GenError> {
    let spec = sh.spec;
    let (d, df, c) = (sh.d, sh.df, spec.corridor);
    let fl = k + 1;
    let id = offset::inset(region, d);
    if !two_bands(region, d, c) {
        return Err(infeasible(format!(
            "floor {fl}: the plate is too thin for two {d} m room bands and a {c} m corridor: \
             keep it at least {:.1} m across everywhere, or lower band_depth",
            2.0 * d + c
        )));
    }
    let rbs = offset::bands(region, d).map_err(|e| infeasible(format!("floor {fl}: {e}")))?;
    let core = sh.core.filter(|_| spec.has_stair());
    let mut plan = plan_floor_with(spec, k, &rbs, d, df, core, next_apt, tight)?;
    // the central core's cells
    let central = core.filter(|c| c.band.is_none());
    let core_first = plan.units.len();
    if let Some(c) = central {
        for &(_, _, kind) in &c.cells {
            plan.add(public(kind, names::kind_name(kind, spec.ru).to_string(), None));
        }
    }
    // store rooms in the dark interior, reached from the corridor round them
    let mut stores = vec![];
    for z in offset::inset(region, d + c) {
        let mut cuts = vec![];
        if let Some(c) = central {
            let reach = 4.0 * (footprint::bbox(&region.outer).1 .0 - footprint::bbox(&region.outer).0 .0).abs() + 100.0;
            cuts.push(to_v(&c.f.rect(0.0, c.w, 0.0, c.d)));
            cuts.push(to_v(&c.f.rect(0.0, c.w, c.d - 1e-9, c.d + reach)));
        }
        let ps = if cuts.is_empty() {
            vec![(to_v(&z.outer), z.holes.iter().map(|h| to_v(h)).collect::<Vec<_>>())]
        } else {
            ddc_geom::poly::subtract(&rings(&z), &cuts).unwrap_or_default()
        };
        for (outer, holes) in ps {
            let Some(outer) = footprint::clean(&to_p(&outer), true) else { continue };
            if !holes.is_empty() {
                continue;
            }
            // slivers thinner than a store, left where the core nearly meets the corridor line,
            // stay corridor instead of making walls a few centimetres long
            for r in opened(&Region { outer, holes: vec![] }, STORE_W / 2.0) {
                if footprint::region_area(&r) < STORE_AREA {
                    continue;
                }
                let u = plan.add(public(RoomKind::Storage, names::kind_name(RoomKind::Storage, spec.ru).to_string(), None));
                stores.push((chord_ring(&r.outer, &[]), u));
            }
        }
    }
    let mut cuts: Vec<(usize, f64, f64, f64)> = vec![];
    for z in &plan.zones {
        let (h0, h1) = match z.strip {
            Strip::Full => (0.0, d),
            Strip::Front => (0.0, df),
            Strip::Back => (df, d),
        };
        let per = rbs[z.ring].perimeter;
        for t in [z.t0, z.t1] {
            let t = t.rem_euclid(per);
            match cuts.iter_mut().find(|c| c.0 == z.ring && ((c.1 - t).abs() < 1e-9 || (per - (c.1 - t).abs()) < 1e-9)) {
                Some(c) => {
                    c.2 = c.2.min(h0);
                    c.3 = c.3.max(h1);
                }
                None => cuts.push((z.ring, t, h0, h1)),
            }
        }
    }
    // the inner band lines run straight between the cuts that meet them
    let at_depth = |h: f64| -> Vec<P> {
        cuts.iter()
            .filter(|c| c.2 <= h + 1e-9 && c.3 >= h - 1e-9)
            .map(|&(ring, t, _, _)| {
                let (e, s) = rbs[ring].locate(t);
                rbs[ring].edges[e].at(s, h)
            })
            .collect()
    };
    let id: Vec<Region> = id.iter().map(|z| chorded(z, &at_depth(d))).collect();
    // the lines of every zone
    let mut segs: Vec<(P, P)> = vec![];
    for r in std::iter::once(&region.outer).chain(&region.holes) {
        ring_segs(r, &mut segs);
    }
    for z in &id {
        for r in std::iter::once(&z.outer).chain(&z.holes) {
            ring_segs(r, &mut segs);
        }
    }
    let ifront = if plan.zones.iter().any(|z| z.strip != Strip::Full) {
        offset::inset(region, df).iter().map(|z| chorded(z, &at_depth(df))).collect()
    } else {
        vec![]
    };
    for z in &ifront {
        for r in std::iter::once(&z.outer).chain(&z.holes) {
            ring_segs(r, &mut segs);
        }
    }
    for rb in &rbs {
        for e in &rb.edges {
            ring_segs(&e.face, &mut segs);
        }
    }
    for &(ring, t, h0, h1) in &cuts {
        let (e, s) = rbs[ring].locate(t);
        let eb = &rbs[ring].edges[e];
        segs.push((eb.at(s, h0), eb.at(s, h1)));
    }
    // a band core on a bowed chord: its cell walls square to the chord, facade to band edge
    if let Some(c) = core.filter(|c| c.band.is_some() && c.sag > 0.0) {
        for cl in &c.cells[1..] {
            segs.push((c.f.at(cl.0, -c.sag - 0.05), c.f.at(cl.0, d)));
        }
    }
    if let Some(c) = central {
        ring_segs(&c.f.rect(0.0, c.w, 0.0, c.d), &mut segs);
        for cl in &c.cells[1..] {
            segs.push((c.f.at(cl.0, 0.0), c.f.at(cl.0, c.d)));
        }
    }
    for (r, _) in &stores {
        ring_segs(r, &mut segs);
    }
    let ((bx0, by0), (bx1, by1)) = footprint::bbox(&region.outer);
    let reach = (bx1 - bx0).hypot(by1 - by0) + 10.0;
    let line = |o: P, dir: P| ((o.0 - dir.0 * reach, o.1 - dir.1 * reach), (o.0 + dir.0 * reach, o.1 + dir.1 * reach));
    // the first splits tie every closed loop of lines to the rest: through the core and
    // through every courtyard
    let mut splits = vec![];
    if let Some(c) = central {
        let mid = c.f.at(c.w / 2.0, c.d / 2.0);
        splits.push(split_line(mid, &[c.f.u, c.f.n()], &segs, reach));
    }
    for hole in &region.holes {
        splits.push(split_line(footprint::centroid(hole), &sh.dirs, &segs, reach));
    }
    let labs = |splits: &[(P, P)]| {
        let mut all = segs.clone();
        for &(o, dir) in splits {
            all.push(line(o, dir));
        }
        let ar = Arrangement::new(&all);
        let seat = core.filter(|c| c.band.is_some());
        let fl = Floor { region, id: id.clone(), ifront: ifront.clone(), rbs: &rbs, plan: &plan, core: central.map(|c| (c, core_first)), seat, stores: stores.clone(), splits: splits.to_vec() };
        let labels = ar.label(|p| fl.cls(p));
        (ar, labels)
    };
    let (mut ar, mut labels) = labs(&splits);
    // a corridor round an island is split until every corridor room is a simple polygon
    for _ in 0..6 {
        let mut corrs: Vec<u32> = labels.iter().filter_map(|l| match l {
            Some(Lab::Corr(b)) => Some(*b),
            _ => None,
        }).collect();
        corrs.sort_unstable();
        corrs.dedup();
        let island = corrs.iter().find_map(|&b| ar.region(&labels, Lab::Corr(b)).1.into_iter().next());
        let Some(hole) = island else { break };
        if splits.len() >= 30 {
            break;
        }
        splits.push(split_line(footprint::centroid(&hole), &sh.dirs, &segs, reach));
        (ar, labels) = labs(&splits);
    }
    finish(sh, k, region, slab, &plan.units, core, &ar, &labels)
}

/// Rooms, walls, doors, windows, slab and core of a labelled arrangement of floor `k`: the
/// rooms of `units` in order, then the corridors.
#[allow(clippy::too_many_arguments)]
fn finish(
    sh: &Shared,
    k: usize,
    region: &Region,
    slab: Vec<P>,
    units: &[Unit],
    core: Option<&Core>,
    ar: &Arrangement,
    labels: &[Option<Lab>],
) -> Result<PStorey, GenError> {
    let spec = sh.spec;
    let n = spec.floors as usize;
    let h = spec.storey_height(k);
    let fl = k + 1;
    if labels.contains(&Some(Lab::Lost)) {
        return Err(infeasible(format!("floor {fl}: a piece of the facade band belongs to no room")));
    }
    // rooms: the program in order, then the corridors
    let mut l = GLayout { height: h, ..GLayout::default() };
    let mut rooms: Vec<PRoom> = vec![];
    let mut labs_of: Vec<Lab> = vec![];
    let probes_of = |lab: Lab| -> Vec<P> {
        (0..ar.faces.len()).filter(|&f| labels[f] == Some(lab)).map(|f| ar.probe(f)).collect()
    };
    let mut public_no = 0;
    let mut per_apt: std::collections::BTreeMap<u32, u32> = Default::default();
    let mut number = |owner: PRole| match owner {
        PRole::Public => {
            public_no += 1;
            format!("{}{:02}", fl, public_no)
        }
        PRole::Apartment(a) => {
            let k = per_apt.entry(a).or_insert(0);
            *k += 1;
            format!("{a}.{k}")
        }
    };
    let mut stair_room = None;
    for (i, u) in units.iter().enumerate() {
        let (outs, holes) = ar.region(&labels, Lab::Room(i));
        if outs.len() != 1 || !holes.is_empty() {
            return Err(infeasible(format!(
                "floor {fl}: room {} came out in {} pieces: the plate is too ragged here",
                u.name,
                outs.len() + holes.len()
            )));
        }
        let ring = outs[0].clone();
        if u.kind == RoomKind::Stair {
            stair_room = Some(rooms.len());
        }
        let apartment = match u.owner {
            PRole::Apartment(a) => Some(a),
            PRole::Public => None,
        };
        l.rooms.push(GRoom { name: u.name.clone(), apartment, point: label_point(&ring, &probes_of(Lab::Room(i))), polygon: ring });
        rooms.push(PRoom { kind: u.kind, number: number(u.owner), owner: u.owner, asked: u.asked, grown: u.grown });
        labs_of.push(Lab::Room(i));
    }
    let mut corrs: Vec<u32> = labels.iter().filter_map(|l| match l {
        Some(Lab::Corr(b)) => Some(*b),
        _ => None,
    }).collect();
    corrs.sort_unstable();
    corrs.dedup();
    let corr_name = names::kind_name(RoomKind::Corridor, spec.ru).to_string();
    for &b in &corrs {
        let (outs, holes) = ar.region(&labels, Lab::Corr(b));
        if !holes.is_empty() {
            return Err(infeasible(format!("floor {fl}: the corridor rings an island it cannot split")));
        }
        if outs.len() != 1 {
            return Err(infeasible(format!("floor {fl}: a corridor came out in {} pieces", outs.len())));
        }
        let ring = outs[0].clone();
        l.rooms.push(GRoom { name: corr_name.clone(), apartment: None, point: label_point(&ring, &probes_of(Lab::Corr(b))), polygon: ring });
        rooms.push(PRoom { kind: RoomKind::Corridor, number: number(PRole::Public), owner: PRole::Public, asked: None, grown: false });
        labs_of.push(Lab::Corr(b));
    }
    // walls
    for (a, b, r) in ar.walls(&labels, |x, y| role(x, y, units)) {
        l.walls.push(GWall::line(a, b, r));
    }
    // doors and windows
    let cs = chains(&ar, &labels);
    let corr_labs: Vec<Option<Lab>> = corrs.iter().map(|&b| Some(Lab::Corr(b))).collect();
    let hall_of = |a: u32| units.iter().position(|u| u.owner == PRole::Apartment(a) && u.kind == RoomKind::Hall);
    let win = sh.win;
    // the entrance: every lobby the builder added, else the first lobby the brief asked for
    let lobby = |u: &Unit| u.kind == RoomKind::Lobby;
    let asked_entrance = if units.iter().any(|u| lobby(u) && u.asked.is_none()) { None } else { units.iter().position(lobby) };
    for (i, u) in units.iter().enumerate() {
        let me = Some(Lab::Room(i));
        let width = door_width(u.kind);
        let to: Vec<Option<Lab>> = match u.owner {
            PRole::Apartment(a) if u.kind != RoomKind::Hall => match hall_of(a) {
                Some(hh) => vec![Some(Lab::Room(hh))],
                None => corr_labs.clone(),
            },
            _ => corr_labs.clone(),
        };
        // stair and lift open on the core front, clear of the flights: in the band the front is
        // the band's inner edge, at or past the core's depth behind a bowed chord
        let front = |ch: &Chain| match core {
            Some(c) if matches!(u.kind, RoomKind::Stair | RoomKind::Lift) => {
                let on = |q: P| {
                    let y = c.f.local(q).1;
                    (y - c.d).abs() < 1e-6 || (c.band.is_some() && y > c.d && y <= c.d + c.sag + 2e-3)
                };
                on(ch.a) && on(ch.b)
            }
            _ => true,
        };
        // a public room cut off from the corridor opens onto the lobby or hall beside it, one
        // that itself reaches the corridor or, on the ground floor, the street
        let reaches = |j: usize| cs.iter().any(|c| corr_labs.iter().any(|&y| c.has(Some(Lab::Room(j)), y)) || (k == 0 && c.has(Some(Lab::Room(j)), None)));
        let via: Vec<Option<Lab>> = if u.owner == PRole::Public && !matches!(u.kind, RoomKind::Stair | RoomKind::Lift) {
            units.iter().enumerate().filter(|(j, v)| *j != i && v.owner == PRole::Public && v.kind.circulation() && reaches(*j)).map(|(j, _)| Some(Lab::Room(j))).collect()
        } else {
            vec![]
        };
        if !door(&mut l, &cs, me, &to, width, win, &front) && (via.is_empty() || !door(&mut l, &cs, me, &via, width, win, &front)) {
            // the why names the runs the room shares with its way out against what a door takes
            let runs: Vec<String> = cs
                .iter()
                .filter(|c| to.iter().chain(&via).any(|&y| c.has(me, y)))
                .map(|c| {
                    let walled = if wall_of(&l.walls, c.a, c.b).is_some() { "" } else { " unwalled" };
                    format!("{:.2}{}{}", c.len(), walled, if front(c) { "" } else { " off-front" })
                })
                .collect();
            return Err(infeasible(format!(
                "floor {fl}: room {} has no wall long enough for a door to its {} (shared runs [{}] m, a door takes {:.2} m)",
                u.name,
                if to == corr_labs { "corridor" } else { "hall" },
                runs.join(", "),
                0.8 + 2.0 * DOOR_CLEAR
            )));
        }
        if k == 0 && u.kind == RoomKind::Lobby && u.asked.is_none() && !door(&mut l, &cs, me, &[None], 1.2, win, &|_| true) {
            return Err(infeasible(format!("floor {fl}: the entrance has no facade for its door")));
        }
        // an asked lobby standing in for the entrance takes its outer door where it has a facade
        if k == 0 && asked_entrance == Some(i) {
            door(&mut l, &cs, me, &[None], 1.2, win, &|_| true);
        }
        // a stair on the facade opens outside on the ground floor: its own way out, not the lobby's
        if k == 0 && u.kind == RoomKind::Stair && cs.iter().any(|c| c.has(me, None)) && !door(&mut l, &cs, me, &[None], 1.2, win, &|_| true) {
            return Err(infeasible(format!("floor {fl}: stair {} on the facade has no wall long enough for its outside door", u.name)));
        }
        if u.kind.habitable() {
            let ri = labs_of.iter().position(|x| *x == Lab::Room(i)).unwrap_or(0);
            let area = footprint::signed_area(&l.rooms[ri].polygon).abs();
            if !windows(&mut l, &cs, Lab::Room(i), area, win) {
                return Err(infeasible(format!("floor {fl}: habitable room {} has no facade for a window", u.name)));
            }
        }
    }
    // corridor pieces open into each other
    for (i, &a) in corrs.iter().enumerate() {
        for &b in &corrs[i + 1..] {
            let touch = cs.iter().any(|ch| ch.has(Some(Lab::Corr(a)), Some(Lab::Corr(b))));
            if touch && !door(&mut l, &cs, Some(Lab::Corr(a)), &[Some(Lab::Corr(b))], 1.2, win, &|_| true) {
                return Err(infeasible(format!("floor {fl}: two corridor parts share no wall long enough for a door")));
            }
        }
    }
    // slab, stair, shaft
    l.slab = slab;
    for hole in &region.holes {
        if hole.iter().all(|p| crate::check::inside(&l.slab, *p)) {
            l.slab_holes.push(hole.clone());
        }
    }
    if let Some(c) = core {
        if k > 0 {
            l.slab_holes.push(c.hole.clone());
            l.slab_holes.extend(c.stair2.iter().map(|s| s.1.clone()));
            l.slab_holes.extend(c.end.iter().map(|e| e.hole.clone()));
        }
        if k + 1 < n || super::fire::stairs_to_roof(spec) {
            // each storey's stairs rise its own height
            let own = |s: &crate::GStair| crate::GStair { height: h, ..s.clone() };
            l.stairs.push(own(&c.stair));
            l.stairs.extend(c.stair2.iter().map(|s| own(&s.0)));
            l.stairs.extend(c.end.iter().map(|e| own(&e.stair)));
        }
        for s in &c.shafts {
            l.shafts.push(GShaft::new(s.clone(), crate::ShaftKind::Lift));
        }
    }
    let caps = [
        (l.walls.len(), slots::OPENINGS - slots::WALLS, "walls"),
        (l.openings.len(), slots::ROOMS - slots::OPENINGS, "openings"),
        (l.rooms.len(), slots::SLAB - slots::ROOMS, "rooms"),
    ];
    for (have, cap, what) in caps {
        if have > cap as usize {
            return Err(infeasible(format!("{have} {what} on floor {fl}; at most {cap}: split the building")));
        }
    }
    crate::check::validate_layout(&l).map_err(|e| infeasible(format!("floor {fl}: {e}")))?;
    Ok(PStorey {
        name: names::level_name(k, spec.ru),
        elevation: spec.storey_elevation(k),
        height: h,
        layout: l,
        rooms,
        stair_room,
        // fitted out in the grid frame once the grid is chosen (anygeo)
        fit: vec![],
        curtains: vec![],
        railings: vec![],
    })
}

/// Roofs of the top plate: one per courtyard-free piece; edges along a split are gables.
fn roofs(spec: &ProjectSpec, top: &Region, dirs: &[P]) -> Vec<PRoof> {
    let mut work = vec![top.clone()];
    let mut lines: Vec<(P, P)> = vec![];
    for _ in 0..32 {
        let Some(i) = work.iter().position(|r| !r.holes.is_empty()) else { break };
        let r = work.remove(i);
        let c = footprint::centroid(&r.holes[0]);
        let dir = dirs.first().copied().unwrap_or((1.0, 0.0));
        lines.push((c, dir));
        work.extend(halves(&r, c, dir));
    }
    let pitched = spec.roof == RoofKind::Pitched;
    let slope = if pitched { spec.roof_slope } else { super::FLAT_FALL_DEG };
    work.into_iter()
        .filter(|r| r.holes.is_empty())
        .map(|r| {
            let ring = r.outer;
            let n = ring.len();
            let on_split = |i: usize| {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                lines.iter().any(|(o, dir)| cross(*dir, sub(a, *o)).abs() < 1e-6 && cross(*dir, sub(b, *o)).abs() < 1e-6)
            };
            let mut edges = match (pitched, spec.roof_form) {
                (true, Some(form)) => footprint::edge_slopes(&ring, form, slope),
                (true, None) => vec![Some(slope); n],
                (false, _) => vec![],
            };
            if !edges.is_empty() {
                for (i, e) in edges.iter_mut().enumerate() {
                    if on_split(i) {
                        *e = None;
                    }
                }
            }
            PRoof {
                name: names::roof_name(spec.ru).to_string(),
                elevation: spec.building_height(),
                ring,
                slope,
                overhang: if pitched { rule("massing.roof_overhang") } else { 0.0 },
                edges,
                vault: None,
                lift: 0.0,
                eave: None,
            }
        })
        .collect()
}

/// Builds every storey of a plate brief and its roof, at the origin of the brief's plan.
pub(super) fn build(spec: &ProjectSpec) -> Result<GProject, GenError> {
    let plate = spec.plate.as_ref().ok_or_else(|| infeasible("no footprint polygon"))?;
    let n = spec.floors as usize;
    let regions: Vec<Region> = (0..n).map(|k| plate.at(k)).collect::<Result<_, _>>().map_err(infeasible)?;
    let d = plate_band(spec, &regions)?;
    let c = spec.corridor;
    let df = if spec.typology == Typology::ApartmentSection {
        let s = (d - 3.0).min(2.4);
        if s < 1.5 {
            return Err(infeasible(format!(
                "band_depth {d} m leaves no strip for the hall and bathrooms behind the rooms: at least 4.5 m"
            )));
        }
        d - s
    } else {
        d - (d - 3.0).clamp(0.0, 2.4)
    };
    // directions of the long facades, for the core and for corridor splits
    let base = &plate.base;
    let nb = base.outer.len();
    let mut ed: Vec<(f64, P)> = (0..nb)
        .map(|i| {
            let e = sub(base.outer[(i + 1) % nb], base.outer[i]);
            let l = e.0.hypot(e.1);
            (l, (e.0 / l, e.1 / l))
        })
        .collect();
    ed.sort_by(|a, b| b.0.total_cmp(&a.0));
    let u0 = ed.first().map_or((1.0, 0.0), |e| e.1);
    let dirs = vec![u0, (-u0.1, u0.0)];
    let core = if spec.has_stair() {
        let pair = core_pair(spec, area(&base.outer));
        let f0 = Frame { o: (0.0, 0.0), u: (1.0, 0.0) };
        let ((bx0, by0), (bx1, by1)) = footprint::bbox(&base.outer);
        let diag = (bx1 - bx0).hypot(by1 - by0);
        // a pair in one core stands one core width apart: too close for the exit separation, so
        // the second stair goes to the plate perimeter and the core moves to the far side
        let mut end: Option<End> = None;
        if pair && plate.uniform() && core_at(spec, f0, d, false)?.w < rule("egress.exit_separation") * diag {
            let rbs = offset::bands(base, d).map_err(infeasible)?;
            let lp = Lp::new(&rbs[0], d, df);
            let sw0 = end_stair(spec, f0, d)?.0;
            if let Some(span) = lp.spans[0].iter().copied().filter(|(a, b)| b - a >= sw0).max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0))) {
                let (e, s) = rbs[0].locate(span.0);
                let eb = &rbs[0].edges[e];
                let f = Frame { o: eb.at(s, 0.0), u: eb.u };
                let (sw, stair, hole) = end_stair(spec, f, d)?;
                end = Some(End { t0: span.0, t1: span.0 + sw, door: f.at(sw / 2.0, d), stair, hole });
            }
        }
        let pair_in = pair && end.is_none();
        let target = match &end {
            Some(e) => {
                let c0 = footprint::centroid(&base.outer);
                let v = sub(c0, e.door);
                let l = v.0.hypot(v.1).max(1e-9);
                let k = 0.65 * diag / l;
                (e.door.0 + v.0 * k, e.door.1 + v.1 * k)
            }
            None => footprint::centroid(&base.outer),
        };
        let zs: Vec<Vec<Region>> = regions.iter().map(|r| offset::inset(r, d + c)).collect();
        // a bank past one row's cars asks the interior core for the depth of two facing rows
        // and their lobby; one deep row where the interior has no room for it
        let facing = if lift_row_full(spec) { facing_depth(spec) as f64 / 1000.0 } else { 0.0 };
        let mut placed = None;
        for dc in [facing, d].into_iter().filter(|&x| x >= d) {
            let probe = core_at(spec, f0, dc, pair_in)?;
            if let Some(f) = central(base, &zs, probe.w, probe.d, target) {
                placed = Some((f, dc));
                break;
            }
        }
        match placed {
            Some((f, dc)) => {
                let mut core = core_at(spec, f, dc, pair_in)?;
                core.end = end;
                Some(core)
            }
            None if plate.uniform() => {
                let probe = core_at(spec, f0, d, pair)?;
                // the flattest stretch of the outer facade takes the core in its band
                let rbs = offset::bands(base, d).map_err(infeasible)?;
                let lp = Lp::new(&rbs[0], d, df);
                let core = band_seat(spec, base, &rbs[0], &lp, d, pair)?.ok_or_else(|| {
                    infeasible(format!(
                        "no stretch of facade holds the {:.1} m core and the interior is too small for it: \
                         give the plate a straight or gently curved side of at least {:.1} m or more depth",
                        probe.w,
                        probe.w + 2.0 * MARGIN + d
                    ))
                })?;
                Some(core)
            }
            None => {
                let probe = core_at(spec, f0, d, pair_in)?;
                return Err(infeasible(format!(
                    "the core ({:.1} x {:.1} m) fits no floor's interior: with per-floor turns, set-backs or \
                     offsets the plate needs at least {:.1} m across inside its room bands on every floor",
                    probe.w,
                    probe.d,
                    probe.w.max(probe.d) + 2.0 * (d + c)
                )))
            }
        }
    } else {
        None
    };
    let sh = Shared { spec, core: core.as_ref(), d, df, win: win_of(spec.typology), dirs: dirs.clone() };
    let mut next_apt = 0;
    let mut storeys = Vec::with_capacity(n);
    for (k, r) in regions.iter().enumerate() {
        let slab = if k == 0 || plate.uniform() {
            r.outer.clone()
        } else {
            union_outer(&regions[k - 1].outer, &r.outer)
        };
        storeys.push(storey(&sh, k, r, slab, &mut next_apt)?);
    }
    let mut rs = roofs(spec, &regions[n - 1], &dirs);
    if rs.is_empty() {
        return Err(infeasible("the top plate leaves no roof"));
    }
    let roof = rs.remove(0);
    let ((x0, y0), (x1, y1)) = footprint::bbox(&base.outer);
    let (w, dd) = (x1 - x0, y1 - y0);
    let gfa: f64 = regions.iter().map(footprint::region_area).sum();
    let rooms: usize = storeys.iter().map(|s| s.rooms.len()).sum();
    let metrics = vec![
        ("floors", n as f64),
        ("width", w),
        ("depth", dd),
        ("gross_floor_area", gfa),
        ("rooms", rooms as f64),
        ("apartments", f64::from(next_apt)),
    ];
    Ok(GProject {
        storeys,
        roof,
        extra_roofs: rs,
        copies: 1,
        court: (plate.uniform() && base.holes.len() == 1).then(|| base.holes[0].clone()),
        podium: None,
        footprint: (w, dd),
        apartments: next_apt,
        metrics,
        hvac: super::GHvac::default(),
        services: super::GServices::default(),
        basements: vec![],
        site: super::site::GSite::default(),
        fire: Default::default(),
        lifts: Default::default(),
        shell: None,
    })
}

/// A pinned room matches a program room within the area tolerance of the gates.
const AREA_MATCH: f64 = 0.02;
/// How far a pinned outline may sit off the lines of the free plan, m.
const PIN_TOL: f64 = 1e-4;
/// A footprint length is probed with this share of the search budget.
const PROBE_SHARE: usize = 8;
/// Longest footprint probed: this many quarter steps past the lower bound.
const MAX_GROW: usize = 40;

/// Why the free placement does not take a brief, if it does not.
pub(super) fn free_refusal(spec: &ProjectSpec) -> Option<&'static str> {
    // a pinned room of an apartment comes with its apartment's zone (see `Pins::flats`)
    let loose = spec.pins.as_ref().is_some_and(|p| {
        p.rooms.iter().any(|r| r.owner != PRole::Public && !p.flats.iter().any(|z| z.storey == r.storey && z.owner == r.owner))
    });
    if loose && has_flats(spec) {
        Some("a pinned apartment room has no pinned apartment zone: pin the room again")
    } else if spec.hall_depth.is_some() {
        Some("a deep-plan hall keeps the band layout")
    } else if spec.plate.is_some() {
        Some("a footprint polygon keeps the band layout")
    } else {
        None
    }
}

fn has_flats(spec: &ProjectSpec) -> bool {
    spec.typology == Typology::ApartmentSection || !spec.apartments.is_empty()
}

/// The rectangle of a free plan: length along the spine, full depth, south side, corridor,
/// where the core starts; a gallery has rooms on the south side only, and with the sun rule
/// apartments face south or close an end.
struct FreeGeo {
    w: f64,
    full: f64,
    ds: f64,
    c: f64,
    core_x: f64,
    gallery: bool,
    sun: bool,
}

/// One apartment type of the brief, sized once: its rooms, the frontage it takes on a side
/// and the width it takes closing an end of the corridor (`None`: it cannot).
struct FlatKind {
    rooms: Vec<(RoomKind, String, f64)>,
    fr: Vec<place::FlatRoom>,
    w: f64,
    cap: Option<f64>,
    count: u32,
}

fn flat_kinds(spec: &ProjectSpec, full: f64, ds: f64, c: f64) -> Result<Vec<FlatKind>, GenError> {
    let module = rule("layout.module");
    let mut out = vec![];
    for a in &spec.apartments {
        let rooms: Vec<(RoomKind, String, f64)> =
            a.rooms.iter().flat_map(|r| (0..r.count).map(move |_| (r.kind, r.name.clone(), r.area))).collect();
        let fr: Vec<place::FlatRoom> = rooms.iter().map(|r| place::FlatRoom { area: r.2, inner: r.0.inner() }).collect();
        let w = place::flat_width(&fr, ds, MIN_ROOM_W, module)
            .ok_or_else(|| infeasible(format!("free placement: apartment {} does not fit {ds:.1} m behind the corridor", a.name)))?;
        let cap = place::flat_cap(&fr, full, (ds, ds + c), MIN_ROOM_W, module);
        out.push(FlatKind { rooms, fr, w, cap, count: a.count });
    }
    Ok(out)
}

/// One free floor ready to solve.
struct FreeFloor {
    units: Vec<Unit>,
    prob: place::Problem,
    /// Pinned outlines, snapped to the lines of the plan, and their units.
    pinned: Vec<(Vec<P>, usize)>,
    core_first: usize,
    /// Hall unit and apartment type of every apartment of the floor placed by the solver.
    flats: Vec<(usize, usize)>,
    /// Pinned apartments: hall unit, type, zone and the pinned rooms inside it.
    held: Vec<(usize, usize, place::Rect, Vec<Pin>)>,
}

/// Where an outline `b` stands on the free plan, fixed into `fixed` or `caps`: its `y` span.
/// `None`: it closes no end and fills no side.
fn stand(g: &FreeGeo, b: (P, P), fixed: &mut Vec<Fixed>, caps: &mut [Option<Cap>; 2]) -> Option<(f64, f64)> {
    let ((x0, y0), (x1, y1)) = b;
    let near = |p: f64, q: f64| (p - q).abs() < PIN_TOL;
    if near(y0, 0.0) && near(y1, g.full) {
        if near(x0, 0.0) {
            caps[0] = Some(Cap::Fixed(x1));
        } else if near(x1, g.w) {
            caps[1] = Some(Cap::Fixed(g.w - x0));
        } else {
            return None;
        }
        Some((0.0, g.full))
    } else if near(y0, 0.0) && near(y1, g.ds) {
        fixed.push(Fixed { side: Side::South, x0, x1 });
        Some((0.0, g.ds))
    } else if near(y0, g.ds + g.c) && near(y1, g.full) {
        fixed.push(Fixed { side: Side::North, x0, x1 });
        Some((g.ds + g.c, g.full))
    } else {
        None
    }
}

/// The rooms of apartment type `kind` the pinned rooms stand for, pairwise: the same kind at
/// the asked area. A pinned store matching no room is left out (stores are what is left over).
fn match_rooms(kind: &FlatKind, pins: &[Pin]) -> Option<Vec<(usize, usize)>> {
    let mut out: Vec<(usize, usize)> = vec![];
    for (j, p) in pins.iter().enumerate() {
        let a = footprint::signed_area(&p.polygon).abs();
        let hit = (0..kind.rooms.len()).find(|&i| {
            let (kd, _, asked) = &kind.rooms[i];
            !out.iter().any(|x| x.1 == i) && *kd == p.kind && (asked - a).abs() <= AREA_MATCH * asked
        });
        match hit {
            Some(i) => out.push((j, i)),
            None if p.kind == RoomKind::Storage => {}
            None => return None,
        }
    }
    Some(out)
}

fn free_floor(spec: &ProjectSpec, k: usize, g: &FreeGeo, core: Option<&Core>, kinds: &[FlatKind], apt0: u32, budget: (usize, usize)) -> Result<FreeFloor, GenError> {
    let ru = spec.ru;
    let fl = k + 1;
    let mut units: Vec<Unit> = vec![];
    // program rooms still to place: unit and a frontage kept whatever the depth
    let mut asks: Vec<(usize, Option<f64>)> = vec![];
    let mut fixed = vec![];
    let mut pinned = vec![];
    let mut flats = vec![];
    let mut held = vec![];
    let mut items = vec![];
    let mut caps = [None, None];
    if kinds.is_empty() {
        // a lobby the brief asks for, wide enough for the entrance on either side, is the
        // entrance: a second one would only add circulation
        let side = g.ds.min(g.full - g.ds - g.c);
        let lobby = spec.storey_rooms(k).iter().any(|r| r.kind == RoomKind::Lobby && r.area / side >= ENTRANCE_W);
        if k == 0 && !lobby {
            units.push(public(RoomKind::Lobby, names::kind_name(RoomKind::Lobby, ru).to_string(), None));
            asks.push((0, Some(ENTRANCE_W)));
        }
    } else {
        // the entrance stands beside the core on every storey, so the storeys above lay out
        // the same and their wet rooms stack: a service room over the entrance
        let x0 = if core.is_some() { g.core_x + core.map_or(0.0, |c| c.w) } else { g.core_x };
        let kind = if k == 0 { RoomKind::Lobby } else { RoomKind::Technical };
        units.push(public(kind, names::kind_name(kind, ru).to_string(), None));
        fixed.push(Fixed { side: Side::South, x0, x1: x0 + ENTRANCE_W });
        pinned.push((vec![(x0, 0.0), (x0 + ENTRANCE_W, 0.0), (x0 + ENTRANCE_W, g.ds), (x0, g.ds)], 0));
        // pinned apartments first: each zone stands fixed as an obstacle, keeps its number and
        // takes the place of one apartment of its type; the rest are laid out around them
        let mut skip = vec![0u32; kinds.len()];
        let mut taken: Vec<u32> = vec![];
        for z in spec.pins.iter().flat_map(|p| &p.flats).filter(|z| z.storey == k) {
            let PRole::Apartment(a) = z.owner else { continue };
            let rooms: Vec<Pin> = spec
                .pins
                .iter()
                .flat_map(|p| &p.rooms)
                .filter(|r| r.storey == k && r.owner == z.owner && r.kind != RoomKind::Hall)
                .cloned()
                .collect();
            let t = (0..kinds.len())
                .find(|&t| skip[t] < kinds[t].count && match_rooms(&kinds[t], &rooms).is_some())
                .ok_or_else(|| infeasible(format!("floor {fl}: pinned apartment {a} matches no apartment type left in the brief: unpin it or ask for its type")))?;
            skip[t] += 1;
            taken.push(a);
            let b = footprint::bbox(&z.polygon);
            let (ya, yb) = stand(g, b, &mut fixed, &mut caps).ok_or_else(|| {
                infeasible(format!("floor {fl}: pinned apartment {a} does not stand on a side of the free plan: unpin it or keep the footprint"))
            })?;
            let hall = names::kind_name(RoomKind::Hall, ru).to_string();
            units.push(Unit { kind: RoomKind::Hall, name: hall, owner: z.owner, asked: None, grown: false });
            let zone = place::Rect { x0: b.0 .0, y0: ya, x1: b.1 .0, y1: yb, part: place::Part::Corridor };
            held.push((units.len() - 1, t, zone, rooms));
        }
        let mut n = apt0;
        for (t, f) in kinds.iter().enumerate() {
            for _ in 0..f.count - skip[t] {
                n += 1;
                while taken.contains(&n) {
                    n += 1;
                }
                let owner = PRole::Apartment(n);
                let hall = names::kind_name(RoomKind::Hall, ru).to_string();
                units.push(Unit { kind: RoomKind::Hall, name: hall, owner, asked: None, grown: false });
                let u = units.len() - 1;
                flats.push((u, t));
                items.push(place::Item {
                    unit: u,
                    area: f.w * g.ds,
                    width: None,
                    inner: false,
                    grows: true,
                    end: f.cap.is_some(),
                    end_w: f.cap,
                    sides: [true, !g.sun],
                    circ: false,
                });
            }
        }
    }
    for r in spec.storey_rooms(k) {
        for _ in 0..r.count {
            units.push(public(r.kind, r.name.clone(), Some(r.area)));
            asks.push((units.len() - 1, None));
        }
    }
    let zoned = |p: &Pin| spec.pins.iter().flat_map(|x| &x.flats).any(|z| z.storey == p.storey && z.owner == p.owner);
    for pin in spec.pins.iter().flat_map(|p| &p.rooms).filter(|p| p.storey == k && !(!kinds.is_empty() && zoned(p))) {
        let a = footprint::signed_area(&pin.polygon).abs();
        let off = || {
            infeasible(format!(
                "floor {fl}: pinned room {} does not stand on a side of the free plan: unpin it or keep the footprint",
                pin.name
            ))
        };
        let ((x0, y0), (x1, y1)) = footprint::bbox(&pin.polygon);
        if ((x1 - x0) * (y1 - y0) - a).abs() > PIN_TOL * a.max(1.0) {
            return Err(off());
        }
        let (ya, yb) = stand(g, ((x0, y0), (x1, y1)), &mut fixed, &mut caps).ok_or_else(off)?;
        // the program room it stands for: the same kind at the asked area
        let hit = asks.iter().position(|&(u, w)| {
            w.is_none() && units[u].kind == pin.kind && units[u].asked.is_some_and(|x| (x - a).abs() <= AREA_MATCH * x)
        });
        let u = match hit {
            Some(j) => asks.remove(j).0,
            None => {
                units.push(public(pin.kind, pin.name.clone(), None));
                units.len() - 1
            }
        };
        units[u].name = pin.name.clone();
        units[u].owner = pin.owner;
        pinned.push((vec![(x0, ya), (x1, ya), (x1, yb), (x0, yb)], u));
    }
    let core_first = units.len();
    if let Some(c) = core {
        for &(_, _, kind) in &c.cells {
            units.push(public(kind, names::kind_name(kind, ru).to_string(), None));
        }
        fixed.push(Fixed { side: Side::South, x0: g.core_x, x1: g.core_x + c.w });
    }
    items.extend(asks.iter().map(|&(u, w)| place::Item {
        width: w,
        grows: w.is_none(),
        circ: units[u].kind.circulation(),
        ..place::Item::room(u, units[u].asked.unwrap_or(w.unwrap_or(0.0) * g.ds), units[u].kind.inner())
    }));
    let prob = place::Problem {
        w: g.w,
        depth: [g.ds, g.full - g.ds - g.c],
        corridor: g.c,
        min_w: MIN_ROOM_W,
        items,
        fixed,
        caps,
        nodes: budget.0,
        rounds: budget.1,
        blind: [false, false],
        costs: place::Costs::ruled(),
        ms: rule("layout.search_ms"),
    };
    Ok(FreeFloor { units, prob, pinned, core_first, flats, held })
}

/// The frame of an apartment zone: local `x` along its corridor edge, local `y` back from it.
/// An end zone closing the corridor at the west (`x1`) or east (`x0`) end, or a side zone
/// starting at `x0` whose corridor edge is at `y`.
#[derive(Clone, Copy)]
enum Zf {
    West(f64),
    East(f64),
    South(f64, f64),
    North(f64, f64),
}

impl Zf {
    fn to(self, lx: f64, ly: f64) -> P {
        match self {
            Zf::West(x1) => (x1 - ly, lx),
            Zf::East(x0) => (x0 + ly, lx),
            Zf::South(x0, y) => (x0 + lx, y - ly),
            Zf::North(x0, y) => (x0 + lx, y + ly),
        }
    }

    fn from(self, (gx, gy): P) -> P {
        match self {
            Zf::West(x1) => (gy, x1 - gx),
            Zf::East(x0) => (gy, gx - x0),
            Zf::South(x0, y) => (gx - x0, y - gy),
            Zf::North(x0, y) => (gx - x0, gy - y),
        }
    }
}

/// Lowers a solved free floor through the same arrangement as the band storeys.
fn free_storey(sh: &Shared, k: usize, g: &FreeGeo, region: &Region, f: FreeFloor, core: Option<&Core>, kinds: &[FlatKind]) -> Result<PStorey, GenError> {
    let spec = sh.spec;
    let plan = place::solve(&f.prob).ok_or_else(|| {
        infeasible(format!(
            "floor {}: the free plan finds no place for the program along {:.1} m: lengthen the footprint or shrink the program",
            k + 1,
            g.w
        ))
    })?;
    let shape = place::lay(&f.prob, &plan);
    let mut units = f.units;
    for &i in &shape.grown {
        units[f.prob.items[i].unit].grown = true;
    }
    let spare0 = units.len();
    for &store in &shape.spares {
        let s = &spec.spare;
        units.push(if store {
            public(RoomKind::Storage, names::kind_name(RoomKind::Storage, spec.ru).to_string(), None)
        } else {
            public(s.kind, if spec.ru { &s.name.1 } else { &s.name.0 }.clone(), None)
        });
    }
    let lab = |part: place::Part| match part {
        place::Part::Item(i) => Lab::Room(f.prob.items[i].unit),
        place::Part::Spare(j) => Lab::Room(spare0 + j),
        place::Part::Corridor => Lab::Corr(0),
    };
    // apartments: each zone laid out inside by the same solver, its hall the zone's unit
    let mut rects: Vec<(place::Rect, Lab)> = vec![];
    let mut zones: Vec<(place::Rect, usize, usize, &[Pin])> = vec![];
    for r in &shape.rects {
        let flat = match r.part {
            place::Part::Item(i) => f.flats.iter().find(|x| x.0 == f.prob.items[i].unit).copied(),
            _ => None,
        };
        match flat {
            Some((hall, t)) => zones.push((*r, hall, t, &[] as &[Pin])),
            None => rects.push((*r, lab(r.part))),
        }
    }
    zones.extend(f.held.iter().map(|(hall, t, r, pins)| (*r, *hall, *t, pins.as_slice())));
    for (r, hall, t, pins) in zones {
        let kind = &kinds[t];
        let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
        let (ds, c) = (g.ds, g.c);
        // local x along the corridor edge, local y back from it
        let (w, depth, door, zf) = if near(r.y0, 0.0) && near(r.y1, g.full) {
            let zf = if near(r.x0, 0.0) { Zf::West(r.x1) } else { Zf::East(r.x0) };
            (g.full, r.x1 - r.x0, Some((ds, ds + c)), zf)
        } else if near(r.y1, ds) {
            (r.x1 - r.x0, ds, None, Zf::South(r.x0, ds))
        } else {
            (r.x1 - r.x0, r.y1 - r.y0, None, Zf::North(r.x0, ds + c))
        };
        let to = |lx: f64, ly: f64| zf.to(lx, ly);
        let fail = || infeasible(format!("floor {}: apartment {} finds no layout in its {w:.1} x {depth:.1} m zone", k + 1, units[hall].name));
        // pinned rooms in the zone's frame, as the rooms of the type they stand for
        let pairs = match_rooms(kind, pins).ok_or_else(fail)?;
        let hold: Vec<(usize, place::Rect)> = pairs
            .iter()
            .map(|&(j, i)| {
                let (a, b) = footprint::bbox(&pins[j].polygon);
                let (a, b) = (zf.from(a), zf.from(b));
                (i, place::Rect { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1), part: place::Part::Item(i) })
            })
            .collect();
        let fl = if !hold.is_empty() && hold.len() == kind.rooms.len() {
            // every room pinned: they and the pinned stores stay, the rest of the zone is the hall
            let mut v: Vec<place::Rect> = hold.iter().map(|h| h.1).collect();
            let stores: Vec<&Pin> = pins.iter().enumerate().filter(|(j, _)| !pairs.iter().any(|x| x.0 == *j)).map(|x| x.1).collect();
            for (j, s) in stores.iter().enumerate() {
                let (a, b) = footprint::bbox(&s.polygon);
                let (a, b) = (zf.from(a), zf.from(b));
                v.push(place::Rect { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1), part: place::Part::Spare(j) });
            }
            v.push(place::Rect { x0: 0.0, y0: 0.0, x1: w, y1: depth, part: place::Part::Corridor });
            place::Flat { rects: v, stores: stores.len(), grown: vec![], cost: 0.0 }
        } else {
            place::interior_around(&kind.fr, w, depth, door, MIN_ROOM_W, &hold).ok_or_else(fail)?
        };
        let owner = units[hall].owner;
        let room0 = units.len();
        for (i, (kd, name, area)) in kind.rooms.iter().enumerate() {
            let name = pairs.iter().find(|x| x.1 == i).map_or_else(|| name.clone(), |x| pins[x.0].name.clone());
            units.push(Unit { kind: *kd, name, owner, asked: Some(*area), grown: fl.grown.contains(&i) });
        }
        let store0 = units.len();
        for _ in 0..fl.stores {
            units.push(Unit { kind: RoomKind::Storage, name: names::kind_name(RoomKind::Storage, spec.ru).to_string(), owner, asked: None, grown: false });
        }
        for s in &fl.rects {
            let (a, b) = (to(s.x0, s.y0), to(s.x1, s.y1));
            let at = place::Rect { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1), part: s.part };
            let l = match s.part {
                place::Part::Item(i) => Lab::Room(room0 + i),
                place::Part::Spare(j) => Lab::Room(store0 + j),
                place::Part::Corridor => Lab::Room(hall),
            };
            rects.push((at, l));
        }
    }
    let mut segs: Vec<(P, P)> = vec![];
    ring_segs(&region.outer, &mut segs);
    for (r, _) in &rects {
        ring_segs(&r.ring(), &mut segs);
    }
    for (poly, _) in &f.pinned {
        ring_segs(poly, &mut segs);
    }
    if let Some(c) = core {
        ring_segs(&c.f.rect(0.0, c.w, 0.0, c.d), &mut segs);
        for cl in &c.cells[1..] {
            segs.push((c.f.at(cl.0, 0.0), c.f.at(cl.0, c.d)));
        }
    }
    let ar = Arrangement::new(&segs);
    let labels = ar.label(|p| {
        if !offset::contains(region, p) {
            return None;
        }
        if let Some((_, u)) = f.pinned.iter().find(|(r, _)| crate::check::inside(r, p)) {
            return Some(Lab::Room(*u));
        }
        if let Some(c) = core {
            let (x, y) = c.f.local(p);
            if x > 0.0 && x < c.w && y > 0.0 && y < c.d {
                let i = c.cells.iter().position(|cl| x < cl.1).unwrap_or(c.cells.len() - 1);
                return Some(Lab::Room(f.core_first + i));
            }
        }
        Some(rects.iter().find(|r| r.0.holds(p)).map_or(Lab::Lost, |r| r.1))
    });
    finish(sh, k, region, region.outer.clone(), &units, core, &ar, &labels)
}

/// Builds a bar brief by free placement (see `place`): the footprint keeps the band depth and
/// takes the shortest length on the placement module that holds every floor, unless the
/// brief or the pins fix it; the core stands mid-length on the south side.
/// An apartment section is tried as a corridor plan (apartments on both sides, the end ones
/// round the corridor ends, the core between them) and as a gallery (one side, the corridor
/// on the north facade); `layout.section` picks one or, on auto, the better that passes.
pub(super) fn build_free(spec: &ProjectSpec) -> Result<GProject, GenError> {
    if let Some(why) = free_refusal(spec) {
        return Err(infeasible(format!("free placement: {why}")));
    }
    if !has_flats(spec) {
        return free_as(spec, false);
    }
    match rule("layout.section") as usize {
        SECTION_CORRIDOR => free_as(spec, false),
        SECTION_GALLERY => free_as(spec, true),
        _ => match (free_as(spec, false), free_as(spec, true)) {
            (Ok(a), Ok(b)) => {
                // basements are added after the storeys: the gates see the storeys alone
                let s = ProjectSpec { basements: 0, ..spec.clone() };
                let ok = |p: &GProject| super::check_project(&s, p).passed();
                let better = super::score::score(spec, &b).compare(&super::score::score(spec, &a)).is_lt();
                Ok(if ok(&b) && (!ok(&a) || better) { b } else { a })
            }
            (Ok(a), Err(_)) => Ok(a),
            (Err(_), Ok(b)) => Ok(b),
            (Err(e), Err(_)) => Err(e),
        },
    }
}

/// `layout.section` options.
const SECTION_CORRIDOR: usize = 1;
const SECTION_GALLERY: usize = 2;
/// `light.sun_side` option: apartments face the south facade or close an end.
const SUN_SOUTH: f64 = 1.0;

fn free_as(spec: &ProjectSpec, gallery: bool) -> Result<GProject, GenError> {
    let pins = spec.pins.as_ref();
    let c = spec.corridor;
    let flats = has_flats(spec);
    let ds = pins.map_or(spec.band_depth, |p| p.south);
    let full = match pins.map(|p| p.depth).or(spec.depth) {
        Some(d) if !gallery => d,
        _ if gallery => ds + c,
        _ => 2.0 * spec.band_depth + c,
    };
    let sun = flats && rule("light.sun_side") == SUN_SOUTH;
    let kinds = if flats { flat_kinds(spec, full, ds, c)? } else { vec![] };
    let per_floor: u32 = kinds.iter().map(|f| f.count).sum();
    if !gallery && full - ds - c < MIN_ROOM_W {
        return Err(infeasible(format!(
            "free placement: {full:.1} m of depth leaves no room north of the {c} m corridor behind {ds} m of rooms"
        )));
    }
    let place_core = |x: f64| -> Result<Option<Core>, GenError> {
        if spec.has_stair() {
            core_at(spec, Frame { o: (x, 0.0), u: (1.0, 0.0) }, ds, false).map(Some)
        } else {
            Ok(None)
        }
    };
    let cw = place_core(0.0)?.map_or(0.0, |c| c.w);
    let module = rule("layout.module");
    let snap = |x: f64| (x / module - 1e-9).ceil() * module;
    let budget = (rule("layout.search_nodes") as usize, rule("layout.lns_rounds") as usize);
    let probe = ((budget.0 / PROBE_SHARE).max(1), 0);
    let n = spec.floors as usize;
    let geo = |w: f64| {
        let mid = (((w - cw) / 2.0 / module).round() * module).clamp(0.0, (w - cw).max(0.0));
        FreeGeo { w, full, ds, c, core_x: pins.and_then(|p| p.core_x).unwrap_or(mid), gallery, sun }
    };
    let fits = |w: f64| -> Result<bool, GenError> {
        let g = geo(w);
        let core = place_core(g.core_x)?;
        let mut seen: Vec<String> = vec![];
        for k in 0..n {
            let f = free_floor(spec, k, &g, core.as_ref(), &kinds, 0, probe)?;
            let key = format!("{:?}|{:?}|{:?}", f.prob.items, f.prob.fixed, f.prob.caps);
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            if place::solve(&f.prob).is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    };
    let w = match pins.map(|p| p.width).or(spec.width) {
        Some(w) => w,
        None => {
            let flats_area: f64 = kinds.iter().map(|f| f64::from(f.count) * f.w * ds).sum::<f64>()
                + if flats { ENTRANCE_W * ds } else { 0.0 };
            let need = (0..n)
                .map(|k| spec.storey_rooms(k).iter().map(|r| r.area * f64::from(r.count)).sum::<f64>() + cw * ds + flats_area)
                .fold(0.0, f64::max);
            // nothing shorter holds the program even without a corridor
            let mut bad = snap(need / full).max(snap(MIN_ROOM_W));
            if fits(bad)? {
                bad
            } else {
                let mut good = bad;
                let mut steps = 0;
                loop {
                    good = snap(good + (good / 4.0).max(module));
                    if fits(good)? {
                        break;
                    }
                    bad = good;
                    steps += 1;
                    if steps > MAX_GROW {
                        return Err(infeasible("free placement: no footprint length holds the program"));
                    }
                }
                // the shortest that holds, between the last that did not and the first that did
                loop {
                    let mid = ((bad + good) / 2.0 / module).round() * module;
                    if mid <= bad + 1e-9 || mid >= good - 1e-9 {
                        break good;
                    }
                    if fits(mid)? {
                        good = mid;
                    } else {
                        bad = mid;
                    }
                }
            }
        }
    };
    let g = geo(w);
    let core = place_core(g.core_x)?;
    let region = Region { outer: vec![(0.0, 0.0), (w, 0.0), (w, full), (0.0, full)], holes: vec![] };
    let sh = Shared { spec, core: core.as_ref(), d: ds, df: ds, win: win_of(spec.typology), dirs: vec![(1.0, 0.0), (0.0, 1.0)] };
    let mut storeys = Vec::with_capacity(n);
    for k in 0..n {
        // a periodic storey (a refuge floor) lays out its own rooms, not flats
        let none: &[FlatKind] = &[];
        let kk: &[FlatKind] = if spec.periodic_at(k).is_some() { none } else { &kinds };
        let f = free_floor(spec, k, &g, core.as_ref(), kk, spec.flat_floors_before(k) * per_floor, budget)?;
        storeys.push(free_storey(&sh, k, &g, &region, f, core.as_ref(), kk)?);
    }
    let apartments = spec.flat_floors_before(n) * per_floor;
    let mut rs = roofs(spec, &region, &sh.dirs);
    if rs.is_empty() {
        return Err(infeasible("the top plate leaves no roof"));
    }
    let roof = rs.remove(0);
    let rooms: usize = storeys.iter().map(|s| s.rooms.len()).sum();
    let metrics = vec![
        ("floors", n as f64),
        ("width", w),
        ("depth", full),
        ("gross_floor_area", w * full * n as f64),
        ("rooms", rooms as f64),
        ("apartments", f64::from(apartments)),
    ];
    Ok(GProject {
        storeys,
        roof,
        extra_roofs: rs,
        copies: 1,
        court: None,
        podium: None,
        footprint: (w, full),
        apartments,
        metrics,
        hvac: super::GHvac::default(),
        services: super::GServices::default(),
        basements: vec![],
        site: super::site::GSite::default(),
        fire: Default::default(),
        lifts: Default::default(),
        shell: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GOpening, GWall, WallRole};

    fn wall_with_one(len: f64) -> GLayout {
        let mut l = GLayout::default();
        l.walls.push(GWall { a: (0.0, 0.0), b: (len, 0.0), role: WallRole::Exterior, bulge: 0.0, fire: None });
        l.openings.push(GOpening { wall: 0, along: 0.15, door: false, width: 0.9, height: 1.5, sill: 0.9, ty: None });
        l
    }

    /// An opening clamped onto its neighbour moves past it with the gap, or is dropped.
    #[test]
    fn an_opening_clamped_onto_a_neighbour_moves_off_it_or_is_dropped() {
        let at = |len: f64, along: f64| free_slot(&wall_with_one(len), 0, along, 0.9, (0.0, len)).unwrap();
        assert!((at(3.0, 0.5) - 1.15).abs() < 1e-9);
        assert!((at(3.0, 1.6) - 1.6).abs() < 1e-9);
        assert_eq!(free_slot(&wall_with_one(2.0), 0, 0.95, 0.9, (0.0, 2.0)), None);
    }

    /// A wall running past several rooms: a room's opening stays on that room's stretch of it.
    #[test]
    fn an_opening_stays_on_its_rooms_stretch_of_a_long_wall() {
        let l = wall_with_one(12.0);
        let t = free_slot(&l, 0, 0.5, 0.9, (4.0, 6.0)).unwrap();
        assert!(t >= 4.15 - 1e-9 && t + 0.9 <= 5.85 + 1e-9, "{t}");
        assert_eq!(free_slot(&l, 0, 5.0, 0.9, (4.0, 5.0)), None, "a stretch too short for it");
    }

    /// A thin plate's corners take the rooms that fill them best: the 60 x 18 m hospital wing,
    /// whose corners round a 2.4 m corridor end eat most of the facade, holds its whole
    /// programme on every storey, with or without a basement, and each asked room is laid.
    #[test]
    fn corner_zones_take_the_rooms_that_fill_them_best() {
        for b in [0, 1] {
            let v = serde_json::json!({"typology": "hospital_wing", "floors": 4, "basements": b,
                "footprint": {"shape": "rectangle", "width": 60, "depth": 18}});
            let s = super::super::parse_spec(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
            let p = super::super::build_project(&s).unwrap_or_else(|e| panic!("{v}: {e}"));
            for (k, st) in p.storeys.iter().enumerate() {
                let mut want: Vec<String> = s.storey_rooms(k).iter().flat_map(|r| std::iter::repeat_n(r.name.clone(), r.count as usize)).collect();
                let mut got: Vec<String> = st.rooms.iter().zip(&st.layout.rooms).filter(|(r, _)| r.asked.is_some()).map(|(_, g)| g.name.clone()).collect();
                want.sort();
                got.sort();
                assert_eq!(got, want, "{v}: storey {k}");
            }
        }
    }

    /// The default bands of a plate fill its depth exactly with the corridor: the plate holds
    /// them as they are, never refused at the exact fit.
    #[test]
    fn a_plain_bar_with_default_bands_builds_at_the_exact_fit() {
        for (w, dp) in [(40.0, 12.0), (40.0, 16.0), (40.0, 18.0)] {
            for shape in [Some("rectangle"), None] {
                let mut fp = serde_json::json!({"width": w, "depth": dp});
                if let Some(s) = shape {
                    fp["shape"] = serde_json::json!(s);
                }
                let v = serde_json::json!({"typology": "office", "floors": 2, "footprint": fp});
                let s = super::super::parse_spec(&v).unwrap_or_else(|e| panic!("{v}: {e}"));
                if let Err(e) = super::super::build_project(&s) {
                    panic!("{v} (bands {} m, corridor {} m): {e}", s.band_depth, s.corridor);
                }
                if s.plate.is_some() && s.hall_depth.is_none() {
                    let regions = [s.plate.as_ref().unwrap().at(0).unwrap()];
                    assert_eq!(plate_band(&s, &regions).unwrap(), s.band_depth, "{v}: bands kept at the exact fit");
                }
            }
        }
    }
}
