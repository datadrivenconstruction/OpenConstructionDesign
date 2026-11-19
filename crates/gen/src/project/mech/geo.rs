// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Boxes, points and the structure the services keep clear of: columns, slabs and beams.

use super::super::{svc, GProject};

pub type P3 = (f64, f64, f64);
/// Axis-aligned box `[x0, y0, z0, x1, y1, z1]`.
pub type Bx = [f64; 6];

/// A downstand beam: axis in plan, underside and top above the project base, width.
#[derive(Clone, Debug, PartialEq)]
pub struct Beam {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub bottom: f64,
    pub top: f64,
    pub width: f64,
}

impl Beam {
    pub fn bbox(&self) -> Bx {
        let h = self.width / 2.0;
        [
            self.a.0.min(self.b.0) - h,
            self.a.1.min(self.b.1) - h,
            self.bottom,
            self.a.0.max(self.b.0) + h,
            self.a.1.max(self.b.1) + h,
            self.top,
        ]
    }
}

pub fn overlap(a: &Bx, b: &Bx, gap: f64) -> bool {
    a[0] < b[3] + gap && b[0] < a[3] + gap && a[1] < b[4] + gap && b[1] < a[4] + gap && a[2] < b[5] + gap && b[2] < a[5] + gap
}

pub fn grow(b: &Bx, d: f64) -> Bx {
    [b[0] - d, b[1] - d, b[2] - d, b[3] + d, b[4] + d, b[5] + d]
}

/// Box of a straight piece from `a` to `b` with a cross-section of half width `hw` across
/// the run and half height `hh` (vertical runs take the larger half on both plan axes).
pub fn seg_box(a: P3, b: P3, hw: f64, hh: f64) -> Bx {
    let (dx, dy, dz) = ((b.0 - a.0).abs(), (b.1 - a.1).abs(), (b.2 - a.2).abs());
    let (ex, ey, ez) = if dz >= dx.max(dy) {
        let m = hw.max(hh);
        (m, m, 0.0)
    } else if dx >= dy {
        (0.0, hw, hh)
    } else {
        (hw, 0.0, hh)
    };
    [a.0.min(b.0) - ex, a.1.min(b.1) - ey, a.2.min(b.2) - ez, a.0.max(b.0) + ex, a.1.max(b.1) + ey, a.2.max(b.2) + ez]
}

pub fn dist(a: P3, b: P3) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2) + (b.2 - a.2).powi(2)).sqrt()
}

pub fn len(p: &[P3]) -> f64 {
    p.windows(2).map(|w| dist(w[0], w[1])).sum()
}

pub fn unit(a: P3, b: P3) -> P3 {
    let l = dist(a, b).max(1e-12);
    ((b.0 - a.0) / l, (b.1 - a.1) / l, (b.2 - a.2) / l)
}

pub fn dot(a: P3, b: P3) -> f64 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

/// Turn angle, degrees, between two directions.
pub fn turn(a: P3, b: P3) -> f64 {
    dot(a, b).clamp(-1.0, 1.0).acos().to_degrees()
}

pub fn area(p: &[(f64, f64)]) -> f64 {
    let n = p.len();
    (0..n).map(|i| p[i].0 * p[(i + 1) % n].1 - p[(i + 1) % n].0 * p[i].1).sum::<f64>().abs() / 2.0
}

pub fn centroid(p: &[(f64, f64)]) -> (f64, f64) {
    let n = p.len().max(1) as f64;
    (p.iter().map(|q| q.0).sum::<f64>() / n, p.iter().map(|q| q.1).sum::<f64>() / n)
}

/// Plan box `[x0, y0, x1, y1]`.
pub fn bbox2(p: &[(f64, f64)]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for q in p {
        b = [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)];
    }
    b
}

pub fn inside(p: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) * (b.0 - a.0) / (b.1 - a.1) {
            c = !c;
        }
    }
    c
}

/// Underside of the slab over storey `k`, above the project base.
pub fn soffit(p: &GProject, k: usize) -> f64 {
    let st = &p.storeys[k];
    st.elevation + st.height - svc::slab()
}

/// Ceiling plane of storey `k` above the project base: the lowest air terminal face, else the
/// clear height less the ceiling void.
pub fn ceiling(p: &GProject, k: usize) -> f64 {
    p.storeys[k].elevation + svc::ceiling(p, k)
}

/// One obstacle of the structure with what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Solid {
    pub b: Bx,
    pub what: String,
}

/// Columns of every storey, the slabs as boxes between their holes' outlines (kept separate:
/// a riser may pass a slab only inside a shaft), and the beams.
pub fn structure(p: &GProject, beams: &[Beam]) -> Vec<Solid> {
    let mut out = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        let e = st.elevation;
        for c in &st.layout.columns {
            let (hx, hy) = (c.section.0 / 2.0, c.section.1 / 2.0);
            out.push(Solid {
                b: [c.at.0 - hx, c.at.1 - hy, e, c.at.0 + hx, c.at.1 + hy, e + st.height - svc::slab()],
                what: format!("column of storey {k}"),
            });
        }
    }
    for (i, b) in beams.iter().enumerate() {
        out.push(Solid { b: b.bbox(), what: format!("beam {i}") });
    }
    out
}

/// Whether the vertical piece at plan point `q` may pass the slab at elevation `z`: inside a
/// shaft ring of the storey at that level or a slab hole.
pub fn slab_passable(p: &GProject, q: (f64, f64), z: f64) -> bool {
    p.storeys.iter().any(|st| {
        (st.elevation - z).abs() < 1e-3
            && (st.layout.shafts.iter().any(|s| inside(q, &s.ring)) || st.layout.slab_holes.iter().any(|h| inside(q, h)))
    })
}
