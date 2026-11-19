//! Host surfaces of surface-hosted structures (diagrid, gridshell, cable net): a parametric
//! surface trait and the analytic hosts plane, cylinder, sphere, hyperbolic paraboloid, torus
//! patch and paraboloid of revolution with exact derivatives. A freeform surface element hosts a grid by implementing
//! [`HostSurface`]; the generators only call the trait.

use std::f64::consts::{FRAC_PI_2, TAU};

use crate::math::{v3, V3};

/// Parametric surface P(u, v) over a rectangular parameter domain.
pub trait HostSurface {
    fn point(&self, u: f64, v: f64) -> V3;
    /// Partial derivatives (dP/du, dP/dv).
    fn derivs(&self, u: f64, v: f64) -> (V3, V3);
    /// Parameter ranges ((u0, u1), (v0, v1)).
    fn domain(&self) -> ((f64, f64), (f64, f64));
    /// The u direction is a full turn: u1 meets u0.
    fn closed_u(&self) -> bool {
        false
    }
    /// Unit normal dP/du x dP/dv; zero where the surface is degenerate.
    fn normal(&self, u: f64, v: f64) -> V3 {
        let (a, b) = self.derivs(u, v);
        let n = a.cross(b);
        let l = n.len();
        if l > 1e-15 {
            n * (1.0 / l)
        } else {
            v3(0.0, 0.0, 0.0)
        }
    }
    /// Parameters of the closest surface point: the best sample of a coarse grid, then
    /// Gauss-Newton steps clamped to the domain (wrapped in u when closed).
    fn project(&self, p: V3) -> (f64, f64) {
        let ((u0, u1), (v0, v1)) = self.domain();
        let n = 16;
        let mut best = (u0, v0, f64::MAX);
        for i in 0..=n {
            for j in 0..=n {
                let u = u0 + (u1 - u0) * i as f64 / n as f64;
                let v = v0 + (v1 - v0) * j as f64 / n as f64;
                let d = (self.point(u, v) - p).len();
                if d < best.2 {
                    best = (u, v, d);
                }
            }
        }
        let (mut u, mut v) = (best.0, best.1);
        let span = (u1 - u0).abs().max(v1 - v0).max(1e-12);
        for _ in 0..60 {
            let r = self.point(u, v) - p;
            let (a, b) = self.derivs(u, v);
            let (aa, ab, bb) = (a.dot(a), a.dot(b), b.dot(b));
            let det = aa * bb - ab * ab;
            if !(det.abs() > 1e-30) {
                break;
            }
            let (ga, gb) = (a.dot(r), b.dot(r));
            let du = -(bb * ga - ab * gb) / det;
            let dv = -(aa * gb - ab * ga) / det;
            u += du;
            v += dv;
            if self.closed_u() {
                let w = u1 - u0;
                u = u0 + (u - u0).rem_euclid(w);
            } else {
                u = u.clamp(u0, u1);
            }
            v = v.clamp(v0, v1);
            if du.abs() + dv.abs() < 1e-14 * span {
                break;
            }
        }
        (u, v)
    }
}

/// Right-handed orthonormal axes at an origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    pub o: V3,
    pub x: V3,
    pub y: V3,
    pub z: V3,
}

impl Axes {
    pub fn world() -> Axes {
        Axes {
            o: v3(0.0, 0.0, 0.0),
            x: v3(1.0, 0.0, 0.0),
            y: v3(0.0, 1.0, 0.0),
            z: v3(0.0, 0.0, 1.0),
        }
    }

    /// Axes with `z` along `zdir` and `x` as close to `xhint` as `z` allows.
    pub fn new(o: V3, zdir: V3, xhint: V3) -> Option<Axes> {
        let zl = zdir.len();
        if !(zl > 1e-12) {
            return None;
        }
        let z = zdir * (1.0 / zl);
        let x = xhint - z * z.dot(xhint);
        let xl = x.len();
        if !(xl > 1e-9) {
            return None;
        }
        let x = x * (1.0 / xl);
        Some(Axes {
            o,
            x,
            y: z.cross(x),
            z,
        })
    }

    pub fn at(&self, a: f64, b: f64, c: f64) -> V3 {
        self.o + self.x * a + self.y * b + self.z * c
    }

    pub fn dir(&self, a: f64, b: f64, c: f64) -> V3 {
        self.x * a + self.y * b + self.z * c
    }

    fn ok(&self) -> bool {
        let f = |p: V3| p.x.is_finite() && p.y.is_finite() && p.z.is_finite();
        f(self.o)
            && [self.x, self.y, self.z]
                .iter()
                .all(|&a| f(a) && (a.len() - 1.0).abs() < 1e-9)
            && self.x.dot(self.y).abs() < 1e-9
            && self.y.dot(self.z).abs() < 1e-9
            && self.x.dot(self.z).abs() < 1e-9
            && (self.x.cross(self.y) - self.z).len() < 1e-9
    }
}

/// Analytic host. Angles in radians; all lengths in metres.
#[derive(Clone, Debug, PartialEq)]
pub enum Host {
    /// P = o + x u + y v, u in [0, w], v in [0, h].
    Plane { ax: Axes, w: f64, h: f64 },
    /// Axis `z` through `o`; u is the angle from `x` toward `y` in [a0, a1], v runs along the
    /// axis in [0, len]. Normal points away from the axis.
    Cylinder {
        ax: Axes,
        r: f64,
        a0: f64,
        a1: f64,
        len: f64,
    },
    /// Centre `o`; u is the longitude from `x` toward `y`, v the latitude toward `z`. The
    /// latitudes stay off the poles. Normal points outward.
    Sphere {
        ax: Axes,
        r: f64,
        lon0: f64,
        lon1: f64,
        lat0: f64,
        lat1: f64,
    },
    /// Saddle z = c u v over u in [-lx/2, lx/2], v in [-ly/2, ly/2], corners at +-`rise`.
    Hypar {
        ax: Axes,
        lx: f64,
        ly: f64,
        rise: f64,
    },
    /// Axis `z` through `o`, ring radius `big`, tube radius `r`; u turns about the axis from
    /// `x`, v about the tube (0 = outer equator, pi/2 = top). Normal points out of the tube.
    Torus {
        ax: Axes,
        big: f64,
        r: f64,
        u0: f64,
        u1: f64,
        v0: f64,
        v1: f64,
    },
    /// Paraboloid of revolution z = rho^2 / (4 f) about `z` through the vertex `o`, focus at
    /// `o + f z`; u turns about the axis from `x` over a full turn, v is the distance from the
    /// axis in [r0, r1] (r0 > 0: a grid cannot meet in the vertex). Normal points away from
    /// the focus.
    Paraboloid { ax: Axes, f: f64, r0: f64, r1: f64 },
}

fn full_turn(a0: f64, a1: f64) -> bool {
    (a1 - a0 - TAU).abs() < 1e-9
}

fn fin(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl Host {
    pub fn kind(&self) -> &'static str {
        match self {
            Host::Plane { .. } => "plane",
            Host::Cylinder { .. } => "cylinder",
            Host::Sphere { .. } => "sphere",
            Host::Hypar { .. } => "hypar",
            Host::Torus { .. } => "torus",
            Host::Paraboloid { .. } => "paraboloid",
        }
    }

    pub fn axes(&self) -> &Axes {
        match self {
            Host::Plane { ax, .. }
            | Host::Cylinder { ax, .. }
            | Host::Sphere { ax, .. }
            | Host::Hypar { ax, .. }
            | Host::Torus { ax, .. }
            | Host::Paraboloid { ax, .. } => ax,
        }
    }

    fn numbers(&self) -> Vec<f64> {
        match *self {
            Host::Plane { w, h, .. } => vec![w, h],
            Host::Cylinder { r, a0, a1, len, .. } => vec![r, a0, a1, len],
            Host::Sphere {
                r,
                lon0,
                lon1,
                lat0,
                lat1,
                ..
            } => vec![r, lon0, lon1, lat0, lat1],
            Host::Hypar { lx, ly, rise, .. } => vec![lx, ly, rise],
            Host::Torus {
                big,
                r,
                u0,
                u1,
                v0,
                v1,
                ..
            } => vec![big, r, u0, u1, v0, v1],
            Host::Paraboloid { f, r0, r1, .. } => vec![f, r0, r1],
        }
    }

    /// Dimensions and ranges a surface can be built from; the message names the bad value.
    pub fn check(&self) -> Result<(), String> {
        if !self.axes().ok() {
            return Err("host axes are not orthonormal".into());
        }
        if !fin(&self.numbers()) {
            return Err("host values must be finite".into());
        }
        let turn = |a: f64, b: f64, what: &str| -> Result<(), String> {
            if !(b > a) || b - a > TAU + 1e-9 {
                return Err(format!("{what} range must be increasing and at most a full turn"));
            }
            Ok(())
        };
        match *self {
            Host::Plane { w, h, .. } => {
                if !(w > 1e-3 && h > 1e-3) {
                    return Err("plane width and height must be positive".into());
                }
            }
            Host::Cylinder { r, a0, a1, len, .. } => {
                if !(r > 1e-3 && len > 1e-3) {
                    return Err("cylinder radius and length must be positive".into());
                }
                turn(a0, a1, "cylinder angle")?;
            }
            Host::Sphere {
                r,
                lon0,
                lon1,
                lat0,
                lat1,
                ..
            } => {
                if !(r > 1e-3) {
                    return Err("sphere radius must be positive".into());
                }
                turn(lon0, lon1, "sphere longitude")?;
                let pole = FRAC_PI_2 - 1e-6;
                if !(lat1 > lat0 && lat0 > -pole && lat1 < pole) {
                    return Err(
                        "sphere latitudes must increase and stay off the poles (a grid meets in \
                         a point there)"
                            .into(),
                    );
                }
            }
            Host::Hypar { lx, ly, .. } => {
                if !(lx > 1e-3 && ly > 1e-3) {
                    return Err("hypar spans must be positive".into());
                }
            }
            Host::Torus {
                big,
                r,
                u0,
                u1,
                v0,
                v1,
                ..
            } => {
                if !(r > 1e-3 && big > r) {
                    return Err("torus ring radius must exceed the tube radius".into());
                }
                turn(u0, u1, "torus ring angle")?;
                turn(v0, v1, "torus tube angle")?;
            }
            Host::Paraboloid { f, r0, r1, .. } => {
                if !(f > 1e-3 && r0 > 1e-3 && r1 > r0) {
                    return Err(
                        "paraboloid focal length must be positive and the hub ring smaller than                          the rim (a grid meets in a point at the vertex)"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// One line of text: kind, origin, z axis, x axis, then the numbers of the kind.
    pub fn text(&self) -> String {
        let a = self.axes();
        let mut v = vec![a.o.x, a.o.y, a.o.z, a.z.x, a.z.y, a.z.z, a.x.x, a.x.y, a.x.z];
        v.extend(self.numbers());
        let nums: Vec<String> = v.iter().map(|x| format!("{x}")).collect();
        format!("{} {}", self.kind(), nums.join(" "))
    }

    pub fn parse(s: &str) -> Option<Host> {
        let mut it = s.split_whitespace();
        let kind = it.next()?;
        let v: Vec<f64> = it.map(|t| t.parse().ok()).collect::<Option<_>>()?;
        if v.len() < 9 {
            return None;
        }
        let ax = Axes::new(v3(v[0], v[1], v[2]), v3(v[3], v[4], v[5]), v3(v[6], v[7], v[8]))?;
        let n = &v[9..];
        let h = match (kind, n.len()) {
            ("plane", 2) => Host::Plane {
                ax,
                w: n[0],
                h: n[1],
            },
            ("cylinder", 4) => Host::Cylinder {
                ax,
                r: n[0],
                a0: n[1],
                a1: n[2],
                len: n[3],
            },
            ("sphere", 5) => Host::Sphere {
                ax,
                r: n[0],
                lon0: n[1],
                lon1: n[2],
                lat0: n[3],
                lat1: n[4],
            },
            ("hypar", 3) => Host::Hypar {
                ax,
                lx: n[0],
                ly: n[1],
                rise: n[2],
            },
            ("torus", 6) => Host::Torus {
                ax,
                big: n[0],
                r: n[1],
                u0: n[2],
                u1: n[3],
                v0: n[4],
                v1: n[5],
            },
            ("paraboloid", 3) => Host::Paraboloid {
                ax,
                f: n[0],
                r0: n[1],
                r1: n[2],
            },
            _ => return None,
        };
        Some(h)
    }

    /// Spherical cap on a base circle of `radius` at `center` rising `rise` (at most the
    /// radius); `oculus` is the radius of the top ring, 0 takes a tenth of the base radius
    /// (a grid cannot meet in the pole). Lattice points match a cap parametrized by longitude
    /// and polar angle from the springing ring.
    pub fn dome(center: V3, radius: f64, rise: f64, oculus: f64) -> Result<Host, String> {
        if !(radius > 1e-3 && rise > 1e-3 && rise <= radius + 1e-9) {
            return Err("dome: radius and rise must be positive, the rise at most the radius".into());
        }
        let top = if oculus > 0.0 { oculus } else { radius / 10.0 };
        if !(top < radius) {
            return Err("dome: the top ring must be smaller than the base circle".into());
        }
        let big = (radius * radius + rise * rise) / (2.0 * rise);
        let h = Host::Sphere {
            ax: Axes { o: v3(center.x, center.y, center.z + rise - big), ..Axes::world() },
            r: big,
            lon0: 0.0,
            lon1: TAU,
            lat0: ((big - rise) / big).clamp(-1.0, 1.0).asin(),
            lat1: (top / big).clamp(-1.0, 1.0).acos(),
        };
        h.check()?;
        Ok(h)
    }

    /// Circular barrel vault: `a` to `b` is the level axis at the springing line, `span`
    /// across it, `rise` at the crown (at most half the span). u runs across, v along.
    pub fn vault(a: V3, b: V3, span: f64, rise: f64) -> Result<Host, String> {
        let d = b - a;
        let len = d.len();
        if !(len > 1e-3 && d.z.abs() < 1e-6 * len.max(1.0)) {
            return Err("vault: the axis must be level and of non-zero length".into());
        }
        let half = span / 2.0;
        if !(half > 1e-3 && rise > 1e-3 && rise <= half + 1e-9) {
            return Err("vault: span and rise must be positive, the rise at most half the span".into());
        }
        let up = v3(0.0, 0.0, 1.0);
        let big = (half * half + rise * rise) / (2.0 * rise);
        let th = (half / big).clamp(-1.0, 1.0).asin();
        let ax = Axes::new(a + up * (rise - big), d, up.cross(d))
            .ok_or("vault: the axis must be level and of non-zero length")?;
        let h = Host::Cylinder { ax, r: big, a0: FRAC_PI_2 - th, a1: FRAC_PI_2 + th, len };
        h.check()?;
        Ok(h)
    }

    /// Saddle over an `lx` x `ly` rectangle centred on `center`, turned by `angle` (rad) about
    /// the vertical, corners at +-`rise`.
    pub fn hypar_at(center: V3, lx: f64, ly: f64, rise: f64, angle: f64) -> Result<Host, String> {
        let ax = Axes::new(center, v3(0.0, 0.0, 1.0), v3(angle.cos(), angle.sin(), 0.0))
            .ok_or("hypar: the angle must be finite")?;
        let h = Host::Hypar { ax, lx, ly, rise };
        h.check()?;
        Ok(h)
    }

    /// Flat grid over a `w` x `h` rectangle from `corner`, turned by `angle` (rad).
    pub fn plane_at(corner: V3, w: f64, h: f64, angle: f64) -> Result<Host, String> {
        let ax = Axes::new(corner, v3(0.0, 0.0, 1.0), v3(angle.cos(), angle.sin(), 0.0))
            .ok_or("plane: the angle must be finite")?;
        let p = Host::Plane { ax, w, h };
        p.check()?;
        Ok(p)
    }

    /// Dish of `radius` at the rim and `depth` below it at the `vertex`, its axis tilted by
    /// `tilt` (rad) from the vertical toward the plan direction `azimuth` (rad); `hub` is the
    /// radius of the inner ring, 0 takes a tenth of the rim radius. The depth is at most the
    /// radius (focal length at least a quarter of it).
    pub fn paraboloid(
        vertex: V3,
        radius: f64,
        depth: f64,
        hub: f64,
        tilt: f64,
        azimuth: f64,
    ) -> Result<Host, String> {
        if !(radius > 1e-3 && depth > 1e-3 && depth <= radius + 1e-9) {
            return Err("paraboloid: radius and depth must be positive, the depth at most the radius".into());
        }
        let r0 = if hub > 0.0 { hub } else { radius / 10.0 };
        if !(r0 < radius) {
            return Err("paraboloid: the hub ring must be smaller than the rim".into());
        }
        if !(tilt.abs() < FRAC_PI_2) {
            return Err("paraboloid: the axis tilt must be less than a right angle".into());
        }
        let (st, ct) = tilt.sin_cos();
        let (sa, ca) = azimuth.sin_cos();
        let ax = Axes::new(vertex, v3(st * ca, st * sa, ct), v3(ct * ca, ct * sa, -st))
            .ok_or("paraboloid: the tilt and azimuth must be finite")?;
        let h = Host::Paraboloid { ax, f: radius * radius / (4.0 * depth), r0, r1: radius };
        h.check()?;
        Ok(h)
    }

    /// Exact area by Gauss-Legendre on |P_u x P_v| (closed forms are checked in the tests).
    pub fn area(&self) -> f64 {
        let ((u0, u1), (v0, v1)) = self.domain();
        // pieces keep the 10-point rule at round-off on these smooth integrands
        let k = 16;
        let piece = |a: f64, b: f64, i: usize| {
            (
                a + (b - a) * i as f64 / k as f64,
                a + (b - a) * (i + 1) as f64 / k as f64,
            )
        };
        let gu = |u: f64| {
            (0..k)
                .map(|j| {
                    let (a, b) = piece(v0, v1, j);
                    crate::frame3::gauss(a, b, |v| self.normal_len(u, v))
                })
                .sum::<f64>()
        };
        (0..k)
            .map(|i| {
                let (a, b) = piece(u0, u1, i);
                crate::frame3::gauss(a, b, gu)
            })
            .sum()
    }

    fn normal_len(&self, u: f64, v: f64) -> f64 {
        let (a, b) = self.derivs(u, v);
        a.cross(b).len()
    }
}

impl HostSurface for Host {
    fn point(&self, u: f64, v: f64) -> V3 {
        match *self {
            Host::Plane { ax, .. } => ax.at(u, v, 0.0),
            Host::Cylinder { ax, r, .. } => ax.at(r * u.cos(), r * u.sin(), v),
            Host::Sphere { ax, r, .. } => {
                ax.at(r * v.cos() * u.cos(), r * v.cos() * u.sin(), r * v.sin())
            }
            Host::Hypar { ax, lx, ly, rise } => {
                let c = 4.0 * rise / (lx * ly);
                ax.at(u, v, c * u * v)
            }
            Host::Torus { ax, big, r, .. } => {
                let w = big + r * v.cos();
                ax.at(w * u.cos(), w * u.sin(), r * v.sin())
            }
            Host::Paraboloid { ax, f, .. } => ax.at(v * u.cos(), v * u.sin(), v * v / (4.0 * f)),
        }
    }

    fn derivs(&self, u: f64, v: f64) -> (V3, V3) {
        match *self {
            Host::Plane { ax, .. } => (ax.x, ax.y),
            Host::Cylinder { ax, r, .. } => (ax.dir(-r * u.sin(), r * u.cos(), 0.0), ax.z),
            Host::Sphere { ax, r, .. } => (
                ax.dir(-r * v.cos() * u.sin(), r * v.cos() * u.cos(), 0.0),
                ax.dir(-r * v.sin() * u.cos(), -r * v.sin() * u.sin(), r * v.cos()),
            ),
            Host::Hypar { ax, lx, ly, rise } => {
                let c = 4.0 * rise / (lx * ly);
                (ax.dir(1.0, 0.0, c * v), ax.dir(0.0, 1.0, c * u))
            }
            Host::Torus { ax, big, r, .. } => {
                let w = big + r * v.cos();
                (
                    ax.dir(-w * u.sin(), w * u.cos(), 0.0),
                    ax.dir(-r * v.sin() * u.cos(), -r * v.sin() * u.sin(), r * v.cos()),
                )
            }
            Host::Paraboloid { ax, f, .. } => (
                ax.dir(-v * u.sin(), v * u.cos(), 0.0),
                ax.dir(u.cos(), u.sin(), v / (2.0 * f)),
            ),
        }
    }

    fn domain(&self) -> ((f64, f64), (f64, f64)) {
        match *self {
            Host::Plane { w, h, .. } => ((0.0, w), (0.0, h)),
            Host::Cylinder { a0, a1, len, .. } => ((a0, a1), (0.0, len)),
            Host::Sphere {
                lon0,
                lon1,
                lat0,
                lat1,
                ..
            } => ((lon0, lon1), (lat0, lat1)),
            Host::Hypar { lx, ly, .. } => ((-lx / 2.0, lx / 2.0), (-ly / 2.0, ly / 2.0)),
            Host::Torus { u0, u1, v0, v1, .. } => ((u0, u1), (v0, v1)),
            Host::Paraboloid { r0, r1, .. } => ((0.0, TAU), (r0, r1)),
        }
    }

    fn closed_u(&self) -> bool {
        match *self {
            Host::Cylinder { a0, a1, .. } => full_turn(a0, a1),
            Host::Sphere { lon0, lon1, .. } => full_turn(lon0, lon1),
            Host::Torus { u0, u1, .. } => full_turn(u0, u1),
            Host::Paraboloid { .. } => true,
            _ => false,
        }
    }

    fn project(&self, p: V3) -> (f64, f64) {
        let ((u0, u1), (v0, v1)) = self.domain();
        let ax = self.axes();
        let q = p - ax.o;
        let (lx, ly, lz) = (q.dot(ax.x), q.dot(ax.y), q.dot(ax.z));
        let wrap = |a: f64| {
            if self.closed_u() {
                u0 + (a - u0).rem_euclid(u1 - u0)
            } else {
                // nearest end of the range when the angle falls outside it
                let mut b = u0 + (a - u0).rem_euclid(TAU);
                if b > u1 {
                    let (d0, d1) = (TAU - (b - u0), b - u1);
                    b = if d0 < d1 { u0 } else { u1 };
                }
                b
            }
        };
        match *self {
            Host::Plane { .. } => (lx.clamp(u0, u1), ly.clamp(v0, v1)),
            Host::Cylinder { .. } => (wrap(ly.atan2(lx)), lz.clamp(v0, v1)),
            Host::Sphere { .. } => {
                let u = wrap(ly.atan2(lx));
                let v = lz.atan2(lx.hypot(ly)).clamp(v0, v1);
                (u, v)
            }
            Host::Torus { big, .. } => {
                let u = wrap(ly.atan2(lx));
                let rho = lx.hypot(ly) - big;
                let mut v = lz.atan2(rho);
                if v < v0 {
                    v += TAU;
                }
                (u, v.clamp(v0, v1))
            }
            Host::Paraboloid { f, .. } => {
                // the foot lies in the meridian plane of p: nearest point of the parabola there
                let (rho, a) = (lx.hypot(ly), 1.0 / (4.0 * f));
                let gap = |v: f64| (v - rho).powi(2) + (a * v * v - lz).powi(2);
                let n = 64;
                let mut v = (0..=n)
                    .map(|i| v0 + (v1 - v0) * i as f64 / n as f64)
                    .min_by(|x, y| gap(*x).total_cmp(&gap(*y)))
                    .unwrap_or(v0);
                for _ in 0..30 {
                    let h = v - rho + 2.0 * a * v * (a * v * v - lz);
                    let dh = 1.0 + 2.0 * a * (3.0 * a * v * v - lz);
                    if !(dh > 1e-12) {
                        break;
                    }
                    let next = (v - h / dh).clamp(v0, v1);
                    let done = (next - v).abs() < 1e-14 * v1;
                    v = next;
                    if done {
                        break;
                    }
                }
                (wrap(ly.atan2(lx)), v)
            }
            Host::Hypar { .. } => {
                // generic Gauss-Newton from the trait (no closed form)
                struct G<'a>(&'a Host);
                impl HostSurface for G<'_> {
                    fn point(&self, u: f64, v: f64) -> V3 {
                        self.0.point(u, v)
                    }
                    fn derivs(&self, u: f64, v: f64) -> (V3, V3) {
                        self.0.derivs(u, v)
                    }
                    fn domain(&self) -> ((f64, f64), (f64, f64)) {
                        self.0.domain()
                    }
                }
                G(self).project(p)
            }
        }
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
