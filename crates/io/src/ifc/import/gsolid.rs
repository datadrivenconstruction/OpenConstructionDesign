//! Generic solids written by this exporter (property set `DDC_GenericSolid`) come back as exact
//! solid rows with their semantic IFC class. A product that does not fit (foreign units, no
//! storey, a class no solid takes, a bad definition) is left to the other import paths.

use std::collections::HashMap;

use ddc_geom::v3;
use ddc_model::{SolidFrame, SolidIfc, SolidSpec, SOLID_IFC_CLASSES};

use super::{Entity, Plan, Rd, Why};

/// One solid of the plan; `spec.level` is set when the plan is committed.
#[derive(Debug, Clone, PartialEq)]
pub struct PSolid {
    pub level: usize,
    pub spec: SolidSpec,
}

/// Class of an entity type: None for the generic proxy, Err for a type no solid takes.
fn class_of(ty: &str, e: &Entity) -> Result<Option<SolidIfc>, Why> {
    if ty == "IFCBUILDINGELEMENTPROXY" {
        // IFC2X3 has no shading device; the export names it in ObjectType
        let ot = Rd::arg(e, 4).as_str().unwrap_or("");
        return Ok(ot.strip_prefix("IfcShadingDevice").and_then(|rest| {
            let pre = rest.strip_prefix('.');
            (rest.is_empty() || pre.is_some())
                .then(|| SolidIfc::new("IfcShadingDevice", pre).ok())
                .flatten()
        }));
    }
    let Some((name, _)) = SOLID_IFC_CLASSES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(ty))
    else {
        return Err("not a class a solid takes");
    };
    let pre = Rd::arg(e, SolidIfc::predefined_at(name)).as_enum();
    // a type this model does not know (a newer schema) keeps the class, not the type
    SolidIfc::new(name, pre)
        .or_else(|_| SolidIfc::new(name, None))
        .map(Some)
        .map_err(|_| "not a class a solid takes")
}

/// Work plane from the `Plane` text: origin, x, y, z as twelve numbers.
fn plane(t: &str) -> Option<SolidFrame> {
    let v: Vec<f64> = t
        .split_whitespace()
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if v.len() != 12 || !v.iter().all(|x| x.is_finite()) {
        return None;
    }
    let p = |i: usize| v3(v[i], v[i + 1], v[i + 2]);
    Some(SolidFrame {
        o: p(0),
        x: p(3),
        y: p(6),
        z: p(9),
    })
}

/// Adds the product `e` of type `ty` with its `DDC_GenericSolid` values `kv` to the plan.
pub(super) fn read(
    pl: &mut Plan,
    scale: f64,
    ty: &str,
    e: &Entity,
    kv: &HashMap<String, String>,
    storey: Option<usize>,
) -> Result<(), Why> {
    // the set holds metres; a file in other units was rewritten by another tool
    if (scale - 1.0).abs() > 1e-12 {
        return Err("DDC_GenericSolid in a file not in metres");
    }
    let level = storey.ok_or("DDC_GenericSolid outside a storey")?;
    let ifc = class_of(ty, e)?;
    let bad = "bad DDC_GenericSolid";
    let def: serde_json::Value =
        serde_json::from_str(kv.get("Definition").ok_or(bad)?).map_err(|_| bad)?;
    let form = ddc_model::form_of(&def).map_err(|_| bad)?;
    let frame = plane(kv.get("Plane").ok_or(bad)?).ok_or(bad)?;
    if !frame.is_valid() {
        return Err(bad);
    }
    ddc_geom::gsolid::validate(&form).map_err(|_| bad)?;
    let spec = SolidSpec {
        level: 0,
        name: Rd::arg(e, 2).as_str().unwrap_or_default().to_string(),
        void: false,
        material: kv.get("Material").cloned().unwrap_or_default(),
        frame,
        form,
        ifc,
    };
    pl.solids.push(PSolid { level, spec });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::plane;

    #[test]
    fn plane_reads_twelve_numbers_and_refuses_the_rest() {
        let f = plane("1.0 2.0 3.5 1.0 0.0 0.0 0.0 1.0 0.0 0.0 0.0 1.0").unwrap();
        assert_eq!((f.o.x, f.o.y, f.o.z), (1.0, 2.0, 3.5));
        assert!(f.is_valid());
        assert!(plane("1 2 3").is_none());
        assert!(plane("1 2 3 1 0 0 0 1 0 0 0 x").is_none());
        assert!(plane("NaN 2 3 1 0 0 0 1 0 0 0 1").is_none());
    }
}
