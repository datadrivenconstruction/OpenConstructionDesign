// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The skin of a built project: the facade system re-glazes the bays of every outer wall,
//! fins, mullions and panels stand off the wall face, balconies and loggias hang off habitable
//! rooms, a canopy marks each entrance, and the site gets paving and trees. All of it is
//! library families placed as fixtures, so the model and the export treat them as components.

use super::design::{Balconies, FacadeSystem};
use super::rules::rule;
use super::{GCurtain, GProject, ProjectSpec, RoofKind};
use crate::check::{dist_to_seg, inside};
use crate::{GFixture, GLayout, GOpening, GWall, GenError, WallRole};

type P = (f64, f64);

/// Margin of a new opening from a wall junction, beyond what the layout check asks.
const BAY_MARGIN: f64 = 0.3;
const SILL: f64 = 0.9;
/// Fixture types of the storey-high families, every 0.3 m from 2.7 m.
const HEIGHTS: [&str; 12] = [
    "H2.7", "H3.0", "H3.3", "H3.6", "H3.9", "H4.2", "H4.5", "H4.8", "H5.1", "H5.4", "H5.7", "H6.0",
];
const WIDTHS: [(f64, &str); 4] = [(2.4, "W2400"), (3.0, "W3000"), (3.6, "W3600"), (4.8, "W4800")];
const PAD: f64 = 1.5;
const MAX_PADS: usize = 900;
/// Nearest two trees of a site stand this far apart, m.
const TREE_SPACING: f64 = 4.0;
/// Least size of a tree squeezed by its neighbours, as a share of its type.
const MIN_FIT: f64 = 0.4;

/// The tallest storey-high type that still fits under `h`.
pub(super) fn height_ty(h: f64) -> &'static str {
    let i = ((h - 2.7) / 0.3 + 1e-6).floor().clamp(0.0, 11.0) as usize;
    HEIGHTS[i]
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn len(a: P) -> f64 {
    a.0.hypot(a.1)
}

fn at(w: &GWall, t: f64) -> P {
    w.at(t)
}

/// Unit normal of `w` at `t` along it: the left one for `side` +1, the right one for -1.
fn normal(w: &GWall, side: f64, t: f64) -> P {
    let u = w.tangent(t);
    (-u.1 * side, u.0 * side)
}

fn in_room(l: &GLayout, p: P) -> Option<usize> {
    l.rooms.iter().position(|r| inside(&r.polygon, p))
}

/// Side of wall `wi` facing out of the building (+1 left, -1 right of a -> b), when one side
/// has a room and the other none: `None` for inner walls, including ends that a link or wing
/// now covers. An arc wall turns its normal along the arc.
pub(super) fn outward(l: &GLayout, wi: usize) -> Option<f64> {
    let w = &l.walls[wi];
    let wl = w.length();
    let samples = [0.25, 0.5, 0.75];
    let side = |s: f64| {
        samples.iter().all(|f| {
            let (m, n) = (at(w, wl * f), normal(w, s, wl * f));
            in_room(l, (m.0 + n.0 * 0.6, m.1 + n.1 * 0.6)).is_none()
        })
    };
    match (side(1.0), side(-1.0)) {
        (true, false) => Some(1.0),
        (false, true) => Some(-1.0),
        _ => None,
    }
}

/// Positions along wall `wi` where other walls meet it, with both ends, sorted.
pub(super) fn stops(l: &GLayout, wi: usize) -> Vec<f64> {
    let w = &l.walls[wi];
    let mut s = vec![0.0, w.length()];
    for (j, o) in l.walls.iter().enumerate() {
        if j == wi {
            continue;
        }
        for e in [o.a, o.b] {
            if w.dist(e) < 1e-6 {
                s.push(w.along(e));
            }
        }
    }
    s.sort_by(f64::total_cmp);
    s.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    s
}

/// Room behind the middle of a bay, seen from the `side` facing out.
fn bay_room(l: &GLayout, wi: usize, side: f64, lo: f64, hi: f64) -> Option<usize> {
    let t = (lo + hi) / 2.0;
    let (m, n) = (at(&l.walls[wi], t), normal(&l.walls[wi], side, t));
    in_room(l, (m.0 - n.0 * 0.6, m.1 - n.1 * 0.6))
}

fn openings_in(l: &GLayout, wi: usize, lo: f64, hi: f64) -> Vec<usize> {
    (0..l.openings.len())
        .filter(|&i| {
            let o = &l.openings[i];
            o.wall == wi && o.along + o.width > lo && o.along < hi
        })
        .collect()
}

/// Puts a window of `width` centred in [lo, hi] if it keeps its margins; true when placed.
pub(super) fn add_window(l: &mut GLayout, wi: usize, (lo, hi): (f64, f64), width: f64, sill: f64, height: f64) -> bool {
    let room = hi - lo - 2.0 * BAY_MARGIN;
    let w = width.min(room);
    if w < 0.6 || height < 0.6 {
        return false;
    }
    let along = (lo + hi - w) / 2.0;
    if !openings_in(l, wi, along - 0.15, along + w + 0.15).is_empty() {
        return false;
    }
    l.openings.push(GOpening {
        wall: wi,
        along,
        door: false,
        width: w,
        sill,
        height,
        ty: None,
    });
    true
}

/// Gives room `ri` a window on one of its outer bays; false when none has room for one.
pub(super) fn relight(l: &mut GLayout, ri: usize, h: f64) -> bool {
    let mut cand: Vec<Bay> = bays(l).into_iter().filter(|b| b.room == Some(ri) && !b.door).collect();
    cand.sort_by(|a, b| (b.hi - b.lo).total_cmp(&(a.hi - a.lo)));
    cand.iter()
        .any(|b| add_window(l, b.wall, (b.lo, b.hi), 1.5, SILL, (h - 1.2).clamp(0.9, 1.5)))
}

fn fixture(family: &'static str, ty: &'static str, p: P, n: P) -> GFixture {
    // local +y of a facade family points out of the building
    GFixture { scale: 1.0, min_clear: 0.0,
        family,
        ty,
        at: p,
        rotation: (-n.0).atan2(n.1),
        wall: None,
        z: 0.0,
    }
}

/// A point on the outer face of wall `wi`, `t` along it, `off` further out.
fn face(l: &GLayout, wi: usize, side: f64, t: f64, off: f64) -> P {
    let (m, n) = (at(&l.walls[wi], t), normal(&l.walls[wi], side, t));
    let o = WallRole::Exterior.thickness() / 2.0 + off;
    (m.0 + n.0 * o, m.1 + n.1 * o)
}

/// A facade family on the outer face of wall `wi`, `t` along it, facing out there.
fn on_face(l: &GLayout, wi: usize, side: f64, t: f64, family: &'static str, ty: &'static str) -> GFixture {
    fixture(family, ty, face(l, wi, side, t, 0.0), normal(&l.walls[wi], side, t))
}

struct Bay {
    wall: usize,
    /// Side of the wall facing out (see [`outward`]).
    side: f64,
    lo: f64,
    hi: f64,
    room: Option<usize>,
    door: bool,
}

fn bays(l: &GLayout) -> Vec<Bay> {
    let mut out = vec![];
    for wi in 0..l.walls.len() {
        if l.walls[wi].role != WallRole::Exterior {
            continue;
        }
        let Some(side) = outward(l, wi) else { continue };
        let s = stops(l, wi);
        for p in s.windows(2) {
            let (lo, hi) = (p[0], p[1]);
            let door = openings_in(l, wi, lo, hi).iter().any(|&i| l.openings[i].door);
            out.push(Bay {
                wall: wi,
                side,
                lo,
                hi,
                room: bay_room(l, wi, side, lo, hi),
                door,
            });
        }
    }
    out
}

/// Curtain wall standing off the outer face of the bay `lo..hi` of wall `wi`, one storey high:
/// the mullion module, spandrel band and section are rules. A curved wall takes one piece per
/// module, so the chords follow the arc.
fn curtain_bay(l: &GLayout, b: &Bay, h: f64) -> Vec<GCurtain> {
    let (module, spandrel) = (rule("facade.curtain_module"), rule("facade.curtain_spandrel"));
    let mullion = (rule("facade.curtain_mullion_w"), rule("facade.curtain_mullion_d"));
    // the axis of the mullions: the gap and half their depth beyond the wall's outer face
    let off = rule("facade.curtain_gap") + mullion.1 / 2.0;
    let w = &l.walls[b.wall];
    let pieces = if w.bulge.abs() > 1e-9 { ((b.hi - b.lo) / module).ceil().max(1.0) as usize } else { 1 };
    let spandrel = spandrel.min(h / 2.0);
    (0..pieces)
        .map(|i| {
            let (t0, t1) = (
                b.lo + (b.hi - b.lo) * i as f64 / pieces as f64,
                b.lo + (b.hi - b.lo) * (i + 1) as f64 / pieces as f64,
            );
            GCurtain {
                name: format!("Curtain {}", b.wall + 1),
                a: face(l, b.wall, b.side, t0, off),
                b: face(l, b.wall, b.side, t1, off),
                height: h,
                // one vision row over the spandrel: its transom is the spandrel's top
                spacing: (module, if spandrel > 0.0 { spandrel } else { h }),
                spandrel,
                mullion,
                wall: b.wall,
            }
        })
        .collect()
}

/// Re-glazes the bays that held windows; fixtures for fins and panels, curtain walls for the
/// curtain system.
fn skin(l: &mut GLayout, sys: FacadeSystem, rhythm: f64, h: f64) -> Vec<GCurtain> {
    let mut curtains = vec![];
    let mut runs: Vec<Run> = vec![];
    if sys == FacadeSystem::Punched {
        return curtains;
    }
    let ty = height_ty(h);
    for b in bays(l) {
        let (lo, hi, wi) = (b.lo, b.hi, b.wall);
        let had = openings_in(l, wi, lo, hi);
        let span = hi - lo;
        // fins stand in front of the punched windows, which stay as they are
        if !b.door && !had.is_empty() && span > 1.4 && sys != FacadeSystem::Fins {
            let keep: Vec<GOpening> = had.iter().map(|&i| l.openings[i].clone()).collect();
            let mut drop = had.clone();
            drop.sort_unstable_by(|a, b| b.cmp(a));
            for i in drop {
                l.openings.remove(i);
            }
            let placed = match sys {
                FacadeSystem::Ribbon => add_window(l, wi, (lo, hi), span, SILL, (h - 1.4).clamp(0.9, 1.5)),
                FacadeSystem::Curtain => add_window(l, wi, (lo, hi), span, 0.1, (h - 0.6).max(2.0)),
                FacadeSystem::Panels => {
                    let n = ((span / rhythm).round() as usize).max(1);
                    let step = span / n as f64;
                    let mut any = false;
                    for k in 0..n {
                        let c = lo + step * (k as f64 + 0.5);
                        any |= add_window(l, wi, (c - step / 2.0, c + step / 2.0), 0.9, 0.3, (h - 0.9).max(1.5));
                    }
                    any
                }
                FacadeSystem::Fins | FacadeSystem::Punched => false,
            };
            if !placed {
                l.openings.extend(keep);
            }
        }
        let side = b.side;
        match sys {
            // one wall per run of bays between doors: the piers between bays are glazed over
            FacadeSystem::Curtain if !b.door => match runs.last_mut() {
                // only a bay that starts where the run ends continues it
                Some(r) if r.wall == b.wall && !r.closed && (r.hi - b.lo).abs() < 1e-6 => r.hi = b.hi,
                _ => runs.push(Run { wall: b.wall, side: b.side, lo: b.lo, hi: b.hi, closed: false }),
            },
            FacadeSystem::Curtain => {
                // a door ends the run before it, whatever wall the run is on
                if let Some(r) = runs.last_mut() {
                    r.closed = true;
                }
            }
            FacadeSystem::Fins if !b.door => {
                let k = (span / rhythm).floor().max(1.0) as usize;
                for i in 0..=k {
                    let t = lo + 0.1 + (span - 0.2) * i as f64 / k as f64;
                    let f = on_face(l, wi, side, t, "Facade Fin", ty);
                    l.fixtures.push(f);
                }
            }
            FacadeSystem::Panels if !b.door => {
                let ops: Vec<(f64, f64)> = openings_in(l, wi, lo, hi)
                    .iter()
                    .map(|&i| (l.openings[i].along, l.openings[i].along + l.openings[i].width))
                    .collect();
                let mut t = lo + 0.05;
                while t + 1.2 <= hi - 0.05 {
                    if ops.iter().all(|o| t + 1.2 <= o.0 - 0.02 || t >= o.1 + 0.02) {
                        let f = on_face(l, wi, side, t + 0.6, "Facade Panel", ty);
                        l.fixtures.push(f);
                        t += 1.2;
                    } else {
                        t += 0.1;
                    }
                }
            }
            _ => {}
        }
    }
    for r in runs {
        let b = Bay { wall: r.wall, side: r.side, lo: r.lo, hi: r.hi, room: None, door: false };
        curtains.extend(curtain_bay(l, &b, h));
    }
    curtains
}

/// A stretch of one wall the curtain wall runs along.
struct Run {
    wall: usize,
    side: f64,
    lo: f64,
    hi: f64,
    /// A door bay ended it.
    closed: bool,
}

/// One balcony or loggia per habitable room above the ground, on its widest outer bay.
fn balconies(l: &mut GLayout, kind: Balconies, habitable: &[bool], h: f64, glazed: bool) {
    let fam = match kind {
        Balconies::None => return,
        Balconies::Balcony => "Balcony",
        Balconies::Loggia => "Loggia",
    };
    let mut best: Vec<Option<Bay>> = (0..l.rooms.len()).map(|_| None).collect();
    for b in bays(l) {
        let Some(r) = b.room.filter(|&r| habitable.get(r).copied().unwrap_or(false)) else {
            continue;
        };
        if b.door || b.hi - b.lo < WIDTHS[0].0 + 0.4 {
            continue;
        }
        if best[r].as_ref().is_none_or(|x| x.hi - x.lo < b.hi - b.lo) {
            best[r] = Some(b);
        }
    }
    for b in best.into_iter().flatten() {
        let span = b.hi - b.lo;
        let Some(&(w, ty)) = WIDTHS.iter().rev().find(|(w, _)| *w <= span - 0.4) else {
            continue;
        };
        let c = (b.lo + b.hi) / 2.0;
        if !glazed {
            // a French window opens onto it
            let had = openings_in(l, b.wall, b.lo, b.hi);
            let keep: Vec<GOpening> = had.iter().map(|&i| l.openings[i].clone()).collect();
            let mut drop = had;
            drop.sort_unstable_by(|a, b| b.cmp(a));
            for i in drop {
                l.openings.remove(i);
            }
            let ok = add_window(l, b.wall, (c - w / 2.0, c + w / 2.0), w - 0.6, 0.0, (h - 0.4).min(2.3));
            if !ok {
                l.openings.extend(keep);
            }
        }
        let f = on_face(l, b.wall, b.side, c, fam, ty);
        l.fixtures.push(f);
    }
}

fn canopies(l: &mut GLayout) {
    let mut add = vec![];
    for wi in 0..l.walls.len() {
        if l.walls[wi].role != WallRole::Exterior {
            continue;
        }
        let Some(side) = outward(l, wi) else { continue };
        for o in l.openings.iter().filter(|o| o.wall == wi && o.door) {
            let ty = if o.width > 1.6 { "6000x2400" } else { "3600x1800" };
            add.push(on_face(l, wi, side, o.along + o.width / 2.0, "Entrance Canopy", ty));
        }
    }
    l.fixtures.extend(add);
}

fn bbox(r: &[P]) -> (P, P) {
    r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
        ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
    })
}

/// Distance from `p` to the outline of the ground slab, its holes included.
fn to_outline(l: &GLayout, p: P) -> f64 {
    std::iter::once(&l.slab)
        .chain(l.slab_holes.iter())
        .flat_map(|r| (0..r.len()).map(move |i| dist_to_seg(p, r[i], r[(i + 1) % r.len()])))
        .fold(f64::MAX, f64::min)
}

fn built_on(l: &GLayout, p: P) -> bool {
    inside(&l.slab, p) && !l.slab_holes.iter().any(|h| inside(h, p))
}

/// Paving within `band` of the building, trees beyond it, on the ground storey.
fn site(l: &mut GLayout, band: f64, trees: u32, courtyard: Option<&[P]>) -> Result<(), GenError> {
    let (lo, hi) = bbox(&l.slab);
    let clear = WallRole::Exterior.thickness() / 2.0 + PAD / 2.0;
    let mut pads = vec![];
    if band > 0.0 {
        let reach = band + PAD;
        let (nx, ny) = (((hi.0 - lo.0 + 2.0 * reach) / PAD) as usize, ((hi.1 - lo.1 + 2.0 * reach) / PAD) as usize);
        for i in 0..=nx {
            for j in 0..=ny {
                let p = (lo.0 - reach + PAD * (i as f64 + 0.5), lo.1 - reach + PAD * (j as f64 + 0.5));
                let d = to_outline(l, p);
                if !built_on(l, p) && d >= clear - 1e-9 && d <= band + clear && pads.len() < MAX_PADS {
                    pads.push(p);
                }
            }
        }
    }
    let doors: Vec<P> = l
        .openings
        .iter()
        .filter(|o| o.door && l.walls[o.wall].role == WallRole::Exterior)
        .map(|o| at(&l.walls[o.wall], o.along + o.width / 2.0))
        .collect();
    let cfg = super::landscape::Cfg::read();
    let land = super::landscape::lay(l, band, clear, PAD, &doors, pads.len(), trees, &cfg)?;
    let mut spots: Vec<P> = vec![];
    if trees > 0 {
        let (in_court, out) = match courtyard {
            Some(_) => (trees / 3, trees - trees / 3),
            None => (0, trees),
        };
        let off = land.tree_off;
        let ring = [
            (lo.0 - off, lo.1 - off),
            (hi.0 + off, lo.1 - off),
            (hi.0 + off, hi.1 + off),
            (lo.0 - off, hi.1 + off),
        ];
        let perim = 2.0 * (hi.0 - lo.0 + hi.1 - lo.1 + 4.0 * off);
        // three candidates per tree round the ring; entrances stay open
        let cands = (out * 3).max(1);
        for k in 0..cands {
            if spots.len() as u32 >= out {
                break;
            }
            let s = perim * ((k * 3) % cands) as f64 / cands as f64
                + perim * ((k * 3) / cands) as f64 / (3 * cands) as f64;
            let mut rest = s;
            let mut p = ring[0];
            for e in 0..4 {
                let (a, b) = (ring[e], ring[(e + 1) % 4]);
                let el = len(sub(b, a));
                if rest <= el {
                    p = (a.0 + (b.0 - a.0) * rest / el, a.1 + (b.1 - a.1) * rest / el);
                    break;
                }
                rest -= el;
            }
            let near_door = doors.iter().any(|d| len(sub(*d, p)) < off + 2.0);
            let crowded = spots.iter().any(|q| len(sub(*q, p)) < TREE_SPACING);
            if !near_door && !crowded && land.tree_ok(l, p) {
                spots.push(p);
            }
        }
        // rows along the street and the paths: kept clear of the ring trees and each other
        for p in &land.rows {
            if spots.iter().all(|q| len(sub(*q, *p)) >= TREE_SPACING) {
                spots.push(*p);
            }
        }
        if let Some(c) = courtyard {
            let (clo, chi) = bbox(c);
            let ctr = ((clo.0 + chi.0) / 2.0, (clo.1 + chi.1) / 2.0);
            let r = ((chi.0 - clo.0).min(chi.1 - clo.1) / 2.0 - 3.0).max(0.0);
            for i in 0..in_court {
                let a = std::f64::consts::TAU * f64::from(i) / f64::from(in_court.max(1));
                spots.push((ctr.0 + r * a.cos() * 0.6, ctr.1 + r * a.sin() * 0.6));
            }
        }
    }
    l.fixtures.extend(land.fixtures());
    for p in pads {
        l.fixtures.push(GFixture { scale: 1.0, min_clear: 0.0,
            family: "Paving Slab",
            ty: "1500",
            at: p,
            rotation: 0.0,
            wall: None,
            z: 0.0,
        });
    }
    // species and turn per tree from the library's Tree family and the site's tree mix; the
    // seed is the plot box, so a site keeps its trees and another site gets other ones
    let seed = [lo.0, lo.1, hi.0, hi.1].iter().fold(0u64, |a, v| a.rotate_left(17) ^ (v * 1000.0).round() as i64 as u64);
    let picks = super::trees::sequence(&super::trees::mix_name(), &super::trees::season_name(), seed, spots.len());
    if picks.len() < spots.len() {
        return Err(GenError::Infeasible(format!(
            "the tree mix `{}` in the season `{}` has no plantable type in the library family Tree",
            super::trees::mix_name(),
            super::trees::season_name()
        )));
    }
    let jitter = super::rules::rule("site.tree_size_jitter");
    let floor = super::rules::rule("site.tree_min_clear");
    let eaves = super::rules::rule("site.tree_building_clear") + super::rules::rule("massing.roof_overhang");
    // placed trees: plan point and crown radius, so that no two crowns meet
    let mut placed: Vec<(P, f64)> = vec![];
    for (k, ((ty, rotation), p)) in picks.into_iter().zip(spots).enumerate() {
        let (height, crown, _) = super::trees::dims(ty).unwrap_or((8.0, 4.0, 2.5));
        let mut scale = super::trees::scale(seed, k, jitter);
        for (q, rq) in &placed {
            scale = scale.min(2.0 * (len(sub(*q, p)) - rq) / crown);
        }
        // nor any crown the building: its walls and the eaves of its roof
        scale = scale.min(2.0 * (to_outline(l, p) - eaves) / crown);
        // a crown squeezed below this share of its type is no tree of that species: leave the spot
        if scale < MIN_FIT {
            continue;
        }
        placed.push((p, 0.5 * crown * scale));
        let min_clear = floor.min(0.4 * height * scale);
        l.fixtures.push(GFixture { scale, min_clear, family: "Tree", ty, at: p, rotation, wall: None, z: 0.0 });
    }
    Ok(())
}

/// Planters along the edge of a roof ring, `ty` lifting them onto the deck.
fn planters(l: &mut GLayout, ring: &[P], inset: f64, ty: &'static str, skip: Option<&[P]>) {
    let n = ring.len();
    let area: f64 = (0..n).map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1).sum();
    let ccw = area > 0.0;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let d = sub(b, a);
        let el = len(d);
        let u = (d.0 / el, d.1 / el);
        // inward normal of the edge
        let nin = if ccw { (-u.1, u.0) } else { (u.1, -u.0) };
        let k = ((el - 2.0) / 4.0).floor().max(0.0) as usize;
        for j in 0..k {
            let t = 2.0 + 4.0 * j as f64 + 1.0;
            let p = (a.0 + u.0 * t + nin.0 * inset, a.1 + u.1 * t + nin.1 * inset);
            if skip.is_some_and(|s| inside(s, p)) {
                continue;
            }
            l.fixtures.push(GFixture { scale: 1.0, min_clear: 0.0,
                family: "Planter",
                ty,
                at: p,
                rotation: u.1.atan2(u.0),
                wall: None,
                z: 0.0,
            });
        }
    }
}

/// Glass rails along a podium edge.
fn rails(l: &mut GLayout, ring: &[P], inset: f64) {
    let n = ring.len();
    let area: f64 = (0..n).map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1).sum();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let d = sub(b, a);
        let el = len(d);
        let u = (d.0 / el, d.1 / el);
        let nin = if area > 0.0 { (-u.1, u.0) } else { (u.1, -u.0) };
        let k = (el / 3.0).floor() as usize;
        let start = (el - 3.0 * k as f64) / 2.0;
        for j in 0..k {
            let t = start + 3.0 * j as f64 + 1.5;
            l.fixtures.push(GFixture { scale: 1.0, min_clear: 0.0,
                family: "Glass Rail",
                ty: "3000",
                at: (a.0 + u.0 * t + nin.0 * inset, a.1 + u.1 * t + nin.1 * inset),
                rotation: u.1.atan2(u.0),
                wall: None,
                z: 0.0,
            });
        }
    }
}

/// Applies the brief's design to a built project at the origin.
pub(super) fn apply(spec: &ProjectSpec, p: &mut GProject) -> Result<(), GenError> {
    let d = &spec.design;
    let n = p.storeys.len();
    let glazed = d.facade == FacadeSystem::Curtain;
    let court = p.court.clone();
    for k in 0..n {
        let s = &mut p.storeys[k];
        let h = s.height;
        let l = &mut s.layout;
        s.curtains = skin(l, d.facade, d.rhythm, h);
        if k > 0 {
            let hab: Vec<bool> = s.rooms.iter().map(|r| r.kind.habitable()).collect();
            balconies(l, d.balconies, &hab, h, glazed);
        }
        if k == 0 {
            if d.canopy {
                canopies(l);
            }
            site(l, d.paving, d.trees, court.as_deref())?;
        }
    }
    if d.terrace {
        if let Some(lower) = p.podium.clone() {
            // the podium roof is the floor of the first upper storey
            let k = p.storeys.iter().position(|s| s.layout.slab == lower).unwrap_or(0);
            if k > 0 {
                let tower = p.storeys[k].layout.walls.clone();
                let l = &mut p.storeys[k].layout;
                rails(l, &lower, 0.2);
                let (tlo, thi) = bbox(&tower.iter().flat_map(|w| [w.a, w.b]).collect::<Vec<_>>());
                let tring = vec![(tlo.0, tlo.1), (thi.0, tlo.1), (thi.0, thi.1), (tlo.0, thi.1)];
                planters(l, &lower, 1.2, "H0", Some(&tring));
            }
        }
        if spec.roof == RoofKind::Flat {
            if let Some(top) = p.storeys.last_mut() {
                let ty = height_ty(top.height);
                let ring = top.layout.slab.clone();
                planters(&mut top.layout, &ring, 1.0, ty, None);
            }
        }
    }
    for (k, s) in p.storeys.iter().enumerate() {
        if s.layout.fixtures.len() > super::slots::FIXTURES_MAX as usize {
            return Err(GenError::Infeasible(format!(
                "{} fixtures on floor {}; at most {}: less paving or fewer trees",
                s.layout.fixtures.len(),
                k + 1,
                super::slots::FIXTURES_MAX
            )));
        }
        if s.curtains.len() > super::slots::CURTAINS_MAX as usize {
            return Err(GenError::Infeasible(format!(
                "{} curtain walls on floor {}; at most {}: a larger facade module or fewer curved walls",
                s.curtains.len(),
                k + 1,
                super::slots::CURTAINS_MAX
            )));
        }
        crate::check::validate_layout(&s.layout)
            .map_err(|e| GenError::Infeasible(format!("floor {} after the facade: {e}", k + 1)))?;
    }
    Ok(())
}
