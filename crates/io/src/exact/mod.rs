// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Exact boundary representation for exchange (docs/IO4.md): analytic and NURBS faces bounded
//! by loops of exact edges, and an assembly of placed parts. Metres; placements right-handed.

pub mod build;
pub mod measure;
pub mod model;
pub mod nurbs_face;
pub mod recog;
pub mod sweep;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
pub(crate) mod sweep_tests;

use std::collections::BTreeMap;

use ddc_geom::math::{v3, V3};
use ddc_geom::nurbs;

pub use measure::volume;
pub use recog::{recognize, Mapped};

/// Placement: origin, axis `z`, reference direction `x` (unit, orthogonal); `y = z x x`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ax3 {
    pub o: V3,
    pub z: V3,
    pub x: V3,
}

impl Ax3 {
    pub const WORLD: Ax3 = Ax3 {
        o: v3(0.0, 0.0, 0.0),
        z: v3(0.0, 0.0, 1.0),
        x: v3(1.0, 0.0, 0.0),
    };

    /// Placement on axis `z`; `x_hint` is made orthogonal to it (any perpendicular if parallel).
    pub fn new(o: V3, z: V3, x_hint: V3) -> Ax3 {
        let z = z.normalized();
        let x = x_hint - z * x_hint.dot(z);
        let x = if x.len() > 1e-9 * (1.0 + x_hint.len()) {
            x.normalized()
        } else {
            perp(z)
        };
        Ax3 { o, z, x }
    }
    pub fn y(&self) -> V3 {
        self.z.cross(self.x)
    }
    pub fn at(&self, a: f64, b: f64, c: f64) -> V3 {
        self.o + self.x * a + self.y() * b + self.z * c
    }
    pub fn local(&self, p: V3) -> V3 {
        let d = p - self.o;
        v3(d.dot(self.x), d.dot(self.y()), d.dot(self.z))
    }
}

/// A unit vector perpendicular to the unit vector `z`.
pub fn perp(z: V3) -> V3 {
    let a = if z.x.abs() < 0.9 { v3(1.0, 0.0, 0.0) } else { v3(0.0, 1.0, 0.0) };
    (a - z * a.dot(z)).normalized()
}

/// Affine map `p -> m p + t`; parts are placed by rigid maps, units may add a uniform scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf {
    /// Rows.
    pub m: [[f64; 3]; 3],
    pub t: V3,
}

impl Xf {
    pub const ID: Xf = Xf {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: v3(0.0, 0.0, 0.0),
    };

    /// Local coordinates of `a` to the containing ones.
    pub fn from_ax(a: &Ax3) -> Xf {
        let (x, y, z) = (a.x, a.y(), a.z);
        Xf {
            m: [[x.x, y.x, z.x], [x.y, y.y, z.y], [x.z, y.z, z.z]],
            t: a.o,
        }
    }
    pub fn scale(s: f64) -> Xf {
        Xf {
            m: [[s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]],
            t: v3(0.0, 0.0, 0.0),
        }
    }
    pub fn lin(&self, d: V3) -> V3 {
        let r = |k: usize| self.m[k][0] * d.x + self.m[k][1] * d.y + self.m[k][2] * d.z;
        v3(r(0), r(1), r(2))
    }
    pub fn apply(&self, p: V3) -> V3 {
        self.lin(p) + self.t
    }
    /// `self` applied after `b`.
    pub fn after(&self, b: &Xf) -> Xf {
        let mut m = [[0.0; 3]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[r][k] * b.m[k][c]).sum();
            }
        }
        Xf {
            m,
            t: self.lin(b.t) + self.t,
        }
    }
    pub fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
    /// Uniform scale of a similarity map, `None` if the map shears or scales unevenly.
    pub fn similarity(&self) -> Option<f64> {
        let cols = [0, 1, 2].map(|c| v3(self.m[0][c], self.m[1][c], self.m[2][c]));
        let s = cols[0].len();
        let tol = 1e-9 * s.max(1.0);
        let ok = s > 1e-300
            && cols.iter().all(|c| (c.len() - s).abs() <= tol)
            && cols[0].dot(cols[1]).abs() <= tol * s
            && cols[0].dot(cols[2]).abs() <= tol * s
            && cols[1].dot(cols[2]).abs() <= tol * s;
        ok.then_some(s)
    }
    /// The rigid placement this map is, if it is one.
    pub fn to_ax(&self) -> Option<Ax3> {
        let s = self.similarity()?;
        if (s - 1.0).abs() > 1e-9 || self.det() < 0.0 {
            return None;
        }
        Some(Ax3 {
            o: self.t,
            z: self.lin(v3(0.0, 0.0, 1.0)).normalized(),
            x: self.lin(v3(1.0, 0.0, 0.0)).normalized(),
        })
    }
}

/// Edge geometry. A line runs between the edge vertices.
#[derive(Clone, Debug, PartialEq)]
pub enum Curve {
    Line,
    /// `o + r (cos t x + sin t y)`.
    Circle { ax: Ax3, r: f64 },
    /// `o + a cos t x + b sin t y`.
    Ellipse { ax: Ax3, a: f64, b: f64 },
    Nurbs(nurbs::Curve),
}

/// Face geometry with the parameterisation of ISO 10303-42: the natural normal of the analytic
/// surfaces points away from the axis or centre, of a plane along `z`.
#[derive(Clone, Debug, PartialEq)]
pub enum Surface {
    Plane(Ax3),
    Cylinder { ax: Ax3, r: f64 },
    /// Radius `r` in the plane of `ax`, growing along `z` by `tan(semi)`.
    Cone { ax: Ax3, r: f64, semi: f64 },
    Sphere { ax: Ax3, r: f64 },
    Torus { ax: Ax3, major: f64, minor: f64 },
    Nurbs(nurbs::Surface),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Nurbs,
}

impl FaceKind {
    pub fn name(self) -> &'static str {
        match self {
            FaceKind::Plane => "plane",
            FaceKind::Cylinder => "cylinder",
            FaceKind::Cone => "cone",
            FaceKind::Sphere => "sphere",
            FaceKind::Torus => "torus",
            FaceKind::Nurbs => "nurbs",
        }
    }
}

impl Surface {
    pub fn kind(&self) -> FaceKind {
        match self {
            Surface::Plane(_) => FaceKind::Plane,
            Surface::Cylinder { .. } => FaceKind::Cylinder,
            Surface::Cone { .. } => FaceKind::Cone,
            Surface::Sphere { .. } => FaceKind::Sphere,
            Surface::Torus { .. } => FaceKind::Torus,
            Surface::Nurbs(_) => FaceKind::Nurbs,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// Start and end vertex; equal for a closed curve.
    pub v: [u32; 2],
    pub curve: Curve,
    /// The edge runs with the curve parameter.
    pub sense: bool,
}

impl Edge {
    /// A degenerate edge: one vertex and a NURBS curve collapsed to it, the pole side of a
    /// patch whose control row meets in a point. It bounds one face and is used once.
    pub fn degenerate(&self) -> bool {
        let Curve::Nurbs(c) = &self.curve else { return false };
        let p0 = c.ctrl()[0];
        self.v[0] == self.v[1] && c.ctrl().iter().all(|p| (*p - p0).len() <= 1e-6 * (1.0 + p0.len()))
    }
}

/// The first edge whose uses (forward, reverse) do not close a solid: once each way, or once
/// in all for a degenerate edge.
pub fn open_edge(edges: &[Edge], uses: &[[u32; 2]]) -> Option<usize> {
    (0..uses.len()).find(|&e| {
        let u = uses[e];
        if edges[e].degenerate() {
            u[0] + u[1] != 1
        } else {
            u != [1, 1]
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub surface: Surface,
    /// The face normal is the natural surface normal (else its opposite); it points out of a solid.
    pub sense: bool,
    /// Loops of (edge, used start to end); the first is the outer one. Each loop runs
    /// counter-clockwise seen against the face normal (material on the left).
    pub loops: Vec<Vec<(u32, bool)>>,
    /// Face colour over the part colour (sRGB).
    pub rgb: Option<[u8; 3]>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Body {
    pub verts: Vec<V3>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    /// Face indices per shell: a closed body has its outer shell first, then voids; an open
    /// body is a set of sheets.
    pub shells: Vec<Vec<u32>>,
    pub closed: bool,
}

impl Body {
    pub fn histogram(&self) -> BTreeMap<FaceKind, usize> {
        let mut h = BTreeMap::new();
        for f in &self.faces {
            *h.entry(f.surface.kind()).or_default() += 1;
        }
        h
    }

    /// Indices in range, loops chained; a closed body uses each edge once in each direction.
    pub fn check(&self) -> Result<(), String> {
        let ends = |e: u32, fwd: bool| -> Result<[u32; 2], String> {
            let ed = self.edges.get(e as usize).ok_or_else(|| format!("edge {e} out of range"))?;
            if ed.v.iter().any(|&v| v as usize >= self.verts.len()) {
                return Err(format!("edge {e}: vertex out of range"));
            }
            Ok(if fwd { ed.v } else { [ed.v[1], ed.v[0]] })
        };
        let mut uses = vec![[0u32; 2]; self.edges.len()];
        for (fi, f) in self.faces.iter().enumerate() {
            if f.loops.is_empty() {
                return Err(format!("face {fi} has no loop"));
            }
            for l in &f.loops {
                if l.is_empty() {
                    return Err(format!("face {fi} has an empty loop"));
                }
                for (k, &(e, fwd)) in l.iter().enumerate() {
                    let a = ends(e, fwd)?;
                    let (ne, nf) = l[(k + 1) % l.len()];
                    let b = ends(ne, nf)?;
                    if a[1] != b[0] {
                        return Err(format!("face {fi}: loop broken after edge {e}"));
                    }
                    uses[e as usize][usize::from(!fwd)] += 1;
                }
            }
        }
        for s in &self.shells {
            if let Some(&f) = s.iter().find(|&&f| f as usize >= self.faces.len()) {
                return Err(format!("shell face {f} out of range"));
            }
        }
        if self.closed {
            if let Some(e) = open_edge(&self.edges, &uses) {
                return Err(format!("edge {e} used {:?} times (forward, reverse) in a closed body", uses[e]));
            }
        }
        Ok(())
    }

    /// The body under `xf` (a similarity map); a mirror reverses the loops.
    pub fn transformed(&self, xf: &Xf) -> Result<Body, String> {
        let s = xf.similarity().ok_or("the placement shears or scales unevenly")?;
        let mirror = xf.det() < 0.0;
        let dir = |d: V3| xf.lin(d).normalized();
        // a circle keeps its parameter under a mirror when its axis is negated
        let ax_keep = |a: &Ax3| Ax3 {
            o: xf.apply(a.o),
            z: if mirror { -dir(a.z) } else { dir(a.z) },
            x: dir(a.x),
        };
        let ax_set = |a: &Ax3| Ax3 {
            o: xf.apply(a.o),
            z: dir(a.z),
            x: dir(a.x),
        };
        let map_curve = |c: &nurbs::Curve| -> Result<nurbs::Curve, String> {
            nurbs::Curve::new(
                c.degree(),
                c.knots().to_vec(),
                c.ctrl().iter().map(|&p| xf.apply(p)).collect(),
                c.weights().to_vec(),
            )
            .map_err(|e| e.to_string())
        };
        let mut out = self.clone();
        out.verts = self.verts.iter().map(|&p| xf.apply(p)).collect();
        for e in &mut out.edges {
            e.curve = match &e.curve {
                Curve::Line => Curve::Line,
                Curve::Circle { ax, r } => Curve::Circle { ax: ax_keep(ax), r: r * s },
                Curve::Ellipse { ax, a, b } => Curve::Ellipse {
                    ax: ax_keep(ax),
                    a: a * s,
                    b: b * s,
                },
                Curve::Nurbs(c) => Curve::Nurbs(map_curve(c)?),
            };
        }
        for f in &mut out.faces {
            let (surface, flip) = match &f.surface {
                Surface::Plane(a) => (Surface::Plane(ax_keep(a)), mirror),
                Surface::Cylinder { ax, r } => (Surface::Cylinder { ax: ax_set(ax), r: r * s }, false),
                Surface::Cone { ax, r, semi } => (
                    Surface::Cone {
                        ax: ax_set(ax),
                        r: r * s,
                        semi: *semi,
                    },
                    false,
                ),
                Surface::Sphere { ax, r } => (Surface::Sphere { ax: ax_set(ax), r: r * s }, false),
                Surface::Torus { ax, major, minor } => (
                    Surface::Torus {
                        ax: ax_set(ax),
                        major: major * s,
                        minor: minor * s,
                    },
                    false,
                ),
                Surface::Nurbs(n) => (
                    Surface::Nurbs(
                        nurbs::Surface::new(
                            n.deg_u(),
                            n.deg_v(),
                            n.knots_u().to_vec(),
                            n.knots_v().to_vec(),
                            n.nu(),
                            n.nv(),
                            n.ctrl().iter().map(|&p| xf.apply(p)).collect(),
                            n.weights().to_vec(),
                        )
                        .map_err(|e| e.to_string())?,
                    ),
                    mirror,
                ),
            };
            f.surface = surface;
            f.sense ^= flip;
            if mirror {
                for l in &mut f.loops {
                    *l = l.iter().rev().map(|&(e, fwd)| (e, !fwd)).collect();
                }
            }
        }
        Ok(out)
    }

    /// Bounding box of the vertices.
    pub fn bbox(&self) -> Option<(V3, V3)> {
        let first = *self.verts.first()?;
        Some(self.verts.iter().fold((first, first), |(lo, hi), p| {
            (
                v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
            )
        }))
    }
}

/// One part: a body with a name and a colour (sRGB), placed by assembly nodes.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub name: String,
    pub body: Body,
    pub rgb: Option<[u8; 3]>,
}

/// Assembly node: a placed part (leaf) or a placed sub-assembly.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub name: String,
    /// Placement in the parent node.
    pub place: Ax3,
    pub part: Option<usize>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn leaf(name: &str, place: Ax3, part: usize) -> Node {
        Node {
            name: name.into(),
            place,
            part: Some(part),
            children: vec![],
        }
    }
}

/// Parts and the assembly tree under one top product.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub name: String,
    pub parts: Vec<Part>,
    pub roots: Vec<Node>,
}

impl Doc {
    /// Every placement of every part: (node path names, part, map to the document).
    pub fn placements(&self) -> Vec<(Vec<String>, usize, Xf)> {
        fn walk(n: &Node, path: &mut Vec<String>, xf: Xf, out: &mut Vec<(Vec<String>, usize, Xf)>) {
            let xf = xf.after(&Xf::from_ax(&n.place));
            path.push(n.name.clone());
            if let Some(p) = n.part {
                out.push((path.clone(), p, xf));
            }
            for c in &n.children {
                walk(c, path, xf, out);
            }
            path.pop();
        }
        let mut out = vec![];
        for r in &self.roots {
            walk(r, &mut vec![], Xf::ID, &mut out);
        }
        out
    }
}
