// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Leaning and tapered straight walls (W5, docs GEOM_PRO_PLAN "wall/lean1").
//!
//! Wall frame: axis `a`, direction `d`, left normal `n`, offset `s` along `n`, height `z` above
//! the wall base. A leaning/tapered wall is the image of its vertical prism under
//! `s -> s * k(z) + z * tan(angle)`, `k(z) = 1 + (top_scale - 1) * z / height`: faces at constant
//! `s` stay planes, the axis surface (`s = 0`) is the plane leaning by `angle`, ends stay
//! vertical. Joins are solved twice (at the base and at the top, `top_strips`) and each layer
//! piece is the ruled solid between its two footprints; its volume is exact by the prismatoid
//! (Simpson) rule because a cross-section's area is quadratic in `z`. Openings are cut normal
//! to the axis surface; the removed volume is the opening width times the clipped area of the
//! layer's cross-section (`cut_area`), which is what the 3D cutter removes.
//!
//! An arc wall (bulge != 0) leans by the radial map: the face at left offset `s` (radius
//! `r - sg s`, `sg` = +1 when the axis turns left) goes to radius `r - sg (s k(z) + shift(z))`,
//! a concentric arc of the same sweep, so every face is a cone and a layer's section area stays
//! quadratic in `z` (Simpson exact). Openings are cut in the radial planes, normal to the cone;
//! their removed volume is the section's moment about the centre times the opening's angle.

use crate::arc::{edge_parts, edge_point, meet, ArcAxis, BulgeRing, Curve as Cut, CHORD_SAG};
use crate::layered::{Opening, Strip};
use crate::math::{v2, v3, Tol, V2, V3};
use crate::mesh::Mesh;
use crate::poly;

/// Largest lean from the vertical, radians (60 degrees).
pub const MAX_LEAN: f64 = std::f64::consts::FRAC_PI_3;
/// Range of the top-to-base thickness ratio.
pub const TOP_SCALE: (f64, f64) = (0.1, 10.0);

/// Lean of the axis surface (radians, positive toward the wall's left normal) and the ratio of
/// the top thickness to the base thickness (1 = no taper).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lean {
    pub angle: f64,
    pub top_scale: f64,
}

impl Default for Lean {
    fn default() -> Self {
        Lean::PLAIN
    }
}

impl Lean {
    pub const PLAIN: Lean = Lean {
        angle: 0.0,
        top_scale: 1.0,
    };

    pub fn new(angle: f64, top_scale: f64) -> Lean {
        Lean { angle, top_scale }
    }

    pub fn is_plain(&self) -> bool {
        self.angle == 0.0 && self.top_scale == 1.0
    }

    pub fn valid(&self) -> bool {
        self.angle.is_finite()
            && self.angle.abs() <= MAX_LEAN + 1e-12
            && self.top_scale.is_finite()
            && (TOP_SCALE.0..=TOP_SCALE.1).contains(&self.top_scale)
    }

    /// Thickness scale at height `z` of a wall of height `h`.
    pub fn k(&self, z: f64, h: f64) -> f64 {
        if h <= 0.0 {
            return 1.0;
        }
        1.0 + (self.top_scale - 1.0) * z / h
    }

    /// Horizontal shift of the axis surface at height `z`.
    pub fn shift(&self, z: f64) -> f64 {
        z * self.angle.tan()
    }

    /// Offset at height `z` of the face that has offset `s` at the base.
    pub fn offset_at(&self, s: f64, z: f64, h: f64) -> f64 {
        s * self.k(z, h) + self.shift(z)
    }

    /// Plan point `p` of the base footprint moved to height `z` (radially on an arc wall).
    pub fn map(&self, st: &Strip, bulge: f64, p: V2, z: f64) -> V2 {
        self.map_in(st, bend(st, bulge), p, z)
    }

    fn map_in(&self, st: &Strip, bd: Option<Bend>, p: V2, z: f64) -> V2 {
        let (s, n) = frame_at(st, bd, p);
        p + n * (self.offset_at(s, z, st.height) - s)
    }

    /// Volume of the mapped prism over a base footprint of area `area`: `area * h * mean k`.
    pub fn prism_volume(&self, area: f64, h: f64) -> f64 {
        area * h * (1.0 + self.top_scale) / 2.0
    }
}

/// Circle of an arc axis: centre, radius and `sg` (+1 when the axis turns left).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bend {
    c: V2,
    r: f64,
    sg: f64,
}

/// The circle of the axis of `st` with `bulge`; None for a straight axis.
fn bend(st: &Strip, bulge: f64) -> Option<Bend> {
    if bulge == 0.0 {
        return None;
    }
    ArcAxis::new(st.a, st.b, bulge, Tol::default()).ok().map(|ax| Bend {
        c: ax.center,
        r: ax.radius,
        sg: ax.sigma(),
    })
}

/// Left offset of plan point `p` from the axis and the axis's left normal there (radial on an
/// arc: toward the centre when the axis turns left).
fn frame_at(st: &Strip, bd: Option<Bend>, p: V2) -> (f64, V2) {
    match bd {
        None => {
            let n = st.normal();
            ((p - st.a).dot(n), n)
        }
        Some(b) => {
            let v = p - b.c;
            let rho = v.len().max(1e-300);
            (b.sg * (b.r - rho), v * (-b.sg / rho))
        }
    }
}

/// Unit tangent of the axis at its point `p`, in the direction a→b.
fn tangent_at(st: &Strip, bd: Option<Bend>, p: V2) -> V2 {
    match bd {
        None => st.dir(),
        Some(b) => {
            let u = (p - b.c) * (1.0 / (p - b.c).len().max(1e-300));
            v2(-u.y, u.x) * b.sg
        }
    }
}

/// Bulge of the arc about `bd.c` from `a` to `b` turning the way `bd.sg` says.
fn bulge_about(bd: Bend, a: V2, b: V2) -> f64 {
    let (u, w) = (a - bd.c, b - bd.c);
    let mut th = u.cross(w).atan2(u.dot(w)) * bd.sg;
    if th <= 0.0 {
        th += std::f64::consts::TAU;
    }
    (th / 4.0).tan() * bd.sg
}

/// The wall cut at height `z` above its base: axis shifted (an arc axis to the concentric arc),
/// layers scaled. The plan of the result, with the same bulge, is the horizontal section of the
/// leaning wall at that height.
pub fn strip_at(st: &Strip, bulge: f64, l: &Lean, z: f64) -> Strip {
    let bd = bend(st, bulge);
    let k = l.k(z, st.height);
    let mv = |p: V2| p + frame_at(st, bd, p).1 * l.shift(z);
    let mut layers = st.layers.clone();
    for x in &mut layers {
        x.thickness *= k;
    }
    Strip {
        a: mv(st.a),
        b: mv(st.b),
        layers,
        offset0: st.offset0 * k,
        base: st.base + z,
        height: st.height,
    }
}

/// `p` lies on the axis of `s` (ends included).
fn on_axis(p: V2, s: &Strip, bd: Option<Bend>) -> bool {
    match bd {
        None => {
            let d = s.b - s.a;
            let t = ((p - s.a).dot(d) / d.dot(d).max(1e-18)).clamp(0.0, 1.0);
            (s.a + d * t - p).len() <= 1e-6
        }
        Some(b) => {
            if ((p - b.c).len() - b.r).abs() > 1e-6 {
                return false;
            }
            if (p - s.a).len() <= 1e-6 || (p - s.b).len() <= 1e-6 {
                return true;
            }
            let (u, w, q) = (s.a - b.c, s.b - b.c, p - b.c);
            let ang = |x: V2| {
                let t = u.cross(x).atan2(u.dot(x)) * b.sg;
                if t < 0.0 {
                    t + std::f64::consts::TAU
                } else {
                    t
                }
            };
            ang(q) <= ang(w)
        }
    }
}

/// The axis of `s` as a line or a circle.
fn axis_curve(s: &Strip, bd: Option<Bend>) -> Cut {
    match bd {
        None => Cut::Line { p: s.a, d: s.b - s.a },
        Some(b) => Cut::Circle { c: b.c, r: b.r },
    }
}

/// Strips at the top of each wall, ends moved onto the neighbours' top lines so that the
/// usual plan join sees the same nodes as at the base. A neighbour is cut at the height of this
/// wall's top. Walls whose ends meet no leaning neighbour keep their ends.
/// Returns the strips and their bulges (an arc's ends move along its concentric arc).
pub fn top_strips(strips: &[Strip], bulges: &[f64], leans: &[Lean]) -> (Vec<Strip>, Vec<f64>) {
    strips_at(strips, bulges, leans, |s| s.base + s.height)
}

/// Horizontal section of every wall at the absolute height `z` (clamped into each wall's own
/// height range), ends moved onto the neighbours' section lines as in [`top_strips`]: the plan
/// join of the result is the section of the joined walls at that height.
pub fn section_strips(
    strips: &[Strip],
    bulges: &[f64],
    leans: &[Lean],
    z: f64,
) -> (Vec<Strip>, Vec<f64>) {
    strips_at(strips, bulges, leans, |_| z)
}

/// Each wall `i` cut at the absolute height `zabs(strips[i])`, its ends met with the
/// neighbours cut at that same height; an arc keeps its concentric circle, its bulge follows.
fn strips_at(
    strips: &[Strip],
    bulges: &[f64],
    leans: &[Lean],
    zabs: impl Fn(&Strip) -> f64,
) -> (Vec<Strip>, Vec<f64>) {
    let lean = |i: usize| leans.get(i).copied().unwrap_or(Lean::PLAIN);
    let bulge = |i: usize| bulges.get(i).copied().unwrap_or(0.0);
    let rel = |s: &Strip, z: f64| (z - s.base).clamp(0.0, s.height);
    let bends: Vec<Option<Bend>> = strips
        .iter()
        .enumerate()
        .map(|(i, s)| bend(s, bulge(i)))
        .collect();
    let mut out: Vec<Strip> = strips
        .iter()
        .enumerate()
        .map(|(i, s)| strip_at(s, bulge(i), &lean(i), rel(s, zabs(s))))
        .collect();
    let mut out_b: Vec<f64> = (0..strips.len()).map(bulge).collect();
    for (i, s) in strips.iter().enumerate() {
        if (s.b - s.a).len() <= 1e-12 {
            continue;
        }
        let ztop = zabs(s);
        // the cut axis of i: the concentric circle of an arc, the shifted line of a straight wall
        let zi = rel(s, ztop);
        let ci = match bends[i] {
            None => axis_curve(&out[i], None),
            Some(b) => Cut::Circle {
                c: b.c,
                r: b.r - b.sg * lean(i).shift(zi),
            },
        };
        let mut moved = false;
        for end in 0..2 {
            let p = if end == 0 { s.a } else { s.b };
            for (j, o) in strips.iter().enumerate() {
                if j == i || (o.b - o.a).len() <= 1e-12 || !on_axis(p, o, bends[j]) {
                    continue;
                }
                let (ti, tj) = (tangent_at(s, bends[i], p), tangent_at(o, bends[j], p));
                if ti.cross(tj).abs() <= 1e-6 {
                    continue;
                }
                if lean(i).is_plain() && lean(j).is_plain() {
                    break;
                }
                let zj = (ztop - o.base).clamp(0.0, o.height);
                let cj = match bends[j] {
                    None => axis_curve(&strip_at(o, 0.0, &lean(j), zj), None),
                    Some(b) => Cut::Circle {
                        c: b.c,
                        r: b.r - b.sg * lean(j).shift(zj),
                    },
                };
                let near = if end == 0 { out[i].a } else { out[i].b };
                if let Some(q) = meet(ci, cj, near, Tol::default()) {
                    if end == 0 {
                        out[i].a = q;
                    } else {
                        out[i].b = q;
                    }
                    moved = true;
                }
                break;
            }
        }
        if let (true, Some(b)) = (moved, bends[i]) {
            out_b[i] = bulge_about(b, out[i].a, out[i].b);
        }
    }
    (out, out_b)
}

/// `top` reordered so that vertex `j` pairs with `base[j]`: same count, same orientation, the
/// cyclic shift nearest to `mapped` (the base mapped by the wall's own lean). None if the
/// rings do not pair.
pub fn pair_rings(mapped: &[V2], top: &[V2]) -> Option<Vec<V2>> {
    let n = mapped.len();
    let r = pair_shift(mapped, top)?;
    Some((0..n).map(|j| top[(j + r) % n]).collect())
}

/// The cyclic shift `r` pairing `top[(j + r) % n]` with `mapped[j]`, as [`pair_rings`].
fn pair_shift(mapped: &[V2], top: &[V2]) -> Option<usize> {
    let n = mapped.len();
    if n < 3 || top.len() != n || (poly::area(mapped) > 0.0) != (poly::area(top) > 0.0) {
        return None;
    }
    let cost = |r: usize| -> f64 {
        (0..n)
            .map(|j| {
                let e = top[(j + r) % n] - mapped[j];
                e.dot(e)
            })
            .sum()
    };
    (0..n).min_by(|&x, &y| cost(x).total_cmp(&cost(y)))
}

/// Base ring `base` and its top: the joined `top` paired vertex by vertex (bulges with them)
/// when it pairs, else the base mapped by the lean (same bulges: concentric arcs keep their
/// sweep).
pub fn top_bulge_ring(
    st: &Strip,
    bulge: f64,
    l: &Lean,
    base: &BulgeRing,
    top: Option<&BulgeRing>,
) -> BulgeRing {
    let bd = bend(st, bulge);
    let mapped: Vec<V2> = base.pts.iter().map(|p| l.map_in(st, bd, *p, st.height)).collect();
    let n = mapped.len();
    if let Some(t) = top {
        if let Some(r) = pair_shift(&mapped, &t.pts) {
            return BulgeRing {
                pts: (0..n).map(|j| t.pts[(j + r) % n]).collect(),
                bulges: (0..n).map(|j| t.bulges[(j + r) % n]).collect(),
            };
        }
    }
    BulgeRing {
        pts: mapped,
        bulges: base.bulges.clone(),
    }
}

/// Polygons of a base ring and its paired top ring with the same chords on every edge (the
/// finer of the two), so that vertex `j` of one pairs with vertex `j` of the other.
pub fn paired_polygons(base: &BulgeRing, top: &BulgeRing, sag: f64) -> (Vec<V2>, Vec<V2>) {
    let n = base.pts.len();
    let (mut b, mut t) = (vec![], vec![]);
    for i in 0..n {
        let (p, q, bb) = (base.pts[i], base.pts[(i + 1) % n], base.bulges[i]);
        let (pt, qt, tb) = (top.pts[i], top.pts[(i + 1) % n], top.bulges[i]);
        let k = edge_parts(p, q, bb, sag).max(edge_parts(pt, qt, tb, sag));
        for j in 0..k {
            b.push(edge_point(p, q, bb, j, k));
            t.push(edge_point(pt, qt, tb, j, k));
        }
    }
    (b, t)
}

/// Volume of the solid between `base` at z = 0 and the paired `top` at z = `h` (vertices
/// moving straight, each edge keeping a bulge between its two): Simpson on the exact areas,
/// exact because the section area is quadratic in z.
pub fn ring_volume(base: &BulgeRing, top: &BulgeRing, h: f64) -> f64 {
    let mid = BulgeRing {
        pts: base.pts.iter().zip(&top.pts).map(|(p, q)| (*p + *q) * 0.5).collect(),
        bulges: base
            .bulges
            .iter()
            .zip(&top.bulges)
            .map(|(a, b)| ((a.atan() + b.atan()) / 2.0).tan())
            .collect(),
    };
    h / 6.0 * (base.area() + 4.0 * mid.area() + top.area()).abs()
}

/// Volume of the ruled solid between `base` (at z = 0) and the paired `top` (at z = h).
pub fn loft_volume(base: &[V2], top: &[V2], h: f64) -> f64 {
    let mid: Vec<V2> = base.iter().zip(top).map(|(p, q)| (*p + *q) * 0.5).collect();
    h / 6.0 * (poly::area(base) + 4.0 * poly::area(&mid) + poly::area(top)).abs()
}

/// Closed mesh of the ruled solid between `base` at `zb` and the paired `top` at `zt`.
pub fn loft(base: &[V2], top: &[V2], zb: f64, zt: f64) -> Mesh {
    let n = base.len();
    let (b, t): (Vec<V2>, Vec<V2>) = if poly::area(base) < 0.0 {
        (base.iter().rev().copied().collect(), top.iter().rev().copied().collect())
    } else {
        (base.to_vec(), top.to_vec())
    };
    let mut m = Mesh::default();
    m.positions.extend(b.iter().map(|p| v3(p.x, p.y, zb)));
    m.positions.extend(t.iter().map(|p| v3(p.x, p.y, zt)));
    let n32 = n as u32;
    // caps: the top keeps the base's triangles (paired rings), seen from above; the bottom flips
    let tris = poly::triangulate(&b);
    for f in &tris {
        let [i, j, k] = f.map(|x| x as u32);
        m.triangles.push([i, k, j]);
        m.face_of.push(0);
        m.triangles.push([n32 + i, n32 + j, n32 + k]);
        m.face_of.push(1);
    }
    for j in 0..n32 {
        let k = (j + 1) % n32;
        m.triangles.push([j, k, n32 + k]);
        m.triangles.push([j, n32 + k, n32 + j]);
        m.face_of.extend([2 + j, 2 + j]);
    }
    m
}

/// Unit vectors of the axis surface in 3D where its plan normal is `n`: up the surface and its
/// normal (toward +n at z = 0).
fn surface_frame(n: V2, l: &Lean) -> (V3, V3) {
    let (sn, cs) = l.angle.sin_cos();
    (v3(n.x * sn, n.y * sn, cs), v3(n.x * cs, n.y * cs, -sn))
}

/// How far an opening cutter reaches across the wall, normal to the axis surface.
fn reach(st: &Strip, l: &Lean) -> f64 {
    let b = st.bounds();
    let far = b.iter().fold(0.0f64, |m, s| m.max(s.abs()));
    far * l.top_scale.max(1.0) + 0.05
}

/// Height above the base of the head line of an opening at the axis surface: `sill` is the
/// height of its sill line there, `height` its size along the leaning surface.
pub fn head_z(l: &Lean, sill: f64, height: f64) -> f64 {
    sill + height * l.angle.cos()
}

/// Opening `o` as a closed box normal to the axis surface, through the whole wall (`o.sill`
/// vertical at the axis surface, `o.height` along the surface). A sill at the base or a head
/// at the top is carried past the wall so the hole is open there. On an arc wall the box is
/// bent: its sections are the radial ones along the opening, chords within the arc sag.
pub fn opening_cutter(st: &Strip, bulge: f64, l: &Lean, o: &Opening) -> Mesh {
    let bd = bend(st, bulge);
    let ax = bd.and_then(|_| ArcAxis::new(st.a, st.b, bulge, Tol::default()).ok());
    let cs = l.angle.cos();
    let r = reach(st, l);
    let ext = (r * l.angle.tan().abs() + 0.05) / cs;
    let mut v0 = o.sill / cs;
    let mut v1 = v0 + o.height;
    if o.sill <= 1e-9 {
        v0 -= ext;
    }
    if head_z(l, o.sill, o.height) >= st.height - 1e-9 {
        v1 += ext;
    }
    // axis point and plan normal at `u` along the axis
    let foot = |u: f64| match ax {
        Some(a) => {
            let p = a.point_at(u);
            (p, frame_at(st, bd, p).1)
        }
        None => (st.a + st.dir() * u, st.normal()),
    };
    let parts = match ax {
        Some(a) => {
            let (p, q) = (a.point_at(o.along), a.point_at(o.along + o.width));
            let b = (a.sigma() * o.width / a.radius / 4.0).tan();
            edge_parts(p, q, b, CHORD_SAG)
        }
        None => 1,
    };
    let mut m = Mesh::default();
    for k in 0..=parts {
        let (p, n) = foot(o.along + o.width * k as f64 / parts as f64);
        let (up, nn) = surface_frame(n, l);
        for &v in &[v0, v1] {
            for &w in &[-r, r] {
                m.positions.push(v3(p.x, p.y, st.base) + up * v + nn * w);
            }
        }
    }
    // vertex index = 4k + 2v + w: the two end caps, then the four sides of every part
    let e = 4 * parts as u32;
    let mut quads: Vec<[u32; 4]> = vec![[0, 2, 3, 1], [e, e + 1, e + 3, e + 2]];
    for k in 0..parts as u32 {
        let (a, b) = (4 * k, 4 * k + 4);
        quads.extend([
            [a, a + 1, b + 1, b],
            [a + 2, b + 2, b + 3, a + 3],
            [a, b, b + 2, a + 2],
            [a + 1, a + 3, b + 3, b + 1],
        ]);
    }
    for (f, q) in quads.iter().enumerate() {
        m.triangles.push([q[0], q[1], q[2]]);
        m.triangles.push([q[0], q[2], q[3]]);
        m.face_of.extend([f as u32, f as u32]);
    }
    if m.volume() < 0.0 {
        for t in &mut m.triangles {
            t.swap(1, 2);
        }
    }
    m
}

/// Keep the part of `poly` where `(p - o) . m >= 0`.
fn clip_half(poly: &[V2], o: V2, m: V2) -> Vec<V2> {
    let mut out = vec![];
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (fp, fq) = ((p - o).dot(m), (q - o).dot(m));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp >= 0.0) != (fq >= 0.0) {
            out.push(p + (q - p) * (fp / (fp - fq)));
        }
    }
    out
}

/// Area of the cross-section of the layer between base offsets `s0..s1` removed by an opening
/// with sill `sill` and size `oh` along the surface (cut normal to the axis surface; open
/// below/above at the base/top).
pub fn cut_area(l: &Lean, h: f64, s0: f64, s1: f64, sill: f64, oh: f64) -> f64 {
    let p = cut_section(l, h, s0, s1, sill, oh);
    if p.len() < 3 {
        return 0.0;
    }
    poly::area(&p).abs()
}

/// The removed part of the layer's cross-section as a polygon (x = offset, y = z).
fn cut_section(l: &Lean, h: f64, s0: f64, s1: f64, sill: f64, oh: f64) -> Vec<V2> {
    let (z0, z1) = (sill, head_z(l, sill, oh));
    // (x = offset, y = z)
    let sec = vec![
        v2(s0, 0.0),
        v2(s1, 0.0),
        v2(l.offset_at(s1, h, h), h),
        v2(l.offset_at(s0, h, h), h),
    ];
    let (sn, cs) = l.angle.sin_cos();
    let up = v2(sn, cs);
    let mut p = sec;
    if z0 > 1e-9 {
        p = clip_half(&p, v2(l.shift(z0), z0), up);
    }
    if z1 < h - 1e-9 {
        p = clip_half(&p, v2(l.shift(z1), z1), up * -1.0);
    }
    p
}

/// Volume removed from layer `s0..s1` by the openings. On an arc wall (`arc`: axis radius and
/// sense) an opening spans the angle width / r and removes its section turned about the
/// centre: angle times the section's moment of the radius `r - sg x` (Pappus).
pub fn openings_volume(
    l: &Lean,
    h: f64,
    s0: f64,
    s1: f64,
    ops: &[Opening],
    arc: Option<(f64, f64)>,
) -> f64 {
    ops.iter()
        .map(|o| {
            let p = cut_section(l, h, s0, s1, o.sill, o.height);
            if p.len() < 3 {
                return 0.0;
            }
            let a = poly::area(&p).abs();
            match arc {
                None => o.width * a,
                Some((r, sg)) => o.width / r * a * (r - sg * poly::centroid(&p).x),
            }
        })
        .sum()
}

/// Axis radius and sense of an arc wall, for [`openings_volume`]; None when straight.
pub fn arc_of(st: &Strip, bulge: f64) -> Option<(f64, f64)> {
    bend(st, bulge).map(|b| (b.r, b.sg))
}

/// Base footprint ring mapped to the top of the wall (no join at the top).
pub fn mapped_ring(st: &Strip, bulge: f64, l: &Lean, ring: &[V2]) -> Vec<V2> {
    let bd = bend(st, bulge);
    ring.iter().map(|p| l.map_in(st, bd, *p, st.height)).collect()
}

/// The top ring paired with `base`: the joined top ring when it pairs, else the mapped base.
pub fn top_ring(
    st: &Strip,
    bulge: f64,
    l: &Lean,
    base: &[V2],
    joined_top: Option<&[V2]>,
) -> Vec<V2> {
    let mapped = mapped_ring(st, bulge, l, base);
    joined_top
        .and_then(|t| pair_rings(&mapped, t))
        .unwrap_or(mapped)
}

/// The paired top equals the mapped base: the piece is an exact (tapered) extrusion.
pub fn is_mapped(st: &Strip, bulge: f64, l: &Lean, base: &[V2], top: &[V2], tol: f64) -> bool {
    let bd = bend(st, bulge);
    base.len() == top.len()
        && base
            .iter()
            .zip(top)
            .all(|(p, q)| (l.map_in(st, bd, *p, st.height) - *q).len() <= tol)
}

/// Point `p` of an element drawn in the vertical wall (door or window fill) turned with the
/// opening: rotated about the wall's horizontal line at height `zc` above the base (the
/// opening's sill), so its vertical becomes the surface's up direction, as the normal cut is.
/// On an arc wall each point turns in its radial plane.
pub fn tilt(st: &Strip, bulge: f64, l: &Lean, p: V3, zc: f64) -> V3 {
    let xy = v2(p.x, p.y);
    let (s, n) = frame_at(st, bend(st, bulge), xy);
    let z = p.z - st.base - zc;
    let (sn, cs) = l.angle.sin_cos();
    let s2 = s * cs + z * sn + l.shift(zc);
    let z2 = zc - s * sn + z * cs;
    let q = xy + n * (s2 - s);
    v3(q.x, q.y, st.base + z2)
}

/// Layer meshes of a leaning wall: each base footprint piece lofted to its top piece (`top`,
/// the joined top footprints; mapped base where they do not pair), minus the openings cut
/// normal to the wall. (layer index, mesh) in layer order.
pub fn layer_meshes(
    st: &Strip,
    l: &Lean,
    base: &[Vec<crate::Profile>],
    top: Option<&[Vec<crate::Profile>]>,
    ops: &[Opening],
    tol: crate::Tol,
) -> Vec<(usize, Mesh)> {
    let mut pieces = vec![];
    for (li, ps) in base.iter().enumerate() {
        let tl = top.and_then(|t| t.get(li)).filter(|t| t.len() == ps.len());
        for (k, p) in ps.iter().enumerate() {
            let t = top_ring(st, 0.0, l, &p.outer, tl.map(|t| &t[k].outer[..]));
            pieces.push((li, p.outer.clone(), t));
        }
    }
    lofted_meshes(st, 0.0, l, &pieces, ops, tol)
}

/// Layer meshes of a leaning wall from its footprint rings with arc edges (`top`: the joined
/// top rings), each base ring paired with its top chord for chord. As [`layer_meshes`].
pub fn ring_meshes(
    st: &Strip,
    bulge: f64,
    l: &Lean,
    base: &[Vec<BulgeRing>],
    top: Option<&[Vec<BulgeRing>]>,
    ops: &[Opening],
    tol: crate::Tol,
) -> Vec<(usize, Mesh)> {
    let mut pieces = vec![];
    for (li, rings) in base.iter().enumerate() {
        let tl = top.and_then(|t| t.get(li)).filter(|t| t.len() == rings.len());
        for (k, r) in rings.iter().enumerate() {
            let t = top_bulge_ring(st, bulge, l, r, tl.map(|t| &t[k]));
            let (b, t) = paired_polygons(r, &t, CHORD_SAG);
            pieces.push((li, b, t));
        }
    }
    lofted_meshes(st, bulge, l, &pieces, ops, tol)
}

/// Each (layer, base, paired top) lofted, minus the openings' cutters.
fn lofted_meshes(
    st: &Strip,
    bulge: f64,
    l: &Lean,
    pieces: &[(usize, Vec<V2>, Vec<V2>)],
    ops: &[Opening],
    tol: crate::Tol,
) -> Vec<(usize, Mesh)> {
    use crate::csg::Body;
    let cutters: Vec<Body> = ops
        .iter()
        .map(|o| Body::from_mesh(&opening_cutter(st, bulge, l, o), 1))
        .collect();
    let mut out = vec![];
    for (li, b, t) in pieces {
        let m = loft(b, t, st.base, st.base + st.height);
        if cutters.is_empty() {
            out.push((*li, m));
            continue;
        }
        let mut body = Body::from_mesh(&m, 0);
        for c in &cutters {
            body = body.subtract(c, tol);
        }
        let cut = body.to_mesh();
        out.push((*li, if cut.triangles.is_empty() { m } else { cut }));
    }
    out
}

#[cfg(test)]
#[path = "wall_lean_tests.rs"]
mod tests;
