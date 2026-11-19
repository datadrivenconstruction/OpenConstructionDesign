// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Editing of imported meshes: welding and cleanup, merging, face removal, affine
//! transforms, exact plane section (contours with holes and open polylines),
//! plane clip and split with caps, closedness/convexity, area and volume.
//!
//! The section is topological: a point is a mesh vertex on the plane or an edge with a sign change;
//! welding splits T-junctions, and remaining gaps are stitched by distance. An edge lying in the
//! plane belongs to the triangle above it (half-open rule: a solid resting on the plane is cut at its bottom).

use std::collections::HashMap;

use crate::math::{v2, v3, V2, V3};
use crate::mesh::Mesh;
use crate::profile::signed_area;

/// Plane `n·p = d`, `n` is a unit vector.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    pub n: V3,
    pub d: f64,
}

impl Plane {
    /// Plane with normal `n` through point `p`; None for a zero or non-finite normal.
    pub fn new(n: V3, p: V3) -> Option<Plane> {
        let l = n.len();
        if !(l > 1e-12 && l.is_finite()) || ![p.x, p.y, p.z].iter().all(|v| v.is_finite()) {
            return None;
        }
        let n = n * (1.0 / l);
        Some(Plane { n, d: n.dot(p) })
    }

    pub fn horizontal(z: f64) -> Plane {
        Plane {
            n: v3(0.0, 0.0, 1.0),
            d: z,
        }
    }

    /// Vertical plane through a→b; the positive side is to the right of a→b.
    pub fn vertical(a: V2, b: V2) -> Option<Plane> {
        let d = b - a;
        Plane::new(v3(d.y, -d.x, 0.0), v3(a.x, a.y, 0.0))
    }

    pub fn flipped(&self) -> Plane {
        Plane {
            n: -self.n,
            d: -self.d,
        }
    }

    pub fn dist(&self, p: V3) -> f64 {
        self.n.dot(p) - self.d
    }

    /// Basis (origin, u, v), u × v = n: for a vertical plane v points up, for a horizontal
    /// plane u, v are the plan x, y axes.
    pub fn frame(&self) -> (V3, V3, V3) {
        let n = self.n;
        let up = if n.z.abs() > 0.999 {
            v3(0.0, 1.0, 0.0)
        } else {
            v3(0.0, 0.0, 1.0)
        };
        let v = (up - n * up.dot(n)).normalized();
        let u = v.cross(n);
        (n * self.d, u, v)
    }

    pub fn to_2d(&self, p: V3) -> V2 {
        let (o, u, v) = self.frame();
        v2((p - o).dot(u), (p - o).dot(v))
    }

    pub fn to_3d(&self, q: V2) -> V3 {
        let (o, u, v) = self.frame();
        o + u * q.x + v * q.y
    }
}

/// Affine transform `p' = m·p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: [[f64; 3]; 3],
    pub t: V3,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: V3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    };

    pub fn translate(d: V3) -> Affine {
        Affine {
            t: d,
            ..Affine::IDENTITY
        }
    }

    /// Rotation about the vertical through `c` by `angle` rad (counter-clockwise in plan).
    pub fn rotate_z(c: V3, angle: f64) -> Affine {
        let (s, k) = angle.sin_cos();
        let m = [[k, -s, 0.0], [s, k, 0.0], [0.0, 0.0, 1.0]];
        Affine::about(c, m)
    }

    /// Per-axis scale about `c`.
    pub fn scale(c: V3, s: V3) -> Affine {
        Affine::about(c, [[s.x, 0.0, 0.0], [0.0, s.y, 0.0], [0.0, 0.0, s.z]])
    }

    /// Reflection in a plane.
    pub fn mirror(p: Plane) -> Affine {
        let n = [p.n.x, p.n.y, p.n.z];
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c = if i == j { 1.0 } else { 0.0 } - 2.0 * n[i] * n[j];
            }
        }
        Affine {
            m,
            t: p.n * (2.0 * p.d),
        }
    }

    fn about(c: V3, m: [[f64; 3]; 3]) -> Affine {
        let a = Affine {
            m,
            t: v3(0.0, 0.0, 0.0),
        };
        let mc = a.linear(c);
        Affine { m, t: c - mc }
    }

    fn linear(&self, p: V3) -> V3 {
        let m = &self.m;
        v3(
            m[0][0] * p.x + m[0][1] * p.y + m[0][2] * p.z,
            m[1][0] * p.x + m[1][1] * p.y + m[1][2] * p.z,
            m[2][0] * p.x + m[2][1] * p.y + m[2][2] * p.z,
        )
    }

    pub fn apply(&self, p: V3) -> V3 {
        self.linear(p) + self.t
    }

    /// Applies `self` first, then `o`.
    pub fn then(&self, o: &Affine) -> Affine {
        let mut m = [[0.0; 3]; 3];
        for (i, row) in m.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c = (0..3).map(|k| o.m[i][k] * self.m[k][j]).sum();
            }
        }
        Affine {
            m,
            t: o.linear(self.t) + o.t,
        }
    }

    pub fn det(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// Numbers are finite and the transform is non-degenerate (|det| > 1e-12).
    pub fn is_valid(&self) -> bool {
        self.m.iter().flatten().all(|v| v.is_finite())
            && [self.t.x, self.t.y, self.t.z].iter().all(|v| v.is_finite())
            && self.det().abs() > 1e-12
    }
}

/// Transformed mesh; under reflection (det < 0) the triangle winding is flipped.
pub fn transform(m: &Mesh, a: &Affine) -> Mesh {
    let flip = a.det() < 0.0;
    Mesh {
        positions: m.positions.iter().map(|&p| a.apply(p)).collect(),
        triangles: m
            .triangles
            .iter()
            .map(|t| if flip { [t[0], t[2], t[1]] } else { *t })
            .collect(),
        face_of: m.face_of.clone(),
    }
}

/// What the cleanup did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CleanReport {
    pub merged_vertices: usize,
    pub degenerate: usize,
    pub duplicate: usize,
    pub unused: usize,
    /// Edges split at T-junctions.
    pub t_junctions: usize,
}

fn face_of(m: &Mesh, i: usize) -> u32 {
    m.face_of.get(i).copied().unwrap_or(i as u32)
}

/// Welds vertices closer than `tol`, removes degenerate (including zero-area) and duplicate
/// (same orientation) triangles and unused vertices; edges with a neighboring face's vertex
/// on them (T-junctions) are split. Opposing pairs (double-sided faces) are kept.
pub fn clean(m: &Mesh, tol: f64) -> (Mesh, CleanReport) {
    let (mut out, mut rep) = clean_with(m, tol, true);
    rep.t_junctions = fix_t_junctions(&mut out, tol);
    (out, rep)
}

/// Welding for analysis (section, recognition, queries): zero-area "needle" triangles with three
/// distinct vertices are kept because they close T-junctions, and T-junctions are split.
pub fn weld(m: &Mesh, tol: f64) -> Mesh {
    let mut w = clean_with(m, tol, false).0;
    fix_t_junctions(&mut w, tol);
    w
}

fn clean_with(m: &Mesh, tol: f64, geometric: bool) -> (Mesh, CleanReport) {
    let tol = if tol.is_finite() && tol > 0.0 {
        tol
    } else {
        1e-9
    };
    let mut rep = CleanReport::default();
    let cell = |p: V3| {
        let q = |v: f64| (v / tol).floor() as i64;
        (q(p.x), q(p.y), q(p.z))
    };
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    let mut reps: Vec<V3> = vec![];
    let mut map = Vec::with_capacity(m.positions.len());
    for &p in &m.positions {
        let (cx, cy, cz) = cell(p);
        let mut found = None;
        'n: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(v) = grid.get(&(cx + dx, cy + dy, cz + dz)) {
                        if let Some(&r) = v.iter().find(|&&r| (reps[r as usize] - p).len() <= tol) {
                            found = Some(r);
                            break 'n;
                        }
                    }
                }
            }
        }
        let r = match found {
            Some(r) => {
                rep.merged_vertices += 1;
                r
            }
            None => {
                let r = reps.len() as u32;
                reps.push(p);
                grid.entry((cx, cy, cz)).or_default().push(r);
                r
            }
        };
        map.push(r);
    }
    let mut seen: HashMap<[u32; 3], ()> = HashMap::new();
    let mut tris = vec![];
    let mut faces = vec![];
    for (i, t) in m.triangles.iter().enumerate() {
        let Some(t) = t
            .iter()
            .map(|&k| map.get(k as usize).copied())
            .collect::<Option<Vec<u32>>>()
        else {
            rep.degenerate += 1;
            continue;
        };
        let t = [t[0], t[1], t[2]];
        let [a, b, c] = t.map(|k| reps[k as usize]);
        let flat = geometric && (b - a).cross(c - a).len() <= tol * tol;
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || flat {
            rep.degenerate += 1;
            continue;
        }
        // duplicate of the same orientation: identical cycle rotated to start at the smallest vertex
        let k = (0..3).min_by_key(|&j| t[j]).unwrap_or(0);
        let key = [t[k], t[(k + 1) % 3], t[(k + 2) % 3]];
        if seen.insert(key, ()).is_some() {
            rep.duplicate += 1;
            continue;
        }
        tris.push(t);
        faces.push(face_of(m, i));
    }
    let out = compact(reps, tris, faces);
    rep.unused = m.positions.len() - rep.merged_vertices - out.positions.len();
    (out, rep)
}

/// Boundary edges are split by boundary vertices lying on them (within `tol`): adjacent
/// faces were tessellated with different vertex sets on the shared edge. Returns the number of splits.
fn fix_t_junctions(m: &mut Mesh, tol: f64) -> usize {
    let tol = if tol.is_finite() && tol > 0.0 {
        tol
    } else {
        1e-9
    };
    let mut uses: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &m.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *uses.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let bnd: Vec<(u32, u32)> = uses.iter().filter(|e| *e.1 == 1).map(|e| *e.0).collect();
    if bnd.is_empty() || bnd.len() > 400_000 {
        return 0;
    }
    let p = &m.positions;
    let mean = bnd
        .iter()
        .map(|&(a, b)| (p[b as usize] - p[a as usize]).len())
        .sum::<f64>()
        / bnd.len() as f64;
    let cell = (mean * 2.0).max(tol * 10.0);
    let key = |q: V3| {
        let f = |v: f64| (v / cell).floor() as i64;
        (f(q.x), f(q.y), f(q.z))
    };
    let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();
    let mut verts: Vec<u32> = bnd.iter().flat_map(|e| [e.0, e.1]).collect();
    verts.sort_unstable();
    verts.dedup();
    for &v in &verts {
        grid.entry(key(p[v as usize])).or_default().push(v);
    }
    let mut splits: HashMap<(u32, u32), Vec<(f64, u32)>> = HashMap::new();
    for &(a, b) in &bnd {
        let (pa, pb) = (p[a as usize], p[b as usize]);
        let d = pb - pa;
        let l2 = d.dot(d);
        if l2 <= tol * tol {
            continue;
        }
        let l = l2.sqrt();
        let (k0, k1) = (
            key(v3(pa.x.min(pb.x), pa.y.min(pb.y), pa.z.min(pb.z)) - v3(tol, tol, tol)),
            key(v3(pa.x.max(pb.x), pa.y.max(pb.y), pa.z.max(pb.z)) + v3(tol, tol, tol)),
        );
        let cells = (k1.0 - k0.0 + 1) * (k1.1 - k0.1 + 1) * (k1.2 - k0.2 + 1);
        if cells > 20_000 {
            continue;
        }
        let mut on = vec![];
        for x in k0.0..=k1.0 {
            for y in k0.1..=k1.1 {
                for z in k0.2..=k1.2 {
                    for &v in grid.get(&(x, y, z)).map_or(&[][..], |g| &g[..]) {
                        if v == a || v == b {
                            continue;
                        }
                        let q = p[v as usize];
                        let t = (q - pa).dot(d) / l2;
                        if t * l > tol && (1.0 - t) * l > tol && (pa + d * t - q).len() <= tol {
                            on.push((t, v));
                        }
                    }
                }
            }
        }
        if !on.is_empty() {
            on.sort_by(|x, y| x.0.total_cmp(&y.0));
            on.dedup_by_key(|x| x.1);
            splits.insert((a, b), on);
        }
    }
    if splits.is_empty() {
        return 0;
    }
    let n = splits.values().map(Vec::len).sum();
    let mut tris = Vec::with_capacity(m.triangles.len() + 2 * n);
    let mut faces = Vec::with_capacity(tris.capacity());
    for (i, t) in m.triangles.iter().enumerate() {
        let f = face_of(m, i);
        let mut poly: Vec<u32> = vec![];
        let mut split_edges = 0;
        let mut last_split = 0;
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            poly.push(a);
            if let Some(on) = splits.get(&(a.min(b), a.max(b))) {
                split_edges += 1;
                last_split = k;
                if a < b {
                    poly.extend(on.iter().map(|x| x.1));
                } else {
                    poly.extend(on.iter().rev().map(|x| x.1));
                }
            }
        }
        match split_edges {
            0 => {
                tris.push(*t);
                faces.push(f);
            }
            1 => {
                // fan from the vertex opposite the split edge
                let apex = t[(last_split + 2) % 3];
                let s = poly.iter().position(|&v| v == apex).unwrap_or(0);
                let r: Vec<u32> = (0..poly.len())
                    .map(|j| poly[(s + j) % poly.len()])
                    .collect();
                for j in 1..r.len() - 1 {
                    tris.push([r[0], r[j], r[j + 1]]);
                    faces.push(f);
                }
            }
            _ => {
                // several edges are split: fan from the triangle center
                let c = (m.positions[t[0] as usize]
                    + m.positions[t[1] as usize]
                    + m.positions[t[2] as usize])
                    * (1.0 / 3.0);
                m.positions.push(c);
                let ci = (m.positions.len() - 1) as u32;
                for j in 0..poly.len() {
                    tris.push([ci, poly[j], poly[(j + 1) % poly.len()]]);
                    faces.push(f);
                }
            }
        }
    }
    m.triangles = tris;
    m.face_of = faces;
    n
}

fn compact(positions: Vec<V3>, tris: Vec<[u32; 3]>, faces: Vec<u32>) -> Mesh {
    let mut map = vec![u32::MAX; positions.len()];
    let mut out = vec![];
    let triangles = tris
        .into_iter()
        .map(|t| {
            t.map(|k| {
                let k = k as usize;
                if map[k] == u32::MAX {
                    map[k] = out.len() as u32;
                    out.push(positions[k]);
                }
                map[k]
            })
        })
        .collect();
    Mesh {
        positions: out,
        triangles,
        face_of: faces,
    }
}

/// Merges several meshes into one; face ids do not overlap.
pub fn merge(ms: &[&Mesh]) -> Mesh {
    let mut out = Mesh::default();
    let mut face0 = 0u32;
    for m in ms {
        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&m.positions);
        let mut top = 0u32;
        for (i, t) in m.triangles.iter().enumerate() {
            out.triangles.push(t.map(|k| k + base));
            let f = face_of(m, i);
            top = top.max(f + 1);
            out.face_of.push(face0 + f);
        }
        face0 += top;
    }
    out
}

/// Mesh without triangles `tris`; vertices left without triangles are dropped.
pub fn delete_faces(m: &Mesh, tris: &[usize]) -> Mesh {
    let drop: std::collections::HashSet<usize> = tris.iter().copied().collect();
    let mut t = vec![];
    let mut f = vec![];
    for (i, tri) in m.triangles.iter().enumerate() {
        if !drop.contains(&i) {
            t.push(*tri);
            f.push(face_of(m, i));
        }
    }
    compact(m.positions.clone(), t, f)
}

/// Mesh properties (after welding with tolerance `tol`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub vertices: usize,
    pub triangles: usize,
    pub area: f64,
    /// Signed: < 0 means normals point inward. Meaningful for a closed mesh.
    pub volume: f64,
    /// Every edge belongs to exactly two triangles.
    pub closed: bool,
    /// Closed and consistently oriented.
    pub oriented: bool,
    /// Convex (closed meshes only); None means not checked (too many faces).
    pub convex: Option<bool>,
    pub boundary_edges: usize,
    pub nonmanifold_edges: usize,
    pub min: V3,
    pub max: V3,
}

pub fn stats(m: &Mesh, tol: f64) -> Stats {
    let w = weld(m, tol);
    let mut edges: HashMap<(u32, u32), (u32, i32)> = HashMap::new();
    for t in &w.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let e = edges.entry((a.min(b), a.max(b))).or_default();
            e.0 += 1;
            e.1 += if a < b { 1 } else { -1 };
        }
    }
    let boundary = edges.values().filter(|e| e.0 == 1).count();
    let nonmanifold = edges.values().filter(|e| e.0 > 2).count();
    let closed = !w.triangles.is_empty() && boundary == 0 && nonmanifold == 0;
    let oriented = closed && edges.values().all(|e| e.1 == 0);
    let (mut lo, mut hi) = (
        v3(f64::MAX, f64::MAX, f64::MAX),
        v3(f64::MIN, f64::MIN, f64::MIN),
    );
    for p in &w.positions {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    if w.positions.is_empty() {
        lo = v3(0.0, 0.0, 0.0);
        hi = lo;
    }
    let volume = w.volume();
    let convex = if oriented {
        convexity(&w, volume < 0.0, tol.max(1e-9) * 10.0)
    } else {
        Some(false)
    };
    Stats {
        vertices: w.positions.len(),
        triangles: w.triangles.len(),
        area: w.area(),
        volume,
        closed,
        oriented,
        convex,
        boundary_edges: boundary,
        nonmanifold_edges: nonmanifold,
        min: lo,
        max: hi,
    }
}

/// No vertex lies outside any face plane (planes are merged within tolerance).
fn convexity(m: &Mesh, inverted: bool, tol: f64) -> Option<bool> {
    let mut planes: HashMap<(i64, i64, i64, i64), (V3, f64)> = HashMap::new();
    for t in &m.triangles {
        let [a, b, c] = t.map(|k| m.positions[k as usize]);
        let n = (b - a).cross(c - a);
        let l = n.len();
        if l <= 0.0 {
            continue;
        }
        let sgn = if inverted { -1.0 } else { 1.0 };
        let n = n * (sgn / l);
        let d = n.dot(a);
        let q = |v: f64, s: f64| (v * s).round() as i64;
        planes
            .entry((q(n.x, 1e5), q(n.y, 1e5), q(n.z, 1e5), q(d, 1.0 / tol)))
            .or_insert((n, d));
    }
    if planes.len().saturating_mul(m.positions.len()) > 50_000_000 {
        return None;
    }
    Some(
        planes
            .values()
            .all(|&(n, d)| m.positions.iter().all(|p| n.dot(*p) - d <= tol)),
    )
}

/// Section region: outer contour counter-clockwise, holes clockwise (in the plane basis).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Region {
    pub outer: Vec<V2>,
    pub holes: Vec<Vec<V2>>,
}

impl Region {
    pub fn area(&self) -> f64 {
        signed_area(&self.outer) + self.holes.iter().map(|h| signed_area(h)).sum::<f64>()
    }
}

/// Mesh section by a plane in the basis [`Plane::frame`].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PlaneSection {
    /// Closed contours grouped into outer contours with holes (section fill).
    pub regions: Vec<Region>,
    /// Open polylines (an open mesh is a surface without thickness).
    pub open: Vec<Vec<V2>>,
}

impl PlaneSection {
    pub fn area(&self) -> f64 {
        self.regions.iter().map(Region::area).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    V(u32),
    E(u32, u32),
}

struct Cutter<'a> {
    m: &'a Mesh,
    d: Vec<f64>,
    s: Vec<i8>,
}

impl<'a> Cutter<'a> {
    fn new(m: &'a Mesh, pl: &Plane, tol: f64) -> Self {
        let d: Vec<f64> = m.positions.iter().map(|&p| pl.dist(p)).collect();
        let s = d
            .iter()
            .map(|&x| {
                if x.abs() <= tol {
                    0
                } else if x > 0.0 {
                    1
                } else {
                    -1
                }
            })
            .collect();
        Cutter { m, d, s }
    }

    fn point(&self, k: Key) -> V3 {
        match k {
            Key::V(i) => self.m.positions[i as usize],
            Key::E(a, b) => {
                let (pa, pb) = (self.m.positions[a as usize], self.m.positions[b as usize]);
                let (da, db) = (self.d[a as usize], self.d[b as usize]);
                pa + (pb - pa) * (da / (da - db))
            }
        }
    }

    fn edge(a: u32, b: u32) -> Key {
        Key::E(a.min(b), a.max(b))
    }

    /// Oriented section segments: material on the left (outer contour counter-clockwise).
    fn segments(&self, n: V3) -> Vec<(Key, Key)> {
        let mut segs: HashMap<(Key, Key), ()> = HashMap::new();
        let mut order = vec![];
        for t in &self.m.triangles {
            let s = t.map(|i| self.s[i as usize]);
            if s == [0, 0, 0] {
                continue;
            }
            let mut ks: Vec<Key> = vec![];
            let mut zero_edge = false;
            for k in 0..3 {
                let (i, j) = (t[k], t[(k + 1) % 3]);
                let (si, sj) = (s[k], s[(k + 1) % 3]);
                if si == 0 {
                    ks.push(Key::V(i));
                    if sj == 0 {
                        zero_edge = true;
                    }
                }
                if si * sj < 0 {
                    ks.push(Self::edge(i, j));
                }
            }
            if ks.len() != 2 {
                continue;
            }
            if zero_edge && !s.contains(&1) {
                continue;
            }
            let [a, b, c] = t.map(|i| self.m.positions[i as usize]);
            let want = n.cross((b - a).cross(c - a));
            let (k0, k1) = (ks[0], ks[1]);
            let seg = if (self.point(k1) - self.point(k0)).dot(want) >= 0.0 {
                (k0, k1)
            } else {
                (k1, k0)
            };
            if seg.0 == seg.1 {
                continue;
            }
            if segs.remove(&(seg.1, seg.0)).is_some() {
                continue;
            }
            if segs.insert(seg, ()).is_none() {
                order.push(seg);
            }
        }
        let mut out = Vec::with_capacity(segs.len());
        for s in order {
            if segs.remove(&s).is_some() {
                out.push(s);
            }
        }
        out
    }
}

/// Segment chains by shared keys: (closed, open).
fn chains<K: Copy + Eq + std::hash::Hash>(segs: &[(K, K)]) -> (Vec<Vec<K>>, Vec<Vec<K>>) {
    let mut next: HashMap<K, Vec<usize>> = HashMap::new();
    let mut indeg: HashMap<K, usize> = HashMap::new();
    for (i, s) in segs.iter().enumerate() {
        next.entry(s.0).or_default().push(i);
        *indeg.entry(s.1).or_default() += 1;
    }
    let mut used = vec![false; segs.len()];
    let walk = |start: usize, used: &mut Vec<bool>| -> (Vec<K>, bool) {
        let mut out = vec![segs[start].0];
        used[start] = true;
        let mut cur = segs[start].1;
        loop {
            if cur == out[0] {
                return (out, true);
            }
            out.push(cur);
            let nx = next
                .get(&cur)
                .and_then(|v| v.iter().copied().find(|&j| !used[j]));
            match nx {
                Some(j) => {
                    used[j] = true;
                    cur = segs[j].1;
                }
                None => return (out, false),
            }
        }
    };
    let (mut closed, mut open) = (vec![], vec![]);
    // open chains start where nothing enters
    for i in 0..segs.len() {
        if !used[i] && indeg.get(&segs[i].0).copied().unwrap_or(0) == 0 {
            let (c, _) = walk(i, &mut used);
            open.push(c);
        }
    }
    for i in 0..segs.len() {
        if !used[i] {
            let (c, ok) = walk(i, &mut used);
            if ok && c.len() >= 3 {
                closed.push(c);
            } else if !ok {
                open.push(c);
            }
        }
    }
    (closed, open)
}

fn inside(q: V2, ring: &[V2]) -> bool {
    let mut c = false;
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + n - 1) % n]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

/// Contours to regions: positive ones are outer, negative ones are holes in the smallest
/// enclosing outer contour; a hole without a parent becomes an outer contour.
pub fn nest(loops: Vec<Vec<V2>>) -> Vec<Region> {
    let total: f64 = loops.iter().map(|l| signed_area(l)).sum();
    let mut loops = loops;
    if total < 0.0 {
        // mesh is inside-out: winding is reversed for all contours
        loops.iter_mut().for_each(|l| l.reverse());
    }
    let (mut outers, mut holes): (Vec<Vec<V2>>, Vec<Vec<V2>>) =
        loops.into_iter().partition(|l| signed_area(l) > 0.0);
    outers.sort_by(|a, b| signed_area(a).total_cmp(&signed_area(b)));
    let mut regions: Vec<Region> = outers
        .into_iter()
        .map(|o| Region {
            outer: o,
            holes: vec![],
        })
        .collect();
    for mut h in holes.drain(..) {
        let probe = (h[0] + h[1]) * 0.5;
        let host = regions.iter().position(|r| inside(probe, &r.outer));
        match host {
            Some(i) => regions[i].holes.push(h),
            None => {
                h.reverse();
                regions.push(Region {
                    outer: h,
                    holes: vec![],
                });
            }
        }
    }
    regions.sort_by(|a, b| signed_area(&b.outer).total_cmp(&signed_area(&a.outer)));
    regions
}

/// Removes vertices on the line between their neighbors (deviation <= `tol`) and duplicates.
pub fn simplify(ring: &[V2], tol: f64, closed: bool) -> Vec<V2> {
    let mut r: Vec<V2> = vec![];
    for &p in ring {
        if r.last().is_none_or(|q: &V2| (p - *q).len() > tol) {
            r.push(p);
        }
    }
    if closed && r.len() > 1 && (r[0] - r[r.len() - 1]).len() <= tol {
        r.pop();
    }
    let mut changed = true;
    while changed && r.len() > 3 {
        changed = false;
        let n = r.len();
        for i in 0..n {
            if !closed && (i == 0 || i == n - 1) {
                continue;
            }
            let (a, b, c) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
            let l = (c - a).len();
            if l > 0.0 && ((b - a).cross(c - a) / l).abs() <= tol && (b - a).dot(c - b) >= 0.0 {
                r.remove(i);
                changed = true;
                break;
            }
        }
    }
    r
}

/// Mesh section by a plane: regions (with holes) and open polylines in the plane basis.
/// `tol` is the vertex-welding and "on plane" tolerance.
pub fn section(m: &Mesh, pl: &Plane, tol: f64) -> PlaneSection {
    let w = weld(m, tol);
    section_welded(&w, pl, tol)
}

pub(crate) fn section_welded(w: &Mesh, pl: &Plane, tol: f64) -> PlaneSection {
    let c = Cutter::new(w, pl, tol);
    let segs = c.segments(pl.n);
    let (closed, open) = chains(&segs);
    let to2 = |k: &Key| pl.to_2d(c.point(*k));
    let eps = tol.max(1e-9);
    // topology did not close (cracks, unwelded vertices): endpoints are stitched by distance
    let (joined, open) = stitch(
        open.iter().map(|l| l.iter().map(to2).collect()).collect(),
        (tol * 100.0).max(1e-7),
    );
    let loops: Vec<Vec<V2>> = closed
        .iter()
        .map(|l| l.iter().map(to2).collect::<Vec<_>>())
        .chain(joined)
        .map(|l| simplify(&l, eps, true))
        .filter(|l| l.len() >= 3 && signed_area(l).abs() > eps * eps)
        .collect();
    PlaneSection {
        regions: nest(loops),
        open: open
            .iter()
            .map(|l| simplify(l, eps, false))
            .filter(|l| l.len() >= 2)
            .collect(),
    }
}

/// Open polylines (in winding direction) whose ends lie within `eps` of another's start are
/// joined into closed contours. Returns (closed, remaining open).
fn stitch(chains: Vec<Vec<V2>>, eps: f64) -> (Vec<Vec<V2>>, Vec<Vec<V2>>) {
    let key = |p: V2| ((p.x / eps).floor() as i64, (p.y / eps).floor() as i64);
    let mut starts: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, c) in chains.iter().enumerate() {
        if let Some(&p) = c.first() {
            starts.entry(key(p)).or_default().push(i);
        }
    }
    let mut used = vec![false; chains.len()];
    let find = |p: V2, used: &[bool], except: usize| -> Option<usize> {
        let (kx, ky) = key(p);
        let mut best: Option<(f64, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &j in starts.get(&(kx + dx, ky + dy)).map_or(&[][..], |v| &v[..]) {
                    if used[j] || j == except {
                        continue;
                    }
                    let d = (chains[j][0] - p).len();
                    if d <= eps && best.is_none_or(|b| d < b.0) {
                        best = Some((d, j));
                    }
                }
            }
        }
        best.map(|b| b.1)
    };
    let (mut closed, mut open) = (vec![], vec![]);
    for i in 0..chains.len() {
        if used[i] || chains[i].is_empty() {
            continue;
        }
        used[i] = true;
        let mut ring = chains[i].clone();
        loop {
            let end = ring[ring.len() - 1];
            if ring.len() >= 3 && (end - ring[0]).len() <= eps {
                ring.pop();
                closed.push(ring);
                break;
            }
            match find(end, &used, i) {
                Some(j) => {
                    used[j] = true;
                    ring.extend_from_slice(&chains[j][1..]);
                }
                None => {
                    open.push(ring);
                    break;
                }
            }
        }
    }
    (closed, open)
}

/// Side kept by a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// `n·p ≤ d`.
    Below,
    Above,
}

#[derive(Clone, Debug)]
pub struct CutOut {
    pub mesh: Mesh,
    /// All contours on the plane are closed with caps.
    pub capped: bool,
    /// The source mesh is closed: without caps the result would be open, which is an error.
    pub source_closed: bool,
}

/// Every mesh edge belongs to exactly two triangles.
fn is_closed(w: &Mesh) -> bool {
    let mut uses: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &w.triangles {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *uses.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    !w.triangles.is_empty() && uses.values().all(|&n| n == 2)
}

/// Plane clip: the `keep` side remains, the cut is closed with planar faces.
pub fn cut(m: &Mesh, pl: &Plane, keep: Keep, tol: f64) -> CutOut {
    let w = weld(m, tol);
    let closed = is_closed(&w);
    match keep {
        Keep::Below => cut_below(&w, pl, tol, closed),
        Keep::Above => cut_below(&w, &pl.flipped(), tol, closed),
    }
}

/// Plane split: (below, above), both parts with caps.
pub fn split(m: &Mesh, pl: &Plane, tol: f64) -> (CutOut, CutOut) {
    let w = weld(m, tol);
    let closed = is_closed(&w);
    (
        cut_below(&w, pl, tol, closed),
        cut_below(&w, &pl.flipped(), tol, closed),
    )
}

fn cut_below(w: &Mesh, pl: &Plane, tol: f64, source_closed: bool) -> CutOut {
    let c = Cutter::new(w, pl, tol);
    let mut pos = w.positions.clone();
    let mut on: Vec<bool> = c.s.iter().map(|&s| s == 0).collect();
    let mut made: HashMap<(u32, u32), u32> = HashMap::new();
    let mut tris = vec![];
    let mut faces = vec![];
    let mut top_face = 0u32;
    for (ti, t) in w.triangles.iter().enumerate() {
        let f = face_of(w, ti);
        top_face = top_face.max(f + 1);
        let s = t.map(|i| c.s[i as usize]);
        let has_pos = s.contains(&1);
        let has_neg = s.contains(&-1);
        if !has_pos {
            if s == [0, 0, 0] {
                let [a, b, cc] = t.map(|i| w.positions[i as usize]);
                if (b - a).cross(cc - a).dot(pl.n) <= 0.0 {
                    continue;
                }
            }
            tris.push(*t);
            faces.push(f);
            continue;
        }
        if !has_neg {
            continue;
        }
        let mut poly: Vec<u32> = vec![];
        for k in 0..3 {
            let (i, j) = (t[k], t[(k + 1) % 3]);
            let (si, sj) = (s[k], s[(k + 1) % 3]);
            if si <= 0 {
                poly.push(i);
            }
            if si * sj < 0 {
                let key = (i.min(j), i.max(j));
                let v = *made.entry(key).or_insert_with(|| {
                    pos.push(c.point(Key::E(key.0, key.1)));
                    on.push(true);
                    (pos.len() - 1) as u32
                });
                poly.push(v);
            }
        }
        for k in 1..poly.len().saturating_sub(1) {
            tris.push([poly[0], poly[k], poly[k + 1]]);
            faces.push(f);
        }
    }
    // caps: boundary edges on the plane, reversed winding
    let mut dir: HashMap<(u32, u32), usize> = HashMap::new();
    for t in &tris {
        for k in 0..3 {
            *dir.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
        }
    }
    let rim: Vec<(u32, u32)> = dir
        .keys()
        .filter(|&&(a, b)| !dir.contains_key(&(b, a)) && on[a as usize] && on[b as usize])
        .map(|&(a, b)| (b, a))
        .collect();
    let mut rim = rim;
    rim.sort_unstable();
    let (loops, open) = chains(&rim);
    let mut capped = open.is_empty();
    let mut back: HashMap<(u64, u64), u32> = HashMap::new();
    let mut rings: Vec<Vec<V2>> = vec![];
    for l in &loops {
        let r: Vec<V2> = l
            .iter()
            .map(|&i| {
                let q = pl.to_2d(pos[i as usize]);
                back.insert((q.x.to_bits(), q.y.to_bits()), i);
                q
            })
            .collect();
        rings.push(r);
    }
    for reg in nest(rings) {
        match triangulate_region(&reg.outer, &reg.holes) {
            Some(ts) => {
                let pts: Vec<V2> = reg
                    .outer
                    .iter()
                    .chain(reg.holes.iter().flatten())
                    .copied()
                    .collect();
                for t in ts {
                    let idx = t.map(|k| {
                        let q = pts[k];
                        back.get(&(q.x.to_bits(), q.y.to_bits()))
                            .copied()
                            .unwrap_or_else(|| {
                                pos.push(pl.to_3d(q));
                                (pos.len() - 1) as u32
                            })
                    });
                    tris.push(idx);
                    faces.push(top_face);
                }
            }
            None => capped = false,
        }
    }
    CutOut {
        mesh: compact(pos, tris, faces),
        capped,
        source_closed,
    }
}

// ---------- planar region triangulation ----------

const NIL: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Node {
    /// Point index in the input (outer contour, then the holes in sequence).
    i: usize,
    p: V2,
    prev: usize,
    next: usize,
    z: u64,
    pz: usize,
    nz: usize,
}

struct Ear {
    n: Vec<Node>,
    lo: V2,
    inv: f64,
}

/// Signed area in the ear-clipping convention: < 0 means a left turn p→q→r.
fn ar(p: V2, q: V2, r: V2) -> f64 {
    (q.y - p.y) * (r.x - q.x) - (q.x - p.x) * (r.y - q.y)
}

fn in_tri(a: V2, b: V2, c: V2, p: V2) -> bool {
    (c.x - p.x) * (a.y - p.y) >= (a.x - p.x) * (c.y - p.y)
        && (a.x - p.x) * (b.y - p.y) >= (b.x - p.x) * (a.y - p.y)
        && (b.x - p.x) * (c.y - p.y) >= (c.x - p.x) * (b.y - p.y)
}

/// Point code on the Morton curve (2^16 grid over the bounding box).
fn morton(x: u64, y: u64) -> u64 {
    let spread = |mut v: u64| {
        v &= 0xffff;
        v = (v | (v << 8)) & 0x00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333;
        (v | (v << 1)) & 0x5555_5555
    };
    spread(x) | (spread(y) << 1)
}

impl Ear {
    fn z(&self, p: V2) -> u64 {
        let q = |v: f64| (v * self.inv).clamp(0.0, 65535.0) as u64;
        morton(q(p.x - self.lo.x), q(p.y - self.lo.y))
    }

    /// Ring of points `pts[from..to]` in the given winding (ccw = counter-clockwise); returns the last node.
    fn ring(&mut self, pts: &[V2], from: usize, to: usize, ccw: bool) -> Option<usize> {
        let s = signed_area(&pts[from..to]);
        let idx: Vec<usize> = if (s > 0.0) == ccw {
            (from..to).collect()
        } else {
            (from..to).rev().collect()
        };
        let mut last = NIL;
        for i in idx {
            let k = self.n.len();
            self.n.push(Node {
                i,
                p: pts[i],
                prev: last,
                next: NIL,
                z: 0,
                pz: NIL,
                nz: NIL,
            });
            if last != NIL {
                self.n[last].next = k;
            }
            last = k;
        }
        if last == NIL {
            return None;
        }
        // close the ring: the first node follows the last
        let mut first = last;
        while self.n[first].prev != NIL {
            first = self.n[first].prev;
        }
        self.n[first].prev = last;
        self.n[last].next = first;
        Some(last)
    }

    fn remove(&mut self, k: usize) {
        let Node {
            prev, next, pz, nz, ..
        } = self.n[k];
        self.n[next].prev = prev;
        self.n[prev].next = next;
        if pz != NIL {
            self.n[pz].nz = nz;
        }
        if nz != NIL {
            self.n[nz].pz = pz;
        }
    }

    /// Bridge a → b: two opposing edges, the rings merge; returns the copy of b.
    fn split(&mut self, a: usize, b: usize) -> usize {
        let (an, bp) = (self.n[a].next, self.n[b].prev);
        let a2 = self.n.len();
        self.n.push(Node {
            pz: NIL,
            nz: NIL,
            ..self.n[a]
        });
        let b2 = self.n.len();
        self.n.push(Node {
            pz: NIL,
            nz: NIL,
            ..self.n[b]
        });
        self.n[a].next = b;
        self.n[b].prev = a;
        self.n[a2].next = an;
        self.n[an].prev = a2;
        self.n[b2].next = a2;
        self.n[a2].prev = b2;
        self.n[bp].next = b2;
        self.n[b2].prev = bp;
        b2
    }

    fn locally_inside(&self, a: usize, b: usize) -> bool {
        let (pa, pb) = (self.n[a].p, self.n[b].p);
        let (pp, pn) = (self.n[self.n[a].prev].p, self.n[self.n[a].next].p);
        if ar(pp, pa, pn) < 0.0 {
            ar(pa, pb, pn) >= 0.0 && ar(pa, pp, pb) >= 0.0
        } else {
            ar(pa, pb, pp) < 0.0 || ar(pa, pn, pb) < 0.0
        }
    }

    /// Outer-ring vertex visible from the leftmost point of hole `h` by a ray to the left.
    fn bridge(&self, h: usize, outer: usize) -> Option<usize> {
        let hp = self.n[h].p;
        let mut p = outer;
        let mut qx = f64::NEG_INFINITY;
        let mut m = NIL;
        loop {
            let (a, b) = (self.n[p].p, self.n[self.n[p].next].p);
            if hp.y <= a.y && hp.y >= b.y && b.y != a.y {
                let x = a.x + (hp.y - a.y) * (b.x - a.x) / (b.y - a.y);
                if x <= hp.x && x > qx {
                    qx = x;
                    m = if a.x < b.x { p } else { self.n[p].next };
                    if x == hp.x {
                        return Some(m);
                    }
                }
            }
            p = self.n[p].next;
            if p == outer {
                break;
            }
        }
        if m == NIL {
            return None;
        }
        // points inside triangle (h, intersection, m): the visible one with the smallest angle is chosen
        let stop = m;
        let mp = self.n[m].p;
        let mut best = f64::INFINITY;
        let mut p = m;
        loop {
            let q = self.n[p].p;
            let (t1, t3) = if hp.y < mp.y { (hp.x, qx) } else { (qx, hp.x) };
            if hp.x >= q.x
                && q.x >= mp.x
                && hp.x != q.x
                && in_tri(v2(t1, hp.y), mp, v2(t3, hp.y), q)
            {
                let tan = (hp.y - q.y).abs() / (hp.x - q.x);
                if self.locally_inside(p, h) && (tan < best || (tan == best && q.x > self.n[m].p.x))
                {
                    m = p;
                    best = tan;
                }
            }
            p = self.n[p].next;
            if p == stop {
                break;
            }
        }
        Some(m)
    }

    fn is_ear(&self, e: usize) -> bool {
        let (a, b, c) = (
            self.n[self.n[e].prev].p,
            self.n[e].p,
            self.n[self.n[e].next].p,
        );
        if ar(a, b, c) >= 0.0 {
            return false;
        }
        let lo = v2(a.x.min(b.x).min(c.x), a.y.min(b.y).min(c.y));
        let hi = v2(a.x.max(b.x).max(c.x), a.y.max(b.y).max(c.y));
        let (z0, z1) = (self.z(lo), self.z(hi));
        let blocks = |k: usize| {
            let nd = &self.n[k];
            let p = nd.p;
            k != self.n[e].prev
                && k != self.n[e].next
                && k != e
                && p != a
                && p != c
                && in_tri(a, b, c, p)
                && ar(self.n[nd.prev].p, p, self.n[nd.next].p) >= 0.0
        };
        // candidates inside the bounding box lie in the Morton curve range [z0, z1]
        let mut k = self.n[e].pz;
        while k != NIL && self.n[k].z >= z0 {
            if blocks(k) {
                return false;
            }
            k = self.n[k].pz;
        }
        let mut k = self.n[e].nz;
        while k != NIL && self.n[k].z <= z1 {
            if blocks(k) {
                return false;
            }
            k = self.n[k].nz;
        }
        true
    }

    /// Vertex on the line between its neighbors (turn < 1e-9 rad) or a backward spike.
    fn straight(&self, e: usize) -> bool {
        let (a, b, c) = (
            self.n[self.n[e].prev].p,
            self.n[e].p,
            self.n[self.n[e].next].p,
        );
        let (u, w) = (b - a, c - b);
        let s = u.len() * w.len();
        s <= 0.0 || u.cross(w).abs() <= 1e-9 * s
    }
}

/// Triangulates a planar region by ear clipping with a Morton-curve index (nearly
/// O(n log n)): holes are first bridged to the outer contour. All input vertices
/// end up in triangles (a vertex on a straight line yields a zero-area triangle, so the mesh
/// stays free of T-junctions). Indices run over `outer`, then `holes` in sequence; counter-clockwise.
/// None if the contour cannot be resolved (self-intersections).
pub fn triangulate_region(outer: &[V2], holes: &[Vec<V2>]) -> Option<Vec<[usize; 3]>> {
    let pts: Vec<V2> = outer
        .iter()
        .chain(holes.iter().flatten())
        .copied()
        .collect();
    if outer.len() < 3 || pts.iter().any(|p| !(p.x.is_finite() && p.y.is_finite())) {
        return None;
    }
    let (mut lo, mut hi) = (v2(f64::MAX, f64::MAX), v2(f64::MIN, f64::MIN));
    for p in &pts {
        lo = v2(lo.x.min(p.x), lo.y.min(p.y));
        hi = v2(hi.x.max(p.x), hi.y.max(p.y));
    }
    let span = (hi.x - lo.x).max(hi.y - lo.y);
    let mut e = Ear {
        n: Vec::with_capacity(pts.len() + 2 * holes.len()),
        lo,
        inv: if span > 0.0 { 65535.0 / span } else { 0.0 },
    };
    let mut start = e.ring(&pts, 0, outer.len(), true)?;
    // holes in ascending order of their leftmost point
    let mut hs: Vec<usize> = vec![];
    let mut from = outer.len();
    for h in holes {
        if h.len() >= 3 {
            if let Some(last) = e.ring(&pts, from, from + h.len(), false) {
                let mut left = last;
                let mut k = e.n[last].next;
                while k != last {
                    let (p, q) = (e.n[k].p, e.n[left].p);
                    if p.x < q.x || (p.x == q.x && p.y < q.y) {
                        left = k;
                    }
                    k = e.n[k].next;
                }
                hs.push(left);
            }
        }
        from += h.len();
    }
    hs.sort_by(|&a, &b| e.n[a].p.x.total_cmp(&e.n[b].p.x));
    for h in hs {
        let b = e.bridge(h, start)?;
        e.split(b, h);
        start = b;
    }
    // Morton index over all nodes
    let mut order: Vec<usize> = (0..e.n.len()).collect();
    for k in 0..e.n.len() {
        e.n[k].z = e.z(e.n[k].p);
    }
    order.sort_by_key(|&k| e.n[k].z);
    for w in order.windows(2) {
        e.n[w[0]].nz = w[1];
        e.n[w[1]].pz = w[0];
    }
    let mut out = Vec::with_capacity(pts.len());
    let mut ear = start;
    let mut stop = ear;
    let mut left = e.n.len();
    while left > 2 && e.n[ear].prev != e.n[ear].next {
        let (p, nx) = (e.n[ear].prev, e.n[ear].next);
        if e.straight(ear) || e.is_ear(ear) {
            out.push([e.n[p].i, e.n[ear].i, e.n[nx].i]);
            e.remove(ear);
            left -= 1;
            ear = e.n[nx].next;
            stop = ear;
            continue;
        }
        ear = nx;
        if ear == stop {
            return None;
        }
    }
    Some(out)
}

/// Mesh edges visible as lines: boundaries and creases sharper than `cos_tol` (after welding).
pub fn edges(m: &Mesh, cos_tol: f64, tol: f64) -> Vec<(V3, V3)> {
    crate::poly::feature_edges(&weld(m, tol), cos_tol)
}

#[cfg(test)]
#[path = "mesh_ops_tests.rs"]
pub(crate) mod tests;
