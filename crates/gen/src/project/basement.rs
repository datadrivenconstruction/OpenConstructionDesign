// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Storeys below ground. The corridors, stairs and lifts of the ground floor run straight
//! down; the rest of the box under the building is parking, narrow leftovers are stores.

use super::build::{hole_clear, shift, stair_box};
use super::names;
use super::rules::rule_mm;
use super::tile::{self, m, mm, Cell, Mm, WinSize};
use super::{slots, GProject, PRole, PStorey, ProjectSpec, RoomKind};
use crate::{GLayout, GenError};

/// An edge of a kept room this close to the box snaps to it; a leftover strip narrower than
/// this could not take a door.
fn snap() -> Mm {
    rule_mm("basement.snap")
}

/// One row of bays with its wall: narrower leftovers are stores.
fn park_min() -> Mm {
    rule_mm("basement.park_min")
}
/// No windows below ground.
const NO_WIN: WinSize = WinSize {
    w: 0.0,
    h: 0.0,
    sill: 0.0,
};

type R = (Mm, Mm, Mm, Mm);

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

fn kept(k: RoomKind) -> bool {
    matches!(k, RoomKind::Corridor | RoomKind::Stair | RoomKind::Lift)
}

fn bbox(poly: &[(f64, f64)]) -> R {
    let mut r = (Mm::MAX, Mm::MAX, Mm::MIN, Mm::MIN);
    for q in poly {
        let (x, y) = (mm(q.0), mm(q.1));
        r = (r.0.min(x), r.1.min(y), r.2.max(x), r.3.max(y));
    }
    r
}

fn overlap(a: &R, b: &R) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// An outline of four axis-parallel edges.
fn is_box(poly: &[(f64, f64)]) -> bool {
    let n = poly.len();
    n == 4
        && (0..n).all(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            mm(a.0) == mm(b.0) || mm(a.1) == mm(b.1)
        })
}

/// Whether `p` lies inside the ring or on its edge.
fn within(ring: &[(Mm, Mm)], p: (Mm, Mm)) -> bool {
    let n = ring.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let cr = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        let on = cr == 0
            && (a.0.min(b.0)..=a.0.max(b.0)).contains(&p.0)
            && (a.1.min(b.1)..=a.1.max(b.1)).contains(&p.1);
        if on {
            return true;
        }
        if (a.1 > p.1) != (b.1 > p.1) {
            let x = a.0 as f64 + (p.1 - a.1) as f64 * (b.0 - a.0) as f64 / (b.1 - a.1) as f64;
            if (p.0 as f64) < x {
                c = !c;
            }
        }
    }
    c
}

/// A box lies under a simple ring when its corners do and no ring corner pokes into it.
fn fits(ring: &[(Mm, Mm)], r: &R) -> bool {
    r.0 < r.2
        && r.1 < r.3
        && [(r.0, r.1), (r.2, r.1), (r.2, r.3), (r.0, r.3)].iter().all(|q| within(ring, *q))
        && !ring.iter().any(|v| v.0 > r.0 && v.0 < r.2 && v.1 > r.1 && v.1 < r.3)
}

/// The largest box under the ground slab of a plate that holds `core`: grown side by side
/// from the box round the core, by `basement.grow_step` and then by halves of it down to a
/// millimetre. A plate's outline is not a box, and the basement stays under the building.
fn inscribed(slab: &[(f64, f64)], core: &[R]) -> Result<((f64, f64), Mm, Mm), GenError> {
    if core.is_empty() || slab.len() < 3 {
        return Err(infeasible("basement: the plate has no stair to take down; set basements to 0"));
    }
    let ring: Vec<(Mm, Mm)> = slab.iter().map(|q| (mm(q.0), mm(q.1))).collect();
    let mut r = core
        .iter()
        .fold((Mm::MAX, Mm::MAX, Mm::MIN, Mm::MIN), |a, c| (a.0.min(c.0), a.1.min(c.1), a.2.max(c.2), a.3.max(c.3)));
    if !fits(&ring, &r) {
        return Err(infeasible(
            "basement: the stairs and lifts of the plate do not stand in one box under it; set basements to 0",
        ));
    }
    let mut step = rule_mm("basement.grow_step").max(1);
    while step >= 1 {
        loop {
            let mut grew = false;
            for side in 0..4 {
                let mut t = r;
                match side {
                    0 => t.0 -= step,
                    1 => t.1 -= step,
                    2 => t.2 += step,
                    _ => t.3 += step,
                }
                if fits(&ring, &t) {
                    r = t;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        step /= 2;
    }
    Ok(((m(r.0), m(r.1)), r.2 - r.0, r.3 - r.1))
}

/// The part of the box `(0, 0)-(w, d)` outside `keep` as rectangles: a strip between every
/// two vertical edges, then equal runs of neighbouring strips joined.
fn free_rects(keep: &[R], w: Mm, d: Mm) -> Vec<R> {
    let mut xs: Vec<Mm> = vec![0, w];
    for r in keep {
        xs.extend([r.0, r.2]);
    }
    xs.sort_unstable();
    xs.dedup();
    let (mut open, mut out): (Vec<R>, Vec<R>) = (vec![], vec![]);
    for s in xs.windows(2) {
        let (xa, xb) = (s[0], s[1]);
        let mut cov: Vec<(Mm, Mm)> = keep
            .iter()
            .filter(|r| r.0 <= xa && r.2 >= xb)
            .map(|r| (r.1, r.3))
            .collect();
        cov.sort_unstable();
        let (mut free, mut y) = (vec![], 0);
        for (a, b) in cov {
            if a > y {
                free.push((y, a));
            }
            y = y.max(b);
        }
        if y < d {
            free.push((y, d));
        }
        let mut next = vec![];
        for (y0, y1) in free {
            match open.iter().position(|o| o.1 == y0 && o.3 == y1 && o.2 == xa) {
                Some(i) => {
                    let mut o = open.remove(i);
                    o.2 = xb;
                    next.push(o);
                }
                None => next.push((xa, y0, xb, y1)),
            }
        }
        out.append(&mut open);
        open = next;
    }
    out.extend(open);
    out
}

fn cell(r: R, kind: RoomKind, ru: bool) -> Cell {
    Cell {
        x0: r.0,
        y0: r.1,
        x1: r.2,
        y1: r.3,
        kind,
        name: names::kind_name(kind, ru).to_string(),
        owner: PRole::Public,
        asked: None,
        grown: false,
        lit: kind.habitable(),
    }
}

/// The ground floor's parts that go down, moved by `o`.
fn moved(g: &GLayout, o: (f64, f64)) -> GLayout {
    let mut l = GLayout {
        stairs: g.stairs.clone(),
        shafts: g.shafts.clone(),
        columns: g.columns.clone(),
        ..GLayout::default()
    };
    shift(&mut l, o);
    l
}

/// The plan of one basement, in the box's own frame from (0, 0).
struct Plan {
    cells: Vec<Cell>,
    w: Mm,
    d: Mm,
    /// The box's corner in project coordinates.
    o: (f64, f64),
    /// Each stair's hole through the slab above it, project coordinates.
    holes: Vec<Vec<(f64, f64)>>,
}

fn plan(spec: &ProjectSpec, p: &GProject) -> Result<Plan, GenError> {
    let g = &p.storeys[0];
    if g.layout.stairs.is_empty() {
        return Err(infeasible(
            "a basement needs the stair down: set floors to 2 or more, or basements to 0",
        ));
    }
    let plated = spec.plate.is_some();
    // a plate keeps its stairs, lifts and straight corridors; a slanted or ring corridor
    // would not square into the parking grid
    let down = |gr: &crate::GRoom, k: RoomKind| kept(k) && (!plated || k != RoomKind::Corridor || is_box(&gr.polygon));
    let (o, w, d) = if plated {
        let core: Vec<R> = g.layout.rooms.iter().zip(&g.rooms).filter(|x| down(x.0, x.1.kind)).map(|x| bbox(&x.0.polygon)).collect();
        inscribed(&g.layout.slab, &core)?
    } else {
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for q in g.layout.walls.iter().flat_map(|w| [w.a, w.b]) {
            lo = (lo.0.min(q.0), lo.1.min(q.1));
            hi = (hi.0.max(q.0), hi.1.max(q.1));
        }
        (lo, mm(hi.0 - lo.0), mm(hi.1 - lo.1))
    };
    let mut keep: Vec<(R, RoomKind)> = vec![];
    let snap = snap();
    for (gr, pr) in g.layout.rooms.iter().zip(&g.rooms).filter(|x| down(x.0, x.1.kind)) {
        let local: Vec<(f64, f64)> = gr.polygon.iter().map(|q| (q.0 - o.0, q.1 - o.1)).collect();
        let mut r = bbox(&local);
        r.0 = if r.0 < snap { 0 } else { r.0 };
        r.1 = if r.1 < snap { 0 } else { r.1 };
        r.2 = if r.2 > w - snap { w } else { r.2 };
        r.3 = if r.3 > d - snap { d } else { r.3 };
        if let Some(x) = keep.iter().find(|k| overlap(&k.0, &r)) {
            return Err(infeasible(format!(
                "basement: the {} and the {} above overlap once squared; use a bar massing \
                 or basements 0",
                names::kind_name(x.1, false),
                names::kind_name(pr.kind, false)
            )));
        }
        keep.push((r, pr.kind));
    }
    // strips too shallow to park on either side of a corridor: the parking's own aisle is the
    // way to the stair, so the corridor shrinks to the landing before its stairs and lifts and
    // the parking takes the whole depth
    let parks = |keep: &[(R, RoomKind)]| {
        let rects: Vec<R> = keep.iter().map(|k| k.0).collect();
        free_rects(&rects, w, d).iter().any(|r| (r.2 - r.0).min(r.3 - r.1) >= park_min())
    };
    let cores: Vec<R> = keep.iter().filter(|k| k.1 != RoomKind::Corridor).map(|k| k.0).collect();
    let open: Vec<(R, RoomKind)> = keep
        .iter()
        .filter_map(|&(r, k)| {
            if k != RoomKind::Corridor {
                return Some((r, k));
            }
            let along_x = r.2 - r.0 >= r.3 - r.1;
            let touch = |c: &R| if along_x { c.0 < r.2 && c.2 > r.0 && (c.3 == r.1 || c.1 == r.3) } else { c.1 < r.3 && c.3 > r.1 && (c.2 == r.0 || c.0 == r.2) };
            let span = cores.iter().filter(|c| touch(c)).fold(None, |s: Option<(Mm, Mm)>, c| {
                let (a, b) = if along_x { (c.0.max(r.0), c.2.min(r.2)) } else { (c.1.max(r.1), c.3.min(r.3)) };
                Some(s.map_or((a, b), |s| (s.0.min(a), s.1.max(b))))
            })?;
            Some((if along_x { (span.0, r.1, span.1, r.3) } else { (r.0, span.0, r.2, span.1) }, k))
        })
        .collect();
    if !plated && !parks(&keep) && parks(&open) && open.iter().any(|k| k.1 == RoomKind::Corridor) {
        keep = open;
    }
    let rects: Vec<R> = keep.iter().map(|k| k.0).collect();
    let mut cells: Vec<Cell> = keep.iter().map(|k| cell(k.0, k.1, spec.ru)).collect();
    for r in free_rects(&rects, w, d) {
        let narrow = (r.2 - r.0).min(r.3 - r.1);
        if narrow < snap {
            return Err(infeasible(format!(
                "basement: a strip {} m wide is left beside the stairs and corridors; change \
                 the width or the core, or set basements to 0",
                m(narrow)
            )));
        }
        let kind = if narrow >= park_min() {
            RoomKind::Parking
        } else {
            RoomKind::Storage
        };
        cells.push(cell(r, kind, spec.ru));
    }
    let mut holes = vec![];
    for st in &g.layout.stairs {
        // the stair's layout stands in the plan already (origin and turn applied)
        let (lo, hi) = stair_box(st)?;
        let e = hole_clear();
        let (x0, y0) = (lo.0 - e, lo.1 - e);
        let (x1, y1) = (hi.0 + e, hi.1 + e);
        let c = ((x0 + x1) / 2.0 - o.0, (y0 + y1) / 2.0 - o.1);
        let room = keep.iter().find(|k| {
            k.1 == RoomKind::Stair && m(k.0 .0) <= c.0 && c.0 <= m(k.0 .2) && m(k.0 .1) <= c.1 && c.1 <= m(k.0 .3)
        });
        let (x0, y0, x1, y1) = match room {
            Some((r, _)) => (
                x0.max(m(r.0) + o.0),
                y0.max(m(r.1) + o.1),
                x1.min(m(r.2) + o.0),
                y1.min(m(r.3) + o.1),
            ),
            None => (x0, y0, x1, y1),
        };
        // the clearance round a stair at the facade stops short of the slab edge by as much
        let (x0, y0) = (x0.max(o.0 + e), y0.max(o.1 + e));
        let (x1, y1) = (x1.min(o.0 + m(w) - e), y1.min(o.1 + m(d) - e));
        if x1 - x0 < e || y1 - y0 < e {
            return Err(infeasible("basement: a stair of the ground floor stands outside the basement box; use basements 0"));
        }
        holes.push(crate::layout::rect(x0, y0, x1, y1));
    }
    Ok(Plan {
        cells,
        w,
        d,
        o,
        holes,
    })
}

/// Basement `b` (0 right below the ground floor).
fn storey(
    spec: &ProjectSpec,
    p: &GProject,
    pl: &Plan,
    cells: &[Cell],
    b: usize,
    n: usize,
    rules: &super::fit_rules::Rules,
) -> Result<PStorey, GenError> {
    let h = spec.model.as_ref().and_then(|m| m.below.as_ref()).and_then(|b| b.height).unwrap_or(spec.floor_height);
    let nums = (0..cells.len()).map(|i| format!("B{}{:02}", b + 1, i + 1)).collect();
    let t = tile::tile(cells, nums, pl.w, pl.d, NO_WIN, false)?;
    let mut l = t.layout;
    let below = spec.model.as_ref().and_then(|m| m.below.as_ref());
    // a wall below grade holds back the soil, typology or not
    if below.map_or(true, |x| x.retaining) {
        super::typology::below::retain(&mut l);
    }
    l.height = h;
    l.slab = crate::layout::rect(0.0, 0.0, m(pl.w), m(pl.d));
    let down = moved(&p.storeys[0].layout, (-pl.o.0, -pl.o.1));
    l.stairs = down.stairs;
    l.shafts = down.shafts;
    let (w, d) = (m(pl.w), m(pl.d));
    l.columns = down
        .columns
        .into_iter()
        .filter(|c| c.at.0 > 0.0 && c.at.0 < w && c.at.1 > 0.0 && c.at.1 < d)
        .collect();
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
    let (fixtures, fit) = super::furnish::furnish(&l, &rooms, h, spec.typology, rules, &mut Default::default());
    l.fixtures = fixtures;
    shift(&mut l, pl.o);
    if b + 1 < n {
        l.slab_holes.extend(pl.holes.iter().cloned());
    }
    let caps = [
        (l.walls.len(), slots::OPENINGS - slots::WALLS, "walls"),
        (l.openings.len(), slots::ROOMS - slots::OPENINGS, "openings"),
        (l.rooms.len(), slots::SLAB - slots::ROOMS, "rooms"),
        (l.columns.len(), slots::MAX_COLUMNS, "columns"),
        (l.fixtures.len(), slots::FIXTURES_MAX, "fixtures"),
    ];
    for (have, cap, what) in caps {
        if have > cap as usize {
            return Err(infeasible(format!("{have} {what} in basement {}; at most {cap}", b + 1)));
        }
    }
    crate::check::validate_layout(&l).map_err(|e| infeasible(format!("basement {}: {e}", b + 1)))?;
    Ok(PStorey {
        name: below.map_or_else(|| names::basement_name(b, spec.ru), |x| super::typology::below::level_name(spec, x, b)),
        elevation: -((b + 1) as f64) * h,
        height: h,
        layout: l,
        rooms: t.rooms,
        stair_room: cells.iter().position(|c| c.kind == RoomKind::Stair),
        fit,
        curtains: vec![],
        railings: vec![],
    })
}

/// Adds `spec.basements` storeys under the ground floor, the stair holes through the ground
/// slab, and the basement metrics.
pub(super) fn add(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    let n = spec.basements as usize;
    if n == 0 {
        return Ok(());
    }
    if p.storeys.len() + n > slots::MAX_STOREYS as usize || n >= slots::LOW_STOREYS as usize {
        return Err(infeasible(format!(
            "{} floors and {n} basements; at most {} storeys in all",
            p.storeys.len(),
            slots::MAX_STOREYS
        )));
    }
    let pl = plan(spec, p)?;
    let rules = super::fit_rules::rules_for(spec.furnish.as_ref())
        .map_err(|e| infeasible(format!("fit-out rules: {}", e.join("; "))))?;
    let mut out = Vec::with_capacity(n);
    for b in 0..n {
        // a typology with levels below ground gives each level its own spaces
        let own = match spec.model.as_ref().and_then(|m| m.below.as_ref().map(|x| (m, x))) {
            Some((m, x)) => Some(super::typology::below::level_cells(spec, m, x, &pl.cells, b)?),
            None => None,
        };
        out.push(storey(spec, p, &pl, own.as_deref().unwrap_or(&pl.cells), b, n, &rules)?);
    }
    p.storeys[0].layout.slab_holes.extend(pl.holes.iter().cloned());
    // the levels of a typology hold its own spaces, not the parking the base plan shows
    let typed = spec.model.as_ref().is_some_and(|m| m.below.is_some());
    let area = |k: RoomKind| -> f64 {
        if typed {
            return 0.0;
        }
        pl.cells.iter().filter(|c| c.kind == k).map(Cell::area).sum::<f64>() * n as f64
    };
    let bays = out
        .iter()
        .flat_map(|s| &s.layout.fixtures)
        .filter(|f| f.family == "Car")
        .count();
    p.metrics.extend([
        ("basements", n as f64),
        ("basement_area", m(pl.w) * m(pl.d) * n as f64),
        ("parking_area", area(RoomKind::Parking)),
        ("parking_bays", bays as f64),
    ]);
    p.basements = out;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_free_part_is_covered_once_and_runs_are_joined() {
        // a corridor across, a stair below it and a lift above, off line with each other
        let keep = [(0, 6000, 30000, 8000), (10000, 0, 14000, 6000), (10000, 8000, 12400, 14000)];
        let free = free_rects(&keep, 30000, 14000);
        let area: Mm = free.iter().map(|r| (r.2 - r.0) * (r.3 - r.1)).sum();
        assert_eq!(area, 30000 * 14000 - 30000 * 2000 - 4000 * 6000 - 2400 * 6000);
        for (i, a) in free.iter().enumerate() {
            assert!(keep.iter().all(|k| !overlap(a, k)), "{a:?}");
            assert!(free[i + 1..].iter().all(|b| !overlap(a, b)), "{a:?}");
        }
        // west and east of the stair, west and east of the lift
        assert_eq!(free.len(), 4, "{free:?}");
        assert!(free.contains(&(12400, 8000, 30000, 14000)));
    }

    /// The stair layout stands in the plan already: every stair far from the origin gets its
    /// hole through the ground slab round its own flights.
    #[test]
    fn a_stair_hole_lies_round_its_stair_wherever_it_stands() {
        let s = super::super::parse_spec(&serde_json::json!({"typology": "hospital_wing", "floors": 4, "basements": 1})).unwrap();
        let p = super::super::build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let g = &p.storeys[0].layout;
        assert!(g.stairs.iter().any(|st| st.origin.0 > 50.0), "a stair far along the wing");
        for st in &g.stairs {
            let (lo, hi) = stair_box(st).unwrap();
            let c = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);
            assert!(g.slab_holes.iter().any(|h| crate::check::inside(h, c)), "no hole round the stair at {c:?}");
        }
    }
}
