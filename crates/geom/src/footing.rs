//! Foundations: pad footings (rectangular and circular, stepped) and a slab with a thickened edge.
//! Quantities are exact: prism = area × height, circle = πd²/4·h; meshes are for display only
//! (a circle becomes a 32-gon). A strip footing is a `layered::Strip` with wall joints (model).

use std::f64::consts::PI;

use crate::math::{v2, v3, V2};
use crate::mesh::Mesh;
use crate::poly;

/// Number of circle segments in the mesh.
pub const CIRCLE_SEGS: usize = 32;

/// Step: plan size (for a circle `w` is the diameter and `l` is unused) and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub w: f64,
    pub l: f64,
    pub h: f64,
}

/// Pad footing: steps bottom to top (base first), center, rotation, top elevation.
#[derive(Clone, Debug, PartialEq)]
pub struct Isolated {
    pub round: bool,
    pub steps: Vec<Step>,
    pub center: V2,
    pub rotation: f64,
    pub top: f64,
}

/// Quantities: volume, base area, lateral surface (formwork), height.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct FootingQty {
    pub volume: f64,
    pub base_area: f64,
    pub formwork: f64,
    pub depth: f64,
}

fn fin(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// Prism over a simple outline: z0 < z1, closed mesh with outward normals.
pub fn prism(ring: &[V2], z0: f64, z1: f64) -> Mesh {
    let n = ring.len();
    let mut m = Mesh::default();
    if n < 3 || z1 <= z0 {
        return m;
    }
    let mut r: Vec<V2> = ring.to_vec();
    if poly::area(&r) < 0.0 {
        r.reverse();
    }
    m.positions.extend(r.iter().map(|p| v3(p.x, p.y, z0)));
    m.positions.extend(r.iter().map(|p| v3(p.x, p.y, z1)));
    let (lo, hi) = (|i: usize| i as u32, |i: usize| (n + i) as u32);
    for i in 0..n {
        let j = (i + 1) % n;
        m.triangles.push([lo(i), lo(j), hi(j)]);
        m.triangles.push([lo(i), hi(j), hi(i)]);
    }
    for t in poly::triangulate(&r) {
        m.triangles.push([lo(t[0]), lo(t[2]), lo(t[1])]);
        m.triangles.push([hi(t[0]), hi(t[1]), hi(t[2])]);
    }
    m.face_of = vec![0; m.triangles.len()];
    m
}

/// Ring prism between an outer outline and a nested one (same vertex count, offset inward).
pub fn annulus(outer: &[V2], inner: &[V2], z0: f64, z1: f64) -> Mesh {
    let n = outer.len();
    let mut m = Mesh::default();
    if n < 3 || inner.len() != n || z1 <= z0 {
        return m;
    }
    let (mut o, mut i_) = (outer.to_vec(), inner.to_vec());
    if poly::area(&o) < 0.0 {
        o.reverse();
        i_.reverse();
    }
    for (ring, z) in [(&o, z0), (&o, z1), (&i_, z0), (&i_, z1)] {
        m.positions.extend(ring.iter().map(|p| v3(p.x, p.y, z)));
    }
    let id = |k: usize, i: usize| (k * n + i % n) as u32;
    for i in 0..n {
        let j = i + 1;
        // outer wall faces outward, inner wall faces inward
        m.triangles.push([id(0, i), id(0, j), id(1, j)]);
        m.triangles.push([id(0, i), id(1, j), id(1, i)]);
        m.triangles.push([id(2, i), id(3, j), id(2, j)]);
        m.triangles.push([id(2, i), id(3, i), id(3, j)]);
        // bottom (facing down) and top (facing up)
        m.triangles.push([id(0, i), id(2, j), id(0, j)]);
        m.triangles.push([id(0, i), id(2, i), id(2, j)]);
        m.triangles.push([id(1, i), id(1, j), id(3, j)]);
        m.triangles.push([id(1, i), id(3, j), id(3, i)]);
    }
    m.face_of = vec![0; m.triangles.len()];
    m
}

/// Outline offset inward by `d` (edges stay parallel, vertices are intersections of the offset edges).
/// None if the outline degenerates or self-intersects.
pub fn inset(ring: &[V2], d: f64) -> Option<Vec<V2>> {
    let n = ring.len();
    if n < 3 || !d.is_finite() || d <= 0.0 {
        return None;
    }
    let mut r = ring.to_vec();
    let flip = poly::area(&r) < 0.0;
    if flip {
        r.reverse();
    }
    let line = |i: usize| -> Option<(V2, V2)> {
        let (p, q) = (r[i], r[(i + 1) % n]);
        let e = q - p;
        let l = e.len();
        if l < 1e-12 {
            return None;
        }
        let u = e * (1.0 / l);
        Some((p + v2(-u.y, u.x) * d, u))
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (p0, u0) = line((i + n - 1) % n)?;
        let (p1, u1) = line(i)?;
        let den = u0.cross(u1);
        if den.abs() < 1e-12 {
            out.push(p1);
            continue;
        }
        let t = (p1 - p0).cross(u1) / den;
        out.push(p0 + u0 * t);
    }
    // each edge must keep its direction, otherwise the corner has flipped inside out
    for i in 0..n {
        let (a, b) = (r[(i + 1) % n] - r[i], out[(i + 1) % n] - out[i]);
        if a.dot(b) <= 0.0 {
            return None;
        }
    }
    if poly::simple_ring(&out, 1e-9).is_err() || poly::area(&out) <= 0.0 {
        return None;
    }
    if flip {
        out.reverse();
    }
    Some(out)
}

fn circle(c: V2, r: f64) -> Vec<V2> {
    (0..CIRCLE_SEGS)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / CIRCLE_SEGS as f64;
            c + v2(r * a.cos(), r * a.sin())
        })
        .collect()
}

impl Isolated {
    /// Sizes are positive and finite; no step is wider than the one below it.
    pub fn check(&self) -> Result<(), String> {
        if self.steps.is_empty() {
            return Err("footing: no steps".into());
        }
        if !fin(&[self.center.x, self.center.y, self.rotation, self.top]) {
            return Err("footing: non-finite placement".into());
        }
        for (k, s) in self.steps.iter().enumerate() {
            let l = if self.round { s.w } else { s.l };
            if !fin(&[s.w, l, s.h]) || s.w <= 0.0 || l <= 0.0 || s.h <= 0.0 {
                return Err(format!("footing step {k}: sizes must be positive"));
            }
            if k > 0 {
                let p = self.steps[k - 1];
                if s.w > p.w + 1e-12 || (!self.round && s.l > p.l + 1e-12) {
                    return Err(format!("footing step {k}: wider than the step below"));
                }
            }
        }
        Ok(())
    }

    pub fn depth(&self) -> f64 {
        self.steps.iter().map(|s| s.h).sum()
    }

    pub fn bottom(&self) -> f64 {
        self.top - self.depth()
    }

    fn step_area(&self, s: &Step) -> f64 {
        if self.round {
            PI * s.w * s.w / 4.0
        } else {
            s.w * s.l
        }
    }

    fn step_perimeter(&self, s: &Step) -> f64 {
        if self.round {
            PI * s.w
        } else {
            2.0 * (s.w + s.l)
        }
    }

    /// Step outline in plan (w runs along the rotated x axis).
    pub fn outline(&self, k: usize) -> Vec<V2> {
        let s = self.steps[k];
        if self.round {
            return circle(self.center, s.w / 2.0);
        }
        let (c, sn) = (self.rotation.cos(), self.rotation.sin());
        let (hx, hy) = (s.w / 2.0, s.l / 2.0);
        [(-hx, -hy), (hx, -hy), (hx, hy), (-hx, hy)]
            .iter()
            .map(|&(x, y)| self.center + v2(x * c - y * sn, x * sn + y * c))
            .collect()
    }

    pub fn quantities(&self) -> FootingQty {
        let volume = self.steps.iter().map(|s| self.step_area(s) * s.h).sum();
        let formwork = self
            .steps
            .iter()
            .map(|s| self.step_perimeter(s) * s.h)
            .sum();
        FootingQty {
            volume,
            base_area: self.step_area(&self.steps[0]),
            formwork,
            depth: self.depth(),
        }
    }

    /// One mesh per step, bottom to top.
    pub fn meshes(&self) -> Vec<Mesh> {
        let mut z = self.bottom();
        (0..self.steps.len())
            .map(|k| {
                let h = self.steps[k].h;
                let m = prism(&self.outline(k), z, z + h);
                z += h;
                m
            })
            .collect()
    }
}

/// Foundation slab: outline, top, thickness; the thickened edge is a band of width `edge_width`
/// along the outline, `edge_depth` deep below the slab (0 means no thickening).
#[derive(Clone, Debug, PartialEq)]
pub struct Raft {
    pub ring: Vec<V2>,
    pub top: f64,
    pub thickness: f64,
    pub edge_width: f64,
    pub edge_depth: f64,
}

impl Raft {
    pub fn thickened(&self) -> bool {
        self.edge_width > 0.0 && self.edge_depth > 0.0
    }

    /// Inner edge of the thickening (outline offset by the edge width).
    pub fn inner(&self) -> Option<Vec<V2>> {
        if !self.thickened() {
            return None;
        }
        inset(&self.ring, self.edge_width)
    }

    pub fn check(&self) -> Result<(), String> {
        let ok = fin(&[self.top, self.thickness, self.edge_width, self.edge_depth])
            && self.ring.iter().all(|p| fin(&[p.x, p.y]));
        if !ok || self.thickness <= 0.0 || self.edge_width < 0.0 || self.edge_depth < 0.0 {
            return Err("raft: sizes must be finite, thickness positive".into());
        }
        if self.ring.len() < 3 || poly::simple_ring(&self.ring, 1e-9).is_err() {
            return Err("raft: outline must be a simple ring".into());
        }
        if self.thickened() && self.inner().is_none() {
            return Err("raft: edge width too large for the outline".into());
        }
        Ok(())
    }

    pub fn quantities(&self) -> FootingQty {
        let a = poly::area(&self.ring).abs();
        let per = poly::perimeter(&self.ring);
        let band = self
            .inner()
            .map_or(0.0, |r| (a - poly::area(&r).abs()) * self.edge_depth);
        let inner_per = self.inner().map_or(0.0, |r| poly::perimeter(&r));
        let deep = if self.thickened() {
            self.edge_depth
        } else {
            0.0
        };
        FootingQty {
            volume: a * self.thickness + band,
            base_area: a,
            formwork: per * (self.thickness + deep) + inner_per * deep,
            depth: self.thickness + deep,
        }
    }

    /// The slab and, if thickened, the ring rib beneath it.
    pub fn meshes(&self) -> Vec<Mesh> {
        let z1 = self.top - self.thickness;
        let mut v = vec![prism(&self.ring, z1, self.top)];
        if let Some(inner) = self.inner() {
            v.push(annulus(&self.ring, &inner, z1 - self.edge_depth, z1));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(a: f64) -> Vec<V2> {
        vec![v2(0.0, 0.0), v2(a, 0.0), v2(a, a), v2(0.0, a)]
    }

    #[test]
    fn stepped_rect_volume_mesh_and_outline() {
        let f = Isolated {
            round: false,
            steps: vec![
                Step {
                    w: 2.0,
                    l: 1.6,
                    h: 0.4,
                },
                Step {
                    w: 1.0,
                    l: 0.8,
                    h: 0.3,
                },
            ],
            center: v2(5.0, 1.0),
            rotation: 0.3,
            top: -0.1,
        };
        f.check().unwrap();
        let q = f.quantities();
        assert!((q.volume - (2.0 * 1.6 * 0.4 + 1.0 * 0.8 * 0.3)).abs() < 1e-12);
        assert!((q.base_area - 3.2).abs() < 1e-12);
        assert!((q.formwork - (7.2 * 0.4 + 3.6 * 0.3)).abs() < 1e-12);
        assert!((f.bottom() + 0.8).abs() < 1e-12);
        let mv: f64 = f.meshes().iter().map(|m| m.volume()).sum();
        assert!((mv - q.volume).abs() < 1e-9, "{mv}");
        assert!(f.meshes().iter().all(|m| m.is_watertight()));
        assert!((poly::area(&f.outline(0)) - 3.2).abs() < 1e-12);
    }

    #[test]
    fn round_footing_exact_and_bad_steps_rejected() {
        let f = Isolated {
            round: true,
            steps: vec![Step {
                w: 1.5,
                l: 0.0,
                h: 0.5,
            }],
            center: v2(0.0, 0.0),
            rotation: 0.0,
            top: 0.0,
        };
        f.check().unwrap();
        assert!((f.quantities().volume - PI * 1.5 * 1.5 / 4.0 * 0.5).abs() < 1e-12);
        let mut g = f.clone();
        g.steps.push(Step {
            w: 2.0,
            l: 0.0,
            h: 0.2,
        });
        assert!(g.check().is_err());
        g.steps.clear();
        assert!(g.check().is_err());
    }

    #[test]
    fn inset_square_and_l_shape() {
        let r = inset(&sq(4.0), 0.5).unwrap();
        assert!((poly::area(&r) - 9.0).abs() < 1e-12);
        let l = vec![
            v2(0.0, 0.0),
            v2(6.0, 0.0),
            v2(6.0, 2.0),
            v2(2.0, 2.0),
            v2(2.0, 6.0),
            v2(0.0, 6.0),
        ];
        let i = inset(&l, 0.5).unwrap();
        // L 6×6 with flanges of 2: inner L with flanges of 1 and sides of 5
        assert!((poly::area(&i) - (5.0 * 1.0 + 1.0 * 4.0)).abs() < 1e-9);
        assert!(inset(&sq(1.0), 0.6).is_none());
    }

    #[test]
    fn raft_with_thickened_edge() {
        let r = Raft {
            ring: sq(10.0),
            top: 0.0,
            thickness: 0.3,
            edge_width: 0.6,
            edge_depth: 0.4,
        };
        r.check().unwrap();
        let q = r.quantities();
        let band = 100.0 - 8.8 * 8.8;
        assert!((q.volume - (30.0 + band * 0.4)).abs() < 1e-9);
        let mv: f64 = r.meshes().iter().map(|m| m.volume()).sum();
        assert!((mv - q.volume).abs() < 1e-9, "{mv}");
        assert!(r.meshes()[1].is_watertight());
        let flat = Raft {
            edge_depth: 0.0,
            ..r.clone()
        };
        assert_eq!(flat.meshes().len(), 1);
        assert!((flat.quantities().volume - 30.0).abs() < 1e-12);
        let bad = Raft {
            edge_width: 6.0,
            ..r
        };
        assert!(bad.check().is_err());
    }
}
