// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tessellation of IFC geometry into triangle meshes (in the frame of the item): extrusions of any
//! profile, revolutions, sweeps, faceted/advanced breps, face sets, surface models, mapped items,
//! CSG primitives, half-space clipping. Curves keep arcs exact until sampled (7.5° steps).

use std::f64::consts::{PI, TAU};

use super::{add, cross, dot, mul, norm, Frame, Rd, Why, V3};
use crate::ifc::reader::Value;

/// Triangle budget of one product (hostile input cannot exhaust memory).
const MAX_TRIS: usize = 4_000_000;
const MAX_DEPTH: usize = 24;

pub(super) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn finite(p: V3) -> bool {
    p.iter().all(|c| c.is_finite())
}

/// Steps of an arc of `sweep` radians.
pub(super) fn arc_steps(sweep: f64) -> usize {
    let n = (sweep.abs() / (PI / 24.0)).ceil();
    if n.is_finite() {
        (n as usize).clamp(1, 96)
    } else {
        1
    }
}

/// Rotates `v` about unit axis `k` by `t` (Rodrigues).
fn rot(v: V3, k: V3, t: f64) -> V3 {
    let (c, s) = (t.cos(), t.sin());
    add(
        add(mul(v, c), mul(cross(k, v), s)),
        mul(k, dot(k, v) * (1.0 - c)),
    )
}

/// Curve piece: straight span or circular arc about centre `c` and unit normal `n` (sweep signed
/// about `n`, right hand).
#[derive(Clone, Copy, Debug)]
pub(super) enum Seg {
    Line(V3, V3),
    Arc {
        a: V3,
        b: V3,
        c: V3,
        n: V3,
        sweep: f64,
    },
}

impl Seg {
    pub fn start(&self) -> V3 {
        match *self {
            Seg::Line(a, _) | Seg::Arc { a, .. } => a,
        }
    }
    pub fn end(&self) -> V3 {
        match *self {
            Seg::Line(_, b) | Seg::Arc { b, .. } => b,
        }
    }
    fn reversed(self) -> Seg {
        match self {
            Seg::Line(a, b) => Seg::Line(b, a),
            Seg::Arc { a, b, c, n, sweep } => Seg::Arc {
                a: b,
                b: a,
                c,
                n,
                sweep: -sweep,
            },
        }
    }
    /// Points after the start (end included).
    fn sample(&self, out: &mut Vec<V3>) {
        match *self {
            Seg::Line(_, b) => out.push(b),
            Seg::Arc { a, b, c, n, sweep } => {
                let k = arc_steps(sweep);
                let r = sub(a, c);
                for i in 1..k {
                    out.push(add(c, rot(r, n, sweep * i as f64 / k as f64)));
                }
                out.push(b);
            }
        }
    }
}

/// Circular arc through three points (None when collinear).
pub(super) fn arc3(a: V3, m: V3, b: V3) -> Option<Seg> {
    let (u, v) = (sub(m, a), sub(b, a));
    let n = cross(u, v);
    let nn = dot(n, n);
    if nn < 1e-24 {
        return None;
    }
    // circumcentre of the triangle a, m, b
    let c = add(
        a,
        mul(
            add(mul(cross(n, u), dot(v, v)), mul(cross(v, n), dot(u, u))),
            0.5 / nn,
        ),
    );
    let n = norm(n)?;
    let ang = |p: V3| {
        let (r0, r1) = (sub(a, c), sub(p, c));
        dot(cross(r0, r1), n).atan2(dot(r0, r1)).rem_euclid(TAU)
    };
    let sweep = ang(b);
    (sweep > 0.0 && ang(m) < sweep).then_some(Seg::Arc { a, b, c, n, sweep })
}

/// Polyline through segments (start, samples), consecutive duplicates dropped.
pub(super) fn polyline(segs: &[Seg]) -> Vec<V3> {
    let mut out = vec![];
    for (i, s) in segs.iter().enumerate() {
        if i == 0 {
            out.push(s.start());
        }
        s.sample(&mut out);
    }
    out.dedup_by(|a, b| len(sub(*a, *b)) < 1e-9);
    out
}

/// Ring of a closed curve: closing point dropped.
fn ring(segs: &[Seg]) -> Vec<V3> {
    let mut p = polyline(segs);
    while p.len() > 1 && len(sub(p[0], p[p.len() - 1])) < 1e-9 {
        p.pop();
    }
    p
}

/// Area of one planar region: outer ring then holes.
pub(super) type Poly = Vec<Vec<V3>>;

/// Triangle mesh.
#[derive(Clone, Debug, Default)]
pub(super) struct Tri {
    pub p: Vec<V3>,
    pub t: Vec<[u32; 3]>,
}

impl Tri {
    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }
    pub fn append(&mut self, o: &Tri) {
        let b = self.p.len() as u32;
        self.p.extend_from_slice(&o.p);
        self.t
            .extend(o.t.iter().map(|t| [t[0] + b, t[1] + b, t[2] + b]));
    }
    /// Affine transform; a mirroring frame flips the winding.
    pub fn transform(&mut self, fr: &Frame) {
        for p in &mut self.p {
            *p = fr.apply(*p);
        }
        if dot(cross(fr.x, fr.y), fr.z) < 0.0 {
            for t in &mut self.t {
                t.swap(1, 2);
            }
        }
    }
    pub fn tri(&mut self, a: V3, b: V3, c: V3) {
        let k = self.p.len() as u32;
        self.p.extend([a, b, c]);
        self.t.push([k, k + 1, k + 2]);
    }
    pub fn quad(&mut self, a: V3, b: V3, c: V3, d: V3) {
        let k = self.p.len() as u32;
        self.p.extend([a, b, c, d]);
        self.t.push([k, k + 1, k + 2]);
        self.t.push([k, k + 2, k + 3]);
    }
    /// Planar polygon with holes, wound like its outer ring.
    pub fn poly(&mut self, rings: &[Vec<V3>]) {
        let tris = tri_poly(rings);
        if tris.is_empty() {
            return;
        }
        let b = self.p.len() as u32;
        for r in rings {
            self.p.extend_from_slice(r);
        }
        self.t.extend(
            tris.iter()
                .map(|t| [b + t[0] as u32, b + t[1] as u32, b + t[2] as u32]),
        );
    }
    pub fn bbox(&self) -> Option<(V3, V3)> {
        let mut it = self.t.iter().flatten().map(|&i| self.p[i as usize]);
        let f = it.next()?;
        Some(it.fold((f, f), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
            )
        }))
    }
    pub fn check(&self) -> Result<(), Why> {
        if self.t.len() > MAX_TRIS {
            return Err("geometry too large");
        }
        if !self.p.iter().all(|p| finite(*p)) {
            return Err("bad geometry");
        }
        Ok(())
    }

    /// Keeps the part where (p − o)·n ≤ 0; the cut is capped where the boundary closes.
    pub fn clip(&mut self, o: V3, n: V3) {
        let d: Vec<f64> = self.p.iter().map(|p| dot(sub(*p, o), n)).collect();
        if d.iter().all(|&x| x <= 1e-9) {
            return;
        }
        let mut p = self.p.clone();
        let mut cut: std::collections::HashMap<(u32, u32), u32> = Default::default();
        let mut at = |p: &mut Vec<V3>, i: u32, j: u32| -> u32 {
            let k = (i.min(j), i.max(j));
            *cut.entry(k).or_insert_with(|| {
                let (a, b) = (k.0 as usize, k.1 as usize);
                let t = d[a] / (d[a] - d[b]);
                p.push(add(p[a], mul(sub(p[b], p[a]), t)));
                (p.len() - 1) as u32
            })
        };
        let mut t = vec![];
        let mut segs: Vec<(u32, u32)> = vec![];
        let inside = |i: u32| d[i as usize] <= 1e-9;
        for tr in &self.t {
            let ins: Vec<bool> = tr.iter().map(|&i| inside(i)).collect();
            match ins.iter().filter(|x| **x).count() {
                3 => t.push(*tr),
                0 => {}
                k => {
                    // rotate so that the odd vertex is first
                    let odd = (0..3).find(|&i| (k == 1) == ins[i]).unwrap_or(0);
                    let (a, b, c) = (tr[odd], tr[(odd + 1) % 3], tr[(odd + 2) % 3]);
                    let ab = at(&mut p, a, b);
                    let ac = at(&mut p, a, c);
                    if k == 1 {
                        t.push([a, ab, ac]);
                        segs.push((ab, ac));
                    } else {
                        t.push([ab, b, c]);
                        t.push([ab, c, ac]);
                        segs.push((ac, ab));
                    }
                }
            }
        }
        self.p = p;
        self.t = t;
        self.cap(&segs, n);
        self.compact();
    }

    /// Drops vertices no triangle uses (clipped-away corners).
    fn compact(&mut self) {
        let mut map = vec![u32::MAX; self.p.len()];
        let mut p = Vec::with_capacity(self.p.len());
        for t in &mut self.t {
            for i in t.iter_mut() {
                let m = &mut map[*i as usize];
                if *m == u32::MAX {
                    *m = p.len() as u32;
                    p.push(self.p[*i as usize]);
                }
                *i = *m;
            }
        }
        self.p = p;
    }

    /// Closes loops of cut segments (directed so that the cap faces `n`).
    fn cap(&mut self, segs: &[(u32, u32)], n: V3) {
        // faces rarely share vertex indices: chain cut points by position (0.1 µm grid)
        let mut canon: std::collections::HashMap<(i64, i64, i64), u32> = Default::default();
        let mut id = |i: u32| {
            let p = self.p[i as usize];
            let k = |c: f64| (c * 1e7).round() as i64;
            *canon.entry((k(p[0]), k(p[1]), k(p[2]))).or_insert(i)
        };
        let segs: Vec<(u32, u32)> = segs
            .iter()
            .map(|&(a, b)| (id(a), id(b)))
            .filter(|(a, b)| a != b)
            .collect();
        let segs = &segs[..];
        let mut next: std::collections::HashMap<u32, u32> = Default::default();
        for &(a, b) in segs {
            next.insert(a, b);
        }
        let mut loops: Vec<Vec<V3>> = vec![];
        let mut used: std::collections::HashSet<u32> = Default::default();
        for &(s, _) in segs {
            if used.contains(&s) {
                continue;
            }
            let mut lp = vec![];
            let mut cur = s;
            let mut closed = false;
            for _ in 0..segs.len() + 1 {
                if !used.insert(cur) {
                    closed = cur == s;
                    break;
                }
                lp.push(self.p[cur as usize]);
                match next.get(&cur) {
                    Some(&x) => cur = x,
                    None => break,
                }
            }
            if closed && lp.len() >= 3 {
                loops.push(lp);
            }
        }
        if loops.is_empty() {
            return;
        }
        // nesting: even depth = outer, odd = hole of the smallest enclosing outer
        let (u, v) = basis(n);
        let flat: Vec<Vec<(f64, f64)>> = loops
            .iter()
            .map(|l| l.iter().map(|p| (dot(*p, u), dot(*p, v))).collect())
            .collect();
        let area: Vec<f64> = flat.iter().map(|r| area2(r).abs()).collect();
        let parent = |i: usize| -> Vec<usize> {
            (0..loops.len())
                .filter(|&j| j != i && area[j] > area[i] && inside2(&flat[j], flat[i][0]))
                .collect()
        };
        let mut groups: Vec<(usize, Vec<usize>)> = vec![];
        let mut holes = vec![];
        for i in 0..loops.len() {
            let ps = parent(i);
            if ps.len() % 2 == 0 {
                groups.push((i, vec![]));
            } else {
                let p = ps
                    .into_iter()
                    .min_by(|a, b| area[*a].total_cmp(&area[*b]))
                    .unwrap_or(i);
                holes.push((i, p));
            }
        }
        for (h, p) in holes {
            if let Some(g) = groups.iter_mut().find(|g| g.0 == p) {
                g.1.push(h);
            }
        }
        for (o, hs) in groups {
            let mut outer = loops[o].clone();
            if area2(&flat[o]) < 0.0 {
                outer.reverse();
            }
            let mut rings = vec![outer];
            for h in hs {
                let mut r = loops[h].clone();
                if area2(&flat[h]) > 0.0 {
                    r.reverse();
                }
                rings.push(r);
            }
            self.poly(&rings);
        }
    }
}

/// `m` minus (half-space ∩ boundary prism): the prism of `ring` (in frame `fr`) along `fr.z`,
/// long enough to pass through `m`, clipped to the removed side of the plane (`o`, `n`).
fn bounded_cut(m: &mut Tri, o: V3, n: V3, fr: &Frame, ring: &[(f64, f64)]) -> bool {
    use ddc_geom::csg::BoolOp;
    let Some((lo, hi)) = m.bbox() else {
        return true;
    };
    let reach = len(sub(hi, lo)) + len(sub(fr.o, lo)) + 1.0;
    let r: Vec<V3> = ring.iter().map(|p| [p.0, p.1, -reach]).collect();
    let mut tool = Tri::default();
    extrude(&oriented(vec![vec![r]]), [0.0, 0.0, 2.0 * reach], &mut tool);
    tool.transform(fr);
    tool.clip(o, mul(n, -1.0));
    if tool.is_empty() {
        return true;
    }
    let mesh = |t: &Tri| ddc_geom::mesh::Mesh {
        positions: t.p.iter().map(|p| ddc_geom::v3(p[0], p[1], p[2])).collect(),
        triangles: t.t.clone(),
        face_of: vec![0; t.t.len()],
    };
    match ddc_geom::csg2::mesh_boolean(&mesh(m), &mesh(&tool), BoolOp::Subtract, Default::default())
    {
        Ok(r) => {
            m.p = r.mesh.positions.iter().map(|p| [p.x, p.y, p.z]).collect();
            m.t = r.mesh.triangles;
            true
        }
        Err(_) => false,
    }
}

fn basis(n: V3) -> (V3, V3) {
    let a = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u = norm(sub(a, mul(n, dot(a, n)))).unwrap_or([1.0, 0.0, 0.0]);
    (u, cross(n, u))
}

fn area2(r: &[(f64, f64)]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        / 2.0
}

pub(super) fn inside2(r: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut c = false;
    let n = r.len();
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

fn newell(r: &[V3]) -> V3 {
    let mut n = [0.0; 3];
    for i in 0..r.len() {
        let (a, b) = (r[i], r[(i + 1) % r.len()]);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

/// Triangulates a planar polygon with holes; indices into the rings concatenated. Triangles
/// follow the outer ring's orientation.
pub(super) fn tri_poly(rings: &[Vec<V3>]) -> Vec<[usize; 3]> {
    let Some(outer) = rings.first() else {
        return vec![];
    };
    if outer.len() < 3 {
        return vec![];
    }
    let Some(n) = norm(newell(outer)) else {
        return vec![];
    };
    let (u, v) = basis(n);
    let mut pts: Vec<(f64, f64)> = vec![];
    let mut starts = vec![];
    for r in rings {
        starts.push(pts.len());
        pts.extend(r.iter().map(|p| (dot(*p, u), dot(*p, v))));
    }
    if outer.len() == 3 && rings.len() == 1 {
        return vec![[0, 1, 2]];
    }
    let ring_ix = |k: usize| -> Vec<usize> { (starts[k]..starts[k] + rings[k].len()).collect() };
    let mut poly = ring_ix(0);
    let idx_area = |ix: &[usize]| area2(&ix.iter().map(|&i| pts[i]).collect::<Vec<_>>());
    if idx_area(&poly) < 0.0 {
        poly.reverse();
    }
    let mut holes: Vec<Vec<usize>> = (1..rings.len())
        .filter(|&k| rings[k].len() >= 3)
        .map(|k| {
            let mut h = ring_ix(k);
            if idx_area(&h) > 0.0 {
                h.reverse();
            }
            h
        })
        .collect();
    holes.sort_by(|a, b| {
        let mx = |h: &Vec<usize>| h.iter().map(|&i| pts[i].0).fold(f64::MIN, f64::max);
        mx(b).total_cmp(&mx(a))
    });
    for (hk, h) in holes.iter().enumerate() {
        let (mi, _) = h
            .iter()
            .enumerate()
            .max_by(|a, b| pts[*a.1].0.total_cmp(&pts[*b.1].0))
            .unwrap_or((0, &0));
        let m = pts[h[mi]];
        let blocked = |q: (f64, f64)| {
            let crosses = |r: &[usize]| {
                (0..r.len()).any(|i| {
                    let (a, b) = (pts[r[i]], pts[r[(i + 1) % r.len()]]);
                    seg_cross(m, q, a, b)
                })
            };
            crosses(&poly) || holes[hk..].iter().any(|r| crosses(r))
        };
        let mut best: Option<(f64, usize)> = None;
        let mut order: Vec<usize> = (0..poly.len()).collect();
        let d2 = |i: usize| {
            let q = pts[poly[i]];
            (q.0 - m.0).powi(2) + (q.1 - m.1).powi(2)
        };
        order.sort_by(|a, b| d2(*a).total_cmp(&d2(*b)));
        for &i in order.iter().take(64) {
            if !blocked(pts[poly[i]]) {
                best = Some((d2(i), i));
                break;
            }
        }
        let pi = best.map_or(order.first().copied().unwrap_or(0), |b| b.1);
        let mut ins = vec![];
        for k in 0..=h.len() {
            ins.push(h[(mi + k) % h.len()]);
        }
        ins.push(poly[pi]);
        poly.splice(pi + 1..pi + 1, ins);
    }
    ear_clip(&pts, poly)
}

fn seg_cross(p: (f64, f64), q: (f64, f64), a: (f64, f64), b: (f64, f64)) -> bool {
    let o = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let same =
        |x: (f64, f64), y: (f64, f64)| (x.0 - y.0).abs() < 1e-12 && (x.1 - y.1).abs() < 1e-12;
    if same(p, a) || same(p, b) || same(q, a) || same(q, b) {
        return false;
    }
    let (d1, d2, d3, d4) = (o(a, b, p), o(a, b, q), o(p, q, a), o(p, q, b));
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

fn ear_clip(pts: &[(f64, f64)], mut ix: Vec<usize>) -> Vec<[usize; 3]> {
    let mut out = Vec::with_capacity(ix.len());
    let cr = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let scale = pts
        .iter()
        .fold(0.0f64, |m, p| m.max(p.0.abs()).max(p.1.abs()))
        .max(1e-9);
    let eps = 1e-14 * scale * scale;
    let mut i = 0;
    let mut stall = 0;
    while ix.len() > 3 {
        let n = ix.len();
        let (pa, pb, pc) = (ix[(i + n - 1) % n], ix[i % n], ix[(i + 1) % n]);
        let (a, b, c) = (pts[pa], pts[pb], pts[pc]);
        let k = cr(a, b, c);
        let same = |x: (f64, f64), y: (f64, f64)| x.0 == y.0 && x.1 == y.1;
        let ear = k > eps
            && !ix.iter().any(|&j| {
                let p = pts[j];
                if same(p, a) || same(p, b) || same(p, c) {
                    return false;
                }
                cr(a, b, p) >= 0.0 && cr(b, c, p) >= 0.0 && cr(c, a, p) >= 0.0
            });
        if ear || k.abs() <= eps && stall > n || stall > 2 * n {
            if k.abs() > eps || stall > 2 * n {
                out.push([pa, pb, pc]);
            }
            ix.remove(i % n);
            stall = 0;
            if i >= ix.len() {
                i = 0;
            }
        } else {
            i = (i + 1) % n;
            stall += 1;
        }
    }
    if ix.len() == 3 {
        out.push([ix[0], ix[1], ix[2]]);
    }
    out
}

/// Frame of a cartesian transformation operator (2D or 3D, uniform or not).
impl Rd<'_> {
    pub(super) fn operator(&self, id: u32) -> Result<Frame, Why> {
        let e = self.ent(id, &[])?;
        let ty = self.f.type_of(id).unwrap_or("");
        let three = ty.contains("3D");
        let o = match Self::arg(e, 2).as_ref() {
            Some(p) => self.point(p)?,
            None => [0.0; 3],
        };
        let a1 = self.direction(Self::arg(e, 0))?;
        let a2 = self.direction(Self::arg(e, 1))?;
        let s = Self::arg(e, 3).as_f64().unwrap_or(1.0);
        let (s2, s3) = if ty.ends_with("NONUNIFORM") {
            if three {
                (
                    Self::arg(e, 5).as_f64().unwrap_or(s),
                    Self::arg(e, 6).as_f64().unwrap_or(s),
                )
            } else {
                (Self::arg(e, 4).as_f64().unwrap_or(s), s)
            }
        } else {
            (s, s)
        };
        let z = if three {
            self.direction(Self::arg(e, 4))?.unwrap_or([0.0, 0.0, 1.0])
        } else {
            [0.0, 0.0, 1.0]
        };
        let mut fr = Frame::from_axes(o, Some(z), a1)?;
        if let Some(y2) = a2 {
            if dot(y2, fr.y) < 0.0 {
                fr.y = mul(fr.y, -1.0);
            }
        }
        if ![s, s2, s3].iter().all(|v| v.is_finite() && *v != 0.0) {
            return Err("bad transformation scale");
        }
        fr.x = mul(fr.x, s);
        fr.y = mul(fr.y, s2);
        fr.z = mul(fr.z, s3);
        Ok(fr)
    }

    /// Segments of a curve in its own coordinates.
    pub(super) fn segs(&self, id: u32, depth: usize) -> Result<Vec<Seg>, Why> {
        if depth > MAX_DEPTH {
            return Err("curve nesting too deep");
        }
        let e = self.ent(id, &[])?;
        let ty = self.f.type_of(id).unwrap_or("");
        match ty {
            "IFCPOLYLINE" => {
                let p = Self::refs(e, 0)
                    .into_iter()
                    .map(|p| self.point(p))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(p.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
            }
            "IFCINDEXEDPOLYCURVE" => self.indexed_curve(e),
            "IFCCOMPOSITECURVE"
            | "IFCCOMPOSITECURVEONSURFACE"
            | "IFCBOUNDARYCURVE"
            | "IFCOUTERBOUNDARYCURVE" => {
                let mut out = vec![];
                for s in Self::refs(e, 0) {
                    let se = self.ent(s, &[])?;
                    match self.f.type_of(s) {
                        Some(
                            "IFCCOMPOSITECURVESEGMENT" | "IFCREPARAMETRISEDCOMPOSITECURVESEGMENT",
                        ) => {
                            let same = Self::arg(se, 1).as_enum() != Some("F");
                            let mut p = self.segs(Self::reference(se, 2)?, depth + 1)?;
                            if !same {
                                p.reverse();
                                p.iter_mut().for_each(|s| *s = s.reversed());
                            }
                            out.extend(p);
                        }
                        Some("IFCCURVESEGMENT") => out.extend(self.curve_segment(se)?),
                        _ => return Err("unsupported curve segment"),
                    }
                }
                Ok(out)
            }
            "IFCTRIMMEDCURVE" => self.trimmed(e, depth),
            "IFCCIRCLE" => {
                let fr = self.axis_placement(Self::arg(e, 0))?;
                let r = Self::arg(e, 1).as_f64().ok_or("bad circle")? * self.scale;
                let a = fr.apply([r, 0.0, 0.0]);
                Ok(vec![Seg::Arc {
                    a,
                    b: a,
                    c: fr.o,
                    n: fr.z,
                    sweep: TAU,
                }])
            }
            "IFCELLIPSE" => {
                let fr = self.axis_placement(Self::arg(e, 0))?;
                let a = Self::arg(e, 1).as_f64().ok_or("bad ellipse")? * self.scale;
                let b = Self::arg(e, 2).as_f64().ok_or("bad ellipse")? * self.scale;
                Ok(ellipse(&fr, a, b, 0.0, TAU))
            }
            "IFCBSPLINECURVEWITHKNOTS" | "IFCRATIONALBSPLINECURVEWITHKNOTS" => {
                let p = self.bspline_points(e, ty == "IFCRATIONALBSPLINECURVEWITHKNOTS")?;
                Ok(p.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
            }
            "IFCBEZIERCURVE" | "IFCRATIONALBEZIERCURVE" => {
                let p = self.bezier_points(e, ty == "IFCRATIONALBEZIERCURVE")?;
                Ok(p.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
            }
            "IFCOFFSETCURVE2D" => {
                let basis = self.segs(Self::reference(e, 0)?, depth + 1)?;
                let d = Self::arg(e, 1).as_f64().ok_or("bad offset curve")? * self.scale;
                offset_segs(&basis, d)
            }
            "IFCOFFSETCURVE3D" => {
                let basis = self.segs(Self::reference(e, 0)?, depth + 1)?;
                let d = Self::arg(e, 1).as_f64().ok_or("bad offset curve")? * self.scale;
                let rf = self
                    .direction(Self::arg(e, 3))?
                    .ok_or("bad offset curve")?;
                offset_3d(&basis, d, rf)
            }
            "IFCGRADIENTCURVE" => self.gradient(e, depth),
            // cant only tilts the sections; the centre line is the base curve
            "IFCSEGMENTEDREFERENCECURVE" => self.segs(Self::reference(e, 2)?, depth + 1),
            _ => Err("unsupported curve"),
        }
    }

    /// IFC4X3 gradient curve: the plan path of the base curve lifted by the vertical profile
    /// (curve segments in distance-along / height coordinates). Vertices sit at both curves' vertices.
    fn gradient(&self, e: &crate::ifc::reader::Entity, depth: usize) -> Result<Vec<Seg>, Why> {
        // stations (distance along the plan, point) of the base curve; arcs measured exactly
        let base = self.segs(Self::reference(e, 2)?, depth + 1)?;
        let mut st: Vec<(f64, V3)> = vec![];
        let mut dist = 0.0;
        for g in &base {
            let mut pts = vec![];
            g.sample(&mut pts);
            let plan = match *g {
                Seg::Line(a, b) => (b[0] - a[0]).hypot(b[1] - a[1]),
                Seg::Arc { a, c, sweep, .. } => sweep.abs() * (a[0] - c[0]).hypot(a[1] - c[1]),
            };
            if st.is_empty() {
                st.push((0.0, g.start()));
            }
            let k = pts.len();
            for (i, q) in pts.into_iter().enumerate() {
                let d = dist + plan * (i + 1) as f64 / k as f64;
                if d - st[st.len() - 1].0 > 1e-9 {
                    st.push((d, q));
                }
            }
            dist += plan;
        }
        if st.len() < 2 {
            return Err("bad gradient base");
        }
        let total = st[st.len() - 1].0;
        let mut prof = vec![];
        for s in Self::refs(e, 0) {
            let se = self.ent(s, &[])?;
            if self.f.type_of(s) != Some("IFCCURVESEGMENT") {
                return Err("unsupported curve segment");
            }
            prof.extend(self.curve_segment(se)?);
        }
        let pp = polyline(&prof);
        if pp.len() < 2 || pp.windows(2).any(|w| w[1][0] < w[0][0] - 1e-9) {
            return Err("bad gradient profile");
        }
        let lo = pp[0][0].max(0.0);
        let hi = pp[pp.len() - 1][0].min(total);
        if !(hi - lo > 1e-9) {
            return Err("gradient outside its base");
        }
        let mut ds = vec![lo, hi];
        ds.extend(
            st.iter()
                .map(|q| q.0)
                .chain(pp.iter().map(|q| q[0]))
                .filter(|d| *d > lo && *d < hi),
        );
        ds.sort_by(|a, b| a.total_cmp(b));
        ds.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        let height = |d: f64| {
            let i = (1..pp.len() - 1)
                .find(|&i| pp[i][0] >= d)
                .unwrap_or(pp.len() - 1);
            let (p, q) = (pp[i - 1], pp[i]);
            if q[0] - p[0] > 1e-12 {
                p[1] + (q[1] - p[1]) * ((d - p[0]) / (q[0] - p[0])).clamp(0.0, 1.0)
            } else {
                q[1]
            }
        };
        let plan = |d: f64| {
            let i = (1..st.len() - 1)
                .find(|&i| st[i].0 >= d)
                .unwrap_or(st.len() - 1);
            let ((d0, p), (d1, q)) = (st[i - 1], st[i]);
            let f = ((d - d0) / (d1 - d0)).clamp(0.0, 1.0);
            add(p, mul(sub(q, p), f))
        };
        let pts: Vec<V3> = ds
            .iter()
            .map(|&d| {
                let p = plan(d);
                [p[0], p[1], p[2] + height(d)]
            })
            .collect();
        Ok(pts.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
    }

    /// IFC4X3 curve segment: the parent trimmed from SegmentStart over the signed SegmentLength,
    /// then moved so its start point and travel tangent sit on Placement's origin and x axis.
    fn curve_segment(&self, se: &crate::ifc::reader::Entity) -> Result<Vec<Seg>, Why> {
        let place = self.axis_placement(Self::arg(se, 1))?;
        let measure = |i: usize| -> Result<(f64, bool), Why> {
            let v = Self::arg(se, i);
            let param = matches!(v, Value::Typed(t, _) if t == "IFCPARAMETERVALUE");
            let x = v.as_f64().filter(|x| x.is_finite());
            Ok((x.ok_or("bad curve segment")?, param))
        };
        let (start, length) = (measure(2)?, measure(3)?);
        let parent = Self::reference(se, 4)?;
        let pe = self.ent(parent, &[])?;
        let unit = self.scale;
        // parent pieces between arc lengths s0 and s1, start point, travel tangent, plane normal
        let (segs, p0, t0, up): (Vec<Seg>, V3, V3, V3) = match self.f.type_of(parent) {
            Some("IFCLINE") => {
                let p = self.point(Self::reference(pe, 0)?)?;
                let vec = self.ent(Self::reference(pe, 1)?, &["IFCVECTOR"])?;
                let d = self.direction(Self::arg(vec, 0))?.ok_or("bad direction")?;
                let k = Self::arg(vec, 1).as_f64().ok_or("bad direction")? * self.scale;
                if !(k.abs() > 1e-12) {
                    return Err("bad direction");
                }
                let (s0, s1) = seg_span(start, length, unit, |u| u * k, |s| s / k);
                if (s1 - s0).abs() < 1e-12 {
                    return Ok(vec![]);
                }
                let (a, b) = (add(p, mul(d, s0)), add(p, mul(d, s1)));
                (vec![Seg::Line(a, b)], a, mul(d, (s1 - s0).signum()), [0.0, 0.0, 1.0])
            }
            Some("IFCCIRCLE") => {
                let fr = self.axis_placement(Self::arg(pe, 0))?;
                let r = Self::arg(pe, 1).as_f64().ok_or("bad circle")? * self.scale;
                if !(r > 0.0) || !(self.angle > 0.0) {
                    return Err("bad circle");
                }
                let ka = self.angle * r;
                let (s0, s1) = seg_span(start, length, unit, |u| u * ka, |s| s / ka);
                if (s1 - s0).abs() < 1e-12 {
                    return Ok(vec![]);
                }
                let (t0, t1) = (s0 / r, s1 / r);
                let pt = |t: f64| fr.apply([r * t.cos(), r * t.sin(), 0.0]);
                let a = pt(t0);
                let tan = fr.dir([-t0.sin(), t0.cos(), 0.0]);
                let arc = Seg::Arc {
                    a,
                    b: pt(t1),
                    c: fr.o,
                    n: fr.z,
                    sweep: t1 - t0,
                };
                (vec![arc], a, mul(tan, (t1 - t0).signum()), fr.z)
            }
            Some("IFCPOLYLINE") => {
                let pts = Self::refs(pe, 0)
                    .into_iter()
                    .map(|p| self.point(p))
                    .collect::<Result<Vec<_>, _>>()?;
                if pts.len() < 2 {
                    return Err("bad polyline");
                }
                let lens: Vec<f64> = pts.windows(2).map(|w| len(sub(w[1], w[0]))).collect();
                let mut cum = vec![0.0];
                for l in &lens {
                    cum.push(cum[cum.len() - 1] + l);
                }
                let last = lens.len() - 1;
                // parameter i at vertex i, linear within each span
                let to_s = |u: f64| {
                    let i = (u.floor().max(0.0) as usize).min(last);
                    cum[i] + (u - i as f64) * lens[i]
                };
                let span_of = |s: f64| (0..last).find(|&i| s < cum[i + 1]).unwrap_or(last);
                let to_u = |s: f64| {
                    let i = span_of(s);
                    i as f64 + if lens[i] > 1e-12 { (s - cum[i]) / lens[i] } else { 0.0 }
                };
                let (s0, s1) = seg_span(start, length, unit, to_s, to_u);
                let tot = cum[lens.len()];
                let tol = 1e-9 * tot.max(1.0);
                if [s0, s1].iter().any(|s| *s < -tol || *s > tot + tol) {
                    return Err("curve segment beyond its parent");
                }
                let at = |s: f64| {
                    let i = span_of(s);
                    let f = if lens[i] > 1e-12 { (s - cum[i]) / lens[i] } else { 0.0 };
                    add(pts[i], mul(sub(pts[i + 1], pts[i]), f))
                };
                let (lo, hi) = (s0.min(s1), s0.max(s1));
                let mut p = vec![at(lo)];
                p.extend((1..=last).filter(|&k| cum[k] > lo && cum[k] < hi).map(|k| pts[k]));
                p.push(at(hi));
                p.dedup_by(|a, b| len(sub(*a, *b)) < 1e-9);
                if s1 < s0 {
                    p.reverse();
                }
                let t = if p.len() > 1 { sub(p[1], p[0]) } else { [0.0; 3] };
                let segs = p.windows(2).map(|w| Seg::Line(w[0], w[1])).collect();
                (segs, p[0], t, [0.0, 0.0, 1.0])
            }
            Some(
                ty @ ("IFCCLOTHOID"
                | "IFCCOSINESPIRAL"
                | "IFCSINESPIRAL"
                | "IFCSECONDORDERPOLYNOMIALSPIRAL"
                | "IFCTHIRDORDERPOLYNOMIALSPIRAL"
                | "IFCSEVENTHORDERPOLYNOMIALSPIRAL"),
            ) => {
                // transition curves start at Position's origin along its x axis; the parameter is arc length
                let fr = self.axis_placement(Self::arg(pe, 0))?;
                let terms: Vec<Option<f64>> = (1..9)
                    .map(|i| Self::arg(pe, i).as_f64().filter(|x| x.is_finite()))
                    .collect();
                let (s0, s1) = seg_span(start, length, unit, |u| u * unit, |s| s / unit);
                if (s1 - s0).abs() < 1e-12 {
                    return Ok(vec![]);
                }
                // the sine and cosine periods use L = |SegmentLength|
                let kappa =
                    spiral_curvature(ty, &terms, self.scale, (s1 - s0).abs()).ok_or("bad spiral")?;
                let (pts, th0) = spiral_points(&*kappa, s0, s1);
                let q: Vec<V3> = pts.iter().map(|p| fr.apply([p[0], p[1], 0.0])).collect();
                let tan = fr.dir([th0.cos(), th0.sin(), 0.0]);
                let segs = q.windows(2).map(|w| Seg::Line(w[0], w[1])).collect();
                (segs, q[0], mul(tan, (s1 - s0).signum()), fr.z)
            }
            _ => return Err("unsupported curve segment"),
        };
        let Some(x) = norm(t0) else {
            return Ok(vec![]); // zero length
        };
        let y = norm(cross(up, x))
            .or_else(|| norm(cross([0.0, 0.0, 1.0], x)))
            .or_else(|| norm(cross([0.0, 1.0, 0.0], x)))
            .ok_or("bad curve segment")?;
        let z = cross(x, y);
        let dir = |v: V3| place.dir([dot(v, x), dot(v, y), dot(v, z)]);
        let pt = |q: V3| add(place.o, dir(sub(q, p0)));
        Ok(segs
            .into_iter()
            .map(|s| match s {
                Seg::Line(a, b) => Seg::Line(pt(a), pt(b)),
                Seg::Arc { a, b, c, n, sweep } => Seg::Arc {
                    a: pt(a),
                    b: pt(b),
                    c: pt(c),
                    n: dir(n),
                    sweep,
                },
            })
            .collect())
    }

    fn indexed_curve(&self, e: &crate::ifc::reader::Entity) -> Result<Vec<Seg>, Why> {
        let pts = self.point_list(
            Self::reference(e, 0)?,
            &["IFCCARTESIANPOINTLIST2D", "IFCCARTESIANPOINTLIST3D"],
        )?;
        let at = |i: &Value| -> Result<V3, Why> {
            let k = i.as_f64().ok_or("bad index")? as i64;
            if k < 1 || k as usize > pts.len() {
                return Err("bad index");
            }
            Ok(pts[k as usize - 1])
        };
        let Some(list) = Self::arg(e, 1).as_list() else {
            return Ok(pts.windows(2).map(|w| Seg::Line(w[0], w[1])).collect());
        };
        let mut out = vec![];
        for s in list {
            let Value::Typed(t, a) = s else {
                return Err("bad segment");
            };
            let ix: &[Value] = match a.as_slice() {
                [Value::List(l)] => l,
                l => l,
            };
            match (t.as_str(), ix.len()) {
                ("IFCLINEINDEX", n) if n >= 2 => {
                    for w in ix.windows(2) {
                        out.push(Seg::Line(at(&w[0])?, at(&w[1])?));
                    }
                }
                ("IFCARCINDEX", 3) => {
                    let (a, m, b) = (at(&ix[0])?, at(&ix[1])?, at(&ix[2])?);
                    out.push(arc3(a, m, b).unwrap_or(Seg::Line(a, b)));
                }
                _ => return Err("bad segment"),
            }
        }
        Ok(out)
    }

    pub(super) fn point_list(&self, id: u32, ty: &[&str]) -> Result<Vec<V3>, Why> {
        let pl = self.leaf(id, ty)?;
        let rows = Self::at(&pl, 0).as_list().ok_or("bad point list")?;
        rows.iter()
            .map(|row| {
                let v = row.as_list().ok_or("bad point list")?;
                let c = |k: usize| v.get(k).and_then(|x| x.as_f64());
                let x = c(0).ok_or("bad point list")?;
                let y = c(1).ok_or("bad point list")?;
                Ok(mul([x, y, c(2).unwrap_or(0.0)], self.scale))
            })
            .collect()
    }

    fn trim_value(
        &self,
        e: &crate::ifc::reader::Entity,
        i: usize,
        param: bool,
    ) -> Option<Result<TrimAt, Why>> {
        let sel = Self::arg(e, i).as_list()?;
        let cart = sel
            .iter()
            .find_map(|s| s.as_ref())
            .map(|r| self.point(r).map(TrimAt::Point));
        let par = sel.iter().find_map(|s| match s {
            Value::Typed(t, a) if t == "IFCPARAMETERVALUE" => a
                .first()
                .and_then(|v| v.as_f64())
                .map(|v| Ok(TrimAt::Param(v))),
            Value::Real(v) => Some(Ok(TrimAt::Param(*v))),
            _ => None,
        });
        if param {
            par.or(cart)
        } else {
            cart.or(par)
        }
    }

    fn trimmed(&self, e: &crate::ifc::reader::Entity, depth: usize) -> Result<Vec<Seg>, Why> {
        let basis = Self::reference(e, 0)?;
        let be = self.ent(basis, &[])?;
        let param = Self::arg(e, 4).as_enum() == Some("PARAMETER");
        let t1 = self.trim_value(e, 1, param).ok_or("bad trim")??;
        let t2 = self.trim_value(e, 2, param).ok_or("bad trim")??;
        let sense = Self::arg(e, 3).as_enum() != Some("F");
        match self.f.type_of(basis).unwrap_or("") {
            "IFCLINE" => {
                let p = self.point(Self::reference(be, 0)?)?;
                let vec = self.ent(Self::reference(be, 1)?, &["IFCVECTOR"])?;
                let d = self.direction(Self::arg(vec, 0))?.ok_or("bad direction")?;
                let mag = Self::arg(vec, 1).as_f64().ok_or("bad direction")?;
                let at = |t: TrimAt| match t {
                    TrimAt::Point(q) => q,
                    TrimAt::Param(t) => add(p, mul(d, t * mag * self.scale)),
                };
                Ok(vec![Seg::Line(at(t1), at(t2))])
            }
            "IFCCIRCLE" | "IFCELLIPSE" => {
                let fr = self.axis_placement(Self::arg(be, 0))?;
                let ra = Self::arg(be, 1).as_f64().ok_or("bad circle")? * self.scale;
                let circle = self.f.type_of(basis) == Some("IFCCIRCLE");
                let rb = if circle {
                    ra
                } else {
                    Self::arg(be, 2).as_f64().ok_or("bad ellipse")? * self.scale
                };
                if !(ra > 0.0 && rb > 0.0) {
                    return Err("bad circle");
                }
                let ang = |t: TrimAt| match t {
                    TrimAt::Param(v) => v * self.angle,
                    TrimAt::Point(q) => {
                        let r = sub(q, fr.o);
                        (dot(r, fr.y) / rb).atan2(dot(r, fr.x) / ra)
                    }
                };
                let (a0, a1) = (ang(t1), ang(t2));
                let mut sweep = if sense {
                    (a1 - a0).rem_euclid(TAU)
                } else {
                    -(a0 - a1).rem_euclid(TAU)
                };
                if sweep.abs() < 1e-12 {
                    sweep = if sense { TAU } else { -TAU };
                }
                if circle {
                    let pt = |a: f64| fr.apply([ra * a.cos(), ra * a.sin(), 0.0]);
                    let (a, b) = match (t1, t2) {
                        (TrimAt::Point(p), TrimAt::Point(q)) => (p, q),
                        _ => (pt(a0), pt(a0 + sweep)),
                    };
                    Ok(vec![Seg::Arc {
                        a,
                        b,
                        c: fr.o,
                        n: fr.z,
                        sweep,
                    }])
                } else {
                    Ok(ellipse(&fr, ra, rb, a0, sweep))
                }
            }
            _ => {
                // other basis curves: trimmed by the nearest sampled points
                let segs = self.segs(basis, depth + 1)?;
                let pts = polyline(&segs);
                let near = |t: TrimAt| match t {
                    TrimAt::Point(q) => (0..pts.len())
                        .min_by(|&a, &b| len(sub(pts[a], q)).total_cmp(&len(sub(pts[b], q))))
                        .unwrap_or(0),
                    TrimAt::Param(_) => 0,
                };
                let (i, j) = (near(t1), near(t2));
                let mut p: Vec<V3> = if i <= j {
                    pts[i..=j.min(pts.len().saturating_sub(1))].to_vec()
                } else {
                    let mut v = pts[j..=i].to_vec();
                    v.reverse();
                    v
                };
                if !sense {
                    p.reverse();
                }
                Ok(p.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
            }
        }
    }

    fn bspline_points(
        &self,
        e: &crate::ifc::reader::Entity,
        rational: bool,
    ) -> Result<Vec<V3>, Why> {
        let deg = Self::arg(e, 0).as_f64().ok_or("bad spline")? as usize;
        let ctrl = Self::refs(e, 1)
            .into_iter()
            .map(|p| self.point(p))
            .collect::<Result<Vec<_>, _>>()?;
        let mults: Vec<usize> = Self::arg(e, 5)
            .as_list()
            .ok_or("bad spline")?
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0).clamp(0.0, 64.0) as usize)
            .collect();
        let ks: Vec<f64> = Self::arg(e, 6)
            .as_list()
            .ok_or("bad spline")?
            .iter()
            .map(|v| v.as_f64().unwrap_or(f64::NAN))
            .collect();
        let mut knots = vec![];
        for (m, k) in mults.iter().zip(&ks) {
            knots.extend(std::iter::repeat_n(*k, *m));
        }
        let w: Option<Vec<f64>> = if rational {
            Some(
                Self::arg(e, 8)
                    .as_list()
                    .ok_or("bad spline")?
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(1.0))
                    .collect(),
            )
        } else {
            None
        };
        spline_samples(deg, &ctrl, w.as_deref(), &knots)
    }

    /// Bezier curve (several spans when the point count is `k * degree + 1`) sampled like a
    /// clamped B-spline.
    fn bezier_points(
        &self,
        e: &crate::ifc::reader::Entity,
        rational: bool,
    ) -> Result<Vec<V3>, Why> {
        let deg = Self::arg(e, 0).as_f64().ok_or("bad bezier")?;
        if !(1.0..=16.0).contains(&deg) {
            return Err("bad bezier");
        }
        let deg = deg as usize;
        let ctrl = Self::refs(e, 1)
            .into_iter()
            .map(|p| self.point(p))
            .collect::<Result<Vec<_>, _>>()?;
        let w: Option<Vec<f64>> = if rational {
            Some(
                Self::arg(e, 5)
                    .as_list()
                    .ok_or("bad bezier")?
                    .iter()
                    .map(|v| v.as_f64().unwrap_or(f64::NAN))
                    .collect(),
            )
        } else {
            None
        };
        bezier_samples(deg, &ctrl, w.as_deref())
    }
}

/// Clamped knot vector of a piecewise Bezier curve (`None` unless `n = k * deg + 1`).
fn bezier_knots(deg: usize, n: usize) -> Option<Vec<f64>> {
    if deg == 0 || n <= deg || (n - 1) % deg != 0 {
        return None;
    }
    let spans = (n - 1) / deg;
    let mut k = vec![0.0; deg + 1];
    for i in 1..spans {
        k.extend(std::iter::repeat_n(i as f64, deg));
    }
    k.extend(std::iter::repeat_n(spans as f64, deg + 1));
    Some(k)
}

/// Points of a (rational) Bezier curve of degree `deg`; refuses degenerate input.
fn bezier_samples(deg: usize, ctrl: &[V3], w: Option<&[f64]>) -> Result<Vec<V3>, Why> {
    let knots = bezier_knots(deg, ctrl.len()).ok_or("bad bezier")?;
    if !ctrl.iter().all(|p| finite(*p)) || ctrl.iter().all(|p| len(sub(*p, ctrl[0])) < 1e-12) {
        return Err("bad bezier");
    }
    spline_samples(deg, ctrl, w, &knots)
}

/// Samples a B-spline over its valid parameter range (step count follows the control count).
fn spline_samples(
    deg: usize,
    ctrl: &[V3],
    w: Option<&[f64]>,
    knots: &[f64],
) -> Result<Vec<V3>, Why> {
    let n = ctrl.len();
    if deg == 0
        || deg > 16
        || n <= deg
        || knots.len() != n + deg + 1
        || knots.iter().any(|k| !k.is_finite())
        || knots.windows(2).any(|w| w[1] < w[0])
        || w.is_some_and(|w| w.len() != n || w.iter().any(|x| !(x.is_finite() && *x > 0.0)))
    {
        return Err("bad spline");
    }
    let (u0, u1) = (knots[deg], knots[n]);
    if !(u1 > u0) {
        return Err("bad spline");
    }
    let steps = (n * 8).clamp(16, 256);
    Ok((0..=steps)
        .map(|i| {
            de_boor(
                deg,
                ctrl,
                w,
                knots,
                u0 + (u1 - u0) * i as f64 / steps as f64,
            )
        })
        .collect())
}

/// Curvature as a function of arc length for the IFC4X3 transition curves. Every term A_p is a
/// length (file units times `scale`) and a term of power p adds `s^p * sign(A_p) / |A_p|^(p+1)`;
/// an absent or zero term adds nothing. Polynomial terms are listed highest power first; the
/// clothoid is `s / (A*|A|)`. Sine spiral (SineTerm A2, LinearTerm A1, ConstantTerm A0):
/// `1/A0 + s/(A1*|A1|) + sin(2*pi*s/L)/A2`; cosine spiral (CosineTerm A1, ConstantTerm A0):
/// `1/A0 + cos(pi*s/L)/A1`, where `seg_len` is L = |SegmentLength| of the curve segment.
fn spiral_curvature(
    ty: &str,
    terms: &[Option<f64>],
    scale: f64,
    seg_len: f64,
) -> Option<Box<dyn Fn(f64) -> f64>> {
    let t = |i: usize| terms.get(i).copied().flatten();
    let inv = |v: Option<f64>, p: i32| -> f64 {
        match v.map(|x| x * scale) {
            Some(a) if a.abs() > 0.0 => a.signum() / a.abs().powi(p + 1),
            _ => 0.0,
        }
    };
    match ty {
        "IFCCLOTHOID" => {
            let a = t(0)? * scale;
            if !(a.abs() > 1e-12) {
                return None;
            }
            let k = 1.0 / (a * a.abs());
            Some(Box::new(move |s| s * k))
        }
        "IFCSECONDORDERPOLYNOMIALSPIRAL"
        | "IFCTHIRDORDERPOLYNOMIALSPIRAL"
        | "IFCSEVENTHORDERPOLYNOMIALSPIRAL" => {
            let n: usize = match ty {
                "IFCSECONDORDERPOLYNOMIALSPIRAL" => 3,
                "IFCTHIRDORDERPOLYNOMIALSPIRAL" => 4,
                _ => 8,
            };
            let c: Vec<f64> = (0..n).map(|j| inv(t(j), (n - 1 - j) as i32)).collect();
            if c.iter().all(|x| *x == 0.0) {
                return None;
            }
            // Horner, highest power first
            Some(Box::new(move |s| c.iter().fold(0.0, |acc, k| acc * s + k)))
        }
        "IFCSINESPIRAL" | "IFCCOSINESPIRAL" => {
            let sine = ty == "IFCSINESPIRAL";
            if !(t(0)?.abs() * scale > 1e-12) || !(seg_len > 1e-12) {
                return None;
            }
            let amp = inv(t(0), 0);
            let lin = if sine { inv(t(1), 1) } else { 0.0 };
            let c0 = inv(t(if sine { 2 } else { 1 }), 0);
            let w = if sine { 2.0 } else { 1.0 } * std::f64::consts::PI / seg_len;
            Some(Box::new(move |s| {
                let r = if sine { (w * s).sin() } else { (w * s).cos() };
                c0 + lin * s + amp * r
            }))
        }
        _ => None,
    }
}

/// Points of a plane curve of curvature `kappa(s)` from arc length `s0` to `s1`, starting at the
/// origin heading along x when s = 0; also the heading at `s0`. Composite Simpson steps keep each
/// step's turning under about 0.02 rad.
fn spiral_points(kappa: &dyn Fn(f64) -> f64, s0: f64, s1: f64) -> (Vec<V3>, f64) {
    let steps = |a: f64, b: f64| -> usize {
        // sample densely: a periodic curvature can vanish at the ends and the middle
        let k = (0..=64)
            .map(|i| kappa(a + (b - a) * i as f64 / 64.0).abs())
            .fold(0.0_f64, f64::max);
        (((b - a).abs() * k / 0.02).ceil() as usize).clamp(4, 4000)
    };
    let mut st = (0.0_f64, 0.0_f64, 0.0_f64); // heading, x, y
    let march = |st: &mut (f64, f64, f64), a: f64, b: f64, mut out: Option<&mut Vec<V3>>| {
        let n = steps(a, b);
        for i in 0..n {
            let u = a + (b - a) * i as f64 / n as f64;
            let v = a + (b - a) * (i + 1) as f64 / n as f64;
            let (h, m) = (v - u, 0.5 * (u + v));
            let (k0, km, k1) = (kappa(u), kappa(m), kappa(v));
            let thm = st.0 + h / 24.0 * (5.0 * k0 + 8.0 * km - k1);
            let thv = st.0 + h / 6.0 * (k0 + 4.0 * km + k1);
            st.1 += h / 6.0 * (st.0.cos() + 4.0 * thm.cos() + thv.cos());
            st.2 += h / 6.0 * (st.0.sin() + 4.0 * thm.sin() + thv.sin());
            st.0 = thv;
            if let Some(o) = out.as_mut() {
                o.push([st.1, st.2, 0.0]);
            }
        }
    };
    if s0.abs() > 1e-12 {
        march(&mut st, 0.0, s0, None);
    }
    let th0 = st.0;
    let mut pts = vec![[st.1, st.2, 0.0]];
    march(&mut st, s0, s1, Some(&mut pts));
    (pts, th0)
}

/// Arc-length span (s0, s1) of a curve segment from its start and signed length, each
/// `(value, is_parameter)`: a length in file units (times `unit`) or a parent parameter.
fn seg_span(
    start: (f64, bool),
    length: (f64, bool),
    unit: f64,
    to_s: impl Fn(f64) -> f64,
    to_u: impl Fn(f64) -> f64,
) -> (f64, f64) {
    let s0 = if start.1 { to_s(start.0) } else { start.0 * unit };
    let s1 = if length.1 {
        let u0 = if start.1 { start.0 } else { to_u(s0) };
        to_s(u0 + length.0)
    } else {
        s0 + length.0 * unit
    };
    (s0, s1)
}

#[derive(Clone, Copy)]
enum TrimAt {
    Point(V3),
    Param(f64),
}

fn ellipse(fr: &Frame, a: f64, b: f64, t0: f64, sweep: f64) -> Vec<Seg> {
    let k = arc_steps(sweep).max(8);
    let pt = |t: f64| fr.apply([a * t.cos(), b * t.sin(), 0.0]);
    (0..k)
        .map(|i| {
            Seg::Line(
                pt(t0 + sweep * i as f64 / k as f64),
                pt(t0 + sweep * (i + 1) as f64 / k as f64),
            )
        })
        .collect()
}

/// Unit left-hand normal of a planar direction (curve plane = XY).
fn left_of(t: V3) -> Option<V3> {
    norm([-t[1], t[0], 0.0])
}

/// Unit tangent of a segment at its start or end.
fn seg_tangent(s: &Seg, at_end: bool) -> Option<V3> {
    match *s {
        Seg::Line(a, b) => norm(sub(b, a)),
        Seg::Arc { a, b, c, n, sweep } => {
            let r = sub(if at_end { b } else { a }, c);
            norm(mul(cross(n, r), sweep.signum()))
        }
    }
}

/// One segment moved to the left by `d` (None for a zero-length line). Arcs stay arcs about the
/// same centre; an arc whose radius would collapse or flip is refused.
fn offset_seg(s: &Seg, d: f64) -> Result<Option<Seg>, Why> {
    match *s {
        Seg::Line(a, b) => {
            let Some(l) = norm(sub(b, a)).and_then(left_of) else {
                return Ok(None);
            };
            let o = mul(l, d);
            Ok(Some(Seg::Line(add(a, o), add(b, o))))
        }
        Seg::Arc { a, b, c, n, sweep } => {
            if n[2].abs() < 0.5 {
                return Err("offset arc out of plane");
            }
            let (Some(ta), Some(tb)) = (seg_tangent(s, false), seg_tangent(s, true)) else {
                return Err("bad offset curve");
            };
            let la = left_of(ta).ok_or("bad offset curve")?;
            let lb = left_of(tb).ok_or("bad offset curve")?;
            let a2 = add(a, mul(la, d));
            let b2 = if len(sub(a, b)) < 1e-9 {
                a2
            } else {
                add(b, mul(lb, d))
            };
            let (r0, r1) = (sub(a, c), sub(a2, c));
            if len(r1) < 1e-9 || dot(r0, r1) <= 0.0 {
                return Err("offset exceeds arc radius");
            }
            Ok(Some(Seg::Arc {
                a: a2,
                b: b2,
                c,
                n,
                sweep,
            }))
        }
    }
}

/// Mitre point of two consecutive offset lines (None when parallel, flipped or too far out).
fn mitre(a1: V3, b1: V3, a2: V3, b2: V3, limit: f64) -> Option<V3> {
    let (u1, u2) = (norm(sub(b1, a1))?, norm(sub(b2, a2))?);
    let den = u1[0] * u2[1] - u1[1] * u2[0];
    if den.abs() < 1e-9 {
        return None;
    }
    let w = sub(a2, b1);
    let s = (w[0] * u2[1] - w[1] * u2[0]) / den;
    let m = [b1[0] + u1[0] * s, b1[1] + u1[1] * s, b1[2]];
    (finite(m)
        && len(sub(m, b1)) <= limit
        && dot(sub(m, a1), u1) > 1e-12
        && dot(sub(b2, m), u2) > 1e-12)
        .then_some(m)
}

/// Offsets a planar curve to its left by `d` (negative = right): lines and arcs stay exact,
/// corners are mitred (bevelled past 4 x |d|). Large offsets of tight curves may self-intersect.
pub(super) fn offset_segs(segs: &[Seg], d: f64) -> Result<Vec<Seg>, Why> {
    if !d.is_finite() {
        return Err("bad offset curve");
    }
    let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
        return Err("bad offset curve");
    };
    let closed = len(sub(first.start(), last.end())) < 1e-9;
    let mut out = vec![];
    for s in segs {
        out.extend(offset_seg(s, d)?);
    }
    let n = out.len();
    if n == 0 {
        return Err("bad offset curve");
    }
    let mut bridge: Vec<Option<Seg>> = vec![None; n];
    for i in 0..if closed { n } else { n - 1 } {
        let j = (i + 1) % n;
        let (e, st) = (out[i].end(), out[j].start());
        if i == j || len(sub(e, st)) < 1e-9 {
            continue;
        }
        if let (Seg::Line(a1, b1), Seg::Line(a2, b2)) = (out[i], out[j]) {
            if let Some(m) = mitre(a1, b1, a2, b2, 4.0 * d.abs()) {
                out[i] = Seg::Line(a1, m);
                out[j] = Seg::Line(m, b2);
                continue;
            }
        }
        bridge[i] = Some(Seg::Line(e, st));
    }
    let mut res = vec![];
    for (s, b) in out.into_iter().zip(bridge) {
        res.push(s);
        res.extend(b);
    }
    Ok(res)
}

/// Offsets a curve by `d` along `rf` (made perpendicular to the curve) at every sampled point.
fn offset_3d(basis: &[Seg], d: f64, rf: V3) -> Result<Vec<Seg>, Why> {
    let p = polyline(basis);
    if p.len() < 2 || !d.is_finite() {
        return Err("bad offset curve");
    }
    let edge = |i: usize| norm(sub(p[i + 1], p[i]));
    let mut q = vec![];
    for i in 0..p.len() {
        let prev = i.checked_sub(1).and_then(edge);
        let next = if i + 1 < p.len() { edge(i) } else { None };
        let t = match (prev, next) {
            (Some(a), Some(b)) => norm(add(a, b)).unwrap_or(b),
            (Some(a), None) | (None, Some(a)) => a,
            _ => return Err("bad offset curve"),
        };
        let v = norm(sub(rf, mul(t, dot(rf, t)))).ok_or("bad offset curve")?;
        q.push(add(p[i], mul(v, d)));
    }
    Ok(q.windows(2).map(|w| Seg::Line(w[0], w[1])).collect())
}

/// Region of a centre-line profile: the curve offset by half the thickness to both sides, capped
/// at the ends (a closed curve gives a ring with a hole).
fn centerline_region(c: &[Seg], thickness: f64) -> Result<Poly, Why> {
    let h = thickness / 2.0;
    let (l, r) = (offset_segs(c, h)?, offset_segs(c, -h)?);
    let closed = match (c.first(), c.last()) {
        (Some(f), Some(e)) => len(sub(f.start(), e.end())) < 1e-9,
        _ => false,
    };
    let area = |v: &[V3]| newell(v)[2].abs();
    if closed {
        let (a, b) = (ring(&l), ring(&r));
        if a.len() < 3 || b.len() < 3 {
            return Err("bad profile curve");
        }
        let (o, mut i) = if area(&a) >= area(&b) { (a, b) } else { (b, a) };
        // holes run against the outer ring, like the hollow parametric profiles
        if newell(&o)[2] * newell(&i)[2] > 0.0 {
            i.reverse();
        }
        return Ok(vec![o, i]);
    }
    let mut v = polyline(&l);
    let mut back = polyline(&r);
    back.reverse();
    v.extend(back);
    v.dedup_by(|a, b| len(sub(*a, *b)) < 1e-9);
    while v.len() > 1 && len(sub(v[0], v[v.len() - 1])) < 1e-9 {
        v.pop();
    }
    if v.len() < 3 || area(&v) < 1e-18 {
        return Err("bad profile curve");
    }
    Ok(vec![v])
}

fn de_boor(p: usize, ctrl: &[V3], w: Option<&[f64]>, u: &[f64], t: f64) -> V3 {
    let n = ctrl.len() - 1;
    let mut k = p;
    while k < n && u[k + 1] <= t {
        k += 1;
    }
    let mut d: Vec<[f64; 4]> = (0..=p)
        .map(|j| {
            let c = ctrl[j + k - p];
            let wt = w.map_or(1.0, |w| w[j + k - p]);
            [c[0] * wt, c[1] * wt, c[2] * wt, wt]
        })
        .collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let (lo, hi) = (u[j + k - p], u[j + 1 + k - r]);
            let a = if hi > lo { (t - lo) / (hi - lo) } else { 0.0 };
            let prev = d[j - 1];
            for (x, y) in d[j].iter_mut().zip(prev) {
                *x = (1.0 - a) * y + a * *x;
            }
        }
    }
    let q = d[p];
    let wt = if q[3].abs() > 1e-300 { q[3] } else { 1.0 };
    [q[0] / wt, q[1] / wt, q[2] / wt]
}

/// Parametric profile as (outer, holes) regions in the profile frame (z = 0).
impl Rd<'_> {
    pub(super) fn profile_area(&self, id: u32, depth: usize) -> Result<Vec<Poly>, Why> {
        if depth > MAX_DEPTH {
            return Err("profile nesting too deep");
        }
        let e = self.ent(id, &[])?;
        let ty = self.f.type_of(id).unwrap_or("");
        let num = |i: usize| -> Option<f64> { Self::arg(e, i).as_f64().map(|v| v * self.scale) };
        let req = |i: usize| -> Result<f64, Why> {
            num(i)
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("bad profile")
        };
        let curve_ring = |c: u32| -> Result<Vec<V3>, Why> {
            let r = ring(&self.segs(c, depth + 1)?);
            if r.len() < 3 {
                return Err("bad profile curve");
            }
            Ok(r)
        };
        let pts = |v: &[(f64, f64)]| -> Vec<V3> { v.iter().map(|p| [p.0, p.1, 0.0]).collect() };
        let circle = |r: f64| -> Vec<V3> {
            let n = 48;
            (0..n)
                .map(|i| {
                    let a = TAU * i as f64 / n as f64;
                    [r * a.cos(), r * a.sin(), 0.0]
                })
                .collect()
        };
        let rect = |x: f64, y: f64| {
            pts(&[
                (-x / 2.0, -y / 2.0),
                (x / 2.0, -y / 2.0),
                (x / 2.0, y / 2.0),
                (-x / 2.0, y / 2.0),
            ])
        };
        let local: Poly = match ty {
            "IFCARBITRARYCLOSEDPROFILEDEF" => {
                return Ok(vec![vec![curve_ring(Self::reference(e, 2)?)?]])
            }
            "IFCARBITRARYPROFILEDEFWITHVOIDS" => {
                let mut v = vec![curve_ring(Self::reference(e, 2)?)?];
                for h in Self::refs(e, 3) {
                    v.push(curve_ring(h)?);
                }
                return Ok(vec![v]);
            }
            "IFCCOMPOSITEPROFILEDEF" => {
                let mut out = vec![];
                for p in Self::refs(e, 2) {
                    out.extend(self.profile_area(p, depth + 1)?);
                }
                return Ok(out);
            }
            "IFCDERIVEDPROFILEDEF" | "IFCMIRROREDPROFILEDEF" => {
                let parent = self.profile_area(Self::reference(e, 2)?, depth + 1)?;
                let op = self.operator(Self::reference(e, 3)?)?;
                let mirror = dot(cross(op.x, op.y), op.z) < 0.0;
                return Ok(parent
                    .into_iter()
                    .map(|p| {
                        p.into_iter()
                            .map(|r| {
                                let mut r: Vec<V3> = r.into_iter().map(|q| op.apply(q)).collect();
                                if mirror {
                                    r.reverse();
                                }
                                r
                            })
                            .collect()
                    })
                    .collect());
            }
            "IFCRECTANGLEPROFILEDEF" | "IFCROUNDEDRECTANGLEPROFILEDEF" => {
                vec![rect(req(3)?, req(4)?)]
            }
            "IFCRECTANGLEHOLLOWPROFILEDEF" => {
                let (x, y, t) = (req(3)?, req(4)?, req(5)?);
                let mut h = rect(x - 2.0 * t, y - 2.0 * t);
                h.reverse();
                if 2.0 * t >= x.min(y) {
                    return Err("bad profile");
                }
                vec![rect(x, y), h]
            }
            "IFCCIRCLEPROFILEDEF" => vec![circle(req(3)?)],
            "IFCCIRCLEHOLLOWPROFILEDEF" => {
                let (r, t) = (req(3)?, req(4)?);
                if t >= r {
                    return Err("bad profile");
                }
                let mut h = circle(r - t);
                h.reverse();
                vec![circle(r), h]
            }
            "IFCCENTERLINEPROFILEDEF" => {
                let c = self.segs(Self::reference(e, 2)?, depth + 1)?;
                return Ok(vec![centerline_region(&c, req(3)?)?]);
            }
            "IFCELLIPSEPROFILEDEF" => {
                let (a, b) = (req(3)?, req(4)?);
                vec![(0..48)
                    .map(|i| {
                        let t = TAU * i as f64 / 48.0;
                        [a * t.cos(), b * t.sin(), 0.0]
                    })
                    .collect()]
            }
            "IFCTRAPEZIUMPROFILEDEF" => {
                let (bx, tx, yd) = (req(3)?, req(4)?, req(5)?);
                let off = num(6).unwrap_or(0.0);
                vec![pts(&[
                    (-bx / 2.0, -yd / 2.0),
                    (bx / 2.0, -yd / 2.0),
                    (-bx / 2.0 + off + tx, yd / 2.0),
                    (-bx / 2.0 + off, yd / 2.0),
                ])]
            }
            _ => match self.section_points(id)? {
                Some(p) => vec![pts(&p)],
                None => return Err("unsupported profile"),
            },
        };
        let pos = self.axis_placement(Self::arg(e, 2))?;
        Ok(vec![local
            .into_iter()
            .map(|r| r.into_iter().map(|p| pos.apply(p)).collect())
            .collect()])
    }

    /// Outline of an I/L/T/U/C/Z/asymmetric-I profile around its bounding-box centre (no fillets).
    pub(super) fn section_points(&self, id: u32) -> Result<Option<Vec<(f64, f64)>>, Why> {
        let e = self.ent(id, &[])?;
        let num = |i: usize| -> Option<f64> { Self::arg(e, i).as_f64().map(|v| v * self.scale) };
        let req = |i: usize| -> Result<f64, Why> {
            num(i)
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("bad profile")
        };
        let p = match self.f.type_of(id).unwrap_or("") {
            "IFCISHAPEPROFILEDEF" => {
                let (b, h, tw, tf) = (req(3)?, req(4)?, req(5)?, req(6)?);
                i_shape(b, b, h, tw, tf, tf)
            }
            "IFCASYMMETRICISHAPEPROFILEDEF" => {
                let (bb, h, tw, tbf) = (req(3)?, req(4)?, req(5)?, req(6)?);
                let tb = num(8).filter(|v| *v > 0.0).unwrap_or(bb);
                let ttf = num(9).filter(|v| *v > 0.0).unwrap_or(tbf);
                i_shape(bb, tb, h, tw, tbf, ttf)
            }
            "IFCLSHAPEPROFILEDEF" => {
                let d = req(3)?;
                let w = num(4).filter(|v| *v > 0.0).unwrap_or(d);
                let t = req(5)?;
                let (x, y) = (w / 2.0, d / 2.0);
                vec![
                    (-x, -y),
                    (x, -y),
                    (x, -y + t),
                    (-x + t, -y + t),
                    (-x + t, y),
                    (-x, y),
                ]
            }
            "IFCTSHAPEPROFILEDEF" => {
                let (d, fw, tw, tf) = (req(3)?, req(4)?, req(5)?, req(6)?);
                let (x, y, w) = (fw / 2.0, d / 2.0, tw / 2.0);
                vec![
                    (-w, -y),
                    (w, -y),
                    (w, y - tf),
                    (x, y - tf),
                    (x, y),
                    (-x, y),
                    (-x, y - tf),
                    (-w, y - tf),
                ]
            }
            "IFCUSHAPEPROFILEDEF" => {
                let (d, fw, tw, tf) = (req(3)?, req(4)?, req(5)?, req(6)?);
                let (x, y) = (fw / 2.0, d / 2.0);
                vec![
                    (-x, -y),
                    (x, -y),
                    (x, -y + tf),
                    (-x + tw, -y + tf),
                    (-x + tw, y - tf),
                    (x, y - tf),
                    (x, y),
                    (-x, y),
                ]
            }
            "IFCCSHAPEPROFILEDEF" => {
                let (d, w, t) = (req(3)?, req(4)?, req(5)?);
                let g = num(6).filter(|v| *v > t).unwrap_or(t);
                let (x, y) = (w / 2.0, d / 2.0);
                vec![
                    (-x, -y),
                    (x, -y),
                    (x, -y + g),
                    (x - t, -y + g),
                    (x - t, -y + t),
                    (-x + t, -y + t),
                    (-x + t, y - t),
                    (x - t, y - t),
                    (x - t, y - g),
                    (x, y - g),
                    (x, y),
                    (-x, y),
                ]
            }
            "IFCZSHAPEPROFILEDEF" => {
                let (d, fw, tw, tf) = (req(3)?, req(4)?, req(5)?, req(6)?);
                let (y, w, f) = (d / 2.0, tw / 2.0, fw - tw / 2.0);
                vec![
                    (-f, -y),
                    (w, -y),
                    (w, y - tf),
                    (f, y - tf),
                    (f, y),
                    (-w, y),
                    (-w, -y + tf),
                    (-f, -y + tf),
                ]
            }
            _ => return Ok(None),
        };
        Ok(Some(p))
    }
}

fn i_shape(bb: f64, tb: f64, h: f64, tw: f64, tbf: f64, ttf: f64) -> Vec<(f64, f64)> {
    let (y, w, b, t) = (h / 2.0, tw / 2.0, bb / 2.0, tb / 2.0);
    vec![
        (-b, -y),
        (b, -y),
        (b, -y + tbf),
        (w, -y + tbf),
        (w, y - ttf),
        (t, y - ttf),
        (t, y),
        (-t, y),
        (-t, y - ttf),
        (-w, y - ttf),
        (-w, -y + tbf),
        (-b, -y + tbf),
    ]
}

/// Profile regions with the outer ring counter-clockwise in xy and holes clockwise.
fn oriented(area: Vec<Poly>) -> Vec<Poly> {
    let a2 = |r: &[V3]| newell(r)[2];
    area.into_iter()
        .filter(|p| p.first().is_some_and(|o| o.len() >= 3))
        .map(|p| {
            p.into_iter()
                .enumerate()
                .map(|(k, mut r)| {
                    if (a2(&r) < 0.0) == (k == 0) {
                        r.reverse();
                    }
                    r
                })
                .collect()
        })
        .collect()
}

fn extrude(area: &[Poly], v: V3, out: &mut Tri) {
    let up = v[2] >= 0.0;
    for p in area {
        let top: Poly = p
            .iter()
            .map(|r| r.iter().map(|q| add(*q, v)).collect())
            .collect();
        let bottom: Poly = p
            .iter()
            .map(|r| r.iter().rev().copied().collect())
            .collect();
        if up {
            out.poly(&top);
            out.poly(&bottom);
        } else {
            let t2: Poly = top
                .iter()
                .map(|r| r.iter().rev().copied().collect())
                .collect();
            out.poly(&t2);
            out.poly(p);
        }
        for r in p {
            for i in 0..r.len() {
                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                if up {
                    out.quad(a, b, add(b, v), add(a, v));
                } else {
                    out.quad(b, a, add(a, v), add(b, v));
                }
            }
        }
    }
}

/// Lofts rings of the same layout from `a` to `b`.
fn loft(a: &[Poly], b: &[Poly], out: &mut Tri) {
    for (p, q) in a.iter().zip(b) {
        out.poly(
            &p.iter()
                .map(|r| r.iter().rev().copied().collect())
                .collect::<Poly>(),
        );
        out.poly(q);
        for (r, s) in p.iter().zip(q) {
            for i in 0..r.len() {
                let j = (i + 1) % r.len();
                out.quad(r[i], r[j], s[j], s[i]);
            }
        }
    }
}

/// Normalised arc-length parameter of each ring vertex (closing edge included).
fn ring_params(r: &[V3]) -> Option<Vec<f64>> {
    let mut acc = 0.0;
    let mut v = Vec::with_capacity(r.len());
    for i in 0..r.len() {
        v.push(acc);
        acc += len(sub(r[(i + 1) % r.len()], r[i]));
    }
    (r.len() >= 3 && acc > 1e-12 && acc.is_finite())
        .then(|| v.into_iter().map(|x| x / acc).collect())
}

/// Point of a closed ring at normalised arc length `t` in [0, 1).
fn ring_at(r: &[V3], p: &[f64], t: f64) -> V3 {
    let i = p.partition_point(|&x| x <= t).saturating_sub(1);
    let j = (i + 1) % r.len();
    let (t0, t1) = (p[i], if j == 0 { 1.0 } else { p[j] });
    let f = if t1 - t0 > 1e-15 { (t - t0) / (t1 - t0) } else { 0.0 };
    add(r[i], mul(sub(r[j], r[i]), f.clamp(0.0, 1.0)))
}

/// Rings with one point count: unchanged when the counts agree, else each started at the vertex
/// nearest the first ring's start and resampled at the union of all arc-length parameters (every
/// corner of every ring survives).
fn match_rings(rs: &[&[V3]]) -> Option<Vec<Vec<V3>>> {
    let n = rs.first()?.len();
    if rs.iter().all(|r| r.len() == n) {
        return Some(rs.iter().map(|r| r.to_vec()).collect());
    }
    let s0 = *rs[0].first()?;
    let rot: Vec<Vec<V3>> = rs
        .iter()
        .map(|r| {
            let k = (0..r.len())
                .min_by(|&a, &b| len(sub(r[a], s0)).total_cmp(&len(sub(r[b], s0))))
                .unwrap_or(0);
            r[k..].iter().chain(&r[..k]).copied().collect()
        })
        .collect();
    let params = rot
        .iter()
        .map(|r| ring_params(r))
        .collect::<Option<Vec<_>>>()?;
    let mut u: Vec<f64> = params.iter().flatten().copied().collect();
    u.sort_by(f64::total_cmp);
    u.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    Some(
        rot.iter()
            .zip(&params)
            .map(|(r, p)| u.iter().map(|&t| ring_at(r, p, t)).collect())
            .collect(),
    )
}

/// Closed loft through consecutive cross sections (profile regions in local xy, each placed by
/// its frame); sections of a different region/hole layout fall back to their largest outer ring.
fn loft_sections(
    mut secs: Vec<(Vec<Poly>, Frame)>,
    out: &mut Tri,
    approx: &mut Vec<Why>,
) -> Result<(), Why> {
    secs.retain(|s| !s.0.is_empty());
    if secs.len() < 2 {
        return Err("too few cross sections");
    }
    let shape = |a: &[Poly]| a.iter().map(|p| p.len()).collect::<Vec<_>>();
    let s0 = shape(&secs[0].0);
    if secs.iter().any(|s| shape(&s.0) != s0) {
        approx.push("cross sections of different layout");
        for s in &mut secs {
            let outer = s
                .0
                .iter()
                .map(|p| p[0].clone())
                .max_by(|a, b| len(newell(a)).total_cmp(&len(newell(b))))
                .unwrap_or_default();
            s.0 = vec![vec![outer]];
        }
    }
    let base = out.t.len();
    for g in 0..secs[0].0.len() {
        let mut placed: Vec<Poly> = vec![vec![]; secs.len()];
        for k in 0..secs[0].0[g].len() {
            let rs: Vec<&[V3]> = secs.iter().map(|s| s.0[g][k].as_slice()).collect();
            let rs = match_rings(&rs).ok_or("degenerate cross section")?;
            for (i, r) in rs.into_iter().enumerate() {
                let fr = secs[i].1;
                placed[i].push(r.into_iter().map(|q| fr.apply(q)).collect());
            }
        }
        for w in placed.windows(2) {
            for (r, s) in w[0].iter().zip(&w[1]) {
                for a in 0..r.len() {
                    let b = (a + 1) % r.len();
                    out.quad(r[a], r[b], s[b], s[a]);
                }
            }
            if out.t.len() > MAX_TRIS {
                return Err("geometry too large");
            }
        }
        out.poly(
            &placed[0]
                .iter()
                .map(|r| r.iter().rev().copied().collect())
                .collect::<Poly>(),
        );
        out.poly(&placed[placed.len() - 1]);
    }
    // sections whose z runs against the order of the list come out inside out
    if let Some(t0) = out.t.get(base) {
        let o = out.p[t0[0] as usize];
        let vol: f64 = out.t[base..]
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| sub(out.p[i as usize], o));
                dot(a, cross(b, c))
            })
            .sum();
        if vol < 0.0 {
            out.t[base..].iter_mut().for_each(|t| t.swap(1, 2));
        }
    }
    Ok(())
}

/// Section frame at arc length `s` along `path`: z the horizontal tangent, y up, x to the left,
/// shifted by (lateral, vertical, longitudinal) offsets.
fn frame_along(path: &[V3], s: f64, off: V3) -> Result<Frame, Why> {
    if path.len() < 2 || !s.is_finite() {
        return Err("bad directrix");
    }
    let mut i = 0;
    let mut acc = 0.0;
    while i + 2 < path.len() && acc + len(sub(path[i + 1], path[i])) < s {
        acc += len(sub(path[i + 1], path[i]));
        i += 1;
    }
    let d = norm(sub(path[i + 1], path[i])).ok_or("bad directrix")?;
    let p = add(path[i], mul(d, s - acc));
    let up = [0.0, 0.0, 1.0];
    let (x, y, z) = match norm([d[0], d[1], 0.0]) {
        Some(t) => (cross(up, t), up, t),
        None => {
            let (x, y) = basis(d);
            (x, y, d)
        }
    };
    let o = add(p, add(mul(x, off[0]), add(mul(y, off[1]), mul(z, off[2]))));
    Ok(Frame { o, x, y, z })
}

/// Sweeps profile regions (local xy, tangent = local z) along a path.
fn sweep(area: &[Poly], path: &[V3], fixed: Option<V3>, out: &mut Tri) -> Result<(), Why> {
    let mut path = path.to_vec();
    path.dedup_by(|a, b| len(sub(*a, *b)) < 1e-9);
    if path.len() < 2 {
        return Err("degenerate sweep path");
    }
    let n = path.len();
    let closed = len(sub(path[0], path[n - 1])) < 1e-9;
    let tan: Vec<V3> = (0..n)
        .map(|i| {
            let d0 = if i > 0 {
                norm(sub(path[i], path[i - 1]))
            } else {
                None
            };
            let d1 = if i + 1 < n {
                norm(sub(path[i + 1], path[i]))
            } else {
                None
            };
            match (d0, d1) {
                (Some(a), Some(b)) => norm(add(a, b)).unwrap_or(b),
                (Some(a), None) | (None, Some(a)) => a,
                _ => [0.0, 0.0, 1.0],
            }
        })
        .collect();
    let mut frames = Vec::with_capacity(n);
    let mut x = {
        let t = tan[0];
        let r = fixed.unwrap_or(if t[2].abs() < 0.9 {
            [0.0, 0.0, 1.0]
        } else {
            [1.0, 0.0, 0.0]
        });
        let y = norm(sub(r, mul(t, dot(r, t)))).unwrap_or(basis(t).1);
        if fixed.is_some() {
            y
        } else {
            cross(y, t)
        }
    };
    for t in &tan {
        let r = fixed.unwrap_or(x);
        x = norm(sub(r, mul(*t, dot(r, *t)))).unwrap_or(x);
        frames.push((x, cross(*t, x)));
    }
    let place = |i: usize, q: V3| add(path[i], add(mul(frames[i].0, q[0]), mul(frames[i].1, q[1])));
    for p in area {
        for r in p {
            for i in 0..n - 1 {
                for k in 0..r.len() {
                    let (a, b) = (r[k], r[(k + 1) % r.len()]);
                    out.quad(place(i, a), place(i, b), place(i + 1, b), place(i + 1, a));
                }
            }
        }
        if !closed {
            out.poly(
                &p.iter()
                    .map(|r| r.iter().rev().map(|q| place(0, *q)).collect())
                    .collect::<Poly>(),
            );
            out.poly(
                &p.iter()
                    .map(|r| r.iter().map(|q| place(n - 1, *q)).collect())
                    .collect::<Poly>(),
            );
        }
        if out.t.len() > MAX_TRIS {
            return Err("geometry too large");
        }
    }
    Ok(())
}

impl Rd<'_> {
    /// Mesh of one representation item in its own coordinates; `approx` collects what was
    /// simplified.
    pub(super) fn item_mesh(
        &mut self,
        id: u32,
        depth: usize,
        approx: &mut Vec<Why>,
    ) -> Result<Tri, Why> {
        if depth > MAX_DEPTH {
            return Err("geometry nesting too deep");
        }
        let ty = self.f.type_of(id).unwrap_or("").to_string();
        if matches!(
            ty.as_str(),
            "IFCTRIANGULATEDFACESET" | "IFCTRIANGULATEDIRREGULARNETWORK" | "IFCPOLYGONALFACESET"
        ) {
            // the biggest leaves of IFC4 files: read once, never pinned
            let out = self.face_set(id, &ty)?;
            out.check()?;
            return Ok(out);
        }
        let e = self.ent(id, &[])?;
        let mut out = Tri::default();
        match ty.as_str() {
            "IFCEXTRUDEDAREASOLID" | "IFCEXTRUDEDAREASOLIDTAPERED" => {
                let area = oriented(self.profile_area(Self::reference(e, 0)?, 0)?);
                let pos = self.axis_placement(Self::arg(e, 1))?;
                let d = self.direction(Self::arg(e, 2))?.ok_or("bad extrusion")?;
                let depth = Self::arg(e, 3).as_f64().ok_or("bad extrusion")? * self.scale;
                let v = mul(d, depth);
                let end = if ty.ends_with("TAPERED") {
                    Self::arg(e, 4)
                        .as_ref()
                        .and_then(|p| self.profile_area(p, 0).ok())
                        .map(oriented)
                        .filter(|b| {
                            b.len() == area.len()
                                && b.iter().zip(&area).all(|(p, q)| {
                                    p.len() == q.len()
                                        && p.iter().zip(q).all(|(r, s)| r.len() == s.len())
                                })
                        })
                } else {
                    None
                };
                match end {
                    Some(b) => {
                        let b: Vec<Poly> = b
                            .into_iter()
                            .map(|p| {
                                p.into_iter()
                                    .map(|r| r.into_iter().map(|q| add(q, v)).collect())
                                    .collect()
                            })
                            .collect();
                        if v[2] >= 0.0 {
                            loft(&area, &b, &mut out)
                        } else {
                            loft(&b, &area, &mut out)
                        }
                    }
                    None => {
                        if ty.ends_with("TAPERED") {
                            approx.push("tapered extrusion");
                        }
                        extrude(&area, v, &mut out)
                    }
                }
                out.transform(&pos);
            }
            "IFCREVOLVEDAREASOLID" | "IFCREVOLVEDAREASOLIDTAPERED" => {
                let area = oriented(self.profile_area(Self::reference(e, 0)?, 0)?);
                let pos = self.axis_placement(Self::arg(e, 1))?;
                let ax = self.ent(Self::reference(e, 2)?, &["IFCAXIS1PLACEMENT"])?;
                let o = self.point(Self::reference(ax, 0)?)?;
                let k = self.direction(Self::arg(ax, 1))?.unwrap_or([0.0, 0.0, 1.0]);
                let ang = Self::arg(e, 3).as_f64().ok_or("bad revolution")? * self.angle;
                if !ang.is_finite() || ang.abs() < 1e-9 {
                    return Err("bad revolution");
                }
                let ang = ang.clamp(-TAU, TAU);
                let steps = arc_steps(ang).max(4);
                let at = |q: V3, t: f64| add(o, rot(sub(q, o), k, t));
                for p in &area {
                    for r in p {
                        for s in 0..steps {
                            let (t0, t1) = (
                                ang * s as f64 / steps as f64,
                                ang * (s + 1) as f64 / steps as f64,
                            );
                            for i in 0..r.len() {
                                let (a, b) = (r[i], r[(i + 1) % r.len()]);
                                out.quad(at(a, t0), at(b, t0), at(b, t1), at(a, t1));
                            }
                        }
                    }
                    if ang.abs() < TAU - 1e-9 {
                        out.poly(
                            &p.iter()
                                .map(|r| r.iter().rev().copied().collect())
                                .collect::<Poly>(),
                        );
                        out.poly(
                            &p.iter()
                                .map(|r| r.iter().map(|q| at(*q, ang)).collect())
                                .collect::<Poly>(),
                        );
                    }
                }
                out.transform(&pos);
            }
            "IFCSWEPTDISKSOLID" | "IFCSWEPTDISKSOLIDPOLYGONAL" => {
                let path = polyline(&self.segs(Self::reference(e, 0)?, 0)?);
                let r = Self::arg(e, 1).as_f64().ok_or("bad swept disk")? * self.scale;
                let ri = Self::arg(e, 2)
                    .as_f64()
                    .map(|v| v * self.scale)
                    .filter(|v| *v > 0.0 && *v < r);
                if !(r > 0.0 && r.is_finite()) {
                    return Err("bad swept disk");
                }
                let c = |r: f64, rev: bool| -> Vec<V3> {
                    let n = 16;
                    let mut v: Vec<V3> = (0..n)
                        .map(|i| {
                            let a = TAU * i as f64 / n as f64;
                            [r * a.cos(), r * a.sin(), 0.0]
                        })
                        .collect();
                    if rev {
                        v.reverse();
                    }
                    v
                };
                let mut p = vec![c(r, false)];
                if let Some(ri) = ri {
                    p.push(c(ri, true));
                }
                sweep(&[p], &path, None, &mut out)?;
            }
            "IFCSURFACECURVESWEPTAREASOLID"
            | "IFCFIXEDREFERENCESWEPTAREASOLID"
            | "IFCDIRECTRIXCURVESWEPTAREASOLID" => {
                let area = oriented(self.profile_area(Self::reference(e, 0)?, 0)?);
                let pos = self.axis_placement(Self::arg(e, 1))?;
                let path = polyline(&self.segs(Self::reference(e, 2)?, 0)?);
                let fixed = if ty == "IFCFIXEDREFERENCESWEPTAREASOLID" {
                    self.direction(Self::arg(e, 5))?
                } else {
                    None
                };
                sweep(&area, &path, fixed, &mut out)?;
                out.transform(&pos);
            }
            "IFCSECTIONEDSPINE" => {
                let (profs, pls) = (Self::refs(e, 1), Self::refs(e, 2));
                if profs.len() < 2 || profs.len() != pls.len() {
                    return Err("bad sectioned spine");
                }
                let spine = Self::arg(e, 0).as_ref().and_then(|c| self.segs(c, 0).ok());
                if spine.is_some_and(|s| s.iter().any(|g| matches!(g, Seg::Arc { .. }))) {
                    approx.push("sectioned spine arcs lofted straight");
                }
                let mut secs = Vec::with_capacity(profs.len());
                for (p, pl) in profs.iter().zip(&pls) {
                    let area = oriented(self.profile_area(*p, 0)?);
                    secs.push((area, self.axis_placement(&Value::Ref(*pl))?));
                }
                loft_sections(secs, &mut out, approx)?;
            }
            "IFCSECTIONEDSOLIDHORIZONTAL" => {
                let dir = Self::reference(e, 0)?;
                if self.f.type_of(dir) == Some("IFCSEGMENTEDREFERENCECURVE") {
                    approx.push("cant ignored");
                }
                let path = polyline(&self.segs(dir, 0)?);
                let (profs, pos) = (Self::refs(e, 1), Self::refs(e, 2));
                if profs.len() < 2 || profs.len() != pos.len() {
                    return Err("bad sectioned solid");
                }
                let mut secs = Vec::with_capacity(profs.len());
                for (p, mut q) in profs.iter().copied().zip(pos) {
                    if self.f.type_of(q) == Some("IFCAXIS2PLACEMENTLINEAR") {
                        q = Self::reference(self.ent(q, &[])?, 0)?;
                    }
                    let d = self.ent(
                        q,
                        &["IFCPOINTBYDISTANCEEXPRESSION", "IFCDISTANCEEXPRESSION"],
                    )?;
                    let s = Self::arg(d, 0)
                        .as_f64()
                        .ok_or("bad section position")?
                        * self.scale;
                    let off = |i: usize| Self::arg(d, i).as_f64().unwrap_or(0.0) * self.scale;
                    let fr = frame_along(&path, s, [off(1), off(2), off(3)])?;
                    secs.push((oriented(self.profile_area(p, 0)?), fr));
                }
                loft_sections(secs, &mut out, approx)?;
            }
            "IFCFACETEDBREP"
            | "IFCFACETEDBREPWITHVOIDS"
            | "IFCADVANCEDBREP"
            | "IFCADVANCEDBREPWITHVOIDS" => {
                let mut shells = vec![Self::reference(e, 0)?];
                shells.extend(Self::refs(e, 1));
                for s in shells {
                    let se = self.ent(s, &[])?;
                    for f in Self::refs(se, 0) {
                        self.face(f, &mut out, approx)?;
                    }
                }
            }
            "IFCCLOSEDSHELL" | "IFCOPENSHELL" | "IFCCONNECTEDFACESET" => {
                for f in Self::refs(e, 0) {
                    self.face(f, &mut out, approx)?;
                }
            }
            "IFCSHELLBASEDSURFACEMODEL" | "IFCFACEBASEDSURFACEMODEL" => {
                for s in Self::refs(e, 0) {
                    let se = self.ent(s, &[])?;
                    for f in Self::refs(se, 0) {
                        self.face(f, &mut out, approx)?;
                    }
                }
            }
            "IFCBOOLEANRESULT" | "IFCBOOLEANCLIPPINGRESULT" => {
                let op = Self::arg(e, 0)
                    .as_enum()
                    .unwrap_or("DIFFERENCE")
                    .to_string();
                let (a, b) = (Self::reference(e, 1)?, Self::reference(e, 2)?);
                out = self.item_mesh(a, depth + 1, approx)?;
                match op.as_str() {
                    "UNION" => {
                        let m = self.item_mesh(b, depth + 1, approx)?;
                        out.append(&m);
                    }
                    "DIFFERENCE" => {
                        if !self.half_space_cut(b, &mut out)? {
                            approx.push("boolean difference not cut");
                        }
                    }
                    _ => approx.push("boolean intersection not cut"),
                }
            }
            "IFCCSGSOLID" => out = self.item_mesh(Self::reference(e, 0)?, depth + 1, approx)?,
            "IFCBLOCK" => {
                let pos = self.axis_placement(Self::arg(e, 0))?;
                let x = Self::arg(e, 1).as_f64().ok_or("bad block")? * self.scale;
                let y = Self::arg(e, 2).as_f64().ok_or("bad block")? * self.scale;
                let z = Self::arg(e, 3).as_f64().ok_or("bad block")? * self.scale;
                let r = vec![[0.0, 0.0, 0.0], [x, 0.0, 0.0], [x, y, 0.0], [0.0, y, 0.0]];
                extrude(&oriented(vec![vec![r]]), [0.0, 0.0, z], &mut out);
                out.transform(&pos);
            }
            "IFCBOUNDINGBOX" => {
                let o = self.point(Self::reference(e, 0)?)?;
                let x = Self::arg(e, 1).as_f64().ok_or("bad box")? * self.scale;
                let y = Self::arg(e, 2).as_f64().ok_or("bad box")? * self.scale;
                let z = Self::arg(e, 3).as_f64().ok_or("bad box")? * self.scale;
                let r = vec![
                    o,
                    add(o, [x, 0.0, 0.0]),
                    add(o, [x, y, 0.0]),
                    add(o, [0.0, y, 0.0]),
                ];
                extrude(&oriented(vec![vec![r]]), [0.0, 0.0, z], &mut out);
            }
            "IFCRECTANGULARPYRAMID" => {
                let pos = self.axis_placement(Self::arg(e, 0))?;
                let x = Self::arg(e, 1).as_f64().ok_or("bad pyramid")? * self.scale;
                let y = Self::arg(e, 2).as_f64().ok_or("bad pyramid")? * self.scale;
                let h = Self::arg(e, 3).as_f64().ok_or("bad pyramid")? * self.scale;
                let b = [[0.0, 0.0, 0.0], [x, 0.0, 0.0], [x, y, 0.0], [0.0, y, 0.0]];
                let apex = [x / 2.0, y / 2.0, h];
                out.poly(&[b.iter().rev().copied().collect()]);
                for i in 0..4 {
                    out.tri(b[i], b[(i + 1) % 4], apex);
                }
                out.transform(&pos);
            }
            "IFCRIGHTCIRCULARCYLINDER" | "IFCRIGHTCIRCULARCONE" => {
                let pos = self.axis_placement(Self::arg(e, 0))?;
                let h = Self::arg(e, 1).as_f64().ok_or("bad primitive")? * self.scale;
                let r = Self::arg(e, 2).as_f64().ok_or("bad primitive")? * self.scale;
                let n = 32;
                let c: Vec<V3> = (0..n)
                    .map(|i| {
                        let a = TAU * i as f64 / n as f64;
                        [r * a.cos(), r * a.sin(), 0.0]
                    })
                    .collect();
                if ty == "IFCRIGHTCIRCULARCYLINDER" {
                    extrude(&[vec![c]], [0.0, 0.0, h], &mut out);
                } else {
                    out.poly(&[c.iter().rev().copied().collect()]);
                    for i in 0..n {
                        out.tri(c[i], c[(i + 1) % n], [0.0, 0.0, h]);
                    }
                }
                out.transform(&pos);
            }
            "IFCSPHERE" => {
                let pos = self.axis_placement(Self::arg(e, 0))?;
                let r = Self::arg(e, 1).as_f64().ok_or("bad sphere")? * self.scale;
                let (nu, nv) = (32, 16);
                let pt = |i: usize, j: usize| {
                    let (u, v) = (
                        TAU * i as f64 / nu as f64,
                        PI * j as f64 / nv as f64 - PI / 2.0,
                    );
                    [r * v.cos() * u.cos(), r * v.cos() * u.sin(), r * v.sin()]
                };
                for j in 0..nv {
                    for i in 0..nu {
                        out.quad(pt(i, j), pt(i + 1, j), pt(i + 1, j + 1), pt(i, j + 1));
                    }
                }
                out.transform(&pos);
            }
            "IFCMAPPEDITEM" => {
                let src = Self::reference(e, 0)?;
                let target = self.operator(Self::reference(e, 1)?)?;
                let m = match self.maps.get(&src) {
                    Some(m) => m.clone(),
                    None => {
                        let me = self.ent(src, &["IFCREPRESENTATIONMAP"])?;
                        let origin = self.axis_placement(Self::arg(me, 0))?;
                        let rep = self.ent(Self::reference(me, 1)?, &[])?;
                        let mut m = Tri::default();
                        for it in Self::refs(rep, 3) {
                            match self.item_mesh(it, depth + 1, approx) {
                                Ok(x) => m.append(&x),
                                Err(w) if is_curve_only(w) => {}
                                Err(w) => return Err(w),
                            }
                        }
                        m.transform(&origin);
                        self.maps.insert(src, m.clone());
                        m
                    }
                };
                out = m;
                out.transform(&target);
            }
            "IFCGEOMETRICSET"
            | "IFCGEOMETRICCURVESET"
            | "IFCPOLYLINE"
            | "IFCTRIMMEDCURVE"
            | "IFCCOMPOSITECURVE"
            | "IFCINDEXEDPOLYCURVE"
            | "IFCCARTESIANPOINT"
            | "IFCANNOTATIONFILLAREA"
            | "IFCTEXTLITERAL"
            | "IFCTEXTLITERALWITHEXTENT"
            | "IFCCIRCLE"
            | "IFCLINE" => return Err("curves only"),
            "IFCHALFSPACESOLID" | "IFCPOLYGONALBOUNDEDHALFSPACE" | "IFCBOXEDHALFSPACE" => {
                return Err("unbounded half space")
            }
            _ => return Err("unsupported geometry"),
        }
        out.check()?;
        Ok(out)
    }

    /// Cuts `m` by a half-space operand; false when the operand is not a half space or a
    /// bounded removal could not be subtracted (the body then stays whole).
    pub(super) fn half_space_cut(&mut self, b: u32, m: &mut Tri) -> Result<bool, Why> {
        let Some((o, n, bound)) = self.half_space(b)? else {
            return Ok(false);
        };
        if let Some((fr, ring)) = bound {
            let inv = |p: V3| {
                let r = sub(p, fr.o);
                (dot(r, fr.x), dot(r, fr.y))
            };
            let all_in =
                m.p.iter()
                    .filter(|p| dot(sub(**p, o), n) > 1e-9)
                    .all(|p| inside2(&ring, inv(*p)));
            // the plane alone is exact only when all it removes lies inside the boundary prism
            if !all_in {
                return Ok(bounded_cut(m, o, n, &fr, &ring));
            }
        }
        m.clip(o, n);
        Ok(true)
    }

    /// (plane point, outward normal of the removed side, optional boundary frame and ring).
    #[allow(clippy::type_complexity)]
    pub(super) fn half_space(
        &mut self,
        b: u32,
    ) -> Result<Option<(V3, V3, Option<(Frame, Vec<(f64, f64)>)>)>, Why> {
        let ty = self.f.type_of(b).unwrap_or("");
        if !matches!(
            ty,
            "IFCHALFSPACESOLID" | "IFCPOLYGONALBOUNDEDHALFSPACE" | "IFCBOXEDHALFSPACE"
        ) {
            return Ok(None);
        }
        let e = self.ent(b, &[])?;
        let pl = self.ent(Self::reference(e, 0)?, &["IFCPLANE"])?;
        let fr = self.axis_placement(Self::arg(pl, 0))?;
        // AgreementFlag true: the plane normal points away from the half-space material
        let agree = Self::arg(e, 1).as_enum() != Some("F");
        let n = if agree { mul(fr.z, -1.0) } else { fr.z };
        let bound = if ty == "IFCPOLYGONALBOUNDEDHALFSPACE" {
            let bf = self.axis_placement(Self::arg(e, 2))?;
            let ring: Vec<(f64, f64)> = ring(&self.segs(Self::reference(e, 3)?, 0)?)
                .into_iter()
                .map(|p| (p[0], p[1]))
                .collect();
            Some((bf, ring))
        } else {
            None
        };
        Ok(Some((fr.o, n, bound)))
    }

    /// Indexed face sets (triangulated or polygonal) from their point list.
    fn face_set(&self, id: u32, ty: &str) -> Result<Tri, Why> {
        let e = self.leaf(id, &[])?;
        let pts = self.point_list(
            Self::at(&e, 0).as_ref().ok_or("missing reference")?,
            &["IFCCARTESIANPOINTLIST3D"],
        )?;
        let mut out = Tri::default();
        // PnIndex: file indices go through it when present
        let pn = int_list(Self::at(
            &e,
            if ty == "IFCPOLYGONALFACESET" { 3 } else { 4 },
        ));
        let index = |k: i64| -> Result<usize, Why> {
            let k = if pn.is_empty() {
                k
            } else {
                *pn.get((k - 1).max(0) as usize).ok_or("bad index")?
            };
            if k < 1 || k as usize > pts.len() {
                return Err("bad index");
            }
            Ok(k as usize - 1)
        };
        if ty != "IFCPOLYGONALFACESET" {
            let mut t = vec![];
            for v in Self::at(&e, 3).as_list().ok_or("bad face set")? {
                let v = int_list(v);
                if v.len() != 3 {
                    return Err("bad face set");
                }
                t.push([
                    index(v[0])? as u32,
                    index(v[1])? as u32,
                    index(v[2])? as u32,
                ]);
            }
            out.p = pts;
            out.t = t;
            return Ok(out);
        }
        let at = |k: i64| index(k).map(|i| pts[i]);
        for f in Self::refs_at(&e, 2) {
            let fe = self.leaf(f, &[])?;
            let mut rings = vec![int_list(Self::at(&fe, 0))
                .into_iter()
                .map(at)
                .collect::<Result<Vec<_>, _>>()?];
            if let Some(inner) = Self::at(&fe, 1).as_list() {
                for h in inner {
                    rings.push(
                        int_list(h)
                            .into_iter()
                            .map(at)
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
            }
            out.poly(&rings);
        }
        Ok(out)
    }

    fn face(&mut self, f: u32, out: &mut Tri, approx: &mut Vec<Why>) -> Result<(), Why> {
        let fe = self.leaf(f, &[])?;
        let ty = self.f.type_of(f).unwrap_or("");
        let mut rings: Vec<(Vec<V3>, bool)> = vec![];
        for b in Self::refs_at(&fe, 0) {
            let be = self.leaf(b, &[])?;
            let outer = self.f.type_of(b) == Some("IFCFACEOUTERBOUND");
            let orient = Self::at(&be, 1).as_enum() != Some("F");
            let lp = Self::at(&be, 0).as_ref().ok_or("missing reference")?;
            drop(be);
            let mut r = self.face_loop(lp)?;
            if !orient {
                r.reverse();
            }
            if r.len() >= 3 {
                rings.push((r, outer));
            }
        }
        if rings.is_empty() {
            return Ok(());
        }
        // outer bound first (largest when not marked)
        let oi = rings.iter().position(|r| r.1).unwrap_or_else(|| {
            (0..rings.len())
                .max_by(|a, b| len(newell(&rings[*a].0)).total_cmp(&len(newell(&rings[*b].0))))
                .unwrap_or(0)
        });
        let o = rings.remove(oi);
        let mut rs = vec![o.0];
        rs.extend(rings.into_iter().map(|r| r.0));
        let surf = if ty == "IFCFACE" {
            None
        } else {
            Self::at(&fe, 1).as_ref()
        };
        match surf.and_then(|s| self.f.type_of(s)) {
            None | Some("IFCPLANE") => out.poly(&rs),
            Some("IFCCYLINDRICALSURFACE") => {
                let se = self.ent(surf.unwrap_or(0), &[])?;
                let fr = self.axis_placement(Self::arg(se, 0))?;
                let r = Self::arg(se, 1).as_f64().ok_or("bad surface")? * self.scale;
                cylinder_face(&fr, r, &rs, out);
            }
            Some(_) => {
                let sense = Self::at(&fe, 2).as_enum() != Some("F");
                match self
                    .surface(surf.unwrap_or(0))
                    .and_then(|s| super::surf::surface_face(&s, &rs, sense))
                {
                    Ok(m) => out.append(&m),
                    Err(why) => {
                        approx.push("curved face flattened");
                        approx.push(why);
                        out.poly(&rs);
                    }
                }
            }
        }
        if out.t.len() > MAX_TRIS {
            return Err("geometry too large");
        }
        Ok(())
    }

    fn face_loop(&self, l: u32) -> Result<Vec<V3>, Why> {
        if self.f.type_of(l) == Some("IFCPOLYLOOP") {
            let pts = Self::refs_at(&self.leaf(l, &[])?, 0);
            return pts.into_iter().map(|p| self.point(p)).collect();
        }
        let le = self.ent(l, &[])?;
        match self.f.type_of(l).unwrap_or("") {
            "IFCPOLYLOOP" => Self::refs(le, 0)
                .into_iter()
                .map(|p| self.point(p))
                .collect(),
            "IFCEDGELOOP" => {
                let mut out: Vec<V3> = vec![];
                for oe in Self::refs(le, 0) {
                    let oee = self.ent(oe, &["IFCORIENTEDEDGE"])?;
                    let ec = Self::reference(oee, 2)?;
                    let mut pts = self.edge_points(ec)?;
                    if Self::arg(oee, 3).as_enum() == Some("F") {
                        pts.reverse();
                    }
                    for p in pts {
                        if out.last().is_none_or(|q| len(sub(*q, p)) > 1e-9) {
                            out.push(p);
                        }
                    }
                }
                while out.len() > 1 && len(sub(out[0], out[out.len() - 1])) < 1e-9 {
                    out.pop();
                }
                Ok(out)
            }
            "IFCVERTEXLOOP" => Ok(vec![]),
            _ => Err("unsupported face loop"),
        }
    }

    /// Points of an edge from its start vertex to its end vertex.
    fn edge_points(&self, ec: u32) -> Result<Vec<V3>, Why> {
        let e = self.ent(ec, &[])?;
        let vp = |i: usize| -> Result<V3, Why> {
            let v = self.ent(Self::reference(e, i)?, &["IFCVERTEXPOINT"])?;
            self.point(Self::reference(v, 0)?)
        };
        let (a, b) = (vp(0)?, vp(1)?);
        if self.f.type_of(ec) != Some("IFCEDGECURVE") {
            return Ok(vec![a, b]);
        }
        let same = Self::arg(e, 3).as_enum() != Some("F");
        let c = Self::reference(e, 2)?;
        let (s, t) = if same { (a, b) } else { (b, a) };
        let mut pts = match self.f.type_of(c).unwrap_or("") {
            "IFCLINE" => vec![s, t],
            "IFCCIRCLE" | "IFCELLIPSE" => {
                let ce = self.ent(c, &[])?;
                let fr = self.axis_placement(Self::arg(ce, 0))?;
                let ra = Self::arg(ce, 1).as_f64().ok_or("bad circle")? * self.scale;
                let rb = if self.f.type_of(c) == Some("IFCCIRCLE") {
                    ra
                } else {
                    Self::arg(ce, 2).as_f64().ok_or("bad ellipse")? * self.scale
                };
                let ang = |p: V3| {
                    let r = sub(p, fr.o);
                    (dot(r, fr.y) / rb).atan2(dot(r, fr.x) / ra)
                };
                let (a0, a1) = (ang(s), ang(t));
                let mut sw = (a1 - a0).rem_euclid(TAU);
                if sw < 1e-9 {
                    sw = TAU;
                }
                let k = arc_steps(sw);
                let mut v = vec![s];
                for i in 1..k {
                    let u = a0 + sw * i as f64 / k as f64;
                    v.push(fr.apply([ra * u.cos(), rb * u.sin(), 0.0]));
                }
                v.push(t);
                v
            }
            _ => match self.segs(c, 0) {
                Ok(segs) => {
                    let p = polyline(&segs);
                    let near = |q: V3| {
                        (0..p.len())
                            .min_by(|&x, &y| len(sub(p[x], q)).total_cmp(&len(sub(p[y], q))))
                            .unwrap_or(0)
                    };
                    let (i, j) = (near(s), near(t));
                    let mut v = vec![s];
                    if i < j {
                        v.extend_from_slice(&p[i + 1..j]);
                    } else if i > j {
                        v.extend(p[j + 1..i].iter().rev());
                    }
                    v.push(t);
                    v
                }
                Err(_) => vec![s, t],
            },
        };
        if !same {
            pts.reverse();
        }
        Ok(pts)
    }
}

/// Face on a cylinder: triangulated in (angle·r, height) and refined so chords follow the surface.
fn cylinder_face(fr: &Frame, r: f64, rings: &[Vec<V3>], out: &mut Tri) {
    if !(r > 0.0 && r.is_finite()) {
        out.poly(rings);
        return;
    }
    let mut uv: Vec<Vec<V3>> = vec![];
    let mut prev: Option<f64> = None;
    for ring in rings {
        let mut v = vec![];
        for p in ring {
            let q = sub(*p, fr.o);
            let mut a = dot(q, fr.y).atan2(dot(q, fr.x));
            if let Some(pa) = prev {
                while a - pa > PI {
                    a -= TAU;
                }
                while a - pa < -PI {
                    a += TAU;
                }
            }
            prev = Some(a);
            v.push([a * r, dot(q, fr.z), 0.0]);
        }
        uv.push(v);
    }
    let map = |q: V3| {
        let a = q[0] / r;
        fr.apply([r * a.cos(), r * a.sin(), q[1]])
    };
    let mut flat = Tri::default();
    flat.poly(&uv);
    // `poly` winds like the uv boundary and the uv→surface map keeps orientation, so the 3D
    // boundary winding survives as is
    let lim = r * PI / 12.0;
    let mut stack: Vec<([V3; 3], usize)> = flat
        .t
        .iter()
        .map(|t| {
            (
                [
                    flat.p[t[0] as usize],
                    flat.p[t[1] as usize],
                    flat.p[t[2] as usize],
                ],
                0,
            )
        })
        .collect();
    while let Some((t, d)) = stack.pop() {
        let span = t.iter().map(|p| p[0]).fold(f64::MIN, f64::max)
            - t.iter().map(|p| p[0]).fold(f64::MAX, f64::min);
        if span > lim && d < 5 && out.t.len() < MAX_TRIS {
            let m = |a: V3, b: V3| mul(add(a, b), 0.5);
            let (ab, bc, ca) = (m(t[0], t[1]), m(t[1], t[2]), m(t[2], t[0]));
            for s in [[t[0], ab, ca], [ab, t[1], bc], [ca, bc, t[2]], [ab, bc, ca]] {
                stack.push((s, d + 1));
            }
            continue;
        }
        out.tri(map(t[0]), map(t[1]), map(t[2]));
    }
}

fn int_list(v: &Value) -> Vec<i64> {
    v.as_list().map_or(vec![], |l| {
        l.iter()
            .filter_map(|x| x.as_f64())
            .map(|x| x as i64)
            .collect()
    })
}

pub(super) fn is_curve_only(w: Why) -> bool {
    w == "curves only"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_with_hole_triangulates_to_its_area() {
        let sq = |s: f64| vec![[-s, -s, 0.0], [s, -s, 0.0], [s, s, 0.0], [-s, s, 0.0]];
        let mut h = sq(1.0);
        h.reverse();
        let rings = vec![sq(2.0), h];
        let mut m = Tri::default();
        m.poly(&rings);
        let a: f64 =
            m.t.iter()
                .map(|t| {
                    let (p, q, r) = (m.p[t[0] as usize], m.p[t[1] as usize], m.p[t[2] as usize]);
                    cross(sub(q, p), sub(r, p))[2] / 2.0
                })
                .sum();
        assert!((a - 12.0).abs() < 1e-9, "{a}");
    }

    #[test]
    fn clip_caps_a_cube() {
        let mut m = Tri::default();
        let r = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        extrude(&[vec![r]], [0.0, 0.0, 1.0], &mut m);
        m.clip([0.0, 0.0, 0.5], [0.0, 0.0, 1.0]);
        let (lo, hi) = m.bbox().unwrap();
        assert_eq!((lo[2], hi[2]), (0.0, 0.5));
        // closed volume: divergence sum of z·nz·area = volume
        let vol: f64 =
            m.t.iter()
                .map(|t| {
                    let (p, q, r) = (m.p[t[0] as usize], m.p[t[1] as usize], m.p[t[2] as usize]);
                    dot(p, cross(q, r)) / 6.0
                })
                .sum();
        assert!((vol - 0.5).abs() < 1e-9, "{vol}");
    }

    #[test]
    fn arc_through_three_points() {
        let Some(Seg::Arc { c, sweep, .. }) =
            arc3([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0])
        else {
            panic!()
        };
        assert!(len(c) < 1e-12 && (sweep - PI).abs() < 1e-12);
        let Some(Seg::Arc { sweep, .. }) = arc3([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0])
        else {
            panic!()
        };
        assert!((sweep - PI).abs() < 1e-12);
    }

    fn step(data: &str) -> crate::ifc::reader::StepFile {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_SCHEMA(('IFC4X3'));\nENDSEC;\nDATA;\n{data}\nENDSEC;\nEND-ISO-10303-21;\n"
        );
        crate::ifc::reader::StepFile::parse(src.as_bytes()).unwrap()
    }

    fn rd(f: &crate::ifc::reader::StepFile) -> Rd<'_> {
        Rd {
            f,
            scale: 1.0,
            angle: 1.0,
            frames: std::collections::HashMap::new(),
            maps: std::collections::HashMap::new(),
        }
    }

    fn segs_of(data: &str, id: u32) -> Result<Vec<Seg>, Why> {
        let f = step(data);
        let r = rd(&f);
        r.segs(id, 0)
    }

    fn mesh_of(data: &str, id: u32) -> (Tri, Vec<Why>) {
        let f = step(data);
        let mut rd = rd(&f);
        let mut approx = vec![];
        let m = rd.item_mesh(id, 0, &mut approx).unwrap();
        (m, approx)
    }

    fn volume(m: &Tri) -> f64 {
        m.t.iter()
            .map(|t| {
                let [p, q, r] = t.map(|i| m.p[i as usize]);
                dot(p, cross(q, r)) / 6.0
            })
            .sum()
    }

    /// Vector area of the surface: zero for a closed one.
    fn open_area(m: &Tri) -> f64 {
        let s = m.t.iter().fold([0.0; 3], |s, t| {
            let [p, q, r] = t.map(|i| m.p[i as usize]);
            add(s, cross(sub(q, p), sub(r, p)))
        });
        len(s)
    }

    /// A bounded half-space removes only what lies inside its boundary prism, even when the
    /// plane also passes through the rest of the body.
    #[test]
    fn bounded_half_space_cuts_only_inside_its_boundary() {
        let (m, approx) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCAXIS2PLACEMENT3D(#1,$,$);\n\
             #3=IFCBLOCK(#2,2.,1.,1.);\n#4=IFCCARTESIANPOINT((0.,0.,0.5));\n\
             #5=IFCAXIS2PLACEMENT3D(#4,$,$);\n#6=IFCPLANE(#5);\n\
             #7=IFCCARTESIANPOINT((-0.5,-1.));\n#8=IFCCARTESIANPOINT((1.,-1.));\n\
             #9=IFCCARTESIANPOINT((1.,2.));\n#10=IFCCARTESIANPOINT((-0.5,2.));\n\
             #11=IFCPOLYLINE((#7,#8,#9,#10,#7));\n\
             #12=IFCPOLYGONALBOUNDEDHALFSPACE(#6,.F.,#2,#11);\n\
             #13=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#3,#12);",
            13,
        );
        assert!(approx.is_empty(), "{approx:?}");
        assert!((volume(&m) - 1.5).abs() < 1e-6, "{}", volume(&m));
        assert!(open_area(&m) < 1e-9);
        let top = |lo: f64, hi: f64| {
            m.p.iter()
                .filter(|p| p[0] > lo && p[0] < hi)
                .map(|p| p[2])
                .fold(0.0, f64::max)
        };
        assert!((top(-0.1, 0.9) - 0.5).abs() < 1e-9 && (top(1.1, 2.1) - 1.0).abs() < 1e-9);
    }

    /// FZK-Haus gable wall: two bounded half-spaces, one per roof slope, meet at the ridge.
    #[test]
    fn gable_wall_clipped_by_two_bounded_half_spaces() {
        let (m, approx) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.3,-0.3));\n#2=IFCCARTESIANPOINT((9.7,-0.3));\n\
             #3=IFCCARTESIANPOINT((10.,0.));\n#4=IFCCARTESIANPOINT((0.,0.));\n\
             #5=IFCPOLYLINE((#1,#2,#3,#4,#1));\n\
             #6=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,'',#5);\n\
             #7=IFCCARTESIANPOINT((0.,0.,0.));\n#8=IFCAXIS2PLACEMENT3D(#7,$,$);\n\
             #9=IFCDIRECTION((0.,0.,1.));\n#10=IFCEXTRUDEDAREASOLID(#6,#8,#9,3.5);\n\
             #11=IFCDIRECTION((0.,1.,0.));\n#12=IFCDIRECTION((0.5,0.,-0.866025403784));\n\
             #13=IFCCARTESIANPOINT((-0.216506350946,0.,0.375));\n\
             #14=IFCAXIS2PLACEMENT3D(#13,#12,#11);\n#15=IFCPLANE(#14);\n\
             #16=IFCCARTESIANPOINT((-0.01,-0.31));\n#17=IFCCARTESIANPOINT((5.,-0.31));\n\
             #18=IFCCARTESIANPOINT((5.,0.01));\n#19=IFCCARTESIANPOINT((-0.01,0.01));\n\
             #20=IFCPOLYLINE((#16,#17,#18,#19,#16));\n\
             #21=IFCPOLYGONALBOUNDEDHALFSPACE(#15,.T.,#8,#20);\n\
             #22=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#10,#21);\n\
             #23=IFCDIRECTION((-0.5,0.,-0.866025403784));\n\
             #24=IFCCARTESIANPOINT((2.71650635095,0.,4.70512701892));\n\
             #25=IFCAXIS2PLACEMENT3D(#24,#23,#11);\n#26=IFCPLANE(#25);\n\
             #27=IFCCARTESIANPOINT((5.,-0.31));\n#28=IFCCARTESIANPOINT((10.01,-0.31));\n\
             #29=IFCCARTESIANPOINT((10.01,0.01));\n#30=IFCCARTESIANPOINT((5.,0.01));\n\
             #31=IFCPOLYLINE((#27,#28,#29,#30,#27));\n\
             #32=IFCPOLYGONALBOUNDEDHALFSPACE(#26,.T.,#8,#31);\n\
             #33=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#22,#32);",
            33,
        );
        assert!(approx.is_empty(), "{approx:?}");
        assert!(open_area(&m) < 1e-6);
        // ridge at x = 5: 0.5 + 5·tan 30°; nothing of the 3.5 m box sticks out above the slopes
        let t = 30f64.to_radians().tan();
        for p in &m.p {
            let roof = 0.5 + p[0].min(10.0 - p[0]) * t;
            assert!(p[2] <= roof.min(3.5) + 1e-6, "{p:?} above the roof {roof}");
        }
        let top = m.p.iter().map(|p| p[2]).fold(0.0, f64::max);
        assert!((top - (0.5 + 5.0 * t)).abs() < 1e-6, "{top}");
    }

    #[test]
    fn sectioned_spine_two_rect_sections_volume() {
        let (m, approx) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCCARTESIANPOINT((0.,0.,10.));\n\
             #3=IFCPOLYLINE((#1,#2));\n#4=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#3);\n\
             #5=IFCCOMPOSITECURVE((#4),.F.);\n\
             #6=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,1.);\n\
             #7=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,4.,2.);\n\
             #8=IFCAXIS2PLACEMENT3D(#1,$,$);\n#9=IFCAXIS2PLACEMENT3D(#2,$,$);\n\
             #10=IFCSECTIONEDSPINE(#5,(#6,#7),(#8,#9));",
            10,
        );
        assert!(approx.is_empty(), "{approx:?}");
        // frustum of similar rectangles: h/3 (A1 + A2 + sqrt(A1 A2))
        let v = volume(&m);
        assert!((v - 140.0 / 3.0).abs() < 1e-9, "{v}");
        assert!(open_area(&m) < 1e-9);
        let (lo, hi) = m.bbox().unwrap();
        assert!(len(sub(lo, [-2.0, -1.0, 0.0])) < 1e-12, "{lo:?}");
        assert!(len(sub(hi, [2.0, 1.0, 10.0])) < 1e-12, "{hi:?}");
    }

    #[test]
    fn sectioned_solid_horizontal_along_line() {
        let (m, _) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCCARTESIANPOINT((100.,0.,0.));\n\
             #3=IFCPOLYLINE((#1,#2));\n#4=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,1.);\n\
             #5=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(10.),$,5.,$,#3);\n\
             #6=IFCAXIS2PLACEMENTLINEAR(#5,$,$);\n\
             #7=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(30.),$,5.,$,#3);\n\
             #8=IFCAXIS2PLACEMENTLINEAR(#7,$,$);\n\
             #9=IFCSECTIONEDSOLIDHORIZONTAL(#3,(#4,#4),(#6,#8));",
            9,
        );
        let v = volume(&m);
        assert!((v - 40.0).abs() < 1e-9, "{v}");
        assert!(open_area(&m) < 1e-9);
        let (lo, hi) = m.bbox().unwrap();
        // profile x lateral (y), profile y vertical (z), lifted by the vertical offset
        assert!(len(sub(lo, [10.0, -1.0, 4.5])) < 1e-9, "{lo:?}");
        assert!(len(sub(hi, [30.0, 1.0, 5.5])) < 1e-9, "{hi:?}");
    }

    #[test]
    fn unequal_sections_resampled_closed() {
        let (m, _) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.,0.,0.));\n#2=IFCCARTESIANPOINT((0.,0.,5.));\n\
             #3=IFCPOLYLINE((#1,#2));\n\
             #4=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,2.);\n\
             #5=IFCCIRCLEPROFILEDEF(.AREA.,$,$,1.);\n\
             #6=IFCAXIS2PLACEMENT3D(#1,$,$);\n#7=IFCAXIS2PLACEMENT3D(#2,$,$);\n\
             #8=IFCSECTIONEDSPINE(#3,(#4,#5),(#6,#7));",
            8,
        );
        assert!(open_area(&m) < 1e-9);
        let v = volume(&m);
        assert!(v > 5.0 * PI * 0.99 && v < 20.0, "{v}");
        let used = || m.t.iter().flatten().map(|&i| m.p[i as usize]);
        // bottom samples stay on the square, top samples on the circle's chords
        let bottom: Vec<V3> = used().filter(|p| p[2].abs() < 1e-12).collect();
        let top: Vec<V3> = used().filter(|p| (p[2] - 5.0).abs() < 1e-12).collect();
        assert_eq!(bottom.len() + top.len(), m.t.len() * 3);
        assert!(bottom.iter().all(|p| (p[0].abs().max(p[1].abs()) - 1.0).abs() < 1e-9));
        let rmin = (PI / 48.0).cos() - 1e-9;
        assert!(top.iter().all(|p| (rmin..=1.0 + 1e-9).contains(&p[0].hypot(p[1]))));
    }

    /// Composite of one IFC4X3 curve segment `#9` over parent `#8` (placement `#7`).
    fn one_segment(parent: &str, place: &str, start: &str, length: &str) -> String {
        format!(
            "{parent}\n{place}\n\
             #9=IFCCURVESEGMENT(.CONTINUOUS.,#7,{start},{length},#8);\n\
             #10=IFCCOMPOSITECURVE((#9),.F.);"
        )
    }

    #[test]
    fn curve_segment_line_length() {
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCVECTOR(#2,2.);\n#8=IFCLINE(#1,#3);",
            "#4=IFCCARTESIANPOINT((1.,2.));\n#5=IFCDIRECTION((0.,1.));\n\
             #7=IFCAXIS2PLACEMENT2D(#4,#5);",
            "IFCPARAMETERVALUE(1.)",
            "IFCLENGTHMEASURE(5.)",
        );
        let s = segs_of(&data, 10).unwrap();
        let p = polyline(&s);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(len(sub(p[0], [1.0, 2.0, 0.0])) < 1e-12, "{p:?}");
        assert!(len(sub(p[1], [1.0, 7.0, 0.0])) < 1e-12, "{p:?}");
    }

    #[test]
    fn curve_segment_circle_arc_endpoints() {
        // clockwise quarter: centre below the start, run against the parameter
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,-5.));\n#2=IFCDIRECTION((0.,1.));\n\
             #3=IFCAXIS2PLACEMENT2D(#1,#2);\n#8=IFCCIRCLE(#3,5.);",
            "#4=IFCCARTESIANPOINT((10.,0.));\n#5=IFCDIRECTION((1.,0.));\n\
             #7=IFCAXIS2PLACEMENT2D(#4,#5);",
            "IFCPARAMETERVALUE(0.)",
            &format!("IFCLENGTHMEASURE({:?})", -5.0 * PI / 2.0),
        );
        let s = segs_of(&data, 10).unwrap();
        let [Seg::Arc { a, b, c, sweep, .. }] = s[..] else {
            panic!("{s:?}")
        };
        assert!(len(sub(a, [10.0, 0.0, 0.0])) < 1e-9, "{a:?}");
        assert!(len(sub(b, [15.0, -5.0, 0.0])) < 1e-9, "{b:?}");
        assert!(len(sub(c, [10.0, -5.0, 0.0])) < 1e-9, "{c:?}");
        assert!((sweep + PI / 2.0).abs() < 1e-9, "{sweep}");
        // samples stay on the circle; the first step heads along the placement x axis
        let p = polyline(&s);
        assert!(p.iter().all(|q| (len(sub(*q, c)) - 5.0).abs() < 1e-9));
        assert!(p[1][0] > 10.0 && p[1][1] < 0.0, "{:?}", p[1]);
    }

    #[test]
    fn curve_segment_unknown_parent_refused() {
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#3=IFCAXIS2PLACEMENT2D(#1,$);\n\
             #8=IFCELLIPSE(#3,5.,3.);",
            "#7=IFCAXIS2PLACEMENT2D(#1,$);",
            "IFCLENGTHMEASURE(0.)",
            "IFCLENGTHMEASURE(50.)",
        );
        assert_eq!(segs_of(&data, 10).unwrap_err(), "unsupported curve segment");
    }

    /// End of the curve with heading a*s^m (curvature m*a*s^(m-1)), by its Taylor series.
    fn mono_end(a: f64, m: i32, s: f64) -> (f64, f64) {
        let fact = |n: i32| (1..=n).map(|i| i as f64).product::<f64>();
        let (mut x, mut y) = (0.0, 0.0);
        for k in 0..16 {
            let sg = if k % 2 == 0 { 1.0 } else { -1.0 };
            x += sg * a.powi(2 * k) * s.powi(2 * k * m) / (fact(2 * k) * (2 * k * m + 1) as f64);
            y += sg * a.powi(2 * k + 1) * s.powi((2 * k + 1) * m)
                / (fact(2 * k + 1) * ((2 * k + 1) * m + 1) as f64);
        }
        (x * s, y * s)
    }

    fn spiral_end(parent: &str, start: f64, length: f64) -> V3 {
        let data = one_segment(
            &format!("#1=IFCCARTESIANPOINT((0.,0.));\n#3=IFCAXIS2PLACEMENT2D(#1,$);\n#8={parent};"),
            "#7=IFCAXIS2PLACEMENT2D(#1,$);",
            &format!("IFCLENGTHMEASURE({start:?})"),
            &format!("IFCLENGTHMEASURE({length:?})"),
        );
        let p = polyline(&segs_of(&data, 10).unwrap());
        assert!(len(p[0]) < 1e-9, "{:?}", p[0]);
        p[p.len() - 1]
    }

    fn near(p: V3, q: (f64, f64)) {
        assert!((p[0] - q.0).abs() < 1e-6 && (p[1] - q.1).abs() < 1e-6, "{p:?} vs {q:?}");
    }

    #[test]
    fn spiral_clothoid_endpoint_matches_series() {
        // curvature s/A^2: heading s^2/(2A^2)
        let e = mono_end(1.0 / 20000.0, 2, 150.0);
        near(spiral_end("IFCCLOTHOID(#3,100.)", 0.0, 150.0), e);
        // a negative constant turns clockwise
        near(spiral_end("IFCCLOTHOID(#3,-100.)", 0.0, 150.0), (e.0, -e.1));
    }

    #[test]
    fn spiral_clothoid_partial_chord() {
        // the chord between s = 40 and 130 does not depend on the placement frame
        let (a, b) = (mono_end(1.0 / 20000.0, 2, 130.0), mono_end(1.0 / 20000.0, 2, 40.0));
        let chord = (a.0 - b.0).hypot(a.1 - b.1);
        let p = spiral_end("IFCCLOTHOID(#3,100.)", 40.0, 90.0);
        assert!((len(p) - chord).abs() < 1e-6, "{p:?} {chord}");
    }

    /// End of the plane curve with heading `h(s)` from 0 to `s`: composite 3-point Gauss-Legendre
    /// on the spec's closed-form heading (the importer integrates curvature by Simpson instead).
    fn heading_end(h: &dyn Fn(f64) -> f64, s: f64) -> (f64, f64) {
        let n = 4000;
        let dh = s / n as f64;
        let g = (0.6_f64).sqrt() / 2.0;
        let (mut x, mut y) = (0.0, 0.0);
        for i in 0..n {
            let m = (i as f64 + 0.5) * dh;
            for (t, w) in [(-g, 5.0 / 18.0), (0.0, 8.0 / 18.0), (g, 5.0 / 18.0)] {
                let th = h(m + t * dh);
                x += w * dh * th.cos();
                y += w * dh * th.sin();
            }
        }
        (x, y)
    }

    #[test]
    fn spiral_polynomials_follow_ifc4x3_terms() {
        // IFC4X3: a term of power p adds s^p * sign(A)/|A|^(p+1); heading = integral
        // 2nd order A2 = 20: kappa s^2/8000, heading s^3/24000
        near(
            spiral_end("IFCSECONDORDERPOLYNOMIALSPIRAL(#3,20.,$,$)", 0.0, 30.0),
            mono_end(1.0 / 24000.0, 3, 30.0),
        );
        // A2 < 0 (odd power p+1 = 3): s^2/A2^3 turns clockwise
        near(
            spiral_end("IFCSECONDORDERPOLYNOMIALSPIRAL(#3,-20.,$,$)", 0.0, 30.0),
            mono_end(-1.0 / 24000.0, 3, 30.0),
        );
        // the linear term A1 s/|A1|^3 = s/(A1*|A1|) equals a clothoid of the same A
        near(
            spiral_end("IFCSECONDORDERPOLYNOMIALSPIRAL(#3,$,-30.,$)", 0.0, 40.0),
            mono_end(-1.0 / 1800.0, 2, 40.0),
        );
        // 3rd order A3 = 10: A3 s^3/|A3|^5 = s^3/1e4, heading s^4/4e4
        near(
            spiral_end("IFCTHIRDORDERPOLYNOMIALSPIRAL(#3,10.,$,$,$)", 0.0, 12.0),
            mono_end(1.0 / 4.0e4, 4, 12.0),
        );
        near(
            spiral_end("IFCTHIRDORDERPOLYNOMIALSPIRAL(#3,-10.,$,$,$)", 0.0, 12.0),
            mono_end(-1.0 / 4.0e4, 4, 12.0),
        );
        // 7th order A7 = 5: s^7/5^8, heading s^8/(8*5^8) = s^8/3125000
        near(
            spiral_end("IFCSEVENTHORDERPOLYNOMIALSPIRAL(#3,5.,$,$,$,$,$,$,$)", 0.0, 6.0),
            mono_end(1.0 / 3.125e6, 8, 6.0),
        );
        // its sextic term A6 = -4: s^6/A6^7 = -s^6/16384, heading -s^7/114688
        near(
            spiral_end("IFCSEVENTHORDERPOLYNOMIALSPIRAL(#3,$,-4.,$,$,$,$,$,$)", 0.0, 5.0),
            mono_end(-1.0 / 114688.0, 7, 5.0),
        );
    }

    #[test]
    fn spiral_polynomial_terms_add_up() {
        // 2nd order A2 = 20, A1 = 30, A0 = 100: heading s/100 + s^2/1800 + s^3/24000
        let h = |s: f64| s / 100.0 + s * s / 1800.0 + s.powi(3) / 24000.0;
        near(
            spiral_end("IFCSECONDORDERPOLYNOMIALSPIRAL(#3,20.,30.,100.)", 0.0, 30.0),
            heading_end(&h, 30.0),
        );
        // 7th order with every term: kappa sum of sign(Ap)/|Ap|^(p+1) s^p
        let a = [9.0_f64, -8.0, 7.0, -6.0, 5.0, -40.0, 30.0, 200.0]; // A7 .. A0
        let h = |s: f64| {
            a.iter()
                .enumerate()
                .map(|(j, &ap)| {
                    let p = (7 - j) as i32;
                    ap.signum() / ap.abs().powi(p + 1) * s.powi(p + 1) / (p + 1) as f64
                })
                .sum::<f64>()
        };
        near(
            spiral_end(
                "IFCSEVENTHORDERPOLYNOMIALSPIRAL(#3,9.,-8.,7.,-6.,5.,-40.,30.,200.)",
                0.0,
                8.0,
            ),
            heading_end(&h, 8.0),
        );
    }

    #[test]
    fn spiral_constant_term_is_a_circle() {
        // curvature 1/50 over a quarter turn, then the mirrored one
        near(
            spiral_end("IFCSECONDORDERPOLYNOMIALSPIRAL(#3,$,$,50.)", 0.0, 25.0 * PI),
            (50.0, 50.0),
        );
        near(
            spiral_end("IFCTHIRDORDERPOLYNOMIALSPIRAL(#3,$,$,$,-50.)", 0.0, 25.0 * PI),
            (50.0, -50.0),
        );
    }

    #[test]
    fn spiral_sine_and_cosine_follow_ifc4x3_heading() {
        // IFC4X3 headings with L = |SegmentLength|:
        // sine   s/A0 + (A1/|A1|)/2*(s/A1)^2 - L/(2*pi*A2)*(cos(2*pi*s/L) - 1)
        // cosine s/A0 + L/(pi*A1)*sin(pi*s/L)
        let l = 100.0_f64;
        let sine = |a2: f64, a1: Option<f64>, a0: Option<f64>| {
            move |s: f64| {
                a0.map_or(0.0, |a| s / a)
                    + a1.map_or(0.0, |a| a.signum() / 2.0 * (s / a).powi(2))
                    - l / (2.0 * PI * a2) * ((2.0 * PI * s / l).cos() - 1.0)
            }
        };
        let cosine = |a1: f64, a0: Option<f64>| {
            move |s: f64| a0.map_or(0.0, |a| s / a) + l / (PI * a1) * (PI * s / l).sin()
        };
        near(
            spiral_end("IFCSINESPIRAL(#3,40.,$,$)", 0.0, l),
            heading_end(&sine(40.0, None, None), l),
        );
        near(
            spiral_end("IFCSINESPIRAL(#3,-40.,-60.,200.)", 0.0, l),
            heading_end(&sine(-40.0, Some(-60.0), Some(200.0)), l),
        );
        near(
            spiral_end("IFCCOSINESPIRAL(#3,40.,$)", 0.0, l),
            heading_end(&cosine(40.0, None), l),
        );
        near(
            spiral_end("IFCCOSINESPIRAL(#3,-40.,150.)", 0.0, l),
            heading_end(&cosine(-40.0, Some(150.0)), l),
        );
        // the period follows the segment length: half the length is not half the curve
        let h = |s: f64| -50.0 / (2.0 * PI * 40.0) * ((2.0 * PI * s / 50.0).cos() - 1.0);
        near(spiral_end("IFCSINESPIRAL(#3,40.,$,$)", 0.0, 50.0), heading_end(&h, 50.0));
    }

    #[test]
    fn spiral_without_terms_is_refused() {
        for parent in [
            "IFCSECONDORDERPOLYNOMIALSPIRAL(#3,$,$,$)",
            "IFCTHIRDORDERPOLYNOMIALSPIRAL(#3,0.,$,$,$)",
            "IFCSINESPIRAL(#3,$,30.,100.)",
            "IFCCOSINESPIRAL(#3,0.,100.)",
            "IFCCLOTHOID(#3,0.)",
        ] {
            let data = one_segment(
                &format!("#1=IFCCARTESIANPOINT((0.,0.));
#3=IFCAXIS2PLACEMENT2D(#1,$);
#8={parent};"),
                "#7=IFCAXIS2PLACEMENT2D(#1,$);",
                "IFCLENGTHMEASURE(0.)",
                "IFCLENGTHMEASURE(10.)",
            );
            assert_eq!(segs_of(&data, 10).unwrap_err(), "bad spiral", "{parent}");
        }
    }

    #[test]
    fn spiral_alignment_chain_is_continuous() {
        // straight 10 m along x, then a clothoid placed at its end and walked 20 m
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCVECTOR(#2,1.);\n#4=IFCLINE(#1,#3);\n#5=IFCAXIS2PLACEMENT2D(#1,#2);\n\
             #6=IFCCARTESIANPOINT((10.,0.));\n#7=IFCAXIS2PLACEMENT2D(#6,#2);\n\
             #8=IFCCLOTHOID(#5,30.);\n\
             #9=IFCCURVESEGMENT(.CONTINUOUS.,#5,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#4);\n\
             #10=IFCCURVESEGMENT(.CONTINUOUS.,#7,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(20.),#8);\n\
             #11=IFCCOMPOSITECURVE((#9,#10),.F.);";
        let p = polyline(&segs_of(data, 11).unwrap());
        let e = mono_end(1.0 / 1800.0, 2, 20.0);
        assert!(len(sub(p[0], [0.0, 0.0, 0.0])) < 1e-9, "{:?}", p[0]);
        near(p[p.len() - 1], (10.0 + e.0, e.1));
        assert!(p.iter().any(|q| (q[0] - 10.0).abs() < 1e-9 && q[1].abs() < 1e-9));
    }

    #[test]
    fn sectioned_solid_horizontal_along_curve_segments() {
        // line 0..10, then a polyline trimmed by parameters 0.5..2 (s 2..10) placed at x 10
        let (m, approx) = mesh_of(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCVECTOR(#2,1.);\n#4=IFCLINE(#1,#3);\n#5=IFCAXIS2PLACEMENT2D(#1,#2);\n\
             #6=IFCCURVESEGMENT(.CONTINUOUS.,#5,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#4);\n\
             #7=IFCCARTESIANPOINT((4.,0.));\n#8=IFCCARTESIANPOINT((10.,0.));\n\
             #9=IFCPOLYLINE((#1,#7,#8));\n#10=IFCAXIS2PLACEMENT2D(#8,#2);\n\
             #11=IFCCURVESEGMENT(.CONTINUOUS.,#10,IFCPARAMETERVALUE(0.5),IFCPARAMETERVALUE(1.5),#9);\n\
             #12=IFCCOMPOSITECURVE((#6,#11),.F.);\n\
             #13=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,1.);\n\
             #14=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(5.),$,5.,$,#12);\n\
             #15=IFCAXIS2PLACEMENTLINEAR(#14,$,$);\n\
             #16=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(15.),$,5.,$,#12);\n\
             #17=IFCAXIS2PLACEMENTLINEAR(#16,$,$);\n\
             #18=IFCSECTIONEDSOLIDHORIZONTAL(#12,(#13,#13),(#15,#17));",
            18,
        );
        assert!(approx.is_empty(), "{approx:?}");
        let v = volume(&m);
        assert!((v - 20.0).abs() < 1e-9, "{v}");
        assert!(open_area(&m) < 1e-9);
        let (lo, hi) = m.bbox().unwrap();
        assert!(len(sub(lo, [5.0, -1.0, 4.5])) < 1e-9, "{lo:?}");
        assert!(len(sub(hi, [15.0, 1.0, 5.5])) < 1e-9, "{hi:?}");
    }

    /// Plan polyline (0,0)-(50,0)-(100,0) in `#1..#4`, profile line rising 10 over 100 in `#5..#9`.
    const GRADE_BASE: &str = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((50.,0.));\n\
        #3=IFCCARTESIANPOINT((100.,0.));\n#4=IFCPOLYLINE((#1,#2,#3));\n\
        #5=IFCCARTESIANPOINT((100.,10.));\n#6=IFCPOLYLINE((#1,#5));\n\
        #7=IFCDIRECTION((1.,0.1));\n#8=IFCAXIS2PLACEMENT2D(#1,#7);\n\
        #9=IFCCURVESEGMENT(.CONTINUOUS.,#8,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#6);\n";

    #[test]
    fn gradient_curve_constant_slope_z() {
        let data = format!("{GRADE_BASE}#10=IFCGRADIENTCURVE((#9),.F.,#4,$);");
        let p = polyline(&segs_of(&data, 10).unwrap());
        assert_eq!(p.len(), 3, "{p:?}");
        for (q, e) in p.iter().zip([[0.0, 0.0, 0.0], [50.0, 0.0, 5.0], [100.0, 0.0, 10.0]]) {
            assert!(len(sub(*q, e)) < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn gradient_curve_follows_base_arc_plan() {
        let l = 5.0 * PI;
        let data = format!(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCAXIS2PLACEMENT2D(#1,$);\n#4=IFCCIRCLE(#3,10.);\n\
             #5=IFCAXIS2PLACEMENT2D(#1,#2);\n\
             #6=IFCCURVESEGMENT(.CONTINUOUS.,#5,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE({l:?}),#4);\n\
             #7=IFCCOMPOSITECURVE((#6),.F.);\n\
             #8=IFCCARTESIANPOINT(({l:?},{:?}));\n#9=IFCPOLYLINE((#1,#8));\n\
             #11=IFCDIRECTION((1.,0.1));\n#12=IFCAXIS2PLACEMENT2D(#1,#11);\n\
             #13=IFCCURVESEGMENT(.CONTINUOUS.,#12,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#9);\n\
             #14=IFCGRADIENTCURVE((#13),.F.,#7,$);",
            0.1 * l
        );
        let p = polyline(&segs_of(&data, 14).unwrap());
        assert!(p.len() > 8, "{}", p.len());
        for q in &p {
            // plan stays on the quarter arc about (0,10); z = 0.1 * distance along it
            let r = (q[0].hypot(q[1] - 10.0) - 10.0).abs();
            let s = 10.0 * (q[1] - 10.0).atan2(q[0]) + 10.0 * PI / 2.0;
            assert!(r < 1e-9, "{q:?}");
            assert!((q[2] - 0.1 * s).abs() < 1e-9, "{q:?} {s}");
        }
        let e = p[p.len() - 1];
        assert!(len(sub(e, [10.0, 10.0, 0.1 * l])) < 1e-9, "{e:?}");
    }

    #[test]
    fn gradient_curve_profile_vertices_inserted() {
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((100.,0.));\n\
            #3=IFCPOLYLINE((#1,#2));\n#4=IFCCARTESIANPOINT((40.,0.));\n\
            #5=IFCCARTESIANPOINT((100.,6.));\n#6=IFCPOLYLINE((#1,#4));\n\
            #7=IFCPOLYLINE((#4,#5));\n#8=IFCDIRECTION((1.,0.));\n#9=IFCDIRECTION((60.,6.));\n\
            #10=IFCAXIS2PLACEMENT2D(#1,#8);\n#11=IFCAXIS2PLACEMENT2D(#4,#9);\n\
            #12=IFCCURVESEGMENT(.CONTINUOUS.,#10,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#6);\n\
            #13=IFCCURVESEGMENT(.CONTINUOUS.,#11,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#7);\n\
            #14=IFCGRADIENTCURVE((#12,#13),.F.,#3,$);";
        let p = polyline(&segs_of(data, 14).unwrap());
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(len(sub(p[1], [40.0, 0.0, 0.0])) < 1e-9, "{p:?}");
        assert!(len(sub(p[2], [100.0, 0.0, 6.0])) < 1e-9, "{p:?}");
    }

    #[test]
    fn gradient_curve_beyond_base_refused() {
        // profile starts at distance 100: nothing of it lies on the 100 long base
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((100.,0.));\n\
            #3=IFCPOLYLINE((#1,#2));\n#4=IFCCARTESIANPOINT((10.,0.));\n#5=IFCPOLYLINE((#1,#4));\n\
            #6=IFCAXIS2PLACEMENT2D(#2,$);\n\
            #7=IFCCURVESEGMENT(.CONTINUOUS.,#6,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#5);\n\
            #8=IFCGRADIENTCURVE((#7),.F.,#3,$);";
        assert_eq!(segs_of(data, 8).unwrap_err(), "gradient outside its base");
    }

    #[test]
    fn curve_segment_rotated_placement_turns_left() {
        // quarter circle r=5 about the origin, counter-clockwise from (5,0) (tangent +y);
        // placed at (100,50) with its tangent on +y: the arc keeps turning left (towards -x)
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCAXIS2PLACEMENT2D(#1,$);\n#8=IFCCIRCLE(#2,5.);",
            "#4=IFCCARTESIANPOINT((100.,50.));\n#5=IFCDIRECTION((0.,1.));\n\
             #7=IFCAXIS2PLACEMENT2D(#4,#5);",
            "IFCPARAMETERVALUE(0.)",
            &format!("IFCLENGTHMEASURE({:?})", 5.0 * PI / 2.0),
        );
        let s = segs_of(&data, 10).unwrap();
        let [Seg::Arc { a, b, c, n, sweep }] = s[..] else {
            panic!("{s:?}")
        };
        assert!(len(sub(a, [100.0, 50.0, 0.0])) < 1e-9, "{a:?}");
        assert!(len(sub(b, [95.0, 55.0, 0.0])) < 1e-9, "{b:?}");
        assert!(len(sub(c, [95.0, 50.0, 0.0])) < 1e-9, "{c:?}");
        assert!(len(sub(n, [0.0, 0.0, 1.0])) < 1e-9 && (sweep - PI / 2.0).abs() < 1e-9);
    }

    #[test]
    fn curve_segment_placement_3d_rotates_in_space() {
        // placement z along +y, x along +x: the local left (y) axis is -z
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCAXIS2PLACEMENT2D(#1,$);\n#8=IFCCIRCLE(#2,5.);",
            "#4=IFCCARTESIANPOINT((1.,2.,3.));\n#5=IFCDIRECTION((0.,1.,0.));\n\
             #6=IFCDIRECTION((1.,0.,0.));\n#7=IFCAXIS2PLACEMENT3D(#4,#5,#6);",
            "IFCPARAMETERVALUE(0.)",
            &format!("IFCLENGTHMEASURE({:?})", 5.0 * PI / 2.0),
        );
        let s = segs_of(&data, 10).unwrap();
        let [Seg::Arc { a, b, c, n, .. }] = s[..] else {
            panic!("{s:?}")
        };
        assert!(len(sub(a, [1.0, 2.0, 3.0])) < 1e-9, "{a:?}");
        assert!(len(sub(b, [6.0, 2.0, -2.0])) < 1e-9, "{b:?}");
        assert!(len(sub(c, [1.0, 2.0, -2.0])) < 1e-9, "{c:?}");
        assert!(len(sub(n, [0.0, 1.0, 0.0])) < 1e-9, "{n:?}");
    }

    #[test]
    fn curve_segment_negative_length_reverses() {
        // line: runs from s=10 back to s=6, yet the travel tangent lands on the placement x axis
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCVECTOR(#2,1.);\n#8=IFCLINE(#1,#3);",
            "#4=IFCCARTESIANPOINT((0.,0.));\n#7=IFCAXIS2PLACEMENT2D(#4,$);",
            "IFCLENGTHMEASURE(10.)",
            "IFCLENGTHMEASURE(-4.)",
        );
        let p = polyline(&segs_of(&data, 10).unwrap());
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(len(sub(p[0], [0.0, 0.0, 0.0])) < 1e-12, "{p:?}");
        assert!(len(sub(p[1], [4.0, 0.0, 0.0])) < 1e-12, "{p:?}");
        // polyline (0,0)-(4,0)-(4,3) from s=7 back to s=2: heads south, then west = a right turn
        let data = one_segment(
            "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((4.,0.));\n\
             #3=IFCCARTESIANPOINT((4.,3.));\n#8=IFCPOLYLINE((#1,#2,#3));",
            "#4=IFCCARTESIANPOINT((0.,0.));\n#7=IFCAXIS2PLACEMENT2D(#4,$);",
            "IFCLENGTHMEASURE(7.)",
            "IFCLENGTHMEASURE(-5.)",
        );
        let p = polyline(&segs_of(&data, 10).unwrap());
        assert_eq!(p.len(), 3, "{p:?}");
        for (q, e) in p.iter().zip([[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, -2.0, 0.0]]) {
            assert!(len(sub(*q, e)) < 1e-9, "{p:?}");
        }
    }

    #[test]
    fn curve_segments_chained_tangentially() {
        // 10 long line, then a left quarter turn (r=10) started where the line ends
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCDIRECTION((1.,0.));\n\
             #3=IFCVECTOR(#2,1.);\n#4=IFCLINE(#1,#3);\n#5=IFCAXIS2PLACEMENT2D(#1,#2);\n\
             #6=IFCCURVESEGMENT(.CONTINUOUS.,#5,IFCLENGTHMEASURE(0.),IFCLENGTHMEASURE(10.),#4);\n\
             #7=IFCAXIS2PLACEMENT2D(#1,$);\n#8=IFCCIRCLE(#7,10.);\n\
             #9=IFCCARTESIANPOINT((10.,0.));\n#10=IFCAXIS2PLACEMENT2D(#9,#2);\n\
             #11=IFCCURVESEGMENT(.CONTINUOUS.,#10,IFCPARAMETERVALUE(0.),IFCLENGTHMEASURE(15.707963267948966),#8);\n\
             #12=IFCCOMPOSITECURVE((#6,#11),.F.);";
        let s = segs_of(data, 12).unwrap();
        assert_eq!(s.len(), 2, "{s:?}");
        assert!(len(sub(s[0].start(), [0.0, 0.0, 0.0])) < 1e-9);
        let Seg::Line(_, e) = s[0] else { panic!() };
        assert!(len(sub(e, s[1].start())) < 1e-9, "{e:?}");
        let Seg::Arc { b, c, .. } = s[1] else { panic!() };
        assert!(len(sub(c, [10.0, 10.0, 0.0])) < 1e-9, "{c:?}");
        assert!(len(sub(b, [20.0, 10.0, 0.0])) < 1e-9, "{b:?}");
        // first arc chord continues the line direction (+x) and bends towards +y
        let p = polyline(&s);
        let d = norm(sub(p[2], p[1])).unwrap();
        assert!(d[0] > 0.9 && d[1] > 0.0, "{d:?}");
    }

    /// Bounding box of a two-section solid along a polyline in plan, a 2 x 1 profile, lateral offset `lat`.
    fn lateral_box(path: &[[f64; 2]], s: [f64; 2], lat: f64) -> (V3, V3) {
        let n = path.len();
        let mut d = String::new();
        for (i, q) in path.iter().enumerate() {
            d += &format!("#{}=IFCCARTESIANPOINT(({:?},{:?},0.));\n", i + 1, q[0], q[1]);
        }
        let ids: Vec<String> = (1..=n).map(|i| format!("#{i}")).collect();
        d += &format!(
            "#{}=IFCPOLYLINE(({}));\n#{}=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,1.);\n",
            n + 1,
            ids.join(","),
            n + 2
        );
        for (k, v) in s.iter().enumerate() {
            d += &format!(
                "#{}=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE({:?}),{:?},$,$,#{});\n\
                 #{}=IFCAXIS2PLACEMENTLINEAR(#{},$,$);\n",
                n + 3 + 2 * k,
                v,
                lat,
                n + 1,
                n + 4 + 2 * k,
                n + 3 + 2 * k
            );
        }
        d += &format!(
            "#{}=IFCSECTIONEDSOLIDHORIZONTAL(#{},(#{},#{}),(#{},#{}));",
            n + 7,
            n + 1,
            n + 2,
            n + 2,
            n + 4,
            n + 6
        );
        let (m, _) = mesh_of(&d, (n + 7) as u32);
        assert!(open_area(&m) < 1e-9);
        m.bbox().unwrap()
    }

    #[test]
    fn sectioned_solid_lateral_left_positive() {
        let near = |a: V3, b: V3| assert!(len(sub(a, b)) < 1e-9, "{a:?} != {b:?}");
        // heading +x: left is +y
        let (lo, hi) = lateral_box(&[[0.0, 0.0], [100.0, 0.0]], [10.0, 30.0], 3.0);
        near(lo, [10.0, 2.0, -0.5]);
        near(hi, [30.0, 4.0, 0.5]);
        let (lo, hi) = lateral_box(&[[0.0, 0.0], [100.0, 0.0]], [10.0, 30.0], -3.0);
        near(lo, [10.0, -4.0, -0.5]);
        near(hi, [30.0, -2.0, 0.5]);
        // heading -y: left is +x
        let (lo, hi) = lateral_box(&[[0.0, 0.0], [0.0, -100.0]], [10.0, 30.0], 3.0);
        near(lo, [2.0, -30.0, -0.5]);
        near(hi, [4.0, -10.0, 0.5]);
        // left turn at (10,0): on the second leg (heading +y) left is -x
        let (lo, hi) = lateral_box(&[[0.0, 0.0], [10.0, 0.0], [10.0, 20.0]], [5.0, 15.0], 2.0);
        near(lo, [5.0, 1.0, -0.5]);
        near(hi, [9.0, 5.0, 0.5]);
    }

    #[test]
    fn gradient_profile_placement_offsets_and_descends() {
        // profile segment (length 80, flat) placed at distance 20, height 5: only 20..100 is graded
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((100.,0.));\n\
            #3=IFCPOLYLINE((#1,#2));\n#4=IFCCARTESIANPOINT((80.,0.));\n#5=IFCPOLYLINE((#1,#4));\n\
            #6=IFCCARTESIANPOINT((20.,5.));\n#7=IFCAXIS2PLACEMENT2D(#6,$);\n\
            #8=IFCCURVESEGMENT(.CONTINUOUS.,#7,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#5);\n\
            #9=IFCGRADIENTCURVE((#8),.F.,#3,$);";
        let p = polyline(&segs_of(data, 9).unwrap());
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(len(sub(p[0], [20.0, 0.0, 5.0])) < 1e-9, "{p:?}");
        assert!(len(sub(p[1], [100.0, 0.0, 5.0])) < 1e-9, "{p:?}");
        // descending 10 per 100: a positive tangent angle is a rise, a negative one a fall
        let data = "#1=IFCCARTESIANPOINT((0.,0.));\n#2=IFCCARTESIANPOINT((100.,0.));\n\
            #3=IFCPOLYLINE((#1,#2));\n#4=IFCCARTESIANPOINT((100.,10.));\n#5=IFCPOLYLINE((#1,#4));\n\
            #6=IFCDIRECTION((1.,-0.1));\n#7=IFCAXIS2PLACEMENT2D(#1,#6);\n\
            #8=IFCCURVESEGMENT(.CONTINUOUS.,#7,IFCPARAMETERVALUE(0.),IFCPARAMETERVALUE(1.),#5);\n\
            #9=IFCGRADIENTCURVE((#8),.F.,#3,$);";
        let p = polyline(&segs_of(data, 9).unwrap());
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(len(sub(p[1], [100.0, 0.0, -10.0])) < 1e-9, "{p:?}");
    }

    #[test]
    fn segmented_reference_uses_base_curve() {
        let data = format!(
            "{GRADE_BASE}#10=IFCGRADIENTCURVE((#9),.F.,#4,$);\n\
             #11=IFCSEGMENTEDREFERENCECURVE((#9),.F.,#10,$);\n\
             #12=IFCRECTANGLEPROFILEDEF(.AREA.,$,$,2.,1.);\n\
             #13=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(10.),$,$,$,#11);\n\
             #14=IFCAXIS2PLACEMENTLINEAR(#13,$,$);\n\
             #15=IFCPOINTBYDISTANCEEXPRESSION(IFCLENGTHMEASURE(40.),$,$,$,#11);\n\
             #16=IFCAXIS2PLACEMENTLINEAR(#15,$,$);\n\
             #17=IFCSECTIONEDSOLIDHORIZONTAL(#11,(#12,#12),(#14,#16));"
        );
        let a = polyline(&segs_of(&data, 10).unwrap());
        let b = polyline(&segs_of(&data, 11).unwrap());
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(p, q)| len(sub(*p, *q)) < 1e-12));
        let (m, approx) = mesh_of(&data, 17);
        assert_eq!(approx, vec!["cant ignored"]);
        assert!(volume(&m) > 50.0);
        assert!(open_area(&m) < 1e-9);
    }

    fn ring_area(r: &[V3]) -> f64 {
        newell(r)[2].abs() / 2.0
    }

    fn ring_perimeter(r: &[V3]) -> f64 {
        (0..r.len())
            .map(|i| len(sub(r[(i + 1) % r.len()], r[i])))
            .sum()
    }

    #[test]
    fn bezier_curve_on_curve_points() {
        let data = "#1=IFCCARTESIANPOINT((0.,0.));
#2=IFCCARTESIANPOINT((1.,2.));
#3=IFCCARTESIANPOINT((2.,0.));
#4=IFCCARTESIANPOINT((3.,-1.));
#5=IFCCARTESIANPOINT((4.,0.));
#10=IFCBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.);
#11=IFCBEZIERCURVE(2,(#1,#2,#3,#4,#5),.UNSPECIFIED.,.F.,.F.);";
        let p = polyline(&segs_of(data, 10).unwrap());
        assert!(p.len() > 8);
        // x = 2t, y = 4t(1 - t): every point lies on y = 2x - x^2, not on the control polygon
        for q in &p {
            assert!((q[1] - (2.0 * q[0] - q[0] * q[0])).abs() < 1e-9, "{q:?}");
        }
        let top = p.iter().map(|q| q[1]).fold(0.0, f64::max);
        assert!((top - 1.0).abs() < 1e-3, "{top}");
        assert!(len(p[0]) < 1e-9 && len(sub(p[p.len() - 1], [2.0, 0.0, 0.0])) < 1e-9);
        // two spans: the joint (2, 0) is on the curve and the end is reached
        let p = polyline(&segs_of(data, 11).unwrap());
        assert!(p.iter().any(|q| len(sub(*q, [2.0, 0.0, 0.0])) < 1e-9));
        assert!(len(sub(p[p.len() - 1], [4.0, 0.0, 0.0])) < 1e-9);
    }

    #[test]
    fn rational_bezier_quarter_circle_stays_on_the_radius() {
        let data = "#1=IFCCARTESIANPOINT((1.,0.));
#2=IFCCARTESIANPOINT((1.,1.));
#3=IFCCARTESIANPOINT((0.,1.));
#10=IFCRATIONALBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));";
        let p = polyline(&segs_of(data, 10).unwrap());
        assert!(p.len() > 8);
        for q in &p {
            assert!((len(*q) - 1.0).abs() < 1e-9, "{q:?}");
        }
        assert!(len(sub(p[p.len() - 1], [0.0, 1.0, 0.0])) < 1e-9);
    }

    #[test]
    fn degenerate_bezier_is_refused() {
        let data = "#1=IFCCARTESIANPOINT((1.,1.));
#2=IFCCARTESIANPOINT((2.,3.));
#10=IFCBEZIERCURVE(2,(#1,#1,#1),.UNSPECIFIED.,.F.,.F.);
#11=IFCBEZIERCURVE(3,(#1,#2,#1),.UNSPECIFIED.,.F.,.F.);
#12=IFCBEZIERCURVE(0,(#1,#2),.UNSPECIFIED.,.F.,.F.);
#13=IFCRATIONALBEZIERCURVE(2,(#1,#2,#1),.UNSPECIFIED.,.F.,.F.,(1.,0.,1.));
#14=IFCBEZIERCURVE(2,(#1,#2),.UNSPECIFIED.,.F.,.F.);";
        for id in 10..=14 {
            assert!(segs_of(data, id).is_err(), "{id}");
        }
    }

    #[test]
    fn bezier_circle_profile_area_and_perimeter() {
        // four rational quarter arcs of radius 2 (file in millimetres) bound a closed profile
        let data = "#1=IFCCARTESIANPOINT((2000.,0.));
#2=IFCCARTESIANPOINT((2000.,2000.));
#3=IFCCARTESIANPOINT((0.,2000.));
#4=IFCCARTESIANPOINT((-2000.,2000.));
#5=IFCCARTESIANPOINT((-2000.,0.));
#6=IFCCARTESIANPOINT((-2000.,-2000.));
#7=IFCCARTESIANPOINT((0.,-2000.));
#8=IFCCARTESIANPOINT((2000.,-2000.));
#10=IFCRATIONALBEZIERCURVE(2,(#1,#2,#3),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));
#11=IFCRATIONALBEZIERCURVE(2,(#3,#4,#5),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));
#12=IFCRATIONALBEZIERCURVE(2,(#5,#6,#7),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));
#13=IFCRATIONALBEZIERCURVE(2,(#7,#8,#1),.UNSPECIFIED.,.F.,.F.,(1.,0.7071067811865476,1.));
#14=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#10);
#15=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#11);
#16=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#12);
#17=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#13);
#18=IFCCOMPOSITECURVE((#14,#15,#16,#17),.F.);
#19=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#18);";
        let f = step(data);
        let mut r = rd(&f);
        r.scale = 0.001;
        let p = r.profile_area(19, 0).unwrap();
        assert_eq!(p.len(), 1);
        let outer = &p[0][0];
        for q in outer {
            assert!((len(*q) - 2.0).abs() < 1e-9, "{q:?}");
        }
        let (a, l) = (ring_area(outer), ring_perimeter(outer));
        assert!((a / (PI * 4.0) - 1.0).abs() < 5e-3, "{a}");
        assert!((l / (TAU * 2.0) - 1.0).abs() < 5e-3, "{l}");
    }

    #[test]
    fn offset_curve_2d_rectangle() {
        let data = "#1=IFCCARTESIANPOINT((0.,0.));
#2=IFCCARTESIANPOINT((4.,0.));
#3=IFCCARTESIANPOINT((4.,2.));
#4=IFCCARTESIANPOINT((0.,2.));
#5=IFCPOLYLINE((#1,#2,#3,#4,#1));
#6=IFCOFFSETCURVE2D(#5,-0.5,.F.);
#7=IFCOFFSETCURVE2D(#5,0.5,.F.);";
        let a = |id| {
            let r = ring(&segs_of(data, id).unwrap());
            (r.len(), ring_area(&r))
        };
        // right of a counter-clockwise outline is outward
        let (n, ar) = a(6);
        assert_eq!(n, 4);
        assert!((ar - 5.0 * 3.0).abs() < 1e-9, "{ar}");
        let (n, ar) = a(7);
        assert_eq!(n, 4);
        assert!((ar - 3.0 * 1.0).abs() < 1e-9, "{ar}");
    }

    #[test]
    fn offset_of_an_arc_keeps_its_centre() {
        let arc = Seg::Arc {
            a: [1.0, 0.0, 0.0],
            b: [0.0, 1.0, 0.0],
            c: [0.0; 3],
            n: [0.0, 0.0, 1.0],
            sweep: PI / 2.0,
        };
        let inner = offset_segs(&[arc], 0.25).unwrap();
        for q in polyline(&inner) {
            assert!((len(q) - 0.75).abs() < 1e-9);
        }
        let outer = offset_segs(&[arc], -0.5).unwrap();
        for q in polyline(&outer) {
            assert!((len(q) - 1.5).abs() < 1e-9);
        }
        assert!(offset_segs(&[arc], 1.0).is_err());
    }

    #[test]
    fn centerline_profile_area() {
        let data = "#1=IFCCARTESIANPOINT((0.,0.));
#2=IFCCARTESIANPOINT((4.,0.));
#3=IFCCARTESIANPOINT((4.,3.));
#4=IFCPOLYLINE((#1,#2));
#5=IFCPOLYLINE((#1,#2,#3));
#10=IFCCENTERLINEPROFILEDEF(.AREA.,$,#4,0.5);
#11=IFCCENTERLINEPROFILEDEF(.AREA.,$,#5,0.5);
#12=IFCCENTERLINEPROFILEDEF(.AREA.,$,#4,0.);";
        let f = step(data);
        let r = rd(&f);
        let area = |id| {
            let p = r.profile_area(id, 0).unwrap();
            assert_eq!(p.len(), 1);
            ring_area(&p[0][0])
        };
        assert!((area(10) - 2.0).abs() < 1e-9);
        assert!((area(11) - 3.5).abs() < 1e-9);
        assert!(r.profile_area(12, 0).is_err());
    }

    #[test]
    fn centerline_profile_on_a_circle_is_a_ring() {
        // radius 1, wall 0.2: ring between radii 0.9 and 1.1 (arcs sampled at 7.5 degrees)
        let data = "#1=IFCCARTESIANPOINT((0.,0.));
#2=IFCAXIS2PLACEMENT2D(#1,$);
#3=IFCCIRCLE(#2,1.);
#4=IFCCENTERLINEPROFILEDEF(.AREA.,$,#3,0.2);";
        let f = step(data);
        let r = rd(&f);
        let p = r.profile_area(4, 0).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), 2);
        let (o, i) = (&p[0][0], &p[0][1]);
        assert!(newell(o)[2] * newell(i)[2] < 0.0, "hole runs against the outline");
        let a = ring_area(o) - ring_area(i);
        assert!((a / (PI * (1.21 - 0.81)) - 1.0).abs() < 1e-2, "{a}");
        assert!((ring_perimeter(o) / (TAU * 1.1) - 1.0).abs() < 1e-2);
        assert!((ring_perimeter(i) / (TAU * 0.9) - 1.0).abs() < 1e-2);
    }
}
