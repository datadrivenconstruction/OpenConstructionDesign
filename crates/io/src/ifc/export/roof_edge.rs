//! Level roof edges: fascia board and soffit — coverings, gutter — a pipe segment,
//! slab edge — a proxy, moulding/cornice — a moulding covering, tapered insulation — an insulation
//! covering, a lantern — a lantern window in a roof opening (IfcOpeningElement, RelVoids/RelFills).
//! Bodies are triangle meshes; empty and broken elements are skipped. Rustication — in views only.

use ddc_geom::mesh::Mesh;
use ddc_geom::sweep::SweepKind;
use ddc_model::Model;

use super::{list, r, real, s, text, Ctx, Geo, NONE};

/// A mesh body; an empty mesh (point list [1:?]) — None.
fn body(c: &mut Ctx, g: &Geo, m: &Mesh, elev: f64) -> Option<u32> {
    if m.triangles.is_empty() || m.positions.is_empty() {
        return None;
    }
    let coords = m
        .positions
        .iter()
        .map(|p| super::reals(&[p.x, p.y, p.z - elev]))
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
    let fs = c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), s(".T."), format!("({idx})"), s(NONE)],
    );
    let rep = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[
            r(g.body_ctx),
            text("Body"),
            text("Tessellation"),
            list(&[fs]),
        ],
    );
    Some(c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[rep])],
    ))
}

fn pset(c: &mut Ctx, kind: &str, id: i64, name: &str, ent: u32, fields: Vec<(&str, String)>) {
    let props: Vec<u32> = fields
        .into_iter()
        .map(|(n, v)| {
            c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)])
        })
        .collect();
    let set = c.rooted("IFCPROPERTYSET", kind, id, name, &[list(&props)]);
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("{kind}-rel"),
        id,
        &[list(&[ent]), r(set)],
    );
}

fn class(k: SweepKind) -> Option<(&'static str, &'static str)> {
    match k {
        SweepKind::Fascia => Some(("IFCCOVERING", ".CLADDING.")),
        SweepKind::Gutter => Some(("IFCPIPESEGMENT", ".GUTTER.")),
        SweepKind::SlabEdge => Some(("IFCBUILDINGELEMENTPROXY", ".ELEMENT.")),
        SweepKind::WallSweep => Some(("IFCCOVERING", ".MOLDING.")),
        SweepKind::Reveal => None,
    }
}

/// An element that cannot be written is listed in the export report, never dropped silently.
fn skip(c: &mut Ctx, kind: &str, id: i64, why: &str) {
    c.g.warnings
        .push(format!("{kind} {id} skipped: {why}, not exported"));
}

pub(super) fn items(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    elems: &mut Vec<u32>,
) -> Result<(), String> {
    let e = |x: ddc_model::Error| x.to_string();
    let label = |v: &str| format!("IFCLABEL({})", text(v));
    let num = |v: f64| format!("IFCREAL({})", real(v));
    let o = c.point3(0.0, 0.0, 0.0);
    let ax = c.axis3(o, None);
    let pl = c.placement(Some(g.storey_pl), ax);
    for id in model.sweeps(Some(level)).map_err(e)? {
        // a broken row is skipped (as in views), not a failure of the whole export
        let Ok(sp) = model.sweep(id) else {
            skip(c, "sweep", id, "row cannot be read");
            continue;
        };
        let Some((ent_name, pre)) = class(sp.kind) else {
            continue;
        };
        // a broken host (wall deleted, etc.) — the element is skipped
        let (Ok(m), Ok(q)) = (model.sweep_mesh(id), model.sweep_quantities(id)) else {
            skip(c, "sweep", id, "geometry cannot be built");
            continue;
        };
        let Some(pds) = body(c, g, &m, elev) else {
            skip(c, "sweep", id, "empty geometry");
            continue;
        };
        let ent = c.rooted(
            ent_name,
            "sweep",
            id,
            &format!("{}:{}", sp.kind.name(), sp.profile),
            &[text(&sp.profile), r(pl), r(pds), s(NONE), s(pre)],
        );
        let mut f = vec![
            ("Kind", label(sp.kind.name())),
            ("Profile", label(&sp.profile)),
            ("Length", num(q.length)),
            ("Section", num(q.section)),
            ("Volume", num(q.volume)),
        ];
        if let Some(mat) = &sp.material {
            f.push(("Material", label(mat)));
        }
        pset(c, "sweep-pset", id, "DDC_Sweep", ent, f);
        elems.push(ent);
    }
    for id in model.level_skylights(level).map_err(e)? {
        let (Ok(k), Ok((mut frame, glass))) = (model.skylight(id), model.skylight_mesh(id)) else {
            skip(c, "skylight", id, "geometry cannot be built");
            continue;
        };
        // an opening in the roof from the lantern box
        let open = body(c, g, &frame, elev).map(|opds| {
            c.rooted(
                "IFCOPENINGELEMENT",
                "skylight-open",
                id,
                &format!("Skylight opening {id}"),
                &[s(NONE), r(pl), r(opds), s(NONE), s(".OPENING.")],
            )
        });
        merge_mesh(&mut frame, &glass);
        let Some(pds) = body(c, g, &frame, elev) else {
            skip(c, "skylight", id, "empty geometry");
            continue;
        };
        let ent = c.rooted(
            "IFCWINDOW",
            "skylight",
            id,
            &format!("Skylight {}x{}", k.width, k.length),
            &[
                s(NONE),
                r(pl),
                r(pds),
                s(NONE),
                real(k.length),
                real(k.width),
                s(".SKYLIGHT."),
                s(NONE),
                s(NONE),
            ],
        );
        pset(
            c,
            "skylight-pset",
            id,
            "DDC_Skylight",
            ent,
            vec![("Roof", num(k.roof as f64)), ("Curb", num(k.curb))],
        );
        if let (Some(open), Some(&roof)) = (open, c.roof_ent.get(&k.roof)) {
            c.rel(
                "IFCRELVOIDSELEMENT",
                "skylight-void",
                id,
                &[r(roof), r(open)],
            );
            c.rel(
                "IFCRELFILLSELEMENT",
                "skylight-fill",
                id,
                &[r(open), r(ent)],
            );
        }
        elems.push(ent);
    }
    for id in model.level_soffits(level).map_err(e)? {
        let Ok((m, area)) = model.soffit_mesh(id) else {
            skip(c, "soffit", id, "geometry cannot be built");
            continue;
        };
        let Some(pds) = body(c, g, &m, elev) else {
            skip(c, "soffit", id, "empty geometry");
            continue;
        };
        let ent = c.rooted(
            "IFCCOVERING",
            "soffit",
            id,
            "Soffit",
            &[s(NONE), r(pl), r(pds), s(NONE), s(".CEILING.")],
        );
        pset(
            c,
            "soffit-pset",
            id,
            "DDC_Soffit",
            ent,
            vec![("Area", num(area))],
        );
        elems.push(ent);
    }
    for id in model.tapers(level).map_err(e)? {
        let (Ok(m), Ok(q)) = (model.taper_mesh(id), model.taper_quantities(id)) else {
            skip(c, "taper", id, "geometry cannot be built");
            continue;
        };
        let Some(pds) = body(c, g, &m, elev) else {
            skip(c, "taper", id, "empty geometry");
            continue;
        };
        let ent = c.rooted(
            "IFCCOVERING",
            "taper",
            id,
            "Tapered insulation",
            &[s(NONE), r(pl), r(pds), s(NONE), s(".INSULATION.")],
        );
        pset(
            c,
            "taper-pset",
            id,
            "DDC_TaperedInsulation",
            ent,
            vec![
                ("Area", num(q.area)),
                ("Volume", num(q.volume)),
                ("ThicknessMin", num(q.t_min)),
                ("ThicknessMax", num(q.t_max)),
                ("ThicknessAvg", num(q.t_avg)),
            ],
        );
        elems.push(ent);
    }
    Ok(())
}

fn merge_mesh(into: &mut Mesh, m: &Mesh) {
    let base = into.positions.len() as u32;
    into.positions.extend_from_slice(&m.positions);
    into.triangles
        .extend(m.triangles.iter().map(|t| t.map(|i| i + base)));
    into.face_of.extend(m.face_of.iter().copied());
}
