//! Ramp: polyline axis, slope and rise. Layout follows the axis: inclined runs no longer than
//! `max_run`, intermediate landings between them, and a corner landing at every axis bend.
//! The rise is always reached: the last segment is extended and any excess axis length is dropped.

use crate::brep::Solid;
use crate::math::{v2, v3, Tol, V2};
use crate::mesh::Mesh;
use crate::profile::Profile;

const EPS: f64 = 1e-6;
/// An axis bend with (1 + cos) below this is a turn in place: no landing is built.
const MIN_TURN: f64 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub struct RampDef {
    /// Ramp axis from bottom to top (at least two points).
    pub path: Vec<V2>,
    pub base_z: f64,
    pub rise: f64,
    pub width: f64,
    /// Slope: rise per unit of plan length (1:12 ≈ 0.0833).
    pub slope: f64,
    pub thickness: f64,
    /// Length of an intermediate landing.
    pub landing: f64,
    /// Maximum run length in plan.
    pub max_run: f64,
}

/// Inclined run: `start` is the midpoint of the lower edge, `dir` is the unit direction of ascent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampFlight {
    pub start: V2,
    pub dir: V2,
    pub len: f64,
    pub z0: f64,
    pub z1: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RampLanding {
    pub ring: Vec<V2>,
    pub top: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ramp {
    pub def: RampDef,
    pub flights: Vec<RampFlight>,
    pub landings: Vec<RampLanding>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampQty {
    /// Total run length in plan.
    pub run: f64,
    pub rise: f64,
    pub volume: f64,
    /// Top surface area in plan.
    pub area: f64,
    pub landings: usize,
}

/// Plan symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct RampPlan {
    /// Outlines of runs and landings.
    pub outlines: Vec<Vec<V2>>,
    /// Direction-of-ascent line along the axis, bottom to top.
    pub run_line: Vec<V2>,
    /// Two arrowhead strokes at the upper end of the ascent line.
    pub head: [(V2, V2); 2],
    /// Break line at the cut-plane elevation, if the ramp crosses it.
    pub cut: Option<(V2, V2)>,
}

fn left(d: V2) -> V2 {
    v2(-d.y, d.x)
}

/// Convex hull, counter-clockwise (monotone chain).
fn hull(mut pts: Vec<V2>) -> Vec<V2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup_by(|a, b| (*a - *b).len() < EPS);
    if pts.len() < 3 {
        return pts;
    }
    let turn = |o: V2, a: V2, b: V2| (a - o).cross(b - o);
    let mut low: Vec<V2> = vec![];
    for &p in &pts {
        while low.len() >= 2 && turn(low[low.len() - 2], low[low.len() - 1], p) <= EPS * EPS {
            low.pop();
        }
        low.push(p);
    }
    let mut up: Vec<V2> = vec![];
    for &p in pts.iter().rev() {
        while up.len() >= 2 && turn(up[up.len() - 2], up[up.len() - 1], p) <= EPS * EPS {
            up.pop();
        }
        up.push(p);
    }
    low.pop();
    up.pop();
    low.extend(up);
    low
}

/// An overlap below this fraction of the width is a joint at a landing, not a collision (hand-drawn axis).
const OVERLAP_TOL: f64 = 0.05;

/// Mutual penetration depth of convex outlines; zero or less means they do not intersect.
fn penetration(a: &[V2], b: &[V2]) -> f64 {
    let span = |r: &[V2], n: V2| {
        r.iter()
            .map(|x| x.dot(n))
            .fold((f64::MAX, f64::MIN), |m: (f64, f64), v: f64| {
                (m.0.min(v), m.1.max(v))
            })
    };
    let mut depth = f64::INFINITY;
    for p in [a, b] {
        for i in 0..p.len() {
            let e = p[(i + 1) % p.len()] - p[i];
            let l = e.len();
            if l < EPS {
                continue;
            }
            let n = v2(-e.y / l, e.x / l);
            let ((a0, a1), (b0, b1)) = (span(a, n), span(b, n));
            depth = depth.min(a1.min(b1) - a0.max(b0));
        }
    }
    depth
}

/// Rectangle along `d` of length `len` from the edge midpoint `o`, counter-clockwise.
fn strip(o: V2, d: V2, len: f64, w: f64) -> Vec<V2> {
    let n = left(d) * (w / 2.0);
    let e = o + d * len;
    vec![o - n, e - n, e + n, o + n]
}

/// Corner landing: edges of the incoming and outgoing runs plus the outer corner.
fn corner(p: V2, d1: V2, d2: V2, w: f64) -> Vec<V2> {
    let (n1, n2) = (left(d1) * (w / 2.0), left(d2) * (w / 2.0));
    let (a, b) = (p - d1 * (w / 2.0), p + d2 * (w / 2.0));
    let k = 1.0 + d1.dot(d2);
    let outer = (n1 + n2) * (1.0 / k.max(MIN_TURN));
    let side = if d1.cross(d2) >= 0.0 { -1.0 } else { 1.0 };
    hull(vec![a - n1, a + n1, b - n2, b + n2, p + outer * side])
}

/// Remove coincident and collinear axis points.
fn simplify(path: &[V2]) -> Vec<V2> {
    let mut out: Vec<V2> = vec![];
    for &p in path {
        if out.last().is_some_and(|l| (*l - p).len() < EPS) {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (u, v) = (b - a, p - b);
            if u.cross(v).abs() < 1e-9 * u.len() * v.len() && u.dot(v) > 0.0 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

impl RampDef {
    /// Run length in plan needed for the rise.
    pub fn required_run(&self) -> f64 {
        if self.slope > 0.0 {
            self.rise / self.slope
        } else {
            f64::INFINITY
        }
    }

    fn check(&self) -> Result<(), String> {
        let nums = [
            self.base_z,
            self.rise,
            self.width,
            self.slope,
            self.thickness,
            self.landing,
            self.max_run,
        ];
        let finite_path = self.path.iter().all(|p| p.x.is_finite() && p.y.is_finite());
        if !nums.iter().all(|v| v.is_finite()) || !finite_path {
            return Err("non-finite value".into());
        }
        if self.rise <= 0.0 {
            return Err("rise must be positive".into());
        }
        if self.width < 0.3 {
            return Err("width below 0.3 m".into());
        }
        if !(self.slope > 0.0 && self.slope <= 1.0) {
            return Err("slope out of range".into());
        }
        if self.thickness <= 0.0 {
            return Err("thickness must be positive".into());
        }
        if self.landing < 0.3 {
            return Err("landing below 0.3 m".into());
        }
        if self.max_run < 0.5 {
            return Err("run limit below 0.5 m".into());
        }
        Ok(())
    }

    pub fn layout(&self) -> Result<Ramp, String> {
        self.check()?;
        let path = simplify(&self.path);
        if path.len() < 2 {
            return Err("path needs two distinct points".into());
        }
        let w = self.width;
        let n = path.len() - 1;
        let dirs: Vec<V2> = path
            .windows(2)
            .map(|s| {
                let v = s[1] - s[0];
                v * (1.0 / v.len())
            })
            .collect();
        for i in 0..n {
            let len = (path[i + 1] - path[i]).len();
            let need = match (i == 0, i == n - 1) {
                (true, true) => 0.0,
                (false, false) => w,
                _ => w / 2.0,
            };
            if len + EPS < need {
                return Err(format!("segment {} shorter than the ramp width", i + 1));
            }
            if i + 1 < n && 1.0 + dirs[i].dot(dirs[i + 1]) < MIN_TURN {
                return Err(format!("turn {} too sharp for a landing", i + 1));
            }
        }
        let mut flights = vec![];
        let mut landings = vec![];
        let mut z = self.base_z;
        let mut left_run = self.required_run();
        for i in 0..n {
            let d = dirs[i];
            let a = path[i];
            let last = i == n - 1;
            let mut s = if i > 0 { w / 2.0 } else { 0.0 };
            // the last segment is unbounded: the axis is extended to reach the required rise
            let end = if last {
                f64::INFINITY
            } else {
                (path[i + 1] - a).len() - w / 2.0
            };
            while left_run > EPS && s < end - EPS {
                let l = left_run.min(self.max_run).min(end - s);
                let z1 = z + l * self.slope;
                flights.push(RampFlight {
                    start: a + d * s,
                    dir: d,
                    len: l,
                    z0: z,
                    z1,
                });
                z = z1;
                s += l;
                left_run -= l;
                if left_run > EPS && s < end - EPS {
                    let g = self.landing.min(end - s);
                    landings.push(RampLanding {
                        ring: strip(a + d * s, d, g, w),
                        top: z,
                    });
                    s += g;
                }
            }
            if left_run > EPS && !last {
                landings.push(RampLanding {
                    ring: corner(path[i + 1], d, dirs[i + 1], w),
                    top: z,
                });
            }
        }
        if flights.is_empty() || left_run > 1e-3 {
            return Err("path cannot reach the required rise".into());
        }
        // parts overlapping in plan with no vertical clearance: sharp turn, self-crossing axis
        let t = self.thickness;
        let parts: Vec<(Vec<V2>, f64, f64)> = flights
            .iter()
            .map(|f| (f.ring(w), f.z0 - t, f.z1))
            .chain(landings.iter().map(|l| (l.ring.clone(), l.top - t, l.top)))
            .collect();
        for (i, a) in parts.iter().enumerate() {
            for b in &parts[i + 1..] {
                let z_clash = a.1 < b.2 - EPS && b.1 < a.2 - EPS;
                if z_clash && penetration(&a.0, &b.0) > OVERLAP_TOL * w {
                    return Err("path overlaps itself".into());
                }
            }
        }
        Ok(Ramp {
            def: self.clone(),
            flights,
            landings,
        })
    }
}

fn ring_area(r: &[V2]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| r[i].cross(r[(i + 1) % n]))
        .sum::<f64>()
        .abs()
        / 2.0
}

/// Outlines along the axis without layout: a ramp that currently fails to build (top level lowered,
/// etc.) stays visible and selectable.
pub fn axis_rings(path: &[V2], width: f64) -> Vec<Vec<V2>> {
    let w = if width.is_finite() && width > 0.0 {
        width
    } else {
        1.0
    };
    let rings: Vec<Vec<V2>> = simplify(path)
        .windows(2)
        .map(|s| {
            let v = s[1] - s[0];
            let l = v.len();
            strip(s[0], v * (1.0 / l), l, w)
        })
        .collect();
    match (rings.is_empty(), path.first()) {
        (true, Some(&c)) => vec![strip(c - v2(w / 2.0, 0.0), v2(1.0, 0.0), w, w)],
        _ => rings,
    }
}

/// Flat slabs from `axis_rings` with the top at `top`.
pub fn axis_meshes(path: &[V2], width: f64, top: f64, thickness: f64) -> Vec<Mesh> {
    let tol = Tol::default();
    let t = if thickness > 0.0 { thickness } else { 0.1 };
    axis_rings(path, width)
        .into_iter()
        .filter_map(|r| {
            let p = Profile::new(r, vec![], tol).ok()?;
            Solid::extrude(&p, t, tol)
                .translated_z(top - t)
                .triangulate()
                .ok()
        })
        .collect()
}

impl RampFlight {
    pub fn end(&self) -> V2 {
        self.start + self.dir * self.len
    }

    pub fn ring(&self, width: f64) -> Vec<V2> {
        strip(self.start, self.dir, self.len, width)
    }
}

impl Ramp {
    /// Axis end accounting for extension or trimming.
    pub fn end(&self) -> V2 {
        self.flights.last().map_or(v2(0.0, 0.0), |f| f.end())
    }

    pub fn top_z(&self) -> f64 {
        self.flights.last().map_or(self.def.base_z, |f| f.z1)
    }

    pub fn qty(&self) -> RampQty {
        let (w, t) = (self.def.width, self.def.thickness);
        let run: f64 = self.flights.iter().map(|f| f.len).sum();
        let la: f64 = self.landings.iter().map(|l| ring_area(&l.ring)).sum();
        RampQty {
            run,
            rise: self.def.rise,
            volume: (run * w + la) * t,
            area: run * w + la,
            landings: self.landings.len(),
        }
    }

    /// Run bodies (parallelogram in section, extruded to the width) and landings.
    pub fn meshes(&self) -> Vec<Mesh> {
        let tol = Tol::default();
        let (w, t) = (self.def.width, self.def.thickness);
        let mut out = vec![];
        for f in &self.flights {
            let h = f.z1 - f.z0;
            let pts = vec![v2(0.0, -t), v2(f.len, h - t), v2(f.len, h), v2(0.0, 0.0)];
            let Ok(pr) = Profile::new(pts, vec![], tol) else {
                continue;
            };
            let Ok(mut m) = Solid::extrude(&pr, w, tol).triangulate() else {
                continue;
            };
            // profile (u, z) → extrusion to the right from the left edge of the run
            let r = v2(f.dir.y, -f.dir.x);
            let o = f.start - r * (w / 2.0);
            for q in &mut m.positions {
                let xy = o + f.dir * q.x + r * q.z;
                *q = v3(xy.x, xy.y, f.z0 + q.y);
            }
            out.push(m);
        }
        for l in &self.landings {
            if let Ok(p) = Profile::new(l.ring.clone(), vec![], tol) {
                if let Ok(m) = Solid::extrude(&p, t, tol)
                    .translated_z(l.top - t)
                    .triangulate()
                {
                    out.push(m);
                }
            }
        }
        out
    }

    /// Plan symbol: `cut_z` is the absolute elevation of the cut plane.
    pub fn plan(&self, cut_z: f64) -> RampPlan {
        let w = self.def.width;
        let outlines = self
            .flights
            .iter()
            .map(|f| f.ring(w))
            .chain(self.landings.iter().map(|l| l.ring.clone()))
            .collect();
        let mut run_line: Vec<V2> = vec![];
        for f in &self.flights {
            if run_line.last().map_or(true, |l| (*l - f.start).len() > EPS) {
                run_line.push(f.start);
            }
            run_line.push(f.end());
        }
        let tip = self.end();
        let d = self.flights.last().map_or(v2(1.0, 0.0), |f| f.dir);
        let s = (w * 0.25).clamp(0.1, 0.4);
        let head = [
            (tip, tip - d * s + left(d) * (s * 0.5)),
            (tip, tip - d * s - left(d) * (s * 0.5)),
        ];
        let cut = self
            .flights
            .iter()
            .find(|f| f.z0 <= cut_z && cut_z < f.z1)
            .map(|f| {
                let p = f.start + f.dir * ((cut_z - f.z0) / (f.z1 - f.z0) * f.len);
                let n = left(f.dir) * (w / 2.0);
                (p - n, p + n)
            });
        RampPlan {
            outlines,
            run_line,
            head,
            cut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(path: Vec<V2>, rise: f64) -> RampDef {
        RampDef {
            path,
            base_z: 0.0,
            rise,
            width: 1.2,
            slope: 1.0 / 12.0,
            thickness: 0.2,
            landing: 1.5,
            max_run: 9.0,
        }
    }

    #[test]
    fn straight_run_extends_to_reach_rise() {
        let r = def(vec![v2(0.0, 0.0), v2(2.0, 0.0)], 0.5).layout().unwrap();
        assert_eq!(r.flights.len(), 1);
        assert!(r.landings.is_empty());
        assert!((r.flights[0].len - 6.0).abs() < 1e-9);
        assert!((r.top_z() - 0.5).abs() < 1e-9);
        assert!((r.end().x - 6.0).abs() < 1e-9);
    }

    #[test]
    fn long_run_gets_intermediate_landing() {
        let r = def(vec![v2(0.0, 0.0), v2(30.0, 0.0)], 1.0)
            .layout()
            .unwrap();
        assert_eq!(r.flights.len(), 2);
        assert_eq!(r.landings.len(), 1);
        assert!((r.flights[0].len - 9.0).abs() < 1e-9);
        assert!((r.flights[1].len - 3.0).abs() < 1e-9);
        assert!((r.flights[1].start.x - 10.5).abs() < 1e-9);
        assert!((r.landings[0].top - 0.75).abs() < 1e-9);
        assert!((r.top_z() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn surplus_path_is_dropped() {
        let p = vec![v2(0.0, 0.0), v2(20.0, 0.0), v2(20.0, 20.0)];
        let r = def(p, 0.25).layout().unwrap();
        assert_eq!(r.flights.len(), 1);
        assert!(r.landings.is_empty());
        assert!((r.end().x - 3.0).abs() < 1e-9);
    }

    #[test]
    fn bend_gets_square_corner_landing() {
        let p = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 5.0)];
        let r = def(p, 0.75).layout().unwrap();
        assert_eq!(r.flights.len(), 2);
        assert_eq!(r.landings.len(), 1);
        assert!((ring_area(&r.landings[0].ring) - 1.44).abs() < 1e-9);
        assert!((r.flights[0].len - 4.4).abs() < 1e-9);
        assert!((r.flights[1].start.y - 0.6).abs() < 1e-9);
        assert!((r.flights[0].z1 - r.flights[1].z0).abs() < 1e-12);
        assert!((r.top_z() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn collinear_points_do_not_create_landings() {
        let p = vec![v2(0.0, 0.0), v2(1.0, 0.0), v2(2.0, 0.0), v2(3.0, 0.0)];
        let r = def(p, 0.5).layout().unwrap();
        assert!(r.landings.is_empty());
        assert_eq!(r.flights.len(), 1);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(def(vec![v2(0.0, 0.0)], 1.0).layout().is_err());
        assert!(def(vec![v2(0.0, 0.0), v2(5.0, 0.0)], 0.0).layout().is_err());
        let mut d = def(vec![v2(0.0, 0.0), v2(5.0, 0.0)], 1.0);
        d.slope = 0.0;
        assert!(d.layout().is_err());
        let sharp = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(0.0, 0.01)];
        assert!(def(sharp, 1.0).layout().is_err());
        let short = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 0.5), v2(9.0, 0.5)];
        assert!(def(short, 1.0).layout().is_err());
    }

    #[test]
    fn sharp_turn_overlapping_in_plan_is_rejected() {
        let turn = |deg: f64| {
            let (s, c) = deg.to_radians().sin_cos();
            vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0 + 5.0 * c, 5.0 * s)]
        };
        assert!(def(turn(90.0), 0.75).layout().is_ok());
        assert!(
            def(turn(91.0), 0.75).layout().is_ok(),
            "a nearly right angle drawn by hand"
        );
        for deg in [120.0, 150.0] {
            assert_eq!(
                def(turn(deg), 0.75).layout().unwrap_err(),
                "path overlaps itself",
                "{deg}"
            );
        }
    }

    #[test]
    fn crossing_needs_clearance_under_the_upper_flight() {
        let p = vec![
            v2(0.0, 0.0),
            v2(10.0, 0.0),
            v2(10.0, 5.0),
            v2(5.0, 5.0),
            v2(5.0, -5.0),
        ];
        // nearly flat: the run returns to the same height
        let mut flat = def(p.clone(), 0.25);
        flat.slope = 0.01;
        assert!(flat.layout().is_err());
        // steep: passes over the first run
        assert!(def(p, 2.0).layout().is_ok());
    }

    #[test]
    fn axis_rings_keep_a_stub_for_any_path() {
        let rs = axis_rings(&[v2(0.0, 0.0), v2(4.0, 0.0), v2(4.0, 3.0)], 1.0);
        assert_eq!(rs.len(), 2);
        assert!((ring_area(&rs[0]) - 4.0).abs() < 1e-9);
        assert_eq!(axis_rings(&[v2(1.0, 1.0)], 1.0).len(), 1);
        assert!(axis_rings(&[], 1.0).is_empty());
        let ms = axis_meshes(&[v2(0.0, 0.0), v2(4.0, 0.0)], 1.0, 0.0, 0.2);
        assert_eq!(ms.len(), 1);
        assert!((ms[0].volume().abs() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn meshes_are_closed_and_match_quantities() {
        let p = vec![v2(0.0, 0.0), v2(5.0, 0.0), v2(5.0, 5.0)];
        let r = def(p, 0.75).layout().unwrap();
        let ms = r.meshes();
        assert_eq!(ms.len(), 3);
        let mut vol = 0.0;
        for m in &ms {
            assert!(m.is_watertight());
            vol += m.volume().abs();
        }
        assert!((vol - r.qty().volume).abs() < 1e-6, "{vol}");
    }

    #[test]
    fn plan_has_arrow_and_cut() {
        let r = def(vec![v2(0.0, 0.0), v2(5.0, 0.0)], 0.5).layout().unwrap();
        let pl = r.plan(0.25);
        assert_eq!(pl.outlines.len(), 1);
        assert_eq!(pl.run_line.len(), 2);
        assert!((pl.head[0].0.x - 6.0).abs() < 1e-9);
        let c = pl.cut.unwrap();
        assert!((c.0.x - 3.0).abs() < 1e-9);
        assert!(r.plan(5.0).cut.is_none());
    }
}
