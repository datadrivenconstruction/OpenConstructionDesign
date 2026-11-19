// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Compressed chunk vertices (20 bytes): position — 21 bits per axis from the chunk origin (high
//! 16 bits in x/y/z, low 5 in w), normal — octahedral in snorm16x2, element id and tag. Vertices
//! are shared: triangles use u16 indices in runs of up to 65,536 vertices (glass — u32, its order
//! is re-sorted). The face normal is taken from the provoking vertex of the triangle (flat
//! interpolation), so every triangle always has a vertex carrying its own normal.

#[cfg(feature = "gpu")]
use crate::chunks::Refused;
use crate::scene::{Aabb, Vertex};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use std::collections::HashMap;

pub const QBITS: u32 = 21;
pub const QMAX: u32 = (1 << QBITS) - 1;
/// Vertices in a run with u16 indices.
const SEG_VERTS: usize = 1 << 16;
const NONE: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct GpuVertex {
    pub q: [u16; 4],
    pub n: [i16; 2],
    pub element: u32,
    pub tag: u32,
}

pub const GPU_VERTEX_BYTES: u64 = std::mem::size_of::<GpuVertex>() as u64;

/// Decoding of a compressed position: `o + q * s`, the step is equal on all axes (normals are not distorted).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deq {
    pub o: Vec3,
    pub s: f32,
}

impl Deq {
    /// Integer coordinates as is (unit proxy box).
    pub const UNIT: Deq = Deq {
        o: Vec3::ZERO,
        s: 1.0,
    };

    pub fn of(b: &Aabb) -> Deq {
        if b.is_empty() {
            return Deq::UNIT;
        }
        let e = (b.max - b.min).max_element();
        let ok = e > 0.0 && e.is_finite() && b.min.is_finite();
        Deq {
            o: if ok { b.min } else { Vec3::ZERO },
            s: if ok { e / QMAX as f32 } else { 1.0 },
        }
    }

    /// Largest position error inside the box: half a step.
    pub fn max_error(&self) -> f32 {
        self.s * 0.5
    }

    pub fn quant(&self, p: Vec3) -> [u32; 3] {
        let q = ((p - self.o) / self.s)
            .round()
            .clamp(Vec3::ZERO, Vec3::splat(QMAX as f32));
        [q.x as u32, q.y as u32, q.z as u32]
    }

    /// Same as the vertex shader: `o + q * s`.
    pub fn pos(&self, q: [u32; 3]) -> Vec3 {
        self.o + Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32) * self.s
    }

    /// Placement row that itself decodes the compressed position: axes × step, translation — to the
    /// chunk origin; the w of the first columns (element id) is unchanged.
    pub fn row(&self, r: &[f32; 16]) -> [f32; 16] {
        let c = |k: usize| Vec3::new(r[k * 4], r[k * 4 + 1], r[k * 4 + 2]);
        let t = c(3) + c(0) * self.o.x + c(1) * self.o.y + c(2) * self.o.z;
        let mut out = *r;
        for k in 0..3 {
            for j in 0..3 {
                out[k * 4 + j] = r[k * 4 + j] * self.s;
            }
        }
        out[12] = t.x;
        out[13] = t.y;
        out[14] = t.z;
        out
    }
}

pub fn pack_q(q: [u32; 3]) -> [u16; 4] {
    let lo = (q[0] & 31) | (q[1] & 31) << 5 | (q[2] & 31) << 10;
    [
        (q[0] >> 5) as u16,
        (q[1] >> 5) as u16,
        (q[2] >> 5) as u16,
        lo as u16,
    ]
}

pub fn unpack_q(v: [u16; 4]) -> [u32; 3] {
    let w = u32::from(v[3]);
    [
        u32::from(v[0]) << 5 | (w & 31),
        u32::from(v[1]) << 5 | ((w >> 5) & 31),
        u32::from(v[2]) << 5 | ((w >> 10) & 31),
    ]
}

fn sgn(x: f32) -> f32 {
    if x >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

/// Octahedral normal; a zero normal is treated as +Z.
pub fn oct(n: Vec3) -> [i16; 2] {
    let l1 = n.x.abs() + n.y.abs() + n.z.abs();
    if !(l1 > 0.0) || !l1.is_finite() {
        return [0, 0];
    }
    let p = n / l1;
    let (x, y) = if p.z >= 0.0 {
        (p.x, p.y)
    } else {
        ((1.0 - p.y.abs()) * sgn(p.x), (1.0 - p.x.abs()) * sgn(p.y))
    };
    let e = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    [e(x), e(y)]
}

/// Same as the vertex shader (snorm16: c / 32767, clamped to at least -1).
pub fn unoct(v: [i16; 2]) -> Vec3 {
    let d = |c: i16| (f32::from(c) / 32767.0).max(-1.0);
    let (x, y) = (d(v[0]), d(v[1]));
    let mut n = Vec3::new(x, y, 1.0 - x.abs() - y.abs());
    let t = (-n.z).max(0.0);
    n.x += if n.x >= 0.0 { -t } else { t };
    n.y += if n.y >= 0.0 { -t } else { t };
    n.normalize()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Idx {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

/// Run: vertices from `base`, indices `[first, first + count)` relative to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub base: u32,
    pub first: u32,
    pub count: u32,
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub v: Vec<GpuVertex>,
    pub idx: Idx,
    pub segs: Vec<Seg>,
    /// The provoking vertex of a triangle is the last (GL), otherwise the first.
    pub last: bool,
}

impl Stream {
    pub fn count(&self) -> u32 {
        match &self.idx {
            Idx::U16(i) => i.len() as u32,
            Idx::U32(i) => i.len() as u32,
        }
    }
    pub fn bytes(&self) -> u64 {
        let idx = match &self.idx {
            Idx::U16(i) => i.len() as u64 * 2,
            Idx::U32(i) => i.len() as u64 * 4,
        };
        self.v.len() as u64 * GPU_VERTEX_BYTES + idx
    }
    /// Vertex number of the `k`-th index in the whole stream.
    pub fn vertex_of(&self, k: u32) -> u32 {
        let s = self
            .segs
            .iter()
            .find(|s| k >= s.first && k < s.first + s.count)
            .copied()
            .unwrap_or(Seg {
                base: 0,
                first: 0,
                count: 0,
            });
        let local = match &self.idx {
            Idx::U16(i) => u32::from(i[k as usize]),
            Idx::U32(i) => i[k as usize],
        };
        s.base + local
    }
}

/// Welding of stream vertices: identical ones (compressed position, element, tag) become one vertex;
/// a vertex has a single normal, a triangle keeps its normal in the provoking vertex.
struct Welder {
    deq: Deq,
    wide: bool,
    last: bool,
    v: Vec<GpuVertex>,
    /// Normal is taken (provoking vertex of at least one triangle or a line end).
    taken: Vec<bool>,
    next: Vec<u32>,
    map: HashMap<([u32; 3], u32, u32), u32>,
    idx: Vec<u32>,
    segs: Vec<Seg>,
    base: usize,
    first: usize,
}

type Key = ([u32; 3], u32, u32);

impl Welder {
    fn new(deq: Deq, wide: bool, last: bool) -> Self {
        Self {
            deq,
            wide,
            last,
            v: vec![],
            taken: vec![],
            next: vec![],
            map: HashMap::new(),
            idx: vec![],
            segs: vec![],
            base: 0,
            first: 0,
        }
    }

    fn room(&mut self, k: usize) {
        if !self.wide && self.v.len() - self.base + k > SEG_VERTS {
            self.cut();
            self.base = self.v.len();
            self.map.clear();
        }
    }

    fn cut(&mut self) {
        if self.idx.len() > self.first {
            self.segs.push(Seg {
                base: self.base as u32,
                first: self.first as u32,
                count: (self.idx.len() - self.first) as u32,
            });
        }
        self.first = self.idx.len();
    }

    fn key(&self, x: &Vertex) -> Key {
        (self.deq.quant(Vec3::from(x.pos)), x.element, x.tag)
    }

    fn add(&mut self, k: Key, n: Option<[i16; 2]>) -> u32 {
        let id = self.v.len() as u32;
        self.v.push(GpuVertex {
            q: pack_q(k.0),
            n: n.unwrap_or([0, 0]),
            element: k.1,
            tag: k.2,
        });
        self.taken.push(n.is_some());
        let head = self.map.insert(k, id).unwrap_or(NONE);
        self.next.push(head);
        id
    }

    fn find(&self, k: &Key, f: impl Fn(u32) -> bool) -> Option<u32> {
        let mut id = self.map.get(k).copied().unwrap_or(NONE);
        while id != NONE {
            if f(id) {
                return Some(id);
            }
            id = self.next[id as usize];
        }
        None
    }

    /// A vertex with this normal or a free one (the normal is assigned to it), otherwise a new one.
    fn with_normal(&mut self, k: &Key, n: [i16; 2]) -> Option<u32> {
        if let Some(id) = self.find(k, |id| {
            self.taken[id as usize] && self.v[id as usize].n == n
        }) {
            return Some(id);
        }
        let id = self.find(k, |id| !self.taken[id as usize])?;
        self.taken[id as usize] = true;
        self.v[id as usize].n = n;
        Some(id)
    }

    fn tri(&mut self, t: &[Vertex]) {
        self.room(3);
        let n = oct(Vec3::from(t[0].normal));
        let keys = [self.key(&t[0]), self.key(&t[1]), self.key(&t[2])];
        let lead = (0..3).find_map(|k| {
            let id = self.find(&keys[k], |id| {
                self.taken[id as usize] && self.v[id as usize].n == n
            })?;
            Some((k, id))
        });
        let lead =
            lead.or_else(|| (0..3).find_map(|k| self.with_normal(&keys[k], n).map(|id| (k, id))));
        let (k0, id0) = lead.unwrap_or_else(|| (0, self.add(keys[0], Some(n))));
        let mut ids = [id0; 3];
        for k in 0..3 {
            if k != k0 {
                ids[k] = match self.map.get(&keys[k]) {
                    Some(&id) => id,
                    None => self.add(keys[k], None),
                };
            }
        }
        let order = if self.last {
            [k0 + 1, k0 + 2, k0]
        } else {
            [k0, k0 + 1, k0 + 2]
        };
        let base = self.base as u32;
        for k in order {
            self.idx.push(ids[k % 3] - base);
        }
    }

    fn line(&mut self, l: &[Vertex]) {
        self.room(2);
        let base = self.base as u32;
        for x in &l[..2] {
            let k = self.key(x);
            let n = oct(Vec3::from(x.normal));
            let id = match self.with_normal(&k, n) {
                Some(id) => id,
                None => self.add(k, Some(n)),
            };
            self.idx.push(id - base);
        }
    }

    fn finish(mut self) -> Stream {
        self.cut();
        let idx = if self.wide {
            Idx::U32(self.idx)
        } else {
            Idx::U16(self.idx.into_iter().map(|i| i as u16).collect())
        };
        Stream {
            v: self.v,
            idx,
            segs: self.segs,
            last: self.last,
        }
    }
}

/// Bounds of flat vertices.
pub fn bounds(v: &[Vertex]) -> Aabb {
    let mut b = Aabb::EMPTY;
    v.iter().for_each(|x| b.grow(Vec3::from(x.pos)));
    b
}

/// Triangles of flat vertices (three each, face normal in every one); `wide` — u32 indices in a
/// single run (glass), `last` — the provoking vertex is the last.
pub fn pack_tris(v: &[Vertex], deq: Deq, wide: bool, last: bool) -> Stream {
    let mut w = Welder::new(deq, wide, last);
    v.chunks_exact(3).for_each(|t| w.tri(t));
    w.finish()
}

/// Line segments (two vertices each).
pub fn pack_lines(l: &[Vertex], deq: Deq) -> Stream {
    let mut w = Welder::new(deq, false, false);
    l.chunks_exact(2).for_each(|s| w.line(s));
    w.finish()
}

#[cfg(feature = "gpu")]
/// Stream on the GPU.
pub(crate) struct StreamGpu {
    verts: wgpu::Buffer,
    pub index: wgpu::Buffer,
    format: wgpu::IndexFormat,
    segs: Vec<Seg>,
    pub count: u32,
}

#[cfg(feature = "gpu")]
impl StreamGpu {
    pub(crate) fn new(
        device: &wgpu::Device,
        label: &str,
        s: &Stream,
        index_extra: wgpu::BufferUsages,
    ) -> Result<Self, Refused> {
        let verts = crate::chunks::try_buffer(
            device,
            label,
            bytemuck::cast_slice(&s.v),
            wgpu::BufferUsages::VERTEX,
        )?;
        let (data, format): (&[u8], _) = match &s.idx {
            Idx::U16(i) => (bytemuck::cast_slice(i), wgpu::IndexFormat::Uint16),
            Idx::U32(i) => (bytemuck::cast_slice(i), wgpu::IndexFormat::Uint32),
        };
        let index = crate::chunks::try_buffer(
            device,
            label,
            data,
            wgpu::BufferUsages::INDEX | index_extra,
        )?;
        Ok(Self {
            verts,
            index,
            format,
            segs: s.segs.clone(),
            count: s.count(),
        })
    }

    /// Draws the stream; the placements buffer (slot 1) is set by the caller.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, inst: std::ops::Range<u32>) {
        if self.count == 0 || inst.is_empty() {
            return;
        }
        pass.set_index_buffer(self.index.slice(..), self.format);
        for s in &self.segs {
            pass.set_vertex_buffer(0, self.verts.slice(u64::from(s.base) * GPU_VERTEX_BYTES..));
            pass.draw_indexed(s.first..s.first + s.count, 0, inst.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunks::{crease_vertices, tri_vertices};
    use crate::scene::SceneMesh;

    /// Stream triangles back: (positions in the original winding order, normal of the provoking vertex).
    pub(crate) fn unpack(s: &Stream, deq: Deq) -> Vec<([Vec3; 3], Vec3)> {
        (0..s.count() / 3)
            .map(|t| {
                let v = |k: u32| s.v[s.vertex_of(t * 3 + k) as usize];
                let p = |k: u32| deq.pos(unpack_q(v(k).q));
                let lead = if s.last { 2 } else { 0 };
                ([p(0), p(1), p(2)], unoct(v(lead).n))
            })
            .collect()
    }

    fn grid_mesh(n: usize, size: f32, at: Vec3) -> SceneMesh {
        let mut positions = vec![];
        let mut triangles = vec![];
        for j in 0..=n {
            for i in 0..=n {
                let (x, y) = (i as f32 / n as f32, j as f32 / n as f32);
                // wave: neighbouring triangles are not coplanar
                let z = (x * 7.0).sin() * (y * 5.0).cos() * 0.3;
                positions.push((at + Vec3::new(x * size, y * size, z)).into());
            }
        }
        let w = n as u32 + 1;
        for j in 0..n as u32 {
            for i in 0..n as u32 {
                let a = j * w + i;
                triangles.push([a, a + 1, a + w + 1]);
                triangles.push([a, a + w + 1, a + w]);
            }
        }
        SceneMesh {
            element: 7,
            tag: 3,
            positions,
            triangles,
        }
    }

    #[test]
    fn half_millimetre_at_chunk_size_and_far_beyond() {
        for (side, at) in [
            (16.0f32, 0.0f32),
            (16.0, 5000.0),
            (512.0, -300.0),
            (1000.0, 0.0),
        ] {
            let b = Aabb {
                min: Vec3::splat(at),
                max: Vec3::splat(at + side),
            };
            let d = Deq::of(&b);
            assert!(d.max_error() <= 0.5e-3, "side {side}: {}", d.max_error());
            let mut worst = 0.0f32;
            for k in 0..20_000u32 {
                let f = |m: u32| ((k.wrapping_mul(m) % 10_007) as f32) / 10_006.0;
                let p = b.min + Vec3::new(f(7919), f(104_729), f(1_299_709)) * side;
                let q = d.quant(p);
                assert_eq!(unpack_q(pack_q(q)), q);
                // in world f32: same arithmetic as the placement row in the shader
                let r = d.row(&glam::Mat4::IDENTITY.to_cols_array());
                let m = glam::Mat4::from_cols_array(&r);
                let w = m.transform_point3(Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32));
                worst = worst.max((w - p).abs().max_element());
            }
            // half a step plus the f32 rounding of the world coordinate itself (as without compression)
            let ulp = (at.abs() + side) * f32::EPSILON;
            assert!(worst <= 0.5e-3, "side {side} at {at}: {worst}");
            assert!(worst <= d.max_error() + 2.0 * ulp, "side {side}: {worst}");
        }
        // for a 16 m chunk the step is 7.6 µm
        let d = Deq::of(&Aabb {
            min: Vec3::ZERO,
            max: Vec3::splat(16.0),
        });
        assert!(d.max_error() < 4e-6);
    }

    #[test]
    fn octahedral_normal_within_a_hundredth_of_a_degree() {
        let mut worst = 0.0f32;
        for k in 0..50_000u32 {
            let a = k as f32 * 0.618_034 * std::f32::consts::TAU;
            let z = 1.0 - 2.0 * (k as f32 + 0.5) / 50_000.0;
            let r = (1.0 - z * z).sqrt();
            let n = Vec3::new(r * a.cos(), r * a.sin(), z);
            worst = worst.max(unoct(oct(n)).angle_between(n));
        }
        for n in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            worst = worst.max(unoct(oct(n)).angle_between(n));
        }
        assert!(worst.to_degrees() < 0.01, "{}", worst.to_degrees());
        assert_eq!(unoct(oct(Vec3::ZERO)), Vec3::Z);
    }

    #[test]
    fn row_keeps_the_element_columns() {
        let mut r = glam::Mat4::from_translation(Vec3::new(3.0, 4.0, 5.0)).to_cols_array();
        r[3] = 42.0;
        r[7] = 2.0;
        let d = Deq {
            o: Vec3::new(1.0, 2.0, 3.0),
            s: 0.5,
        };
        let o = d.row(&r);
        assert_eq!((o[3], o[7], o[11], o[15]), (42.0, 2.0, 0.0, 1.0));
        let m = |r: &[f32; 16]| {
            let mut x = *r;
            x[3] = 0.0;
            x[7] = 0.0;
            glam::Mat4::from_cols_array(&x)
        };
        let q = Vec3::new(4.0, 6.0, 8.0);
        let want = m(&r).transform_point3(d.o + q * d.s);
        assert!((m(&o).transform_point3(q) - want).length() < 1e-6);
    }

    /// Every triangle comes out the same: same corners in the same winding (to compression
    /// precision), the provoking vertex normal is the face normal; for both provoking-vertex conventions.
    fn same_triangles(m: &SceneMesh, wide: bool) -> Stream {
        let mut v = vec![];
        tri_vertices(m, &mut v);
        let deq = Deq::of(&bounds(&v));
        let mut out = None;
        for last in [false, true] {
            let s = pack_tris(&v, deq, wide, last);
            let got = unpack(&s, deq);
            assert_eq!(got.len() * 3, v.len());
            for (t, (p, n)) in got.iter().enumerate() {
                let want = &v[t * 3..t * 3 + 3];
                let i = (0..3)
                    .find(|&i| (p[i] - Vec3::from(want[0].pos)).length() <= 2.0 * deq.max_error())
                    .expect("corner");
                for k in 0..3 {
                    let d = (p[(i + k) % 3] - Vec3::from(want[k].pos)).length();
                    assert!(d <= 2.0 * deq.max_error(), "tri {t} corner {k}: {d}");
                }
                let wn = Vec3::from(want[0].normal);
                if wn != Vec3::ZERO {
                    assert!(n.angle_between(wn).to_degrees() < 0.01, "tri {t}");
                }
            }
            for k in 0..s.count() {
                let id = s.vertex_of(k) as usize;
                let ix = (k / 3 * 3) as usize;
                assert_eq!(s.v[id].element, v[ix].element);
                assert_eq!(s.v[id].tag, v[ix].tag);
            }
            out = Some(s);
        }
        out.unwrap()
    }

    #[test]
    fn welding_keeps_every_triangle_corner_order_and_face_normal() {
        let m = grid_mesh(40, 16.0, Vec3::new(100.0, -50.0, 3.0));
        let s = same_triangles(&m, false);
        // flat fill of the wave: no more vertices than triangles, and fewer than three per triangle
        assert!(
            s.v.len() <= m.triangles.len() + m.positions.len(),
            "{}",
            s.v.len()
        );
        let s = same_triangles(&m, true);
        assert!(matches!(s.idx, Idx::U32(_)));
    }

    #[test]
    fn box_welds_to_its_corners() {
        let b = crate::chunks::box_mesh(
            &Aabb {
                min: Vec3::ZERO,
                max: Vec3::ONE,
            },
            3,
            9,
        );
        let s = same_triangles(&b, false);
        assert!(s.v.len() <= 10, "{}", s.v.len());
        // 12 triangles: 96 → at most 10 × 20 + 36 × 2 bytes
        assert!(s.bytes() <= 272, "{}", s.bytes());
        let mut l = vec![];
        crease_vertices(&b, &mut l);
        let ls = pack_lines(&l, Deq::UNIT);
        assert_eq!(ls.count() as usize, l.len());
        assert_eq!(ls.v.len(), 8);
        for k in 0..ls.count() {
            let p = Deq::UNIT.pos(unpack_q(ls.v[ls.vertex_of(k) as usize].q));
            assert_eq!(p, Vec3::from(l[k as usize].pos));
        }
    }

    #[test]
    fn u16_segments_hold_at_most_65536_vertices() {
        // without shared vertices: 30,000 triangles — 90,000 vertices, two runs or more
        let mut v = vec![];
        for t in 0..30_000u32 {
            let o = Vec3::new(t as f32 * 0.01, 0.0, 0.0);
            for p in [o, o + Vec3::X * 0.004, o + Vec3::Y] {
                v.push(Vertex {
                    pos: p.into(),
                    normal: [0.0, 0.0, 1.0],
                    element: t,
                    tag: 0,
                });
            }
        }
        let deq = Deq::of(&bounds(&v));
        let s = pack_tris(&v, deq, false, false);
        assert!(s.segs.len() >= 2);
        let mut next = 0;
        for g in &s.segs {
            assert_eq!(g.first, next);
            next += g.count;
            let end = s.segs.iter().map(|x| x.base).filter(|&b| b > g.base).min();
            let end = end.unwrap_or(s.v.len() as u32);
            assert!(end - g.base <= SEG_VERTS as u32);
        }
        assert_eq!(next, s.count());
        let got = unpack(&s, deq);
        for (t, (p, _)) in got.iter().enumerate() {
            assert!((p[0] - Vec3::from(v[t * 3].pos)).length() <= 2.0 * deq.max_error());
        }
    }
}
