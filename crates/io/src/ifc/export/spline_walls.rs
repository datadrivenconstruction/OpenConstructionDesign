// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Spline walls: one IfcWall per `wall_spline` row, not one per arc piece of its chain. The axis
//! is the exact curve (IfcBSplineCurveWithKnots, rational when weighted), the body one
//! extrusion per layer from the true offsets of the axis, the quantities the model's exact
//! ones. Openings stay on the arc pieces' frames and void this wall.

use ddc_geom::arc::CHORD_SAG;
use ddc_geom::nurbs::Curve;
use ddc_geom::wall_spline::SplineAxis;
use ddc_geom::V2;
use ddc_model::{Model, SplineWallSpec};

use super::{list, r, real, reals, s, text, Ctx, NONE};

/// What the wall needs from the exporter: its placement and plan-to-local map (the frame of
/// the first piece met), the representation contexts and the material layer set.
pub(super) struct Host<'a> {
    pub wpl: u32,
    pub loc: &'a dyn Fn(V2) -> (f64, f64),
    pub body_ctx: u32,
    pub axis_ctx: u32,
    /// Material layer set and its total thickness.
    pub set: (u32, f64),
    /// Length, area and volume units.
    pub units: [u32; 3],
}

/// Distinct knot values and their multiplicities.
fn knot_runs(knots: &[f64]) -> (Vec<f64>, Vec<i64>) {
    let (mut vals, mut mult): (Vec<f64>, Vec<i64>) = (vec![], vec![]);
    for &k in knots {
        let same = vals.last().is_some_and(|&v: &f64| (k - v).abs() <= 1e-12 * (1.0 + v.abs()));
        match mult.last_mut() {
            Some(m) if same => *m += 1,
            _ => {
                vals.push(k);
                mult.push(1);
            }
        }
    }
    (vals, mult)
}

/// The axis as an IFC curve in the wall frame: rational when any weight differs from 1.
pub(super) fn axis_curve(c: &mut Ctx, cv: &Curve, loc: &dyn Fn(V2) -> (f64, f64)) -> u32 {
    let pts: Vec<u32> = cv
        .ctrl()
        .iter()
        .map(|p| {
            let (x, y) = loc(ddc_geom::v2(p.x, p.y));
            c.point2(x, y)
        })
        .collect();
    let (vals, mult) = knot_runs(cv.knots());
    let mult: Vec<String> = mult.iter().map(i64::to_string).collect();
    let mut a = vec![
        cv.degree().to_string(),
        list(&pts),
        s(".UNSPECIFIED."),
        s(".F."),
        s(".F."),
        format!("({})", mult.join(",")),
        reals(&vals),
        s(".UNSPECIFIED."),
    ];
    if cv.weights().iter().all(|w| (w - 1.0).abs() <= 1e-12) {
        c.s.add("IFCBSPLINECURVEWITHKNOTS", &a)
    } else {
        a.push(reals(cv.weights()));
        c.s.add("IFCRATIONALBSPLINECURVEWITHKNOTS", &a)
    }
}

/// Writes the IfcWall of spline wall `sid` (material and quantities related) and returns it.
pub(super) fn wall(
    c: &mut Ctx,
    model: &Model,
    sid: i64,
    sw: &SplineWallSpec,
    layers: &[f64],
    h: &Host,
) -> Result<u32, String> {
    let err = |e: String| format!("spline wall {sid}: {e}");
    let ax = SplineAxis::new(sw.axis.clone()).map_err(|e| err(format!("{e:?}")))?;
    let (set, total) = h.set;
    let off0 = sw.offset0.unwrap_or(-total / 2.0);
    // body: one extrusion per layer between its faces
    let mut solids = vec![];
    let mut lo = off0;
    for t in layers {
        let ring = ax.layer_ring(lo, lo + t, CHORD_SAG);
        lo += t;
        let mut pts: Vec<u32> = ring
            .iter()
            .map(|&p| {
                let (x, y) = (h.loc)(p);
                c.point2(x, y)
            })
            .collect();
        pts.push(pts[0]);
        let poly = c.s.add("IFCPOLYLINE", &[list(&pts)]);
        let prof = c.s.add(
            "IFCARBITRARYCLOSEDPROFILEDEF",
            &[s(".AREA."), s(NONE), r(poly)],
        );
        let o3 = c.point3(0.0, 0.0, 0.0);
        let sax = c.axis3(o3, None);
        solids.push(c.s.add(
            "IFCEXTRUDEDAREASOLID",
            &[r(prof), r(sax), r(c.dir_z), real(sw.height)],
        ));
    }
    let body = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[r(h.body_ctx), text("Body"), text("SweptSolid"), list(&solids)],
    );
    let line = axis_curve(c, &sw.axis, h.loc);
    let axis = c.s.add(
        "IFCSHAPEREPRESENTATION",
        &[r(h.axis_ctx), text("Axis"), text("Curve2D"), list(&[line])],
    );
    let pds = c.s.add(
        "IFCPRODUCTDEFINITIONSHAPE",
        &[s(NONE), s(NONE), list(&[axis, body])],
    );
    let wall = c.rooted(
        "IFCWALL",
        "wall_spline",
        sid,
        &format!("Spline wall {sid}"),
        &[s(NONE), r(h.wpl), r(pds), s(NONE), s(".STANDARD.")],
    );
    let usage = c.s.add(
        "IFCMATERIALLAYERSETUSAGE",
        &[r(set), s(".AXIS2."), s(".POSITIVE."), real(off0), s(NONE)],
    );
    c.rel(
        "IFCRELASSOCIATESMATERIAL",
        "mat-spline",
        sid,
        &[list(&[wall]), r(usage)],
    );
    // quantities: the model's exact ones (joins at the outer ends included)
    let qs = model
        .spline_wall_quantities(sid)
        .map_err(|e| err(e.to_string()))?;
    let net: f64 = qs.iter().map(|q| q.volume).sum();
    let gross: f64 = qs.iter().map(|q| q.plan_area).sum::<f64>() * sw.height;
    let [len_u, _, vol_u] = h.units;
    let q = |c: &mut Ctx, name: &str, ent: &str, unit: u32, v: f64| {
        c.s.add(ent, &[text(name), s(NONE), r(unit), real(v), s(NONE)])
    };
    let ql = [
        q(c, "Length", "IFCQUANTITYLENGTH", len_u, ax.length()),
        q(c, "Height", "IFCQUANTITYLENGTH", len_u, sw.height),
        q(c, "NetVolume", "IFCQUANTITYVOLUME", vol_u, net),
        q(c, "GrossVolume", "IFCQUANTITYVOLUME", vol_u, gross),
    ];
    let eq = c.rooted(
        "IFCELEMENTQUANTITY",
        "qto-spline",
        sid,
        "Qto_WallBaseQuantities",
        &[s(NONE), list(&ql)],
    );
    c.rel(
        "IFCRELDEFINESBYPROPERTIES",
        "qto-spline-rel",
        sid,
        &[list(&[wall]), r(eq)],
    );
    Ok(wall)
}

#[cfg(test)]
mod tests {
    use super::knot_runs;

    #[test]
    fn knots_become_values_and_multiplicities() {
        let (v, m) = knot_runs(&[0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(v, vec![0.0, 0.5, 1.0]);
        assert_eq!(m, vec![4, 1, 4]);
    }
}
