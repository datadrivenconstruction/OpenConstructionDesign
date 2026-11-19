// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bodies from our solid types: extrusion and revolution of bulge profiles with exact faces,
//! any triangle mesh as a planar polyhedron (forms without an exact mapping).

use std::collections::HashMap;
use std::f64::consts::TAU;

use ddc_geom::arc::{BulgeRing, MIN_BULGE};
use ddc_geom::gsolid::{self, Form, Frame, Section};
use ddc_geom::math::{v2, v3, V2, V3};
use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs;

use super::{Ax3, Body, Curve, Edge, Face, Surface, Xf};

/// One profile edge: a line, or an arc when `bulge` is not zero (tan of a quarter of the
/// signed sweep, counter-clockwise > 0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: V2,
    pub b: V2,
    pub bulge: f64,
}

impl Seg {
    /// Centre, radius and signed sweep of an arc edge.
    pub fn arc(&self) -> Option<(V2, f64, f64)> {
        let ch = self.b - self.a;
        let c = ch.len();
        let bg = self.bulge;
        if bg.abs() < MIN_BULGE || !bg.is_finite() || c <= 0.0 {
            return None;
        }
        let r = c * (1.0 + bg * bg) / (4.0 * bg.abs());
        let d = c * (1.0 - bg * bg) / (4.0 * bg);
        let u = ch * (1.0 / c);
        let centre = (self.a + self.b) * 0.5 + v2(-u.y, u.x) * d;
        Some((centre, r, 4.0 * bg.atan()))
    }
    pub fn mid(&self) -> V2 {
        match self.arc() {
            None => (self.a + self.b) * 0.5,
            Some((c, r, sw)) => {
                let d = self.a - c;
                let a0 = d.y.atan2(d.x) + sw / 2.0;
                c + v2(a0.cos(), a0.sin()) * r
            }
        }
    }
    /// Bulge of the arc a -> m -> b through three points (0 for collinear ones).
    pub fn bulge_through(a: V2, m: V2, b: V2) -> f64 {
        // inscribed angle g at m: the arc through m sweeps 2 (pi - g), so the bulge is tan((pi - g) / 2)
        let (u, w) = (a - m, b - m);
        let g = u.cross(w).abs().atan2(u.dot(w));
        let s = (b - a).cross(m - a);
        if s == 0.0 {
            return 0.0;
        }
        // counter-clockwise a -> b when m lies to the right of the chord
        -s.signum() * ((std::f64::consts::PI - g) / 2.0).tan()
    }
}

/// Ring edges without zero-length ones, turned counter-clockwise (`ccw`) or clockwise.
pub fn ring_segs(r: &BulgeRing, ccw: bool) -> Vec<Seg> {
    let mut r = r.clone();
    r.bulges.resize(r.pts.len(), 0.0);
    let mut r = r.ccw();
    if !ccw {
        let n = r.pts.len();
        r.pts.reverse();
        let old = r.bulges.clone();
        for i in 0..n {
            r.bulges[i] = -old[(2 * n - 2 - i) % n];
        }
    }
    let n = r.pts.len();
    (0..n)
        .map(|i| Seg {
            a: r.pts[i],
            b: r.pts[(i + 1) % n],
            bulge: r.bulges[i],
        })
        .filter(|s| (s.b - s.a).len() > 1e-12 * (1.0 + s.a.len()))
        .collect()
}

#[derive(Default)]
pub(super) struct Bld {
    pub(super) b: Body,
}

impl Bld {
    pub(super) fn v(&mut self, p: V3) -> u32 {
        self.b.verts.push(p);
        (self.b.verts.len() - 1) as u32
    }
    pub(super) fn edge(&mut self, v: [u32; 2], curve: Curve) -> u32 {
        self.b.edges.push(Edge { v, curve, sense: true });
        (self.b.edges.len() - 1) as u32
    }
    /// Circular edge from `a` to `b` counter-clockwise about `n` around `c` (closed if a == b).
    pub(super) fn arc(&mut self, a: u32, b: u32, c: V3, n: V3) -> u32 {
        let pa = self.b.verts[a as usize];
        let r = (pa - c).len();
        self.edge([a, b], Curve::Circle { ax: Ax3::new(c, n, pa - c), r })
    }
    pub(super) fn face(&mut self, surface: Surface, sense: bool, loops: Vec<Vec<(u32, bool)>>) -> u32 {
        self.b.faces.push(Face {
            surface,
            sense,
            loops,
            rgb: None,
        });
        (self.b.faces.len() - 1) as u32
    }
}

pub(super) fn frame_xf(fr: &Frame) -> Xf {
    Xf {
        m: [
            [fr.x.x, fr.y.x, fr.z.x],
            [fr.x.y, fr.y.y, fr.z.y],
            [fr.x.z, fr.y.z, fr.z.z],
        ],
        t: fr.o,
    }
}

pub(super) fn rings(profile: &Section) -> Vec<Vec<Seg>> {
    std::iter::once(ring_segs(&profile.outer, true))
        .chain(profile.holes.iter().map(|h| ring_segs(h, false)))
        .filter(|r| !r.is_empty())
        .collect()
}

/// Accuracy of the bodies built from forms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Opts {
    /// Chord sag of the polyhedron written when a form has no exact body, metres.
    pub sag: f64,
    /// Largest deviation of a fitted B-spline face from the exact sweep, metres.
    pub tol: f64,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            sag: 1e-3,
            tol: super::sweep::FIT_TOL,
        }
    }
}

/// Exact body of a form placed by `fr`, or its planar polyhedron at `o.sag` with the reason
/// the exact body could not be built. An invalid form is an error.
pub fn form_with(f: &Form, fr: &Frame, o: &Opts) -> Result<(Body, Option<String>), String> {
    gsolid::validate(f).map_err(|e| e.to_string())?;
    let exact = match f {
        Form::Extrusion { profile, start, end } => extrusion(profile, *start, *end, fr),
        Form::Revolve {
            profile,
            axis_o,
            axis_d,
            start,
            end,
        } => revolve(profile, *axis_o, *axis_d, *start, *end, fr),
        Form::Blend { bottom, top, height } => super::sweep::blend(bottom, top, *height, fr, o.tol),
        Form::Sweep { profile, path } => super::sweep::sweep(profile, path, fr, o.tol),
        Form::SweptBlend { bottom, top, path } => super::sweep::swept_blend(bottom, top, path, fr, o.tol),
    };
    match exact {
        Ok(b) => Ok((b, None)),
        Err(why) => {
            let m = gsolid::mesh(f, fr, o.sag).map_err(|e| e.to_string())?;
            polyhedron(&m).map(|b| (b, Some(why)))
        }
    }
}

/// [`form_with`] at the default fit tolerance; the flag is false for a polyhedron.
pub fn form(f: &Form, fr: &Frame, sag: f64) -> Result<(Body, bool), String> {
    let o = Opts { sag, ..Opts::default() };
    form_with(f, fr, &o).map(|(b, why)| (b, why.is_none()))
}

/// Profile x [start, end] along the local z of `fr`: line sides are planes, arc sides cylinders.
pub fn extrusion(profile: &Section, start: f64, end: f64, fr: &Frame) -> Result<Body, String> {
    gsolid::validate(&Form::Extrusion {
        profile: profile.clone(),
        start,
        end,
    })
    .map_err(|e| e.to_string())?;
    let (z0, z1) = (start.min(end), start.max(end));
    let up = v3(0.0, 0.0, 1.0);
    let mut k = Bld::default();
    let (mut bottom, mut top) = (vec![], vec![]);
    let mut sides = vec![];
    for ring in rings(profile) {
        let n = ring.len();
        let vb: Vec<u32> = ring.iter().map(|s| k.v(v3(s.a.x, s.a.y, z0))).collect();
        let vt: Vec<u32> = ring.iter().map(|s| k.v(v3(s.a.x, s.a.y, z1))).collect();
        let vert: Vec<u32> = (0..n).map(|i| k.edge([vb[i], vt[i]], Curve::Line)).collect();
        let (mut eb, mut et) = (vec![], vec![]);
        for (i, s) in ring.iter().enumerate() {
            let j = (i + 1) % n;
            let (surface, sense) = match s.arc() {
                None => {
                    eb.push(k.edge([vb[i], vb[j]], Curve::Line));
                    et.push(k.edge([vt[i], vt[j]], Curve::Line));
                    let d = s.b - s.a;
                    let out = v3(d.y, -d.x, 0.0);
                    (Surface::Plane(Ax3::new(v3(s.a.x, s.a.y, z0), out, v3(d.x, d.y, 0.0))), true)
                }
                Some((c, r, sweep)) => {
                    let n_arc = if sweep > 0.0 { up } else { -up };
                    eb.push(k.arc(vb[i], vb[j], v3(c.x, c.y, z0), n_arc));
                    et.push(k.arc(vt[i], vt[j], v3(c.x, c.y, z1), n_arc));
                    let d = s.a - c;
                    (
                        Surface::Cylinder {
                            ax: Ax3::new(v3(c.x, c.y, z0), up, v3(d.x, d.y, 0.0)),
                            r,
                        },
                        sweep > 0.0,
                    )
                }
            };
            sides.push((surface, sense, vec![(eb[i], true), (vert[j], true), (et[i], false), (vert[i], false)]));
        }
        bottom.push((0..n).rev().map(|i| (eb[i], false)).collect::<Vec<_>>());
        top.push((0..n).map(|i| (et[i], true)).collect::<Vec<_>>());
    }
    let x = v3(1.0, 0.0, 0.0);
    k.face(Surface::Plane(Ax3::new(v3(0.0, 0.0, z0), -up, x)), true, bottom);
    k.face(Surface::Plane(Ax3::new(v3(0.0, 0.0, z1), up, x)), true, top);
    for (s, sense, l) in sides {
        k.face(s, sense, vec![l]);
    }
    k.b.shells = vec![(0..k.b.faces.len() as u32).collect()];
    k.b.closed = true;
    k.b.transformed(&frame_xf(fr))
}

/// Profile point in axis coordinates: (along the axis, distance from it).
#[derive(Clone, Copy)]
struct Hr {
    h: f64,
    r: f64,
}

/// Revolution of the profile about the axis (`axis_o`, `axis_d`) of its plane from `start` to
/// `end`. Meridian segments: radial line -> plane, axial line -> cylinder, other line -> cone,
/// arc centred on the axis -> sphere, other arc -> torus (rational NURBS when it would self-cross).
pub fn revolve(profile: &Section, axis_o: V2, axis_d: V2, start: f64, end: f64, fr: &Frame) -> Result<Body, String> {
    gsolid::validate(&Form::Revolve {
        profile: profile.clone(),
        axis_o,
        axis_d,
        start,
        end,
    })
    .map_err(|e| e.to_string())?;
    let d = axis_d * (1.0 / axis_d.len());
    let da = (end - start).abs().min(TAU);
    let full = (da - TAU).abs() < 1e-9;
    let probe = gsolid::tess(&profile.outer, 1e-4);
    let hi = probe.iter().map(|p| d.cross(*p - axis_o)).fold(f64::MIN, f64::max);
    let side = if hi > 1e-9 { 1.0 } else { -1.0 };
    let to_hr = |p: V2| v2(d.dot(p - axis_o), side * d.cross(p - axis_o));
    let map_ring = |r: &BulgeRing| BulgeRing {
        pts: r.pts.iter().map(|&p| to_hr(p)).collect(),
        bulges: (0..r.pts.len()).map(|i| side * r.bulges.get(i).copied().unwrap_or(0.0)).collect(),
    };
    let hr = Section {
        outer: map_ring(&profile.outer),
        holes: profile.holes.iter().map(map_ring).collect(),
    };
    let size = probe.iter().map(|p| (*p - axis_o).len()).fold(0.0, f64::max);
    let tol = 1e-9 * (1.0 + size);

    let ao = v3(axis_o.x, axis_o.y, 0.0);
    let ad = v3(d.x, d.y, 0.0);
    let n0 = v3(-d.y, d.x, 0.0) * side;
    let er = |phi: f64| n0 * phi.cos() + ad.cross(n0) * phi.sin();
    let x0 = er(start.min(end));
    let y0 = ad.cross(x0);
    let e_r = |t: f64| x0 * t.cos() + y0 * t.sin();
    let e_phi = |t: f64| -x0 * t.sin() + y0 * t.cos();
    let at = |q: Hr, t: f64| ao + ad * q.h + e_r(t) * q.r;
    let axis_at = |h: f64| Ax3 {
        o: ao + ad * h,
        z: ad,
        x: x0,
    };

    let mut k = Bld::default();
    let mut shells: Vec<Vec<u32>> = vec![];
    let mut end_loops: Vec<(Vec<(u32, bool)>, Vec<(u32, bool)>)> = vec![];
    for ring in rings(&hr) {
        let n = ring.len();
        let pts: Vec<Hr> = ring.iter().map(|s| Hr { h: s.a.x, r: s.a.y.max(0.0) }).collect();
        let on_axis: Vec<bool> = pts.iter().map(|p| p.r <= tol).collect();
        let v0: Vec<u32> = pts.iter().map(|&p| k.v(at(p, 0.0))).collect();
        let v1: Vec<u32> = (0..n)
            .map(|i| if full || on_axis[i] { v0[i] } else { k.v(at(pts[i], da)) })
            .collect();
        let circ: Vec<Option<u32>> = (0..n)
            .map(|i| (!on_axis[i]).then(|| k.arc(v0[i], v1[i], ao + ad * pts[i].h, ad)))
            .collect();
        let merid = |k: &mut Bld, s: &Seg, t: f64, a: u32, b: u32| -> u32 {
            match s.arc() {
                None => k.edge([a, b], Curve::Line),
                Some((c, _, sweep)) => {
                    let n = if sweep > 0.0 { e_phi(t) } else { -e_phi(t) };
                    k.arc(a, b, at(Hr { h: c.x, r: c.y }, t), n)
                }
            }
        };
        let mut shell = vec![];
        let (mut l_start, mut l_end) = (vec![], vec![]);
        for (i, s) in ring.iter().enumerate() {
            let j = (i + 1) % n;
            let axial_line = on_axis[i] && on_axis[j] && s.arc().is_none();
            if axial_line {
                if !full {
                    let e = merid(&mut k, s, 0.0, v0[i], v0[j]);
                    l_start.push((e, false));
                    l_end.push((e, true));
                }
                continue;
            }
            let (dh, dr) = (s.b.x - s.a.x, s.b.y - s.a.y);
            let len = dh.hypot(dr);
            let (surface, sense) = match s.arc() {
                None if dr.abs() <= 1e-12 * len => (Surface::Cylinder { ax: axis_at(0.0), r: pts[i].r }, dh < 0.0),
                None if dh.abs() <= 1e-12 * len => {
                    let a = axis_at(s.a.x);
                    (
                        Surface::Plane(Ax3 {
                            z: if dr > 0.0 { ad } else { -ad },
                            ..a
                        }),
                        true,
                    )
                }
                None => {
                    let big = if dr > 0.0 { s.b } else { s.a };
                    let z = if dh * dr > 0.0 { ad } else { -ad };
                    let a = axis_at(big.x);
                    (
                        Surface::Cone {
                            ax: Ax3::new(a.o, z, x0),
                            r: big.y.max(0.0),
                            semi: (dr.abs() / dh.abs()).atan(),
                        },
                        dh < 0.0,
                    )
                }
                Some((c, r, sweep)) if c.y.abs() <= tol * 10.0 => (Surface::Sphere { ax: axis_at(c.x), r }, sweep > 0.0),
                Some((c, r, sweep)) if c.y > r + tol => (
                    Surface::Torus {
                        ax: axis_at(c.x),
                        major: c.y,
                        minor: r,
                    },
                    sweep > 0.0,
                ),
                Some((c, r, sweep)) => {
                    let cc = at(Hr { h: c.x, r: c.y }, 0.0);
                    let pa = at(pts[i], 0.0);
                    let xa = (pa - cc) * (1.0 / r);
                    let nrm = if sweep > 0.0 { e_phi(0.0) } else { -e_phi(0.0) };
                    let arc = nurbs::Curve::arc(cc, xa, nrm.cross(xa), r, 0.0, sweep.abs()).map_err(|e| e.to_string())?;
                    let srf = nurbs::revolve(&arc, ao, ad, da).map_err(|e| e.to_string())?;
                    (Surface::Nurbs(srf), false)
                }
            };
            let plane_full = full && matches!(surface, Surface::Plane(_));
            let loops = if plane_full {
                let mut ls: Vec<(f64, Vec<(u32, bool)>)> = vec![];
                if let Some(c) = circ[j] {
                    ls.push((pts[j].r, vec![(c, true)]));
                }
                if let Some(c) = circ[i] {
                    ls.push((pts[i].r, vec![(c, false)]));
                }
                ls.sort_by(|a, b| b.0.total_cmp(&a.0));
                ls.into_iter().map(|x| x.1).collect()
            } else {
                let m0 = merid(&mut k, s, 0.0, v0[i], v0[j]);
                let m1 = if full { m0 } else { merid(&mut k, s, da, v1[i], v1[j]) };
                if !full {
                    l_start.push((m0, false));
                    l_end.push((m1, true));
                }
                let mut l = vec![(m0, true)];
                l.extend(circ[j].map(|c| (c, true)));
                l.push((m1, false));
                l.extend(circ[i].map(|c| (c, false)));
                vec![l]
            };
            shell.push(k.face(surface, sense, loops));
        }
        l_start.reverse();
        end_loops.push((l_start, l_end));
        shells.push(shell);
    }
    if !full {
        let (starts, ends): (Vec<_>, Vec<_>) = end_loops.into_iter().unzip();
        let mut all: Vec<u32> = shells.concat();
        all.push(k.face(Surface::Plane(Ax3::new(ao, -e_phi(0.0), ad)), true, starts));
        all.push(k.face(Surface::Plane(Ax3::new(ao, e_phi(da), ad)), true, ends));
        shells = vec![all];
    }
    k.b.shells = shells;
    k.b.closed = true;
    k.b.transformed(&frame_xf(fr))
}

/// Planar polyhedron of a triangle mesh: positions welded exactly, one face per triangle,
/// closed when every edge has its opposite.
pub fn polyhedron(m: &Mesh) -> Result<Body, String> {
    let mut k = Bld::default();
    let mut ids: HashMap<[u64; 3], u32> = HashMap::new();
    let vid: Vec<u32> = m
        .positions
        .iter()
        .map(|p| {
            let key = [p.x + 0.0, p.y + 0.0, p.z + 0.0].map(f64::to_bits);
            *ids.entry(key).or_insert_with(|| {
                k.b.verts.push(*p);
                (k.b.verts.len() - 1) as u32
            })
        })
        .collect();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut dir_count: HashMap<(u32, u32), u32> = HashMap::new();
    for t in &m.triangles {
        let v = t.map(|i| vid[i as usize]);
        if v[0] == v[1] || v[1] == v[2] || v[0] == v[2] {
            continue;
        }
        let p = v.map(|i| k.b.verts[i as usize]);
        let nrm = (p[1] - p[0]).cross(p[2] - p[0]);
        if nrm.len() <= 1e-300 {
            continue;
        }
        let mut l = vec![];
        for q in 0..3 {
            let (a, b) = (v[q], v[(q + 1) % 3]);
            let key = (a.min(b), a.max(b));
            let e = match edges.get(&key) {
                Some(&e) => e,
                None => {
                    let e = k.edge([key.0, key.1], Curve::Line);
                    edges.insert(key, e);
                    e
                }
            };
            *dir_count.entry((a, b)).or_default() += 1;
            l.push((e, a == key.0));
        }
        k.face(Surface::Plane(Ax3::new(p[0], nrm, p[1] - p[0])), true, vec![l]);
    }
    if k.b.faces.is_empty() {
        return Err("the mesh has no triangle with area".into());
    }
    k.b.closed = dir_count.iter().all(|(&(a, b), &n)| n == 1 && dir_count.get(&(b, a)) == Some(&1));
    k.b.shells = vec![(0..k.b.faces.len() as u32).collect()];
    Ok(k.b)
}
