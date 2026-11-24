// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Element data → `prop` rows: attributes, type, property sets (type then instance),
//! quantities, materials, classification; project georeferencing. Keys are "Set.Name".

use std::collections::HashMap;

use super::{ImportError, PProp, PVal, Rd};
use crate::ifc::reader::{Entity, StepFile, Value};

/// Unit factors to SI (m, rad, m2, m3).
#[derive(Debug, Clone, Copy)]
pub struct Units {
    pub length: f64,
    pub angle: f64,
    pub area: f64,
    pub volume: f64,
}

pub(super) fn si_prefix(p: Option<&str>) -> Option<f64> {
    Some(match p {
        None => 1.0,
        Some("EXA") => 1e18,
        Some("PETA") => 1e15,
        Some("TERA") => 1e12,
        Some("GIGA") => 1e9,
        Some("MEGA") => 1e6,
        Some("KILO") => 1e3,
        Some("HECTO") => 1e2,
        Some("DECA") => 1e1,
        Some("DECI") => 1e-1,
        Some("CENTI") => 1e-2,
        Some("MILLI") => 1e-3,
        Some("MICRO") => 1e-6,
        Some("NANO") => 1e-9,
        Some("PICO") => 1e-12,
        Some("FEMTO") => 1e-15,
        Some("ATTO") => 1e-18,
        Some(_) => return None,
    })
}

/// SI factor of a unit entity of the given SI name and power (area 2, volume 3).
fn unit_k(f: &StepFile, u: u32, name: &str, pow: i32, depth: usize) -> Option<f64> {
    let e = f.get(u)?;
    let k = match f.type_of(u)? {
        "IFCSIUNIT" => {
            if Rd::arg(e, 3).as_enum() != Some(name) {
                return None;
            }
            si_prefix(Rd::arg(e, 2).as_enum())?.powi(pow)
        }
        "IFCCONVERSIONBASEDUNIT" | "IFCCONVERSIONBASEDUNITWITHOFFSET" if depth < 4 => {
            let m = f.get(Rd::arg(e, 3).as_ref()?)?;
            Rd::arg(m, 0).as_f64()? * unit_k(f, Rd::arg(m, 1).as_ref()?, name, pow, depth + 1)?
        }
        _ => return None,
    };
    (k > 0.0 && k.is_finite()).then_some(k)
}

pub fn units(f: &StepFile) -> Result<Units, ImportError> {
    let mut u = Units {
        length: 1.0,
        angle: 1.0,
        area: 1.0,
        volume: 1.0,
    };
    let (mut area, mut volume) = (None, None);
    let mut length = None;
    for (id, ty) in f.iter() {
        if ty != "IFCUNITASSIGNMENT" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        for x in Rd::refs(e, 0) {
            let Some(ue) = f.get(x) else { continue };
            match Rd::arg(ue, 1).as_enum() {
                Some("LENGTHUNIT") if length.is_none() => {
                    length = Some(super::unit_factor(f, x)?);
                }
                Some("PLANEANGLEUNIT") => {
                    if let Some(k) = unit_k(f, x, "RADIAN", 1, 0) {
                        u.angle = k;
                    }
                }
                Some("AREAUNIT") => area = unit_k(f, x, "SQUARE_METRE", 2, 0),
                Some("VOLUMEUNIT") => volume = unit_k(f, x, "CUBIC_METRE", 3, 0),
                _ => {}
            }
        }
        break;
    }
    u.length = length.unwrap_or(1.0);
    u.area = area.unwrap_or(u.length * u.length);
    u.volume = volume.unwrap_or(u.length * u.length * u.length);
    Ok(u)
}

/// Converted property value: (value, unit).
fn value(u: &Units, v: &Value) -> Option<(PVal, &'static str)> {
    Some(match v {
        Value::Typed(t, a) if a.len() == 1 => {
            let t = t.as_str();
            let real = || a[0].as_f64();
            match t {
                "IFCLENGTHMEASURE" | "IFCPOSITIVELENGTHMEASURE" | "IFCNONNEGATIVELENGTHMEASURE" => {
                    (PVal::Real(real()? * u.length), "m")
                }
                "IFCAREAMEASURE" => (PVal::Real(real()? * u.area), "m2"),
                "IFCVOLUMEMEASURE" => (PVal::Real(real()? * u.volume), "m3"),
                "IFCPLANEANGLEMEASURE" | "IFCPOSITIVEPLANEANGLEMEASURE" => {
                    (PVal::Real((real()? * u.angle).to_degrees()), "deg")
                }
                "IFCMASSMEASURE" => (PVal::Real(real()?), "kg"),
                "IFCTHERMODYNAMICTEMPERATUREMEASURE" => (PVal::Real(real()?), "K"),
                "IFCTHERMALTRANSMITTANCEMEASURE" => (PVal::Real(real()?), "W/(m2K)"),
                "IFCBOOLEAN" | "IFCLOGICAL" => match a[0].as_enum()? {
                    "T" => (PVal::Bool(true), ""),
                    "F" => (PVal::Bool(false), ""),
                    _ => (PVal::Text("unknown".into()), ""),
                },
                _ => match &a[0] {
                    Value::Int(i) => (PVal::Int(*i), ""),
                    Value::Real(r) => (PVal::Real(*r), ""),
                    Value::Str(s) => (PVal::Text(s.clone()), ""),
                    Value::Enum(s) => (PVal::Text(s.clone()), ""),
                    _ => return None,
                },
            }
        }
        Value::Int(i) => (PVal::Int(*i), ""),
        Value::Real(r) => (PVal::Real(*r), ""),
        Value::Str(s) => (PVal::Text(s.clone()), ""),
        Value::Enum(s) => (PVal::Text(s.clone()), ""),
        _ => return None,
    })
    .filter(|(v, _)| !matches!(v, PVal::Real(r) if !r.is_finite()))
}

/// Value with the property's own unit (`IfcProperty*Value.Unit`) applied; project units
/// are used when it is absent or not understood.
fn value_own(
    f: &StepFile,
    u: &Units,
    v: &Value,
    own: Option<u32>,
) -> Option<(PVal, &'static str)> {
    let Some(id) = own else { return value(u, v) };
    let x = match v {
        Value::Typed(_, a) if a.len() == 1 => a[0].as_f64(),
        Value::Real(_) | Value::Int(_) => v.as_f64(),
        _ => None,
    };
    let (Some(x), Some(ue)) = (x, f.get(id)) else {
        return value(u, v);
    };
    let r = match Rd::arg(ue, 1).as_enum() {
        Some("LENGTHUNIT") => unit_k(f, id, "METRE", 1, 0).map(|k| (x * k, "m")),
        Some("AREAUNIT") => unit_k(f, id, "SQUARE_METRE", 2, 0).map(|k| (x * k, "m2")),
        Some("VOLUMEUNIT") => unit_k(f, id, "CUBIC_METRE", 3, 0).map(|k| (x * k, "m3")),
        Some("PLANEANGLEUNIT") => {
            unit_k(f, id, "RADIAN", 1, 0).map(|k| ((x * k).to_degrees(), "deg"))
        }
        // The SI mass unit is the gram; internal mass is kg.
        Some("MASSUNIT") => unit_k(f, id, "GRAM", 1, 0).map(|k| (x * k / 1e3, "kg")),
        _ => None,
    };
    match r.filter(|(r, _)| r.is_finite()) {
        Some((r, unit)) => Some((PVal::Real(r), unit)),
        None => value(u, v),
    }
}

fn text_of(v: &PVal) -> String {
    match v {
        PVal::Int(i) => i.to_string(),
        PVal::Real(r) => r.to_string(),
        PVal::Text(s) => s.clone(),
        PVal::Bool(b) => b.to_string(),
    }
}

/// Relations the element data needs.
pub struct Ctx<'a> {
    pub f: &'a StepFile,
    pub u: &'a Units,
    pub props: &'a HashMap<u32, Vec<u32>>,
    pub typed: &'a HashMap<u32, u32>,
    pub material: &'a HashMap<u32, u32>,
    pub classif: &'a HashMap<u32, Vec<u32>>,
}

/// Names of the element's quantity sets (`;`-separated): their values are written back as
/// quantities, not as properties.
pub const QSETS: &str = "IFC.QuantitySets";

struct Out(Vec<PProp>);

impl Out {
    fn put(&mut self, key: String, value: PVal, unit: &str) {
        if let Some(p) = self.0.iter_mut().find(|p| p.key == key) {
            p.value = value;
            p.unit = unit.to_string();
        } else {
            self.0.push(PProp {
                key,
                value,
                unit: unit.to_string(),
            });
        }
    }
    /// Adds `item` to the `;`-separated text list `key`.
    fn list(&mut self, key: &str, item: &str) {
        let cur = self.0.iter().find(|p| p.key == key).map(|p| text_of(&p.value));
        let v = match cur {
            Some(c) if c.split(';').any(|x| x == item) => return,
            Some(c) => format!("{c};{item}"),
            None => item.to_string(),
        };
        self.put(key.to_string(), PVal::Text(v), "");
    }
    fn text(&mut self, key: &str, v: Option<&str>) {
        if let Some(s) = v.filter(|s| !s.is_empty()) {
            self.put(key.to_string(), PVal::Text(s.to_string()), "");
        }
    }
}

impl Ctx<'_> {
    fn name_of(&self, id: u32, arg: usize) -> Option<String> {
        let a = self.f.leaf(id)?;
        Rd::at(&a, arg)
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// Type object name of an element.
    pub fn type_name(&self, id: u32) -> Option<String> {
        self.typed.get(&id).and_then(|t| self.name_of(*t, 2))
    }

    fn materials(&self, mat: u32, out: &mut Vec<String>, layers: &mut Vec<String>, depth: usize) {
        let f = self.f;
        let Some(e) = f.get(mat) else { return };
        if depth > 6 {
            return;
        }
        let ty = f.type_of(mat).unwrap_or("");
        match ty {
            "IFCMATERIAL" => {
                if let Some(n) = self.name_of(mat, 0) {
                    out.push(n);
                }
            }
            "IFCMATERIALLAYERSETUSAGE"
            | "IFCMATERIALPROFILESETUSAGE"
            | "IFCMATERIALPROFILESETUSAGETAPERING" => {
                if let Some(s) = Rd::arg(e, 0).as_ref() {
                    self.materials(s, out, layers, depth + 1);
                }
            }
            "IFCMATERIALLAYERSET" => {
                for l in Rd::refs(e, 0) {
                    let Some(le) = f.get(l) else { continue };
                    let n = Rd::arg(le, 0)
                        .as_ref()
                        .and_then(|m| self.name_of(m, 0))
                        .unwrap_or_else(|| "Material".into());
                    let t = Rd::arg(le, 1).as_f64().unwrap_or(0.0) * self.u.length;
                    layers.push(format!("{n} {}", (t * 1e9).round() / 1e9));
                    out.push(n);
                }
            }
            "IFCMATERIALLIST" => {
                for m in Rd::refs(e, 0) {
                    self.materials(m, out, layers, depth + 1);
                }
            }
            "IFCMATERIALCONSTITUENTSET" | "IFCMATERIALPROFILESET" => {
                for c in Rd::refs(e, 2) {
                    let Some(ce) = f.get(c) else { continue };
                    if let Some(m) = Rd::arg(ce, 2).as_ref() {
                        self.materials(m, out, layers, depth + 1);
                    }
                }
            }
            "IFCMATERIALCONSTITUENT" | "IFCMATERIALPROFILE" => {
                if let Some(m) = Rd::arg(e, 2).as_ref() {
                    self.materials(m, out, layers, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn material_of(&self, id: u32) -> Option<u32> {
        self.material
            .get(&id)
            .or_else(|| self.typed.get(&id).and_then(|t| self.material.get(t)))
            .copied()
    }

    /// First material name of an element (instance, else its type).
    pub fn material_name(&self, id: u32) -> Option<String> {
        let mut v = vec![];
        self.materials(self.material_of(id)?, &mut v, &mut vec![], 0);
        v.into_iter().next()
    }

    fn set(&self, o: &mut Out, ps: u32) {
        let f = self.f;
        let Some(pa) = f.leaf(ps) else { return };
        let pe: &[Value] = &pa;
        let ty = f.type_of(ps).unwrap_or("");
        let name = Rd::at(pe, 2)
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(ty);
        match ty {
            "IFCPROPERTYSET" => {
                for p in Rd::refs_at(pe, 4) {
                    self.prop(o, name, p, 0);
                }
            }
            "IFCELEMENTQUANTITY" => {
                for q in Rd::refs_at(pe, 5) {
                    let Some(qa) = f.leaf(q) else { continue };
                    let qe: &[Value] = &qa;
                    let Some(qn) = Rd::at(qe, 0).as_str() else {
                        continue;
                    };
                    let Some(v) = Rd::at(qe, 3).as_f64().filter(|v| v.is_finite()) else {
                        continue;
                    };
                    let (v, unit) = match f.type_of(q).unwrap_or("") {
                        "IFCQUANTITYLENGTH" => (PVal::Real(v * self.u.length), "m"),
                        "IFCQUANTITYAREA" => (PVal::Real(v * self.u.area), "m2"),
                        "IFCQUANTITYVOLUME" => (PVal::Real(v * self.u.volume), "m3"),
                        "IFCQUANTITYWEIGHT" => (PVal::Real(v), "kg"),
                        "IFCQUANTITYTIME" => (PVal::Real(v), "s"),
                        "IFCQUANTITYCOUNT" => (PVal::Real(v), ""),
                        _ => continue,
                    };
                    o.put(format!("{name}.{qn}"), v, unit);
                }
                o.list(QSETS, name);
            }
            _ => {}
        }
    }

    fn prop(&self, o: &mut Out, set: &str, p: u32, depth: usize) {
        let f = self.f;
        let Some(pa) = f.leaf(p) else { return };
        let pe: &[Value] = &pa;
        let Some(pn) = Rd::at(pe, 0).as_str().filter(|s| !s.is_empty()) else {
            return;
        };
        let key = format!("{set}.{pn}");
        let u = self.u;
        let list = |i: usize, own: Option<u32>| -> Option<(PVal, &'static str)> {
            let vs: Vec<_> = Rd::at(pe, i)
                .as_list()?
                .iter()
                .filter_map(|v| value_own(f, u, v, own))
                .collect();
            match vs.len() {
                0 => None,
                1 => vs.into_iter().next(),
                _ => {
                    let unit = vs[0].1;
                    let t = vs
                        .iter()
                        .map(|v| text_of(&v.0))
                        .collect::<Vec<_>>()
                        .join("; ");
                    Some((PVal::Text(t), unit))
                }
            }
        };
        let v = match f.type_of(p).unwrap_or("") {
            "IFCPROPERTYSINGLEVALUE" => value_own(f, u, Rd::at(pe, 2), Rd::at(pe, 3).as_ref()),
            "IFCPROPERTYENUMERATEDVALUE" => {
                // The unit lives on the referenced IfcPropertyEnumeration (arg 2).
                let own = Rd::at(pe, 3)
                    .as_ref()
                    .and_then(|r| f.leaf(r))
                    .and_then(|a| Rd::at(&a, 2).as_ref());
                list(2, own)
            }
            "IFCPROPERTYLISTVALUE" => list(2, Rd::at(pe, 3).as_ref()),
            "IFCPROPERTYBOUNDEDVALUE" => {
                let own = Rd::at(pe, 4).as_ref();
                let (hi, lo) = (
                    value_own(f, u, Rd::at(pe, 2), own),
                    value_own(f, u, Rd::at(pe, 3), own),
                );
                match (lo, hi) {
                    (Some(l), Some(h)) => Some((
                        PVal::Text(format!("{}..{}", text_of(&l.0), text_of(&h.0))),
                        l.1,
                    )),
                    (a, b) => a.or(b),
                }
            }
            "IFCPROPERTYREFERENCEVALUE" => Rd::at(pe, 3)
                .as_ref()
                .and_then(|r| self.name_of(r, 0).or_else(|| self.name_of(r, 2)))
                .map(|s| (PVal::Text(s), "")),
            "IFCCOMPLEXPROPERTY" if depth < 4 => {
                for c in Rd::refs_at(pe, 3) {
                    self.prop(o, &key, c, depth + 1);
                }
                None
            }
            _ => None,
        };
        if let Some((v, unit)) = v {
            o.put(key, v, unit);
        }
    }

    /// All data of an element as properties.
    pub fn element(&self, id: u32) -> Vec<PProp> {
        let f = self.f;
        let mut o = Out(vec![]);
        let Some(e) = f.get(id) else { return o.0 };
        o.text("IFC.Class", f.type_of(id));
        o.text("IFC.GlobalId", Rd::arg(e, 0).as_str());
        o.text("IFC.Name", Rd::arg(e, 2).as_str());
        o.text("IFC.Description", Rd::arg(e, 3).as_str());
        o.text("IFC.ObjectType", Rd::arg(e, 4).as_str());
        o.text("IFC.Tag", Rd::arg(e, 7).as_str());
        o.text("IFC.PredefinedType", Rd::arg(e, 8).as_enum());
        if let Some(t) = self.typed.get(&id) {
            o.text("IFC.Type", self.name_of(*t, 2).as_deref());
            o.text("IFC.TypeClass", f.type_of(*t));
            if let Some(te) = f.get(*t) {
                for ps in Rd::refs(te, 5) {
                    self.set(&mut o, ps);
                }
            }
        }
        for ps in self.props.get(&id).into_iter().flatten() {
            self.set(&mut o, *ps);
        }
        if let Some(m) = self.material_of(id) {
            let (mut names, mut layers) = (vec![], vec![]);
            self.materials(m, &mut names, &mut layers, 0);
            names.dedup();
            if !names.is_empty() {
                o.put("Material.Name".into(), PVal::Text(names.join(" / ")), "");
            }
            if !layers.is_empty() {
                o.put("Material.Layers".into(), PVal::Text(layers.join("; ")), "");
            }
        }
        for c in self.classif.get(&id).into_iter().flatten() {
            let Some(ce) = f.get(*c) else { continue };
            if f.type_of(*c) != Some("IFCCLASSIFICATIONREFERENCE") {
                continue;
            }
            let src = Rd::arg(ce, 3)
                .as_ref()
                .and_then(|s| self.name_of(s, 3))
                .unwrap_or_else(|| "Classification".into());
            let code = [Rd::arg(ce, 1).as_str(), Rd::arg(ce, 2).as_str()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !code.is_empty() {
                o.put(format!("Classification.{src}"), PVal::Text(code), "");
            }
        }
        o.0
    }
}

/// Compound plane angle (deg, min, s, millionths) → decimal degrees.
fn compound(v: &Value) -> Option<f64> {
    let l = v.as_list()?;
    let p: Vec<f64> = l.iter().filter_map(Value::as_f64).collect();
    let (d, m, s, us) = (
        *p.first()?,
        p.get(1).copied().unwrap_or(0.0),
        p.get(2).copied().unwrap_or(0.0),
        p.get(3).copied().unwrap_or(0.0),
    );
    // all parts share the sign of the degrees
    Some(d + m / 60.0 + (s + us * 1e-6) / 3600.0)
}

fn ent_of<'a>(f: &'a StepFile, ty: &str) -> Option<&'a Entity> {
    f.iter().find(|x| x.1 == ty).and_then(|x| f.get(x.0))
}

/// Coordinate system of the file: IfcProjectedCRS name (code) and description, vertical datum,
/// and the map conversion base point (project length unit -> metres) with true north. `None`:
/// no map conversion or a CRS without a usable `AUTHORITY:CODE` name.
pub fn site_crs(f: &StepFile, u: &Units) -> Option<ddc_model::SiteProps> {
    let m = ent_of(f, "IFCMAPCONVERSION")?;
    let crs = Rd::arg(m, 1).as_ref().and_then(|c| f.get(c))?;
    let text = |i: usize| Rd::arg(crs, i).as_str().map(|t| t.trim().to_string());
    let code = text(0).filter(|c| ddc_model::crs_code_valid(c) && !c.is_empty())?;
    let map_unit = Rd::arg(crs, 6)
        .as_ref()
        .and_then(|x| unit_k(f, x, "METRE", 1, 0))
        .unwrap_or(u.length);
    let at = |i: usize| Rd::arg(m, i).as_f64().filter(|v| v.is_finite());
    let (e, n, h) = (at(2)?, at(3)?, at(4).unwrap_or(0.0));
    let north = match (at(5), at(6)) {
        (Some(a), Some(b)) => -b.atan2(a).to_degrees(),
        _ => 0.0,
    };
    let vertical = text(3).filter(|v| ddc_model::crs_code_valid(v)).unwrap_or_default();
    Some(ddc_model::SiteProps {
        north_deg: north,
        base: (e * map_unit, n * map_unit, h * map_unit),
        crs_code: code,
        crs_name: text(1).unwrap_or_default(),
        crs_vertical: vertical,
        ..Default::default()
    })
}

/// Project georeferencing: map conversion, CRS, true north, site reference point.
pub fn georef(f: &StepFile, u: &Units) -> Vec<PProp> {
    let mut o = Out(vec![]);
    let real = |o: &mut Out, k: &str, v: Option<f64>, unit: &str| {
        if let Some(v) = v.filter(|v| v.is_finite()) {
            o.put(k.to_string(), PVal::Real(v), unit);
        }
    };
    if let Some(m) = ent_of(f, "IFCMAPCONVERSION") {
        let crs = Rd::arg(m, 1).as_ref().and_then(|c| f.get(c));
        let map_unit = crs
            .and_then(|c| Rd::arg(c, 6).as_ref())
            .and_then(|x| unit_k(f, x, "METRE", 1, 0))
            .unwrap_or(u.length);
        real(
            &mut o,
            "Georef.Eastings",
            Rd::arg(m, 2).as_f64().map(|v| v * map_unit),
            "m",
        );
        real(
            &mut o,
            "Georef.Northings",
            Rd::arg(m, 3).as_f64().map(|v| v * map_unit),
            "m",
        );
        real(
            &mut o,
            "Georef.OrthogonalHeight",
            Rd::arg(m, 4).as_f64().map(|v| v * map_unit),
            "m",
        );
        let (a, b) = (Rd::arg(m, 5).as_f64(), Rd::arg(m, 6).as_f64());
        if let (Some(a), Some(b)) = (a, b) {
            real(
                &mut o,
                "Georef.XAxisRotation",
                Some(b.atan2(a).to_degrees()),
                "deg",
            );
        }
        real(&mut o, "Georef.Scale", Rd::arg(m, 7).as_f64(), "");
        if let Some(c) = crs {
            o.text("Georef.CRS", Rd::arg(c, 0).as_str());
            o.text("Georef.GeodeticDatum", Rd::arg(c, 2).as_str());
            o.text("Georef.VerticalDatum", Rd::arg(c, 3).as_str());
            o.text("Georef.MapProjection", Rd::arg(c, 4).as_str());
            o.text("Georef.MapZone", Rd::arg(c, 5).as_str());
        }
    }
    for (id, ty) in f.iter() {
        if ty != "IFCGEOMETRICREPRESENTATIONCONTEXT" {
            continue;
        }
        let Some(c) = f.get(id) else { continue };
        let tn = Rd::arg(c, 5).as_ref().and_then(|d| f.get(d));
        let xy = tn
            .and_then(|d| Rd::arg(d, 0).as_list())
            .map(|l| l.iter().filter_map(Value::as_f64).collect::<Vec<_>>());
        if let Some([x, y, ..]) = xy.as_deref() {
            // angle of true north from the project +Y axis, counter-clockwise
            real(
                &mut o,
                "Georef.TrueNorth",
                Some((-x).atan2(*y).to_degrees()),
                "deg",
            );
            break;
        }
    }
    if let Some(s) = ent_of(f, "IFCSITE") {
        real(&mut o, "Site.Latitude", compound(Rd::arg(s, 9)), "deg");
        real(&mut o, "Site.Longitude", compound(Rd::arg(s, 10)), "deg");
        real(
            &mut o,
            "Site.Elevation",
            Rd::arg(s, 11).as_f64().map(|v| v * u.length),
            "m",
        );
        o.text("Site.Name", Rd::arg(s, 2).as_str());
    }
    o.0
}
