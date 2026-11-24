// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Columns, beams, roofs, ceilings and imported meshes -> IfcColumn / IfcBeam / IfcRoof /
//! IfcCovering(.CEILING.) / IfcBuildingElementProxy. Members, roofs and meshes carry a
//! triangulated body; a ceiling is a plan profile swept down from its top face. The exact model
//! rows go to `DDC_Member` / `DDC_Roof` / `DDC_Mesh`, so a round trip is exact.

use std::collections::BTreeMap;

use ddc_geom::mesh::Mesh;
use ddc_model::{ImportedMesh, MemberShape, Model, RoofKind};

use super::mep::{body, pset};
use super::{db, profile_def, r, real, s, swept, text, Ctx, Geo, NONE};

/// Model-wide data read once for all levels.
pub(super) struct Extra {
    members: Vec<(&'static str, i64, i64, Mesh)>,
    meshes: Vec<ImportedMesh>,
    types: BTreeMap<i64, (String, MemberShape, String)>,
    levels: BTreeMap<i64, String>,
    first: Option<i64>,
}

pub(super) fn prepare(model: &Model) -> Result<Extra, String> {
    let e = |e: ddc_model::Error| e.to_string();
    let sql = model.sql();
    let levels: BTreeMap<i64, String> = {
        let mut st = sql
            .prepare("SELECT id, name FROM level ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    Ok(Extra {
        members: model.member_meshes().map_err(e)?,
        meshes: model.imported_meshes(None).map_err(e)?,
        types: model
            .member_types()
            .map_err(e)?
            .into_iter()
            .map(|(id, n, sh, m)| (id, (n, sh, m)))
            .collect(),
        first: levels.keys().next().copied(),
        levels,
    })
}

fn num(v: f64) -> String {
    format!("{v:?}")
}

fn opt(v: Option<f64>) -> String {
    v.map_or("-".into(), num)
}

fn ftext(v: &str) -> String {
    format!("IFCTEXT({})", text(v))
}

/// Element with an optional triangulated body placed at the storey origin.
#[allow(clippy::too_many_arguments)]
fn element(
    c: &mut Ctx,
    g: &Geo,
    class: &str,
    kind: &str,
    id: i64,
    name: &str,
    object_type: Option<&str>,
    predefined: &str,
    mesh: Option<&Mesh>,
    elev: f64,
) -> u32 {
    let pds = mesh
        .filter(|m| !m.triangles.is_empty())
        .map(|m| body(c, g, m, elev));
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    let pl = c.placement(Some(g.storey_pl), ax);
    c.rooted(
        class,
        kind,
        id,
        name,
        &[
            object_type.map_or(s(NONE), text),
            r(pl),
            pds.map_or(s(NONE), r),
            s(NONE),
            s(predefined),
        ],
    )
}

/// Element classes a body-only element is written back as: the IFC4 attribute list ends with
/// Tag, PredefinedType (the IFC2X3 writer cuts or renames the ones that differ there).
const MESH_CLASSES: [&str; 16] = [
    "IFCSTAIR",
    "IFCRAILING",
    "IFCRAMP",
    "IFCROOF",
    "IFCSLAB",
    "IFCWALL",
    "IFCCOLUMN",
    "IFCBEAM",
    "IFCMEMBER",
    "IFCPLATE",
    "IFCCOVERING",
    "IFCFOOTING",
    "IFCCURTAINWALL",
    "IFCCHIMNEY",
    "IFCSHADINGDEVICE",
    "IFCBUILDINGELEMENTPROXY",
];

/// IFC class, predefined type and object type of a stored mesh: the class it was imported
/// from (`IFC.Class`), else the class of its category, else a proxy.
fn mesh_class(
    sql: &rusqlite::Connection,
    id: i64,
    category: &str,
) -> (&'static str, String, Option<String>) {
    let prop = |key: &str| -> Option<String> {
        sql.query_row(
            "SELECT value FROM prop WHERE kind='mesh' AND ref=?1 AND key=?2",
            rusqlite::params![id, key],
            |r| r.get::<_, rusqlite::types::Value>(0),
        )
        .ok()
        .and_then(|v| match v {
            rusqlite::types::Value::Text(t) => Some(t),
            _ => None,
        })
    };
    let src = prop("IFC.Class").map(|c| {
        let c = c.to_ascii_uppercase();
        match c.strip_suffix("STANDARDCASE") {
            Some(b) => b.to_string(),
            None => c,
        }
    });
    let class = src
        .as_deref()
        .and_then(|c| MESH_CLASSES.iter().find(|k| **k == c))
        .or_else(|| {
            MESH_CLASSES
                .iter()
                .find(|k| crate::ifc::import::category(k) == category)
        })
        .copied()
        .unwrap_or("IFCBUILDINGELEMENTPROXY");
    let same = src.as_deref() == Some(class);
    let predefined = prop("IFC.PredefinedType")
        .filter(|p| {
            same && !p.is_empty() && p.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        })
        .unwrap_or_else(|| "NOTDEFINED".into());
    let object_type = prop("IFC.ObjectType").filter(|t| same && !t.is_empty());
    (class, format!(".{predefined}."), object_type)
}

fn layer_usage(c: &mut Ctx, kind: &str, id: i64, ent: u32, set: u32) {
    let usage = c.s.add(
        "IFCMATERIALLAYERSETUSAGE",
        &[r(set), s(".AXIS3."), s(".NEGATIVE."), real(0.0), s(NONE)],
    );
    c.rel(
        "IFCRELASSOCIATESMATERIAL",
        kind,
        id,
        &[super::list(&[ent]), r(usage)],
    );
}

/// Columns, beams, roofs, ceilings and meshes of `level`.
#[allow(clippy::too_many_arguments)]
pub(super) fn all(
    c: &mut Ctx,
    model: &Model,
    x: &Extra,
    sets: &BTreeMap<i64, (u32, f64)>,
    level: i64,
    elev: f64,
    g: &Geo,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    let e = |e: ddc_model::Error| e.to_string();
    let sql = model.sql();
    for (kind, id, lv, mesh) in &x.members {
        if *lv != level {
            continue;
        }
        let (table, class, pre, mtype, mut fields, bearing) = if *kind == "column" {
            let sp = model.column(*id).map_err(e)?;
            let (tx, ty) = sp.top_at.unzip();
            let spec = [
                num(sp.at.0),
                num(sp.at.1),
                num(sp.base_offset),
                num(sp.top_offset),
                num(sp.rotation),
                (sp.structural as i64).to_string(),
                opt(tx),
                opt(ty),
            ]
            .join(" ");
            let top = sp
                .top_level
                .and_then(|t| x.levels.get(&t))
                .cloned()
                .unwrap_or_default();
            (
                "col",
                "IFCCOLUMN",
                ".COLUMN.",
                sp.member_type,
                vec![("Spec", ftext(&spec)), ("TopLevel", ftext(&top))],
                sp.structural,
            )
        } else {
            let sp = model.beam(*id).map_err(e)?;
            let spec = [
                sp.a.0,
                sp.a.1,
                sp.a.2,
                sp.b.0,
                sp.b.1,
                sp.b.2,
                sp.bulge,
                sp.yjust.to_i() as f64,
                sp.zjust.to_i() as f64,
                sp.rotation,
                sp.start_ext,
                sp.end_ext,
            ]
            .map(num)
            .join(" ");
            (
                "beam",
                "IFCBEAM",
                ".BEAM.",
                sp.member_type,
                vec![
                    ("Spec", ftext(&spec)),
                    ("JoinStart", ftext(sp.join_start.text())),
                    ("JoinEnd", ftext(sp.join_end.text())),
                ],
                true,
            )
        };
        let (tname, shape, mat) = x.types.get(&mtype).ok_or("member type missing")?;
        let ent = element(
            c,
            g,
            class,
            table,
            *id,
            tname,
            Some(tname),
            pre,
            Some(mesh),
            elev,
        );
        let mut all = vec![
            ("Kind", ftext(table)),
            ("Type", ftext(tname)),
            ("Shape", ftext(&shape.text())),
            ("Material", ftext(mat)),
        ];
        all.append(&mut fields);
        pset(c, &format!("{table}-pset"), *id, "DDC_Member", ent, all);
        let (common, qname) = if *kind == "column" {
            ("Pset_ColumnCommon", "Qto_ColumnBaseQuantities")
        } else {
            ("Pset_BeamCommon", "Qto_BeamBaseQuantities")
        };
        let flag = |v: bool| format!("IFCBOOLEAN({})", if v { ".T." } else { ".F." });
        pset(
            c,
            &format!("{table}-common"),
            *id,
            common,
            ent,
            vec![
                ("Reference", format!("IFCIDENTIFIER({})", text(tname))),
                ("IsExternal", flag(false)),
                ("LoadBearing", flag(bearing)),
            ],
        );
        let (area, vol) = (mesh.area(), mesh.volume().abs());
        if area.is_finite() && vol.is_finite() {
            let q = [
                ("GrossSurfaceArea", 1, area),
                ("NetSurfaceArea", 1, area),
                ("GrossVolume", 2, vol),
                ("NetVolume", 2, vol),
            ];
            super::qto(c, g, &format!("{table}-qto"), *id, qname, ent, &q);
        }
        out.push(ent);
    }

    let ids = |q: &str| -> Result<Vec<i64>, String> {
        let mut st = sql.prepare(q).or_else(db)?;
        let rows = st.query_map([level], |x| x.get(0)).or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)
    };
    for id in ids("SELECT id FROM roof WHERE level=?1 ORDER BY id")? {
        let sp = model.roof(id).map_err(e)?;
        let mesh = match model.roof_mesh(id) {
            Ok(m) if !m.triangles.is_empty() => Some(m),
            Ok(_) => {
                c.g.warnings
                    .push(format!("roof {id}: no geometry exported (empty mesh)"));
                None
            }
            Err(err) => {
                c.g.warnings
                    .push(format!("roof {id}: no geometry exported ({err})"));
                None
            }
        };
        let ent = element(
            c,
            g,
            "IFCROOF",
            "roof",
            id,
            &format!("Roof {id}"),
            None,
            ".NOTDEFINED.",
            mesh.as_ref(),
            elev,
        );
        c.roof_ent.insert(id, ent);
        if let Some(&(set, _)) = sets.get(&sp.slab_type) {
            layer_usage(c, "roof-mat", id, ent, set);
        }
        let mut fields = vec![(
            "Spec",
            ftext(&[sp.offset, sp.overhang, sp.fascia].map(num).join(" ")),
        )];
        match &sp.kind {
            RoofKind::Footprint {
                ring,
                slopes,
                mansard,
            } => {
                let v = ring
                    .iter()
                    .zip(slopes)
                    .map(|(p, sl)| format!("{} {} {}", num(p.0), num(p.1), opt(*sl)))
                    .collect::<Vec<_>>()
                    .join(";");
                let m = mansard.map_or(String::new(), |(h, d)| format!("{} {}", num(h), opt(d)));
                fields.extend([
                    ("Kind", ftext("footprint")),
                    ("Ring", ftext(&v)),
                    ("Mansard", ftext(&m)),
                ]);
            }
            RoofKind::Extrusion {
                a,
                b,
                profile,
                depth,
            } => {
                let p = profile
                    .iter()
                    .map(|q| format!("{} {}", num(q.0), num(q.1)))
                    .collect::<Vec<_>>()
                    .join(";");
                let axis = [a.0, a.1, b.0, b.1, *depth].map(num).join(" ");
                fields.extend([
                    ("Kind", ftext("extrusion")),
                    ("Axis", ftext(&axis)),
                    ("Profile", ftext(&p)),
                ]);
            }
            RoofKind::Arrow {
                ring,
                tail,
                head,
                rise,
            } => {
                let v = ring
                    .iter()
                    .map(|p| format!("{} {}", num(p.0), num(p.1)))
                    .collect::<Vec<_>>()
                    .join(";");
                let arrow = [tail.0, tail.1, head.0, head.1, *rise].map(num).join(" ");
                fields.extend([
                    ("Kind", ftext("arrow")),
                    ("Ring", ftext(&v)),
                    ("Arrow", ftext(&arrow)),
                ]);
            }
        }
        pset(c, "roof-pset", id, "DDC_Roof", ent, fields);
        out.push(ent);
    }

    let rows: Vec<(i64, i64, f64)> = {
        let mut st = sql
            .prepare("SELECT id, stype, height FROM ceiling WHERE level=?1 ORDER BY id")
            .or_else(db)?;
        let rows = st
            .query_map([level], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))
            .or_else(db)?;
        rows.collect::<Result<_, _>>().or_else(db)?
    };
    for (id, stype, height) in rows {
        let &(set, total) = sets.get(&stype).ok_or("ceiling type missing")?;
        let rings = model.ceiling_rings(id).map_err(e)?;
        if rings.is_empty() || !height.is_finite() || total.is_nan() || total <= 0.0 {
            continue;
        }
        // top face at height + thickness, swept down: the bottom lands exactly on `height`
        let p = c.point3(0.0, 0.0, height + total);
        let ax = c.axis3(p, None);
        let spl = c.placement(Some(g.storey_pl), ax);
        let prof = profile_def(c, &rings, (0.0, 0.0));
        let pds = swept(c, g, prof, g.dir_down, total);
        let ent = c.rooted(
            "IFCCOVERING",
            "ceiling",
            id,
            &format!("Ceiling {id}"),
            &[s(NONE), r(spl), r(pds), s(NONE), s(".CEILING.")],
        );
        layer_usage(c, "ceiling-mat", id, ent, set);
        out.push(ent);
    }

    for m in &x.meshes {
        if m.level.or(x.first) != Some(level) {
            continue;
        }
        let (class, predefined, object_type) = mesh_class(sql, m.id, &m.category);
        let ent = element(
            c,
            g,
            class,
            "mesh",
            m.id,
            &m.name,
            Some(object_type.as_deref().unwrap_or(&m.category)),
            &predefined,
            Some(&m.mesh),
            elev,
        );
        pset(
            c,
            "mesh-pset",
            m.id,
            "DDC_Mesh",
            ent,
            vec![("Category", ftext(&m.category))],
        );
        out.push(ent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::ifc::export_ifc_with_warnings;
    use ddc_model::{LayerSpec, Model, RoofKind, RoofSpec};

    fn roof_model() -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l = m.add_level("L1", 0.0).unwrap();
        let st = m
            .add_slab_type(
                "roof",
                &[LayerSpec {
                    material: "deck".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let roof = m
            .add_roof(&RoofSpec {
                level: l,
                slab_type: st,
                offset: 3.0,
                overhang: 0.5,
                fascia: 0.0,
                kind: RoofKind::Footprint {
                    ring: vec![(0.0, 0.0), (10.0, 0.0), (10.0, 6.0), (0.0, 6.0)],
                    slopes: vec![Some(30.0); 4],
                    mansard: None,
                },
            })
            .unwrap();
        (m, roof)
    }

    #[test]
    fn good_roof_no_warning() {
        let (m, _) = roof_model();
        let (t, w) = export_ifc_with_warnings(&m).unwrap();
        assert!(w.iter().all(|x| !x.contains("roof")), "{w:?}");
        assert_eq!(t.matches("=IFCROOF(").count(), 1);
        assert!(t.contains("IFCPRODUCTDEFINITIONSHAPE"));
    }

    #[test]
    fn broken_roof_export_warns() {
        let (m, roof) = roof_model();
        m.sql()
            .execute("UPDATE roof_vertex SET x=0, y=0 WHERE roof=?1", [roof])
            .unwrap();
        // the tamper must really break the mesh, otherwise the test proves nothing
        assert!(m.roof_mesh(roof).map_or(true, |x| x.triangles.is_empty()));
        let (t, w) = export_ifc_with_warnings(&m).unwrap();
        let tag = format!("roof {roof}: no geometry exported");
        assert!(w.iter().any(|x| x.contains(&tag)), "{w:?}");
        // the element is kept, without a body
        assert_eq!(t.matches("=IFCROOF(").count(), 1);
        assert!(!t.contains("IFCPRODUCTDEFINITIONSHAPE"));
    }
}
