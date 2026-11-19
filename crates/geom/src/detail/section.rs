// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Columns and beams drawn with their true section (I, channel, angle, tee, round, rectangular
//! and circular hollow) where the exact joined body is too costly: a straight bar, round parts
//! with as many sides as the level of detail asks for, hollow sections open at the ends.

use super::prim::{prism, across};
use super::{round_sides, Lod};
use crate::math::{v2, V2, V3};
use crate::member::Shape;
use crate::mesh::Mesh;

/// Outer ring and hole of a section at a level of detail; x across, y up.
pub fn rings(s: &Shape, lod: Lod) -> (Vec<V2>, Option<Vec<V2>>) {
    let n = round_sides(lod, false).max(8) + if lod == Lod::Fine { 8 } else { 0 };
    let circle = |r: f64| super::prim::ngon(r, n);
    match *s {
        Shape::Circle { d } => (circle(d / 2.0), None),
        Shape::Pipe { d, t } => {
            let inner = (d / 2.0 - t).max(d * 0.05);
            let hole = (lod != Lod::Coarse).then(|| circle(inner));
            (circle(d / 2.0), hole)
        }
        Shape::Tube { h, b, .. } if lod == Lod::Coarse => (rect(b, h), None),
        _ => s.rings(),
    }
}

fn rect(b: f64, h: f64) -> Vec<V2> {
    vec![v2(-b / 2.0, -h / 2.0), v2(b / 2.0, -h / 2.0), v2(b / 2.0, h / 2.0), v2(-b / 2.0, h / 2.0)]
}

/// Bar from `a` to `b` with the section's y along `up` (projected square to the axis).
pub fn bar(m: &mut Mesh, s: &Shape, a: V3, b: V3, up: V3, lod: Lod) {
    let d = b - a;
    if d.len() < 1e-9 {
        return;
    }
    let dn = d.normalized();
    let w = up - dn * up.dot(dn);
    let (eu, ev) = if w.len() < 1e-6 {
        across(dn)
    } else {
        let v = w.normalized();
        (v.cross(dn), v)
    };
    let (outer, hole) = rings(s, lod);
    match hole {
        Some(h) if h.len() == outer.len() => hollow(m, &outer, &h, a, eu, ev, d),
        _ => prism(m, &outer, a, eu, ev, d),
    }
}

/// Hollow bar: outer and inner walls, ring-shaped ends (rings of equal vertex count).
fn hollow(m: &mut Mesh, outer: &[V2], inner: &[V2], o: V3, eu: V3, ev: V3, d: V3) {
    let ccw = |r: &[V2]| {
        let mut v = r.to_vec();
        if crate::profile::signed_area(&v) < 0.0 {
            v.reverse();
        }
        v
    };
    let (po, pi) = (ccw(outer), ccw(inner));
    let k = po.len();
    let at = |q: V2, w: f64| o + eu * q.x + ev * q.y + d * w;
    let base = m.positions.len() as u32;
    // vertex blocks: outer@0, outer@1, inner@0, inner@1
    for (r, w) in [(&po, 0.0), (&po, 1.0), (&pi, 0.0), (&pi, 1.0)] {
        m.positions.extend(r.iter().map(|q| at(*q, w)));
    }
    let id = |blk: usize, i: usize| base + (blk * k + i % k) as u32;
    let mut tris = vec![];
    for i in 0..k {
        let j = i + 1;
        // outer wall faces out, inner wall faces the hole
        tris.push([id(0, i), id(0, j), id(1, j)]);
        tris.push([id(0, i), id(1, j), id(1, i)]);
        tris.push([id(2, i), id(3, j), id(2, j)]);
        tris.push([id(2, i), id(3, i), id(3, j)]);
        // end rings between the walls
        tris.push([id(0, i), id(2, j), id(0, j)]);
        tris.push([id(0, i), id(2, i), id(2, j)]);
        tris.push([id(1, i), id(1, j), id(3, j)]);
        tris.push([id(1, i), id(3, j), id(3, i)]);
    }
    // the winding above assumes eu × ev along d; flip all when it is not
    let flip = eu.cross(ev).dot(d) < 0.0;
    let face = m.face_of.last().map_or(0, |f| f + 1);
    for mut t in tris {
        if flip {
            t.swap(1, 2);
        }
        m.triangles.push(t);
        m.face_of.push(face);
    }
}
