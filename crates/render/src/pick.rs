// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! CPU ray picking: mesh AABB → triangles (Möller–Trumbore), nearest hit.

use crate::camera::Ray;
use crate::perf::TreeEv;
use crate::scene::{Aabb, Scene};
use glam::Vec3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub mesh: usize,
    pub triangle: usize,
    pub element: u32,
    pub t: f32,
    pub point: Vec3,
}

pub fn ray_triangle(r: &Ray, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let (e1, e2) = (b - a, c - a);
    let p = r.dir.cross(e2);
    let det = e1.dot(p);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    let s = r.origin - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = r.dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// Ray entry into the box (t ≥ 0) or None.
pub fn ray_aabb(r: &Ray, b: &Aabb) -> Option<f32> {
    let inv = r.dir.recip();
    let t1 = (b.min - r.origin) * inv;
    let t2 = (b.max - r.origin) * inv;
    let tmin = t1.min(t2).max_element().max(0.0);
    let tmax = t1.max(t2).min_element();
    (tmax >= tmin).then_some(tmin)
}

impl Scene {
    /// Boxes come from the tree (`pick_tree`): only branches hit by the ray are traversed.
    pub fn pick(&self, r: &Ray) -> Option<Hit> {
        let tree = self
            .pick_tree
            .get_or_init(|| crate::perf::BoxTree::build(&self.boxes));
        let mut cand: Vec<(f32, usize)> = Vec::new();
        tree.visit(|e| match e {
            TreeEv::Node(b, _) => ray_aabb(r, b).is_some(),
            TreeEv::Leaf(i) => {
                if let Some(t) = ray_aabb(r, &self.boxes[i as usize]) {
                    cand.push((t, i as usize));
                }
                true
            }
        });
        // Same order as the linear pass: by t, ties broken by mesh number.
        cand.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        self.pick_from(r, cand)
    }

    /// Reference without the tree: all boxes in sequence. For cross-checks and benchmarks.
    #[doc(hidden)]
    pub fn pick_linear(&self, r: &Ray) -> Option<Hit> {
        let mut cand: Vec<(f32, usize)> = self
            .boxes
            .iter()
            .enumerate()
            .filter_map(|(i, b)| ray_aabb(r, b).map(|t| (t, i)))
            .collect();
        cand.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.pick_from(r, cand)
    }

    fn pick_from(&self, r: &Ray, cand: Vec<(f32, usize)>) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for (tb, mi) in cand {
            if best.is_some_and(|h| h.t < tb) {
                break;
            }
            let m = &self.meshes[mi];
            for (ti, tri) in m.triangles.iter().enumerate() {
                let [a, b, c] = tri.map(|i| Vec3::from(m.positions[i as usize]));
                if let Some(t) = ray_triangle(r, a, b, c) {
                    if best.is_none_or(|h| t < h.t) {
                        best = Some(Hit {
                            mesh: mi,
                            triangle: ti,
                            element: m.element,
                            t,
                            point: r.at(t),
                        });
                    }
                }
            }
        }
        self.pick_instanced(r, best)
    }

    /// Instances: ray into mesh coordinates (inverse matrix), nearest of `best` and them.
    /// The hit's `mesh` is `meshes.len()` + group number.
    pub fn pick_instanced(&self, r: &Ray, mut best: Option<Hit>) -> Option<Hit> {
        for (g, grp) in self.instanced.iter().enumerate() {
            for i in grp.inst.iter() {
                let Some(tb) = ray_aabb(r, &grp.world_bounds(i)) else {
                    continue;
                };
                if best.is_some_and(|h| h.t < tb) {
                    continue;
                }
                let inv = i.m.inverse();
                let lr = Ray {
                    origin: inv.transform_point3(r.origin),
                    dir: inv.transform_vector3(r.dir),
                };
                let m = &grp.proto;
                for (ti, tri) in m.triangles.iter().enumerate() {
                    let [a, b, c] = tri.map(|k| Vec3::from(m.positions[k as usize]));
                    // the ray's t is preserved: the direction is transformed without normalisation
                    if let Some(t) = ray_triangle(&lr, a, b, c) {
                        if best.is_none_or(|h| t < h.t) {
                            best = Some(Hit {
                                mesh: self.meshes.len() + g,
                                triangle: ti,
                                element: i.element,
                                t,
                                point: r.at(t),
                            });
                        }
                    }
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::SceneMesh;

    fn quad(element: u32, z: f32) -> SceneMesh {
        SceneMesh {
            element,
            tag: 0,
            positions: vec![[0., 0., z], [1., 0., z], [1., 1., z], [0., 1., z]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        }
    }
    fn down(x: f32, y: f32) -> Ray {
        Ray {
            origin: Vec3::new(x, y, 10.0),
            dir: -Vec3::Z,
        }
    }

    #[test]
    fn triangle_hit_and_miss() {
        let (a, b, c) = (Vec3::ZERO, Vec3::X, Vec3::Y);
        assert!((ray_triangle(&down(0.2, 0.2), a, b, c).unwrap() - 10.0).abs() < 1e-5);
        assert_eq!(ray_triangle(&down(0.8, 0.8), a, b, c), None);
        let up = Ray {
            origin: Vec3::new(0.2, 0.2, 1.0),
            dir: Vec3::Z,
        };
        assert_eq!(ray_triangle(&up, a, b, c), None, "behind origin");
    }

    #[test]
    fn nearest_mesh_wins_and_maps_to_element() {
        let s = Scene::new(vec![quad(7, 0.0), quad(42, 2.0), quad(9, 1.0)]);
        let h = s.pick(&down(0.7, 0.2)).unwrap();
        assert_eq!((h.mesh, h.triangle, h.element), (1, 0, 42));
        assert!((h.point.z - 2.0).abs() < 1e-5);
        assert_eq!(s.pick(&down(0.2, 0.7)).unwrap().triangle, 1);
        assert_eq!(s.pick(&down(3.0, 3.0)), None);
    }

    #[test]
    fn instances_pick_by_their_element() {
        use crate::chunks::{Inst, Instanced};
        use glam::Mat4;
        let g = Instanced::new(
            1,
            std::sync::Arc::new(quad(0, 0.0)),
            vec![
                Inst {
                    m: Mat4::from_translation(Vec3::new(5.0, 0.0, 1.0)),
                    element: 77,
                },
                Inst {
                    m: Mat4::from_translation(Vec3::new(5.0, 0.0, 3.0)),
                    element: 78,
                },
            ],
        );
        let s = Scene::new(vec![quad(7, 0.0)]).with_instanced(vec![g]);
        let h = s.pick(&down(5.5, 0.5)).unwrap();
        assert_eq!(h.element, 78, "upper instance is nearer");
        assert!((h.point.z - 3.0).abs() < 1e-5);
        assert_eq!(s.pick(&down(0.5, 0.5)).unwrap().element, 7);
        assert!(s.bounds.unwrap().max.x >= 6.0);
    }

    /// xorshift64*: reproducible random scenes without external dependencies.
    struct Rng(u64);
    impl Rng {
        fn f(&mut self) -> f32 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
        }
        fn v(&mut self, s: f32) -> Vec3 {
            Vec3::new(self.f(), self.f(), self.f()) * s
        }
    }

    /// Random triangles; some meshes are empty, some coincide (equal t), some are flat.
    fn random_scene(rng: &mut Rng, n: usize, extent: f32) -> Scene {
        let mut meshes = Vec::with_capacity(n);
        for i in 0..n {
            let kind = i % 7;
            let o = rng.v(extent);
            let (positions, triangles) = match kind {
                0 => (vec![], vec![]),
                1 => {
                    let z = (o.z / 4.0).floor() * 4.0;
                    let q = quad(0, z);
                    let p = q
                        .positions
                        .iter()
                        .map(|p| [p[0] + o.x.floor(), p[1] + o.y.floor(), p[2]])
                        .collect();
                    (p, q.triangles)
                }
                _ => {
                    let p: Vec<[f32; 3]> = (0..6)
                        .map(|_| (o + rng.v(3.0) - Vec3::splat(1.5)).to_array())
                        .collect();
                    (p, vec![[0, 1, 2], [3, 4, 5], [0, 2, 4]])
                }
            };
            meshes.push(SceneMesh {
                element: i as u32,
                tag: 0,
                positions,
                triangles,
            });
        }
        Scene::new(meshes)
    }

    fn random_ray(rng: &mut Rng, extent: f32, k: usize) -> Ray {
        let origin = rng.v(extent * 1.4) - Vec3::splat(extent * 0.2);
        let dir = match k % 4 {
            0 => -Vec3::Z,
            1 => Vec3::new(rng.f() - 0.5, rng.f() - 0.5, 0.0).normalize_or(Vec3::X),
            _ => (rng.v(2.0) - Vec3::ONE).normalize_or(Vec3::Y),
        };
        Ray { origin, dir }
    }

    #[test]
    fn tree_pick_equals_linear_pick() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for (n, extent) in [(0, 10.0), (1, 2.0), (9, 4.0), (300, 20.0), (5000, 60.0)] {
            let s = random_scene(&mut rng, n, extent);
            let mut hits = 0;
            for k in 0..2000 {
                let r = random_ray(&mut rng, extent, k);
                let (a, b) = (s.pick(&r), s.pick_linear(&r));
                assert_eq!(a, b, "n={n} ray#{k} {r:?}");
                hits += a.is_some() as usize;
            }
            assert!(n < 300 || hits > 100, "n={n}: too few hits ({hits}) to compare");
        }
    }

    #[test]
    fn large_scene_builds_tree_lazily_and_matches_linear() {
        let mut rng = Rng(7);
        let s = random_scene(&mut rng, 30_000, 200.0);
        assert!(s.pick_tree.get().is_none(), "above the eager limit");
        let mut rays: Vec<Ray> = (0..400).map(|k| random_ray(&mut rng, 200.0, k)).collect();
        rays.push(down(0.5, 0.5));
        let t0 = web_time::Instant::now();
        let a: Vec<_> = rays.iter().map(|r| s.pick(r)).collect();
        let tree = t0.elapsed();
        let t1 = web_time::Instant::now();
        let b: Vec<_> = rays.iter().map(|r| s.pick_linear(r)).collect();
        let lin = t1.elapsed();
        assert_eq!(a, b);
        assert!(s.pick_tree.get().is_some());
        eprintln!(
            "pick 30k meshes, {} rays: tree {:?} (incl. build), linear {:?}",
            rays.len(),
            tree,
            lin
        );
    }

    #[test]
    fn aabb_slab() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        assert_eq!(ray_aabb(&down(0.5, 0.5), &b), Some(9.0));
        assert_eq!(ray_aabb(&down(1.5, 0.5), &b), None);
    }
}
