// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing data of a hierarchical lattice: one mesh per prototype with its pieces as true
//! cross-sections (angles, flats, rolled shapes; no boxes), a coarser level without the lacing,
//! and the world placement of every prototype for instanced drawing.

use super::hier::{Hier, Leaf, Xf};
use super::lattice::MemRole;
use super::sections;
use crate::math::{v2, V2, V3};
use crate::member::Shape;
use crate::poly;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LodMesh {
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

impl LodMesh {
    pub fn bytes(&self) -> usize {
        self.positions.len() * 12 + self.triangles.len() * 12
    }
}

/// One prototype: meshes from fine to coarse with the geometric error of each (m), and its
/// placements as 3x4 column-major matrices (x, y, z axes, origin).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub proto: u32,
    pub lods: Vec<(LodMesh, f32)>,
    pub xfs: Vec<[f32; 12]>,
}

pub fn mat(x: &Xf) -> [f32; 12] {
    let f = |v: V3| [v.x as f32, v.y as f32, v.z as f32];
    let (a, b, c, d) = (f(x.x), f(x.y), f(x.z), f(x.o));
    [a[0], a[1], a[2], b[0], b[1], b[2], c[0], c[1], c[2], d[0], d[1], d[2]]
}

/// Cross-section rings of a piece: (outer, hole), shifted so an angle's heel sits on the axis.
fn rings(s: &Shape, heel: bool) -> (Vec<V2>, Option<Vec<V2>>) {
    let (mut o, h) = s.rings();
    if heel {
        if let Shape::Angle { h, b, .. } = *s {
            for p in &mut o {
                *p = v2(p.x + b / 2.0, p.y + h / 2.0);
            }
        }
    }
    (o, h)
}

/// Appends the closed solid of one piece.
pub fn extrude(l: &Leaf, s: &Shape, m: &mut LodMesh) {
    let len = (l.b - l.a).len();
    if len < 1e-9 {
        return;
    }
    let u = (l.b - l.a) * (1.0 / len);
    let mut y = l.up - u * l.up.dot(u);
    if y.len() < 1e-9 {
        y = if u.z.abs() < 0.9 { crate::math::v3(0.0, 0.0, 1.0) } else { crate::math::v3(1.0, 0.0, 0.0) };
        y = y - u * y.dot(u);
    }
    let y = y.normalized();
    let z = u.cross(y).normalized();
    let (o, h) = rings(s, l.heel);
    let pts: Vec<V2> = o.iter().chain(h.iter().flatten()).copied().collect();
    let base = m.positions.len() as u32;
    let first_tri = m.triangles.len();
    for end in [l.a, l.b] {
        for p in &pts {
            let q = end + y * p.x + z * p.y;
            m.positions.push([q.x as f32, q.y as f32, q.z as f32]);
        }
    }
    let per = pts.len() as u32;
    let (no, nh) = (o.len() as u32, h.as_ref().map_or(0, |h| h.len()) as u32);
    let id = |k: u32, i: u32| base + k * per + i;
    let mut loops = vec![(0, no)];
    if nh > 0 {
        loops.push((no, nh));
    }
    for (b0, n) in loops {
        for i in 0..n {
            let (i0, i1) = (b0 + i, b0 + (i + 1) % n);
            m.triangles.push([id(0, i0), id(0, i1), id(1, i1)]);
            m.triangles.push([id(0, i0), id(1, i1), id(1, i0)]);
        }
    }
    if nh > 0 {
        for i in 0..no {
            let (a0, a1) = (i, (i + 1) % no);
            let (c0, c1) = (no + (no - 1 - a0), no + (no - 1 - a1));
            for (k, flip) in [(0u32, true), (1, false)] {
                for t in [[id(k, a0), id(k, a1), id(k, c1)], [id(k, a0), id(k, c1), id(k, c0)]] {
                    m.triangles.push(if flip { [t[0], t[2], t[1]] } else { t });
                }
            }
        }
    } else {
        for t in poly::triangulate(&o) {
            let t = [t[0] as u32, t[1] as u32, t[2] as u32];
            m.triangles.push([id(0, t[0]), id(0, t[2]), id(0, t[1])]);
            m.triangles.push([id(1, t[0]), id(1, t[1]), id(1, t[2])]);
        }
    }
    // outward faces: flip the piece when its signed volume comes out negative
    let p = |i: u32| {
        let q = m.positions[i as usize];
        crate::math::v3(q[0] as f64, q[1] as f64, q[2] as f64) - l.a
    };
    let vol: f64 = m.triangles[first_tri..].iter().map(|t| p(t[0]).dot(p(t[1]).cross(p(t[2])))).sum();
    if vol < 0.0 {
        for t in &mut m.triangles[first_tri..] {
            t.swap(1, 2);
        }
    }
}

fn shapes(h: &Hier) -> Vec<Option<Shape>> {
    h.sections.iter().map(|s| sections::find(s)).collect()
}

fn mesh_of(leaves: &[Leaf], shapes: &[Option<Shape>], keep: impl Fn(&Leaf) -> bool) -> LodMesh {
    let mut m = LodMesh::default();
    for l in leaves.iter().filter(|l| keep(l)) {
        if let Some(s) = &shapes[l.sec as usize] {
            extrude(l, s, &mut m);
        }
    }
    m
}

/// Instanced groups of every prototype with pieces of its own, and the rolled tower members as
/// one mesh in world axes.
pub fn groups(h: &Hier) -> (Vec<Group>, LodMesh) {
    let sh = shapes(h);
    let mut xfs: Vec<Vec<[f32; 12]>> = vec![vec![]; h.protos.len()];
    for (p, xf) in h.placements() {
        xfs[p as usize].push(mat(&xf));
    }
    let mut out = vec![];
    for (i, p) in h.protos.iter().enumerate() {
        if p.leaves.is_empty() || xfs[i].is_empty() {
            continue;
        }
        let full = mesh_of(&p.leaves, &sh, |_| true);
        let mut lods = vec![(full, 0.0f32)];
        let chords = mesh_of(&p.leaves, &sh, |l| l.role == MemRole::Chord);
        if !chords.triangles.is_empty() && chords.triangles.len() < lods[0].0.triangles.len() {
            lods.push((chords, p.width as f32));
        }
        out.push(Group { proto: i as u32, lods, xfs: std::mem::take(&mut xfs[i]) });
    }
    (out, mesh_of(&h.leaves, &sh, |_| true))
}
