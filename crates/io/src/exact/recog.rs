// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! An imported body as our own solid types where it maps: a prism -> extrusion, a coaxial body
//! of revolution -> revolve, a planar polyhedron -> mesh, NURBS sheets -> NURBS surfaces.

use std::f64::consts::TAU;

use ddc_geom::arc::BulgeRing;
use ddc_geom::gsolid::{self, Form, Frame, Section};
use ddc_geom::math::{v2, V2, V3};
use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs;

use super::build::Seg;
use super::{perp, Ax3, Body, Curve, Surface};

#[derive(Clone, Debug)]
pub enum Mapped {
    /// `Form::Extrusion` in its working plane.
    Extrusion { form: Form, frame: Frame },
    /// `Form::Revolve` in its working plane.
    Revolve { form: Form, frame: Frame },
    /// Closed planar polyhedron, triangulated.
    Polyhedron(Mesh),
    /// Open NURBS sheets.
    Sheets(Vec<nurbs::Surface>),
    /// No mapping; the reason. Such bodies stay on the mesh path.
    Unmapped(String),
}

impl Mapped {
    pub fn volume(&self) -> Option<f64> {
        match self {
            Mapped::Extrusion { form, .. } | Mapped::Revolve { form, .. } => gsolid::quantities(form).ok().map(|q| q.volume),
            Mapped::Polyhedron(m) => Some(m.volume().abs()),
            _ => None,
        }
    }
}

pub fn recognize(b: &Body) -> Mapped {
    if b.faces.is_empty() {
        return Mapped::Unmapped("no faces".into());
    }
    if !b.closed {
        let s: Vec<nurbs::Surface> = b
            .faces
            .iter()
            .filter_map(|f| match &f.surface {
                Surface::Nurbs(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        return if s.len() == b.faces.len() {
            Mapped::Sheets(s)
        } else {
            Mapped::Unmapped("an open body with analytic faces".into())
        };
    }
    let size = b.bbox().map_or(1.0, |(lo, hi)| (hi - lo).len());
    let tol = 1e-7 * (1.0 + size);
    if let Some(m) = extrusion(b, tol) {
        return m;
    }
    if let Some(m) = revolution(b, tol) {
        return m;
    }
    if b.faces.iter().all(|f| matches!(f.surface, Surface::Plane(_))) {
        if let Some(m) = polyhedron(b, tol) {
            return Mapped::Polyhedron(m);
        }
    }
    Mapped::Unmapped("faces fit neither a prism, a body of revolution nor a polyhedron".into())
}

fn outward(s: &Surface, sense: bool) -> Option<V3> {
    match s {
        Surface::Plane(a) => Some(if sense { a.z } else { -a.z }),
        _ => None,
    }
}

fn par(a: V3, b: V3) -> bool {
    a.cross(b).len() <= 1e-9
}

/// Start point and bulge of each piece of a loop edge, in the 2D frame (`o`, `ex`, `ey`);
/// a closed circle gives two half circles. `None` for curves that are not lines or circles
/// in that plane.
fn pieces(b: &Body, e: u32, fwd: bool, o: V3, ex: V3, ey: V3, tol: f64) -> Option<Vec<(V2, f64)>> {
    let ed = &b.edges[e as usize];
    let [s, t] = if fwd { ed.v } else { [ed.v[1], ed.v[0]] };
    let (pa, pb) = (b.verts[s as usize], b.verts[t as usize]);
    let to2 = |p: V3| v2((p - o).dot(ex), (p - o).dot(ey));
    let n = ex.cross(ey);
    match &ed.curve {
        Curve::Line => {
            if ((pa - o).dot(n)).abs() > tol || ((pb - o).dot(n)).abs() > tol {
                return None;
            }
            Some(vec![(to2(pa), 0.0)])
        }
        Curve::Circle { ax, .. } => {
            if !par(ax.z, n) || ((ax.o - o).dot(n)).abs() > tol {
                return None;
            }
            let ccw = (ed.sense == fwd) == (ax.z.dot(n) > 0.0);
            let sign = if ccw { 1.0 } else { -1.0 };
            if s == t {
                let opp = ax.o * 2.0 - pa;
                return Some(vec![(to2(pa), sign), (to2(opp), sign)]);
            }
            let ang = |p: V3| {
                let d = p - ax.o;
                d.dot(ax.y()).atan2(d.dot(ax.x))
            };
            // travel along the curve direction from the start to the end vertex
            let (from, to) = if ed.sense == fwd { (ang(pa), ang(pb)) } else { (ang(pb), ang(pa)) };
            let sweep = (to - from).rem_euclid(TAU);
            let sweep = if sweep <= 1e-12 { TAU } else { sweep };
            Some(vec![(to2(pa), sign * (sweep / 4.0).tan())])
        }
        _ => None,
    }
}

fn ring_of(b: &Body, lp: &[(u32, bool)], o: V3, ex: V3, ey: V3, tol: f64) -> Option<BulgeRing> {
    let mut r = BulgeRing { pts: vec![], bulges: vec![] };
    for &(e, fwd) in lp {
        for (p, bg) in pieces(b, e, fwd, o, ex, ey, tol)? {
            r.pts.push(p);
            r.bulges.push(bg);
        }
    }
    (r.pts.len() >= 2).then_some(r)
}

/// Section from loops: the loop of largest area is the outer one.
fn section(mut rings: Vec<BulgeRing>) -> Option<Section> {
    if rings.is_empty() {
        return None;
    }
    let k = (0..rings.len()).max_by(|&i, &j| rings[i].area().abs().total_cmp(&rings[j].area().abs()))?;
    let outer = rings.swap_remove(k);
    Some(Section { outer, holes: rings })
}

fn frame(o: V3, x: V3, y: V3) -> Frame {
    Frame { o, x, y, z: x.cross(y) }
}

fn extrusion(b: &Body, tol: f64) -> Option<Mapped> {
    let planes: Vec<(usize, V3, &Ax3)> = b
        .faces
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match &f.surface {
            Surface::Plane(a) => Some((i, outward(&f.surface, f.sense)?, a)),
            _ => None,
        })
        .collect();
    if planes.len() > 4096 {
        return None;
    }
    for &(bi, bn, ba) in &planes {
        for &(ti, tn, ta) in &planes {
            if bn.dot(tn) > -1.0 + 1e-9 {
                continue;
            }
            let n = tn;
            let h = (ta.o - ba.o).dot(n);
            if h <= tol {
                continue;
            }
            let sides_ok = b.faces.iter().enumerate().all(|(i, f)| {
                i == bi
                    || i == ti
                    || match &f.surface {
                        Surface::Plane(a) => a.z.dot(n).abs() <= 1e-9,
                        Surface::Cylinder { ax, .. } => par(ax.z, n),
                        _ => false,
                    }
            });
            if !sides_ok {
                continue;
            }
            let on = |p: V3, a: &Ax3| ((p - a.o).dot(n)).abs() <= tol;
            let edges_ok = b.edges.iter().all(|e| {
                let (p, q) = (b.verts[e.v[0] as usize], b.verts[e.v[1] as usize]);
                (on(p, ba) && on(q, ba)) || (on(p, ta) && on(q, ta)) || (matches!(e.curve, Curve::Line) && par(q - p, n))
            });
            if !edges_ok || b.faces[bi].loops.len() != b.faces[ti].loops.len() {
                continue;
            }
            let x = ba.x - n * ba.x.dot(n);
            let x = if x.len() > 1e-9 { x.normalized() } else { perp(n) };
            let y = n.cross(x);
            let rings = b.faces[bi]
                .loops
                .iter()
                .map(|l| ring_of(b, l, ba.o, x, y, tol))
                .collect::<Option<Vec<_>>>()?;
            let form = Form::Extrusion {
                profile: section(rings)?,
                start: 0.0,
                end: h,
            };
            gsolid::validate(&form).ok()?;
            return Some(Mapped::Extrusion {
                form,
                frame: frame(ba.o, x, y),
            });
        }
    }
    None
}

/// Axis of a face of revolution.
fn face_axis(s: &Surface) -> Option<(V3, V3)> {
    match s {
        Surface::Cylinder { ax, .. } | Surface::Cone { ax, .. } | Surface::Torus { ax, .. } | Surface::Sphere { ax, .. } => {
            Some((ax.o, ax.z))
        }
        _ => None,
    }
}

fn revolution(b: &Body, tol: f64) -> Option<Mapped> {
    let (o, ad) = b
        .faces
        .iter()
        .filter(|f| !matches!(f.surface, Surface::Sphere { .. }))
        .find_map(|f| face_axis(&f.surface))
        .or_else(|| b.faces.iter().find_map(|f| face_axis(&f.surface)))?;
    let on_axis = |p: V3| {
        let d = p - o;
        (d - ad * d.dot(ad)).len() <= tol
    };
    let mut meridian = vec![];
    for (i, f) in b.faces.iter().enumerate() {
        let ok = match &f.surface {
            Surface::Cylinder { ax, .. } | Surface::Cone { ax, .. } | Surface::Torus { ax, .. } => par(ax.z, ad) && on_axis(ax.o),
            Surface::Sphere { ax, .. } => on_axis(ax.o),
            Surface::Plane(a) if par(a.z, ad) => true,
            Surface::Plane(a) if a.z.dot(ad).abs() <= 1e-9 && ((o - a.o).dot(a.z)).abs() <= tol => {
                meridian.push((i, outward(&f.surface, f.sense)?));
                true
            }
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    let x_hint = b.faces.iter().find_map(|f| match &f.surface {
        Surface::Cylinder { ax, .. } | Surface::Cone { ax, .. } | Surface::Torus { ax, .. } | Surface::Sphere { ax, .. } => {
            Some(ax.x)
        }
        _ => None,
    })?;
    match meridian.len() {
        0 => full_turn(b, o, ad, x_hint, tol),
        2 => part_turn(b, o, ad, &meridian, tol),
        _ => None,
    }
}

fn part_turn(b: &Body, o: V3, ad: V3, mer: &[(usize, V3)], tol: f64) -> Option<Mapped> {
    for ad in [ad, -ad] {
        for k in 0..2 {
            let (fs, ns) = mer[k];
            let (_, ne) = mer[1 - k];
            let e_phi = -ns;
            let x0 = e_phi.cross(ad);
            let pts: Vec<f64> = b.faces[fs]
                .loops
                .iter()
                .flatten()
                .flat_map(|&(e, _)| b.edges[e as usize].v)
                .map(|v| (b.verts[v as usize] - o).dot(x0))
                .collect();
            let lo = pts.iter().copied().fold(f64::MAX, f64::min);
            let hi = pts.iter().copied().fold(f64::MIN, f64::max);
            if lo < -tol || hi <= tol {
                continue;
            }
            let y0 = ad.cross(x0);
            let da = (-ne.dot(x0)).atan2(ne.dot(y0)).rem_euclid(TAU);
            let da = if da <= 1e-12 { TAU } else { da };
            let rings = b.faces[fs]
                .loops
                .iter()
                .map(|l| ring_of(b, l, o, ad, x0, tol))
                .collect::<Option<Vec<_>>>()?;
            let form = Form::Revolve {
                profile: section(rings)?,
                axis_o: v2(0.0, 0.0),
                axis_d: v2(1.0, 0.0),
                start: 0.0,
                end: da,
            };
            gsolid::validate(&form).ok()?;
            return Some(Mapped::Revolve {
                form,
                frame: frame(o, ad, x0),
            });
        }
    }
    None
}

fn full_turn(b: &Body, o: V3, ad: V3, x_hint: V3, tol: f64) -> Option<Mapped> {
    let hr = |p: V3| {
        let d = p - o;
        let h = d.dot(ad);
        v2(h, (d - ad * h).len())
    };
    let mut segs: Vec<Seg> = vec![];
    for f in &b.faces {
        match &f.surface {
            Surface::Plane(_) => {
                // a disc or an annulus between coaxial circles
                let mut radii = vec![];
                let mut h = None;
                for l in &f.loops {
                    let [(e, _)] = l.as_slice() else { return None };
                    let ed = &b.edges[*e as usize];
                    let Curve::Circle { ax, r } = &ed.curve else { return None };
                    if ed.v[0] != ed.v[1] || !par(ax.z, ad) {
                        return None;
                    }
                    radii.push(*r);
                    h = Some(hr(ax.o).x);
                }
                let h = h?;
                radii.sort_by(f64::total_cmp);
                let (lo, hi) = match radii.as_slice() {
                    [r] => (0.0, *r),
                    [a, c] => (*a, *c),
                    _ => return None,
                };
                segs.push(Seg {
                    a: v2(h, lo),
                    b: v2(h, hi),
                    bulge: 0.0,
                });
            }
            _ => {
                // the seam: the edge used twice by the face
                let all: Vec<u32> = f.loops.iter().flatten().map(|x| x.0).collect();
                let coaxial = |e: u32| match &b.edges[e as usize].curve {
                    Curve::Circle { ax, .. } => par(ax.z, ad),
                    _ => false,
                };
                let seam = all
                    .iter()
                    .copied()
                    .find(|&e| !coaxial(e) && all.iter().filter(|&&x| x == e).count() == 2)?;
                let ed = &b.edges[seam as usize];
                let (pa, pb) = (b.verts[ed.v[0] as usize], b.verts[ed.v[1] as usize]);
                match &ed.curve {
                    Curve::Line => segs.push(Seg {
                        a: hr(pa),
                        b: hr(pb),
                        bulge: 0.0,
                    }),
                    Curve::Circle { ax, r } => {
                        // points along the travel; (h, rho) is isometric in the seam's half-plane
                        let ang = |p: V3| {
                            let d = p - ax.o;
                            d.dot(ax.y()).atan2(d.dot(ax.x))
                        };
                        let a0 = ang(pa);
                        let sweep = (ang(pb) - a0).rem_euclid(TAU);
                        let sweep = if ed.v[0] == ed.v[1] || sweep <= 1e-12 { TAU } else { sweep };
                        let sgn = if ed.sense { 1.0 } else { -1.0 };
                        let sweep = if ed.sense { sweep } else { TAU - sweep % TAU };
                        let at = |f: f64| {
                            let t = a0 + sgn * sweep * f;
                            hr(ax.o + (ax.x * t.cos() + ax.y() * t.sin()) * *r)
                        };
                        if ed.v[0] == ed.v[1] {
                            let (p0, p1, p2, p3) = (at(0.0), at(0.25), at(0.5), at(0.75));
                            segs.push(Seg {
                                a: p0,
                                b: p2,
                                bulge: Seg::bulge_through(p0, p1, p2),
                            });
                            segs.push(Seg {
                                a: p2,
                                b: p0,
                                bulge: Seg::bulge_through(p2, p3, p0),
                            });
                        } else {
                            let (p0, pm, p1) = (hr(pa), at(0.5), hr(pb));
                            segs.push(Seg {
                                a: p0,
                                b: p1,
                                bulge: Seg::bulge_through(p0, pm, p1),
                            });
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    let rings = chain(segs, tol)?;
    let x0 = x_hint - ad * x_hint.dot(ad);
    let x0 = if x0.len() > 1e-9 { x0.normalized() } else { perp(ad) };
    let form = Form::Revolve {
        profile: section(rings)?,
        axis_o: v2(0.0, 0.0),
        axis_d: v2(1.0, 0.0),
        start: 0.0,
        end: TAU,
    };
    gsolid::validate(&form).ok()?;
    Some(Mapped::Revolve {
        form,
        frame: frame(o, ad, x0),
    })
}

/// Meridian segments joined end to end into rings; open chains whose ends lie on the axis
/// are closed along it.
fn chain(mut left: Vec<Seg>, tol: f64) -> Option<Vec<BulgeRing>> {
    let mut rings = vec![];
    while let Some(first) = left.pop() {
        let mut ring = vec![first];
        loop {
            let (start, end) = (ring[0].a, ring[ring.len() - 1].b);
            if ring.len() >= 2 && (end - start).len() <= tol {
                break;
            }
            if let Some(k) = left.iter().position(|s| (s.a - end).len() <= tol || (s.b - end).len() <= tol) {
                let s = left.swap_remove(k);
                ring.push(if (s.a - end).len() <= tol {
                    s
                } else {
                    Seg {
                        a: s.b,
                        b: s.a,
                        bulge: -s.bulge,
                    }
                });
            } else if let Some(k) = left.iter().position(|s| (s.a - start).len() <= tol || (s.b - start).len() <= tol) {
                // grow the chain backwards
                let s = left.swap_remove(k);
                ring.insert(
                    0,
                    if (s.b - start).len() <= tol {
                        s
                    } else {
                        Seg {
                            a: s.b,
                            b: s.a,
                            bulge: -s.bulge,
                        }
                    },
                );
            } else if end.y.abs() <= tol && start.y.abs() <= tol && (end - start).len() > tol {
                ring.push(Seg {
                    a: end,
                    b: start,
                    bulge: 0.0,
                });
                break;
            } else {
                return None;
            }
        }
        rings.push(BulgeRing {
            pts: ring.iter().map(|s| s.a).collect(),
            bulges: ring.iter().map(|s| s.bulge).collect(),
        });
    }
    Some(rings)
}

/// Triangles of a closed body of planar faces bounded by straight edges.
fn polyhedron(b: &Body, tol: f64) -> Option<Mesh> {
    let mut m = Mesh {
        positions: vec![],
        triangles: vec![],
        face_of: vec![],
    };
    for (fi, f) in b.faces.iter().enumerate() {
        let Surface::Plane(a) = &f.surface else { return None };
        let n = outward(&f.surface, f.sense)?;
        let (x, y) = (a.x, n.cross(a.x));
        let mut rings: Vec<Vec<V2>> = vec![];
        let mut pts3: Vec<Vec<V3>> = vec![];
        for l in &f.loops {
            let mut r = vec![];
            let mut p3 = vec![];
            for &(e, fwd) in l {
                let ed = &b.edges[e as usize];
                if !matches!(ed.curve, Curve::Line) {
                    return None;
                }
                let v = if fwd { ed.v[0] } else { ed.v[1] };
                let p = b.verts[v as usize];
                if ((p - a.o).dot(n)).abs() > tol {
                    return None;
                }
                r.push(v2((p - a.o).dot(x), (p - a.o).dot(y)));
                p3.push(p);
            }
            rings.push(r);
            pts3.push(p3);
        }
        let area = |r: &[V2]| (0..r.len()).map(|i| r[i].cross(r[(i + 1) % r.len()])).sum::<f64>() / 2.0;
        let k = (0..rings.len()).max_by(|&i, &j| area(&rings[i]).abs().total_cmp(&area(&rings[j]).abs()))?;
        let mut order: Vec<usize> = vec![k];
        order.extend((0..rings.len()).filter(|&i| i != k));
        let mut flat = vec![];
        let mut holes = vec![];
        let mut base = vec![];
        for (q, &i) in order.iter().enumerate() {
            let (mut r, mut p) = (rings[i].clone(), pts3[i].clone());
            let want_ccw = q == 0;
            if (area(&r) > 0.0) != want_ccw {
                r.reverse();
                p.reverse();
            }
            if q == 0 {
                flat = r;
            } else {
                holes.push(r);
            }
            base.extend(p);
        }
        let start = m.positions.len() as u32;
        for t in gsolid::cap(&flat, &holes) {
            m.triangles.push(t.map(|i| start + i as u32));
            m.face_of.push(fi as u32);
        }
        m.positions.extend(base);
    }
    (!m.triangles.is_empty()).then_some(m)
}
