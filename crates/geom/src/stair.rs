// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Stairs: straight / L / U-shaped flights, the riser/tread rule (2R + T ∈ [0.60, 0.65] m),
//! landings, stringers; path-following railings with posts, balusters and a handrail.
//!
//! A flight is a profile in the (u along the flight, z) plane with a sawtooth of steps on top and an inclined slab
//! below (thickness along the normal), extruded by the width to the right of the direction. Volume = area × width.

use crate::brep::Solid;
use crate::math::{v2, v3, Tol, V2, V3};
use crate::mesh::Mesh;
use crate::poly;
use crate::profile::Profile;

pub mod run;

/// Bounds of the step rule, m.
pub const COMFORT: (f64, f64) = (0.60, 0.65);
/// Rule target when the tread is not given: T = 0.63 − 2R.
pub const COMFORT_TARGET: f64 = 0.63;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Straight,
    /// 90° turn on a square landing of the flight width.
    L {
        left: bool,
        first: Option<usize>,
    },
    /// 180° turn: landing of depth `landing`, gap between flights `gap`.
    U {
        left: bool,
        first: Option<usize>,
        gap: f64,
    },
    /// Spiral stair: sector treads around the core `core` out to `radius`, `turn` rad in total;
    /// `origin` is the core centre.
    Spiral {
        radius: f64,
        core: f64,
        turn: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StairDef {
    /// Midpoint of the bottom line of the first riser.
    pub origin: V2,
    /// Ascent direction of the first flight, rad.
    pub angle: f64,
    pub base_z: f64,
    pub height: f64,
    pub width: f64,
    pub max_riser: f64,
    /// Tread; None means by the rule 2R + T = 0.63.
    pub tread: Option<f64>,
    /// Landing depth of a U-stair (an L-stair always uses the flight width).
    pub landing: f64,
    /// Thickness of the flight slab (along the normal) and of the landing.
    pub thickness: f64,
    /// Stringers (width, height along the normal) on both sides outside the flight.
    pub stringer: Option<(f64, f64)>,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StairError {
    Input(&'static str),
    /// 2R + T outside [0.60, 0.65] m.
    Comfort {
        riser: f64,
        tread: f64,
        value: f64,
    },
    /// An L/U-stair flight shorter than two risers.
    Split {
        first: usize,
        total: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flight {
    /// Lower-left corner (on the first riser line).
    pub start: V2,
    pub dir: V2,
    pub width: f64,
    pub risers: usize,
    /// Elevation of the bottom of the first riser.
    pub z0: f64,
}

impl Flight {
    pub fn right(&self) -> V2 {
        v2(self.dir.y, -self.dir.x)
    }
    pub fn treads(&self) -> usize {
        self.risers - 1
    }
    /// Plan ring, counter-clockwise.
    pub fn ring(&self, tread: f64) -> Vec<V2> {
        let l = tread * self.treads() as f64;
        let (s, d, r) = (self.start, self.dir, self.right() * self.width);
        vec![s, s + r, s + r + d * l, s + d * l].pipe_ccw()
    }
}

trait Ccw {
    fn pipe_ccw(self) -> Vec<V2>;
}
impl Ccw for Vec<V2> {
    fn pipe_ccw(mut self) -> Vec<V2> {
        if poly::area(&self) < 0.0 {
            self.reverse();
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Landing {
    pub ring: Vec<V2>,
    pub top: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stair {
    pub risers: usize,
    pub riser: f64,
    pub tread: f64,
    pub flights: Vec<Flight>,
    pub landings: Vec<Landing>,
    /// Spiral stair treads: sectors around the core, with the top elevation of each.
    pub winders: Vec<Landing>,
    pub thickness: f64,
    pub stringer: Option<(f64, f64)>,
    pub shape: Shape,
    /// Landing depth of a U-stair.
    pub landing_depth: f64,
    /// Bottom and top elevations.
    pub z: (f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StairQty {
    pub risers: usize,
    pub treads: usize,
    pub riser: f64,
    pub tread: f64,
    /// 2R + T.
    pub comfort: f64,
    pub run_length: f64,
    pub flight_volume: f64,
    pub landing_volume: f64,
    pub stringer_volume: f64,
    pub volume: f64,
    /// Plan area of treads and landings.
    pub walk_area: f64,
}

fn right(d: V2) -> V2 {
    v2(d.y, -d.x)
}

impl StairDef {
    pub fn layout(&self) -> Result<Stair, StairError> {
        let fin = [
            self.height,
            self.width,
            self.max_riser,
            self.thickness,
            self.base_z,
            self.angle,
            self.landing,
        ];
        if fin.iter().any(|v| !v.is_finite()) {
            return Err(StairError::Input("non-finite value"));
        }
        if self.height <= 0.0 || self.width <= 0.0 || self.max_riser <= 0.0 || self.thickness <= 0.0
        {
            return Err(StairError::Input(
                "height, width, max riser, thickness must be > 0",
            ));
        }
        let n = (self.height / self.max_riser - 1e-9).ceil().max(1.0) as usize;
        let r = self.height / n as f64;
        if let Shape::Spiral { radius, core, turn } = self.shape {
            if !(core > 0.0 && radius > core && turn > 0.0 && radius.is_finite() && turn.is_finite()) {
                return Err(StairError::Input("spiral: 0 < core < radius and turn > 0"));
            }
        }
        // a spiral's going is its arc on the walk line, half way across the treads
        let t = match self.shape {
            Shape::Spiral { radius, core, turn } => (core + radius) / 2.0 * turn / n as f64,
            _ => self.tread.unwrap_or(COMFORT_TARGET - 2.0 * r),
        };
        if !(t.is_finite() && t > 0.0) {
            return Err(StairError::Input("tread must be > 0"));
        }
        let c = 2.0 * r + t;
        if c < COMFORT.0 - 1e-9 || c > COMFORT.1 + 1e-9 {
            return Err(StairError::Comfort {
                riser: r,
                tread: t,
                value: c,
            });
        }
        let d = v2(self.angle.cos(), self.angle.sin());
        let rt = right(d);
        let w = self.width;
        let p0 = self.origin - rt * (w / 2.0);
        let z0 = self.base_z;
        let fl = |start, dir, risers, z0| Flight {
            start,
            dir,
            width: w,
            risers,
            z0,
        };
        let split = |first: Option<usize>| -> Result<(usize, usize), StairError> {
            let k1 = first.unwrap_or(n.div_ceil(2));
            if k1 < 2 || n < k1 + 2 {
                return Err(StairError::Split {
                    first: k1,
                    total: n,
                });
            }
            Ok((k1, n - k1))
        };
        let rect = |o: V2, u: (f64, f64), v: (f64, f64)| -> Vec<V2> {
            vec![
                o + d * u.0 + rt * v.0,
                o + d * u.1 + rt * v.0,
                o + d * u.1 + rt * v.1,
                o + d * u.0 + rt * v.1,
            ]
            .pipe_ccw()
        };
        let winders: Vec<Landing> = match self.shape {
            Shape::Spiral { radius, core, turn } => {
                let theta = turn / n as f64;
                let c = self.origin;
                let arc = |rad: f64, a: f64, b: f64| -> Vec<V2> {
                    (0..=4)
                        .map(|j| {
                            let x = a + (b - a) * j as f64 / 4.0;
                            c + v2(x.cos(), x.sin()) * rad
                        })
                        .collect()
                };
                (0..n)
                    .map(|i| {
                        let a0 = self.angle + theta * i as f64;
                        let a1 = a0 + theta;
                        let mut ring = arc(radius, a0, a1);
                        ring.extend(arc(core, a1, a0));
                        Landing { ring: ring.pipe_ccw(), top: z0 + r * (i + 1) as f64 }
                    })
                    .collect()
            }
            _ => vec![],
        };
        let (flights, landings) = match self.shape {
            Shape::Spiral { .. } => (vec![], vec![]),
            Shape::Straight => {
                if n < 2 {
                    return Err(StairError::Split { first: n, total: n });
                }
                (vec![fl(p0, d, n, z0)], vec![])
            }
            Shape::L { left, first } => {
                let (k1, k2) = split(first)?;
                let l1 = t * (k1 - 1) as f64;
                let zl = z0 + r * k1 as f64;
                let f2 = if left {
                    fl(p0 + d * l1, v2(-d.y, d.x), k2, zl)
                } else {
                    fl(p0 + d * (l1 + w) + rt * w, rt, k2, zl)
                };
                let land = Landing {
                    ring: rect(p0, (l1, l1 + w), (0.0, w)),
                    top: zl,
                };
                (vec![fl(p0, d, k1, z0), f2], vec![land])
            }
            Shape::U { left, first, gap } => {
                if !(gap.is_finite() && gap >= 0.0 && self.landing > 0.0) {
                    return Err(StairError::Input("U stair: gap >= 0, landing > 0"));
                }
                let (k1, k2) = split(first)?;
                let l1 = t * (k1 - 1) as f64;
                let zl = z0 + r * k1 as f64;
                let (s2, across) = if left {
                    (p0 + d * l1 - rt * gap, (-(w + gap), w))
                } else {
                    (p0 + d * l1 + rt * (2.0 * w + gap), (0.0, 2.0 * w + gap))
                };
                let land = Landing {
                    ring: rect(p0, (l1, l1 + self.landing), across),
                    top: zl,
                };
                (vec![fl(p0, d, k1, z0), fl(s2, -d, k2, zl)], vec![land])
            }
        };
        if let Some((sw, sh)) = self.stringer {
            if !(sw > 0.0 && sh > 0.0) {
                return Err(StairError::Input("stringer size must be > 0"));
            }
        }
        Ok(Stair {
            risers: n,
            riser: r,
            tread: t,
            flights,
            landings,
            winders,
            thickness: self.thickness,
            stringer: self.stringer,
            shape: self.shape,
            landing_depth: self.landing,
            z: (z0, z0 + self.height),
        })
    }
}

/// Area by the shoelace (Gauss) formula for points (u, z).
fn shoelace(p: &[(f64, f64)]) -> f64 {
    let n = p.len();
    (0..n)
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

impl Stair {
    fn slope(&self) -> f64 {
        self.riser / self.tread
    }

    /// Vertical thickness of the inclined slab.
    fn waist(&self) -> f64 {
        self.thickness * (1.0 + self.slope().powi(2)).sqrt()
    }

    /// Flight profile (u, z measured from the flight bottom), counter-clockwise.
    pub fn flight_profile(&self, f: &Flight) -> Vec<(f64, f64)> {
        let (r, t) = (self.riser, self.tread);
        let m = f.treads();
        let l = t * m as f64;
        let top = r * m as f64;
        let dv = self.waist();
        let u0 = dv / self.slope();
        let mut p = if u0 < l && top - dv > 0.0 {
            vec![(0.0, 0.0), (u0, 0.0), (l, top - dv)]
        } else {
            vec![(0.0, 0.0), (l, 0.0)]
        };
        p.push((l, top));
        for i in (1..m).rev() {
            p.push((t * i as f64, r * (i + 1) as f64));
            p.push((t * i as f64, r * i as f64));
        }
        p.push((0.0, r));
        p
    }

    /// Flight top for sections: polyline (u, h) with vertical risers.
    pub fn flight_top(&self, f: &Flight) -> Vec<(f64, f64)> {
        let (r, t) = (self.riser, self.tread);
        let mut p = vec![(0.0, r)];
        for i in 1..f.treads() {
            p.push((t * i as f64, r * i as f64));
            p.push((t * i as f64, r * (i + 1) as f64));
        }
        p.push((t * f.treads() as f64, r * f.treads() as f64));
        p
    }

    /// Flight underside for sections: polyline (u, h).
    pub fn flight_bottom(&self, f: &Flight) -> Vec<(f64, f64)> {
        let p = self.flight_profile(f);
        let k = if p[2].0 == p[1].0 { 2 } else { 3 };
        p[..k].to_vec()
    }

    /// Stringer profiles (u, z measured from the flight bottom), counter-clockwise.
    fn stringer_profile(&self, f: &Flight, sh: f64) -> Vec<(f64, f64)> {
        let s = self.slope();
        let l = self.tread * f.treads() as f64;
        let hv = sh * (1.0 + s * s).sqrt();
        let (r, top_end) = (self.riser, self.riser + s * l);
        if r - hv >= 0.0 {
            vec![(0.0, r - hv), (l, top_end - hv), (l, top_end), (0.0, r)]
        } else {
            let ub = (hv - r) / s;
            if ub >= l {
                vec![(0.0, 0.0), (l, 0.0), (l, top_end), (0.0, r)]
            } else {
                vec![
                    (0.0, 0.0),
                    (ub, 0.0),
                    (l, top_end - hv),
                    (l, top_end),
                    (0.0, r),
                ]
            }
        }
    }

    pub fn quantities(&self) -> StairQty {
        let mut fv = 0.0;
        let mut sv = 0.0;
        let mut run = 0.0;
        let mut walk = 0.0;
        for f in &self.flights {
            fv += shoelace(&self.flight_profile(f)) * f.width;
            if let Some((sw, sh)) = self.stringer {
                sv += 2.0 * shoelace(&self.stringer_profile(f, sh)) * sw;
            }
            let l = self.tread * f.treads() as f64;
            run += l;
            walk += l * f.width;
        }
        run += self.tread * self.winders.len() as f64;
        let la: f64 = self.landings.iter().chain(&self.winders).map(|l| poly::area(&l.ring)).sum();
        let lv = la * self.thickness;
        StairQty {
            risers: self.risers,
            treads: self.flights.iter().map(|f| f.treads()).sum::<usize>() + self.winders.len(),
            riser: self.riser,
            tread: self.tread,
            comfort: 2.0 * self.riser + self.tread,
            run_length: run,
            flight_volume: fv,
            landing_volume: lv,
            stringer_volume: sv,
            volume: fv + lv + sv,
            walk_area: walk + la,
        }
    }

    /// Bodies: flights, landings, stringers, one mesh each.
    pub fn meshes(&self) -> Vec<Mesh> {
        let tol = Tol::default();
        let mut out = vec![];
        // profile (u, z) → extrusion by the width to the right of the direction
        let sweep = |p: &[(f64, f64)], o: V2, d: V2, z0: f64, w: f64| -> Option<Mesh> {
            let pts: Vec<V2> = p.iter().map(|q| v2(q.0, q.1)).collect();
            let pr = Profile::new(pts, vec![], tol).ok()?;
            let m = Solid::extrude(&pr, w, tol).triangulate().ok()?;
            let r = right(d);
            let mut m = m;
            for q in &mut m.positions {
                let xy = o + d * q.x + r * q.z;
                *q = v3(xy.x, xy.y, z0 + q.y);
            }
            Some(m)
        };
        for f in &self.flights {
            out.extend(sweep(
                &self.flight_profile(f),
                f.start,
                f.dir,
                f.z0,
                f.width,
            ));
            if let Some((sw, sh)) = self.stringer {
                let sp = self.stringer_profile(f, sh);
                let r = f.right();
                out.extend(sweep(&sp, f.start - r * sw, f.dir, f.z0, sw));
                out.extend(sweep(&sp, f.start + r * f.width, f.dir, f.z0, sw));
            }
        }
        for l in self.landings.iter().chain(&self.winders) {
            if let Ok(p) = Profile::new(l.ring.clone(), vec![], tol) {
                if let Ok(m) = Solid::extrude(&p, self.thickness, tol)
                    .translated_z(l.top - self.thickness)
                    .triangulate()
                {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Railing paths along the edges (left, right): points on the nosing line, landings along the edge.
    pub fn edge_paths(&self) -> [Vec<V3>; 2] {
        let (r, t) = (self.riser, self.tread);
        let edge = |f: &Flight, e: usize| -> (V3, V3) {
            let a = f.start + f.right() * (f.width * e as f64);
            let b = a + f.dir * (t * f.treads() as f64);
            (
                v3(a.x, a.y, f.z0 + r),
                v3(b.x, b.y, f.z0 + r * f.risers as f64),
            )
        };
        let mut out: [Vec<V3>; 2] = [vec![], vec![]];
        let Some(&f1) = self.flights.first() else {
            return out;
        };
        for (e, path) in out.iter_mut().enumerate() {
            let (a, b) = edge(&f1, e);
            path.extend([a, b]);
            let Some(f2) = self.flights.get(1) else {
                continue;
            };
            let zl = self.landings[0].top;
            let d = f1.dir;
            let rt = f1.right();
            let w = f1.width;
            let p0 = f1.start;
            let l1 = t * f1.treads() as f64;
            let corners: Vec<V2> = match self.shape {
                Shape::L { left: true, .. } if e == 1 => vec![p0 + d * (l1 + w) + rt * w],
                Shape::L { left: false, .. } if e == 0 => vec![p0 + d * (l1 + w)],
                Shape::U {
                    left: false, gap, ..
                } if e == 0 => {
                    let ld = self.landing_depth;
                    vec![
                        p0 + d * (l1 + ld),
                        p0 + d * (l1 + ld) + rt * (2.0 * w + gap),
                    ]
                }
                Shape::U {
                    left: true, gap, ..
                } if e == 1 => {
                    let ld = self.landing_depth;
                    vec![
                        p0 + d * (l1 + ld) + rt * w,
                        p0 + d * (l1 + ld) - rt * (gap + w),
                    ]
                }
                _ => vec![],
            };
            path.extend(corners.into_iter().map(|c| v3(c.x, c.y, zl)));
            let (a2, b2) = edge(f2, e);
            // the start of the second flight at the landing edge is at the landing elevation
            path.push(v3(a2.x, a2.y, zl));
            path.push(a2);
            path.push(b2);
            path.dedup_by(|x, y| (*x - *y).len() < 1e-9);
        }
        out
    }

    /// Plan view for cut elevation `cut_z`.
    pub fn plan(&self, cut_z: f64) -> StairPlan {
        let (r, t) = (self.riser, self.tread);
        let mut p = StairPlan::default();
        let mut walk: Vec<V2> = vec![];
        let mut cut_at: Option<V2> = None;
        for f in &self.flights {
            p.outline.push(f.ring(t));
            let rw = f.right() * f.width;
            let c0 = f.start + rw * 0.5;
            if walk.last().map_or(true, |q| (*q - c0).len() > 1e-9) {
                walk.push(c0);
            }
            for i in 0..f.risers {
                let a = f.start + f.dir * (t * i as f64);
                let z = f.z0 + r * (i + 1) as f64;
                let seg = (a, a + rw);
                if z <= cut_z + 1e-9 {
                    p.below.push(seg);
                } else {
                    if cut_at.is_none() {
                        // break line: a diagonal across half of the tread
                        let u = (t * i as f64 - t * 0.5).max(0.0);
                        let a = f.start + f.dir * u;
                        let b = a + rw + f.dir * t;
                        p.cut = Some([a, a + (b - a) * 0.45 + f.dir * (t * 0.3), b]);
                        cut_at = Some(f.start + rw * 0.5 + f.dir * (u + t * 0.5));
                    }
                    p.above.push(seg);
                }
            }
            walk.push(f.start + rw * 0.5 + f.dir * (t * f.treads() as f64));
        }
        for l in &self.landings {
            p.outline.push(l.ring.clone());
        }
        // ascent arrow: along the walking line up to the break (or to the end)
        if let Some(c) = cut_at {
            let mut path = vec![walk[0]];
            let mut done = false;
            for w in walk.windows(2) {
                let (a, b) = (w[0], w[1]);
                let ab = b - a;
                let l2 = ab.dot(ab);
                let s = if l2 > 0.0 { (c - a).dot(ab) / l2 } else { -1.0 };
                let off = if l2 > 0.0 {
                    (c - a).cross(ab).abs() / l2.sqrt()
                } else {
                    1.0
                };
                if (0.0..=1.0).contains(&s) && off < 1e-6 {
                    path.push(c);
                    done = true;
                    break;
                }
                path.push(b);
            }
            if done {
                walk = path;
            }
        }
        if walk.len() >= 2 {
            let (a, b) = (walk[walk.len() - 2], walk[walk.len() - 1]);
            let d = (b - a) * (1.0 / (b - a).len().max(1e-12));
            let n = v2(-d.y, d.x);
            let h = (t * 0.6).min(0.25);
            p.head = Some([b - d * h + n * (h * 0.4), b, b - d * h - n * (h * 0.4)]);
        }
        p.arrow = walk;
        p
    }
}

/// Stair plan: outlines, riser lines below/above the cut, break line, arrow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StairPlan {
    pub outline: Vec<Vec<V2>>,
    pub below: Vec<(V2, V2)>,
    pub above: Vec<(V2, V2)>,
    pub cut: Option<[V2; 3]>,
    pub arrow: Vec<V2>,
    pub head: Option<[V2; 3]>,
}

/// Railing along a path (bottom points: step nosings, landing or floor edge).
#[derive(Clone, Debug, PartialEq)]
pub struct Railing {
    pub path: Vec<V3>,
    pub height: f64,
    pub post_spacing: f64,
    pub baluster_spacing: f64,
    /// Side of the square cross-section of posts and balusters.
    pub post: f64,
    pub baluster: f64,
    /// Handrail: width × height.
    pub rail: (f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailingLayout {
    pub posts: Vec<V3>,
    pub balusters: Vec<V3>,
    /// Handrail axis (top of the railing).
    pub rail: Vec<V3>,
    pub length: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailingQty {
    pub length: f64,
    pub posts: usize,
    pub balusters: usize,
    pub post_volume: f64,
    pub baluster_volume: f64,
    pub rail_volume: f64,
}

impl Railing {
    pub fn check(&self) -> Result<(), &'static str> {
        let v = [
            self.height,
            self.post_spacing,
            self.baluster_spacing,
            self.post,
            self.baluster,
            self.rail.0,
            self.rail.1,
        ];
        if v.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
            return Err("railing sizes and spacings must be > 0");
        }
        if self.rail.1 >= self.height {
            return Err("rail higher than railing");
        }
        if self.path.len() < 2
            || self
                .path
                .iter()
                .any(|p| !(p.x.is_finite() && p.y.is_finite() && p.z.is_finite()))
        {
            return Err("railing path needs two finite points");
        }
        Ok(())
    }

    /// Posts at vertices and at spacing no greater than the given one; balusters evenly between posts.
    pub fn layout(&self) -> RailingLayout {
        let mut posts = vec![];
        let mut bal = vec![];
        let mut len = 0.0;
        for (k, w) in self.path.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let l = (b - a).len();
            if l < 1e-12 {
                continue;
            }
            len += l;
            let n = ((l / self.post_spacing - 1e-9).ceil() as usize).max(1);
            if k == 0 || posts.is_empty() {
                posts.push(a);
            }
            for i in 1..=n {
                let p = a + (b - a) * (i as f64 / n as f64);
                let q = a + (b - a) * ((i - 1) as f64 / n as f64);
                let span = l / n as f64;
                let m = ((span / self.baluster_spacing - 1e-9).ceil() as usize).max(1) - 1;
                for j in 1..=m {
                    bal.push(q + (p - q) * (j as f64 / (m + 1) as f64));
                }
                posts.push(p);
            }
        }
        let up = v3(0.0, 0.0, self.height - self.rail.1 / 2.0);
        RailingLayout {
            rail: self.path.iter().map(|p| *p + up).collect(),
            posts,
            balusters: bal,
            length: len,
        }
    }

    pub fn quantities(&self) -> RailingQty {
        let l = self.layout();
        let below = self.height - self.rail.1;
        RailingQty {
            length: l.length,
            posts: l.posts.len(),
            balusters: l.balusters.len(),
            post_volume: l.posts.len() as f64 * self.post * self.post * below,
            baluster_volume: l.balusters.len() as f64 * self.baluster * self.baluster * below,
            rail_volume: self.rail.0 * self.rail.1 * l.length,
        }
    }

    /// Meshes of posts, balusters and handrail links (vertical side faces).
    pub fn meshes(&self) -> Vec<Mesh> {
        let l = self.layout();
        let below = self.height - self.rail.1;
        let mut out = vec![];
        for (pts, s) in [(&l.posts, self.post), (&l.balusters, self.baluster)] {
            for p in pts {
                let h = s / 2.0;
                let ring = [v2(-h, -h), v2(h, -h), v2(h, h), v2(-h, h)].map(|q| v2(p.x, p.y) + q);
                out.push(box_mesh(&ring, &[p.z; 4], below));
            }
        }
        for w in self.path.windows(2) {
            let (a, b) = (w[0], w[1]);
            let d = v2(b.x - a.x, b.y - a.y);
            if d.len() < 1e-12 {
                continue;
            }
            let n = v2(-d.y, d.x) * (self.rail.0 / 2.0 / d.len());
            let (pa, pb) = (v2(a.x, a.y), v2(b.x, b.y));
            let z0 = [a.z, b.z, b.z, a.z].map(|z| z + below);
            out.push(box_mesh(
                &[pa - n, pb - n, pb + n, pa + n],
                &z0,
                self.rail.1,
            ));
        }
        out
    }
}

/// Prism over a quadrilateral (counter-clockwise) with per-vertex bottom elevations and height h.
fn box_mesh(r: &[V2; 4], z0: &[f64; 4], h: f64) -> Mesh {
    let mut m = Mesh::default();
    for i in 0..4 {
        m.positions.push(v3(r[i].x, r[i].y, z0[i]));
    }
    for i in 0..4 {
        m.positions.push(v3(r[i].x, r[i].y, z0[i] + h));
    }
    let f: [[u32; 3]; 12] = [
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    m.triangles = f.to_vec();
    m.face_of = (0..12u32).map(|i| i / 2).collect();
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(shape: Shape) -> StairDef {
        StairDef {
            origin: v2(0.0, 0.0),
            angle: 0.0,
            base_z: 0.0,
            height: 3.0,
            width: 1.2,
            max_riser: 0.18,
            tread: None,
            landing: 1.2,
            thickness: 0.15,
            stringer: None,
            shape,
        }
    }

    #[test]
    fn straight_3000_gives_17_risers() {
        let s = def(Shape::Straight).layout().unwrap();
        assert_eq!(s.risers, 17);
        assert!((s.riser * 1000.0 - 176.470_588_235).abs() < 1e-6);
        assert_eq!(format!("{:.2}", s.riser * 1000.0), "176.47");
        let t = COMFORT_TARGET - 2.0 * 3.0 / 17.0;
        assert!((s.tread - t).abs() < 1e-15);
        let c = 2.0 * s.riser + s.tread;
        assert!((COMFORT.0..=COMFORT.1).contains(&c));
        let q = s.quantities();
        assert_eq!((q.risers, q.treads), (17, 16));
        assert!((q.run_length - 16.0 * t).abs() < 1e-12);
        // flight volume = profile area × width; matches the body
        let m = &s.meshes()[0];
        assert!(m.is_watertight());
        assert!((m.volume() - q.flight_volume).abs() < 1e-9 * q.flight_volume);
    }

    #[test]
    fn comfort_rule_rejects_bad_tread() {
        let mut d = def(Shape::Straight);
        d.tread = Some(0.20);
        assert!(matches!(d.layout(), Err(StairError::Comfort { .. })));
        d.tread = Some(0.28);
        assert!(d.layout().is_ok());
    }

    #[test]
    fn l_and_u_shapes() {
        for left in [true, false] {
            let s = def(Shape::L { left, first: None }).layout().unwrap();
            assert_eq!(s.flights.len(), 2);
            assert_eq!(s.flights[0].risers + s.flights[1].risers, 17);
            assert_eq!(s.landings.len(), 1);
            assert!((s.landings[0].top - 9.0 * s.riser).abs() < 1e-12);
            let q = s.quantities();
            assert!((q.landing_volume - 1.2 * 1.2 * 0.15).abs() < 1e-12);
            for m in s.meshes() {
                assert!(m.is_watertight());
                assert!(m.volume() > 0.0);
            }
            let p = s.edge_paths();
            // the outer edge goes around the landing corner, the inner one does not
            let (inner, outer) = if left { (0, 1) } else { (1, 0) };
            assert_eq!(p[inner].len(), 4);
            assert_eq!(p[outer].len(), 6);
            assert!((p[0].last().unwrap().z - 3.0).abs() < 1e-12);
            let u = def(Shape::U {
                left,
                first: None,
                gap: 0.1,
            })
            .layout()
            .unwrap();
            let q = u.quantities();
            assert!((q.landing_volume - 1.2 * 2.5 * 0.15).abs() < 1e-12);
            assert_eq!(u.edge_paths()[outer].len(), 7);
        }
    }

    #[test]
    fn spiral_winds_treads_round_the_core() {
        let mut d = def(Shape::Spiral { radius: 1.2, core: 0.3, turn: 4.2 });
        d.height = 2.8;
        d.max_riser = 0.2;
        d.base_z = 0.0;
        d.angle = 0.0;
        let s = d.layout().unwrap();
        assert_eq!(s.risers, 14);
        assert_eq!(s.winders.len(), 14);
        assert!(s.flights.is_empty() && s.landings.is_empty());
        assert!((s.winders.last().unwrap().top - 2.8).abs() < 1e-9);
        assert!(s.edge_paths().iter().all(|p| p.is_empty()));
        assert!(!s.meshes().is_empty());
        let q = s.quantities();
        assert!(q.walk_area > 0.0);
        assert!(COMFORT.0 <= q.comfort && q.comfort <= COMFORT.1, "{}", q.comfort);
    }

    #[test]
    fn spiral_needs_a_core_inside_its_radius() {
        let mut d = def(Shape::Spiral { radius: 1.2, core: 0.0, turn: 4.2 });
        d.height = 2.8;
        d.max_riser = 0.2;
        assert!(matches!(d.layout(), Err(StairError::Input(_))));
    }

    #[test]
    fn plan_has_cut_and_arrow() {
        let s = def(Shape::Straight).layout().unwrap();
        let p = s.plan(1.2);
        // 1.2 / 0.17647 = 6.8 → 6 risers below the cut
        assert_eq!(p.below.len(), 6);
        assert_eq!(p.above.len(), 11);
        assert!(p.cut.is_some() && p.head.is_some());
        assert_eq!(p.arrow.len(), 2);
    }

    #[test]
    fn railing_posts_for_4_2_m_at_1_0() {
        let r = Railing {
            path: vec![v3(0.0, 0.0, 0.0), v3(4.2, 0.0, 0.0)],
            height: 0.9,
            post_spacing: 1.0,
            baluster_spacing: 0.12,
            post: 0.05,
            baluster: 0.02,
            rail: (0.05, 0.04),
        };
        let q = r.quantities();
        assert_eq!(q.posts, 6);
        // span 0.84 / 0.12 = 7 → 6 balusters in each of the 5 spans
        assert_eq!(q.balusters, 30);
        assert!((q.length - 4.2).abs() < 1e-12);
        assert!((q.rail_volume - 0.05 * 0.04 * 4.2).abs() < 1e-15);
        assert_eq!(r.meshes().len(), 6 + 30 + 1);
        assert!(r
            .meshes()
            .iter()
            .all(|m| m.is_watertight() && m.volume() > 0.0));
    }
}
