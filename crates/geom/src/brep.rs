// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Half-edge B-rep stored in flat arrays (KERNEL.md §4).

use std::collections::BTreeMap;

use crate::math::{v3, Tol, V3};
use crate::profile::Profile;

/// Face origin: the basis of stable topology names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Bottom,
    Top,
    /// Side face: (profile contour, contour edge).
    Side {
        ring: u32,
        edge: u32,
    },
    /// Obtained from an external kernel (KERNEL.md §6).
    External,
}

#[derive(Clone, Copy, Debug)]
pub struct HalfEdge {
    pub origin: u32,
    pub twin: u32,
    pub next: u32,
    pub face: u32,
}

#[derive(Clone, Debug)]
pub struct Face {
    /// First half-edge of each loop; 0 is the outer loop.
    pub loops: Vec<u32>,
    pub normal: V3,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub struct Solid {
    pub verts: Vec<V3>,
    pub half_edges: Vec<HalfEdge>,
    pub faces: Vec<Face>,
    pub tol: Tol,
}

#[derive(Debug, PartialEq)]
pub enum ValidateError {
    BadTwin(u32),
    OpenLoop(u32),
    Euler { v: i64, e: i64, f: i64, rings: i64 },
}

const NONE: u32 = u32::MAX;

impl Solid {
    /// Straight extrusion of a profile along +Z. `h` must exceed the tolerance.
    pub fn extrude(p: &Profile, h: f64, tol: Tol) -> Solid {
        assert!(h > tol.length, "extrusion height must exceed tolerance");
        let mut s = Solid {
            verts: vec![],
            half_edges: vec![],
            faces: vec![],
            tol,
        };
        let mut pairs: BTreeMap<(u32, u32), u32> = BTreeMap::new();
        s.faces.push(Face {
            loops: vec![],
            normal: v3(0., 0., -1.),
            origin: Origin::Bottom,
        });
        s.faces.push(Face {
            loops: vec![],
            normal: v3(0., 0., 1.),
            origin: Origin::Top,
        });

        for (ring, pts) in p.rings().enumerate() {
            let n = pts.len() as u32;
            let b = s.verts.len() as u32;
            s.verts.extend(pts.iter().map(|q| v3(q.x, q.y, 0.0)));
            s.verts.extend(pts.iter().map(|q| v3(q.x, q.y, h)));
            // The bottom is walked in reverse order so the normal points outward (down).
            let bottom: Vec<u32> = (0..n).rev().map(|i| b + i).collect();
            let top: Vec<u32> = (0..n).map(|i| b + n + i).collect();
            let l = s.add_loop(&bottom, 0, &mut pairs);
            s.faces[0].loops.push(l);
            let l = s.add_loop(&top, 1, &mut pairs);
            s.faces[1].loops.push(l);
            for i in 0..n {
                let j = (i + 1) % n;
                let quad = [b + i, b + j, b + n + j, b + n + i];
                let face = s.faces.len() as u32;
                let dir = s.verts[quad[1] as usize] - s.verts[quad[0] as usize];
                let l = s.add_loop(&quad, face, &mut pairs);
                s.faces.push(Face {
                    loops: vec![l],
                    normal: dir.cross(v3(0., 0., 1.)).normalized(),
                    origin: Origin::Side {
                        ring: ring as u32,
                        edge: i,
                    },
                });
            }
        }
        for (&(a, b), &he) in &pairs {
            if let Some(&t) = pairs.get(&(b, a)) {
                s.half_edges[he as usize].twin = t;
            }
        }
        s
    }

    fn add_loop(&mut self, vs: &[u32], face: u32, pairs: &mut BTreeMap<(u32, u32), u32>) -> u32 {
        let first = self.half_edges.len() as u32;
        let n = vs.len();
        for i in 0..n {
            self.half_edges.push(HalfEdge {
                origin: vs[i],
                twin: NONE,
                next: first + ((i + 1) % n) as u32,
                face,
            });
            pairs.insert((vs[i], vs[(i + 1) % n]), first + i as u32);
        }
        first
    }

    /// Invariants: mutual twins, closed loops, and
    /// Euler-Poincare for a single shell: V − E + F − (L − F) = 2(1 − genus).
    pub fn validate(&self, genus: i64) -> Result<(), ValidateError> {
        for (i, h) in self.half_edges.iter().enumerate() {
            let t = h.twin as usize;
            if t >= self.half_edges.len() || self.half_edges[t].twin as usize != i {
                return Err(ValidateError::BadTwin(i as u32));
            }
        }
        for f in &self.faces {
            for &l in &f.loops {
                let mut cur = l;
                for step in 0.. {
                    cur = self.half_edges[cur as usize].next;
                    if cur == l {
                        break;
                    }
                    if step > self.half_edges.len() {
                        return Err(ValidateError::OpenLoop(l));
                    }
                }
            }
        }
        let v = self.verts.len() as i64;
        let e = self.half_edges.len() as i64 / 2;
        let f = self.faces.len() as i64;
        let rings = self
            .faces
            .iter()
            .map(|f| f.loops.len() as i64 - 1)
            .sum::<i64>();
        if v - e + f - rings != 2 * (1 - genus) {
            return Err(ValidateError::Euler { v, e, f, rings });
        }
        Ok(())
    }

    /// Volume via the divergence theorem: sum of signed tetrahedra from the origin
    /// over a fan triangulation of each loop (hole loops are oriented in reverse and subtract).
    pub fn volume(&self) -> f64 {
        let mut six_v = 0.0;
        for f in &self.faces {
            for &l in &f.loops {
                let pts = self.loop_points(l);
                for w in 1..pts.len() - 1 {
                    six_v += pts[0].dot(pts[w].cross(pts[w + 1]));
                }
            }
        }
        six_v / 6.0
    }

    pub fn translated_z(mut self, dz: f64) -> Solid {
        self.verts.iter_mut().for_each(|v| v.z += dz);
        self
    }

    pub fn loop_verts(&self, l: u32) -> Vec<u32> {
        let mut out = vec![];
        let mut cur = l;
        loop {
            out.push(self.half_edges[cur as usize].origin);
            cur = self.half_edges[cur as usize].next;
            if cur == l {
                return out;
            }
        }
    }

    pub fn loop_points(&self, l: u32) -> Vec<V3> {
        let mut out = vec![];
        let mut cur = l;
        loop {
            out.push(self.verts[self.half_edges[cur as usize].origin as usize]);
            cur = self.half_edges[cur as usize].next;
            if cur == l {
                return out;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::v2;

    #[test]
    fn extruded_wall_is_valid_and_has_exact_volume() {
        let s = Solid::extrude(&Profile::rectangle(5.0, 0.3).unwrap(), 3.0, Tol::default());
        assert_eq!(s.validate(0), Ok(()));
        assert!((s.volume() - 4.5).abs() < 1e-12);
        assert_eq!(s.faces.len(), 6);
    }

    #[test]
    fn slab_with_shaft_has_genus_one_and_subtracts_hole() {
        let outer = vec![v2(0., 0.), v2(4., 0.), v2(4., 4.), v2(0., 4.)];
        let hole = vec![v2(1., 1.), v2(3., 1.), v2(3., 3.), v2(1., 3.)];
        let p = Profile::new(outer, vec![hole], Tol::default()).unwrap();
        let s = Solid::extrude(&p, 0.25, Tol::default());
        assert_eq!(s.validate(1), Ok(()));
        assert!((s.volume() - 3.0).abs() < 1e-12);
    }

    #[test]
    fn side_faces_keep_stable_names() {
        let s = Solid::extrude(&Profile::rectangle(1.0, 1.0).unwrap(), 1.0, Tol::default());
        let sides: Vec<_> = s
            .faces
            .iter()
            .filter_map(|f| match f.origin {
                Origin::Side { ring, edge } => Some((ring, edge)),
                _ => None,
            })
            .collect();
        assert_eq!(sides, vec![(0, 0), (0, 1), (0, 2), (0, 3)]);
    }

    #[test]
    fn side_normals_point_outward() {
        let s = Solid::extrude(&Profile::rectangle(2.0, 1.0).unwrap(), 1.0, Tol::default());
        for f in &s.faces {
            let pts = s.loop_points(f.loops[0]);
            let centre = pts.iter().fold(v3(0., 0., 0.), |a, &p| a + p) * (1.0 / pts.len() as f64);
            assert!(
                (centre - v3(1.0, 0.5, 0.5)).dot(f.normal) > 0.0,
                "{:?}",
                f.origin
            );
        }
    }
}
