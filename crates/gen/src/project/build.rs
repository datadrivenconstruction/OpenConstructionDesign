// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The deterministic builder. Every typology is a double-loaded plan: room bands on both sides
//! of a corridor, a core (stair, lift) in the middle, the program packed into the four band
//! segments. Rooms take their asked area; spare length grows the neighbouring room or becomes
//! the programme's spare room. A single storey has no stair core.

use super::names;
use super::rules::{rule, rule_mm};
use super::tile::{self, m, mm, res_win, Cell, Mm, WinSize};
use super::{
    slots, ApartmentReq, GProject, PRole, PRoof, PStorey, ProjectSpec, RoofKind, RoomKind, RoomReq,
    Typology,
};
use crate::{GGrid, GLayout, GShaft, GStair, GenError};
use ddc_geom::stair::Shape;

/// Narrowest room along the facade.
fn min_unit() -> Mm {
    rule_mm("room.min_unit")
}

/// Shortest leftover a band may keep before it joins a neighbour.
fn min_fill() -> Mm {
    rule_mm("room.min_fill")
}

/// Free strip between the stair and the corridor door of its room.
fn door_zone() -> Mm {
    rule_mm("core.door_zone")
}

fn stair_margin() -> Mm {
    rule_mm("core.stair_margin")
}

pub(super) fn lift_room() -> Mm {
    rule_mm("core.lift_room")
}

/// The shaft of one car: the rule's size, or the car of the rated capacity with its clearances
/// when that is larger, so the car always fits.
pub(super) fn lift_shaft(spec: &ProjectSpec) -> (f64, f64) {
    shaft_of(super::lift::bank_class(super::lift::serve_height(spec)))
}

/// The shaft of a car of class `c`: the rule's size or the car with its clearances.
pub(super) fn shaft_of(c: &super::lift::Class) -> (f64, f64) {
    let (w, d) = super::lift::envelope(c);
    (rule("core.lift_shaft_w").max(w), rule("core.lift_shaft_d").max(d))
}

/// Floor area of one storey the lift traffic is sized for, m2; 0 when the footprint is not
/// known before the plan is made.
fn plate_area(spec: &ProjectSpec) -> f64 {
    match (spec.width, spec.depth) {
        (Some(w), Some(d)) => w * d,
        (Some(w), None) => w * (2.0 * spec.band_depth + spec.corridor),
        _ => 0.0,
    }
}

/// The lift cars of the core: the brief's count, or, from `core.lift_traffic_from` storeys,
/// the cars the passenger traffic needs in the busiest period. The building is cut into
/// zones of `core.lift_zone_height`; a zone's cars make one round trip of the travel to its
/// top at the rated speed and the stops they take (the probable-stops count for a full car),
/// and carry the peak share of the zone's population in the peak window. The bank holds at
/// most `core.lift_bank_max` shafts; what the traffic wanted beyond it is reported.
pub(super) fn lift_plan(spec: &ProjectSpec) -> super::GLifts {
    let none = super::GLifts { cars: 1, wanted: 1, ..Default::default() };
    if !spec.lift {
        return super::GLifts::default();
    }
    // a plate core stands the bank in two facing rows past `core.lift_row_max`; a plate too
    // small for it lowers this bank in `build_project`, one car at a time
    let bank = rule("core.lift_bank_max") as u32;
    if let Some(n) = spec.lifts {
        // the brief reader refuses a count above the bank; only a plate's lowered bank cuts it
        let issues = (n > bank).then(|| format!("the plate's core holds {bank} of the {n} cars asked")).into_iter().collect();
        return super::GLifts { cars: n.min(bank), wanted: n, issues, ..none };
    }
    let (floors, area) = (f64::from(spec.floors), plate_area(spec));
    if spec.floors < rule("core.lift_traffic_from") as u32 || area <= 0.0 {
        return none;
    }
    let pop = floors * area / rule("core.occupant_area");
    let peak = pop * rule("core.lift_peak_share");
    let height = spec.building_height();
    let zones = (height / rule("core.lift_zone_height")).ceil().max(1.0);
    let per_zone = (floors / zones).ceil().max(1.0);
    let load = rule("core.lift_capacity") * rule("core.lift_fill");
    // stops of a full car among the zone's storeys
    let stops = per_zone * (1.0 - (1.0 - 1.0 / per_zone).powf(load));
    let (mut wanted, mut worst) = (0.0, 0.0_f64);
    for z in 0..zones as u32 {
        let top = height * f64::from(z + 1) / zones;
        let trip = 2.0 * top / rule("core.lift_speed") + (stops + 1.0) * rule("core.lift_stop_time");
        let per_car = rule("core.lift_peak_window") / trip * load;
        // the cars the peak needs, and the cars that keep the wait within the interval
        let by_peak = (peak / zones / per_car).ceil().max(1.0);
        wanted += by_peak.max((trip / rule("core.lift_interval")).ceil());
        worst = worst.max(trip);
    }
    let wanted = wanted as u32;
    let cars = wanted.min(bank).max(1);
    let interval = worst / (f64::from(cars) / zones).max(1.0);
    let carried = peak * f64::from(cars) / f64::from(wanted.max(1));
    let mut issues = vec![];
    if carried + 1e-9 < peak || interval > rule("core.lift_interval") + 1e-9 {
        issues.push(format!(
            "the bank is cut to {cars} of the {wanted} cars the traffic asks for: it carries {carried:.0} of {peak:.0} persons in the peak and waits {interval:.0} s against {:.0} s",
            rule("core.lift_interval")
        ));
    }
    super::GLifts {
        cars,
        wanted,
        zones: zones as u32,
        demand: peak,
        carried,
        interval,
        persons: rule("core.lift_capacity") as u32,
        fighting: 0,
        issues,
    }
}

/// The room of the lift bank in a band `ds` deep: one car's room, then a shaft and a wall for
/// each further car of the longer row and for each service bay.
pub(super) fn lift_room_for(spec: &ProjectSpec, ds: Mm) -> Mm {
    bank_room(spec, ds, service_bays(spec))
}

/// The lift room of the bank in a room `ds` deep with `bays` service bays beside it.
pub(super) fn bank_room(spec: &ProjectSpec, ds: Mm, bays: u32) -> Mm {
    let row = bank_rows(spec, ds).0;
    let extra = i64::from(row - 1) + i64::from(bays);
    lift_room() + extra * (mm(lift_shaft(spec).0) + rule_mm("core.lift_wall"))
}

/// The bank in a lift room `ds` deep, for every core shape: the length of the longer row, and
/// each car's place in its row and whether it stands in the far row, facing the first.
pub(super) fn bank_rows(spec: &ProjectSpec, ds: Mm) -> (u32, Vec<(u32, bool)>) {
    let cars = if spec.lift { lift_plan(spec).cars.max(1) } else { 0 };
    let row = cars.max(1).div_ceil(if spec.lift { lift_rows(spec, ds) } else { 1 });
    (row, (0..cars).map(|i| if i < row { (i, false) } else { (i - row, true) }).collect())
}

/// Whether the bank has more cars than one row takes (`core.lift_row_max`).
pub(super) fn lift_row_full(spec: &ProjectSpec) -> bool {
    spec.lift && lift_plan(spec).cars.max(1) > rule("core.lift_row_max") as u32
}

/// Band depth two facing rows need: each row's shaft off its wall, and the lobby between them
/// `core.lift_lobby_facing` car depths deep.
pub(super) fn facing_depth(spec: &ProjectSpec) -> Mm {
    let car = super::lift::bank_class(super::lift::serve_height(spec)).car.1;
    mm(2.0 * (lift_inset() + lift_shaft(spec).1) + rule("core.lift_lobby_facing") * car)
}

/// Rows of the lift bank in a band `ds` deep: two facing rows past one row's cars when the
/// band holds them and their lobby, else one.
pub(super) fn lift_rows(spec: &ProjectSpec, ds: Mm) -> u32 {
    if lift_row_full(spec) && ds >= facing_depth(spec) {
        2
    } else {
        1
    }
}

/// Service shaft bays the typology asks for beside the lift bank; they stand in the lift room,
/// so a building without a lift has none.
pub(super) fn service_bays(spec: &ProjectSpec) -> u32 {
    let Some(id) = spec.model.as_ref().map(|m| m.id.as_str()).filter(|_| spec.lift) else { return 0 };
    spec.rules.value(&format!("typology.{id}.shafts")).unwrap_or(0.0) as u32
}

/// The bay layouts a core may take, from the full set down to none: bays of one kind each, the
/// duct and cable bays merged, one bay for every kind, and the old per-system shafts alone.
fn bay_levels(spec: &ProjectSpec) -> Vec<Vec<Vec<crate::ShaftKind>>> {
    use crate::ShaftKind::{Duct, Electrical, Riser};
    let n = service_bays(spec);
    if n == 0 {
        return vec![vec![]];
    }
    let full: Vec<Vec<crate::ShaftKind>> = (0..n).map(|i| vec![bay_kind(i)]).collect();
    let mut levels = vec![full.clone()];
    if [Duct, Electrical].iter().all(|k| full.iter().any(|b| b[0] == *k)) {
        let mut merged = vec![];
        for b in &full {
            match b[0] {
                Duct | Electrical if merged.iter().any(|m: &Vec<crate::ShaftKind>| m.contains(&Duct)) => {}
                Duct | Electrical => merged.push(vec![Duct, Electrical]),
                k => merged.push(vec![k]),
            }
        }
        levels.push(merged);
    }
    levels.push(vec![vec![Riser, Electrical, Duct]]);
    levels.push(vec![]);
    levels
}

/// The first layout whose lift room fits in `have`; a core too narrow even for none keeps the
/// last, which is the building as it was built before the bays. Returns the level too.
fn pick_bays(spec: &ProjectSpec, have: Mm) -> (usize, Vec<Vec<crate::ShaftKind>>) {
    let cars = i64::from(lift_plan(spec).cars.max(1)) - 1;
    let mut levels = bay_levels(spec);
    let at = levels
        .iter()
        .position(|b| lift_room() + (cars + b.len() as i64) * (mm(lift_shaft(spec).0) + rule_mm("core.lift_wall")) <= have)
        .unwrap_or(levels.len() - 1);
    (at, levels.swap_remove(at))
}

/// What each service bay carries: risers, then cables, then ducts, round again.
fn bay_kind(i: u32) -> crate::ShaftKind {
    [crate::ShaftKind::Riser, crate::ShaftKind::Electrical, crate::ShaftKind::Duct][i as usize % 3]
}

fn hall_min() -> f64 {
    rule("corridor.hall_min")
}

fn inner_min() -> f64 {
    rule("depth.inner_min")
}

fn facade_min() -> f64 {
    rule("depth.facade_min")
}

pub(super) fn grid_overrun() -> f64 {
    rule("grid.overrun")
}

/// The lift shaft's distance from the wall behind it.
pub(super) fn lift_inset() -> f64 {
    rule("core.lift_inset")
}

/// Gap around a stair in its slab hole.
pub(super) fn hole_clear() -> f64 {
    rule("core.hole_clear")
}

/// Window of the office and school typologies.
pub(super) fn work_win() -> WinSize {
    WinSize {
        w: rule("facade.work_win_w"),
        h: rule("facade.work_win_h"),
        sill: rule("facade.work_sill"),
    }
}

/// Floor area from which a storage room is a warehouse hall.
pub(super) fn hall_area() -> f64 {
    rule("room.hall_area")
}

/// A room of a unit in band coordinates: `u` along the facade, `v` away from the corridor.
#[derive(Clone, Debug)]
struct Piece {
    u0: Mm,
    u1: Mm,
    v0: Mm,
    v1: Mm,
    kind: RoomKind,
    name: String,
    asked: Option<f64>,
}

/// One packed item of a band: a room, a stack of small rooms or a whole apartment.
#[derive(Clone, Debug)]
struct Unit {
    len: Mm,
    apartment: bool,
    /// Baths and toilets: pinned beside the core so they stack floor over floor.
    wet: bool,
    /// An open hall in the deep bay of a deep plan.
    hall: bool,
    pieces: Vec<Piece>,
}

#[derive(Clone, Copy, PartialEq)]
enum PlanKind {
    /// Two bands, the corridor between, the stair core across the bands.
    Core,
    /// Two full-width bands, the stair in the middle strip.
    Narrow,
    /// One band of rooms and the core on the south facade, an open bay behind the corridor.
    Deep,
}

/// Rooms of the deep bay: the open halls, and any room of `room.hall_area` or more that needs
/// no window (a gallery, an atrium, a nave, a warehouse store) and is not a wet room.
fn bay_room(r: &RoomReq) -> bool {
    let dry = !matches!(r.kind, RoomKind::Bath | RoomKind::Wc);
    r.kind.open_hall() || (!r.kind.habitable() && dry && r.area >= hall_area())
}

/// A room the open bay may take when the band runs out of length: dry, no daylight of its
/// own needed, and not the way in.
fn bay_fit(r: &RoomReq) -> bool {
    use RoomKind::*;
    !is_wet(r.kind) && (r.kind.open_hall() || !r.kind.habitable()) && !matches!(r.kind, Corridor | Stair | Lift | Lobby)
}

impl Unit {
    /// Spare band length may widen it: public rooms that are not wet.
    fn grows(&self) -> bool {
        !self.apartment && !self.wet
    }
}

/// Share a room may grow past its asked area and still count as the room asked.
fn grow_tolerance() -> f64 {
    rule("room.grow_tolerance")
}

fn is_wet(k: RoomKind) -> bool {
    matches!(k, RoomKind::Bath | RoomKind::Wc)
}

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

fn expand(rooms: &[RoomReq]) -> Vec<RoomReq> {
    rooms
        .iter()
        .flat_map(|r| std::iter::repeat(r.clone()).take(r.count as usize))
        .collect()
}

fn full_depth(r: &RoomReq, d: Mm) -> Unit {
    let len = mm(r.area / m(d));
    Unit {
        len,
        apartment: false,
        wet: is_wet(r.kind),
        hall: false,
        pieces: vec![Piece {
            u0: 0,
            u1: len,
            v0: 0,
            v1: d,
            kind: r.kind,
            name: r.name.clone(),
            asked: Some(r.area),
        }],
    }
}

/// Rooms too narrow for the band share one strip, stacked from the corridor outwards.
fn stack(small: &[RoomReq], d: Mm, ru: bool) -> Unit {
    let total: f64 = small.iter().map(|r| r.area).sum();
    let dm = m(d);
    let mut w = total / dm;
    let mut filler = 0.0;
    let lit = small.last().is_some_and(|r| r.kind.habitable());
    if mm(w) < min_unit() && lit {
        // the lit room on the facade takes the depth the others leave
        w = m(min_unit());
    } else if mm(w) < min_unit() {
        w = m(min_unit());
        filler = dm - total / w;
        if mm(filler) < min_fill() {
            w = total / (dm - m(min_fill()));
            filler = m(min_fill());
        }
    }
    let len = mm(w);
    let mut pieces = vec![];
    let mut v = 0;
    for (i, r) in small.iter().enumerate() {
        // a store behind a WC or bath would be reached through it, its door swing over the
        // fixture zone: the wet room takes the spare depth instead
        let last = i + 1 == small.len() && (filler == 0.0 || is_wet(r.kind));
        let v1 = if last { d } else { v + mm(r.area / w) };
        pieces.push(Piece {
            u0: 0,
            u1: len,
            v0: v,
            v1,
            kind: r.kind,
            name: r.name.clone(),
            asked: Some(r.area),
        });
        v = v1;
    }
    if v < d {
        pieces.push(Piece {
            u0: 0,
            u1: len,
            v0: v,
            v1: d,
            kind: RoomKind::Storage,
            name: names::kind_name(RoomKind::Storage, ru).to_string(),
            asked: None,
        });
    }
    Unit {
        len,
        apartment: false,
        wet: small.iter().all(|r| is_wet(r.kind)),
        hall: false,
        pieces,
    }
}

/// A room entered only through its anteroom: the anteroom takes the corridor side over the
/// whole width, the room the rest of the depth, so the door between them is the only one.
fn theatre_suite(r: &RoomReq, d: Mm, ru: bool) -> Unit {
    let a = rule_mm("room.anteroom_depth").min(d - min_unit());
    let len = mm(r.area / m(d - a));
    let ante = if ru { crate::lang::ru("Scrub anteroom") } else { "Scrub anteroom" };
    let piece = |v0, v1, kind, name: &str, asked| Piece { u0: 0, u1: len, v0, v1, kind, name: name.to_string(), asked };
    Unit {
        len,
        apartment: false,
        wet: false,
        hall: false,
        pieces: vec![piece(0, a, RoomKind::Lobby, ante, None), piece(a, d, r.kind, &r.name, Some(r.area))],
    }
}

/// Wet rooms first and only with each other, so their strip is the same on every floor.
fn room_units(rooms: &[RoomReq], d: Mm, spec: &ProjectSpec) -> Vec<Unit> {
    let ru = spec.ru;
    // an operating theatre comes with its anteroom: one block, the only way in is through it
    let theatre = super::RoomTags::parse("operating").unwrap_or_default();
    let (suites, rest): (Vec<RoomReq>, Vec<RoomReq>) =
        expand(rooms).into_iter().partition(|r| spec.room_tags.of(r.kind, &r.name).meets(theatre));
    let (wet, dry): (Vec<RoomReq>, Vec<RoomReq>) = rest.into_iter().partition(|r| is_wet(r.kind));
    let mut out: Vec<Unit> = suites.iter().map(|r| theatre_suite(r, d, ru)).collect();
    for group in [wet, dry] {
        let mut small = vec![];
        // a stack holds at most one room people stay in, last, so it keeps the facade
        let narrow = |r: &RoomReq| mm(r.area / m(d)) < min_unit();
        let (lit, rest): (Vec<RoomReq>, Vec<RoomReq>) = group
            .into_iter()
            .partition(|r| narrow(r) && r.kind.habitable());
        let mut rest = rest.into_iter().peekable();
        for h in lit {
            let mut g = vec![];
            while g.len() < 2 && rest.peek().is_some_and(narrow) {
                g.extend(rest.next());
            }
            g.push(h);
            out.push(stack(&g, d, ru));
        }
        for r in rest {
            if narrow(&r) {
                small.push(r);
                if small.len() == 3 {
                    out.push(stack(&small, d, ru));
                    small.clear();
                }
            } else {
                out.push(full_depth(&r, d));
            }
        }
        if !small.is_empty() {
            out.push(stack(&small, d, ru));
        }
    }
    out
}

/// Inner zone (wet rooms, hall) along the corridor, rooms with windows along the facade. The
/// inner depth grows until the facade is long enough for the inner rooms and a hall.
fn apartment_unit(a: &ApartmentReq, d: Mm, ru: bool) -> Result<Unit, GenError> {
    let dm = m(d);
    let facade: Vec<&RoomReq> = a
        .rooms
        .iter()
        .filter(|r| !r.kind.inner() && r.kind != RoomKind::Hall)
        .collect();
    let inner: Vec<&RoomReq> = a.rooms.iter().filter(|r| r.kind.inner()).collect();
    let hall_name = a
        .rooms
        .iter()
        .find(|r| r.kind == RoomKind::Hall)
        .map(|r| r.name.clone())
        .unwrap_or_else(|| names::kind_name(RoomKind::Hall, ru).to_string());
    let af: f64 = facade.iter().map(|r| r.area).sum();
    let ai: f64 = inner.iter().map(|r| r.area).sum();
    let short = |hi: f64| af / (dm - hi) - ai / hi - hall_min();
    let top = dm - facade_min();
    if top < inner_min() {
        return Err(infeasible(format!(
            "apartment {}: band depth {dm} m leaves no inner zone; use band_depth >= {}",
            a.name,
            facade_min() + inner_min()
        )));
    }
    let mut hi = 2.0_f64.clamp(inner_min(), top);
    if short(hi) < 0.0 {
        if short(top) < 0.0 {
            return Err(infeasible(format!(
                "apartment {}: rooms with windows ({af:.1} m2) are too small for the wet \
                 rooms and hall ({ai:.1} m2 + hall); add living area or shrink bath and wc",
                a.name
            )));
        }
        let (mut lo, mut up) = (hi, top);
        for _ in 0..40 {
            let mid = (lo + up) / 2.0;
            if short(mid) >= 0.0 {
                up = mid;
            } else {
                lo = mid;
            }
        }
        hi = up;
    }
    let mut facade = facade;
    // the largest room over the inner rooms of the left end, so it still meets the hall
    facade.sort_by(|x, y| y.area.total_cmp(&x.area));
    // or the two largest at the two ends, the others over the hall between them
    let mut ends = facade.clone();
    if ends.len() > 2 {
        let second = ends.remove(1);
        ends.push(second);
    }
    // the inner zone deepens, its rooms narrowing, until every room with a window meets the
    // hall over a door
    let step = 0.05;
    let tries = ((top - hi) / step).floor().max(0.0) as usize;
    let first = apartment_lay(&facade, &inner, &hall_name, d, hi);
    let lay = (0..=tries)
        .flat_map(|i| [&facade, &ends].map(|f| (f, if i == tries { top } else { hi + step * i as f64 })))
        .map(|(f, h)| apartment_lay(f, &inner, &hall_name, d, h))
        .find(|l| l.2)
        .unwrap_or(first);
    let (pieces, len, _) = lay;
    Ok(Unit {
        len,
        apartment: true,
        wet: false,
        hall: false,
        pieces,
    })
}

/// One apartment laid with inner depth `hi`: its pieces, its length, and whether every room
/// with a window meets the hall over a door's run.
fn apartment_lay(facade: &[&RoomReq], inner: &[&RoomReq], hall_name: &str, d: Mm, hi: f64) -> (Vec<Piece>, Mm, bool) {
    let him = mm(hi);
    let df = m(d) - m(him);
    let mut pieces = vec![];
    let mut u = 0;
    let n = facade.len();
    let len = facade.iter().map(|r| mm(r.area / df)).sum::<Mm>();
    for (i, r) in facade.iter().enumerate() {
        let u1 = if i + 1 == n { len } else { u + mm(r.area / df) };
        pieces.push(Piece {
            u0: u,
            u1,
            v0: him,
            v1: d,
            kind: r.kind,
            name: r.name.clone(),
            asked: Some(r.area),
        });
        u = u1;
    }
    let (inner, left) = ends_split(inner, &pieces, hi);
    let mut u = 0;
    for r in &inner[..left] {
        let u1 = u + mm(r.area / hi);
        pieces.push(inner_piece(r, u, u1, him));
        u = u1;
    }
    let hall0 = u;
    let mut u = len;
    for r in inner[left..].iter().rev() {
        let u0 = u - mm(r.area / hi);
        pieces.push(inner_piece(r, u0, u, him));
        u = u0;
    }
    pieces.push(Piece {
        u0: hall0,
        u1: u,
        v0: 0,
        v1: him,
        kind: RoomKind::Hall,
        name: hall_name.to_string(),
        asked: None,
    });
    let need = mm(rule("room.door")) + 2 * rule_mm("facade.edge_clear");
    let (h0, h1) = (hall0, u);
    // the hall's own entrance from the corridor is a door between owners, the wide one
    let entry = mm(rule("room.door_wide")) + 2 * rule_mm("facade.edge_clear");
    let ok = h1 - h0 >= entry && pieces.iter().filter(|p| p.v1 == d).all(|p| p.u1.min(h1) - p.u0.max(h0) >= need);
    (pieces, len, ok)
}

/// The inner rooms of an apartment split between its two ends: under each end room they
/// leave it a door's run (door and its edge clearances) onto the hall, so every room with a
/// window reaches the hall. The most even split that does; half and half when none does.
/// Gives the rooms left end first, right end next, and the number at the left.
fn ends_split<'a>(inner: &[&'a RoomReq], facade: &[Piece], hi: f64) -> (Vec<&'a RoomReq>, usize) {
    let half = (inner.to_vec(), inner.len().div_ceil(2));
    let (Some(f0), Some(f1)) = (facade.first(), facade.last()) else { return half };
    if inner.len() > 12 {
        return half;
    }
    let need = mm(rule("room.door")) + 2 * rule_mm("facade.edge_clear");
    let w: Vec<Mm> = inner.iter().map(|r| mm(r.area / hi)).collect();
    let (hl, hr) = (w[..half.1].iter().sum::<Mm>(), w[half.1..].iter().sum::<Mm>());
    if (hl == 0 || hl + need <= f0.u1 - f0.u0) && (hr == 0 || hr + need <= f1.u1 - f1.u0) {
        return half;
    }
    let mut best: Option<(usize, Mm, u32)> = None;
    for mask in 0..(1u32 << inner.len()) {
        let (mut l, mut r) = (0, 0);
        for (i, wi) in w.iter().enumerate() {
            if mask >> i & 1 == 1 {
                l += wi;
            } else {
                r += wi;
            }
        }
        let fits = (l == 0 || l + need <= f0.u1 - f0.u0) && (r == 0 || r + need <= f1.u1 - f1.u0);
        let nl = mask.count_ones() as usize;
        let key = (nl.abs_diff(inner.len() - nl), (l - r).abs());
        if fits && best.is_none_or(|b| key < (b.0, b.1)) {
            best = Some((key.0, key.1, mask));
        }
    }
    let Some((_, _, mask)) = best else { return half };
    let pick = |side: u32| inner.iter().enumerate().filter(move |(i, _)| (mask >> i & 1) == side).map(|(_, r)| *r);
    let mut out: Vec<&RoomReq> = pick(1).collect();
    let n = out.len();
    out.extend(pick(0));
    (out, n)
}

fn inner_piece(r: &RoomReq, u0: Mm, u1: Mm, him: Mm) -> Piece {
    Piece {
        u0,
        u1,
        v0: 0,
        v1: him,
        kind: r.kind,
        name: r.name.clone(),
        asked: Some(r.area),
    }
}

/// Segments: 0 south-left, 1 north-left, 2 south-right, 3 north-right. A narrow plan has
/// its stair in the middle strip and only the two full-width segments 0 and 1.
/// A deep plan has its halls in segment 1, the whole bay, and the rest south.
fn assign(units: &[Unit], plan: PlanKind) -> ([Vec<usize>; 4], [Mm; 4]) {
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(units[i].len), i));
    let mut segs: [Vec<usize>; 4] = Default::default();
    let mut load = [0; 4];
    // wet strips start at the core, in brief order, on every floor: south-right of a core
    // plan, south from the stair end of a narrow one
    let wet_seg = if plan == PlanKind::Narrow { 0 } else { 2 };
    for i in (0..units.len()).filter(|&i| units[i].wet) {
        load[wet_seg] += units[i].len;
        segs[wet_seg].push(i);
    }
    for i in (0..units.len()).filter(|&i| units[i].hall) {
        load[1] += units[i].len;
        segs[1].push(i);
    }
    let allowed: &[usize] = match plan {
        PlanKind::Core => &[0, 1, 2, 3],
        PlanKind::Narrow => &[0, 1],
        PlanKind::Deep => &[0, 2],
    };
    for i in order
        .into_iter()
        .filter(|&i| !units[i].wet && !units[i].hall)
    {
        let mut best = (Mm::MAX, Mm::MAX, 0);
        for &s in allowed {
            let mut l = load;
            l[s] += units[i].len;
            let total = match plan {
                PlanKind::Deep => (l[0] + l[2]).max(l[1]),
                _ => l[0].max(l[1]) + l[2].max(l[3]),
            };
            let key = (total, l[s], s);
            if key < best {
                best = key;
            }
        }
        let s = best.2;
        load[s] += units[i].len;
        segs[s].push(i);
    }
    (segs, load)
}

struct Program {
    units: Vec<Unit>,
    segs: [Vec<usize>; 4],
    load: [Mm; 4],
}

/// Placed stair: the definition and its plan box in absolute millimetres.
pub(super) struct StairFit {
    pub(super) angle: f64,
    /// Box of the stair at origin zero.
    pub(super) lo: (f64, f64),
    pub(super) hi: (f64, f64),
}

pub(super) fn stair_def(spec: &ProjectSpec, angle: f64) -> GStair {
    GStair {
        origin: (0.0, 0.0),
        angle,
        shape: Shape::U {
            left: true,
            first: None,
            gap: rule("core.stair_gap"),
        },
        width: spec.stair_width,
        max_riser: spec.max_riser,
        landing: spec.stair_width,
        // the core is fitted to the tallest storey; each storey lays its own stair
        height: spec.tallest_storey(),
    }
}

pub(super) fn stair_box(g: &GStair) -> Result<((f64, f64), (f64, f64)), GenError> {
    let st = g
        .def()
        .layout()
        .map_err(|e| infeasible(format!("stair: {e:?}")))?;
    let mut lo = (f64::MAX, f64::MAX);
    let mut hi = (f64::MIN, f64::MIN);
    let rings = st
        .flights
        .iter()
        .map(|f| f.ring(st.tread))
        .chain(st.landings.iter().map(|l| l.ring.clone()));
    for r in rings {
        for p in r {
            lo = (lo.0.min(p.x), lo.1.min(p.y));
            hi = (hi.0.max(p.x), hi.1.max(p.y));
        }
    }
    Ok((lo, hi))
}

/// Flights along the depth when the band is deep enough, else along the facade.
pub(super) fn fit_stair(spec: &ProjectSpec, ds: Mm) -> Result<(StairFit, Mm), GenError> {
    let room = ds - door_zone() - stair_margin();
    let mut need = Mm::MAX;
    for angle in [std::f64::consts::FRAC_PI_2, 0.0] {
        let (lo, hi) = stair_box(&stair_def(spec, angle))?;
        let (bw, bh) = (mm(hi.0 - lo.0), mm(hi.1 - lo.1));
        if bh <= room {
            return Ok((StairFit { angle, lo, hi }, bw + 2 * stair_margin()));
        }
        need = need.min(bh);
    }
    // the band the stair needs, rounded up to a centimetre, named in the field the spec used
    let band = (need + door_zone() + stair_margin() + 9) / 10 * 10;
    let msg = match spec.depth {
        Some(d) => format!(
            "footprint.depth {d} m is too shallow for the stair: at least {:.2} m (a {:.2} m \
             band each side of the {} m corridor), or lower floor_height or stair.width",
            2.0 * m(band) + spec.corridor,
            m(band),
            spec.corridor
        ),
        None => format!(
            "band_depth {} m is too shallow for the stair: at least {:.2} m, or lower \
             floor_height or stair.width",
            m(ds),
            m(band)
        ),
    };
    Err(infeasible(msg))
}

/// The stair of a narrow plan lies in the middle strip, flights along the facade: the
/// orientation with the shallower box, and the strip depth and stair length it needs.
fn fit_strip(spec: &ProjectSpec) -> Result<(StairFit, Mm, Mm), GenError> {
    let mut best: Option<(StairFit, Mm, Mm)> = None;
    for angle in [0.0, std::f64::consts::FRAC_PI_2] {
        let (lo, hi) = stair_box(&stair_def(spec, angle))?;
        let (bw, bh) = (mm(hi.0 - lo.0), mm(hi.1 - lo.1));
        let strip = (bh + 2 * stair_margin()).max(mm(spec.corridor));
        if best.as_ref().is_none_or(|b| strip < b.1) {
            best = Some((StairFit { angle, lo, hi }, strip, bw + 2 * stair_margin()));
        }
    }
    best.ok_or_else(|| infeasible("no stair orientation"))
}

/// `dn`: depth of the open bay of a deep plan.
/// `room`: the band length the footprint leaves beside the core of a deep plan.
fn programs(spec: &ProjectSpec, d: Mm, plan: PlanKind, dn: Mm, room: Mm) -> Result<Vec<Program>, GenError> {
    let mut lists: Vec<Vec<Unit>> = vec![];
    if plan == PlanKind::Deep {
        if spec.typology == Typology::ApartmentSection {
            return Err(infeasible(
                "footprint.depth past two bands makes an open bay: apartments need two lit \
                 bands, so keep the depth within them",
            ));
        }
        for k in 0..spec.floors as usize {
            let (mut bay, mut rest): (Vec<RoomReq>, Vec<RoomReq>) =
                expand(&spec.storey_rooms(k)).into_iter().map(|r| RoomReq { count: 1, ..r }).partition(bay_room);
            // a band longer than the footprint hands its largest rooms that need no facade of
            // their own to the bay, which has the length the band lacks
            let long = |rest: &[RoomReq]| room_units(rest, d, spec).iter().map(|u| u.len).sum::<Mm>() > room;
            while long(&rest) {
                let Some(i) = (0..rest.len()).filter(|&i| bay_fit(&rest[i])).max_by(|&a, &b| rest[a].area.total_cmp(&rest[b].area))
                else {
                    break;
                };
                bay.push(rest.remove(i));
            }
            let mut units = room_units(&rest, d, spec);
            for r in expand(&bay) {
                let mut u = full_depth(&r, dn);
                u.hall = true;
                units.push(u);
            }
            lists.push(units);
        }
    } else if spec.typology == Typology::ApartmentSection {
        let mut units = vec![];
        for a in &spec.apartments {
            let u = apartment_unit(a, d, spec.ru)?;
            for _ in 0..a.count {
                units.push(u.clone());
            }
        }
        // a typology with periodic storeys (a refuge floor) gives those storeys their own rooms
        if spec.model.as_ref().is_some_and(|m| !m.periodic.is_empty()) {
            for k in 0..spec.floors as usize {
                lists.push(match spec.periodic_at(k) {
                    Some(rooms) => room_units(&rooms, d, spec),
                    None => units.clone(),
                });
            }
        } else {
            lists.push(units);
        }
    } else {
        // the skeleton first: the wet stack is the same on every storey it passes
        let sk = super::stack::skeleton(spec);
        for rooms in &sk.storeys {
            lists.push(room_units(rooms, d, spec));
        }
    }
    Ok(lists
        .into_iter()
        .map(|units| {
            let (segs, load) = assign(&units, plan);
            Program { units, segs, load }
        })
        .collect())
}

/// Side lengths: the longest segment of a side, plus a filler wide enough to be a room.
fn side_lengths(progs: &[Program]) -> (Mm, Mm) {
    let side = |a: usize, b: usize| {
        let mut len = progs
            .iter()
            .map(|p| p.load[a].max(p.load[b]))
            .max()
            .unwrap_or(0);
        let short = progs.iter().any(|p| {
            [a, b]
                .iter()
                .any(|&s| len - p.load[s] > 0 && len - p.load[s] < min_fill())
        });
        if short {
            len += min_fill();
        }
        len
    };
    (side(0, 1), side(2, 3))
}

#[derive(Clone, Copy)]
struct Frame {
    w: Mm,
    d: Mm,
    ds: Mm,
    c: Mm,
    k0: Mm,
    k1: Mm,
    left: Mm,
    right: Mm,
    lift_w: Mm,
    /// Narrow plan: the stair room's length in the middle strip from x 0; the rest of the
    /// strip is the corridor.
    strip: Option<Mm>,
    /// Deep plan: the stair room's width at the core start, all of the core in the south
    /// band; the bay north of the corridor is one segment over the whole width.
    bay: Option<Mm>,
    /// Core plan: lengths of the end stair rooms at x 0 and at the far end; 0 for none.
    ew: Mm,
    ee: Mm,
}

fn cell(x0: Mm, y0: Mm, x1: Mm, y1: Mm, kind: RoomKind, name: &str, owner: PRole) -> Cell {
    Cell {
        x0,
        y0,
        x1,
        y1,
        kind,
        name: name.to_string(),
        owner,
        asked: None,
        grown: false,
    }
}

fn floor_cells(spec: &ProjectSpec, f: &Frame, p: &Program, next_apt: &mut u32) -> Vec<Cell> {
    let ru = spec.ru;
    let nm = |k| names::kind_name(k, ru);
    let pubr = PRole::Public;
    let x_corr = match f.strip {
        Some(sx) if spec.has_stair() => sx,
        _ => 0,
    };
    let mut cells = vec![cell(
        x_corr,
        f.ds,
        f.w,
        f.ds + f.c,
        RoomKind::Corridor,
        nm(RoomKind::Corridor),
        pubr,
    )];
    if x_corr > 0 {
        let k = RoomKind::Stair;
        cells.push(cell(0, f.ds, x_corr, f.ds + f.c, k, nm(k), pubr));
    }
    let (rest_kind, rest_name) = (spec.core_rest.0, spec.core_rest.1.as_str());
    if let Some(sw) = f.bay {
        // deep plan: stair, then lift, side by side on the south facade
        let sx = f.k0 + sw;
        if sw > 0 {
            let (k, n) = if spec.has_stair() {
                (RoomKind::Stair, nm(RoomKind::Stair))
            } else {
                (rest_kind, rest_name)
            };
            cells.push(cell(f.k0, 0, sx, f.ds, k, n, pubr));
        }
        if sx < f.k1 {
            let k = if spec.lift { RoomKind::Lift } else { rest_kind };
            let n = if spec.lift { nm(k) } else { rest_name };
            cells.push(cell(sx, 0, f.k1, f.ds, k, n, pubr));
        }
    } else if f.k1 > f.k0 {
        // without a stair (one storey) the core holds only the lift and its neighbour
        let (k, n) = if spec.has_stair() {
            (RoomKind::Stair, nm(RoomKind::Stair))
        } else {
            (rest_kind, rest_name)
        };
        cells.push(cell(f.k0, 0, f.k1, f.ds, k, n, pubr));
    }
    let yn = f.ds + f.c;
    let core = f.k1 > f.k0 && f.bay.is_none();
    if core && spec.lift {
        let lx = f.k0 + f.lift_w;
        cells.push(cell(
            f.k0,
            yn,
            lx,
            f.d,
            RoomKind::Lift,
            nm(RoomKind::Lift),
            pubr,
        ));
        if lx < f.k1 {
            let k = RoomKind::Technical;
            cells.push(cell(lx, yn, f.k1, f.d, k, nm(k), pubr));
        }
    } else if core {
        cells.push(cell(f.k0, yn, f.k1, f.d, rest_kind, rest_name, pubr));
    }
    // end stairs on the facade side, the rest of the core's kind behind them
    for (a, b) in [(0, f.ew), (f.w - f.ee, f.w)] {
        if b > a {
            let k = RoomKind::Stair;
            cells.push(cell(a, 0, b, f.ds, k, nm(k), pubr));
            cells.push(cell(a, yn, b, f.d, rest_kind, rest_name, pubr));
        }
    }
    for (s, list) in p.segs.iter().enumerate() {
        let north = s % 2 == 1;
        let (x_start, len) = match (s, f.bay) {
            (1, Some(_)) => (0, f.w),
            (3, Some(_)) => continue,
            (0 | 1, _) => (f.ew, f.left),
            _ => (f.k1, f.right),
        };
        let used: Mm = list.iter().map(|&i| p.units[i].len).sum();
        let extra = spread(spec, &p.units, list, len - used);
        let mut x = x_start;
        for (&i, &e) in list.iter().zip(&extra) {
            let u = &p.units[i];
            let owner = if u.apartment {
                *next_apt += 1;
                PRole::Apartment(*next_apt)
            } else {
                pubr
            };
            // a widened unit stretches every piece in proportion along the facade
            let at = |v: Mm| x + v + v * e / u.len.max(1);
            for pc in &u.pieces {
                let (y0, y1) = if north {
                    (yn + pc.v0, yn + pc.v1)
                } else {
                    (f.ds - pc.v1, f.ds - pc.v0)
                };
                cells.push(Cell {
                    x0: at(pc.u0),
                    y0,
                    x1: at(pc.u1),
                    y1,
                    kind: pc.kind,
                    name: pc.name.clone(),
                    owner,
                    asked: pc.asked,
                    grown: pc.asked.is_some_and(|a| {
                        e > 0 || m(pc.u1 - pc.u0) * m(pc.v1 - pc.v0) > a * 1.01
                    }),
                });
            }
            x += u.len + e;
        }
        if x < x_start + len {
            fill_spare(spec, &mut cells, (x, x_start + len), north, f);
        }
    }
    cells
}

/// How much each unit of a segment widens: the spare spreads over the units that may grow, in
/// proportion to their length, unless it is long enough to be a room and more than they may
/// take. Never a sliver: a short spare always spreads when anything may grow.
fn spread(spec: &ProjectSpec, units: &[Unit], list: &[usize], spare: Mm) -> Vec<Mm> {
    let mut out = vec![0; list.len()];
    let grow: Vec<usize> = (0..list.len()).filter(|&k| units[list[k]].grows()).collect();
    let total: Mm = grow.iter().map(|&k| units[list[k]].len).sum();
    if spare <= 0 || total == 0 {
        return out;
    }
    // growth within the area tolerance of the brief; more becomes the programme's spare room
    let share = spec.spare.grow_share.min(grow_tolerance());
    // an open hall takes the whole bay: its area follows the footprint, not the brief
    let halls = grow.iter().all(|&k| units[list[k]].hall);
    let room = !halls && spare >= min_unit() && (spare as f64) > share * total as f64;
    if room {
        return out;
    }
    let mut given = 0;
    for &k in &grow {
        let e = spare * units[list[k]].len / total;
        out[k] = e;
        given += e;
    }
    if let Some(&k) = grow.last() {
        out[k] += spare - given;
    }
    out
}

/// Band length no asked room took and none could grow into: the programme's spare room, or
/// storage when it is short.
fn fill_spare(spec: &ProjectSpec, cells: &mut Vec<Cell>, (x, end): (Mm, Mm), north: bool, f: &Frame) {
    let spare = end - x;
    let yn = f.ds + f.c;
    let (y0, y1) = if north { (yn, f.d) } else { (0, f.ds) };
    let parks = spec
        .rooms
        .iter()
        .chain(&spec.ground_rooms)
        .any(|r| r.kind == RoomKind::Parking);
    let (k, name) = if spare >= min_unit() && f.bay.is_some() {
        // beside a parking deck more spaces along the drive aisle; else rooms for the plant
        let k = if parks {
            RoomKind::Parking
        } else {
            RoomKind::Technical
        };
        (k, names::kind_name(k, spec.ru).to_string())
    } else if spare >= min_unit() {
        let n = &spec.spare.name;
        (spec.spare.kind, if spec.ru { &n.1 } else { &n.0 }.clone())
    } else {
        let k = RoomKind::Storage;
        (k, names::kind_name(k, spec.ru).to_string())
    };
    cells.push(cell(x, y0, end, y1, k, &name, PRole::Public));
}

/// Numbers of one floor: public rooms `{floor}{nn}`, with three digits in the whole project
/// once a floor has more than 99, so 1 + 001 never meets 10 + 01; apartment rooms `{apt}.{k}`.
fn numbers(cells: &[Cell], floor: usize, digits: usize) -> Vec<String> {
    let mut public = 0;
    let mut per: std::collections::BTreeMap<u32, u32> = Default::default();
    cells
        .iter()
        .map(|c| match c.owner {
            PRole::Public => {
                public += 1;
                format!("{}{:0digits$}", floor + 1, public)
            }
            PRole::Apartment(a) => {
                let k = per.entry(a).or_insert(0);
                *k += 1;
                format!("{a}.{k}")
            }
        })
        .collect()
}

fn axes(start: Mm, end: Mm, grid: Mm, out: &mut Vec<Mm>) {
    let len = end - start;
    if len <= 0 {
        return;
    }
    let n = (len + grid - 1) / grid;
    for i in 0..n {
        out.push(start + len * i / n);
    }
}

/// Axis lines across x and y; the bay of a deep plan gets axes at most a grid apart.
fn grid_lines(f: &Frame, grid: f64) -> (Vec<Mm>, Vec<Mm>) {
    let g = mm(grid);
    let mut xs = vec![];
    axes(0, f.k0, g, &mut xs);
    xs.push(f.k0);
    xs.push(f.k1);
    axes(f.k1, f.w, g, &mut xs);
    xs.push(f.w);
    xs.sort_unstable();
    xs.dedup();
    let yn = f.ds + f.c;
    let mut ys = vec![0, f.ds];
    if f.bay.is_some() {
        axes(yn, f.d, g, &mut ys);
    } else {
        ys.push(yn);
    }
    ys.push(f.d);
    (xs, ys)
}

/// Columns of a deep plan: on every inner axis crossing in the bay, clear of every wall.
fn bay_columns(f: &Frame, grid: f64, h: f64, l: &GLayout) -> Vec<crate::GColumn> {
    if f.bay.is_none() {
        return vec![];
    }
    let (xs, ys) = grid_lines(f, grid);
    let yn = f.ds + f.c;
    let mut out = vec![];
    for &x in xs.iter().filter(|&&x| x > 0 && x < f.w) {
        for &y in ys.iter().filter(|&&y| y > yn && y < f.d) {
            let p = (m(x), m(y));
            let clear = l.walls.iter().all(|w| seg_dist(p, w.a, w.b) >= column_clear());
            if clear {
                out.push(crate::GColumn {
                    at: p,
                    rotation: 0.0,
                    section: column(h),
                });
            }
        }
    }
    out
}

/// Columns of a storey. A frame structure (`design.structure` frame or timber_frame) stands a
/// column on every axis crossing, walls fill between; stair holes, shafts and door openings
/// stay clear. Bearing-wall systems get columns only in a deep bay.
fn storey_columns(f: &Frame, spec: &ProjectSpec, l: &GLayout, stair: Option<&[(f64, f64)]>) -> Vec<crate::GColumn> {
    if !spec.design.structure.is_frame() {
        return bay_columns(f, spec.grid, l.height, l);
    }
    let (xs, ys) = grid_lines(f, spec.grid);
    let half = column(l.height).0 / 2.0 + column_clear();
    let in_ring = |r: &[(f64, f64)], p: (f64, f64)| {
        let (x0, x1) = r.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
        let (y0, y1) = r.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
        p.0 > x0 - half && p.0 < x1 + half && p.1 > y0 - half && p.1 < y1 + half
    };
    let in_door = |p: (f64, f64)| {
        l.openings.iter().filter(|o| o.door).any(|o| {
            let w = &l.walls[o.wall];
            let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
            if len <= 0.0 || seg_dist(p, w.a, w.b) > half {
                return false;
            }
            let t = ((p.0 - w.a.0) * (w.b.0 - w.a.0) + (p.1 - w.a.1) * (w.b.1 - w.a.1)) / len;
            t > o.along - half && t < o.along + o.width + half
        })
    };
    let mut out = vec![];
    for &x in xs.iter().filter(|&&x| x >= 0 && x <= f.w) {
        for &y in ys.iter().filter(|&&y| y >= 0 && y <= f.d) {
            let p = (m(x), m(y));
            let blocked = l.slab_holes.iter().chain(stair.map(|s| s.to_vec()).as_ref()).any(|h| in_ring(h, p))
                || l.shafts.iter().any(|s| in_ring(&s.ring, p))
                || in_door(p);
            if !blocked {
                out.push(crate::GColumn { at: p, rotation: 0.0, section: column(l.height) });
            }
        }
    }
    out
}

/// Square column section.
/// Square section of a column of a storey `h` high: the grid's column, grown in 5 cm steps until
/// height over side is within `structure.column_slenderness_max`.
pub(super) fn column(h: f64) -> (f64, f64) {
    let per_m = (1.0 / rule("structure.column_step")).round();
    let need = (h / rule("structure.column_slenderness_max") * per_m - 1e-9).ceil() / per_m;
    let s = rule("grid.column").max(need);
    (s, s)
}

/// Half the section plus the gap a wall needs beside a column.
fn column_clear() -> f64 {
    rule("grid.column_clear")
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

fn grids(f: &Frame, grid: f64, ru: bool) -> Vec<GGrid> {
    let (xs, ys) = grid_lines(f, grid);
    let (w, d) = (m(f.w), m(f.d));
    let mut out = vec![];
    for (i, x) in xs.iter().enumerate() {
        out.push(GGrid {
            name: (i + 1).to_string(),
            a: (m(*x), -grid_overrun()),
            b: (m(*x), d + grid_overrun()),
        });
    }
    for (i, y) in ys.iter().enumerate() {
        out.push(GGrid {
            name: ddc_geom::axes::axis_letter(i, ru),
            a: (-grid_overrun(), m(*y)),
            b: (w + grid_overrun(), m(*y)),
        });
    }
    out
}

fn turned(p: &mut (f64, f64), q: u8, o: (f64, f64)) {
    let (x, y) = *p;
    let (x, y) = match q % 4 {
        0 => (x, y),
        1 => (-y, x),
        2 => (-x, -y),
        _ => (y, -x),
    };
    *p = (x + o.0, y + o.1);
}

/// Moves the plan points of a storey's fit-out records as [`place`] moves its layout.
pub(super) fn place_fit(fit: &mut [super::RoomFit], quarter: u8, o: (f64, f64)) {
    for f in fit {
        f.stack.iter_mut().for_each(|p| turned(p, quarter, o));
        f.turning.iter_mut().for_each(|(p, _)| turned(p, quarter, o));
    }
}

pub(super) fn shift(l: &mut GLayout, o: (f64, f64)) {
    place(l, 0, o);
}

/// Turns the layout by `quarter` quarter turns counter-clockwise about the origin, then moves
/// it by `o`. Every point and angle of every element goes along.
pub(super) fn place(l: &mut GLayout, quarter: u8, o: (f64, f64)) {
    let q = quarter % 4;
    let s = |p: &mut (f64, f64)| turned(p, q, o);
    let turn = f64::from(q) * std::f64::consts::FRAC_PI_2;
    for c in &mut l.columns {
        s(&mut c.at);
        c.rotation += turn;
    }
    for f in &mut l.fixtures {
        s(&mut f.at);
        f.rotation += turn;
    }
    for st in &mut l.stairs {
        st.angle += turn;
    }
    for w in &mut l.walls {
        s(&mut w.a);
        s(&mut w.b);
    }
    for r in &mut l.rooms {
        s(&mut r.point);
        r.polygon.iter_mut().for_each(s);
    }
    l.slab.iter_mut().for_each(s);
    l.slab_holes.iter_mut().flatten().for_each(s);
    for st in &mut l.stairs {
        s(&mut st.origin);
    }
    l.shafts
        .iter_mut()
        .flat_map(|x| x.ring.iter_mut())
        .for_each(s);
    for g in &mut l.grids {
        s(&mut g.a);
        s(&mut g.b);
    }
}

fn frame(
    spec: &ProjectSpec,
    progs: &[Program],
    core: Mm,
    (ds, c): (Mm, Mm),
    strip: Option<Mm>,
    bay: Option<(Mm, Mm)>,
    (ew, ee): (Mm, Mm),
) -> Result<Frame, GenError> {
    let (mut left, mut right) = side_lengths(progs);
    let mut core = core;
    let mut d = 2 * ds + c;
    if let Some((_, dn)) = bay {
        d = ds + c + dn;
        let raw = |s: usize| progs.iter().map(|p| p.load[s]).max().unwrap_or(0);
        left = raw(0);
        right = raw(2);
        // the bay is one segment: the south band widens to its halls
        let north = raw(1);
        if left + core + right < north {
            right = north - left - core;
        }
    }
    if strip.is_some() {
        // the narrow plan's bands span the whole width; the stair needs room for a corridor
        let w = spec.width.map(mm).unwrap_or(0);
        // no filler margin: a short spare here spreads over the rooms of the band
        let raw = progs
            .iter()
            .map(|p| p.load[0].max(p.load[1]))
            .max()
            .unwrap_or(0);
        let need = raw.max(strip.unwrap_or(0) + min_unit());
        if need > w {
            return Err(infeasible(format!(
                "footprint.width {} m is too small even with the stair in the middle strip: \
                 the program needs {:.2} m; widen it, add a floor or drop rooms",
                m(w),
                m((need + 9) / 10 * 10)
            )));
        }
        return Ok(Frame {
            w,
            d: 2 * ds + c,
            ds,
            c,
            k0: w,
            k1: w,
            left: w,
            right: 0,
            lift_w: 0,
            strip,
            bay: None,
            ew: 0,
            ee: 0,
        });
    }
    let ends = ew + ee;
    if let Some(w) = spec.width {
        let spare = mm(w) - (left + core + right + ends);
        if spare < 0 {
            // rounded up to a centimetre: the width named here is one that fits
            let need = m((left + core + right + ends + 9) / 10 * 10);
            let stairs = if ends > 0 { format!(" + end stairs {:.2}", m(ends)) } else { String::new() };
            return Err(infeasible(format!(
                "footprint.width {w} m is too small: the program needs {need:.2} m (left \
                 {:.2} + core {:.2} + right {:.2}{stairs}); widen it, deepen band_depth or drop rooms",
                m(left),
                m(core),
                m(right)
            )));
        }
        if spare >= 2 * min_fill() {
            left += spare / 2;
            right += spare - spare / 2;
        } else {
            core += spare;
        }
    }
    let lift_w = if core >= lift_room_for(spec, ds) + min_fill() {
        lift_room_for(spec, ds)
    } else {
        core
    };
    Ok(Frame {
        w: left + core + right + ends,
        d,
        ds,
        c,
        k0: ew + left,
        k1: ew + left + core,
        left,
        right,
        lift_w,
        strip: None,
        bay: bay.map(|b| b.0),
        ew,
        ee,
    })
}

/// What every storey shares: the stair placed in the core, its slab hole, the lift shaft and
/// the window size.
struct Shared {
    /// The flights and their slab holes, the core's first; none for a single storey.
    stairs: Vec<(GStair, Vec<(f64, f64)>)>,
    /// The shafts of the lift bank, side by side.
    shafts: Vec<Vec<(f64, f64)>>,
    /// The service bays after the lift shafts, with what each carries.
    bays: Vec<(Vec<(f64, f64)>, Vec<crate::ShaftKind>)>,
    win: WinSize,
}

fn shared_parts(spec: &ProjectSpec, f: &Frame, fit: Option<&StairFit>, ds: Mm, plan: Vec<Vec<crate::ShaftKind>>) -> Shared {
    let win = match spec.typology {
        Typology::House | Typology::ApartmentSection => res_win(),
        Typology::Office | Typology::School => work_win(),
    };
    let hole = hole_clear();
    // the room the stair stands in: the core's facade half, or the strip's stair end; then
    // the end stair rooms of a core plan
    let mut rooms = vec![match (f.strip, f.bay) {
        (Some(sx), _) => (0.0, m(sx), m(f.ds), m(f.ds + f.c)),
        (None, Some(sw)) => (m(f.k0), m(f.k0 + sw), 0.0, m(ds)),
        (None, None) => (m(f.k0), m(f.k1), 0.0, m(ds)),
    }];
    for (a, b) in [(0, f.ew), (f.w - f.ee, f.w)] {
        if b > a {
            rooms.push((m(a), m(b), 0.0, m(ds)));
        }
    }
    // the stair box sits against the facade, centred in its room
    let stairs = fit.map_or(vec![], |fit| rooms.iter().map(|&(rx0, rx1, ry0, ry1)| {
        let (bw, bd) = (fit.hi.0 - fit.lo.0, fit.hi.1 - fit.lo.1);
        let target = match f.strip {
            Some(_) => (m(stair_margin()), ry0 + (ry1 - ry0 - bd) / 2.0),
            None => (rx0 + (rx1 - rx0 - bw) / 2.0, m(stair_margin())),
        };
        let mut stair = stair_def(spec, fit.angle);
        stair.origin = (target.0 - fit.lo.0, target.1 - fit.lo.1);
        let x0 = (target.0 - hole).max(rx0);
        let x1 = (target.0 + bw + hole).min(rx1);
        let y0 = (target.1 - hole).max(ry0);
        let y1 = (target.1 + bd + hole).min(ry1);
        (stair, crate::layout::rect(x0, y0, x1, y1))
    }).collect());
    let (sw, sd, pitch) = (lift_shaft(spec).0, lift_shaft(spec).1, lift_shaft(spec).0 + rule("core.lift_wall"));
    // the bank centred in the lift room, its shafts a wall apart; past one row's cars a second
    // row stands at the far side of the room and faces the first across the lobby
    let nb = plan.len() as u32;
    let (row, places) = bank_rows(spec, f.ds);
    let span = sw + pitch * f64::from(row - 1 + nb);
    let at = |i: u32, far: bool| {
        let (x0, y0) = match f.bay {
            // the lift room follows the stair on the south facade; the first row at its back
            Some(s0) => (
                m(f.k0 + s0) + (m(f.k1 - f.k0 - s0) - span) / 2.0,
                if far { m(f.ds) - lift_inset() - sd } else { lift_inset() },
            ),
            None => (
                m(f.k0) + (m(f.lift_w) - span) / 2.0,
                if far { m(f.d) - lift_inset() - sd } else { m(f.ds + f.c) + lift_inset() },
            ),
        };
        let x = x0 + pitch * f64::from(i);
        crate::layout::rect(x, y0, x + sw, y0 + sd)
    };
    let bays = (0..nb).map(|i| (at(row + i, false), plan[i as usize].clone())).collect();
    let shafts = places.iter().map(|&(i, far)| at(i, far)).collect();
    Shared {
        stairs,
        shafts,
        bays,
        win,
    }
}

/// Builds every storey and the roof; each storey passes the layout validity check.
/// A core plan first; a given width too narrow for it takes the narrow plan, whose stair
/// lies in the middle strip, before the brief is refused with the core plan's reason.
pub fn build_project(spec: &ProjectSpec) -> Result<GProject, GenError> {
    let run = |s: &ProjectSpec| super::assembly::with_structure(s.design.structure, super::assembly::project_rating(s), || super::rules::scoped(&s.rules, || build_ruled(s)));
    let mut r = run(spec);
    // a plate too small for the whole bank round its core takes one car fewer at a time
    let mut cars = if spec.plate.is_some() && r.is_err() { super::rules::scoped(&spec.rules, || lift_plan(spec).cars) } else { 0 };
    while cars > 1 {
        cars -= 1;
        let mut s = spec.clone();
        if s.rules.set(super::rules::Layer::Brief, "core.lift_bank_max", f64::from(cars)).is_err() {
            break;
        }
        if let Ok(p) = run(&s) {
            r = Ok(p);
            break;
        }
    }
    let mut p = r?;
    names::localize(&mut p, spec.lang);
    Ok(p)
}

/// The bar by the `layout.strategy` rule: the band, free placement, or (auto) the free plan
/// when it passes the gates and scores better on the goals, else the band.
fn bar_or_free(spec: &ProjectSpec) -> Result<GProject, GenError> {
    let strategy = rule("layout.strategy") as usize;
    if strategy == 0 || super::plate::free_refusal(spec).is_some() {
        return build_bar(spec);
    }
    if strategy == 1 {
        return super::plate::build_free(spec);
    }
    // the band is compared unventilated (no goal reads the air system) and ventilated only when
    // kept: an edit the free plan wins rebuilds no band ducts
    let vent = |mut b: GProject| super::hvac::ventilate(spec, &mut b).map(|()| b);
    // basements are added after the storeys: the gates see the storeys alone
    let s = ProjectSpec { basements: 0, ..spec.clone() };
    let free_wins = |f: &GProject, b: &GProject| {
        super::check_project(&s, f).passed() && super::score::score(spec, f).compare(&super::score::score(spec, b)).is_lt()
    };
    match (build_bar_dry(spec), super::plate::build_free(spec)) {
        (Ok(b), Ok(f)) => {
            if free_wins(&f, &b) {
                return Ok(f);
            }
            // a band whose air system fails takes the ventilated band's own fallbacks
            Ok(match vent(b) {
                Ok(b) => b,
                Err(_) => match build_bar(spec) {
                    Ok(b) if !free_wins(&f, &b) => b,
                    _ => f,
                },
            })
        }
        (Ok(b), Err(_)) => vent(b).or_else(|_| build_bar(spec)),
        // a band that fails dry fails ventilated too
        (Err(_), Ok(f)) => Ok(f),
        (Err(e), Err(_)) => Err(e),
    }
}

fn build_ruled(spec: &ProjectSpec) -> Result<GProject, GenError> {
    let mut base = spec.clone();
    base.origin = (0.0, 0.0);
    // the plan is drawn plain and the design applied after, but the structure is the plan's
    // own: columns and wall assemblies are set while the storeys are drawn
    base.design = super::Design { structure: spec.design.structure, ..super::Design::default() };
    let d = &spec.design;
    let mut p = if spec.pins.is_some() {
        super::plate::build_free(&base)?
    } else if spec.plate.is_some() {
        super::plate::build(&base)?
    } else if d.massing == super::Massing::Bar && d.step_at(spec.floors).is_none() {
        bar_or_free(&base)?
    } else {
        super::massing::compose(spec, &base)?
    };
    if d.curve > 0.0 || d.curve_north > 0.0 {
        super::curve::apply(d.curve, d.curve_north, &mut p)?;
    }
    super::facade::apply(spec, &mut p)?;
    if d.atrium {
        super::massing::roof_court(&mut p)?;
    }
    super::basement::add(spec, &mut p)?;
    super::fire::protect_stairs(spec, &mut p);
    super::typology::apply(spec, &mut p);
    super::site::apply(spec, &mut p)?;
    if spec.origin != (0.0, 0.0) {
        let o = spec.origin;
        p.site.shift(o);
        for s in p.storeys.iter_mut().chain(p.basements.iter_mut()) {
            shift(&mut s.layout, o);
        }
        let rings = std::iter::once(&mut p.roof.ring)
            .chain(p.extra_roofs.iter_mut().map(|r| &mut r.ring))
            .chain(p.court.iter_mut())
            .chain(p.podium.iter_mut());
        for r in rings {
            r.iter_mut().for_each(|q| *q = (q.0 + o.0, q.1 + o.1));
        }
        for d in &mut p.hvac.ducts {
            d.points.iter_mut().for_each(|q| *q = (q.0 + o.0, q.1 + o.1, q.2));
        }
        for f in &mut p.hvac.plant {
            f.at = (f.at.0 + o.0, f.at.1 + o.1);
        }
    }
    super::shell::apply(spec, &mut p)?;
    super::anygeo::vault::apply(spec, &mut p);
    if spec.plate.is_some() {
        super::anygeo::apply(spec, &mut p)?;
    }
    super::anygeo::form_measures(&mut p);
    super::services(spec, &mut p);
    super::protect(spec, &mut p);
    super::coord::apply(spec, &mut p);
    super::openings::apply(spec, &mut p);
    super::railings::apply(spec, &mut p)?;
    // after the guards, which follow the whole roof edge
    super::fire::open_roof(&mut p);
    fit_slab_holes(&mut p);
    Ok(p)
}

/// A safety net, not a placement: every slab hole lies on its slab. One across the slab's edge
/// notches the outline instead (the part past the edge is open air), one wholly off it is
/// dropped; each is counted and named in the services issues (floor, hole, what it opens for),
/// so the element that put it there can be fixed.
fn fit_slab_holes(p: &mut GProject) {
    use ddc_geom::{poly, v2};
    let (mut notched, mut dropped) = (0.0, 0.0);
    let mut log = vec![];
    let levels = p.basements.iter_mut().enumerate().map(|(k, s)| (format!("basement {k}"), s));
    let levels: Vec<(String, &mut PStorey)> = levels.chain(p.storeys.iter_mut().enumerate().map(|(k, s)| (format!("floor {k}"), s))).collect();
    for (name, s) in levels {
        let l = &mut s.layout;
        if l.slab.len() < 3 {
            continue;
        }
        let mut keep = vec![];
        for (i, h) in std::mem::take(&mut l.slab_holes).into_iter().enumerate() {
            if h.iter().all(|q| crate::check::inside(&l.slab, *q)) {
                keep.push(h);
                continue;
            }
            let v = |r: &[(f64, f64)]| r.iter().map(|q| v2(q.0, q.1)).collect::<Vec<_>>();
            let n = h.len() as f64;
            let c = h.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0 / n, a.1 + q.1 / n));
            let what = if let Some(sh) = l.shafts.iter().find(|sh| crate::check::inside(&h, ring_centre(&sh.ring)) || crate::check::inside(&sh.ring, c)) {
                format!("{:?} shaft", sh.kind)
            } else if l.stairs.iter().any(|st| crate::check::inside(&h, st.origin)) {
                "a stair".to_string()
            } else {
                l.rooms.iter().find(|r| crate::check::inside(&r.polygon, c)).map_or("a riser or opening off every room".to_string(), |r| format!("a riser or opening in {}", r.name))
            };
            let off = poly::subtract(&[v(&h)], &[v(&l.slab)]).map(|x| poly::pieces_area(&x));
            let on = poly::area(&v(&h)).abs() - off.unwrap_or(0.0);
            let cut = poly::subtract(&[v(&l.slab)], &[v(&h)]);
            let done = match cut {
                Some(parts) if on > 1e-6 && parts.len() == 1 && parts[0].1.is_empty() => {
                    l.slab = parts[0].0.iter().map(|q| (q.x, q.y)).collect();
                    notched += 1.0;
                    "notched into the slab's outline"
                }
                _ => {
                    dropped += 1.0;
                    "dropped"
                }
            };
            log.push(format!("{name}: slab hole {i} for {what} at ({:.2}, {:.2}) is not wholly on the slab: {done}", c.0, c.1));
        }
        l.slab_holes = keep;
    }
    if notched + dropped > 0.0 {
        p.metrics.extend([("slab_holes_notched", notched), ("slab_holes_dropped", dropped)]);
        p.services.issues.extend(log);
    }
}

fn ring_centre(r: &[(f64, f64)]) -> (f64, f64) {
    let n = r.len().max(1) as f64;
    r.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0 / n, a.1 + q.1 / n))
}

/// One double-loaded bar at the origin, the plan kind chosen by the brief.
pub(super) fn build_bar(spec: &ProjectSpec) -> Result<GProject, GenError> {
    bar(spec, true, true)
}

/// A bar without ventilation, for a composition that ventilates the whole.
pub(super) fn build_bar_dry(spec: &ProjectSpec) -> Result<GProject, GenError> {
    bar(spec, false, true)
}

/// A dry bar with its core stair alone, for a stepped block whose parts share that stair.
pub(super) fn build_bar_one_stair(spec: &ProjectSpec) -> Result<GProject, GenError> {
    bar(spec, false, false)
}

fn bar(spec: &ProjectSpec, vent: bool, ends: bool) -> Result<GProject, GenError> {
    if spec.hall_depth.is_some() {
        return build_plan(spec, PlanKind::Deep, vent, 0);
    }
    let core = if ends && spec.has_stair() {
        core_with_ends(spec, vent)
    } else {
        build_plan(spec, PlanKind::Core, vent, 0)
    };
    match core {
        Err(e) if spec.width.is_some() && spec.has_stair() && !spec.lift => {
            build_plan(spec, PlanKind::Narrow, vent, 0).map_err(|n| match (e, n) {
                (GenError::Infeasible(a), GenError::Infeasible(b)) => {
                    infeasible(format!("{a}; with the stair in the middle strip: {b}"))
                }
                (e, _) => e,
            })
        }
        r => r,
    }
}

/// The core plan with the stairs its occupants need: the core stair alone, then one at the
/// west corridor end, then at both, while a storey above ground falls short of exits, stair
/// width, exit spacing, travel or dead ends (`egress.stairs_max` in all); stairs still too
/// narrow then widen up to the stair width rule's range. A variant that does not fit keeps
/// the last that did.
fn core_with_ends(spec: &ProjectSpec, vent: bool) -> Result<GProject, GenError> {
    let mut p = build_plan(spec, PlanKind::Core, false, 0)?;
    let mut ends = 0;
    let most = rule("egress.stairs_max") as usize;
    for e in 1..=2usize {
        if 1 + e > most || !super::fire::stairs_short(spec, &p) {
            break;
        }
        match build_plan(spec, PlanKind::Core, false, e) {
            Ok(q) => {
                p = q;
                ends = e;
            }
            Err(_) => break,
        }
    }
    if let Some(need) = super::fire::stair_need(spec, &p) {
        let id = super::spec::stair_width_rule(spec.typology);
        let cap = super::rules::def(id).map_or(spec.stair_width, |d| d.max);
        let wide = ProjectSpec { stair_width: need, ..spec.clone() };
        if wide.stair_width <= cap && wide.stair_width > spec.stair_width {
            if let Ok(q) = build_plan(&wide, PlanKind::Core, false, ends) {
                p = q;
            }
        }
    }
    if vent {
        super::hvac::ventilate(spec, &mut p)?;
    }
    Ok(p)
}

/// `ends`: end stairs of a core plan, 1 at the west end, 2 at both.
fn build_plan(spec: &ProjectSpec, plan: PlanKind, vent: bool, ends: usize) -> Result<GProject, GenError> {
    let narrow = plan == PlanKind::Narrow;
    let (mut ds, mut c) = (mm(spec.band_depth), mm(spec.corridor));
    let mut hall = mm(spec.hall_depth.unwrap_or(0.0));
    // a bank past one row's cars faces a second row across its lobby: the lift band deepens to
    // hold both where the footprint leaves it free (a derived depth, or the open bay behind)
    if lift_row_full(spec) && !narrow {
        let need = facing_depth(spec);
        let most = super::rules::def("depth.band").map_or(12.0, |d| d.max);
        if need > ds && need <= mm(most) {
            match plan {
                PlanKind::Deep if hall - (need - ds) >= need => {
                    hall -= need - ds;
                    ds = need;
                }
                PlanKind::Core if spec.depth.is_none() => ds = need,
                _ => {}
            }
        }
    }
    let mut strip = None;
    let mut end_room = 0;
    let (fit, mut core) = if narrow {
        let (fit, sc, len) = fit_strip(spec)?;
        // the footprint depth stays: the deeper strip comes out of both bands
        ds -= (sc - c + 1) / 2;
        c = sc;
        strip = Some(len);
        (Some(fit), 0)
    } else if spec.has_stair() {
        let (fit, stair_core) = fit_stair(spec, ds)?;
        end_room = stair_core;
        (Some(fit), stair_core.max(rule_mm("core.min_width")))
    } else {
        (None, 0)
    };
    let mut bay = None;
    if plan == PlanKind::Deep {
        // stair and lift side by side in the south band
        let sw = if spec.has_stair() { core } else { 0 };
        core = sw + if spec.lift { lift_room_for(spec, ds) } else { 0 };
        bay = Some((sw, hall));
    } else if spec.lift && !narrow {
        core = core.max(lift_room_for(spec, ds));
    }
    let progs = programs(spec, ds, plan, hall, spec.width.map_or(Mm::MAX, |w| mm(w) - core))?;
    let end = |i: usize| if plan == PlanKind::Core && ends > i { end_room } else { 0 };
    let f = frame(spec, &progs, core, (ds, c), strip, bay, (end(0), end(1)))?;
    let have = match f.bay {
        Some(s0) => f.k1 - f.k0 - s0,
        None => f.lift_w,
    };
    let (level, plan) = if spec.lift { pick_bays(spec, have) } else { (0, vec![]) };
    let sh = shared_parts(spec, &f, fit.as_ref(), ds, plan);
    let n = spec.floors as usize;
    let mut next_apt = 0;
    let yn = f.ds + f.c;
    let mut cuts: Vec<Vec<Mm>> = vec![];
    let floors: Vec<Vec<Cell>> = (0..n)
        .map(|k| {
            let p = progs.get(k).unwrap_or(&progs[0]);
            let mut cells = floor_cells(spec, &f, p, &mut next_apt);
            // fire walls across a bar whose corridor runs its whole length
            let whole = f.strip.is_none() && f.bay.is_none();
            cuts.push(if whole { super::fire::cut_cells(spec, &mut cells, f.d, f.ds, yn) } else { vec![] });
            cells
        })
        .collect();
    let apartments = next_apt;
    // room numbers take three digits project-wide once a floor has more than 99 rooms
    let public = |c: &Vec<Cell>| c.iter().filter(|x| x.owner == PRole::Public).count();
    let digits = if floors.iter().any(|c| public(c) > 99) {
        3
    } else {
        2
    };
    let rules = super::fit_rules::rules_for(spec.furnish.as_ref())
        .map_err(|e| infeasible(format!("fit-out rules: {}", e.join("; "))))?;
    let mut storeys = Vec::with_capacity(n);
    let mut dwell = std::collections::HashSet::new();
    for (k, cells) in floors.into_iter().enumerate() {
        storeys.push(storey(spec, &f, &sh, &cells, (k, cuts[k].as_slice()), digits, (&rules, &mut dwell))?);
    }
    let (w, d) = (m(f.w), m(f.d));
    let rooms: usize = storeys.iter().map(|s| s.rooms.len()).sum();
    let mut metrics = vec![
        ("floors", n as f64),
        ("width", w),
        ("depth", d),
        ("gross_floor_area", w * d * n as f64),
        ("rooms", rooms as f64),
        ("apartments", f64::from(apartments)),
    ];
    if level > 0 {
        metrics.push(("core_shafts_cut", sh.bays.len() as f64));
    }
    let mut p = GProject {
        storeys,
        roof: roof_of(spec, w, d),
        extra_roofs: vec![],
        copies: 1,
        court: None,
        podium: None,
        footprint: (w, d),
        apartments,
        metrics,
        hvac: super::GHvac::default(),
        services: super::GServices::default(),
        basements: vec![],
        site: super::site::GSite::default(),
        fire: Default::default(),
        lifts: Default::default(),
        shell: None,
    };
    if vent {
        super::hvac::ventilate(spec, &mut p)?;
    }
    Ok(p)
}

/// Storey `k`: walls and openings from the tiling, the slab with the stair hole above the
/// ground, the flight up (from the top too when it runs on to the roof), the shaft, and the grid on the ground floor.
fn storey(
    spec: &ProjectSpec,
    f: &Frame,
    sh: &Shared,
    cells: &[Cell],
    (k, cuts): (usize, &[Mm]),
    digits: usize,
    (rules, dwell): (&super::fit_rules::Rules, &mut std::collections::HashSet<(u32, usize)>),
) -> Result<PStorey, GenError> {
    let n = spec.floors as usize;
    let h = spec.storey_height(k);
    let nums = numbers(cells, k, digits);
    let t = tile::tile(cells, nums, f.w, f.d, sh.win, k == 0)?;
    let mut l = t.layout;
    l.height = h;
    l.slab = crate::layout::rect(0.0, 0.0, m(f.w), m(f.d));
    let xs: Vec<f64> = cuts.iter().map(|&x| m(x)).collect();
    super::fire::fire_walls(spec, &mut l, &xs, (m(f.ds), m(f.ds + f.c)));
    super::fire::open_exits(spec, &mut l, &t.rooms, k);
    for (stair, hole) in &sh.stairs {
        if k > 0 {
            l.slab_holes.push(hole.clone());
        }
        if k + 1 < n || super::fire::stairs_to_roof(spec) {
            l.stairs.push(crate::GStair { height: h, ..stair.clone() });
        }
    }
    for ring in &sh.shafts {
        l.shafts.push(GShaft::new(ring.clone(), crate::ShaftKind::Lift));
    }
    for (ring, kinds) in &sh.bays {
        for kind in kinds {
            l.shafts.push(GShaft::new(ring.clone(), *kind));
        }
    }
    if k == 0 {
        l.grids = grids(f, spec.grid, spec.ru);
    }
    l.columns = storey_columns(f, spec, &l, sh.stairs.first().map(|s| s.1.as_slice()));
    let rooms: Vec<super::furnish::RoomIn<'_>> = cells
        .iter()
        .map(|c| super::furnish::RoomIn {
            rect: [m(c.x0), m(c.y0), m(c.x1), m(c.y1)],
            kind: c.kind,
            owner: c.owner,
            name: &c.name,
            tags: spec.room_tags.of(c.kind, &c.name),
            keep: vec![],
        })
        .collect();
    let (fixtures, fit) = super::furnish::furnish(&l, &rooms, h, spec.typology, rules, dwell);
    l.fixtures = fixtures;
    let caps = [
        (l.walls.len(), slots::OPENINGS - slots::WALLS, "walls"),
        (l.openings.len(), slots::ROOMS - slots::OPENINGS, "openings"),
        (l.rooms.len(), slots::SLAB - slots::ROOMS, "rooms"),
        (l.grids.len(), slots::COLUMNS - slots::GRIDS, "grid axes"),
        (l.columns.len(), slots::MAX_COLUMNS, "columns"),
        (l.fixtures.len(), slots::FIXTURES_MAX, "fixtures"),
    ];
    for (have, cap, what) in caps {
        if have > cap as usize {
            return Err(infeasible(format!(
                "{have} {what} on floor {}; at most {cap}: split the building",
                k + 1
            )));
        }
    }
    crate::check::validate_layout(&l).map_err(|e| infeasible(format!("floor {}: {e}", k + 1)))?;
    Ok(PStorey {
        name: names::level_name(k, spec.ru),
        elevation: spec.storey_elevation(k),
        height: h,
        layout: l,
        rooms: t.rooms,
        stair_room: cells.iter().position(|c| c.kind == RoomKind::Stair),
        fit,
        curtains: vec![],
        railings: vec![],
    })
}

fn roof_of(spec: &ProjectSpec, w: f64, d: f64) -> PRoof {
    let (ox, oy) = spec.origin;
    let pitched = spec.roof == RoofKind::Pitched;
    let ring = vec![(ox, oy), (ox + w, oy), (ox + w, oy + d), (ox, oy + d)];
    // gable, shed and barrel slope only some edges; hip and plain pitched slope all of them
    let edges = match (pitched, spec.roof_form) {
        (true, Some(form)) => super::footprint::edge_slopes(&ring, form, spec.roof_slope),
        _ => vec![],
    };
    PRoof {
        name: names::roof_name(spec.ru).to_string(),
        elevation: spec.building_height(),
        ring,
        slope: if pitched {
            spec.roof_slope
        } else {
            super::FLAT_FALL_DEG
        },
        overhang: if pitched { rule("massing.roof_overhang") } else { 0.0 },
        edges,
        vault: None,
        lift: 0.0,
        eave: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every room with a window of an apartment meets its hall over a door's run, and the hall
    /// is wide enough for its own entrance from the corridor: in towers, sections and hotels.
    #[test]
    fn every_window_room_of_an_apartment_meets_its_hall_over_a_door() {
        let need = mm(rule("room.door")) + 2 * rule_mm("facade.edge_clear");
        let mut seen = 0;
        for t in ["residential_tower", "apartment_section", "conference_hotel"] {
            let s = super::super::parse_spec(&serde_json::json!({"typology": t})).unwrap_or_else(|e| panic!("{t}: {e}"));
            for depth in [s.band_depth, s.band_depth + 1.5] {
                let d = mm(depth);
                for a in &s.apartments {
                    let Ok(u) = apartment_unit(a, d, false) else { continue };
                    let hall = u.pieces.iter().find(|p| p.kind == RoomKind::Hall).expect("a hall");
                    let entry = mm(rule("room.door_wide")) + 2 * rule_mm("facade.edge_clear");
                    assert!(hall.u1 - hall.u0 >= entry, "{t} {} at {depth} m: hall {} mm wide for its entrance", a.name, hall.u1 - hall.u0);
                    for p in u.pieces.iter().filter(|p| p.v1 == d) {
                        let run = p.u1.min(hall.u1) - p.u0.max(hall.u0);
                        assert!(run >= need, "{t} {} at {depth} m: {} meets the hall over {run} mm", a.name, p.name);
                        seen += 1;
                    }
                }
            }
        }
        assert!(seen > 0, "no apartment was laid");
    }
}
