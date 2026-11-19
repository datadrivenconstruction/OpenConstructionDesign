// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Triangulation of planar solids: a watertight mesh on the shared vertices of the solid.
//!
//! A face is projected onto a plane along the dominant normal axis, holes are stitched to
//! the outer loop with bridges, then ear clipping runs on exact predicates.

use crate::brep::Solid;
use crate::math::{v2, V2, V3};
use crate::predicates::{orient2d, point_in_ring, segments_meet, shared_end_overlap};

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<V3>,
    pub triangles: Vec<[u32; 3]>,
    /// Body face of each triangle, used for picking and highlighting.
    pub face_of: Vec<u32>,
}

#[derive(Debug, PartialEq)]
pub enum MeshError {
    /// Ear clipping got stuck: the face is non-planar or self-intersecting.
    NoEar {
        face: u32,
    },
    NoBridge {
        face: u32,
        hole: usize,
    },
}

/// Projection to 2D preserving orientation (counter-clockwise when viewed against the normal).
pub(crate) fn projector(n: V3) -> impl Fn(V3) -> V2 {
    let (ax, ay, az) = (n.x.abs(), n.y.abs(), n.z.abs());
    move |p: V3| {
        if az >= ax && az >= ay {
            if n.z >= 0.0 {
                v2(p.x, p.y)
            } else {
                v2(p.y, p.x)
            }
        } else if ax >= ay {
            if n.x >= 0.0 {
                v2(p.y, p.z)
            } else {
                v2(p.z, p.y)
            }
        } else if n.y >= 0.0 {
            v2(p.z, p.x)
        } else {
            v2(p.x, p.z)
        }
    }
}

/// Bridge m→p is blocked by edge (ia, ib): any shared point other than a shared vertex index
/// without overlap along the edge. Passing through a foreign vertex or along an edge is blocked.
fn edge_blocks(mi: u32, pi: u32, ia: u32, ib: u32, pt: &dyn Fn(u32) -> V2) -> bool {
    let (m, p, a, b) = (pt(mi), pt(pi), pt(ia), pt(ib));
    let shared = [(mi, m, p), (pi, p, m)].into_iter().find_map(|(si, s, q)| {
        if ia == si {
            Some((s, q, b))
        } else if ib == si {
            Some((s, q, a))
        } else {
            None
        }
    });
    match shared {
        // Shared vertex: non-collinear segments meet only there.
        Some((s, q, o)) => shared_end_overlap(s, q, o),
        None => segments_meet(m, p, a, b),
    }
}

/// Direction s→m lies strictly inside the polygon's (counter-clockwise) angle at vertex s.
fn in_wedge(prev: V2, s: V2, next: V2, m: V2) -> bool {
    let (l, r) = (orient2d(prev, s, m), orient2d(s, next, m));
    if orient2d(prev, s, next) >= 0.0 {
        l > 0.0 && r > 0.0
    } else {
        l > 0.0 || r > 0.0
    }
}

/// Inserts a hole into the loop with a bridge from the hole's rightmost vertex to the nearest visible
/// loop vertex. The bridge touches no foreign edges or vertices, leaves the vertex into the interior angle
/// and runs inside the face (midpoint inside the loop and outside the holes).
fn bridge(outer: &mut Vec<u32>, hole: &[u32], others: &[Vec<u32>], pt: &dyn Fn(u32) -> V2) -> bool {
    let Some(m_i) = (0..hole.len()).max_by(|&a, &b| {
        let (pa, pb) = (pt(hole[a]), pt(hole[b]));
        pa.x.total_cmp(&pb.x).then(pa.y.total_cmp(&pb.y))
    }) else {
        return false;
    };
    let mi = hole[m_i];
    let m = pt(mi);
    let rings: Vec<&[u32]> = std::iter::once(outer.as_slice())
        .chain(std::iter::once(hole))
        .chain(others.iter().map(|h| h.as_slice()))
        .collect();
    let coords = |r: &[u32]| r.iter().map(|&i| pt(i)).collect::<Vec<V2>>();
    let outer_pts = coords(outer);
    let hole_pts: Vec<Vec<V2>> = std::iter::once(hole)
        .chain(others.iter().map(|h| h.as_slice()))
        .map(coords)
        .collect();
    let n = outer.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let (pa, pb) = (pt(outer[a]) - m, pt(outer[b]) - m);
        pa.len().total_cmp(&pb.len()).then(a.cmp(&b))
    });
    for k in order {
        let pi = outer[k];
        let p = pt(pi);
        if !in_wedge(outer_pts[(k + n - 1) % n], p, outer_pts[(k + 1) % n], m) {
            continue;
        }
        let blocked = rings
            .iter()
            .any(|r| (0..r.len()).any(|e| edge_blocks(mi, pi, r[e], r[(e + 1) % r.len()], pt)));
        if blocked {
            continue;
        }
        let mid = (m + p) * 0.5;
        if point_in_ring(mid, &outer_pts) != 1
            || hole_pts.iter().any(|h| point_in_ring(mid, h) != -1)
        {
            continue;
        }
        let mut spliced: Vec<u32> = Vec::with_capacity(outer.len() + hole.len() + 2);
        spliced.extend_from_slice(&outer[..=k]);
        spliced.extend((0..=hole.len()).map(|i| hole[(m_i + i) % hole.len()]));
        spliced.extend_from_slice(&outer[k..]);
        *outer = spliced;
        return true;
    }
    false
}
/// Ear clipping. Indices are body vertices; repeats (after bridges) are allowed.
fn clip_ears(mut poly: Vec<u32>, pt: &dyn Fn(u32) -> V2, out: &mut Vec<[u32; 3]>) -> bool {
    while poly.len() > 3 {
        let n = poly.len();
        let ear = (0..n).find(|&i| {
            let (ia, ib, ic) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
            let (a, b, c) = (pt(ia), pt(ib), pt(ic));
            if orient2d(a, b, c) <= 0.0 {
                return false;
            }
            poly.iter().all(|&j| {
                if j == ia || j == ib || j == ic {
                    return true;
                }
                let p = pt(j);
                !(orient2d(a, b, p) >= 0.0 && orient2d(b, c, p) >= 0.0 && orient2d(c, a, p) >= 0.0)
            })
        });
        let Some(i) = ear else { return false };
        out.push([poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]]);
        poly.remove(i);
    }
    out.push([poly[0], poly[1], poly[2]]);
    true
}

impl Solid {
    /// Loop of 3–4 vertices without allocation; otherwise None.
    fn small_loop(&self, l: u32) -> Option<([u32; 4], usize)> {
        let mut v = [0u32; 4];
        let mut cur = l;
        let mut n = 0;
        loop {
            if n == 4 {
                return None;
            }
            let h = &self.half_edges[cur as usize];
            v[n] = h.origin;
            n += 1;
            cur = h.next;
            if cur == l {
                return (n >= 3).then_some((v, n));
            }
        }
    }

    pub fn triangulate(&self) -> Result<Mesh, MeshError> {
        let nf = self.faces.len();
        let mut mesh = Mesh {
            positions: self.verts.clone(),
            triangles: Vec::with_capacity(nf * 2),
            face_of: Vec::with_capacity(nf * 2),
        };
        for (fi, f) in self.faces.iter().enumerate() {
            let proj = projector(f.normal);
            let pt = |i: u32| proj(self.verts[i as usize]);
            // Fast path: a triangle or a strictly convex quadrilateral without holes.
            if f.loops.len() == 1 {
                if let Some(q) = self.small_loop(f.loops[0]) {
                    let n = q.1;
                    let v = q.0;
                    let before = mesh.triangles.len();
                    if n == 3 {
                        mesh.triangles.push([v[0], v[1], v[2]]);
                    } else {
                        let p = [pt(v[0]), pt(v[1]), pt(v[2]), pt(v[3])];
                        if (0..4).all(|i| orient2d(p[i], p[(i + 1) % 4], p[(i + 2) % 4]) > 0.0) {
                            // Same result as ear clipping gives (the first ear is vertex 0).
                            mesh.triangles.push([v[3], v[0], v[1]]);
                            mesh.triangles.push([v[1], v[2], v[3]]);
                        }
                    }
                    if mesh.triangles.len() > before {
                        let k = mesh.triangles.len() - before;
                        mesh.face_of.extend(std::iter::repeat_n(fi as u32, k));
                        continue;
                    }
                }
            }
            let mut outer = self.loop_verts(f.loops[0]);
            let mut holes: Vec<Vec<u32>> =
                f.loops[1..].iter().map(|&l| self.loop_verts(l)).collect();
            // Rightmost holes first: their bridges do not cross holes not yet inserted.
            holes.sort_by(|a, b| {
                let mx = |h: &Vec<u32>| h.iter().map(|&i| pt(i).x).fold(f64::MIN, f64::max);
                mx(b).total_cmp(&mx(a))
            });
            for h in 0..holes.len() {
                let rest = holes[h + 1..].to_vec();
                if !bridge(&mut outer, &holes[h], &rest, &pt) {
                    return Err(MeshError::NoBridge {
                        face: fi as u32,
                        hole: h,
                    });
                }
            }
            let start = mesh.triangles.len();
            if !clip_ears(outer, &pt, &mut mesh.triangles) {
                return Err(MeshError::NoEar { face: fi as u32 });
            }
            mesh.face_of
                .extend(std::iter::repeat_n(fi as u32, mesh.triangles.len() - start));
        }
        Ok(mesh)
    }
}

impl Mesh {
    pub fn area(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| self.tri_cross(t).len() * 0.5)
            .sum()
    }

    pub fn volume(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| self.positions[i as usize]);
                a.dot(b.cross(c))
            })
            .sum::<f64>()
            / 6.0
    }

    /// Every directed edge has an opposite one: the mesh is closed and consistently oriented.
    pub fn is_watertight(&self) -> bool {
        let mut edges = std::collections::BTreeMap::<(u32, u32), i32>::new();
        for t in &self.triangles {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *edges.entry((a.min(b), a.max(b))).or_default() += if a < b { 1 } else { -1 };
            }
        }
        edges.values().all(|&v| v == 0)
    }

    fn tri_cross(&self, t: &[u32; 3]) -> V3 {
        let [a, b, c] = t.map(|i| self.positions[i as usize]);
        (b - a).cross(c - a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Tol;
    use crate::profile::Profile;

    fn slab_with_holes(holes: Vec<Vec<V2>>) -> Solid {
        let outer = vec![v2(0., 0.), v2(10., 0.), v2(10., 6.), v2(0., 6.)];
        Solid::extrude(
            &Profile::new(outer, holes, Tol::default()).unwrap(),
            0.3,
            Tol::default(),
        )
    }

    fn sq(x: f64, y: f64, s: f64) -> Vec<V2> {
        vec![v2(x, y), v2(x + s, y), v2(x + s, y + s), v2(x, y + s)]
    }

    #[test]
    fn box_mesh_is_watertight_with_exact_area_and_volume() {
        let s = Solid::extrude(&Profile::rectangle(5.0, 0.3).unwrap(), 3.0, Tol::default());
        let m = s.triangulate().unwrap();
        assert_eq!(m.triangles.len(), 12);
        assert!(m.is_watertight());
        assert!((m.volume() - 4.5).abs() < 1e-12);
        assert!((m.area() - 2.0 * (5.0 * 0.3 + 5.0 * 3.0 + 0.3 * 3.0)).abs() < 1e-12);
    }

    #[test]
    fn slab_with_several_shafts_is_watertight() {
        let s = slab_with_holes(vec![sq(1., 1., 1.), sq(4., 2., 1.5), sq(7., 1., 2.)]);
        let m = s.triangulate().unwrap();
        assert!(m.is_watertight());
        let plan = 60.0 - 1.0 - 2.25 - 4.0;
        assert!((m.volume() - plan * 0.3).abs() < 1e-12);
        assert!((m.volume() - s.volume()).abs() < 1e-12);
    }

    #[test]
    fn concave_l_profile_triangulates() {
        let l = vec![
            v2(0., 0.),
            v2(4., 0.),
            v2(4., 1.),
            v2(1., 1.),
            v2(1., 3.),
            v2(0., 3.),
        ];
        let s = Solid::extrude(
            &Profile::new(l, vec![], Tol::default()).unwrap(),
            2.0,
            Tol::default(),
        );
        let m = s.triangulate().unwrap();
        assert!(m.is_watertight());
        assert!((m.volume() - 6.0 * 2.0).abs() < 1e-12);
    }

    #[test]
    fn triangles_know_their_face() {
        let s = Solid::extrude(&Profile::rectangle(1.0, 1.0).unwrap(), 1.0, Tol::default());
        let m = s.triangulate().unwrap();
        assert_eq!(m.face_of.len(), m.triangles.len());
        assert_eq!(m.face_of.iter().filter(|&&f| f == 1).count(), 2);
    }

    fn check(holes: Vec<Vec<V2>>, plan: f64) {
        let perim: f64 = holes
            .iter()
            .map(|h| {
                (0..h.len())
                    .map(|i| (h[(i + 1) % h.len()] - h[i]).len())
                    .sum::<f64>()
            })
            .sum();
        let s = slab_with_holes(holes);
        let m = s.triangulate().unwrap();
        assert!(m.is_watertight());
        assert!((m.volume() - plan * 0.3).abs() < 1e-12, "{}", m.volume());
        let area = 2.0 * plan + (32.0 + perim) * 0.3;
        assert!((m.area() - area).abs() < 1e-9, "{} vs {area}", m.area());
    }

    #[test]
    fn holes_with_rightmost_vertices_on_same_y() {
        // The bridge from the left hole to the right passes exactly through a vertex of the right hole.
        check(vec![sq(1., 2., 1.), sq(5., 2., 1.)], 60.0 - 2.0);
        // Rhombi: the rightmost vertices lie on one horizontal line.
        let d = |x: f64| vec![v2(x, 3.), v2(x + 1., 2.), v2(x + 2., 3.), v2(x + 1., 4.)];
        check(vec![d(1.), d(4.), d(7.)], 60.0 - 6.0);
    }

    #[test]
    fn bridge_along_edge_of_unbridged_hole_is_rejected() {
        // The nearest loop vertex (0,3) is visible only along the top edge of the left hole.
        let outer = vec![v2(0., 0.), v2(20., 0.), v2(20., 6.), v2(0., 6.), v2(0., 3.)];
        let holes = vec![sq(3., 2.5, 0.5), sq(1., 2.5, 0.5)];
        let s = Solid::extrude(
            &Profile::new(outer, holes, Tol::default()).unwrap(),
            1.0,
            Tol::default(),
        );
        let m = s.triangulate().unwrap();
        assert!(m.is_watertight());
        assert!((m.volume() - (120.0 - 0.5)).abs() < 1e-12, "{}", m.volume());
        let area = 2.0 * 119.5 + 52.0 + 4.0;
        assert!((m.area() - area).abs() < 1e-9, "{} vs {area}", m.area());
    }

    #[test]
    fn holes_aligned_on_grid() {
        let mut holes = vec![];
        for i in 0..4 {
            for j in 0..2 {
                holes.push(sq(1.0 + 2.0 * i as f64, 1.0 + 2.5 * j as f64, 1.0));
            }
        }
        check(holes, 60.0 - 8.0);
    }

    #[test]
    fn bridge_rejects_path_through_vertex_or_along_edge() {
        let ids = |p: &[V2]| p.to_vec();
        let pts = ids(&[v2(0., 0.), v2(2., 0.), v2(4., 0.), v2(3., 1.)]);
        let pt = |i: u32| pts[i as usize];
        // m=0 → p=2 passes through vertex 1 of edge (1,3).
        assert!(edge_blocks(0, 2, 1, 3, &pt));
        // Along edge (0,1), which shares an endpoint with the bridge.
        assert!(edge_blocks(0, 2, 0, 1, &pt));
        // Edge (2,3) touches only at the shared endpoint.
        assert!(!edge_blocks(0, 2, 2, 3, &pt));
    }
}
