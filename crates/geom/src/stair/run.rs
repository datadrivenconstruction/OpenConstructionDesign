// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stairs along a free run line (GEOM_PRO_PLAN W6). The centre line is a chain of straight and
//! circular pieces (one bulge per piece), joined tangentially or, between two straight pieces,
//! at a corner. Nosings are spaced equally along the walk line (offset `walk` from the centre),
//! so the going on the walk line is the same for every tread and 2R + T is checked there. On
//! arcs the nosings are normal to the run (radial). A corner is wound with balanced steps: each
//! nosing passes through its walk-line point, the going along the inner edge varies linearly
//! over the turn zone (smallest at the corner) and the zone widens until that going reaches
//! `min_inner`. Every step is a prism of its tread (exact area with arc pieces), `riser +
//! thickness` deep; the first stands on the floor. Spirals: headroom between treads one turn
//! apart is measured from the plan overlap of the treads.

use std::f64::consts::PI;

use super::COMFORT;
use crate::arc::{BulgeRing, MAX_BULGE, MIN_BULGE};
use crate::brep::Solid;
use crate::math::{v2, Tol, V2};
use crate::mesh::Mesh;
use crate::poly;
use crate::profile::Profile;

/// Two pieces whose directions differ by less than this (rad) join tangentially.
const SMOOTH: f64 = 1e-6;
/// Largest turn at a corner, rad.
const MAX_TURN: f64 = 2.0 * PI / 3.0;
/// Length tolerance on stations, m.
const EPS: f64 = 1e-9;
/// Chord sag of tessellated treads used for the plan overlap (headroom), m.
const OVERLAP_SAG: f64 = 1e-3;
/// Plan overlap below this (m²) is touching, not overlap.
const OVERLAP_AREA: f64 = 1e-6;

#[derive(Clone, Debug, PartialEq)]
pub struct RunDef {
    /// Centre line of the run from the bottom nosing to the top one.
    pub pts: Vec<V2>,
    /// Bulge of each piece `pts[i] -> pts[i + 1]` (0 is straight).
    pub bulges: Vec<f64>,
    pub width: f64,
    /// Walk line offset from the centre line, positive to the left of the run direction.
    pub walk: f64,
    pub base_z: f64,
    pub height: f64,
    pub max_riser: f64,
    /// Depth of each step block below its tread.
    pub thickness: f64,
    /// Narrowest going allowed along the inner edge of a winder.
    pub min_inner: f64,
    /// Clear height required above every tread (0 skips the check).
    pub headroom: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RunError {
    Input(&'static str),
    /// 2R + T on the walk line outside [`COMFORT`].
    Comfort { riser: f64, going: f64, value: f64 },
    /// The turn at run point `corner` cannot be wound with an inner going of `min_inner`;
    /// `best` is the largest inner going a valid winding reaches (0 when none is valid).
    Winder { corner: usize, best: f64 },
    /// Clear height between tread `below` and the tread `above` over it.
    Headroom { below: usize, above: usize, clearance: f64 },
}

/// A nosing line from the left edge to the right edge through its walk-line point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nosing {
    pub left: V2,
    pub walk: V2,
    pub right: V2,
    /// Stations along the left and right edges.
    sl: f64,
    sr: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunStair {
    pub risers: usize,
    pub riser: f64,
    /// Going on the walk line, equal for every tread.
    pub going: f64,
    pub walk_length: f64,
    /// `treads + 1` nosings, bottom to top.
    pub nosings: Vec<Nosing>,
    /// Tread outlines, counter-clockwise, with exact arc pieces.
    pub treads: Vec<BulgeRing>,
    pub thickness: f64,
    /// Bottom and top levels.
    pub z: (f64, f64),
    /// Walk line sampled for the plan arrow.
    pub walk: Vec<V2>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunQty {
    pub risers: usize,
    pub treads: usize,
    pub riser: f64,
    pub going: f64,
    /// 2R + T on the walk line.
    pub comfort: f64,
    pub walk_length: f64,
    pub tread_areas: Vec<f64>,
    pub walk_area: f64,
    pub volume: f64,
    /// Narrowest tread going along either edge.
    pub narrow: f64,
    /// Smallest clear height between overlapping treads (spirals), if any overlap.
    pub headroom: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunPlan {
    pub outline: Vec<Vec<V2>>,
    pub nosings: Vec<(V2, V2)>,
    pub walk: Vec<V2>,
}

fn dir(a: f64) -> V2 {
    v2(a.cos(), a.sin())
}

fn lft(v: V2) -> V2 {
    v2(-v.y, v.x)
}

/// Angle `x` brought into (r - pi, r + pi].
fn near_angle(x: f64, r: f64) -> f64 {
    let mut d = (x - r) % (2.0 * PI);
    if d > PI {
        d -= 2.0 * PI;
    } else if d <= -PI {
        d += 2.0 * PI;
    }
    r + d
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Line { a: V2, d: V2, len: f64 },
    /// Centre, radius, start angle, signed sweep.
    Arc { c: V2, r: f64, a0: f64, sw: f64 },
}

impl Seg {
    fn new(p: V2, q: V2, b: f64) -> Result<Seg, RunError> {
        let ch = q - p;
        let c = ch.len();
        if c <= EPS {
            return Err(RunError::Input("a piece of the run has zero length"));
        }
        if b.abs() < MIN_BULGE {
            return Ok(Seg::Line {
                a: p,
                d: ch * (1.0 / c),
                len: c,
            });
        }
        if b.abs() > MAX_BULGE {
            return Err(RunError::Input("an arc of the run is too large"));
        }
        let r = c * (1.0 + b * b) / (4.0 * b.abs());
        let off = c * (1.0 - b * b) / (4.0 * b);
        let cen = (p + q) * 0.5 + lft(ch * (1.0 / c)) * off;
        let v = p - cen;
        Ok(Seg::Arc {
            c: cen,
            r,
            a0: v.y.atan2(v.x),
            sw: 4.0 * b.atan(),
        })
    }

    fn len(&self) -> f64 {
        match *self {
            Seg::Line { len, .. } => len,
            Seg::Arc { r, sw, .. } => r * sw.abs(),
        }
    }

    fn point(&self, s: f64) -> V2 {
        match *self {
            Seg::Line { a, d, .. } => a + d * s,
            Seg::Arc { c, r, a0, sw } => c + dir(a0 + sw.signum() * s / r) * r,
        }
    }

    fn tangent(&self, s: f64) -> V2 {
        match *self {
            Seg::Line { d, .. } => d,
            Seg::Arc { r, a0, sw, .. } => lft(dir(a0 + sw.signum() * s / r)) * sw.signum(),
        }
    }

    /// The piece offset by `o` to the left (not trimmed).
    fn offset(&self, o: f64) -> Result<Seg, RunError> {
        Ok(match *self {
            Seg::Line { a, d, len } => Seg::Line {
                a: a + lft(d) * o,
                d,
                len,
            },
            Seg::Arc { c, r, a0, sw } => {
                let r2 = r - sw.signum() * o;
                if r2 <= EPS {
                    return Err(RunError::Input(
                        "an arc of the run is tighter than the half width of the stair",
                    ));
                }
                Seg::Arc { c, r: r2, a0, sw }
            }
        })
    }

    /// Parameter of the projection of `p` on the piece (not clamped).
    fn local(&self, p: V2) -> f64 {
        match *self {
            Seg::Line { a, d, .. } => (p - a).dot(d),
            Seg::Arc { c, r, a0, sw } => {
                let v = p - c;
                let ang = near_angle(v.y.atan2(v.x), a0 + sw / 2.0);
                (ang - a0) * sw.signum() * r
            }
        }
    }

    /// Hits `(lambda, parameter)` of the ray `o + u * lambda` (`u` unit) inside the piece.
    fn hits(&self, o: V2, u: V2, out: &mut Vec<(f64, f64)>) {
        let len = self.len();
        match *self {
            Seg::Line { a, d, .. } => {
                let den = u.cross(d);
                if den.abs() < 1e-14 {
                    return;
                }
                let w = a - o;
                let lam = w.cross(d) / den;
                let mu = w.cross(u) / den;
                if (-EPS..=len + EPS).contains(&mu) {
                    out.push((lam, mu.clamp(0.0, len)));
                }
            }
            Seg::Arc { c, r, .. } => {
                let w = o - c;
                let b = w.dot(u);
                let disc = b * b - (w.dot(w) - r * r);
                if disc < 0.0 {
                    return;
                }
                for lam in [-b - disc.sqrt(), -b + disc.sqrt()] {
                    let mu = self.local(o + u * lam);
                    if (-EPS..=len + EPS).contains(&mu) {
                        out.push((lam, mu.clamp(0.0, len)));
                    }
                }
            }
        }
    }
}

/// The run offset by `o`: pieces trimmed at the corners, stations by cumulative length.
#[derive(Clone, Debug)]
struct Chain {
    o: f64,
    segs: Vec<Seg>,
    cum: Vec<f64>,
}

impl Chain {
    fn new(base: &[Seg], o: f64) -> Result<Chain, RunError> {
        let mut segs = base
            .iter()
            .map(|s| s.offset(o))
            .collect::<Result<Vec<_>, _>>()?;
        for j in 0..segs.len() - 1 {
            let (
                Seg::Line {
                    a: a1,
                    d: d1,
                    len: _,
                },
                Seg::Line {
                    a: a2,
                    d: d2,
                    len: l2,
                },
            ) = (segs[j], segs[j + 1])
            else {
                continue;
            };
            let den = d1.cross(d2);
            if den.abs() < 1e-12 {
                continue;
            }
            let lam = (a2 - a1).cross(d2) / den;
            let x = a1 + d1 * lam;
            let rest = l2 - (x - a2).dot(d2);
            if lam <= EPS || rest <= EPS {
                return Err(RunError::Input(
                    "a straight piece of the run is shorter than its corner cut",
                ));
            }
            segs[j] = Seg::Line {
                a: a1,
                d: d1,
                len: lam,
            };
            segs[j + 1] = Seg::Line {
                a: x,
                d: d2,
                len: rest,
            };
        }
        let mut cum = vec![0.0];
        for s in &segs {
            cum.push(cum[cum.len() - 1] + s.len());
        }
        Ok(Chain { o, segs, cum })
    }

    fn len(&self) -> f64 {
        self.cum[self.cum.len() - 1]
    }

    /// Piece index and parameter of station `s` (clamped to the chain).
    fn locate(&self, s: f64) -> (usize, f64) {
        let s = s.clamp(0.0, self.len());
        let k = (0..self.segs.len())
            .find(|&k| s < self.cum[k + 1])
            .unwrap_or(self.segs.len() - 1);
        (k, (s - self.cum[k]).min(self.segs[k].len()))
    }

    fn point(&self, s: f64) -> V2 {
        let (k, u) = self.locate(s);
        self.segs[k].point(u)
    }

    /// Nearest hit of the ray `o + u * lambda` with `lambda > min`: (station, point).
    fn hit(&self, o: V2, u: V2, min: f64) -> Option<(f64, V2)> {
        let mut best: Option<(f64, f64)> = None;
        let mut hs = vec![];
        for (k, s) in self.segs.iter().enumerate() {
            hs.clear();
            s.hits(o, u, &mut hs);
            for &(lam, mu) in &hs {
                if lam > min + EPS && best.map_or(true, |b| lam < b.0) {
                    best = Some((lam, self.cum[k] + mu));
                }
            }
        }
        best.map(|(lam, st)| (st, o + u * lam))
    }

    /// Pieces between stations `s0 < s1`: (start point, bulge), end point not included.
    fn pieces(&self, s0: f64, s1: f64) -> Vec<(V2, f64)> {
        let mut out = vec![];
        for (k, s) in self.segs.iter().enumerate() {
            let lo = s0.max(self.cum[k]) - self.cum[k];
            let hi = s1.min(self.cum[k + 1]) - self.cum[k];
            if hi - lo <= EPS {
                continue;
            }
            let b = match *s {
                Seg::Line { .. } => 0.0,
                Seg::Arc { r, sw, .. } => (sw.signum() * (hi - lo) / r / 4.0).tan(),
            };
            out.push((s.point(lo), b));
        }
        out
    }

    /// Points between stations `s0 < s1`, chord sag at most `sag` on arcs.
    fn sample(&self, s0: f64, s1: f64, sag: f64) -> Vec<V2> {
        let mut out = vec![];
        for (k, s) in self.segs.iter().enumerate() {
            let lo = s0.max(self.cum[k]) - self.cum[k];
            let hi = s1.min(self.cum[k + 1]) - self.cum[k];
            if hi - lo <= EPS {
                continue;
            }
            let n = match *s {
                Seg::Line { .. } => 1,
                Seg::Arc { r, .. } => {
                    let step = 2.0 * (1.0 - (sag / r).min(1.0)).acos();
                    (((hi - lo) / r / step.max(1e-6)).ceil() as usize).max(1)
                }
            };
            for i in 0..n {
                out.push(s.point(lo + (hi - lo) * i as f64 / n as f64));
            }
        }
        out.push(self.point(s1));
        out
    }
}

/// A wound turn: nosings `ia..=ib`, inner and outer stations of the interior ones.
struct Zone {
    ia: usize,
    ib: usize,
    gmin: f64,
    inner: Vec<f64>,
    outer: Vec<f64>,
}

impl RunDef {
    pub fn layout(&self) -> Result<RunStair, RunError> {
        use RunError::Input;
        let n = self.pts.len();
        if n < 2 || self.bulges.len() != n - 1 {
            return Err(Input("the run needs two points or more and one bulge per piece"));
        }
        let scalars = [
            self.width,
            self.walk,
            self.base_z,
            self.height,
            self.max_riser,
            self.thickness,
            self.min_inner,
            self.headroom,
        ];
        let finite = scalars.iter().all(|v| v.is_finite())
            && self.bulges.iter().all(|v| v.is_finite())
            && self.pts.iter().all(|p| p.x.is_finite() && p.y.is_finite());
        if !finite {
            return Err(Input("non-finite value"));
        }
        if self.width <= 0.0 || self.height <= 0.0 || self.max_riser <= 0.0 || self.thickness <= 0.0
        {
            return Err(Input("width, height, max riser and thickness must be > 0"));
        }
        if self.min_inner < 0.0 || self.headroom < 0.0 {
            return Err(Input("min inner going and headroom must be >= 0"));
        }
        let hw = self.width / 2.0;
        if self.walk.abs() >= hw {
            return Err(Input("the walk line must lie inside the stair"));
        }
        let base = (0..n - 1)
            .map(|i| Seg::new(self.pts[i], self.pts[i + 1], self.bulges[i]))
            .collect::<Result<Vec<_>, _>>()?;
        let mut corners = vec![];
        for j in 0..base.len() - 1 {
            let (t1, t2) = (base[j].tangent(base[j].len()), base[j + 1].tangent(0.0));
            let turn = t1.cross(t2).atan2(t1.dot(t2));
            if turn.abs() <= SMOOTH {
                continue;
            }
            if !matches!((base[j], base[j + 1]), (Seg::Line { .. }, Seg::Line { .. })) {
                return Err(Input("a corner of the run must join two straight pieces"));
            }
            if turn.abs() > MAX_TURN + 1e-12 {
                return Err(Input("a corner of the run turns by more than 120 degrees"));
            }
            corners.push((j, turn));
        }
        let lc = Chain::new(&base, hw)?;
        let rc = Chain::new(&base, -hw)?;
        let wc = Chain::new(&base, self.walk)?;
        let risers = (self.height / self.max_riser - 1e-9).ceil() as usize;
        if risers < 2 {
            return Err(Input("the stair needs two risers or more"));
        }
        let riser = self.height / risers as f64;
        let m = risers - 1;
        let lw = wc.len();
        let going = lw / m as f64;
        let value = 2.0 * riser + going;
        if !(COMFORT.0 - 1e-12..=COMFORT.1 + 1e-12).contains(&value) {
            return Err(RunError::Comfort {
                riser,
                going,
                value,
            });
        }
        let walk_pts: Vec<V2> = (0..=m).map(|i| wc.point(going * i as f64)).collect();
        // station on edge `e` of the nosing normal to the run at walk point `i`, if it stays
        // on the piece that carries the walk point
        let natural = |i: usize, e: &Chain| -> Option<f64> {
            let (k, u) = wc.locate(going * i as f64);
            let p = wc.segs[k].point(u) + lft(wc.segs[k].tangent(u)) * (e.o - wc.o);
            let s = &e.segs[k];
            let mu = s.local(p);
            if !(-EPS..=s.len() + EPS).contains(&mu) || (s.point(mu) - p).len() > 1e-7 {
                return None;
            }
            Some(e.cum[k] + mu.clamp(0.0, s.len()))
        };
        let mut sl: Vec<Option<f64>> = vec![None; m + 1];
        let mut sr: Vec<Option<f64>> = vec![None; m + 1];
        let mut prev_end = 0usize;
        for &(j, turn) in &corners {
            let (inner, outer) = if turn > 0.0 { (&lc, &rc) } else { (&rc, &lc) };
            let sc = wc.cum[j + 1];
            let c0 = ((sc / going).floor() as usize).min(m - 1);
            let wind = |ia: usize, ib: usize| -> Option<Zone> {
                let (a, b) = (natural(ia, inner)?, natural(ib, inner)?);
                let (oa, ob) = (natural(ia, outer)?, natural(ib, outer)?);
                let delta = b - a;
                if delta <= EPS {
                    return None;
                }
                let u: Vec<f64> = (ia..ib)
                    .map(|t| (t as f64 + 0.5 - sc / going).abs())
                    .collect();
                let dmax = u.iter().cloned().fold(0.0, f64::max) + 0.5;
                let w: Vec<f64> = u.iter().map(|d| 1.0 - d / dmax).collect();
                let cut = ((ib - ia) as f64 * going - delta) / w.iter().sum::<f64>();
                let g: Vec<f64> = w.iter().map(|w| going - cut * w).collect();
                let gmin = g.iter().cloned().fold(f64::INFINITY, f64::min);
                if gmin <= EPS {
                    return None;
                }
                let (mut si, mut so) = (vec![], vec![]);
                let (mut s, mut last) = (a, oa);
                for (q, i) in (ia + 1..ib).enumerate() {
                    s += g[q];
                    let pi = inner.point(s);
                    let dv = walk_pts[i] - pi;
                    let dl = dv.len();
                    if dl <= EPS {
                        return None;
                    }
                    let (st, _) = outer.hit(pi, dv * (1.0 / dl), dl)?;
                    if st <= last + EPS {
                        return None;
                    }
                    last = st;
                    si.push(s);
                    so.push(st);
                }
                if ob <= last + EPS {
                    return None;
                }
                Some(Zone {
                    ia,
                    ib,
                    gmin,
                    inner: si,
                    outer: so,
                })
            };
            let mut best: f64 = 0.0;
            let mut found = None;
            for k in 0..=m {
                let ia = (c0 as isize - k as isize).max(prev_end as isize) as usize;
                let ib = (c0 + 1 + k).min(m);
                if let Some(z) = wind(ia, ib) {
                    if z.gmin >= self.min_inner - 1e-12 {
                        found = Some(z);
                        break;
                    }
                    best = best.max(z.gmin);
                }
                if ia == prev_end && ib == m {
                    break;
                }
            }
            let Some(z) = found else {
                return Err(RunError::Winder {
                    corner: j + 1,
                    best,
                });
            };
            let (si, so) = if turn > 0.0 {
                (&mut sl, &mut sr)
            } else {
                (&mut sr, &mut sl)
            };
            for (q, i) in (z.ia + 1..z.ib).enumerate() {
                si[i] = Some(z.inner[q]);
                so[i] = Some(z.outer[q]);
            }
            prev_end = z.ib;
        }
        let leaves = Input("a nosing normal to the run leaves the stair; lengthen the pieces");
        for i in 0..=m {
            if sl[i].is_none() {
                sl[i] = Some(natural(i, &lc).ok_or(leaves.clone())?);
            }
            if sr[i].is_none() {
                sr[i] = Some(natural(i, &rc).ok_or(leaves.clone())?);
            }
        }
        let sl: Vec<f64> = sl.into_iter().flatten().collect();
        let sr: Vec<f64> = sr.into_iter().flatten().collect();
        let rising = |v: &[f64]| v.windows(2).all(|w| w[1] > w[0] + EPS);
        if !rising(&sl) || !rising(&sr) {
            return Err(Input("nosing lines cross: the run is too tight for this width"));
        }
        let nosings: Vec<Nosing> = (0..=m)
            .map(|i| Nosing {
                left: lc.point(sl[i]),
                walk: walk_pts[i],
                right: rc.point(sr[i]),
                sl: sl[i],
                sr: sr[i],
            })
            .collect();
        let treads = (0..m)
            .map(|i| tread_ring(&lc, &rc, (sl[i], sl[i + 1]), (sr[i], sr[i + 1])))
            .collect();
        let stair = RunStair {
            risers,
            riser,
            going,
            walk_length: lw,
            nosings,
            treads,
            thickness: self.thickness,
            z: (self.base_z, self.base_z + self.height),
            walk: wc.sample(0.0, lw, 0.002),
        };
        if self.headroom > 0.0 {
            if let Some((below, above, clearance)) = stair.headroom() {
                if clearance < self.headroom - 1e-9 {
                    return Err(RunError::Headroom {
                        below,
                        above,
                        clearance,
                    });
                }
            }
        }
        Ok(stair)
    }
}

/// Outline of one tread: left edge forward, top nosing, right edge back, bottom nosing.
fn tread_ring(lc: &Chain, rc: &Chain, l: (f64, f64), r: (f64, f64)) -> BulgeRing {
    let mut pts = vec![];
    let mut bulges = vec![];
    for (p, b) in lc.pieces(l.0, l.1) {
        pts.push(p);
        bulges.push(b);
    }
    pts.push(lc.point(l.1));
    bulges.push(0.0);
    let rp = rc.pieces(r.0, r.1);
    let end = rc.point(r.1);
    for i in (0..rp.len()).rev() {
        pts.push(if i + 1 == rp.len() { end } else { rp[i + 1].0 });
        bulges.push(-rp[i].1);
    }
    pts.push(rc.point(r.0));
    bulges.push(0.0);
    BulgeRing { pts, bulges }.ccw()
}

impl RunStair {
    pub fn tread_top(&self, i: usize) -> f64 {
        self.z.0 + self.riser * (i + 1) as f64
    }

    pub fn step_bottom(&self, i: usize) -> f64 {
        (self.tread_top(i) - self.riser - self.thickness).max(self.z.0)
    }

    /// Smallest clear height between treads that overlap in plan: (below, above, clearance).
    pub fn headroom(&self) -> Option<(usize, usize, f64)> {
        let rings: Vec<Vec<V2>> = self.treads.iter().map(|t| t.tessellate(OVERLAP_SAG)).collect();
        let boxes: Vec<(V2, V2)> = rings.iter().map(|r| bbox(r)).collect();
        let mut best: Option<(usize, usize, f64)> = None;
        for k in 2..rings.len() {
            let pieces = poly::convex_pieces(&rings[k]);
            for j in 0..k - 1 {
                let (a, b) = (boxes[j], boxes[k]);
                if a.1.x < b.0.x || b.1.x < a.0.x || a.1.y < b.0.y || b.1.y < a.0.y {
                    continue;
                }
                if poly::overlap_area(&rings[j], &pieces).abs() <= OVERLAP_AREA {
                    continue;
                }
                let c = self.step_bottom(k) - self.tread_top(j);
                if best.map_or(true, |b| c < b.2) {
                    best = Some((j, k, c));
                }
            }
        }
        best
    }

    pub fn quantities(&self) -> RunQty {
        let areas: Vec<f64> = self.treads.iter().map(|t| t.area()).collect();
        let volume: f64 = areas
            .iter()
            .enumerate()
            .map(|(i, a)| a * (self.tread_top(i) - self.step_bottom(i)))
            .sum();
        let narrow = self
            .nosings
            .windows(2)
            .map(|w| (w[1].sl - w[0].sl).min(w[1].sr - w[0].sr))
            .fold(f64::INFINITY, f64::min);
        RunQty {
            risers: self.risers,
            treads: self.treads.len(),
            riser: self.riser,
            going: self.going,
            comfort: 2.0 * self.riser + self.going,
            walk_length: self.walk_length,
            walk_area: areas.iter().sum(),
            tread_areas: areas,
            volume,
            narrow,
            headroom: self.headroom().map(|h| h.2),
        }
    }

    /// One closed body per step, arcs within chord sag `sag`.
    pub fn meshes(&self, sag: f64) -> Vec<Mesh> {
        let tol = Tol::default();
        let mut out = vec![];
        for (i, t) in self.treads.iter().enumerate() {
            let (lo, hi) = (self.step_bottom(i), self.tread_top(i));
            let Ok(p) = Profile::new(t.tessellate(sag), vec![], tol) else {
                continue;
            };
            if let Ok(m) = Solid::extrude(&p, hi - lo, tol).translated_z(lo).triangulate() {
                out.push(m);
            }
        }
        out
    }

    pub fn plan(&self, sag: f64) -> RunPlan {
        RunPlan {
            outline: self.treads.iter().map(|t| t.tessellate(sag)).collect(),
            nosings: self.nosings.iter().map(|n| (n.left, n.right)).collect(),
            walk: self.walk.clone(),
        }
    }
}

fn bbox(r: &[V2]) -> (V2, V2) {
    let mut lo = v2(f64::INFINITY, f64::INFINITY);
    let mut hi = v2(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in r {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    (lo, hi)
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
