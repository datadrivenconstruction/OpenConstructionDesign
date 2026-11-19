// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Voxel proxies for hierarchical detail: geometry is voxelized conservatively (exact
//! triangle-box overlap), enclosed empty space is filled from outside, and the exposed faces are
//! merged into quads per label. Every proxy surface point lies within `error` metres of the
//! geometry it was built from; a proxy built from another proxy adds the errors, so the error grows
//! monotonically up the tree. Contract: `docs/CITY1.md`.

use crate::chunks::Instanced;
use crate::scene::{is_translucent, mesh_box, tag_rgb, Aabb, SceneMesh};
use glam::{Mat4, Vec3};
use std::collections::HashMap;
use std::sync::Arc;

pub const SQRT3: f32 = 1.732_050_8;
/// Most voxels in one grid; a coarser step is taken above it (the error then grows with it).
pub const MAX_VOXELS: usize = 4_000_000;
/// A coarser level is kept only when it has at most this share of the finer level's triangles.
pub const KEEP_RATIO: f32 = 0.7;
/// Colour of glass in proxies: they are opaque, a far facade reads as dark glass.
pub const PROXY_GLASS: [u8; 3] = [92, 112, 128];

/// Who owns a proxy face: the element a pick selects and the opaque colour tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label {
    pub element: u32,
    pub tag: u32,
}

/// Opaque tag for a proxy face: glass becomes a dark tint, everything else keeps its colour.
pub fn opaque_tag(tag: u32) -> u32 {
    if is_translucent(tag) {
        tag_rgb(PROXY_GLASS, false)
    } else {
        tag
    }
}

/// Simplified stand-in of some geometry: meshes (one per label) with their boxes.
#[derive(Clone, Debug, Default)]
pub struct Proxy {
    pub meshes: Vec<(Arc<SceneMesh>, Aabb)>,
    pub bounds: Aabb,
    /// Metres: every proxy surface point is this close to the source geometry.
    pub error: f32,
    /// Voxel step it was built with, metres.
    pub step: f32,
    pub triangles: usize,
    pub bytes: u64,
}

impl Proxy {
    fn of(meshes: Vec<SceneMesh>, error: f32, step: f32) -> Self {
        let mut bounds = Aabb::EMPTY;
        let mut triangles = 0;
        let mut bytes = 0;
        let meshes = meshes
            .into_iter()
            .map(|m| {
                let b = mesh_box(&m);
                bounds.union(&b);
                triangles += m.triangles.len();
                bytes += mesh_bytes(&m);
                (Arc::new(m), b)
            })
            .collect();
        Self {
            meshes,
            bounds,
            error,
            step,
            triangles,
            bytes,
        }
    }
}

/// Bytes a mesh holds on the CPU (positions and triangles).
pub fn mesh_bytes(m: &SceneMesh) -> u64 {
    (m.positions.len() * 12 + m.triangles.len() * 12) as u64
}

/// Exact triangle / axis-aligned box overlap (separating axes: 3 box axes, the triangle normal,
/// 9 edge cross products). `c` box centre, `h` half extents.
pub fn tri_box(c: Vec3, h: Vec3, t: [Vec3; 3]) -> bool {
    let v = [t[0] - c, t[1] - c, t[2] - c];
    for k in 0..3 {
        let lo = v[0][k].min(v[1][k]).min(v[2][k]);
        let hi = v[0][k].max(v[1][k]).max(v[2][k]);
        if lo > h[k] || hi < -h[k] {
            return false;
        }
    }
    let e = [v[1] - v[0], v[2] - v[1], v[0] - v[2]];
    for ei in e {
        for a in [Vec3::X, Vec3::Y, Vec3::Z] {
            let axis = a.cross(ei);
            if axis.length_squared() < 1e-12 {
                continue;
            }
            let p = v.map(|x| x.dot(axis));
            let r = h.x * axis.x.abs() + h.y * axis.y.abs() + h.z * axis.z.abs();
            let lo = p[0].min(p[1]).min(p[2]);
            let hi = p[0].max(p[1]).max(p[2]);
            if lo > r || hi < -r {
                return false;
            }
        }
    }
    let n = e[0].cross(e[1]);
    let r = h.x * n.x.abs() + h.y * n.y.abs() + h.z * n.z.abs();
    n.dot(v[0]).abs() <= r
}

/// Occupancy grid with a label per voxel (0 empty, else label index + 1).
pub struct Grid {
    origin: Vec3,
    h: f32,
    dims: [usize; 3],
    cells: Vec<u32>,
    labels: Vec<Label>,
    lookup: HashMap<Label, u32>,
}

impl Grid {
    /// Grid over `bounds` with step `h`, coarsened until it holds at most `MAX_VOXELS`.
    pub fn new(bounds: &Aabb, h: f32) -> Self {
        let mut h = h.max(1e-3);
        // a small margin keeps faces on the bounds inside a cell, away from rounding at the edge
        let (pad, dims) = loop {
            let pad = Vec3::splat(h * 0.01);
            let ext = (bounds.max - bounds.min).max(Vec3::splat(1e-3)) + pad * 2.0;
            let d = [0, 1, 2].map(|k| ((ext[k] / h).ceil() as usize).max(1));
            if d[0] * d[1] * d[2] <= MAX_VOXELS {
                break (pad, d);
            }
            h *= 1.25;
        };
        Self {
            origin: bounds.min - pad,
            h,
            dims,
            cells: vec![0; dims[0] * dims[1] * dims[2]],
            labels: vec![],
            lookup: HashMap::new(),
        }
    }

    /// The step actually used (may be coarser than asked).
    pub fn step(&self) -> f32 {
        self.h
    }

    pub fn dims(&self) -> [usize; 3] {
        self.dims
    }

    fn idx(&self, x: usize, y: usize, z: usize) -> usize {
        x + self.dims[0] * (y + self.dims[1] * z)
    }

    pub fn solid(&self, x: usize, y: usize, z: usize) -> bool {
        self.cells[self.idx(x, y, z)] != 0
    }

    pub fn solid_count(&self) -> usize {
        self.cells.iter().filter(|&&c| c != 0).count()
    }

    fn label_id(&mut self, l: Label) -> u32 {
        if let Some(&i) = self.lookup.get(&l) {
            return i;
        }
        self.labels.push(l);
        let i = self.labels.len() as u32;
        self.lookup.insert(l, i);
        i
    }

    fn cell_range(&self, lo: f32, hi: f32, k: usize) -> Option<(usize, usize)> {
        let a = ((lo - self.origin[k]) / self.h).floor();
        let b = ((hi - self.origin[k]) / self.h).floor();
        let n = self.dims[k] as f32;
        if b < 0.0 || a >= n {
            return None;
        }
        Some((a.max(0.0) as usize, (b.min(n - 1.0)) as usize))
    }

    /// Marks every voxel the triangle touches; a voxel keeps its first label.
    pub fn mark_triangle(&mut self, t: [Vec3; 3], label: Label) {
        let lo = t[0].min(t[1]).min(t[2]);
        let hi = t[0].max(t[1]).max(t[2]);
        let (Some(rx), Some(ry), Some(rz)) = (
            self.cell_range(lo.x, hi.x, 0),
            self.cell_range(lo.y, hi.y, 1),
            self.cell_range(lo.z, hi.z, 2),
        ) else {
            return;
        };
        let id = self.label_id(label);
        let half = Vec3::splat(self.h * 0.5);
        let single = rx.0 == rx.1 && ry.0 == ry.1 && rz.0 == rz.1;
        for z in rz.0..=rz.1 {
            for y in ry.0..=ry.1 {
                for x in rx.0..=rx.1 {
                    let i = self.idx(x, y, z);
                    if self.cells[i] != 0 {
                        continue;
                    }
                    let c = self.origin
                        + Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5) * self.h;
                    if single || tri_box(c, half, t) {
                        self.cells[i] = id;
                    }
                }
            }
        }
    }

    /// Marks a mesh placed by `m`; faces get `label(mesh element, mesh tag)`.
    pub fn mark_mesh(&mut self, mesh: &SceneMesh, m: &Mat4, label: Label) {
        let ident = *m == Mat4::IDENTITY;
        for t in &mesh.triangles {
            let p = t.map(|i| {
                let v = Vec3::from(mesh.positions[i as usize]);
                if ident {
                    v
                } else {
                    m.transform_point3(v)
                }
            });
            self.mark_triangle(p, label);
        }
    }

    /// Empty voxels not reachable from outside the grid (6-neighbour flood fill) become solid,
    /// with the label of their solid neighbour on the -x side: enclosed interiors stop producing
    /// faces, a courtyard open to the sky stays open.
    pub fn fill_interior(&mut self) {
        let [nx, ny, nz] = self.dims;
        let mut outside = vec![false; self.cells.len()];
        let mut stack = vec![];
        let mut seed = |x: usize, y: usize, z: usize, out: &mut Vec<bool>, st: &mut Vec<usize>| {
            let i = x + nx * (y + ny * z);
            if self.cells[i] == 0 && !out[i] {
                out[i] = true;
                st.push(i);
            }
        };
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    if x == 0 || y == 0 || z == 0 || x == nx - 1 || y == ny - 1 || z == nz - 1 {
                        seed(x, y, z, &mut outside, &mut stack);
                    }
                }
            }
        }
        while let Some(i) = stack.pop() {
            let x = i % nx;
            let y = (i / nx) % ny;
            let z = i / (nx * ny);
            let mut go = |j: usize| {
                if self.cells[j] == 0 && !outside[j] {
                    outside[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                go(i - 1);
            }
            if x + 1 < nx {
                go(i + 1);
            }
            if y > 0 {
                go(i - nx);
            }
            if y + 1 < ny {
                go(i + nx);
            }
            if z > 0 {
                go(i - nx * ny);
            }
            if z + 1 < nz {
                go(i + nx * ny);
            }
        }
        for i in 0..self.cells.len() {
            if self.cells[i] == 0 && !outside[i] {
                // never on the border (border cells are outside), so i - 1 is in the same row
                self.cells[i] = self.cells[i - 1];
            }
        }
    }

    /// Exposed voxel faces merged into maximal rectangles of one label (greedy, per slice and
    /// direction); one mesh per label, faces wound outward.
    pub fn surface(&self) -> Vec<SceneMesh> {
        let mut out: Vec<SceneMesh> = self
            .labels
            .iter()
            .map(|l| SceneMesh {
                element: l.element,
                tag: l.tag,
                positions: vec![],
                triangles: vec![],
            })
            .collect();
        let d3 = self.dims;
        for d in 0..3 {
            let (u, v) = ((d + 1) % 3, (d + 2) % 3);
            let (nu, nv) = (d3[u], d3[v]);
            let mut mask = vec![0u32; nu * nv];
            for k in 0..=d3[d] {
                for positive in [true, false] {
                    let mut any = false;
                    for j in 0..nv {
                        for i in 0..nu {
                            let at = |kk: usize| {
                                let mut c = [0usize; 3];
                                c[d] = kk;
                                c[u] = i;
                                c[v] = j;
                                self.cells[self.idx(c[0], c[1], c[2])]
                            };
                            let below = if k > 0 { at(k - 1) } else { 0 };
                            let above = if k < d3[d] { at(k) } else { 0 };
                            let l = if positive {
                                if below != 0 && above == 0 {
                                    below
                                } else {
                                    0
                                }
                            } else if above != 0 && below == 0 {
                                above
                            } else {
                                0
                            };
                            mask[i + j * nu] = l;
                            any |= l != 0;
                        }
                    }
                    if any {
                        self.greedy(&mut mask, (d, u, v), k, positive, &mut out);
                    }
                }
            }
        }
        out.retain(|m| !m.triangles.is_empty());
        out
    }

    fn greedy(
        &self,
        mask: &mut [u32],
        (d, u, v): (usize, usize, usize),
        k: usize,
        positive: bool,
        out: &mut [SceneMesh],
    ) {
        let (nu, nv) = (self.dims[u], self.dims[v]);
        for j in 0..nv {
            let mut i = 0;
            while i < nu {
                let l = mask[i + j * nu];
                if l == 0 {
                    i += 1;
                    continue;
                }
                let mut w = 1;
                while i + w < nu && mask[i + w + j * nu] == l {
                    w += 1;
                }
                let mut hgt = 1;
                'rows: while j + hgt < nv {
                    for a in 0..w {
                        if mask[i + a + (j + hgt) * nu] != l {
                            break 'rows;
                        }
                    }
                    hgt += 1;
                }
                for b in 0..hgt {
                    for a in 0..w {
                        mask[i + a + (j + b) * nu] = 0;
                    }
                }
                let corner = |a: usize, b: usize| {
                    let mut p = [0.0f32; 3];
                    p[d] = self.origin[d] + k as f32 * self.h;
                    p[u] = self.origin[u] + a as f32 * self.h;
                    p[v] = self.origin[v] + b as f32 * self.h;
                    p
                };
                let m = &mut out[(l - 1) as usize];
                let base = m.positions.len() as u32;
                m.positions.extend([
                    corner(i, j),
                    corner(i + w, j),
                    corner(i + w, j + hgt),
                    corner(i, j + hgt),
                ]);
                if positive {
                    m.triangles
                        .extend([[base, base + 1, base + 2], [base, base + 2, base + 3]]);
                } else {
                    m.triangles
                        .extend([[base, base + 2, base + 1], [base, base + 3, base + 2]]);
                }
                i += w;
            }
        }
    }

    /// Proxy of what was marked: interior filled, surface extracted. `base_error` — error of the
    /// marked geometry itself (0 for real geometry, the source proxy's error for a proxy).
    pub fn proxy(mut self, base_error: f32) -> Proxy {
        self.fill_interior();
        let error = self.h * SQRT3 + base_error;
        Proxy::of(self.surface(), error, self.h)
    }
}

/// Geometry handed to a proxy build: plain meshes and instanced groups.
pub struct Source<'a> {
    pub meshes: &'a [(Arc<SceneMesh>, Aabb)],
    pub groups: &'a [Instanced],
}

impl Source<'_> {
    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        self.meshes.iter().for_each(|(_, x)| b.union(x));
        for g in self.groups {
            b.union(&g.placed_bounds().all);
        }
        b
    }

    pub fn triangles(&self) -> usize {
        let m: usize = self.meshes.iter().map(|(m, _)| m.triangles.len()).sum();
        let g: usize = self
            .groups
            .iter()
            .map(|g| g.proto.triangles.len() * g.inst.len())
            .sum();
        m + g
    }

    fn mark(&self, grid: &mut Grid, label: &dyn Fn(u32, u32) -> Label) {
        for (m, _) in self.meshes {
            grid.mark_mesh(m, &Mat4::IDENTITY, label(m.element, m.tag));
        }
        for g in self.groups {
            for i in g.inst.iter() {
                grid.mark_mesh(&g.proto, &i.m, label(i.element, g.proto.tag));
            }
        }
    }
}

/// Screen error the proxies are built for, px: the city view's tolerance at rest.
pub const TOL_PX: f32 = 1.5;
/// A unit's finest proxy must hold until the unit spans this many pixels; only a closer view
/// loads its full content.
pub const FULL_PX: f32 = 64.0;

/// Voxel step of the finest proxy of a unit: its error bound `step * SQRT3` stays within
/// `TOL_PX` while the unit's extent projects to at most `FULL_PX` (finer only by the limits).
pub fn unit_step(b: &Aabb) -> f32 {
    let e = (b.max - b.min).max_element();
    (e * TOL_PX / (SQRT3 * FULL_PX)).clamp(0.25, 2.0)
}

/// Proxy chain of one unit, fine to coarse. The first level is always kept (a unit is never
/// without a stand-in); each further level halves the step, is built from the previous proxy
/// and is kept only while it saves triangles (`KEEP_RATIO`).
pub fn unit_chain(src: &Source<'_>, label: &dyn Fn(u32, u32) -> Label) -> Vec<Proxy> {
    let b = src.bounds();
    if b.is_empty() {
        return vec![];
    }
    let mut g = Grid::new(&b, unit_step(&b));
    src.mark(&mut g, label);
    let mut h = g.step();
    let first = g.proxy(0.0);
    let extent = (b.max - b.min).max_element();
    let mut chain = vec![first];
    loop {
        let last = chain.last().expect("chain starts with one level");
        h *= 2.0;
        if h > extent * 0.5 || last.triangles <= 12 {
            break;
        }
        let next = from_proxies(&[last], &b, h, &|l: Label| l);
        if next.triangles as f32 > last.triangles as f32 * KEEP_RATIO {
            break;
        }
        chain.push(next);
    }
    chain
}

/// Proxy of several proxies at step `h`; labels are remapped by `relabel` (keep the owner, or
/// merge owners at coarse levels).
pub fn from_proxies(
    parts: &[&Proxy],
    bounds: &Aabb,
    h: f32,
    relabel: &dyn Fn(Label) -> Label,
) -> Proxy {
    // a coarser grid may end short of a finer proxy's outer faces: cover them all
    let mut all = *bounds;
    parts.iter().for_each(|p| all.union(&p.bounds));
    let mut g = Grid::new(&all, h);
    let mut base = 0.0f32;
    for p in parts {
        base = base.max(p.error);
        for (m, _) in &p.meshes {
            let l = relabel(Label {
                element: m.element,
                tag: m.tag,
            });
            g.mark_mesh(m, &Mat4::IDENTITY, l);
        }
    }
    g.proxy(base)
}
