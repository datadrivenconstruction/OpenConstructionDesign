//! Split levels on a steep site. The building parts on wall lines across the slope, one cut
//! per wing, and every face of the plan downhill of a cut stands half a storey lower; a wing
//! without a cut stands whole on the level its neighbours give it. Flights in the corridors
//! and lobbies on the cuts join the half-levels. A lift serves both: as a through car on a cut,
//! else by `site.split_lift` a platform lift beside each flight or a second lift, and every
//! room keeps a barrier-free way to an entrance.

use std::collections::{BTreeMap, BTreeSet};

use super::super::footprint::{self, P};
use super::super::{build, names, slots, GProject, PRoof, PRoom, PStorey, ProjectSpec, RoomKind};
use super::{area, dist_ring, dist_seg, inside, rule, Approach, SplitLift};
use crate::{GFixture, GLayout, GOpening, GShaft, GStair, GWall, WallRole};
use ddc_geom::stair::Shape;

/// The library family and type of the platform lift beside a half-level flight.
const PLATFORM: (&str, &str) = ("Platform Lift", "1100x1400");

/// A face of a parted plan: its outer ring and holes.
pub type Face = (Vec<P>, Vec<Vec<P>>);

/// One cut: the chord of a wall line across one wing of the ground slab.
#[derive(Clone, Debug, PartialEq)]
pub struct GCut {
    /// Ends of the chord on the ground slab's outline.
    pub a: P,
    pub b: P,
    /// Unit normal of the line towards the lower side; `q . low = at` on the line.
    pub low: P,
    pub at: f64,
}

impl GCut {
    fn f(&self, q: P) -> f64 {
        q.0 * self.low.0 + q.1 * self.low.1 - self.at
    }

    fn u(&self) -> P {
        (-self.low.1, self.low.0)
    }

    /// Position of `q` along the line.
    fn t(&self, q: P) -> f64 {
        let u = self.u();
        q.0 * u.0 + q.1 * u.1
    }

    fn at_t(&self, t: f64) -> P {
        let u = self.u();
        (self.low.0 * self.at + u.0 * t, self.low.1 * self.at + u.1 * t)
    }

    fn extent(&self) -> (f64, f64) {
        let (s, e) = (self.t(self.a), self.t(self.b));
        (s.min(e), s.max(e))
    }

    fn overlaps(&self, s: f64, e: f64, tol: f64) -> bool {
        let (lo, hi) = self.extent();
        e.min(hi) - s.max(lo) > tol
    }

    /// Whether `q` lies on the chord.
    fn on(&self, q: P, tol: f64) -> bool {
        let (lo, hi) = self.extent();
        let t = self.t(q);
        self.f(q).abs() <= tol && t >= lo - tol && t <= hi + tol
    }

    /// Where segment `a`-`b` passes through the chord from one side to the other, as the
    /// parameter along the segment.
    fn spans(&self, a: P, b: P, tol: f64) -> Option<f64> {
        let (fa, fb) = (self.f(a), self.f(b));
        if !((fa > tol && fb < -tol) || (fa < -tol && fb > tol)) {
            return None;
        }
        let s = fa / (fa - fb);
        self.on((a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s), tol).then_some(s)
    }

    /// Whether a ring has points on both sides of the line beside the chord.
    fn crosses(&self, ring: &[P], tol: f64) -> bool {
        let (lo, hi) = ring.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(self.f(*q)), a.1.max(self.f(*q))));
        if lo >= -tol || hi <= tol {
            return false;
        }
        let (s, e) = ring.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(self.t(*q)), a.1.max(self.t(*q))));
        self.overlaps(s, e, tol)
    }

    /// The stretches of the line inside a region with holes, as ranges along the line.
    fn stretches(&self, outer: &[P], holes: &[Vec<P>]) -> Vec<(f64, f64)> {
        let mut ts = vec![];
        for r in std::iter::once(outer).chain(holes.iter().map(Vec::as_slice)) {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                let (fa, fb) = (self.f(a), self.f(b));
                if (fa > 0.0) != (fb > 0.0) {
                    let s = fa / (fa - fb);
                    ts.push(self.t((a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s)));
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        ts.chunks_exact(2).map(|c| (c[0], c[1])).collect()
    }

    fn shift(&mut self, o: P) {
        self.a = (self.a.0 + o.0, self.a.1 + o.1);
        self.b = (self.b.0 + o.0, self.b.1 + o.1);
        self.at += o.0 * self.low.0 + o.1 * self.low.1;
    }
}

/// How a lift reaches both half-levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// A lift car on a cut with a door on each face, one per half-level.
    Through,
    /// A platform lift beside a half-level flight, from the lower floor to the upper.
    Platform,
    /// A second lift on the half-levels the first does not stop at.
    Lift,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GAccess {
    /// The storey block, as `GProject::blocks` numbers it.
    pub block: u32,
    pub kind: Access,
    /// The room it stands in: the corridor or lobby of a platform lift, else the lift.
    pub room: usize,
    pub at: P,
}

/// Split levels: the cuts, the faces of the plan on the lower half-level, the flights and the
/// lifts that join the half-levels. Each block keeps whole storeys, so its risers run straight.
#[derive(Clone, Debug, PartialEq)]
pub struct GSplit {
    pub cuts: Vec<GCut>,
    /// The ground slab's faces on the lower half-level.
    pub lows: Vec<Face>,
    pub tol: f64,
    /// How far the lower half-levels stand below the upper: half the ground storey.
    pub drop: f64,
    /// Share of the ground slab on the lower half-level.
    pub share: f64,
    /// Flights from a lower half-level up to its upper one: the storey block and the stair,
    /// its foot on the lower half-level.
    pub flights: Vec<(u32, GStair)>,
    pub access: Vec<GAccess>,
    /// Cut and fill of the one pad the split levels replace, m3.
    pub plinth_cut: f64,
    pub plinth_fill: f64,
}

impl GSplit {
    fn new(cuts: Vec<GCut>, tol: f64, drop: f64) -> GSplit {
        GSplit {
            cuts,
            lows: vec![],
            tol,
            drop,
            share: 0.0,
            flights: vec![],
            access: vec![],
            plinth_cut: 0.0,
            plinth_fill: 0.0,
        }
    }

    fn near_cut(&self, q: P) -> bool {
        self.cuts.iter().any(|c| c.on(q, self.tol))
    }

    /// Whether `q` stands on the lower half-level; a wall on a cut stays with the upper.
    pub fn is_low(&self, q: P) -> bool {
        !self.near_cut(q)
            && self.lows.iter().any(|(o, hs)| {
                (inside(q, o) || dist_ring(q, o) <= self.tol) && !hs.iter().any(|h| inside(q, h) && dist_ring(q, h) > self.tol)
            })
    }

    fn is_high(&self, q: P) -> bool {
        !self.near_cut(q) && !self.is_low(q)
    }

    /// Whether a ring has points on both half-levels.
    pub fn crosses(&self, ring: &[P]) -> bool {
        ring.iter().any(|q| self.is_low(*q)) && ring.iter().any(|q| self.is_high(*q))
    }

    /// The half-level of a ring that keeps to one, by its centroid.
    pub fn ring_low(&self, ring: &[P]) -> bool {
        !ring.is_empty() && self.is_low(footprint::centroid(ring))
    }

    /// Name of the level of a storey's lower half-level.
    pub fn low_name(name: &str) -> String {
        format!("{name} split")
    }

    /// Whether segment `a`-`b` lies on the line of a cut.
    pub(super) fn on_line(&self, a: P, b: P) -> bool {
        self.cuts.iter().any(|c| c.f(a).abs() <= self.tol && c.f(b).abs() <= self.tol)
    }

    /// The chords of the cuts across a region: each stretch of a cut's line inside it that
    /// runs beside the cut, with the cut's index.
    pub(super) fn chords(&self, outer: &[P], holes: &[Vec<P>]) -> Vec<(P, P, usize)> {
        let mut out = vec![];
        for (i, c) in self.cuts.iter().enumerate() {
            for (s, e) in c.stretches(outer, holes) {
                if e - s > self.tol && c.overlaps(s, e, self.tol) {
                    out.push((c.at_t(s), c.at_t(e), i));
                }
            }
        }
        out
    }

    /// A region parted on the cuts: its faces on the upper half-level, then those on the
    /// lower; `None` when a face would stand on both.
    pub fn pieces(&self, outer: &[P], holes: &[Vec<P>]) -> Option<(Vec<Face>, Vec<Face>)> {
        let cs = self.chords(outer, holes);
        let faces = carve(outer, holes, &cs, self.tol)?;
        let eps = self.tol * 2.0;
        let mut marks = vec![(false, false); faces.len()];
        for &(a, b, i) in &cs {
            let n = self.cuts[i].low;
            let m = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
            for (s, low) in [(eps, true), (-eps, false)] {
                let q = (m.0 + n.0 * s, m.1 + n.1 * s);
                if let Some(j) = faces.iter().position(|f| in_face(q, f)) {
                    if low {
                        marks[j].1 = true;
                    } else {
                        marks[j].0 = true;
                    }
                }
            }
        }
        let (mut hi, mut lo) = (vec![], vec![]);
        for (f, (h, l)) in faces.into_iter().zip(marks) {
            if h && l {
                return None;
            }
            let low = if h || l { l } else { self.is_low(inner(&f.0)) };
            if low {
                lo.push(f);
            } else {
                hi.push(f);
            }
        }
        Some((hi, lo))
    }

    /// A roof parted on the cuts: the upper roofs, then the lower, each a roof of its own
    /// with its own ridge. An edge on a cut takes the form of the roof's edge most nearly
    /// along the cut: a gable stays a gable, a hip a hip.
    pub fn roofs(&self, r: &PRoof) -> (Vec<PRoof>, Vec<PRoof>) {
        let Some((hi, lo)) = self.pieces(&r.ring, &[]) else {
            return (vec![r.clone()], vec![]);
        };
        if hi.len() + lo.len() == 1 {
            return if hi.is_empty() { (vec![], vec![r.clone()]) } else { (vec![r.clone()], vec![]) };
        }
        // a vault spans its whole roof: the parted pieces are plain roofs, each on its half-level
        let mk = |f: Face| PRoof { edges: self.roof_edges(&f.0, r), ring: f.0, vault: None, ..r.clone() };
        (hi.into_iter().map(mk).collect(), lo.into_iter().map(mk).collect())
    }

    fn roof_edges(&self, ring: &[P], r: &PRoof) -> Vec<Option<f64>> {
        if r.edges.is_empty() {
            return vec![];
        }
        let (n, m) = (ring.len(), r.ring.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let cut = self.cuts.iter().find(|c| c.f(p).abs() <= self.tol && c.f(q).abs() <= self.tol);
            let j = match cut {
                Some(c) => {
                    let across = |j: &usize| {
                        let (a, b) = (r.ring[*j], r.ring[(*j + 1) % m]);
                        let l = (b.0 - a.0).hypot(b.1 - a.1);
                        if l <= self.tol {
                            1.0
                        } else {
                            ((b.0 - a.0) * c.low.0 + (b.1 - a.1) * c.low.1).abs() / l
                        }
                    };
                    (0..m).min_by(|x, y| across(x).total_cmp(&across(y)))
                }
                None => (0..m).find(|&j| {
                    let (a, b) = (r.ring[j], r.ring[(j + 1) % m]);
                    dist_seg(p, a, b) <= self.tol && dist_seg(q, a, b) <= self.tol
                }),
            };
            out.push(j.map_or(Some(r.slope), |j| r.edges.get(j).copied().unwrap_or(Some(r.slope))));
        }
        out
    }

    pub(in super::super) fn shift(&mut self, o: P) {
        let mv = |q: &mut P| *q = (q.0 + o.0, q.1 + o.1);
        for c in &mut self.cuts {
            c.shift(o);
        }
        for (r, hs) in &mut self.lows {
            r.iter_mut().for_each(mv);
            for h in hs.iter_mut() {
                h.iter_mut().for_each(mv);
            }
        }
        for f in &mut self.flights {
            mv(&mut f.1.origin);
        }
        for a in &mut self.access {
            mv(&mut a.at);
        }
    }
}

fn in_face(q: P, f: &Face) -> bool {
    inside(q, &f.0) && !f.1.iter().any(|h| inside(q, h))
}

/// A point inside a ring: its centroid, else a point on a median of a fan triangle inside it.
fn inner(r: &[P]) -> P {
    let c = footprint::centroid(r);
    if inside(c, r) || r.len() < 3 {
        return c;
    }
    (1..r.len() - 1)
        .map(|i| {
            let m = ((r[i].0 + r[i + 1].0) * 0.5, (r[i].1 + r[i + 1].1) * 0.5);
            ((r[0].0 + m.0) * 0.5, (r[0].1 + m.1) * 0.5)
        })
        .find(|q| inside(*q, r))
        .unwrap_or(c)
}

fn face_area(f: &Face) -> f64 {
    area(&f.0) - f.1.iter().map(|h| area(h)).sum::<f64>()
}

/// The ring with `q` made a vertex where it lies on an edge.
fn insert(r: &[P], q: P, tol: f64) -> Vec<P> {
    if r.iter().any(|v| (v.0 - q.0).hypot(v.1 - q.1) <= tol) {
        return r.to_vec();
    }
    let n = r.len();
    match (0..n).find(|&i| dist_seg(q, r[i], r[(i + 1) % n]) <= tol) {
        Some(i) => {
            let mut out = r.to_vec();
            out.insert(i + 1, q);
            out
        }
        None => r.to_vec(),
    }
}

fn vertex(r: &[P], q: P, tol: f64) -> Option<usize> {
    r.iter().position(|v| (v.0 - q.0).hypot(v.1 - q.1) <= tol)
}

/// A region parted along chords that run through it from boundary to boundary: a chord
/// between two points of a face's outer ring splits the face, one from the outer ring to a
/// hole opens the hole into the outer ring. `None` when a chord meets no face that way.
fn carve(outer: &[P], holes: &[Vec<P>], chords: &[(P, P, usize)], tol: f64) -> Option<Vec<Face>> {
    let mut faces: Vec<Face> = vec![(outer.to_vec(), holes.to_vec())];
    for &(a, b, _) in chords {
        let m = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        let k = faces.iter().position(|f| in_face(m, f))?;
        let (o, mut hs) = faces.swap_remove(k);
        let on = (dist_ring(a, &o) <= tol, dist_ring(b, &o) <= tol);
        if on.0 && on.1 {
            let r = insert(&insert(&o, a, tol), b, tol);
            let (i, j) = (vertex(&r, a, tol)?, vertex(&r, b, tol)?);
            let (i, j) = (i.min(j), i.max(j));
            let one: Vec<P> = r[i..=j].to_vec();
            let two: Vec<P> = r[j..].iter().chain(&r[..=i]).copied().collect();
            if one.len() < 3 || two.len() < 3 {
                return None;
            }
            let (h1, h2): (Vec<_>, Vec<_>) = hs.into_iter().partition(|h| inside(inner(h), &one));
            faces.push((one, h1));
            faces.push((two, h2));
            continue;
        }
        let (p, q) = match on {
            (true, false) => (a, b),
            (false, true) => (b, a),
            _ => return None,
        };
        let hk = hs.iter().position(|h| dist_ring(q, h) <= tol)?;
        let mut h = hs.swap_remove(hk);
        if (footprint::signed_area(&h) > 0.0) == (footprint::signed_area(&o) > 0.0) {
            h.reverse();
        }
        let r = insert(&o, p, tol);
        let i = vertex(&r, p, tol)?;
        let h = insert(&h, q, tol);
        let j = vertex(&h, q, tol)?;
        let mut ring: Vec<P> = r[..=i].to_vec();
        ring.extend(h[j..].iter().chain(&h[..=j]));
        ring.extend(&r[i..]);
        faces.push((ring, hs));
    }
    Some(faces)
}

/// The plan outline of a stair's flights and landings.
pub(super) fn stair_ring(s: &GStair) -> Option<Vec<P>> {
    let lay = s.def().layout().ok()?;
    Some(
        lay.flights
            .iter()
            .flat_map(|f| f.ring(lay.tread))
            .chain(lay.landings.iter().flat_map(|l| l.ring.clone()))
            .map(|v| (v.x, v.y))
            .collect(),
    )
}

/// Corridors and lobbies may cross a cut: a flight in them joins the half-levels.
fn joins(k: RoomKind) -> bool {
    matches!(k, RoomKind::Corridor | RoomKind::Lobby)
}

/// Centre of a door and the wall's half thickness.
fn door_points(l: &GLayout) -> Vec<(P, f64, usize)> {
    let mut out = vec![];
    for (k, o) in l.openings.iter().enumerate().filter(|(_, o)| o.door) {
        let w = &l.walls[o.wall];
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        if len <= 0.0 {
            continue;
        }
        let t = o.along + o.width * 0.5;
        let q = (w.a.0 + (w.b.0 - w.a.0) / len * t, w.a.1 + (w.b.1 - w.a.1) / len * t);
        out.push((q, w.role.thickness() * 0.5, k));
    }
    out
}

/// Where the line of a cut enters and leaves a ring that crosses it, ordered along the line.
fn span(ring: &[P], c: &GCut, tol: f64) -> Option<(P, P)> {
    let n = ring.len();
    let mut pts: Vec<P> = vec![];
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (fa, fb) = (c.f(a), c.f(b));
        if fa.abs() <= tol {
            pts.push(a);
        } else if (fa > tol && fb < -tol) || (fa < -tol && fb > tol) {
            let s = fa / (fa - fb);
            pts.push((a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s));
        }
    }
    let lo = pts.iter().copied().min_by(|x, y| c.t(*x).total_cmp(&c.t(*y)))?;
    let hi = pts.iter().copied().max_by(|x, y| c.t(*x).total_cmp(&c.t(*y)))?;
    Some((lo, hi))
}

/// The half-level flight in a corridor or lobby on a cut, as wide as the room clear of its
/// walls; with `aside`, the plan room (width, depth) a platform lift takes beside it, the
/// flight narrowing to leave it and the platform standing on the lower floor at the step.
/// The flight stands in the lower block when it keeps clear of the doors there, else rises
/// into the upper block.
#[allow(clippy::too_many_arguments)]
fn flight(
    spec: &ProjectSpec,
    l: &GLayout,
    room: &[P],
    c: &GCut,
    drop: f64,
    tol: f64,
    aside: Option<(f64, f64)>,
) -> Option<(GStair, Option<GFixture>)> {
    let (a, b) = span(room, c, tol)?;
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let th = WallRole::Exterior.thickness();
    let clear = len - th;
    if len <= tol || clear <= tol {
        return None;
    }
    let u = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let along = |s: f64| (a.0 + u.0 * (th * 0.5 + s), a.1 + u.1 * (th * 0.5 + s));
    let least = rule("site.split_flight_min");
    let gap = WallRole::Partition.thickness();
    let places: Vec<(P, f64, Option<P>)> = match aside {
        None => vec![(along(clear * 0.5), clear, None)],
        Some((pw, _)) => {
            let w = clear - pw - gap;
            vec![(along(pw + gap + w * 0.5), w, Some(along(pw * 0.5))), (along(w * 0.5), w, Some(along(clear - pw * 0.5)))]
        }
    };
    let angle = (-c.low.1).atan2(-c.low.0);
    let reach = WallRole::Corridor.thickness();
    let mut doors: Vec<P> = vec![];
    for o in l.openings.iter().filter(|o| o.door) {
        let w = &l.walls[o.wall];
        let wl = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        if wl <= tol {
            continue;
        }
        let wu = ((w.b.0 - w.a.0) / wl, (w.b.1 - w.a.1) / wl);
        for t in [o.along, o.along + o.width * 0.5, o.along + o.width] {
            doors.push((w.a.0 + wu.0 * t, w.a.1 + wu.1 * t));
        }
    }
    let clear_of = |ring: &[P]| doors.iter().all(|q| !inside(*q, ring) && dist_ring(*q, ring) > reach);
    let within = |ring: &[P]| ring.iter().all(|q| inside(*q, room) || dist_ring(*q, room) <= tol);
    for (mid, width, pl) in places {
        if width < least {
            continue;
        }
        let make = |origin: P| GStair {
            origin,
            angle,
            shape: Shape::Straight,
            width,
            max_riser: spec.max_riser,
            landing: width,
            height: drop,
        };
        let Ok(lay) = make(mid).def().layout() else { continue };
        let run = lay.tread * lay.risers.saturating_sub(1) as f64;
        let platform = match (pl, aside) {
            (Some(q), Some((pw, pd))) => {
                let n = c.low;
                let h = (u.0 * pw * 0.5, u.1 * pw * 0.5);
                let ring = vec![
                    (q.0 - h.0, q.1 - h.1),
                    (q.0 + h.0, q.1 + h.1),
                    (q.0 + h.0 + n.0 * pd, q.1 + h.1 + n.1 * pd),
                    (q.0 - h.0 + n.0 * pd, q.1 - h.1 + n.1 * pd),
                ];
                if !within(&ring) || !clear_of(&ring) {
                    continue;
                }
                // the family's +y, its upper exit, faces the step
                Some(GFixture { scale: 1.0, min_clear: 0.0,
                    family: PLATFORM.0,
                    ty: PLATFORM.1,
                    at: (q.0 + n.0 * pd * 0.5, q.1 + n.1 * pd * 0.5),
                    rotation: n.0.atan2(-n.1),
                    wall: None,
                    z: 0.0,
                })
            }
            _ => None,
        };
        for origin in [(mid.0 + c.low.0 * run, mid.1 + c.low.1 * run), mid] {
            let st = make(origin);
            let Some(ring) = stair_ring(&st) else { continue };
            let apart = platform.as_ref().is_none_or(|f| !inside(f.at, &ring));
            if within(&ring) && clear_of(&ring) && apart {
                return Some((st, platform));
            }
        }
    }
    None
}

/// A lift room on a cut whose two faces along the line each open on a corridor or lobby: the
/// car takes a door on each face, on its own half-level. Gives the doors still to add.
fn through(l: &GLayout, rooms: &[PRoom], i: usize, c: &GCut, tol: f64) -> Option<Vec<GOpening>> {
    let r = &l.rooms[i].polygon;
    if r.len() != 4 {
        return None;
    }
    let door = door_points(l).into_iter().find(|d| dist_ring(d.0, r) <= d.1 + tol).map(|d| l.openings[d.2].clone())?;
    let mut out = vec![];
    let mut faces = 0;
    for e in 0..4 {
        let (a, b) = (r[e], r[(e + 1) % 4]);
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        if len <= tol || ((b.0 - a.0) * c.low.0 + (b.1 - a.1) * c.low.1).abs() > tol * len {
            continue;
        }
        let m = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        if c.f(m).abs() <= tol {
            return None;
        }
        faces += 1;
        let open = l.rooms.iter().zip(rooms).enumerate().any(|(j, (g, pr))| j != i && joins(pr.kind) && dist_ring(m, &g.polygon) <= tol);
        if !open {
            return None;
        }
        let w = l.walls.iter().position(|w| dist_seg(m, w.a, w.b) <= tol)?;
        let wall = &l.walls[w];
        let wl = (wall.b.0 - wall.a.0).hypot(wall.b.1 - wall.a.1);
        if wl <= tol {
            return None;
        }
        let s = ((m.0 - wall.a.0) * (wall.b.0 - wall.a.0) + (m.1 - wall.a.1) * (wall.b.1 - wall.a.1)) / wl;
        let has = l.openings.iter().any(|o| o.door && o.wall == w && s >= o.along - tol && s <= o.along + o.width + tol);
        if !has {
            out.push(GOpening { wall: w, along: s - door.width * 0.5, ..door.clone() });
        }
    }
    (faces == 2).then_some(out)
}

/// Whether one cut parts every storey: no room but a corridor or lobby (or a lift car with a
/// door on each face) across it, a flight in each of those, a corridor wherever rooms meet
/// the cut, no door, window, hole, shaft or stair on it.
fn check_cut(spec: &ProjectSpec, p: &GProject, c: &GCut, drop: f64, tol: f64) -> Result<(), String> {
    for (_, st) in p.blocks() {
        let l = &st.layout;
        let car = |i: usize| st.rooms[i].kind == RoomKind::Lift && through(l, &st.rooms, i, c, tol).is_some();
        let mut joined = false;
        for (i, (gr, pr)) in l.rooms.iter().zip(&st.rooms).enumerate() {
            if !c.crosses(&gr.polygon, tol) || car(i) {
                continue;
            }
            if !joins(pr.kind) {
                return Err(format!("{}: room {} would stand on both half-levels", st.name, pr.number));
            }
            flight(spec, l, &gr.polygon, c, drop, tol, None)
                .ok_or_else(|| format!("{}: no room for a half-level flight clear of the doors in {}", st.name, pr.number))?;
            joined = true;
        }
        let touched = l.rooms.iter().any(|r| r.polygon.iter().any(|q| c.on(*q, tol)));
        if touched && !joined {
            return Err(format!("{}: no corridor or lobby crosses the cut to join the half-levels", st.name));
        }
        let in_car = |r: &[P]| {
            l.rooms.iter().enumerate().any(|(i, g)| {
                c.crosses(&g.polygon, tol) && car(i) && r.iter().all(|q| inside(*q, &g.polygon) || dist_ring(*q, &g.polygon) <= tol)
            })
        };
        let held = l.slab_holes.iter().any(|r| c.crosses(r, tol)) || l.shafts.iter().any(|s| c.crosses(&s.ring, tol) && !in_car(&s.ring));
        let stairs = l.stairs.iter().filter_map(stair_ring).any(|r| c.crosses(&r, tol));
        if held || stairs {
            return Err(format!("{}: a stair, hole or shaft crosses the cut", st.name));
        }
        for o in &l.openings {
            let w = &l.walls[o.wall];
            if o.door && c.on(w.a, tol) && c.on(w.b, tol) {
                return Err(format!("{}: a door in a wall on the cut would join two half-levels", st.name));
            }
            if let Some(s) = c.spans(w.a, w.b, tol) {
                let x = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1) * s;
                if x > o.along - tol && x < o.along + o.width + tol {
                    return Err(format!("{}: a door or window stands on the cut", st.name));
                }
            }
        }
    }
    Ok(())
}

/// Calls `f` with every set of at most `most` of `0..n`, in increasing order.
fn each_set(n: usize, most: usize, f: &mut dyn FnMut(&[usize])) {
    fn go(start: usize, n: usize, most: usize, cur: &mut Vec<usize>, f: &mut dyn FnMut(&[usize])) {
        for i in start..n {
            cur.push(i);
            f(cur.as_slice());
            if cur.len() < most {
                go(i + 1, n, most, cur, f);
            }
            cur.pop();
        }
    }
    go(0, n, most, &mut vec![], f);
}

/// The ways the building may part, best first: sets of at most `site.split_cuts` cuts, each the
/// chord of a ground-floor wall line across the slope within `site.split_skew` that every
/// storey parts on, whose faces fall on two half-levels with at least `site.split_share` of
/// the floor on each; the most even share first, then the fewest cuts.
pub(in super::super) fn candidates(spec: &ProjectSpec, p: &GProject, downhill: P) -> Result<Vec<GSplit>, String> {
    let sv = spec.services;
    if p.podium.is_some() {
        return Err("a podium keeps one level: its storeys step back over it".into());
    }
    if !p.hvac.ducts.is_empty() || sv.electrical || sv.water || sv.appliances {
        return Err("ducts, pipes and cables run along whole storeys: split levels need the services off".into());
    }
    let Some(g) = p.storeys.first() else {
        return Err("no storey".into());
    };
    let l = &g.layout;
    let tol = rule("site.tolerance");
    let skew = rule("site.split_skew");
    let drop = g.height * 0.5;
    // a cut spans a wing from facade to facade or court; stair holes on it refuse it below
    let court: Vec<Vec<P>> = p.court.iter().cloned().collect();
    let mut cands: Vec<GCut> = vec![];
    for w in &l.walls {
        let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
        if len <= tol {
            continue;
        }
        let u = ((w.b.0 - w.a.0) / len, (w.b.1 - w.a.1) / len);
        if (u.0 * downhill.0 + u.1 * downhill.1).abs() > skew {
            continue;
        }
        let n = if u.1 * downhill.0 - u.0 * downhill.1 >= 0.0 { (u.1, -u.0) } else { (-u.1, u.0) };
        let line = GCut { a: w.a, b: w.b, low: n, at: w.a.0 * n.0 + w.a.1 * n.1 };
        for (s, e) in line.stretches(&l.slab, &court) {
            let c = GCut { a: line.at_t(s), b: line.at_t(e), ..line.clone() };
            let m = ((c.a.0 + c.b.0) * 0.5, (c.a.1 + c.b.1) * 0.5);
            let inner = e - s > tol && inside(m, &l.slab) && dist_ring(m, &l.slab) > tol;
            let known = cands.iter().any(|x| {
                x.low.0 * n.0 + x.low.1 * n.1 > 1.0 - tol && (x.at - c.at).abs() <= tol && (x.t(x.a) - c.t(c.a)).abs() <= tol
            });
            if inner && !known {
                cands.push(c);
            }
        }
    }
    let mut why = String::from("site.split_skew: no wall line of the ground floor runs across the slope");
    let mut ok: Vec<GCut> = vec![];
    for c in cands {
        match check_cut(spec, p, &c, drop, tol) {
            Ok(()) => ok.push(c),
            Err(e) => why = e,
        }
    }
    let most = rule("site.split_cuts") as usize;
    let least = rule("site.split_share");
    let total = area(&l.slab) - l.slab_holes.iter().map(|h| area(h)).sum::<f64>();
    let mut sets: Vec<(f64, usize, GSplit)> = vec![];
    each_set(ok.len(), most, &mut |set: &[usize]| {
        let mut sp = GSplit::new(set.iter().map(|&i| ok[i].clone()).collect(), tol, drop);
        if let Some((_, lo)) = sp.pieces(&l.slab, &l.slab_holes) {
            let share = if total > 0.0 { lo.iter().map(face_area).sum::<f64>() / total } else { 0.0 };
            if share >= least && share <= 1.0 - least {
                sp.share = share;
                sp.lows = lo;
                sets.push(((share - 0.5).abs(), set.len(), sp));
            }
        }
    });
    if sets.is_empty() {
        if !ok.is_empty() {
            why = format!("site.split_share: no set of cuts leaves {least} of the floor on each half-level");
        }
        return Err(why);
    }
    sets.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    Ok(sets.into_iter().map(|s| s.2).collect())
}

fn block(p: &GProject, k: u32) -> Option<&PStorey> {
    p.blocks().find(|b| b.0 == k).map(|b| b.1)
}

fn block_mut(p: &mut GProject, k: u32) -> Option<&mut PStorey> {
    match slots::floor_of(k, p.basements.len())? {
        slots::Floor::Storey(i) => p.storeys.get_mut(i),
        slots::Floor::Basement(b) => p.basements.get_mut(b),
    }
}

/// Parts every wall that crosses a cut where it crosses, so each piece stands on one
/// half-level; the openings and hosted fixtures past the cut move to the new piece.
fn part_walls(l: &mut GLayout, c: &GCut, tol: f64) {
    let n = l.walls.len();
    for i in 0..n {
        let w = l.walls[i].clone();
        let Some(t) = c.spans(w.a, w.b, tol) else { continue };
        let x = (w.a.0 + (w.b.0 - w.a.0) * t, w.a.1 + (w.b.1 - w.a.1) * t);
        let cut = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1) * t;
        let j = l.walls.len();
        l.walls[i].b = x;
        let far = c.f(w.b) > 0.0;
        l.walls.push(GWall { a: x, ..w });
        for o in l.openings.iter_mut().filter(|o| o.wall == i && o.along >= cut) {
            o.wall = j;
            o.along -= cut;
        }
        for f in l.fixtures.iter_mut().filter(|f| f.wall == Some(i)) {
            if (c.f(f.at) > 0.0) == far {
                f.wall = Some(j);
            }
        }
    }
}

/// The flights of every block; with a platform lift beside each in the blocks of `aside`.
type Flights = (Vec<(u32, GStair)>, Vec<(u32, usize, GFixture)>);

fn layout_flights(spec: &ProjectSpec, p: &GProject, sp: &GSplit, aside: &[u32]) -> Result<Flights, String> {
    let room = (rule("site.platform_width"), rule("site.platform_depth"));
    let (mut flights, mut platforms) = (vec![], vec![]);
    for (k, st) in p.blocks() {
        let l = &st.layout;
        let want = aside.contains(&k);
        for c in &sp.cuts {
            for (i, (gr, pr)) in l.rooms.iter().zip(&st.rooms).enumerate() {
                if !joins(pr.kind) || !c.crosses(&gr.polygon, sp.tol) {
                    continue;
                }
                let (f, pl) = flight(spec, l, &gr.polygon, c, sp.drop, sp.tol, want.then_some(room)).ok_or_else(|| {
                    if want {
                        format!(
                            "site.platform_width: {} {}: no room for a {} m platform lift beside a flight of site.split_flight_min",
                            st.name, pr.number, room.0
                        )
                    } else {
                        format!("{}: no room for a half-level flight in {}", st.name, pr.number)
                    }
                })?;
                flights.push((k, f));
                if let Some(x) = pl {
                    platforms.push((k, i, x));
                }
            }
        }
    }
    Ok((flights, platforms))
}

/// A lift shaft of the core's size centred in a rectangular room, when it fits.
fn shaft_in(r: &[P]) -> Option<Vec<P>> {
    let (sw, sd) = build::shaft_of(super::super::lift::class_for(crate::project::rules::rule("core.lift_capacity") as u32));
    let margin = build::lift_inset() * 2.0 + WallRole::Party.thickness();
    let e0 = (r[1].0 - r[0].0, r[1].1 - r[0].1);
    let e1 = (r[2].0 - r[1].0, r[2].1 - r[1].1);
    let (l0, l1) = (e0.0.hypot(e0.1), e1.0.hypot(e1.1));
    if l0 <= 0.0 || l1 <= 0.0 {
        return None;
    }
    let (d0, d1) = ((e0.0 / l0, e0.1 / l0), (e1.0 / l1, e1.1 / l1));
    let (a, b) = if l0 >= sw + margin && l1 >= sd + margin {
        (sw, sd)
    } else if l0 >= sd + margin && l1 >= sw + margin {
        (sd, sw)
    } else {
        return None;
    };
    let c = footprint::centroid(r);
    let at = |s: f64, t: f64| (c.0 + (d0.0 * a * s + d1.0 * b * t) * 0.5, c.1 + (d0.1 * a * s + d1.1 * b * t) * 0.5);
    Some(vec![at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0)])
}

/// A second lift for the half-levels the first misses: a technical or storage room there with
/// a door on a corridor or lobby, the same on every block that needs it, large enough for the
/// core's shaft. Gives every block's room and the shaft.
fn second_lift(p: &GProject, sp: &GSplit, need: &[u32]) -> Option<(Vec<(u32, usize)>, Vec<P>)> {
    let tol = sp.tol;
    let high = p
        .blocks()
        .any(|(_, st)| st.layout.rooms.iter().zip(&st.rooms).any(|(g, r)| r.kind == RoomKind::Lift && !sp.is_low(g.point)));
    let st0 = block(p, *need.first()?)?;
    let l = &st0.layout;
    let ds = door_points(l);
    for (i, (gr, pr)) in l.rooms.iter().zip(&st0.rooms).enumerate() {
        let r = &gr.polygon;
        if !matches!(pr.kind, RoomKind::Technical | RoomKind::Storage) || r.len() != 4 || sp.crosses(r) || sp.is_low(gr.point) != high {
            continue;
        }
        let Some(shaft) = shaft_in(r) else { continue };
        let opens = ds.iter().any(|(q, reach, _)| {
            dist_ring(*q, r) <= reach + tol
                && l.rooms.iter().zip(&st0.rooms).enumerate().any(|(j, (g, x))| j != i && joins(x.kind) && dist_ring(*q, &g.polygon) <= reach + tol)
        });
        if !opens {
            continue;
        }
        let (c, a) = (footprint::centroid(r), area(r));
        let mut rooms = vec![];
        for (k, st) in p.blocks() {
            let same = st.layout.rooms.iter().zip(&st.rooms).position(|(g, x)| {
                let m = footprint::centroid(&g.polygon);
                x.kind == pr.kind && (m.0 - c.0).hypot(m.1 - c.1) <= tol && (area(&g.polygon) - a).abs() <= tol
            });
            if let Some(j) = same {
                rooms.push((k, j));
            }
        }
        if need.iter().all(|k| rooms.iter().any(|x| x.0 == *k)) {
            return Some((rooms, shaft));
        }
    }
    None
}

/// Turns the rooms into the second lift: kind and name, the shaft, its furniture cleared.
fn convert(spec: &ProjectSpec, p: &mut GProject, rooms: &[(u32, usize)], shaft: &[P], tol: f64) {
    for &(k, i) in rooms {
        let Some(st) = block_mut(p, k) else { continue };
        let poly = st.layout.rooms[i].polygon.clone();
        st.rooms[i].kind = RoomKind::Lift;
        st.layout.rooms[i].name = names::kind_name(RoomKind::Lift, spec.ru).to_string();
        st.layout.shafts.push(GShaft::new(shaft.to_vec(), crate::ShaftKind::Lift));
        st.layout.fixtures.retain(|f| !(inside(f.at, &poly) || dist_ring(f.at, &poly) <= tol));
        st.fit.retain(|f| f.room != i);
    }
}

/// Parts the project on the split's cuts: doors of through cars, walls parted on the cuts, the
/// flights, and a lift for both half-levels of every storey a lift serves (and the ground
/// floor): a through car, else by `site.split_lift` platform lifts beside the flights or a
/// second lift. Refuses when a slab or roof would stand on both half-levels, a storey has no
/// slots left, or no lift reaches both half-levels.
pub(in super::super) fn provide(spec: &ProjectSpec, p: &mut GProject, sp: &mut GSplit) -> Result<(), String> {
    let tol = sp.tol;
    for r in std::iter::once(&p.roof).chain(&p.extra_roofs) {
        sp.pieces(&r.ring, &[]).ok_or("a roof would stand on both half-levels")?;
    }
    let blocks: Vec<u32> = p.blocks().map(|b| b.0).collect();
    for &k in &blocks {
        let Some(st) = block_mut(p, k) else { continue };
        let l = &mut st.layout;
        let mut doors = vec![];
        for (i, pr) in st.rooms.iter().enumerate() {
            if pr.kind != RoomKind::Lift {
                continue;
            }
            for c in sp.cuts.iter().filter(|c| c.crosses(&l.rooms[i].polygon, tol)) {
                if let Some(d) = through(l, &st.rooms, i, c, tol) {
                    doors.extend(d);
                    let at = footprint::centroid(&l.rooms[i].polygon);
                    sp.access.push(GAccess { block: k, kind: Access::Through, room: i, at });
                }
            }
        }
        l.openings.extend(doors);
        for c in &sp.cuts {
            part_walls(l, c, tol);
        }
    }
    let mut need = vec![];
    for (k, st) in p.blocks() {
        let l = &st.layout;
        let lows = l.rooms.iter().any(|r| sp.is_low(r.point));
        let highs = l.rooms.iter().any(|r| sp.is_high(r.point));
        let lifts: Vec<bool> = l.rooms.iter().zip(&st.rooms).filter(|x| x.1.kind == RoomKind::Lift).map(|x| sp.is_low(x.0.point)).collect();
        let served = k == 0 || !lifts.is_empty();
        let car = sp.access.iter().any(|a| a.block == k && a.kind == Access::Through);
        let both = lifts.contains(&true) && lifts.contains(&false);
        if lows && highs && served && !car && !both {
            need.push(k);
        }
    }
    let order = match spec.site.split_lift {
        SplitLift::Platform => [Access::Platform, Access::Lift],
        SplitLift::Lift => [Access::Lift, Access::Platform],
    };
    let mut tried = vec![];
    let mut plan = if need.is_empty() { Some(layout_flights(spec, p, sp, &[])?) } else { None };
    for kind in order {
        if plan.is_some() {
            break;
        }
        if kind == Access::Platform {
            let rise = rule("site.platform_rise");
            if sp.drop > rise + tol {
                tried.push(format!("site.platform_rise: the half-level drop {:.2} m passes a platform lift's {rise} m", sp.drop));
                continue;
            }
            match layout_flights(spec, p, sp, &need) {
                Ok(x) => plan = Some(x),
                Err(e) => tried.push(e),
            }
        } else {
            match second_lift(p, sp, &need) {
                Some((rooms, shaft)) => {
                    convert(spec, p, &rooms, &shaft, tol);
                    let at = footprint::centroid(&shaft);
                    sp.access.extend(rooms.iter().map(|&(k, i)| GAccess { block: k, kind: Access::Lift, room: i, at }));
                    plan = Some(layout_flights(spec, p, sp, &[])?);
                }
                None => tried.push("site.split_lift: no technical or storage room on the other half-levels holds a second lift on every storey".into()),
            }
        }
    }
    let (flights, platforms) = plan.ok_or_else(|| format!("no lift reaches both half-levels: {}", tried.join("; ")))?;
    for (k, i, f) in platforms {
        sp.access.push(GAccess { block: k, kind: Access::Platform, room: i, at: f.at });
        if let Some(st) = block_mut(p, k) {
            st.layout.fixtures.push(f);
        }
    }
    sp.flights = flights;
    for (k, st) in p.blocks() {
        let l = &st.layout;
        let (hi, lo) = sp.pieces(&l.slab, &l.slab_holes).ok_or_else(|| format!("{}: the slab would stand on both half-levels", st.name))?;
        let n = (hi.len() + lo.len()).saturating_sub(1) + sp.flights.iter().filter(|f| f.0 == k).count();
        if n > slots::SPLIT_PER_STOREY as usize {
            return Err(format!("{}: {n} slab pieces and flights, at most {}", st.name, slots::SPLIT_PER_STOREY));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Graph {
    ids: BTreeMap<(u32, usize, bool), usize>,
    up: Vec<usize>,
}

impl Graph {
    fn id(&mut self, key: (u32, usize, bool)) -> usize {
        if let Some(&i) = self.ids.get(&key) {
            return i;
        }
        let i = self.up.len();
        self.up.push(i);
        self.ids.insert(key, i);
        i
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.up[i] != i {
            self.up[i] = self.up[self.up[i]];
            i = self.up[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (x, y) = (self.root(a), self.root(b));
        self.up[x] = y;
    }
}

/// A point on a stretch two rooms share with no wall on it, when they have one.
fn open_side(a: &[P], b: &[P], walls: &[GWall], tol: f64) -> Option<P> {
    let least = WallRole::Partition.thickness();
    let (n, m) = (a.len(), b.len());
    for i in 0..n {
        let (p, q) = (a[i], a[(i + 1) % n]);
        let len = (q.0 - p.0).hypot(q.1 - p.1);
        if len <= tol {
            continue;
        }
        let u = ((q.0 - p.0) / len, (q.1 - p.1) / len);
        let off = |r: P| ((r.0 - p.0) * -u.1 + (r.1 - p.1) * u.0).abs();
        let t = |r: P| (r.0 - p.0) * u.0 + (r.1 - p.1) * u.1;
        for j in 0..m {
            let (r, s) = (b[j], b[(j + 1) % m]);
            if off(r) > tol || off(s) > tol {
                continue;
            }
            let (lo, hi) = (t(r).min(t(s)).max(0.0), t(r).max(t(s)).min(len));
            if hi - lo <= least {
                continue;
            }
            let mid = (lo + hi) * 0.5;
            let x = (p.0 + u.0 * mid, p.1 + u.1 * mid);
            if !walls.iter().any(|w| dist_seg(x, w.a, w.b) <= tol) {
                return Some(x);
            }
        }
    }
    None
}

/// Rooms with no barrier-free way to an entrance, as "storey room (part)": the way runs
/// through doors and open sides, by lift between storeys and by platform lift or through car
/// between half-levels, never up a flight. It counts the ground floor and the storeys a lift
/// stops on; an entrance is a ground-floor exterior door at grade or by ramp. Each part of a
/// corridor or lobby on a cut counts on its own.
pub fn barrier_free(p: &GProject) -> Vec<String> {
    let sp = p.site.split.as_ref();
    let tol = rule("site.tolerance");
    let level = |q: P| {
        !p.site.terrain
            || p.site.approaches.iter().any(|a| {
                (a.door.0 - q.0).hypot(a.door.1 - q.1) <= tol && matches!(a.kind, Some(Approach::Grade) | Some(Approach::Ramp))
            })
    };
    let mut gr = Graph::default();
    let mut entrances = vec![];
    let mut lifts: Vec<(P, usize)> = vec![];
    let mut counted: Vec<(u32, &PStorey, Vec<bool>)> = vec![];
    for (k, st) in p.blocks() {
        let l = &st.layout;
        let rooms = &l.rooms;
        let cross: Vec<bool> = rooms.iter().map(|r| sp.is_some_and(|s| s.crosses(&r.polygon))).collect();
        let key = |i: usize, q: P| (k, i, cross[i] && sp.is_some_and(|s| s.is_low(q)));
        for (i, r) in rooms.iter().enumerate() {
            let a = gr.id((k, i, false));
            if cross[i] {
                let b = gr.id((k, i, true));
                let joined = st.rooms[i].kind == RoomKind::Lift
                    || sp.is_some_and(|s| s.access.iter().any(|x| x.block == k && x.room == i && x.kind == Access::Platform));
                if joined {
                    gr.join(a, b);
                }
            }
            if st.rooms[i].kind == RoomKind::Lift {
                lifts.push((footprint::centroid(&r.polygon), a));
            }
        }
        for (q, reach, o) in door_points(l) {
            let by: Vec<usize> = (0..rooms.len()).filter(|&i| dist_ring(q, &rooms[i].polygon) <= reach + tol).collect();
            let w = &l.walls[l.openings[o].wall];
            match by.as_slice() {
                [i] if k == 0 && w.role == WallRole::Exterior && level(q) => entrances.push(gr.id(key(*i, q))),
                [i, rest @ ..] => {
                    let a = gr.id(key(*i, q));
                    for j in rest {
                        let b = gr.id(key(*j, q));
                        gr.join(a, b);
                    }
                }
                _ => {}
            }
        }
        for i in 0..rooms.len() {
            for j in i + 1..rooms.len() {
                if let Some(x) = open_side(&rooms[i].polygon, &rooms[j].polygon, &l.walls, tol) {
                    let (a, b) = (gr.id(key(i, x)), gr.id(key(j, x)));
                    gr.join(a, b);
                }
            }
        }
        if k == 0 || st.rooms.iter().any(|r| r.kind == RoomKind::Lift) {
            counted.push((k, st, cross));
        }
    }
    for i in 0..lifts.len() {
        for j in i + 1..lifts.len() {
            let (a, b) = (lifts[i].0, lifts[j].0);
            if (a.0 - b.0).hypot(a.1 - b.1) <= tol {
                gr.join(lifts[i].1, lifts[j].1);
            }
        }
    }
    let roots: BTreeSet<usize> = entrances.iter().map(|&e| gr.root(e)).collect();
    let mut out = vec![];
    for (k, st, cross) in counted {
        for (i, r) in st.rooms.iter().enumerate() {
            let parts: &[(bool, &str)] = if cross[i] { &[(false, " (upper part)"), (true, " (lower part)")] } else { &[(false, "")] };
            for &(low, what) in parts {
                let n = gr.id((k, i, low));
                if !roots.contains(&gr.root(n)) {
                    out.push(format!("{} room {}{what}", st.name, r.number));
                }
            }
        }
    }
    out
}

/// Split levels on the built project: every room but a corridor, lobby or through car on one
/// half-level, so wet rooms and their risers too; on every storey with rooms on both, a flight
/// of exactly the drop within the rules in a corridor or lobby; platform lifts within their
/// rise; and every room with a barrier-free way to an entrance.
pub(in super::super) fn audit(spec: &ProjectSpec, p: &GProject, sp: &GSplit, fail: &mut Vec<String>) {
    for (k, st) in p.blocks() {
        let l = &st.layout;
        for (i, (gr, pr)) in l.rooms.iter().zip(&st.rooms).enumerate() {
            let car = pr.kind == RoomKind::Lift && sp.access.iter().any(|a| a.block == k && a.room == i && a.kind == Access::Through);
            if sp.crosses(&gr.polygon) && !joins(pr.kind) && !car {
                fail.push(format!("site.split_slope: {} room {} stands on both half-levels", st.name, pr.number));
            }
        }
        let both = l.rooms.iter().any(|r| sp.is_low(r.point)) && l.rooms.iter().any(|r| sp.is_high(r.point));
        let mine: Vec<&GStair> = sp.flights.iter().filter(|f| f.0 == k).map(|f| &f.1).collect();
        if both && mine.is_empty() {
            fail.push(format!("site.split_slope: {}: no flight joins the half-levels", st.name));
        }
        for s in mine {
            let lay = match s.def().layout() {
                Ok(x) => x,
                Err(e) => {
                    fail.push(format!("site.split_slope: {}: half-level flight {e:?}", st.name));
                    continue;
                }
            };
            let rise = lay.risers as f64 * lay.riser;
            if (rise - sp.drop).abs() > sp.tol || lay.riser > spec.max_riser + sp.tol {
                fail.push(format!(
                    "site.split_slope: {}: the half-level flight rises {rise:.3} m by {:.3} m risers, the drop is {:.3} m",
                    st.name, lay.riser, sp.drop
                ));
            }
            let ring = stair_ring(s).unwrap_or_default();
            let held = l.rooms.iter().zip(&st.rooms).any(|(gr, pr)| {
                joins(pr.kind) && ring.iter().all(|q| inside(*q, &gr.polygon) || dist_ring(*q, &gr.polygon) <= sp.tol)
            });
            if !held {
                fail.push(format!("site.split_slope: {}: a half-level flight leaves its corridor", st.name));
            }
        }
    }
    let rise = rule("site.platform_rise");
    if sp.access.iter().any(|a| a.kind == Access::Platform) && sp.drop > rise + sp.tol {
        fail.push(format!("site.platform_rise: platform lifts rise {:.2} m, at most {rise}", sp.drop));
    }
    let lost = barrier_free(p);
    if !lost.is_empty() {
        fail.push(format!("site.split_lift: no barrier-free way to an entrance from {}", lost.join(", ")));
    }
}
