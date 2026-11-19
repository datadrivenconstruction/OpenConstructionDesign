// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Site and terrain -> IFC: one IfcGeographicElement (.TERRAIN.) per toposolid with a
//! triangulated body per material piece, building pads as IfcSlab (.BASESLAB.), all contained
//! in the site; true north on the model context, reference latitude/longitude/elevation on IfcSite.

use std::collections::BTreeMap;

use ddc_geom::mesh::Mesh;
use ddc_model::Model;

use super::{list, r, real, reals, s, text, Ctx, NONE};

fn face_set(c: &mut Ctx, m: &Mesh) -> u32 {
    let coords = m
        .positions
        .iter()
        .map(|p| reals(&[p.x, p.y, p.z]))
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
    let closed = if m.is_watertight() { ".T." } else { ".F." };
    c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), s(closed), format!("({idx})"), s(NONE)],
    )
}

fn shape(c: &mut Ctx, body_ctx: u32, items: &[u32]) -> u32 {
    let rep = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[r(body_ctx), text("Body"), text("Tessellation"), list(items)],
    );
    c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[rep])],
    )
}

fn material(c: &mut Ctx, mats: &mut BTreeMap<String, (u32, Vec<u32>)>, name: &str, el: u32) {
    let name = if name.is_empty() { "earth" } else { name };
    match mats.get_mut(name) {
        Some(m) => m.1.push(el),
        None => {
            let id = c.material(name);
            mats.insert(name.to_string(), (id, vec![el]));
        }
    }
}

fn pset(c: &mut Ctx, kind: &str, id: i64, name: &str, el: u32, fields: &[(&str, String)]) {
    let props: Vec<u32> = fields
        .iter()
        .map(|(n, v)| {
            c.s.add(
                "IFCPROPERTYSINGLEVALUE",
                &[text(n), s(NONE), v.clone(), s(NONE)],
            )
        })
        .collect();
    let set = c.rooted("IFCPROPERTYSET", kind, id, name, &[list(&props)]);
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("{kind}-rel"),
        id,
        &[list(&[el]), r(set)],
    );
}

/// Quantities (name, unit 0 length / 1 area / 2 volume, value).
fn qto(c: &mut Ctx, units: [u32; 3], kind: &str, id: i64, el: u32, q: &[(&str, usize, f64)]) {
    let ents = ["IFCQUANTITYLENGTH", "IFCQUANTITYAREA", "IFCQUANTITYVOLUME"];
    let ql: Vec<u32> = q
        .iter()
        .map(|(n, k, v)| {
            c.s.add(
                ents[*k],
                &[text(n), s(NONE), r(units[*k]), real(*v), s(NONE)],
            )
        })
        .collect();
    let eq = c.rooted(
        "IFCELEMENTQUANTITY",
        kind,
        id,
        "Qto_DDC_Site",
        &[s(NONE), list(&ql)],
    );
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        &format!("{kind}-rel"),
        id,
        &[list(&[el]), r(eq)],
    );
}

fn plan_area(m: &Mesh) -> f64 {
    // upward triangles only: the top surface of the terrain solid
    m.triangles
        .iter()
        .map(|t| {
            let (a, b, c) = (
                m.positions[t[0] as usize],
                m.positions[t[1] as usize],
                m.positions[t[2] as usize],
            );
            ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)) / 2.0
        })
        .filter(|a| *a > 0.0)
        .sum()
}

/// Toposolids and pads of the model, contained in `site`.
pub(super) fn site_elements(
    c: &mut Ctx,
    model: &Model,
    site: u32,
    site_pl: u32,
    body_ctx: u32,
    units: [u32; 3],
) -> Result<Vec<String>, String> {
    let e = |x: ddc_model::Error| x.to_string();
    let topos = model.toposolids().map_err(e)?;
    let mut elems = vec![];
    let mut warnings = vec![];
    let mut mats: BTreeMap<String, (u32, Vec<u32>)> = BTreeMap::new();
    for (id, name) in topos {
        // one failed terrain does not stop the model export
        let built = model.topo_built_shared(id);
        let (spec, pieces) = match (model.toposolid(id), model.topo_pieces_shared(id)) {
            (Ok(s), Ok(p)) => (s, p),
            (Err(x), _) | (_, Err(x)) => {
                warnings.push(format!("toposolid {id} skipped: {x}"));
                continue;
            }
        };
        if let Ok(b) = &built {
            warnings.extend(b.warnings.iter().map(|w| format!("toposolid {id}: {w}")));
        }
        let mut items = vec![];
        let (mut volume, mut area) = (0.0, 0.0);
        let mut terrain_mats = vec![];
        for p in pieces.iter().filter(|p| p.pad.is_none()) {
            if p.mesh.triangles.is_empty() {
                continue;
            }
            items.push(face_set(c, &p.mesh));
            volume += p.mesh.volume();
            area += plan_area(&p.mesh);
            if !terrain_mats.contains(&p.material) {
                terrain_mats.push(p.material.clone());
            }
        }
        if !items.is_empty() {
            let shp = shape(c, body_ctx, &items);
            let o = c.point3(0.0, 0.0, 0.0);
            let ax = c.axis3(o, None);
            let pl = c.placement(Some(site_pl), ax);
            let el = c.rooted(
                "IFCGEOGRAPHICELEMENT",
                "toposolid",
                id,
                &name,
                &[s(NONE), r(pl), r(shp), s(NONE), s(".TERRAIN.")],
            );
            elems.push(el);
            for m in &terrain_mats {
                material(c, &mut mats, m, el);
            }
            pset(
                c,
                "topo-pset",
                id,
                "Pset_DDC_Toposolid",
                el,
                &[
                    (
                        "Thickness",
                        format!("IFCLENGTHMEASURE({})", real(spec.thickness)),
                    ),
                    ("PointCount", format!("IFCINTEGER({})", spec.points.len())),
                    ("PlanArea", format!("IFCAREAMEASURE({})", real(area))),
                ],
            );
            qto(
                c,
                units,
                "topo-qto",
                id,
                el,
                &[("GrossArea", 1, area), ("GrossVolume", 2, volume)],
            );
        }
        for p in pieces.iter() {
            let Some(pad) = p.pad else { continue };
            if p.mesh.triangles.is_empty() {
                continue;
            }
            let fs = face_set(c, &p.mesh);
            let shp = shape(c, body_ctx, &[fs]);
            let o = c.point3(0.0, 0.0, 0.0);
            let ax = c.axis3(o, None);
            let pl = c.placement(Some(site_pl), ax);
            let el = c.rooted(
                "IFCSLAB",
                "pad",
                pad,
                &format!("Pad {pad}"),
                &[s(NONE), r(pl), r(shp), s(NONE), s(".BASESLAB.")],
            );
            elems.push(el);
            material(c, &mut mats, &p.material, el);
            qto(
                c,
                units,
                "pad-qto",
                pad,
                el,
                &[("GrossVolume", 2, p.mesh.volume())],
            );
        }
    }
    for (k, (_, (mid, els))) in mats.iter().enumerate() {
        c.rel(
            "IFCRELASSOCIATESMATERIAL",
            "topo-mat",
            k as i64,
            &[list(els), r(*mid)],
        );
    }
    if !elems.is_empty() {
        c.rel(
            "IFCRELCONTAINEDINSPATIALSTRUCTURE",
            "contain-site",
            0,
            &[list(&elems), r(site)],
        );
    }
    Ok(warnings)
}

/// TrueNorth of the model context: 2D direction of true north in project axes, or `$`.
pub(super) fn true_north(c: &mut Ctx, model: &Model) -> String {
    let Ok(sp) = model.site() else {
        return s(NONE);
    };
    let a = sp.north_deg.to_radians();
    if !a.is_finite() || sp.north_deg.rem_euclid(360.0) == 0.0 {
        return s(NONE);
    }
    r(c.s.add("IFCDIRECTION", &[reals(&[-a.sin(), a.cos()])]))
}

/// IfcCompoundPlaneAngleMeasure: degrees, minutes, seconds, millionths (all with one sign).
fn compound(v: f64) -> String {
    if !v.is_finite() {
        return s(NONE);
    }
    let sign = if v < 0.0 { -1i64 } else { 1 };
    let micro = (v.abs() * 3600.0 * 1e6).round() as i64;
    let (d, rest) = (micro / 3_600_000_000, micro % 3_600_000_000);
    let (m, rest) = (rest / 60_000_000, rest % 60_000_000);
    let (sec, mic) = (rest / 1_000_000, rest % 1_000_000);
    format!("({},{},{},{})", sign * d, sign * m, sign * sec, sign * mic)
}

/// IfcSite RefLatitude, RefLongitude, RefElevation (`$` when the site is unset).
pub(super) fn site_attrs(model: &Model) -> [String; 3] {
    let Ok(sp) = model.site() else {
        return [s(NONE), s(NONE), s(NONE)];
    };
    let geo = sp.latitude != 0.0 || sp.longitude != 0.0;
    let (lat, lon) = if geo {
        (compound(sp.latitude), compound(sp.longitude))
    } else {
        (s(NONE), s(NONE))
    };
    // no site given — RefElevation is not written (otherwise 0. would be read as an elevation)
    let set = model
        .sql()
        .query_row("SELECT count(*) FROM site_info", [], |r| r.get::<_, i64>(0))
        .is_ok_and(|n| n > 0);
    let z = if set && sp.base.2.is_finite() {
        real(sp.base.2)
    } else {
        s(NONE)
    };
    [lat, lon, z]
}

/// IfcProjectedCRS + IfcMapConversion from the project base point (easting, northing, height)
/// in the coordinate system stored on the site (`crs_code`, `crs_name`, `crs_vertical`).
/// Without a stored code nothing is written: a map conversion into a guessed system would
/// misplace the model in every viewer.
pub(super) fn map_conversion(c: &mut Ctx, model: &Model, context: u32, len_unit: u32) {
    let Ok(sp) = model.site() else { return };
    let (e, n, h) = sp.base;
    let finite = e.is_finite() && n.is_finite() && h.is_finite() && sp.north_deg.is_finite();
    if sp.crs_code.trim().is_empty() || !finite {
        return;
    }
    let opt = |v: &str| if v.trim().is_empty() { s(NONE) } else { text(v.trim()) };
    let crs = c.s.add(
        "IFCPROJECTEDCRS",
        &[
            text(sp.crs_code.trim()),
            opt(&sp.crs_name),
            s(NONE),
            opt(&sp.crs_vertical),
            s(NONE),
            s(NONE),
            r(len_unit),
        ],
    );
    let a = sp.north_deg.to_radians();
    c.s.add(
        "IFCMAPCONVERSION",
        &[
            r(context),
            r(crs),
            real(e),
            real(n),
            real(h),
            real(a.cos()),
            real(-a.sin()),
            s(NONE),
        ],
    );
}

/// Site name from the site properties, "Site" by default.
pub(super) fn site_name(model: &Model) -> String {
    model
        .site()
        .ok()
        .map(|sp| sp.name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Site".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{SiteProps, TopoSpec};

    fn model() -> Model {
        let mut m = Model::open(":memory:").unwrap();
        m.add_level("L1", 0.0).unwrap();
        let mut pts = vec![];
        for i in 0..6 {
            for j in 0..5 {
                let (x, y) = (i as f64 * 4.0, j as f64 * 5.0);
                pts.push((x, y, 100.0 + 0.05 * x + 0.02 * y));
            }
        }
        m.add_toposolid(&TopoSpec {
            name: "Ground".into(),
            thickness: 1.5,
            material: "earth".into(),
            points: pts,
            breaklines: vec![],
            boundary: vec![],
        })
        .unwrap();
        m.set_site(&SiteProps {
            name: "Plot 7".into(),
            north_deg: 30.0,
            base: (1000.0, 2000.0, 100.0),
            latitude: 55.75,
            longitude: 37.6175,
            ..Default::default()
        })
        .unwrap();
        m
    }

    #[test]
    fn terrain_exported_into_site() {
        let m = model();
        let t = crate::ifc::export_ifc(&m).unwrap();
        crate::ifc::check_step(&t).unwrap();
        assert!(t.contains("IFCGEOGRAPHICELEMENT("));
        assert!(t.contains(".TERRAIN."));
        let site_id = t
            .lines()
            .find(|l| l.contains("=IFCSITE("))
            .and_then(|l| l.split('=').next())
            .unwrap()
            .to_string();
        assert!(t
            .lines()
            .any(|l| l.contains("IFCRELCONTAINEDINSPATIALSTRUCTURE")
                && l.trim_end_matches(");").ends_with(&site_id)));
        let site = t.lines().find(|l| l.contains("=IFCSITE(")).unwrap();
        assert!(site.contains("'Plot 7'"));
        assert!(site.contains("(55,45,0,0)"), "{site}");
        assert!(site.contains("100."));
        assert!(t.contains("Pset_DDC_Toposolid"));
        // true north rotated 30° ccw from +Y
        assert!(t
            .lines()
            .any(|l| l.contains("IFCDIRECTION((-0.49999999999999994,")
                || l.contains("IFCDIRECTION((-0.5,")));
    }

    #[test]
    fn compound_angle() {
        assert_eq!(compound(37.6175), "(37,37,3,0)");
        assert_eq!(compound(-0.5), "(0,-30,0,0)");
    }

    #[test]
    fn round_trip_points_and_area() {
        let m = model();
        let id = m.toposolids().unwrap()[0].0;
        let n = m.toposolid(id).unwrap().points.len();
        let area = m.topo_built(id).unwrap().tin.plan_area();
        let t = crate::ifc::export_ifc(&m).unwrap();
        let src = crate::topo::from_ifc(t.as_bytes()).unwrap();
        assert_eq!(src.points.len(), n);
        let ring = &src.boundary;
        let a2: f64 = (0..ring.len())
            .map(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
                p.0 * q.1 - q.0 * p.1
            })
            .sum::<f64>()
            / 2.0;
        assert!((a2 - area).abs() <= 1e-6 * area, "{a2} vs {area}");
    }
}
