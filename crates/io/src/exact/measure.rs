// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Volume of a closed body from its faces (divergence theorem): planar faces through their
//! boundary, B-spline faces over their whole parameter domain. Other surface kinds are left to
//! `recognize`, which maps them back to our forms.

use ddc_geom::math::V3;
use ddc_geom::nurbs;

use super::{Body, Curve, Surface};

/// Gauss-Legendre nodes and weights on [-1, 1].
const GL: [(f64, f64); 6] = [
    (-0.932_469_514_203_152, 0.171_324_492_379_170),
    (-0.661_209_386_466_265, 0.360_761_573_048_139),
    (-0.238_619_186_083_197, 0.467_913_934_572_691),
    (0.238_619_186_083_197, 0.467_913_934_572_691),
    (0.661_209_386_466_265, 0.360_761_573_048_139),
    (0.932_469_514_203_152, 0.171_324_492_379_170),
];

fn spans(knots: &[f64]) -> Vec<(f64, f64)> {
    knots.windows(2).filter(|w| w[1] > w[0]).map(|w| (w[0], w[1])).collect()
}

/// Integral of p x dp along the curve over its whole domain.
fn curve_moment(c: &nurbs::Curve) -> V3 {
    let mut s = V3::default();
    for (a, b) in spans(c.knots()) {
        let h = (b - a) / 2.0;
        for &(x, w) in &GL {
            let d = c.derivs(a + h * (x + 1.0), 1);
            s = s + d[0].cross(d[1]) * (w * h);
        }
    }
    s
}

/// Integral of p x dp along an edge from its first to its second vertex.
fn edge_moment(b: &Body, e: usize) -> Option<V3> {
    let ed = &b.edges[e];
    let (p0, p1) = (b.verts[ed.v[0] as usize], b.verts[ed.v[1] as usize]);
    match &ed.curve {
        Curve::Line => Some(p0.cross(p1)),
        Curve::Circle { ax, r } => {
            let ang = |p: V3| {
                let d = p - ax.o;
                d.dot(ax.y()).atan2(d.dot(ax.x))
            };
            let tau = std::f64::consts::TAU;
            let (a0, a1) = (ang(p0), ang(p1));
            let ccw = (a1 - a0).rem_euclid(tau);
            let sweep = if ed.v[0] == ed.v[1] {
                if ed.sense {
                    tau
                } else {
                    -tau
                }
            } else if ed.sense {
                ccw
            } else {
                ccw - tau
            };
            Some(ax.o.cross(p1 - p0) + ax.z * (r * r * sweep))
        }
        Curve::Nurbs(c) => {
            let m = curve_moment(c);
            Some(if ed.sense { m } else { -m })
        }
        Curve::Ellipse { .. } => None,
    }
}

/// Signed volume enclosed by the faces (positive for outward normals); None when a face is
/// neither planar nor a whole B-spline patch.
pub fn volume(b: &Body) -> Option<f64> {
    let mut v = 0.0;
    for f in &b.faces {
        let sign = if f.sense { 1.0 } else { -1.0 };
        match &f.surface {
            Surface::Plane(ax) => {
                let n = ax.z * sign;
                let mut m = V3::default();
                for l in &f.loops {
                    for &(e, fwd) in l {
                        let x = edge_moment(b, e as usize)?;
                        m = m + if fwd { x } else { -x };
                    }
                }
                v += ax.o.dot(n) * m.dot(n) / 2.0;
            }
            Surface::Nurbs(s) => {
                let mut acc = 0.0;
                for (u0, u1) in spans(s.knots_u()) {
                    for (w0, w1) in spans(s.knots_v()) {
                        let (hu, hv) = ((u1 - u0) / 2.0, (w1 - w0) / 2.0);
                        for &(x, wx) in &GL {
                            for &(y, wy) in &GL {
                                let d = s.derivs(u0 + hu * (x + 1.0), w0 + hv * (y + 1.0), 1);
                                acc += d[0][0].dot(d[1][0].cross(d[0][1])) * wx * wy * hu * hv;
                            }
                        }
                    }
                }
                v += sign * acc;
            }
            _ => return None,
        }
    }
    Some(v / 3.0)
}
