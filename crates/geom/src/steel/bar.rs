//! A joined member as a connection sees it: straight axis, section axes, the end at the node,
//! the faces a plate can sit on, and the section's material along a line (bolt grips).

use super::ConnError;
use crate::math::{v2, v3, V2, V3};
use crate::member::{Cap, Member, Shape};
use crate::poly;

/// Section kind with the sizes a detail needs (metres). `tw`/`tf` of a hollow section are its wall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    I,
    Channel,
    Angle,
    Tee,
    Tube,
    Pipe,
    Rect,
    Circle,
    Poly,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dims {
    pub kind: Kind,
    /// Depth along ez and width along ey.
    pub h: f64,
    pub b: f64,
    pub tw: f64,
    pub tf: f64,
}

impl Dims {
    pub fn of(s: &Shape) -> Dims {
        let (b, h) = s.size();
        let (kind, tw, tf) = match *s {
            Shape::I { tw, tf, .. } => (Kind::I, tw, tf),
            Shape::Channel { tw, tf, .. } => (Kind::Channel, tw, tf),
            Shape::Tee { tw, tf, .. } => (Kind::Tee, tw, tf),
            Shape::Angle { t, .. } => (Kind::Angle, t, t),
            Shape::Tube { t, .. } => (Kind::Tube, t, t),
            Shape::Pipe { t, .. } => (Kind::Pipe, t, t),
            Shape::Rect { .. } => (Kind::Rect, b.min(h), b.min(h)),
            Shape::Circle { d } => (Kind::Circle, d, d),
            Shape::Poly(_) => (Kind::Poly, b.min(h), b.min(h)),
        };
        Dims { kind, h, b, tw, tf }
    }

    /// Thinnest wall: what a weld or a bolt bears on.
    pub fn thin(&self) -> f64 {
        self.tw.min(self.tf)
    }

    pub fn round(&self) -> bool {
        matches!(self.kind, Kind::Pipe | Kind::Circle)
    }

    pub fn hollow(&self) -> bool {
        matches!(self.kind, Kind::Pipe | Kind::Tube)
    }
}

/// Which end of a member is at the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Start,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub shape: Shape,
    pub dims: Dims,
    /// Section box centre at the path start; axis and section axes.
    pub c0: V3,
    pub u: V3,
    pub ey: V3,
    pub ez: V3,
    /// Body ends along `u` from `c0` (end planes cut at the axis).
    pub t0: f64,
    pub t1: f64,
    pub caps: (Cap, Cap),
    pub column: bool,
    /// Outer ring and hole of the section (y, z about the box centre).
    pub outer: Vec<V2>,
    pub inner: Option<Vec<V2>>,
}

impl Bar {
    pub fn new(m: &Member) -> Result<Bar, ConnError> {
        let (c0, u, ey, ez) = m
            .section_basis()
            .ok_or_else(|| ConnError::new("a joined member must be straight"))?;
        let caps = m
            .end_caps()
            .ok_or_else(|| ConnError::new("a joined member must be straight"))?;
        let hit = |c: &Cap| c.n.dot(c.p - c0) / c.n.dot(u);
        let (t0, t1) = (hit(&caps.0), hit(&caps.1));
        if !(t1 - t0 > 1e-6) {
            return Err(ConnError::new("a joined member has no length"));
        }
        m.shape.check().map_err(ConnError::new)?;
        let (outer, inner) = m.shape.rings();
        Ok(Bar {
            dims: Dims::of(&m.shape),
            shape: m.shape.clone(),
            c0,
            u,
            ey,
            ez,
            t0,
            t1,
            caps,
            column: m.column,
            outer,
            inner,
        })
    }

    pub fn at(&self, t: f64, y: f64, z: f64) -> V3 {
        self.c0 + self.u * t + self.ey * y + self.ez * z
    }

    /// (t, y, z) of a world point.
    pub fn local(&self, p: V3) -> V3 {
        let d = p - self.c0;
        v3(d.dot(self.u), d.dot(self.ey), d.dot(self.ez))
    }

    /// Axis point at `t`.
    pub fn axis(&self, t: f64) -> V3 {
        self.c0 + self.u * t
    }

    pub fn len(&self) -> f64 {
        self.t1 - self.t0
    }

    /// The end nearer to `p`, its axis parameter and the unit direction pointing into the bar.
    pub fn end_near(&self, p: V3) -> (End, f64, V3) {
        let t = self.local(p).x;
        if (t - self.t0).abs() <= (t - self.t1).abs() {
            (End::Start, self.t0, self.u)
        } else {
            (End::End, self.t1, -self.u)
        }
    }

    pub fn cap(&self, e: End) -> Cap {
        match e {
            End::Start => self.caps.0,
            End::End => self.caps.1,
        }
    }

    /// Section direction (y, z) → world.
    pub fn sdir(&self, d: V2) -> V3 {
        self.ey * d.x + self.ez * d.y
    }

    /// Material of the section along the line through (y, z) in direction `d` (section plane),
    /// as intervals of the line parameter (unit `d`).
    pub fn cut_line(&self, p: V2, d: V2) -> Vec<(f64, f64)> {
        let reach = 2.0 * (self.dims.h + self.dims.b);
        let (a, b) = (p - d * reach, p + d * reach);
        let mut rings = vec![self.outer.clone()];
        rings.extend(self.inner.clone());
        poly::inside_intervals(a, b, &rings)
            .into_iter()
            .map(|(s, e)| (s * 2.0 * reach - reach, e * 2.0 * reach - reach))
            .collect()
    }

    /// Point of the section (y, z) strictly inside the material.
    pub fn contains(&self, p: V2) -> bool {
        let mut rings = vec![self.outer.clone()];
        rings.extend(self.inner.clone());
        poly::contains(&rings, p)
    }

    /// World point inside the body (between the end planes and in the section), shrunk by `tol`.
    pub fn holds(&self, p: V3, tol: f64) -> bool {
        let q = self.local(p);
        let (s, e) = self.caps;
        let inside_caps = s.n.dot(p - s.p) < -tol && e.n.dot(p - e.p) < -tol;
        if !inside_caps {
            return false;
        }
        let p = v2(q.y, q.z);
        let mut rings = vec![self.outer.clone()];
        rings.extend(self.inner.clone());
        poly::contains(&rings, p) && rings.iter().all(|r| ring_dist(r, p) > tol)
    }

    /// Outer face of the section that best faces the world direction `d`: (outward unit normal
    /// in the section, distance of the face from the box centre, its extent along the face).
    pub fn face_toward(&self, d: V3) -> Face {
        let ds = v2(d.dot(self.ey), d.dot(self.ez));
        let r = &self.outer;
        let n = r.len();
        let mut best: Option<(f64, Face)> = None;
        for i in 0..n {
            let (p, q) = (r[i], r[(i + 1) % n]);
            let e = q - p;
            let l = e.len();
            if l < 1e-9 {
                continue;
            }
            // counter-clockwise ring: outward normal is the edge turned clockwise
            let nn = v2(e.y / l, -e.x / l);
            let off = nn.dot(p);
            // facing d first, then the widest face (a web between flanges beats a flange tip)
            let score = nn.dot(ds) * 10.0 + l + off * 1e-3;
            if nn.dot(ds) < 0.3 {
                continue;
            }
            if best.as_ref().map_or(true, |b| score > b.0) {
                best = Some((
                    score,
                    Face {
                        n: nn,
                        off,
                        a: p,
                        b: q,
                    },
                ));
            }
        }
        best.map(|b| b.1).unwrap_or_else(|| {
            let len = ds.len().max(1e-12);
            let nn = v2(ds.x / len, ds.y / len);
            let off = r.iter().map(|p| nn.dot(*p)).fold(f64::MIN, f64::max);
            let t = v2(-nn.y, nn.x);
            Face {
                n: nn,
                off,
                a: nn * off - t * 0.01,
                b: nn * off + t * 0.01,
            }
        })
    }

    /// Half extent of the section along a section direction (from the box centre).
    pub fn reach(&self, d: V2) -> f64 {
        self.outer.iter().map(|p| p.dot(d)).fold(f64::MIN, f64::max)
    }
}

/// A flat face of a section: outward normal and offset (section coordinates), edge ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub n: V2,
    pub off: f64,
    pub a: V2,
    pub b: V2,
}

/// Distance from `p` to the closed ring's edges.
fn ring_dist(r: &[V2], p: V2) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.dot(ab).max(1e-18)).clamp(0.0, 1.0);
            (a + ab * t - p).len()
        })
        .fold(f64::MAX, f64::min)
}

/// Least-squares meeting point of the bar axes; parallel axes meet between their nearest ends.
pub fn meet(bars: &[&Bar]) -> Result<V3, ConnError> {
    let mut a = [[0.0f64; 3]; 3];
    let mut rhs = [0.0f64; 3];
    for b in bars {
        let u = [b.u.x, b.u.y, b.u.z];
        let p = [b.c0.x, b.c0.y, b.c0.z];
        for i in 0..3 {
            for j in 0..3 {
                let m = if i == j { 1.0 } else { 0.0 } - u[i] * u[j];
                a[i][j] += m;
                rhs[i] += m * p[j];
            }
        }
    }
    if let Some(x) = solve3(a, rhs) {
        return Ok(v3(x[0], x[1], x[2]));
    }
    // parallel: the nearest pair of ends of the first two bars
    let (p, q) = match bars {
        [b0, b1, ..] => {
            let ends = |b: &Bar| [b.axis(b.t0), b.axis(b.t1)];
            let mut best = (f64::MAX, V3::default(), V3::default());
            for x in ends(b0) {
                for y in ends(b1) {
                    let d = (x - y).len();
                    if d < best.0 {
                        best = (d, x, y);
                    }
                }
            }
            (best.1, best.2)
        }
        _ => return Err(ConnError::new("the joined members do not meet")),
    };
    Ok((p + q) * 0.5)
}

fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(&a);
    let scale = a
        .iter()
        .flatten()
        .map(|x| x.abs())
        .fold(0.0, f64::max)
        .max(1e-300);
    if d.abs() < 1e-6 * scale * scale * scale {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut m = a;
        for i in 0..3 {
            m[i][k] = b[i];
        }
        *o = det(&m) / d;
    }
    Some(out)
}

/// Distance between the closest points of two segments.
pub fn seg_dist(p0: V3, p1: V3, q0: V3, q1: V3) -> f64 {
    let (d1, d2, r) = (p1 - p0, q1 - q0, p0 - q0);
    let (a, e, f) = (d1.dot(d1), d2.dot(d2), d2.dot(r));
    let (s, t);
    if a <= 1e-18 && e <= 1e-18 {
        return r.len();
    }
    if a <= 1e-18 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-18 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let den = a * e - b * b;
            let mut s0 = if den > 1e-18 {
                ((b * f - c * e) / den).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    ((p0 + d1 * s) - (q0 + d2 * t)).len()
}

/// Unit vector of `v` with its component along `n` removed.
pub fn across(v: V3, n: V3) -> V3 {
    let n = n.normalized();
    (v - n * n.dot(v)).normalized()
}

/// Intersection of the line `p + d s` with the plane through `o` with normal `n`.
pub fn hit_plane(p: V3, d: V3, o: V3, n: V3) -> Option<f64> {
    let den = n.dot(d);
    (den.abs() > 1e-12).then(|| n.dot(o - p) / den)
}
