//! Material look and physics from the file: surface colour (IfcMaterialDefinitionRepresentation
//! -> IfcStyledItem -> IfcSurfaceStyle -> rendering) and the IfcMaterialProperties values
//! MassDensity, ThermalConductivity, SpecificHeatCapacity (SI). They fill NEW material rows only.

use std::collections::HashMap;

use super::{PMaterial, Rd};
use crate::ifc::reader::{StepFile, Value};

/// Colour of a style entity: rendering colour, or the plain shading colour.
fn rgb_of(f: &StepFile, style: u32) -> Option<u32> {
    let a = f.leaf(style)?;
    let col = match f.type_of(style)? {
        "IFCSURFACESTYLERENDERING" | "IFCSURFACESTYLESHADING" => Rd::at(&a, 0).as_ref()?,
        _ => return None,
    };
    let c = f.leaf(col)?;
    if f.type_of(col)? != "IFCCOLOURRGB" {
        return None;
    }
    let ch = |i: usize| -> Option<u32> {
        let v = Rd::at(&c, i).as_f64()?;
        v.is_finite().then(|| (v.clamp(0.0, 1.0) * 255.0).round() as u32)
    };
    Some((ch(1)? << 16) | (ch(2)? << 8) | ch(3)?)
}

/// First colour found below a style reference (IFC4 surface style, or an IFC2X3 assignment).
fn colour(f: &StepFile, id: u32, depth: usize) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    let a = f.leaf(id)?;
    match f.type_of(id)? {
        "IFCSURFACESTYLE" => Rd::refs_at(&a, 2)
            .into_iter()
            .find_map(|s| rgb_of(f, s)),
        "IFCPRESENTATIONSTYLEASSIGNMENT" => Rd::refs_at(&a, 0)
            .into_iter()
            .find_map(|s| colour(f, s, depth + 1)),
        _ => None,
    }
}

fn number(f: &StepFile, prop: u32) -> Option<(String, f64)> {
    let a = f.leaf(prop)?;
    if f.type_of(prop)? != "IFCPROPERTYSINGLEVALUE" {
        return None;
    }
    let name = Rd::at(&a, 0).as_str()?.to_string();
    let v = match Rd::at(&a, 2) {
        Value::Typed(_, x) if x.len() == 1 => x[0].as_f64()?,
        v => v.as_f64()?,
    };
    (v.is_finite() && v >= 0.0).then_some((name, v))
}

/// Materials named in the file that carry a colour or a physical value.
pub(super) fn read(f: &StepFile) -> Vec<PMaterial> {
    let mut out: HashMap<u32, PMaterial> = HashMap::new();
    let mut order: Vec<u32> = vec![];
    for (id, ty) in f.iter() {
        if ty != "IFCMATERIAL" {
            continue;
        }
        let Some(name) = f
            .leaf(id)
            .and_then(|a| Rd::at(&a, 0).as_str().map(str::to_string))
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        order.push(id);
        out.insert(
            id,
            PMaterial {
                name,
                ..Default::default()
            },
        );
    }
    for (id, ty) in f.iter() {
        match ty {
            "IFCMATERIALDEFINITIONREPRESENTATION" => {
                let Some(a) = f.leaf(id) else { continue };
                let Some(m) = Rd::at(&a, 3).as_ref().and_then(|m| out.get_mut(&m)) else {
                    continue;
                };
                for rep in Rd::refs_at(&a, 2) {
                    let Some(ra) = f.leaf(rep) else { continue };
                    for item in Rd::refs_at(&ra, 3) {
                        let Some(ia) = f.leaf(item) else { continue };
                        if f.type_of(item) != Some("IFCSTYLEDITEM") {
                            continue;
                        }
                        let c = Rd::refs_at(&ia, 1)
                            .into_iter()
                            .find_map(|s| colour(f, s, 0));
                        if m.color.is_none() {
                            m.color = c;
                        }
                    }
                }
            }
            "IFCMATERIALPROPERTIES" => {
                let Some(a) = f.leaf(id) else { continue };
                let Some(m) = Rd::at(&a, 3).as_ref().and_then(|m| out.get_mut(&m)) else {
                    continue;
                };
                for p in Rd::refs_at(&a, 2) {
                    match number(f, p) {
                        Some((n, v)) if n == "MassDensity" => m.density = m.density.or(Some(v)),
                        Some((n, v)) if n == "ThermalConductivity" => {
                            m.conductivity = m.conductivity.or(Some(v));
                        }
                        Some((n, v)) if n == "SpecificHeatCapacity" => {
                            m.specific_heat = m.specific_heat.or(Some(v));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut seen = std::collections::HashSet::new();
    order
        .into_iter()
        .filter_map(|id| out.remove(&id))
        .filter(|m| {
            (m.color.is_some()
                || m.density.is_some()
                || m.conductivity.is_some()
                || m.specific_heat.is_some())
                && seen.insert(m.name.clone())
        })
        .collect()
}
