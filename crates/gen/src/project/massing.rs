// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Massing from bars: L, U and courtyard blocks are copies of the double-loaded bar turned
//! round corner halls that join their corridor ends; a stepped block (set back, podium or
//! cantilever) is two variants of the bar, one above the other, their stairs on one axis.

use super::build::{build_bar, build_bar_one_stair, place, place_fit};
use super::design::Massing;
use super::facade;
use super::limits::BAND_DEPTH;
use super::{slots, GHvac, GProject, PRole, PRoom, PStorey, ProjectSpec, RoomKind};
use crate::check::dist_to_seg;
use crate::{GLayout, GOpening, GRoom, GWall, GenError, WallRole};

type P = (f64, f64);

const EPS: f64 = 1e-6;
const DOOR: f64 = 1.2;
const DOOR_H: f64 = 2.1;
/// Set-back of a podium when the brief names none, m.
const PODIUM_STEP: f64 = 3.0;
const MAX_STAIRS: usize = (slots::SHAFTS - slots::STAIRS) as usize;
const MAX_SHAFTS: usize = (slots::GRIDS - slots::SHAFTS) as usize;

fn infeasible(s: impl Into<String>) -> GenError {
    GenError::Infeasible(s.into())
}

pub(super) fn compose(spec: &ProjectSpec, base: &ProjectSpec) -> Result<GProject, GenError> {
    let p = match spec.design.step_at(spec.floors) {
        Some(k0) => stepped(spec, base, k0 as usize)?,
        None => wings(spec, base)?,
    };
    for (k, s) in p.storeys.iter().enumerate() {
        caps(&s.layout, k)?;
        crate::check::validate_layout(&s.layout)
            .map_err(|e| infeasible(format!("{} massing, floor {}: {e}", spec.design.massing.name(), k + 1)))?;
    }
    Ok(p)
}

fn caps(l: &GLayout, k: usize) -> Result<(), GenError> {
    let caps = [
        (l.walls.len(), (slots::OPENINGS - slots::WALLS) as usize, "walls"),
        (l.openings.len(), (slots::ROOMS - slots::OPENINGS) as usize, "openings"),
        (l.rooms.len(), (slots::SLAB - slots::ROOMS) as usize, "rooms"),
        (l.stairs.len(), MAX_STAIRS, "stairs"),
        (l.shafts.len(), MAX_SHAFTS, "shafts"),
        (l.columns.len(), slots::MAX_COLUMNS as usize, "columns"),
    ];
    for (have, cap, what) in caps {
        if have > cap {
            return Err(infeasible(format!(
                "{have} {what} on floor {}; at most {cap}: a shorter program per wing",
                k + 1
            )));
        }
    }
    Ok(())
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
        ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
    })
}

pub(super) fn ring_area(r: &[P]) -> f64 {
    let n = r.len();
    ((0..n).map(|i| r[i].0 * r[(i + 1) % n].1 - r[(i + 1) % n].0 * r[i].1).sum::<f64>() / 2.0).abs()
}

/// The atrium: the ground slab runs through the court as its floor, the upper slabs stay
/// open round it, and a flat roof closes it at the top.
pub(super) fn roof_court(p: &mut GProject) -> Result<(), GenError> {
    let Some(c) = p.court.clone() else {
        return Err(infeasible("the atrium roofs a courtyard: design.massing courtyard"));
    };
    let ground = &mut p.storeys[0].layout.slab_holes;
    let before = ground.len();
    ground.retain(|h| *h != c);
    if ground.len() == before {
        return Err(infeasible("the courtyard is missing from the ground slab"));
    }
    p.extra_roofs.push(super::PRoof {
        ring: c.clone(),
        slope: super::FLAT_FALL_DEG,
        overhang: 0.0,
        ..p.roof.clone()
    });
    let area = ring_area(&c);
    p.metrics.push(("atrium_area", area));
    for (k, v) in &mut p.metrics {
        if *k == "gross_floor_area" {
            *v += area;
        }
    }
    Ok(())
}

fn set_metrics(p: &mut GProject) {
    let pts: Vec<P> = p.storeys.iter().flat_map(|s| s.layout.slab.clone()).collect();
    let (lo, hi) = bbox(&pts);
    p.footprint = (hi.0 - lo.0, hi.1 - lo.1);
    let gfa: f64 = p
        .storeys
        .iter()
        .map(|s| ring_area(&s.layout.slab) - p.court.as_deref().map_or(0.0, ring_area))
        .sum();
    let rooms: usize = p.storeys.iter().map(|s| s.rooms.len()).sum();
    for (k, v) in &mut p.metrics {
        match *k {
            "width" => *v = p.footprint.0,
            "depth" => *v = p.footprint.1,
            "gross_floor_area" => *v = gfa,
            "rooms" => *v = rooms as f64,
            "apartments" => *v = f64::from(p.apartments),
            _ => {}
        }
    }
}

fn renumber_apartments(s: &mut PStorey, off: u32) {
    if off == 0 {
        return;
    }
    for r in &mut s.layout.rooms {
        if let Some(a) = &mut r.apartment {
            *a += off;
        }
    }
    for r in &mut s.rooms {
        if let PRole::Apartment(a) = &mut r.owner {
            *a += off;
        }
    }
}

fn apartment_ids(s: &PStorey) -> impl Iterator<Item = u32> + '_ {
    s.layout.rooms.iter().filter_map(|r| r.apartment)
}

/// Lower floors and upper floors from two widths of the same program. The upper part moves so
/// its stair stands on the lower one: the flight from the last lower floor lands in its hole.
fn stepped(spec: &ProjectSpec, base: &ProjectSpec, k0: usize) -> Result<GProject, GenError> {
    let d = &spec.design;
    // both parts dry: the composed block gets one ventilation system
    // both parts on the core stair alone: the upper part's stair continues the lower one's
    let narrow = build_bar_one_stair(base)?;
    let (w, dp) = narrow.footprint;
    let cant = d.cantilever > 0.0;
    let e = if cant {
        d.cantilever
    } else if d.setback > 0.0 {
        d.setback
    } else {
        PODIUM_STEP
    };
    let mut ws = base.clone();
    ws.width = Some(w + 2.0 * e);
    if d.massing == Massing::TowerPodium {
        let band = (base.band_depth + e).min(BAND_DEPTH.max);
        ws.depth = base.depth.map(|x| x + 2.0 * (band - base.band_depth));
        ws.band_depth = band;
    }
    let what = if cant { "cantilevered upper part" } else { "podium" };
    let wide = build_bar_one_stair(&ws).map_err(|x| infeasible(format!("the {what} {:.1} m wide: {x}", w + 2.0 * e)))?;
    let (lower, upper) = if cant { (narrow, wide) } else { (wide, narrow) };
    let stair = |p: &GProject| p.storeys[0].layout.stairs.first().map(|s| (s.origin, s.angle));
    let (Some((lo, la)), Some((uo, ua))) = (stair(&lower), stair(&upper)) else {
        return Err(infeasible("a stepped block needs a stair: two floors or more"));
    };
    if (la - ua).abs() > EPS {
        return Err(infeasible("the stair runs differently in the two parts: give footprint.width"));
    }
    let t = (lo.0 - uo.0, lo.1 - uo.1);
    let n = spec.floors as usize;
    let lower_ring = lower.storeys[0].layout.slab.clone();
    let mut storeys = Vec::with_capacity(n);
    for k in 0..n {
        if k < k0 {
            storeys.push(lower.storeys[k].clone());
        } else {
            let mut s = upper.storeys[k].clone();
            place(&mut s.layout, 0, t);
            place_fit(&mut s.fit, 0, t);
            s.layout.grids.clear();
            if k == k0 && !cant {
                s.layout.slab = lower_ring.clone();
            }
            storeys.push(s);
        }
    }
    // apartment numbers stay unique across the step
    let top_low = storeys[..k0].iter().flat_map(apartment_ids).max().unwrap_or(0);
    let first_up = storeys[k0..].iter().flat_map(apartment_ids).min();
    if let Some(m) = first_up.filter(|m| *m <= top_low) {
        for s in &mut storeys[k0..] {
            renumber_apartments(s, top_low + 1 - m);
        }
    }
    let apartments = storeys.iter().flat_map(apartment_ids).max().unwrap_or(0);
    let mut roof = upper.roof.clone();
    roof.ring.iter_mut().for_each(|q| *q = (q.0 + t.0, q.1 + t.1));
    let mut p = GProject {
        storeys,
        roof,
        extra_roofs: vec![],
        copies: 1,
        court: None,
        podium: (!cant).then_some(lower_ring),
        footprint: lower.footprint,
        apartments,
        metrics: lower.metrics.clone(),
        hvac: GHvac::default(),
        services: super::GServices::default(),
        basements: vec![],
        site: super::site::GSite::default(),
        fire: Default::default(),
        lifts: Default::default(),
        shell: None,
    };
    p.metrics.push(("upper_from", k0 as f64));
    set_metrics(&mut p);
    super::hvac::ventilate(base, &mut p)?;
    Ok(p)
}

#[derive(Clone, Copy, PartialEq)]
enum Corner {
    Se,
    Sw,
    Ne,
    Nw,
}

/// A copy of the bar: quarter turns, offset, and which corner each of its ends opens into.
struct Part {
    quarter: u8,
    at: P,
    /// Corner at local x = 0 and at local x = width.
    ends: [Option<Corner>; 2],
}

fn corner_rect(c: Corner, w: f64, d: f64) -> (P, P) {
    match c {
        Corner::Se => ((w, 0.0), (w + d, d)),
        Corner::Sw => ((-d, 0.0), (0.0, d)),
        Corner::Ne => ((w, d + w), (w + d, 2.0 * d + w)),
        Corner::Nw => ((-d, d + w), (0.0, 2.0 * d + w)),
    }
}

/// The two sides of a corner hall on the outside of the block.
fn corner_walls(c: Corner, w: f64, d: f64) -> [(P, P); 2] {
    let ((x0, y0), (x1, y1)) = corner_rect(c, w, d);
    match c {
        Corner::Se => [((x0, y0), (x1, y0)), ((x1, y0), (x1, y1))],
        Corner::Sw => [((x0, y0), (x1, y0)), ((x0, y0), (x0, y1))],
        Corner::Ne => [((x0, y1), (x1, y1)), ((x1, y0), (x1, y1))],
        Corner::Nw => [((x0, y1), (x1, y1)), ((x0, y0), (x0, y1))],
    }
}

fn layout_of(m: Massing, w: f64, d: f64) -> (Vec<Part>, Vec<Corner>, Vec<P>, Option<Vec<P>>) {
    let bar = |sw| Part {
        quarter: 0,
        at: (0.0, 0.0),
        ends: [sw, Some(Corner::Se)],
    };
    let east = |ne| Part {
        quarter: 1,
        at: (w + d, d),
        ends: [Some(Corner::Se), ne],
    };
    let west = |nw| Part {
        quarter: 3,
        at: (-d, d + w),
        ends: [nw, Some(Corner::Sw)],
    };
    let north = Part {
        quarter: 2,
        at: (w, 2.0 * d + w),
        ends: [Some(Corner::Ne), Some(Corner::Nw)],
    };
    match m {
        Massing::L => (
            vec![bar(None), east(None)],
            vec![Corner::Se],
            vec![(0.0, 0.0), (w + d, 0.0), (w + d, d + w), (w, d + w), (w, d), (0.0, d)],
            None,
        ),
        Massing::U => (
            vec![bar(Some(Corner::Sw)), east(None), west(None)],
            vec![Corner::Se, Corner::Sw],
            vec![
                (-d, 0.0),
                (w + d, 0.0),
                (w + d, d + w),
                (w, d + w),
                (w, d),
                (0.0, d),
                (0.0, d + w),
                (-d, d + w),
            ],
            None,
        ),
        _ => (
            vec![bar(Some(Corner::Sw)), east(Some(Corner::Ne)), west(Some(Corner::Nw)), north],
            vec![Corner::Se, Corner::Sw, Corner::Ne, Corner::Nw],
            rect(-d, 0.0, w + d, 2.0 * d + w),
            Some(rect(0.0, d, w, d + w)),
        ),
    }
}

/// Opens the corridor end at local `x = e` into the corner hall: the windows of that end go,
/// a door takes the corridor's axis.
fn open_end(l: &mut GLayout, rooms: &[PRoom], e: f64) -> Result<(), GenError> {
    let corridor = l.rooms.iter().zip(rooms).find_map(|(r, m)| {
        let (lo, hi) = bbox(&r.polygon);
        let touches = (lo.0 - e).abs() < EPS || (hi.0 - e).abs() < EPS;
        (m.kind == RoomKind::Corridor && touches).then_some((lo.1, hi.1))
    });
    let Some((c0, c1)) = corridor else {
        return Err(infeasible(
            "wings join at the corridor ends, and this plan has no corridor reaching its end: \
             use massing bar or tower_podium, or rooms that need a corridor",
        ));
    };
    let on_end = |w: &GWall| (w.a.0 - e).abs() < EPS && (w.b.0 - e).abs() < EPS;
    let mid = (e, (c0 + c1) / 2.0);
    let Some(wi) = (0..l.walls.len()).find(|&i| on_end(&l.walls[i]) && dist_to_seg(mid, l.walls[i].a, l.walls[i].b) < EPS) else {
        return Err(infeasible("the corridor end has no wall to open"));
    };
    let ends: Vec<usize> = (0..l.walls.len()).filter(|&i| on_end(&l.walls[i])).collect();
    let w = &l.walls[wi];
    let dir = ((w.b.0 - w.a.0), (w.b.1 - w.a.1));
    let wl = dir.0.hypot(dir.1);
    let tc = ((mid.0 - w.a.0) * dir.0 + (mid.1 - w.a.1) * dir.1) / wl;
    let dw = DOOR.min(c1 - c0 - 0.5);
    if dw < 0.8 {
        return Err(infeasible(format!("the corridor ({:.2} m) is too narrow to join a wing", c1 - c0)));
    }
    let (s0, s1) = (tc - dw / 2.0, tc + dw / 2.0);
    l.openings.retain(|o| {
        let span_hit = o.wall == wi && o.along < s1 + 0.15 && o.along + o.width > s0 - 0.15;
        !(ends.contains(&o.wall) && (!o.door || span_hit))
    });
    l.openings.push(GOpening {
        wall: wi,
        along: s0,
        door: true,
        width: dw,
        sill: 0.0,
        height: DOOR_H,
        ty: None,
    });
    Ok(())
}

fn merge(into: &mut PStorey, part: PStorey, stairs: bool, grids: bool) {
    let (l, mut q) = (&mut into.layout, part.layout);
    let off = l.walls.len();
    for o in &mut q.openings {
        o.wall += off;
    }
    for f in &mut q.fixtures {
        f.wall = f.wall.map(|w| w + off);
    }
    l.walls.append(&mut q.walls);
    l.openings.append(&mut q.openings);
    l.rooms.append(&mut q.rooms);
    l.columns.append(&mut q.columns);
    l.shafts.append(&mut q.shafts);
    l.fixtures.append(&mut q.fixtures);
    if stairs {
        l.stairs.append(&mut q.stairs);
        l.slab_holes.append(&mut q.slab_holes);
    }
    if grids {
        l.grids.append(&mut q.grids);
    }
    let base = into.rooms.len();
    into.fit.extend(part.fit.into_iter().map(|mut f| {
        f.room += base;
        f
    }));
    into.rooms.extend(part.rooms);
}

fn wings(spec: &ProjectSpec, base: &ProjectSpec) -> Result<GProject, GenError> {
    let bar = build_bar(base)?;
    let (w, d) = bar.footprint;
    let (parts, corners, outline, court) = layout_of(spec.design.massing, w, d);
    let letters = ["", "B", "C", "D"];
    let n = bar.storeys.len();
    let mut storeys = Vec::with_capacity(n);
    // each wing carries a turned copy of the bar's ventilation; links follow the merge offsets
    let mut offs = vec![vec![0usize; n]; parts.len()];
    for (k, bs) in bar.storeys.iter().enumerate() {
        let mut acc: Option<PStorey> = None;
        for (i, part) in parts.iter().enumerate() {
            offs[i][k] = acc.as_ref().map_or(0, |a| a.layout.fixtures.len());
            let mut s = bs.clone();
            for (end, c) in part.ends.iter().enumerate() {
                if c.is_some() {
                    let e = if end == 0 { 0.0 } else { w };
                    open_end(&mut s.layout, &bs.rooms, e)?;
                }
            }
            place(&mut s.layout, part.quarter, part.at);
            place_fit(&mut s.fit, part.quarter, part.at);
            renumber_apartments(&mut s, bar.apartments * i as u32);
            for r in &mut s.rooms {
                r.number = format!("{}{}", letters[i], r.number);
            }
            let stairs = acc.as_ref().is_none_or(|a| a.layout.stairs.len() + s.layout.stairs.len() <= MAX_STAIRS);
            match &mut acc {
                None => acc = Some(s),
                Some(a) => merge(a, s, stairs, false),
            }
        }
        let mut s = acc.expect("a block has its bar");
        for (j, c) in corners.iter().enumerate() {
            corner_hall(&mut s, *c, w, d, k, j, spec.ru);
            let i = s.rooms.len() - 1;
            fit_added(spec, &mut s, i);
        }
        s.layout.slab = outline.clone();
        if let Some(h) = &court {
            s.layout.slab_holes.push(h.clone());
        }
        clear_grid_ends(&mut s.layout.grids, &outline, court.as_deref());
        let h = s.height;
        // the fit-out already stands: a new window keeps clear of its tall pieces
        let tall: Vec<[f64; 4]> = super::fit_rules::rules_for(spec.furnish.as_ref())
            .map(|r| {
                s.layout
                    .fixtures
                    .iter()
                    .filter_map(|f| r.item_of(f).filter(|it| it.tall).map(|it| super::fit_rules::Placed::of(it, f).body))
                    .collect()
            })
            .unwrap_or_default();
        for ri in 0..s.rooms.len() {
            if s.rooms[ri].kind.habitable() && !lit(&s.layout, ri) {
                facade::relight(&mut s.layout, ri, h, &tall);
            }
        }
        storeys.push(s);
    }
    let rings: Vec<Vec<P>> = parts
        .iter()
        .map(|p| match p.quarter {
            0 => rect(0.0, 0.0, w, d),
            1 => rect(w, d, w + d, d + w),
            3 => rect(-d, d, 0.0, d + w),
            _ => rect(0.0, d + w, w, 2.0 * d + w),
        })
        .chain(corners.iter().map(|c| {
            let (a, b) = corner_rect(*c, w, d);
            rect(a.0, a.1, b.0, b.1)
        }))
        .collect();
    let extra_roofs = rings[1..]
        .iter()
        .map(|r| super::PRoof {
            ring: r.clone(),
            ..bar.roof.clone()
        })
        .collect();
    let mut p = GProject {
        storeys,
        roof: bar.roof.clone(),
        extra_roofs,
        copies: parts.len() as u32,
        court,
        podium: None,
        footprint: (w, d),
        apartments: bar.apartments * parts.len() as u32,
        metrics: bar.metrics.clone(),
        hvac: GHvac::default(),
        services: super::GServices::default(),
        basements: vec![],
        site: super::site::GSite::default(),
        fire: Default::default(),
        lifts: Default::default(),
        shell: None,
    };
    for (i, part) in parts.iter().enumerate() {
        add_wing_hvac(&mut p.hvac, &bar.hvac, part.quarter, part.at, &offs[i], n);
    }
    // each wing repeats the bar's air system: its counts and flows add up
    for m in p.metrics.iter_mut().filter(|m| m.0.starts_with("hvac_")) {
        m.1 *= parts.len() as f64;
    }
    if p.hvac.ducts.len() > slots::DUCTS_MAX as usize {
        return Err(infeasible(format!(
            "{} air ducts; at most {}: split the building",
            p.hvac.ducts.len(),
            slots::DUCTS_MAX
        )));
    }
    if p.hvac.plant.len() > slots::PLANT_MAX as usize {
        return Err(infeasible(format!(
            "{} air handling units; at most {}: split the building",
            p.hvac.plant.len(),
            slots::PLANT_MAX
        )));
    }
    p.metrics.push(("wings", parts.len() as f64));
    set_metrics(&mut p);
    Ok(p)
}

/// The first wing's grid lines ran to its own edges, which in a joined block lie in a corner
/// hall: each end goes on along its line until it is out of the building, then overruns.
fn clear_grid_ends(grids: &mut [crate::GGrid], outline: &[P], court: Option<&[P]>) {
    use ddc_geom::math::v2;
    use ddc_geom::predicates::point_in_ring;
    let ring: Vec<_> = outline.iter().map(|q| v2(q.0, q.1)).collect();
    let hole: Vec<_> = court.unwrap_or_default().iter().map(|q| v2(q.0, q.1)).collect();
    // a wall line counts as built: a grid on a wing's wall line runs on past its end
    let inside = |q: P| {
        let q = v2(q.0, q.1);
        point_in_ring(q, &ring) >= 0 && (hole.is_empty() || point_in_ring(q, &hole) <= 0)
    };
    let span = outline
        .iter()
        .flat_map(|q| outline.iter().map(move |r| (q.0 - r.0).hypot(q.1 - r.1)))
        .fold(0.0, f64::max);
    const STEP: f64 = 0.01;
    for g in grids.iter_mut() {
        let l = (g.b.0 - g.a.0).hypot(g.b.1 - g.a.1);
        if l < EPS {
            continue;
        }
        let u = ((g.b.0 - g.a.0) / l, (g.b.1 - g.a.1) / l);
        for (end, sign) in [(&mut g.a, -1.0), (&mut g.b, 1.0)] {
            if !inside(*end) {
                continue;
            }
            let mut t = 0.0;
            while t < span && inside((end.0 + sign * u.0 * t, end.1 + sign * u.1 * t)) {
                t += STEP;
            }
            let t = t + super::build::grid_overrun();
            *end = (end.0 + sign * u.0 * t, end.1 + sign * u.1 * t);
        }
    }
}

/// Appends the bar's ventilation turned by `quarter` and moved to `at`; `off[k]` is where the
/// wing's fixtures start on storey `k` (`n` storeys; links on storey `n` point at the plant).
fn add_wing_hvac(into: &mut GHvac, h: &GHvac, quarter: u8, at: P, off: &[usize], n: usize) {
    let q = quarter % 4;
    let turn = |x: f64, y: f64| {
        let (x, y) = match q {
            0 => (x, y),
            1 => (-y, x),
            2 => (-x, -y),
            _ => (y, -x),
        };
        (x + at.0, y + at.1)
    };
    let rot = f64::from(q) * std::f64::consts::FRAC_PI_2;
    let plant0 = into.plant.len();
    // each wing is its own set of networks, numbered on from the wings before it
    let per = 1 + h.nets.len();
    let net0 = if into.plant.is_empty() { 0 } else { (into.nets.len() + 1) as u8 };
    if net0 > 0 {
        for k in 0..per {
            let no = usize::from(net0) + k + 1;
            let (a, b) = if k == 0 { &h.names } else { &h.nets[k - 1] };
            into.nets.push((renumber(a, no), renumber(b, no)));
        }
    } else {
        into.names.clone_from(&h.names);
        into.nets.clone_from(&h.nets);
    }
    into.plant.extend(h.plant.iter().map(|f| {
        let mut f = f.clone();
        f.at = turn(f.at.0, f.at.1);
        f.rotation += rot;
        f
    }));
    into.ducts.extend(h.ducts.iter().map(|d| {
        let mut d = d.clone();
        d.points.iter_mut().for_each(|p| {
            let (x, y) = turn(p.0, p.1);
            *p = (x, y, p.2);
        });
        d.net += net0;
        d
    }));
    into.links.extend(h.links.iter().map(|l| {
        let mut l = l.clone();
        l.fixture += if l.storey >= n { plant0 } else { off[l.storey] };
        l.net += net0;
        l
    }));
    into.flow = (into.flow.0 + h.flow.0, into.flow.1 + h.flow.1);
}

/// A system name with its trailing number replaced by `no`.
fn renumber(name: &str, no: usize) -> String {
    format!("{}{no}", name.trim_end_matches(|c: char| c.is_ascii_digit()))
}

/// Whether room `ri` has a window on its outline.
fn lit(l: &GLayout, ri: usize) -> bool {
    let poly = &l.rooms[ri].polygon;
    l.openings.iter().filter(|o| !o.door).any(|o| {
        let w = &l.walls[o.wall];
        let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
        let wl = dx.hypot(dy);
        let t = o.along + o.width / 2.0;
        let c = (w.a.0 + dx * t / wl, w.a.1 + dy * t / wl);
        (0..poly.len()).any(|i| dist_to_seg(c, poly[i], poly[(i + 1) % poly.len()]) < 1e-3)
    })
}

/// Fits out room `i`, added once the storey's own rooms were fitted, by the same rules.
fn fit_added(spec: &ProjectSpec, s: &mut PStorey, i: usize) {
    let Ok(rules) = super::fit_rules::rules_for(spec.furnish.as_ref()) else {
        return;
    };
    let (lo, hi) = bbox(&s.layout.rooms[i].polygon);
    let (kind, owner) = (s.rooms[i].kind, s.rooms[i].owner);
    let name = s.layout.rooms[i].name.clone();
    let room = super::furnish::RoomIn {
        rect: [lo.0, lo.1, hi.0, hi.1],
        kind,
        owner,
        name: &name,
        tags: spec.room_tags.of(kind, &name),
        keep: vec![],
    };
    let (fx, fit) = super::furnish::furnish(&s.layout, std::slice::from_ref(&room), s.height, spec.typology, &rules, &mut Default::default());
    s.layout.fixtures.extend(fx);
    s.fit.extend(fit.into_iter().map(|mut f| {
        f.room = i;
        f
    }));
}

/// The hall in corner `c`: its two outer walls with a window each, the entrance on the ground.
fn corner_hall(s: &mut PStorey, c: Corner, w: f64, d: f64, k: usize, j: usize, ru: bool) {
    let ((x0, y0), (x1, y1)) = corner_rect(c, w, d);
    let l = &mut s.layout;
    let first = l.walls.len();
    for (a, b) in corner_walls(c, w, d) {
        l.walls.push(GWall::line(a, b, WallRole::Exterior));
    }
    let name = if ru { "\u{425}\u{43e}\u{43b}\u{43b}" } else { "Link hall" };
    l.rooms.push(GRoom {
        name: name.to_string(),
        apartment: None,
        point: ((x0 + x1) / 2.0, (y0 + y1) / 2.0),
        polygon: rect(x0, y0, x1, y1),
    });
    s.rooms.push(PRoom {
        kind: RoomKind::Lobby,
        number: format!("{}L{}", k + 1, j + 1),
        owner: PRole::Public,
        asked: None,
        grown: false,
    });
    let h = s.height;
    for (i, wi) in [first, first + 1].into_iter().enumerate() {
        if k == 0 && i == 0 {
            l.openings.push(GOpening {
                wall: wi,
                along: (d - 1.8) / 2.0,
                door: true,
                width: 1.8,
                sill: 0.0,
                height: DOOR_H,
                ty: None,
            });
        } else {
            facade::add_window(l, wi, (0.0, d), 2.4, 0.9, (h - 1.3).clamp(0.9, 1.8));
        }
    }
}
