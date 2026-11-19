// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! STEP (ISO 10303-21, AP203/AP214/AP242) B-rep solids → meshes. The geometric entities share
//! their meaning with the IFC ones, minus the leading name: the file is rewritten under IFC names
//! and tessellated by the IFC geometry reader (planes, cylinders, lines, circles, ellipses,
//! B-spline curves). Faces on other surfaces are reported, not guessed. Placements of parts in
//! an assembly are not applied: each solid stays in its own part axes.

use std::collections::HashMap;
use std::fmt::Write;

use crate::ifc::{StepFile, Value};

use super::{Collect, Source};

/// STEP entity → IFC entity; every one of them drops its first (name) argument.
fn ifc_name(t: &str) -> Option<&'static str> {
    Some(match t {
        "CARTESIAN_POINT" => "IFCCARTESIANPOINT",
        "DIRECTION" => "IFCDIRECTION",
        "VECTOR" => "IFCVECTOR",
        "AXIS2_PLACEMENT_3D" => "IFCAXIS2PLACEMENT3D",
        "AXIS2_PLACEMENT_2D" => "IFCAXIS2PLACEMENT2D",
        "AXIS1_PLACEMENT" => "IFCAXIS1PLACEMENT",
        "LINE" => "IFCLINE",
        "CIRCLE" => "IFCCIRCLE",
        "ELLIPSE" => "IFCELLIPSE",
        "POLYLINE" => "IFCPOLYLINE",
        "TRIMMED_CURVE" => "IFCTRIMMEDCURVE",
        "B_SPLINE_CURVE_WITH_KNOTS" => "IFCBSPLINECURVEWITHKNOTS",
        "PLANE" => "IFCPLANE",
        "CYLINDRICAL_SURFACE" => "IFCCYLINDRICALSURFACE",
        "VERTEX_POINT" => "IFCVERTEXPOINT",
        "EDGE_CURVE" => "IFCEDGECURVE",
        "ORIENTED_EDGE" => "IFCORIENTEDEDGE",
        "EDGE_LOOP" => "IFCEDGELOOP",
        "POLY_LOOP" => "IFCPOLYLOOP",
        "VERTEX_LOOP" => "IFCVERTEXLOOP",
        "FACE_OUTER_BOUND" => "IFCFACEOUTERBOUND",
        "FACE_BOUND" => "IFCFACEBOUND",
        "ADVANCED_FACE" | "FACE_SURFACE" => "IFCADVANCEDFACE",
        "FACE" => "IFCFACE",
        "CLOSED_SHELL" => "IFCCLOSEDSHELL",
        "OPEN_SHELL" => "IFCOPENSHELL",
        "MANIFOLD_SOLID_BREP" => "IFCADVANCEDBREP",
        "FACETED_BREP" => "IFCFACETEDBREP",
        "BREP_WITH_VOIDS" => "IFCADVANCEDBREPWITHVOIDS",
        "SHELL_BASED_SURFACE_MODEL" => "IFCSHELLBASEDSURFACEMODEL",
        _ => return None,
    })
}

const SOLIDS: [&str; 4] = [
    "MANIFOLD_SOLID_BREP",
    "FACETED_BREP",
    "BREP_WITH_VOIDS",
    "SHELL_BASED_SURFACE_MODEL",
];

fn val(v: &Value, o: &mut String) {
    match v {
        Value::Null => o.push('$'),
        Value::Derived => o.push('*'),
        Value::Int(i) => {
            let _ = write!(o, "{i}");
        }
        Value::Real(r) => {
            let s = format!("{r:?}");
            o.push_str(&s);
            if !s.contains(['.', 'e', 'E']) {
                o.push('.');
            }
        }
        Value::Str(s) => {
            o.push('\'');
            o.push_str(&s.replace('\'', "''"));
            o.push('\'');
        }
        Value::Enum(e) => {
            let _ = write!(o, ".{e}.");
        }
        Value::Ref(r) => {
            let _ = write!(o, "#{r}");
        }
        Value::List(l) => {
            o.push('(');
            args(l, o);
            o.push(')');
        }
        Value::Typed(n, a) => {
            o.push_str(n);
            o.push('(');
            args(a, o);
            o.push(')');
        }
        Value::Binary(b) => {
            let _ = write!(o, "\"{b}\"");
        }
    }
}

fn args(a: &[Value], o: &mut String) {
    for (k, v) in a.iter().enumerate() {
        if k > 0 {
            o.push(',');
        }
        val(v, o);
    }
}

/// Rational B-spline curve written as a complex instance → the IFC rational curve.
fn rational_curve(parts: &[Value]) -> Option<Vec<Value>> {
    let part = |n: &str| {
        parts.iter().find_map(|p| match p {
            Value::Typed(t, a) if t == n => Some(a.clone()),
            _ => None,
        })
    };
    let mut a = part("B_SPLINE_CURVE")?;
    a.extend(part("B_SPLINE_CURVE_WITH_KNOTS")?);
    a.extend(part("RATIONAL_B_SPLINE_CURVE")?);
    Some(a)
}

/// Metres per length unit of the file (millimetres when the file does not say).
fn unit(f: &StepFile) -> f64 {
    for (id, ty) in f.iter() {
        if ty != "COMPLEX" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let a = e.args();
        let has = |n: &str| a.iter().any(|p| matches!(p, Value::Typed(t, _) if t == n));
        if !has("LENGTH_UNIT") {
            continue;
        }
        for p in a {
            match p {
                Value::Typed(t, x) if t == "SI_UNIT" => {
                    return match x.first() {
                        Some(Value::Enum(e)) if e == "MILLI" => 1e-3,
                        Some(Value::Enum(e)) if e == "CENTI" => 1e-2,
                        Some(Value::Enum(e)) if e == "KILO" => 1e3,
                        _ => 1.0,
                    }
                }
                Value::Typed(t, x) if t == "CONVERSION_BASED_UNIT" => {
                    let n = x.first().and_then(|v| v.as_str()).unwrap_or("").to_ascii_uppercase();
                    return match n.as_str() {
                        "INCH" => 0.0254,
                        "FOOT" => 0.3048,
                        _ => 1e-3,
                    };
                }
                _ => {}
            }
        }
    }
    1e-3
}

/// Colour of a styled item: the first RGB under its styles.
fn colours(f: &StepFile) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for (id, ty) in f.iter() {
        if ty != "STYLED_ITEM" && ty != "OVER_RIDING_STYLED_ITEM" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let Some(item) = e.args().get(2).and_then(|v| v.as_ref()) else { continue };
        let mut stack: Vec<u32> = e.args().get(1).map(|v| v.refs()).unwrap_or_default();
        let mut seen = 0;
        while let Some(s) = stack.pop() {
            seen += 1;
            if seen > 256 {
                break;
            }
            let Some(se) = f.get(s) else { continue };
            if f.type_of(s) == Some("COLOUR_RGB") {
                let c = |i: usize| se.args().get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let to = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                out.insert(item, format!("Colour #{:02X}{:02X}{:02X}", to(c(1)), to(c(2)), to(c(3))));
                break;
            }
            for a in se.args() {
                stack.extend(a.refs());
            }
        }
    }
    out
}

pub fn read(bytes: &[u8]) -> Result<Source, String> {
    let f = StepFile::parse_tolerant(bytes).map_err(|e| format!("STEP: {e}"))?;
    let scale = unit(&f);
    let mut text = String::from(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('','',(''),(''),'','','');\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n",
    );
    let mut items = vec![];
    let mut unknown: HashMap<String, usize> = HashMap::new();
    let col = colours(&f);
    let mut name = String::new();
    for (id, ty) in f.iter() {
        let Some(e) = f.get(id) else { continue };
        let a = e.args();
        if ty == "PRODUCT" && name.is_empty() {
            name = a.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        }
        let (n, rest): (&str, Vec<Value>) = if ty == "COMPLEX" {
            match rational_curve(a) {
                Some(x) => ("IFCRATIONALBSPLINECURVEWITHKNOTS", x),
                None => continue,
            }
        } else {
            match ifc_name(ty) {
                Some(n) => (n, a.get(1..).unwrap_or(&[]).to_vec()),
                None => {
                    if ty.ends_with("_SURFACE") || ty.ends_with("_CURVE") {
                        *unknown.entry(ty.to_string()).or_default() += 1;
                    }
                    continue;
                }
            }
        };
        let _ = write!(text, "#{id}={n}(");
        args(&rest, &mut text);
        text.push_str(");\n");
        if SOLIDS.contains(&ty) {
            items.push((id, col.get(&id).cloned().unwrap_or_else(|| "Default".into())));
        }
    }
    text.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    if items.is_empty() {
        return Err("STEP: no solids or shells".into());
    }
    let g = StepFile::parse_tolerant(text.as_bytes()).map_err(|e| format!("STEP: {e}"))?;
    let mut warnings = vec![];
    let parts = crate::ifc::item_parts(&g, scale, &items, &mut warnings);
    let mut u: Vec<_> = unknown.into_iter().collect();
    u.sort();
    for (t, n) in u {
        warnings.push(format!("{n} × {t} not tessellated"));
    }
    if f.iter().any(|(_, t)| t == "NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
        warnings.push("assembly: parts are placed in their own axes".into());
    }
    let mut c = Collect::default();
    for p in parts {
        let s = c.slot(&p.material);
        c.add(s, &p.body.pos, p.body.tris.iter().copied());
    }
    Ok(Source {
        name,
        parts: c.parts(),
        warnings,
    })
}
