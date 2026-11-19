// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Schema-26 structure -> IFC: isolated/strip footings as IfcFooting (PAD_FOOTING /
//! STRIP_FOOTING), rafts as IfcSlab BASESLAB, braces as IfcMember BRACE, trusses as an
//! IfcElementAssembly TRUSS aggregating IfcMember CHORD / POST / STRUT, beam systems as an
//! IfcElementAssembly BEAM_GRID aggregating IfcBeam JOIST. Bodies are triangulated, placed
//! in the storey frame; base quantities and a material association per element.

use std::collections::{BTreeMap, HashMap};

use ddc_geom::arc::ArcAxis;
use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs::Curve;
use ddc_geom::wall_spline::SplineAxis;
use ddc_geom::{v2, Tol, V2};
use ddc_model::{Model, StripAxis, StructBody, SPLINE_TOL};

use super::{list, r, real, reals, s, text, Ctx, Geo, NONE, Q};

fn body(c: &mut Ctx, g: &Geo, m: &Mesh, elev: f64) -> u32 {
    let rep = tess(c, g, &[m], elev);
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[rep])],
    )
}

/// Body of a strip footing with its exact axis next to it: a circular arc as IfcTrimmedCurve,
/// a spline as IfcBSplineCurveWithKnots, a straight band as a polyline (plan coordinates).
fn swept_body(c: &mut Ctx, g: &Geo, ms: &[&Mesh], elev: f64, axis: &FootAxis) -> u32 {
    let rep = tess(c, g, ms, elev);
    let line = match axis {
        FootAxis::Spline(cv) => super::spline_walls::axis_curve(c, cv, &|p: V2| (p.x, p.y)),
        FootAxis::Arc(a, b, bulge) => {
            let (pa, pb) = (v2(a.0, a.1), v2(b.0, b.1));
            match ArcAxis::new(pa, pb, *bulge, Tol::default()) {
                Ok(ax) if *bulge != 0.0 => {
                    let cp = c.point2(ax.center.x, ax.center.y);
                    let cpl = c.s.add("IFCAXIS2PLACEMENT2D", &[r(cp), s(NONE)]);
                    let circle = c.s.add("IFCCIRCLE", &[r(cpl), real(ax.radius)]);
                    let (p0, p1) = (c.point2(pa.x, pa.y), c.point2(pb.x, pb.y));
                    let sense = if ax.sweep > 0.0 { ".T." } else { ".F." };
                    c.s.add(
                        "IFCTRIMMEDCURVE",
                        &[r(circle), list(&[p0]), list(&[p1]), s(sense), s(".CARTESIAN.")],
                    )
                }
                _ => {
                    let (p0, p1) = (c.point2(pa.x, pa.y), c.point2(pb.x, pb.y));
                    c.s.add("IFCPOLYLINE", &[list(&[p0, p1])])
                }
            }
        }
    };
    let axis = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[r(g.axis_ctx), text("Axis"), text("Curve2D"), list(&[line])],
    );
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[axis, rep])],
    )
}

fn tess(c: &mut Ctx, g: &Geo, ms: &[&Mesh], elev: f64) -> u32 {
    let sets: Vec<u32> = ms.iter().map(|m| faceset(c, m, elev)).collect();
    c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[r(g.body_ctx), text("Body"), text("Tessellation"), list(&sets)],
    )
}

fn faceset(c: &mut Ctx, m: &Mesh, elev: f64) -> u32 {
    let coords = m
        .positions
        .iter()
        .map(|p| reals(&[p.x, p.y, p.z - elev]))
        .collect::<Vec<_>>()
        .join(",");
    let pts =
        c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)]);
    let idx = m
        .triangles
        .iter()
        .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
        .collect::<Vec<_>>()
        .join(",");
    c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), s(".T."), format!("({idx})"), s(NONE)],
    )
}

/// Entity, predefined type and quantity set of a single part.
fn class(kind: &str, role: &str) -> (&'static str, &'static str, &'static str) {
    match (kind, role) {
        ("footing", _) => ("IFCFOOTING", "PAD_FOOTING", "Qto_FootingBaseQuantities"),
        ("strip_footing", _) => ("IFCFOOTING", "STRIP_FOOTING", "Qto_FootingBaseQuantities"),
        ("raft", _) => ("IFCSLAB", "BASESLAB", "Qto_SlabBaseQuantities"),
        ("beam_system", _) => ("IFCBEAM", "JOIST", "Qto_BeamBaseQuantities"),
        ("gridshell", "node") => ("IFCDISCRETEACCESSORY", "USERDEFINED", "BaseQuantities"),
        ("gridshell", "member" | "lath") => ("IFCMEMBER", "MEMBER", "Qto_MemberBaseQuantities"),
        ("gridshell", "edge") => ("IFCMEMBER", "CHORD", "Qto_MemberBaseQuantities"),
        ("gridshell", "cable") => ("IFCMEMBER", "USERDEFINED", "Qto_MemberBaseQuantities"),
        (_, "top" | "bottom") => ("IFCMEMBER", "CHORD", "Qto_MemberBaseQuantities"),
        (_, "vertical") => ("IFCMEMBER", "POST", "Qto_MemberBaseQuantities"),
        (_, "diagonal") => ("IFCMEMBER", "STRUT", "Qto_MemberBaseQuantities"),
        _ => ("IFCMEMBER", "BRACE", "Qto_MemberBaseQuantities"),
    }
}

fn label(kind: &str) -> &'static str {
    match kind {
        "footing" => "Footing",
        "strip_footing" => "Strip footing",
        "raft" => "Raft",
        "brace" => "Brace",
        "truss" => "Truss",
        "gridshell" => "Grid shell",
        _ => "Beam system",
    }
}

/// ObjectType of a user-defined part: grid nodes and cables have no predefined type.
fn object_type(kind: &str, role: &str) -> Option<&'static str> {
    match (kind, role) {
        ("gridshell", "node") => Some("NODE"),
        ("gridshell", "cable") => Some("CABLE"),
        _ => None,
    }
}

/// Assembly predefined type and ObjectType: a grid tells its kind by its member roles.
fn assembly_type(kind: &str, ps: &[&StructBody]) -> (&'static str, Option<&'static str>) {
    match kind {
        "truss" => ("TRUSS", None),
        "gridshell" => {
            let has = |r: &str| ps.iter().any(|p| p.role == r);
            let ot = if has("cable") {
                "CABLE_NET"
            } else if has("lath") {
                "GRIDSHELL"
            } else {
                "DIAGRID"
            };
            ("USERDEFINED", Some(ot))
        }
        _ => ("BEAM_GRID", None),
    }
}

/// Exact axis of a strip footing in plan.
pub(super) enum FootAxis {
    Arc((f64, f64), (f64, f64), f64),
    Spline(Curve),
}

/// Bodies with geometry, grouped by level, and the exact axes of the strip footings (the
/// footings under the pieces of one spline wall keyed by that wall); read once per export.
#[derive(Default)]
pub(super) struct Bodies {
    by_level: BTreeMap<i64, Vec<StructBody>>,
    axes: HashMap<i64, FootAxis>,
    spline_of: HashMap<i64, i64>,
    spline_axes: HashMap<i64, FootAxis>,
}

pub(super) fn load(model: &Model) -> Result<Bodies, String> {
    let mut b = Bodies::default();
    for p in model.struct_part_bodies(None).map_err(|e| e.to_string())? {
        // an empty tessellation is invalid STEP: no product for it
        if p.mesh.triangles.is_empty() {
            continue;
        }
        if p.kind == "strip_footing" && !b.axes.contains_key(&p.id) && !b.spline_of.contains_key(&p.id) {
            match model.strip_footing_axis(p.id).map_err(|e| e.to_string())? {
                StripAxis::Arc { a, b: e, bulge } => {
                    b.axes.insert(p.id, FootAxis::Arc(a, e, bulge));
                }
                StripAxis::Spline { spline, offset } => {
                    if !b.spline_axes.contains_key(&spline) {
                        let sw = model.spline_wall(spline).map_err(|e| e.to_string())?;
                        let cv = if offset == 0.0 {
                            sw.axis
                        } else {
                            let ax = SplineAxis::new(sw.axis)
                                .map_err(|e| format!("spline wall {spline}: {e:?}"))?;
                            ax.offset(offset, SPLINE_TOL)
                                .map_err(|e| format!("spline footing {}: {e:?}", p.id))?
                                .0
                        };
                        b.spline_axes.insert(spline, FootAxis::Spline(cv));
                    }
                    b.spline_of.insert(p.id, spline);
                    // the piece's own arc: used when the pieces differ and go out one by one
                    if let StripAxis::Arc { a, b: e, bulge } =
                        model.strip_footing_arc(p.id).map_err(|e| e.to_string())?
                    {
                        b.axes.insert(p.id, FootAxis::Arc(a, e, bulge));
                    }
                }
            }
        }
        b.by_level.entry(p.level).or_default().push(p);
    }
    Ok(b)
}

/// Full string key of one part; `guid(key, 0)` hashes it, so keys never collide by arithmetic.
fn part_key(kind: &str, id: i64, part: i64) -> String {
    format!("g6/{kind}/{id}/{part}")
}

/// Quantity set (lengths, areas, volumes; kg weights when the material density is known).
#[allow(clippy::too_many_arguments)]
fn qset(
    c: &mut Ctx,
    g: &Geo,
    kg: &mut Option<u32>,
    key: &str,
    name: &str,
    el: u32,
    q: &[Q],
    weight: Option<f64>,
) {
    let ents = ["IFCQUANTITYLENGTH", "IFCQUANTITYAREA", "IFCQUANTITYVOLUME"];
    let mut ql: Vec<u32> = q
        .iter()
        .map(|(n, k, v)| {
            c.s.add(
                ents[*k],
                &[text(n), s(NONE), r(g.units[*k]), real(*v), s(NONE)],
            )
        })
        .collect();
    if let Some(w) = weight.filter(|w| w.is_finite() && *w > 0.0) {
        let unit = match *kg {
            Some(u) => u,
            None => {
                let u = c.s.add(
                    "IFCSIUNIT",
                    &[s("*"), s(".MASSUNIT."), s(".KILO."), s(".GRAM.")],
                );
                *kg = Some(u);
                u
            }
        };
        for n in ["GrossWeight", "NetWeight"] {
            ql.push(c.s.add(
                "IFCQUANTITYWEIGHT",
                &[text(n), s(NONE), r(unit), real(w), s(NONE)],
            ));
        }
    }
    let eq = c.rooted(
        "IFCELEMENTQUANTITY",
        &format!("g6/qto/{key}"),
        0,
        name,
        &[s(NONE), list(&ql)],
    );
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("g6/qtorel/{key}"),
        0,
        &[list(&[el]), r(eq)],
    );
}

/// Every schema-26 element of the level; contained elements go to `elems`.
pub(super) fn export(
    c: &mut Ctx,
    bodies: &Bodies,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let Some(parts) = bodies.by_level.get(&level) else {
        return Ok(());
    };
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    let mut kg: Option<u32> = None;
    let mut by_el: BTreeMap<(&str, i64), Vec<&StructBody>> = BTreeMap::new();
    // footings under the pieces of one spline wall: one IfcFooting along the spline
    let mut by_spline: BTreeMap<i64, Vec<&StructBody>> = BTreeMap::new();
    for p in parts {
        match bodies.spline_of.get(&p.id).filter(|_| p.kind == "strip_footing") {
            Some(&sid) => by_spline.entry(sid).or_default().push(p),
            None => by_el.entry((p.kind, p.id)).or_default().push(p),
        }
    }
    // pieces of different section or material are separate footings, each on its exact arc
    by_spline.retain(|_, ps| {
        let q = ps[0];
        let same = ps.iter().all(|p| {
            (p.size[1] - q.size[1]).abs() <= 1e-9
                && (p.size[2] - q.size[2]).abs() <= 1e-9
                && p.material == q.material
        });
        if !same {
            for &p in ps.iter() {
                by_el.entry((p.kind, p.id)).or_default().push(p);
            }
        }
        same
    });
    let mut mats: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for ((kind, id), ps) in by_el {
        let name = format!("{} {id}", label(kind));
        let assembly = matches!(kind, "truss" | "beam_system" | "gridshell");
        // identity offset in the storey frame; parts of an assembly nest under it
        let asm_pl = assembly.then(|| c.placement(Some(g.storey_pl), ax));
        let mut ents = vec![];
        for p in &ps {
            let (ent, pre, qs) = class(kind, p.role);
            let key = part_key(kind, id, p.part);
            let pds = match bodies.axes.get(&id).filter(|_| kind == "strip_footing") {
                Some(fa) => swept_body(c, g, &[&p.mesh], elev, fa),
                None => body(c, g, &p.mesh, elev),
            };
            let ppl = c.placement(Some(asm_pl.unwrap_or(g.storey_pl)), ax);
            let pname = if assembly {
                format!("{name}:{}", p.role)
            } else {
                name.clone()
            };
            let ot = object_type(kind, p.role).map_or_else(|| s(NONE), text);
            let e = c.rooted(
                ent,
                &key,
                0,
                &pname,
                &[ot, r(ppl), r(pds), s(NONE), format!(".{pre}.")],
            );
            let [len, wid, hgt] = p.size;
            let mut q: Vec<Q> = vec![];
            match kind {
                "footing" => {
                    q.extend([("Length", 0, len), ("Width", 0, wid), ("Height", 0, hgt)]);
                }
                "strip_footing" => {
                    q.extend([("Length", 0, len), ("Width", 0, wid), ("Height", 0, hgt)]);
                    q.push(("CrossSectionArea", 1, wid * hgt));
                }
                "raft" => {
                    if hgt > 0.0 {
                        q.push(("Depth", 0, hgt));
                    }
                    q.push(("Perimeter", 0, p.length));
                    q.push(("GrossArea", 1, p.area));
                }
                _ => {
                    q.push(("Length", 0, p.length));
                    q.push(("OuterSurfaceArea", 1, p.area));
                }
            }
            q.push(("GrossVolume", 2, p.volume));
            q.push(("NetVolume", 2, p.volume));
            qset(c, g, &mut kg, &key, qs, e, &q, p.weight);
            mats.entry(p.material.clone()).or_default().push(e);
            ents.push(e);
        }
        if let Some(apl) = asm_pl {
            if ents.is_empty() {
                continue;
            }
            let (pre, ot) = assembly_type(kind, &ps);
            let asm = c.rooted(
                "IFCELEMENTASSEMBLY",
                &format!("g6/{kind}/{id}"),
                0,
                &name,
                &[
                    ot.map_or_else(|| s(NONE), text),
                    r(apl),
                    s(NONE),
                    s(NONE),
                    s(NONE),
                    format!(".{pre}."),
                ],
            );
            c.rel(
                "IFCRELAGGREGATES",
                &format!("g6/agg/{kind}/{id}"),
                0,
                &[r(asm), list(&ents)],
            );
            elems.push(asm);
        } else {
            elems.extend(ents);
        }
    }
    for (sid, ps) in by_spline {
        let Some(fa) = bodies.spline_axes.get(&sid) else { continue };
        let key = format!("g6/strip_footing_spline/{sid}");
        let ms: Vec<&Mesh> = ps.iter().map(|p| &p.mesh).collect();
        let pds = swept_body(c, g, &ms, elev, fa);
        let ppl = c.placement(Some(g.storey_pl), ax);
        let e = c.rooted(
            "IFCFOOTING",
            &key,
            0,
            &format!("Spline strip footing {sid}"),
            &[s(NONE), r(ppl), r(pds), s(NONE), s(".STRIP_FOOTING.")],
        );
        let [_, wid, hgt] = ps[0].size;
        let len: f64 = ps.iter().map(|p| p.length).sum();
        let vol: f64 = ps.iter().map(|p| p.volume).sum();
        let weight = ps.iter().map(|p| p.weight).sum::<Option<f64>>();
        let q: Vec<Q> = vec![
            ("Length", 0, len),
            ("Width", 0, wid),
            ("Height", 0, hgt),
            ("CrossSectionArea", 1, wid * hgt),
            ("GrossVolume", 2, vol),
            ("NetVolume", 2, vol),
        ];
        qset(c, g, &mut kg, &key, "Qto_FootingBaseQuantities", e, &q, weight);
        mats.entry(ps[0].material.clone()).or_default().push(e);
        elems.push(e);
    }
    for (m, ents) in mats {
        let mat = c.material(&m);
        c.rel(
            "IFCRELASSOCIATESMATERIAL",
            &format!("g6/matrel/{level}/{m}"),
            0,
            &[list(&ents), r(mat)],
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::part_key;
    use crate::ifc::check_step;
    use crate::ifc::export::export_ifc;
    use crate::ifc::step::guid;
    use ddc_model::{
        BeamLayout, BeamSystemSpec, FootingSpec, FootingStep, Just, Model, RaftSpec,
        StripFootingSpec, TrussChord, TrussSpec, TrussWeb,
    };

    /// Attribute list of the entity line `id` (e.g. "#12").
    fn args(t: &str, id: &str) -> Vec<String> {
        let l = t
            .lines()
            .find(|l| l.starts_with(&format!("{id}=")))
            .unwrap_or_else(|| panic!("no entity {id}"));
        let a = &l[l.find('(').unwrap() + 1..l.rfind(')').unwrap()];
        a.split(',').map(str::to_string).collect()
    }

    fn first_args(t: &str, name: &str) -> Vec<String> {
        let l = t
            .lines()
            .find(|l| l.contains(&format!("={name}(")))
            .unwrap_or_else(|| panic!("no {name}"));
        args(t, &l[..l.find('=').unwrap()])
    }

    fn pad(m: &mut Model, lv: i64) {
        m.add_footing(&FootingSpec {
            level: lv,
            column: None,
            at: (0.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep {
                w: 1.5,
                l: 1.5,
                h: 0.4,
            }],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap();
    }

    fn count(t: &str, name: &str) -> usize {
        t.matches(&format!("={name}(")).count()
    }

    #[test]
    fn footings_raft_and_truss_assembly() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        m.add_footing(&FootingSpec {
            level: lv,
            column: None,
            at: (0.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![
                FootingStep {
                    w: 1.8,
                    l: 1.8,
                    h: 0.3,
                },
                FootingStep {
                    w: 1.0,
                    l: 1.0,
                    h: 0.3,
                },
            ],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap();
        m.add_raft(&RaftSpec {
            level: lv,
            ring: vec![(5.0, 0.0), (15.0, 0.0), (15.0, 8.0), (5.0, 8.0)],
            thickness: 0.3,
            edge_width: 0.6,
            edge_depth: 0.3,
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap();
        let ct = m.add_catalog_member_type("SHS 200x200x10").unwrap();
        let wt = m.add_catalog_member_type("L 100x100x10").unwrap();
        m.add_truss(&TrussSpec {
            level: lv,
            web: TrussWeb::Warren,
            chord: TrussChord::Flat,
            a: (0.0, 20.0, 6.0),
            b: (12.0, 20.0, 6.0),
            height: 1.5,
            heel: 0.0,
            panels: 6,
            chord_type: ct,
            web_type: wt,
        })
        .unwrap();
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCFOOTING"), 1);
        assert!(t.contains(".PAD_FOOTING."));
        assert!(t.contains(".BASESLAB."));
        assert_eq!(count(&t, "IFCELEMENTASSEMBLY"), 1);
        let parts = m.struct_parts("truss", 1).unwrap().len();
        assert_eq!(count(&t, "IFCMEMBER"), parts);
        assert!(t.contains(".CHORD.") && t.contains(".STRUT."));
        assert!(t.contains("Qto_FootingBaseQuantities"));
        // stepped footing volume 1.8²·0.3 + 1.0²·0.3
        let v = format!("{:?}", 1.8 * 1.8 * 0.3 + 0.3);
        assert!(t.contains(&v[..5]), "{v}");
    }

    #[test]
    fn truss_parts_nest_under_the_assembly_placement() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let ct = m.add_catalog_member_type("SHS 200x200x10").unwrap();
        m.add_truss(&TrussSpec {
            level: lv,
            web: TrussWeb::Warren,
            chord: TrussChord::Flat,
            a: (0.0, 0.0, 6.0),
            b: (12.0, 0.0, 6.0),
            height: 1.5,
            heel: 0.0,
            panels: 4,
            chord_type: ct,
            web_type: ct,
        })
        .unwrap();
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        let asm_pl = first_args(&t, "IFCELEMENTASSEMBLY")[5].clone();
        let mem_pl = first_args(&t, "IFCMEMBER")[5].clone();
        assert_ne!(asm_pl, mem_pl);
        // member placement -> assembly placement -> storey placement
        assert_eq!(args(&t, &mem_pl)[0], asm_pl);
        assert_ne!(args(&t, &asm_pl)[0], "$");
        // every member has its own placement
        let pls: std::collections::BTreeSet<String> = t
            .lines()
            .filter(|l| l.contains("=IFCMEMBER("))
            .map(|l| args(&t, &l[..l.find('=').unwrap()])[5].clone())
            .collect();
        assert_eq!(pls.len(), count(&t, "IFCMEMBER"));
    }

    #[test]
    fn pad_footing_quantities_follow_the_base_quantity_set() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        pad(&mut m, lv);
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert!(t.contains("Qto_FootingBaseQuantities"));
        assert!(!t.contains("GrossArea"));
        assert!(!t.contains("CrossSectionArea"));
        for n in [
            "'Length'",
            "'Width'",
            "'Height'",
            "'GrossVolume'",
            "'NetVolume'",
        ] {
            assert!(t.contains(n), "{n}");
        }
        let w = m.struct_part_bodies(None).unwrap()[0].weight;
        assert_eq!(t.contains("'NetWeight'"), w.is_some_and(|w| w > 0.0));
    }

    #[test]
    fn strip_footing_and_beam_system_export() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        m.add_strip_footings(&[StripFootingSpec {
            level: lv,
            wall: None,
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            bulge: 0.0,
            width: 0.6,
            depth: 0.4,
            ecc: 0.0,
            top_offset: 0.0,
            material: "Concrete".into(),
        }])
        .unwrap();
        let jt = m.add_catalog_member_type("SHS 200x200x10").unwrap();
        let id = m
            .add_beam_system(&BeamSystemSpec {
                level: lv,
                member_type: jt,
                ring: vec![(0.0, 5.0), (6.0, 5.0), (6.0, 9.0), (0.0, 9.0)],
                holes: vec![],
                start_edge: None,
                z: 3.0,
                angle: std::f64::consts::FRAC_PI_2,
                spacing: 1.5,
                layout: BeamLayout::FixedStart,
                end_gap: 0.01,
                zjust: Just::Pos,
            })
            .unwrap();
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert!(t.contains(".STRIP_FOOTING."));
        assert!(t.contains("'CrossSectionArea'"));
        assert!(!t.contains("GrossArea"));
        assert!(t.contains(".BEAM_GRID.") && t.contains(".JOIST."));
        let n = m.struct_parts("beam_system", id).unwrap().len();
        assert!(n > 0);
        assert_eq!(count(&t, "IFCBEAM"), n);
        assert_eq!(count(&t, "IFCELEMENTASSEMBLY"), 1);
        assert!(t.contains("Qto_BeamBaseQuantities"));
    }

    #[test]
    fn curved_strip_footings_export_their_exact_axes() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        m.add_strip_footings(&[StripFootingSpec {
            level: lv,
            wall: None,
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            bulge: 0.4,
            width: 0.6,
            depth: 0.4,
            ecc: 0.0,
            top_offset: 0.0,
            material: "Concrete".into(),
        }])
        .unwrap();
        let wt = m
            .add_wall_type(
                "W200",
                &[ddc_model::LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let sw = ddc_model::SplineWallSpec::through(
            lv,
            wt,
            &[(0.0, 5.0), (4.0, 7.5), (9.0, 4.0), (13.0, 6.5)],
            3.0,
        )
        .unwrap();
        let sid = m.add_spline_wall(&sw).unwrap();
        let pieces = m.spline_wall_pieces(sid).unwrap();
        let ids = m
            .strip_footings_under_walls(&pieces[..1], 0.6, 0.5, "Concrete")
            .unwrap();
        assert_eq!(ids.len(), pieces.len());
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        // arc band: a trimmed circle; spline band: one footing along the spline, not per piece
        assert_eq!(count(&t, "IFCFOOTING"), 2);
        assert!(t.contains("'Spline strip footing"));
        assert_eq!(count(&t, "IFCTRIMMEDCURVE"), 1, "only the arc footing's axis");
        assert_eq!(count(&t, "IFCBSPLINECURVEWITHKNOTS"), 2, "spline wall and its footing");
        // the spline footing's quantities are the sum of its pieces
        let vol: f64 = ids
            .iter()
            .map(|&i| m.struct_quantities("strip_footing", i).unwrap().volume)
            .sum();
        let l = t
            .lines()
            .filter(|l| l.contains("'NetVolume'"))
            .map(|l| {
                let a: Vec<&str> = l.split(',').collect();
                a[3].parse::<f64>().unwrap()
            })
            .any(|v| (v - vol).abs() < 1e-6 * vol);
        assert!(l, "NetVolume {vol} of the spline footing");
        // a wider piece splits the chain: one footing per piece, each on its own exact arc
        let mut wide = m.strip_footing(ids[1]).unwrap();
        wide.width = 0.9;
        m.update_strip_footing(ids[1], &wide).unwrap();
        let t = export_ifc(&m).unwrap();
        check_step(&t).unwrap();
        assert_eq!(count(&t, "IFCFOOTING"), 1 + ids.len());
        assert!(!t.contains("'Spline strip footing"));
        let arcs = ids
            .iter()
            .filter(|&&i| m.strip_footing(i).unwrap().bulge != 0.0)
            .count();
        assert!(arcs > 0);
        assert_eq!(count(&t, "IFCTRIMMEDCURVE"), 1 + arcs, "every arc piece on its own arc");
        assert_eq!(count(&t, "IFCBSPLINECURVEWITHKNOTS"), 1, "only the spline wall");
    }

    #[test]
    fn part_guids_do_not_collide_in_big_systems() {
        // the old `id * 10_000 + part` arithmetic mapped both to 10_000
        assert_ne!(
            part_key("beam_system", 1, 10_000),
            part_key("beam_system", 2, 0)
        );
        assert_ne!(
            guid(&part_key("beam_system", 1, 10_000), 0),
            guid(&part_key("beam_system", 2, 0), 0)
        );
        assert_ne!(
            guid(&part_key("truss", 1, 0), 0),
            guid(&part_key("beam_system", 1, 0), 0)
        );
    }

    /// Number of products aggregated by the assembly `asm` ("#12").
    fn aggregated(t: &str, asm: &str) -> usize {
        let l = t
            .lines()
            .find(|l| l.contains("=IFCRELAGGREGATES(") && l.contains(&format!(",{asm},(")))
            .unwrap_or_else(|| panic!("{asm} aggregates nothing"));
        let list = &l[l.find(&format!(",{asm},(")).unwrap() + asm.len() + 2..];
        list.matches('#').count()
    }

    #[test]
    fn grids_export_as_assemblies_of_members_and_nodes() {
        use ddc_geom::host::{Axes, Host};
        use ddc_geom::member::Shape;
        use ddc_model::{GridKind, GridPattern, GridShellSpec, GridSupports};
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_member_type("CHS", &Shape::Pipe { d: 0.0889, t: 0.005 }, "S355")
            .unwrap();
        let host = Host::Plane {
            ax: Axes::world(),
            w: 9.0,
            h: 6.0,
        };
        let spec = |kind| GridShellSpec {
            level: lv,
            kind,
            host: host.clone(),
            pattern: GridPattern::Quad,
            nu: 3,
            nv: 2,
            supports: GridSupports::Edges,
            member_type: t,
            edge_type: None,
            node_d: 0.3,
        };
        let a = m.add_gridshell(&spec(GridKind::Diagrid)).unwrap();
        let (nodes, bars) = (
            m.gridshell_nodes(a).unwrap().len(),
            m.gridshell_bars(a).unwrap().len(),
        );
        assert_eq!((nodes, bars), (12, 17));
        let txt = export_ifc(&m).unwrap();
        check_step(&txt).unwrap();
        assert_eq!(count(&txt, "IFCELEMENTASSEMBLY"), 1);
        assert_eq!(count(&txt, "IFCMEMBER"), bars);
        assert_eq!(count(&txt, "IFCDISCRETEACCESSORY"), nodes);
        let asm = txt
            .lines()
            .find(|l| l.contains("=IFCELEMENTASSEMBLY("))
            .unwrap();
        assert!(asm.contains("'DIAGRID'") && asm.contains(".USERDEFINED."), "{asm}");
        let id = &asm[..asm.find('=').unwrap()];
        assert_eq!(aggregated(&txt, id), bars + nodes);
        let node = first_args(&txt, "IFCDISCRETEACCESSORY");
        assert_eq!(node[4], "'NODE'");
        assert_eq!(node.last().unwrap(), ".USERDEFINED.");
        assert!(txt.contains("'BaseQuantities'"));
        // a cable net beside it: user-defined cable members in their own assembly
        m.add_gridshell(&spec(GridKind::CableNet)).unwrap();
        let txt = export_ifc(&m).unwrap();
        check_step(&txt).unwrap();
        assert_eq!(count(&txt, "IFCELEMENTASSEMBLY"), 2);
        assert!(txt.contains("'CABLE_NET'") && txt.contains("'CABLE'"));
        assert_eq!(count(&txt, "IFCMEMBER"), 2 * bars);
        assert_eq!(count(&txt, "IFCDISCRETEACCESSORY"), 2 * nodes);
    }

    /// The `geometry.gridshell` op (command, script and assistant path) exports like a grid
    /// built in the model: one assembly of every bar.
    #[test]
    fn gridshell_op_exports_its_bars() {
        use ddc_ops::{Actor, Envelope, GeoOp, Group, LatticeHost, LatticeKind, Len, Op, Ref, P3};
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let mut env = Envelope::new(Actor::Agent { name: "test".into() }, "grid");
        let op = GeoOp::Gridshell {
            slot: 0,
            level: Some(Ref::name(ddc_ops::Cat::Level, "L1")),
            host: LatticeHost::Vault {
                a: P3::m(0.0, 0.0, 0.0),
                b: P3::m(0.0, 24.0, 0.0),
                span: Len::m(16.0),
                rise: Len::m(5.0),
            },
            kind: Some(LatticeKind::Gridshell),
            pattern: None,
            count_u: 8,
            count_v: 6,
            supports: None,
            member: "CHS 88.9x5".into(),
            edge: Some("CHS 168.3x8".into()),
            node: Some(Len::mm(200.0)),
        };
        env.groups.push(Group { id: 1, title: "g".into(), ops: vec![Op::Geometry(op)], depends_on: vec![], why: String::new() });
        m.apply_envelope(&env).unwrap();
        let id = m.gridshells(None).unwrap()[0];
        let bars = m.gridshell_bars(id).unwrap().len();
        assert!(bars > 50, "{bars}");
        let txt = export_ifc(&m).unwrap();
        check_step(&txt).unwrap();
        assert_eq!(count(&txt, "IFCELEMENTASSEMBLY"), 1);
        assert_eq!(count(&txt, "IFCMEMBER"), bars);
        assert!(txt.contains("'GRIDSHELL'"));
    }
}
