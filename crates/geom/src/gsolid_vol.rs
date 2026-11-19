//! Volume of the intersection of a closed mesh with a convex polyhedron, and of two closed meshes,
//! without building the boolean mesh. By the divergence theorem with apex point o:
//! V(M ∩ C) = Σ_t V(cone o, t ∩ C) + Σ_i V(cone o, (plane i ∩ M) ∩ C), where the second term is
//! the section of M by the plane of a face of C, assembled as a fan of oriented section segments.
//! Mesh ∩ mesh: B is decomposed into signed tetrahedra from the apex point.

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;

/// Half-space n·p ≤ d (n is a unit vector).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Half {
    pub n: V3,
    pub d: f64,
}

impl Half {
    /// Through point `p` with outward normal `n` (any length); None for a degenerate normal.
    pub fn through(p: V3, n: V3) -> Option<Half> {
        let l = n.len();
        if !(l.is_finite() && l > 1e-300) {
            return None;
        }
        let n = n * (1.0 / l);
        Some(Half { n, d: n.dot(p) })
    }
    fn s(&self, p: V3) -> f64 {
        self.n.dot(p) - self.d
    }
}

/// Half-spaces of a convex prism: convex plan (either winding) × [z0, z1].
pub fn prism_halves(ring: &[V2], z0: f64, z1: f64) -> Vec<Half> {
    let k = ring.len();
    let s = if crate::poly::area(ring) < 0.0 {
        -1.0
    } else {
        1.0
    };
    let mut out = Vec::with_capacity(k + 2);
    for i in 0..k {
        let (a, b) = (ring[i], ring[(i + 1) % k]);
        let d = (b - a) * s;
        if let Some(h) = Half::through(v3(a.x, a.y, 0.0), v3(d.y, -d.x, 0.0)) {
            out.push(h);
        }
    }
    out.push(Half {
        n: v3(0.0, 0.0, 1.0),
        d: z1,
    });
    out.push(Half {
        n: v3(0.0, 0.0, -1.0),
        d: -z0,
    });
    out
}

/// Inward face offset: mesh faces coinciding with a cutter face do not land exactly on it.
const SHRINK: f64 = 1e-9;

fn det(a: V3, b: V3, c: V3) -> f64 {
    a.dot(b.cross(c))
}

/// Clipping a polygon by a half-space: inside means strictly s < 0.
fn clip(poly: &[V3], h: &Half) -> Vec<V3> {
    let k = poly.len();
    let mut out = Vec::with_capacity(k + 2);
    for i in 0..k {
        let (p, q) = (poly[i], poly[(i + 1) % k]);
        let (sp, sq) = (h.s(p), h.s(q));
        if sp < 0.0 {
            out.push(p);
        }
        if (sp < 0.0) != (sq < 0.0) {
            out.push(p + (q - p) * (sp / (sp - sq)));
        }
    }
    out
}

fn clip_all(mut poly: Vec<V3>, hs: &[Half], skip: Option<usize>) -> Vec<V3> {
    for (i, h) in hs.iter().enumerate() {
        if Some(i) == skip {
            continue;
        }
        if poly.len() < 3 {
            return vec![];
        }
        poly = clip(&poly, h);
    }
    poly
}

/// Volume of the cone from `o` over a planar polygon (sign follows the winding).
fn fan(o: V3, poly: &[V3]) -> f64 {
    if poly.len() < 3 {
        return 0.0;
    }
    let a = poly[0] - o;
    (1..poly.len() - 1)
        .map(|i| det(a, poly[i] - o, poly[i + 1] - o))
        .sum::<f64>()
        / 6.0
}

fn mesh_box(m: &Mesh) -> (V3, V3) {
    crate::gsolid::bbox(m)
}

/// The bounding box intersects the half-spaces (conservative test over the box corners).
fn box_outside(lo: V3, hi: V3, h: &Half) -> bool {
    let c = v3(
        if h.n.x > 0.0 { lo.x } else { hi.x },
        if h.n.y > 0.0 { lo.y } else { hi.y },
        if h.n.z > 0.0 { lo.z } else { hi.z },
    );
    h.s(c) >= 0.0
}

fn box_inside(lo: V3, hi: V3, h: &Half) -> bool {
    let c = v3(
        if h.n.x > 0.0 { hi.x } else { lo.x },
        if h.n.y > 0.0 { hi.y } else { lo.y },
        if h.n.z > 0.0 { hi.z } else { lo.z },
    );
    h.s(c) < 0.0
}

/// Volume of the part of a closed, consistently oriented mesh inside the intersection of half-spaces.
pub fn clip_volume(m: &Mesh, halves: &[Half]) -> f64 {
    if m.triangles.is_empty() {
        return 0.0;
    }
    let hs: Vec<Half> = halves
        .iter()
        .enumerate()
        .map(|(i, h)| Half {
            n: h.n,
            d: h.d - SHRINK * (1.0 + 0.137 * i as f64),
        })
        .collect();
    let (lo, hi) = mesh_box(m);
    if hs.iter().any(|h| box_outside(lo, hi, h)) {
        return 0.0;
    }
    if hs.iter().all(|h| box_inside(lo, hi, h)) {
        return m.volume();
    }
    let o = (lo + hi) * 0.5;
    let pos = &m.positions;
    // 1) mesh faces inside the cutter
    let mut v = 0.0;
    for t in &m.triangles {
        let p = t.map(|i| pos[i as usize]);
        let s: Vec<[f64; 3]> = hs.iter().map(|h| p.map(|q| h.s(q))).collect();
        if s.iter().any(|x| x.iter().all(|&y| y >= 0.0)) {
            continue;
        }
        if s.iter().all(|x| x.iter().all(|&y| y < 0.0)) {
            v += det(p[0] - o, p[1] - o, p[2] - o) / 6.0;
            continue;
        }
        v += fan(o, &clip_all(p.to_vec(), &hs, None));
    }
    // 2) sections of the mesh by the cutter face planes, clipped to the remaining faces
    for (i, h) in hs.iter().enumerate() {
        if box_outside(lo, hi, h) || box_inside(lo, hi, h) {
            continue;
        }
        let apex = o - h.n * h.s(o);
        for t in &m.triangles {
            let p = t.map(|k| pos[k as usize]);
            let s = p.map(|q| h.s(q));
            let mut pts: Vec<V3> = vec![];
            for k in 0..3 {
                let (a, b) = (k, (k + 1) % 3);
                if (s[a] < 0.0) != (s[b] < 0.0) {
                    let (x, y) = if t[a] < t[b] { (a, b) } else { (b, a) };
                    pts.push(p[x] + (p[y] - p[x]) * (s[x] / (s[x] - s[y])));
                }
            }
            if pts.len() != 2 {
                continue;
            }
            let nt = (p[1] - p[0]).cross(p[2] - p[0]);
            let dir = h.n.cross(nt);
            let (a, b) = if (pts[1] - pts[0]).dot(dir) >= 0.0 {
                (pts[0], pts[1])
            } else {
                (pts[1], pts[0])
            };
            // fan triangle (apex, a, b) is counterclockwise viewed from +n: outward from M∩C
            v += fan(o, &clip_all(vec![apex, a, b], &hs, Some(i)));
        }
    }
    v
}

/// Volume of the intersection of two closed, consistently oriented meshes.
pub fn intersect_volume(a: &Mesh, b: &Mesh) -> f64 {
    if a.triangles.is_empty() || b.triangles.is_empty() {
        return 0.0;
    }
    let (alo, ahi) = mesh_box(a);
    let (blo, bhi) = mesh_box(b);
    let apart = alo.x > bhi.x
        || blo.x > ahi.x
        || alo.y > bhi.y
        || blo.y > ahi.y
        || alo.z > bhi.z
        || blo.z > ahi.z;
    if apart {
        return 0.0;
    }
    // apex point is the center of the common part of the bounding boxes
    let r = v3(
        (alo.x.max(blo.x) + ahi.x.min(bhi.x)) / 2.0,
        (alo.y.max(blo.y) + ahi.y.min(bhi.y)) / 2.0,
        (alo.z.max(blo.z) + ahi.z.min(bhi.z)) / 2.0,
    );
    let mut v = 0.0;
    for t in &b.triangles {
        let [p0, p1, p2] = t.map(|i| b.positions[i as usize]);
        let d = det(p0 - r, p1 - r, p2 - r);
        if d.abs() < 1e-18 {
            continue;
        }
        let pts = [r, p0, p1, p2];
        let faces = [[1, 2, 3, 0], [0, 2, 3, 1], [0, 1, 3, 2], [0, 1, 2, 3]];
        let mut hs = Vec::with_capacity(4);
        for f in faces {
            let (q0, q1, q2, q3) = (pts[f[0]], pts[f[1]], pts[f[2]], pts[f[3]]);
            let mut n = (q1 - q0).cross(q2 - q0);
            if n.dot(q3 - q0) > 0.0 {
                n = n * -1.0;
            }
            if let Some(h) = Half::through(q0, n) {
                hs.push(h);
            }
        }
        if hs.len() == 4 {
            v += d.signum() * clip_volume(a, &hs);
        }
    }
    v
}

/// Volume of the part of the mesh inside prism `rings` (outer ring + holes, plan) × [z0, z1].
pub fn prism_overlap(m: &Mesh, rings: &[Vec<V2>], z0: f64, z1: f64) -> f64 {
    if z1 <= z0 {
        return 0.0;
    }
    let Some((outer, holes)) = rings.split_first() else {
        return 0.0;
    };
    let mut o = outer.clone();
    if crate::poly::area(&o) < 0.0 {
        o.reverse();
    }
    let hs: Vec<Vec<V2>> = holes
        .iter()
        .map(|h| {
            let mut h = h.clone();
            if crate::poly::area(&h) > 0.0 {
                h.reverse();
            }
            h
        })
        .collect();
    let all: Vec<V2> = o.iter().chain(hs.iter().flatten()).copied().collect();
    crate::gsolid::cap(&o, &hs)
        .into_iter()
        .map(|t| {
            let tri = [all[t[0]], all[t[1]], all[t[2]]];
            clip_volume(m, &prism_halves(&tri, z0, z1))
        })
        .sum()
}

// ---------- boolean volumes by sections ----------

/// Solid for boolean estimates: point membership is even-odd over the section at its height.
#[derive(Clone, Copy, Debug)]
pub enum Body<'a> {
    /// Plan (pieces and holes, even-odd over all rings) × [z0, z1].
    Prism {
        rings: &'a [Vec<V2>],
        z0: f64,
        z1: f64,
    },
    /// Closed mesh.
    Mesh(&'a Mesh),
}

const HOST: u8 = 0;
const MINUS: u8 = 1;
const VOID_A: u8 = 2;
const VOID_B: u8 = 3;

struct Prepared<'a> {
    body: Body<'a>,
    role: u8,
    lo: V3,
    hi: V3,
}

impl<'a> Prepared<'a> {
    fn new(body: Body<'a>, role: u8) -> Self {
        let (lo, hi) = match body {
            Body::Prism { rings, z0, z1 } => {
                let mut lo = v3(f64::INFINITY, f64::INFINITY, z0);
                let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, z1);
                for p in rings.iter().flatten() {
                    lo = v3(lo.x.min(p.x), lo.y.min(p.y), z0);
                    hi = v3(hi.x.max(p.x), hi.y.max(p.y), z1);
                }
                (lo, hi)
            }
            Body::Mesh(m) => mesh_box(m),
        };
        Prepared { body, role, lo, hi }
    }

    fn meets(&self, lo: V3, hi: V3) -> bool {
        self.lo.x < hi.x
            && lo.x < self.hi.x
            && self.lo.y < hi.y
            && lo.y < self.hi.y
            && self.lo.z < hi.z
            && lo.z < self.hi.z
    }

    fn z_events(&self, out: &mut Vec<f64>) {
        match self.body {
            Body::Prism { z0, z1, .. } => out.extend([z0, z1]),
            Body::Mesh(m) => out.extend(m.positions.iter().map(|p| p.z)),
        }
    }

    /// Section segments at height `z` tagged with label `k`.
    fn segs(&self, k: usize, z: f64, out: &mut Vec<(usize, V2, V2)>) {
        if z <= self.lo.z || z >= self.hi.z {
            return;
        }
        match self.body {
            Body::Prism { rings, .. } => {
                for r in rings {
                    let n = r.len();
                    for i in 0..n {
                        out.push((k, r[i], r[(i + 1) % n]));
                    }
                }
            }
            Body::Mesh(m) => {
                for (a, b) in crate::poly::slice(m, v3(0.0, 0.0, 1.0), z) {
                    out.push((k, v2(a.x, a.y), v2(b.x, b.y)));
                }
            }
        }
    }
}

fn union_box(ps: &[Prepared]) -> Option<(V3, V3)> {
    let mut it = ps.iter();
    let f = it.next()?;
    let (mut lo, mut hi) = (f.lo, f.hi);
    for p in it {
        lo = v3(lo.x.min(p.lo.x), lo.y.min(p.lo.y), lo.z.min(p.lo.z));
        hi = v3(hi.x.max(p.hi.x), hi.y.max(p.hi.y), hi.z.max(p.hi.z));
    }
    Some((lo, hi))
}

/// y of the segment intersection point, if there is one.
fn cross_y(a: V2, b: V2, c: V2, d: V2) -> Option<f64> {
    let r = b - a;
    let s = d - c;
    let den = r.x * s.y - r.y * s.x;
    if den.abs() < 1e-300 {
        return None;
    }
    let w = c - a;
    let t = (w.x * s.y - w.y * s.x) / den;
    let u = (w.x * r.y - w.y * r.x) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| a.y + r.y * t)
}

/// Area of plan points that have a base, no deduction, and a void (and void `B` if
/// `need_b`). The x-length on line y is linear between events (endpoints and pairwise crossings
/// of segments of different solids), so the midpoint of each interval gives the exact integral.
fn area2d(segs: &[(usize, V2, V2)], roles: &[u8], need_b: bool) -> f64 {
    if segs.is_empty() {
        return 0.0;
    }
    let mut ys: Vec<f64> = Vec::with_capacity(segs.len() * 2);
    for s in segs {
        ys.push(s.1.y);
        ys.push(s.2.y);
    }
    for i in 0..segs.len() {
        let (ki, a, b) = segs[i];
        let (ax0, ax1) = (a.x.min(b.x), a.x.max(b.x));
        let (ay0, ay1) = (a.y.min(b.y), a.y.max(b.y));
        for s in &segs[i + 1..] {
            if s.0 == ki {
                continue;
            }
            let (c, d) = (s.1, s.2);
            if c.x.max(d.x) < ax0 || c.x.min(d.x) > ax1 || c.y.max(d.y) < ay0 || c.y.min(d.y) > ay1
            {
                continue;
            }
            if let Some(y) = cross_y(a, b, c, d) {
                ys.push(y);
            }
        }
    }
    ys.sort_by(f64::total_cmp);
    ys.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    let mut xs: Vec<(f64, usize)> = vec![];
    let mut inside = vec![false; roles.len()];
    let mut area = 0.0;
    for w in ys.windows(2) {
        let dy = w[1] - w[0];
        if dy < 1e-12 {
            continue;
        }
        let y = (w[0] + w[1]) * 0.5;
        xs.clear();
        for &(k, a, b) in segs {
            if (a.y > y) != (b.y > y) {
                xs.push((a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y), k));
            }
        }
        xs.sort_by(|p, q| p.0.total_cmp(&q.0));
        inside.iter_mut().for_each(|f| *f = false);
        let mut cnt = [0i32; 4];
        let mut len = 0.0;
        for i in 0..xs.len() {
            let (x, k) = xs[i];
            inside[k] = !inside[k];
            cnt[roles[k] as usize] += if inside[k] { 1 } else { -1 };
            let on = cnt[HOST as usize] > 0
                && cnt[MINUS as usize] == 0
                && cnt[VOID_A as usize] > 0
                && (!need_b || cnt[VOID_B as usize] > 0);
            if on {
                if let Some(nx) = xs.get(i + 1) {
                    len += nx.0 - x;
                }
            }
        }
        area += len * dy;
    }
    area
}

/// Selected solids: bases, voids at the base, deductions at the common part; z-range of the common part.
struct Bool<'a> {
    bodies: Vec<Prepared<'a>>,
    roles: Vec<u8>,
    z0: f64,
    z1: f64,
}

impl<'a> Bool<'a> {
    fn new(host: &[Body<'a>], minus: &[Body<'a>], voids: &[Body<'a>]) -> Option<Self> {
        let hs: Vec<Prepared> = host.iter().map(|b| Prepared::new(*b, HOST)).collect();
        let (hlo, hhi) = union_box(&hs)?;
        let vs: Vec<Prepared> = voids
            .iter()
            .map(|b| Prepared::new(*b, VOID_A))
            .filter(|p| p.meets(hlo, hhi))
            .collect();
        let (vlo, vhi) = union_box(&vs)?;
        let lo = v3(hlo.x.max(vlo.x), hlo.y.max(vlo.y), hlo.z.max(vlo.z));
        let hi = v3(hhi.x.min(vhi.x), hhi.y.min(vhi.y), hhi.z.min(vhi.z));
        if lo.x >= hi.x || lo.y >= hi.y || lo.z >= hi.z {
            return None;
        }
        let ms = minus
            .iter()
            .map(|b| Prepared::new(*b, MINUS))
            .filter(|p| p.meets(lo, hi));
        let bodies: Vec<Prepared> = hs.into_iter().chain(vs).chain(ms).collect();
        let roles = bodies.iter().map(|p| p.role).collect();
        Some(Bool {
            bodies,
            roles,
            z0: lo.z,
            z1: hi.z,
        })
    }

    fn area(&self, z: f64) -> f64 {
        let mut segs = vec![];
        for (k, b) in self.bodies.iter().enumerate() {
            b.segs(k, z, &mut segs);
        }
        area2d(&segs, &self.roles, false)
    }
}

/// Gauss–Legendre, 3 nodes inside the interval (exact for cubics; section area is piecewise quadratic).
fn gauss3(f: &dyn Fn(f64) -> f64, a: f64, b: f64) -> f64 {
    const X: f64 = 0.774_596_669_241_483_4;
    let (c, h) = ((a + b) * 0.5, (b - a) * 0.5);
    h * (5.0 * f(c - h * X) + 8.0 * f(c) + 5.0 * f(c + h * X)) / 9.0
}

fn adaptive(f: &dyn Fn(f64) -> f64, a: f64, b: f64, whole: f64, depth: u32) -> f64 {
    let m = (a + b) * 0.5;
    let (l, r) = (gauss3(f, a, m), gauss3(f, m, b));
    if depth == 0 || (l + r - whole).abs() <= 1e-11 + 1e-9 * (l + r).abs() {
        return l + r;
    }
    adaptive(f, a, m, l, depth - 1) + adaptive(f, m, b, r, depth - 1)
}

/// Volume (∪bases) \ (∪deductions) ∩ (∪voids): overlapping voids are counted once, volume
/// already removed by openings/shafts/columns (`minus`) is not subtracted again. Solids are closed.
pub fn boolean_volume(host: &[Body], minus: &[Body], voids: &[Body]) -> f64 {
    let Some(b) = Bool::new(host, minus, voids) else {
        return 0.0;
    };
    let mut zs = vec![b.z0, b.z1];
    for p in &b.bodies {
        p.z_events(&mut zs);
    }
    zs.retain(|z| *z >= b.z0 && *z <= b.z1);
    zs.sort_by(f64::total_cmp);
    zs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let f = |z: f64| b.area(z);
    zs.windows(2)
        .filter(|w| w[1] - w[0] > 1e-9)
        .map(|w| adaptive(&f, w[0], w[1], gauss3(&f, w[0], w[1]), 6))
        .sum::<f64>()
        .max(0.0)
}

/// Plan area of a base of thickness [z0, z1] that voids cut straight through (present at both the
/// top and bottom face), minus `minus`.
pub fn through_area(host: &[Body], minus: &[Body], voids: &[Body], z0: f64, z1: f64) -> f64 {
    if z1 - z0 <= 1e-9 {
        return 0.0;
    }
    let Some(b) = Bool::new(host, minus, voids) else {
        return 0.0;
    };
    let d = ((z1 - z0) * 1e-3).min(1e-4);
    let (zt, zb, zm) = (z1 - d, z0 + d, (z0 + z1) * 0.5);
    let mut segs = vec![];
    let mut roles = b.roles.clone();
    for (k, p) in b.bodies.iter().enumerate() {
        if p.role == VOID_A {
            p.segs(k, zt, &mut segs);
            let kb = roles.len();
            roles.push(VOID_B);
            p.segs(kb, zb, &mut segs);
        } else {
            p.segs(k, zm, &mut segs);
        }
    }
    area2d(&segs, &roles, true)
}
