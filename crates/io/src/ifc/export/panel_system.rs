//! Curtain systems on freeform hosts -> IfcCurtainWall aggregating one IfcPlate
//! (.CURTAIN_PANEL.) per kept panel and one IfcMember (.MULLION.) per grid edge. Each part is
//! a closed triangulated prism in storey coordinates; the plate's ObjectType names its panel
//! type and its Description the bend kind (flat, cold-bent, hot-bent).

use ddc_geom::mesh::Mesh;
use ddc_geom::panelize::PanelKind;
use ddc_model::Model;

use super::{list, qto, r, real, reals, s, text, Ctx, Geo, NONE};

/// Closed triangle set of a part, lowered by the storey elevation `z0`.
fn faceset(c: &mut Ctx, m: &Mesh, z0: f64) -> u32 {
    let m = &super::mep::finite_part(m);
    let coords: Vec<String> = m.positions.iter().map(|p| reals(&[p.x, p.y, p.z - z0])).collect();
    let pts = c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({})", coords.join(",")), s(NONE)]);
    let idx: Vec<String> = m.triangles.iter().map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1)).collect();
    c.s.add("IFCTRIANGULATEDFACESET", &[r(pts), s(NONE), s(".T."), format!("({})", idx.join(",")), s(NONE)])
}

/// Placement under the storey and the body of one part.
fn body(c: &mut Ctx, g: &Geo, m: &Mesh, z0: f64) -> (u32, u32) {
    let tri = faceset(c, m, z0);
    let rep = c.s.add("IFCSHAPEREPRESENTATION", &[r(g.body_ctx), text("Body"), text("Tessellation"), list(&[tri])]);
    let pds = c.s.add("IFCPRODUCTDEFINITIONSHAPE", &[s(NONE), s(NONE), list(&[rep])]);
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    (c.placement(Some(g.storey_pl), ax), pds)
}

fn kind_key(k: PanelKind) -> &'static str {
    match k {
        PanelKind::Flat => "flat",
        PanelKind::ColdBent => "cold-bent",
        PanelKind::HotBent => "hot-bent",
    }
}

/// Curtain systems whose host lies on `level` (storey elevation `z0`).
pub(super) fn panel_systems(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    z0: f64,
    g: &Geo,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    let rows = model.panel_systems(Some(level)).map_err(|e| e.to_string())?;
    for (id, sp) in rows {
        let Ok(geo) = model.panel_system_geom(id) else {
            continue; // a system its host can no longer carry is not written
        };
        let q = geo.quantities(&sp);
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let label = if sp.name.is_empty() { format!("Curtain system {id}") } else { sp.name.clone() };
        let cw = c.rooted("IFCCURTAINWALL", "panel-system", id, &label, &[text("PanelSystem"), r(pl), s(NONE), s(NONE), s(NONE)]);
        out.push(cw);
        let mut parts = vec![];
        let mut plates = vec![];
        for (k, &pi) in geo.kept.iter().enumerate() {
            let p = &geo.panels.panels[pi];
            let (ppl, pds) = body(c, g, &geo.glass[k], z0);
            let pid = c.g.get(&format!("panel-plate:{pi}"), id);
            let ty = format!("Type {}", p.cluster + 1);
            let a = [
                pid,
                NONE.into(),
                text(&format!("Panel {}", pi + 1)),
                text(kind_key(p.kind)),
                text(&ty),
                r(ppl),
                r(pds),
                s(NONE),
                s(".CURTAIN_PANEL."),
            ];
            let ent = c.s.add("IFCPLATE", &a);
            let qs = [("Area", 1, p.area), ("Width", 0, sp.glass)];
            qto(c, g, &format!("panel-plate-qto:{pi}"), id, "Qto_PlateBaseQuantities", ent, &qs);
            parts.push(ent);
            plates.push(ent);
        }
        for (k, (&[a, b], frame)) in geo.edges.iter().zip(&geo.frames).enumerate() {
            let len = (geo.panels.nodes[b] - geo.panels.nodes[a]).len();
            let (mpl, pds) = body(c, g, frame, z0);
            let mid = c.g.get(&format!("panel-member:{a}:{b}"), id);
            let at = [
                mid,
                NONE.into(),
                text(&format!("Mullion {}", k + 1)),
                s(NONE),
                s(NONE),
                r(mpl),
                r(pds),
                s(NONE),
                s(".MULLION."),
            ];
            let m = c.s.add("IFCMEMBER", &at);
            qto(c, g, &format!("panel-member-qto:{a}:{b}"), id, "Qto_MemberBaseQuantities", m, &[("Length", 0, len)]);
            parts.push(m);
        }
        if !parts.is_empty() {
            c.rel("IFCRELAGGREGATES", "agg-panel-system", id, &[r(cw), list(&parts)]);
        }
        if !plates.is_empty() {
            super::stairs::material(c, "panel_system", id, &sp.material, &plates);
        }
        let int = |n: usize| format!("IFCINTEGER({n})");
        let len = |x: f64| format!("IFCLENGTHMEASURE({})", real(x));
        let fields = [
            ("Panels", int(q.panels)),
            ("PanelTypes", int(q.types)),
            ("Flat", int(q.flat)),
            ("ColdBent", int(q.cold_bent)),
            ("HotBent", int(q.hot_bent)),
            ("MaxPlanarity", len(q.max_planarity)),
            ("OffSurface", len(q.off_surface)),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)]))
            .collect();
        let set = c.rooted("IFCPROPERTYSET", "panel-system-pset", id, "DDC_PanelSystem", &[list(&props)]);
        c.rel("IFCRELDEFINESBYPROPERTIES", "panel-system-pset-rel", id, &[list(&[cw]), r(set)]);
        let qs = [("GrossSideArea", 1, q.area), ("MullionLength", 0, q.mullion_length)];
        qto(c, g, "panel-system-qto", id, "BaseQuantities", cw, &qs);
    }
    Ok(())
}

#[cfg(test)]
#[path = "panel_system_tests.rs"]
mod tests;
