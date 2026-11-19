// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Advanced geometry of an export: the exact / tessellated choice and its note, tapered
//! extrusions of similar blends, large meshes as IfcPolygonalFaceSet (coplanar triangles
//! merged into planar faces, holes as voids), and mass forms as proxies (opt-in). Freeform
//! NURBS bodies have their own module.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};

use ddc_geom::arc::BulgeRing;
use ddc_geom::V3;
use ddc_model::{Model, SolidFrame, SolidSection};

use super::post::{IfcOptions, IfcSchema};
use super::{list, qto, r, real, reals, s, solids, text, Ctx, Geo, NONE};

thread_local! {
    static MASSES: Cell<bool> = const { Cell::new(false) };
    /// Bodies with an exact form written as triangles in this export.
    static FLATTENED: Cell<usize> = const { Cell::new(0) };
}

/// Runs an export under `opts`: exact bodies off for IFC2X3 or on request, mass forms on
/// request. Also returns the note for the export report when exact bodies became triangles.
pub(super) fn with_options<R>(
    opts: &IfcOptions,
    f: impl FnOnce() -> Result<R, String>,
) -> Result<(R, Option<String>), String> {
    let legacy = opts.schema == IfcSchema::Ifc2x3;
    let old_m = MASSES.with(|m| m.replace(opts.mass_forms));
    let old_n = FLATTENED.with(|n| n.replace(0));
    let out = solids::sweep::tessellated(legacy || opts.tessellate, f);
    let n = FLATTENED.with(|c| c.replace(old_n));
    MASSES.with(|m| m.set(old_m));
    let why = if legacy {
        "IFC2X3 has no exact form for them"
    } else {
        "exact geometry is off for this export"
    };
    let note = (n > 0).then(|| format!("{n} typed solids written as triangles: {why}"));
    out.map(|r| (r, note))
}

/// Counts a body whose exact form was not written (see [`with_options`]).
pub(super) fn flattened() {
    FLATTENED.with(|n| n.set(n.get() + 1));
}

/// IfcExtrudedAreaSolidTapered of a blend whose top ring is `k * bottom + t`: the end
/// profile is the bottom profile through a uniform 2D transformation.
pub(super) fn tapered(
    c: &mut Ctx,
    f: &SolidFrame,
    bottom: &BulgeRing,
    k: f64,
    t: ddc_geom::V2,
    height: f64,
) -> u32 {
    let sec = SolidSection {
        outer: bottom.clone(),
        holes: vec![],
    };
    let prof = solids::profile(c, &sec);
    let o = c.point2(t.x, t.y);
    let op = c.s.add(
        "IFCCARTESIANTRANSFORMATIONOPERATOR2D",
        &[s(NONE), s(NONE), r(o), real(k)],
    );
    let end = c.s.add(
        "IFCDERIVEDPROFILEDEF",
        &[s(".AREA."), s(NONE), r(prof), r(op), s(NONE)],
    );
    let pos = solids::position(c, f, f.o);
    let d = solids::dir(c, ddc_geom::v3(0.0, 0.0, f.det().signum()));
    c.s.add(
        "IFCEXTRUDEDAREASOLIDTAPERED",
        &[r(prof), r(pos), r(d), real(height), r(end)],
    )
}

/// Meshes from this many triangles on are merged into planar polygons.
pub(super) const LARGE: usize = 512;

/// One planar face: outer ring and holes (indices into the point list).
type PFace = (Vec<u32>, Vec<Vec<u32>>);

fn find(p: &mut [usize], mut i: usize) -> usize {
    while p[i] != i {
        p[i] = p[p[i]];
        i = p[i];
    }
    i
}

/// Coplanar edge-connected triangles merged into faces (outer ring + holes). A group whose
/// boundary does not form simple loops keeps its triangles.
pub(super) fn merge_planar(pos: &[V3], tris: &[[u32; 3]]) -> Vec<PFace> {
    let size = pos
        .iter()
        .fold(0.0f64, |m, p| m.max(p.x.abs()).max(p.y.abs()).max(p.z.abs()))
        .max(1.0);
    let normal = |t: &[u32; 3]| {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        let n = (b - a).cross(c - a);
        let l = n.len();
        (l > 1e-14 * size * size).then(|| n * (1.0 / l))
    };
    let ns: Vec<Option<V3>> = tris.iter().map(normal).collect();
    let mut by_edge: HashMap<(u32, u32), usize> = HashMap::new();
    for (k, t) in tris.iter().enumerate() {
        for i in 0..3 {
            by_edge.insert((t[i], t[(i + 1) % 3]), k);
        }
    }
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    for (k, t) in tris.iter().enumerate() {
        let Some(n) = ns[k] else { continue };
        let p0 = pos[t[0] as usize];
        for i in 0..3 {
            let Some(&o) = by_edge.get(&(t[(i + 1) % 3], t[i])) else {
                continue;
            };
            let Some(m) = ns[o] else { continue };
            let far = tris[o].iter().all(|v| {
                let d = (pos[*v as usize] - p0).dot(n);
                d.abs() <= 1e-9 * size
            });
            if o != k && n.dot(m) > 1.0 - 1e-10 && far {
                let (a, b) = (find(&mut parent, k), find(&mut parent, o));
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for k in 0..tris.len() {
        let g = find(&mut parent, k);
        groups.entry(g).or_default().push(k);
    }
    let mut out = vec![];
    for (_, g) in groups {
        let tri_faces = |out: &mut Vec<PFace>| {
            for &k in &g {
                out.push((tris[k].to_vec(), vec![]));
            }
        };
        if g.len() == 1 {
            tri_faces(&mut out);
            continue;
        }
        match loops(pos, tris, &g, ns[g[0]].unwrap_or_default()) {
            Some(f) => out.push(f),
            None => tri_faces(&mut out),
        }
    }
    out
}

/// Boundary loops of one coplanar group: one counter-clockwise outer ring (about `n`) and
/// clockwise holes, or None.
fn loops(pos: &[V3], tris: &[[u32; 3]], g: &[usize], n: V3) -> Option<PFace> {
    let mut edges: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for &k in g {
        let t = tris[k];
        for i in 0..3 {
            *edges.entry((t[i], t[(i + 1) % 3])).or_default() += 1;
        }
    }
    if edges.values().any(|c| *c > 1) {
        return None;
    }
    let mut next: BTreeMap<u32, u32> = BTreeMap::new();
    for &(a, b) in edges.keys() {
        if edges.contains_key(&(b, a)) {
            continue;
        }
        if next.insert(a, b).is_some() {
            return None; // two boundary edges leave one vertex
        }
    }
    let mut rings: Vec<(Vec<u32>, f64)> = vec![];
    while let Some((&start, _)) = next.iter().next() {
        let mut ring = vec![start];
        let mut v = next.remove(&start)?;
        while v != start {
            ring.push(v);
            v = next.remove(&v)?;
        }
        if ring.len() < 3 {
            return None;
        }
        let mut a = V3::default();
        for i in 0..ring.len() {
            let (p, q) = (pos[ring[i] as usize], pos[ring[(i + 1) % ring.len()] as usize]);
            a = a + p.cross(q);
        }
        rings.push((ring, a.dot(n) * 0.5));
    }
    let outer: Vec<usize> = (0..rings.len()).filter(|i| rings[*i].1 > 0.0).collect();
    if outer.len() != 1 || rings.iter().any(|r| r.1 == 0.0) {
        return None;
    }
    let o = outer[0];
    let holes = (0..rings.len())
        .filter(|i| *i != o)
        .map(|i| rings[i].0.clone())
        .collect();
    Some((rings[o].0.clone(), holes))
}

fn idx(r: &[u32]) -> String {
    let v: Vec<String> = r.iter().map(|i| (i + 1).to_string()).collect();
    format!("({})", v.join(","))
}

fn polygonal(c: &mut Ctx, pts: u32, faces: &[PFace], closed: bool) -> u32 {
    let fs: Vec<u32> = faces
        .iter()
        .map(|(o, h)| {
            if h.is_empty() {
                c.s.add("IFCINDEXEDPOLYGONALFACE", &[idx(o)])
            } else {
                let hs: Vec<String> = h.iter().map(|x| idx(x)).collect();
                c.s.add(
                    "IFCINDEXEDPOLYGONALFACEWITHVOIDS",
                    &[idx(o), format!("({})", hs.join(","))],
                )
            }
        })
        .collect();
    let cl = s(if closed { ".T." } else { ".F." });
    c.s.add("IFCPOLYGONALFACESET", &[r(pts), cl, list(&fs), s(NONE)])
}

fn point_list(c: &mut Ctx, pos: &[V3], dz: f64) -> u32 {
    let coords = pos
        .iter()
        .map(|p| reals(&[p.x, p.y, p.z - dz]))
        .collect::<Vec<_>>()
        .join(",");
    c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({coords})"), s(NONE)])
}

/// Closed tessellated body, z lowered by `dz`: IfcTriangulatedFaceSet, or for a large mesh
/// whose planar merge saves a quarter of the faces, IfcPolygonalFaceSet.
pub(super) fn face_set(c: &mut Ctx, pos: &[V3], tris: &[[u32; 3]], dz: f64) -> u32 {
    let pts = point_list(c, pos, dz);
    if tris.len() >= LARGE {
        let faces = merge_planar(pos, tris);
        if faces.len() * 4 <= tris.len() * 3 {
            return polygonal(c, pts, &faces, true);
        }
    }
    let ix = tris
        .iter()
        .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
        .collect::<Vec<_>>()
        .join(",");
    c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), s(".T."), format!("({ix})"), s(NONE)],
    )
}

/// Mass forms of a level (on request) -> IfcBuildingElementProxy with ObjectType = form kind;
/// the polyhedral body as IfcPolygonalFaceSet, one face per body face.
pub(super) fn masses(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    elev: f64,
    g: &Geo,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    if !MASSES.with(Cell::get) {
        return Ok(());
    }
    for id in model.masses().map_err(|e| e.to_string())? {
        let sp = model.mass(id).map_err(|e| e.to_string())?;
        if sp.level != level || sp.void {
            continue;
        }
        let Ok(body) = model.mass_body(id) else {
            continue; // a form with a geometry error is not exported
        };
        let (pos, rings) = body.polygon_rings(1e-7);
        if rings.is_empty() {
            continue;
        }
        let pts = point_list(c, &pos, elev);
        let faces: Vec<PFace> = rings.into_iter().map(|r| (r, vec![])).collect();
        let fs = polygonal(c, pts, &faces, true);
        let rep = c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(g.body_ctx), text("Body"), text("Tessellation"), list(&[fs])],
        );
        let pds = c.s.add("IFCPRODUCTDEFINITIONSHAPE", &[s(NONE), s(NONE), list(&[rep])]);
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let label = if sp.name.is_empty() {
            format!("Mass {id}")
        } else {
            sp.name.clone()
        };
        let kind = sp.form.kind();
        let ent = c.rooted(
            "IFCBUILDINGELEMENTPROXY",
            "mass",
            id,
            &label,
            &[text(kind), r(pl), r(pds), s(NONE), s(".ELEMENT.")],
        );
        out.push(ent);
        let flag = |b: bool| format!("IFCBOOLEAN({})", if b { ".T." } else { ".F." });
        let fields = [
            ("Kind", format!("IFCLABEL({})", text(kind))),
            ("InPlace", flag(sp.inplace)),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)]))
            .collect();
        let set = c.rooted("IFCPROPERTYSET", "mass-pset", id, "DDC_MassForm", &[list(&props)]);
        c.rel("IFCRELDEFINESBYPROPERTIES", "mass-pset-rel", id, &[list(&[ent]), r(set)]);
        let area: f64 = body.polys.iter().map(|p| p.area()).sum();
        qto(
            c,
            g,
            "mass-qto",
            id,
            "BaseQuantities",
            ent,
            &[("GrossVolume", 2, body.volume()), ("GrossSurfaceArea", 1, area)],
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "adv_tests.rs"]
mod tests;
