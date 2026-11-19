//! Roofs and slabs on freeform surfaces -> IfcRoof (.FREEFORM.) / IfcSlab (.FLOOR.). An
//! untrimmed open patch without openings is written as an exact IfcAdvancedBrep of the
//! whole stack; every element also gets one closed triangulated set per layer ('Body' or
//! 'Body-FallBack'). The layers go out as an IfcMaterialLayerSet in stack order.

use ddc_model::shell_elem::{mesh_of, ShellElemSpec, ShellKind};
use ddc_model::{FreeformSpec, Model};

use super::freeform::{exact, logical};
use super::{list, qto, r, real, reals, s, text, Ctx, Geo, NONE};

fn layer_sets(c: &mut Ctx, id: i64, sp: &ShellElemSpec, ent: u32) {
    let mut ids = vec![];
    for l in &sp.layers {
        let mat = if l.material.is_empty() { &l.name } else { &l.material };
        let mid = c.material(mat);
        ids.push(c.s.add(
            "IFCMATERIALLAYER",
            &[r(mid), real(l.thickness), s(NONE), text(&l.name), s(NONE), s(NONE), s(NONE)],
        ));
    }
    let name = if sp.name.is_empty() { format!("Shell {id}") } else { sp.name.clone() };
    let set = c.s.add("IFCMATERIALLAYERSET", &[list(&ids), text(&name), s(NONE)]);
    c.rel("IFCRELASSOCIATESMATERIAL", "shell-mat", id, &[list(&[ent]), r(set)]);
}

/// Roofs and slabs on surfaces of one level.
pub(super) fn shells(c: &mut Ctx, model: &Model, level: i64, g: &Geo, out: &mut Vec<u32>) -> Result<(), String> {
    let rows = model.shell_elems(Some(level)).map_err(|e| e.to_string())?;
    for (id, sp) in rows {
        let (Ok(q), Ok(sm)) = (model.shell_elem_quantities(id), mesh_of(&sp, 0.0)) else {
            continue; // an element that does not tessellate is not written
        };
        let mut tris = vec![];
        for m in &sm.layers {
            let coords: Vec<String> = m.positions.iter().map(|p| reals(&[p.x, p.y, p.z])).collect();
            let pts = c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({})", coords.join(",")), s(NONE)]);
            let idx: Vec<String> =
                m.triangles.iter().map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1)).collect();
            tris.push(c.s.add(
                "IFCTRIANGULATEDFACESET",
                &[r(pts), s(NONE), logical(true), format!("({})", idx.join(",")), s(NONE)],
            ));
        }
        // the whole stack as one exact shell: the freeform writer on the same faces
        let whole = FreeformSpec {
            level: sp.level,
            name: sp.name.clone(),
            material: String::new(),
            surface: sp.surface.clone(),
            thickness: sp.thickness(),
            side: sp.side,
            trim: sp.full_trim().map_err(|e| e.to_string())?,
            origin: String::new(),
        };
        let ex = exact(c, &whole)?;
        let fallback = if ex.is_some() { "Body-FallBack" } else { "Body" };
        let mut reps = vec![];
        if let Some((item, kind)) = ex {
            reps.push(c.s.add("IFCSHAPEREPRESENTATION", &[r(g.body_ctx), text("Body"), text(kind), list(&[item])]));
        }
        reps.push(c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(g.body_ctx), text(fallback), text("Tessellation"), list(&tris)],
        ));
        let pds = c.s.add("IFCPRODUCTDEFINITIONSHAPE", &[s(NONE), s(NONE), list(&reps)]);
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let (class, kind, label, pre) = match sp.kind {
            ShellKind::Roof => ("IFCROOF", "shell-roof", "Roof", ".FREEFORM."),
            ShellKind::Slab => ("IFCSLAB", "shell-slab", "Slab", ".FLOOR."),
        };
        let name = if sp.name.is_empty() { format!("{label} {id}") } else { sp.name.clone() };
        let ent = c.rooted(class, kind, id, &name, &[text("Surface"), r(pl), r(pds), s(NONE), s(pre)]);
        out.push(ent);
        layer_sets(c, id, &sp, ent);

        let lab = |v: &str| format!("IFCLABEL({})", text(v));
        let fields = [
            ("Side", lab(sp.side.key())),
            ("Layers", format!("IFCINTEGER({})", sp.layers.len())),
            ("Thickness", format!("IFCLENGTHMEASURE({})", real(q.thickness))),
            ("Openings", format!("IFCINTEGER({})", sp.openings.len())),
            ("Host", lab(&sp.host.map(|h| format!("Freeform {h}")).unwrap_or_default())),
            ("Orphan", format!("IFCBOOLEAN({})", logical(sp.orphan))),
            ("ExactGeometry", format!("IFCBOOLEAN({})", logical(ex.is_some()))),
            ("ExactQuantities", format!("IFCBOOLEAN({})", logical(q.exact))),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)]))
            .collect();
        let set = c.rooted("IFCPROPERTYSET", "shell-pset", id, "DDC_SurfaceShell", &[list(&props)]);
        c.rel("IFCRELDEFINESBYPROPERTIES", "shell-pset-rel", id, &[list(&[ent]), r(set)]);
        let mut qs = vec![("Width", 0, q.thickness), ("GrossArea", 1, q.area), ("GrossVolume", 2, q.volume)];
        if sp.kind == ShellKind::Roof {
            qs.remove(0);
        }
        qto(c, g, "shell-qto", id, "BaseQuantities", ent, &qs);
    }
    Ok(())
}

#[cfg(test)]
#[path = "shell_elem_tests.rs"]
mod tests;
