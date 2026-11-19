//! Clashes between MEP networks and structure: convex polyhedra, exact intersection volume
//! (face clipping), BVH over bounding boxes. An obstacle body is a set of signed convex
//! pieces (+ body, - hole inside it); the intersection volume is their sum.

use crate::math::{v2, v3, V2, V3};

/// Convex polyhedron: faces are polygons counter-clockwise when viewed from outside.
#[derive(Clone, Debug, PartialEq)]
pub struct Convex {
    pub faces: Vec<Vec<V3>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: V3,
    pub max: V3,
}

impl Aabb {
    pub fn of(pts: impl IntoIterator<Item = V3>) -> Aabb {
        let mut b = Aabb {
            min: v3(f64::INFINITY, f64::INFINITY, f64::INFINITY),
            max: v3(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
        };
        for p in pts {
            b.min = v3(b.min.x.min(p.x), b.min.y.min(p.y), b.min.z.min(p.z));
            b.max = v3(b.max.x.max(p.x), b.max.y.max(p.y), b.max.z.max(p.z));
        }
        b
    }
    pub fn union(&self, o: &Aabb) -> Aabb {
        Aabb::of([self.min, self.max, o.min, o.max])
    }
    pub fn overlaps(&self, o: &Aabb) -> bool {
        self.min.x <= o.max.x
            && o.min.x <= self.max.x
            && self.min.y <= o.max.y
            && o.min.y <= self.max.y
            && self.min.z <= o.max.z
            && o.min.z <= self.max.z
    }
    fn center(&self, axis: usize) -> f64 {
        let c = (self.min + self.max) * 0.5;
        [c.x, c.y, c.z][axis]
    }
}

impl Convex {
    /// Prism: convex plan contour (any orientation), bottom `z0`, top `z1`.
    pub fn prism(poly: &[V2], z0: f64, z1: f64) -> Convex {
        let mut p = poly.to_vec();
        if crate::poly::area(&p) < 0.0 {
            p.reverse();
        }
        Self::extrude(
            &p.iter().map(|q| v3(q.x, q.y, z0)).collect::<Vec<_>>(),
            v3(0.0, 0.0, z1 - z0),
        )
    }
    /// Convex 3D contour (counter-clockwise when looking against `d`) extruded by translation `d`.
    pub fn extrude(base: &[V3], d: V3) -> Convex {
        let n = base.len();
        let top: Vec<V3> = base.iter().map(|&p| p + d).collect();
        let mut faces = vec![base.iter().rev().copied().collect::<Vec<_>>(), top.clone()];
        for i in 0..n {
            let j = (i + 1) % n;
            faces.push(vec![base[i], base[j], top[j], top[i]]);
        }
        let mut c = Convex { faces };
        if c.volume() < 0.0 {
            for f in &mut c.faces {
                f.reverse();
            }
        }
        c
    }
    /// Straight run: contour in (side, up) along a→b.
    pub fn segment(outline: &[V2], a: V3, b: V3, side: V3, up: V3) -> Convex {
        let base: Vec<V3> = outline.iter().map(|q| a + side * q.x + up * q.y).collect();
        Self::extrude(&base, b - a)
    }
    pub fn aabb(&self) -> Aabb {
        Aabb::of(self.faces.iter().flatten().copied())
    }
    pub fn volume(&self) -> f64 {
        self.faces
            .iter()
            .map(|f| {
                (1..f.len().saturating_sub(1))
                    .map(|k| f[0].dot(f[k].cross(f[k + 1])))
                    .sum::<f64>()
            })
            .sum::<f64>()
            / 6.0
    }
    fn planes(&self) -> Vec<(V3, f64)> {
        self.faces
            .iter()
            .filter_map(|f| {
                let mut n = v3(0.0, 0.0, 0.0);
                for k in 0..f.len() {
                    n = n + f[k].cross(f[(k + 1) % f.len()]);
                }
                let l = n.len();
                (l > 1e-18).then(|| {
                    let n = n * (1.0 / l);
                    (n, n.dot(f[0]))
                })
            })
            .collect()
    }
    /// Clip by the half-space `n·x <= c`.
    pub fn clip(&self, n: V3, c: f64) -> Convex {
        let eps = 1e-12;
        // A face lying in the plane already is the cap (outward normal along n), or the body is
        // on the far side (opposite normal) and nothing is left; a second cap would double it.
        let mut has_cap = false;
        for f in &self.faces {
            if f.iter().all(|p| (n.dot(*p) - c).abs() <= eps) {
                let nf = (0..f.len()).fold(v3(0.0, 0.0, 0.0), |s, k| {
                    s + f[k].cross(f[(k + 1) % f.len()])
                });
                if nf.dot(n) < 0.0 {
                    return Convex { faces: vec![] };
                }
                has_cap = true;
            }
        }
        let mut faces = vec![];
        let mut cut: Vec<V3> = vec![];
        for f in &self.faces {
            let mut out = vec![];
            for k in 0..f.len() {
                let (p, q) = (f[k], f[(k + 1) % f.len()]);
                let (dp, dq) = (n.dot(p) - c, n.dot(q) - c);
                if dp <= eps {
                    out.push(p);
                }
                if (dp < -eps && dq > eps) || (dp > eps && dq < -eps) {
                    let x = p + (q - p) * (dp / (dp - dq));
                    out.push(x);
                    cut.push(x);
                } else if dp.abs() <= eps {
                    cut.push(p);
                }
            }
            if out.len() >= 3 {
                faces.push(out);
            }
        }
        if cut.len() >= 3 && !has_cap {
            // cap: points on the plane ordered by angle around the center, counter-clockwise looking against n
            let o = cut.iter().fold(v3(0.0, 0.0, 0.0), |s, p| s + *p) * (1.0 / cut.len() as f64);
            let e1 = {
                let t = if n.x.abs() < 0.9 {
                    v3(1.0, 0.0, 0.0)
                } else {
                    v3(0.0, 1.0, 0.0)
                };
                n.cross(t).normalized()
            };
            let e2 = n.cross(e1);
            let mut pts: Vec<(f64, V3)> = cut
                .iter()
                .map(|p| {
                    let d = *p - o;
                    (d.dot(e2).atan2(d.dot(e1)), *p)
                })
                .collect();
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            pts.dedup_by(|a, b| (a.1 - b.1).len() < 1e-9);
            if pts.len() >= 3 {
                faces.push(pts.into_iter().map(|x| x.1).collect());
            }
        }
        Convex { faces }
    }
    /// Intersection volume of two convex bodies.
    pub fn intersection_volume(&self, o: &Convex) -> f64 {
        self.intersection(o).map_or(0.0, |c| c.volume().max(0.0))
    }
    /// Intersection body; `None` if the bodies do not intersect.
    pub fn intersection(&self, o: &Convex) -> Option<Convex> {
        let mut c = self.clone();
        for (n, d) in o.planes() {
            c = c.clip(n, d);
            if c.faces.len() < 4 {
                return None;
            }
        }
        Some(c)
    }
}

/// Obstacle or network run body: signed convex pieces.
#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    pub pieces: Vec<(Convex, f64)>,
    pub aabb: Aabb,
}

impl Body {
    pub fn new(pieces: Vec<(Convex, f64)>) -> Body {
        let aabb = pieces
            .iter()
            .filter(|p| p.1 > 0.0)
            .map(|p| p.0.aabb())
            .reduce(|a, b| a.union(&b))
            .unwrap_or(Aabb {
                min: v3(0.0, 0.0, 0.0),
                max: v3(0.0, 0.0, 0.0),
            });
        Body { pieces, aabb }
    }
    pub fn solid(c: Convex) -> Body {
        Body::new(vec![(c, 1.0)])
    }
    /// Prism of a polygon with holes: pieces are contour triangles.
    pub fn prism_with_holes(outer: &[V2], holes: &[Vec<V2>], z0: f64, z1: f64) -> Body {
        let mut pieces = vec![];
        for (ring, sign) in
            std::iter::once((outer, 1.0)).chain(holes.iter().map(|h| (&h[..], -1.0)))
        {
            for t in crate::poly::convex_pieces(ring) {
                pieces.push((Convex::prism(&t, z0, z1), sign));
            }
        }
        Body::new(pieces)
    }
    pub fn intersection_volume(&self, o: &Body) -> f64 {
        if !self.aabb.overlaps(&o.aabb) {
            return 0.0;
        }
        let mut v = 0.0;
        for (a, sa) in &self.pieces {
            let ba = a.aabb();
            for (b, sb) in &o.pieces {
                if ba.overlaps(&b.aabb()) {
                    v += sa * sb * a.intersection_volume(b);
                }
            }
        }
        v.max(0.0)
    }
}

impl Body {
    /// Intersection volume and bounding box (over + bodies); the box is `None` if there is no volume.
    pub fn intersection_bounds(&self, o: &Body) -> (f64, Option<Aabb>) {
        if !self.aabb.overlaps(&o.aabb) {
            return (0.0, None);
        }
        let (mut v, mut bounds): (f64, Option<Aabb>) = (0.0, None);
        for (a, sa) in &self.pieces {
            let ba = a.aabb();
            for (b, sb) in &o.pieces {
                if !ba.overlaps(&b.aabb()) {
                    continue;
                }
                if let Some(c) = a.intersection(b) {
                    let cv = c.volume();
                    v += sa * sb * cv;
                    if *sa > 0.0 && *sb > 0.0 && cv > 1e-12 {
                        let cb = c.aabb();
                        bounds = Some(bounds.map_or(cb, |x| x.union(&cb)));
                    }
                }
            }
        }
        (v.max(0.0), bounds)
    }
}

/// BVH over bounding boxes: leaves are indices.
pub struct Bvh {
    nodes: Vec<Node>,
    items: Vec<usize>,
}

struct Node {
    b: Aabb,
    /// Leaf: (start, count) in `items`; otherwise children.
    kind: (usize, usize, bool),
}

impl Bvh {
    pub fn build(boxes: &[Aabb]) -> Bvh {
        let mut t = Bvh {
            nodes: vec![],
            items: (0..boxes.len()).collect(),
        };
        if !boxes.is_empty() {
            let n = boxes.len();
            t.split(boxes, 0, n);
        }
        t
    }
    fn split(&mut self, boxes: &[Aabb], lo: usize, hi: usize) -> usize {
        let b = self.items[lo..hi]
            .iter()
            .map(|&i| boxes[i])
            .reduce(|a, c| a.union(&c))
            .expect("non-empty");
        let id = self.nodes.len();
        self.nodes.push(Node {
            b,
            kind: (lo, hi - lo, true),
        });
        if hi - lo <= 4 {
            return id;
        }
        let ext = b.max - b.min;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let mid = (lo + hi) / 2;
        self.items[lo..hi].select_nth_unstable_by(mid - lo, |&a, &c| {
            boxes[a].center(axis).total_cmp(&boxes[c].center(axis))
        });
        let l = self.split(boxes, lo, mid);
        let r = self.split(boxes, mid, hi);
        self.nodes[id].kind = (l, r, false);
        id
    }
    /// Indices with overlapping bounding boxes.
    pub fn query(&self, q: &Aabb, out: &mut Vec<usize>) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            if !n.b.overlaps(q) {
                continue;
            }
            let (a, b, leaf) = n.kind;
            if leaf {
                out.extend(self.items[a..a + b].iter().copied());
            } else {
                stack.push(a);
                stack.push(b);
            }
        }
    }
}

/// Clash: indices into sets `a` (network) and `b` (structure), volume, m³.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub a: usize,
    pub b: usize,
    pub volume: f64,
}

/// All body pairs `a x b` with intersection volume above `min_volume`; sorted by (a, b).
pub fn clashes(a: &[Body], b: &[Body], min_volume: f64) -> Vec<Hit> {
    let tree = Bvh::build(&b.iter().map(|x| x.aabb).collect::<Vec<_>>());
    let mut out = vec![];
    let mut cand = vec![];
    for (i, x) in a.iter().enumerate() {
        cand.clear();
        tree.query(&x.aabb, &mut cand);
        cand.sort_unstable();
        for &j in &cand {
            let v = x.intersection_volume(&b[j]);
            if v > min_volume {
                out.push(Hit {
                    a: i,
                    b: j,
                    volume: v,
                });
            }
        }
    }
    out
}

/// Wall plan rectangle along axis a→b with thickness `t` (axis offset `off` to the left). A wall of zero
/// length (or with non-finite data) has no plan: an empty contour, which the caller skips.
pub fn wall_footprint(a: V2, b: V2, t: f64, off: f64) -> Vec<V2> {
    let d = b - a;
    let l = d.len();
    if !(l > 1e-9) || !(t.is_finite() && off.is_finite()) {
        return vec![];
    }
    let n = v2(-d.y / l, d.x / l);
    let (l0, l1) = (off - t / 2.0, off + t / 2.0);
    vec![a + n * l0, b + n * l0, b + n * l1, a + n * l1]
}
