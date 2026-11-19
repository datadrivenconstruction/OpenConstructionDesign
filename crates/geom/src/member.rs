// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Linear members (columns, beams): sections, catalogue, swept solid and exact quantities.
//! A section is given in the (y, z) plane: y is transverse (to the left along the run), z is up; the origin is
//! the centre of the bounding box. Fillets of rolled profiles are not modelled (area is below the tabulated one).
//! Volume = A·Lc (Lc is the length of the centroid line between the end planes; exact for any
//! planar cuts), lateral surface = Σ|edge|·length at the edge midpoint (exact for
//! polygons; circle/pipe is P·Lc, exact by symmetry), arcs use Pappus' theorem.

use std::f64::consts::PI;

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::poly;

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect {
        b: f64,
        h: f64,
    },
    Circle {
        d: f64,
    },
    /// I-beam (I and H are the same shape).
    I {
        h: f64,
        b: f64,
        tw: f64,
        tf: f64,
    },
    /// Channel: web on the left (−y), flanges to the right.
    Channel {
        h: f64,
        b: f64,
        tw: f64,
        tf: f64,
    },
    /// Angle: vertical leg on the left, horizontal leg at the bottom.
    Angle {
        h: f64,
        b: f64,
        t: f64,
    },
    /// Tee: flange on top.
    Tee {
        h: f64,
        b: f64,
        tw: f64,
        tf: f64,
    },
    Pipe {
        d: f64,
        t: f64,
    },
    Tube {
        h: f64,
        b: f64,
        t: f64,
    },
    /// Arbitrary simple loop (y, z).
    Poly(Vec<(f64, f64)>),
}

/// Circle segments in the mesh (quantities are exact).
pub const CIRCLE_SEGS: usize = 32;

fn rect(y0: f64, z0: f64, y1: f64, z1: f64) -> Vec<V2> {
    vec![v2(y0, z0), v2(y1, z0), v2(y1, z1), v2(y0, z1)]
}

fn circle(r: f64, n: usize) -> Vec<V2> {
    (0..n)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / n as f64;
            v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

impl Shape {
    /// Size check: all positive, walls thinner than the bounding box, loop simple.
    pub fn check(&self) -> Result<(), String> {
        let pos = |v: &[f64]| v.iter().all(|x| x.is_finite() && *x > 0.0);
        let ok = match self {
            Shape::Rect { b, h } => pos(&[*b, *h]),
            Shape::Circle { d } => pos(&[*d]),
            Shape::I { h, b, tw, tf } | Shape::Channel { h, b, tw, tf } => {
                pos(&[*h, *b, *tw, *tf]) && tw < b && 2.0 * tf < *h
            }
            Shape::Tee { h, b, tw, tf } => pos(&[*h, *b, *tw, *tf]) && tw < b && tf < h,
            Shape::Angle { h, b, t } => pos(&[*h, *b, *t]) && t < h && t < b,
            Shape::Pipe { d, t } => pos(&[*d, *t]) && 2.0 * t < *d,
            Shape::Tube { h, b, t } => pos(&[*h, *b, *t]) && 2.0 * t < *h && 2.0 * t < *b,
            Shape::Poly(p) => {
                p.len() >= 3
                    && p.iter().all(|q| q.0.is_finite() && q.1.is_finite())
                    && crate::profile::Profile::new(
                        p.iter().map(|q| v2(q.0, q.1)).collect(),
                        vec![],
                        crate::Tol::default(),
                    )
                    .is_ok()
            }
        };
        if ok {
            Ok(())
        } else {
            Err(format!("bad section {self:?}"))
        }
    }

    /// Loops: outer (counter-clockwise), hole (clockwise, with the same vertex count).
    pub fn rings(&self) -> (Vec<V2>, Option<Vec<V2>>) {
        match *self {
            Shape::Rect { b, h } => (rect(-b / 2.0, -h / 2.0, b / 2.0, h / 2.0), None),
            Shape::Circle { d } => (circle(d / 2.0, CIRCLE_SEGS), None),
            Shape::I { h, b, tw, tf } => {
                let (y, z, w) = (b / 2.0, h / 2.0, tw / 2.0);
                let zi = z - tf;
                (
                    vec![
                        v2(-y, -z),
                        v2(y, -z),
                        v2(y, -zi),
                        v2(w, -zi),
                        v2(w, zi),
                        v2(y, zi),
                        v2(y, z),
                        v2(-y, z),
                        v2(-y, zi),
                        v2(-w, zi),
                        v2(-w, -zi),
                        v2(-y, -zi),
                    ],
                    None,
                )
            }
            Shape::Channel { h, b, tw, tf } => {
                let (y, z) = (b / 2.0, h / 2.0);
                let (yw, zi) = (-y + tw, z - tf);
                (
                    vec![
                        v2(-y, -z),
                        v2(y, -z),
                        v2(y, -zi),
                        v2(yw, -zi),
                        v2(yw, zi),
                        v2(y, zi),
                        v2(y, z),
                        v2(-y, z),
                    ],
                    None,
                )
            }
            Shape::Angle { h, b, t } => {
                let (y, z) = (b / 2.0, h / 2.0);
                (
                    vec![
                        v2(-y, -z),
                        v2(y, -z),
                        v2(y, -z + t),
                        v2(-y + t, -z + t),
                        v2(-y + t, z),
                        v2(-y, z),
                    ],
                    None,
                )
            }
            Shape::Tee { h, b, tw, tf } => {
                let (y, z, w) = (b / 2.0, h / 2.0, tw / 2.0);
                (
                    vec![
                        v2(-w, -z),
                        v2(w, -z),
                        v2(w, z - tf),
                        v2(y, z - tf),
                        v2(y, z),
                        v2(-y, z),
                        v2(-y, z - tf),
                        v2(-w, z - tf),
                    ],
                    None,
                )
            }
            Shape::Pipe { d, t } => {
                let mut i = circle(d / 2.0 - t, CIRCLE_SEGS);
                i.reverse();
                (circle(d / 2.0, CIRCLE_SEGS), Some(i))
            }
            Shape::Tube { h, b, t } => {
                let (y, z) = (b / 2.0, h / 2.0);
                let mut i = rect(-y + t, -z + t, y - t, z - t);
                i.reverse();
                (rect(-y, -z, y, z), Some(i))
            }
            Shape::Poly(ref p) => {
                let mut r: Vec<V2> = p.iter().map(|q| v2(q.0, q.1)).collect();
                if poly::area(&r) < 0.0 {
                    r.reverse();
                }
                // bounding-box centre at the origin
                let (y0, y1, z0, z1) = bounds(&r);
                let c = v2((y0 + y1) / 2.0, (z0 + z1) / 2.0);
                (r.into_iter().map(|q| q - c).collect(), None)
            }
        }
    }

    fn round(&self) -> bool {
        matches!(self, Shape::Circle { .. } | Shape::Pipe { .. })
    }

    /// Exact section area.
    pub fn area(&self) -> f64 {
        match *self {
            Shape::Circle { d } => PI * d * d / 4.0,
            Shape::Pipe { d, t } => PI * (d * d - (d - 2.0 * t).powi(2)) / 4.0,
            _ => {
                let (o, h) = self.rings();
                poly::area(&o) + h.map_or(0.0, |h| poly::area(&h))
            }
        }
    }

    /// Exact perimeter (outer + inner).
    pub fn perimeter(&self) -> f64 {
        match *self {
            Shape::Circle { d } => PI * d,
            Shape::Pipe { d, t } => PI * (2.0 * d - 2.0 * t),
            _ => {
                let (o, h) = self.rings();
                poly::perimeter(&o) + h.map_or(0.0, |h| poly::perimeter(&h))
            }
        }
    }

    /// Section centroid (from the bounding-box centre).
    pub fn centroid(&self) -> V2 {
        if self.round() {
            return v2(0.0, 0.0);
        }
        let (o, h) = self.rings();
        let (ao, co) = (poly::area(&o), poly::centroid(&o));
        match h {
            Some(h) => {
                let (ah, ch) = (poly::area(&h), poly::centroid(&h));
                (co * ao + ch * ah) * (1.0 / (ao + ah))
            }
            None => co,
        }
    }

    /// Bounding box (width along y, height along z).
    pub fn size(&self) -> (f64, f64) {
        let (y0, y1, z0, z1) = bounds(&self.rings().0);
        (y1 - y0, z1 - z0)
    }

    /// Web and flange thickness (for coping): (tw, tf); None if not a rolled I-beam/channel/tee.
    pub fn web_flange(&self) -> Option<(f64, f64)> {
        match *self {
            Shape::I { tw, tf, .. } | Shape::Channel { tw, tf, .. } | Shape::Tee { tw, tf, .. } => {
                Some((tw, tf))
            }
            _ => None,
        }
    }

    /// Section text in a file: `rect b h` · `circ d` · `i h b tw tf` · `c …` · `l h b t` ·
    /// `t h b tw tf` · `pipe d t` · `tube h b t` · `poly y z;y z;…` (metres).
    pub fn text(&self) -> String {
        let j = |v: &[f64]| v.iter().map(|x| fmt(*x)).collect::<Vec<_>>().join(" ");
        match self {
            Shape::Rect { b, h } => format!("rect {}", j(&[*b, *h])),
            Shape::Circle { d } => format!("circ {}", j(&[*d])),
            Shape::I { h, b, tw, tf } => format!("i {}", j(&[*h, *b, *tw, *tf])),
            Shape::Channel { h, b, tw, tf } => format!("c {}", j(&[*h, *b, *tw, *tf])),
            Shape::Angle { h, b, t } => format!("l {}", j(&[*h, *b, *t])),
            Shape::Tee { h, b, tw, tf } => format!("t {}", j(&[*h, *b, *tw, *tf])),
            Shape::Pipe { d, t } => format!("pipe {}", j(&[*d, *t])),
            Shape::Tube { h, b, t } => format!("tube {}", j(&[*h, *b, *t])),
            Shape::Poly(p) => format!(
                "poly {}",
                p.iter()
                    .map(|q| format!("{} {}", fmt(q.0), fmt(q.1)))
                    .collect::<Vec<_>>()
                    .join(";")
            ),
        }
    }

    pub fn parse(s: &str) -> Option<Shape> {
        let (k, rest) = s.split_once(' ')?;
        if k == "poly" {
            let p = rest
                .split(';')
                .map(|q| {
                    let mut it = q.split(' ');
                    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
                })
                .collect::<Option<Vec<(f64, f64)>>>()?;
            return Some(Shape::Poly(p));
        }
        let v: Vec<f64> = rest
            .split(' ')
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?;
        let s = match (k, v.as_slice()) {
            ("rect", &[b, h]) => Shape::Rect { b, h },
            ("circ", &[d]) => Shape::Circle { d },
            ("i", &[h, b, tw, tf]) => Shape::I { h, b, tw, tf },
            ("c", &[h, b, tw, tf]) => Shape::Channel { h, b, tw, tf },
            ("l", &[h, b, t]) => Shape::Angle { h, b, t },
            ("t", &[h, b, tw, tf]) => Shape::Tee { h, b, tw, tf },
            ("pipe", &[d, t]) => Shape::Pipe { d, t },
            ("tube", &[h, b, t]) => Shape::Tube { h, b, t },
            _ => return None,
        };
        Some(s)
    }
}

fn fmt(v: f64) -> String {
    let x = (v * 1e9).round() / 1e9;
    format!("{}", if x == 0.0 { 0.0 } else { x })
}

fn bounds(r: &[V2]) -> (f64, f64, f64, f64) {
    r.iter().fold(
        (f64::MAX, f64::MIN, f64::MAX, f64::MIN),
        |(a, b, c, d), p| (a.min(p.x), b.max(p.x), c.min(p.y), d.max(p.y)),
    )
}

/// Catalogue of standard sizes: (name, section), metres. Metric ones in mm, imperial ones in inches.
pub fn catalog() -> Vec<(&'static str, Shape)> {
    let mm = |v: f64| v / 1000.0;
    let i = |n, h, b, tw, tf, k: &dyn Fn(f64) -> f64| {
        (
            n,
            Shape::I {
                h: k(h),
                b: k(b),
                tw: k(tw),
                tf: k(tf),
            },
        )
    };
    let mut v = vec![
        i("IPE 100", 100.0, 55.0, 4.1, 5.7, &mm),
        i("IPE 200", 200.0, 100.0, 5.6, 8.5, &mm),
        i("IPE 300", 300.0, 150.0, 7.1, 10.7, &mm),
        i("IPE 400", 400.0, 180.0, 8.6, 13.5, &mm),
        i("HEA 200", 190.0, 200.0, 6.5, 10.0, &mm),
        i("HEA 300", 290.0, 300.0, 8.5, 14.0, &mm),
        i("HEB 200", 200.0, 200.0, 9.0, 15.0, &mm),
        i("HEB 300", 300.0, 300.0, 11.0, 19.0, &mm),
        (
            "UPN 200",
            Shape::Channel {
                h: mm(200.0),
                b: mm(75.0),
                tw: mm(8.5),
                tf: mm(11.5),
            },
        ),
        (
            "L 100x100x10",
            Shape::Angle {
                h: mm(100.0),
                b: mm(100.0),
                t: mm(10.0),
            },
        ),
        (
            "L 50x50x5",
            Shape::Angle {
                h: mm(50.0),
                b: mm(50.0),
                t: mm(5.0),
            },
        ),
        (
            "T 100",
            Shape::Tee {
                h: mm(100.0),
                b: mm(100.0),
                tw: mm(11.0),
                tf: mm(11.0),
            },
        ),
        (
            "CHS 168.3x8",
            Shape::Pipe {
                d: mm(168.3),
                t: mm(8.0),
            },
        ),
        (
            "SHS 200x200x10",
            Shape::Tube {
                h: mm(200.0),
                b: mm(200.0),
                t: mm(10.0),
            },
        ),
        (
            "RHS 200x100x8",
            Shape::Tube {
                h: mm(200.0),
                b: mm(100.0),
                t: mm(8.0),
            },
        ),
        (
            "R 300x500",
            Shape::Rect {
                b: mm(300.0),
                h: mm(500.0),
            },
        ),
        (
            "R 400x400",
            Shape::Rect {
                b: mm(400.0),
                h: mm(400.0),
            },
        ),
        ("D 400", Shape::Circle { d: mm(400.0) }),
    ];
    v.extend(inch_sections());
    let have: std::collections::HashSet<&str> = v.iter().map(|c| c.0).collect();
    let rolled = crate::steel::rolled::table().iter().filter(|r| !have.contains(r.name.as_str()));
    let rolled: Vec<_> = rolled.map(|r| (r.name.as_str(), r.shape.clone())).collect();
    v.extend(rolled);
    v
}

/// Inch-series rows of the catalogue, from `data/steel/sections_inch.csv` (provenance there).
fn inch_sections() -> Vec<(&'static str, Shape)> {
    let inch = |v: f64| v * 0.0254;
    include_str!("../data/steel/sections_inch.csv")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && *l != "name,shape")
        .map(|l| {
            let (name, spec) = l.split_once(',').expect("sections_inch.csv: name,shape");
            let mut it = spec.split(' ');
            let kind = it.next().unwrap_or("");
            let v: Vec<f64> = it
                .map(|x| inch(x.parse().expect("sections_inch.csv: number")))
                .collect();
            let s = match (kind, v.as_slice()) {
                ("i", &[h, b, tw, tf]) => Shape::I { h, b, tw, tf },
                ("c", &[h, b, tw, tf]) => Shape::Channel { h, b, tw, tf },
                ("l", &[h, b, t]) => Shape::Angle { h, b, t },
                ("tube", &[h, b, t]) => Shape::Tube { h, b, t },
                ("pipe", &[d, t]) => Shape::Pipe { d, t },
                _ => panic!("sections_inch.csv: bad shape in {l}"),
            };
            (name, s)
        })
        .collect()
}

/// Justification of the section to the reference line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Just {
    /// Line at the −y face (z: bottom).
    Neg,
    #[default]
    Center,
    /// Line at the +y face (z: top).
    Pos,
}

impl Just {
    pub fn from_i(v: i64) -> Just {
        match v.signum() {
            -1 => Just::Neg,
            1 => Just::Pos,
            _ => Just::Center,
        }
    }
    pub fn to_i(self) -> i64 {
        match self {
            Just::Neg => -1,
            Just::Center => 0,
            Just::Pos => 1,
        }
    }
    fn shift(self, size: f64) -> f64 {
        match self {
            Just::Neg => size / 2.0,
            Just::Center => 0.0,
            Just::Pos => -size / 2.0,
        }
    }
}

/// End plane: a point and a normal (pointing out of the body along the axis).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cap {
    pub p: V3,
    pub n: V3,
}

/// Member path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Path {
    Line {
        a: V3,
        b: V3,
    },
    /// Horizontal arc a→b, bulge = tan(angle/4) (>0 means counter-clockwise).
    Arc {
        a: V3,
        b: V3,
        bulge: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub shape: Shape,
    pub path: Path,
    /// Section rotation about the axis, rad.
    pub rotation: f64,
    pub yjust: Just,
    pub zjust: Just,
    /// Column: the axis is vertical or inclined, ends are horizontal, the section is ⟂ to the axis.
    pub column: bool,
    /// End cuts (straight members only): None means perpendicular to the axis (a column: horizontal).
    pub start: Option<Cap>,
    pub end: Option<Cap>,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MemberQty {
    pub area: f64,
    /// Length of the centroid line between the ends.
    pub length: f64,
    pub volume: f64,
    pub surface: f64,
}

/// Section basis on a straight axis: (u, ey, ez), including rotation.
fn frame(u: V3, rotation: f64) -> (V3, V3) {
    let z = v3(0.0, 0.0, 1.0);
    let h = z.cross(u);
    let ey = if h.len() < 1e-9 {
        // vertical axis: profile y maps to plan x
        v3(1.0, 0.0, 0.0)
    } else {
        h.normalized()
    };
    let ez = u.cross(ey);
    let (c, s) = (rotation.cos(), rotation.sin());
    (ey * c + ez * s, ez * c - ey * s)
}

fn plane_hit(q: V3, u: V3, cap: &Cap) -> f64 {
    cap.n.dot(cap.p - q) / cap.n.dot(u)
}

impl Member {
    fn offset(&self) -> V2 {
        let (w, h) = self.shape.size();
        v2(self.yjust.shift(w), self.zjust.shift(h))
    }

    /// Start, end, unit axis (straight members).
    fn line(&self) -> Option<(V3, V3, V3)> {
        match self.path {
            Path::Line { a, b } => {
                let d = b - a;
                (d.len() > 1e-12).then(|| (a, b, d.normalized()))
            }
            Path::Arc { .. } => None,
        }
    }

    /// Ends of a straight member (default: horizontal for a column, ⟂ to the axis for a beam).
    fn caps(&self) -> Option<(Cap, Cap, V3, V3, V3)> {
        let (a, b, u) = self.line()?;
        let n0 = if self.column { v3(0.0, 0.0, -1.0) } else { -u };
        let s = self.start.unwrap_or(Cap { p: a, n: n0 });
        let e = self.end.unwrap_or(Cap { p: b, n: -n0 });
        let (ey, ez) = frame(u, self.rotation);
        Some((s, e, u, ey, ez))
    }

    /// Section point (y, z) in space at reference point `o`.
    fn at(o: V3, ey: V3, ez: V3, off: V2, p: V2) -> V3 {
        o + ey * (p.x + off.x) + ez * (p.y + off.y)
    }

    /// Arc: centre, radius, start angle, sweep angle (signed).
    fn arc(&self) -> Option<(V2, f64, f64, f64, f64)> {
        let Path::Arc { a, b, bulge } = self.path else {
            return None;
        };
        let (pa, pb) = (v2(a.x, a.y), v2(b.x, b.y));
        let theta = 4.0 * bulge.atan();
        let c = (pb - pa).len();
        let r = c / (2.0 * (theta / 2.0).sin()).abs();
        let mid = (pa + pb) * 0.5;
        let d = (pb - pa) * (1.0 / c);
        let nrm = v2(-d.y, d.x);
        let h = r * (theta / 2.0).cos().abs() * if theta.abs() > PI { -1.0 } else { 1.0 };
        let center = mid + nrm * (h * theta.signum());
        let a0 = (pa.y - center.y).atan2(pa.x - center.x);
        Some((center, r, a0, theta, a.z))
    }

    pub fn quantities(&self) -> MemberQty {
        let sh = &self.shape;
        let (area, per) = (sh.area(), sh.perimeter());
        let off = self.offset();
        let c = sh.centroid() + off;
        if let Some((center, r, _, theta, _)) = self.arc() {
            let _ = center;
            // ey points left along the run: towards the centre when moving counter-clockwise
            let rad = |y: f64| r - theta.signum() * y;
            let lc = theta.abs() * rad(c.x);
            let lateral = if sh.round() {
                per * lc
            } else {
                let (o, h) = sh.rings();
                let mut s = 0.0;
                for ring in std::iter::once(&o).chain(h.iter()) {
                    for i in 0..ring.len() {
                        let (p, q) = (ring[i] + off, ring[(i + 1) % ring.len()] + off);
                        s += (q - p).len() * theta.abs() * rad((p.x + q.x) / 2.0);
                    }
                }
                s
            };
            return MemberQty {
                area,
                length: lc,
                volume: area * lc,
                surface: lateral + 2.0 * area,
            };
        }
        let Some((s, e, u, ey, ez)) = self.caps() else {
            return MemberQty::default();
        };
        let (a, _, _) = self.line().unwrap();
        let len_at = |p: V2| {
            let q = Self::at(a, ey, ez, v2(0.0, 0.0), p);
            plane_hit(q, u, &e) - plane_hit(q, u, &s)
        };
        let lc = len_at(c);
        let lateral = if sh.round() {
            per * lc
        } else {
            let (o, h) = sh.rings();
            let mut acc = 0.0;
            for ring in std::iter::once(&o).chain(h.iter()) {
                for i in 0..ring.len() {
                    let (p, q) = (ring[i] + off, ring[(i + 1) % ring.len()] + off);
                    acc += (q - p).len() * len_at((p + q) * 0.5);
                }
            }
            acc
        };
        let caps = area / s.n.normalized().dot(u).abs() + area / e.n.normalized().dot(u).abs();
        MemberQty {
            area,
            length: lc,
            volume: area * lc,
            surface: lateral + caps,
        }
    }

    /// Stations: for each, the points of all loop vertices (outer, then hole).
    fn stations(&self) -> Vec<Vec<V3>> {
        let (o, h) = self.shape.rings();
        let pts: Vec<V2> = o.iter().chain(h.iter().flatten()).copied().collect();
        let off = self.offset();
        if let Some((center, r, a0, theta, z)) = self.arc() {
            let n = ((theta.abs() / (PI / 36.0)).ceil() as usize).max(4);
            return (0..=n)
                .map(|k| {
                    let ang = a0 + theta * k as f64 / n as f64;
                    let rad = v2(ang.cos(), ang.sin());
                    let o3 = v3(center.x + r * rad.x, center.y + r * rad.y, z);
                    let tangent = v3(-rad.y, rad.x, 0.0) * theta.signum();
                    let (ey, ez) = frame(tangent, self.rotation);
                    pts.iter().map(|&p| Self::at(o3, ey, ez, off, p)).collect()
                })
                .collect();
        }
        let Some((s, e, u, ey, ez)) = self.caps() else {
            return vec![];
        };
        let (a, _, _) = self.line().unwrap();
        let mk = |cap: &Cap| -> Vec<V3> {
            pts.iter()
                .map(|&p| {
                    let q = Self::at(a, ey, ez, off, p);
                    q + u * plane_hit(q, u, cap)
                })
                .collect()
        };
        vec![mk(&s), mk(&e)]
    }

    /// Closed body mesh (a circle is a 32-gon).
    pub fn mesh(&self) -> Mesh {
        let st = self.stations();
        let (o, h) = self.shape.rings();
        let (no, nh) = (o.len(), h.as_ref().map_or(0, |h| h.len()));
        let per = no + nh;
        let mut m = Mesh::default();
        if st.len() < 2 {
            return m;
        }
        for s in &st {
            m.positions.extend_from_slice(s);
        }
        let id = |k: usize, i: usize| (k * per + i) as u32;
        let rings: Vec<(usize, usize)> = if nh > 0 {
            vec![(0, no), (no, nh)]
        } else {
            vec![(0, no)]
        };
        let mut tris: Vec<[u32; 3]> = vec![];
        for k in 0..st.len() - 1 {
            for &(b, n) in &rings {
                for i in 0..n {
                    let (i0, i1) = (b + i, b + (i + 1) % n);
                    tris.push([id(k, i0), id(k, i1), id(k + 1, i1)]);
                    tris.push([id(k, i0), id(k + 1, i1), id(k + 1, i0)]);
                }
            }
        }
        let last = st.len() - 1;
        if nh > 0 {
            // ring: outer i ↔ inner (reversed), same angles
            for i in 0..no {
                let (a0, a1) = (i, (i + 1) % no);
                let (b0, b1) = (no + (no - 1 - a0), no + (no - 1 - a1));
                for (k, flip) in [(0usize, true), (last, false)] {
                    let t1 = [id(k, a0), id(k, a1), id(k, b1)];
                    let t2 = [id(k, a0), id(k, b1), id(k, b0)];
                    for t in [t1, t2] {
                        tris.push(if flip { [t[0], t[2], t[1]] } else { t });
                    }
                }
            }
        } else {
            for t in poly::triangulate(&o) {
                tris.push([id(0, t[0]), id(0, t[2]), id(0, t[1])]);
                tris.push([id(last, t[0]), id(last, t[1]), id(last, t[2])]);
            }
        }
        m.face_of = vec![0; tris.len()];
        m.triangles = tris;
        if m.volume() < 0.0 {
            for t in &mut m.triangles {
                t.swap(1, 2);
            }
        }
        m
    }

    /// Section area within the world-z band [z0, z1] (horizontal beam without rotation);
    /// otherwise None. Round sections use the polygon corrected to the exact area.
    pub fn band_area(&self, z0: f64, z1: f64) -> Option<f64> {
        let (a, b) = match self.path {
            Path::Line { a, b } | Path::Arc { a, b, .. } => (a, b),
        };
        if self.column || (a.z - b.z).abs() > 1e-9 || self.rotation.abs() > 1e-12 {
            return None;
        }
        let off = self.offset();
        let (o, h) = self.shape.rings();
        let zr = a.z + off.y;
        let band = |r: &[V2]| {
            let r: Vec<V2> = r.iter().map(|p| v2(p.x, p.y + zr)).collect();
            let r = poly::clip_half(&r, v2(0.0, -1.0), -z0);
            poly::area(&poly::clip_half(&r, v2(0.0, 1.0), z1))
        };
        let poly_area = poly::area(&o) + h.as_ref().map_or(0.0, |h| poly::area(h));
        let a_band = band(&o) + h.as_ref().map_or(0.0, |h| band(h));
        Some(a_band * self.shape.area() / poly_area)
    }

    /// Section bounding box with the justification offset: (y0, z0), (y1, z1) from the reference line.
    pub fn extents(&self) -> (V2, V2) {
        let (w, h) = self.shape.size();
        let o = self.offset();
        (
            v2(o.x - w / 2.0, o.y - h / 2.0),
            v2(o.x + w / 2.0, o.y + h / 2.0),
        )
    }

    /// Reference line between the end planes (straight members).
    pub fn axis_ends(&self) -> Option<(V3, V3)> {
        let (s, e, u, _, _) = self.caps()?;
        let (a, _, _) = self.line()?;
        Some((a + u * plane_hit(a, u, &s), a + u * plane_hit(a, u, &e)))
    }

    /// Direction of the straight axis.
    pub fn axis_dir(&self) -> Option<V3> {
        self.line().map(|l| l.2)
    }

    /// Section basis of a straight member: the centre of the section box at the path start
    /// (justification applied), the axis `u` and the section axes `ey`, `ez` (rotation applied).
    pub fn section_basis(&self) -> Option<(V3, V3, V3, V3)> {
        let (_, _, u, ey, ez) = self.caps()?;
        let (a, _, _) = self.line()?;
        let off = self.offset();
        Some((a + ey * off.x + ez * off.y, u, ey, ez))
    }

    /// End planes of a straight member (start, end), outward normals.
    pub fn end_caps(&self) -> Option<(Cap, Cap)> {
        self.caps().map(|(s, e, ..)| (s, e))
    }

    /// z extent of the body (for height intersections).
    pub fn z_range(&self) -> (f64, f64) {
        let m = self.mesh();
        m.positions
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.z), b.max(p.z)))
    }

    /// Horizontal section of a column at height z (plan polygon); None if not a column or out of range.
    pub fn plan_section(&self, z: f64) -> Option<Vec<V2>> {
        let (a, b, _) = self.line()?;
        if !self.column || z < a.z.min(b.z) || z > a.z.max(b.z) {
            return None;
        }
        let segs: Vec<(V2, V2)> = poly::slice(&self.mesh(), v3(0.0, 0.0, 1.0), z)
            .into_iter()
            .map(|(p, q)| (v2(p.x, p.y), v2(q.x, q.y)))
            .collect();
        let mut rings = poly::chain(&segs, 1e-9);
        rings.sort_by(|x, y| poly::area(y).abs().total_cmp(&poly::area(x).abs()));
        rings.into_iter().next().map(|mut r| {
            if poly::area(&r) < 0.0 {
                r.reverse();
            }
            r
        })
    }
}

/// Mitre of two straight members at a common point `p`: a plane whose normal follows the plan bisector.
/// `u_in` is the direction of the first towards the joint, `u_out` of the second away from it. Returns the end for both.
pub fn miter(p: V3, u_in: V3, u_out: V3) -> Cap {
    let n = u_in + u_out;
    let n = if n.len() < 1e-9 { u_in } else { n.normalized() };
    Cap { p, n }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beam(shape: Shape, a: V3, b: V3) -> Member {
        Member {
            shape,
            path: Path::Line { a, b },
            rotation: 0.0,
            yjust: Just::Center,
            zjust: Just::Center,
            column: false,
            start: None,
            end: None,
        }
    }

    #[test]
    fn ipe300_beam_6m_exact() {
        let s = catalog().into_iter().find(|c| c.0 == "IPE 300").unwrap().1;
        // 2·0.15·0.0107 + (0.3 − 2·0.0107)·0.0071 = 0.00321 + 0.00197806
        let a = 2.0 * 0.15 * 0.0107 + (0.3 - 2.0 * 0.0107) * 0.0071;
        assert!((s.area() - a).abs() < 1e-15);
        assert!((s.area() - 0.00518806).abs() < 1e-12);
        let m = beam(s.clone(), v3(0.0, 0.0, 3.0), v3(6.0, 0.0, 3.0));
        let q = m.quantities();
        assert!((q.volume - 0.03112836).abs() < 1e-12, "{}", q.volume);
        assert!((q.length - 6.0).abs() < 1e-12);
        let mesh = m.mesh();
        assert!(mesh.is_watertight());
        assert!((mesh.volume() - q.volume).abs() < 1e-12);
        assert!((mesh.area() - q.surface).abs() < 1e-10);
        // I-beam perimeter: 2b + 2h + 2(b − tw) ... = 4b + 2h − 2tw
        assert!((s.perimeter() - (4.0 * 0.15 + 2.0 * 0.3 - 2.0 * 0.0071)).abs() < 1e-12);
    }

    #[test]
    fn profiles_area_and_parse_roundtrip() {
        for (n, s) in catalog() {
            s.check().unwrap_or_else(|e| panic!("{n}: {e}"));
            assert_eq!(
                Shape::parse(&s.text()).map(|p| p.text()),
                Some(s.text()),
                "{n}"
            );
            let m = beam(s.clone(), v3(0.0, 0.0, 0.0), v3(0.0, 2.0, 0.0));
            let mesh = m.mesh();
            assert!(mesh.is_watertight(), "{n}");
            let exact = s.area() * 2.0;
            let tol = if s.round() { 0.01 * exact } else { 1e-12 };
            assert!(
                (mesh.volume() - exact).abs() < tol,
                "{n} {} {}",
                mesh.volume(),
                exact
            );
        }
        let pipe = Shape::Pipe { d: 0.2, t: 0.01 };
        assert!((pipe.area() - PI * (0.04 - 0.0324) / 4.0).abs() < 1e-15);
    }

    #[test]
    fn slanted_column_volume_and_horizontal_caps() {
        let c = Member {
            column: true,
            ..beam(
                Shape::Rect { b: 0.4, h: 0.4 },
                v3(0.0, 0.0, 0.0),
                v3(3.0, 0.0, 4.0),
            )
        };
        let q = c.quantities();
        // A·L = 0.16·5; ends are horizontal: 2·A/cos φ = 2·0.16·5/4
        assert!((q.volume - 0.8).abs() < 1e-12);
        let mesh = c.mesh();
        assert!((mesh.volume() - 0.8).abs() < 1e-12);
        assert!((mesh.area() - q.surface).abs() < 1e-10);
        assert!((q.surface - (1.6 * 5.0 + 0.4)).abs() < 1e-12);
        let sec = c.plan_section(2.0).unwrap();
        // horizontal section: 0.4 × 0.4/cos φ
        assert!((poly::area(&sec) - 0.16 * 5.0 / 4.0).abs() < 1e-12);
    }

    #[test]
    fn miter_corner_keeps_centerline_volume() {
        let s = Shape::Rect { b: 0.2, h: 0.4 };
        let p = v3(4.0, 0.0, 0.0);
        let cap = miter(p, v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        let mut a = beam(s.clone(), v3(0.0, 0.0, 0.0), p);
        a.end = Some(cap);
        let mut b = beam(s, p, v3(4.0, 3.0, 0.0));
        b.start = Some(cap);
        let (qa, qb) = (a.quantities(), b.quantities());
        assert!((qa.volume - 0.08 * 4.0).abs() < 1e-12);
        assert!((qb.volume - 0.08 * 3.0).abs() < 1e-12);
        assert!((a.mesh().volume() - qa.volume).abs() < 1e-12);
        assert!((a.mesh().area() - qa.surface).abs() < 1e-10);
    }

    #[test]
    fn arc_beam_pappus() {
        // semicircle r = 5 (bulge = 1), rectangle 0.2×0.4 centred
        let m = Member {
            path: Path::Arc {
                a: v3(5.0, 0.0, 3.0),
                b: v3(-5.0, 0.0, 3.0),
                bulge: 1.0,
            },
            ..beam(
                Shape::Rect { b: 0.2, h: 0.4 },
                v3(0.0, 0.0, 0.0),
                v3(1.0, 0.0, 0.0),
            )
        };
        let q = m.quantities();
        assert!((q.volume - 0.08 * PI * 5.0).abs() < 1e-12);
        let mut j = m.clone();
        j.yjust = Just::Pos; // line at the left (inner) face: centroid at r + 0.1
        assert!((j.quantities().volume - 0.08 * PI * 5.1).abs() < 1e-12);
        let mesh = m.mesh();
        assert!(mesh.is_watertight());
        assert!((mesh.volume() - q.volume).abs() < 0.002 * q.volume);
    }

    #[test]
    fn band_area_of_beam_in_slab() {
        let m = Member {
            zjust: Just::Pos,
            ..beam(
                Shape::Rect { b: 0.3, h: 0.5 },
                v3(0.0, 0.0, 3.0),
                v3(5.0, 0.0, 3.0),
            )
        };
        // beam top at 3.0; slab 2.8…3.0
        assert!((m.band_area(2.8, 3.0).unwrap() - 0.06).abs() < 1e-12);
    }

    /// Budget: 10k members (body + quantities) in under 1 s in release.
    #[test]
    #[ignore]
    fn bench_10k_columns_and_beams() {
        let heb = catalog().into_iter().find(|c| c.0 == "HEB 200").unwrap().1;
        let ipe = catalog().into_iter().find(|c| c.0 == "IPE 300").unwrap().1;
        let t0 = web_time::Instant::now();
        let (mut v, mut tris) = (0.0, 0usize);
        for i in 0..5000 {
            let (x, y) = ((i % 100) as f64 * 6.0, (i / 100) as f64 * 6.0);
            let c = Member {
                column: true,
                ..beam(heb.clone(), v3(x, y, 0.0), v3(x, y, 3.0))
            };
            let mut b = beam(ipe.clone(), v3(x, y, 3.0), v3(x + 6.0, y, 3.0));
            b.end = Some(miter(
                v3(x + 6.0, y, 3.0),
                v3(1.0, 0.0, 0.0),
                v3(0.0, 1.0, 0.0),
            ));
            for m in [c, b] {
                v += m.quantities().volume;
                tris += m.mesh().triangles.len();
            }
        }
        let dt = t0.elapsed();
        eprintln!("10k members: {dt:?}, {tris} triangles, volume {v:.3}");
        assert!(dt.as_secs_f64() < 1.0, "{dt:?}");
    }
}
