// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Exact STEP import: B-rep faces with their surfaces and edge curves into
//! [`crate::exact::Body`] parts, placed like the mesh reader places them (metres).

use std::collections::HashMap;

use ddc_geom::math::{v3, V3};
use ddc_geom::nurbs;

use super::curve::{placement, point};
use super::math::Ax;
use super::p21::{self, Store, P};
use super::{colours, solid_name, structure, topo, Units};
use crate::exact::{Ax3, Body, Curve, Doc, Edge, Face, Node, Part, Surface, Xf};

/// Parts and placements of a STEP file with exact geometry; warnings for skipped solids.
pub fn read_exact(bytes: &[u8]) -> Result<(Doc, Vec<String>), String> {
    let st = p21::parse(bytes, None)?;
    let mut warns = vec![];
    let (insts, _) = structure(&st, &mut warns);
    let cols = colours(&st);
    let mut doc = Doc::default();
    let mut part_of: HashMap<(u64, u64, u64), Option<usize>> = HashMap::new();
    for inst in &insts {
        let key = (inst.solid, inst.units.len.to_bits(), inst.units.ang.to_bits());
        let part = match part_of.get(&key) {
            Some(p) => *p,
            None => {
                let p = match Reader::new(&st, inst.units, &cols).body(inst.solid) {
                    Ok(body) => {
                        let rgb = cols
                            .get(&inst.solid)
                            .or_else(|| topo::shells(&st, inst.solid).iter().find_map(|(s, _)| cols.get(s)))
                            .map(|c| c.0);
                        doc.parts.push(Part {
                            name: solid_name(&st, inst.solid).unwrap_or_else(|| inst.name.clone()),
                            body,
                            rgb,
                        });
                        Some(doc.parts.len() - 1)
                    }
                    Err(e) => {
                        warns.push(format!("#{}: {e}", inst.solid));
                        None
                    }
                };
                part_of.insert(key, p);
                p
            }
        };
        let Some(part) = part else { continue };
        let x = &inst.xf;
        let xf = Xf {
            m: x.m,
            t: v3(x.t[0], x.t[1], x.t[2]),
        };
        let place = match xf.to_ax() {
            Some(a) => (a, part),
            None => {
                // mirrored or scaled placement: a part of its own in document coordinates
                let mut p = doc.parts[part].clone();
                p.body = p.body.transformed(&xf)?;
                doc.parts.push(p);
                (Ax3::WORLD, doc.parts.len() - 1)
            }
        };
        // the occurrence name of the last assembly usage, else the product name
        let name = inst
            .path
            .last()
            .and_then(|&u| st.rec(u, "NEXT_ASSEMBLY_USAGE_OCCURRENCE"))
            .and_then(|a| a.get(1))
            .and_then(P::text)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| inst.name.clone(), String::from);
        doc.roots.push(Node::leaf(&name, place.0, place.1));
    }
    if doc.parts.is_empty() {
        let mut e = "no solid or surface model with exact faces found".to_string();
        if let Some(w) = warns.last() {
            e += &format!(" ({w})");
        }
        return Err(e);
    }
    Ok((doc, warns))
}

struct Reader<'a> {
    st: &'a Store,
    u: Units,
    cols: &'a HashMap<u64, ([u8; 3], f32)>,
    b: Body,
    verts: HashMap<u64, u32>,
    edges: HashMap<u64, u32>,
    poly: HashMap<(u32, u32), u32>,
}

fn v(a: [f64; 3]) -> V3 {
    v3(a[0], a[1], a[2])
}

impl<'a> Reader<'a> {
    fn new(st: &'a Store, u: Units, cols: &'a HashMap<u64, ([u8; 3], f32)>) -> Self {
        Reader {
            st,
            u,
            cols,
            b: Body::default(),
            verts: HashMap::new(),
            edges: HashMap::new(),
            poly: HashMap::new(),
        }
    }

    fn args(&self, id: u64) -> Result<(&'a str, &'a [P]), String> {
        let st: &'a Store = self.st;
        let k = st.kind(id).ok_or_else(|| format!("#{id} is missing"))?;
        Ok((k, st.rec(id, k).unwrap_or(&[])))
    }

    fn pt(&self, id: u64) -> Result<V3, String> {
        Ok(v(point(self.st, id)?) * self.u.len)
    }

    fn ax(&self, id: Option<&P>) -> Result<Ax3, String> {
        let a: Ax = placement(self.st, id.and_then(P::id).unwrap_or(0))?;
        Ok(Ax3 {
            o: v(a.o) * self.u.len,
            z: v(a.z),
            x: v(a.x),
        })
    }

    fn body(mut self, solid: u64) -> Result<Body, String> {
        let kind = self.st.kind(solid).unwrap_or("?");
        self.b.closed = matches!(kind, "MANIFOLD_SOLID_BREP" | "BREP_WITH_VOIDS" | "FACETED_BREP");
        for (shell, flip) in topo::shells(self.st, solid) {
            let mut fs = vec![];
            topo::faces(self.st, shell, flip, &mut fs, 0);
            let mut ids = vec![];
            for (f, flip) in fs {
                let face = self.face(f, flip).map_err(|e| format!("face #{f}: {e}"))?;
                self.b.faces.push(face);
                ids.push((self.b.faces.len() - 1) as u32);
            }
            if !ids.is_empty() {
                self.b.shells.push(ids);
            }
        }
        if self.b.faces.is_empty() {
            return Err("no faces".into());
        }
        Ok(self.b)
    }

    fn face(&mut self, id: u64, flip: bool) -> Result<Face, String> {
        let (k, a) = self.args(id)?;
        if !matches!(k, "ADVANCED_FACE" | "FACE_SURFACE") {
            return Err(format!("unsupported face {}", self.st.describe(id)));
        }
        let surface = self.surface(a.get(2).and_then(P::id).ok_or("no surface")?)?;
        let sense = a.get(3).and_then(P::flag).unwrap_or(true) != flip;
        let mut outer = vec![];
        let mut inner = vec![];
        for b in a.get(1).map(P::ids).unwrap_or_default() {
            let (bk, ba) = self.args(b)?;
            let lp = ba.get(1).and_then(P::id).ok_or("bound without loop")?;
            let ori = ba.get(2).and_then(P::flag).unwrap_or(true) != flip;
            let Some(mut l) = self.lp(lp)? else { continue };
            if !ori {
                l = l.into_iter().rev().map(|(e, f)| (e, !f)).collect();
            }
            if bk == "FACE_OUTER_BOUND" {
                outer.push(l);
            } else {
                inner.push(l);
            }
        }
        outer.extend(inner);
        if outer.is_empty() {
            return Err("no bounding loop".into());
        }
        Ok(Face {
            surface,
            sense,
            loops: outer,
            rgb: self.cols.get(&id).map(|c| c.0),
        })
    }

    /// An edge or poly loop; a vertex loop (apex) gives none.
    fn lp(&mut self, id: u64) -> Result<Option<Vec<(u32, bool)>>, String> {
        let (k, a) = self.args(id)?;
        match k {
            "EDGE_LOOP" => {
                let mut out = vec![];
                for oe in a.get(1).map(P::ids).unwrap_or_default() {
                    let (ok, oa) = self.args(oe)?;
                    if ok != "ORIENTED_EDGE" {
                        return Err(format!("unsupported loop member {}", self.st.describe(oe)));
                    }
                    let e = oa.get(3).and_then(P::id).ok_or("oriented edge without edge")?;
                    let fwd = oa.get(4).and_then(P::flag).unwrap_or(true);
                    out.push((self.edge(e)?, fwd));
                }
                Ok(Some(out))
            }
            "POLY_LOOP" => {
                let pts = a.get(1).map(P::ids).unwrap_or_default();
                let n = pts.len();
                let mut out = vec![];
                for i in 0..n {
                    let (p, q) = (self.vertex_pt(pts[i])?, self.vertex_pt(pts[(i + 1) % n])?);
                    // polygon sides are shared by the vertex pair
                    let key = (p.min(q), p.max(q));
                    let e = match self.poly.get(&key) {
                        Some(&e) => e,
                        None => {
                            self.b.edges.push(Edge {
                                v: [key.0, key.1],
                                curve: Curve::Line,
                                sense: true,
                            });
                            let e = (self.b.edges.len() - 1) as u32;
                            self.poly.insert(key, e);
                            e
                        }
                    };
                    out.push((e, p == key.0));
                }
                Ok(Some(out))
            }
            "VERTEX_LOOP" => Ok(None),
            _ => Err(format!("unsupported loop {}", self.st.describe(id))),
        }
    }

    /// Vertex of a loop point (cartesian point id), shared by point identity.
    fn vertex_pt(&mut self, p: u64) -> Result<u32, String> {
        if let Some(&i) = self.verts.get(&p) {
            return Ok(i);
        }
        let q = self.pt(p)?;
        self.b.verts.push(q);
        let i = (self.b.verts.len() - 1) as u32;
        self.verts.insert(p, i);
        Ok(i)
    }

    fn vertex(&mut self, id: u64) -> Result<u32, String> {
        if let Some(&i) = self.verts.get(&id) {
            return Ok(i);
        }
        let (_, a) = self.args(id)?;
        let p = self.pt(a.get(1).and_then(P::id).ok_or("vertex without point")?)?;
        self.b.verts.push(p);
        let i = (self.b.verts.len() - 1) as u32;
        self.verts.insert(id, i);
        Ok(i)
    }

    fn edge(&mut self, id: u64) -> Result<u32, String> {
        if let Some(&i) = self.edges.get(&id) {
            return Ok(i);
        }
        let (k, a) = self.args(id)?;
        if k != "EDGE_CURVE" {
            return Err(format!("unsupported edge {}", self.st.describe(id)));
        }
        let v0 = self.vertex(a.get(1).and_then(P::id).ok_or("edge without start")?)?;
        let v1 = self.vertex(a.get(2).and_then(P::id).ok_or("edge without end")?)?;
        let curve = self.curve(a.get(3).and_then(P::id).ok_or("edge without curve")?, 0)?;
        let sense = a.get(4).and_then(P::flag).unwrap_or(true);
        self.b.edges.push(Edge { v: [v0, v1], curve, sense });
        let i = (self.b.edges.len() - 1) as u32;
        self.edges.insert(id, i);
        Ok(i)
    }

    fn knots(mults: Option<&P>, knots: Option<&P>) -> Vec<f64> {
        let m = mults.map(P::nums).unwrap_or_default();
        let k = knots.map(P::nums).unwrap_or_default();
        k.iter()
            .zip(&m)
            .flat_map(|(&x, &n)| std::iter::repeat_n(x, n.max(0.0) as usize))
            .collect()
    }

    fn curve(&self, id: u64, depth: u32) -> Result<Curve, String> {
        if depth > 8 {
            return Err(format!("#{id}: curve references nest too deep"));
        }
        let st = self.st;
        if let Some(a) = st.rec(id, "B_SPLINE_CURVE") {
            let k = st.rec(id, "B_SPLINE_CURVE_WITH_KNOTS").ok_or_else(|| format!("unsupported curve {}", st.describe(id)))?;
            let w = st.rec(id, "RATIONAL_B_SPLINE_CURVE").and_then(|r| r.first());
            return self.spline_curve(a.first(), a.get(1), k.first(), k.get(1), w);
        }
        let (k, a) = self.args(id)?;
        let num = |i: usize| a.get(i).and_then(P::num).filter(|x| x.is_finite() && *x > 0.0);
        match k {
            "LINE" => Ok(Curve::Line),
            "CIRCLE" => Ok(Curve::Circle {
                ax: self.ax(a.get(1))?,
                r: num(2).ok_or_else(|| format!("#{id}: bad radius"))? * self.u.len,
            }),
            "ELLIPSE" => Ok(Curve::Ellipse {
                ax: self.ax(a.get(1))?,
                a: num(2).ok_or_else(|| format!("#{id}: bad semi axis"))? * self.u.len,
                b: num(3).ok_or_else(|| format!("#{id}: bad semi axis"))? * self.u.len,
            }),
            "POLYLINE" if a.get(1).map(P::ids).unwrap_or_default().len() == 2 => Ok(Curve::Line),
            "B_SPLINE_CURVE_WITH_KNOTS" => self.spline_curve(a.get(1), a.get(2), a.get(6), a.get(7), None),
            "SURFACE_CURVE" | "SEAM_CURVE" | "INTERSECTION_CURVE" | "TRIMMED_CURVE" => {
                self.curve(a.get(1).and_then(P::id).ok_or_else(|| format!("#{id}: no basis curve"))?, depth + 1)
            }
            _ => Err(format!("unsupported curve {}", st.describe(id))),
        }
    }

    fn spline_curve(&self, deg: Option<&P>, ctrl: Option<&P>, mults: Option<&P>, knots: Option<&P>, w: Option<&P>) -> Result<Curve, String> {
        let pts = ctrl
            .map(P::ids)
            .unwrap_or_default()
            .into_iter()
            .map(|i| self.pt(i))
            .collect::<Result<Vec<V3>, String>>()?;
        let n = pts.len();
        let weights = w.map(P::nums).unwrap_or_else(|| vec![1.0; n]);
        let deg = deg.and_then(P::num).unwrap_or(0.0) as usize;
        nurbs::Curve::new(deg, Self::knots(mults, knots), pts, weights)
            .map(Curve::Nurbs)
            .map_err(|e| e.to_string())
    }

    fn surface(&self, id: u64) -> Result<Surface, String> {
        let st = self.st;
        if let Some(a) = st.rec(id, "B_SPLINE_SURFACE") {
            let k = st.rec(id, "B_SPLINE_SURFACE_WITH_KNOTS").ok_or_else(|| format!("unsupported surface {}", st.describe(id)))?;
            let w = st.rec(id, "RATIONAL_B_SPLINE_SURFACE").and_then(|r| r.first());
            return self.spline_surface(&a[..a.len().min(3)], k, w);
        }
        let (k, a) = self.args(id)?;
        let num = |i: usize| a.get(i).and_then(P::num).filter(|x| x.is_finite());
        let rad = |i: usize| {
            num(i)
                .filter(|r| *r > 0.0)
                .map(|r| r * self.u.len)
                .ok_or_else(|| format!("#{id}: bad radius"))
        };
        let ax = || self.ax(a.get(1));
        Ok(match k {
            "PLANE" => Surface::Plane(ax()?),
            "CYLINDRICAL_SURFACE" => Surface::Cylinder { ax: ax()?, r: rad(2)? },
            "CONICAL_SURFACE" => Surface::Cone {
                ax: ax()?,
                r: num(2).filter(|r| *r >= 0.0).ok_or_else(|| format!("#{id}: bad radius"))? * self.u.len,
                semi: num(3).ok_or_else(|| format!("#{id}: bad semi-angle"))? * self.u.ang,
            },
            "SPHERICAL_SURFACE" => Surface::Sphere { ax: ax()?, r: rad(2)? },
            "TOROIDAL_SURFACE" => Surface::Torus {
                ax: ax()?,
                major: rad(2)?,
                minor: rad(3)?,
            },
            "B_SPLINE_SURFACE_WITH_KNOTS" => {
                return self.spline_surface(a.get(1..4).unwrap_or(&[]), a.get(8..).unwrap_or(&[]), None)
            }
            _ => return Err(format!("unsupported surface {}", st.describe(id))),
        })
    }

    /// `head`: degrees and control rows; `k`: u and v multiplicities and knots.
    fn spline_surface(&self, head: &[P], k: &[P], w: Option<&P>) -> Result<Surface, String> {
        let du = head.first().and_then(P::num).unwrap_or(0.0) as usize;
        let dv = head.get(1).and_then(P::num).unwrap_or(0.0) as usize;
        let rows: Vec<Vec<u64>> = head
            .get(2)
            .and_then(P::list)
            .unwrap_or(&[])
            .iter()
            .map(P::ids)
            .collect();
        let nu = rows.len();
        let nv = rows.first().map_or(0, Vec::len);
        if nu == 0 || nv == 0 || rows.iter().any(|r| r.len() != nv) {
            return Err("ragged B-spline control net".into());
        }
        let mut ctrl = Vec::with_capacity(nu * nv);
        for r in &rows {
            for &i in r {
                ctrl.push(self.pt(i)?);
            }
        }
        let weights = match w.and_then(P::list) {
            Some(rows) => rows.iter().flat_map(P::nums).collect(),
            None => vec![1.0; nu * nv],
        };
        let ku = Self::knots(k.first(), k.get(2));
        let kv = Self::knots(k.get(1), k.get(3));
        nurbs::Surface::new(du, dv, ku, kv, nu, nv, ctrl, weights)
            .map(Surface::Nurbs)
            .map_err(|e| e.to_string())
    }
}
