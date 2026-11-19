//! Foreign type objects (IfcTypeProduct with representation maps) → families. A product whose
//! body is one mapped item of its type's map, placed upright and unscaled, becomes an instance
//! of a family made from that map: mesh bodies per style, the type's property sets as text
//! parameters, category and IFC class from the classes. Anything else stays a plain mesh.

use std::collections::HashMap;

use ddc_geom::component::from_mesh::{family_text, ImportSpec, Insertion, MeshPart};
use ddc_geom::component::meshbody::MeshBody;
use ddc_geom::component::{Category, Family, Hosting};
use ddc_geom::math::v3;

use super::{dot, Entity, Frame, Rd, StepFile, Value, Why};

/// Family made from one representation map, shared by every instance of the map.
#[derive(Clone, Debug)]
pub(super) struct TypeFamily {
    pub def: String,
    pub type_name: String,
    pub type_vals: String,
}

#[derive(Default)]
pub(super) struct Types {
    fams: HashMap<u32, Result<TypeFamily, Why>>,
    styles: Option<HashMap<u32, String>>,
}

/// Placement of an instance: world origin, rotation about z.
pub(super) struct Placed {
    pub fam: TypeFamily,
    pub at: [f64; 3],
    pub rotation: f64,
}

/// Category by the entity class of the type or the occurrence (upper case, with or without
/// the `TYPE` suffix).
pub(super) fn category_of(class: &str) -> Category {
    let c = class.strip_suffix("TYPE").unwrap_or(class);
    let c = c.strip_suffix("STYLE").unwrap_or(c);
    match c {
        "IFCDOOR" => Category::Door,
        "IFCWINDOW" => Category::Window,
        "IFCFURNITURE" | "IFCFURNISHINGELEMENT" | "IFCSYSTEMFURNITUREELEMENT" => {
            Category::Furniture
        }
        "IFCSANITARYTERMINAL" | "IFCWASTETERMINAL" | "IFCFIRESUPPRESSIONTERMINAL" => {
            Category::Plumbing
        }
        "IFCLIGHTFIXTURE" | "IFCLAMP" => Category::Lighting,
        "IFCAIRTERMINAL" => Category::AirTerminal,
        "IFCPIPEFITTING" => Category::PipeFitting,
        "IFCDUCTFITTING" => Category::DuctFitting,
        "IFCSTAIR" | "IFCSTAIRFLIGHT" => Category::Stairs,
        "IFCMEMBER" | "IFCBEAM" | "IFCCOLUMN" | "IFCPLATE" => Category::Structural,
        "IFCGEOGRAPHICELEMENT" => Category::Planting,
        "IFCOUTLET" | "IFCSWITCHINGDEVICE" | "IFCELECTRICAPPLIANCE" | "IFCELECTRICDISTRIBUTIONBOARD"
        | "IFCCOMMUNICATIONSAPPLIANCE" | "IFCALARM" | "IFCSENSOR" => Category::Electrical,
        "IFCUNITARYEQUIPMENT" | "IFCBOILER" | "IFCCHILLER" | "IFCPUMP" | "IFCFAN" | "IFCTANK"
        | "IFCSPACEHEATER" | "IFCTRANSPORTELEMENT" | "IFCAUDIOVISUALAPPLIANCE" => {
            Category::Equipment
        }
        _ => Category::Generic,
    }
}

/// IFC class in its schema spelling for the classes `category_of` knows.
fn class_name(upper: &str) -> Option<&'static str> {
    const NAMES: [&str; 33] = [
        "IfcDoor", "IfcWindow", "IfcFurniture", "IfcFurnishingElement", "IfcSystemFurnitureElement",
        "IfcSanitaryTerminal", "IfcWasteTerminal", "IfcFireSuppressionTerminal", "IfcLightFixture",
        "IfcLamp", "IfcAirTerminal", "IfcPipeFitting", "IfcDuctFitting", "IfcStair", "IfcStairFlight",
        "IfcMember", "IfcBeam", "IfcColumn", "IfcPlate", "IfcGeographicElement", "IfcOutlet",
        "IfcSwitchingDevice", "IfcElectricAppliance", "IfcElectricDistributionBoard",
        "IfcCommunicationsAppliance", "IfcAlarm", "IfcSensor", "IfcUnitaryEquipment", "IfcPump",
        "IfcFan", "IfcTank", "IfcSpaceHeater", "IfcBuildingElementProxy",
    ];
    NAMES.into_iter().find(|n| n.eq_ignore_ascii_case(upper))
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Enum(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        Value::Real(r) => Some(format!("{r}")),
        Value::Typed(_, a) => a.first().and_then(text_of),
        _ => None,
    }
}

/// Single values of the type's property sets as `Pset.Name` → text.
fn type_props(f: &StepFile, ty: &Entity) -> Vec<(String, String)> {
    let mut out = vec![];
    for ps in Rd::refs(ty, 5) {
        if f.type_of(ps) != Some("IFCPROPERTYSET") {
            continue;
        }
        let Some(pe) = f.get(ps) else { continue };
        let set = Rd::arg(pe, 2).as_str().unwrap_or("");
        for p in Rd::refs(pe, 4) {
            if f.type_of(p) != Some("IFCPROPERTYSINGLEVALUE") {
                continue;
            }
            let Some(pv) = f.get(p) else { continue };
            if let (Some(n), Some(v)) = (Rd::arg(pv, 0).as_str(), text_of(Rd::arg(pv, 2))) {
                out.push((format!("{set}.{n}"), v));
            }
        }
    }
    out
}

/// Surface style name (or colour) by styled geometric item.
fn styles(f: &StepFile) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for (id, ty) in f.iter() {
        if ty != "IFCSTYLEDITEM" {
            continue;
        }
        let Some(e) = f.get(id) else { continue };
        let Some(item) = Rd::arg(e, 0).as_ref() else { continue };
        let mut stack: Vec<u32> = Rd::refs(e, 1);
        let mut name = None;
        let mut guard = 0;
        while let Some(s) = stack.pop() {
            guard += 1;
            if guard > 64 {
                break;
            }
            match f.type_of(s) {
                Some("IFCPRESENTATIONSTYLEASSIGNMENT") => {
                    stack.extend(f.get(s).map(|x| Rd::refs(x, 0)).unwrap_or_default())
                }
                Some("IFCSURFACESTYLE") => {
                    let Some(se) = f.get(s) else { continue };
                    if let Some(n) = Rd::arg(se, 0).as_str().filter(|n| !n.is_empty()) {
                        name = Some(n.to_string());
                        break;
                    }
                    for r in Rd::refs(se, 2) {
                        if let Some(c) = f.get(r).and_then(|x| Rd::arg(x, 0).as_ref()) {
                            if let Some(ce) = f.get(c) {
                                let ch = |i: usize| Rd::arg(ce, i).as_f64().unwrap_or(0.0);
                                let to = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                                name = Some(format!(
                                    "Colour #{:02X}{:02X}{:02X}",
                                    to(ch(1)),
                                    to(ch(2)),
                                    to(ch(3))
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(n) = name {
            out.insert(item, n);
        }
    }
    out
}

fn upright(fr: &Frame) -> bool {
    let unit = |v: [f64; 3]| (dot(v, v) - 1.0).abs() < 1e-6;
    unit(fr.x)
        && unit(fr.y)
        && unit(fr.z)
        && dot(fr.x, fr.y).abs() < 1e-6
        && (fr.z[2] - 1.0).abs() < 1e-6
        // right-handed: y = z × x
        && (fr.x[0] * fr.y[1] - fr.x[1] * fr.y[0] - 1.0).abs() < 1e-6
}

impl Types {
    /// Meshes of a representation map (origin applied), one part per surface style.
    fn parts(&mut self, rd: &mut Rd, map: u32) -> Result<Vec<MeshPart>, Why> {
        let f = rd.f;
        if self.styles.is_none() {
            self.styles = Some(styles(f));
        }
        let me = rd.ent(map, &["IFCREPRESENTATIONMAP"])?;
        let origin = rd.axis_placement(Rd::arg(me, 0))?;
        let rep = rd.ent(Rd::reference(me, 1)?, &[])?;
        let mut by_mat: ByMat = vec![];
        let mut approx = vec![];
        for item in Rd::refs(rep, 3) {
            let m = match rd.item_mesh(item, 0, &mut approx) {
                Ok(m) => m,
                Err(w) if super::tess::is_curve_only(w) => continue,
                Err(w) => return Err(w),
            };
            let mat = self
                .styles
                .as_ref()
                .and_then(|s| s.get(&item).cloned())
                .unwrap_or_else(|| "Default".into());
            push_mesh(&mut by_mat, &mat, &m, Some(&origin));
        }
        let parts = to_parts(by_mat);
        if parts.is_empty() {
            return Err("type map has no surfaces");
        }
        Ok(parts)
    }

    /// Instance of a type family, or `None` when the product is not a plain placed type.
    pub(super) fn instance(
        &mut self,
        rd: &mut Rd,
        e: &Entity,
        class: &str,
        ty: Option<u32>,
    ) -> Option<Result<Placed, Why>> {
        let f = rd.f;
        let ty = ty?;
        let te = f.get(ty)?;
        let maps = Rd::refs(te, 6);
        if maps.is_empty() {
            return None;
        }
        let items = rd.body_items(e).ok()?;
        let &[it] = items.as_slice() else { return None };
        if f.type_of(it) != Some("IFCMAPPEDITEM") {
            return None;
        }
        let me = f.get(it)?;
        let src = Rd::arg(me, 0).as_ref()?;
        if !maps.contains(&src) {
            return None;
        }
        let fr = match rd.product_frame(e) {
            Ok(x) => x,
            Err(w) => return Some(Err(w)),
        };
        let target = match Rd::arg(me, 1).as_ref().map(|o| rd.operator(o)) {
            Some(Ok(t)) => t,
            Some(Err(w)) => return Some(Err(w)),
            None => return None,
        };
        let place = fr.then(&target);
        if !upright(&place) {
            return None;
        }
        if !self.fams.contains_key(&src) {
            let fam = self.family(rd, te, f.type_of(ty).unwrap_or(""), class, src);
            self.fams.insert(src, fam);
        }
        let fam = match self.fams.get(&src)? {
            Ok(x) => x.clone(),
            Err(w) => return Some(Err(w)),
        };
        Some(Ok(Placed {
            fam,
            at: place.o,
            rotation: place.x[1].atan2(place.x[0]),
        }))
    }

    fn family(
        &mut self,
        rd: &mut Rd,
        te: &Entity,
        type_class: &str,
        class: &str,
        map: u32,
    ) -> Result<TypeFamily, Why> {
        let f = rd.f;
        let parts = self.parts(rd, map)?;
        let name = Rd::arg(te, 2)
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Imported type")
            .to_string();
        let mut cat = category_of(type_class);
        if cat == Category::Generic {
            cat = category_of(class);
        }
        let mut spec = ImportSpec::new(&name, cat);
        spec.type_name = name.clone();
        // the occurrence keeps its own placement; hosts are not re-derived from foreign files
        spec.hosting = Hosting::None;
        spec.insertion = Insertion::Origin;
        let predefined = te
            .args()
            .last()
            .and_then(|v| v.as_enum())
            .filter(|p| *p != "NOTDEFINED" && *p != "USERDEFINED")
            .unwrap_or("NOTDEFINED");
        spec.ifc = class_name(class).map(|c| (c.to_string(), predefined.to_string()));
        spec.props = type_props(f, te);
        if let Some(g) = Rd::arg(te, 0).as_str() {
            spec.props.push(("IfcType.GlobalId".into(), g.to_string()));
        }
        let def = family_text(&parts, &spec).map_err(|_| "type map does not make a family")?;
        let fam = Family::parse(&def).map_err(|_| "type map does not make a family")?;
        let t = fam.types.first().ok_or("type map does not make a family")?;
        Ok(TypeFamily {
            type_name: t.name.clone(),
            type_vals: fam.assignments_text(&t.values),
            def,
        })
    }
}

/// One foreign type object ready for the import wizard: meshes of its first body map in the
/// map's own axes and a proposed specification (category, IFC class, property sets).
#[derive(Clone, Debug)]
pub struct TypeDraft {
    pub guid: String,
    pub class: String,
    pub parts: Vec<MeshPart>,
    pub spec: ImportSpec,
}

fn reader(f: &StepFile) -> Result<Rd<'_>, String> {
    let u = super::props::units(f).map_err(|e| e.to_string())?;
    Ok(Rd {
        f,
        scale: u.length,
        angle: u.angle,
        frames: HashMap::new(),
        maps: HashMap::new(),
    })
}

/// Every IfcTypeProduct with a representation map that tessellates. Doors and windows get a
/// wall host, Width/Height/Sill and an opening when the wizard keeps their category.
pub fn type_drafts(f: &StepFile) -> Result<Vec<TypeDraft>, String> {
    let mut rd = reader(f)?;
    let mut t = Types::default();
    let mut out = vec![];
    let ids: Vec<(u32, String)> = f
        .iter()
        .filter(|(_, ty)| ty.ends_with("TYPE") || ty.ends_with("STYLE"))
        .map(|(id, ty)| (id, ty.to_string()))
        .collect();
    for (id, ty) in ids {
        let Some(te) = f.get(id) else { continue };
        let Some(&map) = Rd::refs(te, 6).first() else { continue };
        if f.type_of(map) != Some("IFCREPRESENTATIONMAP") {
            continue;
        }
        let occ = ty.strip_suffix("TYPE").or_else(|| ty.strip_suffix("STYLE")).unwrap_or(&ty);
        let parts = match t.parts(&mut rd, map) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let name = Rd::arg(te, 2)
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Imported type")
            .to_string();
        let mut spec = ImportSpec::new(&name, category_of(&ty));
        spec.type_name = name;
        spec.props = type_props(f, te);
        let predefined = te
            .args()
            .last()
            .and_then(|v| v.as_enum())
            .unwrap_or("NOTDEFINED")
            .to_string();
        spec.ifc = class_name(occ).map(|c| (c.to_string(), predefined));
        out.push(TypeDraft {
            guid: Rd::arg(te, 0).as_str().unwrap_or("").to_string(),
            class: occ.to_string(),
            parts,
            spec,
        });
    }
    Ok(out)
}

/// Meshes of geometric items by material in a file that uses IFC entity names (STEP files are
/// renamed to them first); `scale` is metres per file unit.
pub(crate) fn item_parts(
    f: &StepFile,
    scale: f64,
    items: &[(u32, String)],
    skipped: &mut Vec<String>,
) -> Vec<MeshPart> {
    let mut rd = Rd {
        f,
        scale,
        angle: 1.0,
        frames: HashMap::new(),
        maps: HashMap::new(),
    };
    let mut by_mat: Vec<(String, Vec<[f64; 3]>, Vec<[u32; 3]>)> = vec![];
    let mut approx = vec![];
    for (item, mat) in items {
        match rd.item_mesh(*item, 0, &mut approx) {
            Ok(m) => push_mesh(&mut by_mat, mat, &m, None),
            Err(w) if super::tess::is_curve_only(w) => {}
            Err(w) => skipped.push(format!("#{item} {}: {w}", f.type_of(*item).unwrap_or(""))),
        }
    }
    skipped.extend(approx.iter().map(|w| format!("approximated: {w}")));
    to_parts(by_mat)
}

type ByMat = Vec<(String, Vec<[f64; 3]>, Vec<[u32; 3]>)>;

fn push_mesh(by_mat: &mut ByMat, mat: &str, m: &super::tess::Tri, fr: Option<&Frame>) {
    let k = match by_mat.iter().position(|x| x.0 == mat) {
        Some(k) => k,
        None => {
            by_mat.push((mat.to_string(), vec![], vec![]));
            by_mat.len() - 1
        }
    };
    let (_, p, t) = &mut by_mat[k];
    let base = p.len() as u32;
    p.extend(m.p.iter().map(|q| fr.map_or(*q, |fr| fr.apply(*q))));
    t.extend(m.t.iter().map(|x| x.map(|i| i + base)));
}

fn to_parts(by_mat: ByMat) -> Vec<MeshPart> {
    by_mat
        .into_iter()
        .filter_map(|(material, p, t)| {
            let pos = p.into_iter().map(|q| v3(q[0], q[1], q[2])).collect();
            MeshBody::new(pos, t).ok().map(|body| MeshPart { material, body })
        })
        .collect()
}
