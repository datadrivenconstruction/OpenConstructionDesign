//! Freeform NURBS elements -> IfcBuildingElementProxy. An untrimmed open patch is written
//! exactly: a shell as IfcAdvancedBrep (the two offset faces and four ruled sides, B-spline
//! edges), a bare surface as IfcShellBasedSurfaceModel of one IfcAdvancedFace. Every element
//! also gets a triangulated 'Body-FallBack'; a trimmed or closed patch gets triangles only.

use ddc_geom::nurbs::{self, Curve, Surface};
use ddc_geom::V3;
use ddc_model::freeform::{mesh_of, FreeformSpec};
use ddc_model::Model;

use super::{list, qto, r, real, reals, s, text, Ctx, Geo, NONE};

/// Allowed deviation of an offset face from the true offset, m.
const OFFSET_TOL: f64 = 5e-4;

pub(super) fn logical(b: bool) -> String {
    s(if b { ".T." } else { ".F." })
}

/// Distinct knots and their multiplicities.
fn knots(k: &[f64]) -> (String, String) {
    let mut vals: Vec<f64> = vec![];
    let mut mult: Vec<usize> = vec![];
    for &x in k {
        match vals.last() {
            Some(&l) if l == x => *mult.last_mut().unwrap() += 1,
            _ => {
                vals.push(x);
                mult.push(1);
            }
        }
    }
    let m: Vec<String> = mult.iter().map(|m| m.to_string()).collect();
    (format!("({})", m.join(",")), reals(&vals))
}

fn rational(w: &[f64]) -> bool {
    w.iter().any(|w| (w - 1.0).abs() > 1e-12)
}

fn point(c: &mut Ctx, p: V3) -> u32 {
    c.point3(p.x, p.y, p.z)
}

fn bspline_curve(c: &mut Ctx, cv: &Curve) -> u32 {
    let pts: Vec<u32> = cv.ctrl().iter().map(|p| point(c, *p)).collect();
    let (m, k) = knots(cv.knots());
    let mut a = vec![
        cv.degree().to_string(),
        list(&pts),
        s(".UNSPECIFIED."),
        s(".F."),
        s(".F."),
        m,
        k,
        s(".UNSPECIFIED."),
    ];
    if rational(cv.weights()) {
        a.push(reals(cv.weights()));
        return c.s.add("IFCRATIONALBSPLINECURVEWITHKNOTS", &a);
    }
    c.s.add("IFCBSPLINECURVEWITHKNOTS", &a)
}

fn bspline_surface(c: &mut Ctx, sf: &Surface) -> u32 {
    let (nu, nv) = (sf.nu(), sf.nv());
    let rows: Vec<String> = (0..nu)
        .map(|i| {
            let row: Vec<u32> = (0..nv).map(|j| point(c, sf.ctrl()[i * nv + j])).collect();
            list(&row)
        })
        .collect();
    let (mu, ku) = knots(sf.knots_u());
    let (mv, kv) = knots(sf.knots_v());
    let mut a = vec![
        sf.deg_u().to_string(),
        sf.deg_v().to_string(),
        format!("({})", rows.join(",")),
        s(".UNSPECIFIED."),
        s(".F."),
        s(".F."),
        s(".F."),
        mu,
        mv,
        ku,
        kv,
        s(".UNSPECIFIED."),
    ];
    if rational(sf.weights()) {
        let w: Vec<String> = (0..nu).map(|i| reals(&sf.weights()[i * nv..(i + 1) * nv])).collect();
        a.push(format!("({})", w.join(",")));
        return c.s.add("IFCRATIONALBSPLINESURFACEWITHKNOTS", &a);
    }
    c.s.add("IFCBSPLINESURFACEWITHKNOTS", &a)
}

/// Topological edge with its two vertices.
#[derive(Clone, Copy)]
struct Edge {
    id: u32,
}

fn edge(c: &mut Ctx, a: u32, b: u32, geom: u32) -> Edge {
    Edge { id: c.s.add("IFCEDGECURVE", &[r(a), r(b), r(geom), s(".T.")]) }
}

/// Face bounded by the loop `base` (edge, forward) taken in the order that is counterclockwise
/// around the surface normal; `same` false reverses both the face and the loop.
fn face(c: &mut Ctx, surface: u32, base: &[(Edge, bool)], same: bool) -> u32 {
    let uses: Vec<(Edge, bool)> = if same {
        base.to_vec()
    } else {
        base.iter().rev().map(|(e, f)| (*e, !f)).collect()
    };
    let oes: Vec<u32> = uses
        .iter()
        .map(|(e, f)| c.s.add("IFCORIENTEDEDGE", &[s("*"), s("*"), r(e.id), logical(*f)]))
        .collect();
    let lp = c.s.add("IFCEDGELOOP", &[list(&oes)]);
    let bound = c.s.add("IFCFACEOUTERBOUND", &[r(lp), s(".T.")]);
    c.s.add("IFCADVANCEDFACE", &[list(&[bound]), r(surface), logical(same)])
}

/// Corners [S(u0,v0), S(u1,v0), S(u1,v1), S(u0,v1)] as (cartesian point, vertex).
fn corners(c: &mut Ctx, sf: &Surface) -> [(u32, u32); 4] {
    let (u0, u1) = sf.domain_u();
    let (v0, v1) = sf.domain_v();
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)].map(|(u, v)| {
        let p = point(c, sf.point(u, v));
        (p, c.s.add("IFCVERTEXPOINT", &[r(p)]))
    })
}

/// Boundary edges [S(.,v0) 0->1, S(u1,.) 1->2, S(.,v1) 3->2, S(u0,.) 0->3].
fn rim(c: &mut Ctx, curves: &[Curve; 4], vx: &[(u32, u32); 4]) -> [Edge; 4] {
    let ends = [(0, 1), (1, 2), (3, 2), (0, 3)];
    let mut out = [Edge { id: 0 }; 4];
    for k in 0..4 {
        let g = bspline_curve(c, &curves[k]);
        out[k] = edge(c, vx[ends[k].0].1, vx[ends[k].1].1, g);
    }
    out
}

/// Loop of a patch face in its own counterclockwise order.
fn patch_loop(e: &[Edge; 4]) -> [(Edge, bool); 4] {
    [(e[0], true), (e[1], true), (e[2], false), (e[3], false)]
}

/// The exact body item and its representation type, or None when only triangles fit.
pub(super) fn exact(c: &mut Ctx, sp: &FreeformSpec) -> Result<Option<(u32, &'static str)>, String> {
    let sf = &sp.surface;
    let open = !sf.closed_u() && !sf.closed_v();
    let poles = sf.pole_u(false) || sf.pole_u(true) || sf.pole_v(false) || sf.pole_v(true);
    if !sp.trim.is_empty() || !open || poles {
        return Ok(None);
    }
    if sp.thickness <= 0.0 {
        let bc = sf.boundary().map_err(|e| e.to_string())?;
        let vx = corners(c, sf);
        let es = rim(c, &bc, &vx);
        let g = bspline_surface(c, sf);
        let f = face(c, g, &patch_loop(&es), true);
        let sh = c.s.add("IFCOPENSHELL", &[list(&[f])]);
        let m = c.s.add("IFCSHELLBASEDSURFACEMODEL", &[list(&[sh])]);
        return Ok(Some((m, "AdvancedSurface")));
    }
    let (a, b) = sp.side.offsets(sp.thickness);
    let off = |d: f64| -> Result<Option<Surface>, String> {
        let o = nurbs::offset(sf, d, OFFSET_TOL).map_err(|e| e.to_string())?;
        Ok(o.converged.then_some(o.surface))
    };
    let (Some(sa), Some(sb)) = (off(a)?, off(b)?) else {
        return Ok(None);
    };
    let (ca, cb) = (
        sa.boundary().map_err(|e| e.to_string())?,
        sb.boundary().map_err(|e| e.to_string())?,
    );
    let (va, vb) = (corners(c, &sa), corners(c, &sb));
    let (ea, eb) = (rim(c, &ca, &va), rim(c, &cb, &vb));
    // straight edges from each corner of face a to the same corner of face b
    let mut up = [Edge { id: 0 }; 4];
    for k in 0..4 {
        let pl = c.s.add("IFCPOLYLINE", &[list(&[va[k].0, vb[k].0])]);
        up[k] = edge(c, va[k].1, vb[k].1, pl);
    }
    let out = b > a; // the normal points from face a to face b
    let ga = bspline_surface(c, &sa);
    let gb = bspline_surface(c, &sb);
    let mut faces = vec![face(c, ga, &patch_loop(&ea), !out), face(c, gb, &patch_loop(&eb), out)];
    // side k joins rim k of a and b: (start corner, end corner) of the rim edge
    let ends = [(0, 1), (1, 2), (3, 2), (0, 3)];
    for k in 0..4 {
        let side = nurbs::ruled(&ca[k], &cb[k]).map_err(|e| e.to_string())?;
        let g = bspline_surface(c, &side);
        let (s0, s1) = ends[k];
        let lp = [(ea[k], true), (up[s1], true), (eb[k], false), (up[s0], false)];
        // the ruled normal points out of the shell on sides v0 and u1 when b > a
        let same = if k < 2 { out } else { !out };
        faces.push(face(c, g, &lp, same));
    }
    let sh = c.s.add("IFCCLOSEDSHELL", &[list(&faces)]);
    let br = c.s.add("IFCADVANCEDBREP", &[r(sh)]);
    Ok(Some((br, "AdvancedBrep")))
}

/// Triangles in level coordinates.
fn tessellated(c: &mut Ctx, sp: &FreeformSpec) -> Result<u32, String> {
    let fm = mesh_of(sp, 0.0).map_err(|e| e.to_string())?;
    let mesh = super::mep::finite_part(&fm.mesh);
    let coords: Vec<String> = mesh.positions.iter().map(|p| reals(&[p.x, p.y, p.z])).collect();
    let pts = c.s.add("IFCCARTESIANPOINTLIST3D", &[format!("({})", coords.join(",")), s(NONE)]);
    let idx: Vec<String> = mesh
        .triangles
        .iter()
        .map(|t| format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1))
        .collect();
    // a mesh with pieces left out is no longer closed
    let closed = logical(fm.solid && mesh.triangles.len() == fm.mesh.triangles.len());
    Ok(c.s.add(
        "IFCTRIANGULATEDFACESET",
        &[r(pts), s(NONE), closed, format!("({})", idx.join(",")), s(NONE)],
    ))
}

/// Freeform elements of one level.
pub(super) fn freeforms(
    c: &mut Ctx,
    model: &Model,
    level: i64,
    g: &Geo,
    out: &mut Vec<u32>,
) -> Result<(), String> {
    let rows = model.freeforms(Some(level)).map_err(|e| e.to_string())?;
    for (id, sp) in rows {
        let Ok(q) = model.freeform_quantities(id) else {
            continue; // a patch that does not tessellate is not written
        };
        let tri = tessellated(c, &sp)?;
        let mut reps = vec![];
        let ex = exact(c, &sp)?;
        let fallback = if ex.is_some() { "Body-FallBack" } else { "Body" };
        if let Some((item, kind)) = ex {
            reps.push(c.s.add(
                "IFCSHAPEREPRESENTATION",
                &[r(g.body_ctx), text("Body"), text(kind), list(&[item])],
            ));
        }
        reps.push(c.s.add(
            "IFCSHAPEREPRESENTATION",
            &[r(g.body_ctx), text(fallback), text("Tessellation"), list(&[tri])],
        ));
        let pds = c.s.add("IFCPRODUCTDEFINITIONSHAPE", &[s(NONE), s(NONE), list(&reps)]);
        let o = c.point3(0.0, 0.0, 0.0);
        let ax = c.axis3(o, None);
        let pl = c.placement(Some(g.storey_pl), ax);
        let label = if sp.name.is_empty() { format!("Freeform {id}") } else { sp.name.clone() };
        let ent = c.rooted(
            "IFCBUILDINGELEMENTPROXY",
            "freeform",
            id,
            &label,
            &[text("Freeform"), r(pl), r(pds), s(NONE), s(".ELEMENT.")],
        );
        out.push(ent);
        super::stairs::material(c, "freeform", id, &sp.material, &[ent]);
        let lab = |v: &str| format!("IFCLABEL({})", text(v));
        let fields = [
            ("Origin", lab(&sp.origin)),
            ("Thickness", format!("IFCLENGTHMEASURE({})", real(sp.thickness))),
            ("Side", lab(sp.side.key())),
            ("Degree", lab(&format!("{} x {}", sp.surface.deg_u(), sp.surface.deg_v()))),
            ("ControlNet", lab(&format!("{} x {}", sp.surface.nu(), sp.surface.nv()))),
            ("Trimmed", format!("IFCBOOLEAN({})", logical(!sp.trim.is_empty()))),
            ("ExactGeometry", format!("IFCBOOLEAN({})", logical(ex.is_some()))),
        ];
        let props: Vec<u32> = fields
            .into_iter()
            .map(|(n, v)| c.s.add("IFCPROPERTYSINGLEVALUE", &[text(n), s(NONE), v, s(NONE)]))
            .collect();
        let set = c.rooted("IFCPROPERTYSET", "freeform-pset", id, "DDC_Freeform", &[list(&props)]);
        c.rel("IFCRELDEFINESBYPROPERTIES", "freeform-pset-rel", id, &[list(&[ent]), r(set)]);
        let mut qs = vec![("GrossSurfaceArea", 1, q.area)];
        if q.volume > 0.0 {
            qs.push(("GrossVolume", 2, q.volume));
        }
        qto(c, g, "freeform-qto", id, "BaseQuantities", ent, &qs);
    }
    Ok(())
}

#[cfg(test)]
#[path = "freeform_tests.rs"]
mod tests;
