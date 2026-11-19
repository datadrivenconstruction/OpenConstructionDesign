// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Generic solids -> IfcBuildingElementProxy (a sweep along a smooth path with a role — IfcBeam,
//! IfcColumn, IfcMember, IfcRailing): extrusion — IfcExtrudedAreaSolid, revolution —
//! IfcRevolvedAreaSolid (profiles with arcs — IfcIndexedPolyCurve), smooth sweep — an exact body
//! from `solids_sweep` where possible; other kinds — IfcTriangulatedFaceSet triangulation.
//! Voids — IfcOpeningElement + IfcRelVoidsElement to each cut host (wall,
//! slab, roof, solid — on any level; relations are written after all levels). Exact parameters — in `DDC_GenericSolid` (definition and JSON plane).
//! A solid with a semantic class (`SolidSpec::ifc`) is written as that class with its material
//! associated; parts of a recipe group nest in one IfcElementAssembly per group path.
//! A solid with a role (`solid_comp`, e.g. blade, gondola) carries it as its ObjectType and in
//! `DDC_ComponentRole`; the role's component row is not exported on its own.

use std::collections::{BTreeMap, HashSet};

use ddc_geom::arc::BulgeRing;
use ddc_geom::frame3::Role;
use ddc_geom::{v3, V2, V3};
use ddc_model::{form_json, Model, SolidForm, SolidFrame, SolidIfc, SolidPath, SolidSection};
use rusqlite::OptionalExtension;

#[path = "solids_sweep.rs"]
pub(super) mod sweep;

use super::{indexed_curve, list, qto, r, real, reals, s, text, Ctx, Geo, NONE};

pub(super) fn dir(c: &mut Ctx, v: V3) -> u32 {
    c.s.add("IFCDIRECTION", &[reals(&[v.x, v.y, v.z])])
}

/// Profile in work-plane coordinates.
pub(super) fn profile(c: &mut Ctx, p: &SolidSection) -> u32 {
    let loc = |q: V2| (q.x, q.y);
    let outer = indexed_curve(c, &p.outer, &loc);
    if p.holes.is_empty() {
        return c.s.add(
            "IFCARBITRARYCLOSEDPROFILEDEF",
            &[s(".AREA."), s(NONE), r(outer)],
        );
    }
    let holes: Vec<u32> = p
        .holes
        .iter()
        .map(|h: &BulgeRing| indexed_curve(c, h, &loc))
        .collect();
    c.s.add(
        "IFCARBITRARYPROFILEDEFWITHVOIDS",
        &[s(".AREA."), s(NONE), r(outer), list(&holes)],
    )
}

/// Right-handed system for IFC: z' = x × y (for a mirrored plane z' = −z).
pub(super) fn position(c: &mut Ctx, f: &SolidFrame, o: V3) -> u32 {
    let p = c.point3(o.x, o.y, o.z);
    let z = f.x.cross(f.y);
    let zd = dir(c, z);
    let xd = dir(c, f.x);
    c.s.add("IFCAXIS2PLACEMENT3D", &[r(p), r(zd), r(xd)])
}

/// Body: (representation item, representation type).
fn body(
    c: &mut Ctx,
    model: &Model,
    id: i64,
    f: &SolidFrame,
    form: &SolidForm,
    elev: f64,
) -> Result<(u32, &'static str), String> {
    let det = f.det();
    match form {
        SolidForm::Extrusion {
            profile: p,
            start,
            end,
        } => {
            let prof = profile(c, p);
            let pos = position(c, f, f.o + f.z * *start);
            let d = dir(c, v3(0.0, 0.0, det.signum()));
            let e = c.s.add(
                "IFCEXTRUDEDAREASOLID",
                &[r(prof), r(pos), r(d), real(end - start)],
            );
            return Ok((e, "SweptSolid"));
        }
        SolidForm::Revolve {
            profile: p,
            axis_o,
            axis_d,
            start,
            end,
        } if det > 0.0 && axis_d.len() > 0.0 => {
            let prof = profile(c, p);
            let l = axis_d.len();
            // a start angle turns the profile plane about the axis line, which it contains
            let pa = f.o + f.x * axis_o.x + f.y * axis_o.y;
            let d = (f.x * axis_d.x + f.y * axis_d.y) * (1.0 / l);
            let d = d * (1.0 / d.len());
            let (cs, sn) = (start.cos(), start.sin());
            let rot = |v: V3| v * cs + d.cross(v) * sn + d * (d.dot(v) * (1.0 - cs));
            let turned = SolidFrame {
                o: pa + rot(f.o - pa),
                x: rot(f.x),
                y: rot(f.y),
                z: rot(f.z),
            };
            let pos = position(c, &turned, turned.o);
            let ao = c.point3(axis_o.x, axis_o.y, 0.0);
            let ad = dir(c, v3(axis_d.x / l, axis_d.y / l, 0.0));
            let ax = c.s.add("IFCAXIS1PLACEMENT", &[r(ao), r(ad)]);
            let e = c.s.add(
                "IFCREVOLVEDAREASOLID",
                &[r(prof), r(pos), r(ax), real(end - start)],
            );
            return Ok((e, "SweptSolid"));
        }
        SolidForm::Sweep {
            profile: p,
            path: SolidPath {
                smooth: Some(sm), ..
            },
        } => {
            if sweep::tess_only() {
                super::adv::flattened();
            } else if let Some(e) = sweep::smooth_body(c, f, p, sm) {
                return Ok((e, "AdvancedSweptSolid"));
            }
        }
        SolidForm::Blend {
            bottom,
            top,
            height,
        } => {
            if let Some((k, t)) = ddc_geom::gsolid::blend_taper(bottom, top) {
                if sweep::tess_only() {
                    super::adv::flattened();
                } else {
                    let e = super::adv::tapered(c, f, bottom, k, t, *height);
                    return Ok((e, "AdvancedSweptSolid"));
                }
            }
        }
        _ => {}
    }
    let m = super::mep::finite_part(&model.solid_mesh(id).map_err(|e| e.to_string())?);
    let fs = super::adv::face_set(c, &m.positions, &m.triangles, elev);
    Ok((fs, "Tessellation"))
}

/// A void awaiting relations to hosts: (id, IfcOpeningElement, hosts (kind, id)).
pub(super) struct PendingVoid {
    id: i64,
    ent: u32,
    hosts: Vec<(String, i64)>,
}

/// IfcRelVoidsElement to each host written to the file (the host's GlobalId comes from `c.g`,
/// the same as for its entity, including one preserved on import).
pub(super) fn relate_voids(c: &mut Ctx, voids: &[PendingVoid]) {
    let ids: Vec<Vec<String>> = voids
        .iter()
        .map(|v| v.hosts.iter().map(|(k, h)| c.g.get(k, *h)).collect())
        .collect();
    let want: HashSet<String> = ids.iter().flatten().cloned().collect();
    let ents = c.s.find_rooted(&want);
    for (v, gs) in voids.iter().zip(&ids) {
        let mut k = 0i64;
        for g in gs {
            let Some(&h) = ents.get(g) else {
                continue;
            };
            c.rel(
                "IFCRELVOIDSELEMENT",
                "solid-void",
                v.id * 1024 + k,
                &[r(h), r(v.ent)],
            );
            k += 1;
        }
    }
}

/// Recipe parts waiting for their IfcElementAssembly: (recipe, group path) -> (level, parts).
/// A path `a/b` nests in `a`; the root assembly is contained in the storey of its first part.
#[derive(Default)]
pub(super) struct Assemblies {
    groups: BTreeMap<(i64, String), (i64, Vec<u32>)>,
    /// What each group holds, its sub-groups included: the assembly type comes from it.
    tags: BTreeMap<(i64, String), Vec<PartTag>>,
}

/// Class, predefined type and role (first word of the name) of an assembly part.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct PartTag {
    pub class: String,
    pub pre: String,
    pub role: String,
}

impl PartTag {
    fn of(ifc: Option<&SolidIfc>, name: &str) -> PartTag {
        PartTag {
            class: ifc.map(|k| k.class.clone()).unwrap_or_default(),
            pre: ifc.map(|k| k.predefined.clone()).unwrap_or_default(),
            role: name.split(' ').next().unwrap_or("").to_string(),
        }
    }
}

/// IfcElementAssemblyTypeEnum of a group, where its parts leave no doubt; else NOTDEFINED.
/// A truss holds only chords and web members, a girder only beams and plates around a
/// `girder`, a frame only columns, beams and (braced) bracing, a slab field only slabs.
pub(super) fn assembly_type(p: &[PartTag]) -> &'static str {
    let all = |f: &dyn Fn(&PartTag) -> bool| !p.is_empty() && p.iter().all(f);
    let any = |f: &dyn Fn(&PartTag) -> bool| p.iter().any(f);
    let is = |t: &PartTag, c: &str| t.class == c;
    let web = |t: &PartTag| is(t, "IfcMember") && matches!(t.pre.as_str(), "STRUT" | "POST");
    let brace = |t: &PartTag| is(t, "IfcMember") && matches!(t.pre.as_str(), "BRACE" | "STRUT");
    if all(&|t| is(t, "IfcMember"))
        && any(&|t| t.pre == "CHORD")
        && any(&|t| web(t))
    {
        return "TRUSS";
    }
    if all(&|t| is(t, "IfcBeam") || is(t, "IfcPlate"))
        && any(&|t| is(t, "IfcBeam") && t.role == "girder")
    {
        return "GIRDER";
    }
    let frame = |t: &PartTag| is(t, "IfcColumn") || is(t, "IfcBeam");
    let posts_and_beams = any(&|t| is(t, "IfcColumn")) && any(&|t| is(t, "IfcBeam"));
    if posts_and_beams && all(&|t| frame(t) || brace(t)) && any(&|t| brace(t)) {
        return "BRACED_FRAME";
    }
    if posts_and_beams && all(&|t| frame(t)) {
        return "RIGID_FRAME";
    }
    if p.len() >= 2 && all(&|t| is(t, "IfcSlab")) {
        return "SLAB_FIELD";
    }
    "NOTDEFINED"
}

impl Assemblies {
    fn add(&mut self, recipe: i64, path: &str, level: i64, ent: u32, tag: PartTag) {
        self.groups
            .entry((recipe, path.to_string()))
            .or_insert_with(|| (level, vec![]))
            .1
            .push(ent);
        self.tags.entry((recipe, path.to_string())).or_default().push(tag.clone());
        // every ancestor path exists, even without parts of its own
        let mut p = path;
        while let Some((up, _)) = p.rsplit_once('/') {
            self.groups.entry((recipe, up.to_string())).or_insert_with(|| (level, vec![]));
            self.tags.entry((recipe, up.to_string())).or_default().push(tag.clone());
            p = up;
        }
    }
}

/// Recipe group path of a solid ("tower/core" of key "tower/core/column#3"); None when the
/// solid came from no recipe or from no group.
fn group_path(model: &Model, id: i64) -> Result<Option<(i64, String)>, String> {
    let r = model.recipe_of("solid", id).map_err(|e| e.to_string())?;
    Ok(r.and_then(|(rec, key)| key.rsplit_once('/').map(|(p, _)| (rec, p.to_string()))))
}

/// One IfcElementAssembly per recipe group, parts and sub-assemblies by IfcRelAggregates; the
/// roots join the storey containment. `levels`, `storey_pls`, `contain` share one index.
pub(super) fn assemblies(
    c: &mut Ctx,
    a: &Assemblies,
    levels: &[i64],
    storey_pls: &[u32],
    contain: &mut [(u32, Vec<u32>)],
) {
    let mut ents: BTreeMap<(i64, &str), u32> = BTreeMap::new();
    for ((rec, path), (level, _)) in &a.groups {
        let Some(k) = levels.iter().position(|l| l == level) else { continue };
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(storey_pls[k]), ax);
        let name = path.rsplit('/').next().unwrap_or(path);
        let tags = a.tags.get(&(*rec, path.clone())).map_or(&[][..], Vec::as_slice);
        let ty = format!(".{}.", assembly_type(tags));
        let e = c.rooted(
            "IFCELEMENTASSEMBLY",
            &format!("solid-asm/{rec}/{path}"),
            0,
            name,
            &[s(NONE), r(pl), s(NONE), s(NONE), s(NONE), ty],
        );
        ents.insert((*rec, path.as_str()), e);
    }
    // children: own parts and the direct sub-assemblies, in path order
    for ((rec, path), (level, parts)) in &a.groups {
        let Some(&e) = ents.get(&(*rec, path.as_str())) else { continue };
        let mut kids = parts.clone();
        kids.extend(
            ents.iter()
                .filter(|((r2, p2), _)| {
                    r2 == rec && p2.rsplit_once('/').is_some_and(|(up, _)| up == path)
                })
                .map(|(_, &x)| x),
        );
        if !kids.is_empty() {
            c.rel(
                "IFCRELAGGREGATES",
                &format!("solid-asm-agg/{rec}/{path}"),
                0,
                &[r(e), list(&kids)],
            );
        }
        if !path.contains('/') {
            if let Some(k) = levels.iter().position(|l| l == level) {
                contain[k].1.push(e);
            }
        }
    }
}

/// Solids of a level; voids wait in `voids` for [`relate_voids`], recipe group parts in `asm`.
#[allow(clippy::too_many_arguments)]
pub(super) fn solids(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    voids: &mut Vec<PendingVoid>,
    asm: &mut Assemblies,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    let rows = model.solids(Some(level)).map_err(|e| e.to_string())?;
    let mut mats: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (id, sp) in rows {
        let Some(q) = model.solid_quantities(id).map_err(|e| e.to_string())? else {
            continue; // a body with a geometry error is not exported
        };
        let hosts: Vec<(String, i64)> = if sp.void {
            model
                .solid_cuts(id)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|k| (k.kind, k.id))
                .collect()
        } else {
            vec![]
        };
        if sp.void && hosts.is_empty() {
            continue;
        }
        let (item, rtype) = body(c, model, id, &sp.frame, &sp.form, elev)?;
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(g.body_ctx), text("Body"), text(rtype), list(&[item])],
        );
        let pds = c.s.add(
            "IFCPRODUCTDEFINITIONSHAPE",
            &[s(NONE), s(NONE), list(&[rep])],
        );
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let role: Option<String> = model
            .sql()
            .query_row(
                "SELECT f.name FROM solid_comp sc JOIN comp c ON c.id=sc.comp                  JOIN comp_type t ON t.id=c.ctype JOIN comp_family f ON f.id=t.family WHERE sc.solid=?1",
                [id],
                |x| x.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // ObjectType: the role where there is one, else the form kind
        let object_type = text(role.as_deref().unwrap_or(sp.form.kind()));
        let label = if sp.name.is_empty() {
            format!("Generic model {id}")
        } else {
            sp.name.clone()
        };
        let ent = if sp.void {
            let e = c.rooted(
                "IFCOPENINGELEMENT",
                "solid",
                id,
                &label,
                &[s(NONE), r(pl), r(pds), s(NONE), s(".OPENING.")],
            );
            voids.push(PendingVoid { id, ent: e, hosts });
            e
        } else {
            let e = match &sp.ifc {
                None => {
                    // a smooth sweep with a role names its class; anything else is a proxy
                    let (class, kind) = class_of(&sp.form);
                    c.rooted(
                        class,
                        "solid",
                        id,
                        &label,
                        &[object_type.clone(), r(pl), r(pds), s(NONE), s(kind)],
                    )
                }
                Some(k) => {
                    let mut rest = vec![object_type.clone(), r(pl), r(pds), s(NONE)];
                    // optional attributes before PredefinedType stay unset
                    let at = SolidIfc::predefined_at(&k.class);
                    rest.resize(at - 4, s(NONE));
                    rest.push(format!(".{}.", k.predefined));
                    c.rooted(&k.entity(), "solid", id, &label, &rest)
                }
            };
            if sp.ifc.is_some() && !sp.material.is_empty() {
                mats.entry(sp.material.clone()).or_default().push(e);
            }
            match group_path(model, id)? {
                Some((rec, path)) => {
                    asm.add(rec, &path, level, e, PartTag::of(sp.ifc.as_ref(), &sp.name))
                }
                None => out.push(e),
            }
            e
        };
        let label_v = |v: &str| format!("IFCLABEL({})", text(v));
        let ftext = |v: &str| format!("IFCTEXT({})", text(v));
        let fields = [
            ("Kind", label_v(sp.form.kind())),
            (
                "Void",
                format!("IFCBOOLEAN({})", if sp.void { ".T." } else { ".F." }),
            ),
            ("Material", label_v(&sp.material)),
            ("Definition", ftext(&form_json(&sp.form).to_string())),
            ("Plane", ftext(&plane_text(&sp.frame))),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| {
                c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)])
            })
            .collect();
        let set = c.rooted(
            "IFCPROPERTYSET",
            "solid-pset",
            id,
            "DDC_GenericSolid",
            &[list(&props)],
        );
        c.rel(
            "IFCRELDEFINESBYPROPERTIES",
            "solid-pset-rel",
            id,
            &[list(&[ent]), r(set)],
        );
        if let Some(role) = &role {
            let p = c.s.add("IFCPROPERTYSINGLEVALUE", &[text("Role"), s(NONE), label_v(role), s(NONE)]);
            let set = c.rooted("IFCPROPERTYSET", "solid-role-pset", id, "DDC_ComponentRole", &[list(&[p])]);
            c.rel("IFCRELDEFINESBYPROPERTIES", "solid-role-rel", id, &[list(&[ent]), r(set)]);
        }
        qto(
            c,
            g,
            "solid-qto",
            id,
            "BaseQuantities",
            ent,
            &[
                ("GrossVolume", 2, q.volume),
                ("NetVolume", 2, q.net_volume),
                ("GrossSurfaceArea", 1, q.area),
            ],
        );
    }
    // semantic parts carry their material as a real association, not only as a property
    for (m, ents) in mats {
        let mat = c.material(&m);
        c.rel(
            "IFCRELASSOCIATESMATERIAL",
            &format!("solid-matrel/{level}/{m}"),
            0,
            &[list(&ents), r(mat)],
        );
    }
    Ok(())
}

/// IFC class and predefined type: a smooth sweep carries the role of the element it stands for.
fn class_of(form: &SolidForm) -> (&'static str, &'static str) {
    let role = match form {
        SolidForm::Sweep {
            path: SolidPath {
                smooth: Some(sm), ..
            },
            ..
        } => sm.role,
        _ => Role::Generic,
    };
    match role {
        Role::Generic => ("IFCBUILDINGELEMENTPROXY", ".ELEMENT."),
        Role::Beam => ("IFCBEAM", ".BEAM."),
        Role::Column => ("IFCCOLUMN", ".COLUMN."),
        Role::Member => ("IFCMEMBER", ".MEMBER."),
        Role::Mullion => ("IFCMEMBER", ".MULLION."),
        Role::Railing => ("IFCRAILING", ".HANDRAIL."),
    }
}

fn plane_text(f: &SolidFrame) -> String {
    let v = |p: V3| format!("{:?} {:?} {:?}", p.x, p.y, p.z);
    format!("{} {} {} {}", v(f.o), v(f.x), v(f.y), v(f.z))
}

#[cfg(test)]
mod tests {
    use super::super::export_ifc;
    use ddc_geom::{v2, V2};
    use ddc_model::{LayerSpec, Model, SolidForm, SolidFrame, SolidSection, SolidSpec, WallSpec};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
        vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
    }

    fn count(t: &str, e: &str) -> usize {
        t.matches(&format!("={e}(")).count()
    }

    #[test]
    fn a_role_rides_on_its_solid_not_as_a_marker_proxy() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let solid = m
            .add_solid(&SolidSpec {
                level: l,
                name: "blade1/section1".into(),
                void: false,
                material: "steel".into(),
                ifc: None,
                frame: SolidFrame::default(),
                form: SolidForm::Extrusion { profile: SolidSection::polygon(rect(0.0, 0.0, 1.0, 1.0)), start: 0.0, end: 2.0 },
            })
            .unwrap();
        let fam = m
            .load_family("family Blade
category generic
host none
param Size length type 10mm
type \"Default\" Size=10mm
extrude Marker rect(-Size, -Size, Size, Size) z(0, Size)
")
            .unwrap();
        let ct = m.comp_types(fam).unwrap()[0].0;
        let comp = m.add_component(&ddc_model::CompSpec::at(l, ct, 0.5, 0.5)).unwrap();
        let unlinked = export_ifc(&m).unwrap();
        m.link_role_comp(solid, comp).unwrap();
        let t = export_ifc(&m).unwrap();
        // a component is a mapped family body; the extrusion is not
        assert!(unlinked.contains("=IFCMAPPEDITEM("), "the unlinked row exports as a component");
        assert!(!t.contains("=IFCMAPPEDITEM("), "the role's component row is not exported on its own");
        let el = t.lines().find(|x| x.contains("IFCBUILDINGELEMENTPROXY(") && x.contains("'blade1/section1'")).expect("the solid");
        assert!(el.contains("'Blade'"), "ObjectType is the role: {el}");
        assert!(t.contains("'DDC_ComponentRole'") && t.contains("IFCLABEL('Blade')"), "role property set");
        assert!(!unlinked.contains("DDC_ComponentRole"));
    }

    #[test]
    fn proxy_extrusion_revolve_sweep_and_void() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "w",
                &[LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        m.add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .unwrap();
        let spec = |form: SolidForm, void: bool| SolidSpec {
            level: l,
            name: String::new(),
            void,
            material: "steel".into(),
            ifc: None,
            frame: SolidFrame::default(),
            form,
        };
        m.add_solid(&spec(
            SolidForm::Extrusion {
                profile: SolidSection::polygon(rect(0.0, 2.0, 1.0, 3.0)),
                start: 0.0,
                end: 2.0,
            },
            false,
        ))
        .unwrap();
        m.add_solid(&spec(
            SolidForm::Revolve {
                profile: SolidSection::polygon(rect(1.0, 0.0, 2.0, 1.0)),
                axis_o: v2(0.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.0,
                end: 3.0,
            },
            false,
        ))
        .unwrap();
        m.add_solid(&spec(
            SolidForm::Sweep {
                profile: SolidSection::polygon(rect(-0.1, -0.1, 0.1, 0.1)),
                path: ddc_model::SolidPath::polyline(vec![
                    ddc_geom::v3(0.0, 5.0, 1.0),
                    ddc_geom::v3(4.0, 5.0, 1.0),
                    ddc_geom::v3(4.0, 8.0, 2.0),
                ]),
            },
            false,
        ))
        .unwrap();
        m.add_solid(&spec(
            SolidForm::Extrusion {
                profile: SolidSection::polygon(rect(2.0, -1.0, 3.0, 1.0)),
                start: 0.5,
                end: 1.5,
            },
            true,
        ))
        .unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCBUILDINGELEMENTPROXY"), 3);
        assert_eq!(count(&t, "IFCREVOLVEDAREASOLID"), 1);
        assert!(count(&t, "IFCTRIANGULATEDFACESET") >= 1);
        assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
        assert_eq!(count(&t, "IFCRELVOIDSELEMENT"), 1);
        assert!(t.contains("DDC_GenericSolid"));
    }

    /// Classed solids, one proxy and a recipe with a nested group: each class goes out as its
    /// own entity, the groups as assemblies, and an import brings the classes back exactly.
    #[test]
    fn semantic_classes_assemblies_and_reimport() {
        use crate::ifc::{export_ifc_with, import_ifc, validate_ifc, IfcOptions, IfcSchema};
        use ddc_model::{Recipe, RecipePart, SolidIfc};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let classes = [
            ("IfcMember", Some("CHORD")),
            ("IfcMember", Some("STRUT")),
            ("IfcPlate", Some("CURTAIN_PANEL")),
            ("IfcSlab", Some("FLOOR")),
            ("IfcColumn", Some("COLUMN")),
            ("IfcBeam", None),
            ("IfcCovering", Some("MEMBRANE")),
            ("IfcRailing", Some("HANDRAIL")),
            ("IfcFooting", Some("PAD_FOOTING")),
            ("IfcElementAssembly", Some("TRUSS")),
        ];
        let mut ids = vec![];
        for (k, (c, p)) in classes.iter().enumerate() {
            let x = k as f64 * 2.0;
            ids.push(
                m.add_solid(&SolidSpec {
                    level: l,
                    name: format!("part {k}"),
                    void: false,
                    material: if k % 2 == 0 { "S355" } else { "Glass" }.into(),
                    ifc: Some(SolidIfc::new(c, *p).unwrap()),
                    frame: SolidFrame::default(),
                    form: SolidForm::Extrusion {
                        profile: SolidSection::polygon(rect(x, 0.0, x + 1.0, 1.0)),
                        start: 0.0,
                        end: 1.0,
                    },
                })
                .unwrap(),
            );
        }
        let proxy = m
            .add_solid(&SolidSpec {
                level: l,
                name: "free".into(),
                void: false,
                material: String::new(),
                ifc: None,
                frame: SolidFrame::default(),
                form: SolidForm::Extrusion {
                    profile: SolidSection::polygon(rect(0.0, 5.0, 1.0, 6.0)),
                    start: 0.0,
                    end: 1.0,
                },
            })
            .unwrap();
        // bridge/deck holds two parts and the sub-group bridge/deck/rail one; part 5 stays loose
        let part = |key: &str, el: i64| RecipePart {
            key: key.into(),
            tbl: "solid".into(),
            el,
        };
        m.recipe_add(
            &Recipe {
                id: 0,
                name: "bridge".into(),
                script: "-- test".into(),
                params_json: "{}".into(),
                hash: "h".into(),
                version: 1,
            },
            &[
                part("bridge/deck/top_chord", ids[0]),
                part("bridge/deck/diagonal", ids[1]),
                part("bridge/deck/rail/handrail", ids[7]),
                part("bridge/column", ids[4]),
                part("loose", ids[5]),
            ],
        )
        .unwrap();
        let t = export_ifc(&m).unwrap();
        for e in [
            "IFCPLATE",
            "IFCSLAB",
            "IFCCOLUMN",
            "IFCBEAM",
            "IFCCOVERING",
            "IFCRAILING",
            "IFCFOOTING",
        ] {
            assert_eq!(count(&t, e), 1, "{e}");
        }
        assert_eq!(count(&t, "IFCMEMBER"), 2);
        assert!(t.contains(".CHORD.") && t.contains(".STRUT.") && t.contains(".MEMBRANE."));
        assert_eq!(count(&t, "IFCBUILDINGELEMENTPROXY"), 1);
        // one solid of class assembly + bridge, bridge/deck, bridge/deck/rail
        assert_eq!(count(&t, "IFCELEMENTASSEMBLY"), 4);
        let aggs = t.lines().filter(|x| x.contains("=IFCRELAGGREGATES(")).count();
        assert!(aggs >= 3 + 3, "project/site/building/storey and three groups: {aggs}");
        assert!(t.lines().any(|x| x.contains("=IFCRELASSOCIATESMATERIAL(")));
        let rep = validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{}", rep.brief(10));
        let opts = IfcOptions {
            schema: IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        let (t23, _) = export_ifc_with(&m, &opts).unwrap();
        let rep = validate_ifc(t23.as_bytes());
        assert!(rep.is_clean(), "IFC2X3: {}", rep.brief(10));
        // stats: only the classless solid is a proxy
        let p = std::env::temp_dir().join(format!("ddc-w1-cls-{}.ifc", std::process::id()));
        let st = super::super::sink::export_ifc_file(&m, &p, "model.ifc", None).unwrap();
        assert_eq!(st.proxies, 1);
        let _ = std::fs::remove_file(&p);

        let mut back = Model::open(":memory:").unwrap();
        let r = import_ifc(&mut back, t.as_bytes()).unwrap();
        assert_eq!(r.meshes, 0, "{r:?}");
        let mut got: Vec<(Option<SolidIfc>, String)> = back
            .solids(None)
            .unwrap()
            .into_iter()
            .map(|(_, s)| (s.ifc, s.material))
            .collect();
        let mut want: Vec<(Option<SolidIfc>, String)> = ids
            .iter()
            .chain([&proxy])
            .map(|id| {
                let s = m.solid(*id).unwrap();
                (s.ifc, s.material)
            })
            .collect();
        let key = |x: &(Option<SolidIfc>, String)| format!("{:?}", x);
        got.sort_by_key(key);
        want.sort_by_key(key);
        assert_eq!(got, want);
        let (a, b) = (m.solid(ids[3]).unwrap(), back.solids(None).unwrap());
        let s = b.iter().find(|(_, s)| s.name == "part 3").expect("slab part back").1.clone();
        assert_eq!((s.form, s.frame), (a.form, a.frame));
        // and out again: the same classes
        let t2 = export_ifc(&back).unwrap();
        for e in ["IFCMEMBER", "IFCPLATE", "IFCSLAB", "IFCBUILDINGELEMENTPROXY"] {
            assert_eq!(count(&t2, e), count(&t, e), "{e}");
        }
    }

    #[test]
    fn assembly_type_only_where_the_parts_leave_no_doubt() {
        use super::{assembly_type, PartTag};
        let t = |class: &str, pre: &str, role: &str| PartTag {
            class: class.into(),
            pre: pre.into(),
            role: role.into(),
        };
        let chord = t("IfcMember", "CHORD", "top_chord");
        let diag = t("IfcMember", "STRUT", "diagonal");
        let post = t("IfcMember", "POST", "vertical");
        let brace = t("IfcMember", "BRACE", "brace");
        let col = t("IfcColumn", "COLUMN", "column");
        let beam = t("IfcBeam", "BEAM", "beam");
        let girder = t("IfcBeam", "BEAM", "girder");
        let plate = t("IfcPlate", "SHEET", "plate");
        let slab = t("IfcSlab", "FLOOR", "slab");
        let proxy = t("", "", "blob");
        let rail = t("IfcRailing", "HANDRAIL", "handrail");
        let cases: Vec<(Vec<&PartTag>, &str)> = vec![
            (vec![&chord, &diag, &post], "TRUSS"),
            (vec![&chord, &post], "TRUSS"),
            (vec![&chord, &diag, &slab], "NOTDEFINED"),
            (vec![&chord], "NOTDEFINED"),
            (vec![&girder, &plate], "GIRDER"),
            (vec![&beam, &plate], "NOTDEFINED"),
            (vec![&col, &beam, &brace], "BRACED_FRAME"),
            (vec![&col, &beam, &diag], "BRACED_FRAME"),
            (vec![&col, &beam], "RIGID_FRAME"),
            (vec![&col, &beam, &rail], "NOTDEFINED"),
            (vec![&slab, &slab], "SLAB_FIELD"),
            (vec![&slab], "NOTDEFINED"),
            (vec![&slab, &proxy], "NOTDEFINED"),
            (vec![], "NOTDEFINED"),
        ];
        for (parts, want) in cases {
            let p: Vec<PartTag> = parts.into_iter().cloned().collect();
            assert_eq!(assembly_type(&p), want, "{p:?}");
        }
    }

    /// Shading devices, fasteners and typed assemblies: IFC4X3 and IFC2X3 validate clean, the
    /// IFC2X3 shading device is a proxy naming it, and both files import back to the classes.
    #[test]
    fn shading_fasteners_and_typed_assemblies() {
        use crate::ifc::{export_ifc_with, import_ifc, validate_ifc, IfcOptions, IfcSchema};
        use ddc_model::{Recipe, RecipePart, SolidIfc};
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let mut add = |k: usize, name: &str, class: &str, pre: Option<&str>| {
            let x = k as f64 * 2.0;
            m.add_solid(&SolidSpec {
                level: l,
                name: name.into(),
                void: false,
                material: "Steel".into(),
                ifc: Some(SolidIfc::new(class, pre).unwrap()),
                frame: SolidFrame::default(),
                form: SolidForm::Extrusion {
                    profile: SolidSection::polygon(rect(x, 0.0, x + 1.0, 1.0)),
                    start: 0.0,
                    end: 1.0,
                },
            })
            .unwrap()
        };
        add(0, "louvre a", "IfcShadingDevice", Some("JALOUSIE"));
        add(1, "fin a", "IfcShadingDevice", None);
        add(2, "bolt M20", "IfcMechanicalFastener", Some("BOLT"));
        add(3, "anchor M24", "IfcMechanicalFastener", Some("ANCHORBOLT"));
        let chord = add(4, "top_chord a", "IfcMember", Some("CHORD"));
        let web = add(5, "diagonal a", "IfcMember", Some("STRUT"));
        let s1 = add(6, "slab a", "IfcSlab", Some("FLOOR"));
        let s2 = add(7, "slab b", "IfcSlab", Some("FLOOR"));
        let part = |key: &str, el: i64| RecipePart {
            key: key.into(),
            tbl: "solid".into(),
            el,
        };
        m.recipe_add(
            &Recipe {
                id: 0,
                name: "kit".into(),
                script: "-- test".into(),
                params_json: "{}".into(),
                hash: "h".into(),
                version: 1,
            },
            &[
                part("truss/top_chord", chord),
                part("truss/diagonal", web),
                part("floor/slab", s1),
                part("floor/slab#2", s2),
            ],
        )
        .unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCSHADINGDEVICE"), 2);
        assert_eq!(count(&t, "IFCMECHANICALFASTENER"), 2);
        assert!(t.contains(".JALOUSIE.") && t.contains(".BOLT.") && t.contains(".ANCHORBOLT."));
        let asm: Vec<&str> = t.lines().filter(|x| x.contains("=IFCELEMENTASSEMBLY(")).collect();
        assert_eq!(asm.len(), 2, "{asm:?}");
        assert!(asm.iter().any(|x| x.contains("'truss'") && x.ends_with(".TRUSS.);")), "{asm:?}");
        assert!(asm.iter().any(|x| x.contains("'floor'") && x.ends_with(".SLAB_FIELD.);")), "{asm:?}");
        let rep = validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{}", rep.brief(10));
        let opts = IfcOptions {
            schema: IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        let (t23, _) = export_ifc_with(&m, &opts).unwrap();
        let rep = validate_ifc(t23.as_bytes());
        assert!(rep.is_clean(), "IFC2X3: {}", rep.brief(10));
        assert_eq!(count(&t23, "IFCSHADINGDEVICE"), 0);
        assert!(t23.contains("'IfcShadingDevice.JALOUSIE'") && t23.contains("'IfcShadingDevice'"));
        assert_eq!(count(&t23, "IFCMECHANICALFASTENER"), 2);

        let classes = |m: &Model| {
            let mut v: Vec<(String, Option<SolidIfc>)> =
                m.solids(None).unwrap().into_iter().map(|(_, s)| (s.name, s.ifc)).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        let want = classes(&m);
        assert_eq!(want.iter().filter(|x| x.1.is_some()).count(), 8);
        for (schema, text) in [("IFC4X3", &t), ("IFC2X3", &t23)] {
            let mut back = Model::open(":memory:").unwrap();
            let r = import_ifc(&mut back, text.as_bytes()).unwrap();
            assert_eq!(r.meshes, 0, "{schema}: {r:?}");
            let got = classes(&back);
            if schema == "IFC4X3" {
                assert_eq!(got, want);
            } else {
                // IFC2X3 has no predefined type on members and fasteners; the class stays
                let bare = |v: &[(String, Option<SolidIfc>)]| -> Vec<(String, Option<String>)> {
                    v.iter().map(|(n, k)| (n.clone(), k.as_ref().map(|k| k.class.clone()))).collect()
                };
                assert_eq!(bare(&got), bare(&want), "{schema}");
                let l = got.iter().find(|x| x.0 == "louvre a").unwrap();
                assert_eq!(l.1.as_ref().unwrap().predefined, "JALOUSIE");
            }
        }
    }

    #[test]
    fn void_relates_to_slab_and_wall_on_later_level() {
        let mut m = Model::open(":memory:").unwrap();
        let l1 = m.add_level("L1", 0.0).unwrap();
        let l2 = m.add_level("L2", 3.0).unwrap();
        let layer = |t: f64| {
            vec![LayerSpec {
                material: "c".into(),
                thickness: t,
            }]
        };
        let st = m.add_slab_type("floor", &layer(0.2)).unwrap();
        m.add_slab(&ddc_model::SlabSpec {
            level: l1,
            slab_type: st,
            offset: 0.0,
            outer: vec![(0.0, -5.0), (10.0, -5.0), (10.0, 5.0), (0.0, 5.0)],
            holes: vec![],
        })
        .unwrap();
        let wt = m.add_wall_type("w", &layer(0.2)).unwrap();
        m.add_walls(&[WallSpec {
            a: (0.0, 0.0),
            b: (6.0, 0.0),
            level: l2,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        }])
        .unwrap();
        // a void on L1 reaching up to L2: cuts the L1 slab and the L2 wall
        m.add_solid(&SolidSpec {
            level: l1,
            name: String::new(),
            void: true,
            material: String::new(),
            ifc: None,
            frame: SolidFrame::default(),
            form: SolidForm::Extrusion {
                profile: SolidSection::polygon(rect(2.0, -1.0, 3.0, 1.0)),
                start: -1.0,
                end: 4.0,
            },
        })
        .unwrap();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCOPENINGELEMENT"), 1);
        assert_eq!(count(&t, "IFCRELVOIDSELEMENT"), 2);
        // voids find their hosts the same way when the lines are not kept
        let p = std::env::temp_dir().join(format!("ddc-o5-void-{}.ifc", std::process::id()));
        super::super::sink::export_ifc_file(&m, &p, "model.ifc", None).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), t);
    }

    use ddc_geom::frame3::{FrameMode, Law, Role, Seg, Smooth};
    use ddc_geom::v3;
    use std::f64::consts::PI;

    fn smooth_solid(m: &mut Model, l: i64, profile: SolidSection, sm: Smooth) -> i64 {
        m.add_solid(&SolidSpec {
            level: l,
            name: String::new(),
            void: false,
            material: "steel".into(),
            frame: SolidFrame::default(),
            form: SolidForm::Sweep {
                profile,
                path: ddc_model::SolidPath::smooth(sm),
            },
            ifc: None,
        })
        .unwrap()
    }

    fn tube() -> SolidSection {
        SolidSection {
            outer: ddc_geom::arc::BulgeRing {
                pts: vec![v2(0.0212, 0.0), v2(-0.0212, 0.0)],
                bulges: vec![1.0, 1.0],
            },
            holes: vec![ddc_geom::arc::BulgeRing {
                pts: vec![v2(0.018, 0.0), v2(-0.018, 0.0)],
                bulges: vec![-1.0, -1.0],
            }],
        }
    }

    fn helix(turns: f64, rise: f64) -> Smooth {
        Smooth::new(
            v3(1.0, 0.0, 0.9),
            vec![Seg::Helix {
                center: v3(0.0, 0.0, 0.9),
                axis: v3(0.0, 0.0, 1.0),
                angle: 2.0 * PI * turns,
                rise,
            }],
        )
    }

    /// Curved beam, spiral handrail, spline mullion and a twisted bar.
    fn curved_model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let mut beam = Smooth::new(
            v3(0.0, 0.0, 3.0),
            vec![Seg::Arc {
                via: v3(5.0, 1.0, 3.0),
                to: v3(10.0, 0.0, 3.0),
            }],
        );
        beam.role = Role::Beam;
        smooth_solid(&mut m, l, SolidSection::polygon(rect(-0.1, -0.15, 0.1, 0.15)), beam);
        let mut rail = helix(1.5, 3.0);
        rail.frame = FrameMode::Fixed(v3(0.0, 0.0, 1.0));
        rail.role = Role::Railing;
        smooth_solid(&mut m, l, tube(), rail);
        let mut mullion = Smooth::new(
            v3(20.0, 0.0, 0.0),
            vec![Seg::Spline {
                via: vec![v3(20.5, 0.3, 1.5), v3(20.5, 0.3, 3.0)],
                to: v3(20.0, 0.0, 4.5),
            }],
        );
        mullion.frame = FrameMode::Fixed(v3(1.0, 0.0, 0.0));
        mullion.role = Role::Mullion;
        smooth_solid(&mut m, l, SolidSection::polygon(rect(-0.03, -0.08, 0.03, 0.08)), mullion);
        let mut bar = Smooth::new(v3(0.0, 10.0, 1.0), vec![Seg::Line { to: v3(5.0, 10.0, 1.0) }]);
        bar.twist = Law::Linear(0.0, PI / 2.0);
        smooth_solid(&mut m, l, SolidSection::polygon(rect(-0.1, -0.05, 0.1, 0.05)), bar);
        m
    }

    #[test]
    fn smooth_sweeps_export_exact_and_valid() {
        let m = curved_model();
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCBEAM"), 1);
        assert_eq!(count(&t, "IFCRAILING"), 1);
        assert_eq!(count(&t, "IFCMEMBER"), 1);
        assert!(t.contains(".HANDRAIL.") && t.contains(".MULLION."));
        assert_eq!(count(&t, "IFCSWEPTDISKSOLID"), 1, "the tube on the helix");
        assert_eq!(count(&t, "IFCFIXEDREFERENCESWEPTAREASOLID"), 2, "beam and mullion");
        assert!(count(&t, "IFCBSPLINECURVEWITHKNOTS") >= 2, "helix and spline directrices");
        assert!(count(&t, "IFCINDEXEDPOLYCURVE") >= 1, "the arc directrix");
        assert_eq!(count(&t, "IFCTRIANGULATEDFACESET"), 1, "the twisted bar");
        let rep = crate::ifc::validate_ifc(t.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
        let old = crate::ifc::IfcOptions {
            schema: crate::ifc::IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        };
        let (t2, _) = crate::ifc::export_ifc_with(&m, &old).unwrap();
        assert!(!t2.contains("IFCFIXEDREFERENCESWEPTAREASOLID"));
        assert!(!t2.contains("BSPLINECURVEWITHKNOTS"));
        let rep = crate::ifc::validate_ifc(t2.as_bytes());
        assert!(rep.is_clean(), "{:?}", rep.errors);
        // the flag does not leak into later exports
        assert_eq!(export_ifc(&m).unwrap(), t);
    }

    fn mesh_volume(p: &crate::ifc::PMesh) -> f64 {
        let v = |i: u32| {
            let q = p.positions[i as usize];
            ddc_geom::v3(q[0], q[1], q[2])
        };
        p.indices
            .chunks(3)
            .map(|t| v(t[0]).dot(v(t[1]).cross(v(t[2]))))
            .sum::<f64>()
            .abs()
            / 6.0
    }

    #[test]
    fn exact_sweeps_read_back_with_their_volume() {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let prof = || SolidSection::polygon(rect(-0.1, -0.15, 0.1, 0.15));
        let arc = Smooth::new(
            v3(0.0, 0.0, 3.0),
            vec![Seg::Arc {
                via: v3(5.0, 1.0, 3.0),
                to: v3(10.0, 0.0, 3.0),
            }],
        );
        let mut rail = helix(1.0, 2.0);
        rail.frame = FrameMode::Fixed(v3(0.0, 0.0, 1.0));
        let ids = [
            smooth_solid(&mut m, l, prof(), arc),
            smooth_solid(&mut m, l, prof(), rail),
        ];
        let t = export_ifc(&m).unwrap();
        assert_eq!(count(&t, "IFCFIXEDREFERENCESWEPTAREASOLID"), 2);
        let plan = crate::ifc::plan_bytes(t.as_bytes()).unwrap();
        assert_eq!(plan.meshes.len(), 2, "{:?}", plan.report.skipped);
        let sorted = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v
        };
        let want = sorted(ids.iter().map(|i| m.solid_quantities(*i).unwrap().unwrap().volume).collect());
        let got = sorted(plan.meshes.iter().map(mesh_volume).collect());
        for (g, w) in got.iter().zip(&want) {
            assert!((g - w).abs() < 0.01 * w, "{got:?} vs {want:?}");
        }
    }
}
